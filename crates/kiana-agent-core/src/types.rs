//! Agent 跨 crate 契约：状态、事件、工具定义、钩子配置。
//!
//! 字段顺序与 JSON 形状按 `packages/agent/src/types.ts`；`AgentMessage` 是开放联合，
//! TS 通过 declaration merging 扩展，Rust 用 [`AgentMessage::Custom`] 携带
//! `role` 判别 + 任意 JSON（coding-agent 的 bashExecution/custom/branchSummary/
//! compactionSummary 走这条路径；键序由 `preserve_order` 保证，写出时再按 JS
//! `JSON.stringify` 规则重排整数索引键并格式化数字，见 `serialize_js_json`）。
//!
//! # 这个文件在 crate 中的位置
//!
//! 本 crate 的其他模块（`agent` / `agent_loop` / `proxy`）以及下游 crate（`kiana-session`、
//! `kiana-tools`、`kiana-extensions`）都通过这里的类型交互。类型本身大多是数据载体，
//! 真正的执行语义在 `agent_loop`；但字段名、serde 属性与默认值会直接决定落盘会话和事件的
//! 字节形状，改它们等于改兼容契约。
//!
//! # 序列化边界
//!
//! - 反序列化走普通 serde：允许读取 TS 写出的会话文件（`undefined` 的键缺省、数字解析由
//!   crate 的 `serde_json` `float_roundtrip` 特性与 `JSON.parse` 逐位对齐）。
//! - 序列化则要迁就 TS：这些对象在 TS 侧都经过 `JSON.stringify`，而 serde 默认规则不同
//!   （`1.0` 写成 `1`、整数索引键升序排最前、超 2^53 的整数按 f64 舍入、非有限数写 `null`）。
//!   因此 `AgentMessage` 手写 `Serialize`，事件与工具结果里的自由形状字段统一走
//!   `serialize_js_json`（本文件的私有辅助函数）。改这些属性前先看 `tests/public_api.rs`
//!   的 golden 断言（含 node 生成的 `tests/fixtures/agent_message_js_stringify.jsonl`）。
//!
//! # 快照与共享句柄
//!
//! TS 的钩子参数是对象引用，就地修改会回写到运行；Rust 的值语义无法表达别名。约定：
//! `AgentContext` / `AssistantMessage` 等以**克隆快照**传给钩子（修改不回写），唯一例外是
//! [`ToolArguments`]：它用 `Arc<Mutex<_>>` 承载校验后的参数，`beforeToolCall` 的就地改写会
//! 原样传给 `execute()`（TS issue #528 的行为）。
//!
//! # 阅读顺序
//!
//! 先看 [`AgentMessage`] 与 [`AgentToolResult`]（消息/结果形状），再看 [`AgentEvent`]
//! （对外事件），最后看 [`AgentLoopConfig`] 与各钩子类型别名（agent loop 的注入点）。

use std::collections::BTreeSet;

use indexmap::IndexMap;
use kiana_ai::types::{
    AssistantMessage, AssistantMessageEvent, Model, SimpleStreamOptions, ToolResultMessage,
    TranscriptContext, Usage,
};
use serde::{Deserialize, Serialize};

/// 任意 JSON。
pub type JsonValue = serde_json::Value;
/// JSON 对象。
///
/// 用 `IndexMap` 而不是 `serde_json::Map` 来承载：自定义消息要按 TS 对象的**插入序**
/// 保留键序（写出时再按 `JSON.stringify` 规则把整数索引键升序提前），
/// `tests/public_api.rs` 的键序断言依赖这一点。
pub type JsonObject = IndexMap<String, JsonValue>;

/// 异步盒装 future。
///
/// 用于把 `async` 回调（钩子、工具执行）放进 `Arc<dyn Fn>`。`Send` 是回调跨线程/跨 tokio
/// 任务传递的要求；生命周期参数 `'a` 让回调可以借用调用方的数据（例如 `&str` 形式的
/// `tool_call_id`）。
pub type BoxFuture<'a, T> = std::pin::Pin<Box<dyn std::future::Future<Output = T> + Send + 'a>>;

/// 保留“键存在且为 JSON `null`”与“键缺省”的区别。
///
/// TS 的 `field?: JsonValue` 里 `undefined`（缺省）与 `null` 是两种不同形状；serde 的
/// `Option` 默认把两者都收成 `None`，这里把存在（含 null）一律收成 `Some`。
/// 与 `kiana_ai::types::ToolResultMessage.details` 的处理一致。
fn deserialize_present_json<'de, D>(deserializer: D) -> Result<Option<JsonValue>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    JsonValue::deserialize(deserializer).map(Some)
}

/// 按 JS `JSON.stringify` 语义写出任意可序列化的值。
///
/// `serde_json::to_string` 不是 `JSON.stringify`：数字走 ryu（`1.0` 而非 `1`、`-0.0`
/// 而非 `0`、大整数不做 f64 舍入），对象键保持插入序（JS 把整数索引键升序排最前）。
/// TS 侧这些字段（自定义消息整对象、事件 `args`/`partialResult`/`result`、工具结果的
/// `details`/`structuredContent`）都经过 `JSON.stringify`，落盘与上屏的字节必须一致
/// （ARCHITECTURE.md §4.3 第 3、4 条）。
///
/// 实现先把值收敛成 [`JsonValue`]，再借 [`kiana_json::js_object::stringify`] 生成文本，
/// 最后用 [`serde_json::value::RawValue`] 原样嵌入（与 `kiana-ai`/`kiana-mcp` 同类字段一致；
/// `RawValue` 在 `to_string` 与 `to_value` 两条路径上都保留 JSON 结构）。
fn serialize_js_json<T, S>(value: &T, serializer: S) -> Result<S::Ok, S::Error>
where
    T: Serialize + ?Sized,
    S: serde::Serializer,
{
    use serde::ser::Error;
    let value = serde_json::to_value(value).map_err(S::Error::custom)?;
    let text = kiana_json::js_object::stringify(&value);
    serde_json::value::RawValue::from_string(text)
        .map_err(S::Error::custom)?
        .serialize(serializer)
}

/// `details` 的“缺省”判定：字段的 JSON 形态是 `null` 时按 TS 的 `undefined` 处理。
///
/// TS 的 `AgentToolResult<T = JsonValue | undefined>['details']` 允许 `undefined`，
/// `JSON.stringify` 会**省略**该键（`bash`/`grep`/`ls`/`find`/`write` 等工具都返回
/// `details: undefined`）。Rust 的泛型 `T` 无法表达 `undefined`，约定用 `null`
/// （`T::default()`，`JsonValue` 即 `Null`、`Option<JsonValue>` 即 `None`）表示缺省；
/// 写出时据此省略该键，与 `JSON.stringify` 一致。只是“显式 `details: null`”在 TS 侧
/// 从不出现（`afterToolCall` 的合并语义把 `null` 与 `undefined` 等同，见
/// [`AfterToolCallResult`]），因此这一歧义不影响实际线格式。
fn json_value_is_null<T>(value: &T) -> bool
where
    T: Serialize + ?Sized,
{
    serde_json::to_value(value)
        .map(|value| value.is_null())
        .unwrap_or(false)
}

