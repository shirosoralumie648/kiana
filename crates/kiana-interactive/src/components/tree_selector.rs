//! components/tree-selector.ts。
//!
//! `/tree` 会话树选择器：按条目父子关系构建树、折叠/展开、五种过滤模式、搜索、
//! 标签编辑与时间戳、水平视口平移、复制选中条目内容、语义化帮助行。
//!
//! TS 源：`packages/coding-agent/src/modes/interactive/components/tree-selector.ts`。
//!
//! ## 与 TS 的表示差异
//!
//! * `SessionTreeNode` 是不可移动出字段的（实现了 `Drop`，防止深树递归析构）。构造时把树
//!   展开到 `TreeArena`（`entry` 逐条 clone，`children` 用下标），之后所有导航与渲染都
//!   基于稳定下标。
//! * TS 的 `TreeList`/`TreeHelp`/`SearchLine`/`LabelInput` 是模块私有类。Rust 侧公开
//!   [`TreeList`]（测试/宿主通过 `getTreeList()` 访问）与 [`LabelInput`]（仅组件内部用，
//!   但类型出现在可见 API 上）。
//! * `TreeSelectorComponent` 的 `focused` setter **只**下发给当前 `LabelInput`，
//!   不下发给 `TreeList`（后者不因焦点改变绘制）。
//! * 空树的 `setTimeout(() => onCancel(), 100)` 改为「首次渲染/输入时若已过 100ms 再触发」，
//!   避免在没有定时器上下文的宿主里创建线程任务。
//! * label 时间戳格式化用 `chrono::Local`（JS `Date` 的本地时区语义）；无法解析时保留
//!   TS 的 Invalid Date 输出 `aN/NaN/NaN NaN:NaN`（见 `format_label_timestamp` 与测试
//!   `formats_label_timestamps_like_js_date_parsing`）。
//!
//! ## 职责边界
//!
//! 组件只持有会话树的**快照**（构造时传入 `Vec<SessionTreeNode>`），选择、折叠、过滤、
//! 搜索、标签编辑都只改组件内部状态，不写回 session 文件。需要持久化的动作通过回调交给
//! 宿主：`on_select`（确认跳转）、`on_cancel`、`on_copy`（宿主写剪贴板）、
//! `on_label_change`（宿主追加 label 条目）。实际接线见 `interactive_mode` 的
//! `show_tree_selector`（构造 [`TreeSelectorComponent`] 并设置 `on_copy`）。
//!
//! ## 数据流
//!
//! 1. 构造：`TreeArena::from_roots` 把父子树展开为带稳定下标的 arena（显式栈，避免深树
//!    递归），同时建立 `id -> 下标` 表；
//! 2. `flatten_tree`：按“当前叶所在子树优先”的顺序做前序遍历，产出 `FlatNode` 列表，并
//!    重建 `toolCallId -> {name, arguments}` 表（供 toolResult 行显示工具名/参数）；
//! 3. `build_active_path`：从 `current_leaf_id` 沿 parent 链收集活动路径，渲染时显示 `•`；
//! 4. `apply_filter`：过滤 `flat_nodes` 得到 `filtered_nodes`（先按条目类型/过滤模式/搜索词，
//!    再剔除折叠节点的后代），随后 `recalculate_visual_structure` 依据**可见节点**重算
//!    缩进/连接符/gutter；
//! 5. `render`：只渲染选中项附近 `max_visible_lines` 行，最后一行是状态行。
//!
//! 任何改变可见集合的输入（过滤模式、搜索词、折叠）都必须走 `apply_filter`，否则缩进和
//! 折叠标记会与实际可见节点不一致。
//!
//! ## 依赖的全局状态
//!
//! 渲染依赖 `kiana_theme::THEME`（未初始化时 `expect` panic，与 TS 的抛错对应）；按键匹配
//! 依赖 `kiana_tui::keybindings::get_keybindings()`（测试里是 thread-local 单例，需先
//! `set_keybindings`，见 `tests/components_tree_selector.rs` 的 `setup`）。
//!
//! ## 注释约定
//!
//! 大量辅助函数是 TS 的逐字移植：`slice_utf16`/`utf16_len` 保持 JS 的 UTF-16 code unit
//! 长度语义，`js_round`/`js_number_to_string` 保持 `Math.round` 与模板字面量的数字格式，
//! `is_js_whitespace` 对应 JS 的 `trim`/`split(/\s+/)`。这些细节影响搜索截断、行宽与
//! 标签时间戳，不能按“Rust 更自然”的方式替换。
//!
//! ## 边界与失败模式
//!
//! * 主题未初始化时 `fg`/`bg`/`bold`/`italic` 会 `expect` panic（对应 TS 在此抛错），所以
//!   第一次渲染前宿主必须初始化 `kiana_theme::THEME`。
//! * `get_keybindings()` 未显式安装时会懒初始化默认表，本身不会失败；但某个键位 id 未定义
//!   或被配置成空列表时，它不匹配任何分支，最终落入兜底分支：可打印字符被追加到搜索词，
//!   含控制字符的按键被丢弃。自定义按键绑定因此可能改变 `/tree` 的实际按键含义。
//! * 空树的自动取消没有后台定时器：只有 `handle_input`/`render` 被调用且已过 100ms 时才
//!   触发一次；若宿主不再轮询，取消回调永远不会执行。
//! * `label_timestamp` 无法按 `parse_js_date` 支持的格式解析时，渲染保留 TS Invalid Date
//!   的占位输出 `aN/NaN/NaN NaN:NaN`。

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::time::Instant;

use chrono::{Datelike, Local, NaiveDate, NaiveDateTime, SecondsFormat, TimeZone, Timelike, Utc};
use kiana_agent_core::types::AgentMessage;
use kiana_ai::types::{
    AssistantContentBlock, JsonObject, JsonValue, Message, StopReason, SystemContent, UserContent,
    UserContentBlock,
};
use kiana_json::js_num::format_js_number;
use kiana_json::js_object::stringify as js_stringify;
use kiana_json::js_string::{is_truthy, js_string};
use kiana_session_core::{SessionEntry, SessionTreeNode};
use kiana_theme::{ThemeBg, ThemeColor};
use kiana_tools::render_utils::format_read_line_range;
use kiana_tui::components::input::Input;
use kiana_tui::components::text_component::Text;
use kiana_tui::keybindings::get_keybindings;
use kiana_tui::text::{
    is_js_whitespace, slice_by_column, truncate_to_width, visible_width, wrap_text_with_ansi,
};
use kiana_tui::tui::{Component, Focusable};
use kiana_ui_kit::dynamic_border::DynamicBorder;
use kiana_ui_kit::keybinding_hints::{KeyTextFormatOptions, format_key_text, key_hint};

// ---------------------------------------------------------------------------
// 常量与主题辅助
// ---------------------------------------------------------------------------

/// 水平视口为 gutter 预留的固定宽度（不是缩进单位；每层缩进 3 字符）。
const TREE_GUTTER_WIDTH: usize = 2;
// 以下四个常量只服务于 `render_horizontal_viewport` 的“选中行锚点是否被挤出右边界”判断：
// 锚点后至少保留 `clamp(viewport/3, MIN_VISIBLE_ANCHOR_CONTENT_WIDTH,
// MAX_VISIBLE_ANCHOR_CONTENT_WIDTH)` 列正文；确认需要左移时，锚点左侧只保留
// `clamp(viewport/4, MIN_ANCHOR_CONTEXT_WIDTH, MAX_ANCHOR_CONTEXT_WIDTH)` 列上下文。
// 调整它们只改变深层节点在窄终端中的可见内容，不影响树结构或选中状态。
const MIN_VISIBLE_ANCHOR_CONTENT_WIDTH: usize = 4;
const MAX_VISIBLE_ANCHOR_CONTENT_WIDTH: usize = 20;
const MIN_ANCHOR_CONTEXT_WIDTH: usize = 2;
const MAX_ANCHOR_CONTEXT_WIDTH: usize = 12;

// 下面四个包装只在主题未初始化时 panic（`expect`），与 TS 版本抛错的位置对应。
// 宿主或测试必须先调用 `kiana_theme::init_theme`（见 tests 的 `setup`），否则首次渲染即失败。
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

fn italic(text: &str) -> String {
    kiana_theme::THEME
        .italic(text)
        .expect("theme must be initialized before rendering (TS throws here)")
}

/// JS `String.prototype.trim()`：按 JS 的空白字符集合裁掉首尾（不是 Rust 的 `char::is_whitespace`）。
fn js_trim(text: &str) -> &str {
    text.trim_matches(is_js_whitespace)
}

/// `Math.round`：`floor(x + 0.5)`（负数半值向 +∞）。
fn js_round(value: f64) -> f64 {
    if value.is_finite() {
        (value + 0.5).floor()
    } else {
        value
    }
}

/// JS `.slice(start, end)`（UTF-16 code unit）。
///
/// 用于复刻 TS 对 `String.prototype.slice` 的截断（搜索摘要 200、bash 命令 50、JSON 参数
/// 40 等）。若边界切断代理对，`String::from_utf16_lossy` 会把孤立代理项替换成 U+FFFD，
/// 而 JS 会保留孤立代理项；这只影响该行显示的最后一个字符，长度判断仍由 [`utf16_len`]
/// 在原始字符串上完成。
fn slice_utf16(text: &str, start: usize, end: usize) -> String {
    let units: Vec<u16> = text
        .encode_utf16()
        .skip(start)
        .take(end.saturating_sub(start))
        .collect();
    String::from_utf16_lossy(&units)
}

/// JS `string.length`（UTF-16 code unit）。
///
/// 对非 BMP 字符一个 Rust `char` 计 2；用于判断 TS `slice` 是否真的发生了截断（例如
/// `rawCmd.length > 50` 时补 `...`）。
fn utf16_len(text: &str) -> usize {
    text.encode_utf16().count()
}

/// `String(number)`（ECMAScript `Number::toString`）：与 [`format_js_number`]（`JSON.stringify`
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

// 下面这组 `raw_*` 是“Unknown 条目按原始 JSON 读取”的工具。`SessionEntry` 的 typed 变体
// 在字段畸形时会解析失败并退化为 `Unknown`，但 TS 仍按 `entry.type` 走原分支；这些函数
// 保证同样的字段在 Rust 侧仍可读到，缺失键返回 None，由调用方决定 JS 的 undefined/null
// 语义（例如 `raw_str_or_undefined` 把缺失渲染成 `"undefined"`）。

/// 未解析成 typed 变体的 [`SessionEntry::Unknown`] 的原始 JSON 对象。
fn raw_entry_object(entry: &SessionEntry) -> Option<&JsonObject> {
    match entry {
        SessionEntry::Unknown(object) => Some(object),
        _ => None,
    }
}

/// 读取 Unknown 条目的原始字段（字符串）。
fn raw_str<'a>(entry: &'a SessionEntry, key: &str) -> Option<&'a str> {
    raw_entry_object(entry)?
        .get(key)
        .and_then(JsonValue::as_str)
}

/// 读取 Unknown 条目的原始字段。
fn raw_value<'a>(entry: &'a SessionEntry, key: &str) -> Option<&'a JsonValue> {
    raw_entry_object(entry)?.get(key)
}

/// `entry.replacement === null` 的精确语义。
///
/// TS 里「缺少 `replacement` 键」是 `undefined`，与显式 `null` 不同；typed 视图把两者都
/// 变成 `None`，所以优先从加载时保留的原始对象判断。
fn context_edit_omits(entry: &SessionEntry) -> bool {
    match entry {
        SessionEntry::ContextEdit(entry) => match &entry.raw_entry {
            Some(raw) => matches!(raw.get("replacement"), Some(JsonValue::Null)),
            None => entry.replacement.is_none(),
        },
        _ => matches!(raw_value(entry, "replacement"), Some(JsonValue::Null)),
    }
}

/// `Math.round(entry.tokensBefore / 1000)` 的输入；缺失键在 JS 里是 `undefined`（NaN）。
fn compaction_tokens_before(entry: &SessionEntry) -> f64 {
    match entry {
        SessionEntry::Compaction(entry) => match &entry.raw_entry {
            Some(raw) if !raw.contains_key("tokensBefore") => f64::NAN,
            _ => entry.tokens_before,
        },
        _ => raw_value(entry, "tokensBefore")
            .and_then(JsonValue::as_f64)
            .unwrap_or(f64::NAN),
    }
}

/// 读取 Unknown 条目里 `message` 对象的字段。
fn raw_message_value<'a>(entry: &'a SessionEntry, key: &str) -> Option<&'a JsonValue> {
    match raw_entry_object(entry)?.get("message") {
        Some(JsonValue::Object(message)) => message.get(key),
        _ => None,
    }
}

/// JS 的 `"content" in entry.message`。
///
/// 只判断键是否存在，值为 `null` 也算存在（与 `in` 运算符一致）；这是复制逻辑里区分
/// “无 content 字段”和“content 为空”的依据。
fn raw_message_has_content(entry: &SessionEntry) -> bool {
    matches!(
        raw_entry_object(entry).and_then(|object| object.get("message")),
        Some(JsonValue::Object(message)) if message.contains_key("content")
    )
}

/// 模板字面量里的缺失字段：`${undefined}` -> `"undefined"`。
fn raw_str_or_undefined<'a>(entry: &'a SessionEntry, key: &str) -> &'a str {
    raw_str(entry, key).unwrap_or("undefined")
}

// ---------------------------------------------------------------------------
// 过滤模式
// ---------------------------------------------------------------------------

/// 树过滤模式（TS `FilterMode`）。
///
/// 过滤作用在扁平化后的节点列表上（`TreeList` 的 `apply_filter`），与树的父子结构无关：
/// 被隐藏的中间节点不会打断其可见后代的祖先链，`recalculate_visual_structure` 会把后代
/// 挂到最近的可见祖先下。`usage` 条目在所有模式下都会在模式判断之前被单独排除。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FilterMode {
    /// 隐藏设置类/簿记条目。
    ///
    /// 具体为 `label`、`context_edit`、`custom`、`model_change`、
    /// `thinking_level_change`、`session_info`（按 `SessionEntry::kind()` 字符串判断，
    /// 因此退化成 `Unknown` 的已知类型同样被隐藏）。
    Default,
    /// 默认模式再去掉 toolResult。
    ///
    /// 设置类条目同样隐藏。
    NoTools,
    /// 仅用户消息。
    ///
    /// 只匹配 `message` 条目中 role 为 `user` 的项，其余类型一律不显示。
    UserOnly,
    /// 仅有标签的条目。
    ///
    /// 判断依据是节点上解析后的 label 是否为 `Some`；空串 label 也算“有标签”。
    LabeledOnly,
    /// 全部条目。
    ///
    /// `usage` 条目仍在模式判断前被排除。
    All,
}

impl FilterMode {
    /// TS 的模式字符串（toggle 循环顺序用）。
    ///
    /// 返回值就是 TS `FilterMode` 的五个字面量；实际循环切换顺序由 [`FilterMode::CYCLE`]
    /// 决定，不经过本方法。
    pub fn as_str(self) -> &'static str {
        match self {
            FilterMode::Default => "default",
            FilterMode::NoTools => "no-tools",
            FilterMode::UserOnly => "user-only",
            FilterMode::LabeledOnly => "labeled-only",
            FilterMode::All => "all",
        }
    }

    /// `["default","no-tools","user-only","labeled-only","all"]`。
    ///
    /// 数组顺序即 `cycleForward`/`cycleBackward` 的循环顺序（前进 `+1`、后退 `-1`，
    /// 按长度取模回绕）。
    pub const CYCLE: [FilterMode; 5] = [
        FilterMode::Default,
        FilterMode::NoTools,
        FilterMode::UserOnly,
        FilterMode::LabeledOnly,
        FilterMode::All,
    ];
}

// ---------------------------------------------------------------------------
// 树数据（arena）
// ---------------------------------------------------------------------------

/// 一个树节点的稳定存储。
///
/// `parent`/`children` 是 arena 下标而不是引用：`TreeArena` 构建完成后不再增删节点，
/// 因此下标可以安全地作为 `FlatNode`、可见树映射与选中状态的稳定键。
struct ArenaNode {
    entry: SessionEntry,
    label: Option<String>,
    label_timestamp: Option<String>,
    parent: Option<usize>,
    children: Vec<usize>,
}

/// 扁平化后的树（TS `SessionTreeNode[]` 的 Rust 表示）。
///
/// `nodes` 按 `from_roots` 的先序顺序存放；`roots` 保持输入顺序；`id_to_node` 用
/// `entry.id()` 建立索引，供按 id 向上找父节点（活动路径、选择恢复、可见树重算）使用。
struct TreeArena {
    nodes: Vec<ArenaNode>,
    roots: Vec<usize>,
    id_to_node: HashMap<String, usize>,
}

