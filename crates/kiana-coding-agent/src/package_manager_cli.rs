//! `package-manager-cli.ts`（1151 行）：install/remove/update/list/config（`kiana config` 入口）。
//!
//! 与 TS 的差异（全部是显式化/无法复刻项）：
//!
//! - TS 直接读 `process.cwd()` / `getAgentDir()` / `process.stdout` / `process.exitCode`；
//!   Rust 把 cwd、agent dir、输出通道放进 [`PackageCommandRuntimeOptions`]，退出码放进
//!   [`PackageCommandOutcome`]（库内不调用 `process::exit`）。
//! - `SettingsManager.drainErrors()` 的 TS 错误带 `stack` 并逐字打印；Rust 只保留
//!   `error.message`，因此 `reportSettingsErrors` 不打印 stack（记录为已知偏差）。
//! - `proper-lockfile` 用目录锁（`<managedRoot>/update.lock`，`realpath:false`，stale 10 s，
//!   `lockSync` 无重试，异步锁每 5 s 刷新 mtime）。这里用 `create_dir`/目录 mtime 复刻，
//!   心跳用 `filetime::set_file_mtime`（Windows 打开目录需要 `FILE_FLAG_BACKUP_SEMANTICS`，
//!   std 的 `File::open` 在 Windows 上无法刷新目录 mtime）；`onCompromised`（锁被外部改写时
//!   TS 会抛未捕获异常）没有对应物，心跳检测到锁目录消失后静默停止。
//! - `printSelfUpdateUnavailable` 的入口位置：TS 打印 `process.argv[1]`（Node 的入口脚本）。
//!   原生二进制没有脚本参数，这里用 `argv[0]`（用户实际调用的入口路径，不做符号链接解析，
//!   更接近 Node 的 `path.resolve(argv[1])`；`kiana_core::config` 内部用 `current_exe` 推导包
//!   目录，是另一处用途）。
//! - `handleConfigCommand` / `handlePackageCommand` 里 `createCommandSettingsManager` 与
//!   resolve 的早期失败：TS 没有 try/catch，Node 以未捕获异常（含 stack）退出 1；Rust 打印
//!   `Error: {message}` 并返回 exit code 1（栈没有对应物，与 `reportSettingsErrors` 的偏差同类）。
//!
//! TS 源：`packages/coding-agent/src/package-manager-cli.ts`、`spec/ca-cli.md` §2.9 / §4.1 /
//! §4.2 / §4.5 / §4.6。
//!
//! # 这个文件负责什么
//!
//! `kiana install|remove|uninstall|update|list` 与 `kiana config` 两条子命令的完整处理链：
//! 参数解析 → 按项目信任状态创建设置管理器 → 调用 `kiana-resources` 的包管理器完成
//! 安装/删除/更新/列出 → 输出与退出码；`update` 还包含自身更新（普通安装与 managed
//! 安装两条路径）。本模块**不结束进程**：所有结果都通过 [`PackageCommandOutcome`] 交回
//! `crate::app::main`，由后者决定退出码。
//!
//! # 执行流程
//!
//! `kiana` 启动时 `crate::app::main` 按固定顺序调用本模块：
//!
//! 1. [`cleanup_managed_install`]（启动期，清理上次中断的 managed 更新 staging）；
//! 2. [`handle_package_command`]（install / remove / uninstall / update / list）；
//! 3. 上一步未接管时再调用 [`handle_config_command`]（config）；
//! 4. 两者都返回 `handled == false` 才继续其它子命令。
//!
//! package 命令内部顺序：解析（[`parse_package_command`]）→ `--help` 短路 → 四类参数
//! 错误 → install/remove 的缺 source 检查 → `update --models` 短路 → 创建设置管理器并
//! 解析项目信任 → `-l` 的信任门禁 → 打印设置警告 → 构造 `DefaultPackageManager` →
//! 执行动作 → `flush()` 设置写队列 → 返回结果。config 命令另有 `-l` 的信任门禁，
//! 并在最后打开配置 TUI（`select_config`）。
//!
//! # 重要约束
//!
//! - 本模块自己的提示文本都经 [`PackageCommandRuntimeOptions::output`]（测试用
//!   `Output::capture()` 收集）；但子进程（`npm`、自更新命令）使用继承的 stdio 直接写
//!   终端，配置 TUI 也直接操作终端，这两类输出不经过 `Output`。
//! - 帮助文本被 `tests/golden/package_help/*.txt` 逐字锁定，改动文案会直接破坏测试。
//! - 设置写盘是异步队列：两个 handler 在返回前都调用 `SettingsManager::flush()`。不 flush
//!   的话调用方可能在 worker 写盘前结束进程，`await` 之后读到的文件内容会与 TS 不一致。
//! - 空字符串按 JS 假值处理（`js_truthy`）：`kiana install ""` 报缺少 source，
//!   `update ""` 视为没有位置参数，而不是操作名为空串的包。
//! - 各子命令的开关集合不同：`-l/--local` 只属于 install/remove，
//!   `--self/--extensions/--models/--all/--force/--extension` 只属于 update；用在别处会
//!   记进 `invalid_option`。
//! - `handled == true` 只表示命令被接管，不代表成功；成功与否看 `exit_code`。
//!
//! # managed 安装与目录锁
//!
//! `KIANA_MANAGED_INSTALL_ROOT` 存在且当前包目录位于 `<root>/releases` 之下时，
//! `kiana update --self` 走 `run_managed_self_update`（私有）：校验
//! `managed-install.json` → 获取 `<root>/update.lock` 目录锁 → 下载 release 的
//! `package.json`/`package-lock.json` 到 `staging/update-*` → `npm ci` → 冒烟测试
//! `<release>/node_modules/.bin/kiana --version` → 原子 rename 到 `releases/<version>`
//! → 写 `current-version` → 删除旧 release。任一步失败都不会切换 `current-version`，
//! 旧 release 保持不变，本次创建的 staging 目录会被删除。锁用“`create_dir` 原子创建 +
//! 目录 mtime 心跳（每 5 s，10 s 未刷新视为 stale）”复刻 `proper-lockfile` 的语义，细节见
//! `acquire_managed_lock_sync` 与常量 `MANAGED_LOCK_STALE_MS` / `MANAGED_LOCK_UPDATE_MS`。
//!
//! # 阅读顺序
//!
//! [`parse_package_command`]（参数模型）→ [`handle_package_command`]（主流程）→
//! `run_package_action`（四个动作）→ `run_managed_self_update`（managed 自更新状态机）。

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use indexmap::IndexSet;
use kiana_ai::utils::abort::AbortScope;
use kiana_config::models::runtime::{CreateModelRuntimeOptions, ModelRuntime, ModelsPathOption};
use kiana_config::settings::{SettingsManager, SettingsManagerCreateOptions};
use kiana_config::trust::manager::{ProjectTrustStore, has_trust_requiring_project_resources};
use kiana_config::trust::project_trust::{
    AppMode, ProjectTrustEmitter, ResolveProjectTrustedOptions, resolve_project_trusted,
};
use kiana_core::changelog::{
    VersionCheckOptions, format_version_check_error, get_kiana_user_agent,
    get_latest_kiana_release, is_newer_package_version,
};
use kiana_core::changelog::{
    cleanup_windows_self_update_quarantine, quarantine_windows_native_dependencies,
};
use kiana_core::child_process::{
    SpawnOptions, SpawnSyncOptions, StdioMode, spawn_process, spawn_process_sync,
    wait_for_child_process,
};
use kiana_core::config::{
    APP_NAME, InstallMethod, PACKAGE_NAME, SelfUpdateCommand, SelfUpdatePackageTarget, VERSION,
    detect_install_method, get_agent_dir, get_package_dir, get_self_update_command,
    get_self_update_unavailable_instruction,
};
use kiana_core::paths::{PathInputOptions, canonicalize_path, get_cwd_relative_path, resolve_path};
use kiana_core::text::strip_bom;
use kiana_ext_api::types::InlineExtension;
use kiana_resources::loader::warnings::{format_package_install_warning, is_builtin_extension};
use kiana_resources::loader::{DefaultResourceLoader, DefaultResourceLoaderOptions};
use kiana_resources::package::{
    DefaultPackageManager, InstallOptions, InstallScope, PackageManager, PackageManagerOptions,
};
use kiana_tui::components::markdown::{Markdown, MarkdownTheme, StyleFn};
use kiana_tui::tui::Component;
use regex::Regex;

use crate::cli::config_selector::{ConfigSelectorOptions, select_config};
use crate::cli::project_trust::{
    ProjectTrustEmitterAdapter, ProjectTrustOptions, create_project_trust_context_views,
};
use crate::cli::setup::{Output, Style, node_fs_error_message};

/// `PackageCommand`：`install` / `remove` / `update` / `list` 四个包管理子命令。
///
/// 由 [`parse_package_command`] 从 argv 首参数得到（`uninstall` 归一化为
/// [`PackageCommand::Remove`]），再由 [`handle_package_command`] 分发到对应动作与帮助文本。
/// [`PackageCommand::as_str`] 返回 TS 字面量，只用于用户可见文案，不参与解析。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PackageCommand {
    /// `install`。
    Install,
    /// `remove`（`uninstall` 别名）。
    Remove,
    /// `update`。
    Update,
    /// `list`。
    List,
}

impl PackageCommand {
    /// TS 的字面量：`"install"` / `"remove"` / `"update"` / `"list"`。
    ///
    /// `uninstall` 只是 remove 的输入别名，不会出现在输出里。
    pub fn as_str(self) -> &'static str {
        match self {
            PackageCommand::Install => "install",
            PackageCommand::Remove => "remove",
            PackageCommand::Update => "update",
            PackageCommand::List => "list",
        }
    }
}

/// `UpdateTarget`：`update` 本次要更新的范围。
///
/// 由 [`parse_package_command`] 根据 `--self`/`--extensions`/`--models`/`--all`/
/// `--extension` 与位置参数推导；`command` 不是 `update` 时该字段恒为 `None`。
/// `All` 表示“自身 + 全部已安装扩展”；`Extensions { source: None }` 表示全部已安装扩展，
/// `Some(src)` 只更新匹配 `src` 的包。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpdateTarget {
    /// `{type:"all"}`。
    All,
    /// `{type:"self"}`。
    SelfOnly,
    /// `{type:"extensions", source?}`。
    Extensions {
        /// 指定单个包时的源（来自 `--extension <source>` 或位置参数）；`None` 表示全部
        /// 已安装扩展。
        source: Option<String>,
    },
    /// `{type:"models"}`。
    Models,
}

/// `PackageCommandOptions`（内部解析结果）：`parse_package_command` 的输出（纯数据，
/// 不产生副作用）。
///
/// 解析器**不提前失败**：错误分别记录在 `invalid_option` / `invalid_argument` /
/// `missing_option_value` / `conflicting_options`，每个字段只保留第一个。调用方
/// [`handle_package_command`] 按 `invalid_option` → `missing_option_value` →
/// `invalid_argument` → `conflicting_options` 的顺序检查并打印用法；`help == true` 时
/// 跳过全部检查。
///
/// `source` 保留原始字符串（可能是空串）；需要“TS 真值”语义的地方由调用方判断空串，
/// 见 [`parse_package_command`]。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackageCommandOptions {
    /// 子命令。
    pub command: PackageCommand,
    /// 位置参数（源）；空串表示“提供了但按 JS 假值视为没有值”。
    pub source: Option<String>,
    /// update 的目标；`command == Update` 时恒为 `Some`（默认 `SelfOnly`），否则为 `None`。
    pub update_target: Option<UpdateTarget>,
    /// 是否打印 "Extensions are skipped" 提示（仅 update 未指定任何目标时置位）。
    pub show_extensions_skipped_note: bool,
    /// `-l/--local`。
    pub local: bool,
    /// `--force`。
    pub force: bool,
    /// `--approve` / `--no-approve`。
    pub project_trust_override: Option<bool>,
    /// `-h/--help`。
    pub help: bool,
    /// 第一个未知选项。
    pub invalid_option: Option<String>,
    /// 第一个多余位置参数。
    pub invalid_argument: Option<String>,
    /// 缺少值的选项。
    pub missing_option_value: Option<String>,
    /// 第一个冲突组合的文案。
    pub conflicting_options: Option<String>,
}

/// 托管安装器的 release API：`KIANA_INSTALLER_API_BASE` 优先，否则由
/// `KIANA_API_BASE_URL/api/installer/releases` 派生；两者都未配置时为 `None`。
///
/// 环境变量先 trim，空串按未配置处理；派生值再去掉末尾 `/`，便于直接拼接
/// `/{encode_uri_component(version)}`。`None` 时 managed 更新只能报“不可用”，没有其它
/// 下载源可回退。
fn installer_api_base() -> Option<String> {
    std::env::var(kiana_core::config::ENV_INSTALLER_API_BASE)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .or_else(|| kiana_core::config::get_api_url("api/installer/releases"))
        .map(|value| value.trim_end_matches('/').to_string())
}

/// `MANAGED_INSTALL_MARKER`。
const MANAGED_INSTALL_MARKER: &str = "managed-install.json";
/// `MANAGED_RELEASE_VERSION_RE`。
static MANAGED_RELEASE_VERSION_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^[0-9]+\.[0-9]+\.[0-9]+(?:-[0-9A-Za-z.-]+)?(?:\+[0-9A-Za-z.-]+)?$")
        .expect("managed release version regex is valid")
});

/// 目录锁的 stale 阈值（proper-lockfile 默认 10 s）。
const MANAGED_LOCK_STALE_MS: u128 = 10_000;
/// 目录锁 mtime 刷新间隔（proper-lockfile 默认 stale/2 = 5 s）。
const MANAGED_LOCK_UPDATE_MS: u64 = 5_000;

/// `PackageCommandRuntimeOptions`：Rust 显式化的进程上下文。
///
/// TS 版直接读 `process.cwd()`、`getAgentDir()`、`process.stdout/stderr`，库层既无法注入也
/// 无法并行测试，因此统一由此结构传入。应用入口（`crate::app::main`）直接构造该结构体，
/// 测试用 `Output::capture()` 加临时目录构造。
pub struct PackageCommandRuntimeOptions {
    /// `process.cwd()`。
    pub cwd: String,
    /// `getAgentDir()`。
    pub agent_dir: String,
    /// 内联扩展（`MainOptions.extensionFactories` + 内置扩展）。
    ///
    /// 只在需要向扩展询问项目信任时使用：`config` 与未给 `--approve/--no-approve` 的
    /// install/remove/list，在工作目录包含需要信任的资源时会加载它们；`update` 只读已
    /// 保存的信任记录，不加载扩展。
    pub extension_factories: Vec<InlineExtension>,
    /// 本模块提示文本的输出通道（帮助、错误、警告、进度）。子进程（npm、自更新命令）使用
    /// 继承的 stdio 直接写终端，不经过它。
    pub output: Output,
}

impl std::fmt::Debug for PackageCommandRuntimeOptions {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PackageCommandRuntimeOptions")
            .field("cwd", &self.cwd)
            .field("agent_dir", &self.agent_dir)
            .field("extension_factories", &self.extension_factories.len())
            .finish_non_exhaustive()
    }
}

impl Default for PackageCommandRuntimeOptions {
    fn default() -> Self {
        Self {
            cwd: process_cwd(),
            agent_dir: get_agent_dir().to_string_lossy().into_owned(),
            extension_factories: Vec::new(),
            output: Output::process(),
        }
    }
}

impl PackageCommandRuntimeOptions {
    /// 从进程默认值构造：保留传入的扩展工厂，其余字段等价于 `Default::default()`
    /// （`cwd` 取 [`std::env::current_dir`]，`agent_dir` 取 `get_agent_dir()`，`output` 写
    /// 真实进程通道）。仓库内当前没有调用方（应用入口直接构造结构体）。
    pub fn from_process(extension_factories: Vec<InlineExtension>) -> Self {
        Self {
            extension_factories,
            ..Self::default()
        }
    }
}

