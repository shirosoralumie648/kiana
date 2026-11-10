//! index.ts：遥测契约（context / span / store / propagation / schema）。
//!
//! TS 的 `TelemetryContext.startSpan(options, callback)` 是泛型回调；Rust 侧用
//! “对象安全的擦除形式 + 泛型辅助函数” 复刻（见 `start_span` 与 [`TelemetryContextExt`]）。
//!
//! # 这个文件解决什么问题
//!
//! 本 crate 的 context / span / store / propagation 契约集中在这里：遥测适配器（telemetry
//! adapter，把 span / 指标写到某个后端的实现）主要实现本文件的 trait；指标契约
//! （`TelemetryMetrics`、`TypedMetrics`、`TelemetryMetricDefinition` 等）定义在 `metrics.rs`，
//! 由 `lib.rs` 根再导出。上层（`kiana-chord`、`kiana-durable` 等）通过这些 trait 与数据
//! 类型使用遥测，不依赖具体后端。这里的 span 是“一次操作的记录单元”（名称、起止时间、
//! 属性、事件），有父子关系的 span 组成一条 trace。
//!
//! 因此 `index.rs` 的公开签名属于冻结契约（见仓库根目录 `ARCHITECTURE.md` §4.9），改动前
//! 必须确认下游编译与行为；本文件只描述数据形状与行为约定，**不含任何后端实现**（内存实现
//! 见 `memory.rs`，no-op 实现见 `noop.rs`）。
//!
//! # 与 TS 的主要形状差异
//!
//! 最影响使用方式的两处是：
//!
//! 1. **泛型回调的擦除**：TS 的 `startSpan<T>` 是泛型方法，可以直接返回 `Promise<T>`；
//!    Rust 的 trait 方法若要对象安全（能作为 `dyn TelemetryContext` 使用）就不能再带
//!    泛型，因此 trait 上保存擦除版本（[`SpanCallback`] → [`ErasedValue`]），由
//!    [`TelemetryContextExt::start_span_typed`] / [`TelemetryContextExt::start_span_try`]
//!    负责装箱与 `downcast`。调用方通常不应直接构造或拆解 `ErasedValue`。
//! 2. **`SpanLink.context` 的形状**：TS 里链接直接持有活的 `TelemetryContext`；Rust 的
//!    [`SpanLink`] 需要可序列化，而 trait object 不能放进 JSON，所以改存标识视图
//!    [`SpanLinkContext`]（trace/span/parent 三个 id）。内存适配器再用进程级注册表把标识
//!    解析回 carrier，解析不到时 link 被静默丢弃。
//!
//! # 序列化约定
//!
//! `Stored*`、`TraceQuery`、`MetricQuery` 等派生 `Serialize`/`Deserialize`，字段名用
//! camelCase，与 TS 的 JSON 形状一致；可选字段用 `Option` + `skip_serializing_if` 表示
//! “键不存在”（TS 的 `undefined` 在 `JSON.stringify` 中同样消失），而不是 `null`。
//! 例外包括 [`TelemetrySpanDefinition::parents`] 与 [`StoredMetric::kind`]：二者都是
//! `Option` 但没有 `skip`，显式 `None` 会被派生 `Serialize` 写成 `null`（`kind` 在 TS
//! 必填；`json.rs` 的快照投影另有把 `None` 写成空字符串的规则）。
//! 注意 serde 只描述形状：需要与 JS **逐字节**一致的数字文本必须走 `kiana_json`
//! （serde_json 的指数写法与 `-0` 的文本都与 `JSON.stringify` 不同，非有限值两者都写
//! `null`；详见 `json.rs`）。
//!
//! # 阅读顺序
//!
//! 属性与错误（[`AttributeValue`] / [`TelemetryError`]）→ span 与查询的数据类型
//! （[`StoredSpan`] / [`TraceQuery`] / [`StoredMetric`]）→ store 与 context 契约
//! （[`TelemetryStore`] / [`TelemetryContext`] / [`TelemetrySpan`]）→ schema 类型 →
//! [`TypedSpanStarter`] → 末尾的“仅类型层面的名字”。

use std::any::Any;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use indexmap::IndexMap;
use serde::{Deserialize, Serialize};

use crate::metrics::TelemetryMetrics;

/// 任意异步返回值的装箱形式。
///
/// 本 crate 的 trait 方法用它代替 `async fn`：trait 需要作为 `dyn Trait` 使用，而 trait 中的
/// 原生 `async fn` 不满足 dyn 兼容（对象安全），显式返回 `Pin<Box<dyn Future>>` 则可以。
///
/// `dyn Future` 必须 `Pin` 才能轮询；`Send` 约束来自 `TelemetryContext: Send + Sync` ——
/// 返回的 future 可能被移到其他线程执行。`'a` 通常绑定 `&self` 的借用，表示 future 不能比
/// 它依赖的上下文活得更久。
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;
/// 擦除后的 span 回调返回值（调用方 downcast）。
///
/// 具体类型 `T` 由 [`TelemetryContextExt::start_span_typed`] /
/// [`TelemetryContextExt::start_span_try`] 在调用点确定；trait 的擦除通道只认识
/// `Any + Send`。`downcast` 失败只可能发生在手写 [`SpanCallback`] 且装箱类型与期望不一致
/// 时，两个泛型辅助函数会把它转成 [`TelemetryError`]。
pub type ErasedValue = Box<dyn Any + Send>;
/// 擦除后的 span 回调：接收共享的 span 句柄，返回装箱 future。
///
/// - `FnOnce`：每个 span 只调用一次回调（TS 契约：`startSpan` 恰好调用回调一次）；
/// - `Send`：回调可能随 future 跨线程执行；
/// - 生命周期 `'a`：回调 future 可以借用上下文；但返回值 [`ErasedValue`] 隐含 `'static`，
///   所以回调结果不能借用外部数据（需要时请先克隆或移动到 `async move` 块里）。
///
/// 回调返回 `Err` 表示“回调抛出了异常”：适配器会把它作为 span 的拒绝值向上传播，并在
/// 调用方没有显式设置 status 时把 span 标记为 error。
pub type SpanCallback<'a> = Box<
    dyn FnOnce(Arc<dyn TelemetrySpan>) -> BoxFuture<'a, Result<ErasedValue, TelemetryError>>
        + Send
        + 'a,
>;

/// `AttributeValue`：`string | number | boolean | readonly string[] | readonly number[] | readonly boolean[]`。
///
/// 对应 TS 的 `AttributeValue` 联合类型。`#[serde(untagged)]` 表示 JSON 里没有类型标签：
/// 序列化时直接写成标量或数组；反序列化时按变体声明顺序依次尝试（标量在前、数组在后），
/// 普通 JSON 值不会被误判为数组。
///
/// # 注意事项
///
/// - `Number` 用 `f64`，因此可以承载 JS 的 `NaN`/`±Infinity`。JSON 无法直接表示非有限值，
///   渲染路径按 `JSON.stringify` 语义把它们写成 `null`（`kiana_json` 的行为）。
/// - 派生 `PartialEq` 逐字段比较；由于 `NaN != NaN`，不能假定两次计算出的 NaN 属性相等。
/// - TS 的索引签名还允许 `undefined`（表示“未设置”）；Rust 没有该值，未设置的属性直接
///   不放进 map（见 [`SpanAttributes`]）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum AttributeValue {
    /// JS 字符串。
    String(String),
    /// JS number；写出时必须走 `kiana_json` 的数字格式化。
    Number(f64),
    /// JS boolean。
    Boolean(bool),
    /// `readonly string[]`。
    StringArray(Vec<String>),
    /// `readonly number[]`；元素同样可能是非有限值。
    NumberArray(Vec<f64>),
    /// `readonly boolean[]`。
    BooleanArray(Vec<bool>),
}

