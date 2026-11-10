//! components/scoped-models-selector.ts。
//!
//! `/scoped-models`（Ctrl+P 轮换范围）的模型启用/排序选择器。改动只作用于会话，`Ctrl+S` 才写入
//! settings。
//!
//! TS: `packages/coding-agent/src/modes/interactive/components/scoped-models-selector.ts`
//!
//! ## 与 TS 的差异（仅运行时适配）
//!
//! - `TUI` 参数不需要：组件不主动请求重绘（TS 的 `ScopedModelsSelector` 也不调用
//!   `tui.requestRender()`）。
//! - `keyDisplayText` / `keyHint` 读 `kiana-tui` 的全局键位表。宿主必须把
//!   `kiana_config::keybindings::keybindings()`（47 个 TUI + 43 个 app.*）装进
//!   `kiana_tui::set_keybindings`，否则 `app.models.*` 的匹配恒为 false（TS 的
//!   `KeybindingsManager` 天然包含两者）。
//! - `onChange` / `onPersist` 的返回值（`void | Promise<void>`）在 Rust 里是 boxed future。
//!   组件先同步 poll 一次（复刻「async 函数体在第一个 await 前同步执行」），仍未完成时
//!   `spawn_local` 继续驱动（交互模式整体跑在单线程 `LocalSet` 上）。
//!
//! ## 职责边界
//!
//! - 选择只作用于当前会话：每次修改先更新本组件的 `enabled_ids`，再通过 `on_change` 通知宿主；
//!   只有 `app.models.save` 才调用 `on_persist`。组件自身不读写 settings，也不解释启用列表如
//!   何变成 session 的 scoped models（那由 `interactive_mode/commands/model.rs` 完成）。
//! - 两种启用状态互斥：`None` 表示「全部启用」（不筛选，新模型自动算启用）；`Some(list)` 是
//!   显式且有序的启用集合，`Some(vec![])` 表示全部禁用。显式列表允许包含目录中已不存在的
//!   id，它们仍会显示（删除线 + `[unavailable]`），便于用户按 Enter 移除。
//! - 全 id 一律按 `provider/id` 拼装，与 `model_search` 的搜索文本和宿主的 `model_key` 一致。
//!
//! ## 主要流程
//!
//! 1. 宿主 `InteractiveMode::show_models_selector` 用目录快照、当前启用集合和可选的刷新状态行
//!    构造组件，再把它放进 overlay；构造过程不调用任何回调。
//! 2. 每帧 `render` 先 `update_list()` 重建列表行，再按 `children_mut()` 的固定顺序渲染子组件，
//!    同时缓存各子组件的行高到 `mouse_layout` 供鼠标命中测试。
//! 3. `handle_input` 按固定优先级消费按键：导航 → 重排 → Enter 切换 → 全开 → 全清 →
//!    按 provider 切换 → 保存 → Ctrl+C / Escape → 其余输入交给搜索框。所有会改变选择的
//!    分支都遵循「改 `enabled_ids` → `refresh()` → `notify_change()`」这一顺序。
//! 4. 后台目录刷新完成后，宿主调用 `update_models*`（保持或显式设置启用集合）与
//!    `set_refresh_status`，并在外部请求重绘；组件本身不主动请求重绘。
//!
//! ## 回调时序
//!
//! `on_change` / `on_persist` 返回的 future 由 `run_callback` 先用 noop waker 同步 poll 一次，
//! 因此 async 块在第一个 `await` 之前的部分与 TS 的同步回调一样先执行；返回 `Pending` 时才
//! 交给 `tokio::task::spawn_local`。组件不等待 future：它只在同一次输入处理里完成
//! `is_dirty`、列表与 footer 的更新（`on_persist` 是调用后再清 `is_dirty` 并重绘 footer），
//! 且 future 的 `Output` 是 `()`，回调失败没有错误通道。
//!
//! ## 注意事项
//!
//! - 标题、提示与 footer 的着色在构造或刷新时立即执行，调用前全局主题必须已初始化，否则
//!   `fg` / `bold` / `strikethrough` 会 panic（TS 在对应位置抛异常）。
//! - `selected_index` 用 `isize` 保留 TS 可能出现的 `-1`：过滤激活时重排会触发它，随后
//!   `update_list()` 按 TS 的同一文本 panic，详见该字段与 `handle_input` 的注释。
//! - 子组件集合与顺序在构造后固定（状态行要么一开始就有、要么永远没有），因此 `handle_mouse`
//!   可以直接用下标对齐 `children_mut()`，不需要 `Container` 那套稳定 child id。

use std::collections::HashSet;
use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Waker};

use kiana_ai::types::Model;
use kiana_theme::{THEME, ThemeColor};
use kiana_tui::components::input::{Input, InputOptions};
use kiana_tui::components::spacer::Spacer;
use kiana_tui::components::text_component::Text;
use kiana_tui::fuzzy::fuzzy_filter;
use kiana_tui::keybindings::get_keybindings;
use kiana_tui::keys::matches_key;
use kiana_tui::tui::{
    Component, Container, Focusable, MouseFocusTarget, TuiMouseEvent, TuiMouseEventResult,
    dispatch_mouse_event,
};
use kiana_ui_kit::DynamicBorder;
use kiana_ui_kit::keybinding_hints::key_display_text;

use crate::model_search::{get_distinct_model_name, get_model_search_text};

/// 按全局主题着色（TS `theme.fg(token, str)`）。
fn fg(color: ThemeColor, text: &str) -> String {
    THEME
        .fg(color, text)
        .expect("theme must be initialized before rendering (TS throws here)")
}

/// TS `theme.bold(str)`。
fn bold(text: &str) -> String {
    THEME
        .bold(text)
        .expect("theme must be initialized before rendering (TS throws here)")
}

/// TS `theme.strikethrough(str)`。
fn strikethrough(text: &str) -> String {
    THEME
        .strikethrough(text)
        .expect("theme must be initialized before rendering (TS throws here)")
}

/// `onChange` / `onPersist` 的返回值（TS `void | Promise<void>`）。
///
/// 与 TS 一样不要求 `Send`：组件跑在单线程 `LocalSet` 上（`run_callback` 用 `spawn_local`），
/// 宿主回调可以捕获 `Rc<RefCell<…>>` 形式的 `InteractiveMode` 状态。
pub type ModelsCallbackFuture = Pin<Box<dyn Future<Output = ()> + 'static>>;

