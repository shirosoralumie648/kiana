//! source.ts：codemode 源格式（可选的一行 `// @options:` 指令 + JavaScript）。
//!
//! TS 源：`packages/codemode/src/source.ts`（100 行）。逐字移植，包括错误消息。
//!
//! ```js
//! // @options: {"max_output_tokens": 2000, "timeout_ms": 30000}
//! const text = await tools.read({ path: "package.json" });
//! text(JSON.parse(text).name);
//! ```
//!
//! # JS 语义落点
//!
//! - `input.trim() === ""` 与 `code.trim() === ""` 用 **JS 的空白集合**（`js_trim`：0009-000D、
//!   0020、00A0、1680、2000-200A、2028、2029、202F、205F、3000、FEFF），不是 Rust 的
//!   `char::is_whitespace`（后者缺 FEFF、多 0085）。
//! - `trimStart()` 同理（`js_trim_start`）。
//! - `input.split("\n")` 只按 LF 切；`firstLine.replace(/\r$/, "")` 只去掉行尾**一个** CR。
//! - `input.slice(newline)` 从换行符开始截取，行号因此不变。
//! - `JSON.parse` 的错误消息用 `kiana_json::parse`（V8 的 `SyntaxError.message` 逐字），
//!   与 TS 的 `error.message` 相同。
//! - `Number.isSafeInteger` 且 `>= 0`：[`kiana_json::js_num::is_js_integer`] + 安全整数上界
//!   `9007199254740991`。JSON 文本 `1.0` 解析成 `1.0`（f64），`Number.isSafeInteger(1.0)`
//!   为 `true`，所以必须用 `f64` 判定（`1.0_f64.fract() == 0.0`）而不是“文本里有小数点”。
//!   本文件的 `is_safe_integer` 把这些条件（有限、整数、安全范围内且非负）写在一起，
//!   没有调用 `is_js_integer`。
//!
//! # 调用方
//!
//! - `kiana-durable` 的 codemode 执行路径先调用 [`parse_codemode_source`] 拆出脚本与
//!   `max_output_tokens`/`timeout_ms`，并把 [`CODEMODE_SOURCE_GRAMMAR`] 作为语法约束交给
//!   provider；
//! - `kiana-builtin-extensions` 的 codemode 工具把用户传入的 `code` 交给同一个解析器；
//! - 本 crate 的 `index` 再导出解析器与两个公开常量；`portable` 不导出 source 面
//!   （见 `portable` 模块文档的差异清单）。
//!
//! # 解析顺序（与 TS 逐条对应）
//!
//! 1. 用 JS 空白集合判断整体是否为空；空输入直接报错，不返回空脚本。
//! 2. **先**扫描第 2 行起的每一行：`trimStart()` 后以 `// @options:` 开头即报错。这一步先于
//!    首行解析，所以“首行是 options 行、后面还有一行 options”报的是后面那一行的行号。
//! 3. 去掉首行末尾**一个** CR 后判断前缀；没有前缀时原样返回整个输入（CRLF/BOM 保留）。
//! 4. 有前缀时从第一个 LF 开始截取 `code`（保留 LF）：行号不变；CRLF 的 CR 留在 options
//!    行里，不进入脚本。
//! 5. options 行之后必须还有非空白的代码，否则报错（`// @options: {...}` 单独一行是错误）。
//! 6. 解析 options：空指令 → JSON 解析 → 必须是对象 → 未知键优先 → 两个字段按固定顺序检查。

use serde_json::Value;

/// `CODEMODE_OPTIONS_PREFIX`。
///
/// 匹配方式是 `starts_with`（作用于首行 `trimStart()` 之后）：`// @optionsx {}` 不匹配，
/// 冒号之后的文本全部交给 options 解析。
pub const CODEMODE_OPTIONS_PREFIX: &str = "// @options:";

/// `SUPPORTED_FIELDS`（顺序即消息里的顺序）。
const SUPPORTED_FIELDS: [&str; 2] = ["max_output_tokens", "timeout_ms"];

/// `SUPPORTED_FIELDS_TEXT`。
const SUPPORTED_FIELDS_TEXT: &str = "`max_output_tokens` and `timeout_ms`";

/// `MAX_TIMEOUT_MS`：`setTimeout` 支持的最大延迟，`timeout_ms` 的上限。
const MAX_TIMEOUT_MS: f64 = 2_147_483_647.0;

