//! components/settings-selector.ts。
//!
//! `/settings` 主面板：一个带搜索的 [`SettingsList`]，以及 warnings / per-model thinking /
//! theme 三个子菜单。所有标签、描述与回调映射逐字移植。
//!
//! ## 与 TS 的表示差异
//!
//! * `SettingsConfig` / `SettingsCallbacks` 在 TS 里是接口；Rust 用字段全为 `pub` 的结构体，
//!   回调是 [`Rc`] 共享的 `Fn`（组件与子菜单需要同时持有）。
//! * JS 的 `Record<string, ThinkingLevel>`（`modelThinkingLevels`）用 [`IndexMap`] 复刻插入序。
//! * 数字 → 字符串（`String(config.x)`）走 `js_number_string`，与 JS `Number::toString`
//!   对齐；`parseInt` 对应 `parse_int_js`。
//! * 列表项按 TS 的「基础数组 + 级联 splice」顺序插入（见 `spec/ca-interactive-components.md`
//!   §A.8b），保证 `supportsImages === false` 时索引仍然正确。
//! * 模块私有的子菜单组件（`WarningSettingsSubmenu` / `ThemeSubmenu`）保持私有；它们通过
//!   `Rc<RefCell<…>>` 共享状态，使子菜单回调能在自己的 `handleInput` 调用栈里替换当前面板。
//!
//! TS 源：`packages/coding-agent/src/modes/interactive/components/settings-selector.ts`。
//!
//! ## 设计原因与执行流程
//!
//! 面板本身不持有设置存储：宿主 `interactive_mode/commands/settings.rs` 在打开面板时
//! 把当前状态拼成 [`SettingsConfig`] 快照，再把每个 setter 包装成 [`SettingsCallbacks`]；
//! 用户改动只经回调交回宿主。组件内部仅保留两份**可变副本**（`warnings` 与
//! `model_thinking_levels`），用于同一面板内反复打开子菜单时显示最新摘要，它们不会
//! 写回 `config` 字段——下一次打开面板仍以宿主重新组装的快照为准。
//!
//! 构造顺序本身就是契约（黄金渲染逐行比对），改动需同步测试：
//!
//! 1. 读终端能力 `get_capabilities().images`，它只决定 `show-images` 与
//!    `image-width-cells` 两项是否插入；`auto-resize-images`、`block-images` 始终存在。
//! 2. 先建立 25 项基础数组，再按「图片两项（硬编码下标 1、2）→ 其余按 id 级联插到
//!    目标项之后」的顺序展开；`insert_after` 复刻 `findIndex(...) + 1`，目标缺失时
//!    插入到索引 0。
//! 3. 主 [`SettingsList`] 固定 `max_visible = 10`、启用模糊搜索；搜索只匹配条目的
//!    `label`（不看 id 与 description）。查询为空时展示顺序就是构造顺序；查询非空时
//!    `SettingsList` 内部的 `fuzzy_filter` 会按匹配分数做稳定排序，屏幕上的顺序因此
//!    可能不同于构造顺序（条目本身不被修改）。
//! 4. 渲染时上下各包一条 [`DynamicBorder`]；鼠标坐标要先减去顶部边框的一行。
//!
//! ## 子菜单的状态机（ThemeSubmenu）
//!
//! `ThemeSubmenuState` 的 `mode` 只有 `Single` / `Automatic` 两态，`content`（渲染目标）
//! 与 `input`（按键目标）分开保存，进入某个模式时由 `theme_show_single_menu` /
//! `theme_show_automatic_menu` 成对替换。移动光标只经 `on_theme_preview` 预览；单模式下
//! 选中具体主题、或自动模式下按 `Apply`，才通过 `on_done(Some(..))` 把设置交回宿主；取消会先把
//! 预览恢复成原始设置，再以 `on_done(None)` 关闭（不写值）。`on_theme_preview` 是可选
//! 回调，缺失时预览和恢复都被跳过，其余流程不变。
//!
//! ## 注意事项
//!
//! * 回调在触发它的 `handle_input` / `handle_mouse` 调用栈内同步执行，宿主实现必须
//!   能承受“子菜单处理输入的过程中被调用”。
//! * 渲染依赖全局主题已初始化：`muted` / `bold_accent` 在 `THEME` 未初始化时会 `expect`
//!   panic（对应 TS 在这里抛异常）；测试在 `setup()` 里先调用 `kiana_theme::init_theme`。
//! * 组件不校验 `config` 取值：`default_model` 不在 `available_default_models` 中时
//!   只视为“无默认模型”；`current_theme` 既可以是单主题名，也可以是 `light/dark`
//!   形式的自动设置，识别交给 `parse_auto_theme_setting`。
//! * `tests/components_settings_selector.rs` 锁定了条目顺序、主题/思考等级标记、自动
//!   主题的 40 列窄宽度鼠标偏移，以及 120 列黄金渲染；改文案或插入顺序前先核对该文件。
//!
//! ## 阅读顺序
//!
//! 1. [`SettingsConfig`] / [`SettingsCallbacks`]：宿主传入的只读快照与回调面；
//! 2. [`SettingsSelectorComponent::new`]：25 项基础数组、图片项与级联插入、id → 回调映射；
//! 3. `WarningSettingsSubmenu` / `ThemeSubmenu` 一族：三个子菜单各自的局部状态与切换流程；
//! 4. 文件末尾的 `Component` 实现：上下边框、鼠标坐标换算与焦点下钻。
//!
//! 公开面由 `components/index.rs` 再导出 `SettingsConfig` / `SettingsCallbacks` /
//! `SettingsSelectorComponent`；本文件其余类型（含两个子菜单）均为模块私有。

use std::cell::RefCell;
use std::cmp::Ordering;
use std::collections::HashMap;
use std::rc::Rc;

use indexmap::IndexMap;
use kiana_ai::models::get_supported_thinking_levels;
use kiana_ai::{Model, ModelThinkingLevel, Transport};
use kiana_config::settings::defaults::SETTINGS_DEFAULTS;
use kiana_config::settings::{
    CACHE_WARMING_MODES, CacheWarmingMode, DefaultProjectTrust, DoubleEscapeAction,
    FullscreenExitOutput, FullscreenScrollbar, MermaidRenderingMode, QuietStartup,
    SessionShareDestination, SteeringMode, TreeFilterMode, TuiMode, WarningSettings,
};
use kiana_core::http_dispatcher::{HTTP_IDLE_TIMEOUT_CHOICES, format_http_idle_timeout_ms};
use kiana_theme::{
    AutoTheme, SYSTEM_THEME_NAME, ThemeColor, get_settings_list_theme, parse_auto_theme_setting,
};
use kiana_tui::TerminalColorScheme;
use kiana_tui::components::settings_list::{
    SettingChangeCallback, SettingsDoneCallback, SettingsListOptions, SubmenuFactory,
};
use kiana_tui::{
    Component, SettingItem, SettingsList, Spacer, Text, TuiMouseEvent, TuiMouseEventResult,
    WheelScrollLines, get_capabilities,
};
use kiana_ui_kit::{DynamicBorder, key_display_text};

use crate::components::settings_submenu::{
    SelectSubmenu, SelectSubmenuOptions, SelectValueCallback, StepCompleteCallback, StepText,
    SteppedSubmenu, SteppedSubmenuOptions, SteppedSubmenuStep, VoidCallback,
};

const MODEL_PICKER_LAYOUT_MIN: f64 = 12.0;
const MODEL_PICKER_LAYOUT_MAX: f64 = 46.0;
const CLEAR_OVERRIDE_VALUE: &str = "__clear__";

/// TS `MODEL_PICKER_LAYOUT`。
fn model_picker_layout() -> kiana_tui::SelectListLayoutOptions {
    kiana_tui::SelectListLayoutOptions {
        min_primary_column_width: Some(MODEL_PICKER_LAYOUT_MIN),
        max_primary_column_width: Some(MODEL_PICKER_LAYOUT_MAX),
        truncate_primary: None,
    }
}

/// TS `THINKING_DESCRIPTIONS`。
fn thinking_description(level: ModelThinkingLevel) -> &'static str {
    match level {
        ModelThinkingLevel::Off => "No reasoning",
        ModelThinkingLevel::Minimal => "Very brief reasoning (~1k tokens)",
        ModelThinkingLevel::Low => "Light reasoning (~2k tokens)",
        ModelThinkingLevel::Medium => "Moderate reasoning (~8k tokens)",
        ModelThinkingLevel::High => "Deep reasoning (~16k tokens)",
        ModelThinkingLevel::Xhigh => "Extra-high reasoning (~32k tokens)",
        ModelThinkingLevel::Max => "Maximum reasoning",
    }
}

/// `ThinkingLevel` 的 JSON 字符串形式（TS 的字符串字面量）。
fn thinking_level_str(level: ModelThinkingLevel) -> &'static str {
    match level {
        ModelThinkingLevel::Off => "off",
        ModelThinkingLevel::Minimal => "minimal",
        ModelThinkingLevel::Low => "low",
        ModelThinkingLevel::Medium => "medium",
        ModelThinkingLevel::High => "high",
        ModelThinkingLevel::Xhigh => "xhigh",
        ModelThinkingLevel::Max => "max",
    }
}

/// 把选项字符串解析回 `ThinkingLevel`（内部生成的选项，未知值回落到 `off`）。
fn parse_thinking_level(value: &str) -> ModelThinkingLevel {
    match value {
        "minimal" => ModelThinkingLevel::Minimal,
        "low" => ModelThinkingLevel::Low,
        "medium" => ModelThinkingLevel::Medium,
        "high" => ModelThinkingLevel::High,
        "xhigh" => ModelThinkingLevel::Xhigh,
        "max" => ModelThinkingLevel::Max,
        _ => ModelThinkingLevel::Off,
    }
}

/// JS `String(number)`（`NaN`/`±Infinity` 有专门文本；有限值走 kiana-json 的
/// `Number::toString` 算法）。
fn js_number_string(value: f64) -> String {
    if value.is_nan() {
        "NaN".to_string()
    } else if value == f64::INFINITY {
        "Infinity".to_string()
    } else if value == f64::NEG_INFINITY {
        "-Infinity".to_string()
    } else {
        kiana_json::format_js_number(value)
    }
}

/// JS `parseInt(value, 10)`（解析失败 → `NaN`；只取前导整数部分）。
///
/// 不能直接用 Rust 的 `str::parse`：后者遇到尾部垃圾会失败，而 `parseInt("80px")`
/// 返回 80。这里先跳过前导空白，只接受位于字符串开头的 `+`/`-`，然后读取连续 ASCII
/// 数字并在第一个非数字字符处停止；没有任何数字（包括只有符号）时返回 `NaN`。
fn parse_int_js(value: &str) -> f64 {
    let trimmed = value.trim_start();
    let mut end = 0usize;
    let mut saw_digit = false;
    for (index, character) in trimmed.char_indices() {
        let is_sign = index == 0 && (character == '+' || character == '-');
        if character.is_ascii_digit() || is_sign {
            if character.is_ascii_digit() {
                saw_digit = true;
            }
            end = index + character.len_utf8();
        } else {
            break;
        }
    }
    if !saw_digit {
        return f64::NAN;
    }
    trimmed[..end].parse::<f64>().unwrap_or(f64::NAN)
}

/// TS `theme.fg("muted", text)`。
fn muted(text: &str) -> String {
    kiana_theme::THEME
        .fg(ThemeColor::Muted, text)
        .expect("theme must be initialized before rendering (TS throws here)")
}

/// TS `theme.bold(theme.fg("accent", text))`（SelectSubmenu 标题的顺序）。
fn bold_accent(text: &str) -> String {
    let accent = kiana_theme::THEME
        .fg(ThemeColor::Accent, text)
        .expect("theme must be initialized before rendering (TS throws here)");
    kiana_theme::THEME
        .bold(&accent)
        .expect("theme must be initialized before rendering (TS throws here)")
}

// =============================================================================
// SettingsConfig / SettingsCallbacks
// =============================================================================

/// `onModelThinkingLevelChange` 的回调类型。
///
/// 参数依次是模型所属 `provider`、模型 `id` 与新的默认思考等级。只在思考等级子菜单
/// 第二步选中具体等级时调用；清除 override 走 [`ModelThinkingRemoveCallback`]。
pub type ModelThinkingLevelChangeCallback = Rc<dyn Fn(&str, &str, ModelThinkingLevel)>;
/// `onModelThinkingLevelRemove` 的回调类型。
///
/// 用户选择 `(clear override)` 时调用，参数是 `provider` 与模型 `id`。宿主应删除该模型的
/// 持久化覆盖值（本组件已同步从本地副本中移除）。
pub type ModelThinkingRemoveCallback = Rc<dyn Fn(&str, &str)>;
/// 接受文本参数的回调类型（`onThemeChange` / `onThemePreview`）。
///
/// 参数是主题设置字符串：可能是单个主题名，也可能是自动模式的 `light/dark` 形式；
/// 两种形式都直接交给宿主解析。
pub type TextCallback = Rc<dyn Fn(&str)>;

