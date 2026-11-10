//! ToolDefinition/ToolLoadout/ToolNamespace/ToolRenderContext 与各工具 Input/Details。
//!
//! TS 源：`core/extensions/types.ts`（约 458–694 行）与
//! `core/tools/{bash,powershell,read,edit,write,grep,find,ls}.ts`、`core/tools/truncate.ts`。
//!
//! # 契约上移
//!
//! `ARCHITECTURE.md` §3.4/§5 决定：具体工具的 `Input`/`Details` 与 `TruncationResult` 定义在
//! 本模块（`ToolCallEvent`/`ToolResultEvent` 引用它们，而 `kiana-tools`（L8）依赖本 crate（L7））。
//! `kiana-tools::bash/read/edit/...` 与 `kiana-tools::truncate` 只再导出，不得复制。
//!
//! # 与运行时工具的关系
//!
//! [`ToolDefinition`] 是扩展/UI 视角的完整定义（20 个字段，含 prompt 元数据与渲染器）；
//! 运行时使用的是 `kiana-agent-core` 的 `AgentTool`（瘦视图：只保留 name、label、description、
//! parameters、outputSchema、constrainedSampling、prepareArguments、executionMode、execute，
//! 外加包装器固定填 `None` 的 replay）。
//! `kiana-tools::definition::wrap_tool_definition` 负责单向投影（会话经
//! `kiana-extensions::wrapper::wrap_registered_tools` 调用它），投影结果是发给模型的工具声明。
//! 修改 `ToolDefinition` 中不进入 `AgentTool` 的字段不会改变模型看到的参数 schema，
//! 但仍可能影响会话行为（例如 `exposure`/`defaultActive`/`prepareLoadout` 决定声明与激活）；
//! 若要新增会影响模型声明的字段，必须同时修改那个包装函数。
//!
//! # 键序与数字
//!
//! 各 `*ToolInput`/`*ToolDetails`/`TruncationResult` 的字段顺序就是 TS schema 与
//! `JSON.stringify` 的键顺序；这些类型会写进工具结果事件与会话文件
//! （`tests/serde_contracts.rs` 用字符串比较钉住键序）。其中 `timeout`、`offset`、`limit`、
//! `context` 与三个 `*_limit_reached` 对应 JS `number`：用 `f64` 接收负数/小数，并通过
//! `js_number::serialize_option` 按 ECMAScript 文本写出，不要换成整数类型。

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use kiana_agent_core::{
    AgentToolError, AgentToolResult, AgentToolUpdateCallback, ToolExecutionMode,
};
use kiana_ai::types::{ConstrainedSampling, JsonValue};
use kiana_theme::theme::Theme;
use kiana_tui::tui::Component;
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

use crate::BoxFuture;
use crate::js_number;
use crate::types::ExtensionToolContextApi;

/// 渲染壳类型。
///
/// TS: `types.ts#ToolDefinition.renderShell`（`"default" | "self"`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RenderShell {
    /// `"default"`：`ToolExecutionComponent` 画标准彩色外壳。
    #[serde(rename = "default")]
    Default,
    /// `"self"`：工具自己渲染框架（`outputPad` 由渲染器自行应用）。
    #[serde(rename = "self")]
    SelfRender,
}

/// 模型如何触达一个工具。
///
/// TS: `types.ts#ToolExposure`，逐字 `"direct" | "model-only" | "codemode" | "deferred" | "hidden"`。
/// 变体含多词，逐变体 `serde(rename)`（不能用 `rename_all = "lowercase"`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum ToolExposure {
    /// `"direct"`（默认）：active 时声明给模型，且 active 时可调用。
    #[default]
    #[serde(rename = "direct")]
    Direct,
    /// `"model-only"`：active 时声明给模型，永不可调用。
    #[serde(rename = "model-only")]
    ModelOnly,
    /// `"codemode"`：注册后始终可调用；除非显式激活，否则不声明给模型。
    #[serde(rename = "codemode")]
    Codemode,
    /// `"deferred"`：同 `codemode`，但 codemode 工具不列出它；`tool_search` 能找到它。
    #[serde(rename = "deferred")]
    Deferred,
    /// `"hidden"`：已注册但不可达。
    #[serde(rename = "hidden")]
    Hidden,
}