/// `CODEMODE_SOURCE_GRAMMAR`：给 provider 做语法约束的 Lark 语法。
///
/// 只固定 options 行的形状；options JSON 与代码由 [`parse_codemode_source`] 检查。
/// Rust 侧只逐字保留文本（不解析它）。注意首字符是换行、缩进为 TAB、行尾为 `\n`。
///
/// `OPTIONS_LINE` 允许行首空格/TAB，`NEWLINE` 允许 CRLF，`SOURCE` 要求至少一个字符；
/// 这些规则只约束形状，不保证 options 的语义合法。
pub const CODEMODE_SOURCE_GRAMMAR: &str = "\nstart: options_source | plain_source\noptions_source: OPTIONS_LINE NEWLINE SOURCE\nplain_source: SOURCE\n\nOPTIONS_LINE: /[ \\t]*\\/\\/ @options:[^\\r\\n]*/\nNEWLINE: /\\r?\\n/\nSOURCE: /[\\s\\S]+/\n";

/// `CodemodeSourceOptions`：从 options 行解析出的选项。
///
/// # 不变量
///
/// 结构体本身不做范围校验：负数、非整数、超过 `setTimeout` 上限的值都可以直接构造。
/// 校验只发生在 [`parse_codemode_source`] 的 options 解析里；直接构造或从 JSON 反序列化
/// 不会检查这些约束。
///
/// 序列化字段名是 camelCase（`maxOutputTokens`/`timeoutMs`），数字按 JS 的
/// `Number::toString` 输出（`1.0` → `1`），见字段上的 `serialize_with`。
#[derive(Debug, Clone, Copy, Default, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CodemodeSourceOptions {
    /// 脚本输出的 token 预算。
    ///
    /// JSON 文本按 JS 的 `Number::toString`（`1` 而不是 `1.0`）；见 `types` 模块文档。
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        serialize_with = "crate::types::serialize_optional_js_number",
        deserialize_with = "crate::types::deserialize_optional_js_number"
    )]
    pub max_output_tokens: Option<f64>,
    /// 整个脚本（含工具调用）的硬 deadline，毫秒。
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        serialize_with = "crate::types::serialize_optional_js_number",
        deserialize_with = "crate::types::deserialize_optional_js_number"
    )]
    pub timeout_ms: Option<f64>,
}

/// `ParsedCodemodeSource { code, options }`。
///
/// # 不变量
///
/// 没有 options 行时 `code` 与输入逐字节相同；有 options 行时 `code` 以 `\n` 开头（原
/// options 行变成空行），因此两边的行号一致。
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ParsedCodemodeSource {
    /// options 行被替换为空行后的脚本（行号不变）。
    pub code: String,
    /// 解析出的选项（无 options 行时为默认值）。
    pub options: CodemodeSourceOptions,
}

/// `CodemodeSourceError extends Error`（`name` 为 `"CodemodeSourceError"`）。
///
/// Rust 侧是手写错误类型（ARCHITECTURE.md §4.1）：`Display` 逐字等于 TS 的 `error.message`。
/// `Clone`/`Eq` 只比较 `message`；`name()` 恒为固定字符串，不参与比较。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodemodeSourceError {
    /// TS 的 `error.message`。
    pub message: String,
}

impl CodemodeSourceError {
    /// 用消息文本构造。
    pub fn new(message: impl Into<String>) -> Self {
        CodemodeSourceError {
            message: message.into(),
        }
    }

    /// TS 的 `error.name`。
    pub fn name(&self) -> &'static str {
        "CodemodeSourceError"
    }
}

impl std::fmt::Display for CodemodeSourceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for CodemodeSourceError {}

/// TS 源码里的局部 `isSafeInteger`：`typeof value === "number" && Number.isSafeInteger(value)
/// && value >= 0`。
///
/// 注意它**带** `>= 0`，所以 `timeout_ms: -1` 与 `max_output_tokens: -1` 都落进错误分支
/// （`timeout_ms` 的消息说 "positive"）。`-0` 满足（`-0 >= 0` 为 true），但 `@options: {}`
/// 里 `timeout_ms: -0` 随后被 `timeout_ms === 0` 拒绝——JS 的 `-0 === 0`。
fn is_safe_integer(value: f64) -> bool {
    value.is_finite()
        && value.fract() == 0.0
        && value.abs() <= 9_007_199_254_740_991.0
        && value >= 0.0
}