/// 把 `Option<f64>` 的毫秒数写成 JS `JSON.stringify` 的数字形态。
///
/// TS 的 `durationMs` 由 `Math.round()` 产生，恒为整数；serde 默认会把 `12.0f64` 写成
/// `12.0`，而 JS 写 `12`。非有限值按 JS 规则写成 `null`（`JSON.stringify(NaN) === "null"`）。
fn serialize_duration_ms<S>(value: &Option<f64>, serializer: S) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    match value {
        None => serializer.serialize_none(),
        Some(ms) if !ms.is_finite() => serializer.serialize_none(),
        Some(ms) if ms.fract() == 0.0 && *ms >= i64::MIN as f64 && *ms <= i64::MAX as f64 => {
            serializer.serialize_i64(*ms as i64)
        }
        Some(ms) => serializer.serialize_f64(*ms),
    }
}

/// kiana-ai 的 `ThinkingLevel`（不含 `"off"`）：`SimpleStreamOptions.reasoning` 的取值集合。
pub use kiana_ai::types::ThinkingLevel as AiThinkingLevel;

/// `ThinkingLevel = "off" | "minimal" | "low" | "medium" | "high" | "xhigh" | "max"`
/// （TS: `types.ts#ThinkingLevel`，含 `"off"`）。
pub use kiana_ai::types::ModelThinkingLevel as ThinkingLevel;

// ---------------------------------------------------------------------------
// AgentMessage
// ---------------------------------------------------------------------------

/// `AgentMessage = Message | CustomAgentMessages[keyof CustomAgentMessages]`。
///
/// 标准四种角色用 [`AgentMessage::Standard`]，其余走 [`AgentMessage::Custom`]；
/// `role` 字段在两者中都出现在 JSON 顶层，因此序列化结果形状统一。
///
/// 反序列化用 `#[serde(untagged)]`，按声明顺序**先尝试标准消息**，全部不匹配才落入
/// [`AgentMessage::Custom`]。`Custom` 只接受 JSON 对象（`JsonObject`），其 `role` 由对象
/// 自身携带；因此一条自定义消息如果碰巧满足某个标准消息的字段形状，会被读成标准消息。
/// 序列化是手写的（见下面的 `Serialize` 实现）：两个分支都按 JS `JSON.stringify` 语义写出。
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(untagged)]
#[allow(clippy::large_enum_variant)]
pub enum AgentMessage {
    /// 标准四类消息（system / user / assistant / toolResult），内部是 `kiana_ai` 的类型。
    Standard(kiana_ai::types::Message),
    /// `{ role: <自定义名>, ... }` 的整对象。
    Custom(Box<JsonObject>),
}

impl Serialize for AgentMessage {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        match self {
            // TS 的 `AgentMessage` 整体走 `JSON.stringify`（会话文件逐字回放）：标准消息里
            // `usage.cost` 等 f64 字段同样必须按 JS 规则写出（`0.0` → `0`、`5e-6` →
            // `0.000005`），因此不能交给 kiana-ai 的 serde（ryu）直接写出。
            AgentMessage::Standard(message) => serialize_js_json(message, serializer),
            // TS: 自定义消息整体走 `JSON.stringify`（会话文件逐字回放）：键序与数字格式按
            // JS 规则（字符串键保持插入序、整数索引键升序在前、`1.0` → `1`、`-0` → `0`、
            // 超过 2^53 的整数按 f64 舍入）。
            AgentMessage::Custom(object) => serialize_js_json(object.as_ref(), serializer),
        }
    }
}

impl AgentMessage {
    /// 角色判别值：标准消息返回 `"system"` / `"user"` / `"assistant"` / `"toolResult"`；
    /// 自定义消息读取对象的 `role` 字段，缺失或不是字符串时返回空串（不会 panic）。
    pub fn role(&self) -> &str {
        match self {
            AgentMessage::Standard(m) => m.role(),
            AgentMessage::Custom(v) => v
                .get("role")
                .and_then(JsonValue::as_str)
                .unwrap_or_default(),
        }
    }
    /// 仅当消息是标准四类消息时返回内部 `kiana_ai::types::Message`；自定义消息返回 `None`。
    pub fn as_standard(&self) -> Option<&kiana_ai::types::Message> {
        match self {
            AgentMessage::Standard(m) => Some(m),
            AgentMessage::Custom(_) => None,
        }
    }
    /// 仅当消息是**标准** assistant 消息时返回引用；自定义消息没有类型化视图，恒为 `None`
    /// （哪怕它的 `role` 字段写的是 `"assistant"`）。
    pub fn as_assistant(&self) -> Option<&AssistantMessage> {
        match self {
            AgentMessage::Standard(kiana_ai::types::Message::Assistant(a)) => Some(a),
            _ => None,
        }
    }
    /// Unix 毫秒时间戳。
    ///
    /// 标准消息的 `timestamp` 字段恒存在，因此返回 `Some(..)`；自定义消息读取对象的
    /// `timestamp` 字段且只接受整数形态（`as_i64`），缺失、浮点或超出 i64 范围时为 `None`。
    pub fn timestamp(&self) -> Option<i64> {
        match self {
            AgentMessage::Standard(m) => Some(match m {
                kiana_ai::types::Message::System(s) => s.timestamp,
                kiana_ai::types::Message::User(u) => u.timestamp,
                kiana_ai::types::Message::Assistant(a) => a.timestamp,
                kiana_ai::types::Message::ToolResult(t) => t.timestamp,
            }),
            AgentMessage::Custom(v) => v.get("timestamp").and_then(JsonValue::as_i64),
        }
    }
}

// ---------------------------------------------------------------------------
// Tool definitions
// ---------------------------------------------------------------------------

/// `AgentToolCall = Extract<AssistantMessage["content"][number], { type: "toolCall" }>`。
///
/// 就是 `kiana_ai::types::ToolCall`：`type` 固定为 `"toolCall"`，`arguments` 是已解析的
/// JSON 对象；模型给出的参数文本不是合法对象时，`arguments` 为空对象并把原因放进
/// `arguments_error`（见该类型的文档）。
pub type AgentToolCall = kiana_ai::types::ToolCall;

