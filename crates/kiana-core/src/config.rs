//! config.ts：APP_NAME/APP_TITLE/CONFIG_DIR_NAME/VERSION/getAgentDir/getDocsPath/getSessionsDir/normalizePath/resolvePath/canonicalizePath。
//!
//! 逐字移植 `packages/coding-agent/src/config.ts`（656 行）。规范：
//! `spec/ca-cli.md` §2.6（含 §2.6.1-§2.6.5）、`spec/GAPS.md` §G4.5/§G5.1。
//!
//! ## 常量取值（决策：`ARCHITECTURE.md` §8 第 24 条 / GAPS G5.1）
//!
//! TS 在模块加载时读一次 `package.json`（`getPackageJsonPath()`），用 `pkg.version`、
//! `pkg.name`、`pkg.kianaConfig.{name,configDir}` 得到下面前五个常量。Rust 版把
//! `packages/coding-agent/package.json` 的**当前值**直接写成字面量，不读文件、不读
//! `CARGO_PKG_VERSION`（workspace 版本 `0.1.0` 与 npm 包版本无关）：
//!
//! | TS | 值 | 来源 |
//! |---|---|---|
//! | `PACKAGE_NAME` | `"@earendil-works/kiana-coding-agent"` | `package.json#name` |
//! | `APP_NAME` | `"kiana"` | `kianaConfig.name` 缺省时的回退 |
//! | `APP_TITLE` | `"Kiana"` | `kianaConfig.name` 缺省时的回退 |
//! | `CONFIG_DIR_NAME` | `".kiana"` | `package.json#kianaConfig.configDir` |
//! | `VERSION` | `"1.4.0"` | `package.json#version`（`pkg.version || "0.0.0"` 的实际值） |
//!
//! 改版本时同步改 `VERSION` 与 `kiana_ai::KIANA_AI_VERSION`；CI 校验两者分别等于对应
//! `package.json` 的 `version`（G5.1）。`ENV_AGENT_DIR`/`ENV_SESSION_DIR` 由 `APP_NAME`
//! 派生（`"KIANA_CODING_AGENT_DIR"`/`"KIANA_CODING_AGENT_SESSION_DIR"`），由 `tests/config.rs`
//! 断言与 `APP_NAME` 一致。
//!
//! ## 运行期检测常量
//!
//! 原生二进制下 `IS_BUN_BINARY = IS_BUN_RUNTIME = IS_BUNDLED_NODE = false`；分支代码保留，
//! 便于将来换 runtime（`spec/ca-cli.md` §2.6.1）。
//!
//! ## `__dirname` 的替代
//!
//! Rust 没有 `__dirname`。`get_package_dir()` 在 `KIANA_PACKAGE_DIR` 未设置时用
//! `current_exe().parent()` 走 `find_node_package_dir`；测试一律设置 `KIANA_PACKAGE_DIR`，
//! 不依赖仓库布局。
//!
//! `getEntrypointPackageDir()` 的 `process.argv[1]` 是 Node 的**入口脚本**路径（Node 会把
//! 它 `path.resolve` 成绝对路径）。原生二进制没有脚本参数，因此 `entrypoint_argument` 的
//! 缺省值是可执行文件自身（`current_exe()`，即 `argv[0]` 的实际路径）；测试用
//! [`set_entrypoint_override_for_test`] 注入 TS 场景里的 `argv[1]`。
//!
//! ## 测试缝
//!
//! - [`detect_install_method_from`]：注入 `{package_dir}\0{exec_path}`（doc-hidden）。
//! - [`set_exec_path_override_for_test`] / [`clear_exec_path_override_for_test`]：模拟 TS 测试里的
//!   `process.execPath`（doc-hidden，**test hook, not public API**）。
//! - [`set_entrypoint_override_for_test`] / [`clear_entrypoint_override_for_test`]：模拟
//!   `process.argv[1]`（doc-hidden）。
//! - [`startup_package_json_path`]：TS 模块加载时捕获的 package.json 路径（doc-hidden）；
//!   注意 `startup_package_json_path` 在 `spec/ca-cli.md` 的“私有”清单里，这里仅作为测试缝暴露。
//!
//! ## 已知偏差
//!
//! 1. `readCommandOutput(..., {requireSuccess: true})` 在 TS 里会 `throw`；冻结的 Rust 签名
//!    （`-> Option<...>` / `-> String`）无法抛错，因此失败被当作“没有该输出”（返回 `None`）。
//!    **用户可见后果**（配置了 `npmCommand` 且该命令不存在时）：TS 在
//!    `getSelfUpdateCommand`/`getSelfUpdateUnavailableInstruction` 里抛出
//!    `Failed to run <cmd...>: <reason>`（package-manager-cli 打印 `Error: ...` 并退出 1），
//!    Rust 则返回 `None` 并打印 “This installation is not managed by a global npm install…”；
//!    诊断信息（同样的 `Failed to run ...` 文案）只在内部 `Err` 里，没有出口。这是冻结签名的
//!    直接后果，修改需要把返回类型改为 `Result`（非 source-compatible）。
//! 2. `isSelfUpdatePathWritable` 在 Windows 上用 `Permissions::readonly()` 近似 libuv 的
//!    `uv_fs_access`（后者在 Windows 上只检查 `FILE_ATTRIBUTE_READONLY`，同样不看 ACL），
//!    因此不需要 `windows-sys`；Windows 不进入自更新验收矩阵。
//! 3. `detectInstallChange` 的“启动时 package.json”在 TS 里是模块加载时捕获；Rust 用
//!    `LazyLock`，以**首次调用** `startup_package_json_path()` 为“启动”时点。
//! 4. `detectInstallMethod` 的路径前缀用 `get_package_dir()`（TS 的 `__dirname` 是
//!    `<pkg>/src|dist`）。`spec/ca-cli.md` §2.6.1 把 `__dirname` 映射为“可执行文件路径或
//!    `KIANA_PACKAGE_DIR`”；只要 `KIANA_PACKAGE_DIR` 指向**同一个包**（其文档用途：Nix/Guix store），
//!    两者的 `/pnpm/`、`/.pnpm/`、`/yarn/`、`/.yarn/`、`/install/global/node_modules/`、
//!    `/npm/`、`/node_modules/` 子串判定等价。若把 `KIANA_PACKAGE_DIR` 指到与模块位置路径形状
//!    不同的另一个目录，Rust 会按该目录判定而 TS 仍按模块位置判定——这超出该变量的语义。
//! 5. `detectInstallChange` 的 `installed.version !== VERSION` 已按 JS 严格不等实现（非字符串
//!    永远算变更）。冻结的 `InstallChange::Updated.version` 是 `String`，因此非字符串值按
//!    JS `String(value)` 转换（`42` -> `"42"`、`["1.4.0"]` -> `"1.4.0"`、`9007199254740993`
//!    -> `"9007199254740992"`），TS 运行期保留原始类型。JSON 文档为 `null` 时 TS 抛
//!    `TypeError`，Rust 签名不能抛 → `None`。`1e400` 这类溢出字面量 serde_json 无法表示
//!    （解析失败 → `None`），而 TS 的 `JSON.parse` 得到 truthy 的 `Infinity` → `Updated`；
//!    这是工作区级强制偏差（非有限数，见 ARCH §8 第 32 条与 `kiana-json` 模块文档）。
//!
//! ## `node:path.join` 语义
//!
//! TS 的目录辅助函数全部用 `path.join`（会折叠 `//`、`.` 与 `..`）。Rust 侧统一经
//! `join_path`（即 `paths::path_join`）返回 `PathBuf`，而不是裸 `PathBuf::join`，
//! 以保证各调用方拿到相同、规范化的路径文本。
//!
//! ## 目录家族与调用方
//!
//! 本模块不保存配置对象：除下面“缓存与‘启动时点’”一节列出的几处外，从环境变量或文件
//! 系统取值的函数都在**调用时**重新读取，调用方拿到的是当次结果而不是启动快照。
//!
//! | 家族 | 入口 | 组成 |
//! |---|---|---|
//! | 包内资源 | `get_package_dir()` | `KIANA_PACKAGE_DIR` 或 `current_exe()` 向上找到的包目录 + `src`/`dist` + 固定相对布局 |
//! | 用户配置 | `get_agent_dir()` | `KIANA_CODING_AGENT_DIR` 或 `<home>/.kiana/agent` + 固定文件名 |
//! | 安装方式 | `detect_install_method()` | `{package_dir}\0{exec_path}` 的子串匹配 |
//! | 自更新 | `get_self_update_command()` | 命令构造 + 托管判定 + 可写性判定 |
//! | 安装变更 | `detect_install_change()` | 读 `package.json` 并与 `VERSION` 比较 |
//! | 服务 URL | `get_api_base_url()` 等 | 纯环境变量，未配置即关闭 |
//!
//! 主要调用方：
//!
//! - `kiana-coding-agent` 的 `package_manager_cli`：`get_self_update_command` /
//!   `get_self_update_unavailable_instruction` 驱动包管理 CLI 的 update 流程，按
//!   `SelfUpdateCommand::steps` 逐条执行（有卸载步骤时先卸载旧包名）。
//! - `kiana-interactive` 的 `interactive_mode::session`：`detect_install_change(None)` 决定
//!   是否提示“运行期间安装被更新/移除”。
//! - `kiana-extensions` 的 `virtual_modules`、`kiana-resources`、`kiana-theme` 等：读取
//!   常量与目录辅助函数。
//!
//! ## 何时访问文件系统 / 启动子进程
//!
//! - `get_package_dir()` 在未设置 `KIANA_PACKAGE_DIR` 时，从 `current_exe()` 的父目录开始
//!   逐级检查 `package.json` 是否存在；`find_node_package_dir` 同理。
//! - `detect_install_change` 读取 `package.json`；`is_self_update_path_writable` 做
//!   `access(W_OK)`（Unix）或 `metadata().permissions().readonly()`（Windows）。
//! - `get_global_package_roots` / `get_self_update_command_for_method` 可能同步执行
//!   `pnpm root -g`、`yarn global dir`、`bun pm bin -g`、`<npm> root -g` 等探测命令；
//!   这些失败被吞成 `None`，不会向上传播。
//! - 其余目录辅助函数只做路径计算（`path.resolve` 会读取进程 cwd），不检查目标文件是否
//!   存在；例外是 `src_or_dist` 对 `<packageDir>/src` 的存在性检查。URL 函数只读环境变量。
//!
//! ## 缓存与“启动时点”
//!
//! - `startup_package_json_path()`：`LazyLock`，以**首次调用**为 Rust 的“启动”时点读一次
//!   `package.json`，此后不再重读（TS 是模块加载时捕获，见“已知偏差 3”）。
//! - `get_quickjs_wasm_path()`：`OnceLock`，**首次结果（包括 `None`）永久缓存**；之后再调用
//!   [`set_embedded_quickjs_wasm_path`] 或修改 `KIANA_QUICKJS_WASM` 都不会改变返回值。
//! - [`get_codemode_worker_specifier`] 的 `BundledNode` 分支：进程级 `Mutex` 缓存
//!   `data:` URL；原生构建（`Unbundled`）走不到该分支。
//!
//! ## 注意事项
//!
//! - `KIANA_PACKAGE_DIR` 分支只经 `normalize_path`（`~` 展开、`file://` 转换、Windows
//!   shell 路径规范化），**不转绝对路径**；未设置时从 `current_exe()` 向上找到的目录本身
//!   是绝对路径。需要绝对路径的辅助函数（README/docs/examples/CHANGELOG）自己调用
//!   `path_resolve`，其余只用 `path.join`。测试
//!   `relative_package_dir_yields_absolute_resolved_asset_paths` 锁定了这一差异。
//! - 进程级测试钩子（`*_for_test`）用 `Mutex` 保存 override，改变的是整个进程的行为；
//!   测试必须串行化（`tests/config.rs` 用 `ENV_LOCK`），生产代码不要调用。

