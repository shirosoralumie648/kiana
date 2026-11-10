//! crate 私有的 JS 语义桥接：键序、`Number(string)`、`String(number)` 与 JSON 文本。
//!
//! 这些函数只在 crate 内使用（`memory.rs` / `render.rs` / `json.rs` / `jsjson.rs` /
//! `testing.rs`）；公开 API 上的 JSON 输出统一走 `kiana_json`（[`kiana_json::js_object::stringify`] 与
//! [`kiana_json::js_num::format_js_number`]），这里补的是 `kiana_json` 没有覆盖的少数情形：
//! - `IndexMap` 形态的 `Object.fromEntries` / `Object.entries` 键序（整数样键升序在前）；
//! - `Number(string)`（`Number("0x10") === 16`、`Number("12abc") === NaN`）；
//! - `String(number)`（`NaN` → `"NaN"`，模板字面量用；`format_js_number` 是 JSON 语义，会写成
//!   `null`，两者不能混用——`render.rs` 的时长/事件时间用前者，属性值用后者）；
//! - JS 字符串的 `<` / `>`（UTF-16 code unit，不是 Rust 的 UTF-8/标量序）。
//!
//! # 为什么单独成模块
//!
//! 这些差异集中在“数值/字符串/键序”的边界上。集中到一处后，每个调用方只需选择语义正确
//! 的函数，而不必各自复制一套边界处理；回归测试（本文件末尾与 `tests/js_edges.rs`）也能
//! 只覆盖一次。
//!
//! # 关键不变量
//!
//! - [`js_number_text`] 与 `kiana_json::format_js_number` 语义不同：前者用于模板字面量 /
//!   `String(x)`（保留 `NaN`/`±Infinity`），后者用于 JSON（非有限 → `null`），不可互换。
//! - [`js_entries`] / [`sort_js_keys`] 只重排键，不改变值；它们假设 `IndexMap` 的插入顺序
//!   就是 JS 对象的属性插入顺序。
//! - [`js_number_from_str`] 只实现 `Number(string)` 的**字符串**分支，不做对象/数组的
//!   `ToPrimitive`；调用方负责先把值转成字符串。
//!
//! TS 源：`packages/telemetry/src/memory.ts`（`copyAttributes`、`Number(span)`）、
//! `packages/telemetry/src/render.ts`（模板字面量里的数字）。

use indexmap::IndexMap;
use serde_json::Value;

use crate::index::{AttributeValue, SpanAttributes, SpanStatus, StatusError, TelemetryCarrier};
use kiana_json::format_js_number;
use kiana_json::js_quote;

/// JS 数组索引形态的键：`0`…`4294967294`（规范形式，无前导零）。
///
/// `Object.keys` / `Object.entries` 把这类键排到最前并按数值升序；`"01"`、`"-1"`、
/// `"1.5"` 不是数组索引。
///
/// 判定顺序：长度 1..=10（合法数组索引最大 4294967294，最多 10 位）、除 `"0"` 外不以 `0`
/// 开头、全 ASCII 数字、解析后严格小于 `u32::MAX`（4294967295 恰好不是合法索引）。这与规范
/// 对 array index 的定义一致：必须是规范形式的数字串，且是 `< 2^32 - 1` 的非负整数。
fn array_index(key: &str) -> Option<u64> {
    if key.is_empty() || key.len() > 10 {
        return None;
    }
    if key.len() > 1 && key.starts_with('0') {
        return None;
    }
    if !key.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let value: u64 = key.parse().ok()?;
    if value >= u32::MAX as u64 {
        return None;
    }
    Some(value)
}