/// `AgentToolResult<T>`。
///
/// 工具执行结果：`content` 会原样成为 `ToolResultMessage.content`；`details` /
/// `structuredContent` 是给日志或 UI 渲染的结构化附加数据。
///
/// 泛型 `T` 只影响 `details`。写方向要求 `T: Serialize`，读方向要求
/// `T: Deserialize + Default`；这由 `#[serde(bound)]` 显式声明，因为 `details` 用了
/// `serialize_with`，serde 不会自动推导泛型 bound。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
// `details` 用自定义 `serialize_with`、且带 `#[serde(default)]`，serde 不再自动推导泛型
// 参数上的 bound，这里显式补齐（否则 `T: Serialize` / `T: Default` 不可用）。
#[serde(
    rename_all = "camelCase",
    bound(
        serialize = "T: Serialize",
        deserialize = "T: Deserialize<'de> + Default"
    )
)]
pub struct AgentToolResult<T = JsonValue> {
    /// Text or image content returned to the model.
    pub content: Vec<kiana_ai::types::UserContentBlock>,
    /// Arbitrary structured details for logs or UI rendering.
    ///
    /// TS 的 `details` 可以是 `undefined`（`CalculateResult`、codemode/MCP 工具都返回
    /// `details: undefined`），`JSON.stringify` 会**省略**该键；Rust 的泛型 `T` 无法表达
    /// “缺省”，约定用 `T::default()` 表示（`JsonValue` 即 `null`）。
    /// `#[serde(default)]` 保证 TS 写出的会话文件（没有 `details` 键）仍可读回；
    /// 写出方向由 `json_value_is_null` 完成：`null` 视为 `undefined` 并省略该键。
    /// `ToolResultMessage.details` 另由 agent loop 把缺省映射成 `None`
    /// （见 ARCHITECTURE.md §4.9 冻结契约与本文件的测试注释）。
    #[serde(
        default,
        serialize_with = "serialize_js_json",
        skip_serializing_if = "json_value_is_null"
    )]
    pub details: T,
    /// Machine-readable result matching the tool's `outputSchema`.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "deserialize_present_json",
        serialize_with = "serialize_js_json"
    )]
    pub structured_content: Option<JsonValue>,
    /// Usage from the final tool execution itself.
    ///
    /// `usage.cost` 是 f64；TS 的 `JSON.stringify` 写出 `0` / `0.000005`，serde 的 ryu 会写出
    /// `0.0` / `5e-6`，因此这里同样走 `serialize_js_json`。
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        serialize_with = "serialize_js_json"
    )]
    pub usage: Option<Usage>,
    /// Report a failure without throwing.
    ///
    /// agent loop 把它折算成执行阶段的错误判定（`Some(true)` 即失败），最终结果仍可被
    /// `afterToolCall` 覆盖。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_error: Option<bool>,
    /// Hint that the agent should stop after the current tool batch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terminate: Option<bool>,
}

/// `AgentToolCallOutcome`。
///
/// 一次工具调用的完整结果：原始调用、工具结果、最终错误判定与耗时。它是
/// `agent_loop::run_tool_call` 的返回值，也被扩展宿主用作嵌套工具调用的结果
/// （`kiana-extensions` 的 `ctx.executeTool`）。
///
/// 注意 `is_error` 是**最终**判定：`afterToolCall` 可以覆盖它；它与
/// `result.is_error`（工具自己上报的值，可为 `None`）不是同一来源。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentToolCallOutcome {
    /// 发起调用的工具调用；`prepareArguments` 的就地改写已反映在 `arguments` 上。
    pub tool_call: AgentToolCall,
    /// 工具返回的结果（工具报错时是由错误消息合成的结果，见 [`AgentToolExecute`]）。
    pub result: AgentToolResult<JsonValue>,
    /// 最终错误判定；agent loop 用它写 `ToolResultMessage.is_error`。
    pub is_error: bool,
    /// Milliseconds `execute()` took; absent when the tool did not run.
    ///
    /// 未知工具、参数校验失败、被 `beforeToolCall` 拦截等“没有真正执行”的路径为 `None`；
    /// 工具执行后抛错仍然有值。
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        serialize_with = "serialize_duration_ms"
    )]
    pub duration_ms: Option<f64>,
}

/// `AgentToolUpdateCallback<T>`：工具流式回报。
///
/// 工具可以在 `execute` 运行期间多次调用它，每次携带一份 `AgentToolResult`。agent loop 会
/// 把每次调用转成一条 `tool_execution_update` 事件（见 [`AgentEvent::ToolExecutionUpdate`]）：
/// 事件 future 先被收集，等 `execute()` 返回后再统一 await，与 TS 的
/// `updateEvents.push(Promise.resolve(onUpdate(partial)))` 一致。`Send + Sync` 是回调跨任务
/// 传递的要求。
pub type AgentToolUpdateCallback = std::sync::Arc<dyn Fn(AgentToolResult<JsonValue>) + Send + Sync>;

/// `ToolExecutionMode`。
///
/// 一次 assistant 回复里多个工具调用的调度方式。配置级
/// （[`AgentLoopConfig::tool_execution`]）与工具级（[`AgentTool::execution_mode`]）都参与
/// 判定：只要配置为 `Sequential`，或批次里**任一**工具声明了 `Sequential`，整批就按顺序
/// 执行；否则走并行分支。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ToolExecutionMode {
    /// 逐个执行：前一个工具（含 `afterToolCall`）结束后才开始下一个。
    Sequential,
    /// 并行推进：先顺序预检（逐个发 `tool_execution_start`、执行 `prepareArguments` 与参数
    /// 校验、等待 `beforeToolCall`），再把已准备好的调用并发执行；每个 `execute` 完成后
    /// 立即做自己的 `afterToolCall`。
    Parallel,
}

/// `QueueMode`。
///
/// steering / follow-up 消息队列的取用方式（`AgentOptions::steering_mode` /
/// `AgentOptions::follow_up_mode`，缺省 `one-at-a-time`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum QueueMode {
    /// 一次取走队列中的全部消息。
    All,
    /// 每次只取最早的一条，其余留到下一轮。
    OneAtATime,
}

