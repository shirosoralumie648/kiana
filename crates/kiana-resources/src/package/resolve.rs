//! `resolve` / `resolvePackageSources` / dedupe / `toResolvedPaths`。
//!
//! TS: `package-manager.ts` 的 `resolve`（主流程部分在 `mod.rs`）、`resolveLocalEntries` /
//! `addAutoDiscoveredResources` / `collectFilesFromPaths` / `resolvePackageSources` /
//! `findAutoloadDeltaBase` / `resolveLocalExtensionSource` / `collectPackageResources` /
//! `collectDefaultResources` / `applyPackageFilter` / `applyPackageDeltaFilter` /
//! `collectManifestFiles` / `addManifestEntries` / `collectFilesFromManifestEntries` /
//! `getTargetMap` / `addResource` / `createAccumulator` / `toResolvedPaths`。
//!
//! ## 不变量（`spec/ca-resources.md` §0.4）
//!
//! - `addResource` 首次写入胜出（先 `contains_key` 再 `insert`，**不是**无条件 `insert`）。
//! - `toResolvedPaths` 按 `resourcePrecedenceRank` **稳定升序**排序，再按
//!   `canonicalizePath` 去重保留首个。
//! - `resolve()` 先加项目源、再加用户源。
//!
//! ## 为什么需要这个模块
//!
//! `package` 模块的其余文件负责“怎么找到/装好一个包”（`parse` / `npm` / `git` /
//! `paths`），本文件负责“把各来源的资源汇总成最终列表”：settings 顶层条目、自动发现、
//! 包内资源、内置扩展都往同一个累积器里写，最后由 `to_resolved_paths` 统一排序、去重、
//! 定型。把累积逻辑集中在这里，是为了让“谁先写谁生效”的优先级规则只有一个实现点。
//!
//! ## 在系统中的位置
//!
//! - 调用方：同模块的 `mod.rs`。`resolve_inner` 依次调用 `create_accumulator` →
//!   `resolve_package_sources`（包）→ `resolve_local_entries`（settings 顶层条目）→
//!   `add_auto_discovered_resources`（自动发现）→ 内置扩展 → `to_resolved_paths`；
//!   `resolve_extension_sources_inner` 复用 `resolve_package_sources` 与
//!   `to_resolved_paths`。
//! - 被调用方：`discover`（目录发现）、`patterns`（模式过滤、优先级、glob）、`paths`
//!   （路径词法运算）、`parse`（源解析与 identity）、`npm` / `git`（安装与刷新）。
//! - settings 原始 JSON 的宽松回退不在本文件，统一由 `settings_io` 处理。
//!
//! ## 一次解析的执行流程
//!
//! 1. 调用方按“项目 → 用户”的顺序准备条目/包列表，保证首次写入胜出时项目优先。
//! 2. 每个包：算出真正要解析的源（`autoload:false` 的项目条目会退回到同 identity 的
//!    用户包，即 delta 基座），按 local / npm / git 分支安装并收集资源；安装失败只记录到
//!    返回值，不中断后续包。
//! 3. 顶层 settings 条目与自动发现继续往同一个累积器写；每条都带 `enabled`。被规则排除
//!    的路径也会写入（`enabled = false`），从而占据该路径的首次写入并出现在最终列表中。
//! 4. `to_resolved_paths` 对每种类型：按插入顺序取出 → 按优先级稳定排序 → 按
//!    `canonicalize_path` 去重保留首个。`install_failures` 由调用方在之后填入。
//!
//! ## 数据结构
//!
//! - 累积器是四个 `IndexMap<路径字符串, AddedResource>`。用 `IndexMap` 而不是 `HashMap`
//!   是为了保留插入顺序：`map_to_resolved` 的稳定排序在同 rank 时依赖它（TS 的
//!   `Array.prototype.sort` 同样是稳定排序）。
//! - 路径字符串一律是已解析的绝对路径（`builtin:<name>` 合成路径是唯一例外），作为 map
//!   的键做精确字符串比较；符号链接/大小写差异只在最终的 `canonicalize_path` 阶段处理。
//!
//! ## 失败模式
//!
//! - 本地路径不存在、`metadata` 失败：静默跳过。
//! - 本地目录既没有能解析出 kiana 对象的 package.json、也没有任何约定资源目录时，
//!   `resolve_local_extension_source` 会把目录本身当作扩展写入（约定目录哪怕为空也算
//!   存在，`collect_package_resources` 只看目录是否存在，不看是否有文件）；npm/git 包
//!   目录里没有可识别资源时不写入任何条目（`collect_package_resources` 的返回值在两条
//!   路径上的用途不同）。
//! - 包安装失败：折叠成 `PackageInstallFailure` 返回给调用方作为启动警告，绝不 `Err`。
//! - `onMissing` 返回 `Error`：这是 `install_missing_source` 内部唯一会以 `Err` 形式
//!   冒出的错误（安装失败都折叠成 `Failed` 警告），文案为 `Missing source: <源>`。
//! - 源字符串非法、identity 解析失败、project 作用域的 trust 检查、settings 形状错误：
//!   这些路径返回 `Err`，由调用方（`mod.rs`）整体上抛，不会降级成启动警告。
//! - 离线（`KIANA_OFFLINE`）时安装与临时 git 刷新都跳过，已安装包的资源照常收集。
//!
//! ## 注意事项
//!
//! - `ResourceType::ALL` 的顺序同时是目录遍历顺序与 `add_auto_discovered_resources`
//!   里 overrides 数组的下标顺序；改顺序会改变结果。
//! - `collect_package_resources` / `collect_default_resources` / `collect_manifest_files`
//!   每次调用都会重新读取并解析 `package.json`（同步 IO），没有缓存；同一个包内会出现
//!   多次读取。
//! - manifest 字段的**空数组**语义在两条路径上不同：`collect_default_resources` 视为
//!   “已声明且没有条目”，不再看约定目录；`collect_manifest_files` 的
//!   `entries && entries.length > 0` 则回退到约定目录。
//!
//! ## 推荐阅读顺序
//!
//! 1. `ResourceType`、`ResourceAccumulator`、`add_resource`：先理解“路径 → 首次写入”的
//!    累积模型。
//! 2. `resolve_package_sources`：包来源如何选中作用域、安装并收集资源。
//! 3. `collect_package_resources` / `collect_manifest_files`：包内资源的三个收集分支与
//!    manifest 过滤规则。
//! 4. `resolve_local_entries` / `add_auto_discovered_resources`：顶层 settings 与自动发现。
//! 5. `map_to_resolved` / `to_resolved_paths`：排序与去重的收尾。

use indexmap::IndexMap;

