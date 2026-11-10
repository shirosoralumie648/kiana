//! extensions/tool-search/tool.ts：工具发现（BM25 排名）与 `tool_search` 工具。
//!
//! `tool_search` 搜索没有声明给模型的工具（`codemode` / `deferred` exposure）并激活命中项，
//! 因此它们在下一次模型调用时被声明。激活走 active 工具集合，所以像其它工具变更一样记录在
//! transcript 里，并能在 `/tree`、resume、fork 后保留。
//!
//! # Rust 形态
//!
//! - [`ToolSearchTools`] 是 TS `Pick<ExtensionAPI, "getAllTools" | "getActiveTools" |
//!   "setActiveTools">` 的 `Send + Sync` 版本：`ToolDefinition` 要求执行器 `Send + Sync`，
//!   而 [`kiana_ext_api::types::ExtensionApi`] 没有这两个超 trait（它跑在单线程 `LocalSet` 上）。
//!   `create_tool_search_extension` 用一条本地通道把调用转交给持有 `ExtensionApi` 的同线程任务。
//! - [`create_tool_search_document`] 通过 [`ToolSearchDocumentSource`] 同时接受
//!   `AgentTool`（codemode 脚本的 `searchTools()`）与 `ToolInfo`（`tool_search`）。
//!
//! # 负责的边界
//!
//! 本文件不持有会话状态、不做 IO：输入是查询字符串与工具元数据，输出是排名结果和
//! `tool_search` 的 `ToolDefinition`。真正的工具集合由 [`ToolSearchTools`] 注入：生产实现
//! 见 `crate::tool_search::index`，测试用 `tests/tool_search.rs` 的 `FakeTools`。codemode 的
//! `searchTools()` 复用本文件的 [`Bm25Ranker`] 与 [`create_tool_search_document`]。
//!
//! # 一次 `tool_search` 调用的执行流程
//!
//! 1. `read_input`（私有）按 TS 的解构顺序校验 `query` / `limit`，错误文案逐字对齐；
//! 2. `search_and_load`（私有）读取 active 集合与全部工具，只保留 exposure 为 `codemode` /
//!    `deferred` 且尚未 active 的候选；
//! 3. 为每个候选构造 [`ToolSearchDocument`]，用默认参数的 [`Bm25Ranker`] 排名；
//! 4. 有命中时**一次**调用 `setActiveTools(active + 命中名)`（整份替换，顺序即分数顺序）；
//! 5. 把命中项渲染成模型可见文本，并把名字放进 `details.loaded`。
//!
//! # 重要约束
//!
//! - 参数校验与结果文案属于 TS 的可观察行为（`tests/tool_search.rs` 逐字断言），改动会破坏
//!   跨实现一致性。空串检查用的是 JS trim 后的结果，传给排名器的是**未 trim** 的原值。
//! - 排名是确定性的：同分保持输入文档顺序，而输入顺序来自 `getAllTools()` 的顺序。
//! - tokenizer 只把 ASCII 字母数字当词字符；停用词与单数化规则按 TS 逐字移植，改动会同时
//!   影响 codemode 的 `searchTools()`。

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, LazyLock};

use async_trait::async_trait;
use kiana_agent_core::{AgentTool, AgentToolError, AgentToolResult, AgentToolUpdateCallback};
use kiana_ext_api::tool_types::{
    ReachesToolsResult, ToolDefinition, ToolDefinitionExecute, ToolExposure, ToolNamespace,
};
use kiana_ext_api::types::ToolInfo;
use kiana_ext_api::{BoxFuture, ExtensionToolContextApi};
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use tokio_util::sync::CancellationToken;

/// `tool_search` 的工具名。
///
/// 它同时是注册名与模型调用名，也是同 crate 的 MCP 扩展判断“搜索工具是否存在”的依据
/// （`crate::mcp::index::ensure_discovery_active` 按这个名字查找带 `reaches_tools` 的工具）。
/// 改名会改变模型可见的工具名，并让上述按名匹配失效。
pub const TOOL_SEARCH_TOOL_NAME: &str = "tool_search";
/// `tool_search` 的默认结果数。
///
/// 类型是 `f64` 以对齐 TS 的 `number`。它只在 `limit` 缺省/null 时生效，显式传入的值必须是
/// 正整数（见 `read_input`）。schema 描述里的 “Defaults to 8.” 由该常量经
/// `kiana_json::js_num::format_js_number` 格式化，避免两处漂移。
pub const DEFAULT_TOOL_SEARCH_LIMIT: f64 = 8.0;

/// 排序器看到的一个工具：名字与 [`create_tool_search_document`] 生成的文本。
///
/// `text` 是已经拼接好的可搜索文本，`name` 保持原样（用于命中后回填 `setActiveTools`）。
/// 排名器只读这两个字段，不会改写它们。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolSearchDocument {
    /// 工具名（原样）。
    pub name: String,
    /// 可搜索文本。
    pub text: String,
}

/// 一条排名结果。
///
/// `score` 是 Okapi BM25 分数：越高越相关，只有 `> 0` 的文档会被返回；分数没有归一化，
/// 只在同一次 `rank` 调用内可比（idf 依赖本次 `documents` 集合）。
#[derive(Debug, Clone, PartialEq)]
pub struct ToolSearchMatch {
    /// 工具名（原样）。
    pub name: String,
    /// BM25 分数。
    pub score: f64,
}

