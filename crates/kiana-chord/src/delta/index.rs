//! `delta/index.ts`：操作语言（`Op`/`WireOp`）、应用器、路径校验与 codec。
//!
//! 每个函数逐条对应 TS 的同名导出；错误消息（`TypeError` 文案、`PathError`、
//! `UnsafePathError`）是 wire 兼容性的一部分，逐字保留，统一由 [`OpError`] 承载
//! （`Display` 输出完整消息，[`OpError::name`] 输出错误名）。
//!
//! ## 与 TS 的表示差异
//!
//! - TS 的 `Op` 是元组数组，`WireOp` 额外允许 `#` 定义与省略路径的短形式；Rust 的
//!   [`Op`] 是强类型枚举，wire 形式用 JSON 数组表示（[`WireOp`] = `serde_json::Value`）。
//!   [`Encoder`] 产出 wire JSON，[`Decoder`] 消费 wire JSON 并产出 [`Op`]。
//! - TS 的 `apply`/`applyImmutable` 操作可变对象；Rust 的 [`apply`] 接收**所有权**并返回
//!   新根（`r` 无法就地替换）。`applyImmutable` 在 TS 里用 copy-on-write 避免修改入参；
//!   Rust 的所有权已经保证入参不被调用方观察到修改，因此 [`apply_immutable`] 复用
//!   [`apply`] 的语义（结构共享的身份不可表示，见模块测试注释）。
//! - 路径段用 [`Seg`]（`Key(String)`/`Index(i64)`）。TS 路径里的数字段在对象上是键、
//!   在数组上是下标，本模块在运行期按父容器类型判别。超出 `i64` 的非负整数段在对象上
//!   保留为 `Key(String(number))`（见 `number_to_seg`）；其 wire 再编码会变成字符串，
//!   这是 `Seg` 类型带来的唯一差异。
//! - JS 字符串是 UTF-16 码元序列，孤立代理项在 Rust 的 `String` 里无法表示（`JsonValue`
//!   是 `serde_json::Value`，ARCHITECTURE §8 第 31 条的被迫偏差）：`t` 截断到代理对中间
//!   时，TS 保留孤立代理项，Rust 在重建时用 U+FFFD 取代（与 `kiana_json` 解析 `"\ud800"`
//!   时的处理一致）。
//!
//! ## 模块结构
//!
//! 文件按依赖顺序排列，建议按这个顺序阅读：
//!
//! 1. **错误类型**（[`UnsafePathError`]、[`PathError`]、[`OpError`]）：消息文本和
//!    `error.name` 是 wire 兼容性的一部分，测试逐字锁定，改动它们等于改动协议。
//! 2. **校验**：`assert_safe_path` 与两套词汇表各自的入口——`assert_valid_op`
//!    （decoded，路径内联）和 [`assert_valid_wire_op`]（wire，允许 id 引用与短形式）。
//! 3. **应用器**：[`apply`] 及其不校验变体 `apply_unvalidated`。`s`/`d`/`a`/`t` 共用
//!    “解析父容器 → 检查最后一段 → 读/写/删”的前导，`p`/`m` 直接解析目标容器。
//! 4. **codec**：[`Encoder`] 做路径 interning（第二次使用才分配 id）与相邻路径省略；
//!    [`Decoder`] 按同一套规则还原。
//!
//! ## apply 的执行流程
//!
//! 对批次里的每条 op 依次执行：`assert_valid_op`（`apply_unvalidated` 会关闭）→ 若为
//! `r` 则替换根并继续 → 否则解析目标并写入。`root` 以所有权传入，出错时已应用的部分
//! 随 `root` 一起被丢弃，调用方看不到半成品；需要保留原值的调用方必须在调用前克隆。
//!
//! ## codec 的状态约束
//!
//! [`Encoder`] 与 [`Decoder`] 都持有跨批次状态（路径 id 表），必须**成对、按相同顺序**
//! 消费同一个状态流。`r` 是恢复点，两端遇到它都会清空 id 表；省略路径（短形式）只在
//! 单个批次内有效。`encode`/`decode` 两个便捷函数每次新建实例，只适合单批、自包含的
//! 往返；长期复用 encoder 时，后续批次可能引用更早批次定义的 id，解码端也必须复用
//! 同一个 [`Decoder`]。
//!
//! ## 失败模式
//!
//! - 结构或语法错误 → [`OpError::Invalid`]（TS `TypeError` 文案）；路径不可解析 →
//!   [`OpError::Path`]；保留段或非法数组下标 → [`OpError::UnsafePath`]。
//! - [`apply`] 只接受 decoded op；wire 数据必须先 [`decode`]。直接构造 [`Op`] 可绕过
//!   [`parse_op`]，但 [`apply`] 仍会重新校验每一条。
//! - [`Decoder::decode`] 中途失败时，失败前已处理、且未被后续 `r` 清空的 `#` 定义仍
//!   留在 id 表里；批次内的 `previous` 是局部变量，随调用丢弃。

use std::collections::{HashMap, HashSet};

use serde_json::{Number, Value};

use crate::delta::{Op, Path, Seg};

/// wire 形式的一条操作（TS: `WireOp`）。JSON 元组：`["r", …]`、`["s", path|id, …]`、
/// 省略路径的短形式、以及 `["#", id, path]`。
pub type WireOp = Value;

/// 保留段：可到达原型链的路径段（TS: `RESERVED_SEGMENTS`）。
///
/// 这三个字符串只作为**路径段**被拒绝；出现在 `s` 的 payload 里时仍是普通数据
/// （`tests/delta_ops.rs` 的 `reserved_names_inside_values_are_plain_keys` 锁定）。
/// Rust 的 `serde_json::Value` 没有可执行的原型链，但本文件与 TS 逐函数对应，两端必须
/// 接受/拒绝同一组路径，因此这里保持与 TS 相同的集合，而不是按 Rust 的能力放宽。
/// 需要“自产 op、不做路径校验”的场景走 `crate::delta::apply_immutable_trusted`。
pub const RESERVED_SEGMENTS: [&str; 3] = ["__proto__", "constructor", "prototype"];

/// TS `RESERVED_SEGMENTS.has(seg)`。
pub fn is_reserved_segment(segment: &str) -> bool {
    RESERVED_SEGMENTS.contains(&segment)
}

// ─── 路径错误 ────────────────────────────────────────────────────────────────

/// TS `UnsafePathError`：`name = "UnsafePathError"`，消息 `` `unsafe path segment: ${String(segment)}` ``。
///
/// 触发来源都是路径段本身不合法：命中 [`RESERVED_SEGMENTS`]；数组父容器上的非下标段或
/// 负数下标；写路径与父容器检查中 `index > len` 的下标；以及 wire 解析时出现的
/// 非字符串/非负整数元素。消息用 JS 的 `String()` 而不是 `JSON.stringify()`：字符串段
/// 因此不带引号，数组/对象按 `String()` 规则展开。
#[derive(Debug, Clone, PartialEq)]
pub struct UnsafePathError {
    /// 违规的路径段（TS 的 `Seg`；运行期也可能是任意值，因此用 JSON 保存）。
    ///
    /// 来自已解码路径时是 [`Seg`] 的 JSON 形态；来自 wire 校验时保留原始值（可能是
    /// 对象、布尔、`null` 等 TS 运行期才可能出现的段）。
    pub segment: Value,
}

impl UnsafePathError {
    /// 用已解码路径段构造。
    pub fn from_seg(segment: &Seg) -> Self {
        UnsafePathError {
            segment: seg_to_json(segment),
        }
    }

    /// 用原始 JSON 值构造（wire 校验路径）。
    ///
    /// 值原样保存，不做规范化；`message()` 对它的 `String()` 结果与 TS 对同一值
    /// 调用 `String()` 一致（数字走 JS 数字格式，字符串原样，对象为 `[object Object]`）。
    pub fn from_value(segment: &Value) -> Self {
        UnsafePathError {
            segment: segment.clone(),
        }
    }

    /// `` `unsafe path segment: ${String(segment)}` ``（`String()` 而非 `JSON.stringify()`）。
    pub fn message(&self) -> String {
        format!(
            "unsafe path segment: {}",
            kiana_json::js_string::js_string(Some(&self.segment))
        )
    }

    /// TS 的 `error.name`。
    pub fn name(&self) -> &'static str {
        "UnsafePathError"
    }
}

impl std::fmt::Display for UnsafePathError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message())
    }
}

impl std::error::Error for UnsafePathError {}

/// TS `PathError`：`path` 是路径或（wire 解码时的）数字 id；消息
/// `` `unresolvable path: ${JSON.stringify(path)}` ``。
///
/// 与 `UnsafePathError` 的分工：路径**语法/安全**不合法用后者；路径在值里走不通
/// （中间节点不存在、不是容器、中间数组下标越界、`a`/`t` 的目标不是字符串）用前者。
/// 数组末尾段的越界规则不同：`index == len` 时 `d`/`a`/`t` 报前者；`index > len`
/// 属于 [`UnsafePathError`]，由 `check_parent_key`（写路径还有 `write_value` 兜底）
/// 在应用前拒绝，例如对长度为 1 的数组写 `Index(2)`。
#[derive(Debug, Clone, PartialEq)]
pub struct PathError {
    /// 路径或数字 id（JSON 形态）。
    ///
    /// 由 [`PathError::new`] 构造时是 JSON 数组；由 [`PathError::from_id`] 或 decoder
    /// 的未解析 id 构造时是裸数字。`message()` 用 `JSON.stringify`，因此数组/字符串
    /// 带引号，而数字不带引号（`unresolvable path: 3`）。
    pub path: Value,
}

impl PathError {
    /// 用路径构造。
    pub fn new(path: &[Seg]) -> Self {
        PathError {
            path: path_to_json(path),
        }
    }

