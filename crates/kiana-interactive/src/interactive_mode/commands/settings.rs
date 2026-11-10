//! `interactive-mode.ts` 的 `/settings`（§2.50.9）。
//!
//! `showSettingsSelector`：构造 `SettingsConfig` / `SettingsCallbacks` 并挂载
//! [`SettingsSelectorComponent`]。
//!
//! # 为什么需要这个文件
//!
//! `/settings` 面板的组件（`components/settings_selector.rs`）不持有任何存储：它只展示一份
//! 只读的 [`SettingsConfig`] 快照，用户每次改动都通过 [`SettingsCallbacks`] 里的回调交回
//! 本文件。于是“设置项 → 真正的副作用”的映射全部集中在这里：写 settings 管理器的全局
//! 配置、改会话运行时状态、刷新已渲染的组件、切换 TUI 模式等。
//!
//! # 一次打开的流程
//!
//! 1. 读当前状态组装 [`SettingsConfig`]：会话（自动压缩、当前模型、steering / follow-up
//!    队列模式）、settings 管理器（各类偏好）、主题控制器（当前主题设置与终端明暗）；
//! 2. 取 `Weak`（`InteractiveMode::downgrade`）后调用 `show_selector`；
//! 3. 闭包里为每个设置项创建捕获 `Weak` 的回调并装入 [`SettingsCallbacks`]，再构造
//!    [`SettingsSelectorComponent`]；
//! 4. 焦点指向选择器内部的设置列表（`SelectorFocus::Address`），与 TS 的
//!    `focus: selector.getSettingsList()` 一致：列表才是滚动/键位处理的主体，
//!    面板自身只负责上下边框与鼠标坐标偏移。
//!
//! # 回调约定
//!
//! * 回调由选择器在它自己的 `handle_input` / `handle_mouse` 调用栈里同步触发（见
//!   `components/settings_selector.rs` 的模块文档）。因此回调不能假设“当前没有借用”；
//!   捕获的都是 `Weak`，模式已释放时升级失败即静默返回，不会写入已销毁的状态。
//! * 大多数回调经 `mode!` 宏同步执行：写 settings / 会话状态、遍历已挂载组件、显示状态行。
//! * 会替换渲染器或直接操作 `InteractiveTuiReference` 的回调走 `run_mode_callback`：主题
//!   应用与预览、硬件光标、clear-on-shrink、TUI 模式切换、全屏复制与滚轮行数。它检测到
//!   渲染器已被借用时把动作延后一个本地任务，避免在输入分发/渲染期间重入。
//! * 少数只写设置的简单回调（如 collapse changelog、telemetry、quiet startup）直接
//!   `weak.upgrade()` 后调用 setter，不经过 `mode!` 宏，也不刷新当前 UI：这些值由各自的
//!   读取方在下次使用时重新读取（例如 changelog 渲染、启动横幅、`kiana-config::defaults`
//!   的 telemetry 兜底）。
//! * 失败不试图“修复”状态：例如切换 TUI 模式被拒（存在活动 overlay）时，回调把选择器
//!   显示的值改回当前真实模式并提示用户，设置本身不写入。
//!
//! # 注意事项
//!
//! * [`SettingsConfig`] 只在打开面板时组装一次；面板打开期间的其它改动不会反映到列表上，
//!   `warnings` 与 `model_thinking_levels` 由组件内部另存可变副本供子菜单显示。
//! * 多数设置写入走“写全局配置 + 保存”；`set_show_images` / `set_image_width_cells` 等
//!   返回 `Result`，本文件的回调有意用 `let _ =` 忽略错误，只继续传播值与 UI 更新。
//! * `output_pad` 从 settings 的 `i64` 收窄成 `u8`，转换失败按 0；
//!   `http_idle_timeout_ms` 读取失败时退回 `DEFAULT_HTTP_IDLE_TIMEOUT_MS`。

use std::cell::RefCell;
use std::rc::{Rc, Weak};

use kiana_config::defaults::DEFAULT_THINKING_LEVEL;
use kiana_config::settings::schema::{DoubleEscapeAction, TuiMode as SettingsTuiMode};
use kiana_config::settings::{
    CacheWarmingMode, FullscreenScrollbar, SteeringMode, WarningSettings,
};
use kiana_theme::SYSTEM_THEME_NAME;
use kiana_tui::tui::Component;

use crate::components::assistant_message::AssistantMessageComponent;
use crate::components::bash_execution::BashExecutionComponent;
use crate::components::branch_summary_message::BranchSummaryMessageComponent;
use crate::components::compaction_summary_message::CompactionSummaryMessageComponent;
use crate::components::custom_entry::CustomEntryComponent;
use crate::components::custom_message::CustomMessageComponent;
use crate::components::settings_selector::{
    ModelThinkingLevelChangeCallback, ModelThinkingRemoveCallback, SettingsCallbacks,
    SettingsConfig, SettingsSelectorComponent, TextCallback,
};
use crate::components::skill_invocation_message::SkillInvocationMessageComponent;
use crate::components::tool_execution::ToolExecutionComponent;
use crate::components::user_message::UserMessageComponent;
use crate::interactive_mode::extension_ui::widgets::OwnedComponent;
use crate::tui_renderer::{SharedComponentProxy, component_address};

use super::super::{InteractiveMode, InteractiveModeInner};
use super::{SelectorCreation, SelectorFocus};

/// `steeringMode` 的 `QueueMode` → settings 的 `SteeringMode`。
///
/// 两套枚举来自不同 crate：会话侧用 `kiana_agent_core::types::QueueMode`，设置文件里存的
/// 是 `kiana_config` 的 `SteeringMode`；面板展示与写盘都用后者，因此每个方向都要显式转一次
/// （本文件里的 `transport_*`、`settings_tui_mode` 等是同一模式）。两侧所有变体一一对应，
/// 新增变体时两个 `match` 会一起编译失败，避免漏映射。
fn steering_mode(mode: kiana_agent_core::types::QueueMode) -> SteeringMode {
    match mode {
        kiana_agent_core::types::QueueMode::All => SteeringMode::All,
        kiana_agent_core::types::QueueMode::OneAtATime => SteeringMode::OneAtATime,
    }
}

