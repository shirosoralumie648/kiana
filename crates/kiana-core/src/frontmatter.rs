//! utils/frontmatter.ts + 一个最小 YAML 1.2 子集解析器。
//!
//! TS: `packages/coding-agent/src/utils/frontmatter.ts`（40 行，使用 npm `yaml` 2.9.0）。
//!
//! ```ts
//! const normalizeNewlines = (value) => value.replace(/\r\n/g, "\n").replace(/\r/g, "\n");
//! const extractFrontmatter = (content) => { ... };
//! export const parseFrontmatter = (content) => { ... };
//! export const stripFrontmatter = (content) => parseFrontmatter(content).body;
//! ```
//!
//! ## 提取规则（逐字）
//!
//! 1. 先 `stripBom`（[`crate::text::strip_bom`]），再把 `\r\n`、`\r` 归一为 `\n`；
//! 2. 不以 `---` 开头 → `{ frontmatter: {}, body: normalized }`；
//! 3. `endIndex = normalized.indexOf("\n---", 3)`，找不到 → 同上；
//! 4. `yamlString = normalized.slice(4, endIndex)`（**跳过开头的 `---\n`**）；
//!    `body = normalized.slice(endIndex + 4).trim()`（跳过 `\n---`，再 trim 两端空白）；
//! 5. `yamlString` falsy（`null` 或空串）→ `{}`；否则解析 YAML；
//!    `parse` 返回 `null`（空文档）→ `{}`。
//!
//! ## 与 TS 的接口差异（必要偏差）
//!
//! TS 的 `parseFrontmatter` 在 YAML 语法错误时 **throw**，调用方（`skills.ts` /
//! `prompt-templates.ts`）把它转成诊断消息。Rust 无法抛出，因此本模块返回
//! `Result<_, FrontmatterError>`；`FrontmatterError` 的 `Display` 逐字等于 TS
//! `error.message`（含 ` at line L, column C:` 与插入符上下文），
//! `strip_frontmatter` 同样返回 `Result`（TS 里它也会 throw）。
//!
//! ## 内置 YAML 子集
//!
//! workspace 没有 pin 任何 YAML crate（`spec/ca-cli.md` §8.2 第 13 条、`ARCHITECTURE.md`
//! §7.0），因此这里实现 `yaml` 包在本项目里实际用到的 YAML 1.2 子集：
//!
//! - block mapping / block sequence（含 `- key: value` 紧凑映射、同缩进序列）；
//! - flow sequence / flow map（`[a, b]`、`{a: 1}`、`[a: b]` 单对映射、尾随逗号）；
//! - 单/双引号标量（含跨行折叠、`''`、`\n`/`\xXX`/`\uXXXX`/`\UXXXXXXXX` 等转义）；
//! - `|` / `>` 块标量（chomping `+`/`-`、显式缩进指示符、tab 内容、根节点内容缩进 0、
//!   尾部空行与 EOF 换行的精确语义，逐行复刻 `resolveBlockScalar`）；
//! - 注释、空文档 → `{}`、多行 plain scalar 折叠与 `---`/`...` 文档标记；
//! - `&anchor` / `*alias`（未定义别名在组合成功后才报 yaml 的 “Unresolved alias” 错误）、
//!   `!tag` / `!!str` / `!!int` / `!!float` / `!!bool` / `!!null` / `!!binary` /
//!   `!!timestamp` 显式 tag（未知 tag 退化为字符串；`<<:` 在 core schema 里是普通键）；
//! - YAML 1.2 core schema 的标量解析（`yes`/`on` 保持字符串、`0o`/`0x`、`.inf`/`.nan`、
//!   布尔三写法、`null`/`~`）；
//! - 重复键按 YAML 值身份判重（`null` 与 `""`、`1` 与 `"1"` 不同；`1` 与 `1.0` 相同；
//!   `NaN` 永不相等），键按 JS 规则字符串化（`null`/`~` → `""`、`.inf` → `"Infinity"`）。
//!
//! 错误消息逐字复刻 `yaml` 的 `prettifyError`：`at line L, column C`（列按 UTF-16 code
//! unit，与 JS `string.length` 一致）、必要时附带上一行、插入符按 yaml 的 token 范围下划线，
//! 以及 `yaml` 特有的错误文案（`Missing , or : between flow … items`、
//! `Implicit keys need to be on a single line`、`Source contains multiple documents…` 等）。
//!
//! ## 已知偏差
//!
//! - 非 mapping 的 YAML 文档（标量/序列）在 TS 里会成为 `frontmatter` 本体；Rust 的
//!   `ParsedFrontmatter.frontmatter` 是 `Map`，因此退化为空 map（下游只按字段读，等价）。
//!   `---\n- foo: 1\n---`、`---\n!: v\n---`（yaml 把 `!:` 整体当 tag，得到根标量）等
//!   文档因此在 Rust 侧得到空 map。
//! - `.inf`/`.nan` 与溢出浮点（`1e400`）的**值**在 JS 是 `Infinity`/`NaN`；`serde_json`
//!   无法表示，落为 `null`；作为 mapping **键**时仍按 JS 规则字符串化为
//!   `"Infinity"`/`"-Infinity"`/`"NaN"`。
//! - block 的显式 `? key`（`? a\n: 1`、`? [1, 2]\n: v`、`? |\n  text\n: v`）、flow 的
//!   `? key`（`{? k}`、`[? a]` → `[{"a": null}]`、`[?]` → `[{"": null}]`）都已支持；
//!   `!!pairs`、flow 集合 key（`[1]: v` → `"[ 1 ]"`）、key 上的 anchor
//!   （`&x a: 1` 后 `*x` → `"a"`）也已复刻。
//! - 大整数（`2^63`、`2^63-1`）在 JS 里是同一个 Number（double）：作为 mapping 键时
//!   文本由 [`kiana_json::js_num::format_js_number`] 生成（`9223372036854776000`）；但值本身
//!   是 `f64`，直接 `serde_json::to_string` 会得到指数形式
//!   （`9.223372036854776e+18`）。需要 TS 文本时用 [`kiana_json::js_object::stringify`]。
//! - 自引用别名（`a: &x [*x]`、`a: &x\n  b: *x`）在 TS 里解析成**循环对象**；
//!   `serde_json::Value` 是树，无法表示循环，这里报
//!   `Unresolved alias (the anchor must be set before the alias): x`（不会 panic）。
//! - 孤立代理项（`\ud800`，或 `---😀\n---` 这类按 UTF-16 切出来的单独低代理项）无法用
//!   Rust 字符串表示：替换为 `U+FFFD` 或退化为空 map。合法的代理对转义
//!   （`😀`）会正确合并为星平面字符。
//! - 病态/边角输入的值或文案可能不同（双方通常都报错）：flow 里的 `- ` 块序列
//!   （`a: [- 1\n  ]` 在 yaml 里是 `Implicit keys of flow sequence pairs…`）、
//!   空 alias 键（`*: v` 在 yaml 里是 `Implicit map keys need to be followed by map
//!   values`）、flow 中 `1 'v': 2` 的 `BLOCK_IN_FLOW` 插入符跨度、block scalar 去缩进后
//!   紧跟的跨行隐式键、`%YAML` 指令、注释/空行夹在值与错误位置之间时的
//!   `at line/column`、嵌套块集合的 `All mapping items must start at the same column`
//!   列号。
//! - 错误发生在跨行 quoted scalar **之前**时，yaml 的错误恢复会跳过该 scalar token，
//!   而它的 lineCounter 不记录被跳过 token 内部的换行，于是错误片段会把该 scalar 的
//!   续行一起打印出来；Rust 按物理行显示（输入本身非法，只有片段文本不同）。
//! - JS 对象的整数键排序（数组下标键按数值升序排在最前）已实现；`null` 键 → `""`
//!   （`yaml` 包的怪异行为）也已复刻。
//! - 合法文档的值与错误消息已用 281 条 `yaml` 2.9.0 黄金语料锁定（见
//!   `tests/fixtures/yaml_corpus.json`）。
//!
//! ## 在系统中的位置
//!
//! 调用方通过 [`parse_frontmatter`] / [`strip_frontmatter`] 进入本模块
//! （返回类型是 [`ParsedFrontmatter`] / [`FrontmatterError`]）：
//!
//! - `kiana-resources` 的技能发现（`skills/discover.rs`）与 prompt 模板加载
//!   （`prompts/mod.rs`）把 `Err` 转成文件级诊断；技能发现只对 `SKILL.md`
//!   （`basename == "SKILL.md"`）上报 YAML 错误，其它散装 `.md` 静默跳过；
//! - `kiana-session` 的技能展开（`agent_session.rs`）用 `strip_frontmatter` 取正文，
//!   `Err` 时上报 `skill_expansion` 错误并原样返回未展开的命令文本；
//! - `kiana-extensions` 的 `virtual_modules.rs` 把本模块暴露成 JS 函数
//!   `parseFrontmatter`，`Err` 时以 `error.to_string()` 抛 JS 异常；
//! - `kiana-coding-agent` 的 `sdk_exports.rs` 以 `parseFrontmatter` /
//!   `stripFrontmatter` 的名字重导出这两个函数。
//!
//! ## 实现结构与流程
//!
//! 1. `extract_frontmatter`：剥 BOM、归一换行，再在 UTF-16 空间按 TS 的
//!    `indexOf("\n---", 3)` / `slice` 语义切出 YAML 文本与 body（见上方提取规则）；
//! 2. `parse_yaml`：创建一次性的 `Parser` 解析单个文档，成功后再检查被推迟的
//!    未定义别名错误（yaml 在 `toJS()` 阶段才解析别名）；
//! 3. `Parser`：手写递归下降。`chars: Vec<char>` 支持随机访问与 `state`/`restore`
//!    回滚（块映射的隐式 key 需要先试读再回退），`lines` 保留原始行文本供错误片段
//!    使用；`anchors`、`last_*`、`pending_key_start` 等字段记录跨 token 的上下文
//!    （逐字段含义见结构体注释）；
//! 4. 标量按 YAML 1.2 core schema 解析（`resolve_plain_scalar`）；映射键额外保留
//!    YAML 值身份（`KeyId`）用于重复键判定；最后 `js_order_object` 复刻 JS 对象的
//!    整数键排序。
//!
//! 除 `LazyLock<Regex>` 的正则缓存外没有全局状态，每次 `parse_frontmatter` 都新建
//! `Parser`。`Parser` 的 `Clone` 只用于 tab 缩进的无副作用探测
//! （`line_starts_block_collection`），克隆体上的改动全部丢弃。
//!
//! ## 使用注意
//!
//! - 只接受**单文档** YAML：出现第二个 `---`（即使其后没有内容），或 `...` 之后
//!   还有内容时，报 `Source contains multiple documents...`（与 `yaml.parse` 一致）；
//! - `body` 只在找到闭合的 `\n---` 时按 JS `trim` 去两端空白；没有 frontmatter
//!   或没有结束标记时，`body` 是剥 BOM、归一换行后的原文（保留首尾空白）；
//! - `line()` / `column()` 是 1-based，列按 UTF-16 code unit 计；未定义别名错误
//!   没有位置信息，二者均为 0；
//! - 带位置的错误消息已含 `at line L, column C` 与插入符片段；现有调用方均直接
//!   `to_string()` 使用，拼进更高层消息时不要再重复加位置；
//! - 错误文案与解析结果是对外契约：`tests/frontmatter.rs` 逐条断言具体的值与消息，
//!   `tests/fixtures/yaml_corpus.json` 的黄金用例由 `golden_tests` 比对；改解析
//!   行为后必须重新核对这两处（语料由 `yaml` 2.9.0 生成，代表 TS 侧行为）。

use std::sync::LazyLock;

use regex::Regex;
use serde_json::{Map, Number, Value};

use crate::text::strip_bom;

/// TS: `parseFrontmatter` 的返回类型（`{ frontmatter: T; body: string }`）。
#[derive(Debug, Clone, PartialEq)]
pub struct ParsedFrontmatter {
    /// YAML 文档的根映射。没有 frontmatter、YAML 为空文档，或根节点不是 mapping
    /// （标量/序列）时为空 map（后者的 TS 差异见模块文档“已知偏差”）。
    pub frontmatter: Map<String, Value>,
    /// 去掉 frontmatter 后的正文，换行已归一为 `\n`。
    ///
    /// 找到闭合的 `\n---` 时按 JS `trim` 去两端空白；没有 frontmatter 或没有结束
    /// 标记时是剥 BOM、归一换行后的原文，**不**去空白（见模块文档“使用注意”）。
    pub body: String,
}

/// 内部错误类别（用于解析器在调用方转换为 yaml 的不同错误文案）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ErrorKind {
    Positioned,
    NestedMapping,
}

/// YAML 语法错误。
///
/// `Display` 逐字等于 TS `error.message`（`yaml` 包的完整消息，含 `at line L, column C:`
/// 与插入符上下文）。例外是未定义别名：它来自 `toJS()` 阶段，消息里没有位置与
/// 插入符，`line()`/`column()` 返回 0。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FrontmatterError {
    message: String,
    line: usize,
    column: usize,
    kind: ErrorKind,
}

impl FrontmatterError {
    /// 消息文本（当前稳定契约，改动需同步测试）。
    pub fn message(&self) -> &str {
        &self.message
    }

    /// 1-based 行号。
    pub fn line(&self) -> usize {
        self.line
    }

    /// 1-based 列号（按 UTF-16 code unit 计）。
    pub fn column(&self) -> usize {
        self.column
    }

    /// yaml 在 `toJS()` 阶段抛出的别名错误：`ReferenceError` 没有位置信息，
    /// 因此 `line()`/`column()` 为 0。
    fn unresolved_alias(name: &str) -> Self {
        Self {
            message: format!("Unresolved alias (the anchor must be set before the alias): {name}"),
            line: 0,
            column: 0,
            kind: ErrorKind::Positioned,
        }
    }
}

impl std::fmt::Display for FrontmatterError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for FrontmatterError {}

/// TS: `normalizeNewlines`。
///
/// 先替换 `\r\n` 再替换裸 `\r`，顺序不能反（否则 CRLF 会变成两个换行）。
fn normalize_newlines(value: &str) -> String {
    if !value.contains('\r') {
        return value.to_string();
    }
    value.replace("\r\n", "\n").replace('\r', "\n")
}

/// JS `String.prototype.trim`（含 `﻿`）。
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

/// 按 JS `String.prototype.trim` 的空白集合裁剪两端（集合见 `is_js_whitespace`）。
fn js_trim(value: &str) -> &str {
    value.trim_matches(is_js_whitespace)
}

/// TS 的私有 `extractFrontmatter`。
///
/// 两项结果：`yaml_string` 是待解析的 YAML 文本（`None` 表示没有 frontmatter 块），
/// `body` 是去掉头部后的正文。
struct ExtractedFrontmatter {
    /// `None`：输入不以 `---` 开头，或找不到结束的 `\n---`。
    yaml_string: Option<String>,
    /// 换行已归一；只有找到结束标记时才按 JS `trim` 去两端空白。
    body: String,
}

/// 定位 `---` 头部并把内容拆成 YAML 文本与 body（TS 的私有 `extractFrontmatter`）。
///
/// 所有查找/切片都在 UTF-16 code unit 空间进行，复刻 JS 的 `indexOf` / `slice`（见
/// 函数内注释）；`slice(4, endIndex)` 若落在代理对中间，Rust 侧用 `from_utf16_lossy`
/// 得到 U+FFFD 而不是 panic。`yaml_string` 为 `None` 时 `body` 是不 trim 的归一化
/// 原文（测试 `returns_body_when_no_frontmatter_present` 锁定此行为）。
fn extract_frontmatter(content: &str) -> ExtractedFrontmatter {
    let normalized = normalize_newlines(&strip_bom(content));

    if !normalized.starts_with("---") {
        return ExtractedFrontmatter {
            yaml_string: None,
            body: normalized,
        };
    }

    // TS 的 `indexOf` / `slice` 都按 UTF-16 code unit 计索引；按字节切片会在
    // `---` 后紧跟非 ASCII 字符时切进多字节序列（JS 不会 panic）。这里逐字复刻
    // JS 语义：在 UTF-16 空间查找 `"\n---"`，再按 UTF-16 切片。
    let units: Vec<u16> = normalized.encode_utf16().collect();
    let mut end_index = None;
    let mut index = 3usize;
    while index + 4 <= units.len() {
        if units[index] == u16::from(b'\n')
            && units[index + 1] == u16::from(b'-')
            && units[index + 2] == u16::from(b'-')
            && units[index + 3] == u16::from(b'-')
        {
            end_index = Some(index);
            break;
        }
        index += 1;
    }
    let Some(end_index) = end_index else {
        return ExtractedFrontmatter {
            yaml_string: None,
            body: normalized,
        };
    };

    // `slice(4, endIndex)` 可能从代理对中间开始（TS 得到孤立低代理项）；Rust 的
    // String 无法表示孤立代理项，`from_utf16_lossy` 将其替换为 U+FFFD。
    let yaml_string = if 4 <= end_index {
        String::from_utf16_lossy(&units[4..end_index])
    } else {
        String::new()
    };
    let body = js_trim(&String::from_utf16_lossy(&units[end_index + 4..])).to_string();

    ExtractedFrontmatter {
        yaml_string: Some(yaml_string),
        body,
    }
}

/// TS: `parseFrontmatter(content)`。
///
/// YAML 语法错误返回 [`FrontmatterError`]（TS 里是 throw）。`yamlString` 为空/空文档时
/// `frontmatter` 是空 map。
///
/// 流程：`extract_frontmatter` 切出 YAML 文本与 body → YAML 文本缺失/为空时不解析
/// YAML，直接返回空 map 与已切好的正文 → `parse_yaml` 解析 → 根节点不是 mapping
/// 时退化为空 map。
///
/// # Errors
///
/// 语法错误（重复键、未闭合 flow 集合、单文档模式下出现第二个文档、未定义别名、
/// 非法转义等）返回 [`FrontmatterError`]。它的 `Display` 已是完整的 TS 错误消息
/// （带位置的错误含 `at line/column` 与插入符片段，未定义别名只有消息本身），
/// 调用方直接转成诊断或 JS 异常即可。
pub fn parse_frontmatter(content: &str) -> Result<ParsedFrontmatter, FrontmatterError> {
    let extracted = extract_frontmatter(content);
    let Some(yaml_string) = extracted.yaml_string else {
        return Ok(ParsedFrontmatter {
            frontmatter: Map::new(),
            body: extracted.body,
        });
    };
    if yaml_string.is_empty() {
        return Ok(ParsedFrontmatter {
            frontmatter: Map::new(),
            body: extracted.body,
        });
    }

    let parsed = parse_yaml(&yaml_string)?;
    let frontmatter = match parsed {
        Value::Object(map) => map,
        // TS 会把标量/序列原样作为 frontmatter；下游只按字段读取，等价于空 map（见模块文档）。
        _ => Map::new(),
    };
    Ok(ParsedFrontmatter {
        frontmatter,
        body: extracted.body,
    })
}

/// TS: `stripFrontmatter(content)` —— `parseFrontmatter(content).body`（YAML 错误时返回 `Err`）。
///
/// 只需要正文的调用方（如技能展开）用它；它内部完整执行一次 [`parse_frontmatter`]，
/// 同时需要头部字段时直接读 `ParsedFrontmatter::body`，不要解析两遍。
///
/// # Errors
///
/// 与 [`parse_frontmatter`] 相同；YAML 错误时没有 body 可返回（TS 同样会 throw）。
pub fn strip_frontmatter(content: &str) -> Result<String, FrontmatterError> {
    Ok(parse_frontmatter(content)?.body)
}

