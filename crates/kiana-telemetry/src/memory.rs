//! memory.ts：`InMemoryTelemetryContext` 与 `Recorded*` 快照类型。
//!
//! 参考实现把 span 记在进程内存里，并同时实现 [`TelemetryContext`]、
//! [`TelemetryPropagation`]、[`TelemetryMetrics`]、[`TelemetryStore`] 四个契约。
//!
//! # 与 TS 的映射要点
//!
//! - **身份注册表**：TS 用模块级 `WeakMap<TelemetryContext, TelemetryCarrier>`，跨实例可见。
//!   Rust 不能把 trait object 作为 map 键，且冻结的 [`TelemetryContext`] 没有 `identity()`，
//!   因此这里用**进程级** `LazyLock<Mutex<HashMap<(trace, span), TelemetryCarrier>>>`，
//!   键取自 [`TelemetryContext::span_context_id`] 的 `trace_id` + `span_id`。
//!   注册发生在 span 创建与 `extract` 时；`inject` / link / exemplar 只解析已注册的上下文。
//!   根上下文、惰性 span 与 no-op span 返回哨兵标识（空字符串），永不注册。
//! - **不变量**：已结算 span 仍留在注册表里（迟到的 `addLink` / `inject` 仍有效）；注册表
//!   进程内增长（TS 的 WeakMap 会被 GC），可接受并在此声明。
//! - **时钟**：`Date.now()` / `performance.now()` 通过 [`Clock`] 注入，测试可替换。
//! - **`Number(span)`**：`extract` 的本地父 id 用 JS `Number()` 语义（见 `jsval` 模块）。
//! - **字符串序**：`storedSpans` / `findTraces` 的 id 平局判定用 JS 的 UTF-16 code unit 比较
//!   （`jsval::js_utf16_cmp`），不是 Rust 的字节/标量序。
//! - **cursor**：`after[0]` / `after[1]` 复刻 JS 的括号访问（数组/对象属性/字符串下标）与
//!   `<` / `===` / `>` 的取值转换；`[null, id]`（非有限起点编码）在续页时按严格相等自然排除。
//!   解码走 `jsjson`（`JSON.parse` 等价），不是 `kiana_json::parse`：后者未开启
//!   `float_roundtrip`，最短表示可能被读成相差 1 ULP 的 `f64`（续页判据是严格相等，会错页），
//!   且对 `1e400` 报错而 `JSON.parse` 得到 `Infinity`。
//! - **排序**：`Array#sort` 用 `timsort`（V8 14.6 `array-sort.tq` 的逐行移植）。
//!   含 NaN 起点的比较器不是一致顺序关系，V8 的实现会给出一个具体（非稳定、非字典序）的顺序；
//!   Rust 的 `slice::sort_by` 会 **panic**（"user-provided comparison function does not
//!   correctly implement a total order"），而 `sort_unstable_by`/稳定排序的顺序也与 V8 不同。
//!   移植 TimSort 后两侧逐元素一致（含 104 个黄金向量与 4000 组随机用例的对照）。
//! - **取消（与 TS 的 `finally` 对齐）**：TS 的 `startInMemorySpan` 只在 `try` 块的 `await
//!   callback(span)` **完成**时执行 `finally` 结算；若调用方丢弃 promise 且被 await 的 inner
//!   work 永不完成，`finally` 永不运行，该 span 永久保持 `settled = false`，因此被 `storedSpans`
//!   （`findTraces` / `getTrace`）的 `settled` 过滤排除，只有 `getSpans()` 还能看到它。
//!   Rust 侧丢弃 `start_span(...)` 返回的 future 时回调 future 一并被 drop、`await` 永远不会
//!   「完成」，因此**不结算**（`settled = false`）—— 与 TS 的「abandoned promise」同一可观察
//!   结果。不要在 `Drop` 里补结算：那会让被取消/超时的调用以 `ok` 截断 span 的形态出现在 store
//!   里，而 TS 不会。需要「保证结算」的调用方应让 future 跑完（必要时用后台任务 +
//!   `CancellationToken` 取消回调内部的工作，而不是丢弃 outer future）。
//! - **cursor 的错误**：TS 侧两类失败的**错误名**是契约的一部分，这里逐一对齐：
//!   非法 cursor 由 `JSON.parse` 抛 `SyntaxError`（[`TelemetryError::with_name`] 记 `"SyntaxError"`）；
//!   `limit` 使 `slice(0, limit)` 得到空页后读 `traces.at(-1).spans` 抛 `TypeError`
//!   （文本逐字为 `Cannot read properties of undefined (reading 'spans')`）。消息**文本**来自
//!   V8 的 `SyntaxError` 模板，本单元 spec §22.5 明确接受该文本近似（Rust 文案不同），
//!   但错误名、抛出时机与「有匹配才抛」的语义都保持一致。
//! - **深嵌套 cursor**：`jsjson` 用显式栈解析，与 V8 一样接受任意深度；但 TS 随后在
//!   `after[0]` 的 `ToNumber` 里走 `Array#join`，V8 的 `join` 是递归的，约 4650 层处抛
//!   `RangeError: Maximum call stack size exceeded`；Rust 侧能算出同样的 `join` 文本（等价于
//!   返回空页）。只有敌对 cursor 能到达该深度。
//!
//! # 执行流程
//!
//! 1. **创建 recorder**：`InMemoryTelemetryContext::new` / `with_clock` 建立一份共享状态
//!    `Inner`（scope UUID、时钟、span/测量表），并在构造末尾把唯一的 `RecorderIdentity`
//!    装入 `OnceLock`。
//! 2. **启动 span**：`start_span` → `start_in_memory_span`。父 span 已结算时降级为惰性 span
//!    （不记录、仍执行回调）；否则分配数字 id、追加到 `state.spans`、在进程级注册表登记
//!    carrier，然后把 span 句柄交给回调。
//! 3. **结算 span**：回调 future 就绪后（`Ok` 或 `Err`）写入 `settled`、`endedMs` 与结算序号；
//!    `Err` 且回调未显式设状态时，按 `TelemetryError.name` 决定是否复制 `error` 详情。
//! 4. **查询 store**：`getTrace` / `findTraces` 都先经 `storedSpans`（过滤未结算、投影、
//!    排序）；`findTraces` 再分组、过滤、排序、按 `limit` 切片并生成 cursor，`getTrace`
//!    只按 trace id 过滤；`findMetrics` 按记录顺序直接投影。
//! 5. **记录测量**：仪表工厂创建 `Recorder`，每次 `add` / `record` 追加一条独立测量（不聚合），
//!    时间落在 10 秒桶的桶首。
//!
//! # 数据结构与并发
//!
//! - `Inner` 是跨句柄共享的核心：`MemState`（span 表、测量表、两个自增计数器）由一把
//!   `Mutex` 保护；scope 与时钟是只读字段。`state.spans` **只追加、从不删除**，因此
//!   `InMemorySpan` 保存的下标永久有效，不需要代际标记。
//! - 锁只在同步代码段内持有，没有 guard 跨越 `.await`；需要 await 的回调总在锁外执行。
//!   mutex 中毒时统一 `into_inner()` 继续使用：这是记录型诊断后端，一次 panic 不应让之后
//!   所有 span/metrics 调用连锁失败。
//! - `Inner` 强引用 `RecorderIdentity`，`RecorderIdentity` 只弱引用 `Inner`，避免
//!   `Arc` 环。`metrics()` / `propagation()` 返回的能力句柄与 `extract` 的根共用同一个
//!   `Arc<RecorderIdentity>`（对应 TS 里的 `this`，可用 `Arc::ptr_eq` 验证）。
//! - 进程级 `CONTEXT_CARRIERS` 只做“标识 → carrier”查询，不参与传播语义，也没有淘汰；
//!   它的生命周期与 TS 的 `WeakMap` 不同，这一点已在上文“不变量”中声明。
//! - 时间有两套来源：`startedMs` 用墙钟（`Clock::now_ms`），时长与事件相对时间用单调钟
//!   （`Clock::now_monotonic_ms`），因此墙钟回拨不会产生负时长。
//!
//! # 阅读顺序
//!
//! 先读公开快照类型（`Recorded*`）与 `Clock`，再读 `Inner` / `RecorderIdentity` 理解共享与
//! 生命周期，随后 `start_in_memory_span`（创建/结算）→ `InMemorySpan`（写入）→
//! `stored_spans_impl` / `find_traces_impl`（读路径）→ cursor 辅助函数（JS 语义边界）。
//!
//! TS: `packages/telemetry/src/memory.ts`

use std::collections::HashMap;
use std::sync::{Arc, LazyLock, Mutex, MutexGuard, OnceLock, Weak};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::index::{
    BoxFuture, ErasedValue, MetricKind, MetricQuery, SpanAttributes, SpanCallback, SpanLink,
    SpanLinkContext, SpanOptions, SpanStatus, StatusError, StoredMetric, StoredSpan,
    StoredSpanEvent, StoredSpanLink, StoredTrace, TelemetryCarrier, TelemetryContext,
    TelemetryContextExt, TelemetryError, TelemetryPropagation, TelemetrySpan, TelemetryStore,
    TracePage, TraceQuery,
};
use crate::jsval;
use crate::metrics::{
    HistogramOptions, InstrumentOptions, TelemetryCounter, TelemetryHistogram, TelemetryMetrics,
    TelemetryUpDownCounter,
};
use crate::noop::noop_telemetry_context;

// ---------------------------------------------------------------------------
// 公开的 recorded 类型
// ---------------------------------------------------------------------------

/// `RecordedTelemetryEvent`：回调期间记录的事件（不含相对时间）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RecordedTelemetryEvent {
    /// 事件名（`addEvent(name, ...)` 的 `name` 原样保存）。
    pub name: String,
    /// 事件属性快照：记录时已深拷贝并按 JS 键序重排，之后修改调用方的 map 不影响快照。
    pub attributes: SpanAttributes,
}

/// `RecordedTelemetryLink`：link 的 carrier 快照与属性快照。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RecordedTelemetryLink {
    /// 对端上下文的 carrier 快照（只能来自身份注册表里已登记的上下文）。
    pub context: TelemetryCarrier,
    /// link 属性快照；原始 `SpanLink.attributes` 为 `None` 时这里是空 map。
    pub attributes: SpanAttributes,
}

/// `RecordedTelemetrySpan`：`getSpans()` 的分离快照。
///
/// `parentId` 是 recorder 内的数字 id（根或外部父级为 `null`；同 scope 的非数字 span id
/// 解析为 `NaN`，序列化为 JSON `null`）。`endSequence` 只在结算后出现。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RecordedTelemetrySpan {
    /// 本 recorder 内单调递增的 span id（从 1 开始；跨 recorder 不可比较）。
    pub id: f64,
    /// 父级在本 recorder 内的数字 id。根 span 与外部（跨 scope）父级为 `None`；
    /// 同 scope 的非数字 span 文本解析为 `Some(NaN)`，由于 `NaN` 不是合法 JSON 数字，
    /// 序列化结果为 `null`。
    pub parent_id: Option<f64>,
    /// 自身 carrier，含 `kiana-memory-scope` / `kiana-memory-span` / `kiana-memory-trace`。
    pub context: TelemetryCarrier,
    /// 父级 carrier 快照；根 span 为 `None`。
    pub parent_context: Option<TelemetryCarrier>,
    /// 建 span 时已解析并深拷贝的 link；指向未注册上下文的 link 已被静默丢弃。
    pub links: Vec<RecordedTelemetryLink>,
    /// span 名。
    pub name: String,
    /// 属性快照（JS 键序、深拷贝）。
    pub attributes: SpanAttributes,
    /// 事件快照，按 `add_event` 的调用顺序排列。
    pub events: Vec<RecordedTelemetryEvent>,
    /// 状态快照（`StatusError` 也被复制，避免与内部状态共享）。
    pub status: SpanStatus,
    /// 是否已结算。未结算的 span 不会出现在 `find_traces` / `get_trace` 的结果里，
    /// 只有 `get_spans` 能看到。
    pub settled: bool,
    /// 结算序号：按结算发生顺序从 1 递增；未结算为 `None`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end_sequence: Option<f64>,
}

