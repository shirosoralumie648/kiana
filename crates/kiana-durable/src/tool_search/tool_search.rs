//! tool-search/tool-search.ts：`tool_search` 工具。
//!
//! 模型搜索尚未提供给它的 deferred 工具，命中项用 `control.addTools` 装载，下一次请求就会提供。
//! 装载会写会话的 `modelTools`（它的 `add`），所以装载过的工具在该分支上持续提供，压缩之后也是。
//!
//! # 为什么需要它
//!
//! 会话可以启用很多工具（尤其是 MCP server 提供的工具），其中一部分以 `deferred` 标记注册：
//! 它们在模型侧默认不写进模型请求，只出现在 agent 的 `deferred` 列表里；`deferred` 标记
//! 本身不会把它们从工具侧的 `callable` 列表中排除（`callable` 的过滤条件里没有 `deferred`，
//! 只受已启用工具与 `callers` 影响）。模型先调用 `tool_search` 按自然语言描述找到它们，再通过
//! 本工具返回的 `control.addTools` 让它们进入下一次请求。
//! 没有它，这些工具的名字与说明不会出现在模型请求里，模型也就没有按描述搜索它们的入口。
//!
//! # 一次调用的执行流程
//!
//! 1. 读取参数：`query` 缺失或不是字符串时按空串处理，JS `trim()` 后仍为空就报错；`limit`
//!    缺失或为 JSON `null` 时用
//!    [`crate::tool_search::tool_search::DEFAULT_TOOL_SEARCH_LIMIT`]，否则必须是正整数的
//!    JSON number。
//! 2. 通过 `api.agent(context)` 解析当前 agent，把它的 `deferred` 快照作为候选。该列表只含
//!    「允许模型调用、标记为 deferred、且当前尚未被提供」的工具，所以装载过的工具不会再出现
//!    在后续搜索里。
//! 3. 为每个候选拼出可搜索文本（工具名、下划线换成空格的名字、描述、递归收集的 schema
//!    描述与属性名），交给 [`crate::search::rank_documents`] 做 BM25 排序，取前 `limit` 个
//!    名字。
//! 4. 组装结果：文本块列出命中的名字与描述首行；`details.loaded` 记录命中名字；命中非空时
//!    额外返回 `control.addTools`。工具轮次的提交会把各次调用的 `addTools` 累积起来交给
//!    `harness::agent::add_tools`，由它并入会话的 `modelTools`（对象形态写 `add`，数组形态
//!    追加到末尾）与 `tools`（仅数组形态），下一次请求就会提供这些工具。
//!
//! # 注意事项
//!
//! - 命中顺序就是排序结果（分数降序，同分保持候选顺序），并原样进入 `addTools`；集成测试
//!   因此断言工具按装载顺序而非注册顺序追加声明。
//! - 装载只记录**名字**，不要求此刻 registry 里存在同名工具：暂时下线的扩展重新提供同名
//!   工具后会照常被提供（`harness/agent.rs` 的 `add_tools`）。
//! - 没有命中时不返回 `control`，因此不会写 `modelTools`；输出固定为
//!   `No matching tools found.`。
//! - `execute` 返回 `Err`（空查询、非法 `limit`、解析 agent 失败）时，`harness/tool.rs`
//!   把它折算成 `is_error` 的工具结果，并让本次工具任务以失败结束（不是让进程失败）。
//! - 工具描述是常量，不随 deferred 集合变化：deferred 工具注册/解注册不会改变本工具在请求
//!   里的声明。
//! - 排序算法在 `crate::search`；`kiana-builtin-extensions` 里另有一份独立的 `tool_search`
//!   实现（另一套扩展 API），两处不共享代码。修改搜索文本或输出格式时需要一起检查。

use std::sync::Arc;

use kiana_ai::types::{TextContent, TextKind, UserContentBlock};
use kiana_chord::context::Context;
use serde::{Deserialize, Serialize};
use serde_json::{Value as JsonValue, json};

