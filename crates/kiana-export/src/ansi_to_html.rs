//! core/export-html/ansi-to-html.ts。
//!
//! 把终端 ANSI SGR 颜色/样式转成带内联样式的 HTML。支持：
//!
//! - 标准前景色（30-37）与亮色变体（90-97）；
//! - 标准背景色（40-47）与亮色变体（100-107）；
//! - 256 色调色板（`38;5;N` / `48;5;N`）；
//! - RGB 真彩色（`38;2;R;G;B` / `48;2;R;G;B`）；
//! - 文本样式：bold（1）、dim（2）、italic（3）、underline（4）、reset（0）。
//!
//! 只识别以 `m` 结尾的 SGR 序列（`ESC [` + `[0-9;]*` + `m`）；其它 CSI/OSC 序列与
//! 普通文本一样按字面量转义。扫描按字节进行：ESC、`[`、数字与 `;` 都是 ASCII，
//! 因此切片边界总是合法的 UTF-8 边界。
//!
//! # 为什么需要这个模块
//!
//! HTML 导出里的工具区来自 TUI 组件的 ANSI 输出（见 [`crate::tool_renderer`]）：组件
//! 只会产出带转义序列的文本行，浏览器不识别 ANSI。本模块把这个中间表示转成带内联
//! `style` 的 HTML，最终由 `index` 放进 `renderedTools` 载荷。
//!
//! # 一次转换的执行顺序
//!
//! 1. 按字节扫描，只有 `ESC [` 开始、由数字/`;` 组成、以 `m` 结尾的序列才被识别；
//! 2. 命中时先把上一段普通文本做 HTML 转义写出（`&`、`<`、`>`、`"`、`'`）；
//! 3. 关闭当前 `<span>`，把参数应用到 `TextStyle`，若仍有样式则开一个新 `<span>`；
//! 4. 扫描结束后写出剩余文本，并闭合仍未关闭的 span。
//!
//! 每个 SGR 序列都会“关旧 span、开新 span”，即使样式没有变化，因此
//! `ESC[31mESC[32mX` 会留下一个空的红色 span（`tests/ansi_to_html.rs` 明确断言）。
//!
//! # 与 TS 的字节级对齐
//!
//! 输出要与 `ansi-to-html.ts` 逐字节相同，所以数字格式化必须复刻 JS：
//! `parseInt` 只取前导十进制数字、`Number::toString`（本模块的 `js_number_to_string`）
//! 以及 `toString(16).padStart(2, "0")`（只补零、从不截断）。内联 CSS 的属性顺序
//! （color、background-color、font-weight、opacity、font-style、text-decoration）
//! 也是输出的一部分，不能按 CSS 习惯重排。
//!
//! # 边界
//!
//! 纯字符串变换，不返回 `Result` 也不会 panic：无法识别的 CSI/OSC、孤立的 `ESC`
//! 都作为普通文本保留（仅做 HTML 特殊字符转义）。写入 `style` 属性的值要么来自固定
//! 调色板，要么由数字格式化得到（不含引号、尖括号等 HTML 元字符），因此不需要再转义。

/// 标准 ANSI 调色板（0-15）。
///
/// 0-7 是常规色、8-15 是亮色；30-37 与 40-47 映射到 0-7，90-97 与 100-107 映射到
/// 8-15。十六进制字母大小写会进入导出产物，不要改成大写。
const ANSI_COLORS: [&str; 16] = [
    "#000000", // 0: black
    "#800000", // 1: red
    "#008000", // 2: green
    "#808000", // 3: yellow
    "#000080", // 4: blue
    "#800080", // 5: magenta
    "#008080", // 6: cyan
    "#c0c0c0", // 7: white
    "#808080", // 8: bright black
    "#ff0000", // 9: bright red
    "#00ff00", // 10: bright green
    "#ffff00", // 11: bright yellow
    "#0000ff", // 12: bright blue
    "#ff00ff", // 13: bright magenta
    "#00ffff", // 14: bright cyan
    "#ffffff", // 15: bright white
];

/// 当前文本样式。
#[derive(Debug, Clone, Default)]
struct TextStyle {
    fg: Option<String>,
    bg: Option<String>,
    bold: bool,
    dim: bool,
    italic: bool,
    underline: bool,
}

