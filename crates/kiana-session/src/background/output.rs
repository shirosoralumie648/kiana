//! 后台任务输出文件：O_NOFOLLOW + 硬上限。
//!
//! 任务输出可能来自不可信进程；输出目录里的符号链接不能被跟随（否则沙箱内的
//! 攻击者可以诱导宿主进程写任意文件）。Linux 上使用 `O_NOFOLLOW` 打开最终路径，
//! 并用 `O_APPEND` 保证多写入者不覆盖彼此。
//!
//! 硬上限是**字节级**的：`written` 从不超过 `max_bytes`，超出的内容直接丢弃并置
//! `truncated=true`；不会在文件末尾追加无界错误信息。截断时按 UTF-8 字符边界切分，
//! 避免产生半个多字节字符。
//!
//! 参考设计：`reference_project/ClaudeCode/src/utils/task/diskOutput.ts` 的
//! `DiskTaskOutput`（O_NOFOLLOW、`#capped` 丢弃后续 chunk、truncated 标记）与
//! `MAX_TASK_OUTPUT_BYTES` 的硬上限思路。

use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;

/// Linux 的 `O_NOFOLLOW`（0o400000）。
#[cfg(target_os = "linux")]
const O_NOFOLLOW: i32 = 0o400000;
/// 其它 Unix 的常见值；只用于编译，运行时以平台为准。
#[cfg(all(unix, not(target_os = "linux")))]
const O_NOFOLLOW: i32 = 0x100;

/// 默认输出上限：8 MiB。
pub const DEFAULT_MAX_OUTPUT_BYTES: u64 = 8 * 1024 * 1024;

/// 一次 append 的结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AppendOutcome {
    /// 实际写入的字节数。
    pub written: u64,
    /// 是否因达到上限而丢弃了部分内容。
    pub truncated: bool,
}

/// 单个任务的输出文件。
#[derive(Debug)]
pub struct BackgroundTaskOutput {
    path: PathBuf,
    file: File,
    max_bytes: u64,
    written: u64,
    truncated: bool,
}

impl BackgroundTaskOutput {
    /// 创建/打开输出文件（若不存在则创建，存在则追加）。
    ///
    /// 最终组件的符号链接会因 `O_NOFOLLOW` 被拒绝（Linux 上返回 ELOOP）。
    pub fn open(path: impl Into<PathBuf>, max_bytes: u64) -> io::Result<Self> {
        let path = path.into();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut options = OpenOptions::new();
        options.write(true).create(true).append(true);
        #[cfg(unix)]
        {
            options.mode(0o600);
            options.custom_flags(O_NOFOLLOW);
        }
        let file = options.open(&path)?;
        let written = file.metadata().map(|metadata| metadata.len()).unwrap_or(0);
        let truncated = written >= max_bytes;
        Ok(Self {
            path,
            file,
            max_bytes,
            written,
            truncated,
        })
    }

    /// 输出路径。
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// 已写入字节数（含打开前已有的内容）。
    pub fn written(&self) -> u64 {
        self.written
    }

    /// 是否已发生截断。
    pub fn is_truncated(&self) -> bool {
        self.truncated
    }

    /// 截断到指定长度（恢复路径专用）。
    ///
    /// `finish_task` 先追加终态输出、后提交 journal CAS。进程若死在两者之间，
    /// 记录仍是 Running 而文件已包含上次追加；重放前回到记录里的 `output_bytes`，
    /// 同一段终态输出就不会重复写入。调用方只在文件比记录更长时截断。
    pub fn truncate_to(&mut self, length: u64) -> io::Result<()> {
        self.file.set_len(length)?;
        self.file.flush()?;
        self.written = length;
        self.truncated = length >= self.max_bytes;
        Ok(())
    }

    /// 追加文本。超过上限的部分被丢弃。
    pub fn append(&mut self, text: &str) -> io::Result<AppendOutcome> {
        if text.is_empty() {
            return Ok(AppendOutcome {
                written: 0,
                truncated: self.truncated,
            });
        }
        let remaining = self.max_bytes.saturating_sub(self.written);
        if remaining == 0 {
            self.truncated = true;
            return Ok(AppendOutcome {
                written: 0,
                truncated: true,
            });
        }
        let bytes = text.as_bytes();
        let take = bytes.len().min(remaining as usize);
        // 不允许切在多字节字符中间；回退到最近的 char 边界。
        let mut take = take;
        while take > 0 && !text.is_char_boundary(take) {
            take -= 1;
        }
        if take < bytes.len() {
            self.truncated = true;
        }
        if take > 0 {
            self.file.write_all(&bytes[..take])?;
            self.file.flush()?;
            self.written = self.written.saturating_add(take as u64);
        }
        Ok(AppendOutcome {
            written: take as u64,
            truncated: self.truncated,
        })
    }
}

/// 追加到已有输出文件（sweep 恢复路径用；没有内存中的 handle 时重新打开）。
pub fn append_to_path(
    path: &Path,
    max_bytes: u64,
    text: &str,
) -> io::Result<(BackgroundTaskOutput, AppendOutcome)> {
    let mut output = BackgroundTaskOutput::open(path, max_bytes)?;
    let outcome = output.append(text)?;
    Ok((output, outcome))
}
