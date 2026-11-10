//! keybindings.ts。
//!
//! 全局键位注册表。下游包通过 TS 的 declaration merging 增加键位（`app.*`）；Rust 侧
//! [`KeybindingsManager`] 接受任意 `String` 定义表，因此扩展无需改类型。
//!
//! 全局单例（TS `globalKeybindings`）落在 thread-local `RefCell` 里；测试可以替换它。
//! [`get_keybindings`] 返回 `&'static KeybindingsManager`（与 TS 的
//! `getKeybindings(): KeybindingsManager` 语义一致）：安装时把管理器泄漏到进程生命周期，
//! 因为 TUI 是单线程且单例只在启动、配置重载与测试里替换。
//!
//! ## 数据结构
//!
//! [`KeybindingsManager`] 持有四份状态：
//!
//! - `definitions`：键位 id → 默认键与描述（[`KeybindingDefinitions`] 是 `IndexMap`，
//!   **顺序即源码顺序**，决定 `get_resolved_bindings` 的键顺序）；
//! - `user_bindings`：用户配置（[`KeybindingsConfig`]）。键不存在 = 用默认值；显式空列表 =
//!   解绑；
//! - `keys_by_id`：`rebuild()` 的产物，每个定义 id 一条最终键列表；
//! - `conflicts`：只在**用户绑定之间**检测到的重复键。
//!
//! ## rebuild 的执行顺序
//!
//! 1. 清空 `keys_by_id` 与 `conflicts`（全量重建，没有增量路径）；
//! 2. 按用户配置的插入顺序遍历，跳过不在 `definitions` 里的 id；每个键经
//!    [`KeyIdList::normalize`] 去重后登记到“键 → 声明者列表”；
//! 3. 按登记的插入顺序，把声明者多于一个的键写成 [`KeybindingConflict`]；
//! 4. 按 `definitions` 顺序，为每个 id 选键：用户配置存在就用它（空列表即解绑），否则用
//!    默认键；两边都再做一次去重。
//!
//! 因此用户配置里的未知 id 会被**静默忽略**（见单元测试
//! `ignores_unknown_ids_and_unbinds_with_an_explicit_empty_list`）；默认表里不同 id 共用
//! 同一个键（`up` 同时属于 `tui.editor.cursorUp` 与 `tui.select.up`）不算冲突。
//!
//! ## 全局单例与线程
//!
//! [`set_keybindings`] / [`get_keybindings`] 的状态在 `thread_local!` 的 `RefCell` 里，不是
//! 进程级共享：在 A 线程安装的管理器，B 线程看不到（会各自懒初始化默认表）。
//! [`set_keybindings`] 用 `Box::leak` 换 `&'static`，旧管理器不会被释放；它适合启动、配置
//! 重载与测试重置这类低频替换点，不应在每次按键的热路径里反复安装。
//!
//! ## 被谁调用
//!
//! - `crate::alt_screen` 的备用屏滚动/搜索分发，以及
//!   `crate::components::{editor,input,select_list,settings_list,cancellable_loader}` 的输入
//!   处理：通过 [`get_keybindings`] 取管理器后调用 [`KeybindingsManager::matches`]；
//! - `kiana-extensions` 的 `keyHint`：调用 [`KeybindingsManager::get_keys`] 展示按键提示；
//! - `kiana-config` 的键位模块：以 [`tui_keybindings`] 为基底追加 43 个 `app.*` 定义，并用
//!   自己的管理器包装本类型。**本模块的默认表只有 47 个 `tui.*` id**，不含 `app.*`。

use std::cell::RefCell;
use std::sync::LazyLock;

use indexmap::IndexMap;

use crate::keys::{KeyId, matches_key};

// =============================================================================
// 键位 id 与定义
// =============================================================================

