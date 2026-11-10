//! `interactive-mode.ts` 的资源列表显示（§2.50.21）。
//!
//! 逐字移植 `showLoadedResources` 与它的路径 / 来源分组辅助；所有 `localeCompare`
//! 统一走 [`kiana_tui::autocomplete::locale_compare`]。
//!
//! # 这个文件负责什么
//!
//! 交互模式在 header 与聊天区之间挂了一个独立的 `loaded_resources_container`
//! （见 `InteractiveMode` 构造器）。本文件把资源加载器的快照——上下文文件、技能、
//! prompt 模板、扩展，以及技能/prompt/扩展/主题的冲突与加载诊断——渲染成若干可折叠
//! 小节写进该容器。没有它，用户看不到本次会话实际加载了哪些资源，资源冲突只能靠日志
//! 排查。
//!
//! 它不是实时视图：`show_loaded_resources` 每次调用先清空容器再整体重建。生产代码里
//! 只有两个调用点——扩展初始化完成后（`interactive_mode/session.rs`）与会话 reload
//! 之后（`interactive_mode/reload.rs`），两次都传 `force = false, show_diagnostics_when_quiet = true`。
//! 之后的展开/折叠由 `set_tools_expanded` 直接遍历容器里的 `ExpandableText`，不重建。
//!
//! # 一次调用的流程
//!
//! 1. 取两个开关：`show_listing`（`force` 或 `should_show_startup_details()`）与
//!    `show_diagnostics`（列表可见，或调用方要求静默时也显示诊断）。
//! 2. 无论是否显示都先 `clear()`，避免上一次调用的条目残留。
//! 3. 从 `session.resource_loader()` 取技能、prompt（这一份只用于诊断）、主题、扩展四份
//!    克隆快照；列表里的 prompt 模板另取自 `session.prompt_templates()`。扩展先过滤掉
//!    `hidden == Some(true)` 的条目。
//! 4. 建立“路径 → 来源信息”索引，供诊断行的来源标签使用。
//! 5. 列表可见时按 `Context`、`Skills`、`Prompts`、`Extensions` 顺序追加小节；诊断可见时
//!    按 `Skill conflicts`、`Prompt conflicts`、`Extension issues`、`Theme conflicts`
//!    顺序追加。空诊断列表不产生小节。
//!
//! 小节内容放在 `ExpandableText` / `ThemedText` 的闭包里，**渲染期才求值**：颜色、
//! 初始展开状态（`verbose` 或 `tool_output_expanded`）与文本都在 `Container::render`
//! 时读取。因此 `show_loaded_resources` 的构建阶段不读颜色；真正渲染前必须初始化主题，
//! 否则 `fg` 会 panic。
//!
//! # 闭包与所有权
//!
//! 各小节的正文是 `Rc<dyn Fn() -> String>`，在 `Container::render` 时才调用，所以闭包必须
//! 是 `'static`，也不能在渲染期借用 `InteractiveMode.inner`。需要访问 `self` 的小节把
//! `InteractiveMode` 克隆进闭包：它只是 `Rc<RefCell<InteractiveModeInner>>` 的薄包装并
//! `derive(Clone)`，克隆只增加引用计数，不复制会话状态。同一小节有多处正文时（例如
//! Skills 的折叠/展开）各自持有一个克隆，避免闭包之间的所有权纠缠。
//!
//! # 测试接缝
//!
//! 文件末尾的 `#[doc(hidden)] impl InteractiveMode` 是给集成测试用的：它把本文件的私有
//! 辅助函数原样转发出去，让 `tests/interactive_mode_resources.rs` 能直接断言紧凑标签与
//! 诊断格式。这些方法不属于 TS 的公开面，不应在生产路径调用。
//!
//! # 重要约束
//!
//! - 输出文本是 TS 快照契约：小节顺序、组顺序（`project`、`user`、`path`）、诊断分组
//!   顺序与空格缩进都被 `tests/interactive_mode_resources.rs` 整串断言；改格式会直接
//!   让这些快照失败。
//! - 闭包在渲染期执行，不能调用任何会借用 `InteractiveMode.inner` 的方法。Context 小节
//!   的紧凑标签因此在构建期提前算好（见函数内注释）。
//! - 所有 `localeCompare` 排序都走 [`kiana_tui::autocomplete::locale_compare`]，以复刻
//!   TS 的本地化排序；它读进程 locale，未建立 collator 时回退字节序。

use std::rc::Rc;

use indexmap::IndexMap;
use kiana_core::diagnostics::{DiagnosticKind, ResourceDiagnostic};
use kiana_core::source_info::{SourceInfo, SourceScope};
use kiana_theme::theme_tokens::ThemeColor;
use kiana_tui::autocomplete::locale_compare;
use kiana_tui::components::spacer::Spacer;
use kiana_tui::tui::Component;
use kiana_ui_kit::themed_text::ThemedText;

use super::{ExpandableText, InteractiveMode};

/// `getLoadedResources` 使用的 `{ path, sourceInfo? }`。
///
/// 仅在本文件内构造：`path` 是展示用路径（可能是合成路径或 Windows 路径），
/// `source_info` 为 `None` 表示来源未知，此时路径辅助函数走不依赖来源的分支。列表构建
/// 后被克隆进各小节闭包，所以本类型按值传递、字段不再变化。
#[derive(Debug, Clone)]
pub(crate) struct ResourceItem {
    /// 展示路径（扩展用 `Extension.path`，技能/prompt 用各自的 `file_path`）。
    path: String,
    /// 来源信息；`None` 时无法判断是否为 npm/git 包来源。
    source_info: Option<SourceInfo>,
}

/// `getDisplaySourceInfo` 的返回值。
///
/// `label` 是来源标签（`user`、`project`、`path` 或原始 `source` 字符串），`scope_label`
/// 是需要括号补充的限定（`user`、`project`、`temp`），`color` 是 TS 表格里的颜色意图。
/// 本文件目前只用前两个字段拼 `format_path_with_source` 的文本，`color` 没有读取点
/// （保留该字段以对应 TS 的返回结构）。
#[derive(Debug, Clone)]
struct DisplaySourceInfo {
    /// 主标签。
    label: String,
    /// 括号内的 scope 限定。
    scope_label: Option<String>,
    /// 颜色意图（当前未被消费）。
    color: ThemeColor,
}