use crate::harness::define::{define_extension, define_tool};
use crate::harness::types::{
    Extension, ToolCaller, ToolControl, ToolExecuteFn, ToolExecutionApi, ToolExecutionResult,
    ToolRegistration,
};
use crate::search::{SearchDocument, rank_documents};
use crate::types::BoxError;

/// 工具名。
///
/// 工具声明的 `name`，模型用这个名字发起调用；集成测试也按这个名字构造 `tool_search` 调用。
/// 注意 `TOOL_SEARCH_DESCRIPTION` 里是手写的同一个名字（没有用本常量拼接），改名时要连同
/// 描述一起改，否则提示里的自指与实际工具名不一致。
pub const TOOL_SEARCH_TOOL_NAME: &str = "tool_search";
/// 默认返回多少条。
///
/// 只在 `limit` 缺失或显式为 JSON `null` 时使用；显式给出的值必须通过正整数校验，非法时
/// 报错而不是回退到这个默认值。
pub const DEFAULT_TOOL_SEARCH_LIMIT: u64 = 8;

/// 固定描述，所以 deferred 工具注册变化（例如 MCP 服务器连上）时它不会变。
///
/// 描述里不枚举具体工具（那会随 registry 变化），只说明用途，并明确提示模型对 MCP 工具发现
/// 应使用 `tool_search`。
const TOOL_SEARCH_DESCRIPTION: &str = "# Tool discovery\n\nSearches over deferred tool metadata with BM25 and exposes matching tools for the next model call.\n\nSome of the tools, such as tools of MCP servers, may not have been provided to you upfront, and you should use this tool (`tool_search`) to search for the required tools. For MCP tool discovery, always use `tool_search`.";

/// `tool_search` 结果的 details：它装载的工具。
///
/// 本文件把它序列化进 [`ToolExecutionResult`] 的 `details` 字段（给 UI 的结构化结果），与
/// 给模型的文本块相互独立。类型没有 serde 重命名，所以 JSON 键就是 `loaded`。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolSearchToolDetails {
    /// 已装载的工具名。
    ///
    /// 顺序与 [`rank_documents`] 的返回一致（BM25 分数降序，同分保持候选顺序）。没有命中时
    /// 是空数组：此时 `details` 仍会返回，但不会有 `control`。
    pub loaded: Vec<String>,
}

/// schema 的描述与属性名，递归收集。
///
/// 收集范围（与 TS 的 `schemaText` 一致）：
/// - 任意层级的 `description` 字符串；
/// - `properties` 的键名，并递归进入每个属性的 schema；
/// - `items` 的 schema（`items` 是数组等非对象值时直接跳过）；
/// - `anyOf` / `oneOf` / `allOf` 的每个变体。
///
/// `type`、`required`、默认值等其它键不参与，因此从 schema 里收集到的只有说明文字与字段名。
/// 输入是 `serde_json::Value`，本质是一棵树（可能来自 JSON 解析，也可能由程序构造），树
/// 本身不带环；但无环只排除了无限递归，并不能保证嵌套深度有限——本函数没有单独的深度上限。
/// 收集顺序不影响排序：BM25 的得分只依赖词项计数、IDF 与文档长度归一化，这些都与片段的
/// 收集顺序无关。
fn schema_text(schema: &JsonValue, parts: &mut Vec<String>) {
    let JsonValue::Object(object) = schema else {
        return;
    };
    if let Some(JsonValue::String(description)) = object.get("description") {
        parts.push(description.clone());
    }
    if let Some(JsonValue::Object(properties)) = object.get("properties") {
        for (name, property) in properties {
            parts.push(name.clone());
            schema_text(property, parts);
        }
    }
    if let Some(items) = object.get("items") {
        schema_text(items, parts);
    }
    for key in ["anyOf", "oneOf", "allOf"] {
        if let Some(JsonValue::Array(variants)) = object.get(key) {
            for variant in variants {
                schema_text(variant, parts);
            }
        }
    }
}