/// `Keybindings` 接口的 47 个键位 id（**【逐字】**顺序即源码顺序）。
///
/// 这个数组同时锁定默认表的**键集合与键顺序**：单元测试
/// `default_table_matches_the_interface_id_set` 与
/// `resolves_single_keys_to_single_values_in_definition_order` 会把它与 [`tui_keybindings`]
/// 的实际键顺序逐项比较。不要按字母排序。
pub const KEYBINDING_IDS: [&str; 47] = [
    // 编辑器导航与编辑（23）
    "tui.editor.cursorUp",
    "tui.editor.cursorDown",
    "tui.editor.historyPrevious",
    "tui.editor.historyNext",
    "tui.editor.cursorLeft",
    "tui.editor.cursorRight",
    "tui.editor.cursorWordLeft",
    "tui.editor.cursorWordRight",
    "tui.editor.cursorLineStart",
    "tui.editor.cursorLineEnd",
    "tui.editor.jumpForward",
    "tui.editor.jumpBackward",
    "tui.editor.pageUp",
    "tui.editor.pageDown",
    "tui.editor.deleteCharBackward",
    "tui.editor.deleteCharForward",
    "tui.editor.deleteWordBackward",
    "tui.editor.deleteWordForward",
    "tui.editor.deleteToLineStart",
    "tui.editor.deleteToLineEnd",
    "tui.editor.yank",
    "tui.editor.yankPop",
    "tui.editor.undo",
    // 通用输入（4）
    "tui.input.newLine",
    "tui.input.submit",
    "tui.input.tab",
    "tui.input.copy",
    // 通用选择（6）
    "tui.select.up",
    "tui.select.down",
    "tui.select.pageUp",
    "tui.select.pageDown",
    "tui.select.confirm",
    "tui.select.cancel",
    // 备用屏视口导航（14）
    "tui.altScreen.pageUp",
    "tui.altScreen.pageDown",
    "tui.altScreen.halfPageUp",
    "tui.altScreen.halfPageDown",
    "tui.altScreen.lineUp",
    "tui.altScreen.lineDown",
    "tui.altScreen.previousPrompt",
    "tui.altScreen.nextPrompt",
    "tui.altScreen.search",
    "tui.altScreen.searchNext",
    "tui.altScreen.searchPrevious",
    "tui.altScreen.searchClose",
    "tui.altScreen.top",
    "tui.altScreen.bottom",
];

/// 键位 id（TS `Keybinding = keyof Keybindings`）。
pub type Keybinding = &'static str;

/// 一个键位 id 可以绑定一个键或一组键（TS `KeyId | KeyId[]`）。
///
/// 两种形态都用于 [`KeybindingDefinition::default_keys`] 与用户配置；
/// [`KeyIdList::normalize`] 把它们统一成去重后的 `Vec<KeyId>`。`Many` 保留调用方给出的
/// 顺序，`get_keys` / `get_resolved_bindings` 会原样返回这个顺序。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyIdList {
    /// 单个键。
    One(KeyId),
    /// 一组键（顺序即优先级）。
    Many(Vec<KeyId>),
}

impl From<KeyId> for KeyIdList {
    fn from(key: KeyId) -> Self {
        KeyIdList::One(key)
    }
}

impl From<&str> for KeyIdList {
    fn from(key: &str) -> Self {
        KeyIdList::One(key.to_string())
    }
}

impl From<Vec<KeyId>> for KeyIdList {
    fn from(keys: Vec<KeyId>) -> Self {
        KeyIdList::Many(keys)
    }
}

impl From<Vec<&str>> for KeyIdList {
    fn from(keys: Vec<&str>) -> Self {
        KeyIdList::Many(keys.into_iter().map(str::to_string).collect())
    }
}

impl<const N: usize> From<[&str; N]> for KeyIdList {
    fn from(keys: [&str; N]) -> Self {
        KeyIdList::Many(keys.iter().map(|key| (*key).to_string()).collect())
    }
}

impl KeyIdList {
    /// `normalizeKeys(keys)`：去重并保持首次出现顺序。
    ///
    /// `One` 与 `Many` 按同一规则处理；始终返回新的 `Vec`，调用方修改它不会影响管理器。
    pub fn normalize(&self) -> Vec<KeyId> {
        let list: &[KeyId] = match self {
            KeyIdList::One(key) => std::slice::from_ref(key),
            KeyIdList::Many(keys) => keys,
        };
        let mut seen: Vec<&str> = Vec::with_capacity(list.len());
        let mut result = Vec::with_capacity(list.len());
        for key in list {
            if seen.contains(&key.as_str()) {
                continue;
            }
            seen.push(key);
            result.push(key.clone());
        }
        result
    }
}

