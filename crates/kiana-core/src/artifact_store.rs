//! 会话级 artifact 存储：把过大的工具输出落盘，正文只保留可读的预览与 handle。
//!
//! 设计来源（只借鉴行为，不复制代码）：
//! - `reference_project/ClaudeCode/src/utils/toolResultStorage.ts`：大结果落盘 +
//!   `<persisted-output>` 包装、按 tool_use_id 幂等写入、预览在换行边界截断；
//! - `reference_project/oh-my-pi/packages/coding-agent/src/session/artifacts.ts`：
//!   session 级目录、`artifact://<id>` 句柄、工具名清洗、临时文件 + 原子 rename；
//! - `reference_project/oh-my-pi/packages/coding-agent/src/tools/settings.ts`：
//!   落盘阈值与 head/tail 预览参数。
//!
//! # 契约
//!
//! - **会话作用域**：每个 [`ArtifactStore`] 绑定一个 `(base_dir, session_id)`，
//!   不同会话互不可见。`session_id` 与 artifact id 都经过严格校验，任何 `..`、
//!   路径分隔符、绝对路径都会被 [`ArtifactError::PathTraversal`] 拒绝。
//! - **文件权限**：Unix 上目录 `0700`、文件 `0600`。写入先落到同目录临时文件，
//!   校验字节数后再 `rename` 发布；失败时删除临时文件，绝不覆盖已有 artifact。
//! - **secret 不落盘**：写入前执行 [`redact_secrets`]，PEM 私钥、AWS/OpenAI/GitHub/
//!   Slack token、`password=`/`Bearer ` 等模式会被替换成 `[REDACTED:<kind>]`。
//!   发生 redaction 时 [`ArtifactHandle::redacted`] 为真。
//! - **跨压缩存活**：handle 是稳定的 `artifact://<id>` 字符串，id 由目录扫描分配；
//!   压缩只改写消息内容，不删除 artifact 目录，因此 handle 在压缩后仍解析到同一文件。
//!
//! # 与其它模块的关系
//!
//! - [`crate::output_budget`] 在工具输出超阈值时调用 [`ArtifactStore::save`]；
//! - kiana-compaction 的 pruning/recovery 用 handle 生成
//!   `[Old tool result content cleared; artifact: <path>]` 并做预算裁剪；
//! - kiana-tools 的 read/grep 等工具可直接读取 `read_range` 回读完整内容。
//!
//! 本模块不做 token 估算，也不决定何时落盘；阈值与预览策略属于
//! [`crate::output_budget`] 与调用方。

use std::fs::{self, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::LazyLock;
use std::sync::atomic::{AtomicU64, Ordering};

use regex::Regex;
use thiserror::Error;

/// artifact 句柄的 URI 前缀。
pub const ARTIFACT_SCHEME: &str = "artifact://";

/// artifact 文件扩展名（纯文本工具输出）。
pub const ARTIFACT_EXTENSION: &str = "log";

/// artifact 错误。
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum ArtifactError {
    /// id / session id / 工具名包含路径穿越或非法字符。
    #[error("invalid artifact path: {0}")]
    PathTraversal(String),
    /// artifact 不存在（或不是普通文件）。
    #[error("artifact not found: {0}")]
    NotFound(String),
    /// 文件系统错误。
    #[error("artifact I/O failed: {0}")]
    Io(String),
    /// 区间读取的字节不是合法 UTF-8（调用方应改用 [`ArtifactStore::read_range_bytes`]）。
    #[error("artifact range is not valid UTF-8")]
    NotUtf8,
}

/// 一个已落盘 artifact 的句柄。
///
/// `uri()` 是给模型看的稳定字符串；`path` 是磁盘绝对路径（用于提示里的
/// `artifact: path` 占位符）。两者在压缩前后不变。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtifactHandle {
    /// 会话内唯一 id（数字字符串）。
    pub id: String,
    /// 磁盘绝对路径。
    pub path: PathBuf,
    /// 落盘后的字节数（redaction 之后的长度）。
    pub bytes: u64,
    /// 内容是否发生过 secret redaction。
    pub redacted: bool,
}