impl TextStyle {
    fn has_style(&self) -> bool {
        self.fg.is_some()
            || self.bg.is_some()
            || self.bold
            || self.dim
            || self.italic
            || self.underline
    }

    /// TS `styleToInlineCSS`：固定顺序、`;` 连接（无空格）。
    ///
    /// 属性顺序与分隔符都属于导出产物的一部分（测试逐字节比对），不要重排；
    /// `;` 之后也没有空格。
    fn to_inline_css(&self) -> String {
        let mut parts: Vec<String> = Vec::with_capacity(6);
        if let Some(fg) = &self.fg {
            parts.push(format!("color:{fg}"));
        }
        if let Some(bg) = &self.bg {
            parts.push(format!("background-color:{bg}"));
        }
        if self.bold {
            parts.push("font-weight:bold".to_string());
        }
        if self.dim {
            parts.push("opacity:0.6".to_string());
        }
        if self.italic {
            parts.push("font-style:italic".to_string());
        }
        if self.underline {
            parts.push("text-decoration:underline".to_string());
        }
        parts.join(";")
    }

    fn reset(&mut self) {
        self.fg = None;
        self.bg = None;
        self.bold = false;
        self.dim = false;
        self.italic = false;
        self.underline = false;
    }
}

/// 把非负整数值格式化成 JS `String(value)` 的十进制文本。
///
/// 只服务于 ANSI 参数（`parseInt` 的结果）与主题颜色通道（`Math.round`/`Math.min`
/// 的结果）：负零、`NaN`、`Infinity` 与 `>= 1e21` 的指数写法按 ECMAScript
/// `Number::toString` 处理。
///
/// 两类调用点的输入都是**非负数值**：ANSI 参数只由十进制数字组成（可能大到
/// `>= 1e21`），主题通道是 `Math.round`/`min`/`max` 计算出的整数。因此不必覆盖 JS
/// 对 `1e-7` 这类小数使用的指数写法。使用 `Display` 而非 `Debug` 也是为了对齐：
/// 整数值输出 `5` 而不是 `5.0`，与 JS `String(5)` 相同。
pub(crate) fn js_number_to_string(value: f64) -> String {
    if value == 0.0 {
        return "0".to_string();
    }
    if value.is_nan() {
        return "NaN".to_string();
    }
    if value.is_infinite() {
        return if value > 0.0 {
            "Infinity".to_string()
        } else {
            "-Infinity".to_string()
        };
    }
    if value >= 1e21 {
        // Rust 的 `{:e}` 输出形如 `1.5e21`；JS 要求 `1.5e+21`。
        let formatted = format!("{value:e}");
        let (mantissa, exponent) = match formatted.split_once('e') {
            Some(parts) => parts,
            None => return formatted,
        };
        return format!("{mantissa}e+{exponent}");
    }
    format!("{value}")
}

/// 把非负整数 `f64` 格式化成小写十六进制（JS `Number.prototype.toString(16)`）。
///
/// `padStart(2, "0")` 只补零、从不截断，因此 256 色索引 999 的灰度 7678 输出 `1dfe`。
///
/// 只接受**整数值**：JS 的 `toString(16)` 对小数会产出小数位，而这里的 `{:x}` 与
/// 位运算只处理整数部分；当前调用点（256 色索引与灰度的整数运算）保证输入是整数。
/// `>= 2^53` 的值统一走位布局分支：按 IEEE-754 的尾数与指数展开，避免依赖 `as u64`
/// 对超出 `u64` 范围的值发生的饱和。
fn integral_to_hex(value: f64) -> String {
    if value.is_nan() {
        return "NaN".to_string();
    }
    if value.is_infinite() {
        return if value > 0.0 {
            "Infinity".to_string()
        } else {
            "-Infinity".to_string()
        };
    }
    if value == 0.0 {
        return "0".to_string();
    }
    // 实际 256 色路径上的值都远小于 2^53；更大的整数用 f64 的精确位布局展开。
    if value < 9_007_199_254_740_992.0 {
        return format!("{:x}", value as u64);
    }
    let bits = value.to_bits();
    let exponent = ((bits >> 52) & 0x7ff) as i32 - 1023;
    let mantissa = (bits & ((1_u64 << 52) - 1)) | (1_u64 << 52);
    let shift = exponent - 52;
    if shift < 0 {
        // 值仍然是整数且 < 2^53 的情形已被上面的分支覆盖。
        return format!("{:x}", value as u64);
    }
    let remainder = (shift % 4) as u32;
    let high = mantissa << remainder;
    let mut hex = format!("{high:x}");
    for _ in 0..(shift / 4) {
        hex.push('0');
    }
    hex
}