/// 单个键位定义（TS `KeybindingDefinition`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeybindingDefinition {
    /// 默认键；`[]` 表示默认不绑定。
    pub default_keys: KeyIdList,
    /// 人类可读描述；`None` 对应 TS 的可选字段缺失。
    pub description: Option<String>,
}

impl KeybindingDefinition {
    /// 构造一个带描述的默认定义。
    ///
    /// `default_keys` 经 `Into<KeyIdList>` 转换，可传单个 `&str`、`Vec<&str>` 或数组；
    /// `description` 总是 `Some`。
    pub fn new(default_keys: impl Into<KeyIdList>, description: &str) -> Self {
        KeybindingDefinition {
            default_keys: default_keys.into(),
            description: Some(description.to_string()),
        }
    }
}

/// 键位定义表（TS `KeybindingDefinitions`）。
///
/// 必须是 `IndexMap`：[`KeybindingsManager::get_resolved_bindings`] 的键顺序直接来自这里的
/// 插入顺序，换成 `HashMap` 会让结果顺序不稳定。
pub type KeybindingDefinitions = IndexMap<String, KeybindingDefinition>;

/// 用户键位配置（TS `KeybindingsConfig`）。
///
/// TS 的 `undefined` 表示"使用默认值"，对应 Rust 的**键不存在**；显式空数组表示解绑。
/// 同样是 `IndexMap`：遍历顺序决定 [`KeybindingConflict`] 列表的顺序。
pub type KeybindingsConfig = IndexMap<String, KeyIdList>;

/// 用户绑定之间的直接冲突（TS `KeybindingConflict`）。
///
/// 只统计用户配置里同一个键被多个**已知**键位声明的情况；默认值之间的重复（例如 `up`
/// 同时出现在两个默认项里）不产生冲突。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeybindingConflict {
    /// 被多个键位声明的键。
    pub key: KeyId,
    /// 声明该键的键位 id（按用户配置顺序；同一 id 重复声明只记一次）。
    pub keybindings: Vec<String>,
}

