//! utils/shell.ts。
//!
//! 逐字移植 `packages/coding-agent/src/utils/shell.ts`（264 行）与
//! `spec/ca-cli.md` §2.21.27 / `spec/ca-tools-compaction-export.md` §2.28.1 的契约。
//!
//! ## 平台注入（测试缝）
//!
//! TS 测试用 `vi.mock` 替换 `node:fs.accessSync` / `child_process.spawnSync` 并改写
//! `process.platform`。Rust 侧等价的入口是参数化的
//! [`shell_config_with`] / [`power_shell_config_with`] / [`find_executable_on_path_with`] /
//! [`kill_process_tree_with`]（`#[doc(hidden)]`，**test hook, not public API**）：平台、env、
//! 存在性检查、PATH 查找、kill/spawn 全部注入，因此 Windows 断言可以在 Linux 上跑。
//!
//! `getShellConfig("")` 与 TS 一样把空串当作“未设置”（`if (customShellPath)` 的 falsy），
//! 继续走平台搜索；只有非空的显式路径才会产生 `Custom shell path not found`。
//!
//! ## 调用方
//!
//! - `kiana-tools` 的 Bash 工具：`get_shell_config` 决定可执行文件/参数/传输方式，
//!   `get_shell_env` 提供子进程环境，`sanitize_binary_output` 清理输出；
//! - `kiana-tools` 的 PowerShell 工具：`get_power_shell_config`（非 Windows 平台直接报
//!   `The powershell tool is only available on Windows.`，工具据此不可用）；
//! - `kiana-config/src/resolve_config_value.rs`：`!command` 执行消费同一份 [`ShellConfig`]
//!   与 [`CommandTransport`]；
//! - `kiana-rpc-print` / `kiana-interactive`：退出或取消时调用
//!   [`kill_tracked_detached_children`]；
//! - `kiana-coding-agent/src/sdk_exports.rs`：以 TS 原名再导出两个配置函数。
//!
//! ## 分离子进程集合
//!
//! [`track_detached_child_pid`] / [`untrack_detached_child_pid`] 维护进程级的
//! `Mutex<Option<HashSet<u32>>>`。清理函数 [`kill_tracked_detached_children`] 先在锁内取
//! 快照并清空集合，再在锁外逐个 kill，避免在 kill 期间持锁。集合只增删调用方给出的 PID，
//! 不校验进程是否仍存活。
//!
//! ## 失败模式
//!
//! 配置解析失败返回 [`ShellConfigError`]（`Display` 文案逐字对齐 TS，见各变体）；运行时的
//! kill / taskkill 都是 best-effort：本模块不返回错误，也不重试。
//!
//! ## 已知偏差
//!
//! `shellPathExists` 在 TS 里是 `accessSync(path, F_OK)`，**故意不用 `existsSync`**，因为
//! Windows 上 `existsSync` 的额外 `stat` 会拒绝 Microsoft Store 的可执行别名
//! （nodejs/node#36790）。std 没有 `access()` 的跨平台等价物，而 §8 第 26 条禁止引入
//! `windows-sys`，所以 Rust 版在 Windows 上退化为 `Path::exists`（stat 语义）：可执行别名
//! 会被拒绝。Unix 上 `Path::exists` 与 `access(F_OK)` 等价（都是 `stat`），无差异。

use std::ffi::OsString;
use std::fmt;

use regex::Regex;
use std::sync::LazyLock;
use std::sync::Mutex;

use crate::child_process::{SpawnSyncOptions, SpawnSyncResult, spawn_process_sync};

/// `POWERSHELL_ARGS`：`getPowerShellConfig()` 的固定参数。
///
/// 依次是 `-NoProfile`、`-NonInteractive`、`-ExecutionPolicy Bypass`、`-Command`；调用方在
/// 末尾 `-Command` 之后追加要执行的脚本文本（`ShellConfig.args` 只含这 5 项）。
pub const POWERSHELL_ARGS: [&str; 5] = [
    "-NoProfile",
    "-NonInteractive",
    "-ExecutionPolicy",
    "Bypass",
    "-Command",
];

/// 命令如何传给 shell：`argv`（`bash -c <command>`）或 `stdin`（`bash -s` + stdin）。
///
/// 对应 TS 的可选字段 `commandTransport?: "argv" | "stdin"`；Rust 用 `Option<Self>` 表达
/// “未设置”。本模块的构造函数产生的普通 bash 配置是 `None`，legacy WSL 才是
/// `Some(Stdin)`；现有调用方都只把 `Some(Stdin)` 当作特殊情形，`None` 与 `Some(Argv)`
/// 的行为相同（命令追加到 `args` 末尾）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandTransport {
    /// 命令作为 shell 的最后一个参数（`bash -c <command>`）。
    Argv,
    /// 命令从 stdin 传入（`bash -s`，用于旧版 WSL 的 `bash.exe`）。
    Stdin,
}

