//! evals/src/docker.ts。
//!
//! 构建两个评测镜像、生成 `docker run` 的参数、发现用例、跑单个 arm。
//!
//! 当前设计约束（改动需同步测试）：
//! - `packageRoot` = 本 crate 的目录（TS 的 `import.meta.url` 的 `..`），`repositoryRoot` =
//!   它的 `../..`；两者都做**词法**归一（不解析符号链接）；
//! - docker 参数顺序、`--mount` 的 `source=`/`target=` 拼写、`--env` 与 `--file` 的长选项
//!   保持原样；
//! - 子进程的 stdout/stderr 继承行为与 `spawnSync` 一致：capture 时 stdin 为 null、stdout 管道、
//!   stderr 继承；否则全部继承；被信号杀死时 status 记为 1；启动失败按 Node 的
//!   `spawnSync <command> <ERRNO>` 文本报错，并在 `ENOEXEC` 时复刻 `execvp` 的 `/bin/sh` 回退；
//! - 文件系统错误按 Node 的 `SystemError` 文本报错（`{CODE}: {strerror}, {syscall} '<path>'`）；
//! - 所有错误文本是当前稳定契约（改动需同步测试；见各函数文档）。
//!
//! 构建镜像时使用 TypeScript monorepo 根（`KIANA_EVAL_TYPESCRIPT_REPOSITORY`；未设置时取
//! `package_root()/../../kiana`）作为 build context。构建上下文不是 [`repository_root`]
//! （本移植里它是 Rust 工作区根），而是 TS 仓库根。Cargo 工作区没有 Dockerfile 所需的 npm
//! lockfile 与 eval runtime，不能作为 context。启动 Docker 前会验证必需输入，并给出可操作错误。
//!
//! # 在流水线中的位置
//! `cli::run_with` 的主要调用顺序如下，任何一步返回错误都会让整个评测提前终止：
//! 1. [`require_eval_auth_file`]（或可注入环境的 `require_eval_auth_file_with`）校验凭据；
//! 2. [`build_images`] 构建两个文档变体镜像；
//! 3. [`create_docker_context`] 把镜像、产物目录、凭据路径、provider/model 与
//!    `runs_per_variant` 组装成 [`DockerContext`]；
//! 4. 对每个变体调用 [`discover_cases`]，由 `cli` 读取并解析 `discovered-tests.json`；
//! 5. 对任务计划里的每个 arm 调用 [`run_task`]，报告文件由 `report` 模块读取。
//! 此外 `cli` 还复用本模块的 `js_resolve`/`js_relative`/`path_text` 处理路径，并用
//! `create_dir_all_mode` 创建 `.eval/<runId>` 目录。
//!
//! 本模块写入的产物路径（`artifact_directory` 是 `.eval/<runId>` 的绝对路径）：
//! `discovery/<variant>/discovered-tests.json` 与 `tasks/<64 位十六进制任务身份>/vitest.json`。
//!
//! # 执行模型
//! 本模块所有函数都是同步的，内部用 `std::process::Command::output` 等待 docker 退出；
//! `cli::run_with` 虽然是 async 函数，调用这些函数时仍会阻塞当前异步执行线程。本模块不提供
//! 超时、取消或异步接口，也没有内部锁；并发调用会各自启动独立的 docker 进程。
//!
//! # 设计原因
//! TS 源全部使用 `spawnSync` 与 `mkdirSync`：参数顺序、退出码处理、错误文本和目录创建时机
//! 都是可观察行为。移植时保持同步调用与相同顺序，避免另起一套错误路径；评测产物
//! `protocol.json` 的 sha256 也依赖这里生成的镜像名与 id。
//!
//! # 约束与失败模式
//! - `docker` 从 `PATH` 查找；ENOEXEC 回退用绝对路径 `/bin/sh`，不经 `PATH` 查找。启动失败统一报
//!   `spawnSync <command> <ERRNO>`；退出码由各调用点解释（构建是
//!   `<command> exited with status <N>.`，发现是 `<variant> eval discovery failed.`，
//!   `run_task` 直接忽略）；
//! - 镜像标签中的哈希取自**构建上下文路径文本**，换一个 TS 仓库路径就得到另一组本地标签；
//! - 路径全部按词法处理，不解析符号链接；容器内看到什么由 `--mount` 的
//!   `source=`/`target=` 与 `--env` 决定。`tests/docker.rs` 会比对构建与 run 的关键
//!   argv，本文件的 `docker_args_match_the_spec_vector` 比对完整向量；修改参数顺序会破坏
//!   该契约。
//!
//! 本模块的错误经 `cli::CliError::Docker`（`#[from]`）传播到 `main`，最终打印到 stderr
//! 并以退出码 1 结束（`main` 的实现见 `main.rs`）。

use std::ffi::{OsStr, OsString};
#[cfg(unix)]
use std::os::unix::fs::DirBuilderExt as _;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};

use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

use kiana_ai::auth::oauth::js_trim;

use crate::plan::{DOCUMENTATION_VARIANTS, DocumentationVariant, EvalTask};

/// `packageRoot`：TS `resolve(dirname(fileURLToPath(import.meta.url)), "..")`。
///
/// Rust 侧没有模块 URL，等价物是 `CARGO_MANIFEST_DIR`（编译期绝对路径）。
///
/// 除派生 `repository_root` 与 `typescript_repository_root` 外，还有两处直接使用：
/// `build_images` 用它拼出 `docker/Dockerfile` 的 `--file` 路径，
/// `spawn_process` 用它作为 docker 子进程的 cwd（对应 TS `spawnSync` 的 `cwd: packageRoot`）。
/// 因此运行时该目录必须存在；把编译好的二进制搬到另一台机器不会自动找到本机的 crate。
pub fn package_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// `repositoryRoot = resolve(packageRoot, "../..")`（词法解析）。
///
/// 对应 TS 的 `repositoryRoot`；本 crate 当前没有调用点，`build_images` 的构建上下文用的是
/// [`typescript_repository_root`]。
pub fn repository_root() -> PathBuf {
    lexical_normalize(&package_root().join("..").join(".."))
}

/// TypeScript eval runtime's monorepo root.
///
/// The Rust workspace is not a substitute: the supplied Dockerfile executes
/// `npm ci`, `build:offline`, and `packages/evals/docker/install-runtime.mjs`.
/// `KIANA_EVAL_TYPESCRIPT_REPOSITORY` selects an already provisioned checkout;
/// the default is the sibling `kiana` checkout used by the port's workspace.
///
/// 精确的默认值是 `lexical_normalize(package_root().join("../..").join("kiana"))`，即
/// `<Rust 工作区根>/kiana`。本函数只解析环境变量、不验证目录；校验发生在 [`build_images`]
/// 调用的 `validate_typescript_repository` 里。
pub fn typescript_repository_root() -> PathBuf {
    std::env::var_os("KIANA_EVAL_TYPESCRIPT_REPOSITORY")
        .map(PathBuf::from)
        .unwrap_or_else(|| lexical_normalize(&package_root().join("../..").join("kiana")))
}

/// 校验构建上下文是否具备 Dockerfile 需要的输入。
///
/// 只做 `is_file` 存在性检查（会跟随符号链接），不检查文件内容；缺失时把**第一个**缺失的
/// 相对路径写进错误，并提示用 `KIANA_EVAL_TYPESCRIPT_REPOSITORY` 指向可用的 TS monorepo。
/// 这个检查在启动任何 docker 进程之前执行。
fn validate_typescript_repository(root: &Path) -> Result<(), DockerError> {
    const REQUIRED: &[&str] = &[
        "package.json",
        "package-lock.json",
        "packages/evals/package.json",
        "packages/evals/docker/entrypoint.ts",
        "packages/evals/docker/install-runtime.mjs",
        "packages/coding-agent/package.json",
    ];
    if let Some(missing) = REQUIRED
        .iter()
        .find(|relative| !root.join(relative).is_file())
    {
        return Err(DockerError::message(format!(
            "TypeScript eval repository is invalid: missing {} in {}. Set KIANA_EVAL_TYPESCRIPT_REPOSITORY to a provisioned kiana monorepo; the Rust workspace is not a Docker build context.",
            missing,
            path_text(root),
        )));
    }
    Ok(())
}

/// `crypto.createHash("sha256").update(text).digest("hex")`。
pub(crate) fn sha256_hex(text: &str) -> String {
    hex::encode(Sha256::digest(text.as_bytes()))
}

/// 一个镜像的名字与 inspect 得到的 id。
///
/// TS: `docker.ts#BuiltImages` 的 `Record` 值类型 `{ name: string; id: string }`
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BuiltImage {
    /// `docker build --tag` 的名字。
    pub name: String,
    /// `docker image inspect --format "{{.Id}}"` 的输出（trim 后）。
    ///
    /// 只用于记录与比较（`cli` 据此拒绝两个变体解析到同一镜像）；`docker run` 用的是 `name`。
    pub id: String,
}