impl ArtifactHandle {
    /// `artifact://<id>`。
    pub fn uri(&self) -> String {
        format!("{ARTIFACT_SCHEME}{}", self.id)
    }

    /// 模型可读的简短描述：`artifact://<id> (<path>)`。
    pub fn display(&self) -> String {
        format!("{} ({})", self.uri(), self.path.display())
    }
}

/// 会话级 artifact 存储。
///
/// 内部只用 `&self`，可被 `Arc` 共享；id 分配用原子计数器，首次使用时扫描目录
/// 恢复序号，避免 resume 覆盖旧文件。
#[derive(Debug)]
pub struct ArtifactStore {
    root: PathBuf,
    session_id: String,
    next_id: AtomicU64,
    /// 是否已扫描过目录（0 = 未扫描，1 = 已扫描）。
    scanned: AtomicU64,
    lock: std::sync::Mutex<()>,
}

impl ArtifactStore {
    /// 创建会话存储。目录在第一次写入时创建。
    pub fn new(base_dir: impl Into<PathBuf>, session_id: &str) -> Result<Self, ArtifactError> {
        validate_component(session_id, "session id")?;
        Ok(Self {
            root: base_dir.into().join(session_id),
            session_id: session_id.to_string(),
            next_id: AtomicU64::new(1),
            scanned: AtomicU64::new(0),
            lock: std::sync::Mutex::new(()),
        })
    }