/// `Object.entries(map)` 的遍历顺序：整数样键按数值升序在前，其余保持插入序。
///
/// 返回借用避免复制属性值；`render.rs` 的 `formatAttributes` 需要这一顺序。
///
/// 实现分两趟：先按键的类型分组，再对索引键按数值排序。索引键互不相同，所以排序结果与
/// 它们的原始插入位置无关；非索引键直接沿用 `IndexMap` 的迭代顺序。复杂度
/// `O(n + k log k)`（`k` 为索引键个数）；返回的 `Vec` 只借用原 map 的键值，不复制它们。
pub(crate) fn js_entries<V>(map: &IndexMap<String, V>) -> Vec<(&str, &V)> {
    let mut indices: Vec<(u64, &str, &V)> = Vec::new();
    let mut others: Vec<(&str, &V)> = Vec::new();
    for (key, value) in map {
        match array_index(key) {
            Some(index) => indices.push((index, key.as_str(), value)),
            None => others.push((key.as_str(), value)),
        }
    }
    indices.sort_by_key(|(index, _, _)| *index);
    let mut out = Vec::with_capacity(map.len());
    out.extend(indices.into_iter().map(|(_, key, value)| (key, value)));
    out.extend(others);
    out
}

/// `Object.fromEntries` 之后的键序：整数样键按数值升序在前，其余保持插入序。
///
/// 与 [`js_entries`] 的区别是返回**新的** map：每个值都要 `Clone`，原 map 不被修改。
/// `memory.rs` 的 `copyAttributes` 用它把调用方传入的属性重排成 JS 对象顺序。
pub(crate) fn sort_js_keys<V: Clone>(map: &IndexMap<String, V>) -> IndexMap<String, V> {
    let mut out = IndexMap::with_capacity(map.len());
    for (key, value) in js_entries(map) {
        out.insert(key.to_string(), value.clone());
    }
    out
}

/// JS 字符串的 `<` / `>` 比较：按 **UTF-16 code unit** 逐单元比较（代理对按两个单元）。
///
/// Rust 的 `str`/`String` 的 `Ord` 是 UTF-8 字节序（等价于 Unicode 标量序），对 BMP 内的字符
/// 与 UTF-16 一致，但排到代理对（U+10000 以上）时顺序不同：U+1F600（😀）在 UTF-16 里以
/// U+D83D 开头，小于 U+FFFD，而按标量序却更大（ARCHITECTURE.md §4.6）。
///
/// 例：JS 中 `"😀" < "\u{FFFD}"` 为真（首单元 U+D83D < U+FFFD），Rust 的 `"😀" < "\u{FFFD}"`
/// 为假（标量 U+1F600 > U+FFFD）。`encode_utf16()` 是迭代器，比较过程不分配临时字符串。
pub(crate) fn js_utf16_cmp(left: &str, right: &str) -> std::cmp::Ordering {
    left.encode_utf16().cmp(right.encode_utf16())
}

/// `String(span)` 的数字文本（`RecordedTelemetrySpan.context["kiana-memory-span"]`）。
///
/// 与 `format_js_number` 的分工：本函数服务模板字面量（`${x}`）和 `String(x)`，非有限值必须
/// 原样写成 `NaN` / `Infinity` / `-Infinity`；JSON 桥接则用 `format_js_number` 写成 `null`。
/// 两者混用会改变黄金文本。有限值委托 `format_js_number`，因此 `-0` 也写成 `"0"`。
pub(crate) fn js_number_text(value: f64) -> String {
    if value.is_nan() {
        return "NaN".to_string();
    }
    if value.is_infinite() {
        return if value > 0.0 { "Infinity" } else { "-Infinity" }.to_string();
    }
    format_js_number(value)
}

/// ECMAScript 的 WhiteSpace + LineTerminator 集合：含 TAB/LF/VT/FF/CR、普通空格、NBSP、
/// 各类 Unicode 空格、行/段分隔符、U+3000 与 BOM（U+FEFF）。
///
/// 不能用 `str::trim`：它按 Rust 的 `char::is_whitespace` 判定，集合并不相同（例如 U+FEFF
/// 不在 `White_Space` 属性里，但 JS 的 `Number()` 会 trim 它）。
fn is_js_whitespace(ch: char) -> bool {
    matches!(
        ch,
        '\u{0009}'
            | '\u{000a}'
            | '\u{000b}'
            | '\u{000c}'
            | '\u{000d}'
            | '\u{0020}'
            | '\u{00a0}'
            | '\u{1680}'
            | '\u{2000}'
            ..='\u{200a}'
                | '\u{2028}'
                | '\u{2029}'
                | '\u{202f}'
                | '\u{205f}'
                | '\u{3000}'
                | '\u{feff}'
    )
}

