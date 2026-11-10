//! `partial-json@0.1.7` 的自写移植（MIT, © Muspi Merol）。
//!
//! 源文件：`node_modules/partial-json/dist/index.js`（220 行）+ `dist/options.js`（72 行）。
//! 语义是「宽容解析不完整 JSON」，被 `parseStreamingJson` 作为第 3/4 级回退使用。
//!
//! # 与 TS 的对应关系
//!
//! - `Allow` 位掩码完整保留（[`allow`] 模块），但 `parse_partial` 只用 `allow::ALL`——这正是
//!   `parseStreamingJson` 的调用方式；保留位掩码是为了与源码结构一一对应，且后续若有调用方
//!   需要部分类型可直接传参。
//! - 输入先 `.trim()`（JS 的 `trim`，即 ECMAScript WhiteSpace + LineTerminator）再解析；
//!   但**空串错误消息用的是未 trim 的原文**（`"   "` → `"    is empty"`）。
//! - `parseAny` 的判定顺序、前缀匹配的长度条件、`parseStr` 的补引号回退、
//!   `parseObj`/`parseArr` 吞掉内部错误返回已收集部分、`parseNum` 的 `lastIndexOf("e")`
//!   重试，全部逐句移植（`parseNum` 在找不到 `e` 时的错误文本有两处极小的内部差异，
//!   见该函数注释；它们不会从公开入口逃出）。
//! - 三类错误（`PartialJSON`/`MalformedJSON`/普通 `Error`）在消息后追加
//!   ` at position ${index}`；错误文本保留 JS `String(e)` 的 `SyntaxError: …` 前缀，因为
//!   `parseNum` 里是 `throwMalformedError(String(e))`。
//! - 第四类 [`errors::PartialErrorKind::Syntax`]：`parseStr` 的第二次 `JSON.parse` 不在 `try` 里，
//!   抛出的 V8 `SyntaxError` 原样逃逸（**没有** ` at position N` 后缀）。
//! - `"__proto__"` 键：TS 里 `obj[key] = value` 会设置原型而不是自有属性，因此该键被静默
//!   丢弃。Rust 的 `Map` 没有原型，这里显式丢弃以对齐 fixture（与 `JSON.parse` 相反！）。
//! - **键序**：`obj[key] = value` 产生的是普通 JS 对象，枚举序（`Object.keys`/`Map` 迭代）
//!   永远是「整数索引键升序 + 其余插入序」，且每一层都如此。`serde_json` 的
//!   `preserve_order` 只保插入序，因此 `Parser::parse_any` 在返回前就地重排整棵树
//!   （`crate::js_object` 的 `reorder_in_place`，显式栈）。重复键沿用 JS 的
//!   `obj[key] = value` 语义：值取最后一个、位置取第一个。
//!
//! # 与 TS 的被迫偏差
//!
//! 非有限数（`NaN`/`±Infinity`）在 TS 里是 number；Rust 的 `serde_json::Number` 存不下，
//! 统一落为 [`serde_json::Value::Null`]（可见 JSON 文本一致，`JSON.stringify(NaN) === "null"`）。
//!
//! # 在本 crate 中的位置
//!
//! 上游是 [`crate::js_parse::parse_streaming`]（TS 的 `parseStreamingJson`）：严格解析与
//! `repair` 都失败后，它用本模块的 [`parse_partial_tagged`] 依次解析原文与修复后文本，
//! 作为第 3/4 级回退。本模块在 `kiana-json` 之外没有调用方（测试除外）；上游
//! `parse_streaming` 本身被 `kiana-ai` 的 `parse_streaming_json` 使用。本模块只负责「把不完整
//! 文本读成当前快照」，不做修复、不做类型校验，也不保证结果一定是对象。
//!
//! # 执行流程
//!
//! 1. [`parse`]/[`parse_tagged`] 先用 `trim_js_whitespace` 判空并裁掉首尾空白，再把结果
//!    展开成 UTF-16 code unit 序列（JS 字符串的索引单位）。
//! 2. `Parser::parse_any` 取第一个值：原子（字符串/数字/字面量）直接返回；`{`/`[` 则进入
//!    `Parser::parse_container` 的显式栈。
//! 3. 每个容器帧按 `ObjState`（对象）或「下一个元素」（数组）推进；子容器完成后作为
//!    普通值回填父帧。
//! 4. 某一层读不下去时，按该层 `Allow` 位决定「返回已收集的部分容器」还是「抛出该层的
//!    收尾错误」；错误沿栈向上传递，并在每一层重复同样的判断。
//! 5. 顶层容器解析完成后，用 `crate::js_object::reorder_in_place` 就地重排整棵树，模拟
//!    JS 对象的键枚举序；带标注入口再根据 `non_finite` 区分顶层非有限数。
//!
//! # 设计原因
//!
//! - **显式栈而不是递归**：TS 用 `parseObj`/`parseArr` 相互递归，Rust 固定栈在约 1.6 万层
//!   就会 abort（详见 `ARCHITECTURE.md` §8 第 33 条）；流式输入由模型控制，深度不可信，
//!   因此容器全部用 `Vec<Frame>` 迭代。
//! - **UTF-16 而不是字节**：TS 的 `index`、`substring`、`lastIndexOf` 都以 UTF-16 code unit
//!   计数；`starts_with_at`/`remaining_is_prefix_of` 也按 code unit 比较，才能与 TS 的
//!   前缀判定和错误位置一致。
//! - **只按前缀读一个值**：`partial-json` 的目标是流式快照，不要求输入完整；字符串、字面量
//!   与容器不检查第一个值之后的剩余文本，因此 `{"a":1} trailing` 会成功返回 `{"a":1}`。
//!   顶层数字例外：`parse_num` 的 `index == 0` 分支用 `JSON.parse` 解析整串，`1 2` 报
//!   `Malformed`（`1e` 经 `lastIndexOf("e")` 重试返回 `1`）。
//! - **保留错误分类**：调用方无法从字符串可靠区分「还不完整」与「已经畸形」，因此保留
//!   `PartialJSON`/`MalformedJSON`/`Error`/`SyntaxError` 四类，便于测试与排障。
//!
//! # 注意事项
//!
//! - `allow` 位掩码表示「允许哪些类型不完整」，不是「允许解析哪些类型」：位为 0 时，完整
//!   的字面量/数字/字符串仍正常解析，只有读到一半时的行为不同。
//! - 位置字段是**裁剪后输入**的 UTF-16 游标（0-based），不是原串字节偏移，也不是行列号；
//!   部分字面量分支会一次前进固定长度，游标可能短暂超过输入长度（与 TS 相同）。
//! - 容器内部的错误在 `Allow` 命中时会被**吞掉**并退化为部分结果；这是 TS 的 `catch`
//!   语义，不是本实现的优化。
//! - [`PartialKind`] 只描述**顶层结果**：数组里出现 `Infinity` 时顶层仍是数组，种类为
//!   [`PartialKind::Value`]；判断必须看 `parse_tagged` 的返回值，不能递归检查 `Value`。
//! - `serde_json::Value` 的 `Drop` 仍是递归的；本模块的显式栈只保证解析过程不爆栈，
//!   与 `crate::js_object` 模块文档中的说明一致。
//!
//! # 阅读顺序
//!
//! `allow`/`errors`（数据模型）→ `parse`/`parse_tagged`（入口）→ `Parser::parse_any` →
//! `parse_str`/`parse_num`（原子）→ `advance_frame`/`accept_value`/`frame_error`（容器）。