/// `transport` 的 settings 值 → `kiana_ai::Transport`。
///
/// 一个方向用于把配置文件里的值交给面板显示；反方向见下一函数。设置序列化用的枚举在
/// `kiana_config`，运行时 agent 只认 `kiana_ai` 的类型，所以回调里要各转一次。
fn transport_from_setting(
    setting: kiana_config::settings::schema::TransportSetting,
) -> kiana_ai::Transport {
    match setting {
        kiana_config::settings::schema::TransportSetting::Auto => kiana_ai::Transport::Auto,
        kiana_config::settings::schema::TransportSetting::Sse => kiana_ai::Transport::Sse,
        kiana_config::settings::schema::TransportSetting::Websocket => {
            kiana_ai::Transport::Websocket
        }
        kiana_config::settings::schema::TransportSetting::WebsocketCached => {
            kiana_ai::Transport::WebsocketCached
        }
    }
}

/// `kiana_ai::Transport` → settings 的 `TransportSetting`。
fn transport_to_setting(
    transport: kiana_ai::Transport,
) -> kiana_config::settings::schema::TransportSetting {
    match transport {
        kiana_ai::Transport::Auto => kiana_config::settings::schema::TransportSetting::Auto,
        kiana_ai::Transport::Sse => kiana_config::settings::schema::TransportSetting::Sse,
        kiana_ai::Transport::Websocket => {
            kiana_config::settings::schema::TransportSetting::Websocket
        }
        kiana_ai::Transport::WebsocketCached => {
            kiana_config::settings::schema::TransportSetting::WebsocketCached
        }
    }
}

/// `${cacheWarmingMode}`。
fn cache_warming_mode_str(mode: CacheWarmingMode) -> &'static str {
    match mode {
        CacheWarmingMode::Off => "off",
        CacheWarmingMode::Streaming => "streaming",
        CacheWarmingMode::Idle => "idle",
    }
}

/// `fullscreenScrollbar` 的 `kiana_tui` 值 → settings 值。
fn fullscreen_scrollbar_from_tui(scrollbar: kiana_tui::ScrollViewScrollbar) -> FullscreenScrollbar {
    match scrollbar {
        kiana_tui::ScrollViewScrollbar::Auto => FullscreenScrollbar::Auto,
        kiana_tui::ScrollViewScrollbar::Always => FullscreenScrollbar::Always,
        kiana_tui::ScrollViewScrollbar::Hidden => FullscreenScrollbar::Hidden,
    }
}

/// 把模式里的 `TuiMode` 转成 settings 的 `TuiMode`。
fn settings_tui_mode(mode: kiana_tui::TuiMode) -> SettingsTuiMode {
    match mode {
        kiana_tui::TuiMode::Regular => SettingsTuiMode::Regular,
        kiana_tui::TuiMode::Fullscreen => SettingsTuiMode::Fullscreen,
    }
}

/// `${tuiMode}`。
fn tui_mode_str(mode: SettingsTuiMode) -> &'static str {
    match mode {
        SettingsTuiMode::Regular => "regular",
        SettingsTuiMode::Fullscreen => "fullscreen",
    }
}

/// 在渲染器借用期间（输入分发/渲染）同步重入 TUI 会 panic；TS 的 `this` 没有
/// `RefCell` 限制。检测到渲染器已被借用时把回调延后一个本地任务，否则同步执行
/// （直接调用 `showSettingsSelector` 的测试保持同步语义）。
///
/// “渲染器借用”同时看两层：`InteractiveModeInner` 本身被借用（`try_borrow` 失败）或
/// 它的 `renderer` 已被 `borrow_mut`。后者正是 `handle_input` 分发期间的常态。
/// 这是保守判定：内层只要被借用就延后，不做进一步区分。
///
/// 延后路径要求当前线程处于 tokio `LocalSet`（见 `commands/mod.rs` 模块文档）；`Weak`
/// 升级失败说明模式已释放，回调被静默丢弃——不会在关闭后再操作 UI。
///
/// 注意：延后的是整个回调体的执行时机，写设置与刷新 UI 都在延后任务里发生；每个回调只
/// 捕获自己的值，不在任务里重新读取“最新值”。
fn run_mode_callback<F>(weak: &Weak<RefCell<InteractiveModeInner>>, callback: F)
where
    F: FnOnce(&mut InteractiveMode) + 'static,
{
    let Some(inner) = weak.upgrade() else {
        return;
    };
    let renderer_borrowed = match inner.try_borrow() {
        Ok(inner) => inner.renderer.try_borrow_mut().is_err(),
        Err(_) => true,
    };
    if renderer_borrowed {
        let weak = Weak::clone(weak);
        tokio::task::spawn_local(async move {
            if let Some(inner) = weak.upgrade() {
                let mut mode = InteractiveMode { inner };
                callback(&mut mode);
            }
        });
    } else {
        let mut mode = InteractiveMode { inner };
        callback(&mut mode);
    }
}

/// 对容器里的每个组件调用 `update`：解开共享代理（`SharedComponentProxy` /
/// `OwnedComponent`）后把具体组件当作 `&mut dyn Any` 交出（TS 的
/// `child instanceof ToolExecutionComponent` / `"setOutputPad" in child`）。
///
/// 为什么要解代理：聊天容器里挂的是 [`SharedComponentProxy`]（指向宿主仍持有的共享
/// 句柄）或扩展 widget 的 `OwnedComponent`，直接对它们 `downcast_mut::<具体类型>()` 永远
/// 失败；必须取到内层组件再下转。内层 `try_borrow_mut` 失败（该组件正在被渲染/分发）时
/// 静默跳过这一项，不会 panic，也不会在稍后补做。
///
/// 边界：`Component::as_any_mut` 默认返回 `None`；不实现它的组件（例如普通文本）会被
/// 直接跳过。函数在 `update` 返回后继续遍历，无提前终止。
///
/// 遍历期间持有容器的可变借用（`container.borrow_mut()`），因此 `update` 里不得再借用
/// 同一个容器。
fn for_each_message_child(
    container: &Rc<RefCell<kiana_tui::tui::Container>>,
    mut update: impl FnMut(&mut dyn std::any::Any),
) {
    let mut container = container.borrow_mut();
    for child in &mut container.children {
        let Some(any) = child.as_any_mut() else {
            continue;
        };
        if let Some(proxy) = any.downcast_mut::<SharedComponentProxy>() {
            if let Ok(mut inner) = proxy.component().try_borrow_mut()
                && let Some(any) = inner.as_any_mut()
            {
                update(any);
            }
            continue;
        }
        if let Some(owned) = any.downcast_mut::<OwnedComponent>() {
            if let Some(any) = owned.0.as_any_mut() {
                update(any);
            }
            continue;
        }
        update(any);
    }
}

