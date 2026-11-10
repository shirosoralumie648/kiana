//! `JSON.parse` 等价入口，以及「带修复」解析的分层。
//!
//! 三层（与 `packages/ai/src/utils/json-parse.ts` 及 `partial-json` 的对应关系）：
//! 1. [`parse`]：严格的 `JSON.parse`。数字按 JS 语义解析（超出 f64 精确范围时舍入），
//!    解析期间用 `preserve_order` 保持文本顺序、返回前再重排为 JS 键序；错误消息逐字
//!    对齐 V8 的 `SyntaxError`。
//! 2. [`repair`]：TS 的 `repairJson`——转义字符串内的非法转义与控制字符；`parse_with_repair`
//!    在严格解析失败后重试。
//! 3. [`parse_streaming`]：TS 的 `parseStreamingJson`——严格解析 → 修复 → `partial-json`
//!    三级回退，对不完整输入返回「当前可用的部分」。
//!
//! 这一层是 `kiana-json` 里唯一允许返回「宽松结果」的地方；其余模块必须用严格 [`parse`]。
//!
//! # 键序
//!
//! [`parse`] 的结果是**普通 JS 对象**：其枚举序（`Object.keys`/`for...in`/`Map` 迭代）是
//! 「整数索引键升序 + 其余插入序」。因此 `parse` 在返回前就地重排每一层对象
//! （`crate::js_object` 的就地重排，显式栈）。重复键按 JS 语义「值取最后一个、
//! 位置取第一个」；`"__proto__"` 是普通自有键（与 `partial-json` 相反，见 `partial` 模块）。
//!
//! # 两处与 V8 的被迫偏差（`ARCHITECTURE.md` §8 第 31、32 条）
//!
//! 1. **孤立代理项**：V8 的 `JSON.parse` 接受 `"\uD800"` 并产生一个孤立代理项。Rust 的
//!    `String` 装不下孤立代理项（WTF-8 之外），因此本实现在解析成功后把每个未配对的
//!    code unit 替换为 U+FFFD。**不拒绝**是刻意的：接受度必须与 V8 一致，否则
//!    `parse_with_repair`/`parse_streaming` 的回退层数与 TS 不同。
//!    后果：再次 `JSON.stringify` 时写出 `"\uFFFD"` 而不是 `"\ud800"`。
//! 2. **非有限数**：V8 的 `JSON.parse("1e400")` 得到 `Infinity`；`serde_json::Number` 无法
//!    保存非有限值。本实现接受该输入并保存为 [`serde_json::Value::Null`]——这让
//!    `JSON.stringify(Infinity) === "null"` 这一条文本等价，但**不是**所有观察都等价：
//!    `String(Infinity)` 在 TS 是 `"Infinity"`、`Boolean(Infinity)` 是 `true`、
//!    `typeof Infinity === "number"`，而落成 `Value::Null` 后分别得到 `"null"`/`false`/`"object"`
//!    （见 `crate::js_string` 模块文档的说明）。要真正保持这些语义需要自定义数字类型，
//!    但 §4.9 的签名已冻结（`Value`），故记为被迫偏差。
//!
//! 递归深度不受 `serde_json` 的 `recursion_limit`（128）限制：解析器是显式栈实现的，且
//! 解析结果的重排/序列化/深比较也都用显式栈。唯一残留的深度限制来自 `serde_json::Value`
//! 自身的递归 `Drop`（外部类型，实测 8 MB 栈上约 100k–1M 层），见 [`crate::js_object`]。
//!
//! # 执行流程
//!
//! [`parse`] 的路径（也是 [`parse_with_repair`] 与 [`parse_streaming`] 的公共底座）：
//! 1. `Parser::new` 一次性把输入展开成 UTF-16 code unit 序列（理由见下节）；
//! 2. `parse_document` 先对四个特殊整串（`[object Object]`/`undefined`/`Infinity`/`NaN`）
//!    做整串判定，再交给 `parse_value`；
//! 3. `parse_value` 用显式栈自顶向下构建 `Value`，字符串与数字分别走 `parse_string`、
//!    `parse_number`；
//! 4. 值解析完后 `parse_document` 检查「尾部只剩空白」，否则报
//!    `Unexpected non-whitespace character after JSON`，然后才返回；
//! 5. 返回前调用 `crate::js_object` 的就地重排，让每个对象的枚举序等于 JS 的键序。
//!
//! [`parse_with_repair`] 在 [`parse`] 之上加一层「修复后重试」；[`parse_streaming`] 再加
//! `partial` 子模块的部分解析回退。`partial` 是独立文件（`src/js_parse/partial.rs`），
//! 不复用本文件的 `Parser`：两者接受范围不同（partial 明确允许不完整输入），不能互相替代。
//!
//! # 为什么按 UTF-16 而不是 Rust 的 `char`
//!
//! V8 的错误位置、行列与 excerpt 全部以 UTF-16 code unit 计数；Rust 的 `char` 是 Unicode
//! 标量值，两者对非 BMP 字符（例如 emoji）的计数不同。若按 `char` 计，错误消息里的
//! `at position N` 就会与 V8 相差若干位。因此 `Parser` 持有 `Vec<u16>`，只在产出最终
//! `String` 与拼装错误文本时解码；代价是输入多一份 UTF-16 副本。
//!
//! # 注意事项
//!
//! - 本模块的错误消息是**对外契约**（测试按 TS 记录的文本逐字比对），改任何模板前先看
//!   `tests/api_golden.rs` 与 `tests/fixtures/` 下的期望值。
//! - [`parse`] 的返回值已按 JS 键序重排，调用方不要再做一次 `sort_js_keys`。
//! - [`repair`] 只修字符串字面量内部，绝不补括号/引号/逗号；补结构是 `partial` 的职责。
//! - 从 `i64`/`u64` 字段手工构造的 `Value` 不会自动归一化，跨边界前要先过
//!   [`normalize_numbers`]（或直接用 [`crate::js_object::stringify`]）。

use serde_json::Value;

use crate::js_num::{parse_decimal_literal, trim_js_whitespace};

pub mod partial;

/// 解析失败。
///
/// `message` 是当前稳定契约：严格面与 V8 的 `SyntaxError.message` 一致（`JSON.parse`
/// 面），宽松面沿用 `repairJson`/`partial-json` 的错误文本。改动需同步测试。
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
#[error("{message}")]
pub struct ParseError {
    /// 完整错误文本；带位置的错误已包含 `at position N (line L column C)` 后缀（与 `position`
    /// 字段对应），无位置错误（如 `Unexpected end of JSON input`）只有模板文本。
    /// 测试按 TS 记录的文本整串比对，因此模板与标点都不能随意改。
    pub message: String,
    /// 1-based 行/列；`serde_json` 提供，其他实现可缺省。
    pub position: Option<(usize, usize)>,
}