    /// 用数字 id 构造（`unresolvable path: 3`）。
    pub fn from_id(id: i64) -> Self {
        PathError {
            path: Value::Number(Number::from(id)),
        }
    }

    /// 完整消息。
    pub fn message(&self) -> String {
        format!("unresolvable path: {}", serialize_json(&self.path))
    }

    /// TS 的 `error.name`。
    pub fn name(&self) -> &'static str {
        "PathError"
    }
}

impl std::fmt::Display for PathError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message())
    }
}

impl std::error::Error for PathError {}

/// 操作层的全部错误：TS 的 `TypeError`（静态消息）、`PathError`、`UnsafePathError`
/// 与未知 verb（`` `unknown op verb: ${String(op[0])}` ``）。
///
/// # 映射与不变量
///
/// - [`OpError::name`] 给出 TS 的 `error.name`：前两个变体都是 `"TypeError"`；
/// - `Display` 等于 `message()`，只输出消息本身，不带 `name` 前缀；
/// - `Invalid` 只装静态文案，因此可以逐字比对 TS；需要拼接值的消息走 `UnknownVerb`
///   或嵌套的具体错误类型；
/// - `From<PathError>` / `From<UnsafePathError>` 已实现，应用器内部可以直接 `?`。
#[derive(Debug, Clone, PartialEq)]
pub enum OpError {
    /// `TypeError` 系：消息是静态串（`r arity`、`path is empty` …）。
    Invalid(&'static str),
    /// 未知 verb。
    UnknownVerb(String),
    /// `PathError`。
    Path(PathError),
    /// `UnsafePathError`。
    UnsafePath(UnsafePathError),
}

impl OpError {
    /// 完整消息（= TS `error.message`）。
    pub fn message(&self) -> String {
        match self {
            OpError::Invalid(message) => (*message).to_owned(),
            OpError::UnknownVerb(verb) => format!("unknown op verb: {verb}"),
            OpError::Path(error) => error.message(),
            OpError::UnsafePath(error) => error.message(),
        }
    }

    /// TS 的 `error.name`（`TypeError`/`PathError`/`UnsafePathError`）。
    pub fn name(&self) -> &'static str {
        match self {
            OpError::Invalid(_) | OpError::UnknownVerb(_) => "TypeError",
            OpError::Path(_) => "PathError",
            OpError::UnsafePath(_) => "UnsafePathError",
        }
    }
}

impl std::fmt::Display for OpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message())
    }
}

impl std::error::Error for OpError {}

impl From<PathError> for OpError {
    fn from(error: PathError) -> Self {
        OpError::Path(error)
    }
}

impl From<UnsafePathError> for OpError {
    fn from(error: UnsafePathError) -> Self {
        OpError::UnsafePath(error)
    }
}

// ─── 分类 ────────────────────────────────────────────────────────────────────

/// TS `isReplace`。
///
/// 只对 [`Op::Replace`] 返回 true；wire 形态没有对应类型，调用方需自行检查
/// `WireOp` 数组的第 0 项是否为 `"r"`。
pub fn is_replace(op: &Op) -> bool {
    matches!(op, Op::Replace(_))
}

/// TS `isBase`：批次以替换开头。
///
/// 只看第一条 op；空批次返回 false。`r` 在 decoded 与 wire 两种词汇表里编码相同，
/// 但本函数接收已解码的 [`Op`]，wire 批次应先 [`decode`]。
pub fn is_base(ops: &[Op]) -> bool {
    matches!(ops.first(), Some(Op::Replace(_)))
}

// ─── 字符串工具（JS 的 UTF-16 语义）──────────────────────────────────────────

/// JS 字符串的 UTF-16 码元序列。
fn utf16_units(text: &str) -> Vec<u16> {
    text.encode_utf16().collect()
}

/// 从 UTF-16 码元重建 `String`；孤立代理项在 Rust 里无法表示，用 U+FFFD 代替
/// （TS 能表示孤立代理项，这是记录在案的类型层差异）。
///
/// 只有切点落在代理对中间时才会触发替换；这种情况下 `String::from_utf16` 返回
/// `Err`，随后 `from_utf16_lossy` 把每个非法单元换成 U+FFFD。
fn from_utf16_units(units: &[u16]) -> String {
    String::from_utf16(units).unwrap_or_else(|_| String::from_utf16_lossy(units))
}

/// `text.slice(start, end)`（UTF-16 码元）。
///
/// - `start`/`end` 的单位是 UTF-16 码元，不是字节，也不是 Unicode 标量；
/// - 两者都按长度钳制，`end` 还会被抬到不小于 `start`，因此 `start > end` 返回空串，
///   越界下标不会 panic；
/// - `end = None` 表示取到串尾；
/// - 签名用 `usize`，无法表达 JS 的负下标（负值表示从尾部倒数）。本模块的 `t` 只接受
///   非负 count，不需要该语义；
/// - 切在代理对中间时，孤立的半个代理项用 U+FFFD 代替（见模块文档的差异说明）。
pub fn utf16_slice(text: &str, start: usize, end: Option<usize>) -> String {
    let units = utf16_units(text);
    let start = start.min(units.len());
    let end = end.unwrap_or(units.len()).min(units.len()).max(start);
    from_utf16_units(&units[start..end])
}

/// `text.length`（UTF-16 码元数，不是字节数）。
pub fn utf16_len(text: &str) -> usize {
    text.encode_utf16().count()
}

/// TS `ownedSlice(text, start, end = text.length)`：返回值等值的独立字符串
/// （TS 里用 `+\` \`.slice(0,-1)` 强制 V8 复制；Rust 的 `String` 本就独立）。
///
/// 语义与 [`utf16_slice`] 相同，保留这个函数只是为了让 TS 的调用点一一对应。
/// `end = None` 表示取到串尾；返回的 `String` 与入参不共享内存。
pub fn owned_slice(text: &str, start: usize, end: Option<usize>) -> String {
    utf16_slice(text, start, end)
}

/// TS `overlap(a, b, scan)`：`a` 的后缀同时是 `b` 的前缀的最长长度。
///
/// `scan` 限制只看 `a` 末尾多少个 UTF-16 码元；`probe = 64`、`max_candidates = 8`
/// 是 TS 的默认参数，需要别的上限请用 [`overlap_with`]。返回 0 表示没有可用的重叠
/// （或候选数超限而放弃），调用方 `delta::diff` 在 0 时退回整串替换，仍然正确。
pub fn overlap(a: &str, b: &str, scan: usize) -> usize {
    overlap_with(a, b, scan, 64, 8)
}

/// 带 `probe`/`maxCandidates` 参数的 `overlap`。
///
/// # 算法
///
/// 1. 取 `a` 末尾 `scan` 个码元（不足则整个 `a`）；任一串为空或 `scan == 0` 时返回 0。
/// 2. 第一轮用长度 `min(probe, b.len())` 的 `b` 头部在 `tail` 中找候选位置；第二轮
///    退化为单码元头部。长头部候选少、能快速命中大重叠；单码元头部用于发现比 `probe`
///    短的重叠，但仍受 `max_candidates` 限制。
/// 3. 对每个候选位置 `found` 检查 `tail[found..]` 是否恰为 `b` 的前缀；是则返回
///    `tail.len() - found`。
/// 4. 每轮超出该轮上限或没有候选时结束该轮，两轮都失败才返回 0。
///
/// # 不变量与边界
///
/// 返回非 0 的 `n` 一定满足：`a` 的末 `n` 个 UTF-16 码元等于 `b` 的前 `n` 个码元。
/// 重复字符很多时（构建日志、同一字符的长串）候选会超过上限，函数放弃该轮；
/// 两轮都没有结果时返回 0：0 只表示“没有可用的重叠”，不保证真的不存在重叠，
/// 调用方必须按退化路径处理。
/// 比较全部在 UTF-16 码元上进行，与 JS `indexOf` 的单位一致；切点可能落在代理对
/// 中间，但本函数只比较 `u16`，不构造 `String`。
pub fn overlap_with(a: &str, b: &str, scan: usize, probe: usize, max_candidates: usize) -> usize {
    let a_units = utf16_units(a);
    let b_units = utf16_units(b);
    if a_units.is_empty() || b_units.is_empty() || scan == 0 {
        return 0;
    }
    let tail: &[u16] = if a_units.len() > scan {
        &a_units[a_units.len() - scan..]
    } else {
        &a_units
    };
    let candidates = [probe.min(b_units.len()), 1];
    for (position, h) in candidates.iter().copied().enumerate() {
        // 长头部优先；`h = 1` 是兜底轮，能覆盖比 `probe` 短的重叠。
        let head = &b_units[..h];
        let mut tried = 0usize;
        let mut k = 0usize;
        while k + head.len() <= tail.len() {
            let found = (k..=tail.len() - head.len())
                .find(|&index| tail[index..index + head.len()] == *head);
            let Some(found) = found else { break };
            tried += 1;
            // 上限是“尝试的候选数”，不是结果长度；放弃只结束当前轮。
            if tried > max_candidates {
                break;
            }
            let n = tail.len() - found;
            if n <= b_units.len() && tail[found..] == b_units[..n] {
                return n;
            }
            k = found + 1;
        }
        // 第二个候选（h = 1）后结束。
        if position == 1 {
            break;
        }
    }
    0
}

// ─── 路径转换 ────────────────────────────────────────────────────────────────

/// 路径段转 JSON（`string | number`）。
///
/// 数字段写成 JSON 数字，字符串段写成 JSON 字符串，二者**不**互相转换。这是 wire 上
/// 路径的表示；对象属性名的生成走另一条规则（`seg_key`，数字按 JS 数字文本格式化），
/// 两者用途不同，不能互换。
pub fn seg_to_json(seg: &Seg) -> Value {
    match seg {
        Seg::Key(key) => Value::String(key.clone()),
        Seg::Index(index) => Value::Number(Number::from(*index)),
    }
}

