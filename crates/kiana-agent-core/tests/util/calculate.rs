//! `packages/agent/test/utils/calculate.ts`：测试用计算器工具。
//!
//! TS 用 `new Function(\`return ${expression}\`)()` 交给 JS 引擎求值；Rust 侧没有 JS 引擎
//! （测试也不应引入），因此实现一个**只覆盖数值表达式**的递归下降求值器：
//! 十进制/十六/二/八进制数字字面量（含非严格模式的 legacy octal：`010` → 8、`08` → 8）、
//! 数字分隔符（`1_000`）、`Infinity`/`NaN` 标识符、`+ - * / % **`、一元 `+/-`、括号。
//! 求值结果是 `f64`，文本按 JS `String(number)` 规则格式化（`1/3` → `"0.3333333333333333"`、
//! `1/0` → `"Infinity"`、`-0` → `"0"`）。
//!
//! 语法层同样按 ECMAScript 的（非严格模式）文法判错，关键边界与 V8 一致：
//! - 一元运算符不能直接修饰未加括号的 `**`（`-2 ** 2` 是 SyntaxError，`(-2) ** 2` 才是 4）；
//! - `--5`/`++5` 是前缀自增/自减（SyntaxError），`2--3`/`2++3` 是后缀自增/自减（SyntaxError），
//!   而 `2 - -3`/`2 + +3` 合法；
//! - legacy octal 不能带小数或指数（`010.5` → SyntaxError，`08.5` → 8.5）。
//!
//! 运算符语义按 ECMAScript：`**` 右结合，且用 `Number::exponentiate`（见 [`js_pow`]），
//! `1**(1/0)` 是 `NaN`（V8）而不是 IEEE `pow` 的 1。
//!
//! 空表达式（或只有空白/注释）复刻 `new Function("return ")()`：求值结果是 `undefined`，
//! 所以 `calculate("")` 的文本是 `" = undefined"`、`calculate(" ")` 是 `"  = undefined"`。
//!
//! 与 TS 的差异（`spec/agent-codemode.md` §2.1.7 明确允许「等价的小型求值器」）：
//! - 不支持字符串/布尔/`null`/BigInt 字面量及其类型强制转换（`'a' + 'b'`、`true + 1`、
//!   `1n` 等；TS 里 `calculate("1n")` 得到 `"1n = 1"`）。
//! - 上述关键错误分支逐字复刻 V8 的 `SyntaxError` 消息；其余（如 `1..2`、`1e_3`）
//!   只保证「同样抛错」，文本是 Rust 侧自己的解析错误（`e.message || String(e)` 的
//!   **错误传播路径**保持：消息原样从 `calculate` 抛出成 `AgentToolError.message`）。
//!
//! 文本格式逐字：`` `${expression} = ${String(result)}` ``，表达式按**原始文本**回显
//! （不重新格式化，空白也保留）。

use std::sync::Arc;

use kiana_agent_core::types::{
    AgentTool, AgentToolError, AgentToolExecute, AgentToolResult, BoxFuture, JsonValue,
};
use kiana_ai::types::Usage;
use serde_json::json;

/// `CalculateResult extends AgentToolResult<undefined>`。
///
/// `details: undefined` 在 Rust 里用 `JsonValue::Null` 表示（`AgentToolResult` 的约定）。
pub type CalculateResult = AgentToolResult<JsonValue>;

/// `calculate(expression)`：求值并按 `` `${expression} = ${result}` `` 返回。
pub fn calculate(expression: &str) -> Result<CalculateResult, AgentToolError> {
    let value = evaluate(expression).map_err(|message| AgentToolError {
        message,
        name: None,
        stack: None,
    })?;
    // `new Function("return ")()` 返回 `undefined`，模板字符串写 `undefined`。
    let result = match value {
        Some(value) => js_string_number(value),
        None => "undefined".to_string(),
    };
    Ok(CalculateResult {
        content: vec![super::text_block(format!("{expression} = {result}"))],
        details: JsonValue::Null,
        structured_content: None,
        usage: None,
        is_error: None,
        terminate: None,
    })
}

