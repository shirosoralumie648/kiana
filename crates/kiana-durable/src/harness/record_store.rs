//! 通用 JSON 记录存储：跨进程串行化的读改写 + 乐观并发。
//!
//! cron 的 `pending_slot`、`next_run_at`、execution 账本与 delivery claim 需要
//! “同一时刻只有一个写入者”和“比较后再写”（CAS）。`harness/scheduler.rs` 的
//! `TaskScheduler` 面向并发任务图，不适配这种同步、按 job/execution 记录推进的形态；
//! 本模块把缺失的**存储原语**补在 `kiana-durable` 内（ADR `docs/adr/durable.md`
//! 决定 4：先扩展 durable 契约，不在上层再长一套事务账本）：
//!
//! - 目录布局 `<root>/<collection>/<id>.json`，写入走临时文件 + `rename`，崩溃不会留下
//!   半个 JSON；
//! - 每个操作持 `<root>/.lock` 的 `flock(LOCK_EX)`，跨进程串行化；
//! - [`JsonRecordStore::compare_and_swap`] 以记录的 `version` 字段做乐观并发；
//! - [`JsonRecordStore::update`] 在锁内读改写，供没有 version 字段的派生状态
//!   （例如 delivery claim）使用。
//!
//! 非 Unix 平台没有 `flock`：退化为进程内互斥（仍是单进程正确，跨进程由上层保证）。

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::Serialize;
use serde::de::DeserializeOwned;

/// 记录存储错误。
#[derive(Debug, thiserror::Error)]
pub enum JsonRecordStoreError {
    /// I/O 失败。
    #[error("record store I/O error at {path}: {source}")]
    Io {
        /// 路径。
        path: PathBuf,
        /// 底层错误。
        source: std::io::Error,
    },
    /// JSON 解析/序列化失败。
    #[error("record store JSON error at {path}: {source}")]
    Json {
        /// 路径。
        path: PathBuf,
        /// 底层错误。
        source: serde_json::Error,
    },
    /// 记录 id 非法或记录损坏。
    #[error("invalid record at {path}: {message}")]
    Corrupt {
        /// 路径。
        path: PathBuf,
        /// 原因。
        message: String,
    },
}

impl JsonRecordStoreError {
    fn io(path: &Path, source: std::io::Error) -> Self {
        Self::Io {
            path: path.to_path_buf(),
            source,
        }
    }

    fn json(path: &Path, source: serde_json::Error) -> Self {
        Self::Json {
            path: path.to_path_buf(),
            source,
        }
    }

    fn corrupt(path: &Path, message: impl Into<String>) -> Self {
        Self::Corrupt {
            path: path.to_path_buf(),
            message: message.into(),
        }
    }
}

/// 同一目录下按 collection 分组的 JSON 记录。
pub struct JsonRecordStore {
    root: PathBuf,
    /// 没有 `flock` 的平台上的进程内串行化。
    local: Mutex<()>,
}

impl std::fmt::Debug for JsonRecordStore {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("JsonRecordStore")
            .field("root", &self.root)
            .finish_non_exhaustive()
    }
}

impl JsonRecordStore {
    /// 创建 `<root>/<collection>` 目录与锁文件。
    pub fn new(root: impl Into<PathBuf>) -> Result<Self, JsonRecordStoreError> {
        let root = root.into();
        fs::create_dir_all(&root).map_err(|error| JsonRecordStoreError::io(&root, error))?;
        let lock = root.join(".lock");
        if !lock.exists() {
            fs::OpenOptions::new()
                .create(true)
                .write(true)
                .truncate(false)
                .open(&lock)
                .map_err(|error| JsonRecordStoreError::io(&lock, error))?;
        }
        Ok(Self {
            root,
            local: Mutex::new(()),
        })
    }

    /// 根目录。
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// 准备一个 collection（幂等）。
    pub fn ensure_collection(&self, collection: &str) -> Result<(), JsonRecordStoreError> {
        let directory = self.collection_dir(collection)?;
        fs::create_dir_all(&directory).map_err(|error| JsonRecordStoreError::io(&directory, error))
    }

    fn collection_dir(&self, collection: &str) -> Result<PathBuf, JsonRecordStoreError> {
        if collection.is_empty() || collection.contains(['/', '\\']) || collection.contains("..") {
            return Err(JsonRecordStoreError::corrupt(
                &self.root,
                format!("collection {collection:?} must be a single path segment"),
            ));
        }
        Ok(self.root.join(collection))
    }

