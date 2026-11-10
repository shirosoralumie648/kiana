//! noop.ts：`NOOP_TELEMETRY_CONTEXT` / `NOOP_TELEMETRY_METRICS` / `NOOP_TELEMETRY_PROPAGATION`。
//!
//! 全部是进程级单例：根与所有嵌套 span 共用**同一个** `Arc`（`Arc::ptr_eq` 成立，对应 TS 的
//! `child === parent`）；所有仪表也共用同一批冻结实例（`NOOP_TELEMETRY_METRICS.counter("a")`
//! 与 `.counter("b")` 返回同一个对象）。
//!
//! `startSpan` 在首次 poll 时同步进入回调（Rust 的 future 惰性，见 spec §24）；记录方法全部是
//! no-op；`inject` 恒为 `None`；`extract` 恒为 no-op 上下文。
//!
//! # 为什么用进程级单例
//!
//! TS 的 no-op 对象是模块级常量，“所有取用都得到同一个对象”（`===`）是其可观察语义。
//! Rust 用 `LazyLock<Arc<dyn ...>>` 复刻：每次 getter 返回同一分配的克隆，
//! `Arc::ptr_eq` 成立；`LazyLock` 只初始化一次。所有类型都没有内部可变状态，因此共享安全。
//!
//! # 行为边界
//!
//! - 所有记录方法立即返回，不保存任何数据；
//! - `NoopSpan` 的 `metrics()` / `propagation()` 返回 `Some`（no-op 实现本身），因此调用方
//!   在 no-op 上下文上做的“适配器缺失”兜底不会触发；
//! - `start_span` 返回的 future 是惰性的：回调在**首次 poll** 时执行，而不是调用 `start_span`
//!   时；回调拿到的仍是同一个 no-op 上下文，其返回值/错误原样传回；
//! - `inject` 恒 `None`、`extract` 恒返回哨兵上下文，两者都不依赖入参内容。
//!
//! TS: `packages/telemetry/src/noop.ts`

use std::sync::{Arc, LazyLock};

use crate::index::{
    BoxFuture, ErasedValue, SpanAttributes, SpanCallback, SpanLink, SpanLinkContext, SpanOptions,
    SpanStatus, TelemetryCarrier, TelemetryContext, TelemetryError, TelemetryPropagation,
    TelemetrySpan,
};
use crate::metrics::{
    HistogramOptions, InstrumentOptions, TelemetryCounter, TelemetryHistogram, TelemetryMetrics,
    TelemetryUpDownCounter,
};

/// no-op 上下文/span 的标识哨兵：两个字段都为空，注册表永不命中。
///
/// 与 `memory::resolve_context` 的约定一起保证 `inject(noop) === undefined`：该函数把
/// `(trace_id, span_id)` 都为空视为“未注册”并返回 `None`，所以 no-op 上下文永远解析不出
/// carrier。`parent_id` 同样保持 `None`，不指向任何真实 span。
pub const NOOP_TELEMETRY_CONTEXT_SENTINEL: SpanLinkContext = SpanLinkContext {
    trace_id: String::new(),
    span_id: String::new(),
    parent_id: None,
};

// 三个无状态仪器：方法忽略所有入参，因此可以共享同一实例而不会有任何可观察副作用。
struct NoopCounter;

impl TelemetryCounter for NoopCounter {
    fn add(&self, _value: f64, _attributes: Option<SpanAttributes>) {}
}

struct NoopUpDownCounter;

impl TelemetryUpDownCounter for NoopUpDownCounter {
    fn add(&self, _value: f64, _attributes: Option<SpanAttributes>) {}
}

struct NoopHistogram;

impl TelemetryHistogram for NoopHistogram {
    fn record(
        &self,
        _value: f64,
        _attributes: Option<SpanAttributes>,
        _context: Option<&dyn TelemetryContext>,
    ) {
    }
}

// 进程级唯一实例；getter 只做 `Arc::clone`，因此多次取用得到同一分配。
static NOOP_COUNTER: LazyLock<Arc<dyn TelemetryCounter>> = LazyLock::new(|| Arc::new(NoopCounter));
static NOOP_UP_DOWN_COUNTER: LazyLock<Arc<dyn TelemetryUpDownCounter>> =
    LazyLock::new(|| Arc::new(NoopUpDownCounter));
static NOOP_HISTOGRAM: LazyLock<Arc<dyn TelemetryHistogram>> =
    LazyLock::new(|| Arc::new(NoopHistogram));

// 工厂忽略 name/options：任意名字、任意元数据都返回上面三个共享实例。
struct NoopMetrics;

impl TelemetryMetrics for NoopMetrics {
    fn counter(
        &self,
        _name: &str,
        _options: Option<InstrumentOptions>,
    ) -> Arc<dyn TelemetryCounter> {
        Arc::clone(&NOOP_COUNTER)
    }