/// TS: `ModelsConfig`。
///
/// 构造时的输入快照。构造之后目录或启用集合的变化要经 `update_models*` 同步，而不是重新
/// 构造组件（重建会丢失搜索框内容、选中位置与 `is_dirty`）。组件会克隆 `all_models` 中的
/// 模型，调用方后续修改原 `Vec` 不影响已构造的组件。
#[derive(Debug, Clone, Default)]
pub struct ModelsConfig {
    /// `allModels`：当前可用模型目录快照。每个模型展开为 `provider/id`；本列表的顺序决定
    /// 「未启用模型」的追加顺序（`get_sorted_ids` 先列启用项，再按本序追加其余项）。
    pub all_models: Vec<Model>,
    /// `null` = 全部启用（不筛选）；`Some(list)` = 显式且有序的启用列表。
    ///
    /// 列表可以包含 `all_models` 中不存在的 id：这些 id 被视为不可用模型，保留显示并可通过
    /// Enter 移除。空 `Vec` 表示全部禁用（与 `None` 的含义相反）。
    pub enabled_model_ids: Option<Vec<String>>,
    /// 初始刷新状态行（纯字符串，无 kind）。
    ///
    /// 非空时在列表下方创建一行 Muted 文本，之后可用 `set_refresh_status` 更新；`None` 或
    /// 空串按 TS 的 falsy 处理——不创建该行，且 `set_refresh_status` 永远是无操作。
    pub refresh_status: Option<String>,
}

/// TS: `ModelsCallbacks`。
///
/// TS 的回调是普通闭包（无线程约束），组件也在单线程上运行，因此这里不加 `Send`/`Sync`
/// （否则宿主无法捕获 `Rc<RefCell<…>>` 状态，与 TS 的同步回调语义不符）。
///
/// 三个回调都在驱动 `handle_input` 的线程上触发：`on_change` / `on_persist` 返回的 future
/// 先被同步 poll 一次（见 `run_callback`），`on_cancel` 是纯同步调用。构造组件、`update_models`
/// 与 `set_refresh_status` 都不会触发任何回调。
pub struct ModelsCallbacks {
    /// 启用集合或顺序变化（仅会话，不写盘）。
    ///
    /// 值总是当前 `enabled_ids` 的克隆：`None` = 全部启用，`Some` = 显式有序列表。仅在用户
    /// 操作（切换/全开/全清/按 provider 切换/重排）之后、列表与 footer 刷新完成后调用。
    pub on_change: Box<dyn Fn(Option<Vec<String>>) -> ModelsCallbackFuture>,
    /// 用户要求把当前选择写入 settings。
    ///
    /// 参数语义同 `on_change`。组件调用后立即清掉 `is_dirty` 并重绘 footer，不关心 future
    /// 何时完成、也不接收任何完成/失败通知。
    pub on_persist: Box<dyn Fn(Option<Vec<String>>) -> ModelsCallbackFuture>,
    /// Escape / Ctrl+C（搜索为空时）。
    ///
    /// 同步调用；组件自身不关闭 overlay、不请求重绘，关闭动作由宿主在回调里完成。
    pub on_cancel: Box<dyn Fn()>,
}

/// `setRefreshStatus(message, kind)` 的 kind。
///
/// 只决定状态行的前景色：Muted → `ThemeColor::Muted`，Success → `ThemeColor::Success`，
/// Warning → `ThemeColor::Warning`。构造时没有创建状态行（`refresh_status` 为 `None` 或
/// 空串）时，kind 不影响任何输出。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefreshStatusKind {
    /// 中性提示，例如构造时的 “Refreshing model catalogs…”。
    Muted,
    /// 成功提示，例如目录刷新完成。
    Success,
    /// 警告，例如目录刷新失败或超时。
    Warning,
}

impl RefreshStatusKind {
    fn color(self) -> ThemeColor {
        match self {
            RefreshStatusKind::Muted => ThemeColor::Muted,
            RefreshStatusKind::Success => ThemeColor::Success,
            RefreshStatusKind::Warning => ThemeColor::Warning,
        }
    }
}

// ---------------------------------------------------------------------------
// 模块私有纯函数（TS 里全部不导出）
// ---------------------------------------------------------------------------

/// EnabledIds：`null` = 全部启用（无筛选），`string[]` = 显式且有序的列表。
type EnabledIds = Option<Vec<String>>;

/// `isEnabled(enabledIds, id)`。
fn is_enabled(enabled_ids: &EnabledIds, id: &str) -> bool {
    match enabled_ids {
        None => true,
        Some(ids) => ids.iter().any(|enabled| enabled == id),
    }
}

/// `normalizeEnabled(result, allIds)`：覆盖全部可用模型时折叠回 `null`。
///
/// 判定只比较长度与逐项成员关系（与 TS 相同），因此重复项也算「覆盖」：
/// `["a", "a"]` 对 `allIds = ["a", "b"]` 会折叠成 `None`（见本文件测试）。
fn normalize_enabled(result: Vec<String>, all_ids: &[String]) -> EnabledIds {
    if result.len() == all_ids.len() && result.iter().all(|id| all_ids.contains(id)) {
        None
    } else {
        Some(result)
    }
}

/// `toggle(enabledIds, allIds, id)`。
///
/// 从 `None`（全部启用）出发时结果为「全部减去 id」的显式列表；从显式列表移除时保留原顺序，
/// 移除最后一项会得到 `Some(vec![])`（全部禁用），**不会**折叠成 `None`——两者语义相反，
/// 不能互换。只有「加回后覆盖全部」才经 `normalize_enabled` 折叠。
fn toggle(enabled_ids: &EnabledIds, all_ids: &[String], id: &str) -> EnabledIds {
    match enabled_ids {
        // TS: `allIds.filter(modelId => modelId !== id)`（不做 normalize）。
        None => Some(
            all_ids
                .iter()
                .filter(|model_id| model_id.as_str() != id)
                .cloned()
                .collect(),
        ),
        Some(ids) => match ids.iter().position(|model_id| model_id == id) {
            Some(index) => {
                let mut result = ids.clone();
                result.remove(index);
                Some(result)
            }
            None => {
                let mut result = ids.clone();
                result.push(id.to_string());
                normalize_enabled(result, all_ids)
            }
        },
    }
}

