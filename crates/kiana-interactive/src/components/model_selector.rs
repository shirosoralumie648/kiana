//! components/model-selector.ts。
//!
//! `/model` 的模型选择器：模糊搜索、当前模型标记、all/scoped 作用域切换、15 秒有界刷新与
//! 进程内共享的目录刷新。
//!
//! TS: `packages/coding-agent/src/modes/interactive/components/model-selector.ts`
//!
//! ## 与 TS 的差异（仅运行时适配）
//!
//! - `TUI` 参数换成 `Option<RequestRenderCallback>`（`tui.requestRender()` 的宿主注入）；
//!   构造末尾的 `void this.refreshModels()` 用 `spawn_local` 启动（交互模式整体跑在单线程
//!   `LocalSet` 上，见 `spec/ca-interactive-components.md` §8.1）。
//! - `ModelRuntime` 通过 [`ModelSelectorRuntime`] 抽象注入，生产代码传真实运行时，测试可传
//!   假运行时（对应 TS 的 `vi.spyOn(runtime, ...)`）。
//! - `keyDisplayText` / `getKeybindings` 读 `kiana-tui` 的全局键位表。宿主必须把
//!   `kiana_config::keybindings::keybindings()`（TUI + app.*）装进 `kiana_tui::set_keybindings`。
//!
//! ## 职责与调用关系
//!
//! 宿主（`InteractiveMode::show_model_selector`，见 `interactive_mode/commands/model.rs`）
//! 把当前模型、scoped 列表、默认模型引用与三个回调（`on_select` / `on_select_as_default` /
//! `on_cancel`）交给本组件。组件只负责展示与选择，真正的模型切换由宿主在回调里完成
//! （`session.set_model`）；本文件不写会话或配置。
//!
//! ## 执行流程
//!
//! 1. 构造（[`ModelSelectorComponent::new`]）：按 `scoped_models` 是否为空决定初始 scope
//!    （非空 → `Scoped`，空 → `All`）并建好固定子组件；`load_models_from_snapshot()` 用
//!    运行时快照填充列表，再按初始搜索词过滤；最后调用一次 `request_render` 并启动后台
//!    刷新（总是刷新，与是否传入初始搜索词无关）。
//! 2. 后台刷新：`refresh_model_catalogs` 与 15 秒定时器竞速；`Ok` 结果（即使带有逐 provider
//!    错误）会重载快照并重新过滤，`Err`/超时只更新状态行/错误行，列表继续显示缓存内容。
//! 3. 输入：Tab 切 scope、上下移动、确认/取消/保存默认都有独立分支；其余按键交给内部
//!    搜索框并重新过滤。需要触发宿主回调的分支只返回 `PendingAction`，由
//!    [`ModelSelectorComponent::handle_input`] 在释放 `RefCell` 借用后统一派发。
//!
//! ## 数据结构与不变量
//!
//! - `all_models` 来自运行时快照并经 `sort_models` 排序（当前模型 → 默认模型 → provider
//!   `locale_compare`）；`scoped_model_items` **保持调用方顺序**，不参与排序（回归测试
//!   3217 锁定这一点）。
//! - `active_models` 是上述两者之一的克隆，`filtered_models` 是它经搜索过滤后的结果；
//!   只要 `filtered_models` 非空，`selected_index` 一定小于它的长度（`update_list` 在
//!   非空分支直接用它索引）。
//! - 列表窗口固定最多 10 行（`update_list` 的 `max_visible`），以选中行为中心并夹在两端。
//!
//! ## 失败模式与注意事项
//!
//! - 所有着色都经 [`THEME`]（`ThemeProxy`）在调用时读取全局主题；主题未初始化时本文件的
//!   `fg` 辅助函数会 panic（对应 TS 在渲染阶段抛异常）。构造与 `render` 一定会着色，
//!   [`ModelSelectorComponent::handle_input`] 的过滤/scope 分支也会，因此这些入口都要求
//!   宿主已初始化主题。
//! - 组件状态放在 `Rc<RefCell<...>>` 里；[`ModelSelectorComponent::get_search_input`] 返回的
//!   `Ref` 在存活期间保持借用，此时再调用其它 `&mut self` 方法会 panic（`BorrowMutError`）。
//! - `dispose` 只置 `closed` 并取消刷新令牌，不清理回调；之后到来的按键仍会
//!   被处理并触发回调（`closed` 只让刷新结果失效，不拦截输入）。
//! - 后台刷新用 `tokio::task::spawn_local` 启动，因此组件只能在 `LocalSet` 上下文中构造
//!   （交互模式与测试都这样安排）。

use std::any::Any;
use std::cell::{Cell, Ref, RefCell};
use std::collections::HashSet;
use std::rc::Rc;
use std::sync::Arc;

use kiana_ai::error::AiError;
use kiana_ai::models::ModelsRefreshResult;
use kiana_ai::types::Model;
use kiana_config::models::runtime::ModelRuntime;
use kiana_theme::{THEME, ThemeColor};
use kiana_tui::components::input::{Input, InputOptions};
use kiana_tui::components::loader::RequestRenderCallback;
use kiana_tui::components::spacer::Spacer;
use kiana_tui::components::text_component::Text;
use kiana_tui::fuzzy::fuzzy_filter;
use kiana_tui::keybindings::get_keybindings;
use kiana_tui::text::trim_js;
use kiana_tui::tui::{
    Component, Container, Focusable, MouseFocusTarget, TuiMouseEvent, TuiMouseEventResult,
    dispatch_mouse_event,
};
use kiana_ui_kit::DynamicBorder;
use kiana_ui_kit::keybinding_hints::{key_display_text, key_hint};
use tokio_util::sync::CancellationToken;

use crate::model_catalog_refresh::{ModelCatalogRuntime, refresh_model_catalogs};
use crate::model_search::{get_distinct_model_name, get_model_selector_search_text};

/// 按全局主题着色（TS `theme.fg(token, str)`）。
///
/// [`THEME`] 是 `ThemeProxy`：每次调用都重新读取当前全局主题，颜色是在调用时解析的
/// （不是编译期常量）；主题未初始化时 `fg` 返回 `Err`，这里的 `expect` 会 panic（对应
/// TS 在同样情况下抛异常）。本文件的构造与渲染路径都依赖这个前提。
fn fg(color: ThemeColor, text: &str) -> String {
    THEME
        .fg(color, text)
        .expect("theme must be initialized before rendering (TS throws here)")
}

