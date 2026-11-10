//! theme.ts（1086 行）：Theme 类、加载、全局单例、TUI 主题工厂、highlightCode。
//!
//! TS: `packages/coding-agent/src/modes/interactive/theme/theme.ts`。
//!
//! 结构（与 TS 一一对应）：
//!
//! 1. 类型与全局 JSON 校验器钩子（[`ThemeJsonValidator`]）；
//! 2. 颜色工具：变量解析、token fallback、终端默认色猜测、外观检测；
//! 3. [`Theme`] 类：预计算 ANSI、`colors` 缓存（按 `Arc<TerminalColors>` 身份）、
//!    `fg`/`bg`/`style`/chalk 风格属性；
//! 4. 主题加载：内置（`include_str!`）、已注册、自定义目录、文件名校验、错误文案；
//! 5. 全局单例：TS 用 `globalThis[Symbol.for(...)]` + `Proxy`（node/jiti 双模块加载器
//!    共享同一实例）。Rust 用 `RwLock<Option<Arc<Theme>>>` + [`ThemeProxy`]；
//! 6. 热重载：`watchWithErrorHandler` + 100 ms 去抖（spec §2.2.10）；
//! 7. HTML 导出辅助、CLI 语法高亮主题、TUI 主题工厂。
//!
//! 与 TS 的实现差异（都是语言约束下的等价改写，逐条在对应位置说明）：
//!
//! - TS 的 `theme` 是 `Proxy`：任何属性访问在未初始化时抛
//!   `Theme not initialized. Call initTheme() first.`。Rust 的 [`ThemeProxy`] 方法返回
//!   `Result<_, ThemeError>`；`get_markdown_theme()` 等工厂返回的闭包在全局主题缺失时
//!   原样返回文本（闭包无法抛错；生产路径总是先 `initTheme()`）。`Theme` 缺少某个 token
//!   时 `fg()/bg()` 退化为终端默认色，[`Theme::try_fg`]/[`Theme::try_bg`] 与代理方法给出
//!   TS 的 `Unknown theme color: {token}`。
//! - 热重载的 100 ms 去抖用「后台线程 + mpsc + deadline」实现（TS 用 `setTimeout`）；
//!   过期定时器靠全局 generation + `currentThemeName` 复查忽略。
//! - `getAvailableThemesWithPaths()` 的排序在 TS 里用 `localeCompare`；这里调用
//!   `kiana_tui::autocomplete::locale_compare`（与 Node 26 / ICU 默认 locale 的 collation
//!   逐位一致，以保证主题列表排序在平台间稳定，见 kiana-tui 的 localeCompare 说明）。
//! - `parseThemeJsonContent()` 的 `JSON.parse` 走 [`kiana_json::js_parse::parse`]（V8 的
//!   position/line/column、片段规则、整串特例与错误文案逐字一致，保证解析错误文案稳定）。它接受 `1e999`
//!   （JS 解析成 Infinity）与孤立代理项转义；按 ARCHITECTURE §8 第 31/32 条的已决偏差，
//!   前者存成 `null`、后者替换成 U+FFFD（Rust `String`/`serde_json::Number` 无法表示）。
//! - chalk 在 level 0（非 TTY）返回原文；Rust 侧与 kiana-tui 的样式函数一样总是输出 ANSI
//!   （交互式 TUI 运行在 TTY 上，chalk 的 level 也是 >= 1）。level >= 1 的换行/内嵌关闭
//!   序列语义逐字复刻（`chalk_wrap`）。
//! - `ThemeJson` 的颜色解析发生在 `create_theme()`（TS 的 `createTheme()`），因此
//!   `get_theme_export_colors()` 不会因为颜色的变量引用损坏而返回 `{}`（见 theme_schema）。
//! - 未校验文档（没有安装校验器时）里的 `name`/`appearance` 可以是任意 JSON 值。TS 把它们
//!   原样存进 `Theme`，`theme.colors` 在 `appearance` 不是 `"dark"`/`"light"` 时还会抛
//!   `Cannot read properties of undefined (reading 'foreground')`。Rust 的
//!   [`Theme::name`]/[`Theme::appearance`] 是类型化访问器（原始值见 [`Theme::raw_name`]/
//!   [`Theme::raw_appearance`]，[`is_light_theme`] 按原始值做 `=== "light"`），
//!   [`Theme::colors`] 是无错误的 getter，遇到非字面量外观时退回颜色检测与猜测的终端默认色。
//!   `get_available_themes_with_paths()` 也不会列出非字符串名字的主题（TS 会在排序的
//!   `localeCompare` 上抛 TypeError）。
//!
//! # 一次主题加载的执行流程
//!
//! 1. [`init_theme`] / [`set_theme`] 先把名字写入全局 `currentThemeName`，再调用私有的
//!    `load_theme()`。名字与主题实例由两把独立的锁保存，读取方可能短暂看到“新名字 + 旧实例”。
//! 2. `load_theme()` 的分支顺序固定：`system`（按当前终端颜色现生成）→ 已注册主题
//!    （[`set_registered_themes`] 登记，命中即复用同一个 `Arc<Theme>`）→ `load_theme_json()`
//!    （内置 → 已注册且带 `sourcePath` → 自定义目录 `<custom-themes-dir>/<name>.json`）。
//! 3. 一次加载依次做：读文件（自定义/已注册带路径的主题；内置主题直接解析 `include_str!`
//!    内容）→ 与 `JSON.parse` 等价的解析 → 可选校验器（仅磁盘上的主题；内置主题直接转换）
//!    → 颜色变量解析与 fallback 补全 → [`Theme::new`] 预计算 ANSI。
//! 4. 成功后替换全局主题（[`current_theme`] 可见）并可选启动 watcher；`set_theme` 在成功
//!    路径上同步触发通过 [`on_theme_change`] 注册的回调。失败时两个入口都回退到 `system`：
//!    `set_theme` 把错误文案放进 [`ThemeResult`]，`init_theme` 没有返回值，错误被丢弃。
//!
//! 导出路径（[`get_resolved_theme_colors`] / [`get_theme_export_colors`]）只读取主题（必要时
//! 读磁盘），不修改全局状态；省略名字时按“参数 → 当前名字 → `system`”选择。
//!
//! # 全局状态与并发
//!
//! 本模块的可变状态都是进程级静态量（对应 TS 的 `globalThis`）：全局主题、当前名字、
//! 已注册主题表、校验器与主题变化回调各由一把 `Mutex`/`RwLock` 保护。多数读取路径在锁内
//! 克隆 `Arc`/`String`/闭包后立即释放锁（回调不在锁内执行）；例外是
//! [`get_available_themes_with_paths`] 遍历注册表期间会一直持有读锁。内部锁都把 poison
//! 当作可用值（`unwrap_or_else(into_inner)`），一个线程 panic 不会让主题子系统永久不可用。
//!
//! 终端颜色以 `Arc<TerminalColors>` **整体替换**（[`set_terminal_colors`]），从不原地修改；
//! [`Theme::colors`] 以该 `Arc` 的身份作为缓存键，替换快照即失效。CLI 语法高亮主题则按
//! `Arc<Theme>` 的身份缓存，切换主题后的下一次 `highlightCode` 会重建。
//! 同时持有多个终端颜色快照的进程（例如测试）每切换一次快照都会让缓存重算（每个
//! [`Theme`] 只保留最近一次快照对应的解析结果）。
//!
//! 热重载在后台线程 `kiana-theme-watcher` 上运行：watcher 回调 → mpsc → 100 ms 去抖 →
//! 重新读文件并调用主题变化回调。因此 [`on_theme_change`] 注册的闭包可能在非主线程执行
//! （签名要求 `Send + Sync`）；[`stop_theme_watcher`] 只递增代号并丢弃 watcher，**不 join**
//! 去抖线程（见 `ThemeWatcher::drop` 的说明），线程在下一次 `recv` 返回时退出。
//!
//! # 修改时的约束
//!
//! - [`Theme::fg`] / [`Theme::style`] 的 ANSI 拼接和所有错误文案都与 TS 逐字对齐；本文件的
//!   `tests` 模块与 `tests/theme_style.rs` 断言具体字节，不能顺手“美化”。
//! - 新增或调整 token 必须同时维护 `theme_tokens.rs`：fallback 补全按 `THEME_TOKENS` 的
//!   声明顺序单遍执行，fallback 目标必须能在此之前解析（出现在主题数据里，或自身可被补全）。
//! - 全局状态是进程级的：同一进程内的并行测试必须串行化（见 `tests/common`）；生产代码
//!   不应假设 [`current_theme`] 与 [`current_theme_name`] 是同时更新的。

use std::any::Any;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::sync::{
    Arc, LazyLock, Mutex, MutexGuard, OnceLock, RwLock, RwLockReadGuard, RwLockWriteGuard,
};
use std::time::{Duration, Instant};

use indexmap::IndexMap;
use kiana_tui::colors::style_text_with_parts;
use kiana_tui::terminal_image::get_terminal_color_mode_default;
use kiana_tui::{
    Color, ColorMixSpace, EditorTheme, MarkdownTheme, SelectListTheme, SettingsListTheme,
    TerminalColorMode, TerminalColorScheme, TerminalColors, TextAttributes, background_ansi,
    color_to_hex, color_to_oklch, foreground_ansi, indexed_color, mix_colors, parse_color,
    rgb_color,
};

use crate::assets::{DARK_JSON, LIGHT_JSON};
use crate::system_theme::{SystemThemeInput, generate_system_theme_colors, terminal_appearance};
use crate::theme_schema::{
    ThemeColorValue, ThemeError, ThemeJson, is_object_prototype_key, js_trim, resolve_theme_colors,
    resolve_theme_colors_for_create, resolve_theme_export_from_document, theme_json_from_value,
};
use crate::theme_tokens::{THEME_TOKENS, ThemeBg, ThemeColor, ThemeToken};

use kiana_core::config::{get_custom_themes_dir, get_themes_dir};

/// TS: `export { SYSTEM_THEME_NAME } from "./system-theme.ts"`.
pub use crate::system_theme::SYSTEM_THEME_NAME;

// =============================================================================
// Types & schema
// =============================================================================

/// TS: `type ThemeAppearance = TerminalTheme`（主题面向的终端外观）。
pub type ThemeAppearance = TerminalColorScheme;

/// TS: `type TerminalTheme = "dark" | "light"`。
pub type TerminalTheme = TerminalColorScheme;

/// TS: `ThemeJsonValidator = (label, json) => ThemeJson`（`main.ts` 安装
/// [`crate::theme_schema::validate_theme_json`]，避免只使用内置主题的进程为 typebox 付费）。
pub type ThemeJsonValidator =
    Box<dyn Fn(&str, &serde_json::Value) -> Result<ThemeJson, ThemeError> + Send + Sync>;

/// [`ThemeJsonValidator`] 的内部共享形态（读锁下克隆，调用时不持锁）。
type SharedThemeJsonValidator =
    Arc<dyn Fn(&str, &serde_json::Value) -> Result<ThemeJson, ThemeError> + Send + Sync>;

/// TS: `ThemeStyle extends TextAttributes { fg?: ThemeColor | Color; bg?: ThemeBg | Color }`。
///
/// token 只允许出现在自己的 slot 里，因为 `""`（终端默认）在 fg/bg 上含义不同。想用某个
/// token 的颜色作为另一种 slot 时用 `theme.colors[token]`（Rust：`theme.color(token)`）。
///
/// 链式构造：每个方法消费并返回 `Self`（`ThemeStyle::new().bold().fg(ThemeColor::Accent)`）。
/// `fg`/`bg` 为 `None` 时不输出对应序列；属性只有 `Some(true)` 才开启，`None`/`Some(false)`
/// 都不输出（与 `kiana_tui` 的 `TextAttributes` 语义一致）。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ThemeStyle {
    /// 六个文本属性（bold/dim/italic/underline/inverse/strikethrough）。
    pub attributes: TextAttributes,
    /// 前景：token、具体颜色，或运行期 token 名。
    pub fg: Option<ThemeFg>,
    /// 背景：token、具体颜色，或运行期 token 名。
    pub bg: Option<ThemeBgValue>,
}

/// [`ThemeStyle::fg`] 的取值（TS: `ThemeColor | Color`，另加运行期名字）。
#[derive(Debug, Clone, PartialEq)]
pub enum ThemeFg {
    /// 前景 token。
    Token(ThemeColor),
    /// 具体颜色（`theme.colors[token]`）。
    Color(Color),
    /// 运行期 token 名：TS 的字符串键 API（`{ fg: "accent" }`）会把未知 token 或
    /// 错误 slot 的 token 变成 `Unknown theme color: {token}`；Rust 用这个变体保留同一
    /// 运行时检查。
    Named(&'static str),
}

/// [`ThemeStyle::bg`] 的取值（TS: `ThemeBg | Color`，另加运行期名字）。
#[derive(Debug, Clone, PartialEq)]
pub enum ThemeBgValue {
    /// 背景 token。
    Token(ThemeBg),
    /// 具体颜色。
    Color(Color),
    /// 运行期 token 名（同 [`ThemeFg::Named`]）。
    Named(&'static str),
}

impl From<ThemeColor> for ThemeFg {
    fn from(value: ThemeColor) -> Self {
        ThemeFg::Token(value)
    }
}

impl From<Color> for ThemeFg {
    fn from(value: Color) -> Self {
        ThemeFg::Color(value)
    }
}

impl From<&'static str> for ThemeFg {
    fn from(value: &'static str) -> Self {
        ThemeFg::Named(value)
    }
}

impl From<ThemeBg> for ThemeBgValue {
    fn from(value: ThemeBg) -> Self {
        ThemeBgValue::Token(value)
    }
}

impl From<Color> for ThemeBgValue {
    fn from(value: Color) -> Self {
        ThemeBgValue::Color(value)
    }
}

impl From<&'static str> for ThemeBgValue {
    fn from(value: &'static str) -> Self {
        ThemeBgValue::Named(value)
    }
}

impl ThemeStyle {
    /// 空样式。
    pub fn new() -> Self {
        ThemeStyle::default()
    }

    /// 设置前景。
    pub fn fg(mut self, fg: impl Into<ThemeFg>) -> Self {
        self.fg = Some(fg.into());
        self
    }

    /// 设置背景。
    pub fn bg(mut self, bg: impl Into<ThemeBgValue>) -> Self {
        self.bg = Some(bg.into());
        self
    }

    /// `bold: true`。
    pub fn bold(mut self) -> Self {
        self.attributes.bold = Some(true);
        self
    }

    /// `dim: true`。
    pub fn dim(mut self) -> Self {
        self.attributes.dim = Some(true);
        self
    }

    /// `italic: true`。
    pub fn italic(mut self) -> Self {
        self.attributes.italic = Some(true);
        self
    }

    /// `underline: true`。
    pub fn underline(mut self) -> Self {
        self.attributes.underline = Some(true);
        self
    }

    /// `inverse: true`。
    pub fn inverse(mut self) -> Self {
        self.attributes.inverse = Some(true);
        self
    }

    /// `strikethrough: true`。
    pub fn strikethrough(mut self) -> Self {
        self.attributes.strikethrough = Some(true);
        self
    }
}

/// TS: `setThemeJsonValidator(validator)`。
///
/// 进程级、最后安装者生效。加载路径在调用前先从锁里克隆 `Arc`，因此并发替换不会影响已经
/// 开始的这次解析。
pub fn set_theme_json_validator(validator: ThemeJsonValidator) {
    *write(&THEME_JSON_VALIDATOR) = Some(Arc::from(validator));
}

/// 卸载 JSON 校验器（Rust 测试隔离用；TS 没有对应 API）。
///
/// 卸载后 `parseThemeJson()` 退化为“只要求一个带 `colors` 对象”的快速检查。
pub fn clear_theme_json_validator() {
    *write(&THEME_JSON_VALIDATOR) = None;
}

static THEME_JSON_VALIDATOR: LazyLock<RwLock<Option<SharedThemeJsonValidator>>> =
    LazyLock::new(|| RwLock::new(None));

// =============================================================================
// Color utilities
// =============================================================================

/// TS: `withThemeTokenFallbacks(values)` —— 按 `THEME_TOKENS` 声明顺序补全 fallback。
///
/// 返回新的 map（不修改入参）。fallback 本身缺失时抛
/// `Theme token {name} has unresolved fallback {fallback}`。
///
/// 补全是**单遍**的：要求 fallback 目标要么已经出现在入参里，要么在 `THEME_TOKENS` 中先于
/// 使用者声明且自身能在那一轮之前被补全（当前表满足；若新增反向引用，这里会误报
/// unresolved）。补进来的 token 追加在 map 末尾，因此会影响 [`Theme::colors`] 的迭代顺序。
pub(crate) fn with_theme_token_fallbacks(
    values: IndexMap<ThemeToken, ThemeColorValue>,
) -> Result<IndexMap<ThemeToken, ThemeColorValue>, ThemeError> {
    let mut resolved = values;
    for (index, descriptor) in THEME_TOKENS.iter().enumerate() {
        let Some(fallback) = descriptor.fallback else {
            continue;
        };
        let token = ThemeToken::ALL[index];
        if resolved.contains_key(&token) {
            continue;
        }
        let Some(value) = resolved.get(&fallback).cloned() else {
            return Err(ThemeError::new(format!(
                "Theme token {token} has unresolved fallback {fallback}"
            )));
        };
        resolved.insert(token, value);
    }
    Ok(resolved)
}

/// TS: `splitThemeColors(colors)` —— 按 token slot 拆成前景/背景两个 map。
///
/// 两个 map 各自保留入参里的相对顺序；`Theme::new` 依赖这一点把前景 token 排在背景之前
/// 写入 `concrete_colors`。
fn split_theme_colors(
    colors: IndexMap<ThemeToken, ThemeColorValue>,
) -> (
    IndexMap<ThemeToken, ThemeColorValue>,
    IndexMap<ThemeToken, ThemeColorValue>,
) {
    let mut foregrounds = IndexMap::with_capacity(colors.len());
    let mut backgrounds = IndexMap::new();
    for (token, value) in colors {
        if token.is_background() {
            backgrounds.insert(token, value);
        } else {
            foregrounds.insert(token, value);
        }
    }
    (foregrounds, backgrounds)
}

/// TS: `GUESSED_DEFAULT_COLORS`（终端没有上报默认色时的猜测）。
///
/// 深色外观猜测 `#e5e5e7` 前景 + 黑色背景，浅色外观猜测黑色前景 + 白色背景。
fn guessed_default_colors(appearance: TerminalColorScheme) -> (Color, Color) {
    match appearance {
        TerminalColorScheme::Dark => (
            // parseColor("#e5e5e7")
            Color::Rgb {
                r: 229.0,
                g: 229.0,
                b: 231.0,
            },
            Color::Rgb {
                r: 0.0,
                g: 0.0,
                b: 0.0,
            },
        ),
        TerminalColorScheme::Light => (
            Color::Rgb {
                r: 0.0,
                g: 0.0,
                b: 0.0,
            },
            Color::Rgb {
                r: 255.0,
                g: 255.0,
                b: 255.0,
            },
        ),
    }
}

