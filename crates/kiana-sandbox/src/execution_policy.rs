//! 统一执行策略：所有执行路径（bash / PowerShell / `execute_bash` / MCP 子进程 /
//! 文件写入）都通过 [`ExecutionPolicy::resolve`] 得到同一个结论。
//!
//! # 为什么需要它
//!
//! 只靠 [`SandboxPolicy`] 还不够：`bash` 会在 `SandboxManager` 里被隔离，但 `write` /
//! `edit` 是进程内文件操作，没有自己的 OS 沙箱；如果两者各自解释「工作区」与
//! `.git`/`.kiana` 保护，就会出现「shell 写不进去、文件工具写得进去」的绕过。
//! 本模块把「这条工具调用允许在什么世界里执行」收敛成一个纯决策函数：
//!
//! - shell / MCP 子进程：返回 [`SandboxDecision::Allow`] 携带的原策略，由
//!   [`crate::SandboxManager`] 真正施加隔离；
//! - 文件写入：先按同一策略做路径门禁（realpath、可写根、受保护子路径、大小写），
//!   拒绝时返回带稳定错误码的 [`SandboxDenial`]。
//!
//! 决策函数**不做后端探测**：bubblewrap/Landlock 是否可用是执行期问题，由
//! [`crate::SandboxManager::spawn`] / [`crate::SandboxManager::spawn_relayed`]
//! 以 fail-closed 的方式暴露为 [`ExecutionStatus::BackendUnavailable`]。
//!
//! # 参考实现
//!
//! - Codex `codex-rs/sandboxing`：三档策略与「可写根 + 根内只读子路径」的减法模型。
//! - qwen-code `priorReadEnforcement.ts`：路径不可验证时 fail-closed，而不是猜。
//! - Claude Code `FileEditTool` / `FileWriteTool`：读写路径用 realpath 归一后再比较。

use std::io;
use std::path::{Component, Path, PathBuf};

use crate::error::SandboxError;
use crate::policy::{SandboxPolicy, default_protected_subpaths};

/// 一次工具调用的输入形状。
///
/// 只有 `ExecutionPolicy` 关心的字段：命令本身不参与第 2 周的决策（shell 语法分析属于
/// 第 3 周），文件写入则必须给出所有目标路径。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolInput<'a> {
    /// `bash` / `powershell` / `execute_bash`：在给定策略下跑一条命令。
    Shell {
        /// 原始命令文本。第 2 周不解析它，只保留给日志/未来分析。
        command: &'a str,
    },
    /// MCP stdio 子进程：与 shell 共用策略。
    McpProcess {
        /// 可执行文件。
        program: &'a str,
        /// 参数。
        args: &'a [String],
    },
    /// `write` / `edit`：所有会被写入的绝对路径。
    FileWrite {
        /// 写入目标。
        paths: &'a [PathBuf],
    },
    /// `read`：读取不受 workspace-write 限制（与 bwrap/Landlock 的读语义一致）。
    FileRead {
        /// 读取目标。
        paths: &'a [PathBuf],
    },
}

/// 统一决策结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SandboxDecision {
    /// 允许执行；策略需要沙箱时由执行器施加，`DangerFullAccess` 表示明确不隔离。
    Allow(SandboxPolicy),
    /// 拒绝执行。拒绝发生在任何副作用之前。
    Deny(SandboxDenial),
}

impl SandboxDecision {
    /// 是否允许。
    pub fn is_allowed(&self) -> bool {
        matches!(self, Self::Allow(_))
    }

    /// 被拒绝时的详情。
    pub fn denial(&self) -> Option<&SandboxDenial> {
        match self {
            Self::Allow(_) => None,
            Self::Deny(denial) => Some(denial),
        }
    }

    /// 允许时携带的策略。
    pub fn policy(&self) -> Option<&SandboxPolicy> {
        match self {
            Self::Allow(policy) => Some(policy),
            Self::Deny(_) => None,
        }
    }
}

/// 稳定错误码 + 人类可读原因。
///
/// 错误码是给模型/程序化调用者的契约（例如 `OUTSIDE_WORKSPACE`），不要为了文案改动它。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SandboxDenial {
    /// 稳定错误码。
    pub code: &'static str,
    /// 人类可读原因（可包含路径）。
    pub reason: String,
}