/// TS: `ModelRuntime` 中 `/model` 选择器用到的子集。
///
/// 生产实现是 [`ModelRuntime`]；测试可以注入假运行时。`identity` / `refresh_catalogs` 由
/// [`ModelCatalogRuntime`] 提供。
///
/// 各方法在本文件中的使用点：
/// - `get_available_snapshot`：`load_models_from_snapshot` 读取完整快照（不触发刷新）；
/// - `get_model`：刷新后用新实例替换 scoped 条目，取不到时保留旧对象；
/// - `get_error`：刷新成功且没有逐 provider 错误时，用运行时的配置/鉴权错误作为错误行。
///
/// trait 要求 `Send + Sync`（继承自 [`ModelCatalogRuntime`]）：共享刷新任务经
/// `tokio::spawn` 创建（见 `model_catalog_refresh`），这个 API 要求 future 为 `Send`。
/// `ModelRuntime` 是 `Arc` 句柄，克隆开销小。
pub trait ModelSelectorRuntime: ModelCatalogRuntime {
    /// `getAvailableSnapshot()`：返回当前缓存的可用模型快照（不触发刷新）。
    fn get_available_snapshot(&self) -> Vec<Model>;
    /// `getModel(provider, id)`：按 provider+id 取模型；`None` 表示目录里没有这一项。
    fn get_model(&self, provider_id: &str, model_id: &str) -> Option<Model>;
    /// `getError()`：运行时的配置/鉴权错误；组件把 `None` 与空串都视为「没有错误」。
    fn get_error(&self) -> Option<String>;
}

impl ModelSelectorRuntime for ModelRuntime {
    fn get_available_snapshot(&self) -> Vec<Model> {
        ModelRuntime::get_available_snapshot(self)
    }

    fn get_model(&self, provider_id: &str, model_id: &str) -> Option<Model> {
        ModelRuntime::get_model(self, provider_id, model_id)
    }

    fn get_error(&self) -> Option<String> {
        ModelRuntime::get_error(self)
    }
}

/// TS: scoped 模型条目（`{ model, thinkingLevel? }`）。
///
/// `scoped_models` 的顺序由调用方（会话的 scoped 配置）决定，选择器不排序也不去重；
/// 每次加载快照时只把 `model` 换成运行时里的同 provider/id 新实例，`thinking_level` 原样带回。
#[derive(Debug, Clone)]
pub struct ScopedModelItem {
    /// scoped 列表里的模型。刷新后会被同 provider/id 的新实例覆盖；运行时找不到时保留旧值。
    pub model: Model,
    /// TS 的 `thinkingLevel`。本选择器不读取它，只在重载快照时原样保留。
    pub thinking_level: Option<String>,
}

/// TS: `DefaultModelReference`（`{ provider, id }`，注意不是 `Model` 对象）。
///
/// 用于三处：排序时默认模型排在当前模型之后、行尾渲染 ` · default` 徽标、以及输入
/// `default` 前缀时把默认模型置顶。匹配只比较 `provider` + `id`，不看模型对象的其它字段。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DefaultModelReference {
    /// 默认模型所属的 provider id。
    pub provider: String,
    /// 默认模型的 id（与 `provider` 一起构成唯一键）。
    pub id: String,
}

/// `ModelScope = "all" | "scoped"`。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ModelScope {
    All,
    Scoped,
}

/// `ModelItem`。
///
/// 列表项：模型对象加上预先抽出的 provider/id（渲染、排序与 `default` 去重键都直接用它们，
/// 不必反复从 `Model` 取字段）。
#[derive(Debug, Clone)]
struct ModelItem {
    provider: String,
    id: String,
    model: Model,
}

/// `modelsAreEqual(currentModel, item.model)`。
///
/// `get_available_snapshot()` 只返回 chat 模型，两边的 `getModelType` 恒为 `"chat"`，因此
/// 与 `modelsAreEqual` 等价（provider + id；`model_type` 不参与）。
///
/// 调用点：排序比较、`✓` 当前标记、加载快照/切换 scope 时的选中行定位；不比较 name/api/
/// base_url 等展示字段，因此目录更新导致的对象差异不会被误判成「换了模型」。
fn models_equal(current: Option<&Model>, item: &Model) -> bool {
    current.is_some_and(|current| current.id == item.id && current.provider == item.provider)
}

/// `sortModels`：当前模型优先 → 默认模型其次 → `provider.localeCompare`（稳定排序）。
///
/// 比较器在两项都相同（含同一 provider 的多个模型）时返回 `Ordering::Equal`，由
/// `Vec::sort_by` 的稳定性保留快照原顺序；这里没有 provider 内的 id 次级排序（与 TS 相同，
/// 依赖 `Array.prototype.sort` 的稳定性）。当前/默认模型都按 provider+id 比较，不看对象指针。
fn sort_models(
    models: Vec<ModelItem>,
    current: Option<&Model>,
    default_model: Option<&DefaultModelReference>,
) -> Vec<ModelItem> {
    let is_default = |model: &Model| {
        default_model.is_some_and(|reference| {
            reference.provider == model.provider && reference.id == model.id
        })
    };
    let mut sorted = models;
    sorted.sort_by(|a, b| {
        let a_is_current = models_equal(current, &a.model);
        let b_is_current = models_equal(current, &b.model);
        if a_is_current != b_is_current {
            return if a_is_current {
                std::cmp::Ordering::Less
            } else {
                std::cmp::Ordering::Greater
            };
        }
        let a_is_default = is_default(&a.model);
        let b_is_default = is_default(&b.model);
        if a_is_default != b_is_default {
            return if a_is_default {
                std::cmp::Ordering::Less
            } else {
                std::cmp::Ordering::Greater
            };
        }
        kiana_tui::autocomplete::locale_compare(&a.provider, &b.provider)
    });
    sorted
}

/// `handleInput` 里需要延后到 `RefCell` 借用释放后再调用的宿主回调。
///
/// `ModelSelectorInner::handle_input` 在 `&mut self` 上运行，而宿主回调可以做任意事
/// （访问共享状态、请求重绘、替换选择器）；在持有 `RefCell` 借用时调用它会撞上运行时
/// 借用检查。因此这里只记录意图，实际调用由 [`ModelSelectorComponent::handle_input`] 在
/// 借用块结束后完成。
enum PendingAction {
    None,
    Select(Model),
    SelectAsDefault(Model),
    Cancel,
}

