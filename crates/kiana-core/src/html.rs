//! utils/html.ts。
//!
//! TS: `packages/coding-agent/src/utils/html.ts`（51 行）。
//!
//! ```ts
//! export interface DecodedHtmlEntity { text: string; length: number }
//! export function decodeHtmlEntity(entity: string): string | undefined
//! export function decodeHtmlEntityAt(html: string, index: number): DecodedHtmlEntity | undefined
//! ```
//!
//! ## UTF-16（`ARCHITECTURE.md` §4.6，`spec/ca-cli.md` §8.2 第 8 条）
//!
//! `decodeHtmlEntityAt` 的 `index` 与返回的 `length` 都按 **UTF-16 code unit** 计：
//! `index` 指向 `&`；`html.indexOf(";", index + 1)` 上限 16 个 code unit；
//! `html.slice(index + 1, semicolonIndex)` 也是 code unit 切片。
//!
//! ## `Number.parseInt` 语义
//!
//! `#x`/`#X` 十六进制、`#` 十进制；允许前导空白与 `+`/`-`；遇非法字符停止；
//! 完全无数字 → `NaN` → `undefined`；十六进制允许 `0x` 前缀。码点必须是
//! `Number.isInteger` 且落在 `0..=0x10FFFF`。
//!
//! ## 已知偏差
//!
//! - 代理项码点（`0xD800..=0xDFFF`）：JS 的 `String.fromCodePoint` 会产生孤立代理项，
//!   Rust `String` 无法表示，替换为 `U+FFFD`（同类记录见 `ARCHITECTURE.md` §8 第 31 条）。
//! - 输入 `&str` 中不含孤立代理项，因此 `index + 1` 落在代理对中间时 `from_utf16` 失败
//!   等价于 JS 里得到一段含孤立代理项的实体名——它不可能匹配任何实体，返回 `None`。
//!
//! ## 这个模块为什么存在
//!
//! `syntax_highlight` 把 highlight.js 生成的 HTML 转成 ANSI 输出时，需要还原
//! `&amp;`、`&#x41;` 这类实体。实体在 TS 里按 UTF-16 下标推进，长度算错会让后续文本
//! 整体错位，所以这里不复用现成的 HTML 解码库，而是逐条复刻 TS 的两个纯函数。
//!
//! ## 与调用方的关系
//!
//! 仓库内的生产调用点只有 `syntax_highlight::decode_entity_window`：它先从 `&` 起
//! 截一段窗口，再以 `index = 0` 调用 [`decode_html_entity_at`]。这样做的原因是
//! [`decode_html_entity_at`] 每次调用都会把整个输入重新编码成 UTF-16（`Vec<u16>`），
//! 直接把整段 HTML 传进来会让每个 `&` 都付出 O(n)，整体退化为 O(n²)。
//!
//! ## 注意事项
//!
//! - `decode_html_entity` 区分大小写，只认 5 个命名实体，不是完整的 HTML 实体表；
//! - [`decode_html_entity_at`] 不校验 `html[index] == '&'`，调用方必须传 UTF-16
//!   code unit 下标且保证它指向 `&`；
//! - 返回的 `length` 是 UTF-16 code unit 数，不是 UTF-8 字节数。

/// TS: `interface DecodedHtmlEntity { text; length }`。
///
/// `length` 按 UTF-16 code unit 计（含 `&` 与 `;`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodedHtmlEntity {
    /// 解码结果，总是单个字符（代理项码点被替换为 `U+FFFD`）。
    pub text: String,
    /// 实体在输入中占用的 UTF-16 code unit 数（含首尾的 `&` 与 `;`）。
    pub length: usize,
}

/// TS 的 `\s`（`String.prototype.trim` / `Number.parseInt` 的 TrimString 用同一集合）。
///
/// 这里只用于 `parse_int_js` 去掉前导空白；与 Rust 的 `char::is_whitespace` 不同，
/// 集合包含 BOM `U+FEFF`，必须逐字对齐 JS。
fn is_js_whitespace(c: char) -> bool {
    matches!(
        c,
        ' ' | '\t' | '\n' | '\u{000B}' | '\u{000C}' | '\r' | '\u{00A0}' | '\u{1680}' | '\u{2000}'
            ..='\u{200A}'
                | '\u{2028}'
                | '\u{2029}'
                | '\u{202F}'
                | '\u{205F}'
                | '\u{3000}'
                | '\u{FEFF}'
    )
}