/// `ToolRanker`：为查询排出工具。今天是 BM25，将来可以换成 embedding 混合排序。
///
/// # 实现契约
///
/// - 返回至多 `limit` 条。`limit` 是 TS 的 `number`（不是 `usize`），实现必须自行处理
///   小数与非正数；[`Bm25Ranker`] 会先截断小数、再把负数与 `NaN` 夹到 0。
/// - 必须按 `score` 降序；分数相同时保持 `documents` 的原始顺序（对应 TS 中稳定的
///   `Array#sort`）。
/// - 不得修改 `documents`；它按借用传入，调用方在排名后仍要用文档顺序回查元数据。
pub trait ToolRanker {
    /// 取前 `limit` 条匹配，分数降序；同分保持文档顺序。
    ///
    /// `limit <= 0`、空查询（分词后没有词）或空文档集合都返回空 `Vec`。分数为 `NaN` 的
    /// 文档不会出现在结果里（见 [`Bm25Ranker`]）。
    // `Vec` 已带 `#[must_use]`，这里无需再加消息（与工作区其它 trait 相同的局部例外）。
    #[allow(clippy::double_must_use)]
    fn rank(
        &self,
        query: &str,
        documents: &[ToolSearchDocument],
        limit: f64,
    ) -> Vec<ToolSearchMatch>;
}

/// `STOP_WORDS`（逐字 21 项；TS 数组长度就是 21）。
static STOP_WORDS: LazyLock<HashSet<&'static str>> = LazyLock::new(|| {
    [
        "a", "an", "and", "are", "as", "at", "be", "by", "for", "from", "in", "is", "it", "of",
        "on", "or", "that", "the", "this", "to", "with",
    ]
    .into_iter()
    .collect()
});

/// 朴素的单数形式，让 `issues` 命中 `issue`、`searches` 命中 `search`。
///
/// TS 的 `term.length` / `term.slice` 都按 UTF-16 code unit 计算（ARCHITECTURE §4.6），因此
/// 这里也按 code unit 判长与截断。被去掉的后缀都是 ASCII，截断点必然落在码点边界上。
///
/// # 规则顺序与边界
///
/// 三条规则依次短路：`ies`（长度 > 4）、`ches|shes|sses|xes|zes`（长度 > 4）、单个 `s`
/// 但不是 `ss`（长度 > 3）。因此 `boxes`→`box`、`classes`→`class`，而 `status`→`statu`
/// （TS 的既有行为，本文件测试锁定；spec §2.15.3 的示例写成 `status`→`status`，与 TS 源码
/// 不符）。不规则复数不处理：`tickets` 不会匹配 `issues`，这是 `tests/tool_search.rs` 的
/// `returns_nothing_for_unknown_or_empty_queries` 锁定的 v1 限制。
///
/// 长度阈值按 UTF-16 code unit 计：`😀s` 的长度是 3，不满足 `> 3`，因此保持原样
/// （`stems_by_utf16_code_units` 锁定）。
pub fn stem(term: &str) -> String {
    let units: Vec<u16> = term.encode_utf16().collect();
    let length = units.len();
    let ends_with = |suffix: &str| {
        let suffix: Vec<u16> = suffix.encode_utf16().collect();
        length >= suffix.len() && units[length - suffix.len()..] == suffix[..]
    };
    if length > 4 && ends_with("ies") {
        return format!("{}y", utf16_slice(&units, 0, length - 3));
    }
    if length > 4
        && (ends_with("ches")
            || ends_with("shes")
            || ends_with("sses")
            || ends_with("xes")
            || ends_with("zes"))
    {
        return utf16_slice(&units, 0, length - 2);
    }
    if length > 3 && ends_with("s") && !ends_with("ss") {
        return utf16_slice(&units, 0, length - 1);
    }
    term.to_string()
}

/// 把 UTF-16 code unit 片段转回 `String`（截断点总在码点边界上）。
fn utf16_slice(units: &[u16], start: usize, end: usize) -> String {
    String::from_utf16_lossy(&units[start..end])
}

/// 小写、在 camelCase 边界与非字母数字处切分、去掉停用词。
///
/// # 处理顺序
///
/// 1. 正则 `([a-z0-9])([A-Z])` 与 `([A-Z]+)([A-Z][a-z])` 先插入空格，拆开 `fooBar` 与
///    `HTTPServer`；
/// 2. `to_lowercase()`；
/// 3. 以“非 ASCII 字母数字”为界切分（连续分隔符产生的空片段被过滤）；
/// 4. 去掉 `STOP_WORDS` 中的停用词；
/// 5. 对每个词调用 [`stem`]。
///
/// 因为第 3 步只认 ASCII，非 ASCII 字母（如 `é`）会被当作分隔符。返回值不去重，顺序即
/// 文本中的出现顺序；BM25 用它计算词频，查询侧的去重在 [`ToolRanker::rank`] 的实现里做。
pub fn tokenize(text: &str) -> Vec<String> {
    static LOWER_UPPER: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"([a-z0-9])([A-Z])").expect("valid regex"));
    static ACRONYM: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"([A-Z]+)([A-Z][a-z])").expect("valid regex"));
    let spaced = LOWER_UPPER.replace_all(text, "$1 $2");
    let spaced = ACRONYM.replace_all(&spaced, "$1 $2");
    spaced
        .to_lowercase()
        .split(|character: char| !character.is_ascii_alphanumeric())
        .filter(|term| !term.is_empty() && !STOP_WORDS.contains(term))
        .map(stem)
        .collect()
}