/// `child instanceof ToolExecutionComponent` 的两个显示设置。
fn set_show_images(any: &mut dyn std::any::Any, enabled: bool) {
    if let Some(tool) = any.downcast_mut::<ToolExecutionComponent>() {
        tool.set_show_images(enabled);
    }
}

/// 见 `set_show_images`：只影响已挂载的工具组件，其它类型静默忽略。
fn set_image_width_cells(any: &mut dyn std::any::Any, width: f64) {
    if let Some(tool) = any.downcast_mut::<ToolExecutionComponent>() {
        tool.set_image_width_cells(width);
    }
}

/// TS 的 `"setOutputPad" in child && typeof child.setOutputPad === "function"`：
/// 所有带 `setOutputPad` 的消息组件都跟随设置（见 interactive-mode.ts 5095-5112）。
///
/// 宏按顺序尝试下转，命中第一个即 `return`；每个类型都互不相同，因此等价于逐一判断。
/// 新增带缩进的消息组件时要把类型加进这份列表，否则该组件不会跟随设置（消息组件在
/// 构造时读取一次 `inner.output_pad`，之后不再回读）。
fn set_output_pad(any: &mut dyn std::any::Any, padding: usize) {
    macro_rules! try_set {
        ($($ty:ty),* $(,)?) => {
            $(
                if let Some(component) = any.downcast_mut::<$ty>() {
                    component.set_output_pad(padding);
                    return;
                }
            )*
        };
    }
    try_set!(
        ToolExecutionComponent,
        AssistantMessageComponent,
        UserMessageComponent,
        BashExecutionComponent,
        CompactionSummaryMessageComponent,
        BranchSummaryMessageComponent,
        CustomEntryComponent,
        CustomMessageComponent,
        SkillInvocationMessageComponent,
    );
}

