//! components/session-selector.ts。
//!
//! `/resume` 会话选择器：作用域（当前目录 / 全部）切换、threaded/recent/relevance 排序、
//! 名称过滤、搜索、路径显示、重命名与删除（先 `trash` 后 `unlink`）。
//!
//! TS 源：`packages/coding-agent/src/modes/interactive/components/session-selector.ts`。
//!
//! ## 与 TS 的表示差异
//!
//! * TS 的 `SessionSelectorComponent`/`SessionList` 通过 `AbortController` + `Promise`
//!   驱动加载与竞态保护。Rust 侧把加载状态放在共享的 `SelectorCore`（`Rc<RefCell<_>>`）
//!   里：loader 在调用线程上同步启动（`onProgress` 的同步部分照常生效），返回的 future
//!   交给当前 tokio runtime 驱动；结果写入带 generation 的槽位，组件下次交互/渲染时
//!   `sync` 落地。旧加载的槽位不再被读取（`cancel_loads` 已把对应 `LoadHandle` 从
//!   core 取走），因此只有当前登记的那次加载会被采纳；槽位的 generation 是创建
//!   时写入的一次性标记。
//! * `requestRender` 是 `Arc<dyn Fn() + Send + Sync>`（后台任务完成时可以请求重绘）。
//! * `deleteSessionFile` 在 TS 里是 async；Rust 用同步 `std::fs::remove_file`（`trash` 是
//!   `spawnSync`，本来就阻塞）。副作用顺序一致。
//! * `renameSession` 回调是同步的（宿主在回调里完成持久化），且没有返回值；只要回调被
//!   调用，Rust 侧就无法区分宿主的持久化成败，因此总是刷新列表并在加载结束时退出
//!   rename 模式（TS 的 async 失败传播行为不保留）。
//! * `setStatusMessage` 的自动隐藏用 `Instant` 截止时间在下次渲染时清除（TS 用
//!   `setTimeout`）。
//! * `SessionSelectorHeader`/`SessionList` 在 TS 里是模块私有；Rust 侧公开 `SessionList`
//!   （测试与宿主需要通过 `getSessionList()` 设置回调），`SessionSelectorHeader` 保持
//!   模块私有。
//!
//! ## 职责边界
//!
//! 本文件只负责会话选择界面的呈现与交互：会话文件列举交给注入的 [`SessionsLoader`]，
//! 重命名持久化交给 [`RenameSessionFn`]。删除是唯一的例外——`delete_session_file`
//! 在本模块内完成 `trash`/`unlink` 的副作用。
//!
//! ## 设计原因：为什么把状态放进 `SelectorCore`
//!
//! TS 的 `SessionSelectorComponent` 同时持有列表、头部与加载状态。Rust 的 TUI 组件以
//! `Rc<RefCell<_>>` 传递，如果列表既持有数据又回调组件，很容易在 `handle_input` 期间
//! 重复借用。这里把“作用域 / 缓存 / 进行中的加载 / 头部状态”集中到 `SelectorCore`，
//! `SessionList` 只持 `Rc` 并通过可选回调把事件交回组件；列表需要读 core 的地方只做
//! 短借用，用户回调总在借用释放后执行（见 `SessionSelectorComponent::new` 的包装）。
//!
//! ## 一次加载的执行流程
//!
//! 1. 构造：`SessionSelectorComponent::new` 建头部与空列表、安装回调，最后调用
//!    `begin_load(Current)`。
//! 2. 启动：`begin_load` 分配 generation 与 `LoadSlot`，把头部切到 loading 并请求重绘，
//!    然后在**调用线程**上调用 loader 取得 future；loader 可能在此之前同步触发
//!    `onProgress`。
//! 3. 后台：future 由 `spawn_task` 驱动；进度写入槽位并递增 `revision`，完成时写入
//!    `completed`（错误转成字符串）并请求重绘。
//! 4. 落地：下一次 `render` / `handle_input` / `get_session_list` 调用 `sync`；
//!    `sync_slot` 用 `applied_revision` 判断槽位是否有新内容，把结果写入对应作用域的
//!    缓存与 `pending_list`，列表再 `set_sessions`。后台作用域的结果只更新缓存，不改动
//!    头部或当前列表。
//! 5. 变更：删除成功后，或重命名回调返回后，`refresh_sessions_after_mutation` 取消所有
//!    加载、清空缓存并重新加载当前作用域。
//!
//! ## 注意事项
//!
//! * `request_render` 在 core 的 `RefCell` 可变借用期间被同步调用（`begin_load`、
//!   `toggle_*` 与各回调）；宿主实现不得同步重入本组件（例如直接调用 `render`），
//!   否则会触发 `already borrowed` panic。仓库内两个宿主都只是向通道发送重绘请求。
//! * 列表输入混用两套键位来源：多数键走全局 `get_keybindings()`，
//!   `app.session.toggleNamedFilter` 走构造时注入的 `KeybindingsManager`
//!   （见 `SessionList::handle_input`）。
//! * `delete_session_file` 与 `confirm_rename` 的宿主回调都在按键处理路径上同步执行，
//!   会阻塞调用线程。
//! * `SessionList` 的 `filtered_sessions` 只保存 `all_sessions` 的下标；任何重新过滤都
//!   必须保持下标有效，否则渲染时会越界。
//! * 本文件的公开类型都能从 `kiana_interactive::components::session_selector` 访问；
//!   只有 `SessionSelectorComponent` 由 `components::index` 再导出。

use std::cell::RefCell;
use std::collections::HashMap;
use std::future::Future;
use std::path::Path;
use std::pin::Pin;
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use kiana_ai::utils::abort::AbortScope;
use kiana_config::keybindings::KeybindingsManager;
use kiana_core::child_process::{SpawnSyncOptions, spawn_process_sync_bytes};
use kiana_core::paths::canonicalize_path;
use kiana_json::js_num::format_js_number;
use kiana_session_core::{SessionInfo, SessionListError, SessionListProgress};
use kiana_theme::{ThemeBg, ThemeColor};
use kiana_tui::components::input::Input;
use kiana_tui::components::text_component::Text;
use kiana_tui::keybindings::get_keybindings;
use kiana_tui::text::{is_js_whitespace, truncate_to_width, visible_width};
use kiana_tui::tui::{Component, Focusable};
use kiana_ui_kit::dynamic_border::DynamicBorder;
use kiana_ui_kit::keybinding_hints::{key_hint, key_text};

use super::session_selector_search::{
    NameFilter, SortMode, filter_and_sort_session_refs, has_session_name,
};

/// 会话作用域（TS `SessionScope`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionScope {
    /// 当前工作目录下的会话。
    Current,
    /// 所有会话。
    All,
}

impl SessionScope {
    /// `scope === "all"`。
    pub fn is_all(self) -> bool {
        matches!(self, SessionScope::All)
    }
}

/// 请求重绘回调（TS `requestRender: () => void`）。
pub type RequestRenderFn = Arc<dyn Fn() + Send + Sync>;

/// loader 返回的 future。
///
/// 必须是 `Send + 'static`：它会被 `spawn_task` 交给 tokio runtime 或临时线程驱动。
pub type SessionLoaderFuture =
    Pin<Box<dyn Future<Output = Result<Vec<SessionInfo>, SessionListError>> + Send + 'static>>;

/// `SessionsLoader`：`(onProgress?, signal?) => Promise<SessionInfo[]>`。
///
/// loader 自己负责把 `signal` 传给底层可取消的工作（本仓库的 `SessionManager::list*`
/// 会检查它）以及把 `onProgress` 挂到发布点；返回的 future 在后台执行，不在这里等待。
pub type SessionsLoader = Arc<
    dyn Fn(Option<SessionListProgress>, Option<AbortScope>) -> SessionLoaderFuture + Send + Sync,
>;

/// 重命名回调（TS `renameSession(path, currentName) => Promise<void>`）。
///
/// 同步执行；宿主应在回调内完成持久化。没有返回值，组件无法得知成败，因此只要回调被
/// 调用就会刷新列表。
pub type RenameSessionFn = Box<dyn FnMut(&str, &str)>;

/// 无参数回调（TS `() => void`）。
pub type VoidCallback = Box<dyn FnMut()>;
/// 单字符串参数回调（TS `(value: string) => void`）。
pub type StringCallback = Box<dyn FnMut(&str)>;
/// 可选字符串参数回调（TS `(path: string | null) => void`）。
pub type OptionalStringCallback = Box<dyn FnMut(Option<&str>)>;
/// 布尔参数回调（TS `(show: boolean) => void`）。
pub type BoolCallback = Box<dyn FnMut(bool)>;

/// [`SessionSelectorComponent`] 的可选参数（TS `options`）。
#[derive(Default)]
pub struct SessionSelectorOptions {
    /// 重命名持久化回调；`None` 时禁用重命名。
    pub rename_session: Option<RenameSessionFn>,
    /// 是否显示 `ctrl+r rename` 提示；`None` 时取 `rename_session.is_some()`。
    pub show_rename_hint: Option<bool>,
    /// 注入的键位管理器（`None` 时读取磁盘配置）。
    pub keybindings: Option<KeybindingsManager>,
}

/// 删除方式（TS `method: "trash" | "unlink"`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeleteMethod {
    /// `trash` 命令成功（或文件已不存在）。
    Trash,
    /// 回退到永久删除；即使回退失败，结果里的 `method` 仍是 `Unlink`。
    Unlink,
}

/// `deleteSessionFile` 的结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeleteSessionResult {
    /// 是否成功。
    pub ok: bool,
    /// 使用的方式。
    pub method: DeleteMethod,
    /// 失败信息。
    pub error: Option<String>,
}

// ---------------------------------------------------------------------------
// 主题/格式化辅助
// ---------------------------------------------------------------------------

// 渲染期取色：主题未初始化时 panic（TS 的 `theme.fg/bg/bold` 在同样条件下抛错）。
// 这些函数只应在 render 路径调用，不要放到后台任务里。
fn fg(color: ThemeColor, text: &str) -> String {
    kiana_theme::THEME
        .fg(color, text)
        .expect("theme must be initialized before rendering (TS throws here)")
}

fn bg(color: ThemeBg, text: &str) -> String {
    kiana_theme::THEME
        .bg(color, text)
        .expect("theme must be initialized before rendering (TS throws here)")
}

fn bold(text: &str) -> String {
    kiana_theme::THEME
        .bold(text)
        .expect("theme must be initialized before rendering (TS throws here)")
}

const BORDER_COLOR: ThemeColor = ThemeColor::Accent;

/// 选择器外框颜色与渲染（对应 TS `new DynamicBorder(s => theme.fg("accent", s))`）。
fn accent_border_lines(width: usize) -> Vec<String> {
    DynamicBorder::from_fn(|text: &str| fg(BORDER_COLOR, text)).render(width)
}

/// 渲染一个 `Text(text, paddingX, 0)` 组件。
fn text_lines(text: &str, padding_x: usize, width: usize) -> Vec<String> {
    let mut component = Text::with_padding(text, padding_x, 0);
    component.render(width)
}

/// JS `String.prototype.trim()`（JS 空白集合）。
fn js_trim(text: &str) -> &str {
    text.trim_matches(is_js_whitespace)
}

/// 当前 Unix 毫秒（f64；与 JS `Date.now()` 一致）。
fn now_ms() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs_f64() * 1000.0)
        .unwrap_or(f64::NAN)
}

/// `shortenPath(path)`：`os.homedir()` 前缀替换为 `~`。
fn shorten_path(path: &str) -> String {
    if path.is_empty() {
        return path.to_string();
    }
    let home = dirs::home_dir()
        .map(|home| home.to_string_lossy().into_owned())
        .unwrap_or_default();
    if !home.is_empty() && path.starts_with(&home) {
        return format!("~{}", &path[home.len()..]);
    }
    path.to_string()
}

