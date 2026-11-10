//! 嵌套工具调用（`ctx.executeTool()`）的记录与串行化执行。
//!
//! TS: `packages/coding-agent/src/core/nested-tool-calls.ts`。
//!
//! agent loop 不知道这些调用：会话把每个嵌套调用通过 agent 的工具管线
//! （`runToolCall`）执行，发出带 `parentToolCallId` 的 `tool_execution_*` 事件，并把调用
//! 记录与用量写回模型发起的工具调用的 tool result 消息。
//!
//! # Rust 侧形状
//!
//! - `NestedToolCalls.calls` 在 `kiana-ai` 里是 `Vec<JsonValue>`，因此每条记录按 TS 的
//!   **插入顺序**手工构造（`id, name, status, arguments|argumentsBytes, durationMs, error`）。
//! - `NestedCallRecorder` 以「记录下标」代替 TS 的对象引用（`startedAt: Map<record, number>`）。
//! - `NestedToolCallRunner` 对 host 泛型化（Host trait 的异步方法用 `Pin<Box<dyn Future>>`，
//!   不需要 `async-trait`），保持对象安全之外的零成本。
//!
//! # 为什么需要这一层
//!
//! agent loop 只认识模型消息里的 tool call（assistant 消息的 toolCall 块与配对的
//! toolResult 消息）。`ctx.executeTool()` 是扩展脚本在**工具执行期间**临时发起的调用，
//! 既不属于模型请求，也不应该在消息流里多出一对 assistant/toolResult。本模块把它限制在
//! “父工具调用的作用域”内：事件带 `parentToolCallId` 供 UI 归属，记录与用量最终由调用方
//! 合回父 toolResult 消息（`nestedCalls` 字段与合并后的 usage），agent loop 无感知。
//!
//! # 一次嵌套调用的执行顺序
//!
//! 1. 以调用方 id 在作用域表里取（或建立）内部作用域，分配调用 id `<callerId>/<n>`
//!    （`n` 从 1 开始；每次 `execute` 都自增，**即使记录被限流丢弃也会消耗一个编号**）。
//! 2. `NestedCallRecorder` 记录初态：参数超限时只保留字节数，条数超限时整条丢弃。
//! 3. 发出 `tool_execution_start`（`args` 为原始 JSON 值，`null` 已归一为 `{}`）。
//! 4. 判定是否独占：host 声明串行、或该工具 `execution_mode == Sequential`、且该调用自身
//!    不持有队列时，在 runner 的 `tokio::sync::Mutex<()>` 上排队。
//! 5. 以 `<callerId>/<n>` 注册临时作用域（它的子调用共享同一个 recorder），然后调用
//!    `NestedToolCallHost::run_tool_call` 真正执行工具。
//! 6. 移除临时作用域、释放队列、写回状态/耗时/错误、累加结果 usage，最后发
//!    `tool_execution_end`。
//!
//! 父 toolResult 消息的落盘由调用方完成：`kiana-session` 在 `message_start` 事件上调用
//! [`crate::nested_tool_calls::NestedToolCallRunner::take_record`]，把摘要填进消息。
//!
//! # 并发模型与锁
//!
//! - `scopes` 用 `std::sync::Mutex` 保护，只在同步语句块内持锁，**不跨 `await`**。所有取锁点
//!   都写成 `unwrap_or_else(|e| e.into_inner())`，即**忽略锁毒化**（持锁线程 panic 后继续
//!   使用内部数据）。
//! - 串行化用 `tokio::sync::Mutex<()>`，独占调用横跨整个工具执行持有它，因此同一 runner 上
//!   的独占调用严格串行；非独占调用不取锁。`holds_queue` 会传给子调用，使独占调用内部的
//!   嵌套调用不再取锁（否则自我死锁）。
//! - recorder 是 `Arc<Mutex<NestedCallRecorder>>`，父调用与其所有层级的子调用共享同一个实例，
//!   因此记录与用量都归到最外层模型工具调用名下。
//!
//! # 失败模式
//!
//! - 工具失败不会变成 `Err`：`execute` 永远返回 `AgentToolCallOutcome`，只把 `is_error`
//!   写进记录与 `tool_execution_end`。
//! - 记录超限只影响 `snapshot()` 的内容与 `complete`，不影响调用本身是否执行。
//! - 本模块不捕获 panic：`run_tool_call` 展开时，临时作用域不会被移除，`finish` 与 `End`
//!   事件也会被跳过（不承诺 panic 后的一致状态）；队列锁随 guard 的 Drop 释放。

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use kiana_agent_core::types::AgentToolCall;
use kiana_agent_core::{AgentTool, AgentToolCallOutcome, AgentToolResult, AgentToolUpdateCallback};
use kiana_ai::types::{JsonObject, JsonValue, NestedToolCalls, Usage, UserContentBlock};
use kiana_ai::utils::abort::AbortScope;
use serde::Serialize;

use crate::usage_totals::combine_usage;

