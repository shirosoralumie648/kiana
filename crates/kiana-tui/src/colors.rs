//! `colors.ts`：颜色解析、色彩空间变换与 ANSI 序列拼接。
//!
//! 类型定义在 [`crate::terminal_colors`]（与 `terminal-colors.ts` 合并，见 `ARCHITECTURE.md` §3），
//! 本模块只提供函数：
//!
//! | TS | Rust |
//! |---|---|
//! | `indexedColor` / `rgbColor` / `oklchColor` / `okhslColor` | [`indexed_color`] / [`rgb_color`] / [`oklch_color`] / [`okhsl_color`] |
//! | `parseColor` | [`parse_color`] |
//! | `colorToRgb` / `colorToOklch` / `colorToOkhsl` / `colorToHex` | 同名 snake_case |
//! | `mixColors` | [`mix_colors`] |
//! | `foregroundAnsi` / `backgroundAnsi` / `styleText` / `styleTextWithAnsi` | 同名 snake_case |
//!
//! 色彩数学（OKLab/OKHSL）在 `oklab.rs`；本文件只做分派与 ANSI 编码。
//!
//! ## 与 TS 的两处签名差异（骨架已冻结，见 `ARCHITECTURE.md` §4.9）
//!
//! 1. TS 的构造函数与 `parseColor` 会抛异常（`ANSI color index must be an integer from 0 to
//!    255: 256` 之类）；冻结签名收窄了入参（`index: u8`、`parse_color(&str) -> Option<Color>`），
//!    因此这些逐字文案在本模块里没有承载点——校验由调用方在解析配置/主题时完成。
//!    数值校验保留在 [`parse_color`] 的 OKLCH/OKHSL 分支里：非法值返回 `None`，与
//!    `parseColor` 对非法输入的表现一致（TS 抛错，调用方按"无效颜色"处理）。
//! 2. TS 的 `styleTextWithAnsi(text, fgAnsi, bgAnsi, options)` 收**已格式化的** ANSI 片段；
//!    冻结签名是 `(text, options, mode)`，因此这里重新从 `options.fg`/`options.bg` 生成片段。
//!    单元格检查（`kiana-theme` 的缓存色）走 [`style_text_with_parts`]，它保留 TS 的四参数形态。
//!
//! ## 数据流
//!
//! 1. **解析**：[`parse_color`] 只识别 `#rgb`、`#rrggbb`、`oklch(...)`、`okhsl(...)` 四种写法，
//!    大小写不敏感且整串匹配（前后有多余字符即失败）；没有 `rgb()`、命名色或 `oklab()`。
//! 2. **构造**：[`indexed_color`] / [`rgb_color`] / [`oklch_color`] / [`okhsl_color`] 只做封装，
//!    不校验通道范围；`okhsl_color` 会立即换算成 sRGB（[`Color::Rgb`]）。
//! 3. **变换**：`color_to_*` 把任意 [`Color`] 统一到 sRGB / OKLCH / OKHSL。OKLCH 超出 sRGB
//!    色域时走 `oklch_to_rgb` 的色度二分：保持色相、降低色度，不会失败。
//! 4. **编码**：`foreground_ansi` / `background_ansi` 生成 SGR，`style_text*` 追加属性与复位。
//!
//! ## 注意事项
//!
//! - [`Color::Rgb`] 的通道是 `f64`（对应 TS 的 number），允许分数与越界值；
//!   [`crate::terminal_colors::RgbColor`]（`u8`）是最终 sRGB 值的类型（查表常量与各层
//!   返回类型都使用它）；把浮点通道写成 `u8` 的路径统一按 `Math.round` 取整。混色与
//!   OKLab 变换必须走 `color_to_rgb_float` 保留分数，否则 `mixColors(..., "srgb")` 的
//!   结果会被提前截断。
//! - `Indexed` 颜色在 ANSI 编码里**不经过** `mode`：无论 `Palette256` 还是 `Truecolor`
//!   都输出 `38;5;n` / `48;5;n`；`mode` 只影响 RGB / OKLCH 颜色。
//! - `style_text_with_parts` 的复位序列按开启顺序的逆序前置拼装；属性判定只认 `Some(true)`。
//!
//! 测试入口：`crates/kiana-tui/tests/colors.rs`（对齐 TS 的 4 个用例）与
//! `crates/kiana-tui/tests/regression_review_fixes.rs`（Unicode 空白、OKHSL 往返、分数通道取整）。