/// 两端删除 JS 空白（见 [`is_js_whitespace`]）；字符串中间的空白原样保留。
fn trim_js_whitespace(text: &str) -> &str {
    text.trim_matches(is_js_whitespace)
}

/// `Number(string)`：与 `str::parse::<f64>()` 的差异必须逐条复刻。
///
/// - 空串/纯空白 → `0`（`Number("") === 0`）；
/// - `"Infinity"`/`"+Infinity"`/`"-Infinity"`；`"inf"`/`"NaN"` → `NaN`；
/// - `0x`/`0o`/`0b` 前缀（`Number("0x10") === 16`）；
/// - 十进制文法必须整串匹配（`Number("12abc") === NaN`）。
///
/// 执行顺序：先按 JS 空白集 trim，再特判 `±Infinity`，然后识别三种进制前缀，最后用
/// [`is_decimal_literal`] 校验十进制文法。之所以不能直接 `trimmed.parse::<f64>()`，是因为
/// Rust 的解析器还接受 `"inf"`、`"NaN"` 等 JS 不接受的拼写；前缀形式也不能直接交给它。
/// 任何解析失败都用 `NaN` 表示——`Number()` 没有异常通道。
pub(crate) fn js_number_from_str(text: &str) -> f64 {
    let trimmed = trim_js_whitespace(text);
    if trimmed.is_empty() {
        return 0.0;
    }
    match trimmed {
        "Infinity" | "+Infinity" => return f64::INFINITY,
        "-Infinity" => return f64::NEG_INFINITY,
        _ => {}
    }
    if let Some(rest) = trimmed
        .strip_prefix("0x")
        .or_else(|| trimmed.strip_prefix("0X"))
    {
        return parse_radix_digits(rest, 16);
    }
    if let Some(rest) = trimmed
        .strip_prefix("0o")
        .or_else(|| trimmed.strip_prefix("0O"))
    {
        return parse_radix_digits(rest, 8);
    }
    if let Some(rest) = trimmed
        .strip_prefix("0b")
        .or_else(|| trimmed.strip_prefix("0B"))
    {
        return parse_radix_digits(rest, 2);
    }
    if !is_decimal_literal(trimmed) {
        return f64::NAN;
    }
    trimmed.parse::<f64>().unwrap_or(f64::NAN)
}

/// `2f64.powf(exp)` 的精确版（只用于本模块的正规化结果，`exp` 由二进制位数推出）。
///
/// 直接拼 f64 的指数位：`exp > 1023` 溢出为 `Infinity`；`exp >= -1022` 是正规数；更小的指数
/// 在本模块不可达（`parse_radix_digits` 的指数 ≥ 12），返回 `0.0` 只是防御性分支。
/// 之所以不用 `powf`：这里需要按位精确的 2 的幂，不能引入 libm 的舍入差异。
fn pow2(exp: i64) -> f64 {
    if exp > 1023 {
        return f64::INFINITY;
    }
    if exp >= -1022 {
        f64::from_bits(((exp + 1023) as u64) << 52)
    } else {
        // 本模块不会走到次正规：radix 字面量的指数 ≥ 12（见 [`parse_radix_digits`]）。
        0.0
    }
}