impl SandboxDenial {
    fn new(code: &'static str, reason: impl Into<String>) -> Self {
        Self {
            code,
            reason: reason.into(),
        }
    }

    /// `CODE: reason` 形式的单行文本。
    pub fn message(&self) -> String {
        format!("{}: {}", self.code, self.reason)
    }
}

/// 统一的执行结果状态。
///
/// `ConfirmedExit` 才允许调用方相信退出码；`Unconfirmed` 表示「可能已经执行，但无法确认
/// 是否完成」，调用方（工具提示）必须要求模型不要自动重试。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExecutionStatus {
    /// relay/直接等待拿到了确定的退出状态。
    ConfirmedExit {
        /// 退出码（信号终止时为 128 + signal，与 shell 约定一致）。
        code: i32,
        /// Unix 信号编号。
        signal: Option<i32>,
    },
    /// 命令已经启动，但 relay 被外部杀死，退出状态不可知。
    Unconfirmed {
        /// 原因（诊断用）。
        reason: String,
    },
    /// 沙箱后端不可用或启动失败；命令**没有**执行。
    BackendUnavailable {
        /// 具体原因。
        reason: String,
    },
    /// 在命令确认启动之前就被中断；命令没有执行。
    Interrupted {
        /// 中断原因。
        reason: String,
    },
}

impl ExecutionStatus {
    /// 稳定标签。
    pub fn label(&self) -> &'static str {
        match self {
            Self::ConfirmedExit { .. } => "confirmed_exit",
            Self::Unconfirmed { .. } => "unconfirmed",
            Self::BackendUnavailable { .. } => "backend_unavailable",
            Self::Interrupted { .. } => "interrupted",
        }
    }

    /// 面向工具错误的 CamelCase 形态（与计划里的状态名一致）。
    pub fn tool_label(&self) -> &'static str {
        match self {
            Self::ConfirmedExit { .. } => "ConfirmedExit",
            Self::Unconfirmed { .. } => "Unconfirmed",
            Self::BackendUnavailable { .. } => "BackendUnavailable",
            Self::Interrupted { .. } => "Interrupted",
        }
    }

    /// `Label: user_message` 形态，供 shell 执行器直接放进工具错误。
    pub fn tool_message(&self) -> String {
        format!("{}: {}", self.tool_label(), self.user_message())
    }

    /// 确定的退出码；只有 `ConfirmedExit` 有。
    pub fn exit_code(&self) -> Option<i32> {
        match self {
            Self::ConfirmedExit { code, .. } => Some(*code),
            _ => None,
        }
    }

    /// 信号编号；只有 `ConfirmedExit` 且被信号终止时才有。
    pub fn signal(&self) -> Option<i32> {
        match self {
            Self::ConfirmedExit { signal, .. } => *signal,
            _ => None,
        }
    }

    /// 是否拿到确定结论。
    pub fn is_confirmed(&self) -> bool {
        matches!(self, Self::ConfirmedExit { .. })
    }

    /// 命令是否可能已经执行。
    ///
    /// 只有 `Unconfirmed` 表示「启动了但状态未知」；`BackendUnavailable` 与
    /// `Interrupted` 都发生在启动之前，命令确定没有执行。
    pub fn may_have_executed(&self) -> bool {
        matches!(self, Self::Unconfirmed { .. })
    }

    /// 调用方是否可以在**没有人工确认**的情况下自动重试。
    ///
    /// 只有 `BackendUnavailable`（后端没起来，命令确定没有执行）允许自动重试；
    /// `Unconfirmed` 永远不允许自动重试——重试可能把已经产生副作用的命令跑第二遍。
    /// `ConfirmedExit` 与新的一次调用语义不同，也不在「自动重试」范围内。
    pub fn allows_automatic_retry(&self) -> bool {
        matches!(self, Self::BackendUnavailable { .. })
    }

    /// 面向模型/用户的完整提示。
    ///
    /// `Unconfirmed` 的文案是安全契约：必须明确「可能已经执行」与「不要自动重试」。
    pub fn user_message(&self) -> String {
        match self {
            Self::ConfirmedExit { code, signal } => match signal {
                Some(signal) => format!("Command exited with code {code} (signal {signal})."),
                None => format!("Command exited with code {code}."),
            },
            Self::Unconfirmed { reason } => format!(
                "The command may have already executed, but its exit status could not be \
                 confirmed ({reason}). Do not retry automatically; inspect the workspace first."
            ),
            Self::BackendUnavailable { reason } => format!(
                "The command was not executed: the sandbox backend is unavailable ({reason}). \
                 Do not fall back to running it unsandboxed."
            ),
            Self::Interrupted { reason } => {
                format!("The command was interrupted before it started ({reason}); it did not run.")
            }
        }
    }

    /// 把沙箱启动错误映射成执行状态。
    ///
    /// 所有 [`SandboxError`] 都发生在命令启动之前（参数构造、后端不可用、relay 准备失败），
    /// 因此一律是 `BackendUnavailable`：命令没有执行，可以安全地报告给模型。
    pub fn from_sandbox_error(error: &SandboxError) -> Self {
        Self::BackendUnavailable {
            reason: error.to_string(),
        }
    }
}