    /// 会话 id。
    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    /// artifact 根目录。
    pub fn root(&self) -> &Path {
        &self.root
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, ()> {
        self.lock
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn ensure_dir(&self) -> Result<(), ArtifactError> {
        if self.scanned.load(Ordering::SeqCst) == 0 {
            let _guard = self.lock();
            if self.scanned.load(Ordering::SeqCst) == 0 {
                fs::create_dir_all(&self.root).map_err(io_error)?;
                set_dir_mode(&self.root)?;
                let mut max_id = 0u64;
                for entry in fs::read_dir(&self.root).map_err(io_error)? {
                    let entry = entry.map_err(io_error)?;
                    let name = entry.file_name();
                    let Some(name) = name.to_str() else {
                        continue;
                    };
                    if let Some(id) = name
                        .split('.')
                        .next()
                        .and_then(|part| part.parse::<u64>().ok())
                        && id > max_id
                    {
                        max_id = id;
                    }
                }
                self.next_id.store(max_id + 1, Ordering::SeqCst);
                self.scanned.store(1, Ordering::SeqCst);
            }
        }
        Ok(())
    }

    fn allocate_id(&self) -> u64 {
        self.next_id.fetch_add(1, Ordering::SeqCst)
    }

    /// 保存内容并返回 handle。
    ///
    /// 内容先经过 [`redact_secrets`]；文件名由 `<id>.<sanitized tool>.<ext>` 组成。
    /// 写入使用 `create_new`，绝不覆盖已有文件。
    pub fn save(&self, tool_name: &str, content: &str) -> Result<ArtifactHandle, ArtifactError> {
        self.ensure_dir()?;
        let _guard = self.lock();
        let (safe_content, redacted) = redact_secrets(content);
        let tool = sanitize_tool_name(tool_name);
        let id = self.allocate_id();
        let file_name = format!("{id:08}.{tool}.{ARTIFACT_EXTENSION}");
        let path = self.root.join(&file_name);
        // 防御性检查：即使 sanitize 出错也不允许逃出 root。
        ensure_within(&self.root, &path)?;
        write_private_file(&path, safe_content.as_bytes())?;
        Ok(ArtifactHandle {
            id: id.to_string(),
            path,
            bytes: safe_content.len() as u64,
            redacted,
        })
    }

    /// 用调用方指定的 id 保存（测试与恢复路径使用；id 必须已校验）。
    ///
    /// 与 [`Self::save`] 一样执行 redaction 与 `create_new`。
    pub fn save_with_id(
        &self,
        id: &str,
        tool_name: &str,
        content: &str,
    ) -> Result<ArtifactHandle, ArtifactError> {
        validate_component(id, "artifact id")?;
        self.ensure_dir()?;
        let _guard = self.lock();
        let (safe_content, redacted) = redact_secrets(content);
        let tool = sanitize_tool_name(tool_name);
        let file_name = format!("{id}.{tool}.{ARTIFACT_EXTENSION}");
        let path = self.root.join(&file_name);
        ensure_within(&self.root, &path)?;
        write_private_file(&path, safe_content.as_bytes())?;
        if let Ok(numeric) = id.parse::<u64>() {
            let mut current = self.next_id.load(Ordering::SeqCst);
            while numeric >= current {
                match self.next_id.compare_exchange(
                    current,
                    numeric + 1,
                    Ordering::SeqCst,
                    Ordering::SeqCst,
                ) {
                    Ok(_) => break,
                    Err(observed) => current = observed,
                }
            }
        }
        Ok(ArtifactHandle {
            id: id.to_string(),
            path,
            bytes: safe_content.len() as u64,
            redacted,
        })
    }

    /// 解析 id 对应的磁盘路径；拒绝路径穿越与不存在的文件。
    pub fn path_for_id(&self, id: &str) -> Result<PathBuf, ArtifactError> {
        validate_component(id, "artifact id")?;
        let path = self.root.join(format!("{id}.{ARTIFACT_EXTENSION}"));
        // 文件名可能带 tool 段；先按精确名查找，再扫描前缀。
        if path.is_file() {
            ensure_within(&self.root, &path)?;
            return Ok(path);
        }
        if self.root.is_dir() {
            let numeric = id.parse::<u64>().ok();
            for entry in fs::read_dir(&self.root).map_err(io_error)? {
                let entry = entry.map_err(io_error)?;
                let name = entry.file_name();
                let Some(name) = name.to_str() else {
                    continue;
                };
                let matches = match name.split_once('.') {
                    Some((prefix, rest)) => {
                        rest.ends_with(&format!(".{ARTIFACT_EXTENSION}"))
                            && match (numeric, prefix.parse::<u64>().ok()) {
                                (Some(expected), Some(actual)) => expected == actual,
                                _ => prefix == id,
                            }
                    }
                    None => false,
                };
                if matches {
                    let candidate = entry.path();
                    ensure_within(&self.root, &candidate)?;
                    if candidate.is_file() {
                        return Ok(candidate);
                    }
                }
            }
        }
        Err(ArtifactError::NotFound(id.to_string()))
    }

    /// 解析 `artifact://<id>` 或裸 id。
    pub fn resolve(&self, handle: &str) -> Result<ArtifactHandle, ArtifactError> {
        let id = handle.strip_prefix(ARTIFACT_SCHEME).unwrap_or(handle);
        let path = self.path_for_id(id)?;
        let bytes = fs::metadata(&path).map_err(io_error)?.len();
        Ok(ArtifactHandle {
            id: id.to_string(),
            path,
            bytes,
            redacted: false,
        })
    }

    /// 读取完整内容（严格 UTF-8）。
    pub fn read(&self, id: &str) -> Result<String, ArtifactError> {
        let path = self.path_for_id(id)?;
        fs::read_to_string(&path).map_err(io_error)
    }

    /// 按字节区间读取；`offset + length` 超出末尾时截断到末尾。
    ///
    /// 这是“artifact 可以 offset 回读且逐字节一致”的底层保证：返回的字节与写入
    /// 文件中的对应区间完全相同（redaction 之后的内容）。
    pub fn read_range_bytes(
        &self,
        id: &str,
        offset: u64,
        length: u64,
    ) -> Result<Vec<u8>, ArtifactError> {
        let path = self.path_for_id(id)?;
        let mut file = fs::File::open(&path).map_err(io_error)?;
        let size = file.metadata().map_err(io_error)?.len();
        let start = offset.min(size);
        let end = offset.saturating_add(length).min(size);
        if start > 0 {
            use std::io::Seek;
            file.seek(io::SeekFrom::Start(start)).map_err(io_error)?;
        }
        let mut buffer = vec![0u8; (end - start) as usize];
        file.read_exact(&mut buffer).map_err(io_error)?;
        Ok(buffer)
    }

    /// [`Self::read_range_bytes`] 的 UTF-8 版本；区间切在字符中间时返回
    /// [`ArtifactError::NotUtf8`]。
    pub fn read_range(&self, id: &str, offset: u64, length: u64) -> Result<String, ArtifactError> {
        let bytes = self.read_range_bytes(id, offset, length)?;
        String::from_utf8(bytes).map_err(|_| ArtifactError::NotUtf8)
    }

    /// 列出所有 artifact 文件名（排序后）。
    pub fn list(&self) -> Result<Vec<String>, ArtifactError> {
        if !self.root.is_dir() {
            return Ok(Vec::new());
        }
        let mut names = Vec::new();
        for entry in fs::read_dir(&self.root).map_err(io_error)? {
            let entry = entry.map_err(io_error)?;
            if !entry.path().is_file() {
                continue;
            }
            if let Some(name) = entry.file_name().to_str() {
                names.push(name.to_string());
            }
        }
        names.sort();
        Ok(names)
    }

    /// 删除一个 artifact（清理失败返回 IO 错误）。
    pub fn remove(&self, id: &str) -> Result<(), ArtifactError> {
        let path = self.path_for_id(id)?;
        fs::remove_file(path).map_err(io_error)
    }
}

// ============================================================================
// 路径与权限
// ============================================================================

fn io_error(error: io::Error) -> ArtifactError {
    ArtifactError::Io(error.to_string())
}

/// 校验单个路径组件：拒绝空串、`.`、`..`、路径分隔符、绝对路径与 NUL。
fn validate_component(value: &str, label: &str) -> Result<(), ArtifactError> {
    if value.is_empty()
        || value == "."
        || value == ".."
        || value.contains('/')
        || value.contains('\\')
        || value.contains('\0')
        || Path::new(value).is_absolute()
    {
        return Err(ArtifactError::PathTraversal(format!(
            "{label} contains path traversal: {value:?}"
        )));
    }
    Ok(())
}

/// 工具名清洗：非 `[A-Za-z0-9_-]` 折叠为 `_`，截断到 64 字节。
fn sanitize_tool_name(tool_name: &str) -> String {
    let mut sanitized = String::with_capacity(tool_name.len().min(64));
    for ch in tool_name.chars() {
        if ch.is_ascii_alphanumeric() || ch == '_' || ch == '-' {
            sanitized.push(ch);
        } else {
            sanitized.push('_');
        }
        if sanitized.len() >= 64 {
            break;
        }
    }
    let trimmed = sanitized.trim_matches('_');
    if trimmed.is_empty() {
        "tool".to_string()
    } else {
        trimmed.to_string()
    }
}

/// 确认 `candidate` 位于 `root` 之内（词法检查，不要求文件存在）。
fn ensure_within(root: &Path, candidate: &Path) -> Result<(), ArtifactError> {
    let normalized_root = normalize_lexically(root);
    let normalized_candidate = normalize_lexically(candidate);
    if normalized_candidate.starts_with(&normalized_root) {
        Ok(())
    } else {
        Err(ArtifactError::PathTraversal(format!(
            "{} escapes {}",
            candidate.display(),
            root.display()
        )))
    }
}

/// 词法归一化：折叠 `.` 与 `..`，不访问文件系统。
fn normalize_lexically(path: &Path) -> PathBuf {
    let mut parts: Vec<std::ffi::OsString> = Vec::new();
    for component in path.components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                parts.pop();
            }
            other => parts.push(other.as_os_str().to_owned()),
        }
    }
    let mut result = PathBuf::new();
    for part in parts {
        result.push(part);
    }
    result
}