impl TreeArena {
    /// 把递归的 `SessionTreeNode` 树展开到 arena。
    ///
    /// 用显式栈而非递归：会话树可能很深，而 `SessionTreeNode` 的 `Drop` 也被刻意实现成
    /// 迭代式（见 `kiana-session-core` 的同名结构）。压栈时 `roots` 与每个节点的
    /// `children` 都逆序压入，弹出顺序即前序（父在子前、兄弟保持原顺序）；每个节点记录
    /// 父下标，根的下标按输入顺序进入 `roots`。`entry`/`label` 是 clone 出来的副本，
    /// 后续操作（包括标签编辑）都只改 arena，不写回 session。
    fn from_roots(roots: Vec<SessionTreeNode>) -> Self {
        let mut arena = TreeArena {
            nodes: Vec::new(),
            roots: Vec::new(),
            id_to_node: HashMap::new(),
        };
        let mut stack: Vec<(SessionTreeNode, Option<usize>)> =
            roots.into_iter().rev().map(|node| (node, None)).collect();
        while let Some((mut node, parent)) = stack.pop() {
            let children = std::mem::take(&mut node.children);
            let index = arena.nodes.len();
            arena.nodes.push(ArenaNode {
                entry: node.entry.clone(),
                label: node.label.clone(),
                label_timestamp: node.label_timestamp.clone(),
                parent,
                children: Vec::new(),
            });
            arena
                .id_to_node
                .insert(arena.nodes[index].entry.id().to_string(), index);
            match parent {
                Some(parent_index) => arena.nodes[parent_index].children.push(index),
                None => arena.roots.push(index),
            }
            for child in children.into_iter().rev() {
                stack.push((child, Some(index)));
            }
        }
        arena
    }

    fn entry_id(&self, index: usize) -> &str {
        self.nodes[index].entry.id()
    }
}

/// 一个 gutter 段：在某显示层显示 `│` 还是空格。
///
/// gutter 记录祖先的竖向连接线：渲染时每层占 3 列，`position` 层的第 0 列按 `show`
/// 输出 `│` 或空格，其余两列是空格。`flatten_tree`/`recalculate_visual_structure` 在
/// 节点要画连接符时（`show_connector` 且不是虚拟根的直连根），把该层记录压入子节点的
/// gutter 列表；`show` 取决于该节点自身是否是其父节点的最后一个子节点——是 `└` 时记录
/// 仍会压入，只是 `show` 为 false，用来抑制子节点在该层画出自己的连接符。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GutterInfo {
    /// 显示层（displayIndent 层级）。
    ///
    /// 从 0 起算；多根模式下根节点的显示层是 0（`indent` 为 1，渲染时减去虚拟根那一层）。
    pub position: usize,
    /// 是否显示 `│`。
    ///
    /// 祖先不是其父节点的最后一个子节点时为 `true`（竖线要继续画到后续兄弟）。
    pub show: bool,
}

/// 扁平化节点（TS `FlatNode`）。
///
/// `node` 是 arena 下标；其余字段都是渲染几何信息，会在 `recalculate_visual_structure`
/// 里按**可见树**重写（`flatten_tree` 给出的是全量树的初值）。
/// `indent` 以层为单位、每层 3 列；`show_connector`/`is_last` 决定 `├─`/`└─`；
/// `is_virtual_root_child` 表示多根模式下由虚拟根挂下来的根节点（自身不画连接符，
/// 但它的子节点会多缩进一层）。
#[derive(Debug, Clone, PartialEq)]
struct FlatNode {
    node: usize,
    /// 每层 3 字符。
    indent: usize,
    show_connector: bool,
    is_last: bool,
    gutters: Vec<GutterInfo>,
    is_virtual_root_child: bool,
}

/// 无参数回调（TS `() => void`）。
pub type VoidCallback = Box<dyn FnMut()>;
/// 单字符串参数回调（TS `(value: string) => void`）。
pub type StringCallback = Box<dyn FnMut(&str)>;
/// 可选字符串参数回调（TS `(text: string | undefined) => void`）。
///
/// 目前用于复制：参数来自 `TreeList::copy_selected`，`None` 表示选中条目没有可复制文本
/// （没有选中项，或内容 `js_trim` 后为空）；是否写剪贴板由宿主决定。
pub type OptionalStringCallback = Box<dyn FnMut(Option<&str>)>;
/// 标签编辑回调（TS `(entryId, currentLabel) => void`）。
///
/// 同一个别名有两种用途，第二个参数含义不同：`TreeList::on_label_edit` 收到的是**当前**
/// label（打开编辑器前的值），而 `TreeSelectorComponent` 构造参数 `on_label_change` 收到
/// 的是**提交后**的新 label（空串已转成 `None`）。
pub type LabelEditCallback = Box<dyn FnMut(&str, Option<&str>)>;

/// `toolCallId -> {name, arguments}` 的查找表条目。
///
/// 由 `flatten_tree` 从全量树的 assistant 消息重建；`message_display_text` 渲染
/// toolResult 行时按 `toolCallId` 取出，交给 `format_tool_call` 生成 `[read: ...]` 形式
/// 的显示文本。工具调用条目即使随后被当前过滤模式隐藏，映射仍然保留。
#[derive(Debug, Clone)]
struct ToolCallInfo {
    name: String,
    arguments: JsonObject,
}

/// 选中节点的只读视图。
///
/// 借用自 `TreeList` 内部的 arena，生命周期跟随 `&TreeList`；没有可见选中项时
/// `get_selected_node` 返回 `None`。`label`/`label_timestamp` 来自构建树时的
/// `SessionTreeNode` 字段（不是 `entry` 自身的字段）；`label_timestamp` 是原始字符串，
/// 可能无法解析成日期（此时渲染走 Invalid Date 分支）。
#[derive(Debug, Clone, Copy)]
pub struct TreeListNodeRef<'a> {
    /// 条目。
    pub entry: &'a SessionEntry,
    /// 标签。
    pub label: Option<&'a str>,
    /// 标签时间戳。
    pub label_timestamp: Option<&'a str>,
}

// ---------------------------------------------------------------------------
// 条目内容辅助
// ---------------------------------------------------------------------------

// TS 侧这些 helper 直接读 `entry.message.content` 等动态字段；Rust 侧同一条目可能是
// typed 变体，也可能因字段畸形退化成 `SessionEntry::Unknown`。两套读取路径必须给出相同
// 结果，否则同一棵树里“解析成功/失败”的条目会在显示、搜索、复制上出现差异：
// `*_message_*` 处理 typed/已知消息，`raw_*` 处理原始 JSON。

/// JSON 内容块里的文本块拼接（`extractFullContent` 对非字符串的版本）。
///
/// 只拼接 `type == "text"` 的块；图片、thinking 等块被忽略（与 TS 相同）。非字符串/数组
/// 输入返回空串。
fn extract_full_content_json(content: &JsonValue) -> String {
    match content {
        JsonValue::String(text) => text.clone(),
        JsonValue::Array(blocks) => {
            let mut result = String::new();
            for block in blocks {
                if let JsonValue::Object(object) = block
                    && object.get("type").and_then(JsonValue::as_str) == Some("text")
                    && let Some(text) = object.get("text").and_then(JsonValue::as_str)
                {
                    result.push_str(text);
                }
            }
            result
        }
        _ => String::new(),
    }
}

/// `extractFullContent(content)`（消息里的内容）。
///
/// Standard 各变体只取文本块（用户/工具结果的图片块、assistant 的工具调用/thinking 块
/// 都被跳过）；自定义消息读 `content` 字段并交给 [`extract_full_content_json`]。
fn extract_full_content_message(message: &AgentMessage) -> String {
    match message {
        AgentMessage::Standard(Message::System(message)) => match &message.content {
            SystemContent::Text(text) => text.clone(),
            SystemContent::Blocks(blocks) => {
                blocks.iter().map(|block| block.text.as_str()).collect()
            }
        },
        AgentMessage::Standard(Message::User(message)) => match &message.content {
            UserContent::Text(text) => text.clone(),
            UserContent::Blocks(blocks) => blocks
                .iter()
                .map(|block| match block {
                    UserContentBlock::Text(text) => text.text.as_str(),
                    UserContentBlock::Image(_) => "",
                })
                .collect(),
        },
        AgentMessage::Standard(Message::Assistant(message)) => message
            .content
            .iter()
            .map(|block| match block {
                AssistantContentBlock::Text(text) => text.text.as_str(),
                _ => "",
            })
            .collect(),
        AgentMessage::Standard(Message::ToolResult(message)) => message
            .content
            .iter()
            .map(|block| match block {
                UserContentBlock::Text(text) => text.text.as_str(),
                UserContentBlock::Image(_) => "",
            })
            .collect(),
        AgentMessage::Custom(object) => match object.get("content") {
            Some(content) => extract_full_content_json(content),
            None => String::new(),
        },
    }
}

/// `extractContent(content) = extractFullContent(content).slice(0, 200)`。
fn extract_content_message(message: &AgentMessage) -> String {
    slice_utf16(&extract_full_content_message(message), 0, 200)
}

/// `normalize(s) = s.replace(/[\n\t]/g, " ").trim()`。
fn normalize_display(text: &str) -> String {
    text.replace(['\n', '\t'], " ")
        .trim_matches(is_js_whitespace)
        .to_string()
}

/// `hasTextContent(content)`（消息）。
///
/// 判断是否存在 `js_trim` 后非空的文本块；图片块不算文本。用于过滤“只有工具调用的
/// assistant 轮次”（见 `apply_filter`）。
fn has_text_content(message: &AgentMessage) -> bool {
    match message {
        AgentMessage::Standard(Message::System(message)) => match &message.content {
            SystemContent::Text(text) => !js_trim(text).is_empty(),
            SystemContent::Blocks(blocks) => {
                blocks.iter().any(|block| !js_trim(&block.text).is_empty())
            }
        },
        AgentMessage::Standard(Message::User(message)) => match &message.content {
            UserContent::Text(text) => !js_trim(text).is_empty(),
            UserContent::Blocks(blocks) => blocks.iter().any(|block| match block {
                UserContentBlock::Text(text) => !js_trim(&text.text).is_empty(),
                UserContentBlock::Image(_) => false,
            }),
        },
        AgentMessage::Standard(Message::Assistant(message)) => {
            message.content.iter().any(|block| match block {
                AssistantContentBlock::Text(text) => !js_trim(&text.text).is_empty(),
                _ => false,
            })
        }
        AgentMessage::Standard(Message::ToolResult(message)) => {
            message.content.iter().any(|block| match block {
                UserContentBlock::Text(text) => !js_trim(&text.text).is_empty(),
                UserContentBlock::Image(_) => false,
            })
        }
        AgentMessage::Custom(object) => match object.get("content") {
            Some(JsonValue::String(text)) => !js_trim(text).is_empty(),
            Some(JsonValue::Array(blocks)) => blocks.iter().any(|block| {
                if let JsonValue::Object(object) = block
                    && let Some(text) = object.get("text").and_then(JsonValue::as_str)
                {
                    !js_trim(text).is_empty()
                } else {
                    false
                }
            }),
            _ => false,
        },
    }
}

/// 消息的 `stopReason` 字符串（仅 assistant / 自定义消息）。
///
/// 把 `StopReason` 枚举映射成 TS 的字面量（`toolUse`、`aborted` 等）；自定义消息直接读
/// 原始 `stopReason` 字段。其它角色返回 `None`（JS 里是 `undefined`）。
fn message_stop_reason(message: &AgentMessage) -> Option<&str> {
    match message {
        AgentMessage::Standard(Message::Assistant(assistant)) => {
            Some(match assistant.stop_reason {
                StopReason::Pending => "pending",
                StopReason::Stop => "stop",
                StopReason::Length => "length",
                StopReason::ToolUse => "toolUse",
                StopReason::Error => "error",
                StopReason::Aborted => "aborted",
                StopReason::Deferred => "deferred",
                StopReason::TurnCapped => "turnCapped",
                StopReason::TokenCapped => "tokenCapped",
                StopReason::CostCapped => "costCapped",
                StopReason::TimeCapped => "timeCapped",
            })
        }
        AgentMessage::Custom(object) => object.get("stopReason").and_then(JsonValue::as_str),
        _ => None,
    }
}

/// 消息的 `errorMessage`。
fn message_error_message(message: &AgentMessage) -> Option<&str> {
    match message {
        AgentMessage::Standard(Message::Assistant(assistant)) => assistant.error_message.as_deref(),
        AgentMessage::Custom(object) => object.get("errorMessage").and_then(JsonValue::as_str),
        _ => None,
    }
}

/// 消息的 `command`（bashExecution）。
fn message_command(message: &AgentMessage) -> Option<&str> {
    match message {
        AgentMessage::Custom(object) => object.get("command").and_then(JsonValue::as_str),
        _ => None,
    }
}

/// toolResult 的 `toolCallId`。
fn message_tool_call_id(message: &AgentMessage) -> Option<&str> {
    match message {
        AgentMessage::Standard(Message::ToolResult(result)) => Some(&result.tool_call_id),
        AgentMessage::Custom(object) => object.get("toolCallId").and_then(JsonValue::as_str),
        _ => None,
    }
}

/// toolResult 的 `toolName`。
fn message_tool_name(message: &AgentMessage) -> Option<&str> {
    match message {
        AgentMessage::Standard(Message::ToolResult(result)) => Some(&result.tool_name),
        AgentMessage::Custom(object) => object.get("toolName").and_then(JsonValue::as_str),
        _ => None,
    }
}

/// 消息里的 toolCall 块。
///
/// 只收集 assistant 的 `ToolCall` 块（text/thinking 块忽略），返回顺序与块顺序一致，
/// 每项是 `(id, name, arguments)`。`flatten_tree` 用它填充 `tool_call_map`。
fn message_tool_calls(message: &AgentMessage) -> Vec<(String, String, JsonObject)> {
    let mut calls = Vec::new();
    match message {
        AgentMessage::Standard(Message::Assistant(assistant)) => {
            for block in &assistant.content {
                if let AssistantContentBlock::ToolCall(call) = block {
                    calls.push((call.id.clone(), call.name.clone(), call.arguments.clone()));
                }
            }
        }
        AgentMessage::Custom(object) => {
            if let Some(JsonValue::Array(blocks)) = object.get("content") {
                for block in blocks {
                    if let JsonValue::Object(block) = block
                        && block.get("type").and_then(JsonValue::as_str) == Some("toolCall")
                        && let (Some(id), Some(name)) = (
                            block.get("id").and_then(JsonValue::as_str),
                            block.get("name").and_then(JsonValue::as_str),
                        )
                    {
                        let arguments: JsonObject = match block.get("arguments") {
                            Some(JsonValue::Object(arguments)) => arguments
                                .iter()
                                .map(|(key, value)| (key.clone(), value.clone()))
                                .collect(),
                            _ => JsonObject::new(),
                        };
                        calls.push((id.to_string(), name.to_string(), arguments));
                    }
                }
            }
        }
        _ => {}
    }
    calls
}

/// Unknown message 条目里的 toolCall 块（`message.content` 原始 JSON）。
///
/// `flatten_tree` 在 typed 消息缺失（`entry.message()` 为 `None`）时改走这里，保证退化成
/// `Unknown` 的 assistant 条目也能填充 `tool_call_map`。
fn raw_message_tool_calls(entry: &SessionEntry) -> Vec<(String, String, JsonObject)> {
    let Some(JsonValue::Array(blocks)) = raw_message_value(entry, "content") else {
        return Vec::new();
    };
    let mut calls = Vec::new();
    for block in blocks {
        if let JsonValue::Object(block) = block
            && block.get("type").and_then(JsonValue::as_str) == Some("toolCall")
            && let (Some(id), Some(name)) = (
                block.get("id").and_then(JsonValue::as_str),
                block.get("name").and_then(JsonValue::as_str),
            )
        {
            let arguments: JsonObject = match block.get("arguments") {
                Some(JsonValue::Object(arguments)) => arguments
                    .iter()
                    .map(|(key, value)| (key.clone(), value.clone()))
                    .collect(),
                _ => JsonObject::new(),
            };
            calls.push((id.to_string(), name.to_string(), arguments));
        }
    }
    calls
}