/// 两个文档变体的镜像。
///
/// TS: `docker.ts#BuiltImages`（`Record<DocumentationVariant, { name; id }>`；键顺序
/// `without_docs`、`with_docs`）
///
/// 变体名（`without_docs` / `with_docs`）同时用作 Dockerfile 的 `--target` 和产物子目录名。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BuiltImages {
    /// 不提供文档的镜像。
    pub without_docs: BuiltImage,
    /// 提供文档的镜像。
    pub with_docs: BuiltImage,
}

impl BuiltImages {
    /// 按变体取镜像。
    pub fn get(&self, variant: DocumentationVariant) -> &BuiltImage {
        match variant {
            DocumentationVariant::WithoutDocs => &self.without_docs,
            DocumentationVariant::WithDocs => &self.with_docs,
        }
    }

    /// 按变体取可变镜像。
    pub fn get_mut(&mut self, variant: DocumentationVariant) -> &mut BuiltImage {
        match variant {
            DocumentationVariant::WithoutDocs => &mut self.without_docs,
            DocumentationVariant::WithDocs => &mut self.with_docs,
        }
    }
}

/// docker.ts 的错误。`Display` 等于 TS 的 `error.message`。
#[derive(Debug, thiserror::Error)]
pub enum DockerError {
    /// 与 TS `new Error(message)` 对应的文本。
    #[error("{0}")]
    Message(String),
    /// 子进程启动失败或文件系统操作失败（TS 里是抛出的 `Error`/`SystemError`）。
    #[error("{0}")]
    Io(#[from] std::io::Error),
    /// `auth.json` 读取或解析失败；TS 用 `new Error(message, { cause })` 保留原始错误
    /// （`readFileSync` 与 `JSON.parse` 在同一个 `try` 里，两者都归入 invalid）。
    #[error("Eval authentication file is invalid: {path}")]
    InvalidAuthFile {
        /// `auth.json` 的绝对路径。
        path: String,
        /// 原始读取/解析错误。
        #[source]
        source: AuthFileCause,
    },
}

/// [`DockerError::InvalidAuthFile`] 的 `cause`。
#[derive(Debug, thiserror::Error)]
pub enum AuthFileCause {
    /// `readFileSync` 失败（Node 的 `SystemError`）。
    #[error("{0}")]
    Io(#[from] std::io::Error),
    /// `JSON.parse` 失败。
    #[error("{0}")]
    Json(#[from] kiana_json::ParseError),
}

impl DockerError {
    fn message(text: impl Into<String>) -> Self {
        DockerError::Message(text.into())
    }
}

/// 一次子进程执行的结果。
#[derive(Debug)]
struct ExecOutput {
    status: i32,
    stdout: String,
}

/// 按 `spawnSync` 的 stdio 约定启动子进程：capture 时 stdin 为 null、stdout 管道、
/// stderr 继承；否则三者全部继承。
fn spawn_process(
    command: &str,
    args: &[String],
    capture: bool,
) -> std::io::Result<std::process::Output> {
    let mut process = Command::new(command);
    process.args(args).current_dir(package_root());
    if capture {
        process
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit());
    } else {
        process
            .stdin(Stdio::inherit())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit());
    }
    process.output()
}

/// `spawnSync(command, args, { cwd: packageRoot, stdio, encoding: "utf8" })`。
///
/// capture 为真时：stdin 为 null、stdout 管道、stderr 继承；否则三者全部继承。
/// `result.error` → `Err`；`result.status ?? 1`（被信号杀死时为 1）。
///
/// 这是阻塞调用：函数返回时子进程已经退出。stderr 在两种模式下都直接继承到父进程，
/// 不会进入返回值；capture 模式下的 stdout 用有损 UTF-8 解码，非 UTF-8 字节变成替换字符。
fn execute(command: &str, args: &[String], capture: bool) -> Result<ExecOutput, DockerError> {
    let output = match spawn_process(command, args, capture) {
        Ok(output) => output,
        // Node/libuv 的 `execvp` 在 ENOEXEC 时回退成 `/bin/sh <文件> <参数...>`；
        // Rust 的 `Command` 直接返回该错误，这里补齐同样的回退。
        #[cfg(unix)]
        Err(error) if error.raw_os_error() == Some(libc::ENOEXEC) => {
            let program = resolve_program_path(command).unwrap_or_else(|| PathBuf::from(command));
            let mut shell_args: Vec<String> = Vec::with_capacity(args.len() + 1);
            shell_args.push(path_text(&program));
            shell_args.extend(args.iter().cloned());
            spawn_process("/bin/sh", &shell_args, capture)
                .map_err(|error| spawn_error("/bin/sh", &error))?
        }
        Err(error) => return Err(spawn_error(command, &error)),
    };
    let status = output.status.code().unwrap_or(1);
    let stdout = if capture {
        String::from_utf8_lossy(&output.stdout).into_owned()
    } else {
        String::new()
    };
    Ok(ExecOutput { status, stdout })
}

/// `result.error.message`：`spawnSync <command> <ERRNO>`（`ERRNO` 用 libuv 的 `uv_err_name`）。
fn spawn_error(command: &str, error: &std::io::Error) -> DockerError {
    DockerError::message(format!("spawnSync {command} {}", uv_error_name(error)))
}

/// `execvp` 的 PATH 查找（只在 ENOEXEC 回退里用；含 `/` 时按原样使用）。
#[cfg(unix)]
fn resolve_program_path(command: &str) -> Option<PathBuf> {
    if command.contains('/') {
        return Some(PathBuf::from(command));
    }
    let paths = std::env::var_os("PATH")?;
    for directory in std::env::split_paths(&paths) {
        let candidate = if directory.as_os_str().is_empty() {
            PathBuf::from(command)
        } else {
            directory.join(command)
        };
        let Ok(metadata) = std::fs::metadata(&candidate) else {
            continue;
        };
        // `execvp` 只会停在可执行的文件上；其它候选继续搜索。
        use std::os::unix::fs::PermissionsExt as _;
        if metadata.is_file() && metadata.permissions().mode() & 0o111 != 0 {
            return Some(candidate);
        }
    }
    None
}

/// libuv 的 `uv_err_name`（`spawnSync` 错误里的 `ERRNO`）。
#[cfg(unix)]
fn uv_error_name(error: &std::io::Error) -> &'static str {
    let Some(code) = error.raw_os_error() else {
        return "UNKNOWN";
    };
    match code {
        libc::EPERM => "EPERM",
        libc::ENOENT => "ENOENT",
        libc::ESRCH => "ESRCH",
        libc::EINTR => "EINTR",
        libc::EIO => "EIO",
        libc::ENXIO => "ENXIO",
        libc::E2BIG => "E2BIG",
        libc::ENOEXEC => "ENOEXEC",
        libc::EBADF => "EBADF",
        libc::ECHILD => "ECHILD",
        libc::EAGAIN => "EAGAIN",
        libc::ENOMEM => "ENOMEM",
        libc::EACCES => "EACCES",
        libc::EFAULT => "EFAULT",
        libc::EBUSY => "EBUSY",
        libc::EEXIST => "EEXIST",
        libc::EXDEV => "EXDEV",
        libc::ENODEV => "ENODEV",
        libc::ENOTDIR => "ENOTDIR",
        libc::EISDIR => "EISDIR",
        libc::EINVAL => "EINVAL",
        libc::ENFILE => "ENFILE",
        libc::EMFILE => "EMFILE",
        libc::ENOTTY => "ENOTTY",
        libc::ETXTBSY => "ETXTBSY",
        libc::EFBIG => "EFBIG",
        libc::ENOSPC => "ENOSPC",
        libc::ESPIPE => "ESPIPE",
        libc::EROFS => "EROFS",
        libc::EMLINK => "EMLINK",
        libc::EPIPE => "EPIPE",
        libc::EDOM => "EDOM",
        libc::ERANGE => "ERANGE",
        libc::ENAMETOOLONG => "ENAMETOOLONG",
        libc::ELOOP => "ELOOP",
        libc::ENOSYS => "ENOSYS",
        libc::EOVERFLOW => "EOVERFLOW",
        libc::ENOTEMPTY => "ENOTEMPTY",
        _ => "UNKNOWN",
    }
}

#[cfg(not(unix))]
fn uv_error_name(error: &std::io::Error) -> &'static str {
    match error.kind() {
        std::io::ErrorKind::NotFound => "ENOENT",
        std::io::ErrorKind::PermissionDenied => "EACCES",
        std::io::ErrorKind::AlreadyExists => "EEXIST",
        std::io::ErrorKind::NotADirectory => "ENOTDIR",
        std::io::ErrorKind::InvalidInput => "EINVAL",
        _ => "UNKNOWN",
    }
}