/// `AgentTool`。
///
/// `parameters` 是 JSON Schema（TypeBox 的序列化结果）；`execute` 是异步回调。
/// 由于 trait 对象不能带泛型方法，这里用具体结构 + `Arc<dyn Fn>`，
/// 与 TS 的对象字面量形状一一对应。
#[derive(Clone)]
pub struct AgentTool {
    /// 工具名：`toolCall.name` 按它查找工具，也是发给模型的声明名。
    pub name: String,
    /// 供 UI 展示的可读名字（不参与查找；`name` 才是唯一标识）。
    pub label: String,
    /// 工具说明，投影为工具声明的 `description` 发给模型。
    pub description: String,
    /// JSON Schema（TypeBox 的序列化结果），用于校验模型给出的参数。
    pub parameters: JsonValue,
    /// 受约束采样的 grammar 配置；随工具声明原样投影给 provider（本 crate 不做解析）。
    pub constrained_sampling: Option<kiana_ai::types::ConstrainedSampling>,
    /// 工具结果的 JSON Schema（TS 的 `outputSchema`）。本 crate 的 agent loop 不读取它，
    /// 只由工具层/扩展层投影给调用方。
    pub output_schema: Option<JsonValue>,
    /// Compatibility shim for raw tool-call arguments before schema validation.
    ///
    /// TS: `prepareArguments?: (args: unknown) => Static<TParameters>`。TS 的闭包收到的是
    /// `toolCall.arguments` 的**同一个对象引用**，可以就地改写（edit 工具的
    /// `args.edits = parsed`）并返回它，也可以返回一个新对象。Rust 的值语义用返回值区分：
    ///
    /// - `None`：闭包返回了同一个对象（TS 的 `preparedArguments === toolCall.arguments`），
    ///   对 `&mut args` 的就地改写保留在原始工具调用里；
    /// - `Some(value)`：闭包返回了新值，原始工具调用保留就地改写后的参数（未改写则保持原值），
    ///   校验与执行使用 `value`（与 TS 的 `{...toolCall, arguments: preparedArguments}` 相同）。
    // 复杂类型是 TS 对象字面量方法的直接对应；拆成别名只会分散契约定义（clippy 例外见 §4.10）。
    #[allow(clippy::type_complexity)]
    pub prepare_arguments:
        Option<std::sync::Arc<dyn Fn(&mut JsonValue) -> Option<JsonValue> + Send + Sync>>,
    /// 工具级执行模式；`Some(Sequential)` 会把所在批次强制改为顺序执行，`Some(Parallel)`
    /// 不能覆盖配置级的 `Sequential`（判定见 [`ToolExecutionMode`]）。
    pub execution_mode: Option<ToolExecutionMode>,
    /// Recovery policy for an effect whose durable intent exists but whose outcome is unknown.
    ///
    /// 本 crate 的 agent loop 不读取/校验该字段；`AgentTool` 也没有 `Serialize` 实现，
    /// 不会随工具声明序列化。只有 `kiana-extensions` 投影 `ctx.tools` 时按 `"never"` /
    /// `"safe"` 输出给扩展脚本，具体含义由消费方定义。
    pub replay: Option<ReplayPolicy>,
    /// 实际执行回调；返回 `Err` 时 agent loop 会把它转成 `is_error` 的工具结果
    /// （见 [`AgentToolExecute`]）。
    pub execute: std::sync::Arc<dyn AgentToolExecute>,
}

impl std::fmt::Debug for AgentTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AgentTool")
            .field("name", &self.name)
            .field("label", &self.label)
            .field("description", &self.description)
            .finish_non_exhaustive()
    }
}

/// `replay?: "never" | "safe"`。
///
/// 工具的可选重放标记。本 crate 的 agent loop 既不读取也不校验它，只在序列化时按
/// `"never"` / `"safe"` 写出；是否、何时重放由下游消费方决定。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ReplayPolicy {
    /// 序列化为 `"never"`。
    Never,
    /// 序列化为 `"safe"`。
    Safe,
}

/// 工具的 `execute`。
///
/// TS 签名：`(toolCallId, params, signal, onUpdate) => Promise<AgentToolResult>`。
/// `signal` 在这里是 `Option<&CancellationToken>`；`onUpdate` 缺失即 `None`（agent loop
/// 总会传入回调，直接调用方可以省略）。
///
/// 错误处理：返回 `Err(AgentToolError)` 时，agent loop 用 `AgentToolError::message` 合成
/// 一个 `is_error: true` 的工具结果并保留耗时，**不会**让整轮 run 失败；随后
/// `afterToolCall` 仍会收到这个结果并可以覆盖它。
pub trait AgentToolExecute: Send + Sync {
    fn execute<'a>(
        &'a self,
        tool_call_id: &'a str,
        params: JsonValue,
        signal: Option<&'a tokio_util::sync::CancellationToken>,
        on_update: Option<AgentToolUpdateCallback>,
    ) -> BoxFuture<'a, Result<AgentToolResult<JsonValue>, AgentToolError>>;
}

/// 工具抛出的错误。TS 里是任意 `unknown` throw；Rust 保留消息与可选的结构化细节。
///
/// `Display` 只输出 `message`（`#[error("{message}")]`），与 TS 的 `error.message` 一致；
/// agent loop 在合成错误工具结果时也用它。
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
#[error("{message}")]
pub struct AgentToolError {
    /// 错误消息（逐字进入工具结果）。
    pub message: String,
    /// JS `Error.name`；从脚本异常转换时填充，内置工具通常为 `None`。
    pub name: Option<String>,
    /// JS `Error.stack`；仅在转换方拿得到时填充。
    pub stack: Option<String>,
}

/// `AgentContext`。
///
/// 不可序列化：`AgentTool` 含回调。需要落盘的调用方先用
/// `convert_to_llm` 转成 `Message`（与 TS 的会话写入路径一致）。
///
/// `tools` 是运行期工具集：`None` 与 `Some(vec![])` 对 agent loop 等价——都表示没有可调用
/// 工具，模型发起的调用会得到「Tool 工具名 not found」的错误结果。
#[derive(Clone, Default)]
pub struct AgentContext {
    /// Transcript visible to the model.
    pub messages: Vec<AgentMessage>,
    /// Tools available for execution in this run.
    pub tools: Option<Vec<AgentTool>>,
}

/// `AgentState`。
///
/// TS 用 getter/setter 保证赋值时复制数组；Rust 的 `Vec` 赋值天然复制语义不同，
/// 但 `tools` / `messages` 仍是独立的 `Vec`，修改时按调用方语义处理。
///
/// `Default` 与 TS 的初始状态一致：`thinking_level` 为 `Off`、其余字段为空
/// （`tests/public_api.rs` 的 `agent_state_defaults_match_ts_initial_state` 锁定）。
#[derive(Clone, Default)]
pub struct AgentState {
    /// Current system prompt, replayed from the transcript's system messages.
    pub system_prompt: String,
    /// Active model used for future turns.
    pub model: Option<Model>,
    /// 当前思考档位；`Off` 表示不请求推理。
    pub thinking_level: ThinkingLevel,
    /// Executable tools.
    pub tools: Vec<AgentTool>,
    /// Conversation transcript.
    pub messages: Vec<AgentMessage>,
    /// True while the agent is processing a prompt or continuation.
    pub is_streaming: bool,
    /// Partial assistant message for the current streamed response.
    pub streaming_message: Option<AgentMessage>,
    /// Tool call ids currently executing.
    pub pending_tool_calls: BTreeSet<String>,
    /// Error message from the most recent failed or aborted assistant turn.
    pub error_message: Option<String>,
}