use serde_json::Value;

use crate::js_num::trim_js_whitespace;
use crate::js_parse::partial::errors::PartialError;

/// `partial-json` 的 `Allow` 位掩码（`dist/options.js` 逐项对应）。
///
/// 每一位表示「该类型允许不完整」；判断统一写成 `allow & bit != 0`。例如
/// `allow::ALL & !allow::STR` 表示「其余类型仍可部分解析，但字符串必须完整」。
/// 位值是公开契约（fixture 与测试锁定），不要重排或合并。
pub mod allow {
    /// allow partial strings like `"hello \u12` to be parsed as `"hello "`
    pub const STR: u32 = 0b000000001;
    /// allow partial numbers like `123.` to be parsed as `123`
    pub const NUM: u32 = 0b000000010;
    /// allow partial arrays like `[1, 2,` to be parsed as `[1, 2]`
    pub const ARR: u32 = 0b000000100;
    /// allow partial objects like `{"a": 1, "b":` to be parsed as `{"a": 1}`
    pub const OBJ: u32 = 0b000001000;
    /// allow `nu` to be parsed as `null`
    pub const NULL: u32 = 0b000010000;
    /// allow `tr` to be parsed as `true`, and `fa` to be parsed as `false`
    pub const BOOL: u32 = 0b000100000;
    /// allow `Na` to be parsed as `NaN`
    pub const NAN: u32 = 0b001000000;
    /// allow `Inf` to be parsed as `Infinity`
    pub const INFINITY: u32 = 0b010000000;
    /// allow `-Inf` to be parsed as `-Infinity`
    pub const _INFINITY: u32 = 0b100000000;

    /// `INFINITY | _INFINITY`：允许 `Inf`/`-Inf` 这类不完整写法。
    pub const INF: u32 = INFINITY | _INFINITY;
    /// 所有特殊字面量的不完整形式：`null`/`true`/`false`/`NaN`/`±Infinity`。
    pub const SPECIAL: u32 = NULL | BOOL | INF | NAN;
    /// 所有原子类型：字符串、数字与特殊字面量（不含数组/对象）。
    pub const ATOM: u32 = STR | NUM | SPECIAL;
    /// 容器类型：数组与对象。
    pub const COLLECTION: u32 = ARR | OBJ;
    /// 全部类型（9 个位全开，数值 511）；`ALLOW_ALL` 是它的别名。
    pub const ALL: u32 = ATOM | COLLECTION;
}

/// `Allow.ALL`（`parseStreamingJson` 使用的默认值）。
///
/// 与 `allow::ALL` 是同一个常量（511）；测试直接断言其数值为 511。
pub use allow::ALL as ALLOW_ALL;

/// 错误分类（对应 TS 的三个异常类）。
///
/// TS 的 `partial-json` 用 `PartialJSON`/`MalformedJSON`/普通 `Error` 三个子类区分失败
/// 原因，另有一种未被捕获、原样逃逸的 V8 `SyntaxError`。这里用
/// [`errors::PartialErrorKind`] 承担同样的判别；[`errors::PartialError`] 只保存
/// 种类与最终文本（`Display` 直接输出 `message`，见其 `#[error]` 属性）。
pub mod errors {
    /// `PartialJSON` / `MalformedJSON` / 普通 `Error` 的判别。
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum PartialErrorKind {
        /// 输入不完整（`PartialJSON`）。
        Partial,
        /// 输入畸形（`MalformedJSON`）。
        Malformed,
        /// 参数错误（如空串的 `Error`）。
        Plain,
        /// 未被捕获、原样逃逸的 V8 `SyntaxError`（`parseStr` 的第二次 `JSON.parse`
        /// 不在 try 里，见该函数注释）。它的消息**不带** ` at position N` 后缀。
        Syntax,
    }

    /// `partial-json` 的错误。
    #[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
    #[error("{message}")]
    pub struct PartialError {
        /// 对应 TS 的异常类；调用方据此区分「输入未完成」与「输入畸形」。
        pub kind: PartialErrorKind,
        /// 最终错误文本；通常已包含 TS 的 ` at position N` 后缀（`Plain` 与 `Syntax` 种类
        /// 除外：`Plain` 的消息是未裁剪原文加 ` is empty`，`Syntax` 是 V8 原文）。
        pub message: String,
    }

    impl PartialError {
        /// 构造「输入不完整」错误。
        ///
        /// `message` 由调用方给出，本函数**不**追加 ` at position N`；位置后缀来自
        /// `Parser::mark_partial`（或先拼好文本再传入）。
        pub fn partial(message: impl Into<String>) -> Self {
            PartialError {
                kind: PartialErrorKind::Partial,
                message: message.into(),
            }
        }

        /// 构造「输入畸形」错误；同样不追加位置，位置由调用方拼进 `message`。
        pub fn malformed(message: impl Into<String>) -> Self {
            PartialError {
                kind: PartialErrorKind::Malformed,
                message: message.into(),
            }
        }

        /// 构造普通参数错误（对应 TS 的 `Error`）。
        ///
        /// 本 crate 内目前只用于「输入为空/全空白」这一种情况（消息使用未裁剪的原文）。
        pub fn plain(message: impl Into<String>) -> Self {
            PartialError {
                kind: PartialErrorKind::Plain,
                message: message.into(),
            }
        }

        /// 原样逃逸的 `SyntaxError`：消息是 V8 的原文，不加 ` at position N`。
        pub fn syntax(err: crate::js_parse::ParseError) -> Self {
            PartialError {
                kind: PartialErrorKind::Syntax,
                message: err.message,
            }
        }
    }
}