/// 路径转 JSON 数组。
///
/// 按顺序转换每个段并分配新数组。编码器再对结果做 `JSON.stringify` 得到 interning
/// 键（`path_key`）：数字段与字符串段不同（`[3]` 与 `["3"]` 是两个键）。该键用于
/// encoder 区分路径身份、决定何时分配 id；decoder 只按数字 id 查表（`number_key`），
/// 不计算也不使用 `path_key`。
pub fn path_to_json(path: &[Seg]) -> Value {
    Value::Array(path.iter().map(seg_to_json).collect())
}

/// TS `JSON.stringify(value)`；不会失败（值总能序列化）。
///
/// 必须走 [`kiana_json::js_object::stringify`]：`serde_json::to_string` 的数字格式与
/// `JSON.stringify` 不同（例如 `1e21` 在 JS 是 `1e+21`），而本函数的输出会进入
/// `PathError` 的用户可见消息。
///
/// 另外两个调用点也依赖同一套文本：`PathError::message` 与 `path_key`（encoder 的
/// interning 键）。改动这里的格式会同时改变错误文案和 wire 路径的字典身份。
fn serialize_json(value: &Value) -> String {
    kiana_json::js_object::stringify(value)
}

/// 段作为对象键时的字符串（`parent[key]` 的 ToPropertyKey = `String(number)`）。
///
/// 数字段在 JS 里是 f64：`String(2 ** 56)` 是 `"72057594037927940"`，而不是精确整数
/// 展开的 `"72057594037927936"`。因此这里必须走 JS 的数字格式化，否则写入/查找会用
/// 一个 JS 永远不会产生的属性名（对端 op 的目标键也随之错位）。
///
/// 只在父容器是对象时使用；数组父容器要求 `Seg::Index`，由 `resolve_mut` 与
/// `check_parent_key` 在调用本函数前就拒绝非下标段。对象上 `Seg::Key("3")` 与
/// `Seg::Index(3)` 都落到键 `"3"`，与 JS 的属性键语义一致。
fn seg_key(seg: &Seg) -> String {
    match seg {
        Seg::Key(key) => key.clone(),
        Seg::Index(index) => kiana_json::js_num::format_js_number(*index as f64),
    }
}

// ─── 校验 ────────────────────────────────────────────────────────────────────

/// TS `assertSafePath(path)`。
///
/// 逐段检查：字符串段命中 [`RESERVED_SEGMENTS`] → [`UnsafePathError`]；`Seg::Index`
/// 为负 → [`UnsafePathError`]。错误里保存违规段的 JSON 形态，消息按 `String()` 生成。
///
/// 不检查路径是否为空，也不检查段数：非空约束由各 verb 的校验器决定（`s`/`d`/`a`/`t`
/// 要求非空，`p`/`m` 允许空）。`Seg::Index` 是 `i64`，所以“非整数下标”在类型上不存在；
/// 来自 wire 的非整数数字在 `parse_path_value`（经 `number_to_seg`）就已被拒绝。
pub fn assert_safe_path(path: &[Seg]) -> Result<(), OpError> {
    for seg in path {
        match seg {
            Seg::Key(key) => {
                if is_reserved_segment(key) {
                    return Err(UnsafePathError::from_seg(seg).into());
                }
            }
            Seg::Index(index) => {
                if *index < 0 {
                    return Err(UnsafePathError::from_seg(seg).into());
                }
            }
        }
    }
    Ok(())
}

/// 判断 `permutation` 是否恰为 `0..len` 的一个排列（双射）。
///
/// 越界或重复都返回 false；`seen[index]` 的越界访问由 `||` 的短路保证不会发生
/// （`index >= permutation.len()` 时不会走到 `seen[index]`）。复杂度 O(len)。
fn is_permutation(permutation: &[usize]) -> bool {
    let mut seen = vec![false; permutation.len()];
    for &index in permutation {
        if index >= permutation.len() || seen[index] {
            return false;
        }
        seen[index] = true;
    }
    true
}

// `assert_valid_op` 与 `parse_op` 共用的路径前导：先查非空（`path is empty`），
// 再查保留段/负下标。先查非空让空路径得到更具体的 `path is empty`；`assert_safe_path`
// 对空切片本就返回 `Ok`，不会产生 `UnsafePathError`。
fn assert_path_arg(path: &[Seg], non_empty: bool) -> Result<(), OpError> {
    if non_empty && path.is_empty() {
        return Err(OpError::Invalid("path is empty"));
    }
    assert_safe_path(path)
}

/// TS `assertValidOp(op)`：校验**已解码** op 的 verb、arity 与 payload 形状。
///
/// 因为 [`Op`] 是强类型枚举，TS 里的大部分运行期检查已由类型承担（payload 的 JSON
/// 形态、`Append` 的文本、`Splice`/`Truncate` 的非负整数等），这里只保留类型无法表达
/// 的部分：
///
/// - `s`/`d`/`a`/`t` 的路径必须非空且安全；
/// - `p`/`m` 的路径可以为空（被跟踪的值本身可以是数组），`p` 的下标在应用时按数组
///   长度钳制，`m` 的排列必须是双射；
/// - 保留段与负下标统一由 [`assert_safe_path`] 拒绝。
///
/// [`apply`] 在应用每条 op 前调用本函数；直接构造的 [`Op`] 也会在这里被拦住。
pub fn assert_valid_op(op: &Op) -> Result<(), OpError> {
    match op {
        // `r` 没有路径，payload 可以是任意 JSON，没有可校验的形状。
        Op::Replace(_) => Ok(()),
        Op::Set(path, _) => assert_path_arg(path, true),
        Op::Delete(path) => assert_path_arg(path, true),
        Op::Append(path, _) => assert_path_arg(path, true),
        Op::Truncate(path, _) => assert_path_arg(path, true),
        Op::Splice { path, .. } => {
            // TS 只要求 `Number.isInteger(index) && index >= 0`：任意大的数字都合法，
            // 应用时由 `Array.prototype.splice` 按数组长度钳制（见 [`splice_range`]）。
            assert_path_arg(path, false)
        }
        Op::Permute(path, permutation) => {
            // 空路径合法：目标是根本身的数组。排列本身还要过双射检查。
            assert_path_arg(path, false)?;
            if !is_permutation(permutation) {
                return Err(OpError::Invalid("m permutation is not a bijection"));
            }
            Ok(())
        }
    }
}

/// JS `Number.isInteger(value)` 且非负；返回 `u64`（饱和到 `u64::MAX` 的安全范围）。
///
/// - 非数字 JSON 值（字符串/布尔/`null`/数组/对象）→ `None`（`as_f64` 失败）；
/// - 非有限值、负数、小数 → `None`；`-0.0` 视作 0（JS 同样接受）；
/// - `>= 2^64` 的整数饱和为 `u64::MAX`：调用方只把它当“很大的计数/下标”，后续按容器
///   长度钳制，不做精确算术。`number_key` 用同一判定但保留 f64 位模式，不经过饱和。
fn non_negative_integer(value: &Value) -> Option<u64> {
    let number = value.as_f64()?;
    if !number.is_finite() || number < 0.0 || number.fract() != 0.0 {
        return None;
    }
    if number >= u64::MAX as f64 {
        return Some(u64::MAX);
    }
    Some(number as u64)
}

/// JS `Map<number, _>` 的键：任何非负整数（含超出 `u64` 的 f64 整数）。
///
/// TS 的 decoder 用 JavaScript 数字作 `paths` 的键，`2e19` 与 `3e19` 是两个不同的
/// 键；用 `Number.isInteger` 判定后取 f64 的位模式，能保持这一语义。`-0` 与 `0` 在
/// JS 的 `Map` 里是同一个键（SameValueZero），这里同样归一。
///
/// 不用整数而用位模式还有一个原因：JS 的 Number 是 f64，超过 2^53 的整数（如
/// `2^56` 与 `2^56 + 1`）在 JS 里是同一个值，位模式恰好复刻这种相等性；不同 f64 的
/// 位模式不同，因此不需要额外的冲突处理。非有限值、负数、小数返回 `None`，与
/// `non_negative_integer` 的接受集合一致。
fn number_key(value: &Value) -> Option<u64> {
    let number = value.as_f64()?;
    if !number.is_finite() || number < 0.0 || number.fract() != 0.0 {
        return None;
    }
    let number = if number == 0.0 { 0.0 } else { number };
    Some(number.to_bits())
}

/// JS 数字转路径段：非负整数 → `Index`。
///
/// TS 接受任意非负整数段。超过 `i64` 的整数在 [`Seg::Index`] 里放不下，但它在对象上
/// 就是属性键 `String(number)`（例如 `1e20` → `"100000000000000000000"`、`1e21` →
/// `"1e+21"`），因此保留为 [`Seg::Key`]：对象写入/读取按普通属性键处理；数组父容器随后按
/// "非数字段"报 `UnsafePathError`，消息里的 `String(segment)` 也相同。
///
/// 负数、小数、非数字值返回 `None`；错误类型由调用方决定（`parse_path_value` 转成
/// `UnsafePathError`，`path_from_json` 直接变成 `None`）。
fn number_to_seg(value: &Value) -> Option<Seg> {
    let number = value.as_f64()?;
    if !number.is_finite() || number < 0.0 || number.fract() != 0.0 {
        return None;
    }
    // `i64::MAX as f64` 恰是 2^63；等于它的 f64 已经超出 i64 的精确范围（`as i64` 会
    // 饱和成 `i64::MAX`，键文本会与 JS 的 `String(2^63)` 不同）。
    if number >= i64::MAX as f64 {
        return Some(Seg::Key(kiana_json::js_num::format_js_number(number)));
    }
    Some(Seg::Index(number as i64))
}

