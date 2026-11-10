//! `core/settings-defaults.ts`：`SETTINGS_DEFAULTS`（`ca-config-models.md` §2.7）。
//!
//! TS 的 `SETTINGS_DEFAULTS` 是 `as const` 对象，字段顺序即 source 顺序；运行期只按
//! 属性名读取（`SETTINGS_DEFAULTS.compaction.reserveTokens` 等），没有代码或测试对它做
//! `JSON.stringify`。这里因此用一组**非可选**的类型化默认值（[`SettingsDefaults`]）
//! 逐字段复刻同一形状：settings-manager 的 getter 写
//! `settings.field.unwrap_or(SETTINGS_DEFAULTS.field)`，auth-trust 直接读
//! `SETTINGS_DEFAULTS.default_project_trust`。
//!
//! 需要一份「全部字段都填好默认值」的 [`Settings`] 时用
//! [`SettingsDefaults::to_settings`]；该值的序列化键序是 `settings.rs` 的字段声明顺序
//! （§2.8.3），不是 TS source 顺序（§2.7）。§2.7 的顺序只影响源码可读性：仓库里没有
//! 任何测试或生成脚本依赖 `SETTINGS_DEFAULTS` 的序列化字节序（4 个 JSON Schema 产物
//! 属于阶段 8，`ARCHITECTURE.md` §8.25）。
//!
//! # 调用方
//!
//! - `settings::mod` 的 getter：类型化字段缺失时回落到 `SETTINGS_DEFAULTS.<字段>`；
//! - `trust/project_trust.rs`：读 `default_project_trust` 决定未配置时的信任策略；
//! - `kiana-interactive` 的 settings 选择器与 `kiana-builtin-extensions` 的 codemode：
//!   用这些值显示/实现「未设置时的行为」。
//!
//! 新增带默认值的设置项时，要同时改 [`SettingsDefaults`] 的字段和 [`SETTINGS_DEFAULTS`]
//! 的初始化（结构体字面量必须列出全部字段，漏改会编译失败）；若还希望
//! [`SettingsDefaults::to_settings`] 的展开结果包含它，需在 `to_settings` 里补对应赋值。

use indexmap::IndexMap;
use kiana_ai::utils::retry::DEFAULT_MAX_AGENT_RETRY_DELAY_MS;

use crate::settings::schema::{
    BranchSummarySettings, CacheWarmingMode, CodemodeMode, CodemodeSettings, CompactionSettings,
    DefaultProjectTrust, DoubleEscapeAction, FullscreenExitOutput, FullscreenScrollbar,
    FullscreenWheelScrollLines, ImageSettings, MarkdownSettings, MermaidRenderingMode,
    ProviderRetrySettings, QuietStartup, RetrySettings, SessionShareDestination, Settings,
    SteeringMode, TerminalSettings, TransportSetting, TreeFilterMode, TuiMode, WarningSettings,
};

/// `SETTINGS_DEFAULTS.compaction`。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CompactionDefaults {
    /// `compaction.enabled`：是否启用自动压缩（settings 选择器把它显示为 auto-compact）。
    pub enabled: bool,
    /// `compaction.reserveTokens`：为 prompt + 回复保留的 token 数。
    pub reserve_tokens: f64,
    /// `compaction.keepRecentTokens`：压缩时最近需要保留的 token 数。
    pub keep_recent_tokens: f64,
}

/// `SETTINGS_DEFAULTS.branchSummary`。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BranchSummaryDefaults {
    /// `branchSummary.reserveTokens`：分支摘要的预留 token 数。
    pub reserve_tokens: f64,
    /// `branchSummary.skipPrompt`：是否跳过摘要前的确认提示。
    pub skip_prompt: bool,
}

/// `SETTINGS_DEFAULTS.retry.provider`。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ProviderRetryDefaults {
    /// `retry.provider.maxRetryDelayMs`：provider 重试的最大延迟（毫秒）。
    pub max_retry_delay_ms: f64,
}

/// `SETTINGS_DEFAULTS.retry`。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RetryDefaults {
    /// `retry.enabled`。
    pub enabled: bool,
    /// `retry.maxRetries`：最大重试次数。
    pub max_retries: f64,
    /// `retry.baseDelayMs`：重试的基础延迟（毫秒）。
    pub base_delay_ms: f64,
    /// `retry.maxAgentDelayMs`：agent 层重试的延迟上限（毫秒），初值取自
    /// `kiana_ai::utils::retry::DEFAULT_MAX_AGENT_RETRY_DELAY_MS`。
    pub max_agent_delay_ms: f64,
    /// `retry.provider` 的默认值。
    pub provider: ProviderRetryDefaults,
}

/// `SETTINGS_DEFAULTS.terminal`。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TerminalDefaults {
    /// `terminal.showImages`。
    pub show_images: bool,
    /// `terminal.imageWidthCells`：图片显示宽度，单位是终端单元格。
    pub image_width_cells: f64,
    /// `terminal.clearOnShrink`。
    pub clear_on_shrink: bool,
    /// `terminal.showTerminalProgress`。
    pub show_terminal_progress: bool,
}