/// 命令处理结果：`handled` 对应 TS 的返回值，`exit_code` 对应 `process.exitCode`。
///
/// `handled == true` 只说明该子命令被本模块接管，**不代表成功**；成功与否看 `exit_code`
/// （0 成功，1 失败）。`crate::app::main` 在 `handled` 时立即以 `exit_code` 结束启动流程，
/// 不再尝试后续子命令。
///
/// TS 在配置 TUI 的退出键等路径上直接 `process.exit(0)`；库层不结束进程，只把该退出码
/// 放进这里返回。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PackageCommandOutcome {
    /// 是否处理了该命令。
    pub handled: bool,
    /// 进程退出码（0 = 正常；TS 里 `process.exit(0)` 也是 0）。
    pub exit_code: i32,
}

impl PackageCommandOutcome {
    /// 未处理（首参数不属于本命令集）；调用方应继续尝试其它子命令。
    pub const NOT_HANDLED: Self = Self {
        handled: false,
        exit_code: 0,
    };

    /// 处理成功（`exit_code == 0`）。
    pub const HANDLED: Self = Self {
        handled: true,
        exit_code: 0,
    };

    /// 处理但失败（`process.exitCode = 1`）：命令已被接管，具体错误文案由各失败路径打印。
    pub const FAILED: Self = Self {
        handled: true,
        exit_code: 1,
    };
}

fn process_cwd() -> String {
    std::env::current_dir()
        .map(|path| path.to_string_lossy().into_owned())
        .unwrap_or_else(|_| ".".to_string())
}

/// `path.resolve(value)`（不展开 `~`）。
fn js_path_resolve(value: &str) -> String {
    resolve_path(
        value,
        &process_cwd(),
        &PathInputOptions {
            expand_tilde: false,
            ..PathInputOptions::default()
        },
    )
}

/// `path.join`（字符串形式）。
fn join_path(parts: &[&str]) -> String {
    let mut buffer = PathBuf::new();
    for part in parts {
        buffer.push(part);
    }
    buffer.to_string_lossy().into_owned()
}

/// `encodeURIComponent(value)`。
///
/// 按 WHATWG 的保留集实现：`A-Z a-z 0-9 - _ . ! ~ * ' ( )` 原样保留，其余**字节**（UTF-8
/// 多字节字符逐字节）编码为 `%XX`（大写十六进制）。用于把版本号放进 installer release
/// URL 的路径段，因此 `+` 等 semver 元字符必须转义。函数不会失败。
pub fn encode_uri_component(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.as_bytes() {
        let ch = *byte as char;
        if ch.is_ascii_alphanumeric()
            || matches!(ch, '-' | '_' | '.' | '!' | '~' | '*' | '\'' | '(' | ')')
        {
            out.push(ch);
        } else {
            out.push('%');
            out.push_str(&format!("{byte:02X}"));
        }
    }
    out
}

// ---------------------------------------------------------------------------
// managed install
// ---------------------------------------------------------------------------

/// `getActiveManagedInstallRoot()`。
///
/// 返回 `Err` 对应 TS 抛出的 marker 错误；`Ok(None)` 表示当前不是 managed 安装。
///
/// 判定顺序：`KIANA_MANAGED_INSTALL_ROOT` 未设置/trim 后为空 → `Ok(None)`；否则要求
/// canonicalize 后的包目录位于 `<root>/releases` 之下（子进程会继承 launcher 的环境，
/// 这样不会把源码 checkout 或另一个 Kiana 安装误判成 managed），再要求
/// `<root>/managed-install.json` 是去 BOM 后可解析的 JSON，且 `kind ==
/// "kiana-managed-install"`、`schemaVersion` 数值上等于 1、`layout == "releases-v1"`。
/// 任一条件不满足返回带 marker 路径的 `Err`（启动期的 [`cleanup_managed_install`] 会
/// 吞掉这个错误，只有 `update --self` 会展示它）。
fn get_active_managed_install_root() -> Result<Option<String>, String> {
    let configured_root = std::env::var("KIANA_MANAGED_INSTALL_ROOT")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());
    let Some(configured_root) = configured_root else {
        return Ok(None);
    };

    let managed_root = js_path_resolve(&configured_root);
    let releases_dir = canonicalize_path(&join_path(&[&managed_root, "releases"]));
    // 子进程会继承 launcher 的环境：不要把自己启动的源码 checkout 或另一个 Kiana 安装
    // 误判成 managed 安装。
    let package_dir = canonicalize_path(&get_package_dir().to_string_lossy());
    if get_cwd_relative_path(&package_dir, &releases_dir).is_none() {
        return Ok(None);
    }

    let marker_path = join_path(&[&managed_root, MANAGED_INSTALL_MARKER]);
    let valid = std::fs::read(&marker_path)
        .ok()
        .and_then(|bytes| {
            let content = String::from_utf8_lossy(&bytes).into_owned();
            serde_json::from_str::<serde_json::Value>(&strip_bom(&content)).ok()
        })
        .is_some_and(|marker| {
            marker.get("kind").and_then(serde_json::Value::as_str) == Some("kiana-managed-install")
                && marker
                    .get("schemaVersion")
                    .and_then(serde_json::Value::as_f64)
                    == Some(1.0)
                && marker.get("layout").and_then(serde_json::Value::as_str) == Some("releases-v1")
        });
    if !valid {
        return Err(format!(
            "Managed install marker is missing or invalid: {marker_path}"
        ));
    }
    Ok(Some(managed_root))
}

/// `fetchInstallerArtifact(url, label)`：下载 managed release 的单个构建产物并返回文本。
///
/// 错误文案复刻 TS/undici：传输失败（连接、DNS 等）统一为 `"fetch failed"`，响应体读取
/// 失败为 `"terminated"`，HTTP 非 2xx 带上 `label`、URL 与状态码。`label` 只用于文案。
/// 请求带 `get_kiana_user_agent(VERSION)`。
async fn fetch_installer_artifact(url: &str, label: &str) -> Result<String, String> {
    let client = kiana_core::http_dispatcher::shared_http_client();
    let response = client
        .get(url)
        .header("User-Agent", get_kiana_user_agent(VERSION))
        .send()
        .await
        .map_err(|_| "fetch failed".to_string())?;
    if !response.status().is_success() {
        return Err(format!(
            "Could not download managed installer {label} from {url}: HTTP {}",
            response.status().as_u16()
        ));
    }
    response.text().await.map_err(|_| "terminated".to_string())
}

/// `runManagedNpmCi(stageDir)` 的 npm 参数（**【逐字】**）。
///
/// `--ignore-scripts` 禁止依赖执行安装脚本；`--omit=dev` 不装开发依赖；
/// `--include=optional` 保留可选依赖；其余四项关闭赞助提示与审计、限制日志级别并禁用
/// 进度条。这些参数是当前稳定契约，改动需同步测试。
fn managed_npm_ci_args() -> Vec<String> {
    [
        "ci",
        "--ignore-scripts",
        "--omit=dev",
        "--include=optional",
        "--no-fund",
        "--no-audit",
        "--loglevel=error",
        "--progress=false",
    ]
    .into_iter()
    .map(str::to_string)
    .collect()
}

/// `runManagedNpmCi(stageDir)`。
async fn run_managed_npm_ci(stage_dir: &str) -> Result<(), String> {
    // 没有 age-gate override：发布锁文件固定了所有包，npm 不会解析版本。
    let args = managed_npm_ci_args();
    // 继承 stdio：npm 的错误（`--loglevel=error`）直接出现在用户终端。cwd 固定为 staging
    // 目录，安装结果随后交给 verify 与 rename。
    let mut child = spawn_process(
        "npm",
        &args,
        &SpawnOptions {
            cwd: Some(PathBuf::from(stage_dir)),
            ..SpawnOptions::with_stdio(StdioMode::Inherit)
        },
    )
    .map_err(|error| error.to_string())?;
    let code = wait_for_child_process(&mut child)
        .await
        .map_err(|error| error.to_string())?;
    if code != Some(0) {
        let rendered = code.map_or_else(|| "unknown".to_string(), |code| code.to_string());
        return Err(format!(
            "npm {} exited with code {rendered}",
            args.join(" ")
        ));
    }
    Ok(())
}

/// `verifyManagedRelease(releaseDir, expectedVersion)`：冒烟测试一个已安装的 release。
///
/// 运行 `<releaseDir>/node_modules/.bin/kiana`（Windows 为 `kiana.cmd`）的 `--version`，
/// 要求退出码 0 且 stdout trim 后**逐字**等于 `expectedVersion`。错误原因优先取 spawn
/// 错误，其次取非空 stderr，最后退回退出码。该检查会真正执行待启用的代码，因此也覆盖
/// “文件存在但无法运行”的情况。
fn verify_managed_release(release_dir: &str, expected_version: &str) -> Result<(), String> {
    let bin_name = if cfg!(windows) {
        format!("{APP_NAME}.cmd")
    } else {
        APP_NAME.to_string()
    };
    let bin_path = join_path(&[release_dir, "node_modules", ".bin", &bin_name]);
    let result = spawn_process_sync(
        &bin_path,
        &["--version".to_string()],
        &SpawnSyncOptions {
            stdin: StdioMode::Ignore,
            stdout: StdioMode::Pipe,
            stderr: StdioMode::Pipe,
            ..SpawnSyncOptions::default()
        },
    );
    if result.error.is_some() || result.status != Some(0) {
        let reason = result
            .error
            .as_ref()
            .map(|error| error.to_string())
            .or_else(|| {
                let trimmed = result.stderr.trim();
                (!trimmed.is_empty()).then(|| trimmed.to_string())
            })
            .unwrap_or_else(|| {
                let status = result
                    .status
                    .map_or_else(|| "unknown".to_string(), |status| status.to_string());
                format!("exit code {status}")
            });
        return Err(format!(
            "Could not verify managed Kiana {expected_version}: {reason}"
        ));
    }
    let installed_version = result.stdout.trim();
    if installed_version != expected_version {
        return Err(format!(
            "Managed Kiana smoke test returned version {installed_version}; expected {expected_version}."
        ));
    }
    Ok(())
}

/// 一次 `Date.now()`。
fn now_millis() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or(0)
}

/// `activateManagedRelease(managedRoot, version)`。
fn activate_managed_release(managed_root: &str, version: &str) -> Result<(), String> {
    let current_path = join_path(&[managed_root, "current-version"]);
    let temporary_path = join_path(&[
        managed_root,
        &format!(
            "current-version.tmp.{}-{}",
            std::process::id(),
            now_millis()
        ),
    ]);
    let result = (|| {
        // 先写临时文件再 rename（而不是原地覆写）：避免 current-version 被读到写了一半的
        // 内容。临时名带 pid 与毫秒时间戳，避免重入时互相覆盖。
        std::fs::write(&temporary_path, format!("{version}\n")).map_err(|error| {
            node_fs_error_message(&error, "open", Some(Path::new(&temporary_path)), None)
        })?;
        std::fs::rename(&temporary_path, &current_path).map_err(|error| {
            node_fs_error_message(
                &error,
                "rename",
                Some(Path::new(&temporary_path)),
                Some(Path::new(&current_path)),
            )
        })
    })();
    // 失败也要清掉临时文件；清理错误与 `rmSync(..., {force:true})` 一样忽略。
    let _ = std::fs::remove_file(&temporary_path);
    result
}

/// 递归删除（`rmSync(..., {force:true, recursive:true})`），错误忽略。
///
/// 用 `symlink_metadata` 判断类型：符号链接只删除链接本身（不跟随），普通文件/目录分别
/// `remove_file`/`remove_dir_all`。目标不存在、权限不足等错误一律忽略——调用方用它做
/// “尽力清理”，不依赖删除结果。
fn remove_path_force(path: &Path) {
    let Ok(metadata) = std::fs::symlink_metadata(path) else {
        return;
    };
    if metadata.is_dir() {
        let _ = std::fs::remove_dir_all(path);
    } else {
        let _ = std::fs::remove_file(path);
    }
}

/// `pruneManagedReleases(managedRoot, activeVersion)`：删除 `releases/` 下不再使用的版本。
///
/// 保留当前激活版本与本进程的 `VERSION`（测试
/// `removes_older_managed_releases_after_an_update` 断言两者都保留），以及任何不匹配
/// semver 正则的条目（例如 `not-a-release`）。`read_dir` 顺序不保证，删除失败被忽略，
/// 函数无返回值。
fn prune_managed_releases(managed_root: &str, active_version: &str) {
    let releases_root = join_path(&[managed_root, "releases"]);
    let Ok(entries) = std::fs::read_dir(&releases_root) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name == active_version || name == VERSION || !MANAGED_RELEASE_VERSION_RE.is_match(&name)
        {
            continue;
        }
        remove_path_force(&entry.path());
    }
}

/// `cleanupManagedStaging(managedRoot)`：删除 `<root>/staging/update-*`。
///
/// 只处理以 `update-` 开头的条目（`mkdtemp_update` 的命名），其它残留不动；目录不存在时
/// 直接返回。错误忽略。调用方须持有 update.lock，避免删掉另一个更新进程正在写的 staging。
fn cleanup_managed_staging(managed_root: &str) {
    let staging_root = join_path(&[managed_root, "staging"]);
    let Ok(entries) = std::fs::read_dir(&staging_root) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with("update-") {
            remove_path_force(&entry.path());
        }
    }
}

// ---------------------------------------------------------------------------
// proper-lockfile 的目录锁（mkdir 语义 + filetime 刷新 mtime）
// ---------------------------------------------------------------------------

/// 目录锁获取失败的两类原因；错误文案由调用方决定（更新路径会转成 stderr）。
enum ManagedLockError {
    /// proper-lockfile 的 `ELOCKED`：锁存在且未过期。
    Locked,
    /// 目录锁操作失败；`syscall` 是 Node 错误文案里的系统调用名（`mkdir`/`stat`/`rmdir`）。
    Io {
        /// 底层错误。
        error: std::io::Error,
        /// Node 风格的 syscall 名。
        syscall: &'static str,
    },
}

/// 已获取的目录锁；drop（或显式 `release`）时停止心跳并删除锁目录。
///
/// `release` 直接 `remove_dir`，删除前不校验锁目录是否仍属于自己（没有 inode/标记）；
/// 若锁曾被外部判定 stale 并重建，这里可能误删新锁。该竞态未被测试覆盖。
struct ManagedLockGuard {
    lock_path: PathBuf,
    released: Arc<AtomicBool>,
    heartbeat: Option<tokio::task::JoinHandle<()>>,
}

impl ManagedLockGuard {
    fn release(&mut self) {
        // 先置位再 abort：心跳若正处于 exists()/touch 之间，被取消后不会再刷新；touch 与
        // remove_dir 的错误都被忽略（锁已被外部删除时 `NotFound` 属正常情况）。
        self.released.store(true, Ordering::SeqCst);
        if let Some(handle) = self.heartbeat.take() {
            handle.abort();
        }
        let _ = std::fs::remove_dir(&self.lock_path);
    }
}

impl Drop for ManagedLockGuard {
    fn drop(&mut self) {
        self.release();
    }
}

/// `mtime` 距现在是否超过 stale 阈值。
///
/// mtime 位于未来时（时钟回拨或被 `touch_lock` 设成 now+5 ms）`duration_since` 返回
/// `Err`，这里按 age = 0 处理，不会误判为 stale。`stat` 错误原样返回给调用方。
fn lock_is_stale(lock_path: &Path) -> std::io::Result<bool> {
    let metadata = std::fs::metadata(lock_path)?;
    let modified = metadata.modified()?;
    let age = SystemTime::now()
        .duration_since(modified)
        .map(|duration| duration.as_millis())
        .unwrap_or(0);
    Ok(age > MANAGED_LOCK_STALE_MS)
}