/// `0x`/`0o`/`0b` 字面量的数字串 → `f64`，与 V8 的 `StringToDouble` 一样**正确舍入**
/// （round-half-to-even）。
///
/// 逐位 `value * radix + digit` 的 f64 累加在超过 53 位后每步都会二次舍入，
/// 长字面量（> 32 个十六进制位）常出现 1 ULP 偏低。这里改为：
///
/// 1. 位数 ≤ 64 → `u64` 精确保存后 `as f64`（Rust 的整数→浮点转换是正确舍入）；
/// 2. 否则取前 64 位（`head`）+ 一个 sticky 位（更低位的任意 1），按 round-half-to-even
///    手工进位，再乘以精确的 2 的幂。
fn parse_radix_digits(digits: &str, radix: u32) -> f64 {
    // 阶段 1：整串必须是该进制的合法数字；空串/混入非法字符都按 Number() 的语义得到 NaN。
    if digits.is_empty() || !digits.chars().all(|c| c.is_digit(radix)) {
        return f64::NAN;
    }
    debug_assert!(matches!(radix, 2 | 8 | 16));
    let bits_per_digit = radix.trailing_zeros();
    // 前导零不影响数值，但会污染位长计算（`Number("0x0000ff") === 255`）。
    let significant = digits.trim_start_matches('0');
    if significant.is_empty() {
        return 0.0;
    }
    let first_digit = significant
        .chars()
        .next()
        .and_then(|ch| ch.to_digit(radix))
        .unwrap_or(0);
    // `significant` 的十进制长度就是 digit 数（radix ≤ 16 时 ASCII）。
    let digit_count = significant.len() as u64;
    let first_bits = 32 - first_digit.leading_zeros();
    // 阶段 2：位长 = 首位有效位数 + 其余 digit 的位数；用它决定走精确整数还是舍入路径。
    let total_bits = (digit_count - 1) * u64::from(bits_per_digit) + u64::from(first_bits);

    // 位长 ≤ 64：u64 能精确保存，整数→浮点的转换由 Rust 保证正确舍入（round-half-to-even）。
    if total_bits <= 64 {
        let mut value: u64 = 0;
        for ch in significant.chars() {
            value = value * u64::from(radix) + u64::from(ch.to_digit(radix).unwrap_or(0));
        }
        return value as f64;
    }

    // 阶段 3：取前 k 个 digit，使它们的位宽首次达到 ≥ 64（最多 67 位）。
    // 这样 head 的精度足以决定 f64 的 53 位尾数与舍入位，后面的 digit 只贡献 sticky。
    let mut head: u128 = 0;
    let mut prefix_bits: u64 = 0;
    let mut consumed = 0usize;
    for ch in significant.chars() {
        let digit = u128::from(ch.to_digit(radix).unwrap_or(0));
        head = (head << bits_per_digit) | digit;
        prefix_bits = if consumed == 0 {
            u64::from(32 - (digit as u32).leading_zeros())
        } else {
            prefix_bits + u64::from(bits_per_digit)
        };
        consumed += 1;
        if prefix_bits >= 64 {
            break;
        }
    }
    let extra = prefix_bits - 64; // 0..=3
    let top = (head >> extra) as u64; // 恰好 64 位（首位为 1）
    let mut sticky = (head & ((1u128 << extra) - 1)) != 0;
    if !sticky {
        // 剩余 digit 中只要有一个非零，就说明被截断的小数部分 > 0（sticky 位）。
        // `significant` 已由 `is_digit` 校验为 ASCII，按字节切片不会落到 UTF-8 中间。
        sticky = significant[consumed..].chars().any(|ch| ch != '0');
    }

    // 阶段 4：小数点固定在 64 位处，先取高 53 位，再用“舍入位 + sticky（含被截断部分）”
    // 做 round-half-to-even（与 V8 StringToDouble 一致）。
    // value = (top + fraction) * 2^exponent，其中 fraction ∈ [0,1)，sticky 表示其是否 > 0。
    let exponent = (total_bits - 64) as i64;
    let mut mantissa = top >> 11; // 高 53 位
    let round_bit = (top >> 10) & 1;
    let rest = top & 0x3ff;
    let sticky = sticky || rest != 0;
    let mut exponent = exponent + 11;
    // 进位可能把 mantissa 顶到 2^53，此时右移一位并增大指数（保持值不变）。
    if round_bit == 1 && (sticky || mantissa & 1 == 1) {
        mantissa += 1;
        if mantissa >> 53 != 0 {
            mantissa >>= 1;
            exponent += 1;
        }
    }
    // 阶段 5：mantissa（≤ 2^53）转 f64 是精确的；乘 2 的幂可能溢出为 Infinity（与 V8 一致）。
    mantissa as f64 * pow2(exponent)
}

