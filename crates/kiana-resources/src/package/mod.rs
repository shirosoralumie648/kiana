//! `core/package-manager.ts`（2791 行）：`DefaultPackageManager` / `PackageManager` trait / 公开类型。
//!
//! TS: `packages/coding-agent/src/core/package-manager.ts`。
//!
//! 模块划分与 `ARCHITECTURE.md` §5 一致：
//! - `types`：公开类型与 `PathMetadata` 再导出；
//! - `parse`：`parseSource` / npm spec / identity / dedupe；
//! - `discover`：目录发现与 ignore 规则；
//! - `patterns`：模式过滤（`MiniMatch`）与 glob 展开；
//! - `resolve`：资源累积器与 `resolve` 主流程的辅助函数；
//! - `paths`：安装根、临时目录、`resolveManagedPath` 与 `node:path` 词法工具；
//! - `npm` / `git`：安装、卸载、更新与命令构造；
//! - `settings_io`：settings 里的源增删与规范化；
//! - `exec` / `concurrency`：子进程与并发。
//!
//! 公开 API 见 [`PackageManager`]；`DefaultPackageManager` 另外公开
//! [`DefaultPackageManager::check_for_available_updates`]（放在具体类型上，接口保持最小）。
//!
//! # 这个文件负责什么
//! 它是包管理子系统的公开门面与编排层：定义 [`PackageManager`] 接口、[`DefaultPackageManager`]
//! 实现、公开错误 [`PackageManagerError`] 与构造选项 [`PackageManagerOptions`]，并按固定
//! 顺序串起 `resolve` / `install` / `remove` / `update` 等流程。源码字符串解析、目录发现、
//! 模式过滤、路径布局、npm/git 子进程分别在同目录的 `parse`、`discover`、`patterns`、
//! `paths`、`npm`、`git` 等子模块中；本文件只负责编排、作用域判定、错误汇总与进度事件。
//!
//! # 被谁调用
//! - `crate::loader` 的 `DefaultResourceLoader` 在 reload 时调用 `resolve(None)` 取四类
//!   资源路径，并把结果里的 `install_failures` 转成启动警告；CLI 的 `-e` 额外扩展源走
//!   `resolve_extension_sources(..., temporary: true)`。
//! - `kiana-coding-agent` 的 package 子命令调用 `install_and_persist` /
//!   `remove_and_persist` / `list_configured_packages` / `update`；`kiana-interactive` 的
//!   交互式模式（`interactive_mode.rs`）调用 `check_for_available_updates`。这些调用方
//!   只使用 trait 或 `DefaultPackageManager` 的公开方法。
//!
//! # `resolve` 的执行顺序（顺序决定优先级）
//! `resolve_inner` 固定按以下阶段向同一个累积器写入：
//! 1. 读取 settings 快照；按 project 在前、user 在后收集 packages，再按 identity 去重；
//! 2. 解析包源（缺失时尝试安装，成功或已存在则收集包内资源）；单个包的安装失败被记录
//!    为 `PackageInstallFailure`，不会中止整个解析（`spec/ca-resources.md` §0.4 第 7 条）；
//! 3. 处理 settings 顶层的 extensions/skills/prompts/themes 字符串列表，每类仍是
//!    project 先、user 后；
//! 4. 自动发现 agent 目录、项目 `.kiana` 与 `.agents/skills` 下的资源；
//! 5. 最后追加上内置扩展（`builtin:<name>`），其启用状态由 settings 的覆盖模式决定。
//! 累积器对同一路径“首次写入胜出”，因此这个阶段顺序就是资源优先级；最终列表顺序另由
//! `resourcePrecedenceRank` 的稳定升序排序与 `canonicalizePath` 去重决定（见
//! `resolve` 子模块与 `spec/ca-resources.md` §0.4 第 1、2 条）。
//!
//! # 作用域、信任与错误
//! 公开类型 `InstallScope` 只有 `user` / `project`；`resolve_extension_sources` 通过
//! `ResolveExtensionSourcesOptions::temporary` 在内部使用 `temporary` 作用域。任何
//! `project` 作用域的包存储访问都要先通过 trust 检查，未受信时返回
//! `Project is not trusted; refusing to access project package storage`。
//! [`PackageManager::get_installed_path`] 与 [`PackageManager::list_configured_packages`]
//! 受冻结签名限制，会把这类错误折叠成 `None` / 跳过条目；需要区分错误时使用
//! [`DefaultPackageManager::try_get_installed_path`]。
//!
//! # 离线与进度
//! `KIANA_OFFLINE=1|true|yes`（大小写不敏感）时，解析不会安装缺失包，也不会刷新临时
//! git 源；`check_for_available_updates` 直接返回空列表，`update` 的末端更新阶段
//! （`update_configured_sources`）不做任何网络或安装动作。进度事件只由 `with_progress`
//! 在安装、卸载、npm 批量更新、git 更新与临时 git 刷新时发出；`resolve` 安装缺失包时
//! 不发进度。
//!
//! # 推荐阅读顺序
//! 先看 [`types`] 的公开结果类型与 [`parse`] 的源解析，再看 [`paths`] 的作用域目录布局和
//! [`resolve`] 的累积器/收集逻辑，最后回到本文件的 `resolve_inner` 与
//! `update_configured_sources` 理解编排；`npm` / `git` 只在需要核对子进程参数时阅读。

pub mod concurrency;
pub mod discover;
pub mod exec;
pub mod git;
pub mod npm;
pub mod parse;
pub mod paths;
pub mod patterns;
pub mod resolve;
pub mod settings_io;
pub mod types;

use std::fmt;
use std::future::Future;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use kiana_config::settings::SettingsManager;
use kiana_config::settings::schema::PackageSource;
use kiana_core::source_info::{SourceOrigin, SourceScope};

pub use types::{
    ConfiguredPackage, InstallOptions, InstallScope, MissingSourceAction, MissingSourceCallback,
    MissingSourceFuture, PackageInstallFailure, PackageType, PackageUpdate, ProgressAction,
    ProgressCallback, ProgressEvent, ProgressEventType, ResolveExtensionSourcesOptions,
    ResolvedPaths, ResolvedResource, missing_source_callback,
};

pub use kiana_core::source_info::PathMetadata;

use resolve::{ResourceType, add_resource, create_accumulator, to_resolved_paths};

/// `UPDATE_CHECK_CONCURRENCY`：并发执行版本/更新检查的任务上限。
///
/// 同时用于 `check_for_available_updates` 的逐包检查与 `update_configured_sources` 里的
/// `should_update_npm_source` 批量检查。npm 检查会启动 `npm view`，git 检查会启动
/// `git rev-parse` / `git ls-remote`，因此这个值直接决定并发子进程数量；
/// `run_with_concurrency` 还会按实际任务数再截断。
pub(crate) const UPDATE_CHECK_CONCURRENCY: usize = 4;
/// `GIT_UPDATE_CONCURRENCY`：并发执行 git 更新任务的上限。
///
/// 只在 `update_configured_sources` 的 git 分支使用；每个任务是一次 `update_git`
/// （fetch/checkout，路径不存在时改为安装）。任务按候选顺序启动，但完成顺序不保证。
pub(crate) const GIT_UPDATE_CONCURRENCY: usize = 4;