/// 解析 YAML 子集（`yaml.parse` 的等价物；错误消息见模块文档）。
fn parse_yaml(source: &str) -> Result<Value, FrontmatterError> {
    let mut parser = Parser::new(source);
    let value = parser.parse_document()?;
    // yaml 的 `parse()` 先抛组合阶段的错误；只有组合成功后才在 `toJS()` 解析别名，
    // 因此未定义锚点的错误要等整个文档解析成功后再报（且无位置信息）。
    if let Some(name) = parser.unresolved_alias.take() {
        return Err(FrontmatterError::unresolved_alias(&name));
    }
    Ok(value)
}

// ===========================================================================
// 标量解析（YAML 1.2 core schema）与 JS 键字符串化
// ===========================================================================

static INT_DECIMAL_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[-+]?[0-9]+$").expect("valid decimal integer regex"));
static INT_OCTAL_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^0o[0-7]+$").expect("valid octal integer regex"));
static INT_HEX_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^0x[0-9a-fA-F]+$").expect("valid hex integer regex"));
static FLOAT_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^[-+]?(?:\.[0-9]+|[0-9]+(?:\.[0-9]*)?)(?:[eE][-+]?[0-9]+)?$")
        .expect("valid float regex")
});
/// `yaml` 包 `floatExp` tag 的 test（必须有指数）。
static FLOAT_EXP_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^[-+]?(?:\.[0-9]+|[0-9]+(?:\.[0-9]*)?)[eE][-+]?[0-9]+$")
        .expect("valid float exp regex")
});
/// `yaml` 包 `float` tag 的 test（必须有小数点；不接受裸整数）。
static FLOAT_DOT_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^[-+]?(?:\.[0-9]+|[0-9]+\.[0-9]*)$").expect("valid float dot regex")
});
static INFINITY_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[-+]?\.(?:inf|Inf|INF)$").expect("valid infinity regex"));
static NAN_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\.(?:nan|NaN|NAN)$").expect("valid nan regex"));

/// `f64` → `serde_json::Value`（整数值写成 `i64`，与 JS `JSON.stringify` 的 `1` 一致）。
fn number_value(value: f64) -> Value {
    if !value.is_finite() {
        // JS 是 Infinity/NaN；JSON 无法表示（JSON.stringify → null），见模块文档。
        return Value::Null;
    }
    // `value as i64` 在越界时饱和：2^63 会落成 `i64::MAX`，而 `i64::MAX as f64` 又恰好
    // 等于 2^63，旧判据 `as_i64 as f64 == value` 会错误地接受它（存成 9223372036854775807）。
    // JS 的 Number 是 double：2^63 只能存成 f64。这里把 i64 快速路径限制在
    // `[-2^63, 2^63)`；-2^63 本身可精确表示，仍写 i64（数值相等）。
    const TWO_POW_63: f64 = 9_223_372_036_854_775_808.0;
    if value.fract() == 0.0 && (-TWO_POW_63..TWO_POW_63).contains(&value) {
        let as_i64 = value as i64;
        if as_i64 as f64 == value {
            return Value::Number(Number::from(as_i64));
        }
    }
    match Number::from_f64(value) {
        Some(number) => Value::Number(number),
        None => Value::Null,
    }
}

fn parse_radix_digits(digits: &str, radix: u32) -> f64 {
    let mut value = 0.0_f64;
    for c in digits.chars() {
        let digit = c.to_digit(radix).unwrap_or(0);
        value = value * f64::from(radix) + f64::from(digit);
    }
    value
}

/// TS `yaml` 包（YAML 1.2 core schema）的 plain scalar 解析。
///
/// 判定顺序：空串/`~`/`null`（含大小写变体）→ null；`true`/`false` 三写法 → bool；
/// `.inf`/`.nan` → 值侧 null（JS 的 Infinity/NaN 无法用 `serde_json` 表示）；
/// 十进制/八进制（`0o`）/十六进制（`0x`）整数与浮点 → 数字；其余保持字符串。
/// 数字统一经 `number_value` 转换（整数值优先写成 i64，非有限数落为 null）。
fn resolve_plain_scalar(text: &str) -> Value {
    if text.is_empty() || matches!(text, "~" | "null" | "Null" | "NULL") {
        return Value::Null;
    }
    match text {
        "true" | "True" | "TRUE" => return Value::Bool(true),
        "false" | "False" | "FALSE" => return Value::Bool(false),
        _ => {}
    }
    if INFINITY_RE.is_match(text) || NAN_RE.is_match(text) {
        return Value::Null;
    }
    if INT_DECIMAL_RE.is_match(text) {
        return match text.parse::<f64>() {
            Ok(value) => number_value(value),
            Err(_) => Value::String(text.to_string()),
        };
    }
    if INT_OCTAL_RE.is_match(text) {
        return number_value(parse_radix_digits(&text[2..], 8));
    }
    if INT_HEX_RE.is_match(text) {
        return number_value(parse_radix_digits(&text[2..], 16));
    }
    if FLOAT_RE.is_match(text) {
        return match text.parse::<f64>() {
            Ok(value) => number_value(value),
            Err(_) => Value::String(text.to_string()),
        };
    }
    Value::String(text.to_string())
}

/// YAML 映射键的身份：对应 yaml 包 `a.value === b.value` 的标量相等语义。
#[derive(Clone, Debug)]
enum KeyId {
    Null,
    Bool(bool),
    Number(f64),
    Str(String),
    /// 集合键：yaml 包按节点身份比较，两个不同节点永不相等。
    Collection,
}

impl PartialEq for KeyId {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (KeyId::Null, KeyId::Null) => true,
            (KeyId::Bool(left), KeyId::Bool(right)) => left == right,
            // 与 JS `===` 一致：`NaN !== NaN`，`0 === -0`。
            (KeyId::Number(left), KeyId::Number(right)) => left == right,
            (KeyId::Str(left), KeyId::Str(right)) => left == right,
            _ => false,
        }
    }
}

/// 解析后的映射键：JS 对象键文案 + YAML 值身份（查重只看身份，不看文案）。
#[derive(Clone, Debug)]
struct KeyInfo {
    text: String,
    id: KeyId,
    /// 键 token 是否跨行（quoted 键可能跨行 → MULTILINE_IMPLICIT_KEY）。
    multiline: bool,
    /// 键 token 起始位置（错误定位用）。
    position: Position,
}

/// 从已解析值构造键信息（JS 键文案 + YAML 值身份）。
///
/// 返回的 `multiline`/`position` 是占位值；块映射的 key 由调用方用 token 信息覆盖，
/// flow 的 plain key 由 `resolve_plain_scalar_key` 另行提供 `Infinity`/`NaN` 文案。
fn key_info_from_value(value: &Value) -> KeyInfo {
    let id = match value {
        Value::Null => KeyId::Null,
        Value::Bool(value) => KeyId::Bool(*value),
        Value::Number(number) => KeyId::Number(number.as_f64().unwrap_or(f64::NAN)),
        Value::String(value) => KeyId::Str(value.clone()),
        Value::Array(_) | Value::Object(_) => KeyId::Collection,
    };
    KeyInfo {
        text: stringify_key(value),
        id,
        multiline: false,
        position: (0, 0),
    }
}

/// `resolve_plain_scalar` + 映射键信息。
///
/// `serde_json::Value` 无法表示非有限数，值侧落为 `null`；但作为键时 TS 会得到
/// `String(Infinity) === "Infinity"`（`NaN` 同理），身份仍是 JS number。
fn resolve_plain_scalar_key(text: &str) -> (Value, KeyInfo) {
    if INFINITY_RE.is_match(text) {
        let (text, value) = if text.starts_with('-') {
            ("-Infinity", f64::NEG_INFINITY)
        } else {
            ("Infinity", f64::INFINITY)
        };
        return (
            Value::Null,
            KeyInfo {
                text: text.to_string(),
                id: KeyId::Number(value),
                multiline: false,
                position: (0, 0),
            },
        );
    }
    if NAN_RE.is_match(text) {
        return (
            Value::Null,
            KeyInfo {
                text: "NaN".to_string(),
                id: KeyId::Number(f64::NAN),
                multiline: false,
                position: (0, 0),
            },
        );
    }
    if (FLOAT_RE.is_match(text) || INT_DECIMAL_RE.is_match(text))
        && let Ok(parsed) = text.parse::<f64>()
        && !parsed.is_finite()
    {
        // yaml 的 parseInt/parseFloat 对巨数返回 ±Infinity，`String(Infinity)` 作为 JS 键。
        let (text, identity) = if parsed < 0.0 {
            ("-Infinity", f64::NEG_INFINITY)
        } else if parsed.is_nan() {
            ("NaN", f64::NAN)
        } else {
            ("Infinity", f64::INFINITY)
        };
        return (
            Value::Null,
            KeyInfo {
                text: text.to_string(),
                id: KeyId::Number(identity),
                multiline: false,
                position: (0, 0),
            },
        );
    }
    let value = resolve_plain_scalar(text);
    let info = key_info_from_value(&value);
    (value, info)
}

/// JS 对象键的字符串化（`null` → `""`，数字走 `Number::toString`）。
///
/// JS 里所有数字都是 double：`String(n)` 用最短往返表示（`9223372036854775807` →
/// `"9223372036854776000"`）。`serde_json` 的 i64 只是存储形态，键文案必须走
/// [`kiana_json::js_num::format_js_number`]。
fn stringify_key(value: &Value) -> String {
    // 顶层键的 JS 字符串化：`null` → `""`（`stringifyKey` 的 `jsKey === null` 分支）。
    if value.is_null() {
        return String::new();
    }
    stringify_key_in(value, false)
}

/// `in_map_value`：yaml 的节点字符串化对「隐式空值」的 Pair 值不写内容（`{ a: }`），
/// 而序列里的 `null` 写成 `null`。真正的源文本（`~`、`"quoted"` 等）无法从解析后的
/// `Value` 还原，这里做 best-effort（见模块文档）。
fn stringify_key_in(value: &Value, in_map_value: bool) -> String {
    match value {
        Value::Null => {
            if in_map_value {
                String::new()
            } else {
                "null".to_string()
            }
        }
        Value::Bool(value) => value.to_string(),
        Value::Number(number) => {
            if let Some(value) = number.as_f64() {
                kiana_json::js_num::format_js_number(value)
            } else {
                number.to_string()
            }
        }
        Value::String(value) => value.clone(),
        // 集合键在 yaml 包里用节点自身的流式表示字符串化（`[1]` → `"[ 1 ]"`）。
        Value::Array(items) => {
            if items.is_empty() {
                return "[]".to_string();
            }
            let inner: Vec<String> = items
                .iter()
                .map(|item| stringify_key_in(item, false))
                .collect();
            format!("[ {} ]", inner.join(", "))
        }
        Value::Object(map) => {
            if map.is_empty() {
                return "{}".to_string();
            }
            let inner: Vec<String> = map
                .iter()
                .map(|(key, value)| {
                    let value = stringify_key_in(value, true);
                    if value.is_empty() {
                        format!("{key}:")
                    } else {
                        format!("{key}: {value}")
                    }
                })
                .collect();
            format!("{{ {} }}", inner.join(", "))
        }
    }
}

/// JS 对象的键序：数组下标键（`0` .. `2^32-2`）按数值升序排在最前，其余保持插入序。
fn js_order_object(map: Map<String, Value>) -> Map<String, Value> {
    fn array_index(key: &str) -> Option<u32> {
        if key == "0" {
            return Some(0);
        }
        let bytes = key.as_bytes();
        if bytes.is_empty() || !bytes[0].is_ascii_digit() || bytes[0] == b'0' {
            return None;
        }
        if !bytes.iter().all(u8::is_ascii_digit) {
            return None;
        }
        let value: u64 = key.parse().ok()?;
        (value < 4_294_967_295).then_some(value as u32)
    }

    let mut indexed: Vec<(u32, String, Value)> = Vec::new();
    let mut others: Vec<(String, Value)> = Vec::new();
    for (key, value) in map {
        match array_index(&key) {
            Some(index) => indexed.push((index, key, value)),
            None => others.push((key, value)),
        }
    }
    if indexed.is_empty() {
        let mut result = Map::new();
        for (key, value) in others {
            result.insert(key, value);
        }
        return result;
    }
    indexed.sort_by_key(|(index, _, _)| *index);
    let mut result = Map::new();
    for (_, key, value) in indexed {
        result.insert(key, value);
    }
    for (key, value) in others {
        result.insert(key, value);
    }
    result
}

/// Node `Buffer.from(str, 'base64')` 的宽松解码：忽略非法字符，`=` 结束。
fn decode_base64_node(text: &str) -> Vec<u8> {
    let mut output = Vec::new();
    let mut bits: u32 = 0;
    let mut bit_count: u32 = 0;
    for byte in text.bytes() {
        let value = match byte {
            b'A'..=b'Z' => byte - b'A',
            b'a'..=b'z' => byte - b'a' + 26,
            b'0'..=b'9' => byte - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            b'=' => break,
            _ => continue,
        };
        bits = (bits << 6) | u32::from(value);
        bit_count += 6;
        if bit_count >= 8 {
            bit_count -= 8;
            output.push(((bits >> bit_count) & 0xff) as u8);
        }
    }
    output
}

/// Node `Buffer` 的 `JSON.stringify` 视图：`{"type":"Buffer","data":[...]}`。
fn buffer_value(data: &[u8]) -> Value {
    let mut map = Map::new();
    map.insert("type".to_string(), Value::String("Buffer".to_string()));
    map.insert(
        "data".to_string(),
        Value::Array(
            data.iter()
                .map(|byte| Value::Number(Number::from(*byte)))
                .collect(),
        ),
    );
    Value::Object(map)
}

static TIMESTAMP_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"^([0-9]{4})-([0-9]{1,2})-([0-9]{1,2})(?:(?:t|T|[ \t]+)([0-9]{1,2}):([0-9]{1,2}):([0-9]{1,2}(\.[0-9]+)?)(?:[ \t]*(Z|[-+][012]?[0-9](?::[0-9]{2})?))?)?$",
    )
    .expect("valid timestamp regex")
});

/// `yaml` 包 `!!timestamp` 的 resolve → JS `Date.toISOString()`。
fn parse_timestamp(text: &str) -> Option<String> {
    let captures = TIMESTAMP_RE.captures(text)?;
    let year: i64 = captures.get(1)?.as_str().parse().ok()?;
    // Date.UTC 把 0..=99 的年份解释为 1900..1999。
    let year = if (0..=99).contains(&year) {
        1900 + year
    } else {
        year
    };
    let month: i64 = captures.get(2)?.as_str().parse().ok()?;
    let day: i64 = captures.get(3)?.as_str().parse().ok()?;
    let hour: f64 = captures
        .get(4)
        .map(|m| m.as_str().parse::<f64>().unwrap_or(0.0))
        .unwrap_or(0.0);
    let minute: f64 = captures
        .get(5)
        .map(|m| m.as_str().parse::<f64>().unwrap_or(0.0))
        .unwrap_or(0.0);
    let second: f64 = captures
        .get(6)
        .map(|m| m.as_str().parse::<f64>().unwrap_or(0.0))
        .unwrap_or(0.0);
    // yaml 的 timestamp resolve 把整段秒（含小数）交给 `Date.UTC`，而 `Date.UTC` 对
    // 秒参数做 ToIntegerOrInfinity（向零截断）；小数只通过 `millisec`（捕获组 7）进入。
    // 旧实现把 `second * 1000` 和小数各算一次（`.999` 会进位到下一秒）。
    let second = second.trunc();
    let millisec: f64 = captures
        .get(7)
        .map(|m| {
            let mut fraction = m.as_str().to_string();
            fraction.push_str("00");
            fraction
                .chars()
                .skip(1)
                .take(3)
                .collect::<String>()
                .parse::<f64>()
                .unwrap_or(0.0)
        })
        .unwrap_or(0.0);

    let month_index = month - 1;
    let normalized_year = year + month_index.div_euclid(12);
    let normalized_month = month_index.rem_euclid(12) + 1;
    let first = chrono::NaiveDate::from_ymd_opt(
        i32::try_from(normalized_year).ok()?,
        u32::try_from(normalized_month).ok()?,
        1,
    )?;
    let epoch = chrono::NaiveDate::from_ymd_opt(1970, 1, 1)?;
    let days = first.signed_duration_since(epoch).num_days() as f64 + (day - 1) as f64;
    let mut total_ms =
        days * 86_400_000.0 + hour * 3_600_000.0 + minute * 60_000.0 + second * 1000.0 + millisec;

    if let Some(zone) = captures.get(8) {
        let zone = zone.as_str();
        if zone != "Z" {
            let (sign, body) = match zone.chars().next()? {
                '-' => (-1.0, &zone[1..]),
                '+' => (1.0, &zone[1..]),
                _ => (1.0, zone),
            };
            let offset: f64 = body
                .split(':')
                .map(|part| part.parse::<f64>().unwrap_or(0.0))
                .fold(0.0, |accumulator, part| accumulator * 60.0 + part);
            let mut offset = sign * offset;
            if offset.abs() < 30.0 {
                offset *= 60.0;
            }
            total_ms -= 60_000.0 * offset;
        }
    }

    // TimeClip：向零取整。
    let millis = total_ms.trunc() as i64;
    let timestamp = chrono::DateTime::<chrono::Utc>::from_timestamp_millis(millis)?;
    Some(timestamp.format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string())
}

// ===========================================================================
// YAML 1.2 子集解析器
// ===========================================================================

/// flow 集合的种类：序列 `[...]` 或映射 `{...}`。闭合错误文案与缩进检查按它区分。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FlowKind {
    Sequence,
    Map,
}

/// 1-based 的 `(行, 列)`；列按 UTF-16 code unit 计（与 JS 字符串索引一致）。
type Position = (usize, usize);

/// flow 集合所处的块上下文：`Root` 时内容行不受缩进限制（错误文案也不同）；
/// `Block { indent, level }` 时内容行必须比所在块集合缩进更深（`indent` 为块集合缩进），
/// 且只有最外层 flow（`level == 1`）允许闭合括号出现在 `indentNext - 1` 的缩进上
/// （yaml lexer 的 `atFlowEndMarker` 特例）。
#[derive(Clone, Copy, PartialEq, Eq)]
enum FlowContext {
    Root,
    Block { indent: usize, level: usize },
}

impl FlowContext {
    fn block(indent: usize) -> Self {
        // `level` 是“进入该集合内部后 lexer 的 flowLevel”：块上下文本身没有打开的
        // flow 集合，所以从 0 开始，`parse_flow_node` 打开集合时再 +1。
        FlowContext::Block { indent, level: 0 }
    }

    fn nested(self) -> Self {
        match self {
            FlowContext::Root => FlowContext::Root,
            FlowContext::Block { indent, level } => FlowContext::Block {
                indent,
                level: level + 1,
            },
        }
    }

    fn indent_limit(self) -> Option<usize> {
        match self {
            FlowContext::Root => None,
            FlowContext::Block { indent, .. } => Some(indent),
        }
    }

    /// 该缩进的行是否可能是内容行。
    fn allows_content(self, spaces: usize) -> bool {
        match self {
            FlowContext::Root => true,
            FlowContext::Block { indent, .. } => spaces > indent,
        }
    }