use crate::oklab::{
    js_hypot, js_max, js_min, js_round, linear_srgb_to_rgb, okhsl_to_rgb, oklab_to_linear_srgb,
    rgb_channels_to_okhsl, rgb_channels_to_oklab,
};
use crate::terminal_colors::RgbColor;
use kiana_json::format_js_number;

// 类型定义在 `crate::terminal_colors`（见该模块文档）；这里按 TS `colors.ts` 的导出面再导出，
// 让下游可以写 `kiana_tui::colors::Color`。`crate::index` 也是从本模块转出这些名字的。
pub use crate::terminal_colors::{
    Color, ColorMixSpace, IndexedColor, OkhslChannels, OklchChannels, RgbColorValue,
    TerminalColorMode, TextAttributes, TextStyle,
};

/// `indexedColor(index)`。
///
/// 只保存索引、不换算 RGB：索引含义（16 基色 / 6×6×6 色立方 / 24 级灰阶）由私有的
/// `indexed_to_rgb` 解释，只有 `color_to_*` 或 ANSI 编码需要具体色值时才查表。
pub fn indexed_color(index: u8) -> Color {
    Color::Indexed { index }
}

/// `rgbColor(r, g, b)`。
///
/// 不校验、不裁剪通道：`Color::Rgb` 原样保留传入的 `f64`（可以是分数或越界值）。
/// 需要具体字节时由 [`color_to_rgb`] / [`color_to_hex`] / ANSI 编码按各自规则转换
/// （前两者与 truecolor 编码按 `Math.round` 取整，256 色量化直接用分数值找最近档）。
pub fn rgb_color(r: f64, g: f64, b: f64) -> Color {
    Color::Rgb { r, g, b }
}

/// `oklchColor(l, c, h)`：`h` 归一化到 `[0, 360)`（色相是环形量，归一化后便于比较与编码）。
pub fn oklch_color(l: f64, c: f64, h: f64) -> Color {
    Color::Oklch {
        l,
        c,
        h: ((h % 360.0) + 360.0) % 360.0,
    }
}

/// `okhslColor(h, s, l)`：转换为 sRGB 色值返回。
///
/// `h` 的归一化（`(h % 360 + 360) % 360`）与 `s`/`l` 的范围处理都在
/// `crate::oklab::okhsl_to_rgb` 内部，本函数只做分派。正常输入下返回的 [`Color::Rgb`]
/// 通道是 0-255 的整数值（`f64` 存储）。不会 panic：`l` 为 `NaN`（或 `h` 为 `NaN` 且
/// `s > 0`）时通道为 `NaN`；`s` 为 `NaN` 时按 0 处理，输出有限值。
pub fn okhsl_color(h: f64, s: f64, l: f64) -> Color {
    let [r, g, b] = okhsl_to_rgb(h, s, l);
    Color::Rgb { r, g, b }
}

/// JS 正则 `\s` 的字符类（25 个码点，与 [`crate::text::is_js_whitespace`] 同一集合）。
///
/// 不能直接用 `regex` 的 `\s`：那是 Unicode `White_Space`，多 U+0085、少 U+FEFF。
const JS_SPACE_CLASS: &str = r"[\t\n\x{0B}\x{0C}\r \x{00A0}\x{1680}\x{2000}-\x{200A}\x{2028}\x{2029}\x{202F}\x{205F}\x{3000}\x{FEFF}]";

/// `NUMBER_PATTERN`：`[+-]?(?:\d+(?:\.\d*)?|\.\d+)(?:e[+-]?\d+)?`。
///
/// JS 的 `\d` 与非 unicode 正则的 `i` 标志都是 **ASCII-only**，而 `regex` 的 `\d` / `(?i)`
/// 是 Unicode 感知的（会接受 U+0660 之类的数字、把 KELVIN SIGN 折成 `k`）。这里因此显式写
/// `[0-9]` / `[eE]`，字母关键字的 `i` 也展开成 `[oO][kK]…` 形式，避免 Unicode 大小写折叠
/// 把 KELVIN SIGN 之类的非 ASCII 码点折成 ASCII 字母。
const NUMBER_PATTERN: &str = r"[+-]?(?:[0-9]+(?:\.[0-9]*)?|\.[0-9]+)(?:[eE][+-]?[0-9]+)?";