/// TS: `averageLightness(colors)` —— 忽略 ANSI 0-15（它们跟随用户调色板）。
///
/// 亮度取 OKLCH 的 `l`；过滤后没有剩余颜色时返回 `None`（调用方据此跳过该维度的判断）。
fn average_lightness(colors: &[Color]) -> Option<f64> {
    let fixed: Vec<Color> = colors
        .iter()
        .copied()
        .filter(|color| !matches!(color, Color::Indexed { index } if *index < 16))
        .collect();
    if fixed.is_empty() {
        return None;
    }
    Some(
        fixed
            .iter()
            .map(|color| color_to_oklch(*color).l)
            .sum::<f64>()
            / fixed.len() as f64,
    )
}

/// TS: `detectAppearance(foregrounds, backgrounds)`。
///
/// 前景、背景都可用时按“背景比前景暗 = 深色”判断；只有背景时按亮度 0.5 分界；只有前景时按
/// “前景亮 = 深色”（深色背景需要亮文字）；两者都不可用返回 `None`，由调用方退回终端外观。
fn detect_appearance(foregrounds: &[Color], backgrounds: &[Color]) -> Option<TerminalColorScheme> {
    let foreground = average_lightness(foregrounds);
    let background = average_lightness(backgrounds);
    match (foreground, background) {
        (Some(foreground), Some(background)) => Some(if background < foreground {
            TerminalColorScheme::Dark
        } else {
            TerminalColorScheme::Light
        }),
        (None, Some(background)) => Some(if background < 0.5 {
            TerminalColorScheme::Dark
        } else {
            TerminalColorScheme::Light
        }),
        (Some(foreground), None) => Some(if foreground > 0.5 {
            TerminalColorScheme::Dark
        } else {
            TerminalColorScheme::Light
        }),
        (None, None) => None,
    }
}

// =============================================================================
// Appearance & terminal default colors
// =============================================================================

static TERMINAL_COLORS: LazyLock<RwLock<Arc<TerminalColors>>> =
    LazyLock::new(|| RwLock::new(Arc::new(TerminalColors::default())));
static TERMINAL_COLORS_PENDING: AtomicBool = AtomicBool::new(false);
static TERMINAL_COLOR_SCHEME: LazyLock<Mutex<Option<TerminalColorScheme>>> =
    LazyLock::new(|| Mutex::new(None));

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn read<T>(lock: &RwLock<T>) -> RwLockReadGuard<'_, T> {
    lock.read().unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn write<T>(lock: &RwLock<T>) -> RwLockWriteGuard<'_, T> {
    lock.write()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// 当前记录的终端颜色（整体替换，从不原地修改 —— 主题按 `Arc` 身份缓存）。
///
/// 返回当前快照的克隆；拿到的 `Arc<TerminalColors>` 之后不会随新上报而改变。
pub fn terminal_colors() -> Arc<TerminalColors> {
    Arc::clone(&read(&TERMINAL_COLORS))
}

/// TS: `setTerminalColors(colors)` —— 记录终端上报的颜色，结束 pending 状态。
///
/// 替换整个快照（新 `Arc`），因此 [`Theme::colors`] 按 `Arc` 身份缓存的解析结果随之失效。
pub fn set_terminal_colors(colors: &TerminalColors) {
    *write(&TERMINAL_COLORS) = Arc::new(colors.clone());
    TERMINAL_COLORS_PENDING.store(false, Ordering::SeqCst);
}

/// 当前记录的终端明暗报告（mode 2031）。
pub fn terminal_color_scheme() -> Option<TerminalColorScheme> {
    *lock(&TERMINAL_COLOR_SCHEME)
}

/// TS: `setTerminalColorScheme(scheme)`。
pub fn set_terminal_color_scheme(scheme: Option<TerminalColorScheme>) {
    *lock(&TERMINAL_COLOR_SCHEME) = scheme;
}

/// 终端颜色查询是否在途（system 主题在此期间渲染为灰阶）。
///
/// 由 [`mark_terminal_colors_pending`] 置位、[`set_terminal_colors`] 清除；`system` 主题在
/// 置位期间用 `saturation = 0.0` 生成（见 `create_system_theme`）。
pub fn terminal_colors_pending() -> bool {
    TERMINAL_COLORS_PENDING.load(Ordering::SeqCst)
}

/// TS: `markTerminalColorsPending()`。
pub fn mark_terminal_colors_pending() {
    TERMINAL_COLORS_PENDING.store(true, Ordering::SeqCst);
}

// =============================================================================
// Theme class
// =============================================================================

/// TS: `Theme` 构造选项（`constructor(..., options = {})`）。
#[derive(Debug, Clone, Default)]
pub struct ThemeOptions {
    /// 主题名。
    pub name: Option<String>,
    /// 主题文件路径。
    pub source_path: Option<PathBuf>,
    /// 资源配置来源信息（`kiana-resources` 写入）。
    pub source_info: Option<kiana_core::source_info::SourceInfo>,
    /// 声明的终端外观（省略时由颜色推导）。
    pub appearance: Option<TerminalColorScheme>,
    /// 原始 `name` JSON 值（TS `createTheme()` 原样保存；只用于 [`Theme::raw_name`]）。
    pub raw_name: Option<serde_json::Value>,
    /// 原始 `appearance` JSON 值（TS `Theme.appearance` 原样返回；只用于
    /// [`Theme::raw_appearance`] 与 `isLightTheme()` 的比较）。
    pub raw_appearance: Option<serde_json::Value>,
    /// 渲染为 SGR 2 的前景 token。
    pub dim: Vec<ThemeColor>,
}

/// `Theme::colors` 缓存的具体颜色表。
type ResolvedColors = Arc<IndexMap<ThemeToken, Color>>;

/// `Theme::colors` 的缓存：终端颜色整体替换，`Arc` 身份变化即失效。
type ResolvedColorsCache = Mutex<Option<(Arc<TerminalColors>, ResolvedColors)>>;

/// 一个已加载的主题。
///
/// TS: `class Theme`。`fg()`/`bg()` 在渲染热路径上只做一次查表 + 拼接，因此 ANSI 序列在
/// 构造时预计算；`colors` 按 `Arc<TerminalColors>` 身份缓存（终端颜色整体替换，身份变化即
/// 失效）。
#[derive(Debug)]
pub struct Theme {
    name: Option<String>,
    source_path: Option<PathBuf>,
    source_info: Mutex<Option<kiana_core::source_info::SourceInfo>>,
    mode: TerminalColorMode,
    fg_ansi: IndexMap<ThemeColor, String>,
    bg_ansi: IndexMap<ThemeBg, String>,
    /// `""` token 没有自己的颜色；`colors` 用终端默认色填充它们。
    concrete_colors: IndexMap<ThemeToken, Color>,
    default_foreground_tokens: Vec<ThemeColor>,
    default_background_tokens: Vec<ThemeBg>,
    /// 渲染为 SGR 2 的前景 token。
    dim_tokens: HashSet<ThemeColor>,
    own_appearance: Option<TerminalColorScheme>,
    /// TS `Theme.name` 的原始 JSON 值（未校验路径下可以是非字符串）。
    raw_name: Option<serde_json::Value>,
    /// TS `Theme.appearance` 的原始 JSON 值（未校验路径下可以是非字面量）。
    raw_appearance: Option<serde_json::Value>,
    resolved_colors: ResolvedColorsCache,
}

impl Theme {
    /// TS: `constructor(fgColors, bgColors, mode, options = {})`。
    ///
    /// `fgColors`/`bgColors` 是已解析变量引用的 token 值；fallback 会在这里再补一次
    /// （与 TS 一样，`createTheme` 与构造函数各调用一次 `withThemeTokenFallbacks`）。
    /// 颜色字符串无法解析时返回 TS `parseColor()` 的逐字原因（`Invalid color value: …`、
    /// `s must be between 0 and 1: …`、`l must be finite` 等，见 `invalid_color_error`）。
    ///
    /// `ThemeOptions::appearance` 为 `None` 时，构造末尾用**非空** token 的具体颜色推断外观
    /// （空字符串 token 表示终端默认色，不参与推断）。`options.dim` 里的前景 token 在
    /// `fg()`/`style()` 输出时追加 SGR 2；`raw_name`/`raw_appearance` 原样保存。
    pub fn new(
        fg_colors: IndexMap<ThemeToken, ThemeColorValue>,
        bg_colors: IndexMap<ThemeToken, ThemeColorValue>,
        mode: TerminalColorMode,
        options: ThemeOptions,
    ) -> Result<Theme, ThemeError> {
        // 阶段 1：合并两个 slot 的 token 表（前景在前、背景在后），下面再按 slot 拆回。
        let mut merged = IndexMap::with_capacity(fg_colors.len() + bg_colors.len());
        for (token, value) in fg_colors {
            merged.insert(token, value);
        }
        for (token, value) in bg_colors {
            merged.insert(token, value);
        }
        // 阶段 2：按 THEME_TOKENS 声明顺序补全 fallback；缺失的 fallback 目标让构造失败。
        let values = with_theme_token_fallbacks(merged)?;
        let (foregrounds, backgrounds) = split_theme_colors(values);

        // 阶段 3：逐个 token 预计算 ANSI；空字符串 token 只登记为“终端默认色”，不进入
        // concrete_colors（`colors()` 稍后用终端前景/背景填充它们）。
        let mut fg_ansi = IndexMap::with_capacity(foregrounds.len());
        let mut bg_ansi = IndexMap::with_capacity(backgrounds.len());
        let mut concrete_colors = IndexMap::new();
        let mut default_foreground_tokens = Vec::new();
        let mut default_background_tokens = Vec::new();
        let mut concrete_foregrounds = Vec::new();
        let mut concrete_backgrounds = Vec::new();

        for (token, value) in &foregrounds {
            let color = ThemeColor::from_token(*token)
                .ok_or_else(|| ThemeError::new(format!("Unknown theme color: {token}")))?;
            let ansi = add_token(
                *token,
                value,
                false,
                mode,
                &mut concrete_colors,
                &mut concrete_foregrounds,
                &mut default_foreground_tokens,
            )?;
            fg_ansi.insert(color, ansi);
        }
        for (token, value) in &backgrounds {
            let color = ThemeBg::from_token(*token)
                .ok_or_else(|| ThemeError::new(format!("Unknown theme color: {token}")))?;
            let ansi = add_token(
                *token,
                value,
                true,
                mode,
                &mut concrete_colors,
                &mut concrete_backgrounds,
                &mut default_background_tokens,
            )?;
            bg_ansi.insert(color, ansi);
        }

        // 阶段 4：声明的 appearance 优先；否则用具体前景/背景的 OKLCH 平均亮度推断。
        let own_appearance = options
            .appearance
            .or_else(|| detect_appearance(&concrete_foregrounds, &concrete_backgrounds));
        // 阶段 5：dim 名单转成集合；默认色 token 名单按槽位转成 ThemeColor/ThemeBg
        // （防御性地丢弃错槽位的 token；正常路径不会出现）。
        let dim_tokens: HashSet<ThemeColor> = options.dim.iter().copied().collect();
        let default_foreground_tokens: Vec<ThemeColor> = default_foreground_tokens
            .iter()
            .filter_map(|token| ThemeColor::from_token(*token))
            .collect();
        let default_background_tokens: Vec<ThemeBg> = default_background_tokens
            .iter()
            .filter_map(|token| ThemeBg::from_token(*token))
            .collect();

        Ok(Theme {
            name: options.name,
            source_path: options.source_path,
            source_info: Mutex::new(options.source_info),
            mode,
            fg_ansi,
            bg_ansi,
            concrete_colors,
            default_foreground_tokens,
            default_background_tokens,
            dim_tokens,
            own_appearance,
            raw_name: options.raw_name,
            raw_appearance: options.raw_appearance,
            resolved_colors: Mutex::new(None),
        })
    }

    /// TS: `readonly name?: string`。
    pub fn name(&self) -> Option<&str> {
        self.name.as_deref()
    }

    /// TS: `theme.name` 的原始 JSON 值。
    ///
    /// 安装校验器时它总是字符串（或 `None`）；未校验的文档会原样传给 `createTheme()`，
    /// 因此 `"name": 5` 在 TS 里得到 `theme.name === 5`。Rust 的 [`Theme::name`] 只保留
    /// 字符串形态，这里提供原始值供调用方判断。
    pub fn raw_name(&self) -> Option<&serde_json::Value> {
        self.raw_name.as_ref()
    }

    /// TS: `theme.appearance` 的原始 JSON 值（未校验文档里可以是非字面量）。
    ///
    /// TS 的 getter 原样返回它并用 `=== "light"` 判断浅色主题（见
    /// [`is_light_theme`]）；[`Theme::appearance`] 只能返回 `dark`/`light`，此时退回颜色
    /// 推导的结果。
    pub fn raw_appearance(&self) -> Option<&serde_json::Value> {
        self.raw_appearance.as_ref()
    }

    /// TS: `readonly sourcePath?: string`。
    pub fn source_path(&self) -> Option<&Path> {
        self.source_path.as_deref()
    }

    /// TS: `sourceInfo?: SourceInfo`（`kiana-resources` 在注册前写入）。
    pub fn source_info(&self) -> Option<kiana_core::source_info::SourceInfo> {
        lock(&self.source_info).clone()
    }

    /// 写入来源信息（对应 TS 的直接字段赋值；Rust 用内部可变性保持 `Arc<Theme>` 共享）。
    pub fn set_source_info(&self, info: Option<kiana_core::source_info::SourceInfo>) {
        *lock(&self.source_info) = info;
    }

    /// TS: `get appearance()` —— 声明 / 从颜色推导 / 终端外观。
    ///
    /// 优先级：构造时传入的 `ThemeOptions::appearance` → 从主题自身颜色推断的结果 →
    /// 终端当前外观（[`get_terminal_theme`]，由已上报的终端颜色/明暗决定）。
    pub fn appearance(&self) -> TerminalColorScheme {
        self.own_appearance.unwrap_or_else(get_terminal_theme)
    }

    /// TS: `get colors()` —— 全部 token 的具体颜色（按 token 顺序缓存）。
    ///
    /// 计算顺序：终端默认前景/背景（终端未上报时按 `appearance()` 猜测，见
    /// `guessed_default_colors`）→ 用它们填充空字符串 token → 把 dim token 与终端背景在
    /// OKLCH 空间按 `mix_colors(..., 0.4, Oklch)` 混合后替换。
    ///
    /// 缓存键是 `Arc<TerminalColors>` 的**身份**（[`set_terminal_colors`] 整体替换快照，
    /// 因此替换即失效）；缓存互斥锁只在读写结果时持有，计算过程中不持锁。返回的是不可变
    /// 快照，调用方可以跨主题切换持有它。
    ///
    /// 映射的迭代顺序不是 `THEME_TOKENS` 的声明顺序：构造时全部前景 token 在前、背景 token
    /// 在后（各自沿用主题文档 `colors` 的相对顺序），fallback 补全项追加在各自槽位段的末尾，
    /// 空字符串 token 追加在整张表末尾。[`get_resolved_theme_colors`] 的返回顺序直接继承它。
    pub fn colors(&self) -> Arc<IndexMap<ThemeToken, Color>> {
        let terminal = terminal_colors();
        {
            let resolved = lock(&self.resolved_colors);
            if let Some((cached_terminal, colors)) = resolved.as_ref()
                && Arc::ptr_eq(cached_terminal, &terminal)
            {
                return Arc::clone(colors);
            }
        }
        let (guess_foreground, guess_background) = guessed_default_colors(self.appearance());
        let foreground = terminal
            .foreground
            .map(|rgb| rgb_color(f64::from(rgb.r), f64::from(rgb.g), f64::from(rgb.b)))
            .unwrap_or(guess_foreground);
        let background = terminal
            .background
            .map(|rgb| rgb_color(f64::from(rgb.r), f64::from(rgb.g), f64::from(rgb.b)))
            .unwrap_or(guess_background);
        let mut colors = self.concrete_colors.clone();
        for token in &self.default_foreground_tokens {
            colors.insert(token.token(), foreground);
        }
        for token in &self.default_background_tokens {
            colors.insert(token.token(), background);
        }
        for token in &self.dim_tokens {
            if let Some(color) = colors.get(&token.token()).copied() {
                colors.insert(
                    token.token(),
                    mix_colors(color, background, 0.4, ColorMixSpace::Oklch),
                );
            }
        }
        let colors = Arc::new(colors);
        *lock(&self.resolved_colors) = Some((terminal, Arc::clone(&colors)));
        colors
    }

    /// 单个 token 的具体颜色（`theme.colors[token]`）。
    ///
    /// 空字符串 token 返回终端默认色（由 [`Theme::colors`] 填充）；主题完全缺少该 token
    /// （绕过校验器构造的不完整主题）时返回 `None`，而 `fg()`/`bg()` 会退化到终端默认色。
    pub fn color(&self, token: ThemeToken) -> Option<Color> {
        self.colors().get(&token).copied()
    }

    /// TS: `style(text, options)`。
    ///
    /// 与 `styleTextWithAnsi(text, fgAnsi, bgAnsi, options)` 完全等价：token 的 ANSI 是查表
    /// 得到的（`Theme::style` 必须和 `styleText(..., theme.colors[…], mode)` 逐字节一致，这是 ANSI 输出的稳定契约）。
    ///
    /// 失败路径：`ThemeFg::Named`/`ThemeBgValue::Named` 的未知 token 或 slot 用错（例如把
    /// `userMessageBg` 当作前景）返回 `Unknown theme color: {token}`；`Token` 变体在主题缺
    /// 该 token 时同样报错。`Color` 变体总是成功。dim token 会强制把 `attributes.dim` 设为
    /// `Some(true)`，即使调用方传的是 `Some(false)`。
    pub fn style(&self, text: &str, options: &ThemeStyle) -> Result<String, ThemeError> {
        let mut attributes = options.attributes;
        let fg_ansi = match &options.fg {
            None => None,
            Some(ThemeFg::Token(color)) => {
                if self.dim_tokens.contains(color) {
                    attributes.dim = Some(true);
                }
                Some(self.try_fg_ansi_for(*color)?)
            }
            Some(ThemeFg::Color(color)) => Some(foreground_ansi(*color, self.mode)),
            Some(ThemeFg::Named(name)) => {
                let color = ThemeColor::from_str(name)
                    .ok_or_else(|| ThemeError::new(format!("Unknown theme color: {name}")))?;
                let ansi = self
                    .fg_ansi
                    .get(&color)
                    .cloned()
                    .ok_or_else(|| ThemeError::new(format!("Unknown theme color: {name}")))?;
                if self.dim_tokens.contains(&color) {
                    attributes.dim = Some(true);
                }
                Some(ansi)
            }
        };
        let bg_ansi = match &options.bg {
            None => None,
            Some(ThemeBgValue::Token(color)) => Some(self.try_bg_ansi_for(*color)?),
            Some(ThemeBgValue::Color(color)) => Some(background_ansi(*color, self.mode)),
            Some(ThemeBgValue::Named(name)) => {
                let color = ThemeBg::from_str(name)
                    .ok_or_else(|| ThemeError::new(format!("Unknown theme color: {name}")))?;
                Some(
                    self.bg_ansi
                        .get(&color)
                        .cloned()
                        .ok_or_else(|| ThemeError::new(format!("Unknown theme color: {name}")))?,
                )
            }
        };
        Ok(style_text_with_parts(
            text,
            fg_ansi.as_deref(),
            bg_ansi.as_deref(),
            &attributes,
        ))
    }