/// `typeof value === "object" && value !== null && !Array.isArray(value)`。
fn is_object(value: &Value) -> bool {
    value.is_object()
}

/// schema 的描述与属性名，递归收集。
///
/// 只识别 `description`、`properties`（先压属性名，再递归属性值）、`items` 与
/// `anyOf`/`oneOf`/`allOf` 三类变体；`$defs`、`additionalProperties`、`title`、`enum` 等
/// 其它关键字不会贡献搜索文本。`properties` 按 `serde_json` Map 的键序遍历（工作区启用了
/// `preserve_order`），因此同一份输入每次生成相同文本；`items` 递归处理单个值，
/// `anyOf`/`oneOf`/`allOf` 的变体按数组顺序递归。
fn schema_text(schema: &Value, parts: &mut Vec<String>) {
    if !is_object(schema) {
        return;
    }
    if let Some(description) = schema.get("description").and_then(Value::as_str) {
        parts.push(description.to_string());
    }
    if let Some(Value::Object(properties)) = schema.get("properties") {
        for (name, property) in properties {
            parts.push(name.clone());
            schema_text(property, parts);
        }
    }
    if let Some(items) = schema.get("items") {
        schema_text(items, parts);
    }
    for key in ["anyOf", "oneOf", "allOf"] {
        if let Some(Value::Array(variants)) = schema.get(key) {
            for variant in variants {
                schema_text(variant, parts);
            }
        }
    }
}

/// [`create_tool_search_document`] 的数据来源。
///
/// TS 的类型是 `Pick<ToolInfo, "name" | "description" | "parameters">`；Rust 用 trait 让同一份
/// 实现同时服务 `AgentTool`（codemode 脚本）与 `ToolInfo`（`tool_search`）。
///
/// 三个方法都返回借用，调用方无需克隆即可构造文档；实现只需直接暴露对应字段（本文件的
/// 两个实现都是如此）。`search_parameters` 返回非 object 的 JSON 时，`schema_text` 会直接
/// 跳过，不会报错。
pub trait ToolSearchDocumentSource {
    /// 工具名。
    fn search_name(&self) -> &str;
    /// 工具描述。
    fn search_description(&self) -> &str;
    /// 参数 schema。
    fn search_parameters(&self) -> &Value;
}

impl ToolSearchDocumentSource for AgentTool {
    fn search_name(&self) -> &str {
        &self.name
    }

    fn search_description(&self) -> &str {
        &self.description
    }

    fn search_parameters(&self) -> &Value {
        &self.parameters
    }
}

impl ToolSearchDocumentSource for ToolInfo {
    fn search_name(&self) -> &str {
        &self.name
    }

    fn search_description(&self) -> &str {
        &self.description
    }

    fn search_parameters(&self) -> &Value {
        &self.parameters
    }
}

