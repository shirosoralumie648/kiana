//! `JSON.parse` 的等价实现（严格 JSON 文法 + JS 数字语义）。
//!
//! 只给 cursor 用：`findTraces` 的 `query.cursor` 是上一页的 `JSON.stringify([startedMs, traceId])`，
//! TS 用 `JSON.parse` 读回。`serde_json`（`kiana_json::parse`）在两处与 JS 不同，而这两处都会改变
//! `findTraces` 的结果：
//!
//! 1. **浮点往返**：`serde_json` 1.0.154 未开启 `float_roundtrip` 时用 `significand * POW10`
//!    的快速算法，反序列化出的 `f64` 可能与最短往返表示相差 1 ULP。TS 的续页判据是
//!    `first.startedMs === after[0]`（严格相等），差 1 ULP 会让 cursor 永远匹配不上：同一页重复、
//!    多返回或丢 trace。这里用 `str::parse::<f64>()`（正确舍入，等价于 V8 的 `StringToDouble`）。
//! 2. **溢出字面量**：TS 的 `JSON.parse("1e400")` 得到 `Infinity`（不是错误）。`serde_json`
//!    返回 `number out of range`。这里溢出为 `±Infinity`，与 JS 一致。
//!
//! 另外，`serde_json::Value` 无法表示 `Infinity`/`NaN`，也无法表示孤立代理项（`"\ud800"`；
//! 代理项 surrogate 是 UTF-16 里用来表示 BMP 以外字符的 16 位码元，孤立时不能构成字符），
//! 因此本模块自带值类型：[`JsonValue`] 的数字是 `f64`，字符串是 UTF-16 code unit
//! （码元，JS 字符串的索引单位）序列。
//!
//! 严格 JSON 文法与 `JSON.parse` 一致：只接受 RFC 8259 的空白（空格、制表、LF、CR）、
//! 数字必须是 `-?(0|[1-9]\d*)(\.\d+)?([eE][+-]?\d+)?`（拒绝 `01`、`+1`、`.5`、`1.`、
//! `NaN`、`Infinity`）、字符串只允许 JSON 转义、对象允许重复键（后者覆盖前者，位置保持首次出现）。
//!
//! 解析用**显式栈**（不是递归下降）：V8 的 `JSON.parse` 接受任意深度的嵌套（实测 100 万层 `[]`），
//! 递归实现既会在深输入上打爆 Rust 栈，也无法复刻「解析成功、后续比较才失败」的阶段差异。
//! `JsonValue` 的 `Drop` 与 [`js_string`] 同样是迭代实现。实现中已记录的残留差别：TS 在
//! `after[0]` 的 `ToNumber` 里调 `Array#join`，V8 的 `join` 递归、约 4650 层抛 `RangeError`；
//! Rust 侧能算出同样的 `join` 文本（见 `memory.rs` 模块文档）。
//!
//! TS: `packages/telemetry/src/memory.ts` 的 `JSON.parse(query.cursor)`
//!
//! # 这个模块解决什么问题
//!
//! 生产路径上只有一处使用：`memory.rs` 的 `decode_cursor` 解析 `TraceQuery.cursor`。
//! 它不是通用 JSON 库，也不对外导出（`lib.rs` 里是 `mod jsjson;`）；新增调用点前应先确认
//! 是否真的需要 JS 语义，否则用 `kiana_json::parse` 更简单。
//!
//! # 调用链
//!
//! 1. `memory.rs::decode_cursor` 调用 [`parse`]；解析失败映射成
//!    `TelemetryError::with_name("SyntaxError", ...)`（错误名与 TS 对齐）；
//! 2. 解析结果 [`JsonValue`] 交给 `memory.rs` 的 `js_bracket_get` / `primitive_of`，
//!    复刻 `after[0]` / `after[1]` 的 JS 括号访问与类型转换；
//! 3. 数组/对象转字符串时调用 [`js_string`]（`String(value)` / `Array#join`），其中数字
//!    文本走 `jsval::js_number_text`。
//!
//! 本模块只负责“值 + 字符串化”这一层，不参与 cursor 的比较逻辑。
//!
//! # 设计原因（为什么不用 serde_json）
//!
//! 除了开头列出的两个解析差异（浮点往返、溢出为 Infinity），`serde_json::Value` 还表达不了
//! 孤立代理项与非有限数字；而 cursor 的比较必须与 V8 严格一致（`<`、`===`、`Array#join`）。
//! 因此这里自带一个最小类型。代价是只实现了 `JSON.parse` 与 `String(value)` 需要的那部分，
//! 没有通用序列化 API（`stringify` 仅在测试中使用）。
//!
//! # 注意事项
//!
//! - 错误消息文本是本模块自己的近似（[`JsonSyntaxError`]），不是 V8 的原文；调用方只应依赖
//!   错误类别/名称（`memory.rs` 统一命名为 `"SyntaxError"`）。
//! - 输入按 UTF-16 code unit 索引，错误消息里的 position 也是 code unit 偏移，不是字节偏移。
//! - 解析、`Drop`、[`js_string`] 都是迭代实现；`stringify` 是递归的，且只在测试中使用。
//! - 对象一律转换为 `"[object Object]"`（spec §22.2 的假设：JSON 解析出的对象没有自定义
//!   `toString`）。极端边界：对象若含**自有、不可调用**的 `toString` 属性（JSON 可表达，
//!   如 `{"toString":1}`），V8 的 ToPrimitive 会抛 `TypeError`，而这里仍得到
//!   `"[object Object]"`（再转数字为 `NaN`）；该差异当前没有被测试覆盖。