/// 解析 JSON 路径数组为 `Path`。
///
/// 任何失败都折叠成 `None`（不区分“不是数组”与“某个段非法”），适合只关心成功/失败的
/// 调用方；需要 TS 的错误名或消息时用 `parse_path_value` 或 [`parse_op`]。
/// **不检查保留段**：`["__proto__"]` 能解析成功，调用方若在写入路径上使用，必须自己
/// 再调 [`assert_safe_path`]。数字段必须是有限的非负整数（经 `number_to_seg`）。
pub fn path_from_json(value: &Value) -> Option<Path> {
    let items = value.as_array()?;
    let mut path = Vec::with_capacity(items.len());
    for item in items {
        match item {
            Value::String(key) => path.push(Seg::Key(key.clone())),
            Value::Number(_) => path.push(number_to_seg(item)?),
            _ => return None,
        }
    }
    Some(path)
}

/// 解析 wire 路径数组并逐段校验；非法段报 `UnsafePathError`（携带原始段值）。
///
/// 与 `path_from_json` 的差别是错误形态：非数组 → `TypeError("path is not an array")`，
/// 非字符串/非负整数元素 → `UnsafePathError` 并保留原始值。**不检查保留段**，调用方
/// （`check_ref`、`assert_safe_json_path`）随后调 [`assert_safe_path`]；空数组返回空
/// 路径，是否合法由具体 verb 决定。
fn parse_path_value(value: &Value) -> Result<Path, OpError> {
    let Some(items) = value.as_array() else {
        return Err(OpError::Invalid("path is not an array"));
    };
    let mut path = Vec::with_capacity(items.len());
    for item in items {
        match item {
            Value::String(key) => path.push(Seg::Key(key.clone())),
            Value::Number(_) => match number_to_seg(item) {
                Some(segment) => path.push(segment),
                None => return Err(UnsafePathError::from_value(item).into()),
            },
            other => return Err(UnsafePathError::from_value(other).into()),
        }
    }
    Ok(path)
}

// `#` 定义与 decoder 的共用路径入口：解析并校验保留段，返回可存进 id 表的路径。
fn assert_safe_json_path(value: &Value) -> Result<Path, OpError> {
    let path = parse_path_value(value)?;
    assert_safe_path(&path)?;
    Ok(path)
}

/// `okRef`：wire 路径引用（内联数组或非负整数 id）。
///
/// - 数字：必须是有限非负整数（任意大小都可以，id 用 f64 位模式保存），否则
///   `TypeError("bad path id")`；
/// - 字符串：直接拒绝为 `path is not an array`。TS 里 `"a".slice(0, -1)` 得到 `""`，
///   会把字符串当成根路径写错位置，因此不能尝试“解析”字符串；
/// - 数组：按 `parse_path_value` 解析并逐段 [`assert_safe_path`]；空数组在这里合法，
///   是否允许空路径由 verb 与 decoder 决定。
fn check_ref(value: &Value) -> Result<(), OpError> {
    if value.is_number() {
        if non_negative_integer(value).is_none() {
            return Err(OpError::Invalid("bad path id"));
        }
        return Ok(());
    }
    // 字符串不是路径：`"a".slice(0,-1)` 是 `""`，会解析成根。
    if value.as_array().is_none() {
        return Err(OpError::Invalid("path is not an array"));
    }
    let path = parse_path_value(value)?;
    assert_safe_path(&path)
}

/// TS `assertValidWireOp(op)`：wire 语法（id 引用与短形式合法）。
///
/// 只校验**语法**：不解析 id 是否已定义（那是 [`Decoder`] 的职责），也不检查应用期的
/// 容器类型。两套校验器刻意分开：wire 允许数字引用与省略路径的短形式，而 decoded
/// 语法不允许；用同一套规则会让 `["s", value]` 这类短形式被 [`apply`] 误读。
///
/// 错误选择固定如下：元组不是非空数组 → `op is not a tuple`；首元素不是字符串 →
/// `UnknownVerb(String(op[0]))`；数字引用非法 → `bad path id`；非数组引用 →
/// `path is not an array`；保留段/非法段 → [`UnsafePathError`]。短形式只按 arity
/// 识别，具体是否有“上一条路径”由 decoder 检查。
pub fn assert_valid_wire_op(op: &Value) -> Result<(), OpError> {
    let Some(items) = op.as_array() else {
        return Err(OpError::Invalid("op is not a tuple"));
    };
    let Some(verb) = items.first() else {
        return Err(OpError::Invalid("op is not a tuple"));
    };
    let verb = match verb {
        Value::String(verb) => verb.as_str(),
        other => {
            // 非字符串 verb 不存在“未知字符串”这一说，错误消息仍是 `String(op[0])`。
            return Err(OpError::UnknownVerb(kiana_json::js_string::js_string(
                Some(other),
            )));
        }
    };
    match verb {
        "r" => {
            // 只有 2 元；payload 可以是任意 JSON。
            if items.len() != 2 {
                return Err(OpError::Invalid("r arity"));
            }
        }
        "s" => {
            // 3 元是 `[ref, value]`；2 元是省略路径的短形式 `[value]`。
            if items.len() == 3 {
                check_ref(&items[1])?;
            } else if items.len() != 2 {
                return Err(OpError::Invalid("s arity"));
            }
        }
        "d" => {
            // 2 元带 ref；1 元是短形式。
            if items.len() == 2 {
                check_ref(&items[1])?;
            } else if items.len() != 1 {
                return Err(OpError::Invalid("d arity"));
            }
        }
        "a" => {
            // 3 元是 `[ref, text]`；2 元是短形式 `[text]`。文本必须是字符串。
            if items.len() == 3 {
                check_ref(&items[1])?;
                if !items[2].is_string() {
                    return Err(OpError::Invalid("a value"));
                }
            } else if items.len() == 2 {
                if !items[1].is_string() {
                    return Err(OpError::Invalid("a value"));
                }
            } else {
                return Err(OpError::Invalid("a arity"));
            }
        }
        "t" => {
            // 3 元是 `[ref, count]`；2 元是短形式 `[count]`。count 必须是非负整数。
            if items.len() == 3 {
                check_ref(&items[1])?;
                if non_negative_integer(&items[2]).is_none() {
                    return Err(OpError::Invalid("t count"));
                }
            } else if items.len() == 2 {
                if non_negative_integer(&items[1]).is_none() {
                    return Err(OpError::Invalid("t count"));
                }
            } else {
                return Err(OpError::Invalid("t arity"));
            }
        }
        "p" => {
            // 5 元带 ref，4 元是短形式；index/remove 都按 JS 的 Number.isInteger + >= 0
            // 判定，items 必须是数组。
            let (index, remove, payload) = if items.len() == 5 {
                check_ref(&items[1])?;
                (&items[2], &items[3], &items[4])
            } else if items.len() == 4 {
                (&items[1], &items[2], &items[3])
            } else {
                return Err(OpError::Invalid("p arity"));
            };
            if non_negative_integer(index).is_none() {
                return Err(OpError::Invalid("p index"));
            }
            if non_negative_integer(remove).is_none() {
                return Err(OpError::Invalid("p remove"));
            }
            if !payload.is_array() {
                return Err(OpError::Invalid("p items"));
            }
        }
        "m" => {
            // 3 元带 ref，2 元是短形式；排列在最后一项。
            if items.len() == 3 {
                check_ref(&items[1])?;
            } else if items.len() != 2 {
                return Err(OpError::Invalid("m arity"));
            }
            let Some(permutation) = items.last().and_then(Value::as_array) else {
                return Err(OpError::Invalid("m permutation is not an array"));
            };
            let mut values = Vec::with_capacity(permutation.len());
            for value in permutation {
                let Some(index) = non_negative_integer(value) else {
                    return Err(OpError::Invalid("m permutation is not a bijection"));
                };
                // 防止 `as usize` 在 32 位平台截断出一个恰好合法的小下标；截断后的值
                // 可能骗过后面的双射检查。64 位平台这条分支不可达。
                if index > usize::MAX as u64 {
                    return Err(OpError::Invalid("m permutation is not a bijection"));
                }
                values.push(index as usize);
            }
            if !is_permutation(&values) {
                return Err(OpError::Invalid("m permutation is not a bijection"));
            }
        }
        "#" => {
            // `[#, id, path]`：id 非负整数，path 必须是安全的内联数组。id 是否重复定义
            // 不在这里检查（decoder 直接覆盖旧值）。
            if items.len() != 3 || non_negative_integer(&items[1]).is_none() || !items[2].is_array()
            {
                return Err(OpError::Invalid("# shape"));
            }
            assert_safe_json_path(&items[2])?;
        }
        _ => {
            // 未知 verb 必须显式报错；静默跳过会让新 producer 的 op 在旧 reader 里消失。
            return Err(OpError::UnknownVerb(kiana_json::js_string::js_string(
                Some(&items[0]),
            )));
        }
    }
    Ok(())
}

// ─── 已解码 op 与 JSON 的互转 ────────────────────────────────────────────────

