//! `core/resource-loader.ts`（1322 行）：`DefaultResourceLoader` / `ResourceLoader` trait /
//! reload 步骤机。
//!
//! 与 TS 的并发模型差异（`spec/ca-resources.md` §8.3 的推荐方案）：TS 在单线程事件循环里原地
//! 修改 `this.xxx`；Rust 用 `&self` + `RwLock<LoaderState>`。锁**不跨 `await` 持有**：每步先在
//! 局部变量里算出结果，再按与 TS 相同的位置写回 `state`。因此 reload 中途失败时，外部可见的
//! 状态与 TS 一样停留在“已完成步骤”的混合状态，而不是回滚或最后一次性替换（测试
//! `failed_reload_keeps_the_new_extensions_result_like_ts` 锁定第 19 步先安装扩展结果的行为）。
//! getter 都同步返回克隆。
//!
//! **`reload` 的 27 步顺序不可调整**（§2.11.11、§8.4 第 7 条）：任何一步提前/延后都会改变
//! 优先级、metadata 归属或诊断顺序。下面每一步都标了 TS 源里的步骤号。
//!
//! `kiana-extensions` 的 `load_extensions_cached` / `load_extension_from_factory` 返回
//! **非 `Send`** 的 future（rquickjs 的 `Persistent` 是 `!Send`），因此 [`ResourceLoader::reload`]
//! 用 [`kiana_ext_api::LocalBoxFuture`] 而不是 `async_trait` 的 `Send` future；调用方应在
//! `tokio::task::LocalSet` 上驱动它（`ARCHITECTURE.md` §4.2）。
//!
//! # 这个文件负责什么
//!
//! [`DefaultResourceLoader`] 把四类可配置资源——扩展、技能、prompt 模板、主题——以及上下文
//! 文件与系统提示，从「settings 里的包配置 + 标准目录 + CLI/扩展注入的路径」收敛成一份可
//! 直接查询的快照。没有它，每个调用方都要自己处理包解析、启用/禁用、路径优先级、元数据
//! 归属与诊断文案，并且无法保证与 TS 的顺序一致。
//!
//! 输入是构造时的 [`DefaultResourceLoaderOptions`]；输出是 9 个 getter 的快照，以及
//! [`ResourceLoader::reload`] 的 `Result<(), ResourceLoaderError>`。`reload` 的副作用是
//! 重读 settings、可能写回内存中的项目信任状态、重新求值扩展模块。
//!
//! # 一次 reload 的阶段
//!
//! 1. **准备（步骤 1-7）**：重置 `extensions` 计时、按需清扩展工厂缓存、解析项目信任、
//!    重读 settings、解析 settings 里的包与 `-e` 临时源、汇总安装失败警告。
//! 2. **装扩展（步骤 8-20）**：重建 path→metadata 与三张扩展来源表，合并出最终扩展路径
//!    并过滤禁用的内置扩展，加载扩展，套用 `extensions_override`，最后回填 `source_info`。
//! 3. **装其余资源（步骤 21-27）**：按固定优先级合并路径，加载技能/prompt/主题，加载上下文
//!    文件，解析系统提示与追加提示，置 `loaded = true`。
//!
//! 每个步骤的编号与 TS 的 `reload()` 一一对应；步骤之间的顺序是外部契约，改动会改变优先级、
//! metadata 归属或诊断顺序（见 `spec/ca-resources.md` §2.11.11）。
//!
//! # 状态结构与顺序敏感点
//!
//! 所有 getter 都从 `DefaultResourceLoader` 内部的 `LoaderState` 克隆一份快照（`Vec` /
//! `IndexMap` 逐元素克隆；主题与扩展 runtime 是 `Arc` 共享）。`LoaderState` 可分为四组：
//!
//! - getter 结果：扩展、技能、prompt、主题、上下文文件、系统提示及其来源路径；
//! - 上次安装路径 `last_*_paths`：`extend_resources` 在此基础上继续合并新路径，`reload`
//!   的第 21-23 步会整体覆盖；
//! - `extend_resources` 注入的来源表 `extension_*_source_infos`：`reload` 第 9 步清空，之后
//!   由（第 16 步 await 期间的）并发 `extend_resources` 重新填充；
//! - metadata 表 `resource_metadata_by_path`：第 14 步写入，供 `extend_resources` 与第 16 步
//!   await 期间的并发调用使用。
//!
//! metadata 表是 `IndexMap`；`find_source_info_for_path` 在精确匹配失败后按插入序做前缀匹配
//! 并返回第一个命中。因此第 10-13 步的插入顺序（resolved extensions → skills → prompts →
//! themes → CLI 四类）本身就是嵌套路径的归属优先级：改动这些步骤的顺序不仅改变诊断顺序，
//! 还会改变资源最终报告的 `source` / `scope`。
//!
//! # 并发、重入与线程归属
//!
//! - `state` 由 `std::sync::RwLock` 保护；读锁只覆盖字段克隆，写锁通常只覆盖字段写入。
//!   例外是第 20 步：`apply_extension_source_info` 在写锁内执行并可能 `stat`，期间任何尝试
//!   读取 `state` 的调用都会阻塞；该函数不调用用户回调，所以不会因重入而死锁。`extend_resources`
//!   的第一个写锁块内也会经 `merge_paths`（`canonicalize_path` / realpath 的 lstat、readlink）
//!   做文件系统访问，但同样不调用用户回调。
//! - 用户提供的 override 回调一律在锁外调用，因此回调里可以再调用 getter（测试
//!   `extend_resources_overrides_may_call_getters_without_deadlocking` 锁定了这一点）。
//! - 锁中毒时 `read_lock` / `write_lock` 用 `into_inner` 取回数据而不 panic；持锁代码若
//!   panic，后续调用仍能工作，但可能看到写了一半的状态。
//! - 扩展工厂缓存是 thread-local（`kiana_extensions::loader::factory_cache`），reload 的
//!   future 又是非 `Send` 的，因此创建、reload、查询都应在同一线程完成（调用方用
//!   `current_thread` runtime / `LocalSet`）。
//!
//! # 失败模式
//!
//! - 包解析失败 → [`ResourceLoaderError::PackageManager`]；扩展包 `package.json` 读取/解析
//!   失败 → [`ResourceLoaderError::Manifest`]；回填来源信息时的 `stat` 失败 →
//!   [`ResourceLoaderError::Io`]。三者都会让 reload 提前返回，且**不回滚**已写回的状态。
//! - 单个扩展加载失败只进 `LoadExtensionsResult.errors`，不中断 reload。
//! - `additional_*` 里不存在的本地路径只产生诊断/错误条目，不使 reload 失败。
//!
//! # 阅读顺序
//!
//! 先看 [`DefaultResourceLoaderOptions`] 与 [`DefaultResourceLoader::new`]，再读上面的
//! “状态结构与顺序敏感点”与 [`DefaultResourceLoader::reload`] 的 27 步注释；随后按出现顺序
//! 看辅助函数：
//! `source_info_map`（路径与来源信息）、`warnings`（警告文案）、`context_files`、
//! `themes`。

pub mod context_files;
pub mod source_info_map;
pub mod themes;
pub mod warnings;

use std::collections::HashSet;
use std::fmt;
use std::future::Future;
use std::path::Path;
use std::pin::Pin;
use std::sync::{Arc, RwLock, RwLockReadGuard, RwLockWriteGuard};

use indexmap::IndexMap;
use kiana_config::settings::SettingsManager;
use kiana_core::config::CONFIG_DIR_NAME;
use kiana_core::diagnostics::{
    DiagnosticKind, ResourceCollision, ResourceDiagnostic, ResourceType,
};
use kiana_core::event_bus::{EventBus, create_event_bus};
use kiana_core::paths::{PathInputOptions, is_local_path, resolve_path};
use kiana_core::source_info::{
    BUILTIN_PATH_PREFIX, PathMetadata, SourceInfo, SourceOrigin, SourceScope, create_source_info,
};
use kiana_core::text::strip_bom;
use kiana_core::timings::{TimingLabel, reset_timings};
use kiana_ext_api::LocalBoxFuture;
use kiana_ext_api::system_prompt::{ContextFile, Skill};
use kiana_ext_api::types::{
    Extension, ExtensionFactory, ExtensionRuntime, InlineExtension, LoadError,
    LoadExtensionsResult, LoadWarning,
};
use kiana_tui::terminal_image::{
    CapabilityOverrides, TerminalCapabilities, detect_capabilities, get_terminal_color_mode,
};

use crate::package::{
    DefaultPackageManager, PackageManager, PackageManagerError, PackageManagerOptions,
    ResolveExtensionSourcesOptions, ResolvedResource,
};
use crate::prompts::{LoadPromptTemplatesOptions, PromptTemplate, load_prompt_templates};
use crate::skills::{LoadSkillsOptions, load_skills};

/// `ResourceExtensionPaths` 的一项：路径 + 元数据。
/// TS: `resource-loader.ts#ResourceExtensionPaths` 的内联对象。
///
/// `path` 可以是相对路径、目录、文件或 `file://` URL；
/// [`DefaultResourceLoader::extend_resources`] 会以 loader 的 `cwd` 为基准解析它，并把非空的
/// `metadata.base_dir` 一并解析（空字符串按 TS 的 truthiness 规则原样保留）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResourceExtensionPath {
    /// 资源路径（文件、目录或 `file://` URL）。
    pub path: String,
    /// 来源元数据。
    pub metadata: PathMetadata,
}

/// 扩展通过 `extendResources()` 注入的资源路径。
/// TS: `resource-loader.ts#ResourceExtensionPaths`。
///
/// 三个列表独立处理：只有非空的列表才会触发对应类型的重新加载与 override 回调。调用方是
/// `kiana-session`：它把扩展 `resources_discover` 事件返回的路径装进这里再调用
/// [`ResourceLoader::extend_resources`]。
#[derive(Debug, Clone, Default)]
pub struct ResourceExtensionPaths {
    /// 技能路径。
    pub skill_paths: Vec<ResourceExtensionPath>,
    /// prompt 模板路径。
    pub prompt_paths: Vec<ResourceExtensionPath>,
    /// 主题路径。
    pub theme_paths: Vec<ResourceExtensionPath>,
}

/// `resolveProjectTrust` 返回的 future（非 `Send`，与扩展加载同线程）。
///
/// 之所以不用 `Pin<Box<dyn Future + Send>>`：回调里通常要继续调用扩展加载器，而那条链上的
/// `Persistent` 是 `!Send`；整个 reload 因此也被限制在同一线程。
pub type ResolveProjectTrustFuture = Pin<Box<dyn Future<Output = bool> + 'static>>;

/// `reload({ resolveProjectTrust })` 的回调。TS 收到 `{ extensionsResult }` 对象；
/// Rust 直接收到预信任阶段的结果。
///
/// 入参是项目信任被强制设为 `false`、重新读取 settings 后加载的扩展集合：用户/全局资源 +
/// `-e` 临时源 + 内联工厂，**不含**项目资源与 `builtin:`。回调返回 `true` 时 reload 才会把
/// 项目标记为可信并重新读取 settings，再加载项目资源；返回 `false` 则本轮继续跳过项目资源。
pub type ResolveProjectTrust = Box<dyn Fn(LoadExtensionsResult) -> ResolveProjectTrustFuture>;

/// `reload(options?)` 的选项。TS: `resource-loader.ts#ResourceLoaderReloadOptions`。
#[derive(Default)]
pub struct ResourceLoaderReloadOptions {
    /// 项目信任解析（存在时先跑预信任引导）。
    ///
    /// 只对本次 `reload` 生效；为 `None` 时不触碰项目信任状态。回调在锁外执行，可以安全
    /// 调用 loader 的 getter。
    pub resolve_project_trust: Option<ResolveProjectTrust>,
}

impl fmt::Debug for ResourceLoaderReloadOptions {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ResourceLoaderReloadOptions")
            .field(
                "resolve_project_trust",
                &self.resolve_project_trust.is_some(),
            )
            .finish()
    }
}