/// JS `String(number)`（ECMAScript `Number::toString`）：与 [`format_js_number`]（`JSON.stringify`
/// 语义）只在非有限值上不同——模板字面量写 `"NaN"` / `"Infinity"` / `"-Infinity"`。
fn js_number_to_string(value: f64) -> String {
    if value.is_nan() {
        "NaN".to_string()
    } else if value == f64::INFINITY {
        "Infinity".to_string()
    } else if value == f64::NEG_INFINITY {
        "-Infinity".to_string()
    } else {
        format_js_number(value)
    }
}

/// `formatSessionDate(date)`：`now` / `Nm` / `Nh` / `Nd` / `Nw` / `Nmo` / `Ny`。
///
/// 阶段：先按毫秒差算出向下取整的分钟/小时/天数，再按固定阈值选择单位。`now` 由调用方
/// 在每帧只取一次（`SessionList::render`），保证同一屏所有会话共用同一基准。
fn format_session_date(modified_ms: f64, now: f64) -> String {
    // `session.modified` 是 Date：非有限毫秒（NaN/±Infinity）构造出 Invalid Date，
    // `getTime()` 返回 NaN。直接按 f64 计算会得到 "-inf"/"now"，与 TS 不同。
    let modified_ms = if modified_ms.is_finite() {
        modified_ms
    } else {
        f64::NAN
    };
    let diff_ms = now - modified_ms;
    let diff_mins = (diff_ms / 60_000.0).floor();
    let diff_hours = (diff_ms / 3_600_000.0).floor();
    let diff_days = (diff_ms / 86_400_000.0).floor();

    if diff_mins < 1.0 {
        return "now".to_string();
    }
    if diff_mins < 60.0 {
        return format!("{}m", js_number_to_string(diff_mins));
    }
    if diff_hours < 24.0 {
        return format!("{}h", js_number_to_string(diff_hours));
    }
    if diff_days < 7.0 {
        return format!("{}d", js_number_to_string(diff_days));
    }
    if diff_days < 30.0 {
        return format!("{}w", js_number_to_string((diff_days / 7.0).floor()));
    }
    if diff_days < 365.0 {
        return format!("{}mo", js_number_to_string((diff_days / 30.0).floor()));
    }
    format!("{}y", js_number_to_string((diff_days / 365.0).floor()))
}

/// `canonicalizePath(path)`：falsy 原样返回，否则走 `utils/paths`（失败回退原串）。
///
/// `Some("")` 保持空串而不是解析成 cwd：空父路径在 `build_session_tree` 里表示“没有父”。
fn canonicalize_selector_path(path: Option<&str>) -> Option<String> {
    match path {
        None => None,
        Some("") => Some(String::new()),
        Some(path) => Some(canonicalize_path(path)),
    }
}

/// 在 `threaded` 模式下按 `parentSessionPath` 构建展示树。
///
/// 节点用 `all_sessions` 的下标表示；`selected` 是经过 name filter 后的下标集合
/// （只有这些会话参与建树）。
///
/// `children[parent]` 是子节点下标（已按子树最新活动降序），`roots` 是根节点下标（同样
/// 排序）。两张表都只含 `selected` 里的下标。
struct SessionTreeData {
    children: HashMap<usize, Vec<usize>>,
    roots: Vec<usize>,
}

/// `buildSessionTree(sessions)`。
///
/// 分三步：
/// 1. `by_path` 收录过滤后的会话，同一规范化路径**后写覆盖前写**（TS `Map.set`），因此
///    重复路径只有最后一条会成为树节点（测试
///    `duplicate_canonical_paths_render_last_session_twice`）。
/// 2. 连边：父路径同样规范化；只有父节点也在过滤后的集合里才连边，否则该会话是根。
/// 3. 聚合 `latestActivity`：从根做前序 DFS，再逆序遍历把子树最大值回填给父节点。
///
/// 防环：重复路径/自引用可能形成环。前序收集用 `visited` 截断，未被根遍历到的节点不会
/// 参与排序，也不会出现在渲染结果里（TS 的递归实现在这种病态输入上会栈溢出）。
/// `latestActivity` 用 `js_math_max` 聚合，NaN 会传染。排序是稳定排序，NaN 视为相等。
fn build_session_tree(all_sessions: &[SessionInfo], selected: &[usize]) -> SessionTreeData {
    let mut by_path: HashMap<String, usize> = HashMap::new();
    for index in selected {
        let session_path = canonicalize_path(&all_sessions[*index].path);
        // 同一 path 后写覆盖前写（TS Map.set 语义）。
        by_path.insert(session_path, *index);
    }

    let mut children: HashMap<usize, Vec<usize>> = HashMap::new();
    let mut latest_activity: HashMap<usize, f64> = HashMap::new();
    let mut roots: Vec<usize> = Vec::new();

    for index in selected {
        children.entry(*index).or_default();
        latest_activity.insert(*index, all_sessions[*index].modified);
    }

    for index in selected {
        let session = &all_sessions[*index];
        // TS 取 `byPath.get(sessionPath)` 的节点（同一规范化路径的**最后一个**会话）。
        let session_path = canonicalize_path(&session.path);
        let node = by_path.get(&session_path).copied().unwrap_or(*index);
        let parent_path = canonicalize_selector_path(session.parent_session_path.as_deref());
        let parent_index = parent_path
            .as_deref()
            .filter(|path| !path.is_empty())
            .and_then(|path| by_path.get(path).copied());
        match parent_index {
            Some(parent) => children.entry(parent).or_default().push(node),
            None => roots.push(node),
        }
    }

    // `updateLatestActivity`：子树最大 modified。
    // 先做前序收集，再逆序聚合（children 前序包含父在子前）。`visited` 防御重复路径造成
    // 的环（TS 在这种病态输入上会栈溢出；这里保证不会挂死）。
    let mut order: Vec<usize> = Vec::with_capacity(selected.len());
    let mut visited: std::collections::HashSet<usize> = std::collections::HashSet::new();
    let mut stack: Vec<usize> = roots.iter().rev().copied().collect();
    while let Some(index) = stack.pop() {
        if !visited.insert(index) {
            continue;
        }
        order.push(index);
        if let Some(child_nodes) = children.get(&index) {
            for child in child_nodes.iter().rev() {
                stack.push(*child);
            }
        }
    }
    for index in order.into_iter().rev() {
        let mut latest = all_sessions[index].modified;
        if let Some(child_nodes) = children.get(&index) {
            for child in child_nodes {
                latest = js_math_max(latest, latest_activity[child]);
            }
        }
        latest_activity.insert(index, latest);
    }

    // 根与每一层子节点都按 latestActivity 降序（稳定排序）。
    roots.sort_by(|a, b| compare_number_desc(latest_activity[a], latest_activity[b]));
    let all_indices: Vec<usize> = children.keys().copied().collect();
    for index in all_indices {
        if let Some(child_nodes) = children.get_mut(&index) {
            child_nodes.sort_by(|a, b| compare_number_desc(latest_activity[a], latest_activity[b]));
        }
    }

    SessionTreeData { children, roots }
}

/// `Math.max(a, b)`（NaN 传染）。
fn js_math_max(a: f64, b: f64) -> f64 {
    if a.is_nan() || b.is_nan() {
        f64::NAN
    } else if a >= b {
        a
    } else {
        b
    }
}

/// 降序比较（`b - a`；NaN 视为相等，稳定排序）。
fn compare_number_desc(a: f64, b: f64) -> std::cmp::Ordering {
    let delta = b - a;
    if delta.is_nan() || delta == 0.0 {
        std::cmp::Ordering::Equal
    } else if delta < 0.0 {
        std::cmp::Ordering::Less
    } else {
        std::cmp::Ordering::Greater
    }
}

/// 扁平化后的会话树节点（TS `FlatSessionNode`）。
///
/// `session_index` 指向 `SessionList.all_sessions`，而不是持有 `SessionInfo` 副本：列表
/// 重新加载后节点重建，避免引用过期数据。非树形模式（Recent/Relevance，或带搜索的
/// Threaded）生成 depth = 0、`is_last = true`、空祖先链的节点。
#[derive(Debug, Clone, PartialEq)]
pub struct FlatSessionNode {
    /// `allSessions` 下标。
    pub session_index: usize,
    /// 显示层级。
    pub depth: usize,
    /// 是否为本层最后一个。
    pub is_last: bool,
    /// 每层祖先是否还有后续兄弟。
    pub ancestor_continues: Vec<bool>,
}

/// `flattenSessionTree(roots)`。
///
/// 前序遍历树，输出渲染行所需的层级信息：`ancestor_continues` 记录每一层祖先是否还有
/// 后续兄弟（`build_tree_prefix` 据此画 `│` 或空格），`path` 是当前根到节点的下标链，
/// 用于跳过重复路径造成的环（TS 的递归遍历没有这层保护）。
fn flatten_session_tree(tree: &SessionTreeData) -> Vec<FlatSessionNode> {
    let mut result: Vec<FlatSessionNode> = Vec::new();
    struct Item {
        index: usize,
        depth: usize,
        continues: Vec<bool>,
        is_last: bool,
        /// 从根到当前节点的下标路径（防御重复路径造成的环）。
        path: Vec<usize>,
    }
    let mut stack: Vec<Item> = Vec::new();
    for (position, root) in tree.roots.iter().enumerate().rev() {
        stack.push(Item {
            index: *root,
            depth: 0,
            continues: Vec::new(),
            is_last: position == tree.roots.len() - 1,
            path: vec![*root],
        });
    }
    while let Some(item) = stack.pop() {
        result.push(FlatSessionNode {
            session_index: item.index,
            depth: item.depth,
            is_last: item.is_last,
            ancestor_continues: item.continues.clone(),
        });
        let empty: Vec<usize> = Vec::new();
        let child_nodes = tree.children.get(&item.index).unwrap_or(&empty);
        // 只有非根祖先才绘制延续线。
        let continues = item.depth > 0 && !item.is_last;
        for (position, child) in child_nodes.iter().enumerate().rev() {
            if item.path.contains(child) {
                continue;
            }
            let mut next = item.continues.clone();
            next.push(continues);
            let mut path = item.path.clone();
            path.push(*child);
            stack.push(Item {
                index: *child,
                depth: item.depth + 1,
                continues: next,
                is_last: position == child_nodes.len() - 1,
                path,
            });
        }
    }
    result
}

/// `buildTreePrefix(node)`。
fn build_tree_prefix(node: &FlatSessionNode) -> String {
    if node.depth == 0 {
        return String::new();
    }
    let mut prefix = String::new();
    for continues in &node.ancestor_continues {
        prefix.push_str(if *continues { "│  " } else { "   " });
    }
    prefix.push_str(if node.is_last { "└─ " } else { "├─ " });
    prefix
}

/// 状态消息类型（TS `{ type: "info" | "error" }`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatusKind {
    /// 信息。
    Info,
    /// 错误。
    Error,
}

/// 状态消息。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatusMessage {
    /// 类型。
    pub kind: StatusKind,
    /// 文本。
    pub message: String,
}

/// 选择器头部（TS `SessionSelectorHeader`，模块私有类）。
///
/// 只保存展示状态；加载进度与作用域由 `SelectorCore` 通过 `set_*` 推送。`render` 是
/// 纯展示路径（唯一副作用是 `sync_status` 清理过期状态消息）。
struct SessionSelectorHeader {
    scope: SessionScope,
    sort_mode: SortMode,
    name_filter: NameFilter,
    request_render: RequestRenderFn,
    loading: bool,
    load_progress: Option<(u64, u64)>,
    show_path: bool,
    confirming_delete_path: Option<String>,
    status_message: Option<StatusMessage>,
    /// 自动隐藏截止时间；`None` 表示永久显示。到期由 `sync_status` 在下一次渲染时清除。
    status_deadline: Option<Instant>,
    show_rename_hint: bool,
}

impl SessionSelectorHeader {
    fn new(
        scope: SessionScope,
        sort_mode: SortMode,
        name_filter: NameFilter,
        request_render: RequestRenderFn,
    ) -> Self {
        SessionSelectorHeader {
            scope,
            sort_mode,
            name_filter,
            request_render,
            loading: false,
            load_progress: None,
            show_path: false,
            confirming_delete_path: None,
            status_message: None,
            status_deadline: None,
            show_rename_hint: false,
        }
    }