/// `TUI_KEYBINDINGS` 默认表（**【逐字】**，`as const satisfies KeybindingDefinitions`）。
///
/// 保持源码顺序：`getResolvedBindings()` 的键顺序依赖它（冲突列表的顺序则来自用户配置的
/// 遍历顺序，见 `rebuild`）。
///
/// 每次调用都会新建一张表；进程内共享的只读副本见 [`tui_keybindings_static`]。表里只有
/// 47 个 `tui.*` id，`app.*` 等扩展 id 由调用方（例如 `kiana-config`）自行追加。
pub fn tui_keybindings() -> KeybindingDefinitions {
    let mut table = KeybindingDefinitions::new();
    let mut add = |id: &str, keys: KeyIdList, description: &str| {
        table.insert(
            id.to_string(),
            KeybindingDefinition {
                default_keys: keys,
                description: Some(description.to_string()),
            },
        );
    };
    add("tui.editor.cursorUp", "up".into(), "Move cursor up");
    add("tui.editor.cursorDown", "down".into(), "Move cursor down");
    add(
        "tui.editor.historyPrevious",
        Vec::<KeyId>::new().into(),
        "Select previous prompt history entry",
    );
    add(
        "tui.editor.historyNext",
        Vec::<KeyId>::new().into(),
        "Select next prompt history entry",
    );
    add(
        "tui.editor.cursorLeft",
        vec!["left", "ctrl+b"].into(),
        "Move cursor left",
    );
    add(
        "tui.editor.cursorRight",
        vec!["right", "ctrl+f"].into(),
        "Move cursor right",
    );
    add(
        "tui.editor.cursorWordLeft",
        vec!["alt+left", "ctrl+left", "alt+b"].into(),
        "Move cursor word left",
    );
    add(
        "tui.editor.cursorWordRight",
        vec!["alt+right", "ctrl+right", "alt+f"].into(),
        "Move cursor word right",
    );
    add(
        "tui.editor.cursorLineStart",
        vec!["home", "ctrl+a"].into(),
        "Move to line start",
    );
    add(
        "tui.editor.cursorLineEnd",
        vec!["end", "ctrl+e"].into(),
        "Move to line end",
    );
    add(
        "tui.editor.jumpForward",
        "ctrl+]".into(),
        "Jump forward to character",
    );
    add(
        "tui.editor.jumpBackward",
        "ctrl+alt+]".into(),
        "Jump backward to character",
    );
    add(
        "tui.editor.pageUp",
        vec!["pageUp", "ctrl+pageUp"].into(),
        "Page up",
    );
    add(
        "tui.editor.pageDown",
        vec!["pageDown", "ctrl+pageDown"].into(),
        "Page down",
    );
    add(
        "tui.editor.deleteCharBackward",
        "backspace".into(),
        "Delete character backward",
    );
    add(
        "tui.editor.deleteCharForward",
        vec!["delete", "ctrl+d"].into(),
        "Delete character forward",
    );
    add(
        "tui.editor.deleteWordBackward",
        vec!["ctrl+w", "alt+backspace"].into(),
        "Delete word backward",
    );
    add(
        "tui.editor.deleteWordForward",
        vec!["alt+d", "alt+delete"].into(),
        "Delete word forward",
    );
    add(
        "tui.editor.deleteToLineStart",
        "ctrl+u".into(),
        "Delete to line start",
    );
    add(
        "tui.editor.deleteToLineEnd",
        "ctrl+k".into(),
        "Delete to line end",
    );
    add("tui.editor.yank", "ctrl+y".into(), "Yank");
    add("tui.editor.yankPop", "alt+y".into(), "Yank pop");
    add("tui.editor.undo", "ctrl+-".into(), "Undo");
    add(
        "tui.input.newLine",
        vec!["shift+enter", "ctrl+j"].into(),
        "Insert newline",
    );
    add("tui.input.submit", "enter".into(), "Submit input");
    add("tui.input.tab", "tab".into(), "Tab / autocomplete");
    add("tui.input.copy", "ctrl+c".into(), "Copy selection");
    add("tui.select.up", "up".into(), "Move selection up");
    add("tui.select.down", "down".into(), "Move selection down");
    add("tui.select.pageUp", "pageUp".into(), "Selection page up");
    add(
        "tui.select.pageDown",
        "pageDown".into(),
        "Selection page down",
    );
    add("tui.select.confirm", "enter".into(), "Confirm selection");
    // 这些键位在全屏模式下**故意**遮蔽编辑器的同名未修饰键。
    add(
        "tui.select.cancel",
        vec!["escape", "ctrl+c"].into(),
        "Cancel selection",
    );
    add(
        "tui.altScreen.pageUp",
        "pageUp".into(),
        "Scroll viewport up one page",
    );
    add(
        "tui.altScreen.pageDown",
        "pageDown".into(),
        "Scroll viewport down one page",
    );
    add(
        "tui.altScreen.halfPageUp",
        Vec::<KeyId>::new().into(),
        "Scroll viewport up half a page",
    );
    add(
        "tui.altScreen.halfPageDown",
        Vec::<KeyId>::new().into(),
        "Scroll viewport down half a page",
    );
    add(
        "tui.altScreen.lineUp",
        Vec::<KeyId>::new().into(),
        "Scroll viewport up one line",
    );
    add(
        "tui.altScreen.lineDown",
        Vec::<KeyId>::new().into(),
        "Scroll viewport down one line",
    );
    add(
        "tui.altScreen.previousPrompt",
        vec!["ctrl+shift+up", "ctrl+up"].into(),
        "Jump to previous semantic prompt",
    );
    add(
        "tui.altScreen.nextPrompt",
        vec!["ctrl+shift+down", "ctrl+down"].into(),
        "Jump to next semantic prompt",
    );
    add(
        "tui.altScreen.search",
        "ctrl+shift+f".into(),
        "Search the primary scroll view",
    );
    add(
        "tui.altScreen.searchNext",
        vec!["enter", "ctrl+g"].into(),
        "Select the next search match",
    );
    add(
        "tui.altScreen.searchPrevious",
        vec!["shift+enter", "ctrl+shift+g"].into(),
        "Select the previous search match",
    );
    add(
        "tui.altScreen.searchClose",
        "escape".into(),
        "Close transcript search",
    );
    add(
        "tui.altScreen.top",
        "ctrl+home".into(),
        "Scroll viewport to top",
    );
    add(
        "tui.altScreen.bottom",
        "ctrl+end".into(),
        "Scroll viewport to bottom",
    );
    table
}

