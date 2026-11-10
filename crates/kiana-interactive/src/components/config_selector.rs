//! components/config-selector.ts。
//!
//! `/config` 资源管理面板：头部（scope 提示 + 键位提示）+ 自带搜索框的资源列表。
//! 支持 global / project 两个写入 scope、package 与顶层资源、project 侧三态覆盖
//! （inherit / load / unload）。
//!
//! ## 与 TS 的表示差异
//!
//! * `Record<ConfigWriteScope, …>` 用 [`IndexMap`]（组件内部固定按 global → project 插入）。
//! * TS 的 `ResourceItem` 是对象引用，列表与组树共享同一个对象；Rust 用
//!   `Rc<RefCell<ResourceItem>>` 复刻同一身份语义（`updateItem` 只改第一份匹配项）。
//! * `node:path` 的 `basename`/`dirname`/`join`/`relative` 在本文件内实现（`kiana-core::paths`
//!   只公开了 `canonicalizePath`/`isLocalPath`/`resolvePath`，其 node:path 词法实现是
//!   crate 私有）。POSIX 分支按 POSIX 词法完整实现；Windows 分支只做 `cfg!(windows)` 的词法近似
//!   （不保留 UNC/根前缀、比较按大小写敏感、跨盘 `relative` 不返回绝对路径）——见
//!   `spec/GAPS.md` 平台条目（验收目标 linux + macOS）。
//! * TS 的 `ConfigSelectorComponent extends Container`；Rust 侧状态放进 `Rc<RefCell<…>>`
//!   以便回调共享，渲染顺序与 TS 的 addChild 顺序逐行一致。表头与 `ResourceList` 共享
//!   一个 `Rc<RefCell<ConfigSelectorHeader>>`：TS 的 Tab 切换发生在 `ResourceList`
//!   内部（`onSwitchMode` → `switchWriteScope()`），共享句柄让 Rust 侧同样能同步更新表头。
//!
//! ## 执行流程
//!
//! 1. [`ConfigSelectorComponent::new`] 按 global / project 两个写入 scope 各调用一次
//!    `build_groups`：以 `origin:scope:source:baseDir` 为键归组，组内按资源类型分子组，
//!    再按显示名排序。两份组树都放进 `groups_by_scope`，构造后不再重建。
//! 2. [`ResourceList::new`] 把初始 scope 的组树展平成 `flat_items`（组 → 子组 → 条目），
//!    复制一份作为 `filtered_items`，并把选中位置放到第一个条目行。
//! 3. `handle_input` 先按 keybinding 分派（上/下、翻页、取消、退出、Tab、空格/确认）；
//!    未命中的输入交给搜索框，随后重新过滤。project scope 下空格在
//!    inherit / load / unload 三态间循环（见下节）。
//! 4. `render` 先输出搜索框及其后的空行，再输出视口内的扁平行（组、子组、条目各占一行），
//!    列表被裁剪时追加 `(当前条目/总条目)` 计数；外层组件再包 Spacer 与 DynamicBorder。
//!
//! ## 状态机（project scope 的覆盖状态）
//!
//! 每个条目在 project scope 下由“项目设置里的模式条目”推导出三种显示状态：
//!
//! * `inherit`：项目设置里没有指向该条目的模式；显示继承自 global 的启用状态，
//!   继承项会变暗并带 `inherited global` 后缀。
//! * `load`：存在匹配的 `+pattern` 或无前缀条目，显示 `[+]`。
//! * `unload`：存在匹配的 `-pattern` 或 `!pattern` 条目，显示 `[-]`。
//!
//! 按空格时由私有的 `get_next_override_state` 计算下一状态：先看该条目在 global 快照里的
//! 启用值 `inherited_enabled`，从 `inherit` 出发的第一次按键总是翻转“有效启用值”，因此
//! 从 `inherit` 连按三次必然回到 `inherit`。完整转换表见该函数的文档注释。
//!
//! ## 注意事项
//!
//! * 条目的身份是 `Rc<RefCell<ResourceItem>>`：`flat_items`、`filtered_items` 与组树共享
//!   同一份数据，[`ResourceList::update_item`] 改一份后所有视图同时可见。
//! * [`ConfigSelectorComponent::get_resource_list`] 通过 `RefCell::borrow_mut` 借出内部
//!   状态；守卫存活期间再取第二个守卫会在运行期 panic（见 [`ResourceListGuard`]）。
//! * project scope 的写入走 `SettingsManager::set_project_*`；项目未受信时这些方法直接
//!   返回 `Err` 且不改内存设置，而本文件统一丢弃返回值，因此界面显示的新状态**不能**
//!   证明设置已写入。
//! * 表头与列表的 scope 必须同步：只有 `ResourceList` 的 Tab 分支通过共享的 `header`
//!   句柄同时更新两者；`on_switch_mode` 回调本身只负责触发重绘。
//!
//! ## 调用方
//!
//! 生产入口在 `kiana-coding-agent`：`package_manager_cli` 汇总 global / project 两套
//! `ResolvedPaths` 并发起 `select_config`，后者在本 crate 之外构造
//! [`ConfigSelectorComponent`]，把 Esc / Ctrl+C / 重绘分别接到组件的 `on_close` /
//! `on_exit` / `request_render`，再把组件挂到 TUI 上。本模块不启动 TUI、不创建主题；
//! 设置修改只通过 [`SettingsManager`] 的 setter 入队（写盘由该管理器的后台队列完成），
//! 本模块不调用 `flush` 等待写完成，等待由调用方负责。
//! `crates/kiana-interactive/tests/components_config_selector.rs` 则直接通过
//! [`ConfigSelectorComponent::get_resource_list`] 驱动列表的 `handle_input` / `render`，
//! 用黄金渲染行锁定输出。
//!
//! ## 阅读顺序
//!
//! 类型与 `ConfigWriteScope` → `build_groups`（组树）→ [`ResourceList`] 的展平/过滤 →
//! 三态覆盖与设置写入 → `render` / `handle_input` → [`ConfigSelectorComponent`] 的组装。
//!
//! TS 源：`packages/coding-agent/src/modes/interactive/components/config-selector.ts`。

use std::cell::{RefCell, RefMut};
use std::collections::HashSet;
use std::ops::{Deref, DerefMut};
use std::rc::Rc;

use indexmap::IndexMap;
use kiana_config::settings::SettingsManager;
use kiana_config::settings::schema::{PackageSource, PackageSourceObject, Settings};
use kiana_core::paths::{PathInputOptions, canonicalize_path, is_local_path, resolve_path};
use kiana_core::source_info::{BUILTIN_PATH_PREFIX, PathMetadata, SourceOrigin, SourceScope};
use kiana_resources::package::types::{ResolvedPaths, ResolvedResource};
use kiana_theme::ThemeColor;
use kiana_tui::keybindings::get_keybindings;
use kiana_tui::{
    Component, Focusable, Input, Spacer, matches_key, truncate_to_width, visible_width,
};
use kiana_ui_kit::{DynamicBorder, key_hint, raw_key_hint};

/// 资源类型（模块私有 `type ResourceType`）。
///
/// TS 的类型别名只在文件内使用；Rust 侧声明为 `pub`，是因为公开的 [`ResourceItem`]
/// （及其 `resource_type` 字段）和 [`ToggleCallback`] 会把它暴露到公开 API 里。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ResourceType {
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
    /// `RESOURCE_TYPES`（顺序即子组排序）。
    ///
    /// 顺序与 `type_order` 一致，但目前没有调用点读取它：`build_groups`
    /// 是按四个类型显式依次调用 `add_to_group` 的，子组排序则用 `type_order`。
    pub const ALL: [ResourceType; 4] = [
        ResourceType::Extensions,
        ResourceType::Skills,
        ResourceType::Prompts,
        ResourceType::Themes,
    ];

    /// JSON 键名（`"extensions"` 等）。
    pub const fn as_str(self) -> &'static str {
        match self {
            ResourceType::Extensions => "extensions",
            ResourceType::Skills => "skills",
            ResourceType::Prompts => "prompts",
            ResourceType::Themes => "themes",
        }
    }

    /// `RESOURCE_TYPE_LABELS`。
    pub const fn label(self) -> &'static str {
        match self {
            ResourceType::Extensions => "Extensions",
            ResourceType::Skills => "Skills",
            ResourceType::Prompts => "Prompts",
            ResourceType::Themes => "Themes",
        }
    }

    /// TS 的 `typeOrder`。
    ///
    /// 子组排序键：数值小的排前面（Extensions → Skills → Prompts → Themes）。
    const fn type_order(self) -> usize {
        match self {
            ResourceType::Extensions => 0,
            ResourceType::Skills => 1,
            ResourceType::Prompts => 2,
            ResourceType::Themes => 3,
        }
    }
}

/// 写入 scope（TS `ConfigWriteScope`）。
///
/// 它决定“把修改写到哪个设置文件”，不要与 `SourceScope`（条目本身来自哪里）混淆：
/// 一个 `SourceScope::User` 的继承条目，在 project 写入 scope 下也可以被覆盖。
/// 两个写入 scope 分别对应 [`SettingsManager::get_global_settings`] /
/// [`SettingsManager::get_project_settings`] 所读的那份设置。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ConfigWriteScope {
    /// 全局（`~/.kiana/agent/settings.json`）。
    Global,
    /// 项目（`.kiana/settings.json`）。
    Project,
}

impl ConfigWriteScope {
    fn toggled(self) -> Self {
        match self {
            ConfigWriteScope::Global => ConfigWriteScope::Project,
            ConfigWriteScope::Project => ConfigWriteScope::Global,
        }
    }
}

/// TS `ScopedResolvedPaths = Record<ConfigWriteScope, ResolvedPaths>`。
///
/// `IndexMap` 只保留了调用方自己的插入顺序，类型本身并不强制 global → project；
/// [`ConfigSelectorComponent::new`] 读的是键而不是位置，并会按固定顺序重新组装内部的
/// `groups_by_scope`。缺少某个键时按 `ResolvedPaths::default()`（全空）处理。
pub type ScopedResolvedPaths = IndexMap<ConfigWriteScope, ResolvedPaths>;

/// TS `ProjectOverrideState`。
///
/// 只在 project 写入 scope 下有意义：global scope 的 `render` 直接按
/// `item.enabled` 画勾选框。三种状态的判定见私有的 `get_project_override_state`，
/// 状态之间的循环见 `get_next_override_state`。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProjectOverrideState {
    Inherit,
    Load,
    Unload,
}

/// TS `ResourceItem`（模块私有；Rust 公开是因为 `ResourceList` 的公开回调使用它）。
///
/// 一个条目对应列表里的一行资源。`enabled` 是构造时从 `ResolvedResource.enabled` 拷贝的
/// **解析结果**，不代表项目覆盖状态：project scope 下 `[+]` / `[-]` / `[x]` 由设置文件
/// 现算，只有 `inherit` 状态才回落到这个值。
/// `group_key` / `subgroup_key` 与 TS 一样只在构造时写入，Rust 侧没有读取点。
#[derive(Debug, Clone)]
pub struct ResourceItem {
    /// 资源路径。
    pub path: String,
    /// 是否启用。
    pub enabled: bool,
    /// 来源元数据。
    pub metadata: PathMetadata,
    /// 资源类型。
    pub resource_type: ResourceType,
    /// 显示名。
    pub display_name: String,
    /// 组键（只写不读）。
    pub group_key: String,
    /// 子组键（只写不读）。
    pub subgroup_key: String,
}

/// TS `ResourceSubgroup`。
///
/// 一个组内按资源类型划分的一层（`Extensions` / `Skills` / …）。`items` 在构造时按
/// `display_name` 的 `locale_compare` 排序，此后只通过内部 `RefCell` 修改 `enabled`。
#[derive(Debug)]
pub struct ResourceSubgroup {
    /// 资源类型。
    pub resource_type: ResourceType,
    /// 显示标签。
    pub label: String,
    /// 条目。
    pub items: Vec<Rc<RefCell<ResourceItem>>>,
}

/// TS `ResourceGroup`。
///
/// 同一个 `origin + scope + source + base_dir` 的资源归为一组；组树按
/// “包优先、user 优先、source 按 `locale_compare`”排序，子组按资源类型排序。
/// 构造完成后组树本身不再增删，变化只发生在条目的 `enabled` 上。
/// `scope` 用于 project scope 下判断是否为“继承的 global 组”（`SourceScope::User`）；
/// `origin` / `source` 在当前文件中构造后没有读取点（排序用的是构造前的 `DraftGroup`）。
#[derive(Debug)]
pub struct ResourceGroup {
    /// 组键。
    pub key: String,
    /// 组标签。
    pub label: String,
    /// 作用域。
    pub scope: SourceScope,
    /// 来源层级。
    pub origin: SourceOrigin,
    /// 来源标签。
    pub source: String,
    /// 子组。
    pub subgroups: Vec<Rc<ResourceSubgroup>>,
}

/// `onToggle` 的回调类型（TS `(item, newEnabled) => void`）。
///
/// `newEnabled` 是切换后的**有效值**：project scope 回到 `inherit` 时它是继承的
/// global 状态，而不是 `true`。回调在条目更新之后触发（见 [`ResourceList::update_item`]）。
pub type ToggleCallback = Rc<dyn Fn(&ResourceItem, bool)>;