impl AttributeValue {
    /// 类型名，与 TS 的 `TelemetryAttributeType` 字面量一一对应。
    ///
    /// 返回 `"string"`、`"number"`、`"boolean"`、`"string[]"`、`"number[]"`、
    /// `"boolean[]"` 之一；该文本与 [`TelemetryAttributeType`] 的 serde 表示相同，可直接
    /// 用于诊断输出或 schema 比对。返回 `&'static str`，不会分配。
    pub fn type_name(&self) -> &'static str {
        match self {
            AttributeValue::String(_) => "string",
            AttributeValue::Number(_) => "number",
            AttributeValue::Boolean(_) => "boolean",
            AttributeValue::StringArray(_) => "string[]",
            AttributeValue::NumberArray(_) => "number[]",
            AttributeValue::BooleanArray(_) => "boolean[]",
        }
    }
}

/// `SpanAttributes`：插入序（`IndexMap`）与 TS 对象字面量顺序一致。
///
/// 用 `IndexMap` 而不是 `HashMap`，是为了保留插入顺序：TS 对象枚举/序列化时按属性创建
/// 顺序输出（整数样键会前置，这一步由 `jsval::js_entries` 处理），而 `HashMap` 的迭代
/// 顺序不稳定。值类型是 [`AttributeValue`]；TS 中值为 `undefined` 的键不会出现在 map 里。
pub type SpanAttributes = IndexMap<String, AttributeValue>;

/// `TelemetryCarrier`：不透明的、可 JSON 序列化的传播数据。
///
/// 键值都是字符串（适配器私有的格式），因此可以直接写进 JSON 并跨传输传递：
/// [`TelemetryPropagation::inject`] 从上下文生成它，[`TelemetryPropagation::extract`]
/// 从它恢复上下文。它是可选的关联性诊断数据，不是应用状态，也不应被持久化为业务数据
/// （见 crate 文档的“安全与可移植性”一节）。
pub type TelemetryCarrier = IndexMap<String, String>;

/// `SpanLink`。
///
/// 表示 span 之间的显式关联（不同于父子因果链）。TS 的 `context` 是活的
/// `TelemetryContext`；Rust 冻结为可序列化的 [`SpanLinkContext`] 标识视图（见模块文档）。
/// 内存适配器只在标识能于进程级注册表中解析时保留 link，未知标识被静默丢弃
/// （`memory.rs` 的 `copy_link`）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SpanLink {
    /// 被链接 span 的标识（不是属性，也不是活对象）。
    pub context: SpanContextId,
    /// 链接附带的属性；`None` 对应 TS 的 `undefined`，JSON 中省略该键。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attributes: Option<SpanAttributes>,
}

/// `SpanOptions`。
///
/// 启动一个 span 所需的输入。TS 的 `attributes`/`links` 都是可选字段；Rust 用 `Option`
/// 区分“未提供”与“提供了空集合”，`None` 序列化时省略键。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SpanOptions {
    /// span 名称；store 的 `name` 过滤按它精确比较。
    pub name: String,
    /// 初始属性；span 创建时写入，之后仍可用 `TelemetrySpan::set_attributes` 合并/覆盖。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attributes: Option<SpanAttributes>,
    /// 初始链接；与 TS 一样只能在启动时提供。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub links: Option<Vec<SpanLink>>,
}

/// `SpanStatus`。
///
/// 复刻 TS 的可判别联合，`#[serde(tag = "status")]` 产生的 JSON 形状为：
///
/// ```text
/// { "status": "ok" }
/// { "status": "error" }                                  // 没有 error 细节
/// { "status": "error", "error": { "name": "...", "message": "..." } }
/// ```
///
/// # 状态转换
///
/// - 初始状态由实现决定（内存实现为 `Ok`）；
/// - 正常完成 ⇒ 保持当前状态（初始为 `Ok`；回调中显式 `set_status` 后不会被自动改回
///   `Ok`）；回调以错误结束 ⇒ `Error`，但调用方在回调中显式 `set_status` 后不再被自动
///   覆盖（`explicit_status` 标记）；
/// - 重复 `set_status` 后写覆盖；已结算 span 上的 `set_status` 被内存实现忽略。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "lowercase")]
pub enum SpanStatus {
    /// 正常完成（默认）。
    #[default]
    Ok,
    /// 失败；自动结算时，`error` 只在错误是 JS `Error` 实例（或带 `name` 的等价物）时有值；
    /// 显式调用 `set_status` 的调用方可以自行提供任意 `StatusError`。
    Error {
        /// `{name, message}`；`None` 对应非 `Error` 的 rejection 值。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<StatusError>,
    },
}

/// `{ name: string; message: string }`。
///
/// 一段可序列化的错误摘要：`name` 来自 JS `error.name`（如 `"TypeError"`、
/// `"AbortError"`），`message` 是错误消息。只保留文本，不保留堆栈、`cause` 或原始错误对象。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct StatusError {
    /// JS `error.name`。
    pub name: String,
    /// 错误消息。
    pub message: String,
}

/// 遥测错误。TS 侧的错误语义由实现决定；Rust 只保证 `Display` 稳定。
///
/// # `name`：JS `Error` 的 `name` 属性
///
/// TS 的适配器回调可以 throw 任意值；`memory.ts` 只对 `error instanceof Error` 的值记录
/// `{ name: error.name, message: error.message }`，其余 rejection 值记录裸 `{ status: "error" }`。
/// Rust 的 rejection 通道统一是 `TelemetryError`，因此这里显式建模同一区分：
///
/// - [`TelemetryError::new`]（以及 [`TelemetryError::with_name`]）表示一个 `Error` 实例，
///   `name` 为 `Some("Error")`（或给定名字）；
/// - [`TelemetryError::rejection`] 表示「不是 `Error` 的 rejection 值」，`name` 为 `None`，
///   结算时只写 `{ status: "error" }`（不带 `error` 详情）。
///
/// `Display` 始终只是 `message`。
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
#[error("{message}")]
pub struct TelemetryError {
    /// 错误消息；`Display` 只输出它。
    pub message: String,
    /// JS `error.name`；`None` 表示非 `Error` 的 rejection 值（TS 记 `{ status: "error" }`）。
    pub name: Option<String>,
}

impl TelemetryError {
    /// `new Error(message)`：`name` 为 `"Error"`。
    pub fn new(message: impl Into<String>) -> Self {
        TelemetryError {
            message: message.into(),
            name: Some("Error".to_string()),
        }
    }

    /// 带自定义 `name` 的 `Error`（如 `"TypeError"`、`"AbortError"`）。
    ///
    /// TS: `error.name` 被 `memory.ts` 原样复制进 `SpanStatus.error.name`。
    pub fn with_name(name: impl Into<String>, message: impl Into<String>) -> Self {
        TelemetryError {
            message: message.into(),
            name: Some(name.into()),
        }
    }

    /// 非 `Error` 的 rejection 值（普通对象/字符串/数字/`undefined` 的等价物）。
    ///
    /// TS: `error instanceof Error` 为假时只记 `{ status: "error" }`，不复制 name/message。
    pub fn rejection(message: impl Into<String>) -> Self {
        TelemetryError {
            message: message.into(),
            name: None,
        }
    }