/// JS `String(number)`（含 `NaN`/`±Infinity`；有限值走 [`kiana_json::format_js_number`]）。
fn js_string_number(value: f64) -> String {
    if value.is_nan() {
        "NaN".to_string()
    } else if value == f64::INFINITY {
        "Infinity".to_string()
    } else if value == f64::NEG_INFINITY {
        "-Infinity".to_string()
    } else {
        // `format_js_number` 即 ECMAScript `Number::toString`（`-0` → `"0"`）。
        kiana_json::format_js_number(value)
    }
}

/// `base ** exponent`（ECMAScript `Number::exponentiate`，V8 的 `**` / `Math.pow`）。
///
/// IEEE 754 的 `pow` 与它在两处不同：指数是 `NaN` 时 JS 返回 `NaN`（Rust 的
/// `1f64.powf(NaN)` 返回 1），`|base| == 1` 配 `±Infinity` 指数时 JS 返回 `NaN`（Rust 返回 1）。
/// 其余边界一致（`NaN ** 0 == 1`、`0 ** -1 == Infinity`、`(-0) ** -1 == -Infinity`）。
fn js_pow(base: f64, exponent: f64) -> f64 {
    if exponent.is_nan() {
        return f64::NAN;
    }
    if exponent == 0.0 {
        return 1.0;
    }
    if base.abs() == 1.0 && exponent.is_infinite() {
        return f64::NAN;
    }
    base.powf(exponent)
}

// ---------------------------------------------------------------------------
// 表达式求值器
// ---------------------------------------------------------------------------

/// 求值失败（对应 TS 的 `throw new Error(e.message || String(e))`，消息原样上抛）。
///
/// `Ok(None)` 表示 JS 的 `undefined`：`new Function(`return ${expression}`)()` 在表达式为空
/// （或只有空白/注释）时返回 `undefined`，TS 的 `calculate("")` 因此得到 ` = undefined`。
fn evaluate(expression: &str) -> Result<Option<f64>, String> {
    if is_empty_body(expression) {
        return Ok(None);
    }
    let mut parser = Parser {
        text: expression,
        pos: 0,
    };
    parser.skip_whitespace();
    if parser.at_end() {
        return Err("Unexpected end of input".to_string());
    }
    let value = parser.parse_additive()?;
    parser.skip_whitespace();
    if !parser.at_end() {
        return Err(parser.unexpected_token());
    }
    Ok(Some(value))
}

/// `expression` 是否只由空白与注释组成（`return` 后面什么都没有 → `undefined`）。
fn is_empty_body(expression: &str) -> bool {
    let mut rest = expression;
    loop {
        rest = rest.trim_start();
        if rest.is_empty() {
            return true;
        }
        if let Some(line) = rest.strip_prefix("//") {
            rest = line.find('\n').map_or("", |index| &line[index..]);
            continue;
        }
        if let Some(block) = rest.strip_prefix("/*") {
            match block.find("*/") {
                Some(index) => rest = &block[index + 2..],
                // 未闭合的块注释是 SyntaxError，不是 `undefined`。
                None => return false,
            }
            continue;
        }
        return false;
    }
}

struct Parser<'a> {
    text: &'a str,
    pos: usize,
}

impl<'a> Parser<'a> {
    fn at_end(&self) -> bool {
        self.pos >= self.text.len()
    }

