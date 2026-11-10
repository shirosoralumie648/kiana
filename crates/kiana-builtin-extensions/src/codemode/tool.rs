//! extensions/codemode/tool.ts：`codemode` 工具（模型写 JavaScript 调用其它工具）。
//!
//! 脚本可用 `tools`、`ALL_TOOLS`、`text()`、`image()`、`exit()`、`store()`/`load()`、
//! `console.*`、`return <value>`，首行可以是 `// @options:`，并可经 `models.*` 访问模型目录、
//! 分类器与图像模型。结果以 "Script completed" / "Script failed" 开头。
//!
//! 脚本可以调用 agent loop 的嵌套工具：active 的 `direct` 工具，以及所有 `codemode` /
//! `deferred` 工具。嵌套调用走 agent loop 的工具管线（`ctx.executeTool`），因此校验、
//! `tool_call`/`tool_result` 钩子与权限检查与直接调用一致。只有脚本的输出进入模型上下文。
//!
//! 嵌套结果交给脚本的方式：
//! - 声明了 `outputSchema` 的工具解析为它的 `structuredContent`（带该字段的错误结果也一样）；
//! - 其它工具解析为其文本内容（多块以 `\n` 连接）组成的单个字符串；
//! - 失败/被阻止/非法调用以带工具错误文本的 Error reject。
//!
//! 失败的脚本返回**正常错误结果**，保留部分输出，后接 "Script error:" 与错误。
//! `store(key, value)`/`load(key)` 跨调用保存 JSON 值；成功的脚本把写入作为
//! `codemode-store` custom entry 追加到会话，因此每个分支只看到自己路径上的值。
//!
//! # 设计原因
//!
//! 描述文本是模型了解 codemode 用法的主要来源：它同时承担“工具清单”（每个工具的声明与
//! 描述）和“用法说明”（如何等待、如何 `store()`）。因此字符串的顺序（intro → globals → 按
//! namespace 分组的工具）与预算裁剪都是可观察行为，被
//! `tests/fixtures/codemode_loadout.json` 的 golden 快照锁定。
//!
//! 会话里的描述由 [`prepare_codemode_loadout`] 在 loadout 变化时重算：`on` 模式把已声明
//! 工具的说明改写成“脚本里如何调用”，并只列出没有 `direct` exposure 的可调用工具；
//! `only` 模式列出全部可调用工具，并隐藏 active 的 `direct` 声明以免重复。列表按 exposure
//! 而不是 active 集合决定，所以 `tool_search` 激活工具不会让 codemode 描述抖动。
//!
//! # 执行流程
//!
//! 1. `kiana-session` 在计算 loadout 时调用 [`prepare_codemode_loadout`]，把返回的描述合并进
//!    模型可见的工具声明。
//! 2. agent loop 调用 `codemode` 时进入 `CodemodeToolExecutable::execute`，再把参数交给
//!    `crate::codemode::execute::execute_codemode`。
//! 3. `execute_codemode` 用 `ctx.tools()` 取可调用工具，按同样的声明规则生成脚本侧工具，
//!    并通过 `ctx.execute_tool()` 执行嵌套调用。本文件只负责声明、描述与可达性，不执行脚本。
//!
//! # 注意事项
//!
//! - 选项里的回调必须在每次使用时重新调用（settings 会变）；不要缓存到
//!   [`CodemodeToolOptions`] 的构造时刻。
//! - 工具 section 的成本按 UTF-16 码元数 / 4 估算，和 JS 的 `string.length` 对齐；换成字节数
//!   会改变预算裁剪的结果。
//! - [`CodemodeToolDetails`] 会随工具结果进入会话记录，所以嵌套调用的参数与错误只保留预览。

use std::collections::HashSet;
use std::sync::{Arc, LazyLock};

use indexmap::IndexMap;
use kiana_agent_core::{AgentTool, AgentToolError, AgentToolResult, AgentToolUpdateCallback};
use kiana_ai::types::{ConstrainedSampling, ConstrainedSamplingConfig, GrammarVariants, JsonValue};
use kiana_codemode::types::CodemodeToolExecute;
use kiana_codemode::{
    CODEMODE_SOURCE_GRAMMAR, CodemodeTool, render_tool_output_type, render_tool_sample,
    to_codemode_identifier,
};
use kiana_config::settings::CodemodeMode;
use kiana_config::settings::defaults::SETTINGS_DEFAULTS;
use kiana_ext_api::tool_types::{
    PrepareLoadoutFn, ReachesToolsFn, ReachesToolsResult, ToolDefinition, ToolDefinitionExecute,
    ToolExposure, ToolLoadout, ToolLoadoutChanges, ToolNamespace,
};
use kiana_ext_api::{BoxFuture, ExtensionToolContextApi};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use tokio_util::sync::CancellationToken;

/// `codemode` 的工具名。
///
/// [`get_codemode_callable_tools`] 用它把 codemode 自己排除出脚本可调用集合（脚本不能再
/// 启动脚本），[`prepare_codemode_loadout`] 用它作为写回描述的键。这个字面量同时进入模型
/// 可见的工具声明，golden 快照按它比对。
pub const CODEMODE_TOOL_NAME: &str = "codemode";

/// 保存一次脚本 `store()` 写入的 custom entry 类型。
///
/// 回放时 `read_codemode_store` 只认这个字符串；会话记录一旦写出就靠它继续可读，因此它属于
/// 持久化格式，改名会让旧会话里的 `store()` 数据静默失效。
pub const CODEMODE_STORE_ENTRY_TYPE: &str = "codemode-store";

/// `codemode-store` custom entry 的 `data`。
///
/// 同一 entry 内 `delete` 先于 `set` 应用；多个 entry 按分支顺序累加，因此同键
/// “删除后又写入”时最终存在。两个字段都带 `#[serde(default)]`，直接反序列化本结构时缺字段
/// 按空集处理。但会话回放并不只依赖 serde：`read_codemode_store` 先用 `is_store_entry_data`
/// 检查 `set`/`delete` 都存在且类型正确，不满足的 entry 会被整条跳过。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct CodemodeStoreEntryData {
    /// 写入的值。
    ///
    /// `IndexMap` 保留写入顺序，回放时按同一顺序插回 store。
    #[serde(default)]
    pub set: IndexMap<String, JsonValue>,
    /// 删除的键。
    ///
    /// 不存在的键按无操作处理。
    #[serde(default)]
    pub delete: Vec<String>,
}

/// 一次嵌套调用的状态。
///
/// 序列化为 `running`/`ok`/`error`/`cancelled`。`running` 只出现在执行期间的流式 details；
/// 脚本结束时仍为 `running` 的记录会被执行器统一改成 `cancelled`。对工具调用，`is_error`
/// 时脚本的 `CancellationToken` 已取消记 `cancelled`、否则记 `error`；对 `models.*` 调用，
/// 则按模型调用的停止原因（stop/aborted/error）映射。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CodemodeNestedCallStatus {
    /// 正在运行。
    Running,
    /// 正常返回。
    Ok,
    /// 失败。
    ///
    /// 工具调用返回 `is_error`，或 `models.*` 的停止原因为 error。
    Error,
    /// 取消。
    ///
    /// 脚本结束时仍未完成、工具调用期间信号已取消，或 `models.*` 停止原因为 aborted。
    Cancelled,
}

