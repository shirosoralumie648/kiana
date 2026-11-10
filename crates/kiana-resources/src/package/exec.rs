//! `run_command` / `run_command_capture` / `run_command_sync` / spawn 辅助（`kiana_core::exec` 之上）。
//!
//! 被 `super::npm` / `super::git` 调用（npm 与 git 的所有子命令都经这里的三个入口，
//! `NETWORK_TIMEOUT_MS` 也由它们引用），`super::resolve` 与 `super::paths` 另用
//! `get_home_dir` 定位用户目录；`js_trim` 是 parse / settings_io / discover 共用的
//! JS `trim` 复刻。
//!
//! TS: `package-manager.ts` 的私有 `spawnCommand` / `spawnCaptureCommand` / `runCommand` /
//! `runCommandCapture` / `runCommandSync`。
//!
//! 关键语义（`spec/ca-resources.md` §8.4 第 2 条）：
//! - `runCommand` 等价 `child.on("exit")`：进程退出即返回，非零退出用
//!   `"{command} {args} failed with code {code}"`（被信号杀死时 `code` 是字面量 `null`）。
//! - `runCommandCapture` 等待 stdout/stderr 都到 EOF（Node 的 `close`），成功时返回
//!   `stdout.trim()`（**不含** stderr）；失败时用 stderr（为空则 stdout）。
//! - 超时先 kill 子进程，但**仍等管道 EOF 再 reject**；错误文案由 `timedOut` 标志选择。
//! - `isStdoutTakenOver()` 时 stdio 是 `["ignore", 2, 2]`：Unix 上 dup 一份 fd 2 给
//!   stdout/stderr；Windows 无对应物，退化为 `inherit`。
//! - `runCommandSync` 的 `status !== 0` 与 `result.error` 都算失败，错误文案是
//!   `error.message || stderr || stdout`。

use std::ffi::OsString;
use std::io;
use std::process::{ExitStatus, Stdio};
use std::time::Duration;

use tokio::io::AsyncReadExt;

use super::{PackageManagerError, err};

/// `NETWORK_TIMEOUT_MS`。
pub(crate) const NETWORK_TIMEOUT_MS: u64 = 10_000;

/// JS `String.prototype.trim()` 的空白集合（`WhiteSpace` + `LineTerminator`）。
fn is_js_whitespace(c: char) -> bool {
    matches!(
        c,
        '\u{0009}'..='\u{000D}'
            | '\u{0020}'
            | '\u{00A0}'
            | '\u{1680}'
            | '\u{2000}'..='\u{200A}'
            | '\u{2028}'
            | '\u{2029}'
            | '\u{202F}'
            | '\u{205F}'
            | '\u{3000}'
            | '\u{FEFF}'
    )
}

/// JS `String.prototype.trim()`。
pub(crate) fn js_trim(input: &str) -> &str {
    input.trim_matches(is_js_whitespace)
}

/// `getEnv()`：`process.env`，Linux 下进程环境为空时回退读 `/proc/self/environ`
///（Bun 编译二进制的兼容路径）。
///
/// 正常路径是 `std::env::vars_os()`（等价 `process.env`）；仅当它为空时才走
/// `/proc/self/environ` 的内核格式解析：条目以 `\0` 分隔、首个 `=` 之前是键。键为空
/// （行以 `=` 开头）的条目被丢弃。`index > 0` 同时排除了 `=value` 这种畸形条目。
pub(crate) fn get_env() -> Vec<(OsString, OsString)> {
    let vars: Vec<(OsString, OsString)> = std::env::vars_os().collect();
    if !vars.is_empty() {
        return vars;
    }
    #[cfg(target_os = "linux")]
    {
        if let Ok(data) = std::fs::read("/proc/self/environ") {
            use std::os::unix::ffi::OsStringExt;
            let mut env = Vec::new();
            for entry in data.split(|byte| *byte == 0) {
                if let Some(index) = entry.iter().position(|byte| *byte == b'=')
                    && index > 0
                {
                    env.push((
                        OsString::from_vec(entry[..index].to_vec()),
                        OsString::from_vec(entry[index + 1..].to_vec()),
                    ));
                }
            }
            return env;
        }
    }
    vars
}

