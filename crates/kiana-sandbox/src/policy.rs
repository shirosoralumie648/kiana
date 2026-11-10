//! 沙箱策略。

use std::path::PathBuf;

/// 一条命令被允许看到/写入的世界。
///
/// 与 Codex 的 `PermissionProfile` 相比，这里刻意只保留三种形状；审批、per-tool
/// 规则、网络代理等由上层负责。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SandboxPolicy {
    /// 整个文件系统只读。
    ///
    /// bubblewrap 后端会给命令一个私有 tmpfs `/tmp`（可写但退出即丢弃）；
    /// Landlock 后端没有任何可写根，连 `/tmp` 也拒绝。
    ReadOnly {
        /// 是否保留宿主网络命名空间。
        network_access: bool,
    },
    /// 整个文件系统只读，但列出的根目录可写；可写的根目录内还能再指定只读子路径
    /// （默认 `.git` / `.kiana`）。
    WorkspaceWrite {
        /// 可写根目录。
        writable_roots: Vec<PathBuf>,
        /// 即使位于可写根目录内也保持只读的子路径。
        ///
        /// bubblewrap 用后置 `--ro-bind` 施加；Landlock 的 path-beneath 规则只有加法，
        /// 无法表达这种减法，因此 Landlock 回退不保证该字段生效。
        read_only_subpaths: Vec<PathBuf>,
        /// 是否保留宿主网络命名空间。
        network_access: bool,
    },
    /// 不做任何隔离。
    DangerFullAccess,
}

/// 可写根目录下默认保持只读的目录名。
///
/// `.git` 防止模型改写历史/钩子；`.kiana` 是 Kiana 自己的会话与本地产物目录。
/// 与 Codex 的 `.git` / `.agents` / `.codex` 对应，但用 Kiana 自己的目录名。
pub const DEFAULT_PROTECTED_SUBDIR_NAMES: [&str; 2] = [".git", ".kiana"];

impl SandboxPolicy {
    /// 只读、无网络。
    pub fn read_only() -> Self {
        Self::ReadOnly {
            network_access: false,
        }
    }

    /// 只读、保留网络。
    pub fn read_only_with_network() -> Self {
        Self::ReadOnly {
            network_access: true,
        }
    }

    /// 单根 workspace-write：`root` 可写，默认保护 `.git` / `.kiana`，无网络。
    pub fn workspace_write(root: impl Into<PathBuf>) -> Self {
        let root = root.into();
        let read_only_subpaths = default_protected_subpaths(&root);
        Self::WorkspaceWrite {
            writable_roots: vec![root],
            read_only_subpaths,
            network_access: false,
        }
    }

    /// 多根 workspace-write；每个根下默认保护 `.git` / `.kiana`。
    pub fn workspace_write_with_roots(roots: Vec<PathBuf>) -> Self {
        let mut read_only_subpaths = Vec::new();
        for root in &roots {
            read_only_subpaths.extend(default_protected_subpaths(root));
        }
        Self::WorkspaceWrite {
            writable_roots: roots,
            read_only_subpaths,
            network_access: false,
        }
    }

    /// 完全放行。
    pub fn danger_full_access() -> Self {
        Self::DangerFullAccess
    }

    /// 是否需要隔离。
    pub fn requires_sandbox(&self) -> bool {
        !matches!(self, Self::DangerFullAccess)
    }

    /// 是否允许网络。
    pub fn network_access(&self) -> bool {
        match self {
            Self::ReadOnly { network_access } | Self::WorkspaceWrite { network_access, .. } => {
                *network_access
            }
            Self::DangerFullAccess => true,
        }
    }

    /// 可写根目录；`ReadOnly` 与 `DangerFullAccess` 为空。
    pub fn writable_roots(&self) -> &[PathBuf] {
        match self {
            Self::WorkspaceWrite { writable_roots, .. } => writable_roots,
            _ => &[],
        }
    }

    /// 可写根目录内保持只读的子路径。
    pub fn read_only_subpaths(&self) -> &[PathBuf] {
        match self {
            Self::WorkspaceWrite {
                read_only_subpaths, ..
            } => read_only_subpaths,
            _ => &[],
        }
    }

    /// 修改网络开关；`DangerFullAccess` 保持不变。
    pub fn with_network_access(mut self, enabled: bool) -> Self {
        match &mut self {
            Self::ReadOnly { network_access } | Self::WorkspaceWrite { network_access, .. } => {
                *network_access = enabled
            }
            Self::DangerFullAccess => {}
        }
        self
    }

    /// 替换只读子路径；非 `WorkspaceWrite` 保持不变。
    pub fn with_read_only_subpaths(mut self, paths: Vec<PathBuf>) -> Self {
        if let Self::WorkspaceWrite {
            read_only_subpaths, ..
        } = &mut self
        {
            *read_only_subpaths = paths;
        }
        self
    }

    /// 诊断用稳定名称。
    pub fn label(&self) -> &'static str {
        match self {
            Self::ReadOnly { .. } => "read-only",
            Self::WorkspaceWrite { .. } => "workspace-write",
            Self::DangerFullAccess => "danger-full-access",
        }
    }
}

/// 某个根目录下「默认保护」的子路径。
///
/// 返回路径本身，不判断是否存在：bubblewrap 在每次 `spawn` 构造参数时检查，
/// 这样构造 manager 之后才创建的 `.git` 也能被保护。Landlock 不支持减法规则，
/// 不会使用这个列表。
pub fn default_protected_subpaths(root: &std::path::Path) -> Vec<PathBuf> {
    DEFAULT_PROTECTED_SUBDIR_NAMES
        .iter()
        .map(|name| root.join(name))
        .collect()
}