/// `getSkills()` 的返回值。TS: `resource-loader.ts#ResourceLoader.getSkills`。
///
/// 同一个类型也是 `skills_override` 的输入与输出。override 返回后，loader 会按每个技能的
/// `file_path` 重新计算 `source_info`：命中扩展注入的来源或 metadata 表时覆盖，未命中时保留
/// override 给的值；`diagnostics` 原样保留。
#[derive(Debug, Clone, Default)]
pub struct SkillsResult {
    /// 技能。
    pub skills: Vec<Skill>,
    /// 诊断。
    pub diagnostics: Vec<ResourceDiagnostic>,
}

/// `getPrompts()` 的返回值。TS: `resource-loader.ts#ResourceLoader.getPrompts`。
///
/// 与 [`SkillsResult`] 一样兼作 `prompts_override` 的输入/输出；override 之后会按
/// `file_path` 重新回填 `source_info`。去重（同名首见保留）发生在 override **之前**。
#[derive(Debug, Clone, Default)]
pub struct PromptsResult {
    /// 模板。
    pub prompts: Vec<PromptTemplate>,
    /// 诊断。
    pub diagnostics: Vec<ResourceDiagnostic>,
}

/// `getThemes()` 的返回值。TS: `resource-loader.ts#ResourceLoader.getThemes`。
///
/// 兼作 `themes_override` 的输入/输出。override 之后：有 `source_path` 的主题先查扩展来源
/// 与 metadata；查不到且自身没有 `source_info` 时退回默认推断——这一步会对路径做 `stat`，
/// 路径不存在会让整个 `reload` 返回 [`ResourceLoaderError::Io`]。
#[derive(Debug, Clone, Default)]
pub struct ThemesResult {
    /// 主题（`Arc` 共享，与 TS 的对象引用一致）。
    pub themes: Vec<Arc<kiana_theme::Theme>>,
    /// 诊断。
    pub diagnostics: Vec<ResourceDiagnostic>,
}

/// `getAgentsFiles()` 的返回值。TS: `resource-loader.ts#ResourceLoader.getAgentsFiles`。
///
/// 也是 `agents_files_override` 的输入/输出；override 返回的列表整体替换自动发现结果。
#[derive(Debug, Clone, Default)]
pub struct AgentsFilesResult {
    /// 上下文文件（全局 → 根祖先 → cwd）。
    pub agents_files: Vec<ContextFile>,
}

/// `getSystemPromptSource()` / `getAppendSystemPromptSources()` 的元素。
/// TS 的内联对象 `{ path: string }`。
///
/// 只有在来源确实是一个**存在的文件**时才会产生；字面文本（或指向不存在路径的字符串）
/// 仍会成为系统提示内容，但不会暴露成 source。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SystemPromptSource {
    /// 文件路径。
    pub path: String,
}

/// `reload()` 的错误。`Display` 逐字等于 TS 对应错误的 `error.message`。
///
/// reload 在第一个错误处提前返回，**不**回滚此前已经写回的字段；因此调用方拿到 `Err` 后仍
/// 应把 loader 视为“部分更新”。`kiana-session` 的调用点只把它转成
/// `AiErrorKind::Other` 并保留文本，不会区分具体变体。
#[derive(Debug)]
pub enum ResourceLoaderError {
    /// 扩展包 `package.json` 无法读取或解析（TS 的未捕获 `JSON.parse`）。
    Manifest(warnings::ManifestParseError),
    /// 文件系统错误（TS 里未捕获的 `statSync` / `readFileSync`）。
    Io(std::io::Error),
    /// 包管理器 `resolve()` / `resolveExtensionSources()` 失败。
    PackageManager(PackageManagerError),
}

impl fmt::Display for ResourceLoaderError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ResourceLoaderError::Manifest(error) => error.fmt(formatter),
            ResourceLoaderError::Io(error) => error.fmt(formatter),
            ResourceLoaderError::PackageManager(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for ResourceLoaderError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            ResourceLoaderError::Manifest(error) => Some(error),
            ResourceLoaderError::Io(error) => Some(error),
            ResourceLoaderError::PackageManager(error) => Some(error),
        }
    }
}

impl From<warnings::ManifestParseError> for ResourceLoaderError {
    fn from(error: warnings::ManifestParseError) -> Self {
        ResourceLoaderError::Manifest(error)
    }
}

impl From<std::io::Error> for ResourceLoaderError {
    fn from(error: std::io::Error) -> Self {
        ResourceLoaderError::Io(error)
    }
}

impl From<PackageManagerError> for ResourceLoaderError {
    fn from(error: PackageManagerError) -> Self {
        ResourceLoaderError::PackageManager(error)
    }
}

/// 覆盖函数类型别名（TS 的 7 个 `xxxOverride`）。
///
/// 每个 override 都在对应资源加载**之后**调用，返回的整个结果替换基础结果（不是增量合并）。
/// 扩展/技能/prompt/主题这四类还会在 override 之后回填 `source_info`。它们都在内部锁之外
/// 执行，因此实现里可以调用 loader 的 getter。
pub type ExtensionsOverride = Box<dyn Fn(LoadExtensionsResult) -> LoadExtensionsResult>;
/// `skillsOverride`。入参是自动加载/注入路径得到的结果；返回值的 `source_info` 会被重新
/// 计算（见 [`SkillsResult`]）。
pub type SkillsOverride = Box<dyn Fn(SkillsResult) -> SkillsResult>;
/// `promptsOverride`。入参已按 name 去重；返回后重新回填 `source_info`（见 [`PromptsResult`]）。
pub type PromptsOverride = Box<dyn Fn(PromptsResult) -> PromptsResult>;
/// `themesOverride`。入参已按 name 去重；返回后重新回填 `source_info`（见 [`ThemesResult`]）。
pub type ThemesOverride = Box<dyn Fn(ThemesResult) -> ThemesResult>;
/// `agentsFilesOverride`。返回的列表成为 `get_agents_files()` 的最终结果。
pub type AgentsFilesOverride = Box<dyn Fn(AgentsFilesResult) -> AgentsFilesResult>;
/// `systemPromptOverride`。入参是已解析的正文（`None` 表示没有来源）；返回 `None` 表示最终
/// 没有系统提示。
pub type SystemPromptOverride = Box<dyn Fn(Option<String>) -> Option<String>>;
/// `appendSystemPromptOverride`。入参是各来源解析后的正文列表（空串项已过滤）；返回值原样
/// 成为 `get_append_system_prompt()`，不会再过滤。
pub type AppendSystemPromptOverride = Box<dyn Fn(Vec<String>) -> Vec<String>>;

/// `DefaultResourceLoader` 的构造选项。TS: `resource-loader.ts#DefaultResourceLoaderOptions`。
///
/// 选项在 [`DefaultResourceLoader::new`] 时一次性消费：`cwd` / `agent_dir` 以进程当前目录为
/// 基准解析；缺省的 `settings_manager` / `event_bus` 会被创建；`extension_factories` 中
/// `builtin: true` 的具名项移入独立表，供 `builtin:<name>` 路径使用。各 `*_override` 在
/// **每次** reload 的对应步骤调用，不是只调用一次。
///
/// `no_extensions` / `no_skills` / `no_prompt_templates` / `no_themes` 只关闭自动发现与
/// settings 合并分支；`additional_*` 传入的路径仍会加载（技能在测试
/// `should_still_load_additional_skill_paths_when_no_skills_is_true` 中锁定）。
#[derive(Default)]
pub struct DefaultResourceLoaderOptions {
    /// 项目工作目录。
    pub cwd: String,
    /// agent 配置目录。
    pub agent_dir: String,
    /// settings 管理器（缺省 `SettingsManager.create(cwd, agentDir)`）。
    ///
    /// 传入外部实例时，reload 会调用它的 `reload()`；预信任阶段还会改写其内存中的项目信任
    /// 标记（`set_project_trusted`），因此该实例的可见状态会随 reload 变化。
    pub settings_manager: Option<SettingsManager>,
    /// 事件总线（缺省新建）。
    pub event_bus: Option<Arc<dyn EventBus>>,
    /// `-e` 传入的额外扩展源。
    ///
    /// 可以是本地路径、包源（如 `npm:`）或 `builtin:<name>`，由包管理器按 temporary 作用域
    /// 解析。不存在的本地路径在 reload 第 18 步记一条 `LoadError`，不会让 reload 失败。
    pub additional_extension_paths: Vec<String>,
    /// 额外技能路径。
    pub additional_skill_paths: Vec<String>,
    /// 额外 prompt 模板路径。
    pub additional_prompt_template_paths: Vec<String>,
    /// 额外主题路径。
    pub additional_theme_paths: Vec<String>,
    /// 内联扩展（含 `builtin: true` 的具名内置扩展）。
    ///
    /// `builtin: true` 的项不参与普通内联加载：只有最终扩展集合里出现 `builtin:<name>`
    /// （settings 或 `-e` 指定）时才实例化，并且在项目信任解析之后。
    pub extension_factories: Vec<InlineExtension>,
    /// 不加载任何扩展（`-e builtin:<name>` 仍生效）。
    pub no_extensions: bool,
    /// 按名禁用的内置扩展。
    ///
    /// 存的是不带 `builtin:` 前缀的名字（如 `"mcp"`）；过滤作用在合并后的最终扩展路径上，
    /// 所以 settings 的 `+builtin:x` 与 `-e builtin:x` 都绕不过它。
    pub disabled_builtin_extensions: Vec<String>,
    /// 不加载技能。
    pub no_skills: bool,
    /// 不加载 prompt 模板。
    pub no_prompt_templates: bool,
    /// 不加载主题。
    pub no_themes: bool,
    /// 不加载上下文文件。
    pub no_context_files: bool,
    /// 系统提示来源（文件路径或字面文本）。
    ///
    /// reload 第 25 步解析：路径存在时读取文件内容（去 BOM），否则按字面文本使用；读取失败
    /// 会记一条 warning 并回退为路径原文。显式给出时优先于自动发现的 `SYSTEM.md`。
    pub system_prompt: Option<String>,
    /// 追加系统提示来源列表。
    ///
    /// 每项同样按“存在的文件读内容，否则字面文本”解析；解析为空的项被丢弃。给出时完全替代
    /// 自动发现的 `APPEND_SYSTEM.md`。
    pub append_system_prompt: Option<Vec<String>>,
    /// 扩展结果覆盖。
    pub extensions_override: Option<ExtensionsOverride>,
    /// 技能结果覆盖。
    pub skills_override: Option<SkillsOverride>,
    /// prompt 结果覆盖。
    pub prompts_override: Option<PromptsOverride>,
    /// 主题结果覆盖。
    pub themes_override: Option<ThemesOverride>,
    /// 上下文文件覆盖。
    pub agents_files_override: Option<AgentsFilesOverride>,
    /// 系统提示覆盖。
    pub system_prompt_override: Option<SystemPromptOverride>,
    /// 追加系统提示覆盖。
    pub append_system_prompt_override: Option<AppendSystemPromptOverride>,
}

impl fmt::Debug for DefaultResourceLoaderOptions {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DefaultResourceLoaderOptions")
            .field("cwd", &self.cwd)
            .field("agent_dir", &self.agent_dir)
            .field(
                "additional_extension_paths",
                &self.additional_extension_paths,
            )
            .finish_non_exhaustive()
    }
}

/// 内置扩展的加载规格（`InlineExtension::Named` 去掉已消费的字段）。
#[derive(Clone)]
struct BuiltinExtensionSpec {
    factory: ExtensionFactory,
    // 构造时把 `Option<bool>` 归一化成 `bool`（仅 `Some(true)` 为真）；加载出的内置扩展会
    // 以 `Some(...)` 写回 `Extension::replaceable`。
    replaceable: bool,
}