/// `partial-json` 的 `parse(jsonString, allowPartial = Allow.ALL)`。
///
/// 错误消息是当前稳定契约（含 ` at position N` 后缀与 `SyntaxError: ` 前缀），
/// 改动需同步测试。
///
/// # 参数
/// - `input`：可能不完整的 JSON 文本；首尾按 ECMAScript 空白裁剪（包括 BOM、NBSP
///   等，见 `trim_js_whitespace`），全空白输入返回 [`errors::PartialErrorKind::Plain`]。
/// - `allow`：`allow` 模块的位掩码；取 `ALLOW_ALL` 时所有类型都允许不完整。
///
/// # 返回值
/// 输入中**第一个**值的解析结果。容器在到达输入末尾或遇到错误时，只要该层的 `Allow`
/// 位打开，就返回已收集的部分内容（例如 `{"a":1,"b":` → `{"a":1}`）。字符串、字面量与
/// 容器不检查第一个值之后的剩余文本：`truex`、`{"a":1} trailing` 都会成功。
///
/// 顶层数字是例外：`parse_num` 的 `index == 0` 分支用 `JSON.parse` 解析整串，`1 2`
/// 报 [`errors::PartialErrorKind::Malformed`]；`1e` 会经 `lastIndexOf("e")` 重试返回 `1`。
///
/// # 错误
/// - [`errors::PartialErrorKind::Partial`]：输入在该层不完整且对应 `Allow` 位未打开；
/// - [`errors::PartialErrorKind::Malformed`]：确定的语法错误；
/// - [`errors::PartialErrorKind::Plain`]：空输入（消息使用未裁剪的原文）；
/// - [`errors::PartialErrorKind::Syntax`]：`parse_str` 第二次补引号解析时 V8 风格的
///   `SyntaxError` 原样逃逸，消息不带位置后缀。
///
/// # 复杂度
/// 常规输入 O(n)，额外空间 O(n)（UTF-16 副本 + 结果树）；最坏情形 O(n²)——每个解析
/// 失败的数字/字符串 token 都会对整串做一次 `last_index_of` 全扫描（见 `parse_num`/
/// `parse_str` 的失败回退），深层嵌套中每层一个失败 token 可使失败次数与输入同阶。
/// 顶层容器另做一次全树键序重排，代价是每层整数键排序的 O(k log k)（k 为该层整数键
/// 个数）。
///
/// # 注意
/// 位置是**裁剪后**文本的 UTF-16 code unit 游标（0-based），不是字节偏移或行号；
/// 空输入在解析前直接返回，不存在游标位置。
pub fn parse(input: &str, allow: u32) -> Result<Value, PartialError> {
    if trim_js_whitespace(input).is_empty() {
        // TS: `throw new Error(`${jsonString} is empty`)` —— 用未 trim 的原文。
        return Err(PartialError::plain(format!("{input} is empty")));
    }
    let trimmed = trim_js_whitespace(input);
    let units: Vec<u16> = trimmed.encode_utf16().collect();
    let mut parser = Parser {
        units,
        index: 0,
        allow,
        non_finite: false,
    };
    parser.parse_any()
}

/// 便捷入口：等价于 `parse(input, ALLOW_ALL)`。
///
/// 测试用它覆盖 `Allow.ALL` 的行为；`parseStreamingJson` 的回退使用的是带标注的
/// [`parse_partial_tagged`]（见 `crate::js_parse::parse_streaming`），因为需要区分
/// `null` 与非有限数。错误语义与 [`parse`] 相同。
pub fn parse_partial(input: &str) -> Result<Value, PartialError> {
    parse(input, ALLOW_ALL)
}

/// `partial-json` 结果的「JS 类型」补充：Rust 的 `Value` 无法区分 null 字面量与
/// 非有限数（`NaN`/`±Infinity` 都落为 `null`），但 TS 的 `result ?? {}` 只替换前者。
///
/// 只由 [`parse_tagged`]/[`parse_partial_tagged`] 返回；普通 [`parse`]/[`parse_partial`]
/// 的 `Value` 无法区分这两种情况，需要区分时必须改用带标注入口。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PartialKind {
    /// 普通 JSON 值（含 null 字面量）。
    Value,
    /// `NaN`/`Infinity`/`-Infinity`（Rust 侧落为 `Value::Null`，但 JS 是 number）。
    NonFiniteNumber,
}

/// 带类型标注的 `parse_partial`：`parseStreamingJson` 用它区分 `null` 与非有限数。
///
/// 等价于 `parse_tagged(input, ALLOW_ALL)`；错误语义与 [`parse_partial`] 相同。
pub fn parse_partial_tagged(input: &str) -> Result<(Value, PartialKind), PartialError> {
    parse_tagged(input, ALLOW_ALL)
}

/// 带类型标注的 `parse`。
///
/// 返回 `(value, kind)`，其中 `kind == PartialKind::NonFiniteNumber` 表示顶层结果在 JS
/// 里是非有限数（本实现存成 `Value::Null`）。判定条件是「整棵树出现过非有限数**且**
/// 顶层值为 null」，因此 `[Infinity]` 仍是 `Value`，`null` 字面量也不会被误判。
///
/// 裁剪、`allow` 语义与错误类型都与 [`parse`] 相同；解析逻辑与 [`parse`] 平行，
/// 区别只在返回前额外做一次顶层种类判定。
pub fn parse_tagged(input: &str, allow: u32) -> Result<(Value, PartialKind), PartialError> {
    if trim_js_whitespace(input).is_empty() {
        return Err(PartialError::plain(format!("{input} is empty")));
    }
    let trimmed = trim_js_whitespace(input);
    let units: Vec<u16> = trimmed.encode_utf16().collect();
    let mut parser = Parser {
        units,
        index: 0,
        allow,
        non_finite: false,
    };
    let value = parser.parse_any()?;
    // `non_finite` 是整棵树上的记录，但只有**顶层**结果是非有限数时才会落成
    // `Value::Null` 且不是 JS 的 `null`（`[Infinity]` 是数组，不会被 `??` 替换）。
    let kind = if parser.non_finite && value.is_null() {
        PartialKind::NonFiniteNumber
    } else {
        PartialKind::Value
    };
    Ok((value, kind))
}

/// 容器种类（`parseObj` / `parseArr`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ContainerKind {
    /// `{ ... }`。
    Object,
    /// `[ ... ]`。
    Array,
}

/// `parse_any` 的结果：原子值或「刚消费开括号、待压栈的容器」。
enum AnyEntry {
    /// 完整原子值，调用方可直接使用。
    Value(Value),
    /// 容器：开括号已消费，但内容尚未解析；调用方负责压入 `parse_container` 的栈。
    Container(ContainerKind),
}

/// 显式栈里的一个容器帧。
///
/// 帧只保存「构建中的容器 + 该层解析阶段」，不保存子帧的中间状态；子容器完成时通过
/// `FrameStep::Return` 把值交回父帧。这样任意深度的解析都只占用一个 `Vec<Frame>`，
/// 不消耗调用栈。
enum Frame {
    Object {
        /// 已解析的自有键值；`__proto__` 不会出现在这里（见 `accept_value` 的说明）。
        obj: serde_json::Map<String, Value>,
        /// 已读到、等待值的键（`ObjState::Value` 时必为 `Some`，其余状态为 `None`）。
        key: Option<String>,
        /// 该对象当前的解析阶段。
        state: ObjState,
    },
    Array {
        /// 已收集的元素，顺序与输入一致。
        arr: Vec<Value>,
    },
}