/// 读取进程环境变量 `KIANA_OFFLINE`（TS: `isOfflineModeEnabled()`）。
///
/// 规则：未设置、空串或非 Unicode 值 → `false`；`"1"` 精确匹配，或 `"true"` / `"yes"`
/// 大小写不敏感匹配 → `true`；其它值（包括 `"0"`、`"false"`、`"2"`）→ `false`。不做
/// trim，因此 `"TRUE "` 也是 `false`。每次调用都重新读环境变量，没有缓存；环境变量是
/// 进程级状态，测试里修改它会影响并发运行的其它测试（本文件测试已用 `unsafe` 说明）。
pub fn is_offline_mode_enabled() -> bool {
    let value = std::env::var("KIANA_OFFLINE").unwrap_or_default();
    if value.is_empty() {
        return false;
    }
    value == "1" || value.eq_ignore_ascii_case("true") || value.eq_ignore_ascii_case("yes")
}

/// 包管理的公开错误：`Display` 逐字等于 TS 的 `error.message`。
///
/// `ARCHITECTURE.md` §4.1：跨 crate 的公开错误用手写类型 + 手写 `Display`，不使用
/// `thiserror` 的字段展开。错误只承载一段展示文本，没有错误码或 `source` 链；相等性
/// 也按文本比较，调用方只能依靠上下文（哪个方法返回的）区分错误类别。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackageManagerError {
    message: String,
}

impl PackageManagerError {
    /// 由消息文本创建。
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }

    /// `error.message`。
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for PackageManagerError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for PackageManagerError {}

/// 内部错误构造辅助。
pub(crate) fn err(message: impl Into<String>) -> PackageManagerError {
    PackageManagerError::new(message)
}

/// `DefaultPackageManager` 的构造选项。TS: `PackageManagerOptions`。
///
/// 构造后会由 [`DefaultPackageManager::new`] 把 `cwd` / `agent_dir` 解析成绝对路径：
/// 相对路径以**进程当前目录**为基准，开头的 `~` 会展开；默认选项不 trim 首尾空白，
/// 也不会创建目录。
#[derive(Clone)]
pub struct PackageManagerOptions {
    /// 项目工作目录（构造时 `resolvePath`）。
    pub cwd: String,
    /// agent 配置目录（构造时 `resolvePath`）。
    pub agent_dir: String,
    /// settings 管理器。
    pub settings_manager: SettingsManager,
    /// 内置扩展名（`-e builtin:<name>` / `resolve()` 尾部）。
    pub builtin_extensions: Vec<String>,
}

impl fmt::Debug for PackageManagerOptions {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        // `SettingsManager` 的 `Debug` 只有 `finish_non_exhaustive()`、不输出任何字段
        // （内部是共享句柄与写队列发送端，没有可打印的快照字段），所以这里手写实现，
        // 只输出可安全打印的配置字段；`finish_non_exhaustive` 标出还有被省略的字段。
        formatter
            .debug_struct("PackageManagerOptions")
            .field("cwd", &self.cwd)
            .field("agent_dir", &self.agent_dir)
            .field("builtin_extensions", &self.builtin_extensions)
            .finish_non_exhaustive()
    }
}

/// 包管理器接口：12 个方法（与 TS 的 `PackageManager` 一致）。
///
/// # 方法分组与共同约定
/// - 只读：`resolve`、`list_configured_packages`、`get_installed_path`。
/// - 有副作用：`install`、`install_and_persist`、`remove`、`remove_and_persist`、
///   `update`；`*_and_persist` 变体在文件系统操作之后额外写回 settings。
/// - 只改 settings：`add_source_to_settings`、`remove_source_from_settings`。
/// - 回调：`set_progress_callback` 设置当前 manager 实例（含其克隆体）的进度监听器，
///   传 `None` 清除。
///
/// 所有可能失败的异步方法统一返回 [`PackageManagerError`]，其文本就是 TS 的
/// `error.message`，没有错误码。`DefaultPackageManager` 为保持冻结的 `bool` 签名，会把
/// settings 写回错误折叠为 `false`；需要错误时用同名的 `try_*` 方法（在具体类型上）。
///
/// `async_trait` 会为生成的 `Pin<Box<dyn Future>>` 返回类型附加无消息的 `#[must_use]`，
/// clippy 的 `double_must_use` 会误报；这里局部关闭。
#[allow(clippy::double_must_use)]
#[async_trait]
pub trait PackageManager: Send + Sync {
    /// `resolve(onMissing?)`：解析全部已配置资源，返回四类资源路径和安装失败列表。
    ///
    /// `on_missing` 只在某个包缺失（未安装）时被调用：`Install` 继续安装、`Skip` 跳过
    /// 该源、`Error` 立即以 `Missing source: <source>` 失败；离线模式下不会调用它，也
    /// 不会安装。单个包的安装失败会进入 `ResolvedPaths::install_failures`，本方法仍返回
    /// `Ok`；settings 形状非法、project 未受信等配置/路径错误才会返回 `Err`。
    async fn resolve(
        &self,
        on_missing: Option<MissingSourceCallback>,
    ) -> Result<ResolvedPaths, PackageManagerError>;

    /// `install(source, options?)`：安装一个包源，不写回 settings。
    ///
    /// `options.local` 选择作用域：`true` 为 project，`false` 为 user；project 先做
    /// trust 检查。npm 源安装到该作用域的 npm 根，git 源克隆到 git 根；local 源只检查
    /// 解析后的路径是否存在（不存在时返回 `Path does not exist: <path>`），不复制任何
    /// 文件。整个过程包在一条 `Start` / `Complete` / `Error` 进度事件里。
    async fn install(
        &self,
        source: &str,
        options: InstallOptions,
    ) -> Result<(), PackageManagerError>;

    /// `installAndPersist(source, options?)`：先 [`PackageManager::install`]，成功后把源
    /// 写回 settings。
    ///
    /// 两步不是原子操作：安装成功而 settings 写入失败时返回 `Err`，此时磁盘上已有安装
    /// 但配置未更新；安装失败则不会尝试写 settings。
    async fn install_and_persist(
        &self,
        source: &str,
        options: InstallOptions,
    ) -> Result<(), PackageManagerError>;

    /// `remove(source, options?)`：删除包源在磁盘上的安装，不写回 settings。
    ///
    /// local 源是空操作（返回 `Ok(())`）；npm/git 分别删除对应安装目录，project 作用域
    /// 同样先做 trust 检查。整个过程包在一条 `Start` / `Complete` / `Error` 进度事件里。
    async fn remove(
        &self,
        source: &str,
        options: InstallOptions,
    ) -> Result<(), PackageManagerError>;

    /// `removeAndPersist(source, options?)`：先 [`PackageManager::remove`]，再从 settings
    /// 删除匹配条目。
    ///
    /// 返回 `true` 表示 settings 中确实有条目被删除，`false` 表示没有匹配（磁盘删除
    /// 本身可能是空操作）；settings 写回失败返回 `Err`。CLI 把 `false` 当作
    /// `No matching package found` 处理。
    async fn remove_and_persist(
        &self,
        source: &str,
        options: InstallOptions,
    ) -> Result<bool, PackageManagerError>;