/// `buildScopeGroups` 的组。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ScopeKey {
    /// 用户级资源。
    User,
    /// 项目级资源。
    Project,
    /// 无 scope 或临时资源（`cli` 来源一律归此组）。
    Path,
}

impl ScopeKey {
    /// TS 分组的字面量；直接作为组标题打印。
    fn as_str(self) -> &'static str {
        match self {
            ScopeKey::User => "user",
            ScopeKey::Project => "project",
            ScopeKey::Path => "path",
        }
    }
}

/// `buildScopeGroups` 的一个组。
struct ScopeGroup {
    /// 组名（打印为标题）。
    scope: ScopeKey,
    /// 非包来源的条目，保留输入顺序（渲染时才排序）。
    paths: Vec<ResourceItem>,
    /// 包来源的条目，键是原始 `source` 字符串（如 `npm:foo`）；`IndexMap` 保留首次出现
    /// 顺序，渲染时再按 `locale_compare` 排序。
    packages: IndexMap<String, Vec<ResourceItem>>,
}

/// 用全局主题给 `text` 着色（等价 TS `theme.fg(color, text)`）。
///
/// 读的是**当前**全局主题，因此只有在渲染闭包里调用才能跟随主题切换；主题未初始化时
/// `THEME.fg` 返回错误，这里 `expect` panic（与 TS 在同样位置直接抛错一致）。
fn fg(color: ThemeColor, text: &str) -> String {
    kiana_theme::THEME
        .fg(color, text)
        .expect("theme must be initialized before rendering (TS throws here)")
}

/// `path.posix.parse(p)` 的 `{ dir, name }`（本模块只用到这两个字段）。
///
/// 只按最后一个 `/` 切分，不做其它归一化。`name` 只剥「既非开头也非结尾」的最后一个
/// `.` 之后的内容：`a.tar.gz` → `a.tar`；`.foo` 与 `foo.` 因点落在开头/结尾，整段保留。
/// 结尾带点时这里与 Node 不一致——`path.posix.parse('foo.')` 的 `ext` 是 `.`、`name`
/// 是 `foo`——属移植分歧（待核实）。没有 `/` 时 `dir` 返回空串，与 Node `parse` 一致；
/// 唯一调用点把空串与 `.` 同等对待，因此可见输出不受影响。
fn posix_parse(path: &str) -> (String, String) {
    let (dir, base) = match path.rfind('/') {
        Some(index) => (path[..index].to_string(), &path[index + 1..]),
        None => (String::new(), path),
    };
    let name = match base.rfind('.') {
        Some(index) if index > 0 && index + 1 < base.len() => base[..index].to_string(),
        _ => base.to_string(),
    };
    (dir, name)
}

impl InteractiveMode {
    /// `isPackageSource(sourceInfo)`。
    ///
    /// 行为完全委托 `is_package_source`：只看 `source` 是否以 `npm:` / `git:` 开头，
    /// `None` 视为非包来源。当前生产代码与测试均无调用点，仅为与 TS `isPackageSource`
    /// 对应的入口保留。
    pub(crate) fn is_package_source_info(&self, source_info: Option<&SourceInfo>) -> bool {
        self.is_package_source(source_info)
    }

    /// `getCompactPathLabel(resourcePath, sourceInfo)`。
    ///
    /// 先 `get_short_path`，把 `\` 统一成 `/` 后按 `/` 分段，去掉空段与单独的 `~`
    /// （home 被缩短后残留的段），取最后一段。没有任何可用段时（例如路径就是 `/` 或
    /// `~`）原样返回 `get_short_path` 的结果——注意此时结果里可能仍带 `\`。
    pub(crate) fn get_compact_path_label(
        &self,
        resource_path: &str,
        source_info: Option<&SourceInfo>,
    ) -> String {
        let short_path = self.get_short_path(resource_path, source_info);
        let normalized = short_path.replace('\\', "/");
        let segments: Vec<&str> = normalized
            .split('/')
            .filter(|segment| !segment.is_empty() && *segment != "~")
            .collect();
        match segments.last() {
            Some(segment) => (*segment).to_string(),
            None => short_path,
        }
    }

    /// `getCompactPackageSourceLabel(sourceInfo)`。
    ///
    /// 只在调用方已确认 `is_package_source` 时使用：
    /// - `npm:pkg` → `pkg`；`npm:` 后为空时保留原串 `npm:`；
    /// - 能被 `kiana_core::git::parse_git_url` 解析的 `git:` 来源 → 该 URL 的 `path` 段
    ///   （host/repo）；解析失败或 `path` 为空时回退整个 `source`；
    /// - 其它情况原样返回。
    pub(crate) fn get_compact_package_source_label(
        &self,
        source_info: Option<&SourceInfo>,
    ) -> String {
        let source = source_info
            .map(|info| info.source.as_str())
            .unwrap_or_default();
        if let Some(rest) = source.strip_prefix("npm:") {
            return if rest.is_empty() {
                source.to_string()
            } else {
                rest.to_string()
            };
        }
        if let Some(git_source) = kiana_core::git::parse_git_url(source) {
            return if git_source.path.is_empty() {
                source.to_string()
            } else {
                git_source.path
            };
        }
        source.to_string()
    }