    /// JS `error.name`（`None` = 非 `Error` 的 rejection 值）。
    pub fn name(&self) -> Option<&str> {
        self.name.as_deref()
    }
}

/// 已结束 span 的快照；事件时间是相对 span 起点的毫秒数。
///
/// 事件按入表顺序保存在 [`StoredSpan::events`]（单线程下等于 `add_event` 的调用顺序；
/// 并发调用时入表顺序由锁竞争决定，见 `memory.rs` 的 `add_event`）；属性是记录时的深拷贝。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StoredSpanEvent {
    /// 事件名。
    pub name: String,
    /// 相对 span 起点的毫秒数（`performance.now()` 差值，可为小数）。
    pub time_ms: f64,
    /// 事件属性快照（JS 键序）。
    pub attributes: SpanAttributes,
}

/// `StoredSpan.links` 中的一项。
///
/// 与 [`SpanLink`] 的区别：源上下文已经被解析成 `traceId`/`spanId` 两个字符串，不再持有
/// 可解析的标识。内存适配器在记录 link 时就把未知目标丢掉，因此这里出现的都是当时已知的
/// 上下文。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StoredSpanLink {
    /// 被链接 span 所属的 trace id。
    pub trace_id: String,
    /// 被链接 span 的 id。
    pub span_id: String,
    /// 链接属性；内存适配器总是写入 Some(属性 map)（无属性时为空 map；copyLink 总会设置
    /// attributes），因此内存实现的 JSON 中总是存在该键。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attributes: Option<SpanAttributes>,
}

/// `StoredSpan`。
///
/// 一个已结算 span 的、与后端无关的快照；只由 store 返回已结算的 span。字段是记录时值的
/// 深拷贝，调用方修改它不会影响 store 内部状态。
///
/// 时间语义：`started_ms`/`ended_ms` 是纪元毫秒（墙钟）；内存实现只对起点取墙钟，终点用
/// `ended_ms = started_ms + 单调时钟流逝` 计算。事件的 `time_ms` 则相对 `started_ms`。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StoredSpan {
    /// 适配器内唯一的 span id（内存实现是 `"<scope>:<本地序号>"`）。
    pub id: String,
    /// span 所属的 trace id。
    pub trace_id: String,
    /// 父 span id；根 span 时缺省（JSON 中无该键）；父级属于另一个 recorder 时仍会出现，
    /// 值为父 carrier 的 `<scope>:<spanId>` 文本（见 `memory.rs` 的 `stored_spans_impl`）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_id: Option<String>,
    /// span 名称。
    pub name: String,
    /// 起点：纪元毫秒。
    pub started_ms: f64,
    /// 终点：纪元毫秒。
    pub ended_ms: f64,
    /// 最终属性（深拷贝、JS 键序：整数样键升序在前）。
    pub attributes: SpanAttributes,
    /// 事件，按记录顺序。
    pub events: Vec<StoredSpanEvent>,
    /// 链接，按入表顺序（单线程下等于添加顺序；并发调用时由锁竞争决定，见 `memory.rs`
    /// 的 `add_link`）；未知目标的 link 在记录时已被丢弃。
    pub links: Vec<StoredSpanLink>,
    /// 最终状态。
    pub status: SpanStatus,
}

/// `StoredTrace`。
///
/// 一条 trace 的快照：trace id 相同的一组 span。`get_trace` 在没有任何匹配的已结算 span
/// 时返回 `None`（TS 的 `undefined`）。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StoredTrace {
    /// trace 标识。
    pub trace_id: String,
    /// 该 trace 的 span；内存实现按 `started_ms` 升序、并列按 id 字符串升序返回
    /// （`storedSpans()` 的排序）。
    pub spans: Vec<StoredSpan>,
}

/// `TraceQuery`。
///
/// `find_traces` 的过滤与分页条件；`None` 字段表示该维度不筛选。过滤语义按 TS：
/// `name`/`start_min`/`start_max` 只看 trace 的**最早存储 span**（“first span”，即按
/// `started_ms` 升序、并列按 id 排序后的第一个）；`attributes`/`errors` 则只要 trace 中
/// **任意**一个 span 满足即可，且 `attributes` 的所有条目必须命中**同一个** span。
///
/// 分页时 `limit` 默认 20；`cursor` 是上一页返回的不透明字符串，续页**必须**带上与上一页
/// 相同的过滤条件，否则结果没有意义（TS 契约）。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TraceQuery {
    /// Name of the earliest stored span, with span ID breaking timestamp ties.
    ///
    /// 最早存储 span 的名称；时间戳并列时由 span id 决定先后。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// All equalities must match one span; that span may be anywhere in the trace.
    ///
    /// 属性等值过滤：每个条目都必须命中同一个 span。按 JS `===` 比较：数字 `NaN` 不等于
    /// 自身，数组属性永远不会匹配（哪怕内容相同，`===` 按引用比较）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attributes: Option<IndexMap<String, AttributeValue>>,
    /// `Some(true)` 只保留任一 span 状态为 error 的 trace；`Some(false)`/`None` 不过滤。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub errors: Option<bool>,
    /// Inclusive bounds on the earliest stored span's start time.
    ///
    /// 最早存储 span 起点的闭区间下界。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start_min: Option<f64>,
    /// 最早存储 span 起点的闭区间上界。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start_max: Option<f64>,
    /// Positive page size; defaults to 20.
    ///
    /// 页大小；`None` 默认 20。类型是 `u32`，因此 TS 里的负数/小数/`NaN` 不可达；`0`
    /// 可达——内存实现在存在匹配时返回与 TS 相同的 `TypeError`（见 `memory.rs`）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
    /// 上一页返回的续页标记（不透明字符串）。
    ///
    /// 内存实现的编码是 `JSON.stringify([startedMs, traceId])`；无法解析时 store 返回
    /// `SyntaxError`（内存实现）。调用方只应原样回传。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
}

/// `MetricQuery`。
///
/// `find_metrics` 的过滤条件：`name` 必填且精确匹配，时间与属性是可选过滤。内存实现按
/// 记录顺序返回、不排序也不分页。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MetricQuery {
    /// 指标名（精确匹配）。
    pub name: String,
    /// 区间起点的闭区间下界（纪元毫秒）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start_min: Option<f64>,
    /// 区间起点的闭区间上界（纪元毫秒）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start_max: Option<f64>,
    /// 属性等值过滤；每个条目都必须命中同一条测量（JS `===`）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attributes: Option<IndexMap<String, AttributeValue>>,
}

/// `StoredMetric.kind`。
///
/// 与 TS 的字面量一一对应；`serde(rename_all = "camelCase")` 产生线上 JSON 的
/// `"counter"`、`"upDownCounter"`、`"histogram"`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum MetricKind {
    /// 计数器（只做累加语义的仪表）。
    Counter,
    /// 可增可减的计数器（up-down counter）。
    UpDownCounter,
    /// 直方图：按桶记录数值分布。
    Histogram,
}