/// `getHomeDir() = process.env.HOME || homedir()`。
///
/// 与 Node 一样只认 `HOME`（不接受 `USERPROFILE` 等其它变量）；两个来源都拿不到时
/// 返回空串，调用方需要自行处理空路径。
pub(crate) fn get_home_dir() -> String {
    if let Some(home) = std::env::var_os("HOME")
        && !home.is_empty()
    {
        return home.to_string_lossy().into_owned();
    }
    dirs::home_dir()
        .map(|path| path.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// `SpawnOptions.env` 的形态：`None` 继承宿主环境；`Some` 用 `env_clear + envs` 替换。
///
/// `CaptureOptions.env` 里的变量覆盖到 `get_env()` 之上：已存在的键原地替换、新键追加，
/// 其余环境变量原样保留——对应 Node spawn 默认不设 `env` 时的继承语义；TS 里显式传
/// `env: { ...process.env, ...overrides }` 的写法在这里由 [`env_for_spawn`] 统一实现。
fn env_for_spawn(overrides: &[(String, String)]) -> Vec<(OsString, OsString)> {
    let mut env: Vec<(OsString, OsString)> = get_env();
    for (key, value) in overrides {
        let os_key = OsString::from(key);
        if let Some(slot) = env.iter_mut().find(|(existing, _)| *existing == os_key) {
            slot.1 = OsString::from(value);
        } else {
            env.push((os_key, OsString::from(value)));
        }
    }
    env
}

/// 把 [`io::Error`] 的 message 包成公开错误。
///
/// spawn / wait / 管道读取失败的统一出口：错误消息就是 OS 的 errno 描述，不做二次
/// 包装，因此调用方看到的文案因平台而异。
pub(crate) fn io_error(error: io::Error) -> PackageManagerError {
    err(error.to_string())
}

/// 把 argv 拼成 TS 的 `args.join(" ")`。
fn join_args(args: &[String]) -> String {
    args.join(" ")
}

/// 退出码的 Node 文本：正常退出 `code {n}`，被信号杀死 `signal {name}`。
///
/// 仅用于 [`run_command_capture`] 的失败文案（对应 `close` 事件的 `(code, signal)`）；
/// [`run_command`] 用 [`exit_code_text`]（对应 `exit` 事件，信号时 `code` 是 `null`），
/// 两者的文案必须保持这一区分。
fn exit_status_text(status: ExitStatus) -> String {
    if let Some(code) = status.code() {
        return format!("code {code}");
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(signal) = status.signal() {
            return format!("signal {}", signal_name(signal));
        }
    }
    "signal unknown".to_string()
}

/// `runCommand` 的错误文本里 `code` 直接来自 Node 的 `exit` 事件：信号终止时是 `null`。
///
/// `ExitStatus::code()` 在信号终止时恰好返回 `None`，所以 `None -> "null"` 就完成了
/// Node 语义的映射；Unix 的信号名文本由 [`exit_status_text`] 负责另一个出口。
fn exit_code_text(status: ExitStatus) -> String {
    match status.code() {
        Some(code) => code.to_string(),
        None => "null".to_string(),
    }
}

#[cfg(unix)]
fn signal_name(signal: i32) -> String {
    // 信号编号随平台不同；Node 的 `child.on("close", (code, signal))` 用 libuv/平台的
    // 名字表。Linux 与 macOS 的编号在前 16 个信号之后开始分叉。
    #[cfg(target_os = "macos")]
    let name = match signal {
        1 => "SIGHUP",
        2 => "SIGINT",
        3 => "SIGQUIT",
        4 => "SIGILL",
        5 => "SIGTRAP",
        6 => "SIGABRT",
        7 => "SIGEMT",
        8 => "SIGFPE",
        9 => "SIGKILL",
        10 => "SIGBUS",
        11 => "SIGSEGV",
        12 => "SIGSYS",
        13 => "SIGPIPE",
        14 => "SIGALRM",
        15 => "SIGTERM",
        16 => "SIGURG",
        17 => "SIGSTOP",
        18 => "SIGTSTP",
        19 => "SIGCONT",
        20 => "SIGCHLD",
        21 => "SIGTTIN",
        22 => "SIGTTOU",
        23 => "SIGIO",
        24 => "SIGXCPU",
        25 => "SIGXFSZ",
        26 => "SIGVTALRM",
        27 => "SIGPROF",
        28 => "SIGWINCH",
        29 => "SIGINFO",
        30 => "SIGUSR1",
        31 => "SIGUSR2",
        _ => return "unknown".to_string(),
    };
    #[cfg(all(unix, not(target_os = "macos")))]
    let name = match signal {
        1 => "SIGHUP",
        2 => "SIGINT",
        3 => "SIGQUIT",
        4 => "SIGILL",
        5 => "SIGTRAP",
        6 => "SIGABRT",
        7 => "SIGBUS",
        8 => "SIGFPE",
        9 => "SIGKILL",
        10 => "SIGUSR1",
        11 => "SIGSEGV",
        12 => "SIGUSR2",
        13 => "SIGPIPE",
        14 => "SIGALRM",
        15 => "SIGTERM",
        16 => "SIGSTKFLT",
        17 => "SIGCHLD",
        18 => "SIGCONT",
        19 => "SIGSTOP",
        20 => "SIGTSTP",
        21 => "SIGTTIN",
        22 => "SIGTTOU",
        23 => "SIGURG",
        24 => "SIGXCPU",
        25 => "SIGXFSZ",
        26 => "SIGVTALRM",
        27 => "SIGPROF",
        28 => "SIGWINCH",
        29 => "SIGIO",
        30 => "SIGPWR",
        31 => "SIGSYS",
        _ => return "unknown".to_string(),
    };
    name.to_string()
}

/// 把底层 [`io::Error`] 转成 Node 的 spawn 错误文本：`spawn <command> ENOENT`。
///
/// `kiana_core::child_process` 的同类映射是 crate 私有；stdout 被接管时本模块直接使用
/// `tokio::process`（为了把 stdout/stderr 都指向 fd 2），因此在这里复刻同一文案。
/// errno 只按 `ErrorKind` 近似映射（`NotFound -> ENOENT` 等），没有精确复刻每个
/// 平台的 errno 值；测试 `takeover_spawn_errors_use_node_error_message` 锁定 ENOENT 情形。
fn node_spawn_error(command: &str, verb: &str, error: io::Error) -> io::Error {
    let code = match error.kind() {
        io::ErrorKind::NotFound => "ENOENT",
        io::ErrorKind::PermissionDenied => "EACCES",
        io::ErrorKind::AlreadyExists => "EEXIST",
        io::ErrorKind::InvalidInput => "EINVAL",
        _ => "UNKNOWN",
    };
    io::Error::new(error.kind(), format!("{verb} {command} {code}"))
}

/// `spawnCommand`：inherit（或 stdout 被接管时 `["ignore", 2, 2]`）。
///
/// 两个分支的关键差异：
/// - **普通路径**：`StdioMode::Inherit`，由 `kiana_core::child_process` 处理错误文案；
/// - **stdout 被接管路径**（仅 Unix）：子进程输出必须可见，但父进程的 stdout 已被
///   换成接管用的 fd，因此把 stdout/stderr 都 dup 到 fd 2。这里必须用
///   `tokio::process` 直接构造（`kiana_core` 的 `SpawnOptions` 表达不了这种 stdio），
///   并且 spawn 前先 `env_clear`——`tokio::process` 的 `envs` 只是追加而非替换。
///   空命令名的检查是为了复刻 Node `spawn("")` 的 `TypeError` 文案。
fn spawn_command(
    command: &str,
    args: &[String],
    cwd: Option<&str>,
) -> io::Result<tokio::process::Child> {
    let env = env_for_spawn(&[]);
    if kiana_core::output_guard::is_stdout_taken_over() {
        #[cfg(unix)]
        {
            if command.is_empty() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "The argument 'file' cannot be empty. Received ''",
                ));
            }
            let mut cmd = tokio::process::Command::new(command);
            cmd.args(args);
            if let Some(cwd) = cwd
                && !cwd.is_empty()
            {
                cmd.current_dir(cwd);
            }
            cmd.stdin(Stdio::null());
            // Node 的 `["ignore", 2, 2]`：stdout 与 stderr 都指向 fd 2。dup 一份 fd 2，
            // 避免 `Stdio` 在 drop 时关闭父进程真正的 stderr。
            let stderr_dup = stderr_dup_stdio()?;
            cmd.stdout(stderr_dup);
            cmd.stderr(stderr_dup_stdio()?);
            cmd.env_clear();
            cmd.envs(env);
            // Node 的 `error.message` 是 `spawn <cmd> <ERRNO>`，与普通分支一致。
            return cmd
                .spawn()
                .map_err(|error| node_spawn_error(command, "spawn", error));
        }
        #[cfg(not(unix))]
        {
            let options = kiana_core::child_process::SpawnOptions {
                cwd: cwd.map(std::path::PathBuf::from),
                env: Some(env),
                ..kiana_core::child_process::SpawnOptions::with_stdio(
                    kiana_core::child_process::StdioMode::Inherit,
                )
            };
            return kiana_core::child_process::spawn_process(command, args, &options);
        }
    }
    let options = kiana_core::child_process::SpawnOptions {
        cwd: cwd.map(std::path::PathBuf::from),
        env: Some(env),
        ..kiana_core::child_process::SpawnOptions::with_stdio(
            kiana_core::child_process::StdioMode::Inherit,
        )
    };
    kiana_core::child_process::spawn_process(command, args, &options)
}