/// 选择器的全部状态（单线程 UI 线程上以 `Rc<RefCell<...>>` 共享给刷新任务）。
///
/// `all_models` / `scoped_model_items` 是数据源，`active_models` 与 `filtered_models` 是
/// 依次派生的视图；`selected_index` 在 `filtered_models` 非空时必须是有效下标（见模块文档
/// 的不变量）。`refresh_abort` 是后台刷新的取消令牌，`closed` 是 `dispose` 的幂等标志。
struct ModelSelectorInner {
    runtime: Arc<dyn ModelSelectorRuntime>,
    request_render: Option<RequestRenderCallback>,
    current_model: Option<Model>,
    scoped_models: Vec<ScopedModelItem>,
    default_model: Option<DefaultModelReference>,
    scope: ModelScope,
    all_models: Vec<ModelItem>,
    scoped_model_items: Vec<ModelItem>,
    active_models: Vec<ModelItem>,
    filtered_models: Vec<ModelItem>,
    selected_index: usize,
    /// 刷新失败/超时或运行时错误时显示在列表位置的错误文本；非空时按 `\n` 拆成多行渲染。
    error_message: Option<String>,
    /// 列表末尾的状态行；为空串时不渲染（初始值是 `Refreshing model catalogs…`）。
    refresh_status_message: String,
    /// 状态行颜色：成功为绿色，其余为 muted；初值 `false`。
    refresh_status_success: bool,
    /// `dispose` 置位；刷新任务完成时若已置位就丢弃结果、不再请求重绘。
    closed: bool,
    /// 后台刷新与 `dispose` 共享的取消令牌（15 秒超时也会取消它）。
    refresh_abort: CancellationToken,
    /// `Input::on_submit` 只能捕获独立共享状态，用它把「已提交」传回 `handle_input`。
    submit_requested: Rc<Cell<bool>>,
    on_select: Option<Box<dyn FnMut(Model)>>,
    on_cancel: Option<Box<dyn FnMut()>>,
    on_select_as_default: Option<Box<dyn FnMut(Model)>>,
    // 可选子组件：构造时按输入决定是否创建，生命周期内不增删；鼠标布局缓存依赖这个固定顺序。
    scope_text: Option<Text>,
    scope_hint_text: Option<Text>,
    warning_text: Option<Text>,
    hint_text: Option<Text>,
    search_input: Input,
    list_container: Container,
    top_border: DynamicBorder,
    bottom_border: DynamicBorder,
    spacer_top: Spacer,
    spacer_before_search: Spacer,
    spacer_after_search: Spacer,
    spacer_after_list: Spacer,
    /// TS `Container.mouseLayout`：最近一次 render 的子组件高度（按渲染顺序）。
    mouse_layout: Option<(usize, Vec<usize>)>,
}

impl ModelSelectorInner {
    /// 初始化全部状态并完成第一次列表构建；后台刷新由调用方（`with_runtime`）启动，
    /// 这里不触发任何网络请求。
    ///
    /// 初始 scope 只看 `scoped_models` 是否为空；为空时 `scoped_model_items` 始终为空，
    /// Tab 分支不会生效，界面改显示 `/login` 警告行。
    #[allow(clippy::too_many_arguments)]
    fn new(
        runtime: Arc<dyn ModelSelectorRuntime>,
        request_render: Option<RequestRenderCallback>,
        current_model: Option<Model>,
        scoped_models: Vec<ScopedModelItem>,
        on_select: Box<dyn FnMut(Model)>,
        on_cancel: Box<dyn FnMut()>,
        initial_search_input: Option<String>,
        on_select_as_default: Option<Box<dyn FnMut(Model)>>,
        default_model: Option<DefaultModelReference>,
    ) -> Self {
        // 初始 scope：有 scoped 列表就从 scoped 页签开始，否则只能看全部。
        let scope = if scoped_models.is_empty() {
            ModelScope::All
        } else {
            ModelScope::Scoped
        };
        let mut inner = ModelSelectorInner {
            runtime,
            request_render,
            current_model,
            scoped_models,
            default_model,
            scope,
            all_models: Vec::new(),
            scoped_model_items: Vec::new(),
            active_models: Vec::new(),
            filtered_models: Vec::new(),
            selected_index: 0,
            error_message: None,
            refresh_status_message: "Refreshing model catalogs…".to_string(),
            refresh_status_success: false,
            closed: false,
            refresh_abort: CancellationToken::new(),
            submit_requested: Rc::new(Cell::new(false)),
            on_select: Some(on_select),
            on_cancel: Some(on_cancel),
            on_select_as_default,
            scope_text: None,
            scope_hint_text: None,
            warning_text: None,
            hint_text: None,
            search_input: Input::new(InputOptions::default()),
            list_container: Container::new(),
            top_border: DynamicBorder::default(),
            bottom_border: DynamicBorder::default(),
            spacer_top: Spacer::new(),
            spacer_before_search: Spacer::new(),
            spacer_after_search: Spacer::new(),
            spacer_after_list: Spacer::new(),
            mouse_layout: None,
        };

        // 无 scoped 列表时 scoped 页签没有内容：不建 scope 行，改显示 /login 警告。
        if inner.scoped_models.is_empty() {
            inner.warning_text = Some(Text::with_padding(
                fg(
                    ThemeColor::Warning,
                    "Only showing models from configured providers. Use /login to add providers.",
                ),
                0,
                0,
            ));
        } else {
            inner.scope_text = Some(Text::with_padding(inner.get_scope_text(), 0, 0));
            inner.scope_hint_text = Some(Text::with_padding(inner.get_scope_hint_text(), 0, 0));
        }
        // 只有宿主提供保存默认模型的回调时，才显示第三行操作提示。
        if inner.on_select_as_default.is_some() {
            let hint = format!(
                "  {} to select · {} to set as default · {} to cancel",
                key_display_text("tui.select.confirm"),
                key_display_text("app.models.save"),
                key_display_text("tui.select.cancel"),
            );
            inner.hint_text = Some(Text::with_padding(fg(ThemeColor::Dim, &hint), 0, 0));
        }

        // TS 的 `searchInput.onSubmit` 在输入框内部被调用；这里只置位，避免 RefCell 重入。
        // 闭包在 `Input::handle_input` 内部同步执行，那时 `search_input` 已被可变借用，
        // 无法再借用 `inner`；`Rc<Cell<bool>>` 是它与本结构共享的最小状态。
        let submit_requested = Rc::clone(&inner.submit_requested);
        inner.search_input.on_submit = Some(Box::new(move |_value: &str| {
            submit_requested.set(true);
        }));
        let initial_search = initial_search_input.filter(|value| !value.is_empty());
        if let Some(initial) = &initial_search {
            inner.search_input.set_value(initial.clone());
        }

        // 先渲染当前快照，再在后台刷新。
        inner.load_models_from_snapshot();
        match &initial_search {
            Some(initial) => inner.filter_models(initial),
            None => inner.update_list(),
        }
        inner
    }