/// `StoredMetric`。
///
/// store 返回的一行指标数据：某个时间区间内、按属性分组的聚合值。
/// `interval_ms` 是区间起点（内存实现按 10_000 ms 向下取整分桶）；`count`/`sum`/`min`/
/// `max` 是聚合统计。注意内存适配器当前**每个 measurement 生成一行**（`count = 1`、
/// `sum = min = max = value`），不做跨记录聚合（见 `memory.rs` 的 `find_metrics_impl`）。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StoredMetric {
    /// 区间起点：纪元毫秒（内存实现按 10 秒对齐）。
    pub interval_ms: f64,
    /// 指标名。
    pub name: String,
    /// 指标种类。TS 的 `kind` 是必填；Rust 形状允许 `None` 表示“未标注”，内存实现总是
    /// `Some`。`json::stored_metrics_to_value` 把 `None` 渲染成空字符串。
    pub kind: Option<MetricKind>,
    /// 单位；缺省表示没有单位。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unit: Option<String>,
    /// 该行对应的属性集合（深拷贝、JS 键序）。
    pub attributes: SpanAttributes,
    /// 区间内记录次数。
    pub count: f64,
    /// 区间内数值之和。
    pub sum: f64,
    /// 区间内最小值。
    pub min: f64,
    /// 区间内最大值。
    pub max: f64,
}

/// `findTraces` 的返回。
///
/// `traces` 的顺序由 store 决定；内存实现按 trace 的最早 span 起点**倒序**（最新在前），
/// 起点并列时 trace id 升序（UTF-16 字符串比较）。`cursor` 只在还有更多匹配时出现：
/// 续页时把它原样传回 [`TraceQuery::cursor`]，并保持其余过滤条件不变。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TracePage {
    /// 本页 trace。
    pub traces: Vec<StoredTrace>,
    /// 续页标记；`None` 表示没有更多页。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
}

/// `TelemetryStore`。
///
/// 只读的已结算遥测数据查询接口，不依赖具体存储后端。三个方法都返回 [`BoxFuture`]：
/// store 通常以 `Arc<dyn TelemetryStore>` 的形式被共享，trait 必须对象安全，而 trait 中的
/// 原生 `async fn` 不满足 dyn 兼容，所以显式装箱。
///
/// # 错误
///
/// 所有失败都通过 [`TelemetryError`] 返回，而不是 panic。错误是否可重试由实现决定：
/// 内存实现没有瞬时错误，但 `find_traces` 会为非法 cursor 或 `limit: 0` 返回由调用方
/// 输入导致的错误，原样重试不会成功。
pub trait TelemetryStore: Send + Sync {
    /// 查询一页 trace；`query = None` 等价于空过滤条件（默认页大小 20）。
    ///
    /// 过滤语义与分页规则见 [`TraceQuery`] / [`TracePage`]。续页时应把上一页的
    /// [`TracePage::cursor`] 原样放进 [`TraceQuery::cursor`]，并保留相同的过滤条件。
    ///
    /// # 错误
    ///
    /// - cursor 无法解析：内存实现返回 `name = "SyntaxError"` 的 [`TelemetryError`]；
    /// - `limit == Some(0)` 且存在匹配：内存实现复刻 TS 的 `TypeError`（文本见 `memory.rs`）。
    fn find_traces(
        &self,
        query: Option<TraceQuery>,
    ) -> BoxFuture<'_, Result<TracePage, TelemetryError>>;
    /// 按 trace id 取一条完整 trace。
    ///
    /// 没有匹配的**已结算** span 时返回 `Ok(None)`（对应 TS 的 `undefined`）；未结算 span
    /// 不会被返回。是否过滤 trace id、span 如何排序由实现决定（内存实现复用
    /// `storedSpans()` 的排序）。
    fn get_trace(
        &self,
        trace_id: &str,
    ) -> BoxFuture<'_, Result<Option<StoredTrace>, TelemetryError>>;
    /// 按条件查询指标行。
    ///
    /// 没有匹配时返回空 `Vec`（不是错误）。内存实现按记录顺序返回、不聚合、不分页
    /// （见 [`StoredMetric`]）。
    fn find_metrics(
        &self,
        query: MetricQuery,
    ) -> BoxFuture<'_, Result<Vec<StoredMetric>, TelemetryError>>;
}

/// 传播能力。
///
/// 把当前上下文编码成 [`TelemetryCarrier`]，或从 carrier 恢复上下文；实现必须是
/// `Send + Sync`，因为上下文/span 会在任务间共享。carrier 只是可选的关联性诊断数据，
/// 不是应用状态（见 crate 文档）。
pub trait TelemetryPropagation: Send + Sync {
    /// 把上下文编码为 carrier。
    ///
    /// 返回 `None` 表示该上下文没有可传播的身份（内存实现的根上下文、no-op 上下文如此），
    /// 或适配器不支持注入；这不是错误，调用方应跳过传播。实现可能查询上下文的
    /// [`TelemetryContext::span_context_id`] 来决定能否编码。
    fn inject(&self, context: &dyn TelemetryContext) -> Option<TelemetryCarrier>;
    /// 缺失或未知 carrier 使用 root；可识别的标识是不透明字符串。
    ///
    /// 返回的上下文只保证可用于创建子 span 与读取能力，不承诺拥有自己的记录：内存实现
    /// 返回一个“已提取父级”（`memory.rs` 的私有 `ExtractedParent`），它不会在 store 里
    /// 产生 span。
    fn extract(&self, carrier: Option<&TelemetryCarrier>) -> Arc<dyn TelemetryContext>;
}

/// 供 `SpanLink` 与 store 查询使用的上下文标识视图。
///
/// 与 [`SpanLinkContext`] 是同一类型；别名用于强调“这是可用于建立关联的 id 三元组”，
/// 而不是完整上下文。它只含字符串，因此可克隆、可序列化；本 crate 没有公开的“按 id 取回
/// 上下文”的 API，适配器若要解析 link 目标，必须使用自己的身份注册表。
pub type SpanContextId = SpanLinkContext;

/// `SpanLink.context` 的形状：TS 里是 `TelemetryContext`，序列化时只保留标识。
///
/// # 不变量
///
/// - `trace_id` + `span_id` 是身份键；内存适配器用它们查进程级注册表（见 `memory.rs`）。
/// - 根上下文、no-op 上下文与不可记录的惰性 span 返回**两个空字符串**的哨兵；空 id 不会
///   命中任何真实 span，因此以它为目标的 link 会被丢弃。
/// - `parent_id` 只是诊断信息（父级 id），不参与身份解析，可为 `None`。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SpanLinkContext {
    /// trace id。
    pub trace_id: String,
    /// span id（内存实现是 `"<scope>:<span>"` 文本）。
    pub span_id: String,
    /// 父 span id；根 span 或合成父上下文（extract 得到的父级自身）为 `None`；有父级的
    /// span（含外部父级）为父 carrier 的 `<scope>:<spanId>` 文本。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_id: Option<String>,
}

/// `TelemetryContext`。
///
/// 对象安全形式：`start_span` 接收擦除后的回调与返回值。调用方通常用
/// `TelemetryContextExt::start_span_typed` 的泛型版本，避免手动 downcast。
///
/// # 契约（README / 一致性套件）
///
/// - 创建子 span 并**恰好一次**调用回调；返回的 future 结算前 span 保持打开。
/// - 正常完成 ⇒ 保持当前状态（初始为 `Ok`，回调显式 `set_status` 后不被自动覆盖）；
///   回调以错误结束 ⇒ span 标记 error，除非回调已显式 `set_status`。
/// - 能力（`metrics`/`propagation`）在根、子 span、`extract` 得到的父上下文上都可用
///   （前提是适配器提供）。
/// - 记录方法同步且没有返回值；调用方无法从签名得知记录是否被接受。
///
/// # 取消
///
/// 丢弃**已 poll 过**的返回 future 时，内存实现不会结算 span（`settled = false`），因此它
/// 不会出现在 `find_traces`/`get_trace` 里。这对应 TS 中“被 await 的回调 promise 永不完成
/// 时 finally 不运行”的情形；注意 JS 丢弃返回的 promise 本身不会取消 async 函数。若返回的
/// future 从未被 poll，span 根本不会被创建，也就谈不上 `settled = false`。需要保证结算的
/// 调用方必须让 future 跑完。
pub trait TelemetryContext: Send + Sync {
    /// Adapter capabilities remain available on child spans and extracted parents.
    ///
    /// 适配器提供的指标能力；`None` 表示该上下文没有接指标后端。
    fn metrics(&self) -> Option<Arc<dyn TelemetryMetrics>>;
    /// 适配器提供的传播能力；`None` 表示不能 inject/extract。
    fn propagation(&self) -> Option<Arc<dyn TelemetryPropagation>>;