/// `enableAll(enabledIds, allIds, targetIds?)`。
fn enable_all(
    enabled_ids: &EnabledIds,
    all_ids: &[String],
    target_ids: Option<&[String]>,
) -> EnabledIds {
    let Some(ids) = enabled_ids else {
        // 已经全部启用。
        return None;
    };
    let targets = target_ids.unwrap_or(all_ids);
    let mut result = ids.clone();
    for id in targets {
        if !result.iter().any(|candidate| candidate == id) {
            result.push(id.clone());
        }
    }
    normalize_enabled(result, all_ids)
}

/// `clearAll(enabledIds, allIds, targetIds?)`。
fn clear_all(
    enabled_ids: &EnabledIds,
    all_ids: &[String],
    target_ids: Option<&[String]>,
) -> EnabledIds {
    match enabled_ids {
        None => match target_ids {
            Some(targets) => Some(
                all_ids
                    .iter()
                    .filter(|id| !targets.iter().any(|target| target == *id))
                    .cloned()
                    .collect(),
            ),
            None => Some(Vec::new()),
        },
        Some(ids) => {
            let targets: HashSet<&str> = match target_ids {
                Some(targets) => targets.iter().map(String::as_str).collect(),
                None => ids.iter().map(String::as_str).collect(),
            };
            Some(
                ids.iter()
                    .filter(|id| !targets.contains(id.as_str()))
                    .cloned()
                    .collect(),
            )
        }
    }
}

/// `move(enabledIds, id, delta)`：越界 / 未找到 → 原列表副本。
fn move_id(enabled_ids: &EnabledIds, id: &str, delta: isize) -> EnabledIds {
    let Some(ids) = enabled_ids else {
        return None;
    };
    let mut list = ids.clone();
    let Some(index) = list.iter().position(|model_id| model_id == id) else {
        return Some(list);
    };
    let new_index = index as isize + delta;
    if new_index < 0 || new_index as usize >= list.len() {
        return Some(list);
    }
    let new_index = new_index as usize;
    list.swap(index, new_index);
    Some(list)
}

/// `getSortedIds(enabledIds, allIds)`：启用项保持显式顺序，其余按 allIds 顺序追加。
fn get_sorted_ids(enabled_ids: &EnabledIds, all_ids: &[String]) -> Vec<String> {
    match enabled_ids {
        None => all_ids.to_vec(),
        Some(ids) => {
            let enabled_set: HashSet<&str> = ids.iter().map(String::as_str).collect();
            let mut result = ids.clone();
            result.extend(
                all_ids
                    .iter()
                    .filter(|id| !enabled_set.contains(id.as_str()))
                    .cloned(),
            );
            result
        }
    }
}

/// `ModelItem`：一行的派生快照。
///
/// `build_items` 在构建列表时一次性把 id、目录中的模型与启用状态复制进来，渲染时不再
/// 回查 `models_by_id`，因此列表内容与当次 `enabled_ids` 一致。
#[derive(Debug, Clone)]
struct ModelItem {
    /// `provider/id`，始终存在；不可用模型也靠它显示与移除。
    full_id: String,
    /// 目录中的模型；`None` = 该 id 已不在当前目录（删除线 + `[unavailable]`）。
    model: Option<Model>,
    /// 构建时的启用状态（`is_enabled` 的结果）。
    enabled: bool,
}

/// 执行回调返回的 future：先同步 poll 一次（TS 的 async 函数在第一个 await 前同步执行），
/// 仍未完成时交给 `spawn_local` 继续。
///
/// 失败模式与约束：
/// - 首轮 poll 同步执行，future 在这里 panic 会直接冒泡到 `handle_input` 的调用方；
/// - 只有返回 `Pending` 才会调用 `tokio::task::spawn_local`，该函数要求当前线程存在
///   `LocalSet`（交互模式整体如此；测试里的回调都是立即 `Ready`）；
/// - spawn 出去的 future 由执行它的 `LocalSet` 驱动；组件被丢弃不会取消它（组件不保存
///   任何任务句柄）。
fn run_callback(future: ModelsCallbackFuture) {
    let mut future = future;
    let mut context = Context::from_waker(Waker::noop());
    if future.as_mut().poll(&mut context).is_pending() {
        tokio::task::spawn_local(future);
    }
}

/// TS: `ScopedModelsSelectorComponent`。
///
/// `/models` 覆盖层里的模型启用/排序选择器。它同时实现 [`Component`]（渲染、键盘、鼠标）与
/// [`Focusable`]（焦点转发给内部搜索框，供终端 IME 定位光标）。
///
/// # 状态与不变量
///
/// - `models_by_id` / `all_ids` 是最近一次目录快照，二者同源；`all_ids` 的顺序决定未启用模型
///   的展示顺序。
/// - `enabled_ids` 是唯一可变的选择状态：`None` = 全部启用，`Some(list)` = 显式有序列表。
///   显式列表可能包含快照中不存在的 id（不可用模型），它们会保留显示并可被移除。
/// - `filtered_items` 是派生数据：由 `enabled_ids`、`all_ids` 与搜索框当前值重建，搜索非空时
///   再经 `fuzzy_filter` 过滤排序。不要把它当作长期状态。
/// - `selected_index` 是 `filtered_items` 的下标（`isize` 以保留 TS 的 `-1`），其余字段都只
///   影响呈现或回调时序。
///
/// # 生命周期
///
/// 宿主（`interactive_mode/commands/model.rs` 的 `show_models_selector`）在覆盖层打开时构造，
/// 后台目录刷新完成后调用 `update_models*`；用户按键经 `handle_input` 进入，渲染经 `render`。
/// 本组件不持有定时器或后台任务，也不需要 `dispose`：需要取消的外部工作（目录刷新）由宿主
/// 自己的 dispose 回调负责。
///
/// # 失败模式
///
/// 构造与刷新时的着色要求全局主题已初始化（见文件级文档）；TS 在过滤激活时重排会把
/// `selected_index` 变成 `-1` 并在渲染时抛 `TypeError`，Rust 侧保留同一行为（见
/// `handle_input` 的重排分支与 `update_list`）。
pub struct ScopedModelsSelectorComponent {
    /// 最近一次目录快照：全 id → 模型；目录中缺失的启用 id 不在这里。
    models_by_id: std::collections::HashMap<String, Model>,
    /// 快照顺序的全 id 列表，与 `models_by_id` 同源。
    all_ids: Vec<String>,
    /// 当前启用集合（唯一可变的选择状态）；`None` = 全部启用。
    enabled_ids: EnabledIds,
    /// `enabled_ids` + `all_ids` + 搜索词的派生列表，由 `refresh` 或构造重建。
    filtered_items: Vec<ModelItem>,
    /// TS 的 `selectedIndex` 是普通 `number`：重排 + 过滤时可以变成 `-1`（随后渲染抛
    /// TypeError）。用 `isize` 保留这个负值，见 `handle_input` 的重排分支。
    selected_index: isize,
    /// 搜索框；未被命令消费的按键都交给它，其值参与 `refresh` 的过滤。
    search_input: Input,
    /// 模型列表行（含滚动指示与详情行），每次 `update_list` 清空重建。
    list_container: Container,
    /// 底部快捷键提示，内容由 `get_footer_text` 生成。
    footer_text: Text,
    callbacks: ModelsCallbacks,
    /// 列表窗口最多显示的行数（固定 8，与 TS 的 `maxVisible` 一致）。
    max_visible: usize,
    /// 自上次保存以来是否有未落盘的修改；只影响 footer 的 `(unsaved)`。
    is_dirty: bool,
    /// 可选的刷新状态行；构造时未创建则永远为 `None`（`set_refresh_status` 为 no-op）。
    refresh_status_text: Option<Text>,
    /// 标题与 “Session-only” 提示行。
    title_text: Text,
    session_hint_text: Text,
    // 上下边框与各段之间的空行；渲染顺序由 `children_mut` 固定，见该方法。
    top_border: DynamicBorder,
    bottom_border: DynamicBorder,
    spacer_top: Spacer,
    spacer_after_hint: Spacer,
    spacer_after_search: Spacer,
    spacer_after_list: Spacer,
    /// TS `Container.mouseLayout`：最近一次 render 的子组件高度（按渲染顺序）。
    mouse_layout: Option<(usize, Vec<usize>)>,
    /// `Focusable` 的焦点状态；`set_focused` 会同步传给 `search_input`。
    focused: bool,
}