/// 一次嵌套工具调用（`CodemoToolDetails.calls` 的一项）。
///
/// 记录在调用开始时以 `running` 追加，完成后原地更新状态、耗时与错误。工具调用的 `id` 先写成
/// 占位 `<codemode call id>/?`，返回后替换为会话分配的 `<codemode call id>/<n>`；`models.*`
/// 调用则一开始就是 `<codemode call id>/<name>/<count>`。`serde` 的 camelCase 让字段名与 TS 的
/// details JSON 一致（`durationMs` 等），TUI 渲染器按这个形状反序列化。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CodemodeNestedCall {
    /// 嵌套调用的工具调用 id，`<codemode call id>/<n>`。
    ///
    /// `models.*` 调用使用另一种形状：`<codemode call id>/<name>/<count>`。
    pub id: String,
    /// 工具名。
    pub name: String,
    /// 参数的紧凑 JSON（供显示截断）。
    ///
    /// 超过 200 个 UTF-16 码元时截断并加 `...`；参数为 `null` 时存空串（kiana-codemode 用
    /// `null` 表示无参的 `undefined`）。
    pub args: String,
    /// 状态。
    pub status: CodemodeNestedCallStatus,
    /// 耗时（毫秒）。
    ///
    /// `running` 时为 `None`，完成后由执行器填入。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<f64>,
    /// 错误文本（供显示截断）。
    ///
    /// 超过 500 个 UTF-16 码元时截断并加 `...`；执行器在出错时填入。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// 报告了 usage 的 `models.*` 调用成本（USD）。
    ///
    /// 嵌套工具调用自身的 usage 不在其中（由会话侧统计）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost: Option<f64>,
}

/// `codemode` 的 details。
///
/// 序列化为 camelCase（`calls`/`fullOutputPath`）。`calls` 按调用开始顺序排列；脚本运行期间
/// 执行器会通过 `on_update` 发送它的快照，最终结果里再完整写一次。渲染器反序列化失败时退回
/// `Default`，所以缺字段或旧格式只会显示为空，不会中断渲染。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CodemodeToolDetails {
    /// 嵌套调用。
    ///
    /// 按调用开始顺序排列。
    #[serde(default)]
    pub calls: Vec<CodemodeNestedCall>,
    /// 输出被截断时保存全文的临时文件。
    ///
    /// 只有发生截断且写临时文件成功时才有值；未截断或落盘失败都省略。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub full_output_path: Option<String>,
}

/// `codemode` 的系统提示贡献。
///
/// 字段是 `'static`：常量直接编译进二进制，创建 `ToolDefinition` 时才拷贝成 `String` 装入
/// `prompt_snippet` / `prompt_guidelines`。
#[derive(Debug, Clone, Copy)]
pub struct CodemodeSystemPromptContribution {
    /// 一行摘要。
    ///
    /// 进入系统提示的“Available tools”段。
    pub snippet: &'static str,
    /// guideline 条目。
    ///
    /// 工具 active 时追加到系统提示的 Guidelines 段。
    pub guidelines: &'static [&'static str],
}

/// `codemodeToolSystemPromptContribution`（冻结的导出名保持 TS 的小写拼写）。
///
/// 名字沿用参考实现的冻结导出名（小写拼写），`#[allow(non_upper_case_globals)]` 仅为保留它；重命名会丢掉
/// 与参考源码的一一对应关系。
#[allow(non_upper_case_globals)]
pub const codemode_tool_system_prompt_contribution: CodemodeSystemPromptContribution =
    CodemodeSystemPromptContribution {
        snippet: "Run JavaScript that calls other tools",
        guidelines: &[
            "Use codemode to batch independent tool calls (Promise.allSettled), chain them, or filter large output, instead of many separate calls.",
        ],
    };

/// 脚本参考文档 `docs/codemode.md` 的路径。
///
/// TS 在模块加载时 `join(getDocsPath(), "codemode.md")`；Rust 用惰性静态保存同一路径。
///
/// 首次解引用时计算一次，之后进程内复用。描述文本（`describe_globals` 的输出）与
/// `models.*` 的错误消息都会拼进这个路径，所以它的取值是可观察行为。
pub static CODEMODE_DOCS_PATH: LazyLock<String> = LazyLock::new(|| {
    kiana_core::config::get_docs_path()
        .join("codemode.md")
        .to_string_lossy()
        .into_owned()
});

/// `SETTINGS_DEFAULTS.codemode.inlineBudget`（3000）。
///
/// [`prepare_codemode_loadout`] 在 `get_inline_budget` 缺失或返回 `None` 时回落到它。
pub const DEFAULT_CODEMODE_INLINE_BUDGET: f64 = SETTINGS_DEFAULTS.codemode.inline_budget;

/// 估算工具 section 成本时每 token 的字符数。
///
/// 这里的“字符”是 UTF-16 码元（JS 的 `string.length`）；成本 = `ceil(码元数 / 4)`。
const CHARS_PER_TOKEN: f64 = 4.0;

/// `DESCRIPTION_INTRO`（逐字）。
///
/// 这是模型可见协议的一部分（原始 JS 而不是 JSON、沙箱能力、`tools.<name>` 的解析规则、
/// `// @options:` 首行），golden 快照按逐字内容比对；不要翻译或重排。
const DESCRIPTION_INTRO: &str = "Run JavaScript that calls other tools. The input is raw JavaScript (not JSON, no code fence), run as an async function body in a QuickJS sandbox: top-level `await` and `return` work. No Node, file system, network, or timers.\n- `await tools.<name>({ ...args })` resolves to a string, or an object if the tool's declaration says so, and rejects with an Error on failure. A tool whose declaration returns an object resolves on failure too when it returned that object; check its fields. Calls still running when the script ends are cancelled.\n- Optional first line: `// @options: {\"max_output_tokens\": 10000, \"timeout_ms\": 60000}`";

/// `TEXT_OUTPUT_SCHEMA`。
///
/// 没有 `outputSchema` 的工具在声明里的默认输出类型：只含 `type: "string"`，于是脚本把调用
/// 结果当字符串处理。[`to_codemode_declaration`] 用它保证声明里始终有输出 schema。
fn text_output_schema() -> JsonValue {
    let mut schema = Map::new();
    schema.insert("type".to_string(), Value::String("string".to_string()));
    Value::Object(schema)
}

/// `codemodeSchema`（TypeBox `Type.Object({ code: Type.String(...) })` 的 JSON 形态）。
///
/// 键的插入顺序是 `type`/`required`/`properties`，`code` 内是 `type`/`description`。工作区的
/// `serde_json` 开启了 `preserve_order`，所以序列化字符串的键序稳定；测试
/// `schemas_match_the_typebox_output_order` 锁定该字符串，调整字段顺序会破坏它。
pub fn codemode_schema() -> JsonValue {
    let mut code = Map::new();
    code.insert("type".to_string(), Value::String("string".to_string()));
    code.insert(
        "description".to_string(),
        Value::String("Raw JavaScript source.".to_string()),
    );
    let mut properties = Map::new();
    properties.insert("code".to_string(), Value::Object(code));
    let mut schema = Map::new();
    schema.insert("type".to_string(), Value::String("object".to_string()));
    schema.insert(
        "required".to_string(),
        Value::Array(vec![Value::String("code".to_string())]),
    );
    schema.insert("properties".to_string(), Value::Object(properties));
    Value::Object(schema)
}