    /// 以当前上下文为父启动一个 span，并在 span 打开期间执行回调。
    ///
    /// `options` 描述名称/初始属性/链接；`callback` 接收共享的 [`TelemetrySpan`] 且只调用
    /// 一次，返回值是回调结果的类型擦除版本（见 [`ErasedValue`]）。失败（适配器拒绝或
    /// 回调返回 `Err`）沿 future 的 `Err` 通道传播。
    ///
    /// 注意：这是返回 future 的普通函数，span 的创建与回调的进入发生在**首次 poll**，
    /// 而不是调用本方法的瞬间（Rust future 惰性）。TS 的“同步接纳”在 Rust 中对应
    /// “首次 poll 时进入回调”。
    fn start_span<'a>(
        &'a self,
        options: SpanOptions,
        callback: SpanCallback<'a>,
    ) -> BoxFuture<'a, Result<ErasedValue, TelemetryError>>;

    /// 当前上下文的标识（trace/span），用于构造 link 与记录。
    fn span_context_id(&self) -> SpanLinkContext;
}

/// 泛型辅助：`ctx.start_span_typed(opts, |span| async move { ... })`。
///
/// 名字与 [`TelemetryContext::start_span`] 区分，避免方法解析歧义。
///
/// # 错误传播（与 TS 的对应关系）
///
/// TS 的 `startSpan(options, callback)` 返回 `Promise<T>`：回调 **throw** 才 reject，回调
/// **返回**的值（哪怕是一个 `Error` 对象、被拒绝的 promise 之外的值）都原样成为结果。
/// `start_span_typed` 的语义是：回调 future 的输出**就是**结果值，即使它是
/// `Result<_, TelemetryError>`：
///
/// ```text
/// // 返回 Ok(Err(boom))：span 正常结算、状态保持 ok（TS 里 `return err` 同样不 reject）。
/// let value: Result<(), TelemetryError> =
///     ctx.start_span_typed(opts, |_| Box::pin(async { Err(boom) })).await??;
/// ```
///
/// 需要「失败即 reject」的语义（TS 里回调 `throw`）时用 [`TelemetryContextExt::start_span_try`]：
/// 它把回调返回的 `Err` 当作 throw 处理，span 状态置为
/// `{ status: "error", error: { name: error.name, message } }`（`error.name` 为 `None` 时记裸
/// `{ status: "error" }`；除非回调已显式设置状态）并把错误返回给调用方。Rust 无法在保持本方法
/// 签名（`T` 无约束，§4.9 冻结）的同时做 Result 展平：那需要特化（unstable），所以展平放在
/// 显式的 `start_span_try` 上。
///
/// # 类型约束
///
/// `T: Send + 'static`：结果会被装箱成 [`ErasedValue`]（`Box<dyn Any + Send>`），必须满足
/// `'static` 才能放进 `Any`，`Send` 才能跨线程。回调返回的 future 也要求 `'static`；
/// 需要借用调用方数据时，请在进入回调前克隆或移动到 `async move` 块里。
///
/// # 何时选哪个
///
/// - 失败也是**普通返回值**（例如返回一个 `Result` 值本身，不想让 span 标红）：
///   用 [`start_span_typed`](TelemetryContextExt::start_span_typed)；
/// - 失败应让 span 标红并把错误交还调用方：用
///   [`start_span_try`](TelemetryContextExt::start_span_try)。
pub trait TelemetryContextExt: TelemetryContext {
    /// 泛型版本的 `start_span`：回调的返回值原样成为结果，不参与 span 状态判定。
    ///
    /// # 参数与返回
    ///
    /// - `options`：子 span 的名称/属性/链接，原样交给适配器；
    /// - `callback`：只调用一次；其 future 的输出被装箱成 `Any`，结算后 downcast 回 `T`；
    /// - 返回 `Ok(T)`；适配器拒绝时返回 `Err(TelemetryError)`。若装箱类型与 `T` 不一致
    ///   （正常路径不会发生；手写擦除回调或适配器违反契约时才会），返回 `Err` 且消息为
    ///   `"span callback returned an unexpected type"`。
    ///
    /// `T` 自身可以是 `Result`：它只会被当作普通值包装，span 仍按正常完成结算。
    fn start_span_typed<T, F>(
        &self,
        options: SpanOptions,
        callback: F,
    ) -> BoxFuture<'_, Result<T, TelemetryError>>
    where
        T: Send + 'static,
        F: FnOnce(Arc<dyn TelemetrySpan>) -> BoxFuture<'static, T> + Send + 'static,
    {
        Box::pin(async move {
            // 把泛型回调包成 trait 认识的擦除形式：回调结果装箱成 `Any`；它永远返回
            // `Ok`，因此适配器不会把回调输出误判为“抛出”。
            let cb: SpanCallback<'_> = Box::new(move |span| {
                Box::pin(async move {
                    let value = callback(span).await;
                    Ok(Box::new(value) as ErasedValue)
                })
            });
            // 适配器结算 span 后交回擦除值；调用点知道真实类型是 T，downcast 恢复。
            let value = self.start_span(options, cb).await?;
            value
                .downcast::<T>()
                .map(|v| *v)
                .map_err(|_| TelemetryError::new("span callback returned an unexpected type"))
        })
    }

    /// 同 [`start_span_typed`](TelemetryContextExt::start_span_typed)，但把回调返回的 `Err`
    /// 作为「回调抛出」处理：错误沿 `start_span` 的拒绝路径向上传播，span 未显式设置状态时
    /// 被标记为 `{ status: "error", error: { name: error.name, message } }`（`error.name` 为
    /// `None` 时只写 `{ status: "error" }`，对应 TS 里 throw 的不是 `Error` 实例），然后照常结算。
    ///
    /// 对应 TS 的 `ctx.startSpan(options, async (span) => { ...; throw error })`：
    /// `memory.ts` 的 `try/catch/finally` 把抛出的 `Error` 写成 status 并原值 rethrow。
    ///
    /// ```text
    /// let value: () = ctx
    ///     .start_span_try(opts, |_| Box::pin(async move { Err::<(), _>(boom) }))
    ///     .await?; // ← Err(boom)，且 span 记为 error
    /// ```
    ///
    /// # 参数与返回
    ///
    /// 参数与 [`start_span_typed`](TelemetryContextExt::start_span_typed) 相同，区别只在回调
    /// 输出类型是 `Result<T, TelemetryError>`：
    /// - `Ok(value)` ⇒ 返回 `Ok(value)`，span 正常结算；
    /// - `Err(error)` ⇒ 返回 `Err(error)`，span 结算为 error（除非回调已显式设置 status）。
    fn start_span_try<T, F>(
        &self,
        options: SpanOptions,
        callback: F,
    ) -> BoxFuture<'_, Result<T, TelemetryError>>
    where
        T: Send + 'static,
        F: FnOnce(Arc<dyn TelemetrySpan>) -> BoxFuture<'static, Result<T, TelemetryError>>
            + Send
            + 'static,
    {
        Box::pin(async move {
            // 关键差异：`?` 让回调的 Err 在装箱前离开包装 future，于是适配器看到的是
            // “回调失败”，会走 rejection 路径（span 标 error）；start_span_typed 则把
            // Err 当普通值装箱，span 保持 ok。
            let cb: SpanCallback<'_> = Box::new(move |span| {
                Box::pin(async move {
                    let value = callback(span).await?;
                    Ok(Box::new(value) as ErasedValue)
                })
            });
            let value = self.start_span(options, cb).await?;
            value
                .downcast::<T>()
                .map(|v| *v)
                .map_err(|_| TelemetryError::new("span callback returned an unexpected type"))
        })
    }
}