    /// `loadModelsFromSnapshot()`：用运行时快照重建两份模型列表，并把选中行定位到当前模型。
    ///
    /// 阶段：
    /// 1. 快照 → `ModelItem`，按「当前模型 → 默认模型 → provider」排序（只影响 `all_models`）。
    /// 2. scoped 条目按**原顺序**逐个到运行时查新实例：查到就替换 `model`、保留
    ///    `thinking_level`；查不到保留旧对象（目录暂时不可用时不丢条目）。
    /// 3. 重建 `scoped_model_items`，按当前 scope 选 `active_models`，再复制成
    ///    `filtered_models`（调用方随后通常还会按搜索词过滤）。
    /// 4. 选中行：优先当前模型的下标；找不到时把旧下标夹到新列表末尾。
    ///
    /// 注意：这里不排序 `scoped_model_items`，也不去重——顺序由调用方的 scoped 配置决定。
    fn load_models_from_snapshot(&mut self) {
        // 全部模型：排序后作为 All 页签的数据源。
        let models = self
            .runtime
            .get_available_snapshot()
            .into_iter()
            .map(|model| ModelItem {
                provider: model.provider.clone(),
                id: model.id.clone(),
                model,
            })
            .collect();
        self.all_models = sort_models(
            models,
            self.current_model.as_ref(),
            self.default_model.as_ref(),
        );

        // scoped 列表：保持调用方顺序，只把模型对象换成刷新后的新实例。
        let runtime = Arc::clone(&self.runtime);
        let previous_scoped = std::mem::take(&mut self.scoped_models);
        self.scoped_models = previous_scoped
            .into_iter()
            .map(
                |scoped| match runtime.get_model(&scoped.model.provider, &scoped.model.id) {
                    Some(model) => ScopedModelItem {
                        model,
                        thinking_level: scoped.thinking_level,
                    },
                    // 拿不到新实例则保留旧对象。
                    None => scoped,
                },
            )
            .collect();
        // 再投影成列表项；provider/id 从更新后的模型上重新取。
        self.scoped_model_items = self
            .scoped_models
            .iter()
            .map(|scoped| ModelItem {
                provider: scoped.model.provider.clone(),
                id: scoped.model.id.clone(),
                model: scoped.model.clone(),
            })
            .collect();
        // 当前页签的完整数据源（未过滤）。
        self.active_models = match self.scope {
            ModelScope::Scoped => self.scoped_model_items.clone(),
            ModelScope::All => self.all_models.clone(),
        };
        self.filtered_models = self.active_models.clone();
        // 选中行优先跟随当前模型；当前模型不在本页签时，把旧下标夹到有效范围。
        let current_index = self
            .filtered_models
            .iter()
            .position(|item| models_equal(self.current_model.as_ref(), &item.model));
        self.selected_index = match current_index {
            Some(index) => index,
            None => self
                .selected_index
                .min(self.filtered_models.len().saturating_sub(1)),
        };
    }

    /// `getScopeText()`：生成 `Scope: all | scoped` 一行。
    ///
    /// 选中侧用 accent，另一侧用 muted。只在构造与 `set_scope` 时调用；结果缓存在
    /// `scope_text` 这个 `Text` 子组件里，渲染阶段不会重新计算。
    fn get_scope_text(&self) -> String {
        let all_text = if self.scope == ModelScope::All {
            fg(ThemeColor::Accent, "all")
        } else {
            fg(ThemeColor::Muted, "all")
        };
        let scoped_text = if self.scope == ModelScope::Scoped {
            fg(ThemeColor::Accent, "scoped")
        } else {
            fg(ThemeColor::Muted, "scoped")
        };
        format!(
            "{}{}{}{}",
            fg(ThemeColor::Muted, "Scope: "),
            all_text,
            fg(ThemeColor::Muted, " | "),
            scoped_text
        )
    }

    /// `getScopeHintText()`：Tab 键提示行（键名来自全局键位表）。
    fn get_scope_hint_text(&self) -> String {
        key_hint("tui.input.tab", "scope") + &fg(ThemeColor::Muted, " (all/scoped)")
    }

    /// `isDefaultModel(model)`：与 [`DefaultModelReference`] 的 provider+id 相同即为默认模型。
    ///
    /// 排序、`default` 前缀搜索、` · default` 徽标三处共用这个判定，都不看对象指针。
    fn is_default_model(&self, model: &Model) -> bool {
        self.default_model.as_ref().is_some_and(|reference| {
            reference.provider == model.provider && reference.id == model.id
        })
    }

    /// `isDefaultSearch(query)`：JS `trim().toLowerCase()` 后非空，且是 `"default"` 的前缀。
    ///
    /// 命中时 `filter_models` 只是把默认模型置顶，并**不**过滤掉其它结果。
    fn is_default_search(&self, query: &str) -> bool {
        let normalized = trim_js(query).to_lowercase();
        !normalized.is_empty() && "default".starts_with(&normalized)
    }

    /// `setScope(scope)`：切换数据源并刷新 scope 行。
    ///
    /// 先按新页签把选中行定位到当前模型（找不到回 0），再用当前搜索词重新过滤。注意有
    /// 查询时 `filter_models` 会把选中行重置为 0，因此上面的定位只在查询为空时保留。
    /// 与当前 scope 相同时直接返回（连文本都不重建）。
    fn set_scope(&mut self, scope: ModelScope) {
        if self.scope == scope {
            return;
        }
        self.scope = scope;
        self.active_models = match self.scope {
            ModelScope::Scoped => self.scoped_model_items.clone(),
            ModelScope::All => self.all_models.clone(),
        };
        let current_index = self
            .active_models
            .iter()
            .position(|item| models_equal(self.current_model.as_ref(), &item.model));
        self.selected_index = current_index.unwrap_or(0);
        let query = self.search_input.get_value().to_string();
        self.filter_models(&query);
        let scope_text = self.get_scope_text();
        if let Some(text) = self.scope_text.as_mut() {
            text.set_text(scope_text);
        }
    }

    /// `filterModels(query)`：按搜索词重建 `filtered_models`，并重算选中行。
    ///
    /// 查询非空时用 [`fuzzy_filter`] 过滤 `active_models`；搜索文本由
    /// `get_model_selector_search_text` 生成，默认模型额外拼上 `" default"`，因此输入
    /// `default` 也能命中默认模型。若查询是 `"default"` 的前缀（`is_default_search`），
    /// 默认模型被整体置顶，其余模糊匹配结果仍然保留（不是「只显示默认」）。
    ///
    /// 查询为空时直接展示全部 `active_models`。选中行：有查询 → 第一行；无查询 → 保持
    /// 原位并 clamp 到列表长度。
    ///
    /// 边界：空串与「仅含 JS 空白」的查询不同——后者在 [`fuzzy_filter`] 内部会被
    /// `trim_js` 成空并直接返回全部条目，但这里仍按「查询非空」把选中行归零。
    fn filter_models(&mut self, query: &str) {
        if !query.is_empty() {
            let filtered = fuzzy_filter(self.active_models.clone(), query, |item| {
                let default_text = if self.is_default_model(&item.model) {
                    " default"
                } else {
                    ""
                };
                format!(
                    "{}{default_text}",
                    get_model_selector_search_text(
                        &item.id,
                        &item.provider,
                        Some(&item.model.name)
                    )
                )
            });
            if self.is_default_search(query) {
                // 默认搜索：默认模型整体前置，再从模糊结果里去掉重复项；去重键用
                // `provider\0id` 拼接（与 TS 的 Set 键相同）。
                let default_items: Vec<ModelItem> = self
                    .active_models
                    .iter()
                    .filter(|item| self.is_default_model(&item.model))
                    .cloned()
                    .collect();
                let default_keys: HashSet<String> = default_items
                    .iter()
                    .map(|item| format!("{}\0{}", item.provider, item.id))
                    .collect();
                self.filtered_models = default_items
                    .into_iter()
                    .chain(filtered.into_iter().filter(|item| {
                        !default_keys.contains(&format!("{}\0{}", item.provider, item.id))
                    }))
                    .collect();
            } else {
                self.filtered_models = filtered;
            }
        } else {
            self.filtered_models = self.active_models.clone();
        }
        // 输入查询时把选择移到第一行；清空时保持当前位置并 clamp。
        self.selected_index = if !query.is_empty() {
            0
        } else {
            self.selected_index
                .min(self.filtered_models.len().saturating_sub(1))
        };
        self.update_list();
    }