/// `TUI_KEYBINDINGS` 的静态副本（首次访问时构造一次）。
///
/// 定义表在进程内不变，`LazyLock` 避免每次 `get_keybindings()` 都重建 47 项。
static TUI_KEYBINDINGS_STATIC: LazyLock<KeybindingDefinitions> = LazyLock::new(tui_keybindings);

/// 默认键位表（TS `TUI_KEYBINDINGS`）。
///
/// 返回共享引用；需要修改时先 `.clone()`（[`KeybindingsManager::with_defaults`] 就是这么做的）。
pub fn tui_keybindings_static() -> &'static KeybindingDefinitions {
    &TUI_KEYBINDINGS_STATIC
}

// =============================================================================
// KeybindingsManager
// =============================================================================

/// 键位注册表（TS `KeybindingsManager`）。
///
/// 不变量（由 [`KeybindingsManager::new`] / `rebuild` 维护）：`keys_by_id` 覆盖且仅覆盖
/// `definitions` 中的 id；`conflicts` 只来自用户配置。`Clone` 是深拷贝，克隆体与原体互不影响。
#[derive(Debug, Clone)]
pub struct KeybindingsManager {
    definitions: KeybindingDefinitions,
    user_bindings: KeybindingsConfig,
    keys_by_id: IndexMap<String, Vec<KeyId>>,
    conflicts: Vec<KeybindingConflict>,
}

impl KeybindingsManager {
    /// `new KeybindingsManager(definitions, userBindings = {})`。
    ///
    /// 构造末尾立即执行一次 `rebuild()`，因此返回时 `keys_by_id` 与 `conflicts` 已经可用。
    pub fn new(definitions: KeybindingDefinitions, user_bindings: KeybindingsConfig) -> Self {
        let mut manager = KeybindingsManager {
            definitions,
            user_bindings,
            keys_by_id: IndexMap::new(),
            conflicts: Vec::new(),
        };
        manager.rebuild();
        manager
    }

    /// 用默认表构造（`new KeybindingsManager(TUI_KEYBINDINGS)`）。
    ///
    /// 会克隆静态默认表，用户配置为空。
    pub fn with_defaults() -> Self {
        KeybindingsManager::new(tui_keybindings_static().clone(), KeybindingsConfig::new())
    }

    /// `rebuild()`：**精确顺序**见 `spec/tui-core.md` §2.6.3。
    ///
    /// 全量重建 `keys_by_id` 与 `conflicts`（没有增量路径）；调用方负责在此之前写好
    /// `definitions` / `user_bindings`，入口是 [`KeybindingsManager::new`] 与
    /// [`KeybindingsManager::set_user_bindings`]。
    fn rebuild(&mut self) {
        self.keys_by_id.clear();
        self.conflicts.clear();

        // userClaims：Map<KeyId, Set<Keybinding>>，按用户配置的插入顺序。
        let mut user_claims: IndexMap<KeyId, Vec<String>> = IndexMap::new();
        for (keybinding, keys) in &self.user_bindings {
            // 未知 id（不在 definitions 里）静默丢弃：它既不会产生冲突，也不会出现在结果表。
            if !self.definitions.contains_key(keybinding) {
                continue;
            }
            for key in keys.normalize() {
                let claimants = user_claims.entry(key).or_default();
                if !claimants.contains(keybinding) {
                    claimants.push(keybinding.clone());
                }
            }
        }
        // 冲突只统计用户绑定之间；默认表里不同 id 复用同一个键是允许的，不在这里报。
        for (key, keybindings) in user_claims {
            if keybindings.len() > 1 {
                self.conflicts.push(KeybindingConflict { key, keybindings });
            }
        }

        // 遍历 definitions 的插入顺序：IndexMap 保序，get_resolved_bindings 的键顺序即来自这里。
        for (id, definition) in &self.definitions {
            // 用户配置存在即覆盖（空列表 = 解绑）；否则回退到默认键。
            let keys = match self.user_bindings.get(id) {
                Some(user_keys) => user_keys.normalize(),
                None => definition.default_keys.normalize(),
            };
            self.keys_by_id.insert(id.clone(), keys);
        }
    }