    /// `update(source?)`：更新已配置的包；`Some(source)` 只处理 identity 匹配的条目。
    ///
    /// identity 规则：npm 是包名、git 是 host/path、local 是解析后的绝对路径。未给
    /// `source` 时更新 settings 里的全部包。离线时不做网络与安装动作。固定版本的 npm
    /// 包被跳过；固定 ref 的 git 包仍参与更新，用于把 checkout 对齐到配置的 ref。
    /// 给了 `source` 却没有任何条目匹配时返回 `No matching package found for ...`。
    async fn update(&self, source: Option<&str>) -> Result<(), PackageManagerError>;

    /// `listConfiguredPackages()`：列出 settings 中配置的包，顺序为先 user、后 project。
    ///
    /// 只读 settings；`installed_path` 仅在安装路径存在时给出。`DefaultPackageManager`
    /// 的实现会吞掉 settings 读取 / 条目解析错误并跳过对应条目（TS 在这种情况下会抛错）。
    fn list_configured_packages(&self) -> Vec<ConfiguredPackage>;

    /// `resolveExtensionSources(sources, options?)`：解析一组临时扩展源（CLI 的 `-e`）。
    ///
    /// `builtin:` 开头的源直接作为已启用的内置扩展加入；其余源按 `temporary` →
    /// `local` → `user` 的优先级确定一个**共同**作用域后交给包解析流程，缺失时会尝试安装
    /// （离线时跳过）。返回值的 `install_failures` 总是 `Some`（可能为空列表）。
    async fn resolve_extension_sources(
        &self,
        sources: &[String],
        options: ResolveExtensionSourcesOptions,
    ) -> Result<ResolvedPaths, PackageManagerError>;

    /// `addSourceToSettings(source, options?)`：把源规范化后写入 settings。
    ///
    /// 返回 `true` 表示确实新增或改写了条目；条目已存在且规范化后与现有值相同、因此没有
    /// 变化时返回 `false`（不是错误）。只读写 settings，不安装包。`DefaultPackageManager`
    /// 的实现把错误也折叠成 `false`，需要区分错误时用
    /// [`DefaultPackageManager::try_add_source_to_settings`]。
    fn add_source_to_settings(&self, source: &str, options: InstallOptions) -> bool;

    /// `removeSourceFromSettings(source, options?)`：从 settings 删除匹配条目；有删除时
    /// 返回 `true`。
    ///
    /// 只读写 settings，不删除磁盘安装。`DefaultPackageManager` 的实现把错误折叠成
    /// `false`，需要错误时用 [`DefaultPackageManager::try_remove_source_from_settings`]。
    fn remove_source_from_settings(&self, source: &str, options: InstallOptions) -> bool;

    /// `setProgressCallback(callback)`：安装 / 卸载 / 更新进度回调；`None` 表示清除。
    ///
    /// 回调以 `Arc<dyn Fn>` 保存在 manager 上，clone 出的 manager 共享同一个槽位，替换
    /// 会影响所有克隆。回调由触发它的异步任务同步调用，执行期间不持有 manager 的锁；
    /// 因此不要在回调里做长时间阻塞操作，否则会拖慢对应的安装 / 更新任务。
    fn set_progress_callback(&self, callback: Option<ProgressCallback>);

    /// `getInstalledPath(source, scope)`：返回安装路径；路径不存在时 `None`。
    ///
    /// 冻结签名为 `Option<String>`：settings 读取 / 路径解析错误（例如 project 未受信）
    /// 也会被折叠成 `None`。需要区分“未安装”与“出错”时使用
    /// [`DefaultPackageManager::try_get_installed_path`]。
    fn get_installed_path(&self, source: &str, scope: InstallScope) -> Option<String>;
}

/// 已配置更新源。TS: `ConfiguredUpdateSource`。
///
/// `update()` 在 settings 中找到匹配条目后生成，作为 `update_configured_sources` 的输入；
/// `scope` 决定后续分组到 user 还是 project 的批量更新。
#[derive(Debug, Clone, PartialEq, Eq)]
struct ConfiguredUpdateSource {
    source: String,
    scope: SourceScope,
}

/// npm 更新目标。TS: `NpmUpdateTarget`。
///
/// 保存原始源字符串、作用域与已解析的 `NpmSource`，避免在分类、检查和批量安装之间重复
/// 解析。`source` 只用于进度事件里的展示。
#[derive(Debug, Clone)]
struct NpmUpdateTarget {
    source: String,
    scope: SourceScope,
    parsed: parse::NpmSource,
}

/// git 更新目标。TS: `GitUpdateTarget`。
///
/// git 候选不预先过滤：带 `ref` 的包也会进入更新，因为这类条目的语义是“把 checkout
/// 对齐到配置的 ref”，而不像固定版本的 npm 包那样完全不动。`source` 用于逐个 git
/// 更新时的进度事件（事件里的 source 与消息都取它）。
#[derive(Debug, Clone)]
struct GitUpdateTarget {
    #[allow(dead_code)]
    source: String,
    scope: SourceScope,
    parsed: kiana_core::git::GitSource,
}

/// 包管理器实现。TS: `package-manager.ts#DefaultPackageManager`。
///
/// 实例持有已解析的 `cwd` / `agent_dir`、settings 句柄与内置扩展名列表；可变状态只有
/// 两个共享槽位：`global_npm_root`（npm 全局根缓存）和 `progress_callback`。`Clone` 是
/// 浅克隆，克隆体共享这两个 `Arc<Mutex<...>>`；settings 通过 `settings_manager` 在每次
/// 调用时重新读取，不会在 manager 内缓存。
#[derive(Clone)]
pub struct DefaultPackageManager {
    cwd: String,
    agent_dir: String,
    settings_manager: SettingsManager,
    builtin_extensions: Vec<String>,
    global_npm_root: Arc<Mutex<paths::NpmRootCache>>,
    progress_callback: Arc<Mutex<Option<ProgressCallback>>>,
}

impl fmt::Debug for DefaultPackageManager {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        // `SettingsManager` 的 `Debug` 只输出 `finish_non_exhaustive()`，两个
        // `Arc<Mutex<...>>` 槽位属于运行时状态，也不放进调试输出。
        formatter
            .debug_struct("DefaultPackageManager")
            .field("cwd", &self.cwd)
            .field("agent_dir", &self.agent_dir)
            .finish_non_exhaustive()
    }
}

impl DefaultPackageManager {
    /// 构造：`cwd` / `agentDir` 先经过模块级 `resolvePath`（默认选项，不 trim）。
    ///
    /// 相对路径以 `paths::process_cwd()` 为基准，`~` 会展开；这里只做路径规范化，不
    /// 创建任何目录，也不校验 `settings_manager` 是否可读。内置扩展列表按传入顺序保存，
    /// `resolve` 会按该顺序追加 `builtin:<name>`。
    pub fn new(options: PackageManagerOptions) -> Self {
        let base = paths::process_cwd();
        let cwd = kiana_core::paths::resolve_path(
            &options.cwd,
            &base,
            &kiana_core::paths::PathInputOptions::default(),
        );
        let agent_dir = kiana_core::paths::resolve_path(
            &options.agent_dir,
            &base,
            &kiana_core::paths::PathInputOptions::default(),
        );
        Self {
            cwd,
            agent_dir,
            settings_manager: options.settings_manager,
            builtin_extensions: options.builtin_extensions,
            global_npm_root: Arc::new(Mutex::new(paths::NpmRootCache::default())),
            progress_callback: Arc::new(Mutex::new(None)),
        }
    }