impl Frame {
    /// 刚消费开括号时的初始帧：对象处于 `Open`（尚未跳过 `{` 后的空白），数组为空。
    fn new(kind: ContainerKind) -> Self {
        match kind {
            ContainerKind::Object => Frame::Object {
                obj: serde_json::Map::new(),
                key: None,
                state: ObjState::Open,
            },
            ContainerKind::Array => Frame::Array { arr: Vec::new() },
        }
    }
}

/// 对象帧的阶段：`Open` 是刚消费 `{`（TS 里 `parseObj` 的 `skipBlank()` 尚未执行），
/// `Key` 等待下一个键，`Value` 已读完键与冒号、等待值。
///
/// 转换：`Open -> Key -> Value -> Key -> ...`；`Key` 时读到 `}` 直接结束对象；
/// `Value` 时取值失败由 `frame_error` 决定返回部分对象还是收尾错误。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ObjState {
    /// 刚消费 `{`；下一次 `advance_frame` 会先 `skip_blank`，再转入 `Key`。
    Open,
    /// 等待下一个键（也可能紧接着 `}` 结束对象）。
    Key,
    /// 已读完键与冒号，等待 `parse_any_entry` 提供值。
    Value,
}

/// 栈顶容器的一步推进结果。
///
/// `advance_frame` 会产生任意一种；`WantValue` 由 `parse_container` 消费（取下一个值后
/// 用 `accept_value` 回填），其余两种交给 `deliver` 沿栈冒泡。
enum FrameStep {
    /// 容器已完整结束（含「部分完成」），把值交还父帧。
    Return(Value),
    /// 需要下一个值：主循环调用 `parse_any_entry` 后 `accept_value` 回填。
    WantValue,
    /// `Allow` 缺位：把该层的收尾错误向上抛。
    Fail(PartialError),
}

/// 一次解析的可变状态（对应 TS `_parseJSON` 闭包里的 `index` 与 `allow`）。
///
/// 所有读写都按 UTF-16 code unit 进行；`index` 只增不减。注意字面量分支会一次前进
/// 固定长度（3/4/5/8/9），游标可能越过输入末尾，因此每次读取都必须走
/// `char_at`/`current` 的 `Option` 边界检查。
struct Parser {
    /// 裁剪后的输入，按 UTF-16 code unit 存储（JS 字符串的索引单位）。
    units: Vec<u16>,
    /// 当前游标（code unit 下标）；见结构体文档，可能暂时超过 `units.len()`。
    index: usize,
    /// 「允许部分解析」的位掩码快照，来自公开入口的 `allow` 参数。
    allow: u32,
    /// 结果里出现过 `NaN`/`±Infinity`（它们在 Rust 侧落为 null，但 JS 里是 number）。
    non_finite: bool,
}

impl Parser {
    fn len(&self) -> usize {
        self.units.len()
    }

    fn char_at(&self, i: usize) -> Option<u16> {
        self.units.get(i).copied()
    }

    /// `jsonString[index]`（越界为 `undefined`）。
    fn current(&self) -> Option<u16> {
        self.char_at(self.index)
    }

    /// `jsonString.substring(index, index + n) === prefix`。
    ///
    /// 调用点只传 JSON 关键字，因此 `prefix` 必须是 ASCII；实现逐字节转成 UTF-16
    /// code unit 比较，输入不足 `prefix` 长度时返回 `false`。
    fn starts_with_at(&self, prefix: &str) -> bool {
        for (i, b) in (self.index..).zip(prefix.bytes()) {
            if self.char_at(i) != Some(u16::from(b)) {
                return false;
            }
        }
        true
    }

    /// `prefix.startsWith(jsonString.substring(index))`，即剩余内容是前缀的前缀。
    ///
    /// 按 UTF-16 code unit 比较：经 `u as u8 as char` 折成 Latin-1 会把 U+016E 之类
    /// 截断成 `n`，从而误判成 `null` 的前缀。
    fn remaining_is_prefix_of(&self, literal: &str) -> bool {
        let remaining = &self.units[self.index.min(self.len())..];
        let target: Vec<u16> = literal.encode_utf16().collect();
        remaining.len() <= target.len() && target[..remaining.len()] == *remaining
    }

    /// 构造 `Partial` 错误，并在消息末尾追加当前游标的 ` at position N`。
    ///
    /// `index` 是 UTF-16 code unit 游标（不是行列号）；`Syntax` 种类不经过这里。
    fn mark_partial(&self, msg: &str) -> PartialError {
        PartialError::partial(format!("{msg} at position {}", self.index))
    }

    /// 构造 `Malformed` 错误，追加 ` at position N`；消息通常以 `SyntaxError: ` 开头。
    fn throw_malformed(&self, msg: &str) -> PartialError {
        PartialError::malformed(format!("{msg} at position {}", self.index))
    }

    /// 解析输入中的第一个值（模块的两个公开入口都在 `index == 0` 时调用它）。
    ///
    /// 原子值直接返回；容器交给 `parse_container`。容器结果返回前会做一次全树
    /// `reorder_in_place`，让对象键的迭代序等于 JS 枚举序；原子值不需要重排。
    /// 本函数只解析输入中的第一个值，不检查其后是否还有内容（部分解析的既有契约）。
    fn parse_any(&mut self) -> Result<Value, PartialError> {
        match self.parse_any_entry()? {
            AnyEntry::Value(value) => Ok(value),
            AnyEntry::Container(kind) => {
                let value = self.parse_container(kind)?;
                // TS 的 `parseObj` 构建的是普通 JS 对象：枚举序是「整数索引键升序 + 其余插入序」，
                // 每一层都如此（`Object.keys` / `Map` 迭代 / `JSON.stringify` 都按它）。
                // `serde_json` 的 `Map` 只保插入序，因此返回前就地重排（显式栈，任意深度）。
                let mut value = value;
                crate::js_object::reorder_in_place(&mut value);
                Ok(value)
            }
        }
    }