    /// TS: `fg(color, text)` —— 前景 token 着色，dim token 追加 SGR 2。
    ///
    /// 主题缺少该 token（只在绕过校验器构造不完整主题时出现）会静默退化为终端默认前景；
    /// 需要 TS 的 `Unknown theme color: {token}` 错误时用 [`Theme::try_fg`]。
    pub fn fg(&self, color: ThemeColor, text: &str) -> String {
        let ansi = self.fg_ansi_for(color);
        if self.dim_tokens.contains(&color) {
            format!("{ansi}\x1b[2m{text}\x1b[22;39m")
        } else {
            format!("{ansi}{text}\x1b[39m")
        }
    }

    /// TS: `fg(color, text)` 的失败可见版本：缺少 token 时返回
    /// `Unknown theme color: {token}`（TS 的 `tokenAnsi()` 文案）。
    pub fn try_fg(&self, color: ThemeColor, text: &str) -> Result<String, ThemeError> {
        let ansi = self.try_fg_ansi_for(color)?;
        Ok(if self.dim_tokens.contains(&color) {
            format!("{ansi}\x1b[2m{text}\x1b[22;39m")
        } else {
            format!("{ansi}{text}\x1b[39m")
        })
    }

    /// TS: `bg(color, text)` —— 背景 token 着色。
    pub fn bg(&self, color: ThemeBg, text: &str) -> String {
        format!("{}{text}\x1b[49m", self.bg_ansi_for(color))
    }

    /// [`Theme::bg`] 的失败可见版本。
    pub fn try_bg(&self, color: ThemeBg, text: &str) -> Result<String, ThemeError> {
        Ok(format!("{}{text}\x1b[49m", self.try_bg_ansi_for(color)?))
    }

    /// 运行期 token 名的 `fg()`（TS 的字符串键 API）；未知或错误 slot →
    /// `Unknown theme color: {token}`。
    pub fn fg_named(&self, token: &str, text: &str) -> Result<String, ThemeError> {
        let color = ThemeColor::from_str(token)
            .ok_or_else(|| ThemeError::new(format!("Unknown theme color: {token}")))?;
        let ansi = self
            .fg_ansi
            .get(&color)
            .cloned()
            .ok_or_else(|| ThemeError::new(format!("Unknown theme color: {token}")))?;
        Ok(if self.dim_tokens.contains(&color) {
            format!("{ansi}\x1b[2m{text}\x1b[22;39m")
        } else {
            format!("{ansi}{text}\x1b[39m")
        })
    }

    /// 运行期 token 名的 `bg()`。
    pub fn bg_named(&self, token: &str, text: &str) -> Result<String, ThemeError> {
        let color = ThemeBg::from_str(token)
            .ok_or_else(|| ThemeError::new(format!("Unknown theme color: {token}")))?;
        let ansi = self
            .bg_ansi
            .get(&color)
            .cloned()
            .ok_or_else(|| ThemeError::new(format!("Unknown theme color: {token}")))?;
        Ok(format!("{ansi}{text}\x1b[49m"))
    }

    /// TS: `getFgAnsi(color)` —— 前景 token 的开启序列（dim token 带 SGR 2，不关闭）。
    pub fn get_fg_ansi(&self, color: ThemeColor) -> String {
        let ansi = self.fg_ansi_for(color);
        if self.dim_tokens.contains(&color) {
            format!("{ansi}\x1b[2m")
        } else {
            ansi
        }
    }

    /// TS: `getBgAnsi(color)`。
    pub fn get_bg_ansi(&self, color: ThemeBg) -> String {
        self.bg_ansi_for(color)
    }

    /// [`Theme::get_fg_ansi`] 的失败可见版本。
    pub fn try_get_fg_ansi(&self, color: ThemeColor) -> Result<String, ThemeError> {
        let ansi = self.try_fg_ansi_for(color)?;
        Ok(if self.dim_tokens.contains(&color) {
            format!("{ansi}\x1b[2m")
        } else {
            ansi
        })
    }

    /// [`Theme::get_bg_ansi`] 的失败可见版本。
    pub fn try_get_bg_ansi(&self, color: ThemeBg) -> Result<String, ThemeError> {
        self.try_bg_ansi_for(color)
    }

    /// 运行期 token 名的 `getFgAnsi()`。
    pub fn get_fg_ansi_named(&self, token: &str) -> Result<String, ThemeError> {
        let color = ThemeColor::from_str(token)
            .ok_or_else(|| ThemeError::new(format!("Unknown theme color: {token}")))?;
        let ansi = self
            .fg_ansi
            .get(&color)
            .cloned()
            .ok_or_else(|| ThemeError::new(format!("Unknown theme color: {token}")))?;
        Ok(if self.dim_tokens.contains(&color) {
            format!("{ansi}\x1b[2m")
        } else {
            ansi
        })
    }

    /// 运行期 token 名的 `getBgAnsi()`。
    pub fn get_bg_ansi_named(&self, token: &str) -> Result<String, ThemeError> {
        let color = ThemeBg::from_str(token)
            .ok_or_else(|| ThemeError::new(format!("Unknown theme color: {token}")))?;
        self.bg_ansi
            .get(&color)
            .cloned()
            .ok_or_else(|| ThemeError::new(format!("Unknown theme color: {token}")))
    }

    /// TS: `bold(text)`（chalk：空串返回空串，关闭序列是 `\x1b[22m`）。
    pub fn bold(&self, text: &str) -> String {
        chalk_wrap("\x1b[1m", "\x1b[22m", text)
    }

    /// TS: `italic(text)`。
    pub fn italic(&self, text: &str) -> String {
        chalk_wrap("\x1b[3m", "\x1b[23m", text)
    }

    /// TS: `underline(text)`。
    pub fn underline(&self, text: &str) -> String {
        chalk_wrap("\x1b[4m", "\x1b[24m", text)
    }

    /// TS: `inverse(text)`。
    pub fn inverse(&self, text: &str) -> String {
        chalk_wrap("\x1b[7m", "\x1b[27m", text)
    }

    /// TS: `strikethrough(text)`。
    pub fn strikethrough(&self, text: &str) -> String {
        chalk_wrap("\x1b[9m", "\x1b[29m", text)
    }

    /// TS: `getColorMode()`。
    pub fn get_color_mode(&self) -> TerminalColorMode {
        self.mode
    }

    /// TS: `getThinkingBorderColor(level)` —— thinking 等级 → 边框着色函数。
    ///
    /// `level` 用 TS 的字符串字面量（`"off"`/`"minimal"`/…/`"max"`）；未知值走 default 分支
    /// （`thinkingOff`），与 TS 相同。Rust 不能依赖 `kiana-ai::ThinkingLevel`（依赖方向），
    /// 因此接受 `&str`。这个具体主题的闭包捕获预计算的 ANSI（与 TS 的 `this.fg` 等价）。
    pub fn get_thinking_border_color(
        &self,
        level: &str,
    ) -> Box<dyn Fn(&str) -> String + Send + Sync> {
        let color = thinking_border_color_for(level);
        let ansi = self.fg_ansi_for(color);
        let dim = self.dim_tokens.contains(&color);
        Box::new(move |text: &str| {
            if dim {
                format!("{ansi}\x1b[2m{text}\x1b[22;39m")
            } else {
                format!("{ansi}{text}\x1b[39m")
            }
        })
    }

    /// TS: `getBashModeBorderColor()`。
    pub fn get_bash_mode_border_color(&self) -> Box<dyn Fn(&str) -> String + Send + Sync> {
        let ansi = self.fg_ansi_for(ThemeColor::BashMode);
        Box::new(move |text: &str| format!("{ansi}{text}\x1b[39m"))
    }

    fn fg_ansi_for(&self, color: ThemeColor) -> String {
        // 不完整的主题（绕过校验器直接构造）在 TS 里会抛 `Unknown theme color`；类型化入口
        // 无法在每个调用点返回错误，这里退化成终端默认前景（`try_*` 提供失败可见的版本）。
        self.fg_ansi
            .get(&color)
            .cloned()
            .unwrap_or_else(|| "\x1b[39m".to_string())
    }

    fn bg_ansi_for(&self, color: ThemeBg) -> String {
        self.bg_ansi
            .get(&color)
            .cloned()
            .unwrap_or_else(|| "\x1b[49m".to_string())
    }

    fn try_fg_ansi_for(&self, color: ThemeColor) -> Result<String, ThemeError> {
        self.fg_ansi
            .get(&color)
            .cloned()
            .ok_or_else(|| ThemeError::new(format!("Unknown theme color: {}", color.as_str())))
    }

    fn try_bg_ansi_for(&self, color: ThemeBg) -> Result<String, ThemeError> {
        self.bg_ansi
            .get(&color)
            .cloned()
            .ok_or_else(|| ThemeError::new(format!("Unknown theme color: {}", color.as_str())))
    }
}

/// TS: `chalk.<style>(text)` 的等价物。
///
/// 复刻 chalk（level >= 1）的 `applyStyle()`：
///
/// 1. 空串返回空串；
/// 2. 文本含 ESC 时，把样式自己的关闭序列替换成「关闭 + 重新开启」（`replaceClose`），
///    避免内嵌关闭序列之后的内容掉色；
/// 3. 每个换行前关闭、换行后重新开启（`stringEncaseCRLFWithFirstIndex`，`\r\n` 保持完整）；
/// 4. 最后 `open + text + close`。
///
/// chalk 在 level 0（非 TTY）返回原文；Rust 侧与 kiana-tui 的样式函数一样总是输出 ANSI
/// （交互式 TUI 运行在 TTY 上，chalk 的 level 也是 >= 1），见模块文档。
fn chalk_wrap(open: &str, close: &str, text: &str) -> String {
    if text.is_empty() {
        return String::new();
    }
    let mut string = if text.contains('\u{1b}') {
        // chalk: `stringReplaceAll(string, styler.close, styler.open)`，即 close → close+open。
        text.replace(close, &format!("{close}{open}"))
    } else {
        text.to_string()
    };
    if string.contains('\n') {
        string = encase_crlf_with_first_index(&string, close, open);
    }
    format!("{open}{string}{close}")
}

/// chalk 的 `stringEncaseCRLFWithFirstIndex(string, prefix, postfix, index)`。
///
/// 在第一个 `\n` 起，每个换行前插入 `prefix`、换行后插入 `postfix`；`\r\n` 作为一个整体，
/// 前缀插在 `\r` 之前。
fn encase_crlf_with_first_index(string: &str, prefix: &str, postfix: &str) -> String {
    let bytes = string.as_bytes();
    let mut return_value = String::with_capacity(string.len() + 8);
    let mut end_index = 0usize;
    let mut index = string.find('\n');
    while let Some(position) = index {
        let is_cr = position > 0 && bytes[position - 1] == b'\r';
        return_value.push_str(&string[end_index..if is_cr { position - 1 } else { position }]);
        return_value.push_str(prefix);
        if is_cr {
            return_value.push_str("\r\n");
        } else {
            return_value.push('\n');
        }
        return_value.push_str(postfix);
        end_index = position + 1;
        index = string[end_index..].find('\n').map(|next| end_index + next);
    }
    return_value.push_str(&string[end_index..]);
    return_value
}

/// 一个 token 的 ANSI 预计算（`addToken`）。
fn add_token(
    token: ThemeToken,
    value: &ThemeColorValue,
    is_background: bool,
    mode: TerminalColorMode,
    concrete_colors: &mut IndexMap<ThemeToken, Color>,
    concrete: &mut Vec<Color>,
    defaults: &mut Vec<ThemeToken>,
) -> Result<String, ThemeError> {
    if value.is_empty() {
        defaults.push(token);
        return Ok(if is_background {
            "\x1b[49m".to_string()
        } else {
            "\x1b[39m".to_string()
        });
    }
    let color = match value {
        ThemeColorValue::Str(text) => parse_color(text).ok_or_else(|| invalid_color_error(text))?,
        ThemeColorValue::Index(index) => indexed_color(*index),
    };
    concrete_colors.insert(token, color);
    concrete.push(color);
    Ok(if is_background {
        background_ansi(color, mode)
    } else {
        foreground_ansi(color, mode)
    })
}

/// `parseColor()` 失败时的逐字原因（TS 的 `oklchColor`/`okhslColor` 抛出具体文案）。
///
/// `kiana_tui::parse_color()` 对非法 OKLCH/OKHSL 只返回 `None`（冻结签名），但 TS 的主题
/// 构造器直接调用 `parseColor()`，作者会看到 `s must be between 0 and 1: 1.5` 之类的
/// 具体原因。这里用同一组正则**重新分类**失败原因（只做文本分类，不复算颜色）。
///
/// 正则与 kiana-tui `colors.rs` 的 `OKLCH_PATTERN`/`OKHSL_PATTERN` 相同（`\s` 用 JS 的 25 个
/// 空白码点；`i` 是 ASCII-only 的旧式语义，逐字母展开大小写类）。
fn invalid_color_error(text: &str) -> ThemeError {
    const NUMBER: &str = r"[+-]?(?:[0-9]+(?:\.[0-9]*)?|\.[0-9]+)(?:[eE][+-]?[0-9]+)?";
    const SPACE: &str = r"[\t\n\x{0B}\x{0C}\r \x{00A0}\x{1680}\x{2000}-\x{200A}\x{2028}\x{2029}\x{202F}\x{205F}\x{3000}\x{FEFF}]";
    static OKLCH: LazyLock<regex::Regex> = LazyLock::new(|| {
        let source = format!(
            r"^[oO][kK][lL][cC][hH]\({SPACE}*({NUMBER})(%)?{SPACE}+({NUMBER}){SPACE}+({NUMBER})(?:[dD][eE][gG])?{SPACE}*\)$"
        );
        regex::Regex::new(&source).expect("oklch failure pattern compiles")
    });
    static OKHSL: LazyLock<regex::Regex> = LazyLock::new(|| {
        let source = format!(
            r"^[oO][kK][hH][sS][lL]\({SPACE}*({NUMBER})(?:[dD][eE][gG])?{SPACE}+({NUMBER})(%)?{SPACE}+({NUMBER})(%)?{SPACE}*\)$"
        );
        regex::Regex::new(&source).expect("okhsl failure pattern compiles")
    });

    // `Number.parseFloat()` 在正则已限定的输入上与 Rust 的 `f64` 解析一致（含 1e999 → ∞）。
    let parse = |capture: &str| -> f64 { capture.parse::<f64>().unwrap_or(f64::NAN) };
    let percent = |capture: &regex::Captures<'_>, group: usize| -> f64 {
        if capture.get(group).is_some() {
            100.0
        } else {
            1.0
        }
    };

    if let Some(captures) = OKHSL.captures(text) {
        let hue = parse(&captures[1]);
        let saturation = parse(&captures[2]) / percent(&captures, 3);
        let lightness = parse(&captures[4]) / percent(&captures, 5);
        // TS: requireFinite(h), requireFinite(s), requireFinite(l)，再查 s/l 范围。
        if !hue.is_finite() {
            return ThemeError::new("h must be finite");
        }
        if !saturation.is_finite() {
            return ThemeError::new("s must be finite");
        }
        if !lightness.is_finite() {
            return ThemeError::new("l must be finite");
        }
        if !(0.0..=1.0).contains(&saturation) {
            return ThemeError::new(format!(
                "s must be between 0 and 1: {}",
                kiana_tui::colors::format_number(saturation)
            ));
        }
        if !(0.0..=1.0).contains(&lightness) {
            return ThemeError::new(format!(
                "l must be between 0 and 1: {}",
                kiana_tui::colors::format_number(lightness)
            ));
        }
        return ThemeError::new(format!("Invalid color value: {text}"));
    }

    if let Some(captures) = OKLCH.captures(text) {
        let lightness = parse(&captures[1]) / percent(&captures, 2);
        let chroma = parse(&captures[3]);
        let hue = parse(&captures[4]);
        // TS: requireFinite(l), requireFinite(c), requireFinite(h)，再查 l 范围与 c 非负。
        if !lightness.is_finite() {
            return ThemeError::new("l must be finite");
        }
        if !chroma.is_finite() {
            return ThemeError::new("c must be finite");
        }
        if !hue.is_finite() {
            return ThemeError::new("h must be finite");
        }
        if !(0.0..=1.0).contains(&lightness) {
            return ThemeError::new(format!(
                "l must be between 0 and 1: {}",
                kiana_tui::colors::format_number(lightness)
            ));
        }
        if chroma < 0.0 {
            return ThemeError::new(format!(
                "c must not be negative: {}",
                kiana_tui::colors::format_number(chroma)
            ));
        }
    }

    ThemeError::new(format!("Invalid color value: {text}"))
}

// =============================================================================
// Theme loading
// =============================================================================

static BUILTIN_THEMES: OnceLock<IndexMap<String, ThemeJson>> = OnceLock::new();

/// TS: `getBuiltinThemes()` —— 内置 dark/light（`include_str!`，不走磁盘与校验器）。
///
/// TS 在这里也走 `JSON.parse`，所以用同一个 [`kiana_json::js_parse::parse`]（键序等 JS 语义
/// 与 `parseThemeJsonContent()` 一致）。
fn builtin_themes() -> &'static IndexMap<String, ThemeJson> {
    BUILTIN_THEMES.get_or_init(|| {
        let mut themes = IndexMap::new();
        for (name, content) in [("dark", DARK_JSON), ("light", LIGHT_JSON)] {
            let value: serde_json::Value =
                kiana_json::js_parse::parse(&kiana_core::text::strip_bom(content))
                    .expect("embedded theme documents are valid JSON (checked by unit tests)");
            let json = theme_json_from_value(&value)
                .expect("embedded theme documents convert (checked by unit tests)");
            themes.insert(name.to_string(), json);
        }
        themes
    })
}

/// TS: `ThemeInfo`。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThemeInfo {
    /// 主题名（来自主题内容，不是文件名）。
    pub name: String,
    /// 主题文件路径（`system` 是生成的，没有文件）。
    pub path: Option<PathBuf>,
}

/// TS: `getAvailableThemes()`。
///
/// 等价于 [`get_available_themes_with_paths`] 的 name 列（包括同样的 panic 行为）。
pub fn get_available_themes() -> Vec<String> {
    get_available_themes_with_paths()
        .into_iter()
        .map(|info| info.name)
        .collect()
}