    /// 项目目录（已解析的绝对路径）。
    pub fn cwd(&self) -> &str {
        &self.cwd
    }

    /// agent 配置目录。
    pub fn agent_dir(&self) -> &str {
        &self.agent_dir
    }

    /// `getInstalledPath(source, scope)` 的 `Result` 版本。
    ///
    /// 流程：解析源 → 按类型计算安装路径 → 路径存在则返回 `Some`：
    /// - npm 在 user 作用域下，managed 路径不存在时会回退到遗留的全局 npm / pnpm 安装，
    ///   只有回退路径也存在才使用它；
    /// - git 按 `host/path` 布局计算；
    /// - local 以作用域基准目录解析（user 为 `agent_dir`，project 为 `<cwd>/.kiana`）。
    ///
    /// project 作用域先做 trust 检查，未受信错误原样返回；路径不存在是 `Ok(None)`，
    /// 不是错误。
    pub fn try_get_installed_path(
        &self,
        source: &str,
        scope: InstallScope,
    ) -> Result<Option<String>, PackageManagerError> {
        let parsed = self.parse_source(source);
        let scope = scope.as_source_scope();
        match parsed {
            parse::ParsedSource::Npm(npm) => {
                let path = self.get_npm_install_path(&npm, scope)?;
                Ok(paths::path_exists(&path).then_some(path))
            }
            parse::ParsedSource::Git(git) => {
                let path = self.get_git_install_path(&git, scope)?;
                Ok(paths::path_exists(&path).then_some(path))
            }
            parse::ParsedSource::Local(local) => {
                let base_dir = self.get_base_dir_for_scope(scope)?;
                let path = self.resolve_path_from_base(&local.path, &base_dir);
                Ok(paths::path_exists(&path).then_some(path))
            }
        }
    }

    /// `emitProgress(event)`：把事件交给当前注册的回调（未注册时静默丢弃）。
    ///
    /// 先克隆 `Arc` 回调并释放 `Mutex`，再调用；因此回调执行期间不持有该锁，回调里
    /// 调用 [`PackageManager::set_progress_callback`] 也不会在同一线程上重入死锁。
    /// 回调是同步 `Fn`，在触发事件的任务线程上直接执行，抛 panic 会继续向上传播。
    fn emit_progress(&self, event: ProgressEvent) {
        let callback = npm::lock(&self.progress_callback).clone();
        if let Some(callback) = callback {
            callback(event);
        }
    }

    /// `withProgress(action, source, message, op)`：给一次异步操作包上进度事件。
    ///
    /// 固定发出两条事件：操作前一条 `Start`（带 `message`），操作后一条终态事件——
    /// 成功为 `Complete`（无 message），失败为 `Error`（message 取
    /// [`PackageManagerError::message`]）。操作本身的错误原样返回，不会被替换。
    /// 本模块内它包住 install / remove / npm 批量更新 / 逐个 git 更新；`git` 子模块的
    /// 临时源刷新也复用它（`Pull`），因此同一 manager 上的事件顺序取决于并发任务。
    pub(crate) async fn with_progress<F, Fut>(
        &self,
        action: ProgressAction,
        source: &str,
        message: &str,
        operation: F,
    ) -> Result<(), PackageManagerError>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = Result<(), PackageManagerError>>,
    {
        self.emit_progress(ProgressEvent {
            kind: ProgressEventType::Start,
            action,
            source: source.to_string(),
            message: Some(message.to_string()),
        });
        match operation().await {
            Ok(()) => {
                self.emit_progress(ProgressEvent {
                    kind: ProgressEventType::Complete,
                    action,
                    source: source.to_string(),
                    message: None,
                });
                Ok(())
            }
            Err(error) => {
                self.emit_progress(ProgressEvent {
                    kind: ProgressEventType::Error,
                    action,
                    source: source.to_string(),
                    message: Some(error.message().to_string()),
                });
                Err(error)
            }
        }
    }

    /// `resolve(onMissing?)`：资源解析主流程。
    ///
    /// 输入是 settings 中的包源与四类资源列表，输出是排序、去重后的四类资源路径加上
    /// 安装失败列表。执行阶段见本文件模块文档；这里补充两点失败语义：
    /// - 包解析中的“安装失败”只记录到 `install_failures`，资源被跳过，整体仍成功；
    /// - settings 读取/形状错误、trust 错误直接向上返回 `Err`。
    async fn resolve_inner(
        &self,
        on_missing: Option<MissingSourceCallback>,
    ) -> Result<ResolvedPaths, PackageManagerError> {
        let mut accumulator = create_accumulator();
        // 每次调用各取一次 settings 快照，后续阶段复用这两份值。
        let global_settings = self.settings_manager.get_global_settings();
        let project_settings = self.settings_manager.get_project_settings();

        // 收集包（project 先，user 后），再按 identity 去重。
        let mut all_packages: Vec<parse::ConfiguredEntry> = Vec::new();
        for pkg in settings_io::settings_packages(&project_settings)? {
            all_packages.push(parse::ConfiguredEntry {
                pkg,
                scope: SourceScope::Project,
            });
        }
        for pkg in settings_io::settings_packages(&global_settings)? {
            all_packages.push(parse::ConfiguredEntry {
                pkg,
                scope: SourceScope::User,
            });
        }
        // dedupe 规则见 `parse` 子模块：project 条目覆盖同 identity 的 user 条目；例外是
        // 项目对象条目 `autoload:false`，此时两条都保留（user 条目作为增量基底）。
        let package_sources = self.dedupe_packages(&all_packages)?;
        let install_failures = self
            .resolve_package_sources(&package_sources, &mut accumulator, on_missing.as_ref())
            .await?;

        // 用户作用域的基准目录是 agent 目录；项目作用域是 <cwd>/.kiana。
        let global_base_dir = self.agent_dir.clone();
        let project_base_dir = paths::join(&[&self.cwd, kiana_core::config::CONFIG_DIR_NAME]);

        // 顶层 settings 资源：每种类型 project 先、user 后。metadata 的 source 固定为
        // "local"、origin 为 top-level，与包内资源（origin=Package）区分。
        for resource_type in ResourceType::ALL {
            let project_entries =
                settings_io::settings_string_list(&project_settings, resource_type.as_str())?;
            let global_entries =
                settings_io::settings_string_list(&global_settings, resource_type.as_str())?;
            let project_metadata = PathMetadata {
                source: "local".to_string(),
                scope: SourceScope::Project,
                origin: SourceOrigin::TopLevel,
                base_dir: None,
                package_root: None,
            };
            self.resolve_local_entries(
                &project_entries,
                resource_type,
                &mut accumulator,
                &project_metadata,
                &project_base_dir,
            );
            let user_metadata = PathMetadata {
                source: "local".to_string(),
                scope: SourceScope::User,
                origin: SourceOrigin::TopLevel,
                base_dir: None,
                package_root: None,
            };
            self.resolve_local_entries(
                &global_entries,
                resource_type,
                &mut accumulator,
                &user_metadata,
                &global_base_dir,
            );
        }

        // 自动发现放在显式 settings 之后：同一路径若已被前面的阶段写入，这里的
        // add_resource 会因“首次写入胜出”被忽略。
        self.add_auto_discovered_resources(
            &mut accumulator,
            &global_settings,
            &project_settings,
            &global_base_dir,
            &project_base_dir,
        )?;

        // 内置扩展默认启用，除非 settings 的 `extensions` 用 `-builtin:x` 等覆盖。
        // `builtin_extension_enabled` 返回 (最终 enabled, 项目级判定)；项目级判定存在时
        // metadata.scope 记为 project，否则记 user（默认走全局覆盖规则）。
        for name in &self.builtin_extensions {
            let path = format!("{}{}", kiana_core::source_info::BUILTIN_PATH_PREFIX, name);
            let (enabled, project_enabled) = self.builtin_extension_enabled(
                &path,
                &project_settings,
                &global_settings,
                &project_base_dir,
                &global_base_dir,
            )?;
            let metadata = PathMetadata {
                source: "builtin".to_string(),
                scope: if project_enabled.is_some() {
                    SourceScope::Project
                } else {
                    SourceScope::User
                },
                origin: SourceOrigin::TopLevel,
                base_dir: None,
                package_root: None,
            };
            add_resource(
                accumulator.target_map(ResourceType::Extensions),
                &path,
                &metadata,
                enabled,
            );
        }

        // `to_resolved_paths` 不带 install_failures（默认 None）；这里总是补上一个可能
        // 为空的列表，与 TS 的 `{...toResolvedPaths(accumulator), installFailures}` 一致。
        let mut resolved = to_resolved_paths(&accumulator);
        resolved.install_failures = Some(install_failures);
        Ok(resolved)
    }

