//! utils/tools-manager.ts：fd/ripgrep 的查找、下载、解压与安装。
//!
//! TS: `packages/coding-agent/src/utils/tools-manager.ts`（397 行）。
//!
//! ## 关键语义
//!
//! - `TOOLS_DIR = getBinDir()`：TS 在**模块加载时**捕获；Rust 用 [`LazyLock`] 在首次使用时
//!   捕获（`getBinDir()` 依赖 `KIANA_CODING_AGENT_DIR` 等环境变量，放在使用点更接近真实行为）。
//! - `NETWORK_TIMEOUT_MS = 10_000`、`DOWNLOAD_TIMEOUT_MS = 120_000`。
//! - `getReleaseTarget` 只映射 `arm64 → aarch64`、`x64 → x86_64`；其它架构（`arm`/`ia32`）
//!   返回 `None`，**不回退 x86_64**（earendil-works/kiana#11209）。
//! - `getLatestVersion` 用 `redirect: "manual"` 读 3xx 的 `Location`：`reqwest` 的重定向策略
//!   只能在 `Client` 级配置，因此使用 `http_dispatcher` 提供的同代理配置、`Policy::none()`
//!   ／`Policy::none()` 专用 client；下载走进程级共享 client（跟随重定向）。
//! - 解压**调用外部命令**（tar/unzip/tar.exe/PowerShell `Expand-Archive`），命令与参数固定；
//!   **不引入** tar/zip crate（`spec/ca-tools-compaction-export.md` §2.28.9）。
//! - `ensureTool` 的 info/warning 文案、离线/Android/unsupported 分支与错误 cause 链逐字。
//!
//! ## 与 TS 的已知差异
//!
//! - `spawnSync` 启动失败已按 Node 的 `spawnSync <command> <errno>` 文案复刻
//!   （`format_spawn_sync_error`）；`readdirSync`/`renameSync`/`mkdirSync` 等文件系统错误
//!   仍用 `std::io::Error` 的 `Display`（如 `Is a directory (os error 21)`），它们只出现在
//!   解压/安装失败的 warning 里。
//! - `Math.random().toString(36).slice(2, 10)` 的临时目录后缀用 uuid v4 字节映射成 8 位
//!   base36（只要求唯一，名字不会出现在成功路径的输出里）。
//! - 下载请求本身不设置 `User-Agent`（`fetchWithRetry(url, undefined, …)`），因此带的是
//!   undici fetch 的默认 `user-agent: undici`（`shared_http_client` 的默认头）；`spec/ca-cli.md`
//!   §4.6 的表格写成“同上（kiana UA）”，以 TS 源码为准。
//! - 响应体读取受与 `fetch_with_retry` 相同的总预算约束：请求上附了剩余预算
//!   （`RequestBuilder::timeout`），本模块另外用 `timeout_at` 包住整个下载（双保险），
//!   等价 TS 的共享 `AbortSignal.timeout`；调用方取消（`CancellationToken`）在 body 阶段
//!   仍不可感知（`reqwest` 的 `Response` 无法携带取消）。
//! - **多地址连接失败的 cause 形状**：undici 的 `fetch` 在 DNS 解析出多个地址且全部失败时把
//!   `AggregateError`（`message` 为空串）挂在 `error.cause` 上，`ensureTool` 的遍历会得到
//!   `` `fetch failed: ` ``（尾随分隔符）；`reqwest` 没有多地址聚合错误（`hyper-util` 只保留
//!   最后一个地址的错误），Rust 取源链最深文案（`fetch failed: <detail>`）。单地址/IP 失败时
//!   两侧的 cause 文案按 Node 的 errno 格式生成（`fetch failed: connect ECONNREFUSED <host>:<port>`、
//!   `fetch failed: getaddrinfo ENOTFOUND <host>`），见 `management_http` 的
//!   `FetchError::from_reqwest`；主机名目标时 Node 打印解析后的 IP，Rust 只能打印 URL 主机名。
//! - **body 截断的 cause**：undici 的 `TypeError("terminated")` 对 `content-length` 不匹配给出
//!   `Response body length does not match content-length header`（已逐字映射）；chunked 解析等
//!   其它 body 错误的原因是 llhttp 内部状态，`reqwest` 无法对应，只保留 `terminated`。
//!
//! ## 对外入口
//!
//! - [`get_tool_path`]：只查找、不下载，返回本地路径或 PATH 上的命令名。
//! - [`ensure_tool`] / [`ensure_tool_without_status`]：找不到就下载；后者用于需要 `Send`
//!   future 的调用方（`kiana-tools` 的 grep/find）。
//! - [`get_latest_version`]：GitHub release 版本查询；`download_tool` 内部使用的是
//!   `get_latest_version_with_base`（`get_latest_version` 的测试注入版本，基地址可替换），
//!   但 fd 在 darwin/x64 上固定为 `10.3.0`、跳过查询（见 `download_tool`）。
//! - [`ToolsManagerEnv`] 与各 `*_with` 变体：测试注入点，见其文档。
//!
//! ## 设计原因
//!
//! - fd/ripgrep 是外部可执行文件，不是 crate 依赖：本模块只负责把可执行文件准备好，真正的
//!   搜索由 `kiana-tools` 的 `find`/`grep` 以子进程执行。因此主要入口 `ensure_tool` 把失败
//!   转换成 `None`（并可选地发 warning），而不是向上抛错，由上层生成
//!   “not available and could not be downloaded” 一类的工具错误。
//! - 版本查询走 `github.com/<repo>/releases/latest` 的 3xx，而不是 `api.github.com`：TS 源码
//!   （`tools-manager.ts` 第 93–98 行）说明匿名 API 配额是每 IP 每小时 60 次，共享出口
//!   （企业代理、CI）会把它耗尽；release 页面端点没有配额，且与二进制下载同源。
//! - 解压调用外部 `tar`/`unzip`/`powershell.exe`，而不是引入 tar/zip crate：TS 就是这样做的
//!   （`spec/ca-tools-compaction-export.md` §2.28.9），Rust 保持同一行为。代价是宿主机缺少这些
//!   命令时安装失败：命令根本启动不了（`Command::output()` 返回 `Err`，如命令不在 PATH 上）
//!   时文案是 `spawnSync <cmd> <errno>`（命令不存在时 errno 为 `ENOENT`，见
//!   `format_spawn_sync_error`）；只有命令启动成功但退出非 0 时，文案才带它的
//!   stderr/stdout（见 `format_spawn_failure`）。
//!
//! ## 执行流程（`ensure_tool`）
//!
//! 1. `get_tool_path_with`：先看 `<bin_dir>/<binary>(.exe)` 是否存在，再按 `system_binary_names`
//!    （fd：`fd`/`fdfind`；rg：`rg`）探测 PATH；命中直接返回，不触发状态回调。
//! 2. 未注册的工具 → `None`（无回调）。
//! 3. 离线 → warning；Android/Termux → `pkg install` 提示；平台/架构无预编译资产 →
//!    “Automatic download is not supported” 提示；三者都直接返回 `None`。
//! 4. 其余：info “Downloading...” → `download_tool`（查版本 → 下载 → 解压 → 安装到
//!    `<bin_dir>/<binary>(.exe)`）→ 成功 info “installed to ...”，失败 warning
//!    `Failed to download <name>: <cause 链>`。
//!
//! 检查顺序是输出契约的一部分：离线优先于 Android，Android 优先于平台支持检查；调整顺序会
//! 改变用户看到的提示。状态回调是同步 `FnMut`，在调用 `ensure_tool` 的任务里执行。
//!
//! ## 注意事项
//!
//! - 本模块的入口没有取消参数（TS 的 `ensureTool` 也不接收 signal）。调用方若用
//!   `tokio::select!` 丢弃整个 future（`kiana-tools` 的 `abortable` 就是如此），下载会随
//!   future 中止，但不会运行 `download_tool` 的清理代码，可能留下未下载完的归档。
//! - 下载与解压混用异步 IO（`reqwest`/`tokio::fs`）和同步 IO（`std::fs`、
//!   `std::process::Command`）。同步调用会阻塞当前运行时线程直到完成（对齐 TS 的同步
//!   `fs`/`child_process` API）；调用点主要在启动期（`kiana-interactive` 的
//!   `interactive_mode` 第 8 步）与首次使用搜索工具时。
//! - `download_tool` 对同一工具的并发调用没有互斥：它们写同一个归档路径和最终二进制路径
//!   （TS 相同）；`extract_tmp_*` 的唯一目录名只解决 fd 与 rg 之间的竞争。
//! - 返回给调用方的路径经过 `to_string_lossy`（`get_tool_path_with` / `download_tool`），
//!   非 UTF-8 路径会变成替换字符，可能不再对应磁盘上的原始字节。

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, LazyLock};
use std::time::Duration;