/// TS: `getAvailableThemesWithPaths()` —— `system` 永远排第一。
///
/// 排序用 `localeCompare`（`kiana_tui::autocomplete::locale_compare`，与 Node 默认 locale 一致）。
///
/// 收集顺序（最后会按上面的 `localeCompare` 规则统一排序）：`system` → 内置主题
/// （`dark`/`light`）→ 自定义主题目录里能成功加载的主题 → 已注册主题。同名只保留第一次
/// 出现的项（因此 `system` 与内置主题的名字优先于自定义文件/注册表）。内置主题的 `path` 是
/// `get_themes_dir()` 下的 `<name>.json`，不做存在性检查；`system` 的 `path` 是 `None`。
/// 自定义文件的文件名只用于筛 `.json`，显示名取主题内容里的 `name`（空名跳过）。
///
/// # Panics
///
/// 自定义主题目录存在但无法读取时（TS 的 `fs.readdirSync()` 抛 `ENOTDIR` / `EACCES`）。
/// 返回类型冻结为 `Vec<ThemeInfo>`，无法返回错误，panic 载荷消息与 Node 的
/// `error.message` 逐字相同，这条文案是当前稳定契约，改动需同步测试。
pub fn get_available_themes_with_paths() -> Vec<ThemeInfo> {
    let themes_dir = get_themes_dir();
    let mut result: Vec<ThemeInfo> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();

    add_theme_info(
        &mut result,
        &mut seen,
        ThemeInfo {
            name: SYSTEM_THEME_NAME.to_string(),
            path: None,
        },
    );
    for name in builtin_themes().keys() {
        add_theme_info(
            &mut result,
            &mut seen,
            ThemeInfo {
                name: name.clone(),
                path: Some(themes_dir.join(format!("{name}.json"))),
            },
        );
    }
    // TS: `getCustomThemeInfos()` 让 `readdirSync` 的错误直接抛出（调用方、例如主题选择器
    // 会看到异常）。公开签名返回 Vec，无法返回错误：用 panic 承载同一条 Node 错误消息。
    let custom_themes = get_custom_theme_infos().unwrap_or_else(|error| panic!("{error}"));
    for info in custom_themes {
        add_theme_info(&mut result, &mut seen, info);
    }
    for (name, theme) in lock(&REGISTERED_THEMES).iter() {
        add_theme_info(
            &mut result,
            &mut seen,
            ThemeInfo {
                name: name.clone(),
                path: theme.source_path().map(Path::to_path_buf),
            },
        );
    }

    result.sort_by(|a, b| {
        if a.name == SYSTEM_THEME_NAME {
            std::cmp::Ordering::Less
        } else if b.name == SYSTEM_THEME_NAME {
            std::cmp::Ordering::Greater
        } else {
            // TS: `a.name.localeCompare(b.name)` —— Node 默认 locale 的 ICU collation
            // （`kiana-tui` 已提供与 Node 26 逐位一致的实现，以保证排序稳定）。
            kiana_tui::autocomplete::locale_compare(&a.name, &b.name)
        }
    });
    result
}

fn add_theme_info(result: &mut Vec<ThemeInfo>, seen: &mut HashSet<String>, info: ThemeInfo) {
    if seen.contains(&info.name) {
        return;
    }
    seen.insert(info.name.clone());
    result.push(info);
}

/// TS: `getCustomThemeInfos()` —— 自定义目录里的 `.json`（非法主题静默忽略）。
///
/// 文件名只用于 `ends_with(".json")` 过滤（区分大小写）；条目名来自主题内容里的 `name`
/// （缺失或空串的条目跳过），`load_theme_from_path()` 失败的条目也跳过。
///
/// TS 的 `fs.readdirSync()` 在目录存在但不可读（EACCES，或路径是文件时的 ENOTDIR）时抛出，
/// `getAvailableThemesWithPaths()` 把错误原样传给调用方。公开签名（spec §5.2）返回
/// `Vec<ThemeInfo>`，无法表达错误，因此由 [`get_available_themes_with_paths`] 用 panic 复刻
/// 这次抛出，panic 消息逐字等于 Node 的 `error.message`（`Error.message` 即
/// `ENOTDIR: not a directory, scandir '<path>'`）。目录不存在时与 TS 一样直接返回空。
fn get_custom_theme_infos() -> Result<Vec<ThemeInfo>, ThemeError> {
    let custom_themes_dir = get_custom_themes_dir();
    let mut result = Vec::new();
    if !custom_themes_dir.exists() {
        return Ok(result);
    }
    let entries = std::fs::read_dir(&custom_themes_dir)
        .map_err(|error| node_fs_scandir_error(&custom_themes_dir, &error))?;
    for entry in entries {
        let entry = entry.map_err(|error| node_fs_scandir_error(&custom_themes_dir, &error))?;
        let file = entry.file_name().to_string_lossy().into_owned();
        if !file.ends_with(".json") {
            continue;
        }
        let theme_path = custom_themes_dir.join(&file);
        if let Ok(theme) = load_theme_from_path(&theme_path, None)
            && let Some(name) = theme.name().filter(|name| !name.is_empty())
        {
            result.push(ThemeInfo {
                name: name.to_string(),
                path: Some(theme_path),
            });
        }
    }
    Ok(result)
}

/// TS: `assertThemeNameIsValid(name)`。
///
/// 只拒绝 `/`（自动明暗主题设置的分隔符）；空名字不在这里检查，由调用方过滤。
fn assert_theme_name_is_valid(name: &str) -> Result<(), ThemeError> {
    if name.contains('/') {
        return Err(ThemeError::new(format!(
            "Invalid theme name \"{name}\": theme names cannot contain \"/\" because it is reserved for automatic light/dark theme settings."
        )));
    }
    Ok(())
}

/// TS: `parseThemeJson(label, json)` —— 有校验器就交给它，否则只要求“带 colors 的对象”。
fn parse_theme_json(label: &str, json: &serde_json::Value) -> Result<ThemeJson, ThemeError> {
    let validator = read(&THEME_JSON_VALIDATOR).clone();
    if let Some(validator) = validator {
        return validator(label, json);
    }
    let has_colors = json
        .as_object()
        .map(|object| object.contains_key("colors"))
        .unwrap_or(false);
    if !has_colors {
        return Err(ThemeError::new(format!(
            "Invalid theme \"{label}\": expected an object with a \"colors\" map."
        )));
    }
    theme_json_from_value(json)
}

/// Node `fs.readFileSync(path, "utf-8")` 的文本解码：非法 UTF-8 字节被替换成 U+FFFD
/// （Node 的 UTF-8 解码器行为），而不是像 `fs::read_to_string` 那样直接报 IO 错误。
fn read_theme_file_text(path: &Path) -> Result<String, ThemeError> {
    let bytes = std::fs::read(path).map_err(|error| node_fs_read_error(path, &error))?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

/// Node `fs.readFileSync(path, "utf-8")` 的错误文案。
///
/// TS 的读取错误（`ENOENT: no such file or directory, open '<path>'`）会原样进入
/// `Failed to load theme "…": …`；Rust 的 `io::Error` 默认只有 os 错误字符串，这里按 Node
/// （libuv）的 errno 名/描述重建。
fn node_fs_read_error(path: &Path, error: &std::io::Error) -> ThemeError {
    let Some((code, description)) = node_error_code_and_description(error) else {
        return ThemeError::new(error.to_string());
    };
    if error.kind() == std::io::ErrorKind::IsADirectory {
        // `readFileSync` 对目录是打开成功、读取失败：Node 用 "read" 动作且不带路径。
        return ThemeError::new(format!("{code}: {description}, read"));
    }
    ThemeError::new(format!("{code}: {description}, open '{}'", path.display()))
}

/// Node `fs.readdirSync(path)` 的错误文案（`ENOTDIR: not a directory, scandir '<path>'`）。
fn node_fs_scandir_error(path: &Path, error: &std::io::Error) -> ThemeError {
    match node_error_code_and_description(error) {
        Some((code, description)) => ThemeError::new(format!(
            "{code}: {description}, scandir '{}'",
            path.display()
        )),
        None => ThemeError::new(error.to_string()),
    }
}

/// libuv 的 `uv_strerror()` errno 名与描述（Linux errno）。
///
/// 先按 `ErrorKind` 归类（跨平台稳定的常见错误），其余按 `raw_os_error()` 的数字查表；
/// 查不到时返回 `None`，调用方退回 `io::Error` 自己的 `Display` 文本。
fn node_error_code_and_description(error: &std::io::Error) -> Option<(&'static str, &'static str)> {
    Some(match error.kind() {
        std::io::ErrorKind::NotFound => ("ENOENT", "no such file or directory"),
        std::io::ErrorKind::PermissionDenied => ("EACCES", "permission denied"),
        std::io::ErrorKind::IsADirectory => ("EISDIR", "illegal operation on a directory"),
        std::io::ErrorKind::NotADirectory => ("ENOTDIR", "not a directory"),
        _ => match error.raw_os_error()? {
            // libuv 的 uv_strerror() 文案（Linux errno）。
            40 => ("ELOOP", "too many symbolic links encountered"),
            36 => ("ENAMETOOLONG", "name too long"),
            24 => ("EMFILE", "too many open files"),
            23 => ("ENFILE", "file table overflow"),
            5 => ("EIO", "i/o error"),
            28 => ("ENOSPC", "no space left on device"),
            30 => ("EROFS", "read-only file system"),
            _ => return None,
        },
    })
}

/// TS: `parseThemeJsonContent(label, content)`。
///
/// TS 的 `JSON.parse` 失败会被包成 `Failed to parse theme {label}: {error}`，其中
/// `{error}` 是 V8 的 `SyntaxError`（带 position/line/column 或原文片段）。这里直接用
/// [`kiana_json::js_parse::parse`]：它就是 `JSON.parse` 的 Rust 等价物，错误文案、UTF-16
/// 位置、行/列（`\r`/`\r\n` 都是换行）、整串特例与片段截断都与 V8 逐字一致，保证错误文案稳定。
///
/// 按 ARCHITECTURE §8 第 31/32 条的两项已决偏差：孤立代理项转义替换为 U+FFFD，非有限数
/// （`1e999`）解析成 `null`（`JSON.stringify(Infinity) === "null"`）。两者的**接受度**
/// 与 V8 相同，因此这里不会再把 JS 合法的文档当成语法错误拒绝。
///
/// 非 BMP 字符作为非法 token 时，V8 的消息里是单独的高代理项（如 `'\ud83d'`）；这里用
/// U+FFFD 表示。Node 把该消息写成 UTF-8 时同样编码成 EF BF BD，因此输出逐字节一致（当前稳定契约）。
fn parse_theme_json_content(label: &str, content: &str) -> Result<ThemeJson, ThemeError> {
    let content = kiana_core::text::strip_bom(content);
    let json = kiana_json::js_parse::parse(&content).map_err(|error| {
        ThemeError::new(format!(
            "Failed to parse theme {label}: SyntaxError: {}",
            error.message
        ))
    })?;
    parse_theme_json(label, &json)
}

/// TS: `loadThemeJson(name)` —— 内置 → 已注册（有/无 sourcePath）→ 自定义目录。
///
/// 注意查找顺序与 `load_theme()` 不同：`load_theme()` 先查已注册主题，因此
/// [`set_registered_themes`] 登记的同名主题可以遮蔽内置主题；导出路径（本函数）则永远先
/// 返回内置版本。已注册但没有 `sourcePath` 的主题在导出时返回“does not have a source
/// path for export”；自定义主题用参数 `name` 拼文件名（不读取文件内容里的 `name`）。
///
/// `getBuiltinThemes()` 返回的是**普通对象**：`name in builtinThemes` 对
/// `Object.prototype` 的成员（`toString`、`constructor`、`__proto__`…）也为真，取到的值是
/// 函数/原型对象；`createTheme()` 随后读 `.colors`（undefined），在
/// `Object.entries(undefined)` 上抛出 `Cannot convert undefined or null to object`。这些名字
/// 因此永远到不了已注册/自定义主题分支，这条错误文案是当前稳定契约，改动需同步测试。
fn load_theme_json(name: &str) -> Result<ThemeJson, ThemeError> {
    if is_object_prototype_key(name) {
        return Err(ThemeError::new(
            "Cannot convert undefined or null to object",
        ));
    }
    if let Some(json) = builtin_themes().get(name) {
        return Ok(json.clone());
    }
    let registered = lock(&REGISTERED_THEMES).get(name).cloned();
    if let Some(theme) = registered {
        if let Some(source_path) = theme.source_path().map(Path::to_path_buf) {
            let content = read_theme_file_text(&source_path)?;
            return parse_theme_json_content(&source_path.to_string_lossy(), &content);
        }
        return Err(ThemeError::new(format!(
            "Theme \"{name}\" does not have a source path for export"
        )));
    }
    let custom_themes_dir = get_custom_themes_dir();
    let theme_path = custom_themes_dir.join(format!("{name}.json"));
    if !theme_path.exists() {
        return Err(ThemeError::new(format!("Theme not found: {name}")));
    }
    let content = read_theme_file_text(&theme_path)?;
    parse_theme_json_content(name, &content)
}

/// TS: `createTheme(themeJson, mode?, sourcePath?)`。
///
/// 颜色变量引用在**这里**解析（`loadThemeJson()` 不解析）：有原始文档时走
/// `resolve_theme_colors_for_create()`（解析 + fallback 可解析性检查），否则用已转换的
/// token map；随后拆分前景/背景，[`Theme::new`] 再补 fallback 并预计算 ANSI。
fn create_theme(
    theme_json: &ThemeJson,
    mode: Option<TerminalColorMode>,
    source_path: Option<PathBuf>,
) -> Result<Theme, ThemeError> {
    // TS 在 `createTheme()` 里解析变量引用；`loadThemeJson()` 不解析颜色。
    // 顺序：resolveThemeColors → withThemeTokenFallbacks → splitThemeColors（见
    // `resolve_theme_colors_for_create`）。
    let resolved = if theme_json.raw.as_object().is_some() {
        resolve_theme_colors_for_create(&theme_json.raw)?
    } else {
        resolve_theme_colors(&theme_json.colors, &theme_json.vars)?
    };
    let (fg_colors, bg_colors) = split_theme_colors(resolved);
    Theme::new(
        fg_colors,
        bg_colors,
        mode.unwrap_or_else(get_terminal_color_mode_default),
        ThemeOptions {
            name: theme_json.name.clone(),
            source_path,
            appearance: theme_json.appearance,
            raw_name: theme_json.raw_name.clone(),
            raw_appearance: theme_json.raw_appearance.clone(),
            ..ThemeOptions::default()
        },
    )
}

/// TS: `createSystemTheme(mode?)`。
///
/// `system` 不读文件：用当前终端颜色现生成（`terminal_colors_pending()` 为真时 saturation
/// 传 0.0，即灰阶），外观提示取 [`get_terminal_theme`]，dim 名单来自生成结果。
fn create_system_theme(mode: Option<TerminalColorMode>) -> Result<Theme, ThemeError> {
    let terminal_colors = terminal_colors();
    let generated = generate_system_theme_colors(&SystemThemeInput {
        foreground: terminal_colors.foreground,
        background: terminal_colors.background,
        palette: terminal_colors.palette.clone(),
        saturation: Some(if terminal_colors_pending() { 0.0 } else { 1.0 }),
        appearance_hint: Some(get_terminal_theme()),
    });
    let values = with_theme_token_fallbacks(generated.colors)?;
    let (fg_colors, bg_colors) = split_theme_colors(values);
    Theme::new(
        fg_colors,
        bg_colors,
        mode.unwrap_or_else(get_terminal_color_mode_default),
        ThemeOptions {
            name: Some(SYSTEM_THEME_NAME.to_string()),
            appearance: generated.appearance,
            dim: generated.dim,
            ..ThemeOptions::default()
        },
    )
}

/// TS: `loadThemeFromPath(themePath, mode?)`。
///
/// 完整流程：读文件（非法 UTF-8 字节按损失式解码）→ `parseThemeJsonContent()` 等价的
/// 解析 → `createTheme`。`mode` 为 `None` 时用终端能力默认值
/// （`kiana_tui::terminal_image::get_terminal_color_mode_default`）。
///
/// 返回的 `Arc<Theme>` 不注册、不设为全局主题，调用方自行决定用途。错误文案是当前稳定契约：
/// 读取失败是 Node 的 errno 文案，语法错误是 V8 的 `SyntaxError` 文案，其余是颜色/校验错误。
pub fn load_theme_from_path(
    theme_path: &Path,
    mode: Option<TerminalColorMode>,
) -> Result<Arc<Theme>, ThemeError> {
    let content = read_theme_file_text(theme_path)?;
    let theme_json = parse_theme_json_content(&theme_path.to_string_lossy(), &content)?;
    Ok(Arc::new(create_theme(
        &theme_json,
        mode,
        Some(theme_path.to_path_buf()),
    )?))
}

/// TS: `loadTheme(name, mode?)` —— `system` 名称保留，优先于同名自定义主题。
///
/// 分支顺序：`system`（每次现生成）→ 已注册主题（直接复用实例，**忽略本次 `mode`**，也
/// 不会重新读文件）→ `load_theme_json()`（内置/自定义/已注册带路径）。已注册主题因此可以
/// 遮蔽同名内置主题，与 `load_theme_json()` 的优先级相反。
fn load_theme(name: &str, mode: Option<TerminalColorMode>) -> Result<Arc<Theme>, ThemeError> {
    if name == SYSTEM_THEME_NAME {
        return Ok(Arc::new(create_system_theme(mode)?));
    }
    if let Some(registered) = lock(&REGISTERED_THEMES).get(name) {
        return Ok(Arc::clone(registered));
    }
    let theme_json = load_theme_json(name)?;
    Ok(Arc::new(create_theme(&theme_json, mode, None)?))
}

/// TS: `getThemeByName(name)` —— 任何错误都变成 `None`。
///
/// 只查找/构造主题，不修改全局主题与当前名字；找不到、JSON 非法、颜色非法都返回 `None`。
pub fn get_theme_by_name(name: &str) -> Option<Arc<Theme>> {
    load_theme(name, None).ok()
}

/// TS: `parseAutoThemeSetting(themeSetting)` 的返回值。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AutoTheme {
    /// `/` 左侧（浅色终端使用的主题）。
    pub light_theme: String,
    /// `/` 右侧（深色终端使用的主题）。
    pub dark_theme: String,
}

/// TS: `parseAutoThemeSetting(themeSetting)`。
///
/// 语法是 `light/dark`：在**第一个** `/` 处切分，两侧用 JS 语义的 `trim()`（`js_trim`）去掉
/// 空白；没有 `/`、出现第二个 `/`、或任一侧 trim 后为空都返回 `None`。
pub fn parse_auto_theme_setting(theme_setting: Option<&str>) -> Option<AutoTheme> {
    let theme_setting = theme_setting?;
    let slash_index = theme_setting.find('/')?;
    if theme_setting[slash_index + 1..].contains('/') {
        return None;
    }
    let light_theme = js_trim(&theme_setting[..slash_index]);
    let dark_theme = js_trim(&theme_setting[slash_index + 1..]);
    if light_theme.is_empty() || dark_theme.is_empty() {
        return None;
    }
    Some(AutoTheme {
        light_theme: light_theme.to_string(),
        dark_theme: dark_theme.to_string(),
    })
}

/// TS: `resolveThemeSetting(themeSetting, terminalTheme)`。
///
/// 自动设置（[`parse_auto_theme_setting`] 成功）按 `terminal_theme` 选左/右一侧；不是自动
/// 设置时**原样返回**（包括空字符串），只有“含 `/` 但无法解析成自动设置”才返回 `None`。
pub fn resolve_theme_setting(
    theme_setting: Option<&str>,
    terminal_theme: TerminalTheme,
) -> Option<String> {
    if let Some(auto_theme) = parse_auto_theme_setting(theme_setting) {
        return Some(match terminal_theme {
            TerminalColorScheme::Light => auto_theme.light_theme,
            TerminalColorScheme::Dark => auto_theme.dark_theme,
        });
    }
    let theme_setting = theme_setting?;
    if theme_setting.contains('/') {
        return None;
    }
    Some(theme_setting.to_string())
}

