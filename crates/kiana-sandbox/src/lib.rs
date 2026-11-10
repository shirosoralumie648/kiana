//! 进程级文件系统/网络沙箱。
//!
//! # 这个 crate 解决什么问题
//!
//! `bash` 工具默认直接在宿主进程的权限下跑命令：模型可以写任意路径、读 `~/.ssh`、
//! 访问网络。本 crate 把「一条命令」包进一个受控的执行环境，提供两种后端：
//!
//! - **bubblewrap（Linux 主后端）**：用 `bwrap` 构造挂载命名空间。默认只读绑定 `/`，
//!   再把策略允许的根目录以读写方式绑回去；网络用 `--unshare-net` 关闭。这是最完整的
//!   一层：写与网络都受约束；读取仍然是全盘可读（见下方边界说明）。
//! - **Landlock（Linux 回退）**：内核 5.13+ 的 unprivileged LSM。只处理写类访问，
//!   因此它限制「写哪里」，不限制「读哪里」；ABI 4+ 还能拒绝 TCP bind/connect，
//!   但 UDP、ICMP、Unix socket 不在覆盖范围内。在 `bwrap` 不可用（容器、无 user
//!   namespace）时仍然能挡住最危险的写操作；需要完整网络隔离时应使用 bubblewrap。
//!   策略禁止网络而内核 ABI < 4 时，Landlock 后端会拒绝启用（fail-closed），不会
//!   静默放行网络。Landlock 也没有减法规则，无法在「可写整个根」之后再让
//!   `.git` / `.kiana` 只读，因此**自动选择**时这种策略会 fail-closed；只有显式
//!   `KIANA_SANDBOX=landlock` 才会接受这层削弱（见 [`SandboxManager::new`]）。
//!
//! macOS 的 seatbelt 后端尚未实现；在 macOS 上需要沙箱时本 crate 会 fail-closed
//! （返回 [`SandboxError::Unavailable`]），而不是静默放行。
//!
//! # 设计边界
//!
//! - **不做审批**。本 crate 只回答「这条命令在什么沙箱里跑」，不回答「用户是否允许」。
//!   审批/权限策略属于 `kiana-permissions`（后续）。
//! - **不替换 bash 执行器**。调用方仍然负责超时、流式输出、进程树清理与错误文案；
//!   本 crate 只提供 [`SandboxManager::spawn`] 与 [`SandboxManager::wrap_command`]。
//! - **fail-closed**。策略需要沙箱但后端不可用时，`spawn` 返回错误；只有显式
//!   `KIANA_SANDBOX=none` 或 `SandboxType::None` 才会放行。
//!
//! # 环境变量
//!
//! - `KIANA_SANDBOX`：`auto`（默认）/ `bwrap` / `landlock` / `none`。
//! - `KIANA_SANDBOX_POLICY`：`read-only` / `workspace-write` / `danger-full-access`。
//!   设置后 [`SandboxManager::from_env`] 会为 `bash` 工具自动构造沙箱；未设置时保持
//!   现有行为（不启用沙箱）。
//! - `KIANA_BWRAP`：显式指定 `bwrap` 可执行文件路径。
//!
//! # 与 Codex 的关系
//!
//! 架构取自 `reference_project/codex/codex-rs/sandboxing` 与 `linux-sandbox`：
//! `SandboxManager` 负责选择后端、构造命令；bubblewrap 参数遵循其「先只读绑定 `/`，
//! 再逐层绑定可写根与只读子路径」的顺序。Kiana 侧刻意缩小了策略模型（三种策略而不是
//! Codex 的完整 permission profile），避免把 `codex-protocol` 的类型带进来。

mod error;
mod policy;
mod spawn;

#[cfg(target_os = "linux")]
mod bwrap;
mod execution_policy;
#[cfg(target_os = "linux")]
mod landlock;
mod relay;

