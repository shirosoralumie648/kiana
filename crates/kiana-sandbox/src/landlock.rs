//! Linux Landlock 后端。
//!
//! Landlock 是内核的 unprivileged LSM：进程在 `restrict_self` 之后只能访问 ruleset
//! 明确允许的路径。本实现处理**写类**文件系统访问（创建/删除/重命名/截断/写文件），
//! 不处理读与执行，因此文件系统语义是「全盘可读，只有白名单可写」——与 Codex 的
//! legacy Landlock 回退一致。
//!
//! 网络方面，ABI 4+ 可以处理 `BIND_TCP` / `CONNECT_TCP`；策略禁止网络时不添加任何
//! 网络规则，等于拒绝这两个操作。UDP、ICMP、Unix socket 不在 Landlock 的覆盖范围内，
//! 因此 Landlock 后端不是完整的网络沙箱；需要完整隔离时应使用 bubblewrap。
//!
//! ruleset 在父进程构造好（包含所有 path rules），子进程只需在 `pre_exec` 里
//! `prctl(PR_SET_NO_NEW_PRIVS)` + `landlock_restrict_self(fd)`，不涉及分配，满足
//! async-signal-safe 约束。

use std::ffi::CString;
use std::io;
use std::os::fd::RawFd;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use crate::error::SandboxError;
use crate::policy::SandboxPolicy;

/// `landlock_create_ruleset(NULL, 0, LANDLOCK_CREATE_RULESET_VERSION)`。
const LANDLOCK_CREATE_RULESET_VERSION: u32 = 1;
/// `LANDLOCK_RULE_PATH_BENEATH`。
const LANDLOCK_RULE_PATH_BENEATH: u32 = 1;

// linux/landlock.h 的访问位。只在支持的 ABI 上设置对应位，否则
// `landlock_create_ruleset` 会返回 EINVAL。
const ACCESS_FS_WRITE_FILE: u64 = 1 << 1;
const ACCESS_FS_REMOVE_DIR: u64 = 1 << 4;
const ACCESS_FS_REMOVE_FILE: u64 = 1 << 5;
const ACCESS_FS_MAKE_CHAR: u64 = 1 << 6;
const ACCESS_FS_MAKE_DIR: u64 = 1 << 7;
const ACCESS_FS_MAKE_REG: u64 = 1 << 8;
const ACCESS_FS_MAKE_SOCK: u64 = 1 << 9;
const ACCESS_FS_MAKE_FIFO: u64 = 1 << 10;
const ACCESS_FS_MAKE_BLOCK: u64 = 1 << 11;
const ACCESS_FS_MAKE_SYM: u64 = 1 << 12;
const ACCESS_FS_REFER: u64 = 1 << 13;
const ACCESS_FS_TRUNCATE: u64 = 1 << 14;
/// ABI 5（Linux 6.10+）新增。
const ACCESS_FS_IOCTL_DEV: u64 = 1 << 15;

// linux/landlock.h 的网络访问位（ABI 4）。
const ACCESS_NET_BIND_TCP: u64 = 1 << 0;
const ACCESS_NET_CONNECT_TCP: u64 = 1 << 1;

#[repr(C)]
struct RulesetAttr {
    handled_access_fs: u64,
    handled_access_net: u64,
}

/// `struct landlock_path_beneath_attr` 是 packed（u64 + i32 = 12 字节）。
/// `repr(C)` 的前 12 字节布局与 packed 相同，内核只读这 12 字节。
#[repr(C)]
struct PathBeneathAttr {
    allowed_access: u64,
    parent_fd: i32,
}

/// 已构造好的 Landlock ruleset。`fd` 在 `Drop` 时关闭。
pub(crate) struct LandlockRuleset {
    fd: RawFd,
}

impl std::fmt::Debug for LandlockRuleset {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("LandlockRuleset")
            .field("fd", &self.fd)
            .finish()
    }
}