/// 工具行为提示（语义同 MCP tool annotations；作者提供、不被验证）。
///
/// TS: `types.ts#ToolAnnotations`。字段顺序即 TS 声明顺序。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolAnnotations {
    /// 工具不修改环境。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub read_only_hint: Option<bool>,
    /// 工具可能删除/覆盖数据（非只读时有意义）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub destructive_hint: Option<bool>,
    /// 相同参数重复调用没有进一步效果（非只读时有意义）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idempotent_hint: Option<bool>,
    /// 工具触达开放的外部实体集合（如 web）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub open_world_hint: Option<bool>,
}

/// 一组相关工具（例如一个 MCP server 的工具）。
///
/// TS: `types.ts#ToolNamespace`。字段顺序 `name`、`aliases`、`description`、`instructions`。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolNamespace {
    /// 命名空间名，例如 `mcp__docs`。
    pub name: String,
    /// 其它引用该命名空间的名字（codemode 的 `describeNamespace()`/`searchTools()` 按名查找）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub aliases: Option<Vec<String>>,
    /// 在模型可见的工具列表里随组显示一次的短摘要。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// 更长的使用指引（如 MCP server instructions），不进入工具列表。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instructions: Option<String>,
}

/// [`ToolDefinition::prepare_loadout`] 对模型所见内容的修改。
///
/// TS: `types.ts#ToolLoadoutChanges`。`descriptions` 与 `hiddenDeclarations` 均可选。
/// 映射使用 [`IndexMap`](indexmap::IndexMap) 保留插入序（spec §5.2 的 `BTreeMap` 草图被
/// `ARCHITECTURE.md` §6.3 的键序规则取代）。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolLoadoutChanges {
    /// 按工具名给出的、模型可见的描述。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub descriptions: Option<indexmap::IndexMap<String, String>>,
    /// 声明中省略的工具（它们仍 active、仍可调用，transcript 仍声明它们）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hidden_declarations: Option<Vec<String>>,
}

/// 会话的工具视图（`prepareLoadout` 看到的内容）。
///
/// TS: `types.ts#ToolLoadout`。只读视图 + 三个按名查询方法。
///
/// 快照语义：会话在每次派发 `prepare_loadout` 前构造该视图，hook 只能读到当前快照。
/// 修改列表的请求通过返回 [`ToolLoadoutChanges`] 表达，不能在这里直接改。
pub trait ToolLoadout {
    /// 声明给模型的工具（active），按顺序，保留原始描述。
    ///
    /// 这是 `prepare_loadout` 看到的候选集合：之后 hook 返回的 `descriptions` 覆盖与
    /// `hiddenDeclarations` 都不在这个切片里体现（`declared()` 始终是覆盖前的快照）。
    fn declared(&self) -> &[kiana_agent_core::AgentTool];
    /// 可通过 `ctx.executeTool()` 调用的工具。
    ///
    /// 会话实现按曝光度过滤：`codemode`/`deferred` 始终可调用，`direct` 仅 active 时可调用，
    /// `model-only`/`hidden` 不可调用。
    fn callable(&self) -> &[kiana_agent_core::AgentTool];
    /// 全部已注册工具。
    ///
    /// 会话实现按注册顺序返回（内部注册表是 `IndexMap`），包含 hidden/model-only 的工具，
    /// 不受当前 active 集合影响。
    fn registered(&self) -> &[kiana_agent_core::AgentTool];
    /// 工具的可达性。
    ///
    /// 未知名字在会话实现中回退为 [`ToolExposure::Direct`]（与「未声明 exposure 即 direct」一致）。
    fn get_exposure(&self, name: &str) -> ToolExposure;
    /// 工具所属命名空间。
    fn get_namespace(&self, name: &str) -> Option<&ToolNamespace>;
    /// 工具的 `promptGuidelines`；hidden declaration 的工具不贡献指引。
    ///
    /// 本方法只按名字查快照；会话在构建系统提示时另用 `hidden_tools` 过滤，
    /// 因此 `hidden_declarations` 里的工具即使这里返回非空，也不会进入提示的 Guidelines 段。
    fn get_prompt_guidelines(&self, name: &str) -> &[String];
}

/// 工具结果的渲染选项。TS: `types.ts#ToolRenderResultOptions`。
///
/// 由 UI 在调用 `render_result` 时按该工具行当前状态构造；渲染器只读，不负责维护状态。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolRenderResultOptions {
    /// 结果视图是否展开。
    pub expanded: bool,
    /// 是否是部分/流式结果。
    pub is_partial: bool,
}