/// 一个工具的搜索文本：名字、把下划线换成空格的名字、描述、schema 描述与属性名，
/// 以及命名空间的名字/描述/instructions。
///
/// # 输入如何变成文本
///
/// 依次加入：原名、下划线换成空格的名字、描述、schema 的 `description` 与属性名、
/// namespace 的 `name`/`description`/`instructions`。每个片段用 JS 的 `trim` 判空后丢弃，
/// 其余用单个空格连接。名字出现两次，是为了让 `mcp__docs__search` 既能按原样命中，也能按
/// `mcp docs search` 命中。
///
/// 返回的 `name` 始终是原名。`text` 可能为空字符串（所有片段都是空白），这样的文档仍会进入
/// 排名，只是永远不命中。本函数同时被 codemode 的 `searchTools()` 使用，修改文本组成会
/// 同时改变两处的排名结果。
pub fn create_tool_search_document<T: ToolSearchDocumentSource + ?Sized>(
    tool: &T,
    namespace: Option<&ToolNamespace>,
) -> ToolSearchDocument {
    let name = tool.search_name();
    let mut parts = vec![
        name.to_string(),
        name.replace('_', " "),
        tool.search_description().to_string(),
    ];
    schema_text(tool.search_parameters(), &mut parts);
    if let Some(namespace) = namespace {
        parts.push(namespace.name.clone());
        parts.push(namespace.description.clone().unwrap_or_default());
        parts.push(namespace.instructions.clone().unwrap_or_default());
    }
    let text = parts
        .into_iter()
        .filter(|part| !kiana_json::js_string::js_trim(part).is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    ToolSearchDocument {
        name: name.to_string(),
        text,
    }
}

/// Okapi BM25（常用参数）。同分保持文档顺序。
///
/// 只保存两个参数：`k1` 控制词频饱和，`b` 控制文档长度归一化。默认值 `k1 = 1.2`、
/// `b = 0.75`；构造器不校验取值，非有限值会原样参与运算——产生的 `NaN` 分数会
/// 被“只返回 `score > 0`”的规则过滤掉，该文档因此不进入结果。
///
/// 分数只在同一次 [`ToolRanker::rank`] 调用内可比：idf 依赖本次 `documents` 集合，且没有
/// 归一化。
#[derive(Debug, Clone, Copy)]
pub struct Bm25Ranker {
    k1: f64,
    b: f64,
}

/// [`Bm25Ranker`] 的构造参数（TS `constructor(options: { k1?, b? } = {})`）。
///
/// 两个字段为 `None` 时分别回落到 1.2 / 0.75；没有其它校验。
#[derive(Debug, Clone, Copy, Default)]
pub struct Bm25RankerOptions {
    /// `k1` 参数，缺省 1.2。
    pub k1: Option<f64>,
    /// `b` 参数，缺省 0.75。
    pub b: Option<f64>,
}

impl Default for Bm25Ranker {
    fn default() -> Self {
        Bm25Ranker { k1: 1.2, b: 0.75 }
    }
}

impl Bm25Ranker {
    /// 默认参数（`k1 = 1.2`、`b = 0.75`）。
    pub fn new() -> Self {
        Self::default()
    }

    /// 显式参数。
    pub fn with_options(options: Bm25RankerOptions) -> Self {
        Bm25Ranker {
            k1: options.k1.unwrap_or(1.2),
            b: options.b.unwrap_or(0.75),
        }
    }
}

impl ToolRanker for Bm25Ranker {
    fn rank(
        &self,
        query: &str,
        documents: &[ToolSearchDocument],
        limit: f64,
    ) -> Vec<ToolSearchMatch> {
        // 查询词去重并保留首次出现顺序（TS 的 `[...new Set(tokenize(query))]`）。
        // 顺序决定后面浮点累加的顺序，从而保证同一输入每次得到相同结果。
        let mut seen = HashSet::new();
        let query_terms: Vec<String> = tokenize(query)
            .into_iter()
            .filter(|term| seen.insert(term.clone()))
            .collect();
        // `limit` 为 NaN 时 `<= 0.0` 为 false，会走到下面的 trunc（NaN 夹成 0）后返回空。
        if query_terms.is_empty() || documents.is_empty() || limit <= 0.0 {
            return Vec::new();
        }
        // 每篇文档一个词频表；文档长度是词频之和（含重复词），不是唯一词数。
        let term_counts: Vec<HashMap<String, usize>> = documents
            .iter()
            .map(|document| {
                let mut counts: HashMap<String, usize> = HashMap::new();
                for term in tokenize(&document.text) {
                    *counts.entry(term).or_insert(0) += 1;
                }
                counts
            })
            .collect();
        let lengths: Vec<usize> = term_counts
            .iter()
            .map(|counts| counts.values().copied().sum())
            .collect();
        let mut average_length = lengths.iter().sum::<usize>() as f64 / documents.len() as f64;
        // TS: `... || 1` —— 0 与 NaN 都是 falsy。documents 非空时总和有限，因此实际只需要
        // 处理“所有文档分词都为空”导致的 0；NaN 分支是为了与 TS 的同一表达式逐字对齐。
        if average_length == 0.0 || average_length.is_nan() {
            average_length = 1.0;
        }
        // 只对查询词计算 idf：df 是包含该词的文档数，+0.5 平滑避免除零。
        let idf: HashMap<String, f64> = query_terms
            .iter()
            .map(|term| {
                let frequency = term_counts
                    .iter()
                    .filter(|counts| counts.contains_key(term))
                    .count();
                let value = (1.0
                    + (documents.len() as f64 - frequency as f64 + 0.5) / (frequency as f64 + 0.5))
                    .ln();
                (term.clone(), value)
            })
            .collect();
        // 按文档顺序打分；`score > 0` 同时过滤掉“一个查询词都没命中”和 NaN 分数。
        let mut matches = Vec::new();
        for (index, document) in documents.iter().enumerate() {
            let mut score = 0.0;
            for term in &query_terms {
                let Some(count) = term_counts[index].get(term) else {
                    continue;
                };
                let count = *count as f64;
                let norm =
                    self.k1 * (1.0 - self.b + self.b * lengths[index] as f64 / average_length);
                score += idf.get(term).copied().unwrap_or(0.0) * (count * (self.k1 + 1.0))
                    / (count + norm);
            }
            if score > 0.0 {
                matches.push(ToolSearchMatch {
                    name: document.name.clone(),
                    score,
                });
            }
        }
        // `sort_by` 稳定，因此同分保持上述文档顺序（TS 的 `Array#sort` 同样稳定）。
        // NaN 分数已经在上一步被过滤；这里的 partial_cmp 只兜底相等比较。
        matches.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        // TS `slice(0, limit)`：先 Math.trunc 再夹到非负。Rust 的 float→usize 转换会饱和，
        // 因此超大 limit 等价于 usize::MAX，最终仍被 matches.len() 限制。
        let limit = limit.trunc().max(0.0) as usize;
        matches.truncate(limit);
        matches
    }
}

/// `tool_search` 的参数 schema（TypeBox 的 JSON 形态）。
///
/// 形状与 TypeBox 的输出逐字对齐：顶层键序是 `type`、`required`、`properties`，属性顺序是
/// `query`、`limit`，`required` 只含 `query`。`limit` 的描述由
/// [`DEFAULT_TOOL_SEARCH_LIMIT`] 格式化而来。键序被本文件的
/// `schema_matches_the_typebox_output_order` 测试锁定（工作区 serde_json 启用了
/// `preserve_order`），因此插入顺序是可观察行为，不能随意调整。
pub fn tool_search_schema() -> Value {
    let mut code = Map::new();
    code.insert("type".to_string(), Value::String("string".to_string()));
    code.insert(
        "description".to_string(),
        Value::String("Search query for deferred tools.".to_string()),
    );
    let mut limit = Map::new();
    limit.insert("type".to_string(), Value::String("number".to_string()));
    limit.insert(
        "description".to_string(),
        Value::String(format!(
            "Maximum number of tools to return. Defaults to {}.",
            kiana_json::js_num::format_js_number(DEFAULT_TOOL_SEARCH_LIMIT)
        )),
    );
    let mut properties = Map::new();
    properties.insert("query".to_string(), Value::Object(code));
    properties.insert("limit".to_string(), Value::Object(limit));
    let mut schema = Map::new();
    schema.insert("type".to_string(), Value::String("object".to_string()));
    schema.insert(
        "required".to_string(),
        Value::Array(vec![Value::String("query".to_string())]),
    );
    schema.insert("properties".to_string(), Value::Object(properties));
    Value::Object(schema)
}

/// `tool_search` 的结果。
///
/// 字段名经 `rename_all = "camelCase"`（当前两个字段都是单词，序列化名不变）。
/// `description` 是完整的原始描述，不是渲染用的一行摘要；摘要只在
/// `ToolSearchExecutable::execute` 拼文本时用 `first_line` 生成。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolSearchResultTool {
    /// 工具名。
    pub name: String,
    /// 工具描述。
    pub description: String,
}