/// 消息的 `content` 在 JS 里是否 truthy（用于 `getSearchableText` 的
/// `"content" in msg && msg.content`）。
///
/// TS 随后调用 `extractContent`（= `extractFullContent(...).slice(0, 200)`），所以这里也
/// 截断到 200 个 UTF-16 code unit；否则只在 200 之后出现的匹配会被错误保留。
fn message_content_for_search(message: &AgentMessage) -> Option<String> {
    match message {
        AgentMessage::Standard(Message::System(system)) => match &system.content {
            SystemContent::Text(text) if text.is_empty() => None,
            _ => Some(extract_content_message(message)),
        },
        AgentMessage::Standard(Message::User(user)) => match &user.content {
            UserContent::Text(text) if text.is_empty() => None,
            _ => Some(extract_content_message(message)),
        },
        AgentMessage::Standard(Message::Assistant(_))
        | AgentMessage::Standard(Message::ToolResult(_)) => Some(extract_content_message(message)),
        AgentMessage::Custom(object) => match object.get("content") {
            Some(content) if is_truthy(Some(content)) => Some(extract_content_json(content)),
            _ => None,
        },
    }
}

/// `extractContent(content)` for raw JSON（`extractFullContent(...).slice(0, 200)`）。
fn extract_content_json(content: &JsonValue) -> String {
    slice_utf16(&extract_full_content_json(content), 0, 200)
}

/// `entry.message.role`：typed 变体直接读，Unknown 的 `message.role` 读原始 JSON。
fn entry_message_role(entry: &SessionEntry) -> Option<&str> {
    if entry.kind() != "message" {
        return None;
    }
    entry
        .message()
        .map(|message| message.role())
        .or_else(|| raw_message_value(entry, "role").and_then(JsonValue::as_str))
}

/// `entry.message.content` 的 truthiness + `extractContent`（200 units）。
fn entry_message_content_for_search(entry: &SessionEntry) -> Option<String> {
    if let Some(message) = entry.message() {
        return message_content_for_search(message);
    }
    let content = raw_message_value(entry, "content")?;
    if !is_truthy(Some(content)) {
        return None;
    }
    Some(extract_content_json(content))
}

/// `hasTextContent(entry.message.content)`（typed 或 Unknown 原始 JSON）。
fn entry_message_has_text(entry: &SessionEntry) -> bool {
    if let Some(message) = entry.message() {
        return has_text_content(message);
    }
    has_text_content_json(raw_message_value(entry, "content"))
}

/// `hasTextContent(content)` 的 JSON 版本。
fn has_text_content_json(content: Option<&JsonValue>) -> bool {
    match content {
        Some(JsonValue::String(text)) => !js_trim(text).is_empty(),
        Some(JsonValue::Array(blocks)) => blocks.iter().any(|block| {
            if let JsonValue::Object(object) = block
                && let Some(text) = object.get("text").and_then(JsonValue::as_str)
            {
                !js_trim(text).is_empty()
            } else {
                false
            }
        }),
        _ => false,
    }
}

/// `entry.message.stopReason`（typed 或 Unknown 原始 JSON）。
fn entry_message_stop_reason(entry: &SessionEntry) -> Option<&str> {
    if let Some(message) = entry.message() {
        return message_stop_reason(message);
    }
    raw_message_value(entry, "stopReason").and_then(JsonValue::as_str)
}

/// `entry.message.errorMessage`（typed 或 Unknown 原始 JSON）。
fn entry_message_error_message(entry: &SessionEntry) -> Option<&str> {
    if let Some(message) = entry.message() {
        return message_error_message(message);
    }
    raw_message_value(entry, "errorMessage").and_then(JsonValue::as_str)
}

/// `entry.message.command`（typed 或 Unknown 原始 JSON）。
fn entry_message_command(entry: &SessionEntry) -> Option<&str> {
    if let Some(message) = entry.message() {
        return message_command(message);
    }
    raw_message_value(entry, "command").and_then(JsonValue::as_str)
}

/// 条目的消息（仅 `type: "message"`）。
fn entry_message(entry: &SessionEntry) -> Option<&AgentMessage> {
    entry.message()
}

// ---------------------------------------------------------------------------
// TreeList
// ---------------------------------------------------------------------------

/// `handleInput` 产生、由 [`TreeSelectorComponent`] 分发的动作。
///
/// `TreeList::handle_input_action` 只改自身状态并返回该动作，不直接调用回调；这样组件层
/// 可以决定回调优先级（见 [`TreeSelectorComponent`] 的 `handle_input`），单独使用
/// [`TreeList`] 时则由其 `Component::handle_input` 调用自身回调字段。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TreeAction {
    /// 无动作。
    None,
    /// 选中条目。
    ///
    /// 仅当确认键按下时存在可见选中项才产生；否则按键被吞掉，仍返回 `None`。
    Select(String),
    /// 取消。
    ///
    /// 搜索词为空时按取消键才产生；有搜索词时第一下只清空搜索与折叠，不取消。
    Cancel,
    /// 复制选中内容（`None` 表示无内容）。
    ///
    /// 即使没有可复制文本也会返回动作：组件不吞掉按键，由宿主决定是否提示错误。
    Copy(Option<String>),
    /// 编辑条目标签。
    ///
    /// 参数是条目 id 与当前 label；仅在存在可见选中项时产生。
    EditLabel(String, Option<String>),
}

/// 树列表组件（TS `TreeList`，模块私有；Rust 侧公开供测试/宿主访问）。
///
/// 它同时是 `/tree` 的视图模型与输入处理器：`arena` 保存全量树，`flat_nodes` 是渲染前序
/// 遍历的缓存，`filtered_nodes` 是当前可见集合。公开方法是给宿主/测试的读取与编程式修改
/// 入口；交互式输入走 `handle_input_action`。
///
/// 不变量：`filtered_nodes` 的每个元素都是 `flat_nodes` 的合法下标，顺序与全量前序一致；
/// `visible_*` 映射与 `filtered_nodes` 在 `apply_filter` 末尾一起更新。
pub struct TreeList {
    arena: TreeArena,
    /// 全量树的前序缓存；每个节点的 entry 在 arena 中只存一份。
    flat_nodes: Vec<FlatNode>,
    /// 当前可见节点（`flat_nodes` 下标，保持过滤后的顺序）。
    filtered_nodes: Vec<usize>,
    /// 选中项在 `filtered_nodes` 中的下标。
    ///
    /// 用 `isize` 而不是 `usize`：空列表上按上下键可能得到负值，负值一律表示“没有选中
    /// 项”，所有访问器（`filtered_index`）都先检查 `< 0`，不会用负下标索引。
    selected_index: isize,
    /// 构造时传入的当前叶 id；活动路径标记与“当前叶不隐藏”的过滤规则都依赖它。
    current_leaf_id: Option<String>,
    /// 渲染窗口高度，同时也是翻页步长（cursorLeft/Right 与 pageUp/Down）。
    max_visible_lines: usize,
    filter_mode: FilterMode,
    /// 搜索词原文；分词时统一小写，多个 token 之间是 AND 关系（见 `apply_filter`）。
    search_query: String,
    /// `toolCallId -> {name, arguments}`；由 `flatten_tree` 从全量树的 assistant 消息建立，
    /// 供 toolResult 行显示工具名与参数。
    tool_call_map: HashMap<String, ToolCallInfo>,
    /// 当前**可见**树是否有多个根。`recalculate_visual_structure` 会按可见根重写，
    /// 决定根层是否使用虚拟根缩进与连接符。
    multiple_roots: bool,
    /// 是否在 label 后显示时间戳（`app.tree.toggleLabelTimestamp` 切换）。
    show_label_timestamps: bool,
    /// 当前叶到根的条目 id 集合，渲染时在行首显示 `•`。
    active_path_ids: HashSet<String>,
    /// 条目 id -> 最近的可见祖先（可见根对应 `None`）。
    visible_parent_map: HashMap<String, Option<String>>,
    /// 可见父 id -> 可见子 id 列表（顺序与 `filtered_nodes` 一致）。
    visible_children_map: HashMap<String, Vec<String>>,
    /// 可见根 id 列表（顺序与 `filtered_nodes` 一致）。
    visible_roots: Vec<String>,
    /// 上一次有效选择的 id；`apply_filter` 用它把选择恢复到最近可见祖先，从而在切换空
    /// 过滤器再切回时保留位置。
    last_selected_id: Option<String>,
    /// 已折叠节点的条目 id 集合；任何过滤模式或搜索词变化都会清空（旧 id 在新结果里未必存在）。
    folded_nodes: HashSet<String>,
    /// 选中回调（独立使用 `TreeList` 时生效；组件自己分发动作）。
    pub on_select: Option<StringCallback>,
    /// 取消回调。
    pub on_cancel: Option<VoidCallback>,
    /// 复制回调。
    pub on_copy: Option<OptionalStringCallback>,
    /// 标签编辑回调。
    pub on_label_edit: Option<LabelEditCallback>,
}

impl TreeList {
    /// `new(tree, currentLeafId, maxVisibleLines, initialSelectedId?, initialFilterMode?)`。
    ///
    /// `tree` 是会话树快照（渲染顺序会把含当前叶的根排前，但 `arena.roots` 不被修改）。
    /// `max_visible_lines` 同时是渲染窗口高度与翻页步长；[`TreeSelectorComponent`] 传入的是
    /// `max(terminal_height / 2, 5)`。构造流程：arena 展开 → 计算活动路径 → 应用过滤 →
    /// 以 `initial_selected_id`（缺省 `current_leaf_id`）为起点寻找最近的可见项。
    /// 若目标及其祖先都不可见，选中最后一个可见项；`filtered_nodes` 为空时下标为 0。
    /// `initial_filter_mode` 缺省为 [`FilterMode::Default`]。
    pub fn new(
        tree: Vec<SessionTreeNode>,
        current_leaf_id: Option<String>,
        max_visible_lines: usize,
        initial_selected_id: Option<String>,
        initial_filter_mode: Option<FilterMode>,
    ) -> Self {
        let multiple_roots = tree.len() > 1;
        let arena = TreeArena::from_roots(tree);
        let mut list = TreeList {
            arena,
            flat_nodes: Vec::new(),
            filtered_nodes: Vec::new(),
            selected_index: 0,
            current_leaf_id,
            max_visible_lines,
            filter_mode: initial_filter_mode.unwrap_or(FilterMode::Default),
            search_query: String::new(),
            tool_call_map: HashMap::new(),
            multiple_roots,
            show_label_timestamps: false,
            active_path_ids: HashSet::new(),
            visible_parent_map: HashMap::new(),
            visible_children_map: HashMap::new(),
            visible_roots: Vec::new(),
            last_selected_id: None,
            folded_nodes: HashSet::new(),
            on_select: None,
            on_cancel: None,
            on_copy: None,
            on_label_edit: None,
        };
        list.multiple_roots = multiple_roots;
        list.flat_nodes = list.flatten_tree();
        list.build_active_path();
        list.apply_filter();
        let target_id = initial_selected_id.or_else(|| list.current_leaf_id.clone());
        list.selected_index = list.find_nearest_visible_index(target_id.as_deref());
        list.last_selected_id = list.selected_entry_id().map(str::to_string);
        list
    }

    /// `getSearchQuery()`。
    ///
    /// 返回原始大小写与空白的搜索词；空串表示未搜索（匹配时才做小写分词）。
    pub fn get_search_query(&self) -> &str {
        &self.search_query
    }