/// TS: `detectColorFgBgTheme(env = process.env)` —— 从 `COLORFGBG` 读取终端明暗。
pub fn detect_color_fg_bg_theme() -> Option<TerminalTheme> {
    detect_color_fg_bg_theme_from(std::env::var("COLORFGBG").ok().as_deref())
}

/// [`detect_color_fg_bg_theme`] 的显式环境变体（测试用）。
///
/// 值形如 `fg;bg` 或 `fg;xpm;bg`（rxvt），最后一个字段是 ANSI 颜色索引（或 `default`）。
/// 按索引分类（与 Vim 相同）：0-6 与 8（亮黑，例如 Solarized Dark 的背景）算深色，
/// 7 与 9-15 算浅色。
///
/// 输入缺失、末字段为空/不是 1-2 位 ASCII 数字、或索引大于 15 时返回 `None`。
pub fn detect_color_fg_bg_theme_from(color_fgbg: Option<&str>) -> Option<TerminalTheme> {
    let background = js_trim(color_fgbg?.split(';').next_back()?);
    if background.is_empty() || !is_short_decimal(background) {
        return None;
    }
    let index: u32 = background.parse().ok()?;
    if index > 15 {
        return None;
    }
    Some(if index <= 6 || index == 8 {
        TerminalColorScheme::Dark
    } else {
        TerminalColorScheme::Light
    })
}

/// `/^\d{1,2}$/`（ASCII 数字）。
fn is_short_decimal(text: &str) -> bool {
    let bytes = text.as_bytes();
    (1..=2).contains(&bytes.len()) && bytes.iter().all(u8::is_ascii_digit)
}

/// TS: `detectTerminalTheme(colors = {}, reportedScheme?, env = process.env)`。
pub fn detect_terminal_theme(
    colors: &TerminalColors,
    reported_scheme: Option<TerminalTheme>,
) -> TerminalTheme {
    detect_terminal_theme_with_env(
        colors,
        reported_scheme,
        std::env::var("COLORFGBG").ok().as_deref(),
    )
}

/// [`detect_terminal_theme`] 的显式环境变体（测试用）。
///
/// 背景色优先（按它自己的可读方向分类）；没有背景时依次用明暗报告、`COLORFGBG`，最后
/// 默认深色。
pub fn detect_terminal_theme_with_env(
    colors: &TerminalColors,
    reported_scheme: Option<TerminalTheme>,
    color_fgbg: Option<&str>,
) -> TerminalTheme {
    if let Some(background) = colors.background {
        return terminal_appearance(background, colors.foreground);
    }
    reported_scheme
        .or_else(|| detect_color_fg_bg_theme_from(color_fgbg))
        .unwrap_or(TerminalColorScheme::Dark)
}

/// TS: `getTerminalTheme()` —— 由目前所有已上报信息决定终端明暗。
///
/// 每次调用读取当前全局快照；终端颜色与明暗报告是两把独立的锁，因此两者可能来自不同
/// 时刻的上报（不会崩溃，只是反映“最近一次已记录的组合”）。
pub fn get_terminal_theme() -> TerminalTheme {
    detect_terminal_theme(&terminal_colors(), terminal_color_scheme())
}

// =============================================================================
// Global theme instance
// =============================================================================

/// TS 用 `globalThis[Symbol.for(...)]` 保存全局主题（node + jiti 双加载器共享）。
static THEME_STATE: LazyLock<RwLock<Option<Arc<Theme>>>> = LazyLock::new(|| RwLock::new(None));
static CURRENT_THEME_NAME: LazyLock<Mutex<Option<String>>> = LazyLock::new(|| Mutex::new(None));
static REGISTERED_THEMES: LazyLock<Mutex<IndexMap<String, Arc<Theme>>>> =
    LazyLock::new(|| Mutex::new(IndexMap::new()));
/// 主题变化回调（TS: 唯一的 `onThemeChangeCallback`）。
type ThemeChangeCallback = Arc<dyn Fn() + Send + Sync>;

static ON_THEME_CHANGE: LazyLock<Mutex<Option<ThemeChangeCallback>>> =
    LazyLock::new(|| Mutex::new(None));

/// 当前全局主题；`initTheme()`/`setTheme()` 之前是 `None`。
pub fn current_theme() -> Option<Arc<Theme>> {
    read(&THEME_STATE).clone()
}

/// 当前全局主题的名字（`currentThemeName`）。
pub fn current_theme_name() -> Option<String> {
    lock(&CURRENT_THEME_NAME).clone()
}

fn set_current_theme_name(name: Option<String>) {
    *lock(&CURRENT_THEME_NAME) = name;
}

fn set_global_theme(theme: Arc<Theme>) {
    *write(&THEME_STATE) = Some(theme);
}

/// TS: `setRegisteredThemes(themes)` —— 清空后重新登记（名字含 `/` 时抛错）。
///
/// 空名字/缺名字的主题被静默跳过；同名主题后者覆盖前者（`IndexMap::insert`）。
///
/// 失败模式：函数**先清空**再逐个校验名字。遇到含 `/` 的名字会立即返回错误，此时已经处理
/// 过的主题仍留在注册表里（注册表被部分更新），调用方不能假设失败后状态不变。
pub fn set_registered_themes(themes: Vec<Arc<Theme>>) -> Result<(), ThemeError> {
    let mut registered = lock(&REGISTERED_THEMES);
    registered.clear();
    for theme in themes {
        let name = theme
            .name()
            .filter(|name| !name.is_empty())
            .map(str::to_string);
        if let Some(name) = name {
            assert_theme_name_is_valid(&name)?;
            registered.insert(name, theme);
        }
    }
    Ok(())
}

fn notify_theme_change() {
    let callback = lock(&ON_THEME_CHANGE).clone();
    if let Some(callback) = callback {
        callback();
    }
}

/// TS: `onThemeChange(callback)` —— 只保留最后一个回调。
///
/// 再次调用会替换而不是追加。回调在触发它的线程上同步执行（`set_theme` 在调用者线程，
/// 文件热重载在去抖线程），且调用时**不持有**回调表的互斥锁。
pub fn on_theme_change(callback: Box<dyn Fn() + Send + Sync>) {
    *lock(&ON_THEME_CHANGE) = Some(Arc::from(callback));
}

/// TS: `initTheme(themeName?, enableWatcher = false)`。
///
/// 省略名字时用 `system`。函数先记录名字再加载：成功则替换全局主题并在 `enable_watcher`
/// 时启动 watcher；失败则把名字和全局主题都切到 `system`（system 也加载失败时全局主题保持
/// 旧值），**不触发** [`on_theme_change`] 回调，也不返回错误。
pub fn init_theme(theme_name: Option<&str>, enable_watcher: bool) {
    let name = theme_name.unwrap_or(SYSTEM_THEME_NAME);
    set_current_theme_name(Some(name.to_string()));
    match load_theme(name, None) {
        Ok(theme) => {
            set_global_theme(theme);
            if enable_watcher {
                start_theme_watcher();
            }
        }
        Err(_) => {
            // 主题非法：静默回退到 system，不启动 watcher。
            set_current_theme_name(Some(SYSTEM_THEME_NAME.to_string()));
            if let Ok(theme) = load_theme(SYSTEM_THEME_NAME, None) {
                set_global_theme(theme);
            }
        }
    }
}

/// TS: `setTheme(name, enableWatcher = false)` 的返回值。
///
/// `Default` 是“失败且没有消息”（两个字段的默认值）；正常构造请用 [`ThemeResult::ok`] /
/// [`ThemeResult::failed`]。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ThemeResult {
    /// 是否成功加载。
    pub success: bool,
    /// 失败时的错误消息（TS: `error.message`）。
    pub error: Option<String>,
}

impl ThemeResult {
    /// 成功。
    pub fn ok() -> Self {
        ThemeResult {
            success: true,
            error: None,
        }
    }

    /// 失败。
    pub fn failed(message: impl Into<String>) -> Self {
        ThemeResult {
            success: false,
            error: Some(message.into()),
        }
    }
}

/// TS: `setTheme(name, enableWatcher = false)` —— 失败时回退到 system 并返回错误消息。
///
/// 成功：替换全局主题、按需启动 watcher，并同步触发通过 [`on_theme_change`] 注册的回调。
/// 失败：名字切到 `system` 并重新加载 system（失败则保留旧全局主题），**不触发回调**，
/// 返回带逐字错误消息的 [`ThemeResult::failed`]。
pub fn set_theme(name: &str, enable_watcher: bool) -> ThemeResult {
    set_current_theme_name(Some(name.to_string()));
    match load_theme(name, None) {
        Ok(theme) => {
            set_global_theme(theme);
            if enable_watcher {
                start_theme_watcher();
            }
            notify_theme_change();
            ThemeResult::ok()
        }
        Err(error) => {
            set_current_theme_name(Some(SYSTEM_THEME_NAME.to_string()));
            if let Ok(theme) = load_theme(SYSTEM_THEME_NAME, None) {
                set_global_theme(theme);
            }
            ThemeResult::failed(error.to_string())
        }
    }
}

/// TS: `setThemeInstance(themeInstance)`。
///
/// 不经过加载/校验，直接把调用方构造的实例设为全局主题；当前名字记为 `"<in-memory>"`，
/// 停掉 watcher，并触发通过 [`on_theme_change`] 注册的回调。该名字不会出现在
/// [`get_available_themes`] 里（除非自定义目录或注册表里恰好有同名主题）。
pub fn set_theme_instance(theme_instance: Arc<Theme>) {
    set_global_theme(theme_instance);
    set_current_theme_name(Some("<in-memory>".to_string()));
    stop_theme_watcher();
    notify_theme_change();
}

// =============================================================================
// Theme file hot reload
// =============================================================================

/// 当前 watcher 的代号；`stop_theme_watcher()` 递增它，让在途的防抖循环失效。
static WATCHER_GENERATION: AtomicU64 = AtomicU64::new(0);
static WATCHER: LazyLock<Mutex<Option<ThemeWatcher>>> = LazyLock::new(|| Mutex::new(None));

/// 一个主题文件 watcher + 去抖线程。
struct ThemeWatcher {
    /// `RecommendedWatcher` 的类型来自 `notify`（本 crate 不直接依赖它），用 `Any` 持有：
    /// drop 就是 `closeWatcher()`。
    watcher: Option<Box<dyn Any + Send>>,
    sender: Option<std::sync::mpsc::Sender<()>>,
    stop: Arc<AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl Drop for ThemeWatcher {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        // 断开通道让去抖线程退出；不 join（回调可能正持有全局锁）。
        self.sender = None;
        self.watcher = None;
        self.handle = None;
    }
}

/// TS: `startThemeWatcher()`。
///
/// 只监视自定义主题目录里的 `<currentName>.json`：内置名（`dark`/`light`/`system`）、没有
/// 当前名字、或文件不存在时直接返回。启动前先调用 [`stop_theme_watcher`] 停掉旧 watcher；
/// 事件回调会再次核对当前主题名，因此切换主题后旧 watcher 的迟到事件不会误重载。
fn start_theme_watcher() {
    stop_theme_watcher();

    let Some(watched_theme_name) = current_theme_name() else {
        return;
    };
    if watched_theme_name == "dark"
        || watched_theme_name == "light"
        || watched_theme_name == SYSTEM_THEME_NAME
    {
        return;
    }

    let custom_themes_dir = get_custom_themes_dir();
    let watched_file_name = format!("{watched_theme_name}.json");
    let theme_file = custom_themes_dir.join(&watched_file_name);
    if !theme_file.exists() {
        return;
    }

    let generation = WATCHER_GENERATION.fetch_add(1, Ordering::SeqCst) + 1;
    let stop = Arc::new(AtomicBool::new(false));
    let (sender, receiver) = std::sync::mpsc::channel::<()>();

    let listener_name = watched_theme_name.clone();
    let listener_file = watched_file_name.clone();
    let listener_sender = sender.clone();
    let listener_stop = Arc::clone(&stop);
    let watcher = kiana_core::fs_watch::watch_with_error_handler(
        &custom_themes_dir,
        move |_event_type, filename| {
            if current_theme_name().as_deref() != Some(listener_name.as_str()) {
                return;
            }
            match filename {
                None => {
                    // 事件没有携带文件名时按目标文件变化处理（保守触发一次重载）。
                    let _ = listener_sender.send(());
                }
                Some(filename) if filename == listener_file => {
                    let _ = listener_sender.send(());
                }
                Some(_) => {}
            }
        },
        move || {
            // notify 线程上的错误回调：只停掉去抖（watcher 本身在下一次 start/stop 时
            // 关闭 —— 在自己的回调里 drop RecommendedWatcher 可能死锁）。
            listener_stop.store(true, Ordering::SeqCst);
        },
    );

    let Some(watcher) = watcher else {
        return;
    };

    let thread_name = watched_theme_name.clone();
    let thread_file = theme_file.clone();
    let thread_stop = Arc::clone(&stop);
    let handle = std::thread::Builder::new()
        .name("kiana-theme-watcher".to_string())
        .spawn(move || {
            debounce_reload(receiver, thread_stop, generation, thread_name, thread_file);
        })
        .ok();

    *lock(&WATCHER) = Some(ThemeWatcher {
        watcher: Some(Box::new(watcher)),
        sender: Some(sender),
        stop,
        handle,
    });
}

/// 100 ms 去抖循环（TS 的 `setTimeout(..., 100)` + `clearTimeout`）。
fn debounce_reload(
    receiver: Receiver<()>,
    stop: Arc<AtomicBool>,
    generation: u64,
    watched_theme_name: String,
    theme_file: PathBuf,
) {
    let debounce = Duration::from_millis(100);
    while !stop.load(Ordering::SeqCst) {
        // 阶段 1：阻塞等待第一个事件；通道断开（watcher 已被丢弃）时退出。
        if receiver.recv().is_err() {
            break;
        }
        let mut deadline = Instant::now() + debounce;
        // 阶段 2：100 ms 窗口内每来一个事件就顺延 deadline（尾部去抖）。
        loop {
            let now = Instant::now();
            if now >= deadline {
                break;
            }
            match receiver.recv_timeout(deadline.saturating_duration_since(now)) {
                Ok(()) => deadline = Instant::now() + debounce,
                Err(RecvTimeoutError::Timeout) => break,
                Err(RecvTimeoutError::Disconnected) => return,
            }
        }
        if stop.load(Ordering::SeqCst) {
            break;
        }
        // 忽略过期定时器（切换主题或停止 watcher 之后）。
        if WATCHER_GENERATION.load(Ordering::SeqCst) != generation {
            break;
        }
        reload_watched_theme(&watched_theme_name, &theme_file);
    }
}

/// TS: 去抖定时器到期后的重载逻辑。
///
/// 依次做三道检查：当前主题名仍是监视名 → 文件仍存在 → 文件能成功加载。任一失败都保留
/// 旧主题（编辑中的半成品文件不会把界面清空）。成功后用**监视名**（而不是文件内容里的
/// `name`）覆盖注册表，替换全局主题并触发通过 [`on_theme_change`] 注册的回调。
fn reload_watched_theme(watched_theme_name: &str, theme_file: &Path) {
    if current_theme_name().as_deref() != Some(watched_theme_name) {
        return;
    }
    // 文件暂时不存在时保留最后一次成功加载的主题。
    if !theme_file.exists() {
        return;
    }
    let Ok(reloaded) = load_theme_from_path(theme_file, None) else {
        // 编辑中的文件可能暂时非法：忽略。
        return;
    };
    lock(&REGISTERED_THEMES).insert(watched_theme_name.to_string(), Arc::clone(&reloaded));
    set_global_theme(reloaded);
    notify_theme_change();
}

/// TS: `stopThemeWatcher()` —— 清定时器 + 关 watcher。
///
/// 可重复调用；没有 watcher 时只递增代号。不会等待去抖线程结束（见 `ThemeWatcher::drop`）。
pub fn stop_theme_watcher() {
    WATCHER_GENERATION.fetch_add(1, Ordering::SeqCst);
    let watcher = lock(&WATCHER).take();
    drop(watcher);
}

// =============================================================================
// HTML export helpers
// =============================================================================

/// TS: `getResolvedThemeColors(themeName?)` —— 每个 token 的 CSS hex（保持 token 顺序）。
///
/// 名字解析：参数 → [`current_theme_name`] → `system`。返回的 map 迭代顺序沿用
/// [`Theme::colors`]（前景在前、背景在后，见其说明），值统一是 `#rrggbb`。加载失败
/// （主题不存在 / JSON / 颜色非法）时返回错误，而不是空表。
pub fn get_resolved_theme_colors(
    theme_name: Option<&str>,
) -> Result<IndexMap<String, String>, ThemeError> {
    let name = theme_name
        .map(str::to_string)
        .or_else(current_theme_name)
        .unwrap_or_else(|| SYSTEM_THEME_NAME.to_string());
    let theme = load_theme(&name, None)?;
    let colors = theme.colors();
    Ok(colors
        .iter()
        .map(|(token, color)| (token.as_str().to_string(), color_to_hex(*color)))
        .collect())
}

/// TS: `isLightTheme(themeName?)` —— `theme.appearance === "light"`。
///
/// 名字解析同 [`get_resolved_theme_colors`]。判定优先使用主题文档里的原始 `appearance`：
/// 值不是 `null` 时只有字符串 `"light"` 为真（`"Light"`、数字、布尔都是假）；缺省或
/// `null` 时才回退到 [`Theme::appearance`]（声明/颜色推导/终端外观）。
pub fn is_light_theme(theme_name: Option<&str>) -> Result<bool, ThemeError> {
    let name = theme_name
        .map(str::to_string)
        .or_else(current_theme_name)
        .unwrap_or_else(|| SYSTEM_THEME_NAME.to_string());
    let theme = load_theme(&name, None)?;
    // TS 的 getter 返回原始值：`"Light"`/`0`/`false`/`""` 都 `!== "light"`。只有
    // `null`/缺省才回退到检测结果（`options.appearance ?? detectAppearance(...)`）。
    if let Some(raw) = theme
        .raw_appearance
        .as_ref()
        .filter(|value| !value.is_null())
    {
        return Ok(raw.as_str() == Some("light"));
    }
    Ok(theme.appearance() == TerminalColorScheme::Light)
}

/// TS: `getThemeExportColors()` 的返回值（未显式设置的颜色是 `None`）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ThemeExportColors {
    /// `pageBg`。
    pub page_bg: Option<String>,
    /// `cardBg`。
    pub card_bg: Option<String>,
    /// `infoBg`。
    pub info_bg: Option<String>,
}