impl ParseError {
    /// 构造一条没有位置信息的错误。
    ///
    /// V8 有若干错误模板不带位置（如 `Unexpected end of JSON input`、特殊整串的
    /// `"<input>" is not valid JSON`），这些路径用本构造函数。
    pub fn new(message: impl Into<String>) -> Self {
        ParseError {
            message: message.into(),
            position: None,
        }
    }

    /// `"<prefix> in JSON at position N (line L column C)"`（V8 的 15 类位置模板）。
    fn at_position(prefix: &str, source: &str, pos: usize) -> Self {
        let (line, column) = file_location(source, pos);
        ParseError {
            message: format!("{prefix} in JSON at position {pos} (line {line} column {column})"),
            position: Some((line, column)),
        }
    }

    /// `"<prefix> at position N (line L column C)"`——
    /// `Unexpected non-whitespace character after JSON` 的模板里没有 `in JSON`。
    fn at_position_plain(prefix: &str, source: &str, pos: usize) -> Self {
        let (line, column) = file_location(source, pos);
        ParseError {
            message: format!("{prefix} at position {pos} (line {line} column {column})"),
            position: Some((line, column)),
        }
    }
}

/// `JSON.parse(text)`。
///
/// 与 `serde_json::from_str` 的差异（全部对齐 V8）：
/// - 接受孤立代理项转义（替换为 U+FFFD，见模块文档）；
/// - 接受数值溢出（`1e400` → `null`，见模块文档）；
/// - 无递归深度限制；
/// - 十进制字面量→f64 与 V8 逐 bit 一致；
/// - 错误消息是 V8 的 16 类模板之一，位置是 0-based UTF-16 偏移 + 1-based 行列。
pub fn parse(text: &str) -> Result<Value, ParseError> {
    let mut parser = Parser::new(text);
    parser.parse_document()
}

// ---------------------------------------------------------------------------
// V8 兼容的 JSON 解析器
// ---------------------------------------------------------------------------

/// V8 的「特殊字符串」：这些整串输入走 `"%\" is not valid JSON` 模板（不带 token）。
const SPECIAL_STRINGS: [&str; 4] = ["[object Object]", "undefined", "Infinity", "NaN"];

/// V8 `JsonParser::kMaxContextCharacters`。
const MAX_CONTEXT_CHARS: usize = 10;
/// V8 `kMinOriginalSourceLengthForContext = kMaxContextCharacters * 2 + 1`。
const MIN_SOURCE_LEN_FOR_CONTEXT: usize = MAX_CONTEXT_CHARS * 2 + 1;

/// 输入按 UTF-16 code unit 处理（与 V8 的 `JsonParser<uint16_t>` 一致）。
///
/// JS 的 `String` 是 UTF-16 序列，V8 的 JSON.parse 位置、excerpt、行列全部按 code unit 计。
/// Rust 的 `&str` 可以无损展开成 `Vec<u16>`（不能表示孤立代理项，但输入本身是合法 UTF-8）。
struct Parser<'a> {
    /// 原始输入。用于特殊整串的整串判定，以及错误文本里的位置/行列计算
    /// （`file_location` 会按原文重新展开成 UTF-16 计数）。
    source: &'a str,
    /// 输入展开后的 UTF-16 code unit 序列；解析游标与所有下标都以它为坐标系。
    units: Vec<u16>,
    /// 当前游标（code unit 下标）。
    pos: usize,
}

impl<'a> Parser<'a> {
    /// 建立解析器：一次性把整个输入展开成 UTF-16，时间和内存都是 O(n)。
    ///
    /// 之所以不边解析边按 `char` 迭代：同一份输入要多次按下标回看（excerpt、
    /// 数字子串、error 位置），先展开成随机访问的 `Vec<u16>` 最直接。
    fn new(source: &'a str) -> Self {
        Parser {
            source,
            units: source.encode_utf16().collect(),
            pos: 0,
        }
    }

    // -- 基础读取 ----------------------------------------------------------

    fn len(&self) -> usize {
        self.units.len()
    }

    fn at_end(&self) -> bool {
        self.pos >= self.units.len()
    }

    /// `CurrentCharacter()`：越界返回 `None`（V8 的 `kEndOfString`）。
    fn current(&self) -> Option<u16> {
        self.units.get(self.pos).copied()
    }

    fn char_at(&self, i: usize) -> Option<u16> {
        self.units.get(i).copied()
    }

    fn advance(&mut self) {
        self.pos += 1;
    }

    /// V8 `GetNextNonWhitespaceToken`：跳过 ` \t\r\n`。
    ///
    /// JSON 语法只把这四个 ASCII 字符算作空白；U+00A0、U+FEFF 等在 JSON 里不是空白
    /// （与 JS `String.prototype.trim` 的空白集合不同，后者用于流式入口的空输入判断）。
    fn skip_whitespace(&mut self) {
        while let Some(c) = self.current() {
            if c == u16::from(b' ')
                || c == u16::from(b'\t')
                || c == u16::from(b'\r')
                || c == u16::from(b'\n')
            {
                self.pos += 1;
            } else {
                break;
            }
        }
    }

    // -- 错误构造 ----------------------------------------------------------

    /// 以 `"<prefix> in JSON at position …"` 模板构造错误。
    ///
    /// 行列循环 O(pos)，但前置的整串 UTF-16 展开是 O(n)，整体 O(n)（n 为输入长度）。
    fn err_at(&self, prefix: &str, pos: usize) -> ParseError {
        ParseError::at_position(prefix, self.source, pos)
    }

    /// `ReportUnexpectedCharacter` / `ReportUnexpectedToken` 的等价物。
    ///
    /// - `None`（EOS）→ `Unexpected end of JSON input`（无位置后缀）；
    /// - 数字起始字符（`-` 或 0-9）→ `Unexpected number in JSON at position …`；
    /// - `"` → `Unexpected string in JSON at position …`；
    /// - 其余 → `Unexpected token '<X>', <excerpt> is not valid JSON`。
    fn unexpected_token(&self, pos: usize) -> ParseError {
        let Some(ch) = self.char_at(pos) else {
            return ParseError::new("Unexpected end of JSON input");
        };
        // V8 的一张字符表 `one_char_json_tokens`：`-`/数字是 NUMBER，`"` 是 STRING，
        // 二者走专属模板（`LookUpErrorMessageForJsonToken` 的前两个 case）。
        if ch == b'-' as u16 || is_decimal_digit(ch) {
            return self.err_at("Unexpected number", pos);
        }
        if ch == b'"' as u16 {
            return self.err_at("Unexpected string", pos);
        }
        let token = render_code_unit(ch);
        let excerpt = match self.excerpt(pos) {
            Some(e) => e,
            None => return ParseError::new("Unexpected end of JSON input"),
        };
        // V8 的四种模板：整串、前缀省略、两侧省略、后缀省略。
        let message = if self.len() < MIN_SOURCE_LEN_FOR_CONTEXT {
            format!("Unexpected token '{token}', \"{excerpt}\" is not valid JSON")
        } else if pos < MAX_CONTEXT_CHARS {
            format!("Unexpected token '{token}', \"{excerpt}\"... is not valid JSON")
        } else if pos < self.len() - MAX_CONTEXT_CHARS {
            format!("Unexpected token '{token}', ...\"{excerpt}\"... is not valid JSON")
        } else {
            format!("Unexpected token '{token}', ...\"{excerpt}\" is not valid JSON")
        };
        ParseError::new(message)
    }

