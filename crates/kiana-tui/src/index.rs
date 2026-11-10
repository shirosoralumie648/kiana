//! `index.ts`：`kiana-tui` 的公共导出面（176 个符号：91 运行时 + 85 类型）。
//!
//! 本模块只是**再导出**，不定义任何东西。分节顺序与 `packages/tui/src/index.ts` 的源码顺序
//! 一致（见 `spec/tui-core.md` §2.18）；节内成员顺序不等同于 TS，核对应以符号集合为准。
//!
//! ## 覆盖状态
//!
//! 全部单元已合并：除 `marked` 的 `Marked`/`Token`/`Tokens` 外（按 `spec/tui-core.md`
//! §6 的决策省略——Rust 侧 markdown 由本 crate 自实现，没有等价的 `Token` 结构），
//! `index.ts` 的每个具名符号在这里都有对应再导出。
//!
//! 命名差异：`encodeITerm2` → [`encode_iterm2`]（crate 内 snake_case 约定）。
//!
//! 注意：`tui.ts` 内部还有一条独立的 `export { visibleWidth }`；`index.ts` 自己走
//! `./utils`，因此 `visibleWidth` 在导出面里只出现一次（总数仍是 176）。
//!
//! ## 为什么是“平铺”的再导出
//!
//! crate 根 `lib.rs` 用 `pub use crate::index::*;` 把这里的名字抬到根上，因此下游 crate
//! 既可以写 `kiana_tui::Component`，也可以继续用子模块路径（例如
//! `kiana_tui::tui::Component`），两条路径指向同一批项；`src/lib.rs` 把前者作为跨 crate
//! 接口的推荐写法。因此新增一个对外符号时，除了在所属模块写 `pub`，还要在这里补一条
//! `pub use`；只加在子模块里的名字不会出现在 crate 根。
//!
//! ## 阅读与维护
//!
//! - 每节标题对应 `packages/tui/src/index.ts` 的一个或一组相邻的源码分组；分节顺序与
//!   `packages/tui/src/index.ts` 一致。节内成员受 Rust 类型/函数分组与 rustfmt 排序影响，
//!   顺序与 TS 并不逐项相同，核对应以符号集合为准。
//! - 命名差异只影响 Rust 标识符（`encodeITerm2` → [`encode_iterm2`]）；字符串常量、协议字段
//!   与错误消息保持原文。
//! - 本模块没有逻辑：行为问题应到各节指向的模块排查，这里只负责可见性。
//! - 所有再导出目标都必须是 `pub`；子模块里的私有项不能出现在导出面里。

// ---------------------------------------------------------------------------
// autocomplete.ts
// ---------------------------------------------------------------------------
pub use crate::autocomplete::{
    AutocompleteItem, AutocompleteProvider, AutocompleteSuggestions, CombinedAutocompleteProvider,
    SlashCommand,
};

// ---------------------------------------------------------------------------
// colors.ts
// ---------------------------------------------------------------------------
pub use crate::colors::{
    Color, ColorMixSpace, IndexedColor, OkhslChannels, OklchChannels, RgbColorValue,
    TerminalColorMode, TextAttributes, TextStyle, background_ansi, color_to_hex, color_to_okhsl,
    color_to_oklch, color_to_rgb, foreground_ansi, indexed_color, mix_colors, okhsl_color,
    oklch_color, parse_color, rgb_color, style_text, style_text_with_ansi,
};
// `OklchColorValue` 由 `terminal-colors` 单元定义，`colors.ts` 只做再导出；Rust 的
// `colors` 模块目前没有转出它（`Color` 的 `Oklch` 变体直接内联），所以这里直接从定义处取。
pub use crate::terminal_colors::OklchColorValue;

// ---------------------------------------------------------------------------
// components/*
// ---------------------------------------------------------------------------
pub use crate::components::box_component::BoxComponent as Box;
pub use crate::components::cancellable_loader::CancellableLoader;
pub use crate::components::editor::{Editor, EditorOptions, EditorTheme};
pub use crate::components::h_stack::HStack;
pub use crate::components::image::{
    Image, ImageOptions, ImageTheme, ImageTranscoder, set_image_transcoder,
};
pub use crate::components::input::Input;
pub use crate::components::loader::{Loader, LoaderIndicatorOptions};
pub use crate::components::markdown::{DefaultTextStyle, Markdown, MarkdownOptions, MarkdownTheme};
pub use crate::components::mouse_region::{MouseRegion, MouseRegionHandler};
pub use crate::components::scroll_view::{
    ScrollView, ScrollViewOptions, ScrollViewScrollToOptions, ScrollViewScrollbar,
};
pub use crate::components::select_list::{
    SelectItem, SelectList, SelectListLayoutOptions, SelectListTheme,
    SelectListTruncatePrimaryContext,
};
pub use crate::components::settings_list::{SettingItem, SettingsList, SettingsListTheme};
pub use crate::components::spacer::Spacer;
pub use crate::components::text_component::Text;
pub use crate::components::truncated_text::TruncatedText;
pub use crate::components::v_stack::{
    StackChild, StackEntry, StackEntryOptions, StackOptions, VStack,
};

// ---------------------------------------------------------------------------
// editor-component.ts
// ---------------------------------------------------------------------------
pub use crate::editor_component::EditorComponent;