/// 每个全局一行；细节在 [`CODEMODE_DOCS_PATH`] 的文档里。
///
/// `models` 为 true 时追加一行 `models` 命名空间说明，并拼进文档路径；路径因安装位置而异，
/// 测试用正则把它归一成 `<docs>` 后再与 golden 比对（见 `tests/tool_search.rs`）。
fn describe_globals(models: bool) -> String {
    let mut lines = vec![
        "Globals:".to_string(),
        "- `text(value)`, `image(dataUrlOrImageBlock)`, `console.log(...)`, and top-level `return` add output; `exit()` ends the script. With several text items, each starts with a `==> text N/M <==` line, and `console` lines follow the other output in one `<console_output>` block. `image()` also saves the image to a temp file and the result names its path.".to_string(),
        "- `store(key, value)` and `load(key)` keep JSON values across codemode calls.".to_string(),
        "- `ALL_TOOLS`, `await searchTools(query, { limit?, namespace? })`, `await describeTool(name)`, `await describeNamespace(name)`: find unlisted tools, such as MCP tools.".to_string(),
    ];
    if models {
        lines.push(format!(
            "- `models`: classifiers and image generation. Read {} first.",
            *CODEMODE_DOCS_PATH
        ));
    }
    lines.join("\n")
}

/// 读取工具 namespace 的回调（TS `getToolNamespace?: (toolName) => ToolNamespace | undefined`）。
///
/// 执行脚本时按工具名查询，供 `searchTools()` 的排名/`namespace` 过滤与 `describeNamespace()`
/// 使用；模型描述的 namespace 分组走的是 `ToolLoadout::get_namespace`，不经过本回调。返回
/// `None` 表示该工具不属于任何 namespace。回调必须 `Send + Sync`，因为它随 `ToolDefinition`
/// 跨线程传递。
pub type GetToolNamespaceFn = Arc<dyn Fn(&str) -> Option<ToolNamespace> + Send + Sync>;

/// 读取全部工具 guideline 的回调（TS `getToolGuidelines?: () => ReadonlyMap<...>`）。
///
/// 一次返回整张“工具名 → guideline 列表”的表，而不是按名查询。执行脚本前用它生成脚本侧
/// 工具样例；生成模型描述时走的是 `ToolLoadout::get_prompt_guidelines`，不经过本回调。
pub type GetToolGuidelinesFn = Arc<dyn Fn() -> IndexMap<String, Vec<String>> + Send + Sync>;

/// 持久化 `store()` 写入的回调。
///
/// TS 的 `appendEntry?: (customType, data) => void` 是同步的；Rust 侧的扩展 API 在单线程
/// `LocalSet` 上，`ToolDefinitionExecute` 的 future 却可能在任何线程轮询，因此这里用返回
/// future 的版本，让实现可以把调用转发到同线程任务并等待完成，从而保证写入顺序。
///
/// 只有成功的脚本、且 `store` 写入非空时才调用，执行器会 `await` 它后再返回工具结果。
pub type AppendEntryFn =
    Arc<dyn Fn(String, CodemodeStoreEntryData) -> BoxFuture<'static, ()> + Send + Sync>;

/// 读取 `codemode.mode` 的回调。
///
/// 回调本身总是返回一个模式；[`CodemodeToolOptions`] 里没有这个回调时，由
/// [`prepare_codemode_loadout`] 回落到 settings 默认值（`on`）。每次计算 loadout 都会重新
/// 读取，以反映最新的设置。
pub type GetModeFn = Arc<dyn Fn() -> CodemodeMode + Send + Sync>;

/// 读取 `codemode.inlineBudget` 的回调；`None` 表示未设置（用默认预算）。
///
/// 返回的值直接进入预算比较，因此应当是有限、非负的；`codemode/index.rs` 在读 settings 时
/// 会过滤掉非有限值与负数，直接构造选项的调用方需要自行保证。
pub type GetInlineBudgetFn = Arc<dyn Fn() -> Option<f64> + Send + Sync>;

/// `createCodemodeToolDefinition` 的选项。
///
/// 全部字段都是回调/开关，而不是快照值：[`prepare_codemode_loadout`] 与执行器在每次用到时
/// 重新查询，从而跟随设置变化与工具注册表变化。`codemode/index.rs` 用线程本地表把这些回调
/// 桥接到单线程的扩展 API；直接 `Default` 则得到不带 `models`、不持久化 store、`on` 模式、
/// 默认预算的独立工具。
#[derive(Clone, Default)]
pub struct CodemodeToolOptions {
    /// 工具的 namespace，用于 `searchTools()` 排名与 `namespace` 过滤。
    pub get_tool_namespace: Option<GetToolNamespaceFn>,
    /// 每个工具的 guideline，按工具名索引；`describeTool()` 与 `ALL_TOOLS` 展示用。
    pub get_tool_guidelines: Option<GetToolGuidelinesFn>,
    /// 把 `models` 命名空间暴露给脚本（由会话的 model registry 支撑）。缺省不声明。
    pub models: Option<bool>,
    /// 把 `store()` 写入持久化为会话 custom entry。
    pub append_entry: Option<AppendEntryFn>,
    /// 工具 active 时如何呈现 loadout（`codemode.mode` 设置）。缺省 `on`。
    pub get_mode: Option<GetModeFn>,
    /// 描述里工具声明的 token 预算。缺省 [`DEFAULT_CODEMODE_INLINE_BUDGET`]。
    pub get_inline_budget: Option<GetInlineBudgetFn>,
}

// 手动实现：闭包字段没有 `Debug`。只打印回调是否存在，让日志能看出配置了哪些能力，
// 又不会把 `Arc<dyn Fn>` 打成无意义的地址。
impl std::fmt::Debug for CodemodeToolOptions {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CodemodeToolOptions")
            .field("models", &self.models)
            .field("append_entry", &self.append_entry.is_some())
            .field("get_mode", &self.get_mode.is_some())
            .field("get_inline_budget", &self.get_inline_budget.is_some())
            .field("get_tool_namespace", &self.get_tool_namespace.is_some())
            .field("get_tool_guidelines", &self.get_tool_guidelines.is_some())
            .finish()
    }
}

