//! Command hook 的执行器：进程组、超时、stdin、输出截断、环境变量白名单。

use std::collections::{BTreeSet, HashMap};
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio::process::Command;
use tokio::task::JoinHandle;

use crate::event::{HookEvent, HookSource};

/// 默认传给 command hook 的环境变量名。
///
/// 只包含运行 shell / 编译器等通常需要的“无秘密”变量；`OPENAI_API_KEY`、
/// `ANTHROPIC_API_KEY`、`KIANA_*` 等敏感变量默认不可见。调用方可以通过
/// [`crate::HookRunnerConfig::env_allowlist`] 扩展，但要自行承担泄漏风险。
pub const DEFAULT_HOOK_ENV_ALLOWLIST: &[&str] = &[
    "PATH",
    "HOME",
    "USER",
    "LOGNAME",
    "SHELL",
    "TMPDIR",
    "TEMP",
    "TMP",
    "LANG",
    "LANGUAGE",
    "LC_ALL",
    "LC_CTYPE",
    "TERM",
    "PWD",
    "SystemRoot",
    "SYSTEMROOT",
    "COMSPEC",
    "PATHEXT",
    "WINDIR",
];

/// 读取子进程输出时，单个流保留的最大字节数。
pub const MAX_HOOK_OUTPUT_BYTES: usize = 64 * 1024;

/// 读取输出时每个 chunk 的大小。
const READ_CHUNK_BYTES: usize = 8 * 1024;
/// 子进程退出后等待 reader 收尾的宽限；超时则 abort，防止后台孙进程占住管道。
const READER_GRACE: Duration = Duration::from_secs(2);
/// 等待 stdin writer 收尾的宽限。
const STDIN_GRACE: Duration = Duration::from_millis(500);

/// 单个流的输出（已截断）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CappedOutput {
    /// 保留的文本（UTF-8，非法字节被替换）。
    pub text: String,
    /// 是否发生过截断。
    pub truncated: bool,
    /// 子进程实际写出的总字节数。
    pub total_bytes: usize,
}

#[derive(Debug, Default)]
struct RawOutput {
    bytes: Vec<u8>,
    truncated: bool,
    total_bytes: usize,
}

impl RawOutput {
    fn finish(self) -> CappedOutput {
        let mut text = String::from_utf8_lossy(&self.bytes).into_owned();
        if self.truncated {
            text.push_str(&format!(
                "\n...[hook output truncated: kept {} of {} bytes]",
                self.bytes.len(),
                self.total_bytes
            ));
        }
        CappedOutput {
            text,
            truncated: self.truncated,
            total_bytes: self.total_bytes,
        }
    }
}

/// command hook 的执行上下文。
#[derive(Debug, Clone)]
pub(crate) struct CommandContext {
    pub project_root: PathBuf,
    pub env_allowlist: Vec<String>,
    pub depth: u32,
    pub event: HookEvent,
    pub source: HookSource,
}

/// 一次 command hook 的执行结果。
#[derive(Debug, Clone)]
pub(crate) struct CommandRun {
    pub exit_code: Option<i32>,
    pub stdout: CappedOutput,
    pub stderr: CappedOutput,
    pub timed_out: bool,
    pub spawn_error: Option<String>,
    pub duration: Duration,
}

/// 构造白名单过滤后的环境变量。
///
/// 纯函数：`source` 是父进程环境（生产路径传 `std::env::vars()`，测试可传任意
/// 集合），`overrides` 覆盖同名变量（本 crate 注入的 `KIANA_*`）。
pub(crate) fn build_hook_environment(
    source: impl IntoIterator<Item = (String, String)>,
    allowlist: &[String],
    overrides: &[(String, String)],
) -> Vec<(String, String)> {
    let allowed: BTreeSet<&str> = allowlist.iter().map(String::as_str).collect();
    let mut environment: Vec<(String, String)> = source
        .into_iter()
        .filter(|(name, _)| allowed.contains(name.as_str()))
        .collect();

    let mut index: HashMap<String, usize> = environment
        .iter()
        .enumerate()
        .map(|(position, (name, _))| (name.clone(), position))
        .collect();
    for (name, value) in overrides {
        if let Some(position) = index.get(name).copied() {
            environment[position].1 = value.clone();
        } else {
            index.insert(name.clone(), environment.len());
            environment.push((name.clone(), value.clone()));
        }
    }
    environment
}

