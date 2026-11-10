//! 沙箱子进程启动。
//!
//! 这里只负责「把 `tokio::process::Command` 配好并 spawn」；等待、超时、流式输出
//! 与进程树清理仍由调用方（`kiana-tools` 的 bash 执行器）负责。

use std::ffi::OsString;
use std::path::PathBuf;
use std::process::Stdio;

use crate::error::SandboxError;

/// 子进程 stdio 模式（与 Kiana `kiana_core::child_process::StdioMode` 对齐）。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum StdioMode {
    /// 空设备。
    Ignore,
    /// 父进程持有管道（默认）。
    #[default]
    Pipe,
    /// 继承父进程句柄。
    Inherit,
}

/// 启动沙箱子进程的选项。
///
/// 与 `kiana_core::child_process::SpawnOptions` 形状相同但**不复用**：本 crate 需要在
/// `pre_exec` 里做 Landlock `restrict_self`，而 `kiana-core` 的 `spawn_process` 没有这个
/// 注入点；同时本 crate 刻意只依赖 `tokio`/`libc`，不把 `kiana-core` 带进沙箱后端。
/// 两边字段一旦分叉，需要同时修改调用方（`kiana-tools` 的 bash 执行器）。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SpawnOptions {
    /// 工作目录；`None` 继承当前进程 cwd。
    pub cwd: Option<PathBuf>,
    /// stdin。
    pub stdin: StdioMode,
    /// stdout。
    pub stdout: StdioMode,
    /// stderr。
    pub stderr: StdioMode,
    /// Unix 上调用 `setsid()`；Windows 上设置 `DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP`。
    pub detached: bool,
    /// 仅 Windows：设置 `CREATE_NO_WINDOW`。
    pub windows_hide: bool,
    /// `Some` 时清空并整体替换环境；`None` 继承。
    pub env: Option<Vec<(OsString, OsString)>>,
    /// W3C `traceparent`；`Some` 时注入子进程环境变量 `TRACEPARENT`。
    ///
    /// 该变量在 `env` 的替换（`env_clear`）之后写入，因此即使调用方提供了完整
    /// 环境清单也不会丢失；值为调用方从 `kiana_telemetry::w3c::TraceContext` 生成的
    /// 字符串（沙箱 crate 不依赖遥测 crate，避免把 telemetry 带进 L0 后端）。
    pub traceparent: Option<String>,
    /// W3C `tracestate`；`Some` 时注入 `TRACESTATE`。
    pub tracestate: Option<String>,
}

impl SpawnOptions {
    /// 注入 W3C `traceparent`（builder 风格）。
    pub fn with_traceparent(mut self, traceparent: impl Into<String>) -> Self {
        self.traceparent = Some(traceparent.into());
        self
    }

    /// 注入 W3C `tracestate`（builder 风格）。
    pub fn with_tracestate(mut self, tracestate: impl Into<String>) -> Self {
        self.tracestate = Some(tracestate.into());
        self
    }
}

// winbase.h 的进程创建标志。
#[cfg(windows)]
const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
#[cfg(windows)]
const DETACHED_PROCESS: u32 = 0x0000_0008;
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

pub(crate) fn spawn_command(
    program: &str,
    args: &[String],
    options: &SpawnOptions,
    landlock_fd: Option<i32>,
) -> Result<tokio::process::Child, SandboxError> {
    if program.is_empty() {
        return Err(SandboxError::EmptyCommand);
    }

    let mut command = tokio::process::Command::new(program);
    command.args(args);
    if let Some(cwd) = &options.cwd
        && !cwd.as_os_str().is_empty()
    {
        command.current_dir(cwd);
    }
    command
        .stdin(to_stdio(options.stdin))
        .stdout(to_stdio(options.stdout))
        .stderr(to_stdio(options.stderr));
    if let Some(env) = &options.env {
        command.env_clear();
        command.envs(env.iter().map(|(key, value)| (key, value)));
    }
    // W3C trace context 必须在 env_clear 之后写入，保证替换环境时也不丢失。
    if let Some(traceparent) = &options.traceparent {
        command.env("TRACEPARENT", traceparent);
    }
    if let Some(tracestate) = &options.tracestate {
        command.env("TRACESTATE", tracestate);
    }

    #[cfg(unix)]
    {
        let detached = options.detached;
        if detached || landlock_fd.is_some() {
            // SAFETY: pre_exec 在 fork 之后、exec 之前运行。闭包只捕获 Copy 值，
            // 且只调用 async-signal-safe 的 setsid/prctl/syscall。
            unsafe {
                command.pre_exec(move || {
                    if detached && libc::setsid() == -1 {
                        return Err(std::io::Error::last_os_error());
                    }
                    // Landlock 只在 Linux 存在；其它 Unix（macOS）即使拿到 fd 也
                    // 没有这些符号，必须按目标平台裁剪，否则整个 crate 在 macOS
                    // 上编译失败。
                    #[cfg(target_os = "linux")]
                    if let Some(fd) = landlock_fd {
                        if libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) != 0 {
                            return Err(std::io::Error::last_os_error());
                        }
                        if libc::syscall(libc::SYS_landlock_restrict_self, fd, 0u32) != 0 {
                            return Err(std::io::Error::last_os_error());
                        }
                    }
                    #[cfg(not(target_os = "linux"))]
                    let _ = landlock_fd;
                    Ok(())
                });
            }
        }
    }
    #[cfg(not(unix))]
    {
        let _ = landlock_fd;
    }

    #[cfg(windows)]
    {
        let mut flags = 0u32;
        if options.windows_hide {
            flags |= CREATE_NO_WINDOW;
        }
        if options.detached {
            flags |= DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP;
        }
        if flags != 0 {
            command.creation_flags(flags);
        }
    }

    command.spawn().map_err(SandboxError::Spawn)
}

fn to_stdio(mode: StdioMode) -> Stdio {
    match mode {
        StdioMode::Ignore => Stdio::null(),
        StdioMode::Pipe => Stdio::piped(),
        StdioMode::Inherit => Stdio::inherit(),
    }
}