use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex, OnceLock};

use regex::Regex;

use crate::paths::{
    PathInputOptions, contains_js_whitespace, js_trim, normalize_path, path_basename, path_dirname,
    path_join, path_resolve, path_sep,
};
use crate::text::strip_bom;

/// `package.json#name`。
pub const PACKAGE_NAME: &str = "@earendil-works/kiana-coding-agent";
/// `kianaConfig.name` 的回退值（也是 CLI 名）。
pub const APP_NAME: &str = "kiana";
/// `kianaConfig.name` 缺省时的标题。
pub const APP_TITLE: &str = "Kiana";
/// `kianaConfig.configDir`：`~/.kiana`。
pub const CONFIG_DIR_NAME: &str = ".kiana";
/// `package.json#version`（`kiana --version` 的输出）。
pub const VERSION: &str = "1.4.0";

/// TS 的 `isBunBinary`：原生二进制恒 `false`。
pub const IS_BUN_BINARY: bool = false;
/// TS 的 `isBunRuntime`：原生二进制恒 `false`。
pub const IS_BUN_RUNTIME: bool = false;
/// TS 的 `isBundledNode`：原生二进制恒 `false`。
pub const IS_BUNDLED_NODE: bool = false;

/// `KIANA_CODING_AGENT_DIR`（由 `APP_NAME` 派生）。
pub const ENV_AGENT_DIR: &str = "KIANA_CODING_AGENT_DIR";
/// `KIANA_CODING_AGENT_SESSION_DIR`（由 `APP_NAME` 派生）。
pub const ENV_SESSION_DIR: &str = "KIANA_CODING_AGENT_SESSION_DIR";
/// Kiana 自建服务（模型目录、版本检查、安装器、遥测、OAuth 元数据）的根 URL。
///
/// 未配置时所有相关网络功能保持关闭；不存在隐式上游默认值。
pub const ENV_API_BASE_URL: &str = "KIANA_API_BASE_URL";
/// 托管安装器 release API 的覆盖值；未设置时由 [`ENV_API_BASE_URL`] 派生。
pub const ENV_INSTALLER_API_BASE: &str = "KIANA_INSTALLER_API_BASE";

// =============================================================================
// Install Method Detection
// =============================================================================

/// `InstallMethod`：安装方式。
///
/// 由 [`detect_install_method`] 对 `{package_dir}\0{exec_path}` 做**固定顺序**的子串匹配
/// 得到：先 pnpm，再 yarn、bun，最后 npm。顺序不能随意调整：`.../.pnpm/...` 路径同时包含
/// `/.pnpm/` 与 `/node_modules/`，若先判 npm 会被误分类；Bun 的
/// `/install/global/node_modules/` 同样包含 `/node_modules/`。子串匹配本身是纯字符串操作，
/// 不访问文件系统，也不验证对应包管理器是否真的存在。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstallMethod {
    /// Bun 编译出的自包含二进制（TS `isBunBinary`）。原生 Rust 构建的
    /// `IS_BUN_BINARY` 是 `false`，因此检测函数不会返回它。
    BunBinary,
    /// npm 全局安装：路径包含 `/npm/` 或 `/node_modules/`（在前几种都不匹配后）。
    Npm,
    /// pnpm 全局安装：路径包含 `/pnpm/` 或 `/.pnpm/`。
    Pnpm,
    /// yarn 全局安装：路径包含 `/yarn/` 或 `/.yarn/`。
    Yarn,
    /// bun 全局安装：路径包含 `/install/global/node_modules/`（或运行在 Bun 运行时）。
    Bun,
    /// 以上都不匹配（源码检出、包装脚本等），无法据此生成自更新命令。
    Unknown,
}

impl InstallMethod {
    /// `InstallMethod` 的 TS 字符串字面量（用于错误/提示文案）。
    ///
    /// 返回值是 `"bun-binary"`、`"npm"` 等固定小写拼写，直接插入用户可见文案；
    /// [`std::fmt::Display`] 只是转发它。
    pub fn as_str(self) -> &'static str {
        match self {
            InstallMethod::BunBinary => "bun-binary",
            InstallMethod::Npm => "npm",
            InstallMethod::Pnpm => "pnpm",
            InstallMethod::Yarn => "yarn",
            InstallMethod::Bun => "bun",
            InstallMethod::Unknown => "unknown",
        }
    }
}

impl std::fmt::Display for InstallMethod {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// 单个自更新命令步骤。
///
/// `display` 是给人看的命令行文本：参数含 JS 空白字符时用双引号包裹，否则原样拼接。
/// 它不做 shell 转义（参数里的 `"` 不会被处理），只用于展示与错误文案，不能当作可安全
/// 交给 shell 的字符串。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelfUpdateStep {
    /// 要执行的程序名（如 `"npm"`），不含参数。
    pub command: String,
    /// 传给程序的参数，按执行顺序排列。
    pub args: Vec<String>,
    /// 展示用文本：`command` 与 `args` 以空格连接，含空白的参数加双引号。
    pub display: String,
}

/// `SelfUpdateCommandStep`（`spec/ca-cli.md` §5.1 的旧名字）的别名。
pub type SelfUpdateCommandStep = SelfUpdateStep;

/// 自更新命令：安装步骤 + 可选的卸载步骤。
///
/// `command`/`args`/`display` 始终描述**安装**步骤（与 `steps` 的最后一项相同）。
/// `steps` 为 `Some` 表示包名发生变更、需要先卸载旧包名，顺序是 `[卸载, 安装]`，
/// 此时 `display` 是两段以 `" && "` 连接的完整命令；`steps` 为 `None` 时只执行安装步骤。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelfUpdateCommand {
    /// 安装步骤的程序名。
    pub command: String,
    /// 安装步骤的参数。
    pub args: Vec<String>,
    /// 展示用文本；有卸载步骤时是 `"<uninstall.display> && <install.display>"`。
    pub display: String,
    /// `Some([卸载, 安装])` 表示需要先卸载旧包名；`None` 表示只安装。
    pub steps: Option<Vec<SelfUpdateStep>>,
}

/// 自更新的目标包：`string | { packageName, installSpec? }`。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SelfUpdatePackageTarget {
    /// 字符串形式：包名与 install spec 相同。
    Name(String),
    /// 对象形式。
    Target {
        /// 要安装的包名。
        package_name: String,
        /// 安装说明（可带版本/标签）；`None` 时退回 `package_name`。
        install_spec: Option<String>,
    },
}

impl From<&str> for SelfUpdatePackageTarget {
    fn from(value: &str) -> Self {
        SelfUpdatePackageTarget::Name(value.to_string())
    }
}

impl From<String> for SelfUpdatePackageTarget {
    fn from(value: String) -> Self {
        SelfUpdatePackageTarget::Name(value)
    }
}

/// 把 [`SelfUpdatePackageTarget`] 拆成 `(package_name, install_spec)`。
///
/// `install_spec` 缺省（`None`）时等于 `package_name`：命令里安装的始终是 `install_spec`，
/// 而 `package_name` 用于“是否与当前包名相同”的判断（决定要不要先生成卸载步骤）。
fn normalize_self_update_package_target(target: &SelfUpdatePackageTarget) -> (String, String) {
    match target {
        SelfUpdatePackageTarget::Name(name) => (name.clone(), name.clone()),
        SelfUpdatePackageTarget::Target {
            package_name,
            install_spec,
        } => (
            package_name.clone(),
            install_spec.clone().unwrap_or_else(|| package_name.clone()),
        ),
    }
}

/// 构造单个步骤，并按 TS 规则生成展示文本。
///
/// 只有含 JS 空白字符的参数会被双引号包裹（`contains_js_whitespace`），不对引号做转义。
fn make_self_update_command_step(command: &str, args: &[String]) -> SelfUpdateStep {
    let mut parts: Vec<String> = Vec::with_capacity(args.len() + 1);
    for arg in std::iter::once(command).chain(args.iter().map(String::as_str)) {
        // JS 的 `\s` 判定（含 NBSP 等 Unicode 空白），不是 Rust 的 `char::is_whitespace`。
        if contains_js_whitespace(arg) {
            parts.push(format!("\"{arg}\""));
        } else {
            parts.push(arg.to_string());
        }
    }
    SelfUpdateStep {
        command: command.to_string(),
        args: args.to_vec(),
        display: parts.join(" "),
    }
}

/// 把安装步骤与可选卸载步骤合成 [`SelfUpdateCommand`]。
///
/// 有卸载步骤时：`steps` 顺序固定为 `[卸载, 安装]`（调用方按序执行），顶层字段仍复制
/// 安装步骤；`display` 拼接为 `"<uninstall> && <install>"`，与实际执行顺序一致。
fn make_self_update_command(
    install_step: SelfUpdateStep,
    uninstall_step: Option<SelfUpdateStep>,
) -> SelfUpdateCommand {
    match uninstall_step {
        None => SelfUpdateCommand {
            command: install_step.command,
            args: install_step.args,
            display: install_step.display,
            steps: None,
        },
        Some(uninstall_step) => SelfUpdateCommand {
            command: install_step.command.clone(),
            args: install_step.args.clone(),
            display: format!("{} && {}", uninstall_step.display, install_step.display),
            steps: Some(vec![uninstall_step, install_step]),
        },
    }
}