/// 已解码 op → JSON 元组。
///
/// 产出的是 wire 的**完整形式**：路径始终内联，不使用数字 id、短形式或 `#`；因此结果
/// 是自包含的，可独立跨边界。`index`/`remove`/`count` 以 JSON 数字写出（`usize as u64`）。
///
/// 往返 `parse_op(op_to_json(op))` 对 wire 解析得到的 op 是恒等的（那些 `Seg::Index`
/// 本身就是 f64 可表示的值）；手工构造超出 f64 精确范围的 `Seg::Index` 会在解析时按
/// JS 数字语义舍入。需要压缩的跨边界编码请用 [`Encoder`]。
pub fn op_to_json(op: &Op) -> Value {
    match op {
        Op::Replace(value) => Value::Array(vec![Value::String("r".into()), value.clone()]),
        Op::Set(path, value) => Value::Array(vec![
            Value::String("s".into()),
            path_to_json(path),
            value.clone(),
        ]),
        Op::Delete(path) => Value::Array(vec![Value::String("d".into()), path_to_json(path)]),
        Op::Append(path, text) => Value::Array(vec![
            Value::String("a".into()),
            path_to_json(path),
            Value::String(text.clone()),
        ]),
        Op::Truncate(path, count) => Value::Array(vec![
            Value::String("t".into()),
            path_to_json(path),
            Value::Number(Number::from(*count as u64)),
        ]),
        Op::Splice {
            path,
            index,
            remove,
            items,
        } => Value::Array(vec![
            Value::String("p".into()),
            path_to_json(path),
            Value::Number(Number::from(*index as u64)),
            Value::Number(Number::from(*remove as u64)),
            Value::Array(items.clone()),
        ]),
        Op::Permute(path, permutation) => Value::Array(vec![
            Value::String("m".into()),
            path_to_json(path),
            Value::Array(
                permutation
                    .iter()
                    .map(|index| Value::Number(Number::from(*index as u64)))
                    .collect(),
            ),
        ]),
    }
}

/// `assertValidOp` + 转成 [`Op`]（输入是 JSON 元组）。
///
/// 这是 **decoded** 词汇表的入口：路径必须内联。`#` 在这里是未知 verb（得到
/// `unknown op verb: #`）；省略路径的短形式（如 `["s", value]`、`["d"]`）则命中已知
/// verb 分支，因 arity 或形状不符报 `s arity`、`d arity`、`a shape` 等错误。wire 数据
/// 应先 [`decode`]。错误文案对应 TS 的 `assertValidOp`，因此与 [`assert_valid_wire_op`]
/// 不同（例如 `a shape`、`t shape` 而不是 `a value`、`t count`）。
///
/// 校验顺序按 verb 而定：每条先查 arity（`a`/`t` 的长度不符也报 `a shape`/`t shape`）；
/// `a`/`t` 随后先查 payload 形状再解析路径，`p`/`m` 先解析路径，再查
/// `index`/`remove`/`items`（`m` 则查排列数组与双射），`s`/`d` 在 arity 之后直接
/// 解析路径。`s`/`d`/`a`/`t` 要求非空路径，`p`/`m` 允许空。`usize` 字段的转换失败时
/// 饱和为 `usize::MAX`（超大计数在应用时本来就会被容器长度钳制）。
pub fn parse_op(value: &Value) -> Result<Op, OpError> {
    let items = value
        .as_array()
        .ok_or(OpError::Invalid("op is not a tuple"))?;
    if items.is_empty() {
        return Err(OpError::Invalid("op is not a tuple"));
    }
    let verb = match &items[0] {
        Value::String(verb) => verb.clone(),
        other => {
            return Err(OpError::UnknownVerb(kiana_json::js_string::js_string(
                Some(other),
            )));
        }
    };
    // 路径参数统一走这里：解析（拒绝非法段）→ 非空（按 verb）→ 保留段/负下标。
    let path_at = |index: usize, non_empty: bool| -> Result<Path, OpError> {
        let path = parse_path_value(&items[index])?;
        assert_path_arg(&path, non_empty)?;
        Ok(path)
    };
    match verb.as_str() {
        "r" => {
            if items.len() != 2 {
                return Err(OpError::Invalid("r arity"));
            }
            Ok(Op::Replace(items[1].clone()))
        }
        "s" => {
            if items.len() != 3 {
                return Err(OpError::Invalid("s arity"));
            }
            Ok(Op::Set(path_at(1, true)?, items[2].clone()))
        }
        "d" => {
            if items.len() != 2 {
                return Err(OpError::Invalid("d arity"));
            }
            Ok(Op::Delete(path_at(1, true)?))
        }
        "a" => {
            // 形状检查先于路径：长度或文本类型不对时报 `a shape`，
            // 不会先触达路径解析（与 TS 的 `||` 求值顺序一致）。
            if items.len() != 3 || !items[2].is_string() {
                return Err(OpError::Invalid("a shape"));
            }
            Ok(Op::Append(
                path_at(1, true)?,
                items[2].as_str().unwrap_or_default().to_owned(),
            ))
        }
        "t" => {
            if items.len() != 3 {
                return Err(OpError::Invalid("t shape"));
            }
            // count 是 `slice(count)` 的丢弃数，必须是非负整数；超大值饱和处理。
            let count = non_negative_integer(&items[2]).ok_or(OpError::Invalid("t shape"))?;
            Ok(Op::Truncate(
                path_at(1, true)?,
                usize::try_from(count).unwrap_or(usize::MAX),
            ))
        }
        "p" => {
            if items.len() != 5 {
                return Err(OpError::Invalid("p arity"));
            }
            // decoded 词汇表只有 5 元形式（无 id、无短形式）；下标允许任意大，
            // 应用时按数组长度钳制。
            let path = path_at(1, false)?;
            let index = non_negative_integer(&items[2]).ok_or(OpError::Invalid("p index"))?;
            let remove = non_negative_integer(&items[3]).ok_or(OpError::Invalid("p remove"))?;
            let Some(payload) = items[4].as_array() else {
                return Err(OpError::Invalid("p items"));
            };
            Ok(Op::Splice {
                path,
                index: usize::try_from(index).unwrap_or(usize::MAX),
                remove: usize::try_from(remove).unwrap_or(usize::MAX),
                items: payload.clone(),
            })
        }
        "m" => {
            if items.len() != 3 {
                return Err(OpError::Invalid("m arity"));
            }
            let path = path_at(1, false)?;
            let Some(permutation) = items[2].as_array() else {
                return Err(OpError::Invalid("m permutation is not an array"));
            };
            let mut values = Vec::with_capacity(permutation.len());
            for value in permutation {
                let index = non_negative_integer(value)
                    .ok_or(OpError::Invalid("m permutation is not a bijection"))?;
                values.push(usize::try_from(index).unwrap_or(usize::MAX));
            }
            // 这里的双射检查同时挡住“饱和成 usize::MAX”的项（它们必然越界）。
            if !is_permutation(&values) {
                return Err(OpError::Invalid("m permutation is not a bijection"));
            }
            Ok(Op::Permute(path, values))
        }
        _ => Err(OpError::UnknownVerb(verb)),
    }
}

// ─── 应用器 ──────────────────────────────────────────────────────────────────

// 沿 `path` 逐段下钻，返回末端节点的可变引用。
// - 对象：段经 `seg_key` 转成 JS 属性名（数字段按 JS 数字格式化）；
// - 数组：只接受 `Seg::Index`，负下标 → UnsafePathError；越界/缺失 → PathError；
// - 标量或 `null`：无法继续下钻 → PathError。
// `full` 是完整路径，单独传入是因为递归时 `path` 已切成剩余段；错误消息必须携带原始
// 路径（与 TS 的 `resolve` 一致）。递归深度等于路径段数，本函数不限制深度。
fn resolve_mut<'a>(
    node: &'a mut Value,
    path: &[Seg],
    full: &[Seg],
) -> Result<&'a mut Value, OpError> {
    let Some((seg, rest)) = path.split_first() else {
        return Ok(node);
    };
    match node {
        Value::Object(map) => {
            let key = seg_key(seg);
            let child = map.get_mut(&key).ok_or_else(|| PathError::new(full))?;
            resolve_mut(child, rest, full)
        }
        Value::Array(items) => {
            let Seg::Index(index) = seg else {
                return Err(UnsafePathError::from_seg(seg).into());
            };
            if *index < 0 {
                return Err(UnsafePathError::from_seg(seg).into());
            }
            let child = items
                .get_mut(*index as usize)
                .ok_or_else(|| PathError::new(full))?;
            resolve_mut(child, rest, full)
        }
        _ => Err(PathError::new(full).into()),
    }
}

// 在 `resolve_mut` 之上要求末端是对象或数组：这是 `p`/`m` 的目标要求。
// 标量（字符串/数字/布尔/null）一律 PathError(full)。
fn resolve_container_mut<'a>(
    root: &'a mut Value,
    path: &[Seg],
    full: &[Seg],
) -> Result<&'a mut Value, OpError> {
    let node = resolve_mut(root, path, full)?;
    if node.is_object() || node.is_array() {
        Ok(node)
    } else {
        Err(PathError::new(full).into())
    }
}

/// JS `Array.prototype.splice(start, deleteCount)` 的区间计算：返回 `(start, delete_count)`，
/// 二者都已按数组长度钳制。
///
/// `start > len` → `(len, 0)`；`delete` 截到 `len - start`（`start` 先被钳制，所以减法
/// 不会下溢）。参数是 `usize`，JS 的负 `start`（从尾部计数）不可达：wire 校验只接受
/// 非负整数。
fn splice_range(length: usize, start: usize, delete: usize) -> (usize, usize) {
    let start = start.min(length);
    let delete = delete.min(length - start);
    (start, delete)
}

/// 在 `start` 处插入（JS `splice(start, 0, ...items)`，`start` 按当前长度钳制）。
///
/// `start` 超过当前长度时插到末尾；插入的是 `items` 的克隆，批次里的 payload 不被移动。
fn js_insert(vec: &mut Vec<Value>, start: usize, items: &[Value]) {
    let position = start.min(vec.len());
    vec.splice(position..position, items.iter().cloned());
}