#[cfg(unix)]
fn stderr_dup_stdio() -> io::Result<Stdio> {
    use std::os::fd::AsFd;
    let stderr = std::io::stderr();
    let owned = stderr.as_fd().try_clone_to_owned()?;
    Ok(Stdio::from(owned))
}

/// `spawnCaptureCommand`：stdio `["ignore", "pipe", "pipe"]`，环境为 base + options.env。
///
/// stdin 用 `Ignore`（读端立即 EOF）而不是 `Null`：行为对子进程不可区分，这里跟随
/// TS 的 `ignore`。capture 路径不需要接管分支，因为它的输出进管道而不进终端。
fn spawn_capture_command(
    command: &str,
    args: &[String],
    cwd: Option<&str>,
    env_overrides: &[(String, String)],
) -> io::Result<tokio::process::Child> {
    let env = env_for_spawn(env_overrides);
    let options = kiana_core::child_process::SpawnOptions {
        cwd: cwd.map(std::path::PathBuf::from),
        stdin: kiana_core::child_process::StdioMode::Ignore,
        stdout: kiana_core::child_process::StdioMode::Pipe,
        stderr: kiana_core::child_process::StdioMode::Pipe,
        env: Some(env),
        ..Default::default()
    };
    kiana_core::child_process::spawn_process(command, args, &options)
}