pub use error::SandboxError;
pub use execution_policy::{
    ExecutionPolicy, ExecutionStatus, SandboxDecision, SandboxDenial, ToolInput,
    case_insensitive_filesystem, normalize_lexical, path_is_within, resolve_real_path,
};
pub use policy::{DEFAULT_PROTECTED_SUBDIR_NAMES, SandboxPolicy, default_protected_subpaths};
pub use relay::{RelayedChild, parse_status};
pub use spawn::{SpawnOptions, StdioMode};

use std::path::{Path, PathBuf};
use std::sync::Arc;

/// 实际使用的沙箱后端。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SandboxType {
    /// 不沙箱。可能来自 `DangerFullAccess`、显式 `none`，或「后端不可用」。
    None,
    /// Linux bubblewrap（挂载命名空间 + 可选网络命名空间）。
    LinuxBubblewrap,
    /// Linux Landlock（只限制写类文件系统访问）。
    LinuxLandlock,
    /// macOS seatbelt（尚未实现）。
    MacosSeatbelt,
}

impl SandboxType {
    /// 稳定字符串，用于日志、诊断与配置。
    pub fn as_str(self) -> &'static str {
        match self {
            SandboxType::None => "none",
            SandboxType::LinuxBubblewrap => "linux-bubblewrap",
            SandboxType::LinuxLandlock => "linux-landlock",
            SandboxType::MacosSeatbelt => "macos-seatbelt",
        }
    }

    /// 是否真正施加了隔离。
    pub fn is_sandboxed(self) -> bool {
        !matches!(self, SandboxType::None)
    }
}

/// 包装后的命令：`program` + `args`。
///
/// 对 bubblewrap，`program` 是 `bwrap`，`args` 已包含原始命令；对 Landlock，
/// `program`/`args` 保持原样，隔离在 `pre_exec` 阶段施加。调用方如果只想展示
/// 「最终会执行什么」，用 [`SandboxManager::wrap_command`]；如果真要执行，用
/// [`SandboxManager::spawn`]。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SandboxCommand {
    /// 最终执行的程序。
    pub program: String,
    /// 最终参数。
    pub args: Vec<String>,
    /// 实际后端。
    pub sandbox_type: SandboxType,
}

/// 沙箱管理器：策略 + 工作目录 + 已选后端。
///
/// 构造时做一次后端探测（bubblewrap 会跑一个 500ms 上限的探针）；之后 clone 只是
/// `Arc` 计数。Landlock 的 ruleset 在**每次 `spawn`** 时按当时的策略与现状新建，
/// 这样构造之后才创建/替换的可写根仍然是可写的。
#[derive(Clone)]
pub struct SandboxManager {
    inner: Arc<ManagerInner>,
}

struct ManagerInner {
    policy: SandboxPolicy,
    cwd: PathBuf,
    backend: Backend,
}

#[derive(Clone)]
enum Backend {
    /// 不施加隔离。`explicit` 为真表示调用方明确关闭；为假表示「需要沙箱但没有后端」。
    None { explicit: bool },
    /// 后端不可用或配置非法：`spawn`/`wrap_command` 一律 fail-closed。
    Unavailable { reason: String },
    #[cfg(target_os = "linux")]
    Bubblewrap { path: PathBuf },
    /// Linux Landlock。ruleset 在每次 `spawn` 时新建（见 [`SandboxManager::spawn`]）。
    #[cfg(target_os = "linux")]
    Landlock,
}

impl std::fmt::Debug for SandboxManager {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SandboxManager")
            .field("policy", &self.inner.policy)
            .field("cwd", &self.inner.cwd)
            .field("sandbox_type", &self.sandbox_type())
            .finish()
    }
}