use tokio::io::AsyncWriteExt;

use crate::changelog::get_kiana_user_agent;
use crate::config::VERSION;
use crate::http_dispatcher::shared_http_client;
use crate::management_http::{FetchError, FetchRetryOptions, fetch_with_retry};

/// `NETWORK_TIMEOUT_MS`（版本查询）。
///
/// 毫秒；作为 `fetch_with_retry` 的总预算（从函数入口计时，重试共享），不是单次尝试的超时。
const NETWORK_TIMEOUT_MS: u64 = 10_000;

/// `DOWNLOAD_TIMEOUT_MS`（下载，跨重试共享的总预算）。
///
/// 同一个值被用了两次：`download_file` 外层的 `timeout_at` 与 `fetch_with_retry` 内部的
/// `timeout_ms`。外层 deadline 在进入 `fetch_with_retry` 之前建立，实际先到期。
const DOWNLOAD_TIMEOUT_MS: u64 = 120_000;

/// TS 里硬编码的 GitHub 基地址；测试通过 [`ToolsManagerEnv::release_base`] 覆盖为 wiremock。
const DEFAULT_RELEASE_BASE: &str = "https://github.com";

/// `getBinDir()`（TS 在模块加载时捕获 `TOOLS_DIR`；Rust 首次使用时捕获）。
///
/// 首次访问会调用 [`crate::config::get_bin_dir`]，后者读取 `KIANA_CODING_AGENT_DIR` 等环境
/// 变量；之后再改环境变量也不会换目录。测试若要替换安装目录，必须在首次访问前设置环境变量，
/// 或改用 [`ToolsManagerEnv::bin_dir`] 注入。
static TOOLS_DIR: LazyLock<PathBuf> = LazyLock::new(crate::config::get_bin_dir);

/// `TERMUX_PACKAGES`。
///
/// Android/Termux 下提示 `pkg install <包名>` 用的映射；未注册的工具返回 `None`，调用方
/// （`ensure_tool_inner`）会退而使用工具名本身。
fn termux_package(tool: &str) -> Option<&'static str> {
    match tool {
        "fd" => Some("fd"),
        "rg" => Some("ripgrep"),
        _ => None,
    }
}

/// `ToolConfig`（TS 的内部接口）。
///
/// `TOOLS` 的每一项同时描述：怎么在 PATH 上找、从哪个 GitHub 仓库下载、归档里的二进制叫什么
/// 以及资产名怎么拼。它是工具查找/下载配置的主表，但不是唯一要改的地方：Termux 包名另见
/// `termux_package`（`rg → ripgrep`，包名与工具名不同），`download_tool` 里还有 fd 在
/// darwin/x64 上固定版本的特例；新增工具时这两处也要一并检查。
struct ToolConfig {
    /// `TOOLS` 的键（`"fd"`/`"rg"`）。
    key: &'static str,
    /// 展示名（`"fd"`/`"ripgrep"`）。
    name: &'static str,
    /// GitHub 仓库（`"sharkdp/fd"`）。
    repo: &'static str,
    /// 归档内的二进制名。
    binary_name: &'static str,
    /// 优先尝试的系统命令名（下载前）。
    system_binary_names: Option<&'static [&'static str]>,
    /// tag 前缀（fd `v`，rg 空）。
    tag_prefix: &'static str,
    /// 资产名构造函数。
    asset_name: fn(&str, &str) -> String,
}

/// fd 的 GitHub 资产名（`fd-v{version}-{target}`）。
///
/// 资产名是 GitHub release 的冻结契约，拼错会导致下载失败
/// （通常是 404）。
fn fd_asset_name(version: &str, target: &str) -> String {
    format!("fd-v{version}-{target}")
}

/// ripgrep 的 GitHub 资产名（`ripgrep-{version}-{target}`，无 `v` 前缀）。
fn rg_asset_name(version: &str, target: &str) -> String {
    format!("ripgrep-{version}-{target}")
}

/// 工具注册表（顺序无语义，[`tool_config`] 按 `key` 线性查找）。
static TOOLS: [ToolConfig; 2] = [
    ToolConfig {
        key: "fd",
        name: "fd",
        repo: "sharkdp/fd",
        binary_name: "fd",
        system_binary_names: Some(&["fd", "fdfind"]),
        tag_prefix: "v",
        asset_name: fd_asset_name,
    },
    ToolConfig {
        key: "rg",
        name: "ripgrep",
        repo: "BurntSushi/ripgrep",
        binary_name: "rg",
        system_binary_names: None,
        tag_prefix: "",
        asset_name: rg_asset_name,
    },
];

/// 按 `key` 查注册表；未注册的工具返回 `None`。
///
/// 它是所有入口（`get_tool_path_with`/`ensure_tool_inner`/`download_tool`）判断工具名是否
/// 合法的唯一来源。
fn tool_config(tool: &str) -> Option<&'static ToolConfig> {
    TOOLS.iter().find(|config| config.key == tool)
}

/// `getReleaseTarget(plat, arch)`：Node platform/arch → release 资产后缀。
///
/// 只接受 `arm64`/`x64`；`darwin → {arch}-apple-darwin.tar.gz`、
/// `linux → {arch}-unknown-linux-musl.tar.gz`、`win32 → {arch}-pc-windows-msvc.zip`；
/// 其它平台/架构 → `None`（**arm/ia32 不回退 x86-64**，#11209）。
fn get_release_target(platform: &str, architecture: &str) -> Option<String> {
    let arch = match architecture {
        "arm64" => "aarch64",
        "x64" => "x86_64",
        _ => return None,
    };
    match platform {
        "darwin" => Some(format!("{arch}-apple-darwin.tar.gz")),
        "linux" => Some(format!("{arch}-unknown-linux-musl.tar.gz")),
        "win32" => Some(format!("{arch}-pc-windows-msvc.zip")),
        _ => None,
    }
}