/// 左侧补零到至少 `width` 个字符（JS `padStart`，不截断）。
fn pad_start_zero(text: &str, width: usize) -> String {
    if text.len() >= width {
        return text.to_string();
    }
    let mut padded = String::with_capacity(width);
    for _ in 0..(width - text.len()) {
        padded.push('0');
    }
    padded.push_str(text);
    padded
}

/// TS `color256ToHex`：256 色索引 → 十六进制颜色。
///
/// 三段映射：0-15 固定调色板；16-231 是 6×6×6 颜色立方（每通道 `0` 或
/// `55 + 40*n`）；232-255 是 24 档灰度 `8 + 10*(index-232)`。`>= 256` 的索引
/// （例如 `38;5;999`）仍走灰度公式，`padStart` 不截断，会输出 4 位十六进制。
///
/// `index` 来自 `parse_sgr_params`，只可能是非负值（十进制数字串的解析结果）；
/// `get(...).unwrap_or("#000000")` 是无法到达的防御性分支。
fn color256_to_hex(index: f64) -> String {
    // 标准颜色（0-15）。
    if index < 16.0 {
        let slot = index as usize;
        return ANSI_COLORS.get(slot).unwrap_or(&"#000000").to_string();
    }
    // 颜色立方（16-231）：6x6x6 = 216 色。
    if index < 232.0 {
        let cube_index = index - 16.0;
        let r = (cube_index / 36.0).floor();
        let g = ((cube_index % 36.0) / 6.0).floor();
        let b = cube_index % 6.0;
        let to_component = |n: f64| if n == 0.0 { 0.0 } else { 55.0 + n * 40.0 };
        let to_hex = |n: f64| pad_start_zero(&integral_to_hex(to_component(n)), 2);
        return format!("#{}{}{}", to_hex(r), to_hex(g), to_hex(b));
    }
    // 灰度（232-255）：24 档。
    let gray = 8.0 + (index - 232.0) * 10.0;
    let gray_hex = pad_start_zero(&integral_to_hex(gray), 2);
    format!("#{gray_hex}{gray_hex}{gray_hex}")
}

/// TS `escapeHtml`：按 `&`、`<`、`>`、`"`、`'` 的顺序替换；单引号是 `&#039;`。
///
/// 这里用逐字符单遍替换而不是链式 `replace`：既避免把刚生成的 `&amp;` 再转义一次，
/// 也与 TS 五步链式替换的最终结果相同。
fn escape_html_into(text: &str, output: &mut String) {
    for character in text.chars() {
        match character {
            '&' => output.push_str("&amp;"),
            '<' => output.push_str("&lt;"),
            '>' => output.push_str("&gt;"),
            '"' => output.push_str("&quot;"),
            '\'' => output.push_str("&#039;"),
            _ => output.push(character),
        }
    }
}

/// TS `escapeHtml`：返回转义后的文本（单元测试用；生产路径直接写入缓冲区）。
#[cfg(test)]
fn escape_html(text: &str) -> String {
    let mut output = String::with_capacity(text.len());
    escape_html_into(text, &mut output);
    output
}

/// 解析 SGR 参数串（`parseInt(p, 10) || 0`）：空参数串 → `[0]`。
///
/// 对应 TS `paramStr ? paramStr.split(";").map(p => parseInt(p, 10) || 0) : [0]`：
/// 空参数串（裸 `ESC[m`）等价于 reset；每个分段只取前导十进制数字（`"12x"` → 12），
/// 没有数字的分段（`""`、`"abc"`）按 0 处理，所以 `1;;3` 中间的 `0` 也是一次 reset。
fn parse_sgr_params(param_str: &str) -> Vec<f64> {
    if param_str.is_empty() {
        return vec![0.0];
    }
    param_str
        .split(';')
        .map(|part| {
            // `parseInt(part, 10)`：取最长的前导十进制数字前缀；没有数字或为 0 都按 0。
            let digits: String = part.chars().take_while(char::is_ascii_digit).collect();
            if digits.is_empty() {
                return 0.0;
            }
            digits.parse::<f64>().unwrap_or(0.0)
        })
        .collect()
}