impl InteractiveMode {
    /// `showSettingsSelector()`。
    ///
    /// 组装一份配置快照，挂载 `/settings` 面板，并把每个设置项的回调绑定到
    /// [`InteractiveMode`] 的相应方法上。
    ///
    /// # 输入与默认值
    ///
    /// - `default_model`：`provider/modelId` 仅在两者都存在且非空时拼接，否则用字面量
    ///   `"not set"`（组件按字符串与 `available_default_models` 比较，见
    ///   `components/settings_selector.rs` 的注意事项）。
    /// - `http_idle_timeout_ms`：settings 读取失败（值非法）时退回
    ///   [`kiana_core::http_dispatcher::DEFAULT_HTTP_IDLE_TIMEOUT_MS`]，面板仍可打开。
    /// - `current_theme`：优先主题控制器的 `get_theme_selection()`（可能是 `light/dark`
    ///   形式的自动设置，不一定是单个主题名），没有控制器时用 `SYSTEM_THEME_NAME`；
    ///   `terminal_theme` 有控制器时经 `get_terminal_theme()` 读取（它读的是全局推导值），
    ///   没有控制器时直接调用 `kiana_theme::get_terminal_theme()`。
    /// - `thinking_level`：settings 没有显式值时用 `DEFAULT_THINKING_LEVEL`；
    ///   `available_thinking_levels` 固定取 `THINKING_LEVEL_OPTIONS`（全部 7 项）以对齐
    ///   TS 的 `config.availableThinkingLevels`；组件目前不读取该字段（见
    ///   `components/settings_selector.rs` 字段说明），真正决定子菜单可选项的是每个模型
    ///   自身的 `get_supported_thinking_levels`。
    ///   `output_pad` 从 `i64` 收窄为 `u8`，失败按 0。
    /// - `tui_mode` 取自渲染器当前模式（`ui.mode()`），而不是设置文件：用户可能刚切换过。
    ///
    /// # 回调集合
    ///
    /// 每个回调都只捕获 `Weak`（个别包装成 `Rc<dyn Fn>`），并在模式仍存活时：
    ///
    /// - 写 settings 管理器（多数即写全局配置并保存）；
    /// - 按需同步运行时状态：会话的自动压缩/steering/follow-up/传输/缓存预热/思考等级、
    ///   footer 与编辑器边框色、聊天容器内已挂载组件的显示参数、`InteractiveTuiReference`
    ///   的硬件光标/clear-on-shrink/复制选中/滚轮行数；
    /// - 部分回调附带状态行提示（HTTP idle timeout、Cache warming、TUI mode）。
    ///
    /// 主题相关回调走 `apply_theme_setting` / `preview_theme`；TUI 模式
    /// 切换失败时不写设置，而是把选择器上的值改回真实模式并提示
    /// `Close active overlays before changing TUI mode`。
    ///
    /// # 副作用与生命周期
    ///
    /// - `on_cancel` 只关闭选择器（`done.finish()`）并请求重绘，不写任何设置。
    /// - 闭包里保存了一份 `Rc<RefCell<Option<Rc<RefCell<SettingsSelectorComponent>>>>>`
    ///   槽位，供 `on_tui_mode_change` 在失败分支里回改列表显示值。`SettingsSelectorComponent`
    ///   在构造时把整组回调放进 `Rc` 长期持有，而 `on_tui_mode_change` 又持有该槽位，因此
    ///   两者形成 `Rc` 引用环（只记录，不在此处修改）。
    /// - 选择器创建后焦点由 `SelectorFocus::Address` 指向内部设置列表；地址在挂载前用
    ///   `component_address` 取得。
    pub(crate) fn show_settings_selector(&mut self) {
        let session = self.session();
        let settings_manager = self.settings_manager();
        let default_provider = settings_manager.get_default_provider();
        let default_model_id = settings_manager.get_default_model();
        let default_model = match (default_provider, default_model_id) {
            (Some(provider), Some(model_id)) if !provider.is_empty() && !model_id.is_empty() => {
                format!("{provider}/{model_id}")
            }
            _ => "not set".to_string(),
        };
        let http_idle_timeout_ms = settings_manager
            .get_http_idle_timeout_ms()
            .unwrap_or(kiana_core::http_dispatcher::DEFAULT_HTTP_IDLE_TIMEOUT_MS as f64);
        let current_theme = self
            .inner
            .borrow()
            .theme_controller
            .as_ref()
            .and_then(|controller| controller.get_theme_selection())
            .unwrap_or_else(|| SYSTEM_THEME_NAME.to_string());
        let terminal_theme = self
            .inner
            .borrow()
            .theme_controller
            .as_ref()
            .map(|controller| controller.get_terminal_theme())
            .unwrap_or_else(kiana_theme::get_terminal_theme);
        let ui_mode = self.inner.borrow().ui.mode();
        let hide_thinking_block = self.inner.borrow().hide_thinking_block;

        // 一次性快照：面板打开后不再回读这些值，回调只是把新值写回各自的存储。
        // `session.model()` 与 `model_runtime().get_available_snapshot()` 都取自会话；
        // 后者是模型选择子菜单/默认模型校验所用的可用模型列表。
        let config = SettingsConfig {
            auto_compact: session.auto_compaction_enabled(),
            default_model,
            current_model: session.model(),
            available_default_models: session.model_runtime().get_available_snapshot(),
            show_images: settings_manager.get_show_images(),
            image_width_cells: settings_manager.get_image_width_cells(),
            auto_resize_images: settings_manager.get_image_auto_resize(),
            block_images: settings_manager.get_block_images(),
            enable_skill_commands: settings_manager.get_enable_skill_commands(),
            steering_mode: steering_mode(session.steering_mode()),
            follow_up_mode: steering_mode(session.follow_up_mode()),
            transport: transport_from_setting(settings_manager.get_transport()),
            session_share_destination: settings_manager.get_session_share_destination(),
            http_idle_timeout_ms,
            cache_warming_mode: settings_manager.get_cache_warming_mode(),
            thinking_level: settings_manager
                .get_default_thinking_level()
                .unwrap_or(DEFAULT_THINKING_LEVEL),
            available_thinking_levels: kiana_config::defaults::THINKING_LEVEL_OPTIONS.to_vec(),
            model_thinking_levels: settings_manager.get_all_model_thinking_levels(),
            current_theme,
            ui_locale: settings_manager.get_ui_locale(),
            terminal_theme,
            available_themes: kiana_theme::get_available_themes(),
            hide_thinking_block,
            mermaid_rendering_mode: settings_manager.get_mermaid_rendering_mode(),
            show_cache_miss_notices: settings_manager.get_show_cache_miss_notices(),
            collapse_changelog: settings_manager.get_collapse_changelog(),
            enable_install_telemetry: settings_manager.get_enable_install_telemetry(),
            double_escape_action: settings_manager.get_double_escape_action(),
            tree_filter_mode: settings_manager.get_tree_filter_mode(),
            show_hardware_cursor: settings_manager.get_show_hardware_cursor(),
            editor_padding_x: settings_manager.get_editor_padding_x(),
            output_pad: u8::try_from(settings_manager.get_output_pad()).unwrap_or(0),
            autocomplete_max_visible: settings_manager.get_autocomplete_max_visible(),
            quiet_startup: settings_manager.get_quiet_startup(),
            default_project_trust: settings_manager.get_default_project_trust(),
            clear_on_shrink: settings_manager.get_clear_on_shrink(),
            show_terminal_progress: settings_manager.get_show_terminal_progress(),
            tui_mode: settings_tui_mode(ui_mode),
            fullscreen_exit_output: settings_manager.get_fullscreen_exit_output(),
            fullscreen_scrollbar: fullscreen_scrollbar_from_tui(
                settings_manager.get_fullscreen_scrollbar(),
            ),
            fullscreen_copy_on_select: settings_manager.get_fullscreen_copy_on_select(),
            fullscreen_wheel_scroll_lines: settings_manager.get_fullscreen_wheel_scroll_lines(),
            warnings: settings_manager.get_warnings(),
        };

        let weak = self.downgrade();
        self.show_selector(move |_mode, done| {
            let selector_slot: Rc<RefCell<Option<Rc<RefCell<SettingsSelectorComponent>>>>> =
                Rc::new(RefCell::new(None));

            // 每个回调都通过弱引用取回当前模式；不在回调里建新任务（TS 全部同步）。
            //
            // `mode!`：升级 `Weak` 后构造一个临时的薄包装 `InteractiveMode { inner }` 交给
            // 闭包。模式已释放时整段体被跳过，因此回调不需要自己处理 `None`。这里用的是
            // 同步版本：`mode!` 回调对渲染器的唯一接触是 `request_render`（内部
            // `try_borrow_mut`，分发期间借用失败时静默跳过，渲染由分发结束后的
            // `request_immediate_render` 兜底）；需要无条件 `borrow_mut` 渲染器的操作
            // （`ui.set_clear_on_shrink`、`set_show_hardware_cursor`、`set_copy_on_select`、
            // `set_wheel_scroll_lines`、`switch_tui_mode` 等）才改用 `run_mode_callback`。
            macro_rules! mode {
                ($weak:expr, $body:expr) => {{
                    if let Some(inner) = $weak.upgrade() {
                        let mut mode = InteractiveMode { inner };
                        let f: &dyn Fn(&mut InteractiveMode) = &$body;
                        f(&mut mode);
                    }
                }};
            }

            let weak_auto_compact = weak.clone();
            let on_auto_compact_change: Rc<dyn Fn(bool)> = Rc::new(move |enabled| {
                mode!(weak_auto_compact, |mode: &mut InteractiveMode| {
                    mode.session().set_auto_compaction_enabled(enabled);
                    mode.inner
                        .borrow()
                        .footer
                        .borrow_mut()
                        .set_auto_compact_enabled(enabled);
                });
            });
            let weak_show_images = weak.clone();
            let on_show_images_change: Rc<dyn Fn(bool)> = Rc::new(move |enabled| {
                mode!(weak_show_images, |mode: &mut InteractiveMode| {
                    // 写设置返回 `Result`，这里有意忽略：失败时（`terminal` 原始值非法）
                    // 内存与磁盘都不变（见 `set_show_images` 的 `# Errors`），但已渲染的
                    // 组件仍按用户本次选择更新。
                    let _ = mode.settings_manager().set_show_images(enabled);
                    // 只更新聊天容器里已挂载的工具组件，不重建历史消息。
                    let chat = Rc::clone(&mode.inner.borrow().chat_container);
                    for_each_message_child(&chat, |any| set_show_images(any, enabled));
                });
            });
            let weak_image_width = weak.clone();
            let on_image_width_cells_change: Rc<dyn Fn(f64)> = Rc::new(move |width| {
                mode!(weak_image_width, |mode: &mut InteractiveMode| {
                    let _ = mode.settings_manager().set_image_width_cells(width);
                    let chat = Rc::clone(&mode.inner.borrow().chat_container);
                    for_each_message_child(&chat, |any| set_image_width_cells(any, width));
                });
            });
            let weak_auto_resize = weak.clone();
            let on_auto_resize_images_change: Rc<dyn Fn(bool)> = Rc::new(move |enabled| {
                if let Some(inner) = weak_auto_resize.upgrade() {
                    let _ = InteractiveMode { inner }
                        .settings_manager()
                        .set_image_auto_resize(enabled);
                }
            });
            let weak_block_images = weak.clone();
            let on_block_images_change: Rc<dyn Fn(bool)> = Rc::new(move |blocked| {
                if let Some(inner) = weak_block_images.upgrade() {
                    let _ = InteractiveMode { inner }
                        .settings_manager()
                        .set_block_images(blocked);
                }
            });
            let weak_skill = weak.clone();
            let on_enable_skill_commands_change: Rc<dyn Fn(bool)> = Rc::new(move |enabled| {
                mode!(weak_skill, |mode: &mut InteractiveMode| {
                    mode.settings_manager().set_enable_skill_commands(enabled);
                    // 自动补全候选里包含 `/skill:<name>`（`create_base_autocomplete_provider`
                    // 只在开关为真时生成并重建 `skill_commands` 映射），必须重建 provider，
                    // 否则本会话内新增/移除的技能命令不会生效。
                    mode.setup_autocomplete_provider();
                });
            });
            let weak_steering = weak.clone();
            let on_steering_mode_change: Rc<dyn Fn(SteeringMode)> = Rc::new(move |mode_value| {
                if let Some(inner) = weak_steering.upgrade() {
                    let queue_mode = InteractiveMode { inner };
                    queue_mode.session().set_steering_mode(match mode_value {
                        SteeringMode::All => kiana_agent_core::types::QueueMode::All,
                        SteeringMode::OneAtATime => kiana_agent_core::types::QueueMode::OneAtATime,
                    });
                }
            });
            let weak_follow_up = weak.clone();
            let on_follow_up_mode_change: Rc<dyn Fn(SteeringMode)> = Rc::new(move |mode_value| {
                if let Some(inner) = weak_follow_up.upgrade() {
                    InteractiveMode { inner }
                        .session()
                        .set_follow_up_mode(match mode_value {
                            SteeringMode::All => kiana_agent_core::types::QueueMode::All,
                            SteeringMode::OneAtATime => {
                                kiana_agent_core::types::QueueMode::OneAtATime
                            }
                        });
                }
            });
            let weak_transport = weak.clone();
            let on_transport_change: Rc<dyn Fn(kiana_ai::Transport)> = Rc::new(move |transport| {
                mode!(weak_transport, |mode: &mut InteractiveMode| {
                    mode.settings_manager()
                        .set_transport(transport_to_setting(transport));
                    mode.session().agent().set_transport(transport);
                });
            });
            let weak_share_dest = weak.clone();
            let on_session_share_destination_change: Rc<
                dyn Fn(kiana_config::settings::SessionShareDestination),
            > = Rc::new(move |destination| {
                if let Some(inner) = weak_share_dest.upgrade() {
                    InteractiveMode { inner }
                        .settings_manager()
                        .set_session_share_destination(destination);
                }
            });
            let weak_http = weak.clone();
            let on_http_idle_timeout_ms_change: Rc<dyn Fn(f64)> = Rc::new(move |timeout_ms| {
                mode!(weak_http, |mode: &mut InteractiveMode| {
                    // 三处必须同步：设置文件、进程级 HTTP 调度器、状态行。调度器的错误被
                    // 忽略（超时值非法或代理 URL 非法时返回 `Err`，旧 client 保持不变，
                    // 见 `configure_http_dispatcher`）；状态行仍显示用户刚选的值。
                    let _ = mode.settings_manager().set_http_idle_timeout_ms(timeout_ms);
                    let _ =
                        kiana_core::http_dispatcher::configure_http_dispatcher(timeout_ms as i64);
                    mode.show_status(&format!(
                        "HTTP idle timeout: {}",
                        kiana_core::http_dispatcher::format_http_idle_timeout_ms(timeout_ms)
                    ));
                });
            });
            let weak_cache_warming = weak.clone();
            let on_cache_warming_mode_change: Rc<dyn Fn(CacheWarmingMode)> =
                Rc::new(move |mode_value| {
                    mode!(weak_cache_warming, |mode: &mut InteractiveMode| {
                        mode.session().set_cache_warming_mode(mode_value);
                        mode.show_status(&format!(
                            "Cache warming: {}",
                            cache_warming_mode_str(mode_value)
                        ));
                    });
                });
            let weak_model_thinking = weak.clone();
            let on_model_thinking_level_change: ModelThinkingLevelChangeCallback =
                Rc::new(move |provider, model_id, level| {
                    mode!(weak_model_thinking, |mode: &mut InteractiveMode| {
                        let _ = mode
                            .settings_manager()
                            .set_model_thinking_level(provider, model_id, level);
                        // 只有被改的模型正是当前模型时才动会话状态；否则只写入持久化覆盖，
                        // 由会话在切换模型时读取（`agent_session.rs` 里
                        // `get_model_thinking_level` 的调用点是切模型路径）。
                        let current = mode.session().model();
                        if current.is_some_and(|current| {
                            current.provider == provider && current.id == model_id
                        }) {
                            // `persist: None`：本次只改会话内级别，不覆盖全局默认。
                            mode.session().set_thinking_level(level, Default::default());
                            mode.inner.borrow().footer.borrow_mut().invalidate();
                            mode.update_editor_border_color();
                        }
                    });
                });
            let weak_model_thinking_remove = weak.clone();
            let on_model_thinking_level_remove: ModelThinkingRemoveCallback =
                Rc::new(move |provider, model_id| {
                    mode!(weak_model_thinking_remove, |mode: &mut InteractiveMode| {
                        mode.settings_manager()
                            .remove_model_thinking_level(provider, model_id);
                        // 删除覆盖后当前模型回到“全局默认级别”；仍是同一个判断：只有当前
                        // 模型被删时才需要重算会话级别与边框色。
                        let current = mode.session().model();
                        if current.is_some_and(|current| {
                            current.provider == provider && current.id == model_id
                        }) {
                            let global_default = mode
                                .settings_manager()
                                .get_default_thinking_level()
                                .unwrap_or(DEFAULT_THINKING_LEVEL);
                            mode.session()
                                .set_thinking_level(global_default, Default::default());
                            mode.inner.borrow().footer.borrow_mut().invalidate();
                            mode.update_editor_border_color();
                        }
                    });
                });
            let weak_theme = weak.clone();
            let on_theme_change: Rc<dyn Fn(&str)> = Rc::new(move |theme_setting| {
                let theme_setting = theme_setting.to_string();
                run_mode_callback(&weak_theme, move |mode| {
                    mode.settings_manager().set_theme(theme_setting.clone());
                    mode.apply_theme_setting(&theme_setting);
                });
            });
            let weak_theme_preview = weak.clone();
            let on_theme_preview: Option<TextCallback> = Some(Rc::new(move |theme_name: &str| {
                let theme_name = theme_name.to_string();
                run_mode_callback(&weak_theme_preview, move |mode| {
                    mode.preview_theme(&theme_name);
                });
            }));
            let weak_ui_locale = weak.clone();
            let on_ui_locale_change: TextCallback = Rc::new(move |code: &str| {
                let code = code.to_string();
                mode!(weak_ui_locale, |mode: &mut InteractiveMode| {
                    // 写 `ui.locale`（只影响界面文案）后立即重建 translator 并刷新已存在
                    // 的 pending 行与审批队列；模型回答语言不受影响。
                    mode.settings_manager().set_ui_locale(&code);
                    mode.apply_ui_locale(&code);
                    let display = if code == "zh" {
                        "简体中文"
                    } else {
                        "English"
                    };
                    mode.show_status(&format!("Language: {display}"));
                });
            });
            let weak_hide_thinking = weak.clone();
            let on_hide_thinking_block_change: Rc<dyn Fn(bool)> = Rc::new(move |hidden| {
                mode!(weak_hide_thinking, |mode: &mut InteractiveMode| {
                    mode.inner.borrow_mut().hide_thinking_block = hidden;
                    mode.settings_manager().set_hide_thinking_block(hidden);
                    mode.update_thinking_block_visibility();
                });
            });
            let weak_mermaid = weak.clone();
            let on_mermaid_rendering_mode_change: Rc<
                dyn Fn(kiana_config::settings::MermaidRenderingMode),
            > = Rc::new(move |mode_value| {
                mode!(weak_mermaid, |mode: &mut InteractiveMode| {
                    let _ = mode
                        .settings_manager()
                        .set_mermaid_rendering_mode(mode_value);
                    let chat = Rc::clone(&mode.inner.borrow().chat_container);
                    chat.borrow_mut().invalidate();
                    mode.request_render();
                });
            });
            let weak_cache_miss = weak.clone();
            let on_show_cache_miss_notices_change: Rc<dyn Fn(bool)> = Rc::new(move |shown| {
                mode!(weak_cache_miss, |mode: &mut InteractiveMode| {
                    mode.settings_manager().set_show_cache_miss_notices(shown);
                    // 该设置决定历史条目里是否渲染 cache-miss 提示，只标脏不足以增删条目，
                    // 必须清空聊天容器并按会话条目重建。
                    mode.rebuild_chat_from_messages();
                });
            });
            let weak_collapse_changelog = weak.clone();
            let on_collapse_changelog_change: Rc<dyn Fn(bool)> = Rc::new(move |collapsed| {
                // 只写设置，不刷新当前 UI：该值在渲染更新提示/changelog 时重新读取
                // （`interactive_mode.rs` 的 `get_collapse_changelog` 调用点）。
                if let Some(inner) = weak_collapse_changelog.upgrade() {
                    InteractiveMode { inner }
                        .settings_manager()
                        .set_collapse_changelog(collapsed);
                }
            });
            let weak_telemetry = weak.clone();
            let on_enable_install_telemetry_change: Rc<dyn Fn(bool)> = Rc::new(move |enabled| {
                if let Some(inner) = weak_telemetry.upgrade() {
                    InteractiveMode { inner }
                        .settings_manager()
                        .set_enable_install_telemetry(enabled);
                }
            });
            let weak_quiet = weak.clone();
            let on_quiet_startup_change: Rc<dyn Fn(kiana_config::settings::QuietStartup)> =
                Rc::new(move |quiet| {
                    if let Some(inner) = weak_quiet.upgrade() {
                        InteractiveMode { inner }
                            .settings_manager()
                            .set_quiet_startup(quiet);
                    }
                });
            let weak_trust = weak.clone();
            let on_default_project_trust_change: Rc<
                dyn Fn(kiana_config::settings::DefaultProjectTrust),
            > = Rc::new(move |trust| {
                if let Some(inner) = weak_trust.upgrade() {
                    InteractiveMode { inner }
                        .settings_manager()
                        .set_default_project_trust(trust);
                }
            });
            let weak_double_escape = weak.clone();
            let on_double_escape_action_change: Rc<dyn Fn(DoubleEscapeAction)> =
                Rc::new(move |action| {
                    if let Some(inner) = weak_double_escape.upgrade() {
                        InteractiveMode { inner }
                            .settings_manager()
                            .set_double_escape_action(action);
                    }
                });
            let weak_tree_filter = weak.clone();
            let on_tree_filter_mode_change: Rc<dyn Fn(kiana_config::settings::TreeFilterMode)> =
                Rc::new(move |mode_value| {
                    if let Some(inner) = weak_tree_filter.upgrade() {
                        InteractiveMode { inner }
                            .settings_manager()
                            .set_tree_filter_mode(mode_value);
                    }
                });
            let weak_cursor = weak.clone();
            let on_show_hardware_cursor_change: Rc<dyn Fn(bool)> = Rc::new(move |enabled| {
                run_mode_callback(&weak_cursor, move |mode| {
                    mode.settings_manager().set_show_hardware_cursor(enabled);
                    mode.inner.borrow_mut().ui.set_show_hardware_cursor(enabled);
                });
            });
            let weak_padding = weak.clone();
            let on_editor_padding_x_change: Rc<dyn Fn(f64)> = Rc::new(move |padding| {
                mode!(weak_padding, |mode: &mut InteractiveMode| {
                    mode.settings_manager().set_editor_padding_x(padding);
                    // 两处都要改：`default_editor` 是常驻的主编辑器；`ActiveEditor::Custom`
                    // 时 `editor` 槽里装的是扩展提供的另一份编辑器实现，也要同步。
                    mode.inner
                        .borrow()
                        .default_editor
                        .borrow_mut()
                        .set_padding_x(padding);
                    if !matches!(
                        mode.inner.borrow().editor,
                        super::super::ActiveEditor::Default { .. }
                    ) {
                        // 编辑器接口收 `usize`；负数按 0 处理。
                        mode.inner
                            .borrow()
                            .editor
                            .set_padding_x(padding.max(0.0) as usize);
                    }
                });
            });
            let weak_output_pad = weak.clone();
            let on_output_pad_change: Rc<dyn Fn(u8)> = Rc::new(move |padding| {
                mode!(weak_output_pad, |mode: &mut InteractiveMode| {
                    mode.settings_manager().set_output_pad(i64::from(padding));
                    // `inner.output_pad` 是后续新建消息组件的初始值；已挂载的组件必须逐个
                    // 更新，否则新旧消息的缩进不一致。待发送队列容器同样要改。
                    mode.inner.borrow_mut().output_pad = usize::from(padding);
                    let (chat, pending) = {
                        let inner = mode.inner.borrow();
                        (
                            Rc::clone(&inner.chat_container),
                            Rc::clone(&inner.pending_messages_container),
                        )
                    };
                    for_each_message_child(&chat, |any| {
                        set_output_pad(any, usize::from(padding));
                    });
                    for_each_message_child(&pending, |any| {
                        set_output_pad(any, usize::from(padding));
                    });
                    mode.request_render();
                });
            });
            let weak_max_visible = weak.clone();
            let on_autocomplete_max_visible_change: Rc<dyn Fn(f64)> = Rc::new(move |max_visible| {
                mode!(weak_max_visible, |mode: &mut InteractiveMode| {
                    mode.settings_manager()
                        .set_autocomplete_max_visible(max_visible);
                    mode.inner
                        .borrow()
                        .default_editor
                        .borrow_mut()
                        .set_autocomplete_max_visible(max_visible);
                    if !matches!(
                        mode.inner.borrow().editor,
                        super::super::ActiveEditor::Default { .. }
                    ) {
                        mode.inner
                            .borrow()
                            .editor
                            .set_autocomplete_max_visible(max_visible.max(0.0) as usize);
                    }
                });
            });
            let weak_clear_on_shrink = weak.clone();
            let on_clear_on_shrink_change: Rc<dyn Fn(bool)> = Rc::new(move |enabled| {
                run_mode_callback(&weak_clear_on_shrink, move |mode| {
                    let _ = mode.settings_manager().set_clear_on_shrink(enabled);
                    mode.inner.borrow_mut().ui.set_clear_on_shrink(enabled);
                    // 与 `applyRuntimeSettings`（`interactive_mode/session.rs`）保持一致：
                    // 关闭 clear-on-shrink 且当前没有活动状态指示器时清空状态区，避免残留
                    // 上一次状态行/指示器；有活动指示器时不清，否则会拆掉正在渲染的它。
                    let inner = mode.inner.borrow();
                    if !enabled && inner.active_status_indicator.is_none() {
                        inner.status_container.borrow_mut().clear();
                    }
                });
            });
            let weak_progress = weak.clone();
            let on_show_terminal_progress_change: Rc<dyn Fn(bool)> = Rc::new(move |enabled| {
                if let Some(inner) = weak_progress.upgrade() {
                    let _ = InteractiveMode { inner }
                        .settings_manager()
                        .set_show_terminal_progress(enabled);
                }
            });
            let weak_tui_mode = weak.clone();
            let slot = Rc::clone(&selector_slot);
            let on_tui_mode_change: Rc<dyn Fn(SettingsTuiMode)> = Rc::new(move |mode_value| {
                let slot = Rc::clone(&slot);
                run_mode_callback(&weak_tui_mode, move |mode| {
                    let tui_mode = match mode_value {
                        SettingsTuiMode::Regular => kiana_tui::TuiMode::Regular,
                        SettingsTuiMode::Fullscreen => kiana_tui::TuiMode::Fullscreen,
                    };
                    // 失败（返回 false）只有一种原因：旧渲染器仍有活动 overlay。此时：
                    // 不写设置、把列表显示改回真实模式（下拉框已经显示了用户的选择）、
                    // 再用状态行提示。三步缺一不可，否则 UI 会显示一个未生效的值。
                    if !mode.switch_tui_mode(tui_mode, true, true) {
                        let current = mode.inner.borrow().ui.mode();
                        let selector = slot.borrow().clone();
                        if let Some(selector) = selector
                            && let Ok(mut selector) = selector.try_borrow_mut()
                        {
                            selector
                                .get_settings_list()
                                .update_value("tui-mode", tui_mode_str(settings_tui_mode(current)));
                        }
                        mode.show_status("Close active overlays before changing TUI mode");
                        return;
                    }
                    mode.settings_manager().set_tui_mode(mode_value);
                    // 与 `applyRuntimeSettings` 一致：清掉上一模式残留的状态行/指示器；
                    // 有活动状态指示器时跳过，避免拆掉正在渲染的指示器。
                    if mode.inner.borrow().active_status_indicator.is_none() {
                        mode.inner.borrow().status_container.borrow_mut().clear();
                    }
                    mode.show_status(&format!("TUI mode: {}", tui_mode_str(mode_value)));
                });
            });
            let weak_exit_output = weak.clone();
            let on_fullscreen_exit_output_change: Rc<
                dyn Fn(kiana_config::settings::FullscreenExitOutput),
            > = Rc::new(move |output| {
                if let Some(inner) = weak_exit_output.upgrade() {
                    InteractiveMode { inner }
                        .settings_manager()
                        .set_fullscreen_exit_output(output);
                }
            });
            let weak_scrollbar = weak.clone();
            let on_fullscreen_scrollbar_change: Rc<dyn Fn(FullscreenScrollbar)> =
                Rc::new(move |mode_value| {
                    mode!(weak_scrollbar, |mode: &mut InteractiveMode| {
                        let scrollbar = match mode_value {
                            FullscreenScrollbar::Auto => kiana_tui::ScrollViewScrollbar::Auto,
                            FullscreenScrollbar::Always => kiana_tui::ScrollViewScrollbar::Always,
                            FullscreenScrollbar::Hidden => kiana_tui::ScrollViewScrollbar::Hidden,
                        };
                        mode.settings_manager().set_fullscreen_scrollbar(scrollbar);
                        mode.apply_fullscreen_scrollbar_setting();
                    });
                });
            let weak_copy_on_select = weak.clone();
            let on_fullscreen_copy_on_select_change: Rc<dyn Fn(bool)> = Rc::new(move |enabled| {
                run_mode_callback(&weak_copy_on_select, move |mode| {
                    mode.settings_manager()
                        .set_fullscreen_copy_on_select(enabled);
                    mode.inner.borrow_mut().ui.set_copy_on_select(enabled);
                });
            });
            let weak_wheel = weak.clone();
            let on_fullscreen_wheel_scroll_lines_change: Rc<dyn Fn(kiana_tui::WheelScrollLines)> =
                Rc::new(move |lines| {
                    run_mode_callback(&weak_wheel, move |mode| {
                        mode.settings_manager()
                            .set_fullscreen_wheel_scroll_lines(lines);
                        mode.inner.borrow_mut().ui.set_wheel_scroll_lines(lines);
                    });
                });
            let weak_warnings = weak.clone();
            let on_warnings_change: Rc<dyn Fn(WarningSettings)> = Rc::new(move |warnings| {
                if let Some(inner) = weak_warnings.upgrade() {
                    InteractiveMode { inner }
                        .settings_manager()
                        .set_warnings(warnings);
                }
            });
            let weak_cancel = weak.clone();
            let done_cancel = done.clone();
            // 取消不写任何设置、也不做主题恢复：主题预览的恢复由 ThemeSubmenu 在取消时
            // 先用原始设置再调一次 `on_theme_preview` 完成（见组件文档）。这里只关闭选择器
            // 并请求重绘，让编辑器槽位恢复。
            let on_cancel: Rc<dyn Fn()> = Rc::new(move || {
                done_cancel.finish();
                if let Some(inner) = weak_cancel.upgrade() {
                    InteractiveMode { inner }.request_render();
                }
            });

            let callbacks = SettingsCallbacks {
                on_auto_compact_change,
                on_show_images_change,
                on_image_width_cells_change,
                on_auto_resize_images_change,
                on_block_images_change,
                on_enable_skill_commands_change,
                on_steering_mode_change,
                on_follow_up_mode_change,
                on_transport_change,
                on_session_share_destination_change,
                on_http_idle_timeout_ms_change,
                on_cache_warming_mode_change,
                on_model_thinking_level_change,
                on_model_thinking_level_remove,
                on_theme_change,
                on_theme_preview,
                on_ui_locale_change,
                on_hide_thinking_block_change,
                on_mermaid_rendering_mode_change,
                on_show_cache_miss_notices_change,
                on_collapse_changelog_change,
                on_enable_install_telemetry_change,
                on_double_escape_action_change,
                on_tree_filter_mode_change,
                on_show_hardware_cursor_change,
                on_editor_padding_x_change,
                on_output_pad_change,
                on_autocomplete_max_visible_change,
                on_quiet_startup_change,
                on_default_project_trust_change,
                on_clear_on_shrink_change,
                on_show_terminal_progress_change,
                on_tui_mode_change,
                on_fullscreen_exit_output_change,
                on_fullscreen_scrollbar_change,
                on_fullscreen_copy_on_select_change,
                on_fullscreen_wheel_scroll_lines_change,
                on_warnings_change,
                on_cancel,
            };
            let selector = SettingsSelectorComponent::new(config.clone(), callbacks);
            let shared = Rc::new(RefCell::new(selector));
            // 焦点地址取自组件内部的设置列表（`get_settings_list`），与 TS 的
            // `focus: selector.getSettingsList()` 一致；面板自身上下边框占两行，聚焦列表
            // 才能正确滚动。同时把共享句柄登记进 `selector_slot`，供 TUI 模式回调在失败
            // 分支回改列表显示值。
            let focus_address = {
                let mut selector = shared.borrow_mut();
                *selector_slot.borrow_mut() = Some(Rc::clone(&shared));
                component_address(selector.get_settings_list() as &dyn Component)
            };
            SelectorCreation {
                component: Rc::clone(&shared) as kiana_tui::layout_node::SharedComponent,
                focus: SelectorFocus::Address(focus_address),
                dispose: None,
            }
        });
    }