/// libuv 的 `uv_strerror` 文本（Node `SystemError.message` 里逗号前的那段）。
#[cfg(unix)]
fn errno_description(error: &std::io::Error) -> &'static str {
    let Some(code) = error.raw_os_error() else {
        return "unknown error";
    };
    match code {
        libc::EPERM => "operation not permitted",
        libc::ENOENT => "no such file or directory",
        libc::ESRCH => "no such process",
        libc::EINTR => "interrupted system call",
        libc::EIO => "i/o error",
        libc::ENXIO => "no such device or address",
        libc::E2BIG => "argument list too long",
        libc::ENOEXEC => "exec format error",
        libc::EBADF => "bad file descriptor",
        libc::ECHILD => "no child processes",
        libc::EAGAIN => "resource temporarily unavailable",
        libc::ENOMEM => "not enough memory",
        libc::EACCES => "permission denied",
        libc::EFAULT => "bad address in system call argument",
        libc::EBUSY => "resource busy or locked",
        libc::EEXIST => "file already exists",
        libc::EXDEV => "cross-device link not permitted",
        libc::ENODEV => "no such device",
        libc::ENOTDIR => "not a directory",
        libc::EISDIR => "illegal operation on a directory",
        libc::EINVAL => "invalid argument",
        libc::ENFILE => "file table overflow",
        libc::EMFILE => "too many open files",
        libc::ENOTTY => "inappropriate ioctl for device",
        libc::ETXTBSY => "text file is busy",
        libc::EFBIG => "file too large",
        libc::ENOSPC => "no space left on device",
        libc::ESPIPE => "invalid seek",
        libc::EROFS => "read-only file system",
        libc::EMLINK => "too many links",
        libc::EPIPE => "broken pipe",
        libc::EDOM => "argument out of domain",
        libc::ERANGE => "result too large",
        libc::ENAMETOOLONG => "name too long",
        libc::ELOOP => "too many symbolic links encountered",
        libc::ENOSYS => "function not implemented",
        libc::EOVERFLOW => "value too large for defined data type",
        libc::ENOTEMPTY => "directory not empty",
        _ => "unknown error",
    }
}

#[cfg(not(unix))]
fn errno_description(error: &std::io::Error) -> &'static str {
    match error.kind() {
        std::io::ErrorKind::NotFound => "no such file or directory",
        std::io::ErrorKind::PermissionDenied => "permission denied",
        std::io::ErrorKind::AlreadyExists => "file already exists",
        std::io::ErrorKind::NotADirectory => "not a directory",
        std::io::ErrorKind::InvalidInput => "invalid argument",
        _ => "unknown error",
    }
}

/// Node/uv 的 `SystemError.message`：`{CODE}: {strerror}, {syscall}[ '<path>']`。
///
/// `fs.promises.readFile` 的读失败带路径（`..., read '<path>'`），写失败不带路径
/// （`..., write`，libuv 的写错误没有 path）。
pub(crate) fn node_system_error(
    syscall: &str,
    path: Option<&Path>,
    error: &std::io::Error,
) -> String {
    let mut message = format!("{}: {}", uv_error_name(error), errno_description(error));
    message.push_str(", ");
    message.push_str(syscall);
    if let Some(path) = path {
        message.push_str(" '");
        message.push_str(&path_text(path));
        message.push('\'');
    }
    message
}

/// 一次失败的文件系统操作（已格式化成 Node 的 `SystemError` 文本）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NodeFsError {
    message: String,
}

impl NodeFsError {
    fn from_io(syscall: &str, path: &Path, error: &std::io::Error) -> Self {
        NodeFsError {
            message: node_system_error(syscall, Some(path), error),
        }
    }
}

impl std::fmt::Display for NodeFsError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for NodeFsError {}

impl From<NodeFsError> for DockerError {
    fn from(error: NodeFsError) -> Self {
        DockerError::Message(error.message)
    }
}

/// `requireSuccess(command, args)`：非零退出码 → `{command} exited with status {status}.`
///
/// 以 capture = false 运行，所以构建日志直接显示在终端；只有启动失败或退出码非零才返回
/// `Err`，不解析输出。
fn require_success(command: &str, args: &[String]) -> Result<(), DockerError> {
    let result = execute(command, args, false)?;
    if result.status != 0 {
        return Err(DockerError::message(format!(
            "{command} exited with status {}.",
            result.status
        )));
    }
    Ok(())
}

/// 构建（或复用）两个文档变体镜像并返回它们的 id。
///
/// TS: `docker.ts#buildImages`
///
/// 前缀 `kiana-evals-<sha256(构建上下文路径文本) 前 12 位>`（TS 里哈希的是
/// `repositoryRoot`，本移植里是 [`typescript_repository_root`] 的返回值）；名字
/// `{prefix}-without-docs:local` / `{prefix}-with-docs:local`。
///
/// 执行顺序：
/// 1. 先校验构建上下文：`package.json`、`package-lock.json`、`packages/evals/package.json`、
///    `packages/evals/docker/entrypoint.ts`、`packages/evals/docker/install-runtime.mjs`、
///    `packages/coding-agent/package.json` 缺一不可；缺失时在启动 docker 之前失败。
/// 2. 按 `DOCUMENTATION_VARIANTS` 的顺序（`without_docs`、`with_docs`）逐个执行
///    `docker build --target <variant> --tag <name> --file <packageRoot>/docker/Dockerfile <context>`；
///    任一 variant 失败立即返回，后面的 variant 不再构建。
/// 3. 每次构建成功后用 `docker image inspect --format "{{.Id}}"`（capture）取 id；
///    退出码非零或 trim 后为空 → `Cannot inspect <name>.`。
///
/// 重复调用会再次执行 `docker build`（是否复用层缓存由 docker 自己决定，本函数不查询
/// 已有镜像）。
pub fn build_images() -> Result<BuiltImages, DockerError> {
    let build_context = typescript_repository_root();
    validate_typescript_repository(&build_context)?;
    let digest = sha256_hex(&path_text(&build_context));
    let prefix = format!("kiana-evals-{}", &digest[..12]);
    let mut images = BuiltImages {
        without_docs: BuiltImage {
            name: format!("{prefix}-without-docs:local"),
            id: String::new(),
        },
        with_docs: BuiltImage {
            name: format!("{prefix}-with-docs:local"),
            id: String::new(),
        },
    };
    for variant in DOCUMENTATION_VARIANTS {
        let name = images.get(variant).name.clone();
        require_success(
            "docker",
            &[
                "build".to_string(),
                "--target".to_string(),
                variant.as_str().to_string(),
                "--tag".to_string(),
                name.clone(),
                "--file".to_string(),
                path_text(&package_root().join("docker").join("Dockerfile")),
                path_text(&build_context),
            ],
        )?;
        let inspected = execute(
            "docker",
            &[
                "image".to_string(),
                "inspect".to_string(),
                "--format".to_string(),
                "{{.Id}}".to_string(),
                name.clone(),
            ],
            true,
        )?;
        if inspected.status != 0 || js_trim(&inspected.stdout).is_empty() {
            return Err(DockerError::message(format!("Cannot inspect {name}.")));
        }
        images.get_mut(variant).id = js_trim(&inspected.stdout).to_string();
    }
    Ok(images)
}

/// `--env name=value`（TS 的 `environment(name, value)`；注意是 `--env` 不是 `-e`）。
fn push_environment(args: &mut Vec<String>, name: &str, value: &str) {
    args.push("--env".to_string());
    args.push(format!("{name}={value}"));
}

/// `os.homedir()`。
///
/// POSIX 上 libuv 只要 `HOME` **被设置**就原样返回（空串也算已设置，结果是相对路径
/// `.kiana/agent`）；只有未设置时才回退到 passwd 条目。
///
/// Windows 上 libuv 的 `uv_os_homedir` **优先返回 `USERPROFILE`**（即使被覆盖），未设置时
/// 才走 `uv_os_get_passwd()`（`GetUserProfileDirectoryW`）；`dirs::home_dir()` 只读 Known
/// Folder，会忽略 `USERPROFILE`，因此这里单独处理。
fn process_home_directory() -> Option<OsString> {
    #[cfg(unix)]
    {
        if let Some(home) = std::env::var_os("HOME") {
            return Some(home);
        }
        dirs::home_dir().map(PathBuf::into_os_string)
    }
    #[cfg(windows)]
    {
        windows_home_directory(std::env::var_os("USERPROFILE"), dirs::home_dir)
    }
    #[cfg(not(any(unix, windows)))]
    {
        dirs::home_dir().map(PathBuf::into_os_string)
    }
}

/// Windows 的 `os.homedir()` 取值规则（独立成函数以便在任意平台上测试）。
///
/// libuv `uv_os_homedir`：`USERPROFILE` 已设置时直接返回；但长度小于 3 字节的值
/// （空串、`C:` 之类）会被当作不可用并返回 `ENOENT`（Node 的 `os.homedir()` 随之为错），
/// **不会**回退到 profile 目录。只有变量完全未设置时才回退到 `uv_os_get_passwd()`。
/// libuv 不读 `HOMEDRIVE`/`HOMEPATH`。
#[cfg(any(windows, test))]
fn windows_home_directory(
    userprofile: Option<OsString>,
    fallback: impl FnOnce() -> Option<PathBuf>,
) -> Option<OsString> {
    match userprofile {
        Some(profile) if profile.as_os_str().len() >= 3 => Some(profile),
        Some(_) => None,
        None => fallback().map(PathBuf::into_os_string),
    }
}