impl ScopedModelsSelectorComponent {
    /// `constructor(config, callbacks)`。
    ///
    /// 把 `all_models` 展开成 `models_by_id` 与有序的 `all_ids`，保留 `enabled_model_ids`
    /// 原样（不与目录求交，因此不可用 id 不会被丢弃），初始化搜索框、各布局子组件，并调用
    /// 一次 `build_items` / `update_list` 得到首帧内容。
    ///
    /// 副作用与前提：
    /// - 会立即用主题着色生成标题、提示和状态行文本，所以全局主题必须先初始化；
    /// - 不调用任何回调，也不读取 settings；
    /// - 空 `refresh_status` 按 TS 的 falsy 语义不创建状态行。
    pub fn new(config: ModelsConfig, callbacks: ModelsCallbacks) -> Self {
        let mut models_by_id = std::collections::HashMap::new();
        let mut all_ids = Vec::new();
        for model in &config.all_models {
            let full_id = format!("{}/{}", model.provider, model.id);
            models_by_id.insert(full_id.clone(), model.clone());
            all_ids.push(full_id);
        }
        let enabled_ids = config.enabled_model_ids.map(|ids| ids.to_vec());
        let mut component = ScopedModelsSelectorComponent {
            models_by_id,
            all_ids,
            enabled_ids,
            filtered_items: Vec::new(),
            selected_index: 0,
            search_input: Input::new(InputOptions::default()),
            list_container: Container::new(),
            footer_text: Text::with_padding("", 0, 0),
            callbacks,
            max_visible: 8,
            is_dirty: false,
            refresh_status_text: config
                .refresh_status
                .as_deref()
                // TS 的 `if (config.refreshStatus)`：空串是 falsy → 不创建状态行。
                .filter(|status| !status.is_empty())
                .map(|status| {
                    Text::with_padding(fg(ThemeColor::Muted, &format!("  {status}")), 0, 0)
                }),
            title_text: Text::with_padding(
                fg(ThemeColor::Accent, &bold("Model Configuration")),
                0,
                0,
            ),
            session_hint_text: Text::with_padding(
                fg(
                    ThemeColor::Muted,
                    &format!(
                        "Session-only. {} to save to settings.",
                        key_display_text("app.models.save")
                    ),
                ),
                0,
                0,
            ),
            top_border: DynamicBorder::default(),
            bottom_border: DynamicBorder::default(),
            spacer_top: Spacer::new(),
            spacer_after_hint: Spacer::new(),
            spacer_after_search: Spacer::new(),
            spacer_after_list: Spacer::new(),
            mouse_layout: None,
            focused: false,
        };
        component.filtered_items = component.build_items();
        component.footer_text = Text::with_padding(component.get_footer_text(), 0, 0);
        component.update_list();
        component
    }

    /// `updateModels(models, enabledModelIds?)`。
    ///
    /// `None` 相当于 TS 的 `undefined`（保持当前启用集合）；`Some(None)` 相当于 `null`
    /// （全部启用）。
    ///
    /// `Option<Option<_>>` 逐字映射 TS 的 `undefined | string[] | null`，因此显式关闭 lint。
    ///
    /// 行为：
    /// - 先记下当前选中项的 `full_id`，替换整个目录快照并 `refresh()` 后，再在新列表里按 id
    ///   找回它，使后台刷新不会把选中项重置到第 0 行；找不到时保留 `refresh()` clamp 的结果。
    /// - 只重建派生列表与 footer，不触发 `on_change`，也不改 `is_dirty`（目录刷新不是用户操作）。
    /// - 搜索框内容保留；若刷新后过滤结果变空，列表显示 “No matching models”。
    #[allow(clippy::option_option)]
    pub fn update_models(
        &mut self,
        models: &[Model],
        enabled_model_ids: Option<Option<Vec<String>>>,
    ) {
        let selected_id = self.selected_item().map(|item| item.full_id.clone());
        if let Some(enabled) = enabled_model_ids {
            self.enabled_ids = enabled;
        }
        self.models_by_id.clear();
        self.all_ids.clear();
        for model in models {
            let full_id = format!("{}/{}", model.provider, model.id);
            self.models_by_id.insert(full_id.clone(), model.clone());
            self.all_ids.push(full_id);
        }
        self.refresh();
        let refreshed_index = selected_id.and_then(|id| {
            self.filtered_items
                .iter()
                .position(|item| item.full_id == id)
        });
        if let Some(index) = refreshed_index {
            self.selected_index = index as isize;
            self.update_list();
        }
    }

    /// `updateModels(models)`：保持当前启用集合（TS 省略第二个参数）。
    pub fn update_models_keep_enabled(&mut self, models: &[Model]) {
        self.update_models(models, None);
    }