/// `toCodemodeDeclaration(tool, guidelines = [])`：脚本看到的一个工具。
///
/// 无 guideline 时 description 原样保留（不 trim）；有 guideline 时先 `trim()` 再拼接。
/// `outputSchema` 缺省为 `{ type: "string" }`。空/全空白的 guideline 会被丢弃，其余先做 JS
/// `trim` 再加 `- ` 前缀。
///
/// 返回的声明只用于渲染与检索：`execute` 是空占位，真正的嵌套调用由执行器经
/// `ctx.execute_tool()` 完成，因此这里不需要绑定会话上下文。
pub fn to_codemode_declaration(tool: &AgentTool, guidelines: &[String]) -> CodemodeTool {
    let bullets: Vec<String> = guidelines
        .iter()
        .filter_map(|guideline| {
            let trimmed = kiana_json::js_string::js_trim(guideline);
            (!trimmed.is_empty()).then(|| format!("- {trimmed}"))
        })
        .collect();
    let description = if bullets.is_empty() {
        tool.description.clone()
    } else {
        format!(
            "{}\n\n{}",
            kiana_json::js_string::js_trim(&tool.description),
            bullets.join("\n")
        )
    };
    CodemodeTool {
        name: tool.name.clone(),
        description: Some(description),
        input_schema: Some(tool.parameters.clone()),
        output_schema: Some(
            tool.output_schema
                .clone()
                .unwrap_or_else(text_output_schema),
        ),
        spread: None,
        signature: None,
        execute: noop_execute(),
    }
}

/// 声明渲染用的空执行器（`renderToolSample` 只读 name/description/schema）。
fn noop_execute() -> CodemodeToolExecute {
    Arc::new(
        |_args: JsonValue, _context: kiana_codemode::CodemodeToolContext| {
            Box::pin(async { Ok::<Value, String>(Value::Null) })
        },
    )
}

/// 脚本可以调用的工具：给定工具中除 `codemode` 自身以外的全部。
///
/// 描述生成与脚本沙箱的工具表都用这一份结果，因此“模型看到的清单”和“脚本能调用的集合”来自
/// 同一规则；返回的是克隆（`AgentTool` 内部是 `Arc`，克隆很便宜），调用方可以自由过滤。
pub fn get_codemode_callable_tools(tools: &[AgentTool]) -> Vec<AgentTool> {
    tools
        .iter()
        .filter(|tool| tool.name != CODEMODE_TOOL_NAME)
        .cloned()
        .collect()
}

/// `createCodemodeDescription` 的选项。
///
/// 这些字段只影响模型可见的描述，不改变脚本能调用什么：真正执行时沙箱工具表来自
/// `ctx.tools()`，与这里的过滤无关。因此 `deferred` 工具“不列出”但仍然可调用。
#[derive(Debug, Clone, Default)]
pub struct CodemodeDescriptionOptions {
    /// 声明 `models` 命名空间；只对带模型访问的工具创建。
    ///
    /// 它只控制描述文本；执行器还要同时满足 `options.models == Some(true)` 且有会话上下文，
    /// 才会真的注入 `models` 全局。
    pub models: bool,
    /// 每个工具的 namespace，按工具名索引。同一 namespace 的工具列在一个标题下。
    ///
    /// 这里的 `ToolNamespace` 只用 `name` 与 `description`（`aliases`/`instructions` 不进
    /// 工具列表）；没有条目的工具归入无 namespace 的默认组。
    pub namespaces: Option<IndexMap<String, ToolNamespace>>,
    /// 可调用但从不列出声明（`deferred` exposure）的工具。
    ///
    /// 命中集合的工具在分组与按预算挑选之前就被过滤掉，因此既不出现在描述里，也不占用
    /// `inline_budget`。
    pub deferred: Option<HashSet<String>>,
    /// 每个工具的 prompt guideline，按工具名索引，列在描述之后。
    ///
    /// 每条 guideline 经 JS `trim` 后以 `- ` 前缀追加到该工具描述末尾；空条目被丢弃。
    pub guidelines: Option<IndexMap<String, Vec<String>>>,
    /// 工具 section 可用的估算 token 数（字符数 / 4）。放不下的工具像 deferred 一样省略。
    /// 未设置时列出全部非 deferred 工具。
    ///
    /// 预算只约束“工具 section”：intro、globals 与 namespace 标题不计入，也不会因为预算
    /// 被截断。
    pub inline_budget: Option<f64>,
}

/// `### \`id\` (\`raw name\`)` 加工具描述与声明。
///
/// `id` 是脚本里的标识符（`to_codemode_identifier` 的结果）。只有转换后与原始工具名不同时
/// 才在标题里附带 `(raw name)`，让模型仍能找到 `tools["<原始名>"]`。section 正文是
/// `render_tool_sample` 经 JS `trim` 后的结果；这段字符串的长度就是预算成本的计量对象。
fn render_tool_section(declaration: &CodemodeTool) -> String {
    let id = to_codemode_identifier(&declaration.name);
    let heading = if id == declaration.name {
        format!("### `{id}`")
    } else {
        format!("### `{id}` (`{}`)", declaration.name)
    };
    format!(
        "{heading}\n{}",
        kiana_json::js_string::js_trim(&render_tool_sample(declaration, None))
    )
}

/// 目录里的一项：一个工具的名字、渲染好的 section 与估算成本（token）。
struct CatalogEntry {
    name: String,
    section: String,
    cost: f64,
}

/// 按 namespace 分组的一组条目；`namespace == None` 是不属于任何 namespace 的默认组。
struct CatalogGroup {
    namespace: Option<ToolNamespace>,
    entries: Vec<CatalogEntry>,
}

/// 在预算内挑出工具 section：每轮每个组（无 namespace 的组在前，其后按名字）放一个最便宜的
/// 剩余工具；下一项放不下的组退出，其它组继续。任一 namespace 完成之前，每个 namespace 都
/// 已被代表。
///
/// 具体规则（与 `create_codemode_description` 的调用配合）：
/// - `budget == None`：无条件返回全部工具名；
/// - 否则每组内部先按 `cost` 升序排序（稳定排序，同成本保持声明顺序），再按组轮转；
/// - 某组最便宜的剩余项都放不下时，该组在后续轮次不再参与（`continue` 后不回填
///   `next_queues`），其它组继续；
/// - 每轮至少消费一项或淘汰一组，循环必然终止。
///
/// 边界：成本恒为有限非负数；预算为 `NaN` 时 `cost > remaining` 恒为 false，等价于不限
/// 预算，负预算则一个也不选。正常路径由 `codemode/index.rs` 的 settings 读取过滤掉非有限值。
fn select_catalog(groups: &[CatalogGroup], budget: Option<f64>) -> HashSet<String> {
    let Some(mut remaining) = budget else {
        return groups
            .iter()
            .flat_map(|group| group.entries.iter().map(|entry| entry.name.clone()))
            .collect();
    };
    let mut queues: Vec<Vec<&CatalogEntry>> = groups
        .iter()
        .map(|group| {
            let mut entries: Vec<&CatalogEntry> = group.entries.iter().collect();
            entries.sort_by(|a, b| {
                a.cost
                    .partial_cmp(&b.cost)
                    .unwrap_or(std::cmp::Ordering::Equal)
            });
            entries
        })
        .collect();
    let mut shown = HashSet::new();
    loop {
        let mut active: Vec<Vec<&CatalogEntry>> = queues
            .into_iter()
            .filter(|queue| !queue.is_empty())
            .collect();
        if active.is_empty() {
            break;
        }
        let mut next_queues = Vec::with_capacity(active.len());
        for mut queue in active.drain(..) {
            let next = queue[0];
            if next.cost > remaining {
                continue;
            }
            remaining -= next.cost;
            shown.insert(next.name.clone());
            queue.remove(0);
            if !queue.is_empty() {
                next_queues.push(queue);
            }
        }
        if next_queues.is_empty() {
            break;
        }
        queues = next_queues;
    }
    shown
}

