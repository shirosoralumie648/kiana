//! 沙箱 relay：一个不受沙箱约束的监督进程，负责给出**确定的**执行状态。
//!
//! # 为什么需要 relay
//!
//! 父进程只等待一个直接子进程时，无法区分三种情况：
//!
//! 1. 命令正常退出，拿到了退出码；
//! 2. 命令被外层（超时/取消/进程树清理）杀死；
//! 3. 父进程自己被杀死，命令的最终状态不可知。
//!
//! 第 2、3 种都会被 `wait()` 折叠成「退出码 137」。把实际命令交给一个 relay 子进程
//! （`/bin/sh` 脚本）后，relay 在命令启动前写 `started`、命令结束后写 `exit <code>`：
//!
//! - 两个标记都有 → [`ExecutionStatus::ConfirmedExit`]；
//! - 只有 `started`（relay 被 SIGKILL/崩溃）→ [`ExecutionStatus::Unconfirmed`]：
//!   命令**可能已经执行**，调用方必须提示模型不要自动重试；
//! - 连 `started` 都没有 → [`ExecutionStatus::BackendUnavailable`]：命令没有启动。
//!
//! relay 本身在沙箱**外面**（否则它写不了状态文件）；对 bubblewrap 后端，relay 启动的
//! 就是已经包装好的 `bwrap ...` 命令；对 Landlock 后端，relay 进程本身被
//! `restrict_self`，状态目录额外列入可写根（私有临时目录，权限 0700）。
//!
//! # 信号与进程树
//!
//! 调用方 spawn 时设置 `detached`，relay 成为进程组组长；命令及其沙箱子进程留在同一
//! 进程组。`kill_process_tree(relay_pid)` 对 `-pid` 发 SIGKILL，relay 与命令一起结束，
//! 状态文件停留在 `started`，得到 `Unconfirmed`。
//!
//! # 失败模式
//!
//! - 状态目录创建失败：返回 [`SandboxError::Relay`]，命令不会启动；
//! - `/bin/sh` 启动失败：`SandboxError::Spawn`，命令不会启动；
//! - relay 在写 `started` 之前就退出：`BackendUnavailable`（安全：命令确实没跑）；
//! - 状态文件被外部篡改：只认 `started` / `exit` 两种行，未知内容按「未确认」处理。

use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::error::SandboxError;
use crate::execution_policy::ExecutionStatus;

/// relay 脚本。位置参数：`$1` = 状态文件，`$2..` = 要执行的程序与参数。
///
/// 命令放在后台执行，relay 通过 `wait` 取得退出码；收到 TERM/INT/HUP 时转发给命令。
/// 使用位置参数而不是把用户命令拼进脚本，避免任何引用/转义问题。
const RELAY_SCRIPT: &str = r#"
status=$1
shift
if ! printf 'started %s\n' "$$" > "$status"; then
  exit 125
fi
"$@" &
child=$!
forward() { kill -TERM "$child" 2>/dev/null; }
trap forward TERM INT HUP QUIT
while :; do
  wait "$child"
  code=$?
  if kill -0 "$child" 2>/dev/null; then
    continue
  fi
  break
done
printf 'exit %s\n' "$code" > "$status" 2>/dev/null
exit "$code"
"#;

/// relay 启动的进程。
///
/// `child` 是 relay（不是实际命令）；实际命令是 relay 的子进程。调用方照常等待
/// `child`、流式读取 stdout/stderr，结束后用 [`RelayedChild::outcome`] 读取状态文件。
pub struct RelayedChild {
    child: tokio::process::Child,
    status_file: PathBuf,
    status_dir: PathBuf,
}

impl std::fmt::Debug for RelayedChild {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RelayedChild")
            .field("pid", &self.child.id())
            .field("status_file", &self.status_file)
            .finish_non_exhaustive()
    }
}

impl RelayedChild {
    pub(crate) fn new(
        child: tokio::process::Child,
        status_file: PathBuf,
        status_dir: PathBuf,
    ) -> Self {
        Self {
            child,
            status_file,
            status_dir,
        }
    }

    /// relay 子进程（可等待、可 kill）。
    pub fn child_mut(&mut self) -> &mut tokio::process::Child {
        &mut self.child
    }

    /// relay 的 PID（= 进程组组长）。
    pub fn pid(&self) -> Option<u32> {
        self.child.id()
    }

    /// 状态文件路径（测试/诊断用）。
    pub fn status_path(&self) -> &Path {
        &self.status_file
    }