/// StrDecimalLiteral（不含 `Infinity`，已在外层处理）。
///
/// 文法：`[+|-] (digits [. digits?] | . digits) ([eE] [+|-] digits)?`，且必须消费完整串。
/// 返回 `false` 时调用方直接得到 `NaN`，不会进入 `str::parse::<f64>` —— 后者会接受
/// `"inf"`、`"NaN"` 等 JS `Number()` 不接受的拼写，需要在这里挡掉。
fn is_decimal_literal(text: &str) -> bool {
    let bytes = text.as_bytes();
    let mut index = 0usize;
    if index < bytes.len() && (bytes[index] == b'+' || bytes[index] == b'-') {
        index += 1;
    }
    let int_start = index;
    while index < bytes.len() && bytes[index].is_ascii_digit() {
        index += 1;
    }
    let int_digits = index - int_start;
    let mut frac_digits = 0usize;
    if index < bytes.len() && bytes[index] == b'.' {
        index += 1;
        let frac_start = index;
        while index < bytes.len() && bytes[index].is_ascii_digit() {
            index += 1;
        }
        frac_digits = index - frac_start;
    }
    if int_digits == 0 && frac_digits == 0 {
        return false;
    }
    if index < bytes.len() && (bytes[index] == b'e' || bytes[index] == b'E') {
        index += 1;
        if index < bytes.len() && (bytes[index] == b'+' || bytes[index] == b'-') {
            index += 1;
        }
        let exp_start = index;
        while index < bytes.len() && bytes[index].is_ascii_digit() {
            index += 1;
        }
        if index == exp_start {
            return false;
        }
    }
    index == bytes.len()
}

/// `JSON.stringify(attribute)` 的文本（`render.rs` 的属性片段）。
///
/// 每个分支对应一种 `AttributeValue`：字符串经 [`js_quote`]；数字经 `format_js_number`
/// （非有限 → `null`）；布尔为 `true`/`false`；数组用 `,` 连接且元素之间没有空格。
/// 返回的是**值片段**，不含 `key=` 与分隔符，由调用方拼装。
pub(crate) fn attribute_json_text(value: &AttributeValue) -> String {
    match value {
        AttributeValue::String(text) => js_quote(text),
        AttributeValue::Number(number) => format_js_number(*number),
        AttributeValue::Boolean(flag) => flag.to_string(),
        AttributeValue::StringArray(items) => {
            let parts: Vec<String> = items.iter().map(|item| js_quote(item)).collect();
            format!("[{}]", parts.join(","))
        }
        AttributeValue::NumberArray(items) => {
            let parts: Vec<String> = items.iter().map(|item| format_js_number(*item)).collect();
            format!("[{}]", parts.join(","))
        }
        AttributeValue::BooleanArray(items) => {
            let parts: Vec<String> = items.iter().map(|item| item.to_string()).collect();
            format!("[{}]", parts.join(","))
        }
    }
}