impl SandboxManager {
    /// 按平台与环境变量选择后端。
    ///
    /// 选择顺序：`KIANA_SANDBOX` 显式指定 → bubblewrap（探针通过）→ Landlock →
    /// 无后端（fail-closed）。`DangerFullAccess` 直接返回 `None` 后端。
    ///
    /// Landlock 只有加法规则，无法在「可写整个根」之后再让 `.git` / `.kiana` 保持
    /// 只读。因此**自动选择**时，只要策略里存在需要保护的子路径，就不会退回到
    /// Landlock，而是 fail-closed；`KIANA_SANDBOX=landlock` 属于显式选择，视为
    /// 调用方已知情接受这层削弱。
    ///
    /// 第一次构造会执行 bubblewrap 探针（最多阻塞 500ms）。成功结果在进程内永久缓存，
    /// 失败结果 30 秒后重试；在异步上下文里应尽量在启动阶段构造 manager，避免在
    /// worker 上做这次探测。
    pub fn new(policy: SandboxPolicy, cwd: impl Into<PathBuf>) -> Self {
        let cwd = cwd.into();
        if !policy.requires_sandbox() {
            return Self::disabled(policy, cwd, true);
        }

        let preference = std::env::var("KIANA_SANDBOX")
            .ok()
            .map(|value| value.trim().to_ascii_lowercase());

        match preference.as_deref() {
            Some("none" | "off" | "disabled" | "false") => Self::disabled(policy, cwd, true),
            Some("bwrap" | "bubblewrap") => {
                #[cfg(target_os = "linux")]
                {
                    if let Some(path) = bwrap::working_bwrap() {
                        return Self::bubblewrap(policy, cwd, path);
                    }
                }
                Self::disabled(policy, cwd, false)
            }
            Some("landlock") => {
                #[cfg(target_os = "linux")]
                {
                    // 构造一次 ruleset 只为确认内核支持；真正的 ruleset 在每次 spawn 时新建。
                    if let Ok(Some(_)) = landlock::LandlockRuleset::new(&policy) {
                        return Self::landlock(policy, cwd);
                    }
                }
                Self::disabled(policy, cwd, false)
            }
            _ => {
                #[cfg(target_os = "linux")]
                {
                    if let Some(path) = bwrap::working_bwrap() {
                        return Self::bubblewrap(policy, cwd, path);
                    }
                    // 没有 bubblewrap 时，只要策略里列了需要保护的子路径（`workspace_write`
                    // 默认就有 `.git` / `.kiana`，与它们当前是否已创建无关），Landlock
                    // 就无法兑现。这里按策略判定而不是按「此刻是否存在」判定，避免会话
                    // 中途 `git init` 之后突然永久 fail-closed。
                    if !policy.read_only_subpaths().is_empty() {
                        return Self::fail_closed(
                            policy,
                            cwd,
                            "the Landlock fallback cannot keep protected subpaths (.git/.kiana) \
                             read-only inside writable roots; install bubblewrap, or set \
                             KIANA_SANDBOX=landlock to accept the weaker isolation"
                                .to_string(),
                        );
                    }
                    if let Ok(Some(_)) = landlock::LandlockRuleset::new(&policy) {
                        return Self::landlock(policy, cwd);
                    }
                }
                Self::disabled(policy, cwd, false)
            }
        }
    }