/// JSON 值（JS 视角：数字是 `f64`，字符串是 UTF-16 code unit）。
///
/// 与 `serde_json::Value` 的关键差异，都是 cursor 比较所需要的：
/// - `Number` 保存原始 `f64`：`JSON.parse("1e400")` 得到 `Infinity`，而 serde_json 会报
///   “number out of range”；`parse` 从不产生 `NaN`（JSON 文法里没有）。
/// - `String` 保存 `Vec<u16>`：`"\ud800"` 这样的孤立代理项可以原样保留，而 Rust `String`
///   只能存合法标量。
/// - `Object` 的键是 UTF-16 code unit 序列（可含孤立代理项；`serde_json::Value` 的
///   `String` 键表达不了），按插入顺序保存，重复键保留首次位置、使用最后值。
///
/// 这些表示让 `after[0]`/`after[1]` 的 JS 下标、严格相等与 `Array#join` 能被逐字复刻。
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum JsonValue {
    /// `null`。
    Null,
    /// `true` / `false`。
    Bool(bool),
    /// JS number（`parse` 可能产生 `±Infinity` 与 `-0`）。
    Number(f64),
    /// JS 字符串的 UTF-16 code unit 序列（可能含孤立代理项）。
    String(Vec<u16>),
    /// JS 数组。
    Array(Vec<JsonValue>),
    /// 对象的自有键按文本顺序；重复键保留首个位置并使用最后一个值（JS 对象语义）。
    Object(Vec<(Vec<u16>, JsonValue)>),
}

/// 深层嵌套的 `JsonValue` 不能靠 derive 的递归 drop glue 释放（`JSON.parse` 接受任意深度，
/// 指针链一长就会打爆调用方线程的栈）。这里用显式工作栈拆解整棵树。
///
/// 实现要点：先把当前值的子节点 `append`/`drain` 到 `pending`，再逐个弹出处理；每个节点在
/// 迭代末尾被 drop 时其子节点已被取走，所以 drop glue 只释放本层的 `Vec`，不会继续递归。
impl Drop for JsonValue {
    fn drop(&mut self) {
        let mut pending: Vec<JsonValue> = Vec::new();
        match self {
            JsonValue::Array(items) => pending.append(items),
            JsonValue::Object(entries) => {
                pending.extend(entries.drain(..).map(|(_, value)| value));
            }
            _ => return,
        }
        while let Some(mut value) = pending.pop() {
            match &mut value {
                JsonValue::Array(items) => pending.append(items),
                JsonValue::Object(entries) => {
                    pending.extend(entries.drain(..).map(|(_, value)| value));
                }
                _ => {}
            }
        }
    }
}

impl JsonValue {
    /// JS truthiness：`false`、`0`、`-0`、`NaN`、`""`、`null`、`undefined` 为假。
    ///
    /// 注意空数组与空对象在 JS 里是**真值**，所以这里返回 `true`（与 Python 等语言不同）。
    /// JSON 没有 `undefined`，因此本枚举没有该变体。
    pub(crate) fn is_truthy(&self) -> bool {
        match self {
            JsonValue::Null => false,
            JsonValue::Bool(flag) => *flag,
            JsonValue::Number(number) => *number != 0.0 && !number.is_nan(),
            JsonValue::String(units) => !units.is_empty(),
            JsonValue::Array(_) | JsonValue::Object(_) => true,
        }
    }
}

/// 解析失败：`JSON.parse` 抛 `SyntaxError`；这里返回同样的「错误类别」，消息文本取简明描述。
///
/// 只保留 `message`（`Display` 即消息文本），不区分语法错误的具体子类。`memory.rs` 在边界处
/// 把它转换成 `TelemetryError::with_name("SyntaxError", ...)`，错误名与 TS 对齐。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub(crate) struct JsonSyntaxError {
    /// 人类可读的错误描述（多数消息含 code unit 位置，未终止字符串/Unicode 转义等除外；
    /// 措辞不保证与 V8 相同）。
    pub message: String,
}

impl JsonSyntaxError {
    fn new(message: impl Into<String>) -> Self {
        JsonSyntaxError {
            message: message.into(),
        }
    }
}

/// JSON 解析器的游标状态。
///
/// 输入被转成 UTF-16 code unit 序列，`position` 是 code unit 下标（不是字节下标）。这样字符串
/// 转义、代理对与错误位置都与 JS 的字符串索引一致。
struct Parser {
    /// 待解析文本的 UTF-16 code unit。
    input: Vec<u16>,
    /// 下一个待读 code unit 的下标。
    position: usize,
}