use kiana_config::settings::schema::{PackageSource, PackageSourceObject};
use kiana_core::kiana_manifest::KianaManifest;
use kiana_core::source_info::{PathMetadata, SourceOrigin};

use super::discover::{
    SkillDiscoveryMode, collect_ancestor_agents_skill_dirs, collect_auto_extension_entries,
    collect_auto_prompt_entries, collect_auto_skill_entries, collect_auto_theme_entries,
    collect_resource_files,
};
use super::exec::get_home_dir;
use super::parse::{ConfiguredEntry, LocalSource, ParsedSource, get_package_source_string};
use super::paths::{dirname, join, path_exists, path_resolve, resolve_join};
use super::patterns::{
    apply_autoload_disabled_patterns, apply_patterns, expand_package_glob, get_override_patterns,
    has_glob_pattern, is_enabled_by_overrides, is_override_pattern, resource_precedence_rank,
    split_patterns,
};
use super::settings_io::settings_string_list;
use super::types::{
    MissingSourceAction, MissingSourceCallback, PackageInstallFailure, ResolvedPaths,
    ResolvedResource, SourceScope,
};
use super::{DefaultPackageManager, PackageManagerError, err};

/// 四种资源类型。TS: `type ResourceType`（顺序即 `RESOURCE_TYPES`）。
///
/// 顺序有语义：`ALL` 的次序决定目录遍历顺序，也决定 `add_auto_discovered_resources`
/// 里 overrides 数组的下标（`user_overrides[i]` 对应 `ALL[i]`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum ResourceType {
    /// `extensions`。
    Extensions,
    /// `skills`。
    Skills,
    /// `prompts`。
    Prompts,
    /// `themes`。
    Themes,
}

impl ResourceType {
    /// `RESOURCE_TYPES`：四种类型按 TS 声明顺序排列。
    ///
    /// 本文件所有“遍历四种类型”的循环都依赖这个数组；下标与类型的对应关系还被
    /// `add_auto_discovered_resources` 用作 overrides 数组的索引。
    pub(crate) const ALL: [ResourceType; 4] = [
        ResourceType::Extensions,
        ResourceType::Skills,
        ResourceType::Prompts,
        ResourceType::Themes,
    ];

    /// TS 的字符串拼写。同时用作约定目录名（`<root>/<name>`）与 settings 的键名。
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            ResourceType::Extensions => "extensions",
            ResourceType::Skills => "skills",
            ResourceType::Prompts => "prompts",
            ResourceType::Themes => "themes",
        }
    }

    /// 未知类型（类型系统不可达）时的逐字错误：`Unknown resource type: ${resourceType}`。
    ///
    /// TS 的 `getTargetMap` 有一个类型上不可达的 `default` 分支；Rust 的 enum 不可能出现
    /// 未知值，因此这个入口只用于测试与文档化该不变量。
    #[cfg(test)]
    pub(crate) fn from_name(resource_type: &str) -> Result<Self, PackageManagerError> {
        match resource_type {
            "extensions" => Ok(ResourceType::Extensions),
            "skills" => Ok(ResourceType::Skills),
            "prompts" => Ok(ResourceType::Prompts),
            "themes" => Ok(ResourceType::Themes),
            _ => Err(err(format!("Unknown resource type: {resource_type}"))),
        }
    }
}

/// 累积器里的一条资源。TS: `{ metadata: PathMetadata; enabled: boolean }`。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AddedResource {
    /// 元数据（来源、作用域、`base_dir`、`package_root`），由首次写入者提供。
    pub metadata: PathMetadata,
    /// 是否启用：被过滤规则排除的路径仍留在 map 里，只是标记为 `false`。
    pub enabled: bool,
}

/// 四类资源的累积器。TS: `ResourceAccumulator`。
///
/// 四个 `IndexMap` 都以资源路径为键，保留首次插入顺序；同一路径只保留首次写入的
/// `metadata` 与 `enabled`（见 `add_resource` 的语义）。用 `IndexMap` 而不是
/// `HashMap`，是因为最终列表在同优先级时要保持插入顺序。
#[derive(Debug, Clone, Default)]
pub(crate) struct ResourceAccumulator {
    /// 扩展。
    pub extensions: IndexMap<String, AddedResource>,
    /// 技能。
    pub skills: IndexMap<String, AddedResource>,
    /// prompt 模板。
    pub prompts: IndexMap<String, AddedResource>,
    /// 主题。
    pub themes: IndexMap<String, AddedResource>,
}

impl ResourceAccumulator {
    /// `getTargetMap(accumulator, resourceType)`：返回所选类型的整张 map（可变借用），
    /// 调用方随后通常把它交给 `add_resource`。
    pub(crate) fn target_map(
        &mut self,
        resource_type: ResourceType,
    ) -> &mut IndexMap<String, AddedResource> {
        match resource_type {
            ResourceType::Extensions => &mut self.extensions,
            ResourceType::Skills => &mut self.skills,
            ResourceType::Prompts => &mut self.prompts,
            ResourceType::Themes => &mut self.themes,
        }
    }
}

/// `addResource(map, path, metadata, enabled)`：首次写入胜出。
///
/// 空路径直接忽略；已存在的键不会被更新（`enabled` 与 `metadata` 都不变），这是“先加的
/// 来源优先”的唯一实现点。键是精确字符串比较，不做路径规范化。
pub(crate) fn add_resource(
    map: &mut IndexMap<String, AddedResource>,
    path: &str,
    metadata: &PathMetadata,
    enabled: bool,
) {
    if path.is_empty() {
        return;
    }
    if !map.contains_key(path) {
        map.insert(
            path.to_string(),
            AddedResource {
                metadata: metadata.clone(),
                enabled,
            },
        );
    }
}

/// `mapToResolved(entries)`：稳定排序 + canonicalize 去重。
///
/// 步骤：
/// 1. 按 map 的插入顺序克隆成 `Vec`；
/// 2. `sort_by_key(resource_precedence_rank)`：Rust 的稳定排序，同 rank 保持插入顺序；
/// 3. 依序 `canonicalize_path`，规范路径重复的条目丢弃靠后者。
///
/// 去重发生在排序之后，所以保留的是“排序后最靠前”的写法，而不是“路径字符串相同”的
/// 第一条。`canonicalize_path` 在 realpath 失败时原样返回入参（见 `kiana-core` 的
/// `paths` 模块文档），因此不存在的路径只能按字符串相等去重。
pub(crate) fn map_to_resolved(entries: &IndexMap<String, AddedResource>) -> Vec<ResolvedResource> {
    let mut resolved: Vec<ResolvedResource> = entries
        .iter()
        .map(|(path, added)| ResolvedResource {
            path: path.clone(),
            enabled: added.enabled,
            metadata: added.metadata.clone(),
        })
        .collect();
    resolved.sort_by_key(|entry| resource_precedence_rank(&entry.metadata));

    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    resolved.retain(|entry| {
        let canonical_path = kiana_core::paths::canonicalize_path(&entry.path);
        if seen.contains(&canonical_path) {
            return false;
        }
        seen.insert(canonical_path);
        true
    });
    resolved
}