    /// 读取状态文件并给出执行状态。
    ///
    /// 必须在 relay 退出后调用（`wait` 返回之后），否则会把还活着的命令误判成
    /// `Unconfirmed`。
    pub fn outcome(&self) -> ExecutionStatus {
        match std::fs::read_to_string(&self.status_file) {
            Ok(contents) => parse_status(&contents),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                ExecutionStatus::BackendUnavailable {
                    reason: "the sandbox relay never wrote a status file; the command was not \
                             started"
                        .to_string(),
                }
            }
            Err(error) => ExecutionStatus::BackendUnavailable {
                reason: format!("cannot read the sandbox relay status file: {error}"),
            },
        }
    }
}

impl Drop for RelayedChild {
    fn drop(&mut self) {
        // 状态目录只包含状态文件；relay 已经退出时直接删除。删除失败不影响结果。
        let _ = std::fs::remove_dir_all(&self.status_dir);
    }
}

/// 解析状态文件内容。
///
/// 只接受 `started ...` 与 `exit <i32>`；`exit` 行存在时它就是权威结果。未知行忽略。
pub fn parse_status(contents: &str) -> ExecutionStatus {
    let mut started = false;
    let mut exit: Option<i32> = None;
    for line in contents.lines() {
        if line.starts_with("started") {
            started = true;
        } else if let Some(raw) = line.strip_prefix("exit ")
            && let Ok(code) = raw.trim().parse::<i32>()
        {
            exit = Some(code);
        }
    }
    match (started, exit) {
        // 状态文件里只有 `wait` 得到的退出码；`> 128` 也可能只是程序自己 `exit 137`，
        // 无法据此判断是否被信号终止，不猜。
        (_, Some(code)) => ExecutionStatus::ConfirmedExit { code, signal: None },
        (true, None) => ExecutionStatus::Unconfirmed {
            reason: "the sandbox relay was terminated after the command started".to_string(),
        },
        (false, None) => ExecutionStatus::BackendUnavailable {
            reason: "the sandbox relay exited before the command could start".to_string(),
        },
    }
}

/// 原子计数器：同一进程内状态目录名唯一。
static RELAY_COUNTER: AtomicU64 = AtomicU64::new(0);

/// 创建一个私有状态目录（0700）并返回目录路径。
pub(crate) fn create_status_dir() -> Result<PathBuf, SandboxError> {
    let base = std::env::temp_dir();
    let pid = std::process::id();
    for _ in 0..100 {
        let nonce = RELAY_COUNTER.fetch_add(1, Ordering::Relaxed);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or_default();
        let dir = base.join(format!("kiana-relay-{pid}-{nanos:x}-{nonce}"));
        match std::fs::create_dir(&dir) {
            Ok(()) => {
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))
                        .map_err(|error| SandboxError::Relay(error.to_string()))?;
                }
                return Ok(dir);
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(SandboxError::Relay(error.to_string())),
        }
    }
    Err(SandboxError::Relay(
        "could not allocate a unique relay status directory".to_string(),
    ))
}

/// 构造 relay 命令：`/bin/sh -c SCRIPT kiana-relay STATUS PROGRAM ARGS...`。
#[cfg(unix)]
pub(crate) fn relay_command(
    program: &str,
    args: &[String],
    status_file: &Path,
) -> (String, Vec<String>) {
    let mut relay_args = Vec::with_capacity(args.len() + 5);
    relay_args.push("-c".to_string());
    relay_args.push(RELAY_SCRIPT.to_string());
    relay_args.push("kiana-relay".to_string());
    relay_args.push(status_file.to_string_lossy().into_owned());
    relay_args.push(program.to_string());
    relay_args.extend(args.iter().cloned());
    ("/bin/sh".to_string(), relay_args)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn confirmed_exit_is_parsed() {
        let status = parse_status("started 42\nexit 7\n");
        assert_eq!(
            status,
            ExecutionStatus::ConfirmedExit {
                code: 7,
                signal: None
            }
        );
    }

    #[test]
    fn signal_like_exit_codes_are_not_guessed_as_signals() {
        // 137 也可能是命令自己 `exit 137`；relay 没有 wait status，不能推断信号。
        let status = parse_status("started 42\nexit 137\n");
        assert_eq!(
            status,
            ExecutionStatus::ConfirmedExit {
                code: 137,
                signal: None
            }
        );
    }

    #[test]
    fn started_without_exit_is_unconfirmed() {
        let status = parse_status("started 42\n");
        assert!(matches!(status, ExecutionStatus::Unconfirmed { .. }));
        assert!(status.user_message().contains("Do not retry automatically"));
    }

    #[test]
    fn missing_markers_mean_the_command_never_started() {
        let status = parse_status("");
        assert!(matches!(status, ExecutionStatus::BackendUnavailable { .. }));
    }

    #[test]
    fn unknown_lines_are_ignored() {
        let status = parse_status("garbage\nstarted 1\nmore garbage\nexit 0\n");
        assert_eq!(status.exit_code(), Some(0));
    }
}