/// 嵌套调用记录的限制。
///
/// 四个上限共同决定 toolResult 消息里 `nestedCalls` 的体积；`NESTED_CALL_LIMITS` 是 TS 的
/// 同一组常量。达到调用数或参数上限会把记录器的 `complete` 置为 `false`（见
/// [`NestedCallRecorder::snapshot`]），让消费方能区分“完整记录”与“被截断的记录”；
/// `max_error_chars` 只做文本截断，不影响 `complete`。
///
/// TS: `nested-tool-calls.ts#NESTED_CALL_LIMITS`
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NestedCallLimits {
    /// 单条 tool result 上最多记录的调用数。
    ///
    /// 达到后 [`NestedCallRecorder::start_with_arguments`] 返回 `None`，调用仍会执行但不再记录。
    pub max_calls: usize,
    /// 单次调用的 arguments 上限（UTF-8 字节）。
    ///
    /// 按 `JSON.stringify(arguments)` 文本的 UTF-8 字节数计（不是字符数，含引号与转义）。
    pub max_argument_bytes_per_call: usize,
    /// 一条 tool result 上 arguments 总量上限（UTF-8 字节）。
    ///
    /// 只累计**实际保存**了 `arguments` 的记录；被省略的调用不占预算。
    pub max_argument_bytes_total: usize,
    /// 错误文本最多保留的 UTF-16 码元数。
    ///
    /// 截断由 [`truncate_utf16_units`] 完成：不拆代理对，因此结果可能比 TS 的 `slice(0, n)`
    /// 少一个码元。
    pub max_error_chars: usize,
}

/// 常量实例（TS 同名常量）：256 条、单条 8 KiB、合计 32 KiB、错误 500 UTF-16 码元。
///
/// TS: `nested-tool-calls.ts#NESTED_CALL_LIMITS`
pub const NESTED_CALL_LIMITS: NestedCallLimits = NestedCallLimits {
    max_calls: 256,
    max_argument_bytes_per_call: 8 * 1024,
    max_argument_bytes_total: 32 * 1024,
    max_error_chars: 500,
};

/// 一条嵌套调用的状态（TS 字符串字面量 `"unfinished" | "ok" | "error"`）。
///
/// 首次 `finish` 把 `Unfinished` 置为 `Ok` 或 `Error`；`finish` 是公开方法，对同一下标
/// 重复调用会无条件覆盖为新的 `Ok`/`Error`（`Ok → Error`、`Error → Ok` 与同值覆盖都
/// 可达），但不会回到 `Unfinished`。[`NestedCallRecorder::snapshot`] 的 `complete`
/// 要求所有记录都已离开 `Unfinished`。
///
/// TS: `NestedToolCallRecord["status"]`
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum NestedCallStatus {
    /// 尚未结束（记录刚创建时的默认值）。
    #[default]
    Unfinished,
    /// 成功（`finish(is_error = false)`）。
    Ok,
    /// 失败（`finish(is_error = true)`）。
    Error,
}

impl NestedCallStatus {
    /// 写进 `nestedCalls[].status` 的 TS 字面量。
    /// TS: nested-tool-calls.ts#NestedCallStatus.asStr
    pub fn as_str(self) -> &'static str {
        match self {
            NestedCallStatus::Unfinished => "unfinished",
            NestedCallStatus::Ok => "ok",
            NestedCallStatus::Error => "error",
        }
    }
}

/// 一条嵌套调用的记录。
///
/// 写入 JSON 由 [`NestedToolCallRecord::to_json`] 手工完成（字段顺序固定），不是 serde 派生：
/// `kiana-ai` 的 `NestedToolCalls.calls` 是 `Vec<JsonValue>`，记录形状必须由本模块保证。
/// `arguments` 与 `arguments_bytes` 互斥：超限时只写字节数，未超限才写参数。
///
/// TS: `kiana-ai#NestedToolCallRecord`
#[derive(Debug, Clone, Default, PartialEq)]
pub struct NestedToolCallRecord {
    /// 调用 id（`<callerId>/<n>`）。
    pub id: String,
    /// 工具名。
    pub name: String,
    /// 状态。
    pub status: NestedCallStatus,
    /// 未超限时的参数（TS 的 `JSON.parse(JSON.stringify(arguments ?? {}))`：任意 JSON 值，
    /// 不只是对象）。
    pub arguments: Option<JsonValue>,
    /// 超限时的参数字节数（此时不保存 `arguments`）。
    pub arguments_bytes: Option<u64>,
    /// 执行耗时（毫秒，`Math.round`）。
    ///
    /// 由 [`NestedCallRecorder::finish`] 用 `Instant` 从记录起点计时；记录未被 `finish`
    /// 时保持 `None`（JSON 中省略该键）。
    pub duration_ms: Option<f64>,
    /// 错误文本（最多 500 UTF-16 码元）。
    ///
    /// 只有 `is_error` 且文本非空时才会写入。
    pub error: Option<String>,
}