/// 一个工具的搜索文本：名字、把 `_` 换成空格的名字、描述，以及参数里的文字。
///
/// 分词按非 `[a-z0-9]` 切分（见 `crate::search` 的模块文档），下划线本来就会断开，所以第二份
/// 名字不增加新的词项，只让名字里的词在文本中出现两次，从而影响 BM25 的词频与文档长度。
/// 描述与 schema 文字由 `schema_text` 递归收集；JS `trim()` 后为空的片段会被丢弃，避免拼接
/// 出多余空格（与 TS 的 `parts.filter(...)` 对齐）。
fn search_text(tool: &ToolRegistration) -> String {
    let mut parts = vec![
        tool.tool.name.clone(),
        tool.tool.name.replace('_', " "),
        tool.tool.description.clone(),
    ];
    schema_text(&tool.tool.parameters, &mut parts);
    parts
        .into_iter()
        .filter(|part| !js_trim_is_empty(part))
        .collect::<Vec<_>>()
        .join(" ")
}

/// `part.trim() !== ""`。
///
/// 用 `kiana_json::js_string::js_trim` 而不是 `str::trim()`：JS 的空白字符集与 Rust 不同
/// （例如包含 BOM/U+FEFF 与 U+00A0，不含 U+0085），按 JS 语义判空才能与 TS 过滤片段的结果
/// 一致。
fn js_trim_is_empty(part: &str) -> bool {
    kiana_json::js_string::js_trim(part).is_empty()
}

/// 描述的第一行（`description.trim().split(/\r?\n/)[0]`）。
///
/// 先按 JS 语义去掉首尾空白，再取第一个 `\n` 之前的内容，并去掉行尾可能残留的 `\r`
/// （CRLF 换行）。因此空描述得到空串，单行描述得到整段 trim 后的文本；返回值不含 `\n`，
/// 首行没有前导空白，但行尾（`\n` 之前）的空格、制表符会保留，只去掉 CRLF 的 `\r`
/// （例如 `first_line("a \nb")` 得到 `"a "`）。命中列表每个工具只占一行，靠的就是这里
/// 不返回换行。
fn first_line(description: &str) -> String {
    let trimmed = kiana_json::js_string::js_trim(description);
    match trimmed.find('\n') {
        Some(index) => {
            let line = &trimmed[..index];
            line.strip_suffix('\r').unwrap_or(line).to_owned()
        }
        None => trimmed.to_owned(),
    }
}

fn text_block(text: String) -> UserContentBlock {
    UserContentBlock::Text(TextContent {
        kind_text: TextKind::Text,
        text,
        text_signature: None,
    })
}

/// `tool_search` 工具。
///
/// 返回一份工具注册：
/// - `name` 是 [`TOOL_SEARCH_TOOL_NAME`]，`description` 是固定常量；
/// - 参数 schema 必填 `query`（string），可选 `limit` 声明为 **number** 而不是 integer
///   （TS 的 schema 用 `Type.Number`）；小数会在执行期被 `execute_search` 拒绝；
/// - `callers` 只有模型：脚本（codemode）有各自的 `searchTools()`，不能调用本工具，因此它
///   也不会进入工具可解析的 `callable` 列表；
/// - `deferred: false`：本工具自身不是延迟工具，不需要先被“装载”；
/// - `replay: Safe`：中断后允许安全重跑（搜索只读会话，真正的写入由工具轮次提交完成）；
/// - `execute` 是转发到 `execute_search` 的闭包；构造本身无副作用，也不访问会话。
pub fn create_tool_search_tool() -> ToolRegistration {
    let execute: ToolExecuteFn = Arc::new(move |args, api, context| {
        Box::pin(async move { execute_search(args, api, context).await })
    });
    // 默认值从常量拼进描述，避免描述与 DEFAULT_TOOL_SEARCH_LIMIT 分叉。
    let limit_description =
        format!("Maximum number of tools to return. Defaults to {DEFAULT_TOOL_SEARCH_LIMIT}.");
    define_tool(ToolRegistration {
        tool: kiana_ai::Tool {
            name: TOOL_SEARCH_TOOL_NAME.to_owned(),
            description: TOOL_SEARCH_DESCRIPTION.to_owned(),
            parameters: json!({
                "type": "object",
                "required": ["query"],
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "Search query for deferred tools."
                    },
                    // 与 TS 的 `Type.Number` 对齐：schema 层不限制整数，执行期再校验正整数。
                    "limit": {
                        "type": "number",
                        "description": limit_description
                    }
                }
            }),
            constrained_sampling: None,
        },
        // 搜索改变提供给模型的东西；脚本有自己的 `searchTools()`。
        replay: Some(crate::harness::types::ReplayPolicy::Safe),
        execution_mode: None,
        prepare_arguments: None,
        structured_output_schema: None,
        // `callers: [Model]` 同时意味着本工具不进入工具可解析的 `callable` 列表
        // （见 `ToolRegistration.callers` 的约定）。
        callers: Some(vec![ToolCaller::Model]),
        // 工具自身直接提供，不参与延迟装载。
        deferred: false,
        output_limits: None,
        execute: Some(execute),
    })
}