    /// 该缩进的行是否允许是流的闭合括号（yaml 只对最外层 flow 放宽一格）。
    fn allows_closer(self, spaces: usize) -> bool {
        match self {
            FlowContext::Root => true,
            FlowContext::Block { indent, level } => {
                spaces > indent || (level == 1 && spaces == indent)
            }
        }
    }
}

/// 节点属性 `&anchor` / `!tag`（可同时出现，顺序任意）。
#[derive(Clone, Default)]
struct NodeProps {
    anchor: Option<AnchorProp>,
    /// `(tag 源码, 起始位置, UTF-16 长度)`。
    tag: Option<(String, Position, usize)>,
}

/// `&name` 的锚点属性（保留位置/长度用于 `A node can have at most one anchor` 的定位）。
#[derive(Clone)]
struct AnchorProp {
    name: String,
    /// 锚点 token 起点（`&` 的位置）。
    #[allow(dead_code)]
    position: Position,
    /// 锚点 token 的 UTF-16 长度（含 `&`）。
    length: usize,
}

/// yaml 的 anchor 名字符集：除空白（空格/tab/换行/回车）、`,` 与 flow 指示符
/// `[]{}` 之外都算。
fn is_anchor_char(c: char) -> bool {
    !matches!(c, ' ' | ',' | '[' | ']' | '{' | '}' | '\n' | '\r' | '\t')
}

/// yaml 包 `tagChars` 的字符集合。
fn is_tag_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || "-#;/?:@&=+$_.!~*'()".contains(c)
}

/// 手写递归下降解析器。`Clone` 只用于 tab 缩进处的无副作用探测
/// （[`Parser::line_starts_block_collection`]）。
#[derive(Clone)]
struct Parser<'a> {
    /// 输入按 `char` 切分后的序列；`pos` 是它的下标（不是字节或 UTF-16 下标）。
    chars: Vec<char>,
    /// `Parser::new` 收到的输入按 `\n` 切分的结果，供错误构造使用：`make_error`
    /// 提取错误片段，`source_end_position` 计算文档结束位置（multi-doc 错误的 span 末端）。
    lines: Vec<&'a str>,
    /// `chars` 中的当前位置。
    pos: usize,
    /// 1-based 行号。
    line: usize,
    /// 1-based 列号，按 UTF-16 code unit 累加（见 `advance`）。
    column: usize,
    /// 已解析的锚点（yaml 的 anchor 语义：后定义覆盖先定义）。
    anchors: std::collections::HashMap<String, Value>,
    /// 最近一个叶子标量的原始文本（tag 显式指定时用它做解析）。
    last_scalar_text: Option<String>,
    /// 最近一次 flow plain scalar 的键信息（文本 + YAML 值身份，见 [`resolve_plain_scalar_key`]）。
    last_plain_key: Option<(Value, KeyInfo)>,
    /// 最近解析的 flow 节点是否是“标量类”token（plain/quoted/alias），而不是
    /// `[`/`{` 集合 —— flow 集合里少写 `,`/`:` 时的错误文案按此区分
    /// （resolveFlowCollection 的 “Missing , or : between … items”）。
    last_node_scalar_like: bool,
    /// 刚解析完的 block scalar 在 yaml token 模型里的结束行（见 `parse_block_mapping`）。
    last_block_scalar_end: Option<usize>,
    /// 最近一次 block plain scalar 是否跨行（yaml 的 MULTILINE_IMPLICIT_KEY 判定）。
    last_plain_scalar_multiline: bool,
    /// 最近一次 quoted scalar 是否跨行（yaml 的 MULTILINE_IMPLICIT_KEY 判定）。
    last_quoted_multiline: bool,
    /// 第一个未解析的别名。yaml 的 `parse()` 先抛组合阶段的错误，只有组合全部成功
    /// 才在 `toJS()` 里抛 “Unresolved alias”；这里同样把该错误推迟到解析结束。
    unresolved_alias: Option<String>,
    /// 上一对键值的值为空时，行尾换行在 yaml token 模型里算作**下一个键**的起始属性，
    /// 因此下一个键的 keyStart 落在上一行行尾（错误定位用）。只对**同一个块映射**里
    /// 同缩进的兄弟键生效（嵌套块节点的 key 有各自的 token 起点）。
    pending_key_start: Option<(Position, usize)>,
    /// 最近一次 `parse_properties` 消费的属性的起始位置（plain scalar 的
    /// “Nested mappings” 错误定位在属性起点，见 resolve-block-map 的 offset）。
    props_start: Option<Position>,
    /// 上一对键值中，值是否是在后续行上的块节点（影响下一个 key 的 MULTILINE 判定）。
    last_value_block: bool,
    /// 正在解析 `? ` 显式 key 的节点：plain scalar 在续行行首遇到 `: ` 时结束
    /// （该 `: ` 是显式 key 的 map-value-ind，而不是 “Nested mappings”）。
    explicit_key_plain: bool,
    /// 与节点同一行、位于节点之前的 `&anchor`/`!tag`。yaml 的 CST 会把它们移到
    /// **块映射第一个 key** 的 token 序列里（`&x a: 1` 的 anchor 属于 key `a`，
    /// 之后 `*x` 解析为 `"a"`；而独占一行的属性属于映射节点本身）。
    pending_node_props: Option<NodeProps>,
}

impl<'a> Parser<'a> {
    fn new(source: &'a str) -> Self {
        Self {
            chars: source.chars().collect(),
            lines: source.split('\n').collect(),
            pos: 0,
            line: 1,
            column: 1,
            anchors: std::collections::HashMap::new(),
            last_scalar_text: None,
            last_plain_key: None,
            last_node_scalar_like: true,
            last_block_scalar_end: None,
            last_plain_scalar_multiline: false,
            last_quoted_multiline: false,
            unresolved_alias: None,
            pending_key_start: None,
            props_start: None,
            last_value_block: false,
            explicit_key_plain: false,
            pending_node_props: None,
        }
    }

    fn peek(&self) -> Option<char> {
        self.chars.get(self.pos).copied()
    }

    fn peek_at(&self, offset: usize) -> Option<char> {
        self.chars.get(self.pos + offset).copied()
    }

    fn eof(&self) -> bool {
        self.pos >= self.chars.len()
    }

    fn advance(&mut self) -> Option<char> {
        let c = *self.chars.get(self.pos)?;
        self.pos += 1;
        if c == '\n' {
            self.line += 1;
            self.column = 1;
        } else {
            // `yaml` 包按 JS 字符串索引（UTF-16 code unit）计列号（`lineCounter`），
            // 因此非 BMP 字符占 2 列。
            self.column += c.len_utf16();
        }
        Some(c)
    }

    fn position(&self) -> Position {
        (self.line, self.column)
    }

    fn state(&self) -> (usize, usize, usize) {
        (self.pos, self.line, self.column)
    }

    fn restore(&mut self, state: (usize, usize, usize)) {
        self.pos = state.0;
        self.line = state.1;
        self.column = state.2;
    }

    fn error(&self, message: impl Into<String>, position: Position) -> FrontmatterError {
        make_error(message.into(), &self.lines, position.0, position.1, None)
    }

    /// “Nested mappings” 错误（序列项里的跨行 plain key 会被调用方转换成 MULTILINE）。
    fn nested_mapping_error(&self, position: Position) -> FrontmatterError {
        let mut error = self.error(
            "Nested mappings are not allowed in compact mappings",
            position,
        );
        error.kind = ErrorKind::NestedMapping;
        error
    }

    /// 带 yaml token 区间的错误（插入符覆盖 `start..end` 的长度）。
    fn error_span(
        &self,
        message: impl Into<String>,
        start: Position,
        end: Position,
    ) -> FrontmatterError {
        make_error(message.into(), &self.lines, start.0, start.1, Some(end))
    }

    fn error_here(&self, message: impl Into<String>) -> FrontmatterError {
        self.error(message, self.position())
    }

    // -- whitespace helpers -------------------------------------------------

    fn skip_inline_spaces(&mut self) {
        while matches!(self.peek(), Some(' ') | Some('\t')) {
            self.advance();
        }
    }

    fn skip_to_line_end(&mut self) {
        while let Some(c) = self.peek() {
            if c == '\n' {
                break;
            }
            self.advance();
        }
    }

    /// 跳过空行/注释行，停在下一行第一个非空白字符（或 EOF）。
    ///
    /// yaml 的 lexer 把行首空格与 tab 分成两个 token（`pushSpaces(false)` 只吃空格，
    /// 之后 `pushSpaces(true)` 从第一个 tab 起吃掉连续空格/tab）；tab 只有在作为
    /// **内容的缩进**时才由 `resolveProps` 报 `Tabs are not allowed as indentation`：
    ///
    /// - 空行（包括只有 tab/空格的行）和 tab 后面的注释行是合法的；
    /// - tab 后面跟内容时，若 tab 所在列（0 基）<= 所在块集合的缩进，或内容会开始一个
    ///   block map / block sequence，则报错；否则 tab 只是缩进空白（`a:\n  \tfoo`
    ///   的值是 `"foo"`）。
    ///
    /// `parent_indent` 是所在块集合的缩进（文档根为 0）。
    fn skip_blank_and_comment_lines(
        &mut self,
        parent_indent: usize,
    ) -> Result<(), FrontmatterError> {
        loop {
            while self.peek() == Some(' ') {
                self.advance();
            }
            let mut tab: Option<(Position, usize)> = None;
            if self.peek() == Some('\t') {
                let start = self.position();
                let mut width = 0usize;
                while let Some(c @ (' ' | '\t')) = self.peek() {
                    width += c.len_utf16();
                    self.advance();
                }
                tab = Some((start, width));
            }
            match self.peek() {
                None => return Ok(()),
                Some('\n') => {
                    // 空行（含 tab-only）——tab 不算缩进错误。
                    self.advance();
                }
                Some('#') => {
                    // tab 后面的注释行。
                    self.skip_to_line_end();
                }
                Some(_) => {
                    if let Some((start, width)) = tab {
                        let tab_indent = start.1 - 1;
                        if tab_indent <= parent_indent
                            || self.line_starts_block_collection(parent_indent)
                        {
                            return Err(self.error_span(
                                "Tabs are not allowed as indentation",
                                start,
                                (start.0, start.1 + width),
                            ));
                        }
                    }
                    return Ok(());
                }
            }
        }
    }

    /// tab 缩进后的内容是否会开始一个 block map / block sequence
    /// （yaml `resolveProps` 的 `next?.type === 'block-map' || 'block-seq'`）。
    ///
    /// 用 clone 出来的解析器做一次无副作用的 key 探测；flow 集合不会在这里报
    /// TAB_AS_INDENT（yaml 的 `next` 是 flow-collection token）。
    fn line_starts_block_collection(&mut self, parent_indent: usize) -> bool {
        if self.line_starts_sequence_entry() {
            return true;
        }
        if self.peek() == Some('?')
            && matches!(self.peek_at(1), None | Some(' ') | Some('\t') | Some('\n'))
        {
            return true;
        }
        // anchor/tag token 出现在 props 列表里时，yaml 的 `if (tab)` 检查立即报错。
        if matches!(self.peek(), Some('&') | Some('!')) {
            return true;
        }
        if matches!(self.peek(), Some('[') | Some('{')) {
            return false;
        }
        let mut probe = self.clone();
        matches!(probe.try_parse_mapping_key(parent_indent), Ok(Some(_)))
    }

    /// 当前行缩进（调用时位于该行第一个非空白字符）。
    fn current_indent(&self) -> usize {
        self.column - 1
    }

    fn line_starts_sequence_entry(&self) -> bool {
        self.peek() == Some('-')
            && matches!(self.peek_at(1), None | Some(' ') | Some('\t') | Some('\n'))
    }

    /// yaml 的 token 模型里，行内值（plain/quoted/flow）的 range 结束落在哪一行：
    /// 行尾换行、同行注释都会把结束行推到下一行；下一行开头的注释则留在本行。
    fn inline_value_end_line(&self) -> usize {
        match self.peek() {
            Some('\n') => self.line + 1,
            Some('#') if self.column > 1 => self.line + 1,
            _ => self.line,
        }
    }

    /// 文档结束标记 `...`（只在第 1 列生效）。
    fn at_document_end_marker(&self) -> bool {
        self.peek() == Some('.')
            && self.peek_at(1) == Some('.')
            && self.peek_at(2) == Some('.')
            && matches!(self.peek_at(3), None | Some(' ') | Some('\t') | Some('\n'))
    }

    /// 文档开始标记 `---`（只在第 1 列生效）。
    fn at_document_start_marker(&self) -> bool {
        self.peek() == Some('-')
            && self.peek_at(1) == Some('-')
            && self.peek_at(2) == Some('-')
            && matches!(self.peek_at(3), None | Some(' ') | Some('\t') | Some('\n'))
    }

    /// 第 1 列的文档标记 `---` / `...`（yaml 的 continueScalar 用它终止 plain scalar）。
    fn at_block_document_marker(&self) -> bool {
        let marker = match self.peek() {
            Some(c @ ('-' | '.')) => c,
            _ => return false,
        };
        self.peek_at(1) == Some(marker)
            && self.peek_at(2) == Some(marker)
            && matches!(self.peek_at(3), None | Some(' ') | Some('\t') | Some('\n'))
    }

    // -- node properties (anchor / tag) -------------------------------------

    /// 解析节点前的 `&anchor` / `!tag` 属性（可连续出现、顺序任意、以空白分隔）。
    fn parse_properties(&mut self) -> Result<Option<NodeProps>, FrontmatterError> {
        let mut props: Option<NodeProps> = None;
        self.props_start = None;
        loop {
            match self.peek() {
                Some('&') => {
                    let start = self.position();
                    self.props_start.get_or_insert(start);
                    self.advance();
                    let mut name = String::new();
                    while let Some(c) = self.peek() {
                        if is_anchor_char(c) {
                            name.push(c);
                            self.advance();
                        } else {
                            break;
                        }
                    }
                    if name.is_empty() {
                        return Err(self.error("Anchor cannot be an empty string", start));
                    }
                    let length = 1 + name.encode_utf16().count();
                    let props = props.get_or_insert_with(NodeProps::default);
                    if props.anchor.is_some() {
                        return Err(self.error_span(
                            "A node can have at most one anchor",
                            start,
                            (start.0, start.1 + length),
                        ));
                    }
                    props.anchor = Some(AnchorProp {
                        name,
                        position: start,
                        length,
                    });
                }
                Some('!') => {
                    let start = self.position();
                    self.props_start.get_or_insert(start);
                    let raw = self.parse_tag_token();
                    let length = raw.encode_utf16().count();
                    let props = props.get_or_insert_with(NodeProps::default);
                    if props.tag.is_some() {
                        return Err(self.error_span(
                            "A node can have at most one tag",
                            start,
                            (start.0, start.1 + length),
                        ));
                    }
                    props.tag = Some((raw, start, length));
                }
                _ => break,
            }
            self.skip_inline_spaces();
        }
        Ok(props)
    }

    /// 读取一个 tag token（`!<verbatim>`、`!!suffix`、`!handle!suffix`、`!`）。
    ///
    /// verbatim tag 的扫描与 yaml lexer 的 `pushTag` 一致：遇到空白
    /// （空格/tab/换行/回车）或 EOF 就停止，`>` 本身属于 token；所以
    /// `!<tag:yaml.org,2002:str 1` 的 tag token 是 23 个字符，裸 `1` 仍是值。
    fn parse_tag_token(&mut self) -> String {
        if self.peek_at(1) == Some('<') {
            let mut text = String::from("!<");
            self.advance();
            self.advance();
            while let Some(c) = self.peek() {
                if matches!(c, ' ' | '\t' | '\n' | '\r') {
                    break;
                }
                text.push(c);
                self.advance();
                if c == '>' {
                    break;
                }
            }
            return text;
        }
        let mut text = String::from("!");
        self.advance();
        while let Some(c) = self.peek() {
            if is_tag_char(c) {
                text.push(c);
                self.advance();
            } else if c == '%'
                && self.peek_at(1).is_some_and(|next| next.is_ascii_hexdigit())
                && self.peek_at(2).is_some_and(|next| next.is_ascii_hexdigit())
            {
                for _ in 0..3 {
                    if let Some(c) = self.advance() {
                        text.push(c);
                    }
                }
            } else {
                break;
            }
        }
        text
    }

    /// 把节点属性应用到已解析的节点：先用显式 tag 重新解释标量/集合，再把结果以
    /// 锚点名登记（锚点保存**已解析值**，别名解析时 clone）。
    ///
    /// 没有 tag 时清空 `last_scalar_text`，避免上一个标量的原文被后面的 tag 误用；
    /// 显式 tag 解析失败（如非法 `!!timestamp`）在此返回错误。
    fn apply_props(
        &mut self,
        props: Option<NodeProps>,
        value: Value,
    ) -> Result<Value, FrontmatterError> {
        let Some(props) = props else {
            return Ok(value);
        };
        let mut value = value;
        if let Some((raw_tag, position, length)) = &props.tag {
            value = self.resolve_tag(raw_tag, *position, *length, value)?;
        } else {
            self.last_scalar_text = None;
        }
        if let Some(anchor) = props.anchor {
            self.anchors.insert(anchor.name, value.clone());
        }
        Ok(value)
    }

    /// yaml `composeNode` 的 ALIAS_PROPS 检查：别名节点不能再带 `&anchor` / `!tag`。
    /// 错误定位在别名 token（`*name`）。
    fn reject_alias_props(&self, props: &Option<NodeProps>) -> Result<(), FrontmatterError> {
        if props.is_none() || self.peek() != Some('*') {
            return Ok(());
        }
        let start = self.position();
        let width = 1 + self.anchor_name_length_at(1);
        Err(self.error_span(
            "An alias node must not specify any properties",
            start,
            (start.0, start.1 + width),
        ))
    }

    /// `*alias`：查已定义的锚点（yaml 要求锚点先于别名出现）。
    fn parse_alias(&mut self) -> Result<Value, FrontmatterError> {
        let start = self.position();
        self.advance(); // '*'
        let mut name = String::new();
        while let Some(c) = self.peek() {
            if is_anchor_char(c) {
                name.push(c);
                self.advance();
            } else {
                break;
            }
        }
        if name.is_empty() {
            return Err(self.error("Alias cannot be an empty string", start));
        }
        match self.anchors.get(&name) {
            Some(value) => Ok(value.clone()),
            None => {
                if self.unresolved_alias.is_none() {
                    self.unresolved_alias = Some(name);
                }
                Ok(Value::Null)
            }
        }
    }