    /// `updateList()`：清空并重建 `list_container`（行、滚动指示、错误/空提示、名称行、状态行）。
    ///
    /// 每次调用都先 `clear()` 再按当前 `filtered_models` / `selected_index` 重建，不复用旧
    /// 子项；调用点包括构造、过滤、上下移动、刷新失败分支以及每一帧 `render_lines`。
    ///
    /// 窗口固定 10 行：`start_index` 以选中行为中心并夹在 `[0, len - 10]`（上界为负时被压成
    /// 0），只有窗口没有覆盖全部条目时才追加 `  (i/N)`。
    ///
    /// 内容优先级：非空 `error_message`（按 `\n` 拆行）> 空列表提示 > 选中项名称行；
    /// 刷新状态行最后追加。
    fn update_list(&mut self) {
        self.list_container.clear();
        let max_visible = 10usize;
        let len = self.filtered_models.len();
        // 以选中行为中心开窗，并把起点夹在 [0, len - max_visible]；len < 10 时上界为负，
        // 被 max(0) 压成 0，窗口自然缩短到 len。
        let start_index = {
            let start = self.selected_index as i64 - (max_visible / 2) as i64;
            let upper = len as i64 - max_visible as i64;
            start.min(upper).max(0) as usize
        };
        let end_index = (start_index + max_visible).min(len);

        // 行格式固定（布局契约）：光标 + 当前标记 + id + (distinct name) + [provider] + ·default。
        let mut rows: Vec<String> = Vec::new();
        for index in start_index..end_index {
            let item = &self.filtered_models[index];
            let is_selected = index == self.selected_index;
            let is_current = models_equal(self.current_model.as_ref(), &item.model);
            let is_default = self.is_default_model(&item.model);
            let default_badge = if is_default {
                fg(ThemeColor::Muted, " · default")
            } else {
                String::new()
            };
            let cursor = if is_selected {
                fg(ThemeColor::Accent, "→ ")
            } else {
                "  ".to_string()
            };
            let current_marker = if is_current {
                fg(ThemeColor::Accent, "✓ ")
            } else {
                "  ".to_string()
            };
            let model_text = if is_selected {
                fg(ThemeColor::Accent, &item.id)
            } else {
                item.id.clone()
            };
            // name 只有包含 id 无法覆盖的词时才显示（避免把与 id 等价的别名重复一遍）。
            let name = get_distinct_model_name(&item.id, Some(&item.model.name));
            let name_text = match name {
                Some(name) => fg(ThemeColor::Muted, &format!(" ({name})")),
                None => String::new(),
            };
            let provider_badge = fg(ThemeColor::Muted, &format!("[{}]", item.provider));
            rows.push(format!(
                "{cursor}{current_marker}{model_text}{name_text} {provider_badge}{default_badge}"
            ));
        }
        for row in rows {
            self.list_container
                .add_child(Box::new(Text::with_padding(row, 0, 0)));
        }

        // 窗口未覆盖全部条目时才显示滚动指示（1 基的选中序号 + 总数）。
        if start_index > 0 || end_index < len {
            self.list_container.add_child(Box::new(Text::with_padding(
                fg(
                    ThemeColor::Muted,
                    &format!("  ({}/{})", self.selected_index + 1, len),
                ),
                0,
                0,
            )));
        }

        // TS 的 `if (this.errorMessage)`：空串与 undefined 一样是 falsy。
        if let Some(error_message) = self
            .error_message
            .as_deref()
            .filter(|message| !message.is_empty())
        {
            // 错误按 `\n` 拆成多行（muted/空列表提示此时不渲染）。
            for line in error_message.split('\n') {
                self.list_container.add_child(Box::new(Text::with_padding(
                    fg(ThemeColor::Error, line),
                    0,
                    0,
                )));
            }
        } else if len == 0 {
            self.list_container.add_child(Box::new(Text::with_padding(
                fg(ThemeColor::Muted, "  No matching models"),
                0,
                0,
            )));
        } else {
            // 无错误且列表非空：显示选中模型的完整名称（name 为空串时仍输出前缀）。
            let selected = &self.filtered_models[self.selected_index];
            let detail = fg(
                ThemeColor::Muted,
                &format!("  Model Name: {}", selected.model.name),
            );
            self.list_container.add_child(Box::new(Spacer::new()));
            self.list_container
                .add_child(Box::new(Text::with_padding(detail, 0, 0)));
        }
        if !self.refresh_status_message.is_empty() {
            let color = if self.refresh_status_success {
                ThemeColor::Success
            } else {
                ThemeColor::Muted
            };
            let status = fg(color, &format!("  {}", self.refresh_status_message));
            self.list_container.add_child(Box::new(Spacer::new()));
            self.list_container
                .add_child(Box::new(Text::with_padding(status, 0, 0)));
        }
    }