/// 解析中的容器帧（显式栈：嵌套深度不受 Rust 调用栈限制）。
enum FrameKind {
    /// `[` … `]`：解析出的值追加到 `items`。
    Array,
    /// `{` … `}`：解析出的值按键写入 `entries`。
    Object,
}

/// 显式栈中的一层容器。
struct Frame {
    kind: FrameKind,
    /// 数组帧已解析的元素（对象帧始终为空）。
    items: Vec<JsonValue>,
    /// 对象帧的键值对（`Array` 帧始终为空）。
    entries: Vec<(Vec<u16>, JsonValue)>,
    /// 对象帧当前等待值的键。
    ///
    /// 不变量：对象帧在“刚读完 `key:`、尚未附着值”时一定是 `Some`，[`Parser::attach`]
    /// 依赖这一点；数组帧始终是 `None`。
    key: Option<Vec<u16>>,
}

impl Parser {
    fn peek(&self) -> Option<u16> {
        self.input.get(self.position).copied()
    }

    fn peek_is(&self, byte: u8) -> bool {
        self.peek() == Some(u16::from(byte))
    }

    fn next(&mut self) -> Option<u16> {
        let unit = self.peek()?;
        self.position += 1;
        Some(unit)
    }

    fn expect(&mut self, byte: u8) -> Result<(), JsonSyntaxError> {
        match self.next() {
            Some(actual) if actual == u16::from(byte) => Ok(()),
            _ => Err(self.unexpected()),
        }
    }

    /// 构造“意外 token”错误；position 回退一格，指向刚读到的字符。
    fn unexpected(&self) -> JsonSyntaxError {
        JsonSyntaxError::new(format!(
            "Unexpected token in JSON at position {}",
            self.position.saturating_sub(1)
        ))
    }

    /// JSON 空白：`\t`、`\n`、`\r`、空格（其他 Unicode 空白**不**允许）。
    ///
    /// JSON 文法只认这四个字符；`\u{00a0}`（NBSP）、`\u{feff}`（BOM）等在其他语言里算空白
    /// 的字符，在 `JSON.parse` 里都是语法错误，这里保持一致。
    fn skip_whitespace(&mut self) {
        while let Some(unit) = self.peek() {
            match unit {
                0x0009 | 0x000a | 0x000d | 0x0020 => self.position += 1,
                _ => break,
            }
        }
    }

    /// 依次消费 `literal` 的每个 UTF-16 单元并比对。
    ///
    /// 调用方只 `peek` 了首字符、没有前进；因此这里必须从首字符开始消费，不能跳过。
    fn parse_literal(
        &mut self,
        literal: &str,
        value: JsonValue,
    ) -> Result<JsonValue, JsonSyntaxError> {
        for expected in literal.encode_utf16() {
            if self.next() != Some(expected) {
                return Err(self.unexpected());
            }
        }
        Ok(value)
    }

    /// 读对象成员的键与冒号，把键放进当前帧。
    ///
    /// 允许键与冒号之间出现 JSON 空白；读完把键暂存到 `frame.key`，由后续的 [`Parser::attach`]
    /// 消费。
    fn read_key(&mut self, frame: &mut Frame) -> Result<(), JsonSyntaxError> {
        self.skip_whitespace();
        self.expect(b'"')?;
        let key = self.parse_string_body()?;
        self.skip_whitespace();
        self.expect(b':')?;
        frame.key = Some(key);
        Ok(())
    }

    /// 把一个已解析的值并入当前帧。
    ///
    /// 对象帧必须持有 pending key（见 [`Frame::key`] 的不变量），否则是内部错误。重复键用
    /// 线性查找定位：值覆盖、位置保持首次出现；因此含 n 个键的对象最坏 O(n²)，这是保持
    /// JS 对象语义下的简单实现。
    fn attach(&mut self, frame: &mut Frame, value: JsonValue) {
        match frame.kind {
            FrameKind::Array => frame.items.push(value),
            FrameKind::Object => {
                let key = frame
                    .key
                    .take()
                    .expect("object frames always hold a pending key");
                // 重复键：值覆盖，位置保持首次出现（`JSON.parse` 的 [[DefineOwnProperty]] 语义）。
                match frame
                    .entries
                    .iter_mut()
                    .find(|(existing, _)| *existing == key)
                {
                    Some(entry) => entry.1 = value,
                    None => frame.entries.push((key, value)),
                }
            }
        }
    }
}