/// `ShellConfig`：解析出的 shell 及其参数。
///
/// 调用方按下面的方式组装子进程：`program = shell`，`args = args`；若
/// `command_transport != Some(Stdin)`，再把命令文本追加为最后一个参数，否则把命令写进
/// stdin。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShellConfig {
    /// 可执行文件路径或名字（`/bin/bash`、`C:\...\bash.exe`、`sh` 等）。
    pub shell: String,
    /// 固定参数，不含命令文本本身（`bash -c` 的 `-c`、PowerShell 的 `POWERSHELL_ARGS`）。
    pub args: Vec<String>,
    /// 命令传输方式；`None` 与 `Some(Argv)` 在现有调用方中等价。
    pub command_transport: Option<CommandTransport>,
}

/// `ShellConfigError`：`getShellConfig` / `getPowerShellConfig` 抛出的错误，`Display`
/// 逐字等于 TS 的 `Error.message`。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ShellConfigError {
    /// `Custom shell path not found: ${path}`
    ///
    /// 非空的显式 `shellPath` 未通过存在性检查时返回。
    CustomShellPathNotFound(String),
    /// `No bash shell found. Options:\n...`
    ///
    /// Windows 上 ProgramFiles 候选与 PATH 都没有 bash。`searched` 只包含探测过的
    /// ProgramFiles 候选路径（不含 PATH 查询结果），用于拼出错误文案的列表。
    NoBashShellFound { searched: Vec<String> },
    /// `The powershell tool is only available on Windows.`
    ///
    /// 平台不是 Win32 时 `getPowerShellConfig` 立即返回这个错误。
    PowerShellOnlyOnWindows,
    /// `No PowerShell executable found. Install PowerShell or add powershell.exe/pwsh.exe to PATH.`
    ///
    /// Windows 上 PATH 与两个已知安装位置都没有找到 PowerShell。
    NoPowerShellExecutable,
}

impl fmt::Display for ShellConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ShellConfigError::CustomShellPathNotFound(path) => {
                write!(f, "Custom shell path not found: {path}")
            }
            ShellConfigError::NoBashShellFound { searched } => {
                let list = searched
                    .iter()
                    .map(|path| format!("  {path}"))
                    .collect::<Vec<_>>()
                    .join("\n");
                write!(
                    f,
                    "No bash shell found. Options:\n  1. Install Git for Windows: https://git-scm.com/download/win\n  2. Add your bash to PATH (Cygwin, MSYS2, etc.)\n  3. Set shellPath in settings.json\n\nSearched Git Bash in:\n{list}"
                )
            }
            ShellConfigError::PowerShellOnlyOnWindows => {
                write!(f, "The powershell tool is only available on Windows.")
            }
            ShellConfigError::NoPowerShellExecutable => write!(
                f,
                "No PowerShell executable found. Install PowerShell or add powershell.exe/pwsh.exe to PATH."
            ),
        }
    }
}

impl std::error::Error for ShellConfigError {}

/// 平台（用于注入；`Platform::current()` 对应 `process.platform`）。
///
/// shell 探测与 kill 路径的平台判断都接受这个枚举（而不是内部直接读 `cfg!(windows)`），
/// 这样 Windows 分支的测试可以在 Linux 上运行（见模块文档的“平台注入”）。除
/// [`Platform::current`] 自身按编译目标映射平台外，只有 [`get_shell_env`] 的 PATH 分隔符
/// 用 `cfg!(windows)` 选取。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Platform {
    /// Windows（`process.platform === "win32"`）。
    Win32,
    /// macOS（`"darwin"`）。
    Darwin,
    /// Linux（`"linux"`）。
    Linux,
    /// 其它平台；在 shell 探测与 kill 逻辑中按 Unix 语义处理。
    Other,
}

impl Platform {
    /// 当前平台的等价物。
    ///
    /// 由编译目标决定（`cfg!` 在编译期展开），不读取任何运行时环境变量。
    pub fn current() -> Self {
        if cfg!(windows) {
            Platform::Win32
        } else if cfg!(target_os = "macos") {
            Platform::Darwin
        } else if cfg!(target_os = "linux") {
            Platform::Linux
        } else {
            Platform::Other
        }
    }

    fn is_windows(self) -> bool {
        self == Platform::Win32
    }
}

/// 进程信号（注入用的最小集合）。
///
/// 只覆盖 `kill_process_tree_with` 用到的两个信号；真实实现把 `pid` 直接交给
/// `libc::kill`，负值表示进程组。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KillSignal {
    /// SIGSTOP：macOS 上先暂停整个进程组，减少取消时的启动竞态。
    SigStop,
    /// SIGKILL：不可捕获的强制终止。
    SigKill,
}

/// `SystemRoot` 必须形如 `<盘符>:\` 或 `<盘符>:/`（大小写不敏感）才参与 legacy WSL 判定。
static SYSTEM_ROOT_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)^[a-z]:[\\/]").expect("valid SystemRoot regex"));
/// 没有可用 `SystemRoot` 时的回退规则：
/// `<盘符>:\windows\(system32|sysnative)\bash.exe`（匹配前已转小写）。
static WSL_BASH_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^[a-z]:\\windows\\(?:system32|sysnative)\\bash\.exe$")
        .expect("valid WSL bash regex")
});