impl NestedToolCallRecord {
    /// 转成落盘/上屏使用的 JSON 对象（键序与 TS 记录一致）。
    ///
    /// 键序固定为 `id, name, status, arguments|argumentsBytes, durationMs?, error?`：
    ///
    /// - `argumentsBytes` 优先于 `arguments`（正常路径不会同时设置）；
    /// - 整数值的 `durationMs` 写成 JSON 整数，非整数才走 f64（`NaN`/无穷退化为 `null`）；
    /// - 值为 `None` 的字段不写键，对应 TS 的 `undefined` 被 `JSON.stringify` 省略。
    ///
    /// 键序依赖 workspace 对 `serde_json` 启用的 `preserve_order`（见根 `Cargo.toml`）：
    /// `serde_json::Map` 因此保持插入顺序。
    /// TS: nested-tool-calls.ts#NestedToolCallRecord.toJson
    pub fn to_json(&self) -> JsonValue {
        let mut object = JsonObject::new();
        object.insert("id".to_string(), JsonValue::String(self.id.clone()));
        object.insert("name".to_string(), JsonValue::String(self.name.clone()));
        object.insert(
            "status".to_string(),
            JsonValue::String(self.status.as_str().to_string()),
        );
        if let Some(arguments_bytes) = self.arguments_bytes {
            object.insert(
                "argumentsBytes".to_string(),
                JsonValue::Number(serde_json::Number::from(arguments_bytes)),
            );
        } else if let Some(arguments) = &self.arguments {
            object.insert("arguments".to_string(), arguments.clone());
        }
        if let Some(duration_ms) = self.duration_ms {
            let value = if duration_ms.fract() == 0.0
                && duration_ms >= i64::MIN as f64
                && duration_ms <= i64::MAX as f64
            {
                JsonValue::Number(serde_json::Number::from(duration_ms as i64))
            } else {
                serde_json::Number::from_f64(duration_ms)
                    .map(JsonValue::Number)
                    .unwrap_or(JsonValue::Null)
            };
            object.insert("durationMs".to_string(), value);
        }
        if let Some(error) = &self.error {
            object.insert("error".to_string(), JsonValue::String(error.clone()));
        }
        JsonValue::Object(map_to_serde(object))
    }
}

/// `IndexMap` → `serde_json::Map`。
///
/// workspace 启用了 `serde_json` 的 `preserve_order`（见根 `Cargo.toml`），因此这里的插入
/// 顺序就是最终 JSON 的键序；[`NestedToolCallRecord::to_json`] 依赖这一点还原 TS 的字段顺序。
fn map_to_serde(object: JsonObject) -> serde_json::Map<String, JsonValue> {
    let mut map = serde_json::Map::with_capacity(object.len());
    for (key, value) in object {
        map.insert(key, value);
    }
    map
}

/// 一条模型发起的工具调用留下的嵌套调用摘要。
///
/// 由 [`NestedToolCallRunner::take_record`] 产生；调用方把 `calls` 写进 toolResult 消息的
/// `nestedCalls`，把 `usage` 与消息自身 usage 用 `combine_usage` 合并。
///
/// TS: `nested-tool-calls.ts#NestedCallSummary`
#[derive(Debug, Clone, PartialEq)]
pub struct NestedCallSummary {
    /// 成为 `nestedCalls`；没有任何嵌套调用（或记录器为空且完整）时为 `None`。
    pub calls: Option<NestedToolCalls>,
    /// 嵌套结果用量之和（加到消息自身的 `usage` 上）；没有任何结果上报用量时为 `None`。
    pub usage: Option<Usage>,
}

/// 嵌套调用执行选项。
///
/// `signal` 只透传给 [`NestedToolCallHost::run_tool_call`]；runner 自身在排队与等待期间不
/// 检查取消（队列锁没有可中断的获取方式）。`on_update` 是调用方回调，先于
/// `tool_execution_update` 事件被调用。
///
/// TS: `nested-tool-calls.ts#NestedToolCallOptions`
#[derive(Clone, Default)]
pub struct NestedToolCallOptions {
    /// 默认使用发起调用的工具的 signal。
    pub signal: Option<AbortScope>,
    /// 接收嵌套工具的流式结果。
    pub on_update: Option<AgentToolUpdateCallback>,
}