/// 执行一个 command hook。
pub(crate) async fn run_command(
    command: &str,
    timeout: Duration,
    input_json: &str,
    context: &CommandContext,
) -> CommandRun {
    let started = Instant::now();
    let project_dir = context.project_root.to_string_lossy().into_owned();
    let overrides = [
        ("KIANA_PROJECT_DIR".to_string(), project_dir),
        (
            "KIANA_HOOK_EVENT".to_string(),
            context.event.as_str().to_string(),
        ),
        (
            "KIANA_HOOK_SOURCE".to_string(),
            context.source.as_str().to_string(),
        ),
        (
            "KIANA_HOOK_DEPTH".to_string(),
            context.depth.saturating_add(1).to_string(),
        ),
    ];
    let environment = build_hook_environment(
        // `env::vars()` 遇到非 UTF-8 变量会 panic；hook 环境只需要可转换为 UTF-8 的
        // 变量，非 UTF-8 的直接跳过。
        std::env::vars_os().filter_map(|(name, value)| {
            Some((name.into_string().ok()?, value.into_string().ok()?))
        }),
        &context.env_allowlist,
        &overrides,
    );

    let mut process = shell_command(command);
    process
        .current_dir(&context.project_root)
        .env_clear()
        .envs(environment)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    #[cfg(unix)]
    process.process_group(0);

    let mut child = match process.spawn() {
        Ok(child) => child,
        Err(error) => {
            return CommandRun {
                exit_code: None,
                stdout: CappedOutput::default(),
                stderr: CappedOutput::default(),
                timed_out: false,
                spawn_error: Some(format!("failed to spawn hook command: {error}")),
                duration: started.elapsed(),
            };
        }
    };

    let pid = child.id();
    let mut guard = ProcessGroupGuard::new(pid);

    let stdin = child.stdin.take();
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();

    let stdout_buffer = Arc::new(Mutex::new(RawOutput::default()));
    let stderr_buffer = Arc::new(Mutex::new(RawOutput::default()));

    let stdout_task = stdout.map(|stdout| {
        let buffer = Arc::clone(&stdout_buffer);
        tokio::spawn(read_capped_into(stdout, MAX_HOOK_OUTPUT_BYTES, buffer))
    });
    let stderr_task = stderr.map(|stderr| {
        let buffer = Arc::clone(&stderr_buffer);
        tokio::spawn(read_capped_into(stderr, MAX_HOOK_OUTPUT_BYTES, buffer))
    });
    let stdin_task = stdin.map(|mut stdin| {
        let input = input_json.to_string();
        tokio::spawn(async move {
            let _ = stdin.write_all(input.as_bytes()).await;
            let _ = stdin.write_all(b"\n").await;
            let _ = stdin.shutdown().await;
        })
    });

    let (status, timed_out, wait_error) = match tokio::time::timeout(timeout, child.wait()).await {
        Ok(Ok(status)) => (Some(status), false, None),
        Ok(Err(error)) => (
            None,
            false,
            Some(format!("failed to wait for hook command: {error}")),
        ),
        Err(_) => {
            // 超时：先杀整个进程组，再兜底杀直接子进程，最后回收，避免僵尸。
            if let Some(pid) = pid {
                kill_process_group(pid);
            }
            let _ = child.start_kill();
            let status = child.wait().await.ok();
            (status, true, None)
        }
    };
    guard.disarm();

    if let Some(task) = stdin_task {
        join_with_grace(task, STDIN_GRACE).await;
    }
    if let Some(task) = stdout_task {
        join_with_grace(task, READER_GRACE).await;
    }
    if let Some(task) = stderr_task {
        join_with_grace(task, READER_GRACE).await;
    }

    let stdout = take_output(&stdout_buffer);
    let stderr = take_output(&stderr_buffer);

    CommandRun {
        exit_code: status.and_then(|status| status.code()),
        stdout,
        stderr,
        timed_out,
        spawn_error: wait_error,
        duration: started.elapsed(),
    }
}

fn shell_command(command: &str) -> Command {
    #[cfg(windows)]
    {
        let mut process = Command::new("cmd");
        process.arg("/C").arg(command);
        process
    }
    #[cfg(not(windows))]
    {
        let mut process = Command::new("/bin/sh");
        process.arg("-c").arg(command);
        process
    }
}