/// `RecordedTelemetryMeasurement`：`getMetrics()` 的分离快照。
///
/// 这是一条**原始测量**，不是聚合行：一次 `add` / `record` 对应一条。桶起点 `intervalMs`
/// 只出现在 `findMetrics` 的 `StoredMetric` 上，不在本快照里（对应 TS 的私有 timing 字段）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RecordedTelemetryMeasurement {
    /// 仪表种类，由创建它的工厂决定（`counter` / `upDownCounter` / `histogram`）。
    pub kind: MetricKind,
    /// 仪表名（工厂调用时的 `name`）。
    pub name: String,
    /// 创建仪表时传入的 `description`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// 创建仪表时传入的 `unit`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unit: Option<String>,
    /// 直方图的桶边界提示；只有 histogram 有值。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub boundaries: Option<Vec<f64>>,
    /// 本次记录的原始数值（不累加、不取绝对值）。
    pub value: f64,
    /// 属性快照（JS 键序、深拷贝）。
    pub attributes: SpanAttributes,
    /// exemplar 上下文：记录时能解析到已注册 span 才有值；无上下文、根上下文或未注册上下文
    /// 均为 `None`。本适配器的 counter / upDownCounter 公开签名没有 context 参数，
    /// 因此这两类测量恒为 `None`。
    pub context: Option<TelemetryCarrier>,
}

// ---------------------------------------------------------------------------
// Clock
// ---------------------------------------------------------------------------

/// `Date.now()` 与 `performance.now()` 的注入点。
///
/// TS 测试用 `vi.spyOn(Date, "now")` / `vi.spyOn(performance, "now")`；Rust 侧改为构造
/// `InMemoryTelemetryContext::with_clock`，语义一致（`now_ms` 是纪元毫秒，`now_monotonic_ms`
/// 是单调毫秒，只有差值可观察）。
///
/// # 实现约束
///
/// - 两个方法都以毫秒为单位返回 `f64`；`now_monotonic_ms` 的绝对值没有意义，只有两次调用
///   之差可观察，因此不需要与系统启动时刻对齐。
/// - 实现必须 `Send + Sync`：recorder 被多个 span / instrument 句柄共享，时钟可能从不同
///   线程读取。
/// - 墙钟回拨不会影响已记录 span 的时长，因为时长只用单调钟差值（见 `create_span` /
///   `settle_span`）；但 `startedMs` 本身仍是墙钟值，可能非单调。
pub trait Clock: Send + Sync {
    /// 纪元毫秒（`Date.now()`）。
    fn now_ms(&self) -> f64;
    /// 单调毫秒（`performance.now()`）；只用于求差。
    fn now_monotonic_ms(&self) -> f64;
}

/// 默认时钟：`SystemTime` 墙钟 + 基于进程内 `Instant` 原点的单调钟。
///
/// `MONOTONIC_ORIGIN` 是 `LazyLock`，原点是**本进程首次读取单调时钟**的时刻，而不是进程
/// 创建时刻；因为只用于求差，原点位置不影响任何可观察结果。`now_ms` 在系统时间早于
/// `UNIX_EPOCH` 时返回 `0.0`（`unwrap_or(0.0)`），这是防御性分支。
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemClock;

/// 进程内单调时钟的原点：首次访问时初始化的 `Instant`，供 `SystemClock` 求差。
static MONOTONIC_ORIGIN: LazyLock<Instant> = LazyLock::new(Instant::now);

impl Clock for SystemClock {
    fn now_ms(&self) -> f64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as f64)
            .unwrap_or(0.0)
    }

    fn now_monotonic_ms(&self) -> f64 {
        MONOTONIC_ORIGIN.elapsed().as_micros() as f64 / 1000.0
    }
}

// ---------------------------------------------------------------------------
// 身份注册表（模块级，跨实例；TS 的 WeakMap 等价物）
// ---------------------------------------------------------------------------

/// 注册表键：`(trace_id, carrier_id)`。两个分量都直接取 carrier 文本，不做格式校验。
type ContextKey = (String, String);

/// 进程级身份注册表：把已见过的 carrier 标识映射回 carrier 本体。
///
/// - 写入点只有 `register_context`（span 创建与 `extract` 时）；读取点是 `inject`、link
///   解析、histogram exemplar 以及所有 `span_context_id` 的查询方。
/// - 键包含 trace 与 `scope:span`，所以不同 trace 下的同名 span id 不会互相覆盖。
/// - 没有删除路径：进程内只增不减，与 TS 的 `WeakMap`（随对象回收）不同；模块文档已把
///   这一差异声明为可接受。
/// - 值存的是 carrier 克隆；调用方拿到的是分离副本，修改它不会影响注册表。
static CONTEXT_CARRIERS: LazyLock<Mutex<HashMap<ContextKey, TelemetryCarrier>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// 获取注册表锁；mutex 中毒时取回内部数据继续使用。
///
/// 注册表只存纯数据（字符串 map），一次持锁线程 panic 不会留下半更新的结构，因此忽略中毒
/// 标记比把失败传播给每个 span 创建点更符合“诊断不应中断业务”的定位。
fn registry() -> MutexGuard<'static, HashMap<ContextKey, TelemetryCarrier>> {
    CONTEXT_CARRIERS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// `carrierId(carrier)` = `` `${scope}:${span}` ``。
///
/// 缺失的键按空字符串处理；因此两个键都缺失时结果固定是 `":"`，仍是一个字符串标识（只是
/// 不会对应任何真实 span）。[`SpanLinkContext`] 的 `span_id` 字段用的就是这个文本。
fn carrier_id(carrier: &TelemetryCarrier) -> String {
    format!(
        "{}:{}",
        carrier
            .get("kiana-memory-scope")
            .map(String::as_str)
            .unwrap_or(""),
        carrier
            .get("kiana-memory-span")
            .map(String::as_str)
            .unwrap_or("")
    )
}

/// 读取 `kiana-memory-trace`；缺失时返回 `None`（调用方通常退化为空 trace）。
fn carrier_trace(carrier: &TelemetryCarrier) -> Option<&str> {
    carrier.get("kiana-memory-trace").map(String::as_str)
}

/// 把 carrier 登记进进程级注册表。
///
/// 只有存在 trace 键时才登记：没有 trace 的 carrier 无法构成 `(trace, scope:span)` 键，
/// 也无从与其它 span 区分。scope/span 缺失仍会生成 `":"` 形态的键；这与 `extract`
/// 接受空字符串标识的行为一致（空字符串在 JS 里同样满足 `typeof === "string"`）。
fn register_context(carrier: &TelemetryCarrier) {
    if let Some(trace) = carrier_trace(carrier) {
        let key = (trace.to_string(), carrier_id(carrier));
        registry().insert(key, carrier.clone());
    }
}

/// 按 `span_context_id()` 解析已注册的 carrier（分离副本）。
///
/// 哨兵标识（trace 与 span 都为空）直接返回 `None`：根上下文、惰性 span 与 no-op 上下文
/// 都用它表示“我没有身份”，因此 `inject` 自然得到 `undefined`。其余标识只查注册表，
/// 查不到就是 `None`，不做格式推断。
fn resolve_context(id: &SpanLinkContext) -> Option<TelemetryCarrier> {
    if id.trace_id.is_empty() && id.span_id.is_empty() {
        return None;
    }
    registry()
        .get(&(id.trace_id.clone(), id.span_id.clone()))
        .cloned()
}

/// “无身份”哨兵：根上下文、惰性 span 与 no-op span 共用。
///
/// 两个 id 都为空字符串时 `resolve_context` 直接短路为 `None`（不再查注册表）；
/// `parent_id` 不参与判定。注册表查询未命中同样返回 `None`，调用方无需区分两者。
fn sentinel_context_id() -> SpanLinkContext {
    SpanLinkContext {
        trace_id: String::new(),
        span_id: String::new(),
        parent_id: None,
    }
}

/// 由 carrier 得到 `span_context_id()` 视图；`parent` 是父 carrier（若有）。
///
/// `span_id` 用 `scope:span` 文本（与 store 的 `StoredSpan.id` 同一格式），`parent_id` 同样
/// 取父 carrier 的文本。这里只做格式转换，不查注册表。
fn context_id_from_carrier(
    carrier: &TelemetryCarrier,
    parent: Option<&TelemetryCarrier>,
) -> SpanLinkContext {
    SpanLinkContext {
        trace_id: carrier_trace(carrier).unwrap_or("").to_string(),
        span_id: carrier_id(carrier),
        parent_id: parent.map(carrier_id),
    }
}

// ---------------------------------------------------------------------------
// 拷贝辅助（对应 copyAttributes / copyLink / copyStatus）
// ---------------------------------------------------------------------------

/// `copyAttributes`：丢弃 `undefined`（Rust 无此值）、深拷贝数组、按 JS 键序重排。
///
/// `AttributeValue` 含 `Vec`，而 `sort_js_keys` 会 clone 每个值，因此返回值与内部状态完全
/// 分离：调用方之后修改快照不会影响 recorder。`None` 统一映射为空 map（冻结契约里属性
/// 字段不可省略，空 map 与“没有属性”在 JSON 上都是 `{}`）。
fn copy_attributes(attributes: Option<&SpanAttributes>) -> SpanAttributes {
    match attributes {
        Some(map) => jsval::sort_js_keys(map),
        None => IndexMap::new(),
    }
}

/// `copyLink`：只接受注册表里已知的上下文；未知上下文返回 `None`（link 被静默丢弃）。
fn copy_link(link: &SpanLink) -> Option<RecordedTelemetryLink> {
    let context = resolve_context(&link.context)?;
    Some(RecordedTelemetryLink {
        context,
        attributes: copy_attributes(link.attributes.as_ref()),
    })
}

/// `copyStatus`：`ok` / 带 error 的 error / 不带 error 的 error，全部返回新副本。
///
/// `StatusError` 由堆上字符串组成：逐字段展开（或直接 `clone` 都能完成深拷贝）；
/// 这里显式展开是为了让“快照不共享内部状态”的约束一眼可见。
fn copy_status(status: &SpanStatus) -> SpanStatus {
    match status {
        SpanStatus::Ok => SpanStatus::Ok,
        SpanStatus::Error { error } => SpanStatus::Error {
            error: error
                .as_ref()
                .map(|StatusError { name, message }| StatusError {
                    name: name.clone(),
                    message: message.clone(),
                }),
        },
    }
}

// ---------------------------------------------------------------------------
// 内部状态
// ---------------------------------------------------------------------------

/// 记录中的事件；与公开快照 `RecordedTelemetryEvent` 的区别是多了相对时间。
struct MutableEvent {
    /// 相对 span 起点的毫秒数（由单调钟算出），不是 epoch 时间。
    time_ms: f64,
    name: String,
    attributes: SpanAttributes,
}

/// 记录中的 span：公开快照 `RecordedTelemetrySpan` 的内部可变形态。
///
/// 两套时间字段不能混用：`started_ms` 是墙钟（对外时间戳），`clock` 是创建时的单调钟读数，
/// 结算与事件时间都通过 `now_monotonic - clock` 换算成相对量，从而不受墙钟调整影响。
struct MutableRecordedSpan {
    /// 墙钟纪元毫秒（`Date.now()` 语义），对外作为 `startedMs`。
    started_ms: f64,
    /// 创建时的单调钟读数，仅用于计算时长与事件相对时间。
    clock: f64,
    /// 结算时写入的墙钟结束时间；未结算为 `None`。
    ended_ms: Option<f64>,
    id: f64,
    parent_id: Option<f64>,
    context: TelemetryCarrier,
    parent_context: Option<TelemetryCarrier>,
    links: Vec<RecordedTelemetryLink>,
    name: String,
    attributes: SpanAttributes,
    events: Vec<MutableEvent>,
    status: SpanStatus,
    /// 回调是否显式调用过 `set_status`；为真时结算不再用错误覆盖状态（TS 的显式优先）。
    explicit_status: bool,
    /// 是否已结算。只有 `settle_span` 会置真，且每个 span 只应结算一次。
    settled: bool,
    /// 结算序号，结算时分配；未结算为 `None`。
    end_sequence: Option<f64>,
}

/// 内部测量行：在公开快照之外多存一个 10 秒桶起点。
///
/// 桶起点 = `floor(now_ms / 10_000) * 10_000`，只在 `find_metrics` 里作为
/// `StoredMetric.interval_ms` 暴露；`get_metrics` 快照不包含它（对应 TS 的私有 timing 字段）。
struct MeasurementRow {
    /// 10 秒对齐的桶起点（墙钟毫秒）。
    interval_ms: f64,
    measurement: RecordedTelemetryMeasurement,
}

/// 受 `Inner.state` 一把 `Mutex` 保护的全部可变状态。
///
/// `spans` **只追加**：`InMemorySpan` 用下标寻址，删除元素会让已有句柄指向错误的 span，
/// 因此实现从不移除元素（包括已结算的）。
struct MemState {
    /// 按创建顺序排列；未结算的 span 也在其中（`get_spans` 会看到）。
    spans: Vec<MutableRecordedSpan>,
    /// 按记录顺序排列的原始测量（不聚合）。
    measurements: Vec<MeasurementRow>,
    /// 下一个 span 数字 id，从 1.0 开始按 1 递增。
    next_span_id: f64,
    /// 下一个结算序号，从 1.0 开始按 1 递增。
    next_end_sequence: f64,
}