    /// `getSelectedNode()`。
    ///
    /// 列表为空、下标为负或越界时返回 `None`（不会 panic）。
    pub fn get_selected_node(&self) -> Option<TreeListNodeRef<'_>> {
        let flat_index = self.selected_flat_index()?;
        let node = &self.arena.nodes[self.flat_nodes[flat_index].node];
        Some(TreeListNodeRef {
            entry: &node.entry,
            label: node.label.as_deref(),
            label_timestamp: node.label_timestamp.as_deref(),
        })
    }

    /// 当前选中条目的 id。
    ///
    /// 与 [`TreeList::get_selected_node`] 使用同一套可见性判断；无可见选中项时为 `None`。
    pub fn get_selected_entry_id(&self) -> Option<&str> {
        self.selected_entry_id()
    }

    /// 当前过滤模式。
    ///
    /// 构造后由输入切换；只影响可见集合，不改变全量树。
    pub fn get_filter_mode(&self) -> FilterMode {
        self.filter_mode
    }

    /// 选中项在 `filtered_nodes` 里的下标（安全版）。
    fn filtered_index(&self) -> Option<usize> {
        if self.selected_index < 0 {
            return None;
        }
        let index = self.selected_index as usize;
        (index < self.filtered_nodes.len()).then_some(index)
    }

    fn selected_flat_index(&self) -> Option<usize> {
        self.filtered_index()
            .map(|index| self.filtered_nodes[index])
    }

    fn selected_entry_id(&self) -> Option<&str> {
        self.selected_flat_index()
            .map(|flat| self.arena.entry_id(self.flat_nodes[flat].node))
    }

    /// `copySelected()`：返回待复制文本（宿主负责写剪贴板）。
    ///
    /// 文本是完整内容（不截断）；`bashExecution` 只复制命令，assistant 没有正文时回退到
    /// `errorMessage`，custom_message 复制其 content，compaction/branch_summary 复制
    /// summary，其余类型不产生文本。结果最后再按 `js_trim` 判一次空：全空白内容也返回
    /// `None`。
    pub fn copy_selected(&self) -> Option<String> {
        self.get_selected_node()
            .and_then(|node| self.get_entry_copy_text(&node))
    }

    /// `updateNodeLabel(entryId, label, labelTimestamp?)`。
    ///
    /// 只修改本地 arena 中**第一个** id 匹配的节点。`label` 为 `None` 或空串时清除标签
    /// 时间戳；非空时使用传入的 `label_timestamp`，缺省取当前 UTC 时间（`toISOString`
    /// 形态，毫秒精度 + `Z`）。注意空串仍会写进 `label` 字段（渲染层再按“非空”决定是否
    /// 显示），但时间戳为 `None`。本方法不写回 session；持久化由宿主的标签回调完成
    /// （见 [`TreeSelectorComponent`] 的构造参数 `on_label_change`）。
    pub fn update_node_label(
        &mut self,
        entry_id: &str,
        label: Option<String>,
        label_timestamp: Option<String>,
    ) {
        for flat_node in &self.flat_nodes {
            let node = &mut self.arena.nodes[flat_node.node];
            if node.entry.id() == entry_id {
                let has_label = label.as_deref().is_some_and(|label| !label.is_empty());
                node.label = label;
                node.label_timestamp = if has_label {
                    Some(label_timestamp.unwrap_or_else(now_iso_string))
                } else {
                    None
                };
                break;
            }
        }
    }

    /// `buildActivePath()`：从当前叶到根的条目 id 集合。
    ///
    /// 沿 `current_leaf_id` 的 arena 父链向上走，遇到不在 `id_to_node` 里的 id 就停止；
    /// 因此即使传入的 leaf id 不在树中，它本身仍会先被插入集合（对渲染无害：只有树中
    /// 存在的 id 才会被比较到）。
    fn build_active_path(&mut self) {
        self.active_path_ids.clear();
        let Some(mut current_id) = self.current_leaf_id.clone() else {
            return;
        };
        loop {
            self.active_path_ids.insert(current_id.clone());
            let Some(index) = self.arena.id_to_node.get(&current_id).copied() else {
                break;
            };
            let Some(parent) = self.arena.nodes[index].parent else {
                break;
            };
            current_id = self.arena.entry_id(parent).to_string();
        }
    }

    /// `flattenTree(roots)`。
    ///
    /// 全量树的前序遍历，产出 `FlatNode` 列表并重建 `tool_call_map`。三个阶段：
    /// 1. 迭代前序收集全部节点，再逆序聚合 `contains_active`（节点自身是当前叶，或任一
    ///    子树含当前叶）；
    /// 2. 根排序：含当前叶的根排前（`sort_by` 只比较布尔值，相等时保持输入顺序）；
    /// 3. 迭代 DFS：父节点先入结果，子节点中含当前叶的排前；缩进规则见下。
    ///
    /// 缩进/连接符规则（固定布局契约）：多根时根挂在虚拟根下（indent 1、自身不画
    /// 连接符）；子节点在“父节点有多个子节点”或“父节点刚分叉且 indent > 0”时缩进 +1，
    /// 否则保持同层（单子链压缩）；只有多子节点的父才给子节点画 `├─`/`└─`，最后一个
    /// 子节点是 `└─`。gutter 列表在画连接符时向上累积，`show` 取决于父节点是否 `is_last`。
    ///
    /// 这里算出的几何信息只是初值：过滤后 `recalculate_visual_structure` 会基于可见树
    /// 重算。`tool_call_map` 则从全量树建立（包含稍后会被过滤掉的 assistant 条目），
    /// 因此 toolResult 行即使与工具调用不在同一条可见链上也能显示参数。
    fn flatten_tree(&mut self) -> Vec<FlatNode> {
        let mut result: Vec<FlatNode> = Vec::new();
        self.tool_call_map.clear();

        // containsActive：前序收集 + 逆序聚合。
        let mut contains_active: HashMap<usize, bool> = HashMap::new();
        let mut order: Vec<usize> = Vec::new();
        let mut pre_order: Vec<usize> = self.arena.roots.iter().rev().copied().collect();
        while let Some(index) = pre_order.pop() {
            order.push(index);
            for child in self.arena.nodes[index].children.iter().rev() {
                pre_order.push(*child);
            }
        }
        for index in order.into_iter().rev() {
            let node = &self.arena.nodes[index];
            let mut has = self
                .current_leaf_id
                .as_deref()
                .is_some_and(|leaf| node.entry.id() == leaf);
            for child in &node.children {
                if contains_active.get(child).copied().unwrap_or(false) {
                    has = true;
                }
            }
            contains_active.insert(index, has);
        }

        let multiple_roots = self.arena.roots.len() > 1;
        let mut ordered_roots = self.arena.roots.clone();
        ordered_roots.sort_by(|a, b| {
            contains_active
                .get(b)
                .copied()
                .unwrap_or(false)
                .cmp(&contains_active.get(a).copied().unwrap_or(false))
        });

        struct StackItem {
            node: usize,
            indent: usize,
            just_branched: bool,
            show_connector: bool,
            is_last: bool,
            gutters: Vec<GutterInfo>,
            is_virtual_root_child: bool,
        }
        let mut stack: Vec<StackItem> = Vec::new();
        for (index, root) in ordered_roots.iter().enumerate().rev() {
            stack.push(StackItem {
                node: *root,
                indent: usize::from(multiple_roots),
                just_branched: multiple_roots,
                show_connector: multiple_roots,
                is_last: index == ordered_roots.len() - 1,
                gutters: Vec::new(),
                is_virtual_root_child: multiple_roots,
            });
        }

        while let Some(item) = stack.pop() {
            // 收集 toolCall（TS 按 `entry.type`/`entry.message.role` 判断：Unknown 的
            // message 条目也读原始 JSON）。
            let tool_calls = {
                let entry = &self.arena.nodes[item.node].entry;
                if entry.kind() == "message" && entry_message_role(entry) == Some("assistant") {
                    match entry_message(entry) {
                        Some(message) => message_tool_calls(message),
                        None => raw_message_tool_calls(entry),
                    }
                } else {
                    Vec::new()
                }
            };
            for (id, name, arguments) in tool_calls {
                self.tool_call_map
                    .insert(id, ToolCallInfo { name, arguments });
            }

            result.push(FlatNode {
                node: item.node,
                indent: item.indent,
                show_connector: item.show_connector,
                is_last: item.is_last,
                gutters: item.gutters.clone(),
                is_virtual_root_child: item.is_virtual_root_child,
            });

            let children = &self.arena.nodes[item.node].children;
            let multiple_children = children.len() > 1;
            let mut ordered_children: Vec<usize> = Vec::with_capacity(children.len());
            let mut rest: Vec<usize> = Vec::new();
            for child in children {
                if contains_active.get(child).copied().unwrap_or(false) {
                    ordered_children.push(*child);
                } else {
                    rest.push(*child);
                }
            }
            ordered_children.extend(rest);

            let child_indent = if multiple_children || (item.just_branched && item.indent > 0) {
                item.indent + 1
            } else {
                item.indent
            };

            let connector_displayed = item.show_connector && !item.is_virtual_root_child;
            let current_display_indent = if self.multiple_roots {
                item.indent.saturating_sub(1)
            } else {
                item.indent
            };
            let connector_position = current_display_indent.saturating_sub(1);
            let child_gutters = if connector_displayed {
                let mut gutters = item.gutters.clone();
                gutters.push(GutterInfo {
                    position: connector_position,
                    show: !item.is_last,
                });
                gutters
            } else {
                item.gutters.clone()
            };

            for (index, child) in ordered_children.iter().enumerate().rev() {
                stack.push(StackItem {
                    node: *child,
                    indent: child_indent,
                    just_branched: multiple_children,
                    show_connector: multiple_children,
                    is_last: index == ordered_children.len() - 1,
                    gutters: child_gutters.clone(),
                    is_virtual_root_child: false,
                });
            }
        }

        result
    }

    /// `findNearestVisibleIndex(entryId)`。
    ///
    /// 从 `entryId` 沿原始父链向上找第一个出现在 `filtered_nodes` 里的节点；找不到（或
    /// `entryId` 为 `None`）时返回最后一个可见下标。空列表返回 0——调用方必须再经过
    /// `filtered_index` 的下标检查，把这种情况当作“无选中项”，不能直接索引。
    fn find_nearest_visible_index(&self, entry_id: Option<&str>) -> isize {
        if self.filtered_nodes.is_empty() {
            return 0;
        }
        let visible_id_to_index: HashMap<&str, usize> = self
            .filtered_nodes
            .iter()
            .enumerate()
            .map(|(index, flat)| (self.arena.entry_id(self.flat_nodes[*flat].node), index))
            .collect();

        let mut current_id = entry_id.map(str::to_string);
        while let Some(id) = current_id {
            if let Some(index) = visible_id_to_index.get(id.as_str()) {
                return *index as isize;
            }
            let Some(node_index) = self.arena.id_to_node.get(&id).copied() else {
                break;
            };
            current_id = self.arena.nodes[node_index]
                .parent
                .map(|parent| self.arena.entry_id(parent).to_string());
        }
        self.filtered_nodes.len() as isize - 1
    }

    /// `applyFilter()`。
    ///
    /// 重建 `filtered_nodes`，顺序固定为：
    /// 1. 列表非空时把当前选中 id 记入 `last_selected_id`，供过滤后恢复选择；
    /// 2. 搜索词按 JS 空白分词并小写化，token 之间是 AND；
    /// 3. 逐节点过滤：`usage` 永远隐藏；非当前叶的 assistant 消息若没有可显示文本且
    ///    `stopReason` 不是 `stop`/`toolUse`（缺失也不满足）则隐藏——只调用工具的轮次不
    ///    显示；随后按 [`FilterMode`] 判断；
    /// 4. 有搜索词时对该节点的可搜索文本做大小写不敏感的子串匹配，要求**所有** token 命中；
    /// 5. 剔除折叠节点的后代：用 `SessionEntry::parent_id()`（原始 `parentId`，与 TS 对齐）
    ///    在 `flat_nodes` 上传播 `skip_set`。这依赖 `flat_nodes` 是前序（父在子前），一次
    ///    遍历即可传递闭包；中间节点被过滤掉也不影响折叠效果；
    /// 6. `recalculate_visual_structure` 重算可见树的缩进/连接符/gutter；
    /// 7. 恢复选中：优先 `last_selected_id`（沿父链找最近可见祖先），否则把越界下标夹到
    ///    最后一个可见项。
    ///
    /// 搜索与过滤只影响可见性，不修改 `flat_nodes` 的结构或 label。
    fn apply_filter(&mut self) {
        if !self.filtered_nodes.is_empty()
            && let Some(id) = self.selected_entry_id().map(str::to_string)
        {
            self.last_selected_id = Some(id);
        }

        let search_tokens: Vec<String> = self
            .search_query
            .to_lowercase()
            .split(is_js_whitespace)
            .filter(|token| !token.is_empty())
            .map(str::to_string)
            .collect();

        self.filtered_nodes = (0..self.flat_nodes.len())
            .filter(|flat_index| {
                let flat = &self.flat_nodes[*flat_index];
                let entry = &self.arena.nodes[flat.node].entry;
                if entry.kind() == "usage" {
                    return false;
                }
                let is_current_leaf = self
                    .current_leaf_id
                    .as_deref()
                    .is_some_and(|leaf| entry.id() == leaf);

                if entry.kind() == "message"
                    && entry_message_role(entry) == Some("assistant")
                    && !is_current_leaf
                {
                    let has_text = entry_message_has_text(entry);
                    let is_error_or_aborted = entry_message_stop_reason(entry)
                        .is_some_and(|reason| reason != "stop" && reason != "toolUse");
                    if !has_text && !is_error_or_aborted {
                        return false;
                    }
                }

                // TS 按 `entry.type` 判断（退化为 Unknown 的已知类型同样算设置类条目）。
                let is_settings_entry = matches!(
                    entry.kind(),
                    "label"
                        | "context_edit"
                        | "custom"
                        | "model_change"
                        | "thinking_level_change"
                        | "session_info"
                );

                let passes_filter = match self.filter_mode {
                    FilterMode::UserOnly => {
                        entry.kind() == "message" && entry_message_role(entry) == Some("user")
                    }
                    FilterMode::NoTools => {
                        !is_settings_entry
                            && !(entry.kind() == "message"
                                && entry_message_role(entry) == Some("toolResult"))
                    }
                    FilterMode::LabeledOnly => self.arena.nodes[flat.node].label.is_some(),
                    FilterMode::All => true,
                    FilterMode::Default => !is_settings_entry,
                };
                if !passes_filter {
                    return false;
                }

                if !search_tokens.is_empty() {
                    let node_text = self.get_searchable_text(flat.node).to_lowercase();
                    return search_tokens.iter().all(|token| node_text.contains(token));
                }
                true
            })
            .collect();

        // 折叠后代剔除。
        if !self.folded_nodes.is_empty() {
            let mut skip_set: HashSet<String> = HashSet::new();
            for flat in &self.flat_nodes {
                let entry = &self.arena.nodes[flat.node].entry;
                let id = entry.id();
                if let Some(parent_id) = entry.parent_id()
                    && (self.folded_nodes.contains(parent_id) || skip_set.contains(parent_id))
                {
                    skip_set.insert(id.to_string());
                }
            }
            self.filtered_nodes.retain(|flat| {
                !skip_set.contains(self.arena.entry_id(self.flat_nodes[*flat].node))
            });
        }

        self.recalculate_visual_structure();

        if let Some(last_selected_id) = self.last_selected_id.clone() {
            self.selected_index = self.find_nearest_visible_index(Some(&last_selected_id));
        } else if self.selected_index >= self.filtered_nodes.len() as isize {
            self.selected_index = (self.filtered_nodes.len() as isize - 1).max(0);
        }

        if !self.filtered_nodes.is_empty()
            && let Some(id) = self.selected_entry_id().map(str::to_string)
        {
            self.last_selected_id = Some(id);
        }
    }

    /// `recalculateVisualStructure()`：按可见树重算缩进/连接符/gutter。
    ///
    /// 过滤会隐藏中间节点，因此不能沿用全量树的几何信息：这里为每个可见节点沿 arena
    /// 父链向上找最近的可见祖先，据此重建 `visible_parent_map`/`visible_children_map`/
    /// `visible_roots`，再用与 `flatten_tree` 相同的缩进/连接符算法重写 `flat_nodes` 中
    /// 对应项的视觉字段。`multiple_roots` 以可见根数量为准重新计算。隐藏节点保留旧的
    /// 视觉字段，但它们不会被渲染。
    ///
    /// `filtered_nodes` 为空时直接返回，三个 `visible_*` 映射保持上一次的值；当前调用
    /// 路径在空列表下不会用它们做折叠或渲染判断。
    fn recalculate_visual_structure(&mut self) {
        if self.filtered_nodes.is_empty() {
            return;
        }

        let visible_ids: HashSet<&str> = self
            .filtered_nodes
            .iter()
            .map(|flat| self.arena.entry_id(self.flat_nodes[*flat].node))
            .collect();

        let mut visible_parent: HashMap<String, Option<String>> = HashMap::new();
        let mut visible_children: HashMap<String, Vec<String>> = HashMap::new();
        let mut visible_roots: Vec<String> = Vec::new();

        for flat in &self.filtered_nodes {
            let node_index = self.flat_nodes[*flat].node;
            let node_id = self.arena.entry_id(node_index).to_string();
            let mut ancestor_id = self.arena.nodes[node_index]
                .parent
                .map(|parent| self.arena.entry_id(parent).to_string());
            let mut visible_ancestor: Option<String> = None;
            while let Some(current) = ancestor_id {
                if visible_ids.contains(current.as_str()) {
                    visible_ancestor = Some(current);
                    break;
                }
                ancestor_id = self
                    .arena
                    .id_to_node
                    .get(&current)
                    .and_then(|index| self.arena.nodes[*index].parent)
                    .map(|parent| self.arena.entry_id(parent).to_string());
            }
            visible_parent.insert(node_id.clone(), visible_ancestor.clone());
            match visible_ancestor {
                Some(parent_id) => visible_children.entry(parent_id).or_default().push(node_id),
                None => visible_roots.push(node_id),
            }
        }

        self.multiple_roots = visible_roots.len() > 1;

        let filtered_node_map: HashMap<String, usize> = self
            .filtered_nodes
            .iter()
            .map(|flat| {
                (
                    self.arena.entry_id(self.flat_nodes[*flat].node).to_string(),
                    *flat,
                )
            })
            .collect();

        struct StackItem {
            node_id: String,
            indent: usize,
            just_branched: bool,
            show_connector: bool,
            is_last: bool,
            gutters: Vec<GutterInfo>,
            is_virtual_root_child: bool,
        }
        let mut stack: Vec<StackItem> = Vec::new();
        for (index, root) in visible_roots.iter().enumerate().rev() {
            stack.push(StackItem {
                node_id: root.clone(),
                indent: usize::from(self.multiple_roots),
                just_branched: self.multiple_roots,
                show_connector: self.multiple_roots,
                is_last: index == visible_roots.len() - 1,
                gutters: Vec::new(),
                is_virtual_root_child: self.multiple_roots,
            });
        }

        while let Some(item) = stack.pop() {
            let Some(flat_index) = filtered_node_map.get(&item.node_id).copied() else {
                continue;
            };
            {
                let flat = &mut self.flat_nodes[flat_index];
                flat.indent = item.indent;
                flat.show_connector = item.show_connector;
                flat.is_last = item.is_last;
                flat.gutters = item.gutters.clone();
                flat.is_virtual_root_child = item.is_virtual_root_child;
            }

            let empty: Vec<String> = Vec::new();
            let children = visible_children.get(&item.node_id).unwrap_or(&empty);
            let multiple_children = children.len() > 1;
            let child_indent = if multiple_children || (item.just_branched && item.indent > 0) {
                item.indent + 1
            } else {
                item.indent
            };
            let connector_displayed = item.show_connector && !item.is_virtual_root_child;
            let current_display_indent = if self.multiple_roots {
                item.indent.saturating_sub(1)
            } else {
                item.indent
            };
            let connector_position = current_display_indent.saturating_sub(1);
            let child_gutters = if connector_displayed {
                let mut gutters = item.gutters.clone();
                gutters.push(GutterInfo {
                    position: connector_position,
                    show: !item.is_last,
                });
                gutters
            } else {
                item.gutters.clone()
            };

            for (index, child) in children.iter().enumerate().rev() {
                stack.push(StackItem {
                    node_id: child.clone(),
                    indent: child_indent,
                    just_branched: multiple_children,
                    show_connector: multiple_children,
                    is_last: index == children.len() - 1,
                    gutters: child_gutters.clone(),
                    is_virtual_root_child: false,
                });
            }
        }

        self.visible_parent_map = visible_parent;
        self.visible_children_map = visible_children;
        self.visible_roots = visible_roots;
    }

    /// `getSearchableText(node)`。
    ///
    /// TS 按 `entry.type` 分派（不是按解析后的变体）：已知类型但字段畸形、退化成
    /// [`SessionEntry::Unknown`] 的条目仍按原类型参与搜索。
    ///
    /// 文本各部分用单个空格连接；非空 label 排在最前。搜索匹配会对整段结果转小写后做
    /// 子串判断，因此 label、role、正文以及各类型的元数据（customType、summary、
    /// modelId、thinkingLevel、targetId 等）都可能被搜索命中。
    fn get_searchable_text(&self, node_index: usize) -> String {
        let node = &self.arena.nodes[node_index];
        let entry = &node.entry;
        let mut parts: Vec<String> = Vec::new();

        if let Some(label) = &node.label
            && !label.is_empty()
        {
            parts.push(label.clone());
        }

        match entry.kind() {
            "message" => {
                let role = entry_message_role(entry).unwrap_or("").to_string();
                parts.push(role.clone());
                if let Some(content) = entry_message_content_for_search(entry) {
                    parts.push(content);
                }
                if role == "bashExecution"
                    && let Some(command) = entry_message_command(entry)
                    && !command.is_empty()
                {
                    parts.push(command.to_string());
                }
            }
            "custom_message" => {
                parts.push(match entry {
                    SessionEntry::CustomMessage(entry) => entry.custom_type.clone(),
                    _ => raw_str_or_undefined(entry, "customType").to_string(),
                });
                match entry {
                    SessionEntry::CustomMessage(entry) => match &entry.content {
                        JsonValue::String(content) => parts.push(content.clone()),
                        content => parts.push(extract_content_json(content)),
                    },
                    _ => match raw_value(entry, "content") {
                        Some(JsonValue::String(content)) => parts.push(content.clone()),
                        Some(content) => parts.push(extract_content_json(content)),
                        None => parts.push(String::new()),
                    },
                }
            }
            "compaction" => parts.push("compaction".to_string()),
            "branch_summary" => {
                parts.push("branch summary".to_string());
                parts.push(match entry {
                    SessionEntry::BranchSummary(entry) => entry.summary.clone(),
                    _ => raw_str(entry, "summary").unwrap_or("").to_string(),
                });
            }
            "session_info" => {
                parts.push("title".to_string());
                let name = match entry {
                    SessionEntry::SessionInfo(entry) => entry.name.as_deref(),
                    _ => raw_str(entry, "name"),
                };
                if let Some(name) = name
                    && !name.is_empty()
                {
                    parts.push(name.to_string());
                }
            }
            "model_change" => {
                parts.push("model".to_string());
                parts.push(match entry {
                    SessionEntry::ModelChange(entry) => entry.model_id.clone(),
                    _ => raw_str(entry, "modelId").unwrap_or("").to_string(),
                });
            }
            "thinking_level_change" => {
                parts.push("thinking".to_string());
                parts.push(match entry {
                    SessionEntry::ThinkingLevelChange(entry) => entry.thinking_level.clone(),
                    _ => raw_str(entry, "thinkingLevel").unwrap_or("").to_string(),
                });
            }
            "custom" => {
                parts.push("custom".to_string());
                parts.push(match entry {
                    SessionEntry::Custom(entry) => entry.custom_type.clone(),
                    _ => raw_str(entry, "customType").unwrap_or("").to_string(),
                });
            }
            "context_edit" => {
                parts.push("context edit".to_string());
                parts.push(
                    if context_edit_omits(entry) {
                        "omit"
                    } else {
                        "replace"
                    }
                    .to_string(),
                );
                parts.push(match entry {
                    SessionEntry::ContextEdit(entry) => entry.target_id.clone(),
                    _ => raw_str(entry, "targetId").unwrap_or("").to_string(),
                });
            }
            "label" => {
                parts.push("label".to_string());
                parts.push(match entry {
                    SessionEntry::Label(entry) => entry.label.clone().unwrap_or_default(),
                    _ => raw_str(entry, "label").unwrap_or("").to_string(),
                });
            }
            _ => {}
        }

        parts.join(" ")
    }

    /// `getStatusLabels()`。
    ///
    /// 非 `Default` 模式追加 ` [no-tools]` 等标记；开启标签时间戳时再追加
    /// ` [+label time]`。结果拼在状态行 `(n/total)` 之后。
    fn get_status_labels(&self) -> String {
        let mut labels = match self.filter_mode {
            FilterMode::NoTools => " [no-tools]",
            FilterMode::UserOnly => " [user]",
            FilterMode::LabeledOnly => " [labeled]",
            FilterMode::All => " [all]",
            FilterMode::Default => "",
        }
        .to_string();
        if self.show_label_timestamps {
            labels.push_str(" [+label time]");
        }
        labels
    }

    /// `isFoldable(entryId)`。
    ///
    /// 可见子节点为空返回 `false`；有可见父节点时，只有父节点下的可见子节点数大于 1 才
    /// 可折叠（单子链不显示折叠标记，折叠键会退化为分支跳转而不是折叠）。判断依据是
    /// `recalculate_visual_structure` 建立的两个可见映射，因此必须在 `apply_filter`
    /// 之后调用。
    fn is_foldable(&self, entry_id: &str) -> bool {
        let Some(children) = self.visible_children_map.get(entry_id) else {
            return false;
        };
        if children.is_empty() {
            return false;
        }
        match self.visible_parent_map.get(entry_id) {
            None | Some(None) => true,
            Some(Some(parent_id)) => self
                .visible_children_map
                .get(parent_id)
                .is_some_and(|siblings| siblings.len() > 1),
        }
    }

    /// `findBranchSegmentStart(direction)`。
    ///
    /// "down"：从选中项沿可见子链向下，遇到分叉点（可见子节点数大于 1）返回其第一个可见
    /// 子节点的下标，走到叶子则返回自身下标。用于 `unfoldOrDown` 在已展开时的“跳到下一
    /// 分支段起点”。
    ///
    /// "up"：沿可见父链向上，遇到第一个分叉的父节点且当前段起点下标小于选中下标时返回
    /// 该段起点（即该父节点下当前这一支的第一个子节点）；一直走到根仍未找到则返回根。
    /// 找不到选中项时原样返回 `selected_index`。
    fn find_branch_segment_start(&self, direction: &str) -> isize {
        let Some(selected_id) = self.selected_entry_id().map(str::to_string) else {
            return self.selected_index;
        };
        let index_by_entry_id: HashMap<&str, usize> = self
            .filtered_nodes
            .iter()
            .enumerate()
            .map(|(index, flat)| (self.arena.entry_id(self.flat_nodes[*flat].node), index))
            .collect();
        let mut current_id = selected_id;

        if direction == "down" {
            loop {
                let empty: Vec<String> = Vec::new();
                let children = self.visible_children_map.get(&current_id).unwrap_or(&empty);
                if children.is_empty() {
                    return index_by_entry_id
                        .get(current_id.as_str())
                        .copied()
                        .unwrap_or(0) as isize;
                }
                if children.len() > 1 {
                    return index_by_entry_id
                        .get(children[0].as_str())
                        .copied()
                        .unwrap_or(0) as isize;
                }
                current_id = children[0].clone();
            }
        }

        loop {
            let parent_id = self.visible_parent_map.get(&current_id).cloned().flatten();
            let Some(parent_id) = parent_id else {
                return index_by_entry_id
                    .get(current_id.as_str())
                    .copied()
                    .unwrap_or(0) as isize;
            };
            let empty: Vec<String> = Vec::new();
            let children = self.visible_children_map.get(&parent_id).unwrap_or(&empty);
            if children.len() > 1
                && let Some(segment_start) = index_by_entry_id.get(current_id.as_str()).copied()
                && (segment_start as isize) < self.selected_index
            {
                return segment_start as isize;
            }
            current_id = parent_id;
        }
    }

    /// `handleInput`：执行状态变更并返回需要宿主分发的动作。
    ///
    /// 按键含义全部来自 `get_keybindings()` 的命名绑定，不硬编码字节序列：导航
    /// （`tui.select.up/down`）、折叠（`app.tree.foldOrUp`/`unfoldOrDown`）、翻页
    /// （复用 `tui.editor.cursorLeft/Right` 与 `tui.select.pageUp/pageDown`）、确认/取消、
    /// 复制、过滤模式、编辑标签、切换时间戳。本方法只改状态并返回 [`TreeAction`]，**不调用
    /// 任何回调**；回调由 `Component::handle_input` 或 [`TreeSelectorComponent`] 分发。
    ///
    /// 关键边界：上下键在列表内循环；列表为空时上键可把 `selected_index` 变成 -1（表示无
    /// 选中项）；翻页会夹在 `[0, len-1]`；任何过滤模式或搜索变化都清空折叠状态；不属于任何
    /// 绑定且不含控制字符的按键会原样追加到搜索词。
    pub fn handle_input_action(&mut self, key_data: &str) -> TreeAction {
        let kb = get_keybindings();
        // 上下移动在列表内循环（0 <-> len-1）；空列表时上键会得到 -1（无选中项）。
        if kb.matches(key_data, "tui.select.up") {
            self.selected_index = if self.selected_index == 0 {
                self.filtered_nodes.len() as isize - 1
            } else {
                self.selected_index - 1
            };
        } else if kb.matches(key_data, "tui.select.down") {
            self.selected_index = if self.selected_index == self.filtered_nodes.len() as isize - 1 {
                0
            } else {
                self.selected_index + 1
            };
        } else if kb.matches(key_data, "app.tree.foldOrUp") {
            // 可折叠且当前展开时先折叠；否则跳到当前分支段的起点（往上）。
            let current_id = self.selected_entry_id().map(str::to_string);
            if let Some(current_id) = current_id
                && self.is_foldable(&current_id)
                && !self.folded_nodes.contains(&current_id)
            {
                self.folded_nodes.insert(current_id);
                self.apply_filter();
            } else {
                self.selected_index = self.find_branch_segment_start("up");
            }
        } else if kb.matches(key_data, "app.tree.unfoldOrDown") {
            // 当前已折叠时先展开；否则跳到下一分支段的起点（往下）。
            let current_id = self.selected_entry_id().map(str::to_string);
            if let Some(current_id) = current_id
                && self.folded_nodes.contains(&current_id)
            {
                self.folded_nodes.remove(&current_id);
                self.apply_filter();
            } else {
                self.selected_index = self.find_branch_segment_start("down");
            }
        } else if kb.matches(key_data, "tui.editor.cursorLeft")
            || kb.matches(key_data, "tui.select.pageUp")
        {
            // ←/→ 与 pageUp/pageDown 共用：一次移动 max_visible_lines 行并夹在边界内。
            self.selected_index = (self.selected_index - self.max_visible_lines as isize).max(0);
        } else if kb.matches(key_data, "tui.editor.cursorRight")
            || kb.matches(key_data, "tui.select.pageDown")
        {
            self.selected_index = (self.selected_index + self.max_visible_lines as isize)
                .min(self.filtered_nodes.len() as isize - 1);
        } else if kb.matches(key_data, "tui.select.confirm") {
            // 确认与编辑标签只读当前选中项；没有可见选中项时按键被吞掉，不产生动作。
            if let Some(id) = self.selected_entry_id().map(str::to_string) {
                return TreeAction::Select(id);
            }
        } else if kb.matches(key_data, "app.message.copy") {
            return TreeAction::Copy(self.copy_selected());
        } else if kb.matches(key_data, "tui.select.cancel") {
            // 有搜索词时第一下只清空搜索与折叠，搜索为空才真正发出取消动作。
            if !self.search_query.is_empty() {
                self.search_query.clear();
                self.folded_nodes.clear();
                self.apply_filter();
            } else {
                return TreeAction::Cancel;
            }
        } else if kb.matches(key_data, "app.tree.filter.default") {
            // 五个过滤键各自是“切到该模式；若已在该模式则回 Default”，且都清空折叠状态。
            self.filter_mode = FilterMode::Default;
            self.folded_nodes.clear();
            self.apply_filter();
        } else if kb.matches(key_data, "app.tree.filter.noTools") {
            self.filter_mode = if self.filter_mode == FilterMode::NoTools {
                FilterMode::Default
            } else {
                FilterMode::NoTools
            };
            self.folded_nodes.clear();
            self.apply_filter();
        } else if kb.matches(key_data, "app.tree.filter.userOnly") {
            self.filter_mode = if self.filter_mode == FilterMode::UserOnly {
                FilterMode::Default
            } else {
                FilterMode::UserOnly
            };
            self.folded_nodes.clear();
            self.apply_filter();
        } else if kb.matches(key_data, "app.tree.filter.labeledOnly") {
            self.filter_mode = if self.filter_mode == FilterMode::LabeledOnly {
                FilterMode::Default
            } else {
                FilterMode::LabeledOnly
            };
            self.folded_nodes.clear();
            self.apply_filter();
        } else if kb.matches(key_data, "app.tree.filter.all") {
            self.filter_mode = if self.filter_mode == FilterMode::All {
                FilterMode::Default
            } else {
                FilterMode::All
            };
            self.folded_nodes.clear();
            self.apply_filter();
        } else if kb.matches(key_data, "app.tree.filter.cycleBackward") {
            // cycle 按 `FilterMode::CYCLE` 数组前后移动（取模回绕），同样清空折叠。
            let index = FilterMode::CYCLE
                .iter()
                .position(|mode| *mode == self.filter_mode)
                .unwrap_or(0);
            self.filter_mode =
                FilterMode::CYCLE[(index + FilterMode::CYCLE.len() - 1) % FilterMode::CYCLE.len()];
            self.folded_nodes.clear();
            self.apply_filter();
        } else if kb.matches(key_data, "app.tree.filter.cycleForward") {
            let index = FilterMode::CYCLE
                .iter()
                .position(|mode| *mode == self.filter_mode)
                .unwrap_or(0);
            self.filter_mode = FilterMode::CYCLE[(index + 1) % FilterMode::CYCLE.len()];
            self.folded_nodes.clear();
            self.apply_filter();
        } else if kb.matches(key_data, "tui.editor.deleteCharBackward") {
            // 退格从搜索词尾部删一个 UTF-16 code unit（可能切开代理对，TS 的 slice 同样如此）。
            if !self.search_query.is_empty() {
                // TS `searchQuery.slice(0, -1)` 按 UTF-16 code unit 截断。
                let mut units: Vec<u16> = self.search_query.encode_utf16().collect();
                units.pop();
                self.search_query = String::from_utf16_lossy(&units);
                self.folded_nodes.clear();
                self.apply_filter();
            }
        } else if kb.matches(key_data, "app.tree.editLabel") {
            // 无可见选中项时不产生 EditLabel，按键被吞掉。
            if let Some(selected) = self.get_selected_node() {
                return TreeAction::EditLabel(
                    selected.entry.id().to_string(),
                    selected.label.map(str::to_string),
                );
            }
        } else if kb.matches(key_data, "app.tree.toggleLabelTimestamp") {
            // 时间戳只影响显示，不改变过滤结果或折叠状态。
            self.show_label_timestamps = !self.show_label_timestamps;
        } else {
            // 其余按键：拦截控制字符（含 ESC 序列）后追加到搜索词，并清空折叠。
            let has_control_chars = key_data.chars().any(|ch| {
                let code = ch as u32;
                code < 32 || code == 0x7f || (0x80..=0x9f).contains(&code)
            });
            if !has_control_chars && !key_data.is_empty() {
                self.search_query.push_str(key_data);
                self.folded_nodes.clear();
                self.apply_filter();
            }
        }
        TreeAction::None
    }

    /// 把动作分发给 `TreeList` 自己的回调字段。
    ///
    /// 只被 `TreeList` 的 `Component::handle_input` 调用；[`TreeSelectorComponent`] 不复用
    /// 它，因为组件层要先检查 `tree_list.on_*` 是否被宿主覆盖，再决定默认行为。
    fn dispatch_action(&mut self, action: TreeAction) {
        match action {
            TreeAction::None => {}
            TreeAction::Select(id) => {
                if let Some(callback) = &mut self.on_select {
                    callback(&id);
                }
            }
            TreeAction::Cancel => {
                if let Some(callback) = &mut self.on_cancel {
                    callback();
                }
            }
            TreeAction::Copy(text) => {
                if let Some(callback) = &mut self.on_copy {
                    callback(text.as_deref());
                }
            }
            TreeAction::EditLabel(id, label) => {
                if let Some(callback) = &mut self.on_label_edit {
                    callback(&id, label.as_deref());
                }
            }
        }
    }

    /// `render(width)`。
    ///
    /// 只渲染窗口内的行：窗口高度是 `max_visible_lines`，以选中项为中心再夹到
    /// `[0, len - max_visible_lines]`。每行组成：光标 gutter（选中时为 `› `）、缩进前缀
    /// （每层 3 列，含 gutter 竖线与 `├`/`└` 连接符，连接符第 2 列按折叠状态显示
    /// `⊟`/`⊞`/`─`）、`[label] `、可选时间戳、条目内容；选中行加选中背景色，条目内容
    /// 加粗。列表为空时输出两行 “No entries found / (0/0)”。最后一行是
    /// `(选中序号/总数)` 状态行。行宽超出时由 `render_horizontal_viewport` 在必要时水平
    /// 平移正文。
    pub fn render(&mut self, width: usize) -> Vec<String> {
        let mut lines: Vec<String> = Vec::new();

        if self.filtered_nodes.is_empty() {
            lines.push(fg(
                ThemeColor::Muted,
                &truncate_to_width("  No entries found", width, "...", false),
            ));
            lines.push(fg(
                ThemeColor::Muted,
                &truncate_to_width(
                    &format!("  (0/0){}", self.get_status_labels()),
                    width,
                    "...",
                    false,
                ),
            ));
            return lines;
        }

        let length = self.filtered_nodes.len();
        let start_index = (self.selected_index - (self.max_visible_lines as isize) / 2)
            .min(length as isize - self.max_visible_lines as isize)
            .max(0) as usize;
        let end_index = (start_index + self.max_visible_lines).min(length);

        let mut rendered_rows: Vec<HorizontalViewportRow> = Vec::new();
        for index in start_index..end_index {
            let flat_node = self.filtered_nodes[index];
            let entry_id = self
                .arena
                .entry_id(self.flat_nodes[flat_node].node)
                .to_string();
            let is_selected = index as isize == self.selected_index;

            let cursor = if is_selected {
                fg(ThemeColor::Accent, "› ")
            } else {
                "  ".to_string()
            };

            let display_indent = if self.multiple_roots {
                self.flat_nodes[flat_node].indent.saturating_sub(1)
            } else {
                self.flat_nodes[flat_node].indent
            };

            let connector = if self.flat_nodes[flat_node].show_connector
                && !self.flat_nodes[flat_node].is_virtual_root_child
            {
                if self.flat_nodes[flat_node].is_last {
                    "└─ "
                } else {
                    "├─ "
                }
            } else {
                ""
            };
            let connector_position = if connector.is_empty() {
                None
            } else {
                Some(display_indent.saturating_sub(1))
            };

            // 每个显示层占 3 列：第 0 列是祖先竖线（`│`）或连接符拐角，第 1 列是连接符
            // 横线/折叠标记，第 2 列留空。命中 gutter 列表时优先按 gutter 绘制，否则才看
            // 当前节点的连接符位置。
            let total_chars = display_indent * 3;
            let prefix_chars: Vec<char> = {
                let is_folded = self.folded_nodes.contains(&entry_id);
                let mut chars = Vec::with_capacity(total_chars);
                for i in 0..total_chars {
                    let level = i / 3;
                    let pos_in_level = i % 3;
                    let gutter = self.flat_nodes[flat_node]
                        .gutters
                        .iter()
                        .find(|gutter| gutter.position == level);
                    if let Some(gutter) = gutter {
                        if pos_in_level == 0 {
                            chars.push(if gutter.show { '│' } else { ' ' });
                        } else {
                            chars.push(' ');
                        }
                    } else if connector_position == Some(level) {
                        if pos_in_level == 0 {
                            chars.push(if self.flat_nodes[flat_node].is_last {
                                '└'
                            } else {
                                '├'
                            });
                        } else if pos_in_level == 1 {
                            let foldable = self.is_foldable(&entry_id);
                            chars.push(if is_folded {
                                '⊞'
                            } else if foldable {
                                '⊟'
                            } else {
                                '─'
                            });
                        } else {
                            chars.push(' ');
                        }
                    } else {
                        chars.push(' ');
                    }
                }
                chars
            };
            let prefix: String = prefix_chars.into_iter().collect();

            let shows_fold_in_connector = self.flat_nodes[flat_node].show_connector
                && !self.flat_nodes[flat_node].is_virtual_root_child;
            let is_folded = self.folded_nodes.contains(&entry_id);
            let fold_marker = if is_folded && !shows_fold_in_connector {
                fg(ThemeColor::Accent, "⊞ ")
            } else {
                String::new()
            };

            let path_marker = if self.active_path_ids.contains(&entry_id) {
                fg(ThemeColor::Accent, "• ")
            } else {
                String::new()
            };

            let node = &self.arena.nodes[self.flat_nodes[flat_node].node];
            let label = node
                .label
                .as_deref()
                .filter(|label| !label.is_empty())
                .map(|label| fg(ThemeColor::Warning, &format!("[{label}] ")))
                .unwrap_or_default();
            let label_timestamp = if self.show_label_timestamps
                && node.label.as_deref().is_some_and(|label| !label.is_empty())
                && let Some(timestamp) = node
                    .label_timestamp
                    .as_deref()
                    .filter(|timestamp| !timestamp.is_empty())
            {
                fg(
                    ThemeColor::Muted,
                    &format!("{} ", self.format_label_timestamp(timestamp)),
                )
            } else {
                String::new()
            };
            let content = self.get_entry_display_text(self.flat_nodes[flat_node].node, is_selected);
            let prefix_part = format!("{}{fold_marker}{path_marker}", fg(ThemeColor::Dim, &prefix));
            let anchor_col = visible_width(&prefix_part);
            let mut gutter = cursor;
            let mut body = format!("{prefix_part}{label}{label_timestamp}{content}");
            if is_selected {
                gutter = bg(ThemeBg::SelectedBg, &gutter);
                body = bg(ThemeBg::SelectedBg, &body);
            }
            rendered_rows.push(HorizontalViewportRow {
                gutter,
                anchor_col,
                body_width: visible_width(&body),
                body,
                is_selected,
            });
        }

        lines.extend(render_horizontal_viewport(&rendered_rows, width));
        lines.push(fg(
            ThemeColor::Muted,
            &truncate_to_width(
                &format!(
                    "  ({}/{}){}",
                    self.selected_index + 1,
                    length,
                    self.get_status_labels()
                ),
                width,
                "...",
                false,
            ),
        ));

        lines
    }

    /// `getEntryDisplayText(node, isSelected)`。
    ///
    /// TS 按 `entry.type` 分派；退化为 [`SessionEntry::Unknown`] 的已知类型仍按原类型渲染
    /// （字段缺失时模板字面量得到 `"undefined"`，与 JS 一致）。
    ///
    /// `is_selected` 只影响最终是否加粗；颜色由各类型的分支固定。未知 `type` 返回空串。
    fn get_entry_display_text(&self, node_index: usize, is_selected: bool) -> String {
        let node = &self.arena.nodes[node_index];
        let entry = &node.entry;
        let result = match entry.kind() {
            "message" => self.message_display_text(entry),
            "custom_message" => {
                let custom_type = match entry {
                    SessionEntry::CustomMessage(entry) => entry.custom_type.clone(),
                    _ => raw_str_or_undefined(entry, "customType").to_string(),
                };
                let content = match entry {
                    SessionEntry::CustomMessage(entry) => match &entry.content {
                        JsonValue::String(content) => content.clone(),
                        content => extract_full_content_json(content),
                    },
                    _ => match raw_value(entry, "content") {
                        Some(JsonValue::String(content)) => content.clone(),
                        Some(content) => extract_full_content_json(content),
                        None => String::new(),
                    },
                };
                format!(
                    "{}{}",
                    fg(
                        ThemeColor::CustomMessageLabel,
                        &format!("[{custom_type}]: ")
                    ),
                    normalize_display(&content)
                )
            }
            "compaction" => {
                let tokens_before = compaction_tokens_before(entry);
                let tokens = js_round(tokens_before / 1000.0);
                fg(
                    ThemeColor::BorderAccent,
                    &format!("[compaction: {}k tokens]", js_number_to_string(tokens)),
                )
            }
            "branch_summary" => {
                let summary = match entry {
                    SessionEntry::BranchSummary(entry) => entry.summary.as_str(),
                    _ => raw_str(entry, "summary").unwrap_or(""),
                };
                format!(
                    "{}{}",
                    fg(ThemeColor::Warning, "[branch summary]: "),
                    normalize_display(summary)
                )
            }
            "model_change" => {
                let model_id = match entry {
                    SessionEntry::ModelChange(entry) => entry.model_id.as_str(),
                    _ => raw_str_or_undefined(entry, "modelId"),
                };
                fg(ThemeColor::Dim, &format!("[model: {model_id}]"))
            }
            "thinking_level_change" => {
                let thinking_level = match entry {
                    SessionEntry::ThinkingLevelChange(entry) => entry.thinking_level.as_str(),
                    _ => raw_str_or_undefined(entry, "thinkingLevel"),
                };
                fg(ThemeColor::Dim, &format!("[thinking: {thinking_level}]"))
            }
            "custom" => {
                let custom_type = match entry {
                    SessionEntry::Custom(entry) => entry.custom_type.as_str(),
                    _ => raw_str_or_undefined(entry, "customType"),
                };
                fg(ThemeColor::Dim, &format!("[custom: {custom_type}]"))
            }
            "context_edit" => {
                let omit = context_edit_omits(entry);
                let target_id = match entry {
                    SessionEntry::ContextEdit(entry) => entry.target_id.as_str(),
                    _ => raw_str_or_undefined(entry, "targetId"),
                };
                fg(
                    ThemeColor::Dim,
                    &format!(
                        "[context {}: {}]",
                        if omit { "omit" } else { "replace" },
                        target_id
                    ),
                )
            }
            "label" => {
                let label = match entry {
                    SessionEntry::Label(entry) => entry.label.clone(),
                    _ => match raw_value(entry, "label") {
                        None | Some(JsonValue::Null) => None,
                        Some(value) => Some(js_string(Some(value))),
                    },
                };
                fg(
                    ThemeColor::Dim,
                    &format!("[label: {}]", label.as_deref().unwrap_or("(cleared)")),
                )
            }
            "session_info" => {
                let name = match entry {
                    SessionEntry::SessionInfo(entry) => entry.name.clone(),
                    _ => raw_value(entry, "name")
                        .filter(|value| is_truthy(Some(value)))
                        .map(|value| js_string(Some(value))),
                };
                match name {
                    Some(name) if !name.is_empty() => format!(
                        "{}{}{}",
                        fg(ThemeColor::Dim, "[title: "),
                        fg(ThemeColor::Dim, &name),
                        fg(ThemeColor::Dim, "]")
                    ),
                    _ => format!(
                        "{}{}{}",
                        fg(ThemeColor::Dim, "[title: "),
                        italic(&fg(ThemeColor::Dim, "empty")),
                        fg(ThemeColor::Dim, "]")
                    ),
                }
            }
            _ => String::new(),
        };

        if is_selected { bold(&result) } else { result }
    }

    /// `entry.type === "message"` 分支（typed 或 Unknown 原始 JSON）。
    ///
    /// 按 `role` 分派：`user` 显示正文；`assistant` 依次尝试正文 → `(aborted)` →
    /// `errorMessage`（截 80 个 UTF-16 code unit）→ `(no content)`；`toolResult` 先在
    /// `tool_call_map` 里查 `toolCallId`，查不到再显示 `[toolName]`，连名字都没有时回退到
    /// `[tool]`；`bashExecution` 显示 `[bash]: command`；其它 role 显示 `[role]`。
    fn message_display_text(&self, entry: &SessionEntry) -> String {
        let role = entry_message_role(entry).unwrap_or("undefined");
        match role {
            "user" => {
                let content = self.entry_message_content_text(entry);
                format!(
                    "{}{}",
                    fg(ThemeColor::Accent, "user: "),
                    normalize_display(&content)
                )
            }
            "assistant" => {
                let text_content = normalize_display(&self.entry_message_content_text(entry));
                if !text_content.is_empty() {
                    format!("{}{}", fg(ThemeColor::Success, "assistant: "), text_content)
                } else if entry_message_stop_reason(entry) == Some("aborted") {
                    format!(
                        "{}{}",
                        fg(ThemeColor::Success, "assistant: "),
                        fg(ThemeColor::Muted, "(aborted)")
                    )
                } else if let Some(error) =
                    entry_message_error_message(entry).filter(|error| !error.is_empty())
                {
                    let error = slice_utf16(&normalize_display(error), 0, 80);
                    format!(
                        "{}{}",
                        fg(ThemeColor::Success, "assistant: "),
                        fg(ThemeColor::Error, &error)
                    )
                } else {
                    format!(
                        "{}{}",
                        fg(ThemeColor::Success, "assistant: "),
                        fg(ThemeColor::Muted, "(no content)")
                    )
                }
            }
            "toolResult" => {
                let tool_call_id = entry
                    .message()
                    .and_then(message_tool_call_id)
                    .or_else(|| raw_message_value(entry, "toolCallId").and_then(JsonValue::as_str));
                let tool_call = tool_call_id.and_then(|id| self.tool_call_map.get(id));
                match tool_call {
                    Some(tool_call) => fg(
                        ThemeColor::Muted,
                        &self.format_tool_call(&tool_call.name, &tool_call.arguments),
                    ),
                    None => {
                        let tool_name = entry
                            .message()
                            .and_then(message_tool_name)
                            .or_else(|| {
                                raw_message_value(entry, "toolName").and_then(JsonValue::as_str)
                            })
                            .unwrap_or("tool");
                        fg(ThemeColor::Muted, &format!("[{tool_name}]"))
                    }
                }
            }
            "bashExecution" => fg(
                ThemeColor::Dim,
                &format!(
                    "[bash]: {}",
                    normalize_display(entry_message_command(entry).unwrap_or(""))
                ),
            ),
            other => fg(ThemeColor::Dim, &format!("[{other}]")),
        }
    }

    /// `extractContent(entry.message.content)`（typed 或 Unknown 原始 JSON）。
    ///
    /// 内容截断到 200 个 UTF-16 code unit（`extractContent` 语义），只用于显示；完整内容
    /// 的复制走 `get_entry_copy_text`。
    fn entry_message_content_text(&self, entry: &SessionEntry) -> String {
        match entry.message() {
            Some(message) => extract_content_message(message),
            None => raw_message_value(entry, "content")
                .map(extract_content_json)
                .unwrap_or_default(),
        }
    }

    /// `formatLabelTimestamp(timestamp)`（TS 私有方法，入参是字符串）。
    ///
    /// TS 用 `new Date(timestamp)`：接受 ISO 日期（date-only 按 **UTC** 午夜）与本地时间
    /// 说明符；无法解析时是 Invalid Date，所有 getter 返回 `NaN`，输出
    /// `"aN/NaN/NaN NaN:NaN"`。
    fn format_label_timestamp(&self, timestamp: &str) -> String {
        let Some(date) = parse_js_date(timestamp) else {
            return "aN/NaN/NaN NaN:NaN".to_string();
        };
        let now = Local::now();
        let time = format!("{:02}:{:02}", date.hour(), date.minute());
        // 同一天：只显示 HH:MM（小时/分钟补零，与 TS 的 getHours/getMinutes 格式化一致）。
        if date.year() == now.year() && date.month() == now.month() && date.day() == now.day() {
            return time;
        }
        let month = date.month();
        let day = date.day();
        // 同一年：显示 M/D（不补零）。
        if date.year() == now.year() {
            return format!("{month}/{day} {time}");
        }
        // 跨年：年份取十进制字符串的最后两位（TS 是 `String(year).slice(-2)`）。
        let year_text = date.year().to_string();
        let year: String = {
            let chars: Vec<char> = year_text.chars().collect();
            chars[chars.len().saturating_sub(2)..].iter().collect()
        };
        format!("{year}/{month}/{day} {time}")
    }

    /// `formatToolCall(name, args)`（本文件的 `shortenPath` 用 HOME/USERPROFILE）。
    ///
    /// 各分支的默认值与截断长度与 TS 相同：`read` 追加 `format_read_line_range`；`bash`
    /// 命令按 UTF-16 截到 50 个 code unit，并按**原始命令**长度决定是否补 `...`；未知工具
    /// 用 JS `JSON.stringify` 序列化参数后截到 40。路径的 `~` 归约只做字符串前缀替换，
    /// 不判断路径分隔符边界（`/home/a` 也会匹配 `/home/ab`，与 TS 的 `startsWith` 一致），
    /// 也不规范化 `.`/`..`。
    fn format_tool_call(&self, name: &str, args: &JsonObject) -> String {
        // HOME 优先，其次 USERPROFILE（TS 同样在 HOME 为空时回退），两者都为空则不归约。
        fn shorten_path_env(path: &str) -> String {
            let home = std::env::var("HOME")
                .ok()
                .filter(|home| !home.is_empty())
                .or_else(|| {
                    std::env::var("USERPROFILE")
                        .ok()
                        .filter(|home| !home.is_empty())
                })
                .unwrap_or_default();
            if !home.is_empty() && path.starts_with(&home) {
                return format!("~{}", &path[home.len()..]);
            }
            path.to_string()
        }

        // 参数读取复刻 TS 的 `args.x || default`：falsy 值（空串、0、false、null、缺失）
        // 都回退到默认值。
        fn arg_string(args: &JsonObject, key: &str, default: &str) -> String {
            match args.get(key) {
                Some(value) if is_truthy(Some(value)) => js_string(Some(value)),
                _ => default.to_string(),
            }
        }

        fn path_argument(args: &JsonObject) -> String {
            for key in ["path", "file_path"] {
                match args.get(key) {
                    Some(value) if is_truthy(Some(value)) => return js_string(Some(value)),
                    _ => {}
                }
            }
            String::new()
        }

        match name {
            "read" => format!(
                "[read: {}{}]",
                shorten_path_env(&path_argument(args)),
                format_read_line_range(args.get("offset"), args.get("limit"))
            ),
            "write" => format!("[write: {}]", shorten_path_env(&path_argument(args))),
            "edit" => format!("[edit: {}]", shorten_path_env(&path_argument(args))),
            "bash" => {
                let raw_command = arg_string(args, "command", "");
                let command = slice_utf16(&normalize_display(&raw_command), 0, 50);
                format!(
                    "[bash: {}{}]",
                    command,
                    if utf16_len(&raw_command) > 50 {
                        "..."
                    } else {
                        ""
                    }
                )
            }
            "grep" => format!(
                "[grep: /{}/ in {}]",
                arg_string(args, "pattern", ""),
                shorten_path_env(&arg_string(args, "path", "."))
            ),
            "find" => format!(
                "[find: {} in {}]",
                arg_string(args, "pattern", ""),
                shorten_path_env(&arg_string(args, "path", "."))
            ),
            "ls" => format!("[ls: {}]", shorten_path_env(&arg_string(args, "path", "."))),
            other => {
                let args_map: serde_json::Map<String, JsonValue> =
                    args.clone().into_iter().collect();
                let args_value = JsonValue::Object(args_map);
                let serialized = js_stringify(&args_value);
                format!(
                    "[{}: {}{}]",
                    other,
                    slice_utf16(&serialized, 0, 40),
                    if utf16_len(&serialized) > 40 {
                        "..."
                    } else {
                        ""
                    }
                )
            }
        }
    }

    /// `getEntryCopyText(node)`。
    ///
    /// TS 按 `entry.type` 分派；Unknown 的已知类型按原始 JSON 取字段。
    ///
    /// 返回的是**完整**文本（不截断），与显示用的 `extractContent` 不同：
    /// - `message`：`bashExecution` 复制命令；其它 role 在存在 `content` 键时复制全文，
    ///   assistant 正文为空时回退到 `errorMessage`；没有 `content` 键则没有可复制文本；
    /// - `custom_message`：复制 content；`compaction`/`branch_summary`：复制 summary；
    /// - 其它类型一律 `None`。最终再按 `js_trim` 过滤空内容。
    fn get_entry_copy_text(&self, node: &TreeListNodeRef<'_>) -> Option<String> {
        let entry = node.entry;
        let text: Option<String> = match entry.kind() {
            "message" => {
                let role = entry_message_role(entry);
                if role == Some("bashExecution") {
                    entry_message_command(entry).map(str::to_string)
                } else {
                    let has_content = match entry.message() {
                        Some(message) => {
                            !matches!(message, AgentMessage::Custom(object) if !object.contains_key("content"))
                        }
                        None => raw_message_has_content(entry),
                    };
                    if has_content {
                        let mut content = match entry.message() {
                            Some(message) => extract_full_content_message(message),
                            None => raw_message_value(entry, "content")
                                .map(extract_full_content_json)
                                .unwrap_or_default(),
                        };
                        if content.is_empty() && role == Some("assistant") {
                            content = entry_message_error_message(entry)
                                .unwrap_or_default()
                                .to_string();
                        }
                        Some(content)
                    } else {
                        None
                    }
                }
            }
            "custom_message" => Some(match entry {
                SessionEntry::CustomMessage(entry) => extract_full_content_json(&entry.content),
                _ => raw_value(entry, "content")
                    .map(extract_full_content_json)
                    .unwrap_or_default(),
            }),
            "compaction" => Some(match entry {
                SessionEntry::Compaction(entry) => entry.summary.clone(),
                _ => raw_str(entry, "summary").unwrap_or("").to_string(),
            }),
            "branch_summary" => Some(match entry {
                SessionEntry::BranchSummary(entry) => entry.summary.clone(),
                _ => raw_str(entry, "summary").unwrap_or("").to_string(),
            }),
            _ => None,
        };

        text.filter(|text| !js_trim(text).is_empty())
    }
}