/// `JSON.parse(text)`。
///
/// 用显式栈而不是递归：V8 的 `JSON.parse` 不是「递归深度 = 嵌套深度」，实测可接受数十万层
/// 嵌套（只有随后的 `ToPrimitive` / `Array#join` 才在数千层处抛 `RangeError`）。递归下降
/// 解析器既会在大深度输入上打爆 Rust 栈，也无法复刻「解析成功、比较阶段才失败」的阶段差异。
pub(crate) fn parse(text: &str) -> Result<JsonValue, JsonSyntaxError> {
    let mut parser = Parser {
        input: text.encode_utf16().collect(),
        position: 0,
    };
    // 显式容器栈：每个尚未闭合的 `[` / `{` 占一层，栈顶是当前正在填充的容器。
    let mut stack: Vec<Frame> = Vec::new();
    parser.skip_whitespace();

    'value: loop {
        parser.skip_whitespace();
        // 值的起始 token；容器直接压栈并继续读下一个值（空容器则直接构造值）。
        let mut value = match parser.peek() {
            Some(unit) if unit == u16::from(b'{') => {
                parser.position += 1;
                parser.skip_whitespace();
                if parser.peek_is(b'}') {
                    parser.position += 1;
                    JsonValue::Object(Vec::new())
                } else {
                    let mut frame = Frame {
                        kind: FrameKind::Object,
                        items: Vec::new(),
                        entries: Vec::new(),
                        key: None,
                    };
                    parser.read_key(&mut frame)?;
                    stack.push(frame);
                    continue 'value;
                }
            }
            Some(unit) if unit == u16::from(b'[') => {
                parser.position += 1;
                parser.skip_whitespace();
                if parser.peek_is(b']') {
                    parser.position += 1;
                    JsonValue::Array(Vec::new())
                } else {
                    stack.push(Frame {
                        kind: FrameKind::Array,
                        items: Vec::new(),
                        entries: Vec::new(),
                        key: None,
                    });
                    continue 'value;
                }
            }
            Some(unit) if unit == u16::from(b'"') => {
                parser.position += 1;
                JsonValue::String(parser.parse_string_body()?)
            }
            Some(unit) if unit == u16::from(b't') => {
                parser.parse_literal("true", JsonValue::Bool(true))?
            }
            Some(unit) if unit == u16::from(b'f') => {
                parser.parse_literal("false", JsonValue::Bool(false))?
            }
            Some(unit) if unit == u16::from(b'n') => {
                parser.parse_literal("null", JsonValue::Null)?
            }
            Some(unit)
                if unit == u16::from(b'-')
                    || (u16::from(b'0')..=u16::from(b'9')).contains(&unit) =>
            {
                parser.parse_number()?
            }
            _ => {
                return Err(JsonSyntaxError::new(format!(
                    "Unexpected token in JSON at position {}",
                    parser.position
                )));
            }
        };

        // 把值并入栈顶容器；关闭容器时把结果当作新值继续向外合并。
        loop {
            let Some(frame) = stack.last_mut() else {
                // 栈空 = 最外层值已完成：只允许尾随 JSON 空白，否则 JSON.parse 也拒绝。
                parser.skip_whitespace();
                if parser.position != parser.input.len() {
                    return Err(JsonSyntaxError::new(format!(
                        "Unexpected non-whitespace character after JSON at position {}",
                        parser.position
                    )));
                }
                return Ok(value);
            };
            parser.attach(frame, value);
            parser.skip_whitespace();
            match parser.next() {
                Some(unit) if unit == u16::from(b',') => {
                    // 对象的下一个成员要先读键与冒号；数组则直接去解析下一个值。
                    if matches!(frame.kind, FrameKind::Object) {
                        parser.read_key(frame)?;
                    }
                    continue 'value;
                }
                Some(unit) if unit == u16::from(b']') && matches!(frame.kind, FrameKind::Array) => {
                    let frame = stack.pop().expect("non-empty stack");
                    value = JsonValue::Array(frame.items);
                }
                Some(unit)
                    if unit == u16::from(b'}') && matches!(frame.kind, FrameKind::Object) =>
                {
                    let frame = stack.pop().expect("non-empty stack");
                    value = JsonValue::Object(frame.entries);
                }
                _ => return Err(parser.unexpected()),
            }
        }
    }
}