    /// V8 `GetErrorMessageWithEllipses` 的 `arg2`（不含省略号）。
    fn excerpt(&self, pos: usize) -> Option<String> {
        let len = self.len();
        // 上下文窗口固定 10 个 code unit：靠近开头时省略号只在右侧（excerpt 从 0 开始），
        // 靠近结尾时省略号只在左侧（到 len 结束），在中间则两侧各取 10 个；
        // 输入本身短于 21 个 unit 时整串都算上下文。
        let (start, end) = if len < MIN_SOURCE_LEN_FOR_CONTEXT {
            (0, len)
        } else if pos < MAX_CONTEXT_CHARS {
            (0, pos + MAX_CONTEXT_CHARS)
        } else if pos >= MAX_CONTEXT_CHARS && pos < len - MAX_CONTEXT_CHARS {
            (pos - MAX_CONTEXT_CHARS, pos + MAX_CONTEXT_CHARS)
        } else {
            (pos - MAX_CONTEXT_CHARS, len)
        };
        if start >= end {
            return Some(String::new());
        }
        Some(decode_units_lossy(&self.units[start..end.min(len)]))
    }

    // -- 入口 --------------------------------------------------------------

    /// 解析整篇文档：特殊整串判定 → 一个值 → 尾部检查 → JS 键序重排。
    ///
    /// 输入为空或只含 JSON 空白时，`parse_value` 会在读到 EOS 时报
    /// `Unexpected end of JSON input`（不带位置后缀）。
    fn parse_document(&mut self) -> Result<Value, ParseError> {
        // V8 在派发到具体语法之前先比对整串：`[object Object]`/`undefined`/`Infinity`/`NaN`
        // 走 `"<input>" is not valid JSON`（不带 token）。必须放在这里——`[object Object]`
        // 以 `[` 开头，否则会先进入 parse_array 并报成 `Unexpected token 'o'`。
        // 比较对象是未 trim 的 `self.source`，带首尾空白的输入不会命中这条特判。
        if SPECIAL_STRINGS.contains(&self.source) {
            return Err(ParseError::new(format!(
                "\"{}\" is not valid JSON",
                self.source
            )));
        }
        let value = self.parse_value()?;
        self.skip_whitespace();
        if !self.at_end() {
            return Err(ParseError::at_position_plain(
                "Unexpected non-whitespace character after JSON",
                self.source,
                self.pos,
            ));
        }
        // `JSON.parse` 的结果在 JS 里是普通对象；`Object.keys`/`for...in`/`Map` 迭代按
        // 「整数索引键升序 + 其余插入序」枚举。`preserve_order` 的 `Map` 只保文本顺序，
        // 因此这里就地重排（显式栈，任意深度），让解析结果的枚举序直接等于 JS 的。
        let mut value = value;
        crate::js_object::reorder_in_place(&mut value);
        Ok(value)
    }

    // -- 值 ----------------------------------------------------------------

    /// 解析一个值。用显式栈，避免 V8 之外的人为深度上限。
    fn parse_value(&mut self) -> Result<Value, ParseError> {
        // 容器栈：每个 frame 记录正在构建的容器与「等待键」状态。
        enum Frame {
            Array(Vec<Value>),
            // (map, 已读到键、等待值)
            Object(serde_json::Map<String, Value>, Option<String>),
        }
        // 双层循环 + 显式栈：
        // - 外层每轮「产出一个值」：读一个原子值，或压入一个新容器后 continue；
        // - 内层把刚产出的值挂到栈顶容器，然后看下一个字符决定去向：`,` 回到外层产下一个值；
        //   闭合符则弹出容器、把该容器当成「新值」继续向上挂；栈空表示整篇文档完成。
        // 不变量：`Frame::Object` 的 slot 为 `Some` 当且仅当「已读到键、正在等它的值」。
        // 递归因此变成堆上的 `Vec`，解析深度只受内存限制（`serde_json::Value` 自身的
        // 递归 `Drop` 是另一处限制，见模块文档）。
        let mut stack: Vec<Frame> = Vec::new();
        loop {
            // --- 产出一个值 ---
            self.skip_whitespace();
            let mut value = match self.current() {
                None => return Err(ParseError::new("Unexpected end of JSON input")),
                Some(c) if c == b'"' as u16 => Value::String(self.parse_string()?),
                Some(c) if c == b'{' as u16 => {
                    self.advance();
                    self.skip_whitespace();
                    // 空容器可以直接产出一个值，不必压栈；非空时才压栈并先读第一个键
                    // （此时值还没产出，所以 slot 稍后填 `Some(key)`）。
                    if self.current() == Some(b'}' as u16) {
                        self.advance();
                        Value::Object(serde_json::Map::new())
                    } else {
                        stack.push(Frame::Object(serde_json::Map::new(), None));
                        let key = self.parse_property_key(true)?;
                        self.expect_colon()?;
                        match &mut stack.last_mut().expect("just pushed") {
                            Frame::Object(_, slot) => *slot = Some(key),
                            Frame::Array(_) => unreachable!(),
                        }
                        continue;
                    }
                }
                Some(c) if c == b'[' as u16 => {
                    self.advance();
                    self.skip_whitespace();
                    if self.current() == Some(b']' as u16) {
                        self.advance();
                        Value::Array(Vec::new())
                    } else {
                        stack.push(Frame::Array(Vec::new()));
                        continue;
                    }
                }
                Some(c) if c == b't' as u16 => {
                    self.scan_literal(b"true")?;
                    Value::Bool(true)
                }
                Some(c) if c == b'f' as u16 => {
                    self.scan_literal(b"false")?;
                    Value::Bool(false)
                }
                Some(c) if c == b'n' as u16 => {
                    self.scan_literal(b"null")?;
                    Value::Null
                }
                Some(c) if c == b'-' as u16 || is_decimal_digit(c) => self.parse_number()?,
                Some(_) => return Err(self.unexpected_token(self.pos)),
            };

            // --- 把值挂到父容器上，并处理分隔符 ---
            loop {
                match stack.last_mut() {
                    None => return Ok(value),
                    Some(Frame::Array(items)) => {
                        items.push(value);
                        self.skip_whitespace();
                        match self.current() {
                            Some(c) if c == b',' as u16 => {
                                self.advance();
                                break; // 产出下一个值
                            }
                            Some(c) if c == b']' as u16 => {
                                self.advance();
                                let Frame::Array(items) = stack.pop().expect("non-empty") else {
                                    unreachable!()
                                };
                                value = Value::Array(items);
                                continue;
                            }
                            _ => {
                                return Err(self
                                    .err_at("Expected ',' or ']' after array element", self.pos));
                            }
                        }
                    }
                    Some(Frame::Object(map, slot)) => {
                        let key = slot.take().expect("key is pending when a value completes");
                        // `preserve_order` 下 `Map` 是 IndexMap：对已存在的键 `insert` 只覆盖
                        // 值、保留首次出现的位置，正好等价于 JS 的重复键语义
                        // 「值取最后一个、位置取第一个」。
                        map.insert(key, value);
                        self.skip_whitespace();
                        match self.current() {
                            Some(c) if c == b',' as u16 => {
                                self.advance();
                                let key = self.parse_property_key(false)?;
                                self.expect_colon()?;
                                match stack.last_mut().expect("still on the stack") {
                                    Frame::Object(_, slot) => *slot = Some(key),
                                    Frame::Array(_) => unreachable!(),
                                }
                                break; // 产出下一个值
                            }
                            Some(c) if c == b'}' as u16 => {
                                self.advance();
                                let Frame::Object(map, _) = stack.pop().expect("non-empty") else {
                                    unreachable!()
                                };
                                value = Value::Object(map);
                                continue;
                            }
                            _ => {
                                return Err(self
                                    .err_at("Expected ',' or '}' after property value", self.pos));
                            }
                        }
                    }
                }
            }
        }
    }