/// `runCommand(command, args, options?): Promise<void>`。
///
/// 等价 Node 的 `child.on("exit")`：进程退出即返回，不等管道 EOF，也不读任何输出
/// （stdio 是 inherit 或 dup 到 fd 2，输出直接进宿主的终端）。非零退出（含信号终止）
/// 返回 `"{command} {args} failed with code {code}"`；spawn 失败（如 ENOENT）也是
/// `Err`，文案见 [`node_spawn_error`]。
pub(crate) async fn run_command(
    command: &str,
    args: &[String],
    cwd: Option<&str>,
) -> Result<(), PackageManagerError> {
    let mut child = spawn_command(command, args, cwd).map_err(io_error)?;
    let status = child.wait().await.map_err(io_error)?;
    if status.code() == Some(0) {
        return Ok(());
    }
    Err(err(format!(
        "{command} {} failed with code {}",
        join_args(args),
        exit_code_text(status)
    )))
}

/// `runCommandCapture` 的选项（`cwd` / `timeoutMs` / `env`）。
#[derive(Debug, Clone, Default)]
pub(crate) struct CaptureOptions {
    /// 工作目录。
    pub cwd: Option<String>,
    /// 超时毫秒数（Node 的 `setTimeout`）。
    pub timeout_ms: Option<u64>,
    /// 追加/覆盖的环境变量。
    pub env: Vec<(String, String)>,
}

