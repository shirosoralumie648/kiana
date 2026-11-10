//! `~/.kiana/ide/<port>.lock` 的发现、校验与清理。
//!
//! lockfile 形状（与 Claude Code 的 IDE 插件兼容）：
//!
//! ```json
//! {
//!   "workspacePath": "/repo",
//!   "workspaceFolders": ["/repo", "/repo/sub"],
//!   "port": 45123,
//!   "authToken": "…",
//!   "ppid": 1234,
//!   "pid": 1234,
//!   "ideName": "vscode",
//!   "transport": "ws"
//! }
//! ```
//!
//! 旧格式是每行一个 workspace 路径（没有 JSON）；解析器保留该回退。端口优先取文件名
//! （`45123.lock` ⇒ 45123），文件名不是数字时退回 JSON 的 `port`，两者都没有则跳过。
//!
//! # 连接条件
//!
//! 1. `pid`（或 `ppid`）对应进程仍存活；两者都没有时无法证明存活，按不可连接处理；
//! 2. lockfile 至少有一个 workspace 与当前 workspace 相同，或当前 workspace 位于该目录
//!    之下（编辑器打开的是父目录）；
//! 3. 多个候选按 mtime 从新到旧排序，调用方取第一个。
//!
//! 清理会删除：无法解析的文件、dead pid 的文件、workspace 不匹配的文件。清理只
//! `remove_file`，不会跟随符号链接写入。

use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// lockfile 解析错误。
#[derive(Debug, Error)]
pub enum LockfileError {
    /// 文件读取失败。
    #[error("failed to read IDE lockfile {path}: {source}")]
    Read {
        /// 文件路径。
        path: PathBuf,
        /// 底层错误。
        source: std::io::Error,
    },
    /// JSON 与旧格式都无法解析。
    #[error("invalid IDE lockfile {path}: {message}")]
    Invalid {
        /// 文件路径。
        path: PathBuf,
        /// 解析错误。
        message: String,
    },
}

/// 一个 IDE lockfile。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IdeLockfile {
    /// 单个 workspace 路径（Kiana/新版插件）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_path: Option<String>,
    /// 打开的 workspace 目录列表（Claude 插件）。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub workspace_folders: Vec<String>,
    /// 端口；文件名通常是权威来源。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,
    /// bearer token。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth_token: Option<String>,
    /// IDE 进程的父进程 id（Claude 插件用它证明存活）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ppid: Option<u32>,
    /// IDE 进程 id。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,
    /// 编辑器名称。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ide_name: Option<String>,
    /// `ws` 或 `http`；本模块只实现 HTTP。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transport: Option<String>,
    /// 是否运行在 Windows（WSL 场景）。
    #[serde(default, skip_serializing_if = "is_false")]
    pub running_in_windows: bool,
}

fn is_false(value: &bool) -> bool {
    !*value
}

impl IdeLockfile {
    /// 用于存活检查的 pid：优先 `pid`，退回 `ppid`。
    pub fn liveness_pid(&self) -> Option<u32> {
        self.pid.or(self.ppid)
    }

    /// 所有 workspace 候选。
    pub fn workspaces(&self) -> Vec<&str> {
        let mut out = Vec::new();
        if let Some(path) = self.workspace_path.as_deref()
            && !path.is_empty()
        {
            out.push(path);
        }
        for path in &self.workspace_folders {
            if !path.is_empty() && !out.contains(&path.as_str()) {
                out.push(path);
            }
        }
        out
    }

    /// 解析 lockfile 内容；`port` 是文件名推断出的端口。
    pub fn parse(port: u16, contents: &str) -> Result<Self, String> {
        let trimmed = contents.trim();
        if trimmed.is_empty() {
            return Err("empty lockfile".to_string());
        }
        match serde_json::from_str::<IdeLockfile>(trimmed) {
            Ok(mut lock) => {
                if lock.port.is_none() {
                    lock.port = Some(port);
                }
                Ok(lock)
            }
            Err(json_error) => {
                // 旧格式：每行一个 workspace 路径。
                let folders: Vec<String> = trimmed
                    .lines()
                    .map(str::trim)
                    .filter(|line| !line.is_empty())
                    .map(str::to_string)
                    .collect();
                if folders.is_empty() {
                    return Err(format!("invalid JSON ({json_error})"));
                }
                Ok(IdeLockfile {
                    workspace_folders: folders,
                    port: Some(port),
                    ..Default::default()
                })
            }
        }
    }
}

/// 发现到的、可连接的 IDE。
#[derive(Debug, Clone)]
pub struct DiscoveredIde {
    /// lockfile 路径。
    pub lock_path: PathBuf,
    /// 端口（文件名或 JSON）。
    pub port: u16,
    /// 解析出的 lock。
    pub lock: IdeLockfile,
    /// 文件 mtime，用于排序。
    pub modified: SystemTime,
}