/// 模型可见的描述：helper 列表、查找未列出工具的提示、`models` API，以及每个列出工具的一段
/// （按 namespace 分组）。deferred 工具从不列出，也完全不影响描述，所以在工具晚注册
/// （例如 MCP 服务器连接）时描述保持不变。工具 section 受 `inlineBudget` 限制。
///
/// 候选来自 [`get_codemode_callable_tools`]（`codemode` 自身被排除），再减去 `deferred`
/// 集合。生成顺序固定为：`DESCRIPTION_INTRO`、globals、`Nested tools:` + 各组段落，段落之间
/// 用一个空行连接。没有任何可列出工具时提前返回前两段（不含 `Nested tools:`）。
///
/// 分组规则：先按 `namespaces` 查每个工具，缺少条目的归入无 namespace 组；组顺序是
/// “无 namespace 组在前，其余按 `locale_compare` 排序”；组内工具保持调用方给的顺序
/// （成本只用于挑选，不用于排序展示）。有 namespace 的组即使一个工具都没被预算选中，也会
/// 输出标题并标 `(tools not listed)`。
pub fn create_codemode_description(
    tools: &[AgentTool],
    options: &CodemodeDescriptionOptions,
) -> String {
    // 阶段 1：确定候选工具（排除 codemode 自身与 deferred），并生成脚本侧声明。
    let declarations: Vec<CodemodeTool> = get_codemode_callable_tools(tools)
        .iter()
        .filter(|tool| {
            !options
                .deferred
                .as_ref()
                .is_some_and(|deferred| deferred.contains(&tool.name))
        })
        .map(|tool| {
            let guidelines = options
                .guidelines
                .as_ref()
                .and_then(|guidelines| guidelines.get(&tool.name));
            to_codemode_declaration(tool, guidelines.map_or(&[][..], Vec::as_slice))
        })
        .collect();
    // 阶段 2：按 namespace 分组并渲染每个工具的 section，同时量出 UTF-16 成本。
    // 无 namespace 的默认组先插入，作为组排序的固定基准。
    let mut groups: IndexMap<String, CatalogGroup> = IndexMap::new();
    groups.insert(
        String::new(),
        CatalogGroup {
            namespace: None,
            entries: Vec::new(),
        },
    );
    for declaration in &declarations {
        let namespace = options
            .namespaces
            .as_ref()
            .and_then(|namespaces| namespaces.get(&declaration.name))
            .cloned();
        let key = namespace
            .as_ref()
            .map_or_else(String::new, |namespace| format!("ns:{}", namespace.name));
        let group = groups.entry(key).or_insert_with(|| CatalogGroup {
            namespace: namespace.clone(),
            entries: Vec::new(),
        });
        let section = render_tool_section(declaration);
        let cost = (utf16_len(&section) as f64 / CHARS_PER_TOKEN).ceil();
        group.entries.push(CatalogEntry {
            name: declaration.name.clone(),
            section,
            cost,
        });
    }
    // 阶段 3：组排序（无 namespace 在前，其余按 localeCompare），再按预算挑选要展示的项。
    let mut ordered: Vec<CatalogGroup> = groups.into_values().collect();
    ordered.sort_by(|a, b| match (&a.namespace, &b.namespace) {
        (None, None) => std::cmp::Ordering::Equal,
        (None, Some(_)) => std::cmp::Ordering::Less,
        (Some(_), None) => std::cmp::Ordering::Greater,
        (Some(a_namespace), Some(b_namespace)) => {
            kiana_tui::autocomplete::locale_compare(&a_namespace.name, &b_namespace.name)
        }
    });
    let shown = select_catalog(&ordered, options.inline_budget);

    let mut sections = vec![
        DESCRIPTION_INTRO.to_string(),
        describe_globals(options.models),
    ];
    // 阶段 4：拼装文本。没有可列出的工具时连 "Nested tools:" 标题一起省略。
    if declarations.is_empty() {
        return sections.join("\n\n");
    }

    let mut tool_sections = vec!["Nested tools:".to_string()];
    for group in &ordered {
        let visible: Vec<&CatalogEntry> = group
            .entries
            .iter()
            .filter(|entry| shown.contains(&entry.name))
            .collect();
        if let Some(namespace) = &group.namespace {
            let listing = if visible.len() == group.entries.len() {
                ""
            } else if visible.is_empty() {
                " (tools not listed)"
            } else {
                " (some tools not listed)"
            };
            let description = namespace
                .description
                .as_deref()
                .map(kiana_json::js_string::js_trim)
                .filter(|description| !description.is_empty());
            tool_sections.push(format!(
                "## {}{listing}{}",
                namespace.name,
                description.map_or_else(String::new, |description| format!("\n{description}"))
            ));
        }
        for entry in visible {
            tool_sections.push(entry.section.clone());
        }
    }
    sections.push(tool_sections.join("\n\n"));
    sections.join("\n\n")
}

/// JS 的 `string.length`（UTF-16 code unit）。
///
/// 预算成本按它而不是字节数/`chars` 计：emoji 等非 BMP 字符在 JS 里占 2 个码元，用字节数会
/// 高估、用 `chars` 会低估，进而让描述裁剪与 TS 产生差异。
fn utf16_len(value: &str) -> usize {
    value.encode_utf16().count()
}

/// 对象 schema 的字段名，`{ output, exit_code, full_output_path? }`，对象字段下探 `depth` 层。
///
/// 只处理 `type == "object"` 且带 `properties` 的 schema，其它形状返回 `None`（调用方退回
/// 通用类型渲染）。缺少 `required` 时所有字段都标 `?`；`required` 里的非字符串项被忽略。
/// `properties` 是数组时按 `Object.entries` 的字符串索引键处理（见函数内注释与测试
/// `output_fields_accept_array_properties_like_javascript`）。
fn describe_fields(schema: &JsonValue, depth: usize) -> Option<String> {
    let schema = schema.as_object()?;
    if schema.get("type").and_then(JsonValue::as_str) != Some("object") {
        return None;
    }
    let properties = schema.get("properties")?;
    // TS 只检查 `typeof properties === "object" && properties !== null`，数组也通过；
    // `Object.entries` 对数组给出字符串索引键（`[]` → 无字段，渲染成 `{  }`）。
    let fields_values: Vec<(String, &JsonValue)> = match properties {
        JsonValue::Object(object) => object
            .iter()
            .map(|(name, field)| (name.clone(), field))
            .collect(),
        JsonValue::Array(array) => array
            .iter()
            .enumerate()
            .map(|(index, field)| (index.to_string(), field))
            .collect(),
        _ => return None,
    };
    let required: HashSet<&str> = schema
        .get("required")
        .and_then(JsonValue::as_array)
        .map(|required| {
            required
                .iter()
                .filter_map(JsonValue::as_str)
                .collect::<HashSet<_>>()
        })
        .unwrap_or_default();
    let fields: Vec<String> = fields_values
        .into_iter()
        .map(|(name, field)| {
            let nested = if depth > 0 {
                describe_fields(field, depth - 1)
            } else {
                None
            };
            let optional = if required.contains(name.as_str()) {
                ""
            } else {
                "?"
            };
            match nested {
                Some(nested) => format!("{name}{optional}: {nested}"),
                None => format!("{name}{optional}"),
            }
        })
        .collect();
    Some(format!("{{ {} }}", fields.join(", ")))
}