/// reload 后对所有 getter 可见的状态。
///
/// `reload` 边算边写，任何一步失败时已写入的字段都保留，因此本结构可能停留在“部分更新”
/// 状态；所有字段的读写都由 [`DefaultResourceLoader`] 内部的 `RwLock` 保护。
#[derive(Clone)]
struct LoaderState {
    // ---- getter 结果：分步写入，失败时可能停在混合状态 ----
    extensions_result: LoadExtensionsResult,
    skills: Vec<Skill>,
    skill_diagnostics: Vec<ResourceDiagnostic>,
    prompts: Vec<PromptTemplate>,
    prompt_diagnostics: Vec<ResourceDiagnostic>,
    themes: Vec<Arc<kiana_theme::Theme>>,
    theme_diagnostics: Vec<ResourceDiagnostic>,
    agents_files: Vec<ContextFile>,
    system_prompt: Option<String>,
    // 只记录“来源字符串指向存在的路径”的情形；与文件能否读出、override 是否保留正文无关。
    system_prompt_source_path: Option<String>,
    append_system_prompt: Vec<String>,
    // 与 `system_prompt_source_path` 同规则，按来源列表顺序。
    append_system_prompt_source_paths: Vec<String>,
    // ---- 上次安装的完整路径列表：extend_resources 在其上追加，reload 第 21-23 步整体覆盖 ----
    last_skill_paths: Vec<String>,
    last_prompt_paths: Vec<String>,
    last_theme_paths: Vec<String>,
    // ---- extend_resources 注入的 路径→SourceInfo 表：reload 第 9 步清空 ----
    extension_skill_source_infos: IndexMap<String, SourceInfo>,
    extension_prompt_source_infos: IndexMap<String, SourceInfo>,
    extension_theme_source_infos: IndexMap<String, SourceInfo>,
    // 第 14 步写入的 path→metadata 快照；IndexMap 的插入序决定前缀匹配优先级。
    resource_metadata_by_path: IndexMap<String, PathMetadata>,
    // 是否完整走完过一次 reload（第 27 步置 true）；决定下次 reload 是否清扩展工厂缓存。
    loaded: bool,
}

impl LoaderState {
    fn empty() -> Self {
        LoaderState {
            extensions_result: empty_extensions_result(),
            skills: Vec::new(),
            skill_diagnostics: Vec::new(),
            prompts: Vec::new(),
            prompt_diagnostics: Vec::new(),
            themes: Vec::new(),
            theme_diagnostics: Vec::new(),
            agents_files: Vec::new(),
            system_prompt: None,
            system_prompt_source_path: None,
            append_system_prompt: Vec::new(),
            append_system_prompt_source_paths: Vec::new(),
            last_skill_paths: Vec::new(),
            last_prompt_paths: Vec::new(),
            last_theme_paths: Vec::new(),
            extension_skill_source_infos: IndexMap::new(),
            extension_prompt_source_infos: IndexMap::new(),
            extension_theme_source_infos: IndexMap::new(),
            resource_metadata_by_path: IndexMap::new(),
            loaded: false,
        }
    }
}

/// 建一个空的 `LoadExtensionsResult`（新 runtime；`warnings: None` 表示没有 warning 键，
/// 与 TS 的 `undefined` 对应）。
fn empty_extensions_result() -> LoadExtensionsResult {
    LoadExtensionsResult {
        extensions: Vec::new(),
        errors: Vec::new(),
        warnings: None,
        runtime: kiana_extensions::create_extension_runtime(),
    }
}

/// `ResourceLoader` 接口（TS 的 11 个方法）。TS: `resource-loader.ts#ResourceLoader`。
///
/// 所有 getter 同步；`extend_resources` / `reload` 通过内部 `RwLock` 修改状态（TS 是原地
/// 修改，语义相同）。`reload` 返回非 `Send` future（见模块文档）。
pub trait ResourceLoader {
    /// `getExtensions()`：返回最近一次 reload 安装的扩展集合（含 errors/warnings 与共享
    /// runtime）。未 reload 时是空集合加新建 runtime。
    fn get_extensions(&self) -> LoadExtensionsResult;
    /// `getSkills()`：返回技能与诊断的快照。
    fn get_skills(&self) -> SkillsResult;
    /// `getPrompts()`：返回 prompt 模板与诊断的快照。
    fn get_prompts(&self) -> PromptsResult;
    /// `getThemes()`：返回主题（`Arc` 共享）与诊断的快照。
    fn get_themes(&self) -> ThemesResult;
    /// `getAgentsFiles()`：返回上下文文件列表（全局 → 根祖先 → cwd）。
    fn get_agents_files(&self) -> AgentsFilesResult;
    /// `getSystemPrompt()`：返回已解析的系统提示正文。
    fn get_system_prompt(&self) -> Option<String>;
    /// `getSystemPromptSource()`：返回来源文件路径（仅当该路径存在）。
    fn get_system_prompt_source(&self) -> Option<SystemPromptSource>;
    /// `getAppendSystemPrompt()`：返回追加系统提示的正文列表。
    fn get_append_system_prompt(&self) -> Vec<String>;
    /// `getAppendSystemPromptSources()`：返回追加来源中实际存在的文件路径。
    fn get_append_system_prompt_sources(&self) -> Vec<SystemPromptSource>;
    /// `extendResources(paths)`：注入扩展来源并立即重跑对应加载。
    fn extend_resources(&self, paths: ResourceExtensionPaths) -> Result<(), ResourceLoaderError>;
    /// `reload(options?)`：完整重载（27 步）；错误与部分更新的语义见
    /// [`DefaultResourceLoader::reload`]。
    fn reload<'a>(
        &'a self,
        options: Option<ResourceLoaderReloadOptions>,
    ) -> LocalBoxFuture<'a, Result<(), ResourceLoaderError>>;
}

/// 资源加载器实现。TS: `resource-loader.ts#DefaultResourceLoader`。
///
/// 构造参数在 `new` 之后不变，全部可查询的资源放在 `state` 里由 `RwLock` 保护。类型含有
/// 非 `Send`/`Sync` 的 `ExtensionFactory`（`Arc<dyn Fn ... -> LocalBoxFuture>`），因此整个
/// 对象不能跨线程移动；加上扩展工厂缓存是 thread-local，创建、reload 与查询应在同一线程
/// 完成（调用方在 `current_thread` runtime / `LocalSet` 上构造并驱动它；`kiana-coding-agent`
/// 的 `main.rs` 用 `Builder::new_current_thread` + `LocalSet::block_on` 启动，`kiana-session`
/// 自身不创建 runtime，只在宿主的 `LocalSet` 上驱动）。
pub struct DefaultResourceLoader {
    cwd: String,
    agent_dir: String,
    settings_manager: SettingsManager,
    event_bus: Arc<dyn EventBus>,
    package_manager: DefaultPackageManager,
    additional_extension_paths: Vec<String>,
    additional_skill_paths: Vec<String>,
    additional_prompt_template_paths: Vec<String>,
    additional_theme_paths: Vec<String>,
    extension_factories: Vec<InlineExtension>,
    builtin_extensions: IndexMap<String, BuiltinExtensionSpec>,
    no_extensions: bool,
    disabled_builtin_extensions: HashSet<String>,
    no_skills: bool,
    no_prompt_templates: bool,
    no_themes: bool,
    no_context_files: bool,
    system_prompt_source: Option<String>,
    append_system_prompt_source: Option<Vec<String>>,
    extensions_override: Option<ExtensionsOverride>,
    skills_override: Option<SkillsOverride>,
    prompts_override: Option<PromptsOverride>,
    themes_override: Option<ThemesOverride>,
    agents_files_override: Option<AgentsFilesOverride>,
    system_prompt_override: Option<SystemPromptOverride>,
    append_system_prompt_override: Option<AppendSystemPromptOverride>,
    // 所有 getter 可见的可变数据。构造后只由 reload / extend_resources 写入，且每次都在
    // 锁内只做字段读写；不要在持锁期间调用用户回调。
    state: RwLock<LoaderState>,
}

impl fmt::Debug for DefaultResourceLoader {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DefaultResourceLoader")
            .field("cwd", &self.cwd)
            .field("agent_dir", &self.agent_dir)
            .finish_non_exhaustive()
    }
}

/// `loadExtensionFactories` 的返回值。
struct InlineExtensionsResult {
    extensions: Vec<Extension>,
    errors: Vec<LoadError>,
}

// 锁中毒（持锁线程 panic）时仍取回数据：loader 没有可用的“重启”路径，向上传播 panic 会让
// 所有后续 getter 永久失败。代价是 panic 发生在多次字段写入之间时，读到的可能是半更新状态。
fn read_lock(state: &RwLock<LoaderState>) -> RwLockReadGuard<'_, LoaderState> {
    state
        .read()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn write_lock(state: &RwLock<LoaderState>) -> RwLockWriteGuard<'_, LoaderState> {
    state
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

impl DefaultResourceLoader {
    /// `new DefaultResourceLoader(options)`。
    ///
    /// 解析 `cwd` / `agent_dir`（以进程当前目录为基准），补齐缺省的 `SettingsManager` 与
    /// `EventBus`，并把 `extension_factories` 中 `builtin: true` 的具名项移入独立表。构造后
    /// 资源状态为空且 `loaded = false`，不加载任何资源；在第一次 reload 之前 getter 返回空
    /// 结果。
    ///
    /// 注意：`options.settings_manager` 为 `None` 时，缺省创建的 `SettingsManager::create`
    /// 会读取磁盘上的 global / project settings.json（`FileSettingsStorage::with_lock` →
    /// `read_utf8_lossy`），这是本函数里唯一的真实 IO；除此之外不做任何资源加载。
    ///
    /// `extension_factories` 中只有 `builtin: true` 的具名项被移入内置表（同名后者覆盖前者）；
    /// 其余项（包括 `builtin: false` 或未指定的具名项）按普通内联工厂处理。
    pub fn new(options: DefaultResourceLoaderOptions) -> Self {
        let process_cwd = source_info_map::process_cwd();
        let cwd = resolve_path(&options.cwd, &process_cwd, &PathInputOptions::default());
        let agent_dir = resolve_path(
            &options.agent_dir,
            &process_cwd,
            &PathInputOptions::default(),
        );
        let settings_manager = options
            .settings_manager
            .unwrap_or_else(|| SettingsManager::create(&cwd, &agent_dir));
        let event_bus = options
            .event_bus
            .unwrap_or_else(|| create_event_bus() as Arc<dyn EventBus>);

        let mut extension_factories: Vec<InlineExtension> = Vec::new();
        let mut builtin_extensions: IndexMap<String, BuiltinExtensionSpec> = IndexMap::new();
        for input in options.extension_factories {
            if warnings::is_builtin_extension(&input) {
                if let InlineExtension::Named {
                    name,
                    factory,
                    replaceable,
                    ..
                } = input
                {
                    builtin_extensions.insert(
                        name,
                        BuiltinExtensionSpec {
                            factory,
                            replaceable: replaceable == Some(true),
                        },
                    );
                }
            } else {
                extension_factories.push(input);
            }
        }

        let package_manager = DefaultPackageManager::new(PackageManagerOptions {
            cwd: cwd.clone(),
            agent_dir: agent_dir.clone(),
            settings_manager: settings_manager.clone(),
            builtin_extensions: builtin_extensions.keys().cloned().collect(),
        });

        DefaultResourceLoader {
            cwd,
            agent_dir,
            settings_manager,
            event_bus,
            package_manager,
            additional_extension_paths: options.additional_extension_paths,
            additional_skill_paths: options.additional_skill_paths,
            additional_prompt_template_paths: options.additional_prompt_template_paths,
            additional_theme_paths: options.additional_theme_paths,
            extension_factories,
            builtin_extensions,
            no_extensions: options.no_extensions,
            disabled_builtin_extensions: options.disabled_builtin_extensions.into_iter().collect(),
            no_skills: options.no_skills,
            no_prompt_templates: options.no_prompt_templates,
            no_themes: options.no_themes,
            no_context_files: options.no_context_files,
            system_prompt_source: options.system_prompt,
            append_system_prompt_source: options.append_system_prompt,
            extensions_override: options.extensions_override,
            skills_override: options.skills_override,
            prompts_override: options.prompts_override,
            themes_override: options.themes_override,
            agents_files_override: options.agents_files_override,
            system_prompt_override: options.system_prompt_override,
            append_system_prompt_override: options.append_system_prompt_override,
            state: RwLock::new(LoaderState::empty()),
        }
    }

    /// 项目工作目录（已解析）。
    pub fn cwd(&self) -> &str {
        &self.cwd
    }

    /// agent 配置目录（已解析）。
    pub fn agent_dir(&self) -> &str {
        &self.agent_dir
    }