/// pnpm 全局安装的路径特征：捕获 `<...>/global/<版本或槽位>` 前缀。
///
/// 只匹配 `global/` 下**直接一层**目录再进入 `.pnpm/` 的布局（如
/// `.../global/5/.pnpm/...`）；捕获组 1 用于在 `pnpm root -g` 不可用时推算 bin 目录。
static PNPM_GLOBAL_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(.*[\\/]global[\\/][^\\/]+)[\\/]\.pnpm[\\/]").expect("valid pnpm global regex")
});

/// `detectInstallMethod()` 的可注入版本：`resolvedPath = {package_dir}\0{exec_path}`。
/// **test hook, not public API**。
///
/// 判定前统一 `to_lowercase()` 并把 `\` 换成 `/`，因此 Windows 路径与 POSIX 路径走同一套
/// 子串规则（见 `tests/config.rs::detects_pnpm_from_windows_pnpm_install_paths`）。匹配顺序
/// 见 [`InstallMethod`]：更宽泛的 `/node_modules/` 必须放在最后。
#[doc(hidden)]
pub fn detect_install_method_from(package_dir: &str, exec_path: Option<&str>) -> InstallMethod {
    // 原生构建恒为 false，保留该分支以对齐 TS 的 isBunBinary（模块文档“运行期检测常量”）。
    if IS_BUN_BINARY {
        return InstallMethod::BunBinary;
    }
    // 两个输入用 NUL 拼接后一起小写化、把反斜杠归一成 `/`：Windows 安装路径因此能和 POSIX
    // 路径用同一组子串匹配。NUL 不可能出现在路径中，不会额外制造匹配。
    let resolved_path = format!("{package_dir}\0{}", exec_path.unwrap_or(""))
        .to_lowercase()
        .replace('\\', "/");

    // 顺序敏感：保留最先命中的分类。pnpm 的 `/.pnpm/` 路径同时含 `/node_modules/`。
    if resolved_path.contains("/pnpm/") || resolved_path.contains("/.pnpm/") {
        return InstallMethod::Pnpm;
    }
    if resolved_path.contains("/yarn/") || resolved_path.contains("/.yarn/") {
        return InstallMethod::Yarn;
    }
    // Bun 的全局目录 `/install/global/node_modules/` 也含 `/node_modules/`，必须先于 npm。
    if IS_BUN_RUNTIME || resolved_path.contains("/install/global/node_modules/") {
        return InstallMethod::Bun;
    }
    // 最后才回退到最宽泛的 npm 特征。
    if resolved_path.contains("/npm/") || resolved_path.contains("/node_modules/") {
        return InstallMethod::Npm;
    }
    InstallMethod::Unknown
}

/// `detectInstallMethod()`。
///
/// 组合 [`get_package_dir`] 与 `process.execPath` 的等价物（[`get_package_dir`] 每次读
/// 环境变量；exec path 可被测试钩子覆盖），再交给 `detect_install_method_from`。
pub fn detect_install_method() -> InstallMethod {
    detect_install_method_from(
        &get_package_dir().to_string_lossy(),
        current_exec_path().as_deref(),
    )
}

/// `process.execPath` 的等价物；测试可用 [`set_exec_path_override_for_test`] 覆盖。
///
/// 状态编码：`None` = 没有 override，读真实的 `current_exe()`；`Some(None)` = 显式模拟
/// “没有 exec path”；`Some(Some(path))` = 返回该路径。锁只在读取 override 时持有。
static EXEC_PATH_OVERRIDE: Mutex<Option<Option<PathBuf>>> = Mutex::new(None);

/// `process.execPath` 的等价物：优先读测试 override，未设置时用 `current_exe()`；
/// 取不到路径时返回 `None`（`detect_install_method` 会把 `None` 当作空串）。
fn current_exec_path() -> Option<String> {
    let guard = EXEC_PATH_OVERRIDE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(value) = guard.as_ref() {
        return value
            .as_ref()
            .map(|path| path.to_string_lossy().into_owned());
    }
    drop(guard);
    std::env::current_exe()
        .ok()
        .map(|path| path.to_string_lossy().into_owned())
}

/// **test hook, not public API**：模拟 `process.execPath`（`None` 表示“没有 exec path”）。
#[doc(hidden)]
pub fn set_exec_path_override_for_test(path: Option<PathBuf>) {
    let mut guard = EXEC_PATH_OVERRIDE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    *guard = Some(path);
}

/// **test hook, not public API**：恢复真实的 `process.execPath`。
#[doc(hidden)]
pub fn clear_exec_path_override_for_test() {
    let mut guard = EXEC_PATH_OVERRIDE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    *guard = None;
}

/// 测试 override：保存入口路径（`process.argv[1]`）的**原始值**；解析在
/// `entrypoint_argument` 中进行，因此这里的值可能是相对路径。测试钩子见
/// [`set_entrypoint_override_for_test`]。
///
/// Node 的 `process.argv[1]` 是**入口脚本**（Node 会把它 `path.resolve` 成绝对路径）；原生
/// 二进制没有脚本参数，最接近的等价物是可执行文件自身（`current_exe`，`argv[0]` 的实际
/// 解析结果）。早期实现误用 `args_os().nth(1)`（第一个用户参数），会让
/// `getEntrypointPackageDir()` 在真实安装里永远找不到包目录（#self-update）。
///
/// 状态编码与 `EXEC_PATH_OVERRIDE` 相同：`None` = 无 override（用 `current_exe()`）；
/// `Some(None)` = 显式模拟“没有 argv[1]”；`Some(Some(path))` = 使用该路径。
static ENTRYPOINT_OVERRIDE: Mutex<Option<Option<PathBuf>>> = Mutex::new(None);

/// **test hook, not public API**：模拟 `process.argv[1]`（`None` 表示“没有 argv[1]”）。
#[doc(hidden)]
pub fn set_entrypoint_override_for_test(path: Option<PathBuf>) {
    let mut guard = ENTRYPOINT_OVERRIDE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    *guard = Some(path);
}

/// **test hook, not public API**：恢复真实的入口路径。
#[doc(hidden)]
pub fn clear_entrypoint_override_for_test() {
    let mut guard = ENTRYPOINT_OVERRIDE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    *guard = None;
}

/// 取得入口路径并做 `path.resolve`，对应 Node 启动时对 `argv[1]` 的处理。
///
/// 优先返回测试 override，否则用 `current_exe()`（见 `ENTRYPOINT_OVERRIDE` 的说明）。
/// 相对路径会被 `path_resolve` 转成基于进程 cwd 的绝对路径；`current_exe()` 失败且没有
/// override 时返回 `None`。有 override 时在锁内复制；没有 override 时也在锁内调用
/// `current_exe()`；只有 `path_resolve` 在锁外执行。
fn entrypoint_argument() -> Option<PathBuf> {
    let selected = {
        let guard = ENTRYPOINT_OVERRIDE
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        match guard.as_ref() {
            Some(value) => value.clone(),
            None => std::env::current_exe().ok(),
        }
    };
    // Node 把 argv[1] 交给 `path.resolve` 变成绝对路径后再取 dirname。
    selected.map(|path| PathBuf::from(path_resolve(&[&path.to_string_lossy()])))
}

/// 从 `KIANA_PACKAGE_DIR` 读取包目录（每次调用都读 env，**不缓存**）。
fn env_package_dir() -> Option<String> {
    std::env::var_os("KIANA_PACKAGE_DIR")
        .map(|value| value.to_string_lossy().into_owned())
        .filter(|value| !value.is_empty())
}

/// `getInferredNpmInstall()`：从包目录反推 npm 全局安装的 `(node_modules 根, prefix)`。
///
/// 只识别 `<prefix>/lib/node_modules/[<@scope>/]<pkg>` 布局；其它布局（例如没有 `lib` 层的
/// `<prefix>/node_modules/...`）返回 `None`。prefix 供 npm 命令的 `--prefix` 使用。
///
/// 路径拆分按 Windows 语义的条件是宿主为 Windows **或**包目录字符串里含 `\`：后者让非
/// Windows 上也能复现 TS 的 win32 夹具（见
/// `tests/config.rs::does_not_infer_windows_npm_custom_prefixes_from_package_paths`）。
fn get_inferred_npm_install() -> Option<(String, String)> {
    let package_dir = get_package_dir().to_string_lossy().into_owned();
    let win32_style = cfg!(windows) || package_dir.contains('\\');
    let dirname = |path: &str| -> String {
        if win32_style {
            crate::paths::win32_dirname_for_shell(path)
        } else {
            path_dirname(path)
        }
    };
    let basename = |path: &str| -> String {
        if win32_style {
            crate::paths::win32_basename_for_shell(path)
        } else {
            path_basename(path)
        }
    };

    // 包目录的父目录可能是 `node_modules`（无 scope）或 `@scope`（有 scope）；
    // `@scope` 上一级必须是 `node_modules`，据此确认 root。
    let parent = dirname(&package_dir);
    let root =
        if basename(&parent).starts_with('@') && basename(&dirname(&parent)) == "node_modules" {
            Some(dirname(&parent))
        } else if basename(&parent) == "node_modules" {
            Some(parent)
        } else {
            None
        }?;

    // npm 的全局前缀布局要求 node_modules 挂在 `lib` 下；只有这时才能把 lib 的父目录
    // 当作 prefix 交给 `npm --prefix`。不满足就返回 None，让调用方执行不带 prefix 的命令。
    let root_parent = dirname(&root);
    if basename(&root_parent) == "lib" {
        Some((root, dirname(&root_parent)))
    } else {
        None
    }
}

/// `readCommandOutput(command, args, {requireSuccess?})`。
///
/// 失败且 `requireSuccess` 时返回 `Err(message)`（TS 的 `throw new Error(...)`）；
/// 冻结的公开签名无法抛错，调用方按“没有输出”处理（见模块文档的偏差 1）。
///
/// 成功只看退出码是否为 `Some(0)`：stdout 经 JS `trim` 后为空也算“没有输出”（返回
/// `Ok(None)`）。退出码非 0、被信号杀死或启动失败都走失败路径；失败原因优先取
/// spawn 错误的 `Display`，其次非空 stderr，最后退化为 `exit code <n>` / `exit code unknown`。
/// 展示文本里命令与参数用空格原样连接，不加引号。
fn read_command_output(
    command: &str,
    args: &[String],
    require_success: bool,
) -> Result<Option<String>, String> {
    let options = crate::child_process::SpawnSyncOptions::default();
    let result = crate::child_process::spawn_process_sync(command, args, &options);
    // 只有“正常退出码 0”算成功；stderr 在成功路径被忽略。
    if result.status == Some(0) {
        let trimmed = js_trim(&result.stdout);
        return Ok(if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_string())
        });
    }
    if require_success {
        let reason = match &result.error {
            Some(error) => error.to_string(),
            None => {
                let stderr = js_trim(&result.stderr);
                if !stderr.is_empty() {
                    stderr.to_string()
                } else {
                    match result.status {
                        Some(status) => format!("exit code {status}"),
                        None => "exit code unknown".to_string(),
                    }
                }
            }
        };
        let rendered = std::iter::once(command)
            .chain(args.iter().map(String::as_str))
            .collect::<Vec<_>>()
            .join(" ");
        return Err(format!("Failed to run {rendered}: {reason}"));
    }
    Ok(None)
}