/// `isOfflineModeEnabled()`：`KIANA_OFFLINE` 为 `1`/`true`/`yes`（大小写不敏感，空值 false）。
///
/// 不做 trim：`" true "` 不算离线（与 TS 的 `value.toLowerCase()` 比较一致）。用 `var_os`
/// 读取并对非 UTF-8 值做有损转换，因此非 UTF-8 值不会等于这三个字面量。
fn is_offline_mode_enabled() -> bool {
    let Some(value) = std::env::var_os("KIANA_OFFLINE") else {
        return false;
    };
    let value = value.to_string_lossy();
    if value.is_empty() {
        return false;
    }
    value == "1" || value.to_lowercase() == "true" || value.to_lowercase() == "yes"
}

/// `commandExists(cmd)` 的注入类型（`spawnSync(cmd, ["--version"])`：能启动即存在）。
///
/// 只表示“进程能启动”：退出码非 0 也算存在。`Send + Sync` 是类型约束，让回调可以放进
/// [`ToolsManagerEnv`]（内部用 `Arc` 克隆共享）并跨任务/线程调用。
pub type CommandExistsFn = Arc<dyn Fn(&str) -> bool + Send + Sync>;

/// 测试注入点（`spec` 要求避免进程级 mock：platform/arch/bin_dir/spawn_sync/release base）。
///
/// 生产代码用 `ToolsManagerEnv::default()`（全部回退到真实 `std::env::consts` / `getBinDir()` /
/// `https://github.com` / 真实 `spawnSync` 等价物）。
///
/// 该类型是 `#[doc(hidden)]` + `pub`：它是跨 crate 的测试接缝（集成测试
/// `crates/kiana-core/tests/tools_manager.rs` 直接构造并传入 `*_with` 版本），不是稳定 API。
#[doc(hidden)]
#[derive(Clone, Default)]
pub struct ToolsManagerEnv {
    /// Node `process.platform` 值（`"linux"`/`"darwin"`/`"win32"`/`"android"`/…）。
    pub platform: Option<String>,
    /// Node `process.arch` 值（`"x64"`/`"arm64"`/`"arm"`/`"ia32"`/…）。
    pub arch: Option<String>,
    /// `getBinDir()` 的覆盖。
    pub bin_dir: Option<PathBuf>,
    /// GitHub 基地址（默认 `https://github.com`）。
    pub release_base: Option<String>,
    /// `commandExists(cmd)` 的等价物（`spawnSync(cmd, ["--version"])`：能启动即存在）。
    pub command_exists: Option<CommandExistsFn>,
}

impl ToolsManagerEnv {
    // 以下访问器把 `None` 解释为“使用真实环境”：`ToolsManagerEnv::default()` 因此等价于生产
    // 行为。注入值只影响借用它的那次调用。
    fn platform(&self) -> String {
        self.platform.clone().unwrap_or_else(host_platform)
    }

    fn arch(&self) -> String {
        self.arch.clone().unwrap_or_else(host_arch)
    }

    fn bin_dir(&self) -> PathBuf {
        self.bin_dir.clone().unwrap_or_else(|| TOOLS_DIR.clone())
    }

    fn release_base(&self) -> String {
        self.release_base
            .clone()
            .unwrap_or_else(|| DEFAULT_RELEASE_BASE.to_string())
    }

    fn command_exists(&self, command: &str) -> bool {
        match &self.command_exists {
            Some(exists) => exists(command),
            None => default_command_exists(command),
        }
    }
}

/// Node `process.platform` 的等价物。
///
/// `macos → "darwin"`、`windows → "win32"`，其余原样返回（Android 的
/// `std::env::consts::OS` 是 `"android"`，正好被 `ensure_tool` 的 Termux 分支识别）。
fn host_platform() -> String {
    match std::env::consts::OS {
        "macos" => "darwin".to_string(),
        "windows" => "win32".to_string(),
        other => other.to_string(),
    }
}

/// Node `process.arch` 的等价物。
///
/// `x86_64 → "x64"`、`aarch64 → "arm64"`、`x86 → "ia32"`，其余原样返回（如 `"arm"`）。
/// `get_release_target` 只接受前两种，因此 32 位架构在非 Android 平台上会走到“不支持自动
/// 下载”的分支。
fn host_arch() -> String {
    match std::env::consts::ARCH {
        "x86_64" => "x64".to_string(),
        "aarch64" => "arm64".to_string(),
        "x86" => "ia32".to_string(),
        other => other.to_string(),
    }
}

/// `spawnSync(cmd, ["--version"], {stdio:"pipe"})` 的“命令是否存在”判定：
/// 只要进程能启动（无论退出码）`result.error` 就是 undefined → 存在。
///
/// 探测子进程继承当前进程的环境与 PATH，stdin/stdout/stderr 全部指向空设备（不读取、不转发
/// 输出）；`.status()` 同步等待子进程退出，没有超时。与 `spawnSync` 一样，它不是只在启动期
/// 运行：`kiana-interactive` 的 `interactive_mode` 启动时，以及 `kiana-tools` 的 `find`/`grep`
/// 每次执行（经 [`ensure_tool_without_status`] → `get_tool_path_with`）时都会走这里探测 PATH。
fn default_command_exists(command: &str) -> bool {
    Command::new(command)
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok()
}

/// `ToolStatus.type` 的取值（Rust 侧字段名用 `kind`，见 [`ToolStatus`]）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolStatusKind {
    /// `"info"`。
    Info,
    /// `"warning"`。
    Warning,
}

/// `ToolStatus`。
///
/// TS 的字段名是 `type`；`type` 在 Rust 里是关键字，因此用 [`ToolStatus::kind`]
/// （文档注明对应关系）。
///
/// `message` 是逐字契约（`ensure_tool` 生成的 info/warning 文本保持固定），本类型不负责
/// 展示格式。下游 `kiana-interactive` 的 `show_managed_tool_status` 会给 `Warning` 加
/// `Warning: ` 前缀并使用警告色，`Info` 用暗色。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolStatus {
    /// 对应 TS 的 `type` 字段。
    pub kind: ToolStatusKind,
    /// 状态文案（逐字）。
    pub message: String,
}

/// `getToolPath(tool)`：先看 `<bin>/<binary>(.exe)`，再按系统命令名探测 PATH。
///
/// 未知 tool（不是 `"fd"`/`"rg"`）→ `None`。
///
/// 返回值有两种形状：本地命中时是 `bin_dir` 下的完整路径；PATH 命中时是命令名本身
/// （`"fd"`/`"fdfind"`/`"rg"`），调用方（`kiana-tools` 的 `find`/`grep`）把它直接当作
/// `Command::new` 的程序名。本地检查只用 `Path::exists`：不验证可执行位，也不区分普通文件与
/// 目录；探测命令会同步启动子进程并等待退出。路径转成 `String` 时使用 `to_string_lossy`。
pub fn get_tool_path(tool: &str) -> Option<String> {
    get_tool_path_with(tool, &ToolsManagerEnv::default())
}

/// [`get_tool_path`] 的测试注入版本。
///
/// `env` 中为 `None` 的字段回退到真实环境（`host_platform`/`host_arch`/`TOOLS_DIR`/
/// `https://github.com`/真实子进程探测），因此它也是生产实现的入口。
#[doc(hidden)]
pub fn get_tool_path_with(tool: &str, env: &ToolsManagerEnv) -> Option<String> {
    let config = tool_config(tool)?;
    let platform = env.platform();
    let extension = if platform == "win32" { ".exe" } else { "" };
    let local_path = env
        .bin_dir()
        .join(format!("{}{extension}", config.binary_name));
    if local_path.exists() {
        return Some(local_path.to_string_lossy().into_owned());
    }

    match config.system_binary_names {
        Some(names) => names
            .iter()
            .find(|name| env.command_exists(name))
            .map(|name| (*name).to_string()),
        None => env
            .command_exists(config.binary_name)
            .then(|| config.binary_name.to_string()),
    }
}