    /// `updateModels(models, enabledModelIds)`：显式设置启用集合（`None` = 全部启用）。
    pub fn update_models_with_enabled(
        &mut self,
        models: &[Model],
        enabled_model_ids: Option<Vec<String>>,
    ) {
        self.update_models(models, Some(enabled_model_ids));
    }

    /// `setRefreshStatus(message, kind)`。
    ///
    /// 只在构造时创建了状态行（`ModelsConfig::refresh_status` 非空）时生效；否则静默丢弃
    /// 本次更新（TS 的 `refreshStatusText?.setText`）。文本统一带两个前导空格并按 `kind`
    /// 着色。本方法不请求重绘：宿主调用后需自行触发渲染。
    pub fn set_refresh_status(&mut self, message: &str, kind: RefreshStatusKind) {
        if let Some(text) = self.refresh_status_text.as_mut() {
            text.set_text(fg(kind.color(), &format!("  {message}")));
        }
    }

    /// `getSearchInput()`：只读访问内部搜索框。
    ///
    /// 用于读取当前查询与光标位置（测试用它断言鼠标点击把光标放到了正确位置）；修改查询
    /// 应通过 `handle_input` 走正常流程，否则列表不会重新过滤。
    pub fn get_search_input(&self) -> &Input {
        &self.search_input
    }

    /// `buildItems()`：按 `enabled_ids` 的顺序列出启用项，再按 `all_ids` 的顺序追加其余项。
    ///
    /// 目录里不存在的 id 仍然生成条目（`model: None`），这正是不可用模型能被看到和移除的
    /// 原因；`enabled` 由 `enabled_ids` 判定，与 `model` 是否存在无关。
    fn build_items(&self) -> Vec<ModelItem> {
        get_sorted_ids(&self.enabled_ids, &self.all_ids)
            .into_iter()
            .map(|id| ModelItem {
                model: self.models_by_id.get(&id).cloned(),
                enabled: is_enabled(&self.enabled_ids, &id),
                full_id: id,
            })
            .collect()
    }

    /// TS `this.filteredItems[this.selectedIndex]`：负下标（重排 + 过滤后的 `-1`）与越界都
    /// 返回 `None`（对应 JS 的 `undefined`）。
    fn selected_item(&self) -> Option<&ModelItem> {
        usize::try_from(self.selected_index)
            .ok()
            .and_then(|index| self.filtered_items.get(index))
    }

    /// `getFooterText()`：底部一行快捷键提示 + 计数 + 可选的 `(unsaved)`。
    ///
    /// 计数只统计 `enabled_ids` 中的 id：能对应目录的算 enabled，对不上的单列为 unavailable；
    /// `None`（全部启用）时显示 `all enabled`，并且不统计不可用项。快捷键文本来自全局键位表
    /// （`key_display_text`），键位表未安装 `app.models.*` 时对应键位显示为空，只剩描述文字。
    fn get_footer_text(&self) -> String {
        let enabled_count = match &self.enabled_ids {
            Some(ids) => ids
                .iter()
                .filter(|id| self.models_by_id.contains_key(*id))
                .count(),
            None => self.all_ids.len(),
        };
        let unavailable_count = match &self.enabled_ids {
            Some(ids) => ids
                .iter()
                .filter(|id| !self.models_by_id.contains_key(*id))
                .count(),
            None => 0,
        };
        let count_text = match &self.enabled_ids {
            None => "all enabled".to_string(),
            Some(_) => {
                let mut text = format!("{enabled_count}/{} enabled", self.all_ids.len());
                if unavailable_count > 0 {
                    text.push_str(&format!(" · {unavailable_count} unavailable"));
                }
                text
            }
        };
        let parts = [
            format!("{} toggle", key_display_text("tui.select.confirm")),
            format!("{} all", key_display_text("app.models.enableAll")),
            format!("{} clear", key_display_text("app.models.clearAll")),
            format!("{} provider", key_display_text("app.models.toggleProvider")),
            format!(
                "{}/{} reorder",
                key_display_text("app.models.reorderUp"),
                key_display_text("app.models.reorderDown")
            ),
            format!("{} save", key_display_text("app.models.save")),
            count_text,
        ];
        if self.is_dirty {
            fg(ThemeColor::Dim, &format!("  {} ", parts.join(" · ")))
                + &fg(ThemeColor::Warning, "(unsaved)")
        } else {
            fg(ThemeColor::Dim, &format!("  {}", parts.join(" · ")))
        }
    }

    /// `refresh()`：重建 filteredItems、clamp 选中、重绘列表与 footer。
    ///
    /// 搜索框非空时走 `fuzzy_filter`：按匹配分数升序稳定排序（同分保持 `build_items` 的
    /// 顺序），因此过滤会改变行的顺序。本方法只刷新呈现，不触发 `on_change`。
    fn refresh(&mut self) {
        let query = self.search_input.get_value().to_string();
        let items = self.build_items();
        self.filtered_items = if query.is_empty() {
            items
        } else {
            fuzzy_filter(items, &query, |item| match &item.model {
                Some(model) => get_model_search_text(&model.id, &model.provider, Some(&model.name)),
                None => item.full_id.clone(),
            })
        };
        // TS: `Math.min(this.selectedIndex, Math.max(0, this.filteredItems.length - 1))`。
        // `selectedIndex` 可以是重排留下的 `-1`；对非负值这里等价于 clamp 到 `len - 1`。
        let max_index = 0.max(self.filtered_items.len() as isize - 1);
        self.selected_index = self.selected_index.min(max_index);
        self.update_list();
        self.footer_text.set_text(self.get_footer_text());
    }

    /// `notifyChange()`。
    fn notify_change(&self) {
        let value = self.enabled_ids.clone();
        let future = (self.callbacks.on_change)(value);
        run_callback(future);
    }