/// JS `String.prototype.replace(/\s+/g, " ")` 的空格/换行折叠。
///
/// 每一段连续的 JS 空白（含换行、制表与全角空格）替换成一个 ASCII 空格；首尾空白也保留为
/// 一个空格，不做 trim。用于把可能是多行的类型渲染压成一行放进描述。
fn collapse_whitespace(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut in_space = false;
    for character in text.chars() {
        if is_js_whitespace(character) {
            in_space = true;
        } else {
            if in_space {
                out.push(' ');
                in_space = false;
            }
            out.push(character);
        }
    }
    if in_space {
        out.push(' ');
    }
    out
}

/// JS 正则 `\s` 的字符集合（ECMA-262）。
///
/// 包含 NBSP、各类 Unicode 空格、行/段分隔符与 BOM（U+FEFF）；不包含零宽空格 U+200B。
/// `char::is_whitespace` 的集合与此不同，不能替代。
fn is_js_whitespace(character: char) -> bool {
    matches!(
        character,
        '\u{0009}'
            | '\u{000a}'
            | '\u{000b}'
            | '\u{000c}'
            | '\u{000d}'
            | '\u{0020}'
            | '\u{00a0}'
            | '\u{1680}'
            | '\u{2000}'
            ..='\u{200a}'
                | '\u{2028}'
                | '\u{2029}'
                | '\u{202f}'
                | '\u{205f}'
                | '\u{3000}'
                | '\u{feff}'
    )
}

/// 脚本调用解析成什么：`a string`、对象字段名，或其它类型渲染出的类型。
///
/// 三分支：
/// - 渲染结果恰好是 `string` → 用英文短语 `a string`（而不是反引号包住的类型名）；
/// - 是 object schema 且 `properties` 为对象/数组 → 展开成 `` `{ 字段, 可选字段? }` ``
///   （`depth = 1`，再展开一层子对象）；
/// - 其它 → 把通用类型渲染折叠空白后放进反引号。
fn describe_output(schema: &JsonValue) -> String {
    let rendered = render_tool_output_type(Some(schema));
    if rendered == "string" {
        return "a string".to_string();
    }
    match describe_fields(schema, 1) {
        Some(fields) => format!("`{fields}`"),
        None => format!("`{}`", collapse_whitespace(&rendered)),
    }
}

/// 已声明工具的描述后接脚本如何调用它、以及调用解析成什么。
///
/// 只用于 `on` 模式下已声明（active）的工具：它们的正常描述已经展示了参数，这里补一句
/// `Codemode: tools.<id>(args) resolves to ...`，让模型知道同一个工具也能在脚本里用。
/// 传入空 guideline，避免把 prompt 指引重复两遍。`to_codemode_declaration` 保证输出 schema
/// 非空，所以末尾的 `expect` 不会触发。
fn describe_script_call(tool: &AgentTool) -> String {
    let declaration = to_codemode_declaration(tool, &[]);
    format!(
        "{}\n\nCodemode: `tools.{}(args)` resolves to {}.",
        kiana_json::js_string::js_trim(&tool.description),
        to_codemode_identifier(&tool.name),
        describe_output(
            declaration
                .output_schema
                .as_ref()
                .expect("declaration always has an output schema")
        )
    )
}

/// codemode 工具如何呈现既已声明又可从脚本调用的工具：
/// - `on`：它们的描述说明脚本如何调用；codemode 描述只列出没有 `direct` exposure 的可调用工具。
/// - `only`：codemode 描述列出全部可调用工具；请求省略 active `direct` 工具的声明。
///
/// 按 exposure（而不是 active 集合）列出，使 `tool_search` 加载工具时 codemode 描述保持不变。
///
/// 返回值只有两项：`descriptions`（工具名 → 新描述）与 `hidden_declarations`（要从模型声明里
/// 省略的工具）。`on` 模式下，每个“已声明且可调用”的工具的描述被换成
/// `describe_script_call` 的结果（原描述 + 脚本调用说明），同时 `listed` 排除所有 `direct`
/// exposure，避免同一工具重复出现。`only` 模式下 `listed` 是全部可调用工具，其中已声明
/// （active）的 direct 工具同时进入 `hidden_declarations`：它们仍然 active、仍可通过
/// `ctx.execute_tool()` 调用，只是不再单独声明，改由 codemode 的描述承载。
///
/// 两种模式都会替换 `codemode` 自己的描述；预算来自 `get_inline_budget`，缺省用
/// [`DEFAULT_CODEMODE_INLINE_BUDGET`]。
///
/// 本函数不返回 `Result`。会话对每个 `prepare_loadout` hook 使用 `catch_unwind`：这里的 panic
/// 会被记成扩展错误事件，本次改动全部丢弃；该次 loadout 重算会用工具注册表里的原始描述继续
/// （改写结果不写回注册表，不会保留上一次的描述）。
pub fn prepare_codemode_loadout(
    loadout: &dyn ToolLoadout,
    options: &CodemodeToolOptions,
) -> ToolLoadoutChanges {
    // 每次计算 loadout 都重新读模式；未提供回调时用 settings 默认值（on）。
    let mode = options
        .get_mode
        .as_ref()
        .map_or(SETTINGS_DEFAULTS.codemode.mode, |get_mode| get_mode());
    let is_direct = |name: &str| loadout.get_exposure(name) == ToolExposure::Direct;
    let callable = get_codemode_callable_tools(loadout.callable());
    let callable_names: HashSet<&str> = callable.iter().map(|tool| tool.name.as_str()).collect();
    let mut descriptions: IndexMap<String, String> = IndexMap::new();
    // on：给已声明且可调用的工具补一段“脚本里如何调用”，不区分它自己的 exposure。
    if mode == CodemodeMode::On {
        for tool in loadout.declared() {
            if callable_names.contains(tool.name.as_str()) {
                descriptions.insert(tool.name.clone(), describe_script_call(tool));
            }
        }
    }
    // on：direct 工具已在自己的描述里说明脚本用法，不再重复列出；only：全部列出。
    let listed: Vec<AgentTool> = if mode == CodemodeMode::Only {
        callable.clone()
    } else {
        callable
            .iter()
            .filter(|tool| !is_direct(&tool.name))
            .cloned()
            .collect()
    };
    // 只为 listed 收集元数据；deferred 工具虽然也在 listed 里，稍后会被描述生成器过滤掉。
    let mut namespaces: IndexMap<String, ToolNamespace> = IndexMap::new();
    for tool in &listed {
        if let Some(namespace) = loadout.get_namespace(&tool.name) {
            namespaces.insert(tool.name.clone(), namespace.clone());
        }
    }
    let mut guidelines: IndexMap<String, Vec<String>> = IndexMap::new();
    for tool in &listed {
        guidelines.insert(
            tool.name.clone(),
            loadout.get_prompt_guidelines(&tool.name).to_vec(),
        );
    }
    // deferred 工具可调用但不进描述；按 exposure 判定，与 active 集合无关。
    let deferred: HashSet<String> = listed
        .iter()
        .filter(|tool| loadout.get_exposure(&tool.name) == ToolExposure::Deferred)
        .map(|tool| tool.name.clone())
        .collect();
    // 未配置回调或返回 None 时回落到默认预算。
    let inline_budget = options
        .get_inline_budget
        .as_ref()
        .and_then(|get_inline_budget| get_inline_budget())
        .unwrap_or(DEFAULT_CODEMODE_INLINE_BUDGET);
    // codemode 自身的描述总是被替换：即使没有可列出的工具，intro/globals 也会重新生成。
    descriptions.insert(
        CODEMODE_TOOL_NAME.to_string(),
        create_codemode_description(
            &listed,
            &CodemodeDescriptionOptions {
                models: options.models == Some(true),
                namespaces: Some(namespaces),
                deferred: Some(deferred),
                guidelines: Some(guidelines),
                inline_budget: Some(inline_budget),
            },
        ),
    );
    let declared_names: HashSet<&str> = loadout
        .declared()
        .iter()
        .map(|tool| tool.name.as_str())
        .collect();
    // only：隐藏 active direct 工具的独立声明，让它们只通过 codemode 描述出现。
    let hidden_declarations = if mode == CodemodeMode::Only {
        callable
            .iter()
            .filter(|tool| is_direct(&tool.name) && declared_names.contains(tool.name.as_str()))
            .map(|tool| tool.name.clone())
            .collect()
    } else {
        Vec::new()
    };
    ToolLoadoutChanges {
        descriptions: Some(descriptions),
        hidden_declarations: Some(hidden_declarations),
    }
}

