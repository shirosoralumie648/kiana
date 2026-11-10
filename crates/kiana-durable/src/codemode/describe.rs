//! codemode/describe.ts：模型读到的 code mode 文档。
//!
//! 工具那句固定描述（脚本环境与全局函数），以及 `codemode` 提示段：它把会话可调用的工具
//! 按命名空间分组、以 TypeScript 声明呈现，并限制在 token 预算内。放不下的工具用
//! `searchTools()` 找。提示段随可调用工具变化；工具描述永远不变。
//!
//! # 为什么分成两段文本
//! - 工具的固定描述（[`codemode_description`]）只讲脚本环境与全局函数，不列工具，因此
//!   每个会话都相同；它被用作 `codemode` 工具自身的 `description`。
//! - 系统提示里的 `codemode` 段（[`codemode_section`]）随传入的工具集变化：同一个会话里
//!   新增或删除可见（非内部、非 deferred）工具，或把可见工具改为 deferred，都会改变这段
//!   文本。`codemode.rs` 的渲染闭包会先滤掉内部工具与 deferred 工具，所以只新增一个
//!   deferred 或内部工具时，这段文本逐字不变。
//!
//! 这样“需要稳定的部分”与“必须变化的部分”分开，模型看到的工具描述不会因为工具清单
//! 改变而漂移（函数签名只接收 `models` 开关，拿不到工具列表）；测试
//! `keeps_the_tool_description_the_same_for_every_conversation` 锁定该段在 `models`
//! 关闭时不包含 `models` 说明、打开时包含。
//!
//! # 调用方与数据流
//! - `codemode.rs::create_codemode_tool()` 用 [`codemode_description`] 生成工具描述。
//! - `codemode.rs::create_codemode_extension()` 的提示段渲染闭包把 `agent.callable`
//!   （已排除内部工具与 deferred 工具）连同命名空间传入 [`codemode_section`]。
//! - `codemode.rs::execute()` 构造 start 数据时用 [`sample_of`] 冻结每个工具的声明文本，
//!   并用 [`namespace_of`] 把工具分到命名空间。
//! - [`declaration_of`] 只被 [`sample_of`] 使用，把 `ToolRegistration` 归一成声明结构。
//!
//! # 预算算法
//! 1. 每个工具的段 = 标题 + 描述 + `render_tool_sample()` 的 TypeScript 声明；成本按
//!    `ceil(UTF-16 码元数 / 4)` 估算（`CHARS_PER_TOKEN = 4`）。
//! 2. 分组：没有命中任何命名空间前缀的工具一组，每个命中前缀的命名空间一组；组按工具
//!    首次出现的顺序创建。
//! 3. 组序：无命名空间组最先，其余按 `locale_compare`（`en-US` 的 `localeCompare`
//!    等价物）升序。
//! 4. 组内按成本升序；每轮从每个仍活跃的组取一个最便宜的工具，放不下就让该组永久退出
//!    候选（剩余预算只减不增）。效果是“先让每个命名空间都有代表，再谈完整”。
//! 5. 渲染时组内仍按工具注册顺序输出（不是成本顺序）；组头会说明
//!    `(some tools not listed)` / `(tools not listed)`，无命名空间组被截断时给出
//!    `searchTools()` 提示。
//!
//! # 约束与失败模式
//! - 预算单位是估算 token，不是精确 tokenizer；它只影响提示段大小，不影响工具能否执行。
//! - [`codemode_section`] 以工具名为去重键（`HashSet<String>`）：若 registry 里出现同名
//!   工具，它们共享“是否展示”的判断；本文件不做唯一性检查。
//! - 本文件不返回错误；工具列表为空时 [`codemode_section`] 返回 `None`。工具声明渲染
//!   （`render_tool_sample`）在 schema 的 `$ref` 含畸形百分号转义时会 panic，本文件不
//!   捕获该 panic（见 kiana-codemode 的 `declarations` 文档）。
//! - 输出只依赖输入（`locale_compare` 是确定性的），同样的工具与命名空间产生同样的字节。

use kiana_ai::model_catalog::locale_compare;
use kiana_chord::delta::index::utf16_len;
use kiana_codemode::declarations::{render_tool_sample, to_codemode_identifier};
use kiana_codemode::types::CodemodeTool;
use kiana_json::js_string::js_trim;
use serde_json::{Value as JsonValue, json};

use crate::codemode::models::MODELS_DESCRIPTION;
use crate::harness::types::ToolRegistration;