    fn set_scope(&mut self, scope: SessionScope) {
        self.scope = scope;
    }

    fn set_sort_mode(&mut self, sort_mode: SortMode) {
        self.sort_mode = sort_mode;
    }

    fn set_name_filter(&mut self, name_filter: NameFilter) {
        self.name_filter = name_filter;
    }

    fn set_loading(&mut self, loading: bool) {
        self.loading = loading;
        // Progress 只属于当前这次加载；每次设置加载状态都清空。
        self.load_progress = None;
    }

    fn set_progress(&mut self, loaded: u64, total: u64) {
        self.load_progress = Some((loaded, total));
    }

    fn set_show_path(&mut self, show_path: bool) {
        self.show_path = show_path;
    }

    fn set_show_rename_hint(&mut self, show: bool) {
        self.show_rename_hint = show;
    }

    fn set_confirming_delete_path(&mut self, path: Option<String>) {
        self.confirming_delete_path = path;
    }

    /// `setStatusMessage(msg, autoHideMs?)`：`msg` 为 `None` 或 `auto_hide_ms` 为
    /// `None`/0 时永久显示。
    ///
    /// 设置新消息会重置截止时间，但**不会**取消旧消息已经派发的定时任务；旧任务到期时只
    /// 请求一次重绘，`sync_status` 按当前截止时间判断，因此不会提前清除新消息。
    fn set_status_message(&mut self, message: Option<StatusMessage>, auto_hide_ms: Option<u64>) {
        self.status_deadline = None;
        self.status_message = message;
        if self.status_message.is_none() {
            return;
        }
        if let Some(auto_hide_ms) = auto_hide_ms
            && auto_hide_ms > 0
        {
            self.status_deadline = Some(Instant::now() + Duration::from_millis(auto_hide_ms));
            // TS `setTimeout`：到期只请求重绘；真正的清理在 `sync_status`（下一次渲染）。
            let request_render = Arc::clone(&self.request_render);
            spawn_task(Box::pin(async move {
                tokio::time::sleep(Duration::from_millis(auto_hide_ms)).await;
                request_render();
            }));
        }
    }

    /// `setTimeout` 的等价物：渲染时检查截止时间。
    ///
    /// 只在调用线程的同步点调用（`SessionSelectorHeader::render` 与 `SelectorCore::sync`，
    /// 后者由渲染/交互路径触发），绝不在后台任务里；后台任务的到期回调本身只负责
    /// `request_render`，消息的实际清除发生在下一个同步点。
    fn sync_status(&mut self) {
        if let Some(deadline) = self.status_deadline
            && Instant::now() >= deadline
        {
            self.status_deadline = None;
            self.status_message = None;
        }
    }

    fn render(&mut self, width: usize) -> Vec<String> {
        self.sync_status();

        let title = if self.scope == SessionScope::Current {
            "Resume Session (Current Folder)"
        } else {
            "Resume Session (All)"
        };
        let left_text = bold(title);

        let sort_label = match self.sort_mode {
            SortMode::Threaded => "Threaded",
            SortMode::Recent => "Recent",
            SortMode::Relevance => "Fuzzy",
        };
        let sort_text = format!(
            "{}{}",
            fg(ThemeColor::Muted, "Sort: "),
            fg(ThemeColor::Accent, sort_label)
        );

        let name_label = if self.name_filter == NameFilter::All {
            "All"
        } else {
            "Named"
        };
        let name_text = format!(
            "{}{}",
            fg(ThemeColor::Muted, "Name: "),
            fg(ThemeColor::Accent, name_label)
        );

        let scope_text = if self.loading {
            let progress_text = match self.load_progress {
                Some((loaded, total)) => format!("{loaded}/{total}"),
                None => "...".to_string(),
            };
            format!(
                "{}{}",
                fg(ThemeColor::Muted, "○ Current Folder | "),
                fg(ThemeColor::Accent, &format!("Loading {progress_text}"))
            )
        } else if self.scope == SessionScope::Current {
            format!(
                "{}{}",
                fg(ThemeColor::Accent, "◉ Current Folder"),
                fg(ThemeColor::Muted, " | ○ All")
            )
        } else {
            format!(
                "{}{}",
                fg(ThemeColor::Muted, "○ Current Folder | "),
                fg(ThemeColor::Accent, "◉ All")
            )
        };

        // 布局：右栏（作用域/名称/排序）先占宽度，左标题截断到剩余列宽，中间用空格补齐。
        // 窄终端下因此优先保留右栏状态（与 TS 的 `truncateToWidth` 顺序一致）。
        let right_text = truncate_to_width(
            &format!("{scope_text}  {name_text}  {sort_text}"),
            width,
            "",
            false,
        );
        let available_left = width
            .saturating_sub(visible_width(&right_text))
            .saturating_sub(1);
        let left = truncate_to_width(&left_text, available_left, "", false);
        let spacing = width.saturating_sub(visible_width(&left) + visible_width(&right_text));

        // 两行提示的优先级固定为：删除确认态 > 临时状态消息 > 常规快捷键。
        // 前两者占第一行、第二行为空；常规提示分两行，超宽时都用 `…` 截断。
        let (hint_line1, hint_line2) = if self.confirming_delete_path.is_some() {
            let confirm_hint = format!(
                "Delete session? {} · {}",
                key_hint("tui.select.confirm", "confirm"),
                key_hint("tui.select.cancel", "cancel")
            );
            (
                fg(
                    ThemeColor::Error,
                    &truncate_to_width(&confirm_hint, width, "…", false),
                ),
                String::new(),
            )
        } else if let Some(status) = &self.status_message {
            let color = if status.kind == StatusKind::Error {
                ThemeColor::Error
            } else {
                ThemeColor::Accent
            };
            (
                fg(
                    color,
                    &truncate_to_width(&status.message, width, "…", false),
                ),
                String::new(),
            )
        } else {
            let path_state = if self.show_path { "(on)" } else { "(off)" };
            let sep = fg(ThemeColor::Muted, " · ");
            let hint1 = format!(
                "{}{}{}",
                key_hint("tui.input.tab", "scope"),
                sep,
                fg(ThemeColor::Muted, "re:<pattern> regex · \"phrase\" exact")
            );
            let mut hint2_parts = vec![
                key_hint("app.session.toggleSort", "sort"),
                key_hint("app.session.toggleNamedFilter", "named"),
                key_hint("app.session.delete", "delete"),
                key_hint("app.session.togglePath", &format!("path {path_state}")),
            ];
            if self.show_rename_hint {
                hint2_parts.push(key_hint("app.session.rename", "rename"));
            }
            let hint2 = hint2_parts.join(&sep);
            (
                truncate_to_width(&hint1, width, "…", false),
                truncate_to_width(&hint2, width, "…", false),
            )
        };

        vec![
            format!("{left}{}{right_text}", " ".repeat(spacing)),
            hint_line1,
            hint_line2,
        ]
    }
}

// ---------------------------------------------------------------------------
// 加载状态（后台任务与 UI 共享）
// ---------------------------------------------------------------------------

/// loader future 在后台完成后写入的槽位（`Arc<Mutex<...>>`，因为 future 要求 `Send`）。
///
/// * `generation`：槽位创建时的加载编号，只写一次，见 `begin_load` 的说明。
/// * `revision`：每写入一次进度或终态就 +1；`sync_slot` 用 `applied_revision` 去重。
/// * `progress` / `partial`：最近一次进度与部分快照；`partial` 只在回调给出快照时更新。
/// * `completed`：终态结果（错误已转成字符串），被 `sync_slot` 取走后不再有值。
struct LoadSlot {
    generation: u64,
    revision: u64,
    progress: Option<(u64, u64)>,
    partial: Option<Vec<SessionInfo>>,
    completed: Option<Result<Vec<SessionInfo>, String>>,
}

/// 一次进行中的加载：取消令牌、共享槽位与已落地的 revision。
///
/// `applied_revision` 只在 `sync_slot` 持有 core 可变借用时更新，后台任务不碰它。
struct LoadHandle {
    cancel: AbortScope,
    slot: Arc<Mutex<LoadSlot>>,
    applied_revision: u64,
}

impl LoadHandle {
    /// 向 loader 发出取消信号；不等待 future 结束，也不清空槽位。
    fn abort(&self) {
        self.cancel.abort();
    }
}

type TaskFuture = Pin<Box<dyn Future<Output = ()> + Send + 'static>>;

/// 在 tokio runtime（或临时单线程 runtime）上驱动一个任务。
///
/// 优先使用调用线程当前的 runtime handle；没有 runtime 时（同步宿主或直接调用公开
/// API），新建线程并在其中建立 current-thread runtime。runtime 构造失败会静默丢弃任务
/// （调用方收不到通知，也不会重试）；线程创建失败则按 `std::thread::spawn` 的契约直接
/// panic（非静默）。
fn spawn_task(task: TaskFuture) {
    if let Ok(handle) = tokio::runtime::Handle::try_current() {
        handle.spawn(task);
        return;
    }
    // 没有运行时（同步宿主）：用一次性线程 + current-thread runtime 跑完。
    let _ = std::thread::spawn(move || {
        let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        else {
            return;
        };
        runtime.block_on(task);
    });
}

/// 选择器的共享核心：作用域、缓存、加载槽位与头部状态。
///
/// `SessionList` 与 `SessionSelectorComponent` 都持有它的 `Rc`；它**不**持有列表本身，
/// 因此列表的输入处理可以安全地回调到这里（没有 `RefCell` 重入）。
///
/// 不变量：
/// * `scope` / `sort_mode` / `name_filter` 是内置交互的权威值；`header` 与 `SessionList`
///   各有一份镜像，由 `toggle_*` 更新 core 后再推送给它们。
/// * `current_sessions` / `all_sessions` 分别是两个 loader 的最后一次成功或部分结果；
///   `None` 表示尚未加载、已取消或加载失败。
/// * `pending_list` 是一次性交接消息：`take_pending_list` 取走后为空。它让 core 不必
///   持有 `SessionList`，从而避免借用环。
struct SelectorCore {
    scope: SessionScope,
    sort_mode: SortMode,
    name_filter: NameFilter,
    header: SessionSelectorHeader,
    current_sessions: Option<Vec<SessionInfo>>,
    all_sessions: Option<Vec<SessionInfo>>,
    current_load: Option<LoadHandle>,
    all_load: Option<LoadHandle>,
    current_loader: SessionsLoader,
    all_loader: SessionsLoader,
    request_render: RequestRenderFn,
    pending_list: Option<(Vec<SessionInfo>, bool)>,
    pending_rename: Option<String>,
    generation: u64,
}

impl SelectorCore {
    fn is_loading(&self, scope: SessionScope) -> bool {
        match scope {
            SessionScope::Current => self.current_load.is_some(),
            SessionScope::All => self.all_load.is_some(),
        }
    }

    fn sessions_for(&self, scope: SessionScope) -> Option<&Vec<SessionInfo>> {
        match scope {
            SessionScope::Current => self.current_sessions.as_ref(),
            SessionScope::All => self.all_sessions.as_ref(),
        }
    }

    fn take_pending_list(&mut self) -> Option<(Vec<SessionInfo>, bool)> {
        self.pending_list.take()
    }

    fn take_pending_rename(&mut self) -> Option<String> {
        self.pending_rename.take()
    }

    /// 应用后台加载写入的结果；与 TS 的 `onProgress` + `isActive()` 等价。
    ///
    /// 先 current 后 all，最后检查头部状态消息是否到期。两个槽位互不影响。
    fn sync(&mut self) {
        self.sync_slot(SessionScope::Current);
        self.sync_slot(SessionScope::All);
        self.header.sync_status();
    }