    /// `getCompactExtensionLabel(resourcePath, sourceInfo)`。
    ///
    /// 非包来源直接走 `get_compact_path_label`。包来源分三步：
    /// 1. 用 `get_compact_package_source_label` 得到来源标签（空标签时退回路径标签）；
    /// 2. 取短路径并只剥掉**开头**的 `extensions/`（兄弟包等落在 `base_dir` 之外的路径
    ///    没有这个前缀，会保留 `../`）；
    /// 3. `posix_parse` 后：文件名是 `index` → 目录为空或 `.` 时只输出源标签，否则输出
    ///    `来源:目录`；其它文件 → `来源:完整相对路径`（保留扩展名）。
    pub(crate) fn get_compact_extension_label(
        &self,
        resource_path: &str,
        source_info: Option<&SourceInfo>,
    ) -> String {
        if !self.is_package_source(source_info) {
            return self.get_compact_path_label(resource_path, source_info);
        }
        let source_label = self.get_compact_package_source_label(source_info);
        if source_label.is_empty() {
            return self.get_compact_path_label(resource_path, source_info);
        }
        let short_path = self
            .get_short_path(resource_path, source_info)
            .replace('\\', "/");
        let package_path = short_path
            .strip_prefix("extensions/")
            .map(str::to_string)
            .unwrap_or(short_path);
        let (dir, name) = posix_parse(&package_path);
        if name == "index" {
            if dir.is_empty() || dir == "." {
                return source_label;
            }
            return format!("{source_label}:{dir}");
        }
        format!("{source_label}:{package_path}")
    }

    /// `getCompactDisplayPathSegments(resourcePath)`。
    ///
    /// `format_display_path`（home → `~`）之后按 `/` 切段，丢弃空段与单独的 `~` 段。
    /// 这里**不**剥离 `index.ts`/`index.js`，需要时由
    /// [`Self::get_compact_extension_labels`] 在调用后 `pop`。
    pub(crate) fn get_compact_display_path_segments(&self, resource_path: &str) -> Vec<String> {
        self.format_display_path(resource_path)
            .replace('\\', "/")
            .split('/')
            .filter(|segment| !segment.is_empty() && *segment != "~")
            .map(str::to_string)
            .collect()
    }

    /// `getCompactNonPackageExtensionLabel(resourcePath, index, allPaths)`。
    ///
    /// 为非包扩展选一个“在同类条目中唯一”的最短路径后缀：
    /// - `index` 是该扩展在 `all_paths`（由 [`Self::get_compact_extension_labels`] 构造）
    ///   中的位置，比较时跳过自身；
    /// - 从 1 段后缀开始逐级加长，要求所有其它条目的“同长度后缀”都不等于候选；其它条目
    ///   不足 `segment_count` 段时用其完整路径参与比较（段数不同，拼接结果必然不同）；
    /// - 找到第一个唯一候选即返回该后缀；全部冲突时返回所有段拼接的路径（无前导 `/`，
    ///   单独的 `~` 段已在构造 segments 时丢弃）。
    ///
    /// `index` 越界或对应条目没有 segment 时，退回 `get_compact_path_label(path, None)`
    /// （不读来源信息）。
    pub(crate) fn get_compact_non_package_extension_label(
        &self,
        resource_path: &str,
        index: usize,
        all_paths: &[(String, Option<SourceInfo>, Vec<String>)],
    ) -> String {
        let Some((_, _, segments)) = all_paths.get(index) else {
            return self.get_compact_path_label(resource_path, None);
        };
        if segments.is_empty() {
            return self.get_compact_path_label(resource_path, None);
        }
        for segment_count in 1..=segments.len() {
            let candidate = segments[segments.len() - segment_count..].join("/");
            let unique = all_paths.iter().enumerate().all(|(item_index, item)| {
                if item_index == index {
                    return true;
                }
                let other = if item.2.len() >= segment_count {
                    item.2[item.2.len() - segment_count..].join("/")
                } else {
                    item.2.join("/")
                };
                other != candidate
            });
            if unique {
                return candidate;
            }
        }
        segments.join("/")
    }

    /// `getCompactExtensionLabels(extensions)`。
    ///
    /// 输出与输入等长、同序。先为**非包**扩展建一张表：segments 由
    /// [`Self::get_compact_display_path_segments`] 得到，末段是 `index.ts`/`index.js` 且
    /// 段数大于 1 时去掉末段（`plan-mode/index.ts` 显示为 `plan-mode`）。然后逐项处理：
    /// - 包来源 → `get_compact_extension_label`；
    /// - 非包 → 在表里按 `path` 找第一个相同项，用
    ///   `get_compact_non_package_extension_label` 消歧；
    /// - 找不到（防御分支）→ `get_compact_path_label`。
    ///
    /// 重复路径会命中同一个表项、得到相同标签：但表里仍保留重复项本身，消歧时把它当作
    /// 冲突，结果两者都退回 `segments.join("/")`（去掉前导 `/`、已剥离 index 入口的整条
    /// 段路径）。
    pub(crate) fn get_compact_extension_labels(&self, extensions: &[ResourceItem]) -> Vec<String> {
        let non_package_extensions: Vec<(String, Option<SourceInfo>, Vec<String>)> = extensions
            .iter()
            .filter(|extension| !self.is_package_source(extension.source_info.as_ref()))
            .map(|extension| {
                let mut segments = self.get_compact_display_path_segments(&extension.path);
                // `index.ts` / `index.js` 是目录扩展的入口文件：去掉入口名，用目录名作为
                // 候选标签。只有一级段时保留原样（否则会变成空标签）。
                let last_is_index = segments
                    .last()
                    .is_some_and(|last| last == "index.ts" || last == "index.js");
                if segments.len() > 1 && last_is_index {
                    segments.pop();
                }
                (
                    extension.path.clone(),
                    extension.source_info.clone(),
                    segments,
                )
            })
            .collect();

        extensions
            .iter()
            .map(|extension| {
                if self.is_package_source(extension.source_info.as_ref()) {
                    return self.get_compact_extension_label(
                        &extension.path,
                        extension.source_info.as_ref(),
                    );
                }
                // 表与 `extensions` 的过滤条件相同，正常情况下一定能命中；按下标而不是
                // 按引用比较，是因为条目在 `ResourceItem` 里是值而不是共享对象。
                let non_package_index = non_package_extensions
                    .iter()
                    .position(|(path, _, _)| path == &extension.path);
                match non_package_index {
                    Some(index) => self.get_compact_non_package_extension_label(
                        &extension.path,
                        index,
                        &non_package_extensions,
                    ),
                    None => {
                        self.get_compact_path_label(&extension.path, extension.source_info.as_ref())
                    }
                }
            })
            .collect()
    }