fn string_args(args: &[&str]) -> Vec<String> {
    args.iter().map(|arg| (*arg).to_string()).collect()
}

fn extend_args(base: &[String], extra: &[&str]) -> Vec<String> {
    let mut out = base.to_vec();
    out.extend(string_args(extra));
    out
}

/// `getSelfUpdateCommandForMethod(...)`：按安装方式构造命令，**不检查**托管与可写性。
///
/// 共同规则：安装步骤总是使用 `install_spec`；仅当目标包名 `package_name` 与当前
/// `installed_package_name` 不同时，额外生成“卸载旧包名”的步骤并排在安装之前（返回的
/// `SelfUpdateCommand::display` 因而形如 `"<remove> && <install>"`）。
///
/// `BunBinary` 与 `Unknown` 没有自更新命令，返回 `None`。包管理器探测失败只影响是否补上
/// 额外参数，不会让 `Pnpm`/`Yarn`/`Bun`/`Npm` 分支从 `Some` 变成 `None`。
fn get_self_update_command_for_method(
    method: InstallMethod,
    installed_package_name: &str,
    update_package_target: &SelfUpdatePackageTarget,
    npm_command: Option<&[String]>,
) -> Option<SelfUpdateCommand> {
    let (package_name, install_spec) = normalize_self_update_package_target(update_package_target);
    // 规则见函数文档：只有包名变更时才生成卸载步骤，且卸载排在安装之前。
    match method {
        InstallMethod::BunBinary => None,
        InstallMethod::Pnpm => {
            // pnpm 能报告全局根时无需额外参数；报告不了（命令失败或输出为空）才显式指定
            // global-bin-dir。
            let has_global_root = read_command_output("pnpm", &string_args(&["root", "-g"]), false)
                .ok()
                .flatten()
                .is_some();
            let mut bin_dir_args: Vec<String> = Vec::new();
            if !has_global_root {
                let package_dir = get_package_dir().to_string_lossy().into_owned();
                if let Some(captures) = PNPM_GLOBAL_RE.captures(&package_dir) {
                    let matched = captures.get(1).map(|m| m.as_str()).unwrap_or_default();
                    // PNPM_HOME 优先（非空才算设置）；否则取 `<...>/global/<槽位>` 的上两级
                    // 目录作为 bin 目录。
                    let bin_dir = std::env::var("PNPM_HOME")
                        .ok()
                        .filter(|value| !value.is_empty())
                        .unwrap_or_else(|| path_dirname(&path_dirname(matched)));
                    bin_dir_args.push(format!("--config.global-bin-dir={bin_dir}"));
                }
            }
            let install_args = extend_args(
                &extend_args(
                    &string_args(&[
                        "install",
                        "-g",
                        "--ignore-scripts",
                        "--config.minimumReleaseAge=0",
                    ]),
                    &bin_dir_args.iter().map(String::as_str).collect::<Vec<_>>(),
                ),
                &[&install_spec],
            );
            let install_step = make_self_update_command_step("pnpm", &install_args);
            let uninstall_step = if package_name == installed_package_name {
                None
            } else {
                let uninstall_args = extend_args(
                    &extend_args(
                        &string_args(&["remove", "-g"]),
                        &bin_dir_args.iter().map(String::as_str).collect::<Vec<_>>(),
                    ),
                    &[installed_package_name],
                );
                Some(make_self_update_command_step("pnpm", &uninstall_args))
            };
            Some(make_self_update_command(install_step, uninstall_step))
        }
        InstallMethod::Yarn => {
            let install_step = make_self_update_command_step(
                "yarn",
                &extend_args(
                    &string_args(&["global", "add", "--ignore-scripts"]),
                    &[&install_spec],
                ),
            );
            let uninstall_step = if package_name == installed_package_name {
                None
            } else {
                Some(make_self_update_command_step(
                    "yarn",
                    &extend_args(
                        &string_args(&["global", "remove"]),
                        &[installed_package_name],
                    ),
                ))
            };
            Some(make_self_update_command(install_step, uninstall_step))
        }
        InstallMethod::Bun => {
            let install_step = make_self_update_command_step(
                "bun",
                &extend_args(
                    &string_args(&[
                        "install",
                        "-g",
                        "--ignore-scripts",
                        "--minimum-release-age=0",
                    ]),
                    &[&install_spec],
                ),
            );
            let uninstall_step = if package_name == installed_package_name {
                None
            } else {
                Some(make_self_update_command_step(
                    "bun",
                    &extend_args(
                        &string_args(&["uninstall", "-g"]),
                        &[installed_package_name],
                    ),
                ))
            };
            Some(make_self_update_command(install_step, uninstall_step))
        }
        InstallMethod::Npm => {
            // 显式配置了 npmCommand：第一个元素是程序名、其余原样作为前缀参数（例如
            // `["npm", "--prefix", "/x"]`），此时不再从包目录反推 prefix。
            // 未配置或空数组时使用 "npm"，并用 get_inferred_npm_install 补 --prefix。
            let configured = npm_command
                .map(|command| !command.is_empty())
                .unwrap_or(false);
            let (command, npm_args) = split_npm_command(npm_command);
            let inferred = if configured {
                None
            } else {
                get_inferred_npm_install()
            };
            let mut prefix_args = npm_args.clone();
            if let Some((_, prefix)) = inferred {
                prefix_args.push("--prefix".to_string());
                prefix_args.push(prefix);
            }
            let install_args = extend_args(
                &extend_args(
                    &prefix_args,
                    &["install", "-g", "--ignore-scripts", "--min-release-age=0"],
                ),
                &[&install_spec],
            );
            let install_step = make_self_update_command_step(&command, &install_args);
            let uninstall_step = if package_name == installed_package_name {
                None
            } else {
                let uninstall_args = extend_args(
                    &extend_args(&prefix_args, &["uninstall", "-g"]),
                    &[installed_package_name],
                );
                Some(make_self_update_command_step(&command, &uninstall_args))
            };
            Some(make_self_update_command(install_step, uninstall_step))
        }
        InstallMethod::Unknown => None,
    }
}

/// `const [command = "npm", ...npmArgs] = npmCommand ?? []`。
fn split_npm_command(npm_command: Option<&[String]>) -> (String, Vec<String>) {
    match npm_command {
        Some(values) if !values.is_empty() => (values[0].clone(), values[1..].to_vec()),
        _ => ("npm".to_string(), Vec::new()),
    }
}

/// Bun 全局安装的默认 `node_modules` 根：`<home>/.bun/install/global/node_modules`。
///
/// `dirs::home_dir()` 取不到时以空串参与 `path.join`，结果会退化为相对路径；这是
/// `home_dir()` 为 `None` 时的失败路径。
fn bun_global_root() -> String {
    let home = dirs::home_dir().unwrap_or_default();
    join_path(&[
        &home.to_string_lossy(),
        ".bun",
        "install",
        "global",
        "node_modules",
    ])
    .to_string_lossy()
    .into_owned()
}

/// `getGlobalPackageRoots(method, _packageName, npmCommand?)`。
///
/// 返回该安装方式**可能**把包放在哪些 `node_modules` 根下，供
/// `is_managed_by_global_package_manager` 做前缀比较。探测顺序即返回顺序；其中可能同步
/// 执行 `pnpm root -g`、`yarn global dir`、`bun pm bin -g`、`<npm> root -g`。命令不存在
/// 或失败时该项被跳过（`read_command_output` 的 `Err` 在这里统一 `.ok()` 丢弃）。
/// `BunBinary`/`Unknown` 返回空表。
///
/// - npm：显式配置的 npmCommand 指向 `bun` 时，额外返回 bun 的默认根；否则“命令报告的
///   root”在前，未配置 npmCommand 时再追加从包目录反推的 root。
/// - pnpm：`pnpm root -g` 的根与其父目录；探测失败时退回从包路径正则提取的 global 目录。
/// - yarn：`yarn global dir` 及其下的 `node_modules`。
/// - bun：默认的 `~/.bun/install/global/node_modules`，加上 `bun pm bin -g` 推导的路径。
///
/// `_package_name` 当前未使用（与 TS 签名保持一致）。
fn get_global_package_roots(
    method: InstallMethod,
    _package_name: &str,
    npm_command: Option<&[String]>,
) -> Vec<String> {
    match method {
        InstallMethod::Npm => {
            let configured = npm_command
                .map(|command| !command.is_empty())
                .unwrap_or(false);
            let (command, npm_args) = split_npm_command(npm_command);
            // 特例：配置的“npm 命令”实际是 bun 时，改用 bun 自己的 `pm bin -g` 探测，
            // 而不是走 npm 的 `root -g`。
            if configured && command == "bun" {
                let bun_bin = read_command_output(
                    &command,
                    &extend_args(&npm_args, &["pm", "bin", "-g"]),
                    true,
                )
                .ok()
                .flatten();
                let mut roots = vec![bun_global_root()];
                if let Some(bun_bin) = bun_bin {
                    roots.push(path_join(&[
                        &path_dirname(&bun_bin),
                        "install",
                        "global",
                        "node_modules",
                    ]));
                }
                return roots;
            }
            // 普通 npm：只有显式配置时才要求 `root -g` 必须成功（require_success = configured）；
            // 未配置时失败只会得到空 root，再追加反推的 root。
            let root = read_command_output(
                &command,
                &extend_args(&npm_args, &["root", "-g"]),
                configured,
            )
            .ok()
            .flatten();
            let inferred = if configured {
                None
            } else {
                get_inferred_npm_install().map(|(root, _)| root)
            };
            let mut roots = Vec::new();
            if let Some(root) = root {
                roots.push(root);
            }
            if let Some(inferred) = inferred {
                roots.push(inferred);
            }
            roots
        }
        InstallMethod::Pnpm => {
            let root = read_command_output("pnpm", &string_args(&["root", "-g"]), false)
                .ok()
                .flatten();
            // 同时给出 root 与其父目录两个候选；探测失败则退回正则捕获的 global 目录。
            if let Some(root) = root {
                return vec![root.clone(), path_dirname(&root)];
            }
            let package_dir = get_package_dir().to_string_lossy().into_owned();
            match PNPM_GLOBAL_RE.captures(&package_dir) {
                Some(captures) => vec![
                    captures
                        .get(1)
                        .map(|m| m.as_str())
                        .unwrap_or_default()
                        .to_string(),
                ],
                None => Vec::new(),
            }
        }
        InstallMethod::Yarn => {
            let dir = read_command_output("yarn", &string_args(&["global", "dir"]), false)
                .ok()
                .flatten();
            // yarn 的全局目录本身与其中的 node_modules 都可能是包目录的前缀。
            match dir {
                Some(dir) => vec![dir.clone(), path_join(&[&dir, "node_modules"])],
                None => Vec::new(),
            }
        }
        InstallMethod::Bun => {
            // 默认根始终作为一个候选，`bun pm bin -g` 能报告时再补一个。
            let bun_bin = read_command_output("bun", &string_args(&["pm", "bin", "-g"]), false)
                .ok()
                .flatten();
            let mut roots = vec![bun_global_root()];
            if let Some(bun_bin) = bun_bin {
                roots.push(path_join(&[
                    &path_dirname(&bun_bin),
                    "install",
                    "global",
                    "node_modules",
                ]));
            }
            roots
        }
        InstallMethod::BunBinary | InstallMethod::Unknown => Vec::new(),
    }
}