impl<T: TelemetryContext + ?Sized> TelemetryContextExt for T {}

/// 自由函数入口，便于以函数形式调用（无需方法语法）。
///
/// 把 `options` 与 `callback` 原样转发给 [`TelemetryContext::start_span`]，不增加任何语义；
/// 返回的 future 与方法调用完全相同。调用方也可以直接用方法语法，这里只是提供函数形式的
/// 入口。
pub fn start_span_dyn<'a>(
    context: &'a dyn TelemetryContext,
    options: SpanOptions,
    callback: SpanCallback<'a>,
) -> BoxFuture<'a, Result<ErasedValue, TelemetryError>> {
    context.start_span(options, callback)
}

/// `TelemetrySpan`。
///
/// 一个打开的 span：除 [`TelemetryContext`] 的能力外，还提供同步的记录方法。这些方法与
/// TS 一样返回 `()`，没有 `Result`：适配器可以忽略调用（no-op 实现全部忽略），调用方无法
/// 从返回值判断记录是否被接受。
///
/// 内存实现的约束（`memory.rs`）：
/// - 已结算 span 上的记录调用无效果；
/// - `add_event` 的时间由实现取当前单调时钟并减去 span 起点，因此事件时间相对起点；
/// - `set_attributes` 是**合并**（后写覆盖同名键），不是整体替换；
/// - `add_link` 的目标必须能在适配器身份注册表中解析，否则 link 被静默丢弃；
/// - `set_status` 会把状态标记为“显式”，之后回调异常结束时不再自动覆盖。
pub trait TelemetrySpan: TelemetryContext {
    /// 添加一条指向其他 span 的链接；无返回值。
    fn add_link(&self, link: SpanLink);
    /// 记录一个带可选属性的事件；无返回值。
    fn add_event(&self, name: &str, attributes: Option<SpanAttributes>);
    /// 合并属性：同名键由新值覆盖，其余键保留。
    fn set_attributes(&self, attributes: SpanAttributes);
    /// 设置最终状态；重复调用时后写覆盖。
    fn set_status(&self, status: SpanStatus);
}

// ---------------------------------------------------------------------------
// Schema types（defineTelemetrySchema 的等价物）
// ---------------------------------------------------------------------------

/// `TelemetryAttributeType`。
///
/// schema 中属性值的类型标签，对应 TS 的字面量联合。这里用显式 `rename` 而不是
/// `rename_all`，因为 `"string[]"` 等值包含符号、不符合任何命名规则；序列化文本与
/// [`AttributeValue::type_name`] 的返回值一致。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TelemetryAttributeType {
    #[serde(rename = "string")]
    String,
    #[serde(rename = "number")]
    Number,
    #[serde(rename = "boolean")]
    Boolean,
    #[serde(rename = "string[]")]
    StringArray,
    #[serde(rename = "number[]")]
    NumberArray,
    #[serde(rename = "boolean[]")]
    BooleanArray,
}

/// `TelemetryAttributeMetadata`。
///
/// 所有属性定义共有的元数据。它通过 `#[serde(flatten)]` 合并进
/// [`TelemetryAttributeDefinition`] 的 JSON，因此线上形状是
/// `{ "type": "...", "description": "...", ... }`，而不是嵌套的 `meta` 对象。
/// `description` 在 TS 里是必填；`sensitive`/`cardinality` 只是给采集方的提示，本 crate
/// 不做脱敏或索引策略。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TelemetryAttributeMetadata {
    /// 人读描述（schema 要求必填）。
    pub description: String,
    /// 是否敏感数据（如凭据、个人标识）；仅提示，本 crate 不做脱敏。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sensitive: Option<bool>,
    /// 基数提示：该属性的不同取值规模。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cardinality: Option<Cardinality>,
}

/// `cardinality?: "low" | "high"`。
///
/// 提示该属性的不同取值规模（低基数/高基数）；本 crate 只保存该值，不做解释或校验。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Cardinality {
    /// 取值种类少（例如状态枚举）。
    Low,
    /// 取值种类多（例如请求 id）。
    High,
}

/// `TelemetryAttributeDefinition`：TS 是带 `type` 判别的联合。Rust 用内部标记枚举保留
/// 各分支独有的 `values` / `elementValues` / `examples`。
///
/// 序列化形状：`#[serde(tag = "type")]` 把 `type` 写成对象内的判别键，`meta` 用 `flatten`
/// 展开，因此六个分支的 JSON 都是
/// `{ "type": ..., "description": ..., "sensitive"?: ..., "cardinality"?: ... }`。
/// 标量类型的取值域字段是 `values`，数组类型是 `elementValues`；`examples` 的元素形状与
/// 属性类型一致（数组类型的 example 本身是数组）。这些字段只描述 schema，运行时不做校验。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum TelemetryAttributeDefinition {
    /// 字符串属性。
    #[serde(rename = "string")]
    String {
        #[serde(flatten)]
        meta: TelemetryAttributeMetadata,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        values: Option<Vec<String>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        examples: Option<Vec<String>>,
    },
    /// number 属性；`values`/`examples` 中的非有限值同样只是 schema 数据。
    #[serde(rename = "number")]
    Number {
        #[serde(flatten)]
        meta: TelemetryAttributeMetadata,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        values: Option<Vec<f64>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        examples: Option<Vec<f64>>,
    },
    /// boolean 属性。
    #[serde(rename = "boolean")]
    Boolean {
        #[serde(flatten)]
        meta: TelemetryAttributeMetadata,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        values: Option<Vec<bool>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        examples: Option<Vec<bool>>,
    },
    /// 字符串数组属性；元素取值域用 `elementValues` 而不是 `values`。
    #[serde(rename = "string[]")]
    StringArray {
        #[serde(flatten)]
        meta: TelemetryAttributeMetadata,
        #[serde(
            default,
            rename = "elementValues",
            skip_serializing_if = "Option::is_none"
        )]
        element_values: Option<Vec<String>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        examples: Option<Vec<Vec<String>>>,
    },
    /// number 数组属性；元素取值域用 `elementValues`。
    #[serde(rename = "number[]")]
    NumberArray {
        #[serde(flatten)]
        meta: TelemetryAttributeMetadata,
        #[serde(
            default,
            rename = "elementValues",
            skip_serializing_if = "Option::is_none"
        )]
        element_values: Option<Vec<f64>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        examples: Option<Vec<Vec<f64>>>,
    },
    /// boolean 数组属性；元素取值域用 `elementValues`。
    #[serde(rename = "boolean[]")]
    BooleanArray {
        #[serde(flatten)]
        meta: TelemetryAttributeMetadata,
        #[serde(
            default,
            rename = "elementValues",
            skip_serializing_if = "Option::is_none"
        )]
        element_values: Option<Vec<bool>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        examples: Option<Vec<Vec<bool>>>,
    },
}