/// `toResolvedPaths(accumulator)`。
///
/// 把四个累积器分别转成排序去重后的列表；`install_failures` 留空（`None`），由调用方
/// （`mod.rs` 的 `resolve_inner` / `resolve_extension_sources_inner`）在之后填入实际结果。
pub(crate) fn to_resolved_paths(accumulator: &ResourceAccumulator) -> ResolvedPaths {
    ResolvedPaths {
        extensions: map_to_resolved(&accumulator.extensions),
        skills: map_to_resolved(&accumulator.skills),
        prompts: map_to_resolved(&accumulator.prompts),
        themes: map_to_resolved(&accumulator.themes),
        install_failures: None,
    }
}

/// `createAccumulator()`。
pub(crate) fn create_accumulator() -> ResourceAccumulator {
    ResourceAccumulator::default()
}

/// `manifest[field]`（借用版）。
///
/// `None` 表示 `kiana` 对象没有该字段或字段值无效；`Some(vec![])` 表示显式声明为空列表。
/// 两者在 `collect_default_resources` 中语义不同：后者会阻止回退到约定目录。
fn manifest_field(manifest: &KianaManifest, resource_type: ResourceType) -> Option<&Vec<String>> {
    match resource_type {
        ResourceType::Extensions => manifest.extensions.as_ref(),
        ResourceType::Skills => manifest.skills.as_ref(),
        ResourceType::Prompts => manifest.prompts.as_ref(),
        ResourceType::Themes => manifest.themes.as_ref(),
    }
}

/// `filter[type]`（`PackageFilter`）。
///
/// `None` 表示 settings 对象没有该字段（回退到默认发现）；`Some(&[])` 表示显式空数组
/// （禁用该类型的全部资源）。
fn filter_field(filter: &PackageSourceObject, resource_type: ResourceType) -> Option<&Vec<String>> {
    match resource_type {
        ResourceType::Extensions => filter.extensions.as_ref(),
        ResourceType::Skills => filter.skills.as_ref(),
        ResourceType::Prompts => filter.prompts.as_ref(),
        ResourceType::Themes => filter.themes.as_ref(),
    }
}

/// `installMissing()` 的结果。
///
/// 安装错误不会以 `Err` 冒泡：它被转成 `Failed(PackageInstallFailure)`，由
/// `resolve_package_sources` 收集后作为启动警告返回；`Err` 只用于 `onMissing` 显式要求
/// 报错的情况。
enum MissingOutcome {
    /// 安装成功。
    Installed,
    /// 跳过（离线 / `onMissing` 返回 `skip`）。
    Skipped,
    /// 安装失败，需要作为启动警告上报。
    Failed(PackageInstallFailure),
}

impl DefaultPackageManager {
    /// `resolvePackageSources(sources, accumulator, onMissing?)`。
    ///
    /// 串行处理每个包源，把发现的资源写进 `accumulator`，返回无法安装的包列表。
    ///
    /// 每个条目的处理顺序：
    /// 1. 取源字符串；对象形式额外取出过滤规则。`metadata` 始终使用**原始**源字符串与
    ///    作用域，即使下面改用了 delta 基座。
    /// 2. `find_autoload_delta_base` 判断项目条目是否为 `autoload: false` 的增量；是则改用
    ///    同 identity 的用户包来解析/安装。
    /// 3. 按源类型分派：local 直接就地收集（不安装）；npm 在路径缺失或已装版本不满足配置
    ///    时安装；git 在路径缺失时安装，临时作用域的非固定 ref 额外刷新缓存。
    /// 4. 安装失败记录进返回值并跳过该包，后续包继续处理。
    ///
    /// 错误：`get_package_source_string`、路径计算、trust 断言以及 `onMissing` 返回
    /// `Error` 会冒泡；安装失败本身不会。
    ///
    /// 处理是串行的（普通 `for` 循环，不并发安装），因此写入累积器的顺序严格等于
    /// `sources` 的顺序；调用方已按“项目在前、用户在后”排好序并去重。返回的
    /// `PackageInstallFailure.source` 是**实际解析后**的源（delta 基座时是 user 条目的
    /// 源），而写进 metadata 的 `source` 始终是当前条目的原始源字符串。
    pub(crate) async fn resolve_package_sources(
        &self,
        sources: &[ConfiguredEntry],
        accumulator: &mut ResourceAccumulator,
        on_missing: Option<&MissingSourceCallback>,
    ) -> Result<Vec<PackageInstallFailure>, PackageManagerError> {
        let mut install_failures: Vec<PackageInstallFailure> = Vec::new();
        for entry in sources {
            let source_str = get_package_source_string(&entry.pkg)?.to_string();
            let filter = match &entry.pkg {
                PackageSource::Text(_) => None,
                PackageSource::Object(object) => Some(object),
            };
            // 项目侧的 autoload:false 条目是增量：解析与安装都改用同 identity 的用户包，
            // 但 metadata 仍记录项目条目的 source/scope（见下方构造）。
            let delta_base = self.find_autoload_delta_base(&entry.pkg, entry.scope, sources)?;
            let (resolved_source, resolved_scope) = match delta_base {
                Some((source, scope)) => (source, scope),
                None => (source_str.clone(), entry.scope),
            };
            let parsed = self.parse_source(&resolved_source);
            let mut metadata = PathMetadata {
                source: source_str.clone(),
                scope: entry.scope,
                origin: SourceOrigin::Package,
                base_dir: None,
                package_root: None,
            };

            match &parsed {
                ParsedSource::Local(local) => {
                    let base_dir = self.get_base_dir_for_scope(resolved_scope)?;
                    self.resolve_local_extension_source(
                        local,
                        accumulator,
                        filter,
                        &mut metadata,
                        &base_dir,
                    );
                    continue;
                }
                ParsedSource::Npm(npm) => {
                    let mut installed_path = self.get_npm_install_path(npm, resolved_scope)?;
                    // 路径存在不代表版本正确：已装版本不满足配置的 semver 范围时也要重装。
                    let needs_install = !path_exists(&installed_path)
                        || !self.installed_npm_matches_configured_version(npm, &installed_path);
                    if needs_install {
                        match self
                            .install_missing_source(
                                &parsed,
                                resolved_scope,
                                &resolved_source,
                                on_missing,
                            )
                            .await?
                        {
                            MissingOutcome::Installed => {
                                // 重新计算：user 作用域下 get_npm_install_path 会先看 managed
                                // 路径、不存在时回退遗留全局安装，结果取决于安装后的文件系统。
                                installed_path = self.get_npm_install_path(npm, resolved_scope)?;
                            }
                            MissingOutcome::Skipped => continue,
                            MissingOutcome::Failed(failure) => {
                                install_failures.push(failure);
                                continue;
                            }
                        }
                    }
                    metadata.base_dir = Some(installed_path.clone());
                    metadata.package_root = Some(installed_path.clone());
                    self.collect_package_resources(&installed_path, accumulator, filter, &metadata);
                }
                ParsedSource::Git(git) => {
                    let installed_path = self.get_git_install_path(git, resolved_scope)?;
                    if !path_exists(&installed_path) {
                        match self
                            .install_missing_source(
                                &parsed,
                                resolved_scope,
                                &resolved_source,
                                on_missing,
                            )
                            .await?
                        {
                            MissingOutcome::Installed => {}
                            MissingOutcome::Skipped => continue,
                            MissingOutcome::Failed(failure) => {
                                install_failures.push(failure);
                                continue;
                            }
                        }
                    } else if resolved_scope == SourceScope::Temporary
                        && !git.pinned
                        && !super::is_offline_mode_enabled()
                    {
                        // 临时作用域的缓存跨次运行复用；非固定 ref 且在线时先刷新。刷新失败
                        // 静默保留旧缓存（refresh_temporary_git_source 内部吞掉错误）。
                        self.refresh_temporary_git_source(git, &resolved_source)
                            .await?;
                    }
                    metadata.base_dir = Some(installed_path.clone());
                    metadata.package_root = Some(installed_path.clone());
                    self.collect_package_resources(&installed_path, accumulator, filter, &metadata);
                }
            }
        }
        Ok(install_failures)
    }