impl Parser {
    /// 调用方已消费开引号；读到闭引号为止。
    ///
    /// 返回 UTF-16 code unit 序列（不是 `String`）：`\uD800` 这类孤立代理项在 JS 字符串里
    /// 合法，但无法表示成 Rust `String`。代理对会被规范化为两个连续 code unit；孤立高代理
    /// 后跟非低代理的 `\u` 转义时，先回退位置，让外层循环按普通转义处理它。
    ///
    /// # 错误
    ///
    /// - 输入结束（未闭合字符串或转义不完整）；
    /// - 非法的反斜杠转义；
    /// - 字符串内出现未转义的 `< 0x20` 控制字符。
    fn parse_string_body(&mut self) -> Result<Vec<u16>, JsonSyntaxError> {
        let mut units: Vec<u16> = Vec::new();
        loop {
            let unit = self.next().ok_or_else(|| {
                JsonSyntaxError::new("Unterminated string in JSON at end of input")
            })?;
            if unit == u16::from(b'"') {
                return Ok(units);
            }
            if unit == u16::from(b'\\') {
                let escape = self.next().ok_or_else(|| {
                    JsonSyntaxError::new("Unterminated string in JSON at end of input")
                })?;
                let byte = u8::try_from(escape).unwrap_or(0);
                match byte {
                    b'"' => units.push(u16::from(b'"')),
                    b'\\' => units.push(u16::from(b'\\')),
                    b'/' => units.push(u16::from(b'/')),
                    b'b' => units.push(0x0008),
                    b'f' => units.push(0x000c),
                    b'n' => units.push(0x000a),
                    b'r' => units.push(0x000d),
                    b't' => units.push(0x0009),
                    b'u' => {
                        let high = self.parse_hex4()?;
                        // 代理对：高代理 + 后续 `\u` 低代理 → 两个 code unit 原样保留。
                        if (0xd800..=0xdbff).contains(&high)
                            && self.peek_is(b'\\')
                            && self.input.get(self.position + 1) == Some(&u16::from(b'u'))
                        {
                            // 先记下反斜杠位置：若后面不是低代理，就回到这里重新按普通转义
                            // 解析，而不是丢掉那段 `\uXXXX`。
                            let saved = self.position;
                            self.position += 2;
                            let low = self.parse_hex4()?;
                            if (0xdc00..=0xdfff).contains(&low) {
                                units.push(high);
                                units.push(low);
                                continue;
                            }
                            self.position = saved;
                        }
                        units.push(high);
                    }
                    _ => {
                        return Err(JsonSyntaxError::new(format!(
                            "Bad escaped character in JSON at position {}",
                            self.position.saturating_sub(1)
                        )));
                    }
                }
                continue;
            }
            // JSON 字符串里不允许裸控制字符（必须写成转义）。
            if unit < 0x20 {
                return Err(JsonSyntaxError::new(format!(
                    "Bad control character in string literal in JSON at position {}",
                    self.position.saturating_sub(1)
                )));
            }
            units.push(unit);
        }
    }

    /// 读 4 个十六进制数字并合成一个 UTF-16 code unit。
    ///
    /// 调用方已消费 `\u`。只接受 ASCII `0-9`/`a-f`/`A-F`；其他字符或提前结束都是语法错误。
    fn parse_hex4(&mut self) -> Result<u16, JsonSyntaxError> {
        let mut value: u16 = 0;
        for _ in 0..4 {
            let unit = self.next().ok_or_else(|| {
                JsonSyntaxError::new("Unterminated Unicode escape in JSON string")
            })?;
            let digit = match unit {
                unit if (u16::from(b'0')..=u16::from(b'9')).contains(&unit) => {
                    unit - u16::from(b'0')
                }
                unit if (u16::from(b'a')..=u16::from(b'f')).contains(&unit) => {
                    unit - u16::from(b'a') + 10
                }
                unit if (u16::from(b'A')..=u16::from(b'F')).contains(&unit) => {
                    unit - u16::from(b'A') + 10
                }
                _ => {
                    return Err(JsonSyntaxError::new(format!(
                        "Bad Unicode escape in JSON at position {}",
                        self.position.saturating_sub(1)
                    )));
                }
            };
            value = value * 16 + digit;
        }
        Ok(value)
    }

    fn peek_is_digit(&self) -> bool {
        matches!(self.peek(), Some(unit) if (u16::from(b'0')..=u16::from(b'9')).contains(&unit))
    }