/// 脚本里会列出/查找工具与 namespace 的全局名；脚本随后可以按任意名字调用它们。
const DISCOVERY_GLOBALS: [&str; 4] = [
    "searchTools",
    "describeNamespace",
    "describeTool",
    "ALL_TOOLS",
];

/// 脚本可以调用的工具（`ToolDefinition.reachesTools`）：使用发现全局时返回 `All`，
/// 否则返回源码里的全部 `\w+` 词（JS 的 `\w` 是 ASCII `[A-Za-z0-9_]`）。
///
/// 参数是脚本源码值（调用方从工具调用 input 的 `code` 字段取出）；不是字符串时返回空
/// `Names`（没有可识别的名字）。分词不做 JS 词法分析：字符串字面量或注释里的单词也会
/// 被收集，任何 `DISCOVERY_GLOBALS` 命中（必须作为完整单词，`mysearchTools` 不算）就返回
/// `All`。这是偏保守的方向：例如同 crate 的 MCP 扩展会用该结果决定执行前要等待哪些已就绪的
/// server，`All` 会让它等待全部 server。
pub fn get_script_reach(code: &JsonValue) -> ReachesToolsResult {
    let Some(code) = code.as_str() else {
        return ReachesToolsResult::Names(Vec::new());
    };
    let mut words = Vec::new();
    let mut current = String::new();
    // 按 ASCII 单词字符切分（等价 JS 的 `/\w+/g`）；非 ASCII 字符只起分隔作用，不进入词表。
    for character in code.chars() {
        if character.is_ascii_alphanumeric() || character == '_' {
            current.push(character);
        } else if !current.is_empty() {
            words.push(std::mem::take(&mut current));
        }
    }
    if !current.is_empty() {
        words.push(current);
    }
    // 任一发现全局以完整单词出现，就无法静态枚举触达范围，只能保守地报 All。
    if words
        .iter()
        .any(|word| DISCOVERY_GLOBALS.contains(&word.as_str()))
    {
        ReachesToolsResult::All
    } else {
        ReachesToolsResult::Names(words)
    }
}

/// `createCodemodeToolDefinition(options = {})`。
///
/// 构造时还没有工具列表，所以 `description` 只含 intro 与 globals；真正的工具 section 由
/// [`prepare_codemode_loadout`] 在注册后按会话 loadout 重算。`reaches_tools` 读取调用的
/// `code` 字段；受限采样只填 `openai_lark`（语法见 `CODEMODE_SOURCE_GRAMMAR`），
/// `openai_regex` 留空；渲染器来自 `codemode_renderers`。
pub fn create_codemode_tool_definition(options: CodemodeToolOptions) -> ToolDefinition {
    let models = options.models == Some(true);
    let prepare_options = options.clone();
    let execute_options = options;
    // 初始描述没有工具可列，只有 intro/globals；prepare_loadout 之后会被整体替换。
    let description = create_codemode_description(
        &[],
        &CodemodeDescriptionOptions {
            models,
            ..CodemodeDescriptionOptions::default()
        },
    );
    // 只做静态词扫描：非对象输入或缺少 code 时得到空 Names，交给消费方按未知处理。
    let reaches_tools: ReachesToolsFn = Arc::new(|input: &JsonValue| {
        get_script_reach(input.get("code").unwrap_or(&JsonValue::Null))
    });
    // 闭包捕获 options 的克隆；每次 loadout 变化都会调用。
    let prepare_loadout: PrepareLoadoutFn = Arc::new(move |loadout: &dyn ToolLoadout| {
        Some(prepare_codemode_loadout(loadout, &prepare_options))
    });
    // 只提供 OpenAI 的 Lark 语法；regex 变体保持缺省。
    let mut variants = GrammarVariants {
        openai_lark: None,
        openai_regex: None,
    };
    variants.openai_lark = Some(CODEMODE_SOURCE_GRAMMAR.to_string());
    let renderers = crate::codemode::renderer::codemode_renderers();
    ToolDefinition {
        name: CODEMODE_TOOL_NAME.to_string(),
        label: CODEMODE_TOOL_NAME.to_string(),
        description,
        prompt_snippet: Some(codemode_tool_system_prompt_contribution.snippet.to_string()),
        prompt_guidelines: Some(
            codemode_tool_system_prompt_contribution
                .guidelines
                .iter()
                .map(|guideline| (*guideline).to_string())
                .collect(),
        ),
        parameters: codemode_schema(),
        // 有能力的模型会把脚本写成原始文本而不是 JSON 转义字符串。
        constrained_sampling: Some(ConstrainedSampling::Config(
            ConstrainedSamplingConfig::Grammar { variants },
        )),
        render_shell: renderers.render_shell,
        prepare_arguments: None,
        output_schema: None,
        // 脚本不能启动别的脚本。
        exposure: Some(ToolExposure::ModelOnly),
        namespace: None,
        reaches_tools: Some(reaches_tools),
        annotations: None,
        default_active: None,
        prepare_loadout: Some(prepare_loadout),
        execution_mode: None,
        execute: Arc::new(CodemodeToolExecutable {
            options: execute_options,
        }),
        render_call: renderers.render_call,
        render_result: renderers.render_result,
    }
}