/// TS `FlatEntry`。
///
/// `flat_items` / `filtered_items` 的元素：组、子组、条目都占一行，选择与渲染共用这份
/// 展平序列。`Subgroup` 变体同时带上所属组，是因为渲染子组行时要读 `group.scope`
/// 决定是否为继承的 global 子组（变暗）。
#[derive(Clone)]
enum FlatEntry {
    Group(Rc<ResourceGroup>),
    Subgroup(Rc<ResourceSubgroup>, Rc<ResourceGroup>),
    Item(Rc<RefCell<ResourceItem>>),
}

// =============================================================================
// node:path 词法辅助（见模块文档）
// =============================================================================

/// 当前平台的分隔符判定：Windows 同时接受 `/` 与 `\`，其它平台只认 `/`。
///
/// `cfg!(windows)` 是编译期常量；两个分支都会参与类型检查，但只有当前平台的分支会
/// 实际生效。
fn is_sep(character: char) -> bool {
    character == '/' || (cfg!(windows) && character == '\\')
}

/// 生成路径时使用的分隔符（Windows 为 `\`，其它平台为 `/`）。
fn join_sep() -> char {
    if cfg!(windows) { '\\' } else { '/' }
}

/// 去掉尾部所有分隔符（`"/a//"` → `"/a"`）；全是分隔符时返回空串。
fn trim_trailing_seps(path: &str) -> &str {
    path.trim_end_matches(is_sep)
}

/// Node `path.isAbsolute` 的近似：Windows 接受前导 `\`/`/` 或“盘符 + 分隔符”，
/// 其它平台只接受前导 `/`。
///
/// 这是词法判断，不访问文件系统；UNC、`\\?\` 前缀等细节没有完整复刻，模块文档已注明
/// 只保证 POSIX 语义完整。
fn node_is_absolute(path: &str) -> bool {
    if cfg!(windows) {
        let bytes = path.as_bytes();
        if path.starts_with('\\') || path.starts_with('/') {
            return true;
        }
        bytes.len() >= 3
            && bytes[0].is_ascii_alphabetic()
            && bytes[1] == b':'
            && (bytes[2] == b'\\' || bytes[2] == b'/')
    } else {
        path.starts_with('/')
    }
}

/// `path.basename(path)`。
///
/// 先去掉尾部分隔符再取最后一段：`"/a/b/"` → `"b"`，`"/"` → `""`（Node 同样返回空串，
/// 而不是 `"/"`）。路径全是分隔符或为空时返回空串。
fn node_basename(path: &str) -> String {
    let trimmed = trim_trailing_seps(path);
    if trimmed.is_empty() {
        return String::new();
    }
    match trimmed.rfind(is_sep) {
        Some(index) => trimmed[index + 1..].to_string(),
        None => trimmed.to_string(),
    }
}

/// `path.dirname(path)`。
///
/// 空串 → `"."`；全是分隔符 → 根分隔符（POSIX `"/"`）；没有分隔符的相对名 → `"."`；
/// 以分隔符开头的路径保留根（`"/a"` → `"/"`）。尾部分隔符先被忽略。
fn node_dirname(path: &str) -> String {
    if path.is_empty() {
        return ".".to_string();
    }
    let trimmed = trim_trailing_seps(path);
    if trimmed.is_empty() {
        return join_sep().to_string();
    }
    match trimmed.rfind(is_sep) {
        None => ".".to_string(),
        Some(0) => trimmed[..1].to_string(),
        Some(index) => trimmed[..index].to_string(),
    }
}

/// `path.normalize(path)`（足够 `join`/`relative` 使用）。
///
/// 逐段处理：丢弃空段与 `"."`；`".."` 在可回退时弹出上一段，在相对路径且无可回退时
/// 保留；绝对路径的 `".."` 不会越过根。保留输入末尾的分隔符（`"a/b/"` → `"a/b/"`，
/// `"./"` → `"./"`），结果为空时按是否为绝对路径返回根或 `"."`/`"./"`。
///
/// 这是词法近似：Windows 盘符作为第一段被保留，但盘符根不带尾随分隔符（`"C:/a/.."`
/// 得到 `"C:"` 而非 `"C:\\"`），细节见模块文档的平台说明。
fn node_normalize(path: &str) -> String {
    if path.is_empty() {
        return ".".to_string();
    }
    let absolute = node_is_absolute(path);
    let trailing = path.ends_with(is_sep);
    let mut parts: Vec<&str> = Vec::new();
    for segment in path.split(is_sep) {
        match segment {
            "" | "." => {}
            ".." => {
                if let Some(last) = parts.last() {
                    if *last != ".." {
                        parts.pop();
                    } else if !absolute {
                        parts.push("..");
                    }
                } else if !absolute {
                    parts.push("..");
                }
            }
            other => parts.push(other),
        }
    }
    let mut normalized = parts.join(&join_sep().to_string());
    if normalized.is_empty() {
        return if absolute {
            join_sep().to_string()
        } else if trailing {
            "./".to_string()
        } else {
            ".".to_string()
        };
    }
    if trailing {
        normalized.push(join_sep());
    }
    if absolute && !cfg!(windows) {
        format!("/{normalized}")
    } else if absolute {
        // Windows 盘符前缀在 split 后仍作为第一段保留。
        normalized
    } else {
        normalized
    }
}

/// `path.resolve(path)`（相对路径基于进程 cwd）。
///
/// 注意用的是 `std::env::current_dir()`（进程当前目录），不是组件构造时传入的 `cwd`
/// 字段；绝对输入直接归一化，不碰 cwd。取 cwd 失败时以 `"."` 作为前缀再归一化，
/// 因此相对路径会退化成基于 `.` 的结果。
fn node_resolve(path: &str) -> String {
    if node_is_absolute(path) {
        return node_normalize(path);
    }
    let cwd = std::env::current_dir()
        .map(|path| path.to_string_lossy().into_owned())
        .unwrap_or_else(|_| ".".to_string());
    node_normalize(&format!("{cwd}{}{path}", join_sep()))
}

/// `path.join(parts...)`。
///
/// 跳过空段后用分隔符连接并归一化；全部为空（包括零个参数）时返回 `"."`。与
/// `path.resolve` 不同，后面的绝对段**不会**重置结果，只是被当成普通片段拼接。
fn node_join(parts: &[&str]) -> String {
    let separator = join_sep().to_string();
    let joined = parts
        .iter()
        .filter(|part| !part.is_empty())
        .copied()
        .collect::<Vec<_>>()
        .join(&separator);
    if joined.is_empty() {
        return ".".to_string();
    }
    node_normalize(&joined)
}

/// `path.relative(from, to)`。
///
/// 两边都先按 [`node_resolve`] 解析成绝对路径（因此相对输入依赖进程 cwd），相同则返回
/// 空串；否则取公共前缀，剩下的 `from` 段各补一个 `".."`，再接 `to` 的剩余段。
/// 只做词法计算；Windows 跨盘时不会像 Node 那样直接返回 `to` 的绝对路径，而是拼出
/// 普通相对串（模块文档已列为平台近似）。
fn node_relative(from: &str, to: &str) -> String {
    let from = node_resolve(from);
    let to = node_resolve(to);
    if from == to {
        return String::new();
    }
    let from_parts: Vec<&str> = from.split(is_sep).filter(|part| !part.is_empty()).collect();
    let to_parts: Vec<&str> = to.split(is_sep).filter(|part| !part.is_empty()).collect();
    let common = from_parts
        .iter()
        .zip(to_parts.iter())
        .take_while(|(left, right)| left == right)
        .count();
    let mut output: Vec<&str> = Vec::new();
    output.extend(std::iter::repeat_n("..", from_parts.len() - common));
    output.extend_from_slice(&to_parts[common..]);
    output.join(&join_sep().to_string())
}

// =============================================================================
// 组构建
// =============================================================================

/// `origin` 的 TS 字面量；同时参与 `group_key`，改变它会改变分组结果。
fn origin_str(origin: SourceOrigin) -> &'static str {
    match origin {
        SourceOrigin::Package => "package",
        SourceOrigin::TopLevel => "top-level",
    }
}

/// `scope` 的 TS 字面量；同时参与 `group_key`。
fn scope_str(scope: SourceScope) -> &'static str {
    match scope {
        SourceScope::User => "user",
        SourceScope::Project => "project",
        SourceScope::Temporary => "temporary",
    }
}

/// `formatBaseDir(baseDir)`。
///
/// 把主目录前缀换成 `~`、反斜杠统一成 `/`，并保证以 `/` 结尾。前缀替换是纯字符串
/// 操作，不检查目录边界：主目录为 `/home/a` 时 `/home/abc` 会显示成 `~bc`（与 TS 的
/// `startsWith` 判断一致）。取不到主目录时 `home_dir` 是空串，而任何字符串都
/// `starts_with("")`，于是所有路径都会变成 `~<原路径>`——这是该实现的退化行为。
fn format_base_dir(base_dir: &str) -> String {
    let home_dir = dirs::home_dir()
        .map(|path| path.to_string_lossy().into_owned())
        .unwrap_or_default();
    let mut display_path = if base_dir == home_dir {
        "~".to_string()
    } else if base_dir.starts_with(&home_dir) {
        let rest = &base_dir[home_dir.len()..];
        format!("~{}", rest.replace('\\', "/"))
    } else {
        base_dir.replace('\\', "/")
    };
    if !display_path.ends_with('/') {
        display_path.push('/');
    }
    display_path
}

/// `getGroupLabel(metadata, agentDir)`。
///
/// 分支顺序固定，后面的分支只处理前面未命中的来源：
/// 包资源 → `<source> (<scope>)`；内置资源 → `Built-in` 或 `Built-in (project override)`；
/// `source == "auto"` 的顶层资源 → 用 `base_dir`（缺失时 user 用 `agent_dir`、project 用
/// `.kiana/`）；其余来源（例如 `local`）→ `User settings` / `Project settings`。
fn get_group_label(metadata: &PathMetadata, agent_dir: &str) -> String {
    if metadata.origin == SourceOrigin::Package {
        return format!("{} ({})", metadata.source, scope_str(metadata.scope));
    }
    if metadata.source == "builtin" {
        return if metadata.scope == SourceScope::User {
            "Built-in".to_string()
        } else {
            "Built-in (project override)".to_string()
        };
    }
    if metadata.source == "auto" {
        if let Some(base_dir) = &metadata.base_dir {
            return if metadata.scope == SourceScope::User {
                format!("User ({})", format_base_dir(base_dir))
            } else {
                format!("Project ({})", format_base_dir(base_dir))
            };
        }
        return if metadata.scope == SourceScope::User {
            format!("User ({})", format_base_dir(agent_dir))
        } else {
            format!("Project ({}/)", kiana_core::config::CONFIG_DIR_NAME)
        };
    }
    if metadata.scope == SourceScope::User {
        "User settings".to_string()
    } else {
        "Project settings".to_string()
    }
}

/// 条目的 `displayName`。
///
/// * `builtin` 来源直接截掉 `builtin:` 前缀（长短不足时得空串，见下面注释）。
/// * 扩展不在名为 `extensions` 的父目录下时显示 `父目录/文件名`，否则显示文件名。
/// * 技能文件名为 `SKILL.md` 时显示父目录名（技能目录即技能名）。
/// * 其余情况显示文件名。
///
/// 结果既用于渲染，也是条目排序键，修改会改变列表顺序与搜索命中文本。
fn resource_display_name(path: &str, resource_type: ResourceType, source: &str) -> String {
    if source == "builtin" {
        // TS 的 `path.slice(BUILTIN_PATH_PREFIX.length)` 是无条件截断；路径更短时得到空串。
        return path
            .get(BUILTIN_PATH_PREFIX.len()..)
            .unwrap_or("")
            .to_string();
    }
    let file_name = node_basename(path);
    let parent_folder = node_basename(&node_dirname(path));
    if resource_type == ResourceType::Extensions && parent_folder != "extensions" {
        format!("{parent_folder}/{file_name}")
    } else if resource_type == ResourceType::Skills && file_name == "SKILL.md" {
        parent_folder
    } else {
        file_name
    }
}

/// 构建期的子组暂存（与最终 [`ResourceSubgroup`] 字段相同）。
///
/// 组/子组容器先以可变结构存在，排序完成后才包成 `Rc`——这样容器本身不需要
/// `RefCell`（条目仍用 `Rc<RefCell<ResourceItem>>` 以支持就地改 `enabled`）。
struct DraftSubgroup {
    resource_type: ResourceType,
    label: String,
    items: Vec<Rc<RefCell<ResourceItem>>>,
}

/// 构建期的组暂存（与最终 [`ResourceGroup`] 字段相同）。
///
/// `key` 就是 `origin:scope:source:baseDir` 组成的 `group_key`，既用于分组去重，
/// 也原样写进每个条目的 `group_key`。
struct DraftGroup {
    key: String,
    label: String,
    scope: SourceScope,
    origin: SourceOrigin,
    source: String,
    subgroups: Vec<DraftSubgroup>,
}