    /// JSON 数字：`-?(0|[1-9]\d*)(\.\d+)?([eE][+-]?\d+)?`，按 JS 语义转 `f64`（溢出为
    /// `±Infinity`，下溢为 `±0`，`-0` 保留）。
    ///
    /// 文法逐字符校验（不接受前导零、`+1`、`.5`、`1.`），与 `JSON.parse` 一致；任何一步失败
    /// 都返回语法错误，而不是像 `Number()` 那样放宽到 `01`、`+1`、`.5`、`1.`、`Infinity`
    /// 等非 JSON 数字字面量（`parseFloat` 才接受合法前缀）。
    fn parse_number(&mut self) -> Result<JsonValue, JsonSyntaxError> {
        let start = self.position;
        // 阶段 1：符号。
        if self.peek_is(b'-') {
            self.position += 1;
        }
        // 阶段 2：整数部分。`0` 后面不能再跟数字（拒绝 `01`）；非零首位后连续读数字。
        match self.peek() {
            Some(unit) if unit == u16::from(b'0') => {
                self.position += 1;
                if self.peek_is_digit() {
                    return Err(JsonSyntaxError::new(format!(
                        "Unexpected number in JSON at position {start}"
                    )));
                }
            }
            Some(unit) if (u16::from(b'1')..=u16::from(b'9')).contains(&unit) => {
                while self.peek_is_digit() {
                    self.position += 1;
                }
            }
            _ => return Err(self.unexpected()),
        }
        // 阶段 3：可选小数部分，点后至少一位数字。
        if self.peek_is(b'.') {
            self.position += 1;
            if !self.peek_is_digit() {
                return Err(self.unexpected());
            }
            while self.peek_is_digit() {
                self.position += 1;
            }
        }
        // 阶段 4：可选指数部分，`e`/`E` + 可选符号 + 至少一位数字。
        if self.peek_is(b'e') || self.peek_is(b'E') {
            self.position += 1;
            if self.peek_is(b'+') || self.peek_is(b'-') {
                self.position += 1;
            }
            if !self.peek_is_digit() {
                return Err(self.unexpected());
            }
            while self.peek_is_digit() {
                self.position += 1;
            }
        }

        // 数字 token 只由上面校验过的 ASCII 字符组成，因此从 UTF-16 退回 String 是无损的。
        let literal: String = String::from_utf16_lossy(&self.input[start..self.position]);
        // `str::parse` 是正确舍入的（与 V8 的 StringToDouble 一致）；溢出得到 ±Infinity。
        let number = literal.parse::<f64>().map_err(|_| {
            JsonSyntaxError::new(format!("Unexpected number in JSON at position {start}"))
        })?;
        Ok(JsonValue::Number(number))
    }
}
/// `String(value)`（数组 `join(",")`、对象 `"[object Object]"`）。
///
/// 迭代实现：`Array#join` 在 V8 里是递归的（数千层嵌套抛 `RangeError`），Rust 侧不能跟着
/// 递归 —— `cursor` 是外部输入，深度不受控。这里对任意深度都算出同样的 `join` 文本；
/// 「超过 V8 栈深度的 cursor 在 TS 里抛 RangeError」这一差别记录在模块文档里。
///
/// # 语义细节
///
/// - 本实现对对象一律输出 `"[object Object]"`（spec §22.2 的假设；TS 在极端边界会抛
///   `TypeError`，见模块文档）；
/// - 数组是 `join(",")`：`null` 元素被跳过但分隔符保留（`[1,null,2]` → `"1,,2"`），嵌套
///   数组先转成自己的 join 文本；
/// - 数字走 `jsval::js_number_text`（`String(number)` 语义：`NaN` → `"NaN"`、`Infinity` →
///   `"Infinity"`、`-0` → `"0"`）。
///
/// 复杂度：只遍历数组与标量；对象直接输出常量、内部节点不访问，因此总时间与“访问到的节点数
/// + 输出长度”线性，不会为每一层构造中间字符串。
pub(crate) fn js_string(value: &JsonValue) -> Vec<u16> {
    let mut out: Vec<u16> = Vec::new();
    // 工作栈任务：Value = 把该值转成字符串；Separator = 输出一个逗号。逗号由父数组在压栈
    // 时插入，这样嵌套数组展开后分隔符数量仍然正确。
    enum Task<'a> {
        Value(&'a JsonValue),
        Separator,
    }
    let mut stack: Vec<Task<'_>> = vec![Task::Value(value)];
    while let Some(task) = stack.pop() {
        match task {
            Task::Separator => out.push(u16::from(b',')),
            Task::Value(value) => match value {
                JsonValue::Null => out.extend("null".encode_utf16()),
                JsonValue::Bool(flag) => out.extend(flag.to_string().encode_utf16()),
                JsonValue::Number(number) => {
                    out.extend(crate::jsval::js_number_text(*number).encode_utf16());
                }
                JsonValue::String(units) => out.extend_from_slice(units),
                JsonValue::Object(_) => out.extend("[object Object]".encode_utf16()),
                JsonValue::Array(items) => {
                    // `join` 跳过 null/undefined 元素，但保留它们之间的分隔符：`[1,null,2]` → "1,,2"。
                    // 逆序压栈，pop 时按原序输出。
                    for index in (0..items.len()).rev() {
                        if !matches!(items[index], JsonValue::Null) {
                            stack.push(Task::Value(&items[index]));
                        }
                        if index > 0 {
                            stack.push(Task::Separator);
                        }
                    }
                }
            },
        }
    }
    out
}