    /// 读一个对象键（含引号）。`first` 为真时用 `Expected property name or '}'` 模板，
    /// 否则用 `Expected double-quoted property name`（与 V8 的两个不同报错点一致）。
    fn parse_property_key(&mut self, first: bool) -> Result<String, ParseError> {
        self.skip_whitespace();
        if self.current() == Some(b'"' as u16) {
            return self.parse_string();
        }
        // V8: 第一个键用 kJsonParseExpectedPropNameOrRBrace；后续键先查逗号。
        let prefix = if first {
            "Expected property name or '}'"
        } else {
            "Expected double-quoted property name"
        };
        Err(self.err_at(prefix, self.pos))
    }

    /// V8 `ExpectNext<JsonToken::COLON>`：跳过空白后必须出现 `:`。
    fn expect_colon(&mut self) -> Result<(), ParseError> {
        self.skip_whitespace();
        if self.current() == Some(b':' as u16) {
            self.advance();
            Ok(())
        } else {
            Err(self.err_at("Expected ':' after property name", self.pos))
        }
    }

    /// `ScanLiteral`：第一个字符已由调用方确认，比较剩余字符。
    ///
    /// 逐字符比较；不匹配时报 `Unexpected token '<该字符>'`（EOS 时报 `Unexpected end of JSON input`）。
    fn scan_literal(&mut self, literal: &[u8]) -> Result<(), ParseError> {
        debug_assert_eq!(self.current(), Some(literal[0] as u16));
        self.advance();
        for &expected in &literal[1..] {
            match self.current() {
                None => return Err(ParseError::new("Unexpected end of JSON input")),
                Some(c) if c == expected as u16 => self.advance(),
                Some(_) => return Err(self.unexpected_token(self.pos)),
            }
        }
        Ok(())
    }

    // -- 数字 --------------------------------------------------------------

    /// V8 `ParseJsonNumberAsDoubleOrSmi` 的语法部分（我们只保留 double 语义）。
    ///
    /// 语法与报错点：
    /// - `-` 后必须跟数字，否则 `No number after minus sign`（位置在 `-` 之后）；
    /// - 前导零后不能紧跟数字，否则 `Unexpected number`（位置在后一个数字）；
    /// - `.` 后必须跟数字，否则 `Unterminated fractional number`；
    /// - `e`/`E` 允许 `+`/`-`，其后必须跟数字，否则 `Exponent part is missing a number`。
    fn parse_number(&mut self) -> Result<Value, ParseError> {
        // 三个子阶段按 JSON 语法顺序扫描：整数 → 小数 → 指数。每个阶段记录起点是为了
        // 切出该阶段的数字片段；错误分支用当前游标 `self.pos` 定位，因此位置落在实际
        // 出错的字符上，而不是字面量开头。
        let start = self.pos;
        let negative = self.current() == Some(b'-' as u16);
        if negative {
            self.advance();
        }
        // 整数部分。
        let mut int_start = self.pos;
        if self.current() == Some(b'0' as u16) {
            self.advance();
            // 前导零后紧跟数字 → Unexpected number（在第二个数字处）。
            if let Some(c) = self.current()
                && is_decimal_digit(c)
            {
                return Err(self.err_at("Unexpected number", self.pos));
            }
        } else {
            let digits_start = self.pos;
            while self.current().is_some_and(is_decimal_digit) {
                self.advance();
            }
            if self.pos == digits_start {
                // 空整数部分：只有 `-` 后无数字会走到这里。
                return Err(self.err_at("No number after minus sign", self.pos));
            }
            int_start = digits_start;
        }
        let int_digits = self.slice_ascii(int_start, self.pos);

        // 小数部分。
        let mut frac_digits = String::new();
        if self.current() == Some(b'.' as u16) {
            self.advance();
            let frac_start = self.pos;
            while self.current().is_some_and(is_decimal_digit) {
                self.advance();
            }
            if self.pos == frac_start {
                return Err(self.err_at("Unterminated fractional number", self.pos));
            }
            frac_digits = self.slice_ascii(frac_start, self.pos);
        }

        // 指数部分。
        let mut exp10: i64 = 0;
        if self
            .current()
            .is_some_and(|c| c == b'e' as u16 || c == b'E' as u16)
        {
            self.advance();
            let mut exp_negative = false;
            if let Some(c) = self.current() {
                if c == b'+' as u16 {
                    self.advance();
                } else if c == b'-' as u16 {
                    exp_negative = true;
                    self.advance();
                }
            }
            let exp_start = self.pos;
            while self.current().is_some_and(is_decimal_digit) {
                self.advance();
            }
            if self.pos == exp_start {
                return Err(self.err_at("Exponent part is missing a number", self.pos));
            }
            let text = self.slice_ascii(exp_start, self.pos);
            // 指数可能极长（`0.4e006699…`）；饱和到 i64 边界即可，值会溢出/下溢成 ∞/0。
            // `parse` 失败只可能是位数超出 i64；该量级已远超 f64 的十进制指数范围（约 ±324），
            // 所以后续标准库解析必然给出 ∞ 或 0，饱和的具体数值不再影响结果。
            exp10 = text.parse::<i64>().unwrap_or(i64::MAX / 4);
            if exp_negative {
                exp10 = -exp10;
            }
        }

        // `start` 记录了字面量起点，但后续没有读取点（错误都定位到具体子阶段）；
        // `let _ = start;` 是显式丢弃，避免未使用变量警告。
        let _ = start;
        let magnitude = parse_decimal_literal(negative, &int_digits, &frac_digits, exp10);
        // 非有限值（V8 得到 ±Infinity）存为 Null：JSON.stringify(Infinity) === "null"。
        if !magnitude.is_finite() {
            return Ok(Value::Null);
        }
        // 整数在内存里保留整数表示（`ARCHITECTURE.md` §4.3 第 4 条）：`as_u64()`/`as_i64()`
        // 必须可用，`serde_json::to_string` 也才会写 `1` 而不是 `1.0`。JS 只有 f64 一个数字
        // 类型，字面量怎么拼写无关紧要：`1`、`1.0`、`1e2`、`0.1e1` 在 V8 里都是
        // `Number.isInteger` 为真的整数 1/100，TS 的强类型消费方（TypeBox `Integer`、
        // Rust 侧 `i64`/`u64` 反序列化）也照收。因此判据是**解析出的 f64 值**是否整数，
        // 而不是字面量里有没有小数点/指数。
        //
        // `-0` 不能走整数路径（`-0i64 == 0` 会丢符号）；JS 的 `-0` 在 `Object.is` 下可观察，
        // 且 `format_js_number(-0.0)` 与 `format_js_number(0.0)` 都是 `"0"`，所以仅
        // `Object.is(JSON.parse("-0"), -0)` 这类用例依赖它——按 f64 保留。
        if !(negative && magnitude == 0.0)
            && let Some(n) = integer_number(magnitude)
        {
            return Ok(Value::Number(n));
        }
        match serde_json::Number::from_f64(magnitude) {
            Some(n) => Ok(Value::Number(n)),
            None => Ok(Value::Null),
        }
    }