impl Component for TreeList {
    fn render(&mut self, width: usize) -> Vec<String> {
        TreeList::render(self, width)
    }

    fn handle_input(&mut self, data: &str) {
        let action = self.handle_input_action(data);
        self.dispatch_action(action);
    }

    fn invalidate(&mut self) {
        // TS 的 TreeList.invalidate() 是空实现。
    }

    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }
}

/// 水平视口行。
///
/// `body` 已包含前缀/label/内容与颜色；`anchor_col` 是正文起点（光标 gutter 之后）的
/// 可见列号，`body_width` 是整行正文宽度。两者都在进入视口前算好，滚动时只做切片。
struct HorizontalViewportRow {
    gutter: String,
    body: String,
    anchor_col: usize,
    body_width: usize,
    is_selected: bool,
}

/// `renderHorizontalViewport(rows, width)`：只在选中行锚点太靠右时左移正文。
///
/// 背景：深层节点的前缀可能吃掉全部宽度，选中行只剩几个字符。先算所有可见行的最大正文
/// 宽度 `max_body_width`，只有存在溢出（`max_body_width > width - TREE_GUTTER_WIDTH`）且
/// 选中行的 `anchor_col` 逼近右边界时才滚动：
/// `scroll = min(anchor_col - context_width, max_scroll)`，其中 `context_width` 由
/// `MIN/MAX_ANCHOR_CONTEXT_WIDTH` 夹在 `viewport/4` 附近。
///
/// 滚动时对每行的 `body` 按可见列执行 `slice_by_column(..., strict=true)`，并补一个
/// `\x1b[0m`（切片可能丢掉结尾的样式 reset）；gutter 始终保留在最左，不参与水平滚动；
/// 最后统一 `truncate_to_width` 到 `width`。没有溢出或没有选中行时不滚动。
fn render_horizontal_viewport(rows: &[HorizontalViewportRow], width: usize) -> Vec<String> {
    let viewport_width = width.saturating_sub(TREE_GUTTER_WIDTH);
    let max_body_width = rows.iter().map(|row| row.body_width).max().unwrap_or(0);
    let max_horizontal_scroll = max_body_width.saturating_sub(viewport_width);
    let selected_row = rows.iter().find(|row| row.is_selected);

    let mut horizontal_scroll: isize = 0;
    if let Some(selected_row) = selected_row
        && max_horizontal_scroll > 0
    {
        let min_visible_anchor_content_width = MAX_VISIBLE_ANCHOR_CONTENT_WIDTH
            .min(MIN_VISIBLE_ANCHOR_CONTENT_WIDTH.max(viewport_width / 3));
        if selected_row.anchor_col as isize
            > viewport_width as isize - min_visible_anchor_content_width as isize
        {
            let anchor_context_width =
                MAX_ANCHOR_CONTEXT_WIDTH.min(MIN_ANCHOR_CONTEXT_WIDTH.max(viewport_width / 4));
            horizontal_scroll = (selected_row.anchor_col as isize - anchor_context_width as isize)
                .min(max_horizontal_scroll as isize);
        }
    }

    rows.iter()
        .map(|row| {
            let line = if horizontal_scroll > 0 {
                format!(
                    "{}{}\x1b[0m",
                    row.gutter,
                    slice_by_column(&row.body, horizontal_scroll as usize, viewport_width, true)
                )
            } else {
                format!("{}{}", row.gutter, row.body)
            };
            truncate_to_width(&line, width, "", false)
        })
        .collect()
}