/// TS `SettingsConfig`（§2.54 的全字段）。
///
/// 这是宿主打开面板时拍下的**只读快照**：组件构造后不再读回宿主，也不把用户改动写回
/// 这里的字段；改动通过 [`SettingsCallbacks`] 交回宿主处理。
/// 例外是 `warnings` 与 `model_thinking_levels`：组件会克隆出内部可变副本供子菜单
/// 连续操作，详见各字段说明。
#[derive(Clone)]
pub struct SettingsConfig {
    /// `autoCompact`。
    pub auto_compact: bool,
    /// `defaultModel`（`provider/modelId` 形式）。
    ///
    /// 不在 `available_default_models` 中时（例如宿主传 `"not set"`）组件视为没有默认
    /// 模型：思考等级子菜单仍列出全部可用模型，只是它不参与默认模型的排序优先级与
    /// 第一步预选。
    pub default_model: String,
    /// `currentModel`。
    ///
    /// 仅用于思考等级子菜单：当前模型的排序优先级最高，并作为第一步的预选项。
    /// 若它不在 `available_default_models` 中，排序比较不会命中，预选也会回落到首项。
    pub current_model: Option<Model>,
    /// `availableDefaultModels`。
    ///
    /// 思考等级子菜单的数据源。每项以 `provider/id` 生成 key；列表为空时第一步会显示
    /// `No models available` 占位项（该占位项不对应任何模型，选中后没有可选等级）。
    pub available_default_models: Vec<Model>,
    /// `showImages`。
    pub show_images: bool,
    /// `imageWidthCells`。
    ///
    /// 单位是终端列，用 `String(number)` 形式显示；候选项固定为 60/80/120，但当前值
    /// 可以是任意数字（照原样显示）。
    pub image_width_cells: f64,
    /// `autoResizeImages`。
    pub auto_resize_images: bool,
    /// `blockImages`。
    pub block_images: bool,
    /// `enableSkillCommands`。
    pub enable_skill_commands: bool,
    /// `steeringMode`。
    pub steering_mode: SteeringMode,
    /// `followUpMode`。
    pub follow_up_mode: SteeringMode,
    /// `transport`。
    pub transport: Transport,
    /// `sessionShareDestination`。
    pub session_share_destination: SessionShareDestination,
    /// `httpIdleTimeoutMs`。
    ///
    /// 单位毫秒；选项文案来自 `HTTP_IDLE_TIMEOUT_CHOICES` 的 label，选中后回报该选项的
    /// `timeout_ms`（其中 `disabled` 对应 0）。
    pub http_idle_timeout_ms: f64,
    /// `cacheWarmingMode`。
    pub cache_warming_mode: CacheWarmingMode,
    /// `thinkingLevel`。
    ///
    /// 全局默认思考等级；本文件里只用于 `(clear override)` 项的说明文本
    /// （`Revert to global default (...)`）。
    pub thinking_level: ModelThinkingLevel,
    /// `availableThinkingLevels`（TS 声明但当前实现未使用）。
    pub available_thinking_levels: Vec<ModelThinkingLevel>,
    /// `modelThinkingLevels`。
    ///
    /// key 为 `provider/modelId`，用 [`IndexMap`] 保持插入序。构造时被克隆成组件内的
    /// 可变副本：子菜单里的设置/清除只改副本并通知回调，本字段保持快照值。
    pub model_thinking_levels: IndexMap<String, ModelThinkingLevel>,
    /// `currentTheme`。
    ///
    /// 单主题名或自动模式的 `light/dark` 形式；后者由 `parse_auto_theme_setting` 识别，
    /// 决定主题子菜单初始进入单主题还是自动模式。
    pub current_theme: String,
    /// `uiLocale`。
    ///
    /// 存语言代码（如 `en` / `zh`，可带地区如 `zh-CN`）；面板显示 `English` /
    /// `简体中文`，回调只回传代码。只影响界面文案，不影响模型回答语言。
    pub ui_locale: String,
    /// `terminalTheme`。
    ///
    /// 决定自动模式下当前生效的是 light 还是 dark，也用于从自动模式切回单模式时挑选
    /// 初始主题。
    pub terminal_theme: TerminalColorScheme,
    /// `availableThemes`。
    ///
    /// 顺序即选择菜单顺序；名为 `SYSTEM_THEME_NAME` 的主题会附带说明，并在单主题菜单
    /// 中被移到最前。
    pub available_themes: Vec<String>,
    /// `hideThinkingBlock`。
    pub hide_thinking_block: bool,
    /// `mermaidRenderingMode`。
    pub mermaid_rendering_mode: MermaidRenderingMode,
    /// `showCacheMissNotices`。
    pub show_cache_miss_notices: bool,
    /// `collapseChangelog`。
    pub collapse_changelog: bool,
    /// `enableInstallTelemetry`。
    pub enable_install_telemetry: bool,
    /// `doubleEscapeAction`。
    pub double_escape_action: DoubleEscapeAction,
    /// `treeFilterMode`。
    pub tree_filter_mode: TreeFilterMode,
    /// `showHardwareCursor`。
    pub show_hardware_cursor: bool,
    /// `editorPaddingX`。
    pub editor_padding_x: f64,
    /// `outputPad`（`0 | 1`）。
    pub output_pad: u8,
    /// `autocompleteMaxVisible`。
    pub autocomplete_max_visible: f64,
    /// `quietStartup`。
    pub quiet_startup: QuietStartup,
    /// `defaultProjectTrust`。
    pub default_project_trust: DefaultProjectTrust,
    /// `clearOnShrink`。
    pub clear_on_shrink: bool,
    /// `showTerminalProgress`。
    pub show_terminal_progress: bool,
    /// `tuiMode`。
    pub tui_mode: TuiMode,
    /// `fullscreenExitOutput`。
    pub fullscreen_exit_output: FullscreenExitOutput,
    /// `fullscreenScrollbar`。
    pub fullscreen_scrollbar: FullscreenScrollbar,
    /// `fullscreenCopyOnSelect`。
    pub fullscreen_copy_on_select: bool,
    /// `fullscreenWheelScrollLines`。
    ///
    /// 候选项由固定集合 1/2/3/5/10 与当前值去重、升序排列后生成，`auto` 永远排在最前。
    pub fullscreen_wheel_scroll_lines: WheelScrollLines,
    /// `warnings`。
    ///
    /// 组件用它初始化警告子菜单的本地副本；每次切换都会把完整的 `WarningSettings`
    /// 快照交给 `on_warnings_change`。
    pub warnings: WarningSettings,
}

// 测试与调用方兜底用的默认快照：能从 SETTINGS_DEFAULTS 取到的字段都取默认设置，
// 其余字段直接写固定常量（`transport = Auto`、`thinking_level = Medium`、
// `current_theme = "dark"`、`terminal_theme = Dark`、`show_hardware_cursor = false`）。
// `warnings` 例外：用的是 `WarningSettings::default()`（`anthropic_extra_usage = None`），
// 不是 `SETTINGS_DEFAULTS.warnings`；警告子菜单打开时才回落到后者的 `true`。
// 模型/主题列表默认为空，因此直接 new 出来的主列表在思考等级子菜单里只有
// “No models available” 占位项，主题菜单也只有 `automatic` 一项。
impl Default for SettingsConfig {
    fn default() -> Self {
        SettingsConfig {
            auto_compact: SETTINGS_DEFAULTS.compaction.enabled,
            default_model: String::new(),
            current_model: None,
            available_default_models: Vec::new(),
            show_images: SETTINGS_DEFAULTS.terminal.show_images,
            image_width_cells: SETTINGS_DEFAULTS.terminal.image_width_cells,
            auto_resize_images: SETTINGS_DEFAULTS.images.auto_resize,
            block_images: SETTINGS_DEFAULTS.images.block_images,
            enable_skill_commands: SETTINGS_DEFAULTS.enable_skill_commands,
            steering_mode: SETTINGS_DEFAULTS.steering_mode,
            follow_up_mode: SETTINGS_DEFAULTS.follow_up_mode,
            transport: Transport::Auto,
            session_share_destination: SETTINGS_DEFAULTS.session_share_destination,
            http_idle_timeout_ms: kiana_core::http_dispatcher::DEFAULT_HTTP_IDLE_TIMEOUT_MS as f64,
            cache_warming_mode: SETTINGS_DEFAULTS.cache_warming,
            thinking_level: ModelThinkingLevel::Medium,
            available_thinking_levels: Vec::new(),
            model_thinking_levels: IndexMap::new(),
            current_theme: "dark".to_string(),
            ui_locale: "en".to_string(),
            terminal_theme: TerminalColorScheme::Dark,
            available_themes: Vec::new(),
            hide_thinking_block: SETTINGS_DEFAULTS.hide_thinking_block,
            mermaid_rendering_mode: SETTINGS_DEFAULTS.markdown.mermaid,
            show_cache_miss_notices: SETTINGS_DEFAULTS.show_cache_miss_notices,
            collapse_changelog: SETTINGS_DEFAULTS.collapse_changelog,
            enable_install_telemetry: SETTINGS_DEFAULTS.enable_install_telemetry,
            double_escape_action: SETTINGS_DEFAULTS.double_escape_action,
            tree_filter_mode: SETTINGS_DEFAULTS.tree_filter_mode,
            show_hardware_cursor: false,
            editor_padding_x: SETTINGS_DEFAULTS.editor_padding_x,
            output_pad: SETTINGS_DEFAULTS.output_pad as u8,
            autocomplete_max_visible: SETTINGS_DEFAULTS.autocomplete_max_visible,
            quiet_startup: SETTINGS_DEFAULTS.quiet_startup,
            default_project_trust: SETTINGS_DEFAULTS.default_project_trust,
            clear_on_shrink: SETTINGS_DEFAULTS.terminal.clear_on_shrink,
            show_terminal_progress: SETTINGS_DEFAULTS.terminal.show_terminal_progress,
            tui_mode: SETTINGS_DEFAULTS.tui_mode,
            fullscreen_exit_output: SETTINGS_DEFAULTS.fullscreen_exit_output,
            fullscreen_scrollbar: SETTINGS_DEFAULTS.fullscreen_scrollbar,
            fullscreen_copy_on_select: SETTINGS_DEFAULTS.fullscreen_copy_on_select,
            fullscreen_wheel_scroll_lines: wheel_lines_from_settings(
                SETTINGS_DEFAULTS.fullscreen_wheel_scroll_lines,
            ),
            warnings: WarningSettings::default(),
        }
    }
}

/// TS `SettingsCallbacks`（除 `onThemePreview` 外全部必需）。
///
/// 回调统一用 [`Rc`] 共享：主列表与各 `SubmenuFactory` 会同时持有克隆，因此实现必须
/// 是 `'static`。它们由 [`SettingsList`] 在触发它的输入/鼠标处理内部**同步**调用，
/// 没有返回值或错误通道——失败需要在宿主实现里自行处理。
/// `Default` 为所有回调填入空实现、`onThemePreview` 为 `None`，方便测试只覆写关心的
/// 字段；生产路径由 `interactive_mode/commands/settings.rs` 全部填实。
#[derive(Clone)]
pub struct SettingsCallbacks {
    /// `onAutoCompactChange`。
    pub on_auto_compact_change: Rc<dyn Fn(bool)>,
    /// `onShowImagesChange`。
    pub on_show_images_change: Rc<dyn Fn(bool)>,
    /// `onImageWidthCellsChange`。
    pub on_image_width_cells_change: Rc<dyn Fn(f64)>,
    /// `onAutoResizeImagesChange`。
    pub on_auto_resize_images_change: Rc<dyn Fn(bool)>,
    /// `onBlockImagesChange`。
    pub on_block_images_change: Rc<dyn Fn(bool)>,
    /// `onEnableSkillCommandsChange`。
    pub on_enable_skill_commands_change: Rc<dyn Fn(bool)>,
    /// `onSteeringModeChange`。
    pub on_steering_mode_change: Rc<dyn Fn(SteeringMode)>,
    /// `onFollowUpModeChange`。
    pub on_follow_up_mode_change: Rc<dyn Fn(SteeringMode)>,
    /// `onTransportChange`。
    pub on_transport_change: Rc<dyn Fn(Transport)>,
    /// `onSessionShareDestinationChange`。
    pub on_session_share_destination_change: Rc<dyn Fn(SessionShareDestination)>,
    /// `onHttpIdleTimeoutMsChange`。
    ///
    /// 收到的是所选选项对应的毫秒数（`disabled` 为 0），不是选项文案。
    pub on_http_idle_timeout_ms_change: Rc<dyn Fn(f64)>,
    /// `onCacheWarmingModeChange`。
    pub on_cache_warming_mode_change: Rc<dyn Fn(CacheWarmingMode)>,
    /// `onModelThinkingLevelChange`。
    pub on_model_thinking_level_change: ModelThinkingLevelChangeCallback,
    /// `onModelThinkingLevelRemove`。
    pub on_model_thinking_level_remove: ModelThinkingRemoveCallback,
    /// `onThemeChange`。
    pub on_theme_change: Rc<dyn Fn(&str)>,
    /// `onThemePreview`（可选）。
    ///
    /// 光标在主题菜单中移动、以及取消时恢复原主题都会调用；它是临时预览，具体如何应用
    /// 由宿主实现决定。没有提供时所有预览/恢复动作被跳过，主题选择流程不变。
    pub on_theme_preview: Option<TextCallback>,
    /// `onUiLocaleChange`。
    ///
    /// 收到语言代码（如 `en` / `zh`），不是面板显示名；宿主负责写 `ui.locale` 并刷新
    /// 界面文案。
    pub on_ui_locale_change: TextCallback,
    /// `onHideThinkingBlockChange`。
    pub on_hide_thinking_block_change: Rc<dyn Fn(bool)>,
    /// `onMermaidRenderingModeChange`。
    pub on_mermaid_rendering_mode_change: Rc<dyn Fn(MermaidRenderingMode)>,
    /// `onShowCacheMissNoticesChange`。
    pub on_show_cache_miss_notices_change: Rc<dyn Fn(bool)>,
    /// `onCollapseChangelogChange`。
    pub on_collapse_changelog_change: Rc<dyn Fn(bool)>,
    /// `onEnableInstallTelemetryChange`。
    pub on_enable_install_telemetry_change: Rc<dyn Fn(bool)>,
    /// `onDoubleEscapeActionChange`。
    pub on_double_escape_action_change: Rc<dyn Fn(DoubleEscapeAction)>,
    /// `onTreeFilterModeChange`。
    pub on_tree_filter_mode_change: Rc<dyn Fn(TreeFilterMode)>,
    /// `onShowHardwareCursorChange`。
    pub on_show_hardware_cursor_change: Rc<dyn Fn(bool)>,
    /// `onEditorPaddingXChange`。
    pub on_editor_padding_x_change: Rc<dyn Fn(f64)>,
    /// `onOutputPadChange`。
    ///
    /// 只会收到 0 或 1；选项值中除 `"0"` 外的一切都映射为 1。
    pub on_output_pad_change: Rc<dyn Fn(u8)>,
    /// `onAutocompleteMaxVisibleChange`。
    pub on_autocomplete_max_visible_change: Rc<dyn Fn(f64)>,
    /// `onQuietStartupChange`。
    pub on_quiet_startup_change: Rc<dyn Fn(QuietStartup)>,
    /// `onDefaultProjectTrustChange`。
    pub on_default_project_trust_change: Rc<dyn Fn(DefaultProjectTrust)>,
    /// `onClearOnShrinkChange`。
    pub on_clear_on_shrink_change: Rc<dyn Fn(bool)>,
    /// `onShowTerminalProgressChange`。
    pub on_show_terminal_progress_change: Rc<dyn Fn(bool)>,
    /// `onTuiModeChange`。
    pub on_tui_mode_change: Rc<dyn Fn(TuiMode)>,
    /// `onFullscreenExitOutputChange`。
    pub on_fullscreen_exit_output_change: Rc<dyn Fn(FullscreenExitOutput)>,
    /// `onFullscreenScrollbarChange`。
    pub on_fullscreen_scrollbar_change: Rc<dyn Fn(FullscreenScrollbar)>,
    /// `onFullscreenCopyOnSelectChange`。
    pub on_fullscreen_copy_on_select_change: Rc<dyn Fn(bool)>,
    /// `onFullscreenWheelScrollLinesChange`。
    pub on_fullscreen_wheel_scroll_lines_change: Rc<dyn Fn(WheelScrollLines)>,
    /// `onWarningsChange`。
    ///
    /// 每次开关都回传完整的 `WarningSettings` 快照（不是单个字段的增量）。
    pub on_warnings_change: Rc<dyn Fn(WarningSettings)>,
    /// `onCancel`。
    ///
    /// 主列表收到取消键且**没有打开子菜单**时调用；子菜单打开时 Esc 由子菜单自己处理
    /// （要么回到上一步/关闭，要么经 done 关闭），不会直达这里。
    pub on_cancel: Rc<dyn Fn()>,
}