    /// 取 `[start, end)` 的 ASCII 子串（数字字面量只含 ASCII）。
    fn slice_ascii(&self, start: usize, end: usize) -> String {
        self.units[start..end]
            .iter()
            .map(|&u| u as u8 as char)
            .collect()
    }

    // -- 字符串 ------------------------------------------------------------

    /// V8 `ScanJsonString`：当前位置必须是 `"`。
    ///
    /// 报错点：
    /// - 输入在闭合引号前结束 → `Unterminated string`（位置在 EOF）；
    /// - `\` 后跟非 Latin1 字符 → `Unexpected token '<该字符>'`（V8 的 JSON 转义只允许 Latin1）；
    /// - 非法转义 → `Bad escaped character`（位置在转义字符本身）；
    /// - `\u` 后 4 位非 hex → `Bad Unicode escape`（位置在 `u` 之后的第一个非 hex 位）；
    /// - 字符串内原始控制字符（< 0x20）→ `Bad control character in string literal`。
    fn parse_string(&mut self) -> Result<String, ParseError> {
        debug_assert_eq!(self.current(), Some(b'"' as u16));
        self.advance(); // 跳过开引号
        // V8 内部是 UTF-16 字符串；这里同样先收集 code unit，最后统一按「合法代理对组合、
        // 孤立代理项替换为 U+FFFD」解码（Rust 的 String 装不下孤立代理项）。
        let mut out: Vec<u16> = Vec::new();
        loop {
            let Some(c) = self.current() else {
                return Err(self.err_at("Unterminated string", self.pos));
            };
            if c == b'"' as u16 {
                self.advance();
                break;
            }
            if c == b'\\' as u16 {
                self.advance();
                let Some(esc) = self.current() else {
                    // V8: `\` 后是 EOS -> NextCharacter() 返回 kEndOfString，
                    // ReportUnexpectedCharacter(EOS) -> "Unexpected end of JSON input"。
                    return Err(ParseError::new("Unexpected end of JSON input"));
                };
                if esc > 0xff {
                    // V8：转义字符超出 Latin1 → ReportUnexpectedCharacter(c)。
                    return Err(self.unexpected_token(self.pos));
                }
                self.advance();
                match esc {
                    e if e == b'"' as u16 => out.push(b'"' as u16),
                    e if e == b'\\' as u16 => out.push(b'\\' as u16),
                    e if e == b'/' as u16 => out.push(b'/' as u16),
                    e if e == b'b' as u16 => out.push(0x08),
                    e if e == b'f' as u16 => out.push(0x0c),
                    e if e == b'n' as u16 => out.push(0x0a),
                    e if e == b'r' as u16 => out.push(0x0d),
                    e if e == b't' as u16 => out.push(0x09),
                    e if e == b'u' as u16 => {
                        let mut value: u16 = 0;
                        for _ in 0..4 {
                            let Some(h) = self.current() else {
                                return Err(self.err_at("Bad Unicode escape", self.pos));
                            };
                            match hex_value(h) {
                                Some(d) => {
                                    value = value.wrapping_mul(16).wrapping_add(u16::from(d));
                                    self.advance();
                                }
                                None => {
                                    return Err(self.err_at("Bad Unicode escape", self.pos));
                                }
                            }
                        }
                        out.push(value);
                    }
                    _ => return Err(self.err_at("Bad escaped character", self.pos - 1)),
                }
                continue;
            }
            if c < 0x20 {
                return Err(self.err_at("Bad control character in string literal", self.pos));
            }
            out.push(c);
            self.advance();
        }
        // 此时 `out` 是 JS 字符串的 code unit 序列。Rust 的 `String` 只能装合法 UTF-8，
        // 所以孤立的代理项在这里统一替换成 U+FFFD（模块文档记录的被迫偏差第 1 条）。
        Ok(decode_units_lossy(&out))
    }
}

/// 把解析出的整数值转成 `serde_json::Number`，保持整数表示。
///
/// 只有**值本身是整数且落在 ±2^53 内**时才保留整数类型：
/// - JS 的数字是 f64，`9007199254740993` 在 JS 里是 `9007199254740992`；若原样保留 u64
///   会让 `as_u64()` 暴露 JS 不存在的精度，因此超过 2^53 的整数一律走 f64 路径
///   （与骨架的 `normalize_numbers` 判据一致）。
/// - 超出 u64 的值同样走 f64 路径（`18446744073709551616` → f64）。
///
/// `value` 是 `parse_decimal_literal` 的结果（带符号；`-0.0` 由调用方排除）。
/// 边界取闭区间：`|value| == 2^53` 仍保留整数（2^53 本身可被 f64 精确表示，
/// 闭区间内整数值 f64 转 u64/i64 无精度损失）。
/// 返回 `None` 表示需要按 f64 处理。
fn integer_number(value: f64) -> Option<serde_json::Number> {
    // `Number.isInteger` 语义：非有限值与带小数的值都不是整数。
    if !crate::js_num::is_js_integer(value) {
        return None;
    }
    const MAX_SAFE: f64 = 9_007_199_254_740_992.0; // 2^53
    if value.abs() > MAX_SAFE {
        return None;
    }
    // 2^53 以内整数的 f64 值可无损转 i64/u64（含 0）。
    Some(if value < 0.0 {
        serde_json::Number::from(value as i64)
    } else {
        serde_json::Number::from(value as u64)
    })
}