    /// `onThemeChange` 的控制器路径（`themeController.setThemeSetting`）。
    ///
    /// 先 `take()` 再放回：`set_theme_setting` 借用控制器与 host，期间不能持有
    /// `inner` 的可变借用（见 `interactive_mode.rs` 的借用纪律）；`RendererThemeHost`
    /// 只是渲染器句柄的包装，`invalidate` / `request_render` 都转发给当前渲染器，因此
    /// 主题应用会触发重绘。控制器缺失时（例如未初始化）静默返回，设置本身已由调用方写入。
    ///
    /// 返回的 future 被立即丢弃：它只用于等待终端颜色查询完成，而“发起查询”这一步在同步
    /// 部分已经执行（`kiana-theme` 的 `apply_from_settings` 文档明确丢弃 future 不会丢结果，
    /// 未就绪的查询仍留在控制器队列里）。
    fn apply_theme_setting(&mut self, theme_setting: &str) {
        let controller = self.inner.borrow_mut().theme_controller.take();
        let Some(mut controller) = controller else {
            return;
        };
        let renderer = Rc::clone(&self.inner.borrow().renderer);
        {
            let mut host = super::super::RendererThemeHost { renderer };
            let future = controller.set_theme_setting(&mut host, theme_setting);
            drop(future);
        }
        self.inner.borrow_mut().theme_controller = Some(controller);
    }