    /// 解析一个字符串字面量（对应 TS 的 `parseStr`）。
    ///
    /// 正常前置条件：`index` 指向开引号（`parse_any_entry` 只在当前字符是 `"` 时调用）。
    /// 但 `advance_frame` 解析对象键时与 TS 一样**不检查**这一点：它把任意当前位置当作
    /// 引号起点，畸形输入（例如 `{a:1}` 或 `{"a":1, }`）由后续扫描/补引号逻辑处理。
    /// 成功时返回 `Value::String`，并把游标移到闭合引号之后。
    ///
    /// 三种结局：
    /// 1. 找到未转义的 `"`：把 `[start, end)` 交给本地 `crate::js_parse::parse`；
    ///    失败转成 `Malformed`，消息加 `SyntaxError: ` 前缀（TS 的 `String(e)`）。
    /// 2. 到输入末尾仍未闭合且 `allow::STR` 打开：去掉悬空的 `\` 后补 `"` 解析；
    ///    失败再从**最后一个反斜杠**处截断并补 `"` 重试。第二次解析不在 try 里，
    ///    失败以 `Syntax` 原样返回（无 ` at position N`）。
    /// 3. 到末尾仍未闭合且 `STR` 未打开：`Partial`「Unterminated string literal」。
    ///
    /// 转义判定用 `escape` 记录「前一个字符是尚未配对的反斜杠」，因此 `\\"` 视为已闭合、
    /// `\"` 视为转义引号；不在此解码 `\uXXXX`，交给本地 JSON 解析器。
    ///
    /// 例：`"hello \u12` 且允许 `STR` → `"hello "`；`"a<TAB>b` 两次补引号都失败 →
    /// `Syntax`（消息来自 V8 风格解析器）。
    fn parse_str(&mut self) -> Result<Value, PartialError> {
        // 记录开引号位置；下面两条补引号回退都从 start 开始截取。
        let start = self.index;
        let mut escape = false;
        self.index += 1; // skip initial quote
        // escape 在遇到反斜杠时取反、遇到其他字符时清零，用来判断「当前引号是否被奇数个
        // 连续反斜杠转义」。循环条件沿用这一判据：未到末尾，且未遇闭合引号或闭合引号
        // 本身被转义时继续。
        while self.index < self.len()
            && (self.current().is_some_and(|c| c != u16::from(b'"'))
                || (escape && self.char_at(self.index - 1) == Some(u16::from(b'\\'))))
        {
            escape = if self.current() == Some(u16::from(b'\\')) {
                !escape
            } else {
                false
            };
            self.index += 1;
        }
        if self.current() == Some(u16::from(b'"')) {
            // 闭合分支里 escape 必为 false（否则循环条件会继续），`- escape` 不改变结果；
            // 这样写与上面的循环判据保持一致。此时 index 指向闭合引号。
            // TS: `jsonString.substring(start, ++index - Number(escape))` —— `++index`
            // 把游标推到闭合引号之后。
            let end = self.index + 1 - usize::from(escape);
            self.index += 1;
            match self.parse_json_substring(start, end) {
                Ok(v) => return Ok(v),
                // TS: `catch (e) { throwMalformedError(String(e)) }` —— `String(e)` 带
                // `SyntaxError: ` 前缀。
                Err(e) => {
                    return Err(self.throw_malformed(&format!("SyntaxError: {}", e.message)));
                }
            }
        } else if self.allow & allow::STR != 0 {
            // 第一次补引号：末尾有悬空反斜杠时（escape=true）先丢掉它，再补一个 `"`。
            let end = self.index.saturating_sub(usize::from(escape));
            match self.parse_json_substring_plus_quote(start, end) {
                Ok(v) => return Ok(v),
                Err(_) => {
                    // TS 的第二次 `JSON.parse` **不在** try 里：
                    //   catch (e) { return JSON.parse(substring(start, lastIndexOf("\\")) + '"'); }
                    // 因此它抛出的 V8 SyntaxError 会原样逃逸（无 ` at position N` 后缀）。
                    // `lastIndexOf` 面向整串；-1 时 `substring` 交换两端，等价于 `(0, start)`。
                    let end = self.last_index_of('\\').unwrap_or(0);
                    return match self.parse_json_substring_plus_quote(start, end) {
                        Ok(v) => Ok(v),
                        Err(e) => Err(PartialError::syntax(e)),
                    };
                }
            }
        }
        Err(self.mark_partial("Unterminated string literal"))
    }

    /// `JSON.parse(jsonString.substring(start, end))`（错误保留为 V8 的 `ParseError`）。
    fn parse_json_substring(
        &self,
        start: usize,
        end: usize,
    ) -> Result<Value, crate::js_parse::ParseError> {
        crate::js_parse::parse(&self.slice(start, end))
    }

    /// `JSON.parse(jsonString.substring(start, end) + '"')`。
    ///
    /// 用于 `parse_str` 的两条补引号回退；错误保留为 `ParseError`，让调用方能把它
    /// 原样转成 `Syntax` 种类（而不是拼成 `Malformed` 文本）。
    fn parse_json_substring_plus_quote(
        &self,
        start: usize,
        end: usize,
    ) -> Result<Value, crate::js_parse::ParseError> {
        let mut text = self.slice(start, end);
        text.push('"');
        crate::js_parse::parse(&text)
    }

    /// `jsonString.substring(start, end)`：两端先夹到 `[0, len]`，`start > end` 时交换。
    ///
    /// `lastIndexOf` 返回 -1 的调用点传入 0，交换后正好等价于 JS 的 `substring(0, x)`。
    fn slice(&self, start: usize, end: usize) -> String {
        let len = self.len();
        let a = start.min(len);
        let b = end.min(len);
        let (lo, hi) = if a <= b { (a, b) } else { (b, a) };
        String::from_utf16_lossy(&self.units[lo..hi])
    }

    /// `jsonString.lastIndexOf(ch)`：在**整段裁剪后输入**里找最后一个匹配的 code unit。
    ///
    /// 只用于 ASCII 字符（`\` 与 `e`），按 UTF-16 比较与 JS 一致；返回 code unit 下标。
    /// 找不到时返回 `None`（对应 JS 的 `-1`），由调用方决定如何回退。
    fn last_index_of(&self, ch: char) -> Option<usize> {
        let target = u16::try_from(u32::from(ch)).ok()?;
        self.units.iter().rposition(|&u| u == target)
    }

    /// `markPartialJSON("Expected '}' at end of object")`。
    fn expected_close_brace(&self) -> PartialError {
        self.mark_partial("Expected '}' at end of object")
    }

    // -- 容器：显式栈（TS 递归；Rust 固定栈在约 1.6 万层会 abort，见 ARCHITECTURE §8 第 33 条） --