/// 以公共前缀命名的一组工具，例如 MCP 服务器的 `mcp__github__`。
///
/// 调用方通过 `CodemodeToolOptions::namespaces` 提供这些配置：`prefix` 只参与
/// [`namespace_of`] 的前缀匹配，`name`/`description` 用于 [`codemode_section`] 的组头，
/// `instructions` 不出现在 `codemode` 段里，而是随 start 数据交给脚本的
/// `describeNamespace()`（见 `codemode.rs` 构造 start 数据的部分）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodemodeNamespace {
    /// 命名空间名。
    pub name: String,
    /// 给模型看的说明。
    pub description: Option<String>,
    /// 额外指示。
    pub instructions: Option<String>,
    /// 工具名前缀；最长命中者胜。
    pub prefix: String,
}

/// `codemode` 段里工具声明的默认 token 预算，与 coding agent 一致。
///
/// 只在 `CodemodeToolOptions::inline_budget` 为 `None` 时使用（`create_codemode_extension`
/// 里的 `unwrap_or`）。单位是估算 token：每 4 个 UTF-16 码元算 1 个。
pub const DEFAULT_CODEMODE_INLINE_BUDGET: f64 = 3000.0;

/// 估算工具段成本时每个 token 的字符数。
const CHARS_PER_TOKEN: f64 = 4.0;

/// 没有输出 schema 的工具解析为字符串。
fn text_output_schema() -> JsonValue {
    json!({ "type": "string" })
}

/// 脚本环境的固定开头。
const DESCRIPTION_INTRO: &str = "Run JavaScript that calls other tools. The input is raw JavaScript (not JSON, no code fence), run as an async function body in a QuickJS sandbox: top-level `await` and `return` work. No Node, file system, network, or timers.\n- `await tools.<name>({ ...args })` resolves to a string, or an object if the tool's declaration says so, and rejects with an Error on failure. A tool whose declaration returns an object resolves on failure too when it returned that object; check its fields. Calls still running when the script ends are cancelled.\n- The script survives a restart: it resumes where it was, without running finished calls again.\n- Optional first line: `// @options: {\"max_output_tokens\": 10000, \"timeout_ms\": 60000}`";

/// 全局函数的固定说明。
fn globals_description() -> String {
    [
        "Globals:",
        "- `text(value)`, `image(dataUrlOrImageBlock)`, `console.log(...)`, and top-level `return` add output; `exit()` ends the script. With several text items, each starts with a `==> text N/M <==` line, and `console` lines follow the other output in one `<console_output>` block. `image()` also saves the image to a temp file and the result names its path.",
        "- `store(key, value)` and `load(key)` keep JSON values across codemode calls.",
        "- `ALL_TOOLS`, `await searchTools(query, { limit?, namespace? })`, `await describeTool(name)`, `await describeNamespace(name)`: find tools the `codemode` section does not list.",
    ]
    .join("\n")
}

/// code mode 工具的固定描述：脚本环境与全局函数，每个会话都一样。
///
/// `models` 为真时在中间插入 `models` 命名空间的说明（`MODELS_DESCRIPTION`），与脚本
/// 实际可用的全局函数保持一致；最后一段指向系统提示的 `codemode` 段。
/// 各段用空行连接，返回值没有结尾换行；函数不接触工具列表，清单只在
/// [`codemode_section`] 里生成。
pub fn codemode_description(models: bool) -> String {
    let mut parts = vec![DESCRIPTION_INTRO.to_owned(), globals_description()];
    if models {
        parts.push(MODELS_DESCRIPTION.clone());
    }
    parts.push(
        "The `codemode` section of the system prompt lists the tools scripts can call, by namespace, as far as space allows."
            .to_owned(),
    );
    parts.join("\n\n")
}

/// 脚本看到的一个工具：它的声明。
///
/// `input_schema` 直接取工具的 `parameters`；`output_schema` 在没有结构化输出 schema 时
/// 回退为 `{ "type": "string" }`。本 crate 内只被 [`sample_of`] 消费；这个结构不参与执行，
/// 也不持久化。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Declaration {
    /// 工具名。
    pub name: String,
    /// 描述。
    pub description: String,
    /// 输入 schema。
    pub input_schema: JsonValue,
    /// 输出 schema。
    pub output_schema: JsonValue,
}

/// 脚本看到的一个工具：它的声明。没有输出 schema 的工具解析为文本输出。
///
/// 名称、描述与输入 schema 都从 `ToolRegistration` 复制；`structured_output_schema` 为
/// `None` 时输出类型渲染为 `Promise<string>`（回退 schema 由私有函数
/// `text_output_schema` 提供），与脚本实际拿到的字符串一致。
pub fn declaration_of(tool: &ToolRegistration) -> Declaration {
    Declaration {
        name: tool.tool.name.clone(),
        description: tool.tool.description.clone(),
        input_schema: tool.tool.parameters.clone(),
        output_schema: tool
            .structured_output_schema
            .clone()
            .unwrap_or_else(text_output_schema),
    }
}