/// JS 的 `WhiteSpace` + `LineTerminator` 集合（ECMA-262 `String.prototype.trim` 用的那套）。
///
/// 0009-000D、0020、00A0、1680、2000-200A、2028、2029、202F、205F、3000、FEFF。
/// 注意它与 Rust 的 `char::is_whitespace` 不同：后者**不含** FEFF、但**含** 0085（U+0085
/// NEL），而 JS 两者相反。
fn is_js_whitespace(ch: char) -> bool {
    matches!(
        ch,
        '\u{0009}'
            | '\u{000A}'
            | '\u{000B}'
            | '\u{000C}'
            | '\u{000D}'
            | '\u{0020}'
            | '\u{00A0}'
            | '\u{1680}'
            | '\u{2000}'
            ..='\u{200A}'
                | '\u{2028}'
                | '\u{2029}'
                | '\u{202F}'
                | '\u{205F}'
                | '\u{3000}'
                | '\u{FEFF}'
    )
}

/// JS 的 `String.prototype.trimStart`（JS 空白集合）。
fn js_trim_start(text: &str) -> &str {
    text.trim_start_matches(is_js_whitespace)
}

/// JS 的 `String.prototype.trim`（JS 空白集合）。
fn js_trim(text: &str) -> &str {
    text.trim_matches(is_js_whitespace)
}

/// `error instanceof Error ? error.message : String(error)`——只用于 `JSON.parse` 的错误。
fn json_error_message(error: &kiana_json::ParseError) -> &str {
    &error.message
}

/// `parseOptions(directive)`。
fn parse_options(directive: &str) -> Result<CodemodeSourceOptions, CodemodeSourceError> {
    let object_message = || {
        CodemodeSourceError::new(format!(
            "@options must be a JSON object with supported fields {SUPPORTED_FIELDS_TEXT}"
        ))
    };
    if directive.is_empty() {
        // 空指令（冒号后什么都没有）在 TS 里先于 JSON.parse 报对象错误；不能把它交给
        // 解析器，否则会得到“JSON 语法错误”这条不同的消息。
        return Err(object_message());
    }
    let value: Value = kiana_json::parse(directive).map_err(|error| {
        CodemodeSourceError::new(format!(
            "@options must be valid JSON with supported fields {SUPPORTED_FIELDS_TEXT}: {}",
            json_error_message(&error)
        ))
    })?;
    let Value::Object(fields) = &value else {
        // 非对象、null、数组 → 同一条对象错误。
        return Err(object_message());
    };
    // 未知键检查先于字段值检查：`{"max_output_tokens": 1.5, "yield": 1}` 报的是 `yield`
    // （测试 `unknown_key_check_precedes_field_checks`）。多个未知键时只报迭代到的第一个。
    for key in fields.keys() {
        if !SUPPORTED_FIELDS.contains(&key.as_str()) {
            return Err(CodemodeSourceError::new(format!(
                "@options only supports {SUPPORTED_FIELDS_TEXT}; got `{key}`"
            )));
        }
    }
    let mut options = CodemodeSourceOptions::default();
    // 解构 + if 的顺序：`max_output_tokens` 先于 `timeout_ms`，与 JSON 键序无关。
    if let Some(raw) = fields.get("max_output_tokens") {
        let number = raw.as_f64().filter(|number| is_safe_integer(*number));
        match number {
            Some(number) => options.max_output_tokens = Some(number),
            None => {
                return Err(CodemodeSourceError::new(
                    "@options field `max_output_tokens` must be a non-negative safe integer",
                ));
            }
        }
    }
    // timeout_ms 在 is_safe_integer 之上再要求非零且不超过 setTimeout 的上限（`-0` 也因
    // `-0 === 0` 被拒绝）；错误文案与其他字段不同，含上界数字。
    if let Some(raw) = fields.get("timeout_ms") {
        let valid = raw
            .as_f64()
            .filter(|number| is_safe_integer(*number))
            .filter(|number| *number != 0.0 && *number <= MAX_TIMEOUT_MS);
        match valid {
            Some(number) => options.timeout_ms = Some(number),
            None => {
                return Err(CodemodeSourceError::new(format!(
                    "@options field `timeout_ms` must be a positive integer up to {}",
                    format_js_integer(MAX_TIMEOUT_MS)
                )));
            }
        }
    }
    Ok(options)
}

/// `2_147_483_647` 的 JS `String(number)` 形式（插入消息文本）。
fn format_js_integer(value: f64) -> String {
    kiana_json::format_js_number(value)
}