impl Drop for LandlockRuleset {
    fn drop(&mut self) {
        // SAFETY: fd 由 landlock_create_ruleset 返回，且本结构独占所有权。
        unsafe {
            libc::close(self.fd);
        }
    }
}

impl LandlockRuleset {
    /// 为策略构造 ruleset；内核不支持 Landlock 时返回 `Ok(None)`。
    ///
    /// ruleset 始终允许写 `/dev/null` 等标准设备（否则 `> /dev/null` 会被拒绝），
    /// 再逐个允许策略的可写根目录。
    pub(crate) fn new(policy: &SandboxPolicy) -> Result<Option<Self>, SandboxError> {
        Self::new_with_extra_writable_roots(policy, &[])
    }

    /// 同 [`Self::new`]，额外允许写 `extra` 中的路径。
    ///
    /// 只给 sandbox relay 使用：relay 在 Landlock 下承担监督进程职责，需要写自己的
    /// 私有状态目录；该目录权限 0700、只包含状态文件，把它列入可写根不会扩大用户命令
    /// 能触及的宿主范围。
    pub(crate) fn new_with_extra_writable_roots(
        policy: &SandboxPolicy,
        extra: &[PathBuf],
    ) -> Result<Option<Self>, SandboxError> {
        if !policy.requires_sandbox() {
            return Ok(None);
        }
        let Some(abi) = abi_version() else {
            return Ok(None);
        };
        // ABI 3 之前没有 TRUNCATE 访问位：`truncate(2)` 完全不受 Landlock 约束，
        // 「白名单可写」的承诺不成立。宁可让上层 fail-closed，也不启用残缺的隔离。
        // 策略禁止网络时还必须能真正拦住网络；ABI 4 之前 Landlock 没有网络访问位。
        if !abi_can_enforce(policy, abi) {
            return Ok(None);
        }
        let restrict_network = !policy.network_access();
        let write_access = write_access_mask(abi);
        let attr = RulesetAttr {
            handled_access_fs: write_access,
            handled_access_net: if restrict_network {
                ACCESS_NET_BIND_TCP | ACCESS_NET_CONNECT_TCP
            } else {
                0
            },
        };
        let attr_size = if abi >= 4 {
            std::mem::size_of::<RulesetAttr>()
        } else {
            std::mem::size_of::<u64>()
        };

        // SAFETY: attr 是有效的 RulesetAttr，size 不超过结构体大小，flags 为 0。
        let fd = unsafe {
            libc::syscall(
                libc::SYS_landlock_create_ruleset,
                &attr as *const RulesetAttr,
                attr_size,
                0u32,
            )
        };
        if fd < 0 {
            return Err(SandboxError::Landlock(io::Error::last_os_error()));
        }
        // ruleset 只用于 pre_exec 里的 `restrict_self`；子进程 exec 后不应继续持有它。
        // SAFETY: fd 刚由 syscall 创建，本函数独占所有权。
        unsafe {
            libc::fcntl(fd as RawFd, libc::F_SETFD, libc::FD_CLOEXEC);
        }
        let ruleset = Self { fd: fd as RawFd };

        // ABI 5 起 `IOCTL_DEV` 可被处理：只在标准设备上显式放行，其它设备节点
        // （例如可写根里 mknod 出来的、或宿主块设备）的 ioctl 一律拒绝。
        let mut file_access = ACCESS_FS_WRITE_FILE | if abi >= 3 { ACCESS_FS_TRUNCATE } else { 0 };
        if abi >= 5 {
            file_access |= ACCESS_FS_IOCTL_DEV;
        }
        for path in [
            "/dev/null",
            "/dev/zero",
            "/dev/full",
            "/dev/random",
            "/dev/urandom",
            "/dev/tty",
        ] {
            let path = Path::new(path);
            if !path.exists() {
                continue;
            }
            ruleset
                .add_path_rule(path, file_access)
                .map_err(SandboxError::Landlock)?;
        }

        // `write_access` 是 ruleset 处理的集合（含 MAKE_CHAR/MAKE_BLOCK/IOCTL_DEV）；
        // 可写根只授予 `writable_root_access`，即不含创建设备节点与设备 ioctl 的位。
        // 处理但不授予 = 拒绝：可写根内 `mknod` 与任意设备 ioctl 都会 EACCES。
        let writable_root_access = writable_root_access(abi);
        for root in policy.writable_roots().iter().chain(extra.iter()) {
            if !root.exists() {
                continue;
            }
            ruleset
                .add_path_rule(root, writable_root_access)
                .map_err(SandboxError::Landlock)?;
        }

        Ok(Some(ruleset))
    }