/// 把一棵值树里的整数按 JS 的 f64 语义归一化（`ARCHITECTURE.md` §4.3 第 4 条指名要求的入口）。
///
/// 场景：Rust 侧从强类型结构体（`i64`/`u64` 字段）构造出 `Value` 时，数字可能带 JS 不存在的
/// 精度。JS 只有 f64 一个数字类型，`18446744073709551615` 在 JS 里是
/// `18446744073709552000`；若原样把 u64 交给 `serde_json::to_string`，写出的文本就是
/// `18446744073709551615`，与 TS 的 `JSON.stringify` 不同。**任何进入模型可见文本 / 会话文件 /
/// 线协议的整数**都必须先过这里（或直接走 [`crate::js_object::stringify`]，它用
/// `as_f64()` 读数字，已经在写文本时做了同样的舍入）。
///
/// 判据与 [`crate::js_object::stringify`] 一致：`|v| > 2^53` 的整数转成 f64 表示。2^53 以内
/// 的整数保持整数类型（`as_u64()`/`as_i64()` 继续可用，`serde` 也写 `1` 而不是 `1.0`）。
/// 本函数只替换数字值，**不移动任何键**：对象的迭代序与传入时相同（单元测试锁定这一点）。
/// 递归用显式栈，深度无上限（`serde_json::Value` 的 `recursion_limit` 不适用于内存里的值）。
pub fn normalize_numbers(value: Value) -> Value {
    fn fix(number: serde_json::Number) -> serde_json::Number {
        if let Some(u) = number.as_u64() {
            if u > (1u64 << 53) {
                return serde_json::Number::from_f64(u as f64).unwrap_or(number);
            }
            return number;
        }
        if let Some(i) = number.as_i64() {
            if i.unsigned_abs() > (1u64 << 53) {
                return serde_json::Number::from_f64(i as f64).unwrap_or(number);
            }
            return number;
        }
        number
    }

    enum Task {
        /// 待处理的值（按下标放回父容器）。
        Visit(Value),
        /// 数组：`len` 个已完成的值在 `done` 的尾部（正序）。
        FinishArray(usize),
        /// 对象：键列表与已完成的值的个数。
        FinishObject(Vec<String>),
    }

    // 任务栈 + 结果栈的迭代式后序遍历：
    // - `Visit` 把容器拆开，子项按原顺序**逆序**压栈，保证弹出时是正序；
    // - 子项完成后依次排在 `done` 尾部，`Finish*` 一次性切下这段并重组父容器；
    // - 对象在拆开前先记下键列表，重组时按键序 zip 回去，因此键序保持不变。
    let mut tasks = vec![Task::Visit(value)];
    let mut done: Vec<Value> = Vec::new();
    while let Some(task) = tasks.pop() {
        match task {
            Task::Visit(Value::Number(n)) => done.push(Value::Number(fix(n))),
            Task::Visit(Value::Array(items)) => {
                if items.is_empty() {
                    done.push(Value::Array(Vec::new()));
                    continue;
                }
                let len = items.len();
                tasks.push(Task::FinishArray(len));
                for item in items.into_iter().rev() {
                    tasks.push(Task::Visit(item));
                }
            }
            Task::Visit(Value::Object(map)) => {
                if map.is_empty() {
                    done.push(Value::Object(serde_json::Map::new()));
                    continue;
                }
                let keys: Vec<String> = map.keys().cloned().collect();
                tasks.push(Task::FinishObject(keys));
                for (_, v) in map.into_iter().rev() {
                    tasks.push(Task::Visit(v));
                }
            }
            Task::Visit(other) => done.push(other),
            Task::FinishArray(len) => {
                let start = done.len() - len;
                let items = done.split_off(start);
                done.push(Value::Array(items));
            }
            Task::FinishObject(keys) => {
                let start = done.len() - keys.len();
                let values = done.split_off(start);
                let mut map = serde_json::Map::with_capacity(keys.len());
                for (key, value) in keys.into_iter().zip(values) {
                    map.insert(key, value);
                }
                done.push(Value::Object(map));
            }
        }
    }
    // 根值完成时 `done` 里恰好只剩一个元素；`unwrap_or` 是 release 下断言被编译掉时的
    // 防御性兜底，不应被触发。
    debug_assert_eq!(done.len(), 1);
    done.pop().unwrap_or(Value::Null)
}

fn is_decimal_digit(c: u16) -> bool {
    (b'0' as u16..=b'9' as u16).contains(&c)
}

fn hex_value(c: u16) -> Option<u8> {
    match c {
        c if (b'0' as u16..=b'9' as u16).contains(&c) => Some((c as u8) - b'0'),
        c if (b'a' as u16..=b'f' as u16).contains(&c) => Some((c as u8) - b'a' + 10),
        c if (b'A' as u16..=b'F' as u16).contains(&c) => Some((c as u8) - b'A' + 10),
        _ => None,
    }
}

/// 把单个 code unit 渲染成错误消息里的 token。
///
/// V8 的消息模板会把该 code unit 原样插入；Rust 字符串装不下孤立代理项，因此非 BMP 的
/// 高/低代理项输出 U+FFFD（fixture: `'[1,😀]'` → `Unexpected token '\u{FFFD}'`）。
fn render_code_unit(unit: u16) -> String {
    match char::from_u32(u32::from(unit)) {
        Some(c) => c.to_string(),
        None => "\u{fffd}".to_string(),
    }
}

/// 把 UTF-16 code unit 序列解码成字符串；孤立代理项替换为 U+FFFD。
///
/// 单遍 O(n)：高代理项（`0xD800..0xDC00`）先看后一个 unit，若为低代理项
/// （`0xDC00..0xE000`）则按 UTF-16 公式组合成 BMP 外字符并跳过两个 unit；
/// 配不上对的（包括孤立的低代理项）各输出一个 U+FFFD。
fn decode_units_lossy(units: &[u16]) -> String {
    let mut out = String::with_capacity(units.len());
    let mut i = 0;
    while i < units.len() {
        let u = units[i];
        if (0xd800..0xdc00).contains(&u) {
            if let Some(&low) = units.get(i + 1)
                && (0xdc00..0xe000).contains(&low)
            {
                let cp = 0x10000 + ((u32::from(u) - 0xd800) << 10) + (u32::from(low) - 0xdc00);
                out.push(char::from_u32(cp).unwrap_or('\u{fffd}'));
                i += 2;
                continue;
            }
            out.push('\u{fffd}');
            i += 1;
        } else if (0xdc00..0xe000).contains(&u) {
            out.push('\u{fffd}');
            i += 1;
        } else {
            out.push(char::from_u32(u32::from(u)).unwrap_or('\u{fffd}'));
            i += 1;
        }
    }
    out
}

/// V8 `JsonParser::CalculateFileLocation`：位置是 0-based UTF-16 偏移，返回 1-based (line, column)。
///
/// JSON 只认 `\r` 与 `\n` 为换行；`\r\n` 只算一次；U+2028/U+2029 不算换行。
/// `pos` 超过输入长度时先截到末尾。先把整串展开为 UTF-16（O(n)），再扫描到 `pos`；
/// 只在构造错误时调用，成功路径没有这份开销。
fn file_location(source: &str, pos: usize) -> (usize, usize) {
    let units: Vec<u16> = source.encode_utf16().collect();
    let pos = pos.min(units.len());
    let mut line = 1usize;
    let mut last_break = 0usize;
    let mut i = 0usize;
    while i < pos {
        let c = units[i];
        if c == b'\r' as u16 && i + 1 < pos && units[i + 1] == b'\n' as u16 {
            i += 1;
        }
        if c == b'\r' as u16 || c == b'\n' as u16 {
            line += 1;
            last_break = i + 1;
        }
        i += 1;
    }
    (line, 1 + pos - last_break)
}