/// `isLegacyWslBashPath(path)`（`SystemRoot` 由调用方注入）。
///
/// 判定规则：
///
/// 1. 先用 `paths::win32_normalize_for_shell` 做 win32 归一化并转小写；
/// 2. `system_root` 非空时：若它不匹配 `^[a-z]:[\\/]` 直接返回 false（**不**回退到正则），
///    否则与 `<SystemRoot>\System32\bash.exe`、`<SystemRoot>\Sysnative\bash.exe` 比较；
/// 3. `system_root` 缺失或为空串时：匹配 `^[a-z]:\\windows\\(system32|sysnative)\\bash\.exe$`。
///
/// 因此 `C:\Windows\System32\bash.exe` 在 `SystemRoot="WinNT"`（非法值）时不会被识别为
/// legacy WSL；测试 `legacy_wsl_detection` 锁定这些分支。
fn is_legacy_wsl_bash_path_with(path: &str, system_root: Option<&str>) -> bool {
    let normalized = crate::paths::win32_normalize_for_shell(path).to_lowercase();
    if let Some(system_root) = system_root.filter(|root| !root.is_empty()) {
        if !SYSTEM_ROOT_RE.is_match(system_root) {
            return false;
        }
        return ["System32", "Sysnative"].iter().any(|directory| {
            normalized == path_join_windows(&[system_root, directory, "bash.exe"]).to_lowercase()
        });
    }
    WSL_BASH_RE.is_match(&normalized)
}

fn path_join_windows(parts: &[&str]) -> String {
    // Node 在 win32 上 `join` 用反斜杠；`path_join` 在 Linux 上会用 `/`，
    // 因此这里显式用 win32 语义（测试在 Linux 上也要得到反斜杠）。
    crate::paths::win32_join_for_shell(parts)
}

/// `getBashShellConfig(shell)`：legacy WSL 的 bash 走 `-s` + stdin。
///
/// 旧版 WSL 启动器从 stdin 读脚本，因此参数是 `-s` 且 `command_transport =
/// Some(Stdin)`；其余 bash（Git Bash、`/bin/bash`、PATH 上的 bash、`sh`）都返回 `-c` 与
/// `None`，由调用方把命令追加成下一个参数。
fn bash_shell_config(shell: &str, system_root: Option<&str>) -> ShellConfig {
    if is_legacy_wsl_bash_path_with(shell, system_root) {
        ShellConfig {
            shell: shell.to_string(),
            args: vec!["-s".to_string()],
            command_transport: Some(CommandTransport::Stdin),
        }
    } else {
        ShellConfig {
            shell: shell.to_string(),
            args: vec!["-c".to_string()],
            command_transport: None,
        }
    }
}

/// `shellPathExists(path)`：`accessSync(path, F_OK)`（见模块文档的 Windows 偏差）。
///
/// `Path::exists` 把“不存在”“无权限”“路径非法”等所有失败都折叠为 false，不区分原因；
/// 这正是存在性探测需要的语义。相对路径按当前工作目录解析。
fn shell_path_exists(path: &str) -> bool {
    std::path::Path::new(path).exists()
}

/// 取进程环境变量（JS 里空串是 falsy，这里同样把空串当作未设置）。
///
/// 非 UTF-8 的值走 `to_string_lossy`（替换非法字节）；下游只把它们拼进路径或错误文案。
fn env_lookup(name: &str) -> Option<String> {
    std::env::var_os(name)
        .map(|value| value.to_string_lossy().into_owned())
        .filter(|value| !value.is_empty())
}

/// `findExecutableOnPath(executable)` 的真实实现（`where` / `which`）。
fn find_executable_on_path(executable: &str) -> Option<String> {
    find_executable_on_path_with(
        Platform::current(),
        executable,
        &shell_path_exists,
        &spawn_process_sync,
    )
}

/// `findExecutableOnPath(executable)` 的注入版本：平台、存在性检查与 `spawnSync` 都可替换。
///
/// Windows 用 `where` 并**复检**第一行（`access(F_OK)`）；Unix 用 `which` 且只信 stdout
/// （Termux / 特殊文件系统）。**test hook, not public API**。
///
/// 两种平台都要求 `status == Some(0)`、stdout 非空，且 `first_line` 取到的第一行非空；
/// `spawn_sync` 返回的 `error` 字段（超时/启动失败）不参与判定。超时固定为 5000ms（TS 的
/// `timeout: 5000`），Windows 还设置 `windows_hide` 以免弹出控制台窗口。
#[doc(hidden)]
pub fn find_executable_on_path_with(
    platform: Platform,
    executable: &str,
    exists: &dyn Fn(&str) -> bool,
    spawn_sync: &dyn Fn(&str, &[String], &SpawnSyncOptions) -> SpawnSyncResult,
) -> Option<String> {
    let options = SpawnSyncOptions {
        timeout_ms: Some(5000),
        windows_hide: platform.is_windows(),
        ..SpawnSyncOptions::default()
    };
    let args = [executable.to_string()];
    if platform.is_windows() {
        let result = spawn_sync("where", &args, &options);
        if result.status == Some(0)
            && !result.stdout.is_empty()
            && let Some(first_match) = first_line(&result.stdout)
            && exists(&first_match)
        {
            return Some(first_match);
        }
        return None;
    }

    let result = spawn_sync("which", &args, &options);
    if result.status == Some(0)
        && !result.stdout.is_empty()
        && let Some(first_match) = first_line(&result.stdout)
    {
        return Some(first_match);
    }
    None
}