    /// `installMissing()`。
    ///
    /// 判定顺序固定：先看离线（离线时直接 `Skipped`，**不会**调用 `on_missing`）；
    /// 有回调时按 `Skip` / `Error` / `Install` 处理；只有 `Error` 返回 `Err`，文案为
    /// `Missing source: <实际源>`。真正的安装失败被折叠成 `Failed(failure)`（记录实际源
    /// 与作用域），让调用方继续处理后面的包。
    async fn install_missing_source(
        &self,
        parsed: &ParsedSource,
        scope: SourceScope,
        resolved_source: &str,
        on_missing: Option<&MissingSourceCallback>,
    ) -> Result<MissingOutcome, PackageManagerError> {
        if super::is_offline_mode_enabled() {
            return Ok(MissingOutcome::Skipped);
        }
        if let Some(callback) = on_missing {
            match callback(resolved_source.to_string()).await {
                MissingSourceAction::Skip => return Ok(MissingOutcome::Skipped),
                MissingSourceAction::Error => {
                    return Err(err(format!("Missing source: {resolved_source}")));
                }
                MissingSourceAction::Install => {}
            }
        }
        match self.install_parsed_source(parsed, scope).await {
            Ok(()) => Ok(MissingOutcome::Installed),
            Err(error) => Ok(MissingOutcome::Failed(PackageInstallFailure {
                source: resolved_source.to_string(),
                scope,
                message: error.message().to_string(),
            })),
        }
    }

    /// `installParsedSource(parsed, scope)`。
    ///
    /// 只做分派：npm 以 `scope == Temporary` 决定是否走临时安装根，git 交给
    /// `install_git`；local 源不需要安装，直接成功。安装器返回的错误原样上抛，由
    /// `install_missing_source` 决定折叠成警告还是继续冒泡。
    pub(crate) async fn install_parsed_source(
        &self,
        parsed: &ParsedSource,
        scope: SourceScope,
    ) -> Result<(), PackageManagerError> {
        match parsed {
            ParsedSource::Npm(npm) => {
                self.install_npm(npm, scope, scope == SourceScope::Temporary)
                    .await
            }
            ParsedSource::Git(git) => self.install_git(git, scope).await,
            ParsedSource::Local(_) => Ok(()),
        }
    }

    /// `findAutoloadDeltaBase(pkg, scope, sources)`。
    ///
    /// 只有“项目作用域 + 对象形式 + `autoload == false`”三个条件同时满足才会查找基座，
    /// 其余情况立即返回 `None`。查找时把当前条目与每个 user 条目都转成 identity 再比较，
    /// 返回 `sources` 中第一个 identity 相同的 user 条目（源字符串 + user 作用域）。
    /// 源字符串缺失或 identity 解析（含 project trust 检查）出错时向上冒泡。
    fn find_autoload_delta_base(
        &self,
        pkg: &PackageSource,
        scope: SourceScope,
        sources: &[ConfiguredEntry],
    ) -> Result<Option<(String, SourceScope)>, PackageManagerError> {
        if scope != SourceScope::Project {
            return Ok(None);
        }
        let PackageSource::Object(object) = pkg else {
            return Ok(None);
        };
        if object.autoload != Some(false) {
            return Ok(None);
        }
        let identity = self.get_package_identity(get_package_source_string(pkg)?, Some(scope))?;
        for entry in sources {
            if entry.scope != SourceScope::User {
                continue;
            }
            let key = self.get_package_identity(
                get_package_source_string(&entry.pkg)?,
                Some(SourceScope::User),
            )?;
            if key == identity {
                return Ok(Some((
                    get_package_source_string(&entry.pkg)?.to_string(),
                    SourceScope::User,
                )));
            }
        }
        Ok(None)
    }

    /// `resolveLocalExtensionSource(source, accumulator, filter, metadata, baseDir)`。
    ///
    /// 与 npm/git 不同，local 源不安装：直接把 `source.path` 按 `baseDir` 做词法解析。
    /// 路径不存在或 `metadata` 失败时静默返回。文件直接作为扩展写入（`base_dir` 记为父
    /// 目录）；目录先按包规则收集，只有当 `collect_package_resources` 返回 false（既没有
    /// 能解析出 `kiana` 对象的 `package.json`，也没有任何约定资源目录）时，才把目录本身
    /// 当作扩展写入。
    ///
    /// `metadata` 是可变借用，本函数会就地填写 `base_dir` / `package_root`；`source` 与
    /// `scope` 保持不变。
    fn resolve_local_extension_source(
        &self,
        source: &LocalSource,
        accumulator: &mut ResourceAccumulator,
        filter: Option<&PackageSourceObject>,
        metadata: &mut PathMetadata,
        base_dir: &str,
    ) {
        let resolved = self.resolve_path_from_base(&source.path, base_dir);
        if !path_exists(&resolved) {
            return;
        }
        let Ok(stats) = std::fs::metadata(&resolved) else {
            return;
        };
        if stats.is_file() {
            metadata.base_dir = Some(dirname(&resolved));
            add_resource(
                accumulator.target_map(ResourceType::Extensions),
                &resolved,
                metadata,
                true,
            );
            return;
        }
        if stats.is_dir() {
            metadata.base_dir = Some(resolved.clone());
            metadata.package_root = Some(resolved.clone());
            let resources =
                self.collect_package_resources(&resolved, accumulator, filter, metadata);
            if !resources {
                add_resource(
                    accumulator.target_map(ResourceType::Extensions),
                    &resolved,
                    metadata,
                    true,
                );
            }
        }
    }