#[cfg(unix)]
fn set_dir_mode(path: &Path) -> Result<(), ArtifactError> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).map_err(io_error)
}

#[cfg(not(unix))]
fn set_dir_mode(_path: &Path) -> Result<(), ArtifactError> {
    Ok(())
}

/// 写入私有文件：临时文件 + 校验 + 原子 rename。
fn write_private_file(path: &Path, bytes: &[u8]) -> Result<(), ArtifactError> {
    let parent = path
        .parent()
        .ok_or_else(|| ArtifactError::Io("artifact path has no parent".to_string()))?;
    let temp_name = format!(
        ".tmp-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or(0)
    );
    let temp_path = parent.join(temp_name);
    let result = (|| -> Result<(), ArtifactError> {
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&temp_path).map_err(io_error)?;
        file.write_all(bytes).map_err(io_error)?;
        file.flush().map_err(io_error)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&temp_path, fs::Permissions::from_mode(0o600)).map_err(io_error)?;
        }
        let written = fs::metadata(&temp_path).map_err(io_error)?.len();
        if written != bytes.len() as u64 {
            return Err(ArtifactError::Io(format!(
                "artifact write incomplete: wrote {written} of {} bytes",
                bytes.len()
            )));
        }
        fs::rename(&temp_path, path).map_err(io_error)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp_path);
    }
    result
}