    /// yaml 的 `directives.tagName`：tag 源码 → 完整 tag 名（或 yaml 的致命错误）。
    ///
    /// 逐字复刻 `yaml` 2.9.0 `doc/directives.js`：
    /// - `!` 是非特定 tag；
    /// - `!<verbatim>` 原样返回内容，但 `!<!>`/`!<!!>` 报错；
    /// - `/^(.*!)([^!]*)$/s` 贪婪取到**最后一个** `!` 作为 handle：`!!suffix` 用默认
    ///   `tag:yaml.org,2002:` 前缀，`!suffix` 是本地 tag，其余未声明的 handle 报
    ///   `Could not resolve tag`；空 suffix 报 `The <tag> tag has no suffix`。
    fn resolve_tag_name(
        &self,
        source: &str,
        position: Position,
        length: usize,
    ) -> Result<String, FrontmatterError> {
        let error =
            |message: String| self.error_span(message, position, (position.0, position.1 + length));
        if source == "!" {
            return Ok("!".to_string());
        }
        if !source.starts_with('!') {
            return Err(error(format!("Not a valid tag: {source}")));
        }
        if source.as_bytes().get(1) == Some(&b'<') {
            // JS 的 `source.slice(2, -1)`：即使没有结尾 `>` 也砍掉最后一个字符。
            let inner = source.get(2..source.len().saturating_sub(1)).unwrap_or("");
            if inner == "!" || inner == "!!" {
                return Err(error(format!(
                    "Verbatim tags aren't resolved, so {source} is invalid."
                )));
            }
            if !source.ends_with('>') {
                return Err(error("Verbatim tags must end with a >".to_string()));
            }
            return Ok(inner.to_string());
        }
        let last = source.rfind('!').expect("tag source starts with '!'");
        let handle = &source[..=last];
        let suffix = &source[last + 1..];
        if suffix.is_empty() {
            return Err(error(format!("The {source} tag has no suffix")));
        }
        match handle {
            // 本地 tag：`!foo` 保持原样（未知 tag 只发 warning，值退化为字符串）。
            "!" => Ok(source.to_string()),
            "!!" => {
                let decoded = percent_encoding::percent_decode_str(suffix)
                    .decode_utf8()
                    .map_err(|_| error("URI malformed".to_string()))?;
                Ok(format!("tag:yaml.org,2002:{decoded}"))
            }
            _ => Err(error(format!("Could not resolve tag: {source}"))),
        }
    }

    /// tag 源码 → 完整 tag 名（`!!str` → `tag:yaml.org,2002:str`）。
    fn resolve_tag(
        &mut self,
        raw_tag: &str,
        position: Position,
        length: usize,
        value: Value,
    ) -> Result<Value, FrontmatterError> {
        let name = self.resolve_tag_name(raw_tag, position, length)?;
        if name.is_empty() {
            // `!<>`：yaml 的 `directives.tagName` 返回空串，而 `composeScalar` /
            // `composeCollection` 都用 falsy 判断（`else if (tagName)`），等价于没有
            // tag —— 标量按默认规则（plain → 数字/布尔/null，引号/block → 字符串）解析。
            // 本解析器在打 tag 之前已经按默认规则得到 `value`，直接原样返回。
            self.last_scalar_text = None;
            return Ok(value);
        }
        match &value {
            Value::Array(_) | Value::Object(_) => {
                self.last_scalar_text = None;
                self.resolve_collection_tag(&name, position, length, value)
            }
            _ => {
                let text = self.last_scalar_text.take().unwrap_or_else(|| {
                    if let Value::String(text) = &value {
                        text.clone()
                    } else {
                        String::new()
                    }
                });
                self.resolve_scalar_tag(&name, &text, position, length)
            }
        }
    }

    /// 显式标量 tag 的解析（`yaml` 包 core schema + knownTags 的行为）。
    fn resolve_scalar_tag(
        &mut self,
        name: &str,
        text: &str,
        position: Position,
        length: usize,
    ) -> Result<Value, FrontmatterError> {
        let string = || Value::String(text.to_string());
        match name {
            "tag:yaml.org,2002:str" | "!" => Ok(string()),
            "tag:yaml.org,2002:int" => {
                if INT_OCTAL_RE.is_match(text) {
                    Ok(number_value(parse_radix_digits(&text[2..], 8)))
                } else if INT_HEX_RE.is_match(text) {
                    Ok(number_value(parse_radix_digits(&text[2..], 16)))
                } else if INT_DECIMAL_RE.is_match(text) {
                    Ok(text
                        .parse::<f64>()
                        .map(number_value)
                        .unwrap_or_else(|_| string()))
                } else {
                    Ok(string())
                }
            }
            "tag:yaml.org,2002:float" => {
                // `findScalarTagByName` 只认 floatNaN / floatExp / float 三个 tag 的 test：
                // `!!float 1` 不匹配任何一个（没有小数点/指数），tag 无法解析（仅 warning），
                // 值保持字符串。旧实现用合并后的 FLOAT_RE 把裸整数也解析成了数字。
                if INFINITY_RE.is_match(text) || NAN_RE.is_match(text) {
                    Ok(Value::Null)
                } else if FLOAT_EXP_RE.is_match(text) || FLOAT_DOT_RE.is_match(text) {
                    Ok(text
                        .parse::<f64>()
                        .map(number_value)
                        .unwrap_or_else(|_| string()))
                } else {
                    Ok(string())
                }
            }
            "tag:yaml.org,2002:bool" => {
                if matches!(text, "true" | "True" | "TRUE") {
                    Ok(Value::Bool(true))
                } else if matches!(text, "false" | "False" | "FALSE") {
                    Ok(Value::Bool(false))
                } else {
                    Ok(string())
                }
            }
            "tag:yaml.org,2002:null" => {
                if text.is_empty() || matches!(text, "~" | "null" | "Null" | "NULL") {
                    Ok(Value::Null)
                } else {
                    Ok(string())
                }
            }
            "tag:yaml.org,2002:binary" => Ok(buffer_value(&decode_base64_node(text))),
            "tag:yaml.org,2002:timestamp" => match parse_timestamp(text) {
                Some(iso) => Ok(Value::String(iso)),
                None => Err(self.error_span(
                    "!!timestamp expects a date, starting with yyyy-mm-dd",
                    position,
                    (position.0, position.1 + length),
                )),
            },
            // 未知 tag / 无法解析的显式 tag：yaml 只发 warning，值退化为字符串。
            _ => Ok(string()),
        }
    }

    /// 集合上的显式 tag：`!!map`/`!!seq`/`!!merge`/自定义 tag 不改变值；
    /// `!!set`/`!!omap`/`!!pairs` 按 yaml 的 knownTags 语义处理（JSON 视角）。
    fn resolve_collection_tag(
        &mut self,
        name: &str,
        position: Position,
        length: usize,
        value: Value,
    ) -> Result<Value, FrontmatterError> {
        match name {
            "tag:yaml.org,2002:set" => {
                if let Value::Object(map) = &value {
                    if map.values().any(|value| !value.is_null()) {
                        return Err(self.error_span(
                            "Set items must all have null values",
                            position,
                            (position.0, position.1 + length),
                        ));
                    }
                    // JS 的 Set → JSON.stringify → {}（空对象）。
                    return Ok(Value::Object(Map::new()));
                }
                Ok(value)
            }
            "tag:yaml.org,2002:omap" => {
                // JS 的 Map → JSON.stringify → {}（空对象）。
                Ok(Value::Object(Map::new()))
            }
            "tag:yaml.org,2002:pairs" => {
                // yaml 的 `resolvePairs`：序列的每一项变成单个 Pair。Pair 的 key 若是
                // 集合节点，JS 键用节点自身的流式表示（`[ 1, 2 ]`）；value 恒为 null。
                let Value::Array(items) = value else {
                    // 非序列：composeCollection 只发 BAD_COLLECTION_TYPE warning，值不变。
                    return Ok(value);
                };
                let mut pairs = Vec::with_capacity(items.len());
                for item in items {
                    match item {
                        Value::Object(map) => {
                            if map.len() > 1 {
                                return Err(self.error_span(
                                    "Each pair must have its own sequence indicator",
                                    position,
                                    (position.0, position.1 + length),
                                ));
                            }
                            if map.is_empty() {
                                // `new Pair(new Scalar(null))` → JS 键 ""。
                                let mut single = Map::new();
                                single.insert(String::new(), Value::Null);
                                pairs.push(Value::Object(single));
                            } else {
                                pairs.push(Value::Object(map));
                            }
                        }
                        other => {
                            let mut single = Map::new();
                            single.insert(stringify_key(&other), Value::Null);
                            pairs.push(Value::Object(single));
                        }
                    }
                }
                Ok(Value::Array(pairs))
            }
            _ => Ok(value),
        }
    }

    // -- document -----------------------------------------------------------

    /// 源文本末尾（忽略结尾空白行）的位置，用作 yaml 文档 range 的结束。
    fn source_end_position(&self) -> Position {
        for (index, line) in self.lines.iter().enumerate().rev() {
            let trimmed = line.trim_end_matches([' ', '\t']);
            if !trimmed.is_empty() {
                return (index + 1, trimmed.encode_utf16().count() + 1);
            }
        }
        (1, 1)
    }

    fn multiple_documents_error(&self, start: Position) -> FrontmatterError {
        // yaml 用第二个文档 token 的起点（行首，含缩进）作为错误位置。
        let start = (start.0, 1);
        let end = self.source_end_position();
        make_error(
            "Source contains multiple documents; please use YAML.parseAllDocuments()".to_string(),
            &self.lines,
            start.0,
            start.1,
            Some(end),
        )
    }

    /// 解析一个 YAML 文档（`yaml.parse` 的单文档模式）。
    ///
    /// 顺序：跳过空行/注释 → 处理第 1 列的 `%` 指令（其后必须跟 `---`）→ 处理
    /// 文档结束标记 `...`（其后还有内容即为多文档错误）→ 处理 `---` → 解析根块节点
    /// → 检查剩余 token（第二个文档、多余标量等）。空输入返回 `Null`，随后在
    /// `parse_frontmatter` 里变成空 map。
    fn parse_document(&mut self) -> Result<Value, FrontmatterError> {
        self.skip_blank_and_comment_lines(0)?;
        if self.eof() {
            return Ok(Value::Null);
        }

        // 指令行（`%YAML` / `%TAG` ...）只在第 1 列、文档开始处生效。指令后必须跟
        // `---`（yaml：`Missing directives-end indicator line` / `...doc-start...`）。
        let mut had_directives = false;
        while self.current_indent() == 0 && self.peek() == Some('%') {
            had_directives = true;
            self.skip_to_line_end();
            self.skip_blank_and_comment_lines(0)?;
            if self.eof() {
                return Err(self.error_here("Missing directives-end indicator line"));
            }
        }
        if had_directives && !(self.current_indent() == 0 && self.at_document_start_marker()) {
            return Err(self.error_here("Missing directives-end/doc-start indicator line"));
        }

        if self.current_indent() == 0 && self.at_document_end_marker() {
            self.skip_to_line_end();
            self.skip_blank_and_comment_lines(0)?;
            if self.eof() {
                return Ok(Value::Null);
            }
            let start = self.position();
            return Err(self.multiple_documents_error(start));
        }

        // 文档开始标记 `---`（frontmatter 提取通常已去掉，`yaml.parse` 本身接受）。
        if self.current_indent() == 0 && self.at_document_start_marker() {
            self.skip_to_line_end();
            self.skip_blank_and_comment_lines(0)?;
            if self.eof() {
                return Ok(Value::Null);
            }
        }

        let indent = self.current_indent();
        let value = self.parse_block_node(indent, None, 0, true)?;
        self.skip_blank_and_comment_lines(0)?;
        if self.current_indent() == 0 && self.at_document_start_marker() {
            // 第二个文档（哪怕内容为空）→ yaml.parse 单文档模式报 MULTIPLE_DOCS。
            let start = self.position();
            return Err(self.multiple_documents_error(start));
        }
        if self.current_indent() == 0 && self.at_document_end_marker() {
            self.skip_to_line_end();
            self.skip_blank_and_comment_lines(0)?;
            if !self.eof() {
                let start = self.position();
                return Err(self.multiple_documents_error(start));
            }
        }
        if !self.eof() {
            let position = self.position();
            let (message, end) = self.unexpected_token_at(position, false);
            return Err(self.error_span(message, position, end));
        }
        Ok(value)
    }

    /// 当前位置上无法继续时的 yaml 错误文案与 token 结束位置。
    fn unexpected_token_at(&self, position: Position, flow: bool) -> (String, Position) {
        let span = |width: usize| (position.0, position.1 + width);
        match self.peek() {
            Some('"') => {
                let end = self.skip_quoted_token(true);
                (
                    "Unexpected double-quoted-scalar at node end".to_string(),
                    end,
                )
            }
            Some('\'') => {
                let end = self.skip_quoted_token(false);
                (
                    "Unexpected single-quoted-scalar at node end".to_string(),
                    end,
                )
            }
            Some('[') => ("Unexpected flow-seq-start at node end".to_string(), span(1)),
            Some('{') => ("Unexpected flow-map-start at node end".to_string(), span(1)),
            Some(']') => (
                "Unexpected flow-seq-end token in YAML stream: \"]\"".to_string(),
                span(1),
            ),
            Some('}') => (
                "Unexpected flow-map-end token in YAML stream: \"}\"".to_string(),
                span(1),
            ),
            Some(':') if self.empty_at(1) => (
                "Unexpected map-value-ind token in YAML stream: \":\"".to_string(),
                span(1),
            ),
            Some('?') if self.empty_at(1) => (
                "Unexpected explicit-key-ind at node end".to_string(),
                span(1),
            ),
            Some('-') if self.line_starts_sequence_entry() => {
                ("Unexpected seq-item-ind at node end".to_string(), span(1))
            }
            Some('&') => (
                "Unexpected anchor at node end".to_string(),
                span(1 + self.anchor_name_length_at(1)),
            ),
            Some('*') => (
                "Unexpected alias at node end".to_string(),
                span(1 + self.anchor_name_length_at(1)),
            ),
            Some('!') => (
                "Unexpected tag at node end".to_string(),
                span(self.tag_token_length_at()),
            ),
            _ => {
                let end = if flow {
                    self.flow_plain_token_end(position)
                } else {
                    self.skip_plain_token_end(position)
                };
                ("Unexpected scalar at node end".to_string(), end)
            }
        }
    }

    /// flow 里 plain scalar token 的结束位置（yaml lexer `parsePlainScalar`：
    /// 行内空白不推进 end，`,[]{}` 与 `: ` 终止 token）。
    fn flow_plain_token_end(&self, start: Position) -> Position {
        let mut cursor = self.pos;
        let mut last_end = self.pos;
        while let Some(c) = self.chars.get(cursor).copied() {
            match c {
                ':' => {
                    let next = self.chars.get(cursor + 1).copied();
                    let stops = matches!(
                        next,
                        None | Some(' ') | Some('\t') | Some('\n') | Some('\r')
                    ) || matches!(
                        next,
                        Some(',') | Some('[') | Some(']') | Some('{') | Some('}')
                    );
                    if stops {
                        break;
                    }
                    last_end = cursor + 1;
                }
                '\n' | '\r' => break,
                ' ' | '\t' => {
                    // 空白后的注释或 flow 指示符结束 token（尾随空白不算）。
                    let next = self.chars.get(cursor + 1).copied();
                    if next == Some('#')
                        || matches!(
                            next,
                            Some(',') | Some('[') | Some(']') | Some('{') | Some('}')
                        )
                    {
                        break;
                    }
                }
                ',' | '[' | ']' | '{' | '}' => break,
                _ => last_end = cursor + 1,
            }
            cursor += 1;
        }
        let width: usize = self.chars[self.pos..last_end]
            .iter()
            .map(|c| c.len_utf16())
            .sum();
        (start.0, start.1 + width)
    }

    fn empty_at(&self, offset: usize) -> bool {
        matches!(
            self.peek_at(offset),
            None | Some(' ') | Some('\t') | Some('\n') | Some('\r')
        )
    }

    fn anchor_name_length_at(&self, offset: usize) -> usize {
        let mut width = 0usize;
        while let Some(c) = self.peek_at(offset + width) {
            if is_anchor_char(c) {
                width += 1;
            } else {
                break;
            }
        }
        width
    }

    fn tag_token_length_at(&self) -> usize {
        if self.peek_at(1) == Some('<') {
            let mut width = 2usize;
            while let Some(c) = self.peek_at(width) {
                width += 1;
                if c == '>' {
                    break;
                }
            }
            return width;
        }
        let mut width = 1usize;
        while let Some(c) = self.peek_at(width) {
            if is_tag_char(c) {
                width += 1;
            } else if c == '%'
                && self
                    .peek_at(width + 1)
                    .is_some_and(|c| c.is_ascii_hexdigit())
                && self
                    .peek_at(width + 2)
                    .is_some_and(|c| c.is_ascii_hexdigit())
            {
                width += 3;
            } else {
                break;
            }
        }
        width
    }

    /// 从当前位置（引号）起计算 token 结束位置：到同行/跨行的闭合引号为止（含）。
    fn skip_quoted_token(&self, double: bool) -> Position {
        let quote = if double { '"' } else { '\'' };
        let mut index = self.pos;
        let mut position = self.position();
        // 跳过开引号
        if let Some(c) = self.chars.get(index).copied() {
            index += 1;
            if c == '\n' {
                position.0 += 1;
                position.1 = 1;
            } else {
                position.1 += c.len_utf16();
            }
        }
        while let Some(c) = self.chars.get(index).copied() {
            index += 1;
            if c == '\n' {
                position.0 += 1;
                position.1 = 1;
                continue;
            }
            position.1 += c.len_utf16();
            if c == quote {
                break;
            }
        }
        position
    }

    /// 当前位置（plain scalar 起点）起 token 结束位置。
    fn skip_plain_token_end(&self, start: Position) -> Position {
        self.plain_token_end_at(self.pos, start)
    }

    /// yaml 的 lexer 里 block plain scalar 的 token 范围：到行尾、` #` 注释或
    /// `: `（map-value-ind）之前，尾部空白不算。
    fn plain_token_end_at(&self, index: usize, start: Position) -> Position {
        let mut cursor = index;
        let mut last_end = index;
        while let Some(c) = self.chars.get(cursor).copied() {
            if c == ':' {
                let next = self.chars.get(cursor + 1).copied();
                if matches!(
                    next,
                    None | Some(' ') | Some('\t') | Some('\n') | Some('\r')
                ) {
                    break;
                }
                last_end = cursor + 1;
            } else if matches!(c, ' ' | '\t' | '\n' | '\r') {
                if c == '\n' || c == '\r' {
                    break;
                }
                if self.chars.get(cursor + 1).copied() == Some('#') {
                    break;
                }
            } else {
                last_end = cursor + 1;
            }
            cursor += 1;
        }
        let width: usize = self.chars[index..last_end]
            .iter()
            .map(|c| c.len_utf16())
            .sum();
        (start.0, start.1 + width)
    }

    // -- block nodes --------------------------------------------------------