    /// 把某个作用域槽位的最新变化落到 core。
    ///
    /// 流程：
    /// 1. 没有活动句柄，或 `revision` 与 `applied_revision` 相同 → 直接返回（不请求重绘）。
    /// 2. 有终态结果：先取走 `LoadHandle`（`is_loading` 变 false）。成功时写缓存，失败时
    ///    清缓存。只有该作用域正是当前作用域时，才更新头部、写 `pending_list` 并请求重绘；
    ///    后台预加载完成不能改变用户正在看的界面（否则列表会在浏览途中跳动）。
    /// 3. 只有进度/部分结果：有部分快照就更新对应缓存；进度显示与列表交接只发生在当前
    ///    作用域。
    ///
    /// `completed` 用 `take()` 取走，保证终态只处理一次；进度字段保留到句柄被丢弃。
    fn sync_slot(&mut self, scope: SessionScope) {
        let request_render = Arc::clone(&self.request_render);
        let (progress, partial, completed) = {
            let Some(handle) = (match scope {
                SessionScope::Current => self.current_load.as_mut(),
                SessionScope::All => self.all_load.as_mut(),
            }) else {
                return;
            };
            let mut slot = handle
                .slot
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let revision = slot.revision;
            if revision == handle.applied_revision {
                return;
            }
            handle.applied_revision = revision;
            let progress = slot.progress;
            let partial = slot.partial.clone();
            let completed = slot.completed.take();
            (progress, partial, completed)
        };

        if let Some(completed) = completed {
            match scope {
                SessionScope::Current => self.current_load = None,
                SessionScope::All => self.all_load = None,
            }
            match completed {
                Ok(sessions) => {
                    match scope {
                        SessionScope::Current => self.current_sessions = Some(sessions.clone()),
                        SessionScope::All => self.all_sessions = Some(sessions.clone()),
                    }
                    if scope == self.scope {
                        self.header.set_loading(false);
                        self.pending_list = Some((sessions, scope.is_all()));
                        request_render();
                    }
                }
                Err(message) => {
                    match scope {
                        SessionScope::Current => self.current_sessions = None,
                        SessionScope::All => self.all_sessions = None,
                    }
                    if scope == self.scope {
                        self.header.set_loading(false);
                        self.header.set_status_message(
                            Some(StatusMessage {
                                kind: StatusKind::Error,
                                message: format!("Failed to load sessions: {message}"),
                            }),
                            Some(4000),
                        );
                        self.pending_list = Some((Vec::new(), scope.is_all()));
                        request_render();
                    }
                }
            }
            return;
        }

        if let Some(partial) = partial {
            match scope {
                SessionScope::Current => self.current_sessions = Some(partial.clone()),
                SessionScope::All => self.all_sessions = Some(partial.clone()),
            }
            if scope == self.scope {
                self.pending_list = Some((partial, scope.is_all()));
            }
        }
        if scope == self.scope {
            if let Some((loaded, total)) = progress {
                self.header.set_progress(loaded, total);
            }
            request_render();
        }
    }

    /// 开始加载 `scope`；该作用域已有活动加载时直接返回。
    ///
    /// 顺序（与 TS `loadScope` 对齐）：
    /// 1. 递增 core 的 generation、创建槽位；把头部切到新的 scope/loading 并请求重绘。
    /// 2. 包装 `onProgress`：锁住槽位写入进度与部分快照、递增 revision，然后请求重绘。
    ///    回调可能来自多个 worker 线程（`SessionListProgress` 要求 `Send + Sync`），也可能
    ///    在 loader 返回 future 之前**同步**发生。
    /// 3. 在调用线程上调用 loader 取得 future，登记 `LoadHandle`（此时 `is_loading` 为
    ///    true），再用 `spawn_task` 等待 future。
    /// 4. 完成时写入 `completed` 并请求重绘；末尾立即 `sync()` 一次，让 loader 同步产生的
    ///    进度/完成结果在这一轮就可见。
    ///
    /// 槽位的 generation 在创建后不再变化，因此回调里的 `slot.generation != generation`
    /// 永远不会成立；真正让旧结果不可见的机制是 `cancel_loads` 丢弃 `LoadHandle` 后
    /// `sync_slot` 不再读取旧槽位。这里的比较只是防御性自检。
    fn begin_load(&mut self, scope: SessionScope) {
        if self.is_loading(scope) {
            return;
        }
        self.generation = self.generation.wrapping_add(1);
        let generation = self.generation;
        let slot = Arc::new(Mutex::new(LoadSlot {
            generation,
            revision: 0,
            progress: None,
            partial: None,
            completed: None,
        }));
        let cancel = AbortScope::new();

        // 头部先进入 loading 并请求重绘，然后才调用 loader（重绘由宿主在事件循环中执行）。
        self.header.set_scope(scope);
        self.header.set_loading(true);
        (self.request_render)();

        let progress_callback: SessionListProgress = {
            let slot = Arc::clone(&slot);
            let request_render = Arc::clone(&self.request_render);
            Arc::new(move |loaded, total, partial_sessions| {
                let mut slot = slot.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
                // 终态已写入后忽略迟到的进度；`partial` 只在本次回调给出快照时覆盖。
                if slot.generation != generation || slot.completed.is_some() {
                    return;
                }
                slot.progress = Some((loaded, total));
                if partial_sessions.is_some() {
                    slot.partial = partial_sessions;
                }
                slot.revision += 1;
                drop(slot);
                request_render();
            })
        };

        let loader = match scope {
            SessionScope::Current => Arc::clone(&self.current_loader),
            SessionScope::All => Arc::clone(&self.all_loader),
        };
        let future = loader(Some(progress_callback), Some(cancel.clone()));

        let handle = LoadHandle {
            cancel,
            slot: Arc::clone(&slot),
            applied_revision: 0,
        };
        match scope {
            SessionScope::Current => self.current_load = Some(handle),
            SessionScope::All => self.all_load = Some(handle),
        }

        let request_render = Arc::clone(&self.request_render);
        spawn_task(Box::pin(async move {
            let result = future.await;
            {
                let mut slot = slot.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
                // 与进度回调相同的防御性检查（槽位 generation 创建后不变）。
                if slot.generation != generation {
                    return;
                }
                slot.completed = Some(result.map_err(|error| error.message));
                slot.revision += 1;
            }
            request_render();
        }));

        // loader 可能在返回 future 之前同步调用 onProgress（TS async 函数体在首个 await
        // 前同步执行）；立即落地一次，保持与 TS 相同的可见顺序。
        self.sync();
    }

    /// 取消并丢弃两个作用域的进行中加载，同时清空缓存。
    ///
    /// `abort()` 只发出取消信号；已开始的工作可能继续运行一段时间（`SessionListProgress`
    /// 的文档也说明取消不是即时的），其回调写入的是已无人读取的槽位。这里不等待 future
    /// 结束。
    fn cancel_loads(&mut self) {
        if let Some(handle) = self.current_load.take() {
            handle.abort();
            self.current_sessions = None;
        }
        if let Some(handle) = self.all_load.take() {
            handle.abort();
            self.all_sessions = None;
        }
    }

    /// 变更（删除/重命名）后的重载：取消全部加载、清空缓存，再加载当前作用域。
    ///
    /// 用“清空 + 重载”而不是局部更新，以 loader 返回的最新列表为准（排序与父子关系都会
    /// 重新计算）。`confirm_rename` 依赖随后的 `is_loading` 判断是否延迟退出 rename 面板。
    fn refresh_sessions_after_mutation(&mut self) {
        self.cancel_loads();
        self.current_sessions = None;
        self.all_sessions = None;
        self.begin_load(self.scope);
    }

    /// 切换 current/all。
    ///
    /// 先用缓存立即更新列表；缓存缺失且该作用域没有进行中的加载时才发起加载。若 loader
    /// 正在后台运行，这里只切换显示，等结果由 `sync` 落地。
    fn toggle_scope(&mut self) {
        self.scope = match self.scope {
            SessionScope::Current => SessionScope::All,
            SessionScope::All => SessionScope::Current,
        };
        let sessions = self.sessions_for(self.scope).cloned();
        let loading = self.is_loading(self.scope);
        self.header.set_scope(self.scope);
        self.header.set_loading(loading);
        self.pending_list = Some((sessions.clone().unwrap_or_default(), self.scope.is_all()));
        (self.request_render)();
        if sessions.is_none() && !loading {
            self.begin_load(self.scope);
        }
    }

    /// 循环切换排序：Threaded → Recent → Relevance → Threaded。返回新值。
    fn toggle_sort_mode(&mut self) -> SortMode {
        self.sort_mode = match self.sort_mode {
            SortMode::Threaded => SortMode::Recent,
            SortMode::Recent => SortMode::Relevance,
            SortMode::Relevance => SortMode::Threaded,
        };
        self.header.set_sort_mode(self.sort_mode);
        (self.request_render)();
        self.sort_mode
    }

    /// 在 All ↔ Named 之间切换名称过滤。返回新值。
    fn toggle_name_filter(&mut self) -> NameFilter {
        self.name_filter = match self.name_filter {
            NameFilter::All => NameFilter::Named,
            NameFilter::Named => NameFilter::All,
        };
        self.header.set_name_filter(self.name_filter);
        (self.request_render)();
        self.name_filter
    }

    /// 在当前作用域的缓存里按**路径字符串完全相等**查找显示名，用于 rename 输入框预填。
    ///
    /// 缓存缺失（尚未加载/已取消）时返回 `None`，输入框留空。
    fn session_name_for(&self, path: &str) -> Option<String> {
        self.sessions_for(self.scope)
            .and_then(|sessions| sessions.iter().find(|session| session.path == path))
            .and_then(|session| session.name.clone())
    }

    /// 删除流程（TS `sessionList.onDeleteSession`）。
    ///
    /// 1. 同步执行 `delete_session_file`（可能 spawn `trash`，会阻塞调用线程）。
    /// 2. 成功：从两个缓存里删除**路径字符串完全相等**的条目，把当前作用域的剩余列表交给
    ///    列表，显示 2 秒状态消息，再重载当前作用域。
    /// 3. 失败：显示 3 秒错误消息，不改缓存。
    ///
    /// 缓存删除使用原始 `path`，不做规范化；以别名路径出现的同一会话要等重载后的结果。
    fn handle_delete_session(&mut self, path: &str) {
        let result = delete_session_file(path);
        if result.ok {
            if let Some(sessions) = self.current_sessions.as_mut() {
                sessions.retain(|session| session.path != path);
            }
            if let Some(sessions) = self.all_sessions.as_mut() {
                sessions.retain(|session| session.path != path);
            }
            let sessions = self.sessions_for(self.scope).cloned().unwrap_or_default();
            self.pending_list = Some((sessions, self.scope.is_all()));
            let message = if result.method == DeleteMethod::Trash {
                "Session moved to trash"
            } else {
                "Session deleted"
            };
            self.header.set_status_message(
                Some(StatusMessage {
                    kind: StatusKind::Info,
                    message: message.to_string(),
                }),
                Some(2000),
            );
            self.refresh_sessions_after_mutation();
        } else {
            let error_message = result.error.unwrap_or_else(|| "Unknown error".to_string());
            self.header.set_status_message(
                Some(StatusMessage {
                    kind: StatusKind::Error,
                    message: format!("Failed to delete: {error_message}"),
                }),
                Some(3000),
            );
        }
        (self.request_render)();
    }
}

// ---------------------------------------------------------------------------
// 删除
// ---------------------------------------------------------------------------

/// JS `.slice(start, end)`（UTF-16 code unit）。
///
/// 用于把 `trash` 错误提示限制在 200 个 UTF-16 单元（TS `.slice(0, 200)`）。若切点落在
/// 代理对中间，`from_utf16_lossy` 会产生替换字符（与 JS 保留孤立代理项不同）。
fn slice_utf16(text: &str, start: usize, end: usize) -> String {
    let units: Vec<u16> = text
        .encode_utf16()
        .skip(start)
        .take(end.saturating_sub(start))
        .collect();
    String::from_utf16_lossy(&units)
}