/// `tool_search` 的 details。
///
/// 序列化进 `AgentToolResult.details` 的形状是 `{"loaded": [...]}`；`loaded` 的顺序与排名
/// 结果一致，没有命中时为空数组。字段上的 `serde(default)` 让缺失该键的旧数据也能读回。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolSearchToolDetails {
    /// 本次调用加载的工具。
    #[serde(default)]
    pub loaded: Vec<String>,
}

/// 会话工具视图：TS `Pick<ExtensionAPI, "getAllTools" | "getActiveTools" | "setActiveTools">`。
///
/// 三个方法都是异步的，因为实现通常把调用转交给单线程 `LocalSet` 上的扩展 API。
///
/// # 实现契约
///
/// - `get_all_tools` / `get_active_tools` 返回**当前**快照；两次调用之间状态可能变化，
///   实现不保证跨调用原子性。
/// - `set_active_tools` 接收的是**完整的** active 列表，不是增量。`kiana-session` 的
///   `ExtensionApi::set_active_tools` 实现是整体替换：传入列表即新的 active 集合，未知与
///   `hidden` 名字被忽略；`search_and_load` 因此先把读到的 active 拼在前面再整体写回。
/// - 没有错误通道：实现失败时只能退化为空列表或忽略写入（生产实现见
///   `crate::tool_search::index`），调用方无法区分“没有工具”与“查询失败”。
#[async_trait]
#[allow(clippy::double_must_use)] // async_trait 生成的 future 已带 `#[must_use]`，返回值也是。
pub trait ToolSearchTools: Send + Sync {
    /// 全部已配置工具（含 exposure 与 namespace）。
    async fn get_all_tools(&self) -> Vec<ToolInfo>;
    /// 当前 active（声明给模型）的工具名。
    async fn get_active_tools(&self) -> Vec<String>;
    /// 按名设置 active 工具。
    async fn set_active_tools(&self, tool_names: Vec<String>);
}

/// `createToolSearchToolDefinition` 的选项。
///
/// TS 的 `tools` 是扩展 API 的三个同步方法；Rust 用 [`ToolSearchTools`]（异步）表达，
/// 语义相同。
///
/// `tools` 为 `None` 时，参数合法的执行仍会成功，但固定返回 “No matching tools found.”
/// （见 `tests/tool_search.rs`）。手写的 `Debug` 只打印是否提供，避免要求 trait 对象实现
/// `Debug`。
#[derive(Clone, Default)]
pub struct ToolSearchToolOptions {
    /// 会话的工具视图；缺省时找不到任何工具。
    pub tools: Option<Arc<dyn ToolSearchTools>>,
}

impl std::fmt::Debug for ToolSearchToolOptions {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ToolSearchToolOptions")
            .field("tools", &self.tools.is_some())
            .finish()
    }
}

/// `tool_search` 能否加载该 exposure 的工具。
///
/// 只有 `codemode` 与 `deferred` 属于“已注册但默认不声明给模型、可经搜索激活”的发现通道；
/// `direct` / `model-only` 由会话按普通激活规则声明，`hidden` 即使激活也不可达。因此这里
/// 不看 active 集合，active 过滤由调用方 `search_and_load` 完成。
fn is_searchable(exposure: ToolExposure) -> bool {
    matches!(exposure, ToolExposure::Codemode | ToolExposure::Deferred)
}