/// 把路径转成可比较的规范形式；路径不存在（或无法获取元数据）时返回 `None`。
///
/// 先用 `path_resolve` 转绝对路径（相对路径按进程 cwd 解析），再用 `Path::exists()` 检查
/// 存在性（跟随符号链接，断链视为不存在）。`resolve_symlinks` 为真时继续用
/// `fs::canonicalize` 解析符号链接，失败（权限、竞态等）返回 `None`。Windows 上最后统一
/// 转小写，使比较变成大小写不敏感。
fn normalize_existing_path_for_comparison(path: &str, resolve_symlinks: bool) -> Option<String> {
    let resolved_path = path_resolve(&[path]);
    if !Path::new(&resolved_path).exists() {
        return None;
    }
    let mut normalized_path = resolved_path;
    if resolve_symlinks {
        normalized_path = std::fs::canonicalize(&normalized_path)
            .ok()?
            .to_string_lossy()
            .into_owned();
    }
    if cfg!(windows) {
        normalized_path = normalized_path.to_lowercase();
    }
    Some(normalized_path)
}

/// 返回 `path` 的所有可比较形态：词法绝对路径与符号链接解析后的路径。
///
/// 保持 `[词法, 解析后]` 顺序并按值去重；路径不存在时两者都失败，返回空 `Vec`。
/// 调用方对两份候选做前缀匹配，因此词法路径或符号链接解析后的路径任一路径落在 root 下
/// 都算命中。
fn get_path_comparison_candidates(path: &str) -> Vec<String> {
    let mut candidates: Vec<String> = Vec::new();
    for candidate in [
        normalize_existing_path_for_comparison(path, false),
        normalize_existing_path_for_comparison(path, true),
    ]
    .into_iter()
    .flatten()
    {
        if !candidates.contains(&candidate) {
            candidates.push(candidate);
        }
    }
    candidates
}

/// `getEntrypointPackageDir()`：从入口路径的目录逐级向上找 `package.json`。
///
/// 入口路径是 Node 的 `process.argv[1]`；原生二进制用 `entrypoint_argument` 的缺省值
/// （可执行文件自身）。只检查 `package.json` 是否存在，不读取内容；找不到（或入口路径为
/// `None`）返回 `None`。这个目录与 `get_package_dir()` 可能不同（例如 pnpm 的
/// store 路径与入口所在的全局目录），托管判定会同时使用两者。
fn get_entrypoint_package_dir() -> Option<String> {
    let entrypoint = entrypoint_argument()?;
    let mut dir = entrypoint.parent()?.to_path_buf();
    // 从入口所在目录开始，每轮先检查当前目录，再上移一层；到文件系统根（parent 为
    // None 或等于自身）时结束。
    loop {
        let parent = dir.parent()?.to_path_buf();
        if parent == dir {
            break;
        }
        if dir.join("package.json").exists() {
            return Some(dir.to_string_lossy().into_owned());
        }
        dir = parent;
    }
    None
}

/// Unix 上的可写性：`access(path, W_OK) == 0`。
///
/// 路径含 NUL 字节、目标不存在、权限不足或 `access` 的任何失败都返回 `false`。结果只
/// 反映调用瞬间的权限；检查与后续真正写入之间没有加锁，权限可能已经变化。
#[cfg(unix)]
fn path_is_writable(path: &Path) -> bool {
    use std::os::unix::ffi::OsStrExt;
    let Ok(c_path) = std::ffi::CString::new(path.as_os_str().as_bytes()) else {
        return false;
    };
    // SAFETY: `access(2)` 只读取路径字符串。
    unsafe { libc::access(c_path.as_ptr(), libc::W_OK) == 0 }
}

/// Windows 上 `accessSync(path, W_OK)` 的等价物：libuv 的 `uv_fs_access` 只检查
/// `FILE_ATTRIBUTE_READONLY`（不做 ACL 检查），std 的 `Permissions::readonly()` 正好映射到
/// 该属性，因此无需 `windows-sys`（§8 第 26 条）。
#[cfg(windows)]
fn path_is_writable(path: &Path) -> bool {
    match std::fs::metadata(path) {
        Ok(metadata) => !metadata.permissions().readonly(),
        Err(_) => false,
    }
}

#[cfg(not(any(unix, windows)))]
fn path_is_writable(path: &Path) -> bool {
    let _ = path;
    true
}

/// `isSelfUpdatePathWritable()`：`accessSync(packageDir, W_OK)` 且
/// `accessSync(dirname(packageDir), W_OK)`（dirname 用 `node:path.dirname` 语义）。
///
/// 两个检查都必须通过；`get_package_dir()` 每次调用重新读环境变量。
fn is_self_update_path_writable() -> bool {
    let package_dir = get_package_dir();
    let package_dir_str = package_dir.to_string_lossy().into_owned();
    let parent_dir = path_dirname(&package_dir_str);
    path_is_writable(Path::new(&package_dir_str)) && path_is_writable(Path::new(&parent_dir))
}

/// 包目录是否位于某个全局包管理器的 root 之下。
///
/// 候选包目录 = `get_package_dir()` 与 `get_entrypoint_package_dir()`（各自去空）；两侧都
/// 先转成词法路径与符号链接解析结果各一份。比较时给 root 补上平台分隔符做前缀，避免
/// `/usr/lib/node_modules-x` 误匹配 `/usr/lib/node_modules`。任一 root 命中即返回 `true`。
///
/// `get_global_package_roots` 内部可能同步执行包管理器命令。命令失败并不单调地缩小
/// 候选集合：Pnpm 探测失败时会退回到从包路径正则提取的候选，可能替换（而非缩减）成功
/// 时的候选；其它安装方式才只减少候选。因此命令失败不必然让本函数更容易返回 `false`。
fn is_managed_by_global_package_manager(
    method: InstallMethod,
    package_name: &str,
    npm_command: Option<&[String]>,
) -> bool {
    let mut package_dirs: Vec<String> = Vec::new();
    let package_dir = get_package_dir().to_string_lossy().into_owned();
    if !package_dir.is_empty() {
        package_dirs.push(package_dir);
    }
    if let Some(entrypoint) =
        get_entrypoint_package_dir().filter(|entrypoint| !entrypoint.is_empty())
    {
        package_dirs.push(entrypoint);
    }
    let package_dir_candidates: Vec<String> = package_dirs
        .iter()
        .flat_map(|dir| get_path_comparison_candidates(dir))
        .collect();
    let separator = path_sep();
    get_global_package_roots(method, package_name, npm_command)
        .iter()
        .any(|root| {
            get_path_comparison_candidates(root)
                .iter()
                .any(|normalized_root| {
                    let root_prefix = if normalized_root.ends_with(separator) {
                        normalized_root.clone()
                    } else {
                        format!("{normalized_root}{separator}")
                    };
                    package_dir_candidates
                        .iter()
                        .any(|package_dir| package_dir.starts_with(&root_prefix))
                })
        })
}

/// `getSelfUpdateCommand(packageName, npmCommand?, updatePackageTarget = packageName)`。
///
/// 返回要执行的自更新命令；`None` 表示“不应由本程序自更新”。判定顺序：
///
/// 1. 按 [`detect_install_method`] 的结果构造命令（`BunBinary`/`Unknown` 直接 `None`）。
///    这一步可能同步执行 `pnpm root -g` 等探测命令。
/// 2. 包目录（或入口所在目录）必须位于该安装方式的全局 root 之下
///    （`is_managed_by_global_package_manager`）。
/// 3. 包目录与其父目录都必须可写（`is_self_update_path_writable`）。
///
/// 由于第 1 步先于第 2/3 步，即使最终返回 `None`，也可能已经运行过探测命令。`update_target`
/// 为 `None` 时取 `package_name` 作为目标；`npm_command` 是 npm 的程序名与前缀参数
/// （`None` 或空切片按未配置处理）。
pub fn get_self_update_command(
    package_name: &str,
    npm_command: Option<&[String]>,
    update_target: Option<SelfUpdatePackageTarget>,
) -> Option<SelfUpdateCommand> {
    let target =
        update_target.unwrap_or_else(|| SelfUpdatePackageTarget::Name(package_name.to_string()));
    let method = detect_install_method();
    let command = get_self_update_command_for_method(method, package_name, &target, npm_command)?;
    if !is_managed_by_global_package_manager(method, package_name, npm_command)
        || !is_self_update_path_writable()
    {
        return None;
    }
    Some(command)
}