    /// 从 `KIANA_SANDBOX_POLICY` 构造；未设置或为 `danger-full-access` 时返回 `None`。
    ///
    /// 这是给 `bash` 工具的零配置入口：设置
    /// `KIANA_SANDBOX_POLICY=workspace-write` 即可让默认工具表启用沙箱。
    ///
    /// 无法识别的策略名不会被当成「不沙箱」（那会把拼写错误变成 fail-open），而是
    /// 返回一个 `spawn` 必然失败的 manager，让错误在第一条命令上显式暴露。
    /// `KIANA_SANDBOX=none` 在这里仍然有效：它是显式的、整条策略链的关闭开关。
    pub fn from_env(cwd: impl AsRef<Path>) -> Option<Self> {
        let cwd = cwd.as_ref().to_path_buf();
        // 显式关闭优先级最高：错误信息里给出的 `KIANA_SANDBOX=none` 逃生舱对
        // 「策略值非法 / 非 UTF-8」同样有效。
        if sandbox_explicitly_disabled() {
            return None;
        }
        let raw = std::env::var_os("KIANA_SANDBOX_POLICY")?;
        // 非 UTF-8 的值不能被 `.ok()?` 当成「未设置」——那会静默关闭沙箱。
        let policy_name = match raw.into_string() {
            Ok(name) => name,
            Err(_) => {
                return Some(Self::fail_closed(
                    SandboxPolicy::read_only(),
                    cwd,
                    "KIANA_SANDBOX_POLICY is not valid UTF-8; expected read-only, \
                     workspace-write, or danger-full-access (set KIANA_SANDBOX=none to run \
                     unsandboxed)"
                        .to_string(),
                ));
            }
        };
        // 空字符串等价于「未设置」：CI 里常见的 `export KIANA_SANDBOX_POLICY="${X:-}"`
        // 不应让所有 bash 调用 fail-closed。
        if policy_name.trim().is_empty() {
            return None;
        }
        let policy = match policy_name.trim().to_ascii_lowercase().as_str() {
            "read-only" | "readonly" => SandboxPolicy::read_only(),
            "workspace-write" | "workspace" => SandboxPolicy::workspace_write(&cwd),
            "danger-full-access" | "danger" | "none" | "off" | "disabled" => return None,
            _ => {
                return Some(Self::fail_closed(
                    SandboxPolicy::read_only(),
                    cwd,
                    format!(
                        "unrecognized KIANA_SANDBOX_POLICY value {:?}; expected read-only, \
                         workspace-write, or danger-full-access (set KIANA_SANDBOX=none to run \
                         unsandboxed)",
                        policy_name.trim()
                    ),
                ));
            }
        };
        Some(Self::new(policy, cwd))
    }

    /// 显式指定后端，主要用于测试与上层配置。
    ///
    /// `SandboxType::None` 表示明确关闭沙箱（即使策略不是 `DangerFullAccess`）。
    pub fn with_type(
        sandbox_type: SandboxType,
        policy: SandboxPolicy,
        cwd: impl Into<PathBuf>,
    ) -> Result<Self, SandboxError> {
        let cwd = cwd.into();
        if matches!(sandbox_type, SandboxType::None) || !policy.requires_sandbox() {
            return Ok(Self::disabled(policy, cwd, true));
        }

        match sandbox_type {
            SandboxType::None => Ok(Self::disabled(policy, cwd, true)),
            SandboxType::LinuxBubblewrap => {
                #[cfg(target_os = "linux")]
                {
                    let path = bwrap::working_bwrap().ok_or_else(|| SandboxError::Unavailable {
                        policy: policy.label(),
                        reason: "bubblewrap was not found or its user-namespace probe failed"
                            .to_string(),
                    })?;
                    Ok(Self::bubblewrap(policy, cwd, path))
                }
                #[cfg(not(target_os = "linux"))]
                {
                    let _ = policy;
                    Err(SandboxError::Unsupported("linux-bubblewrap"))
                }
            }
            SandboxType::LinuxLandlock => {
                #[cfg(target_os = "linux")]
                {
                    landlock::LandlockRuleset::new(&policy)?.ok_or_else(|| {
                        SandboxError::Unavailable {
                            policy: policy.label(),
                            reason: "the running kernel does not support Landlock".to_string(),
                        }
                    })?;
                    Ok(Self::landlock(policy, cwd))
                }
                #[cfg(not(target_os = "linux"))]
                {
                    let _ = policy;
                    Err(SandboxError::Unsupported("linux-landlock"))
                }
            }
            SandboxType::MacosSeatbelt => Err(SandboxError::Unsupported("macos-seatbelt")),
        }
    }

    fn disabled(policy: SandboxPolicy, cwd: PathBuf, explicit: bool) -> Self {
        Self {
            inner: Arc::new(ManagerInner {
                policy,
                cwd,
                backend: Backend::None { explicit },
            }),
        }
    }

    /// 构造一个必然 fail-closed 的 manager：`spawn`/`wrap_command` 都会带上 `reason`
    /// 返回 [`SandboxError::Unavailable`]。
    fn fail_closed(policy: SandboxPolicy, cwd: PathBuf, reason: String) -> Self {
        Self {
            inner: Arc::new(ManagerInner {
                policy,
                cwd,
                backend: Backend::Unavailable { reason },
            }),
        }
    }