/// `parseCodemodeSource(input)`：把可选的首页 `// @options: {...}` 与脚本分开。
///
/// 错误（逐字）：
/// - 空输入 → `Expected JavaScript source text (non-empty). Provide JS only, optionally with a first line \`// @options: {"max_output_tokens": 1000}\`.`
/// - options 行不在第一行 → `The // @options: line must be the first line of the input; found one on line N`
/// - options 行后没有代码 → `The @options line must be followed by JavaScript source on subsequent lines`
/// - 非法 options → 见 `parse_options`。
///
/// TS: `source.ts#parseCodemodeSource`。
pub fn parse_codemode_source(input: &str) -> Result<ParsedCodemodeSource, CodemodeSourceError> {
    // 用 JS 空白集合判空：BOM/NBSP/U+2028 在 JS 里都算空白，而 Rust 的
    // `char::is_whitespace` 不含 FEFF、却含 U+0085。
    if js_trim(input).is_empty() {
        return Err(CodemodeSourceError::new(
            "Expected JavaScript source text (non-empty). Provide JS only, optionally with a first line `// @options: {\"max_output_tokens\": 1000}`.",
        ));
    }
    let lines: Vec<&str> = input.split('\n').collect();
    // 先查所有非首行的 options 行，再解析首行：否则第 2 行起的 options 行会被静默忽略，
    // 其中的 timeout 永远不生效（#10631）。报错行号是 1 基的。
    if let Some(index) = lines
        .iter()
        .enumerate()
        .find(|(index, line)| {
            *index > 0 && js_trim_start(line).starts_with(CODEMODE_OPTIONS_PREFIX)
        })
        .map(|(index, _)| index)
    {
        return Err(CodemodeSourceError::new(format!(
            "The {CODEMODE_OPTIONS_PREFIX} line must be the first line of the input; found one on line {}",
            index + 1
        )));
    }
    let newline = input.find('\n');
    // 前缀判断只看首行；只去掉行尾一个 CR（CRLF 的首行）。没有 options 行时返回原输入，
    // 不重新拼接，因此 BOM/CRLF 逐字节保留。
    let first_line = lines[0].strip_suffix('\r').unwrap_or(lines[0]);
    let trimmed = js_trim_start(first_line);
    let Some(rest) = trimmed.strip_prefix(CODEMODE_OPTIONS_PREFIX) else {
        return Ok(ParsedCodemodeSource {
            code: input.to_string(),
            options: CodemodeSourceOptions::default(),
        });
    };
    // 从 LF 起截取原始切片：options 行变成空行，后续行号不变；CRLF 的 CR 留在 options
    // 行一侧，不进入 code。
    let code = match newline {
        None => String::new(),
        // 从换行符开始：保留该换行，行号不变。
        Some(index) => input[index..].to_string(),
    };
    // options 行之后必须还有非空白的代码：只有 options 行（或后面全是 JS 空白）是错误，
    // 不能静默产出空脚本。
    if js_trim(&code).is_empty() {
        return Err(CodemodeSourceError::new(
            "The @options line must be followed by JavaScript source on subsequent lines",
        ));
    }
    Ok(ParsedCodemodeSource {
        code,
        // 冒号之后是 JSON 文本；`js_trim` 与 TS 的 `.trim()` 同为 JS 空白集合。
        options: parse_options(js_trim(rest))?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn options(max_output_tokens: Option<f64>, timeout_ms: Option<f64>) -> CodemodeSourceOptions {
        CodemodeSourceOptions {
            max_output_tokens,
            timeout_ms,
        }
    }

    #[test]
    fn returns_plain_code_unchanged() {
        assert_eq!(
            parse_codemode_source("text('hi')").unwrap(),
            ParsedCodemodeSource {
                code: "text('hi')".into(),
                options: options(None, None),
            }
        );
        assert_eq!(
            parse_codemode_source("// just a comment\nreturn 1").unwrap(),
            ParsedCodemodeSource {
                code: "// just a comment\nreturn 1".into(),
                options: options(None, None),
            }
        );
    }

    #[test]
    fn parses_the_options_line_and_keeps_line_numbers() {
        assert_eq!(
            parse_codemode_source("// @options: {\"timeout_ms\": 10}\nconst a = 1;\ntext(a)")
                .unwrap(),
            ParsedCodemodeSource {
                code: "\nconst a = 1;\ntext(a)".into(),
                options: options(None, Some(10.0)),
            }
        );
        assert_eq!(
            parse_codemode_source(
                "  // @options:{\"max_output_tokens\":0,\"timeout_ms\":1500}\r\ntext(1)"
            )
            .unwrap()
            .options,
            options(Some(0.0), Some(1500.0))
        );
        assert_eq!(
            parse_codemode_source("// @options: {}\ntext(1)").unwrap(),
            ParsedCodemodeSource {
                code: "\ntext(1)".into(),
                options: options(None, None),
            }
        );
    }

    #[test]
    fn only_treats_the_first_line_as_an_options_line() {
        assert_eq!(
            parse_codemode_source("// @optionsx {}\ntext(1)")
                .unwrap()
                .options,
            options(None, None)
        );
        assert_eq!(
            parse_codemode_source("text(\"// @options: {}\")")
                .unwrap()
                .options,
            options(None, None)
        );
    }

    // #10631: an options line after the first line used to be ignored, so its timeout never applied.
    #[test]
    fn rejects_an_options_line_that_is_not_the_first_line() {
        let cases: [(&str, usize); 4] = [
            (
                "// count things\n// @options: {\"timeout_ms\": 1000}\nwhile (true) {}",
                2,
            ),
            (
                "\n  // @options: {\"timeout_ms\": 1000}\nwhile (true) {}",
                2,
            ),
            ("text(1)\r\n// @options: {\"timeout_ms\": 1}", 2),
            (
                "// @options: {\"timeout_ms\": 1}\ntext(1)\n// @options: {\"timeout_ms\": 2}",
                3,
            ),
        ];
        for (input, line) in cases {
            let error = parse_codemode_source(input).expect_err(input);
            assert!(
                error.message.starts_with(
                    "The // @options: line must be the first line of the input; found one on line"
                ),
                "{input}: {}",
                error.message
            );
            assert_eq!(
                error.message,
                format!(
                    "The // @options: line must be the first line of the input; found one on line {line}"
                )
            );
        }
    }

    #[test]
    fn rejects_empty_input_and_invalid_options() {
        type Case = (&'static str, &'static str);
        let cases: [Case; 10] = [
            ("", "Expected JavaScript source text (non-empty)"),
            ("  \n", "Expected JavaScript source text (non-empty)"),
            (
                "// @options:\ntext(1)",
                "@options must be a JSON object with supported fields",
            ),
            (
                "// @options: {timeout_ms: 1}\ntext(1)",
                "@options must be valid JSON with supported fields",
            ),
            (
                "// @options: [1]\ntext(1)",
                "@options must be a JSON object with supported fields",
            ),
            (
                "// @options: {\"yield\": 1}\ntext(1)",
                "@options only supports `max_output_tokens` and `timeout_ms`; got `yield`",
            ),
            (
                "// @options: {\"max_output_tokens\": 1.5}\ntext(1)",
                "@options field `max_output_tokens` must be a non-negative safe integer",
            ),
            (
                "// @options: {\"timeout_ms\": 0}\ntext(1)",
                "@options field `timeout_ms` must be a positive integer",
            ),
            (
                "// @options: {\"timeout_ms\": 1}",
                "The @options line must be followed by JavaScript source on subsequent lines",
            ),
            (
                "// @options: {\"timeout_ms\": 1}\n  \n",
                "The @options line must be followed by JavaScript source on subsequent lines",
            ),
        ];
        for (input, message) in cases {
            let error = parse_codemode_source(input).expect_err(input);
            assert!(
                error.message.contains(message),
                "{input:?}: expected {message:?} in {:?}",
                error.message
            );
        }
    }

    #[test]
    fn empty_input_message_is_verbatim() {
        let error = parse_codemode_source("").unwrap_err();
        assert_eq!(
            error.message,
            "Expected JavaScript source text (non-empty). Provide JS only, optionally with a first line `// @options: {\"max_output_tokens\": 1000}`."
        );
        assert_eq!(error.name(), "CodemodeSourceError");
        assert_eq!(error.to_string(), error.message);
    }

    #[test]
    fn json_parse_message_is_verbatim_v8_text() {
        let error = parse_codemode_source("// @options: {timeout_ms: 1}\ntext(1)").unwrap_err();
        assert_eq!(
            error.message,
            "@options must be valid JSON with supported fields `max_output_tokens` and `timeout_ms`: \
             Expected property name or '}' in JSON at position 1 (line 1 column 2)"
        );
    }

    #[test]
    fn timeout_upper_bound_message_is_verbatim() {
        let error = parse_codemode_source("// @options: {\"timeout_ms\": 2147483648}\ntext(1)")
            .unwrap_err();
        assert_eq!(
            error.message,
            "@options field `timeout_ms` must be a positive integer up to 2147483647"
        );
        assert!(
            parse_codemode_source("// @options: {\"timeout_ms\": 2147483647}\ntext(1)").is_ok()
        );
    }

    #[test]
    fn accepts_integral_json_numbers() {
        // JSON 的 `1.0` 在 JS 里是 `1`，`Number.isSafeInteger` 为 true。
        let parsed = parse_codemode_source(
            "// @options: {\"timeout_ms\": 1.0, \"max_output_tokens\": 1e3}\ntext(1)",
        )
        .unwrap();
        assert_eq!(parsed.options, options(Some(1000.0), Some(1.0)));
        // 超出安全整数范围。
        let error = parse_codemode_source(
            "// @options: {\"max_output_tokens\": 9007199254740992}\ntext(1)",
        )
        .unwrap_err();
        assert_eq!(
            error.message,
            "@options field `max_output_tokens` must be a non-negative safe integer"
        );
        // 负数。
        let error =
            parse_codemode_source("// @options: {\"max_output_tokens\": -1}\ntext(1)").unwrap_err();
        assert_eq!(
            error.message,
            "@options field `max_output_tokens` must be a non-negative safe integer"
        );
    }

    #[test]
    fn unknown_key_check_precedes_field_checks() {
        // `yield` 在前 → 先报未知键（与键序无关的是字段检查顺序，不是未知键与字段检查之间）。
        let error = parse_codemode_source(
            "// @options: {\"max_output_tokens\": 1.5, \"yield\": 1}\ntext(1)",
        )
        .unwrap_err();
        assert_eq!(
            error.message,
            "@options only supports `max_output_tokens` and `timeout_ms`; got `yield`"
        );
    }

    #[test]
    fn only_the_first_unknown_key_is_reported() {
        let error =
            parse_codemode_source("// @options: {\"a\": 1, \"b\": 2}\ntext(1)").unwrap_err();
        assert_eq!(
            error.message,
            "@options only supports `max_output_tokens` and `timeout_ms`; got `a`"
        );
    }

    #[test]
    fn null_values_are_treated_as_absent() {
        // TS: `fields.max_output_tokens !== undefined`；JSON 的 null 是值而不是 undefined，
        // 但 `null` 不是 number → 报错。这里锁定 TS 的实际行为。
        let error = parse_codemode_source("// @options: {\"max_output_tokens\": null}\ntext(1)")
            .unwrap_err();
        assert_eq!(
            error.message,
            "@options field `max_output_tokens` must be a non-negative safe integer"
        );
    }

    #[test]
    fn trims_js_whitespace_including_bom_and_nbsp() {
        // FEFF、NBSP 在 JS 里是空白；Rust 的 is_whitespace 不把 FEFF 当空白。
        assert!(parse_codemode_source("\u{feff}").is_err());
        assert!(parse_codemode_source("\u{00a0}\n").is_err());
        assert!(parse_codemode_source("\u{2028}").is_err());
        // 0085（NEL）**不是** JS 空白 → 非空输入 → 原样返回。
        assert_eq!(parse_codemode_source("\u{0085}").unwrap().code, "\u{0085}");
    }

    #[test]
    fn options_line_may_be_indented_and_use_crlf() {
        let parsed =
            parse_codemode_source("\t // @options: {\"timeout_ms\": 5}\r\ntext(1)").unwrap();
        // `input.indexOf("\n")` 在 CRLF 里指向 LF，因此切片丢掉了前导 CR（与 TS 相同）。
        assert_eq!(parsed.code, "\ntext(1)");
        assert_eq!(parsed.options, options(None, Some(5.0)));
    }

    #[test]
    fn grammar_text_is_verbatim() {
        let expected = "\nstart: options_source | plain_source\noptions_source: OPTIONS_LINE NEWLINE SOURCE\nplain_source: SOURCE\n\nOPTIONS_LINE: /[ \\t]*\\/\\/ @options:[^\\r\\n]*/\nNEWLINE: /\\r?\\n/\nSOURCE: /[\\s\\S]+/\n";
        assert_eq!(CODEMODE_SOURCE_GRAMMAR, expected);
        assert_eq!(CODEMODE_SOURCE_GRAMMAR.len(), 184);
        assert_eq!(CODEMODE_OPTIONS_PREFIX, "// @options:");
    }
}