/// `join(process.env.KIANA_CODING_AGENT_DIR ? resolve(KIANA_CODING_AGENT_DIR) : join(homedir(), ".kiana", "agent"), "auth.json")`。
///
/// `home == None` 表示宿主没有 home 目录（`homedir()` 抛错的等价物）。`path.join` 还做
/// 词法归一（去掉 `.`/`..`/重复分隔符），所以 homedir 分支的拼接结果要过
/// [`lexical_normalize`]。
fn eval_auth_path(coding_agent_dir: Option<&str>, home: Option<&OsStr>) -> Option<PathBuf> {
    let base = match coding_agent_dir.filter(|value| !value.is_empty()) {
        Some(directory) => js_resolve_single(directory),
        None => lexical_normalize(&PathBuf::from(home?).join(".kiana").join("agent")),
    };
    Some(base.join("auth.json"))
}

/// 校验评测凭据文件存在、可解析且含有 provider 的凭据，返回它的绝对路径。
///
/// TS: `docker.ts#requireEvalAuthFile`
///
/// 路径 = `(KIANA_CODING_AGENT_DIR ? resolve(...) : join(homedir(), ".kiana", "agent"))/auth.json`。
///
/// 判定顺序（每一步失败都直接返回；错误文本是当前稳定契约）：
/// 1. 无法确定 home 目录 → `Cannot determine the home directory.`；
/// 2. 路径不存在，或存在但不是普通文件（例如目录、悬空符号链接）→
///    `Eval authentication file does not exist: <path>`；
/// 3. 读取或 JSON 解析失败 → `Eval authentication file is invalid: <path>`；
/// 4. 顶层不是对象，或没有 `provider` 这个键 →
///    `Eval authentication file has no credential for provider <id>.`
///
/// 只要求 provider 键存在，不校验凭据值本身。返回的路径随后被放进
/// [`DockerContext::auth_path`]，以只读 bind mount 挂到容器内。
pub fn require_eval_auth_file(provider: &str) -> Result<PathBuf, DockerError> {
    let coding_agent_dir = std::env::var_os("KIANA_CODING_AGENT_DIR")
        .map(|value| value.to_string_lossy().into_owned());
    require_eval_auth_file_with(provider, coding_agent_dir.as_deref())
}

/// [`require_eval_auth_file`] 的可注入环境版本（`KIANA_CODING_AGENT_DIR` 由调用方提供）。
///
/// `coding_agent_dir` 为空字符串时按未设置处理（JS 的 falsy 语义），回退到 homedir；
/// 相对路径按进程 cwd 解析。`cli` 传入的是从可注入环境查到的值，测试因此不必改进程全局
/// 环境即可覆盖两条分支。
pub(crate) fn require_eval_auth_file_with(
    provider: &str,
    coding_agent_dir: Option<&str>,
) -> Result<PathBuf, DockerError> {
    let home = process_home_directory();
    let path = eval_auth_path(coding_agent_dir, home.as_deref())
        .ok_or_else(|| DockerError::message("Cannot determine the home directory."))?;
    let display_path = path_text(&path);
    let metadata = std::fs::metadata(&path).map_err(|_| {
        DockerError::message(format!(
            "Eval authentication file does not exist: {display_path}"
        ))
    })?;
    if !metadata.is_file() {
        return Err(DockerError::message(format!(
            "Eval authentication file does not exist: {display_path}"
        )));
    }
    // TS 把 `readFileSync` 与 `JSON.parse` 放在同一个 `try` 里：任何读取失败同样是
    // `Eval authentication file is invalid`。解析必须用 V8 语义（孤立代理项转义、数值溢出），
    // 因此走 `kiana_json::parse` 而不是 `serde_json`。
    let bytes = std::fs::read(&path).map_err(|error| DockerError::InvalidAuthFile {
        path: display_path.clone(),
        source: AuthFileCause::Io(error),
    })?;
    let text = String::from_utf8_lossy(&bytes);
    let credentials: serde_json::Value =
        kiana_json::parse(&text).map_err(|source| DockerError::InvalidAuthFile {
            path: display_path.clone(),
            source: AuthFileCause::Json(source),
        })?;
    let has_provider = match &credentials {
        serde_json::Value::Object(map) => map.contains_key(provider),
        _ => false,
    };
    if !has_provider {
        return Err(DockerError::message(format!(
            "Eval authentication file has no credential for provider {provider}."
        )));
    }
    Ok(path)
}

/// 生成 `docker run` 的完整参数，并先创建产物目录（递归，权限 0o700）。
///
/// TS: `docker.ts#dockerArgs`（内部函数）
///
/// 参数顺序是冻结契约（本文件的 `docker_args_match_the_spec_vector` 测试逐项比对，
/// spec §2.5.3 也固定了该顺序）：`run --rm --read-only`、两个 tmpfs、产物 bind mount、
/// 一组 `--env`、凭据只读 bind mount，最后是镜像名与 entrypoint 参数。`--mount` 必须用
/// `source=`/`target=` 长拼写，不能用 `src=`/`dst=`。`--read-only` 使容器根文件系统只读，
/// 可写位置只有两个 tmpfs 与 bind mount 进来的 `/artifacts`。
///
/// 副作用：先执行 `mkdirSync(outputDirectory, {recursive: true, mode: 0o700})`；失败时以
/// Node 的 `SystemError` 文本返回（如 `EACCES`、`ENOTDIR`），并且不会启动 docker。该目录被
/// 挂载到容器 `/artifacts`，entrypoint 写出的 `discovered-tests.json` / `vitest.json`
/// 因此直接落回宿主的输出目录。
fn docker_args(
    context: &DockerContext,
    variant: DocumentationVariant,
    output_directory: &Path,
    entrypoint_args: &[String],
) -> Result<Vec<String>, DockerError> {
    // TS: `mkdirSync(outputDirectory, { recursive: true, mode: 0o700 })`（同步语义）。
    create_dir_all_mode_sync(output_directory, 0o700)?;
    let output_text = path_text(output_directory);
    let mut args: Vec<String> = vec![
        "run".to_string(),
        "--rm".to_string(),
        "--read-only".to_string(),
        "--tmpfs".to_string(),
        "/tmp:rw,exec,mode=1777".to_string(),
        "--tmpfs".to_string(),
        "/repo/node_modules/.vite-temp:rw,exec,mode=1777".to_string(),
        "--mount".to_string(),
        format!("type=bind,source={output_text},target=/artifacts"),
    ];
    push_environment(&mut args, "KIANA_EVAL_ARTIFACT_DIR", "/artifacts");
    // TS 把 number 直接插进模板字符串；用 JS 的数字格式化保持文本一致（测试里 2.0 → "2"）。
    push_environment(
        &mut args,
        "KIANA_EVAL_RUNS_PER_VARIANT",
        &crate::report::js_number::format_js_number(context.runs_per_variant),
    );
    push_environment(&mut args, "KIANA_EVAL_SANDBOX_UID", "65532");
    push_environment(&mut args, "KIANA_EVAL_SANDBOX_GID", "65532");
    push_environment(&mut args, "KIANA_PROVIDER", &context.provider);
    push_environment(&mut args, "KIANA_MODEL", &context.model);
    #[cfg(unix)]
    {
        // Node 只在 `process.getuid`/`getgid` 存在时才传这两个 env；unix 上它们恒存在。
        // 容器内 entrypoint 用它们把 /artifacts 的属主改回宿主用户。
        // SAFETY: `getuid`/`getgid` 没有前置条件，也不会失败（POSIX 保证）。
        let (uid, gid) = unsafe { (libc::getuid(), libc::getgid()) };
        push_environment(&mut args, "KIANA_EVAL_ARTIFACT_UID", &uid.to_string());
        push_environment(&mut args, "KIANA_EVAL_ARTIFACT_GID", &gid.to_string());
    }
    args.push("--mount".to_string());
    args.push(format!(
        "type=bind,source={},target=/run/kiana-eval-secrets/auth.json,readonly",
        path_text(&context.auth_path)
    ));
    args.push(context.images.get(variant).name.clone());
    args.extend(entrypoint_args.iter().cloned());
    Ok(args)
}

/// 递归 mkdir 的两种 Node 语义。
#[derive(Clone, Copy, PartialEq, Eq)]
enum MkdirFlavor {
    /// `fs.promises.mkdir`（cli.ts）：错误路径是失败的组件；祖先上的 `EEXIST` 若 stat
    /// 到的不是目录（含悬空符号链接导致的 stat 失败）一律改报 `ENOTDIR`
    /// （`node_file.cc` 的 `MKDirpAsync`）。
    Async,
    /// `mkdirSync`（docker.ts）：错误路径始终是原始请求路径；stat 失败时原样返回
    /// stat 的错误码（`MKDirpSync`）。
    Sync,
}

/// `fs.promises.mkdir(path, { recursive: true, mode })`（cli.ts 用的异步版本）。
///
/// JS 的 mkdirp 逐层 `mkdir`，错误路径是最先失败的 syscall 路径。
pub(crate) fn create_dir_all_mode(path: &Path, mode: u32) -> Result<(), NodeFsError> {
    create_dir_all_inner(path, mode, MkdirFlavor::Async, true)
        .map_err(|(failed, error)| NodeFsError::from_io("mkdir", &failed, &error))
}