/// TS `apply(target, ops)`：应用已解码 op，返回新根。
///
/// 接收 `root` 的所有权，逐条校验后应用；`r` 替换整个根。出错时返回 [`OpError`]，
/// 已应用的部分随被消耗的 `root` 一起丢弃，调用方看不到半成品（需要保留原值必须在
/// 调用前克隆）。`ops` 必须是 decoded 形态（内联路径）；wire 数据要先 [`decode`]。
///
/// 与 TS 的一处实现差异：`r` 的 payload 在这里被 clone 进结果，因为 `ops` 是借用切片；
/// 因此应用器不会修改批次里的值，而 TS 的 `apply` 会直接把 payload 对象放进结果、
/// 后续 op 会改到它。序列化结果相同。
///
/// # 错误
///
/// - [`OpError::Invalid`]：`path is empty` 等结构性错误；
/// - [`OpError::UnsafePath`]：保留段、数组上的非下标段、越过末尾一格的写；
/// - [`OpError::Path`]：路径中间节点缺失/不是容器、下标越界、`a`/`t` 目标不是字符串。
pub fn apply(root: Value, ops: &[Op]) -> Result<Value, OpError> {
    apply_inner(root, ops, true)
}

/// [`apply`] 的**不校验**版本：供 `applyImmutableTrusted` 使用（TS 的 trusted 应用器
/// 刻意不做 `assertValidOp`，例如 `__proto__` 作为普通数据键也能写入）。
///
/// # 仍然生效的检查
///
/// 关闭校验只跳过 `assert_valid_op`（保留段、排列双射等）；应用期的结构检查仍在：
/// 路径必须可解析、目标容器类型必须匹配、`m` 的长度必须相等、`p` 的下标按长度钳制、
/// 数组父容器上的非下标段仍报 `UnsafePathError`；`s`/`d`/`a`/`t` 的空路径也仍会报
/// `path is empty`。但 `m` 的排列若长度与数组相同、却含越界下标，本函数会因
/// `previous[source]` 越界而 panic——trusted 契约要求调用方只传入自产的合法 op。
/// 这与 TS 是行为差异：spec/chord-rest.md §2.31 的 `permuteTrusted` 只检查长度，
/// 越界项直接读到 `undefined` 并写回，不抛错；抛 `TypeError` 的是带 `assertValidOp`
/// 的校验版 apply（见 `apply_immutable_trusted` 的文档）。
pub(crate) fn apply_unvalidated(root: Value, ops: &[Op]) -> Result<Value, OpError> {
    apply_inner(root, ops, false)
}

fn apply_inner(root: Value, ops: &[Op], validate: bool) -> Result<Value, OpError> {
    // 逐条处理：校验（可关闭）发生在该条 op 的任何修改之前；此前成功的 op 已经写进
    // `root`，一旦返回 Err，整个 `root` 被丢弃，调用方观察不到部分结果。
    let mut root = root;
    for op in ops {
        if validate {
            assert_valid_op(op)?;
        }
        if let Op::Replace(value) = op {
            // 语义上是“采用”：结果拥有 payload 的内容，而不是继续在旧 root 上写入。
            // Rust 侧 `ops` 是借用切片、无法移动 payload，因此这里 clone；副作用是
            // 批次里的值不会被应用器修改（TS 的 `apply` 会把同一个对象放进结果，
            // 后续 op 会改到它）。两者序列化后的结果相同。
            root = value.clone();
            continue;
        }
        match op {
            // 上面已经处理并 `continue`，此分支只用于让 match 完整。
            Op::Replace(_) => unreachable!(),
            Op::Permute(path, permutation) => {
                // 空路径表示根；`p`/`m` 允许指向根（被跟踪的值本身可以是数组）。
                let target = if path.is_empty() {
                    &mut root
                } else {
                    resolve_container_mut(&mut root, path, path)?
                };
                let Value::Array(items) = target else {
                    return Err(PathError::new(path).into());
                };
                // 长度不等无法建立双射，按 TS 的规则统一报 PathError（不是 TypeError）。
                if items.len() != permutation.len() {
                    return Err(PathError::new(path).into());
                }
                // 先快照再覆写：permutation 允许引用任意位置，就地写会读到已被覆盖的值。
                let previous = items.clone();
                for (index, source) in permutation.iter().enumerate() {
                    items[index] = previous[*source].clone();
                }
            }
            Op::Splice {
                path,
                index,
                remove,
                items: payload,
            } => {
                let target = if path.is_empty() {
                    &mut root
                } else {
                    resolve_container_mut(&mut root, path, path)?
                };
                let Value::Array(items) = target else {
                    return Err(PathError::new(path).into());
                };
                // 删除区间先按 JS splice 的规则钳制；插入位置用原 index（可能超出长度），
                // 由 `js_insert` 的钳制保证它落在末尾，后续块因此保持顺序。
                let (start, delete) = splice_range(items.len(), *index, *remove);
                items.drain(start..start + delete);
                let chunk_size = 10_000usize;
                let mut offset = 0usize;
                while offset < payload.len() {
                    let end = (offset + chunk_size).min(payload.len());
                    js_insert(items, index.saturating_add(offset), &payload[offset..end]);
                    offset = end;
                }
            }
            Op::Set(path, value) => {
                // `s`/`d`/`a`/`t` 的共同前导：最后一段是 key，前面是父路径。
                // 父路径解析失败时，PathError 携带的是**父路径**而不是完整 path
                // （与 TS 的 `resolve(root, path.slice(0, -1))` 一致）。
                let (key, parent_path) =
                    path.split_last().ok_or(OpError::Invalid("path is empty"))?;
                let parent = resolve_container_mut(&mut root, parent_path, parent_path)?;
                check_parent_key(parent, key)?;
                write_value(parent, key, value.clone())?;
            }
            Op::Delete(path) => {
                let (key, parent_path) =
                    path.split_last().ok_or(OpError::Invalid("path is empty"))?;
                let parent = resolve_container_mut(&mut root, parent_path, parent_path)?;
                check_parent_key(parent, key)?;
                delete_value(parent, key, path)?;
            }
            Op::Append(path, text) => {
                let (key, parent_path) =
                    path.split_last().ok_or(OpError::Invalid("path is empty"))?;
                let parent = resolve_container_mut(&mut root, parent_path, parent_path)?;
                check_parent_key(parent, key)?;
                // 缺失或不是字符串都按同一个 PathError(完整 path) 处理（`undefined` 不是字符串）。
                let Some(Value::String(current)) = read_value(parent, key)? else {
                    return Err(PathError::new(path).into());
                };
                let next = format!("{current}{text}");
                write_value(parent, key, Value::String(next))?;
            }
            Op::Truncate(path, count) => {
                let (key, parent_path) =
                    path.split_last().ok_or(OpError::Invalid("path is empty"))?;
                let parent = resolve_container_mut(&mut root, parent_path, parent_path)?;
                check_parent_key(parent, key)?;
                let Some(Value::String(current)) = read_value(parent, key)? else {
                    return Err(PathError::new(path).into());
                };
                // TS 的 `t` 是 `value.slice(count)`：`count` 是**丢弃**的前缀 UTF-16 码元数，
                // 不是保留长度。切在代理对中间时用 U+FFFD 代替孤立代理项。
                let sliced = utf16_slice(current, *count, None);
                write_value(parent, key, Value::String(sliced))?;
            }
        }
    }
    Ok(root)
}

/// TS `applyOps` 在 `s`/`d`/`a`/`t` 的 switch 之前执行的数组检查：
/// `typeof key !== "number"` → `UnsafePathError(key)`；`assertIndexInRange(parent, key)`
/// （`index > parent.length`）→ `UnsafePathError(index)`。
///
/// `index === parent.length` 是允许的（`s` 追加一格）；`d`/`a`/`t` 随后各自以
/// `PathError` 失败（追加位置只对 `s` 有意义）。
///
/// 对象父容器直接通过：键不做类型或保留段限制（`apply_unvalidated` 故意允许
/// `__proto__` 作为普通数据键；校验版本已在 `assert_valid_op` 里查过路径）。
fn check_parent_key(parent: &Value, key: &Seg) -> Result<(), OpError> {
    if let Value::Array(items) = parent {
        let Seg::Index(index) = key else {
            return Err(UnsafePathError::from_seg(key).into());
        };
        if *index < 0 || usize::try_from(*index).map_or(true, |index| index > items.len()) {
            return Err(UnsafePathError::from_seg(key).into());
        }
    }
    Ok(())
}

/// 写入 `parent[key]`（`defineProperty` 语义：数组只接受索引，且不能越过末尾一格）。
///
/// - 对象：任何段都经 `seg_key` 转成属性名后插入；已存在则覆盖并保留原插入位置，
///   不存在则追加到末尾（与 JS 定义新属性、`Map::insert` 的行为一致）；
/// - 数组：只接受 `Seg::Index`；`index < len` 覆盖、`index == len` 追加一格，其余
///   （负数、超长、`Seg::Key`）→ [`UnsafePathError`]。上界检查与 `check_parent_key`
///   重复，是防御性写法，供未校验路径与内部直接调用兜底；
/// - 标量父容器：`PathError::new(&[])`（不可达：调用前 `resolve_container_mut`
///   已保证父容器是对象或数组）。
fn write_value(parent: &mut Value, key: &Seg, value: Value) -> Result<(), OpError> {
    match parent {
        Value::Array(items) => {
            let Seg::Index(index) = key else {
                return Err(UnsafePathError::from_seg(key).into());
            };
            if *index < 0 {
                return Err(UnsafePathError::from_seg(key).into());
            }
            let index = *index as usize;
            if index > items.len() {
                return Err(UnsafePathError::from_seg(key).into());
            }
            if index == items.len() {
                items.push(value);
            } else {
                items[index] = value;
            }
            Ok(())
        }
        Value::Object(map) => {
            map.insert(seg_key(key), value);
            Ok(())
        }
        _ => Err(PathError::new(&[]).into()),
    }
}