/// `JSON.stringify(value)` 的紧凑形式（仅本模块测试用来与 Node 的 `JSON.stringify` 对照）。
///
/// 这是**测试专用**的递归实现（数组/对象逐层递归），只处理 fixture 的有限深度；生产路径
/// 不使用它，也没有 [`js_string`] 的任意深度保证。
///
/// 语义细节：
/// - 非有限数字写成 `null`（`JSON.stringify(Infinity) === "null"`）；
/// - 合法代理对输出为实际字符，孤立代理项写成 `\udXXX`（ES2019 起 well-formed stringify）；
/// - 对象键按存储顺序输出。注意它**没有**复刻 `JSON.stringify` 对整数样键的提前排序
///   （JS 对象枚举会把 `"0"`、`"1"` 这类键按数值升序前置）；现有 fixture 不含会触发该
///   重排的对象，生产路径的键序由 `jsval::js_entries` 处理。
#[cfg(test)]
fn stringify(value: &JsonValue) -> String {
    fn escape(units: &[u16], out: &mut String) {
        out.push('"');
        let mut index = 0;
        while index < units.len() {
            let unit = units[index];
            match unit {
                0x0022 => out.push_str("\\\""),
                0x005c => out.push_str("\\\\"),
                0x0008 => out.push_str("\\b"),
                0x000c => out.push_str("\\f"),
                0x000a => out.push_str("\\n"),
                0x000d => out.push_str("\\r"),
                0x0009 => out.push_str("\\t"),
                unit if unit < 0x20 => out.push_str(&format!("\\u{unit:04x}")),
                // 合法代理对按 `char` 输出（`JSON.stringify` 的 well-formed 行为）。
                0xd800..=0xdbff
                    if units
                        .get(index + 1)
                        .is_some_and(|low| (0xdc00..=0xdfff).contains(low)) =>
                {
                    let low = units[index + 1];
                    let code =
                        0x10000 + ((u32::from(unit) - 0xd800) << 10) + (u32::from(low) - 0xdc00);
                    out.push(char::from_u32(code).expect("valid scalar"));
                    index += 1;
                }
                // 孤立代理项写成 `\udXXX`（`JSON.stringify` 从 ES2019 起转义它们）。
                0xd800..=0xdfff => out.push_str(&format!("\\u{unit:04x}")),
                unit => out.push(char::from_u32(u32::from(unit)).expect("BMP scalar")),
            }
            index += 1;
        }
        out.push('"');
    }

    match value {
        JsonValue::Null => "null".to_string(),
        JsonValue::Bool(flag) => flag.to_string(),
        // `JSON.stringify` 对非有限数字输出 `null`（`String(Infinity)` 才是 "Infinity"）。
        JsonValue::Number(number) if !number.is_finite() => "null".to_string(),
        JsonValue::Number(number) => crate::jsval::js_number_text(*number),
        JsonValue::String(units) => {
            let mut out = String::new();
            escape(units, &mut out);
            out
        }
        JsonValue::Array(items) => {
            let mut out = String::from("[");
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                out.push_str(&stringify(item));
            }
            out.push(']');
            out
        }
        JsonValue::Object(entries) => {
            let mut out = String::from("{");
            for (index, (key, item)) in entries.iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                escape(key, &mut out);
                out.push(':');
                out.push_str(&stringify(item));
            }
            out.push('}');
            out
        }
    }
}

#[cfg(test)]
mod tests {
    // 测试策略（不逐条注释断言）：
    // - `stringify` 只存在于测试中，用来把本模块解析出的值转成与 Node `JSON.stringify`
    //   相同形状，从而做“接受/拒绝 + 值文本”的差分对照；
    // - fixture 由 Node 26 录制：`tests/fixtures/json_parse_vectors.json`（731 条固定向量）
    //   与 `json_fuzz.json`（1499 条：1200 个随机值 + 300 个单字符变异，剔除 1 条孤立
    //   代理项后；确定性 PRNG）；
    // - `parses_like_json_parse` 锁定本模块有意偏离 serde_json 的两点（正确舍入、溢出为
    //   Infinity），以及重复键与孤立代理项语义；
    // - 深嵌套测试锁定“解析、String()、drop 都不递归”的承诺。
    use super::*;

    fn number(value: &JsonValue) -> f64 {
        match value {
            JsonValue::Number(number) => *number,
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn parses_like_json_parse() {
        assert!(!parse("null").unwrap().is_truthy());
        assert_eq!(number(&parse("1e400").unwrap()), f64::INFINITY);
        assert_eq!(number(&parse("-1e400").unwrap()), f64::NEG_INFINITY);
        assert_eq!(number(&parse("1e-400").unwrap()), 0.0);
        assert!(number(&parse("-1e-400").unwrap()).is_sign_negative());
        assert_eq!(
            number(&parse("1700000000000.6575").unwrap()).to_bits(),
            1_700_000_000_000.657_5_f64.to_bits()
        );
        // 285/2000 的随机 f64 在 serde_json 的快速路径上会差 1 ULP；这里必须精确。
        for value in [
            4.878569930398358e-20,
            1.3420630559022735e-96,
            1.4790774848363597e-174,
        ] {
            let text = crate::jsval::js_number_text(value);
            let parsed = parse(&text).expect("number");
            assert_eq!(number(&parsed).to_bits(), value.to_bits(), "{text}");
        }
        assert!(parse("01").is_err());
        assert!(parse("+1").is_err());
        assert!(parse(".5").is_err());
        assert!(parse("1.").is_err());
        assert!(parse("Infinity").is_err());
        assert!(parse("NaN").is_err());
        assert!(parse("'a'").is_err());
        assert!(parse("[\"a\",]").is_err());
        assert!(parse("").is_err());
        assert!(parse("1 2").is_err());
        // 重复键：值取后者，位置取首次出现。
        let parsed = parse("{\"a\":1,\"a\":2,\"b\":3}").unwrap();
        let JsonValue::Object(entries) = &parsed else {
            panic!("object");
        };
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].0, "a".encode_utf16().collect::<Vec<_>>());
        assert_eq!(number(&entries[0].1), 2.0);
        // 孤立代理项原样保留（`"\\ud800"` 是合法的 JSON.parse 输入）。
        let parsed = parse("\"\\ud800\"").unwrap();
        let JsonValue::String(units) = &parsed else {
            panic!("string");
        };
        assert_eq!(*units, vec![0xd800]);
        let parsed = parse("\"\\ud83d\\ude00\"").unwrap();
        let JsonValue::String(units) = &parsed else {
            panic!("string");
        };
        assert_eq!(*units, vec![0xd83d, 0xde00]);
    }