/// 统一执行策略：策略 + 绑定的工作目录。
#[derive(Debug, Clone)]
pub struct ExecutionPolicy {
    policy: SandboxPolicy,
    cwd: PathBuf,
    /// 配置非法时的原因；非空时所有 `resolve` 都拒绝（fail-closed）。
    invalid_reason: Option<String>,
}

impl ExecutionPolicy {
    /// 用显式策略构造。`cwd` 是 workspace-write 的默认根。
    pub fn new(policy: SandboxPolicy, cwd: impl Into<PathBuf>) -> Self {
        Self {
            policy,
            cwd: cwd.into(),
            invalid_reason: None,
        }
    }

    /// 从环境变量构造（纯解析，不探测后端）。
    ///
    /// - `KIANA_SANDBOX=none`：整条链明确关闭，返回 `DangerFullAccess`；
    /// - `KIANA_SANDBOX_POLICY` 未设置/空：保持历史行为（不启用沙箱）；
    /// - `read-only` / `workspace-write` / `danger-full-access`：对应策略；
    /// - 无法识别的值：拒绝所有执行（不能把拼写错误变成 fail-open）。
    pub fn from_env(cwd: impl Into<PathBuf>) -> Self {
        let cwd = cwd.into();
        if sandbox_explicitly_disabled() {
            return Self::new(SandboxPolicy::danger_full_access(), cwd);
        }
        let Some(raw) = std::env::var_os("KIANA_SANDBOX_POLICY") else {
            return Self::new(SandboxPolicy::danger_full_access(), cwd);
        };
        let Ok(name) = raw.into_string() else {
            return Self {
                policy: SandboxPolicy::danger_full_access(),
                cwd,
                invalid_reason: Some(
                    "KIANA_SANDBOX_POLICY is not valid UTF-8; expected read-only, \
                     workspace-write, or danger-full-access"
                        .to_string(),
                ),
            };
        };
        if name.trim().is_empty() {
            return Self::new(SandboxPolicy::danger_full_access(), cwd);
        }
        match name.trim().to_ascii_lowercase().as_str() {
            "read-only" | "readonly" => Self::new(SandboxPolicy::read_only(), cwd),
            "workspace-write" | "workspace" => Self::new(SandboxPolicy::workspace_write(&cwd), cwd),
            "danger-full-access" | "danger" | "none" | "off" | "disabled" => {
                Self::new(SandboxPolicy::danger_full_access(), cwd)
            }
            other => Self {
                policy: SandboxPolicy::danger_full_access(),
                cwd,
                invalid_reason: Some(format!(
                    "unrecognized KIANA_SANDBOX_POLICY value {other:?}; expected read-only, \
                     workspace-write, or danger-full-access"
                )),
            },
        }
    }

    /// 当前策略。
    pub fn policy(&self) -> &SandboxPolicy {
        &self.policy
    }

    /// 策略绑定的工作目录。
    pub fn cwd(&self) -> &Path {
        &self.cwd
    }