/// `tool_execution_*` 事件（带 `parentToolCallId`）。
///
/// serde 形状：`type` 为判别键（`tool_execution_start|update|end`），其余字段 camelCase。
/// 与 agent loop 自己发出的同名事件相比只多一个 `parent_tool_call_id`，供消费方把子调用
/// 归属到父工具调用。[`NestedToolExecutionEvent::End`] 的 `duration_ms` 来自
/// `AgentToolCallOutcome.duration_ms`（工具未真正运行时为 `None`，
/// 键被省略），与记录里的 `durationMs`（runner 自己计时）是两个独立的值。
///
/// TS: `nested-tool-calls.ts#NestedToolExecutionEvent`
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "type", rename_all_fields = "camelCase")]
pub enum NestedToolExecutionEvent {
    /// 开始。
    #[serde(rename = "tool_execution_start")]
    Start {
        /// 嵌套调用 id。
        tool_call_id: String,
        /// 工具名。
        tool_name: String,
        /// 参数。
        args: JsonValue,
        /// 发起调用的模型工具调用 id。
        parent_tool_call_id: String,
    },
    /// 流式更新。
    #[serde(rename = "tool_execution_update")]
    Update {
        /// 嵌套调用 id。
        tool_call_id: String,
        /// 工具名。
        tool_name: String,
        /// 参数。
        args: JsonValue,
        /// 部分结果。
        partial_result: AgentToolResult<JsonValue>,
        /// 发起调用的模型工具调用 id。
        parent_tool_call_id: String,
    },
    /// 结束。
    #[serde(rename = "tool_execution_end")]
    End {
        /// 嵌套调用 id。
        tool_call_id: String,
        /// 工具名。
        tool_name: String,
        /// 完整结果。
        result: AgentToolResult<JsonValue>,
        /// 是否失败。
        is_error: bool,
        /// 耗时（毫秒），工具未运行时缺省。
        #[serde(skip_serializing_if = "Option::is_none")]
        duration_ms: Option<f64>,
        /// 发起调用的模型工具调用 id。
        parent_tool_call_id: String,
    },
}

/// `Pin<Box<dyn Future>>` 别名（host 的异步方法用它，避免 `async-trait` 依赖）。
///
/// `Send` 约束保证返回的 future 能嵌进同样要求 `Send` 的异步调用链里（`execute` 的 future
/// 会被调用方跨任务持有）。
/// TS: nested-tool-calls.ts#LocalFuture
pub type LocalFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// 嵌套调用的宿主（会话实现工具管线与事件发射）。
///
/// 契约：
///
/// - `get_tools` 返回可被嵌套调用解析的工具集合。runner 在判定独占时会用它查找名字对应的
///   `execution_mode`（host 已把 `is_sequential` 返回为 true 时短路，不再查找）；**找不到
///   该名字时按非串行处理**。
/// - `run_tool_call` 必须执行工具，并在工具产生流式结果时调用传入的 `on_update`。它不应
///   自己发 [`NestedToolExecutionEvent`]：事件由 runner 统一负责，`parentToolCallId` 也由
///   runner 写入事件。签名仍把该 id 作为 `parent_tool_call_id` 交给 host，实现方可拿它归属
///   父调用（`kiana-session` 的 `SessionNestedHost` 用它驱动 before/after 工具钩子）。
/// - `emit` 会被 `await`，因此实现方可以用它给事件投递施加背压。
///
/// 本 trait 只要求 `Sync`：runner 通过 `&self` 调用，多个 `execute` future 可以并发共享同一
/// 个 runner。
///
/// TS: `nested-tool-calls.ts#NestedToolCallHost`
///
/// `run_tool_call` 对更新回调泛型化（runner 本身对 host 泛型），因此不需要 `async-trait`；
/// `emit` 使用手工的 `Pin<Box<dyn Future>>`。
pub trait NestedToolCallHost: Sync {
    /// 嵌套调用可解析的工具集合。
    fn get_tools(&self) -> &[AgentTool];
    /// 是否所有嵌套调用都以独占方式执行（如 agent 串行执行工具调用）。
    fn is_sequential(&self) -> bool;
    /// 通过工具管线执行调用，并以上报 `parentToolCallId` 的钩子报告更新。
    fn run_tool_call<'a, F>(
        &'a self,
        tool_call: AgentToolCall,
        parent_tool_call_id: &'a str,
        signal: Option<&'a AbortScope>,
        on_update: F,
    ) -> LocalFuture<'a, AgentToolCallOutcome>
    where
        F: Fn(AgentToolResult<JsonValue>) -> LocalFuture<'a, ()> + Send + 'a;
    /// 发出事件。
    fn emit<'a>(&'a self, event: NestedToolExecutionEvent) -> LocalFuture<'a, ()>;
}

/// 收集一条模型发起的工具调用的嵌套调用（含嵌套工具发起的调用）。
///
/// 一个 recorder 对应一个作用域（父调用 id）。`calls` 与 `started_at` 用**同一下标**关联：
/// TS 用记录对象本身作 `Map` 键（`startedAt: Map<record, number>`），Rust 无法用引用建键，
/// 因此改用下标（见模块文档）。
///
/// 构造请用 [`NestedCallRecorder::new`]：`#[derive(Default)]` 得到的实例 `complete` 为
/// `false`，与 `new()` 的 `true` 不同。
///
/// TS: `nested-tool-calls.ts#NestedCallRecorder`
#[derive(Debug, Default)]
pub struct NestedCallRecorder {
    /// 已接受的记录，按 `start` 调用顺序排列（即 `nestedCalls.calls` 的顺序）。
    calls: Vec<NestedToolCallRecord>,
    /// 与 `calls` 同下标的开始时刻；`finish` 后置为 `None`。
    started_at: Vec<Option<Instant>>,
    /// 未被限流时为 `true`；发生过丢弃或参数省略后固定为 `false`。
    complete: bool,
    /// 已保存的 arguments 的 JSON 文本字节数之和（总预算用量）。
    argument_bytes: usize,
    /// 嵌套结果用量累计；`None` 表示还没有结果上报过用量。
    usage: Option<Usage>,
}