/// 工具行的共享渲染状态。
///
/// TS 的 `ToolRenderContext.state` 是渲染器自定义的 `TState`（默认 `any`）。
/// Rust 侧 `ToolRenderContext` 不做泛型（要放在 trait 对象后面传递），因此固定为
/// `Arc<Mutex<JsonValue>>`；渲染器用 JSON 保存自己的状态。
///
/// 与 `RefCell` 相比，`Arc<Mutex<_>>` 是 `Send + Sync` 的共享单元格，可以跨线程共享；
/// 持锁范围与锁的粒度由渲染器自己决定，本类型不提供额外保证（std `Mutex` 不可重入，
/// 同一线程在未释放时再次 `lock()` 可能死锁）。
pub type RenderState = Arc<Mutex<JsonValue>>;

/// 传给工具渲染器的上下文。
///
/// TS: `types.ts#ToolRenderContext<TState = any, TArgs = any>`。Rust 侧**去掉泛型**：
/// `args` 固定为 [`JsonValue`]，`state` 固定为 [`RenderState`]（见
/// `ARCHITECTURE.md` §5.2 的偏离说明）。14 个字段名/顺序是当前稳定契约，改动需同步测试。
pub struct ToolRenderContext {
    /// 当前工具调用参数；同一 tool call 的 call/result 渲染共享。
    pub args: JsonValue,
    /// 本次工具执行的唯一 id；同一 tool call 的 call/result 渲染稳定。
    pub tool_call_id: String,
    /// 只重绘该工具执行组件。
    ///
    /// 类型要求 `Send + Sync`：实现可能被后台计时器（耗时文本刷新）从非 UI 线程调用，
    /// 调用方负责把重绘请求转回 UI 线程。
    pub invalidate: Arc<dyn Fn() + Send + Sync>,
    /// 该渲染槽上一次返回的组件。
    ///
    /// 调用方在重绘时把上一帧缓存的组件交回，渲染器可据其复用内部状态；首帧或上次渲染
    /// 未成功缓存时为 `None`。
    pub last_component: Option<Box<dyn Component>>,
    /// 该工具行共享的渲染状态。
    pub state: RenderState,
    /// 该次执行的 cwd（§5.2 草图：`PathBuf`；转换到 JS 时用字符串）。
    pub cwd: PathBuf,
    /// 执行是否已开始。
    pub execution_started: bool,
    /// 参数是否已完整。
    pub args_complete: bool,
    /// 结果是否流式中。
    pub is_partial: bool,
    /// 结果视图是否展开。
    pub expanded: bool,
    /// TUI 当前是否显示内嵌图片。
    pub show_images: bool,
    /// 当前结果是否错误。
    pub is_error: bool,
    /// 最终结果的耗时（毫秒）；运行中、未运行或旧记录为 `None`（TS `undefined`，省略键）。
    ///
    /// TS 类型是 `number`；契约冻结为 `u64`，与 [`crate::events::ToolExecutionEndEvent`] 的
    /// `duration_ms` 保持同一量纲（调用方从同一个测量值填充两处，无需转换）。
    pub duration_ms: Option<u64>,
    /// `outputPad` 设置的水平内缩；`renderShell: "self"` 的渲染器自行应用。
    pub output_pad: usize,
}

/// 工具渲染器集合（`Pick<ToolDefinition, "renderShell" | "renderCall" | "renderResult">`）。
///
/// TS: `types.ts#ToolRenderers`。
///
/// 三个字段都是可选的：解析链返回的集合**整体生效**，不会与 base 逐字段合并；缺省的
/// `render_shell` 由消费方按 [`RenderShell::Default`] 处理。
#[derive(Clone)]
pub struct ToolRenderers {
    /// 渲染壳类型。
    pub render_shell: Option<RenderShell>,
    /// 工具调用显示。
    pub render_call: Option<RenderCallFn>,
    /// 工具结果显示。
    pub render_result: Option<RenderResultFn>,
}

// 手写 Debug：闭包字段无法 Debug，只打印 `Option::is_some`；保留字段名以便日志定位。
impl std::fmt::Debug for ToolRenderers {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ToolRenderers")
            .field("render_shell", &self.render_shell)
            .field("render_call", &self.render_call.is_some())
            .field("render_result", &self.render_result.is_some())
            .finish()
    }
}