/// `stdout.trim().split(/\r?\n/)[0]`，空串返回 `None`。
///
/// `trim` 使用 JS 的空白集合（`paths::js_trim`，含 BOM、全角空格等）；只按 `\n` 分行并
/// 去掉行尾的 `\r`，不按 Unicode 其它行分隔符切分。`where` 在 Windows 上输出 CRLF，
/// 因此最后这一步是必要的。
fn first_line(stdout: &str) -> Option<String> {
    let trimmed = crate::paths::js_trim(stdout);
    if trimmed.is_empty() {
        return None;
    }
    let line = trimmed.split('\n').next().unwrap_or("");
    let line = line.strip_suffix('\r').unwrap_or(line);
    if line.is_empty() {
        None
    } else {
        Some(line.to_string())
    }
}

/// `getShellConfig(customShellPath?)` 的注入版本。
///
/// 参数依次是：平台、env 读取、路径存在性检查、PATH 查找、用户指定的 shell 路径。
/// **test hook, not public API**。
///
/// 解析顺序：
///
/// 1. `custom` 非空且存在 → `getBashShellConfig(custom)`；非空但不存在 →
///    `CustomShellPathNotFound`；空串或 `None` 视为未设置，继续下面的搜索；
/// 2. Windows：`%ProgramFiles%\Git\bin\bash.exe` →
///    `%ProgramFiles(x86)%\Git\bin\bash.exe` → PATH 上的 `bash.exe` →
///    `NoBashShellFound`（`searched` 只收集实际构造出的候选；对应环境变量缺失时列表为空）；
/// 3. 非 Windows：`/bin/bash` → PATH 上的 `bash` → 字面量 `sh`（不报错）。
///
/// `env` 注入点应像 `env_lookup` 一样把空字符串当作未设置；本函数自身只对 `custom` 做空串
/// 过滤。
#[doc(hidden)]
pub fn shell_config_with(
    platform: Platform,
    env: &dyn Fn(&str) -> Option<String>,
    exists: &dyn Fn(&str) -> bool,
    which: &dyn Fn(&str) -> Option<String>,
    custom: Option<&str>,
) -> Result<ShellConfig, ShellConfigError> {
    // TS 的 `if (customShellPath)` 把空串当作未设置（falsy），继续走平台搜索。
    if let Some(custom) = custom.filter(|value| !value.is_empty()) {
        if exists(custom) {
            return Ok(bash_shell_config(custom, env("SystemRoot").as_deref()));
        }
        return Err(ShellConfigError::CustomShellPathNotFound(
            custom.to_string(),
        ));
    }

    if platform.is_windows() {
        let mut paths: Vec<String> = Vec::new();
        if let Some(program_files) = env("ProgramFiles") {
            paths.push(format!("{program_files}\\Git\\bin\\bash.exe"));
        }
        if let Some(program_files_x86) = env("ProgramFiles(x86)") {
            paths.push(format!("{program_files_x86}\\Git\\bin\\bash.exe"));
        }
        for path in &paths {
            if exists(path) {
                return Ok(bash_shell_config(path, env("SystemRoot").as_deref()));
            }
        }
        if let Some(bash_on_path) = which("bash.exe") {
            return Ok(bash_shell_config(
                &bash_on_path,
                env("SystemRoot").as_deref(),
            ));
        }
        return Err(ShellConfigError::NoBashShellFound { searched: paths });
    }

    if exists("/bin/bash") {
        return Ok(bash_shell_config("/bin/bash", env("SystemRoot").as_deref()));
    }
    if let Some(bash_on_path) = which("bash") {
        return Ok(bash_shell_config(
            &bash_on_path,
            env("SystemRoot").as_deref(),
        ));
    }
    Ok(ShellConfig {
        shell: "sh".to_string(),
        args: vec!["-c".to_string()],
        command_transport: None,
    })
}

/// `getShellConfig(customShellPath?)`。
///
/// 真实入口：平台取 [`Platform::current`]，环境变量取 `env_lookup`，存在性检查用
/// `shell_path_exists`（Windows 上已退化为 `Path::exists`，与 `access(F_OK)` 的差异见模块
/// 文档）。搜索顺序与错误语义见 `shell_config_with`。
///
/// `custom_shell_path` 为 `Some("")` 等同于 `None`，不会报 `CustomShellPathNotFound`。
pub fn get_shell_config(custom_shell_path: Option<&str>) -> Result<ShellConfig, ShellConfigError> {
    shell_config_with(
        Platform::current(),
        &env_lookup,
        &shell_path_exists,
        &find_executable_on_path,
        custom_shell_path,
    )
}