/// 编译 `OKLCH_PATTERN` / `OKHSL_PATTERN`（`i` 标志展开成逐字母大小写类，见 [`NUMBER_PATTERN`]）。
///
/// 捕获组顺序（[`parse_color`] 依赖它，改模式时必须同步改索引）：
/// - OKLCH：1 = L、2 = L 的 `%` 标记、3 = C、4 = H（H 后可有 `deg`；C 不接受 `%`）；
/// - OKHSL：1 = H（可有 `deg`）、2 = S、3 = S 的 `%`、4 = L、5 = L 的 `%`。
///
/// 函数名大小写不敏感，参数之间要求至少一个 JS 空白（`{SPACE}+`），括号紧邻处允许零个。
fn oklch_pattern() -> &'static regex::Regex {
    static PATTERN: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        let source = format!(
            r"^[oO][kK][lL][cC][hH]\({SPACE}*({NUMBER_PATTERN})(%)?{SPACE}+({NUMBER_PATTERN}){SPACE}+({NUMBER_PATTERN})(?:[dD][eE][gG])?{SPACE}*\)$",
            SPACE = JS_SPACE_CLASS
        );
        regex::Regex::new(&source).expect("OKLCH_PATTERN compiles")
    });
    &PATTERN
}

fn okhsl_pattern() -> &'static regex::Regex {
    static PATTERN: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        let source = format!(
            r"^[oO][kK][hH][sS][lL]\({SPACE}*({NUMBER_PATTERN})(?:[dD][eE][gG])?{SPACE}+({NUMBER_PATTERN})(%)?{SPACE}+({NUMBER_PATTERN})(%)?{SPACE}*\)$",
            SPACE = JS_SPACE_CLASS
        );
        regex::Regex::new(&source).expect("OKHSL_PATTERN compiles")
    });
    &PATTERN
}

/// `/^#([\da-f]{3}|[\da-f]{6})$/i`（`\d` 与 `i` 都是 ASCII-only，见 [`NUMBER_PATTERN`]）。
fn hex_pattern() -> &'static regex::Regex {
    static PATTERN: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(r"^#([0-9a-fA-F]{3}|[0-9a-fA-F]{6})$").expect("hex pattern compiles")
    });
    &PATTERN
}

/// `Number.parseFloat(text)`（正则已保证输入形态，只需处理前导符号/数字/指数）。
///
/// 解析失败返回 `NaN`；指数标记后没有数字时（如 `1e`）回退到指数之前的部分，
/// 与 `parseFloat` 的"读到不能读为止"一致。
fn parse_float(text: &str) -> f64 {
    let trimmed = text.trim_start_matches([' ', '\t', '\n', '\r', '\u{0B}', '\u{0C}']);
    let bytes = trimmed.as_bytes();
    let mut end = 0usize;
    if matches!(bytes.first(), Some(b'+' | b'-')) {
        end = 1;
    }
    let mut seen_digit = false;
    while end < bytes.len() && bytes[end].is_ascii_digit() {
        end += 1;
        seen_digit = true;
    }
    if end < bytes.len() && bytes[end] == b'.' {
        end += 1;
        while end < bytes.len() && bytes[end].is_ascii_digit() {
            end += 1;
            seen_digit = true;
        }
    }
    if !seen_digit {
        return f64::NAN;
    }
    if end < bytes.len() && (bytes[end] == b'e' || bytes[end] == b'E') {
        let mut exp_end = end + 1;
        if exp_end < bytes.len() && matches!(bytes[exp_end], b'+' | b'-') {
            exp_end += 1;
        }
        let digits_start = exp_end;
        while exp_end < bytes.len() && bytes[exp_end].is_ascii_digit() {
            exp_end += 1;
        }
        if exp_end > digits_start {
            end = exp_end;
        }
    }
    trimmed[..end].parse::<f64>().unwrap_or(f64::NAN)
}

/// `Number.parseInt(text, 16)`：读到非十六进制为止。
fn parse_hex(text: &str) -> u32 {
    let mut value: u32 = 0;
    for ch in text.chars() {
        let Some(digit) = ch.to_digit(16) else { break };
        value = value.wrapping_mul(16).wrapping_add(digit);
    }
    value
}