    /// `collectPackageResources(packageRoot, accumulator, filter, metadata): boolean`。
    ///
    /// 三个互斥分支，判断顺序固定：
    /// 1. 有过滤对象：逐类型按 `autoload` 与字段是否存在分派到 delta / filter / default
    ///    收集，**总是返回 true**（是否真的写入了资源不影响返回值）。
    /// 2. 没有过滤对象但 `package.json` 能解析出 `kiana` 对象：按 manifest 四类字段收集，
    ///    返回 true。字段缺省或空数组都不会回退到约定目录。
    /// 3. 其它情况：扫描 `<root>/<type>` 约定目录，返回“是否至少存在一个类型目录”。
    ///
    /// 返回值只被 `resolve_local_extension_source` 使用：false 时调用方把目录本身当作
    /// 扩展写入。npm/git 路径忽略返回值。
    ///
    /// 注意：`autoload == false` 而过滤对象又没有某类型字段时，该类型的模式是空数组，
    /// `apply_package_delta_filter` 直接返回，因此这一类型不会从当前包收集任何资源。
    pub(crate) fn collect_package_resources(
        &self,
        package_root: &str,
        accumulator: &mut ResourceAccumulator,
        filter: Option<&PackageSourceObject>,
        metadata: &PathMetadata,
    ) -> bool {
        if let Some(filter) = filter {
            for resource_type in ResourceType::ALL {
                let patterns = filter_field(filter, resource_type);
                if filter.autoload == Some(false) {
                    let patterns = patterns.cloned().unwrap_or_default();
                    let target = accumulator.target_map(resource_type);
                    self.apply_package_delta_filter(
                        package_root,
                        &patterns,
                        resource_type,
                        target,
                        metadata,
                    );
                } else if let Some(patterns) = patterns {
                    let patterns = patterns.clone();
                    let target = accumulator.target_map(resource_type);
                    self.apply_package_filter(
                        package_root,
                        &patterns,
                        resource_type,
                        target,
                        metadata,
                    );
                } else {
                    let target = accumulator.target_map(resource_type);
                    self.collect_default_resources(package_root, resource_type, target, metadata);
                }
            }
            return true;
        }

        let manifest =
            kiana_core::kiana_manifest::read_kiana_manifest(std::path::Path::new(&join(&[
                package_root,
                "package.json",
            ])));
        if let Some(manifest) = manifest {
            for resource_type in ResourceType::ALL {
                let entries = manifest_field(&manifest, resource_type).cloned();
                let target = accumulator.target_map(resource_type);
                self.add_manifest_entries(
                    entries.as_deref(),
                    package_root,
                    resource_type,
                    target,
                    metadata,
                );
            }
            return true;
        }

        let mut has_any_dir = false;
        for resource_type in ResourceType::ALL {
            let dir = join(&[package_root, resource_type.as_str()]);
            if path_exists(&dir) {
                let files = collect_resource_files(&dir, resource_type);
                let target = accumulator.target_map(resource_type);
                for file in files {
                    add_resource(target, &file, metadata, true);
                }
                has_any_dir = true;
            }
        }
        has_any_dir
    }

    /// `collectDefaultResources(root, type, target, metadata)`。
    ///
    /// 单类型的默认收集，用于“有过滤对象但没有该类型字段”的情形。manifest 字段存在
    /// （包括空数组）时只走 `add_manifest_entries`：空数组即什么都不加，**不会**回退到
    /// 约定目录；字段缺省或 manifest 不可读时才扫描 `<root>/<type>`。
    fn collect_default_resources(
        &self,
        package_root: &str,
        resource_type: ResourceType,
        target: &mut IndexMap<String, AddedResource>,
        metadata: &PathMetadata,
    ) {
        let manifest =
            kiana_core::kiana_manifest::read_kiana_manifest(std::path::Path::new(&join(&[
                package_root,
                "package.json",
            ])));
        let entries = manifest
            .as_ref()
            .and_then(|manifest| manifest_field(manifest, resource_type))
            .cloned();
        if let Some(entries) = entries {
            self.add_manifest_entries(
                Some(&entries),
                package_root,
                resource_type,
                target,
                metadata,
            );
            return;
        }
        let dir = join(&[package_root, resource_type.as_str()]);
        if path_exists(&dir) {
            let files = collect_resource_files(&dir, resource_type);
            for file in files {
                add_resource(target, &file, metadata, true);
            }
        }
    }

    /// `applyPackageFilter(root, userPatterns, type, target, metadata)`。
    ///
    /// 候选文件先由 `collect_manifest_files` 决定（manifest 显式条目或约定目录）。
    /// `user_patterns` 为空时，所有候选都以 `enabled = false` 写入：它们仍占据首次写入并
    /// 出现在最终列表中，对应 TS 的“空数组显式禁用”。非空时用 `apply_patterns` 求启用
    /// 集合，候选文件逐个写入，未命中的也写入但 `enabled = false`；函数不会丢弃任何
    /// 候选文件。
    fn apply_package_filter(
        &self,
        package_root: &str,
        user_patterns: &[String],
        resource_type: ResourceType,
        target: &mut IndexMap<String, AddedResource>,
        metadata: &PathMetadata,
    ) {
        let (all_files, _) = self.collect_manifest_files(package_root, resource_type);

        if user_patterns.is_empty() {
            // 空数组显式禁用该类型的全部资源。
            for file in &all_files {
                add_resource(target, file, metadata, false);
            }
            return;
        }

        let enabled_by_user = apply_patterns(&all_files, user_patterns, package_root);
        for file in &all_files {
            add_resource(target, file, metadata, enabled_by_user.contains(file));
        }
    }