/// `getPowerShellConfig()` 的注入版本。**test hook, not public API**。
///
/// 只接受 Windows 平台；解析顺序：
///
/// 1. PATH 上的 `pwsh.exe`；
/// 2. `%ProgramFiles%\PowerShell\7\pwsh.exe`（存在才采用）；
/// 3. PATH 上的 `powershell.exe`；
/// 4. `%SystemRoot%\System32\WindowsPowerShell\v1.0\powershell.exe`（存在才采用）；
/// 5. 都没有 → `NoPowerShellExecutable`。
///
/// 第 2、4 步用 `exists` 检查固定安装位置；PATH 上的两个候选由 `which` 注入点负责。
#[doc(hidden)]
pub fn power_shell_config_with(
    platform: Platform,
    env: &dyn Fn(&str) -> Option<String>,
    exists: &dyn Fn(&str) -> bool,
    which: &dyn Fn(&str) -> Option<String>,
) -> Result<ShellConfig, ShellConfigError> {
    if !platform.is_windows() {
        return Err(ShellConfigError::PowerShellOnlyOnWindows);
    }

    let mut shell = which("pwsh.exe");
    if shell.is_none()
        && let Some(program_files) = env("ProgramFiles")
    {
        let pwsh7 = path_join_windows(&[&program_files, "PowerShell", "7", "pwsh.exe"]);
        if exists(&pwsh7) {
            shell = Some(pwsh7);
        }
    }
    if shell.is_none() {
        shell = which("powershell.exe");
    }
    if shell.is_none()
        && let Some(system_root) = env("SystemRoot")
    {
        let windows_power_shell = path_join_windows(&[
            &system_root,
            "System32",
            "WindowsPowerShell",
            "v1.0",
            "powershell.exe",
        ]);
        if exists(&windows_power_shell) {
            shell = Some(windows_power_shell);
        }
    }

    let Some(shell) = shell else {
        return Err(ShellConfigError::NoPowerShellExecutable);
    };
    Ok(ShellConfig {
        shell,
        args: POWERSHELL_ARGS.iter().map(|arg| arg.to_string()).collect(),
        command_transport: None,
    })
}

/// `getPowerShellConfig()`。
///
/// 仅在 Windows 上成功；成功时 `args` 固定为 [`POWERSHELL_ARGS`]，命令文本由调用方追加。
/// 非 Windows 返回 [`ShellConfigError::PowerShellOnlyOnWindows`]，找不到可执行文件返回
/// [`ShellConfigError::NoPowerShellExecutable`]。搜索顺序见 `power_shell_config_with`。
pub fn get_power_shell_config() -> Result<ShellConfig, ShellConfigError> {
    power_shell_config_with(
        Platform::current(),
        &env_lookup,
        &shell_path_exists,
        &find_executable_on_path,
    )
}

/// `getShellEnv()`：把 `getBinDir()` 前置到 PATH（保持原 env 顺序与 PATH 键名大小写）。
///
/// 步骤：
///
/// 1. 取 `crate::config::get_bin_dir()`（`<agentDir>/bin`）的文本形式；
/// 2. 对 `std::env::vars_os()` 做快照——标准库不承诺顺序，这里的“保持原顺序”是指不做额外
///    排序/重排，更新 PATH 时保持它在原位置，缺失时追加到向量末尾；
/// 3. 找键名大小写不敏感等于 `path` 的第一个条目，保留其原始拼写；找不到就用 `"PATH"`；
/// 4. 按平台分隔符（Windows `;`、其它 `:`）切分现有 PATH 并忽略空项；若已有条目**逐字**
///    等于 binDir 则不改动，否则把 binDir 放在最前面（binDir 为空时不添加）。
///
/// 返回的是完整环境列表，调用方（Bash 工具）会把它整体交给子进程；本函数不修改进程自身
/// 的环境。
///
/// 注意：同一环境里若存在多个大小写不同的 PATH 键，只有第一个会被更新，其余保持原值。
pub fn get_shell_env() -> Vec<(OsString, OsString)> {
    let bin_dir = crate::config::get_bin_dir();
    let mut env: Vec<(OsString, OsString)> = std::env::vars_os().collect();
    let path_key_index = env
        .iter()
        .position(|(key, _)| key.to_string_lossy().eq_ignore_ascii_case("path"));
    let key = match path_key_index {
        Some(index) => env[index].0.clone(),
        None => OsString::from("PATH"),
    };
    let delimiter = if cfg!(windows) { ';' } else { ':' };
    // 用 `OsString`/`split_paths` 处理 PATH：Unix 上 PATH 条目可以是合法但非 UTF-8
    // 的字节串，`to_string_lossy` 会把它们替换成 U+FFFD，导致那些目录里的程序找不到。
    let has_bin_dir = path_key_index
        .is_some_and(|index| std::env::split_paths(&env[index].1).any(|entry| entry == bin_dir));
    if has_bin_dir {
        return env;
    }
    let current_path = path_key_index.map(|index| env[index].1.clone());
    let mut updated = OsString::new();
    if !bin_dir.as_os_str().is_empty() {
        updated.push(bin_dir.as_os_str());
        if current_path.as_ref().is_some_and(|path| !path.is_empty()) {
            updated.push(delimiter.to_string());
        }
    }
    if let Some(current_path) = current_path {
        updated.push(current_path);
        if let Some(index) = path_key_index {
            env[index].1 = updated;
        }
    } else {
        env.push((key, updated));
    }
    env
}