/// `codemode` 的执行器。
///
/// 持有构造时的 [`CodemodeToolOptions`]。每次 `execute` 先把调用方借用的 `tool_call_id` /
/// `signal` 复制成 future 自有的值，再把执行委托给
/// `crate::codemode::execute::execute_codemode`；占位上下文会被还原成 `None`（见函数内注释）。
struct CodemodeToolExecutable {
    options: CodemodeToolOptions,
}

impl ToolDefinitionExecute for CodemodeToolExecutable {
    fn execute<'a>(
        &'a self,
        tool_call_id: &'a str,
        params: JsonValue,
        signal: Option<&'a CancellationToken>,
        on_update: Option<AgentToolUpdateCallback>,
        ctx: Arc<dyn ExtensionToolContextApi>,
    ) -> BoxFuture<'a, Result<AgentToolResult<JsonValue>, AgentToolError>> {
        let options = self.options.clone();
        let tool_call_id = tool_call_id.to_string();
        let signal = signal.cloned();
        Box::pin(async move {
            // TS 的 `ctx === undefined`（例如 `createCodemodeTool()` 直接执行）不能用 Rust 的
            // 非 Option 上下文表达；`wrap_tool_definition` 在没有工厂时注入占位上下文，
            // 这里把它还原成 None，否则脚本会拿到 `models` 命名空间与空工具集。
            let ctx = if kiana_tools::definition::is_missing_tool_context(&ctx) {
                None
            } else {
                Some(ctx)
            };
            crate::codemode::execute::execute_codemode(
                tool_call_id,
                params,
                signal,
                on_update,
                ctx,
                options,
            )
            .await
        })
    }
}

/// 把 codemode 工具创建成 `AgentTool`（SDK/测试直接使用）。描述列出给定工具。
///
/// 描述使用缺省选项：不按 namespace 分组、不附加 guideline、也不受 inline 预算限制。
///
/// 包装时没有上下文工厂，执行阶段拿到的是 `missing_tool_context`，执行器会把它还原成
/// `None`：脚本没有可调用的工具、`store()` 从空开始，即使 `options.models == true` 也不会
/// 注入 `models` 全局（测试 `create_codemode_tool_without_a_session_does_not_declare_models`
/// 锁定）。因此描述里列出的工具在这个入口下只是展示：脚本里的 `tools` 对象没有成员，访问
/// 具体工具会在 JS 层失败。
pub fn create_codemode_tool(tools: &[AgentTool], options: CodemodeToolOptions) -> AgentTool {
    let models = options.models == Some(true);
    let definition = create_codemode_tool_definition(options);
    let description = create_codemode_description(
        tools,
        &CodemodeDescriptionOptions {
            models,
            ..CodemodeDescriptionOptions::default()
        },
    );
    let mut tool = kiana_tools::definition::wrap_tool_definition(definition, None);
    // TS: `Object.assign(tool, { description })`。
    tool.description = description;
    tool
}

// 锁定与 TS 对齐的可观察行为：schema 的键顺序、`reachesTools` 的分词、数组形式的
// `properties` 处理，以及声明的默认输出 schema 与描述拼接规则。
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    struct NoopExecute;

    impl kiana_agent_core::AgentToolExecute for NoopExecute {
        fn execute<'a>(
            &'a self,
            _tool_call_id: &'a str,
            _params: JsonValue,
            _signal: Option<&'a CancellationToken>,
            _on_update: Option<AgentToolUpdateCallback>,
        ) -> BoxFuture<'a, Result<AgentToolResult<JsonValue>, AgentToolError>> {
            Box::pin(async {
                Ok(AgentToolResult {
                    content: Vec::new(),
                    details: JsonValue::Null,
                    structured_content: None,
                    usage: None,
                    is_error: None,
                    terminate: None,
                })
            })
        }
    }

    #[test]
    fn schemas_match_the_typebox_output_order() {
        assert_eq!(
            kiana_json::js_object::stringify(&codemode_schema()),
            "{\"type\":\"object\",\"required\":[\"code\"],\"properties\":{\"code\":{\"type\":\"string\",\"description\":\"Raw JavaScript source.\"}}}"
        );
        assert_eq!(codemode_schema()["required"], json!(["code"]));
    }

    #[test]
    fn script_reach_uses_discovery_globals_or_words() {
        assert_eq!(
            get_script_reach(&json!("await tools.mcp__docs__search({ q: 1 })")),
            ReachesToolsResult::Names(vec![
                "await".to_string(),
                "tools".to_string(),
                "mcp__docs__search".to_string(),
                "q".to_string(),
                "1".to_string(),
            ])
        );
        // 任一发现全局 → 全部工具。
        assert_eq!(
            get_script_reach(&json!("ALL_TOOLS.map(t => t.name)")),
            ReachesToolsResult::All
        );
        assert_eq!(
            get_script_reach(&json!("await searchTools('docs')")),
            ReachesToolsResult::All
        );
        assert_eq!(
            get_script_reach(&json!("describeNamespace('dev_radius')")),
            ReachesToolsResult::All
        );
        assert_eq!(
            get_script_reach(&json!(null)),
            ReachesToolsResult::Names(Vec::new())
        );
        assert_eq!(
            get_script_reach(&json!(123)),
            ReachesToolsResult::Names(Vec::new())
        );
        // 作为更长单词的一部分不算命中。
        assert_eq!(
            get_script_reach(&json!("mysearchTools")),
            ReachesToolsResult::Names(vec!["mysearchTools".to_string()])
        );
    }

    #[test]
    fn output_fields_accept_array_properties_like_javascript() {
        // TS 只要求 `typeof properties === "object" && properties !== null`，数组也通过；
        // `Object.entries` 给出字符串索引键。`properties: []` 因此渲染成 `{  }`，而不是
        // 退回 `renderToolOutputType` 的 `{ [key: string]: unknown; }`。
        assert_eq!(
            describe_output(&json!({"type": "object", "properties": []})),
            "`{  }`"
        );
        assert_eq!(
            describe_output(&json!({
                "type": "object",
                "properties": [{"type": "string"}],
                "required": ["0"]
            })),
            "`{ 0 }`"
        );
    }

    #[test]
    fn declaration_defaults_to_the_string_output_schema() {
        let tool = kiana_agent_core::AgentTool {
            name: "noop".to_string(),
            label: "Noop".to_string(),
            description: "  keep spaces  ".to_string(),
            parameters: json!({"type": "object"}),
            constrained_sampling: None,
            output_schema: None,
            prepare_arguments: None,
            execution_mode: None,
            replay: None,
            execute: std::sync::Arc::new(NoopExecute),
        };
        let declaration = to_codemode_declaration(&tool, &[]);
        assert_eq!(declaration.description.as_deref(), Some("  keep spaces  "));
        assert_eq!(declaration.output_schema, Some(json!({"type": "string"})));
        let declaration = to_codemode_declaration(
            &tool,
            &[
                "  Use it.  ".to_string(),
                "   ".to_string(),
                "Also.".to_string(),
            ],
        );
        assert_eq!(
            declaration.description.as_deref(),
            Some("keep spaces\n\n- Use it.\n- Also.")
        );
    }
}