    /// `applyPackageDeltaFilter(root, userPatterns, type, target, metadata)`。
    ///
    /// delta 基座专用：`user_patterns` 为空时**不读取 manifest、不写入任何条目**。
    /// 非空时只写入被模式命中的文件，`enabled` 由 `apply_autoload_disabled_patterns`
    /// 决定（同一路径后出现的模式覆盖先出现的；`-` / `!` 禁用，`+` 启用，无前缀的普通
    /// 模式按 glob 匹配、命中即启用）。未被任何模式
    /// 命中的文件不由本分支写入；同一 identity 的 user 条目仍在 `sources` 中，随后会按
    /// 它自己的规则处理这些路径。
    fn apply_package_delta_filter(
        &self,
        package_root: &str,
        user_patterns: &[String],
        resource_type: ResourceType,
        target: &mut IndexMap<String, AddedResource>,
        metadata: &PathMetadata,
    ) {
        if user_patterns.is_empty() {
            return;
        }
        let (all_files, _) = self.collect_manifest_files(package_root, resource_type);
        let enabled_by_user =
            apply_autoload_disabled_patterns(&all_files, user_patterns, package_root);
        for (file_path, enabled) in enabled_by_user {
            add_resource(target, &file_path, metadata, enabled);
        }
    }

    /// `collectManifestFiles(root, type)`。
    ///
    /// 返回 `(all_files, enabled_by_manifest)`：
    /// - manifest 字段是**非空**数组：候选文件由 `collect_files_from_manifest_entries`
    ///   展开（glob 与非 glob），再用字段里的 `!` / `+` / `-` 覆盖模式跑一遍
    ///   `apply_patterns`；返回的 `all_files` 就是过滤后的启用集合（顺序同 `IndexSet`）。
    /// - 字段是空数组或缺省、或 manifest 不可读：回退到 `<root>/<type>` 约定目录，
    ///   此时所有文件都视为启用。
    ///
    /// 与 `collect_default_resources` 的关键差异：这里空数组会回退约定目录，那里不会。
    fn collect_manifest_files(
        &self,
        package_root: &str,
        resource_type: ResourceType,
    ) -> (Vec<String>, indexmap::IndexSet<String>) {
        let manifest =
            kiana_core::kiana_manifest::read_kiana_manifest(std::path::Path::new(&join(&[
                package_root,
                "package.json",
            ])));
        let entries = manifest
            .as_ref()
            .and_then(|manifest| manifest_field(manifest, resource_type))
            .cloned();
        if let Some(entries) = entries
            && !entries.is_empty()
        {
            let all_files =
                self.collect_files_from_manifest_entries(&entries, package_root, resource_type);
            let manifest_patterns: Vec<String> = entries
                .iter()
                .filter(|entry| is_override_pattern(entry))
                .cloned()
                .collect();
            let enabled_by_manifest = if manifest_patterns.is_empty() {
                all_files.iter().cloned().collect()
            } else {
                apply_patterns(&all_files, &manifest_patterns, package_root)
            };
            let all_files: Vec<String> = enabled_by_manifest.iter().cloned().collect();
            return (all_files, enabled_by_manifest);
        }

        let convention_dir = join(&[package_root, resource_type.as_str()]);
        if !path_exists(&convention_dir) {
            return (Vec::new(), indexmap::IndexSet::new());
        }
        let all_files = collect_resource_files(&convention_dir, resource_type);
        let enabled_by_manifest = all_files.iter().cloned().collect();
        (all_files, enabled_by_manifest)
    }

    /// `addManifestEntries(entries, root, type, target, metadata)`。
    ///
    /// `None`（字段缺省）时什么都不做；`Some(&[])` 也走同一路径，结果为空。非覆盖模式的
    /// 条目先展开成候选文件，再用条目里的覆盖模式求启用集合；只有启用的文件才会写入，
    /// 且 `enabled` 恒为 true —— 与 `apply_package_filter` 不同，这里不记录被禁用的文件。
    fn add_manifest_entries(
        &self,
        entries: Option<&[String]>,
        root: &str,
        resource_type: ResourceType,
        target: &mut IndexMap<String, AddedResource>,
        metadata: &PathMetadata,
    ) {
        let Some(entries) = entries else {
            return;
        };
        let all_files = self.collect_files_from_manifest_entries(entries, root, resource_type);
        let patterns: Vec<String> = entries
            .iter()
            .filter(|entry| is_override_pattern(entry))
            .cloned()
            .collect();
        let enabled_paths = apply_patterns(&all_files, &patterns, root);
        for file in &all_files {
            if enabled_paths.contains(file) {
                add_resource(target, file, metadata, true);
            }
        }
    }

    /// `collectFilesFromManifestEntries(entries, root, type)`。
    ///
    /// 只把非覆盖模式的条目当作路径（`!` / `+` / `-` 开头的是过滤指令）。非 glob 条目
    /// 直接 `resolve_join(root, entry)`：纯词法拼接，不检查存在性；glob 条目交给
    /// `expand_package_glob`，展开失败（`Err`）被静默忽略。最后统一交给
    /// `collect_files_from_paths` 过滤不存在的路径。
    ///
    /// 返回顺序是条目顺序、每个 glob 的匹配按 JS 字符串序；不同条目指向同一文件时不去重。
    fn collect_files_from_manifest_entries(
        &self,
        entries: &[String],
        root: &str,
        resource_type: ResourceType,
    ) -> Vec<String> {
        let source_entries: Vec<&String> = entries
            .iter()
            .filter(|entry| !is_override_pattern(entry))
            .collect();
        let mut resolved: Vec<String> = Vec::new();
        for entry in source_entries {
            if !has_glob_pattern(entry) {
                resolved.push(resolve_join(&[root, entry]));
            } else if let Ok(matches) = expand_package_glob(entry, root) {
                resolved.extend(matches);
            }
        }
        self.collect_files_from_paths(&resolved, resource_type)
    }

    /// `resolveLocalEntries(entries, type, target, metadata, baseDir)`。
    ///
    /// settings 顶层字符串列表的入口（`mod.rs` 对每种类型先 project、后 user 调用）。
    /// 空列表直接返回，不读取任何文件。普通路径按 `baseDir` 词法解析（含 `~` 展开与
    /// trim，见 `resolve_path_from_base`），目录展开成资源文件；模式只用于计算启用集合，
    /// 不会新增候选文件。
    ///
    /// 注意：`split_patterns` 把含 `*` / `?` 的 glob 条目也归入模式，因此顶层 settings
    /// 里的 glob 不会自己展开成文件，而是过滤普通条目带来的候选。
    ///
    /// 所有候选文件都会写入：命中模式的 `enabled = true`，未命中的 `enabled = false`，
    /// 因此它们仍占据首次写入并出现在最终结果里。
    pub(crate) fn resolve_local_entries(
        &self,
        entries: &[String],
        resource_type: ResourceType,
        accumulator: &mut ResourceAccumulator,
        metadata: &PathMetadata,
        base_dir: &str,
    ) {
        if entries.is_empty() {
            return;
        }
        let (plain, patterns) = split_patterns(entries);
        let resolved_plain: Vec<String> = plain
            .iter()
            .map(|entry| self.resolve_path_from_base(entry, base_dir))
            .collect();
        let all_files = self.collect_files_from_paths(&resolved_plain, resource_type);
        let enabled_paths = apply_patterns(&all_files, &patterns, base_dir);
        let target = accumulator.target_map(resource_type);
        for file in &all_files {
            add_resource(target, file, metadata, enabled_paths.contains(file));
        }
    }