/// 排名尚未 active 的可搜索工具并激活命中项，供下一次模型调用声明。
///
/// # 流程
///
/// 1. 读一次 active 快照；
/// 2. 读全部工具，过滤出 `is_searchable` 且名字不在 active 中的候选；
/// 3. 为候选构造搜索文档（namespace 取 `ToolInfo.namespace`）；
/// 4. 用默认参数的 [`Bm25Ranker`] 排名，`limit` 原样传入；
/// 5. 有命中时调用 `set_active_tools(active + 命中名)` 一次写入，顺序即分数顺序。
///
/// # 约束与边界
///
/// - 没有命中时完全不调用 `set_active_tools`；有命中时也只写一次
///   （`tests/tool_search.rs` 的 `validates_tool_search_arguments_and_loads_matches` 断言
///   调用次数与列表内容）。
/// - 返回项的 `description` 从候选里按名字回查，是**完整描述**；回查不到时为空串。
/// - 读 active 与写 active 不是原子操作：期间其它来源的激活会被这份快照覆盖。
/// - 不校验 `set_active_tools` 是否真的接受了名字；若实现忽略了某个名字（未知或 `hidden`），
///   返回文本仍会声称它已加载。
async fn search_and_load(
    tools: &dyn ToolSearchTools,
    query: &str,
    limit: f64,
) -> Vec<ToolSearchResultTool> {
    let active = tools.get_active_tools().await;
    let candidates: Vec<ToolInfo> = tools
        .get_all_tools()
        .await
        .into_iter()
        .filter(|tool| is_searchable(tool.exposure) && !active.contains(&tool.name))
        .collect();
    let documents: Vec<ToolSearchDocument> = candidates
        .iter()
        .map(|tool| create_tool_search_document(tool, tool.namespace.as_ref()))
        .collect();
    let matches = Bm25Ranker::new().rank(query, &documents, limit);
    if !matches.is_empty() {
        let mut next = active;
        next.extend(matches.iter().map(|matched| matched.name.clone()));
        tools.set_active_tools(next).await;
    }
    matches
        .into_iter()
        .map(|matched| ToolSearchResultTool {
            description: candidates
                .iter()
                .find(|tool| tool.name == matched.name)
                .map_or_else(String::new, |tool| tool.description.clone()),
            name: matched.name,
        })
        .collect()
}

/// `tool_search` 的描述。它不列出可搜索工具或命名空间，因此工具注册（例如 MCP 服务器连接）
/// 时描述保持不变。
///
/// 文案是当前稳定契约（来源见 spec §2.15.11）；它直接进入模型可见的
/// 工具声明，修改会改变模型对何时使用本工具的判断。
pub const TOOL_SEARCH_DESCRIPTION: &str = "# Tool discovery\n\nSearches over deferred tool metadata with BM25 and exposes matching tools for the next model call.\n\nSome of the tools, such as tools of MCP servers, may not have been provided to you upfront, and you should use this tool (`tool_search`) to search for the required tools. For MCP tool discovery, always use `tool_search`.";

/// 描述第一行（`description.trim().split(/\r?\n/)[0]`）。
///
/// 先用 JS 的 `trim` 去掉首尾空白（不只是 ASCII 空格），再按 `\n` 取第一段并剥掉末尾的
/// `\r`，与正则 `/\r?\n/` 的切分一致。只保留第一行；后续行丢弃，行内空白不做额外规整。
fn first_line(description: &str) -> &str {
    let trimmed = kiana_json::js_string::js_trim(description);
    let first = trimmed.split('\n').next().unwrap_or("");
    first.strip_suffix('\r').unwrap_or(first)
}

/// 参数读取结果：已按 TS 的解构/校验顺序检查过。
struct ToolSearchInput {
    /// 原始 query（未 trim；空串检查用的是 JS trim 语义）。
    query: String,
    /// 已校验的正整数 limit，保持 TS 的 `number` 类型。
    limit: f64,
}

/// 把一次参数错误包成 [`AgentToolError`]（TS 的 `new Error(message)`）。
fn input_error(message: impl Into<String>) -> AgentToolError {
    AgentToolError {
        message: message.into(),
        name: None,
        stack: None,
    }
}

/// TS 运行时抛出的 `TypeError`（解构 `null`、读取 `undefined`/`null` 的属性、调用非函数）。
fn type_error(message: impl Into<String>) -> AgentToolError {
    AgentToolError {
        message: message.into(),
        name: Some("TypeError".to_string()),
        stack: None,
    }
}