    /// `getExtensions()`。
    ///
    /// 返回快照（克隆）；扩展本身共享同一个 `runtime`。未 reload 时是构造时的空结果，
    /// 其中 runtime 尚未被 runner 初始化。
    pub fn get_extensions(&self) -> LoadExtensionsResult {
        read_lock(&self.state).extensions_result.clone()
    }

    /// `getSkills()`。
    ///
    /// 返回最近一次 reload 或 [`DefaultResourceLoader::extend_resources`] 安装的技能与诊断。
    pub fn get_skills(&self) -> SkillsResult {
        let state = read_lock(&self.state);
        SkillsResult {
            skills: state.skills.clone(),
            diagnostics: state.skill_diagnostics.clone(),
        }
    }

    /// `getPrompts()`。
    ///
    /// 返回最近一次 reload 或 extensions 注入后安装的模板与诊断（自动加载阶段同名已去重，
    /// override 返回的内容不再去重）。
    pub fn get_prompts(&self) -> PromptsResult {
        let state = read_lock(&self.state);
        PromptsResult {
            prompts: state.prompts.clone(),
            diagnostics: state.prompt_diagnostics.clone(),
        }
    }

    /// `getThemes()`。
    ///
    /// 返回最近一次 reload 或 extensions 注入后安装的主题与诊断；主题是 `Arc`，调用方与
    /// loader 共享同一实例。
    pub fn get_themes(&self) -> ThemesResult {
        let state = read_lock(&self.state);
        ThemesResult {
            themes: state.themes.clone(),
            diagnostics: state.theme_diagnostics.clone(),
        }
    }

    /// `getAgentsFiles()`。
    ///
    /// 返回快照；未 reload 或设置了 `no_context_files` 时为空列表。
    pub fn get_agents_files(&self) -> AgentsFilesResult {
        AgentsFilesResult {
            agents_files: read_lock(&self.state).agents_files.clone(),
        }
    }

    /// `getSystemPrompt()`。
    ///
    /// 返回解析后的正文（来源是文件时已读入内容；读取失败时是路径原文）。未 reload 或
    /// `systemPromptOverride` 返回 `None` 时为 `None`。
    pub fn get_system_prompt(&self) -> Option<String> {
        read_lock(&self.state).system_prompt.clone()
    }

    /// `getSystemPromptSource()`。
    ///
    /// 只反映来源字符串指向一个**存在路径**的情形；字面文本（以及指向不存在路径的字符串）
    /// 返回 `None`（见 [`SystemPromptSource`]）。判断只用 `Path::exists()`，与文件是否可读、
    /// override 是否保留了正文无关。
    pub fn get_system_prompt_source(&self) -> Option<SystemPromptSource> {
        read_lock(&self.state)
            .system_prompt_source_path
            .as_ref()
            .map(|path| SystemPromptSource { path: path.clone() })
    }

    /// `getAppendSystemPrompt()`。
    ///
    /// 返回各来源解析后的正文列表。空串来源在 reload 时被丢弃；不存在的路径会按字面原文
    /// 保留。返回值是 override 的最终结果，可能包含 override 新加的内容。
    pub fn get_append_system_prompt(&self) -> Vec<String> {
        read_lock(&self.state).append_system_prompt.clone()
    }

    /// `getAppendSystemPromptSources()`。
    ///
    /// 只包含实际存在的文件来源，顺序与追加来源列表一致；字面文本项不出现。判断同样只基于
    /// `Path::exists()`：文件存在但读取失败仍会出现在这里。
    pub fn get_append_system_prompt_sources(&self) -> Vec<SystemPromptSource> {
        read_lock(&self.state)
            .append_system_prompt_source_paths
            .iter()
            .map(|path| SystemPromptSource { path: path.clone() })
            .collect()
    }

    /// `extendResources(paths)`：注册扩展来源并立即重跑对应资源的加载。
    /// TS: `resource-loader.ts#extendResources`。
    ///
    /// 在 `reload()` 之前调用也有效——此时 `resourceMetadataByPath` 为空。
    ///
    /// 处理顺序固定为技能 → prompt → 主题；只有非空的列表才重跑，空列表保留现有结果。路径先经
    /// `normalize_extension_paths` 解析（相对 cwd；非空 `base_dir` 也解析），再并入
    /// `last_*_paths`（`merge_paths` 去重、保留首次出现）。
    ///
    /// 锁只覆盖来源表与路径的更新；会调用用户 override 的 `update_*_from_paths` 在锁外执行，
    /// 因此 override 里可以重入 getter 或 `extend_resources`（测试
    /// `extend_resources_overrides_may_call_getters_without_deadlocking`）。
    ///
    /// 错误只来自来源信息回填（`stat` 失败）。此时已写入的来源表与 `last_*_paths` 不回滚，
    /// 对应资源字段保留旧值；调用方 `kiana-session` 只在 `is_ok()` 时刷新系统提示。
    /// 三类资源按技能 → prompt → 主题串行更新；某一步失败会立即 `?` 返回，后面的类型不再
    /// 更新，但此前已并入的路径仍然保留。
    pub fn extend_resources(
        &self,
        paths: ResourceExtensionPaths,
    ) -> Result<(), ResourceLoaderError> {
        let skill_paths = self.normalize_extension_paths(&paths.skill_paths);
        let prompt_paths = self.normalize_extension_paths(&paths.prompt_paths);
        let theme_paths = self.normalize_extension_paths(&paths.theme_paths);

        // TS 单线程、无锁：override 回调里调用 getter / extendResources 都能继续。
        // 因此只在读写状态时短暂持锁，`update*FromPaths`（会调用用户回调）在锁外执行。
        let mut metadata = IndexMap::new();
        let mut skill_job = None;
        let mut prompt_job = None;
        let mut theme_job = None;
        {
            let mut state = write_lock(&self.state);
            for entry in &skill_paths {
                state.extension_skill_source_infos.insert(
                    entry.path.clone(),
                    create_source_info(&entry.path, &entry.metadata),
                );
            }
            for entry in &prompt_paths {
                state.extension_prompt_source_infos.insert(
                    entry.path.clone(),
                    create_source_info(&entry.path, &entry.metadata),
                );
            }
            for entry in &theme_paths {
                state.extension_theme_source_infos.insert(
                    entry.path.clone(),
                    create_source_info(&entry.path, &entry.metadata),
                );
            }

            if !skill_paths.is_empty() || !prompt_paths.is_empty() || !theme_paths.is_empty() {
                metadata = state.resource_metadata_by_path.clone();
            }
            if !skill_paths.is_empty() {
                let new_paths: Vec<String> =
                    skill_paths.iter().map(|entry| entry.path.clone()).collect();
                state.last_skill_paths =
                    source_info_map::merge_paths(&self.cwd, &state.last_skill_paths, &new_paths);
                skill_job = Some((
                    state.last_skill_paths.clone(),
                    state.extension_skill_source_infos.clone(),
                ));
            }
            if !prompt_paths.is_empty() {
                let new_paths: Vec<String> = prompt_paths
                    .iter()
                    .map(|entry| entry.path.clone())
                    .collect();
                state.last_prompt_paths =
                    source_info_map::merge_paths(&self.cwd, &state.last_prompt_paths, &new_paths);
                prompt_job = Some((
                    state.last_prompt_paths.clone(),
                    state.extension_prompt_source_infos.clone(),
                ));
            }
            if !theme_paths.is_empty() {
                let new_paths: Vec<String> =
                    theme_paths.iter().map(|entry| entry.path.clone()).collect();
                state.last_theme_paths =
                    source_info_map::merge_paths(&self.cwd, &state.last_theme_paths, &new_paths);
                theme_job = Some((
                    state.last_theme_paths.clone(),
                    state.extension_theme_source_infos.clone(),
                ));
            }
        }

        if let Some((paths, source_infos)) = skill_job {
            let result = self.update_skills_from_paths(&paths, Some(&metadata), &source_infos)?;
            let mut state = write_lock(&self.state);
            state.skills = result.skills;
            state.skill_diagnostics = result.diagnostics;
        }
        if let Some((paths, source_infos)) = prompt_job {
            let result = self.update_prompts_from_paths(&paths, Some(&metadata), &source_infos)?;
            let mut state = write_lock(&self.state);
            state.prompts = result.prompts;
            state.prompt_diagnostics = result.diagnostics;
        }
        if let Some((paths, source_infos)) = theme_job {
            let result = self.update_themes_from_paths(&paths, Some(&metadata), &source_infos)?;
            let mut state = write_lock(&self.state);
            state.themes = result.themes;
            state.theme_diagnostics = result.diagnostics;
        }
        Ok(())
    }

    /// 预信任引导：强制未受信的项目设置，加载用户/全局与临时 CLI 扩展。
    /// TS: `resource-loader.ts#loadProjectTrustExtensions`。
    ///
    /// 固定顺序：`set_project_trusted(false)`（丢弃内存中的项目设置）→ `settings.reload()`
    /// → `load_current_extension_set(true)`。结果不含项目资源与 `builtin:`。`reload` 会把它
    /// 交给 `resolveProjectTrust` 回调，并在最终加载时复用其中的用户扩展，避免模块体执行两次。
    ///
    /// 单独调用会改变 settings 管理器内存中的项目信任状态，但不会持久化。
    ///
    /// 本函数不读写 loader 的 `state`；返回的集合由调用方（`reload` 第 3 步）交给
    /// `resolveProjectTrust` 回调，并在第 16 步复用。
    pub async fn load_project_trust_extensions(
        &self,
    ) -> Result<LoadExtensionsResult, ResourceLoaderError> {
        self.settings_manager.set_project_trusted(false);
        self.settings_manager.reload().await;
        self.load_current_extension_set(true).await
    }