/// `now.toISOString()`。
///
/// UTC、毫秒精度、以 `Z` 结尾（`SecondsFormat::Millis` + `use_z = true`）；用作新标签的
/// 默认时间戳。
fn now_iso_string() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true)
}

/// `new Date(timestamp)` 的本地时间结果；`None` 表示 Invalid Date。
///
/// 覆盖 TS 实际会用到的输入形态：带偏移的 ISO 8601、date-only（ES 规范要求按 **UTC**
/// 解释）、以及不带时区的 ISO 日期时间（按本地时间解释）。V8 还接受若干实现自定义格式，
/// 这里不逐个复刻（见文件头的表示差异说明）。
///
/// 本地时间统一用 `earliest()`：夏令时重叠时取较早的偏移，本地时间不存在（春季跳变）
/// 时返回 `None`，按 Invalid Date 处理。
fn parse_js_date(timestamp: &str) -> Option<chrono::DateTime<Local>> {
    // 带 `Z` 或 `+hh:mm` 偏移的 ISO 8601。
    if let Ok(date) = chrono::DateTime::parse_from_rfc3339(timestamp) {
        return Some(date.with_timezone(&Local));
    }
    // date-only：ES 规范把 `YYYY-MM-DD` 当作 UTC。
    if let Ok(date) = NaiveDate::parse_from_str(timestamp, "%Y-%m-%d") {
        let naive = date.and_hms_opt(0, 0, 0)?;
        return Some(Utc.from_utc_datetime(&naive).with_timezone(&Local));
    }
    // `YYYY/MM/DD`：V8 按本地时间解释。
    if let Ok(date) = NaiveDate::parse_from_str(timestamp, "%Y/%m/%d") {
        let naive = date.and_hms_opt(0, 0, 0)?;
        return Local.from_local_datetime(&naive).earliest();
    }
    // 不带时区的日期时间：按本地时间解释。
    for format in [
        "%Y-%m-%dT%H:%M:%S%.f",
        "%Y-%m-%d %H:%M:%S%.f",
        "%Y-%m-%dT%H:%M:%S",
        "%Y-%m-%d %H:%M:%S",
        "%Y-%m-%dT%H:%M",
        "%Y-%m-%d %H:%M",
    ] {
        if let Ok(naive) = NaiveDateTime::parse_from_str(timestamp, format) {
            return Local.from_local_datetime(&naive).earliest();
        }
    }
    None
}