/// 把一类资源追加进构建期组表。
///
/// 组键是 `origin:scope:source:base_dir` 的拼接（仅作不透明相等键使用；`source` 本身
/// 可能含 `:`，例如 `npm:...`）。组与子组都在首次出现时创建；条目按 `resources` 的
/// 输入顺序追加，不去重。`expect` 安全：键不存在时上一行刚插入，循环中不删除任何组。
fn add_to_group(
    groups: &mut IndexMap<String, DraftGroup>,
    resources: &[ResolvedResource],
    resource_type: ResourceType,
    agent_dir: &str,
) {
    for resource in resources {
        let metadata = &resource.metadata;
        let group_key = format!(
            "{}:{}:{}:{}",
            origin_str(metadata.origin),
            scope_str(metadata.scope),
            metadata.source,
            metadata.base_dir.as_deref().unwrap_or("")
        );
        if !groups.contains_key(&group_key) {
            groups.insert(
                group_key.clone(),
                DraftGroup {
                    key: group_key.clone(),
                    label: get_group_label(metadata, agent_dir),
                    scope: metadata.scope,
                    origin: metadata.origin,
                    source: metadata.source.clone(),
                    subgroups: Vec::new(),
                },
            );
        }
        let group = groups
            .get_mut(&group_key)
            .expect("group inserted immediately above");
        let subgroup_index = match group
            .subgroups
            .iter()
            .position(|subgroup| subgroup.resource_type == resource_type)
        {
            Some(index) => index,
            None => {
                group.subgroups.push(DraftSubgroup {
                    resource_type,
                    label: resource_type.label().to_string(),
                    items: Vec::new(),
                });
                group.subgroups.len() - 1
            }
        };
        let display_name = resource_display_name(&resource.path, resource_type, &metadata.source);
        group.subgroups[subgroup_index]
            .items
            .push(Rc::new(RefCell::new(ResourceItem {
                path: resource.path.clone(),
                enabled: resource.enabled,
                metadata: metadata.clone(),
                resource_type,
                display_name,
                group_key: group_key.clone(),
                subgroup_key: format!("{group_key}:{}", resource_type.as_str()),
            })));
    }
}

/// `buildGroups(resolved, agentDir)`。
///
/// 阶段：
/// 1. 按固定类型顺序 Extensions → Skills → Prompts → Themes 调用 `add_to_group`；
///    组表是 `IndexMap`，保留组的首次出现顺序。
/// 2. 组排序：package 在 top-level 前；user 在其它 scope 前；前两级都相同时按 `source`
///    做 `locale_compare`。注意两个非 user 且互不相同的 scope（Project / Temporary）
///    互相比较都返回 Greater——比较函数不构成全序（与 TS 的比较函数一致），这一对的
///    相对顺序没有可靠语义。
/// 3. 组内子组按 `type_order` 排序，条目按 `display_name` 的 `locale_compare` 排序；
///    `sort_by` 是稳定排序，比较相等时保留输入顺序。
/// 4. 最后把构建期结构转成 `Rc<ResourceGroup>` / `Rc<ResourceSubgroup>`。
fn build_groups(resolved: &ResolvedPaths, agent_dir: &str) -> Vec<Rc<ResourceGroup>> {
    let mut group_map: IndexMap<String, DraftGroup> = IndexMap::new();
    add_to_group(
        &mut group_map,
        &resolved.extensions,
        ResourceType::Extensions,
        agent_dir,
    );
    add_to_group(
        &mut group_map,
        &resolved.skills,
        ResourceType::Skills,
        agent_dir,
    );
    add_to_group(
        &mut group_map,
        &resolved.prompts,
        ResourceType::Prompts,
        agent_dir,
    );
    add_to_group(
        &mut group_map,
        &resolved.themes,
        ResourceType::Themes,
        agent_dir,
    );

    let mut groups: Vec<DraftGroup> = group_map.into_values().collect();
    groups.sort_by(|left, right| {
        if left.origin != right.origin {
            return if left.origin == SourceOrigin::Package {
                std::cmp::Ordering::Less
            } else {
                std::cmp::Ordering::Greater
            };
        }
        if left.scope != right.scope {
            return if left.scope == SourceScope::User {
                std::cmp::Ordering::Less
            } else {
                std::cmp::Ordering::Greater
            };
        }
        kiana_tui::autocomplete::locale_compare(&left.source, &right.source)
    });

    for group in &mut groups {
        group
            .subgroups
            .sort_by_key(|subgroup| subgroup.resource_type.type_order());
        for subgroup in &mut group.subgroups {
            subgroup.items.sort_by(|left, right| {
                let left = left.borrow().display_name.clone();
                let right = right.borrow().display_name.clone();
                kiana_tui::autocomplete::locale_compare(&left, &right)
            });
        }
    }

    groups
        .into_iter()
        .map(|group| {
            Rc::new(ResourceGroup {
                key: group.key,
                label: group.label,
                scope: group.scope,
                origin: group.origin,
                source: group.source,
                subgroups: group
                    .subgroups
                    .into_iter()
                    .map(|subgroup| {
                        Rc::new(ResourceSubgroup {
                            resource_type: subgroup.resource_type,
                            label: subgroup.label,
                            items: subgroup.items,
                        })
                    })
                    .collect(),
            })
        })
        .collect()
}

// =============================================================================
// 设置读写辅助
// =============================================================================

/// `settings[arrayKey] ?? []`。
///
/// `None` 与 `Some(vec![])` 都得到空 `Vec`，但两者在设置文件里的语义不同（后者表示显式
/// 空数组）；本函数只用于展开当前条目，不区分这两种情况。
fn settings_paths(settings: &Settings, resource_type: ResourceType) -> Vec<String> {
    match resource_type {
        ResourceType::Extensions => settings.extensions.clone(),
        ResourceType::Skills => settings.skills.clone(),
        ResourceType::Prompts => settings.prompts.clone(),
        ResourceType::Themes => settings.themes.clone(),
    }
    .unwrap_or_default()
}

/// `settings.packages ?? []`。
fn settings_packages(settings: &Settings) -> Vec<PackageSource> {
    settings.packages.clone().unwrap_or_default()
}

fn set_global_paths(manager: &SettingsManager, resource_type: ResourceType, paths: Vec<String>) {
    match resource_type {
        ResourceType::Extensions => manager.set_extension_paths(paths),
        ResourceType::Skills => manager.set_skill_paths(paths),
        ResourceType::Prompts => manager.set_prompt_template_paths(paths),
        ResourceType::Themes => manager.set_theme_paths(paths),
    }
}

/// 写 project 设置里的资源数组。
///
/// `SettingsManager` 的项目 setter 返回 `Result`：项目未受信时返回错误且不改内存设置
/// （见该 setter 的文档）。本文件不处理该错误，统一丢弃，因此调用方无法从返回值得知
/// 写入是否被拒绝。
fn set_project_paths(manager: &SettingsManager, resource_type: ResourceType, paths: Vec<String>) {
    let _ = match resource_type {
        ResourceType::Extensions => manager.set_project_extension_paths(paths),
        ResourceType::Skills => manager.set_project_skill_paths(paths),
        ResourceType::Prompts => manager.set_project_prompt_template_paths(paths),
        ResourceType::Themes => manager.set_project_theme_paths(paths),
    };
}

/// 按 `project` 分派到 global / project 的 setter（global 侧无错误返回值）。
fn set_settings_paths(
    manager: &SettingsManager,
    project: bool,
    resource_type: ResourceType,
    paths: Vec<String>,
) {
    if project {
        set_project_paths(manager, resource_type, paths);
    } else {
        set_global_paths(manager, resource_type, paths);
    }
}

/// `typeof pkg === "string" ? pkg : pkg.source`。
///
/// 对象形式没有 `source` 时返回 `None`；调用方据此判断“无法确认同一个包”。
fn package_source_string(package: &PackageSource) -> Option<&str> {
    match package {
        PackageSource::Text(source) => Some(source),
        PackageSource::Object(object) => object.source.as_deref(),
    }
}

/// `pkg[arrayKey]`（可能缺失）。
///
/// 字符串形式的包没有过滤数组，等价于空数组；对象形式 `None` 也返回空数组。
fn package_paths(package: &PackageSource, resource_type: ResourceType) -> Vec<String> {
    let PackageSource::Object(object) = package else {
        return Vec::new();
    };
    match resource_type {
        ResourceType::Extensions => object.extensions.clone(),
        ResourceType::Skills => object.skills.clone(),
        ResourceType::Prompts => object.prompts.clone(),
        ResourceType::Themes => object.themes.clone(),
    }
    .unwrap_or_default()
}

/// `pkg[arrayKey] = updated.length > 0 ? updated : undefined`。
///
/// 只对对象形式生效：字符串包没有数组字段，直接返回。空数组写回 `None`，序列化时该键
/// 会消失，后续 `package_has_filters` 才能观察到“没有过滤器”。
fn set_package_paths(
    package: &mut PackageSource,
    resource_type: ResourceType,
    updated: Vec<String>,
) {
    let PackageSource::Object(object) = package else {
        return;
    };
    let value = if updated.is_empty() {
        None
    } else {
        Some(updated)
    };
    match resource_type {
        ResourceType::Extensions => object.extensions = value,
        ResourceType::Skills => object.skills = value,
        ResourceType::Prompts => object.prompts = value,
        ResourceType::Themes => object.themes = value,
    }
}

/// `["extensions","skills","prompts","themes"].some(k => pkg[k] !== undefined)`。
///
/// 判据是 `Option` 是否为 `Some`：显式空数组 `Some(vec![])` 也算“有过滤器”，因此只有
/// 四个键全被写成 `None` 时才会把对象压缩回字符串。
fn package_has_filters(package: &PackageSource) -> bool {
    match package {
        PackageSource::Text(_) => false,
        PackageSource::Object(object) => {
            object.extensions.is_some()
                || object.skills.is_some()
                || object.prompts.is_some()
                || object.themes.is_some()
        }
    }
}

/// `typeof pkg === "string" ? pkg : pkg.source` 的可变版本（把字符串转成对象）。
///
/// 转换结果只有 `source` 有值，其余 `Option` 字段（含 `autoload`）都是 `None`，
/// 而不是 `Some(false)`——与 TS 里 `{ source: pkg }` 的字段集合一致。
fn package_into_object(package: &mut PackageSource) {
    if let PackageSource::Text(source) = package {
        let source = source.clone();
        *package = PackageSource::Object(PackageSourceObject {
            source: Some(source),
            ..PackageSourceObject::default()
        });
    }
}

/// `getPatternEntryTarget(entry)`。
///
/// 只剥掉**一个**前导字符：`"!+x"` → `"+x"`，`"--x"` → `"-x"`，无前缀时原样返回。
/// 空串与非前缀条目不会被改写。
fn pattern_entry_target(entry: &str) -> &str {
    if let Some(stripped) = entry
        .strip_prefix('!')
        .or_else(|| entry.strip_prefix('+'))
        .or_else(|| entry.strip_prefix('-'))
    {
        stripped
    } else {
        entry
    }
}

/// `entry.startsWith("!") || entry.startsWith("+") || entry.startsWith("-")`。
///
/// 判断“带模式前缀”；前缀集合与 `pattern_entry_target` 的剥离集合一致（`!` / `+` / `-`），
/// 只是返回布尔而不是剥离后的目标。
fn entry_has_prefix(entry: &str) -> bool {
    entry.starts_with('!') || entry.starts_with('+') || entry.starts_with('-')
}

/// TS `theme.fg(color, text)`。
///
/// panic 条件：全局主题尚未 `init_theme`（TS 在同样情形抛错），或当前主题里缺少该颜色
/// token（`ThemeProxy::fg` 返回 `Unknown theme color`）。测试在使用前统一 `init_theme`。
fn fg(color: ThemeColor, text: &str) -> String {
    kiana_theme::THEME
        .fg(color, text)
        .expect("theme must be initialized before rendering (TS throws here)")
}

/// TS `theme.bold(text)`。
///
/// panic 条件同 [`fg`]：全局主题未初始化。`bold` 本身不需要颜色 token。
fn bold(text: &str) -> String {
    kiana_theme::THEME
        .bold(text)
        .expect("theme must be initialized before rendering (TS throws here)")
}

// =============================================================================
// ConfigSelectorHeader
// =============================================================================

/// TS `ConfigSelectorHeader`（模块私有）。
///
/// Rust 侧声明为 `pub`，但当前文件之外没有使用点；实例只由
/// [`ConfigSelectorComponent::new`] 创建并与 [`ResourceList`] 共享。
/// 它只渲染两行表头（标题 + 键位提示、当前 scope 的设置文件提示），不持有资源状态。
/// `project_mode_available` 只影响是否显示 `tab switch mode` 提示；能否真正切换由
/// [`ResourceList`] 是否挂载了 `on_switch_mode` 决定。
pub struct ConfigSelectorHeader {
    write_scope: ConfigWriteScope,
    project_mode_available: bool,
}

impl ConfigSelectorHeader {
    /// `constructor(writeScope, projectModeAvailable)`。
    ///
    /// `project_mode_available == false` 时表头不显示切换提示。就表头而言它只影响这行
    /// 提示；实际的 Tab 切换能力取决于 [`ResourceList`] 是否挂载了 `on_switch_mode`。
    pub fn new(write_scope: ConfigWriteScope, project_mode_available: bool) -> Self {
        ConfigSelectorHeader {
            write_scope,
            project_mode_available,
        }
    }