    fn rest(&self) -> &'a str {
        &self.text[self.pos..]
    }

    fn peek(&self) -> Option<char> {
        self.rest().chars().next()
    }

    fn skip_whitespace(&mut self) {
        while let Some(ch) = self.peek() {
            if ch.is_whitespace() {
                self.pos += ch.len_utf8();
            } else {
                break;
            }
        }
    }

    /// `Unexpected token '<剩余输入首字符>'`（V8 的错误文本形状类似，逐字不复刻）。
    fn unexpected_token(&self) -> String {
        match self.peek() {
            Some(ch) => format!("Unexpected token '{ch}'"),
            None => "Unexpected end of input".to_string(),
        }
    }

    /// `AdditiveExpression`：`MultiplicativeExpression (('+' | '-') MultiplicativeExpression)*`。
    fn parse_additive(&mut self) -> Result<f64, String> {
        let mut value = self.parse_multiplicative()?;
        loop {
            self.skip_whitespace();
            match self.peek() {
                Some('+') => {
                    // `1++2` 是后缀自增（对字面量非法）；`1 + +2` 才是两次一元加。
                    if self.rest().starts_with("++") {
                        return Err(
                            "Invalid left-hand side expression in postfix operation".to_string()
                        );
                    }
                    self.pos += 1;
                    value += self.parse_multiplicative()?;
                }
                Some('-') => {
                    if self.rest().starts_with("--") {
                        return Err(
                            "Invalid left-hand side expression in postfix operation".to_string()
                        );
                    }
                    self.pos += 1;
                    value -= self.parse_multiplicative()?;
                }
                _ => return Ok(value),
            }
        }
    }

    /// `MultiplicativeExpression`：`UnaryExpression (('*' | '/' | '%') UnaryExpression)*`。
    fn parse_multiplicative(&mut self) -> Result<f64, String> {
        let mut value = self.parse_unary_value()?;
        loop {
            self.skip_whitespace();
            match self.peek() {
                Some('*') => {
                    // `**` 由 `parse_power` 处理；这里只可能是乘号。
                    if self.rest().starts_with("**") {
                        return Ok(value);
                    }
                    self.pos += 1;
                    value *= self.parse_unary_value()?;
                }
                Some('/') => {
                    self.pos += 1;
                    value /= self.parse_unary_value()?;
                }
                Some('%') => {
                    self.pos += 1;
                    value %= self.parse_unary_value()?;
                }
                _ => return Ok(value),
            }
        }
    }

    fn parse_unary_value(&mut self) -> Result<f64, String> {
        Ok(self.parse_unary()?.0)
    }

    /// `UnaryExpression`：`('+' | '-') UnaryExpression | UpdateExpression`。
    ///
    /// 返回值第二项表示「该一元运算符的操作数是**没有括号的** `base ** exp`」——
    /// ECMAScript 规定这种写法是 SyntaxError（`-2 ** 2`），必须写成 `(-2) ** 2` 或
    /// `-(2 ** 2)`。
    fn parse_unary(&mut self) -> Result<(f64, bool), String> {
        self.skip_whitespace();
        let operator = match self.peek() {
            Some(operator @ ('+' | '-')) => operator,
            _ => return self.parse_power(),
        };
        // `--`/`++` 是前缀自减/自增运算符：`new Function("return --5")` 抛
        // `Invalid left-hand side expression in prefix operation`。
        let bytes = self.rest().as_bytes();
        if bytes.len() >= 2 && bytes[0] == operator as u8 && bytes[1] == operator as u8 {
            return Err("Invalid left-hand side expression in prefix operation".to_string());
        }
        self.pos += 1;
        let (value, powered) = self.parse_unary()?;
        if powered {
            return Err(
                "Unary operator used immediately before exponentiation expression. \
                 Parenthesis must be used to disambiguate operator precedence"
                    .to_string(),
            );
        }
        Ok((if operator == '-' { -value } else { value }, false))
    }

    /// `ExponentiationExpression`：`UpdateExpression ('**' ExponentiationExpression)?`
    /// （右结合）。返回值第二项见 [`Self::parse_unary`]。
    fn parse_power(&mut self) -> Result<(f64, bool), String> {
        let value = self.parse_primary()?;
        self.skip_whitespace();
        if self.rest().starts_with("**") {
            self.pos += 2;
            let exponent = self.parse_unary_value()?;
            return Ok((js_pow(value, exponent), true));
        }
        Ok((value, false))
    }

    /// `PrimaryExpression`：数字字面量、`Infinity`/`NaN` 或括号表达式。
    fn parse_primary(&mut self) -> Result<f64, String> {
        self.skip_whitespace();
        match self.peek() {
            Some('(') => {
                self.pos += 1;
                let value = self.parse_additive()?;
                self.skip_whitespace();
                if self.peek() == Some(')') {
                    self.pos += 1;
                    Ok(value)
                } else {
                    Err(self.unexpected_token())
                }
            }
            Some(ch) if ch.is_ascii_digit() || ch == '.' => self.parse_number(),
            Some(ch) if ch.is_ascii_alphabetic() || ch == '_' || ch == '$' => {
                let start = self.pos;
                while let Some(ch) = self.peek() {
                    if ch.is_ascii_alphanumeric() || ch == '_' || ch == '$' {
                        self.pos += ch.len_utf8();
                    } else {
                        break;
                    }
                }
                // JS 里 `Infinity`/`NaN` 是全局标识符（其余标识符引用会抛
                // `<name> is not defined`，与 V8 一致）。
                match &self.text[start..self.pos] {
                    "Infinity" => Ok(f64::INFINITY),
                    "NaN" => Ok(f64::NAN),
                    identifier => Err(format!("{identifier} is not defined")),
                }
            }
            Some(_) => Err(self.unexpected_token()),
            None => Err("Unexpected end of input".to_string()),
        }
    }

    /// 数字字面量：`0x`/`0b`/`0o` 前缀、十进制（含 `.5`/`1.`/指数）、数字分隔符 `_`，
    /// 以及非严格模式下的 legacy octal（`010` → 8）。
    fn parse_number(&mut self) -> Result<f64, String> {
        let start = self.pos;
        let bytes = self.rest().as_bytes();
        if bytes.len() > 2
            && bytes[0] == b'0'
            && matches!(bytes[1], b'x' | b'X' | b'b' | b'B' | b'o' | b'O')
        {
            let radix = match bytes[1] {
                b'x' | b'X' => 16,
                b'b' | b'B' => 2,
                _ => 8,
            };
            self.pos += 2;
            if !self.scan_digits(radix, false)? {
                return Err(self.unexpected_token());
            }
            let literal = self.text[start..self.pos].replace('_', "");
            return Ok(kiana_json::parse_js_number(&literal));
        }

        // `0_1`：分隔符不能跟在前导 0 之后。
        let rest = self.rest().as_bytes();
        if rest.len() > 1 && rest[0] == b'0' && rest[1] == b'_' {
            return Err("Numeric separator can not be used after leading 0.".to_string());
        }
        let integer_start = self.pos;
        let integer_digits = self.scan_digits(10, true)?;
        let integer_part = &self.text[integer_start..self.pos];
        let mut has_fraction = false;
        let mut has_exponent = false;
        if self.text.as_bytes().get(self.pos) == Some(&b'.') {
            self.pos += 1;
            self.scan_fraction()?;
            has_fraction = true;
        }
        if let Some(b'e' | b'E') = self.text.as_bytes().get(self.pos).copied() {
            let exponent_start = self.pos;
            self.pos += 1;
            if let Some(b'+' | b'-') = self.text.as_bytes().get(self.pos).copied() {
                self.pos += 1;
            }
            if !self.scan_digits(10, true)? {
                // `1e` / `1e+`：指数标记不属于数字。
                self.pos = exponent_start;
            } else {
                has_exponent = true;
            }
        }
        if !integer_digits && !has_fraction {
            return Err("Unexpected token '.'".to_string());
        }
        if integer_part.len() > 1 && integer_part.starts_with('0') && integer_part.contains('_') {
            return Err("Numeric separator can not be used after leading 0.".to_string());
        }
        let raw_literal = &self.text[start..self.pos];
        let has_separator = raw_literal.contains('_');
        let literal = raw_literal.replace('_', "");
        if !has_separator
            && integer_part.len() > 1
            && integer_part.starts_with('0')
            && integer_part.bytes().all(|byte| byte.is_ascii_digit())
        {
            let octal = integer_part
                .bytes()
                .all(|byte| (b'0'..=b'7').contains(&byte));
            if octal {
                // `LegacyOctalIntegerLiteral` 不能带小数/指数（`010.5` → SyntaxError）。
                if has_fraction || has_exponent {
                    return Err("Unexpected number".to_string());
                }
                let mut value = 0.0;
                for byte in integer_part.bytes() {
                    value = value * 8.0 + f64::from(byte - b'0');
                }
                return Ok(value);
            }
            // `08`/`019` 是 NonOctalDecimalIntegerLiteral（按十进制）。
        }
        let value = kiana_json::parse_js_number(&literal);
        if value.is_nan() {
            return Err(self.unexpected_token());
        }
        Ok(value)
    }

    /// 扫描 `<radix>` 进制数字与数字分隔符 `_`；返回是否至少读到一个数字。
    fn scan_digits(&mut self, radix: u32, allow_empty: bool) -> Result<bool, String> {
        let bytes = self.text.as_bytes();
        let mut saw_digit = false;
        let mut previous_underscore = false;
        while let Some(&byte) = bytes.get(self.pos) {
            if byte == b'_' {
                if !saw_digit {
                    return Err("Numeric separator can not be used after leading 0.".to_string());
                }
                if previous_underscore {
                    return Err("Only one underscore is allowed as numeric separator".to_string());
                }
                previous_underscore = true;
                self.pos += 1;
            } else if (byte as char).is_digit(radix) {
                saw_digit = true;
                previous_underscore = false;
                self.pos += 1;
            } else {
                break;
            }
        }
        if previous_underscore {
            return Err(
                "Numeric separators are not allowed at the end of numeric literals".to_string(),
            );
        }
        if !saw_digit && !allow_empty {
            return Err(self.unexpected_token());
        }
        Ok(saw_digit)
    }

    /// 小数点后的数字（可以为空：`1.`），分隔符规则同整数部分。
    fn scan_fraction(&mut self) -> Result<(), String> {
        let bytes = self.text.as_bytes();
        let mut saw_digit = false;
        let mut previous_underscore = false;
        while let Some(&byte) = bytes.get(self.pos) {
            if byte == b'_' {
                if !saw_digit || previous_underscore {
                    return Err("Only one underscore is allowed as numeric separator".to_string());
                }
                previous_underscore = true;
                self.pos += 1;
            } else if byte.is_ascii_digit() {
                saw_digit = true;
                previous_underscore = false;
                self.pos += 1;
            } else {
                break;
            }
        }
        if previous_underscore {
            return Err(
                "Numeric separators are not allowed at the end of numeric literals".to_string(),
            );
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// 工具
// ---------------------------------------------------------------------------

/// `calculateSchema = Type.Object({ expression: Type.String({...}) })`。
///
/// 键序与 TypeBox 的 `JSON.stringify` 输出一致（`type`、`required`、`properties`）。
pub fn calculate_parameters() -> JsonValue {
    json!({
        "type": "object",
        "required": ["expression"],
        "properties": {
            "expression": {
                "type": "string",
                "description": "The mathematical expression to evaluate"
            }
        }
    })
}

struct CalculateExecute {
    usage: Option<Usage>,
}

impl AgentToolExecute for CalculateExecute {
    fn execute<'a>(
        &'a self,
        _tool_call_id: &'a str,
        params: JsonValue,
        _signal: Option<&'a tokio_util::sync::CancellationToken>,
        _on_update: Option<kiana_agent_core::types::AgentToolUpdateCallback>,
    ) -> BoxFuture<'a, Result<AgentToolResult<JsonValue>, AgentToolError>> {
        let usage = self.usage.clone();
        Box::pin(async move {
            // TS：`calculate(args.expression)`，模板字符串对非字符串参数做 `String()`。
            let expression = match params.get("expression") {
                Some(JsonValue::String(text)) => text.clone(),
                Some(other) => kiana_json::js_string(Some(other)),
                None => "undefined".to_string(),
            };
            let mut result = calculate(&expression)?;
            result.usage = usage;
            Ok(result)
        })
    }
}