// ---------------------------------------------------------------------------
// repairJson / parseJsonWithRepair / parseStreamingJson
// ---------------------------------------------------------------------------

/// TS 的 `repairJson`：修复 JSON 字符串字面量里的非法转义与原始控制字符。
///
/// 按 UTF-16 code unit 扫描（TS 的 `json[index]` 是 code unit），字符串外的内容一律原样复制，
/// **绝不**补括号/引号/逗号（那是 `partial-json` 的职责）。
///
/// 规则（`spec/ai-core.md` §2.2.25）：
/// - 串内 `"` → 原样并退出；
/// - 串内 `\`：
///   - 末尾（无后继）→ 输出 `\\`（两个字符）；
///   - 后继 `u` 且随后 4 位都是 hex → 整段 `\uXXXX` 原样复制并跳过 5 位；
///   - 后继在 `VALID_JSON_ESCAPES`（`" \ / b f n r t`）→ 原样复制两者并跳过 1 位；
///   - 否则输出 `\\` 且**不消费**后继字符；
/// - 串内控制字符（0x00-0x1F）→ `\b \f \n \r \t` 短形式，其余 `\u00xx` 小写四位；
/// - 其余原样。
///
/// 与 TS 唯一的差别：TS 的字符串是 UTF-16，`json[index]` 对代理对是两个 code unit；这里同样
/// 按 code unit 扫描，因此代理对原样保留（不会被拆开或替换）。
///
/// 当前实现没有失败分支，总是返回 `Some`；调用方仍按 `Option` 处理，把 `None` 当作
/// 「没有可用的修复文本」。
pub fn repair(text: &str) -> Option<String> {
    let units: Vec<u16> = text.encode_utf16().collect();
    let mut repaired: Vec<u16> = Vec::with_capacity(units.len());
    // 单遍状态机：`in_string` 表示当前是否位于字符串字面量内部。串外的字符（包括
    // 多余的括号/逗号）一律原样复制；只有串内的转义与控制字符会被改写。
    let mut in_string = false;
    let mut index = 0usize;
    while index < units.len() {
        let ch = units[index];
        if !in_string {
            repaired.push(ch);
            if ch == b'"' as u16 {
                in_string = true;
            }
            index += 1;
            continue;
        }
        if ch == b'"' as u16 {
            repaired.push(ch);
            in_string = false;
            index += 1;
            continue;
        }
        if ch == b'\\' as u16 {
            let Some(next) = units.get(index + 1).copied() else {
                // 末尾反斜杠 → 双写。
                repaired.push(b'\\' as u16);
                repaired.push(b'\\' as u16);
                index += 1;
                continue;
            };
            if next == b'u' as u16 {
                let digits = units.get(index + 2..index + 6);
                if let Some(digits) = digits
                    && digits.iter().all(|&d| hex_value(d).is_some())
                {
                    repaired.push(b'\\' as u16);
                    repaired.push(b'u' as u16);
                    repaired.extend_from_slice(digits);
                    index += 5;
                    index += 1;
                    continue;
                }
            }
            if is_valid_json_escape(next) {
                repaired.push(b'\\' as u16);
                repaired.push(next);
                index += 1;
                index += 1;
                continue;
            }
            repaired.push(b'\\' as u16);
            repaired.push(b'\\' as u16);
            index += 1;
            continue;
        }
        if ch <= 0x1f {
            match ch {
                0x08 => push_ascii(&mut repaired, "\\b"),
                0x0c => push_ascii(&mut repaired, "\\f"),
                0x0a => push_ascii(&mut repaired, "\\n"),
                0x0d => push_ascii(&mut repaired, "\\r"),
                0x09 => push_ascii(&mut repaired, "\\t"),
                _ => push_ascii(&mut repaired, &format!("\\u{ch:04x}")),
            }
            index += 1;
            continue;
        }
        repaired.push(ch);
        index += 1;
    }
    Some(String::from_utf16_lossy(&repaired))
}

fn push_ascii(out: &mut Vec<u16>, text: &str) {
    out.extend(text.encode_utf16());
}

/// [`repair`] 认作合法的单字符转义集合：RFC 8259 的 `" \ / b f n r t` 八个。
fn is_valid_json_escape(unit: u16) -> bool {
    matches!(
        unit,
        c if c == b'"' as u16
            || c == b'\\' as u16
            || c == b'/' as u16
            || c == b'b' as u16
            || c == b'f' as u16
            || c == b'n' as u16
            || c == b'r' as u16
            || c == b't' as u16
    )
}

/// TS 的 `parseJsonWithRepair`：先严格解析；失败则修复后重试。
///
/// 关键语义（TS 源码）：修复文本与原文**不同**时，重新解析；若那次仍失败，抛的是**修复后**
/// 那次的错误；修复文本与原文相同时，抛最初的错误。
///
/// 当前 [`repair`] 恒返回 `Some`，所以 `None` 分支实际不会命中，属于防御性写法。
pub fn parse_with_repair(text: &str) -> Result<Value, ParseError> {
    match parse(text) {
        Ok(v) => Ok(v),
        Err(first) => match repair(text) {
            Some(repaired) if repaired != text => parse(&repaired),
            _ => Err(first),
        },
    }
}

/// `(result ?? {})`：仅当结果是 `null` 字面量时替换为空对象。
///
/// `NaN`/`Infinity` 在 JS 里不是 nullish，即使 Rust 侧把它们存成 `Value::Null` 也保持原样。
/// `partial::PartialKind` 只在**顶层结果**为 `null` 且由非有限数落成时给出
/// `NonFiniteNumber`，所以这里只判断整个结果，不会深入数组/对象内部。
fn apply_nullish(value: Value, kind: partial::PartialKind) -> Value {
    if value.is_null() && kind == partial::PartialKind::Value {
        Value::Object(serde_json::Map::new())
    } else {
        value
    }
}