    /// `updateList()`：清空并重建列表容器的全部行。
    ///
    /// 行序固定为：可见模型行 → 可选的滚动指示 `(选中序号/总数)` → 空行 → 选中项详情。
    /// 只读取 `filtered_items` 与 `selected_index`，不改变选择状态；每次 `render` 前都会调用。
    fn update_list(&mut self) {
        self.list_container_clear();
        if self.filtered_items.is_empty() {
            self.list_container_push(fg(ThemeColor::Muted, "  No matching models"));
            return;
        }

        // 可见窗口：以选中行为中心，上下各留 max_visible/2，再夹到 [0, len - max_visible]。
        // len <= max_visible 时 upper <= 0，结果从 0 开始、end_index 取 len，即显示全部。
        // selected_index 为 -1 时窗口退化为从 0 开始，且没有行会被标成选中；随后详情行的
        // selected_item() 会按 TS 的行为 panic（见下方）。
        let len = self.filtered_items.len();
        let start_index = {
            let start = self.selected_index as i64 - (self.max_visible / 2) as i64;
            let upper = len as i64 - self.max_visible as i64;
            start.min(upper).max(0) as usize
        };
        let end_index = (start_index + self.max_visible).min(len);
        // 每行依次是：选中箭头、启用勾选、模型 id（不可用则加删除线）、可选的展示名、
        // provider 徽标（不可用显示 [unavailable]）。
        for index in start_index..end_index {
            let item = &self.filtered_items[index];
            let is_selected = index as isize == self.selected_index;
            let prefix = if is_selected {
                fg(ThemeColor::Accent, "→ ")
            } else {
                "  ".to_string()
            };
            let id = match &item.model {
                Some(model) => model.id.clone(),
                None => item.full_id.clone(),
            };
            let styled_id = if item.model.is_some() {
                id
            } else {
                strikethrough(&id)
            };
            let model_text = if is_selected {
                fg(ThemeColor::Accent, &styled_id)
            } else {
                styled_id
            };
            let name = item
                .model
                .as_ref()
                .and_then(|model| get_distinct_model_name(&model.id, Some(&model.name)));
            let name_text = match name {
                Some(name) => fg(ThemeColor::Muted, &format!(" ({name})")),
                None => String::new(),
            };
            let provider_badge = match &item.model {
                Some(model) => fg(ThemeColor::Muted, &format!(" [{}]", model.provider)),
                None => fg(ThemeColor::Muted, " [unavailable]"),
            };
            let status = if item.model.is_some() && item.enabled {
                fg(ThemeColor::Accent, "✓ ")
            } else {
                "  ".to_string()
            };
            self.list_container_push(format!(
                "{prefix}{status}{model_text}{name_text}{provider_badge}"
            ));
        }

        // 只有窗口被裁剪时才显示滚动指示；`+1` 是给人看的 1 基序号。
        if start_index > 0 || end_index < len {
            self.list_container_push(fg(
                ThemeColor::Muted,
                &format!("  ({}/{})", self.selected_index + 1, len),
            ));
        }

        // 详情行取当前选中项。`selected_index == -1`（或越界）时 `selected_item()` 返回
        // `None`，这里刻意让缺失选中项 panic：不静默降级，把内部状态不一致当作 bug 暴露出来。
        let selected = match self.selected_item() {
            Some(selected) => selected,
            // TS `updateList()`：`this.filteredItems[this.selectedIndex]` 为 `undefined` 时
            // 读取 `.model` 抛 `TypeError: Cannot read properties of undefined (reading 'model')`
            // （只有 `handleInput` 的重排分支 + 过滤能把 selectedIndex 变成 -1）。
            None => panic!("Cannot read properties of undefined (reading 'model')"),
        };
        let detail = match &selected.model {
            Some(model) => format!("Model Name: {}", model.name),
            None => "Model unavailable".to_string(),
        };
        self.list_container_push_spacer();
        self.list_container_push(fg(ThemeColor::Muted, &format!("  {detail}")));
    }

    // --- list container 的小助手（避免在 update_list 里反复拼 Container API）---

    fn list_container_clear(&mut self) {
        self.list_container.clear();
    }

    fn list_container_push(&mut self, text: String) {
        self.list_container
            .add_child(Box::new(Text::with_padding(text, 0, 0)));
    }

    fn list_container_push_spacer(&mut self) {
        self.list_container.add_child(Box::new(Spacer::new()));
    }

    /// 按渲染顺序列出子组件（TS `Container.children`）。
    ///
    /// `render` / `handleMouse` 都走这份列表，因此子组件的行高与渲染顺序由这里统一决定；
    /// 可选的状态行只在构造时创建，顺序在组件生命周期内不变。每次调用都重新
    /// 构造 `Vec`，返回的引用只在本次调用内有效。
    fn children_mut(&mut self) -> Vec<&mut dyn Component> {
        let mut children: Vec<&mut dyn Component> = vec![
            &mut self.top_border,
            &mut self.spacer_top,
            &mut self.title_text,
            &mut self.session_hint_text,
            &mut self.spacer_after_hint,
            &mut self.search_input,
            &mut self.spacer_after_search,
            &mut self.list_container,
            &mut self.spacer_after_list,
        ];
        if let Some(status) = self.refresh_status_text.as_mut() {
            children.push(status);
        }
        children.push(&mut self.footer_text);
        children.push(&mut self.bottom_border);
        children
    }

    /// TS `Container.handleMouse`：把事件按行偏移转发给命中的子组件。
    ///
    /// 流程：
    /// 1. `event.y` 不在 `[0, event.height)` 内则视为未命中（返回 `None`）；
    /// 2. 若缓存宽度与 `event.width` 相同，复用 `mouse_layout` 的行高；否则现场渲染子组件
    ///    求高度（宽度变化后列表可能换行，不能沿用旧高度）；
    /// 3. 按顺序累加行高定位命中的子组件，把事件的 `y` 改成子组件局部坐标、`height` 改成该
    ///    子组件高度后调用 [`dispatch_mouse_event`]；没有任何子组件覆盖 `event.y` 时返回 `None`。
    ///
    /// 子组件请求焦点时，结果里的 `focus_target` 会被改成本组件地址：TS 的转发型容器定义了
    /// `handleInput`，焦点应留在选择器（按键仍由 `handle_input` 处理），而不是交给内部搜索框。
    fn handle_mouse(&mut self, event: &TuiMouseEvent) -> Option<TuiMouseEventResult> {
        if event.y < 0 || event.y as usize >= event.height {
            return None;
        }
        let cached = match &self.mouse_layout {
            Some((width, heights)) if *width == event.width => Some(heights.clone()),
            _ => None,
        };
        let heights: Vec<usize> = match cached {
            Some(heights) => heights,
            None => self
                .children_mut()
                .iter_mut()
                .map(|child| child.render(event.width).len())
                .collect(),
        };
        let mut child_y: isize = 0;
        let mut hit: Option<(usize, isize, usize)> = None;
        for (index, height) in heights.iter().enumerate() {
            if event.y >= child_y && event.y < child_y + *height as isize {
                hit = Some((index, event.y - child_y, *height));
                break;
            }
            child_y += *height as isize;
        }
        let (index, y, height) = hit?;
        let mut child_event = event.clone();
        child_event.y = y;
        child_event.height = height;
        let result = {
            let mut children = self.children_mut();
            let child = children.get_mut(index)?;
            dispatch_mouse_event(&mut **child, &child_event)
        };
        // TS `Container.handleMouse`：转发型容器定义了 `handleInput` 时，子控件请求焦点会把
        // 焦点留给自己（`ScopedModelsSelectorComponent` 定义了 `handleInput`）。
        result.map(|mut result| {
            if result.focus == Some(true) {
                let address = self as *const ScopedModelsSelectorComponent as *const () as usize;
                result.focus_target = Some(MouseFocusTarget::Address(address));
            }
            result
        })
    }

