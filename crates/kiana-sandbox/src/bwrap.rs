//! bubblewrap 后端：参数构造与可用性探测。

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant};

use crate::error::SandboxError;
use crate::policy::SandboxPolicy;

const BWRAP_NAME: &str = "bwrap";
const PROBE_TIMEOUT: Duration = Duration::from_millis(500);
const PROBE_POLL_INTERVAL: Duration = Duration::from_millis(10);
/// 探针失败后的重试间隔（成功结果永久缓存）。
const PROBE_RETRY_AFTER: Duration = Duration::from_secs(30);

/// 找到并缓存一个真正能创建用户命名空间的 `bwrap`。
///
/// 成功结果永久缓存。失败结果只缓存 [`PROBE_RETRY_AFTER`]：一次瞬时失败（机器负载、
/// 探针超时）不会把 bubblewrap 在进程生命周期内判死，同时也不会在每次构造 manager
/// 时重复付 500ms 的探测代价。`KIANA_BWRAP` 指定的路径优先，但仍要经过同一探针
/// （避免指向一个不能用的包装脚本）。
pub(crate) fn working_bwrap() -> Option<PathBuf> {
    struct ProbeCache {
        checked_at: Instant,
        path: Option<PathBuf>,
    }
    static CACHE: Mutex<Option<ProbeCache>> = Mutex::new(None);

    {
        let guard = lock(&CACHE);
        if let Some(cache) = guard.as_ref()
            && (cache.path.is_some() || cache.checked_at.elapsed() < PROBE_RETRY_AFTER)
        {
            return cache.path.clone();
        }
    }

    let found = candidates().into_iter().find(|path| probe(path));
    *lock(&CACHE) = Some(ProbeCache {
        checked_at: Instant::now(),
        path: found.clone(),
    });
    found
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// 构造 `bwrap` 的完整参数（不包含 `bwrap` 自身）。
///
/// 顺序遵循 Codex 的挂载模型：
/// 1. 先 `--ro-bind / /`，让整个文件系统只读；
/// 2. `--dev /dev` / `--proc /proc` 提供最小设备与进程视图；
/// 3. `/tmp` 用 tmpfs 覆盖（除非策略显式把 `/tmp` 当可写根/只读子路径）；
/// 4. 逐个 `--bind` 可写根，再逐个 `--ro-bind` 保护子路径（后绑定的规则胜出）；
/// 5. 命名空间：user / pid / ipc，按需 net；
/// 6. `--chdir` 到策略工作目录，`--cap-drop ALL`，`--` 分隔原始命令。
pub(crate) fn build_args(
    policy: &SandboxPolicy,
    cwd: &Path,
    program: &str,
    args: &[String],
) -> Result<Vec<String>, SandboxError> {
    if program.is_empty() {
        return Err(SandboxError::EmptyCommand);
    }

    let mut output = Vec::new();
    // `--new-session` 让子进程脱离调用方的控制终端：否则沙箱里的程序仍能向用户终端
    // 写入转义序列或 TIOCSTI 注入。调用方自己的 `detached` 只影响宿主进程的会话，
    // 不能替代这一层。
    push(&mut output, "--new-session");
    push(&mut output, "--die-with-parent");
    push(&mut output, "--ro-bind");
    push(&mut output, "/");
    push(&mut output, "/");
    push(&mut output, "--dev");
    push(&mut output, "/dev");
    push(&mut output, "--proc");
    push(&mut output, "/proc");

    // tmpfs 会遮住宿主 /tmp。只有策略明确把 /tmp 或其子路径列为可写根/保护子路径时
    // 才放弃覆盖（否则后面的 bind 目标会不存在）；cwd 落在 /tmp 时在 tmpfs 之后用
    // 只读 bind 把真实目录挂回原位。
    let tmp_is_policy_visible = policy
        .writable_roots()
        .iter()
        .chain(policy.read_only_subpaths().iter())
        .any(|path| path == Path::new("/tmp") || path.starts_with("/tmp/"));
    let cwd_under_tmp = cwd == Path::new("/tmp") || cwd.starts_with("/tmp/");
    let mut tmpfs_added = false;
    if !tmp_is_policy_visible && Path::new("/tmp").is_dir() {
        push(&mut output, "--tmpfs");
        push(&mut output, "/tmp");
        tmpfs_added = true;
    }

    for root in policy.writable_roots() {
        if !root.exists() {
            // 与 Codex 一致：不存在于本机的根目录跳过，让跨平台配置可以共用一份策略。
            continue;
        }
        let root = path_string(root)?;
        push(&mut output, "--bind");
        push(&mut output, &root);
        push(&mut output, &root);
    }

    // 可写根是 `/` 时，后置的 `--bind / /` 会把私有的 `/dev`、`/proc`、`/tmp`
    // 重新盖回宿主视图：`--unshare-pid` 失效、宿主设备节点全部暴露。在只读子路径
    // 之前把它们再挂一次（否则会盖掉 `/dev`、`/proc` 下的保护路径）。
    if policy
        .writable_roots()
        .iter()
        .any(|root| root == Path::new("/"))
    {
        push(&mut output, "--dev");
        push(&mut output, "/dev");
        push(&mut output, "--proc");
        push(&mut output, "/proc");
        if tmpfs_added {
            push(&mut output, "--tmpfs");
            push(&mut output, "/tmp");
        }
    }

    for subpath in policy.read_only_subpaths() {
        let subpath = path_string(subpath)?;
        if Path::new(&subpath).exists() {
            push(&mut output, "--ro-bind");
            push(&mut output, &subpath);
            push(&mut output, &subpath);
        } else {
            // 目标还不存在时也要保护：`git init && … > .git/hooks/post-checkout`
            // 会在同一个沙箱里先创建再写入。挂一个空的只读 tmpfs 占位，既让路径
            // 存在，又阻止在其下创建任何内容。
            push(&mut output, "--tmpfs");
            push(&mut output, &subpath);
            push(&mut output, "--remount-ro");
            push(&mut output, &subpath);
        }
    }

    // cwd 被 tmpfs 遮住时用 bind 把真实目录挂回来，命令才能 `--chdir` 进去；
    // cwd 位于可写根内（典型是 `workspace_write("/")`）时必须用读写 bind，
    // 否则会把自己声明为可写的工作目录变成只读。
    if tmpfs_added && cwd_under_tmp {
        let cwd_text = path_string(cwd)?;
        let cwd_writable = policy
            .writable_roots()
            .iter()
            .any(|root| cwd == root || cwd.starts_with(root));
        push(
            &mut output,
            if cwd_writable { "--bind" } else { "--ro-bind" },
        );
        push(&mut output, &cwd_text);
        push(&mut output, &cwd_text);
    }

    push(&mut output, "--unshare-user");
    push(&mut output, "--unshare-pid");
    push(&mut output, "--unshare-ipc");
    if !policy.network_access() {
        push(&mut output, "--unshare-net");
    }

    let cwd = path_string(cwd)?;
    push(&mut output, "--chdir");
    push(&mut output, &cwd);
    push(&mut output, "--cap-drop");
    push(&mut output, "ALL");
    push(&mut output, "--");
    push(&mut output, program);
    output.extend(args.iter().cloned());
    Ok(output)
}

fn push(output: &mut Vec<String>, value: &str) {
    output.push(value.to_string());
}

fn path_string(path: &Path) -> Result<String, SandboxError> {
    path.to_str()
        .map(str::to_owned)
        .ok_or_else(|| SandboxError::NonUtf8Path(path.to_path_buf()))
}

fn candidates() -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    if let Some(explicit) = std::env::var_os("KIANA_BWRAP") {
        candidates.push(PathBuf::from(explicit));
    }
    if let Some(path) = std::env::var_os("PATH") {
        for directory in std::env::split_paths(&path) {
            candidates.push(directory.join(BWRAP_NAME));
        }
    }
    // 缓存与 spawn 用的路径必须是**绝对且已解析**的：spawn 会先 chdir 到不受信任的
    // 工作目录，相对路径（`KIANA_BWRAP=./bwrap`、PATH 里的相对项）会被解析到那里，
    // 让模型写入的同名文件在沙箱建立前以宿主权限执行。
    let mut candidates: Vec<PathBuf> = candidates
        .into_iter()
        .filter(|path| is_executable(path))
        .filter_map(|path| std::fs::canonicalize(path).ok())
        .collect();
    candidates.dedup();
    candidates
}