    /// `resolveExtensionSources(sources, options?)`：解析 CLI `-e` 指定的临时源。
    ///
    /// 作用域按 `temporary` → `local` → `user` 的优先级折叠成一个 `SourceScope`，
    /// **所有**源共用这一个作用域，而不是逐源选择。
    ///
    /// 处理分两轮：
    /// 1. 遍历 `sources`，`builtin:` 前缀的源直接以 `enabled = true` 写入扩展累积器
    ///    （metadata 的 `source` 为 `"builtin"`、`origin` 为 `TopLevel`）；
    /// 2. 其余源按输入顺序收集成 `PackageSource::Text` 条目，交给
    ///    `resolve_package_sources` 统一收集资源（缺失时尝试安装）。
    ///
    /// 因为第二轮统一在循环结束后执行，最终顺序是“第一轮的内置扩展在前、第二轮包资源在
    /// 后”。`on_missing` 为 `None`：没有交互回调，包缺失时直接尝试安装；离线模式下缺失的
    /// 包只跳过、不安装。单个包的安装失败只记录进返回值的 `install_failures`，settings
    /// 读取、trust、源字符串解析等错误则原样向上返回。
    async fn resolve_extension_sources_inner(
        &self,
        sources: &[String],
        options: ResolveExtensionSourcesOptions,
    ) -> Result<ResolvedPaths, PackageManagerError> {
        let mut accumulator = create_accumulator();
        let scope = if options.temporary {
            SourceScope::Temporary
        } else if options.local {
            SourceScope::Project
        } else {
            SourceScope::User
        };
        let mut package_sources: Vec<parse::ConfiguredEntry> = Vec::new();
        for source in sources {
            if source.starts_with(kiana_core::source_info::BUILTIN_PATH_PREFIX) {
                let metadata = PathMetadata {
                    source: "builtin".to_string(),
                    scope,
                    origin: SourceOrigin::TopLevel,
                    base_dir: None,
                    package_root: None,
                };
                add_resource(
                    accumulator.target_map(ResourceType::Extensions),
                    source,
                    &metadata,
                    true,
                );
            } else {
                package_sources.push(parse::ConfiguredEntry {
                    pkg: PackageSource::Text(source.clone()),
                    scope,
                });
            }
        }
        let install_failures = self
            .resolve_package_sources(&package_sources, &mut accumulator, None)
            .await?;
        let mut resolved = to_resolved_paths(&accumulator);
        resolved.install_failures = Some(install_failures);
        Ok(resolved)
    }

    /// `checkForAvailableUpdates()`：检查已配置包是否有可用更新。
    ///
    /// 不在 [`PackageManager`] 接口里：它不属于所有实现的通用能力，调用方需要具体类型。
    ///
    /// 流程：离线时直接返回空列表（不读 settings、不启动子进程）；否则按 project 先、
    /// user 后的顺序读取包，用 `dedupe_packages` 去重并过滤掉 `Temporary` 作用域，再用
    /// `UPDATE_CHECK_CONCURRENCY` 并发执行每个包的检查。返回顺序跟随去重后的条目顺序
    /// （`run_with_concurrency` 按输入下标回收结果），单个包的“无更新”在 `flatten`
    /// 时被丢弃。
    ///
    /// 失败：settings 形状错误、trust 失败、源字符串缺失都会让整个方法返回 `Err`
    /// （并发池在第一个错误处停止启动新任务）；单包的网络/子进程错误在底层被吞成
    /// “无更新”。本方法不安装任何东西，也不发进度事件。
    pub async fn check_for_available_updates(
        &self,
    ) -> Result<Vec<PackageUpdate>, PackageManagerError> {
        if is_offline_mode_enabled() {
            return Ok(Vec::new());
        }

        let global_settings = self.settings_manager.get_global_settings();
        let project_settings = self.settings_manager.get_project_settings();
        let mut all_packages: Vec<parse::ConfiguredEntry> = Vec::new();
        for pkg in settings_io::settings_packages(&project_settings)? {
            all_packages.push(parse::ConfiguredEntry {
                pkg,
                scope: SourceScope::Project,
            });
        }
        for pkg in settings_io::settings_packages(&global_settings)? {
            all_packages.push(parse::ConfiguredEntry {
                pkg,
                scope: SourceScope::User,
            });
        }

        let package_sources = self.dedupe_packages(&all_packages)?;
        let mut checks = Vec::new();
        for entry in package_sources
            .into_iter()
            .filter(|entry| entry.scope != SourceScope::Temporary)
        {
            checks.push(move || async move { self.check_package_update(&entry).await });
        }

        let results = concurrency::run_with_concurrency(checks, UPDATE_CHECK_CONCURRENCY).await?;
        Ok(results.into_iter().flatten().collect())
    }