/// `InMemoryTelemetryContext` 的共享核心：状态、scope、时钟与唯一的 recorder 身份。
///
/// 所有克隆句柄共享同一个 `Arc<Inner>`；`Inner` 自身不实现 trait，对外的 context /
/// propagation / metrics 能力由 `RecorderIdentity`（TS 的 `this`）或
/// `InMemoryTelemetryContext` 提供。
struct Inner {
    state: Mutex<MemState>,
    scope: String,
    clock: Arc<dyn Clock>,
    /// TS 的 `private readonly state = { metrics: this, propagation: this, … }`：同一个 recorder
    /// 对象同时充当 `metrics` 能力、`propagation` 能力与 `extract` 的根上下文
    /// （`readonly metrics = this; readonly propagation = this`）。Rust 里那个对象是
    /// [`RecorderIdentity`]，`Inner` 强引用它（`RecorderIdentity` 只弱引用 `Inner`，不形成环）。
    recorder: OnceLock<Arc<RecorderIdentity>>,
}

impl Inner {
    /// 获取状态锁；中毒时继续使用内部数据（理由同 `registry`）。
    ///
    /// 持有 guard 的代码不得重入 `state()`：`std::sync::Mutex` 不可重入，同线程二次加锁
    /// 会死锁。现有的 `with_record` 闭包都只操作字段，不回调 recorder。
    fn state(&self) -> MutexGuard<'_, MemState> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// TS 的 `this`（recorder 对象本身）。
    ///
    /// 不变量：`RecorderIdentity` 在 `Inner::new` 构造末尾写入 `OnceLock`，之后不再修改；
    /// 因此任何通过 `InMemoryTelemetryContext` 创建或 `Inner` 可达的路径都能安全 `expect`。
    fn recorder(&self) -> Arc<RecorderIdentity> {
        Arc::clone(
            self.recorder
                .get()
                .expect("recorder is installed during construction"),
        )
    }

    /// `this` 作为 `TelemetryContext`（`extract` 的根、span 的父上下文能力来源）。
    ///
    /// `recorder()` 与三个 `recorder_*` 访问器返回的是同一个 `Arc<RecorderIdentity>` 的
    /// 克隆；调用方可以用 `Arc::ptr_eq` 验证它们与 `metrics()` / `propagation()` 的返回值
    /// 是同一对象（对应 TS 的 `context.metrics === recorder`）。
    fn recorder_context(&self) -> Arc<dyn TelemetryContext> {
        self.recorder()
    }

    /// `this` 作为 `TelemetryMetrics`（`context.metrics === recorder`）。
    fn recorder_metrics(&self) -> Arc<dyn TelemetryMetrics> {
        self.recorder()
    }

    /// `this` 作为 `TelemetryPropagation`（`context.propagation === recorder`）。
    fn recorder_propagation(&self) -> Arc<dyn TelemetryPropagation> {
        self.recorder()
    }

    /// 建立共享状态：scope 用随机 UUID v4，计数器从 1.0 起，`OnceLock` 暂时为空。
    ///
    /// 构造顺序由弱引用约束决定：必须先有 `Arc<Inner>` 才能 `Arc::downgrade`，也必须先
    /// 创建 recorder 才能放进 `OnceLock`。这样 recorder 只持有弱引用，不会形成
    /// `Inner -> RecorderIdentity -> Inner` 的 `Arc` 环；而 `Inner` 直到销毁前都能拿到
    /// 同一个 recorder。
    fn new(clock: Arc<dyn Clock>) -> Arc<Inner> {
        let inner = Arc::new(Inner {
            state: Mutex::new(MemState {
                spans: Vec::new(),
                measurements: Vec::new(),
                next_span_id: 1.0,
                next_end_sequence: 1.0,
            }),
            scope: uuid::Uuid::new_v4().to_string(),
            clock,
            recorder: OnceLock::new(),
        });
        let recorder = Arc::new(RecorderIdentity {
            inner: Arc::downgrade(&inner),
        });
        let _ = inner.recorder.set(recorder);
        inner
    }
}

// ---------------------------------------------------------------------------
// 上下文对象
// ---------------------------------------------------------------------------

/// `InMemoryTelemetryContext`：进程内记录 span 的后端无关参考实现。
///
/// `Clone` 共享同一份记录状态（对应 TS 的对象引用）。
///
/// TS: `packages/telemetry/src/memory.ts#InMemoryTelemetryContext`
#[derive(Clone)]
pub struct InMemoryTelemetryContext {
    inner: Arc<Inner>,
}

impl Default for InMemoryTelemetryContext {
    fn default() -> Self {
        Self::new()
    }
}

impl InMemoryTelemetryContext {
    /// `new InMemoryTelemetryContext()`：每个实例得到独立 scope（`crypto.randomUUID()`）。
    ///
    /// 独立性只体现在 scope 与状态上；进程级身份注册表仍然共享（见模块文档）。
    /// `Default` 与 `new` 等价。
    pub fn new() -> Self {
        Self::with_clock(Arc::new(SystemClock))
    }

    /// 注入自定义时钟（对应 TS 测试对 `Date.now` / `performance.now` 的 spy）。
    ///
    /// 时钟同时提供墙钟与单调钟：测试若只钉住其中一个，`startedMs` 与时长/事件时间的断言
    /// 会分别受影响。生产代码一般用 [`SystemClock`]。
    pub fn with_clock(clock: Arc<dyn Clock>) -> Self {
        InMemoryTelemetryContext {
            inner: Inner::new(clock),
        }
    }

    /// 该 recorder 的 scope（UUID v4，小写 36 字符）。
    pub fn scope(&self) -> &str {
        &self.inner.scope
    }

    /// `getSpans()`：按 span 启动顺序的分离快照（含未结算 span）。
    ///
    /// - “启动顺序”是**创建顺序**（`state.spans` 的追加顺序），不是 `startedMs` 排序结果；
    ///   测试钉住时钟时两者可能相同，但并发/时钟回拨下不保证。
    /// - 快照会复制属性、link、事件、context 与状态，调用方修改返回值不会影响 recorder。
    /// - 未结算的 span 也会出现（`settled = false`）；公开 API 里只有这里能看到“被丢弃的
    ///   future”遗留的 span。
    pub fn get_spans(&self) -> Vec<RecordedTelemetrySpan> {
        let state = self.inner.state();
        state
            .spans
            .iter()
            .map(|span| RecordedTelemetrySpan {
                id: span.id,
                parent_id: span.parent_id,
                context: span.context.clone(),
                parent_context: span.parent_context.clone(),
                links: span
                    .links
                    .iter()
                    .map(|link| RecordedTelemetryLink {
                        context: link.context.clone(),
                        attributes: copy_attributes(Some(&link.attributes)),
                    })
                    .collect(),
                name: span.name.clone(),
                attributes: copy_attributes(Some(&span.attributes)),
                events: span
                    .events
                    .iter()
                    .map(|event| RecordedTelemetryEvent {
                        name: event.name.clone(),
                        attributes: copy_attributes(Some(&event.attributes)),
                    })
                    .collect(),
                status: copy_status(&span.status),
                settled: span.settled,
                end_sequence: span.end_sequence,
            })
            .collect()
    }

    /// `getMetrics()`：按记录顺序的分离快照（不聚合）。
    ///
    /// 每条记录对应一次 `add` / `record`，不做计数/求和/分位数聚合；10 秒桶起点不在快照里
    /// （见 [`RecordedTelemetryMeasurement`] 的说明），需要它时用 `find_metrics`。
    pub fn get_metrics(&self) -> Vec<RecordedTelemetryMeasurement> {
        let state = self.inner.state();
        state
            .measurements
            .iter()
            .map(|row| RecordedTelemetryMeasurement {
                kind: row.measurement.kind,
                name: row.measurement.name.clone(),
                description: row.measurement.description.clone(),
                unit: row.measurement.unit.clone(),
                boundaries: row.measurement.boundaries.clone(),
                value: row.measurement.value,
                attributes: copy_attributes(Some(&row.measurement.attributes)),
                context: row.measurement.context.clone(),
            })
            .collect()
    }

    /// `inject(context)`：已注册上下文（含已结算 span 与 extract 的父上下文）返回分离 carrier。
    ///
    /// `resolve_context` 先对哨兵标识（`trace_id` 与 `span_id` 都为空）短路返回 `None`
    /// （不查表），其余标识才按 `(trace_id, span_id)` 查询进程级注册表；两种路径未命中都
    /// 返回 `None`（JS 的 `undefined`）。已结算 span 仍在注册表里，因此迟到的 inject 仍有效。
    /// 返回值是 carrier 克隆，修改它不会改变注册表或后续 link 的解析结果。
    pub fn inject(&self, context: &dyn TelemetryContext) -> Option<TelemetryCarrier> {
        resolve_context(&context.span_context_id())
    }

    /// `extract(carrier)`：缺失/未知 carrier 返回 root；已识别标识被视为不透明字符串。
    ///
    /// 只要三个 `kiana-memory-*` 键都存在（值可以是任意字符串，包括空串），就构造一个
    /// `ExtractedParent`；缺任何一个则回退到 recorder 根。它不校验标识是否对应真实 span，
    /// 也不要求 scope 属于本 recorder —— 跨实例的标识同样被接受（注册表是进程级的）。
    /// 同 scope 的 span 文本会用 JS `Number()` 解析成本地父 id，供 `RecordedTelemetrySpan`
    /// 的数字 `parentId` 使用；解析结果可能是 `NaN`（此时 JSON 写作 `null`）。
    ///
    /// 返回值的 `Arc` 身份在「回退到 root」时稳定（所有回退共享同一个 root 上下文对象）。
    /// 成功识别时每次调用都新建一个 `ExtractedParent`，两次 `extract` 同一 carrier 不保证
    /// `Arc::ptr_eq`。
    pub fn extract(&self, carrier: Option<&TelemetryCarrier>) -> Arc<dyn TelemetryContext> {
        extract_context(&self.inner, carrier)
    }

    /// TS `extract(carrier)` 的**任意 JSON 值**入口（`{ [key]: 42 }` 这类非字符串 carrier）。
    ///
    /// `TelemetryCarrier` 是 `String` map，冻结的 [`TelemetryPropagation::extract`] 表达不了
    /// 「键存在但值不是字符串」；TS 侧这一分支会回退到根（`typeof value !== "string"`）。
    /// 这个方法接收 `serde_json::Value`，逐键按 `typeof` 判定，因此适配器一致性套件可以像 TS
    /// 一样覆盖 `{...carrier, [key]: 42}` 这一半。
    ///
    /// 判定规则：只有字符串值、且三个必需键都存在时才算可识别 carrier。数字/布尔/数组/对象/
    /// `null` 都算「非字符串」→ 回退到根（`JSON null` 与 `undefined`/缺失键同等对待）。
    ///
    /// 实现细节：非对象（数组、字符串、数字等）直接回退；对象中额外出现的键被忽略，只按
    /// scope → span → trace 的固定顺序复制三个必需键，再交给 `extract_context`。
    pub fn extract_json(&self, carrier: &Value) -> Arc<dyn TelemetryContext> {
        let Some(map) = carrier.as_object() else {
            return self.inner.recorder_context();
        };
        let mut decoded = TelemetryCarrier::new();
        for key in [
            "kiana-memory-scope",
            "kiana-memory-span",
            "kiana-memory-trace",
        ] {
            match map.get(key) {
                Some(Value::String(text)) => {
                    decoded.insert(key.to_string(), text.clone());
                }
                // `typeof value !== "string"`（缺失、null、数字、布尔、数组、对象）→ 根。
                _ => return self.inner.recorder_context(),
            }
        }
        extract_context(&self.inner, Some(&decoded))
    }

    /// `counter(name, options)`。
    ///
    /// 工厂不校验名字、不查 schema；`kind` 固定为 `Counter`，`description`/`unit` 原样保存。
    /// 返回的仪表持有 `Arc<Inner>`，因此即使所有 context 句柄都被丢弃，继续 `add` 仍会记录到
    /// 同一份状态（直到仪表本身被释放）。
    pub fn counter(
        &self,
        name: &str,
        options: Option<InstrumentOptions>,
    ) -> Arc<dyn TelemetryCounter> {
        create_counter(&self.inner, name, options)
    }