    /// `matches(data, keybinding)`：该键位的任一键匹配即 true。
    ///
    /// `data` 必须是**一条完整**的按键序列（与 [`crate::keys::matches_key`] 的约定一致，不做
    /// 前缀匹配）。匹配依赖 Kitty 协议状态，而该状态是线程级 `thread_local`：同一个管理器在
    /// 不同线程、或同一线程协议状态改变后，对同一条输入可能给出不同结果。未知 id 返回 `false`。
    pub fn matches(&self, data: &str, keybinding: &str) -> bool {
        match self.keys_by_id.get(keybinding) {
            Some(keys) => keys.iter().any(|key| matches_key(data, key)),
            None => false,
        }
    }

    /// `getKeys(keybinding)`：返回副本。
    ///
    /// 未知 id，以及被显式空列表解绑的 id，都返回空 `Vec`；调用方常用 `is_empty()` 判断
    /// “默认不绑定”。
    pub fn get_keys(&self, keybinding: &str) -> Vec<KeyId> {
        self.keys_by_id.get(keybinding).cloned().unwrap_or_default()
    }

    /// `getDefinition(keybinding)`。
    ///
    /// 返回表内引用（不是副本）；未知 id 返回 `None`。
    pub fn get_definition(&self, keybinding: &str) -> Option<&KeybindingDefinition> {
        self.definitions.get(keybinding)
    }

    /// `getConflicts()`：深拷贝副本。
    ///
    /// 顺序是冲突键在用户配置中**首次出现**的顺序；每次 `rebuild()` 都会全部重算。
    pub fn get_conflicts(&self) -> Vec<KeybindingConflict> {
        self.conflicts.clone()
    }

    /// `setUserBindings(config)`：替换并重建。
    ///
    /// 是整体替换而非合并：新配置里没有出现的 id 会回退到默认键。
    pub fn set_user_bindings(&mut self, user_bindings: KeybindingsConfig) {
        self.user_bindings = user_bindings;
        self.rebuild();
    }

    /// `getUserBindings()`：浅拷贝。
    ///
    /// Rust 的 `Clone` 会把 `KeyIdList` 内的 `Vec` 一并复制，所以修改返回值不会影响管理器。
    pub fn get_user_bindings(&self) -> KeybindingsConfig {
        self.user_bindings.clone()
    }

    /// `getResolvedBindings()`：每个定义 id 一条，1 个键时给出单值。
    ///
    /// 键顺序等于 `definitions` 的插入顺序；未知 id 不会出现（只遍历 `definitions`）。
    /// 0 个键给 `Many(vec![])` 而不是省略该 id——测试
    /// `resolves_single_keys_to_single_values_in_definition_order` 锁定了这个形状。
    pub fn get_resolved_bindings(&self) -> KeybindingsConfig {
        let mut resolved = KeybindingsConfig::new();
        for id in self.definitions.keys() {
            let keys = self.keys_by_id.get(id).cloned().unwrap_or_default();
            let value = if keys.len() == 1 {
                KeyIdList::One(keys.into_iter().next().expect("len 1"))
            } else {
                KeyIdList::Many(keys)
            };
            resolved.insert(id.clone(), value);
        }
        resolved
    }

    /// 定义表（调试/测试用）。
    ///
    /// 返回内部表的共享引用；包含默认键与描述，不包含解析后的结果。
    pub fn definitions(&self) -> &KeybindingDefinitions {
        &self.definitions
    }
}

impl Default for KeybindingsManager {
    fn default() -> Self {
        KeybindingsManager::with_defaults()
    }
}

// =============================================================================
// 全局单例
// =============================================================================

thread_local! {
    /// TS `globalKeybindings`：全局单例。`thread_local!` 决定它是**每线程一份**，不是进程级
    /// 共享状态；TUI 的单线程模型下等价于进程级。
    static GLOBAL_KEYBINDINGS: RefCell<Option<&'static KeybindingsManager>> = const { RefCell::new(None) };
}