/// Node `fs.unlink` 的错误文案：`<CODE>: <strerror>, unlink '<path>'`。
///
/// 已知 errno 走 `node_errno` 的表；未知 errno（或非 Unix 未映射的 `ErrorKind`）退回
/// Rust 的 `io::Error` Display——此时文案与 Node 不同。
fn node_unlink_error_message(error: &std::io::Error, path: &str) -> String {
    match node_errno(error) {
        Some((code, text)) => format!("{code}: {text}, unlink '{path}'"),
        None => error.to_string(),
    }
}

/// libuv 的 errno 名称与 strerror 文本（Node 的错误消息用它们）。
///
/// 表项文案固定对齐 libuv（错误消息是对外稳定契约，改动需同步测试）；只按 `raw_os_error()`
/// 的数值匹配，未列出的 errno 返回 `None`，由调用方退回 Rust 的错误文案。
#[cfg(unix)]
fn node_errno(error: &std::io::Error) -> Option<(&'static str, &'static str)> {
    let raw = error.raw_os_error()?;
    let table: &[(i32, &'static str, &'static str)] = &[
        (libc::ENOENT, "ENOENT", "no such file or directory"),
        (libc::EACCES, "EACCES", "permission denied"),
        (libc::EPERM, "EPERM", "operation not permitted"),
        (libc::EISDIR, "EISDIR", "illegal operation on a directory"),
        (libc::ENOTDIR, "ENOTDIR", "not a directory"),
        (libc::EBUSY, "EBUSY", "resource busy or locked"),
        (libc::EROFS, "EROFS", "read-only file system"),
        (libc::ELOOP, "ELOOP", "too many symbolic links encountered"),
        (libc::ENAMETOOLONG, "ENAMETOOLONG", "name too long"),
        (libc::ENOSPC, "ENOSPC", "no space left on device"),
        (libc::EMFILE, "EMFILE", "too many open files"),
        (libc::ENFILE, "ENFILE", "file table overflow"),
        (libc::EXDEV, "EXDEV", "cross-device link not permitted"),
        (libc::ETXTBSY, "ETXTBSY", "text file busy"),
        (libc::EIO, "EIO", "i/o error"),
    ];
    table
        .iter()
        .find(|(code, _, _)| *code == raw)
        .map(|(_, code, text)| (*code, *text))
}

#[cfg(not(unix))]
fn node_errno(error: &std::io::Error) -> Option<(&'static str, &'static str)> {
    match error.kind() {
        std::io::ErrorKind::NotFound => Some(("ENOENT", "no such file or directory")),
        std::io::ErrorKind::PermissionDenied => Some(("EACCES", "permission denied")),
        _ => None,
    }
}

/// `deleteSessionFile(sessionPath)`：先试 `trash`，失败回退 `unlink`。
///
/// `trash` 走 `kiana_core::child_process::spawn_process_sync`（Node `spawnSync` 的等价物），
/// 因此 spawn 失败的 `error.message`（`spawnSync trash ENOENT`）与 Node 一致；`unlink`
/// 的失败文案按 Node 的 errno 格式（`ENOENT: no such file or directory, unlink '<path>'`）
/// 拼装，而不是 Rust 的 `io::Error` Display。
pub fn delete_session_file(session_path: &str) -> DeleteSessionResult {
    let mut args: Vec<String> = Vec::new();
    // 以 `-` 开头的路径先加 `--` 作为选项结束符（TS 相同）。
    if session_path.starts_with('-') {
        args.push("--".to_string());
    }
    args.push(session_path.to_string());
    let trash_result = spawn_process_sync_bytes("trash", &args, &SpawnSyncOptions::default());

    let trash_error_hint = {
        let mut parts: Vec<String> = Vec::new();
        if let Some(error) = &trash_result.error {
            // `node_spawn_error` 已经把消息改写成 `spawnSync trash ENOENT`。
            parts.push(error.to_string());
        }
        let stderr = String::from_utf8_lossy(&trash_result.stderr);
        // 与 JS 一致：先 `trim()`，再 `split("\n")[0]`（保留行尾的 `\r`）。
        let stderr = stderr.trim_matches(is_js_whitespace);
        if !stderr.is_empty() {
            parts.push(stderr.split('\n').next().unwrap_or(stderr).to_string());
        }
        if parts.is_empty() {
            None
        } else {
            // TS `.slice(0, 200)` 按 UTF-16 单元截断。
            Some(format!(
                "trash: {}",
                slice_utf16(&parts.join(" · "), 0, 200)
            ))
        }
    };

    // `trash` 成功或文件已不存在都算成功（TS `!existsSync`）；`Path::exists` 跟随
    // 符号链接，与 Node 的 `existsSync` 在常规文件上一致。
    if trash_result.status == Some(0) || !Path::new(session_path).exists() {
        return DeleteSessionResult {
            ok: true,
            method: DeleteMethod::Trash,
            error: None,
        };
    }

    // 回退到同步 unlink。Unix 上目录会以 EISDIR 失败（见测试
    // `delete_failure_reports_node_style_error_text`）；错误文案按 Node 的形状拼装。
    match std::fs::remove_file(session_path) {
        Ok(()) => DeleteSessionResult {
            ok: true,
            method: DeleteMethod::Unlink,
            error: None,
        },
        Err(error) => {
            let unlink_error = node_unlink_error_message(&error, session_path);
            // unlink 错误为主，trash 的提示放在括号里（TS 的模板相同）。
            let error = match trash_error_hint {
                Some(hint) => format!("{unlink_error} ({hint})"),
                None => unlink_error,
            };
            DeleteSessionResult {
                ok: false,
                method: DeleteMethod::Unlink,
                error: Some(error),
            }
        }
    }
}

// ---------------------------------------------------------------------------
// SessionList
// ---------------------------------------------------------------------------

/// 会话列表组件（TS `SessionList`，模块私有；Rust 侧公开供宿主/测试设置回调）。
///
/// 不变量：
/// * `filtered_sessions` 的每个 `session_index` 都是 `all_sessions` 的合法下标；
/// * `filter_sessions` 结束后 `selected_index` 落在 `0..len`（空列表为 0）；
/// * `sort_mode` / `name_filter` / `show_path` 是展示镜像；内置交互以 `core` 为准，
///   按键路径会在回调后重新从 core 读取排序/过滤值。
pub struct SessionList {
    all_sessions: Vec<SessionInfo>,
    filtered_sessions: Vec<FlatSessionNode>,
    selected_index: isize,
    /// 用户是否移动过选择。为 false 时重新加载后回到第一项；为 true 时尽量保留原路径。
    selection_touched: bool,
    search_input: Input,
    show_cwd: bool,
    sort_mode: SortMode,
    name_filter: NameFilter,
    keybindings: KeybindingsManager,
    show_path: bool,
    confirming_delete_path: Option<String>,
    current_session_canonical_path: Option<String>,
    max_visible: usize,
    focused: bool,
    core: Rc<RefCell<SelectorCore>>,
    /// 是否允许重命名（宿主提供 `renameSession` 时为 true）。
    pub can_rename: bool,
    /// 选中回调（宿主绑定；内置包装会先清状态并取消加载，再调用宿主回调）。
    pub on_select: Option<StringCallback>,
    /// 取消回调（同上，内置包装会先清状态并取消加载）。
    pub on_cancel: Option<VoidCallback>,
    /// 退出回调（TS 中列表从不主动调用）。
    pub on_exit: Option<VoidCallback>,
    /// 切换作用域；`None` 时按键是 no-op。
    pub on_toggle_scope: Option<VoidCallback>,
    /// 切换排序；`None` 时按键是 no-op（之后仍会从 core 读取 `sort_mode` 同步镜像）。
    pub on_toggle_sort: Option<VoidCallback>,
    /// 切换名称过滤；`None` 时按键是 no-op（之后仍会从 core 读取镜像）。
    pub on_toggle_name_filter: Option<VoidCallback>,
    /// 路径显示变化。
    pub on_toggle_path: Option<BoolCallback>,
    /// 删除确认态变化。
    pub on_delete_confirmation_change: Option<OptionalStringCallback>,
    /// 删除执行回调；`None` 时确认键只退出确认态，不执行删除。
    pub on_delete_session: Option<StringCallback>,
    /// 重命名请求；内置回调把目标写入 core 的 `pending_rename`，由组件进入 rename 模式。
    pub on_rename_session: Option<StringCallback>,
    /// 错误提示。
    pub on_error: Option<StringCallback>,
}

impl SessionList {
    /// 建列表并立即用空查询过滤一次（此时 `all_sessions` 来自构造参数）。
    fn new(
        sessions: Vec<SessionInfo>,
        show_cwd: bool,
        sort_mode: SortMode,
        name_filter: NameFilter,
        keybindings: KeybindingsManager,
        current_session_file_path: Option<String>,
        core: Rc<RefCell<SelectorCore>>,
    ) -> Self {
        let mut list = SessionList {
            all_sessions: sessions,
            filtered_sessions: Vec::new(),
            selected_index: 0,
            selection_touched: false,
            search_input: Input::new(Default::default()),
            show_cwd,
            sort_mode,
            name_filter,
            keybindings,
            show_path: false,
            confirming_delete_path: None,
            current_session_canonical_path: canonicalize_selector_path(
                current_session_file_path.as_deref(),
            ),
            max_visible: 10,
            focused: false,
            core,
            can_rename: false,
            on_select: None,
            on_cancel: None,
            on_exit: None,
            on_toggle_scope: None,
            on_toggle_sort: None,
            on_toggle_name_filter: None,
            on_toggle_path: None,
            on_delete_confirmation_change: None,
            on_delete_session: None,
            on_rename_session: None,
            on_error: None,
        };
        list.filter_sessions("");
        list
    }

    /// `getSelectedSessionPath()`：当前高亮项的会话文件路径；无选中项（空列表或
    /// `selected_index < 0`）时为 `None`。
    pub fn get_selected_session_path(&self) -> Option<&str> {
        self.selected_node()
            .and_then(|node| self.all_sessions.get(node.session_index))
            .map(|session| session.path.as_str())
    }

    /// `selected_node`：把 `selected_index` 映射到过滤后的节点；负索引视为无选中。
    fn selected_node(&self) -> Option<&FlatSessionNode> {
        if self.selected_index < 0 {
            return None;
        }
        self.filtered_sessions.get(self.selected_index as usize)
    }

    /// `setSortMode(sortMode)`：更新本地镜像并按当前搜索词重新过滤；不请求重绘。
    pub fn set_sort_mode(&mut self, sort_mode: SortMode) {
        self.sort_mode = sort_mode;
        let query = self.search_input.get_value().to_string();
        self.filter_sessions(&query);
    }

    /// `setNameFilter(nameFilter)`：更新本地镜像并按当前搜索词重新过滤；不请求重绘。
    pub fn set_name_filter(&mut self, name_filter: NameFilter) {
        self.name_filter = name_filter;
        let query = self.search_input.get_value().to_string();
        self.filter_sessions(&query);
    }

    /// `setSessions(sessions, showCwd)`：用新列表替换全部数据并重新过滤。
    ///
    /// `show_cwd` 同时控制右侧 cwd 列与空列表文案（它对应“全部”作用域）。选中项处理：
    /// 用户从未移动过选择时回到索引 0；否则尝试按路径字符串（非规范化）保持原选中项，
    /// 找不到时保留过滤后钳制过的旧索引。
    pub fn set_sessions(&mut self, sessions: Vec<SessionInfo>, show_cwd: bool) {
        let selected_path = if self.selection_touched {
            self.get_selected_session_path().map(str::to_string)
        } else {
            None
        };
        self.all_sessions = sessions;
        self.show_cwd = show_cwd;
        let query = self.search_input.get_value().to_string();
        self.filter_sessions(&query);
        if !self.selection_touched {
            self.selected_index = 0;
        } else if let Some(selected_path) = selected_path
            && let Some(index) = self.filtered_sessions.iter().position(|node| {
                self.all_sessions
                    .get(node.session_index)
                    .is_some_and(|session| session.path == selected_path)
            })
        {
            self.selected_index = index as isize;
        }
    }