/// 工具调用渲染函数。
///
/// TS: `(args: Static<TParams>, theme: Theme, context: ToolRenderContext<TState, Static<TParams>>) => Component`。
///
/// 用 `Arc` 以便 [`ToolRenderers`] 可 Clone；闭包要求 `Send + Sync`。返回值是该次渲染生成的
/// 组件树，所有权交给调用方；调用方可能缓存旧组件并在下一帧通过
/// `ToolRenderContext::last_component` 交回。
pub type RenderCallFn =
    Arc<dyn Fn(&JsonValue, &Theme, &ToolRenderContext) -> Box<dyn Component> + Send + Sync>;

/// 工具结果渲染函数。
///
/// TS: `(result: AgentToolResult<TDetails>, options: ToolRenderResultOptions, theme: Theme,
/// context: ToolRenderContext<...>) => Component`。
///
/// 与 [`RenderCallFn`] 一样是 `Arc` + `Send + Sync`；`options` 描述当前视图的展开/流式状态。
pub type RenderResultFn = Arc<
    dyn Fn(
            &AgentToolResult<JsonValue>,
            &ToolRenderResultOptions,
            &Theme,
            &ToolRenderContext,
        ) -> Box<dyn Component>
        + Send
        + Sync,
>;

/// 取“剩余渲染器”的回调（TS `next()`）。
///
/// 这是一个**借用**回调，只在 resolver 被调用的同步栈帧内有效；resolver 不能把
/// `&NextToolRenderersFn` 存进长期结构。需要延迟调用时，应立刻取走 `next()` 的结果，或由
/// 宿主桥接层（`kiana-extensions` 的 JS 桥）自行保存续延。
pub type NextToolRenderersFn = dyn Fn() -> Option<ToolRenderers>;

/// 选择工具调用如何绘制的 resolver。
///
/// TS: `types.ts#ToolRendererResolver`
/// （`(toolName, next) => ToolRenderers | undefined`）。resolver 按扩展加载顺序运行；
/// `next()` 返回剩余 resolver（再是已注册工具）会用的渲染器。
///
/// 解析规则：第一个返回 `Some` 的 resolver 短路，后面的 resolver 与 base 都不再运行；
/// `next()` 是惰性的，resolver 不调用它时 base 不会被求值（`kiana-extensions` 的
/// `tests/tool_renderers.rs` 锁定这些行为）。
pub type ToolRendererResolver =
    Arc<dyn Fn(&str, &NextToolRenderersFn) -> Option<ToolRenderers> + Send + Sync>;

/// [`ToolDefinition::reaches_tools`] 的结果。
///
/// TS: `"all" | readonly string[]`。`Names` 是输入中可能命名工具/命名空间的词。
///
/// 消费方把它当“提示”而非精确清单：`All` 表示无法静态枚举，`Names` 里的词由消费方解释
/// （MCP 扩展按“词是否包含命名空间”匹配；空列表在 MCP 扩展中匹配不到任何服务器）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReachesToolsResult {
    /// `"all"`：该调用可能触达全部工具。
    All,
    /// 可能触达的工具/命名空间名字。
    Names(Vec<String>),
}

/// 默认是空 `Names(Vec::new())`；现有消费方（MCP 扩展的 `reaches_server`）据此视为不触达
/// 任何工具。
impl Default for ReachesToolsResult {
    fn default() -> Self {
        ReachesToolsResult::Names(Vec::new())
    }
}

/// [`ToolDefinition::reaches_tools`] 的回调类型。
///
/// TS: `(input: Record<string, unknown>) => "all" | readonly string[]`。
pub type ReachesToolsFn = Arc<dyn Fn(&JsonValue) -> ReachesToolsResult + Send + Sync>;

/// [`ToolDefinition::prepare_loadout`] 的回调类型。
///
/// TS: `(loadout: ToolLoadout) => ToolLoadoutChanges | undefined`。
pub type PrepareLoadoutFn =
    Arc<dyn Fn(&dyn ToolLoadout) -> Option<ToolLoadoutChanges> + Send + Sync>;

/// [`ToolDefinition::prepare_arguments`] 的回调类型。
///
/// 与 `kiana-agent-core::AgentTool::prepare_arguments` 相同：返回 `None` 表示闭包返回了同一个对象
/// （就地改写保留），`Some` 表示返回了新值。
pub type PrepareArgumentsFn = Arc<dyn Fn(&mut JsonValue) -> Option<JsonValue> + Send + Sync>;