impl NestedCallRecorder {
    /// 新建记录器（`complete` 初始为 true）。
    /// TS: nested-tool-calls.ts#new
    pub fn new() -> Self {
        NestedCallRecorder {
            calls: Vec::new(),
            started_at: Vec::new(),
            complete: true,
            argument_bytes: 0,
            usage: None,
        }
    }

    /// 记录一次开始；被丢弃时返回 `None`。
    ///
    /// 把 `AgentToolCall.arguments`（对象）包成 JSON 对象后交给
    /// [`NestedCallRecorder::start_with_arguments`]。返回记录下标（供
    /// [`NestedCallRecorder::finish`]）；条数已达上限时返回 `None`。
    /// TS: nested-tool-calls.ts#start
    pub fn start(&mut self, tool_call: &AgentToolCall) -> Option<usize> {
        let arguments = JsonValue::Object(map_to_serde(tool_call.arguments.clone()));
        self.start_with_arguments(tool_call, &arguments)
    }

    /// [`NestedCallRecorder::start`] 的原始参数版本。
    ///
    /// TS 的 `toolCall.arguments` 在运行期可以是任意 JSON 值（`ctx.executeTool()` 的
    /// `args` 原样传入）：`JSON.stringify(toolCall.arguments ?? {})` 计字节，
    /// `JSON.parse(json)` 保存原始值。
    ///
    /// # 限流规则（与 TS 逐条对应）
    ///
    /// 1. 记录数达到 `max_calls`：不 push，`complete = false`，返回 `None`。
    /// 2. `bytes > max_argument_bytes_per_call`，或 `argument_bytes + bytes >
    ///    max_argument_bytes_total`：不保存 `arguments`，改记 `argumentsBytes`，
    ///    `complete = false`。
    /// 3. 否则保存 `JSON.parse(JSON.stringify(arguments))` 的规范化副本，并把 `bytes` 计入
    ///    总预算。
    ///
    /// `bytes` 是 JSON 文本的 UTF-8 字节数（`String::len`），含引号与转义。传入
    /// `JsonValue::Null` 时按 TS 的 `?? {}` 归一为空对象，因此这里不会记录
    /// `arguments: null`。总预算只累计实际保存的参数，被省略的调用不占预算。
    pub fn start_with_arguments(
        &mut self,
        tool_call: &AgentToolCall,
        arguments: &JsonValue,
    ) -> Option<usize> {
        // TS: `JSON.stringify(toolCall.arguments ?? {})`。
        let null_object = JsonValue::Object(serde_json::Map::new());
        let arguments = if arguments.is_null() {
            &null_object
        } else {
            arguments
        };
        // 条数上限：丢弃后续记录，但调用本身继续执行（runner 拿到 None 后照常跑工具）。
        if self.calls.len() >= NESTED_CALL_LIMITS.max_calls {
            self.complete = false;
            return None;
        }
        let json = kiana_ai::utils::error_body::safe_json_stringify(arguments);
        let bytes = json.len();
        let mut record = NestedToolCallRecord {
            id: tool_call.id.clone(),
            name: tool_call.name.clone(),
            status: NestedCallStatus::Unfinished,
            ..Default::default()
        };
        // 超限只记字节数：`argumentsBytes` 让消费方知道参数被省略，`complete` 标记记录不完整。
        if bytes > NESTED_CALL_LIMITS.max_argument_bytes_per_call
            || self.argument_bytes + bytes > NESTED_CALL_LIMITS.max_argument_bytes_total
        {
            record.arguments_bytes = Some(bytes as u64);
            self.complete = false;
        } else {
            // `JSON.parse(json)`：与 TS 一样保存规范化后的副本。
            record.arguments = Some(
                serde_json::from_str::<JsonValue>(&json).unwrap_or_else(|_| arguments.clone()),
            );
            self.argument_bytes += bytes;
        }
        self.calls.push(record);
        self.started_at.push(Some(Instant::now()));
        Some(self.calls.len() - 1)
    }

    /// 结束一条记录。
    ///
    /// `index` 为 `None`（记录被限流丢弃）或越界时是空操作。耗时从
    /// [`NestedCallRecorder::start_with_arguments`] 的时刻算起并四舍五入到毫秒；开始时刻被
    /// 取走后，对同一下标重复 `finish` 会把耗时写成 0 并覆盖状态。错误文本只在
    /// `is_error && !error_text.is_empty()` 时保留，并按 UTF-16 码元截断。
    /// TS: nested-tool-calls.ts#finish
    pub fn finish(&mut self, index: Option<usize>, is_error: bool, error_text: &str) {
        let Some(index) = index else {
            return;
        };
        let Some(record) = self.calls.get_mut(index) else {
            return;
        };
        record.status = if is_error {
            NestedCallStatus::Error
        } else {
            NestedCallStatus::Ok
        };
        let started = self
            .started_at
            .get(index)
            .copied()
            .flatten()
            .map(|started| started.elapsed().as_secs_f64() * 1000.0)
            .unwrap_or(0.0);
        record.duration_ms = Some(started.round());
        if let Some(slot) = self.started_at.get_mut(index) {
            *slot = None;
        }
        if is_error && !error_text.is_empty() {
            record.error = Some(truncate_utf16_units(
                error_text,
                NESTED_CALL_LIMITS.max_error_chars,
            ));
        }
    }