#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    path.metadata()
        .map(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

fn probe(path: &Path) -> bool {
    // 探针要在一个用户命名空间里真的执行一个程序。`/bin/sh` 在 NixOS / Guix /
    // 极简容器里可能不在固定路径上，所以在 PATH 里再找一次；完全找不到可执行程序时
    // 判为不可用（fail-closed），而不是用 `bwrap --version` 假装可用——那会让
    // `is_available()` 为真、实际 `spawn` 却全部失败。
    let Some((program, program_args)) = probe_program() else {
        return false;
    };

    // 不请求 `--unshare-net`：`with_network_access(true)` 的真实命令不会创建网络
    // 命名空间，探针不应因此否掉一个可用的 bwrap。
    let mut child = match Command::new(path)
        .args([
            "--die-with-parent",
            "--ro-bind",
            "/",
            "/",
            "--dev",
            "/dev",
            "--proc",
            "/proc",
            "--unshare-user",
            "--unshare-pid",
            "--unshare-ipc",
            "--cap-drop",
            "ALL",
            "--",
        ])
        .arg(&program)
        .args(program_args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(child) => child,
        Err(_) => return false,
    };

    let deadline = Instant::now() + PROBE_TIMEOUT;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return status.success(),
            Ok(None) if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return false;
            }
            Ok(None) => std::thread::sleep(PROBE_POLL_INTERVAL),
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                return false;
            }
        }
    }
}