/// `SETTINGS_DEFAULTS.images`。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ImageDefaults {
    /// `images.autoResize`。
    pub auto_resize: bool,
    /// `images.blockImages`。
    pub block_images: bool,
}

/// `SETTINGS_DEFAULTS.markdown`。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MarkdownDefaults {
    /// 恰好两个空格（§2.7 的强调项）。
    pub code_block_indent: &'static str,
    /// `markdown.mermaid`：mermaid 的渲染模式。
    pub mermaid: MermaidRenderingMode,
}

/// `SETTINGS_DEFAULTS.warnings`。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WarningsDefaults {
    /// `warnings.anthropicExtraUsage`。
    pub anthropic_extra_usage: bool,
}

/// `SETTINGS_DEFAULTS.codemode`。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CodemodeDefaults {
    /// `codemode.mode`。
    pub mode: CodemodeMode,
    /// `codemode.inlineBudget`（默认 3000）。
    pub inline_budget: f64,
}

/// `SETTINGS_DEFAULTS` 的类型化形状（字段顺序与 `settings-defaults.ts` 的 source 顺序一致）。
///
/// 每个字段对应 TS 对象的一个顶层属性（字段名是属性名的 snake_case 形式），并且**必有
/// 值**——对应 getter 用 `unwrap_or(...)` 回落到这里。注意它只覆盖有默认值的设置项，
/// `theme`、`defaultProvider` 等没有默认值的字段不在此结构体里（见
/// [`SettingsDefaults::to_settings`] 末尾的 `..Settings::default()`）。具体的默认值直接看
/// [`SETTINGS_DEFAULTS`] 的初始化，那里是唯一事实来源。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SettingsDefaults {
    pub transport: TransportSetting,
    pub session_share_destination: SessionShareDestination,
    pub steering_mode: SteeringMode,
    pub follow_up_mode: SteeringMode,
    pub compaction: CompactionDefaults,
    pub branch_summary: BranchSummaryDefaults,
    pub retry: RetryDefaults,
    pub hide_thinking_block: bool,
    pub show_cache_miss_notices: bool,
    pub quiet_startup: QuietStartup,
    pub default_project_trust: DefaultProjectTrust,
    pub collapse_changelog: bool,
    pub enable_install_telemetry: bool,
    pub enable_analytics: bool,
    pub enable_skill_commands: bool,
    pub terminal: TerminalDefaults,
    pub images: ImageDefaults,
    pub double_escape_action: DoubleEscapeAction,
    pub tree_filter_mode: TreeFilterMode,
    pub editor_padding_x: f64,
    pub output_pad: f64,
    pub autocomplete_max_visible: f64,
    pub markdown: MarkdownDefaults,
    pub warnings: WarningsDefaults,
    pub codemode: CodemodeDefaults,
    pub cache_warming: CacheWarmingMode,
    pub tui_mode: TuiMode,
    pub fullscreen_exit_output: FullscreenExitOutput,
    pub fullscreen_scrollbar: FullscreenScrollbar,
    pub fullscreen_copy_on_select: bool,
    pub fullscreen_wheel_scroll_lines: FullscreenWheelScrollLines,
}

/// `SETTINGS_DEFAULTS`（§2.7，逐字）。
pub const SETTINGS_DEFAULTS: SettingsDefaults = SettingsDefaults {
    transport: TransportSetting::Auto,
    session_share_destination: SessionShareDestination::Auto,
    steering_mode: SteeringMode::OneAtATime,
    follow_up_mode: SteeringMode::OneAtATime,
    compaction: CompactionDefaults {
        enabled: true,
        reserve_tokens: 16_384.0,
        keep_recent_tokens: 20_000.0,
    },
    branch_summary: BranchSummaryDefaults {
        reserve_tokens: 16_384.0,
        skip_prompt: false,
    },
    retry: RetryDefaults {
        enabled: true,
        max_retries: 3.0,
        base_delay_ms: 2_000.0,
        max_agent_delay_ms: DEFAULT_MAX_AGENT_RETRY_DELAY_MS as f64,
        provider: ProviderRetryDefaults {
            max_retry_delay_ms: 60_000.0,
        },
    },
    hide_thinking_block: false,
    show_cache_miss_notices: false,
    quiet_startup: QuietStartup::Boolean(false),
    default_project_trust: DefaultProjectTrust::Ask,
    collapse_changelog: false,
    enable_install_telemetry: true,
    enable_analytics: false,
    enable_skill_commands: true,
    terminal: TerminalDefaults {
        show_images: true,
        image_width_cells: 60.0,
        clear_on_shrink: false,
        show_terminal_progress: false,
    },
    images: ImageDefaults {
        auto_resize: true,
        block_images: false,
    },
    double_escape_action: DoubleEscapeAction::Tree,
    tree_filter_mode: TreeFilterMode::Default,
    editor_padding_x: 0.0,
    output_pad: 1.0,
    autocomplete_max_visible: 5.0,
    markdown: MarkdownDefaults {
        // 两个空格，逐字。
        code_block_indent: "  ",
        mermaid: MermaidRenderingMode::Streaming,
    },
    warnings: WarningsDefaults {
        anthropic_extra_usage: true,
    },
    codemode: CodemodeDefaults {
        mode: CodemodeMode::On,
        inline_budget: 3_000.0,
    },
    cache_warming: CacheWarmingMode::Streaming,
    tui_mode: TuiMode::Fullscreen,
    fullscreen_exit_output: FullscreenExitOutput::Transcript,
    fullscreen_scrollbar: FullscreenScrollbar::Auto,
    fullscreen_copy_on_select: true,
    fullscreen_wheel_scroll_lines: FullscreenWheelScrollLines::Auto,
};