    /// `getDisplaySourceInfo(sourceInfo)`。
    ///
    /// 把来源信息映射成诊断行使用的标签：
    /// - `source_info` 为 `None` 时按 `source = "local"` 处理；
    /// - `local`：`user`/`project` 直接作为标签；`temporary` → 标签 `path` 加限定
    ///   `temp`；无 scope → 标签 `path`；
    /// - `cli`：标签固定 `path`，只有 `temporary` 时附 `temp`；
    /// - 其它（`npm:` / `git:` / 扩展自定义）：标签就是原始 `source`，scope 依次映射为
    ///   `user`/`project`/`temp`，颜色用 `Accent`。
    ///
    /// 颜色字段在本文件内没有读取点（见结构体注释）。
    fn get_display_source_info(&self, source_info: Option<&SourceInfo>) -> DisplaySourceInfo {
        let source = source_info
            .map(|info| info.source.as_str())
            .unwrap_or("local");
        let scope = source_info.map(|info| info.scope);
        if source == "local" {
            return match scope {
                Some(SourceScope::User) => DisplaySourceInfo {
                    label: "user".to_string(),
                    scope_label: None,
                    color: ThemeColor::Muted,
                },
                Some(SourceScope::Project) => DisplaySourceInfo {
                    label: "project".to_string(),
                    scope_label: None,
                    color: ThemeColor::Muted,
                },
                Some(SourceScope::Temporary) => DisplaySourceInfo {
                    label: "path".to_string(),
                    scope_label: Some("temp".to_string()),
                    color: ThemeColor::Muted,
                },
                None => DisplaySourceInfo {
                    label: "path".to_string(),
                    scope_label: None,
                    color: ThemeColor::Muted,
                },
            };
        }
        if source == "cli" {
            return DisplaySourceInfo {
                label: "path".to_string(),
                scope_label: matches!(scope, Some(SourceScope::Temporary))
                    .then(|| "temp".to_string()),
                color: ThemeColor::Muted,
            };
        }
        let scope_label = match scope {
            Some(SourceScope::User) => Some("user".to_string()),
            Some(SourceScope::Project) => Some("project".to_string()),
            Some(SourceScope::Temporary) => Some("temp".to_string()),
            None => None,
        };
        DisplaySourceInfo {
            label: source.to_string(),
            scope_label,
            color: ThemeColor::Accent,
        }
    }

    /// `getScopeGroup(sourceInfo)`。
    ///
    /// `cli` 或 `temporary` 一律归 `path`（先于 scope 判断，`cli` 即使带 user/project
    /// scope 也进 `path`）；否则按 scope 分到 `user` / `project`；scope 缺失时归 `path`。
    fn get_scope_group_key(&self, source_info: Option<&SourceInfo>) -> ScopeKey {
        let source = source_info
            .map(|info| info.source.as_str())
            .unwrap_or("local");
        let scope = source_info.map(|info| info.scope);
        if source == "cli" || scope == Some(SourceScope::Temporary) {
            return ScopeKey::Path;
        }
        match scope {
            Some(SourceScope::User) => ScopeKey::User,
            Some(SourceScope::Project) => ScopeKey::Project,
            _ => ScopeKey::Path,
        }
    }

    /// `buildScopeGroups(items)`：固定返回 `[project, user, path]`，过滤空组。
    ///
    /// 分类依据是 [`Self::is_package_source`]：包来源按原始 `source` 字符串放进
    /// `packages`（`IndexMap` 保留首次出现顺序），非包来源按输入顺序进 `paths`。这里不做
    /// 任何排序；路径与包名的排序发生在 [`Self::format_scope_groups`]，即渲染期。
    fn build_scope_groups(&self, items: &[ResourceItem]) -> Vec<ScopeGroup> {
        let mut groups = [
            ScopeGroup {
                scope: ScopeKey::Project,
                paths: Vec::new(),
                packages: IndexMap::new(),
            },
            ScopeGroup {
                scope: ScopeKey::User,
                paths: Vec::new(),
                packages: IndexMap::new(),
            },
            ScopeGroup {
                scope: ScopeKey::Path,
                paths: Vec::new(),
                packages: IndexMap::new(),
            },
        ];
        // 组数组的顺序（project → user → path）就是打印顺序；条目只做分类，不排序。
        for item in items {
            let key = self.get_scope_group_key(item.source_info.as_ref());
            let group = groups
                .iter_mut()
                .find(|group| group.scope == key)
                .expect("all three scopes exist");
            let source = item
                .source_info
                .as_ref()
                .map(|info| info.source.clone())
                .unwrap_or_else(|| "local".to_string());
            if self.is_package_source(item.source_info.as_ref()) {
                group.packages.entry(source).or_default().push(item.clone());
            } else {
                group.paths.push(item.clone());
            }
        }
        groups
            .into_iter()
            .filter(|group| !group.paths.is_empty() || !group.packages.is_empty())
            .collect()
    }

    /// `formatScopeGroups(groups, options)`。
    ///
    /// 展开态的共用格式器：组标题 `Accent`，直接路径 `Dim`，包名 `MdLink`，包内路径
    /// `Dim`。组内路径与包名都先按 [`locale_compare`] 排序，包内条目再按路径排序。
    /// 两个闭包参数决定“路径怎么显示”：技能用 `format_display_path`，prompt 用
    /// `/<name>`，扩展用 `format_extension_display_path`（及其与 `get_short_path` 的
    /// 组合）。调用方负责过滤空组；本函数对空组仍会打印组标题。返回以 `\n` 连接的行，
    /// 没有尾换行。
    fn format_scope_groups(
        &self,
        groups: &[ScopeGroup],
        format_path: impl Fn(&ResourceItem) -> String,
        format_package_path: impl Fn(&ResourceItem, &str) -> String,
    ) -> String {
        let mut lines: Vec<String> = Vec::new();
        for group in groups {
            lines.push(format!(
                "  {}",
                fg(ThemeColor::Accent, group.scope.as_str())
            ));

            let mut sorted_paths: Vec<&ResourceItem> = group.paths.iter().collect();
            sorted_paths.sort_by(|a, b| locale_compare(&a.path, &b.path));
            for item in sorted_paths {
                lines.push(fg(ThemeColor::Dim, &format!("    {}", format_path(item))));
            }

            let mut sorted_packages: Vec<(&String, &Vec<ResourceItem>)> =
                group.packages.iter().collect();
            sorted_packages.sort_by(|(a, _), (b, _)| locale_compare(a, b));
            for (source, items) in sorted_packages {
                lines.push(format!("    {}", fg(ThemeColor::MdLink, source)));
                let mut sorted_items: Vec<&ResourceItem> = items.iter().collect();
                sorted_items.sort_by(|a, b| locale_compare(&a.path, &b.path));
                for item in sorted_items {
                    lines.push(fg(
                        ThemeColor::Dim,
                        &format!("      {}", format_package_path(item, source)),
                    ));
                }
            }
        }
        lines.join("\n")
    }