impl Default for SettingsCallbacks {
    fn default() -> Self {
        SettingsCallbacks {
            on_auto_compact_change: Rc::new(|_| {}),
            on_show_images_change: Rc::new(|_| {}),
            on_image_width_cells_change: Rc::new(|_| {}),
            on_auto_resize_images_change: Rc::new(|_| {}),
            on_block_images_change: Rc::new(|_| {}),
            on_enable_skill_commands_change: Rc::new(|_| {}),
            on_steering_mode_change: Rc::new(|_| {}),
            on_follow_up_mode_change: Rc::new(|_| {}),
            on_transport_change: Rc::new(|_| {}),
            on_session_share_destination_change: Rc::new(|_| {}),
            on_http_idle_timeout_ms_change: Rc::new(|_| {}),
            on_cache_warming_mode_change: Rc::new(|_| {}),
            on_model_thinking_level_change: Rc::new(|_, _, _| {}),
            on_model_thinking_level_remove: Rc::new(|_, _| {}),
            on_theme_change: Rc::new(|_| {}),
            on_theme_preview: None,
            on_ui_locale_change: Rc::new(|_| {}),
            on_hide_thinking_block_change: Rc::new(|_| {}),
            on_mermaid_rendering_mode_change: Rc::new(|_| {}),
            on_show_cache_miss_notices_change: Rc::new(|_| {}),
            on_collapse_changelog_change: Rc::new(|_| {}),
            on_enable_install_telemetry_change: Rc::new(|_| {}),
            on_double_escape_action_change: Rc::new(|_| {}),
            on_tree_filter_mode_change: Rc::new(|_| {}),
            on_show_hardware_cursor_change: Rc::new(|_| {}),
            on_editor_padding_x_change: Rc::new(|_| {}),
            on_output_pad_change: Rc::new(|_| {}),
            on_autocomplete_max_visible_change: Rc::new(|_| {}),
            on_quiet_startup_change: Rc::new(|_| {}),
            on_default_project_trust_change: Rc::new(|_| {}),
            on_clear_on_shrink_change: Rc::new(|_| {}),
            on_show_terminal_progress_change: Rc::new(|_| {}),
            on_tui_mode_change: Rc::new(|_| {}),
            on_fullscreen_exit_output_change: Rc::new(|_| {}),
            on_fullscreen_scrollbar_change: Rc::new(|_| {}),
            on_fullscreen_copy_on_select_change: Rc::new(|_| {}),
            on_fullscreen_wheel_scroll_lines_change: Rc::new(|_| {}),
            on_warnings_change: Rc::new(|_| {}),
            on_cancel: Rc::new(|| {}),
        }
    }
}

// =============================================================================
// 辅助函数
// =============================================================================

/// `modelSettingKey(model)`。
fn model_setting_key(model: &Model) -> String {
    format!("{}/{}", model.provider, model.id)
}

/// `modelDisplayLabel(model)`：纯文本 `id [provider]`。
fn model_display_label(model: &Model) -> String {
    format!("{} [{}]", model.id, model.provider)
}

/// `modelThinkingOverridesSummary(levels)`。
///
/// 主列表 `model-thinking` 项的显示值：空表为 `none`，否则为按键数量统计的
/// `N configured`。关闭思考等级子菜单时也会用它刷新该显示值。
fn model_thinking_overrides_summary(levels: &IndexMap<String, ModelThinkingLevel>) -> String {
    if levels.is_empty() {
        "none".to_string()
    } else {
        format!("{} configured", levels.len())
    }
}

/// `modelItemLabel(model)`：`id` + 已着色的 `[provider]`。
fn model_item_label(model: &Model) -> String {
    format!("{} {}", model.id, muted(&format!("[{}]", model.provider)))
}

/// `themeItems(themes, currentTheme)`。
///
/// 每个值的 `value` 是主题名本身，`label` 用 `✓ `/两个空格标出当前主题。名为
/// `SYSTEM_THEME_NAME` 的项额外带说明；它是否出现在这里取决于 `available_themes`，
/// 排序调整只发生在 [`single_mode_theme_items`]。
fn theme_items(available_themes: &[String], current_theme: &str) -> Vec<kiana_tui::SelectItem> {
    available_themes
        .iter()
        .map(|name| {
            let label = format!(
                "{}{}",
                if name == current_theme { "✓ " } else { "  " },
                name
            );
            let mut item = kiana_tui::SelectItem::with_label(name.clone(), label);
            if name == SYSTEM_THEME_NAME {
                item = item.with_description("Theme created from your terminal's colors");
            }
            item
        })
        .collect()
}

/// `AUTOMATIC_THEME_VALUE`。
const AUTOMATIC_THEME_VALUE: &str = "/";

/// `singleModeThemeItems(themes, currentTheme)`：system 最前，然后 automatic，再其余主题。
///
/// 具体顺序是「system（若存在）→ 值为 `/` 的 automatic 项 → 其余主题保持原相对顺序」。
/// system 项是从 [`theme_items`] 的结果中按 value 找到并移除的，因此不会重复；列表里
/// 没有 system 时直接以 automatic 开头。
fn single_mode_theme_items(
    available_themes: &[String],
    current_theme: &str,
) -> Vec<kiana_tui::SelectItem> {
    let mut items = theme_items(available_themes, current_theme);
    let system = items
        .iter()
        .position(|item| item.value == SYSTEM_THEME_NAME)
        .map(|index| items.remove(index));
    let mut result = Vec::new();
    if let Some(system) = system {
        result.push(system);
    }
    result.push(kiana_tui::SelectItem {
        value: AUTOMATIC_THEME_VALUE.to_string(),
        label: "  automatic".to_string(),
        description: Some("Use separate themes for light and dark terminal appearance".to_string()),
    });
    result.extend(items);
    result
}

/// `preferredTheme(themes, preferred, fallback)`。
///
/// 选择顺序：`preferred` 在列表中则用它 → `fallback` 在列表中则用它 → 列表第一项 →
/// 都没有时返回 `fallback` 字符串本身（可能并不存在于列表中）。因此列表为空时返回值
/// 只用于预选/显示，不会产生可选中项。
fn preferred_theme(available_themes: &[String], preferred: Option<&str>, fallback: &str) -> String {
    if let Some(preferred) = preferred
        && available_themes.iter().any(|theme| theme == preferred)
    {
        return preferred.to_string();
    }
    if available_themes.iter().any(|theme| theme == fallback) {
        return fallback.to_string();
    }
    available_themes
        .first()
        .cloned()
        .unwrap_or_else(|| fallback.to_string())
}

/// `defaultAutomaticThemes(currentThemeSetting, availableThemes)`。
///
/// 已有自动设置（恰含一个 `/`、两段 trim 后都非空）时直接沿用解析结果。否则以当前单主题名为偏好，
/// 经 [`preferred_theme`] 与 `SYSTEM_THEME_NAME` 兜底，light/dark 取同一个主题——即从
/// 单主题进入自动模式时不会突然换色；当前设置含 `/` 但无法解析时视为没有偏好。
fn default_automatic_themes(current_theme_setting: &str, available_themes: &[String]) -> AutoTheme {
    if let Some(auto_theme) = parse_auto_theme_setting(Some(current_theme_setting)) {
        return auto_theme;
    }
    let current_fixed_theme = if current_theme_setting.contains('/') {
        None
    } else {
        Some(current_theme_setting)
    };
    let name = preferred_theme(available_themes, current_fixed_theme, SYSTEM_THEME_NAME);
    AutoTheme {
        light_theme: name.clone(),
        dark_theme: name,
    }
}

/// `String(config.quietStartup)`。
fn quiet_startup_str(value: QuietStartup) -> &'static str {
    match value {
        QuietStartup::Boolean(true) => "true",
        QuietStartup::Boolean(false) => "false",
        QuietStartup::Header => "header",
    }
}

/// `String(config.doubleEscapeAction)`。
fn double_escape_action_str(value: DoubleEscapeAction) -> &'static str {
    match value {
        DoubleEscapeAction::Fork => "fork",
        DoubleEscapeAction::Tree => "tree",
        DoubleEscapeAction::None => "none",
    }
}

/// `String(config.treeFilterMode)`。
fn tree_filter_mode_str(value: TreeFilterMode) -> &'static str {
    match value {
        TreeFilterMode::Default => "default",
        TreeFilterMode::NoTools => "no-tools",
        TreeFilterMode::UserOnly => "user-only",
        TreeFilterMode::LabeledOnly => "labeled-only",
        TreeFilterMode::All => "all",
    }
}

/// `String(config.tuiMode)`。
fn tui_mode_str(value: TuiMode) -> &'static str {
    match value {
        TuiMode::Regular => "regular",
        TuiMode::Fullscreen => "fullscreen",
    }
}

/// `String(config.fullscreenExitOutput)`。
fn fullscreen_exit_output_str(value: FullscreenExitOutput) -> &'static str {
    match value {
        FullscreenExitOutput::Transcript => "transcript",
        FullscreenExitOutput::ResumeHint => "resume-hint",
    }
}

/// `String(config.fullscreenScrollbar)`。
fn fullscreen_scrollbar_str(value: FullscreenScrollbar) -> &'static str {
    match value {
        FullscreenScrollbar::Auto => "auto",
        FullscreenScrollbar::Always => "always",
        FullscreenScrollbar::Hidden => "hidden",
    }
}

/// `String(config.transport)`。
fn transport_str(value: Transport) -> &'static str {
    match value {
        Transport::Sse => "sse",
        Transport::Websocket => "websocket",
        Transport::WebsocketCached => "websocket-cached",
        Transport::Auto => "auto",
    }
}

/// `String(config.sessionShareDestination)`。
fn session_share_destination_str(value: SessionShareDestination) -> &'static str {
    match value {
        SessionShareDestination::Auto => "auto",
        SessionShareDestination::Gist => "gist",
        SessionShareDestination::Radius => "radius",
    }
}

/// `String(config.steeringMode)`。
fn steering_mode_str(value: SteeringMode) -> &'static str {
    match value {
        SteeringMode::All => "all",
        SteeringMode::OneAtATime => "one-at-a-time",
    }
}

/// `String(config.cacheWarmingMode)`。
fn cache_warming_mode_str(value: CacheWarmingMode) -> &'static str {
    match value {
        CacheWarmingMode::Off => "off",
        CacheWarmingMode::Streaming => "streaming",
        CacheWarmingMode::Idle => "idle",
    }
}

/// `String(config.mermaidRenderingMode)`。
fn mermaid_rendering_mode_str(value: MermaidRenderingMode) -> &'static str {
    match value {
        MermaidRenderingMode::Off => "off",
        MermaidRenderingMode::Final => "final",
        MermaidRenderingMode::Streaming => "streaming",
    }
}

/// kiana-config 的 `FullscreenWheelScrollLines` → kiana-tui 的 `WheelScrollLines`。
fn wheel_lines_from_settings(
    value: kiana_config::settings::FullscreenWheelScrollLines,
) -> WheelScrollLines {
    match value {
        kiana_config::settings::FullscreenWheelScrollLines::Auto => WheelScrollLines::Auto,
        kiana_config::settings::FullscreenWheelScrollLines::Lines(lines) => {
            WheelScrollLines::Fixed(lines)
        }
    }
}

/// `String(config.fullscreenWheelScrollLines)`。
fn wheel_scroll_lines_str(value: WheelScrollLines) -> String {
    match value {
        WheelScrollLines::Auto => "auto".to_string(),
        WheelScrollLines::Fixed(lines) => js_number_string(lines),
    }
}

/// `DEFAULT_PROJECT_TRUST_LABELS` 的标签（`Object.values` 顺序 ask/always/never）。
fn default_project_trust_label(value: DefaultProjectTrust) -> &'static str {
    match value {
        DefaultProjectTrust::Ask => "Ask",
        DefaultProjectTrust::Always => "Always trust",
        DefaultProjectTrust::Never => "Never trust",
    }
}

/// `DEFAULT_PROJECT_TRUST_BY_LABEL` 查表（找不到返回 `None`）。
fn default_project_trust_by_label(label: &str) -> Option<DefaultProjectTrust> {
    match label {
        "Ask" => Some(DefaultProjectTrust::Ask),
        "Always trust" => Some(DefaultProjectTrust::Always),
        "Never trust" => Some(DefaultProjectTrust::Never),
        _ => None,
    }
}

/// `ui.locale` 代码 → 面板显示名。
///
/// 只有内置了语言包的 `zh`（含 `zh-CN` 等变体）显示 `简体中文`；其余（含未知代码）
/// 都显示 `English`——加载器对未知 locale 会回退英文，显示名保持一致。
fn ui_locale_display_name(code: &str) -> &'static str {
    match crate::render_core::i18n::resolve_supported_locale(code) {
        Some("zh") => "简体中文",
        _ => "English",
    }
}