/// TS `applySgrCode`：把一组 SGR 参数应用到样式上。
///
/// 按索引顺序消费参数：`38`/`48` 的扩展色会额外吃掉 2 个（`;5;N`）或 4 个
/// （`;2;R;G;B`）参数（循环末尾还会再 `i += 1`）。若后续参数不足或选择子不是 5/2，
/// 则**不改变样式**且只前进 1，剩余参数继续按独立 code 解释。
///
/// 颜色映射：前景 30-37 → 调色板 0-7、90-97 → 8-15；背景 40-47 → 0-7、
/// 100-107 → 8-15。`0` 重置全部；`1`/`2`/`3`/`4` 分别置 bold/dim/italic/underline；
/// `22` 同时清除 bold 与 dim；`23`/`24` 分别清除 italic/underline；`39`/`49` 分别只清除
/// 前景/背景；其余 code 忽略。
///
/// 参数用 `f64` 是为了保留 TS 的 number 语义；进入调色板分支前都先做区间检查，
/// `as usize` 的转换因此不会溢出。
fn apply_sgr_code(params: &[f64], style: &mut TextStyle) {
    let mut i = 0;
    while i < params.len() {
        let code = params[i];

        if code == 0.0 {
            style.reset();
        } else if code == 1.0 {
            style.bold = true;
        } else if code == 2.0 {
            style.dim = true;
        } else if code == 3.0 {
            style.italic = true;
        } else if code == 4.0 {
            style.underline = true;
        } else if code == 22.0 {
            style.bold = false;
            style.dim = false;
        } else if code == 23.0 {
            style.italic = false;
        } else if code == 24.0 {
            style.underline = false;
        } else if (30.0..=37.0).contains(&code) {
            style.fg = Some(ANSI_COLORS[(code - 30.0) as usize].to_string());
        } else if code == 38.0 {
            // 扩展前景色：38;5;N 或 38;2;R;G;B；参数不足时不改变样式且只前进 1。
            if params.get(i + 1) == Some(&5.0) && params.len() > i + 2 {
                style.fg = Some(color256_to_hex(params[i + 2]));
                i += 2;
            } else if params.get(i + 1) == Some(&2.0) && params.len() > i + 4 {
                let r = js_number_to_string(params[i + 2]);
                let g = js_number_to_string(params[i + 3]);
                let b = js_number_to_string(params[i + 4]);
                style.fg = Some(format!("rgb({r},{g},{b})"));
                i += 4;
            }
        } else if code == 39.0 {
            style.fg = None;
        } else if (40.0..=47.0).contains(&code) {
            style.bg = Some(ANSI_COLORS[(code - 40.0) as usize].to_string());
        } else if code == 48.0 {
            // 扩展背景色，规则与 38 相同。
            if params.get(i + 1) == Some(&5.0) && params.len() > i + 2 {
                style.bg = Some(color256_to_hex(params[i + 2]));
                i += 2;
            } else if params.get(i + 1) == Some(&2.0) && params.len() > i + 4 {
                let r = js_number_to_string(params[i + 2]);
                let g = js_number_to_string(params[i + 3]);
                let b = js_number_to_string(params[i + 4]);
                style.bg = Some(format!("rgb({r},{g},{b})"));
                i += 4;
            }
        } else if code == 49.0 {
            style.bg = None;
        } else if (90.0..=97.0).contains(&code) {
            style.fg = Some(ANSI_COLORS[(code - 90.0 + 8.0) as usize].to_string());
        } else if (100.0..=107.0).contains(&code) {
            style.bg = Some(ANSI_COLORS[(code - 100.0 + 8.0) as usize].to_string());
        }
        // 其余 code 忽略。

        i += 1;
    }
}