// ---------------------------------------------------------------------------
// SearchLine / TreeHelp / LabelInput
// ---------------------------------------------------------------------------

/// `SearchLine.render(width)`。
///
/// 始终输出一行，前缀两个空格：搜索词为空时显示提示语，否则把查询原样（保留大小写）用
/// 强调色显示。整行按 `width` 截断（省略号为 `...`）。
fn search_line_lines(query: &str, width: usize) -> Vec<String> {
    if query.is_empty() {
        vec![truncate_to_width(
            &format!("  {}", fg(ThemeColor::Muted, "Type to search:")),
            width,
            "...",
            false,
        )]
    } else {
        vec![truncate_to_width(
            &format!(
                "  {} {}",
                fg(ThemeColor::Muted, "Type to search:"),
                fg(ThemeColor::Accent, query)
            ),
            width,
            "...",
            false,
        )]
    }
}

/// 帮助项（TS `TREE_HELP_ITEMS`）。
///
/// `keys` 是 keybinding 名字（不是按键字面量，渲染时按当前绑定查询）；`label_first`
/// 决定输出顺序：filters/cycle 先显示标签再显示按键，其余先按键后标签。
struct HelpItem {
    keys: &'static [&'static str],
    label: &'static str,
    label_first: bool,
}

/// 帮助行的条目与顺序是布局契约；改顺序或标签会改变 `renders_ts_golden_tree_layout`
/// 锁定的布局。
const TREE_HELP_ITEMS: &[HelpItem] = &[
    HelpItem {
        keys: &["tui.select.up", "tui.select.down"],
        label: "move",
        label_first: false,
    },
    HelpItem {
        keys: &["tui.editor.cursorLeft", "tui.editor.cursorRight"],
        label: "page",
        label_first: false,
    },
    HelpItem {
        keys: &["app.tree.foldOrUp", "app.tree.unfoldOrDown"],
        label: "branch",
        label_first: false,
    },
    HelpItem {
        keys: &["app.message.copy"],
        label: "copy",
        label_first: false,
    },
    HelpItem {
        keys: &["app.tree.editLabel"],
        label: "label",
        label_first: false,
    },
    HelpItem {
        keys: &["app.tree.toggleLabelTimestamp"],
        label: "label time",
        label_first: false,
    },
    HelpItem {
        keys: &[
            "app.tree.filter.default",
            "app.tree.filter.noTools",
            "app.tree.filter.userOnly",
            "app.tree.filter.labeledOnly",
            "app.tree.filter.all",
        ],
        label: "filters",
        label_first: true,
    },
    HelpItem {
        keys: &[
            "app.tree.filter.cycleForward",
            "app.tree.filter.cycleBackward",
        ],
        label: "cycle",
        label_first: true,
    },
];

/// `formatHelpKeys(keybindings)`。
///
/// 取每个 keybinding 的第一个绑定键（`get_keys(...).first()`），全部缺失时返回空串（调用
/// 方只显示标签）。随后 [`compact_raw_keys`] 合并公共修饰键，`format_key_text` 做显示
/// 格式化，最后用正则把整词的 `pageUp`/`pageDown`/`up`/`down`/`left`/`right` 替换成
/// `pgup`/`pgdn`/方向箭头（`\b` 边界，避免误伤包含这些子串的键名）。
fn format_help_keys(keybindings: &[&str]) -> String {
    let manager = get_keybindings();
    let mut keys: Vec<String> = Vec::new();
    for keybinding in keybindings {
        if let Some(key) = manager.get_keys(keybinding).first() {
            keys.push(key.clone());
        }
    }
    if keys.is_empty() {
        return String::new();
    }
    let compacted = compact_raw_keys(&keys);
    let formatted = format_key_text(&compacted, KeyTextFormatOptions::default());
    let replacements: [(&str, &str); 6] = [
        (r"\bpageUp\b", "pgup"),
        (r"\bpageDown\b", "pgdn"),
        (r"\bup\b", "↑"),
        (r"\bdown\b", "↓"),
        (r"\bleft\b", "←"),
        (r"\bright\b", "→"),
    ];
    let mut result = formatted;
    for (pattern, replacement) in replacements {
        let regex = regex::Regex::new(pattern).expect("valid help-key replacement pattern");
        result = regex.replace_all(&result, replacement).into_owned();
    }
    result
}

/// `compactRawKeys(keys)`。
///
/// 两个及以上按键且修饰键前缀完全一致时，只写一次前缀、后缀用 `/` 连接以压缩帮助行宽度
/// （`ctrl+d`、`ctrl+t` -> `ctrl+d/t`）；否则所有键原样用 `/` 连接。单个键原样返回。
/// 分界点是最后一个 `+`，所以 `ctrl+shift+x` 的前缀是 `ctrl+shift+`。
fn compact_raw_keys(keys: &[String]) -> String {
    if keys.len() == 1 {
        return keys[0].clone();
    }
    let parts: Vec<(String, String)> = keys
        .iter()
        .map(|key| match key.rfind('+') {
            None => (String::new(), key.clone()),
            Some(index) => (key[..=index].to_string(), key[index + 1..].to_string()),
        })
        .collect();
    let prefix = &parts[0].0;
    if !prefix.is_empty() && parts.iter().all(|part| &part.0 == prefix) {
        let suffixes: Vec<&str> = parts.iter().map(|part| part.1.as_str()).collect();
        format!("{prefix}{}", suffixes.join("/"))
    } else {
        keys.join("/")
    }
}

/// `TreeHelp.render(width)`：语义化帮助行的贪心装行。
///
/// 每个条目先转成“按键 标签”或“标签 按键”；然后按分隔符 ` · ` 贪心拼行：当前行放得下
/// 就继续拼，放不下就先把当前行交给 `wrap_text_with_ansi` 换行，再尝试以两个空格缩进
/// 开始新行（缩进本身放不下时省略缩进）。最后整体用 Muted 上色；
/// `width.max(1)` 保证窄终端不会出现 0 宽换行。
fn tree_help_lines(width: usize) -> Vec<String> {
    let items: Vec<String> = TREE_HELP_ITEMS
        .iter()
        .map(|item| {
            let text = format_help_keys(item.keys);
            if text.is_empty() {
                item.label.to_string()
            } else if item.label_first {
                format!("{} {}", item.label, text)
            } else {
                format!("{} {}", text, item.label)
            }
        })
        .collect();

    let available_width = width.max(1);
    let indent = "  ";
    let separator = " · ";
    let mut lines: Vec<String> = Vec::new();
    let mut current_line = String::new();

    for item in &items {
        let candidate = if !current_line.is_empty() {
            format!("{current_line}{separator}{item}")
        } else if visible_width(&format!("{indent}{item}")) <= available_width {
            format!("{indent}{item}")
        } else {
            item.clone()
        };
        if current_line.is_empty() || visible_width(&candidate) <= available_width {
            current_line = candidate;
            continue;
        }
        lines.extend(wrap_text_with_ansi(
            current_line.trim_end_matches(is_js_whitespace),
            available_width,
        ));
        current_line = if visible_width(&format!("{indent}{item}")) <= available_width {
            format!("{indent}{item}")
        } else {
            item.clone()
        };
    }
    if !current_line.is_empty() {
        lines.extend(wrap_text_with_ansi(
            current_line.trim_end_matches(is_js_whitespace),
            available_width,
        ));
    }
    lines
        .into_iter()
        .map(|line| fg(ThemeColor::Muted, &line))
        .collect()
}

/// `LabelInput.handleInput` 的动作。
///
/// `Submit` 的值已经过 JS `trim`；空串表示删除标签，由调用方（
/// `TreeSelectorComponent::handle_label_input`）转成 `None`。本枚举不携带 entry id——输入
/// 组件自己持有它。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LabelInputAction {
    /// 无动作。
    None,
    /// 提交（空串表示删除标签，由调用方转成 `None`）。
    Submit(String),
    /// 取消。
    Cancel,
}