    /// `findSourceInfoForPath(p, sourceInfos)`：精确命中，否则逐级向上找父目录。
    ///
    /// 只认 `/` 分隔符（Windows 反斜杠路径不会向上回退）。先对 `path` 原值做一次精确
    /// 查表；未命中后从最后一个 `/` 处逐级截断，空串也会查表（所以根路径 `/` 精确未命中
    /// 时只会再查一次空串）。命中返回克隆，未命中返回 `None`。
    /// `sourceInfos` 只收录扩展、技能、prompt 与“有 source path 的主题”，因此其它路径的
    /// 诊断行会走无来源的显示分支。
    fn find_source_info_for_path(
        &self,
        path: &str,
        source_infos: &IndexMap<String, SourceInfo>,
    ) -> Option<SourceInfo> {
        if let Some(exact) = source_infos.get(path) {
            return Some(exact.clone());
        }
        let mut current = path.to_string();
        while current.contains('/') {
            let index = current.rfind('/').expect("contains a slash");
            current.truncate(index);
            if let Some(parent) = source_infos.get(&current) {
                return Some(parent.clone());
            }
        }
        None
    }

    /// `formatPathWithSource(p, sourceInfo)`。
    ///
    /// 有来源时输出 `<label> (<scopeLabel>)? <shortPath>`（标签与路径之间一个空格，
    /// 路径经 `get_short_path` 缩短）；无来源时退回 `format_display_path`（home → `~`）。
    /// 本函数不添加颜色，着色由调用方决定。
    fn format_path_with_source(&self, path: &str, source_info: Option<&SourceInfo>) -> String {
        match source_info {
            Some(source_info) => {
                let short_path = self.get_short_path(path, Some(source_info));
                let display = self.get_display_source_info(Some(source_info));
                let label_text = match display.scope_label {
                    Some(scope_label) => format!("{} ({scope_label})", display.label),
                    None => display.label,
                };
                format!("{label_text} {short_path}")
            }
            None => self.format_display_path(path),
        }
    }

    /// `formatDiagnostics(diagnostics, sourceInfos)`。
    ///
    /// 分两段输出：
    /// 1. **冲突组**：`kind == Collision` 且带 `collision` 的诊断按 `collision.name`
    ///    首次出现顺序分组。每组先打印 `  "name" collision:`，再用组内**第一条**诊断的
    ///    `winner_path` 打印获胜行（`✓`，`Success` 色），随后为组内每条诊断的
    ///    `loser_path` 打印 `✗ … (skipped)`（包括第一条自己的 loser）。两条路径都用
    ///    `find_source_info_for_path` 补充来源标签；整行用 `Dim` 包裹。
    /// 2. **其它诊断**：`Error` 用错误色，其余（含 `Collision` 但缺 `collision` 字段的）
    ///    用警告色；有 `path` 时先打印路径行再打印缩进的消息，否则只打印消息。
    ///
    /// 冲突组内部保持输入顺序；组首条目若缺少 `collision` 详情则整组跳过——分组条件已
    /// 保证有详情，这是防御分支。
    ///
    /// 返回以 `\n` 连接的行；输入为空时返回空串（调用方会跳过空诊断列表）。
    fn format_diagnostics(
        &self,
        diagnostics: &[ResourceDiagnostic],
        source_infos: &IndexMap<String, SourceInfo>,
    ) -> String {
        let mut lines: Vec<String> = Vec::new();
        let mut collisions: IndexMap<String, Vec<&ResourceDiagnostic>> = IndexMap::new();
        let mut other: Vec<&ResourceDiagnostic> = Vec::new();
        // 分拣：只有“Collision 且带 collision 详情”的条目进冲突组；其余按原顺序留在
        // `other`（包括缺详情、无法打印 loser 的 Collision）。
        for diagnostic in diagnostics {
            if diagnostic.kind == DiagnosticKind::Collision
                && let Some(collision) = &diagnostic.collision
            {
                collisions
                    .entry(collision.name.clone())
                    .or_default()
                    .push(diagnostic);
            } else {
                other.push(diagnostic);
            }
        }

        // 每组用第一条诊断的 collision 决定 winner，再逐个打印组内条目的 loser。
        for (name, collision_list) in &collisions {
            let Some(first) = collision_list
                .first()
                .and_then(|diagnostic| diagnostic.collision.as_ref())
            else {
                continue;
            };
            lines.push(fg(ThemeColor::Warning, &format!("  \"{name}\" collision:")));
            lines.push(fg(
                ThemeColor::Dim,
                &format!(
                    "    {} {}",
                    fg(ThemeColor::Success, "✓"),
                    self.format_path_with_source(
                        &first.winner_path,
                        self.find_source_info_for_path(&first.winner_path, source_infos)
                            .as_ref(),
                    )
                ),
            ));
            for diagnostic in collision_list {
                if let Some(collision) = &diagnostic.collision {
                    lines.push(fg(
                        ThemeColor::Dim,
                        &format!(
                            "    {} {} (skipped)",
                            fg(ThemeColor::Warning, "✗"),
                            self.format_path_with_source(
                                &collision.loser_path,
                                self.find_source_info_for_path(&collision.loser_path, source_infos)
                                    .as_ref(),
                            )
                        ),
                    ));
                }
            }
        }

        // 非冲突诊断：颜色只区分 Error（红）与其余（警告色）。
        for diagnostic in other {
            let color = if diagnostic.kind == DiagnosticKind::Error {
                ThemeColor::Error
            } else {
                ThemeColor::Warning
            };
            if let Some(path) = &diagnostic.path {
                let formatted = self.format_path_with_source(
                    path,
                    self.find_source_info_for_path(path, source_infos).as_ref(),
                );
                lines.push(fg(color, &format!("  {formatted}")));
                lines.push(fg(color, &format!("    {}", diagnostic.message)));
            } else {
                lines.push(fg(color, &format!("  {}", diagnostic.message)));
            }
        }
        lines.join("\n")
    }