    /// 追加可写根（`--add-dir` / 多根 workspace）。非 `WorkspaceWrite` 策略保持不变。
    pub fn with_additional_writable_roots(mut self, roots: Vec<PathBuf>) -> Self {
        if let SandboxPolicy::WorkspaceWrite { writable_roots, .. } = &mut self.policy {
            for root in roots {
                if !writable_roots.contains(&root) {
                    writable_roots.push(root);
                }
            }
        }
        self
    }

    /// 统一入口：`tool_name` 与输入形状必须匹配，否则 fail-closed。
    ///
    /// 已知工具名：
    /// - `bash` / `powershell` / `execute_bash`：shell；
    /// - `mcp` / `mcp_stdio`：MCP 子进程；
    /// - `write` / `edit` / `apply_patch`：文件写入；
    /// - `read`：读取。
    pub fn resolve(&self, tool_name: &str, input: &ToolInput<'_>) -> SandboxDecision {
        if let Some(reason) = &self.invalid_reason {
            return SandboxDecision::Deny(SandboxDenial::new(
                "INVALID_CONFIG",
                format!("invalid sandbox configuration: {reason}"),
            ));
        }
        let expected = match input {
            ToolInput::Shell { .. } => ToolKind::Shell,
            ToolInput::McpProcess { .. } => ToolKind::Mcp,
            ToolInput::FileWrite { .. } => ToolKind::FileWrite,
            ToolInput::FileRead { .. } => ToolKind::FileRead,
        };
        let actual = match tool_name {
            "bash" | "powershell" | "execute_bash" => ToolKind::Shell,
            "mcp" | "mcp_stdio" => ToolKind::Mcp,
            "write" | "edit" | "apply_patch" => ToolKind::FileWrite,
            "read" => ToolKind::FileRead,
            _ => {
                return SandboxDecision::Deny(SandboxDenial::new(
                    "UNKNOWN_TOOL",
                    format!("no execution policy is defined for tool {tool_name:?}"),
                ));
            }
        };
        if actual != expected {
            return SandboxDecision::Deny(SandboxDenial::new(
                "TOOL_INPUT_MISMATCH",
                format!(
                    "tool {tool_name:?} was resolved with a {:?} input; this is a caller bug, \
                     refusing to guess",
                    expected
                ),
            ));
        }
        match input {
            ToolInput::Shell { .. } | ToolInput::McpProcess { .. } | ToolInput::FileRead { .. } => {
                SandboxDecision::Allow(self.policy.clone())
            }
            ToolInput::FileWrite { paths } => {
                if paths.is_empty() {
                    return SandboxDecision::Deny(SandboxDenial::new(
                        "NO_WRITE_PATHS",
                        "file mutation was resolved without any target paths",
                    ));
                }
                for path in *paths {
                    if let Err(denial) = self.check_write_path(path) {
                        return SandboxDecision::Deny(denial);
                    }
                }
                SandboxDecision::Allow(self.policy.clone())
            }
        }
    }

    /// 文件写入专用入口：校验工具名并返回归一化后的真实路径。
    ///
    /// `resolve("write", FileWrite{..})` 已执行同一套检查；需要真实路径的调用方用本方法
    /// 代替「先 `resolve` 再 `check_write_path`」，避免同一路径 realpath 两次（两次之间
    /// 目录被替换时结果还可能不一致）。
    pub fn resolve_write_target(
        &self,
        tool_name: &str,
        path: &Path,
    ) -> Result<PathBuf, SandboxDenial> {
        match tool_name {
            "write" | "edit" | "apply_patch" => self.check_write_path(path),
            _ => Err(SandboxDenial::new(
                "UNKNOWN_TOOL",
                format!("no write policy is defined for tool {tool_name:?}"),
            )),
        }
    }