/// 工具执行入口（`ToolDefinition::execute`）。
///
/// TS: `execute(toolCallId, params, signal, onUpdate, ctx: ExtensionToolContext) =>
/// Promise<AgentToolResult<TDetails>>`。`TParams`/`TDetails` 被擦除：`params` 与
/// `AgentToolResult` 的 details 都是 [`JsonValue`]。
///
/// 返回 [`BoxFuture`]（`Send`）：工具执行会跨进 `kiana-tools`/`kiana-session` 的 async 管线。
pub trait ToolDefinitionExecute: Send + Sync {
    /// 执行工具。
    ///
    /// - `tool_call_id`：本次调用的稳定 id，用于日志/UI 关联；
    /// - `params`：已通过 `parameters` schema 校验的参数对象（由 agent loop 管线保证；
    ///   直接调用本方法时由调用方自行保证）；
    /// - `signal`：取消信号，生命周期覆盖返回的 future；**取消不会自动丢弃 future**，
    ///   实现必须自己检查 [`CancellationToken::is_cancelled`] 并尽快返回；
    /// - `on_update`：可选的流式结果回调，可在 future 完成前多次调用；
    /// - `ctx`：扩展上下文；`kiana-tools::definition::wrap_tool_definition` 在没有工厂时
    ///   传入一个“无 UI/无会话”的占位实现。
    ///
    /// 错误以 [`AgentToolError`] 返回，agent loop 会把它转成 `is_error` 的工具结果。
    fn execute<'a>(
        &'a self,
        tool_call_id: &'a str,
        params: JsonValue,
        signal: Option<&'a CancellationToken>,
        on_update: Option<AgentToolUpdateCallback>,
        ctx: Arc<dyn ExtensionToolContextApi>,
    ) -> BoxFuture<'a, Result<AgentToolResult<JsonValue>, AgentToolError>>;
}

/// 注册给模型的工具定义。
///
/// TS: `types.ts#ToolDefinition<TParams, TDetails, TState>`（全部 20 个字段，按 TS 声明顺序）。
/// Rust 侧擦除泛型：`parameters`/`outputSchema` 是 [`JsonValue`]（`kiana-agent-core` 的
/// `AgentTool` 约定），`execute` 是 [`ToolDefinitionExecute`] trait 对象。
///
/// `ToolDefinition` 是 `Clone + Send + Sync`（全部字段是 `Arc`/纯数据）；tests 里有编译期断言。
/// 克隆只复制 `Arc` 与纯数据，不会复制闭包或执行器本身（`tests/tool_types.rs` 用
/// `Arc::ptr_eq` 钉住 `define_tool` 的恒等性）。
#[derive(Clone)]
pub struct ToolDefinition {
    /// 工具名（LLM 工具调用名）。
    pub name: String,
    /// UI 人类可读标签。
    pub label: String,
    /// 给 LLM 的描述。
    pub description: String,
    /// 默认系统提示 “Available tools” 段的一行摘要；不提供则自定义工具从该段省略。
    ///
    /// 会话在构建提示前会 trim 并折叠连续空白，空串按“未提供”处理。
    pub prompt_snippet: Option<String>,
    /// 工具 active 时追加到默认系统提示 Guidelines 段的条目。
    pub prompt_guidelines: Option<Vec<String>>,
    /// 参数 schema（TypeBox 的 JSON 表示）；**必须是 object schema**（注册时校验，在 `kiana-extensions`）。
    ///
    /// 校验点是 `kiana-extensions` 的 `register_tool_checked`：`!parameters.is_object()` 时
    /// 直接返回带工具名与扩展路径的错误，工具不会注册。
    pub parameters: JsonValue,
    /// provider 侧受限采样；`None` 表示不启用（等同未设置）。
    pub constrained_sampling: Option<ConstrainedSampling>,
    /// 标准彩色外壳还是工具自渲染框架；缺省按 [`RenderShell::Default`] 处理。
    pub render_shell: Option<RenderShell>,
    /// schema 校验前修正原始参数的兼容 shim。
    pub prepare_arguments: Option<PrepareArgumentsFn>,
    /// 成功结果中 `structuredContent` 的 JSON Schema。
    pub output_schema: Option<JsonValue>,
    /// 模型如何触达该工具；缺省 `direct`。
    pub exposure: Option<ToolExposure>,
    /// 工具所属分组。
    pub namespace: Option<ToolNamespace>,
    /// 该调用可能触达哪些工具（`codemode`/`tool_search` 用）。
    pub reaches_tools: Option<ReachesToolsFn>,
    /// 工具行为提示（例如来自 MCP server）。
    pub annotations: Option<ToolAnnotations>,
    /// 注册时是否激活；`direct`/`model-only` 默认 true，其余默认 false。
    ///
    /// 会话的精确判定是 `is_declarable(exposure) && default_active.unwrap_or(true)`：
    /// 只有 `direct`/`model-only` 会看这个字段，其它曝光度即使写 `Some(true)` 也不会激活。
    pub default_active: Option<bool>,
    /// active 工具变化时调整 loadout 的展示。
    ///
    /// 会话按当前工具列表顺序调用所有 hook 并汇总：`descriptions` 后者覆盖前者，
    /// `hidden_declarations` 取并集。hook 的 panic 由会话用 `catch_unwind` 隔离，
    /// 有 runner 时作为扩展错误上报，不会打断会话。
    pub prepare_loadout: Option<PrepareLoadoutFn>,
    /// 单工具执行模式覆盖。
    pub execution_mode: Option<ToolExecutionMode>,
    /// 执行工具。
    ///
    /// agent loop 会先跑 `prepare_arguments` 与 schema 校验，再通过
    /// `kiana-tools::definition::wrap_tool_definition` 生成的适配器调用本字段；每次调用都会
    /// 重新创建 `ctx`（有上下文工厂时）。
    pub execute: Arc<dyn ToolDefinitionExecute>,
    /// 工具调用显示的自定义渲染。
    pub render_call: Option<RenderCallFn>,
    /// 工具结果显示的自定义渲染。
    pub render_result: Option<RenderResultFn>,
}