    /// `showLoadedResources(options)`。
    ///
    /// 重建启动页的资源小节。`force` 为真时无视 `quietStartup` 强制显示列表；
    /// `show_diagnostics_when_quiet` 让诊断在列表被隐藏时仍然显示（reload 路径用它）。
    /// 两者都为假时只清空容器后返回。
    ///
    /// 列表顺序：`Context` → `Skills` → `Prompts` → `Extensions`；诊断顺序：
    /// `Skill conflicts` → `Prompt conflicts` → `Extension issues` → `Theme conflicts`
    /// （空诊断列表不产生小节）。`Extension issues` 依次拼接 `errors`、`warnings`、
    /// 扩展命令诊断、内置命令冲突诊断与快捷键诊断，保持各自原顺序。
    ///
    /// 数据是调用时刻的克隆快照；`get_startup_expansion_state()` 只取一次，作为每个
    /// `ExpandableText` 的初始展开状态，之后由 `set_tools_expanded` 直接修改组件。
    /// 文本与颜色延迟到渲染期求值，所以本函数不打印、没有返回值；失败只可能来自
    /// `RefCell` 借用冲突或渲染期的主题未初始化。
    ///
    /// `source_infos` 索引按扩展 → 技能 → prompt → 主题的顺序写入，同一路径后写覆盖先写
    /// （`IndexMap` 保留首次插入的位置），只用于诊断行的来源标签。隐藏扩展已被排除在
    /// `extensions` 之外，因此也不会进索引；引用其路径的诊断行只能走无来源分支。
    pub(crate) fn show_loaded_resources(&mut self, force: bool, show_diagnostics_when_quiet: bool) {
        let container = Rc::clone(&self.inner.borrow().loaded_resources_container);
        // 先清空：本函数可能在启动与 reload 时重复调用，旧快照必须整体丢弃（集成测试
        // `show_loaded_resources_clears_stale_entries_before_rendering` 锁定该行为）。
        container.borrow_mut().clear();

        // 列表与诊断是两个独立开关：quietStartup=true 时列表隐藏，但 reload 仍可要求
        // 只显示诊断（`showDiagnosticsWhenQuiet`）。
        let show_listing = force || self.should_show_startup_details();
        let show_diagnostics = show_listing || show_diagnostics_when_quiet;
        if !show_listing && !show_diagnostics {
            return;
        }

        // 四份资源快照都是 loader 的克隆；扩展额外过滤 `hidden == Some(true)`。
        let session = self.session();
        let loader = session.resource_loader();
        let skills_result = loader.get_skills();
        let prompts_result = loader.get_prompts();
        let themes_result = loader.get_themes();
        let extensions: Vec<ResourceItem> = loader
            .get_extensions()
            .extensions
            .iter()
            .filter(|extension| !extension.hidden.unwrap_or(false))
            .map(|extension| ResourceItem {
                path: extension.path.clone(),
                source_info: Some(extension.source_info.clone()),
            })
            .collect();

        // 诊断本身只带路径，需要反查来源标签。主题只有同时具备 source_path 与
        // source_info 时才能进表；同一路径后写覆盖先写。
        let mut source_infos: IndexMap<String, SourceInfo> = IndexMap::new();
        for extension in &extensions {
            if let Some(source_info) = &extension.source_info {
                source_infos.insert(extension.path.clone(), source_info.clone());
            }
        }
        for skill in &skills_result.skills {
            source_infos.insert(skill.file_path.clone(), skill.source_info.clone());
        }
        for prompt in &prompts_result.prompts {
            source_infos.insert(prompt.file_path.clone(), prompt.source_info.clone());
        }
        for theme in &themes_result.themes {
            if let Some(source_path) = theme.source_path()
                && let Some(source_info) = theme.source_info()
            {
                source_infos.insert(source_path.to_string_lossy().into_owned(), source_info);
            }
        }

        let section_header = |name: &str, color: ThemeColor| fg(color, &format!("[{name}]"));

        // 展开状态只在此刻取一次，作为后续所有小节 `ExpandableText` 的初始值。
        let expansion_state = self.get_startup_expansion_state();
        // 小节的折叠/展开正文都包成 `Rc<dyn Fn>`；每次追加“小节 + `Spacer`”两个子项，
        // 所以小节之间总有空行。标题行也在闭包里求值，以便跟随渲染时的主题。
        let add_loaded_section = |name: &str,
                                  collapsed_body: Rc<dyn Fn() -> String>,
                                  expanded_body: Rc<dyn Fn() -> String>,
                                  color: ThemeColor| {
            let name_owned = name.to_string();
            let collapsed_name = name_owned.clone();
            let expanded_name = name_owned;
            let collapsed = Rc::clone(&collapsed_body);
            let expanded = Rc::clone(&expanded_body);
            let section = ExpandableText::new(
                Rc::new(move || {
                    format!(
                        "{}\n{}",
                        section_header(&collapsed_name, color),
                        collapsed()
                    )
                }),
                Rc::new(move || {
                    format!("{}\n{}", section_header(&expanded_name, color), expanded())
                }),
                expansion_state,
                0,
                0,
            );
            container.borrow_mut().add_child(Box::new(section));
            container.borrow_mut().add_child(Box::new(Spacer::new()));
        };

        if show_listing {
            let system_prompt_source = loader.get_system_prompt_source();
            let append_sources = loader.get_append_system_prompt_sources();
            let agents_files = loader.get_agents_files().agents_files;
            let mut context_files: Vec<String> = Vec::new();
            if let Some(source) = &system_prompt_source {
                context_files.push(source.path.clone());
            }
            context_files.extend(append_sources.iter().map(|source| source.path.clone()));
            context_files.extend(agents_files.iter().map(|file| file.path.clone()));
            if !context_files.is_empty() {
                // Materialize the collapsed labels while no renderer borrow is active.
                // ExpandableText evaluates its closures lazily during Container::render;
                // consulting session_manager() there would re-enter InteractiveMode.inner.
                let compact_items = context_files
                    .iter()
                    .map(|path| self.format_context_path(path))
                    .map(|item| item.trim().to_string())
                    .filter(|item| !item.is_empty())
                    .collect::<Vec<_>>();
                container.borrow_mut().add_child(Box::new(Spacer::new()));
                let expanded_paths = context_files.clone();
                let display_for_expanded = self.clone();
                let expanded = Rc::new(move || {
                    expanded_paths
                        .iter()
                        .map(|path| {
                            fg(
                                ThemeColor::Dim,
                                &format!("  {}", display_for_expanded.format_display_path(path)),
                            )
                        })
                        .collect::<Vec<_>>()
                        .join("\n")
                });
                let collapsed = Rc::new(move || {
                    let labels = compact_items.join(", ");
                    fg(ThemeColor::Dim, &format!("  {labels}"))
                });
                add_loaded_section("Context", collapsed, expanded, ThemeColor::MdHeading);
            }

            let skills = &skills_result.skills;
            if !skills.is_empty() {
                let items: Vec<ResourceItem> = skills
                    .iter()
                    .map(|skill| ResourceItem {
                        path: skill.file_path.clone(),
                        source_info: Some(skill.source_info.clone()),
                    })
                    .collect();
                let groups = self.build_scope_groups(&items);
                let display = self.clone();
                let display_for_path = self.clone();
                let expanded_groups = groups
                    .iter()
                    .map(|group| ScopeGroup {
                        scope: group.scope,
                        paths: group.paths.clone(),
                        packages: group.packages.clone(),
                    })
                    .collect::<Vec<_>>();
                let expanded = Rc::new(move || {
                    display_for_path.format_scope_groups(
                        &expanded_groups,
                        |item| display_for_path.format_display_path(&item.path),
                        |item, _| {
                            display_for_path.get_short_path(&item.path, item.source_info.as_ref())
                        },
                    )
                });
                let names: Vec<String> = skills.iter().map(|skill| skill.name.clone()).collect();
                let collapsed = Rc::new(move || display.format_compact_list_static(&names, true));
                add_loaded_section("Skills", collapsed, expanded, ThemeColor::MdHeading);
            }

            let templates = session.prompt_templates();
            if !templates.is_empty() {
                let items: Vec<ResourceItem> = templates
                    .iter()
                    .map(|template| ResourceItem {
                        path: template.file_path.clone(),
                        source_info: Some(template.source_info.clone()),
                    })
                    .collect();
                let groups = self.build_scope_groups(&items);
                let by_path: IndexMap<String, String> = templates
                    .iter()
                    .map(|template| (template.file_path.clone(), template.name.clone()))
                    .collect();
                let display = self.clone();
                let display_for_path = self.clone();
                let expanded_groups = groups;
                let expanded_by_path = by_path.clone();
                let expanded = Rc::new(move || {
                    display_for_path.format_scope_groups(
                        &expanded_groups,
                        |item| match expanded_by_path.get(&item.path) {
                            Some(name) => format!("/{name}"),
                            None => display_for_path.format_display_path(&item.path),
                        },
                        |item, _| match expanded_by_path.get(&item.path) {
                            Some(name) => format!("/{name}"),
                            None => display_for_path.format_display_path(&item.path),
                        },
                    )
                });
                let names: Vec<String> = templates
                    .iter()
                    .map(|template| format!("/{}", template.name))
                    .collect();
                let collapsed = Rc::new(move || display.format_compact_list_static(&names, true));
                add_loaded_section("Prompts", collapsed, expanded, ThemeColor::MdHeading);
            }

            if !extensions.is_empty() {
                let groups = self.build_scope_groups(&extensions);
                let display = self.clone();
                let display_for_path = self.clone();
                let expanded_groups = groups;
                let expanded = Rc::new(move || {
                    display_for_path.format_scope_groups(
                        &expanded_groups,
                        |item| display_for_path.format_extension_display_path(&item.path),
                        |item, _| {
                            display_for_path.format_extension_display_path(
                                &display_for_path
                                    .get_short_path(&item.path, item.source_info.as_ref()),
                            )
                        },
                    )
                });
                let labels = self.get_compact_extension_labels(&extensions);
                let collapsed = Rc::new(move || display.format_compact_list_static(&labels, true));
                add_loaded_section("Extensions", collapsed, expanded, ThemeColor::MdHeading);
            }
        }

        if show_diagnostics {
            let add_diagnostics = |name: &str, diagnostics: &[ResourceDiagnostic]| {
                if diagnostics.is_empty() {
                    return;
                }
                let mode = self.clone();
                let infos = source_infos.clone();
                let diagnostics = diagnostics.to_vec();
                let title = format!("[{name}]");
                // TS 在 ThemedText 的渲染闭包里调用 `warningLines()`（颜色随主题切换）；
                // 这里同样延迟到渲染期计算。
                container
                    .borrow_mut()
                    .add_child(Box::new(ThemedText::with_padding(
                        Box::new(move || {
                            let lines = mode.format_diagnostics(&diagnostics, &infos);
                            format!("{}\n{lines}", fg(ThemeColor::Warning, &title))
                        }),
                        0,
                        0,
                    )));
                container.borrow_mut().add_child(Box::new(Spacer::new()));
            };

            add_diagnostics("Skill conflicts", &skills_result.diagnostics);
            add_diagnostics("Prompt conflicts", &prompts_result.diagnostics);

            let extensions_result = loader.get_extensions();
            let mut extension_diagnostics: Vec<ResourceDiagnostic> = Vec::new();
            for error in &extensions_result.errors {
                extension_diagnostics.push(ResourceDiagnostic {
                    kind: DiagnosticKind::Error,
                    message: error.error.clone(),
                    path: Some(error.path.clone()),
                    collision: None,
                });
            }
            if let Some(warnings) = &extensions_result.warnings {
                for warning in warnings {
                    extension_diagnostics.push(ResourceDiagnostic {
                        kind: DiagnosticKind::Warning,
                        message: warning.warning.clone(),
                        path: Some(warning.path.clone()),
                        collision: None,
                    });
                }
            }
            let extension_runner = session.extension_runner();
            extension_diagnostics.extend(extension_runner.get_command_diagnostics());
            extension_diagnostics
                .extend(self.get_built_in_command_conflict_diagnostics(&extension_runner));
            extension_diagnostics.extend(extension_runner.get_shortcut_diagnostics());
            add_diagnostics("Extension issues", &extension_diagnostics);

            add_diagnostics("Theme conflicts", &themes_result.diagnostics);
        }
    }