    /// 单个写入路径的门禁。
    ///
    /// 返回归一化后的真实路径（用于后续读记录 key），拒绝时给出稳定错误码。
    pub fn check_write_path(&self, path: &Path) -> Result<PathBuf, SandboxDenial> {
        if let Some(reason) = &self.invalid_reason {
            return Err(SandboxDenial::new(
                "INVALID_CONFIG",
                format!("invalid sandbox configuration: {reason}"),
            ));
        }
        let real = resolve_real_path(path).map_err(|error| {
            SandboxDenial::new(
                "PATH_UNRESOLVED",
                format!("cannot resolve {}: {error}", path.display()),
            )
        })?;
        match &self.policy {
            SandboxPolicy::DangerFullAccess => Ok(real),
            SandboxPolicy::ReadOnly { .. } => Err(SandboxDenial::new(
                "READ_ONLY",
                format!(
                    "{} is not writable: the sandbox policy is read-only",
                    real.display()
                ),
            )),
            SandboxPolicy::WorkspaceWrite {
                writable_roots,
                read_only_subpaths,
                ..
            } => {
                let case_insensitive = case_insensitive_filesystem();
                let mut matched_root: Option<PathBuf> = None;
                for root in writable_roots {
                    let root = canonical_or_lexical(root);
                    if path_is_within(&root, &real, case_insensitive) {
                        matched_root = Some(root);
                        break;
                    }
                }
                let Some(root) = matched_root else {
                    return Err(SandboxDenial::new(
                        "OUTSIDE_WORKSPACE",
                        format!(
                            "{} is outside the writable roots ({})",
                            real.display(),
                            writable_roots
                                .iter()
                                .map(|root| root.display().to_string())
                                .collect::<Vec<_>>()
                                .join(", ")
                        ),
                    ));
                };
                for protected in protected_subpaths_for(&root, read_only_subpaths) {
                    if path_is_within(&protected, &real, case_insensitive) {
                        return Err(SandboxDenial::new(
                            "PROTECTED_PATH",
                            format!(
                                "{} is inside the protected path {} (.git/.kiana cannot be \
                                 modified by tools)",
                                real.display(),
                                protected.display()
                            ),
                        ));
                    }
                }
                Ok(real)
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ToolKind {
    Shell,
    Mcp,
    FileWrite,
    FileRead,
}

/// `KIANA_SANDBOX` 是否被显式设成「关闭沙箱」。
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

/// 当前平台的文件系统比较是否默认忽略大小写。
///
/// 只有 Windows 需要：macOS 默认的 APFS 也忽略大小写，但 `canonicalize` 会返回磁盘上的
/// 真实大小写，根与目标因此天然一致；反过来猜测「忽略大小写」会在大小写敏感的卷上把
/// 根外面的路径判成根内（宽松方向），所以非 Windows 一律按敏感比较。
pub fn case_insensitive_filesystem() -> bool {
    cfg!(windows)
}

/// 路径是否在 `root` 内（含等于）。
///
/// 纯组件比较，不做 Unicode 归一。`case_insensitive` 只影响组件文本比较，调用方必须在
/// 大小写敏感的平台上传 `false`。
pub fn path_is_within(root: &Path, path: &Path, case_insensitive: bool) -> bool {
    let root: Vec<Component<'_>> = root.components().collect();
    let path: Vec<Component<'_>> = path.components().collect();
    if root.len() > path.len() {
        return false;
    }
    root.iter().zip(path.iter()).all(|(root, path)| {
        let root = root.as_os_str();
        let path = path.as_os_str();
        if case_insensitive {
            root.to_string_lossy()
                .eq_ignore_ascii_case(&path.to_string_lossy())
        } else {
            root == path
        }
    })
}

/// 符号链接展开的最大跳数（与 Linux 的 `MAXSYMLINKS` 一致）。
const MAX_SYMLINK_DEPTH: usize = 40;

/// 读取归一化后的真实路径。
///
/// 已存在的完整路径直接 `canonicalize`（展开符号链接、得到磁盘上的真实大小写）；
/// 路径里任何组件是符号链接时逐跳解析，**包括最终组件指向不存在目标的悬空链接**
/// （否则「工作区内 `<link>` 指向工作区外」会被误判为工作区内的新文件）。剩余不存在
/// 的组件做纯词法的 `.`/`..` 归一。任何非 `NotFound`/`NotADirectory` 的错误都上抛，
/// 让调用方 fail-closed。
pub fn resolve_real_path(path: &Path) -> io::Result<PathBuf> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    resolve_real_path_inner(&absolute, 0)
}

fn resolve_real_path_inner(path: &Path, depth: usize) -> io::Result<PathBuf> {
    if depth > MAX_SYMLINK_DEPTH {
        return Err(symlink_loop_error());
    }
    match std::fs::canonicalize(path) {
        Ok(resolved) => return Ok(normalize_lexical(&resolved)),
        // 路径（或某个祖先）不存在：退到逐组件解析，继续展开符号链接。
        Err(error) if !is_missing(&error) => return Err(error),
        Err(_) => {}
    }

    let components: Vec<Component<'_>> = path.components().collect();
    let mut resolved = PathBuf::new();
    let mut missing = false;
    for (index, component) in components.iter().enumerate() {
        match component {
            Component::Prefix(prefix) => resolved.push(prefix.as_os_str()),
            Component::RootDir => resolved.push(Component::RootDir.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => {
                resolved.pop();
            }
            Component::Normal(name) => {
                let candidate = resolved.join(name);
                // 一旦有祖先不存在，后续组件不可能再是符号链接；按词法拼接即可。
                if missing {
                    resolved = candidate;
                    continue;
                }
                match std::fs::symlink_metadata(&candidate) {
                    Ok(metadata) if metadata.file_type().is_symlink() => {
                        let target = std::fs::read_link(&candidate)?;
                        let mut next = if target.is_absolute() {
                            target
                        } else {
                            resolved.join(target)
                        };
                        for remaining in &components[index + 1..] {
                            match remaining {
                                Component::Prefix(prefix) => next.push(prefix.as_os_str()),
                                Component::RootDir => next.push(Component::RootDir.as_os_str()),
                                Component::CurDir => {}
                                Component::ParentDir => {
                                    next.pop();
                                }
                                Component::Normal(name) => next.push(name),
                            }
                        }
                        return resolve_real_path_inner(&normalize_lexical(&next), depth + 1);
                    }
                    Ok(_) => resolved = candidate,
                    Err(error) if is_missing(&error) => {
                        missing = true;
                        resolved = candidate;
                    }
                    Err(error) => return Err(error),
                }
            }
        }
    }
    Ok(normalize_lexical(&resolved))
}

#[cfg(unix)]
fn symlink_loop_error() -> io::Error {
    io::Error::from_raw_os_error(libc::ELOOP)
}

#[cfg(not(unix))]
fn symlink_loop_error() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidInput,
        "too many levels of symbolic links",
    )
}

fn is_missing(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
    )
}

/// 可写根的 canonical 形式；不存在时退化为词法绝对路径。
fn canonical_or_lexical(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| {
        let absolute = if path.is_absolute() {
            path.to_path_buf()
        } else {
            std::env::current_dir()
                .map(|cwd| cwd.join(path))
                .unwrap_or_else(|_| path.to_path_buf())
        };
        normalize_lexical(&absolute)
    })
}