    /// `reload(options?)` —— 完整 27 步（`spec/ca-resources.md` §2.11.11）。
    ///
    /// 三个阶段：准备（1-7）、装扩展（8-20）、装其余资源（21-27）。步骤串行；单条扩展加载
    /// 失败只进 `LoadExtensionsResult.errors`，而包解析、扩展包清单读取与来源信息 `stat`
    /// 失败会让本函数以 `Err` 提前返回。返回 `Err` 时**不回滚**已写回的字段（见模块文档
    /// “失败模式”）。
    ///
    /// 没有取消参数：future 被 drop 时状态停留在最后一次写回处，下一次 reload 会从头重跑。
    #[allow(clippy::too_many_lines)]
    pub async fn reload(
        &self,
        options: Option<ResourceLoaderReloadOptions>,
    ) -> Result<(), ResourceLoaderError> {
        // 1. resetTimings("extensions")
        reset_timings(TimingLabel::Extensions);

        // 2. 已加载过才清扩展工厂缓存。首次加载前没有可清的状态；缓存是 thread-local，
        //    清空同时递增 generation 让旧 token 失效，使本轮重新求值扩展模块体。
        let loaded = read_lock(&self.state).loaded;
        if loaded {
            kiana_extensions::loader::factory_cache::clear_extension_cache();
        }

        // 3. 预信任引导。结果保留给第 16 步：最终集合复用其中的用户扩展与 runtime，
        //    避免同一模块体执行两次（用户扩展的模块级状态因此保持不变）。
        let mut pre_trust_extensions: Option<LoadExtensionsResult> = None;
        if let Some(options) = &options
            && let Some(resolve_project_trust) = &options.resolve_project_trust
        {
            let pre_trust = self.load_project_trust_extensions().await?;
            let trusted = resolve_project_trust(pre_trust.clone()).await;
            self.settings_manager.set_project_trusted(trusted);
            pre_trust_extensions = Some(pre_trust);
        }

        // 4. reload() 保留 projectTrusted。
        self.settings_manager.reload().await;

        // 5. packageManager.resolve()（无 onMissing）。
        let resolved_paths = self.package_manager.resolve(None).await?;

        // 6. CLI 临时扩展源。
        let cli_extension_paths = self
            .package_manager
            .resolve_extension_sources(
                &self.additional_extension_paths,
                ResolveExtensionSourcesOptions {
                    local: false,
                    temporary: true,
                },
            )
            .await?;

        // 7. 安装失败警告。
        let install_warnings = warnings::collect_package_install_warnings(&[
            resolved_paths.install_failures.as_deref(),
            cli_extension_paths.install_failures.as_deref(),
        ]);

        // 8. 重置 metadata 表（保留在实例上供 extendResources 使用）。
        // 9. 重置三张扩展来源表。
        //
        // TS 原地替换 `this.xxx`：reload 后续失败时实例上留下的是这两步之后的状态
        // （reset 后的表；第 16 步 await 期间并发 `extendResources` 的插入会保留）。
        {
            let mut state = write_lock(&self.state);
            state.resource_metadata_by_path = IndexMap::new();
            state.extension_skill_source_infos = IndexMap::new();
            state.extension_prompt_source_infos = IndexMap::new();
            state.extension_theme_source_infos = IndexMap::new();
        }
        let mut metadata_by_path: IndexMap<String, PathMetadata> = IndexMap::new();

        // 10. 先记录全部（含 disabled）path→metadata，再过滤 enabled。metadata 表按路径
        //     记录且保留首次写入：同一路径可能在另一个来源里是 enabled，过滤后再记录会漏。
        //     插入顺序同时是后续前缀匹配的优先级（见模块文档“状态结构与顺序敏感点”）。
        let enabled_extensions =
            get_enabled_paths(&resolved_paths.extensions, &mut metadata_by_path);
        let enabled_skill_resources =
            get_enabled_resources(&resolved_paths.skills, &mut metadata_by_path);
        let enabled_prompts = get_enabled_paths(&resolved_paths.prompts, &mut metadata_by_path);
        let enabled_themes = get_enabled_paths(&resolved_paths.themes, &mut metadata_by_path);

        // 11. 目录型技能（auto / package 来源）展开成 SKILL.md：仅当路径本身是目录且目录下
        //     存在 SKILL.md 时替换，否则保留原路径。
        let enabled_skills: Vec<String> = enabled_skill_resources
            .iter()
            .map(|resource| self.map_skill_path(resource, &mut metadata_by_path))
            .collect();

        // 12. CLI 扩展/技能路径的 metadata `{source:"cli", scope:"temporary"}`。
        for resource in &cli_extension_paths.extensions {
            if !metadata_by_path.contains_key(&resource.path) {
                metadata_by_path.insert(
                    resource.path.clone(),
                    PathMetadata {
                        source: "cli".to_string(),
                        scope: SourceScope::Temporary,
                        origin: SourceOrigin::TopLevel,
                        base_dir: None,
                        package_root: None,
                    },
                );
            }
        }
        for resource in &cli_extension_paths.skills {
            if !metadata_by_path.contains_key(&resource.path) {
                metadata_by_path.insert(
                    resource.path.clone(),
                    PathMetadata {
                        source: "cli".to_string(),
                        scope: SourceScope::Temporary,
                        origin: SourceOrigin::TopLevel,
                        base_dir: None,
                        package_root: None,
                    },
                );
            }
        }

        // 13. CLI 四类资源的 enabled 路径（同时补记 prompts/themes 的 metadata）。
        let cli_enabled_extensions =
            get_enabled_paths(&cli_extension_paths.extensions, &mut metadata_by_path);
        let cli_enabled_skills =
            get_enabled_paths(&cli_extension_paths.skills, &mut metadata_by_path);
        let cli_enabled_prompts =
            get_enabled_paths(&cli_extension_paths.prompts, &mut metadata_by_path);
        let cli_enabled_themes =
            get_enabled_paths(&cli_extension_paths.themes, &mut metadata_by_path);

        // 14. 合并并对**最终集合**过滤 disabled 内置扩展。合并顺序：CLI 临时源在前、发现到的
        //     扩展在后（`merge_paths` 保留首次出现）；`no_extensions` 时只保留 CLI 源。
        //     过滤作用在合并后的集合上，所以 `-e builtin:x` 也绕不过 disabled 列表。
        let extension_paths: Vec<String> = if self.no_extensions {
            cli_enabled_extensions
        } else {
            source_info_map::merge_paths(&self.cwd, &cli_enabled_extensions, &enabled_extensions)
        }
        .into_iter()
        .filter(|path| {
            !path.starts_with(BUILTIN_PATH_PREFIX)
                || !self
                    .disabled_builtin_extensions
                    .contains(&path[BUILTIN_PATH_PREFIX.len()..])
        })
        .collect();

        // TS 的 `metadataByPath` 是实例上的同一个 Map：第 16 步 await 期间并发调用
        // `extendResources` 会看到已填充好的表。必须在 await 之前同步到实例。
        write_lock(&self.state).resource_metadata_by_path = metadata_by_path.clone();

        // 15. 扩展包清单警告。
        let package_warnings =
            warnings::collect_extension_package_warnings(&extension_paths, &metadata_by_path)?;

        // 16. 最终扩展集合。预信任结果里已加载的用户扩展直接复用、已失败的路径不重试；
        //     最终顺序仍按 extension_paths（见 load_final_extension_set）。
        let mut extensions_result = self
            .load_final_extension_set(&extension_paths, pre_trust_extensions.as_ref())
            .await?;

        // 17. 合并安装/包清单警告。
        let mut all_warnings = install_warnings;
        all_warnings.extend(package_warnings);
        warnings::merge_extension_warnings(&mut extensions_result, all_warnings);

        // 18. additionalExtensionPaths 里不存在的本地路径（error）。
        for path in &self.additional_extension_paths {
            if !is_local_path(path) {
                continue;
            }
            let resolved = self.resolve_resource_path(path);
            if !Path::new(&resolved).exists() {
                extensions_result.errors.push(LoadError {
                    path: resolved.clone(),
                    error: format!("Extension path does not exist: {resolved}"),
                });
            }
        }

        // 19. extensionsOverride。
        let extensions_result = match &self.extensions_override {
            Some(extensions_override) => extensions_override(extensions_result),
            None => extensions_result,
        };
        // TS 在第 20 步之前就执行 `this.extensionsResult = ...`；apply 失败时保留新结果。
        write_lock(&self.state).extensions_result = extensions_result;
        // override 可能重入 `extendResources` 往实例的 metadata 表里插入条目；TS 用的是同一张表。
        metadata_by_path = read_lock(&self.state).resource_metadata_by_path.clone();

        // 20. 应用扩展来源信息（直接写在实例上，失败时保留 TS 的部分修改）。
        {
            let mut state = write_lock(&self.state);
            self.apply_extension_source_info(
                &mut state.extensions_result.extensions,
                &metadata_by_path,
            )?;
        }

        // 21. 技能路径 + additional 技能路径诊断。优先级：CLI 临时路径 → 发现路径 →
        //     additional_*（`merge_paths` 保留首次出现）；`no_skills` 时丢弃发现路径，
        //     但 CLI 与 additional 仍生效。
        let skill_paths = if self.no_skills {
            source_info_map::merge_paths(
                &self.cwd,
                &cli_enabled_skills,
                &self.additional_skill_paths,
            )
        } else {
            let mut combined = cli_enabled_skills;
            combined.extend(enabled_skills);
            source_info_map::merge_paths(&self.cwd, &combined, &self.additional_skill_paths)
        };
        // TS 在 updateSkillsFromPaths 之前就赋值 `this.lastSkillPaths`。
        write_lock(&self.state).last_skill_paths = skill_paths.clone();
        // 第 9 步重置过的来源表；第 16 步 await / 第 19 步 override 期间 extendResources 的插入要保留。
        let extension_skill_source_infos =
            read_lock(&self.state).extension_skill_source_infos.clone();
        let skills_result = self.update_skills_from_paths(
            &skill_paths,
            Some(&metadata_by_path),
            &extension_skill_source_infos,
        )?;
        let mut skill_diagnostics = skills_result.diagnostics;
        for path in &self.additional_skill_paths {
            if !is_local_path(path) {
                continue;
            }
            let resolved = self.resolve_resource_path(path);
            if !Path::new(&resolved).exists()
                && !skill_diagnostics
                    .iter()
                    .any(|diagnostic| diagnostic.path.as_deref() == Some(resolved.as_str()))
            {
                skill_diagnostics.push(ResourceDiagnostic {
                    kind: DiagnosticKind::Error,
                    message: "Skill path does not exist".to_string(),
                    path: Some(resolved),
                    collision: None,
                });
            }
        }
        {
            let mut state = write_lock(&self.state);
            state.skills = skills_result.skills;
            state.skill_diagnostics = skill_diagnostics;
        }

        // 22. prompt 模板路径 + additional 路径诊断。路径合并规则与第 21 步一致；缺少的
        //     additional 本地路径追加一条 `error` 诊断（已有同路径诊断时不重复追加）。
        let prompt_paths = if self.no_prompt_templates {
            source_info_map::merge_paths(
                &self.cwd,
                &cli_enabled_prompts,
                &self.additional_prompt_template_paths,
            )
        } else {
            let mut combined = cli_enabled_prompts;
            combined.extend(enabled_prompts);
            source_info_map::merge_paths(
                &self.cwd,
                &combined,
                &self.additional_prompt_template_paths,
            )
        };
        write_lock(&self.state).last_prompt_paths = prompt_paths.clone();
        let extension_prompt_source_infos =
            read_lock(&self.state).extension_prompt_source_infos.clone();
        let prompts_result = self.update_prompts_from_paths(
            &prompt_paths,
            Some(&metadata_by_path),
            &extension_prompt_source_infos,
        )?;
        let mut prompt_diagnostics = prompts_result.diagnostics;
        for path in &self.additional_prompt_template_paths {
            if !is_local_path(path) {
                continue;
            }
            let resolved = self.resolve_resource_path(path);
            if !Path::new(&resolved).exists()
                && !prompt_diagnostics
                    .iter()
                    .any(|diagnostic| diagnostic.path.as_deref() == Some(resolved.as_str()))
            {
                prompt_diagnostics.push(ResourceDiagnostic {
                    kind: DiagnosticKind::Error,
                    message: "Prompt template path does not exist".to_string(),
                    path: Some(resolved),
                    collision: None,
                });
            }
        }
        {
            let mut state = write_lock(&self.state);
            state.prompts = prompts_result.prompts;
            state.prompt_diagnostics = prompt_diagnostics;
        }

        // 23. 主题路径 + additional 主题路径诊断（**不**检查 isLocalPath）。路径合并规则与
        //     第 21 步一致；主题加载器自身的诊断是 warning，这里补的是 error。
        let theme_paths = if self.no_themes {
            source_info_map::merge_paths(
                &self.cwd,
                &cli_enabled_themes,
                &self.additional_theme_paths,
            )
        } else {
            let mut combined = cli_enabled_themes;
            combined.extend(enabled_themes);
            source_info_map::merge_paths(&self.cwd, &combined, &self.additional_theme_paths)
        };
        write_lock(&self.state).last_theme_paths = theme_paths.clone();
        let extension_theme_source_infos =
            read_lock(&self.state).extension_theme_source_infos.clone();
        let themes_result = self.update_themes_from_paths(
            &theme_paths,
            Some(&metadata_by_path),
            &extension_theme_source_infos,
        )?;
        let mut theme_diagnostics = themes_result.diagnostics;
        for path in &self.additional_theme_paths {
            let resolved = self.resolve_resource_path(path);
            if !Path::new(&resolved).exists()
                && !theme_diagnostics
                    .iter()
                    .any(|diagnostic| diagnostic.path.as_deref() == Some(resolved.as_str()))
            {
                theme_diagnostics.push(ResourceDiagnostic {
                    kind: DiagnosticKind::Error,
                    message: "Theme path does not exist".to_string(),
                    path: Some(resolved),
                    collision: None,
                });
            }
        }
        {
            let mut state = write_lock(&self.state);
            state.themes = themes_result.themes;
            state.theme_diagnostics = theme_diagnostics;
        }

        // 24. 上下文文件。顺序由 load_project_context_files 固定：全局 → 根祖先 → cwd
        //     （最近的最后）；`no_context_files` 时跳过发现，但 override 仍会被调用。
        let agents_files = AgentsFilesResult {
            agents_files: if self.no_context_files {
                Vec::new()
            } else {
                context_files::load_project_context_files(&self.cwd, &self.agent_dir)
            },
        };
        let agents_files = match &self.agents_files_override {
            Some(agents_files_override) => agents_files_override(agents_files),
            None => agents_files,
        };
        write_lock(&self.state).agents_files = agents_files.agents_files;

        // 25. 系统提示。来源优先级：显式 `system_prompt` → 自动发现（项目 `.kiana/SYSTEM.md`
        //     仅在项目受信时 → 全局 `agentDir/SYSTEM.md`）。`resolve_prompt_input` 只对存在的
        //     文件读内容，其余按字面文本；source 只记录实际存在的文件。
        let system_prompt_source = self
            .system_prompt_source
            .clone()
            .or_else(|| self.discover_system_prompt_file());
        let base_system_prompt =
            resolve_prompt_input(system_prompt_source.as_deref(), "system prompt");
        let system_prompt = match &self.system_prompt_override {
            Some(system_prompt_override) => system_prompt_override(base_system_prompt),
            None => base_system_prompt,
        };
        let system_prompt_source_path = match &system_prompt_source {
            Some(source) if Path::new(source).exists() => {
                Some(source_info_map::resolve_path_from_process_cwd(source))
            }
            _ => None,
        };
        {
            let mut state = write_lock(&self.state);
            state.system_prompt = system_prompt;
            state.system_prompt_source_path = system_prompt_source_path;
        }

        // 26. 追加系统提示。显式列表完全替代自动发现；每项按文件/字面文本解析，解析为空的
        //     项被丢弃；source 列表只保留实际存在的文件。
        let append_sources = match &self.append_system_prompt_source {
            Some(sources) => sources.clone(),
            None => match self.discover_append_system_prompt_file() {
                Some(discovered) => vec![discovered],
                None => Vec::new(),
            },
        };
        let base_append: Vec<String> = append_sources
            .iter()
            .filter_map(|source| resolve_prompt_input(Some(source), "append system prompt"))
            .collect();
        let append_system_prompt = match &self.append_system_prompt_override {
            Some(append_system_prompt_override) => append_system_prompt_override(base_append),
            None => base_append,
        };
        let append_system_prompt_source_paths: Vec<String> = append_sources
            .iter()
            .filter(|source| Path::new(source).exists())
            .map(|source| source_info_map::resolve_path_from_process_cwd(source))
            .collect();

        {
            let mut state = write_lock(&self.state);
            state.append_system_prompt = append_system_prompt;
            state.append_system_prompt_source_paths = append_system_prompt_source_paths;
        }

        // 27. `this.loaded = true`（其余字段已在对应步骤原地赋值）。
        write_lock(&self.state).loaded = true;
        Ok(())
    }