/// `TelemetryStartAttributeDefinition` / `TelemetryEventAttributeDefinition`
/// （两者形状相同：定义 + `required`）。
///
/// `definition` 用 `flatten` 展开，因此 JSON 是属性定义对象再加一个 `"required"` 布尔。
/// `required` 只对 schema 的类型检查有意义；Rust 端没有对应的编译期校验（见文件末尾
/// “仅类型层面的名字”一节）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TelemetryRequiredAttributeDefinition {
    /// 属性本身的类型与元数据。
    #[serde(flatten)]
    pub definition: TelemetryAttributeDefinition,
    /// 启动/记录该事件时是否必须提供此属性。
    pub required: bool,
}

/// `TelemetryEventDefinition`。
///
/// schema 中一个事件的声明：描述 + 属性表。事件名本身是外层 map 的键；
/// `attributes` 用 `IndexMap` 保留声明顺序（schema 是可序列化数据，顺序会影响快照）。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct TelemetryEventDefinition {
    /// 事件描述。
    pub description: String,
    /// 事件属性名 → 定义（按声明顺序）。
    pub attributes: IndexMap<String, TelemetryRequiredAttributeDefinition>,
}

/// `TelemetryParentDefinition`。
///
/// schema 对 span 父级来源的声明，TS 的判别联合（`kind` 为标签）。名称即约定：
/// `any` 允许任意父级；`root_or_external` 表示根或外部父级；`spans` 限定父 span 名。
/// 本 crate 不在运行时使用该声明（`TypedSpanStarter` 忽略整份 schema）。
///
/// `"root_or_external"` 是 TS 的字面量（不是 camelCase 拼写），因此显式 `rename`。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum TelemetryParentDefinition {
    /// 允许任意父级（TS 侧最常见的宽松声明）。
    #[serde(rename = "any")]
    Any,
    /// 根 span 或来自本 recorder/进程之外的父级。
    #[serde(rename = "root_or_external")]
    RootOrExternal,
    /// 父级必须是指定名字的 span 之一；`spans` 是允许作为父级的 span 名列表。
    #[serde(rename = "spans")]
    Spans { spans: Vec<String> },
}

/// `TelemetrySpanDefinition`。
///
/// `parents` 在 TS 里是必填且非 null（`parents: TelemetryParentDefinition`）。冻结的 Rust 形状是
/// `Option<TelemetryParentDefinition>`（公开字段类型不能改），因此**显式**写 `None` 仍会序列化成
/// `"parents":null` —— 一个 TS 类型系统禁止、且 TS 运行时不会产生的值。这条 Rust-only 路径保留给
/// 需要表达「未声明」的调用方；[`Default`] 不产生它：和 `status.default` 一样取 TS 可表达的值
/// （`parents: {kind:"any"}`，即 TS 侧最常见的宽松声明）。真实的 schema 路径
/// （`define_telemetry_schema` 的调用方与反序列化）总是带值。
///
/// 其余字段与 TS 的 `TelemetrySpanDefinition` 一一对应；schema 只是普通可序列化数据，
/// 运行时不做校验。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TelemetrySpanDefinition {
    /// span 描述。
    pub description: String,
    /// 父级约束；显式 `None` 会序列化成 `"parents":null`（TS 无法表达，见上）。
    pub parents: Option<TelemetryParentDefinition>,
    /// 启动属性定义（按声明顺序）。
    pub start_attributes: IndexMap<String, TelemetryRequiredAttributeDefinition>,
    /// 结束属性定义（按声明顺序）；没有 `required` 维度。
    pub end_attributes: IndexMap<String, TelemetryAttributeDefinition>,
    /// 事件定义；`None` 表示 schema 未声明事件。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub events: Option<IndexMap<String, TelemetryEventDefinition>>,
    /// 状态约定。
    pub status: TelemetryStatusDefinition,
}

impl Default for TelemetrySpanDefinition {
    // 手写而不是 derive：derive 会把 `parents` 置为 `None`（TS 要求该字段必有值）；
    // `status` 会复用 TelemetryStatusDefinition 的手写 Default（"ok"）。这里显式给出
    // TS 能表达的 parents。
    fn default() -> Self {
        TelemetrySpanDefinition {
            description: String::new(),
            parents: Some(TelemetryParentDefinition::Any),
            start_attributes: IndexMap::new(),
            end_attributes: IndexMap::new(),
            events: None,
            status: TelemetryStatusDefinition::default(),
        }
    }
}

/// `status: { default: "ok"; errorWhen: string }`。
///
/// `default` 在 TS 里是字面量 `"ok"`，因此 [`Default`] 用 `"ok"`（而不是 `String::default()`
/// 的空串 —— 那个值 TS 类型系统无法表达）。
///
/// `error_when` 是 schema 作者对“何时判定 error”的文本约定；本 crate 不做求值。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TelemetryStatusDefinition {
    /// 恒为字面量 "ok"。
    pub default: String,
    /// 何时标记为错误的说明（运行时忽略）。
    pub error_when: String,
}

impl Default for TelemetryStatusDefinition {
    // 同上：手写默认值以保持 TS 字面量 "ok"，而不是空串。
    fn default() -> Self {
        TelemetryStatusDefinition {
            default: "ok".to_string(),
            error_when: String::new(),
        }
    }
}

/// `TelemetrySchemaDefinition`。
///
/// 一份遥测 schema：版本、span 声明表、可选的指标声明表。schema 只是可 JSON 序列化的
/// 普通数据（`define_telemetry_schema` 是恒等函数），本 crate 不校验版本兼容性，也不在
/// 运行时使用这些声明。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct TelemetrySchemaDefinition {
    /// schema 版本号（TS number；具体含义由调用方约定，本 crate 不解释）。
    pub version: f64,
    /// span 名 → 声明，按声明顺序。
    pub spans: IndexMap<String, TelemetrySpanDefinition>,
    /// 指标名 → 声明；`None` 表示未声明指标。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metrics: Option<IndexMap<String, crate::metrics::TelemetryMetricDefinition>>,
}

/// `defineTelemetrySchema(schema)`：TS 是恒等函数，只为类型推断；Rust 同样是恒等。
///
/// 参数与返回值是同一个值（按值传入再返回，不克隆、不校验、不冻结）。函数不做任何转换，
/// 只作为“这是一份 schema”的标记，保留调用点的可读性与对称性。
pub fn define_telemetry_schema(schema: TelemetrySchemaDefinition) -> TelemetrySchemaDefinition {
    schema
}

// ---------------------------------------------------------------------------
// TypedSpanStarter（createTypedSpanStarter）
// ---------------------------------------------------------------------------

/// `TypedSpanStarter`：绑定到某个显式父上下文的「按名启动 span」入口。
///
/// TS 的 `createTypedSpanStarter(context, schemas)` 返回一个重载集合，schema 只用于类型推断、
/// 运行时被忽略（形参名 `_schemas`）。Rust 没有重载，因此建模为持有上下文的普通结构体：
/// [`start`](TypedSpanStarter::start) 接收 `name` + `attributes`（与 TS 一样无法传 `links`），
/// 并在回调里再给一个**绑定到回调 span** 的子 starter。
///
/// 对象安全与所有权：内部持有 `Arc<dyn TelemetryContext>`，因此可跨任务克隆使用。
///
/// TS: `packages/telemetry/src/index.ts#createTypedSpanStarter`
pub struct TypedSpanStarter {
    context: Arc<dyn TelemetryContext>,
}