/// 按 Node 的可读流行为解码管道：每个数据块单独 `Buffer.toString()`。
///
/// Node 的 `stdout += data.toString()` 在块边界遇到被截断的多字节 UTF-8 序列时会产生
/// U+FFFD；libuv 的读缓冲区是 64 KiB，这里用同样的块大小近似。
async fn decode_pipe<R: tokio::io::AsyncRead + Unpin>(pipe: Option<R>) -> String {
    let Some(mut pipe) = pipe else {
        return String::new();
    };
    let mut buffer = vec![0u8; 64 * 1024];
    let mut output = String::new();
    loop {
        match pipe.read(&mut buffer).await {
            Ok(0) => break,
            Ok(read) => output.push_str(&String::from_utf8_lossy(&buffer[..read])),
            Err(_) => break,
        }
    }
    output
}

/// `runCommandCapture(command, args, options?): Promise<string>`。
///
/// 生命周期与 Node 的 `close` 事件对齐，按顺序：
/// 1. spawn（stdio `["ignore", "pipe", "pipe"]`），并起两个任务分别按块解码
///    stdout/stderr；
/// 2. `select` 押注 `child.wait()` 与超时；超时只负责 kill，之后仍要等两条管道到 EOF
///    才返回（Node 的 `close` 在 stdio 流关闭时触发，而非进程退出时）；
/// 3. 成功返回 `js_trim(stdout)`（不含 stderr）；非零退出/信号返回
///    `"{command} {args} failed with {code|signal}: {stderr 或 stdout}"`。
///
/// 失败不可重试，也不清理已产生的外部副作用：调用方拿到 `Err` 时子进程已经退出。
pub(crate) async fn run_command_capture(
    command: &str,
    args: &[String],
    options: &CaptureOptions,
) -> Result<String, PackageManagerError> {
    let mut child = spawn_capture_command(command, args, options.cwd.as_deref(), &options.env)
        .map_err(io_error)?;

    let stdout_pipe = child.stdout.take();
    let stderr_pipe = child.stderr.take();
    let stdout_task = tokio::spawn(decode_pipe(stdout_pipe));
    let stderr_task = tokio::spawn(decode_pipe(stderr_pipe));

    let timeout_ms = options.timeout_ms;
    let sleep = async move {
        match timeout_ms {
            Some(ms) => tokio::time::sleep(Duration::from_millis(ms)).await,
            None => std::future::pending::<()>().await,
        }
    };
    tokio::pin!(sleep);

    let mut timed_out = false;
    let mut status: Option<io::Result<ExitStatus>> = None;
    {
        let wait = child.wait();
        tokio::pin!(wait);
        tokio::select! {
            result = &mut wait => {
                status = Some(result);
            }
            _ = &mut sleep, if options.timeout_ms.is_some() => {
                timed_out = true;
            }
        }
    }
    if timed_out {
        let _ = child.start_kill();
        let _ = child.wait().await;
    }

    // Node 的 `close`：stdout/stderr 都到 EOF 才结束。
    let stdout = stdout_task.await.unwrap_or_default();
    let stderr = stderr_task.await.unwrap_or_default();

    let status = match status {
        // select 押注 `child.wait()` 成功时拿到退出状态。
        Some(result) => result.map_err(io_error)?,
        // 超时分支：上面已 kill 并 `child.wait().await` 回收过一次，这里再次 wait 只是
        // 取回（随后因 timed_out 提前返回而被丢弃的）退出状态，也保证对 `Child` 的
        // wait 是顺序而非并发的。
        None => child.wait().await.map_err(io_error)?,
    };

    if timed_out {
        return Err(err(format!(
            "{command} {} timed out after {}ms",
            join_args(args),
            options.timeout_ms.unwrap_or(0)
        )));
    }
    if status.code() == Some(0) {
        return Ok(js_trim(&stdout).to_string());
    }
    Err(err(format!(
        "{command} {} failed with {}: {}",
        join_args(args),
        exit_status_text(status),
        if stderr.is_empty() { stdout } else { stderr }
    )))
}