    /// 取一个值：原子直接返回，容器返回 [`AnyEntry::Container`] 由调用方压栈。
    ///
    /// 与 TS `parseAny` 逐句对应；容器分支只消费开括号，帧的后续推进交给
    /// [`Self::advance_frame`]（TS 里 `parseObj`/`parseArr` 第一行的 `index++`）。
    ///
    /// 判定顺序固定：`"`、`{`、`[`，然后 `null`/`true`/`false`/`Infinity`/`-Infinity`/
    /// `NaN`，最后才是数字。关键字先做完整匹配，再按 `Allow` 做「剩余文本是关键字前缀」
    /// 的匹配；前缀匹配只要求剩余长度小于关键字长度（`-Infinity` 另要求剩余至少 2 个
    /// code unit），命中后游标一次前进关键字长度（可能越过输入末尾），交出的仍是完整值。
    ///
    /// 本函数自己先 `skip_blank`，调用方无需预跳空白；到输入末尾时报 `Partial`
    /// 「Unexpected end of input」。
    fn parse_any_entry(&mut self) -> Result<AnyEntry, PartialError> {
        self.skip_blank();
        if self.index >= self.len() {
            return Err(self.mark_partial("Unexpected end of input"));
        }
        match self.current() {
            Some(c) if c == u16::from(b'"') => return self.parse_str().map(AnyEntry::Value),
            Some(c) if c == u16::from(b'{') => {
                self.index += 1; // skip initial brace
                return Ok(AnyEntry::Container(ContainerKind::Object));
            }
            Some(c) if c == u16::from(b'[') => {
                self.index += 1; // skip initial bracket
                return Ok(AnyEntry::Container(ContainerKind::Array));
            }
            _ => {}
        }
        // 上方已检查过 index < len，这里可以安全相减。
        let remaining = self.len() - self.index;
        // 关键字短路：完整匹配优先；前缀匹配只在 Allow 位打开且剩余长度不足时生效。
        // null
        if self.starts_with_at("null")
            || (self.allow & allow::NULL != 0
                && remaining < 4
                && self.remaining_is_prefix_of("null"))
        {
            self.index += 4;
            return Ok(AnyEntry::Value(Value::Null));
        }
        if self.starts_with_at("true")
            || (self.allow & allow::BOOL != 0
                && remaining < 4
                && self.remaining_is_prefix_of("true"))
        {
            self.index += 4;
            return Ok(AnyEntry::Value(Value::Bool(true)));
        }
        if self.starts_with_at("false")
            || (self.allow & allow::BOOL != 0
                && remaining < 5
                && self.remaining_is_prefix_of("false"))
        {
            self.index += 5;
            return Ok(AnyEntry::Value(Value::Bool(false)));
        }
        if self.starts_with_at("Infinity")
            || (self.allow & allow::INFINITY != 0
                && remaining < 8
                && self.remaining_is_prefix_of("Infinity"))
        {
            self.index += 8;
            self.non_finite = true;
            return Ok(AnyEntry::Value(non_finite()));
        }
        if self.starts_with_at("-Infinity")
            || (self.allow & allow::_INFINITY != 0
                && remaining > 1
                && remaining < 9
                && self.remaining_is_prefix_of("-Infinity"))
        {
            self.index += 9;
            self.non_finite = true;
            return Ok(AnyEntry::Value(non_finite()));
        }
        if self.starts_with_at("NaN")
            || (self.allow & allow::NAN != 0 && remaining < 3 && self.remaining_is_prefix_of("NaN"))
        {
            self.index += 3;
            self.non_finite = true;
            return Ok(AnyEntry::Value(non_finite()));
        }
        self.parse_num().map(AnyEntry::Value)
    }

    /// 推进栈顶容器一步，对应 TS 里 `parseObj`/`parseArr` 的一次循环迭代。
    ///
    /// # 状态机（对象）
    /// `Open` 只在第一次迭代出现：先 `skip_blank`（TS 在循环前调一次），随后转为 `Key`。
    /// 之后每次迭代：
    /// 1. 当前是 `}`：消费并 `Return` 已完成对象；
    /// 2. `skip_blank` 后已到输入末尾且 `allow::OBJ` 打开：`Return` 部分对象；
    /// 3. 解析键（复用 `parse_str`）、跳过冒号，状态转 `Value`，返回 `WantValue`。
    ///
    /// 第 3 步的冒号是**无条件** `index += 1`（TS 同样不校验），因此 `{"a" 1}` 会把键后
    /// 第一个非空白字符 `1` 当作冒号吞掉，随后在 `}` 处取值失败，结果由 `frame_error`
    /// 决定（`Allow.OBJ` 打开时得到空对象）。
    ///
    /// # 数组
    /// 数组没有循环前的 `skip_blank`：当前是 `]` 就消费并 `Return`，否则 `WantValue`。
    /// 空白与逗号由 `accept_value` 统一处理。
    ///
    /// # 失败
    /// 键解析失败时，本函数直接调用 `frame_error`，由该层 `Allow` 位决定部分返回还是
    /// `Fail`；等待值时的失败由 `parse_container` 走同一个 `frame_error`。
    fn advance_frame(&mut self, frame: &mut Frame) -> FrameStep {
        match frame {
            Frame::Object { obj, key, state } => {
                if *state == ObjState::Open {
                    // TS: `parseObj` 在循环前先 `skipBlank()`。
                    self.skip_blank();
                    *state = ObjState::Key;
                }
                // TS: `while (jsonString[index] !== "}")`
                if self.current() == Some(u16::from(b'}')) {
                    self.index += 1; // skip final brace
                    return FrameStep::Return(Value::Object(std::mem::take(obj)));
                }
                self.skip_blank();
                if self.index >= self.len() && self.allow & allow::OBJ != 0 {
                    return FrameStep::Return(Value::Object(std::mem::take(obj)));
                }
                let parsed_key = match self.parse_str() {
                    Ok(Value::String(s)) => s,
                    // `JSON.parse` 一个带引号的子串永远是字符串；非字符串只可能是防御分支。
                    Ok(_) => String::new(),
                    Err(e) => return self.frame_error(frame, e),
                };
                self.skip_blank();
                // TS 同样不校验这一个字符确实是 ':'，直接吞掉。
                self.index += 1; // skip colon
                *key = Some(parsed_key);
                *state = ObjState::Value;
                FrameStep::WantValue
            }
            Frame::Array { arr } => {
                // TS: `while (jsonString[index] !== "]")`（数组没有循环前的 skipBlank）
                if self.current() == Some(u16::from(b']')) {
                    self.index += 1; // skip final bracket
                    return FrameStep::Return(Value::Array(std::mem::take(arr)));
                }
                FrameStep::WantValue
            }
        }
    }

    /// 把一个已完成的值交给栈顶容器（TS 的 `obj[key] = value` / `arr.push(value)`，随后
    /// `skipBlank(); if (jsonString[index] === ",") index++`）。
    ///
    /// 不变量：对象帧调用时必须处于 `Value` 状态且 `key` 为 `Some`（`expect` 的前提）；
    /// 返回后对象状态变回 `Key`，数组状态不变。逗号后的空白不在这里跳过，留给下一次
    /// `advance_frame`/`parse_any_entry` 处理（与 TS 的循环条件一致）。
    ///
    /// JS 语义细节：
    /// - 重复键走 `Map::insert`：值被覆盖、位置保持第一次插入的位置（JS 的
    ///   `obj[key] = value` 同样不改变已有键的枚举位置）；
    /// - `__proto__` 在 JS 里写的是原型而不是自有属性，故被丢弃（与 `JSON.parse` 相反）。
    fn accept_value(&mut self, frame: &mut Frame, value: Value) {
        match frame {
            Frame::Object { obj, key, state } => {
                let key = key.take().expect("key is pending when a value arrives");
                // TS 的 `obj[key] = value`：`__proto__` 设置原型而不是自有键，被丢弃。
                if key != "__proto__" {
                    obj.insert(key, value);
                }
                *state = ObjState::Key;
            }
            Frame::Array { arr } => arr.push(value),
        }
        self.skip_blank();
        if self.current() == Some(u16::from(b',')) {
            self.index += 1; // skip comma
        }
    }