    /// 累加嵌套结果的用量。
    ///
    /// 第一次直接保存（不做任何归一），之后用 `combine_usage` 合并；因此可选字段
    /// （`cacheWrite1h` / `reasoning`）的“任一侧有值才保留”规则由 `combine_usage` 决定。
    /// TS: nested-tool-calls.ts#addUsage
    pub fn add_usage(&mut self, usage: Usage) {
        self.usage = match self.usage.take() {
            Some(existing) => Some(combine_usage(&existing, &usage)),
            None => Some(usage),
        };
    }

    /// 累计用量。
    ///
    /// 返回副本（TS 里是 getter）；没有任何嵌套结果上报过用量时为 `None`。
    /// TS: nested-tool-calls.ts#totalUsage
    pub fn total_usage(&self) -> Option<Usage> {
        self.usage.clone()
    }

    /// 记录快照；没有嵌套调用时返回 `None`。
    ///
    /// `complete` 需要两个条件同时成立：记录器自身未被限流（`complete` 标志），且所有记录
    /// 都已 `finish`（状态不是 `Unfinished`）。返回的 `calls` 由 [`NestedToolCallRecord::to_json`]
    /// 复制构造，之后继续记录不会改变这份快照。
    /// TS: nested-tool-calls.ts#snapshot
    pub fn snapshot(&self) -> Option<NestedToolCalls> {
        if self.calls.is_empty() && self.complete {
            return None;
        }
        let calls: Vec<JsonValue> = self
            .calls
            .iter()
            .map(NestedToolCallRecord::to_json)
            .collect();
        let complete = self.complete
            && self
                .calls
                .iter()
                .all(|call| call.status != NestedCallStatus::Unfinished);
        Some(NestedToolCalls { calls, complete })
    }
}

/// 按 UTF-16 码元截断（不拆代理对；TS 的 `slice(0, n)` 可能产生孤立代理项，这里保留整个码点）。
///
/// `max_units` 是 UTF-16 码元数（BMP 字符计 1，emoji 等代理对计 2）。到达边界时宁可少复制
/// 一个码点也不切开代理对，因此结果长度可能比 `max_units` 少 1；`max_units == 0` 返回空串。
/// TS: nested-tool-calls.ts#truncateUtf16Units
pub fn truncate_utf16_units(text: &str, max_units: usize) -> String {
    let mut units = 0usize;
    let mut end = 0usize;
    for (index, character) in text.char_indices() {
        let length = character.len_utf16();
        if units + length > max_units {
            break;
        }
        units += length;
        end = index + character.len_utf8();
    }
    text[..end].to_string()
}

/// 一个模型发起的工具调用下的嵌套调用作用域。
///
/// `recorder` 由父调用与其所有后代共享；`next_id` 只用于给直接子调用编号；`holds_queue`
/// 为真表示该调用（或它的某个祖先）已持有 runner 的队列锁，其后代不再排队（避免自锁）。
#[derive(Debug, Clone)]
struct CallScope {
    recorder: Arc<Mutex<NestedCallRecorder>>,
    next_id: u64,
    /// 该调用持有独占队列（其自身的嵌套调用不再等待）。
    holds_queue: bool,
}

/// 执行嵌套调用并收集记录。
///
/// 一个会话通常只创建一个 runner：`kiana-session` 在首次 `ctx.executeTool()` 时惰性创建并
/// 缓存到会话状态，所有父 tool call 的作用域都放在同一张表里；agent 结束时调用
/// [`NestedToolCallRunner::clear`] 统一丢弃。
///
/// 两个锁的对象与持锁范围见模块文档的“并发模型与锁”。
///
/// TS: `nested-tool-calls.ts#NestedToolCallRunner`
pub struct NestedToolCallRunner<H: NestedToolCallHost> {
    /// 工具执行与事件投递的实际实现（由会话提供）。
    host: H,
    /// 以发起调用的工具调用 id 为键。
    ///
    /// 父调用的条目保留到 `take_record`；执行中的调用会临时以 `<caller>/<n>` 注册一个条目，
    /// 让它的子调用共享 recorder，执行结束即删除。
    scopes: Mutex<HashMap<String, CallScope>>,
    /// 串行化必须独占执行的嵌套调用。
    queue: tokio::sync::Mutex<()>,
}