/// `mkdirSync(path, { recursive: true, mode })`（docker.ts 的 `dockerArgs` 用的同步版本）。
///
/// 同步实现走 native binding，`ctx.path` 始终是最初请求的路径：递归中更深的失败也用
/// **原路径**报错（错误码仍来自失败的 syscall）。
pub(crate) fn create_dir_all_mode_sync(path: &Path, mode: u32) -> Result<(), NodeFsError> {
    create_dir_all_inner(path, mode, MkdirFlavor::Sync, true)
        .map_err(|(_, error)| NodeFsError::from_io("mkdir", path, &error))
}

/// `ENOTDIR` 的 `SystemError` 原材料（Windows 没有 `libc::ENOTDIR`）。
fn not_a_directory_error() -> std::io::Error {
    #[cfg(unix)]
    {
        std::io::Error::from_raw_os_error(libc::ENOTDIR)
    }
    #[cfg(not(unix))]
    {
        std::io::Error::from(std::io::ErrorKind::NotADirectory)
    }
}

/// 递归 mkdir 核心：返回**失败 syscall 的路径**与原始错误。
///
/// `target` 为真表示 `path` 是最初请求的路径（Node 的 `paths` 栈为空）；否则是递归中的
/// 祖先组件（`paths.size() > 0`，Node 的异步实现会把这些 EEXIST 失败统一成 `ENOTDIR`）。
fn create_dir_all_inner(
    path: &Path,
    mode: u32,
    flavor: MkdirFlavor,
    target: bool,
) -> Result<(), (PathBuf, std::io::Error)> {
    match create_dir_once(path, mode) {
        Ok(()) => Ok(()),
        // 缺父目录：先递归创建父路径，成功后**重试本层**；父路径为空或已经到根时无法继续，
        // 把原始 ENOENT 作为失败返回。
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let parent = path.parent();
            match parent {
                Some(parent) if parent != path && !parent.as_os_str().is_empty() => {
                    create_dir_all_inner(parent, mode, flavor, false)?;
                    create_dir_once(path, mode).map_err(|error| (path.to_path_buf(), error))
                }
                _ => Err((path.to_path_buf(), error)),
            }
        }
        // EEXIST 后 `stat`：目录（含指向目录的符号链接）视为成功。
        // - 目标本身（`target`）是普通文件 → EEXIST（两种 flavor 相同）；
        // - 祖先组件是普通文件 → 两种 flavor 都是 ENOTDIR（`MKDirpAsync`/`MKDirpSync`
        //   在 `paths.size() > 0` 时把 EEXIST + 非目录改成 ENOTDIR）；
        // - `stat` 失败（如悬空符号链接）→ 异步版祖先组件改报 ENOTDIR，同步版原样返回
        //   stat 的错误码（路径由调用方替换成原始请求路径）。
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            match std::fs::metadata(path) {
                Ok(metadata) if metadata.is_dir() => Ok(()),
                Ok(_) if !target => Err((path.to_path_buf(), not_a_directory_error())),
                Ok(_) => Err((path.to_path_buf(), error)),
                Err(_) if flavor == MkdirFlavor::Async && !target => {
                    Err((path.to_path_buf(), not_a_directory_error()))
                }
                Err(metadata_error) => Err((path.to_path_buf(), metadata_error)),
            }
        }
        Err(error) => Err((path.to_path_buf(), error)),
    }
}

/// 单层 `mkdir`（`mode` 受 umask 约束，权限位只在 unix 上生效）。
fn create_dir_once(path: &Path, mode: u32) -> std::io::Result<()> {
    let mut builder = std::fs::DirBuilder::new();
    #[cfg(unix)]
    builder.mode(mode);
    #[cfg(not(unix))]
    let _ = mode;
    builder.create(path)
}

/// docker 运行上下文（TS 中未导出，但出现在所有公开签名的参数里）。
///
/// TS: `docker.ts#DockerContext`
#[derive(Clone, Debug)]
pub struct DockerContext {
    /// 构建好的两个变体镜像。
    pub images: BuiltImages,
    /// 本次运行的 `.eval/<runId>` 产物目录（绝对路径）。
    pub artifact_directory: PathBuf,
    /// `auth.json` 的绝对路径。
    pub auth_path: PathBuf,
    /// provider id。
    pub provider: String,
    /// model id。
    pub model: String,
    /// 每个变体的重复次数（已校验的正整数，用 JS number 表达）。
    pub runs_per_variant: f64,
}

/// 组装 [`DockerContext`]。
///
/// TS: `docker.ts#createDockerContext`
///
/// 参数原样存入，不做校验：`runs_per_variant` 的合法性由 `cli` 的参数解析保证，最终以
/// JS 数字文本作为 `KIANA_EVAL_RUNS_PER_VARIANT` 传给容器。
pub fn create_docker_context(
    images: BuiltImages,
    artifact_directory: PathBuf,
    auth_path: PathBuf,
    provider: impl Into<String>,
    model: impl Into<String>,
    runs_per_variant: f64,
) -> DockerContext {
    DockerContext {
        images,
        artifact_directory,
        auth_path,
        provider: provider.into(),
        model: model.into(),
        runs_per_variant,
    }
}

/// 在某个变体镜像里发现用例，返回 `discovered-tests.json` 的路径。
///
/// TS: `docker.ts#discoverCases`
///
/// 输出目录 = `<artifact_directory>/discovery/<variant>`；entrypoint 参数为
/// `--discover --project docs <files...> <vitest_args...>`，顺序即容器内 vitest 的入参。
/// 非零退出码 → `{variant} eval discovery failed.`；docker 启动失败则返回 `spawnSync ...`。
///
/// 返回的 `discovered-tests.json` 路径**不做存在性检查**：文件是否真的写出，要等调用方
/// 读取时才知道（`cli` 紧接着读取并按 V8 语义解析 JSON）。
pub fn discover_cases(
    context: &DockerContext,
    variant: DocumentationVariant,
    files: &[String],
    vitest_args: &[String],
) -> Result<PathBuf, DockerError> {
    let output_directory = context
        .artifact_directory
        .join("discovery")
        .join(variant.as_str());
    let mut entrypoint_args: Vec<String> = vec![
        "--discover".to_string(),
        "--project".to_string(),
        "docs".to_string(),
    ];
    entrypoint_args.extend(files.iter().cloned());
    entrypoint_args.extend(vitest_args.iter().cloned());
    let args = docker_args(context, variant, &output_directory, &entrypoint_args)?;
    let result = execute("docker", &args, false)?;
    if result.status != 0 {
        return Err(DockerError::message(format!(
            "{} eval discovery failed.",
            variant.as_str()
        )));
    }
    Ok(output_directory.join("discovered-tests.json"))
}

/// 任务目录名：`sha256(JSON.stringify([evalSet, caseId, variant, model, runNumber]))`。
///
/// TS: `docker.ts#taskDirectoryName`（内部函数）
fn task_directory_name(task: &EvalTask) -> String {
    // 元组序列化成 JSON 数组，顺序与 TS 的字面量数组一致。
    let identity = serde_json::to_string(&(
        task.eval_set.as_str(),
        task.case_id.as_str(),
        task.variant.as_str(),
        task.model.as_str(),
        task.run_number,
    ))
    .expect("string and integer tuple always serializes");
    sha256_hex(&identity)
}

/// 把 `testName` 转成锚定的正则：只转义 JS `/[.*+?^${}()|[\]\\]/g` 里的元字符。
fn exact_test_name(test_name: &str) -> String {
    let mut escaped = String::with_capacity(test_name.len() + 2);
    escaped.push('^');
    for character in test_name.chars() {
        if matches!(
            character,
            '.' | '*' | '+' | '?' | '^' | '$' | '{' | '}' | '(' | ')' | '|' | '[' | ']' | '\\'
        ) {
            escaped.push('\\');
        }
        escaped.push(character);
    }
    escaped.push('$');
    escaped
}

/// 跑一个任务 arm；`vitest.json` 存在时返回它的路径。
///
/// TS: `docker.ts#runTask`
///
/// 输出目录 = `<artifact_directory>/tasks/<sha256(任务身份 JSON 数组)>`；entrypoint 参数顺序为
/// `--project docs <task.file> --testNamePattern <^evalSet caseId$>`（测试名按 JS 正则元字符
/// 转义后锚定）。
///
/// 忽略 docker 的退出码（测试失败由报告文件体现），只有启动失败才返回 `Err`；因此容器崩溃
/// 也会得到 `Ok(None)`，`cli` 会把缺失报告记成 `errored` observation。报告是否存在用
/// `Path::exists` 判断（与 `existsSync` 一致）。
pub fn run_task(context: &DockerContext, task: &EvalTask) -> Result<Option<PathBuf>, DockerError> {
    let output_directory = context
        .artifact_directory
        .join("tasks")
        .join(task_directory_name(task));
    let test_name = format!("{} {}", task.eval_set, task.case_id);
    let entrypoint_args: Vec<String> = vec![
        "--project".to_string(),
        "docs".to_string(),
        task.file.clone(),
        "--testNamePattern".to_string(),
        exact_test_name(&test_name),
    ];
    let args = docker_args(context, task.variant, &output_directory, &entrypoint_args)?;
    execute("docker", &args, false)?;
    let report_path = output_directory.join("vitest.json");
    // `existsSync`：任何文件系统条目都算存在（含目录）。
    Ok(report_path.exists().then_some(report_path))
}