    /// `upDownCounter(name, options)`。
    ///
    /// 语义与 `counter` 相同，只是 `kind` 记为 `UpDownCounter`；实现不限制负值。
    pub fn up_down_counter(
        &self,
        name: &str,
        options: Option<InstrumentOptions>,
    ) -> Arc<dyn TelemetryUpDownCounter> {
        create_up_down_counter(&self.inner, name, options)
    }

    /// `histogram(name, options)`。
    ///
    /// 只有 histogram 的公开 `record` 带 exemplar 上下文参数；`boundaries` 作为元数据原样
    /// 记录，内存适配器不做分桶计算（每行仍是原始 value）。
    pub fn histogram(
        &self,
        name: &str,
        options: Option<HistogramOptions>,
    ) -> Arc<dyn TelemetryHistogram> {
        create_histogram(&self.inner, name, options)
    }

    /// `getTrace(traceId)`：按 trace 过滤的已结算 span；无匹配返回 `None`。
    ///
    /// 内部复用 `storedSpans`，因此 `spans` 的顺序与 `findTraces` 里的同一 trace 一致
    /// （`startedMs` 升序，平局按 `id` 的 UTF-16 顺序）。未结算 span 不会出现；trace 内只有
    /// 未结算 span 时同样返回 `None`（等价于空 trace）。
    pub async fn get_trace(&self, trace_id: &str) -> Option<StoredTrace> {
        get_trace_impl(&self.inner, trace_id)
    }

    /// `findTraces(query)`。`limit == 0` 且有匹配时返回 TS 的 `TypeError` 文本（见模块文档）。
    ///
    /// `query = None` 等价于默认查询（`limit` 20、无过滤）；`cursor` 非空时只返回严格位于
    /// 上一页最后一条之后的 trace。过滤条件里 `name` / `startMin` / `startMax` 看“最早 span”，
    /// `errors` / `attributes` 只要 trace 内任一 span 满足即可（见 `find_traces_impl`）。
    pub async fn find_traces(
        &self,
        query: Option<TraceQuery>,
    ) -> Result<TracePage, TelemetryError> {
        find_traces_impl(&self.inner, query)
    }

    /// `findMetrics(query)`：记录顺序、无聚合、无 limit。
    ///
    /// 每条原始测量映射成一行 `StoredMetric`：`count = 1`，`sum = min = max = value`，
    /// `intervalMs` 取 10 秒桶起点；`startMin` / `startMax` 是对桶起点的**闭区间**过滤。
    pub async fn find_metrics(&self, query: MetricQuery) -> Vec<StoredMetric> {
        find_metrics_impl(&self.inner, query)
    }
}

// ---------------------------------------------------------------------------
// 主上下文（InMemoryTelemetryContext 的 TelemetryContext 实现）
// ---------------------------------------------------------------------------

// 该实现让“上下文对象”本身也能直接充当 trait object。metrics/propagation 返回的是同一个
// recorder 身份；start_span 无父级（顶层 span）。根上下文的 span_context_id 用哨兵，
// 因此 root 永远不会被 inject 成 carrier，指向 root 的 link 在建 span 时被静默丢弃。
impl TelemetryContext for InMemoryTelemetryContext {
    fn metrics(&self) -> Option<Arc<dyn TelemetryMetrics>> {
        Some(self.inner.recorder_metrics())
    }

    fn propagation(&self) -> Option<Arc<dyn TelemetryPropagation>> {
        Some(self.inner.recorder_propagation())
    }

    fn start_span<'a>(
        &'a self,
        options: SpanOptions,
        callback: SpanCallback<'a>,
    ) -> BoxFuture<'a, Result<ErasedValue, TelemetryError>> {
        start_in_memory_span(Arc::clone(&self.inner), None, options, callback)
    }

    /// 根上下文不登记身份：`inject(root)` → `None`，指向根的 link 被丢弃。
    fn span_context_id(&self) -> SpanLinkContext {
        sentinel_context_id()
    }
}

// ---------------------------------------------------------------------------
// span 对象
// ---------------------------------------------------------------------------

/// 记录型 span：持有 recorder 状态与自身在 `state.spans` 里的下标。
///
/// 不变量：`state.spans` 只追加不删除，所以 `index` 永久指向创建它的那一行；span 结算后
/// 句柄仍可用（迟到的 `add_event` 等会被 `settled` 检查静默忽略）。
/// `carrier` 是创建时确定的不可变快照，用于 `span_context_id` 与子 span 的父级。
struct InMemorySpan {
    inner: Arc<Inner>,
    index: usize,
    carrier: TelemetryCarrier,
}

impl InMemorySpan {
    /// 在持锁状态下修改本 span 的记录行。
    ///
    /// 锁只覆盖 `f` 的执行；`f` 内不得调用 `inner.state()`（不可重入）或执行 `.await`。
    /// 返回值会先于 guard 释放计算出来，因此调用方拿到锁外的结果。
    fn with_record<R>(&self, f: impl FnOnce(&mut MutableRecordedSpan) -> R) -> R {
        let mut state = self.inner.state();
        f(&mut state.spans[self.index])
    }
}

// span 作为上下文：它的能力委托给 recorder，启动子 span 时会带上父级信息。
// `span_index` 让 `start_in_memory_span` 能在锁内判断父 span 是否已结算。
impl TelemetryContext for InMemorySpan {
    fn metrics(&self) -> Option<Arc<dyn TelemetryMetrics>> {
        Some(self.inner.recorder_metrics())
    }

    fn propagation(&self) -> Option<Arc<dyn TelemetryPropagation>> {
        Some(self.inner.recorder_propagation())
    }

    /// 子 span 继承父 carrier 的 trace：`create_span` 从 `parent.context` 读取 trace 键，
    /// 因此同一 trace 下的所有后代共享 trace id。`local_id` 是父 span 的数字 id，
    /// 用于 `RecordedTelemetrySpan.parentId`。
    fn start_span<'a>(
        &'a self,
        options: SpanOptions,
        callback: SpanCallback<'a>,
    ) -> BoxFuture<'a, Result<ErasedValue, TelemetryError>> {
        let parent = InMemoryParent {
            context: self.carrier.clone(),
            local_id: Some(self.with_record(|span| span.id)),
            span_index: Some(self.index),
        };
        start_in_memory_span(Arc::clone(&self.inner), Some(parent), options, callback)
    }

    /// `span_id` 是 `scope:span` 文本（store 的 id 格式），`parent_id` 是父 carrier 的文本，
    /// 而不是父 span 的数字 id —— `RecordedTelemetrySpan` 与 `StoredSpan` 的父 id 格式不同。
    fn span_context_id(&self) -> SpanLinkContext {
        let (parent, id) =
            self.with_record(|span| (span.parent_context.clone(), carrier_id(&span.context)));
        SpanLinkContext {
            trace_id: carrier_trace(&self.carrier).unwrap_or("").to_string(),
            span_id: id,
            parent_id: parent.as_ref().map(carrier_id),
        }
    }
}

// 四个写入方法都以 `settled` 为门：结算后的迟到调用是 no-op（不报错），因为句柄可能在
// 回调结束后仍被外部持有。所有写入都先复制输入，再在锁内合并，避免调用方与内部状态共享。
impl TelemetrySpan for InMemorySpan {
    /// link 指向的上下文必须已在进程级注册表里；否则静默丢弃（对应 TS `copyLink` 的
    /// 丢弃语义）。自链接、已结算 span 的链接都可以成功解析并记录。
    fn add_link(&self, link: SpanLink) {
        let Some(copy) = copy_link(&link) else {
            return;
        };
        self.with_record(|span| {
            if span.settled {
                return;
            }
            span.links.push(copy);
        });
    }

    /// 事件时间在加锁前采样（时钟调用发生在锁外），再减去 span 创建时的单调钟读数，
    /// 得到相对时间。如果同一 span 被多线程并发 `add_event`，事件的入表顺序由锁竞争决定，
    /// 而时间戳顺序由采样顺序决定，两者可能不一致（TS 单线程模型下不存在）。
    fn add_event(&self, name: &str, attributes: Option<SpanAttributes>) {
        let time_ms = self.inner.clock.now_monotonic_ms();
        let attributes = copy_attributes(attributes.as_ref());
        self.with_record(|span| {
            if span.settled {
                return;
            }
            span.events.push(MutableEvent {
                time_ms: time_ms - span.clock,
                name: name.to_string(),
                attributes,
            });
        });
    }

    /// 浅合并：新值覆盖同名旧值，新键追加；结果再按 JS 对象的枚举顺序（整数样键升序在前）
    /// 重排，与 `{ ...old, ...new }` 的可观察键序一致。
    fn set_attributes(&self, attributes: SpanAttributes) {
        let incoming = copy_attributes(Some(&attributes));
        self.with_record(|span| {
            if span.settled {
                return;
            }
            let mut merged = span.attributes.clone();
            for (key, value) in incoming {
                merged.insert(key, value);
            }
            // `{...old, ...new}` 生成的对象按 JS 键序枚举（整数键前置）。
            span.attributes = jsval::sort_js_keys(&merged);
        });
    }

    /// 标记 `explicit_status`：结算时若回调返回 `Err`，显式状态优先，不会被自动错误覆盖。
    fn set_status(&self, status: SpanStatus) {
        self.with_record(|span| {
            if span.settled {
                return;
            }
            span.status = copy_status(&status);
            span.explicit_status = true;
        });
    }
}

/// `extract` 得到的父上下文：不记录自身，只作为子 span 的父级。
///
/// 它是一个“合成上下文”：`carrier` 来自输入而不是某个已记录的 span；`local_id` 只在
/// carrier 的 scope 等于本 recorder 时才存在（用 JS `Number()` 解析 span 文本）。因此用它
/// 启动的子 span 会继承 trace 与父 carrier，但 `RecordedTelemetrySpan.parentId` 只有在
/// 同 scope 时才可能有数字值。
///
/// `extract_context` 已把该 carrier 登记进注册表，所以 `inject(extracted)` 能拿回 carrier；
/// 但 `span_index: None` 表示不追踪“父 span 是否结算”，没有惰性降级路径。
struct ExtractedParent {
    inner: Arc<Inner>,
    carrier: TelemetryCarrier,
    local_id: Option<f64>,
}

// 合成上下文同样把能力委托给 recorder；启动子 span 时只带 carrier/local_id，不带下标。
impl TelemetryContext for ExtractedParent {
    fn metrics(&self) -> Option<Arc<dyn TelemetryMetrics>> {
        Some(self.inner.recorder_metrics())
    }

    fn propagation(&self) -> Option<Arc<dyn TelemetryPropagation>> {
        Some(self.inner.recorder_propagation())
    }

    fn start_span<'a>(
        &'a self,
        options: SpanOptions,
        callback: SpanCallback<'a>,
    ) -> BoxFuture<'a, Result<ErasedValue, TelemetryError>> {
        let parent = InMemoryParent {
            context: self.carrier.clone(),
            local_id: self.local_id,
            span_index: None,
        };
        start_in_memory_span(Arc::clone(&self.inner), Some(parent), options, callback)
    }

    /// 没有 `parent_id`：合成上下文不记录自己的父链，`span_context_id` 只从 `carrier` 的
    /// trace/span 文本派生。
    fn span_context_id(&self) -> SpanLinkContext {
        context_id_from_carrier(&self.carrier, None)
    }
}

/// 惰性 span：父级已结算时使用；不记录、不可 inject、仍暴露真实能力。
///
/// 两个来源：
/// - 父 span 已结算：`inner = Some(..)`，metrics/propagation 仍返回 recorder 的真实能力
///   （`memory_edges` fixture 断言它们与 recorder 的句柄是同一 `Arc`）；
/// - recorder 已销毁（仅剩 propagation/metrics 句柄）：`inner = None`，能力降级为 no-op。
///
/// 无论如何它都不登记身份、不写入 `state.spans`；`start_span` 的子 span 继续是惰性 span，
/// 因此一整棵子树都不会产生记录。回调本身仍会执行，返回值/错误原样传出。
struct InertSpan {
    inner: Option<Arc<Inner>>,
}

impl InertSpan {
    fn metrics_handle(&self) -> Option<Arc<dyn TelemetryMetrics>> {
        self.inner.as_ref().map(|inner| inner.recorder_metrics())
    }

    fn propagation_handle(&self) -> Option<Arc<dyn TelemetryPropagation>> {
        self.inner
            .as_ref()
            .map(|inner| inner.recorder_propagation())
    }
}

// 惰性 span 的 context 能力：能力句柄尽可能转发给 recorder，但身份永远是哨兵。
impl TelemetryContext for InertSpan {
    fn metrics(&self) -> Option<Arc<dyn TelemetryMetrics>> {
        self.metrics_handle()
    }