    /// 解析一个块上下文的节点（可先带 `&anchor` / `!tag` 属性）。
    ///
    /// 参数：
    /// - `indent`：当前块的缩进；独占一行的属性只有在后续节点缩进 `>= indent` 时才
    ///   作用于该节点，否则属性节点的值为 null；
    /// - `parent`：所在块集合的缩进（`None` 表示文档根），用于跳过空行/注释以及给
    ///   独占一行的 block scalar 提供缩进基准；
    /// - `plain_min_indent`：plain scalar 续行所需的最小缩进；
    /// - `at_root`：是否文档根节点。根节点允许 block scalar 内容缩进 0，并由第 0 列
    ///   的 `---`/`...` 终止；集合内节点没有这些特例。
    ///
    /// 属性分两种：独占一行时直接作用于后面的节点；与节点同一行时先存进
    /// `pending_node_props`，由 `try_parse_mapping_key` 决定它属于第一个 key
    /// （`&x a: 1` 的 anchor 属于 key `a`）还是节点本身。
    fn parse_block_node(
        &mut self,
        indent: usize,
        parent: Option<usize>,
        plain_min_indent: usize,
        at_root: bool,
    ) -> Result<Value, FrontmatterError> {
        self.skip_inline_spaces();
        let props = self.parse_properties()?;
        self.skip_inline_spaces();
        self.reject_alias_props(&props)?;
        if props.is_some() && matches!(self.peek(), None | Some('\n') | Some('#')) {
            // 属性独占一行：节点在后续行上（yaml 允许 `&x` / `!tag` 与节点分行）。
            self.skip_to_line_end();
            self.skip_blank_and_comment_lines(parent.unwrap_or(0))?;
            self.reject_alias_props(&props)?;
            if self.eof() {
                self.last_scalar_text = None;
                return self.apply_props(props, Value::Null);
            }
            let current = self.current_indent();
            if current >= indent {
                let value = self.parse_block_node(current, parent, plain_min_indent, at_root)?;
                return self.apply_props(props, value);
            }
            self.last_scalar_text = None;
            return self.apply_props(props, Value::Null);
        }
        if props.is_some() {
            // 与第一个 key 同一行的属性属于**第一个 key**（yaml 的 CST 会把它们移到
            // key 的 token 序列里）；try_parse_mapping_key 会消费掉它们。只有当节点
            // 不是块映射时才由这里应用到节点本身。
            self.pending_node_props = props;
            let value = self.parse_block_node_inner(indent, parent, plain_min_indent, at_root)?;
            let props = self.pending_node_props.take();
            return self.apply_props(props, value);
        }
        self.parse_block_node_inner(indent, parent, plain_min_indent, at_root)
    }

    /// `parse_block_node` 去掉属性后的实际分派：按首字符选择 flow 集合、block
    /// scalar、块序列、显式/隐式映射、别名或标量。
    ///
    /// `[`/`{` 与其它标量都会先用 `try_parse_mapping_key` 试读一次：能读出 `key:` 就
    /// 按块映射解析（`[1]: v`、`a: b`），否则 `restore` 回起点按值解析。这是块映射
    /// 与 flow 值共用同一入口的判定方式。
    fn parse_block_node_inner(
        &mut self,
        indent: usize,
        parent: Option<usize>,
        plain_min_indent: usize,
        at_root: bool,
    ) -> Result<Value, FrontmatterError> {
        self.last_scalar_text = None;
        match self.peek() {
            None => Ok(Value::Null),
            Some('[') | Some('{') => {
                // 先尝试把 flow 集合当作块映射的 key（`[1]: v` / `{a: 1}: v`）；
                // 不是 key（后面没有 `:`）时恢复并当作普通 flow 值。
                let state = self.state();
                if let Some(key) = self.try_parse_mapping_key(parent.unwrap_or(indent))? {
                    return self.parse_block_mapping(indent, Some(key));
                }
                self.restore(state);
                let context = if at_root {
                    FlowContext::Root
                } else {
                    parent.map_or(FlowContext::Root, FlowContext::block)
                };
                let value = self.parse_flow_node(context, FlowKind::Sequence)?;
                self.expect_end_of_inline_value()?;
                Ok(value)
            }
            Some('|') | Some('>') => self.parse_block_scalar(parent.unwrap_or(indent), at_root),
            Some('-') if self.line_starts_sequence_entry() => self.parse_block_sequence(indent),
            Some('?') if self.at_explicit_key_start() => self.parse_block_mapping(indent, None),
            Some('*') => self.parse_alias(),
            Some(_) => {
                let state = self.state();
                if let Some(key) = self.try_parse_mapping_key(parent.unwrap_or(indent))? {
                    return self.parse_block_mapping(indent, Some(key));
                }
                self.restore(state);
                match self.peek() {
                    Some('"') | Some('\'') => {
                        let value = Value::String(self.parse_quoted_scalar(parent)?);
                        self.expect_end_of_inline_value()?;
                        Ok(value)
                    }
                    _ => {
                        let start = self.position();
                        self.parse_plain_scalar(plain_min_indent, start)
                    }
                }
            }
        }
    }

    /// 解析一个可能内联在 `key: ` / `- ` 之后的节点；`parent_indent` 是所在块集合的缩进。
    fn parse_inline_node(&mut self, parent_indent: usize) -> Result<Value, FrontmatterError> {
        self.skip_inline_spaces();
        let props = self.parse_properties()?;
        self.skip_inline_spaces();
        self.reject_alias_props(&props)?;
        if props.is_some() && matches!(self.peek(), None | Some('\n') | Some('#')) {
            self.skip_to_line_end();
            self.skip_blank_and_comment_lines(parent_indent)?;
            self.reject_alias_props(&props)?;
            if self.eof() {
                self.last_scalar_text = None;
                return self.apply_props(props, Value::Null);
            }
            let current = self.current_indent();
            if current > parent_indent {
                self.pending_key_start = None;
                let value = if matches!(self.peek(), Some('|') | Some('>')) {
                    self.parse_block_scalar(parent_indent, false)?
                } else {
                    self.parse_block_node(current, Some(parent_indent), parent_indent + 1, false)?
                };
                return self.apply_props(props, value);
            }
            // 评审回归：这里**不能**把同缩进的 `- ` 序列当作属性节点的值。属性节点是
            // 序列项本身（`- &x`），下一行同缩进的 `- b` 是兄弟项；yaml 给
            // `a:\n  - &x\n  - b` 的结果是 `[null, "b"]`，而不是 `[["b"]]`。
            // （映射值的同缩进序列由 `parse_mapping_value` 处理，不经过本函数。）
            self.last_scalar_text = None;
            return self.apply_props(props, Value::Null);
        }
        let value = self.parse_inline_value(parent_indent)?;
        self.apply_props(props, value)
    }

    /// 解析 `key: ` / `- ` 之后同一行上的值（节点属性由调用方先处理）。
    ///
    /// `parent_indent` 是所在块集合的缩进，传给 flow 上下文与引号/plain scalar 的
    /// 续行缩进检查。空输入返回 `Null`（`key:` 后直接换行的情况由调用方另行处理，
    /// 这里只兜底 EOF）。
    fn parse_inline_value(&mut self, parent_indent: usize) -> Result<Value, FrontmatterError> {
        self.last_scalar_text = None;
        match self.peek() {
            None => Ok(Value::Null),
            Some('[') | Some('{') => {
                let value =
                    self.parse_flow_node(FlowContext::block(parent_indent), FlowKind::Sequence)?;
                self.expect_end_of_inline_value()?;
                Ok(value)
            }
            Some('|') | Some('>') => self.parse_block_scalar(parent_indent, false),
            Some('"') | Some('\'') => {
                let value = Value::String(self.parse_quoted_scalar(Some(parent_indent))?);
                self.expect_end_of_inline_value()?;
                Ok(value)
            }
            Some('*') => self.parse_alias(),
            Some(_) => {
                let start = self.position();
                self.parse_plain_scalar(parent_indent + 1, start)
            }
        }
    }

    /// 内联值结束后：只允许行内空白、注释或行尾。
    fn expect_end_of_inline_value(&mut self) -> Result<(), FrontmatterError> {
        self.skip_inline_spaces();
        match self.peek() {
            None | Some('\n') => Ok(()),
            Some('#') => {
                self.skip_to_line_end();
                Ok(())
            }
            Some(_) => {
                let position = self.position();
                let (message, end) = self.unexpected_token_at(position, false);
                Err(self.error_span(message, position, end))
            }
        }
    }

    // -- mapping ------------------------------------------------------------

    /// 尝试解析 `key:`。成功时消费 key 与 `:` 并返回键信息（JS 文案 + YAML 值身份）；
    /// 该行不是 mapping 项时返回 `None`（调用方负责恢复状态）。
    ///
    /// `parent_indent` 是所在块集合的缩进（quoted key 的跨行续行缩进基准）。
    fn try_parse_mapping_key(
        &mut self,
        parent_indent: usize,
    ) -> Result<Option<KeyInfo>, FrontmatterError> {
        let state = self.state();
        self.skip_inline_spaces();
        let props = self.parse_properties()?;
        self.skip_inline_spaces();
        self.reject_alias_props(&props)?;
        // keyStart：有属性时是属性之后，否则若上一对的值为空，则是上一行行尾的换行。
        let mut key_position = self.position();
        if props.is_none()
            && let Some((pending, pending_indent)) = self.pending_key_start
            && pending_indent == parent_indent
            && pending.0 + 1 == self.line
        {
            key_position = pending;
            self.pending_key_start = None;
        }
        self.last_scalar_text = None;

        let mut plain_info: Option<KeyInfo> = None;
        let mut multiline = false;
        let key_value: Value = match self.peek() {
            Some('"') | Some('\'') => {
                let text = self.parse_quoted_scalar(Some(parent_indent))?;
                multiline = self.last_quoted_multiline;
                Value::String(text)
            }
            Some('*') => self.parse_alias()?,
            Some('[') | Some('{') => {
                // flow 集合作为块映射的隐式 key（`[1]: v`）。yaml 要求隐式 key 单行，
                // 多行 flow 在 parse_block_mapping 里报 MULTILINE_IMPLICIT_KEY。
                let kind = if self.peek() == Some('[') {
                    FlowKind::Sequence
                } else {
                    FlowKind::Map
                };
                let start_line = self.line;
                let context = FlowContext::block(parent_indent);
                match self.parse_flow_node(context, kind) {
                    Ok(value) => {
                        if self.line != start_line {
                            multiline = true;
                        }
                        value
                    }
                    Err(_) => {
                        // 让调用方的普通 flow 解析路径报错（flow 上下文与文案一致）。
                        self.restore(state);
                        return Ok(None);
                    }
                }
            }
            Some(']') | Some('}') => {
                // 不是 mapping key：把位置留给 plain scalar 的 flow-end 错误
                // （`]foo: 1` 在 yaml 里是 "Unexpected flow-seq-end token ..."）。
                self.restore(state);
                return Ok(None);
            }
            // `? ` 是显式 key 指示符，不是隐式 key 的一部分。但 `: ` 在块映射条目
            // 开头是**空 key** 的隐式 key（`---\n: 1` → `{"": 1}`，`---\n: : v` →
            // Nested mappings，位置在内层 `:`），必须走下面的 `Some(_)` 扫描：
            // 冒号位于起点时 raw 为空串，得到 null key。
            Some('?') if matches!(self.peek_at(1), None | Some(' ') | Some('\t') | Some('\n')) => {
                self.restore(state);
                return Ok(None);
            }
            Some(_) => {
                // 预留字符错误在 yaml 里要等 token 扫完才报；若这一行不是 key（没有 `: `），
                // 由值路径（parse_plain_scalar）报，可能先变成 MULTILINE/Nested 错误。
                let reserved: Option<String> = match self.peek() {
                    Some('@') => {
                        Some("Plain value cannot start with reserved character @".to_string())
                    }
                    Some('`') => {
                        Some("Plain value cannot start with reserved character `".to_string())
                    }
                    Some('%') => Some(
                        "Plain value cannot start with directive indicator character %".to_string(),
                    ),
                    Some(',') => {
                        Some("Plain value cannot start with flow indicator character ,".to_string())
                    }
                    _ => None,
                };
                let start = self.pos;
                let mut colon = None;
                loop {
                    match self.peek() {
                        None | Some('\n') => break,
                        Some('#')
                            if self.pos == start
                                || matches!(
                                    self.chars.get(self.pos - 1),
                                    Some(' ') | Some('\t')
                                ) =>
                        {
                            break;
                        }
                        Some(':')
                            if matches!(
                                self.peek_at(1),
                                None | Some(' ') | Some('\t') | Some('\n')
                            ) =>
                        {
                            colon = Some(self.pos);
                            break;
                        }
                        Some(_) => {
                            self.advance();
                        }
                    }
                }
                let Some(colon) = colon else {
                    self.restore(state);
                    return Ok(None);
                };
                if let Some(message) = reserved {
                    return Err(self.error(message, key_position));
                }
                let raw: String = self.chars[start..colon].iter().collect();
                let raw = raw.trim_end_matches([' ', '\t']).to_string();
                let (value, info) = resolve_plain_scalar_key(&raw);
                self.last_scalar_text = Some(raw);
                plain_info = Some(info);
                value
            }
            None => {
                self.restore(state);
                return Ok(None);
            }
        };

        self.skip_inline_spaces();
        if self.peek() != Some(':') {
            self.restore(state);
            return Ok(None);
        }
        self.advance(); // ':'
        // 与 key 同一行的节点属性（`&x a: 1`）在源码顺序上排在这个 key 自己的属性之前。
        let pending = self.pending_node_props.take();
        let props = self.merge_key_props(pending, props)?;
        let key_value = self.apply_props(props.clone(), key_value)?;
        let mut key_info = match (props.is_some(), plain_info) {
            (false, Some(info)) => info,
            _ => key_info_from_value(&key_value),
        };
        key_info.multiline = multiline;
        key_info.position = key_position;
        Ok(Some(key_info))
    }

    /// 合并块节点属性（同一行、在第一个 key 之前）与该 key 自己的属性。
    /// yaml 的 `resolveProps` 按源码顺序处理，重复的锚点/标签报在**后出现**的 token 上。
    fn merge_key_props(
        &self,
        first: Option<NodeProps>,
        second: Option<NodeProps>,
    ) -> Result<Option<NodeProps>, FrontmatterError> {
        match (first, second) {
            (None, other) | (other, None) => Ok(other),
            (Some(mut first), Some(second)) => {
                if let Some(anchor) = second.anchor {
                    if first.anchor.is_some() {
                        return Err(self.error_span(
                            "A node can have at most one anchor",
                            anchor.position,
                            (anchor.position.0, anchor.position.1 + anchor.length),
                        ));
                    }
                    first.anchor = Some(anchor);
                }
                if let Some(tag) = second.tag {
                    if first.tag.is_some() {
                        return Err(self.error_span(
                            "A node can have at most one tag",
                            tag.1,
                            (tag.1.0, tag.1.1 + tag.2),
                        ));
                    }
                    first.tag = Some(tag);
                }
                Ok(Some(first))
            }
        }
    }

    /// yaml 的显式 key 指示符 `?`（后跟空白 / EOL / EOF）。
    fn at_explicit_key_start(&self) -> bool {
        self.peek() == Some('?')
            && matches!(self.peek_at(1), None | Some(' ') | Some('\t') | Some('\n'))
    }

    fn at_map_value_indicator(&self) -> bool {
        self.peek() == Some(':')
            && matches!(self.peek_at(1), None | Some(' ') | Some('\t') | Some('\n'))
    }

    /// 解析 `? <key>` 里的 key 节点（不含 value 指示符）。
    ///
    /// key 可以是任意块节点：标量（含续行）、引号标量、flow 集合、block scalar、
    /// 紧凑映射（`? a: b` → key 是 `{ a: b }`）或缩进的块序列/映射。
    fn parse_explicit_key(&mut self, indent: usize) -> Result<KeyInfo, FrontmatterError> {
        let key_position = self.position();
        self.pending_key_start = None;
        self.advance(); // '?'
        self.skip_inline_spaces();
        let key_value = if self.eof() || matches!(self.peek(), Some('\n') | Some('#')) {
            self.last_scalar_text = None;
            Value::Null
        } else {
            // key 是 `? ` 之后的一个块节点。plain scalar 的续行缩进基准是 map 缩进 + 1
            // （`? a\n b` → "a b"），而不是 key 的起始列。
            let key_indent = self.current_indent();
            let saved = self.explicit_key_plain;
            self.explicit_key_plain = true;
            let parsed = self.parse_block_node(key_indent, Some(indent), indent + 1, false);
            self.explicit_key_plain = saved;
            parsed?
        };
        let mut info = key_info_from_value(&key_value);
        info.position = key_position;
        Ok(info)
    }

    /// 显式 key 的 value：`:` 指示符可以在同一行或后续行（缩进 >= map 缩进），
    /// 没有指示符时 value 为 null（`? a` → `{a: null}`）。
    fn parse_explicit_key_value(&mut self, indent: usize) -> Result<Value, FrontmatterError> {
        // plain scalar key 停在续行行首的 `: ` 上时，位置已经在指示符处。
        if self.at_map_value_indicator() && self.current_indent() >= indent {
            self.advance();
            return self.parse_mapping_value(indent, true);
        }
        // key 的续行结束时 plain_continuation 已经吃掉了换行：此时不能再
        // `skip_to_line_end`（否则会把下一行的第一个 token 当作 key 的残留吞掉，
        // `? a\nb` 的 `b` 会消失）。
        if !self.at_line_start() {
            self.skip_to_line_end();
        }
        self.skip_blank_and_comment_lines(indent)?;
        if self.eof() {
            self.last_scalar_text = None;
            return Ok(Value::Null);
        }
        if self.current_indent() >= indent && self.at_map_value_indicator() {
            self.advance();
            return self.parse_mapping_value(indent, true);
        }
        self.last_scalar_text = None;
        Ok(Value::Null)
    }

    /// 当前是否位于行首（上一个消费的字符是换行，或位于输入开头）。
    fn at_line_start(&self) -> bool {
        self.pos == 0 || self.chars.get(self.pos - 1) == Some(&'\n')
    }

