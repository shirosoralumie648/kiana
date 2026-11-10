//! kiana-logo.ts。
//!
//! 半块像素组成的 kiana logo（4 列 × 2 行）与文字 wordmark。品牌色固定，不随主题变化，
//! 只随终端色彩模式（truecolor / 256）变化。
//!
//! TS 源：`packages/coding-agent/src/modes/interactive/components/kiana-logo.ts`。
//!
//! 与 TS 的差异：`theme.getColorMode()` 在 kiana-theme 里返回 `Result`；主题未初始化时 TS 抛异常，
//! 这里用 `expect` 复现（宿主总是先 `initTheme`）。
//!
//! # 图形与颜色
//!
//! logo 是 4 列 × 2 行的半块像素图：上排用 `▀`（U+2580，上半块）配蓝色背景画出第一格，
//! 其余用珊瑚色前景；下排用 `█`（U+2588，整块）与 `▀` 拼出蓝色和黄色。上排第四个格子没有
//! 像素，用空格补齐，使两行可见宽度都是 4 列。颜色是固定的品牌 RGB 常量（不随主题 token
//! 变化），经 `foreground_ansi` / `background_ansi` 按**当前终端色彩模式**
//! （truecolor / 256）编码，因此每次调用都会读取全局主题的 `get_color_mode()`；主题
//! 未初始化时 panic（与 TS 抛异常一致）。
//!
//! # 调用方
//!
//! `kiana-interactive` 的启动 header（`header_with_logo`）在两行 logo 模式下拼接
//! [`kiana_logo_lines`]，否则用 [`kiana_wordmark`]；`supports_kiana_logo` 决定用哪一种
//! （Apple Terminal 上半块字符会错位）。`kiana-interactive/src/components/index.rs` 再导出
//! 三个函数。

use kiana_tui::colors::{Color, TerminalColorMode, background_ansi, foreground_ansi, rgb_color};
use kiana_tui::terminal::is_apple_terminal_session;

/// 品牌色（不随主题变化，只随终端色彩模式变化）；用于 logo 上排与 wordmark 的 `K`。
fn coral() -> Color {
    rgb_color(228.0, 138.0, 122.0)
}

/// logo 第一格的背景色（蓝色上半块）与下排左侧的前景色。
fn blue() -> Color {
    rgb_color(79.0, 142.0, 179.0)
}

/// wordmark 的 `iana` 与 logo 下排右侧一格的颜色。
fn yellow() -> Color {
    rgb_color(234.0, 182.0, 93.0)
}

/// SGR 全量重置。每段着色后立即关闭，防止颜色泄漏到同行的后续文本。
const RESET: &str = "\x1b[0m";

/// 当前主题的终端色彩模式（TS `theme.getColorMode()`）。
fn color_mode() -> TerminalColorMode {
    kiana_theme::THEME
        .get_color_mode()
        .expect("theme must be initialized before rendering (TS throws here)")
}

/// `kianaLogoLines()`：两行、每行 4 个字符格。
///
/// 顶行第四个格子为空，因此补一个空格使两行等宽。
///
/// 返回值固定两行，每行都是完整的 ANSI 字符串、可见宽度恰好 4 列；调用方直接按
/// `[top, bottom]` 使用。色彩模式在调用时读取一次，两行共用。
pub fn kiana_logo_lines() -> [String; 2] {
    let mode = color_mode();
    let fg = |color: Color| foreground_ansi(color, mode);
    let top = format!(
        "{}{}▀{}{}▀█{} ",
        fg(coral()),
        background_ansi(blue(), mode),
        RESET,
        fg(coral()),
        RESET
    );
    let bottom = format!("{}█▀{} {}█{}", fg(blue()), RESET, fg(yellow()), RESET);
    [top, bottom]
}

/// `supportsKianaLogo()`：Apple Terminal 行间有缝、半块错位，改用文字 wordmark。
///
/// 判定条件是 `kiana_tui::terminal::is_apple_terminal_session()`：仅 macOS 且环境变量
/// `TERM_PROGRAM=Apple_Terminal` 时为真；其它平台恒为 `false`。
pub fn supports_kiana_logo() -> bool {
    !is_apple_terminal_session()
}

/// `kianaWordmark()`：logo 珊瑚色 / 黄色的文字版 "Kiana"。
///
/// 字面 `Kiana`：`K` 为珊瑚色，`iana` 为黄色，可见宽度 5 列；按当前色彩模式编码。
pub fn kiana_wordmark() -> String {
    let mode = color_mode();
    format!(
        "{}K{}{}iana{}",
        foreground_ansi(coral(), mode),
        RESET,
        foreground_ansi(yellow(), mode),
        RESET
    )
}