/// 安装全局键位管理器（`setKeybindings`）。
///
/// 管理器被泄漏到进程生命周期，因此后续 [`get_keybindings`] 可以返回 `&'static`。
/// 这在 TS 里是普通赋值；Rust 侧的泄漏只会发生在启动、配置重载与测试替换时。
///
/// 每次调用都会泄漏一个新的管理器，**旧管理器不会被释放**；且槽位是 thread-local，安装只影响
/// 当前线程（见下方 `GLOBAL_KEYBINDINGS` 的定义）。
pub fn set_keybindings(keybindings: KeybindingsManager) {
    let leaked: &'static KeybindingsManager = Box::leak(Box::new(keybindings));
    GLOBAL_KEYBINDINGS.with(|slot| *slot.borrow_mut() = Some(leaked));
}

/// 取全局键位管理器（`getKeybindings`）；未设置时懒初始化为默认表。
///
/// 懒初始化只发生在**当前线程**：首次调用会泄漏一个 [`KeybindingsManager::with_defaults`]
/// 实例，之后同一线程反复调用返回同一个 `&'static` 引用。
pub fn get_keybindings() -> &'static KeybindingsManager {
    GLOBAL_KEYBINDINGS.with(|slot| {
        let mut slot = slot.borrow_mut();
        if slot.is_none() {
            *slot = Some(Box::leak(Box::new(KeybindingsManager::with_defaults())));
        }
        slot.expect("just initialized")
    })
}

