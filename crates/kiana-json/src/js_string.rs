//! JS 的 `String(v)`、truthiness 与 typeof。
//!
//! 被 `kiana-mcp` 的 `toError`（`String(v)` 语义）、`kiana-chord` 的错误消息、`kiana-durable` 的
//! 日志与 `kiana-tools` 的参数回显共同使用。实现必须逐字符对齐 JS：
//!
//! | 值 | `String(v)` |
//! |---|---|
//! | `undefined` | `"undefined"` |
//! | `null` | `"null"` |
//! | `true` / `false` | `"true"` / `"false"` |
//! | 数字 | `Number::toString`（见 [`crate::js_num`]） |
//! | 字符串 | 自身 |
//! | 数组 | 各元素 `String()` 后以 `","` 连接；`null`/`undefined` 元素为空串 |
//! | 对象 | `"[object Object]"` |
//! | 函数 / 其他 | `"[object Object]"` 或不适用 |
//!
//! # 强制偏差：JSON 溢出数字
//!
//! `js_parse` 被迫把 `1e400` 这类溢出字面量存成 `Value::Null`（`serde_json::Number` 存不下
//! `Infinity`，见 `js_parse` 模块文档）。对**解析结果**调用本模块时因此可观察到差异：
//! TS 的 `String(JSON.parse("1e400"))` 是 `"Infinity"`，这里是 `"null"`；
//! `Boolean(..)` 是 `true` vs `false`；`typeof ..` 是 `"number"` vs `"object"`。
//! 这无法在 `Value` 层面修复（§4.9 冻结签名），故记为 `ARCHITECTURE.md` §8 第 32 条的
//! 一部分。本模块**自身**的非有限数处理是正确的：只要能构造出 `Value::Number`，就按
//! JS 的 `NaN`/`Infinity`/`-Infinity` 输出（见本文件内 `js_number_to_string`）。
//!
//! # 执行流程与实现约束
//!
//! [`js_string`] 用显式栈按先序展开值树：对象直接写 `[object Object]`（不再深入），
//! 数组按 `Array.prototype.join(",")` 展开——元素之间写逗号，`null` 元素写空串，
//! 嵌套数组递归展开（因此 `String([1,[2,3]])` 是 `"1,2,3"`）。`None` 表示
//! `undefined`，`Some(Value::Null)` 表示 `null`，两者输出不同。
//!
//! # 为什么单独建模 `JsType`
//!
//! `typeof` 的结果是字符串（`"object"`/`"number"`/…），但字符串字面量容易拼错大小写；
//! 用枚举 + [`JsType::as_str`] 先在类型层约束取值集合。注意 [`js_typeof`] 只接受
//! `serde_json::Value`，其变体里没有 `undefined` 与函数，因此实际只会返回
//! [`JsType::Object`]、[`JsType::Boolean`]、[`JsType::Number`]、[`JsType::String`]；
//! `Undefined` 与 `Function` 是 `Value` 层不可达的两个变体，不会被本函数返回。
//! 枚举覆盖的是 `Value` 能表达的 `typeof` 子集（Object/Boolean/Number/String）外加这两个
//! 变体；ECMAScript 的 `typeof` 还会返回 `"bigint"` 与 `"symbol"`，本枚举未建模。
//!
//! # 注意事项
//!
//! - 这些函数都在**值层**模拟 JS 语义，不执行 JS 代码；`valueOf`/`toString` 自定义
//!   方法不参与。
//! - [`js_trim`] 与 `js_num::parse_js_number` 共用 `js_num` 里的 ECMAScript 空白集；
//!   修改空白定义会同时改变数字解析与 `trim` 的行为。
//! - [`js_strict_equal`] 对数组/对象一律返回 `false`（JSON 值没有对象身份）；需要按
//!   内容比较时用 [`crate::js_object::deep_equal`]。
//! - [`js_quote`] 不做 `U+2028`/`U+2029` 转义，这是 `JSON.stringify` 的真实行为；
//!   若输出要嵌入 `<script>` 或 JS 源码字面量，需要调用方额外处理。

use serde_json::Value;

use crate::js_num::format_js_number;

