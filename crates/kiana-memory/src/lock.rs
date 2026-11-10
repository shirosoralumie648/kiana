//! 跨进程文件锁。
//!
//! 记忆写入是「读-改-写」：读取索引、计算新内容、temp+rename 覆盖。两个进程同时写时，
//! 只有一把锁能进入临界区；另一个在 `lock_timeout` 后返回 [`MemoryError::LockTimeout`]，
//! 不写任何字节。
//!
//! Linux/macOS 用 `flock(2)`（`LOCK_EX | LOCK_NB` + 轮询）。锁绑定在**独立的锁文件**上，
//! 这样被保护的文件本身仍可被 `rename` 原子替换。Windows 没有 `flock`，退化为
//! `create_new` 的独占文件；语义相同（超时/释放），但崩溃后留下的独占文件需要清理。
//!
//! 注意：`flock` 不提供公平性；轮询间隔 5ms，超时是硬上限。锁文件不删除（删除会引入
//! inode 替换竞态）。

use std::fs::{File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::error::{MemoryError, MemoryResult, io_error};

/// 已持有的文件锁；`Drop` 时释放。
#[derive(Debug)]
pub struct FileLock {
    path: PathBuf,
    #[cfg(unix)]
    file: File,
    #[cfg(not(unix))]
    exclusive_path: PathBuf,
}

impl FileLock {
    /// 获取独占锁，最多等待 `timeout`。
    pub fn acquire(path: impl AsRef<Path>, timeout: Duration) -> MemoryResult<Self> {
        let path = path.as_ref().to_path_buf();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|error| io_error(parent, error))?;
        }
        let started = Instant::now();
        loop {
            match Self::try_acquire(&path) {
                Ok(Some(lock)) => return Ok(lock),
                Ok(None) => {}
                Err(error) => return Err(error),
            }
            let elapsed = started.elapsed();
            if elapsed >= timeout {
                return Err(MemoryError::LockTimeout {
                    path,
                    waited_ms: duration_millis(elapsed),
                });
            }
            let remaining = timeout.saturating_sub(elapsed);
            std::thread::sleep(remaining.min(Duration::from_millis(5)));
        }
    }

    #[cfg(unix)]
    fn try_acquire(path: &Path) -> MemoryResult<Option<Self>> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)
            .map_err(|error| io_error(path, error))?;
        use std::os::fd::AsRawFd;
        let fd = file.as_raw_fd();
        // SAFETY: `fd` 来自仍然存活的 `file`；`flock` 只操作该 fd，不保留指针。
        let result = unsafe { libc::flock(fd, libc::LOCK_EX | libc::LOCK_NB) };
        if result == 0 {
            return Ok(Some(Self {
                path: path.to_path_buf(),
                file,
            }));
        }
        let error = io::Error::last_os_error();
        match error.raw_os_error() {
            Some(code) if code == libc::EWOULDBLOCK || code == libc::EAGAIN => Ok(None),
            Some(code) if code == libc::EINTR => Ok(None),
            _ => Err(io_error(path, error)),
        }
    }

    #[cfg(not(unix))]
    fn try_acquire(path: &Path) -> MemoryResult<Option<Self>> {
        let exclusive_path = path.with_extension("exclusive");
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&exclusive_path)
        {
            Ok(_) => Ok(Some(Self {
                path: path.to_path_buf(),
                exclusive_path,
            })),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => Ok(None),
            Err(error) => Err(io_error(path, error)),
        }
    }

    /// 锁文件路径。
    pub fn path(&self) -> &Path {
        &self.path
    }
}

#[cfg(unix)]
impl Drop for FileLock {
    fn drop(&mut self) {
        use std::os::fd::AsRawFd;
        // SAFETY: `file` 仍然存活；解锁失败不影响正确性（fd 关闭时内核也会释放）。
        unsafe {
            libc::flock(self.file.as_raw_fd(), libc::LOCK_UN);
        }
    }
}

#[cfg(not(unix))]
impl Drop for FileLock {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.exclusive_path);
    }
}

pub(crate) fn duration_millis(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lock_times_out_when_already_held() {
        let dir = std::env::temp_dir().join(format!(
            "kiana-memory-lock-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(".lock");
        let held = FileLock::acquire(&path, Duration::from_millis(100)).unwrap();
        let error = FileLock::acquire(&path, Duration::from_millis(60)).unwrap_err();
        assert!(matches!(error, MemoryError::LockTimeout { .. }));
        drop(held);
        let _ = FileLock::acquire(&path, Duration::from_millis(100)).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }
}