// ============================================================================
// secret redaction
// ============================================================================

struct SecretPattern {
    kind: &'static str,
    regex: Regex,
}

static SECRET_PATTERNS: LazyLock<Vec<SecretPattern>> = LazyLock::new(|| {
    let patterns: &[(&str, &str)] = &[
        (
            "private_key",
            r"-----BEGIN [A-Z0-9 ]*PRIVATE KEY-----[\s\S]*?-----END [A-Z0-9 ]*PRIVATE KEY-----",
        ),
        ("aws_access_key", r"\bAKIA[0-9A-Z]{16}\b"),
        ("openai_key", r"\bsk-[A-Za-z0-9_-]{16,}\b"),
        ("github_token", r"\bghp_[A-Za-z0-9]{20,}\b"),
        ("slack_token", r"\bxox[baprs]-[A-Za-z0-9-]{10,}\b"),
        ("bearer_token", r"(?i)\bBearer\s+[A-Za-z0-9._~+/=-]{20,}"),
        (
            "credential_assignment",
            r#"(?i)\b(password|passwd|secret|token|api[_-]?key)\s*[:=]\s*["']?[^\s"']{6,}["']?"#,
        ),
    ];
    patterns
        .iter()
        .map(|(kind, pattern)| SecretPattern {
            kind,
            regex: Regex::new(pattern).expect("secret pattern must compile"),
        })
        .collect()
});