/// `getLatestVersion(repo)`：从 GitHub release 页面的 302 `Location` 解析 tag。
///
/// 请求 `https://github.com/{repo}/releases/latest`，带 `get_kiana_user_agent(VERSION)` 生成的
/// 详细 UA（以 `kiana/<VERSION>` 开头，完整形如
/// `kiana/<version> (<platform>; <runtime>; <arch>)`；探测不到宿主运行时则为
/// `kiana/<version> (browser)`，见 `changelog::get_kiana_user_agent` 与
/// `tests/kiana_user_agent.rs` 的断言），使用不跟随重定向的专用 client
/// （[`crate::http_dispatcher`] 的 `redirect_manual_http_client`）。
/// 总预算是 `NETWORK_TIMEOUT_MS`（10 秒，重试共享）；`fetch_with_retry` 默认额外重试 2 次，
/// 传输错误与 408/425/429/500/502/503/504 可重试。超时 → `FetchError::Timeout`，其它传输失败
/// → `FetchError::Transport`（message 为 `fetch failed`，细节在 cause 链）。
///
/// 返回去 `v` 前缀后的版本（`v10.4.2` → `10.4.2`；`15.2.0` 原样）。错误文案逐字：
/// - `Failed to resolve latest {repo} release: HTTP {status} without redirect`
/// - `Failed to resolve latest {repo} release: unexpected redirect to {location}`
/// - `URI malformed`（`decodeURIComponent` 对非法 `%` 序列抛 `URIError`）
pub async fn get_latest_version(repo: &str) -> Result<String, FetchError> {
    get_latest_version_with_base(repo, DEFAULT_RELEASE_BASE).await
}

/// [`get_latest_version`] 的测试注入版本（wiremock 基地址）。
///
/// `base` 末尾的 `/` 会被去掉，只影响请求 URL；`Location` 的相对路径仍以
/// `https://github.com` 为基址解析（TS 的 `new URL(location, "https://github.com")`），
/// 且后续只使用 pathname，因此测试基地址不会改变解析结果。
#[doc(hidden)]
pub async fn get_latest_version_with_base(repo: &str, base: &str) -> Result<String, FetchError> {
    let url = format!("{}/{repo}/releases/latest", base.trim_end_matches('/'));
    let user_agent = get_kiana_user_agent(VERSION);
    let client = crate::http_dispatcher::redirect_manual_http_client();
    // `fetch_with_retry` 每次尝试都会重新调用 build：reqwest 的 `RequestBuilder` 是一次性的，
    // 所以闭包每次产生新 builder（client 用 Arc 克隆，UA 在闭包里 clone）。
    let request = {
        let client = client.clone();
        let url = url.clone();
        move || client.get(&url).header("User-Agent", user_agent.clone())
    };
    let response = fetch_with_retry(
        request,
        FetchRetryOptions {
            timeout_ms: Some(NETWORK_TIMEOUT_MS),
            ..FetchRetryOptions::default()
        },
    )
    .await?;

    // 可重试状态码在额度耗尽后会被原样返回（`fetch_with_retry` 不把它转成错误），
    // 因此这里拿到的是最终响应，可能是 4xx/5xx。
    // 只有状态与头有用；丢弃 body 以复用连接（TS：`await response.body?.cancel()`）。
    let status = response.status().as_u16();
    let location = if (300..400).contains(&status) {
        response
            .headers()
            .get("location")
            .map(|value| latin1_string(value.as_bytes()))
            // `if (!location)`：空串在 JS 里是假值 → 走 “without redirect” 分支。
            .filter(|location| !location.is_empty())
    } else {
        None
    };
    drop(response);

    let Some(location) = location else {
        return Err(FetchError::transport(
            format!("Failed to resolve latest {repo} release: HTTP {status} without redirect"),
            None,
        ));
    };

    // `new URL(location, "https://github.com")`：相对重定向解析为绝对 URL。
    // 基址固定为 github.com（不是入参 `base`）；后面只取 pathname。
    let parsed = match url::Url::parse(&location) {
        Ok(url) => url,
        Err(_) => {
            let base = url::Url::parse("https://github.com").expect("constant is a valid URL");
            match base.join(&location) {
                Ok(url) => url,
                // Node 的 `new URL()` 失败时抛 `TypeError: Invalid URL`。
                Err(_) => return Err(FetchError::transport("Invalid URL", None)),
            }
        }
    };

    let pathname = parsed.path();
    let tag = pathname.rsplit('/').next().unwrap_or("");
    // 与 TS 相同：只检查原始 `Location` 是否包含 `/releases/tag/`，不校验重定向目标的主机；
    // pathname 以 `/` 结尾时 tag 为空串，同样走 unexpected redirect。
    if tag.is_empty() || !location.contains("/releases/tag/") {
        return Err(FetchError::transport(
            format!("Failed to resolve latest {repo} release: unexpected redirect to {location}"),
            None,
        ));
    }

    // `decodeURIComponent(tag)`：非法百分号序列抛 `URIError: URI malformed`。
    let decoded =
        decode_uri_component(tag).map_err(|_| FetchError::transport("URI malformed", None))?;
    Ok(decoded.strip_prefix('v').unwrap_or(&decoded).to_string())
}

/// ECMAScript `decodeURIComponent`：解码 `%XX` 序列（含多字节 UTF-8），
/// 非法百分号序列或解码结果不是合法 UTF-8 时返回 `Err`（JS 抛 `URIError: URI malformed`）。
///
/// `percent_encoding::percent_decode_str` 会把孤立的 `%` 原样保留，与 JS 的抛错行为不同，
/// 因此这里手写严格版本。
///
/// 与 `decodeURIComponent` 一致：`+` 不转成空格（那是表单编码的规则），未转义的字节原样保留；
/// 先在字节层解码、最后整体做 UTF-8 校验，因此 `%E4%B8%AD` → `中`。
fn decode_uri_component(value: &str) -> Result<String, ()> {
    let bytes = value.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            // 末尾不足两个十六进制字符（`%`、`%A`）→ URI malformed。
            if index + 2 >= bytes.len() {
                return Err(());
            }
            let high = hex_digit(bytes[index + 1]).ok_or(())?;
            let low = hex_digit(bytes[index + 2]).ok_or(())?;
            decoded.push(high * 16 + low);
            index += 3;
        } else {
            decoded.push(bytes[index]);
            index += 1;
        }
    }
    // JS 对解码后的非法 UTF-8 序列（含孤立代理项）同样抛 `URIError`。
    String::from_utf8(decoded).map_err(|_| ())
}

fn hex_digit(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

/// Node 的 HTTP 头值解码：每个字节 0x80–0xFF → U+0080–U+00FF（latin1），不是 UTF-8。
fn latin1_string(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| *byte as char).collect()
}