/// `parseColor(value)`（字符串形态）：接受 `#rgb` / `#rrggbb` / `oklch(...)` / `okhsl(...)`。
///
/// 无效值返回 `None`（TS 抛 `Invalid color value: ...`，见模块文档）。
///
/// - 十六进制按 3 位或 6 位解析：3 位时每字符重复一次（`#abc` → `#aabbcc`），
///   每两位按 `parseInt(..., 16)` 得到一个 0-255 的通道。
/// - `oklch(L C H)` 的 L 可带 `%`（除以 100）与 `deg` 后缀（H 可带）；L 必须在 0-1、
///   C 必须非负，否则返回 `None`。H 最终由 [`oklch_color`] 归一到 `[0, 360)`。
/// - `okhsl(H S L)` 的参数顺序与 [`okhsl_color`] 相同（H 在前），S/L 可带 `%`，
///   两者都必须在 0-1，否则返回 `None`。
/// - 正则的空白字符类按 JS `\s` 展开（见 `JS_SPACE_CLASS`），因此 NBSP、BOM 等能出现在
///   括号内，但**不能**出现在整串首尾（锚定 `^...$`）。
pub fn parse_color(value: &str) -> Option<Color> {
    if let Some(captures) = hex_pattern().captures(value) {
        let group = captures.get(1)?.as_str();
        // `hexPattern` 只接受 ASCII 十六进制，因此按下标切分是安全的。
        let digits: Vec<u8> = if group.len() == 3 {
            group.bytes().flat_map(|digit| [digit, digit]).collect()
        } else {
            group.bytes().collect()
        };
        let channel = |start: usize| -> f64 {
            let text = std::str::from_utf8(&digits[start..start + 2]).expect("ASCII hex digits");
            f64::from(parse_hex(text))
        };
        return Some(Color::Rgb {
            r: channel(0),
            g: channel(2),
            b: channel(4),
        });
    }

    if let Some(captures) = oklch_pattern().captures(value) {
        let lightness = parse_float(&captures[1])
            / if captures.get(2).is_some() {
                100.0
            } else {
                1.0
            };
        let chroma = parse_float(&captures[3]);
        let hue = parse_float(&captures[4]);
        // `oklchColor` 先 `requireFinite(l/c/h)`（`1e999` → Infinity 也在此被拒），再查范围。
        if !lightness.is_finite()
            || !chroma.is_finite()
            || !hue.is_finite()
            || !(0.0..=1.0).contains(&lightness)
            || chroma < 0.0
        {
            return None;
        }
        return Some(oklch_color(lightness, chroma, hue));
    }

    if let Some(captures) = okhsl_pattern().captures(value) {
        let saturation = parse_float(&captures[2])
            / if captures.get(3).is_some() {
                100.0
            } else {
                1.0
            };
        let lightness = parse_float(&captures[4])
            / if captures.get(5).is_some() {
                100.0
            } else {
                1.0
            };
        let hue = parse_float(&captures[1]);
        // `okhslColor` 的 requireFinite(h/s/l) + 范围检查。
        if !hue.is_finite()
            || !saturation.is_finite()
            || !lightness.is_finite()
            || !(0.0..=1.0).contains(&saturation)
            || !(0.0..=1.0).contains(&lightness)
        {
            return None;
        }
        let color = okhsl_color(hue, saturation, lightness);
        // `okhslColor` 末尾的 `rgbColor(r, g, b)` 还会要求三个通道有限（0-255 已由裁剪保证）。
        if let Color::Rgb { r, g, b } = color
            && !(r.is_finite() && g.is_finite() && b.is_finite())
        {
            return None;
        }
        return Some(color);
    }

    None
}

/// 256 色的 16 个基色。
///
/// 固定查表，不查询终端实际调色板：同一个索引在任何终端上都映射到这里的 sRGB 值。
const BASIC_COLORS: [RgbColor; 16] = [
    RgbColor { r: 0, g: 0, b: 0 },
    RgbColor { r: 128, g: 0, b: 0 },
    RgbColor { r: 0, g: 128, b: 0 },
    RgbColor {
        r: 128,
        g: 128,
        b: 0,
    },
    RgbColor { r: 0, g: 0, b: 128 },
    RgbColor {
        r: 128,
        g: 0,
        b: 128,
    },
    RgbColor {
        r: 0,
        g: 128,
        b: 128,
    },
    RgbColor {
        r: 192,
        g: 192,
        b: 192,
    },
    RgbColor {
        r: 128,
        g: 128,
        b: 128,
    },
    RgbColor { r: 255, g: 0, b: 0 },
    RgbColor { r: 0, g: 255, b: 0 },
    RgbColor {
        r: 255,
        g: 255,
        b: 0,
    },
    RgbColor { r: 0, g: 0, b: 255 },
    RgbColor {
        r: 255,
        g: 0,
        b: 255,
    },
    RgbColor {
        r: 0,
        g: 255,
        b: 255,
    },
    RgbColor {
        r: 255,
        g: 255,
        b: 255,
    },
];
/// 6×6×6 色立方每档的通道值（256 色索引 16-231 使用）。
const CUBE_VALUES: [f64; 6] = [0.0, 95.0, 135.0, 175.0, 215.0, 255.0];