// 手写 Debug：闭包与执行入口无法 Debug，只打印三个可读字段并用 `finish_non_exhaustive`
// 标出“其余字段未展开”。
impl std::fmt::Debug for ToolDefinition {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ToolDefinition")
            .field("name", &self.name)
            .field("label", &self.label)
            .field("description", &self.description)
            .finish_non_exhaustive()
    }
}

/// 保留参数推断的恒等函数（运行期零行为）。
///
/// TS: `types.ts#defineTool`。`defineTool(tool)` 原样返回同一个工具：
/// `Arc::ptr_eq(&define_tool(def.clone()).execute, &def.execute)` 必须为 true。
pub fn define_tool(tool: ToolDefinition) -> ToolDefinition {
    tool
}

// 下面 8 个单变体枚举对应 TS 各工具定义里的 `toolName: "<literal>"` 字面量类型。
// 单变体保证 Rust 侧只能构造出合法拼写；反序列化其它字符串会得到 unknown variant 错误。
/// `bash` 工具的固定 `toolName` 字面量。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum BashToolName {
    /// `"bash"`。
    #[default]
    #[serde(rename = "bash")]
    Bash,
}

/// `powershell` 工具的固定 `toolName` 字面量。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum PowerShellToolName {
    /// `"powershell"`。
    #[default]
    #[serde(rename = "powershell")]
    PowerShell,
}

/// `read` 工具的固定 `toolName` 字面量。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum ReadToolName {
    /// `"read"`。
    #[default]
    #[serde(rename = "read")]
    Read,
}

/// `edit` 工具的固定 `toolName` 字面量。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum EditToolName {
    /// `"edit"`。
    #[default]
    #[serde(rename = "edit")]
    Edit,
}

/// `write` 工具的固定 `toolName` 字面量。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum WriteToolName {
    /// `"write"`。
    #[default]
    #[serde(rename = "write")]
    Write,
}

/// `grep` 工具的固定 `toolName` 字面量。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum GrepToolName {
    /// `"grep"`。
    #[default]
    #[serde(rename = "grep")]
    Grep,
}

/// `find` 工具的固定 `toolName` 字面量。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum FindToolName {
    /// `"find"`。
    #[default]
    #[serde(rename = "find")]
    Find,
}

/// `ls` 工具的固定 `toolName` 字面量。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum LsToolName {
    /// `"ls"`。
    #[default]
    #[serde(rename = "ls")]
    Ls,
}

/// `bash` 工具的输入。
///
/// TS: `core/tools/bash.ts#BashToolInput`（`{ command: string; timeout?: number }`）。
/// `timeout` 的单位是**秒**（`resolveTimeoutMs(timeout)`）。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BashToolInput {
    /// 要执行的 shell 命令。
    pub command: String,
    /// 超时，**秒**（可选，无默认超时；TS `number`，接受任意 JS 数值）。
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        serialize_with = "js_number::serialize_option"
    )]
    pub timeout: Option<f64>,
}

/// PowerShell 工具的输入：TS 的 `PowerShellToolInput` 是 `BashToolInput` 的别名。
pub type PowerShellToolInput = BashToolInput;