// ---------------------------------------------------------------------------
// Events
// ---------------------------------------------------------------------------

/// `AgentEvent`。
///
/// `tool_execution_*` 的 `args` / `partialResult` / `result` 在 TS 里是 `any`；
/// Rust 统一用 `JsonValue`（UI 侧需要时再按工具名反序列化）。
///
/// # 事件顺序
///
/// 正常情况下单次 run 的骨架是 `agent_start` → `turn_start` → 若干消息/工具事件 →
/// `turn_end` →（可能再来若干轮）→ `agent_end`；`message_update` 只在 assistant 流式输出
/// 期间出现。run 以 `Err` 收尾时（`Agent::handle_run_failure` 路径，不限于进入循环前）会
/// 补发合成的 `message_start` / `message_end` / `turn_end` / `agent_end`。
/// `Agent` 的内部状态由这些事件驱动（`Agent::process_events`）：`message_end` 追加消息、
/// `tool_execution_start`/`tool_execution_end` 维护 `pending_tool_calls`、`agent_end` 清空
/// 流式消息。下游（例如 `kiana-session`）用这些事件驱动会话持久化与对外通知，
/// 因此序列化形状是兼容契约。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
// `rename_all_fields` 覆盖 struct variant 的字段名（`toolCallId`/`assistantMessageEvent`/…）；
// 变体判别值由每个变体自己的 `rename` 给出（`agent_start` 等 snake_case）。TS 的 `AgentEvent`
// 联合字段全部是 camelCase。
#[serde(tag = "type", rename_all_fields = "camelCase")]
pub enum AgentEvent {
    /// 一次 run 开始（先于任何 `turn_start`）。
    #[serde(rename = "agent_start")]
    AgentStart,
    /// 一次 run 结束。正常 run 携带**本次 run** 新增的消息（不重复运行前已有的 transcript）；
    /// 失败收尾路径（`Agent::handle_run_failure`）只携带一条合成的失败消息，本次 run 此前
    /// 已产生的消息不会包含在该事件里。
    #[serde(rename = "agent_end")]
    AgentEnd { messages: Vec<AgentMessage> },
    /// 一轮开始：首轮请求，或工具结果之后的新请求。
    #[serde(rename = "turn_start")]
    TurnStart,
    /// 一轮结束。
    ///
    /// 失败/中止的 assistant 轮（`stopReason` 为 `error` / `aborted`）同样会发 `turn_end`，
    /// 此时 `tool_results` 为空。
    #[serde(rename = "turn_end")]
    TurnEnd {
        /// 本轮最终的 assistant 消息。
        message: AgentMessage,
        /// 本轮的工具结果；`usage.cost` 是 f64，按 TS `JSON.stringify` 的数字规则写出。
        #[serde(serialize_with = "serialize_js_json")]
        tool_results: Vec<ToolResultMessage>,
    },
    /// 一条消息开始；`Agent` 在对应的 `message_end` 才把它加入 `state.messages`。
    #[serde(rename = "message_start")]
    MessageStart { message: AgentMessage },
    /// Only emitted for assistant messages during streaming.
    ///
    /// 字段顺序按 TS 运行期在 `agent-loop.ts` 里构造的对象字面量
    /// （`{ type, assistantMessageEvent, message }`）：JS 的 `JSON.stringify` 键序就是
    /// 构造序，`type` → `assistantMessageEvent` → `message`。`types.ts` 的联合声明把
    /// `message` 写在前面，但类型声明不影响运行期键序。
    #[serde(rename = "message_update")]
    MessageUpdate {
        /// 事件内嵌的 partial 消息含 `usage.cost`（f64），按 JS 数字规则写出。
        #[serde(serialize_with = "serialize_js_json")]
        assistant_message_event: AssistantMessageEvent,
        /// 当前累积的 assistant 消息；与 `assistant_message_event` 内嵌的 `partial` 同值。
        message: AgentMessage,
    },
    /// 一条消息结束；`Agent` 在此把它追加到 `state.messages`。
    #[serde(rename = "message_end")]
    MessageEnd { message: AgentMessage },
    /// 工具开始执行；此时 `args` 还没有经过 `prepareArguments` 改写。
    #[serde(rename = "tool_execution_start")]
    ToolExecutionStart {
        /// 对应的 `toolCall.id`。
        tool_call_id: String,
        /// 工具名。
        tool_name: String,
        /// 模型给出的原始参数（未经 `prepareArguments` / schema 校验）。
        #[serde(serialize_with = "serialize_js_json")]
        args: JsonValue,
    },
    /// 工具的流式回报（工具调用 `onUpdate` 回调时发出）。
    #[serde(rename = "tool_execution_update")]
    ToolExecutionUpdate {
        /// 对应的 `toolCall.id`。
        tool_call_id: String,
        /// 工具名。
        tool_name: String,
        /// 本次调用的参数（`prepareArguments` 改写后的原始调用对象，不是校验用的副本）。
        #[serde(serialize_with = "serialize_js_json")]
        args: JsonValue,
        /// 工具给出的部分结果。
        #[serde(serialize_with = "serialize_js_json")]
        partial_result: JsonValue,
    },
    /// 工具执行结束。对已执行的调用，`result` 是经过 `afterToolCall` 合并后的最终结果对象；
    /// 准备阶段的立即结果（未知工具 / 参数校验失败 / 被 `beforeToolCall` 拦截 / abort）不
    /// 经过该钩子。
    #[serde(rename = "tool_execution_end")]
    ToolExecutionEnd {
        /// 对应的 `toolCall.id`。
        tool_call_id: String,
        /// 工具名。
        tool_name: String,
        /// 最终结果（`AgentToolResult` 的 JSON 形态）。
        #[serde(serialize_with = "serialize_js_json")]
        result: JsonValue,
        /// 最终错误判定（`afterToolCall` 可以覆盖工具自身上报的值）。
        is_error: bool,
        /// Milliseconds `execute()` took; absent when the tool did not run.
        ///
        /// 只有真正执行过的工具才有值；未知工具、校验失败、被拦截的调用为 `None`。
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            serialize_with = "serialize_duration_ms"
        )]
        duration_ms: Option<f64>,
    },
}

impl AgentEvent {
    /// `type` 字面量。
    pub fn kind(&self) -> &'static str {
        match self {
            AgentEvent::AgentStart => "agent_start",
            AgentEvent::AgentEnd { .. } => "agent_end",
            AgentEvent::TurnStart => "turn_start",
            AgentEvent::TurnEnd { .. } => "turn_end",
            AgentEvent::MessageStart { .. } => "message_start",
            AgentEvent::MessageUpdate { .. } => "message_update",
            AgentEvent::MessageEnd { .. } => "message_end",
            AgentEvent::ToolExecutionStart { .. } => "tool_execution_start",
            AgentEvent::ToolExecutionUpdate { .. } => "tool_execution_update",
            AgentEvent::ToolExecutionEnd { .. } => "tool_execution_end",
        }
    }
}