    /// 解析一个块映射（缩进为 `indent`）。`first_key` 是调用方已经消费掉的第一个
    /// 键（紧凑映射 `- a: 1`、`[1]: v` 等）；为 `None` 时从当前位置开始取键。
    ///
    /// 每轮：取键（隐式或 `? ` 显式）→ 按 YAML 值身份查重 → 解析值 → 跳过空行/注释
    /// → 判定下一个 token：缩进相同则继续本映射，缩进更小或遇到文档标记则结束，
    /// 缩进更大则报错。错误插入符通常定位在**上一个值的结束行**（块标量用
    /// `last_block_scalar_end`，行内值用 `inline_value_end_line`），因此块标量结尾的
    /// 空行不会把位置带到下一行。
    ///
    /// 返回值经 `js_order_object` 调整为 JS 对象的键序。
    fn parse_block_mapping(
        &mut self,
        indent: usize,
        first_key: Option<KeyInfo>,
    ) -> Result<Value, FrontmatterError> {
        let mut map: Map<String, Value> = Map::new();
        let mut seen_keys: Vec<KeyId> = Vec::new();
        let mut next_key = first_key;
        let mut previous_value_end_line = self.line;

        loop {
            let (key, explicit_value) = match next_key.take() {
                Some(key) => (key, None),
                None => {
                    if self.at_explicit_key_start() {
                        let key = self.parse_explicit_key(indent)?;
                        let value = self.parse_explicit_key_value(indent)?;
                        (key, Some(value))
                    } else if self.line_starts_sequence_entry() {
                        let position = (previous_value_end_line, 1);
                        // 只有第 0 列、上一值是行内值且序列行紧跟其后时，块序列 token 的
                        // start 才含换行（containsNewline → MULTILINE），否则是 BLOCK_AS_IMPLICIT_KEY。
                        let multiline = indent == 0
                            && !self.last_value_block
                            && self.line == previous_value_end_line;
                        let message = if multiline {
                            "Implicit keys need to be on a single line"
                        } else {
                            "A block sequence may not be used as an implicit map key"
                        };
                        return Err(self.error(message, position));
                    } else {
                        match self.try_parse_mapping_key(indent)? {
                            Some(key) => (key, None),
                            None => {
                                // yaml 会先把该位置当作隐式 key 组合出来；若它跨行（或属性后换行），
                                // 报 MULTILINE_IMPLICIT_KEY，否则才是 “key 没有值”。key 的位置在
                                // 属性之后（yaml 的 keyProps.end）。
                                let state = self.state();
                                let _props = self.parse_properties()?;
                                self.skip_inline_spaces();
                                if matches!(self.peek(), None | Some('\n') | Some('#')) {
                                    self.skip_to_line_end();
                                    self.skip_blank_and_comment_lines(indent)?;
                                    let position = self.position();
                                    self.restore(state);
                                    return Err(self.error(
                                        "Implicit keys need to be on a single line",
                                        position,
                                    ));
                                }
                                let start = self.position();
                                let start_index = self.pos;
                                // yaml 的 parseLineStart：只有当行首第二个字符非空时，才会把
                                // indentNext 降到本行缩进；否则沿用上层（近似为 map 缩进 + 1）。
                                let lowered = !matches!(
                                    self.chars.get(self.pos + 1).copied(),
                                    None | Some(' ') | Some('\t') | Some('\n')
                                );
                                let min_indent = if lowered { indent } else { indent + 1 };
                                let parsed = self.parse_plain_scalar(min_indent, start);
                                let multiline = self.last_plain_scalar_multiline;
                                let end_position = self.position();
                                self.restore(state);
                                self.last_plain_scalar_multiline = false;
                                match parsed {
                                    Ok(_) if multiline => {
                                        return Err(self.error_span(
                                            "Implicit keys need to be on a single line",
                                            start,
                                            end_position,
                                        ));
                                    }
                                    Err(_) if multiline => {
                                        return Err(self.error_span(
                                            "Implicit keys need to be on a single line",
                                            start,
                                            end_position,
                                        ));
                                    }
                                    // 评审回归：yaml 在 “key 没有值” 之前先报 token 自身的错误
                                    // （`]`/`}` 的 flow-end、预留字符 `@`/`` ` ``/`%`/`,` 等）。
                                    Err(error) => return Err(error),
                                    Ok(_) => {
                                        let token_end = self.plain_token_end_at(start_index, start);
                                        return Err(self.error_span(
                                            "Implicit map keys need to be followed by map values",
                                            start,
                                            token_end,
                                        ));
                                    }
                                }
                            }
                        }
                    }
                }
            };

            if key.multiline {
                return Err(self.error("Implicit keys need to be on a single line", key.position));
            }

            // yaml 包按 YAML 值身份查重（`null` 与 `""`、`1` 与 `"1"` 不同；`NaN` 永不相等）。
            if seen_keys.contains(&key.id) {
                return Err(self.error("Map keys must be unique", key.position));
            }
            seen_keys.push(key.id);

            let value = match explicit_value {
                Some(value) => value,
                None => self.parse_mapping_value(indent, false)?,
            };
            map.insert(key.text, value);
            let value_end_line = self
                .last_block_scalar_end
                .take()
                .unwrap_or_else(|| self.inline_value_end_line());
            previous_value_end_line = value_end_line;

            self.skip_blank_and_comment_lines(indent)?;
            if self.eof() {
                break;
            }
            let current = self.current_indent();
            if current == 0 && (self.at_document_start_marker() || self.at_document_end_marker()) {
                break;
            }
            if current < indent {
                break;
            }
            if current > indent {
                // yaml 包把错误定位在**上一个值的结束偏移**：block scalar 是最后一行内容之后，
                // 中间的空行/注释行不算（见 `parse_block_scalar` 的 `last_block_scalar_end`）。
                let position = (value_end_line, 1);
                if self.line_starts_sequence_entry() {
                    return Err(self.error(
                        "A block sequence may not be used as an implicit map key",
                        position,
                    ));
                }
                return Err(self.error("All mapping items must start at the same column", position));
            }
        }

        Ok(Value::Object(js_order_object(map)))
    }

    /// 解析 `key:` / `? key\n:` 之后的 value。
    ///
    /// `allow_same_line_sequence`：显式 key 的 `:` 在行首时，`:` 后的 `- ` 是合法的
    /// 块序列（`? a\n: - 1\n  - 2`）；隐式 key 的 `a: - 1` 仍报
    /// `Unexpected block-seq-ind on same line with key`。
    fn parse_mapping_value(
        &mut self,
        indent: usize,
        allow_same_line_sequence: bool,
    ) -> Result<Value, FrontmatterError> {
        self.last_block_scalar_end = None;
        self.pending_key_start = None;
        self.last_value_block = false;
        self.skip_inline_spaces();
        let props = self.parse_properties()?;
        self.skip_inline_spaces();
        self.reject_alias_props(&props)?;

        if self.eof() || self.peek() == Some('\n') || self.peek() == Some('#') {
            if self.peek() == Some('\n') {
                self.pending_key_start = Some((self.position(), indent));
            }
            self.skip_to_line_end();
            self.skip_blank_and_comment_lines(indent)?;
            self.reject_alias_props(&props)?;
            if self.eof() {
                self.last_scalar_text = None;
                return self.apply_props(props, Value::Null);
            }
            let current = self.current_indent();
            if current > indent {
                // 嵌套块节点有自己的 token 起点：上一行行尾的换行不属于它的 key。
                self.pending_key_start = None;
                // 独占一行的 block scalar：yaml token 的 indent 是**所在块集合**的缩进，
                // 隐式缩进指示符以此为基准（见 `parse_block_scalar`）。
                let value = if matches!(self.peek(), Some('|') | Some('>')) {
                    self.parse_block_scalar(indent, false)?
                } else {
                    self.parse_block_node(current, Some(indent), indent + 1, false)?
                };
                self.last_value_block = true;
                return self.apply_props(props, value);
            }
            if current == indent && self.line_starts_sequence_entry() {
                self.pending_key_start = None;
                let value = self.parse_block_sequence(indent)?;
                self.last_value_block = true;
                return self.apply_props(props, value);
            }
            self.last_scalar_text = None;
            return self.apply_props(props, Value::Null);
        }

        let value_position = self.position();
        match self.peek() {
            Some('-') if self.line_starts_sequence_entry() => {
                if allow_same_line_sequence {
                    let item_indent = self.current_indent();
                    self.pending_key_start = None;
                    let value = self.parse_block_sequence(item_indent)?;
                    self.last_value_block = true;
                    return self.apply_props(props, value);
                }
                Err(self.error(
                    "Unexpected block-seq-ind on same line with key",
                    value_position,
                ))
            }
            Some('?') | Some(':')
                if matches!(self.peek_at(1), None | Some(' ') | Some('\t') | Some('\n')) =>
            {
                Err(self.error(
                    "Nested mappings are not allowed in compact mappings",
                    value_position,
                ))
            }
            _ => {
                let value = self.parse_inline_value(indent)?;
                self.apply_props(props, value)
            }
        }
    }

    // -- sequence -----------------------------------------------------------

    /// 解析块序列（每项以 `- ` 开始，缩进为 `indent`）。
    ///
    /// 项的形态：`-` 后无内容 → null；缩进更深的块节点或 block scalar；同一行紧跟的
    /// `- `（`- - a` 的紧凑嵌套序列，内层缩进取 `current_indent()`，即外层 indent + 2）；
    /// flow 集合；紧凑映射（`- a: 1`）。最后一种失败且错误是 “Nested mappings”、
    /// 同时 plain scalar 跨行时，改报 yaml 的 MULTILINE_IMPLICIT_KEY 文案（见分支内注释）。
    ///
    /// 结束前清空 `last_block_scalar_end`：序列本身不是块标量，不能让最后一项的
    /// 结束行泄漏给外层映射的错误定位。
    fn parse_block_sequence(&mut self, indent: usize) -> Result<Value, FrontmatterError> {
        let mut items: Vec<Value> = Vec::new();

        loop {
            // 当前位置是 '-'（缩进 == indent）。
            self.advance(); // '-'
            self.skip_inline_spaces();

            if self.eof() || self.peek() == Some('\n') || self.peek() == Some('#') {
                self.skip_to_line_end();
                self.skip_blank_and_comment_lines(indent)?;
                if self.eof() {
                    items.push(Value::Null);
                    break;
                }
                let current = self.current_indent();
                if current > indent {
                    if matches!(self.peek(), Some('|') | Some('>')) {
                        items.push(self.parse_block_scalar(indent, false)?);
                    } else {
                        items.push(self.parse_block_node(
                            current,
                            Some(indent),
                            indent + 1,
                            false,
                        )?);
                    }
                } else {
                    items.push(Value::Null);
                }
            } else if self.line_starts_sequence_entry() {
                let item_indent = self.current_indent();
                items.push(self.parse_block_sequence(item_indent)?);
            } else if matches!(self.peek(), Some('[') | Some('{')) {
                // flow 集合可以作为 compact mapping 的隐式 key（`- [1]: v`）；
                // 后面没有 `:` 时整体是一个序列项。
                let item_indent = self.current_indent();
                let state = self.state();
                if let Some(key) = self.try_parse_mapping_key(indent)? {
                    items.push(self.parse_block_mapping(item_indent, Some(key))?);
                } else {
                    self.restore(state);
                    items.push(self.parse_inline_node(indent)?);
                }
            } else {
                let item_indent = self.current_indent();
                let state = self.state();
                if let Some(key) = self.try_parse_mapping_key(indent)? {
                    items.push(self.parse_block_mapping(item_indent, Some(key))?);
                } else {
                    self.restore(state);
                    match self.parse_inline_node(indent) {
                        Ok(value) => items.push(value),
                        Err(error)
                            if error.kind == ErrorKind::NestedMapping
                                && self.last_plain_scalar_multiline =>
                        {
                            // 序列项里跨行的 plain scalar 被 yaml 当作 compact mapping 的
                            // 隐式 key → MULTILINE_IMPLICIT_KEY（而不是 Nested mappings）。
                            return Err(self.error(
                                "Implicit keys need to be on a single line",
                                (error.line, error.column),
                            ));
                        }
                        Err(error) => return Err(error),
                    }
                }
            }

            let item_end_line = self
                .last_block_scalar_end
                .take()
                .unwrap_or_else(|| self.inline_value_end_line());
            self.skip_blank_and_comment_lines(indent)?;
            if self.eof() {
                break;
            }
            let current = self.current_indent();
            if current == 0 && (self.at_document_start_marker() || self.at_document_end_marker()) {
                break;
            }
            if current < indent {
                break;
            }
            if current > indent {
                if self.line_starts_sequence_entry() {
                    return Err(self.error(
                        "All sequence items must start at the same column",
                        self.position(),
                    ));
                }
                return Err(self.error("Sequence item without - indicator", (item_end_line, 1)));
            }
            if !self.line_starts_sequence_entry() {
                break;
            }
        }

        // 序列不是 block scalar，别把最后一个元素留下的结束行泄漏给外层 mapping。
        self.last_block_scalar_end = None;
        Ok(Value::Array(items))
    }

    // -- plain scalars ------------------------------------------------------