    #[cfg(target_os = "linux")]
    fn bubblewrap(policy: SandboxPolicy, cwd: PathBuf, path: PathBuf) -> Self {
        Self {
            inner: Arc::new(ManagerInner {
                policy,
                cwd,
                backend: Backend::Bubblewrap { path },
            }),
        }
    }

    #[cfg(target_os = "linux")]
    fn landlock(policy: SandboxPolicy, cwd: PathBuf) -> Self {
        Self {
            inner: Arc::new(ManagerInner {
                policy,
                cwd,
                backend: Backend::Landlock,
            }),
        }
    }

    /// 当前策略。
    pub fn policy(&self) -> &SandboxPolicy {
        &self.inner.policy
    }

    /// 把本 manager 的策略与工作目录投影成统一执行策略。
    ///
    /// 所有执行路径（bash / PowerShell / `execute_bash` / MCP 子进程）都应先调用
    /// [`ExecutionPolicy::resolve`] 再 [`SandboxManager::spawn`] /
    /// [`SandboxManager::spawn_relayed`]，这样文件写入的门禁与 shell 使用同一份策略，
    /// 不会出现「shell 写不进、write/edit 写得进」的绕过。
    pub fn execution_policy(&self) -> ExecutionPolicy {
        ExecutionPolicy::new(self.inner.policy.clone(), self.inner.cwd.clone())
    }

    /// 策略绑定的工作目录（bubblewrap 的 `--chdir`）。
    pub fn cwd(&self) -> &Path {
        &self.inner.cwd
    }

    /// 实际后端。
    pub fn sandbox_type(&self) -> SandboxType {
        match &self.inner.backend {
            Backend::None { .. } | Backend::Unavailable { .. } => SandboxType::None,
            #[cfg(target_os = "linux")]
            Backend::Bubblewrap { .. } => SandboxType::LinuxBubblewrap,
            #[cfg(target_os = "linux")]
            Backend::Landlock => SandboxType::LinuxLandlock,
        }
    }

    /// 策略要求沙箱时，后端是否真的可用。
    ///
    /// 显式关闭（`KIANA_SANDBOX=none` / `SandboxType::None`）也算可用：调用方已经
    /// 明确选择不隔离，`spawn` 不会因此失败。
    pub fn is_available(&self) -> bool {
        match &self.inner.backend {
            Backend::None { explicit } => *explicit || !self.inner.policy.requires_sandbox(),
            Backend::Unavailable { .. } => false,
            #[cfg(target_os = "linux")]
            Backend::Bubblewrap { .. } | Backend::Landlock => true,
        }
    }

    /// 后端是否支持「可写根内的只读子路径」这种减法规则。
    ///
    /// bubblewrap 通过后置的 `--ro-bind` 支持；Landlock 的 path-beneath 规则只有加法，
    /// 无法在允许写整个根之后再把 `.git` 挖成只读。因此 Landlock 回退只保证
    /// `writable_roots` 之外的写被拒绝，不保证根内的只读子路径。
    pub fn enforces_read_only_subpaths(&self) -> bool {
        #[cfg(target_os = "linux")]
        {
            matches!(&self.inner.backend, Backend::Bubblewrap { .. })
        }
        #[cfg(not(target_os = "linux"))]
        {
            false
        }
    }

    /// 把 `program args` 包装成最终要执行的命令。
    ///
    /// - bubblewrap：返回 `bwrap` 及其完整参数；
    /// - Landlock：返回原命令（隔离在 [`SandboxManager::spawn`] 的 `pre_exec` 中）；
    /// - 无后端且策略要求沙箱：返回 [`SandboxError::Unavailable`]。
    ///
    /// `--chdir` 使用 [`SandboxManager::cwd`]；需要让命令在别的工作目录里跑时用
    /// [`SandboxManager::spawn`]（它读取 [`SpawnOptions::cwd`]）。
    pub fn wrap_command(
        &self,
        program: &str,
        args: &[String],
    ) -> Result<SandboxCommand, SandboxError> {
        self.wrap_command_in(program, args, &self.inner.cwd)
    }