/// TS: `getThemeExportColors(themeName?)` —— 主题 JSON 的 `export` 段（CSS 友好形态）。
///
/// 名字解析同 [`get_resolved_theme_colors`]。以下情况都返回全 `None`（对应 TS 的 `{}`）：
/// 名字是 `system`、主题加载失败、没有 `export` 段、或三个值中任一**解析出错**（例如非法的
/// `okhsl(...)` 字面量；缺失/空字符串只是该字段为 `None`，不影响其它字段）。
///
/// 值转换：整数索引 → `#rrggbb`；空字符串 → `None`；`okhsl(...)` 字面量转成 hex（CSS 不支持
/// okhsl）；其余字符串（hex / oklch / rgb / 颜色名）原样返回。变量引用在此之前已按主题的
/// `vars` 递归解析。
pub fn get_theme_export_colors(theme_name: Option<&str>) -> ThemeExportColors {
    let name = theme_name
        .map(str::to_string)
        .or_else(current_theme_name)
        .unwrap_or_else(|| SYSTEM_THEME_NAME.to_string());
    if name == SYSTEM_THEME_NAME {
        return ThemeExportColors::default();
    }
    let Ok(theme_json) = load_theme_json(&name) else {
        return ThemeExportColors::default();
    };
    // TS 的 `getThemeExportColors()` 自己解析 export 段（独立于 createTheme 的颜色解析）。
    let export = if theme_json.raw.as_object().is_some() {
        resolve_theme_export_from_document(&theme_json.raw)
    } else {
        theme_json.export.clone()
    };
    let Some(export) = export else {
        return ThemeExportColors::default();
    };
    // `theme_json_from_value()` 已经把变量引用解析成字面量/索引（TS 在
    // `getThemeExportColors()` 里解析）。这里只做 CSS 形态转换。
    let resolve = |value: &Option<ThemeColorValue>| -> Result<Option<String>, ThemeError> {
        match value {
            None => Ok(None),
            Some(ThemeColorValue::Index(index)) => Ok(Some(color_to_hex(indexed_color(*index)))),
            Some(ThemeColorValue::Str(text)) if text.is_empty() => Ok(None),
            Some(ThemeColorValue::Str(text)) if is_okhsl_literal(text) => parse_color(text)
                .map(|color| Some(color_to_hex(color)))
                .ok_or_else(|| ThemeError::new(format!("Invalid color value: {text}"))),
            Some(ThemeColorValue::Str(text)) => Ok(Some(text.clone())),
        }
    };
    // TS 把整个 `{pageBg, cardBg, infoBg}` 对象字面量放在 try 里：任何一项抛错都返回
    // `{}`，而不是部分结果。
    match (
        resolve(&export.page_bg),
        resolve(&export.card_bg),
        resolve(&export.info_bg),
    ) {
        (Ok(page_bg), Ok(card_bg), Ok(info_bg)) => ThemeExportColors {
            page_bg,
            card_bg,
            info_bg,
        },
        _ => ThemeExportColors::default(),
    }
}

/// `/^okhsl\(/i`。
///
/// 与 [`crate::theme_schema`] 的 `ok(lch|hsl)` 判定一样，JS 的非 unicode `i` 只折叠 ASCII
/// （`/s/i` 不匹配 U+017F），因此用 ASCII 比较而不是 `regex` 的 Unicode `(?i)`。
fn is_okhsl_literal(text: &str) -> bool {
    text.get(..6)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("okhsl("))
}

// =============================================================================
// CLI syntax highlighting
// =============================================================================

/// `highlight.js` scope → 主题 token 的映射（TS: `buildCliHighlightTheme`）。
fn build_cli_highlight_theme() -> kiana_core::syntax_highlight::HighlightTheme {
    let mut theme: kiana_core::syntax_highlight::HighlightTheme =
        kiana_core::syntax_highlight::HighlightTheme::new();
    insert_scope(&mut theme, "keyword", ThemeColor::SyntaxKeyword);
    insert_scope(&mut theme, "built_in", ThemeColor::SyntaxType);
    insert_scope(&mut theme, "literal", ThemeColor::SyntaxNumber);
    insert_scope(&mut theme, "number", ThemeColor::SyntaxNumber);
    insert_scope(&mut theme, "regexp", ThemeColor::SyntaxString);
    insert_scope(&mut theme, "string", ThemeColor::SyntaxString);
    insert_scope(&mut theme, "subst", ThemeColor::Text);
    insert_scope(&mut theme, "comment", ThemeColor::SyntaxComment);
    insert_scope(&mut theme, "doctag", ThemeColor::SyntaxComment);
    insert_scope(&mut theme, "meta", ThemeColor::Muted);
    insert_scope(&mut theme, "function", ThemeColor::SyntaxFunction);
    insert_scope(&mut theme, "title", ThemeColor::SyntaxFunction);
    insert_scope(&mut theme, "class", ThemeColor::SyntaxType);
    insert_scope(&mut theme, "type", ThemeColor::SyntaxType);
    insert_scope(&mut theme, "tag", ThemeColor::SyntaxPunctuation);
    insert_scope(&mut theme, "name", ThemeColor::SyntaxKeyword);
    insert_scope(&mut theme, "attr", ThemeColor::SyntaxVariable);
    insert_scope(&mut theme, "variable", ThemeColor::SyntaxVariable);
    insert_scope(&mut theme, "params", ThemeColor::SyntaxVariable);
    insert_scope(&mut theme, "operator", ThemeColor::SyntaxOperator);
    insert_scope(&mut theme, "punctuation", ThemeColor::SyntaxPunctuation);
    insert_map_style(&mut theme, "emphasis", "\x1b[3m", "\x1b[23m");
    insert_map_style(&mut theme, "strong", "\x1b[1m", "\x1b[22m");
    insert_map_style(&mut theme, "link", "\x1b[4m", "\x1b[24m");
    insert_scope(&mut theme, "addition", ThemeColor::ToolDiffAdded);
    insert_scope(&mut theme, "deletion", ThemeColor::ToolDiffRemoved);
    theme
}

fn insert_scope(
    theme: &mut kiana_core::syntax_highlight::HighlightTheme,
    scope: &str,
    color: ThemeColor,
) {
    theme.insert(
        scope.to_string(),
        Arc::new(move |text: &str| THEME.fg(color, text).unwrap_or_else(|_| text.to_string())),
    );
}

fn insert_map_style(
    theme: &mut kiana_core::syntax_highlight::HighlightTheme,
    scope: &str,
    open: &'static str,
    close: &'static str,
) {
    theme.insert(
        scope.to_string(),
        Arc::new(move |text: &str| chalk_wrap(open, close, text)),
    );
}

/// TS: `getCliHighlightTheme(t)` —— 按 `Theme` 实例身份缓存。
fn get_cli_highlight_theme(theme: &Arc<Theme>) -> kiana_core::syntax_highlight::HighlightTheme {
    let mut cache = lock(&CLI_HIGHLIGHT_CACHE);
    if let Some((cached_theme, highlight_theme)) = cache.as_ref()
        && Arc::ptr_eq(cached_theme, theme)
    {
        return highlight_theme.clone();
    }
    let built = build_cli_highlight_theme();
    *cache = Some((Arc::clone(theme), built.clone()));
    built
}

/// CLI 高亮主题的缓存值（按 `Theme` 实例身份）。
type CliHighlightCache = Mutex<Option<(Arc<Theme>, kiana_core::syntax_highlight::HighlightTheme)>>;

static CLI_HIGHLIGHT_CACHE: LazyLock<CliHighlightCache> = LazyLock::new(|| Mutex::new(None));

/// 没有有效语言时，把每行包上 `mdCodeBlock` 色。
fn colorized_lines(code: &str) -> Vec<String> {
    code.split('\n')
        .map(|line| {
            THEME
                .fg(ThemeColor::MdCodeBlock, line)
                .unwrap_or_else(|_| line.to_string())
        })
        .collect()
}

/// TS: `highlightCode(code, lang?)` —— 返回带 ANSI 的代码行。
///
/// 语言先经 `supportsLanguage()` 校验：无效时不做高亮（cli-highlight 的自动检测会把散文
/// 误判成 AppleScript 之类），只给每行上 `mdCodeBlock` 色。
///
/// 语言为 `None`、空串或不被支持时，每行用**当前全局主题**着色；全局主题未初始化时
/// `THEME.fg` 失败，行原样返回（无 ANSI）。空输入得到长度为 1 的数组（JS 的
/// `"".split("\n")` 语义）。有效语言路径使用的高亮主题按全局主题实例身份缓存，切换主题
/// 后下一次调用重建。
pub fn highlight_code(code: &str, lang: Option<&str>) -> Vec<String> {
    let valid_lang = lang
        .filter(|language| !language.is_empty())
        .filter(|language| kiana_core::syntax_highlight::supports_language(language));
    let Some(valid_lang) = valid_lang else {
        return colorized_lines(code);
    };
    let highlight_theme = match current_theme() {
        Some(theme) => get_cli_highlight_theme(&theme),
        None => build_cli_highlight_theme(),
    };
    let options = kiana_core::syntax_highlight::HighlightOptions {
        language: Some(valid_lang.to_string()),
        ignore_illegals: true,
        language_subset: None,
        theme: highlight_theme,
    };
    // TS 在这里 catch 后返回不带颜色 `code.split("\n")`；Rust 的 `highlight()` 不会失败
    // （未知语言回退 plaintext），因此该分支不可达。
    kiana_core::syntax_highlight::highlight(code, &options)
        .split('\n')
        .map(str::to_string)
        .collect()
}

/// TS: `getLanguageFromPath(filePath)` —— 从扩展名取 highlight.js 语言名。
///
/// 取最后一个 `.` 之后的全部内容并转小写（`rsplit('.')`），所以**没有点的文件名整体**
/// 当作扩展名查表（`Makefile` → `makefile`，测试锁定这一点）；`"x."` 的空扩展名和未知
/// 扩展名都返回 `None`。
pub fn get_language_from_path(file_path: &str) -> Option<&'static str> {
    let extension = file_path.rsplit('.').next()?.to_lowercase();
    if extension.is_empty() {
        return None;
    }
    let language = match extension.as_str() {
        "ts" | "tsx" => "typescript",
        "js" | "jsx" | "mjs" | "cjs" => "javascript",
        "py" => "python",
        "rb" => "ruby",
        "rs" => "rust",
        "go" => "go",
        "java" => "java",
        "kt" => "kotlin",
        "swift" => "swift",
        "c" | "h" => "c",
        "cpp" | "cc" | "cxx" | "hpp" => "cpp",
        "cs" => "csharp",
        "php" => "php",
        "sh" | "bash" | "zsh" => "bash",
        "fish" => "fish",
        "ps1" => "powershell",
        "sql" => "sql",
        "html" | "htm" => "html",
        "css" => "css",
        "scss" => "scss",
        "sass" => "sass",
        "less" => "less",
        "json" => "json",
        "yaml" | "yml" => "yaml",
        "toml" => "toml",
        "xml" => "xml",
        "md" | "markdown" => "markdown",
        "dockerfile" => "dockerfile",
        "makefile" => "makefile",
        "cmake" => "cmake",
        "lua" => "lua",
        "perl" => "perl",
        "r" => "r",
        "scala" => "scala",
        "clj" => "clojure",
        "ex" | "exs" => "elixir",
        "erl" => "erlang",
        "hs" => "haskell",
        "ml" => "ocaml",
        "vim" => "vim",
        "graphql" => "graphql",
        "proto" => "protobuf",
        "tf" | "hcl" => "hcl",
        _ => return None,
    };
    Some(language)
}

// =============================================================================
// TUI theme factories
// =============================================================================

/// 一个「token → 样式」闭包：每次都读全局主题（TS 的 `theme` 是 Proxy）。
fn token_style(color: ThemeColor) -> kiana_tui::components::markdown::StyleFn {
    Arc::new(move |text: &str| THEME.fg(color, text).unwrap_or_else(|_| text.to_string()))
}

fn chalk_style_fn(
    open: &'static str,
    close: &'static str,
) -> kiana_tui::components::markdown::StyleFn {
    Arc::new(move |text: &str| chalk_wrap(open, close, text))
}

/// TS: `getMarkdownTheme()`。
///
/// 每个样式闭包在使用时才读全局主题（主题切换后旧组件立刻换色）。
pub fn get_markdown_theme() -> MarkdownTheme {
    MarkdownTheme {
        heading: token_style(ThemeColor::MdHeading),
        link: token_style(ThemeColor::MdLink),
        link_url: token_style(ThemeColor::MdLinkUrl),
        code: token_style(ThemeColor::MdCode),
        code_block: token_style(ThemeColor::MdCodeBlock),
        code_block_border: token_style(ThemeColor::MdCodeBlockBorder),
        quote: token_style(ThemeColor::MdQuote),
        quote_border: token_style(ThemeColor::MdQuoteBorder),
        hr: token_style(ThemeColor::MdHr),
        list_bullet: token_style(ThemeColor::MdListBullet),
        bold: chalk_style_fn("\x1b[1m", "\x1b[22m"),
        italic: chalk_style_fn("\x1b[3m", "\x1b[23m"),
        strikethrough: chalk_style_fn("\x1b[9m", "\x1b[29m"),
        underline: chalk_style_fn("\x1b[4m", "\x1b[24m"),
        highlight_code: Some(Arc::new(|code: &str, lang: Option<&str>| {
            highlight_code(code, lang)
        })),
        code_block_indent: None,
    }
}

/// TS: `getSelectListTheme()`。
pub fn get_select_list_theme() -> SelectListTheme {
    SelectListTheme {
        selected_prefix: token_style(ThemeColor::Accent),
        selected_text: token_style(ThemeColor::Accent),
        description: token_style(ThemeColor::Muted),
        scroll_info: token_style(ThemeColor::Muted),
        no_match: token_style(ThemeColor::Muted),
    }
}

/// TS: `getEditorTheme()`。
pub fn get_editor_theme() -> EditorTheme {
    EditorTheme {
        border_color: Arc::new(|text: &str| {
            THEME
                .fg(ThemeColor::BorderMuted, text)
                .unwrap_or_else(|_| text.to_string())
        }),
        select_list: get_select_list_theme(),
    }
}

/// TS: `getSettingsListTheme()`。
///
/// `label`/`value` 选中时用 `accent`，未选中的 `value` 用 `muted`；`description`/`hint` 用
/// `dim`。`cursor` 在**调用时**求值并固定，所以切换主题后光标颜色不会跟着变（其余闭包都
/// 在使用时读全局主题）。
pub fn get_settings_list_theme() -> SettingsListTheme {
    SettingsListTheme {
        label: Arc::new(|text: &str, selected: bool| {
            if selected {
                THEME
                    .fg(ThemeColor::Accent, text)
                    .unwrap_or_else(|_| text.to_string())
            } else {
                text.to_string()
            }
        }),
        value: Arc::new(|text: &str, selected: bool| {
            if selected {
                THEME
                    .fg(ThemeColor::Accent, text)
                    .unwrap_or_else(|_| text.to_string())
            } else {
                THEME
                    .fg(ThemeColor::Muted, text)
                    .unwrap_or_else(|_| text.to_string())
            }
        }),
        description: token_style(ThemeColor::Dim),
        // TS 在这里**立即**求值（不是闭包）：切换主题后 cursor 不会跟着变。
        cursor: THEME
            .fg(ThemeColor::Accent, "→ ")
            .unwrap_or_else(|_| "→ ".to_string()),
        hint: token_style(ThemeColor::Dim),
    }
}

// =============================================================================
// Global theme proxy
// =============================================================================

/// TS 的 `theme`：一个读 `globalThis` 的 Proxy。
///
/// 每个方法都读当前全局主题；未初始化时返回
/// `Theme not initialized. Call initTheme() first.`。
///
/// `ThemeProxy` 是零字段的 `Copy` 类型，本身不持有主题实例；需要稳定实例（例如跨主题切换
/// 保持同一份颜色）时用 [`current_theme`] 取 `Arc<Theme>`。返回闭包的方法（
/// `get_thinking_border_color` 等）在闭包被调用时才读取全局主题。
#[derive(Debug, Clone, Copy, Default)]
pub struct ThemeProxy;

/// TS: `export const theme`。
pub static THEME: ThemeProxy = ThemeProxy;

/// 取得全局主题代理（与 [`THEME`] 相同）。
pub fn theme() -> ThemeProxy {
    ThemeProxy
}

/// `Theme not initialized. Call initTheme() first.`
fn theme_not_initialized() -> ThemeError {
    ThemeError::new("Theme not initialized. Call initTheme() first.")
}

impl ThemeProxy {
    /// 当前主题实例；未初始化 → `Theme not initialized. Call initTheme() first.`。
    pub fn current(&self) -> Result<Arc<Theme>, ThemeError> {
        current_theme().ok_or_else(theme_not_initialized)
    }

    /// `theme.name`。
    pub fn name(&self) -> Result<Option<String>, ThemeError> {
        Ok(self.current()?.name().map(str::to_string))
    }

    /// `theme.sourcePath`。
    pub fn source_path(&self) -> Result<Option<PathBuf>, ThemeError> {
        Ok(self.current()?.source_path().map(Path::to_path_buf))
    }

    /// `theme.appearance`。
    pub fn appearance(&self) -> Result<ThemeAppearance, ThemeError> {
        Ok(self.current()?.appearance())
    }

    /// `theme.colors`。
    pub fn colors(&self) -> Result<Arc<IndexMap<ThemeToken, Color>>, ThemeError> {
        Ok(self.current()?.colors())
    }

    /// `theme.colors[token]`。
    pub fn color(&self, token: ThemeToken) -> Result<Option<Color>, ThemeError> {
        Ok(self.current()?.color(token))
    }

    /// `theme.fg(token, text)`。
    pub fn fg(&self, color: ThemeColor, text: &str) -> Result<String, ThemeError> {
        self.current()?.try_fg(color, text)
    }

    /// `theme.bg(token, text)`。
    pub fn bg(&self, color: ThemeBg, text: &str) -> Result<String, ThemeError> {
        self.current()?.try_bg(color, text)
    }

    /// `theme.style(text, options)`。
    pub fn style(&self, text: &str, options: &ThemeStyle) -> Result<String, ThemeError> {
        self.current()?.style(text, options)
    }

    /// `theme.bold(text)`。
    pub fn bold(&self, text: &str) -> Result<String, ThemeError> {
        Ok(self.current()?.bold(text))
    }

    /// `theme.italic(text)`。
    pub fn italic(&self, text: &str) -> Result<String, ThemeError> {
        Ok(self.current()?.italic(text))
    }

    /// `theme.underline(text)`。
    pub fn underline(&self, text: &str) -> Result<String, ThemeError> {
        Ok(self.current()?.underline(text))
    }

    /// `theme.inverse(text)`。
    pub fn inverse(&self, text: &str) -> Result<String, ThemeError> {
        Ok(self.current()?.inverse(text))
    }

    /// `theme.strikethrough(text)`。
    pub fn strikethrough(&self, text: &str) -> Result<String, ThemeError> {
        Ok(self.current()?.strikethrough(text))
    }

    /// `theme.getFgAnsi(token)`。
    pub fn get_fg_ansi(&self, color: ThemeColor) -> Result<String, ThemeError> {
        self.current()?.try_get_fg_ansi(color)
    }

    /// `theme.getBgAnsi(token)`。
    pub fn get_bg_ansi(&self, color: ThemeBg) -> Result<String, ThemeError> {
        self.current()?.try_get_bg_ansi(color)
    }

    /// `theme.getColorMode()`。
    pub fn get_color_mode(&self) -> Result<TerminalColorMode, ThemeError> {
        Ok(self.current()?.get_color_mode())
    }