    /// `setWriteScope(writeScope)`。
    ///
    /// 只改表头自身。调用方必须同时更新 [`ResourceList`] 的 scope，否则表头与列表会
    /// 显示不一致；本文件里唯一负责同步的是私有的 `toggle_write_scope`。
    pub fn set_write_scope(&mut self, write_scope: ConfigWriteScope) {
        self.write_scope = write_scope;
    }

    /// 当前 scope。
    pub fn write_scope(&self) -> ConfigWriteScope {
        self.write_scope
    }
}

impl Component for ConfigSelectorHeader {
    fn render(&mut self, width: usize) -> Vec<String> {
        // 第一行：加粗标题 + 至少 1 个空格 + 键位提示；提示由三段拼成，分隔符统一用
        // 暗色的 " · "。project 模式下空格的动作说明是循环三态，global 模式下是普通开关。
        let title = bold(if self.write_scope == ConfigWriteScope::Project {
            "Project Local Resources"
        } else {
            "Global Resources"
        });
        let separator = fg(ThemeColor::Muted, " · ");
        let switch_hint = if self.project_mode_available {
            format!("{}{separator}", key_hint("tui.input.tab", "switch mode"))
        } else {
            String::new()
        };
        let action_hint = if self.write_scope == ConfigWriteScope::Project {
            raw_key_hint("space", "cycle inherit/+/-")
        } else {
            raw_key_hint("space", "toggle")
        };
        let hint = format!(
            "{switch_hint}{action_hint}{separator}{}",
            raw_key_hint("esc", "close")
        );
        // 与 TS 的 `Math.max(1, width - visibleWidth(title) - visibleWidth(hint))` 一致：
        // 用 f64 计算再转 usize（负数在 Rust 的浮点转整数里饱和为 0，max 保证至少 1）。
        // 宽度不足时随后由 truncate_to_width 截断，提示会被切掉而不是换行。
        let spacing = f64::max(
            1.0,
            width as f64 - visible_width(&title) as f64 - visible_width(&hint) as f64,
        ) as usize;
        // 第二行：当前 scope 对应的设置文件（project 时额外说明继承项会变暗）。
        let scope_hint = if self.write_scope == ConfigWriteScope::Project {
            fg(
                ThemeColor::Muted,
                &format!(
                    "{}/settings.json · inherited global resources are dimmed",
                    kiana_core::config::CONFIG_DIR_NAME
                ),
            )
        } else {
            fg(
                ThemeColor::Muted,
                &format!(
                    "~/{}/agent/settings.json",
                    kiana_core::config::CONFIG_DIR_NAME
                ),
            )
        };
        vec![
            truncate_to_width(
                &format!("{title}{}{hint}", " ".repeat(spacing)),
                width,
                "",
                false,
            ),
            truncate_to_width(&scope_hint, width, "", false),
        ]
    }

    fn handles_input(&self) -> bool {
        // 表头是纯展示组件（TS 的 ConfigSelectorHeader 没有 handleInput）。`Component`
        // 的默认实现返回 true，会让 TUI 把它当成有输入处理器的组件（为它请求立即渲染）；
        // 这里显式返回 false，复现 TS 对纯展示组件的跳过行为。
        false
    }
}

// =============================================================================
// ResourceList
// =============================================================================

/// TS `ResourceList`（模块私有；通过 [`ConfigSelectorComponent::get_resource_list`] 暴露）。
///
/// 自带搜索框的资源列表，负责当前 scope 的展平/过滤、键盘选择、三态切换与渲染。
/// 通常由 [`ConfigSelectorComponent::new`] 构造并挂上回调；`new` 虽是公开的，单独构造时
/// `header` 与 `on_switch_mode` 为空，Tab 不会切换 scope。
pub struct ResourceList {
    /// 两个写入 scope 各自的组树。只在构造时填充；切 scope 只是换一份读取。
    groups_by_scope: IndexMap<ConfigWriteScope, Vec<Rc<ResourceGroup>>>,
    /// 当前 scope 的展平序列（组、子组、条目各一项），顺序即渲染顺序。
    flat_items: Vec<FlatEntry>,
    /// `flat_items` 经搜索过滤后的子序列；选择与渲染都以它为准。
    filtered_items: Vec<FlatEntry>,
    /// 指向 `filtered_items` 的下标。不变量：总是指向 `FlatEntry::Item` 或为 0
    /// （由 `build_flat_list` / `select_first_item` / `find_next_item` 共同维护）。
    /// 用 `isize` 是为了复刻 TS 的有符号下标运算（翻页时会出现负的中间量）。
    selected_index: isize,
    /// 自带渲染与编辑逻辑的搜索框；其值经 `kiana_json::js_trim` 后决定是否过滤。
    search_input: Input,
    /// 视口内最多显示的扁平行数：`max(5, terminal_height - 8)`，8 是外层固定行数
    /// （Spacer / 边框 / 两行表头等），见 TS 构造函数的注释。
    max_visible: usize,
    /// 读写 global/project 设置。`SettingsManager` 是共享状态句柄：clone 之后仍看到
    /// 同一份设置，因此组件写入与调用方读取能保持一致。
    settings_manager: SettingsManager,
    /// project 配置目录的基准（`<cwd>/<CONFIG_DIR_NAME>`）。
    cwd: String,
    /// global 顶层资源的基准目录（agent 目录）。
    agent_dir: String,
    /// 当前写入 scope：决定使用哪份组树、把修改写到哪份设置。
    write_scope: ConfigWriteScope,
    /// 与 [`ConfigSelectorComponent`] 共享的表头。TS 的 `onSwitchMode` 回调在
    /// `switchWriteScope()` 里同时更新 header 与列表；Rust 侧把 header 放进独立
    /// `RefCell`，ResourceList 才能在自身 `handle_input` 内同步完成同一次切换
    /// （宿主可以直接 `get_resource_list().handle_input("\\t")`，不必经过组件）。
    header: Option<Rc<RefCell<ConfigSelectorHeader>>>,
    /// 构造时从 **global** 组树生成的 `资源类型:规范化路径 → enabled` 快照。
    /// project scope 下判断“是否为继承的 global 项”及其原启用值都依赖它；此后不再更新，
    /// 因此它记录的是打开面板时的 global 状态，而不是当前 UI 上可能被切换过的值。
    inherited_enabled_by_key: IndexMap<String, bool>,
    /// Focusable 状态；`set_focused` 会同时同步给 `search_input`。
    focused: bool,
    /// TS `onCancel?`。
    pub on_cancel: Option<Rc<dyn Fn()>>,
    /// TS `onExit?`。
    pub on_exit: Option<Rc<dyn Fn()>>,
    /// TS `onToggle?(item, newEnabled)`。在条目更新之后触发；组件把它接到重绘请求。
    pub on_toggle: Option<ToggleCallback>,
    /// TS `onSwitchMode?`。只有 project 模式可用时才会被 [`ConfigSelectorComponent::new`]
    /// 挂上；Tab 分支先同步切换 scope，再调用它（它本身只负责请求重绘）。
    pub on_switch_mode: Option<Rc<dyn Fn()>>,
}