    fn wrap_command_in(
        &self,
        program: &str,
        args: &[String],
        cwd: &Path,
    ) -> Result<SandboxCommand, SandboxError> {
        if program.is_empty() {
            return Err(SandboxError::EmptyCommand);
        }
        // 只有 bubblewrap 后端会用到逐个命令的 cwd（`--chdir`）。
        #[cfg(not(target_os = "linux"))]
        let _ = cwd;
        match &self.inner.backend {
            Backend::Unavailable { reason } => Err(self.unavailable_reason(reason)),
            #[cfg(target_os = "linux")]
            Backend::Bubblewrap { path } => {
                let args = bwrap::build_args(&self.inner.policy, cwd, program, args)?;
                Ok(SandboxCommand {
                    program: path.to_string_lossy().into_owned(),
                    args,
                    sandbox_type: SandboxType::LinuxBubblewrap,
                })
            }
            #[cfg(target_os = "linux")]
            Backend::Landlock => Ok(SandboxCommand {
                program: program.to_string(),
                args: args.to_vec(),
                sandbox_type: SandboxType::LinuxLandlock,
            }),
            Backend::None { explicit } => {
                if self.inner.policy.requires_sandbox() && !explicit {
                    Err(self.unavailable())
                } else {
                    Ok(SandboxCommand {
                        program: program.to_string(),
                        args: args.to_vec(),
                        sandbox_type: SandboxType::None,
                    })
                }
            }
        }
    }

    /// 在沙箱中启动进程。
    ///
    /// 调用方仍负责等待、超时、`kill` 与输出读取；本函数只返回 `tokio::process::Child`。
    ///
    /// [`SpawnOptions::cwd`] 同时决定 bubblewrap 的 `--chdir` 与子进程的工作目录；
    /// 它是**每条命令**的目录，可以与构造 manager 时的 [`SandboxManager::cwd`] 不同。
    pub fn spawn(
        &self,
        program: &str,
        args: &[String],
        options: &SpawnOptions,
    ) -> Result<tokio::process::Child, SandboxError> {
        if program.is_empty() {
            return Err(SandboxError::EmptyCommand);
        }
        let cwd = options
            .cwd
            .as_deref()
            .filter(|cwd| !cwd.as_os_str().is_empty())
            .unwrap_or(self.inner.cwd.as_path());
        // 只有 Linux 的 bubblewrap 分支需要它；其它平台直接交给 spawn_command。
        #[cfg(not(target_os = "linux"))]
        let _ = cwd;
        match &self.inner.backend {
            Backend::Unavailable { reason } => Err(self.unavailable_reason(reason)),
            #[cfg(target_os = "linux")]
            Backend::Bubblewrap { .. } => {
                let command = self.wrap_command_in(program, args, cwd)?;
                spawn::spawn_command(&command.program, &command.args, options, None)
            }
            #[cfg(target_os = "linux")]
            Backend::Landlock => {
                // 每次 spawn 都按当下的策略重建 ruleset：path-beneath 规则绑定的是
                // 构造时的 inode，可写根在 manager 构造之后被创建或替换时，缓存的
                // ruleset 会让新目录永久不可写。
                let ruleset =
                    landlock::LandlockRuleset::new(&self.inner.policy)?.ok_or_else(|| {
                        self.unavailable_reason(
                            "the running kernel does not support Landlock (or cannot enforce the \
                             policy's network restriction)",
                        )
                    })?;
                spawn::spawn_command(program, args, options, Some(ruleset.fd()))
            }
            Backend::None { explicit } => {
                if self.inner.policy.requires_sandbox() && !explicit {
                    Err(self.unavailable())
                } else {
                    spawn::spawn_command(program, args, options, None)
                }
            }
        }
    }