/// 把锁目录的 mtime 设为“现在 + 5 ms”（proper-lockfile 的 probe 行为）。
///
/// `fs.utimes(dir)` 必须在 Windows 上也能工作：以 `FILE_FLAG_BACKUP_SEMANTICS` 打开目录才
/// 能改它的时间戳，std 的 `File::open` 不支持目录，因此用 `filetime`（跨平台、直接按路径
/// 设置 mtime）。心跳同样走这里，保证 10 s stale 阈值内的活锁不会被并发更新误判为过期。
fn touch_lock(lock_path: &Path) {
    let timestamp =
        filetime::FileTime::from_system_time(SystemTime::now() + Duration::from_millis(5));
    let _ = filetime::set_file_mtime(lock_path, timestamp);
}

/// `acquireLock`：`mkdir` 原子创建；已存在时按 stale 判定（一次重试，跳过 stale 检查）。
///
/// 步骤与失败路径：
///
/// 1. `create_dir` 成功即持锁，并把 mtime 推到“现在 + 5 ms”（与 proper-lockfile 的 probe
///    一致，避免刚创建的锁立刻被别的进程按旧时间判成过期）。
/// 2. `AlreadyExists` 时先 `stat`：`NotFound` 说明锁刚被释放，直接重试（这次跳过 stale
///    判定）；其它 stat 错误按 syscall `stat` 返回（Node 文案里就是 `stat`，不是 `mkdir`）。
/// 3. mtime 超过 stale 阈值 → 删除锁目录后重试；`rmdir` 的 `NotFound` 表示别人已经删了，
///    同样直接重试。
/// 4. 仍未拿到锁 → `ManagedLockError::Locked`（对应 `ELOCKED`，调用方转成“另一个更新
///    正在运行”）。其它 IO 错误带 syscall 名返回，用于拼 Node 风格文案。
///
/// 这里没有“等待并重试”的循环：除 stale 清理后的那一次重试外，拿不到锁立即失败。
fn acquire_managed_lock_sync(lock_path: &Path) -> Result<(), ManagedLockError> {
    fn attempt(lock_path: &Path, check_stale: bool) -> Result<(), ManagedLockError> {
        match std::fs::create_dir(lock_path) {
            Ok(()) => {
                touch_lock(lock_path);
                Ok(())
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                // 先 stat：锁可能在 mkdir 失败后已被删除。
                let stale = if check_stale {
                    match lock_is_stale(lock_path) {
                        Ok(stale) => stale,
                        Err(stat_error) if stat_error.kind() == std::io::ErrorKind::NotFound => {
                            return attempt(lock_path, false);
                        }
                        // proper-lockfile 把 `fs.stat` 的错误直接交给回调：Node 文案里的 syscall
                        // 是 `stat`（不是 `mkdir`）。
                        Err(stat_error) => {
                            return Err(ManagedLockError::Io {
                                error: stat_error,
                                syscall: "stat",
                            });
                        }
                    }
                } else {
                    false
                };
                if !stale {
                    return Err(ManagedLockError::Locked);
                }
                // 过期：删除锁目录后立即重试（不再做 stale 检查）。
                match std::fs::remove_dir(lock_path) {
                    Ok(()) => attempt(lock_path, false),
                    Err(remove_error) if remove_error.kind() == std::io::ErrorKind::NotFound => {
                        attempt(lock_path, false)
                    }
                    Err(remove_error) => Err(ManagedLockError::Io {
                        error: remove_error,
                        syscall: "rmdir",
                    }),
                }
            }
            Err(error) => Err(ManagedLockError::Io {
                error,
                syscall: "mkdir",
            }),
        }
    }
    attempt(lock_path, true)
}

/// 异步获取 managed 锁：成功后启动 mtime 心跳。
///
/// 心跳是一个 `tokio` 任务：每 `MANAGED_LOCK_UPDATE_MS` 醒来一次，若已被释放或锁目录
/// 消失则退出，否则刷新 mtime。它由 `ManagedLockGuard::release` 通过 `abort` 取消；
/// 心跳与 release 之间没有互斥，二者同时操作时的 `touch`/`remove_dir` 错误都被忽略。
/// 锁目录被外部删除时心跳静默停止（TS 的 `onCompromised` 会抛异常，这里的偏差已在模块
/// 文档列出）。
async fn acquire_managed_lock(managed_root: &str) -> Result<ManagedLockGuard, ManagedLockError> {
    let lock_path = PathBuf::from(join_path(&[managed_root, "update.lock"]));
    acquire_managed_lock_sync(&lock_path)?;
    let released = Arc::new(AtomicBool::new(false));
    let heartbeat = {
        let released = Arc::clone(&released);
        let lock_path = lock_path.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_millis(MANAGED_LOCK_UPDATE_MS)).await;
                if released.load(Ordering::SeqCst) {
                    return;
                }
                if !lock_path.exists() {
                    // TS 的 onCompromised 会抛未捕获异常；Rust 侧直接停止心跳。
                    return;
                }
                touch_lock(&lock_path);
            }
        })
    };
    Ok(ManagedLockGuard {
        lock_path,
        released,
        heartbeat: Some(heartbeat),
    })
}

/// `cleanupManagedInstall()`：启动期清理上次中断的 managed 更新。
///
/// 由 `crate::app::main` 在 package/config 命令之前调用。行为：
///
/// 1. 未设置 `KIANA_MANAGED_INSTALL_ROOT`、当前包目录不在 `<root>/releases/` 下、或
///    `managed-install.json` 缺失/不合法时**静默返回**（启动期清理失败不打印、不改变
///    退出码；测试 `cleanup_managed_install_is_silent_for_invalid_markers` 锁定该行为）；
/// 2. 尝试获取 `<root>/update.lock`（同步版、无心跳）；锁被另一个更新持有（ELOCKED）
///    或发生 IO 错误时同样静默返回，避免删掉别人正在写的 staging；
/// 3. 删除 `<root>/staging/update-*`（只匹配该前缀），随后删除自己创建的锁目录。
///
/// 错误全部被吞掉：返回 `()`，不打印、不改变退出码。
pub fn cleanup_managed_install() {
    let Ok(Some(managed_root)) = get_active_managed_install_root() else {
        return;
    };
    let lock_path = PathBuf::from(join_path(&[&managed_root, "update.lock"]));
    let Ok(()) = acquire_managed_lock_sync(&lock_path) else {
        // 另一个更新持有 staging 目录，或清理不可用。
        return;
    };
    cleanup_managed_staging(&managed_root);
    let _ = std::fs::remove_dir(&lock_path);
}

/// `mkdtempSync(join(root, "update-"))`：原子创建唯一目录（6 位 base36 后缀）。
///
/// 后缀由“毫秒时间戳 ^ pid”播种的 LCG 生成，共 6 位；`create_dir` 的原子性负责最终去重，
/// `AlreadyExists` 时换一个后缀重试，最多 1000 次。第 0 次在创建前多做一次 `exists` 检查
/// （后续轮次没有）。1000 次仍失败返回 `AlreadyExists`，调用方按 Node `mkdtemp` 的错误
/// 文案处理。
fn mkdtemp_update(staging_root: &str) -> std::io::Result<String> {
    const ALPHABET: &[u8] = b"0123456789abcdefghijklmnopqrstuvwxyz";
    let seed = now_millis() as u64 ^ ((std::process::id() as u64) << 32);
    let mut state = seed | 1;
    for attempt in 0..1000u64 {
        let mut suffix = String::with_capacity(6);
        for _ in 0..6 {
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            suffix.push(ALPHABET[((state >> 33) % 36) as usize] as char);
        }
        let path = join_path(&[staging_root, &format!("update-{suffix}")]);
        if attempt == 0 && Path::new(&path).exists() {
            continue;
        }
        match std::fs::create_dir(&path) {
            Ok(()) => return Ok(path),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::AlreadyExists,
        "mkdtemp: could not create a unique directory",
    ))
}

/// `runManagedSelfUpdate(managedRoot, version)`。
///
/// 状态机（任一步失败 → 删除本次创建的 staging、释放锁、返回 Err；`current-version` 不会
/// 被切换，旧 release 保持不变。若失败发生在 rename 之后（写 `current-version` 时），新的
/// release 目录会保留，下次更新走第 4 步的复用路径）：
///
/// 1. 版本必须匹配 `MANAGED_RELEASE_VERSION_RE`——它同时是 `releases/<version>` 的目录名，
///    正则排除了 `/` 与 `..` 之类会逃离 releases 目录的值；
/// 2. 获取 `update.lock`：被占用返回“另一个更新正在运行”，IO 错误转 Node 文案；
/// 3. 清空上次的 staging；
/// 4. 若 `releases/<version>` 已存在：冒烟测试后直接激活（幂等修复，不重新下载）；
/// 5. 否则创建 `staging/update-*`，并发下载 `package.json` 与 `package-lock.json`，
///    执行 `npm ci`、冒烟测试，再 `rename` 进 `releases/<version>`（rename 成功后才算发布，
///    并把 `stage_dir` 置空，失败清理不会删到 release）；
/// 6. 写 `current-version`（临时文件 + rename），删除旧 release。
///
/// 下载地址是 `{installer_api_base}/{encode_uri_component(version)}/...`；未配置 API base
/// 时在第 3 步之后立即失败。
async fn run_managed_self_update(managed_root: &str, version: &str) -> Result<(), String> {
    if !MANAGED_RELEASE_VERSION_RE.is_match(version) {
        return Err(format!("Invalid managed release version: {version}"));
    }

    let mut guard = match acquire_managed_lock(managed_root).await {
        Ok(guard) => guard,
        Err(ManagedLockError::Locked) => {
            return Err("Another managed Kiana update is already running.".to_string());
        }
        Err(ManagedLockError::Io { error, syscall }) => {
            return Err(node_fs_error_message(
                &error,
                syscall,
                Some(Path::new(&join_path(&[managed_root, "update.lock"]))),
                None,
            ));
        }
    };

    let mut stage_dir: Option<String> = None;
    let outcome = async {
        // 持锁后才清理；另一个更新进程正在写的 staging 不会被误删。
        cleanup_managed_staging(managed_root);
        let Some(installer_api_base) = installer_api_base() else {
            return Err(
                "Managed updates are unavailable: set KIANA_INSTALLER_API_BASE or KIANA_API_BASE_URL."
                    .to_string(),
            );
        };
        let release_url = format!("{installer_api_base}/{}", encode_uri_component(version));
        let staging_root = join_path(&[managed_root, "staging"]);
        let releases_root = join_path(&[managed_root, "releases"]);
        std::fs::create_dir_all(&releases_root).map_err(|error| {
            node_fs_error_message(&error, "mkdir", Some(Path::new(&releases_root)), None)
        })?;
        let release_dir = join_path(&[&releases_root, version]);
        // 已下载过的版本直接复用：先冒烟测试确认可运行，再切 current-version。
        if Path::new(&release_dir).exists() {
            verify_managed_release(&release_dir, version)?;
            activate_managed_release(managed_root, version)?;
            prune_managed_releases(managed_root, version);
            return Ok(());
        }

        std::fs::create_dir_all(&staging_root).map_err(|error| {
            node_fs_error_message(&error, "mkdir", Some(Path::new(&staging_root)), None)
        })?;
        let stage = mkdtemp_update(&staging_root).map_err(|error| {
            // Node 的 `mkdtempSync` 失败：`... mkdtemp '<root>/update-XXXXXX'`（syscall 是
            // `mkdtemp`，路径带模板后缀；libuv 自己不创建目录）。
            node_fs_error_message(
                &error,
                "mkdtemp",
                Some(Path::new(&join_path(&[&staging_root, "update-XXXXXX"]))),
                None,
            )
        })?;
        stage_dir = Some(stage.clone());
        let package_json_url = format!("{release_url}/package.json");
        let package_lock_url = format!("{release_url}/package-lock.json");
        // 两个文件并行下载；任一失败（含 HTTP 错误）都会短路，stage 由外层统一清理。
        let (package_json, package_lock) = tokio::try_join!(
            fetch_installer_artifact(&package_json_url, "package.json"),
            fetch_installer_artifact(&package_lock_url, "package-lock.json"),
        )?;
        std::fs::write(join_path(&[&stage, "package.json"]), package_json).map_err(|error| {
            node_fs_error_message(
                &error,
                "open",
                Some(Path::new(&join_path(&[&stage, "package.json"]))),
                None,
            )
        })?;
        std::fs::write(join_path(&[&stage, "package-lock.json"]), package_lock).map_err(
            |error| {
                node_fs_error_message(
                    &error,
                    "open",
                    Some(Path::new(&join_path(&[&stage, "package-lock.json"]))),
                    None,
                )
            },
        )?;

        // 安装只发生在 staging；npm 失败时 release 目录尚未出现。
        run_managed_npm_ci(&stage).await?;
        verify_managed_release(&stage, version)?;
        // rename 是发布点：成功后把 stage_dir 置空，后续失败也不会删除已发布的 release。
        std::fs::rename(&stage, &release_dir).map_err(|error| {
            node_fs_error_message(
                &error,
                "rename",
                Some(Path::new(&stage)),
                Some(Path::new(&release_dir)),
            )
        })?;
        stage_dir = None;
        activate_managed_release(managed_root, version)?;
        prune_managed_releases(managed_root, version);
        Ok(())
    }
    .await;

    // 失败时删除本次创建的 staging；成功时 stage_dir 已是 None。锁在返回前显式释放，
    // 之后 Drop 再调用 release 是幂等的（remove_dir 的 NotFound 被忽略）。
    if let Some(stage_dir) = stage_dir {
        remove_path_force(Path::new(&stage_dir));
    }
    guard.release();
    outcome
}

// ---------------------------------------------------------------------------
// self update note
// ---------------------------------------------------------------------------

/// `SELF_UPDATE_NOTE_MARKDOWN_THEME`（chalk 直接着色）。
///
/// 主题函数全部走 `Style`（`Style::detect()` 的颜色级别与 chalk 一致），用于把 release
/// note 渲染成与 TS 相同的 ANSI 文本；`code_block`/`quote` 等共用同一个 dim 函数。
fn self_update_note_markdown_theme(style: Style) -> MarkdownTheme {
    let heading: StyleFn = Arc::new(move |text: &str| style.bold(&style.yellow(text)));
    let link: StyleFn = Arc::new(move |text: &str| style.cyan(text));
    let link_url: StyleFn = Arc::new(move |text: &str| style.dim(text));
    let code: StyleFn = Arc::new(move |text: &str| style.yellow(text));
    let dim: StyleFn = Arc::new(move |text: &str| style.dim(text));
    let list_bullet: StyleFn = Arc::new(move |text: &str| style.yellow(text));
    let bold: StyleFn = Arc::new(move |text: &str| style.bold(text));
    let italic: StyleFn = Arc::new(move |text: &str| style.italic(text));
    let strikethrough: StyleFn = Arc::new(move |text: &str| style.strikethrough(text));
    let underline: StyleFn = Arc::new(move |text: &str| style.underline(text));
    MarkdownTheme {
        heading,
        link,
        link_url,
        code,
        code_block: Arc::clone(&dim),
        code_block_border: Arc::clone(&dim),
        quote: Arc::clone(&dim),
        quote_border: Arc::clone(&dim),
        hr: dim,
        list_bullet,
        bold,
        italic,
        strikethrough,
        underline,
        highlight_code: None,
        code_block_indent: None,
    }
}

/// `Math.max(20, process.stdout.columns ?? 80)`。
///
/// 通过 `ProcessTerminal` 读取当前终端宽度（非 TTY 时其内部退回 `COLUMNS`，再退回 80），
/// 下限 20 列，避免 Markdown 渲染器在极窄终端上得到 0 宽。
fn output_columns() -> usize {
    let terminal = kiana_tui::terminal::ProcessTerminal::new();
    let width = kiana_tui::terminal_colors::Terminal::columns(&terminal);
    usize::max(20, width)
}