    /// `addAutoDiscoveredResources(...)`：**顺序即优先级**，逐条对齐 TS。
    ///
    /// 写入顺序（也用于同 rank 时的稳定排序）：
    /// project extensions → project skills → project `.agents/skills` → project prompts →
    /// project themes → user extensions → user skills → user `.agents/skills` → user prompts
    /// → user themes。项目部分全部受 `is_project_trusted()` 门控；用户部分总是加载。
    ///
    /// 具体步骤：
    /// 1. 构造 user / project 两份 `PathMetadata`（`source = "auto"`，`origin = TopLevel`），
    ///    `.agents/skills` 分支再派生一份 `base_dir` 为 `.agents` 目录的 metadata。
    /// 2. 按 `ResourceType::ALL` 的顺序读取两侧 settings 的字符串列表，得到与类型下标一一
    ///    对应的 overrides；列表形状非法时返回 `Err`（`settings_string_list`）。
    /// 3. 计算四类约定目录 `<base>/<type>`、user 的 `~/.agents/skills`，以及（仅受信时）
    ///    从 `cwd` 向上的 `.agents/skills` 祖先目录，并用 `path_resolve` 词法比较排除与
    ///    user 目录等价的项。
    /// 4. 每条路径的 `enabled` 由 `is_enabled_by_overrides` 计算：只有 `!` / `+` / `-`
    ///    覆盖模式参与判定，普通条目与 glob 在这里不影响结果。
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn add_auto_discovered_resources(
        &self,
        accumulator: &mut ResourceAccumulator,
        global_settings: &kiana_config::settings::Settings,
        project_settings: &kiana_config::settings::Settings,
        global_base_dir: &str,
        project_base_dir: &str,
    ) -> Result<(), PackageManagerError> {
        let user_metadata = PathMetadata {
            source: "auto".to_string(),
            scope: SourceScope::User,
            origin: SourceOrigin::TopLevel,
            base_dir: Some(global_base_dir.to_string()),
            package_root: None,
        };
        let project_metadata = PathMetadata {
            source: "auto".to_string(),
            scope: SourceScope::Project,
            origin: SourceOrigin::TopLevel,
            base_dir: Some(project_base_dir.to_string()),
            package_root: None,
        };

        let mut user_overrides: Vec<Vec<String>> = Vec::new();
        let mut project_overrides: Vec<Vec<String>> = Vec::new();
        for resource_type in ResourceType::ALL {
            user_overrides.push(settings_string_list(
                global_settings,
                resource_type.as_str(),
            )?);
            project_overrides.push(settings_string_list(
                project_settings,
                resource_type.as_str(),
            )?);
        }

        let user_dirs: Vec<String> = ResourceType::ALL
            .iter()
            .map(|resource_type| join(&[global_base_dir, resource_type.as_str()]))
            .collect();
        let project_dirs: Vec<String> = ResourceType::ALL
            .iter()
            .map(|resource_type| join(&[project_base_dir, resource_type.as_str()]))
            .collect();
        let user_agents_skills_dir = join(&[&get_home_dir(), ".agents", "skills"]);
        let project_trusted = self.settings_manager.is_project_trusted();
        let project_agents_skill_dirs: Vec<String> = if project_trusted {
            collect_ancestor_agents_skill_dirs(&self.cwd)
                .into_iter()
                .filter(|dir| path_resolve(&[dir]) != path_resolve(&[&user_agents_skills_dir]))
                .collect()
        } else {
            Vec::new()
        };

        // 项目作用域（受 trust 门禁）。
        if project_trusted {
            let paths = collect_auto_extension_entries(&project_dirs[0]);
            for path in paths {
                let enabled =
                    is_enabled_by_overrides(&path, &project_overrides[0], project_base_dir);
                add_resource(
                    accumulator.target_map(ResourceType::Extensions),
                    &path,
                    &project_metadata,
                    enabled,
                );
            }
            let paths = collect_auto_skill_entries(&project_dirs[1], SkillDiscoveryMode::Kiana);
            for path in paths {
                let enabled =
                    is_enabled_by_overrides(&path, &project_overrides[1], project_base_dir);
                add_resource(
                    accumulator.target_map(ResourceType::Skills),
                    &path,
                    &project_metadata,
                    enabled,
                );
            }
        }

        // 项目 `.agents/skills`（每个目录用自己的 baseDir；trust 未定时列表为空）。
        for agents_skills_dir in &project_agents_skill_dirs {
            let agents_base_dir = dirname(agents_skills_dir);
            let agents_metadata = PathMetadata {
                base_dir: Some(agents_base_dir.clone()),
                ..project_metadata.clone()
            };
            let paths = collect_auto_skill_entries(agents_skills_dir, SkillDiscoveryMode::Agents);
            for path in paths {
                let enabled =
                    is_enabled_by_overrides(&path, &project_overrides[1], &agents_base_dir);
                add_resource(
                    accumulator.target_map(ResourceType::Skills),
                    &path,
                    &agents_metadata,
                    enabled,
                );
            }
        }

        if project_trusted {
            let paths = collect_auto_prompt_entries(&project_dirs[2]);
            for path in paths {
                let enabled =
                    is_enabled_by_overrides(&path, &project_overrides[2], project_base_dir);
                add_resource(
                    accumulator.target_map(ResourceType::Prompts),
                    &path,
                    &project_metadata,
                    enabled,
                );
            }
            let paths = collect_auto_theme_entries(&project_dirs[3]);
            for path in paths {
                let enabled =
                    is_enabled_by_overrides(&path, &project_overrides[3], project_base_dir);
                add_resource(
                    accumulator.target_map(ResourceType::Themes),
                    &path,
                    &project_metadata,
                    enabled,
                );
            }
        }

        // 用户作用域（总是加载）。
        let paths = collect_auto_extension_entries(&user_dirs[0]);
        for path in paths {
            let enabled = is_enabled_by_overrides(&path, &user_overrides[0], global_base_dir);
            add_resource(
                accumulator.target_map(ResourceType::Extensions),
                &path,
                &user_metadata,
                enabled,
            );
        }

        let paths = collect_auto_skill_entries(&user_dirs[1], SkillDiscoveryMode::Kiana);
        for path in paths {
            let enabled = is_enabled_by_overrides(&path, &user_overrides[1], global_base_dir);
            add_resource(
                accumulator.target_map(ResourceType::Skills),
                &path,
                &user_metadata,
                enabled,
            );
        }

        let user_agents_base_dir = dirname(&user_agents_skills_dir);
        let user_agents_metadata = PathMetadata {
            base_dir: Some(user_agents_base_dir.clone()),
            ..user_metadata.clone()
        };
        let paths = collect_auto_skill_entries(&user_agents_skills_dir, SkillDiscoveryMode::Agents);
        for path in paths {
            let enabled = is_enabled_by_overrides(&path, &user_overrides[1], &user_agents_base_dir);
            add_resource(
                accumulator.target_map(ResourceType::Skills),
                &path,
                &user_agents_metadata,
                enabled,
            );
        }

        let paths = collect_auto_prompt_entries(&user_dirs[2]);
        for path in paths {
            let enabled = is_enabled_by_overrides(&path, &user_overrides[2], global_base_dir);
            add_resource(
                accumulator.target_map(ResourceType::Prompts),
                &path,
                &user_metadata,
                enabled,
            );
        }
        let paths = collect_auto_theme_entries(&user_dirs[3]);
        for path in paths {
            let enabled = is_enabled_by_overrides(&path, &user_overrides[3], global_base_dir);
            add_resource(
                accumulator.target_map(ResourceType::Themes),
                &path,
                &user_metadata,
                enabled,
            );
        }
        Ok(())
    }