    /// 在沙箱中启动进程，并额外套一个 relay 监督进程来给出确定状态。
    ///
    /// 与 [`SandboxManager::spawn`] 的区别：
    /// - 实际返回的 `child` 是 relay（`/bin/sh` 脚本），它会启动真正的沙箱命令；
    /// - relay 在命令启动前写 `started`、结束后写 `exit <code>`，调用方在 `wait` 之后用
    ///   [`RelayedChild::outcome`] 获取 [`ExecutionStatus`]；
    /// - relay 被外部杀死（超时/取消的进程树清理）时状态是 `Unconfirmed`，而不是一个
    ///   看似确定的退出码。
    ///
    /// `options` 的 stdio/cwd/env 同时作用于 relay 与命令（命令继承 relay 的 cwd/env）。
    /// 后端不可用时在创建任何临时文件之前返回错误；调用方应把它映射为
    /// [`ExecutionStatus::BackendUnavailable`]。
    pub fn spawn_relayed(
        &self,
        program: &str,
        args: &[String],
        options: &SpawnOptions,
    ) -> Result<RelayedChild, SandboxError> {
        if program.is_empty() {
            return Err(SandboxError::EmptyCommand);
        }
        let cwd = options
            .cwd
            .as_deref()
            .filter(|cwd| !cwd.as_os_str().is_empty())
            .unwrap_or(self.inner.cwd.as_path());
        #[cfg(not(target_os = "linux"))]
        let _ = cwd;

        // 先确认后端可用：这一步不能产生任何临时文件/进程副作用。
        match &self.inner.backend {
            Backend::Unavailable { reason } => return Err(self.unavailable_reason(reason)),
            Backend::None { explicit } if self.inner.policy.requires_sandbox() && !explicit => {
                return Err(self.unavailable());
            }
            _ => {}
        }

        #[cfg(unix)]
        {
            #[cfg(target_os = "linux")]
            match &self.inner.backend {
                Backend::Bubblewrap { .. } => {
                    let command = self.wrap_command_in(program, args, cwd)?;
                    self.spawn_relay_process(command.program, command.args, options, None, None)
                }
                Backend::Landlock => {
                    let status_dir = relay::create_status_dir()?;
                    let extra = [status_dir.clone()];
                    let ruleset = match landlock::LandlockRuleset::new_with_extra_writable_roots(
                        &self.inner.policy,
                        &extra,
                    ) {
                        Ok(Some(ruleset)) => ruleset,
                        Ok(None) => {
                            let _ = std::fs::remove_dir_all(&status_dir);
                            return Err(self.unavailable_reason(
                                "the running kernel does not support Landlock (or cannot \
                                 enforce the policy's network restriction)",
                            ));
                        }
                        Err(error) => {
                            let _ = std::fs::remove_dir_all(&status_dir);
                            return Err(error);
                        }
                    };
                    // ruleset 必须活到 spawn 返回：pre_exec 在 fork 后的子进程里执行，
                    // 父进程只要保证 fd 没有被关闭即可。
                    self.spawn_relay_process(
                        program.to_string(),
                        args.to_vec(),
                        options,
                        Some(ruleset.fd()),
                        Some(status_dir),
                    )
                }
                Backend::None { .. } => self.spawn_relay_process(
                    program.to_string(),
                    args.to_vec(),
                    options,
                    None,
                    None,
                ),
                Backend::Unavailable { .. } => unreachable!("checked before spawning"),
            }
            #[cfg(not(target_os = "linux"))]
            {
                self.spawn_relay_process(program.to_string(), args.to_vec(), options, None, None)
            }
        }
        #[cfg(not(unix))]
        {
            let _ = (program, args, options);
            Err(SandboxError::Unsupported("sandbox relay"))
        }
    }

    #[cfg(unix)]
    fn spawn_relay_process(
        &self,
        program: String,
        args: Vec<String>,
        options: &SpawnOptions,
        landlock_fd: Option<i32>,
        existing_status_dir: Option<std::path::PathBuf>,
    ) -> Result<RelayedChild, SandboxError> {
        let status_dir = match existing_status_dir {
            Some(dir) => dir,
            None => relay::create_status_dir()?,
        };
        let status_file = status_dir.join("status");
        let (relay_program, relay_args) = relay::relay_command(&program, &args, &status_file);
        match spawn::spawn_command(&relay_program, &relay_args, options, landlock_fd) {
            Ok(child) => Ok(RelayedChild::new(child, status_file, status_dir)),
            Err(error) => {
                let _ = std::fs::remove_dir_all(&status_dir);
                Err(error)
            }
        }
    }