    /// `loadCurrentExtensionSet({ includeInlineFactories })`：预信任引导用。
    /// TS: `resource-loader.ts#loadCurrentExtensionSet`。
    ///
    /// 只处理扩展：解析 settings 包与 CLI 临时源，合并路径并**丢弃全部 `builtin:` 路径**
    /// （内置扩展要等项目信任确定后由最终加载处理），再合并安装失败与包清单警告。
    /// `metadata_by_path` 是局部表：resolved 与 CLI 的所有资源（含 disabled）按遍历顺序写入，
    /// 同一路径后者覆盖前者。`include_inline_factories` 为 true 时加载普通内联工厂并省略
    /// 被替换的 replaceable 扩展。
    ///
    /// 本函数不读写 loader 的 `state`：metadata 表是局部变量，因此预信任阶段不会污染实例上
    /// 已安装的扩展/来源信息。
    async fn load_current_extension_set(
        &self,
        include_inline_factories: bool,
    ) -> Result<LoadExtensionsResult, ResourceLoaderError> {
        let resolved_paths = self.package_manager.resolve(None).await?;
        let cli_extension_paths = self
            .package_manager
            .resolve_extension_sources(
                &self.additional_extension_paths,
                ResolveExtensionSourcesOptions {
                    local: false,
                    temporary: true,
                },
            )
            .await?;
        let enabled_extensions: Vec<String> = resolved_paths
            .extensions
            .iter()
            .filter(|resource| resource.enabled)
            .map(|resource| resource.path.clone())
            .collect();
        let cli_enabled_extensions: Vec<String> = cli_extension_paths
            .extensions
            .iter()
            .filter(|resource| resource.enabled)
            .map(|resource| resource.path.clone())
            .collect();
        // 内置扩展等最终一轮：项目设置可能禁用它，而加载过的扩展无法卸载。
        let extension_paths: Vec<String> = if self.no_extensions {
            cli_enabled_extensions
        } else {
            source_info_map::merge_paths(&self.cwd, &cli_enabled_extensions, &enabled_extensions)
        }
        .into_iter()
        .filter(|path| !path.starts_with(BUILTIN_PATH_PREFIX))
        .collect();

        let mut metadata_by_path: IndexMap<String, PathMetadata> = IndexMap::new();
        for resource in resolved_paths
            .extensions
            .iter()
            .chain(cli_extension_paths.extensions.iter())
        {
            metadata_by_path.insert(resource.path.clone(), resource.metadata.clone());
        }
        let package_warnings =
            warnings::collect_extension_package_warnings(&extension_paths, &metadata_by_path)?;
        let install_warnings = warnings::collect_package_install_warnings(&[
            resolved_paths.install_failures.as_deref(),
            cli_extension_paths.install_failures.as_deref(),
        ]);
        let mut extensions_result = kiana_extensions::load_extensions_cached(
            &extension_paths,
            &self.cwd,
            Some(Arc::clone(&self.event_bus)),
            None,
        )
        .await;
        let mut merged_warnings = install_warnings;
        merged_warnings.extend(package_warnings);
        warnings::merge_extension_warnings(&mut extensions_result, merged_warnings);
        if !include_inline_factories {
            return Ok(extensions_result);
        }

        let inline_extensions = self
            .load_extension_factories(&extensions_result.runtime)
            .await;
        extensions_result
            .extensions
            .extend(inline_extensions.extensions);
        extensions_result.errors.extend(inline_extensions.errors);
        let mut replacement_warnings: Vec<LoadWarning> = Vec::new();
        extensions_result.extensions = warnings::omit_replaced_extensions(
            extensions_result.extensions,
            &mut replacement_warnings,
        );
        warnings::merge_extension_warnings(&mut extensions_result, replacement_warnings);
        Ok(extensions_result)
    }

    /// `loadExtensionPaths(paths, runtime?)`：文件扩展 + `builtin:<name>`。
    /// TS: `resource-loader.ts#loadExtensionPaths`。
    ///
    /// 非 `builtin:` 路径一次性交给 `load_extensions_cached`（共享 thread-local 工厂缓存）；
    /// `builtin:` 路径按去前缀后的名字查构造时的表，逐个调用
    /// `load_extension_from_factory`。未知名字记 `Unknown built-in extension: <path>`；成功的
    /// 内置扩展强制 `hidden = Some(true)` 并带上构造时的 `replaceable`。单条失败只进
    /// `errors`，函数本身不会返回 `Err`。
    ///
    /// `runtime` 为 `Some` 时文件扩展与内置扩展共用它；为 `None` 时由
    /// `load_extensions_cached` 新建，内置扩展再复用结果里的 runtime。
    async fn load_extension_paths(
        &self,
        paths: &[String],
        runtime: Option<Arc<ExtensionRuntime>>,
    ) -> Result<LoadExtensionsResult, ResourceLoaderError> {
        let is_builtin_path = |path: &str| path.starts_with(BUILTIN_PATH_PREFIX);
        let file_paths: Vec<String> = paths
            .iter()
            .filter(|path| !is_builtin_path(path))
            .cloned()
            .collect();
        let mut result = kiana_extensions::load_extensions_cached(
            &file_paths,
            &self.cwd,
            Some(Arc::clone(&self.event_bus)),
            runtime,
        )
        .await;
        for path in paths.iter().filter(|path| is_builtin_path(path)) {
            let name = &path[BUILTIN_PATH_PREFIX.len()..];
            let Some(builtin) = self.builtin_extensions.get(name) else {
                result.errors.push(LoadError {
                    path: path.clone(),
                    error: format!("Unknown built-in extension: {path}"),
                });
                continue;
            };
            match kiana_extensions::load_extension_from_factory(
                Arc::clone(&builtin.factory),
                &self.cwd,
                Arc::clone(&self.event_bus),
                Arc::clone(&result.runtime),
                Some(path),
            )
            .await
            {
                Ok(mut extension) => {
                    extension.hidden = Some(true);
                    extension.replaceable = Some(builtin.replaceable);
                    result.extensions.push(extension);
                }
                Err(error) => result.errors.push(LoadError {
                    path: path.clone(),
                    error,
                }),
            }
        }
        Ok(result)
    }

    /// `loadFinalExtensionSet(extensionPaths, preTrustExtensions)`。
    /// TS: `resource-loader.ts#loadFinalExtensionSet`。
    ///
    /// 合并预信任结果与最终路径：
    /// - 预信任里已加载的扩展按 `resolved_path` 复用（`<inline:...>` 不参与文件路径去重）；
    /// - 预信任 `errors` 对应的路径视为已失败，本轮不重试；
    /// - 其余路径交给 [`DefaultResourceLoader::load_extension_paths`]；
    /// - 有预信任结果时直接复用其中的 `<inline:...>` 扩展，否则才运行内联工厂。
    ///
    /// 最终顺序严格按 `extension_paths`，内联扩展追加在末尾；随后省略被替换的 replaceable
    /// 扩展、合并 warning，并把 tool/flag 冲突写进 `errors`。
    ///
    /// 预信任结果中已加载、但既不在 `extension_paths`、也不是 `<inline:...>` 的扩展不会出现在
    /// 返回集合里——文件扩展严格按最终路径列表取用。
    async fn load_final_extension_set(
        &self,
        extension_paths: &[String],
        pre_trust_extensions: Option<&LoadExtensionsResult>,
    ) -> Result<LoadExtensionsResult, ResourceLoaderError> {
        let preloaded: Vec<Extension> = pre_trust_extensions
            .map(|result| result.extensions.clone())
            .unwrap_or_default();
        let mut preloaded_by_path: IndexMap<String, Extension> = IndexMap::new();
        for extension in &preloaded {
            if extension.path.starts_with("<inline:") {
                continue;
            }
            preloaded_by_path.insert(extension.resolved_path.clone(), extension.clone());
        }
        let failed_preload_paths: HashSet<String> = pre_trust_extensions
            .map(|result| {
                result
                    .errors
                    .iter()
                    .map(|error| self.resolve_extension_load_path(&error.path))
                    .collect()
            })
            .unwrap_or_default();
        let remaining_paths: Vec<String> = extension_paths
            .iter()
            .filter(|path| {
                let resolved_path = self.resolve_extension_load_path(path);
                !preloaded_by_path.contains_key(&resolved_path)
                    && !failed_preload_paths.contains(&resolved_path)
            })
            .cloned()
            .collect();
        let pre_trust_runtime = pre_trust_extensions.map(|result| Arc::clone(&result.runtime));
        let remaining_extensions = self
            .load_extension_paths(&remaining_paths, pre_trust_runtime)
            .await?;
        let mut loaded_by_path = preloaded_by_path;
        for extension in &remaining_extensions.extensions {
            loaded_by_path.insert(extension.resolved_path.clone(), extension.clone());
        }

        let inline_extensions = match pre_trust_extensions {
            Some(_) => InlineExtensionsResult {
                extensions: preloaded
                    .iter()
                    .filter(|extension| extension.path.starts_with("<inline:"))
                    .cloned()
                    .collect(),
                errors: Vec::new(),
            },
            None => {
                self.load_extension_factories(&remaining_extensions.runtime)
                    .await
            }
        };
        let mut ordered_extensions: Vec<Extension> = extension_paths
            .iter()
            .filter_map(|path| {
                loaded_by_path
                    .get(&self.resolve_extension_load_path(path))
                    .cloned()
            })
            .collect();
        ordered_extensions.extend(inline_extensions.extensions);

        let mut replacement_warnings: Vec<LoadWarning> = Vec::new();
        let mut warnings_vec: Vec<LoadWarning> = pre_trust_extensions
            .and_then(|result| result.warnings.clone())
            .unwrap_or_default();
        warnings_vec.extend(remaining_extensions.warnings.clone().unwrap_or_default());
        let mut extensions_result = LoadExtensionsResult {
            extensions: warnings::omit_replaced_extensions(
                ordered_extensions,
                &mut replacement_warnings,
            ),
            errors: {
                let mut errors: Vec<LoadError> = pre_trust_extensions
                    .map(|result| result.errors.clone())
                    .unwrap_or_default();
                errors.extend(remaining_extensions.errors.clone());
                errors.extend(inline_extensions.errors);
                errors
            },
            warnings: Some(warnings_vec),
            runtime: Arc::clone(&remaining_extensions.runtime),
        };
        warnings::merge_extension_warnings(&mut extensions_result, replacement_warnings);
        add_extension_conflict_diagnostics(&mut extensions_result);
        Ok(extensions_result)
    }