/// TS 的 `parseStreamingJson`：把（可能未完成的）流式 JSON 解析成当前快照。
///
/// 三级回退（与 `parseStreamingJson` 逐句对应）：
/// 1. 空串或 trim 后为空 → `{}`；
/// 2. `parseJsonWithRepair`（结果原样返回：可能是 `null`/`true`/`123`/字符串）；
/// 3. 失败 → `partialParse(原文)`（`result ?? {}`）；
/// 4. 失败 → `partialParse(repairJson(原文))`（`result ?? {}`）；
/// 5. 再失败 → `{}`。
///
/// 返回的 `bool` 是 TS 不可观察的补充：`true` 表示结果来自第 2 步（严格/修复解析成功），
/// `false` 表示来自 partial-json 回退或**空输入**（空输入的 `{}` 是函数自己造的，不是解析出来的）。
/// 调用方（`kiana-ai`）只用 `value`。
pub fn parse_streaming(text: &str) -> (Value, bool) {
    // 顺序就是优先级，不能调换：严格解析成功时必须原样返回；只有它失败才允许修复，
    // 修复后仍失败才退到 partial-json（先原文、再修复文本）。空输入按 JS 的空白集合
    // 判断（U+00A0/U+FEFF 也算空白），Rust 的 `str::trim` 用的不是同一集合。
    if trim_js_whitespace(text).is_empty() {
        return (Value::Object(serde_json::Map::new()), false);
    }
    if let Ok(value) = parse_with_repair(text) {
        return (value, true);
    }
    // TS 的 `(result ?? {})`：`??` 只在结果是 null/undefined 时替换为空对象。partial-json
    // 对 `"nul"` 这类前缀返回 null，但 `"NaN"`/`"Inf"` 返回的是 **number**（Rust 侧落为
    // `Value::Null`），因此用带标注的版本区分两者。
    if let Ok((value, kind)) = partial::parse_partial_tagged(text) {
        return (apply_nullish(value, kind), false);
    }
    let repaired = repair(text).unwrap_or_else(|| text.to_string());
    if let Ok((value, kind)) = partial::parse_partial_tagged(&repaired) {
        return (apply_nullish(value, kind), false);
    }
    // 前面所有解析尝试都失败：与 TS 的 catch 分支一致返回空对象；
    // `false` 表示结果不是解析出来的。
    (Value::Object(serde_json::Map::new()), false)
}

#[cfg(test)]
// 单元测试锁定三件事：V8 风格的错误文本（含位置）、整数/浮点在 `Value` 里的表示选择、
// 以及 `normalize_numbers` 的显式栈确实能处理 10 万层深值。更完整的黄金对照在
// `tests/api_golden.rs`，不在这里重复。
mod tests {
    use super::*;

    #[test]
    fn parses_simple_values() {
        assert_eq!(parse("null").unwrap(), Value::Null);
        assert_eq!(parse("true").unwrap(), Value::Bool(true));
        assert_eq!(crate::js_object::stringify(&parse(" 12 ").unwrap()), "12");
        assert_eq!(parse("\"a\"").unwrap(), Value::String("a".into()));
        assert_eq!(
            crate::js_object::stringify(&parse("[1,2]").unwrap()),
            "[1,2]"
        );
    }

    #[test]
    fn rejects_with_v8_messages() {
        assert_eq!(
            parse("{\"a\":1,}").unwrap_err().message,
            "Expected double-quoted property name in JSON at position 7 (line 1 column 8)"
        );
        assert_eq!(
            parse("").unwrap_err().message,
            "Unexpected end of JSON input"
        );
    }

    #[test]
    fn normalize_numbers_rounds_like_js_f64() {
        use crate::js_object::stringify;
        use serde_json::Number;

        // 2^53 内的整数保持整数类型（`as_u64` 仍可用）。
        let small = normalize_numbers(Value::Number(Number::from(9_007_199_254_740_992u64)));
        assert_eq!(small.as_u64(), Some(9_007_199_254_740_992));
        assert_eq!(
            normalize_numbers(Value::Number(Number::from(-1i64))).as_i64(),
            Some(-1)
        );

        // 超出 2^53 的整数按 f64 舍入（期望值来自 node：
        // `JSON.stringify(Number(BigInt("...")))`）。
        for (input, want) in [
            (
                Value::Number(Number::from(9_007_199_254_740_993u64)),
                "9007199254740992",
            ),
            (
                Value::Number(Number::from(9_007_199_254_740_994u64)),
                "9007199254740994",
            ),
            (
                Value::Number(Number::from(1_152_921_504_606_846_976u64)),
                "1152921504606847000",
            ),
            (
                Value::Number(Number::from(9_223_372_036_854_775_807u64)),
                "9223372036854776000",
            ),
            (
                Value::Number(Number::from(u64::MAX)),
                "18446744073709552000",
            ),
            (
                Value::Number(Number::from(i64::MIN)),
                "-9223372036854776000",
            ),
            (
                Value::Number(Number::from(-9_007_199_254_740_993i64)),
                "-9007199254740992",
            ),
        ] {
            assert_eq!(stringify(&normalize_numbers(input)), want);
        }

        // 非整数 / 非有限值不动；`-0` 保留。
        let float = normalize_numbers(Value::Number(Number::from_f64(1.5).unwrap()));
        assert_eq!(float.as_f64(), Some(1.5));
        let neg_zero = normalize_numbers(Value::Number(Number::from_f64(-0.0).unwrap()));
        assert!(neg_zero.as_f64().unwrap().is_sign_negative());
        assert_eq!(stringify(&neg_zero), "0");

        // 嵌套：`parse` 已经给出 JS 键序（整数索引键在前），本函数不再动键序。
        let nested =
            normalize_numbers(crate::parse(r#"{"b":[1,18446744073709551615],"2":true}"#).unwrap());
        assert_eq!(
            stringify(&nested),
            r#"{"2":true,"b":[1,18446744073709552000]}"#
        );
        // 手工构造的 Map：`normalize_numbers` 只换数字值，迭代序保持不动。
        let mut map = serde_json::Map::new();
        map.insert("z".to_string(), Value::from(u64::MAX));
        map.insert("a".to_string(), Value::from(1));
        assert_eq!(
            stringify(&normalize_numbers(Value::Object(map))),
            r#"{"z":18446744073709552000,"a":1}"#
        );
    }

    #[test]
    fn normalize_numbers_handles_deep_values_without_recursion() {
        // 与 `js_object` 的遍历一致：显式栈，深值不爆栈。
        // `serde_json::Value` 自身的递归 `Drop` 是外部限制，故在 64 MB 栈的线程里构造。
        std::thread::Builder::new()
            .stack_size(64 * 1024 * 1024)
            .spawn(|| {
                const DEPTH: usize = 100_000;
                let text = format!(
                    "{}18446744073709551615{}",
                    "[".repeat(DEPTH),
                    "]".repeat(DEPTH)
                );
                let value = normalize_numbers(crate::parse(&text).expect("deep parse"));
                // 穿透到底取叶子：序列化本身也是显式栈。
                assert_eq!(
                    crate::js_object::stringify(&value),
                    format!(
                        "{}18446744073709552000{}",
                        "[".repeat(DEPTH),
                        "]".repeat(DEPTH)
                    )
                );
            })
            .expect("thread spawns")
            .join()
            .expect("normalize_numbers handles deep input");
    }

    #[test]
    fn repair_doubles_invalid_escapes() {
        assert_eq!(
            repair("{\"a\":\"\\url\"}").as_deref(),
            Some("{\"a\":\"\\\\url\"}")
        );
        assert_eq!(repair("plain").as_deref(), Some("plain"));
    }
}