/// 面板显示名 → 语言代码。
fn ui_locale_code(display: &str) -> &'static str {
    match display {
        "简体中文" => "zh",
        _ => "en",
    }
}

// =============================================================================
// WarningSettingsSubmenu
// =============================================================================

/// TS `class WarningSettingsSubmenu extends Container`（模块私有）。
struct WarningSettingsSubmenu {
    settings_list: SettingsList,
}

impl WarningSettingsSubmenu {
    /// 用当前 warnings 构造单项列表。
    ///
    /// `current_value` 优先取 `anthropic_extra_usage`，未设置（`None`）时回落到
    /// `SETTINGS_DEFAULTS` 的同名字段：这正是 TS 的 `??` 语义。内部状态用
    /// `Rc<RefCell<WarningSettings>>` 保存，切换时先改状态、克隆快照、**释放借用**再调
    /// `on_change`，避免宿主回调重入时触发 RefCell 双重借用。
    fn new(
        warnings: WarningSettings,
        on_change: Rc<dyn Fn(&WarningSettings)>,
        on_cancel: Rc<dyn Fn()>,
    ) -> Self {
        let state = Rc::new(RefCell::new(warnings));
        let current_value = if state
            .borrow()
            .anthropic_extra_usage
            .unwrap_or(SETTINGS_DEFAULTS.warnings.anthropic_extra_usage)
        {
            "true"
        } else {
            "false"
        };
        let items = vec![SettingItem {
            id: "anthropic-extra-usage".to_string(),
            label: "Anthropic extra usage".to_string(),
            description: Some(
                "Warn when Anthropic subscription auth may use paid extra usage".to_string(),
            ),
            current_value: current_value.to_string(),
            values: Some(vec!["true".to_string(), "false".to_string()]),
            submenu: None,
        }];
        let change_state = Rc::clone(&state);
        let settings_list = SettingsList::new(
            items,
            1,
            get_settings_list_theme(),
            Box::new(move |id: &str, new_value: &str| {
                if id == "anthropic-extra-usage" {
                    let mut state = change_state.borrow_mut();
                    state.anthropic_extra_usage = Some(new_value == "true");
                    let snapshot = state.clone();
                    drop(state);
                    on_change(&snapshot);
                }
            }),
            Box::new(move || on_cancel()),
            SettingsListOptions::default(),
        );
        WarningSettingsSubmenu { settings_list }
    }
}

impl Component for WarningSettingsSubmenu {
    fn render(&mut self, width: usize) -> Vec<String> {
        self.settings_list.render(width)
    }

    fn handle_input(&mut self, data: &str) {
        self.settings_list.handle_input(data);
    }

    fn handle_mouse(&mut self, event: &TuiMouseEvent) -> Option<TuiMouseEventResult> {
        self.settings_list.handle_mouse(event)
    }

    fn invalidate(&mut self) {
        self.settings_list.invalidate();
    }

    fn for_each_container_child(&self, f: &mut dyn FnMut(&dyn Component) -> bool) {
        let _ = f(&self.settings_list);
    }

    /// TS 里本组件是 `Container`，`SettingsList` 是真实子项；可变遍历必须同样暴露它，
    /// 否则经 `FocusTarget::Path` 送达的键盘输入无法下钻到内部列表（`kiana-tui` 契约）。
    fn for_each_container_child_mut(
        &mut self,
        f: &mut dyn FnMut(&mut (dyn Component + 'static)) -> bool,
    ) {
        let _ = f(&mut self.settings_list);
    }
}

// =============================================================================
// ThemeSubmenu
// =============================================================================

/// 主题子菜单的两种模式，与 TS 的 `"single" | "automatic"` 对应。
///
/// `Single`：一个主题同时用于明暗外观；`Automatic`：分别配置 light/dark 两个主题，
/// 由终端外观决定当前生效者。模式只由用户操作（选中 `/`、Change mode）或初始设置
/// 决定，不会因为终端外观变化在会话内自动切换。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ThemeMode {
    Single,
    Automatic,
}

/// 自动模式下正在编辑的是 light 还是 dark 主题。
#[derive(Debug, Clone, Copy)]
enum ThemeSelectKind {
    Light,
    Dark,
}

/// TS `class AutomaticMenu`（TS 里是内联 Container；Rust 侧显式建模以便共享 SettingsList）。
///
/// 渲染顺序固定为 title / 空行 / line1 / line2 / 空行 / 设置列表；`settings_list` 同时被
/// `ThemeSubmenuState.input` 引用，所以用 [`Rc`] 共享。line1/line2 是静态说明，会随宽度
/// 换行，鼠标命中计算必须逐项渲染后再累加行数。
struct AutomaticMenu {
    title: Text,
    line1: Text,
    line2: Text,
    settings_list: Rc<RefCell<SettingsList>>,
}

impl Component for AutomaticMenu {
    fn render(&mut self, width: usize) -> Vec<String> {
        let mut lines = Vec::new();
        lines.extend(self.title.render(width));
        lines.extend(Spacer::new().render(width));
        lines.extend(self.line1.render(width));
        lines.extend(self.line2.render(width));
        lines.extend(Spacer::new().render(width));
        lines.extend(self.settings_list.borrow_mut().render(width));
        lines
    }

    fn handle_input(&mut self, data: &str) {
        self.settings_list.borrow_mut().handle_input(data);
    }

    /// TS 里 `showAutomaticMenu` 的 `content` 是真正的 `Container`：
    /// `Container.handleMouse` 用**实际渲染高度**（`child.render(width).length`）逐个子项
    /// 计算纵向偏移，命中 SettingsList 时把 `y`/`height` 换算成列表局部坐标。这里必须
    /// 逐项测量，不能用固定行数——标题/说明文本在窄宽度下会换行，固定偏移会把点击
    /// 映射到错误的行。
    fn handle_mouse(&mut self, event: &TuiMouseEvent) -> Option<TuiMouseEventResult> {
        if event.y < 0 {
            return None;
        }
        let width = event.width;
        // children: title, Spacer(1), line1, line2, Spacer(1), settingsList。
        let mut offset = self.title.render(width).len() as isize;
        offset += Spacer::new().render(width).len() as isize;
        offset += self.line1.render(width).len() as isize;
        offset += self.line2.render(width).len() as isize;
        offset += Spacer::new().render(width).len() as isize;

        let mut settings_list = self.settings_list.borrow_mut();
        let height = settings_list.render(width).len() as isize;
        // TS `Container.handleMouse`：`event.y >= event.height` 直接返回 undefined；
        // 上级 Container 传入的 height 就是本容器渲染出的总行数。
        if event.y >= offset + height {
            return None;
        }
        if event.y < offset {
            return None;
        }
        let mut local = event.clone();
        local.y = event.y - offset;
        local.height = height.max(0) as usize;
        settings_list.handle_mouse(&local)
    }

    fn invalidate(&mut self) {
        self.title.invalidate();
        self.line1.invalidate();
        self.line2.invalidate();
        self.settings_list.borrow_mut().invalidate();
    }

    fn for_each_container_child(&self, f: &mut dyn FnMut(&dyn Component) -> bool) {
        if !f(&self.title) {
            return;
        }
        if !f(&self.line1) {
            return;
        }
        if !f(&self.line2) {
            return;
        }
        let settings_list = self.settings_list.borrow();
        let _ = f(&*settings_list);
    }