/// 一个工具的 `toolSample`：描述加 TypeScript 声明。
///
/// 只渲染声明：`render_tool_sample` 读取 name/description/input_schema/output_schema，
/// 因此这里构造的 `execute` 是永远不会被调用的占位；`spread`/`signature` 只对全局函数
/// 有意义，工具渲染会忽略它们。调用方在构造 start 数据时用它冻结工具的说明文本。
pub fn sample_of(tool: &ToolRegistration) -> String {
    let declaration = declaration_of(tool);
    let codemode_tool = CodemodeTool {
        name: declaration.name,
        description: Some(declaration.description),
        input_schema: Some(declaration.input_schema),
        output_schema: Some(declaration.output_schema),
        spread: None,
        signature: None,
        // 这里只渲染声明，执行函数不会被调用。
        execute: std::sync::Arc::new(|_args, _context| {
            Box::pin(async { Ok(JsonValue::Null) }) as kiana_codemode::types::BoxFuture<'static, _>
        }),
    };
    render_tool_sample(&codemode_tool, None)
}

/// 一个工具的命名空间：名字以它的前缀开头、且前缀最长的那个。
///
/// 线性扫描 `namespaces`：只有严格更长的前缀才替换已命中的结果，因此长度相同时先出现的
/// 胜出。比较的是 `prefix.len()`（UTF-8 字节数）；空前缀会匹配所有名字，调用方需要避免
/// 这种配置。返回值借用自入参，不复制命名空间。
pub fn namespace_of<'a>(
    name: &str,
    namespaces: &'a [CodemodeNamespace],
) -> Option<&'a CodemodeNamespace> {
    let mut found: Option<&CodemodeNamespace> = None;
    for namespace in namespaces {
        if name.starts_with(&namespace.prefix)
            && (found.is_none() || namespace.prefix.len() > found.unwrap().prefix.len())
        {
            found = Some(namespace);
        }
    }
    found
}

/// `### \`id\` (\`raw name\`)` 后跟工具的描述与声明。
fn render_tool_section(tool: &ToolRegistration) -> String {
    let id = to_codemode_identifier(&tool.tool.name);
    let heading = if id == tool.tool.name {
        format!("### `{id}`")
    } else {
        format!("### `{id}` (`{}`)", tool.tool.name)
    };
    format!("{heading}\n{}", js_trim(&sample_of(tool)))
}

/// 预算选择前的候选：一个工具的渲染文本与估算成本（token）。
struct CatalogEntry {
    name: String,
    section: String,
    cost: f64,
}

/// 一个分组：无命名空间（`namespace == None`）或某个命名空间，以及按注册顺序排列的条目。
struct CatalogGroup {
    namespace: Option<CodemodeNamespace>,
    entries: Vec<CatalogEntry>,
}

/// 挑出预算内的工具段：每一轮每个组（先无命名空间组，再按名字排序的命名空间）放它最便宜的
/// 剩余工具；某组的下一个放不下就让该组退出，其他组继续。任何命名空间都先被表示，再谈完整。
///
/// 实现细节：
/// - 每组先按成本升序排序；`sort_by` 是稳定排序，成本相同时保持注册顺序。成本用
///   `partial_cmp` 比较，无法比较（`NaN`）时视为相等；成本来自 `ceil`，不会出现 `NaN`。
/// - `active` 保存“本轮还愿意尝试”的组下标；一轮结束后只保留仍成功取走条目的组。
/// - 预算是 `f64` 递减；成本是整数 token，因此最后可能剩下不足一个 token 的零头。
/// - 返回的是工具名集合，调用方再按各组条目的原始注册顺序过滤。
///
/// 注意“放不下”是永久退出：预算只减不增，该组之后也不会再放得下。
fn select_catalog(groups: &[CatalogGroup], budget: f64) -> std::collections::HashSet<String> {
    let mut queues: Vec<Vec<&CatalogEntry>> = groups
        .iter()
        .map(|group| {
            let mut entries: Vec<&CatalogEntry> = group.entries.iter().collect();
            entries.sort_by(|left, right| {
                left.cost
                    .partial_cmp(&right.cost)
                    .unwrap_or(std::cmp::Ordering::Equal)
            });
            entries
        })
        .collect();
    let mut shown: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut remaining = budget;
    let mut active: Vec<usize> = (0..queues.len())
        .filter(|i| !queues[*i].is_empty())
        .collect();
    while !active.is_empty() {
        let mut next_active: Vec<usize> = Vec::new();
        for index in active {
            let queue = &mut queues[index];
            let Some(next) = queue.first() else {
                continue;
            };
            if next.cost > remaining {
                continue;
            }
            remaining -= next.cost;
            shown.insert(next.name.clone());
            queue.remove(0);
            if !queue.is_empty() {
                next_active.push(index);
            }
        }
        active = next_active;
    }
    shown
}