/// `GRAY_VALUES = Array.from({length:24}, (_, i) => 8 + i * 10)`。
fn gray_values() -> &'static [f64; 24] {
    static VALUES: std::sync::LazyLock<[f64; 24]> = std::sync::LazyLock::new(|| {
        let mut values = [0.0f64; 24];
        for (index, value) in values.iter_mut().enumerate() {
            *value = 8.0 + index as f64 * 10.0;
        }
        values
    });
    &VALUES
}

/// `indexedToRgb(index)`。
///
/// 256 色索引的三段布局：
/// 1. `0..16`：`BASIC_COLORS` 固定基色；
/// 2. `16..232`：6×6×6 色立方，索引按 `16 + 36*r + 6*g + b` 编码，每通道取
///    `CUBE_VALUES` 的档位；
/// 3. `232..256`：24 级灰阶，灰度 `8 + (index - 232) * 10`（8, 18, …, 238）。
///
/// 三段互不重叠，`u8` 索引保证不会越界。
fn indexed_to_rgb(index: u8) -> RgbColor {
    if index < 16 {
        return BASIC_COLORS[usize::from(index)];
    }
    if index < 232 {
        let cube_index = usize::from(index) - 16;
        return RgbColor {
            r: CUBE_VALUES[cube_index / 36] as u8,
            g: CUBE_VALUES[(cube_index % 36) / 6] as u8,
            b: CUBE_VALUES[cube_index % 6] as u8,
        };
    }
    let gray = 8 + (u16::from(index) - 232) * 10;
    let gray = gray as u8;
    RgbColor {
        r: gray,
        g: gray,
        b: gray,
    }
}

/// `isInSrgbGamut(linear)`。
///
/// 允许 `1e-7` 的误差，避免浮点舍入把恰好落在 0/1 边界上的颜色判为越界。
fn is_in_srgb_gamut(linear: [f64; 3]) -> bool {
    const EPSILON: f64 = 1e-7;
    linear
        .iter()
        .all(|&channel| (-EPSILON..=1.0 + EPSILON).contains(&channel))
}

/// `oklchToRgb({l, c, h})`：色域映射（保持色相，色度二分 20 次）。
fn oklch_to_rgb(l: f64, c: f64, h: f64) -> RgbColor {
    // 先按目标色度取一次：落在 sRGB 色域内就直接使用（绝大多数颜色走这条路径）。
    let radians = (h * std::f64::consts::PI) / 180.0;
    let cos = radians.cos();
    let sin = radians.sin();
    let at_chroma = |chroma: f64| oklab_to_linear_srgb([l, chroma * cos, chroma * sin]);

    let direct = at_chroma(c);
    if is_in_srgb_gamut(direct) {
        return linear_to_rgb_color(direct);
    }

    // 逐步降色度直到落回色域；无彩色一定在色域内，所以它是兜底
    // （`oklch(100% 0.3 150)` 必须映射到白）。
    //
    // 二分假设"色度越低越可能在色域内"：每轮把中点落回色域的一侧记为 low 并保留该候选，
    // 否则收缩 high。固定 20 轮、不做收敛判断；结束后用最后一个合法候选。
    let mut linear = at_chroma(0.0);
    let mut low = 0.0f64;
    let mut high = c;
    for _ in 0..20 {
        let chroma = (low + high) / 2.0;
        let candidate = at_chroma(chroma);
        if is_in_srgb_gamut(candidate) {
            low = chroma;
            linear = candidate;
        } else {
            high = chroma;
        }
    }
    linear_to_rgb_color(linear)
}

/// `linearSrgbToRgb(...)` 落成 `RgbColor`（通道已是 0-255 的整数）。
fn linear_to_rgb_color(linear: [f64; 3]) -> RgbColor {
    let [r, g, b] = linear_srgb_to_rgb(linear);
    RgbColor {
        r: r as u8,
        g: g as u8,
        b: b as u8,
    }
}