// ---------------------------------------------------------------------------
// Hooks / loop config
// ---------------------------------------------------------------------------

/// `BeforeToolCallResult`。
///
/// `beforeToolCall` 的返回值；`None`（钩子没有返回结果）与 `Default` 等价：不阻止、不终止。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct BeforeToolCallResult {
    /// `Some(true)` 阻止本次工具执行；agent loop 用 `reason` 合成错误结果。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub block: Option<bool>,
    /// 阻止原因；只在 `block == Some(true)` 时使用，空字符串回退到
    /// `"Tool execution was blocked"`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// `Some(true)` 时给被阻止的结果带上 `terminate`；批次是否停止要看所有调用是否都
    /// `terminate`（`should_terminate_tool_batch`）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terminate: Option<bool>,
}

/// `AfterToolCallResult`。
///
/// TS 的合并语义逐字段用 `??`（`finalizeExecutedToolCall`）：`null` 与 `undefined`
/// **等价**，都表示“未提供、沿用已执行结果”。因此这里所有字段都把 JSON `null` 收成
/// `None`（与同结构体的 `content`/`usage`/`isError`/`terminate` 一致），
/// 这样 agent loop 用 `Option::or` 合并时不会让一个 `details: null` 覆盖已执行结果。
/// Rust 钩子直接构造结构体时同理应传 `None`；若传 `Some(Value::Null)`，agent loop 的
/// 合并也必须按 TS 的 `??` 把 `null` 当成“未提供”（例如 `Option::or` 前先把
/// `Some(Null)` 归一成 `None`）。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AfterToolCallResult {
    /// 替换工具结果的内容块。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<Vec<kiana_ai::types::UserContentBlock>>,
    /// 替换 `details`；`Some(null)` 与 `None` 一样表示“未提供”（见结构体说明）。
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        serialize_with = "serialize_js_json"
    )]
    pub details: Option<JsonValue>,
    /// 替换 `structuredContent`；只改 `content` 而不提供新值时，旧的结构化结果会被丢弃。
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        serialize_with = "serialize_js_json"
    )]
    pub structured_content: Option<JsonValue>,
    /// 覆盖最终错误判定；`None` 沿用执行结果。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_error: Option<bool>,
    /// 覆盖工具用量。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<Usage>,
    /// 覆盖 `terminate`；与批次内其它调用的值一起决定是否停止继续工具调用。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terminate: Option<bool>,
}

/// 工具参数的共享句柄（`BeforeToolCallContext.args`）。
///
/// TS 的 `beforeToolCall` 收到的 `args` 是校验后参数的**同一个对象引用**：钩子就地修改
/// 参数后，`execute()` 直接收到修改后的值，不重新校验（测试 #528）。Rust 的值语义无法
/// 表达引用共享，这里用 `Arc<Mutex<JsonValue>>` 承载同一份参数：钩子用 [`Self::update`]
/// 改写、[`Self::set`] 整体替换，agent loop 在调用 `execute()` 时读取同一份值。
#[derive(Debug, Clone)]
pub struct ToolArguments(std::sync::Arc<std::sync::Mutex<JsonValue>>);

impl ToolArguments {
    /// 用校验后的参数创建句柄。
    pub fn new(value: JsonValue) -> Self {
        ToolArguments(std::sync::Arc::new(std::sync::Mutex::new(value)))
    }