/// `sanitizeBinaryOutput(str)`：删除 C0 控制字符中的
/// `\x00-\x08`、`\x0B`、`\x0C`、`\x0E-\x1F`，以及 U+FFF9..U+FFFB 三个字符。
///
/// 保留 `\t`(0x09)、`\n`(0x0A)、`\r`(0x0D)。**ESC(0x1B) 会被删除**：TS 的正则写的是
/// `\x0E-\x1F`，覆盖 0x1B（测试 `sanitize_binary_output_keeps_whitespace` 明确断言；
/// `spec/ca-tools-compaction-export.md` 中“保留 \x1B”的括注与源码冲突，以源码为准）。
/// DEL(0x7F) 与 C1 控制字符（0x80-0x9F）不在删除集合内。返回新分配的 `String`，不修改
/// 输入；非匹配字符原样保留。
pub fn sanitize_binary_output(str: &str) -> String {
    str.chars()
        .filter(|c| {
            !matches!(
                *c,
                '\u{0000}'..='\u{0008}' | '\u{000B}' | '\u{000C}' | '\u{000E}'..='\u{001F}' | '\u{FFF9}'..='\u{FFFB}'
            )
        })
        .collect()
}

/// 进程级登记表；`Option` 让 `HashSet` 延迟到第一次访问（track / untrack / 清理）时才分配。
static TRACKED_DETACHED_CHILD_PIDS: Mutex<Option<std::collections::HashSet<u32>>> =
    Mutex::new(None);

/// 在锁内访问登记表；互斥锁中毒时取回内部值继续（集合的每次增删仍然在锁保护下完成）。
fn with_tracked_pids<R>(f: impl FnOnce(&mut std::collections::HashSet<u32>) -> R) -> R {
    let mut guard = TRACKED_DETACHED_CHILD_PIDS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let set = guard.get_or_insert_with(std::collections::HashSet::new);
    f(set)
}

/// `trackDetachedChildPid(pid)`。
///
/// 把分离启动的子进程 PID 登记到进程级集合；重复登记同一个 PID 是幂等的。集合本身不区分
/// 子进程是否已经退出，调用方在回收时应配对调用 [`untrack_detached_child_pid`]。
pub fn track_detached_child_pid(pid: u32) {
    with_tracked_pids(|set| {
        set.insert(pid);
    });
}

/// `untrackDetachedChildPid(pid)`。
///
/// 从进程级集合移除 PID；PID 不在集合里时无操作（幂等）。不发送任何信号。
pub fn untrack_detached_child_pid(pid: u32) {
    with_tracked_pids(|set| {
        set.remove(&pid);
    });
}

/// `killTrackedDetachedChildren()`：逐个 `killProcessTree` 后清空（`spec/ca-rpc-print.md` §5.1.8）。
///
/// 先在锁内取快照并清空集合，再在锁外对每个 PID 调用 [`kill_process_tree`]，因此 kill 的
/// 系统调用期间不会持有互斥锁。`kill_process_tree` 本身吞掉所有错误，所以本函数没有返回值，
/// 调用方无法判断哪些进程实际被杀掉；快照之后新登记的 PID 不受本次清理影响。
pub fn kill_tracked_detached_children() {
    let pids: Vec<u32> = with_tracked_pids(|set| {
        let pids: Vec<u32> = set.iter().copied().collect();
        set.clear();
        pids
    });
    for pid in pids {
        kill_process_tree(pid);
    }
}

/// Unix 的 `process.kill(pid, signal)`。
///
/// `pid` 直接交给 `libc::kill`：正数是单进程，负数是进程组（取绝对值），0 是调用者所在的
/// 进程组。失败时返回 `last_os_error()`（如进程已消失的 `ESRCH`）；调用方
/// [`kill_process_tree_with`] 会忽略这些错误。非 Unix 平台返回
/// `io::ErrorKind::Unsupported`。
fn unix_kill(pid: i32, signal: KillSignal) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        let raw_signal = match signal {
            KillSignal::SigStop => libc::SIGSTOP,
            KillSignal::SigKill => libc::SIGKILL,
        };
        // SAFETY: `kill(2)` 只接收 pid 与信号编号，无内存安全前提。
        let result = unsafe { libc::kill(pid, raw_signal) };
        if result == 0 {
            Ok(())
        } else {
            Err(std::io::Error::last_os_error())
        }
    }
    #[cfg(not(unix))]
    {
        let _ = (pid, signal);
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "kill is not available on this platform",
        ))
    }
}