/// 清除全局键位管理器（测试用；TS 无对应，但测试需要重置）。
///
/// 只把槽位置空，**不会**回收之前泄漏的管理器；下一次 [`get_keybindings`] 会在当前线程重新
/// 初始化默认表。
pub fn reset_global_keybindings() {
    GLOBAL_KEYBINDINGS.with(|slot| *slot.borrow_mut() = None);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manager(user: impl IntoIterator<Item = (&'static str, KeyIdList)>) -> KeybindingsManager {
        let mut config = KeybindingsConfig::new();
        for (id, keys) in user {
            config.insert(id.to_string(), keys);
        }
        KeybindingsManager::new(tui_keybindings(), config)
    }

    #[test]
    fn binds_ctrl_j_as_a_default_newline_alias() {
        let keybindings = KeybindingsManager::with_defaults();
        assert_eq!(
            keybindings.get_keys("tui.input.newLine"),
            vec!["shift+enter".to_string(), "ctrl+j".to_string()]
        );
        assert!(keybindings.matches("\n", "tui.input.newLine"));
        assert!(keybindings.matches("\x1b[106;5u", "tui.input.newLine"));
    }

    #[test]
    fn binds_modified_and_unmodified_editor_viewport_navigation() {
        let keybindings = KeybindingsManager::with_defaults();
        assert_eq!(
            keybindings.get_keys("tui.editor.cursorLineStart"),
            vec!["home".to_string(), "ctrl+a".to_string()]
        );
        assert_eq!(
            keybindings.get_keys("tui.editor.cursorLineEnd"),
            vec!["end".to_string(), "ctrl+e".to_string()]
        );
        assert_eq!(
            keybindings.get_keys("tui.editor.pageUp"),
            vec!["pageUp".to_string(), "ctrl+pageUp".to_string()]
        );
        assert_eq!(
            keybindings.get_keys("tui.editor.pageDown"),
            vec!["pageDown".to_string(), "ctrl+pageDown".to_string()]
        );
    }

    #[test]
    fn leaves_dedicated_prompt_history_navigation_unbound_by_default() {
        let keybindings = KeybindingsManager::with_defaults();
        assert!(
            keybindings
                .get_keys("tui.editor.historyPrevious")
                .is_empty()
        );
        assert!(keybindings.get_keys("tui.editor.historyNext").is_empty());
    }

    #[test]
    fn binds_unmodified_terminal_viewport_shortcuts_to_alternate_screen_navigation() {
        let keybindings = KeybindingsManager::with_defaults();
        assert_eq!(
            keybindings.get_keys("tui.altScreen.pageUp"),
            vec!["pageUp".to_string()]
        );
        assert_eq!(
            keybindings.get_keys("tui.altScreen.pageDown"),
            vec!["pageDown".to_string()]
        );
        assert!(keybindings.get_keys("tui.altScreen.halfPageUp").is_empty());
        assert!(
            keybindings
                .get_keys("tui.altScreen.halfPageDown")
                .is_empty()
        );
        assert!(keybindings.get_keys("tui.altScreen.lineUp").is_empty());
        assert!(keybindings.get_keys("tui.altScreen.lineDown").is_empty());
        assert_eq!(
            keybindings.get_keys("tui.altScreen.previousPrompt"),
            vec!["ctrl+shift+up".to_string(), "ctrl+up".to_string()]
        );
        assert_eq!(
            keybindings.get_keys("tui.altScreen.nextPrompt"),
            vec!["ctrl+shift+down".to_string(), "ctrl+down".to_string()]
        );
        assert_eq!(
            keybindings.get_keys("tui.altScreen.search"),
            vec!["ctrl+shift+f".to_string()]
        );
        assert_eq!(
            keybindings.get_keys("tui.altScreen.searchNext"),
            vec!["enter".to_string(), "ctrl+g".to_string()]
        );
        assert_eq!(
            keybindings.get_keys("tui.altScreen.searchPrevious"),
            vec!["shift+enter".to_string(), "ctrl+shift+g".to_string()]
        );
        assert_eq!(
            keybindings.get_keys("tui.altScreen.searchClose"),
            vec!["escape".to_string()]
        );
        assert_eq!(
            keybindings.get_keys("tui.altScreen.top"),
            vec!["ctrl+home".to_string()]
        );
        assert_eq!(
            keybindings.get_keys("tui.altScreen.bottom"),
            vec!["ctrl+end".to_string()]
        );
    }

    #[test]
    fn does_not_evict_selector_confirm_when_input_submit_is_rebound() {
        let keybindings = manager([("tui.input.submit", vec!["enter", "ctrl+enter"].into())]);
        assert_eq!(
            keybindings.get_keys("tui.input.submit"),
            vec!["enter".to_string(), "ctrl+enter".to_string()]
        );
        assert_eq!(
            keybindings.get_keys("tui.select.confirm"),
            vec!["enter".to_string()]
        );
    }

    #[test]
    fn does_not_evict_cursor_bindings_when_another_action_reuses_the_same_key() {
        let keybindings = manager([("tui.select.up", vec!["up", "ctrl+p"].into())]);
        assert_eq!(
            keybindings.get_keys("tui.select.up"),
            vec!["up".to_string(), "ctrl+p".to_string()]
        );
        assert_eq!(
            keybindings.get_keys("tui.editor.cursorUp"),
            vec!["up".to_string()]
        );
    }

    #[test]
    fn still_reports_direct_user_binding_conflicts_without_evicting_defaults() {
        let keybindings = manager([
            ("tui.input.submit", "ctrl+x".into()),
            ("tui.select.confirm", "ctrl+x".into()),
        ]);
        assert_eq!(
            keybindings.get_conflicts(),
            vec![KeybindingConflict {
                key: "ctrl+x".to_string(),
                keybindings: vec![
                    "tui.input.submit".to_string(),
                    "tui.select.confirm".to_string()
                ],
            }]
        );
        assert_eq!(
            keybindings.get_keys("tui.editor.cursorLeft"),
            vec!["left".to_string(), "ctrl+b".to_string()]
        );
    }

    #[test]
    fn ignores_unknown_ids_and_unbinds_with_an_explicit_empty_list() {
        let keybindings = manager([
            ("app.unknown", "ctrl+x".into()),
            ("tui.select.up", Vec::<KeyId>::new().into()),
        ]);
        assert!(keybindings.get_conflicts().is_empty());
        assert!(keybindings.get_keys("tui.select.up").is_empty());
    }

    #[test]
    fn resolves_single_keys_to_single_values_in_definition_order() {
        let keybindings = KeybindingsManager::with_defaults();
        let resolved = keybindings.get_resolved_bindings();
        let ids: Vec<&str> = resolved.keys().map(String::as_str).collect();
        assert_eq!(ids, KEYBINDING_IDS.to_vec());
        assert_eq!(
            resolved.get("tui.input.submit"),
            Some(&KeyIdList::One("enter".to_string()))
        );
        assert_eq!(
            resolved.get("tui.altScreen.halfPageUp"),
            Some(&KeyIdList::Many(Vec::new()))
        );
    }

    #[test]
    fn default_table_matches_the_interface_id_set() {
        let table = tui_keybindings();
        let ids: Vec<&str> = table.keys().map(String::as_str).collect();
        assert_eq!(ids, KEYBINDING_IDS.to_vec());
        assert_eq!(table.len(), 47);
    }
}