    /// 处理一次键盘输入（`handleInput(data)`）。
    ///
    /// 按固定优先级匹配，先命中者生效并 `return`：
    /// 1. `tui.select.up` / `tui.select.down`：在过滤结果中环绕移动；列表为空时不处理；
    /// 2. `app.models.reorderUp` / `reorderDown`：仅当 `enabled_ids` 是显式列表且选中项已启用
    ///    时移动一位，越界不动（不环绕），成功后标记 `is_dirty` 并通知宿主；
    /// 3. `tui.select.confirm`：切换选中项的启用状态；
    /// 4. `app.models.enableAll` / `app.models.clearAll`：搜索非空时只作用于当前过滤结果，
    ///    否则作用于全部模型；
    /// 5. `app.models.toggleProvider`：对选中项所属 provider 的全部模型做「全开 / 全清」；
    /// 6. `app.models.save`：先调用 `on_persist`，再清掉 `is_dirty` 并重绘 footer；
    /// 7. Ctrl+C：搜索非空时只清空搜索，否则取消；8. Escape：取消；
    /// 9. 其余输入交给搜索框，随后重建过滤列表。
    ///
    /// 键位匹配读取全局表（`get_keybindings`），因此 `app.models.*` 只有在宿主安装
    /// `kiana_config` 的键位表后才可用；`ctrl+c` 与 `escape` 用硬编码匹配，不查表。
    ///
    /// 会改变选择的分支统一执行「改 `enabled_ids` → `refresh()` → `notify_change()`」。
    /// 若 `refresh()` 因 `selected_index == -1` 触发 `update_list()` 的 panic（TS 的同一
    /// 行为），`notify_change()` 不会执行，宿主看到的仍是上一次的选择。
    pub fn handle_input(&mut self, data: &str) {
        // 顺序即优先级：命中任一分支后立即 return。
        let kb = get_keybindings();

        // 导航（环绕）。
        if kb.matches(data, "tui.select.up") {
            if self.filtered_items.is_empty() {
                return;
            }
            self.selected_index = if self.selected_index == 0 {
                self.filtered_items.len() as isize - 1
            } else {
                self.selected_index - 1
            };
            self.update_list();
            return;
        }
        if kb.matches(data, "tui.select.down") {
            if self.filtered_items.is_empty() {
                return;
            }
            self.selected_index = if self.selected_index == self.filtered_items.len() as isize - 1 {
                0
            } else {
                self.selected_index + 1
            };
            self.update_list();
            return;
        }

        // 重排已启用模型。
        let reorder_up = kb.matches(data, "app.models.reorderUp");
        let reorder_down = kb.matches(data, "app.models.reorderDown");
        if reorder_up || reorder_down {
            if self.enabled_ids.is_none() {
                return;
            }
            let selected = self.selected_item().map(|item| {
                (
                    item.full_id.clone(),
                    is_enabled(&self.enabled_ids, &item.full_id),
                )
            });
            if let Some((full_id, enabled)) = selected
                && enabled
            {
                let delta: isize = if reorder_up { -1 } else { 1 };
                let ids = self.enabled_ids.as_ref().expect("checked above");
                let current_index = ids
                    .iter()
                    .position(|id| *id == full_id)
                    .expect("enabled model is in enabledIds");
                let new_index = current_index as isize + delta;
                // 只在界内移动。
                if new_index >= 0 && (new_index as usize) < ids.len() {
                    self.enabled_ids = move_id(&self.enabled_ids, &full_id, delta);
                    self.is_dirty = true;
                    // TS: `selectedIndex += delta`（无下界检查）。过滤激活且选中的启用项不在
                    // 启用列表首位时，这里会得到 -1；随后 `refresh()` → `updateList()` 读取
                    // `filteredItems[-1].model` 抛 TypeError，`notifyChange()` 不会执行。
                    // Rust 用 isize 保留 -1，并在 `update_list` 里按同一文本 panic。
                    self.selected_index += delta;
                    self.refresh();
                    self.notify_change();
                }
            }
            return;
        }

        // Enter 切换。
        if kb.matches(data, "tui.select.confirm") {
            if let Some(full_id) = self.selected_item().map(|item| item.full_id.clone()) {
                self.enabled_ids = toggle(&self.enabled_ids, &self.all_ids, &full_id);
                self.is_dirty = true;
                self.refresh();
                self.notify_change();
            }
            return;
        }

        // 全部启用（搜索激活时只作用于过滤结果）。
        if kb.matches(data, "app.models.enableAll") {
            let target_ids: Option<Vec<String>> = if self.search_input.get_value().is_empty() {
                None
            } else {
                Some(
                    self.filtered_items
                        .iter()
                        .map(|item| item.full_id.clone())
                        .collect(),
                )
            };
            self.enabled_ids = enable_all(&self.enabled_ids, &self.all_ids, target_ids.as_deref());
            self.is_dirty = true;
            self.refresh();
            self.notify_change();
            return;
        }

        // 全部清除（搜索激活时只作用于过滤结果）。
        if kb.matches(data, "app.models.clearAll") {
            let target_ids: Option<Vec<String>> = if self.search_input.get_value().is_empty() {
                None
            } else {
                Some(
                    self.filtered_items
                        .iter()
                        .map(|item| item.full_id.clone())
                        .collect(),
                )
            };
            self.enabled_ids = clear_all(&self.enabled_ids, &self.all_ids, target_ids.as_deref());
            self.is_dirty = true;
            self.refresh();
            self.notify_change();
            return;
        }

        // 按 provider 切换当前项的 provider。
        if kb.matches(data, "app.models.toggleProvider") {
            let provider = self
                .selected_item()
                .and_then(|item| item.model.as_ref())
                .map(|model| model.provider.clone());
            if let Some(provider) = provider {
                let provider_ids: Vec<String> = self
                    .all_ids
                    .iter()
                    .filter(|id| {
                        self.models_by_id
                            .get(*id)
                            .is_some_and(|model| model.provider == provider)
                    })
                    .cloned()
                    .collect();
                let all_enabled = provider_ids
                    .iter()
                    .all(|id| is_enabled(&self.enabled_ids, id));
                self.enabled_ids = if all_enabled {
                    clear_all(&self.enabled_ids, &self.all_ids, Some(&provider_ids))
                } else {
                    enable_all(&self.enabled_ids, &self.all_ids, Some(&provider_ids))
                };
                self.is_dirty = true;
                self.refresh();
                self.notify_change();
            }
            return;
        }

        // 写入 settings。
        if kb.matches(data, "app.models.save") {
            let value = self.enabled_ids.clone();
            let future = (self.callbacks.on_persist)(value);
            run_callback(future);
            self.is_dirty = false;
            self.footer_text.set_text(self.get_footer_text());
            return;
        }

        // Ctrl+C：清空搜索，搜索为空时取消。
        if matches_key(data, "ctrl+c") {
            if !self.search_input.get_value().is_empty() {
                self.search_input.set_value("");
                self.refresh();
            } else {
                (self.callbacks.on_cancel)();
            }
            return;
        }

        // Escape：取消。
        if matches_key(data, "escape") {
            (self.callbacks.on_cancel)();
            return;
        }

        // 其余输入交给搜索框。
        self.search_input.handle_input(data);
        self.refresh();
    }
}