    /// `onThemePreview`（`themeController.preview`）。
    ///
    /// 与 `apply_theme_setting` 相同的 `take()`/放回纪律。区别是 `preview` 是同步函数、
    /// 不写设置：它只切换全局主题并请求重绘，控制器的 `current_theme_setting` /
    /// `active_theme_name` 都不更新（`theme_controller.rs` 的 `preview` 文档）。组件在
    /// 光标移动时调用它，取消子菜单时用原始设置再调一次以恢复画面（见
    /// `components/settings_selector.rs` 的 ThemeSubmenu 说明）。
    fn preview_theme(&mut self, theme_name: &str) {
        let controller = self.inner.borrow_mut().theme_controller.take();
        let Some(mut controller) = controller else {
            return;
        };
        let renderer = Rc::clone(&self.inner.borrow().renderer);
        {
            let mut host = super::super::RendererThemeHost { renderer };
            controller.preview(&mut host, theme_name);
        }
        self.inner.borrow_mut().theme_controller = Some(controller);
    }

    /// `showSettingsSelector()` 的测试接缝。
    ///
    /// 生产上是 `pub(crate)` 的 `show_settings_selector`；测试需要从集成测试（独立 crate）
    /// 直接打开面板，因此暴露为 `pub` + `#[doc(hidden)]`，只做转发。
    #[doc(hidden)]
    pub fn test_show_settings_selector(&mut self) {
        self.show_settings_selector();
    }
}