    fn propagation(&self) -> Option<Arc<dyn TelemetryPropagation>> {
        self.propagation_handle()
    }

    fn start_span<'a>(
        &'a self,
        options: SpanOptions,
        callback: SpanCallback<'a>,
    ) -> BoxFuture<'a, Result<ErasedValue, TelemetryError>> {
        start_inert_span(self.inner.clone(), options, callback)
    }

    /// 哨兵：惰性 span 不会被 `inject`（它从未登记）。
    fn span_context_id(&self) -> SpanLinkContext {
        sentinel_context_id()
    }
}

// 记录类调用全部丢弃：惰性 span 的定位是“行为上的 no-op 包装”，不是错误。
impl TelemetrySpan for InertSpan {
    fn add_link(&self, _link: SpanLink) {}
    fn add_event(&self, _name: &str, _attributes: Option<SpanAttributes>) {}
    fn set_attributes(&self, _attributes: SpanAttributes) {}
    fn set_status(&self, _status: SpanStatus) {}
}

// ---------------------------------------------------------------------------
// 核心算法
// ---------------------------------------------------------------------------

/// 启动 span 时携带的父级信息。
///
/// - `context`：父 carrier，子 span 从中继承 trace；
/// - `local_id`：父 span 的数字 id，写入 `RecordedTelemetrySpan.parentId`；
/// - `span_index`：父 span 在 `state.spans` 的下标，用于启动时检查父级是否已结算。
///   `extract` 得到的合成父级没有下标（`None`），因此不会触发惰性降级。
struct InMemoryParent {
    context: TelemetryCarrier,
    local_id: Option<f64>,
    span_index: Option<usize>,
}

/// 构造 span 的 carrier：三个固定键，span id 用 JS `String(number)` 的文本。
///
/// 用 `js_number_text` 而不是 Rust 的 `Display`：TS 侧模板字符串走 `String(number)`，
/// 例如整数不带 `.0`。这些文本随后会成为注册表键与 store 的 `StoredSpan.id`。
fn new_carrier(scope: &str, id: f64, trace: String) -> TelemetryCarrier {
    let mut carrier = TelemetryCarrier::new();
    carrier.insert("kiana-memory-scope".to_string(), scope.to_string());
    carrier.insert("kiana-memory-span".to_string(), jsval::js_number_text(id));
    carrier.insert("kiana-memory-trace".to_string(), trace);
    carrier
}

/// 在 `state.spans` 里追加一条记录并登记 carrier，返回 `(下标, carrier)`。
///
/// 执行阶段：
/// 1. 在加锁之前读取墙钟与单调钟（`Clock` 的调用位于锁外）；
/// 2. 解析并复制 link，未注册的上下文被静默丢弃；
/// 3. 加锁分配数字 id；trace 优先继承父 carrier 的 `kiana-memory-trace`，没有父级时用
///    `scope:id` 开一条新 trace；
/// 4. 写入记录行后释放 state 锁，再登记进程级注册表。
///
/// 两把全局锁（state 与 registry）从不嵌套持有，因此不存在二者之间的死锁环。返回的下标
/// 依赖 `state.spans` 只追加的不变量。
fn create_span(
    inner: &Arc<Inner>,
    parent: Option<&InMemoryParent>,
    options: SpanOptions,
) -> (usize, TelemetryCarrier) {
    let started_ms = inner.clock.now_ms();
    let clock = inner.clock.now_monotonic_ms();
    let mut links = Vec::new();
    for link in options.links.iter().flatten() {
        if let Some(copy) = copy_link(link) {
            links.push(copy);
        }
    }
    let mut state = inner.state();
    let id = state.next_span_id;
    state.next_span_id += 1.0;
    let trace = match parent.and_then(|p| carrier_trace(&p.context)) {
        Some(trace) => trace.to_string(),
        None => format!("{}:{}", inner.scope, jsval::js_number_text(id)),
    };
    let context = new_carrier(&inner.scope, id, trace);
    let span = MutableRecordedSpan {
        started_ms,
        clock,
        ended_ms: None,
        id,
        parent_id: parent.and_then(|p| p.local_id),
        context: context.clone(),
        parent_context: parent.map(|p| p.context.clone()),
        links,
        name: options.name,
        attributes: copy_attributes(options.attributes.as_ref()),
        events: Vec::new(),
        status: SpanStatus::Ok,
        explicit_status: false,
        settled: false,
        end_sequence: None,
    };
    state.spans.push(span);
    let index = state.spans.len() - 1;
    drop(state);
    register_context(&context);
    (index, context)
}

/// 结算 span：标记 `settled`、写入 `endedMs` 与结算序号。
///
/// 时长 = `now_monotonic - span.clock`，再加上创建时的墙钟 `started_ms`；`endedMs` 仍是墙钟
/// 时间，但时长本身不受墙钟调整影响。调用方保证每个 span 只结算一次；这里没有幂等检查，
/// 重复调用会重新分配序号并覆盖结束时间。
///
/// `next_end_sequence` 与 `next_span_id` 都只在锁内自增，因此序号严格递增且无重复。
fn settle_span(inner: &Inner, index: usize) {
    let monotonic_now = inner.clock.now_monotonic_ms();
    let mut state = inner.state();
    let sequence = state.next_end_sequence;
    state.next_end_sequence += 1.0;
    let span = &mut state.spans[index];
    span.settled = true;
    span.ended_ms = Some(span.started_ms + monotonic_now - span.clock);
    span.end_sequence = Some(sequence);
}

// ---------------------------------------------------------------------------
// startInMemorySpan / startInertSpan
// ---------------------------------------------------------------------------

/// `startInMemorySpan`：与 TS 的 `try { return await callback(span) } catch … finally …` 同构。
///
/// 结算（TS 的 `finally`）只在回调 future **就绪**之后执行：调用方丢弃本 future 时控制流什么
/// 都不会走，span 保持 `settled = false`。这与 TS 里「被丢弃的 promise，其 inner work 永不完成」
/// 的可观察结果一致 —— `storedSpans()` 的 `settled` 过滤把它排除在 `findTraces` / `getTrace`
/// 之外，只有 `getSpans()` 快照还能看到它（见模块文档的「取消」一节）。
fn start_in_memory_span<'a>(
    inner: Arc<Inner>,
    parent: Option<InMemoryParent>,
    options: SpanOptions,
    callback: SpanCallback<'a>,
) -> BoxFuture<'a, Result<ErasedValue, TelemetryError>> {
    Box::pin(async move {
        // 阶段 0：父 span 已结算 → 惰性 span（不记录，但仍执行回调）。查 settled 的 guard
        // 在 match 语句结束时就释放，不会跨越后面的 await。
        let parent_settled = match parent.as_ref().and_then(|p| p.span_index) {
            Some(index) => inner.state().spans[index].settled,
            None => false,
        };
        if parent_settled {
            return start_inert_span(Some(inner), options, callback).await;
        }

        // 阶段 1：创建记录行并登记 carrier。此后 span 已经能被 inject / link 解析。
        let (index, carrier) = create_span(&inner, parent.as_ref(), options);
        let span: Arc<dyn TelemetrySpan> = Arc::new(InMemorySpan {
            inner: Arc::clone(&inner),
            index,
            carrier,
        });

        // 阶段 2：在锁外执行回调。调用方丢弃本 future 时这一行之后的结算代码不会运行，
        // span 永久保持 settled = false（模块文档的「取消」一节解释了为何不补 Drop 结算）。
        let result = callback(span).await;
        match result {
            // 阶段 3a：正常完成 —— 对应 TS finally 的结算。
            Ok(value) => {
                settle_span(&inner, index);
                Ok(value)
            }
            // 阶段 3b：回调报错 —— 先按“是否显式设过状态”决定是否写入错误状态，再结算，
            // 最后把错误原样返回。读 explicit_status 与写 status 分两次加锁，不是原子操作；
            // 在 TS 的单线程模型里不可观察，只有跨线程同时改同一个 span 时才有窗口。
            Err(error) => {
                let explicit = inner.state().spans[index].explicit_status;
                if !explicit {
                    // TS: `error instanceof Error ? { status: "error", error: { name: error.name,
                    // message: error.message } } : { status: "error" }`。
                    // Rust 的 rejection 通道是 `TelemetryError`：`name: Some(..)` 表示 Error 实例
                    // （复制其 name/message），`name: None` 表示非 Error 的 rejection 值（裸 error）。
                    let status = match error.name() {
                        Some(name) => SpanStatus::Error {
                            error: Some(StatusError {
                                name: name.to_string(),
                                message: error.message.clone(),
                            }),
                        },
                        None => SpanStatus::Error { error: None },
                    };
                    inner.state().spans[index].status = status;
                }
                settle_span(&inner, index);
                Err(error)
            }
        }
    })
}

/// 执行一次“惰性” span：构造 [`InertSpan`] 并把它交给回调。
///
/// 不分配 id、不写状态、不登记注册表，也不做任何结算；`_options` 被完全忽略。
/// 与 TS 的差别只在身份：TS 每次都创建一个新的 no-op 包装对象，Rust 同样每次新建
/// `InertSpan`，而不是复用 `noop_telemetry_context` 单例。
fn start_inert_span<'a>(
    inner: Option<Arc<Inner>>,
    _options: SpanOptions,
    callback: SpanCallback<'a>,
) -> BoxFuture<'a, Result<ErasedValue, TelemetryError>> {
    Box::pin(async move {
        let span: Arc<dyn TelemetrySpan> = Arc::new(InertSpan { inner });
        callback(span).await
    })
}

// ---------------------------------------------------------------------------
// extract
// ---------------------------------------------------------------------------

/// `extract` 的核心：校验 carrier 形状、登记并构造合成父上下文。
///
/// 判定只有“三个键都存在”这一条，值本身不做数字/格式校验 —— 它们被视为不透明字符串，
/// 与 [`TelemetryPropagation::extract`] 的契约一致。同 scope 的 span 文本用
/// `jsval::js_number_from_str`（JS `Number()` 语义）解析；跨 scope 一律 `None`，因为那个
/// 数字只在本 recorder 的 id 空间里有意义。
///
/// 副作用：成功识别时会把 carrier 写进进程级注册表，因此随后的 `inject` / link 解析能命中。
/// 失败（`None` 或任一键缺失）返回 recorder 根，且不产生注册表写入。
fn extract_context(
    inner: &Arc<Inner>,
    carrier: Option<&TelemetryCarrier>,
) -> Arc<dyn TelemetryContext> {
    let scope = carrier.and_then(|c| c.get("kiana-memory-scope"));
    let span = carrier.and_then(|c| c.get("kiana-memory-span"));
    let trace = carrier.and_then(|c| c.get("kiana-memory-trace"));
    let (Some(scope), Some(span), Some(trace)) = (scope, span, trace) else {
        return inner.recorder_context();
    };
    let mut context = TelemetryCarrier::new();
    context.insert("kiana-memory-scope".to_string(), scope.clone());
    context.insert("kiana-memory-span".to_string(), span.clone());
    context.insert("kiana-memory-trace".to_string(), trace.clone());
    let local_id = if scope == &inner.scope {
        Some(jsval::js_number_from_str(span))
    } else {
        None
    };
    register_context(&context);
    Arc::new(ExtractedParent {
        inner: Arc::clone(inner),
        carrier: context,
        local_id,
    })
}

// ---------------------------------------------------------------------------
// 指标工厂
// ---------------------------------------------------------------------------

/// 一个仪表的内部记录器：创建时的元数据 + 共享状态。
///
/// 不要与 `RecorderIdentity` 混淆：`Recorder` 是“某个 counter/histogram 的元数据”，
/// `RecorderIdentity` 是 TS 里那个同时充当 context / propagation / metrics 的 `this`。
/// 两者没有类型关系；仪表通过 `Arc<Inner>` 强引用状态，因此仪表活得比 context 句柄久时
/// 仍能继续记录。
struct Recorder {
    inner: Arc<Inner>,
    kind: MetricKind,
    name: String,
    description: Option<String>,
    unit: Option<String>,
    boundaries: Option<Vec<f64>>,
}