// ---------------------------------------------------------------------------
// 帮助文本
// ---------------------------------------------------------------------------

/// `getPackageCommandUsage(command)`：单行用法（不含 `Usage:` 前缀与颜色）。
///
/// 返回值逐字出现在错误提示的 `Usage: ...` 与 `Use "kiana --help" or ...` 中；文案由单元
/// 测试 `usage_strings_are_verbatim` 锁定。`{APP_NAME}` 来自
/// `kiana_core::config::APP_NAME`（当前为 `kiana`）。
pub fn get_package_command_usage(command: PackageCommand) -> String {
    match command {
        PackageCommand::Install => {
            format!("{APP_NAME} install <source> [-l] [--approve|--no-approve]")
        }
        PackageCommand::Remove => {
            format!("{APP_NAME} remove <source> [-l] [--approve|--no-approve]")
        }
        PackageCommand::Update => format!(
            "{APP_NAME} update [source|self|kiana] [--self|--extensions|--models|--all] [--extension <source>] [--approve|--no-approve] [--force]"
        ),
        PackageCommand::List => format!("{APP_NAME} list [--approve|--no-approve]"),
    }
}

/// `CONFIG_COMMAND_USAGE`：`kiana config` 的单行用法。
///
/// 被帮助文本与参数错误提示复用，单元测试 `usage_strings_are_verbatim` 逐字锁定。
pub fn config_command_usage() -> String {
    format!("{APP_NAME} config [-l] [--approve|--no-approve]")
}

/// `kiana config --help` 的帮助文本（换行按 golden 文本锁定）。
///
/// `Style::detect()` 决定是否插入 ANSI（chalk 语义，每进程只检测一次），非 TTY 下是纯
/// 文本。`tests/golden/package_help/config.txt` 锁定该文本，`kiana config --help` 把它
/// 直接写到 stdout。
pub fn render_config_command_help() -> String {
    let style = Style::detect();
    format!(
        "{}\n  {}\n\nOpen the resource configuration TUI to enable or disable package resources.\nWithout -l, starts in global settings (~/{}/agent/settings.json).\nPress Tab in the TUI to switch between global and project-local modes.\n\nOptions:\n  -l, --local       Edit project overrides ({}/settings.json)\n  -a, --approve     Trust project-local files for this command with -l\n  -na, --no-approve Ignore project-local files for this command with -l\n\n",
        style.bold("Usage:"),
        config_command_usage(),
        kiana_core::config::CONFIG_DIR_NAME,
        kiana_core::config::CONFIG_DIR_NAME,
    )
}

/// `kiana <command> --help` 的帮助文本（换行按 golden 文本锁定）。
///
/// 四个子命令各对应 `tests/golden/package_help/{install,remove,update,list}.txt` 之一，
/// 单元测试 `help_text_matches_typescript_goldens` 逐字比较；`Style::detect()` 决定是否
/// 插入 ANSI，非 TTY 下是纯文本。
pub fn render_package_command_help(command: PackageCommand) -> String {
    let style = Style::detect();
    let usage = get_package_command_usage(command);
    match command {
        PackageCommand::Install => format!(
            "{}\n  {usage}\n\nInstall a package and add it to settings.\n\nOptions:\n  -l, --local       Install project-locally ({}/settings.json)\n  -a, --approve     Trust project-local files for this command\n  -na, --no-approve Ignore project-local files for this command\n\nExamples:\n  {APP_NAME} install npm:@foo/bar\n  {APP_NAME} install git:github.com/user/repo\n  {APP_NAME} install git:git@github.com:user/repo\n  {APP_NAME} install https://github.com/user/repo\n  {APP_NAME} install ssh://git@github.com/user/repo\n  {APP_NAME} install ./local/path\n\n",
            style.bold("Usage:"),
            kiana_core::config::CONFIG_DIR_NAME,
        ),
        PackageCommand::Remove => format!(
            "{}\n  {usage}\n\nRemove a package and its source from settings.\nAlias: {APP_NAME} uninstall <source> [-l]\n\nOptions:\n  -l, --local       Remove from project settings ({}/settings.json)\n  -a, --approve     Trust project-local files for this command\n  -na, --no-approve Ignore project-local files for this command\n\nExamples:\n  {APP_NAME} remove npm:@foo/bar\n  {APP_NAME} uninstall npm:@foo/bar\n\n",
            style.bold("Usage:"),
            kiana_core::config::CONFIG_DIR_NAME,
        ),
        PackageCommand::Update => format!(
            "{}\n  {usage}\n\nUpdate kiana, installed packages, or model catalogs.\n\nOptions:\n  --self                  Update kiana only (default when no target is given)\n  --extensions            Update installed packages only\n  --models                Refresh model catalogs only\n  --all                   Update kiana and installed packages\n  --extension <source>    Update one package only\n  -a, --approve           Trust project-local files for this command\n  -na, --no-approve       Ignore project-local files for this command\n  --force                 Reinstall kiana even if the current version is latest\n\nShort forms:\n  {APP_NAME} update                Update kiana only\n  {APP_NAME} update --all          Update kiana and all extensions\n  {APP_NAME} update --models       Refresh model catalogs only\n  {APP_NAME} update <source>       Update one package\n  {APP_NAME} update kiana             Update kiana only (self works as alias to kiana)\n\n",
            style.bold("Usage:"),
        ),
        PackageCommand::List => format!(
            "{}\n  {usage}\n\nList installed packages from user and project settings.\n\nOptions:\n  -a, --approve      Trust project-local files for this command\n  -na, --no-approve  Ignore project-local files for this command\n\n",
            style.bold("Usage:"),
        ),
    }
}

fn print_config_command_help(output: &Output) {
    output.write_stdout(&render_config_command_help());
}

fn print_package_command_help(command: PackageCommand, output: &Output) {
    output.write_stdout(&render_package_command_help(command));
}

// ---------------------------------------------------------------------------
// parsePackageCommand
// ---------------------------------------------------------------------------

/// JS 真值：`!!value`。`parsePackageCommand` 的 `source` 是 `string | undefined`，TS 用
/// `!source` / `!value` 判断，空字符串因此被当作“没有值”（与 `Some("")` 不同）。
fn js_truthy(value: Option<&str>) -> bool {
    value.is_some_and(|value| !value.is_empty())
}

/// `parsePackageCommand(args)`：把 argv（不含程序名）解析成 [`PackageCommandOptions`]。
///
/// 返回 `None` 表示首个参数不是 `install`/`remove`/`uninstall`/`update`/`list`，调用方
/// 应交给后续子命令（`config`/`auth`/...）。本函数只做语法分类：不打印、不读文件、
/// 不访问环境。
///
/// # 解析规则
///
/// - `-h/--help` 只置位 `help`，**不会停止扫描**：其余参数继续按规则消费（未知选项仍会
///   记进 `invalid_option`），但 [`handle_package_command`] 看到 `help` 就直接打印帮助，
///   不再检查这些错误。
/// - 子命令专属开关：`-l/--local` 只对 install/remove 有效；
///   `--self/--extensions/--models/--all/--force/--extension` 只对 update 有效；其它
///   子命令上的这些开关记入 `invalid_option`（每类只保留第一个）。
///   `--approve/-a` 与 `--no-approve/-na` 在所有子命令都接受，后出现者覆盖前者。
/// - `--extension` 需要下一个参数作为值；缺失、空串或以 `-` 开头都算
///   `missing_option_value`；第二次出现记 `conflicting_options`，但第二个值仍被消耗。
/// - 其它以 `-` 开头的参数 → `invalid_option`；多余的第二个位置参数 →
///   `invalid_argument`。第一个位置参数存进 `source`，空串也占位（后续位置参数会覆盖
///   这个空串而不是报错）。
///
/// # update 目标推导
///
/// 仅在 `command == Update` 时计算，按以下优先级取第一个匹配项（不匹配的组合会同时记入
/// `conflicting_options`，但目标仍按该顺序选出；调用方在非 `--help` 路径上看到冲突会直接
/// 失败）：
///
/// 1. `--models` → [`UpdateTarget::Models`]；
/// 2. `--extension <source>` → [`UpdateTarget::Extensions`]；
/// 3. 位置参数：`self`/`kiana` → 若同时有 `--extensions` 则 `All`，否则 `SelfOnly`；
///    其它非空值 → `Extensions { source }`；
/// 4. `--all` 或 `--self --extensions` → `All`；
/// 5. `--self` → `SelfOnly`；`--extensions` → `Extensions { source: None }`；
/// 6. 什么都没有 → `SelfOnly`，并置 `show_extensions_skipped_note`（提示扩展被跳过）。
///
/// 空字符串按 JS 假值处理：`update ""` 走第 6 条而不是更新名为空串的包。
pub fn parse_package_command(args: &[String]) -> Option<PackageCommandOptions> {
    let raw_command = args.first().map(String::as_str)?;
    let rest = &args[1..];
    let command = match raw_command {
        "uninstall" => PackageCommand::Remove,
        "install" => PackageCommand::Install,
        "remove" => PackageCommand::Remove,
        "update" => PackageCommand::Update,
        "list" => PackageCommand::List,
        _ => return None,
    };

    let mut local = false;
    let mut force = false;
    let mut project_trust_override: Option<bool> = None;
    let mut help = false;
    let mut invalid_option: Option<String> = None;
    let mut invalid_argument: Option<String> = None;
    let mut missing_option_value: Option<String> = None;
    let mut conflicting_options: Option<String> = None;
    let mut source: Option<String> = None;
    let mut self_flag = false;
    let mut extensions_flag = false;
    let mut models_flag = false;
    let mut all_flag = false;
    let mut extension_flag_source: Option<String> = None;

    // 第一遍：线性扫描。错误不提前返回，只把“第一个”同类错误记进对应字段；参数消费
    // 规则（值缺失时是否吃掉下一个参数）由下方的 while/index 递增顺序决定，改动需同步解析测试。
    let mut index = 0;
    while index < rest.len() {
        let argument = &rest[index];
        if argument == "-h" || argument == "--help" {
            help = true;
            index += 1;
            continue;
        }
        if argument == "-l" || argument == "--local" {
            if matches!(command, PackageCommand::Install | PackageCommand::Remove) {
                local = true;
            } else {
                invalid_option.get_or_insert_with(|| argument.clone());
            }
            index += 1;
            continue;
        }
        if argument == "--self" {
            if command == PackageCommand::Update {
                self_flag = true;
            } else {
                invalid_option.get_or_insert_with(|| argument.clone());
            }
            index += 1;
            continue;
        }
        if argument == "--extensions" {
            if command == PackageCommand::Update {
                extensions_flag = true;
            } else {
                invalid_option.get_or_insert_with(|| argument.clone());
            }
            index += 1;
            continue;
        }
        if argument == "--models" {
            if command == PackageCommand::Update {
                models_flag = true;
            } else {
                invalid_option.get_or_insert_with(|| argument.clone());
            }
            index += 1;
            continue;
        }
        if argument == "--all" {
            if command == PackageCommand::Update {
                all_flag = true;
            } else {
                invalid_option.get_or_insert_with(|| argument.clone());
            }
            index += 1;
            continue;
        }
        if argument == "--approve" || argument == "-a" {
            project_trust_override = Some(true);
            index += 1;
            continue;
        }
        if argument == "--no-approve" || argument == "-na" {
            project_trust_override = Some(false);
            index += 1;
            continue;
        }
        if argument == "--force" {
            if command == PackageCommand::Update {
                force = true;
            } else {
                invalid_option.get_or_insert_with(|| argument.clone());
            }
            index += 1;
            continue;
        }
        if argument == "--extension" {
            if command != PackageCommand::Update {
                invalid_option.get_or_insert_with(|| argument.clone());
                index += 1;
                continue;
            }
            // 值缺失/空串/以 '-' 开头 → 缺值；此时**不消费**下一个参数（它可能正是另一个
            // 开关，例如 `--extension --self` 仍会让 --self 生效）。
            let value = rest.get(index + 1);
            match value {
                None => {
                    missing_option_value.get_or_insert_with(|| argument.clone());
                }
                // TS: `if (!value || value.startsWith("-"))`——空字符串也是“缺值”。
                Some(value) if value.is_empty() || value.starts_with('-') => {
                    missing_option_value.get_or_insert_with(|| argument.clone());
                }
                // 重复提供：记录冲突，但仍消耗掉这个值（下面统一 index += 1），与 TS 的
                // index++ 一致；最终目标仍使用第一次给出的 source。
                Some(_value) if js_truthy(extension_flag_source.as_deref()) => {
                    conflicting_options
                        .get_or_insert_with(|| "--extension can only be provided once".to_string());
                    index += 1;
                }
                Some(value) => {
                    extension_flag_source = Some(value.clone());
                    index += 1;
                }
            }
            index += 1;
            continue;
        }
        if argument.starts_with('-') {
            invalid_option.get_or_insert_with(|| argument.clone());
            index += 1;
            continue;
        }
        if !js_truthy(source.as_deref()) {
            source = Some(argument.clone());
        } else {
            invalid_argument.get_or_insert_with(|| argument.clone());
        }
        index += 1;
    }

    // 第二遍：只在 update 时推导目标。顺序即优先级：--models > --extension > 位置参数 >
    // --all / (--self && --extensions) > --self > --extensions > 默认 self。
    // 冲突只记录文案（第一个），不会阻止目标选出；调用方会在执行前因 conflicting_options
    // 直接失败。空串一律按“没有提供”处理。
    let mut update_target: Option<UpdateTarget> = None;
    let mut show_extensions_skipped_note = false;
    if command == PackageCommand::Update {
        if all_flag
            && (self_flag
                || extensions_flag
                || models_flag
                || js_truthy(extension_flag_source.as_deref()))
        {
            conflicting_options.get_or_insert_with(|| {
                "--all cannot be combined with --self, --extensions, --models, or --extension"
                    .to_string()
            });
        }
        if all_flag && js_truthy(source.as_deref()) {
            conflicting_options.get_or_insert_with(|| {
                "--all cannot be combined with a positional source".to_string()
            });
        }

        if models_flag {
            if self_flag
                || extensions_flag
                || all_flag
                || js_truthy(extension_flag_source.as_deref())
            {
                conflicting_options.get_or_insert_with(|| {
                    "--models cannot be combined with --self, --extensions, --all, or --extension"
                        .to_string()
                });
            }
            if js_truthy(source.as_deref()) {
                conflicting_options.get_or_insert_with(|| {
                    "--models cannot be combined with a positional source".to_string()
                });
            }
            update_target = Some(UpdateTarget::Models);
        } else if let Some(extension_source) = extension_flag_source
            .as_deref()
            .filter(|value| !value.is_empty())
        {
            if self_flag || extensions_flag || all_flag {
                conflicting_options.get_or_insert_with(|| {
                    "--extension cannot be combined with --self, --extensions, or --all".to_string()
                });
            }
            if js_truthy(source.as_deref()) {
                conflicting_options.get_or_insert_with(|| {
                    "--extension cannot be combined with a positional source".to_string()
                });
            }
            update_target = Some(UpdateTarget::Extensions {
                source: Some(extension_source.to_string()),
            });
        } else if let Some(positional) = source.as_deref().filter(|value| !value.is_empty()) {
            let source_is_self = positional == "self" || positional == "kiana";
            if source_is_self {
                update_target = Some(if extensions_flag {
                    UpdateTarget::All
                } else {
                    UpdateTarget::SelfOnly
                });
            } else {
                if extensions_flag || self_flag || all_flag {
                    conflicting_options.get_or_insert_with(|| {
                        "positional update targets cannot be combined with --self, --extensions, or --all"
                            .to_string()
                    });
                }
                update_target = Some(UpdateTarget::Extensions {
                    source: Some(positional.to_string()),
                });
            }
        } else if all_flag || (self_flag && extensions_flag) {
            update_target = Some(UpdateTarget::All);
        } else if self_flag {
            update_target = Some(UpdateTarget::SelfOnly);
        } else if extensions_flag {
            update_target = Some(UpdateTarget::Extensions { source: None });
        } else {
            update_target = Some(UpdateTarget::SelfOnly);
            show_extensions_skipped_note = true;
        }
    }

    Some(PackageCommandOptions {
        command,
        source,
        update_target,
        show_extensions_skipped_note,
        local,
        force,
        project_trust_override,
        help,
        invalid_option,
        invalid_argument,
        missing_option_value,
        conflicting_options,
    })
}