/// 重放 TS 的 `async execute(_toolCallId, { query, limit })`：解构 → `query.trim() === ""` →
/// `limit ?? DEFAULT_TOOL_SEARCH_LIMIT` → `Number.isInteger(max) && max > 0`。
///
/// 参数来自模型时 schema 校验已保证类型；但 `ToolDefinitionExecute::execute` 也可能被直接
/// 调用，因此这里与 TS 一样对任意 JSON 值给出相同结果（错误文案逐字）。
///
/// 通过校验后返回**原始** `query`（未 trim）：`tokenize` 会忽略空白，所以排名结果与 trim
/// 后相同；空串检查则使用 JS `trim` 语义，只含空白的查询会被拒绝。校验顺序固定为
/// query 先于 limit（`tests/tool_search.rs` 锁定“空 query + 非法 limit”报 query 错误）。
fn read_input(params: &Value) -> Result<ToolSearchInput, AgentToolError> {
    if params.is_null() {
        // 函数参数解构 `async execute(_toolCallId, { query, limit })` 传 null 时的 V8 文案。
        return Err(type_error(
            "Cannot destructure property 'query' of 'object null' as it is null.",
        ));
    }
    // 原始值会被 ToObject 装箱，`query` 仍是 undefined；`Value::get` 对非对象也因此返回
    // None（数组同理：JSON 数组不可能带自定义的 `query` 属性）。
    let query = match params.get("query") {
        None => {
            return Err(type_error(
                "Cannot read properties of undefined (reading 'trim')",
            ));
        }
        Some(Value::String(query)) => query.clone(),
        Some(Value::Null) => {
            return Err(type_error(
                "Cannot read properties of null (reading 'trim')",
            ));
        }
        Some(_) => return Err(type_error("query.trim is not a function")),
    };
    if kiana_json::js_string::js_trim(&query).is_empty() {
        return Err(input_error("query must not be empty"));
    }
    // `??`：缺省与 null 都回落到默认值；其它非 number 值走 `Number.isInteger` 的失败分支。
    let max = match params.get("limit") {
        None | Some(Value::Null) => DEFAULT_TOOL_SEARCH_LIMIT,
        Some(value) => value
            .as_f64()
            .ok_or_else(|| input_error("limit must be a positive integer"))?,
    };
    if !kiana_json::is_js_integer(max) || max <= 0.0 {
        return Err(input_error("limit must be a positive integer"));
    }
    Ok(ToolSearchInput { query, limit: max })
}

/// `createToolSearchToolDefinition(options = {})`。
///
/// # 关键字段
///
/// - `exposure = model-only`：active 时声明给模型，但不会进入 codemode/嵌套调用的可调用
///   集合（`kiana-session` 的 `get_callable_tools_for` 只收 `codemode`/`deferred` 与 active
///   的 `direct`）；
/// - `reaches_tools = All`：查询可能命中任意尚未声明的工具，MCP 扩展据此判断可达性并激活
///   本工具（`crate::mcp::index::ensure_discovery_active`）；
/// - `default_active = None`：本函数不指定注册后的激活状态。会话的判定谓词是
///   `is_declarable && default_active.unwrap_or(true)`（`kiana-session` 的
///   `is_activated_on_registration`），因此 model-only 在未显式关闭时默认算激活；内置扩展
///   工厂会把它覆盖成 `Some(false)`（见 `crate::tool_search::index`）；
/// - `namespace` 与各 `render_*` / `prepare_*` 字段都是 `None`：搜索没有分组，也不自定义
///   调用/结果的渲染与参数预处理。
///
/// `parameters` 由 [`tool_search_schema`] 生成，必须是 object schema——`kiana-extensions`
/// 注册时会校验，否则拒绝注册。
pub fn create_tool_search_tool_definition(options: ToolSearchToolOptions) -> ToolDefinition {
    ToolDefinition {
        name: TOOL_SEARCH_TOOL_NAME.to_string(),
        label: TOOL_SEARCH_TOOL_NAME.to_string(),
        description: TOOL_SEARCH_DESCRIPTION.to_string(),
        prompt_snippet: Some(
            "Search for tools that are not loaded yet and load the matches".to_string(),
        ),
        prompt_guidelines: None,
        parameters: tool_search_schema(),
        constrained_sampling: None,
        render_shell: None,
        prepare_arguments: None,
        output_schema: None,
        // 搜索不是脚本需要的能力；它改变模型看到的内容。
        exposure: Some(ToolExposure::ModelOnly),
        namespace: None,
        // 查询可以命中任何尚未声明的工具。
        reaches_tools: Some(Arc::new(|_input: &Value| ReachesToolsResult::All)),
        annotations: None,
        default_active: None,
        prepare_loadout: None,
        execution_mode: None,
        execute: Arc::new(ToolSearchExecutable { options }),
        render_call: None,
        render_result: None,
    }
}

/// `tool_search` 的执行器。
///
/// 每次执行先克隆 [`ToolSearchToolOptions`]（只增加 `Arc` 引用计数），让返回的 future 持有
/// 所需数据。`_signal` / `_on_update` / `_ctx` 都不使用：搜索是短时的内存查询，没有增量
/// 更新，也不读工具上下文；因此取消信号**不会**中断正在进行的搜索。
struct ToolSearchExecutable {
    options: ToolSearchToolOptions,
}