/// 下载/解压阶段的错误。
///
/// 之所以区分三类，是因为 `ensureTool` 的 warning 要按 TS 的 cause 链规则组装文案
/// （[`DownloadError::message_chain`]），而不是简单地对错误做 `to_string`：
/// `Message` 是普通 `Error`，`Terminated` 是 undici 的 body `TypeError`，`Fetch` 保留
/// `fetchWithRetry` 的 cause 链。
enum DownloadError {
    /// 普通 `Error`（消息逐字）。
    Message(String),
    /// 响应体在传输中断（undici 的 `TypeError("terminated", { cause })`）。
    ///
    /// `cause` 是 undici 对应错误的文案（见 [`body_termination_cause`]）；没有可映射的
    /// 底层原因时为 `None`（与 TS 的 `terminated` 单条消息一致）。
    Terminated(Option<String>),
    /// `fetchWithRetry` 的失败（`error.cause` 链保留）。
    Fetch(FetchError),
}

impl DownloadError {
    /// TS `ensureTool` 的 cause 链遍历：e.message → e.cause.message → …（最多 5 层、去重）。
    ///
    /// 顺序保持“自身在前、cause 在后”；空串不跳过（与 TS 相同），因此
    /// `Transport { message: "fetch failed", cause: [""] }` 会得到尾随 `": "` 的文案。
    /// 去重按首次出现保留，最后用 `": "` 连接。
    fn message_chain(&self) -> String {
        let mut messages: Vec<String> = Vec::new();
        match self {
            Self::Message(message) => messages.push(message.clone()),
            Self::Terminated(cause) => {
                messages.push("terminated".to_string());
                if let Some(cause) = cause {
                    messages.push(cause.clone());
                }
            }
            Self::Fetch(FetchError::Transport { message, cause }) => {
                messages.push(message.clone());
                if let Some(causes) = cause {
                    messages.extend(causes.iter().map(|cause| cause.message.clone()));
                }
            }
            Self::Fetch(error) => messages.push(error.to_string()),
        }
        let mut unique: Vec<String> = Vec::new();
        for message in messages {
            if !unique.contains(&message) {
                unique.push(message);
            }
        }
        unique.truncate(5);
        unique.join(": ")
    }
}

/// undici 在 `content-length` 与实际字节数不符时的 cause 文案（与版本检查共用
/// [`crate::management_http::body_termination_cause`]）。
///
/// undici 的 body 读取失败统一是 `TypeError("terminated", { cause })`：
/// - `content-length` 截断 → `ResponseContentLengthMismatchError`（文案与 code 见
///   `management_http`）。
/// - 其它 body 错误（chunked 解析、压缩等）的 cause 取决于 llhttp 的内部状态，`reqwest`
///   无法一一对应；这里返回 `None`，保留 TS 的 `terminated` 单条消息。
fn body_termination_cause(error: &reqwest::Error) -> Option<String> {
    crate::management_http::body_termination_cause(error).map(|cause| cause.message)
}

/// `downloadFile(url, dest)`：流式写文件；`timeoutMs` 覆盖整个下载（含 body）。
///
/// 走进程级共享 client（跟随重定向），请求不带自定义头，因此带的是 undici 默认头
/// （见模块文档的已知差异）。两层超时都映射为 `FetchError::Timeout`，其 `Display` 是
/// `The operation was aborted due to timeout`，与 TS 的 `AbortError` 文案一致。
///
/// 目标文件在确认响应成功后才创建（已存在则截断）；body 中途失败或 future 被丢弃时会留下
/// 部分文件——`download_tool` 的清理只覆盖解压阶段，不覆盖下载失败。
async fn download_file(url: &str, dest: &Path) -> Result<(), DownloadError> {
    let client = shared_http_client();
    // 与版本查询相同：build 每次尝试都会被重新调用，闭包按值捕获 client/url 的克隆。
    let request = {
        let client = client.clone();
        let url = url.to_string();
        move || client.get(&url)
    };
    let deadline = tokio::time::Instant::now() + Duration::from_millis(DOWNLOAD_TIMEOUT_MS);
    match tokio::time::timeout_at(deadline, download_file_inner(request, url, dest)).await {
        Ok(result) => result,
        Err(_) => Err(DownloadError::Fetch(FetchError::Timeout)),
    }
}

async fn download_file_inner<F>(request: F, url: &str, dest: &Path) -> Result<(), DownloadError>
where
    F: Fn() -> reqwest::RequestBuilder,
{
    let mut response = fetch_with_retry(
        request,
        FetchRetryOptions {
            timeout_ms: Some(DOWNLOAD_TIMEOUT_MS),
            ..FetchRetryOptions::default()
        },
    )
    .await
    .map_err(DownloadError::Fetch)?;
    let status = response.status().as_u16();
    // 非 2xx 直接失败；此时目标文件还没创建（已存在的旧归档不会被截断）。
    if !response.status().is_success() {
        return Err(DownloadError::Message(format!(
            "Download failed with HTTP {status}: {url}"
        )));
    }
    // 这些是 Fetch 规范里“body 为 null”的状态；101/103/304 已在上面被 `is_success` 挡掉，
    // 实际能走到这里的是 204/205。
    if matches!(status, 101 | 103 | 204 | 205 | 304) {
        // fetch 的 null body 状态；TS 的 `if (!response.body)`。
        return Err(DownloadError::Message("No response body".to_string()));
    }

    let mut file = tokio::fs::File::create(dest)
        .await
        .map_err(|error| DownloadError::Message(error.to_string()))?;
    // 逐块转发，避免把整个归档读进内存；写入失败与读取失败都直接结束本次下载。
    loop {
        match response.chunk().await {
            Ok(Some(chunk)) => file
                .write_all(&chunk)
                .await
                .map_err(|error| DownloadError::Message(error.to_string()))?,
            Ok(None) => break,
            // TS 的 `AbortSignal.timeout` 覆盖 body；`reqwest` 的请求级总超时会在同一
            // deadline 中止 body 读取（`is_timeout()`），对应 `AbortError`
            // （`FetchError::Timeout`）。
            Err(error) if error.is_timeout() => {
                return Err(DownloadError::Fetch(FetchError::Timeout));
            }
            // Node 的 body reader 在传输截断时抛 `TypeError: terminated`（cause 映射见
            // `body_termination_cause`）。
            Err(error) => return Err(DownloadError::Terminated(body_termination_cause(&error))),
        }
    }
    // flush 等待所有已排队的写完成，把错误在返回成功前暴露出来。
    file.flush()
        .await
        .map_err(|error| DownloadError::Message(error.to_string()))
}