/// JS 的 `typeof` 结果（对象用 `"object"`；`null` 也是 `"object"`）。
///
/// 变体覆盖 `serde_json::Value` 能表达的 `typeof` 子集（`Object`/`Boolean`/`Number`/`String`），
/// 外加 `Value` 层不可达的 `Undefined`/`Function`；ECMAScript 的 `typeof` 还会返回
/// `"bigint"` 与 `"symbol"`，本枚举未建模。到字符串的映射只在 [`JsType::as_str`]
/// 一处，避免各调用方各写一份字面量导致拼写漂移。
///
/// `Undefined` 与 `Function` 不会被 [`js_typeof`] 返回——`serde_json::Value` 无法
/// 表示这两种 JS 值。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JsType {
    /// `typeof undefined === "undefined"`；对应 [`js_string`] 的 `None` 输入。
    Undefined,
    /// `typeof {}`、`typeof []`、`typeof null` 都是 `"object"`。
    Object,
    /// `typeof true === "boolean"`。
    Boolean,
    /// `typeof 1 === "number"`；`NaN`/`Infinity` 也是 `"number"`。
    Number,
    /// `typeof "x" === "string"`。
    String,
    /// `typeof function(){} === "function"`；`Value` 层不可达。
    Function,
}

impl JsType {
    /// `typeof` 的字符串形式（与 V8 的返回值逐字一致，全部小写）。
    pub fn as_str(self) -> &'static str {
        match self {
            JsType::Undefined => "undefined",
            JsType::Object => "object",
            JsType::Boolean => "boolean",
            JsType::Number => "number",
            JsType::String => "string",
            JsType::Function => "function",
        }
    }
}

/// JSON 值对应的 `typeof`。`Value::Null` 对应 JS 的 `null`（`typeof null === "object"`）。
///
/// # 映射
///
/// | `Value` 变体 | 结果 |
/// |---|---|
/// | `Null` | [`JsType::Object`]（`typeof null === "object"` 的历史行为） |
/// | `Bool` | [`JsType::Boolean`] |
/// | `Number` | [`JsType::Number`] |
/// | `String` | [`JsType::String`] |
/// | `Array` / `Object` | [`JsType::Object`] |
///
/// # 边界
/// `Value` 没有 `undefined`/函数变体，因此本函数永远不会返回 `Undefined`/`Function`。
/// 解析溢出得到的 `Value::Null`（见 `js_parse` 模块文档）会被报成 `"object"`，
/// 属于该模块列出的被迫偏差。
pub fn js_typeof(value: &Value) -> JsType {
    match value {
        Value::Null => JsType::Object,
        Value::Bool(_) => JsType::Boolean,
        Value::Number(_) => JsType::Number,
        Value::String(_) => JsType::String,
        Value::Array(_) | Value::Object(_) => JsType::Object,
    }
}

/// `String(value)`，其中 `undefined` 用 `None` 表示。
///
/// 显式栈实现（不递归）：`String(v)` 在 V8 里遇到过深的数组只抛可捕获的 `RangeError`，
/// 而固定栈的 Rust 递归会在 ~20k 层 abort 整个进程；本 crate 的 `parse` 又能构造出任意
/// 深度的值，因此这里与 `js_object` 的遍历保持一致。
///
/// # 参数
/// - `None`：JS 的 `undefined`，输出 `"undefined"`。
/// - `Some(&Value::Null)`：输出 `"null"`。
///
/// # 规则
/// 标量按字面输出；数组按 `Array.prototype.join(",")`：`null`/`undefined` 元素写成
/// 空串但逗号保留（`[1,null,2]` → `"1,,2"`）；对象（包括嵌套在数组里的对象）一律
/// `"[object Object]"`，不展开字段。
///
/// # 与 V8 的差异
/// V8 对过深数组抛可捕获的 `RangeError`；本实现不抛错：任意有限深度的值都会正常返回，
/// 显式栈占用与总节点数成正比，深度本身不额外消耗调用栈。
/// 调用方若需要与 V8 的异常行为一致，必须在更上层限制深度。
pub fn js_string(value: Option<&Value>) -> String {
    enum Task<'a> {
        Value(Option<&'a Value>),
        /// `Array.prototype.join` 的分隔符（在**下一个**元素之前写出）。
        Comma,
    }
    let mut out = String::new();
    let mut tasks = vec![Task::Value(value)];
    while let Some(task) = tasks.pop() {
        match task {
            Task::Comma => out.push(','),
            Task::Value(None) => out.push_str("undefined"),
            Task::Value(Some(Value::Null)) => out.push_str("null"),
            Task::Value(Some(Value::Bool(b))) => out.push_str(if *b { "true" } else { "false" }),
            Task::Value(Some(Value::Number(n))) => {
                // JS 里 JSON 数字都是 f64；超出 f64 精确范围的值在解析时已经按 JS 舍入。
                let f = n.as_f64().unwrap_or(f64::NAN);
                out.push_str(&js_number_to_string(f));
            }
            Task::Value(Some(Value::String(s))) => out.push_str(s),
            Task::Value(Some(Value::Array(items))) => {
                // `Array.prototype.join(",")`：`null`/`undefined` 元素写成空串。
                for (i, item) in items.iter().enumerate().rev() {
                    match item {
                        Value::Null => {}
                        other => tasks.push(Task::Value(Some(other))),
                    }
                    if i > 0 {
                        tasks.push(Task::Comma);
                    }
                }
            }
            Task::Value(Some(Value::Object(_))) => out.push_str("[object Object]"),
        }
    }
    out
}