/// `runCommandSync(command, args): string`。
///
/// 同步版 `runCommandCapture`（`spawn_process_sync` 阻塞当前线程直到子进程退出），
/// 但细节有三点不同：
/// - spawn 失败文案是 `Failed to run {command} {args}: {error}`，不是 `spawn <cmd> <ERRNO>`；
/// - 非零退出（或没有退出码）的文案也是 `Failed to run ...`，细节取 stderr，为空则
///   stdout，再为空就是空串（对应 TS 的 `error.message || stderr || stdout` 中后两段
///   （`stderr || stdout`））；
/// - 成功时返回 `stdout`，stdout 为空则退回 `stderr`，最后统一 `trim`（与 capture 的
///   “只用 stdout”不同）。
pub(crate) fn run_command_sync(
    command: &str,
    args: &[String],
) -> Result<String, PackageManagerError> {
    let options = kiana_core::child_process::SpawnSyncOptions {
        stdin: kiana_core::child_process::StdioMode::Ignore,
        stdout: kiana_core::child_process::StdioMode::Pipe,
        stderr: kiana_core::child_process::StdioMode::Pipe,
        env: Some(get_env()),
        ..Default::default()
    };
    let result = kiana_core::child_process::spawn_process_sync(command, args, &options);
    if let Some(error) = result.error {
        return Err(err(format!(
            "Failed to run {command} {}: {}",
            join_args(args),
            error
        )));
    }
    if result.status != Some(0) {
        let detail = if !result.stderr.is_empty() {
            result.stderr.clone()
        } else {
            result.stdout.clone()
        };
        return Err(err(format!(
            "Failed to run {command} {}: {detail}",
            join_args(args)
        )));
    }
    let value = if !result.stdout.is_empty() {
        result.stdout
    } else if !result.stderr.is_empty() {
        result.stderr
    } else {
        String::new()
    };
    Ok(js_trim(&value).to_string())
}

#[cfg(test)]
mod tests {
    use std::pin::Pin;
    use std::task::{Context, Poll};

    /// 按预设块返回数据的 reader（模拟 Node 的 `data` 事件边界）。
    struct ChunkedReader {
        chunks: Vec<&'static [u8]>,
        index: usize,
    }

    impl tokio::io::AsyncRead for ChunkedReader {
        fn poll_read(
            mut self: Pin<&mut Self>,
            _context: &mut Context<'_>,
            buffer: &mut tokio::io::ReadBuf<'_>,
        ) -> Poll<std::io::Result<()>> {
            if self.index >= self.chunks.len() {
                return Poll::Ready(Ok(()));
            }
            let chunk = self.chunks[self.index];
            self.index += 1;
            buffer.put_slice(chunk);
            Poll::Ready(Ok(()))
        }
    }