    /// 帧内错误：`Allow` 命中时返回已收集的部分容器，否则转成该层的收尾错误。
    ///
    /// `err` 不会进入最终消息：数组分支显式丢弃它（`let _ = err`），对象分支也不读取。
    /// TS 的 `parseObj`/`parseArr` 在 `Allow` 位关闭时只抛 `Expected '}'` / `Expected ']'`
    /// 模板，原始错误被覆盖；因此容器内部的畸形输入最终常表现为「缺少收尾符」。
    fn frame_error(&self, frame: &mut Frame, err: PartialError) -> FrameStep {
        match frame {
            Frame::Object { obj, .. } => {
                if self.allow & allow::OBJ != 0 {
                    FrameStep::Return(Value::Object(std::mem::take(obj)))
                } else {
                    FrameStep::Fail(self.expected_close_brace())
                }
            }
            Frame::Array { arr } => {
                if self.allow & allow::ARR != 0 {
                    FrameStep::Return(Value::Array(std::mem::take(arr)))
                } else {
                    let _ = err;
                    FrameStep::Fail(self.mark_partial("Expected ']' at end of array"))
                }
            }
        }
    }

    /// 把 `Return`/`Fail` 冒泡到父帧；返回 `Some` 表示整棵树已有结果（完成或最终错误）。
    ///
    /// - `Return`：弹出当前帧。栈空则返回 `Some(value)`；否则用 `accept_value` 把值回填
    ///   父帧并返回 `None`，主循环继续推进父帧。
    /// - `Fail`：弹出当前帧。栈空则返回 `Err`；否则交给父帧的 `frame_error` 重新判断
    ///   （父层 `Allow` 打开时可能把子层错误降级为部分结果），循环直到栈空或出现 `Return`。
    ///
    /// `WantValue` 不会传给本函数（调用方只传 `Return`/`Fail`），故为 `unreachable!`。
    fn deliver(
        &mut self,
        stack: &mut Vec<Frame>,
        step: FrameStep,
    ) -> Result<Option<Value>, PartialError> {
        let mut step = step;
        loop {
            match step {
                FrameStep::Return(value) => {
                    stack.pop();
                    match stack.last_mut() {
                        None => return Ok(Some(value)),
                        Some(parent) => {
                            self.accept_value(parent, value);
                            return Ok(None);
                        }
                    }
                }
                FrameStep::Fail(err) => {
                    stack.pop();
                    match stack.last_mut() {
                        None => return Err(err),
                        Some(parent) => step = self.frame_error(parent, err),
                    }
                }
                FrameStep::WantValue => unreachable!("deliver only receives Return/Fail"),
            }
        }
    }

    /// `parseAny` 的完整实现（显式栈版）。
    ///
    /// 语义与 TS 的互相递归逐句对应：容器帧需要值时主循环取出下一个值再交还父帧；子容器
    /// 完成（或部分完成）后作为普通值回填；错误逐层经过父帧的 `catch`。
    ///
    /// 终止性：每次 `WantValue` 要么由 `parse_any_entry` 消费至少一个 code unit 后交出值/
    /// 压栈，要么以错误结束并交给 `frame_error` 决定「返回部分结果」或「抛错」；`Return`/
    /// `Fail` 只会弹栈或直接返回。栈深不超过输入的嵌套层数，因此循环必然结束。
    ///
    /// 复杂度：成功路径每个 code unit 至多被扫描常数次；失败回退会对整串做
    /// `last_index_of`（见 `parse_num`/`parse_str`），最坏情形总时间为 O(n²)；空间与
    /// 输入同阶。解析循环本身不受调用栈深度限制；残留限制来自 `serde_json::Value`
    /// 的递归 Drop（见模块文档）。
    fn parse_container(&mut self, kind: ContainerKind) -> Result<Value, PartialError> {
        let mut stack: Vec<Frame> = vec![Frame::new(kind)];
        loop {
            let step = self.advance_frame(stack.last_mut().expect("frame exists"));
            match step {
                FrameStep::WantValue => match self.parse_any_entry() {
                    Ok(AnyEntry::Value(value)) => {
                        self.accept_value(stack.last_mut().expect("frame exists"), value);
                    }
                    Ok(AnyEntry::Container(kind)) => stack.push(Frame::new(kind)),
                    Err(err) => {
                        let step = self.frame_error(stack.last_mut().expect("frame exists"), err);
                        if let Some(done) = self.deliver(&mut stack, step)? {
                            return Ok(done);
                        }
                    }
                },
                other => {
                    if let Some(done) = self.deliver(&mut stack, other)? {
                        return Ok(done);
                    }
                }
            }
        }
    }