    /// 单个包的更新检查（`checkForAvailableUpdates` 的 worker）。
    ///
    /// 输入是去重后的配置条目。local 源、固定 npm 版本与固定 git ref 直接返回
    /// `Ok(None)`；npm / git 还要求安装路径已存在。npm 用 `npm_has_available_update`
    /// 比较已装版本与 registry 目标版本，git 用 `git_has_available_update` 比较本地与远端
    /// HEAD（`git ls-remote`，只读）。
    ///
    /// 有更新时构造 `PackageUpdate`：npm 的 `display_name` 是包名，git 是 `host/path`；
    /// `scope` 经 `install_scope_of` 折叠成 user/project。底层检查会吞掉网络/子进程错误，
    /// 能向上冒泡的只有源字符串解析、安装路径计算（含 project trust）这类配置错误。
    async fn check_package_update(
        &self,
        entry: &parse::ConfiguredEntry,
    ) -> Result<Option<PackageUpdate>, PackageManagerError> {
        let source = parse::get_package_source_string(&entry.pkg)?.to_string();
        let parsed = self.parse_source(&source);
        if let parse::ParsedSource::Local(_) = parsed {
            return Ok(None);
        }
        let pinned = match &parsed {
            parse::ParsedSource::Npm(npm) => npm.pinned,
            parse::ParsedSource::Git(git) => git.pinned,
            parse::ParsedSource::Local(_) => false,
        };
        if pinned {
            return Ok(None);
        }

        match parsed {
            parse::ParsedSource::Npm(npm) => {
                let installed_path = self.get_npm_install_path(&npm, entry.scope)?;
                if !paths::path_exists(&installed_path) {
                    return Ok(None);
                }
                if !self.npm_has_available_update(&npm, &installed_path).await? {
                    return Ok(None);
                }
                Ok(Some(PackageUpdate {
                    source,
                    display_name: npm.name.clone(),
                    package_type: PackageType::Npm,
                    scope: install_scope_of(entry.scope),
                }))
            }
            parse::ParsedSource::Git(git) => {
                let installed_path = self.get_git_install_path(&git, entry.scope)?;
                if !paths::path_exists(&installed_path) {
                    return Ok(None);
                }
                if !self.git_has_available_update(&installed_path).await? {
                    return Ok(None);
                }
                Ok(Some(PackageUpdate {
                    source,
                    display_name: format!("{}/{}", git.host, git.path),
                    package_type: PackageType::Git,
                    scope: install_scope_of(entry.scope),
                }))
            }
            parse::ParsedSource::Local(_) => Ok(None),
        }
    }

    /// `installedNpmMatchesConfiguredVersion(source, installedPath)`：已装版本是否满足配置。
    ///
    /// `package.json` 缺失 / 读取失败 / 解析失败 / 没有字符串 `version` 时返回 `false`。
    /// `source.range` 为 `None`（源里没有版本段，或版本段不是合法区间，例如 `@latest`）
    /// 时视为“无版本约束”，只要读到版本就返回 `true`；有区间时要求已装版本是合法 semver
    /// 且被 `NpmRange::satisfies` 接受。
    ///
    /// 只读文件、不发网络请求；被 `resolve_package_sources` 用来判断 npm 包是否需要重装，
    /// 因此“读不到版本”会被当作不匹配。
    pub(crate) fn installed_npm_matches_configured_version(
        &self,
        source: &parse::NpmSource,
        installed_path: &str,
    ) -> bool {
        let Some(installed_version) = self.get_installed_npm_version(installed_path) else {
            return false;
        };
        match &source.range {
            Some(range) => match parse::valid_version(&installed_version) {
                Some(version) => range.satisfies(&version),
                None => false,
            },
            None => true,
        }
    }

    /// `npmHasAvailableUpdate(source, installedPath)`：registry 上是否有更新的版本。
    ///
    /// 离线、读不到已装版本、`npm view` 失败、或任一侧版本不是合法 semver 时都返回
    /// `Ok(false)`（错误被吞，避免单个包的网络问题让整个更新检查失败）。
    /// 查询用的 spec 与安装时保持一致：源里写了版本/标签时用原始 `spec`，否则用裸包名
    /// （交给 `npm view <name> version`）。只有目标版本**严格大于**已装版本才返回 `true`。
    async fn npm_has_available_update(
        &self,
        source: &parse::NpmSource,
        installed_path: &str,
    ) -> Result<bool, PackageManagerError> {
        if is_offline_mode_enabled() {
            return Ok(false);
        }
        let Some(installed_version) = self.get_installed_npm_version(installed_path) else {
            return Ok(false);
        };
        let package_spec = if source.version.is_some() {
            source.spec.clone()
        } else {
            source.name.clone()
        };
        let Ok(target_version) = self
            .get_latest_npm_version(&package_spec, source.range.as_ref())
            .await
        else {
            return Ok(false);
        };
        match (
            parse::valid_version(&target_version),
            parse::valid_version(&installed_version),
        ) {
            (Some(target), Some(installed)) => Ok(target > installed),
            _ => Ok(false),
        }
    }