    /// `handleInput(keyData)`（回调以 [`PendingAction`] 延后执行）。
    ///
    /// 分支顺序固定：Tab → up → down → confirm → cancel → save → 其它。每个分支
    /// 处理完即返回，因此同一按键被多个键位绑定时只有第一个匹配的分支生效。
    ///
    /// 选中/取消/存默认都会先 `dispose()`（与 TS 一样，按这些键即关闭选择器）；但只有
    /// confirm/save 要求列表里有选中项——空列表时它们是 no-op，不会 dispose。Tab 只在
    /// `scoped_model_items` 非空时切换 scope。
    fn handle_input(&mut self, key_data: &str) -> PendingAction {
        let kb = get_keybindings();
        if kb.matches(key_data, "tui.input.tab") {
            // 没有 scoped 项时 Tab 什么也不做（界面上也不会有 scope 提示行）。
            if !self.scoped_model_items.is_empty() {
                let next_scope = if self.scope == ModelScope::All {
                    ModelScope::Scoped
                } else {
                    ModelScope::All
                };
                self.set_scope(next_scope);
                let hint_text = self.get_scope_hint_text();
                if let Some(hint) = self.scope_hint_text.as_mut() {
                    hint.set_text(hint_text);
                }
            }
            return PendingAction::None;
        }
        if kb.matches(key_data, "tui.select.up") {
            if self.filtered_models.is_empty() {
                return PendingAction::None;
            }
            // 环绕移动：第一项再上移回到末尾。
            self.selected_index = if self.selected_index == 0 {
                self.filtered_models.len() - 1
            } else {
                self.selected_index - 1
            };
            self.update_list();
        } else if kb.matches(key_data, "tui.select.down") {
            if self.filtered_models.is_empty() {
                return PendingAction::None;
            }
            // 环绕移动：最后一项再下移回到开头。
            self.selected_index = if self.selected_index == self.filtered_models.len() - 1 {
                0
            } else {
                self.selected_index + 1
            };
            self.update_list();
        } else if kb.matches(key_data, "tui.select.confirm") {
            let selected = self.filtered_models.get(self.selected_index).cloned();
            if let Some(item) = selected {
                return self.handle_select(item.model.clone());
            }
        } else if kb.matches(key_data, "tui.select.cancel") {
            // 取消：先关闭（取消刷新），宿主回调由外层派发。
            self.dispose();
            return PendingAction::Cancel;
        } else if kb.matches(key_data, "app.models.save") && self.on_select_as_default.is_some() {
            // 保存为默认：仅当宿主提供了回调才响应；与 confirm 一样先关闭。
            let selected = self.filtered_models.get(self.selected_index).cloned();
            if let Some(item) = selected {
                self.dispose();
                return PendingAction::SelectAsDefault(item.model);
            }
        } else {
            // 其余输入交给搜索框，随后显式过滤（TS 的 `onSubmit` 只选中当前项）。
            // `on_submit` 在 `Input::handle_input` 内部同步触发，只把共享标志置位；
            // 这里在输入框返回后检查它，相当于在借用结束后执行「提交即选中当前项」。
            self.search_input.handle_input(key_data);
            if self.submit_requested.replace(false) {
                let selected = self.filtered_models.get(self.selected_index).cloned();
                if let Some(item) = selected {
                    // 提交命中时直接返回，不再走下面的过滤（Enter 本身不改变查询文本）。
                    return self.handle_select(item.model.clone());
                }
            }
            let query = self.search_input.get_value().to_string();
            self.filter_models(&query);
        }
        PendingAction::None
    }

    /// `handleSelect(model)`：先关闭（取消后台刷新），再把选中结果交给外层派发回调。
    fn handle_select(&mut self, model: Model) -> PendingAction {
        self.dispose();
        PendingAction::Select(model)
    }

    /// `dispose()`：幂等；取消后台刷新（TS 的定时器在 Rust 侧由 `start_background_refresh`
    /// 的 `select!` sleep 表达，令牌取消后等待中的刷新会以中止错误结束）。
    ///
    /// 只置 `closed` 并取消令牌：不清理回调、列表或搜索框（TS 同样），之后到达的刷新结果
    /// 会被丢弃，但按键路径仍会继续运行并触发回调。
    fn dispose(&mut self) {
        if self.closed {
            return;
        }
        self.closed = true;
        self.refresh_abort.cancel();
    }

    /// `refreshModels()` 的 `.then` / `.catch` 部分：把一次刷新结果映射成错误行/状态行。
    ///
    /// `Ok` 分支先清空状态行，再依次判断：本地超时 → 单 provider 错误 → 多 provider 错误
    /// → 运行时 `get_error()`（为空则记成功）。无论走哪支，最后都会重载快照并按当前搜索词
    /// 重新过滤，因此刷新后列表与选中行立即反映新目录。
    /// `Err` 分支只更新错误行并重建列表，**不**重载快照（继续显示旧模型）。
    ///
    /// 调用方（`start_background_refresh`）已保证此时 `closed == false`。
    fn apply_refresh_result(
        &mut self,
        result: Result<ModelsRefreshResult, AiError>,
        timed_out: bool,
    ) {
        match result {
            Ok(result) => {
                self.refresh_status_message.clear();
                // 超时分支：只有底层刷新也报告 aborted 时才显示超时文案；否则继续看逐
                // provider 错误（例如 signal 被取消但刷新已完成）。
                if result.aborted && timed_out {
                    self.error_message =
                        Some("Model refresh timed out; showing cached models.".to_string());
                } else if result.errors.len() == 1 {
                    // 单个 provider：IndexMap 的 keys() 按插入序（即各 provider 刷新任务的
                    // 完成顺序）。
                    let provider = result
                        .errors
                        .keys()
                        .next()
                        .cloned()
                        .expect("one error entry");
                    self.error_message = Some(format!(
                        "Could not refresh {provider}; showing cached models."
                    ));
                } else if result.errors.len() > 1 {
                    // 多个 provider：数量 + 逗号连接，顺序同样是插入序。
                    let providers: Vec<String> = result.errors.keys().cloned().collect();
                    self.error_message = Some(format!(
                        "Could not refresh {} model catalogs ({}); showing cached models.",
                        result.errors.len(),
                        providers.join(", ")
                    ));
                } else {
                    self.error_message = self.runtime.get_error();
                    // get_error() 可能返回多段（配置、provider 合成、可用性刷新），段间用
                    // "\n\n" 连接，update_list 会按 '\n' 拆成多行渲染。
                    // TS: `if (!this.errorMessage)` —— 空串按 falsy 处理（显示成功状态）。
                    if self.error_message.as_deref().is_none_or(str::is_empty) {
                        self.error_message = None;
                        self.refresh_status_message = "Model catalogs refreshed.".to_string();
                        self.refresh_status_success = true;
                    }
                }
                // 快照可能删除了当前模型；load_models_from_snapshot 会重算选中行。
                self.load_models_from_snapshot();
                let query = self.search_input.get_value().to_string();
                self.filter_models(&query);
            }
            Err(error) => {
                // 与 TS 的 catch 一样不重载快照：缓存列表继续可用，错误行说明失败原因。
                self.refresh_status_message.clear();
                self.error_message = Some(if timed_out {
                    "Model refresh timed out; showing cached models.".to_string()
                } else {
                    format!("Could not refresh model catalogs: {}", error.message)
                });
                self.update_list();
            }
        }
    }

    /// 按渲染顺序列出子组件（TS `Container.children`）。
    ///
    /// `render` / `handleMouse` 都走这份列表，行高与 TS 的 `Container` 布局一致；可选组件
    /// （scope/提示行）只在构造时创建，顺序在生命周期内不变。
    ///
    /// 每次调用临时组装一个 `Vec<&mut dyn Component>`：Rust 没有继承，子组件是结构体字段
    /// 且要保留具体类型（`Input` 的读写、`Text::set_text`），因此用「按固定顺序借出」模拟
    /// TS 的 `Container.children`。`mouse_layout` 缓存依赖这里的顺序与条目数保持不变。
    fn children_mut(&mut self) -> Vec<&mut dyn Component> {
        let mut children: Vec<&mut dyn Component> =
            vec![&mut self.top_border, &mut self.spacer_top];
        if let Some(text) = self.scope_text.as_mut() {
            children.push(text);
        }
        if let Some(text) = self.scope_hint_text.as_mut() {
            children.push(text);
        }
        if let Some(text) = self.warning_text.as_mut() {
            children.push(text);
        }
        children.push(&mut self.spacer_before_search);
        children.push(&mut self.search_input);
        children.push(&mut self.spacer_after_search);
        children.push(&mut self.list_container);
        children.push(&mut self.spacer_after_list);
        if let Some(text) = self.hint_text.as_mut() {
            children.push(text);
        }
        children.push(&mut self.bottom_border);
        children
    }