/// 字符在给定进制下的数值；不是该进制的数字时返回 `None`。
///
/// 字母先映射为 `10..=35` 再与 `radix` 比较，所以 16 进制下 `g` 及之后的字母都不合法。
fn digit_value(c: char, radix: u32) -> Option<u32> {
    let value = match c {
        '0'..='9' => c as u32 - '0' as u32,
        'a'..='z' => c as u32 - 'a' as u32 + 10,
        'A'..='Z' => c as u32 - 'A' as u32 + 10,
        _ => return None,
    };
    (value < radix).then_some(value)
}

/// TS: `Number.parseInt(text, radix)` 的整数子集；返回 `None` 表示 `NaN`。
///
/// 结果可能为 `±Infinity`（超长数字串），与 JS 一致——调用方用 `is_finite` 判断。
///
/// 行为逐条对齐 `parseInt`：
/// - 先跳过开头空白（`is_js_whitespace`），再处理可选的 `+`/`-`；
/// - radix 16 时可选一个 `0x`/`0X` 前缀（10 进制不会把 `0x` 当十六进制）；
/// - 从左到右累积，遇到第一个非法字符立即停止；完全没有数字 → `None`；
/// - 不支持小数、指数与 BigInt 后缀；`+`/`-` 与数字之间不允许再有空白。
///
/// 用 `f64` 是为了复刻 JS 的 `Number`。调用方只关心 `0..=0x10FFFF` 内的精确整数，
/// 这个范围远小于 `2^53`，累积过程没有舍入误差；超出范围的巨大值会在
/// `decode_code_point` 里被拒绝。
fn parse_int_js(text: &str, radix: u32) -> Option<f64> {
    let mut rest = text.trim_start_matches(is_js_whitespace);

    let mut negative = false;
    if let Some(after) = rest.strip_prefix('+') {
        rest = after;
    } else if let Some(after) = rest.strip_prefix('-') {
        negative = true;
        rest = after;
    }

    // `parseInt("0x10", 16) === 16`：只在 radix 16 时跳过 0x/0X 前缀。
    if radix == 16
        && let Some(after) = rest.strip_prefix("0x").or_else(|| rest.strip_prefix("0X"))
    {
        rest = after;
    }

    let mut value = 0.0_f64;
    let mut any = false;
    for c in rest.chars() {
        let Some(digit) = digit_value(c, radix) else {
            break;
        };
        value = value * f64::from(radix) + f64::from(digit);
        any = true;
    }

    if !any {
        return None;
    }
    Some(if negative { -value } else { value })
}

/// TS: `decodeCodePoint(codePoint)` —— 非整数/越界 → `None`。
///
/// 条件是码点合法性要求：必须有限、是整数、且 `0 <= cp <= 0x10FFFF`。`-0.0` 会通过
/// `< 0.0` 检查并转成码点 0（JS 的 ToIntegerOrInfinity 同样把 `-0` 归一为 `+0`，
/// 语料 `#-0` → NUL）。代理项码点替换为 `U+FFFD` 是已知偏差；其余码点都在合法
/// 标量值范围内，`char::from_u32` 不会失败。
fn decode_code_point(code_point: f64) -> Option<String> {
    if !code_point.is_finite() || code_point.fract() != 0.0 || code_point < 0.0 {
        return None;
    }
    if code_point > 0x0010_FFFF as f64 {
        return None;
    }
    let code_point = code_point as u32;
    // JS 会生成孤立代理项；Rust 字符串无法表示，按模块文档替换为 U+FFFD。
    if (0xD800..=0xDFFF).contains(&code_point) {
        return Some("\u{FFFD}".to_string());
    }
    char::from_u32(code_point).map(|c| c.to_string())
}