/// 属性值 → `serde_json::Value`（JSON 语义：非有限数字写成 `null`）。
///
/// 与 [`attribute_json_text`] 的区别：这里产出的是 `Value` 树，供后续 `stringify` 或其他
/// `Value` 处理使用；标量语义相同。`NumberArray` 的每个元素都经 [`number_value`]，
/// 因此数组里不会留下 `NaN` 字面量。
pub(crate) fn attribute_to_value(value: &AttributeValue) -> Value {
    match value {
        AttributeValue::String(text) => Value::String(text.clone()),
        AttributeValue::Number(number) => number_value(*number),
        AttributeValue::Boolean(flag) => Value::Bool(*flag),
        AttributeValue::StringArray(items) => {
            Value::Array(items.iter().cloned().map(Value::String).collect())
        }
        AttributeValue::NumberArray(items) => {
            Value::Array(items.iter().map(|item| number_value(*item)).collect())
        }
        AttributeValue::BooleanArray(items) => {
            Value::Array(items.iter().map(|item| Value::Bool(*item)).collect())
        }
    }
}

/// 有限 `f64` → JSON 数字；`NaN`/`±Infinity` → `Value::Null`。
///
/// `serde_json::Number::from_f64` 对非有限值返回 `None`，这里映射成 `null`，正是
/// `JSON.stringify` 的行为（它把非有限数字写成 `null`，而不是报错）。`-0.0` 可以构造为
/// `Number`，最终文本由 `format_js_number` 写成 `0`（符号只在内存表示里保留）。
pub(crate) fn number_value(number: f64) -> Value {
    serde_json::Number::from_f64(number).map_or(Value::Null, Value::Number)
}

/// `SpanAttributes` → JSON 对象（插入序；调用方应先经过 [`sort_js_keys`]）。
///
/// 本函数不做整数样键重排：直接比较 `Value` 键序的调用方应先用 [`sort_js_keys`]；若最终
/// 走 `kiana_json::js_object::stringify`，键序会在写出时按 JS 规则统一处理。
/// 属性值逐个经 [`attribute_to_value`]，因此非有限数字在树里就是 `Value::Null`。
pub(crate) fn attributes_to_value(attributes: &SpanAttributes) -> Value {
    let mut map = serde_json::Map::with_capacity(attributes.len());
    for (key, value) in attributes {
        map.insert(key.clone(), attribute_to_value(value));
    }
    Value::Object(map)
}

/// `TelemetryCarrier` → JSON 对象（普通字符串 map，保持插入序）。
///
/// carrier 的值按类型定义都是 `String`，因此不需要数字/布尔分支；键序同样由
/// `kiana_json::js_object::stringify` 在文本层规范化为 JS 顺序。
pub(crate) fn carrier_to_value(carrier: &TelemetryCarrier) -> Value {
    let mut map = serde_json::Map::with_capacity(carrier.len());
    for (key, value) in carrier {
        map.insert(key.clone(), Value::String(value.clone()));
    }
    Value::Object(map)
}