/// `getSelfUpdateUnavailableInstruction(packageName, npmCommand?, updatePackageTarget = packageName)`。
///
/// 与 [`get_self_update_command`] 不同，这里**总是**返回一段可展示的文案（即使安装方式
/// 无法识别），按以下顺序分支：
///
/// 1. `BunBinary`：固定提示从 GitHub Releases 下载（原生构建不可达）。
/// 2. 能构造命令，且“被托管但安装目录不可写”：给出 `command.display` 让用户手动执行。
/// 3. 能构造命令的其余情况：说明该安装不由全局包管理器管理（该分支也覆盖“被托管且可写”，
///    但那种情况下 [`get_self_update_command`] 会成功，正常流程不会走到这里）。
/// 4. 无法构造命令（`Unknown`）：用 `install_spec` 生成通用提示。
///
/// 分支 2/3 复用 [`get_self_update_command`] 的同一组判据，因此在正常调用流程（只在
/// `get_self_update_command` 返回 `None` 后展示文案）中二者互补；判定过程同样可能同步
/// 执行包管理器探测命令。
pub fn get_self_update_unavailable_instruction(
    package_name: &str,
    npm_command: Option<&[String]>,
    update_target: Option<SelfUpdatePackageTarget>,
) -> String {
    let target =
        update_target.unwrap_or_else(|| SelfUpdatePackageTarget::Name(package_name.to_string()));
    let (_, install_spec) = normalize_self_update_package_target(&target);
    let method = detect_install_method();
    if method == InstallMethod::BunBinary {
        return "Download from: https://github.com/earendil-works/kiana/releases/latest"
            .to_string();
    }
    let command = get_self_update_command_for_method(method, package_name, &target, npm_command);
    if let Some(command) = command {
        if is_managed_by_global_package_manager(method, package_name, npm_command)
            && !is_self_update_path_writable()
        {
            return format!(
                "This installation is managed by a global {method} install, but the install path is not writable. Update it yourself with: {}",
                command.display
            );
        }
        return format!(
            "This installation is not managed by a global {method} install. Update it with the package manager, wrapper, or source checkout that provides it."
        );
    }
    format!(
        "Update {install_spec} using the package manager, wrapper, or source checkout that provides this installation."
    )
}

/// `getUpdateInstruction(packageName)`。
///
/// 不经过 [`get_self_update_command`] 的“托管 + 可写”门槛：只要能构造出命令就返回
/// `"Run: <display>"`，否则退回 [`get_self_update_unavailable_instruction`]。因此它可能
/// 建议一条实际自更新会拒绝执行的命令（目录不可写或非托管）；本函数不执行命令，只生成
/// 文案。
pub fn get_update_instruction(package_name: &str) -> String {
    let method = detect_install_method();
    let target = SelfUpdatePackageTarget::Name(package_name.to_string());
    let command = get_self_update_command_for_method(method, package_name, &target, None);
    if let Some(command) = command {
        return format!("Run: {}", command.display);
    }
    get_self_update_unavailable_instruction(package_name, None, None)
}

// =============================================================================
// Package Asset Paths (shipped with executable)
// =============================================================================

/// 逐级向上找 `package.json` 的目录。
///
/// 起点目录 `start_dir` 自身也会被检查，之后才逐级上移。`dist/` 特例保留：目录名是
/// `dist` 且父目录也有 `package.json` → 返回父目录（build:binary 把 Bun 的元数据放进
/// `dist/`）。找不到返回 `start_dir`。
pub fn find_node_package_dir(start_dir: impl AsRef<Path>) -> PathBuf {
    let start_dir = start_dir.as_ref();
    let mut dir = start_dir.to_path_buf();
    while let Some(parent) = dir.parent().map(Path::to_path_buf) {
        if parent == dir {
            break;
        }
        if dir.join("package.json").exists() {
            let is_dist = dir.file_name().map(|name| name == "dist").unwrap_or(false);
            if is_dist && parent.join("package.json").exists() {
                return parent;
            }
            return dir;
        }
        dir = parent;
    }
    start_dir.to_path_buf()
}

/// `getPackageDir()`：`KIANA_PACKAGE_DIR` 优先（每次调用都读 env）。
///
/// 环境变量非空时返回其 `normalize_path` 结果（展开 `~`、转换 `file://`、Windows shell
/// 规范化），但**保持相对路径**。未设置时以 `current_exe()` 的父目录为起点向上找最近的
/// `package.json`（[`find_node_package_dir`]），找到则返回该目录，否则返回起点本身；
/// 可执行文件路径或父目录取不到时起点是空路径。`IS_BUN_BINARY` 分支在原生构建不可达。
pub fn get_package_dir() -> PathBuf {
    if let Some(env_dir) = env_package_dir() {
        return PathBuf::from(normalize_path(&env_dir, &PathInputOptions::default()));
    }
    if IS_BUN_BINARY {
        // Bun binary: process.execPath points to the compiled executable.
        return std::env::current_exe()
            .ok()
            .and_then(|path| path.parent().map(Path::to_path_buf))
            .unwrap_or_default();
    }
    // Rust 没有 __dirname：用可执行文件所在目录作为起点（见模块文档）。
    let start = std::env::current_exe()
        .ok()
        .and_then(|path| path.parent().map(Path::to_path_buf))
        .unwrap_or_default();
    find_node_package_dir(&start)
}

/// 选择包内的源码/产物目录名：`<packageDir>/src` 存在则为 `"src"`，否则为 `"dist"`。
///
/// 只检查 `src` 是否存在，不检查 `dist`；两者都不存在时也会选 `dist`。
fn src_or_dist(package_dir: &Path) -> &'static str {
    if package_dir.join("src").exists() {
        "src"
    } else {
        "dist"
    }
}

/// `join(...)`：TS 的 `node:path.join` 会折叠 `//`、`.` 与 `..`，`PathBuf::join` 不会。
fn join_path(parts: &[&str]) -> PathBuf {
    PathBuf::from(path_join(parts))
}

/// `getThemesDir()`：`<packageDir>/<src|dist>/modes/interactive/theme`。
///
/// `<src|dist>` 由 `src_or_dist` 决定（包目录下存在 `src/` 就用 `src`）。只用 `path.join`
/// 拼接，不转绝对路径。`IS_BUN_BINARY` 分支返回 `<packageDir>/theme`，原生构建不可达。
pub fn get_themes_dir() -> PathBuf {
    let package_dir = get_package_dir();
    let src_or_dist = src_or_dist(&package_dir);
    let package_dir = package_dir.to_string_lossy();
    if IS_BUN_BINARY {
        return join_path(&[&package_dir, "theme"]);
    }
    join_path(&[&package_dir, src_or_dist, "modes", "interactive", "theme"])
}

/// `getExportTemplateDir()`：`<packageDir>/<src|dist>/core/export-html`。
///
/// 与 [`get_themes_dir`] 一样按 `src_or_dist` 选择目录，只用 `path.join` 拼接；
/// `IS_BUN_BINARY` 分支返回 `<packageDir>/export-html`，原生构建不可达。
pub fn get_export_template_dir() -> PathBuf {
    let package_dir = get_package_dir();
    let src_or_dist = src_or_dist(&package_dir);
    let package_dir = package_dir.to_string_lossy();
    if IS_BUN_BINARY {
        return join_path(&[&package_dir, "export-html"]);
    }
    join_path(&[&package_dir, src_or_dist, "core", "export-html"])
}

/// `getPackageJsonPath()`：`<packageDir>/package.json`，只用 `path.join`（不转绝对路径）。
pub fn get_package_json_path() -> PathBuf {
    join_path(&[&get_package_dir().to_string_lossy(), "package.json"])
}

/// `getReadmePath()`：`<packageDir>/README.md`，经 `path_resolve` 转绝对路径。
///
/// 相对路径的 `KIANA_PACKAGE_DIR` 会按**进程 cwd** 解析（测试
/// `relative_package_dir_yields_absolute_resolved_asset_paths`），与下面几个文档路径一致。
pub fn get_readme_path() -> PathBuf {
    PathBuf::from(path_resolve(&[&path_join(&[
        &get_package_dir().to_string_lossy(),
        "README.md",
    ])]))
}

/// `getDocsPath()`：`<packageDir>/docs`，经 `path_resolve` 转绝对路径。
pub fn get_docs_path() -> PathBuf {
    PathBuf::from(path_resolve(&[&path_join(&[
        &get_package_dir().to_string_lossy(),
        "docs",
    ])]))
}

/// `getExamplesPath()`：`<packageDir>/examples`，经 `path_resolve` 转绝对路径。
pub fn get_examples_path() -> PathBuf {
    PathBuf::from(path_resolve(&[&path_join(&[
        &get_package_dir().to_string_lossy(),
        "examples",
    ])]))
}

/// `getChangelogPath()`：`<packageDir>/CHANGELOG.md`，经 `path_resolve` 转绝对路径。
pub fn get_changelog_path() -> PathBuf {
    PathBuf::from(path_resolve(&[&path_join(&[
        &get_package_dir().to_string_lossy(),
        "CHANGELOG.md",
    ])]))
}

/// `getInteractiveAssetsDir()`：`<packageDir>/<src|dist>/modes/interactive/assets`。
///
/// 与 [`get_themes_dir`] 相同地按 `src_or_dist` 选择目录，只用 `path.join`；
/// `IS_BUN_BINARY` 分支返回 `<packageDir>/assets`，原生构建不可达。
pub fn get_interactive_assets_dir() -> PathBuf {
    let package_dir = get_package_dir();
    let src_or_dist = src_or_dist(&package_dir);
    let package_dir = package_dir.to_string_lossy();
    if IS_BUN_BINARY {
        return join_path(&[&package_dir, "assets"]);
    }
    join_path(&[&package_dir, src_or_dist, "modes", "interactive", "assets"])
}

/// `getBundledInteractiveAssetPath(name)`：在 [`get_interactive_assets_dir`] 下拼接 `name`。
///
/// `name` 原样传给 `path.join`（其中的分隔符会被当作路径段），不做校验；每次调用都会重新
/// 计算包目录。
pub fn get_bundled_interactive_asset_path(name: &str) -> PathBuf {
    join_path(&[&get_interactive_assets_dir().to_string_lossy(), name])
}

/// 由 Bun 入口注入的“编译进可执行文件”的 wasm 路径（生产构建通常不设置）。
static EMBEDDED_QUICKJS_WASM_PATH: Mutex<Option<PathBuf>> = Mutex::new(None);
/// [`get_quickjs_wasm_path`] 的进程级缓存；`OnceLock` 保证只解析一次，`None` 也会被缓存。
static QUICKJS_WASM_PATH_CACHE: OnceLock<Option<PathBuf>> = OnceLock::new();

/// 由 Bun 入口把编译进可执行文件的 QuickJS wasm 路径交给本模块。
///
/// 只在**首次调用** [`get_quickjs_wasm_path`] 之前设置才有效；一旦缓存形成，后续设置不会
/// 改变返回值。当前仓库中只有 `tests/config.rs` 调用它，生产路径没有设置嵌入式路径的
/// 调用点。
pub fn set_embedded_quickjs_wasm_path(path: impl Into<PathBuf>) {
    let mut guard = EMBEDDED_QUICKJS_WASM_PATH
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    *guard = Some(path.into());
}