    fn for_each_container_child_mut(
        &mut self,
        f: &mut dyn FnMut(&mut (dyn Component + 'static)) -> bool,
    ) {
        if !f(&mut self.title) {
            return;
        }
        if !f(&mut self.line1) {
            return;
        }
        if !f(&mut self.line2) {
            return;
        }
        let mut settings_list = self.settings_list.borrow_mut();
        let _ = f(&mut *settings_list);
    }
}

/// 当前模式下用于渲染的组件。
///
/// 与 [`ThemeInput`] 分开保存，是因为 TS 的 `setContent(render, input = render)` 允许
/// 两者不同：自动模式下渲染整个 [`AutomaticMenu`]，但按键目标只是其中的
/// [`SettingsList`]（标题行不接收输入）。
#[derive(Clone)]
enum ThemeContent {
    Single(Rc<RefCell<Box<dyn Component>>>),
    Automatic(Rc<RefCell<AutomaticMenu>>),
}

/// 当前模式下按键与鼠标事件的目标。
///
/// 单模式交给 [`SelectSubmenu`]；自动模式只交给内层 [`SettingsList`]，避免事件落在
/// 标题/说明行上。
#[derive(Clone)]
enum ThemeInput {
    Submenu(Rc<RefCell<Box<dyn Component>>>),
    SettingsList(Rc<RefCell<SettingsList>>),
}

/// `ThemeSubmenu` 的全部可变状态（与各回调闭包共享）。
///
/// 不变量：`content` 与 `input` 必须属于同一个 `mode`；两者在构造时先放占位 `Spacer`，
/// 随后立刻由 `theme_show_single_menu` / `theme_show_automatic_menu` 成对替换。`single_theme`
/// 只在 `Single` 模式有意义，`light_theme`/`dark_theme` 只在 `Automatic` 模式有意义；
/// 切换模式不会丢弃另一组值，方便来回切换。
struct ThemeSubmenuState {
    callbacks: Rc<SettingsCallbacks>,
    available_themes: Vec<String>,
    terminal_theme: TerminalColorScheme,
    on_done: Rc<dyn Fn(Option<String>)>,
    original_theme_setting: String,
    mode: ThemeMode,
    single_theme: String,
    light_theme: String,
    dark_theme: String,
    content: ThemeContent,
    input: ThemeInput,
}

impl ThemeSubmenuState {
    /// 当前模式的设置字符串：自动模式是 `light/dark`，单模式是主题名。
    ///
    /// 这是“回传值”的形态；预览单模式选中项时用主题名，预览自动设置时才用完整字符串。
    fn get_theme_setting(&self) -> String {
        if self.mode == ThemeMode::Automatic {
            self.get_automatic_theme_setting()
        } else {
            self.single_theme.clone()
        }
    }

    /// 自动模式下按终端外观取当前实际生效的主题名。
    ///
    /// `terminal_theme` 是打开面板时的快照，组件不会重新查询终端，因此面板打开期间
    /// 终端明暗变化不会反映到这里。
    fn get_active_automatic_theme(&self) -> String {
        if self.terminal_theme == TerminalColorScheme::Light {
            self.light_theme.clone()
        } else {
            self.dark_theme.clone()
        }
    }

    /// 自动模式的回传形态 `light/dark`（固定顺序：light 在前）。
    fn get_automatic_theme_setting(&self) -> String {
        format!("{}/{}", self.light_theme, self.dark_theme)
    }
}

/// TS `class ThemeSubmenu extends Container`（模块私有）。
///
/// 只有一个共享状态字段；渲染/输入/鼠标全部按 `state.content` / `state.input` 转发，
/// 因此模式切换后无需重建本组件。
struct ThemeSubmenu {
    state: Rc<RefCell<ThemeSubmenuState>>,
}

impl ThemeSubmenu {
    /// `constructor(currentThemeSetting, terminalTheme, availableThemes, callbacks, onDone)`。
    ///
    /// 关键步骤：
    /// 1. `parse_auto_theme_setting` 能解析 → 初始 `Automatic`；否则 `Single`。
    /// 2. 自动模式的 light/dark 初值经 [`default_automatic_themes`] 从当前设置推导；
    ///    单模式的初值经 [`preferred_theme`] 依次尝试「固定主题名 → 当前活跃自动主题 →
    ///    system → 列表首项」，全都不可用时仍返回 system 名作为预选值（可能不在列表里）。
    /// 3. 先放入 `Spacer` 占位，再调用对应的 `theme_show_*_menu` 填上真实内容，保证
    ///    构造结束时 `content`/`input` 已就绪。
    fn new(
        current_theme_setting: String,
        terminal_theme: TerminalColorScheme,
        available_themes: Vec<String>,
        callbacks: Rc<SettingsCallbacks>,
        on_done: Rc<dyn Fn(Option<String>)>,
    ) -> Self {
        let auto_theme = parse_auto_theme_setting(Some(&current_theme_setting));
        let automatic_themes = default_automatic_themes(&current_theme_setting, &available_themes);
        let fixed_theme = if auto_theme.is_some() || current_theme_setting.contains('/') {
            None
        } else {
            Some(current_theme_setting.clone())
        };
        let initial_mode = if auto_theme.is_some() {
            ThemeMode::Automatic
        } else {
            ThemeMode::Single
        };
        let light_theme = automatic_themes.light_theme;
        let dark_theme = automatic_themes.dark_theme;
        let active_automatic = if terminal_theme == TerminalColorScheme::Light {
            light_theme.clone()
        } else {
            dark_theme.clone()
        };
        let preferred = fixed_theme.or_else(|| {
            if auto_theme.is_some() {
                Some(active_automatic)
            } else {
                None
            }
        });
        let single_theme =
            preferred_theme(&available_themes, preferred.as_deref(), SYSTEM_THEME_NAME);

        let placeholder = || Rc::new(RefCell::new(Box::new(Spacer::new()) as Box<dyn Component>));
        let state = Rc::new(RefCell::new(ThemeSubmenuState {
            callbacks,
            available_themes,
            terminal_theme,
            on_done,
            original_theme_setting: current_theme_setting,
            mode: initial_mode,
            single_theme,
            light_theme,
            dark_theme,
            content: ThemeContent::Single(placeholder()),
            input: ThemeInput::Submenu(placeholder()),
        }));
        if initial_mode == ThemeMode::Automatic {
            theme_show_automatic_menu(&state);
        } else {
            theme_show_single_menu(&state);
        }
        ThemeSubmenu { state }
    }
}

/// `apply(themeSetting)` → `onDone(themeSetting)`。
///
/// `Some(setting)` 会被 `SettingsList::done` 写回 `theme` 条目的当前值，并据 id 触发宿主的
/// `on_theme_change`；本函数只负责把值传出去。
fn theme_apply(state: &Rc<RefCell<ThemeSubmenuState>>, setting: String) {
    let on_done = Rc::clone(&state.borrow().on_done);
    on_done(Some(setting));
}

/// `cancel()`：先恢复原始主题预览，再 `onDone()`（无参数）。
///
/// 先取原始设置与回调、释放 `state` 借用，再调用可能重入的用户回调。`on_done(None)`
/// 只关闭子菜单，不写 `theme` 条目值，所以浏览过程中的选择不会回传；
/// 若 `on_theme_preview` 为 `None`，恢复预览一步被跳过。
fn theme_cancel(state: &Rc<RefCell<ThemeSubmenuState>>) {
    let (original, callbacks, on_done) = {
        let state = state.borrow();
        (
            state.original_theme_setting.clone(),
            Rc::clone(&state.callbacks),
            Rc::clone(&state.on_done),
        )
    };
    if let Some(preview) = &callbacks.on_theme_preview {
        preview(&original);
    }
    on_done(None);
}

/// `showSingleMenu()`。
///
/// 把 `mode` 置为 `Single`，并用一个 [`SelectSubmenu`] 同时替换 `content` 与 `input`。
/// 回调分工：
/// * 选中 `/`（automatic）：只切换模式并预览自动设置字符串，然后重建为自动菜单，
///   不调用 `on_done`，因此不写回值；用户还需在自动菜单里 Apply。
/// * 选中具体主题：写回 `single_theme` 后经 `theme_apply` 回传值并关闭。
/// * 光标移动：只预览主题名，不改状态（保持已保存主题的 ✓ 标记）。
/// * 取消：`theme_cancel` 恢复原始主题预览并关闭。
fn theme_show_single_menu(state: &Rc<RefCell<ThemeSubmenuState>>) {
    state.borrow_mut().mode = ThemeMode::Single;
    let (available_themes, single_theme) = {
        let state = state.borrow();
        (state.available_themes.clone(), state.single_theme.clone())
    };

    let select_state = Rc::clone(state);
    let on_select: SelectValueCallback = Rc::new(move |value: &str| {
        // 选中 automatic 只切模式：先写 mode，再用 get_theme_setting 得到完整的
        // light/dark 预览串（此时 light/dark 仍沿用构造时的推导结果）。
        if value == AUTOMATIC_THEME_VALUE {
            let setting = {
                let mut state = select_state.borrow_mut();
                state.mode = ThemeMode::Automatic;
                state.get_theme_setting()
            };
            let callbacks = Rc::clone(&select_state.borrow().callbacks);
            if let Some(preview) = &callbacks.on_theme_preview {
                preview(&setting);
            }
            theme_show_automatic_menu(&select_state);
            return;
        }
        select_state.borrow_mut().single_theme = value.to_string();
        theme_apply(&select_state, value.to_string());
    });
    let cancel_state = Rc::clone(state);
    let on_cancel: VoidCallback = Rc::new(move || theme_cancel(&cancel_state));
    let change_state = Rc::clone(state);
    // 光标移动只预览：automatic 项预览完整的自动设置，其余预览主题名。
    let on_selection_change: SelectValueCallback = Rc::new(move |value: &str| {
        let setting = if value == AUTOMATIC_THEME_VALUE {
            change_state.borrow().get_automatic_theme_setting()
        } else {
            value.to_string()
        };
        let callbacks = Rc::clone(&change_state.borrow().callbacks);
        if let Some(preview) = &callbacks.on_theme_preview {
            preview(&setting);
        }
    });

    let menu = SelectSubmenu::with_callbacks(
        "Theme",
        "Select a theme, or choose automatic to follow terminal appearance.",
        single_mode_theme_items(&available_themes, &single_theme),
        &single_theme,
        on_select,
        on_cancel,
        Some(on_selection_change),
        SelectSubmenuOptions::default(),
    );
    let menu: Rc<RefCell<Box<dyn Component>>> =
        Rc::new(RefCell::new(Box::new(menu) as Box<dyn Component>));
    let mut state = state.borrow_mut();
    state.content = ThemeContent::Single(Rc::clone(&menu));
    state.input = ThemeInput::Submenu(menu);
}

/// `createThemeSelect(title, description, currentValue, done, onSelect)`。
///
/// 为自动模式的 Light/Dark 两个条目创建子菜单。它与普通 `SelectSubmenu` 的差别在于
/// 回调都要先更新共享状态：
/// * 选中：写入 `light_theme`/`dark_theme` → 预览**合成后**的 `light/dark` 设置（不是
///   单个主题名）→ `done(Some(value))` 关闭子菜单，并把值写回 `light-theme`/`dark-theme`
///   条目的 current_value（自动菜单的 `on_change` 对这两个 id 无分支，不会触发宿主回调）。
/// * 取消：预览当前合成设置后 `done(None, None)`，只关闭、不写值。
/// * 光标移动：只预览单个主题名，便于直接看到该主题效果。
fn theme_create_select(
    state: &Rc<RefCell<ThemeSubmenuState>>,
    kind: ThemeSelectKind,
    title: &str,
    description: &str,
    current_value: &str,
    done: SettingsDoneCallback,
) -> Box<dyn Component> {
    let available_themes = state.borrow().available_themes.clone();
    let items = theme_items(&available_themes, current_value);
    let done = Rc::new(RefCell::new(done));

    let select_state = Rc::clone(state);
    let select_done = Rc::clone(&done);
    let on_select: SelectValueCallback = Rc::new(move |value: &str| {
        let setting = {
            let mut state = select_state.borrow_mut();
            match kind {
                ThemeSelectKind::Light => state.light_theme = value.to_string(),
                ThemeSelectKind::Dark => state.dark_theme = value.to_string(),
            }
            state.get_theme_setting()
        };
        let callbacks = Rc::clone(&select_state.borrow().callbacks);
        if let Some(preview) = &callbacks.on_theme_preview {
            preview(&setting);
        }
        (select_done.borrow_mut())(Some(value.to_string()), None);
    });

    let cancel_state = Rc::clone(state);
    let cancel_done = Rc::clone(&done);
    let on_cancel: VoidCallback = Rc::new(move || {
        let setting = cancel_state.borrow().get_theme_setting();
        let callbacks = Rc::clone(&cancel_state.borrow().callbacks);
        if let Some(preview) = &callbacks.on_theme_preview {
            preview(&setting);
        }
        (cancel_done.borrow_mut())(None, None);
    });

    let change_state = Rc::clone(state);
    let on_selection_change: SelectValueCallback = Rc::new(move |value: &str| {
        let callbacks = Rc::clone(&change_state.borrow().callbacks);
        if let Some(preview) = &callbacks.on_theme_preview {
            preview(value);
        }
    });

    Box::new(SelectSubmenu::with_callbacks(
        title,
        description,
        items,
        current_value,
        on_select,
        on_cancel,
        Some(on_selection_change),
        SelectSubmenuOptions::default(),
    ))
}

/// `showAutomaticMenu()`。
///
/// 构造自动模式面板：两个主题选择子菜单、`Apply`、`Change mode`。`SettingsList` 的
/// `on_change` 只处理后两个 id：
/// * `Change mode`：把 `single_theme` 设为当前终端外观下生效的主题并预览，然后切回
///   单主题菜单；本步不写回设置，用户仍需在单主题菜单里选中具体主题（或返回自动菜单
///   Apply）才会回传值。
/// * `Apply`：把 `light/dark` 字符串交给 `theme_apply` 回传并关闭。
///
/// light/dark 两个子菜单的选中值由各自的工厂直接写入共享状态；`SettingsList` 只看到
/// 条目值变化，其 `on_change` 对这两个 id 无操作。
fn theme_show_automatic_menu(state: &Rc<RefCell<ThemeSubmenuState>>) {
    state.borrow_mut().mode = ThemeMode::Automatic;
    let (light_theme, dark_theme) = {
        let state = state.borrow();
        (state.light_theme.clone(), state.dark_theme.clone())
    };

    let light_factory_state = Rc::clone(state);
    let light_submenu: SubmenuFactory =
        Rc::new(move |current_value: &str, done: SettingsDoneCallback| {
            theme_create_select(
                &light_factory_state,
                ThemeSelectKind::Light,
                "Light Theme",
                "Select the theme to use for light terminal appearance",
                current_value,
                done,
            )
        });
    let dark_factory_state = Rc::clone(state);
    let dark_submenu: SubmenuFactory =
        Rc::new(move |current_value: &str, done: SettingsDoneCallback| {
            theme_create_select(
                &dark_factory_state,
                ThemeSelectKind::Dark,
                "Dark Theme",
                "Select the theme to use for dark terminal appearance",
                current_value,
                done,
            )
        });

    let items = vec![
        SettingItem {
            id: "light-theme".to_string(),
            label: "Light theme".to_string(),
            description: Some(
                "Theme to use in automatic mode when the terminal is light".to_string(),
            ),
            current_value: light_theme,
            values: None,
            submenu: Some(light_submenu),
        },
        SettingItem {
            id: "dark-theme".to_string(),
            label: "Dark theme".to_string(),
            description: Some(
                "Theme to use in automatic mode when the terminal is dark".to_string(),
            ),
            current_value: dark_theme,
            values: None,
            submenu: Some(dark_submenu),
        },
        SettingItem {
            id: "apply".to_string(),
            label: "Apply".to_string(),
            description: Some("Save and go back".to_string()),
            current_value: "save and go back".to_string(),
            values: Some(vec!["save and go back".to_string()]),
            submenu: None,
        },
        SettingItem {
            id: "single-mode".to_string(),
            label: "Change mode".to_string(),
            description: Some("Switch to one theme for light and dark".to_string()),
            current_value: "switch to single theme".to_string(),
            values: Some(vec!["switch to single theme".to_string()]),
            submenu: None,
        },
    ];

    let change_state = Rc::clone(state);
    let cancel_state = Rc::clone(state);
    let settings_list = SettingsList::new(
        items,
        4,
        get_settings_list_theme(),
        Box::new(move |id: &str, _new_value: &str| match id {
            "single-mode" => {
                let (single_theme, callbacks) = {
                    let mut state = change_state.borrow_mut();
                    state.mode = ThemeMode::Single;
                    state.single_theme = state.get_active_automatic_theme();
                    (state.single_theme.clone(), Rc::clone(&state.callbacks))
                };
                if let Some(preview) = &callbacks.on_theme_preview {
                    preview(&single_theme);
                }
                theme_show_single_menu(&change_state);
            }
            "apply" => {
                let setting = change_state.borrow().get_automatic_theme_setting();
                theme_apply(&change_state, setting);
            }
            _ => {}
        }),
        Box::new(move || theme_cancel(&cancel_state)),
        SettingsListOptions::default(),
    );

    let settings_list = Rc::new(RefCell::new(settings_list));
    let menu = AutomaticMenu {
        title: Text::with_padding(bold_accent("Automatic Theme"), 0, 0),
        line1: Text::with_padding(
            muted("Choose themes for terminal light and dark appearance."),
            0,
            0,
        ),
        line2: Text::with_padding(
            muted("Light/dark detection requires terminal support."),
            0,
            0,
        ),
        settings_list: Rc::clone(&settings_list),
    };
    let menu = Rc::new(RefCell::new(menu));
    let mut state = state.borrow_mut();
    state.content = ThemeContent::Automatic(menu);
    state.input = ThemeInput::SettingsList(settings_list);
}

// 所有方法都先克隆 `content`/`input` 再释放 `state` 借用，之后才调用子组件：
// 子菜单的回调会在自己的 `handle_input` 里重入 `state`（选中 `/` 与 Change mode 会调用
// `theme_show_*_menu` 里的 `borrow_mut()`；Apply / 选中主题 / 取消也会经 `theme_apply`、
// `theme_cancel` 借用 state 取 `on_done`）。若在这里持有借用，就会在同一线程触发
// RefCell 双重借用 panic。克隆出来的 `Rc` 同时保证被替换掉的旧组件在本次调用返回前
// 仍然存活。
impl Component for ThemeSubmenu {
    fn render(&mut self, width: usize) -> Vec<String> {
        let content = self.state.borrow().content.clone();
        match content {
            ThemeContent::Single(menu) => menu.borrow_mut().render(width),
            ThemeContent::Automatic(menu) => menu.borrow_mut().render(width),
        }
    }

    fn handle_input(&mut self, data: &str) {
        let input = self.state.borrow().input.clone();
        match input {
            ThemeInput::Submenu(menu) => menu.borrow_mut().handle_input(data),
            ThemeInput::SettingsList(list) => list.borrow_mut().handle_input(data),
        }
    }

    fn handle_mouse(&mut self, event: &TuiMouseEvent) -> Option<TuiMouseEventResult> {
        let content = self.state.borrow().content.clone();
        match content {
            ThemeContent::Single(menu) => menu.borrow_mut().handle_mouse(event),
            ThemeContent::Automatic(menu) => menu.borrow_mut().handle_mouse(event),
        }
    }

    fn invalidate(&mut self) {
        let content = self.state.borrow().content.clone();
        match content {
            ThemeContent::Single(menu) => menu.borrow_mut().invalidate(),
            ThemeContent::Automatic(menu) => menu.borrow_mut().invalidate(),
        }
    }

    fn for_each_container_child(&self, f: &mut dyn FnMut(&dyn Component) -> bool) {
        let content = self.state.borrow().content.clone();
        match content {
            ThemeContent::Single(menu) => {
                let menu = menu.borrow();
                let _ = f(menu.as_ref());
            }
            ThemeContent::Automatic(menu) => {
                let menu = menu.borrow();
                let _ = f(&*menu);
            }
        }
    }

    fn for_each_container_child_mut(
        &mut self,
        f: &mut dyn FnMut(&mut (dyn Component + 'static)) -> bool,
    ) {
        let content = self.state.borrow().content.clone();
        match content {
            ThemeContent::Single(menu) => {
                let mut menu = menu.borrow_mut();
                let _ = f(menu.as_mut());
            }
            ThemeContent::Automatic(menu) => {
                let mut menu = menu.borrow_mut();
                let _ = f(&mut *menu);
            }
        }
    }
}

// =============================================================================
// SettingsSelectorComponent
// =============================================================================

/// 主设置选择器（TS `class SettingsSelectorComponent extends Container`）。
///
/// 内部只有一个 [`SettingsList`]；本结构额外负责渲染上下两条动态边框、把鼠标坐标换算
/// 到列表局部空间，并暴露内部列表供宿主聚焦。列表在整个组件生命周期内不重建，搜索框、
/// 选中项与当前打开的子菜单都保存在它内部。
pub struct SettingsSelectorComponent {
    settings_list: SettingsList,
}

impl SettingsSelectorComponent {
    /// `constructor(config, callbacks)`。
    ///
    /// 按 TS 的顺序构造条目并建立 id → 回调的映射；构造过程不调用任何回调（回调只在
    /// 用户交互时触发）。唯一的对外交互是读取 kiana-tui 的线程级能力缓存
    /// [`get_capabilities`]：缓存冷且无 `KIANA_HYPERLINKS` 覆盖时，该调用会执行一次
    /// `tmux display-message` 探针（250 ms 超时）；实际宿主在 TUI 启动阶段已预热缓存，
    /// 因此交互路径上通常只是缓存命中。`config` 与 `callbacks` 都放进 [`Rc`]，
    /// 因为三个 submenu 工厂需要在离开构造函数后长期读取它们。
    ///
    /// 条目顺序是外部契约（`tests/components_settings_selector.rs` 的黄金渲染逐行比对）：
    /// 终端图片能力只决定 `show-images` / `image-width-cells` 是否插入，其余项依赖
    /// `insert_after` 的级联定位；不要为了“整理”改成一次性静态数组。
    pub fn new(config: SettingsConfig, callbacks: SettingsCallbacks) -> Self {
        let callbacks = Rc::new(callbacks);
        let config = Rc::new(config);
        // 终端能力决定图片相关项是否存在。能力来自 kiana-tui 的线程级配置
        // （测试里显式设为 images: None），本次构造只读一次。
        let supports_images = get_capabilities().images.is_some();
        // 键位文案在构造开头求值并嵌入 description：面板打开期间改键位不会刷新文案。
        let follow_up_key = key_display_text("app.message.followUp");
        let cycle_thinking_key = key_display_text("app.thinking.cycle");
        // 两份可变的本地副本：子菜单关闭后主列表的显示值、以及再次打开子菜单时的
        // 输入都来自这里；宿主 config 快照保持不动。
        let current_warnings = Rc::new(RefCell::new(config.warnings.clone()));
        let current_model_thinking_levels =
            Rc::new(RefCell::new(config.model_thinking_levels.clone()));
        // provider/id → Model 的索引，供思考等级子菜单把第一步的选择还原成模型信息。
        let default_model_by_value: Rc<HashMap<String, Model>> = Rc::new(
            config
                .available_default_models
                .iter()
                .map(|model| (model_setting_key(model), model.clone()))
                .collect(),
        );
        // 默认模型只在可用列表里时才算数；"not set" 等占位值不会被当作预选目标。
        let current_default_model_key =
            if default_model_by_value.contains_key(&config.default_model) {
                Some(config.default_model.clone())
            } else {
                None
            };
        // 当前模型的 key 只用于在 available_default_models 内部做排序比较；若它不在
        // 该列表中，比较永远不会命中，排序不受影响，第一步的预选也会因找不到同名 value
        // 而保持默认下标 0。
        let current_model_key = config.current_model.as_ref().map(model_setting_key);

        // 基础数组（25 项，顺序是稳定契约，改动需同步黄金渲染测试）。
        let mut items: Vec<SettingItem> = vec![
            SettingItem {
                id: "autocompact".to_string(),
                label: "Auto-compact".to_string(),
                description: Some("Automatically compact context when it gets too large".to_string()),
                current_value: if config.auto_compact { "true" } else { "false" }.to_string(),
                values: Some(vec!["true".to_string(), "false".to_string()]),
                submenu: None,
            },
            SettingItem {
                id: "steering-mode".to_string(),
                label: "Steering mode".to_string(),
                description: Some(
                    "Enter while streaming queues steering messages. 'one-at-a-time': deliver one, wait for response. 'all': deliver all at once."
                        .to_string(),
                ),
                current_value: steering_mode_str(config.steering_mode).to_string(),
                values: Some(vec!["one-at-a-time".to_string(), "all".to_string()]),
                submenu: None,
            },
            SettingItem {
                id: "follow-up-mode".to_string(),
                label: "Follow-up mode".to_string(),
                description: Some(format!(
                    "{follow_up_key} queues follow-up messages until agent stops. 'one-at-a-time': deliver one, wait for response. 'all': deliver all at once."
                )),
                current_value: steering_mode_str(config.follow_up_mode).to_string(),
                values: Some(vec!["one-at-a-time".to_string(), "all".to_string()]),
                submenu: None,
            },
            SettingItem {
                id: "transport".to_string(),
                label: "Transport".to_string(),
                description: Some(
                    "Preferred transport for providers that support multiple transports"
                        .to_string(),
                ),
                current_value: transport_str(config.transport).to_string(),
                values: Some(vec![
                    "sse".to_string(),
                    "websocket".to_string(),
                    "websocket-cached".to_string(),
                    "auto".to_string(),
                ]),
                submenu: None,
            },
            SettingItem {
                id: "share-destination".to_string(),
                label: "Session share destination".to_string(),
                description: Some(
                    "Where to upload sessions with /share (auto prefers Radius when authenticated, otherwise GitHub Gist)"
                        .to_string(),
                ),
                current_value: session_share_destination_str(config.session_share_destination)
                    .to_string(),
                values: Some(vec![
                    "auto".to_string(),
                    "gist".to_string(),
                    "radius".to_string(),
                ]),
                submenu: None,
            },
            SettingItem {
                id: "http-idle-timeout".to_string(),
                label: "HTTP idle timeout".to_string(),
                description: Some(
                    "Maximum idle gap while waiting for HTTP headers or body chunks. Disable for local models that pause longer than five minutes."
                        .to_string(),
                ),
                current_value: format_http_idle_timeout_ms(config.http_idle_timeout_ms),
                values: Some(
                    HTTP_IDLE_TIMEOUT_CHOICES
                        .iter()
                        .map(|choice| choice.label.to_string())
                        .collect(),
                ),
                submenu: None,
            },
            SettingItem {
                id: "cache-warming-mode".to_string(),
                label: "Cache warming".to_string(),
                description: Some(
                    "off; streaming while the agent runs; idle also between runs while continuation stays profitable"
                        .to_string(),
                ),
                current_value: cache_warming_mode_str(config.cache_warming_mode).to_string(),
                values: Some(
                    CACHE_WARMING_MODES
                        .iter()
                        .map(|mode| cache_warming_mode_str(*mode).to_string())
                        .collect(),
                ),
                submenu: None,
            },
            SettingItem {
                id: "hide-thinking".to_string(),
                label: "Hide thinking".to_string(),
                description: Some("Hide thinking blocks in assistant responses".to_string()),
                current_value: if config.hide_thinking_block {
                    "true"
                } else {
                    "false"
                }
                .to_string(),
                values: Some(vec!["true".to_string(), "false".to_string()]),
                submenu: None,
            },
            SettingItem {
                id: "mermaid-rendering".to_string(),
                label: "Mermaid diagrams".to_string(),
                description: Some("Render Mermaid code blocks as Unicode diagrams".to_string()),
                current_value: mermaid_rendering_mode_str(config.mermaid_rendering_mode)
                    .to_string(),
                values: Some(vec![
                    "off".to_string(),
                    "final".to_string(),
                    "streaming".to_string(),
                ]),
                submenu: None,
            },
            SettingItem {
                id: "cache-miss-notices".to_string(),
                label: "Cache miss notices".to_string(),
                description: Some(
                    "Show transcript notices for cache costs and provider recovery diagnostics"
                        .to_string(),
                ),
                current_value: if config.show_cache_miss_notices {
                    "true"
                } else {
                    "false"
                }
                .to_string(),
                values: Some(vec!["true".to_string(), "false".to_string()]),
                submenu: None,
            },
            SettingItem {
                id: "collapse-changelog".to_string(),
                label: "Collapse changelog".to_string(),
                description: Some("Show condensed changelog after updates".to_string()),
                current_value: if config.collapse_changelog {
                    "true"
                } else {
                    "false"
                }
                .to_string(),
                values: Some(vec!["true".to_string(), "false".to_string()]),
                submenu: None,
            },
            SettingItem {
                id: "quiet-startup".to_string(),
                label: "Quiet startup".to_string(),
                description: Some(
                    "Disable verbose printing at startup (header: keep only the startup header)"
                        .to_string(),
                ),
                current_value: quiet_startup_str(config.quiet_startup).to_string(),
                values: Some(vec![
                    "true".to_string(),
                    "header".to_string(),
                    "false".to_string(),
                ]),
                submenu: None,
            },
            SettingItem {
                id: "install-telemetry".to_string(),
                label: "Install telemetry".to_string(),
                description: Some(
                    "Send an anonymous version/update ping after changelog-detected updates"
                        .to_string(),
                ),
                current_value: if config.enable_install_telemetry {
                    "true"
                } else {
                    "false"
                }
                .to_string(),
                values: Some(vec!["true".to_string(), "false".to_string()]),
                submenu: None,
            },
            SettingItem {
                id: "default-project-trust".to_string(),
                label: "Default project trust".to_string(),
                description: Some(
                    "Fallback behavior when no extension or saved trust decision decides project trust"
                        .to_string(),
                ),
                current_value: default_project_trust_label(config.default_project_trust)
                    .to_string(),
                values: Some(vec![
                    "Ask".to_string(),
                    "Always trust".to_string(),
                    "Never trust".to_string(),
                ]),
                submenu: None,
            },
            SettingItem {
                id: "double-escape-action".to_string(),
                label: "Double-escape action".to_string(),
                description: Some("Action when pressing Escape twice with empty editor".to_string()),
                current_value: double_escape_action_str(config.double_escape_action).to_string(),
                values: Some(vec![
                    "tree".to_string(),
                    "fork".to_string(),
                    "none".to_string(),
                ]),
                submenu: None,
            },
            SettingItem {
                id: "tree-filter-mode".to_string(),
                label: "Tree filter mode".to_string(),
                description: Some("Default filter when opening /tree".to_string()),
                current_value: tree_filter_mode_str(config.tree_filter_mode).to_string(),
                values: Some(vec![
                    "default".to_string(),
                    "no-tools".to_string(),
                    "user-only".to_string(),
                    "labeled-only".to_string(),
                    "all".to_string(),
                ]),
                submenu: None,
            },
            Self::warnings_item(
                current_warnings.clone(),
                Rc::clone(&callbacks),
            ),
            Self::model_thinking_item(
                Rc::clone(&config),
                Rc::clone(&callbacks),
                Rc::clone(&default_model_by_value),
                Rc::clone(&current_model_thinking_levels),
                current_model_key.clone(),
                current_default_model_key.clone(),
                &cycle_thinking_key,
            ),
            SettingItem {
                id: "tui-mode".to_string(),
                label: "TUI mode".to_string(),
                description: Some(
                    "Interface layout; regular mode uses the terminal's normal scrollback"
                        .to_string(),
                ),
                current_value: tui_mode_str(config.tui_mode).to_string(),
                values: Some(vec!["regular".to_string(), "fullscreen".to_string()]),
                submenu: None,
            },
            SettingItem {
                id: "fullscreen-exit-output".to_string(),
                label: "Fullscreen exit output".to_string(),
                description: Some(
                    "Print the transcript or only a session resume hint when exiting fullscreen mode"
                        .to_string(),
                ),
                current_value: fullscreen_exit_output_str(config.fullscreen_exit_output)
                    .to_string(),
                values: Some(vec![
                    "transcript".to_string(),
                    "resume-hint".to_string(),
                ]),
                submenu: None,
            },
            SettingItem {
                id: "fullscreen-scrollbar".to_string(),
                label: "Fullscreen scrollbar".to_string(),
                description: Some(
                    "Scrollbar behavior in fullscreen mode; has no effect in regular mode"
                        .to_string(),
                ),
                current_value: fullscreen_scrollbar_str(config.fullscreen_scrollbar).to_string(),
                values: Some(vec![
                    "auto".to_string(),
                    "always".to_string(),
                    "hidden".to_string(),
                ]),
                submenu: None,
            },
            SettingItem {
                id: "fullscreen-copy-on-select".to_string(),
                label: "Fullscreen copy on select".to_string(),
                description: Some(
                    "Automatically copy selected text in fullscreen mode; disable to copy selections with Ctrl+X"
                        .to_string(),
                ),
                current_value: if config.fullscreen_copy_on_select {
                    "true"
                } else {
                    "false"
                }
                .to_string(),
                values: Some(vec!["true".to_string(), "false".to_string()]),
                submenu: None,
            },
            SettingItem {
                id: "fullscreen-wheel-scroll-lines".to_string(),
                label: "Fullscreen wheel scrolling".to_string(),
                description: Some(
                    "Lines per mouse-wheel event in fullscreen mode; 'auto' speeds up fast wheel spins where the terminal does not"
                        .to_string(),
                ),
                current_value: wheel_scroll_lines_str(config.fullscreen_wheel_scroll_lines),
                values: Some(fullscreen_wheel_values(config.fullscreen_wheel_scroll_lines)),
                submenu: None,
            },
            Self::theme_item(Rc::clone(&config), Rc::clone(&callbacks)),
            SettingItem {
                id: "ui-locale".to_string(),
                label: "Language".to_string(),
                description: Some(
                    "Language for the interface (UI only; model answers keep their own language)"
                        .to_string(),
                ),
                current_value: ui_locale_display_name(&config.ui_locale).to_string(),
                values: Some(vec!["English".to_string(), "简体中文".to_string()]),
                submenu: None,
            },
        ];

        // 图片相关项（级联 splice，顺序是稳定契约，改动需同步黄金渲染测试）。
        // 前两项只在 supports_images 时插入，位置是硬编码的 1、2（紧跟 autocompact）；
        // 其余九项全部按目标 id 级联插入，因此能力为 false 时顺序仍然正确。测试固定
        // images: None，黄金渲染里不含前两项。
        if supports_images {
            items.insert(
                1,
                SettingItem {
                    id: "show-images".to_string(),
                    label: "Show images".to_string(),
                    description: Some("Render images inline in terminal".to_string()),
                    current_value: if config.show_images { "true" } else { "false" }.to_string(),
                    values: Some(vec!["true".to_string(), "false".to_string()]),
                    submenu: None,
                },
            );
            items.insert(
                2,
                SettingItem {
                    id: "image-width-cells".to_string(),
                    label: "Image width".to_string(),
                    description: Some("Preferred inline image width in terminal cells".to_string()),
                    current_value: js_number_string(config.image_width_cells),
                    values: Some(vec!["60".to_string(), "80".to_string(), "120".to_string()]),
                    submenu: None,
                },
            );
        }
        // 从这里开始的插入都以目标 id 定位：能力为 false 时 auto-resize-images 的目标
        // 退化为 autocompact，插入结果与 TS 的 `splice(supportsImages ? 3 : 1, ...)` 相同。
        Self::insert_after(
            &mut items,
            if supports_images {
                "image-width-cells"
            } else {
                "autocompact"
            },
            SettingItem {
                id: "auto-resize-images".to_string(),
                label: "Auto-resize images".to_string(),
                description: Some(
                    "Resize large images to 2000x2000 max for better model compatibility"
                        .to_string(),
                ),
                current_value: if config.auto_resize_images {
                    "true"
                } else {
                    "false"
                }
                .to_string(),
                values: Some(vec!["true".to_string(), "false".to_string()]),
                submenu: None,
            },
        );
        Self::insert_after(
            &mut items,
            "auto-resize-images",
            SettingItem {
                id: "block-images".to_string(),
                label: "Block images".to_string(),
                description: Some("Prevent images from being sent to LLM providers".to_string()),
                current_value: if config.block_images { "true" } else { "false" }.to_string(),
                values: Some(vec!["true".to_string(), "false".to_string()]),
                submenu: None,
            },
        );
        Self::insert_after(
            &mut items,
            "block-images",
            SettingItem {
                id: "skill-commands".to_string(),
                label: "Skill commands".to_string(),
                description: Some("Register skills as /skill:name commands".to_string()),
                current_value: if config.enable_skill_commands {
                    "true"
                } else {
                    "false"
                }
                .to_string(),
                values: Some(vec!["true".to_string(), "false".to_string()]),
                submenu: None,
            },
        );
        Self::insert_after(
            &mut items,
            "skill-commands",
            SettingItem {
                id: "show-hardware-cursor".to_string(),
                label: "Show hardware cursor".to_string(),
                description: Some(
                    "Use the terminal cursor instead of Kiana's drawn cursor".to_string(),
                ),
                current_value: if config.show_hardware_cursor {
                    "true"
                } else {
                    "false"
                }
                .to_string(),
                values: Some(vec!["true".to_string(), "false".to_string()]),
                submenu: None,
            },
        );
        Self::insert_after(
            &mut items,
            "show-hardware-cursor",
            SettingItem {
                id: "editor-padding".to_string(),
                label: "Editor padding".to_string(),
                description: Some("Horizontal padding for input editor (0-3)".to_string()),
                current_value: js_number_string(config.editor_padding_x),
                values: Some(vec![
                    "0".to_string(),
                    "1".to_string(),
                    "2".to_string(),
                    "3".to_string(),
                ]),
                submenu: None,
            },
        );
        Self::insert_after(
            &mut items,
            "editor-padding",
            SettingItem {
                id: "output-padding".to_string(),
                label: "Output padding".to_string(),
                description: Some(
                    "Horizontal padding for messages, tool output, and command output".to_string(),
                ),
                current_value: config.output_pad.to_string(),
                values: Some(vec!["0".to_string(), "1".to_string()]),
                submenu: None,
            },
        );
        Self::insert_after(
            &mut items,
            "output-padding",
            SettingItem {
                id: "autocomplete-max-visible".to_string(),
                label: "Autocomplete max items".to_string(),
                description: Some("Max visible items in autocomplete dropdown (3-20)".to_string()),
                current_value: js_number_string(config.autocomplete_max_visible),
                values: Some(vec![
                    "3".to_string(),
                    "5".to_string(),
                    "7".to_string(),
                    "10".to_string(),
                    "15".to_string(),
                    "20".to_string(),
                ]),
                submenu: None,
            },
        );
        Self::insert_after(
            &mut items,
            "autocomplete-max-visible",
            SettingItem {
                id: "clear-on-shrink".to_string(),
                label: "Clear on shrink".to_string(),
                description: Some(
                    "Clear empty rows when content shrinks (may cause flicker)".to_string(),
                ),
                current_value: if config.clear_on_shrink {
                    "true"
                } else {
                    "false"
                }
                .to_string(),
                values: Some(vec!["true".to_string(), "false".to_string()]),
                submenu: None,
            },
        );
        Self::insert_after(
            &mut items,
            "clear-on-shrink",
            SettingItem {
                id: "terminal-progress".to_string(),
                label: "Terminal progress".to_string(),
                description: Some(
                    "Show OSC 9;4 progress indicators in the terminal tab bar".to_string(),
                ),
                current_value: if config.show_terminal_progress {
                    "true"
                } else {
                    "false"
                }
                .to_string(),
                values: Some(vec!["true".to_string(), "false".to_string()]),
                submenu: None,
            },
        );

        // id → 宿主回调的映射。三个 submenu（warnings / model-thinking / theme）不在
        // 这里：它们各自持有 callbacks 并在自己的流程里调用。未知 id 走 `_ => {}` 静默
        // 忽略（TS 的 switch 没有 default）；数值项统一用 parse_int_js 复刻 parseInt，
        // 使 "80" 之类字符串得到 f64。
        let change_callbacks = Rc::clone(&callbacks);
        let on_change: SettingChangeCallback = Box::new(move |id: &str, new_value: &str| {
            let callbacks = &change_callbacks;
            match id {
                "autocompact" => (callbacks.on_auto_compact_change)(new_value == "true"),
                "show-images" => (callbacks.on_show_images_change)(new_value == "true"),
                "image-width-cells" => {
                    (callbacks.on_image_width_cells_change)(parse_int_js(new_value));
                }
                "auto-resize-images" => {
                    (callbacks.on_auto_resize_images_change)(new_value == "true");
                }
                "block-images" => (callbacks.on_block_images_change)(new_value == "true"),
                "skill-commands" => {
                    (callbacks.on_enable_skill_commands_change)(new_value == "true");
                }
                "steering-mode" => {
                    (callbacks.on_steering_mode_change)(if new_value == "one-at-a-time" {
                        SteeringMode::OneAtATime
                    } else {
                        SteeringMode::All
                    });
                }
                "follow-up-mode" => {
                    (callbacks.on_follow_up_mode_change)(if new_value == "one-at-a-time" {
                        SteeringMode::OneAtATime
                    } else {
                        SteeringMode::All
                    });
                }
                "transport" => {
                    (callbacks.on_transport_change)(match new_value {
                        "sse" => Transport::Sse,
                        "websocket" => Transport::Websocket,
                        "websocket-cached" => Transport::WebsocketCached,
                        _ => Transport::Auto,
                    });
                }
                "share-destination" => {
                    (callbacks.on_session_share_destination_change)(match new_value {
                        "gist" => SessionShareDestination::Gist,
                        "radius" => SessionShareDestination::Radius,
                        _ => SessionShareDestination::Auto,
                    });
                }
                "http-idle-timeout" => {
                    if let Some(choice) = HTTP_IDLE_TIMEOUT_CHOICES
                        .iter()
                        .find(|choice| choice.label == new_value)
                    {
                        (callbacks.on_http_idle_timeout_ms_change)(choice.timeout_ms as f64);
                    }
                }
                "cache-warming-mode" => {
                    (callbacks.on_cache_warming_mode_change)(match new_value {
                        "off" => CacheWarmingMode::Off,
                        "idle" => CacheWarmingMode::Idle,
                        _ => CacheWarmingMode::Streaming,
                    });
                }
                "hide-thinking" => {
                    (callbacks.on_hide_thinking_block_change)(new_value == "true");
                }
                "mermaid-rendering" => {
                    (callbacks.on_mermaid_rendering_mode_change)(match new_value {
                        "off" => MermaidRenderingMode::Off,
                        "final" => MermaidRenderingMode::Final,
                        _ => MermaidRenderingMode::Streaming,
                    });
                }
                "cache-miss-notices" => {
                    (callbacks.on_show_cache_miss_notices_change)(new_value == "true");
                }
                "collapse-changelog" => {
                    (callbacks.on_collapse_changelog_change)(new_value == "true");
                }
                "quiet-startup" => {
                    (callbacks.on_quiet_startup_change)(if new_value == "header" {
                        QuietStartup::Header
                    } else {
                        QuietStartup::Boolean(new_value == "true")
                    });
                }
                "install-telemetry" => {
                    (callbacks.on_enable_install_telemetry_change)(new_value == "true");
                }
                "default-project-trust" => {
                    if let Some(trust) = default_project_trust_by_label(new_value) {
                        (callbacks.on_default_project_trust_change)(trust);
                    }
                }
                "double-escape-action" => {
                    (callbacks.on_double_escape_action_change)(match new_value {
                        "fork" => DoubleEscapeAction::Fork,
                        "none" => DoubleEscapeAction::None,
                        _ => DoubleEscapeAction::Tree,
                    });
                }
                "tree-filter-mode" => {
                    (callbacks.on_tree_filter_mode_change)(match new_value {
                        "no-tools" => TreeFilterMode::NoTools,
                        "user-only" => TreeFilterMode::UserOnly,
                        "labeled-only" => TreeFilterMode::LabeledOnly,
                        "all" => TreeFilterMode::All,
                        _ => TreeFilterMode::Default,
                    });
                }
                "show-hardware-cursor" => {
                    (callbacks.on_show_hardware_cursor_change)(new_value == "true");
                }
                "editor-padding" => {
                    (callbacks.on_editor_padding_x_change)(parse_int_js(new_value));
                }
                "output-padding" => {
                    (callbacks.on_output_pad_change)(if new_value == "0" { 0 } else { 1 });
                }
                "autocomplete-max-visible" => {
                    (callbacks.on_autocomplete_max_visible_change)(parse_int_js(new_value));
                }
                "clear-on-shrink" => {
                    (callbacks.on_clear_on_shrink_change)(new_value == "true");
                }
                "terminal-progress" => {
                    (callbacks.on_show_terminal_progress_change)(new_value == "true");
                }
                "tui-mode" => {
                    (callbacks.on_tui_mode_change)(if new_value == "regular" {
                        TuiMode::Regular
                    } else {
                        TuiMode::Fullscreen
                    });
                }
                "fullscreen-exit-output" => {
                    (callbacks.on_fullscreen_exit_output_change)(if new_value == "resume-hint" {
                        FullscreenExitOutput::ResumeHint
                    } else {
                        FullscreenExitOutput::Transcript
                    });
                }
                "fullscreen-scrollbar" => {
                    (callbacks.on_fullscreen_scrollbar_change)(match new_value {
                        "always" => FullscreenScrollbar::Always,
                        "hidden" => FullscreenScrollbar::Hidden,
                        _ => FullscreenScrollbar::Auto,
                    });
                }
                "fullscreen-copy-on-select" => {
                    (callbacks.on_fullscreen_copy_on_select_change)(new_value == "true");
                }
                "fullscreen-wheel-scroll-lines" => {
                    (callbacks.on_fullscreen_wheel_scroll_lines_change)(if new_value == "auto" {
                        WheelScrollLines::Auto
                    } else {
                        WheelScrollLines::Fixed(parse_int_js(new_value))
                    });
                }
                "theme" => (callbacks.on_theme_change)(new_value),
                "ui-locale" => (callbacks.on_ui_locale_change)(ui_locale_code(new_value)),
                _ => {}
            }
        });
        let cancel_callback = Rc::clone(&callbacks);
        // 主列表一次显示 10 行条目（不含搜索框、说明与提示），并启用模糊搜索；搜索框
        // 只过滤 label。Esc 在无子菜单时直达 on_cancel，宿主用它关闭整个面板。
        let settings_list = SettingsList::new(
            items,
            10,
            get_settings_list_theme(),
            on_change,
            Box::new(move || (cancel_callback.on_cancel)()),
            SettingsListOptions {
                enable_search: Some(true),
            },
        );
        SettingsSelectorComponent { settings_list }
    }

    /// 主列表（TS `getSettingsList()`）。
    ///
    /// 返回可变引用，宿主用它下钻焦点、调用 `update_value` / `select_item`；列表的
    /// 内部状态与组件共享，因此修改会直接反映到下一次 `render`。本方法不触发任何回调。
    pub fn get_settings_list(&mut self) -> &mut SettingsList {
        &mut self.settings_list
    }

    /// 主列表的只读视图。
    ///
    /// 用于只读取渲染结果或检查列表状态的调用方，不触发任何回调；要修改列表请走
    /// [`SettingsSelectorComponent::get_settings_list`]。
    pub fn settings_list(&self) -> &SettingsList {
        &self.settings_list
    }

    fn insert_after(items: &mut Vec<SettingItem>, id: &str, item: SettingItem) {
        // TS 的 `items.splice(items.findIndex(...) + 1, 0, item)`：找不到时 findIndex 返回
        // -1，插入位置是 0（不是末尾）。
        let index = items.iter().position(|existing| existing.id == id);
        match index {
            Some(index) => items.insert(index + 1, item),
            None => items.insert(0, item),
        }
    }

    /// 构造 `warnings` 条目及其子菜单工厂。
    ///
    /// 每次打开都从 `current_warnings`（组件内可变副本）取当前值新建
    /// [`WarningSettingsSubmenu`]。子菜单内每切换一次都会把完整快照写回该副本并调用
    /// `on_warnings_change`；取消则通过 `done(None, None)` 只关闭子菜单。主列表条目
    /// 的显示值固定为 `configure`，不随开关变化。
    fn warnings_item(
        current_warnings: Rc<RefCell<WarningSettings>>,
        callbacks: Rc<SettingsCallbacks>,
    ) -> SettingItem {
        let submenu: SubmenuFactory =
            Rc::new(move |_current_value: &str, done: SettingsDoneCallback| {
                let warnings = current_warnings.borrow().clone();
                let change_warnings = Rc::clone(&current_warnings);
                let change_callbacks = Rc::clone(&callbacks);
                let done = Rc::new(RefCell::new(done));
                Box::new(WarningSettingsSubmenu::new(
                    warnings,
                    Rc::new(move |warnings: &WarningSettings| {
                        *change_warnings.borrow_mut() = warnings.clone();
                        (change_callbacks.on_warnings_change)(warnings.clone());
                    }),
                    Rc::new(move || (done.borrow_mut())(None, None)),
                ))
            });
        SettingItem {
            id: "warnings".to_string(),
            label: "Warnings".to_string(),
            description: Some("Enable or disable individual warnings".to_string()),
            current_value: "configure".to_string(),
            values: None,
            submenu: Some(submenu),
        }
    }

    /// 构造 `model-thinking` 条目及两步 [`SteppedSubmenu`]。
    ///
    /// 两步通过 [`IndexMap`] 上下文传递选择：第一步存 `model`（`provider/id`），第二步
    /// 存 `level`（等级字符串或 `__clear__`）。完成时调用宿主的设置/移除回调并同步本地
    /// 副本；取消时用本地副本重新计算摘要，经 `done(Some(summary))` 刷新主列表显示值
    /// ——主列表 `on_change` 没有 `model-thinking` 分支，因此这个值只更新显示，不会
    /// 触发宿主保存。
    #[allow(clippy::too_many_arguments)]
    fn model_thinking_item(
        config: Rc<SettingsConfig>,
        callbacks: Rc<SettingsCallbacks>,
        default_model_by_value: Rc<HashMap<String, Model>>,
        current_model_thinking_levels: Rc<RefCell<IndexMap<String, ModelThinkingLevel>>>,
        current_model_key: Option<String>,
        current_default_model_key: Option<String>,
        cycle_thinking_key: &str,
    ) -> SettingItem {
        let cycle_thinking_key = cycle_thinking_key.to_string();
        let current_value =
            model_thinking_overrides_summary(&current_model_thinking_levels.borrow());
        let submenu: SubmenuFactory =
            Rc::new(move |_current_value: &str, done: SettingsDoneCallback| {
                let done = Rc::new(RefCell::new(done));

                let step_models = Rc::new({
                    let config = Rc::clone(&config);
                    let levels = Rc::clone(&current_model_thinking_levels);
                    let current_model_key = current_model_key.clone();
                    let current_default_model_key = current_default_model_key.clone();
                    // 第一步选项。排序优先级：当前模型 → 默认模型 → provider 名的
                    // localeCompare；`sort_by` 是稳定排序，同一 provider 内保持
                    // available_default_models 的原顺序。描述列显示已有 override。
                    move |_context: &IndexMap<String, String>| -> Vec<kiana_tui::SelectItem> {
                        let mut sorted: Vec<&Model> =
                            config.available_default_models.iter().collect();
                        sorted.sort_by(|left, right| {
                            let left_key = model_setting_key(left);
                            let right_key = model_setting_key(right);
                            if Some(&left_key) == current_model_key.as_ref() {
                                return Ordering::Less;
                            }
                            if Some(&right_key) == current_model_key.as_ref() {
                                return Ordering::Greater;
                            }
                            if Some(&left_key) == current_default_model_key.as_ref() {
                                return Ordering::Less;
                            }
                            if Some(&right_key) == current_default_model_key.as_ref() {
                                return Ordering::Greater;
                            }
                            kiana_tui::autocomplete::locale_compare(&left.provider, &right.provider)
                        });
                        let mut items: Vec<kiana_tui::SelectItem> = sorted
                            .into_iter()
                            .map(|model| {
                                let key = model_setting_key(model);
                                let override_level = levels.borrow().get(&key).copied();
                                kiana_tui::SelectItem {
                                    value: key,
                                    label: model_item_label(model),
                                    description: override_level
                                        .map(|level| thinking_level_str(level).to_string()),
                                }
                            })
                            .collect();
                        // `__none__` 不是合法模型 key：第二步会因查不到模型而返回空
                        // 选项，完成回调也会直接返回，因此选中它不会写任何设置。
                        if items.is_empty() {
                            items.push(kiana_tui::SelectItem {
                                value: "__none__".to_string(),
                                label: "No models available".to_string(),
                                description: Some(
                                    "Log in to a provider or configure an API key first"
                                        .to_string(),
                                ),
                            });
                        }
                        items
                    }
                });
                let preselect_models = {
                    let current_model_key = current_model_key.clone();
                    let current_default_model_key = current_default_model_key.clone();
                    Rc::new(move |_context: &IndexMap<String, String>| {
                        current_model_key
                            .clone()
                            .or_else(|| current_default_model_key.clone())
                    })
                };
                let step1 = SteppedSubmenuStep::new(
                    "model",
                    "Per-Model Thinking Level",
                    "Select a model to configure",
                    step_models,
                )
                .with_preselect(preselect_models)
                .with_searchable(true)
                .with_layout(model_picker_layout());

                // 第二步：按第一步的模型生成等级列表。只有 `reasoning == Some(true)` 才
                // 使用模型支持的全部等级；`None`/`false` 只给 `off`。已有 override 时
                // 末尾追加“清除”项，其描述引用全局默认等级。
                let step_levels = Rc::clone(&current_model_thinking_levels);
                let step_models_map = Rc::clone(&default_model_by_value);
                let step_config = Rc::clone(&config);
                let title_models = Rc::clone(&default_model_by_value);
                let step2 = SteppedSubmenuStep::new(
                    "level",
                    StepText::Dynamic(Rc::new(move |context: &IndexMap<String, String>| {
                        let key = context.get("model").map(String::as_str).unwrap_or("");
                        match title_models.get(key) {
                            Some(model) => {
                                format!("Thinking Level for {}", model_display_label(model))
                            }
                            None => format!("Thinking Level for {key}"),
                        }
                    })),
                    "Select default thinking level for this model",
                    Rc::new(move |context: &IndexMap<String, String>| {
                        let key = context.get("model").cloned().unwrap_or_default();
                        let Some(model) = step_models_map.get(&key) else {
                            return Vec::new();
                        };
                        let levels = if model.reasoning == Some(true) {
                            get_supported_thinking_levels(model)
                        } else {
                            vec![ModelThinkingLevel::Off]
                        };
                        let active_level = step_levels.borrow().get(&key).copied();
                        let mut items: Vec<kiana_tui::SelectItem> = levels
                            .iter()
                            .map(|level| kiana_tui::SelectItem {
                                value: thinking_level_str(*level).to_string(),
                                label: format!(
                                    "{}{}",
                                    if Some(*level) == active_level {
                                        "✓ "
                                    } else {
                                        "  "
                                    },
                                    thinking_level_str(*level)
                                ),
                                description: Some(thinking_description(*level).to_string()),
                            })
                            .collect();
                        if step_levels.borrow().contains_key(&key) {
                            items.push(kiana_tui::SelectItem {
                                value: CLEAR_OVERRIDE_VALUE.to_string(),
                                label: "  (clear override)".to_string(),
                                description: Some(format!(
                                    "Revert to global default ({})",
                                    thinking_level_str(step_config.thinking_level)
                                )),
                            });
                        }
                        items
                    }),
                )
                .with_preselect({
                    let step_levels = Rc::clone(&current_model_thinking_levels);
                    Rc::new(move |context: &IndexMap<String, String>| {
                        context
                            .get("model")
                            .and_then(|key| step_levels.borrow().get(key).copied())
                            .map(|level| thinking_level_str(level).to_string())
                    })
                });

                // 完成回调：model/level 缺失、或 model 不在索引里（例如 `__none__`）
                // 都静默返回。清除 override 与设置等级走不同的宿主回调；两条路径都会
                // 同步本地副本，保证本次面板内摘要与再次打开时的预选是最新的。
                let complete_models = Rc::clone(&default_model_by_value);
                let complete_levels = Rc::clone(&current_model_thinking_levels);
                let complete_callbacks = Rc::clone(&callbacks);
                let on_complete: StepCompleteCallback = Rc::new(move |selections| {
                    let Some(model_key) = selections.get("model") else {
                        return;
                    };
                    let Some(model) = complete_models.get(model_key) else {
                        return;
                    };
                    let level_value = selections.get("level").cloned().unwrap_or_default();
                    if level_value == CLEAR_OVERRIDE_VALUE {
                        (complete_callbacks.on_model_thinking_level_remove)(
                            &model.provider,
                            &model.id,
                        );
                        complete_levels.borrow_mut().shift_remove(model_key);
                    } else {
                        let level = parse_thinking_level(&level_value);
                        (complete_callbacks.on_model_thinking_level_change)(
                            &model.provider,
                            &model.id,
                            level,
                        );
                        complete_levels
                            .borrow_mut()
                            .insert(model_key.clone(), level);
                    }
                });

                // 取消（Esc）不写任何设置，只用本地副本重算摘要并回传：SettingsList
                // 会把它写成条目显示值，其 `on_change` 对 `model-thinking` 无分支。
                let cancel_levels = Rc::clone(&current_model_thinking_levels);
                let on_cancel: VoidCallback = Rc::new(move || {
                    let summary = model_thinking_overrides_summary(&cancel_levels.borrow());
                    (done.borrow_mut())(Some(summary), None);
                });

                // `looping: true`：选完等级后先执行 on_complete 保存，然后清空上下文回到
                // 第一步，子菜单**不关闭**（`SteppedSubmenu` 的 looping 分支不调用
                // on_cancel），方便连续配置多个模型。用户从第一步按 Esc 退出时才走上面的
                // on_cancel：刷新主列表摘要并关闭。
                Box::new(SteppedSubmenu::new(
                    vec![step1, step2],
                    on_complete,
                    on_cancel,
                    SteppedSubmenuOptions {
                        looping: true,
                        ..Default::default()
                    },
                ))
            });
        SettingItem {
            id: "model-thinking".to_string(),
            label: "Default thinking level per model".to_string(),
            description: Some(format!(
                "Override the default thinking level for specific models. {cycle_thinking_key} cycles in-session."
            )),
            current_value,
            values: None,
            submenu: Some(submenu),
        }
    }

    /// 构造 `theme` 条目及 [`ThemeSubmenu`] 工厂。
    ///
    /// 工厂每次打开都用条目当前值（`current_value` 参数）作为初始设置；子菜单的
    /// `on_done(Some(setting))` 经这里适配为 `SettingsDoneCallback`：SettingsList 会把
    /// setting 写回 `theme` 条目并据 id 触发宿主的 `on_theme_change`；`None` 只关闭。
    /// `terminal_theme` / `available_themes` 取自构造时的 config 快照。
    fn theme_item(config: Rc<SettingsConfig>, callbacks: Rc<SettingsCallbacks>) -> SettingItem {
        let current_value = config.current_theme.clone();
        let submenu_config = Rc::clone(&config);
        let submenu: SubmenuFactory =
            Rc::new(move |current_value: &str, done: SettingsDoneCallback| {
                let done = Rc::new(RefCell::new(done));
                let on_done: Rc<dyn Fn(Option<String>)> = Rc::new(move |value| {
                    (done.borrow_mut())(value, None);
                });
                Box::new(ThemeSubmenu::new(
                    current_value.to_string(),
                    submenu_config.terminal_theme,
                    submenu_config.available_themes.clone(),
                    Rc::clone(&callbacks),
                    on_done,
                ))
            });
        SettingItem {
            id: "theme".to_string(),
            label: "Theme".to_string(),
            description: Some("Color theme for the interface".to_string()),
            current_value,
            values: None,
            submenu: Some(submenu),
        }
    }
}

/// `fullscreen-wheel-scroll-lines` 的 values（逐字复刻 `["auto", ...sortedUnique]`）。
///
/// 候选集合是固定的 1/2/3/5/10，加上当前值（若是数字且不在集合里，例如配置为 7）。
/// 排序升序、去重后逐个用 [`js_number_string`] 转字符串，最后把 `auto` 放在最前。
/// 因此当前值是 5 时不会出现两个 `"5"`；测试以当前值 7 锁定了依次循环出
/// `10 → auto → 1` 的结果。
fn fullscreen_wheel_values(current: WheelScrollLines) -> Vec<String> {
    let mut candidates: Vec<f64> = vec![1.0, 2.0, 3.0, 5.0, 10.0];
    if let WheelScrollLines::Fixed(lines) = current
        && !candidates.contains(&lines)
    {
        candidates.push(lines);
    }
    candidates.sort_by(|left, right| left.partial_cmp(right).unwrap_or(Ordering::Equal));
    let mut values = vec!["auto".to_string()];
    values.extend(candidates.into_iter().map(js_number_string));
    values
}

impl Component for SettingsSelectorComponent {
    // 渲染结构固定为「上边框 + 主列表 + 下边框」。测试断言总行数恰为列表行数 + 2，
    // 且首尾两行相同；中间不能插入 Spacer 或其它行。
    fn render(&mut self, width: usize) -> Vec<String> {
        let mut lines = Vec::new();
        lines.extend(DynamicBorder::default().render(width));
        lines.extend(self.settings_list.render(width));
        lines.extend(DynamicBorder::default().render(width));
        lines
    }

    fn handle_input(&mut self, data: &str) {
        self.settings_list.handle_input(data);
    }

    fn handle_mouse(&mut self, event: &TuiMouseEvent) -> Option<TuiMouseEventResult> {
        // 行 0 是顶部边框，落在边框上的事件不处理；其余事件整体上移一行再交给列表。
        // 高度也减 1：顶部边框这一行已由本组件消费，剩余高度才是列表的坐标空间。
        if event.y < 1 {
            return None;
        }
        let mut local = event.clone();
        local.y -= 1;
        local.height = local.height.saturating_sub(1);
        self.settings_list.handle_mouse(&local)
    }

    fn invalidate(&mut self) {
        self.settings_list.invalidate();
    }

    fn for_each_container_child(&self, f: &mut dyn FnMut(&dyn Component) -> bool) {
        let _ = f(&self.settings_list);
    }

    /// TS 的 `SettingsSelectorComponent extends Container`，宿主用
    /// `focus: selector.getSettingsList()` 聚焦内部列表（`interactive-mode.ts:5137`）。
    /// Rust 侧宿主无法跨所有权借出字段，只能通过焦点路径下钻；缺少本方法时
    /// `TuiBase::with_component_mut_of` 到不了 `SettingsList`，所有按键都会被丢弃。
    fn for_each_container_child_mut(
        &mut self,
        f: &mut dyn FnMut(&mut (dyn Component + 'static)) -> bool,
    ) {
        let _ = f(&mut self.settings_list);
    }
}