/// 在 `text[start..]`（`start` 是 ESC 的下标）尝试匹配一个 SGR 序列。
///
/// 返回 `(参数串结束下标, 序列结束下标)`：`param_end` 指向 `m`，`end` 是 `m` 之后。
/// `tool_renderer` 的空白行判定复用该扫描器。
///
/// 只接受 ASCII 数字与 `;`，所以 `ESC[1;2H` 这类 CSI 不匹配；参数串本身不解析值，
/// 调用方用 `start + 2 .. param_end` 切片即可，边界都是 ASCII。
pub(crate) fn match_sgr_at(bytes: &[u8], start: usize) -> Option<(usize, usize)> {
    if bytes.get(start) != Some(&0x1b) || bytes.get(start + 1) != Some(&b'[') {
        return None;
    }
    let mut cursor = start + 2;
    while let Some(&byte) = bytes.get(cursor) {
        if byte.is_ascii_digit() || byte == b';' {
            cursor += 1;
        } else {
            break;
        }
    }
    if bytes.get(cursor) == Some(&b'm') {
        Some((cursor, cursor + 1))
    } else {
        None
    }
}

/// 把 ANSI 转义文本转成带内联样式的 HTML。
///
/// 每个 SGR 序列都会关闭并重新打开 span（即使样式未变），因此相邻同色文本会生成
/// 相邻的空 span 片段。
///
/// 算法是单遍线性扫描：普通字节逐个前进；命中 SGR 时跳过整段序列。样式状态
/// `TextStyle` 在整段文本内延续（同一行里未 reset 的颜色会影响后续文本），但每次
/// 调用都从默认样式重新开始，所以 [`ansi_lines_to_html`] 的各行互不影响。
///
/// 输入非法或序列被截断都不会报错：无法匹配的部分作为普通文本转义后输出。
pub fn ansi_to_html(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut style = TextStyle::default();
    let mut result = String::with_capacity(text.len());
    let mut last_index = 0_usize;
    let mut in_span = false;
    let mut cursor = 0_usize;

    while cursor < bytes.len() {
        if let Some((param_end, sequence_end)) = match_sgr_at(bytes, cursor) {
            let before = &text[last_index..cursor];
            if !before.is_empty() {
                escape_html_into(before, &mut result);
            }

            let params = parse_sgr_params(&text[cursor + 2..param_end]);
            if in_span {
                result.push_str("</span>");
                in_span = false;
            }
            apply_sgr_code(&params, &mut style);
            if style.has_style() {
                result.push_str("<span style=\"");
                result.push_str(&style.to_inline_css());
                result.push_str("\">");
                in_span = true;
            }

            last_index = sequence_end;
            cursor = sequence_end;
            continue;
        }
        cursor += 1;
    }

    let remaining = &text[last_index..];
    if !remaining.is_empty() {
        escape_html_into(remaining, &mut result);
    }
    if in_span {
        result.push_str("</span>");
    }
    result
}

/// 把 ANSI 行数组转成 HTML：每行包一个 `<div class="ansi-line">`。
///
/// 行与行之间**没有任何空白字符**（TS 用 `join("")`，测试逐字节比对）；转换结果为
/// 空的行走 `&nbsp;` 占位。每行独立转换，上一行的样式不会延续到下一行。
///
/// 生产调用方是 [`crate::tool_renderer`] 的 `render_call` / `render_result`：组件先按
/// 固定宽度渲染成 ANSI 行，再交给本函数拼成 HTML。
pub fn ansi_lines_to_html(lines: &[String]) -> String {
    let mut result = String::new();
    for line in lines {
        let converted = ansi_to_html(line);
        result.push_str("<div class=\"ansi-line\">");
        if converted.is_empty() {
            result.push_str("&nbsp;");
        } else {
            result.push_str(&converted);
        }
        result.push_str("</div>");
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escape_html_uses_ts_entities() {
        assert_eq!(escape_html("&<>\"'"), "&amp;&lt;&gt;&quot;&#039;");
    }

    #[test]
    fn color256_matches_verified_vectors() {
        assert_eq!(color256_to_hex(0.0), "#000000");
        assert_eq!(color256_to_hex(196.0), "#ff0000");
        assert_eq!(color256_to_hex(255.0), "#eeeeee");
        assert_eq!(color256_to_hex(999.0), "#1dfe1dfe1dfe");
    }

    #[test]
    fn parses_js_parse_int_semantics() {
        assert_eq!(parse_sgr_params(""), vec![0.0]);
        assert_eq!(parse_sgr_params("1;;3"), vec![1.0, 0.0, 3.0]);
        assert_eq!(parse_sgr_params("12x"), vec![12.0]);
        assert_eq!(parse_sgr_params("abc"), vec![0.0]);
    }
}