fn probe_program() -> Option<(PathBuf, &'static [&'static str])> {
    let mut shells = vec![PathBuf::from("/bin/sh"), PathBuf::from("/usr/bin/sh")];
    if let Some(path) = std::env::var_os("PATH") {
        shells.extend(std::env::split_paths(&path).map(|directory| directory.join("sh")));
    }
    if let Some(shell) = shells.into_iter().find(|path| is_executable(path)) {
        return Some((shell, &["-c", "true"]));
    }
    ["/bin/true", "/usr/bin/true"]
        .into_iter()
        .map(PathBuf::from)
        .find(|path| is_executable(path))
        .map(|program| (program, &[][..]))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args_for(policy: &SandboxPolicy, cwd: &Path) -> Vec<String> {
        build_args(
            policy,
            cwd,
            "/bin/sh",
            &["-c".to_string(), "true".to_string()],
        )
        .expect("build args")
    }

    #[test]
    fn read_only_mounts_root_and_isolates_network() {
        let args = args_for(&SandboxPolicy::read_only(), Path::new("/workspace"));
        assert!(args.contains(&"--new-session".to_string()));
        assert!(args.windows(3).any(|w| w == ["--ro-bind", "/", "/"]));
        assert!(args.windows(2).any(|w| w == ["--tmpfs", "/tmp"]));
        assert!(args.contains(&"--unshare-net".to_string()));
        assert!(args.windows(2).any(|w| w == ["--chdir", "/workspace"]));
        assert!(args.windows(2).any(|w| w == ["--", "/bin/sh"]));
        assert!(args.ends_with(&["-c".to_string(), "true".to_string()]));
    }

    #[test]
    fn read_only_with_network_skips_net_namespace() {
        let args = args_for(&SandboxPolicy::read_only_with_network(), Path::new("/"));
        assert!(!args.contains(&"--unshare-net".to_string()));
    }

    #[test]
    fn workspace_write_binds_root_then_protects_subpaths() {
        let root = tempfile::TempDir::new().expect("tempdir");
        let git = root.path().join(".git");

        // 策略先构造、.git 后创建：build_args 在 spawn 时重新检查存在性。
        let policy = SandboxPolicy::workspace_write(root.path()).with_network_access(true);
        std::fs::create_dir(&git).expect("mkdir .git");
        let args = args_for(&policy, root.path());
        let root_text = root.path().to_string_lossy().into_owned();

        let bind_at = args
            .windows(3)
            .position(|w| w == ["--bind", root_text.as_str(), root_text.as_str()])
            .expect("root bind");
        let protect_at = args
            .windows(3)
            .position(|w| w == ["--ro-bind", git.to_str().unwrap(), git.to_str().unwrap()])
            .expect("protected subpath");
        assert!(bind_at < protect_at, "保护子路径必须后绑定");
        assert!(!args.contains(&"--unshare-net".to_string()));
    }

    #[test]
    fn workspace_write_protects_subpaths_that_do_not_exist_yet() {
        let root = tempfile::TempDir::new().expect("tempdir");
        let policy = SandboxPolicy::workspace_write(root.path()).with_network_access(true);
        let git = root.path().join(".git");
        let args = args_for(&policy, root.path());
        let git_text = git.to_str().unwrap();
        assert!(
            args.windows(4)
                .any(|w| w == ["--tmpfs", git_text, "--remount-ro", git_text]),
            "不存在的 .git 也要用只读 tmpfs 占位：{args:?}"
        );
    }

    #[test]
    fn filesystem_root_write_keeps_private_dev_proc_and_tmp() {
        // workspace_write("/") 会 `--bind / /`，把前面挂好的私有 /dev、/proc、/tmp
        // 全覆盖掉；必须在 bind 之后再绑一次。
        let args = args_for(
            &SandboxPolicy::workspace_write("/"),
            Path::new("/workspace"),
        );
        let bind_root = args
            .windows(3)
            .rposition(|w| w == ["--bind", "/", "/"])
            .expect("root bind");
        let dev = args
            .windows(2)
            .rposition(|w| w == ["--dev", "/dev"])
            .expect("dev mount");
        let proc_mount = args
            .windows(2)
            .rposition(|w| w == ["--proc", "/proc"])
            .expect("proc mount");
        assert!(dev > bind_root, "/dev 必须在根 bind 之后重新挂载");
        assert!(proc_mount > bind_root, "/proc 必须在根 bind 之后重新挂载");
        if Path::new("/tmp").is_dir() {
            let tmpfs = args
                .windows(2)
                .rposition(|w| w == ["--tmpfs", "/tmp"])
                .expect("tmpfs");
            assert!(tmpfs > bind_root, "/tmp 必须在根 bind 之后重新挂载");
        }
    }

    #[test]
    fn read_only_with_tmp_cwd_keeps_private_tmp_and_a_visible_cwd() {
        let args = args_for(&SandboxPolicy::read_only(), Path::new("/tmp/project"));
        assert!(
            args.windows(2).any(|w| w == ["--tmpfs", "/tmp"]),
            "read-only 仍应有私有 /tmp"
        );
        assert!(
            args.windows(3)
                .any(|w| w == ["--ro-bind", "/tmp/project", "/tmp/project"]),
            "被 tmpfs 遮住的 cwd 必须只读挂回"
        );
    }

    #[test]
    fn writable_policy_keeps_tmp_cwd_writable() {
        // workspace_write("/") 把整个文件系统声明为可写；cwd 在 /tmp 下时不能退化成
        // 只读 bind，否则命令连自己的工作目录都写不了。
        let args = args_for(
            &SandboxPolicy::workspace_write("/"),
            Path::new("/tmp/project"),
        );
        assert!(
            args.windows(3)
                .any(|w| w == ["--bind", "/tmp/project", "/tmp/project"]),
            "可写根内的 /tmp cwd 必须以读写方式挂回"
        );
    }

    #[test]
    fn non_utf8_cwd_is_rejected() {
        #[cfg(unix)]
        {
            use std::ffi::OsString;
            use std::os::unix::ffi::OsStringExt;
            let path = PathBuf::from(OsString::from_vec(vec![0xff]));
            let error = build_args(&SandboxPolicy::read_only(), &path, "/bin/sh", &[])
                .expect_err("non-utf8");
            assert!(matches!(error, SandboxError::NonUtf8Path(_)));
        }
    }
}