// ---------------------------------------------------------------------------
// fuzzy.ts
// ---------------------------------------------------------------------------
pub use crate::fuzzy::{FuzzyMatch, fuzzy_filter, fuzzy_match};

// ---------------------------------------------------------------------------
// keybindings.ts
// ---------------------------------------------------------------------------
pub use crate::keybindings::{
    KEYBINDING_IDS, Keybinding, KeybindingConflict, KeybindingDefinition, KeybindingDefinitions,
    KeybindingsConfig, KeybindingsManager, get_keybindings, set_keybindings,
    tui_keybindings_static,
};
// 命名差异（TS → Rust）：
// * `TUI_KEYBINDINGS`（`as const` 对象）→ [`tui_keybindings_static`]（惰性构造的 `&'static` 表）；
//   TS 的类型 `Keybindings`（47 个 id 的接口）→ [`KEYBINDING_IDS`]（47 项常量数组）。

// ---------------------------------------------------------------------------
// keys.ts
// ---------------------------------------------------------------------------
pub use crate::keys::{
    Key, KeyEventType, KeyId, decode_kitty_printable, is_key_release, is_key_repeat,
    is_kitty_protocol_active, matches_key, parse_key, set_kitty_protocol_active,
};

// ---------------------------------------------------------------------------
// latex.ts
// ---------------------------------------------------------------------------
pub use crate::latex::{RenderLatexOptions, render_latex};

// ---------------------------------------------------------------------------
// native-platform.ts
// ---------------------------------------------------------------------------
// `index.ts` 只导出这两个；完整再导出面见 [`crate::image`]（额外含
// `get_native_platform_helper` 与 `get_native_module_candidates`）。
pub use crate::native::{NativeClipboard, get_native_clipboard};

// ---------------------------------------------------------------------------
// oklab.ts
// ---------------------------------------------------------------------------
pub use crate::oklab::oklab_to_okhsl_lightness;

// ---------------------------------------------------------------------------
// program-status.ts
// ---------------------------------------------------------------------------
pub use crate::program_status::format_program_status;
pub use crate::terminal_colors::ProgramStatus;

// ---------------------------------------------------------------------------
// stdin-buffer.ts
// ---------------------------------------------------------------------------
// TS 另有 `type StdinBufferEventMap`（事件名 → 载荷）；Rust 用 [`StdinEvent`] 枚举表达同一
// 载荷集合（`"data"` → `Data`，`"paste"` → `Paste`），因此这里导出枚举而不是映射类型。
pub use crate::stdin::{StdinBuffer, StdinBufferOptions, StdinEvent};

// ---------------------------------------------------------------------------
// terminal.ts
// ---------------------------------------------------------------------------
pub use crate::terminal::{ProcessTerminal, is_apple_terminal_session};
pub use crate::terminal_colors::Terminal;

// ---------------------------------------------------------------------------
// terminal-colors.ts
// ---------------------------------------------------------------------------
pub use crate::terminal_colors::{
    RgbColor, TerminalColorScheme, TerminalColors, parse_terminal_color_scheme_report,
};

// ---------------------------------------------------------------------------
// terminal-image.ts
// ---------------------------------------------------------------------------
pub use crate::terminal_image::{
    CellDimensions, ImageDimensions, ImageProtocol, ImageRenderOptions, TerminalCapabilities,
    allocate_image_id, calculate_image_rows, delete_all_kitty_images, delete_kitty_image,
    detect_capabilities, encode_iterm2, encode_kitty, get_capabilities, get_cell_dimensions,
    get_gif_dimensions, get_image_dimensions, get_jpeg_dimensions, get_png_dimensions,
    get_terminal_color_mode, get_webp_dimensions, hyperlink, image_fallback, render_image,
    reset_capabilities_cache, set_capabilities, set_capability_overrides, set_cell_dimensions,
};

// ---------------------------------------------------------------------------
// tui.ts
// ---------------------------------------------------------------------------
pub use crate::tui::{
    CURSOR_MARKER, Component, Container, Focusable, OverlayAnchor, OverlayBounds, OverlayHandle,
    OverlayMargin, OverlayOptions, OverlayUnfocusOptions, SizeValue, Tui, TuiInputListener,
    TuiInputListenerResult, TuiMode, TuiMouseButton, TuiMouseEvent, TuiMouseEventResult,
    TuiMouseEventType, TuiStopOptions, VIEWPORT_TUI, ViewportTui, composite_tui_line, is_focusable,
    is_viewport_tui, render_fake_cursor,
};

// ---------------------------------------------------------------------------
// tui-alt-screen.ts
// ---------------------------------------------------------------------------
pub use crate::alt_screen::{TuiAltScreen, TuiAltScreenOptions};

// ---------------------------------------------------------------------------
// tui-main-screen.ts
// ---------------------------------------------------------------------------
pub use crate::main_screen::{TuiMainScreen, TuiMainScreenRenderState};

// ---------------------------------------------------------------------------
// utils.ts
// ---------------------------------------------------------------------------
pub use crate::text::{
    get_osc8_link_at_column, slice_by_column, strip_terminal_sequences, truncate_to_width,
    visible_width, wrap_text_with_ansi,
};

// ---------------------------------------------------------------------------
// wheel-scroll.ts
// ---------------------------------------------------------------------------
pub use crate::wheel::WheelScrollLines;