impl ResourceList {
    /// `constructor(groupsByScope, settingsManager, cwd, agentDir, terminalHeight?, writeScope?)`。
    ///
    /// * `groups_by_scope`：两个写入 scope 的组树。缺少 `Global` 键时继承快照为空；
    ///   缺少某个 scope 的组时该 scope 渲染为“No resources found”。
    /// * `terminal_height`：`None` 按 24 行处理；`max_visible` 再减去 8 行固定 chrome，
    ///   且下限为 5（见字段文档）。
    /// * 构造会立即展平当前 scope 并复制一份 `filtered_items`（搜索词为空，等于全量）。
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        groups_by_scope: IndexMap<ConfigWriteScope, Vec<Rc<ResourceGroup>>>,
        settings_manager: SettingsManager,
        cwd: impl Into<String>,
        agent_dir: impl Into<String>,
        terminal_height: Option<usize>,
        write_scope: ConfigWriteScope,
    ) -> Self {
        // 继承快照只取 global 组树：project scope 的“继承 global 项”判定以打开面板时的
        // global 解析结果为准，之后不再跟随任何切换更新。
        let inherited_enabled_by_key = {
            let empty = Vec::new();
            let global_groups = groups_by_scope
                .get(&ConfigWriteScope::Global)
                .unwrap_or(&empty);
            build_inherited_enabled_map(global_groups)
        };
        // 8 行是组件外层的固定开销（上下 Spacer、两条 DynamicBorder、两行表头等）。
        let max_visible = {
            let height = terminal_height.unwrap_or(24) as isize - 8;
            height.max(5) as usize
        };
        let mut list = ResourceList {
            groups_by_scope,
            flat_items: Vec::new(),
            filtered_items: Vec::new(),
            selected_index: 0,
            search_input: Input::default(),
            max_visible,
            settings_manager,
            cwd: cwd.into(),
            agent_dir: agent_dir.into(),
            write_scope,
            header: None,
            inherited_enabled_by_key,
            focused: false,
            on_cancel: None,
            on_exit: None,
            on_toggle: None,
            on_switch_mode: None,
        };
        list.build_flat_list();
        list.filtered_items = list.flat_items.clone();
        list
    }

    /// `setWriteScope(scope)`：重建平铺列表并用现有搜索词重新过滤。
    ///
    /// 搜索词会保留（测试 `switching_scope_keeps_the_search_query` 锁定这一点），
    /// 但选中位置会回到过滤后的第一个条目（`filter_items` 的固定行为）。
    /// 它不更新表头：表头同步由调用方负责。
    pub fn set_write_scope(&mut self, write_scope: ConfigWriteScope) {
        self.write_scope = write_scope;
        self.build_flat_list();
        let query = self.search_input.get_value().to_string();
        self.filter_items(&query);
    }

    /// TS `onSwitchMode` 回调里的 `switchWriteScope()`（global ⇄ project）。
    ///
    /// TS 在 `ResourceList.handleInput` 的 tab 分支里**同步**调用 `onSwitchMode`，回调
    /// 立刻 `header.setWriteScope(...)` + `resourceList.setWriteScope(...)`。Rust 侧同样
    /// 在这里同步完成，宿主直接驱动 `get_resource_list()` 也能看到 scope/表头变化。
    ///
    /// 顺序是先改共享表头、再重建列表；`header` 为空（单独构造的 ResourceList）时只
    /// 改列表。这里不调用 `on_switch_mode`，回调由 `handle_input` 的 Tab 分支随后触发。
    fn toggle_write_scope(&mut self) {
        let write_scope = self.write_scope.toggled();
        if let Some(header) = &self.header {
            header.borrow_mut().set_write_scope(write_scope);
        }
        self.set_write_scope(write_scope);
    }

    /// 当前写入 scope（列表自身状态；表头是另一份，见 `toggle_write_scope`）。
    pub fn write_scope(&self) -> ConfigWriteScope {
        self.write_scope
    }

    /// 当前选中下标（测试/宿主观察用）。
    ///
    /// 下标指向 `filtered_items` 而不是 `flat_items`；过滤或切换 scope 后会重置到第一个
    /// 条目行。
    pub fn selected_index(&self) -> isize {
        self.selected_index
    }

    /// 搜索框当前值（原始输入，未做 `js_trim`）。
    pub fn search_value(&self) -> String {
        self.search_input.get_value().to_string()
    }

    /// 当前 scope 的组树；该 scope 不在 `groups_by_scope` 里时返回空切片。
    fn groups(&self) -> &[Rc<ResourceGroup>] {
        match self.groups_by_scope.get(&self.write_scope) {
            Some(groups) => groups.as_slice(),
            None => &[],
        }
    }

    /// 把当前 scope 的组树按“组 → 子组 → 条目”展开成 `flat_items`。
    ///
    /// 先克隆一份组列表再清空 `flat_items`，避免同时借用 `&self.groups()` 与
    /// `&mut self.flat_items`。展开用的是 `Rc::clone`，条目身份与组树保持同一份。
    /// 展开后把选中位置放到第一个条目行；没有任何条目时置 0（此时 0 可能落在组行上，
    /// 甚至越界，读取处都按 `get`/边界检查处理）。
    fn build_flat_list(&mut self) {
        let groups: Vec<Rc<ResourceGroup>> = self.groups().to_vec();
        self.flat_items.clear();
        for group in groups {
            self.flat_items.push(FlatEntry::Group(Rc::clone(&group)));
            for subgroup in &group.subgroups {
                self.flat_items
                    .push(FlatEntry::Subgroup(Rc::clone(subgroup), Rc::clone(&group)));
                for item in &subgroup.items {
                    self.flat_items.push(FlatEntry::Item(Rc::clone(item)));
                }
            }
        }
        self.selected_index = self
            .flat_items
            .iter()
            .position(|entry| matches!(entry, FlatEntry::Item(_)))
            .map(|index| index as isize)
            .unwrap_or(0);
    }

    /// 从 `from_index` 沿 `direction` 找最近的条目行；找不到时返回 `from_index`。
    ///
    /// 不环绕：到列表两端就停住，因此上下键在边界处不会跳回另一端。
    fn find_next_item(&self, from_index: isize, direction: isize) -> isize {
        let mut index = from_index + direction;
        while index >= 0 && index < self.filtered_items.len() as isize {
            if matches!(self.filtered_items[index as usize], FlatEntry::Item(_)) {
                return index;
            }
            index += direction;
        }
        from_index
    }

    /// 选中过滤结果中的第一个条目行；没有条目时置 0。
    fn select_first_item(&mut self) {
        let first_item_index = self
            .filtered_items
            .iter()
            .position(|entry| matches!(entry, FlatEntry::Item(_)));
        self.selected_index = first_item_index.map(|index| index as isize).unwrap_or(0);
    }

    /// 按搜索词过滤 `flat_items`，重建 `filtered_items`。
    ///
    /// * 判空用 [`kiana_json::js_trim`]（JS `String.prototype.trim` 的等价物），只有
    ///   “trim 后为空”才显示全量。单按空格会先被切换分支消费，因此空格键本身无法
    ///   进入搜索词。
    /// * 命中条件：查询串与显示名、资源类型字面量或完整路径做不区分大小写的子串匹配。
    /// * 任一条目命中，其所属子组与组也保留；输出保持 `flat_items` 的原顺序
    ///   （组在子组前、子组在条目前的结构不会被打乱）。
    /// * 过滤完成后把选中位置放到第一个命中条目。
    ///
    /// 命中集合用 `*const` 指针做身份键（对应 TS 的 `Set<ResourceItem>` 引用语义）：
    /// 这里只比较指针、不解引用，`Rc` 在函数执行期间由 `flat_items`/组树持有，因此
    /// 不会悬垂；值相同但分配不同的两个条目不会被合并。
    fn filter_items(&mut self, query: &str) {
        if kiana_json::js_trim(query).is_empty() {
            self.filtered_items = self.flat_items.clone();
            self.select_first_item();
            return;
        }

        let lower_query = query.to_lowercase();
        let mut matching_items: HashSet<*const RefCell<ResourceItem>> = HashSet::new();
        let mut matching_subgroups: HashSet<*const ResourceSubgroup> = HashSet::new();
        let mut matching_groups: HashSet<*const ResourceGroup> = HashSet::new();

        for entry in &self.flat_items {
            if let FlatEntry::Item(item) = entry {
                let item_ref = item.borrow();
                if item_ref.display_name.to_lowercase().contains(&lower_query)
                    || item_ref.resource_type.as_str().contains(&lower_query)
                    || item_ref.path.to_lowercase().contains(&lower_query)
                {
                    matching_items.insert(Rc::as_ptr(item));
                }
            }
        }

        for group in self.groups() {
            for subgroup in &group.subgroups {
                for item in &subgroup.items {
                    if matching_items.contains(&Rc::as_ptr(item)) {
                        matching_subgroups.insert(Rc::as_ptr(subgroup));
                        matching_groups.insert(Rc::as_ptr(group));
                    }
                }
            }
        }

        let mut filtered = Vec::new();
        for entry in &self.flat_items {
            match entry {
                FlatEntry::Group(group) if matching_groups.contains(&Rc::as_ptr(group)) => {
                    filtered.push(entry.clone());
                }
                FlatEntry::Subgroup(subgroup, _)
                    if matching_subgroups.contains(&Rc::as_ptr(subgroup)) =>
                {
                    filtered.push(entry.clone());
                }
                FlatEntry::Item(item) if matching_items.contains(&Rc::as_ptr(item)) => {
                    filtered.push(entry.clone());
                }
                _ => {}
            }
        }
        self.filtered_items = filtered;
        self.select_first_item();
    }

    /// `updateItem(item, enabled)`：改条目本身，并同步组树中的**第一份**匹配项。
    ///
    /// 第一段直接改传入的 `Rc`（通常来自 `filtered_items`）；第二段在**当前 scope** 的
    /// 组树里按 `path + resource_type` 找到第一份并赋同样的值，找到即返回。正常情况两者
    /// 是同一个分配，第二段只是重复赋值；若同一 scope 内存在重复条目，后面的同路径条目
    /// 不会被一并更新。
    pub fn update_item(&mut self, item: &Rc<RefCell<ResourceItem>>, enabled: bool) {
        let (path, resource_type) = {
            let mut item_ref = item.borrow_mut();
            item_ref.enabled = enabled;
            (item_ref.path.clone(), item_ref.resource_type)
        };
        let groups: Vec<Rc<ResourceGroup>> = self.groups().to_vec();
        for group in groups {
            for subgroup in &group.subgroups {
                for found in &subgroup.items {
                    let matches = {
                        let found_ref = found.borrow();
                        found_ref.path == path && found_ref.resource_type == resource_type
                    };
                    if matches {
                        found.borrow_mut().enabled = enabled;
                        return;
                    }
                }
            }
        }
    }

    /// 选中项（用于测试）。
    ///
    /// 判定 `selected_index` 落在 `FlatEntry::Item` 行；越界或指向组/子组行时返回 `false`。
    pub fn selected_entry_is_item(&self) -> bool {
        self.filtered_items
            .get(self.selected_index.max(0) as usize)
            .is_some_and(|entry| matches!(entry, FlatEntry::Item(_)))
    }

    // -- 渲染辅助 ---------------------------------------------------------

    /// 勾选框文本：project scope 优先显示覆盖状态（`[+]` 用 Success 色 / `[-]` 用
    /// Warning 色），`inherit` 才回落到 `item.enabled` 的 `[x]`/`[ ]`（Dim 色）；
    /// global scope 只看 `item.enabled`。
    fn render_checkbox(&self, item: &ResourceItem) -> String {
        if self.write_scope == ConfigWriteScope::Project {
            match self.get_project_override_state(item) {
                ProjectOverrideState::Load => fg(ThemeColor::Success, "[+]"),
                ProjectOverrideState::Unload => fg(ThemeColor::Warning, "[-]"),
                ProjectOverrideState::Inherit => {
                    fg(ThemeColor::Dim, if item.enabled { "[x]" } else { "[ ]" })
                }
            }
        } else if item.enabled {
            fg(ThemeColor::Success, "[x]")
        } else {
            fg(ThemeColor::Dim, "[ ]")
        }
    }

    /// 条目行尾的附加说明，只在 project scope 下非空：
    /// `project load` / `project unload` 对应显式覆盖；`inherit` 且属于继承项时显示
    /// `inherited global`，其余为空。
    fn get_item_suffix(&self, item: &ResourceItem) -> String {
        if self.write_scope != ConfigWriteScope::Project {
            return String::new();
        }
        match self.get_project_override_state(item) {
            ProjectOverrideState::Load => fg(ThemeColor::Muted, "  project load"),
            ProjectOverrideState::Unload => fg(ThemeColor::Muted, "  project unload"),
            ProjectOverrideState::Inherit => {
                if self.is_inherited_global_item(item) {
                    fg(ThemeColor::Dim, "  inherited global")
                } else {
                    String::new()
                }
            }
        }
    }

    /// 条目名是否变暗：project scope、继承自 global、且当前覆盖状态为 `inherit`。
    ///
    /// 三个条件必须同时成立；被 `+`/`-` 覆盖的继承项按正常前景色显示。
    fn is_dimmed_item(&self, item: &ResourceItem) -> bool {
        self.write_scope == ConfigWriteScope::Project
            && self.is_inherited_global_item(item)
            && self.get_project_override_state(item) == ProjectOverrideState::Inherit
    }

    // -- 切换 -------------------------------------------------------------

    /// 切换一个条目并返回新的**有效启用值**；`None` 表示没有可应用的修改。
    ///
    /// * project scope：先按三态循环算出目标状态再写项目设置；目标为 `inherit` 时返回
    ///   继承的 global 启用值，`load` / `unload` 分别返回 `true` / `false`。私有 setter
    ///   返回 `false`（包条目在项目设置里不存在、且目标恰好是 `inherit`）时什么都没写，
    ///   这里返回 `None`。
    /// * global scope：直接翻转 `item.enabled`，按 `metadata.origin` 走顶层资源或包资源
    ///   两条写路径，返回翻转后的值。
    ///
    /// 本函数只写设置，不改 `ResourceItem`、不触发回调；调用方随后做 `update_item` 与
    /// `on_toggle`。写入目标（global/project 设置文件）由**条目自身的 `metadata.scope`**
    /// 决定，而是否走三态循环由列表的 `write_scope` 决定。
    fn toggle_resource(&mut self, item: &Rc<RefCell<ResourceItem>>) -> Option<bool> {
        // 先克隆快照：后面要多次借用/计算，避免在调用 setter 期间仍持有 RefCell 借用。
        let snapshot = item.borrow().clone();
        if self.write_scope == ConfigWriteScope::Project {
            let state = self.get_next_override_state(&snapshot);
            if !self.set_project_resource_override(&snapshot, state) {
                return None;
            }
            return Some(if state == ProjectOverrideState::Inherit {
                self.get_inherited_enabled(&snapshot)
            } else {
                state == ProjectOverrideState::Load
            });
        }
        let enabled = !snapshot.enabled;
        if snapshot.metadata.origin == SourceOrigin::TopLevel {
            self.toggle_top_level_resource(&snapshot, enabled);
        } else {
            self.toggle_package_resource(&snapshot, enabled);
        }
        Some(enabled)
    }

    /// 顶层资源在 global 模式下的开关：把 `+pattern` / `-pattern` 写进对应 scope 的资源
    /// 数组。
    ///
    /// 先从条目自身 `metadata.scope` 对应的设置取当前数组，删掉所有“目标等于当前
    /// pattern”的旧条目（不区分 `!`/`+`/`-` 前缀），再追加一条带符号的模式；因此同一
    /// pattern 最多保留一条，且被移到数组末尾。project 数组的写入错误同样被丢弃。
    fn toggle_top_level_resource(&mut self, item: &ResourceItem, enabled: bool) {
        let project = item.metadata.scope == SourceScope::Project;
        let settings = if project {
            self.settings_manager.get_project_settings()
        } else {
            self.settings_manager.get_global_settings()
        };
        let current = settings_paths(&settings, item.resource_type);
        let pattern = self.get_resource_pattern(item);
        let disable_pattern = format!("-{pattern}");
        let enable_pattern = format!("+{pattern}");
        let mut updated: Vec<String> = current
            .into_iter()
            .filter(|entry| pattern_entry_target(entry) != pattern)
            .collect();
        if enabled {
            updated.push(enable_pattern);
        } else {
            updated.push(disable_pattern);
        }
        set_settings_paths(&self.settings_manager, project, item.resource_type, updated);
    }

    /// 包资源在 global 模式下的开关：改对应包对象里的资源数组。
    ///
    /// 按 `source` 的**原始字符串**（字符串包本身或对象的 `source` 字段）在当前 scope 的
    /// `packages` 里找第一个匹配项；找不到就直接返回——不报错、也不改设置。注意这里不用
    /// `package_source_string_matches` 的路径归一化比较，所以同一路径的不同文本形式
    /// （相对/绝对）不会被视为同一个包。
    ///
    /// 找到后：字符串包先转成对象；删掉目标等于当前 pattern 的旧条目；追加 `+`/`-`
    /// 模式；若四个资源数组全部为空，再把对象压缩回字符串形式（`autoload` 等字段随
    /// 之丢弃，与 TS 的 `packages[i] = pkg.source` 一致）。最后写回条目自身 scope 的
    /// `packages`，project 侧的写入错误被丢弃。
    fn toggle_package_resource(&mut self, item: &ResourceItem, enabled: bool) {
        let project = item.metadata.scope == SourceScope::Project;
        let settings = if project {
            self.settings_manager.get_project_settings()
        } else {
            self.settings_manager.get_global_settings()
        };
        let mut packages = settings_packages(&settings);
        let Some(package_index) = packages.iter().position(|package| {
            package_source_string(package) == Some(item.metadata.source.as_str())
        }) else {
            return;
        };

        package_into_object(&mut packages[package_index]);
        let current = package_paths(&packages[package_index], item.resource_type);
        let pattern = self.get_package_resource_pattern(item);
        let disable_pattern = format!("-{pattern}");
        let enable_pattern = format!("+{pattern}");
        let mut updated: Vec<String> = current
            .into_iter()
            .filter(|entry| pattern_entry_target(entry) != pattern)
            .collect();
        if enabled {
            updated.push(enable_pattern);
        } else {
            updated.push(disable_pattern);
        }
        set_package_paths(&mut packages[package_index], item.resource_type, updated);

        if !package_has_filters(&packages[package_index])
            && let PackageSource::Object(object) = &packages[package_index]
        {
            packages[package_index] =
                PackageSource::Text(object.source.clone().unwrap_or_default());
        }

        if project {
            let _ = self.settings_manager.set_project_packages(packages);
        } else {
            self.settings_manager.set_packages(packages);
        }
    }

    /// 按 `metadata.origin` 分派到顶层资源或包资源的 project 覆盖写入。
    ///
    /// 返回 `false` 表示没有可应用的修改（目前只有“包在项目设置里不存在且目标状态是
    /// inherit”这一种情况），调用方据此跳过 UI 更新。
    fn set_project_resource_override(
        &mut self,
        item: &ResourceItem,
        state: ProjectOverrideState,
    ) -> bool {
        if item.metadata.origin == SourceOrigin::TopLevel {
            self.set_project_top_level_override(item, state)
        } else {
            self.set_project_package_override(item, state)
        }
    }

    /// 顶层资源的 project 覆盖写入；始终返回 `true`（写入错误被丢弃）。
    ///
    /// 步骤：
    /// 1. 从 project 设置取该资源类型的数组。
    /// 2. 计算用于删除/追加的 `pattern`：继承的 global 项用**绝对路径**点名，否则用
    ///    `get_resource_pattern_for_scope(item, true)`。
    /// 3. 删除所有“带前缀且目标在 `patterns` 候选集里”的旧覆盖条目；若目标状态是
    ///    `inherit`、条目是继承项且目标等于 `pattern`，再删掉无前缀的点名条目。
    /// 4. 目标不是 `inherit` 时：继承项、非 builtin、且数组里还没有该绝对路径时先补一条
    ///    无前缀的 `pattern`（让项目解析器“点名”覆盖继承文件；builtin 不需要），再追加
    ///    `+pattern`（load）或 `-pattern`（unload）。
    /// 5. 写回 project 数组；`set_project_*` 的 `Err` 被忽略。
    fn set_project_top_level_override(
        &mut self,
        item: &ResourceItem,
        state: ProjectOverrideState,
    ) -> bool {
        let current = settings_paths(
            &self.settings_manager.get_project_settings(),
            item.resource_type,
        );
        let inherited = self.is_inherited_global_item(item);
        let pattern = if inherited {
            item.path.clone()
        } else {
            self.get_resource_pattern_for_scope(item, true)
        };
        let patterns = self.get_top_level_override_patterns(item, true);
        let mut updated: Vec<String> = current
            .into_iter()
            .filter(|entry| {
                let target = pattern_entry_target(entry);
                if entry_has_prefix(entry) && patterns.iter().any(|pattern| pattern == target) {
                    return false;
                }
                !(state == ProjectOverrideState::Inherit
                    && self.is_inherited_global_item(item)
                    && target == pattern)
            })
            .collect();
        if state != ProjectOverrideState::Inherit {
            // Project 条目要「点名」被覆盖的继承文件；built-in 路径无需条目。
            if inherited && item.metadata.source != "builtin" && !updated.contains(&pattern) {
                updated.push(pattern.clone());
            }
            updated.push(format!(
                "{}{}",
                if state == ProjectOverrideState::Load {
                    "+"
                } else {
                    "-"
                },
                pattern
            ));
        }
        match item.resource_type {
            ResourceType::Extensions => {
                let _ = self.settings_manager.set_project_extension_paths(updated);
            }
            ResourceType::Skills => {
                let _ = self.settings_manager.set_project_skill_paths(updated);
            }
            ResourceType::Prompts => {
                let _ = self
                    .settings_manager
                    .set_project_prompt_template_paths(updated);
            }
            ResourceType::Themes => {
                let _ = self.settings_manager.set_project_theme_paths(updated);
            }
        }
        true
    }

    /// 包资源的 project 覆盖写入。
    ///
    /// 1. 在 project 的 `packages` 里按归一化路径比较（`package_source_string_matches`）
    ///    找包；找不到且目标状态是 `inherit` 时返回 `false`（没有可删的东西）。
    /// 2. 找不到但目标不是 `inherit` 时，用 `create_package_override_source` 新建一个
    ///    `autoload: false` 的包对象并追加到末尾。
    /// 3. 转成对象后删除目标等于当前 package pattern 的旧条目，目标非 `inherit` 时追加
    ///    `+`/`-`。
    /// 4. 若四个资源数组全空：`autoload == Some(false)` 时整条包从数组移除，否则压缩回
    ///    字符串。
    /// 5. 写回 project `packages`；写入错误被忽略。成功走到这里都返回 `true`。
    fn set_project_package_override(
        &mut self,
        item: &ResourceItem,
        state: ProjectOverrideState,
    ) -> bool {
        let mut packages = settings_packages(&self.settings_manager.get_project_settings());
        let mut package_index = packages.iter().position(|package| {
            self.package_source_string_matches(
                &item.metadata.source,
                self.get_item_scope_is_project(item),
                package_source_string(package),
                true,
            )
        });
        if package_index.is_none() {
            if state == ProjectOverrideState::Inherit {
                return false;
            }
            packages.push(self.create_package_override_source(item));
            package_index = Some(packages.len() - 1);
        }
        let Some(index) = package_index else {
            return false;
        };
        package_into_object(&mut packages[index]);
        let pattern = self.get_package_resource_pattern(item);
        let current = package_paths(&packages[index], item.resource_type);
        let mut updated: Vec<String> = current
            .into_iter()
            .filter(|entry| pattern_entry_target(entry) != pattern)
            .collect();
        if state != ProjectOverrideState::Inherit {
            updated.push(format!(
                "{}{}",
                if state == ProjectOverrideState::Load {
                    "+"
                } else {
                    "-"
                },
                pattern
            ));
        }
        set_package_paths(&mut packages[index], item.resource_type, updated);
        if !package_has_filters(&packages[index]) {
            let (autoload, source) = match &packages[index] {
                PackageSource::Text(source) => (false, source.clone()),
                PackageSource::Object(object) => (
                    object.autoload == Some(false),
                    object.source.clone().unwrap_or_default(),
                ),
            };
            if autoload {
                packages.remove(index);
            } else {
                packages[index] = PackageSource::Text(source);
            }
        }
        let _ = self.settings_manager.set_project_packages(packages);
        true
    }

    /// 计算空格键的下一覆盖状态（三态循环的核心，TS 同名逻辑）。
    ///
    /// 先取 `inherited_enabled`（该条目在 global 快照里的启用值），再按当前状态转换：
    ///
    /// | 当前状态 | inherited_enabled = true | inherited_enabled = false |
    /// |----------|--------------------------|---------------------------|
    /// | inherit  | unload                   | load                      |
    /// | unload   | load                     | inherit                   |
    /// | load     | inherit                  | unload                    |
    ///
    /// 直观解释：从 `inherit` 出发的第一次按键总是翻转“有效启用值”，此后继续翻转，
    /// 第三次回到 `inherit`。写回设置时顶层继承项最多留下两条条目（点名条目 + 带符号
    /// 条目），包条目只留一条带符号条目。
    fn get_next_override_state(&self, item: &ResourceItem) -> ProjectOverrideState {
        let state = self.get_project_override_state(item);
        let inherited_enabled = self.get_inherited_enabled(item);
        match state {
            ProjectOverrideState::Inherit => {
                if inherited_enabled {
                    ProjectOverrideState::Unload
                } else {
                    ProjectOverrideState::Load
                }
            }
            ProjectOverrideState::Unload => {
                if inherited_enabled {
                    ProjectOverrideState::Load
                } else {
                    ProjectOverrideState::Inherit
                }
            }
            ProjectOverrideState::Load => {
                if inherited_enabled {
                    ProjectOverrideState::Inherit
                } else {
                    ProjectOverrideState::Unload
                }
            }
        }
    }

    /// 从项目设置推导条目的覆盖状态（TS `getProjectOverrideState`）。
    ///
    /// * 非 project 写入 scope：一律返回 `inherit`。
    /// * 顶层资源：读取 project 设置里该资源类型的数组，与
    ///   `get_top_level_override_patterns(item, true)` 的候选集匹配；空数组不算
    ///   unload（`empty_array_is_unload = false`），因此空数组得到 `inherit`。
    /// * 包资源：在 project `packages` 里找第一个匹配的包；包不存在、是字符串形式、
    ///   或该类型的数组为 `None` 都返回 `inherit`。数组存在时 `empty_array_is_unload`
    ///   取 `autoload != Some(false)`：没有显式关闭 autoload 的包，空数组表示什么都不
    ///   加载（`unload`）；显式 `autoload: false` 的增量包，空数组表示没有覆盖
    ///   （`inherit`）。判定细节见 `get_override_state_from_entries`。
    fn get_project_override_state(&self, item: &ResourceItem) -> ProjectOverrideState {
        if self.write_scope != ConfigWriteScope::Project {
            return ProjectOverrideState::Inherit;
        }
        if item.metadata.origin == SourceOrigin::TopLevel {
            return self.get_override_state_from_entries(
                &settings_paths(
                    &self.settings_manager.get_project_settings(),
                    item.resource_type,
                ),
                &self.get_top_level_override_patterns(item, true),
                false,
            );
        }
        let Some(package) = self.find_matching_package_source(item, true) else {
            return ProjectOverrideState::Inherit;
        };
        let PackageSource::Object(object) = &package else {
            return ProjectOverrideState::Inherit;
        };
        let entries = match item.resource_type {
            ResourceType::Extensions => object.extensions.clone(),
            ResourceType::Skills => object.skills.clone(),
            ResourceType::Prompts => object.prompts.clone(),
            ResourceType::Themes => object.themes.clone(),
        };
        let Some(entries) = entries else {
            return ProjectOverrideState::Inherit;
        };
        self.get_override_state_from_entries(
            &entries,
            &[self.get_package_resource_pattern(item)],
            object.autoload != Some(false),
        )
    }

    /// 从一组模式条目推导单个条目的覆盖状态。
    ///
    /// * `entries` 为空且 `empty_array_is_unload`：返回 `Unload`。该参数由调用方给出：
    ///   包资源传入 `autoload != Some(false)`——“加载这个包但过滤器显式为空”等价于
    ///   什么都不加载；`autoload: false` 的增量包则传入 `false`（空数组 = 无覆盖）。
    /// * 否则从 `Inherit` 开始顺序扫描：只处理剥掉一个前缀后命中 `patterns` 的条目；
    ///   `!` / `-` 前缀置 `Unload`，其余（`+` 或无前缀）置 `Load`。后面的命中会覆盖
    ///   前面的结果，即最后一条匹配条目说了算。
    fn get_override_state_from_entries(
        &self,
        entries: &[String],
        patterns: &[String],
        empty_array_is_unload: bool,
    ) -> ProjectOverrideState {
        if entries.is_empty() && empty_array_is_unload {
            return ProjectOverrideState::Unload;
        }
        let mut state = ProjectOverrideState::Inherit;
        for entry in entries {
            if !patterns
                .iter()
                .any(|pattern| pattern == pattern_entry_target(entry))
            {
                continue;
            }
            if entry.starts_with('!') || entry.starts_with('-') {
                state = ProjectOverrideState::Unload;
            } else {
                state = ProjectOverrideState::Load;
            }
        }
        state
    }

    /// 继承的启用值：优先查构造时的 global 快照；快照没有该键时，project-scope 条目
    /// 回退为 `true`（假定启用），user-scope 条目回退为当前 `item.enabled`。
    fn get_inherited_enabled(&self, item: &ResourceItem) -> bool {
        self.inherited_enabled_by_key
            .get(&self.get_resource_item_key(item))
            .copied()
            .unwrap_or_else(|| {
                if self.get_item_scope_is_project(item) {
                    true
                } else {
                    item.enabled
                }
            })
    }

    /// 是否为“继承自 global 的条目”：user scope 恒为 `true`；project scope 只有当
    /// `资源类型:canonicalize_path(path)` 也出现在 global 快照中时才为 `true`。
    ///
    /// 它决定 project scope 下是否加 `inherited global` 标记、条目是否变暗，以及
    /// 覆盖写入时用绝对路径还是相对 pattern。
    fn is_inherited_global_item(&self, item: &ResourceItem) -> bool {
        !self.get_item_scope_is_project(item)
            || self
                .inherited_enabled_by_key
                .contains_key(&self.get_resource_item_key(item))
    }

    /// 生成用于匹配 project 设置条目的候选 pattern 集合（TS 是 `Set`，Rust 用 `Vec`）。
    ///
    /// 依次收集：`get_resource_pattern_for_scope(item, project)`、条目绝对路径、相对
    /// `get_top_level_base_dir(project)` 的路径，以及（若 `metadata.base_dir` 存在）
    /// 相对该目录的路径。同一条目在不同设置文件里可能以不同文本形式出现，匹配时必须
    /// 同时接受这些形式。返回值只用于成员判断，重复值无影响。
    fn get_top_level_override_patterns(&self, item: &ResourceItem, project: bool) -> Vec<String> {
        let base_dir = self.get_top_level_base_dir(project);
        let mut patterns = vec![
            self.get_resource_pattern_for_scope(item, project),
            item.path.clone(),
            node_relative(&base_dir, &item.path),
        ];
        if let Some(metadata_base_dir) = &item.metadata.base_dir {
            patterns.push(node_relative(metadata_base_dir, &item.path));
        }
        patterns
    }

    /// 计算相对 `project` 目标 scope 的资源 pattern。
    ///
    /// 若目标 scope 与条目自身 scope 不同，或资源来自 `builtin`，直接返回绝对路径
    /// （这两种情况不用相对形式）；否则返回相对 `metadata.base_dir`（缺失时用该 scope
    /// 的顶层基准目录）的路径。
    fn get_resource_pattern_for_scope(&self, item: &ResourceItem, project: bool) -> String {
        let source_is_project = self.get_item_scope_is_project(item);
        if project != source_is_project || item.metadata.source == "builtin" {
            return item.path.clone();
        }
        let base_dir = item
            .metadata
            .base_dir
            .clone()
            .unwrap_or_else(|| self.get_top_level_base_dir(source_is_project));
        node_relative(&base_dir, &item.path)
    }

    /// 为“项目里还没有这个包”的覆盖创建一条包来源。
    ///
    /// * 非本地来源（`npm:`/`git:` 等）：原样保留 source，并设 `autoload: false`。
    ///   在包解析里，`autoload: false` 的项目条目是对同名 user 条目的增量
    ///   （见 `kiana-resources` 的 `dedupe_packages` / `find_autoload_delta_base`），
    ///   而不是重新加载整个包。
    /// * 本地来源：把 source 文本按**条目自身 scope** 的顶层基准目录解析成路径，再表示成
    ///   相对 project 顶层基准目录的路径（相对路径为空时写作 `"."`）。这里用的是顶层
    ///   基准目录而不是 `metadata.base_dir`（TS 同样如此）。
    ///
    /// 两种结果都带 `autoload: false`，其余字段为默认值。
    fn create_package_override_source(&self, item: &ResourceItem) -> PackageSource {
        let source = item.metadata.source.clone();
        if !is_local_path(&source) {
            return PackageSource::Object(PackageSourceObject {
                source: Some(source),
                autoload: Some(false),
                ..PackageSourceObject::default()
            });
        }
        let source_path = resolve_path(
            &source,
            &self.get_top_level_base_dir(self.get_item_scope_is_project(item)),
            &PathInputOptions {
                trim: true,
                ..PathInputOptions::default()
            },
        );
        let relative = node_relative(&self.get_top_level_base_dir(true), &source_path);
        PackageSource::Object(PackageSourceObject {
            source: Some(if relative.is_empty() {
                ".".to_string()
            } else {
                relative
            }),
            autoload: Some(false),
            ..PackageSourceObject::default()
        })
    }

    /// 判断两个包来源是否指向同一个包。
    ///
    /// 先做原始字符串相等比较；不等时，只有两边都是本地路径才继续：各自按自己的 scope
    /// 顶层基准目录用 `resolve_path` 规范化后比较。`resolve_path` 是纯词法解析
    /// （不检查路径是否存在），因此这里不访问文件系统，也不解析 `npm:` 等 URL——
    /// 非本地来源只能靠字符串完全相等来匹配。
    fn package_source_string_matches(
        &self,
        left_source: &str,
        left_is_project: bool,
        right_source: Option<&str>,
        right_is_project: bool,
    ) -> bool {
        let Some(right_source) = right_source else {
            return false;
        };
        if left_source == right_source {
            return true;
        }
        if !is_local_path(left_source) || !is_local_path(right_source) {
            return false;
        }
        let left = resolve_path(
            left_source,
            &self.get_top_level_base_dir(left_is_project),
            &PathInputOptions {
                trim: true,
                ..PathInputOptions::default()
            },
        );
        let right = resolve_path(
            right_source,
            &self.get_top_level_base_dir(right_is_project),
            &PathInputOptions {
                trim: true,
                ..PathInputOptions::default()
            },
        );
        left == right
    }

    /// 在指定 scope 的 `packages` 里找第一个与条目来源匹配的包，返回其克隆。
    ///
    /// `project == true` 时读项目设置，否则读全局设置；顺序就是设置数组的顺序，
    /// `None` 表示没有匹配项。调用方拿到的是快照，修改它不会影响设置管理器。
    fn find_matching_package_source(
        &self,
        item: &ResourceItem,
        project: bool,
    ) -> Option<PackageSource> {
        let settings = if project {
            self.settings_manager.get_project_settings()
        } else {
            self.settings_manager.get_global_settings()
        };
        settings_packages(&settings).into_iter().find(|package| {
            self.package_source_string_matches(
                &item.metadata.source,
                self.get_item_scope_is_project(item),
                package_source_string(package),
                project,
            )
        })
    }

    /// 继承快照的键：`资源类型:canonicalize_path(path)`。
    ///
    /// `canonicalize_path` 会跟随符号链接做 realpath 解析，失败时原样返回输入
    /// （见 `kiana_core::paths` 的文档），因此同一个已存在文件的不同文本路径能得到同一个
    /// 键；不存在的路径则按原字符串区分。渲染与切换时都会重复计算这个键。
    fn get_resource_item_key(&self, item: &ResourceItem) -> String {
        format!(
            "{}:{}",
            item.resource_type.as_str(),
            canonicalize_path(&item.path)
        )
    }

    /// 条目自身 scope 是否为 project；`SourceScope::Temporary` 归入 user 一侧
    /// （与 TS `getItemScope` 只把 `"project"` 当 project 一致）。
    fn get_item_scope_is_project(&self, item: &ResourceItem) -> bool {
        item.metadata.scope == SourceScope::Project
    }

    /// 顶层资源的基准目录：project 是 `<cwd>/<CONFIG_DIR_NAME>`，否则是 `agent_dir`。
    fn get_top_level_base_dir(&self, project: bool) -> String {
        if project {
            node_join(&[&self.cwd, kiana_core::config::CONFIG_DIR_NAME])
        } else {
            self.agent_dir.clone()
        }
    }

    /// 顶层资源在 global 模式写设置时用的 pattern：`builtin` 直接用绝对路径；否则相对
    /// `metadata.base_dir`（缺失时用条目自身 scope 的顶层基准目录）。
    fn get_resource_pattern(&self, item: &ResourceItem) -> String {
        if item.metadata.source == "builtin" {
            return item.path.clone();
        }
        let project = item.metadata.scope == SourceScope::Project;
        let base_dir = item
            .metadata
            .base_dir
            .clone()
            .unwrap_or_else(|| self.get_top_level_base_dir(project));
        node_relative(&base_dir, &item.path)
    }

    /// 包资源写设置时用的 pattern：相对 `metadata.base_dir`（包根）；缺失时退化为相对
    /// 条目路径的父目录，此时结果就是文件名。
    fn get_package_resource_pattern(&self, item: &ResourceItem) -> String {
        let base_dir = item
            .metadata
            .base_dir
            .clone()
            .unwrap_or_else(|| node_dirname(&item.path));
        node_relative(&base_dir, &item.path)
    }
}