    /// 读取当前参数的克隆（不是引用；要修改请用 [`Self::update`] 或 [`Self::set`]）。
    pub fn get(&self) -> JsonValue {
        self.0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    /// 整体替换参数。
    pub fn set(&self, value: JsonValue) {
        *self
            .0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = value;
    }

    /// 就地修改参数（TS 的 `args.foo = ...`）。
    ///
    /// 闭包在互斥锁内执行，修改对同一个句柄的其他使用者立即可见；闭包是同步的，
    /// 应只做 JSON 修改。
    pub fn update<R>(&self, f: impl FnOnce(&mut JsonValue) -> R) -> R {
        let mut guard = self
            .0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        f(&mut guard)
    }
}

impl From<JsonValue> for ToolArguments {
    fn from(value: JsonValue) -> Self {
        ToolArguments::new(value)
    }
}

/// `BeforeToolCallContext`。
///
/// `context` 是运行期上下文的**快照**：TS 传的是同一个对象引用，钩子就地修改
/// `context.messages` 会影响后续请求；Rust 的钩子按值接收（[`AgentContext`] 由 `Vec`
/// 组成，无法表达别名），对 `context` 的修改不会回写到运行（`args` 例外，见
/// [`ToolArguments`]）。
#[derive(Clone)]
pub struct BeforeToolCallContext {
    /// 当前 assistant 消息的**快照**（含本次 `toolCall` 内容块）。
    pub assistant_message: AssistantMessage,
    /// 本次工具调用；`prepareArguments` 的就地改写已反映在 `arguments` 上。
    pub tool_call: AgentToolCall,
    /// Validated tool arguments. 钩子可就地改写；改写结果直接传给 `execute()`（TS #528）。
    pub args: ToolArguments,
    /// 运行期上下文快照；对它的修改不会回写。
    pub context: AgentContext,
}

/// `AfterToolCallContext`。
///
/// `context` 与 [`BeforeToolCallContext::context`] 一样是运行期上下文的快照。
#[derive(Clone)]
pub struct AfterToolCallContext {
    /// 当前 assistant 消息的快照。
    pub assistant_message: AssistantMessage,
    /// 本次工具调用。
    pub tool_call: AgentToolCall,
    /// 校验后（可能经 `beforeToolCall` 改写）的参数**快照**；此时修改它不会影响执行结果。
    pub args: JsonValue,
    /// 已执行、但尚未合并本钩子返回值的工具结果。
    pub result: AgentToolResult<JsonValue>,
    /// 执行阶段的错误判定（本钩子的返回值可以覆盖它）。
    pub is_error: bool,
    /// 运行期上下文快照；对它的修改不会回写。
    pub context: AgentContext,
}

/// `AgentTurnContext`（`finishTurn` / `prepareNextTurn` 的入参）。
///
/// `context` / `new_messages` 是运行期对象的**快照**：TS 里它们是 live reference
/// （`lastCompletedTurn = { ..., context: currentContext, newMessages }`），钩子就地
/// `push` 会进入下一次请求与 `agent_end` 的 `messages`；Rust 的钩子按值接收克隆，
/// 修改不会回写到运行。只读访问与 TS 等价。
#[derive(Clone)]
pub struct AgentTurnContext {
    /// 本轮最终的 assistant 消息（可能包含 `toolCall` 内容块）。
    pub message: AssistantMessage,
    /// 本轮执行的工具结果；没有工具调用时为空。
    pub tool_results: Vec<ToolResultMessage>,
    /// 本轮结束时的上下文快照（历史消息与工具）。
    pub context: AgentContext,
    /// 本次 run 到本轮为止产生的全部新消息。
    pub new_messages: Vec<AgentMessage>,
}

/// `AgentTurnDecision`（`{action:"continue"} | {action:"end"}`）。
///
/// `finishTurn` 的返回值：`End` 立即结束 loop（不再轮询 steering / follow-up）；
/// `Continue` 表示“即使没有更多工具调用，也再发一次只使用当前上下文的请求”；
/// `None` 按缺省规则处理（有工具调用/排队消息才继续）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "lowercase")]
pub enum AgentTurnDecision {
    /// 继续一次请求（TS `{action:"continue"}`）。
    Continue,
    /// 结束本次 run（TS `{action:"end"}`）。
    End,
}

/// `AgentLoopTurnUpdate`。
///
/// `prepareNextTurn` 的返回值：只有 `Some` 字段生效，`None` 表示不修改。
/// `context` 会整体替换当前上下文；`messages` 会排在下一轮请求之前（与 steering 消息一起）；
/// `model` / `thinking_level` 更新后续请求的配置。
#[derive(Clone, Default)]
pub struct AgentLoopTurnUpdate {
    /// 替换当前上下文（历史消息与工具）。
    pub context: Option<AgentContext>,
    /// 追加到下一轮请求之前的消息。
    pub messages: Option<Vec<AgentMessage>>,
    /// 替换后续请求使用的模型。
    pub model: Option<Model>,
    /// 替换后续请求的 thinking 档位。
    pub thinking_level: Option<ThinkingLevel>,
}

/// `PrepareRequestContext`。
///
/// `context` 是运行期上下文的**快照**（同 [`AgentTurnContext`]）：TS 的钩子拿到同一个
/// 对象引用，就地修改会影响紧随其后的 provider 请求；Rust 的修改不会回写。
#[derive(Clone)]
pub struct PrepareRequestContext {
    /// 当前上下文快照。
    pub context: AgentContext,
    /// 本次请求将要使用的模型。
    pub model: Model,
    /// 当前 thinking 档位（`config.options.reasoning` 缺省时为 `Off`）。
    pub thinking_level: ThinkingLevel,
}

/// `AgentRequestUpdate`（`Omit<AgentLoopTurnUpdate, "messages">`）。
///
/// `prepareRequest` 的返回值：同样只覆盖 `Some` 字段；它不能追加消息，只能替换
/// `context` / `model` / `thinking_level`。
#[derive(Clone, Default)]
pub struct AgentRequestUpdate {
    /// 替换当前上下文。
    pub context: Option<AgentContext>,
    /// 替换本次请求使用的模型。
    pub model: Option<Model>,
    /// 替换本次请求的 thinking 档位。
    pub thinking_level: Option<ThinkingLevel>,
}

/// `PrepareNextTurnContext`。
///
/// 与 [`AgentTurnContext`] 是同一个类型（TS 的类型别名），含义见后者。
pub type PrepareNextTurnContext = AgentTurnContext;

/// `StreamFn`：`(model, context, options) -> AssistantMessageEventStream | Promise<...>`。
///
/// 参数依次是模型、已归一化的 transcript（system prompt 与工具声明都在首条 system 消息
/// 里，见 `TranscriptContext`）、本次请求的选项（agent loop 会填入解析后的 API key、
/// 合并后的 identity 与取消令牌）。建流失败用 `Err(AiError)` 返回；流开始后的失败通过
/// `AssistantMessageEvent` 的 `error` 事件表达。
///
/// `Arc<dyn Fn .. + Send + Sync>` 让同一个流函数可以被多个 run 共享；`BoxFuture<'static>`
/// 表示返回的 future 不借用入参，可以脱离调用栈继续执行。
pub type StreamFn = std::sync::Arc<
    dyn Fn(
            Model,
            TranscriptContext,
            Option<SimpleStreamOptions>,
        ) -> BoxFuture<
            'static,
            Result<kiana_ai::event_stream::AssistantMessageEventStream, kiana_ai::AiError>,
        > + Send
        + Sync,
>;

/// `AgentLoopConfig`：`SimpleStreamOptions` 的扩展 + 必需回调。
///
/// 所有钩子都是 `Option<Arc<dyn Fn ...>>`；TS 的同步/异步混用（`T | Promise<T>`）
/// 统一为返回 `BoxFuture`。
pub struct AgentLoopConfig {
    /// `SimpleStreamOptions` 的字段（含 signal）。
    ///
    /// `base.signal` 在每次请求前会被本次 run 的取消令牌覆盖；`base.identity` 会先与默认
    /// 身份合并（请求级字段覆盖默认值）再使用。
    pub options: SimpleStreamOptions,
    /// 本次运行使用的模型；`prepareRequest` / `prepareNextTurn` 可以替换它。
    pub model: Model,
    /// `AgentMessage[] -> Message[]`；必须不抛异常。
    ///
    /// 返回的 `Message` 是 provider 能消费的标准消息；自定义消息是否保留由实现决定。
    pub convert_to_llm:
        std::sync::Arc<dyn Fn(Vec<AgentMessage>) -> Vec<kiana_ai::types::Message> + Send + Sync>,
    /// 在 `convert_to_llm` 之前对消息序列做变换（例如压缩/裁剪）；返回新序列。
    pub transform_context: Option<TransformContextFn>,
    /// 按 provider 解析 API key；返回空字符串会被当成“未提供”，回退到 `options.base.api_key`。
    pub get_api_key: Option<GetApiKeyFn>,
    /// 每轮结束时的回调；返回 [`AgentTurnDecision::End`] 结束 loop。
    pub finish_turn: Option<FinishTurnFn>,
    /// 每次 provider 请求前调用，可替换 context / model / thinking level。
    pub prepare_request: Option<PrepareRequestFn>,
    /// 每轮结束后、下一次请求前调用，可替换 context / 追加 messages / 换模型。
    pub prepare_next_turn: Option<PrepareNextTurnFn>,
    /// 轮询 steering 队列；在内层循环的多个时点被调用（返回空表示没有新消息）。
    pub get_steering_messages: Option<GetMessagesFn>,
    /// 内层循环准备停止时取 follow-up 消息；非空则继续下一轮。
    pub get_follow_up_messages: Option<GetMessagesFn>,
    /// 工具批次调度方式；`None` 走并行分支（除非批次内有工具声明 `Sequential`）。
    pub tool_execution: Option<ToolExecutionMode>,
    /// 工具执行前的钩子；`None` 表示不做拦截/改写。
    pub before_tool_call: Option<BeforeToolCallFn>,
    /// 工具执行后的钩子；`None` 表示不合并额外结果。
    pub after_tool_call: Option<AfterToolCallFn>,
    /// 失控保护；`None` 表示不启用。共享句柄让调用方在 run 结束后读取命中详情。
    pub loop_guard: Option<crate::loop_guard::LoopGuardHandle>,
    /// turn / token / cost / wall-clock 预算；`None` 表示不启用。
    pub budget: Option<crate::budget::BudgetHandle>,
}

/// 缺省配置：所有钩子为 `None`，`model` 为 `Model::default()`。
///
/// 缺省的 `convert_to_llm` 只保留 [`AgentMessage::Standard`]，**丢弃所有**
/// [`AgentMessage::Custom`]。注意 `Agent::new` 在调用方未提供 `convert_to_llm` 时用的是
/// `agent.rs` 里的 `default_convert_to_llm`（会把 `role` 为标准四类的 `Custom` 投影回
/// 标准消息），两者行为不同，直接调用 `agent_loop` 时使用的是这里的实现。
impl Default for AgentLoopConfig {
    fn default() -> Self {
        AgentLoopConfig {
            options: SimpleStreamOptions::default(),
            model: Model::default(),
            convert_to_llm: std::sync::Arc::new(|messages| {
                messages
                    .into_iter()
                    .filter_map(|m| match m {
                        AgentMessage::Standard(msg) => Some(msg),
                        AgentMessage::Custom(_) => None,
                    })
                    .collect()
            }),
            transform_context: None,
            get_api_key: None,
            finish_turn: None,
            prepare_request: None,
            prepare_next_turn: None,
            get_steering_messages: None,
            get_follow_up_messages: None,
            tool_execution: None,
            before_tool_call: None,
            after_tool_call: None,
            loop_guard: None,
            budget: None,
        }
    }
}

impl AgentLoopConfig {
    /// 接线 loop guard。
    pub fn with_loop_guard(mut self, guard: crate::loop_guard::LoopGuardHandle) -> Self {
        self.loop_guard = Some(guard);
        self
    }