    /// 按名称过滤 + 搜索词重建 `filtered_sessions`，最后钳制 `selected_index`。
    ///
    /// 两条路径：
    /// * Threaded 且搜索词为空：按父子关系建树并摊平（树节点只有这一个来源）。
    /// * 其它情况（Recent/Relevance 或带搜索的 Threaded）：`filter_and_sort_session_refs`
    ///   返回 `all_sessions` 的引用，这里再用指针地址映射回下标。空查询时该函数按输入
    ///   顺序原样返回，因此 Recent 的“保持顺序”语义由 loader 顺序决定。
    ///
    /// 名称过滤先于搜索：`NameFilter::Named` 只保留 `has_session_name` 为真的会话。
    fn filter_sessions(&mut self, query: &str) {
        let trimmed = js_trim(query);
        let selected: Vec<usize> = self
            .all_sessions
            .iter()
            .enumerate()
            .filter(|(_, session)| self.name_filter == NameFilter::All || has_session_name(session))
            .map(|(index, _)| index)
            .collect();

        if self.sort_mode == SortMode::Threaded && trimmed.is_empty() {
            let tree = build_session_tree(&self.all_sessions, &selected);
            self.filtered_sessions = flatten_session_tree(&tree);
        } else {
            let refs: Vec<&SessionInfo> = selected
                .iter()
                .map(|index| &self.all_sessions[*index])
                .collect();
            let filtered = filter_and_sort_session_refs(&refs, query, self.sort_mode);
            let mut index_of: HashMap<*const SessionInfo, usize> = HashMap::new();
            for index in &selected {
                index_of.insert(&self.all_sessions[*index] as *const SessionInfo, *index);
            }
            self.filtered_sessions = filtered
                .into_iter()
                .map(|session| FlatSessionNode {
                    session_index: *index_of.get(&(session as *const SessionInfo)).unwrap_or(&0),
                    depth: 0,
                    is_last: true,
                    ancestor_continues: Vec::new(),
                })
                .collect();
        }

        self.selected_index = self
            .selected_index
            .min((self.filtered_sessions.len() as isize - 1).max(0));
    }

    /// 进入/退出删除确认态，并把新状态交给宿主（内置回调会同步到头部）。
    ///
    /// 测试可以整体替换该回调来观察状态变化，此时头部不再跟随。
    fn set_confirming_delete_path(&mut self, path: Option<String>) {
        self.confirming_delete_path = path.clone();
        // 头部更新由组件安装的回调完成（测试可替换该回调）。
        if let Some(callback) = &mut self.on_delete_confirmation_change {
            callback(path.as_deref());
        }
    }

    /// 为当前选中项进入删除确认态；当前正在使用的会话被禁止删除。
    ///
    /// 没有选中项时什么都不做。若选中项就是当前活动会话（规范化路径比较，能识别符号链接
    /// 别名），通过 `on_error` 报错且不进入确认态。
    fn start_delete_confirmation_for_selected_session(&mut self) {
        let Some(selected) = self.selected_node() else {
            return;
        };
        let path = self.all_sessions[selected.session_index].path.clone();
        if self.is_current_session_path(&path) {
            // TS: `this.onError?.("Cannot delete the currently active session")`。
            if let Some(on_error) = &mut self.on_error {
                on_error("Cannot delete the currently active session");
            }
            return;
        }
        self.set_confirming_delete_path(Some(path));
    }

    /// 判断某个路径是否就是当前活动会话：双方都先规范化，符号链接别名视为同一文件。
    ///
    /// 构造时没有传 `currentSessionFilePath` 时恒为 false。
    fn is_current_session_path(&self, path: &str) -> bool {
        let Some(current) = self.current_session_canonical_path.as_ref() else {
            return false;
        };
        let resolved = canonicalize_selector_path(Some(path)).unwrap_or_else(|| path.to_string());
        &resolved == current
    }

    /// 同步核心中已完成/进行中的加载结果。
    ///
    /// `core.sync()` 与 `take_pending_list()` 在同一个借用块里完成，随后释放借用再做
    /// `set_sessions`——避免列表在更新时又通过回调借用 core。
    pub fn sync_from_core(&mut self) {
        let pending = {
            let mut core = self.core.borrow_mut();
            core.sync();
            core.take_pending_list()
        };
        if let Some((sessions, show_cwd)) = pending {
            self.set_sessions(sessions, show_cwd);
        }
    }

    /// `render(width)`：先渲染搜索框与空态，再按 `max_visible` 开窗渲染列表行，最后按需
    /// 追加滚动指示。
    ///
    /// 行布局：光标（2 列）+ 树前缀 + 消息文本 + 填充空格 + 右侧元信息。消息文本先按可用
    /// 宽度截断（下限 10 列，`…`），整行再按 `width` 截断。颜色优先级：删除确认（error）>
    /// 当前会话（accent）> 有名称（warning）；选中行额外加粗并铺 `SelectedBg` 背景。
    /// `now` 每帧只取一次，所有行共用同一时间基准。
    pub fn render(&mut self, width: usize) -> Vec<String> {
        let mut lines: Vec<String> = Vec::new();
        lines.extend(self.search_input.render(width));
        lines.push(String::new());

        if self.filtered_sessions.is_empty() {
            // 空态文案区分“名称过滤”与作用域：All 作用域提示没有会话，Current 作用域
            // 提示可以按 Tab 查看全部。
            let empty_message = if self.name_filter == NameFilter::Named {
                let toggle_key = key_text("app.session.toggleNamedFilter");
                if self.show_cwd {
                    format!("  No named sessions found. Press {toggle_key} to show all.")
                } else {
                    format!(
                        "  No named sessions in current folder. Press {toggle_key} to show all, or Tab to view all."
                    )
                }
            } else if self.show_cwd {
                "  No sessions found".to_string()
            } else {
                "  No sessions in current folder. Press Tab to view all.".to_string()
            };
            lines.push(fg(
                ThemeColor::Muted,
                &truncate_to_width(&empty_message, width, "…", false),
            ));
            return lines;
        }

        let length = self.filtered_sessions.len();
        // 窗口让高亮项尽量居中，但不越过列表两端；`max_visible` 固定为 10 行。
        let start_index = (self.selected_index - (self.max_visible as isize) / 2)
            .min(length as isize - self.max_visible as isize)
            .max(0) as usize;
        let end_index = (start_index + self.max_visible).min(length);
        let now = now_ms();

        for index in start_index..end_index {
            let node = self.filtered_sessions[index].clone();
            let session = &self.all_sessions[node.session_index];
            let is_selected = index as isize == self.selected_index;
            let is_confirming_delete = self
                .confirming_delete_path
                .as_deref()
                .is_some_and(|path| path == session.path);
            let is_current = self.is_current_session_path(&session.path);

            let prefix = build_tree_prefix(&node);
            let has_name = session.name.as_deref().is_some_and(|name| !name.is_empty());
            let display_text = session
                .name
                .clone()
                .unwrap_or_else(|| session.first_message.clone());
            let normalized_message = normalize_control_chars(&display_text);

            // 右侧列逐项前插：基础是“消息数 + 相对时间”；先插入 cwd（All 作用域且非空），
            // 再插入路径（Ctrl+P），最终顺序是 path / cwd / 消息数 / 相对时间。
            let age = format_session_date(session.modified, now);
            let mut right_part = format!("{} {}", session.message_count, age);
            if self.show_cwd && !session.cwd.is_empty() {
                right_part = format!("{} {}", shorten_path(&session.cwd), right_part);
            }
            if self.show_path {
                right_part = format!("{} {}", shorten_path(&session.path), right_part);
            }

            let cursor = if is_selected {
                fg(ThemeColor::Accent, "› ")
            } else {
                "  ".to_string()
            };

            let prefix_width = visible_width(&prefix);
            let right_width = visible_width(&right_part) + 2;
            let available_for_msg =
                width as isize - 2 - prefix_width as isize - right_width as isize;
            let truncated_msg = truncate_to_width(
                &normalized_message,
                available_for_msg.max(10) as usize,
                "…",
                false,
            );

            // 颜色优先级固定为：删除确认 > 当前会话 > 有名称；选中再加粗。
            let message_color = if is_confirming_delete {
                Some(ThemeColor::Error)
            } else if is_current {
                Some(ThemeColor::Accent)
            } else if has_name {
                Some(ThemeColor::Warning)
            } else {
                None
            };
            let mut styled_msg = match message_color {
                Some(color) => fg(color, &truncated_msg),
                None => truncated_msg,
            };
            if is_selected {
                styled_msg = bold(&styled_msg);
            }

            let left_part = format!("{cursor}{}{styled_msg}", fg(ThemeColor::Dim, &prefix));
            let left_width = visible_width(&left_part);
            let spacing = width
                .saturating_sub(left_width + visible_width(&right_part))
                .max(1);
            let styled_right = fg(
                if is_confirming_delete {
                    ThemeColor::Error
                } else {
                    ThemeColor::Dim
                },
                &right_part,
            );
            let mut line = format!("{left_part}{}{styled_right}", " ".repeat(spacing));
            if is_selected {
                line = bg(ThemeBg::SelectedBg, &line);
            }
            lines.push(truncate_to_width(&line, width, "...", false));
        }

        // 窗口没有覆盖整个列表时显示 (当前/总数)，提示还可以继续滚动。
        if start_index > 0 || end_index < length {
            let scroll_text = format!("  ({}/{})", self.selected_index + 1, length);
            lines.push(fg(
                ThemeColor::Muted,
                &truncate_to_width(&scroll_text, width, "", false),
            ));
        }

        lines
    }