/// `formatSpawnFailure(result)`：`stderr.trim()` → `stdout.trim()` →
/// `exit status {status ?? "unknown"}`（启动失败由 [`format_spawn_sync_error`] 负责）。
///
/// trim 用 `kiana_json::js_trim`（JS `String.prototype.trim` 的空白集合），而不是 `str::trim`，
/// 以保证输出与既有文案逐字一致。
fn format_spawn_failure(output: &std::process::Output) -> String {
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stderr = kiana_json::js_trim(&stderr);
    if !stderr.is_empty() {
        return stderr.to_string();
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stdout = kiana_json::js_trim(&stdout);
    if !stdout.is_empty() {
        return stdout.to_string();
    }
    match output.status.code() {
        Some(code) => format!("exit status {code}"),
        None => "exit status unknown".to_string(),
    }
}

/// Node `spawnSync(command, args)` 启动失败时的 `error.message`：`spawnSync <command> <errno>`。
///
/// `libuv` 把 OS errno 命名成 `ENOENT`/`EACCES` 等字符串；映射不到的错误退回 io 文案
/// （极端情况，Node 侧同样会带一个未知 errno 名）。
///
/// 映射依据是 `std::io::ErrorKind` 而不是原始 errno 数字，因此只覆盖常见 POSIX 名称；无法归类
/// 时输出 `spawnSync <command> <io Display>`（可能含 `(os error N)`），不保证完全还原 libuv 的 errno 文案。
fn format_spawn_sync_error(command: &str, error: &std::io::Error) -> String {
    let code = match error.kind() {
        std::io::ErrorKind::NotFound => Some("ENOENT"),
        std::io::ErrorKind::PermissionDenied => Some("EACCES"),
        std::io::ErrorKind::InvalidInput => Some("EINVAL"),
        std::io::ErrorKind::ExecutableFileBusy => Some("ETXTBSY"),
        std::io::ErrorKind::OutOfMemory => Some("ENOMEM"),
        std::io::ErrorKind::ArgumentListTooLong => Some("E2BIG"),
        _ => None,
    };
    match code {
        Some(code) => format!("spawnSync {command} {code}"),
        None => format!("spawnSync {command} {error}"),
    }
}

/// `runExtractionCommand(command, args)`：成功（启动且退出码 0）→ `None`；
/// 否则 `` `${command}: ${formatSpawnFailure(result)}` ``。
///
/// 同步执行（`std::process`）并等待子进程退出，会阻塞当前运行时线程；stdout/stderr 被捕获
/// 而不是转发到当前进程的终端，失败时用于拼文案。成功判定只看退出码 0，不看输出内容。命令名
/// 在错误文案里经 `to_string_lossy` 转换（Windows 路径可能非 UTF-8）。
fn run_extraction_command(command: &OsStr, args: &[&OsStr]) -> Option<String> {
    let display = command.to_string_lossy();
    match Command::new(command)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
    {
        Ok(output) => {
            if output.status.success() {
                None
            } else {
                Some(format!("{display}: {}", format_spawn_failure(&output)))
            }
        }
        Err(error) => Some(format!(
            "{display}: {}",
            format_spawn_sync_error(&display, &error)
        )),
    }
}

/// `extractTarGzArchive(archive, dir, asset)`：`tar xzf <archive> -C <dir>`。
fn extract_tar_gz_archive(
    archive_path: &Path,
    extract_dir: &Path,
    asset_name: &str,
) -> Result<(), DownloadError> {
    let failure = run_extraction_command(
        OsStr::new("tar"),
        &[
            OsStr::new("xzf"),
            archive_path.as_os_str(),
            OsStr::new("-C"),
            extract_dir.as_os_str(),
        ],
    );
    match failure {
        Some(failure) => Err(DownloadError::Message(format!(
            "Failed to extract {asset_name}: {failure}"
        ))),
        None => Ok(()),
    }
}

/// `getWindowsTarCommand()`：`%SystemRoot%\System32\tar.exe` 存在则用它，否则 `tar.exe`。
///
/// 优先 System32 的 bsdtar：PATH 上更靠前的 `tar` 可能是 Git Bash 的 GNU tar，它不能解 zip。
/// 回退值是不带路径的命令名，由 `Command::new` 通过 PATH 解析。
fn get_windows_tar_command() -> PathBuf {
    if let Some(system_root) = std::env::var_os("SystemRoot") {
        let system_tar = PathBuf::from(system_root).join("System32").join("tar.exe");
        if system_tar.exists() {
            return system_tar;
        }
    }
    PathBuf::from("tar.exe")
}

/// PowerShell `Expand-Archive` 脚本（逐字）。
const POWERSHELL_EXPAND_ARCHIVE_SCRIPT: &str = "& { param($archive, $destination) $ErrorActionPreference = 'Stop'; Expand-Archive -LiteralPath $archive -DestinationPath $destination -Force }";

/// `extractZipArchive(archive, dir, asset)`。
///
/// 按平台依次尝试两种命令，前一个失败才尝试下一个；所有失败信息按尝试顺序用 `"; "` 连接后
/// 放在 `Failed to extract {asset}: ...` 里（与 TS 的拼接方式一致），因此两个命令都不可用时
/// 用户能同时看到两段原因。成功（退出码 0）立即返回，剩余命令不再执行。
fn extract_zip_archive(
    archive_path: &Path,
    extract_dir: &Path,
    asset_name: &str,
    platform: &str,
) -> Result<(), DownloadError> {
    let mut failures: Vec<String> = Vec::new();

    if platform == "win32" {
        // Windows 自带 bsdtar（tar.exe）支持 zip；优先 System32 版本（Git Bash 的 GNU tar 不行）。
        let windows_tar = get_windows_tar_command();
        let tar_failure = run_extraction_command(
            windows_tar.as_os_str(),
            &[
                OsStr::new("xf"),
                archive_path.as_os_str(),
                OsStr::new("-C"),
                extract_dir.as_os_str(),
            ],
        );
        if let Some(failure) = tar_failure {
            failures.push(failure);
        } else {
            return Ok(());
        }

        // bsdtar 不可用或解压失败时，回退到 PowerShell 的 Expand-Archive（脚本见常量）。
        let powershell_failure = run_extraction_command(
            OsStr::new("powershell.exe"),
            &[
                OsStr::new("-NoLogo"),
                OsStr::new("-NoProfile"),
                OsStr::new("-NonInteractive"),
                OsStr::new("-ExecutionPolicy"),
                OsStr::new("Bypass"),
                OsStr::new("-Command"),
                OsStr::new(POWERSHELL_EXPAND_ARCHIVE_SCRIPT),
                archive_path.as_os_str(),
                extract_dir.as_os_str(),
            ],
        );
        if let Some(failure) = powershell_failure {
            failures.push(failure);
        } else {
            return Ok(());
        }
    } else {
        // 非 Windows：先 `unzip -q`，失败再 `tar xf`（TS 的回退顺序；tar 能否解 zip 取决于
        // 本机实现）。
        let unzip_failure = run_extraction_command(
            OsStr::new("unzip"),
            &[
                OsStr::new("-q"),
                archive_path.as_os_str(),
                OsStr::new("-d"),
                extract_dir.as_os_str(),
            ],
        );
        if let Some(failure) = unzip_failure {
            failures.push(failure);
        } else {
            return Ok(());
        }

        let tar_failure = run_extraction_command(
            OsStr::new("tar"),
            &[
                OsStr::new("xf"),
                archive_path.as_os_str(),
                OsStr::new("-C"),
                extract_dir.as_os_str(),
            ],
        );
        if let Some(failure) = tar_failure {
            failures.push(failure);
        } else {
            return Ok(());
        }
    }

    Err(DownloadError::Message(format!(
        "Failed to extract {asset_name}: {}",
        failures.join("; ")
    )))
}

/// `findBinaryRecursively(rootDir, binaryFileName)`：显式栈 DFS，返回首个同名文件。
///
/// 与 TS 一样用显式栈迭代（不递归）。目录项顺序来自 `read_dir`，取决于文件系统，因此归档里
/// 有多个同名文件时返回哪一个没有保证——调用方先试两个已知路径，这里只是兜底。只看普通文件；
/// 符号链接既不会被当作文件，也不会被当作目录继续下钻（`DirEntry::file_type` 不跟随链接）。
fn find_binary_recursively(
    root_dir: &Path,
    binary_file_name: &str,
) -> Result<Option<PathBuf>, DownloadError> {
    let mut stack = vec![root_dir.to_path_buf()];
    while let Some(current_dir) = stack.pop() {
        let entries = std::fs::read_dir(&current_dir)
            .map_err(|error| DownloadError::Message(error.to_string()))?;
        for entry in entries {
            let entry = entry.map_err(|error| DownloadError::Message(error.to_string()))?;
            let full_path = current_dir.join(entry.file_name());
            let file_type = entry
                .file_type()
                .map_err(|error| DownloadError::Message(error.to_string()))?;
            if file_type.is_file() && entry.file_name() == OsStr::new(binary_file_name) {
                return Ok(Some(full_path));
            }
            if file_type.is_dir() {
                stack.push(full_path);
            }
        }
    }
    Ok(None)
}

/// `Date.now()`。
///
/// 只用于临时目录名；系统时钟早于 UNIX_EPOCH 时返回 0（JS 会得到负数，正常机器上不会发生）。
fn now_millis() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or(0)
}