fn update_target_includes_self(target: &UpdateTarget) -> bool {
    matches!(target, UpdateTarget::All | UpdateTarget::SelfOnly)
}

fn update_target_includes_extensions(target: &UpdateTarget) -> bool {
    matches!(target, UpdateTarget::All | UpdateTarget::Extensions { .. })
}

// ---------------------------------------------------------------------------
// 模型目录刷新
// ---------------------------------------------------------------------------

/// `refreshModelCatalogs(agentDir)` 发给 `ModelRuntime.create` / `refresh` 的参数。
///
/// TS 测试用 `vi.spyOn(ModelRuntime, "create")` 断言这两组参数；Rust 的 `ModelRuntime`
/// 是具体类型（没有 trait），因此把参数构造提成纯函数，由单元测试逐字段断言。
struct ModelCatalogRequest {
    create: CreateModelRuntimeOptions,
    refresh: kiana_ai::models::ModelsRefreshOptions,
}

/// 构造 [`ModelCatalogRequest`]（`authPath`/`modelsPath`/网络开关/取消信号）。
fn model_catalog_request(agent_dir: &str, signal: &AbortScope) -> ModelCatalogRequest {
    ModelCatalogRequest {
        create: CreateModelRuntimeOptions {
            auth_path: Some(join_path(&[agent_dir, "auth.json"])),
            models_path: ModelsPathOption::Path(join_path(&[agent_dir, "models.json"])),
            allow_model_network: false,
            signal: Some(signal.clone()),
            ..CreateModelRuntimeOptions::default()
        },
        refresh: kiana_ai::models::ModelsRefreshOptions {
            allow_network: Some(true),
            force: Some(true),
            signal: Some(signal.clone()),
            ..kiana_ai::models::ModelsRefreshOptions::default()
        },
    }
}

/// `refreshModelCatalogs(agentDir)`：`update --models` 的实现。
///
/// 用 `AbortScope` + 15 s 定时器做整体超时：超时后 abort，`ModelRuntime::refresh` 返回
/// `aborted == true`，这里转成 "Model catalog refresh timed out."。构造 runtime 时禁止模型
/// 网络（`allow_model_network: false`），刷新阶段再显式开网并 `force` 绕过新鲜度检查。
/// 刷新完成后先检查 `aborted`、再检查 `errors`（超时即使伴随个别 provider 失败也报超时）；
/// 有错误时把 `provider: message` 用 `; ` 连接；成功打印绿色 "Model catalogs refreshed"。
/// 失败返回 `Err(String)`，由 [`handle_package_command`] 打印 `Error:` 并返回 1。
async fn refresh_model_catalogs(
    agent_dir: &str,
    output: &Output,
    style: Style,
) -> Result<(), String> {
    let controller = AbortScope::new();
    // 定时器与刷新并发；刷新先结束时 abort 定时器，避免任务在后台继续存活。
    let timer = {
        let controller = controller.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(15_000)).await;
            controller.abort();
        })
    };
    let request = model_catalog_request(agent_dir, &controller);
    let result = async {
        let runtime = ModelRuntime::create(request.create)
            .await
            .map_err(|error| error.message)?;
        let refresh = runtime
            .refresh(request.refresh)
            .await
            .map_err(|error| error.message)?;
        // 中止优先：超时时不再拼接各 provider 的部分错误。
        if refresh.aborted {
            return Err("Model catalog refresh timed out.".to_string());
        }
        if !refresh.errors.is_empty() {
            let details = refresh
                .errors
                .iter()
                .map(|(provider, error)| format!("{provider}: {}", error.message))
                .collect::<Vec<_>>()
                .join("; ");
            return Err(format!("Could not refresh model catalogs: {details}"));
        }
        Ok(())
    }
    .await;
    timer.abort();
    if result.is_ok() {
        output.write_line_stdout(&style.green("Model catalogs refreshed"));
    }
    result
}

// ---------------------------------------------------------------------------
// 自更新
// ---------------------------------------------------------------------------

/// `process.argv[1]` 的等价物。
///
/// Node 的 `argv[1]` 是入口脚本路径；原生二进制没有脚本参数，入口即可执行文件本身。
/// 这里用 [`std::env::current_exe`]（绝对路径，与 Node 对 `argv[1]` 做 `path.resolve`
/// 后的形态一致，也与 `kiana_core::config` 的 `entrypoint_argument` 缺省值相同）。
/// 空字符串/缺失时不打印该行（TS `if (entrypoint)`）。
fn kiana_entrypoint() -> Option<String> {
    std::env::current_exe()
        .ok()
        .map(|path| path.to_string_lossy().into_owned())
        .filter(|value| !value.is_empty())
}

/// `printSelfUpdateUnavailable` 的等价物：说明无法自更新的原因与入口位置。
///
/// 三段都写 stderr：错误行、`get_self_update_unavailable_instruction` 生成的指引，以及
/// 能取到入口路径时的 `Location of kiana executable: ...`。不返回错误码，调用方随后返回
/// [`PackageCommandOutcome::FAILED`]。
fn print_self_update_unavailable(
    npm_command: Option<&[String]>,
    update_package_target: Option<SelfUpdatePackageTarget>,
    output: &Output,
) {
    output.write_line_stderr(&format!(
        "error: {APP_NAME} cannot self-update this installation."
    ));
    output.write_line_stderr(&get_self_update_unavailable_instruction(
        PACKAGE_NAME,
        npm_command,
        update_package_target,
    ));
    if let Some(entrypoint) = kiana_entrypoint() {
        output.write_line_stderr("");
        output.write_line_stderr(&format!("Location of {APP_NAME} executable: {entrypoint}"));
    }
}

/// 自更新命令失败后的兜底提示：把用户可手动执行的命令打印到 stderr。
fn print_self_update_fallback(command: &SelfUpdateCommand, output: &Output, style: Style) {
    output.write_line_stderr(&style.dim(&format!(
        "If this keeps failing, run this command yourself: {}",
        command.display
    )));
}

/// pnpm 自更新失败时的附加提示：pnpm 缓存中的 registry 元数据可能过期。
fn print_pnpm_self_update_metadata_hint(output: &Output, style: Style) {
    output.write_line_stderr(&style.yellow(
        "If pnpm reports missing package versions, its cached registry metadata may be stale.",
    ));
    output.write_line_stderr(&style.yellow(&format!(
        "Run `pnpm store prune` and retry `{APP_NAME} update --self`."
    )));
}

/// `printSelfUpdateNote(note)`：把 release note 作为 Markdown 渲染到 stdout。
///
/// 空/纯空白 note 直接返回（不打印空标题）。否则输出：空行、加粗黄色的 "Update note"、
/// 按终端宽度渲染的 Markdown（每行去掉行尾空白后 join）、空行。
fn print_self_update_note(note: &str, output: &Output, style: Style) {
    let trimmed = note.trim();
    if trimmed.is_empty() {
        return;
    }

    output.write_line_stdout("");
    output.write_line_stdout(&style.bold(&style.yellow("Update note")));
    let width = output_columns();
    let mut markdown = Markdown::new(
        trimmed.to_string(),
        0,
        0,
        self_update_note_markdown_theme(style),
    );
    let lines: Vec<String> = Component::render(&mut markdown, width)
        .into_iter()
        .map(|line| line.trim_end().to_string())
        .collect();
    output.write_line_stdout(&lines.join("\n"));
    output.write_line_stdout("");
}

/// `interface SelfUpdatePlan`：一次自更新的决策结果。
///
/// `package_name` 是 release API 给出的包名（可能是改名后的新包），`install_spec` 是
/// `"<package>@<version>"`；`should_run == false` 表示当前已是最新（此时
/// `get_self_update_plan` 已向 stdout 打印提示，调用方直接返回成功）。`note` 是要在更新前
/// 展示的 Markdown。
struct SelfUpdatePlan {
    package_name: String,
    install_spec: String,
    version: String,
    should_run: bool,
    note: Option<String>,
}

/// `getSelfUpdatePlan(force)`：查询最新 release 并决定是否需要执行自更新。
///
/// 版本检查固定 `retry: Some(true)`（显式 `update --self` 允许重试）。`Ok(None)`
/// （离线/未配置端点/响应不可用）会转成错误；文案根据
/// `kiana_core::changelog::is_version_check_configured()` 区分“未配置 API base”与
/// “配置了但查不到”。`should_run` 在三种情况下为 true：`--force`、包名变化（迁移到新包
/// 名）、`is_newer_package_version(latest, VERSION)`。否则向 stdout 打印
/// "kiana is already up to date (vVERSION)" 并返回 `should_run: false`。
///
/// 副作用：可能联网；"已是最新" 的提示在**本函数内**打印，调用方只负责判断 `should_run`。
async fn get_self_update_plan(
    force: bool,
    output: &Output,
    style: Style,
) -> Result<SelfUpdatePlan, String> {
    let latest_release = get_latest_kiana_release(
        VERSION,
        &VersionCheckOptions {
            retry: Some(true),
            ..VersionCheckOptions::default()
        },
    )
    .await
    .map_err(|error| {
        format!(
            "Could not determine latest {APP_NAME} version: {}",
            format_version_check_error(&error)
        )
    })?;
    let Some(latest_release) = latest_release else {
        let message = if kiana_core::changelog::is_version_check_configured() {
            format!("Could not determine latest {APP_NAME} version.")
        } else {
            format!(
                "Could not determine latest {APP_NAME} version: KIANA_API_BASE_URL is not configured."
            )
        };
        return Err(message);
    };

    let package_name = latest_release
        .package_name
        .clone()
        .unwrap_or_else(|| PACKAGE_NAME.to_string());
    let install_spec = format!("{package_name}@{}", latest_release.version);
    if force
        || package_name != PACKAGE_NAME
        || is_newer_package_version(&latest_release.version, VERSION)
    {
        return Ok(SelfUpdatePlan {
            package_name,
            install_spec,
            version: latest_release.version,
            should_run: true,
            note: latest_release.note,
        });
    }

    output
        .write_line_stdout(&style.green(&format!("{APP_NAME} is already up to date (v{VERSION})")));
    Ok(SelfUpdatePlan {
        package_name,
        install_spec,
        version: latest_release.version,
        should_run: false,
        note: None,
    })
}

/// Node 的信号名（`child.on("close", (code, signal))`）。
///
/// Node 的 `signal` 对所有已知信号都是符号名（`os.constants.signals` 的反向表），
/// 只有表外编号才退回 `SIG<number>`；旧实现只覆盖了 11 个信号，SIGUSR1/SIGUSR2/
/// SIGBUS/SIGTRAP/SIGXCPU 等会错误地打印成 `SIG10` 这类数字。
#[cfg(unix)]
fn signal_name(signal: i32) -> String {
    // Linux 的 `signal(7)` 编号；别名按 Node 的选择（6 → SIGABRT、29 → SIGIO）。
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
        _ => return format!("SIG{signal}"),
    };
    name.to_string()
}

#[cfg(not(unix))]
fn signal_name(signal: i32) -> String {
    format!("SIG{signal}")
}

/// Unix：子进程被信号终止时返回信号编号；正常退出返回 `None`。
#[cfg(unix)]
fn exit_signal(status: std::process::ExitStatus) -> Option<i32> {
    use std::os::unix::process::ExitStatusExt;
    status.signal()
}

/// 非 Unix 平台没有 Unix 信号语义，恒为 `None`。
#[cfg(not(unix))]
fn exit_signal(_status: std::process::ExitStatus) -> Option<i32> {
    None
}

/// `runSelfUpdate(command)`：按顺序执行自更新命令（`SelfUpdateCommand`；包名变更时
/// `steps` 是“卸载旧包名 + 安装新包名”两步）。
///
/// 先向 stdout 打印 dim 的 "Updating kiana with ..."，然后对每一步用继承的 stdio 启动子
/// 进程并等待；非零退出码或被信号终止时立即返回 Err（后续步骤不再执行），错误文案带
/// step 的展示文本。成功不打印任何内容（成功提示由调用方打印）。
async fn run_self_update(
    command: &SelfUpdateCommand,
    output: &Output,
    style: Style,
) -> Result<(), String> {
    output
        .write_line_stdout(&style.dim(&format!("Updating {APP_NAME} with {}...", command.display)));
    let steps: Vec<kiana_core::config::SelfUpdateStep> = match &command.steps {
        // `steps` 为 Some 时是“卸载旧包名 + 安装新包名”，顺序不能颠倒；None 时退化成单步。
        Some(steps) => steps.clone(),
        None => vec![kiana_core::config::SelfUpdateStep {
            command: command.command.clone(),
            args: command.args.clone(),
            display: command.display.clone(),
        }],
    };
    for step in steps {
        let mut child = spawn_process(
            &step.command,
            &step.args,
            &SpawnOptions::with_stdio(StdioMode::Inherit),
        )
        .map_err(|error| error.to_string())?;
        let status = child.wait().await.map_err(|error| error.to_string())?;
        if let Some(code) = status.code() {
            if code != 0 {
                return Err(format!("{} exited with code {code}", step.display));
            }
        } else if let Some(signal) = exit_signal(status) {
            return Err(format!(
                "{} terminated by signal {}",
                step.display,
                signal_name(signal)
            ));
        } else {
            return Err(format!("{} exited with code unknown", step.display));
        }
    }
    Ok(())
}

/// `prepareWindowsNpmSelfUpdate()`：Windows 上 npm 自更新前的准备。
///
/// 先删除上次遗留的 `<package_dir>/node_modules/.kiana-native-quarantine`；再调用
/// `quarantine_windows_native_dependencies`（Rust 侧是全平台 no-op，原因见该函数的文档）。
/// 非 Windows 直接返回。两处失败都被内部忽略，不影响后续更新。
fn prepare_windows_npm_self_update() {
    if !cfg!(windows) {
        return;
    }
    let package_dir = get_package_dir();
    cleanup_windows_self_update_quarantine(&package_dir);
    quarantine_windows_native_dependencies(&package_dir);
}

// ---------------------------------------------------------------------------
// settings / project trust
// ---------------------------------------------------------------------------

/// `getCommandAppMode()`：stdin 与 stdout 都是 TTY 才算交互。
///
/// 只有交互模式才允许在项目信任解析中弹出选择 UI（`has_ui`）；fd 被重定向时按 print
/// 模式处理，`resolve_project_trusted` 不会阻塞等待输入。
fn get_command_app_mode() -> AppMode {
    use std::io::IsTerminal;
    if std::io::stdin().is_terminal() && std::io::stdout().is_terminal() {
        AppMode::Interactive
    } else {
        AppMode::Print
    }
}

/// 把 project_trust 扩展/资源加载阶段收集到的警告逐条以黄色 `Warning:` 写到 stderr。
fn report_project_trust_warnings(warnings: &[String], output: &Output, style: Style) {
    for warning in warnings {
        output.write_line_stderr(&style.yellow(&format!("Warning: {warning}")));
    }
}