    /// TS `Container.handleMouse`：把事件按行偏移转发给命中的子组件（返回未提升的
    /// `focus_target`；外层 [`ModelSelectorComponent`] 负责把焦点留给自己）。
    ///
    /// 步骤：
    /// 1. 只处理落在自身高度内的行；越界返回 `None`，交给上层/渲染器兜底。
    /// 2. 宽度与 `mouse_layout` 一致时复用上次渲染的行高；否则现场对每个子组件
    ///    `render(width)` 求行数（会顺带刷新它们的渲染缓存）。
    /// 3. 命中第一个覆盖该行的子组件，把 `y` 换成子组件局部坐标、`height` 换成该子组件
    ///    行数，再调用 [`dispatch_mouse_event`]；没有子组件覆盖时返回 `None`。
    ///
    /// 注意缓存只按宽度失效：`update_list` 在两次 render 之间改变列表内容不会主动清空它，
    /// 下一次 `render_lines` 才会重算行高。
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
        let mut children = self.children_mut();
        let child = children.get_mut(index)?;
        dispatch_mouse_event(&mut **child, &child_event)
    }

    /// 按组件顺序拼接渲染输出（`Container` 语义；子组件都是 `Text`/`Input`/`Container`）。
    ///
    /// 每帧先 `update_list()` 重建列表内容，再依次渲染子组件，并把各自行数写入
    /// `mouse_layout`（宽度 + 行高数组）供下一次 `handle_mouse` 复用。行数必须与
    /// `children_mut` 的顺序一一对应，否则鼠标会命中错误的子组件。
    fn render_lines(&mut self, width: usize) -> Vec<String> {
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
}

/// 启动构造末尾的 `void this.refreshModels()`（15 秒超时 + 共享协调器）。
///
/// 用 `Weak` 持有组件：任务在刷新前后重新升级，组件已被宿主丢弃时直接退出。第一次升级后
/// 只拷出 runtime 与取消令牌，随即结束 `RefCell` 借用并 `drop(strong)`——不在 `await`
/// 期间持有借用，也不延长组件的生命周期。
///
/// 超时路径：与 TS 的 setTimeout 一样只 `cancel()` 令牌；为了让 `apply_refresh_result`
/// 走 `Err` 分支（TS 的 `refreshModelCatalogs` 会以 AbortError 拒绝），这里等被取消的
/// future 结束后主动构造 `AiError::aborted`。因此超时分支会无条件丢弃底层刷新的返回值，
/// 即使它在同一轮轮询里也已就绪（`select!` 在多个分支同时就绪时随机选择）。
///
/// 完成后若组件已 `closed`（期间调用过 dispose），直接返回：不应用结果、不请求重绘。
fn start_background_refresh(inner: &Rc<RefCell<ModelSelectorInner>>) {
    let weak = Rc::downgrade(inner);
    tokio::task::spawn_local(async move {
        let Some(strong) = weak.upgrade() else {
            return;
        };
        // 借用只覆盖拷贝；随后释放强引用，让组件可以被宿主正常丢弃。
        let (runtime, signal) = {
            let inner = strong.borrow();
            (Arc::clone(&inner.runtime), inner.refresh_abort.clone())
        };
        drop(strong);

        // 15 秒上限；超时与取消的语义见上方说明。
        let refresh = refresh_model_catalogs(&runtime, Some(&signal));
        tokio::pin!(refresh);
        let mut timed_out = false;
        let result = tokio::select! {
            result = &mut refresh => result,
            _ = tokio::time::sleep(std::time::Duration::from_millis(15_000)) => {
                // TS 的 setTimeout 只 abort controller：`refreshModelCatalogs` 的等待以 AbortError
                // 拒绝，因此 catch 分支看到的是错误（而不是底层刷新的返回值）。
                timed_out = true;
                signal.cancel();
                let _ = refresh.await;
                Err(AiError::aborted("This operation was aborted"))
            }
        };

        // 结果应用前再升级一次：组件可能已在刷新期间被丢弃。
        let Some(strong) = weak.upgrade() else {
            return;
        };
        let request_render = {
            let mut inner = strong.borrow_mut();
            if inner.closed {
                return;
            }
            inner.apply_refresh_result(result, timed_out);
            inner.request_render.clone()
        };
        // 借用已释放；宿主回调（重绘）在借用外调用。
        if let Some(request_render) = request_render {
            request_render();
        }
    });
}

/// TS: `ModelSelectorComponent`。
///
/// 宿主用 [`Component::render`] 取每帧文本、用 [`Component::handle_input`]（或本类型的
/// [`ModelSelectorComponent::handle_input`]）喂按键；选择结果通过构造时传入的回调返回，
/// 组件本身不修改会话。宿主替换组件前应调用 [`ModelSelectorComponent::dispose`]
/// （持有 `Box<dyn Component>` 时可经 [`Component::as_any_mut`] 向下转型）。
pub struct ModelSelectorComponent {
    inner: Rc<RefCell<ModelSelectorInner>>,
}

impl ModelSelectorComponent {
    /// TS `constructor(tui, currentModel, modelRuntime, scopedModels, onSelect, onCancel,
    /// initialSearchInput?, onSelectAsDefault?, defaultModel?)`。
    ///
    /// - `request_render`：TS `tui.requestRender()` 的宿主注入；构造时同步调用一次，之后
    ///   每次后台刷新完成再调用。`None` 表示不需要重绘（测试常用）。
    /// - `current_model`：只影响排序、`✓` 标记与初始选中行，不写回会话。
    /// - `scoped_models`：为空时初始 scope 为 All 并显示 `/login` 提示；非空时保持给定顺序。
    /// - `on_select` / `on_cancel` / `on_select_as_default`：宿主回调，分别在确认、取消、
    ///   保存默认模型时调用；`on_select_as_default` 为 `None` 时 `app.models.save` 不生效。
    ///
    /// 构造会立即读取运行时快照（不触发刷新），随后启动一次后台刷新。要求主题已初始化
    /// （构造期就会着色），并且运行在 `LocalSet` 上下文中（后台任务用 `spawn_local`）。
    #[allow(clippy::too_many_arguments)]
    pub fn new<R>(
        request_render: Option<RequestRenderCallback>,
        current_model: Option<Model>,
        model_runtime: R,
        scoped_models: Vec<ScopedModelItem>,
        on_select: Box<dyn FnMut(Model)>,
        on_cancel: Box<dyn FnMut()>,
        initial_search_input: Option<String>,
        on_select_as_default: Option<Box<dyn FnMut(Model)>>,
        default_model: Option<DefaultModelReference>,
    ) -> Self
    where
        R: ModelSelectorRuntime + 'static,
    {
        Self::with_runtime(
            request_render,
            current_model,
            Arc::new(model_runtime),
            scoped_models,
            on_select,
            on_cancel,
            initial_search_input,
            on_select_as_default,
            default_model,
        )
    }