    #[cfg(unix)]
    #[test]
    fn signal_names_use_the_platform_table() {
        #[cfg(target_os = "linux")]
        {
            assert_eq!(signal_name(1), "SIGHUP");
            assert_eq!(signal_name(10), "SIGUSR1");
            assert_eq!(signal_name(17), "SIGCHLD");
            assert_eq!(signal_name(30), "SIGPWR");
            assert_eq!(signal_name(31), "SIGSYS");
        }
        #[cfg(target_os = "macos")]
        {
            assert_eq!(signal_name(1), "SIGHUP");
            assert_eq!(signal_name(16), "SIGURG");
            assert_eq!(signal_name(20), "SIGCHLD");
            assert_eq!(signal_name(29), "SIGINFO");
            assert_eq!(signal_name(30), "SIGUSR1");
            assert_eq!(signal_name(31), "SIGUSR2");
        }
        assert_eq!(signal_name(9), "SIGKILL");
        assert_eq!(signal_name(15), "SIGTERM");
    }

    #[tokio::test]
    async fn decode_pipe_decodes_each_chunk_like_node() {
        // `€` (E2 82 AC) 被拆到两个数据块：Node 的 `chunk.toString()` 每块各产生 U+FFFD。
        let reader = ChunkedReader {
            chunks: vec![&[0xE2], &[0x82, 0xAC]],
            index: 0,
        };
        assert_eq!(decode_pipe(Some(reader)).await, "\u{FFFD}\u{FFFD}\u{FFFD}");
        // 完整的多字节序列在同一块里正常解码。
        let reader = ChunkedReader {
            chunks: vec!["€ ok".as_bytes()],
            index: 0,
        };
        assert_eq!(decode_pipe(Some(reader)).await, "€ ok");
    }

    use super::*;

    #[test]
    fn js_trim_uses_js_whitespace() {
        assert_eq!(js_trim("\u{FEFF} a \u{3000}"), "a");
        assert_eq!(js_trim("\u{200B}a"), "\u{200B}a");
    }

    #[tokio::test]
    async fn capture_returns_trimmed_stdout_and_ignores_stderr_on_success() {
        let script = if cfg!(windows) {
            vec!["/C".to_string(), "echo hi & echo err 1>&2".to_string()]
        } else {
            vec!["-c".to_string(), "echo hi; echo err 1>&2".to_string()]
        };
        let command = if cfg!(windows) { "cmd" } else { "sh" };
        let output = run_command_capture(command, &script, &CaptureOptions::default())
            .await
            .expect("capture");
        assert_eq!(output, "hi");
    }

    #[tokio::test]
    async fn capture_reports_nonzero_status_with_stderr() {
        let script = if cfg!(windows) {
            vec!["/C".to_string(), "echo boom 1>&2 & exit 3".to_string()]
        } else {
            vec!["-c".to_string(), "echo boom 1>&2; exit 3".to_string()]
        };
        let command = if cfg!(windows) { "cmd" } else { "sh" };
        let error = run_command_capture(command, &script, &CaptureOptions::default())
            .await
            .expect_err("must fail");
        assert!(error.message().starts_with("sh -c"));
        assert!(error.message().contains("failed with code 3: boom"));
    }

    #[tokio::test]
    async fn capture_times_out_and_reports_timeout_text() {
        let script = if cfg!(windows) {
            vec!["/C".to_string(), "ping -n 10 127.0.0.1 >NUL".to_string()]
        } else {
            vec!["-c".to_string(), "exec sleep 10".to_string()]
        };
        let command = if cfg!(windows) { "cmd" } else { "sh" };
        let options = CaptureOptions {
            timeout_ms: Some(50),
            ..Default::default()
        };
        let error = run_command_capture(command, &script, &options)
            .await
            .expect_err("must time out");
        assert!(error.message().ends_with("timed out after 50ms"));
    }