    /// `handleInput(keyData)`：按固定优先级消费按键。
    ///
    /// 1. 删除确认态拦截所有键：confirm 执行删除，cancel 取消，其余忽略。
    /// 2. 快捷键分支（每个都提前返回）：Tab 切作用域、toggleSort、toggleNamedFilter
    ///    （使用构造时注入的 keybindings，其余键用全局 `get_keybindings()`）、togglePath、
    ///    delete/deleteNoninvasive、rename。
    /// 3. 其余按键先标记 `selection_touched`，再依次处理上下/翻页/确认/取消/提交；都匹配
    ///    不上时交给搜索框并重新过滤。
    ///
    /// 入口先 `sync_from_core`，让后台加载结果先反映到列表；会改动列表的回调分支结束后
    /// 也会再同步一次。
    pub fn handle_input(&mut self, key_data: &str) {
        self.sync_from_core();
        let kb = get_keybindings();

        // 删除确认态优先拦截所有键。
        if let Some(path) = self.confirming_delete_path.clone() {
            if kb.matches(key_data, "tui.select.confirm") {
                self.set_confirming_delete_path(None);
                // TS: `void this.onDeleteSession?.(pathToDelete)`——没有回调时什么都不做。
                if let Some(callback) = &mut self.on_delete_session {
                    callback(&path);
                }
                self.sync_from_core();
            } else if kb.matches(key_data, "tui.select.cancel") {
                self.set_confirming_delete_path(None);
            }
            return;
        }

        if kb.matches(key_data, "tui.input.tab") {
            // TS 只调用 `this.onToggleScope?.()`；作用域切换由组件安装的回调完成。
            if let Some(callback) = &mut self.on_toggle_scope {
                callback();
            }
            self.sync_from_core();
            return;
        }

        if kb.matches(key_data, "app.session.toggleSort") {
            if let Some(callback) = &mut self.on_toggle_sort {
                callback();
            }
            // 组件回调更新 core 的 sortMode 与头部；列表随后从 core 读取并同步本地镜像、
            // 重新过滤（TS 是由组件直接调用 `sessionList.setSortMode`）。
            let sort_mode = self.core.borrow().sort_mode;
            self.set_sort_mode(sort_mode);
            self.sync_from_core();
            return;
        }

        if self
            .keybindings
            .matches(key_data, "app.session.toggleNamedFilter")
        {
            if let Some(callback) = &mut self.on_toggle_name_filter {
                callback();
            }
            // 与排序分支相同：组件回调只改 core/头部，列表从 core 读取新值并重新过滤。
            let name_filter = self.core.borrow().name_filter;
            self.set_name_filter(name_filter);
            self.sync_from_core();
            return;
        }

        if kb.matches(key_data, "app.session.togglePath") {
            // 路径显示是列表本地状态；回调只负责让头部镜像同步，缺失时本地仍会切换。
            self.show_path = !self.show_path;
            if let Some(callback) = &mut self.on_toggle_path {
                callback(self.show_path);
            }
            return;
        }

        if kb.matches(key_data, "app.session.delete") {
            self.start_delete_confirmation_for_selected_session();
            return;
        }

        if kb.matches(key_data, "app.session.rename") {
            // TS 只调用 `this.onRenameSession?.(path)`；内置的进入 rename 模式由组件安装的
            // 回调完成，替换该回调即可抑制内置行为。
            if let Some(selected) = self.selected_node() {
                let path = self.all_sessions[selected.session_index].path.clone();
                if let Some(callback) = &mut self.on_rename_session {
                    callback(&path);
                }
            }
            return;
        }

        if kb.matches(key_data, "app.session.deleteNoninvasive") {
            if !self.search_input.get_value().is_empty() {
                self.search_input.handle_input(key_data);
                let query = self.search_input.get_value().to_string();
                self.filter_sessions(&query);
                return;
            }
            self.start_delete_confirmation_for_selected_session();
            return;
        }

        // 从这里开始都属于“用户操作列表内容”：即使最终是文本输入，也视为移动过选择，
        // 重新加载后按路径保留选中项而不是回到第一行。
        self.selection_touched = true;
        if kb.matches(key_data, "tui.select.up") {
            self.selected_index = (self.selected_index - 1).max(0);
        } else if kb.matches(key_data, "tui.select.down") {
            self.selected_index =
                (self.selected_index + 1).min(self.filtered_sessions.len() as isize - 1);
        } else if kb.matches(key_data, "tui.select.pageUp") {
            self.selected_index = (self.selected_index - self.max_visible as isize).max(0);
        } else if kb.matches(key_data, "tui.select.pageDown") {
            self.selected_index = (self.selected_index + self.max_visible as isize)
                .min(self.filtered_sessions.len() as isize - 1);
        } else if kb.matches(key_data, "tui.select.confirm") {
            if let Some(selected) = self.selected_node() {
                let path = self.all_sessions[selected.session_index].path.clone();
                if let Some(callback) = &mut self.on_select {
                    callback(&path);
                }
            }
        } else if kb.matches(key_data, "tui.select.cancel") {
            if let Some(callback) = &mut self.on_cancel {
                callback();
            }
        } else if kb.matches(key_data, "tui.input.submit") || key_data == "\n" {
            // TS 通过 `searchInput.onSubmit` 选中当前项。Input 把 `tui.input.submit` 和
            // 字面 "\n"（Kitty 协议的 shift+enter）都当作提交；这里复刻同样的条件。
            if let Some(selected) = self.selected_node() {
                let path = self.all_sessions[selected.session_index].path.clone();
                if let Some(callback) = &mut self.on_select {
                    callback(&path);
                }
            }
        } else {
            // 其余按键（可打印字符、退格、方向键等）交给搜索框；每次输入后重新过滤。
            self.search_input.handle_input(key_data);
            let query = self.search_input.get_value().to_string();
            self.filter_sessions(&query);
        }
    }

    /// `invalidate()`（空实现）：列表没有需要失效的缓存，渲染时全部现算。
    pub fn invalidate(&mut self) {}
}

/// JS `.replace(/[\x00-\x1f\x7f]/g, " ").trim()`。
fn normalize_control_chars(text: &str) -> String {
    text.chars()
        .map(|ch| {
            let code = ch as u32;
            if code <= 0x1f || code == 0x7f {
                ' '
            } else {
                ch
            }
        })
        .collect::<String>()
        .trim_matches(is_js_whitespace)
        .to_string()
}

/// `Component` 实现只是把调用转发到同名 inherent 方法，供 TUI 容器以 trait object 使用。
impl Component for SessionList {
    fn render(&mut self, width: usize) -> Vec<String> {
        SessionList::render(self, width)
    }

    fn handle_input(&mut self, data: &str) {
        SessionList::handle_input(self, data);
    }

    fn invalidate(&mut self) {
        SessionList::invalidate(self);
    }

    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }

    fn as_focusable(&mut self) -> Option<&mut dyn Focusable> {
        Some(self)
    }
}

/// 焦点转发给搜索框：Input 需要自己的 `focused` 状态来放置 IME 光标（TS 同样的转发）。
impl Focusable for SessionList {
    fn focused(&self) -> bool {
        self.focused
    }

    fn set_focused(&mut self, focused: bool) {
        self.focused = focused;
        self.search_input.set_focused(focused);
    }
}

// ---------------------------------------------------------------------------
// SessionSelectorComponent
// ---------------------------------------------------------------------------

/// 选择器的两个互斥子状态：列表浏览 / 重命名输入。`Rename` 时头部与列表不渲染。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SelectorMode {
    List,
    Rename,
}

/// `/resume` 会话选择器（TS `SessionSelectorComponent`）。
///
/// 组合根：`core` 保存加载/作用域状态，`session_list` 负责列表交互，`rename_input` 与
/// `mode`/`rename_target_path` 构成重命名子状态机：
///
/// * `List`：按键交给列表；列表的 `on_rename_session` 只把目标写进 core 的
///   `pending_rename`，组件在 `handle_input`/`render` 里取出并进入 `Rename`。
/// * `Rename`：按键交给 `rename_input`；提交后调用宿主 `rename_session` 并重载当前
///   作用域，加载结束（`rename_refresh_pending` 清除）后才退出 `Rename`。
///
/// 组件不直接持有会话列表，所有缓存都在 `core` 里。
pub struct SessionSelectorComponent {
    core: Rc<RefCell<SelectorCore>>,
    session_list: SessionList,
    rename_input: Input,
    rename_session: Option<RenameSessionFn>,
    mode: SelectorMode,
    rename_target_path: Option<String>,
    /// `confirmRename` 已发起刷新：等这次加载结束后再退出 rename 模式（TS 的 `finally` 时机）。
    rename_refresh_pending: bool,
    focused: bool,
}