/// `getQuickJSWasmPath()`：**只解析一次并缓存**（升级删除旧文件后仍可用旧路径，见 #10439）。
///
/// 优先级：`setEmbeddedQuickJSWasmPath` 设置的值 → 非空 `KIANA_QUICKJS_WASM` →
/// `<getPackageDir()>/node_modules/quickjs-wasi/quickjs.wasm`（仅当存在）→ `None`。
///
/// 首次调用之后，结果（包括 `None`）被 `OnceLock` 永久缓存；再调用
/// [`set_embedded_quickjs_wasm_path`] 或修改环境变量都不会生效。
///
/// TS 的 `createRequire(...).resolve()` 解析失败会 `throw`；Rust 返回 `None`。kiana-core
/// 不打包 `quickjs.wasm`（资源归 `kiana-codemode` 的 `wasm` feature，§8 第 22 条）。
/// 注：当前仓库内没有对 [`set_embedded_quickjs_wasm_path`] 的生产调用，只有测试使用它。
pub fn get_quickjs_wasm_path() -> Option<PathBuf> {
    QUICKJS_WASM_PATH_CACHE
        .get_or_init(|| {
            // 1) 嵌入式路径优先；锁只包住读取，随后立即释放。
            {
                let guard = EMBEDDED_QUICKJS_WASM_PATH
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                if let Some(path) = guard.as_ref() {
                    return Some(path.clone());
                }
            }
            // 2) 环境变量（空串按未设置处理），不检查文件是否存在。
            if let Some(env_path) = std::env::var_os("KIANA_QUICKJS_WASM")
                .map(|value| value.to_string_lossy().into_owned())
                .filter(|value| !value.is_empty())
            {
                return Some(PathBuf::from(env_path));
            }
            // 3) 包内 node_modules 的默认位置，只有文件存在才返回。
            let candidate = get_package_dir()
                .join("node_modules")
                .join("quickjs-wasi")
                .join("quickjs.wasm");
            candidate.exists().then_some(candidate)
        })
        .clone()
}

/// codemode worker 的运行期类型。
///
/// 由编译期常量推导（见 [`get_codemode_worker_specifier`]）：原生构建三者全为 `false`，
/// 因此恒为 `Unbundled`。保留三种形态是为了与 TS 的 Bun 二进制 / bundled Node 构建对齐。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodemodeRuntime {
    /// Bun 编译的二进制（`IS_BUN_BINARY`）。
    BunBinary,
    /// 打包后的 Node 构建（`IS_BUNDLED_NODE`）：worker 文件与主模块同目录。
    BundledNode,
    /// 未打包的源码/原生构建：使用内置 worker，不需要外部说明符。
    Unbundled,
}

/// `resolveCodemodeWorkerSpecifier(runtime, moduleUrl)`（纯函数，三态）。
///
/// - `BunBinary`：固定返回相对源码入口 `"./src/extensions/codemode/worker.ts"`，不使用
///   `module_url`。
/// - `BundledNode`：以 `module_url` 为基准解析 `"./codemode-worker.js"`；`module_url` 不是
///   可解析的绝对 URL 时返回 `None`。
/// - `Unbundled`：`None`（调用方使用内置 worker）。
///
/// 输入输出都是字符串，不访问文件系统；纯函数便于三态测试
/// （`tests/codemode_worker_config.rs`）。
pub fn resolve_codemode_worker_specifier(
    runtime: CodemodeRuntime,
    module_url: &str,
) -> Option<String> {
    match runtime {
        // Bun 二进制里用相对源码入口（Windows 上 Bun 1.3 无法把 B:\~BUN URL 映射回入口）。
        CodemodeRuntime::BunBinary => Some("./src/extensions/codemode/worker.ts".to_string()),
        CodemodeRuntime::BundledNode => {
            let base = url::Url::parse(module_url).ok()?;
            let joined = base.join("./codemode-worker.js").ok()?;
            Some(joined.to_string())
        }
        CodemodeRuntime::Unbundled => None,
    }
}

/// `getCodemodeWorkerSpecifier()`：原生 unbundled 构建恒 `None`。
///
/// `BundledNode` 分支会把说明符指向的 worker 文件读成 `data:text/javascript;base64,...` 并
/// 进程级缓存；说明符为 `None`（`module_url` 无法解析）、URL 解析失败、file URL 转换失败
/// 或文件读取失败时返回 `None`（base64 编码返回 `String`，本身不会失败）。原生构建
/// （`Unbundled`）不进入该分支，也不读任何文件。
pub fn get_codemode_worker_specifier() -> Option<String> {
    let runtime = if IS_BUN_BINARY {
        CodemodeRuntime::BunBinary
    } else if IS_BUNDLED_NODE {
        CodemodeRuntime::BundledNode
    } else {
        CodemodeRuntime::Unbundled
    };
    let module_url = current_module_url();
    let specifier = resolve_codemode_worker_specifier(runtime, &module_url);
    if runtime != CodemodeRuntime::BundledNode {
        return specifier;
    }
    // bundled-node（release 构建）把 worker 读成 data: URL 并缓存；原生构建不可达，
    // 保留分支以便将来换 runtime（§2.6.3）。
    static CODEMODE_WORKER_DATA_URL: Mutex<Option<String>> = Mutex::new(None);
    let mut guard = CODEMODE_WORKER_DATA_URL
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if guard.is_none() {
        let path = url::Url::parse(specifier.as_deref()?)
            .ok()?
            .to_file_path()
            .ok()?;
        let bytes = std::fs::read(path).ok()?;
        use base64::Engine as _;
        let encoded = base64::engine::general_purpose::STANDARD.encode(bytes);
        *guard = Some(format!("data:text/javascript;base64,{encoded}"));
    }
    guard.clone()
}

/// 把 `current_exe()` 转成 `file://` URL；取不到或转换失败时退回 `"file:///"`。
fn current_module_url() -> String {
    std::env::current_exe()
        .ok()
        .and_then(|path| url::Url::from_file_path(path).ok())
        .map(|url| url.to_string())
        .unwrap_or_else(|| "file:///".to_string())
}

/// 安装变更：`{kind:"updated",version}` 或 `{kind:"removed"}`。
///
/// 由 [`detect_install_change`] 在运行期比较磁盘上的 `package.json` 得出，供
/// `kiana-interactive` 提示“运行期间安装被更新/移除”。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InstallChange {
    /// 版本发生变化；`version` 是 JS `String(value)` 的结果（非字符串值会被转换）。
    Updated { version: String },
    /// 读取 `package.json` 得到 `NotFound`，通常意味着安装被卸载或替换。
    Removed,
}

/// TS 模块加载时捕获的 package.json 路径：只有**读取成功**才缓存（文件存在但不可读时为
/// `None`）。`LazyLock` 以首次调用为 Rust 的“启动”时点，见模块文档“已知偏差 3”。
static STARTUP_PACKAGE_JSON_PATH: LazyLock<Option<PathBuf>> = LazyLock::new(|| {
    let path = get_package_json_path();
    // TS 在模块加载时 `readFileSync`；读成功（而非仅存在）才记录。
    std::fs::read(&path).ok().map(|_| path)
});

/// TS 模块加载时捕获的 package.json 路径（首次调用本函数即为 Rust 的“启动”时点）。
///
/// **test hook, not public API**（doc-hidden）。
#[doc(hidden)]
pub fn startup_package_json_path() -> Option<&'static Path> {
    STARTUP_PACKAGE_JSON_PATH.as_deref()
}

/// `detectInstallChange(packageJsonPath = startupPackageJsonPath)`。
///
/// 返回 `None` 表示“没有可报告的变更”，涵盖：`IS_BUN_BINARY`（原生不可达）、默认路径在
/// 启动时没有读到、文件读取失败且错误不是 `NotFound`、JSON 解析失败、缺少 `version`
/// 字段、`version` 是 JS 假值（`false`/`0`/`""`/`null`）、`version` 是非有限数字面量
/// （如 `1e400`，`js_parse` 把它落成 `Value::Null`，随后按假值处理），或 `version` 是
/// **字符串**且恰好等于 [`VERSION`]。只有读取时的 `NotFound` 才映射为
/// [`InstallChange::Removed`]。
///
/// 除模块文档“已知偏差 5”列出的非有限数字面量（如 `1e400`）外，非字符串的 truthy
/// `version` 一律算变更，即使 `String(value)` 与 `VERSION` 相同
/// （`["1.4.0"]` → `Updated { version: "1.4.0" }`）：这是 JS `!==` 严格不等语义。
/// 显式传入 `packageJsonPath` 时不走启动缓存。解析走 `kiana_json::js_parse`（先剥 BOM），
/// 以复刻 `JSON.parse` 的宽松语义，细节见模块文档“已知偏差 5”。
pub fn detect_install_change(package_json_path: Option<&Path>) -> Option<InstallChange> {
    // 原生构建恒 false，保留对齐 TS。
    if IS_BUN_BINARY {
        return None;
    }
    // 未显式给路径时用启动时捕获的路径；启动时没读到（不存在或不可读）直接放弃。
    let path = match package_json_path {
        Some(path) => path.to_path_buf(),
        None => startup_package_json_path()?.to_path_buf(),
    };
    // 状态分流：NotFound → Removed；其它 IO 错误没有可报告的状态，返回 None。
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) => {
            return if error.kind() == std::io::ErrorKind::NotFound {
                Some(InstallChange::Removed)
            } else {
                None
            };
        }
    };
    let text = String::from_utf8_lossy(&bytes);
    // TS 用 `JSON.parse`：接受数值溢出（`1e400` → ±Infinity，存成 `Null`）、孤立代理项转义
    // （存成 U+FFFD）与任意嵌套深度；`serde_json` 会拒绝前两种。见 `kiana_json::js_parse`。
    let parsed: serde_json::Value = kiana_json::js_parse::parse(&strip_bom(&text)).ok()?;
    let version = parsed.get("version")?;
    // `installed.version && ...`：JS 真值判定（`null`/`false`/`0`/`""` 为假，数组/对象恒真）。
    if !kiana_json::js_string::is_truthy(Some(version)) {
        return None;
    }
    // `installed.version !== VERSION` 是 **JS 严格不等**：只有字符串恰好等于 VERSION 时
    // 才算“没有变化”。`["1.4.0"]`（数组）、`1.4`（数字）等即使 `String(value)` 与 VERSION
    // 相同也报告 updated；返回值是 `${change.version}` 插值（`String(value)`）。
    if let serde_json::Value::String(text) = version
        && text.as_str() == VERSION
    {
        return None;
    }
    Some(InstallChange::Updated {
        version: kiana_json::js_string::js_string(Some(version)),
    })
}