/// TS: `decodeHtmlEntity(entity)` —— 解码实体名（`&` 与 `;` 之间的部分）。
///
/// 支持 `amp`/`lt`/`gt`/`quot`/`apos`、`#x`/`#X` 十六进制、`#` 十进制。
///
/// 只实现 TS 列出的这 5 个命名实体（不是完整 HTML 实体表），名字大小写敏感
/// （`AMP` → `None`）。数字实体把 `#` 后的内容交给 `parse_int_js`，因此 `parseInt`
/// 的宽容规则（前导空白、`+`/`-`、遇到非法字符停止、16 进制可带 `0x`）全部保留；
/// `#x`/`#X` 用 16 进制，`#` 用 10 进制，最后经 `decode_code_point` 校验范围。
///
/// ```text
/// decode_html_entity("amp")      == Some("&")
/// decode_html_entity("#x41")     == Some("A")
/// decode_html_entity("#0x10")    == Some("\0")   // 10 进制在 'x' 处停止 → 0
/// decode_html_entity("#1114112") == None         // 超出 0x10FFFF
/// ```
pub fn decode_html_entity(entity: &str) -> Option<String> {
    // 命名实体只有 TS 列出的 5 个，且大小写敏感。
    match entity {
        "amp" => return Some("&".to_string()),
        "lt" => return Some("<".to_string()),
        "gt" => return Some(">".to_string()),
        "quot" => return Some("\"".to_string()),
        "apos" => return Some("'".to_string()),
        _ => {}
    }

    // `#x`/`#X` 走 16 进制；下面的 `#` 走 10 进制。两者都复用 parseInt 语义
    // （例如 `#x0x41`、`# 65` 都能解出值）。
    if let Some(rest) = entity
        .strip_prefix("#x")
        .or_else(|| entity.strip_prefix("#X"))
    {
        return decode_code_point(parse_int_js(rest, 16)?);
    }

    if let Some(rest) = entity.strip_prefix('#') {
        return decode_code_point(parse_int_js(rest, 10)?);
    }

    None
}

/// TS: `decodeHtmlEntityAt(html, index)` —— 从 UTF-16 下标 `index`（指向 `&`）解码一个实体。
///
/// 返回 `length`（UTF-16 code unit，含 `&` 与 `;`）与解码文本；不是实体时 `None`。
///
/// # 约束与失败模式
///
/// - `index` 按 UTF-16 code unit 计，必须指向 `&`——函数本身不检查这一点；
/// - 从 `index + 1` 起找第一个 `;`；找不到，或 `;` 与 `index` 的距离超过 16 个
///   code unit，返回 `None`；
/// - 实体名是两者之间的 code unit 序列，用 `String::from_utf16` 还原；若 `index + 1`
///   落在代理对中间（即 `index` 指向代理对的高位 code unit），还原失败 → `None`
///   （见模块文档的已知偏差）；
/// - 成功时 `length = 分号位置 - index + 1`。
///
/// # 复杂度
///
/// 每次调用都会把整个 `html` 编码成 `Vec<u16>`，时间与空间都是 O(html.len())；
/// 需要逐字符扫描的调用方应先截窗口（见模块文档）。
pub fn decode_html_entity_at(html: &str, index: usize) -> Option<DecodedHtmlEntity> {
    // TS 的 `html[index]` / `slice` 都按 UTF-16 code unit 计，这里先整体转成
    // UTF-16 序列，后续下标与长度才是同一套单位。
    let units: Vec<u16> = html.encode_utf16().collect();
    let start = index.checked_add(1)?;
    if start > units.len() {
        return None;
    }

    // TS: `html.indexOf(";", index + 1)`。只找第一个分号，不要求它是合法实体结尾。
    let semicolon = units[start..]
        .iter()
        .position(|&unit| unit == u16::from(b';'))
        .map(|offset| start + offset)?;

    // TS: `semicolonIndex - index > 16` 时视为非实体（限制实体名的总长度）。
    if semicolon - index > 16 {
        return None;
    }

    let entity = String::from_utf16(&units[start..semicolon]).ok()?;
    let text = decode_html_entity(&entity)?;

    Some(DecodedHtmlEntity {
        text,
        length: semicolon - index + 1,
    })
}