    /// 与 Node 26 的 `JSON.parse` 逐条对照：接受/拒绝一致（`ok` 里的值是
    /// `JSON.stringify(JSON.parse(input))`，也用于校验数字文本）。
    #[test]
    fn differential_against_json_parse() {
        let raw = include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/json_parse_vectors.json"
        ));
        let vectors: serde_json::Map<String, serde_json::Value> =
            serde_json::from_str(raw).expect("json_parse_vectors.json");
        assert!(vectors.len() > 700);
        for (input, expected) in &vectors {
            let parsed = parse(input);
            match expected.get("ok") {
                Some(serde_json::Value::String(wanted)) => {
                    let value = parsed.unwrap_or_else(|error| panic!("{input:?}: {error}"));
                    assert_eq!(stringify(&value), *wanted, "{input:?}");
                }
                Some(_) => panic!("unexpected expectation for {input:?}"),
                None => {
                    assert!(parsed.is_err(), "{input:?} should be a SyntaxError");
                }
            }
        }
    }

    /// `JSON.parse` 在 V8 里接受极深的嵌套（实测 100 万层 `[]` 仍成功），因此本实现用显式栈
    /// 解析，深度不受 Rust 调用栈限制；解析、`String()`、drop 都不递归。
    ///
    /// 差别：TS 在随后的 `after[0]` 强制转换（`Array#join`）里才因 V8 自身栈深度抛
    /// `RangeError`（约 4650 层），Rust 侧能算出同样的 `join` 文本；见 `memory.rs` 模块文档。
    #[test]
    fn deep_nesting_parses_without_recursion() {
        for depth in [10_000usize, 200_000] {
            let deep = format!("{}{}", "[".repeat(depth), "]".repeat(depth));
            let value = parse(&deep).expect("JSON.parse accepts deep nesting");
            // `String(value)`：每层空数组 join 出空串，多层仍是空串。
            assert_eq!(js_string(&value), Vec::<u16>::new());
            drop(value);
        }
        let objects = format!("{}1{}", "{\"a\":".repeat(20_000), "}".repeat(20_000));
        let value = parse(&objects).expect("deep objects parse");
        assert_eq!(
            String::from_utf16_lossy(&js_string(&value)),
            "[object Object]"
        );
        let shallow = format!("{}{}", "[".repeat(64), "]".repeat(64));
        assert!(parse(&shallow).is_ok());
    }

    /// 随机 JSON（含变异后的非法输入）与 Node 26 的 `JSON.parse` 逐条对照：接受/拒绝一致，
    /// 且接受时 `JSON.stringify(JSON.parse(input))` 逐字相同。
    ///
    /// fixture 由脚本生成（确定性 PRNG）：1200 个随机值 + 300 个单字符变异，`null` 期望表示
    /// `JSON.parse` 抛 `SyntaxError`。含孤立代理项的唯一一条被剔除（`serde_json` 无法把孤立
    /// 代理项读进 fixture；孤立代理项本身由 `parses_like_json_parse` 覆盖）。
    #[test]
    fn differential_fuzz_against_json_parse() {
        let raw = include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/json_fuzz.json"
        ));
        let vectors: Vec<(String, Option<String>)> =
            serde_json::from_str(raw).expect("json_fuzz.json");
        assert_eq!(vectors.len(), 1499);
        let mut accepted = 0;
        let mut rejected = 0;
        for (input, expected) in &vectors {
            match expected {
                Some(wanted) => {
                    let value = parse(input)
                        .unwrap_or_else(|error| panic!("{input:?} should parse: {error}"));
                    assert_eq!(&stringify(&value), wanted, "{input:?}");
                    accepted += 1;
                }
                None => {
                    assert!(parse(input).is_err(), "{input:?} should be a SyntaxError");
                    rejected += 1;
                }
            }
        }
        assert!(accepted > 1000 && rejected > 100);
    }

    /// `String()` 的多层数组行为（`join` 跳过 null/undefined 但保留分隔符）。
    #[test]
    fn nested_array_join_matches_javascript() {
        for (input, expected) in [
            ("[]", ""),
            ("[[]]", ""),
            ("[[],[]]", ","),
            ("[1,null,2]", "1,,2"),
            ("[[1,[2,[3]]]]", "1,2,3"),
            ("[[null]]", ""),
            ("[0]", "0"),
            ("[[0]]", "0"),
            ("[true,false]", "true,false"),
            ("[[\"a\",[\"b\"]]]", "a,b"),
            ("[[[1,2],[3]],4]", "1,2,3,4"),
        ] {
            let value = parse(input).expect("parses");
            assert_eq!(
                String::from_utf16_lossy(&js_string(&value)),
                expected,
                "String({input})"
            );
        }
    }

    #[test]
    fn string_conversion_matches_javascript() {
        let value = parse("[[null,1,\"a\"],true,{}]").unwrap();
        assert_eq!(
            String::from_utf16_lossy(&js_string(&value)),
            ",1,a,true,[object Object]"
        );
    }
}