    /// 解析 block plain scalar：读到行尾并按 yaml 的规则折叠续行。
    ///
    /// - `min_indent`：续行要求的最小缩进（按**空格**数计，tab 见
    ///   `plain_continuation`）；不足则 scalar 在此结束；
    /// - `start`：scalar 起点，用于预留字符与 “Nested mappings” 错误的定位。
    ///
    /// 行内 `#`（其前是行首或空白）开始注释；非显式 key 上下文里遇到 `: ` 立即报
    /// “Nested mappings are not allowed in compact mappings”。续行折叠：1 个换行变
    /// 一个空格，n 个换行变 n-1 个 `\n`，行尾空白在拼接前去掉。首字符若是预留字符
    /// （`@`/`` ` ``/`%`/`,`），错误要等整个 token 扫完才报，以便跨行隐式键的
    /// MULTILINE 错误优先。
    fn parse_plain_scalar(
        &mut self,
        min_indent: usize,
        start: Position,
    ) -> Result<Value, FrontmatterError> {
        self.last_plain_scalar_multiline = false;
        // 显式 key 的 “续行行首 `: ` 结束 scalar” 只作用于 key 本身的第一个 plain scalar。
        let explicit_key = self.explicit_key_plain;
        self.explicit_key_plain = false;
        // yaml 把 “Nested mappings are not allowed in compact mappings” 定位在
        // valueProps.end（节点属性之后/之前？实测是属性起点）。
        let mapping_error_start = self
            .props_start
            .take()
            .filter(|position| position.0 == start.0)
            .unwrap_or(start);
        // yaml 的 lexer 在 plain scalar 起点遇到 `]`/`}` 时产生 flow-seq-end /
        // flow-map-end token，CST parser 立即报错（`description: ]foo` 在 TS 里是
        // 致命错误，而不是字符串）。块上下文里这两个字符不能再作为 plain scalar 开头。
        if let Some(c @ (']' | '}')) = self.peek() {
            let position = self.position();
            let name = if c == ']' {
                "flow-seq-end"
            } else {
                "flow-map-end"
            };
            return Err(self.error_span(
                format!("Unexpected {name} token in YAML stream: \"{c}\""),
                position,
                (position.0, position.1 + 1),
            ));
        }
        // yaml 的 BAD_SCALAR_START 在 token 扫描完成后才报；若这个 plain scalar 跨行变成
        // compact mapping 的隐式 key，则 “Nested mappings”/MULTILINE 错误先出现。这里先记下
        // 预留字符错误，扫描结束再报。
        let reserved_error: Option<&'static str> = match self.peek() {
            Some('@') => Some("Plain value cannot start with reserved character @"),
            Some('`') => Some("Plain value cannot start with reserved character `"),
            Some('%') => Some("Plain value cannot start with directive indicator character %"),
            Some(',') => Some("Plain value cannot start with flow indicator character ,"),
            _ => None,
        };
        let mut text = String::new();
        // 续行行首：yaml 的 plain scalar 在 `: `（map-value-ind）前断开。对显式 key
        // 这是合法的分隔符（`? a\n : 1`），其他上下文仍报 Nested mappings。
        let mut at_continuation_start = false;
        let mut fold_start = 0usize;
        loop {
            let mut segment = String::new();
            let mut ended_by_comment = false;
            let mut ended_by_value_indicator = false;
            loop {
                match self.peek() {
                    None | Some('\n') => break,
                    Some('#')
                        if segment.is_empty()
                            || matches!(self.chars.get(self.pos - 1), Some(' ') | Some('\t')) =>
                    {
                        ended_by_comment = true;
                        break;
                    }
                    Some(':')
                        if at_continuation_start
                            && explicit_key
                            && segment.is_empty()
                            && matches!(
                                self.peek_at(1),
                                None | Some(' ') | Some('\t') | Some('\n')
                            ) =>
                    {
                        // 位置停在 ':'，由 parse_explicit_key_value 消费。
                        text.truncate(fold_start);
                        ended_by_value_indicator = true;
                        break;
                    }
                    Some(':')
                        if matches!(
                            self.peek_at(1),
                            None | Some(' ') | Some('\t') | Some('\n')
                        ) =>
                    {
                        return Err(self.nested_mapping_error(mapping_error_start));
                    }
                    Some(c) => {
                        segment.push(c);
                        self.advance();
                    }
                }
            }
            if ended_by_value_indicator {
                break;
            }
            text.push_str(segment.trim_end_matches([' ', '\t']));

            if ended_by_comment || self.eof() {
                break;
            }
            let line_before_continuation = self.line;
            match self.plain_continuation(min_indent)? {
                Some(breaks) => {
                    self.last_plain_scalar_multiline = true;
                    at_continuation_start = true;
                    fold_start = text.len();
                    if breaks == 1 {
                        text.push(' ');
                    } else {
                        for _ in 0..breaks - 1 {
                            text.push('\n');
                        }
                    }
                }
                None => {
                    // 空行也算 yaml key 标量源码里的换行（containsNewline）。
                    if self.line > line_before_continuation + 1 {
                        self.last_plain_scalar_multiline = true;
                    }
                    break;
                }
            }
        }
        if let Some(message) = reserved_error {
            return Err(self.error(message, start));
        }
        self.last_scalar_text = Some(text.clone());
        Ok(resolve_plain_scalar(&text))
    }

    /// 位于 plain scalar 的行尾换行处：判断下一行是否是续行。
    ///
    /// 返回 `Some(breaks)`（折行数）表示继续；`None` 表示 scalar 结束（位置留在结束处）。
    ///
    /// yaml 的 `continueScalar` 只用**空格**计算缩进：遇到 tab 时若空格数 >= indentNext
    /// 则继续（tab 及其后的空格/tab 都属于缩进空白，折叠时被换行折叠吞掉）。
    /// `a: 1\n  \tfoo` 在 yaml 里是 `"1 foo"`。
    fn plain_continuation(&mut self, min_indent: usize) -> Result<Option<usize>, FrontmatterError> {
        let mut breaks = 1usize;
        self.advance(); // '\n'
        loop {
            let mut spaces = 0usize;
            while self.peek() == Some(' ') {
                self.advance();
                spaces += 1;
            }
            let mut after_tab = false;
            if self.peek() == Some('\t') {
                if spaces < min_indent {
                    return Ok(None);
                }
                after_tab = true;
                while matches!(self.peek(), Some(' ') | Some('\t')) {
                    self.advance();
                }
            }
            match self.peek() {
                None => return Ok(None),
                Some('\n') => {
                    // 空白行（含 tab）也计入折行数。
                    breaks += 1;
                    self.advance();
                }
                Some('#') => return Ok(None),
                Some(_) => {
                    // yaml 的 continueScalar：第 0 列的 `---`/`...` 会终止 plain scalar。
                    if spaces == 0 && !after_tab && self.at_block_document_marker() {
                        return Ok(None);
                    }
                    // 续行缩进只要 >= indentNext（本 scalar 的最小缩进）。
                    if after_tab || spaces >= min_indent {
                        return Ok(Some(breaks));
                    }
                    return Ok(None);
                }
            }
        }
    }

    // -- quoted scalars -----------------------------------------------------

    /// 解析单/双引号标量并返回折叠后的文本。
    ///
    /// 内部用 `Vec<u16>` 累积 UTF-16 code unit：`\uXXXX` / `\UXXXXXXXX` 转义写出的
    /// 是 code unit 而非 `char`，合法代理对由 `from_utf16_lossy` 合并；孤立代理项
    /// （JS 会原样保留）在 Rust 中无法表示，替换为 U+FFFD；不会 panic（见模块文档
    /// “已知偏差”）。
    ///
    /// - 单引号内 `''` 表示一个 `'`；双引号支持 yaml 的全部转义与 `\` 续行；
    /// - 跨行按 foldNewline 折叠：1 个换行 → 空格，n 个 → n-1 个换行；换行前的行尾
    ///   空白（`trailing_ws`）被丢弃；
    /// - `parent` 是续行必须超过的缩进基准（块集合缩进），`spaces <= parent` 时报
    ///   “Missing closing quote”；`None` 表示 flow/根上下文，不检查；
    /// - 源串在 `\"` 转义后恰好 EOF 时，yaml 把该引号视为闭合（见分支内注释）。
    fn parse_quoted_scalar(&mut self, parent: Option<usize>) -> Result<String, FrontmatterError> {
        let quote = self.peek().expect("caller checked the quote");
        self.advance();
        let double_quoted = quote == '"';
        let mut units: Vec<u16> = Vec::new();
        let mut trailing_ws = 0usize;
        self.last_quoted_multiline = false;

        loop {
            let Some(c) = self.peek() else {
                return Err(self.error(format!("Missing closing {quote}quote"), self.position()));
            };

            if c == quote {
                if !double_quoted && self.peek_at(1) == Some('\'') {
                    // 单引号里的 '' 是转义的 '。
                    self.advance();
                    self.advance();
                    units.push(u16::from(b'\''));
                    trailing_ws = 0;
                    continue;
                }
                self.advance();
                let text = String::from_utf16_lossy(&units);
                self.last_scalar_text = Some(text.clone());
                return Ok(text);
            }

            match c {
                '\n' => {
                    self.last_quoted_multiline = true;
                    let break_position = self.position();
                    units.truncate(units.len().saturating_sub(trailing_ws));
                    trailing_ws = 0;
                    self.advance();
                    let mut breaks = 1usize;
                    let mut indent_ok = false;
                    loop {
                        let mut spaces = 0usize;
                        while self.peek() == Some(' ') {
                            self.advance();
                            spaces += 1;
                        }
                        match self.peek() {
                            None => {
                                return Err(self.error(
                                    format!("Missing closing {quote}quote"),
                                    break_position,
                                ));
                            }
                            Some('\n') => {
                                // 空行不受缩进限制（yaml continueScalar 的特例）。
                                breaks += 1;
                                indent_ok = false;
                                self.advance();
                            }
                            Some('\t') => {
                                if !indent_ok && parent.is_some_and(|limit| spaces <= limit) {
                                    return Err(self.error(
                                        format!("Missing closing {quote}quote"),
                                        break_position,
                                    ));
                                }
                                // foldNewline 把续行缩进中的 tab 当空白跳过。
                                indent_ok = true;
                                self.advance();
                            }
                            Some(_) => {
                                if !indent_ok && parent.is_some_and(|limit| spaces <= limit) {
                                    return Err(self.error(
                                        format!("Missing closing {quote}quote"),
                                        break_position,
                                    ));
                                }
                                break;
                            }
                        }
                    }
                    if breaks == 1 {
                        units.push(u16::from(b' '));
                    } else {
                        for _ in 0..breaks - 1 {
                            units.push(u16::from(b'\n'));
                        }
                    }
                }
                '\\' if double_quoted => {
                    let escape_position = self.position();
                    self.advance();
                    let Some(next) = self.peek() else {
                        return Err(
                            self.error(format!("Missing closing {quote}quote"), self.position())
                        );
                    };
                    let push_char = |units: &mut Vec<u16>, c: char| {
                        let mut buffer = [0u16; 2];
                        units.extend_from_slice(c.encode_utf16(&mut buffer));
                    };
                    match next {
                        '\n' => {
                            self.advance();
                            while matches!(self.peek(), Some(' ') | Some('\t')) {
                                self.advance();
                            }
                            trailing_ws = 0;
                        }
                        '0' => {
                            self.advance();
                            units.push(0);
                            trailing_ws = 0;
                        }
                        'a' => {
                            self.advance();
                            units.push(0x07);
                            trailing_ws = 0;
                        }
                        'b' => {
                            self.advance();
                            units.push(0x08);
                            trailing_ws = 0;
                        }
                        't' | '\t' => {
                            self.advance();
                            units.push(u16::from(b'\t'));
                            trailing_ws = 0;
                        }
                        'n' => {
                            self.advance();
                            units.push(u16::from(b'\n'));
                            trailing_ws = 0;
                        }
                        'v' => {
                            self.advance();
                            units.push(0x0B);
                            trailing_ws = 0;
                        }
                        'f' => {
                            self.advance();
                            units.push(0x0C);
                            trailing_ws = 0;
                        }
                        'r' => {
                            self.advance();
                            units.push(u16::from(b'\r'));
                            trailing_ws = 0;
                        }
                        'e' => {
                            self.advance();
                            units.push(0x1B);
                            trailing_ws = 0;
                        }
                        ' ' => {
                            self.advance();
                            units.push(u16::from(b' '));
                            trailing_ws = 0;
                        }
                        '"' => {
                            self.advance();
                            units.push(u16::from(b'"'));
                            trailing_ws = 0;
                            if self.eof() {
                                // yaml 的 EOF quirk：源串以 `"` 结尾（哪怕它是转义引号）
                                // 即视为闭合，值里保留这个引号。
                                let text = String::from_utf16_lossy(&units);
                                self.last_scalar_text = Some(text.clone());
                                return Ok(text);
                            }
                        }
                        '/' => {
                            self.advance();
                            units.push(u16::from(b'/'));
                            trailing_ws = 0;
                        }
                        '\\' => {
                            self.advance();
                            units.push(u16::from(b'\\'));
                            trailing_ws = 0;
                        }
                        'N' => {
                            self.advance();
                            units.push(0x85);
                            trailing_ws = 0;
                        }
                        '_' => {
                            self.advance();
                            units.push(0xA0);
                            trailing_ws = 0;
                        }
                        'L' => {
                            self.advance();
                            units.push(0x2028);
                            trailing_ws = 0;
                        }
                        'P' => {
                            self.advance();
                            units.push(0x2029);
                            trailing_ws = 0;
                        }
                        'x' | 'u' | 'U' => {
                            self.advance();
                            let digits = match next {
                                'x' => 2,
                                'u' => 4,
                                _ => 8,
                            };
                            let mut raw = String::new();
                            let mut value: u32 = 0;
                            let mut all_hex = true;
                            for _ in 0..digits {
                                match self.peek() {
                                    Some(digit) if digit.is_ascii_hexdigit() => {
                                        raw.push(digit);
                                        value = value * 16 + digit.to_digit(16).unwrap_or(0);
                                        self.advance();
                                    }
                                    Some(other) => {
                                        raw.push(other);
                                        all_hex = false;
                                        self.advance();
                                    }
                                    None => {
                                        all_hex = false;
                                    }
                                }
                            }
                            if !all_hex || value > 0x10FFFF {
                                return Err(self.error(
                                    format!("Invalid escape sequence \\{next}{raw}"),
                                    escape_position,
                                ));
                            }
                            match char::from_u32(value) {
                                Some(c) => push_char(&mut units, c),
                                // 代理项码点：push 原始 code unit（成对出现时由
                                // `from_utf16_lossy` 组合，孤立时替换为 U+FFFD）。
                                None => units.push(value as u16),
                            }
                            trailing_ws = 0;
                        }
                        other => {
                            return Err(self.error(
                                format!("Invalid escape sequence \\{other}"),
                                escape_position,
                            ));
                        }
                    }
                }
                _ => {
                    self.advance();
                    if c == ' ' || c == '\t' {
                        trailing_ws += 1;
                    } else {
                        trailing_ws = 0;
                    }
                    let mut buffer = [0u16; 2];
                    units.extend_from_slice(c.encode_utf16(&mut buffer));
                }
            }
        }
    }

    // -- block scalars ------------------------------------------------------

    /// 解析 `|` / `>` 块标量（含 chomping `+`/`-` 与显式缩进指示符 `1..9`）。
    ///
    /// `parent_indent` 是所在块集合的缩进：显式指示符按 `parent_indent + N` 计算
    /// 内容缩进，隐式缩进取第一行内容的缩进。`at_root` 为 true 时内容允许从第 0 列
    /// 开始，且第 0 列的 `---`/`...` 会终止 scalar（文档根的 `|` 文档）。
    ///
    /// 内容先按行收集为 `(缩进, 文本)`，再由 `resolve_block_scalar` 统一裁剪/折叠
    /// （逐行复刻 yaml 的 `resolveBlockScalar`）。非 `+` chomping 下，扫描器会丢弃
    /// 缩进不足以成为内容的尾部空白行；`last_block_scalar_end` 记录 yaml token 模型
    /// 里的结束行，供外层映射的错误定位使用。
    fn parse_block_scalar(
        &mut self,
        parent_indent: usize,
        at_root: bool,
    ) -> Result<Value, FrontmatterError> {
        let header_line = self.line;
        let style = self
            .peek()
            .expect("caller checked the block scalar indicator");
        self.advance();

        let mut chomping: Option<char> = None;
        let mut explicit_indent: Option<usize> = None;
        let mut header = String::new();
        header.push(style);

        loop {
            match self.peek() {
                Some(c @ ('+' | '-')) if chomping.is_none() => {
                    chomping = Some(c);
                    header.push(c);
                    self.advance();
                }
                Some(c @ '1'..='9') if explicit_indent.is_none() => {
                    explicit_indent = Some(c as usize - '0' as usize);
                    header.push(c);
                    self.advance();
                }
                Some(' ') | Some('\t') | Some('\n') | None | Some('#') => break,
                Some(_) => {
                    let position = self.position();
                    while let Some(c) = self.peek() {
                        if matches!(c, ' ' | '\t' | '\n' | '#') {
                            break;
                        }
                        header.push(c);
                        self.advance();
                    }
                    return Err(self.error(
                        format!("Block scalar header includes extra characters: {header}"),
                        position,
                    ));
                }
            }
        }

        self.skip_inline_spaces();
        if self.peek() == Some('#') {
            self.skip_to_line_end();
        }
        if self.peek() == Some('\n') {
            self.advance();
        }

        let mut content_indent = explicit_indent.map(|indicator| parent_indent + indicator);
        // 原始行：前导空格数 + 去掉前导空格后的内容。真正的裁剪/折叠交给
        // `resolve_block_scalar`（逐行复刻 yaml 包的 resolveBlockScalar）。
        let mut lines: Vec<(usize, String)> = Vec::new();
        // 每行之后是否有真实换行（EOF 处的最后一行可能没有）。
        let mut line_breaks: Vec<bool> = Vec::new();
        // 隐式缩进确定前出现的空行里最深的缩进（yaml 包据此报错）。
        let mut leading_blank_indent = 0usize;
        // 第一行真实内容的缩进（yaml scanner 的 `indent`，用来丢弃尾部空白行）。
        let mut first_content_spaces: Option<usize> = None;

        loop {
            if self.eof() {
                break;
            }
            let mut spaces = 0usize;
            while self.peek() == Some(' ') {
                self.advance();
                spaces += 1;
            }
            if matches!(self.peek(), None | Some('\n')) {
                if content_indent.is_none() {
                    leading_blank_indent = leading_blank_indent.max(spaces);
                }
                let had_break = self.peek() == Some('\n');
                if had_break {
                    self.advance();
                }
                lines.push((spaces, String::new()));
                line_breaks.push(had_break);
                continue;
            }
            // 根节点的隐式 block scalar 内容缩进为 0 时，第 0 列的 `---`/`...` 结束 scalar。
            if at_root && spaces == 0 && self.at_block_document_marker() {
                break;
            }
            if self.peek() == Some('\t') {
                // tab 本身可以是内容的一部分（yaml 只按缩进判断），但缩进不足的 tab 行
                // 会被 yaml 并入 source 并报错（lexer 的 “insufficiently indented tabs”）。
                match content_indent {
                    None if !at_root && spaces <= parent_indent => {
                        let message = if parent_indent == 0 {
                            "Block scalar values in collections must be indented"
                        } else {
                            "Block scalars with more-indented leading empty lines must use an explicit indentation indicator"
                        };
                        return Err(self.error_here(message));
                    }
                    Some(ci) if spaces < ci => {
                        let message = if first_content_spaces.is_none() {
                            "Block scalars with more-indented leading empty lines must use an explicit indentation indicator"
                        } else {
                            "Block scalar lines must not be less indented than their first line"
                        };
                        return Err(self.error_here(message));
                    }
                    _ => {}
                }
            }
            let indent = spaces;
            match content_indent {
                None => {
                    // 块集合里的 block scalar：内容必须比所在块集合缩进更深；否则 scalar
                    // 在这里结束（外层再报 key 相关的错误）。根节点不受限制。
                    if !at_root && indent <= parent_indent {
                        break;
                    }
                    if explicit_indent.is_none() && leading_blank_indent > indent {
                        // yaml 包：隐式缩进时，前导空行不能比首行内容缩进更深。
                        return Err(self.error(
                            "Block scalars with more-indented leading empty lines must use an explicit indentation indicator",
                            self.position(),
                        ));
                    }
                    content_indent = Some(indent);
                }
                Some(ci) => {
                    if indent < ci {
                        break;
                    }
                }
            }
            if first_content_spaces.is_none() {
                first_content_spaces = Some(indent);
            }
            let mut content = String::new();
            while let Some(c) = self.peek() {
                if c == '\n' {
                    break;
                }
                content.push(c);
                self.advance();
            }
            let had_break = self.peek() == Some('\n');
            if had_break {
                self.advance();
            }
            lines.push((indent, content));
            line_breaks.push(had_break);
        }

        let trim_indent = match explicit_indent {
            Some(indicator) => parent_indent + indicator,
            None => first_content_spaces.unwrap_or(content_indent.unwrap_or(0)),
        };

        // yaml 的 scanner 在非 keep 模式下会丢弃“缩进不足以成为内容”的尾部空白行。
        let scanner_indent = first_content_spaces.unwrap_or(0);
        let mut source_len = lines.len();
        if chomping != Some('+') {
            while source_len > 0 {
                let (spaces, content) = &lines[source_len - 1];
                if content.is_empty() && *spaces <= scanner_indent {
                    source_len -= 1;
                } else {
                    break;
                }
            }
        }

        let final_newline = source_len > 0 && line_breaks[source_len - 1];
        let value = resolve_block_scalar(
            &lines[..source_len],
            style,
            chomping,
            trim_indent,
            final_newline,
        );

        // yaml token 模型里的结束行：source 含最后一行行尾换行（EOF 无换行则停在内容行）。
        let end_line = if source_len == 0 {
            header_line + 1
        } else if final_newline {
            header_line + 1 + source_len
        } else {
            header_line + source_len
        };
        self.last_block_scalar_end = Some(end_line);

        Ok(Value::String(value))
    }

    // -- flow collections ---------------------------------------------------

    /// 按 flow 是否位于块集合内，选择 yaml 的两种闭合错误文案
    /// （“must end with a ]” 与 “must be sufficiently indented and end with a ]”）。
    fn unclosed_flow_error(
        &self,
        context: FlowContext,
        kind: FlowKind,
        position: Position,
    ) -> FrontmatterError {
        let message = match (kind, context) {
            (FlowKind::Sequence, FlowContext::Root) => "Flow sequence must end with a ]",
            (FlowKind::Map, FlowContext::Root) => "Flow map must end with a }",
            (FlowKind::Sequence, FlowContext::Block { .. }) => {
                "Flow sequence in block collection must be sufficiently indented and end with a ]"
            }
            (FlowKind::Map, FlowContext::Block { .. }) => {
                "Flow map in block collection must be sufficiently indented and end with a }"
            }
        };
        self.error(message, position)
    }

    /// 跳过 flow 集合内的空白、注释与换行，并检查续行缩进。
    ///
    /// yaml 的 lexer 只用**空格**计算 flow 内容缩进（tab 不计入 `indentValue`）：
    /// 行首 tab 后的空行或注释行整体跳过；若后面还有内容，则按已消费的空格数判断
    /// 该行是否缩进足够。不满足 `FlowContext::allows_content` 时直接返回 unclosed
    /// flow 错误（最外层 flow 的闭合括号允许与块缩进齐平，由 `allows_closer` 决定）。
    fn skip_flow_whitespace(
        &mut self,
        context: FlowContext,
        kind: FlowKind,
    ) -> Result<(), FrontmatterError> {
        loop {
            match self.peek() {
                Some(' ') | Some('\t') => {
                    self.advance();
                }
                Some('#') => {
                    self.skip_to_line_end();
                }
                Some('\n') => {
                    self.advance();
                    let mut spaces = 0usize;
                    while self.peek() == Some(' ') {
                        self.advance();
                        spaces += 1;
                    }
                    // 行首的空格/tab 空白：yaml 只用**空格**计算 flow 内容的缩进
                    // （lexer 的 `indentValue`）；tab 之后若是空行/注释行则整个跳过，
                    // 若是内容则按空格数做缩进检查（`a: [1,\n  \t2]` 合法，
                    // `a: [1,\n\t2]` 报 unclosed flow）。
                    while matches!(self.peek(), Some(' ') | Some('\t')) {
                        self.advance();
                    }
                    match self.peek() {
                        None => return Ok(()),
                        Some('\n') => continue,
                        Some('#') => continue,
                        Some(character) => {
                            let closer = character == ']' || character == '}';
                            if !context.allows_content(spaces)
                                && !(closer && context.allows_closer(spaces))
                            {
                                let position = self.position();
                                return Err(self.unclosed_flow_error(context, kind, position));
                            }
                        }
                    }
                }
                _ => return Ok(()),
            }
        }
    }

    /// flow 节点：可带 `&anchor` / `!tag` 属性；`*alias` 解析为锚点值。
    fn parse_flow_node(
        &mut self,
        context: FlowContext,
        kind: FlowKind,
    ) -> Result<Value, FrontmatterError> {
        self.last_plain_key = None;
        self.skip_inline_spaces();
        let props = self.parse_properties()?;
        self.skip_inline_spaces();
        self.reject_alias_props(&props)?;
        let scalar_like = !matches!(self.peek(), Some('[') | Some('{'));
        let value = match self.peek() {
            Some('[') => self.parse_flow_sequence(context.nested())?,
            Some('{') => self.parse_flow_map(context.nested())?,
            Some('"') | Some('\'') => {
                Value::String(self.parse_quoted_scalar(context.indent_limit())?)
            }
            Some('*') => self.parse_alias()?,
            Some('-') if self.flow_block_indicator_ahead() => {
                // yaml 把 flow 里的 `- ` 当作 seq-item-ind，进而组成 block-seq →
                // resolveFlowCollection 报 BLOCK_IN_FLOW。
                let start = self.position();
                let mut width = 1usize;
                while matches!(self.peek_at(width), Some(' ') | Some('\t')) {
                    width += 1;
                }
                return Err(self.error_span(
                    "Block collections are not allowed within flow collections",
                    start,
                    (start.0, start.1 + width),
                ));
            }
            _ => {
                let start = self.position();
                let reserved = match self.peek() {
                    Some('@') => Some("Plain value cannot start with reserved character @"),
                    Some('`') => Some("Plain value cannot start with reserved character `"),
                    Some('%') => {
                        Some("Plain value cannot start with directive indicator character %")
                    }
                    Some('|') => Some("Plain value cannot start with block scalar indicator |"),
                    Some('>') => Some("Plain value cannot start with block scalar indicator >"),
                    _ => None,
                };
                if let Some(message) = reserved {
                    return Err(self.error(message, start));
                }
                let text = self.parse_flow_plain_scalar(context, kind)?;
                let (value, key_info) = resolve_plain_scalar_key(&text);
                self.last_scalar_text = Some(text);
                self.last_plain_key = Some((value.clone(), key_info));
                value
            }
        };
        // 嵌套集合的解析会覆盖这个标志，返回前恢复成当前节点自己的形态。
        self.last_node_scalar_like = scalar_like;
        self.apply_props(props, value)
    }

    /// `-` 是否是 flow 里的块序列指示符（后跟空白或 flow 指示符）。
    fn flow_block_indicator_ahead(&self) -> bool {
        matches!(
            self.peek_at(1),
            None | Some(' ')
                | Some('\t')
                | Some('\n')
                | Some(',')
                | Some('[')
                | Some(']')
                | Some('{')
                | Some('}')
        )
    }

    /// flow 键信息：plain scalar 保留解析时的值身份与 JS 文案（含 `Infinity`/`NaN`）。
    fn flow_key_info(&self, value: &Value) -> KeyInfo {
        if let Some((plain_value, info)) = &self.last_plain_key
            && plain_value == value
        {
            return info.clone();
        }
        key_info_from_value(value)
    }

    /// flow 集合里的显式键指示符 `?`（yaml 的 explicit-key-ind）：只表示后面是 key。
    /// 返回是否消费了 `?` —— flow 序列里它总是构成一个 pair（`[? a]` → `[{"a":null}]`）。
    ///
    /// yaml lexer 在 flow 里把后跟 flow 指示符（`,[]{}`）的 `?` 也当作 indicator
    /// （`[?]`、`[?, 1]`），块上下文则只认空白/EOL。
    fn consume_flow_explicit_key(&mut self) -> bool {
        if self.peek() == Some('?')
            && matches!(
                self.peek_at(1),
                None | Some(' ')
                    | Some('\t')
                    | Some('\n')
                    | Some(',')
                    | Some('[')
                    | Some(']')
                    | Some('{')
                    | Some('}')
            )
        {
            self.advance();
            self.skip_inline_spaces();
            return true;
        }
        false
    }

    /// 解析 flow 序列 `[a, b]`。
    ///
    /// 元素后跟 `:` 时变成单键映射（`[a: b]` → `[{"a": b}]`）；`? ` 显式 key 即使
    /// 没有 `:` 也产生 `{"key": null}`（`[? a]`）。缺少逗号时按元素形态区分文案：
    /// 标量元素报 “Missing , or : between flow sequence items”，集合元素留给
    /// node-end 错误。闭合失败按 `context` 选择 unclosed flow 文案。
    fn parse_flow_sequence(&mut self, context: FlowContext) -> Result<Value, FrontmatterError> {
        self.advance(); // '['
        let mut items: Vec<Value> = Vec::new();

        loop {
            self.skip_flow_whitespace(context, FlowKind::Sequence)?;
            if self.eof() {
                let position = self.position();
                return Err(self.unclosed_flow_error(context, FlowKind::Sequence, position));
            }
            match self.peek() {
                Some(']') => {
                    self.advance();
                    return Ok(Value::Array(items));
                }
                Some(',') => return Err(self.error_here("Unexpected , in flow sequence")),
                _ => {}
            }

            let explicit_key = self.consume_flow_explicit_key();
            let item = self.parse_flow_node(context, FlowKind::Sequence)?;
            let item_scalar_like = self.last_node_scalar_like;
            let item_key = self.flow_key_info(&item);
            self.skip_flow_whitespace(context, FlowKind::Sequence)?;
            let mut is_pair = false;

            if self.peek() == Some(':') {
                self.advance();
                self.skip_flow_whitespace(context, FlowKind::Sequence)?;
                let value_start = self.position();
                let value = match self.peek() {
                    Some(',') | Some(']') | Some('}') | None => Value::Null,
                    _ => self.parse_flow_node(context, FlowKind::Sequence)?,
                };
                if self.peek() == Some(':') {
                    return Err(self.error_span(
                        "Block collections are not allowed within flow collections",
                        value_start,
                        (value_start.0, value_start.1 + 1),
                    ));
                }
                let mut map = Map::new();
                map.insert(item_key.text, value);
                items.push(Value::Object(map));
                is_pair = true;
            } else if explicit_key {
                // `?` 显式 key 即使没有 `:` 也是 pair（值为 null）：
                // `[? a]` → `[{"a": null}]`，`[? ]` → `[{"": null}]`。
                let mut map = Map::new();
                map.insert(item_key.text, Value::Null);
                items.push(Value::Object(map));
                is_pair = true;
            } else {
                items.push(item);
            }

            self.skip_flow_whitespace(context, FlowKind::Sequence)?;
            match self.peek() {
                Some(',') => {
                    self.advance();
                }
                Some(']') => {
                    self.advance();
                    return Ok(Value::Array(items));
                }
                None => {
                    let position = self.position();
                    return Err(self.unclosed_flow_error(context, FlowKind::Sequence, position));
                }
                Some(_) => {
                    let position = self.position();
                    // 标量 item 后直接跟下一个 token（没有 `,`）：yaml 把这个 token 当作
                    // 该 item 的 value，报 “Missing , or : between flow sequence items”。
                    if item_scalar_like && !is_pair {
                        return Err(
                            self.error("Missing , or : between flow sequence items", position)
                        );
                    }
                    let (message, end) = self.unexpected_token_at(position, true);
                    return Err(self.error_span(message, position, end));
                }
            }
        }
    }

    /// 解析 flow 映射 `{a: 1}`。
    ///
    /// 键按 YAML 值身份查重（与块映射相同）；没有 `:` 时值为 null，但标量键后直接
    /// 出现下一个 token 会报 “Missing , or : between flow map items”。缩进检查与
    /// 闭合错误分别由 `skip_flow_whitespace` / `unclosed_flow_error` 负责。
    fn parse_flow_map(&mut self, context: FlowContext) -> Result<Value, FrontmatterError> {
        self.advance(); // '{'
        let mut map: Map<String, Value> = Map::new();
        let mut seen_keys: Vec<KeyId> = Vec::new();

        loop {
            self.skip_flow_whitespace(context, FlowKind::Map)?;
            if self.eof() {
                let position = self.position();
                return Err(self.unclosed_flow_error(context, FlowKind::Map, position));
            }
            match self.peek() {
                Some('}') => {
                    self.advance();
                    return Ok(Value::Object(js_order_object(map)));
                }
                Some(',') => return Err(self.error_here("Unexpected , in flow map")),
                _ => {}
            }

            self.consume_flow_explicit_key();
            let key_position = self.position();
            let key_value = self.parse_flow_node(context, FlowKind::Map)?;
            let key_scalar_like = self.last_node_scalar_like;
            let key = self.flow_key_info(&key_value);
            self.skip_flow_whitespace(context, FlowKind::Map)?;

            let value = if self.peek() == Some(':') {
                self.advance();
                self.skip_flow_whitespace(context, FlowKind::Map)?;
                let value_start = self.position();
                let value = match self.peek() {
                    Some(',') | Some('}') | Some(']') | None => Value::Null,
                    _ => self.parse_flow_node(context, FlowKind::Map)?,
                };
                if self.peek() == Some(':') {
                    return Err(self.error_span(
                        "Block collections are not allowed within flow collections",
                        value_start,
                        (value_start.0, value_start.1 + 1),
                    ));
                }
                value
            } else {
                // 标量 key 后直接跟下一个 token（没有 `:` 也没有 `,`）：yaml 把它当作
                // 该 item 的 value，报 MISSING_CHAR（flow 集合 key 则留给 node-end 错误）。
                if key_scalar_like && matches!(self.peek(), Some(c) if !matches!(c, ',' | '}')) {
                    let position = self.position();
                    return Err(self.error("Missing , or : between flow map items", position));
                }
                Value::Null
            };

            if seen_keys.contains(&key.id) {
                return Err(self.error("Map keys must be unique", key_position));
            }
            seen_keys.push(key.id);
            map.insert(key.text, value);

            self.skip_flow_whitespace(context, FlowKind::Map)?;
            match self.peek() {
                Some(',') => {
                    self.advance();
                }
                Some('}') => {
                    self.advance();
                    return Ok(Value::Object(js_order_object(map)));
                }
                None => {
                    let position = self.position();
                    return Err(self.unclosed_flow_error(context, FlowKind::Map, position));
                }
                Some(_) => {
                    let position = self.position();
                    let (message, end) = self.unexpected_token_at(position, true);
                    return Err(self.error_span(message, position, end));
                }
            }
        }
    }

    /// 解析 flow 上下文里的 plain scalar。
    ///
    /// 终止条件：`,` `[` `]` `{` `}`、后跟空白或 flow 指示符的 `:`、注释、缩进不足的
    /// 续行。跨行折叠与 block plain scalar 相同（1 行折成空格，n 行折成 n-1 个换行）；
    /// 续行行首若是闭合括号且缩进符合 `context.allows_closer`，scalar 直接结束。
    fn parse_flow_plain_scalar(
        &mut self,
        context: FlowContext,
        kind: FlowKind,
    ) -> Result<String, FrontmatterError> {
        let mut text = String::new();
        loop {
            let mut segment = String::new();
            let mut ended_by_comment = false;
            loop {
                match self.peek() {
                    None | Some('\n') => break,
                    Some(',') | Some(']') | Some('}') | Some('[') | Some('{') => break,
                    Some(':')
                        if matches!(
                            self.peek_at(1),
                            None | Some(' ')
                                | Some('\t')
                                | Some('\n')
                                | Some(',')
                                | Some(']')
                                | Some('}')
                                | Some('[')
                                | Some('{')
                        ) =>
                    {
                        break;
                    }
                    Some('#')
                        if segment.is_empty()
                            || matches!(self.chars.get(self.pos - 1), Some(' ') | Some('\t')) =>
                    {
                        ended_by_comment = true;
                        break;
                    }
                    Some(c) => {
                        segment.push(c);
                        self.advance();
                    }
                }
            }
            text.push_str(segment.trim_end_matches([' ', '\t']));

            if ended_by_comment || self.eof() {
                return Ok(text);
            }
            if self.peek() != Some('\n') {
                return Ok(text);
            }

            // 换行折叠：一行折成空格，n 行折成 n-1 个换行。续行若是 flow 指示符
            // （闭合括号/逗号等）则 scalar 直接结束，不追加折行空格。
            self.advance();
            let mut breaks = 1usize;
            loop {
                let mut spaces = 0usize;
                while self.peek() == Some(' ') {
                    self.advance();
                    spaces += 1;
                }
                // 行首 tab：yaml 的 continueScalar 只用空格数做缩进判定；
                // 空格数足够时 tab 属于缩进空白（`a: [x\n  \ty]` → "x y"），
                // 不够时 scalar 在这一行之前结束。
                let mut tabbed = false;
                if self.peek() == Some('\t') {
                    tabbed = true;
                    while matches!(self.peek(), Some(' ') | Some('\t')) {
                        self.advance();
                    }
                }
                match self.peek() {
                    None => return Ok(text),
                    Some('\n') => {
                        // 空行继续（yaml 的 continueScalar 对 `\n` 无条件继续）；
                        // tab 开头且缩进不足时 scalar 结束。
                        if tabbed && !context.allows_content(spaces) {
                            return Ok(text);
                        }
                        breaks += 1;
                        self.advance();
                    }
                    Some('#') => return Ok(text),
                    Some(character) => {
                        let closer = matches!(character, ']' | '}');
                        if closer && context.allows_closer(spaces) {
                            return Ok(text);
                        }
                        if matches!(character, ',' | ']' | '}' | '[' | '{') {
                            if !context.allows_content(spaces) {
                                let position = self.position();
                                return Err(self.unclosed_flow_error(context, kind, position));
                            }
                            return Ok(text);
                        }
                        if character == ':'
                            && matches!(
                                self.peek_at(1),
                                None | Some(' ')
                                    | Some('\t')
                                    | Some('\n')
                                    | Some(',')
                                    | Some(']')
                                    | Some('}')
                                    | Some('[')
                                    | Some('{')
                            )
                        {
                            if !context.allows_content(spaces) {
                                let position = self.position();
                                return Err(self.unclosed_flow_error(context, kind, position));
                            }
                            return Ok(text);
                        }
                        if !context.allows_content(spaces) {
                            let position = self.position();
                            return Err(self.unclosed_flow_error(context, kind, position));
                        }
                        break;
                    }
                }
            }
            if breaks == 1 {
                text.push(' ');
            } else {
                for _ in 0..breaks - 1 {
                    text.push('\n');
                }
            }
        }
    }
}