    /// 解析数字，对应 TS 的 `parseNum`。
    ///
    /// 按游标位置分成两条完全不同的路径：
    ///
    /// # 顶层（`index == 0`）
    /// TS 直接 `JSON.parse(整串)`，并不要求它是数字；Rust 用 `crate::js_parse::parse`
    /// 复现同样的 V8 语义。失败时若 `allow::NUM` 打开，再从最后一个 `e` 处截断并重试
    /// （`1e400e` → `1e400` → Infinity）；否则报 `Malformed`，消息是
    /// `SyntaxError: <V8 消息> at position 0`。单独一个 `-` 是例外：消息为
    /// `Not sure what '-' is at position 0`，没有 `SyntaxError: ` 前缀。
    /// 数字溢出在本实现里落成 `Value::Null`，由 `note_overflow` 记录。
    ///
    /// # 容器内（`index > 0`）
    /// 1. 从当前游标起跳过可选的 `-`，一直扫描到 `,`/`]`/`}` 或输入末尾；
    /// 2. 若扫到输入末尾且 `allow::NUM` 未打开 → `Partial`「Unterminated number literal」；
    /// 3. 把这段原文交给本地 `parse`；成功则返回（并记录溢出）；
    /// 4. 失败时：原文恰为 `-` → `Partial`「Not sure what '-' is」；否则若整段输入里存在
    ///    `e`，从**最后一个** `e` 处截断再试一次（TS 同样不检查 `Allow.NUM`，这是
    ///    `partial-json` 的既定行为）；仍失败则报 `Malformed`。
    ///
    /// # 与 TS 的细节差异（仅容器内部错误文本）
    /// - TS 的 `lastIndexOf("e")` 找不到时返回 -1，随后用 `substring` 交换端点再解析一次
    ///   并失败；Rust 的 `last_index_of` 返回 `None`，直接上报第一次的错误。
    /// - TS 最终 `throwMalformedError(String(e))` 里的 `e` 是**重试**那次的错误（内层
    ///   `catch` 遮蔽了外层变量），Rust 用的是第一次 token 的错误。
    ///
    /// 这两处都只影响容器内部的错误文本：该错误会在 `frame_error` 里被部分结果或
    /// 「Expected ']' / '}'」替换，不会从公开入口逃出；顶层 `index == 0` 分支不受影响。
    ///
    /// # 注意
    /// 扫描时不跳过空白，因此 token 可能带尾部空白（本地 `parse` 会自行裁剪）；
    /// TS 的 `JSON.parse` 同样接受数字前后的空白。
    fn parse_num(&mut self) -> Result<Value, PartialError> {
        // 顶层特殊路径：TS 直接 JSON.parse 整串，错误消息也来自 V8。
        if self.index == 0 {
            if self.remaining_text() == "-" {
                return Err(self.throw_malformed("Not sure what '-' is"));
            }
            match self.parse_json_whole() {
                Ok(v) => {
                    self.note_overflow(&self.slice(0, self.len()), &v);
                    return Ok(v);
                }
                Err(e) => {
                    // TS 的 `JSON.parse(jsonString.substring(0, jsonString.lastIndexOf("e")))`
                    // 返回的仍可能是 `Infinity`（`"1e400e"` → `JSON.parse("1e400")`）；
                    // 必须同样经过 `note_overflow`，否则 `parseStreamingJson` 的 `?? {}`
                    // 会把非有限数误换成 `{}`。
                    if self.allow & allow::NUM != 0
                        && let Some(pos) = self.last_index_of('e')
                        && let Ok(v) = self.parse_json_substring(0, pos)
                    {
                        let text = self.slice(0, pos);
                        self.note_overflow(&text, &v);
                        return Ok(v);
                    }
                    return Err(self.throw_malformed(&e));
                }
            }
        }
        // 容器内：先把数字 token 粗扫出来（到 , ] } 或 EOF 为止），再做语法校验。
        let start = self.index;
        if self.current() == Some(u16::from(b'-')) {
            self.index += 1;
        }
        while let Some(c) = self.current() {
            if matches!(c, c2 if c2 == u16::from(b',') || c2 == u16::from(b']') || c2 == u16::from(b'}'))
            {
                break;
            }
            self.index += 1;
        }
        // NUM 关闭时，读到输入末尾说明数字可能还没写完（例如 `[1`）。
        if self.index == self.len() && self.allow & allow::NUM == 0 {
            return Err(self.mark_partial("Unterminated number literal"));
        }
        // token 可能带尾部空白（扫描不跳空白），本地严格解析器会自行处理。
        let token = self.slice(start, self.index);
        match crate::js_parse::parse(&token) {
            Ok(v) => {
                self.note_overflow(&token, &v);
                Ok(v)
            }
            Err(e) => {
                if token == "-" {
                    return Err(self.mark_partial("Not sure what '-' is"));
                }
                // 无论 NUM 位是否打开，失败后都尝试从最后一个 e 处截断再解析一次；
                // 找不到 e 时直接走下面的最终错误。
                if let Some(pos) = self.last_index_of('e') {
                    let text = self.slice(start, pos);
                    if let Ok(v) = crate::js_parse::parse(&text) {
                        self.note_overflow(&text, &v);
                        return Ok(v);
                    }
                }
                // TS 最终消息用的是重试那次的错误（内层 catch 遮蔽外层变量）；这里用
                // 第一次 token 的错误。该分支只在容器内部可达，错误随后会被 frame_error
                // 替换，因此公开入口看不到这处差异。
                Err(self.throw_malformed(&format!("SyntaxError: {}", e.message)))
            }
        }
    }

    /// 数字字面量溢出成 ±Infinity 时记一笔。
    ///
    /// 本地 `parse` 被迫把 `1e400` 落成 `Value::Null`（serde 存不下 Infinity），但 JS 里它是
    /// 非有限数、不是 `null`，`result ?? {}` 不会替换它。顶层判定见 [`parse_tagged`]。
    ///
    /// 只置位、不区分正负也不区分层级：`parse_tagged` 只关心「顶层值是否为非有限数」。
    fn note_overflow(&mut self, token: &str, value: &Value) {
        if value.is_null() && crate::js_num::parse_js_number(token).is_infinite() {
            self.non_finite = true;
        }
    }

    /// `JSON.parse(jsonString)`（整串，对应 TS 顶层分支）。
    ///
    /// 返回 `String` 而不是 `ParseError`，因为调用方要按 TS 的 `String(e)` 形式把
    /// `SyntaxError: ` 前缀拼进 `MalformedJSON` 的消息。
    fn parse_json_whole(&self) -> Result<Value, String> {
        let text = self.slice(0, self.len());
        crate::js_parse::parse(&text).map_err(|e| format!("SyntaxError: {}", e.message))
    }

    /// `jsonString.substring(index)`（`index` 越界时为空串）。
    fn remaining_text(&self) -> String {
        self.slice(self.index, self.len())
    }

    /// `" \n\r\t".includes(jsonString[index])`
    ///
    /// 只跳过 TS 源码写死的四个 ASCII 字符。ECMAScript 的其余空白（NBSP、U+2028、
    /// BOM 等）只在入口 `trim_js_whitespace` 处理；它们出现在值之间时会被当作普通
    /// 字符，进入数字/解析错误分支。这是 `partial-json@0.1.7` 的原始行为。
    fn skip_blank(&mut self) {
        while let Some(c) = self.current() {
            if c == u16::from(b' ')
                || c == u16::from(b'\n')
                || c == u16::from(b'\r')
                || c == u16::from(b'\t')
            {
                self.index += 1;
            } else {
                break;
            }
        }
    }
}

/// `NaN`/`±Infinity` 在 Rust 侧的落点：Null（可见 JSON 文本一致）。
///
/// `serde_json::Number` 只能表示有限 f64，而 JS 的 `JSON.stringify` 也把这三个值写成
/// `null`，所以文本层面等价。需要区分时只能看 `Parser::non_finite` / `PartialKind`，
/// 不能从 `Value` 反推。
fn non_finite() -> Value {
    Value::Null
}

#[cfg(test)]
mod tests {
    // 本文件的单元测试只覆盖两个最容易回归的入口行为：空串消息使用未裁剪原文、
    // 解析前会裁剪输入。逐条对照 `partial-json@0.1.7` fixture 的测试在
    // `crates/kiana-json/tests/partial_json.rs`。
    use super::*;

    #[test]
    fn empty_input_keeps_raw_text_in_message() {
        assert_eq!(parse_partial("").unwrap_err().message, " is empty");
        assert_eq!(parse_partial("   ").unwrap_err().message, "    is empty");
    }

    #[test]
    fn trims_before_parsing() {
        assert_eq!(
            crate::js_object::stringify(&parse_partial("  {\"a\":1}  ").unwrap()),
            "{\"a\":1}"
        );
    }
}