/// `SpanStatus` → JSON 对象（`{status:"ok"}` / `{status:"error",error:{name,message}}`）。
///
/// `Error` 但 `error` 为 `None` 时只写 `{status:"error"}`（对应 TS 里抛出的不是 `Error`
/// 实例、无法抽出 name/message 的情况）；有详情时才追加嵌套的 `error` 对象。
pub(crate) fn status_to_value(status: &SpanStatus) -> Value {
    let mut map = serde_json::Map::new();
    match status {
        SpanStatus::Ok => {
            map.insert("status".to_string(), Value::String("ok".to_string()));
        }
        SpanStatus::Error { error } => {
            map.insert("status".to_string(), Value::String("error".to_string()));
            if let Some(StatusError { name, message }) = error {
                let mut inner = serde_json::Map::new();
                inner.insert("name".to_string(), Value::String(name.clone()));
                inner.insert("message".to_string(), Value::String(message.clone()));
                map.insert("error".to_string(), Value::Object(inner));
            }
        }
    }
    Value::Object(map)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn js_number_from_str_matches_number_constructor() {
        assert_eq!(js_number_from_str(""), 0.0);
        assert_eq!(js_number_from_str(" 2 "), 2.0);
        assert_eq!(js_number_from_str("0x10"), 16.0);
        assert_eq!(js_number_from_str("1e3"), 1000.0);
        assert_eq!(js_number_from_str(".5"), 0.5);
        assert_eq!(js_number_from_str("5."), 5.0);
        assert!(js_number_from_str("12abc").is_nan());
        assert!(js_number_from_str("inf").is_nan());
        assert!(js_number_from_str("NaN").is_nan());
        assert_eq!(js_number_from_str("Infinity"), f64::INFINITY);
        assert_eq!(js_number_from_str("0b101"), 5.0);
        assert_eq!(js_number_from_str("0o17"), 15.0);
        assert!(js_number_from_str("0x").is_nan());
    }

    /// `Number("0x…")` 必须与 V8 一致地正确舍入：长十六进制字面量（> 32 位）此前会低 1 ULP。
    ///
    /// fixture 由 node 26 录制：每项 `[字面量, "<f64 bits hex>|<finite|inf|-inf>|<String(n)>"]`。
    #[test]
    fn radix_literals_are_correctly_rounded_like_v8() {
        let raw = include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/number_radix.json"
        ));
        let cases: Vec<(String, String)> = serde_json::from_str(raw).expect("number_radix.json");
        assert!(cases.len() > 500);
        let mut checked_large = 0;
        for (literal, packed) in &cases {
            let mut parts = packed.split('|');
            let bits = parts.next().expect("bits");
            let kind = parts.next().expect("kind");
            let text = parts.next().expect("string");
            let value = js_number_from_str(literal);
            assert_eq!(
                format!("{:016x}", value.to_bits()),
                bits,
                "Number({literal}) bits"
            );
            assert_eq!(js_number_text(value), text, "String(Number({literal}))");
            if value.is_finite() && literal.len() > 34 && literal.starts_with("0x") {
                checked_large += 1;
            }
            if kind == "inf" {
                assert!(value.is_infinite(), "{literal}");
            }
        }
        // 确保样本覆盖了 > 32 位十六进制位（u128 放得下、旧实现走 u128 分支）与更长的字面量，
        // 且确实包含旧实现会低 1 ULP 的形态。
        assert!(checked_large >= 200, "checked_large = {checked_large}");
    }

    /// reviewer 的具体向量：旧实现的 f64 逐位累加在这里低 1 ULP。
    #[test]
    fn long_hex_literal_matches_v8_bits() {
        let value = js_number_from_str("0x2cc8a8ecf4c441340a84cf45a8209b86516720db");
        assert_eq!(value.to_bits(), 0x49c6_6454_767a_6221);
    }

    /// 前导零、边界位数与 radix 2/8 的长字面量。
    #[test]
    fn radix_edge_lengths() {
        assert_eq!(
            js_number_from_str("0x00000000000000000000000000000000000000000000abc"),
            2748.0
        );
        assert_eq!(
            js_number_from_str("0xffffffffffffffff"),
            18446744073709551615.0
        );
        // 2^128（33 个十六进制位）：u128 已放不下，走 64 位头部 + 指数路径。
        assert_eq!(
            js_number_from_str("0x100000000000000000000000000000000"),
            pow2(128)
        );
        assert!(js_number_from_str(&format!("0x1{}", "0".repeat(300))).is_infinite());
        // radix 2/8：长串同样正确舍入。
        let binary = format!("0b1{}", "0".repeat(200));
        assert_eq!(js_number_from_str(&binary), 2f64.powi(200));
        let octal = format!("0o1{}", "0".repeat(60));
        assert_eq!(js_number_from_str(&octal), 8f64.powi(60));
    }

    #[test]
    fn integer_keys_move_to_the_front_and_keep_ascending_order() {
        let mut map = IndexMap::new();
        map.insert("b".to_string(), 1);
        map.insert("2".to_string(), 2);
        map.insert("a".to_string(), 3);
        map.insert("1".to_string(), 4);
        let sorted = sort_js_keys(&map);
        assert_eq!(
            sorted.keys().cloned().collect::<Vec<_>>(),
            vec!["1", "2", "b", "a"]
        );
    }
}