/// `colorToRgb`。
///
/// TS 的 `RgbColor` 通道是 **number**：`rgb` 颜色原样返回（可能是 `127.5` 这样的分数），
/// `colorToHex`/`colorAnsi` 再各自 `Math.round`。冻结的 [`RgbColor`] 通道是 `u8`，因此这里
/// 按 `Math.round` 取整——与所有既有调用方（`colorToHex`、`colorAnsi`、`rgbToAnsi256`）
/// 里 TS 的取整结果一致。需要原始分数通道的内部路径见 `color_to_rgb_float`。
pub fn color_to_rgb(color: Color) -> RgbColor {
    let [r, g, b] = color_to_rgb_float(color);
    RgbColor {
        r: js_round(r) as u8,
        g: js_round(g) as u8,
        b: js_round(b) as u8,
    }
}

/// `colorToRgb` 的原始数值形态：`indexed`/`oklch` 分支已是整数，`rgb` 分支保留分数。
///
/// `mixColors` 的 `srgb` 空间要在这上面继续做线性插值（TS 不做中间取整）。
pub(crate) fn color_to_rgb_float(color: Color) -> [f64; 3] {
    match color {
        Color::Indexed { index } => {
            let rgb = indexed_to_rgb(index);
            [f64::from(rgb.r), f64::from(rgb.g), f64::from(rgb.b)]
        }
        Color::Rgb { r, g, b } => [r, g, b],
        Color::Oklch { l, c, h } => {
            let rgb = oklch_to_rgb(l, c, h);
            [f64::from(rgb.r), f64::from(rgb.g), f64::from(rgb.b)]
        }
    }
}

/// `colorToOklch`。
///
/// `Oklch` 输入原样返回（不做色域映射）；其余形态先经 `color_to_rgb_float` 转到 sRGB，
/// 再由 `rgb_channels_to_oklab` 转 OKLab。色相用 `atan2(b, a)` 求出并归一到 `[0, 360)`；
/// 完全无彩色（`a == b == 0`）时 `atan2(0, 0)` 为 0。
pub fn color_to_oklch(color: Color) -> OklchChannels {
    if let Color::Oklch { l, c, h } = color {
        return OklchChannels { l, c, h };
    }
    let [l, a, b] = rgb_channels_to_oklab(color_to_rgb_float(color));
    OklchChannels {
        l,
        // `Math.hypot(a, b)`（除以最大值的算法；`(a*a+b*b).sqrt()` 约 24% 的输入差 1 ulp）。
        c: js_hypot(a, b),
        h: ((libm::atan2(b, a) * 180.0) / std::f64::consts::PI + 360.0) % 360.0,
    }
}

/// `colorToOkhsl`。
///
/// 先把颜色落到 sRGB（`indexed` 查表、`oklch` 做色域映射），再解出 OKHSL 通道。
/// 因此 `color_to_okhsl(oklch_color(..))` 得到的是**映射到 sRGB 之后**的饱和度，
/// 不是原始的 `c`；往返会损失色域外信息。
pub fn color_to_okhsl(color: Color) -> OkhslChannels {
    rgb_channels_to_okhsl(color_to_rgb_float(color))
}

/// `colorToHex`：小写十六进制，`Math.round` 后 `toString(16)` 再 `padStart(2, "0")`（不截断）。
pub fn color_to_hex(color: Color) -> String {
    let rgb = color_to_rgb_float(color);
    let channel = |value: f64| {
        let rounded = js_round(value);
        let text = if rounded.is_finite() {
            format!("{:x}", rounded as i64)
        } else {
            "NaN".to_string()
        };
        format!("{text:0>2}")
    };
    format!("#{}{}{}", channel(rgb[0]), channel(rgb[1]), channel(rgb[2]))
}

/// `mixColors(first, second, amount, space)`。
///
/// - [`ColorMixSpace::Srgb`]：在 sRGB 通道上线性插值，返回**带分数通道**的 [`Color::Rgb`]
///   （不在中途取整）。
/// - [`ColorMixSpace::Oklch`]：对 `l`/`c` 线性插值，色相走最短路径（差角归一到
///   `[-180, 180)`）；任一端无彩色（`c < 1e-7`）时借用另一端的色相，避免灰色混出随机色相。
///   结果统一是 [`Color::Oklch`]（色相已归一化）。
///
/// `amount` 不做裁剪：0 与 1 分别落在两端，区间外是外插值。
pub fn mix_colors(first: Color, second: Color, amount: f64, space: ColorMixSpace) -> Color {
    if space == ColorMixSpace::Srgb {
        let a = color_to_rgb_float(first);
        let b = color_to_rgb_float(second);
        return Color::Rgb {
            r: a[0] + (b[0] - a[0]) * amount,
            g: a[1] + (b[1] - a[1]) * amount,
            b: a[2] + (b[2] - a[2]) * amount,
        };
    }

    let a = color_to_oklch(first);
    let b = color_to_oklch(second);
    // 无彩色（c < 1e-7）时借用对方色相，避免灰色混色产生随机色相
    let first_hue = if a.c < 1e-7 { b.h } else { a.h };
    let second_hue = if b.c < 1e-7 { first_hue } else { b.h };
    // 最短路径色相差 ∈ [-180, 180)
    let hue_delta = ((second_hue - first_hue + 540.0) % 360.0) - 180.0;
    oklch_color(
        a.l + (b.l - a.l) * amount,
        a.c + (b.c - a.c) * amount,
        first_hue + hue_delta * amount,
    )
}