/// `calculateTool`：`label: "Calculator"`、`name: "calculate"`、
/// `description: "Evaluate mathematical expressions"`。
pub fn calculate_tool() -> AgentTool {
    AgentTool {
        name: "calculate".to_string(),
        label: "Calculator".to_string(),
        description: "Evaluate mathematical expressions".to_string(),
        parameters: calculate_parameters(),
        constrained_sampling: None,
        output_schema: None,
        prepare_arguments: None,
        execution_mode: None,
        replay: None,
        execute: Arc::new(CalculateExecute { usage: None }),
    }
}

/// `createCalculateToolWithUsage(usage)`：与 [`calculate_tool`] 相同，结果附带 `usage`。
pub fn create_calculate_tool_with_usage(usage: Usage) -> AgentTool {
    let mut tool = calculate_tool();
    tool.execute = Arc::new(CalculateExecute { usage: Some(usage) });
    tool
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text_of(result: &CalculateResult) -> String {
        match &result.content[0] {
            kiana_ai::types::UserContentBlock::Text(text) => text.text.clone(),
            other => panic!("expected text block, got {other:?}"),
        }
    }

    #[test]
    fn matches_the_typescript_golden_fixture() {
        // `tests/fixtures/calculate.json` 由 TS 参考实现的 `calculate.ts`（`new Function` +
        // 模板字符串）生成；逐条对拍文本与 JS 数字格式化。
        let fixture: Vec<serde_json::Value> =
            serde_json::from_str(include_str!("../fixtures/calculate.json")).expect("fixture");
        assert_eq!(fixture.len(), 32);
        for case in fixture {
            let expression = case["expression"].as_str().expect("expression");
            let expected = case["text"].as_str().expect("text");
            let result = calculate(expression).expect("expression evaluates");
            assert_eq!(text_of(&result), expected, "expression: {expression}");
        }
    }

    #[test]
    fn reports_a_message_for_unparseable_expressions() {
        // 错误消息只要求非空，不要求与其他实现的文本逐字相同；
        // 但同样从 `calculate` 以 Error 形式抛出（非空消息）。
        let error = calculate("1 +").expect_err("unexpected end");
        assert!(!error.message.is_empty());

        let error = calculate("2 2").expect_err("trailing token");
        assert!(!error.message.is_empty());
    }

    #[test]
    fn empty_expressions_evaluate_to_undefined_like_v8() {
        // TS：`new Function(`return ${expression}`)()` 对空白/注释体返回 `undefined`，
        // 模板字符串把它写成 `undefined`，表达式本身按原始文本回显。
        for (expression, expected) in [
            ("", " = undefined"),
            (" ", "  = undefined"),
            ("\n", "\n = undefined"),
            ("\t", "\t = undefined"),
            ("//x", "//x = undefined"),
            ("/*x*/", "/*x*/ = undefined"),
            ("/*x*/ ", "/*x*/  = undefined"),
            ("// x\n", "// x\n = undefined"),
        ] {
            let result = calculate(expression).unwrap_or_else(|error| {
                panic!("`{expression}` should evaluate: {}", error.message)
            });
            assert_eq!(text_of(&result), expected, "expression: {expression:?}");
        }
        // 未闭合的块注释仍是语法错误（`new Function("return /*")` 抛 SyntaxError）。
        assert!(calculate("/*x").is_err());
        // 注释不是整个表达式时，后面的内容照常解析（`return 1 + 2`）。
        assert!(calculate("1 + 2").is_ok());
    }

    #[test]
    fn exponentiation_follows_ecmascript_number_exponentiate() {
        // V8：`Math.pow(±1, ±Infinity)` 与任何 `NaN` 指数都是 `NaN`；`NaN ** 0` 是 1。
        for (expression, expected) in [
            ("1**(1/0)", "1**(1/0) = NaN"),
            ("(-1)**(1/0)", "(-1)**(1/0) = NaN"),
            ("1**(1/-0)", "1**(1/-0) = NaN"),
            ("1**(0/0)", "1**(0/0) = NaN"),
            ("2**(1/0)", "2**(1/0) = Infinity"),
            ("(0/0)**0", "(0/0)**0 = 1"),
            ("(0/0)**2", "(0/0)**2 = NaN"),
            ("0**-1", "0**-1 = Infinity"),
            ("(-0)**-1", "(-0)**-1 = -Infinity"),
            ("(-8)**(1/3)", "(-8)**(1/3) = NaN"),
        ] {
            let result = calculate(expression).expect("expression evaluates");
            assert_eq!(text_of(&result), expected, "expression: {expression}");
        }
    }

    #[test]
    fn matches_v8_for_literal_and_operator_edge_cases() {
        // 期望值来自 Node 26（V8）的探针：`new Function(`return ${expression}`)()`。
        // 值分支逐字对拍 `calculate` 的文本；错误分支对拍 `e.message`——TS 的
        // `calculate` 会把它原样抛成 `AgentToolError.message`。
        let values = [
            ("010", "010 = 8"),
            ("0777", "0777 = 511"),
            ("08", "08 = 8"),
            ("019", "019 = 19"),
            ("08.5", "08.5 = 8.5"),
            ("1_000", "1_000 = 1000"),
            ("1_0.5", "1_0.5 = 10.5"),
            ("0x1_f", "0x1_f = 31"),
            ("1e3_0", "1e3_0 = 1e+30"),
            ("(-2) ** 2", "(-2) ** 2 = 4"),
            ("-(2 ** 2)", "-(2 ** 2) = -4"),
            ("2 ** 3 ** 2", "2 ** 3 ** 2 = 512"),
            ("2 ** -1", "2 ** -1 = 0.5"),
            ("2 ** (-1) ** 2", "2 ** (-1) ** 2 = 2"),
            ("2 ** -1 * 3", "2 ** -1 * 3 = 1.5"),
            ("2 + +3", "2 + +3 = 5"),
            ("2 - -3", "2 - -3 = 5"),
            ("Infinity", "Infinity = Infinity"),
            ("Infinity + 1", "Infinity + 1 = Infinity"),
            ("NaN", "NaN = NaN"),
        ];
        for (expression, expected) in values {
            let result = calculate(expression).unwrap_or_else(|error| {
                panic!(
                    "`{expression}` should evaluate, got error: {}",
                    error.message
                )
            });
            assert_eq!(text_of(&result), expected, "expression: {expression}");
        }

        const UNARY_BEFORE_POWER: &str = "Unary operator used immediately before exponentiation \
             expression. Parenthesis must be used to disambiguate operator precedence";
        let errors = [
            ("-2 ** 2", UNARY_BEFORE_POWER),
            ("+2 ** 2", UNARY_BEFORE_POWER),
            ("-(-2) ** 2", UNARY_BEFORE_POWER),
            ("2 * -3 ** 2", UNARY_BEFORE_POWER),
            ("2 ** -1 ** 2", UNARY_BEFORE_POWER),
            (
                "--5",
                "Invalid left-hand side expression in prefix operation",
            ),
            (
                "++5",
                "Invalid left-hand side expression in prefix operation",
            ),
            (
                "2--3",
                "Invalid left-hand side expression in postfix operation",
            ),
            (
                "2++3",
                "Invalid left-hand side expression in postfix operation",
            ),
            ("010.5", "Unexpected number"),
            ("00.5", "Unexpected number"),
            (
                "1__0",
                "Only one underscore is allowed as numeric separator",
            ),
            (
                "1_",
                "Numeric separators are not allowed at the end of numeric literals",
            ),
            ("0_1", "Numeric separator can not be used after leading 0."),
            ("foo", "foo is not defined"),
        ];
        for (expression, expected) in errors {
            let error = calculate(expression)
                .err()
                .unwrap_or_else(|| panic!("`{expression}` should throw"));
            assert_eq!(error.message, expected, "expression: {expression}");
        }
        // 其余语法错误只要求同样抛错（消息是 Rust 侧自己的文本，见模块文档）。
        assert!(calculate("00_1").is_err());
    }

    #[test]
    fn parameters_serialize_in_typebox_key_order() {
        // TypeBox 的 `JSON.stringify(Type.Object({...}))` 键序：type、required、properties。
        assert_eq!(
            serde_json::to_string(&calculate_parameters()).expect("serialize"),
            r#"{"type":"object","required":["expression"],"properties":{"expression":{"type":"string","description":"The mathematical expression to evaluate"}}}"#
        );
    }

    #[test]
    fn tool_executes_through_the_agent_tool_trait() {
        let tool = calculate_tool();
        assert_eq!(tool.name, "calculate");
        assert_eq!(tool.label, "Calculator");
        assert_eq!(tool.description, "Evaluate mathematical expressions");
        let result = tauri_block_on(tool.execute.execute(
            "calc-1",
            json!({"expression": "5 + 3"}),
            None,
            None,
        ))
        .expect("tool result");
        assert_eq!(text_of(&result), "5 + 3 = 8");
        assert!(result.details.is_null());
        assert!(result.usage.is_none());
    }

    #[test]
    fn tool_with_usage_attaches_usage() {
        let usage = Usage {
            input: 1,
            output: 2,
            ..Default::default()
        };
        let tool = create_calculate_tool_with_usage(usage.clone());
        let result =
            tauri_block_on(
                tool.execute
                    .execute("id", json!({"expression": "1 + 1"}), None, None),
            )
            .expect("tool result");
        assert_eq!(result.usage, Some(usage));
    }

    /// 单线程运行时只用于等待已经完成的 `BoxFuture`（工具不挂起）。
    fn tauri_block_on<F: std::future::Future>(future: F) -> F::Output {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime")
            .block_on(future)
    }
}