/// 读取 `parent[key]`（自有属性；缺失即 `undefined`）。
///
/// - 对象：查 `seg_key(key)`，缺失返回 `Ok(None)`；
/// - 数组：要求 `Seg::Index` 且非负（`Seg::Key` 或负数 → [`UnsafePathError`]）；
///   下标越界返回 `Ok(None)`，**不是错误**——调用方把 `None` 当作 `undefined`，
///   `a`/`t` 再统一报 `PathError`；
/// - 其它父容器：`Ok(None)`（不可达，父容器已由 `resolve_container_mut` 保证）。
///
/// 只查自有属性，不涉及原型链（Rust 的 `Value` 没有原型）。
fn read_value<'a>(parent: &'a Value, key: &Seg) -> Result<Option<&'a Value>, OpError> {
    match parent {
        Value::Array(items) => {
            let Seg::Index(index) = key else {
                return Err(UnsafePathError::from_seg(key).into());
            };
            if *index < 0 {
                return Err(UnsafePathError::from_seg(key).into());
            }
            Ok(items.get(*index as usize))
        }
        Value::Object(map) => Ok(map.get(&seg_key(key))),
        _ => Ok(None),
    }
}

/// 删除 `parent[key]`。
///
/// - 对象：`shift_remove` 保留其余键的插入顺序（依赖 serde_json 的 `preserve_order`，
///   工作区 `Cargo.toml` 已开启；`Map::remove` 是 swap-remove，会改变顺序）。删除不存在
///   的键不是错误，与 JS `delete` 返回 true 一致；
/// - 数组：要求 `Seg::Index` 且非负（`Seg::Key` 或负下标 → [`UnsafePathError`]）；
///   `index >= len` → `PathError`（携带**完整** path）；删除后后续元素前移，等价于
///   JS `splice(index, 1)`；
/// - 标量父容器：`PathError::new(&[])`（不可达）。
fn delete_value(parent: &mut Value, key: &Seg, path: &[Seg]) -> Result<(), OpError> {
    match parent {
        Value::Array(items) => {
            let Seg::Index(index) = key else {
                return Err(UnsafePathError::from_seg(key).into());
            };
            if *index < 0 {
                return Err(UnsafePathError::from_seg(key).into());
            }
            let index = *index as usize;
            if index >= items.len() {
                return Err(PathError::new(path).into());
            }
            items.remove(index);
            Ok(())
        }
        Value::Object(map) => {
            // `delete parent[key]` 保持其余键的插入序；`Map::remove` 在 preserve_order
            // 下是 swap-remove，会把末尾键搬进被删位置（TS 的 `delete` 不会）。
            map.shift_remove(&seg_key(key));
            Ok(())
        }
        _ => Err(PathError::new(&[]).into()),
    }
}

/// TS `applyImmutable(target, ops)`。
///
/// TS 用 copy-on-write 让未触碰的容器与入参共享身份；Rust 的 `Value` 没有指针身份，
/// 无法观察这种共享。所有权本身已保证"入参不被修改"，因此这里复用 [`apply`] 的
/// 值语义（记录的差异见模块文档）。
///
/// 与 [`apply`] 相同的校验与错误；`r` 的 payload 也会被 clone，因此原批次不会被后续
/// op 修改（`tests/delta_ops.rs` 的
/// `immutable_application_keeps_the_base_and_payload_untouched` 锁定）。
pub fn apply_immutable(root: Value, ops: &[Op]) -> Result<Value, OpError> {
    apply(root, ops)
}

/// TS `applyImmutableBatches(target, batches)`：把所有批次当作一次只暴露最终结果的
/// 重放。Rust 的所有权语义下等同于顺序 [`apply_immutable`]。
///
/// 每个批次独立校验；空迭代器原样返回 `root`。中途失败时已处理的批次结果随局部
/// `root` 丢弃，调用方只拿到错误。`batches` 以 `Vec<Op>` 的迭代器传入并逐个应用；
/// 任意批次里的 `r` 都会替换当前根。
pub fn apply_immutable_batches(
    root: Value,
    batches: impl IntoIterator<Item = Vec<Op>>,
) -> Result<Value, OpError> {
    let mut root = root;
    for batch in batches {
        root = apply_immutable(root, &batch)?;
    }
    Ok(root)
}

// ─── Codec ───────────────────────────────────────────────────────────────────

// 路径的 interning 键：`JSON.stringify(path)`。数字段与字符串段是不同的键
// （`[3]` 与 `["3"]`）；NUL 等字符由 JSON 转义，不会与相邻字符拼接混淆
// （`tests/delta_ops.rs` 的 `does_not_collide_paths_containing_null_characters`）。
// 这个键只在 Encoder 内部用于路径 interning，不上 wire；Decoder 的 id 表以
// `number_key`（数字 id 的 f64 位模式）为键，不使用 `path_key`。
fn path_key(path: &[Seg]) -> String {
    serialize_json(&path_to_json(path))
}

/// TS `encoder()`：路径 interning（第二次使用才定义）与相邻路径省略。
///
/// # 状态
///
/// - `seen`：出现过一次的路径键（首次使用内联，不分配 id）；
/// - `ids`：已分配 id 的路径键 → id；
/// - `next_id`：下一个 id，按首次“第二次使用”的顺序递增；
/// - `previous`：**当前批次**上一条写出的路径键，用于省略相邻重复路径。
///
/// `seen`/`ids`/`next_id` 跨 `encode` 调用保留，`previous` 每次调用重置。编码到
/// `r` 时会清空全部状态：`r` 是恢复点，之后的批次必须对“只从 `r` 重放”的 reader
/// 自包含，不能让 reader 引用它没见过的 id。状态只应与配套的 [`Decoder`] 共享，
/// 不同状态流不能混用同一个实例。
#[derive(Debug, Default)]
pub struct Encoder {
    /// 首次出现过的路径键；第二次出现时才分配 id。
    seen: HashSet<String>,
    /// 已分配 id 的路径键 → id（`next_id` 单调递增）。
    ids: HashMap<String, u32>,
    /// 下一个可分配的 id；遇到 `r` 归零。
    next_id: u32,
    /// 当前批次上一条写出的路径键；跨批次不保留。
    previous: Option<String>,
}

impl Encoder {
    /// 新建编码器（`seen`/`ids`/`nextId` 跨批次保留）。
    pub fn new() -> Self {
        Encoder::default()
    }

    /// `encode(ops)`：把已解码 op 编成 wire JSON 数组。
    ///
    /// 不校验 op：手工构造的非法 op（如含保留段）会原样编码，由解码端拒绝。
    ///
    /// 每次调用重置 `previous`，因此短形式的省略作用域是单个批次；id 表跨调用保留，
    /// 后续批次可以引用前面批次定义的 id，解码端必须使用配套的 [`Decoder`] 并按序
    /// 消费。遇到 `r` 时清空 id 表并重置 `next_id`。
    ///
    /// 对每条非 `r` op 的编码选择：
    /// 1. 路径键与 `previous` 相同 → 输出短形式（省略路径）；
    /// 2. 已分配 id → 输出数字引用；
    /// 3. 第二次出现 → 先追加一条 `#` 定义，再输出数字引用；
    /// 4. 首次出现 → 内联路径。
    ///
    /// 一条 op 可能对应两个 wire 元素（`#` 定义 + 引用它的 op）。
    pub fn encode(&mut self, ops: &[Op]) -> Vec<WireOp> {
        // 省略路径的作用域是单个批次。
        self.previous = None;
        let mut out = Vec::with_capacity(ops.len());
        for op in ops {
            if let Op::Replace(value) = op {
                // `r` 编码为自身；它没有路径，因此不参与 interning/省略。
                out.push(Value::Array(vec![Value::String("r".into()), value.clone()]));
                // `r` 是恢复点：此后的批次必须自包含。
                self.seen.clear();
                self.ids.clear();
                self.next_id = 0;
                self.previous = None;
                continue;
            }
            let path = op_path(op);
            let key = path_key(path);
            if Some(&key) == self.previous.as_ref() {
                // 与上一条同路径：短形式靠 arity 表达，不再发引用。
                out.push(short_form(op));
                continue;
            }
            let reference = match self.ids.get(&key) {
                // 已 intern：直接发数字引用。
                Some(id) => Value::Number(Number::from(*id)),
                // 第二次出现：先发 `#` 定义（占一个 wire 元素），再用 id 引用。
                None if self.seen.contains(&key) => {
                    let id = self.next_id;
                    self.next_id += 1;
                    self.ids.insert(key.clone(), id);
                    out.push(Value::Array(vec![
                        Value::String("#".into()),
                        Value::Number(Number::from(id)),
                        path_to_json(path),
                    ]));
                    Value::Number(Number::from(id))
                }
                // 首次出现：内联路径，不分配 id。
                None => {
                    self.seen.insert(key.clone());
                    path_to_json(path)
                }
            };
            out.push(full_form(op, reference));
            // 只有完整形式更新 `previous`；短形式与上一条本就同路径。
            self.previous = Some(key);
        }
        out
    }
}

// 取 op 的路径。`Replace` 返回空切片，但编码循环在到达这里前已 `continue`，不会使用。
fn op_path(op: &Op) -> &[Seg] {
    match op {
        Op::Replace(_) => &[],
        Op::Set(path, _)
        | Op::Delete(path)
        | Op::Append(path, _)
        | Op::Truncate(path, _)
        | Op::Splice { path, .. }
        | Op::Permute(path, _) => path,
    }
}