/// 分离启动 taskkill，静默失败（Windows 分支）。
///
/// 只 `spawn` 不等待：taskkill 是否真的杀掉目标、退出码是多少都不可见。stdin/stdout/stderr
/// 全部接空设备；Windows 上设置 `creation_flags(0x0000_0200 | 0x0000_0008 | 0x0800_0000)`，
/// 其中 `0x200` 是 `CREATE_NEW_PROCESS_GROUP`、`0x8` 是 `DETACHED_PROCESS`、
/// `0x0800_0000` 是 `CREATE_NO_WINDOW`。**不能**用 `0x2`：那是
/// `DEBUG_ONLY_THIS_PROCESS`，本进程从不处理调试事件，taskkill 会卡在初始断点，
/// 取消命令时进程树不会被清理。
/// 返回 `Ok` 只代表进程启动成功。
fn spawn_taskkill(program: &str, args: &[String]) -> std::io::Result<()> {
    let mut command = std::process::Command::new(program);
    command
        .args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NEW_PROCESS_GROUP | DETACHED_PROCESS | CREATE_NO_WINDOW);
    }
    command.spawn().map(|_| ())
}

/// `killProcessTree(pid)` 的注入版本。**test hook, not public API**。
///
/// 按平台分三条路径：
///
/// - Windows：从 `SystemRoot\System32\taskkill.exe`（`SystemRoot` 缺失或为空时用
///   `C:\Windows`）启动 `taskkill /F /T /PID <pid>`；**读的是真实进程环境**的
///   `SystemRoot`，不是注入参数，因此测试需要自己设置该环境变量。此路径完全不调用
///   `kill`。spawn 失败（如 ENOENT）被忽略（#6596）。
/// - macOS（`Platform::Darwin`）：先对 `-pid` 进程组发 SIGSTOP（失败忽略，减少 `a; b`
///   在取消时启动 b 的竞态，#11252），再发 SIGKILL。
/// - 其它平台（含 `Platform::Other`）：直接对 `-pid` 进程组发 SIGKILL。
///
/// 进程组 kill 失败时回退为单进程 `kill(pid, SIGKILL)`；回退也失败则静默忽略。函数没有
/// 返回值，调用方无法得知是否有进程被杀掉。`pid` 应当是正的子进程 PID：0 会被当成
/// “调用者所在进程组”，不要传入。
#[doc(hidden)]
pub fn kill_process_tree_with(
    pid: u32,
    platform: Platform,
    kill: &mut dyn FnMut(i32, KillSignal) -> std::io::Result<()>,
    spawn: &dyn Fn(&str, &[String]) -> std::io::Result<()>,
) {
    if platform.is_windows() {
        // 用 System32 里的可信可执行文件，避免依赖 PATH（#9490）。
        let system_root = env_lookup("SystemRoot").unwrap_or_else(|| "C:\\Windows".to_string());
        let taskkill = path_join_windows(&[&system_root, "System32", "taskkill.exe"]);
        let args = vec![
            "/F".to_string(),
            "/T".to_string(),
            "/PID".to_string(),
            pid.to_string(),
        ];
        // 启动失败（如 ENOENT）在 TS 里是异步 "error" 事件；这里直接吞掉（#6596）。
        let _ = spawn(&taskkill, &args);
        return;
    }

    if platform == Platform::Darwin {
        // macOS 先 best-effort SIGSTOP 进程组，减少 `a; b` 在取消时启动 b 的竞态（#11252）。
        // 停止失败不能阻止后面的 kill 或单进程回退。
        let _ = kill(-(pid as i32), KillSignal::SigStop);
    }
    if kill(-(pid as i32), KillSignal::SigKill).is_err() {
        let _ = kill(pid as i32, KillSignal::SigKill);
    }
}