    /// `mapSkillPath(resource, metadataByPath)`：目录型技能（auto / package）展开成
    /// `SKILL.md`。TS: `resource-loader.ts#mapSkillPath`。
    ///
    /// 只有 `metadata.source == "auto"` 或 `origin == Package` 的资源才展开；`stat` 失败或
    /// 不是目录时原样返回。目录下有 `SKILL.md` 时返回该文件路径，并把**原资源的 metadata**
    /// 记到新路径下（已存在则不覆盖）；没有 `SKILL.md` 时保留目录路径，交给技能加载器按目录
    /// 扫描。
    fn map_skill_path(
        &self,
        resource: &ResolvedResource,
        metadata_by_path: &mut IndexMap<String, PathMetadata>,
    ) -> String {
        if resource.metadata.source != "auto" && resource.metadata.origin != SourceOrigin::Package {
            return resource.path.clone();
        }
        match std::fs::metadata(&resource.path) {
            Ok(stat) if stat.is_dir() => {}
            _ => return resource.path.clone(),
        }
        let skill_file = source_info_map::join_path(&resource.path, "SKILL.md");
        if Path::new(&skill_file).exists() {
            if !metadata_by_path.contains_key(&skill_file) {
                metadata_by_path.insert(skill_file.clone(), resource.metadata.clone());
            }
            return skill_file;
        }
        resource.path.clone()
    }

    /// `normalizeExtensionPaths(entries)`：解析路径与 `baseDir`。
    ///
    /// 路径以 loader 的 `cwd` 为基准；`base_dir` 只在非空时解析（TS 用 truthiness 判断，
    /// 空字符串表示“保持空”）。
    fn normalize_extension_paths(
        &self,
        entries: &[ResourceExtensionPath],
    ) -> Vec<ResourceExtensionPath> {
        entries
            .iter()
            .map(|entry| {
                let metadata = match &entry.metadata.base_dir {
                    // TS 用 truthiness 判断：`baseDir: ""` 为 falsy，metadata 原样保留。
                    Some(base_dir) if !base_dir.is_empty() => PathMetadata {
                        base_dir: Some(self.resolve_resource_path(base_dir)),
                        ..entry.metadata.clone()
                    },
                    _ => entry.metadata.clone(),
                };
                ResourceExtensionPath {
                    path: self.resolve_resource_path(&entry.path),
                    metadata,
                }
            })
            .collect()
    }

    /// `updateSkillsFromPaths(skillPaths, metadataByPath?)`。
    ///
    /// `no_skills && skill_paths.is_empty()` 时直接得到空结果（override 仍会执行）；否则用
    /// `include_defaults: false` 加载——默认目录已由包管理器发现并并入路径。override 返回后，
    /// 按每个技能的 `file_path` 回填 `source_info`：命中扩展来源或 metadata 时覆盖，未命中
    /// 保留原值；查找过程的 `stat` 错误向上返回。
    fn update_skills_from_paths(
        &self,
        skill_paths: &[String],
        metadata_by_path: Option<&IndexMap<String, PathMetadata>>,
        extra_source_infos: &IndexMap<String, SourceInfo>,
    ) -> Result<SkillsResult, ResourceLoaderError> {
        let skills_result = if self.no_skills && skill_paths.is_empty() {
            SkillsResult::default()
        } else {
            let loaded = load_skills(LoadSkillsOptions {
                cwd: self.cwd.clone(),
                agent_dir: self.agent_dir.clone(),
                skill_paths: skill_paths.to_vec(),
                include_defaults: false,
            });
            SkillsResult {
                skills: loaded.skills,
                diagnostics: loaded.diagnostics,
            }
        };
        let resolved = match &self.skills_override {
            Some(skills_override) => skills_override(skills_result),
            None => skills_result,
        };
        let mut skills = Vec::with_capacity(resolved.skills.len());
        for mut skill in resolved.skills {
            if let Some(source_info) = source_info_map::find_source_info_for_path(
                &self.cwd,
                &self.agent_dir,
                &skill.file_path,
                Some(extra_source_infos),
                metadata_by_path,
            )? {
                skill.source_info = source_info;
            }
            skills.push(skill);
        }
        Ok(SkillsResult {
            skills,
            diagnostics: resolved.diagnostics,
        })
    }

    /// `updatePromptsFromPaths(promptPaths, metadataByPath?)`。
    ///
    /// 与 [`DefaultResourceLoader::update_skills_from_paths`] 同构，但在 override **之前**先
    /// 按 name 去重（首见保留，重复项产生 `collision` 诊断）；override 之后不再去重。
    fn update_prompts_from_paths(
        &self,
        prompt_paths: &[String],
        metadata_by_path: Option<&IndexMap<String, PathMetadata>>,
        extra_source_infos: &IndexMap<String, SourceInfo>,
    ) -> Result<PromptsResult, ResourceLoaderError> {
        let prompts_result = if self.no_prompt_templates && prompt_paths.is_empty() {
            PromptsResult::default()
        } else {
            let loaded = load_prompt_templates(LoadPromptTemplatesOptions {
                cwd: self.cwd.clone(),
                agent_dir: self.agent_dir.clone(),
                prompt_paths: prompt_paths.to_vec(),
                include_defaults: false,
            });
            let deduped = dedupe_prompts(loaded.templates);
            PromptsResult {
                prompts: deduped.prompts,
                diagnostics: {
                    let mut diagnostics = loaded.diagnostics;
                    diagnostics.extend(deduped.diagnostics);
                    diagnostics
                },
            }
        };
        let resolved = match &self.prompts_override {
            Some(prompts_override) => prompts_override(prompts_result),
            None => prompts_result,
        };
        let mut prompts = Vec::with_capacity(resolved.prompts.len());
        for mut prompt in resolved.prompts {
            if let Some(source_info) = source_info_map::find_source_info_for_path(
                &self.cwd,
                &self.agent_dir,
                &prompt.file_path,
                Some(extra_source_infos),
                metadata_by_path,
            )? {
                prompt.source_info = source_info;
            }
            prompts.push(prompt);
        }
        Ok(PromptsResult {
            prompts,
            diagnostics: resolved.diagnostics,
        })
    }

    /// `updateThemesFromPaths(themePaths, metadataByPath?)`。
    ///
    /// 加载前先算终端能力：`detect_capabilities(&mut || false)` 跳过 tmux 超链接探测（主题
    /// 构造只需要真彩开关），再叠加 settings 里的 capability override。加载结果按 name 去重
    /// （首见保留，`None` 记为 `"unnamed"`），然后才调用 override。override 之后为有
    /// `source_path` 的主题回填 `source_info`；查不到且自身没有 `source_info` 时退回默认
    /// 推断，该推断会对路径做 `stat`，失败即返回错误。
    fn update_themes_from_paths(
        &self,
        theme_paths: &[String],
        metadata_by_path: Option<&IndexMap<String, PathMetadata>>,
        extra_source_infos: &IndexMap<String, SourceInfo>,
    ) -> Result<ThemesResult, ResourceLoaderError> {
        let themes_result = if self.no_themes && theme_paths.is_empty() {
            ThemesResult::default()
        } else {
            // 主题构造只需要 trueColor，跳过无关的 tmux hyperlink 探测。
            let capabilities = apply_capability_overrides(
                detect_capabilities(&mut || false),
                &self.settings_manager.get_terminal_capability_overrides(),
            );
            let color_mode = get_terminal_color_mode(capabilities);
            let loaded =
                themes::load_themes(&self.cwd, &self.agent_dir, theme_paths, false, color_mode);
            let deduped = dedupe_themes(loaded.themes);
            ThemesResult {
                themes: deduped.themes,
                diagnostics: {
                    let mut diagnostics = loaded.diagnostics;
                    diagnostics.extend(deduped.diagnostics);
                    diagnostics
                },
            }
        };
        let resolved = match &self.themes_override {
            Some(themes_override) => themes_override(themes_result),
            None => themes_result,
        };
        let mut themes = Vec::with_capacity(resolved.themes.len());
        for theme in resolved.themes {
            if let Some(source_path) = theme.source_path() {
                let source_path = source_path.to_string_lossy().into_owned();
                if let Some(source_info) = source_info_map::find_source_info_for_path(
                    &self.cwd,
                    &self.agent_dir,
                    &source_path,
                    Some(extra_source_infos),
                    metadata_by_path,
                )? {
                    theme.set_source_info(Some(source_info));
                } else if theme.source_info().is_none() {
                    let source_info = source_info_map::get_default_source_info_for_path(
                        &self.cwd,
                        &self.agent_dir,
                        &source_path,
                    )?;
                    theme.set_source_info(Some(source_info));
                }
            }
            themes.push(theme);
        }
        Ok(ThemesResult {
            themes,
            diagnostics: resolved.diagnostics,
        })
    }

    /// `applyExtensionSourceInfo(extensions, metadataByPath)`。
    ///
    /// 对每个扩展先查 metadata 表（不查扩展注入的 extra source infos）；查不到时退回默认
    /// 推断——`stat` 失败会返回错误，使 reload 在第 20 步失败。同一个 `SourceInfo` 会复制给
    /// 扩展本身、它注册的所有命令与工具。
    ///
    /// 调用方在第 20 步**持写锁**调用本函数：内部的 `stat` 期间，任何读取 `state` 的调用都会
    /// 阻塞；它不调用用户回调。失败时前面已处理的扩展保留已赋值的 `source_info`，与 TS 的
    /// 部分修改一致。
    fn apply_extension_source_info(
        &self,
        extensions: &mut [Extension],
        metadata_by_path: &IndexMap<String, PathMetadata>,
    ) -> Result<(), ResourceLoaderError> {
        for extension in extensions {
            let source_info = match source_info_map::find_source_info_for_path(
                &self.cwd,
                &self.agent_dir,
                &extension.path,
                None,
                Some(metadata_by_path),
            )? {
                Some(source_info) => source_info,
                None => source_info_map::get_default_source_info_for_path(
                    &self.cwd,
                    &self.agent_dir,
                    &extension.path,
                )?,
            };
            extension.source_info = source_info.clone();
            for command in extension.commands.values_mut() {
                command.source_info = source_info.clone();
            }
            for tool in extension.tools.values_mut() {
                tool.source_info = source_info.clone();
            }
        }
        Ok(())
    }

    /// `loadExtensionFactories(runtime)`：匿名工厂编号 1 基。
    ///
    /// 每个工厂用路径 `<inline:N>`（匿名）或 `<inline:name>`（具名）加载，失败只收集到
    /// `errors`。`hidden` / `replaceable` 的映射规则固定：匿名扩展为
    /// `hidden = Some(false)`、`replaceable = Some(false)`；具名扩展的 `hidden` 原样透传
    /// （输入 `None` 仍为 `None`），`replaceable` 归一成 `Some(输入 == Some(true))`。
    async fn load_extension_factories(
        &self,
        runtime: &Arc<ExtensionRuntime>,
    ) -> InlineExtensionsResult {
        let mut extensions: Vec<Extension> = Vec::new();
        let mut errors: Vec<LoadError> = Vec::new();

        for (index, input) in self.extension_factories.iter().enumerate() {
            let (factory, extension_path, is_named, hidden, replaceable) = match input {
                InlineExtension::Factory(factory) => (
                    Arc::clone(factory),
                    format!("<inline:{}>", index + 1),
                    false,
                    None,
                    false,
                ),
                InlineExtension::Named {
                    name,
                    factory,
                    hidden,
                    replaceable,
                    ..
                } => (
                    Arc::clone(factory),
                    format!("<inline:{name}>"),
                    true,
                    *hidden,
                    (*replaceable) == Some(true),
                ),
            };
            match kiana_extensions::load_extension_from_factory(
                factory,
                &self.cwd,
                Arc::clone(&self.event_bus),
                Arc::clone(runtime),
                Some(&extension_path),
            )
            .await
            {
                Ok(mut extension) => {
                    // TS: `ext.hidden = isNamed && input.hidden`：
                    // 匿名 → false；具名且未给 hidden → undefined（`None`）。
                    extension.hidden = if is_named { hidden } else { Some(false) };
                    extension.replaceable = Some(is_named && replaceable);
                    extensions.push(extension);
                }
                Err(error) => errors.push(LoadError {
                    path: extension_path,
                    error,
                }),
            }
        }

        InlineExtensionsResult { extensions, errors }
    }