impl SessionSelectorComponent {
    /// `new(currentSessionsLoader, allSessionsLoader, onSelect, onCancel, onExit, requestRender,
    ///      options?, currentSessionFilePath?)`。
    ///
    /// 构造末尾立即开始加载 current scope，避免首次渲染时列表为空。
    ///
    /// 顺序：
    /// 1. 解析 keybindings、`can_rename` 与 rename 提示，建头部；
    /// 2. 建空列表（此时还没有会话，稍后由加载结果填充）；
    /// 3. 安装列表回调。`on_select`/`on_cancel`/`on_exit` 的包装先清状态并取消加载，
    ///    然后**在 core 借用释放后**才调用宿主回调；其余回调直接驱动 core；
    /// 4. `begin_load(Current)`，并立即取一次 `pending_list`（loader 可能在返回 future
    ///    之前同步产生部分结果）。
    ///
    /// `rename_session` 为 `None` 时 `can_rename = false`，但 `show_rename_hint` 仍可由
    /// 调用方单独覆盖。
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        current_sessions_loader: SessionsLoader,
        all_sessions_loader: SessionsLoader,
        on_select: StringCallback,
        on_cancel: VoidCallback,
        on_exit: VoidCallback,
        request_render: RequestRenderFn,
        options: SessionSelectorOptions,
        current_session_file_path: Option<String>,
    ) -> Self {
        let keybindings = options
            .keybindings
            .unwrap_or_else(KeybindingsManager::create_default);
        let can_rename = options.rename_session.is_some();
        let mut header = SessionSelectorHeader::new(
            SessionScope::Current,
            SortMode::Threaded,
            NameFilter::All,
            Arc::clone(&request_render),
        );
        header.set_show_rename_hint(options.show_rename_hint.unwrap_or(can_rename));

        let core = Rc::new(RefCell::new(SelectorCore {
            scope: SessionScope::Current,
            sort_mode: SortMode::Threaded,
            name_filter: NameFilter::All,
            header,
            current_sessions: None,
            all_sessions: None,
            current_load: None,
            all_load: None,
            current_loader: current_sessions_loader,
            all_loader: all_sessions_loader,
            request_render,
            pending_list: None,
            pending_rename: None,
            generation: 0,
        }));

        let mut session_list = SessionList::new(
            Vec::new(),
            false,
            SortMode::Threaded,
            NameFilter::All,
            keybindings,
            current_session_file_path,
            Rc::clone(&core),
        );
        session_list.can_rename = can_rename;

        // 三个“离开选择器”的回调共用同一包装：先清状态消息并取消所有加载，再调用宿主；
        // 内层 `core` 借用在调用宿主前已经释放，避免宿主重入组件时二次借用。
        let core_for_select = Rc::clone(&core);
        let mut user_on_select = on_select;
        session_list.on_select = Some(Box::new(move |path: &str| {
            {
                let mut core = core_for_select.borrow_mut();
                core.header.set_status_message(None, None);
                core.cancel_loads();
            }
            user_on_select(path);
        }));

        let core_for_cancel = Rc::clone(&core);
        let mut user_on_cancel = on_cancel;
        session_list.on_cancel = Some(Box::new(move || {
            {
                let mut core = core_for_cancel.borrow_mut();
                core.header.set_status_message(None, None);
                core.cancel_loads();
            }
            user_on_cancel();
        }));

        let core_for_exit = Rc::clone(&core);
        let mut user_on_exit = on_exit;
        session_list.on_exit = Some(Box::new(move || {
            {
                let mut core = core_for_exit.borrow_mut();
                core.header.set_status_message(None, None);
                core.cancel_loads();
            }
            user_on_exit();
        }));

        // Sync list events to header（与 TS 的构造回调绑定一致）。
        let core_for_path = Rc::clone(&core);
        session_list.on_toggle_path = Some(Box::new(move |show_path: bool| {
            let mut core = core_for_path.borrow_mut();
            core.header.set_show_path(show_path);
            (core.request_render)();
        }));
        let core_for_confirmation = Rc::clone(&core);
        session_list.on_delete_confirmation_change = Some(Box::new(move |path: Option<&str>| {
            let mut core = core_for_confirmation.borrow_mut();
            core.header
                .set_confirming_delete_path(path.map(str::to_string));
            (core.request_render)();
        }));
        // TS 构造器安装的 `onRenameSession`：无重命名回调或正在加载时忽略，否则进入
        // rename 模式。Rust 侧先把目标写入 `pending_rename`，由组件在下一次 render/输入时
        // 实际切换模式（见 `SessionSelectorComponent::handle_input`）。
        let core_for_rename = Rc::clone(&core);
        session_list.on_rename_session = Some(Box::new(move |path: &str| {
            if !can_rename {
                return;
            }
            let mut core = core_for_rename.borrow_mut();
            if core.is_loading(core.scope) {
                return;
            }
            core.pending_rename = Some(path.to_string());
        }));
        // TS 构造器安装的 `onDeleteSession`（异步）；Rust 里删除是同步的。
        let core_for_delete = Rc::clone(&core);
        session_list.on_delete_session = Some(Box::new(move |path: &str| {
            core_for_delete.borrow_mut().handle_delete_session(path);
        }));
        let core_for_scope = Rc::clone(&core);
        session_list.on_toggle_scope = Some(Box::new(move || {
            core_for_scope.borrow_mut().toggle_scope();
        }));
        let core_for_sort = Rc::clone(&core);
        session_list.on_toggle_sort = Some(Box::new(move || {
            core_for_sort.borrow_mut().toggle_sort_mode();
        }));
        let core_for_name = Rc::clone(&core);
        session_list.on_toggle_name_filter = Some(Box::new(move || {
            core_for_name.borrow_mut().toggle_name_filter();
        }));
        let core_for_error = Rc::clone(&core);
        session_list.on_error = Some(Box::new(move |message: &str| {
            let mut core = core_for_error.borrow_mut();
            core.header.set_status_message(
                Some(StatusMessage {
                    kind: StatusKind::Error,
                    message: message.to_string(),
                }),
                Some(3000),
            );
            (core.request_render)();
        }));

        // 构造末尾立即加载 current scope。
        {
            let mut core = core.borrow_mut();
            core.begin_load(SessionScope::Current);
            if let Some((sessions, show_cwd)) = core.take_pending_list() {
                session_list.set_sessions(sessions, show_cwd);
            }
        }

        SessionSelectorComponent {
            core,
            session_list,
            rename_input: Input::new(Default::default()),
            rename_session: options.rename_session,
            mode: SelectorMode::List,
            rename_target_path: None,
            rename_refresh_pending: false,
            focused: false,
        }
    }

    /// `getSessionList()`：先把 core 的待落地结果同步给列表，再返回可变引用。
    pub fn get_session_list(&mut self) -> &mut SessionList {
        self.sync_list_from_core();
        &mut self.session_list
    }

    /// 让列表追上 core 的最新加载结果（`set_sessions` 可能改变高亮项）。
    fn sync_list_from_core(&mut self) {
        self.session_list.sync_from_core();
    }

    /// 进入 rename 模式：记录目标路径、用当前作用域缓存里的显示名预填输入框并聚焦。
    ///
    /// 缓存里找不到该路径（例如加载已取消）时输入框为空；实际保存仍会使用目标路径。
    fn enter_rename_mode(&mut self, session_path: &str) {
        self.mode = SelectorMode::Rename;
        self.rename_target_path = Some(session_path.to_string());
        let current_name = {
            let core = self.core.borrow();
            core.session_name_for(session_path)
        };
        self.rename_input
            .set_value(current_name.unwrap_or_default());
        self.rename_input.set_focused(true);
        (self.core.borrow().request_render)();
    }

    /// 退出 rename 模式并回到列表；同时清除“等待刷新”标记。
    fn exit_rename_mode(&mut self) {
        self.mode = SelectorMode::List;
        self.rename_target_path = None;
        self.rename_refresh_pending = false;
        (self.core.borrow().request_render)();
    }

    /// TS 的 `confirmRename` 在 `finally` 里退出 rename 模式，也就是要等
    /// `refreshSessionsAfterMutation()` 的加载 resolve；加载期间面板保持可见。
    ///
    /// Rust 的重载是“启动即返回”，因此用 `rename_refresh_pending` 记录待退出状态；
    /// `render` 与 rename 状态的 `handle_input` 每次都会调用本函数检查加载是否结束。
    fn maybe_finish_rename_refresh(&mut self) {
        if !self.rename_refresh_pending {
            return;
        }
        if self.mode != SelectorMode::Rename {
            self.rename_refresh_pending = false;
            return;
        }
        let loading = {
            let core = self.core.borrow();
            core.is_loading(core.scope)
        };
        if !loading {
            self.rename_refresh_pending = false;
            self.exit_rename_mode();
        }
    }

    /// 提交重命名：trim 后为空则留在输入态；没有目标路径或宿主回调时直接退出；
    /// 否则同步调用宿主回调，然后重载当前作用域。
    ///
    /// 重载期间保持 Rename 面板（`rename_refresh_pending`），等 `is_loading` 变为 false
    /// 后由 `maybe_finish_rename_refresh` 退出——对应 TS `confirmRename` 的 `finally`
    /// 时机。宿主回调没有返回值，Rust 侧无法感知持久化失败。
    fn confirm_rename(&mut self, value: &str) {
        let next = js_trim(value).to_string();
        if next.is_empty() {
            return;
        }
        let Some(target) = self.rename_target_path.clone() else {
            self.exit_rename_mode();
            return;
        };
        let Some(rename_session) = self.rename_session.as_mut() else {
            self.exit_rename_mode();
            return;
        };
        rename_session(&target, &next);
        {
            let mut core = self.core.borrow_mut();
            core.refresh_sessions_after_mutation();
            if core.is_loading(core.scope) {
                // 刷新还在进行：保持面板，等加载结束后在 render 里退出。
                self.rename_refresh_pending = true;
                return;
            }
        }
        self.exit_rename_mode();
    }

    /// rename 模式的按键：cancel 退出；submit（`tui.input.submit` 或字面 `"\n"`）提交；
    /// 其余全部交给 `rename_input`。注意提交不是 `tui.select.confirm`。
    fn handle_rename_input(&mut self, data: &str) {
        let kb = get_keybindings();
        if kb.matches(data, "tui.select.cancel") {
            self.exit_rename_mode();
            return;
        }
        // TS 把其余按键都交给 `renameInput.handleInput`；提交由 `Input.onSubmit` 完成
        // （`tui.input.submit` 或字面 "\n"），而不是 `tui.select.confirm`。
        if kb.matches(data, "tui.input.submit") || data == "\n" {
            let value = self.rename_input.get_value().to_string();
            self.confirm_rename(&value);
            return;
        }
        self.rename_input.handle_input(data);
    }

    /// 重命名面板：标题、输入框与保存/取消提示（TS 的 `enterRenameMode` 构建的面板）。
    fn render_rename_panel(&mut self, width: usize) -> Vec<String> {
        let confirm = key_text("tui.select.confirm");
        let cancel = key_text("tui.select.cancel");
        let mut lines: Vec<String> = Vec::new();
        lines.extend(text_lines(&bold("Rename Session"), 1, width));
        lines.push(String::new());
        lines.extend(self.rename_input.render(width));
        lines.push(String::new());
        lines.extend(text_lines(
            &fg(
                ThemeColor::Muted,
                &format!("{confirm} to save · {cancel} to cancel"),
            ),
            1,
            width,
        ));
        lines
    }
}

/// `Component` 实现：`render` 先同步 core 的加载结果、完成待处理的 rename 切换，再按
/// 模式拼装布局；`handle_input` 在 rename 态与列表态之间分派。
impl Component for SessionSelectorComponent {
    fn render(&mut self, width: usize) -> Vec<String> {
        // 后台结果只能在交互/渲染点落地；这里顺便检查 rename 刷新是否已结束。
        self.sync_list_from_core();
        self.maybe_finish_rename_refresh();
        // 宿主可能直接调用 `session_list.handle_input` 进入 rename（见下方 handle_input），
        // 那时 pending 会留到下一帧，这里补上模式切换。
        if self.mode == SelectorMode::List {
            let pending = { self.core.borrow_mut().take_pending_rename() };
            if let Some(path) = pending {
                self.enter_rename_mode(&path);
            }
        }

        // 布局固定为：空行、上边框、空行、内容、空行、下边框。rename 模式不渲染头部。
        let mut lines: Vec<String> = Vec::new();
        lines.push(String::new());
        lines.extend(accent_border_lines(width));
        lines.push(String::new());
        if self.mode == SelectorMode::List {
            let header_lines = {
                let mut core = self.core.borrow_mut();
                core.header.render(width)
            };
            lines.extend(header_lines);
            lines.push(String::new());
            lines.extend(self.session_list.render(width));
        } else {
            lines.extend(self.render_rename_panel(width));
        }
        lines.push(String::new());
        lines.extend(accent_border_lines(width));
        lines
    }

    fn handle_input(&mut self, data: &str) {
        if self.mode == SelectorMode::Rename {
            // 刷新加载可能已在上一个事件循环完成（TS 的 `await` 先于键盘事件 resolve）。
            self.sync_list_from_core();
            self.maybe_finish_rename_refresh();
            if self.mode == SelectorMode::Rename {
                self.handle_rename_input(data);
                return;
            }
        }
        // 两个 pending 检查点对应两种进入 rename 的时机：
        // * 这里：宿主绕过组件直接调用 `session_list.handle_input(ctrl+r)`，pending 已在上一
        //   次事件写入；当前按键按 TS 的同步语义应直接成为 rename 输入。
        // * 列表处理之后：`session_list.handle_input` 刚刚写入 pending，当前按键是“进入
        //   模式”的快捷键本身，不能再喂给输入框。
        let pending = { self.core.borrow_mut().take_pending_rename() };
        if let Some(path) = pending {
            // TS 的 `onRenameSession` 在 `sessionList.handleInput` 内**同步**进入 rename 模式；
            // 因此这次按键本身已经属于 rename 输入框，而不是被吞掉的模式切换键。
            self.enter_rename_mode(&path);
            self.handle_rename_input(data);
            return;
        }
        self.session_list.handle_input(data);
        self.sync_list_from_core();
        let pending = { self.core.borrow_mut().take_pending_rename() };
        if let Some(path) = pending {
            // 快捷键刚被列表消费；只切换模式，不把该键送入 rename 输入框。
            self.enter_rename_mode(&path);
        }
    }

    fn invalidate(&mut self) {
        // TS 继承 Container.invalidate：逐个失效子组件。
        self.session_list.invalidate();
        self.rename_input.invalidate();
    }

    fn as_focusable(&mut self) -> Option<&mut dyn Focusable> {
        Some(self)
    }
}

/// 焦点转发给列表与 rename 输入框；rename 模式下把输入框再次置为 focused
/// （与 TS 的 `set focused` 分支一致）。
impl Focusable for SessionSelectorComponent {
    fn focused(&self) -> bool {
        self.focused
    }

    fn set_focused(&mut self, focused: bool) {
        self.focused = focused;
        self.session_list.set_focused(focused);
        self.rename_input.set_focused(focused);
        if focused && self.mode == SelectorMode::Rename {
            self.rename_input.set_focused(true);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_relative_dates() {
        let now = 1_000_000_000.0;
        assert_eq!(format_session_date(now - 1_000.0, now), "now");
        assert_eq!(format_session_date(now - 120_000.0, now), "2m");
        assert_eq!(format_session_date(now - 2.0 * 3_600_000.0, now), "2h");
        assert_eq!(format_session_date(now - 10.0 * 86_400_000.0, now), "1w");
        assert_eq!(format_session_date(now - 40.0 * 86_400_000.0, now), "1mo");
        assert_eq!(format_session_date(now - 400.0 * 86_400_000.0, now), "1y");
    }

    /// Date 的 NaN/±Infinity 是 Invalid Date；模板字面量写 `NaN`，不是 JSON 的 `null`。
    #[test]
    fn formats_non_finite_dates_like_js() {
        let now = 1_000_000_000.0;
        assert_eq!(format_session_date(f64::NAN, now), "NaNy");
        assert_eq!(format_session_date(f64::INFINITY, now), "NaNy");
        assert_eq!(format_session_date(f64::NEG_INFINITY, now), "NaNy");
    }

    #[test]
    fn shortens_home_prefix() {
        if let Some(home) = dirs::home_dir() {
            let home = home.to_string_lossy().into_owned();
            assert_eq!(shorten_path(&format!("{home}/x")), "~/x");
            assert_eq!(shorten_path("/elsewhere"), "/elsewhere");
        }
    }

    /// Node `fs.unlink` 的 errno 文案（不是 Rust `io::Error` 的 Display）。
    #[cfg(unix)]
    #[test]
    fn formats_node_unlink_errors_like_node() {
        let error = std::io::Error::from_raw_os_error(libc::ENOENT);
        assert_eq!(
            node_unlink_error_message(&error, "/x"),
            "ENOENT: no such file or directory, unlink '/x'"
        );
        let error = std::io::Error::from_raw_os_error(libc::EACCES);
        assert_eq!(
            node_unlink_error_message(&error, "/y"),
            "EACCES: permission denied, unlink '/y'"
        );
        let error = std::io::Error::from_raw_os_error(libc::EISDIR);
        assert_eq!(
            node_unlink_error_message(&error, "/z"),
            "EISDIR: illegal operation on a directory, unlink '/z'"
        );
    }
}