    /// 子进程 `pre_exec` 使用的原始 fd。
    pub(crate) fn fd(&self) -> RawFd {
        self.fd
    }

    fn add_path_rule(&self, path: &Path, access: u64) -> io::Result<()> {
        let path = CString::new(path.as_os_str().as_bytes()).map_err(|_| {
            io::Error::new(io::ErrorKind::InvalidInput, "path contains an interior NUL")
        })?;
        // SAFETY: path 是有效的 NUL 结尾字符串；fd 由本函数独占并在下面关闭。
        let path_fd = unsafe { libc::open(path.as_ptr(), libc::O_PATH | libc::O_CLOEXEC) };
        if path_fd < 0 {
            return Err(io::Error::last_os_error());
        }
        let attr = PathBeneathAttr {
            allowed_access: access,
            parent_fd: path_fd,
        };
        // SAFETY: attr 是有效的 PathBeneathAttr；ruleset fd 由 self 持有。
        let result = unsafe {
            libc::syscall(
                libc::SYS_landlock_add_rule,
                self.fd,
                LANDLOCK_RULE_PATH_BENEATH,
                &attr as *const PathBeneathAttr,
                0u32,
            )
        };
        let error = io::Error::last_os_error();
        // SAFETY: path_fd 由本函数打开，尚未转移所有权。
        unsafe {
            libc::close(path_fd);
        }
        if result < 0 { Err(error) } else { Ok(()) }
    }
}

/// 当前内核支持的 Landlock ABI；不支持时返回 `None`。
fn abi_version() -> Option<u32> {
    // SAFETY: 按 man landlock_create_ruleset(2)，attr=NULL/size=0 与 VERSION 标志
    // 只查询 ABI，不创建对象。
    let version = unsafe {
        libc::syscall(
            libc::SYS_landlock_create_ruleset,
            std::ptr::null::<RulesetAttr>(),
            0usize,
            LANDLOCK_CREATE_RULESET_VERSION,
        )
    };
    (version >= 1).then_some(version as u32)
}

/// 给定 ABI 是否能完整兑现策略。
///
/// 这是纯函数，便于在没有特定内核的机器上测试 ABI<4 的 fail-closed 分支：
/// - ABI<3 没有 `TRUNCATE`，连「白名单可写」都不成立；
/// - 策略禁止网络时 ABI<4 没有网络访问位，无法兑现。
pub(crate) fn abi_can_enforce(policy: &SandboxPolicy, abi: u32) -> bool {
    if !policy.requires_sandbox() {
        return true;
    }
    if abi < 3 {
        return false;
    }
    !(!policy.network_access() && abi < 4)
}

/// 按 ABI 版本构造写类访问掩码。
///
/// `MAKE_CHAR` / `MAKE_BLOCK` 从 ABI 1 就存在：不处理它们的话，沙箱内的进程只要
/// 对可写根有 `MAKE_*` 权限就能 `mknod`，在 root 容器里可能直接写到块设备。
fn write_access_mask(abi: u32) -> u64 {
    let mut mask = ACCESS_FS_WRITE_FILE
        | ACCESS_FS_REMOVE_DIR
        | ACCESS_FS_REMOVE_FILE
        | ACCESS_FS_MAKE_CHAR
        | ACCESS_FS_MAKE_DIR
        | ACCESS_FS_MAKE_REG
        | ACCESS_FS_MAKE_SOCK
        | ACCESS_FS_MAKE_FIFO
        | ACCESS_FS_MAKE_BLOCK
        | ACCESS_FS_MAKE_SYM;
    if abi >= 2 {
        mask |= ACCESS_FS_REFER;
    }
    if abi >= 3 {
        mask |= ACCESS_FS_TRUNCATE;
    }
    if abi >= 5 {
        mask |= ACCESS_FS_IOCTL_DEV;
    }
    mask
}