    /// `getInstalledNpmVersion(installedPath)`：读 `<installedPath>/package.json` 的 `version`。
    ///
    /// 文件不存在、读取失败、JSON 解析失败或 `version` 不是字符串时返回 `None`。读取使用
    /// `from_utf8_lossy` 并先去掉 BOM；返回的字符串不做 trim，也不校验 semver，由调用方
    /// 决定如何解释。只访问文件系统。
    pub(crate) fn get_installed_npm_version(&self, installed_path: &str) -> Option<String> {
        let package_json_path = paths::join(&[installed_path, "package.json"]);
        if !paths::path_exists(&package_json_path) {
            return None;
        }
        let bytes = std::fs::read(&package_json_path).ok()?;
        let content = String::from_utf8_lossy(&bytes);
        let content = kiana_core::text::strip_bom(&content);
        let package: serde_json::Value = serde_json::from_str(&content).ok()?;
        package
            .get("version")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string)
    }

    /// `shouldUpdateNpmSource(source, scope)`：`update` 是否应该重装这个 npm 包。
    ///
    /// 只看本管理器自己的安装目录（`get_managed_npm_install_path`，**不**回退遗留的全局
    /// 安装）：路径不存在或读不到版本时返回 `true`。否则查询 registry 的目标版本；查询
    /// 失败、任一侧版本非法、或目标版本严格大于已装版本时也返回 `true`，即拿不到信息时
    /// 选择更新而不是跳过。
    async fn should_update_npm_source(
        &self,
        source: &parse::NpmSource,
        scope: SourceScope,
    ) -> Result<bool, PackageManagerError> {
        let installed_path = self.get_managed_npm_install_path(source, scope)?;
        let installed_version = if paths::path_exists(&installed_path) {
            self.get_installed_npm_version(&installed_path)
        } else {
            None
        };
        let Some(installed_version) = installed_version else {
            return Ok(true);
        };
        let package_spec = if source.version.is_some() {
            source.spec.clone()
        } else {
            source.name.clone()
        };
        match self
            .get_latest_npm_version(&package_spec, source.range.as_ref())
            .await
        {
            Ok(target_version) => {
                match (
                    parse::valid_version(&target_version),
                    parse::valid_version(&installed_version),
                ) {
                    (Some(target), Some(installed)) => Ok(target > installed),
                    _ => Ok(true),
                }
            }
            // 版本查询失败时保留既有更新行为。
            Err(_) => Ok(true),
        }
    }

    /// `updateNpmBatch(sources, scope)`：把一组 npm 源合并成一次安装命令。
    ///
    /// 空列表直接返回 `Ok(())`，不发进度。单条时进度事件的 `source` 是该源字符串、消息是
    /// `Updating <source>...`；多条时 `source` 为 `"<scope> npm packages"`、消息为
    /// `Updating <scope> npm packages...`。传给 npm 的 spec：源里带版本/标签
    /// （`version.is_some()`）时保留原始 `spec`（例如 `pkg@^1.2.3`），否则拼成
    /// `name@latest`。整个批量操作包在一条 `Update` 进度事件里，失败时先发 `Error`
    /// 再把原错误返回。
    async fn update_npm_batch(
        &self,
        sources: &[NpmUpdateTarget],
        scope: SourceScope,
    ) -> Result<(), PackageManagerError> {
        if sources.is_empty() {
            return Ok(());
        }
        let source_label = if sources.len() == 1 {
            sources[0].source.clone()
        } else {
            format!("{} npm packages", scope_label(scope))
        };
        let message = if sources.len() == 1 {
            format!("Updating {}...", sources[0].source)
        } else {
            format!("Updating {} npm packages...", scope_label(scope))
        };
        let specs: Vec<String> = sources
            .iter()
            .map(|entry| {
                if entry.parsed.version.is_some() {
                    entry.parsed.spec.clone()
                } else {
                    format!("{}@latest", entry.parsed.name)
                }
            })
            .collect();
        self.with_progress(ProgressAction::Update, &source_label, &message, || async {
            self.install_npm_batch(&specs, scope).await
        })
        .await
    }

    /// `updateConfiguredSources(sources)`：并发更新一批已配置的源。
    ///
    /// 离线或空输入直接返回 `Ok(())`。先按源类型分类：固定版本的 npm 包跳过（`npm.pinned`），
    /// 其余 npm 包进入候选；**所有** git 包都进入候选（固定 ref 的语义是把 checkout 对齐到
    /// 该 ref）；local 源忽略。
    ///
    /// 随后三个阶段：
    /// 1. 用 `UPDATE_CHECK_CONCURRENCY` 并发调用 `should_update_npm_source`，把需要更新的
    ///    npm 候选按 `scope == User` 分成 user / project 两组（其余作用域归入 project 组；
    ///    本方法的输入来自 settings，实际只有这两种）；
    /// 2. 三个顶层 future 并发执行：user npm 批量更新、project npm 批量更新、git 候选按
    ///    `GIT_UPDATE_CONCURRENCY` 并发更新；
    /// 3. 用 `join_first_error` 等三者结束，返回**最先被 poll 到**的错误（同一轮多个错误
    ///    时按 user / project / git 的参数顺序取；错误出现后其余 future 被丢弃。与 TS
    ///    `Promise.all` 的差异见 `concurrency::join_first_error` 文档）。
    ///
    /// 某个分支先失败时其余 future 会被 drop；`exec` 的子进程没有设置 `kill_on_drop`，
    /// 因此已启动的进程可能继续在后台运行。已经完成的安装/更新不会回滚。npm 候选的安装
    /// 路径计算失败（project 未受信等配置错误）会让整个方法失败；npm 版本查询失败被吞成
    /// "需要更新"，不会让方法失败。
    async fn update_configured_sources(
        &self,
        sources: &[ConfiguredUpdateSource],
    ) -> Result<(), PackageManagerError> {
        if is_offline_mode_enabled() || sources.is_empty() {
            return Ok(());
        }

        let mut npm_candidates: Vec<NpmUpdateTarget> = Vec::new();
        let mut git_candidates: Vec<GitUpdateTarget> = Vec::new();
        for entry in sources {
            // 固定的 npm 版本不动；固定的 git ref 是配置的 checkout 目标，需要参与 reconcile。
            match self.parse_source(&entry.source) {
                parse::ParsedSource::Npm(npm) => {
                    if !npm.pinned {
                        npm_candidates.push(NpmUpdateTarget {
                            source: entry.source.clone(),
                            scope: entry.scope,
                            parsed: npm,
                        });
                    }
                }
                parse::ParsedSource::Git(git) => {
                    git_candidates.push(GitUpdateTarget {
                        source: entry.source.clone(),
                        scope: entry.scope,
                        parsed: git,
                    });
                }
                parse::ParsedSource::Local(_) => {}
            }
        }

        let mut checks = Vec::new();
        for entry in &npm_candidates {
            let entry = entry.clone();
            checks.push(move || async move {
                self.should_update_npm_source(&entry.parsed, entry.scope)
                    .await
            });
        }
        let npm_check_results =
            concurrency::run_with_concurrency(checks, UPDATE_CHECK_CONCURRENCY).await?;
        let mut user_npm_updates: Vec<NpmUpdateTarget> = Vec::new();
        let mut project_npm_updates: Vec<NpmUpdateTarget> = Vec::new();
        for (entry, should_update) in npm_candidates.iter().zip(npm_check_results) {
            if !should_update {
                continue;
            }
            if entry.scope == SourceScope::User {
                user_npm_updates.push(entry.clone());
            } else {
                project_npm_updates.push(entry.clone());
            }
        }

        let git_entries = git_candidates;
        let user_updates = user_npm_updates;
        let project_updates = project_npm_updates;
        let git_task = async {
            let mut tasks = Vec::new();
            for entry in &git_entries {
                tasks.push(|| async {
                    self.with_progress(
                        ProgressAction::Update,
                        &entry.source,
                        &format!("Updating {}...", entry.source),
                        || async { self.update_git(&entry.parsed, entry.scope).await },
                    )
                    .await
                });
            }
            concurrency::run_with_concurrency(tasks, GIT_UPDATE_CONCURRENCY)
                .await
                .map(|_| ())
        };
        let user_task = async {
            self.update_npm_batch(&user_updates, SourceScope::User)
                .await
        };
        let project_task = async {
            self.update_npm_batch(&project_updates, SourceScope::Project)
                .await
        };

        // TS 的 `Promise.all(tasks)` 以**最先 reject** 的分支失败。
        concurrency::join_first_error(user_task, project_task, git_task).await
    }
}

/// `scope` 的展示名（`"user" | "project"`）。
///
/// 代码用 `if scope == Project` 二选一，因此 `Temporary` 也会返回 `"user"`；当前调用点
/// 只传 user/project，这是兜底行为而非独立语义。
fn scope_label(scope: SourceScope) -> &'static str {
    if scope == SourceScope::Project {
        "project"
    } else {
        "user"
    }
}

/// `SourceScope` → [`InstallScope`]（只用于 user/project 的场合）。
///
/// 与 [`scope_label`] 一样用 `if == Project` 判断，`Temporary` 会折叠成
/// [`InstallScope::User`]。调用方传入的条目来自 settings，不含 `Temporary`。
fn install_scope_of(scope: SourceScope) -> InstallScope {
    if scope == SourceScope::Project {
        InstallScope::Project
    } else {
        InstallScope::User
    }
}

#[async_trait]
impl PackageManager for DefaultPackageManager {
    async fn resolve(
        &self,
        on_missing: Option<MissingSourceCallback>,
    ) -> Result<ResolvedPaths, PackageManagerError> {
        self.resolve_inner(on_missing).await
    }

    async fn install(
        &self,
        source: &str,
        options: InstallOptions,
    ) -> Result<(), PackageManagerError> {
        let parsed = self.parse_source(source);
        let scope = if options.local {
            SourceScope::Project
        } else {
            SourceScope::User
        };
        self.assert_project_trusted_for_scope(scope)?;
        self.with_progress(
            ProgressAction::Install,
            source,
            &format!("Installing {source}..."),
            || async {
                match &parsed {
                    parse::ParsedSource::Npm(npm) => self.install_npm(npm, scope, false).await,
                    parse::ParsedSource::Git(git) => self.install_git(git, scope).await,
                    parse::ParsedSource::Local(local) => {
                        let resolved = self.resolve_path(&local.path);
                        if !paths::path_exists(&resolved) {
                            return Err(err(format!("Path does not exist: {resolved}")));
                        }
                        Ok(())
                    }
                }
            },
        )
        .await
    }