/// `reportSettingsErrors(settingsManager, context)`。
///
/// 与 TS 的差异：TS 的 `SettingsError.error` 是 `Error` 对象，`error.stack` 存在时会再打印
/// 一行 `chalk.dim(error.stack)`；Rust 的 `SettingsError` 只保留 `error.message`
/// （`kiana-config` 的冻结形状），没有可打印的 stack。V8 的 stack 由 TS 源码路径、行号与
/// `JSON.parse (<anonymous>)` 等内部帧组成，无法在原生实现里重建；伪造一组帧会比缺少
/// 伪造帧比缺少该行更误导，因此这里只打印标准 warning 行。
///
/// `drain_errors()` 会清空队列，所以同一批错误只打印一次。调用点都在信任解析之后：
/// 未受信时项目作用域根本没有被解析，不会产生项目设置错误。
fn report_settings_errors(
    settings_manager: &SettingsManager,
    context: &str,
    output: &Output,
    style: Style,
) {
    for error in settings_manager.drain_errors() {
        output.write_line_stderr(&style.yellow(&format!(
            "Warning ({context}, {} settings): {}",
            error.scope, error.error
        )));
    }
}

/// `createCommandSettingsManager` 的结果：已完成项目信任决策的设置管理器，以及在信任
/// 解析期间收集、需要由调用方打印的警告（单个扩展加载失败、扩展报告的错误等）。
struct CommandSettingsResult {
    settings_manager: SettingsManager,
    project_trust_warnings: Vec<String>,
}

/// `createCommandSettingsManager` 的输入。
///
/// `use_saved_project_trust_only` 为 true 时（仅 `update`）只读信任存储，不加载扩展、
/// 不弹提示；否则走完整的 `resolve_project_trusted` 流程。`project_trust_override`
/// 来自 `--approve/--no-approve`：`Some` 时直接作为结果，且不写入信任存储。
struct CommandSettingsOptions {
    cwd: String,
    agent_dir: String,
    project_trust_override: Option<bool>,
    use_saved_project_trust_only: bool,
    extension_factories: Vec<InlineExtension>,
}

/// `createCommandSettingsManager(options)`：创建设置管理器并解析项目信任。
///
/// 关键点：构造时显式 `project_trusted: Some(false)`，项目设置先按“未受信”处理（不解析、
/// 不产生解析错误）；信任决策完成后 `set_project_trusted` 才会在受信时重新加载项目设置。
/// 这样 `report_settings_errors` 不会报告一份最终没有被采纳的项目设置。
///
/// 信任来源（由 `resolve_project_trusted` 决定）：显式覆盖 `--approve/--no-approve` >
/// 扩展的 `project_trust` 事件 > 保存的信任记录 > 设置里的 `defaultProjectTrust` >
/// 交互提示（仅 TTY）。扩展只会在工作目录包含需要信任的资源时加载；
/// `load_project_trust_extensions` 整体失败是致命的（`Err` 上抛），单个扩展返回的错误
/// 则转成警告交给调用方打印。
///
/// 错误：信任存储读取失败、扩展加载失败或 `resolve_project_trusted` 失败都返回
/// `Err(message)`，调用方打印 `Error:` 并返回 1。
async fn create_command_settings_manager(
    options: CommandSettingsOptions,
    output: &Output,
) -> Result<CommandSettingsResult, String> {
    // 先以“未受信”构造：项目 settings.json 在这一步不会被解析（见 kiana-config 的
    // SettingsManager::from_storage_with_paths），避免在信任决定前采纳项目配置。
    let settings_manager = SettingsManager::create_with_options(
        &options.cwd,
        &options.agent_dir,
        SettingsManagerCreateOptions {
            project_trusted: Some(false),
        },
    );
    let mut project_trust_warnings: Vec<String> = Vec::new();
    let trust_store = ProjectTrustStore::new(&options.agent_dir);
    if options.use_saved_project_trust_only {
        // update 只认已保存的信任记录：不加载项目扩展、不向用户提问（测试
        // does_not_prompt_or_ask_extensions_for_project_trust_during_update 锁定）。
        let saved_project_trusted = trust_store
            .get(&options.cwd)
            .map_err(|error| error.message)?
            == Some(true);
        settings_manager.set_project_trusted(
            options
                .project_trust_override
                .unwrap_or(saved_project_trusted),
        );
        return Ok(CommandSettingsResult {
            settings_manager,
            project_trust_warnings,
        });
    }

    let app_mode = get_command_app_mode();
    // 没有显式覆盖且 cwd 有需要信任的资源时，才加载项目扩展参与 project_trust 派发。
    // 扩展整体加载失败是致命的（`?` 直接上抛）；单个扩展返回的错误则降级为警告。
    let extensions_result = if options.project_trust_override.is_none()
        && has_trust_requiring_project_resources(&options.cwd)
    {
        let loader = DefaultResourceLoader::new(DefaultResourceLoaderOptions {
            cwd: options.cwd.clone(),
            agent_dir: options.agent_dir.clone(),
            settings_manager: Some(settings_manager.clone()),
            extension_factories: options.extension_factories,
            ..DefaultResourceLoaderOptions::default()
        });
        Some(
            loader
                .load_project_trust_extensions()
                .await
                .map_err(|error| error.to_string())?,
        )
    } else {
        None
    };
    if let Some(result) = &extensions_result {
        for error in &result.errors {
            project_trust_warnings.push(format!(
                "Failed to load extension \"{}\": {}",
                error.path, error.error
            ));
        }
    }

    let views = create_project_trust_context_views(ProjectTrustOptions {
        cwd: options.cwd.clone(),
        mode: app_mode,
        settings_manager: settings_manager.clone(),
        has_ui: app_mode == AppMode::Interactive,
        output: output.clone(),
    });
    let emitter: Option<Arc<dyn ProjectTrustEmitter>> = extensions_result.map(|result| {
        // 只有加载过扩展才构造 emitter；同样的视图也决定交互提示能否使用 UI。
        Arc::new(ProjectTrustEmitterAdapter::new(
            result,
            views.extension.clone(),
        )) as Arc<dyn ProjectTrustEmitter>
    });
    let extension_warnings = Arc::new(Mutex::new(Vec::<String>::new()));
    let sink = Arc::clone(&extension_warnings);
    // 最终信任值；扩展通过 on_extension_error 报告的错误与加载错误一起转成警告。
    let project_trusted = resolve_project_trusted(ResolveProjectTrustedOptions {
        cwd: options.cwd.clone(),
        trust_store,
        trust_override: options.project_trust_override,
        default_project_trust: Some(settings_manager.get_default_project_trust()),
        project_trust_context: views.config,
        emit_project_trust: emitter,
        on_extension_error: Some(Arc::new(move |message: &str| {
            sink.lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .push(message.to_string());
        })),
    })
    .await
    .map_err(|error| error.message)?;
    project_trust_warnings.extend(
        extension_warnings
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .drain(..),
    );
    // 受信时这里才会真正加载项目设置；未受信则项目设置保持空。
    settings_manager.set_project_trusted(project_trusted);
    Ok(CommandSettingsResult {
        settings_manager,
        project_trust_warnings,
    })
}

/// 内联扩展里 `builtin: true` 的名字（`kiana config` 的内置扩展开关列表）。
fn builtin_extension_names(extension_factories: &[InlineExtension]) -> Vec<String> {
    extension_factories
        .iter()
        .filter(|input| is_builtin_extension(input))
        .map(|input| match input {
            InlineExtension::Named { name, .. } => name.clone(),
            // `is_builtin_extension` 只匹配 `Named { builtin: Some(true) }`，因此这个分支
            // 实际不会命中。
            InlineExtension::Factory(_) => String::new(),
        })
        .collect()
}

// ---------------------------------------------------------------------------
// handleConfigCommand
// ---------------------------------------------------------------------------

/// `handleConfigCommand(args, runtimeOptions)`：处理 `kiana config [...]`。
///
/// `args` 是完整 argv（含 `config`）；首参数不是 `config` 时返回
/// [`PackageCommandOutcome::NOT_HANDLED`] 且不产生输出。`-h/--help` 优先于其它参数校验。
///
/// 流程：解析 `-l/--local`、`--approve/-a`、`--no-approve/-na`（未知开关或任何位置参数
/// 直接打印用法并返回 1）→ 创建设置管理器与项目信任 → 打印信任/扩展警告 → `-l` 且项目
/// 未受信时拒绝 → 分别解析全局与项目两套包路径 → 打开配置 TUI（`select_config`）→
/// flush 设置写队列。
///
/// 返回：TUI 正常关闭 → [`PackageCommandOutcome::HANDLED`]；TUI 请求退出（TS 的
/// `process.exit(code)`）→ `handled: true` 且 `exit_code` 为该值；设置/信任/包解析失败
/// → 打印 `Error: {message}` 并返回 [`PackageCommandOutcome::FAILED`]。
pub async fn handle_config_command(
    args: &[String],
    runtime: PackageCommandRuntimeOptions,
) -> PackageCommandOutcome {
    let output = runtime.output.clone();
    let style = Style::detect();
    match handle_config_command_inner(args, runtime).await {
        Ok(outcome) => outcome,
        // TS 没有 try/catch：拒绝会逃逸成未捕获异常。Rust 侧打印 `Error:` 并返回 1。
        Err(message) => {
            output.write_line_stderr(&style.red(&format!("Error: {message}")));
            PackageCommandOutcome::FAILED
        }
    }
}

// 命令主体。任何 Err 都由外层 `handle_config_command` 统一打印 `Error:` 并转成 FAILED。
async fn handle_config_command_inner(
    args: &[String],
    runtime: PackageCommandRuntimeOptions,
) -> Result<PackageCommandOutcome, String> {
    let Some(command) = args.first() else {
        return Ok(PackageCommandOutcome::NOT_HANDLED);
    };
    if command != "config" {
        return Ok(PackageCommandOutcome::NOT_HANDLED);
    }
    let output = runtime.output;
    let style = Style::detect();
    let rest = &args[1..];

    // help 优先于所有校验，且允许出现在任意位置。
    if rest.iter().any(|arg| arg == "-h" || arg == "--help") {
        print_config_command_help(&output);
        return Ok(PackageCommandOutcome::HANDLED);
    }

    // 与 package 命令不同，config 只接受 -l / -a / -na / -h；未知开关与位置参数立即失败，
    // 不收集错误（TS 的循环里直接 return）。
    let mut local = false;
    let mut project_trust_override: Option<bool> = None;
    for argument in rest {
        if argument == "-l" || argument == "--local" {
            local = true;
        } else if argument == "-a" || argument == "--approve" {
            project_trust_override = Some(true);
        } else if argument == "-na" || argument == "--no-approve" {
            project_trust_override = Some(false);
        } else if argument.starts_with('-') {
            output.write_line_stderr(
                &style.red(&format!("Unknown option {argument} for \"config\".")),
            );
            output.write_line_stderr(&style.dim(&format!(
                "Use \"{APP_NAME} --help\" or \"{}\".",
                config_command_usage()
            )));
            return Ok(PackageCommandOutcome::FAILED);
        } else {
            output.write_line_stderr(&style.red(&format!("Unexpected argument {argument}.")));
            output.write_line_stderr(&style.dim(&format!("Usage: {}", config_command_usage())));
            return Ok(PackageCommandOutcome::FAILED);
        }
    }

    let cwd = runtime.cwd.clone();
    let agent_dir = runtime.agent_dir.clone();
    let builtin_extensions = builtin_extension_names(&runtime.extension_factories);
    // config 一定走完整信任解析（不限于已保存的信任记录）。
    let CommandSettingsResult {
        settings_manager,
        project_trust_warnings,
    } = create_command_settings_manager(
        CommandSettingsOptions {
            cwd: cwd.clone(),
            agent_dir: agent_dir.clone(),
            project_trust_override,
            use_saved_project_trust_only: false,
            extension_factories: runtime.extension_factories,
        },
        &output,
    )
    .await?;
    report_project_trust_warnings(&project_trust_warnings, &output, style);
    if local && !settings_manager.is_project_trusted() {
        output.write_line_stderr(
            &style.red("Project is not trusted. Use --approve to modify local resource config."),
        );
        return Ok(PackageCommandOutcome::FAILED);
    }
    report_settings_errors(&settings_manager, "config command", &output, style);

    // 全局视图用独立的“未受信”设置管理器解析，确保只拿到全局 packages（不含项目）。
    let global_settings_manager = SettingsManager::create_with_options(
        &cwd,
        &agent_dir,
        SettingsManagerCreateOptions {
            project_trusted: Some(false),
        },
    );
    let global_resolved_paths = DefaultPackageManager::new(PackageManagerOptions {
        cwd: cwd.clone(),
        agent_dir: agent_dir.clone(),
        settings_manager: global_settings_manager,
        builtin_extensions: builtin_extensions.clone(),
    })
    .resolve(None)
    .await
    .map_err(|error| error.message().to_string())?;
    // 项目受信时再用真正的设置管理器解析一次；未受信时项目视图等同于全局视图
    // （TUI 也会因 project_mode_available=false 禁用 Project 模式）。
    let project_resolved_paths = if settings_manager.is_project_trusted() {
        DefaultPackageManager::new(PackageManagerOptions {
            cwd: cwd.clone(),
            agent_dir: agent_dir.clone(),
            settings_manager: settings_manager.clone(),
            builtin_extensions,
        })
        .resolve(None)
        .await
        .map_err(|error| error.message().to_string())?
    } else {
        global_resolved_paths.clone()
    };

    // 两套视图的安装失败去重后逐条警告；IndexSet 保持首次出现顺序。
    let mut install_warnings: IndexSet<String> = IndexSet::new();
    for failure in global_resolved_paths
        .install_failures
        .iter()
        .flatten()
        .chain(project_resolved_paths.install_failures.iter().flatten())
    {
        install_warnings.insert(format_package_install_warning(failure));
    }
    for warning in install_warnings {
        output.write_line_stderr(&style.yellow(&format!("Warning: {warning}")));
    }

    // TUI 需要同时拿到两个作用域；初始写入范围由 -l 决定。
    let mut resolved_paths =
        kiana_interactive::components::config_selector::ScopedResolvedPaths::new();
    resolved_paths.insert(
        kiana_interactive::components::config_selector::ConfigWriteScope::Global,
        global_resolved_paths,
    );
    resolved_paths.insert(
        kiana_interactive::components::config_selector::ConfigWriteScope::Project,
        project_resolved_paths,
    );
    let write_scope = if local {
        kiana_interactive::components::config_selector::ConfigWriteScope::Project
    } else {
        kiana_interactive::components::config_selector::ConfigWriteScope::Global
    };
    let project_mode_available = settings_manager.is_project_trusted();

    // TS 的 SettingsStorage 在 `selectConfig` 返回前同步写盘；Rust 的写队列由异步 worker
    // 消费，调用方可能在同一次 stdin 批次里完成“切换设置 + 退出选择器”（`process.exit(0)`
    // 会终止进程）。返回前 flush，保证配置 TUI 里改过的设置已经落盘。
    let settings_manager_for_flush = settings_manager.clone();
    let select_result = select_config(ConfigSelectorOptions {
        resolved_paths,
        settings_manager,
        cwd,
        agent_dir,
        write_scope,
        project_mode_available,
    })
    .await;
    settings_manager_for_flush.flush().await;
    match select_result {
        Ok(()) => Ok(PackageCommandOutcome::HANDLED),
        // TS 在退出键上 `process.exit(0)`；库层把退出码交给调用方。
        // `Err` 只表示 TUI 请求的退出码，不代表有未完成的设置写入（上面已 flush）。
        Err(exit) => Ok(PackageCommandOutcome {
            handled: true,
            exit_code: exit.code,
        }),
    }
}

// ---------------------------------------------------------------------------
// handlePackageCommand
// ---------------------------------------------------------------------------