impl Recorder {
    /// 追加一条原始测量（不聚合）。
    ///
    /// 步骤：
    /// 1. 锁外解析 exemplar 上下文：`span_context_id` 必须能在进程级注册表里查到，否则记
    ///    `None`（根上下文、惰性 span、no-op 上下文都走这条）；
    /// 2. 用墙钟计算 10 秒桶起点 `floor(now_ms / 10_000) * 10_000`；
    /// 3. 复制仪表元数据与属性快照后追加到测量表；顺序是入锁顺序（单线程下即调用顺序）。
    ///
    /// 不校验 `value`（NaN/Infinity 原样保存），也不合并相同 name/attributes 的行。
    fn record(
        &self,
        value: f64,
        attributes: Option<&SpanAttributes>,
        context: Option<&dyn TelemetryContext>,
    ) {
        let carrier = context.and_then(|context| resolve_context(&context.span_context_id()));
        let interval_ms = (self.inner.clock.now_ms() / 10_000.0).floor() * 10_000.0;
        let measurement = RecordedTelemetryMeasurement {
            kind: self.kind,
            name: self.name.clone(),
            description: self.description.clone(),
            unit: self.unit.clone(),
            boundaries: self.boundaries.clone(),
            value,
            attributes: copy_attributes(attributes),
            context: carrier,
        };
        self.inner.state().measurements.push(MeasurementRow {
            interval_ms,
            measurement,
        });
    }
}

/// counter 仪表：每次 `add` 都追加一条独立测量（不累加），公开签名没有 exemplar 上下文。
struct InMemoryCounter {
    recorder: Recorder,
}

impl TelemetryCounter for InMemoryCounter {
    fn add(&self, value: f64, attributes: Option<SpanAttributes>) {
        self.recorder.record(value, attributes.as_ref(), None);
    }
}

/// upDownCounter 仪表：与 counter 唯一的区别是记录的 `kind`，数值可为负。
struct InMemoryUpDownCounter {
    recorder: Recorder,
}

impl TelemetryUpDownCounter for InMemoryUpDownCounter {
    fn add(&self, value: f64, attributes: Option<SpanAttributes>) {
        self.recorder.record(value, attributes.as_ref(), None);
    }
}

/// histogram 仪表：唯一接受 exemplar 上下文的仪表；`boundaries` 只作为元数据保存，
/// 不做分桶统计。
struct InMemoryHistogram {
    recorder: Recorder,
}

impl TelemetryHistogram for InMemoryHistogram {
    fn record(
        &self,
        value: f64,
        attributes: Option<SpanAttributes>,
        context: Option<&dyn TelemetryContext>,
    ) {
        self.recorder.record(value, attributes.as_ref(), context);
    }
}

// 下面三个空闲仪表在 `Weak<Inner>` 升级失败时返回。它们不持有任何状态，调用即丢弃。
// 为什么需要：propagation/metrics 的 `Arc` 可能比所有 context 句柄活得更久；TS 里
// propagation/metrics 与上下文同寿，这是 Rust 侧的文档化偏差（见 `RecorderIdentity`）。

/// 空闲 counter：当 `Weak<Inner>` 已失效时使用。
fn noop_counter() -> Arc<dyn TelemetryCounter> {
    struct Noop;
    impl TelemetryCounter for Noop {
        fn add(&self, _value: f64, _attributes: Option<SpanAttributes>) {}
    }
    Arc::new(Noop)
}

/// 空闲 upDownCounter：语义同上，调用即丢弃。
fn noop_up_down_counter() -> Arc<dyn TelemetryUpDownCounter> {
    struct Noop;
    impl TelemetryUpDownCounter for Noop {
        fn add(&self, _value: f64, _attributes: Option<SpanAttributes>) {}
    }
    Arc::new(Noop)
}

/// 空闲 histogram：语义同上，调用即丢弃（包括 exemplar 上下文）。
fn noop_histogram() -> Arc<dyn TelemetryHistogram> {
    struct Noop;
    impl TelemetryHistogram for Noop {
        fn record(
            &self,
            _value: f64,
            _attributes: Option<SpanAttributes>,
            _context: Option<&dyn TelemetryContext>,
        ) {
        }
    }
    Arc::new(Noop)
}

/// 组装仪表内部状态；`Arc::clone(inner)` 让仪表独立于任何 context 句柄存活。
fn make_recorder(
    inner: &Arc<Inner>,
    kind: MetricKind,
    name: &str,
    description: Option<String>,
    unit: Option<String>,
    boundaries: Option<Vec<f64>>,
) -> Recorder {
    Recorder {
        inner: Arc::clone(inner),
        kind,
        name: name.to_string(),
        description,
        unit,
        boundaries,
    }
}

/// counter 工厂：只读取 `description` / `unit`，`boundaries` 恒为 `None`。
///
/// TS 的 `createMeasurementRecorder` 三个工厂共用同一个记录函数；Rust 侧按 trait 拆成
/// 三个结构体只是为了让 `add` 与 `record` 的签名差异在类型上成立。
fn create_counter(
    inner: &Arc<Inner>,
    name: &str,
    options: Option<InstrumentOptions>,
) -> Arc<dyn TelemetryCounter> {
    let (description, unit) = match options {
        Some(InstrumentOptions { description, unit }) => (description, unit),
        None => (None, None),
    };
    Arc::new(InMemoryCounter {
        recorder: make_recorder(inner, MetricKind::Counter, name, description, unit, None),
    })
}

/// upDownCounter 工厂：与 counter 相同，只把 `kind` 换成 `UpDownCounter`。
fn create_up_down_counter(
    inner: &Arc<Inner>,
    name: &str,
    options: Option<InstrumentOptions>,
) -> Arc<dyn TelemetryUpDownCounter> {
    let (description, unit) = match options {
        Some(InstrumentOptions { description, unit }) => (description, unit),
        None => (None, None),
    };
    Arc::new(InMemoryUpDownCounter {
        recorder: make_recorder(
            inner,
            MetricKind::UpDownCounter,
            name,
            description,
            unit,
            None,
        ),
    })
}

/// histogram 工厂：额外展开 `HistogramOptions.instrument` 里的 description/unit，
/// 并保留 boundaries 作为元数据。
fn create_histogram(
    inner: &Arc<Inner>,
    name: &str,
    options: Option<HistogramOptions>,
) -> Arc<dyn TelemetryHistogram> {
    let (description, unit, boundaries) = match options {
        Some(HistogramOptions {
            instrument,
            boundaries,
        }) => (instrument.description, instrument.unit, boundaries),
        None => (None, None, None),
    };
    Arc::new(InMemoryHistogram {
        recorder: make_recorder(
            inner,
            MetricKind::Histogram,
            name,
            description,
            unit,
            boundaries,
        ),
    })
}

// ---------------------------------------------------------------------------
// recorder 对象（TS 的 `this`：同一对象实现 context / propagation / metrics）
// ---------------------------------------------------------------------------

/// TS 的 `InMemoryTelemetryContext` 对象本身在 Rust 侧的共享核心。
///
/// TS 里 `readonly metrics = this; readonly propagation = this`，而 `extract(carrier)` 在缺失/
/// 未知 carrier 时 `return this` —— 因此 recorder、它的 metrics 能力、propagation 能力、以及
/// `extract` 的根上下文是**同一个对象**（`test/propagation.test.ts:120` 断言
/// `expect(telemetry.extract(carrier)).toBe(telemetry)`）。
///
/// Rust 的 [`InMemoryTelemetryContext`] 是可复制的句柄，无法同时是 trait object；这里让
/// [`RecorderIdentity`] 充当那个 `this`：它同时实现 [`TelemetryContext`]、
/// [`TelemetryPropagation`] 与 [`TelemetryMetrics`]，`Inner` 持有唯一一个
/// `Arc<RecorderIdentity>`，所有上下文返回同一个 `Arc`（`Arc::ptr_eq` 成立）。
/// `RecorderIdentity` 只弱引用 `Inner`，因此 `Arc::strong_count` 不会成环。
struct RecorderIdentity {
    /// 只弱引用共享状态：`Inner` 是它的所有者；`upgrade` 失败表示所有 context 句柄都已释放。
    inner: Weak<Inner>,
}

// 身份对象作为 context：每个能力调用都先 upgrade。Inner 已销毁时 metrics/propagation
// 返回 None，start_span 退化为惰性 span —— 这与 TS 中对象不可能“半存活”的差异被限制在
// 这一层，调用方看到的 trait 形状保持不变。
impl TelemetryContext for RecorderIdentity {
    fn metrics(&self) -> Option<Arc<dyn TelemetryMetrics>> {
        self.inner.upgrade().map(|inner| inner.recorder_metrics())
    }

    fn propagation(&self) -> Option<Arc<dyn TelemetryPropagation>> {
        self.inner
            .upgrade()
            .map(|inner| inner.recorder_propagation())
    }

    /// 根上下文启动顶层 span（`parent = None`）；Inner 已销毁时只能执行惰性回调。
    fn start_span<'a>(
        &'a self,
        options: SpanOptions,
        callback: SpanCallback<'a>,
    ) -> BoxFuture<'a, Result<ErasedValue, TelemetryError>> {
        match self.inner.upgrade() {
            Some(inner) => start_in_memory_span(inner, None, options, callback),
            None => start_inert_span(None, options, callback),
        }
    }

    /// 根上下文不登记身份：`inject(root)` → `None`，指向根的 link 被丢弃。
    fn span_context_id(&self) -> SpanLinkContext {
        sentinel_context_id()
    }
}

// propagation 能力：inject 与 extract 共用全局注册表。
impl TelemetryPropagation for RecorderIdentity {
    fn inject(&self, context: &dyn TelemetryContext) -> Option<TelemetryCarrier> {
        resolve_context(&context.span_context_id())
    }

    /// `extract` 的 TS 语义是“未识别时 `return this`”：`extract_context` 在 carrier 缺失
    /// 或形状不符时返回同一 recorder 身份，识别成功时返回新的合成父上下文。Inner 已销毁时
    /// 退化为全局 no-op 上下文，因为此时无法再创建有意义的 recorder 根。
    fn extract(&self, carrier: Option<&TelemetryCarrier>) -> Arc<dyn TelemetryContext> {
        match self.inner.upgrade() {
            Some(inner) => extract_context(&inner, carrier),
            None => noop_telemetry_context(),
        }
    }
}

// metrics 能力：先 upgrade 再建仪表；失败时返回无状态 no-op 仪表（见上方 noop_* 注释）。
impl TelemetryMetrics for RecorderIdentity {
    fn counter(&self, name: &str, options: Option<InstrumentOptions>) -> Arc<dyn TelemetryCounter> {
        match self.inner.upgrade() {
            Some(inner) => create_counter(&inner, name, options),
            None => noop_counter(),
        }
    }

    fn up_down_counter(
        &self,
        name: &str,
        options: Option<InstrumentOptions>,
    ) -> Arc<dyn TelemetryUpDownCounter> {
        match self.inner.upgrade() {
            Some(inner) => create_up_down_counter(&inner, name, options),
            None => noop_up_down_counter(),
        }
    }

    fn histogram(
        &self,
        name: &str,
        options: Option<HistogramOptions>,
    ) -> Arc<dyn TelemetryHistogram> {
        match self.inner.upgrade() {
            Some(inner) => create_histogram(&inner, name, options),
            None => noop_histogram(),
        }
    }
}

// ---------------------------------------------------------------------------
// store：storedSpans / getTrace / findTraces / findMetrics
// ---------------------------------------------------------------------------

/// `storedSpans()`：把所有**已结算** span 投影成 store 视图，并按 `(startedMs, id)` 升序排序。
///
/// 与 `get_spans` 的差异：
/// - 未结算 span 被过滤掉（因此被丢弃 future 留下的 span 不可见）；
/// - `id` / `parentId` 用 `scope:span` 文本，而不是 `RecordedTelemetrySpan` 的数字 id；
/// - `ended_ms` 对已结算 span 必有值，`unwrap_or(started_ms)` 只是防御性兜底；
/// - link 的 `attributes` 恒为 `Some(..)`（无属性时是空 map）。
///
/// 排序比较器是 JS 表达式 `a.startedMs - b.startedMs || (a.id < b.id ? -1 : …)`，交给
/// `timsort::sort_by_js`：该表达式可能产生 NaN 或非全序，Rust 标准排序会 panic（见模块文档）。
///
/// 并发：`state` guard 从投影开始一直持有到函数返回（包括排序），期间其他线程的 span
/// 创建与写入会等待这把锁。
fn stored_spans_impl(inner: &Arc<Inner>) -> Vec<StoredSpan> {
    let state = inner.state();
    let mut spans: Vec<StoredSpan> = state
        .spans
        .iter()
        .filter(|span| span.settled)
        .map(|span| StoredSpan {
            id: carrier_id(&span.context),
            trace_id: carrier_trace(&span.context).unwrap_or("").to_string(),
            parent_id: span.parent_context.as_ref().map(carrier_id),
            name: span.name.clone(),
            started_ms: span.started_ms,
            ended_ms: span.ended_ms.unwrap_or(span.started_ms),
            attributes: copy_attributes(Some(&span.attributes)),
            events: span
                .events
                .iter()
                .map(|event| StoredSpanEvent {
                    name: event.name.clone(),
                    time_ms: event.time_ms,
                    attributes: copy_attributes(Some(&event.attributes)),
                })
                .collect(),
            links: span
                .links
                .iter()
                .map(|link| StoredSpanLink {
                    trace_id: carrier_trace(&link.context).unwrap_or("").to_string(),
                    span_id: carrier_id(&link.context),
                    attributes: Some(copy_attributes(Some(&link.attributes))),
                })
                .collect(),
            status: copy_status(&span.status),
        })
        .collect();
    // `sort((a, b) => a.startedMs - b.startedMs || (a.id < b.id ? -1 : a.id > b.id ? 1 : 0))`：
    // 整体是一个 JS 比较器（`||` 把 0/NaN 换成 id 比较），走 V8 TimSort 语义。
    crate::timsort::sort_by_js(&mut spans, compare_stored_spans);
    spans
}