    fn up_down_counter(
        &self,
        _name: &str,
        _options: Option<InstrumentOptions>,
    ) -> Arc<dyn TelemetryUpDownCounter> {
        Arc::clone(&NOOP_UP_DOWN_COUNTER)
    }

    fn histogram(
        &self,
        _name: &str,
        _options: Option<HistogramOptions>,
    ) -> Arc<dyn TelemetryHistogram> {
        Arc::clone(&NOOP_HISTOGRAM)
    }
}

static NOOP_METRICS: LazyLock<Arc<dyn TelemetryMetrics>> = LazyLock::new(|| Arc::new(NoopMetrics));

// 传播实现不读取 carrier：注入永远没有值，提取永远是 no-op 上下文。
struct NoopPropagation;

impl TelemetryPropagation for NoopPropagation {
    fn inject(&self, _context: &dyn TelemetryContext) -> Option<TelemetryCarrier> {
        None
    }

    fn extract(&self, _carrier: Option<&TelemetryCarrier>) -> Arc<dyn TelemetryContext> {
        noop_telemetry_context()
    }
}

static NOOP_PROPAGATION: LazyLock<Arc<dyn TelemetryPropagation>> =
    LazyLock::new(|| Arc::new(NoopPropagation));

// 同时充当 `TelemetryContext` 与 `TelemetrySpan`：no-op 没有“子 span”概念，
// `start_span` 直接把同一个上下文交给回调，记录方法全部丢弃入参。
struct NoopSpan;

impl TelemetryContext for NoopSpan {
    fn metrics(&self) -> Option<Arc<dyn TelemetryMetrics>> {
        Some(noop_telemetry_metrics())
    }

    fn propagation(&self) -> Option<Arc<dyn TelemetryPropagation>> {
        Some(noop_telemetry_propagation())
    }

    fn start_span<'a>(
        &'a self,
        _options: SpanOptions,
        callback: SpanCallback<'a>,
    ) -> BoxFuture<'a, Result<ErasedValue, TelemetryError>> {
        // 惰性 future：首次 poll 才调用 callback；不做结算，也不创建新对象。
        Box::pin(async move { callback(noop_telemetry_context()).await })
    }

    fn span_context_id(&self) -> SpanLinkContext {
        NOOP_TELEMETRY_CONTEXT_SENTINEL
    }
}

impl TelemetrySpan for NoopSpan {
    fn add_link(&self, _link: SpanLink) {}
    fn add_event(&self, _name: &str, _attributes: Option<SpanAttributes>) {}
    fn set_attributes(&self, _attributes: SpanAttributes) {}
    fn set_status(&self, _status: SpanStatus) {}
}

static NOOP_CONTEXT: LazyLock<Arc<dyn TelemetrySpan>> = LazyLock::new(|| Arc::new(NoopSpan));

/// `NOOP_TELEMETRY_CONTEXT`：未提供适配器时使用的共享上下文（也是共享 span）。
///
/// 返回的是同一个 `Arc<dyn TelemetrySpan>`，因此 `Arc::ptr_eq` 可以断言身份相等；
/// 首次调用触发 `LazyLock` 初始化，之后只是引用计数加一。无参数、无错误、无副作用。
pub fn noop_telemetry_context() -> Arc<dyn TelemetrySpan> {
    Arc::clone(&NOOP_CONTEXT)
}

/// `NOOP_TELEMETRY_METRICS`：所有工厂都返回共享 no-op 仪器的 metrics 后端。
///
/// 任意名字/选项都被忽略；同一 getter 多次调用返回同一 `Arc`，三个工厂也总是返回各自
/// 唯一的冻结实例（`Arc::ptr_eq` 可验证）。
pub fn noop_telemetry_metrics() -> Arc<dyn TelemetryMetrics> {
    Arc::clone(&NOOP_METRICS)
}

/// `NOOP_TELEMETRY_PROPAGATION`：`inject` 恒 `None`、`extract` 恒返回 no-op 上下文的传播实现。
pub fn noop_telemetry_propagation() -> Arc<dyn TelemetryPropagation> {
    Arc::clone(&NOOP_PROPAGATION)
}

/// `noopTelemetrySpan` 的 `TelemetryContext` 视图（同一 `Arc`，供只接受 context 的位置使用）。
///
/// 这是 trait 向上转型：返回的 `Arc<dyn TelemetryContext>` 与 [`noop_telemetry_context`]
/// 指向同一对象（同一分配），只是把类型擦除成更窄的 trait 视图。durable 的 harness 在
/// 调用方未提供遥测上下文时用它兜底。
pub fn noop_telemetry_context_as_context() -> Arc<dyn TelemetryContext> {
    let span: Arc<dyn TelemetrySpan> = noop_telemetry_context();
    let context: Arc<dyn TelemetryContext> = span;
    context
}