/// `findClosest(values, target)`：最近的索引；平局取**先出现**的。
fn find_closest(values: &[f64], target: f64) -> usize {
    let mut closest_index = 0usize;
    let mut closest_distance = f64::INFINITY;
    for (index, &value) in values.iter().enumerate() {
        let distance = (target - value).abs();
        if distance < closest_distance {
            closest_index = index;
            closest_distance = distance;
        }
    }
    closest_index
}

/// `colorDistance(first, second)`；通道可以是分数（TS 的 `RgbColor` 是 number）。
fn color_distance(first: [f64; 3], second: [f64; 3]) -> f64 {
    let dr = first[0] - second[0];
    let dg = first[1] - second[1];
    let db = first[2] - second[2];
    dr * dr * 0.299 + dg * dg * 0.587 + db * db * 0.114
}

/// `rgbToAnsi256(color)`；`color` 是 0-255 的（可能带小数的）通道三元组。
fn rgb_to_ansi256(color: [f64; 3]) -> u16 {
    let (r, g, b) = (color[0], color[1], color[2]);
    // 1. 每个通道在 6 档色立方里找最近档位，得到候选的立方色。
    let r_index = find_closest(&CUBE_VALUES, r);
    let g_index = find_closest(&CUBE_VALUES, g);
    let b_index = find_closest(&CUBE_VALUES, b);
    let cube_color = [
        CUBE_VALUES[r_index],
        CUBE_VALUES[g_index],
        CUBE_VALUES[b_index],
    ];
    let cube_index = 16 + 36 * r_index + 6 * g_index + b_index;

    // 2. 同时算一个灰阶候选：亮度按 0.299/0.587/0.114 加权后取整，再找最近的灰阶档。
    let gray = js_round(0.299 * r + 0.587 * g + 0.114 * b);
    let gray_values = gray_values();
    let gray_offset = find_closest(gray_values, gray);
    let gray_value = gray_values[gray_offset];
    let gray_color = [gray_value, gray_value, gray_value];

    // 3. 只有颜色近似中性（通道极差 < 10）且灰阶比立方色更近时才用灰阶，
    //    否则用色立方。距离是加权平方距离，不开方（单调，比较大小即可）。
    let spread = js_max(js_max(r, g), b) - js_min(js_min(r, g), b);
    if spread < 10.0 && color_distance(color, gray_color) < color_distance(color, cube_color) {
        return 232 + u16::try_from(gray_offset).expect("gray offset < 24");
    }
    u16::try_from(cube_index).expect("cube index < 256")
}

/// `colorAnsi(color, mode, background)`。
///
/// `Indexed` 直接输出 256 色序列（不受 `mode` 影响）；其余颜色在 `Truecolor` 下输出
/// `38/48;2;r;g;b`（通道 `Math.round` 取整），在 `Palette256` 下先由 `rgb_to_ansi256`
/// 量化再输出 `38/48;5;n`。序列都以 `m` 结尾，调用方负责复位。
fn color_ansi(color: Color, mode: TerminalColorMode, background: bool) -> String {
    let prefix = if background { 48 } else { 38 };
    if let Color::Indexed { index } = color {
        return format!("\x1b[{prefix};5;{index}m");
    }

    let rgb = color_to_rgb_float(color);
    if mode == TerminalColorMode::Truecolor {
        return format!(
            "\x1b[{prefix};2;{};{};{}m",
            js_round(rgb[0]),
            js_round(rgb[1]),
            js_round(rgb[2])
        );
    }
    format!("\x1b[{prefix};5;{}m", rgb_to_ansi256(rgb))
}