    /// `theme.getThinkingBorderColor(level)`：返回的闭包每次读全局主题（与 TS 的 Proxy
    /// `this` 绑定一致）。
    ///
    /// 返回类型是 port 规格里固定的 `Box<dyn Fn(&str) -> String + Send + Sync>`。
    #[allow(clippy::type_complexity)]
    pub fn get_thinking_border_color(
        &self,
        level: &str,
    ) -> Result<Box<dyn Fn(&str) -> String + Send + Sync>, ThemeError> {
        self.current()?;
        let color = thinking_border_color_for(level);
        Ok(Box::new(move |text: &str| {
            THEME.fg(color, text).unwrap_or_else(|_| text.to_string())
        }))
    }

    /// `theme.getBashModeBorderColor()`。
    #[allow(clippy::type_complexity)]
    pub fn get_bash_mode_border_color(
        &self,
    ) -> Result<Box<dyn Fn(&str) -> String + Send + Sync>, ThemeError> {
        self.current()?;
        Ok(Box::new(|text: &str| {
            THEME
                .fg(ThemeColor::BashMode, text)
                .unwrap_or_else(|_| text.to_string())
        }))
    }
}

/// TS: `theme.getThinkingBorderColor(level)`（全局主题的简写）。
#[allow(clippy::type_complexity)]
pub fn get_thinking_border_color(
    level: &str,
) -> Result<Box<dyn Fn(&str) -> String + Send + Sync>, ThemeError> {
    THEME.get_thinking_border_color(level)
}

/// TS: `theme.getBashModeBorderColor()`（全局主题的简写）。
#[allow(clippy::type_complexity)]
pub fn get_bash_mode_border_color() -> Result<Box<dyn Fn(&str) -> String + Send + Sync>, ThemeError>
{
    THEME.get_bash_mode_border_color()
}