    fn unavailable_reason(&self, reason: &str) -> SandboxError {
        SandboxError::Unavailable {
            policy: self.inner.policy.label(),
            reason: reason.to_string(),
        }
    }
    fn unavailable(&self) -> SandboxError {
        SandboxError::Unavailable {
            policy: self.inner.policy.label(),
            reason: "no working bubblewrap or Landlock backend is available; \
                     install bubblewrap or set KIANA_SANDBOX=none to run unsandboxed"
                .to_string(),
        }
    }
}

/// `KIANA_SANDBOX` 是否被显式设成「关闭沙箱」。
///
/// `from_env` 的未知策略分支用它兑现错误信息里的 `KIANA_SANDBOX=none` 逃生舱。
fn sandbox_explicitly_disabled() -> bool {
    std::env::var("KIANA_SANDBOX")
        .map(|value| {
            matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "none" | "off" | "disabled" | "false"
            )
        })
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn danger_full_access_needs_no_backend() {
        let manager = SandboxManager::new(SandboxPolicy::danger_full_access(), "/tmp");
        assert_eq!(manager.sandbox_type(), SandboxType::None);
        assert!(manager.is_available());
        let command = manager
            .wrap_command("/bin/echo", &["hi".to_string()])
            .expect("passthrough");
        assert_eq!(command.program, "/bin/echo");
        assert_eq!(command.args, vec!["hi".to_string()]);
    }

    #[test]
    fn explicit_none_disables_a_sandboxed_policy() {
        let manager =
            SandboxManager::with_type(SandboxType::None, SandboxPolicy::read_only(), "/tmp")
                .expect("explicit disable");
        assert_eq!(manager.sandbox_type(), SandboxType::None);
        assert!(manager.is_available());
        assert!(
            manager
                .wrap_command("/bin/echo", &[])
                .expect("explicit disable")
                .args
                .is_empty()
        );
    }

    #[test]
    fn policy_accessors_round_trip() {
        let policy = SandboxPolicy::workspace_write("/workspace")
            .with_network_access(true)
            .with_read_only_subpaths(vec![PathBuf::from("/workspace/.git")]);
        assert!(policy.requires_sandbox());
        assert!(policy.network_access());
        assert_eq!(policy.writable_roots(), &[PathBuf::from("/workspace")]);
        assert_eq!(
            policy.read_only_subpaths(),
            &[PathBuf::from("/workspace/.git")]
        );
        assert_eq!(policy.label(), "workspace-write");
    }

    #[test]
    fn fail_closed_manager_never_spawns_unsandboxed() {
        let manager = SandboxManager::fail_closed(
            SandboxPolicy::workspace_write("/workspace"),
            PathBuf::from("/workspace"),
            "invalid policy".to_string(),
        );
        assert_eq!(manager.sandbox_type(), SandboxType::None);
        assert!(!manager.is_available());
        let error = manager
            .wrap_command("/bin/echo", &[])
            .expect_err("fail-closed must reject");
        assert!(matches!(error, SandboxError::Unavailable { .. }));
        assert!(error.to_string().contains("invalid policy"));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn only_bubblewrap_enforces_subtractive_read_only_subpaths() {
        let policy = SandboxPolicy::workspace_write("/tmp");
        if let Ok(manager) =
            SandboxManager::with_type(SandboxType::LinuxBubblewrap, policy.clone(), "/tmp")
        {
            assert!(manager.enforces_read_only_subpaths());
        }
        if let Ok(manager) = SandboxManager::with_type(SandboxType::LinuxLandlock, policy, "/tmp") {
            assert!(!manager.enforces_read_only_subpaths());
        }
    }
}