/// 路径 → 字符串（Node 的路径字符串；非 UTF-8 用替换字符，与 `to_string_lossy` 一致）。
pub(crate) fn path_text(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

/// `path.normalize` 的词法版本（不访问文件系统）。
///
/// 规则：去掉 `.` 与重复分隔符；`..` 消去前一个普通组件，到达根后多余 `..` 被丢弃，相对
/// 路径前面没有可消去组件时保留 `..`。Windows 下 `Component::Prefix` 原样保留，根写成
/// `\`，不做大小写折叠。空路径返回空路径。
pub(crate) fn lexical_normalize(path: &Path) -> PathBuf {
    let mut prefix: Option<OsString> = None;
    let mut rooted = false;
    let mut parts: Vec<OsString> = Vec::new();
    for component in path.components() {
        match component {
            Component::Prefix(value) => prefix = Some(value.as_os_str().to_os_string()),
            Component::RootDir => rooted = true,
            Component::CurDir => {}
            Component::ParentDir => {
                if rooted {
                    parts.pop();
                } else {
                    match parts.last() {
                        Some(last) if last != ".." => {
                            parts.pop();
                        }
                        _ => parts.push(OsString::from("..")),
                    }
                }
            }
            Component::Normal(part) => parts.push(part.to_os_string()),
        }
    }
    let mut result = PathBuf::new();
    if let Some(prefix) = prefix {
        result.push(prefix);
    }
    if rooted {
        #[cfg(windows)]
        result.push("\\");
        #[cfg(not(windows))]
        result.push("/");
    }
    for part in parts {
        result.push(part);
    }
    result
}

/// `path.resolve(base, path)` 的词法版本。
pub(crate) fn js_resolve(base: &Path, path: &str) -> PathBuf {
    let candidate = Path::new(path);
    if candidate.is_absolute() {
        lexical_normalize(candidate)
    } else {
        lexical_normalize(&base.join(candidate))
    }
}

/// `path.resolve(path)`：相对路径以进程 cwd 为基准。
pub(crate) fn js_resolve_single(path: &str) -> PathBuf {
    let candidate = Path::new(path);
    if candidate.is_absolute() {
        lexical_normalize(candidate)
    } else {
        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        lexical_normalize(&cwd.join(candidate))
    }
}

/// `path.relative(from, to)` 的词法版本。
///
/// 先对两边做 [`lexical_normalize`]，再逐组件比较公共前缀；相等时返回空路径。结果可能以
/// 若干 `..` 开头。不访问文件系统，也不做 Windows 的大小写不敏感比较；`cli::container_path`
/// 用它的结果（前缀是否为 `..`）判断文件是否在 `package_root` 内。
pub(crate) fn js_relative(from: &Path, to: &Path) -> PathBuf {
    let from = lexical_normalize(from);
    let to = lexical_normalize(to);
    if from == to {
        return PathBuf::new();
    }
    let from_parts: Vec<String> = from
        .components()
        .map(|component| component.as_os_str().to_string_lossy().into_owned())
        .collect();
    let to_parts: Vec<String> = to
        .components()
        .map(|component| component.as_os_str().to_string_lossy().into_owned())
        .collect();
    let mut common = 0;
    while common < from_parts.len()
        && common < to_parts.len()
        && from_parts[common] == to_parts[common]
    {
        common += 1;
    }
    let mut result = PathBuf::new();
    for _ in common..from_parts.len() {
        result.push("..");
    }
    for part in &to_parts[common..] {
        result.push(part);
    }
    result
}

// 本模块的单元测试锁定的都是与 TS 逐字对齐的契约：任务目录哈希、testName 转义、
// path 的词法函数、docker run 参数顺序、auth.json 的路径与 JSON 语义、Node 的
// mkdir/SystemError 文本，以及 ENOEXEC 的 /bin/sh 回退。涉及真实 docker 进程的端到端
// 路径在 tests/docker.rs 里用假 docker 覆盖。
#[cfg(test)]
mod tests {
    use super::*;

    fn sample_task() -> EvalTask {
        EvalTask {
            file: "evals/example.docs.eval.ts".to_string(),
            full_name: "Demo > case one".to_string(),
            eval_set: "Demo".to_string(),
            case_id: "case one".to_string(),
            variant: DocumentationVariant::WithoutDocs,
            model: "fixture/model".to_string(),
            run_number: 1,
        }
    }

    #[test]
    fn task_directory_name_matches_the_ts_golden_hash() {
        assert_eq!(
            task_directory_name(&sample_task()),
            "744bf236f284bc2f9b3699fc08178a9fa067147a5ab22df26d760df0c83fcf60"
        );
    }

    #[test]
    fn task_directory_name_distinguishes_each_identity_field() {
        let mut other = sample_task();
        other.run_number = 2;
        assert_ne!(
            task_directory_name(&sample_task()),
            task_directory_name(&other)
        );
        other = sample_task();
        other.variant = DocumentationVariant::WithDocs;
        assert_ne!(
            task_directory_name(&sample_task()),
            task_directory_name(&other)
        );
        other = sample_task();
        other.model = "fixture/other".to_string();
        assert_ne!(
            task_directory_name(&sample_task()),
            task_directory_name(&other)
        );
    }

    /// golden：`"a.b*c+d?^${}()|[]\\ end".replace(/[.*+?^${}()|[\]\\]/g, "\\$&")`
    /// （由 Node 生成）。
    #[test]
    fn exact_test_name_escapes_the_js_regex_metacharacters() {
        assert_eq!(
            exact_test_name("a.b*c+d?^${}()|[]\\ end"),
            "^a\\.b\\*c\\+d\\?\\^\\$\\{\\}\\(\\)\\|\\[\\]\\\\ end$"
        );
        assert_eq!(exact_test_name("plain name"), "^plain name$");
    }

    #[test]
    fn lexical_normalize_matches_node_path_rules() {
        assert_eq!(
            lexical_normalize(Path::new("/a/./b/../c")),
            PathBuf::from("/a/c")
        );
        assert_eq!(
            lexical_normalize(Path::new("/a//b///c/")),
            PathBuf::from("/a/b/c")
        );
        assert_eq!(lexical_normalize(Path::new("/../a")), PathBuf::from("/a"));
        assert_eq!(
            lexical_normalize(Path::new("a/../../b")),
            PathBuf::from("../b")
        );
        assert_eq!(lexical_normalize(Path::new("/")), PathBuf::from("/"));
    }

    #[test]
    fn js_resolve_and_relative_match_node_path() {
        let base = Path::new("/repo/packages/evals");
        assert_eq!(
            js_resolve(base, "evals/smoke.eval.ts"),
            PathBuf::from("/repo/packages/evals/evals/smoke.eval.ts")
        );
        assert_eq!(
            js_resolve(base, "/repo/packages/evals/evals/a.ts"),
            PathBuf::from("/repo/packages/evals/evals/a.ts")
        );
        assert_eq!(
            js_resolve(base, "../other.ts"),
            PathBuf::from("/repo/packages/other.ts")
        );
        assert_eq!(
            js_relative(base, Path::new("/repo/packages/evals/evals/a.ts")),
            PathBuf::from("evals/a.ts")
        );
        assert_eq!(
            js_relative(base, Path::new("/repo/packages/other/a.ts")),
            PathBuf::from("../other/a.ts")
        );
        assert_eq!(
            js_relative(base, Path::new("/repo/packages/evals")),
            PathBuf::new()
        );
    }

    fn test_context(output: &Path) -> DockerContext {
        let images = BuiltImages {
            without_docs: BuiltImage {
                name: "kiana-evals-abc-without-docs:local".to_string(),
                id: "sha256:without".to_string(),
            },
            with_docs: BuiltImage {
                name: "kiana-evals-abc-with-docs:local".to_string(),
                id: "sha256:with".to_string(),
            },
        };
        create_docker_context(
            images,
            output.join("artifact"),
            PathBuf::from("/tmp/host-auth/auth.json"),
            "fixture",
            "model",
            2.0,
        )
    }

    #[test]
    fn docker_args_match_the_spec_vector() {
        let directory = tempfile::tempdir().unwrap();
        let context = test_context(directory.path());
        let output = directory.path().join("discovery").join("without_docs");
        let args = docker_args(
            &context,
            DocumentationVariant::WithoutDocs,
            &output,
            &[
                "--discover".to_string(),
                "--project".to_string(),
                "docs".to_string(),
            ],
        )
        .unwrap();
        let output_text = path_text(&output);
        let mut expected: Vec<String> = vec![
            "run".into(),
            "--rm".into(),
            "--read-only".into(),
            "--tmpfs".into(),
            "/tmp:rw,exec,mode=1777".into(),
            "--tmpfs".into(),
            "/repo/node_modules/.vite-temp:rw,exec,mode=1777".into(),
            "--mount".into(),
            format!("type=bind,source={output_text},target=/artifacts"),
            "--env".into(),
            "KIANA_EVAL_ARTIFACT_DIR=/artifacts".into(),
            "--env".into(),
            "KIANA_EVAL_RUNS_PER_VARIANT=2".into(),
            "--env".into(),
            "KIANA_EVAL_SANDBOX_UID=65532".into(),
            "--env".into(),
            "KIANA_EVAL_SANDBOX_GID=65532".into(),
            "--env".into(),
            "KIANA_PROVIDER=fixture".into(),
            "--env".into(),
            "KIANA_MODEL=model".into(),
        ];
        #[cfg(unix)]
        {
            // SAFETY: `getuid`/`getgid` 没有前置条件，也不会失败。
            let (uid, gid) = unsafe { (libc::getuid(), libc::getgid()) };
            expected.push("--env".into());
            expected.push(format!("KIANA_EVAL_ARTIFACT_UID={uid}"));
            expected.push("--env".into());
            expected.push(format!("KIANA_EVAL_ARTIFACT_GID={gid}"));
        }
        expected.push("--mount".into());
        expected.push("type=bind,source=/tmp/host-auth/auth.json,target=/run/kiana-eval-secrets/auth.json,readonly".into());
        expected.push("kiana-evals-abc-without-docs:local".into());
        expected.push("--discover".into());
        expected.push("--project".into());
        expected.push("docs".into());
        assert_eq!(args, expected);
        assert!(output.is_dir());
    }

    #[cfg(unix)]
    #[test]
    fn docker_args_creates_the_output_directory_with_mode_0700() {
        use std::os::unix::fs::PermissionsExt as _;
        let directory = tempfile::tempdir().unwrap();
        let context = test_context(directory.path());
        let output = directory.path().join("tasks").join("nested");
        docker_args(&context, DocumentationVariant::WithDocs, &output, &[]).unwrap();
        let mode = std::fs::metadata(&output).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700);
    }

    fn write_auth(directory: &Path, contents: &str) -> PathBuf {
        std::fs::create_dir_all(directory).unwrap();
        let path = directory.join("auth.json");
        std::fs::write(&path, contents).unwrap();
        path
    }

    #[test]
    fn require_eval_auth_file_accepts_an_object_with_the_provider_key() {
        let directory = tempfile::tempdir().unwrap();
        let path = write_auth(directory.path(), r#"{"fixture": {"type": "api_key"}}"#);
        let resolved =
            require_eval_auth_file_with("fixture", Some(&path_text(directory.path()))).unwrap();
        assert_eq!(resolved, path);
    }

    #[test]
    fn require_eval_auth_file_reports_a_missing_file() {
        let directory = tempfile::tempdir().unwrap();
        let error =
            require_eval_auth_file_with("fixture", Some(&path_text(directory.path()))).unwrap_err();
        assert_eq!(
            error.to_string(),
            format!(
                "Eval authentication file does not exist: {}",
                path_text(&directory.path().join("auth.json"))
            )
        );
    }

    #[test]
    fn require_eval_auth_file_reports_a_directory_at_the_auth_path() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(directory.path().join("auth.json")).unwrap();
        let error =
            require_eval_auth_file_with("fixture", Some(&path_text(directory.path()))).unwrap_err();
        assert_eq!(
            error.to_string(),
            format!(
                "Eval authentication file does not exist: {}",
                path_text(&directory.path().join("auth.json"))
            )
        );
    }

    #[test]
    fn require_eval_auth_file_reports_invalid_json() {
        let directory = tempfile::tempdir().unwrap();
        write_auth(directory.path(), "{not json");
        let error =
            require_eval_auth_file_with("fixture", Some(&path_text(directory.path()))).unwrap_err();
        assert_eq!(
            error.to_string(),
            format!(
                "Eval authentication file is invalid: {}",
                path_text(&directory.path().join("auth.json"))
            )
        );
    }

    /// TS 把 `readFileSync` 与 `JSON.parse` 放在同一个 `try` 里：读取失败也是 invalid。
    #[cfg(unix)]
    #[test]
    fn require_eval_auth_file_reports_unreadable_files_as_invalid() {
        use std::os::unix::fs::PermissionsExt as _;
        // root 会忽略权限位。
        // SAFETY: `geteuid` 没有前置条件。
        if unsafe { libc::geteuid() } == 0 {
            return;
        }
        let directory = tempfile::tempdir().unwrap();
        let path = write_auth(directory.path(), r#"{"fixture": 1}"#);
        let mut permissions = std::fs::metadata(&path).unwrap().permissions();
        permissions.set_mode(0o000);
        std::fs::set_permissions(&path, permissions).unwrap();

        let error =
            require_eval_auth_file_with("fixture", Some(&path_text(directory.path()))).unwrap_err();
        let mut permissions = std::fs::metadata(&path).unwrap().permissions();
        permissions.set_mode(0o600);
        std::fs::set_permissions(&path, permissions).unwrap();
        assert_eq!(
            error.to_string(),
            format!(
                "Eval authentication file is invalid: {}",
                path_text(&directory.path().join("auth.json"))
            )
        );
    }

    /// `os.homedir()`：`HOME=""` 也要保留（相对路径 `.kiana/agent`），只有未设置才回退。
    #[cfg(unix)]
    #[test]
    fn eval_auth_path_matches_node_join_and_homedir_semantics() {
        assert_eq!(
            eval_auth_path(None, Some(OsStr::new(""))).unwrap(),
            PathBuf::from(".kiana/agent/auth.json")
        );
        assert_eq!(
            eval_auth_path(None, Some(OsStr::new("/home/fixture"))).unwrap(),
            PathBuf::from("/home/fixture/.kiana/agent/auth.json")
        );
        // `path.join` 做词法归一：`.`/`..`/重复分隔符都不能留在消息与 `--mount source=` 里。
        assert_eq!(
            eval_auth_path(None, Some(OsStr::new("/tmp/x/./sub/../sub"))).unwrap(),
            PathBuf::from("/tmp/x/sub/.kiana/agent/auth.json")
        );
        assert_eq!(
            eval_auth_path(None, Some(OsStr::new("."))).unwrap(),
            PathBuf::from(".kiana/agent/auth.json")
        );
        assert_eq!(
            eval_auth_path(None, Some(OsStr::new("a//b/"))).unwrap(),
            PathBuf::from("a/b/.kiana/agent/auth.json")
        );
        assert_eq!(eval_auth_path(None, None), None);
        assert_eq!(
            eval_auth_path(Some("/agent/dir"), Some(OsStr::new("/home/fixture"))).unwrap(),
            PathBuf::from("/agent/dir/auth.json")
        );
        // `KIANA_CODING_AGENT_DIR=""` 是 falsy，回退到 homedir。
        assert_eq!(
            eval_auth_path(Some(""), Some(OsStr::new("/home/fixture"))).unwrap(),
            PathBuf::from("/home/fixture/.kiana/agent/auth.json")
        );
        // 相对 `KIANA_CODING_AGENT_DIR` 按进程 cwd 解析（`path.resolve`）。
        let cwd = std::env::current_dir().unwrap();
        assert_eq!(
            eval_auth_path(Some("relative/agent"), None).unwrap(),
            lexical_normalize(&cwd.join("relative/agent")).join("auth.json")
        );
    }

    /// Windows 的 `os.homedir()` 优先 `USERPROFILE`（libuv `uv_os_homedir`），而不是
    /// `dirs::home_dir()` 用的 Known Folder。
    #[test]
    fn windows_home_directory_prefers_userprofile_over_the_profile_api() {
        let profile = OsString::from(r"C:\Users\fixture");
        assert_eq!(
            windows_home_directory(Some(profile.clone()), || Some(PathBuf::from(
                r"D:\fallback"
            ))),
            Some(profile)
        );
        assert_eq!(
            windows_home_directory(None, || Some(PathBuf::from(r"D:\fallback"))),
            Some(OsString::from(r"D:\fallback"))
        );
        // 已设置但短于 3 字节：libuv 视为不可用并返回 ENOENT，不回退到 profile 目录。
        assert_eq!(
            windows_home_directory(Some(OsString::from("C:")), || Some(PathBuf::from(
                r"D:\fallback"
            ))),
            None
        );
        assert_eq!(
            windows_home_directory(Some(OsString::from("")), || Some(PathBuf::from(
                r"D:\fallback"
            ))),
            None
        );
        assert_eq!(windows_home_directory(None, || None), None);
    }

    #[test]
    fn require_eval_auth_file_rejects_non_objects_and_missing_keys() {
        for contents in ["null", "[]", "\"text\"", "42", "{\"other\": 1}"] {
            let directory = tempfile::tempdir().unwrap();
            write_auth(directory.path(), contents);
            let error = require_eval_auth_file_with("fixture", Some(&path_text(directory.path())))
                .unwrap_err();
            assert_eq!(
                error.to_string(),
                "Eval authentication file has no credential for provider fixture."
            );
        }
    }

    #[test]
    fn require_eval_auth_file_reads_kiana_coding_agent_dir_from_the_environment() {
        use std::sync::Mutex;
        static ENV_LOCK: Mutex<()> = Mutex::new(());
        let _guard = ENV_LOCK.lock().unwrap_or_else(|error| error.into_inner());
        let directory = tempfile::tempdir().unwrap();
        let path = write_auth(directory.path(), r#"{"fixture": 1}"#);
        let previous = std::env::var_os("KIANA_CODING_AGENT_DIR");
        // SAFETY: 测试用互斥量串行化该进程内唯一的环境变量写入；写入后立即恢复。
        unsafe { std::env::set_var("KIANA_CODING_AGENT_DIR", directory.path()) };
        let resolved = require_eval_auth_file("fixture");
        match previous {
            // SAFETY: 同上，仍在互斥量保护内。
            Some(value) => unsafe { std::env::set_var("KIANA_CODING_AGENT_DIR", value) },
            // SAFETY: 同上。
            None => unsafe { std::env::remove_var("KIANA_CODING_AGENT_DIR") },
        }
        assert_eq!(resolved.unwrap(), path);
    }

    /// Node 的 `SystemError.message`：`{CODE}: {strerror}, {syscall} '<path>'`。
    #[cfg(unix)]
    #[test]
    fn node_system_error_formats_like_a_node_system_error() {
        let error = std::io::Error::from_raw_os_error(libc::ENOENT);
        assert_eq!(
            node_system_error("open", Some(Path::new("/tmp/x")), &error),
            "ENOENT: no such file or directory, open '/tmp/x'"
        );
        // 写错误（libuv 的 write 没有 path）。
        assert_eq!(
            node_system_error("write", None, &error),
            "ENOENT: no such file or directory, write"
        );
        let exists = std::io::Error::from_raw_os_error(libc::EEXIST);
        assert_eq!(
            node_system_error("mkdir", Some(Path::new("/tmp/y")), &exists),
            "EEXIST: file already exists, mkdir '/tmp/y'"
        );
        // libuv 的 uv_strerror 文本（不是内核 strerror）。
        #[cfg(unix)]
        {
            for (errno, expected) in [
                (libc::ENFILE, "ENFILE: file table overflow, open '/tmp/x'"),
                (
                    libc::EFAULT,
                    "EFAULT: bad address in system call argument, open '/tmp/x'",
                ),
                (
                    libc::EAGAIN,
                    "EAGAIN: resource temporarily unavailable, open '/tmp/x'",
                ),
            ] {
                let error = std::io::Error::from_raw_os_error(errno);
                assert_eq!(
                    node_system_error("open", Some(Path::new("/tmp/x")), &error),
                    expected
                );
            }
        }
    }

    #[test]
    fn create_dir_all_mode_creates_nested_directories() {
        let directory = tempfile::tempdir().unwrap();
        let target = directory.path().join("a").join("b").join("c");
        create_dir_all_mode(&target, 0o700).unwrap();
        assert!(target.is_dir());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = std::fs::metadata(&target).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o700);
        }
        // 已存在的目录：静默成功。
        create_dir_all_mode(&target, 0o700).unwrap();
    }

    /// Node 的 mkdirp 逐层创建，错误路径是**最先失败的那一层**。
    #[cfg(unix)]
    #[test]
    fn create_dir_all_mode_reports_the_failing_component() {
        use std::os::unix::fs::PermissionsExt as _;
        // SAFETY: `geteuid` 没有前置条件。
        if unsafe { libc::geteuid() } == 0 {
            return;
        }
        let directory = tempfile::tempdir().unwrap();
        let readonly = directory.path().join("readonly");
        std::fs::create_dir(&readonly).unwrap();
        let mut permissions = std::fs::metadata(&readonly).unwrap().permissions();
        permissions.set_mode(0o555);
        std::fs::set_permissions(&readonly, permissions).unwrap();

        let target = readonly.join("a").join("b");
        let error = create_dir_all_mode(&target, 0o700).unwrap_err();
        let mut permissions = std::fs::metadata(&readonly).unwrap().permissions();
        permissions.set_mode(0o700);
        std::fs::set_permissions(&readonly, permissions).unwrap();
        assert_eq!(
            error.to_string(),
            format!(
                "EACCES: permission denied, mkdir '{}'",
                path_text(&readonly.join("a"))
            )
        );

        // 已存在的普通文件 → EEXIST（Node 的 mkdirSync 同样报错）。
        let file = directory.path().join("file");
        std::fs::write(&file, "").unwrap();
        assert_eq!(
            create_dir_all_mode(&file, 0o700).unwrap_err().to_string(),
            format!("EEXIST: file already exists, mkdir '{}'", path_text(&file))
        );
    }

    /// 悬空符号链接祖先（`fs.promises.mkdir`）：Node 的异步实现在祖先组件上 `EEXIST`
    /// 后 stat 失败，按 `MKDirpAsync` 改报 **ENOTDIR**，路径是失败组件。
    #[cfg(unix)]
    #[test]
    fn create_dir_all_mode_reports_a_dangling_symlink_ancestor_as_enotdir() {
        let directory = tempfile::tempdir().unwrap();
        let dangling = directory.path().join("dangling");
        std::os::unix::fs::symlink("missing-target", &dangling).unwrap();
        let target = dangling.join("run-id");
        assert_eq!(
            create_dir_all_mode(&target, 0o700).unwrap_err().to_string(),
            format!("ENOTDIR: not a directory, mkdir '{}'", path_text(&dangling))
        );
    }

    /// 悬空符号链接祖先（`mkdirSync`）：Node 的同步实现用**原始请求路径**报 ENOENT
    /// （错误码来自失败的 stat）。
    #[cfg(unix)]
    #[test]
    fn create_dir_all_mode_sync_reports_the_original_path_for_a_dangling_symlink_ancestor() {
        let directory = tempfile::tempdir().unwrap();
        let dangling = directory.path().join("dangling");
        std::os::unix::fs::symlink("missing-target", &dangling).unwrap();
        let target = dangling.join("a").join("b");
        assert_eq!(
            create_dir_all_mode_sync(&target, 0o700)
                .unwrap_err()
                .to_string(),
            format!(
                "ENOENT: no such file or directory, mkdir '{}'",
                path_text(&target)
            )
        );
    }

    /// `mkdirSync` 的深层失败也用原路径（cli 的异步版本用失败组件路径，见上一个测试）。
    #[cfg(unix)]
    #[test]
    fn create_dir_all_mode_sync_reports_the_original_path_for_deep_failures() {
        use std::os::unix::fs::PermissionsExt as _;
        // SAFETY: `geteuid` 没有前置条件。
        if unsafe { libc::geteuid() } == 0 {
            return;
        }
        let directory = tempfile::tempdir().unwrap();
        let readonly = directory.path().join("readonly");
        std::fs::create_dir(&readonly).unwrap();
        let mut permissions = std::fs::metadata(&readonly).unwrap().permissions();
        permissions.set_mode(0o555);
        std::fs::set_permissions(&readonly, permissions).unwrap();

        let target = readonly.join("a").join("b");
        let error = create_dir_all_mode_sync(&target, 0o700).unwrap_err();
        let mut permissions = std::fs::metadata(&readonly).unwrap().permissions();
        permissions.set_mode(0o700);
        std::fs::set_permissions(&readonly, permissions).unwrap();
        assert_eq!(
            error.to_string(),
            format!("EACCES: permission denied, mkdir '{}'", path_text(&target))
        );
    }

    /// libuv 的 `execvp` 在 ENOEXEC 时回退到 `/bin/sh <文件> <参数...>`。
    #[cfg(unix)]
    #[test]
    fn execute_falls_back_to_the_shell_for_enoexec() {
        use std::os::unix::fs::PermissionsExt as _;
        let directory = tempfile::tempdir().unwrap();
        let script = directory.path().join("not-a-binary");
        std::fs::write(&script, "exit 3\n").unwrap();
        let mut permissions = std::fs::metadata(&script).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&script, permissions).unwrap();

        let output = execute(&path_text(&script), &[], true).unwrap();
        assert_eq!(output.status, 3);
        assert_eq!(output.stdout, "");
    }

    #[test]
    fn execute_formats_spawn_failures_like_spawn_sync() {
        let command = "kiana-evals-definitely-missing-command";
        let error = execute(command, &[], true).unwrap_err();
        assert_eq!(error.to_string(), format!("spawnSync {command} ENOENT"));
    }

    /// `requireEvalAuthFile` 的 `JSON.parse` 语义：孤立代理项转义与溢出数字都被接受。
    #[test]
    fn require_eval_auth_file_uses_json_parse_semantics() {
        let directory = tempfile::tempdir().unwrap();
        write_auth(
            directory.path(),
            r#"{"fixture": {"x": "\ud800"}, "other": {"y": 1e999}}"#,
        );
        let resolved =
            require_eval_auth_file_with("fixture", Some(&path_text(directory.path()))).unwrap();
        assert_eq!(resolved, directory.path().join("auth.json"));
    }
}