impl Default for SettingsDefaults {
    fn default() -> Self {
        SETTINGS_DEFAULTS
    }
}

impl SettingsDefaults {
    /// 把全部默认值展开成一份 [`Settings`]（其余字段保持缺省）。
    ///
    /// 注意：这是「默认值基底」，不是 `SettingsManager` 的整体默认设置；例如
    /// `theme`、`defaultProvider` 等没有默认值，保持 `None`。展开只覆盖
    /// [`SettingsDefaults`] 里列出的字段；未列出的嵌套字段由末尾的
    /// `..Settings::default()` 保持 `None`（例如 `terminal.hyperlinks`、`retry.provider`
    /// 的 `timeoutMs` / `maxRetries`）。
    ///
    /// 仓库内目前由 `tests/config_schemas.rs` 用来验证 31 个默认顶层键都存在。
    pub fn to_settings(&self) -> Settings {
        Settings {
            transport: Some(self.transport),
            session_share_destination: Some(self.session_share_destination),
            steering_mode: Some(self.steering_mode),
            follow_up_mode: Some(self.follow_up_mode),
            compaction: Some(CompactionSettings {
                enabled: Some(self.compaction.enabled),
                reserve_tokens: Some(self.compaction.reserve_tokens),
                keep_recent_tokens: Some(self.compaction.keep_recent_tokens),
                model_overrides: None,
                extra: IndexMap::new(),
            }),
            branch_summary: Some(BranchSummarySettings {
                reserve_tokens: Some(self.branch_summary.reserve_tokens),
                skip_prompt: Some(self.branch_summary.skip_prompt),
                extra: IndexMap::new(),
            }),
            retry: Some(RetrySettings {
                enabled: Some(self.retry.enabled),
                max_retries: Some(self.retry.max_retries),
                base_delay_ms: Some(self.retry.base_delay_ms),
                max_agent_delay_ms: Some(self.retry.max_agent_delay_ms),
                provider: Some(ProviderRetrySettings {
                    max_retry_delay_ms: Some(self.retry.provider.max_retry_delay_ms),
                    ..ProviderRetrySettings::default()
                }),
                extra: IndexMap::new(),
            }),
            hide_thinking_block: Some(self.hide_thinking_block),
            show_cache_miss_notices: Some(self.show_cache_miss_notices),
            quiet_startup: Some(self.quiet_startup),
            default_project_trust: Some(self.default_project_trust),
            collapse_changelog: Some(self.collapse_changelog),
            enable_install_telemetry: Some(self.enable_install_telemetry),
            enable_analytics: Some(self.enable_analytics),
            enable_skill_commands: Some(self.enable_skill_commands),
            terminal: Some(TerminalSettings {
                show_images: Some(self.terminal.show_images),
                image_width_cells: Some(self.terminal.image_width_cells),
                clear_on_shrink: Some(self.terminal.clear_on_shrink),
                show_terminal_progress: Some(self.terminal.show_terminal_progress),
                ..TerminalSettings::default()
            }),
            images: Some(ImageSettings {
                auto_resize: Some(self.images.auto_resize),
                block_images: Some(self.images.block_images),
                extra: IndexMap::new(),
            }),
            double_escape_action: Some(self.double_escape_action),
            tree_filter_mode: Some(self.tree_filter_mode),
            editor_padding_x: Some(self.editor_padding_x),
            output_pad: Some(self.output_pad),
            autocomplete_max_visible: Some(self.autocomplete_max_visible),
            markdown: Some(MarkdownSettings {
                code_block_indent: Some(self.markdown.code_block_indent.to_string()),
                mermaid: Some(self.markdown.mermaid),
                extra: IndexMap::new(),
            }),
            warnings: Some(WarningSettings {
                anthropic_extra_usage: Some(self.warnings.anthropic_extra_usage),
                extra: IndexMap::new(),
            }),
            codemode: Some(CodemodeSettings {
                mode: Some(self.codemode.mode),
                inline_budget: Some(self.codemode.inline_budget),
                extra: IndexMap::new(),
            }),
            cache_warming: Some(self.cache_warming),
            tui_mode: Some(self.tui_mode),
            fullscreen_exit_output: Some(self.fullscreen_exit_output),
            fullscreen_scrollbar: Some(self.fullscreen_scrollbar),
            fullscreen_copy_on_select: Some(self.fullscreen_copy_on_select),
            fullscreen_wheel_scroll_lines: Some(self.fullscreen_wheel_scroll_lines),
            ..Settings::default()
        }
    }
}