/// 省略路径的短形式（与上一条 op 同路径）。
///
/// 只保留 payload；解码端靠 arity 判断是否省略了路径（`d` 1 元、`p` 4 元、其它 2 元）。
/// 不会自己发 `#` 定义。
fn short_form(op: &Op) -> Value {
    match op {
        Op::Replace(value) => Value::Array(vec![Value::String("r".into()), value.clone()]),
        Op::Set(_, value) => Value::Array(vec![Value::String("s".into()), value.clone()]),
        Op::Delete(_) => Value::Array(vec![Value::String("d".into())]),
        Op::Append(_, text) => {
            Value::Array(vec![Value::String("a".into()), Value::String(text.clone())])
        }
        Op::Truncate(_, count) => Value::Array(vec![
            Value::String("t".into()),
            Value::Number(Number::from(*count as u64)),
        ]),
        Op::Splice {
            index,
            remove,
            items,
            ..
        } => Value::Array(vec![
            Value::String("p".into()),
            Value::Number(Number::from(*index as u64)),
            Value::Number(Number::from(*remove as u64)),
            Value::Array(items.clone()),
        ]),
        Op::Permute(_, permutation) => Value::Array(vec![
            Value::String("m".into()),
            Value::Array(
                permutation
                    .iter()
                    .map(|index| Value::Number(Number::from(*index as u64)))
                    .collect(),
            ),
        ]),
    }
}

/// 带显式引用的完整形式。
///
/// `reference` 是内联路径数组或数字 id；`#` 定义由调用方（`Encoder::encode`）在需要时
/// 先行追加，本函数不产生它。
fn full_form(op: &Op, reference: Value) -> Value {
    match op {
        Op::Replace(value) => Value::Array(vec![Value::String("r".into()), value.clone()]),
        Op::Set(_, value) => {
            Value::Array(vec![Value::String("s".into()), reference, value.clone()])
        }
        Op::Delete(_) => Value::Array(vec![Value::String("d".into()), reference]),
        Op::Append(_, text) => Value::Array(vec![
            Value::String("a".into()),
            reference,
            Value::String(text.clone()),
        ]),
        Op::Truncate(_, count) => Value::Array(vec![
            Value::String("t".into()),
            reference,
            Value::Number(Number::from(*count as u64)),
        ]),
        Op::Splice {
            index,
            remove,
            items,
            ..
        } => Value::Array(vec![
            Value::String("p".into()),
            reference,
            Value::Number(Number::from(*index as u64)),
            Value::Number(Number::from(*remove as u64)),
            Value::Array(items.clone()),
        ]),
        Op::Permute(_, permutation) => Value::Array(vec![
            Value::String("m".into()),
            reference,
            Value::Array(
                permutation
                    .iter()
                    .map(|index| Value::Number(Number::from(*index as u64)))
                    .collect(),
            ),
        ]),
    }
}

/// TS `decoder()`：路径 id 表跨调用保留，`previous` 每批重置。
///
/// # 状态与配对约束
///
/// `paths` 保存 id → 路径（id 按 f64 位模式索引，见 `number_key`）。`#` 定义写入它，
/// 数字引用读取它，`r` 清空它。批次内的“上一条路径”只存在于 `decode` 的局部变量里，
/// 不会跨调用。
///
/// 一个 decoder 必须与产生该流的 [`Encoder`] 配对，并按相同顺序消费批次，否则可能
/// 解析到错误的 id，或得到 [`PathError`]。两个独立的状态流必须各用一对实例：id 表是
/// 实例状态，共用一个 decoder 会让它们的定义互相覆盖。
#[derive(Debug, Default)]
pub struct Decoder {
    /// 键是 JS 数字 id 的 f64 位模式（见 [`number_key`]）；`2e19` 与 `3e19` 不冲突。
    paths: HashMap<u64, Path>,
}

impl Decoder {
    /// 新建解码器。
    pub fn new() -> Self {
        Decoder::default()
    }

    /// `decode(wire)`：校验每条 wire op 并还原为 [`Op`]。
    ///
    /// 每条 op 先经 [`assert_valid_wire_op`]（语法），再按 id 表/短形式解析路径：
    /// `#` 只更新 id 表、不产生 [`Op`]；`r` 产生 `Replace` 并清空 id 表；短形式复用
    /// 同一批次上一条路径。
    ///
    /// # 错误
    ///
    /// - 语法错误：由 `assert_valid_wire_op` 返回（`TypeError` 系或 `UnsafePathError`）；
    /// - 短形式但批次里还没有上一条路径 → `PathError([])`（消息 `unresolvable path: []`）；
    /// - 数字 id 未定义 → `PathError`，`path` 保存原始数字（消息如 `unresolvable path: 3`）；
    /// - `s`/`d`/`a`/`t` 解析出空路径 → `PathError([])`。
    ///
    /// # 失败后的状态
    ///
    /// 第一条错误即返回：失败前已经处理、且未被后续 `r` 清空的 `#` 定义仍留在
    /// `self.paths` 里，之后的调用可能解析到它们；批次内的 `previous` 是局部变量，
    /// 随调用丢弃。需要干净状态时换用新 [`Decoder`]，或从 `r` 恢复点重新同步。
    pub fn decode(&mut self, wire: &[WireOp]) -> Result<Vec<Op>, OpError> {
        // 批次内“上一条路径”，用于短形式；每次调用重置。
        let mut previous: Option<Path> = None;
        let mut out = Vec::with_capacity(wire.len());
        for op in wire {
            // 先做完整语法校验；下面的 expect/unwrap 都依赖它已通过。
            assert_valid_wire_op(op)?;
            let items = op.as_array().expect("validated tuple");
            let verb = items[0].as_str().expect("validated verb");
            if verb == "#" {
                // 定义 id：只写表，不产出 op。路径已由 assert_safe_json_path 校验。
                let path = assert_safe_json_path(&items[2])?;
                let id = number_key(&items[1]).expect("validated id");
                self.paths.insert(id, path);
                continue;
            }
            if verb == "r" {
                // 恢复点：替换根并清空 id 表与批次路径。
                out.push(Op::Replace(items[1].clone()));
                self.paths.clear();
                previous = None;
                continue;
            }
            // 短形式由 arity 唯一确定：d 1 元、p 4 元、其余 verb 2 元（r/# 已处理）。
            let short = (verb == "d" && items.len() == 1)
                || (verb != "d" && verb != "p" && items.len() == 2)
                || (verb == "p" && items.len() == 4);
            let path: Path;
            if short {
                let Some(current) = previous.as_ref() else {
                    return Err(PathError::new(&[]).into());
                };
                path = current.clone();
            } else {
                let reference = &items[1];
                if reference.is_number() {
                    let id = number_key(reference).expect("validated id");
                    let Some(resolved) = self.paths.get(&id) else {
                        // TS: `new PathError(id)`，消息是 `JSON.stringify(id)`。
                        return Err(PathError {
                            path: reference.clone(),
                        }
                        .into());
                    };
                    path = resolved.clone();
                } else {
                    // 内联路径：保留段与段类型已在 assert_valid_wire_op 里查过。
                    path = parse_path_value(reference)?;
                }
                // 只有完整形式会更新 previous；短形式与上一条同路径。
                previous = Some(path.clone());
            }
            // s/d/a/t 不能指向根；p/m 可以（目标是根数组）。
            if verb != "p" && verb != "m" && path.is_empty() {
                return Err(PathError::new(&path).into());
            }
            // 短形式的 payload 从下标 1 开始，完整形式从 2 开始（第 1 项是路径引用）。
            let payload = |index: usize| -> &Value {
                if short {
                    &items[index]
                } else {
                    &items[index + 1]
                }
            };
            match verb {
                "s" => out.push(Op::Set(path, payload(1).clone())),
                "d" => out.push(Op::Delete(path)),
                "a" => out.push(Op::Append(
                    path,
                    payload(1).as_str().expect("validated text").to_owned(),
                )),
                "t" => {
                    let count = non_negative_integer(payload(1)).expect("validated count");
                    out.push(Op::Truncate(
                        path,
                        usize::try_from(count).unwrap_or(usize::MAX),
                    ));
                }
                "p" => {
                    let index = non_negative_integer(payload(1)).expect("validated index");
                    let remove = non_negative_integer(payload(2)).expect("validated remove");
                    let items_value = payload(3);
                    let items = items_value.as_array().expect("validated items").clone();
                    out.push(Op::Splice {
                        path,
                        index: usize::try_from(index).unwrap_or(usize::MAX),
                        remove: usize::try_from(remove).unwrap_or(usize::MAX),
                        items,
                    });
                }
                "m" => {
                    // 排列元素已在 wire 校验里确认非负整数且构成双射。
                    let permutation: Vec<usize> = payload(1)
                        .as_array()
                        .expect("validated permutation")
                        .iter()
                        .map(|value| {
                            usize::try_from(non_negative_integer(value).expect("validated entry"))
                                .unwrap_or(usize::MAX)
                        })
                        .collect();
                    out.push(Op::Permute(path, permutation));
                }
                _ => unreachable!("validated verb"),
            }
        }
        Ok(out)
    }
}

/// 便捷函数：一次性编码（新建 [`Encoder`]）。
///
/// 每次都是全新状态，因此产出的批次自包含（数字 id 只在批次内定义），适合单批往返
/// `decode(&encode(&ops))`。需要跨批 interning 时请长期持有 [`Encoder`]，并保证解码端
/// 使用配套的 [`Decoder`]。
pub fn encode(ops: &[Op]) -> Vec<WireOp> {
    Encoder::new().encode(ops)
}

/// 便捷函数：一次性解码（新建 [`Decoder`]）。
///
/// 只能解码**自包含**的批次：id 在批次内定义（`#`）或路径内联。长期持有的 encoder
/// 产出的第二个及以后的批次可能引用更早批次定义的 id，此时必须使用配套的持久
/// [`Decoder`]，本函数会报 `PathError`。
pub fn decode(wire: &[WireOp]) -> Result<Vec<Op>, OpError> {
    Decoder::new().decode(wire)
}