    fn record_path(&self, collection: &str, id: &str) -> Result<PathBuf, JsonRecordStoreError> {
        if id.is_empty() || id.contains(['/', '\\']) || id.contains("..") {
            return Err(JsonRecordStoreError::corrupt(
                &self.root,
                format!("record id {id:?} must not contain path separators or `..`"),
            ));
        }
        Ok(self.collection_dir(collection)?.join(format!("{id}.json")))
    }

    /// 在跨进程锁内运行 `action`。
    pub fn with_lock<T>(
        &self,
        action: impl FnOnce() -> Result<T, JsonRecordStoreError>,
    ) -> Result<T, JsonRecordStoreError> {
        let _local = self
            .local
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let lock_path = self.root.join(".lock");
        let file = fs::OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(&lock_path)
            .map_err(|error| JsonRecordStoreError::io(&lock_path, error))?;
        #[cfg(unix)]
        {
            use std::os::unix::io::AsRawFd;
            // SAFETY: flock 只操作已打开的 fd；失败时立即返回。
            let result = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) };
            if result != 0 {
                return Err(JsonRecordStoreError::io(
                    &lock_path,
                    std::io::Error::last_os_error(),
                ));
            }
        }
        let out = action();
        #[cfg(unix)]
        {
            use std::os::unix::io::AsRawFd;
            // SAFETY: 同上；解锁失败不影响正确性（fd 关闭时自动释放）。
            unsafe {
                libc::flock(file.as_raw_fd(), libc::LOCK_UN);
            }
        }
        out
    }

    /// 读取一条记录。
    pub fn get<T: DeserializeOwned>(
        &self,
        collection: &str,
        id: &str,
    ) -> Result<Option<T>, JsonRecordStoreError> {
        self.with_lock(|| self.get_unlocked(collection, id))
    }

    fn get_unlocked<T: DeserializeOwned>(
        &self,
        collection: &str,
        id: &str,
    ) -> Result<Option<T>, JsonRecordStoreError> {
        let path = self.record_path(collection, id)?;
        let contents = match fs::read_to_string(&path) {
            Ok(contents) => contents,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(JsonRecordStoreError::io(&path, error)),
        };
        serde_json::from_str(&contents)
            .map(Some)
            .map_err(|error| JsonRecordStoreError::json(&path, error))
    }

    /// 覆盖写入一条记录。
    pub fn put<T: Serialize>(
        &self,
        collection: &str,
        id: &str,
        value: &T,
    ) -> Result<(), JsonRecordStoreError> {
        self.with_lock(|| self.put_unlocked(collection, id, value))
    }

    fn put_unlocked<T: Serialize>(
        &self,
        collection: &str,
        id: &str,
        value: &T,
    ) -> Result<(), JsonRecordStoreError> {
        let path = self.record_path(collection, id)?;
        let parent = path.parent().unwrap_or(&self.root);
        fs::create_dir_all(parent).map_err(|error| JsonRecordStoreError::io(parent, error))?;
        let temp = parent.join(format!(
            ".tmp-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|duration| duration.as_nanos())
                .unwrap_or(0)
        ));
        let payload = serde_json::to_vec_pretty(value)
            .map_err(|error| JsonRecordStoreError::json(&path, error))?;
        {
            let mut file =
                fs::File::create(&temp).map_err(|error| JsonRecordStoreError::io(&temp, error))?;
            file.write_all(&payload)
                .map_err(|error| JsonRecordStoreError::io(&temp, error))?;
            file.sync_all()
                .map_err(|error| JsonRecordStoreError::io(&temp, error))?;
        }
        fs::rename(&temp, &path).map_err(|error| JsonRecordStoreError::io(&path, error))?;
        Ok(())
    }

    /// 枚举一个 collection 的全部记录（按 id 排序）。
    pub fn list<T: DeserializeOwned>(
        &self,
        collection: &str,
    ) -> Result<Vec<T>, JsonRecordStoreError> {
        self.with_lock(|| self.list_unlocked(collection))
    }

    fn list_unlocked<T: DeserializeOwned>(
        &self,
        collection: &str,
    ) -> Result<Vec<T>, JsonRecordStoreError> {
        let directory = self.collection_dir(collection)?;
        let entries = match fs::read_dir(&directory) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(JsonRecordStoreError::io(&directory, error)),
        };
        let mut records = Vec::new();
        for entry in entries {
            let entry = entry.map_err(|error| JsonRecordStoreError::io(&directory, error))?;
            let path = entry.path();
            if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
                continue;
            }
            let contents = fs::read_to_string(&path)
                .map_err(|error| JsonRecordStoreError::io(&path, error))?;
            let value = serde_json::from_str(&contents)
                .map_err(|error| JsonRecordStoreError::json(&path, error))?;
            records.push(value);
        }
        Ok(records)
    }

    /// 比较后写入：只有当前记录的 `version` 等于 `expected` 才写入 `value` 并把
    /// `version` 设为 `expected + 1`。返回是否写入。
    pub fn compare_and_swap(
        &self,
        collection: &str,
        id: &str,
        expected: u64,
        value: &serde_json::Value,
    ) -> Result<bool, JsonRecordStoreError> {
        self.with_lock(|| {
            let path = self.record_path(collection, id)?;
            let current: Option<serde_json::Value> = match fs::read_to_string(&path) {
                Ok(contents) => Some(
                    serde_json::from_str(&contents)
                        .map_err(|error| JsonRecordStoreError::json(&path, error))?,
                ),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
                Err(error) => return Err(JsonRecordStoreError::io(&path, error)),
            };
            let current_version = current
                .as_ref()
                .and_then(|value| value.get("version"))
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(0);
            if current_version != expected {
                return Ok(false);
            }
            let mut updated = value.clone();
            let Some(object) = updated.as_object_mut() else {
                return Err(JsonRecordStoreError::corrupt(
                    &path,
                    "record value must be a JSON object",
                ));
            };
            object.insert(
                "version".to_string(),
                serde_json::Value::from(expected.saturating_add(1)),
            );
            self.put_unlocked(collection, id, &updated)?;
            Ok(true)
        })
    }

    /// 在锁内读改写一条记录；`mutate` 返回 `Some(value)` 时写回并返回该值，
    /// `None` 表示放弃写入（记录保持不变）。
    pub fn update<T, R>(
        &self,
        collection: &str,
        id: &str,
        mutate: impl FnOnce(&mut T) -> Option<R>,
    ) -> Result<Option<R>, JsonRecordStoreError>
    where
        T: Serialize + DeserializeOwned,
    {
        self.with_lock(|| {
            let Some(mut value) = self.get_unlocked::<T>(collection, id)? else {
                return Ok(None);
            };
            let result = mutate(&mut value);
            if result.is_some() {
                self.put_unlocked(collection, id, &value)?;
            }
            Ok(result)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;

    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    struct Record {
        id: String,
        value: u64,
        #[serde(default)]
        version: u64,
    }

    #[test]
    fn compare_and_swap_is_versioned_and_update_is_atomic() {
        let temp = tempfile::tempdir().expect("tempdir");
        let store = JsonRecordStore::new(temp.path()).expect("store");
        store
            .put(
                "jobs",
                "a",
                &Record {
                    id: "a".to_string(),
                    value: 1,
                    version: 0,
                },
            )
            .expect("put");
        let candidate = serde_json::json!({"id":"a","value":2,"version":0});
        assert!(
            store
                .compare_and_swap("jobs", "a", 0, &candidate)
                .expect("cas")
        );
        let stored: Record = store.get("jobs", "a").expect("get").expect("record");
        assert_eq!(stored.version, 1);
        assert_eq!(stored.value, 2);
        assert!(
            !store
                .compare_and_swap("jobs", "a", 0, &candidate)
                .expect("stale cas")
        );

        let updated = store
            .update::<Record, u64>("jobs", "a", |record| {
                record.value += 1;
                Some(record.value)
            })
            .expect("update");
        assert_eq!(updated, Some(3));
        let stored: Record = store.get("jobs", "a").expect("get").expect("record");
        assert_eq!(stored.value, 3);
    }

    #[test]
    fn records_survive_a_new_store_instance() {
        let temp = tempfile::tempdir().expect("tempdir");
        let first = JsonRecordStore::new(temp.path()).expect("store");
        first
            .put(
                "executions",
                "e1",
                &Record {
                    id: "e1".to_string(),
                    value: 7,
                    version: 0,
                },
            )
            .expect("put");
        drop(first);
        let second = JsonRecordStore::new(temp.path()).expect("store");
        let records: Vec<Record> = second.list("executions").expect("list");
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].value, 7);
    }
}