    /// `formatCompactList` 的自由函数版本（闭包捕获 `self.clone()` 后调用）。
    ///
    /// 每项先 `trim`，空串丢弃；`sort` 为真时按 [`locale_compare`] 排序（当前所有调用点
    /// 都传 `true`）；最后用 `", "` 连接、加两个空格缩进并用 `Dim` 着色。**不**去重：
    /// 重复项会原样重复打印。
    fn format_compact_list_static(&self, items: &[String], sort: bool) -> String {
        let mut labels: Vec<String> = items
            .iter()
            .map(|item| item.trim().to_string())
            .filter(|item| !item.is_empty())
            .collect();
        if sort {
            labels.sort_by(|a, b| locale_compare(a, b));
        }
        fg(ThemeColor::Dim, &format!("  {}", labels.join(", ")))
    }
}

// 测试接缝：以下方法把本文件的私有辅助函数原样转发出去，只供集成测试断言渲染结果，
// 不参与任何生产逻辑。`#[doc(hidden)]` 让它们不进入公开文档，但它们仍是 `pub` API。
#[doc(hidden)]
impl InteractiveMode {
    /// 渲染 loaded resources 容器（宽度固定 120）并去掉 ANSI 转义，行间以 `\n` 连接。
    /// 宽度只影响折行；主题必须已初始化，否则渲染闭包里的 `fg` 会 panic。
    pub fn test_loaded_resources_text(&self) -> String {
        let container = Rc::clone(&self.inner.borrow().loaded_resources_container);
        container
            .borrow_mut()
            .render(120)
            .iter()
            .map(|line| kiana_tui::text::strip_terminal_sequences(line))
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// 计算扩展条目的紧凑标签（`getCompactExtensionLabels`）。
    pub fn test_compact_extension_labels(
        &self,
        extensions: &[(String, Option<SourceInfo>)],
    ) -> Vec<String> {
        let items: Vec<ResourceItem> = extensions
            .iter()
            .map(|(path, source_info)| ResourceItem {
                path: path.clone(),
                source_info: source_info.clone(),
            })
            .collect();
        self.get_compact_extension_labels(&items)
    }

    /// 调用 `formatDiagnostics`（测试用）。
    pub fn test_format_diagnostics(
        &self,
        diagnostics: &[ResourceDiagnostic],
        source_infos: &IndexMap<String, SourceInfo>,
    ) -> String {
        self.format_diagnostics(diagnostics, source_infos)
    }

    /// 设置 `options.verbose`（`getStartupExpansionState` / `shouldShowStartupDetails` 用）。
    pub fn test_set_verbose(&mut self, verbose: bool) {
        self.inner.borrow_mut().options.verbose = Some(verbose);
    }

    /// `showLoadedResources(force, showDiagnosticsWhenQuiet)` 的测试接缝。
    pub fn test_show_loaded_resources(&mut self, force: bool, show_diagnostics_when_quiet: bool) {
        self.show_loaded_resources(force, show_diagnostics_when_quiet);
    }

    /// 往 loadedResourcesContainer 塞一个子项（测试接缝）。
    pub fn test_push_loaded_resources_child(&mut self, component: Box<dyn Component>) {
        self.inner
            .borrow()
            .loaded_resources_container
            .borrow_mut()
            .add_child(component);
    }

    /// `formatContextPath(path)` 的测试接缝。
    pub fn test_format_context_path(&self, path: &str) -> String {
        self.format_context_path(path)
    }

    /// `formatDisplayPath(path)` 的测试接缝。
    pub fn test_format_display_path(&self, path: &str) -> String {
        self.format_display_path(path)
    }

    /// `shouldShowStartupHeader()` 的测试接缝。
    pub fn test_should_show_startup_header(&self) -> bool {
        self.should_show_startup_header()
    }

    /// `shouldShowStartupDetails()` 的测试接缝。
    pub fn test_should_show_startup_details(&self) -> bool {
        self.should_show_startup_details()
    }

    /// `getStartupExpansionState()` 的测试接缝。
    pub fn test_get_startup_expansion_state(&self) -> bool {
        self.get_startup_expansion_state()
    }

    /// 渲染扩展段（`showLoadedResources` 的 Extensions 部分；测试接缝）。
    ///
    /// 直接接受 `{ path, sourceInfo }`，绕过 loader 的来源信息重算。
    pub fn test_render_extensions_listing(
        &self,
        extensions: &[(String, Option<SourceInfo>)],
        expanded: bool,
    ) -> String {
        let items: Vec<ResourceItem> = extensions
            .iter()
            .map(|(path, source_info)| ResourceItem {
                path: path.clone(),
                source_info: source_info.clone(),
            })
            .collect();
        if expanded {
            let groups = self.build_scope_groups(&items);
            self.format_scope_groups(
                &groups,
                |item| self.format_extension_display_path(&item.path),
                |item, _| {
                    self.format_extension_display_path(
                        &self.get_short_path(&item.path, item.source_info.as_ref()),
                    )
                },
            )
        } else {
            let labels = self.get_compact_extension_labels(&items);
            self.format_compact_list_static(&labels, true)
        }
    }
}