/// `foregroundAnsi(color, mode)`。
///
/// `Indexed` 颜色不看 `mode`，总是 `38;5;n`；RGB/OKLCH 在 `Truecolor` 下是
/// `38;2;r;g;b`（取整），在 `Palette256` 下先量化到 256 色。
pub fn foreground_ansi(color: Color, mode: TerminalColorMode) -> String {
    color_ansi(color, mode, false)
}

/// `backgroundAnsi(color, mode)`。
///
/// 与 [`foreground_ansi`] 相同，只是前缀换成背景色 `48`。
pub fn background_ansi(color: Color, mode: TerminalColorMode) -> String {
    color_ansi(color, mode, true)
}

/// `styleText(text, options, mode)`。
pub fn style_text(text: &str, options: &TextStyle, mode: TerminalColorMode) -> String {
    style_text_with_ansi(text, options, mode)
}

/// `styleTextWithAnsi(text, fgAnsi, bgAnsi, options)`：**TS 的四参数形态**。
///
/// `fg_ansi`/`bg_ansi` 是**已格式化**的 ANSI 片段（例如缓存的主题色）；`attrs` 只含六个布尔属性。
/// 输出 = prefix(fg, bg, bold, dim, italic, underline, inverse, strikethrough) + text + suffix(逆序)。
///
/// 属性判定只认 `Some(true)`：`None` 与 `Some(false)` 都不产生序列。bold 与 dim 共用一个
/// 复位序列 `22`，两者都开启时 suffix 里只出现一次；`truecolor`/`indexed` 的片段由调用方
/// 负责生成，本函数不解析也不重排它们。
pub fn style_text_with_parts(
    text: &str,
    fg_ansi: Option<&str>,
    bg_ansi: Option<&str>,
    attrs: &TextAttributes,
) -> String {
    // reset 用前置插入，因此关闭顺序与打开顺序相反
    let mut prefix = String::new();
    let mut suffix = String::new();
    if let Some(fg) = fg_ansi {
        prefix.push_str(fg);
        suffix = "\x1b[39m".to_string();
    }
    if let Some(bg) = bg_ansi {
        prefix.push_str(bg);
        suffix = format!("\x1b[49m{suffix}");
    }
    if attrs.bold == Some(true) {
        prefix.push_str("\x1b[1m");
    }
    if attrs.dim == Some(true) {
        prefix.push_str("\x1b[2m");
    }
    if attrs.bold == Some(true) || attrs.dim == Some(true) {
        suffix = format!("\x1b[22m{suffix}");
    }
    if attrs.italic == Some(true) {
        prefix.push_str("\x1b[3m");
        suffix = format!("\x1b[23m{suffix}");
    }
    if attrs.underline == Some(true) {
        prefix.push_str("\x1b[4m");
        suffix = format!("\x1b[24m{suffix}");
    }
    if attrs.inverse == Some(true) {
        prefix.push_str("\x1b[7m");
        suffix = format!("\x1b[27m{suffix}");
    }
    if attrs.strikethrough == Some(true) {
        prefix.push_str("\x1b[9m");
        suffix = format!("\x1b[29m{suffix}");
    }
    format!("{prefix}{text}{suffix}")
}

/// `styleTextWithAnsi`（冻结签名的三参数形态）：从 `options.fg`/`options.bg` 生成颜色片段
/// 后走 [`style_text_with_parts`]。
pub fn style_text_with_ansi(text: &str, options: &TextStyle, mode: TerminalColorMode) -> String {
    let fg = options.fg.map(|color| foreground_ansi(color, mode));
    let bg = options.bg.map(|color| background_ansi(color, mode));
    style_text_with_parts(text, fg.as_deref(), bg.as_deref(), &options.attributes)
}

/// 数字格式化（错误消息与调试输出用）。
///
/// `kiana-theme` 的主题校验错误文案用它拼数字（见 `crates/kiana-theme/src/theme.rs`）。
/// 委托给 `kiana_json::format_js_number`：有限值按 ECMAScript 的数字格式输出，但
/// `NaN`/`±Infinity` 写成 `null`（`JSON.stringify` 语义），不是 `Number#toString` 的
/// `NaN`/`Infinity`；`-0` 也写成 `0`。
pub fn format_number(value: f64) -> String {
    format_js_number(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constructors_tag_their_kind() {
        assert_eq!(indexed_color(3), Color::Indexed { index: 3 });
        assert_eq!(
            rgb_color(1.0, 2.0, 3.0),
            Color::Rgb {
                r: 1.0,
                g: 2.0,
                b: 3.0
            }
        );
    }
}