/// `tool_search` 的执行体：校验参数 → 取当前未提供的延迟工具 → BM25 排序 → 组装结果。
///
/// # 参数
/// - `args`：已经过 harness 参数校验/转换的 JSON 参数。函数自身仍做防御：`query` 缺失或不是
///   字符串时按空串处理（随后以“不能为空”失败）；`limit` 缺失或为 JSON `null` 时用默认值。
/// - `api`：本次调用的操作句柄，只在本次调用内有效；这里只用它解析一次 agent。
/// - `context`：取消上下文，原样传给 `api.agent`。
///
/// # 返回
/// - `Ok`：文本输出、`details`，以及命中非空时的 `control.addTools`。
/// - `Err`：空查询、非法 `limit` 或 `api.agent` 解析失败；`harness/tool.rs` 会把它折算成
///   `is_error` 的工具结果，并让本次工具任务失败。
///
/// # 副作用
/// 本函数只读会话；写 `modelTools` 发生在工具轮次的提交里，由 `harness/agent.rs` 的
/// `add_tools` 消费这里返回的 `control.addTools`。
async fn execute_search(
    args: JsonValue,
    api: Arc<dyn ToolExecutionApi>,
    context: Arc<dyn Context>,
) -> Result<ToolExecutionResult, BoxError> {
    // 阶段 1：读取并校验 query。schema 已要求 query 必填（缺失会在 harness 的参数校验阶段被
    // 拒绝）；这里再把“非字符串”和“空串”统一成同一个错误。传给 BM25 的是原串，trim 只用于
    // 判空——分词本身会丢弃空白，结果不受影响。
    let query = args
        .get("query")
        .and_then(JsonValue::as_str)
        .unwrap_or("")
        .to_owned();
    if kiana_json::js_string::js_trim(&query).is_empty() {
        return Err("query must not be empty".into());
    }
    // 阶段 2：读取 limit。缺失或 JSON null 用默认值；否则必须是 JSON number，且满足
    // Number.isInteger 语义（有限、没有小数部分）并大于 0。
    let max = match args.get("limit") {
        None | Some(JsonValue::Null) => DEFAULT_TOOL_SEARCH_LIMIT,
        Some(value) => {
            let valid = value
                .as_f64()
                .filter(|number| kiana_json::js_num::is_js_integer(*number) && *number > 0.0);
            match valid {
                Some(number) => number as u64,
                None => return Err("limit must be a positive integer".into()),
            }
        }
    };
    // 阶段 3：取当前 agent 的 deferred 快照。该列表只含“允许模型调用、标记 deferred、且当前
    // 未被提供”的工具（`Agent::deferred` 的定义），所以已装载的工具不会再次成为候选。
    // clone 复制候选注册，后续不再持有 Agent，也不在排序期间保持对它的借用。
    let candidates = api.agent(&*context).await?.deferred.clone();
    // 阶段 4：构造搜索文档。SearchDocument 的 name/text 都是借用，而每个工具的搜索文本是
    // 临时计算的 String，不能直接放进文档；因此先建出文档（text 暂为空串），把文本集中在
    // texts 里，再把切片逐项指过去。texts 必须活到 rank_documents 返回。
    let documents: Vec<SearchDocument<'_>> = candidates
        .iter()
        .map(|tool| SearchDocument {
            name: &tool.tool.name,
            text: "",
        })
        .collect();
    let texts: Vec<String> = candidates.iter().map(search_text).collect();
    // 两份 Vec 与 candidates 按下标一一对应；循环把 documents 里的空串换成对 texts 的借用。
    let mut documents = documents;
    for (index, text) in texts.iter().enumerate() {
        documents[index].text = text;
    }
    let loaded = rank_documents(&query, &documents, max as usize);
    // 阶段 5：组装给模型的文本。没有命中时用固定句子；命中时每个工具一行，描述只取首行，
    // 避免把多行描述塞进列表。plural 只需区分 1 与其它（空集已在上一分支处理）。
    let text = if loaded.is_empty() {
        "No matching tools found.".to_owned()
    } else {
        let lines = loaded
            .iter()
            .map(|name| {
                // 名字来自 candidates，正常路径一定能找回描述；unwrap_or("") 只是防御
                // （与 TS 的 `?? ""` 对应）。
                let description = candidates
                    .iter()
                    .find(|tool| &tool.tool.name == name)
                    .map(|tool| tool.tool.description.as_str())
                    .unwrap_or("");
                format!("- {name}: {}", first_line(description))
            })
            .collect::<Vec<_>>()
            .join("\n");
        let plural = if loaded.len() == 1 { "" } else { "s" };
        format!(
            "Loaded {} tool{plural}. They are available from your next call:\n{lines}",
            loaded.len()
        )
    };
    // 阶段 6：details 总是返回（没有命中时 loaded 是空数组）；control 只在有命中时返回。
    // 没有 control 时工具轮次不会累积任何 addTools，因此“没搜到”不会改变后续提供哪些工具
    // （harness/generation.rs 只在 addTools 非空时调用 add_tools）。
    let details = ToolSearchToolDetails {
        loaded: loaded.clone(),
    };
    Ok(ToolExecutionResult {
        output: Some(vec![text_block(text)]),
        details: Some(serde_json::to_value(&details)?),
        control: if loaded.is_empty() {
            None
        } else {
            Some(ToolControl {
                add_tools: Some(loaded),
                terminate: None,
                handoff: None,
            })
        },
        ..Default::default()
    })
}