/// 标签输入组件（TS `LabelInput`）。
///
/// 只由 [`TreeSelectorComponent`] 在内部创建；它包装一个 [`Input`] 并记住正在编辑的
/// entry id。`focused` 会转发给内部 `Input`，供输入法定位光标。
pub struct LabelInput {
    entry_id: String,
    input: Input,
    focused: bool,
}

impl LabelInput {
    /// `new(entryId, currentLabel)`。
    ///
    /// 只有非空 `current_label` 会预填输入框（与 TS 的 `if (currentLabel)` 一致）；
    /// `focused` 初始为 `false`，由 [`TreeSelectorComponent`] 在显示时同步。
    pub fn new(entry_id: impl Into<String>, current_label: Option<&str>) -> Self {
        let mut input = Input::new(Default::default());
        if let Some(current_label) = current_label
            && !current_label.is_empty()
        {
            input.set_value(current_label);
        }
        LabelInput {
            entry_id: entry_id.into(),
            input,
            focused: false,
        }
    }

    /// 正在编辑的条目 id（输入过程中不变，提交时由调用方读取）。
    pub fn entry_id(&self) -> &str {
        &self.entry_id
    }

    /// `handleInput`。
    ///
    /// confirm 直接返回 `Submit`，值会先做 JS `trim`（不写入内部 `Input`）；cancel 返回
    /// `Cancel`；其余按键原样转交 [`Input`] 并返回 `None`。这里不调用任何回调——动作由
    /// `TreeSelectorComponent::handle_label_input` 解释。
    pub fn handle_input_action(&mut self, key_data: &str) -> LabelInputAction {
        let kb = get_keybindings();
        if kb.matches(key_data, "tui.select.confirm") {
            let value = js_trim(self.input.get_value()).to_string();
            LabelInputAction::Submit(value)
        } else if kb.matches(key_data, "tui.select.cancel") {
            LabelInputAction::Cancel
        } else {
            self.input.handle_input(key_data);
            LabelInputAction::None
        }
    }

    /// `render(width)`。
    ///
    /// 三行结构：两空格缩进的提示 `Label (empty to remove):`、内部 [`Input`] 的渲染行
    /// （输入宽度为 `width - 2`，再统一缩进）、以及 save/cancel 的按键提示。每行都截断到
    /// `width`（省略号 `...`）。
    pub fn render(&mut self, width: usize) -> Vec<String> {
        let indent = "  ";
        let available_width = width.saturating_sub(indent.len());
        let mut lines: Vec<String> = Vec::new();
        lines.push(truncate_to_width(
            &format!(
                "{indent}{}",
                fg(ThemeColor::Muted, "Label (empty to remove):")
            ),
            width,
            "...",
            false,
        ));
        lines.extend(
            self.input
                .render(available_width)
                .into_iter()
                .map(|line| truncate_to_width(&format!("{indent}{line}"), width, "...", false)),
        );
        lines.push(truncate_to_width(
            &format!(
                "{indent}{}  {}",
                key_hint("tui.select.confirm", "save"),
                key_hint("tui.select.cancel", "cancel")
            ),
            width,
            "...",
            false,
        ));
        lines
    }
}

impl Component for LabelInput {
    fn render(&mut self, width: usize) -> Vec<String> {
        LabelInput::render(self, width)
    }

    fn handle_input(&mut self, data: &str) {
        // 独立使用 `Component` 接口时动作被丢弃；组件路径调用的是 `handle_input_action`。
        let _ = self.handle_input_action(data);
    }

    fn as_focusable(&mut self) -> Option<&mut dyn Focusable> {
        Some(self)
    }
}

impl Focusable for LabelInput {
    fn focused(&self) -> bool {
        self.focused
    }

    fn set_focused(&mut self, focused: bool) {
        // 同步给内部 Input，供输入法定位光标（对应 TS 的 focused setter）。
        self.focused = focused;
        self.input.set_focused(focused);
    }
}

// ---------------------------------------------------------------------------
// TreeSelectorComponent
// ---------------------------------------------------------------------------

/// `/tree` 会话树选择器（TS `TreeSelectorComponent`）。
///
/// 组合根：持有 [`TreeList`]（树视图）与可选的 [`LabelInput`]（标签编辑态）。构造时把
/// `on_select`/`on_cancel` 装到 `TreeList` 的回调字段上，`on_copy`/`on_label_change` 留在
/// 组件层。空树时设置一个惰性取消截止时间（不是定时器线程）：下一次 `handle_input` 或
/// `render` 发现截止时间已过就调用构造时的原始 `on_cancel`。
pub struct TreeSelectorComponent {
    /// 树视图与输入处理器；即使处于标签编辑态也保留（只是不渲染）。
    tree_list: TreeList,
    /// 标签编辑态；`Some` 时所有输入都交给它，渲染也替代树列表。
    label_input: Option<LabelInput>,
    /// 标签提交后的持久化回调；组件已先更新本地 `TreeList`。
    on_label_change: Option<LabelEditCallback>,
    /// 空树定时取消用的原始 `onCancel`（TS 的 `setTimeout(() => onCancel(), 100)` 直接捕获
    /// 构造参数，替换 `getTreeList().onCancel` 不会影响它）。
    empty_cancel: Rc<RefCell<VoidCallback>>,
    /// 复制回调（宿主负责写剪贴板）。
    pub on_copy: Option<OptionalStringCallback>,
    /// 焦点状态；只会转发给 `label_input`，不会下发给 `tree_list`。
    focused: bool,
    /// 空树自动取消的截止时间；非空树为 `None`，触发一次后清空。
    ///
    /// 没有后台定时器：`handle_input`/`render` 是仅有的两个检查点。
    empty_auto_cancel_at: Option<Instant>,
}

impl TreeSelectorComponent {
    /// `new(tree, currentLeafId, terminalHeight, onSelect, onCancel, onLabelChange?,
    ///      initialSelectedId?, initialFilterMode?)`。
    ///
    /// `max_visible_lines = max(terminal_height / 2, 5)`（TS 的
    /// `Math.max(5, Math.floor(h / 2))`）。回调接线与 TS 构造器一致：`on_select` 存进
    /// `tree_list.on_select`；`on_cancel` 同时被 `tree_list.on_cancel`（宿主可覆盖）和空树
    /// 自动取消闭包捕获（始终是构造时的原始回调）。`on_label_change` 只在标签提交后调用，
    /// 可为 `None`。空树（`tree.is_empty()`）会设置 100ms 的惰性取消截止时间；非空树不设置。
    /// `tree` 的所有权转移给 [`TreeList`]，条目会被 clone 进 arena。
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        tree: Vec<SessionTreeNode>,
        current_leaf_id: Option<String>,
        terminal_height: usize,
        on_select: StringCallback,
        on_cancel: VoidCallback,
        on_label_change: Option<LabelEditCallback>,
        initial_selected_id: Option<String>,
        initial_filter_mode: Option<FilterMode>,
    ) -> Self {
        let max_visible_lines = (terminal_height / 2).max(5);
        let empty = tree.is_empty();
        let mut tree_list = TreeList::new(
            tree,
            current_leaf_id,
            max_visible_lines,
            initial_selected_id,
            initial_filter_mode,
        );
        // TS 构造器把回调直接装到 `TreeList` 的公开字段上；宿主通过 `getTreeList()`
        // 覆盖它们会立即改变行为。`onCancel` 另外被 setTimeout 闭包直接捕获，
        // 所以空树自动取消始终调用构造时的原始回调。
        tree_list.on_select = Some(on_select);
        let empty_cancel = Rc::new(RefCell::new(on_cancel));
        {
            let shared = Rc::clone(&empty_cancel);
            tree_list.on_cancel = Some(Box::new(move || (shared.borrow_mut())()));
        }
        TreeSelectorComponent {
            tree_list,
            label_input: None,
            on_label_change,
            empty_cancel,
            on_copy: None,
            focused: false,
            empty_auto_cancel_at: if empty {
                Some(Instant::now() + std::time::Duration::from_millis(100))
            } else {
                None
            },
        }
    }

    /// `getTreeList()`。
    ///
    /// 返回内部 [`TreeList`] 的可变引用：宿主/测试可以覆盖其 `on_*` 回调或读取选择状态。
    /// `TreeSelectorComponent::handle_input` 会优先调用这些字段，因此覆盖立即生效（测试
    /// `tree_list_callback_overrides_redirect_component_input` 锁定该行为）。注意空树自动
    /// 取消捕获的是构造时的原始 `on_cancel`，覆盖 `on_cancel` 不影响它。
    pub fn get_tree_list(&mut self) -> &mut TreeList {
        &mut self.tree_list
    }

    /// `showLabelInput(entryId, currentLabel)`。
    ///
    /// 创建 [`LabelInput`] 并同步组件的 focus 状态；不修改 `TreeList` 的选择或折叠。
    fn show_label_input(&mut self, entry_id: &str, current_label: Option<&str>) {
        let mut label_input = LabelInput::new(entry_id, current_label);
        label_input.set_focused(self.focused);
        self.label_input = Some(label_input);
    }

    /// `hideLabelInput()`。
    ///
    /// 丢弃当前 [`LabelInput`]（提交与取消都走这里）；`TreeList` 状态保持不变，下次渲染
    /// 重新显示树列表。
    fn hide_label_input(&mut self) {
        self.label_input = None;
    }

    /// 检查并触发空树自动取消。
    ///
    /// 返回 `true` 表示本次调用刚触发了回调，调用方应放弃继续处理本次输入/渲染结果。
    /// 截止时间只结算一次：触发后置为 `None`，避免重复调用 `on_cancel`。回调是宿主代码、
    /// 同步执行，因此 `handle_input` 在分发按键前先检查，`render` 在产出所有行之后再检查，
    /// 避免在行内容尚未生成时重入。
    fn maybe_auto_cancel(&mut self) -> bool {
        if let Some(deadline) = self.empty_auto_cancel_at
            && Instant::now() >= deadline
        {
            self.empty_auto_cancel_at = None;
            (self.empty_cancel.borrow_mut())();
            return true;
        }
        false
    }

    /// 处理标签编辑态的按键。
    ///
    /// 返回 `true` 表示按键已被消费（无论动作是提交、取消还是继续编辑）；仅在
    /// `label_input` 存在时调用。提交时先更新本地节点标签，再把提交值交给
    /// `on_label_change`（`LabelInput` 已 trim；空串在这里转成 `None` 表示删除），最后
    /// 退出编辑态；取消只退出编辑态。
    fn handle_label_input(&mut self, data: &str) -> bool {
        let Some(label_input) = self.label_input.as_mut() else {
            return false;
        };
        match label_input.handle_input_action(data) {
            LabelInputAction::None => true,
            LabelInputAction::Submit(value) => {
                let entry_id = label_input.entry_id().to_string();
                let label = if value.is_empty() { None } else { Some(value) };
                self.tree_list
                    .update_node_label(&entry_id, label.clone(), None);
                if let Some(callback) = &mut self.on_label_change {
                    callback(&entry_id, label.as_deref());
                }
                self.hide_label_input();
                true
            }
            LabelInputAction::Cancel => {
                self.hide_label_input();
                true
            }
        }
    }

    /// `handleInput(keyData)`。
    ///
    /// 处理顺序（与 TS 的组件一致）：
    /// 1. 空树截止时间已过：先触发 `on_cancel` 并直接返回（TS 的 `setTimeout` 会先于本次
    ///    输入执行）；
    /// 2. 标签编辑态：所有按键交给 [`LabelInput`]，不再进入树；
    /// 3. 否则交给 `TreeList::handle_input_action` 并分发动作：`Select`/`Cancel` 只调用
    ///    `tree_list.on_*`（`None` 时静默丢弃）；`Copy` 先看 `tree_list.on_copy`，否则回退
    ///    到组件的 `on_copy`；`EditLabel` 先看 `tree_list.on_label_edit`，否则打开内置的
    ///    [`LabelInput`]。
    ///
    /// 动作回调都是同步调用，可能重入宿主逻辑。
    pub fn handle_input(&mut self, key_data: &str) {
        // 截止时间已过的空树：TS 的定时器会先于本次输入取消选择器，输入不再分发。
        if self.maybe_auto_cancel() {
            return;
        }
        if self.label_input.is_some() {
            self.handle_label_input(key_data);
            return;
        }
        // TS 把组件回调装在 `TreeList` 的公开字段上，输入由 `TreeList.handleInput` 分发。
        // 这里按同样优先级分发：`getTreeList().on*` 的覆盖优先于组件自身的默认行为。
        match self.tree_list.handle_input_action(key_data) {
            TreeAction::None => {}
            TreeAction::Select(entry_id) => {
                if let Some(callback) = &mut self.tree_list.on_select {
                    callback(&entry_id);
                }
            }
            TreeAction::Cancel => {
                if let Some(callback) = &mut self.tree_list.on_cancel {
                    callback();
                }
            }
            TreeAction::Copy(text) => {
                if let Some(callback) = &mut self.tree_list.on_copy {
                    callback(text.as_deref());
                } else if let Some(callback) = &mut self.on_copy {
                    callback(text.as_deref());
                }
            }
            TreeAction::EditLabel(entry_id, current_label) => {
                if let Some(callback) = &mut self.tree_list.on_label_edit {
                    callback(&entry_id, current_label.as_deref());
                } else {
                    self.show_label_input(&entry_id, current_label.as_deref());
                }
            }
        }
    }

    /// `render(width)`（Container 布局逐行展开）。
    ///
    /// 输出顺序：空行、上边框、标题 `Session Tree`、语义化帮助行、搜索行、下边框、空行，
    /// 然后是有标签编辑态时渲染 [`LabelInput`]、否则渲染 [`TreeList`]，最后空行与下边框。
    /// TS 在显示标签输入时会 `treeContainer.clear()`，所以两者互斥而不是叠加。
    /// 所有行生成完后才调用 `maybe_auto_cancel`：空树取消回调不会打断行内容的产出。
    pub fn render(&mut self, width: usize) -> Vec<String> {
        let mut lines: Vec<String> = Vec::new();
        lines.push(String::new());
        lines.extend(DynamicBorder::default().render(width));
        let mut title = Text::with_padding(bold("  Session Tree"), 1, 0);
        lines.extend(title.render(width));
        lines.extend(tree_help_lines(width));
        lines.extend(search_line_lines(self.tree_list.get_search_query(), width));
        lines.extend(DynamicBorder::default().render(width));
        lines.push(String::new());
        // TS `showLabelInput` 会清空 treeContainer（树被临时摘掉）；`hideLabelInput` 恢复。
        if let Some(label_input) = self.label_input.as_mut() {
            lines.extend(label_input.render(width));
        } else {
            lines.extend(self.tree_list.render(width));
        }
        lines.push(String::new());
        lines.extend(DynamicBorder::default().render(width));
        // 惰性截止时间在渲染完成后结算，避免用户回调在行内容尚未产出时重入。
        self.maybe_auto_cancel();
        lines
    }
}

impl Component for TreeSelectorComponent {
    fn render(&mut self, width: usize) -> Vec<String> {
        TreeSelectorComponent::render(self, width)
    }

    fn handle_input(&mut self, data: &str) {
        TreeSelectorComponent::handle_input(self, data);
    }

    fn invalidate(&mut self) {
        // TS 继承 Container.invalidate：逐个失效子组件。
        self.tree_list.invalidate();
        if let Some(label_input) = self.label_input.as_mut() {
            label_input.invalidate();
        }
    }

    fn as_focusable(&mut self) -> Option<&mut dyn Focusable> {
        Some(self)
    }
}

impl Focusable for TreeSelectorComponent {
    fn focused(&self) -> bool {
        self.focused
    }

    fn set_focused(&mut self, focused: bool) {
        self.focused = focused;
        // TS 只下发给 labelInput，不下发给 treeList。
        if let Some(label_input) = self.label_input.as_mut() {
            label_input.set_focused(focused);
        }
    }
}

// 这里只覆盖两个不依赖主题/keybindings 的纯函数；组件行为（过滤、折叠、渲染 golden、
// 回调覆盖、Unknown 条目语义）在 crates/kiana-interactive/tests/components_tree_selector.rs。
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compacts_raw_keys_with_shared_prefix() {
        assert_eq!(
            compact_raw_keys(&["ctrl+d".to_string(), "ctrl+t".to_string()]),
            "ctrl+d/t"
        );
        assert_eq!(
            compact_raw_keys(&["ctrl+left".to_string(), "alt+left".to_string()]),
            "ctrl+left/alt+left"
        );
        assert_eq!(compact_raw_keys(&["left".to_string()]), "left");
    }

    #[test]
    fn normalizes_display_text() {
        assert_eq!(normalize_display("a\nb\tc  "), "a b c");
    }
}