/// 读取单个 lockfile。
pub fn read_lockfile(path: &Path) -> Result<Option<DiscoveredIde>, LockfileError> {
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default();
    let Some(stem) = file_name.strip_suffix(".lock") else {
        return Ok(None);
    };
    let port_from_name = stem.parse::<u16>().ok();
    let contents = fs::read_to_string(path).map_err(|source| LockfileError::Read {
        path: path.to_path_buf(),
        source,
    })?;
    let parsed_port = port_from_name.unwrap_or(0);
    let mut lock =
        IdeLockfile::parse(parsed_port, &contents).map_err(|message| LockfileError::Invalid {
            path: path.to_path_buf(),
            message,
        })?;
    let port = port_from_name.or(lock.port).unwrap_or(0);
    if port == 0 {
        return Err(LockfileError::Invalid {
            path: path.to_path_buf(),
            message: "lockfile has no port".to_string(),
        });
    }
    lock.port = Some(port);
    let modified = fs::metadata(path)
        .and_then(|metadata| metadata.modified())
        .unwrap_or(SystemTime::UNIX_EPOCH);
    Ok(Some(DiscoveredIde {
        lock_path: path.to_path_buf(),
        port,
        lock,
        modified,
    }))
}

/// 发现可连接的 IDE：存活 + workspace 匹配，按 mtime 从新到旧。
pub fn discover_ides(base_dir: &Path, workspace: &Path) -> Vec<DiscoveredIde> {
    let Ok(entries) = fs::read_dir(base_dir) else {
        return Vec::new();
    };
    let mut found = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("lock") {
            continue;
        }
        let Ok(Some(ide)) = read_lockfile(&path) else {
            continue;
        };
        if !ide_is_connectable(&ide.lock, workspace) {
            continue;
        }
        found.push(ide);
    }
    found.sort_by_key(|ide| std::cmp::Reverse(ide.modified));
    found
}

/// 清理 stale lockfile；返回被删除的路径。
///
/// 删除条件只有两条：文件无法解析（损坏），或记录里的 pid 已不存在。**不**按
/// workspace 过滤：同一台机器上另一个项目的 IDE 会话仍然存活，删掉它的 lockfile
/// 会让那个窗口的 IDE 集成找不到扩展。没有 pid 的记录同样保留——无法证明已死。
pub fn cleanup_stale_locks(base_dir: &Path, _workspace: &Path) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(base_dir) else {
        return Vec::new();
    };
    let mut removed = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("lock") {
            continue;
        }
        let stale = match read_lockfile(&path) {
            Ok(Some(ide)) => match ide.lock.liveness_pid() {
                Some(pid) => !process_is_alive(pid),
                // 没有 pid 就无法证明已死：可能是别的旧插件，保留。
                None => false,
            },
            Ok(None) => false,
            Err(_) => true,
        };
        if stale && fs::remove_file(&path).is_ok() {
            removed.push(path);
        }
    }
    removed
}

fn ide_is_connectable(lock: &IdeLockfile, workspace: &Path) -> bool {
    match lock.liveness_pid() {
        Some(pid) if !process_is_alive(pid) => return false,
        Some(_) => {}
        // 没有 pid 就无法证明存活；不连接，也不自动清理（可能是别的旧插件）。
        None => return false,
    }
    workspace_matches(lock, workspace)
}

/// lockfile 的任一 workspace 是否等于 `workspace`，或包含它。
pub fn workspace_matches(lock: &IdeLockfile, workspace: &Path) -> bool {
    let workspace = normalize(workspace);
    lock.workspaces().into_iter().any(|candidate| {
        let candidate = normalize(Path::new(candidate));
        candidate == workspace || workspace.starts_with(&candidate)
    })
}