    #[tokio::test]
    async fn takeover_spawn_errors_use_node_error_message() {
        let command = "kiana-fixer-missing-command-xyz";
        kiana_core::output_guard::take_over_stdout();
        let result = run_command(command, &[], None).await;
        kiana_core::output_guard::restore_stdout();
        let error = result.expect_err("missing command");
        assert_eq!(error.message(), format!("spawn {command} ENOENT"));
    }

    #[test]
    fn preserves_argv_entries_containing_spaces() {
        let value = "C:\\Users\\A B\\.kiana\\npm";
        let args = if cfg!(windows) {
            vec!["/C".to_string(), format!("echo {value}")]
        } else {
            vec![
                "-c".to_string(),
                "printf '%s' \"$1\"".to_string(),
                "--".to_string(),
                value.to_string(),
            ]
        };
        let command = if cfg!(windows) { "cmd" } else { "sh" };
        assert_eq!(run_command_sync(command, &args).expect("sync"), value);
    }

    #[tokio::test]
    async fn capture_waits_for_pipes_to_close_after_the_child_exits() {
        let args = if cfg!(windows) {
            vec!["/C".to_string(), "echo early".to_string()]
        } else {
            vec![
                "-c".to_string(),
                "(sleep 0.2; printf late) & printf early".to_string(),
            ]
        };
        let command = if cfg!(windows) { "cmd" } else { "sh" };
        let started = std::time::Instant::now();
        let output = run_command_capture(command, &args, &CaptureOptions::default())
            .await
            .expect("capture");
        assert_eq!(output, "earlylate");
        if !cfg!(windows) {
            assert!(
                started.elapsed() >= std::time::Duration::from_millis(150),
                "capture must wait for the pipe to close"
            );
        }
    }

    #[tokio::test]
    async fn run_command_reports_code_null_for_signals() {
        let args = if cfg!(windows) {
            vec!["/C".to_string(), "exit 1".to_string()]
        } else {
            vec!["-c".to_string(), "kill -TERM $$".to_string()]
        };
        let command = if cfg!(windows) { "cmd" } else { "sh" };
        let error = run_command(command, &args, None).await.expect_err("signal");
        let expected = if cfg!(windows) {
            "failed with code 1"
        } else {
            "failed with code null"
        };
        assert!(error.message().ends_with(expected), "{}", error.message());
    }

    #[tokio::test]
    async fn capture_reports_signal_for_signal_termination() {
        let args = if cfg!(windows) {
            vec!["/C".to_string(), "exit 1".to_string()]
        } else {
            vec!["-c".to_string(), "kill -TERM $$".to_string()]
        };
        let command = if cfg!(windows) { "cmd" } else { "sh" };
        let error = run_command_capture(command, &args, &CaptureOptions::default())
            .await
            .expect_err("signal");
        let needle = if cfg!(windows) {
            "failed with code 1"
        } else {
            "failed with signal SIGTERM"
        };
        assert!(error.message().contains(needle), "{}", error.message());
    }

    #[test]
    fn sync_returns_trimmed_stdout_and_errors_use_stderr() {
        let (command, args) = if cfg!(windows) {
            ("cmd", vec!["/C".to_string(), "echo value".to_string()])
        } else {
            ("sh", vec!["-c".to_string(), "echo value".to_string()])
        };
        assert_eq!(run_command_sync(command, &args).expect("ok"), "value");

        let (command, args) = if cfg!(windows) {
            ("cmd", vec!["/C".to_string(), "exit 2".to_string()])
        } else {
            ("sh", vec!["-c".to_string(), "exit 2".to_string()])
        };
        let error = run_command_sync(command, &args).expect_err("fail");
        assert_eq!(error.message(), "Failed to run sh -c exit 2: ");
    }
}