/// `killProcessTree(pid)`：kill 一个进程及其所有子进程（跨平台）。
///
/// Windows 用 `taskkill /T` 递归终止；Unix 依赖调用方以进程组方式启动子进程（Bash 工具在
/// spawn 时设置 `detached: true`，子进程成为新进程组的组长），再对 `-pid` 发信号。因此
/// 如果目标不是进程组组长，进程组 kill 可能失败，只会回退到杀单个 PID。错误全部忽略；
/// `pid` 必须为正（见 `kill_process_tree_with`）。
pub fn kill_process_tree(pid: u32) {
    kill_process_tree_with(pid, Platform::current(), &mut unix_kill, &spawn_taskkill);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn env_from(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let map: HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect();
        move |key: &str| map.get(key).cloned().filter(|v| !v.is_empty())
    }

    #[test]
    fn sanitize_binary_output_keeps_whitespace() {
        assert_eq!(
            sanitize_binary_output("a\u{0}b\tc\nd\re\u{1b}f\u{0b}g\u{fff9}h"),
            // TS 的正则 `[\x00-\x08\x0B\x0C\x0E-\x1F...]` 也删除 ESC(0x1B)：
            // spec 里“保留 \x1B”的括注与源码冲突，按源码。
            "ab\tc\nd\refgh"
        );
    }

    #[test]
    fn legacy_wsl_detection() {
        assert!(is_legacy_wsl_bash_path_with(
            "d:/WINNT/System32/BASH.EXE",
            Some("D:\\WinNT\\")
        ));
        assert!(is_legacy_wsl_bash_path_with(
            "D:\\WinNT\\Sysnative\\bash.exe",
            Some("D:\\WinNT")
        ));
        assert!(is_legacy_wsl_bash_path_with(
            "E:\\Windows\\System32\\bash.exe",
            None
        ));
        assert!(!is_legacy_wsl_bash_path_with(
            "D:\\Git\\bin\\bash.exe",
            Some("D:\\WinNT")
        ));
        assert!(!is_legacy_wsl_bash_path_with(
            "E:\\Windows\\System32\\bash.exe",
            Some("D:\\WinNT")
        ));
        assert!(!is_legacy_wsl_bash_path_with(
            "C:\\Windows\\System32\\bash.exe",
            Some("WinNT")
        ));
        // node:path.win32.normalize("./D:\\...") 会保留 `.\` 前缀（CVE-2024-36139），
        // 因此它不是 Windows 目录下的 bash.exe（TS 用 `-c` 而不是 legacy WSL 的 `-s`）。
        assert!(!is_legacy_wsl_bash_path_with(
            "./D:\\Windows\\System32\\bash.exe",
            None
        ));
        assert!(!is_legacy_wsl_bash_path_with(
            "./D:\\Windows\\System32\\bash.exe",
            Some("D:\\Windows")
        ));
    }

    #[test]
    fn shell_env_prepends_the_bin_dir_and_keeps_key_casing() {
        static ENV_LOCK: Mutex<()> = Mutex::new(());
        let _guard = ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let saved_dir = std::env::var_os("KIANA_CODING_AGENT_DIR");
        let saved_path = std::env::var_os("PATH");
        // SAFETY: 本测试独占 env（模块内只有这一个测试读写环境）。
        unsafe {
            std::env::set_var("KIANA_CODING_AGENT_DIR", "/tmp/kiana-shell-env-agent");
            std::env::set_var("PATH", "/usr/bin:/bin");
        }

        let env = get_shell_env();
        let path_entry = env
            .iter()
            .find(|(key, _)| key.to_string_lossy() == "PATH")
            .expect("PATH entry");
        assert_eq!(
            path_entry.1.to_string_lossy(),
            "/tmp/kiana-shell-env-agent/bin:/usr/bin:/bin"
        );

        // 已经包含 binDir 时值不变。
        // SAFETY: 同上。
        unsafe { std::env::set_var("PATH", "/tmp/kiana-shell-env-agent/bin:/usr/bin") };
        let env = get_shell_env();
        let path_entry = env
            .iter()
            .find(|(key, _)| key.to_string_lossy() == "PATH")
            .expect("PATH entry");
        assert_eq!(
            path_entry.1.to_string_lossy(),
            "/tmp/kiana-shell-env-agent/bin:/usr/bin"
        );

        // 非 UTF-8 的 PATH 条目必须逐字保留，不能被 lossy 替换成 U+FFFD。
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStringExt as _;
            let mut non_utf8 = OsString::from_vec(b"/tmp/kiana-shell-env-agent/bin:/tmp/".to_vec());
            non_utf8.push(OsString::from_vec(vec![0xff]));
            non_utf8.push(OsString::from_vec(b"/bin".to_vec()));
            // SAFETY: 同上。
            unsafe { std::env::set_var("PATH", &non_utf8) };
            let env = get_shell_env();
            let path_entry = env
                .iter()
                .find(|(key, _)| key.to_string_lossy() == "PATH")
                .expect("PATH entry");
            assert_eq!(path_entry.1, non_utf8);
        }

        // SAFETY: 恢复。
        unsafe {
            match saved_dir {
                Some(value) => std::env::set_var("KIANA_CODING_AGENT_DIR", value),
                None => std::env::remove_var("KIANA_CODING_AGENT_DIR"),
            }
            match saved_path {
                Some(value) => std::env::set_var("PATH", value),
                None => std::env::remove_var("PATH"),
            }
        }
    }

    #[test]
    fn windows_shell_config_injected() {
        let env = env_from(&[("ProgramFiles", "C:\\Program Files")]);
        let config = shell_config_with(
            Platform::Win32,
            &env,
            &|path| path == "C:\\Program Files\\Git\\bin\\bash.exe",
            &|_| None,
            None,
        )
        .expect("bash found");
        assert_eq!(config.shell, "C:\\Program Files\\Git\\bin\\bash.exe");
        assert_eq!(config.args, vec!["-c".to_string()]);

        let error = shell_config_with(Platform::Win32, &env, &|_| false, &|_| None, None)
            .expect_err("no bash");
        assert!(
            error
                .to_string()
                .starts_with("No bash shell found. Options:\n")
        );
        assert!(
            error
                .to_string()
                .ends_with("Searched Git Bash in:\n  C:\\Program Files\\Git\\bin\\bash.exe")
        );

        let empty_env = env_from(&[]);
        let error = shell_config_with(Platform::Win32, &empty_env, &|_| false, &|_| None, None)
            .expect_err("no bash");
        assert!(error.to_string().ends_with("Searched Git Bash in:\n"));
    }
}