/// `Math.random().toString(36).slice(2, 10)` 的等价物：8 位 base36（只要求唯一）。
fn random_base36_suffix() -> String {
    const DIGITS: &[u8; 36] = b"0123456789abcdefghijklmnopqrstuvwxyz";
    let uuid = uuid::Uuid::new_v4();
    uuid.as_bytes()
        .iter()
        .take(8)
        .map(|byte| DIGITS[(*byte as usize) % 36] as char)
        .collect()
}

/// `extract_tmp_${binaryName}_${pid}_${Date.now()}_${random}`（唯一目录，避免 fd/rg 并发竞争）。
fn extract_temp_dir_name(binary_name: &str) -> String {
    format!(
        "extract_tmp_{binary_name}_{}_{}_{}",
        std::process::id(),
        now_millis(),
        random_base36_suffix()
    )
}

/// `downloadTool(tool)`：下载 → 解压 → 安装到 `<bin>/<binary>`，返回二进制路径。
///
/// 前置条件：`ensure_tool` 已确认工具名、平台/架构受支持并发出 “Downloading...” 状态；本函数
/// 不再回调。成功时二进制位于 `<bin_dir>/<binary>(.exe)`，非 win32 的 unix 目标会被设为
/// `0o755`。错误形状：版本查询/下载的传输失败是 `DownloadError::Fetch`；body 截断是
/// `DownloadError::Terminated`（映射 undici 的 `terminated`）；解压与安装是普通
/// `DownloadError::Message`（`std::io::Error` 的 `Display`，见模块文档的已知差异）。
///
/// 清理只在进入解压阶段后执行：版本查询失败、下载端点返回非 2xx 或连接失败都发生在
/// `download_file_inner` 创建目标文件之前，会提前返回且不会留下归档；只有失败发生在
/// `tokio::fs::File::create` 之后（body 传输中断、写盘失败等）时才可能留下写了一半的归档
/// （TS 的 `downloadFile` 同样位于 try/finally 之前）。
async fn download_tool(tool: &str, env: &ToolsManagerEnv) -> Result<String, DownloadError> {
    // 工具名在 `ensure_tool` 已校验；这里是与 TS `downloadTool` 相同的防御性重复检查。
    let config =
        tool_config(tool).ok_or_else(|| DownloadError::Message(format!("Unknown tool: {tool}")))?;
    let platform = env.platform();
    let architecture = env.arch();
    // 与 `ensure_tool` 的平台预检查重复：保证本函数单独调用时也不会拼出不存在的资产名。
    let target = get_release_target(&platform, &architecture).ok_or_else(|| {
        DownloadError::Message(format!("Unsupported platform: {platform}/{architecture}"))
    })?;

    // fd 在 darwin/x64 上固定 10.3.0（跳过版本查询）。
    let version = if tool == "fd" && platform == "darwin" && architecture == "x64" {
        "10.3.0".to_string()
    } else {
        get_latest_version_with_base(config.repo, &env.release_base())
            .await
            .map_err(DownloadError::Fetch)?
    };
    let asset_name = (config.asset_name)(&version, &target);

    let tools_dir = env.bin_dir();
    tokio::fs::create_dir_all(&tools_dir)
        .await
        .map_err(|error| DownloadError::Message(error.to_string()))?;

    let download_url = format!(
        "{}/{}/releases/download/{}{}/{}",
        env.release_base().trim_end_matches('/'),
        config.repo,
        config.tag_prefix,
        version,
        asset_name
    );
    let archive_path = tools_dir.join(&asset_name);
    let binary_extension = if platform == "win32" { ".exe" } else { "" };
    let binary_file_name = format!("{}{binary_extension}", config.binary_name);
    let binary_path = tools_dir.join(&binary_file_name);

    // 归档直接落在 bin 目录（TS 相同）：同一工具并发下载会写同一个 archive_path。
    download_file(&download_url, &archive_path).await?;

    let extract_dir = tools_dir.join(extract_temp_dir_name(config.binary_name));
    std::fs::create_dir_all(&extract_dir)
        .map_err(|error| DownloadError::Message(error.to_string()))?;

    // 解压/查找/安装放在闭包里，返回后无论成功失败都统一清理归档与临时目录。
    let result = (|| -> Result<(), DownloadError> {
        // 格式由资产名后缀决定；`get_release_target` 只会产出这两种后缀。
        if asset_name.ends_with(".tar.gz") {
            extract_tar_gz_archive(&archive_path, &extract_dir, &asset_name)?;
        } else if asset_name.ends_with(".zip") {
            extract_zip_archive(&archive_path, &extract_dir, &asset_name, &platform)?;
        } else {
            return Err(DownloadError::Message(format!(
                "Unsupported archive format: {asset_name}"
            )));
        }

        // 有些归档把文件放在根，有些放在版本化子目录。
        let stem = asset_name
            .strip_suffix(".tar.gz")
            .or_else(|| asset_name.strip_suffix(".zip"))
            .unwrap_or(&asset_name);
        let extracted_dir = extract_dir.join(stem);
        // 先试两个确定位置（版本化子目录 / 归档根），都没有才递归搜索，减少对目录遍历顺序的依赖。
        let candidates = [
            extracted_dir.join(&binary_file_name),
            extract_dir.join(&binary_file_name),
        ];
        let mut extracted_binary = candidates.into_iter().find(|candidate| candidate.exists());
        if extracted_binary.is_none() {
            extracted_binary = find_binary_recursively(&extract_dir, &binary_file_name)?;
        }

        let Some(extracted_binary) = extracted_binary else {
            return Err(DownloadError::Message(format!(
                "Binary not found in archive: expected {binary_file_name} under {}",
                extract_dir.to_string_lossy()
            )));
        };

        // 直接改名到最终位置（不复制内容）；重名替换语义由 `std::fs::rename` 所在平台决定。
        std::fs::rename(&extracted_binary, &binary_path)
            .map_err(|error| DownloadError::Message(error.to_string()))?;

        if platform != "win32" {
            // 非 win32 才设可执行位；实现只用 `cfg(unix)`，其它非 Windows 目标上这段为空。
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let permissions = std::fs::Permissions::from_mode(0o755);
                std::fs::set_permissions(&binary_path, permissions)
                    .map_err(|error| DownloadError::Message(error.to_string()))?;
            }
        }

        Ok(())
    })();

    // finally：清理归档与解压目录（忽略错误）。
    let _ = std::fs::remove_file(&archive_path);
    let _ = std::fs::remove_dir_all(&extract_dir);

    result?;
    Ok(binary_path.to_string_lossy().into_owned())
}