/// `String(number)`（ECMAScript `Number::toString`）：与 [`format_js_number`] 的区别只在
/// 非有限值——JSON 里写 `null`，而 `String(NaN)` 是 `"NaN"`、`String(Infinity)` 是 `"Infinity"`。
///
/// `-0.0` 走 [`format_js_number`]，输出 `"0"`，与 `String(-0) === "0"` 一致。
/// 本函数是私有的；公开入口 [`js_string`] 负责先从 `Value` 取数。
fn js_number_to_string(value: f64) -> String {
    if value.is_nan() {
        "NaN".to_string()
    } else if value == f64::INFINITY {
        "Infinity".to_string()
    } else if value == f64::NEG_INFINITY {
        "-Infinity".to_string()
    } else {
        format_js_number(value)
    }
}

/// truthiness：`false`、`0`、`-0`、`NaN`、`""`、`null`、`undefined` 为假；对象/数组/函数为真。
///
/// # 规则
/// - `None`（`undefined`）与 `null`：假；
/// - 布尔取自身；数字 `0`/`-0`/`NaN` 为假，其余为真（`as_f64()` 失败按 `NaN`）；
/// - 空字符串为假，非空为真；
/// - 数组与对象**恒为真**，包括 `[]` 与 `{}`（JS 里空容器是对象，不是假值）。
///
/// # 偏差
/// 解析溢出被存成 `Value::Null` 的数（`js_parse` 的 `1e400`）在这里是假，而 TS 的
/// `Boolean(Infinity)` 为真；这是模块文档列出的被迫偏差之一。
pub fn is_truthy(value: Option<&Value>) -> bool {
    match value {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => {
            let f = n.as_f64().unwrap_or(f64::NAN);
            f != 0.0 && !f.is_nan()
        }
        Some(Value::String(s)) => !s.is_empty(),
        Some(Value::Array(_)) | Some(Value::Object(_)) => true,
    }
}

/// 严格相等（`===`）。JSON 值之间没有函数与 `undefined`；`null` 只等于 `null`。
///
/// # 规则
/// 类型必须相同且值相等：`1` 与 `1.0` 都走数字分支，按 `f64` 比较（相等）；`NaN` 与
/// 任何值都不相等；`-0` 与 `0` 相等。
///
/// # 对象与数组
/// 恒为 `false`：JSON 值没有引用身份，同一个 `Value` 的两个副本在 JS 里也是两个不同
/// 对象（`{} === {}` 为假）。需要按内容比较时用 [`crate::js_object::deep_equal`]；
/// 本函数与它的数字语义相同、容器语义不同，不要混用。
pub fn js_strict_equal(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Null, Value::Null) => true,
        (Value::Bool(x), Value::Bool(y)) => x == y,
        (Value::Number(x), Value::Number(y)) => {
            let fx = x.as_f64().unwrap_or(f64::NAN);
            let fy = y.as_f64().unwrap_or(f64::NAN);
            // NaN !== NaN
            fx == fy && !fx.is_nan()
        }
        (Value::String(x), Value::String(y)) => x == y,
        // 对象/数组按引用比较；JSON 值没有身份，调用方应使用 deep_equal。
        (Value::Array(_), Value::Array(_)) | (Value::Object(_), Value::Object(_)) => false,
        _ => false,
    }
}