/// `codemode` 提示段：脚本能调用的工具，按命名空间分组，在预算内给出声明。
/// 没有可调用工具时返回 `None`。
///
/// 返回文本以固定一行（`Tools scripts can call ...`）开头，随后是无命名空间组与各命名
/// 空间组（有内容的组才输出）；每个工具段由标题加 [`sample_of`] 的文本组成。`budget` 是
/// 估算 token 数，调用方传 `inline_budget` 或 [`DEFAULT_CODEMODE_INLINE_BUDGET`]。
///
/// `namespaces` 只影响分组与组头，不改变工具输出顺序：无命名空间组的工具按注册顺序排在
/// 最前，命名空间组按名字排序，组内仍按注册顺序。未被任何前缀命中的工具进入无命名空间组。
pub fn codemode_section(
    tools: &[ToolRegistration],
    namespaces: &[CodemodeNamespace],
    budget: f64,
) -> Option<String> {
    if tools.is_empty() {
        return None;
    }
    // 阶段 1：分组。0 号组固定是无命名空间组；命名空间组按工具首次出现的顺序追加。
    let mut groups: Vec<CatalogGroup> = vec![CatalogGroup {
        namespace: None,
        entries: Vec::new(),
    }];
    for tool in tools {
        let namespace = namespace_of(&tool.tool.name, namespaces);
        let key = namespace.as_ref().map(|namespace| namespace.name.clone());
        let index = match groups
            .iter()
            .position(|group| match (&group.namespace, &key) {
                (None, None) => true,
                (Some(existing), Some(key)) => &existing.name == key,
                _ => false,
            }) {
            Some(index) => index,
            None => {
                groups.push(CatalogGroup {
                    namespace: namespace.cloned(),
                    entries: Vec::new(),
                });
                groups.len() - 1
            }
        };
        let section = render_tool_section(tool);
        // 成本按 UTF-16 码元估算（不是字节），与 JS 的字符串长度口径一致。
        let cost = (utf16_len(&section) as f64 / CHARS_PER_TOKEN).ceil();
        groups[index].entries.push(CatalogEntry {
            name: tool.tool.name.clone(),
            section,
            cost,
        });
    }
    // 无命名空间组最前；其余按 `localeCompare` 升序。
    groups.sort_by(|left, right| match (&left.namespace, &right.namespace) {
        (None, None) => std::cmp::Ordering::Equal,
        (None, _) => std::cmp::Ordering::Less,
        (_, None) => std::cmp::Ordering::Greater,
        (Some(left), Some(right)) => locale_compare(&left.name, &right.name),
    });
    let shown = select_catalog(&groups, budget);

    // 阶段 2：渲染。组头先说明哪些工具没列出，再按注册顺序输出可见条目。
    let mut tool_sections: Vec<String> =
        vec!["Tools scripts can call (`await tools.<name>(args)`):".to_owned()];
    for group in &groups {
        if group.entries.is_empty() {
            continue;
        }
        let visible: Vec<&CatalogEntry> = group
            .entries
            .iter()
            .filter(|entry| shown.contains(&entry.name))
            .collect();
        if let Some(namespace) = &group.namespace {
            // 全列出、部分列出、一个都没列出，对应三种组头后缀。
            let listing = if visible.len() == group.entries.len() {
                ""
            } else if visible.is_empty() {
                " (tools not listed)"
            } else {
                " (some tools not listed)"
            };
            let description = namespace.description.as_deref().map(js_trim).unwrap_or("");
            let description_part = if description.is_empty() {
                String::new()
            } else {
                format!("\n{description}")
            };
            tool_sections.push(format!("## {}{listing}{description_part}", namespace.name));
        } else if visible.len() < group.entries.len() {
            tool_sections
                .push("Some tools are not listed: find them with `searchTools()`.".to_owned());
        }
        for entry in visible {
            tool_sections.push(entry.section.clone());
        }
    }
    Some(tool_sections.join("\n\n"))
}