    async fn install_and_persist(
        &self,
        source: &str,
        options: InstallOptions,
    ) -> Result<(), PackageManagerError> {
        self.install(source, options).await?;
        self.try_add_source_to_settings(source, options)?;
        Ok(())
    }

    async fn remove(
        &self,
        source: &str,
        options: InstallOptions,
    ) -> Result<(), PackageManagerError> {
        let parsed = self.parse_source(source);
        let scope = if options.local {
            SourceScope::Project
        } else {
            SourceScope::User
        };
        self.assert_project_trusted_for_scope(scope)?;
        self.with_progress(
            ProgressAction::Remove,
            source,
            &format!("Removing {source}..."),
            || async {
                match &parsed {
                    parse::ParsedSource::Npm(npm) => self.uninstall_npm(npm, scope).await,
                    parse::ParsedSource::Git(git) => self.remove_git(git, scope).await,
                    parse::ParsedSource::Local(_) => Ok(()),
                }
            },
        )
        .await
    }

    async fn remove_and_persist(
        &self,
        source: &str,
        options: InstallOptions,
    ) -> Result<bool, PackageManagerError> {
        self.remove(source, options).await?;
        self.try_remove_source_from_settings(source, options)
    }

    async fn update(&self, source: Option<&str>) -> Result<(), PackageManagerError> {
        let global_settings = self.settings_manager.get_global_settings();
        let project_settings = self.settings_manager.get_project_settings();
        let identity = match source {
            Some(source) => Some(self.get_package_identity(source, None)?),
            None => None,
        };
        let mut matched = false;
        let mut update_sources: Vec<ConfiguredUpdateSource> = Vec::new();

        // 先 user、后 project；给了 `source` 时按 identity 过滤（输入侧 identity 以 `cwd`
        // 为基准，settings 条目侧的 local 路径按各自作用域基准解析），没给时全部收集。
        // `matched` 只在给了 source 时用于区分“配置里根本没有”与“有但无需更新”。
        for pkg in settings_io::settings_packages(&global_settings)? {
            let source_str = parse::get_package_source_string(&pkg)?.to_string();
            if let Some(identity) = &identity
                && self.get_package_identity(&source_str, Some(SourceScope::User))? != *identity
            {
                continue;
            }
            matched = true;
            update_sources.push(ConfiguredUpdateSource {
                source: source_str,
                scope: SourceScope::User,
            });
        }
        for pkg in settings_io::settings_packages(&project_settings)? {
            let source_str = parse::get_package_source_string(&pkg)?.to_string();
            if let Some(identity) = &identity
                && self.get_package_identity(&source_str, Some(SourceScope::Project))? != *identity
            {
                continue;
            }
            matched = true;
            update_sources.push(ConfiguredUpdateSource {
                source: source_str,
                scope: SourceScope::Project,
            });
        }

        // 有 source 却没有匹配条目：构造与 TS 相同的“未找到（可能带 Did you mean）”文本。
        // 无 source 时不会走到这里。
        if let Some(source) = source
            && !matched
        {
            let configured: Vec<PackageSource> = settings_io::settings_packages(&global_settings)?
                .into_iter()
                .chain(settings_io::settings_packages(&project_settings)?)
                .collect();
            return Err(err(
                self.build_no_matching_package_message(source, &configured)
            ));
        }

        self.update_configured_sources(&update_sources).await
    }

    fn list_configured_packages(&self) -> Vec<ConfiguredPackage> {
        let global_settings = self.settings_manager.get_global_settings();
        let project_settings = self.settings_manager.get_project_settings();
        let mut configured_packages = Vec::new();

        // 顺序固定为 user 在前、project 在后。`filtered` 表示该条目在 settings 里是对象
        // 形式（带过滤规则）；`installed_path` 复用 trait 的 `get_installed_path`，路径
        // 不存在或路径解析出错都为 `None`。
        //
        // 冻结签名为 `Vec`（TS 在 project 未受信/设置非法时会抛错）：settings 读取失败用
        // `unwrap_or_default()` 当成空列表，单个条目无法解析时跳过。调用方无法从返回值
        // 区分“没有配置”和“读取失败”。
        for pkg in settings_io::settings_packages(&global_settings).unwrap_or_default() {
            let Ok(source) = parse::get_package_source_string(&pkg) else {
                continue;
            };
            let source = source.to_string();
            configured_packages.push(ConfiguredPackage {
                installed_path: self.get_installed_path(&source, InstallScope::User),
                source,
                scope: InstallScope::User,
                filtered: matches!(pkg, PackageSource::Object(_)),
            });
        }
        for pkg in settings_io::settings_packages(&project_settings).unwrap_or_default() {
            let Ok(source) = parse::get_package_source_string(&pkg) else {
                continue;
            };
            let source = source.to_string();
            configured_packages.push(ConfiguredPackage {
                installed_path: self.get_installed_path(&source, InstallScope::Project),
                source,
                scope: InstallScope::Project,
                filtered: matches!(pkg, PackageSource::Object(_)),
            });
        }
        configured_packages
    }

    async fn resolve_extension_sources(
        &self,
        sources: &[String],
        options: ResolveExtensionSourcesOptions,
    ) -> Result<ResolvedPaths, PackageManagerError> {
        self.resolve_extension_sources_inner(sources, options).await
    }

    fn add_source_to_settings(&self, source: &str, options: InstallOptions) -> bool {
        // 解析到 `settings_io` 里的同名 inherent 方法（inherent 方法优先于 trait 方法），
        // 不是递归调用。
        self.add_source_to_settings(source, options)
    }

    fn remove_source_from_settings(&self, source: &str, options: InstallOptions) -> bool {
        // 同上：调用的是 `settings_io` 的 inherent 版本。
        self.remove_source_from_settings(source, options)
    }

    fn set_progress_callback(&self, callback: Option<ProgressCallback>) {
        *npm::lock(&self.progress_callback) = callback;
    }

    fn get_installed_path(&self, source: &str, scope: InstallScope) -> Option<String> {
        // 冻结签名是 `Option<String>`；TS 在 project 未受信时会抛错，调用方可改用
        // [`DefaultPackageManager::try_get_installed_path`] 获得 `Result`。
        self.try_get_installed_path(source, scope).ok().flatten()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn offline_flag_parsing_matches_ts() {
        // 该测试会改动进程环境；`KIANA_OFFLINE` 是测试期唯一安全共享的变量。
        unsafe {
            std::env::remove_var("KIANA_OFFLINE");
        }
        assert!(!is_offline_mode_enabled());
        for value in ["1", "true", "TRUE", "yes", "Yes"] {
            unsafe {
                std::env::set_var("KIANA_OFFLINE", value);
            }
            assert!(is_offline_mode_enabled(), "value {value}");
        }
        for value in ["0", "false", "no", "2", ""] {
            unsafe {
                std::env::set_var("KIANA_OFFLINE", value);
            }
            assert!(!is_offline_mode_enabled(), "value {value}");
        }
        unsafe {
            std::env::remove_var("KIANA_OFFLINE");
        }
    }
}