impl TypedSpanStarter {
    fn new(context: Arc<dyn TelemetryContext>) -> Self {
        TypedSpanStarter { context }
    }

    /// 绑定到该 starter 的上下文（`bindTypedSpanStarter(ctx)` 的 `ctx`）。
    ///
    /// 返回内部 `Arc` 的借用而不是克隆；需要长期持有时自行 `Arc::clone`。
    pub fn context(&self) -> &Arc<dyn TelemetryContext> {
        &self.context
    }

    /// `startSpan(name, attributes, callback(span, startChildSpan))`。
    ///
    /// 回调接收的第二个参数是**每次调用重新绑定**到回调 span 的 starter，所以孙 span 的父级
    /// 是当前 span（TS 的 `bindTypedSpanStarter(span)`）。
    ///
    /// 回调**返回**的值原样成为 `Ok(value)`（TS 的 promise resolve）；要让 `Err` 走拒绝路径
    /// （span 标记 error），在回调里调用 [`TelemetryContextExt::start_span_try`]，或直接用
    /// 擦除的 [`TelemetryContext::start_span`] 并在回调 future 里返回 `Err`。
    pub async fn start<T, F>(
        &self,
        name: &str,
        attributes: SpanAttributes,
        callback: F,
    ) -> Result<T, TelemetryError>
    where
        T: Send + 'static,
        F: FnOnce(Arc<dyn TelemetrySpan>, TypedSpanStarter) -> BoxFuture<'static, T>
            + Send
            + 'static,
    {
        let context = Arc::clone(&self.context);
        // 与 TS 的 `startSpan({ name, attributes }, ...)` 相同：typed starter 只有
        // name + attributes，没有传 links 的入口。
        let options = SpanOptions {
            name: name.to_string(),
            attributes: Some(attributes),
            links: None,
        };
        context
            .start_span_typed(options, move |span| {
                // 关键：子 starter 绑定到**本次回调的 span**（而不是外层 starter 的
                // context），所以 callback 里再启动的 span 以当前 span 为父。
                let child_context: Arc<dyn TelemetryContext> = span.clone();
                let child_starter = TypedSpanStarter::new(child_context);
                callback(span, child_starter)
            })
            .await
    }
}

impl Clone for TypedSpanStarter {
    fn clone(&self) -> Self {
        TypedSpanStarter {
            context: Arc::clone(&self.context),
        }
    }
}

/// `createTypedSpanStarter(context, schemas)`。
///
/// `schemas` 在运行时被忽略；保留参数是为了让调用点在签名上带上「schema 词汇表」。Rust 没有
/// 重载与 const 泛型推断，schema 的名字/kind 校验不在本层表达（仅类型层面的保证，见 spec §25
/// 第 4 条）。
///
/// # 参数与返回
///
/// - `context`：新 starter 绑定的父上下文；通常是根上下文、某个 span 或 `extract` 的父级。
/// - `schemas`：只用于在签名上携带 schema 词汇表的切片，运行时完全忽略。
/// - 返回持有 `context` 的 [`TypedSpanStarter`]；对它的每次 `start` 都会用该上下文作为父级。
///
/// TS: `packages/telemetry/src/index.ts#createTypedSpanStarter`
pub fn create_typed_span_starter(
    context: Arc<dyn TelemetryContext>,
    schemas: &[TelemetrySchemaDefinition],
) -> TypedSpanStarter {
    // 显式消费参数以避免 unused 警告，同时表明忽略是刻意的（TS 形参名 `_schemas`）。
    let _ = schemas;
    TypedSpanStarter::new(context)
}

// ---------------------------------------------------------------------------
// 仅类型层面的名字（spec §19.2 导出清单）
// ---------------------------------------------------------------------------
//
// TS 用这些名字在编译期约束 schema 的 span/metric 名与精确属性；Rust 没有字符串字面量类型与
// 重载，无法表达同样的编译期约束（spec §25 第 4 条：仅类型层面的保证在 Rust 中丢失）。为了让
// §19.2 的导出清单在 Rust 侧仍能逐名核对，这里提供**运行时等价物**：名字存在、可被引用，语义
// 是「该位置实际使用的值类型」，不提供编译期校验。
//
// 具体到各组：
// - `ExactTelemetryAttributes` / `Infer*`：TS 用条件类型区分必填与可选属性；Rust 全部退化为
//   同一个 `SpanAttributes`，必填性没有类型层面的表达。本 crate 的 `TypedSpanStarter` 也不做
//   运行时校验；是否使用 schema 由调用方决定。
// - `TelemetrySchemaSpanName` / `...EventName`：TS 是 schema 推出的字面量联合，Rust 只能退化为
//   `&'static str`（调用方自己保证字符串来自 schema）。
// - `SchemaTelemetry*` / `TelemetrySchemaSpanUnion`：TS 是带 schema 名/属性约束的 span 或仪表
//   类型；Rust 用对应的 trait object，约束只剩“实现了该 trait”。

/// TS 的 `TelemetryStartAttributeDefinition`（= 定义 + `required`）。
pub type TelemetryStartAttributeDefinition = TelemetryRequiredAttributeDefinition;
/// TS 的 `TelemetryEventAttributeDefinition`（= 定义 + `required`）。
pub type TelemetryEventAttributeDefinition = TelemetryRequiredAttributeDefinition;

/// TS 的 `ExactTelemetryAttributes`：精确属性集合 → 运行时就是属性 map。
pub type ExactTelemetryAttributes = SpanAttributes;
/// TS 的 `InferRequiredAndOptionalAttributes`。
pub type InferRequiredAndOptionalAttributes = SpanAttributes;
/// TS 的 `InferStartAttributes`。
pub type InferStartAttributes = SpanAttributes;
/// TS 的 `InferOptionalAttributes`。
pub type InferOptionalAttributes = SpanAttributes;
/// TS 的 `InferEventAttributes`。
pub type InferEventAttributes = SpanAttributes;
/// TS 的 `TelemetrySchemaSpanStartAttributes`。
pub type TelemetrySchemaSpanStartAttributes = SpanAttributes;
/// TS 的 `TelemetrySchemaSpanEndAttributes`。
pub type TelemetrySchemaSpanEndAttributes = SpanAttributes;
/// TS 的 `TelemetrySchemaSpanEventAttributes`。
pub type TelemetrySchemaSpanEventAttributes = SpanAttributes;

/// TS 的 `TelemetrySchemaSpanName`：schema 中声明的 span 名字面量联合。
pub type TelemetrySchemaSpanName = &'static str;
/// TS 的 `TelemetrySchemaSpanEventName`：schema 中声明的 event 名字面量联合。
pub type TelemetrySchemaSpanEventName = &'static str;

/// TS 的 `SchemaTelemetryCounter`：带 schema 属性约束的 counter。
pub type SchemaTelemetryCounter = Arc<dyn crate::metrics::TelemetryCounter>;
/// TS 的 `SchemaTelemetryHistogram`：带 schema 属性约束的 histogram。
pub type SchemaTelemetryHistogram = Arc<dyn crate::metrics::TelemetryHistogram>;
/// TS 的 `SchemaTelemetrySpan`：带 schema 名/属性约束的 span。
pub type SchemaTelemetrySpan = Arc<dyn TelemetrySpan>;
/// TS 的 `TelemetrySchemaSpanUnion`：多个 schema 的 span 联合。
pub type TelemetrySchemaSpanUnion = Arc<dyn TelemetrySpan>;