/// `String.prototype.trim()`（ECMAScript 的 `WhiteSpace` + `LineTerminator`）。
///
/// 与 Rust 的 `str::trim()` 不同：JS 去掉 U+FEFF（BOM/ZWNBSP）、U+00A0、U+1680、U+2000–200A、
/// U+202F、U+205F、U+3000；**不去** U+0085（NEL）、U+180E、U+200B、U+001C–001F。
///
/// 需要它的调用方都是「判断输入是否为空 / 取错误消息里的原文」这类 JS 语义点：
/// `parseToolCallArguments` 的 `!json?.trim()`（`kiana-ai`）、`parseStreamingJson` 的空输入短路
/// （本 crate 的 [`crate::js_parse::parse_streaming`] 已内置）。
///
/// # 与 `js_num` 的耦合
/// 字符集由 `js_num::is_js_whitespace` 定义（`parse_js_number` 也用它）。两者必须
/// 保持同一份定义，否则 `Number(" 1 ")` 与 `String.prototype.trim()` 会出现不一致；
/// 修改时两边的测试都要重跑。
pub fn js_trim(text: &str) -> &str {
    crate::js_num::trim_js_whitespace(text)
}

/// `JSON.stringify(value)` 里字符串的字面量转义（`JSON.stringify("…")` 的内层）。
///
/// 与 Rust 的 `escape_default` 不同，JS 只转义 `"`、`\`、控制字符，且用 `\uXXXX` 表示
/// 其余控制字符；`U+2028`/`U+2029` 在 JSON 字符串里不转义（只有 `JSON.stringify` 之外的
/// JS 源码字面量才需要）。
///
/// # 输出
/// 返回带首尾双引号的字符串字面量，可直接拼进 JSON 文本；
/// [`crate::js_object::stringify`] 的对象键与字符串值都走这里。
///
/// # 转义表
///
/// | 字符 | 输出 |
/// |---|---|
/// | `"` / `\` | `\"` / `\\` |
/// | U+0008 / U+000C / U+000A / U+000D / U+0009 | `\b` / `\f` / `\n` / `\r` / `\t` |
/// | 其余 U+0000–U+001F | `\u00xx`（四位小写十六进制） |
/// | 其他（含 `U+2028`/`U+2029`、非 ASCII） | 原样 |
///
/// # 边界
/// 控制字符只按 code point 判断 `< 0x20`；DEL（U+007F）与 C1 控制字符不转义，这与
/// `JSON.stringify` 一致。Rust 的 `String` 不可能包含孤立代理项，因此不需要处理
/// `\udXXX` 的成对转义。
pub fn js_quote(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for ch in text.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\u{08}' => out.push_str("\\b"),
            '\u{0c}' => out.push_str("\\f"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                out.push_str(&format!("\\u{:04x}", c as u32));
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn string_conversion_matches_javascript() {
        assert_eq!(js_string(None), "undefined");
        assert_eq!(js_string(Some(&Value::Null)), "null");
        assert_eq!(
            js_string(Some(&serde_json::json!([1, "a", null, true]))),
            "1,a,,true"
        );
        assert_eq!(
            js_string(Some(&serde_json::json!({"a": 1}))),
            "[object Object]"
        );
        assert!(!is_truthy(Some(&serde_json::json!(0))));
        assert!(!is_truthy(Some(&serde_json::json!(""))));
        assert!(is_truthy(Some(&serde_json::json!([]))));
    }

    #[test]
    fn trim_matches_javascript() {
        // ECMAScript 的 WhiteSpace + LineTerminator（`String.prototype.trim`）。
        for ws in [
            "\u{0009}", "\u{000a}", "\u{000b}", "\u{000c}", "\u{000d}", "\u{0020}", "\u{00a0}",
            "\u{1680}", "\u{2000}", "\u{2007}", "\u{200a}", "\u{2028}", "\u{2029}", "\u{202f}",
            "\u{205f}", "\u{3000}", "\u{feff}",
        ] {
            let text = format!("{ws}{ws} 12 {ws}");
            assert_eq!(
                js_trim(&text),
                "12",
                "whitespace U+{:04X}",
                ws.chars().next().unwrap() as u32
            );
            assert_eq!(
                js_trim(ws),
                "",
                "whitespace U+{:04X}",
                ws.chars().next().unwrap() as u32
            );
        }
        // 不在 JS trim 集合里：U+0085（NEL）、U+180E、U+200B、U+001C–U+001F。
        for not_ws in ["\u{0085}", "\u{180e}", "\u{200b}", "\u{001c}", "\u{001f}"] {
            assert_eq!(
                js_trim(not_ws),
                not_ws,
                "U+{:04X} is not trimmed",
                not_ws.chars().next().unwrap() as u32
            );
        }
        assert_eq!(js_trim(""), "");
        assert_eq!(js_trim("abc"), "abc");
    }

    #[test]
    fn quoting_matches_json_stringify() {
        assert_eq!(js_quote("a\"b\\c\n"), r#""a\"b\\c\n""#);
        assert_eq!(js_quote("\u{1}"), r#""\u0001""#);
    }
}