/// `bash` 工具的结果详情。
///
/// TS: `core/tools/bash.ts#BashToolDetails`。`truncation` 与 `fullOutputPath` 均可选。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BashToolDetails {
    /// 截断结果。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub truncation: Option<TruncationResult>,
    /// 完整输出所在的临时文件。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub full_output_path: Option<String>,
}

/// PowerShell 工具的结果详情：TS 的 `PowerShellToolDetails` 是 `BashToolDetails` 的别名。
pub type PowerShellToolDetails = BashToolDetails;

/// `read` 工具的输入。
///
/// TS: `core/tools/read.ts#ReadToolInput`
/// （`{ path: string; offset?: number; limit?: number }`）。
///
/// `offset`/`limit` 是 TS 的 `number`：用 `f64` 接受任意 JS 数值（负数、小数），
/// 归一化（`read.ts` 的 `Math.max(0, offset - 1)`、`slice` 的 ToIntegerOrInfinity）
/// 由 `kiana-tools::read` 在使用点完成。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReadToolInput {
    /// 要读取的文件路径（相对或绝对）。
    pub path: String,
    /// 起始行号（1 起）。
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        serialize_with = "js_number::serialize_option"
    )]
    pub offset: Option<f64>,
    /// 最大读取行数。
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        serialize_with = "js_number::serialize_option"
    )]
    pub limit: Option<f64>,
}

/// `read` 工具的结果详情。TS: `core/tools/read.ts#ReadToolDetails`。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReadToolDetails {
    /// 截断结果。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub truncation: Option<TruncationResult>,
}

/// `edit` 工具的输入。
///
/// TS: `core/tools/edit.ts#EditToolInput`（`{ path: string; edits: {oldText,newText}[] }`）。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EditToolInput {
    /// 要编辑的文件路径（相对或绝对）。
    pub path: String,
    /// 一处或多处定向替换（都针对原文件匹配）。
    pub edits: Vec<ReplaceEdit>,
}

/// [`EditToolInput`] 的一处替换。
///
/// TS: `core/tools/edit.ts` 的 TypeBox `replaceEditSchema` 内联对象
/// `{ oldText: string; newText: string }`（Rust-only helper，字段顺序即键顺序）。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReplaceEdit {
    /// 必须唯一且不与其它 `edits[].oldText` 重叠的原文。
    pub old_text: String,
    /// 该处替换后的文本。
    pub new_text: String,
}

/// `edit` 工具的结果详情。TS: `core/tools/edit.ts#EditToolDetails`。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EditToolDetails {
    /// 展示用 diff。
    pub diff: String,
    /// 标准 unified patch。
    pub patch: String,
    /// 新文件中第一处变更的行号（编辑器导航用）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_changed_line: Option<u64>,
}

/// `write` 工具的输入。
///
/// TS: `core/tools/write.ts#WriteToolInput`（`{ path: string; content: string }`）。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WriteToolInput {
    /// 要写入的文件路径（相对或绝对）。
    pub path: String,
    /// 写入内容。
    pub content: String,
}

/// `grep` 工具的输入。
///
/// TS: `core/tools/grep.ts#GrepToolInput`。字段顺序即 TS schema 顺序。
///
/// `context`/`limit` 是 TS 的 `number` → `f64`（负数/小数由 `kiana-tools::grep` 在使用点按
/// `grep.ts` 的 `context > 0`、`Math.max(1, limit ?? 100)` 归一化）。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GrepToolInput {
    /// 搜索模式（正则或字面量）。
    pub pattern: String,
    /// 搜索目录或文件（缺省当前目录）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// 按 glob 过滤文件，例如 `*.ts` 或 `**/*.spec.ts`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub glob: Option<String>,
    /// 大小写不敏感。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ignore_case: Option<bool>,
    /// 把模式当作字面量。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub literal: Option<bool>,
    /// 每个匹配前后显示的行数。
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        serialize_with = "js_number::serialize_option"
    )]
    pub context: Option<f64>,
    /// 最大返回匹配数（缺省 100）。
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        serialize_with = "js_number::serialize_option"
    )]
    pub limit: Option<f64>,
}