/// `root` 下必须保持只读的子路径：策略显式列表 + `.git`/`.kiana` 默认保护。
fn protected_subpaths_for(root: &Path, configured: &[PathBuf]) -> Vec<PathBuf> {
    let mut protected: Vec<PathBuf> = configured
        .iter()
        .map(|path| {
            if path.is_absolute() {
                canonical_or_lexical(path)
            } else {
                canonical_or_lexical(&root.join(path))
            }
        })
        .collect();
    for default in default_protected_subpaths(root) {
        let default = canonical_or_lexical(&default);
        if !protected.contains(&default) {
            protected.push(default);
        }
    }
    protected
}

/// 纯词法归一化：去掉 `.`，把 `..` 折叠到前一个普通组件（不穿透符号链接）。
pub fn normalize_lexical(path: &Path) -> PathBuf {
    let mut output = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Prefix(prefix) => output.push(prefix.as_os_str()),
            Component::RootDir => output.push(Component::RootDir.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => {
                output.pop();
            }
            Component::Normal(name) => output.push(name),
        }
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    fn workspace() -> std::path::PathBuf {
        std::path::PathBuf::from("/workspace")
    }

    #[test]
    fn shell_and_mcp_are_allowed_under_the_policy() {
        let policy = ExecutionPolicy::new(SandboxPolicy::workspace_write(workspace()), workspace());
        assert!(
            policy
                .resolve("bash", &ToolInput::Shell { command: "ls" })
                .is_allowed()
        );
        assert!(
            policy
                .resolve(
                    "mcp",
                    &ToolInput::McpProcess {
                        program: "node",
                        args: &[]
                    }
                )
                .is_allowed()
        );
    }

    #[test]
    fn unknown_tools_and_mismatched_inputs_fail_closed() {
        let policy = ExecutionPolicy::new(SandboxPolicy::danger_full_access(), workspace());
        let decision = policy.resolve("web_fetch", &ToolInput::Shell { command: "ls" });
        assert_eq!(decision.denial().expect("denied").code, "UNKNOWN_TOOL");
        let decision = policy.resolve(
            "write",
            &ToolInput::Shell {
                command: "echo hi > /tmp/x",
            },
        );
        assert_eq!(
            decision.denial().expect("denied").code,
            "TOOL_INPUT_MISMATCH"
        );
    }

    #[test]
    fn resolve_write_target_returns_the_resolved_path_and_checks_the_tool() {
        let policy = ExecutionPolicy::new(SandboxPolicy::danger_full_access(), workspace());
        let path = workspace().join("sub/../file.txt");
        let resolved = policy
            .resolve_write_target("write", &path)
            .expect("danger-full-access allows writes");
        assert_eq!(resolved, workspace().join("file.txt"));
        let denial = policy
            .resolve_write_target("bash", &path)
            .expect_err("bash is not a write tool");
        assert_eq!(denial.code, "UNKNOWN_TOOL");
    }

    #[test]
    fn file_write_requires_at_least_one_path() {
        let policy = ExecutionPolicy::new(SandboxPolicy::danger_full_access(), workspace());
        let decision = policy.resolve("write", &ToolInput::FileWrite { paths: &[] });
        assert_eq!(decision.denial().expect("denied").code, "NO_WRITE_PATHS");
    }

    #[test]
    fn read_only_rejects_every_write() {
        let policy = ExecutionPolicy::new(SandboxPolicy::read_only(), workspace());
        let path = workspace().join("file.txt");
        let decision = policy.resolve("write", &ToolInput::FileWrite { paths: &[path] });
        assert_eq!(decision.denial().expect("denied").code, "READ_ONLY");
    }

    #[test]
    fn path_comparison_handles_case_sensitivity() {
        assert!(!path_is_within(
            Path::new("/Workspace"),
            Path::new("/workspace/file.txt"),
            false
        ));
        assert!(path_is_within(
            Path::new("/Workspace"),
            Path::new("/workspace/file.txt"),
            true
        ));
        assert!(!path_is_within(
            Path::new("/workspace"),
            Path::new("/workspace2/file.txt"),
            false
        ));
    }

    #[test]
    fn lexical_normalization_removes_dot_and_parent_components() {
        assert_eq!(
            normalize_lexical(Path::new("/a/./b/../c")),
            PathBuf::from("/a/c")
        );
    }

    #[test]
    fn invalid_configuration_denies_everything() {
        let policy = ExecutionPolicy {
            policy: SandboxPolicy::danger_full_access(),
            cwd: workspace(),
            invalid_reason: Some("bogus".to_string()),
        };
        let decision = policy.resolve("bash", &ToolInput::Shell { command: "true" });
        assert_eq!(decision.denial().expect("denied").code, "INVALID_CONFIG");
    }

    #[test]
    fn execution_status_messages_are_safety_contracts() {
        let unconfirmed = ExecutionStatus::Unconfirmed {
            reason: "relay killed".to_string(),
        };
        let message = unconfirmed.user_message();
        assert!(message.contains("may have already executed"));
        assert!(message.contains("Do not retry automatically"));

        let unavailable = ExecutionStatus::BackendUnavailable {
            reason: "no backend".to_string(),
        };
        let message = unavailable.user_message();
        assert!(message.contains("was not executed"));
        assert!(message.contains("unsandboxed"));
    }

    #[test]
    fn sandbox_errors_map_to_backend_unavailable() {
        let error = SandboxError::Unavailable {
            policy: "workspace-write",
            reason: "no bwrap".to_string(),
        };
        assert!(matches!(
            ExecutionStatus::from_sandbox_error(&error),
            ExecutionStatus::BackendUnavailable { .. }
        ));
    }

    #[test]
    fn only_backend_unavailable_may_be_retried_automatically() {
        let unconfirmed = ExecutionStatus::Unconfirmed {
            reason: "relay killed".to_string(),
        };
        assert!(unconfirmed.may_have_executed());
        assert!(
            !unconfirmed.allows_automatic_retry(),
            "an unconfirmed command must never be retried automatically"
        );

        let unavailable = ExecutionStatus::BackendUnavailable {
            reason: "no backend".to_string(),
        };
        assert!(!unavailable.may_have_executed());
        assert!(unavailable.allows_automatic_retry());

        let interrupted = ExecutionStatus::Interrupted {
            reason: "cancelled".to_string(),
        };
        assert!(!interrupted.may_have_executed());
        assert!(!interrupted.allows_automatic_retry());

        assert!(
            !ExecutionStatus::ConfirmedExit {
                code: 0,
                signal: None
            }
            .allows_automatic_retry()
        );
    }

    #[test]
    fn every_shell_tool_name_resolves_to_the_same_policy() {
        let policy = ExecutionPolicy::new(SandboxPolicy::workspace_write(workspace()), workspace());
        for tool in ["bash", "powershell", "execute_bash"] {
            let decision = policy.resolve(tool, &ToolInput::Shell { command: "true" });
            assert!(decision.is_allowed(), "{tool} should be allowed");
            assert_eq!(
                decision.policy().map(SandboxPolicy::label),
                Some("workspace-write")
            );
        }
        let decision = policy.resolve(
            "mcp",
            &ToolInput::McpProcess {
                program: "node",
                args: &[],
            },
        );
        assert_eq!(
            decision.policy().map(SandboxPolicy::label),
            Some("workspace-write")
        );
    }

    #[test]
    fn reads_are_not_restricted_by_the_write_policy() {
        let path = workspace().join("file.txt");
        for policy in [
            SandboxPolicy::read_only(),
            SandboxPolicy::workspace_write(workspace()),
            SandboxPolicy::danger_full_access(),
        ] {
            let execution = ExecutionPolicy::new(policy, workspace());
            let decision = execution.resolve(
                "read",
                &ToolInput::FileRead {
                    paths: std::slice::from_ref(&path),
                },
            );
            assert!(
                decision.is_allowed(),
                "read must stay available regardless of the write policy"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn dangling_symlink_cannot_escape_a_writable_root() {
        use std::os::unix::fs::symlink;

        let root = tempfile::tempdir().expect("root");
        let outside = tempfile::tempdir().expect("outside");
        let policy = ExecutionPolicy::new(SandboxPolicy::workspace_write(root.path()), root.path());

        // 最终组件是指向工作区外的悬空链接：canonicalize 会因目标不存在失败，
        // 逐组件解析必须展开它，否则会被当成工作区内的新文件放行。
        let link = root.path().join("escape");
        symlink(outside.path().join("not-yet-created.txt"), &link).expect("symlink");
        let denial = policy
            .resolve("write", &ToolInput::FileWrite { paths: &[link] })
            .denial()
            .expect("dangling symlink escape must be denied")
            .clone();
        assert_eq!(denial.code, "OUTSIDE_WORKSPACE");
    }

    #[cfg(unix)]
    #[test]
    fn symlink_loops_fail_closed() {
        use std::os::unix::fs::symlink;

        let root = tempfile::tempdir().expect("root");
        let policy = ExecutionPolicy::new(SandboxPolicy::workspace_write(root.path()), root.path());
        let loop_path = root.path().join("loop");
        symlink(&loop_path, &loop_path).expect("symlink loop");
        let denial = policy
            .resolve(
                "write",
                &ToolInput::FileWrite {
                    paths: &[loop_path],
                },
            )
            .denial()
            .expect("symlink loop must be denied")
            .clone();
        assert_eq!(denial.code, "PATH_UNRESOLVED");
    }
}