impl ToolDefinitionExecute for ToolSearchExecutable {
    fn execute<'a>(
        &'a self,
        _tool_call_id: &'a str,
        params: Value,
        _signal: Option<&'a CancellationToken>,
        _on_update: Option<AgentToolUpdateCallback>,
        _ctx: Arc<dyn ExtensionToolContextApi>,
    ) -> BoxFuture<'a, Result<AgentToolResult<Value>, AgentToolError>> {
        let options = self.options.clone();
        Box::pin(async move {
            // 校验失败直接以 AgentToolError 结束；agent loop 负责把它转成错误结果。
            let input = read_input(&params)?;
            let max = input.limit;
            // 未注入工具视图时视作空集合（TS: `options.tools ? searchAndLoad(...) : []`）。
            let tools = match &options.tools {
                Some(tools) => search_and_load(tools.as_ref(), &input.query, max).await,
                None => Vec::new(),
            };
            // 文本形状是 TS 的可观察输出：空结果固定一句；非空时拼数量与复数，再为每个
            // 工具写一行 “- name: 描述首行”。
            let text = if tools.is_empty() {
                "No matching tools found.".to_string()
            } else {
                let suffix = if tools.len() == 1 { "" } else { "s" };
                let lines: Vec<String> = tools
                    .iter()
                    .map(|tool| format!("- {}: {}", tool.name, first_line(&tool.description)))
                    .collect();
                format!(
                    "Loaded {} tool{suffix}. They are available from your next call:\n{}",
                    tools.len(),
                    lines.join("\n")
                )
            };
            let loaded: Vec<String> = tools.into_iter().map(|tool| tool.name).collect();
            // details 只有 loaded 一个字段；序列化理论上不会失败，兜底成 null。
            let details =
                serde_json::to_value(ToolSearchToolDetails { loaded }).unwrap_or(Value::Null);
            Ok(AgentToolResult {
                content: vec![kiana_ai::types::UserContentBlock::Text(
                    kiana_ai::types::TextContent {
                        kind_text: kiana_ai::types::TextKind::Text,
                        text,
                        text_signature: None,
                    },
                )],
                details,
                structured_content: None,
                usage: None,
                is_error: None,
                terminate: None,
            })
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    struct NoopExecute;

    impl kiana_agent_core::AgentToolExecute for NoopExecute {
        fn execute<'a>(
            &'a self,
            _tool_call_id: &'a str,
            _params: Value,
            _signal: Option<&CancellationToken>,
            _on_update: Option<AgentToolUpdateCallback>,
        ) -> kiana_ext_api::BoxFuture<'a, Result<AgentToolResult<Value>, AgentToolError>> {
            Box::pin(async {
                Ok(AgentToolResult {
                    content: Vec::new(),
                    details: Value::Null,
                    structured_content: None,
                    usage: None,
                    is_error: None,
                    terminate: None,
                })
            })
        }
    }

    #[test]
    fn stems_naively() {
        assert_eq!(stem("issues"), "issue");
        assert_eq!(stem("searches"), "search");
        assert_eq!(stem("boxes"), "box");
        assert_eq!(stem("classes"), "class");
        // TS 源码（权威）把 "status" 折成 "statu"：`endsWith("ss")` 不成立。
        // spec §2.15.3 的示例写成 "status"→"status"，与 TS 不符。
        assert_eq!(stem("status"), "statu");
    }

    #[test]
    fn stems_by_utf16_code_units() {
        // 由 Node 26 的 TS `stem` 生成（`String#length` / `slice` 按 UTF-16 code unit）。
        // 字节长度会跨过 >4/>3 阈值并在错误的偏移处截断。
        assert_eq!(stem("ïes"), "ïes");
        assert_eq!(stem("aés"), "aés");
        assert_eq!(stem("ßies"), "ßie");
        assert_eq!(stem("aïes"), "aïe");
        assert_eq!(stem("😀s"), "😀s");
        assert_eq!(stem("a😀s"), "a😀");
        assert_eq!(stem("😀ies"), "😀y");
    }

    #[test]
    fn tokenizes_camel_and_snake_case() {
        assert_eq!(
            tokenize("listIssues for the GitHub_repo"),
            vec!["list", "issue", "git", "hub", "repo"]
        );
        assert_eq!(
            tokenize("searches queries HTTPServer"),
            vec!["search", "query", "http", "server"]
        );
    }

    #[test]
    fn schema_matches_the_typebox_output_order() {
        assert_eq!(
            kiana_json::js_object::stringify(&tool_search_schema()),
            "{\"type\":\"object\",\"required\":[\"query\"],\"properties\":{\"query\":{\"type\":\"string\",\"description\":\"Search query for deferred tools.\"},\"limit\":{\"type\":\"number\",\"description\":\"Maximum number of tools to return. Defaults to 8.\"}}}"
        );
    }

    #[test]
    fn document_includes_name_variants_description_and_schema() {
        let tool = kiana_agent_core::AgentTool {
            name: "mcp__docs__search".to_string(),
            label: "search".to_string(),
            description: "Search the docs.".to_string(),
            parameters: json!({
                "type": "object",
                "properties": {"query": {"type": "string", "description": "What to search"}},
                "items": {"description": "item description"},
                "anyOf": [{"description": "variant"}]
            }),
            constrained_sampling: None,
            output_schema: None,
            prepare_arguments: None,
            execution_mode: None,
            replay: None,
            execute: std::sync::Arc::new(NoopExecute),
        };
        let document = create_tool_search_document(&tool, None);
        assert_eq!(document.name, "mcp__docs__search");
        assert_eq!(
            document.text,
            "mcp__docs__search mcp  docs  search Search the docs. query What to search item description variant"
        );
    }
}