/// TS: `getThinkingBorderColor(level)` 的等级映射。
fn thinking_border_color_for(level: &str) -> ThemeColor {
    match level {
        "minimal" => ThemeColor::ThinkingMinimal,
        "low" => ThemeColor::ThinkingLow,
        "medium" => ThemeColor::ThinkingMedium,
        "high" => ThemeColor::ThinkingHigh,
        "xhigh" => ThemeColor::ThinkingXhigh,
        "max" => ThemeColor::ThinkingMax,
        _ => ThemeColor::ThinkingOff,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    /// 这些用例共享进程级全局状态（终端颜色、明暗、注册主题、全局主题、主题回调），
    /// 必须串行执行。
    fn test_lock() -> MutexGuard<'static, ()> {
        static TEST_LOCK: Mutex<()> = Mutex::new(());
        lock(&TEST_LOCK)
    }

    /// 与 TS 测试的 `afterEach` 相同：恢复终端颜色、明暗、注册主题、校验器和全局主题。
    fn reset() {
        set_terminal_colors(&TerminalColors::default());
        set_terminal_color_scheme(None);
        set_registered_themes(Vec::new()).expect("registered themes reset");
        clear_theme_json_validator();
        init_theme(Some("dark"), false);
    }

    /// 取出 `Err`，不要求 `Ok` 类型实现 `Debug`（闭包类型没有 `Debug`）。
    fn error_of<T>(result: Result<T, ThemeError>) -> ThemeError {
        match result {
            Ok(_) => panic!("expected an error"),
            Err(error) => error,
        }
    }

    #[test]
    fn highlight_code_without_a_valid_language_colorizes_every_line() {
        let _guard = test_lock();
        reset();
        let code = "let x = 1;\nlet y = 2;";
        let colorized: Vec<String> = code
            .split('\n')
            .map(|line| {
                THEME
                    .fg(ThemeColor::MdCodeBlock, line)
                    .expect("theme initialized")
            })
            .collect();
        assert_eq!(highlight_code(code, None), colorized);
        assert_eq!(
            highlight_code(code, Some("still-not-a-language")),
            colorized
        );
        assert_eq!(highlight_code(code, Some("")), colorized);
        // 空代码保留一行：`theme.fg(token, "")` 仍然包一层 ANSI（TS 的 `fg` 不特判空串）。
        assert_eq!(
            highlight_code("", None),
            vec![
                THEME
                    .fg(ThemeColor::MdCodeBlock, "")
                    .expect("theme initialized")
            ]
        );
    }

    #[test]
    fn highlight_code_highlights_known_languages() {
        let _guard = test_lock();
        reset();
        assert!(kiana_core::syntax_highlight::supports_language("rust"));
        let lines = highlight_code("const X: i32 = 1;", Some("rust"));
        assert_eq!(lines.len(), 1);
        assert!(lines[0].contains('\x1b'), "{}", lines[0]);
    }

    #[test]
    fn get_language_from_path_maps_extensions_like_the_reference_table() {
        assert_eq!(get_language_from_path("src/app.ts"), Some("typescript"));
        assert_eq!(get_language_from_path("a.TSX"), Some("typescript"));
        assert_eq!(get_language_from_path("x.py"), Some("python"));
        assert_eq!(get_language_from_path("x.rs"), Some("rust"));
        assert_eq!(get_language_from_path("x.MD"), Some("markdown"));
        assert_eq!(get_language_from_path("x.unknown"), None);
        assert_eq!(get_language_from_path("x."), None);
        assert_eq!(get_language_from_path(""), None);
        // TS 的 `split(".").pop()` 把无扩展名的文件名也当作扩展名来查表。
        assert_eq!(get_language_from_path("Makefile"), Some("makefile"));
    }

    #[test]
    fn markdown_and_list_themes_read_the_global_theme_lazily() {
        let _guard = test_lock();
        reset();
        let markdown = get_markdown_theme();
        assert_eq!(
            (markdown.heading)("x"),
            THEME
                .fg(ThemeColor::MdHeading, "x")
                .expect("theme initialized")
        );
        assert_eq!((markdown.bold)("x"), "\x1b[1mx\x1b[22m");
        assert_eq!((markdown.bold)(""), "");
        assert_eq!((markdown.italic)("x"), "\x1b[3mx\x1b[23m");
        assert_eq!((markdown.underline)("x"), "\x1b[4mx\x1b[24m");
        assert_eq!((markdown.strikethrough)("x"), "\x1b[9mx\x1b[29m");
        let highlight = markdown
            .highlight_code
            .as_ref()
            .expect("highlight callback");
        assert_eq!(
            highlight("let x = 1;", None),
            highlight_code("let x = 1;", None)
        );

        let select_list = get_select_list_theme();
        assert_eq!(
            (select_list.selected_text)("x"),
            THEME
                .fg(ThemeColor::Accent, "x")
                .expect("theme initialized")
        );
        assert_eq!(
            (select_list.description)("x"),
            THEME.fg(ThemeColor::Muted, "x").expect("theme initialized")
        );

        let editor = get_editor_theme();
        assert_eq!(
            (editor.border_color)("x"),
            THEME
                .fg(ThemeColor::BorderMuted, "x")
                .expect("theme initialized")
        );

        let settings = get_settings_list_theme();
        assert_eq!(
            (settings.label)("x", true),
            THEME
                .fg(ThemeColor::Accent, "x")
                .expect("theme initialized")
        );
        assert_eq!((settings.label)("x", false), "x");
        assert_eq!(
            (settings.value)("x", false),
            THEME.fg(ThemeColor::Muted, "x").expect("theme initialized")
        );
        assert_eq!(
            (settings.description)("x"),
            THEME.fg(ThemeColor::Dim, "x").expect("theme initialized")
        );
        assert_eq!(
            settings.cursor,
            THEME
                .fg(ThemeColor::Accent, "→ ")
                .expect("theme initialized")
        );

        // 闭包在使用时才读全局主题：切换主题前求的值属于 dark，切换后立刻换色。
        let dark_heading = (markdown.heading)("x");
        let dark = load_theme("dark", None).expect("builtin dark");
        assert_eq!(dark_heading, dark.fg(ThemeColor::MdHeading, "x"));
        assert!(set_theme("light", false).success);
        assert_eq!(
            (markdown.heading)("x"),
            THEME
                .fg(ThemeColor::MdHeading, "x")
                .expect("theme initialized")
        );
    }

    #[test]
    fn current_theme_tracks_the_global_theme_and_notifies_the_callback() {
        let _guard = test_lock();
        reset();
        assert_eq!(current_theme_name().as_deref(), Some("dark"));
        assert_eq!(
            current_theme().expect("theme initialized").name(),
            Some("dark")
        );

        let calls = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&calls);
        on_theme_change(Box::new(move || {
            counter.fetch_add(1, Ordering::SeqCst);
        }));

        assert!(set_theme("light", false).success);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(current_theme_name().as_deref(), Some("light"));
        assert_eq!(
            current_theme().expect("theme initialized").name(),
            Some("light")
        );

        // `setTheme` 失败时回退到 system，但 TS 不触发回调。
        assert!(!set_theme("does-not-exist", false).success);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(current_theme_name().as_deref(), Some("system"));
        assert_eq!(
            current_theme().expect("theme initialized").name(),
            Some("system")
        );
    }

    #[test]
    fn theme_proxy_reports_missing_initialization_and_delegates_to_the_current_theme() {
        let _guard = test_lock();
        reset();
        assert_eq!(
            THEME.name().expect("theme initialized").as_deref(),
            Some("dark")
        );
        assert_eq!(
            THEME.fg(ThemeColor::Text, "x").expect("theme initialized"),
            current_theme()
                .expect("theme initialized")
                .fg(ThemeColor::Text, "x")
        );
        assert_eq!(
            THEME.bold("x").expect("theme initialized"),
            "\x1b[1mx\x1b[22m"
        );
        assert_eq!(
            THEME.get_color_mode().expect("theme initialized"),
            get_terminal_color_mode_default()
        );

        // 未初始化时的逐字文案（TS: `Theme not initialized. Call initTheme() first.`）。
        let previous = current_theme();
        *write(&THEME_STATE) = None;
        let errors = [
            error_of(THEME.name()),
            error_of(THEME.appearance()),
            error_of(THEME.colors()),
            error_of(THEME.style("x", &ThemeStyle::default())),
            error_of(THEME.get_thinking_border_color("high")),
            error_of(THEME.get_bash_mode_border_color()),
        ];
        *write(&THEME_STATE) = previous;
        for error in errors {
            assert_eq!(
                error.to_string(),
                "Theme not initialized. Call initTheme() first."
            );
        }
    }

    /// 回归（gate 评审 1/2/5/6）：V8 的 JSON.parse 文案逐字复刻。
    ///
    /// 期望值全部来自 node 26.12.1（V8）的 `JSON.parse()` 实测，包括：
    /// - `true`/`false`/`null` 字面量在错配字符处的 `Unexpected number`/`Unexpected string`；
    /// - 单独的 `\r`（以及 `\r\n` 作为一个）也计入行号；
    /// - 整串 `NaN`/`Infinity`/`undefined`/`[object Object]` 的特殊文案；
    /// - `\` 后跟 U+2028/U+2029 时是 `Unexpected token`（只有 Latin-1 之外的转义字符走这里）。
    #[test]
    fn reports_v8_json_syntax_errors_like_the_reference() {
        let cases = [
            (
                "{",
                "Expected property name or '}' in JSON at position 1 (line 1 column 2)",
            ),
            (
                "{\"a\":}",
                "Unexpected token '}', \"{\"a\":}\" is not valid JSON",
            ),
            (
                "{\"a\":1,}",
                "Expected double-quoted property name in JSON at position 7 (line 1 column 8)",
            ),
            (
                "{\"a\" 1}",
                "Expected ':' after property name in JSON at position 5 (line 1 column 6)",
            ),
            (
                "{a:1}",
                "Expected property name or '}' in JSON at position 1 (line 1 column 2)",
            ),
            ("[1,]", "Unexpected token ']', \"[1,]\" is not valid JSON"),
            (
                "{\"a\":1}x",
                "Unexpected non-whitespace character after JSON at position 7 (line 1 column 8)",
            ),
            ("", "Unexpected end of JSON input"),
            ("   ", "Unexpected end of JSON input"),
            ("nul", "Unexpected end of JSON input"),
            (
                "{\"a\":01}",
                "Unexpected number in JSON at position 6 (line 1 column 7)",
            ),
            (
                "{\"a\":+1}",
                "Unexpected token '+', \"{\"a\":+1}\" is not valid JSON",
            ),
            (
                "{\"a\":1.}",
                "Unterminated fractional number in JSON at position 7 (line 1 column 8)",
            ),
            (
                "{\"a\":NaN}",
                "Unexpected token 'N', \"{\"a\":NaN}\" is not valid JSON",
            ),
            ("'a'", "Unexpected token ''', \"'a'\" is not valid JSON"),
            (
                "{\"a\":1 \"b\":2}",
                "Expected ',' or '}' after property value in JSON at position 7 (line 1 column 8)",
            ),
            (
                "{\"a\":tru}",
                "Unexpected token '}', \"{\"a\":tru}\" is not valid JSON",
            ),
            (
                "{\"a\":\"x\ny\"}",
                "Bad control character in string literal in JSON at position 7 (line 1 column 8)",
            ),
            (
                "{\"a\":1}\n}",
                "Unexpected non-whitespace character after JSON at position 8 (line 2 column 1)",
            ),
            (
                "{\"a\"",
                "Expected ':' after property name in JSON at position 4 (line 1 column 5)",
            ),
            ("{\"a\":", "Unexpected end of JSON input"),
            (
                "{\"a\":1",
                "Expected ',' or '}' after property value in JSON at position 6 (line 1 column 7)",
            ),
            (
                "[1",
                "Expected ',' or ']' after array element in JSON at position 2 (line 1 column 3)",
            ),
            (
                "\"abc",
                "Unterminated string in JSON at position 4 (line 1 column 5)",
            ),
            ("\"a\\", "Unexpected end of JSON input"),
            (
                "\"a\\u12",
                "Bad Unicode escape in JSON at position 6 (line 1 column 7)",
            ),
            (
                "\"a\\u12zz\"",
                "Bad Unicode escape in JSON at position 6 (line 1 column 7)",
            ),
            (
                "\"a\\q\"",
                "Bad escaped character in JSON at position 3 (line 1 column 4)",
            ),
            (
                "{\"a\":-}",
                "No number after minus sign in JSON at position 6 (line 1 column 7)",
            ),
            (
                "{\"a\":-",
                "No number after minus sign in JSON at position 6 (line 1 column 7)",
            ),
            (
                "{\"a\":1e}",
                "Exponent part is missing a number in JSON at position 7 (line 1 column 8)",
            ),
            (
                "{\"a\":1e+}",
                "Exponent part is missing a number in JSON at position 8 (line 1 column 9)",
            ),
            (
                "{\"a\":0x10}",
                "Expected ',' or '}' after property value in JSON at position 6 (line 1 column 7)",
            ),
            (
                "{\"a\": {,}}",
                "Expected property name or '}' in JSON at position 7 (line 1 column 8)",
            ),
            (
                "   {\"a\": 1, \"b\": }   ",
                "Unexpected token '}', ...\": 1, \"b\": }   \" is not valid JSON",
            ),
            // UTF-16 位置：😀 占两个 code unit。
            (
                "{\"a\":\"😀\",}",
                "Expected double-quoted property name in JSON at position 10 (line 1 column 11)",
            ),
            // gate 评审 1：字面量错配到数字/字符串起始字符时的专属文案。
            (
                "f5",
                "Unexpected number in JSON at position 1 (line 1 column 2)",
            ),
            (
                "t1",
                "Unexpected number in JSON at position 1 (line 1 column 2)",
            ),
            (
                "n1",
                "Unexpected number in JSON at position 1 (line 1 column 2)",
            ),
            (
                "f\"abc\"",
                "Unexpected string in JSON at position 1 (line 1 column 2)",
            ),
            (
                "{\"a\":n5}",
                "Unexpected number in JSON at position 6 (line 1 column 7)",
            ),
            ("fab", "Unexpected token 'b', \"fab\" is not valid JSON"),
            // gate 评审 2：单独的 \r 也是换行；\r\n 只算一次。
            (
                "{\"a\":1}\rx",
                "Unexpected non-whitespace character after JSON at position 8 (line 2 column 1)",
            ),
            (
                "{\"a\":1}\r\nx",
                "Unexpected non-whitespace character after JSON at position 9 (line 2 column 1)",
            ),
            (
                "{\r\"a\" 1}",
                "Expected ':' after property name in JSON at position 6 (line 2 column 5)",
            ),
            (
                "{\r\n\"a\":1\r\n",
                "Expected ',' or '}' after property value in JSON at position 10 (line 3 column 1)",
            ),
            // gate 评审 6：整串特例（V8 在派发语法之前先比对整串）。
            ("NaN", "\"NaN\" is not valid JSON"),
            ("Infinity", "\"Infinity\" is not valid JSON"),
            ("undefined", "\"undefined\" is not valid JSON"),
            ("[object Object]", "\"[object Object]\" is not valid JSON"),
            (" NaN", "Unexpected token 'N', \" NaN\" is not valid JSON"),
            (
                "{\"a\":Infinity}",
                "Unexpected token 'I', \"{\"a\":Infinity}\" is not valid JSON",
            ),
            // gate 评审 5：\ 后跟 U+2028/U+2029 走 Unexpected token（V8 只允许 Latin-1 转义）。
            (
                "\"a\\\u{2028}b\"",
                "Unexpected token '\u{2028}', \"\"a\\\u{2028}b\"\" is not valid JSON",
            ),
            (
                "\"a\\\u{2029}b\"",
                "Unexpected token '\u{2029}', \"\"a\\\u{2029}b\"\" is not valid JSON",
            ),
            (
                "\"a\\ b\"",
                "Bad escaped character in JSON at position 3 (line 1 column 4)",
            ),
            // gate 评审 4：非 BMP 的 token 在 V8 里是孤立高代理项，Node 以 UTF-8 输出时
            // 编码成 EF BF BD（U+FFFD），因此 Rust 字符串用 U+FFFD 表示即可得到逐字节一致的输出。
            (
                "😀",
                "Unexpected token '\u{fffd}', \"😀\" is not valid JSON",
            ),
            (
                "[😀]",
                "Unexpected token '\u{fffd}', \"[😀]\" is not valid JSON",
            ),
        ];
        for (source, expected) in cases {
            assert_eq!(
                parse_theme_json_content("lbl", source)
                    .expect_err("invalid JSON")
                    .to_string(),
                format!("Failed to parse theme lbl: SyntaxError: {expected}"),
                "source: {source:?}"
            );
        }

        // 长文档的片段截断：错误位置前后各 10 个单元，两侧 `...`。
        let mut long = String::from("{\"a\":");
        long.push_str(&" ".repeat(10));
        long.push('}');
        long.push_str(&" ".repeat(50));
        assert_eq!(
            parse_theme_json_content("lbl", &long)
                .expect_err("invalid JSON")
                .to_string(),
            "Failed to parse theme lbl: SyntaxError: Unexpected token '}', ...\"          }         \"... is not valid JSON"
        );
    }

    /// 回归（gate 评审 3）：JS `JSON.parse` 接受、serde_json 拒绝的文档也必须接受。
    ///
    /// 两处偏差按 ARCHITECTURE §8 第 31/32 条：非有限数存成 `null`（于是顶层 `1e999`
    /// 走 TS 的 "expected an object with a \"colors\" map."），孤立代理项转义替换成 U+FFFD
    /// （后续颜色解析的报错与 Node 输出的 UTF-8 字节一致）。
    #[test]
    fn accepts_documents_that_json_parse_accepts() {
        let _guard = test_lock();
        for source in ["1e999", "-1e999", "[1e309]"] {
            let error = parse_theme_json_content("lbl", source).expect_err("not an object");
            assert_eq!(
                error.to_string(),
                "Invalid theme \"lbl\": expected an object with a \"colors\" map.",
                "source: {source:?}"
            );
        }

        // 孤立代理项：主题对象本身可以加载；作为颜色值时是变量引用错误（U+FFFD 的 UTF-8
        // 字节与 Node 打印孤立代理项时相同）。
        let theme = parse_theme_json_content("lbl", "{\"colors\":{\"accent\":\"\\ud800\"}}")
            .expect("JSON.parse accepts a lone surrogate escape");
        assert_eq!(
            crate::theme_schema::resolve_theme_colors_from_document(&theme.raw)
                .expect_err("lone surrogate is not a color")
                .to_string(),
            "Variable reference not found: \u{fffd}"
        );

        // 合法的代理对保持为一个字符。
        let theme = parse_theme_json_content("lbl", "{\"colors\":{\"accent\":\"\\ud83d\\ude00\"}}")
            .expect("surrogate pair");
        assert_eq!(
            theme.colors.get(&ThemeToken::Accent),
            Some(&crate::theme_schema::ThemeColorValue::Str("😀".to_string()))
        );

        // `1e999` 作为颜色值在安装校验器时与 TS 的 Infinity 报同一组错误。
        set_theme_json_validator(Box::new(crate::theme_schema::validate_theme_json));
        let error =
            parse_theme_json_content("lbl", "{\"name\":\"x\",\"colors\":{\"accent\":1e999}}")
                .expect_err("validator rejects");
        clear_theme_json_validator();
        assert_eq!(
            error.to_string(),
            "Invalid theme \"lbl\":\n\nMissing required color tokens:\n  - bashMode\n  - border\n  - borderAccent\n  - borderMuted\n  - customMessageBg\n  - customMessageLabel\n  - customMessageText\n  - dim\n  - error\n  - mdCode\n  - mdCodeBlock\n  - mdCodeBlockBorder\n  - mdHeading\n  - mdHr\n  - mdLink\n  - mdLinkUrl\n  - mdListBullet\n  - mdQuote\n  - mdQuoteBorder\n  - muted\n  - selectedBg\n  - success\n  - syntaxComment\n  - syntaxFunction\n  - syntaxKeyword\n  - syntaxNumber\n  - syntaxOperator\n  - syntaxPunctuation\n  - syntaxString\n  - syntaxType\n  - syntaxVariable\n  - text\n  - thinkingHigh\n  - thinkingLow\n  - thinkingMedium\n  - thinkingMinimal\n  - thinkingOff\n  - thinkingText\n  - thinkingXhigh\n  - toolDiffAdded\n  - toolDiffContext\n  - toolDiffRemoved\n  - toolErrorBg\n  - toolOutput\n  - toolPendingBg\n  - toolSuccessBg\n  - toolTitle\n  - userMessageBg\n  - userMessageText\n  - warning\n\nPlease add these colors to your theme's \"colors\" object.\nSee the built-in themes (dark.json, light.json) for reference values.\n\nOther errors:\n  - /colors/accent: must be string\n  - /colors/accent: must be integer\n  - /colors/accent: must match a schema in anyOf"
        );
    }

    /// 回归（gate 评审 7）：`/^okhsl\(/i` 与非 unicode JS 正则一样只折叠 ASCII。
    #[test]
    fn okhsl_literal_detection_is_ascii_case_insensitive_only() {
        assert!(is_okhsl_literal("okhsl(50 50% 50%)"));
        assert!(is_okhsl_literal("OKHSL(50 50% 50%)"));
        assert!(is_okhsl_literal("OkHsL("));
        assert!(!is_okhsl_literal("okhsl"));
        assert!(!is_okhsl_literal("okh\u{17f}l(50 50% 50%)"));
        assert!(!is_okhsl_literal("o\u{212a}hsl(50 50% 50%)"));
    }

    #[test]
    fn parse_theme_json_content_reports_syntax_errors_and_strips_the_bom() {
        // `parse_theme_json` 读取进程级校验器（`set_theme_json_validator`），
        // 与安装校验器的用例共享全局状态，必须串行执行。
        let _guard = test_lock();
        let error = parse_theme_json_content("broken", "{").expect_err("invalid JSON");
        assert!(
            error
                .to_string()
                .starts_with("Failed to parse theme broken: SyntaxError: "),
            "{error}"
        );

        let json = serde_json::json!({ "colors": {} });
        let theme_json = parse_theme_json("bom", &json).expect("colors accepted");
        assert!(theme_json.colors.is_empty());
        // 文件开头的 BOM 在解析前被去掉（`kiana_core::text::strip_bom`）。
        let content = format!("\u{feff}{}", serde_json::to_string(&json).expect("json"));
        let parsed = parse_theme_json_content("bom", &content).expect("BOM stripped");
        assert!(parsed.colors.is_empty());

        // 没有安装校验器时只要求一个带 `colors` 的对象。
        let error = parse_theme_json("custom", &serde_json::json!({ "name": "custom" }))
            .expect_err("missing colors");
        assert_eq!(
            error.to_string(),
            "Invalid theme \"custom\": expected an object with a \"colors\" map."
        );
    }

    #[test]
    fn reads_invalid_utf8_like_nodes_utf8_decoder() {
        // Node 的 `readFileSync(path, "utf-8")` 把非法字节替换成 U+FFFD；主题照常加载。
        let bytes = DARK_JSON.as_bytes();
        let index = bytes
            .windows(6)
            .position(|window| window == b"\"dark\"")
            .expect("name value");
        let mut corrupted = Vec::with_capacity(bytes.len());
        corrupted.extend_from_slice(&bytes[..index + 1]);
        corrupted.push(0xff);
        corrupted.extend_from_slice(&bytes[index + 5..]);
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("broken.json");
        std::fs::write(&path, &corrupted).expect("write theme");
        let theme = load_theme_from_path(&path, None).expect("lossy decoding");
        assert_eq!(theme.name(), Some("\u{fffd}"));
    }

    /// 回归 #5596 的主题侧：配置的主题不存在时 `initTheme()` 静默回退到 system，
    /// `getResolvedThemeColors()` / `getThemeExportColors()` 仍能工作（HTML 导出不失败）。
    #[test]
    fn init_theme_falls_back_to_system_for_a_missing_theme() {
        let _guard = test_lock();
        reset();
        init_theme(Some("missing-theme"), false);
        assert_eq!(current_theme_name().as_deref(), Some(SYSTEM_THEME_NAME));
        assert_eq!(
            current_theme().expect("theme").name(),
            Some(SYSTEM_THEME_NAME)
        );
        assert!(get_resolved_theme_colors(None).is_ok());
        assert_eq!(get_theme_export_colors(None), ThemeExportColors::default());
    }

    #[test]
    fn load_theme_json_reports_unknown_and_source_less_themes() {
        let _guard = test_lock();
        reset();
        let error = load_theme_json("no-such-theme").expect_err("missing theme");
        assert_eq!(error.to_string(), "Theme not found: no-such-theme");

        // 注册的主题如果没有 sourcePath，导出时报专门的错误。
        let mut dark_json: serde_json::Value =
            serde_json::from_str(DARK_JSON).expect("dark.json parses");
        dark_json["name"] = serde_json::json!("registered-no-source");
        let theme_json = parse_theme_json("registered-no-source", &dark_json).expect("theme json");
        let theme = create_theme(&theme_json, Some(TerminalColorMode::Truecolor), None)
            .expect("theme built");
        assert!(theme.source_path().is_none());
        set_registered_themes(vec![Arc::new(theme)]).expect("registered themes set");
        let error = load_theme_json("registered-no-source").expect_err("no source path");
        assert_eq!(
            error.to_string(),
            "Theme \"registered-no-source\" does not have a source path for export"
        );

        // 内置主题总是可读。
        let builtin = load_theme_json("dark").expect("builtin dark");
        assert_eq!(builtin.name.as_deref(), Some("dark"));
    }

    #[test]
    fn assert_theme_name_is_valid_rejects_slashes() {
        let error = assert_theme_name_is_valid("a/b").expect_err("slash rejected");
        assert_eq!(
            error.to_string(),
            "Invalid theme name \"a/b\": theme names cannot contain \"/\" because it is reserved for automatic light/dark theme settings."
        );
        assert!(assert_theme_name_is_valid("solarized-dark").is_ok());
    }

    #[test]
    fn is_light_theme_uses_the_theme_appearance() {
        let _guard = test_lock();
        reset();
        assert!(is_light_theme(Some("light")).expect("light theme"));
        assert!(!is_light_theme(Some("dark")).expect("dark theme"));
        // 省略名字时用当前主题。
        init_theme(Some("light"), false);
        assert!(is_light_theme(None).expect("current theme"));
        let error = is_light_theme(Some("no-such-theme")).expect_err("missing theme");
        assert_eq!(error.to_string(), "Theme not found: no-such-theme");
    }

    #[test]
    fn border_color_helpers_map_thinking_levels_and_delegate_to_the_proxy() {
        let _guard = test_lock();
        reset();
        let high = get_thinking_border_color("high").expect("theme initialized");
        assert_eq!(
            high("x"),
            THEME
                .fg(ThemeColor::ThinkingHigh, "x")
                .expect("theme initialized")
        );
        // 未知等级回退到 `thinkingOff`。
        let unknown = get_thinking_border_color("unexpected").expect("theme initialized");
        assert_eq!(
            unknown("x"),
            THEME
                .fg(ThemeColor::ThinkingOff, "x")
                .expect("theme initialized")
        );
        let bash = get_bash_mode_border_color().expect("theme initialized");
        assert_eq!(
            bash("x"),
            THEME
                .fg(ThemeColor::BashMode, "x")
                .expect("theme initialized")
        );
        // `theme()` 返回同一份全局代理。
        assert_eq!(
            theme().name().expect("theme initialized"),
            THEME.name().expect("theme initialized")
        );
        // `stopThemeWatcher()` 可以重复调用（没有 watcher 时是空操作）。
        stop_theme_watcher();
        stop_theme_watcher();
    }

    /// 用一份完整的文档构造主题（`Theme::new` 需要所有 fallback token 可解析）。
    fn theme_from_document(mut document: serde_json::Value) -> Arc<Theme> {
        let name = document
            .get("name")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("test")
            .to_string();
        document["name"] = serde_json::Value::String(name);
        let theme_json = parse_theme_json("test", &document).expect("theme json");
        Arc::new(
            create_theme(&theme_json, Some(TerminalColorMode::Truecolor), None)
                .expect("theme builds"),
        )
    }

    #[test]
    fn built_in_names_that_live_on_object_prototype_fail_like_javascript() {
        let _guard = test_lock();
        reset();
        // TS `name in builtinThemes` 会命中 `Object.prototype`，`createTheme()` 随后在
        // `Object.entries(undefined)` 上抛错；这些名字永远不会落到自定义主题目录。
        for name in [
            "toString",
            "constructor",
            "valueOf",
            "hasOwnProperty",
            "isPrototypeOf",
            "__proto__",
        ] {
            assert_eq!(
                error_of(load_theme_json(name)).to_string(),
                "Cannot convert undefined or null to object",
                "name: {name}"
            );
            assert_eq!(
                set_theme(name, false),
                ThemeResult::failed("Cannot convert undefined or null to object"),
                "name: {name}"
            );
            assert!(get_theme_by_name(name).is_none(), "name: {name}");
        }
        // 真正的内置主题（自有属性）不受影响。
        assert!(load_theme_json("dark").is_ok());
        assert!(load_theme_json("light").is_ok());
    }

    #[test]
    fn js_trim_semantics_apply_to_auto_settings_and_colorfgbg() {
        // U+FEFF（BOM）在 JS 的 trim() 里被去掉；U+0085（NEL）不会被去掉（与 Rust 相反）。
        assert_eq!(
            parse_auto_theme_setting(Some("\u{feff}dark/light")),
            Some(AutoTheme {
                light_theme: "dark".to_string(),
                dark_theme: "light".to_string(),
            })
        );
        assert_eq!(
            parse_auto_theme_setting(Some("light\u{0085}/dark"))
                .expect("parsed")
                .light_theme,
            "light\u{0085}"
        );
        assert_eq!(
            parse_auto_theme_setting(Some("dark/\u{feff}light"))
                .expect("parsed")
                .dark_theme,
            "light"
        );
        assert_eq!(
            detect_color_fg_bg_theme_from(Some("0;\u{feff}15")),
            Some(TerminalColorScheme::Light)
        );
        assert_eq!(detect_color_fg_bg_theme_from(Some("0;15\u{0085}")), None);
    }

    #[test]
    fn raw_name_and_appearance_follow_the_unvalidated_document() {
        let _guard = test_lock();
        reset();
        let dir = tempfile::tempdir().expect("temp dir");

        // `"name": 5` / `"appearance": "Dark"`：TS 原样保存在 Theme 上。
        let mut document: serde_json::Value = serde_json::from_str(DARK_JSON).expect("dark.json");
        document["name"] = serde_json::json!(5);
        document["appearance"] = serde_json::json!("Dark");
        let path = dir.path().join("raw.json");
        std::fs::write(&path, serde_json::to_string(&document).expect("json")).expect("write");
        let theme =
            load_theme_from_path(&path, Some(TerminalColorMode::Truecolor)).expect("theme loads");
        assert_eq!(theme.name(), None);
        assert_eq!(theme.raw_name(), Some(&serde_json::json!(5)));
        assert_eq!(theme.raw_appearance(), Some(&serde_json::json!("Dark")));
        assert_eq!(theme.appearance(), TerminalColorScheme::Dark);

        // `isLightTheme()` 是 `theme.appearance === "light"`：原始值不是 "light" 时为 false。
        let mut light_document: serde_json::Value =
            serde_json::from_str(LIGHT_JSON).expect("light.json");
        light_document["name"] = serde_json::json!("raw-dark-appearance");
        light_document["appearance"] = serde_json::json!("Dark");
        let theme = theme_from_document(light_document);
        set_registered_themes(vec![Arc::clone(&theme)]).expect("registered");
        assert_eq!(is_light_theme(Some("raw-dark-appearance")), Ok(false));

        // 合法字面量仍按字面量比较；省略/`null` 时回退到检测结果。
        let mut light_document: serde_json::Value =
            serde_json::from_str(LIGHT_JSON).expect("light.json");
        light_document["name"] = serde_json::json!("raw-light");
        let theme = theme_from_document(light_document);
        set_registered_themes(vec![theme]).expect("registered");
        assert_eq!(is_light_theme(Some("raw-light")), Ok(true));

        let mut dark_document: serde_json::Value = serde_json::from_str(DARK_JSON).expect("dark");
        dark_document["name"] = serde_json::json!("raw-null-appearance");
        dark_document["appearance"] = serde_json::Value::Null;
        let theme = theme_from_document(dark_document);
        set_registered_themes(vec![theme]).expect("registered");
        assert_eq!(is_light_theme(Some("raw-null-appearance")), Ok(false));
        reset();
    }

    #[test]
    fn source_info_is_settable_and_background_ansi_is_exposed() {
        let _guard = test_lock();
        reset();
        let document: serde_json::Value = serde_json::from_str(DARK_JSON).expect("dark.json");
        let theme = theme_from_document(document);
        assert_eq!(theme.source_info(), None);
        let info = kiana_core::source_info::SourceInfo {
            path: "<builtin:themes/dark.json>".to_string(),
            source: "builtin".to_string(),
            scope: kiana_core::source_info::SourceScope::User,
            origin: kiana_core::source_info::SourceOrigin::TopLevel,
            base_dir: None,
        };
        theme.set_source_info(Some(info.clone()));
        assert_eq!(theme.source_info(), Some(info));
        theme.set_source_info(None);
        assert_eq!(theme.source_info(), None);

        // `getBgAnsi()` 是 `bg()` 去掉文本与关闭序列后的前缀。
        let ansi = theme.get_bg_ansi(ThemeBg::UserMessageBg);
        assert_eq!(
            theme.bg(ThemeBg::UserMessageBg, "x"),
            format!("{ansi}x\x1b[49m")
        );
    }

    /// 回归：`initTheme(name, true)` 打开 watcher，文件变化经 100 ms 去抖后重载并通知。
    #[test]
    fn watcher_hot_reloads_a_custom_theme_file() {
        struct EnvGuard {
            key: &'static str,
            previous: Option<std::ffi::OsString>,
        }
        impl EnvGuard {
            fn set(key: &'static str, value: &std::path::Path) -> Self {
                let previous = std::env::var_os(key);
                // SAFETY: 所有修改环境变量的测试都在 `test_lock()` 下串行执行。
                unsafe { std::env::set_var(key, value) };
                EnvGuard { key, previous }
            }
        }
        impl Drop for EnvGuard {
            fn drop(&mut self) {
                // SAFETY: 同上。
                match &self.previous {
                    Some(value) => unsafe { std::env::set_var(self.key, value) },
                    None => unsafe { std::env::remove_var(self.key) },
                }
            }
        }

        let _guard = test_lock();
        reset();
        let dir = tempfile::tempdir().expect("temp dir");
        let themes_dir = dir.path().join("themes");
        std::fs::create_dir_all(&themes_dir).expect("themes dir");
        let _env = EnvGuard::set("KIANA_CODING_AGENT_DIR", dir.path());

        let name = "watched-theme";
        let path = themes_dir.join(format!("{name}.json"));
        let mut document: serde_json::Value = serde_json::from_str(DARK_JSON).expect("dark.json");
        document["name"] = serde_json::json!(name);
        document["colors"]["accent"] = serde_json::json!("#112233");
        std::fs::write(&path, serde_json::to_string(&document).expect("json")).expect("write");

        init_theme(Some(name), true);
        assert_eq!(current_theme_name().as_deref(), Some(name));
        assert_eq!(
            current_theme().expect("theme").color(ThemeToken::Accent),
            Some(parse_color("#112233").expect("color"))
        );

        let changes = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counter = Arc::clone(&changes);
        on_theme_change(Box::new(move || {
            counter.fetch_add(1, Ordering::SeqCst);
        }));

        // 改文件 → 等去抖后的重载（超时给足，避免 CI 抖动）。
        document["colors"]["accent"] = serde_json::json!("#ff0000");
        std::fs::write(&path, serde_json::to_string(&document).expect("json")).expect("write");

        let deadline = Instant::now() + Duration::from_secs(10);
        let expected = Some(parse_color("#ff0000").expect("color"));
        while current_theme().expect("theme").color(ThemeToken::Accent) != expected {
            assert!(
                Instant::now() < deadline,
                "theme file change was not hot-reloaded"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(changes.load(Ordering::SeqCst) >= 1);

        stop_theme_watcher();
        // 清掉回调，避免影响同进程的其它用例。
        on_theme_change(Box::new(|| {}));
        reset();
    }
}