/// `grep` 工具的结果详情。TS: `core/tools/grep.ts#GrepToolDetails`。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GrepToolDetails {
    /// 截断结果。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub truncation: Option<TruncationResult>,
    /// 达到的匹配数上限。
    ///
    /// TS 是 `number` 并被赋值为**未取整**的 `effectiveLimit = Math.max(1, limit ?? 100)`：
    /// 模型给出 `limit: 2.5` 时这里就是 `2.5`（通知文本与 `details` 都是），因此用 `f64`。
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        serialize_with = "js_number::serialize_option"
    )]
    pub match_limit_reached: Option<f64>,
    /// 是否有匹配行被截断。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lines_truncated: Option<bool>,
}

/// `find` 工具的输入。
///
/// TS: `core/tools/find.ts#FindToolInput`（`{ pattern: string; path?: string; limit?: number }`）。
/// `limit` 是 TS 的 `number` → `f64`（`find.ts` 的 `limit ?? DEFAULT_LIMIT` 在使用点处理）。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FindToolInput {
    /// glob 模式，例如 `*.ts`、`**/*.json`、`src/**/*.spec.ts`。
    pub pattern: String,
    /// 搜索目录（缺省当前目录）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// 最大结果数（缺省 1000）。
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        serialize_with = "js_number::serialize_option"
    )]
    pub limit: Option<f64>,
}

/// `find` 工具的结果详情。TS: `core/tools/find.ts#FindToolDetails`。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FindToolDetails {
    /// 截断结果。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub truncation: Option<TruncationResult>,
    /// 达到的结果数上限。
    ///
    /// TS 是 `number` 并被赋值为**未取整**的 `effectiveLimit = limit ?? 1000`：模型给出
    /// `limit: 2.5` 时这里就是 `2.5`（通知文本与 `details` 都是），因此用 `f64`。
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        serialize_with = "js_number::serialize_option"
    )]
    pub result_limit_reached: Option<f64>,
}

/// `ls` 工具的输入。
///
/// TS: `core/tools/ls.ts#LsToolInput`（`{ path?: string; limit?: number }`）。
/// `limit` 是 TS 的 `number` → `f64`（`ls.ts` 的 `limit ?? DEFAULT_LIMIT` 在使用点处理）。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LsToolInput {
    /// 要列出的目录（缺省当前目录）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// 最大条目数（缺省 500）。
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        serialize_with = "js_number::serialize_option"
    )]
    pub limit: Option<f64>,
}

/// `ls` 工具的结果详情。TS: `core/tools/ls.ts#LsToolDetails`。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LsToolDetails {
    /// 截断结果。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub truncation: Option<TruncationResult>,
    /// 达到的条目数上限。
    ///
    /// TS 是 `number` 并被赋值为**未取整**的 `effectiveLimit = limit ?? 500`：模型给出
    /// `limit: 2.5` 时这里就是 `2.5`（通知文本与 `details` 都是），因此用 `f64`。
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        serialize_with = "js_number::serialize_option"
    )]
    pub entry_limit_reached: Option<f64>,
}

/// 截断命中的限制种类。
///
/// TS: `core/tools/truncate.ts#TruncationResult.truncatedBy`（`"lines" | "bytes" | null`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TruncatedBy {
    /// `"lines"`：行数限制。
    Lines,
    /// `"bytes"`：字节限制。
    Bytes,
}

/// 工具输出的截断结果。
///
/// TS: `core/tools/truncate.ts#TruncationResult`。字段顺序即 `JSON.stringify(details)` 的键顺序
/// （`BashToolDetails.truncation` 等会进会话文件，spec `ca-tools-compaction-export.md` §2.1.2）。
///
/// `truncatedBy` 是 `"lines" | "bytes" | null`：**必填键**，未截断时写出 `null`
/// （`Option` 但**不加** `skip_serializing_if`）。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TruncationResult {
    /// 截断后的内容。
    pub content: String,
    /// 是否发生截断。
    pub truncated: bool,
    /// 命中哪个限制；未截断时为 `null`（按键写出）。
    pub truncated_by: Option<TruncatedBy>,
    /// 原始内容的总行数。
    pub total_lines: u64,
    /// 原始内容的总字节数。
    pub total_bytes: u64,
    /// 截断输出中的完整行数。
    pub output_lines: u64,
    /// 截断输出中的字节数。
    pub output_bytes: u64,
    /// 最后一行是否被部分截断（仅 tail 截断的边界情况）。
    pub last_line_partial: bool,
    /// 第一行是否超过字节限制（head 截断）。
    pub first_line_exceeds_limit: bool,
    /// 应用的 maxLines 限制。
    pub max_lines: u64,
    /// 应用的 maxBytes 限制。
    pub max_bytes: u64,
}