/// `tool_search` 扩展的载体：只含一个工具，扩展名固定为 `tool-search`。
///
/// 扩展名是 registry 里的身份：再次 `install` 同名扩展会原位替换，而不是并存两个
/// （`harness/registry.rs` 的 `Registry::install`）。
struct ToolSearchExtension {
    /// 构造扩展时创建一次；`Extension::tools` 每次调用都返回克隆。
    tools: Vec<ToolRegistration>,
}

impl Extension for ToolSearchExtension {
    fn name(&self) -> &str {
        // 固定名字：`install` 用同名替换完成重装。
        "tool-search"
    }

    fn tools(&self) -> Vec<ToolRegistration> {
        // 返回克隆而不是借用：trait 方法交出拥有所有权的 Vec，调用点可能在多个时刻重新求值
        // 且不缓存（见 `Extension` 的文档）。
        self.tools.clone()
    }
}

/// `tool_search` 扩展：装上它，并在模型该找到 deferred 工具的地方启用。
///
/// 返回的扩展名字是 `tool-search`，只提供一个 `deferred: false` 的 `tool_search` 工具。
/// 本函数只构造、不注册；调用方通过 registry 的 `install` 安装，是否真的让模型看到这个工具
/// 还取决于 agent 解析时选了哪些扩展（`harness/agent.rs` 的 `resolve_agent`）。
pub fn create_tool_search_extension() -> Arc<dyn Extension> {
    define_extension(ToolSearchExtension {
        tools: vec![create_tool_search_tool()],
    })
}
