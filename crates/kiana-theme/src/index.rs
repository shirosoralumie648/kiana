//! 主题相关再导出面。
//!
//! 与 `kiana-tui` 的 `index.rs` 同一约定：crate 根把这里的全部再导出抬上去，下游写
//! `kiana_theme::Theme` / `kiana_theme::theme()` 即可，模块路径（`kiana_theme::theme::Theme`）也保留。
//! 本文件只做再导出，没有逻辑。
//!
//! # 维护规则
//!
//! - 分组与源文件一一对应（assets / system-theme / theme-tokens / theme-schema / theme /
//!   theme-controller），新增公开项时放进对应分组。
//! - `lib.rs` 用 `pub use crate::index::*;` 把这里的名字抬到 crate 根；**没有列在这里**的
//!   公开项仍然能通过模块路径访问，但下游按 crate 根引用时看不到。
//! - 再导出项本身不写 `///`：rustdoc 会沿用源条目的文档，在这里重复描述容易与源文档漂移。
//! - 删除/改名这里的一项属于破坏公开 API；模块内部可以自由重构，公开面必须同步本文件。

// ---------------------------------------------------------------------------
// assets.rs
// ---------------------------------------------------------------------------
pub use crate::assets::{DARK_JSON, LIGHT_JSON};

// ---------------------------------------------------------------------------
// system-theme.ts
// ---------------------------------------------------------------------------
pub use crate::system_theme::{
    SYSTEM_THEME_NAME, SystemThemeColors, SystemThemeInput, generate_system_theme_colors,
    relative_luminance, terminal_appearance, wcag_contrast,
};

// ---------------------------------------------------------------------------
// theme-tokens.ts
// ---------------------------------------------------------------------------
pub use crate::theme_tokens::{
    Slot, THEME_COLOR_NAMES, THEME_TOKENS, ThemeBg, ThemeColor, ThemeToken, ThemeTokenDescriptor,
};

// ---------------------------------------------------------------------------
// theme-schema.ts
// ---------------------------------------------------------------------------
pub use crate::theme_schema::{
    ThemeColorValue, ThemeError, ThemeExport, ThemeJson, validate_theme_json,
};

// ---------------------------------------------------------------------------
// theme.ts
// ---------------------------------------------------------------------------
pub use crate::theme::{
    AutoTheme, THEME, TerminalTheme, Theme, ThemeAppearance, ThemeBgValue, ThemeExportColors,
    ThemeFg, ThemeInfo, ThemeJsonValidator, ThemeOptions, ThemeProxy, ThemeResult, ThemeStyle,
    clear_theme_json_validator, current_theme, current_theme_name, detect_color_fg_bg_theme,
    detect_color_fg_bg_theme_from, detect_terminal_theme, detect_terminal_theme_with_env,
    get_available_themes, get_available_themes_with_paths, get_bash_mode_border_color,
    get_editor_theme, get_language_from_path, get_markdown_theme, get_resolved_theme_colors,
    get_select_list_theme, get_settings_list_theme, get_terminal_theme, get_theme_by_name,
    get_theme_export_colors, highlight_code, init_theme, is_light_theme, load_theme_from_path,
    mark_terminal_colors_pending, on_theme_change, parse_auto_theme_setting, resolve_theme_setting,
    set_registered_themes, set_terminal_color_scheme, set_terminal_colors, set_theme,
    set_theme_instance, set_theme_json_validator, stop_theme_watcher, terminal_color_scheme,
    terminal_colors, terminal_colors_pending, theme,
};

// ---------------------------------------------------------------------------
// theme-controller.ts
// ---------------------------------------------------------------------------
pub use crate::theme_controller::{
    InteractiveThemeController, TERMINAL_QUERY_TIMEOUT_MS, ThemeControllerOptions, ThemeHost,
    request_terminal_colors,
};