/// `ensureTool(tool, onStatus?)`：确保工具可用（必要时下载），返回路径或 `None`。
///
/// 状态文案逐字（见 `spec/ca-cli.md` §2.21.31 / §2.28.9）。
///
/// 返回 `Some(path)` 表示本地 bin 或 PATH 上已有可用命令（也可能是本次刚安装的）；`None`
/// 表示不可用——未知工具、离线、Android、平台/架构不支持、下载失败都会返回 `None`，本函数
/// 不向上抛错。回调是同步 `FnMut`：已有工具时不调用；未知工具在 `tool_config` 处直接
/// 返回 `None`、不发回调；离线、Android、平台/架构不支持这三个提前返回分支各发一次
/// warning；下载路径先发 “Downloading...” 的 info，再发成功 info 或失败 warning。
/// 已有命令时的返回值形状见 [`get_tool_path`]（完整路径或 PATH 上的命令名）；新安装的是
/// `<bin_dir>` 下的完整路径。
pub async fn ensure_tool(
    tool: &str,
    on_status: Option<&mut dyn FnMut(ToolStatus)>,
) -> Option<String> {
    let env = ToolsManagerEnv::default();
    ensure_tool_with(tool, on_status, &env).await
}

/// `ensureTool(tool)`（省略 onStatus）：返回的 future 是 `Send`。
///
/// TS 的 `ensureTool(tool)` 不传回调；Rust 为 `Send` 执行管线（`kiana-tools` 的 grep/find）
/// 提供这个无回调入口：`Option<&mut dyn FnMut>` 参数会让 async future 失去 `Send`，
/// 而回调在这里本来就不需要。
///
/// 返回值语义同 [`ensure_tool`]：`None` 表示不可用，只是没有状态输出。
pub async fn ensure_tool_without_status(tool: &str) -> Option<String> {
    let env = ToolsManagerEnv::default();
    ensure_tool_without_status_with(tool, &env).await
}

/// [`ensure_tool_without_status`] 的测试注入版本。
///
/// 内部用空回调调用共享实现，因此与 `on_status = None` 等价。
#[doc(hidden)]
pub async fn ensure_tool_without_status_with(tool: &str, env: &ToolsManagerEnv) -> Option<String> {
    let mut no_status = |_: ToolStatus| {};
    ensure_tool_inner(tool, env, &mut no_status).await
}

/// [`ensure_tool`] 的测试注入版本。
///
/// `on_status = None` 与传入一个空回调等价；返回值语义同 [`ensure_tool`]。
#[doc(hidden)]
pub async fn ensure_tool_with(
    tool: &str,
    on_status: Option<&mut dyn FnMut(ToolStatus)>,
    env: &ToolsManagerEnv,
) -> Option<String> {
    match on_status {
        Some(callback) => ensure_tool_inner(tool, env, &mut |status| callback(status)).await,
        None => {
            let mut no_status = |_: ToolStatus| {};
            ensure_tool_inner(tool, env, &mut no_status).await
        }
    }
}

/// `ensureTool` 的共享实现；`on_status` 是泛型 sink，让无回调版本保持 `Send`。
///
/// 检查顺序就是输出契约：已有工具 → 工具名合法 → 离线 → Android → 平台/架构支持 → 下载。
/// 提前返回的分支只发一条 warning（未知工具连回调都没有）；下载路径发两条状态（开始 + 结果）。
async fn ensure_tool_inner<F: FnMut(ToolStatus)>(
    tool: &str,
    env: &ToolsManagerEnv,
    on_status: &mut F,
) -> Option<String> {
    // 已有工具是唯一不产生状态输出的成功路径。
    if let Some(existing) = get_tool_path_with(tool, env) {
        return Some(existing);
    }

    // 未注册的工具：上面的查找已经失败，这里用 `?` 结束且不触发回调。
    let config = tool_config(tool)?;

    if is_offline_mode_enabled() {
        on_status(ToolStatus {
            kind: ToolStatusKind::Warning,
            message: format!(
                "{} not found. Offline mode enabled, skipping download.",
                config.name
            ),
        });
        return None;
    }

    // Android/Termux：Bionic libc 与 Linux 预编译二进制不兼容，必须用 pkg 安装。
    let platform = env.platform();
    let architecture = env.arch();
    if platform == "android" {
        let package = termux_package(tool).unwrap_or(tool);
        on_status(ToolStatus {
            kind: ToolStatusKind::Warning,
            message: format!(
                "{} not found. Install with: pkg install {package}",
                config.name
            ),
        });
        return None;
    }

    // 预编译资产只覆盖 darwin/linux/win32 的 x64/arm64（见 `get_release_target`）。
    if get_release_target(&platform, &architecture).is_none() {
        on_status(ToolStatus {
            kind: ToolStatusKind::Warning,
            message: format!(
                "{} not found. Automatic download is not supported on {platform}/{architecture}. Install {} with your system package manager.",
                config.name, config.name
            ),
        });
        return None;
    }

    // 进入下载流程：先报告开始，随后成功/失败各报告一次结果。
    on_status(ToolStatus {
        kind: ToolStatusKind::Info,
        message: format!("{} not found. Downloading...", config.name),
    });

    match download_tool(tool, env).await {
        Ok(path) => {
            on_status(ToolStatus {
                kind: ToolStatusKind::Info,
                message: format!("{} installed to {path}", config.name),
            });
            Some(path)
        }
        Err(error) => {
            // 保留 cause 链：fetch 失败的细节（DNS/TLS/超时）在 cause 里。
            on_status(ToolStatus {
                kind: ToolStatusKind::Warning,
                message: format!(
                    "Failed to download {}: {}",
                    config.name,
                    error.message_chain()
                ),
            });
            None
        }
    }
}

// 本模块的单元测试只覆盖 spawn 错误文案；下载/安装的集成测试在
// `crates/kiana-core/tests/tools_manager.rs`（wiremock + 注入 env）。
#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn formats_spawn_sync_errors_like_node() {
        // Node 26：`spawnSync <cmd> ENOENT` / `spawnSync <cmd> EACCES`。
        let missing = std::io::Error::from_raw_os_error(2);
        assert_eq!(
            format_spawn_sync_error("tar", &missing),
            "spawnSync tar ENOENT"
        );
        let denied = std::io::Error::from_raw_os_error(13);
        assert_eq!(
            format_spawn_sync_error("tar", &denied),
            "spawnSync tar EACCES"
        );
    }

    #[test]
    fn formats_spawn_failure_from_stderr_stdout_and_exit_status() {
        let output = std::process::Command::new("sh")
            .args(["-c", "printf '  boom  ' >&2; exit 3"])
            .output()
            .expect("run sh");
        assert_eq!(format_spawn_failure(&output), "boom");

        let output = std::process::Command::new("sh")
            .args(["-c", "printf 'out' ; exit 4"])
            .output()
            .expect("run sh");
        assert_eq!(format_spawn_failure(&output), "out");

        let output = std::process::Command::new("sh")
            .args(["-c", "exit 7"])
            .output()
            .expect("run sh");
        assert_eq!(format_spawn_failure(&output), "exit status 7");
    }
}