    /// `resolveExtensionLoadPath(p)`。
    fn resolve_extension_load_path(&self, path: &str) -> String {
        source_info_map::resolve_extension_load_path(&self.cwd, path)
    }

    /// `resolveResourcePath(p)`。
    fn resolve_resource_path(&self, path: &str) -> String {
        source_info_map::resolve_resource_path(&self.cwd, path)
    }

    /// `discoverSystemPromptFile()`。
    ///
    /// 项目 `.kiana/SYSTEM.md` 仅在 `is_project_trusted()` 为真且文件存在时胜出；否则退回
    /// `agent_dir/SYSTEM.md`。两者都不存在返回 `None`。
    fn discover_system_prompt_file(&self) -> Option<String> {
        let project_path = source_info_map::join_path(
            &source_info_map::join_path(&self.cwd, CONFIG_DIR_NAME),
            "SYSTEM.md",
        );
        if self.settings_manager.is_project_trusted() && Path::new(&project_path).exists() {
            return Some(project_path);
        }
        let global_path = source_info_map::join_path(&self.agent_dir, "SYSTEM.md");
        if Path::new(&global_path).exists() {
            return Some(global_path);
        }
        None
    }

    /// `discoverAppendSystemPromptFile()`。
    ///
    /// 与 [`DefaultResourceLoader::discover_system_prompt_file`] 相同的信任与优先级规则，
    /// 文件名换成 `APPEND_SYSTEM.md`。
    fn discover_append_system_prompt_file(&self) -> Option<String> {
        let project_path = source_info_map::join_path(
            &source_info_map::join_path(&self.cwd, CONFIG_DIR_NAME),
            "APPEND_SYSTEM.md",
        );
        if self.settings_manager.is_project_trusted() && Path::new(&project_path).exists() {
            return Some(project_path);
        }
        let global_path = source_info_map::join_path(&self.agent_dir, "APPEND_SYSTEM.md");
        if Path::new(&global_path).exists() {
            return Some(global_path);
        }
        None
    }
}

impl ResourceLoader for DefaultResourceLoader {
    fn get_extensions(&self) -> LoadExtensionsResult {
        DefaultResourceLoader::get_extensions(self)
    }

    fn get_skills(&self) -> SkillsResult {
        DefaultResourceLoader::get_skills(self)
    }

    fn get_prompts(&self) -> PromptsResult {
        DefaultResourceLoader::get_prompts(self)
    }

    fn get_themes(&self) -> ThemesResult {
        DefaultResourceLoader::get_themes(self)
    }

    fn get_agents_files(&self) -> AgentsFilesResult {
        DefaultResourceLoader::get_agents_files(self)
    }

    fn get_system_prompt(&self) -> Option<String> {
        DefaultResourceLoader::get_system_prompt(self)
    }

    fn get_system_prompt_source(&self) -> Option<SystemPromptSource> {
        DefaultResourceLoader::get_system_prompt_source(self)
    }

    fn get_append_system_prompt(&self) -> Vec<String> {
        DefaultResourceLoader::get_append_system_prompt(self)
    }

    fn get_append_system_prompt_sources(&self) -> Vec<SystemPromptSource> {
        DefaultResourceLoader::get_append_system_prompt_sources(self)
    }

    fn extend_resources(&self, paths: ResourceExtensionPaths) -> Result<(), ResourceLoaderError> {
        DefaultResourceLoader::extend_resources(self, paths)
    }

    fn reload<'a>(
        &'a self,
        options: Option<ResourceLoaderReloadOptions>,
    ) -> LocalBoxFuture<'a, Result<(), ResourceLoaderError>> {
        Box::pin(DefaultResourceLoader::reload(self, options))
    }
}

/// `getEnabledResources(resources)`：先记录**全部**（含 disabled）path→metadata
/// （不覆盖已有），再过滤 enabled。TS: `reload` 内的局部函数。
///
/// 因为“已存在不覆盖”，同一路径的 metadata 归属由调用顺序决定；返回的是 enabled 资源的克隆。
fn get_enabled_resources(
    resources: &[ResolvedResource],
    metadata_by_path: &mut IndexMap<String, PathMetadata>,
) -> Vec<ResolvedResource> {
    for resource in resources {
        if !metadata_by_path.contains_key(&resource.path) {
            metadata_by_path.insert(resource.path.clone(), resource.metadata.clone());
        }
    }
    resources
        .iter()
        .filter(|resource| resource.enabled)
        .cloned()
        .collect()
}

/// `getEnabledPaths(resources)`。
fn get_enabled_paths(
    resources: &[ResolvedResource],
    metadata_by_path: &mut IndexMap<String, PathMetadata>,
) -> Vec<String> {
    get_enabled_resources(resources, metadata_by_path)
        .into_iter()
        .map(|resource| resource.path)
        .collect()
}

/// `{...detectCapabilities(() => false), ...overrides}`：`Some` 的字段覆盖探测结果，`None`
/// 的字段保留原值。
fn apply_capability_overrides(
    capabilities: TerminalCapabilities,
    overrides: &CapabilityOverrides,
) -> TerminalCapabilities {
    let mut capabilities = capabilities;
    if let Some(images) = overrides.images {
        capabilities.images = images;
    }
    if let Some(true_color) = overrides.true_color {
        capabilities.true_color = true_color;
    }
    if let Some(hyperlinks) = overrides.hyperlinks {
        capabilities.hyperlinks = hyperlinks;
    }
    capabilities
}

/// `resolvePromptInput(input, description)`。TS: `resource-loader.ts#resolvePromptInput`。
///
/// 空/缺失 → `None`；文件存在 → 读内容（去 BOM），读失败输出警告并**返回路径原文**；
/// 不存在 → 返回原文。
///
/// `exists` / `read` 都按进程当前目录解析相对路径（`std::path` 语义），与 loader 的 `cwd`
/// 无关；自动发现的来源本身已经用 loader 的 `cwd` 拼好路径。路径是目录时同样会尝试读取，
/// 失败后回退成路径原文。
fn resolve_prompt_input(input: Option<&str>, description: &str) -> Option<String> {
    let input = input?;
    if input.is_empty() {
        return None;
    }
    if Path::new(input).exists() {
        match std::fs::read(input) {
            Ok(bytes) => {
                let content = String::from_utf8_lossy(&bytes);
                return Some(strip_bom(&content));
            }
            Err(error) => {
                let message = source_info_map::node_error_to_string(&error, "open", input);
                tracing::warn!("Warning: Could not read {description} file {input}: {message}");
                return Some(input.to_string());
            }
        }
    }
    Some(input.to_string())
}

/// `dedupePrompts(prompts)`：按 name 去重（首见保留），重复产生 `collision` 诊断。
/// TS: `resource-loader.ts#dedupePrompts`。
///
/// 输出保持首次出现的顺序（`IndexMap`）；诊断里的 `path` / `loser_path` 指向被丢弃的重复
/// 项，`winner_path` 指向先出现的项。
fn dedupe_prompts(prompts: Vec<PromptTemplate>) -> PromptsResult {
    let mut seen: IndexMap<String, PromptTemplate> = IndexMap::new();
    let mut diagnostics: Vec<ResourceDiagnostic> = Vec::new();
    for prompt in prompts {
        match seen.get(&prompt.name) {
            Some(existing) => diagnostics.push(ResourceDiagnostic {
                kind: DiagnosticKind::Collision,
                message: format!("name \"/{}\" collision", prompt.name),
                path: Some(prompt.file_path.clone()),
                collision: Some(ResourceCollision {
                    resource_type: ResourceType::Prompt,
                    name: prompt.name.clone(),
                    winner_path: existing.file_path.clone(),
                    loser_path: prompt.file_path.clone(),
                    winner_source: None,
                    loser_source: None,
                }),
            }),
            None => {
                seen.insert(prompt.name.clone(), prompt);
            }
        }
    }
    PromptsResult {
        prompts: seen.into_values().collect(),
        diagnostics,
    }
}

/// `dedupeThemes(themes)`：按 `name ?? "unnamed"` 去重（首见保留）。
/// TS: `resource-loader.ts#dedupeThemes`。
///
/// `name()` 为 `None` 的主题统一记为 `"unnamed"`；没有 `source_path` 的主题在冲突诊断里用
/// `"<builtin>"` 占位。
fn dedupe_themes(themes: Vec<Arc<kiana_theme::Theme>>) -> ThemesResult {
    let mut seen: IndexMap<String, Arc<kiana_theme::Theme>> = IndexMap::new();
    let mut diagnostics: Vec<ResourceDiagnostic> = Vec::new();
    for theme in themes {
        let name = theme.name().unwrap_or("unnamed").to_string();
        let source_path = theme
            .source_path()
            .map(|path| path.to_string_lossy().into_owned());
        match seen.get(&name) {
            Some(existing) => {
                let winner_path = existing
                    .source_path()
                    .map(|path| path.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "<builtin>".to_string());
                diagnostics.push(ResourceDiagnostic {
                    kind: DiagnosticKind::Collision,
                    message: format!("name \"{name}\" collision"),
                    path: source_path.clone(),
                    collision: Some(ResourceCollision {
                        resource_type: ResourceType::Theme,
                        name,
                        winner_path,
                        loser_path: source_path.unwrap_or_else(|| "<builtin>".to_string()),
                        winner_source: None,
                        loser_source: None,
                    }),
                });
            }
            None => {
                seen.insert(name, theme);
            }
        }
    }
    ThemesResult {
        themes: seen.into_values().collect(),
        diagnostics,
    }
}

/// `detectExtensionConflicts(extensions)`：只检测 tool 与 flag 冲突（命令不检测）。
/// TS: `resource-loader.ts#detectExtensionConflicts`。
///
/// 每个名字记录首个注册者；后续**不同** `path` 的扩展注册同名项时记一条冲突（同一扩展内
/// 不会与自己冲突，因为 `IndexMap` 键唯一且 owner 相同）。冲突按扩展遍历顺序产生，`(path,
/// message)` 里的 path 是后来者。
fn detect_extension_conflicts(extensions: &[Extension]) -> Vec<(String, String)> {
    let mut conflicts: Vec<(String, String)> = Vec::new();
    let mut tool_owners: IndexMap<String, String> = IndexMap::new();
    let mut flag_owners: IndexMap<String, String> = IndexMap::new();

    for extension in extensions {
        for tool_name in extension.tools.keys() {
            match tool_owners.get(tool_name) {
                Some(owner) if owner != &extension.path => conflicts.push((
                    extension.path.clone(),
                    format!("Tool \"{tool_name}\" conflicts with {owner}"),
                )),
                Some(_) => {}
                None => {
                    tool_owners.insert(tool_name.clone(), extension.path.clone());
                }
            }
        }
        for flag_name in extension.flags.keys() {
            match flag_owners.get(flag_name) {
                Some(owner) if owner != &extension.path => conflicts.push((
                    extension.path.clone(),
                    format!("Flag \"--{flag_name}\" conflicts with {owner}"),
                )),
                Some(_) => {}
                None => {
                    flag_owners.insert(flag_name.clone(), extension.path.clone());
                }
            }
        }
    }
    conflicts
}

/// `addExtensionConflictDiagnostics(extensionsResult)`：冲突写进 `errors`（不是 warnings）。
fn add_extension_conflict_diagnostics(result: &mut LoadExtensionsResult) {
    for (path, message) in detect_extension_conflicts(&result.extensions) {
        result.errors.push(LoadError {
            path,
            error: message,
        });
    }
}