// =============================================================================
// App Config (from package.json kianaConfig)
// =============================================================================

/// `expandTildePath(path)`。
///
/// 实际调用 `paths::normalize_path` 的默认选项：`~` / `~/...` 展开为 home，`file://` URL
/// 转成本地路径，Windows 上规范化 shell 路径。**不**转绝对路径，也不检查路径是否存在；
/// 其它路径原样返回（Windows 上仍会做 shell 路径规范化）。
pub fn expand_tilde_path(path: &str) -> String {
    normalize_path(path, &PathInputOptions::default())
}

/// `KIANA_API_BASE_URL`（去掉尾部 `/`）——Kiana 自建服务的统一根地址。
///
/// 每次调用都读环境变量；值先经 `trim` 去首尾空白，空串按未配置处理，最后去掉所有尾部
/// `/`。未配置或为空时返回 `None`；调用方必须把相关网络功能视为不可用，而不是回退到
/// 某个上游默认域名。
///
/// 注意顺序：非空检查发生在去斜杠之前，因此 `"/"` 这类只含斜杠的输入会得到
/// `Some(String::new())`，而不是 `None`。
pub fn get_api_base_url() -> Option<String> {
    std::env::var(ENV_API_BASE_URL)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .map(|value| value.trim_end_matches('/').to_string())
}

/// 把 `path` 拼到 [`get_api_base_url`] 上；未配置时返回 `None`。
///
/// `path` 的前导 `/` 会被去掉再以单个 `/` 连接；`path` 为空（或全是 `/`）时直接返回 base。
/// 不做 URL 转义或校验。
pub fn get_api_url(path: &str) -> Option<String> {
    let base = get_api_base_url()?;
    let path = path.trim_start_matches('/');
    if path.is_empty() {
        Some(base)
    } else {
        Some(format!("{base}/{path}"))
    }
}

/// `getShareViewerUrl(gistId)`。
///
/// `KIANA_SHARE_VIEWER_URL` 优先；否则使用 `KIANA_API_BASE_URL/session/`。
/// 两者都未配置时返回 `None`，由调用方明确提示分享查看器不可用。
///
/// `KIANA_SHARE_VIEWER_URL` 只做**非空**判断、不 trim，因此该变量前后空白会原样保留
/// （回退到 API base 时会走 [`get_api_base_url`] 的 trim）。返回值为 `"{base}#{gist_id}"`，
/// base 末尾是否已有 `/` 不影响拼接。
pub fn get_share_viewer_url(gist_id: &str) -> Option<String> {
    let base = std::env::var("KIANA_SHARE_VIEWER_URL")
        .ok()
        .filter(|value| !value.is_empty())
        .or_else(|| get_api_url("session/"))?;
    Some(format!("{base}#{gist_id}"))
}

// =============================================================================
// User Config Paths (~/.kiana/agent/*)
// =============================================================================

/// 把若干段拼到 home 目录下（`path.join` 语义）。
///
/// `dirs::home_dir()` 取不到时 home 为空串参与拼接，结果会退化为相对路径。
fn home_join(parts: &[&str]) -> PathBuf {
    let home = dirs::home_dir().unwrap_or_default();
    let home = home.to_string_lossy().into_owned();
    let mut all: Vec<&str> = vec![&home];
    all.extend_from_slice(parts);
    join_path(&all)
}

/// `getAgentDir()`：`KIANA_CODING_AGENT_DIR` 优先（每次调用都读 env）。
///
/// 环境变量非空时返回经 [`expand_tilde_path`] 处理的值（`~` 展开、`file://` 转换），
/// 但不转绝对路径；空串视为未设置。未设置时返回 `<home>/.kiana/agent`（home 取不到时
/// 退化为相对路径，见 `home_join`）。
pub fn get_agent_dir() -> PathBuf {
    if let Some(env_dir) = std::env::var_os(ENV_AGENT_DIR)
        .map(|value| value.to_string_lossy().into_owned())
        .filter(|value| !value.is_empty())
    {
        return PathBuf::from(expand_tilde_path(&env_dir));
    }
    home_join(&[CONFIG_DIR_NAME, "agent"])
}

/// `getCustomThemesDir()`：`<agentDir>/themes`。
///
/// 只用 `path.join` 拼接；每次调用重新解析 agent 目录。
pub fn get_custom_themes_dir() -> PathBuf {
    join_path(&[&get_agent_dir().to_string_lossy(), "themes"])
}

/// `getModelsPath()`：`<agentDir>/models.json`。
pub fn get_models_path() -> PathBuf {
    join_path(&[&get_agent_dir().to_string_lossy(), "models.json"])
}

/// `getAuthPath()`：`<agentDir>/auth.json`。
pub fn get_auth_path() -> PathBuf {
    join_path(&[&get_agent_dir().to_string_lossy(), "auth.json"])
}

/// `getSettingsPath()`：`<agentDir>/settings.json`。
pub fn get_settings_path() -> PathBuf {
    join_path(&[&get_agent_dir().to_string_lossy(), "settings.json"])
}

/// `getToolsDir()`：`<agentDir>/tools`。
pub fn get_tools_dir() -> PathBuf {
    join_path(&[&get_agent_dir().to_string_lossy(), "tools"])
}

/// `getBinDir()`：`<agentDir>/bin`。
pub fn get_bin_dir() -> PathBuf {
    join_path(&[&get_agent_dir().to_string_lossy(), "bin"])
}

/// `getPromptsDir()`：`<agentDir>/prompts`。
pub fn get_prompts_dir() -> PathBuf {
    join_path(&[&get_agent_dir().to_string_lossy(), "prompts"])
}

/// `getSessionsDir()`：`<agentDir>/sessions`。
pub fn get_sessions_dir() -> PathBuf {
    join_path(&[&get_agent_dir().to_string_lossy(), "sessions"])
}

/// `getDebugLogPath()`：`<agentDir>/kiana-debug.log`。
///
/// 文件名由 [`APP_NAME`] 派生（`"{APP_NAME}-debug.log"`），改 `APP_NAME` 会同时改变日志
/// 文件名。
pub fn get_debug_log_path() -> PathBuf {
    join_path(&[
        &get_agent_dir().to_string_lossy(),
        &format!("{APP_NAME}-debug.log"),
    ])
}

// 内联单元测试覆盖展示文本、命令拼接、参数拆分、入口解析，以及公开常量的派生
// （`env_constants_derive_from_app_name`）与 `resolve_codemode_worker_specifier` 的三态；
// 需要真实文件系统/环境变量的行为在 `tests/config.rs`。入口相关用例操作进程级 static，用
// `ENTRYPOINT_TEST_LOCK` 串行化。
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[allow(clippy::assertions_on_constants)]
    fn env_constants_derive_from_app_name() {
        assert_eq!(
            ENV_AGENT_DIR,
            format!("{}_CODING_AGENT_DIR", APP_NAME.to_uppercase())
        );
        assert_eq!(
            ENV_SESSION_DIR,
            format!("{}_CODING_AGENT_SESSION_DIR", APP_NAME.to_uppercase())
        );
        assert!(!IS_BUN_BINARY);
        assert!(!IS_BUN_RUNTIME);
        assert!(!IS_BUNDLED_NODE);
    }

    #[test]
    fn display_quotes_only_arguments_with_whitespace() {
        let step = make_self_update_command_step("npm", &["a".to_string(), "b c".to_string()]);
        assert_eq!(step.display, "npm a \"b c\"");
        let step = make_self_update_command_step("pnpm", &[]);
        assert_eq!(step.display, "pnpm");
    }

    #[test]
    fn self_update_command_combines_steps() {
        let install = make_self_update_command_step("npm", &["install".to_string()]);
        let uninstall = make_self_update_command_step("npm", &["uninstall".to_string()]);
        let command = make_self_update_command(install.clone(), Some(uninstall.clone()));
        assert_eq!(command.display, "npm uninstall && npm install");
        assert_eq!(command.steps, Some(vec![uninstall, install]));
        let single = make_self_update_command(
            make_self_update_command_step("npm", &["install".to_string()]),
            None,
        );
        assert_eq!(single.steps, None);
        assert_eq!(single.display, "npm install");
    }

    #[test]
    fn npm_command_destructuring() {
        let empty: Vec<String> = Vec::new();
        assert_eq!(split_npm_command(None), ("npm".to_string(), Vec::new()));
        assert_eq!(
            split_npm_command(Some(&empty)),
            ("npm".to_string(), Vec::new())
        );
        let values = vec!["npm".to_string(), "--prefix".to_string(), "/x".to_string()];
        assert_eq!(
            split_npm_command(Some(&values)),
            (
                "npm".to_string(),
                vec!["--prefix".to_string(), "/x".to_string()]
            )
        );
    }

    /// 串行化入口测试缝（override 是进程级 static）。
    static ENTRYPOINT_TEST_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn entrypoint_defaults_to_the_executable_path() {
        let _guard = ENTRYPOINT_TEST_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        // 无 override 时 `process.argv[1]` 的等价物是 current_exe（入口脚本），不是
        // 第一个用户参数（`args_os().nth(1)` 在 `kiana update` 里会是 "update"）。
        clear_entrypoint_override_for_test();
        let expected = std::env::current_exe()
            .ok()
            .map(|path| PathBuf::from(path_resolve(&[&path.to_string_lossy()])));
        assert_eq!(entrypoint_argument(), expected);
    }

    #[test]
    fn entrypoint_override_is_resolved_like_process_argv() {
        let _guard = ENTRYPOINT_TEST_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        // Node 在启动时把 argv[1] 交给 `path.resolve`；相对入口也要变成绝对路径。
        set_entrypoint_override_for_test(Some(PathBuf::from("cli.js")));
        let expected = PathBuf::from(path_resolve(&["cli.js"]));
        assert_eq!(entrypoint_argument(), Some(expected));
        clear_entrypoint_override_for_test();
    }

    #[test]
    fn codemode_worker_specifier_three_states() {
        assert_eq!(
            resolve_codemode_worker_specifier(
                CodemodeRuntime::BunBinary,
                "file:///B:/~BUN/root/config.js"
            )
            .as_deref(),
            Some("./src/extensions/codemode/worker.ts")
        );
        assert_eq!(
            resolve_codemode_worker_specifier(
                CodemodeRuntime::BundledNode,
                "file:///app/chunks/config.js"
            )
            .as_deref(),
            Some("file:///app/chunks/codemode-worker.js")
        );
        assert_eq!(
            resolve_codemode_worker_specifier(CodemodeRuntime::Unbundled, "file:///app/x.js"),
            None
        );
    }
}