/// `a.startedMs - b.startedMs || (a.id < b.id ? -1 : a.id > b.id ? 1 : 0)`。
///
/// 返回 `f64` 供 `timsort` 使用：`NaN`/`0` 都会在 TimSort 内部归一化为「相等」，
/// 但这里已经把 `0` 替换成 id 比较，所以只有 `startedMs` 差值本身是 `NaN` 时才出现 `NaN`。
fn compare_stored_spans(left: &StoredSpan, right: &StoredSpan) -> f64 {
    let diff = left.started_ms - right.started_ms;
    if diff.is_nan() || diff == 0.0 {
        // `a.id < b.id ? -1 : a.id > b.id ? 1 : 0`：JS 字符串按 UTF-16 code unit 比较。
        match jsval::js_utf16_cmp(&left.id, &right.id) {
            std::cmp::Ordering::Less => -1.0,
            std::cmp::Ordering::Equal => 0.0,
            std::cmp::Ordering::Greater => 1.0,
        }
    } else {
        diff
    }
}

/// `getTrace(traceId)`：复用 `storedSpans` 的投影与排序，再按 trace id 过滤。
///
/// 空结果返回 `None`（而不是空 trace）；`trace_id` 原样回填到结果，即使它是外部传入的
/// 任意字符串。未结算 span 不参与匹配。
fn get_trace_impl(inner: &Arc<Inner>, trace_id: &str) -> Option<StoredTrace> {
    let spans: Vec<StoredSpan> = stored_spans_impl(inner)
        .into_iter()
        .filter(|span| span.trace_id == trace_id)
        .collect();
    if spans.is_empty() {
        None
    } else {
        Some(StoredTrace {
            trace_id: trace_id.to_string(),
            spans,
        })
    }
}

/// `JSON.stringify([startedMs, traceId])`。
///
/// 走 `kiana_json` 的 JS 语义序列化：数字按 ECMAScript 的 `Number::toString` 输出；
/// `number_value` 把 NaN/±Infinity 映射为 JSON `null`（JS `JSON.stringify` 的行为）。
/// 因此非有限 `startedMs` 会编码成 `[null, traceId]`，续页时由严格相等自然排除。
fn encode_cursor(started_ms: f64, trace_id: &str) -> String {
    let value = Value::Array(vec![
        jsval::number_value(started_ms),
        Value::String(trace_id.to_string()),
    ]);
    kiana_json::js_object::stringify(&value)
}

/// `JSON.parse(query.cursor)`；`None` 表示「无 cursor」（JS falsy），`After` 是解析出的值。
///
/// 必须走 [`crate::jsjson::parse`] 而不是 `kiana_json::parse`：后者的浮点快速路径不能保证
/// 最短表示往返（续页判据 `first.startedMs === after[0]` 是严格相等，差 1 ULP 就会错页），
/// 且对 `1e400` 这类溢出字面量报错，而 `JSON.parse` 得到 `Infinity`。
///
/// 解析失败包装成 `name = "SyntaxError"` 的 [`TelemetryError`]：错误名是契约的一部分
/// （消息文本允许与 V8 不同，见模块文档）。解析成功但值在 JS 里为假（`null`、`false`、
/// `0`、`""` 等）时返回 `CursorAfter::None`，等价于没传 cursor。
fn decode_cursor(cursor: &str) -> Result<CursorAfter, TelemetryError> {
    let value = crate::jsjson::parse(cursor)
        .map_err(|error| TelemetryError::with_name("SyntaxError", error.message))?;
    if !value.is_truthy() {
        return Ok(CursorAfter::None);
    }
    Ok(CursorAfter::After(value))
}

/// `query.cursor === undefined ? undefined : JSON.parse(query.cursor)` 之后的两种状态
/// （Rust 侧用 `Option::is_none` 表示“未提供”，此时不调用 `decode_cursor`）。
///
/// - `None`：未提供 cursor，或 `decode_cursor` 解析出的值在 JS 里为假（`null`、`false`、
///   `0`、`""` 等），不按位置过滤。注意空串 cursor 仍会进入解析并因非法 JSON 报
///   `SyntaxError`，不会落到 `None`；
/// - `After(value)`：解析出的任意 JSON 值。它不要求是数组：`after[0]` / `after[1]` 后续会
///   按 JS 括号访问规则处理对象、字符串甚至标量。
enum CursorAfter {
    None,
    After(crate::jsjson::JsonValue),
}

/// 括号访问 `value[index]`（`after[0]` / `after[1]`）之后的 JS 原始值。
///
/// 字符串按 **UTF-16 code unit** 取下标（`"ab"[1] === "b"`，非 BMP 字符得到孤代理，因此内部用
/// `Vec<u16>` 而不是 `String`）；对象读 `"0"`/`"1"` 属性（`{"0":1000}[0] === 1000`）。
#[derive(Debug, Clone, PartialEq)]
enum CursorPrimitive {
    /// 属性不存在（`undefined`）。
    Undefined,
    Null,
    Bool(bool),
    Number(f64),
    /// 字符串的 UTF-16 code unit 序列（可能含孤代理）。
    Str(Vec<u16>),
}

impl CursorPrimitive {
    /// `ToNumber(ToPrimitive(value))`。`undefined`/非数值字符串 → `NaN`。
    fn as_number(&self) -> f64 {
        match self {
            CursorPrimitive::Undefined => f64::NAN,
            CursorPrimitive::Null => 0.0,
            CursorPrimitive::Bool(flag) => {
                if *flag {
                    1.0
                } else {
                    0.0
                }
            }
            CursorPrimitive::Number(number) => *number,
            CursorPrimitive::Str(units) => {
                jsval::js_number_from_str(&String::from_utf16_lossy(units))
            }
        }
    }
}

/// `value[index]` 的 JS 语义，见 [`CursorPrimitive`]。
fn js_bracket_get(value: &crate::jsjson::JsonValue, index: usize) -> CursorPrimitive {
    use crate::jsjson::JsonValue;
    match value {
        JsonValue::Array(items) => items
            .get(index)
            .map_or(CursorPrimitive::Undefined, primitive_of),
        JsonValue::Object(entries) => {
            let key: Vec<u16> = index.to_string().encode_utf16().collect();
            entries
                .iter()
                .find(|(name, _)| *name == key)
                .map_or(CursorPrimitive::Undefined, |(_, value)| primitive_of(value))
        }
        JsonValue::String(units) => units.get(index).map_or(CursorPrimitive::Undefined, |unit| {
            CursorPrimitive::Str(vec![*unit])
        }),
        // `(5)[0]` / `(true)[0]` / `(null)[0]` → undefined。
        _ => CursorPrimitive::Undefined,
    }
}

/// `ToPrimitive(value)`：数组是 `join(",")`，对象是 `"[object Object]"`。
fn primitive_of(value: &crate::jsjson::JsonValue) -> CursorPrimitive {
    use crate::jsjson::JsonValue;
    match value {
        JsonValue::Null => CursorPrimitive::Null,
        JsonValue::Bool(flag) => CursorPrimitive::Bool(*flag),
        JsonValue::Number(number) => CursorPrimitive::Number(*number),
        JsonValue::String(units) => CursorPrimitive::Str(units.clone()),
        JsonValue::Array(_) | JsonValue::Object(_) => {
            CursorPrimitive::Str(crate::jsjson::js_string(value))
        }
    }
}

/// `trace.traceId > after[1]`：抽象关系比较。
///
/// `ToPrimitive(after[1])` 是字符串时两侧都按字符串比较（UTF-16 code unit），否则两侧
/// `ToNumber` 后比较（`NaN` → false）。
fn js_trace_id_greater(trace_id: &str, right: &CursorPrimitive) -> bool {
    match right {
        CursorPrimitive::Str(units) => {
            trace_id.encode_utf16().cmp(units.iter().copied()) == std::cmp::Ordering::Greater
        }
        other => {
            let left_number = jsval::js_number_from_str(trace_id);
            left_number > other.as_number()
        }
    }
}

/// `!after || first.startedMs < after[0] || (first.startedMs === after[0] && traceId > after[1])`。
///
/// `after[0]` 在 `<` 里走抽象关系比较，在 `===` 里走严格相等（右侧是 `null` 时
/// `startedMs === null` 恒假，与 `ToNumber(null) === 0` 不同）。
fn passes_cursor(after: &CursorAfter, started_ms: f64, trace_id: &str) -> bool {
    let CursorAfter::After(value) = after else {
        return true;
    };
    let threshold = js_bracket_get(value, 0);
    let second = js_bracket_get(value, 1);

    if started_ms < threshold.as_number() {
        return true;
    }
    // `first.startedMs === after[0]`：严格相等，只有「数字且相等」能成立。
    let strict_equal = matches!(&threshold, CursorPrimitive::Number(number) if *number == started_ms && !started_ms.is_nan());
    if !strict_equal {
        return false;
    }
    js_trace_id_greater(trace_id, &second)
}

/// 查询属性必须**全部**命中同一份存储属性（AND 语义）。
///
/// 缺失键返回 `false`（JS 里 `undefined === expected` 为假）；值比较走 JS 严格相等，
/// 因此 `"1"` 不匹配 `1`、`true` 不匹配 `1`、数组不匹配标量。`find_traces_impl` 在 trace
/// 级别再套一层“任一 span 命中即可”（OR），两者组合出的语义是：存在某个 span，其属性
/// 同时满足查询里的所有等值条件。
fn attribute_matches(
    stored: &SpanAttributes,
    query: &IndexMap<String, crate::index::AttributeValue>,
) -> bool {
    query.iter().all(|(key, expected)| match stored.get(key) {
        Some(actual) => js_strict_equal_value(actual, expected),
        None => false,
    })
}

/// JS `===`：变体必须完全一致；数组是对象引用，永不等于查询里的标量。
///
/// 三个数组变体（`StringArray` / `NumberArray` / `BooleanArray`）一律返回 `false`：
/// 即便两侧内容相同，JS 里也是两个不同的数组对象，严格相等不成立。`Number(NaN)` 同理
/// 永不等于自身，所以 `a == b` 之外还显式排除了 `NaN`。
fn js_strict_equal_value(
    left: &crate::index::AttributeValue,
    right: &crate::index::AttributeValue,
) -> bool {
    use crate::index::AttributeValue as V;
    match (left, right) {
        (V::String(a), V::String(b)) => a == b,
        (V::Number(a), V::Number(b)) => a == b && !a.is_nan(),
        (V::Boolean(a), V::Boolean(b)) => a == b,
        _ => false,
    }
}