/// 把已知 secret 模式替换成 `[REDACTED:<kind>]`。返回 `(redacted, changed)`。
///
/// 这是 artifact 落盘前的最后一道防线：宁可误伤普通文本，也不把凭据写进会话目录。
pub fn redact_secrets(content: &str) -> (String, bool) {
    let mut result = content.to_string();
    let mut changed = false;
    for pattern in SECRET_PATTERNS.iter() {
        let replaced = pattern
            .regex
            .replace_all(&result, format!("[REDACTED:{}]", pattern.kind))
            .into_owned();
        if replaced != result {
            changed = true;
            result = replaced;
        }
    }
    (result, changed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn store() -> (TempDir, ArtifactStore) {
        let temp = TempDir::new().unwrap();
        let store = ArtifactStore::new(temp.path(), "session-1").unwrap();
        (temp, store)
    }

    #[test]
    fn save_and_read_range_is_byte_exact() {
        let (_temp, store) = store();
        let content = "alpha\nbeta\ngamma\n";
        let handle = store.save("bash", content).unwrap();
        assert_eq!(store.read(&handle.id).unwrap(), content);
        assert_eq!(store.read_range(&handle.id, 6, 4).unwrap(), "beta");
        assert_eq!(
            store
                .read_range_bytes(&handle.id, 0, content.len() as u64)
                .unwrap(),
            content.as_bytes()
        );
        // 超出末尾按末尾截断。
        assert_eq!(store.read_range(&handle.id, 11, 100).unwrap(), "gamma\n");
    }

    #[test]
    fn rejects_path_traversal() {
        let (_temp, store) = store();
        for id in ["../escape", "..", "a/b", "a\\b", "/etc/passwd", ""] {
            assert!(
                matches!(store.path_for_id(id), Err(ArtifactError::PathTraversal(_))),
                "id {id:?} must be rejected"
            );
        }
        assert!(matches!(
            ArtifactStore::new("/tmp", "../other-session"),
            Err(ArtifactError::PathTraversal(_))
        ));
        // 工具名里的分隔符被清洗，不会逃出 root。
        let handle = store.save("../../evil", "data").unwrap();
        assert!(handle.path.starts_with(store.root()));
    }

    #[cfg(unix)]
    #[test]
    fn artifact_file_mode_is_0600() {
        use std::os::unix::fs::PermissionsExt;
        let (_temp, store) = store();
        let handle = store.save("bash", "secret-free").unwrap();
        let mode = fs::metadata(&handle.path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        let dir_mode = fs::metadata(store.root()).unwrap().permissions().mode() & 0o777;
        assert_eq!(dir_mode, 0o700);
    }

    #[test]
    fn redacts_secrets_before_write() {
        let (_temp, store) = store();
        let text = "key sk-abcdefghijklmnop1234\npassword=hunter2\n";
        let handle = store.save("bash", text).unwrap();
        assert!(handle.redacted);
        let disk = fs::read_to_string(&handle.path).unwrap();
        assert!(!disk.contains("sk-abcdefghijklmnop1234"));
        assert!(!disk.contains("hunter2"));
        assert!(disk.contains("[REDACTED:openai_key]"));
        assert!(disk.contains("[REDACTED:credential_assignment]"));
    }

    #[test]
    fn handle_survives_and_resolves_to_same_artifact() {
        let (_temp, store) = store();
        let handle = store.save("grep", "match\n").unwrap();
        // 模拟一次压缩：消息内容被改写，但 artifact 目录不动。
        let resolved = store.resolve(&handle.uri()).unwrap();
        assert_eq!(resolved.path, handle.path);
        assert_eq!(store.read(&resolved.id).unwrap(), "match\n");
        assert_eq!(store.resolve(&handle.id).unwrap().path, handle.path);
    }

    #[test]
    fn session_stores_are_isolated() {
        let temp = TempDir::new().unwrap();
        let first = ArtifactStore::new(temp.path(), "session-a").unwrap();
        let second = ArtifactStore::new(temp.path(), "session-b").unwrap();
        let handle = first.save("read", "a").unwrap();
        assert!(second.path_for_id(&handle.id).is_err());
    }

    #[test]
    fn resume_allocates_past_existing_ids() {
        let (temp, store) = store();
        let first = store.save("bash", "one").unwrap();
        drop(store);
        let reopened = ArtifactStore::new(temp.path(), "session-1").unwrap();
        let second = reopened.save("bash", "two").unwrap();
        assert_ne!(first.id, second.id);
        assert!(second.id.parse::<u64>().unwrap() > first.id.parse::<u64>().unwrap());
    }
}