    /// 与 [`ModelSelectorComponent::new`] 相同，但接受已装箱的运行时（测试 / 动态注入）。
    ///
    /// 顺序与 TS 构造末尾一致：先调用一次 `request_render()`，再启动后台刷新（总是刷新，
    /// 与是否传入 `initial_search_input` 无关）。
    #[allow(clippy::too_many_arguments)]
    pub fn with_runtime(
        request_render: Option<RequestRenderCallback>,
        current_model: Option<Model>,
        runtime: Arc<dyn ModelSelectorRuntime>,
        scoped_models: Vec<ScopedModelItem>,
        on_select: Box<dyn FnMut(Model)>,
        on_cancel: Box<dyn FnMut()>,
        initial_search_input: Option<String>,
        on_select_as_default: Option<Box<dyn FnMut(Model)>>,
        default_model: Option<DefaultModelReference>,
    ) -> Self {
        let inner = ModelSelectorInner::new(
            runtime,
            request_render,
            current_model,
            scoped_models,
            on_select,
            on_cancel,
            initial_search_input,
            on_select_as_default,
            default_model,
        );
        let inner = Rc::new(RefCell::new(inner));

        // TS: `this.tui.requestRender(); void this.refreshModels();`
        if let Some(request_render) = inner.borrow().request_render.clone() {
            request_render();
        }
        start_background_refresh(&inner);
        ModelSelectorComponent { inner }
    }

    /// `dispose()`：幂等。置 `closed` 并取消后台刷新令牌；不清理回调、列表或搜索框，
    /// 之后到达的刷新结果会被丢弃（`closed` 让刷新结果失效，但不拦截输入）。
    pub fn dispose(&mut self) {
        self.inner.borrow_mut().dispose();
    }

    /// `getSearchInput()`：借出内部搜索框。
    ///
    /// 返回的 `Ref` 在整个生命周期内保持 `RefCell` 的不可变借用；持有它期间调用
    /// [`ModelSelectorComponent::handle_input`]、[`ModelSelectorComponent::dispose`] 或
    /// `render` 会 panic（`BorrowMutError`）。只应在读取光标/值时短暂使用。
    pub fn get_search_input(&self) -> Ref<'_, Input> {
        Ref::map(self.inner.borrow(), |inner| &inner.search_input)
    }

    /// 处理一次键盘输入。
    ///
    /// 先在借用内把按键解析成延迟动作，借用释放后再调用宿主回调；因此回调运行时不持有
    /// `inner` 的借用。`dispose` 之后仍可调用：组件不检查 `closed`，按键与回调照常执行
    /// （TS 的 `dispose` 也只置 `closed`）。
    pub fn handle_input(&mut self, data: &str) {
        let action = {
            let mut inner = self.inner.borrow_mut();
            inner.handle_input(data)
        };
        self.dispatch_action(action);
    }

    /// 在借用释放后调用宿主回调（避免回调里重入 `RefCell`）。
    ///
    /// TS 的 `dispose()` 只置 `closed`，不清理 `onSelectCallback`/`onCancelCallback`
    /// （`dispose()` 幂等，回调在每次匹配按键时都会再次调用）。因此这里暂时取出 boxed
    /// 回调、调用后再放回：既不在调用期间持有 `inner` 的借用，又保留回调。
    ///
    /// 失败路径：若回调 panic，展开会跳过「放回」这一步，该回调从此缺失（`inner` 的其它
    /// 状态不受影响，因为调用期间没有持有借用）。
    fn dispatch_action(&self, action: PendingAction) {
        match action {
            PendingAction::None => {}
            PendingAction::Select(model) => {
                let mut callback = self.inner.borrow_mut().on_select.take();
                if let Some(callback) = callback.as_mut() {
                    callback(model);
                }
                self.inner.borrow_mut().on_select = callback;
            }
            PendingAction::SelectAsDefault(model) => {
                let mut callback = self.inner.borrow_mut().on_select_as_default.take();
                if let Some(callback) = callback.as_mut() {
                    callback(model);
                }
                self.inner.borrow_mut().on_select_as_default = callback;
            }
            PendingAction::Cancel => {
                let mut callback = self.inner.borrow_mut().on_cancel.take();
                if let Some(callback) = callback.as_mut() {
                    callback();
                }
                self.inner.borrow_mut().on_cancel = callback;
            }
        }
    }
}

impl Component for ModelSelectorComponent {
    /// 每帧入口：重建列表并把各子组件行数记录到 `mouse_layout`。
    fn render(&mut self, width: usize) -> Vec<String> {
        self.inner.borrow_mut().render_lines(width)
    }

    /// trait 入口：宿主以 `Box<dyn Component>` 持有时走这里，委托给固有方法。
    fn handle_input(&mut self, data: &str) {
        ModelSelectorComponent::handle_input(self, data);
    }

    fn handle_mouse(&mut self, event: &TuiMouseEvent) -> Option<TuiMouseEventResult> {
        let result = self.inner.borrow_mut().handle_mouse(event);
        // TS `Container.handleMouse`：转发型容器定义了 `handleInput` 时，子控件请求焦点会把
        // 焦点留给自己（`ModelSelectorComponent` 定义了 `handleInput`）。
        // 这里的地址算法与 `dispatch_mouse_event` 记录接收者时相同，宿主据此解析焦点。
        result.map(|mut result| {
            if result.focus == Some(true) {
                let address = self as *const ModelSelectorComponent as *const () as usize;
                result.focus_target = Some(MouseFocusTarget::Address(address));
            }
            result
        })
    }

    /// 焦点状态实际存在内部搜索框上（见 [`Focusable`] 实现）；宿主可经这里拿到 `self`。
    fn as_focusable(&mut self) -> Option<&mut dyn Focusable> {
        Some(self)
    }

    /// TS 的 `Component` 接口没有 `dispose`；宿主（`InteractiveMode.showSelector` 的
    /// `dispose` 回调）需要在替换选择器时中止后台刷新，因此提供向下转型入口。
    fn as_any(&self) -> Option<&dyn Any> {
        Some(self)
    }

    /// [`Component::as_any`] 的可变版本；`dispose` 需要 `&mut self` 时用它。
    fn as_any_mut(&mut self) -> Option<&mut dyn Any> {
        Some(self)
    }
}

impl Focusable for ModelSelectorComponent {
    /// 焦点状态存储在内部搜索框上；`Input` 聚焦时会输出硬件光标标记。
    fn focused(&self) -> bool {
        self.inner.borrow().search_input.focused()
    }

    /// 把焦点转交给搜索框（不改变选择器其它状态）。
    fn set_focused(&mut self, focused: bool) {
        self.inner.borrow_mut().search_input.set_focused(focused);
    }
}