/// `buildInheritedEnabledMap(groups)`。
///
/// 遍历传入的组树（调用方只传 global），生成 `资源类型:canonicalize_path(path) → enabled`
/// 的 `IndexMap`。键碰撞时后写的覆盖先写的（`IndexMap::insert` 语义），因此同一路径在
/// global 侧出现多条时只有最后一条的 `enabled` 生效。
fn build_inherited_enabled_map(groups: &[Rc<ResourceGroup>]) -> IndexMap<String, bool> {
    let mut result = IndexMap::new();
    for group in groups {
        for subgroup in &group.subgroups {
            for item in &subgroup.items {
                let item_ref = item.borrow();
                let key = format!(
                    "{}:{}",
                    item_ref.resource_type.as_str(),
                    canonicalize_path(&item_ref.path)
                );
                result.insert(key, item_ref.enabled);
            }
        }
    }
    result
}

impl Component for ResourceList {
    fn render(&mut self, width: usize) -> Vec<String> {
        // 搜索框 + 一个空行；即使列表为空也保留这两行（TS 相同）。
        let mut lines: Vec<String> = Vec::new();
        lines.extend(self.search_input.render(width));
        lines.push(String::new());

        if self.filtered_items.is_empty() {
            // TS 直接 push 未截断的 `theme.fg("muted", "  No resources found")`；spec
            // §2.52.5 明确建议 Rust 侧仍截断（宽度 < 20 时 TS 的 TUI 会因超宽抛错），
            // 文本本身保持一致，只有窄宽度下的 ANSI 复位序列不同。
            lines.push(truncate_to_width(
                &fg(ThemeColor::Muted, "  No resources found"),
                width,
                "",
                false,
            ));
            return lines;
        }

        // 视口计算：以选中行为中心向上取半屏，再把起点夹到 [0, len - max_visible]，
        // 因此靠近列表末尾时窗口会贴底；`len < max_visible` 时 `len - max_visible`
        // 为负，min 后经 max(0) 得到 0。
        let length = self.filtered_items.len() as isize;
        let half = (self.max_visible / 2) as isize;
        let start_index = isize::max(
            0,
            isize::min(
                self.selected_index - half,
                length - self.max_visible as isize,
            ),
        );
        // end 用 min 防越界；此时 start_index 已保证非负。
        let end_index = usize::min(
            (start_index + self.max_visible as isize) as usize,
            self.filtered_items.len(),
        );

        for index in start_index as usize..end_index {
            let entry = self.filtered_items[index].clone();
            let is_selected = index as isize == self.selected_index;
            match entry {
                FlatEntry::Group(group) => {
                    // project scope 下的 user 组是“从 global 继承来的组”：标题追加
                    // ` · inherited global` 并改用 dim 色；其它组用 accent 色。
                    let inherited = self.write_scope == ConfigWriteScope::Project
                        && group.scope == SourceScope::User;
                    let label = bold(&format!(
                        "{}{}",
                        group.label,
                        if inherited {
                            " · inherited global"
                        } else {
                            ""
                        }
                    ));
                    let group_line = fg(
                        if inherited {
                            ThemeColor::Dim
                        } else {
                            ThemeColor::Accent
                        },
                        &label,
                    );
                    lines.push(truncate_to_width(
                        &format!("  {group_line}"),
                        width,
                        "",
                        false,
                    ));
                }
                FlatEntry::Subgroup(subgroup, group) => {
                    // 子组行固定缩进 4 空格；project scope 下继承组（user scope）用 dim，
                    // 其余用 muted。
                    let color = if self.write_scope == ConfigWriteScope::Project
                        && group.scope == SourceScope::User
                    {
                        ThemeColor::Dim
                    } else {
                        ThemeColor::Muted
                    };
                    let subgroup_line = fg(color, &subgroup.label);
                    lines.push(truncate_to_width(
                        &format!("    {subgroup_line}"),
                        width,
                        "",
                        false,
                    ));
                }
                FlatEntry::Item(item) => {
                    // 条目行：光标只在选中条目上；选中且未变暗时名称加粗，继承且未覆盖的
                    // 条目整行（含勾选框与后缀）都用 dim 色。截断用 "..."（三个 ASCII 点）。
                    let item_ref = item.borrow().clone();
                    let cursor = if is_selected { "> " } else { "  " };
                    let dimmed = self.is_dimmed_item(&item_ref);
                    let name_text = if is_selected && !dimmed {
                        bold(&item_ref.display_name)
                    } else {
                        item_ref.display_name.clone()
                    };
                    let name = if dimmed {
                        fg(ThemeColor::Dim, &name_text)
                    } else {
                        name_text
                    };
                    let checkbox = self.render_checkbox(&item_ref);
                    let suffix = self.get_item_suffix(&item_ref);
                    lines.push(truncate_to_width(
                        &format!("{cursor}    {checkbox} {name}{suffix}"),
                        width,
                        "...",
                        false,
                    ));
                }
            }
        }

        // 滚动计数：只在视口被裁剪时出现；分子是选中行之前（含选中行）的条目数，
        // 分母是过滤结果里的条目总数（组/子组行不计入）。
        if start_index > 0 || end_index < self.filtered_items.len() {
            let item_count = self
                .filtered_items
                .iter()
                .filter(|entry| matches!(entry, FlatEntry::Item(_)))
                .count();
            let current_item_index = self.filtered_items[..self.selected_index.max(0) as usize]
                .iter()
                .filter(|entry| matches!(entry, FlatEntry::Item(_)))
                .count()
                + 1;
            lines.push(fg(
                ThemeColor::Dim,
                &format!("  ({current_item_index}/{item_count})"),
            ));
        }

        lines
    }