async fn read_capped_into<R>(mut reader: R, cap: usize, shared: Arc<Mutex<RawOutput>>)
where
    R: AsyncRead + Unpin,
{
    let mut chunk = [0u8; READ_CHUNK_BYTES];
    loop {
        match reader.read(&mut chunk).await {
            Ok(0) => break,
            Ok(read) => {
                let mut output = shared
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                output.total_bytes = output.total_bytes.saturating_add(read);
                let remaining = cap.saturating_sub(output.bytes.len());
                if read > remaining {
                    output.truncated = true;
                }
                output
                    .bytes
                    .extend_from_slice(&chunk[..read.min(remaining)]);
            }
            Err(_) => break,
        }
    }
}

fn take_output(shared: &Arc<Mutex<RawOutput>>) -> CappedOutput {
    let raw = std::mem::take(
        &mut *shared
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()),
    );
    raw.finish()
}

async fn join_with_grace(task: JoinHandle<()>, grace: Duration) {
    let mut task = task;
    tokio::select! {
        _ = &mut task => {}
        _ = tokio::time::sleep(grace) => task.abort(),
    }
}

/// 进程组清理守卫：正常退出后 `disarm`；future 被 drop（取消）时仍会杀组。
struct ProcessGroupGuard {
    pid: Option<u32>,
}

impl ProcessGroupGuard {
    fn new(pid: Option<u32>) -> Self {
        Self { pid }
    }

    fn disarm(&mut self) {
        self.pid = None;
    }
}

impl Drop for ProcessGroupGuard {
    fn drop(&mut self) {
        if let Some(pid) = self.pid {
            kill_process_group(pid);
        }
    }
}

#[cfg(unix)]
fn kill_process_group(pid: u32) {
    let Ok(pid) = i32::try_from(pid) else {
        return;
    };
    // SAFETY: kill(2) 只读参数；负 pid 表示进程组。失败（ESRCH 等）无需处理。
    unsafe {
        libc::kill(-pid, libc::SIGKILL);
    }
}

#[cfg(not(unix))]
fn kill_process_group(_pid: u32) {
    // Windows 没有 POSIX 进程组；`kill_on_drop` 已负责直接子进程。
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn environment_is_filtered_to_the_allowlist_and_overrides_win() {
        let source = vec![
            ("PATH".to_string(), "/usr/bin".to_string()),
            ("HOME".to_string(), "/home/u".to_string()),
            ("KIANA_SECRET".to_string(), "s3cret".to_string()),
        ];
        let allowlist = vec!["PATH".to_string(), "HOME".to_string()];
        let overrides = vec![
            ("HOME".to_string(), "/project".to_string()),
            ("KIANA_PROJECT_DIR".to_string(), "/project".to_string()),
        ];
        let environment = build_hook_environment(source, &allowlist, &overrides);
        let map: HashMap<_, _> = environment.into_iter().collect();
        assert_eq!(map.get("PATH").map(String::as_str), Some("/usr/bin"));
        assert_eq!(map.get("HOME").map(String::as_str), Some("/project"));
        assert_eq!(
            map.get("KIANA_PROJECT_DIR").map(String::as_str),
            Some("/project")
        );
        assert!(!map.contains_key("KIANA_SECRET"));
    }

    #[tokio::test]
    async fn capped_reader_keeps_limit_and_counts_all_bytes() {
        let data = vec![b'x'; 1000];
        let shared = Arc::new(Mutex::new(RawOutput::default()));
        read_capped_into(&data[..], 100, Arc::clone(&shared)).await;
        let output = take_output(&shared);
        assert!(output.truncated);
        assert_eq!(output.total_bytes, 1000);
        assert!(output.text.starts_with(&"x".repeat(100)));
        assert!(output.text.contains("hook output truncated"));
    }

    #[tokio::test]
    async fn capped_reader_does_not_truncate_short_output() {
        let shared = Arc::new(Mutex::new(RawOutput::default()));
        read_capped_into(&b"hello"[..], 100, Arc::clone(&shared)).await;
        let output = take_output(&shared);
        assert!(!output.truncated);
        assert_eq!(output.text, "hello");
        assert_eq!(output.total_bytes, 5);
    }
}