/// `findTraces` 的完整实现。
///
/// 管道：
/// 1. `query.unwrap_or_default()`；缺省 `limit` 为 20（`TraceQuery.limit` 是 `Option<u32>`）。
/// 2. 解析 cursor；非法 JSON 直接以 `SyntaxError` 返回。
/// 3. `storedSpans()` 已按 `(startedMs, id)` 升序，因此按首次出现顺序分组后，每组的
///    `spans[0]` 就是该 trace 最早的 span；组内顺序保持全局排序。
/// 4. 过滤：`name` / `startMin` / `startMax` / cursor 看最早 span；`errors` / `attributes`
///    只要组内任一 span 满足即可（`errors` 仅在 `Some(true)` 时启用）。
/// 5. 按最早 `startedMs` **降序**、`traceId` 升序（UTF-16）排序，取前 `limit` 个。
/// 6. 还有下一页时，用本页最后一条的 `(startedMs, traceId)` 生成 cursor。
///
/// 复杂度：`storedSpans` 的排序 O(n log n)（n = 已结算 span 数），分组与过滤 O(n)，
/// trace 排序 O(t log t)（t = 匹配 trace 数）。
///
/// 错误：cursor 非法 → `SyntaxError`；`limit == 0` 且有匹配 → 与 TS 相同的 `TypeError`
/// 文本（见函数内注释）。`limit == 0` 且无匹配时返回空页且不报错。
fn find_traces_impl(
    inner: &Arc<Inner>,
    query: Option<TraceQuery>,
) -> Result<TracePage, TelemetryError> {
    let query = query.unwrap_or_default();
    let limit = query.limit.map(f64::from).unwrap_or(20.0);
    let after = match query.cursor.as_deref() {
        Some(cursor) => decode_cursor(cursor)?,
        None => CursorAfter::None,
    };

    // 按 traceId 分组（首见顺序）。
    let mut order: Vec<String> = Vec::new();
    let mut grouped: HashMap<String, Vec<StoredSpan>> = HashMap::new();
    for span in stored_spans_impl(inner) {
        if !grouped.contains_key(&span.trace_id) {
            order.push(span.trace_id.clone());
        }
        grouped.entry(span.trace_id.clone()).or_default().push(span);
    }

    // 过滤阶段：`first` 是 trace 内最早的 span；`grouped.remove` 把 spans 移出 map，
    // 因此后面排序/matches 不再持有分组表。
    let mut matches: Vec<(String, Vec<StoredSpan>)> = order
        .into_iter()
        .filter_map(|trace_id| {
            let spans = grouped.remove(&trace_id)?;
            let first = spans.first()?;
            let name_ok = query.name.as_deref().is_none_or(|name| first.name == name);
            let start_min_ok = query.start_min.is_none_or(|min| first.started_ms >= min);
            let start_max_ok = query.start_max.is_none_or(|max| first.started_ms <= max);
            let cursor_ok = passes_cursor(&after, first.started_ms, &trace_id);
            let errors_ok = query.errors != Some(true)
                || spans
                    .iter()
                    .any(|span| matches!(span.status, SpanStatus::Error { .. }));
            let attributes_ok = query.attributes.as_ref().is_none_or(|attributes| {
                spans
                    .iter()
                    .any(|span| attribute_matches(&span.attributes, attributes))
            });
            if name_ok && start_min_ok && start_max_ok && cursor_ok && errors_ok && attributes_ok {
                Some((trace_id, spans))
            } else {
                None
            }
        })
        .collect();

    // `sort((a, b) => b.spans[0].startedMs - a.spans[0].startedMs || (a.traceId < b.traceId ? -1 : ...))`：
    // 同上，整体走 V8 TimSort（不一致/NaN 比较器不 panic）。
    crate::timsort::sort_by_js(&mut matches, |left, right| {
        let left_start = left.1.first().map(|s| s.started_ms).unwrap_or(f64::NAN);
        let right_start = right.1.first().map(|s| s.started_ms).unwrap_or(f64::NAN);
        let diff = right_start - left_start;
        if diff.is_nan() || diff == 0.0 {
            // `a.traceId < b.traceId ? -1 : a.traceId > b.traceId ? 1 : 0`（UTF-16 顺序）。
            match jsval::js_utf16_cmp(&left.0, &right.0) {
                std::cmp::Ordering::Less => -1.0,
                std::cmp::Ordering::Equal => 0.0,
                std::cmp::Ordering::Greater => 1.0,
            }
        } else {
            diff
        }
    });

    // 分页阶段：`total` 是过滤后的总数，`take` 是 `Array#slice(0, limit)` 的结束下标。
    // 只有确实还有下一页（total > limit）才生成 cursor；否则 None（最后一页）。
    let total = matches.len();
    let take = slice_end(total, limit);
    let page: Vec<(String, Vec<StoredSpan>)> = matches.into_iter().take(take).collect();

    let cursor = if (total as f64) > limit {
        // TS 在 `limit <= 0` 时 `slice` 得到空页，随后 `traces.at(-1).spans` 抛 TypeError。
        // 冻结的 `TraceQuery.limit` 是 `Option<u32>`，只有 `0` 可达；这里返回同样的错误文本
        // 而不是 panic（见模块文档与 spec §28.2）。
        let Some(last) = page.last() else {
            return Err(TelemetryError::with_name(
                "TypeError",
                "Cannot read properties of undefined (reading 'spans')",
            ));
        };
        let first = last
            .1
            .first()
            .expect("grouped traces always have a first span");
        Some(encode_cursor(first.started_ms, &last.0))
    } else {
        None
    };

    Ok(TracePage {
        traces: page
            .into_iter()
            .map(|(trace_id, spans)| StoredTrace { trace_id, spans })
            .collect(),
        cursor,
    })
}

/// JS `Array#slice(0, limit)` 的结束下标（ToIntegerOrInfinity 语义）。
///
/// `limit` 来自 `TraceQuery.limit: Option<u32>`，因此负数/小数/NaN/Infinity 不可达；
/// `0` 在有匹配时走到 `at(-1)` 的 `TypeError`（调用方处理）。`NaN` 与负数的分支是不可达的
/// 防御代码，保留它们让本函数单独看也与 JS 的 `ToIntegerOrInfinity` 语义一致。
fn slice_end(total: usize, limit: f64) -> usize {
    if limit.is_nan() {
        return 0;
    }
    if limit <= 0.0 {
        // `slice(0, 0)`/`slice(0, -k)`：负数会截掉尾部，Rust 侧不可达，按 0 处理。
        return 0;
    }
    if limit.is_infinite() {
        return total;
    }
    let limit = limit.trunc();
    if limit >= total as f64 {
        total
    } else {
        limit as usize
    }
}

/// `findMetrics`：按记录顺序过滤并投影原始测量；不做聚合、没有分页。
///
/// 过滤条件：`name` 精确相等；`startMin` / `startMax` 是对 10 秒桶起点的闭区间；
/// `attributes` 走 JS 严格相等（`attribute_matches`）。每一行都映射为
/// `count = 1`、`sum = min = max = value`，即“一条测量一行”（`tests/store.rs` 的
/// count-one 行测试锁定了这一形状）；`kind` 来自仪表创建时的种类。
fn find_metrics_impl(inner: &Arc<Inner>, query: MetricQuery) -> Vec<StoredMetric> {
    let state = inner.state();
    state
        .measurements
        .iter()
        .filter(|row| {
            let measurement = &row.measurement;
            measurement.name == query.name
                && query.start_min.is_none_or(|min| row.interval_ms >= min)
                && query.start_max.is_none_or(|max| row.interval_ms <= max)
                && query
                    .attributes
                    .as_ref()
                    .is_none_or(|attributes| attribute_matches(&measurement.attributes, attributes))
        })
        .map(|row| {
            let measurement = &row.measurement;
            StoredMetric {
                interval_ms: row.interval_ms,
                name: measurement.name.clone(),
                kind: Some(measurement.kind),
                unit: measurement.unit.clone(),
                attributes: copy_attributes(Some(&measurement.attributes)),
                count: 1.0,
                sum: measurement.value,
                min: measurement.value,
                max: measurement.value,
            }
        })
        .collect()
}

// ---------------------------------------------------------------------------
// trait 实现：propagation / metrics / store
// ---------------------------------------------------------------------------

// 下面三个 impl 把 trait 方法委托给同名的 inherent 方法。限定路径调用是有意的：
// 实现体只有一份（inherent 方法），trait impl 只负责适配 trait object / future 形状，
// 避免同名的两套逻辑各自演化。
impl TelemetryPropagation for InMemoryTelemetryContext {
    fn inject(&self, context: &dyn TelemetryContext) -> Option<TelemetryCarrier> {
        InMemoryTelemetryContext::inject(self, context)
    }

    fn extract(&self, carrier: Option<&TelemetryCarrier>) -> Arc<dyn TelemetryContext> {
        InMemoryTelemetryContext::extract(self, carrier)
    }
}

// metrics 能力：上下文本身可直接充当 TelemetryMetrics 工厂（TS 的 `readonly metrics = this`）。
impl TelemetryMetrics for InMemoryTelemetryContext {
    fn counter(&self, name: &str, options: Option<InstrumentOptions>) -> Arc<dyn TelemetryCounter> {
        InMemoryTelemetryContext::counter(self, name, options)
    }

    fn up_down_counter(
        &self,
        name: &str,
        options: Option<InstrumentOptions>,
    ) -> Arc<dyn TelemetryUpDownCounter> {
        InMemoryTelemetryContext::up_down_counter(self, name, options)
    }

    fn histogram(
        &self,
        name: &str,
        options: Option<HistogramOptions>,
    ) -> Arc<dyn TelemetryHistogram> {
        InMemoryTelemetryContext::histogram(self, name, options)
    }
}

// store 能力：三个方法把同步实现包进 `Box::pin(async ...)` 以符合 `TelemetryStore` 的
// future 形状。`get_trace` / `find_metrics` 目前没有失败路径，恒为 `Ok`；保留 `Result`
// 是为了与其它后端（SQLite/JSONL）共享同一 trait 签名。
impl TelemetryStore for InMemoryTelemetryContext {
    fn find_traces(
        &self,
        query: Option<TraceQuery>,
    ) -> BoxFuture<'_, Result<TracePage, TelemetryError>> {
        Box::pin(async move { find_traces_impl(&self.inner, query) })
    }

    fn get_trace(
        &self,
        trace_id: &str,
    ) -> BoxFuture<'_, Result<Option<StoredTrace>, TelemetryError>> {
        let trace_id = trace_id.to_string();
        Box::pin(async move { Ok(get_trace_impl(&self.inner, &trace_id)) })
    }

    fn find_metrics(
        &self,
        query: MetricQuery,
    ) -> BoxFuture<'_, Result<Vec<StoredMetric>, TelemetryError>> {
        Box::pin(async move { Ok(find_metrics_impl(&self.inner, query)) })
    }
}

// ---------------------------------------------------------------------------
// TypedSpanStarter 便捷入口（TS 的 createTypedSpanStarter 可直接传 InMemory 实例）
// ---------------------------------------------------------------------------

impl InMemoryTelemetryContext {
    /// 对应 `createTypedSpanStarter(context, schemas)` 的便捷构造。
    ///
    /// 内部 `self.clone()` 共享同一个 `Inner`，所以 starter 启动的 span 会记录到本实例，
    /// 可在 `get_spans` / `find_traces` 中看到；clone 只是让句柄能放进
    /// `Arc<dyn TelemetryContext>`。
    pub fn typed_span_starter(
        &self,
        schemas: &[crate::index::TelemetrySchemaDefinition],
    ) -> crate::index::TypedSpanStarter {
        let context: Arc<dyn TelemetryContext> = Arc::new(self.clone());
        crate::index::create_typed_span_starter(context, schemas)
    }
}

/// 便捷：`context.start_span(options, |span| async move { ... })` 的泛型版本。
///
/// 转发到 [`TelemetryContextExt::start_span_typed`]；自由函数形式让调用方无需把扩展 trait
/// 导入作用域。回调返回的 `T` 被装箱后 downcast 还原；**`T` 本身是 `Result::Err` 不会
/// 让 span 变成 error** —— 需要该语义请用 [`start_span_try`]。
pub async fn start_span_typed<T, F>(
    context: &dyn TelemetryContext,
    options: SpanOptions,
    callback: F,
) -> Result<T, TelemetryError>
where
    T: Send + 'static,
    F: FnOnce(Arc<dyn TelemetrySpan>) -> BoxFuture<'static, T> + Send + 'static,
{
    context.start_span_typed(options, callback).await
}

/// 便捷：`context.start_span_try(options, |span| async move { ... })` 的泛型版本。
///
/// 回调返回的 `Err` 按「回调抛出」处理（span 标记 error 并结算），见
/// [`TelemetryContextExt::start_span_try`]。
pub async fn start_span_try<T, F>(
    context: &dyn TelemetryContext,
    options: SpanOptions,
    callback: F,
) -> Result<T, TelemetryError>
where
    T: Send + 'static,
    F: FnOnce(Arc<dyn TelemetrySpan>) -> BoxFuture<'static, Result<T, TelemetryError>>
        + Send
        + 'static,
{
    context.start_span_try(options, callback).await
}

/// 便捷：在一个已存在的上下文里启动子 span（等价 `context.startSpan`）。
///
/// 纯转发，不额外建立父子关系：父级信息完全由 `context` 的具体 `start_span` 实现提供
/// （对 `InMemorySpan` 就是它保存的 carrier 与数字 id）。
pub fn start_child_span<'a>(
    context: &'a dyn TelemetryContext,
    options: SpanOptions,
    callback: SpanCallback<'a>,
) -> BoxFuture<'a, Result<ErasedValue, TelemetryError>> {
    context.start_span(options, callback)
}