/// `handlePackageCommand(args, runtimeOptions)`：处理 `install`/`remove`/`uninstall`/
/// `update`/`list`。
///
/// 首参数不属于上述命令时返回 [`PackageCommandOutcome::NOT_HANDLED`]（不输出）。否则一定
/// 返回 `handled: true`：`--help` 打印帮助；参数错误打印用法；其余进入实际动作。
///
/// 处理顺序（改动顺序会改变多种错误同时存在时打印哪一条）：
///
/// 1. `--help`（先于一切校验）；
/// 2. `invalid_option`、`missing_option_value`、`invalid_argument`、`conflicting_options`
///    四类解析错误，每类一个；
/// 3. install/remove 缺 source 的错误；
/// 4. `update --models` 直接刷新模型目录并返回（不创建设置管理器）；
/// 5. 创建设置管理器与项目信任（`update` 只读已保存的信任，不加载扩展、不弹提示）；
///    失败时打印 `Error:` 并返回 1；
/// 6. `-l` 的 install/remove 要求项目已信任，否则拒绝；
/// 7. 打印设置警告 → 构造 `DefaultPackageManager` → 执行动作；
/// 8. 无论动作成败都 flush 设置写队列，然后返回动作结果（Err 在这里打印 `Error:`）。
///
/// 副作用：安装/删除会通过包管理器改写 settings.json；`list` 只读；`update` 可能联网、
/// 执行包管理器命令或替换自身的 managed release。输出写入
/// [`PackageCommandRuntimeOptions::output`]（子进程与 TUI 例外，见模块文档）。
pub async fn handle_package_command(
    args: &[String],
    runtime: PackageCommandRuntimeOptions,
) -> PackageCommandOutcome {
    // 首参数不是 package 子命令：交给后续 handler，且不产生任何输出。
    let Some(options) = parse_package_command(args) else {
        return PackageCommandOutcome::NOT_HANDLED;
    };
    let output = runtime.output.clone();
    let style = Style::detect();

    // help 优先：只要请求了帮助，解析阶段记录的其它错误在这里被忽略。
    if options.help {
        print_package_command_help(options.command, &output);
        return PackageCommandOutcome::HANDLED;
    }

    // 四类解析错误按固定顺序检查，每类只打印一次。
    let usage = get_package_command_usage(options.command);
    if let Some(invalid_option) = &options.invalid_option {
        output.write_line_stderr(&style.red(&format!(
            "Unknown option {invalid_option} for \"{}\".",
            options.command.as_str()
        )));
        output.write_line_stderr(&style.dim(&format!("Use \"{APP_NAME} --help\" or \"{usage}\".")));
        return PackageCommandOutcome::FAILED;
    }
    if let Some(missing_option_value) = &options.missing_option_value {
        output.write_line_stderr(&style.red(&format!("Missing value for {missing_option_value}.")));
        output.write_line_stderr(&style.dim(&format!("Usage: {usage}")));
        return PackageCommandOutcome::FAILED;
    }
    if let Some(invalid_argument) = &options.invalid_argument {
        output.write_line_stderr(&style.red(&format!("Unexpected argument {invalid_argument}.")));
        output.write_line_stderr(&style.dim(&format!("Usage: {usage}")));
        return PackageCommandOutcome::FAILED;
    }
    if let Some(conflicting_options) = &options.conflicting_options {
        output.write_line_stderr(&style.red(conflicting_options));
        output.write_line_stderr(&style.dim(&format!("Usage: {usage}")));
        return PackageCommandOutcome::FAILED;
    }

    // install/remove 必须有 source；空串按 JS 假值算缺失（测试
    // treats_an_empty_install_source_as_missing 锁定）。
    let source = options.source.clone();
    if matches!(
        options.command,
        PackageCommand::Install | PackageCommand::Remove
    ) && !js_truthy(source.as_deref())
    {
        output.write_line_stderr(
            &style.red(&format!("Missing {} source.", options.command.as_str())),
        );
        output.write_line_stderr(&style.dim(&format!("Usage: {usage}")));
        return PackageCommandOutcome::FAILED;
    }

    // --models 不触碰 settings/包管理器，直接刷新模型目录后返回。
    if options.command == PackageCommand::Update
        && options.update_target == Some(UpdateTarget::Models)
    {
        if let Err(message) = refresh_model_catalogs(&runtime.agent_dir, &output, style).await {
            output.write_line_stderr(&style.red(&format!("Error: {message}")));
            return PackageCommandOutcome::FAILED;
        }
        return PackageCommandOutcome::HANDLED;
    }

    let cwd = runtime.cwd.clone();
    let agent_dir = runtime.agent_dir.clone();
    // 只有 -l 的 install/remove 会写项目配置，因此只有它们需要项目已受信。
    let writes_project_package_config = matches!(
        options.command,
        PackageCommand::Install | PackageCommand::Remove
    ) && options.local;
    // update 只读已保存的项目信任；其余命令走完整信任解析。
    let settings_result = create_command_settings_manager(
        CommandSettingsOptions {
            cwd: cwd.clone(),
            agent_dir: agent_dir.clone(),
            project_trust_override: options.project_trust_override,
            use_saved_project_trust_only: options.command == PackageCommand::Update,
            extension_factories: runtime.extension_factories,
        },
        &output,
    )
    .await;
    let CommandSettingsResult {
        settings_manager,
        project_trust_warnings,
    } = match settings_result {
        Ok(result) => result,
        // TS 的这处 await 在 try/catch 之外：拒绝会逃逸成未捕获异常（Node 打印 stack，
        // 退出码 1）。Rust 没有 stack，打印 `Error: {message}` 并返回同样的退出码
        // （模块文档列出的偏差）。
        Err(message) => {
            output.write_line_stderr(&style.red(&format!("Error: {message}")));
            return PackageCommandOutcome::FAILED;
        }
    };
    report_project_trust_warnings(&project_trust_warnings, &output, style);
    if !settings_manager.is_project_trusted() && writes_project_package_config {
        output.write_line_stderr(
            &style.red("Project is not trusted. Use --approve to modify local package config."),
        );
        return PackageCommandOutcome::FAILED;
    }
    report_settings_errors(&settings_manager, "package command", &output, style);
    // TS: `settingsManager.getGlobalSettings().npmCommand`（只看全局作用域）。
    // 类型不是字符串数组（或缺失）时按未配置处理，不报错。
    let self_update_npm_command = settings_manager
        .get_global_settings()
        .raw("npmCommand")
        .and_then(|value| serde_json::from_value::<Vec<String>>(value).ok());

    // 与 config 命令不同，这里 `builtin_extensions` 传空 `Vec::new()`。
    let package_manager = DefaultPackageManager::new(PackageManagerOptions {
        cwd: cwd.clone(),
        agent_dir: agent_dir.clone(),
        settings_manager: settings_manager.clone(),
        builtin_extensions: Vec::new(),
    });

    {
        // 只显示 Start 事件里的 message（包管理器在开始安装/更新时发出，如
        // "Installing ..."）；消息自带换行，所以用 write_stdout 而不是 write_line_stdout。
        let output = output.clone();
        package_manager.set_progress_callback(Some(Arc::new(
            move |event: kiana_resources::package::ProgressEvent| {
                if event.kind == kiana_resources::package::ProgressEventType::Start
                    && let Some(message) = &event.message
                {
                    output.write_stdout(&Style::detect().dim(&format!("{message}\n")));
                }
            },
        )));
    }

    let result = run_package_action(
        &options,
        &source,
        &package_manager,
        self_update_npm_command,
        &output,
        style,
    )
    .await;
    // 设置写队列是异步的，命令返回前排空，保证调用方在 `await` 之后读到最新落盘内容。
    settings_manager.flush().await;
    match result {
        Ok(outcome) => outcome,
        Err(message) => {
            output.write_line_stderr(&style.red(&format!("Error: {message}")));
            PackageCommandOutcome::FAILED
        }
    }
}

/// 执行解析后的动作，返回结果或待打印的错误消息。
///
/// Install / Remove 把源字符串交给 `DefaultPackageManager`（会改写 settings.json）；Remove
/// 的 `bool` 为 false 表示 settings 中没有匹配项（卸载命令本身未报错），这里按失败处理并
/// 打印 `No matching package found for ...`。List 只读 settings，按 user/project 分组输出；
/// `filtered` 的包显示 `(filtered)`，有 `installed_path` 时再缩进一行显示。
///
/// Update 先更新扩展（可选单个源），再处理自身更新；自身更新分 managed 与普通安装两条
/// 路径。部分失败（--force + managed、managed 更新失败、Windows 非 npm/pnpm、不可自更新、
/// 自更新命令失败）在这里直接打印并返回 `FAILED`；install/remove/list 与 update 的扩展
/// 更新错误以 `Err(String)` 冒泡，由 [`handle_package_command`] 统一打印 `Error:`。
#[allow(clippy::too_many_arguments)]
async fn run_package_action(
    options: &PackageCommandOptions,
    source: &Option<String>,
    package_manager: &DefaultPackageManager,
    self_update_npm_command: Option<Vec<String>>,
    output: &Output,
    style: Style,
) -> Result<PackageCommandOutcome, String> {
    match options.command {
        PackageCommand::Install => {
            package_manager
                .install_and_persist(
                    source.as_deref().unwrap_or_default(),
                    InstallOptions {
                        local: options.local,
                    },
                )
                .await
                .map_err(|error| error.message().to_string())?;
            output.write_line_stdout(&style.green(&format!(
                "Installed {}",
                source.as_deref().unwrap_or_default()
            )));
            Ok(PackageCommandOutcome::HANDLED)
        }
        PackageCommand::Remove => {
            let removed = package_manager
                .remove_and_persist(
                    source.as_deref().unwrap_or_default(),
                    InstallOptions {
                        local: options.local,
                    },
                )
                .await
                .map_err(|error| error.message().to_string())?;
            if !removed {
                // `remove_and_persist` 返回 false 表示 settings 里没有匹配项（不是卸载命令
                // 报错）；按失败处理。
                output.write_line_stderr(&style.red(&format!(
                    "No matching package found for {}",
                    source.as_deref().unwrap_or_default()
                )));
                return Ok(PackageCommandOutcome::FAILED);
            }
            output.write_line_stdout(&style.green(&format!(
                "Removed {}",
                source.as_deref().unwrap_or_default()
            )));
            Ok(PackageCommandOutcome::HANDLED)
        }
        PackageCommand::List => {
            // 只读：`list_configured_packages` 从 settings 读出 user/project 两组；
            // `filtered` 表示该条目是带过滤规则的对象形式。
            let configured_packages = package_manager.list_configured_packages();
            let user_packages: Vec<_> = configured_packages
                .iter()
                .filter(|package| package.scope == InstallScope::User)
                .collect();
            let project_packages: Vec<_> = configured_packages
                .iter()
                .filter(|package| package.scope == InstallScope::Project)
                .collect();

            if configured_packages.is_empty() {
                output.write_line_stdout(&style.dim("No packages installed."));
                return Ok(PackageCommandOutcome::HANDLED);
            }

            let format_package = |package: &kiana_resources::package::ConfiguredPackage| {
                let display = if package.filtered {
                    format!("{} (filtered)", package.source)
                } else {
                    package.source.clone()
                };
                output.write_line_stdout(&format!("  {display}"));
                if let Some(installed_path) = &package.installed_path {
                    output.write_line_stdout(&style.dim(&format!("    {installed_path}")));
                }
            };

            if !user_packages.is_empty() {
                output.write_line_stdout(&style.bold("User packages:"));
                for package in &user_packages {
                    format_package(package);
                }
            }
            if !project_packages.is_empty() {
                if !user_packages.is_empty() {
                    output.write_line_stdout("");
                }
                output.write_line_stdout(&style.bold("Project packages:"));
                for package in &project_packages {
                    format_package(package);
                }
            }
            Ok(PackageCommandOutcome::HANDLED)
        }
        PackageCommand::Update => {
            // 先更新扩展、再更新自身（与 TS 顺序一致）：扩展更新失败会直接冒泡，不会进入
            // 自身更新。
            let target = options
                .update_target
                .clone()
                .unwrap_or(UpdateTarget::SelfOnly);
            if options.show_extensions_skipped_note {
                output.write_line_stdout(&style.dim(&format!(
                    "Extensions are skipped. Run {APP_NAME} update --extensions to update extensions."
                )));
            }
            if update_target_includes_extensions(&target) {
                let update_source = match &target {
                    UpdateTarget::Extensions { source } => source.as_deref(),
                    _ => None,
                };
                package_manager
                    .update(update_source)
                    .await
                    .map_err(|error| error.message().to_string())?;
                if let Some(update_source) = update_source {
                    output.write_line_stdout(&style.green(&format!("Updated {update_source}")));
                } else {
                    output.write_line_stdout(&style.green("Updated packages"));
                }
            }
            if update_target_includes_self(&target) {
                let managed_install_root = get_active_managed_install_root()?;
                if managed_install_root.is_some() && options.force {
                    // managed 布局不支持 --force：错误文案要求直接重跑安装器来修复。
                    output.write_line_stderr(&style.red(&format!(
                        "Managed {APP_NAME} installations do not support --force; rerun the installer to repair this installation."
                    )));
                    return Ok(PackageCommandOutcome::FAILED);
                }
                let plan = get_self_update_plan(options.force, output, style).await?;
                if !plan.should_run {
                    return Ok(PackageCommandOutcome::HANDLED);
                }
                if let Some(managed_install_root) = managed_install_root {
                    if let Some(note) = &plan.note {
                        print_self_update_note(note, output, style);
                    }
                    output.write_line_stdout(
                        &style.dim(&format!("Updating managed {APP_NAME} installation...")),
                    );
                    if let Err(message) =
                        run_managed_self_update(&managed_install_root, &plan.version).await
                    {
                        output.write_line_stderr(&style.red(&format!("Error: {message}")));
                        return Ok(PackageCommandOutcome::FAILED);
                    }
                    output.write_line_stdout(&style.green(&format!(
                        "Updated {APP_NAME} from {VERSION} to {}",
                        plan.version
                    )));
                    return Ok(PackageCommandOutcome::HANDLED);
                }

                let install_method = detect_install_method();
                // Windows 上只允许 npm / pnpm 安装方式走自更新；其它方式打印检测结果并
                // 让用户手动更新。
                if cfg!(windows)
                    && !matches!(install_method, InstallMethod::Npm | InstallMethod::Pnpm)
                {
                    output.write_line_stderr(&style.red(&format!(
                        "{APP_NAME} self-update on Windows is only supported for npm and pnpm installs."
                    )));
                    output.write_line_stderr(&style.dim(&format!(
                        "Detected install method: {}. Update {APP_NAME} manually.",
                        install_method_name(install_method)
                    )));
                    return Ok(PackageCommandOutcome::FAILED);
                }
                // 目标包名与 install spec 来自 release API：包名可能已经改变（迁移场景）。
                let self_update_target = SelfUpdatePackageTarget::Target {
                    package_name: plan.package_name.clone(),
                    install_spec: Some(plan.install_spec.clone()),
                };
                let self_update_command = get_self_update_command(
                    PACKAGE_NAME,
                    self_update_npm_command.as_deref(),
                    Some(self_update_target.clone()),
                );
                let Some(self_update_command) = self_update_command else {
                    // 不可自更新（非全局安装、目录不可写、未知安装方式）：打印指引并失败。
                    print_self_update_unavailable(
                        self_update_npm_command.as_deref(),
                        Some(self_update_target.clone()),
                        output,
                    );
                    return Ok(PackageCommandOutcome::FAILED);
                };
                // note 只在确定要执行更新时打印（should_run 已为 true）。
                if let Some(note) = &plan.note {
                    print_self_update_note(note, output, style);
                }
                let update_result = async {
                    // 只有 npm 安装需要做 Windows 隔离清理；其它安装方式下该函数直接返回。
                    if install_method == InstallMethod::Npm {
                        prepare_windows_npm_self_update();
                    }
                    run_self_update(&self_update_command, output, style).await
                }
                .await;
                if let Err(message) = update_result {
                    // 失败时补两条提示：pnpm 额外提示清缓存；所有安装方式都给出可手动执行
                    // 的命令。
                    output.write_line_stderr(&style.red(&format!("Error: {message}")));
                    if install_method == InstallMethod::Pnpm {
                        print_pnpm_self_update_metadata_hint(output, style);
                    }
                    print_self_update_fallback(&self_update_command, output, style);
                    return Ok(PackageCommandOutcome::FAILED);
                }
                output.write_line_stdout(&style.green(&format!(
                    "Updated {APP_NAME} from {VERSION} to {}",
                    plan.version
                )));
                if install_method == InstallMethod::Npm {
                    // npm 安装不锁定依赖版本（文案原话）；更新成功后提示改跑安装器脚本，
                    // 迁移到依赖锁定的 managed 安装。脚本地址来自 KIANA_API_BASE_URL。
                    let installer_command = match kiana_core::config::get_api_base_url() {
                        Some(base) if cfg!(windows) => {
                            format!("powershell -c \"irm {base}/install.ps1 | iex\"")
                        }
                        Some(base) => format!("curl -fsSL {base}/install.sh | sh"),
                        None => {
                            "Installer URL is not configured: set KIANA_API_BASE_URL.".to_string()
                        }
                    };
                    output.write_line_stdout("");
                    output.write_line_stdout(&style.yellow(&format!(
                        "This npm installation of {APP_NAME} does not pin its dependencies."
                    )));
                    output.write_line_stdout(&style.yellow(
                        "Run the installer to migrate to a managed installation that does:",
                    ));
                    output.write_line_stdout("");
                    output.write_line_stdout(&format!("  {}", style.bold(&installer_command)));
                }
            }
            Ok(PackageCommandOutcome::HANDLED)
        }
    }
}