/// 可写根实际授予的访问位：处理设备节点创建与设备 ioctl，但不授予。
///
/// 只把位放进 ruleset 的 handled 集合（[`write_access_mask`]）而**不**授予到路径规则，
/// 等价于拒绝该操作；因此可写根内不能 `mknod`，也不能对设备文件发 ioctl。
fn writable_root_access(abi: u32) -> u64 {
    write_access_mask(abi) & !(ACCESS_FS_MAKE_CHAR | ACCESS_FS_MAKE_BLOCK | ACCESS_FS_IOCTL_DEV)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn access_mask_is_abi_aware() {
        let v1 = write_access_mask(1);
        assert_eq!(v1 & ACCESS_FS_REFER, 0);
        assert_eq!(v1 & ACCESS_FS_TRUNCATE, 0);
        let v2 = write_access_mask(2);
        assert_ne!(v2 & ACCESS_FS_REFER, 0);
        assert_eq!(v2 & ACCESS_FS_TRUNCATE, 0);
        let v3 = write_access_mask(3);
        assert_ne!(v3 & ACCESS_FS_TRUNCATE, 0);
        assert_eq!(v3 & ACCESS_FS_IOCTL_DEV, 0);
        let v5 = write_access_mask(5);
        assert_ne!(v5 & ACCESS_FS_IOCTL_DEV, 0);
    }

    #[test]
    fn writable_roots_do_not_grant_device_creation_or_ioctl() {
        // handled 集合包含这些位（从而拒绝），但可写根的 path rule 不能授予它们。
        let handled = write_access_mask(5);
        assert_ne!(handled & ACCESS_FS_MAKE_CHAR, 0);
        assert_ne!(handled & ACCESS_FS_MAKE_BLOCK, 0);
        assert_ne!(handled & ACCESS_FS_IOCTL_DEV, 0);

        let grant = writable_root_access(5);
        assert_eq!(grant & ACCESS_FS_MAKE_CHAR, 0);
        assert_eq!(grant & ACCESS_FS_MAKE_BLOCK, 0);
        assert_eq!(grant & ACCESS_FS_IOCTL_DEV, 0);
        assert_ne!(grant & ACCESS_FS_MAKE_REG, 0);
        assert_ne!(grant & ACCESS_FS_WRITE_FILE, 0);
    }

    #[test]
    fn write_mask_blocks_device_node_creation() {
        // 不处理这两个位时，可写根内的 `mknod` 会绕过沙箱（root 容器里可以写块设备）。
        let mask = write_access_mask(1);
        assert_ne!(mask & ACCESS_FS_MAKE_CHAR, 0);
        assert_ne!(mask & ACCESS_FS_MAKE_BLOCK, 0);
    }

    #[test]
    fn abi_below_four_cannot_enforce_network_denial() {
        let read_only = SandboxPolicy::read_only();
        let workspace = SandboxPolicy::workspace_write("/tmp");
        let workspace_with_net = workspace.clone().with_network_access(true);

        // ABI<3：连 TRUNCATE 都没有，任何策略都不能启用。
        assert!(!abi_can_enforce(&read_only, 1));
        assert!(!abi_can_enforce(&read_only, 2));
        // ABI 3：文件系统可兑现，但禁止网络需要 ABI 4。
        assert!(!abi_can_enforce(&read_only, 3));
        assert!(abi_can_enforce(&workspace_with_net, 3));
        assert!(abi_can_enforce(&read_only, 4));
        assert!(abi_can_enforce(&workspace, 4));
        // DangerFullAccess 不需要 Landlock。
        assert!(abi_can_enforce(&SandboxPolicy::danger_full_access(), 1));
    }
}