    fn handle_input(&mut self, data: &str) {
        // 分派顺序固定为：方向键 → 翻页 → 取消 → 退出 → Tab → 空格/确认 →
        // 其余输入交给搜索框。每个分支都以 return 结束，一次按键只触发一个动作；
        // 单按空格总被当作切换键，因此无法用空格键在搜索词里输入空格。
        let kb = get_keybindings();

        if kb.matches(data, "tui.select.up") {
            // 上下键只在“条目行”之间移动，组/子组行不可选；到边界时停在原处（不环绕）。
            self.selected_index = self.find_next_item(self.selected_index, -1);
            return;
        }
        if kb.matches(data, "tui.select.down") {
            self.selected_index = self.find_next_item(self.selected_index, 1);
            return;
        }
        if kb.matches(data, "tui.select.pageUp") {
            // 源码方向与直觉相反：向上翻页从目标位置**向后**找 item。
            let mut target = isize::max(0, self.selected_index - self.max_visible as isize);
            while target < self.filtered_items.len() as isize
                && !matches!(self.filtered_items[target as usize], FlatEntry::Item(_))
            {
                target += 1;
            }
            if target < self.filtered_items.len() as isize {
                self.selected_index = target;
            }
            return;
        }
        if kb.matches(data, "tui.select.pageDown") {
            // 源码方向与直觉相反：向下翻页从目标位置**向前**找 item。
            let mut target = isize::min(
                self.filtered_items.len() as isize - 1,
                self.selected_index + self.max_visible as isize,
            );
            while target >= 0 && !matches!(self.filtered_items[target as usize], FlatEntry::Item(_))
            {
                target -= 1;
            }
            if target >= 0 {
                self.selected_index = target;
            }
            return;
        }
        if kb.matches(data, "tui.select.cancel") {
            // 回调缺失时按键仍被吞掉（分支照样 return），不会落到搜索框。
            if let Some(on_cancel) = &self.on_cancel {
                on_cancel();
            }
            return;
        }
        if matches_key(data, "ctrl+c") {
            // 硬编码 ctrl+c（不走 keybinding 表），TS 同。
            if let Some(on_exit) = &self.on_exit {
                on_exit();
            }
            return;
        }
        if kb.matches(data, "tui.input.tab") {
            if let Some(on_switch_mode) = self.on_switch_mode.clone() {
                // TS：`onSwitchMode` 内部先 `switchWriteScope()` 再 `requestRender()`。
                // Rust 侧 scope/表头的切换在 ResourceList 内同步完成，回调只触发重绘。
                self.toggle_write_scope();
                on_switch_mode();
            }
            // 无论是否挂载回调都 return：project 模式不可用时 Tab 被忽略，也不会进入搜索框。
            return;
        }
        if data == " " || kb.matches(data, "tui.select.confirm") {
            // 只有选中条目行才响应；global scope 下跳过 project-scope 条目（TS 同），
            // project scope 下所有条目都可切换。
            let entry = self
                .filtered_items
                .get(self.selected_index.max(0) as usize)
                .cloned();
            if let Some(FlatEntry::Item(item)) = entry {
                let snapshot = item.borrow().clone();
                if (self.write_scope == ConfigWriteScope::Project
                    || !self.get_item_scope_is_project(&snapshot))
                    && let Some(new_enabled) = self.toggle_resource(&item)
                {
                    // `toggle_resource` 返回 None 表示设置层没有可应用的修改：此时既不改
                    // 条目也不触发回调，界面保持原状。
                    self.update_item(&item, new_enabled);
                    if let Some(on_toggle) = &self.on_toggle {
                        // TS 的 updateItem 改的是共享对象，回调看到的是**更新后**的 item。
                        let updated = item.borrow().clone();
                        on_toggle(&updated, new_enabled);
                    }
                }
            }
            return;
        }

        // 其余输入（字符、退格、光标移动等）交给搜索框；每次输入后重新过滤，选中位置
        // 回到第一个命中条目。
        self.search_input.handle_input(data);
        let query = self.search_input.get_value().to_string();
        self.filter_items(&query);
    }