impl<H: NestedToolCallHost> NestedToolCallRunner<H> {
    /// 创建 runner。
    /// TS: nested-tool-calls.ts#NestedToolCallRunner.new
    pub fn new(host: H) -> Self {
        NestedToolCallRunner {
            host,
            scopes: Mutex::new(HashMap::new()),
            queue: tokio::sync::Mutex::new(()),
        }
    }

    /// 以 `caller_id` 的名义执行 `name`。嵌套调用 id 为 `<callerId>/<n>`。
    ///
    /// 工具失败不会以 `Err` 返回，而是 `is_error: true` 的结果。
    ///
    /// # 参数
    ///
    /// - `args`：任意 JSON 值。`null` 视为 `{}`；非对象值不会进入工具管线（`AgentToolCall`
    ///   的 `arguments` 是 `JsonObject`，此时传空对象），但事件与记录保留原始值。
    /// - `options`：`signal` 原样转交 `run_tool_call`（不影响排队）；`on_update` 收到的每个
    ///   partial 会先回调调用方，再转成 `tool_execution_update` 事件。
    ///
    /// # 顺序与副作用
    ///
    /// 记录（未超限时）→ `tool_execution_start` → 排队（独占时）→ 注册子作用域 → 执行工具 →
    /// 移除子作用域/释放队列 → `finish` + 累加 usage → `tool_execution_end`。
    ///
    /// # 并发
    ///
    /// 可以多个任务并发调用：作用域表用短锁保护，独占调用在队列锁上排队；子调用与父调用
    /// 共享 recorder，所以深层调用的记录也归入最外层调用。
    pub async fn execute(
        &self,
        caller_id: &str,
        name: &str,
        args: JsonValue,
        options: NestedToolCallOptions,
    ) -> AgentToolCallOutcome {
        // 作用域表只在这个同步块里短暂加锁；后续的 await 都不持锁。
        let (recorder, tool_call_id, holds_queue) = {
            let mut scopes = self.scopes.lock().unwrap_or_else(|e| e.into_inner());
            let scope = scopes
                .entry(caller_id.to_string())
                .or_insert_with(|| CallScope {
                    recorder: Arc::new(Mutex::new(NestedCallRecorder::new())),
                    next_id: 1,
                    holds_queue: false,
                });
            // id 在记录之前分配：即使记录随后被限流丢弃，编号也照常消耗（与 TS `nextId++` 一致）。
            let tool_call_id = format!("{caller_id}/{}", scope.next_id);
            scope.next_id += 1;
            (Arc::clone(&scope.recorder), tool_call_id, scope.holds_queue)
        };

        // TS: `arguments: (args ?? {}) as AgentToolCall["arguments"]`。运行期 `args` 可以是
        // 任意 JSON 值；`AgentToolCall.arguments` 在 Rust 是 `JsonObject`（冻结契约），
        // 因此工具管线拿到对象视图，而事件与记录保留原始值。
        let raw_arguments = match args {
            JsonValue::Null => JsonValue::Object(serde_json::Map::new()),
            other => other,
        };
        let arguments = match &raw_arguments {
            JsonValue::Object(_) => {
                crate::session_manager::json_object_from_value(raw_arguments.clone())
                    .unwrap_or_default()
            }
            _ => JsonObject::new(),
        };
        let tool_call = AgentToolCall {
            kind_tool_call: kiana_ai::types::ToolCallKind::ToolCall,
            id: tool_call_id.clone(),
            name: name.to_string(),
            arguments: arguments.clone(),
            thought_signature: None,
            namespace: None,
            arguments_error: None,
        };
        let record = recorder
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .start_with_arguments(&tool_call, &raw_arguments);
        // 记录可能被限流丢弃（返回 None）；调用与事件仍然继续，只是不产生记录条目。
        self.host
            .emit(NestedToolExecutionEvent::Start {
                tool_call_id: tool_call_id.clone(),
                tool_name: name.to_string(),
                args: raw_arguments.clone(),
                parent_tool_call_id: caller_id.to_string(),
            })
            .await;

        // 独占判定在 Start 事件之后做：等待队列锁期间 UI 已经能看到这条子调用。
        // `holds_queue` 为真时不再排队，否则独占调用的子调用会与它抢同一把锁而死锁。
        let exclusive = !holds_queue
            && (self.host.is_sequential()
                || self
                    .host
                    .get_tools()
                    .iter()
                    .find(|tool| tool.name == name)
                    .and_then(|tool| tool.execution_mode)
                    == Some(kiana_agent_core::ToolExecutionMode::Sequential));
        // 只在独占调用上取队列锁；guard 一直持有到工具执行结束（非独占调用直接并发执行）。
        let queue_guard = if exclusive {
            Some(self.queue.lock().await)
        } else {
            None
        };

        {
            // 临时注册本调用自己的作用域：其内部再发起的嵌套调用会共享 recorder，并继承
            // “已持有队列”的状态。
            let mut scopes = self.scopes.lock().unwrap_or_else(|e| e.into_inner());
            scopes.insert(
                tool_call_id.clone(),
                CallScope {
                    recorder: Arc::clone(&recorder),
                    next_id: 1,
                    holds_queue: holds_queue || exclusive,
                },
            );
        }

        let user_callback = options.on_update.clone();
        let signal = options.signal.clone();
        let on_update = make_update_emitter(
            &self.host,
            user_callback,
            tool_call_id.clone(),
            name.to_string(),
            raw_arguments.clone(),
            caller_id.to_string(),
        );

        let outcome = self
            .host
            .run_tool_call(tool_call.clone(), caller_id, signal.as_ref(), on_update)
            .await;

        // 先移除临时作用域再收尾：此后若还有以该 id 发起的调用，会新建独立 recorder，
        // 记录不再归属父调用。
        {
            let mut scopes = self.scopes.lock().unwrap_or_else(|e| e.into_inner());
            scopes.remove(&tool_call_id);
        }
        drop(queue_guard);

        let error_text = text_of(&outcome.result);
        {
            let mut recorder = recorder.lock().unwrap_or_else(|e| e.into_inner());
            recorder.finish(record, outcome.is_error, &error_text);
            // 嵌套结果不落盘，其用量只通过记录器累计。
            if let Some(usage) = &outcome.result.usage {
                recorder.add_usage(usage.clone());
            }
        }
        // End 的 `durationMs` 用 `outcome.duration_ms`（工具未真正执行时为 None），与记录里
        // `finish` 自己算出的 `durationMs` 是两个独立的值。
        self.host
            .emit(NestedToolExecutionEvent::End {
                tool_call_id,
                tool_name: name.to_string(),
                result: outcome.result.clone(),
                is_error: outcome.is_error,
                duration_ms: outcome.duration_ms,
                parent_tool_call_id: caller_id.to_string(),
            })
            .await;
        outcome
    }

