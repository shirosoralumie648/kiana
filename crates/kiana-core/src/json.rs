//! utils/json.ts。
//!
//! TS: `packages/coding-agent/src/utils/json.ts`（6 行）。
//!
//! ```ts
//! export function stripJsonComments(input: string): string {
//!   return input
//!     .replace(/"(?:\\.|[^"\\])*"|\/\/[^\n]*/g, (m) => (m[0] === '"' ? m : ""))
//!     .replace(/"(?:\\.|[^"\\])*"|,(\s*[}\]])/g, (m, tail) => tail ?? (m[0] === '"' ? m : ""));
//! }
//! ```
//!
//! 两遍替换，字符串字面量原样保留：
//!
//! 1. 去掉 `//` 行注释（**不处理** `/* ... */`）；
//! 2. 去掉对象/数组尾部的逗号（`,(\s*[}\]])` → 保留捕获的 `tail`）。
//!
//! 正则方言差异（`spec/ca-cli.md` §2.21.3 / §8.2 第 8 条）：
//!
//! - JS 的 `\\.` 中 `.` 不匹配行终止符（`\n`、`\r`、`\u2028`、`\u2029`），因此写成
//!   `\\(?:[^\n\r\u{2028}\u{2029}])`；
//! - JS 的 `\s` 含 `\uFEFF` 与 `\u00A0` 等，Rust `regex` 的 Unicode `\s` 不含 `\uFEFF`，
//!   因此显式展开成 `JS_WHITESPACE_CLASS`。
//!
//! `stripBom` 属于 foundation 的 [`crate::text`]，不在本模块。
//!
//! ## 执行流程与约束
//!
//! 1. 第一遍用 `LINE_COMMENT_RE` 匹配“字符串字面量 | `//` 到行尾”，回调只保留字符串、
//!    删除注释。字符串分支排在前面，所以 `"http://x"` 里的 `//` 不会被误删。
//! 2. 第二遍用 `TRAILING_COMMA_RE` 匹配“字符串字面量 | 逗号 + JS 空白 + `}`/`]`”，
//!    尾逗号分支只保留捕获组（右括号及其前面的空白），逗号本身被丢弃。
//!
//! 两遍的顺序不能交换：先删注释，`,` 与右括号之间才可能只剩空白，尾逗号规则才能命中
//! `{"a": 1, // note\n}` 这种写法。
//!
//! 本函数只做文本替换，不解析也不校验 JSON；输入按合法 JSONC 对待，格式错误时的结果
//! 不构成契约。返回新分配的 `String`（内部 `Cow` 最后 `into_owned`）。直接调用方是
//! `kiana-config` 的模型配置解析（`models/config_file.rs` 在 `js_parse` 前调用）；行为由
//! `crates/kiana-core/tests/json.rs` 的黄金语料锁定。

use std::sync::LazyLock;

use regex::{Captures, Regex};

/// JS `\s` 的精确字符集（ECMA-262 WhiteSpace + LineTerminator）。
const JS_WHITESPACE_CLASS: &str = r"[ \t\n\x0B\f\r\u{00A0}\u{1680}\u{2000}-\u{200A}\u{2028}\u{2029}\u{202F}\u{205F}\u{3000}\u{FEFF}]";

/// JS `\\.`（反斜杠 + 非行终止符）。
const STRING_BODY: &str = r#"\\(?:[^\n\r\u{2028}\u{2029}])|[^"\\]"#;

/// 第 1 遍：字符串字面量 | `//` 行注释。
static LINE_COMMENT_RE: LazyLock<Regex> = LazyLock::new(|| {
    let pattern = format!("\"(?:{STRING_BODY})*\"|//[^\n]*", STRING_BODY = STRING_BODY);
    Regex::new(&pattern).expect("valid stripJsonComments first-pass regex")
});

/// 第 2 遍：字符串字面量 | `,` + 空白 + `}`/`]`（捕获 tail）。
static TRAILING_COMMA_RE: LazyLock<Regex> = LazyLock::new(|| {
    let pattern = format!(
        "\"(?:{STRING_BODY})*\"|,({JS_WHITESPACE_CLASS}*[}}\\]])",
        STRING_BODY = STRING_BODY,
        JS_WHITESPACE_CLASS = JS_WHITESPACE_CLASS
    );
    Regex::new(&pattern).expect("valid stripJsonComments second-pass regex")
});

/// TS: `stripJsonComments(input)` —— 去掉 `//` 行注释与尾随逗号，字符串字面量原样保留。
///
/// 输入 `&str`，返回处理后的新 `String`；不修改入参、没有错误返回值，也不做任何 JSON
/// 语法校验（是否为合法 JSON 由调用方的解析器决定）。只删除 `//` 行注释，**不处理**
/// `/* ... */` 块注释；引号内的 `//`、`,]` 等文本保持原样。对合法 JSONC 输入，
/// 两遍替换都不会改变字符串字面量的内容，便于随后的 `js_parse` 按 JSON 语义解析。
pub fn strip_json_comments(input: &str) -> String {
    // 第一遍：字符串字面量原样输出，`//` 注释替换为空串。
    let without_comments = LINE_COMMENT_RE.replace_all(input, |caps: &Captures<'_>| {
        let matched = caps
            .get(0)
            .expect("the whole match is always present")
            .as_str();
        if matched.starts_with('"') {
            matched.to_string()
        } else {
            String::new()
        }
    });

    // 第二遍：字符串分支没有捕获组，原样返回整段匹配；尾逗号分支有捕获组 1（空白 + 右括号），
    // 只输出该捕获组，从而丢掉逗号本身。
    TRAILING_COMMA_RE
        .replace_all(&without_comments, |caps: &Captures<'_>| match caps.get(1) {
            Some(tail) => tail.as_str().to_string(),
            None => caps
                .get(0)
                .expect("the whole match is always present")
                .as_str()
                .to_string(),
        })
        .into_owned()
}