    /// 接线预算控制器。
    pub fn with_budget(mut self, budget: crate::budget::BudgetHandle) -> Self {
        self.budget = Some(budget);
        self
    }
}

/// `transformContext`。
///
/// 入参是当前 transcript 的快照与本次 run 的取消令牌；返回变换后的完整消息序列，
/// 之后才交给 `convert_to_llm`。`None` 表示不做变换。
pub type TransformContextFn = std::sync::Arc<
    dyn Fn(
            Vec<AgentMessage>,
            Option<tokio_util::sync::CancellationToken>,
        ) -> BoxFuture<'static, Vec<AgentMessage>>
        + Send
        + Sync,
>;
/// `getApiKey`。
///
/// 入参是 provider 名，返回该 provider 的 API key；`None` 或空字符串表示“未解析到”，
/// agent loop 会回退到 `options.base.api_key`。
pub type GetApiKeyFn =
    std::sync::Arc<dyn Fn(String) -> BoxFuture<'static, Option<String>> + Send + Sync>;
/// `finishTurn`。
///
/// 第二个参数是本次 run 的取消令牌（TS 的 `signal?: AbortSignal`），钩子可据此中断
/// 长耗时工作（如压缩）。返回 `None` 表示不干预本轮决策。
pub type FinishTurnFn = std::sync::Arc<
    dyn Fn(
            AgentTurnContext,
            Option<tokio_util::sync::CancellationToken>,
        ) -> BoxFuture<'static, Option<AgentTurnDecision>>
        + Send
        + Sync,
>;
/// `prepareRequest`。
///
/// 第二个参数是取消令牌（TS 的 `signal?: AbortSignal`）。返回 `None` 表示不修改请求。
pub type PrepareRequestFn = std::sync::Arc<
    dyn Fn(
            PrepareRequestContext,
            Option<tokio_util::sync::CancellationToken>,
        ) -> BoxFuture<'static, Option<AgentRequestUpdate>>
        + Send
        + Sync,
>;
/// `prepareNextTurn`（TS 不给这个钩子传 signal）。
///
/// 返回 `None` 表示不做更新；`Some(update)` 的字段按 [`AgentLoopTurnUpdate`] 的规则应用。
pub type PrepareNextTurnFn = std::sync::Arc<
    dyn Fn(PrepareNextTurnContext) -> BoxFuture<'static, Option<AgentLoopTurnUpdate>> + Send + Sync,
>;
/// `getSteeringMessages` / `getFollowUpMessages`。
///
/// 无参数、返回一批 `AgentMessage`；返回空向量表示队列为空。steering 版本会在内层循环的
/// 多个时点被反复调用，follow-up 版本只在内层循环准备停止时调用。实现应返回并取走当前
/// 队列中的消息（`Agent` 用 `drain()`）；若反复返回未消费的同一批消息，循环会重复投递。
pub type GetMessagesFn =
    std::sync::Arc<dyn Fn() -> BoxFuture<'static, Vec<AgentMessage>> + Send + Sync>;
/// `beforeToolCall`。
///
/// 第二个参数是取消令牌（TS 的 `signal?: AbortSignal`）。返回 `None` 表示放行；
/// `Some(result)` 见 [`BeforeToolCallResult`]。
pub type BeforeToolCallFn = std::sync::Arc<
    dyn Fn(
            BeforeToolCallContext,
            Option<tokio_util::sync::CancellationToken>,
        ) -> BoxFuture<'static, Option<BeforeToolCallResult>>
        + Send
        + Sync,
>;
/// `afterToolCall`。
///
/// 第二个参数是取消令牌（TS 的 `signal?: AbortSignal`）。返回 `None` 表示沿用执行结果；
/// `Some(result)` 会按 `??` 语义逐字段合并（见 [`AfterToolCallResult`]）。
pub type AfterToolCallFn = std::sync::Arc<
    dyn Fn(
            AfterToolCallContext,
            Option<tokio_util::sync::CancellationToken>,
        ) -> BoxFuture<'static, Option<AfterToolCallResult>>
        + Send
        + Sync,
>;

/// `AssistantMessage` 的再导出别名（公开面的一部分，便于调用方不写 `kiana_ai` 路径）。
pub use kiana_ai::types::AssistantMessage as AgentAssistantMessage;
/// `TextContent` 的再导出别名（公开面的一部分）。
pub use kiana_ai::types::TextContent as AgentTextContent;