    /// 取出并返回该模型工具调用产生的嵌套调用记录。
    ///
    /// 取走即删除：在再次以同一 id 发起嵌套调用之前，第二次取同一 id 会返回 `None`。
    /// 没有作用域（从未发起嵌套调用，或已经被取走）时返回 `None`。返回的是快照副本，
    /// 之后记录器再变化不会影响它。
    ///
    /// TS: nested-tool-calls.ts#NestedToolCallRunner.takeRecord
    pub fn take_record(&self, tool_call_id: &str) -> Option<NestedCallSummary> {
        let scope = {
            let mut scopes = self.scopes.lock().unwrap_or_else(|e| e.into_inner());
            scopes.remove(tool_call_id)
        };
        let scope = scope?;
        let recorder = scope.recorder.lock().unwrap_or_else(|e| e.into_inner());
        Some(NestedCallSummary {
            calls: recorder.snapshot(),
            usage: recorder.total_usage(),
        })
    }

    /// 清空所有作用域。
    ///
    /// 正在执行的调用持有自己的 recorder `Arc`，仍会跑完并发出 `End` 事件；但它们的记录
    /// 已经无法再通过 [`NestedToolCallRunner::take_record`] 取到。`kiana-session` 在
    /// `agent_end` 时调用它，丢弃本 run 剩余的所有作用域。
    ///
    /// TS: nested-tool-calls.ts#NestedToolCallRunner.clear
    pub fn clear(&self) {
        self.scopes
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
    }
}

/// `textOf(result)`：所有 text 块以 `"\n"` 连接。
///
/// 图片块被忽略；没有 text 块时得到空串（`finish` 只在 `is_error` 且非空时把它写进记录）。
fn text_of(result: &AgentToolResult<JsonValue>) -> String {
    result
        .content
        .iter()
        .filter_map(|block| match block {
            UserContentBlock::Text(text) => Some(text.text.as_str()),
            UserContentBlock::Image(_) => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// 构造工具的 `onUpdate` 钩子：先调用调用方的回调，再发出 `tool_execution_update` 事件。
///
/// 每个 partial 触发一次；用户回调是同步的，事件投递则 `await`。
fn make_update_emitter<'a, H: NestedToolCallHost + 'a>(
    host: &'a H,
    user_callback: Option<AgentToolUpdateCallback>,
    tool_call_id: String,
    tool_name: String,
    args: JsonValue,
    parent_tool_call_id: String,
) -> impl Fn(AgentToolResult<JsonValue>) -> LocalFuture<'a, ()> + Send + 'a {
    move |partial: AgentToolResult<JsonValue>| {
        let user_callback = user_callback.clone();
        let tool_call_id = tool_call_id.clone();
        let tool_name = tool_name.clone();
        let args = args.clone();
        let parent_tool_call_id = parent_tool_call_id.clone();
        Box::pin(async move {
            if let Some(callback) = &user_callback {
                callback(partial.clone());
            }
            host.emit(NestedToolExecutionEvent::Update {
                tool_call_id,
                tool_name,
                args,
                partial_result: partial,
                parent_tool_call_id,
            })
            .await;
        }) as LocalFuture<'a, ()>
    }
}