impl Component for ScopedModelsSelectorComponent {
    fn render(&mut self, width: usize) -> Vec<String> {
        // TS 的 render 是若干子组件的顺序拼接；这里按同一顺序渲染，状态写入与
        // `updateList()` 的时机一致，并记录 `Container.mouseLayout` 供鼠标命中使用。
        self.update_list();
        let mut lines = Vec::new();
        let mut heights = Vec::new();
        for child in self.children_mut() {
            let child_lines = child.render(width);
            heights.push(child_lines.len());
            lines.extend(child_lines);
        }
        self.mouse_layout = Some((width, heights));
        lines
    }

    fn handle_input(&mut self, data: &str) {
        ScopedModelsSelectorComponent::handle_input(self, data);
    }

    fn handle_mouse(&mut self, event: &TuiMouseEvent) -> Option<TuiMouseEventResult> {
        ScopedModelsSelectorComponent::handle_mouse(self, event)
    }

    fn as_focusable(&mut self) -> Option<&mut dyn Focusable> {
        Some(self)
    }
}

impl Focusable for ScopedModelsSelectorComponent {
    fn focused(&self) -> bool {
        self.focused
    }

    fn set_focused(&mut self, focused: bool) {
        self.focused = focused;
        self.search_input.set_focused(focused);
    }
}

#[cfg(test)]
mod tests {
    // 这里只覆盖模块私有纯函数的 TS 等价性；组件的构造、渲染、鼠标与回调行为由
    // `tests/components_model_selector.rs` 覆盖。
    use super::*;

    fn ids(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_string()).collect()
    }

    #[test]
    fn is_enabled_treats_none_as_all() {
        assert!(is_enabled(&None, "a"));
        assert!(is_enabled(&Some(ids(&["a"])), "a"));
        assert!(!is_enabled(&Some(ids(&["a"])), "b"));
    }

    #[test]
    fn normalize_enabled_collapses_to_none_only_when_it_covers_everything() {
        let all = ids(&["a", "b"]);
        assert_eq!(normalize_enabled(ids(&["a", "b"]), &all), None);
        assert_eq!(normalize_enabled(ids(&["b", "a"]), &all), None);
        // JS 只比较长度与成员关系（重复项也折叠）。
        assert_eq!(normalize_enabled(ids(&["a", "a"]), &all), None);
        assert_eq!(normalize_enabled(ids(&["a"]), &all), Some(ids(&["a"])));
    }

    #[test]
    fn toggle_matches_ts() {
        let all = ids(&["a", "b", "c"]);
        assert_eq!(toggle(&None, &all, "b"), Some(ids(&["a", "c"])));
        assert_eq!(
            toggle(&Some(ids(&["a", "b"])), &all, "a"),
            Some(ids(&["b"]))
        );
        // 加回最后一个模型时折叠回 null。
        assert_eq!(toggle(&Some(ids(&["a", "b"])), &all, "c"), None);
    }

    #[test]
    fn enable_all_and_clear_all_match_ts() {
        let all = ids(&["a", "b", "c"]);
        assert_eq!(enable_all(&None, &all, None), None);
        assert_eq!(enable_all(&Some(ids(&["a"])), &all, None), None);
        assert_eq!(
            enable_all(&Some(ids(&["a"])), &all, Some(&ids(&["b"]))),
            Some(ids(&["a", "b"]))
        );
        assert_eq!(clear_all(&None, &all, None), Some(Vec::new()));
        assert_eq!(
            clear_all(&None, &all, Some(&ids(&["b"]))),
            Some(ids(&["a", "c"]))
        );
        assert_eq!(
            clear_all(&Some(ids(&["a", "b"])), &all, None),
            Some(Vec::new())
        );
        assert_eq!(
            clear_all(&Some(ids(&["a", "b"])), &all, Some(&ids(&["a"]))),
            Some(ids(&["b"]))
        );
    }

    #[test]
    fn move_does_not_wrap() {
        let ids_list = ids(&["a", "b", "c"]);
        assert_eq!(move_id(&None, "a", 1), None);
        assert_eq!(
            move_id(&Some(ids_list.clone()), "a", 1),
            Some(ids(&["b", "a", "c"]))
        );
        assert_eq!(
            move_id(&Some(ids_list.clone()), "a", -1),
            Some(ids_list.clone())
        );
        assert_eq!(
            move_id(&Some(ids_list.clone()), "c", 1),
            Some(ids_list.clone())
        );
        assert_eq!(
            move_id(&Some(ids_list.clone()), "missing", 1),
            Some(ids_list)
        );
    }

    #[test]
    fn get_sorted_ids_keeps_enabled_order_then_the_rest() {
        let all = ids(&["a", "b", "c"]);
        assert_eq!(get_sorted_ids(&None, &all), all);
        assert_eq!(
            get_sorted_ids(&Some(ids(&["c", "a"])), &all),
            ids(&["c", "a", "b"])
        );
    }
}