fn normalize(path: &Path) -> PathBuf {
    fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

/// 进程是否存活：`kill(pid, 0)`，`EPERM` 视为存活。
#[cfg(unix)]
pub fn process_is_alive(pid: u32) -> bool {
    if pid == 0 || pid > i32::MAX as u32 {
        return false;
    }
    // SAFETY: `kill` 只读取 pid，不写内存；pid 已做范围检查。
    let result = unsafe { libc::kill(pid as libc::pid_t, 0) };
    if result == 0 {
        return true;
    }
    std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

/// 非 Unix 平台没有可靠的存活检查；保守返回 `true`，避免误删 lockfile。
#[cfg(not(unix))]
pub fn process_is_alive(_pid: u32) -> bool {
    true
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use super::*;

    fn write_lock(dir: &Path, name: &str, contents: &str) -> PathBuf {
        let path = dir.join(name);
        let mut file = fs::File::create(&path).expect("create");
        file.write_all(contents.as_bytes()).expect("write");
        path
    }

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "kiana-ide-test-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        fs::create_dir_all(&dir).expect("mkdir");
        dir
    }

    #[test]
    fn parses_json_and_legacy_lockfiles() {
        let lock = IdeLockfile::parse(
            45123,
            r#"{"workspacePath":"/repo","authToken":"token","ppid":42}"#,
        )
        .expect("json");
        assert_eq!(lock.port, Some(45123));
        assert_eq!(lock.liveness_pid(), Some(42));
        assert_eq!(lock.auth_token.as_deref(), Some("token"));
        assert_eq!(lock.workspaces(), vec!["/repo"]);

        let legacy = IdeLockfile::parse(4000, "/repo/one\n/repo/two\n").expect("legacy");
        assert_eq!(legacy.workspace_folders, vec!["/repo/one", "/repo/two"]);
        assert_eq!(legacy.port, Some(4000));
    }

    #[test]
    fn discovers_only_live_matching_workspaces_and_sorts_by_mtime() {
        let dir = temp_dir("discover");
        let workspace = temp_dir("workspace");
        let other = temp_dir("other");
        let pid = std::process::id();

        write_lock(
            &dir,
            "41000.lock",
            &format!(
                r#"{{"workspacePath":{:?},"pid":{pid},"authToken":"a"}}"#,
                workspace.to_string_lossy()
            ),
        );
        std::thread::sleep(std::time::Duration::from_millis(10));
        write_lock(
            &dir,
            "41001.lock",
            &format!(
                r#"{{"workspacePath":{:?},"pid":{pid},"authToken":"b"}}"#,
                workspace.to_string_lossy()
            ),
        );
        write_lock(
            &dir,
            "41002.lock",
            &format!(
                r#"{{"workspacePath":{:?},"pid":{pid}}}"#,
                other.to_string_lossy()
            ),
        );
        write_lock(
            &dir,
            "41003.lock",
            r#"{"workspacePath":"/x","pid":999999999}"#,
        );

        let found = discover_ides(&dir, &workspace);
        assert_eq!(found.len(), 2);
        assert_eq!(found[0].port, 41001, "newest first");
        assert_eq!(found[1].port, 41000);

        let removed = cleanup_stale_locks(&dir, &workspace);
        assert_eq!(removed.len(), 1, "only the dead pid is stale");
        assert!(
            dir.join("41002.lock").exists(),
            "another workspace's live lockfile must survive"
        );
        assert!(!dir.join("41003.lock").exists());
        assert!(dir.join("41000.lock").exists());
    }

    #[test]
    fn workspace_parent_directory_matches_child() {
        let parent = temp_dir("parent");
        let child = parent.join("nested");
        fs::create_dir_all(&child).expect("mkdir");
        let lock = IdeLockfile {
            workspace_path: Some(parent.to_string_lossy().into_owned()),
            ..Default::default()
        };
        assert!(workspace_matches(&lock, &child));
        assert!(!workspace_matches(
            &lock,
            Path::new("/definitely/elsewhere")
        ));
    }

    #[test]
    fn process_liveness_for_current_and_impossible_pids() {
        assert!(process_is_alive(std::process::id()));
        assert!(!process_is_alive(999_999_999));
    }

    #[test]
    fn unparseable_lockfiles_are_removed_by_cleanup() {
        let dir = temp_dir("invalid");
        let workspace = temp_dir("ws-invalid");
        // 空文件无法解析成任何 lock 形态：确定已损坏。
        write_lock(&dir, "42000.lock", "");
        // 旧格式（每行一个 workspace）没有 pid，无法证明已死：保留。
        write_lock(&dir, "42001.lock", "/some/workspace");
        let removed = cleanup_stale_locks(&dir, &workspace);
        assert_eq!(removed.len(), 1, "{removed:?}");
        assert!(!dir.join("42000.lock").exists());
        assert!(dir.join("42001.lock").exists());
    }

    #[test]
    fn pidless_lockfiles_are_not_deleted_by_cleanup() {
        let dir = temp_dir("pidless");
        let workspace = temp_dir("ws-pidless");
        write_lock(&dir, "42001.lock", r#"{"workspacePath":"/x"}"#);
        let removed = cleanup_stale_locks(&dir, &workspace);
        assert!(removed.is_empty(), "cannot prove a pid-less lock is dead");
        assert!(dir.join("42001.lock").exists());
    }

    #[test]
    fn filename_port_overrides_json_port() {
        let dir = temp_dir("port");
        let workspace = temp_dir("ws-port");
        let path = write_lock(
            &dir,
            "43000.lock",
            &format!(
                r#"{{"port":1,"workspacePath":{:?},"pid":{}}}"#,
                workspace.to_string_lossy(),
                std::process::id()
            ),
        );
        let found = discover_ides(&dir, &workspace);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].port, 43000);
        assert_eq!(
            read_lockfile(&path).expect("read").expect("some").port,
            43000
        );
    }
}