/// `>` 块标量的折叠规则（`one\n  indented\ntwo` → `one\n  indented\ntwo`）。
/// 逐行复刻 yaml 包的 `resolveBlockScalar`（值构造 + chomping）。
///
/// 参数：
/// - `lines`：`(原始缩进, 去掉缩进后的内容)`；调用方在非 `+` chomping 时已截断
///   缩进不足的尾部空白行（`+` 时保留它们）；
/// - `style`：`|` 保留换行，`>` 折叠；
/// - `chomping`：`-` 去掉结尾换行，`+` 保留全部尾部空行，`None` 在有内容时恰好保留
///   一个结尾换行（内容为空时仍返回空串）；
/// - `trim_indent`：每行内容要扣掉的缩进；
/// - `final_newline`：最后一行是否带真实换行（EOF 处可能没有）。
fn resolve_block_scalar(
    lines: &[(usize, String)],
    style: char,
    chomping: Option<char>,
    trim_indent: usize,
    final_newline: bool,
) -> String {
    let mut chomp_start = lines.len();
    while chomp_start > 0 && lines[chomp_start - 1].1.is_empty() {
        chomp_start -= 1;
    }

    if chomp_start == 0 {
        // yaml 的空内容 shortcut：`lines.length` 含行尾换行产生的空行。
        let length = lines.len() + usize::from(final_newline);
        return if chomping == Some('+') && length > 0 {
            "\n".repeat(length.saturating_sub(1).max(1))
        } else {
            String::new()
        };
    }

    let mut content_start = 0usize;
    while lines[content_start].1.is_empty() {
        content_start += 1;
    }

    // 尾部“更缩进”的空行并回内容（yaml 只检查紧邻内容的第一个尾部空行起）。
    for i in (chomp_start..lines.len()).rev() {
        if lines[i].0 > trim_indent {
            chomp_start = i + 1;
            break;
        }
    }

    let literal = style == '|';
    let mut value = String::new();
    let mut sep: &'static str = "";
    let mut prev_more_indented = false;

    for (spaces, _) in &lines[..content_start] {
        value.push_str(&" ".repeat(spaces.saturating_sub(trim_indent)));
        value.push('\n');
    }

    for (spaces, content) in &lines[content_start..chomp_start] {
        let extra = " ".repeat(spaces.saturating_sub(trim_indent));
        if literal {
            value.push_str(sep);
            value.push_str(&extra);
            value.push_str(content);
            sep = "\n";
        } else if *spaces > trim_indent || content.starts_with('\t') {
            if sep == " " {
                sep = "\n";
            } else if !prev_more_indented && sep == "\n" {
                sep = "\n\n";
            }
            value.push_str(sep);
            value.push_str(&extra);
            value.push_str(content);
            sep = "\n";
            prev_more_indented = true;
        } else if content.is_empty() {
            if sep == "\n" {
                value.push('\n');
            } else {
                sep = "\n";
            }
        } else {
            value.push_str(sep);
            value.push_str(content);
            sep = " ";
            prev_more_indented = false;
        }
    }

    match chomping {
        Some('-') => {}
        Some('+') => {
            for (spaces, _) in &lines[chomp_start..] {
                value.push('\n');
                value.push_str(&" ".repeat(spaces.saturating_sub(trim_indent)));
            }
            if final_newline {
                value.push('\n');
            }
            if !value.ends_with('\n') {
                value.push('\n');
            }
        }
        _ => value.push('\n'),
    }

    value
}

/// 逐字复刻 `yaml` 2.9.0 的 `prettifyError`（`errors.js`）。
///
/// - 行/列按 UTF-16 code unit（JS 字符串索引）计；
/// - 出错行长度超过 80 时按 yaml 的规则裁剪（保留列位置、首尾加 `…`）；
/// - 当插入符位于行首空白处且不是第 1 行时，把上一行一起放进上下文；
/// - 插入符长度取自 `end`（同一行且更靠右时为 `end.column - column`，否则 1）。
fn make_error(
    message: String,
    lines: &[&str],
    line: usize,
    column: usize,
    end: Option<Position>,
) -> FrontmatterError {
    let line_text = lines.get(line.saturating_sub(1)).copied().unwrap_or("");
    let mut line_units: Vec<u16> = line_text.encode_utf16().collect();
    while matches!(line_units.last(), Some(&unit) if unit == u16::from(b'\n') || unit == u16::from(b'\r'))
    {
        line_units.pop();
    }
    let mut ci = column.saturating_sub(1);
    if ci >= 60 && line_units.len() > 80 {
        let trim_start = std::cmp::min(ci - 39, line_units.len() - 79);
        let mut trimmed: Vec<u16> = Vec::with_capacity(line_units.len() - trim_start + 1);
        trimmed.push(0x2026); // '…'
        trimmed.extend_from_slice(&line_units[trim_start..]);
        line_units = trimmed;
        ci -= trim_start - 1;
    }
    if line_units.len() > 80 {
        line_units.truncate(79);
        line_units.push(0x2026); // '…'
    }
    if line > 1
        && line_units[..ci.min(line_units.len())]
            .iter()
            .all(|&unit| unit == u16::from(b' '))
    {
        let previous = lines.get(line - 2).copied().unwrap_or("");
        let mut previous_units: Vec<u16> = previous.encode_utf16().collect();
        previous_units.push(u16::from(b'\n'));
        if previous_units.len() > 80 {
            previous_units.truncate(79);
            previous_units.push(0x2026); // '…'
            previous_units.push(u16::from(b'\n'));
        }
        previous_units.extend_from_slice(&line_units);
        line_units = previous_units;
    }

    let mut full_message = format!("{message} at line {line}, column {column}");
    if line_units.iter().any(|&unit| unit != u16::from(b' ')) {
        let count = match end {
            Some((end_line, end_column)) if end_line == line && end_column > column => {
                std::cmp::max(1, std::cmp::min(end_column - column, 80 - ci))
            }
            _ => 1,
        };
        let pointer = format!("{}{}", " ".repeat(ci), "^".repeat(count));
        full_message.push_str(&format!(
            ":\n\n{}\n{}\n",
            String::from_utf16_lossy(&line_units),
            pointer
        ));
    }

    FrontmatterError {
        message: full_message,
        line,
        column,
        kind: ErrorKind::Positioned,
    }
}

#[cfg(test)]
mod golden_tests {
    use super::*;

    /// 由 `/home/ctyun/kiana-work/upstream-reference` 的 `yaml` 2.9.0 生成的黄金语料。
    #[test]
    fn matches_yaml_package_on_corpus() {
        let fixture: Value = serde_json::from_str(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/yaml_corpus.json"
        )))
        .expect("valid yaml corpus fixture");
        let cases = fixture.as_array().expect("fixture is an array");
        let mut failures: Vec<String> = Vec::new();

        for case in cases {
            let input = case["input"].as_str().expect("input string");
            if case["ok"].as_bool().expect("ok bool") {
                let expected = case["value"].clone();
                match parse_yaml(input) {
                    Ok(actual) => {
                        if actual != expected {
                            failures.push(format!(
                                "value mismatch for {input:?}: expected {expected}, got {actual}"
                            ));
                        }
                    }
                    Err(error) => failures.push(format!("unexpected error for {input:?}: {error}")),
                }
            } else {
                let expected_first_line = case["message"].as_str().expect("message string");
                match parse_yaml(input) {
                    Ok(value) => failures.push(format!(
                        "expected error for {input:?} (got {value}), expected {expected_first_line:?}"
                    )),
                    Err(error) => {
                        let first_line = error.message().lines().next().unwrap_or("");
                        if first_line != expected_first_line {
                            failures.push(format!(
                                "error mismatch for {input:?}: expected {expected_first_line:?}, got {first_line:?}"
                            ));
                        }
                    }
                }
            }
        }

        assert!(
            failures.is_empty(),
            "{} corpus case(s) failed:\n{}",
            failures.len(),
            failures.join("\n")
        );
    }
}