    fn invalidate(&mut self) {
        // 列表/组行没有缓存，每次 render 现拼；只有搜索框可能持有内部状态。
        self.search_input.invalidate();
    }

    fn as_focusable(&mut self) -> Option<&mut dyn Focusable> {
        // 宿主通过它把焦点与键盘输入交给列表（TS 里 ResourceList 自身实现 Focusable）。
        Some(self)
    }
}

impl Focusable for ResourceList {
    fn focused(&self) -> bool {
        self.focused
    }

    fn set_focused(&mut self, focused: bool) {
        self.focused = focused;
        // 搜索框需要知道焦点状态来绘制/隐藏光标。
        self.search_input.set_focused(focused);
    }
}

// =============================================================================
// ConfigSelectorComponent
// =============================================================================

/// [`ConfigSelectorComponent::new`] 的可选参数（对应 TS 的后三个有默认值的参数）。
///
/// 默认值见 `ConfigSelectorOptions::default`；`terminal_height: None` 与 `Some(24)`
/// 在 `ResourceList` 里得到相同的 `max_visible`。
#[derive(Debug, Clone, Copy)]
pub struct ConfigSelectorOptions {
    /// `terminalHeight`（缺省 24）。
    pub terminal_height: Option<usize>,
    /// `writeScope`（缺省 global）。
    pub write_scope: ConfigWriteScope,
    /// `projectModeAvailable`（缺省 true）。
    pub project_mode_available: bool,
}

impl Default for ConfigSelectorOptions {
    /// 与 TS 构造参数的默认值一致：`terminalHeight` 未指定（按 24）、
    /// `writeScope = "global"`、`projectModeAvailable = true`。
    fn default() -> Self {
        ConfigSelectorOptions {
            terminal_height: None,
            write_scope: ConfigWriteScope::Global,
            project_mode_available: true,
        }
    }
}

/// 组件内部状态，整体放在 `Rc<RefCell<...>>` 里，[`ConfigSelectorComponent::get_resource_list`]
/// 才能以守卫形式借出列表。
///
/// `header` 用**独立**的 `RefCell`：列表在自身 `handle_input` 里切换 scope 时要同时改
/// 表头，而此时外层 `state` 已经被列表的可变借用占用，不能再从 `state` 里借表头。
struct ConfigSelectorState {
    /// 与 `resource_list` 共享：TS 的 `switchWriteScope()` 同时更新 header 与列表，
    /// 而列表要在自己的 `handle_input` 里同步触发切换，所以 header 必须能独立借用。
    header: Rc<RefCell<ConfigSelectorHeader>>,
    resource_list: ResourceList,
}

/// [`ConfigSelectorComponent::get_resource_list`] 返回的守卫。
///
/// 内部是 `RefMut<ConfigSelectorState>`，通过 `Deref` / `DerefMut` 当 `ResourceList`
/// 使用。守卫存活期间：
///
/// * 组件状态处于可变借用中——对**同一组件**再取第二个守卫会在运行期 panic（编译器允许
///   两次 `&self` 取守卫，借用规则由 `RefCell` 在运行期检查）；
/// * 守卫借用的是 `&self`，因此编译器不允许同时对组件调用需要 `&mut self` 的
///   [`Component::render`] / [`Component::handle_input`]；测试里用作用域块先 drop 守卫。
pub struct ResourceListGuard<'a> {
    state: RefMut<'a, ConfigSelectorState>,
}

impl Deref for ResourceListGuard<'_> {
    type Target = ResourceList;

    fn deref(&self) -> &ResourceList {
        &self.state.resource_list
    }
}

impl DerefMut for ResourceListGuard<'_> {
    fn deref_mut(&mut self) -> &mut ResourceList {
        &mut self.state.resource_list
    }
}

/// `/config` 资源管理组件（TS `ConfigSelectorComponent extends Container implements Focusable`）。
///
/// 固定布局（对应 TS 的 `addChild` 顺序）：Spacer、DynamicBorder、Spacer、表头（两行）、
/// Spacer、[`ResourceList`]、Spacer、DynamicBorder。组件本身不解释按键，全部转发给列表；
/// 焦点状态保存在自己的 `focused` 字段，`set_focused` 时再转发给列表。
pub struct ConfigSelectorComponent {
    state: Rc<RefCell<ConfigSelectorState>>,
    focused: bool,
}

impl ConfigSelectorComponent {
    /// TS 10 参数构造（后三个有默认值，Rust 收进 [`ConfigSelectorOptions`]）。
    ///
    /// * `resolved_paths`：两个写入 scope 的解析结果；构造时按值克隆进组树，组件不持有
    ///   调用方的引用。
    /// * `settings_manager`：共享状态句柄，列表后续写入与调用方读取看到同一份设置。
    /// * `cwd` / `agent_dir`：project / global 顶层资源的基准目录。
    /// * `on_close` / `on_exit`：分别接到列表的 `on_cancel`（Esc）与 `on_exit`（Ctrl+C）。
    /// * `request_render`：接到 `on_toggle` 与（project 模式可用时的）`on_switch_mode`，
    ///   每次切换条目/scope 后由列表触发一次。
    ///
    /// 执行顺序：构建两 scope 的组树 → 构造列表并接线回调 → 建共享表头并挂
    /// `on_switch_mode` → 把状态放进 `Rc<RefCell<_>>`。
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        resolved_paths: &ScopedResolvedPaths,
        settings_manager: SettingsManager,
        cwd: impl Into<String>,
        agent_dir: impl Into<String>,
        on_close: Rc<dyn Fn()>,
        on_exit: Rc<dyn Fn()>,
        request_render: Rc<dyn Fn()>,
        options: ConfigSelectorOptions,
    ) -> Self {
        let cwd = cwd.into();
        let agent_dir = agent_dir.into();
        // 两个 scope 各建一份组树；缺失的 scope 按空 ResolvedPaths 处理（四个资源数组为空）。
        let global_groups = build_groups(
            &resolved_paths
                .get(&ConfigWriteScope::Global)
                .cloned()
                .unwrap_or_default(),
            &agent_dir,
        );
        let project_groups = build_groups(
            &resolved_paths
                .get(&ConfigWriteScope::Project)
                .cloned()
                .unwrap_or_default(),
            &agent_dir,
        );
        let mut groups_by_scope: IndexMap<ConfigWriteScope, Vec<Rc<ResourceGroup>>> =
            IndexMap::new();
        groups_by_scope.insert(ConfigWriteScope::Global, global_groups);
        groups_by_scope.insert(ConfigWriteScope::Project, project_groups);

        let mut resource_list = ResourceList::new(
            groups_by_scope,
            settings_manager,
            cwd,
            agent_dir,
            options.terminal_height,
            options.write_scope,
        );
        resource_list.on_cancel = Some(on_close);
        resource_list.on_exit = Some(on_exit);
        // on_toggle 只请求重绘：条目本身已由 ResourceList 在 handle_input 里 update_item
        // 更新过；TS 的回调同样忽略参数，只调用 requestRender。
        let toggle_render = Rc::clone(&request_render);
        resource_list.on_toggle = Some(Rc::new(move |_item, _enabled| toggle_render()));

        let header = Rc::new(RefCell::new(ConfigSelectorHeader::new(
            options.write_scope,
            options.project_mode_available,
        )));
        resource_list.header = Some(Rc::clone(&header));
        // TS 仅在 `projectModeAvailable` 时挂载 `onSwitchMode`；回调本身只负责
        // `requestRender()`（scope/表头的切换由 ResourceList 在 tab 分支同步完成，
        // 见 [`ResourceList::toggle_write_scope`]）。
        if options.project_mode_available {
            let render = Rc::clone(&request_render);
            resource_list.on_switch_mode = Some(Rc::new(move || render()));
        }
        let state = Rc::new(RefCell::new(ConfigSelectorState {
            // header 与列表一起放进 state；header 仍是独立 RefCell，列表在 Tab 分支里
            // 可以直接借用它。
            header,
            resource_list,
        }));

        ConfigSelectorComponent {
            state,
            focused: false,
        }
    }

    /// TS `getResourceList()`。
    ///
    /// 返回的守卫可变借用整个组件状态（见 [`ResourceListGuard`]）；测试与宿主用它直接
    /// 驱动资源列表（`handle_input`/`render`），不必经过组件转发。
    pub fn get_resource_list(&self) -> ResourceListGuard<'_> {
        ResourceListGuard {
            state: self.state.borrow_mut(),
        }
    }

    /// 当前写入 scope。
    ///
    /// 读的是列表自身状态；表头保存的是另一份副本（由 `toggle_write_scope` 负责同步）。
    pub fn write_scope(&self) -> ConfigWriteScope {
        self.state.borrow().resource_list.write_scope()
    }
}

impl Component for ConfigSelectorComponent {
    fn render(&mut self, width: usize) -> Vec<String> {
        // 顺序镜像 TS 构造函数里的 addChild：Spacer、DynamicBorder、Spacer、表头、
        // Spacer、资源列表、Spacer、DynamicBorder。表头的 `borrow_mut` 是语句级临时
        // 借用，渲染列表前已经释放。
        let mut state = self.state.borrow_mut();
        let mut lines = Vec::new();
        lines.extend(Spacer::new().render(width));
        lines.extend(DynamicBorder::default().render(width));
        lines.extend(Spacer::new().render(width));
        lines.extend(state.header.borrow_mut().render(width));
        lines.extend(Spacer::new().render(width));
        lines.extend(state.resource_list.render(width));
        lines.extend(Spacer::new().render(width));
        lines.extend(DynamicBorder::default().render(width));
        lines
    }

    fn handle_input(&mut self, data: &str) {
        // TS 的 Container.handleInput 只是把输入转给 ResourceList；scope 切换与
        // requestRender 都在 ResourceList.handleInput 的 tab 分支内同步发生。
        // 组件不解释任何按键（包括搜索字符），一律转发。
        self.state.borrow_mut().resource_list.handle_input(data);
    }

    fn invalidate(&mut self) {
        // 表头没有缓存；只需让列表丢弃搜索框状态。
        self.state.borrow_mut().resource_list.invalidate();
    }

    fn as_focusable(&mut self) -> Option<&mut dyn Focusable> {
        // 组件自身实现 Focusable；宿主也可按 TS 的方式把焦点直接设到资源列表。
        Some(self)
    }
}

impl Focusable for ConfigSelectorComponent {
    fn focused(&self) -> bool {
        self.focused
    }

    fn set_focused(&mut self, focused: bool) {
        // 组件自己记一份，同时转发给列表（列表再同步给搜索框，影响光标绘制）。
        // 若通过 `get_resource_list()` 直接设置列表焦点，组件这份 `focused` 不会跟着变。
        self.focused = focused;
        self.state.borrow_mut().resource_list.set_focused(focused);
    }
}