    /// `collectFilesFromPaths(paths, type)`：文件直接收，目录走 `collectResourceFiles`。
    ///
    /// 不存在或 `metadata` 失败的路径静默跳过；返回顺序沿用输入顺序与目录内遍历顺序，
    /// 不去重，也不保证路径已规范化（调用方随后可能用 `add_resource` 的字符串键比较）。
    pub(crate) fn collect_files_from_paths(
        &self,
        paths: &[String],
        resource_type: ResourceType,
    ) -> Vec<String> {
        let mut files = Vec::new();
        for path in paths {
            if !path_exists(path) {
                continue;
            }
            let Ok(stats) = std::fs::metadata(path) else {
                continue;
            };
            if stats.is_file() {
                files.push(path.clone());
            } else if stats.is_dir() {
                files.extend(collect_resource_files(path, resource_type));
            }
        }
        files
    }

    /// 内置扩展的 enabled 计算（`resolve()` 尾部）。
    ///
    /// 先看项目 settings 的 `extensions`：只用其中的覆盖模式在“单路径”上求值，项目侧
    /// 命中时直接采用该结果；未命中（`None`）才回退到全局 settings 的同样判定。
    ///
    /// 返回 `(最终 enabled, 项目侧判定)`。第二个值 `Some(_)` 表示项目 settings 对该路径
    /// 有覆盖，`None` 表示没有；调用方用它在 `PathMetadata.scope` 上区分 project / user。
    /// 与自动发现一样，普通条目与 glob 不参与这里的启用判定，只有 `!` / `+` / `-` 生效。
    pub(crate) fn builtin_extension_enabled(
        &self,
        path: &str,
        project_settings: &kiana_config::settings::Settings,
        global_settings: &kiana_config::settings::Settings,
        project_base_dir: &str,
        global_base_dir: &str,
    ) -> Result<(bool, Option<bool>), PackageManagerError> {
        let project_patterns = settings_string_list(project_settings, "extensions")?;
        let project_enabled = apply_autoload_disabled_patterns(
            &[path.to_string()],
            &get_override_patterns(&project_patterns),
            project_base_dir,
        )
        .get(path)
        .copied();
        let enabled = match project_enabled {
            Some(value) => value,
            None => is_enabled_by_overrides(
                path,
                &settings_string_list(global_settings, "extensions")?,
                global_base_dir,
            ),
        };
        Ok((enabled, project_enabled))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_resource_type_error_is_verbatim() {
        assert_eq!(
            ResourceType::from_name("bogus")
                .expect_err("unknown")
                .message(),
            "Unknown resource type: bogus"
        );
    }

    #[test]
    fn add_resource_keeps_the_first_write() {
        let mut map = IndexMap::new();
        let first = PathMetadata {
            source: "local".into(),
            scope: SourceScope::User,
            origin: SourceOrigin::TopLevel,
            base_dir: Some("/a".into()),
            package_root: None,
        };
        let second = PathMetadata {
            source: "auto".into(),
            ..first.clone()
        };
        add_resource(&mut map, "/x", &first, true);
        add_resource(&mut map, "/x", &second, false);
        assert_eq!(map.get("/x").expect("entry").metadata.source, "local");
        assert!(map.get("/x").expect("entry").enabled);
        add_resource(&mut map, "", &first, true);
        assert_eq!(map.len(), 1);
    }

    #[test]
    fn map_to_resolved_sorts_stably_and_dedupes_by_canonical_path() {
        let dir = tempfile::tempdir().expect("tempdir");
        let base = dir.path().to_string_lossy().into_owned();
        let first = join(&[&base, "a.ts"]);
        std::fs::write(&first, "").expect("write");
        let link = join(&[&base, "link.ts"]);
        #[cfg(unix)]
        std::os::unix::fs::symlink(&first, &link).expect("symlink");

        let package_metadata = PathMetadata {
            source: "npm:x".into(),
            scope: SourceScope::User,
            origin: SourceOrigin::Package,
            base_dir: Some("/p".into()),
            package_root: Some("/p".into()),
        };
        let auto_metadata = PathMetadata {
            source: "auto".into(),
            scope: SourceScope::User,
            origin: SourceOrigin::TopLevel,
            base_dir: Some(base.clone()),
            package_root: None,
        };
        let mut accumulator = create_accumulator();
        add_resource(&mut accumulator.extensions, &first, &package_metadata, true);
        add_resource(&mut accumulator.extensions, &link, &auto_metadata, true);
        add_resource(&mut accumulator.extensions, &first, &auto_metadata, false);

        let resolved = to_resolved_paths(&accumulator);
        // auto (rank 3) 排在 package (rank 4) 前面；第一条 auto 是 symlink，canonical 相同，
        // 因此去重保留 link（auto），丢掉 package 的 a.ts。
        assert_eq!(resolved.extensions.len(), 1);
        assert_eq!(resolved.extensions[0].path, link);
        assert!(resolved.extensions[0].enabled);
        assert!(resolved.install_failures.is_none());
    }
}