/// `InstallMethod` 的字面量（TS 的 `detectInstallMethod()` 返回值）。
///
/// 与 `kiana_core::config::InstallMethod::as_str` 的取值相同；这里是本地副本，修改任一处
/// 都要同步，否则 Windows 的 "Detected install method: ..." 文案会与其它输出不一致。
fn install_method_name(method: InstallMethod) -> &'static str {
    match method {
        InstallMethod::BunBinary => "bun-binary",
        InstallMethod::Npm => "npm",
        InstallMethod::Pnpm => "pnpm",
        InstallMethod::Yarn => "yarn",
        InstallMethod::Bun => "bun",
        InstallMethod::Unknown => "unknown",
    }
}

// 单元测试：解析器、帮助文案（golden）、URL 编码、目录锁心跳、信号名、模型刷新参数。
// 跨进程的 managed 更新与项目信任行为在 tests/package_command_paths.rs 中覆盖。
#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> PackageCommandOptions {
        let owned: Vec<String> = args.iter().map(|arg| arg.to_string()).collect();
        parse_package_command(&owned).expect("command should parse")
    }

    fn conflict(args: &[&str]) -> String {
        parse(args)
            .conflicting_options
            .expect("expected a conflicting options message")
    }

    #[test]
    fn parses_each_command_and_uninstall_alias() {
        assert_eq!(
            parse(&["install", "npm:a"]).command,
            PackageCommand::Install
        );
        assert_eq!(parse(&["remove", "npm:a"]).command, PackageCommand::Remove);
        assert_eq!(
            parse(&["uninstall", "npm:a"]).command,
            PackageCommand::Remove
        );
        assert_eq!(parse(&["list"]).command, PackageCommand::List);
        assert_eq!(parse(&["update"]).command, PackageCommand::Update);
        assert!(parse_package_command(&["config".to_string()]).is_none());
        assert!(parse_package_command(&["auth".to_string()]).is_none());
    }

    #[test]
    fn parses_install_options() {
        let options = parse(&["install", "npm:a", "-l", "--approve", "--force"]);
        assert!(options.local);
        assert_eq!(options.project_trust_override, Some(true));
        // `--force` only applies to update.
        assert!(!options.force);
        assert_eq!(options.invalid_option.as_deref(), Some("--force"));
        assert_eq!(options.source.as_deref(), Some("npm:a"));
    }

    #[test]
    fn first_unknown_option_wins() {
        let options = parse(&["install", "--unknown", "--other"]);
        assert_eq!(options.invalid_option.as_deref(), Some("--unknown"));
    }

    #[test]
    fn second_positional_argument_is_invalid() {
        let options = parse(&["install", "one", "two", "three"]);
        assert_eq!(options.source.as_deref(), Some("one"));
        assert_eq!(options.invalid_argument.as_deref(), Some("two"));
    }

    #[test]
    fn help_flag_stops_parsing_into_help() {
        assert!(parse(&["install", "--help"]).help);
        assert!(parse(&["update", "-h"]).help);
    }

    #[test]
    fn defaults_update_target_to_self_with_skipped_note() {
        let options = parse(&["update"]);
        assert_eq!(options.update_target, Some(UpdateTarget::SelfOnly));
        assert!(options.show_extensions_skipped_note);
    }

    #[test]
    fn derives_update_targets_in_order() {
        assert_eq!(
            parse(&["update", "--self"]).update_target,
            Some(UpdateTarget::SelfOnly)
        );
        assert_eq!(
            parse(&["update", "--extensions"]).update_target,
            Some(UpdateTarget::Extensions { source: None })
        );
        assert_eq!(
            parse(&["update", "--self", "--extensions"]).update_target,
            Some(UpdateTarget::All)
        );
        assert_eq!(
            parse(&["update", "--all"]).update_target,
            Some(UpdateTarget::All)
        );
        assert_eq!(
            parse(&["update", "kiana"]).update_target,
            Some(UpdateTarget::SelfOnly)
        );
        assert_eq!(
            parse(&["update", "self", "--extensions"]).update_target,
            Some(UpdateTarget::All)
        );
        assert_eq!(
            parse(&["update", "npm:a"]).update_target,
            Some(UpdateTarget::Extensions {
                source: Some("npm:a".to_string())
            })
        );
        assert_eq!(
            parse(&["update", "--extension", "npm:a"]).update_target,
            Some(UpdateTarget::Extensions {
                source: Some("npm:a".to_string())
            })
        );
        assert_eq!(
            parse(&["update", "--models"]).update_target,
            Some(UpdateTarget::Models)
        );
        assert!(!parse(&["update", "--models"]).show_extensions_skipped_note);
    }

    #[test]
    fn detects_update_conflicts() {
        assert_eq!(
            conflict(&["update", "--all", "--self"]),
            "--all cannot be combined with --self, --extensions, --models, or --extension"
        );
        assert_eq!(
            conflict(&["update", "--all", "npm:a"]),
            "--all cannot be combined with a positional source"
        );
        assert_eq!(
            conflict(&["update", "--models", "--self"]),
            "--models cannot be combined with --self, --extensions, --all, or --extension"
        );
        assert_eq!(
            conflict(&["update", "--models", "npm:a"]),
            "--models cannot be combined with a positional source"
        );
        assert_eq!(
            conflict(&["update", "--extension", "npm:a", "--self"]),
            "--extension cannot be combined with --self, --extensions, or --all"
        );
        assert_eq!(
            conflict(&["update", "--extension", "npm:a", "npm:b"]),
            "--extension cannot be combined with a positional source"
        );
        assert_eq!(
            conflict(&["update", "npm:a", "--self"]),
            "positional update targets cannot be combined with --self, --extensions, or --all"
        );
    }

    /// JS 真值：`""` 与 `undefined` 一样是“没有值”。TS 的所有 `!source` / `!value` 判断都
    /// 依赖它；用 `Option::is_none` 会把空字符串当成有效值。
    #[test]
    fn treats_empty_string_arguments_as_absent_like_javascript() {
        // `install ""`：source 仍是假值 → handler 报 Missing install source。
        let options = parse(&["install", ""]);
        assert_eq!(options.source.as_deref(), Some(""));

        // `install "" foo`：`if (!source) source = arg` 让 foo 成为 source。
        let options = parse(&["install", "", "foo"]);
        assert_eq!(options.source.as_deref(), Some("foo"));
        assert!(options.invalid_argument.is_none());

        // `update --extension ""`：空值 → Missing value for --extension。
        let options = parse(&["update", "--extension", ""]);
        assert_eq!(options.missing_option_value.as_deref(), Some("--extension"));
        assert_eq!(options.update_target, Some(UpdateTarget::SelfOnly));
        assert!(options.show_extensions_skipped_note);

        // `update --all ""`：空 source 不构成 positional 冲突。
        let options = parse(&["update", "--all", ""]);
        assert!(options.conflicting_options.is_none());
        assert_eq!(options.update_target, Some(UpdateTarget::All));

        // `update ""`：默认 self + skipped note，而不是更新名为 "" 的包。
        let options = parse(&["update", ""]);
        assert_eq!(options.update_target, Some(UpdateTarget::SelfOnly));
        assert!(options.show_extensions_skipped_note);
    }

    #[test]
    fn detects_missing_and_duplicate_extension_values() {
        let missing = parse(&["update", "--extension"]);
        assert_eq!(missing.missing_option_value.as_deref(), Some("--extension"));
        let missing_flag = parse(&["update", "--extension", "--self"]);
        assert_eq!(
            missing_flag.missing_option_value.as_deref(),
            Some("--extension")
        );
        let duplicate = parse(&["update", "--extension", "a", "--extension", "b"]);
        assert_eq!(
            duplicate.conflicting_options.as_deref(),
            Some("--extension can only be provided once")
        );
        // 第二个值被当作 extension 的值消耗掉（TS 的 `index++`）。
        assert_eq!(
            duplicate.update_target,
            Some(UpdateTarget::Extensions {
                source: Some("a".to_string())
            })
        );
    }

    #[test]
    fn help_text_matches_typescript_goldens() {
        assert_eq!(
            render_config_command_help(),
            include_str!("../tests/golden/package_help/config.txt")
        );
        assert_eq!(
            render_package_command_help(PackageCommand::Install),
            include_str!("../tests/golden/package_help/install.txt")
        );
        assert_eq!(
            render_package_command_help(PackageCommand::Remove),
            include_str!("../tests/golden/package_help/remove.txt")
        );
        assert_eq!(
            render_package_command_help(PackageCommand::Update),
            include_str!("../tests/golden/package_help/update.txt")
        );
        assert_eq!(
            render_package_command_help(PackageCommand::List),
            include_str!("../tests/golden/package_help/list.txt")
        );
    }

    #[test]
    fn usage_strings_are_verbatim() {
        assert_eq!(
            get_package_command_usage(PackageCommand::Install),
            "kiana install <source> [-l] [--approve|--no-approve]"
        );
        assert_eq!(
            get_package_command_usage(PackageCommand::Remove),
            "kiana remove <source> [-l] [--approve|--no-approve]"
        );
        assert_eq!(
            get_package_command_usage(PackageCommand::Update),
            "kiana update [source|self|kiana] [--self|--extensions|--models|--all] [--extension <source>] [--approve|--no-approve] [--force]"
        );
        assert_eq!(
            get_package_command_usage(PackageCommand::List),
            "kiana list [--approve|--no-approve]"
        );
        assert_eq!(
            config_command_usage(),
            "kiana config [-l] [--approve|--no-approve]"
        );
    }

    #[test]
    fn encode_uri_component_escapes_like_javascript() {
        assert_eq!(encode_uri_component("1.2.3"), "1.2.3");
        assert_eq!(encode_uri_component("1.2.3+build.4"), "1.2.3%2Bbuild.4");
        assert_eq!(encode_uri_component("a b"), "a%20b");
    }

    /// 目录 mtime 必须能被刷新（Windows 上 std 的 `File::open` 打不开目录，旧实现的心跳
    /// 因此永远不更新，10 s 后活锁会被并发更新当成过期）。
    #[test]
    fn touch_lock_refreshes_the_lock_directory_mtime() {
        let temp = tempfile::tempdir().expect("tempdir");
        let lock = temp.path().join("update.lock");
        std::fs::create_dir(&lock).expect("lock dir");
        filetime::set_file_mtime(&lock, filetime::FileTime::from_unix_time(0, 0))
            .expect("backdate");
        assert!(lock_is_stale(&lock).expect("stat stale"));

        touch_lock(&lock);

        assert!(
            !lock_is_stale(&lock).expect("stat fresh"),
            "touch_lock must refresh the directory mtime"
        );
    }

    /// proper-lockfile 的 `update = stale/2`：心跳每 5 s 刷新 mtime，活锁在 10 s 后仍不过期。
    #[tokio::test(start_paused = true)]
    async fn managed_lock_heartbeat_keeps_a_live_lock_fresh() {
        let temp = tempfile::tempdir().expect("tempdir");
        let managed_root = temp.path().to_string_lossy().into_owned();
        let lock = temp.path().join("update.lock");
        let guard = acquire_managed_lock(&managed_root)
            .await
            .ok()
            .expect("acquire lock");
        filetime::set_file_mtime(&lock, filetime::FileTime::from_unix_time(0, 0))
            .expect("backdate");
        assert!(lock_is_stale(&lock).expect("stat stale"));

        tokio::time::sleep(Duration::from_millis(5_600)).await;

        assert!(
            !lock_is_stale(&lock).expect("stat fresh"),
            "heartbeat must refresh the lock within the stale window"
        );
        drop(guard);
        assert!(!lock.exists(), "release removes the lock directory");
    }

    #[test]
    fn manages_release_version_regex() {
        assert!(MANAGED_RELEASE_VERSION_RE.is_match("1.2.3"));
        assert!(MANAGED_RELEASE_VERSION_RE.is_match("1.2.3-beta.1"));
        assert!(MANAGED_RELEASE_VERSION_RE.is_match("1.2.3-beta.1+build.2"));
        assert!(!MANAGED_RELEASE_VERSION_RE.is_match("1.2"));
        assert!(!MANAGED_RELEASE_VERSION_RE.is_match("v1.2.3"));
        assert!(!MANAGED_RELEASE_VERSION_RE.is_match("1.2.3.4"));
    }

    /// TS `refreshes only model catalogs with update --models` 用 spy 断言
    /// `ModelRuntime.create` / `refresh` 的参数；Rust 侧直接断言构造函数给出的两组选项。
    #[test]
    fn model_catalog_refresh_uses_the_exact_create_and_refresh_options() {
        let signal = AbortScope::new();
        let request = model_catalog_request("/agent", &signal);
        assert_eq!(
            request.create.auth_path.as_deref(),
            Some("/agent/auth.json")
        );
        assert_eq!(
            request.create.models_path,
            ModelsPathOption::Path("/agent/models.json".to_string())
        );
        assert!(!request.create.allow_model_network);
        assert!(request.create.signal.is_some());
        assert_eq!(request.refresh.allow_network, Some(true));
        assert_eq!(request.refresh.force, Some(true));
        assert!(request.refresh.signal.is_some());
    }

    /// Node 的 `child.on("close", (code, signal))` 对所有已知信号都给出符号名
    /// （`os.constants.signals` 反向表的 Linux 编号）；表外编号才退回 `SIG<number>`。
    #[cfg(unix)]
    #[test]
    fn signal_names_match_node_for_the_full_linux_signal_table() {
        assert_eq!(signal_name(1), "SIGHUP");
        assert_eq!(signal_name(5), "SIGTRAP");
        assert_eq!(signal_name(6), "SIGABRT");
        assert_eq!(signal_name(7), "SIGBUS");
        assert_eq!(signal_name(10), "SIGUSR1");
        assert_eq!(signal_name(12), "SIGUSR2");
        assert_eq!(signal_name(16), "SIGSTKFLT");
        assert_eq!(signal_name(17), "SIGCHLD");
        assert_eq!(signal_name(19), "SIGSTOP");
        assert_eq!(signal_name(20), "SIGTSTP");
        assert_eq!(signal_name(24), "SIGXCPU");
        assert_eq!(signal_name(25), "SIGXFSZ");
        assert_eq!(signal_name(28), "SIGWINCH");
        assert_eq!(signal_name(29), "SIGIO");
        assert_eq!(signal_name(31), "SIGSYS");
        // 表外编号（如实时信号）退回数字形式。
        assert_eq!(signal_name(34), "SIG34");
    }
}
