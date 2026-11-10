//! testing/{index,conformance,types}.ts 的 Rust 化：运行器无关的适配器一致性套件。
//!
//! 与原实现的差异（有意为之）：
//! - 每个 case 返回 `Result<(), TelemetryError>` 而不是 panic，任何测试框架都能注册（TS 用
//!   `node:assert/strict`，这里只保留 spec §27.1 要求的**逐字消息**：
//!   `Expected target span`、`Expected source span`、`Expected propagation on each fixture`、
//!   `Expected callback span`、`Expected recorded span {name}`、`Expected operation to reject`）。
//! - `AsyncDisposable` 的 `await using` → `TelemetryAdapterFixture::dispose`；runner 在 case
//!   结束（成功或失败）后一定调用。
//! - JS 的 rejection 可以是任意值（含 `undefined`）；冻结的契约是
//!   `Result<_, TelemetryError>`，因此「同一个 rejection 值」降级为「同一个 `TelemetryError`」
//!   （逐字段相等，含 message）。见 spec §25 第 3 条。
//! - **编译期断言**（`expectTypeOf` / 仅类型层面的 schema 校验，spec §25 第 4 条）没有运行时
//!   对应物，因此不移植：它们只约束 TS 调用方，不产生任何可观察行为，Rust 侧用类型别名
//!   （`TelemetrySchemaSpanName` 等）保留**名字**而不保留检查。
//!
//! TS: `packages/telemetry/src/testing/{index,types,conformance}.ts`
//!
//! # 这个模块解决什么问题
//! 任何适配器（本仓库的 `InMemoryTelemetryContext`，或遵循同一契约的下游实现）只要实现
//! [`TelemetryContext`] 等 trait，就能用同一组可执行断言证明自己的可观察行为与 TS 实现一致。
//! 套件不与测试框架耦合：`create_telemetry_adapter_conformance` 只返回一个 `Vec`，调用方
//! 自行决定注册成 `#[tokio::test]` 还是交给别的 runner（本仓库见 `tests/conformance.rs`）。
//!
//! # 设计原因
//! 套件断言的是“适配器对外可观察的行为”，而不是某种实现细节，因此每个 case 只通过
//! `TelemetryContext` / `TelemetryMetrics` / `TelemetryPropagation` 的公开契约操作数据；
//! 工厂与快照读取器都由调用方注入，套件自身不保存任何可变的全局状态。
//!
//! # 执行流程与 fixture 生命周期
//! 1. 工厂 `TelemetryAdapterFixtureFactory` 在每次 `case.run()` 时至少调用一次（先创建主
//!    fixture），负责创建一个与其它 case 完全隔离的适配器实例；`propagation_multi_hop_case`
//!    还会在 case 体内再调用该工厂两次。工厂失败直接成为该 case 的失败（此时没有 fixture 可释放）。
//! 2. case 主体拿到 fixture 后执行断言并返回 `Result`。
//! 3. 无论主体成功还是失败，`run` 都会调用 `TelemetryAdapterFixture::dispose_now`（对应 TS 的
//!    `await using` / `AsyncDisposable`），最后把主体的结果原样返回。
//!
//! 因此 `run` 是 `Fn` 而不是 `FnOnce`：同一份 case 列表可以反复执行，每次都会重新创建 fixture。
//!
//! # 14 个 case
//! group 与 name 逐字来自 TS（spec `telemetry-mcp.md` §7.3）：capabilities 1 个、metrics 2 个、
//! links 2 个、propagation 3 个、callback lifecycle 2 个、status 1 个、recording 2 个、
//! parentage 1 个。顺序与 TS 注册顺序一致，但 case 之间互不依赖，runner 可以只跑子集。
//!
//! # 可选能力与跳过规则
//! metrics 与 propagation 是可选能力，但“跳过”本身也是契约：
//! - 没有有效 metrics 时，两个 metrics case 直接通过；
//! - 声明了 metrics 却没有 `get_metrics` 读取器时判失败（无法核对规范化测量，而不是静默跳过）；
//! - 没有有效 propagation 时，三个 propagation case 直接通过。
//!
//! # 注意事项
//! - `propagates across fresh adapters and multiple hops` 会额外向工厂要两个 fixture，并在断言
//!   末尾显式 dispose。它们不受 `create_case` 自动 dispose 的保护：若中途 `?` 提前返回，这两个
//!   fixture 不会被释放。新增 case 若也创建额外 fixture，需要自己保证清理。
//! - 断言失败以 `Err(TelemetryError)` 返回，不靠 panic 控制流。`expect` 只用于“本不应失败”的
//!   内部前提：`json_roundtrip_carrier` 的 JSON 往返，以及 `parentage_case` 里本地创建、
//!   从不 close 的信号量；它们出问题表示测试基础设施损坏，而不是适配器不符合契约。
//! - fixture 的 `dispose` 返回 `()`，没有失败通道：清理阶段出错无法上报为本 case 的失败。

use std::sync::Arc;

use crate::index::{
    BoxFuture, ErasedValue, SpanAttributes, SpanLink, SpanOptions, SpanStatus, StatusError,
    TelemetryContext, TelemetryContextExt, TelemetryError, TelemetryPropagation, TelemetrySpan,
};
use crate::memory::{RecordedTelemetryMeasurement, RecordedTelemetrySpan};
use crate::metrics::TelemetryMetrics;

/// 转出 store 一致性套件的公开类型与入口；定义位于 `crate::store_conformance`。
/// 这样调用方只引入 `kiana_telemetry::testing` 就能同时拿到 adapter 与 store 两套 case。
pub use crate::store_conformance::{
    StoreConformanceCase, StoreFixture, StoreFixtureFactory, telemetry_store_conformance,
};

/// 适配器 fixture：一次一致性 case 独占的实例与规范化快照读取器。
///
/// `context` 必须实现 [`TelemetryContext`]；`propagation` / `metrics` 缺省时回退到
/// `context.propagation()` / `context.metrics()`。
///
/// # 不变量
/// - fixture 不跨 case 复用；同一个 case 内可以再创建额外 fixture（见 multi-hop case）。
///   `get_spans` / `get_metrics` 读到的必须是该实例自创建以来的记录，不能混入其它 fixture
///   的 span——套件会用快照里的 span 数量和父级关系做断言。
/// - `propagation` / `metrics` 为 `None` 表示“fixture 不额外覆盖”，**不等于**适配器没有该能力：
///   能力是否存在由 `context.metrics()` / `context.propagation()` 决定（见
///   [`TelemetryAdapterFixture::effective_metrics`] / [`TelemetryAdapterFixture::effective_propagation`]）。
///
/// # 生命周期
/// `dispose` 对应 TS 的 `[Symbol.asyncDispose]()`，由 `create_case` 在 case 结束（成功或失败）后
/// 调用。是否为 `None` 由具体 fixture 决定：`None` 时 `dispose_now` 直接返回，不做任何清理
/// （`TelemetryAdapterFixture::new` 构造的 fixture 就是这种情形）。
#[derive(Clone)]
pub struct TelemetryAdapterFixture {
    /// 被测适配器的主上下文；普通 case 从它或其子 span 启动 span，传播 case 还会从 extract
    /// 得到的父上下文启动（父级可能来自其它适配器）。
    pub context: Arc<dyn TelemetryContext>,
    /// 可选的传播能力覆盖；`None` 时回退到 `context.propagation()`。
    pub propagation: Option<Arc<dyn TelemetryPropagation>>,
    /// 可选的指标能力覆盖；`None` 时回退到 `context.metrics()`。
    pub metrics: Option<Arc<dyn TelemetryMetrics>>,
    /// `getMetrics?.()`：缺省表示该适配器不提供规范化测量快照。
    pub get_metrics: Option<
        Arc<dyn Fn() -> BoxFuture<'static, Vec<RecordedTelemetryMeasurement>> + Send + Sync>,
    >,
    /// `getSpans()`。
    pub get_spans: Arc<dyn Fn() -> BoxFuture<'static, Vec<RecordedTelemetrySpan>> + Send + Sync>,
    /// `[Symbol.asyncDispose]()`；runner 在每个 case 结束后调用。
    pub dispose: Option<Arc<dyn Fn() -> BoxFuture<'static, ()> + Send + Sync>>,
}

impl TelemetryAdapterFixture {
    /// 便捷构造：只有 context 与 getSpans。
    pub fn new(
        context: Arc<dyn TelemetryContext>,
        get_spans: Arc<dyn Fn() -> BoxFuture<'static, Vec<RecordedTelemetrySpan>> + Send + Sync>,
    ) -> Self {
        TelemetryAdapterFixture {
            context,
            propagation: None,
            metrics: None,
            get_metrics: None,
            get_spans,
            dispose: None,
        }
    }

    /// `fixture.metrics ?? fixture.context.metrics`。
    pub fn effective_metrics(&self) -> Option<Arc<dyn TelemetryMetrics>> {
        self.metrics.clone().or_else(|| self.context.metrics())
    }

    /// `fixture.propagation ?? fixture.context.propagation`。
    pub fn effective_propagation(&self) -> Option<Arc<dyn TelemetryPropagation>> {
        self.propagation
            .clone()
            .or_else(|| self.context.propagation())
    }

    /// 调用 `getMetrics()`；缺失时返回 `None`。
    pub async fn metrics_snapshot(&self) -> Option<Vec<RecordedTelemetryMeasurement>> {
        match &self.get_metrics {
            Some(reader) => Some(reader().await),
            None => None,
        }
    }

    /// 调用 `getSpans()`。
    pub async fn spans_snapshot(&self) -> Vec<RecordedTelemetrySpan> {
        (self.get_spans)().await
    }

    /// `await using fixture` 的 dispose 步骤：同步版（无需 await 的适配器也可直接调用）。
    pub async fn dispose_now(&self) {
        if let Some(dispose) = &self.dispose {
            dispose().await;
        }
    }
}

/// `TelemetryAdapterFixtureFactory`：为一个 case 创建隔离的 fixture。
///
/// 每次 `run` 都会先调用工厂一次（创建主 fixture）；multi-hop case 还会在同一个 case 内再调用
/// 两次。因此工厂必须每次都返回新实例，不能复用共享记录器，否则父级与快照断言会互相污染。
/// 工厂返回 `Err` 会让当前 case 直接失败（此时没有 fixture 可供 dispose）。
pub type TelemetryAdapterFixtureFactory = Arc<
    dyn Fn() -> BoxFuture<'static, Result<TelemetryAdapterFixture, TelemetryError>> + Send + Sync,
>;

/// 运行器无关的一致性 case（group/name 逐字来自 TS）。
///
/// `run` 可以被重复调用：每次调用都会重新向工厂要 fixture；只要工厂遵守“每次返回新实例”的
/// 约定，case 之间、同一 case 的多次执行之间就不会共享记录状态。
#[derive(Clone)]
pub struct TelemetryAdapterConformanceCase {
    /// 测试分组标题；保留既有文案作为稳定契约，便于报告与上游测试对照。
    pub group: &'static str,
    /// case 标题；保留既有文案作为稳定契约，runner 常用它筛选单个 case。
    pub name: &'static str,
    /// 执行一次 case：创建 fixture → 运行断言 → 必定 dispose。
    pub run: Arc<dyn Fn() -> BoxFuture<'static, Result<(), TelemetryError>> + Send + Sync>,
}

// ---------------------------------------------------------------------------
// 断言辅助（保留 spec §27.1 的逐字消息）
// ---------------------------------------------------------------------------

/// `ok(value, message)`：条件为假时以给定消息失败。
///
/// 名字沿用 TS 的 `ok(...)` 习惯；返回 `Result` 而非 panic，使错误消息与“第几个 case 失败”
/// 由 runner 统一处理。
pub fn expect(condition: bool, message: impl Into<String>) -> Result<(), TelemetryError> {
    if condition {
        Ok(())
    } else {
        Err(TelemetryError::new(message))
    }
}

/// `strictEqual(actual, expected)` / `deepStrictEqual(actual, expected)`。
///
/// `IndexMap` 的 `PartialEq` 忽略顺序，与 `deepStrictEqual` 的对象比较一致。
/// 失败消息的格式是 `Expected {expected:?}, got {actual:?}`（先期望值后实际值）；
/// 依赖 `Debug` 输出，因此只适合测试诊断，不是稳定的对外格式。
pub fn expect_eq<T: PartialEq + std::fmt::Debug>(
    actual: &T,
    expected: &T,
) -> Result<(), TelemetryError> {
    if actual == expected {
        Ok(())
    } else {
        Err(TelemetryError::new(format!(
            "Expected {expected:?}, got {actual:?}"
        )))
    }
}

/// `strictEqual` 的字符串版（错误消息更短）。
///
/// 本模块的调用方用 `as_deref().unwrap_or("")` 传入可选字段，因此“字段缺失”与“字段为空串”
/// 在该断言里不可区分。
pub fn expect_str(actual: &str, expected: &str) -> Result<(), TelemetryError> {
    if actual == expected {
        Ok(())
    } else {
        Err(TelemetryError::new(format!(
            "Expected {expected:?}, got {actual:?}"
        )))
    }
}

/// `findSpan(spans, name)`：第一个同名 span；缺失时 `Expected recorded span {name}`。
///
/// 只返回第一个匹配项，不检查唯一性；命中哪个由调用方传入切片的顺序决定。
pub fn find_span<'a>(
    spans: &'a [RecordedTelemetrySpan],
    name: &str,
) -> Result<&'a RecordedTelemetrySpan, TelemetryError> {
    spans
        .iter()
        .find(|candidate| candidate.name == name)
        .ok_or_else(|| TelemetryError::new(format!("Expected recorded span {name}")))
}

/// `rejectsWithSameValue(operation, expected)`。
///
/// Rust 侧比较 `TelemetryError` 的字段（含 message），因为契约固定了错误类型。
/// 三种结果：`Ok(_)` → “Expected operation to reject”；错误逐字段相等 → 通过；否则报告两个
/// message。这里用相等而不是 `Arc`/指针身份：Rust 的 rejection 通道传递的是值，调用方无法
/// 观察“同一个错误实例”。
pub async fn rejects_with_same_value(
    operation: BoxFuture<'_, Result<ErasedValue, TelemetryError>>,
    expected: &TelemetryError,
) -> Result<(), TelemetryError> {
    match operation.await {
        Ok(_) => Err(TelemetryError::new("Expected operation to reject")),
        Err(error) if &error == expected => Ok(()),
        Err(error) => Err(TelemetryError::new(format!(
            "Expected rejection {}, got {}",
            expected.message, error.message
        ))),
    }
}

/// 轮询一次 future（无唤醒源），用于断言「首个 poll 已同步接纳回调」。
///
/// noop waker 的 `wake` 是空操作：若被测 future 依赖唤醒才能前进，第一次 poll 会返回
/// `Pending`，而这里不再替它注册真正的 waker。本模块的用法只有两种：首个 poll 已经完成，
/// 或者调用方之后用正常的 `.await` 继续推进（此时运行时会换上自己的 waker）。
/// 要求 `Unpin` 是为了能直接对 `&mut F` 调用 `Pin::new`；调用点都是 `Box::pin(...)`。
pub fn poll_once<F: std::future::Future + Unpin>(future: &mut F) -> std::task::Poll<F::Output> {
    let waker = std::task::Waker::noop();
    let mut context = std::task::Context::from_waker(waker);
    std::pin::Pin::new(future).poll(&mut context)
}

/// 判断两个 trait object 的 `Arc` 是否指向同一分配（`===` 的对象身份）。
///
/// `Arc::as_ptr` 对 trait object 得到胖指针；转成 `*const ()` 后只比较数据地址、丢弃 vtable。
/// 两个 `Arc` 都存活期间，数据地址相同即同一分配；不比较动态类型或实现（同一分配必然同类型）。
/// 对应 TS 的 `===`：仅“行为相同”不够，必须是同一个对象。
pub fn same_allocation<T: ?Sized + Send + Sync>(left: &Arc<T>, right: &Arc<T>) -> bool {
    Arc::as_ptr(left) as *const () == Arc::as_ptr(right) as *const ()
}

// ---------------------------------------------------------------------------
// case 注册
// ---------------------------------------------------------------------------

type CaseBody =
    dyn Fn(TelemetryAdapterFixture) -> BoxFuture<'static, Result<(), TelemetryError>> + Send + Sync;

// 把 case 主体包装成可重复执行的闭包：每次 `run` 都重新向工厂要一个 fixture。
//
// - 工厂失败时直接返回错误；此时没有 fixture 存在，因此没有可 dispose 的对象。
// - 主体结果的 `Result` 先被保存，dispose 之后才返回；`dispose_now` 返回 `()`，没有 `Result`
//   通道，因此清理阶段的错误无法上报为本 case 的失败（fixture 的约定见 `TelemetryAdapterFixture`）。
// - `fixture.clone()` 只是克隆 `Arc` 字段与读取器，dispose 看到的仍是同一底层适配器。
fn create_case(
    factory: &TelemetryAdapterFixtureFactory,
    group: &'static str,
    name: &'static str,
    test: Arc<CaseBody>,
) -> TelemetryAdapterConformanceCase {
    let factory = Arc::clone(factory);
    TelemetryAdapterConformanceCase {
        group,
        name,
        run: Arc::new(move || {
            let factory = Arc::clone(&factory);
            let test = Arc::clone(&test);
            Box::pin(async move {
                let fixture = factory().await?;
                let outcome = test(fixture.clone()).await;
                fixture.dispose_now().await;
                outcome
            })
        }),
    }
}

// ---------------------------------------------------------------------------
// 便捷构造
// ---------------------------------------------------------------------------

/// `SpanOptions` 便捷构造（无属性、无 link）。
pub fn span_options(name: &str) -> SpanOptions {
    SpanOptions {
        name: name.to_string(),
        attributes: None,
        links: None,
    }
}

/// 属性表便捷构造。
///
/// 返回 `IndexMap`，保留 `pairs` 的顺序；套件会用键序断言 JS 对象的枚举顺序，因此这里的
/// 插入顺序是有意义的。
pub fn attributes(pairs: &[(&str, crate::index::AttributeValue)]) -> SpanAttributes {
    let mut map = SpanAttributes::new();
    for (key, value) in pairs {
        map.insert((*key).to_string(), value.clone());
    }
    map
}

/// `{ kind: "error", error?: {...} }`。
///
/// `name` 与 `message` 必须同时给出才写入 `StatusError`；只给其中一个时退化为不带详情的 error
/// （对应 TS 的可选 `error` 字段）。
pub fn error_status(name: Option<&str>, message: Option<&str>) -> SpanStatus {
    SpanStatus::Error {
        error: match (name, message) {
            (Some(name), Some(message)) => Some(StatusError {
                name: name.to_string(),
                message: message.to_string(),
            }),
            _ => None,
        },
    }
}

/// 由 span 的 `span_context_id()` 构造 link（对应 TS 里直接传 `span`）。
pub fn link_to(span: &Arc<dyn TelemetrySpan>) -> SpanLink {
    SpanLink {
        context: span.span_context_id(),
        attributes: None,
    }
}

/// 带属性的 link。
pub fn link_to_with(span: &Arc<dyn TelemetrySpan>, attributes: SpanAttributes) -> SpanLink {
    SpanLink {
        context: span.span_context_id(),
        attributes: Some(attributes),
    }
}

/// `JSON.parse(JSON.stringify(carrier))`。
///
/// 模拟跨进程/跨适配器传输：先用 `kiana_json::js_object::stringify` 按 JS 语义序列化成文本，
/// 再用 `kiana_json::parse` 解析出一个不与被序列化对象共享引用的新 map。传播 case 用它验证
/// 适配器只依赖 carrier 的内容，而不是某次调用中的内存身份。
///
/// # Panics
/// 解析失败时 panic：carrier 的元素全是字符串，正常实现下往返不会失败；触发即表示 JSON 桥
/// 损坏，不属于一致性断言，因此不做 `Result` 降级。解析结果里的非字符串条目会被丢弃
/// （防御性过滤；当前 `TelemetryCarrier` 的值类型都是 `String`，实际不会触发）。
pub fn json_roundtrip_carrier(
    carrier: &crate::index::TelemetryCarrier,
) -> crate::index::TelemetryCarrier {
    let value = crate::jsval::carrier_to_value(carrier);
    let text = kiana_json::js_object::stringify(&value);
    let parsed = kiana_json::parse(&text).expect("carrier JSON roundtrip");
    let mut out = crate::index::TelemetryCarrier::new();
    if let serde_json::Value::Object(map) = parsed {
        for (key, value) in map {
            if let serde_json::Value::String(text) = value {
                out.insert(key, text);
            }
        }
    }
    out
}

// ---------------------------------------------------------------------------
// createTelemetryAdapterConformance：14 个 case
// ---------------------------------------------------------------------------

/// 创建运行器无关的一致性 case 列表（group/name 逐字，见 spec §7.3）。
///
/// 返回的 14 个 case 顺序与 TS 注册顺序一致，但彼此独立：每个 case 自己向工厂要 fixture，
/// 因此 runner 可以只执行其中一部分（本仓库测试按 `name` 查找单个 case）。
///
/// 工厂以 `Arc` 形式被所有 case 共享，但每次 `run` 都会重新调用它；`multi_hop` case 还会在
/// 一个 case 内多次调用，因此工厂必须保证每次返回隔离实例。
///
/// TS: `packages/telemetry/src/testing/conformance.ts#createTelemetryAdapterConformance`
pub fn create_telemetry_adapter_conformance(
    factory: TelemetryAdapterFixtureFactory,
) -> Vec<TelemetryAdapterConformanceCase> {
    vec![
        capabilities_case(&factory),
        metrics_ordered_case(&factory),
        metrics_copied_case(&factory),
        links_initial_and_late_case(&factory),
        links_after_settlement_case(&factory),
        propagation_roundtrip_case(&factory),
        propagation_absent_case(&factory),
        propagation_multi_hop_case(&factory),
        lifecycle_admits_case(&factory),
        lifecycle_rejection_case(&factory),
        status_last_explicit_case(&factory),
        recording_merge_case(&factory),
        recording_inert_case(&factory),
        parentage_case(&factory),
    ]
}

/// 断言 `context.metrics()` / `context.propagation()` 与期望能力是同一对象。
///
/// 三种情况：双方都没有该能力 → 通过；双方都有 → 必须 [`same_allocation`]（对象身份相同，
/// 仅行为等价不够）；一边有一边没有 → 失败。metrics 与 propagation 分别检查，错误消息也分开。
fn assert_capabilities(
    context: &Arc<dyn TelemetryContext>,
    metrics: Option<&Arc<dyn TelemetryMetrics>>,
    propagation: Option<&Arc<dyn TelemetryPropagation>>,
) -> Result<(), TelemetryError> {
    match (context.metrics(), metrics) {
        (Some(actual), Some(expected)) => expect(
            same_allocation(&actual, expected),
            "Expected context.metrics to be the same object",
        )?,
        (None, None) => {}
        _ => {
            return Err(TelemetryError::new(
                "Expected context.metrics to match the fixture",
            ));
        }
    }
    match (context.propagation(), propagation) {
        (Some(actual), Some(expected)) => expect(
            same_allocation(&actual, expected),
            "Expected context.propagation to be the same object",
        )?,
        (None, None) => {}
        _ => {
            return Err(TelemetryError::new(
                "Expected context.propagation to match the fixture",
            ));
        }
    }
    Ok(())
}

// 验证能力句柄（metrics / propagation）沿上下文链保持同一对象：根 context、span、子 span、
// propagation.extract 得到的父上下文及其子 span、已结算 span 及其启动的惰性子 span。
// 最后一种证明“结算”不会让能力丢失。
// 若适配器提供 propagation，还要求 inject → extract 往返仍携带同一能力；
// inject 返回 None 时 extract(None) 必须回退到根上下文（TS 侧无条件执行）。
fn capabilities_case(factory: &TelemetryAdapterFixtureFactory) -> TelemetryAdapterConformanceCase {
    let body: Arc<CaseBody> = Arc::new(|fixture: TelemetryAdapterFixture| {
        Box::pin(async move {
            let metrics = fixture.effective_metrics();
            let propagation = fixture.effective_propagation();
            let context = Arc::clone(&fixture.context);
            assert_capabilities(&context, metrics.as_ref(), propagation.as_ref())?;

            // 把回调里的 span 句柄带出回调，用来在“结算之后”继续检查能力。
            let (settled_tx, settled_rx) =
                tokio::sync::oneshot::channel::<Arc<dyn TelemetrySpan>>();
            let metrics_for_cb = metrics.clone();
            let propagation_for_cb = propagation.clone();
            let root = Arc::clone(&fixture.context);
            root.start_span_typed(span_options("capabilities"), move |span| {
                let metrics = metrics_for_cb.clone();
                let propagation = propagation_for_cb.clone();
                let span_for_check = Arc::clone(&span);
                Box::pin(async move {
                    let span_context: Arc<dyn TelemetryContext> = span_for_check.clone();
                    assert_capabilities(&span_context, metrics.as_ref(), propagation.as_ref())?;

                    let child_metrics = metrics.clone();
                    let child_propagation = propagation.clone();
                    span.start_span_typed(span_options("capabilities.child"), move |child| {
                        let metrics = child_metrics.clone();
                        let propagation = child_propagation.clone();
                        let child_context: Arc<dyn TelemetryContext> = child;
                        Box::pin(async move {
                            assert_capabilities(
                                &child_context,
                                metrics.as_ref(),
                                propagation.as_ref(),
                            )
                        })
                    })
                    .await??;

                    if let Some(propagation) = propagation.clone() {
                        let span_context: Arc<dyn TelemetryContext> = span_for_check.clone();
                        // TS: `const parent = propagation.extract(propagation.inject(span))` —— 无条件
                        // 执行；`inject` 返回 `undefined` 时 `extract(undefined)` 回退到根上下文，
                        // 能力断言与 `capabilities.extracted` 照样运行（适配器的 inject 返回 None
                        // 也必须被测到）。
                        let carrier = propagation.inject(span_context.as_ref());
                        let parent = propagation.extract(carrier.as_ref());
                        let parent_context = Arc::clone(&parent);
                        assert_capabilities(&parent_context, metrics.as_ref(), Some(&propagation))?;
                        let extracted_metrics = metrics.clone();
                        let extracted_propagation = Arc::clone(&propagation);
                        parent
                            .start_span_typed(span_options("capabilities.extracted"), move |span| {
                                let span_context: Arc<dyn TelemetryContext> = span;
                                let metrics = extracted_metrics.clone();
                                let propagation = Arc::clone(&extracted_propagation);
                                Box::pin(async move {
                                    assert_capabilities(
                                        &span_context,
                                        metrics.as_ref(),
                                        Some(&propagation),
                                    )
                                })
                            })
                            .await??;
                    }
                    let _ = settled_tx.send(span_for_check);
                    Ok::<(), TelemetryError>(())
                })
            })
            .await??;

            let settled = settled_rx
                .await
                .map_err(|_| TelemetryError::new("Expected callback span"))?;
            let settled_context: Arc<dyn TelemetryContext> = settled.clone();
            assert_capabilities(&settled_context, metrics.as_ref(), propagation.as_ref())?;
            let inert_metrics = metrics.clone();
            let inert_propagation = propagation.clone();
            settled
                .start_span_typed(span_options("capabilities.inert"), move |span| {
                    let span_context: Arc<dyn TelemetryContext> = span;
                    Box::pin(async move {
                        assert_capabilities(
                            &span_context,
                            inert_metrics.as_ref(),
                            inert_propagation.as_ref(),
                        )
                    })
                })
                .await??;
            Ok(())
        })
    });
    create_case(
        factory,
        "capabilities",
        "carries adapter capabilities through contexts and spans",
        body,
    )
}

// 锁定 getMetrics() 快照的两条性质：
// 1. 快照顺序 = 记录顺序（counter、两次 upDownCounter（+1 与 -1 各一条）、histogram），
//    全部测量都按 (kind, name, value) 比较；
// 2. 只对部分测量的元数据做断言：measurements[0] 的 description / unit / attributes，以及
//    measurements[3]（histogram）的 boundaries 与 exemplar context（record 的第三个参数，
//    与当时 span 的 carrier 一致）；其余测量不检查 description / unit / attributes。
// 有 metrics 能力却没有 get_metrics 时直接判失败，而不是跳过——无法验证“规范化快照”本身
// 就说明 fixture 声明得不完整。
fn metrics_ordered_case(
    factory: &TelemetryAdapterFixtureFactory,
) -> TelemetryAdapterConformanceCase {
    let body: Arc<CaseBody> = Arc::new(|fixture: TelemetryAdapterFixture| {
        Box::pin(async move {
            // 可选能力：未声明 metrics 的适配器跳过整条 case。
            let Some(metrics) = fixture.effective_metrics() else {
                return Ok(());
            };
            // 声明了 metrics 就必须能读出“规范化测量”，否则快照契约无从验证。
            let Some(_) = fixture.get_metrics else {
                return Err(TelemetryError::new(
                    "Metrics fixtures require a normalized measurement reader",
                ));
            };
            metrics
                .counter(
                    "bytes",
                    Some(crate::metrics::InstrumentOptions {
                        description: Some("Encoded bytes".to_string()),
                        unit: Some("By".to_string()),
                    }),
                )
                .add(
                    12.0,
                    Some(attributes(&[(
                        "direction",
                        crate::index::AttributeValue::String("out".into()),
                    )])),
                );
            let active = metrics.up_down_counter("active", None);
            active.add(1.0, None);
            active.add(-1.0, None);
            let histogram = metrics.histogram(
                "duration",
                Some(crate::metrics::HistogramOptions {
                    instrument: crate::metrics::InstrumentOptions {
                        description: None,
                        unit: Some("ms".to_string()),
                    },
                    boundaries: Some(vec![1.0, 10.0]),
                }),
            );
            let histogram_for_cb = Arc::clone(&histogram);
            fixture
                .context
                .start_span_typed(span_options("measured"), move |span| {
                    let histogram = Arc::clone(&histogram_for_cb);
                    Box::pin(async move {
                        let context: &dyn TelemetryContext = span.as_ref();
                        histogram.record(
                            5.0,
                            Some(attributes(&[(
                                "method",
                                crate::index::AttributeValue::String("read".into()),
                            )])),
                            Some(context),
                        );
                    })
                })
                .await?;

            let measurements = fixture.metrics_snapshot().await.unwrap_or_default();
            // 先只比较 (kind, name, value) 三元组；metadata / attributes 在下面单独断言，
            // 这样失败消息能直接指出是“顺序/数值”问题还是“元数据”问题。
            let summary: Vec<(String, String, f64)> = measurements
                .iter()
                .map(|measurement| {
                    (
                        kind_text(measurement.kind).to_string(),
                        measurement.name.clone(),
                        measurement.value,
                    )
                })
                .collect();
            expect_eq(
                &summary,
                &vec![
                    ("counter".to_string(), "bytes".to_string(), 12.0),
                    ("upDownCounter".to_string(), "active".to_string(), 1.0),
                    ("upDownCounter".to_string(), "active".to_string(), -1.0),
                    ("histogram".to_string(), "duration".to_string(), 5.0),
                ],
            )?;
            let first = measurements
                .first()
                .ok_or_else(|| TelemetryError::new("Expected measurement"))?;
            expect_eq(
                &first.attributes,
                &attributes(&[(
                    "direction",
                    crate::index::AttributeValue::String("out".into()),
                )]),
            )?;
            expect_str(first.description.as_deref().unwrap_or(""), "Encoded bytes")?;
            expect_str(first.unit.as_deref().unwrap_or(""), "By")?;
            // 最后一条是 histogram：boundaries 来自创建 instrument 时的选项，
            // context 来自 record 的第三个参数（span 作为 exemplar 上下文）。
            let last = measurements
                .get(3)
                .ok_or_else(|| TelemetryError::new("Expected measurement"))?;
            expect_eq(&last.boundaries, &Some(vec![1.0, 10.0]))?;
            let spans = fixture.spans_snapshot().await;
            let measured = find_span(&spans, "measured")?;
            expect_eq(&last.context, &Some(measured.context.clone()))?;
            Ok(())
        })
    });
    create_case(
        factory,
        "metrics",
        "records ordered measurements and optional histogram exemplars",
        body,
    )
}

fn kind_text(kind: crate::index::MetricKind) -> &'static str {
    match kind {
        crate::index::MetricKind::Counter => "counter",
        crate::index::MetricKind::UpDownCounter => "upDownCounter",
        crate::index::MetricKind::Histogram => "histogram",
    }
}

// 验证快照返回的是“记录时刻的副本”，而不是会随后续写入变化的视图：
// - histogram 的 boundaries 与测量的 attributes 在快照中保持创建/记录时的值；
// - 未提供 context 的测量，其 context 必须是 None。
// 注意：下面在创建/记录之后继续修改本地数组，这是照搬 TS 用例的变异结构；Rust 的
// `HistogramOptions` 与 `SpanAttributes` 都按值保存（调用处传的是 clone），实现从类型上
// 就不可能观察到这些后续修改，因此这部分不构成额外约束。
fn metrics_copied_case(
    factory: &TelemetryAdapterFixtureFactory,
) -> TelemetryAdapterConformanceCase {
    let body: Arc<CaseBody> = Arc::new(|fixture: TelemetryAdapterFixture| {
        Box::pin(async move {
            // 同样的跳过规则：无 metrics 跳过；有 metrics 无读取器判失败。
            let Some(metrics) = fixture.effective_metrics() else {
                return Ok(());
            };
            let Some(_) = fixture.get_metrics else {
                return Err(TelemetryError::new(
                    "Metrics fixtures require a normalized measurement reader",
                ));
            };
            let mut boundaries = vec![1.0, 10.0];
            let histogram = metrics.histogram(
                "snapshot",
                Some(crate::metrics::HistogramOptions {
                    instrument: crate::metrics::InstrumentOptions {
                        description: Some("Original".to_string()),
                        unit: None,
                    },
                    boundaries: Some(boundaries.clone()),
                }),
            );
            boundaries.push(100.0);
            let mut tags = vec!["original".to_string()];
            histogram.record(
                -1.0,
                Some(attributes(&[(
                    "tags",
                    crate::index::AttributeValue::StringArray(tags.clone()),
                )])),
                None,
            );
            // 记录之后修改原数组（传入记录的是它的克隆）：与 TS 用例同构，Rust 下不可被观察。
            tags.push("changed".to_string());
            let measurements = fixture.metrics_snapshot().await.unwrap_or_default();
            expect(measurements.len() == 1, "Expected exactly one measurement")?;
            let first = &measurements[0];
            expect_eq(
                &first.attributes,
                &attributes(&[(
                    "tags",
                    crate::index::AttributeValue::StringArray(vec!["original".to_string()]),
                )]),
            )?;
            expect_eq(&first.boundaries, &Some(vec![1.0, 10.0]))?;
            expect_str(first.description.as_deref().unwrap_or(""), "Original")?;
            expect_eq(&first.context, &None)?;
            Ok(())
        })
    });
    create_case(
        factory,
        "metrics",
        "copies instrument metadata and measurement attributes",
        body,
    )
}

// 锁定三条 link 语义（spec §7.3 第 4 条）：
// 1. 启动选项里的初始 links 被记录，回调里 add_link 的 link 排在它之后；
// 2. 指向 noop 上下文（空 trace/span id）的 link 被静默丢弃（期望向量只有两项）；
// 3. link 不改变 span 的父级（parent_id / parent_context 仍为 None）。
// 回调里对属性数组的修改只是照搬 TS 的变异结构：传给 options 的是克隆，Rust 下不可观察。
fn links_initial_and_late_case(
    factory: &TelemetryAdapterFixtureFactory,
) -> TelemetryAdapterConformanceCase {
    let body: Arc<CaseBody> = Arc::new(|fixture: TelemetryAdapterFixture| {
        Box::pin(async move {
            let (target_tx, target_rx) = tokio::sync::oneshot::channel::<Arc<dyn TelemetrySpan>>();
            fixture
                .context
                .start_span_typed(span_options("target"), move |span| {
                    Box::pin(async move {
                        let _ = target_tx.send(span);
                    })
                })
                .await?;
            let target = target_rx
                .await
                .map_err(|_| TelemetryError::new("Expected target span"))?;

            // 传给 options 的是克隆；下面在回调里修改原变量，Rust 下实现观察不到这次修改。
            let mut initial_attributes = attributes(&[(
                "reason",
                crate::index::AttributeValue::StringArray(vec!["initial".to_string()]),
            )]);
            let options = SpanOptions {
                name: "linked".to_string(),
                attributes: None,
                links: Some(vec![SpanLink {
                    context: target.span_context_id(),
                    attributes: Some(initial_attributes.clone()),
                }]),
            };
            let target_for_cb = Arc::clone(&target);
            fixture
                .context
                .start_span_typed(options, move |span| {
                    let target = Arc::clone(&target_for_cb);
                    Box::pin(async move {
                        if let Some(crate::index::AttributeValue::StringArray(values)) =
                            initial_attributes.get_mut("reason")
                        {
                            values.push("mutated".to_string());
                        }
                        span.add_link(SpanLink {
                            context: target.span_context_id(),
                            attributes: Some(attributes(&[(
                                "reason",
                                crate::index::AttributeValue::String("late".into()),
                            )])),
                        });
                        // 指向 noop 上下文的 link 没有可解析的 span 标识，期望被适配器丢弃。
                        span.add_link(SpanLink {
                            context: crate::noop::noop_telemetry_context().span_context_id(),
                            attributes: None,
                        });
                    })
                })
                .await?;

            let spans = fixture.spans_snapshot().await;
            let linked = find_span(&spans, "linked")?;
            expect_eq(&linked.parent_id, &None)?;
            expect_eq(&linked.parent_context, &None)?;
            let target_context = find_span(&spans, "target")?.context.clone();
            let expected = vec![
                crate::memory::RecordedTelemetryLink {
                    context: target_context.clone(),
                    attributes: attributes(&[(
                        "reason",
                        crate::index::AttributeValue::StringArray(vec!["initial".to_string()]),
                    )]),
                },
                crate::memory::RecordedTelemetryLink {
                    context: target_context,
                    attributes: attributes(&[(
                        "reason",
                        crate::index::AttributeValue::String("late".into()),
                    )]),
                },
            ];
            expect_eq(&linked.links, &expected)?;
            Ok(())
        })
    });
    create_case(
        factory,
        "links",
        "records initial and late links without changing parents",
        body,
    )
}

// 已结算 span 上的 add_link 必须是静默 no-op：快照 links 仍为空。
// 与 recording/makes calls after settlement inert 一起界定“结算后不可写”的边界。
fn links_after_settlement_case(
    factory: &TelemetryAdapterFixtureFactory,
) -> TelemetryAdapterConformanceCase {
    let body: Arc<CaseBody> = Arc::new(|fixture: TelemetryAdapterFixture| {
        Box::pin(async move {
            let (source_tx, source_rx) = tokio::sync::oneshot::channel::<Arc<dyn TelemetrySpan>>();
            fixture
                .context
                .start_span_typed(span_options("link-source"), move |span| {
                    Box::pin(async move {
                        let _ = source_tx.send(span);
                    })
                })
                .await?;
            let source = source_rx
                .await
                .map_err(|_| TelemetryError::new("Expected source span"))?;
            source.add_link(link_to(&source));
            let spans = fixture.spans_snapshot().await;
            let recorded = find_span(&spans, "link-source")?;
            expect_eq(
                &recorded.links,
                &Vec::<crate::memory::RecordedTelemetryLink>::new(),
            )?;
            Ok(())
        })
    });
    create_case(
        factory,
        "links",
        "ignores links added after source settlement",
        body,
    )
}

// 传播只允许依赖 carrier 的内容：
// 1. inject(span) 必须给出 Some(carrier)；
// 2. carrier 经 JSON 序列化/反序列化（模拟跨进程）后 extract，仍能还原父级；
// 3. 在内容相同的独立副本上改值后，已提取的父上下文仍能启动子 span（Rust 的 extract 返回
//    `'static` trait object，无法借用 carrier；这一条主要是保留 TS 用例的变异隔离结构）；
// 4. 从父上下文启动的 receiver 记录的 parent_context 等于 sender 的 carrier；
// 5. 同一 span 再次 inject 仍返回与首次相同的 carrier（不因子 span 的创建而改变）。
fn propagation_roundtrip_case(
    factory: &TelemetryAdapterFixtureFactory,
) -> TelemetryAdapterConformanceCase {
    let body: Arc<CaseBody> = Arc::new(|fixture: TelemetryAdapterFixture| {
        Box::pin(async move {
            let Some(propagation) = fixture.effective_propagation() else {
                return Ok(());
            };
            let sender_propagation = Arc::clone(&propagation);
            let (result_tx, result_rx) =
                tokio::sync::oneshot::channel::<Result<(), TelemetryError>>();
            fixture
                .context
                .start_span_typed(span_options("sender"), move |span| {
                    let propagation = Arc::clone(&sender_propagation);
                    Box::pin(async move {
                        let trace = async move {
                            let span_context: Arc<dyn TelemetryContext> = span.clone();
                            let carrier =
                                propagation.inject(span_context.as_ref()).ok_or_else(|| {
                                    TelemetryError::new("Expected operation to reject")
                                })?;
                            let wire = json_roundtrip_carrier(&carrier);
                            let parent = propagation.extract(Some(&wire));
                            // 在 wire 的独立副本上把每个键都改成 "mutated"。Rust 的 extract 返回
                            // `'static` trait object，不可能借用 wire，所以这里主要保留 TS 用例的
                            // 变异隔离结构，而不是对 extract 深拷贝的额外证明。
                            let mut mutated = wire.clone();
                            let keys: Vec<String> = mutated.keys().cloned().collect();
                            for key in keys {
                                mutated.insert(key, "mutated".to_string());
                            }
                            // 显式使用变量，表达“只做变异、不参与后续断言”。
                            let _ = &mutated;
                            parent
                                .start_span_typed(span_options("receiver"), |_| {
                                    Box::pin(async move { 42 })
                                })
                                .await?;
                            // 同一 span 的 inject 应保持稳定；创建子树不能改变它输出的 carrier。
                            let original = propagation.inject(span_context.as_ref());
                            if original.as_ref() != Some(&carrier) {
                                return Err(TelemetryError::new(
                                    "Expected inject(span) to keep returning the carrier",
                                ));
                            }
                            Ok::<(), TelemetryError>(())
                        }
                        .await;
                        let _ = result_tx.send(trace);
                    })
                })
                .await?;
            // 回调内部完成断言后，用 oneshot 把结论送回外层；回调自身在这里不返回断言结果。
            result_rx
                .await
                .map_err(|_| TelemetryError::new("Expected operation to reject"))??;

            let spans = fixture.spans_snapshot().await;
            let receiver = find_span(&spans, "receiver")?;
            let sender = find_span(&spans, "sender")?;
            expect_eq(&receiver.parent_context, &Some(sender.context.clone()))?;
            Ok(())
        })
    });
    create_case(
        factory,
        "propagation",
        "roundtrips parent identity through JSON without retaining the carrier",
        body,
    )
}

// “没有可识别父级”的入口都必须回退为根上下文：
// inject(noop) 返回 None；extract(None) / extract({}) / extract({unknown:"format"}) 都产生根；
// 从这些上下文启动的 span 记录为根（parent_context 为 None），回调结果照常返回。
// 注意最后一条遍历的是整个 fixture 的快照：fixture 的隔离性在这里也被顺带验证。
fn propagation_absent_case(
    factory: &TelemetryAdapterFixtureFactory,
) -> TelemetryAdapterConformanceCase {
    let body: Arc<CaseBody> = Arc::new(|fixture: TelemetryAdapterFixture| {
        Box::pin(async move {
            let Some(propagation) = fixture.effective_propagation() else {
                return Ok(());
            };
            let noop_context = crate::noop::noop_telemetry_context();
            let noop_as_context: Arc<dyn TelemetryContext> = noop_context;
            expect(
                propagation.inject(noop_as_context.as_ref()).is_none(),
                "Expected inject(NOOP_TELEMETRY_CONTEXT) to be undefined",
            )?;
            // 三种“无父级”载体：缺失、空 map、未知键。
            let carriers: Vec<Option<crate::index::TelemetryCarrier>> = vec![
                None,
                Some(crate::index::TelemetryCarrier::new()),
                Some({
                    let mut carrier = crate::index::TelemetryCarrier::new();
                    carrier.insert("unknown".to_string(), "format".to_string());
                    carrier
                }),
            ];
            for (index, carrier) in carriers.iter().enumerate() {
                let context = propagation.extract(carrier.as_ref());
                let name = format!("fallback-{index}");
                let value: i32 = context
                    .start_span_typed(span_options(&name), |_| Box::pin(async move { 42 }))
                    .await?;
                expect(value == 42, "Expected callback result to be 42")?;
            }
            for span in fixture.spans_snapshot().await {
                expect_eq(&span.parent_context, &None)?;
            }
            Ok(())
        })
    });
    create_case(
        factory,
        "propagation",
        "absent and unknown carriers produce recorded roots",
        body,
    )
}

// 跨适配器传播：sender 在 fixture A 里注入 carrier，receiver 在全新 fixture B 里 extract
// 并再次注入，worker 在 fixture C 里 extract。每一跳都经过 JSON 往返，确保适配器之间只通过
// 字符串 carrier 关联；断言 receiver / worker 的 parent_context 分别等于上一跳注入的 carrier。
// 注意：B、C 是本 case 自己创建的，成功路径末尾显式 dispose；中途 `?` 提前返回会跳过清理。
fn propagation_multi_hop_case(
    factory: &TelemetryAdapterFixtureFactory,
) -> TelemetryAdapterConformanceCase {
    let factory_for_body = Arc::clone(factory);
    let body: Arc<CaseBody> = Arc::new(move |fixture: TelemetryAdapterFixture| {
        let factory = Arc::clone(&factory_for_body);
        Box::pin(async move {
            let Some(fixture_propagation) = fixture.effective_propagation() else {
                return Ok(());
            };
            // 再向工厂要两个独立实例，模拟另两个进程/适配器。
            let receiver = factory().await?;
            let worker = factory().await?;
            let (receiver_propagation, worker_propagation) = match (
                receiver.effective_propagation(),
                worker.effective_propagation(),
            ) {
                (Some(receiver), Some(worker)) => (receiver, worker),
                _ => {
                    return Err(TelemetryError::new("Expected propagation on each fixture"));
                }
            };

            let (sender_tx, sender_rx) =
                tokio::sync::oneshot::channel::<Option<crate::index::TelemetryCarrier>>();
            let sender_propagation = Arc::clone(&fixture_propagation);
            fixture
                .context
                .start_span_typed(span_options("sender"), move |span| {
                    let propagation = Arc::clone(&sender_propagation);
                    Box::pin(async move {
                        let context: Arc<dyn TelemetryContext> = span;
                        let _ = sender_tx.send(propagation.inject(context.as_ref()));
                    })
                })
                .await?;
            let sender_carrier = sender_rx
                .await
                .map_err(|_| TelemetryError::new("Expected operation to reject"))?
                .ok_or_else(|| TelemetryError::new("Expected operation to reject"))?;

            let (receiver_tx, receiver_rx) =
                tokio::sync::oneshot::channel::<Option<crate::index::TelemetryCarrier>>();
            let receiver_capture = Arc::clone(&receiver_propagation);
            receiver_propagation
                .extract(Some(&json_roundtrip_carrier(&sender_carrier)))
                .start_span_typed(span_options("receiver"), move |span| {
                    let propagation = Arc::clone(&receiver_capture);
                    Box::pin(async move {
                        let context: Arc<dyn TelemetryContext> = span;
                        let _ = receiver_tx.send(propagation.inject(context.as_ref()));
                    })
                })
                .await?;
            let receiver_carrier = receiver_rx
                .await
                .map_err(|_| TelemetryError::new("Expected operation to reject"))?
                .ok_or_else(|| TelemetryError::new("Expected operation to reject"))?;

            worker_propagation
                .extract(Some(&json_roundtrip_carrier(&receiver_carrier)))
                .start_span_typed(span_options("worker"), |_| Box::pin(async move {}))
                .await?;

            let receiver_spans = receiver.spans_snapshot().await;
            let recorded_receiver = find_span(&receiver_spans, "receiver")?;
            expect_eq(&recorded_receiver.parent_context, &Some(sender_carrier))?;
            let worker_spans = worker.spans_snapshot().await;
            let recorded_worker = find_span(&worker_spans, "worker")?;
            expect_eq(&recorded_worker.parent_context, &Some(receiver_carrier))?;
            // 这两个 fixture 不在 create_case 的自动 dispose 范围内，必须手动清理。
            receiver.dispose_now().await;
            worker.dispose_now().await;
            Ok(())
        })
    });
    create_case(
        factory,
        "propagation",
        "propagates across fresh adapters and multiple hops",
        body,
    )
}

// 对应 TS 的“startSpan 返回后、不 await 即已接纳回调”：
// Rust 的 future 是惰性的，等价判据是“第一次 poll 之后回调已执行且恰好一次”。
// 同时断言回调返回值被保留、span 正常结算为 ok。
// admitted / calls 用原子量在回调与断言之间传递观察值：回调要求 `Send + 'static`，不能借用
// 栈上的局部变量，因此用 `Arc<Atomic...>` 共享。
fn lifecycle_admits_case(
    factory: &TelemetryAdapterFixtureFactory,
) -> TelemetryAdapterConformanceCase {
    let body: Arc<CaseBody> = Arc::new(|fixture: TelemetryAdapterFixture| {
        Box::pin(async move {
            let admitted = Arc::new(std::sync::atomic::AtomicBool::new(false));
            let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let admitted_for_cb = Arc::clone(&admitted);
            let calls_for_cb = Arc::clone(&calls);
            let mut future = Box::pin(fixture.context.start_span_typed(
                span_options("success"),
                move |_| {
                    admitted_for_cb.store(true, std::sync::atomic::Ordering::SeqCst);
                    calls_for_cb.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    Box::pin(async move { 42 })
                },
            ));
            // `startSpan(...)` 返回后、未 await 即观察接纳（Rust 的等价判据：首个 poll）。
            let first_poll = poll_once(&mut future);
            expect(
                admitted.load(std::sync::atomic::Ordering::SeqCst),
                "Expected admitted to be true after the first poll",
            )?;
            expect(
                calls.load(std::sync::atomic::Ordering::SeqCst) == 1,
                "Expected exactly one callback call",
            )?;
            let result = match first_poll {
                std::task::Poll::Pending => future.await,
                std::task::Poll::Ready(value) => value,
            };
            expect(
                result? == 42,
                "Expected the callback result to be preserved",
            )?;
            // 成功路径：回调结果保留，span 状态为 ok 且已结算。
            let spans = fixture.spans_snapshot().await;
            let success = find_span(&spans, "success")?;
            expect_eq(&success.status, &SpanStatus::Ok)?;
            expect(success.settled, "Expected the span to be settled")?;
            Ok(())
        })
    });
    create_case(
        factory,
        "callback lifecycle",
        "admits once synchronously and preserves the result",
        body,
    )
}

// 回调返回 Err（对应 TS 的 throw / reject）必须原样向上传播，同时 span 结算为 error：
// - `sync-error` / `async-error` 的 rejection 必须逐字段等于预期错误（TS 比较对象身份，
//   Rust 的错误是值类型，只能降级为相等比较）；
// - `undefined-error` 用 message 为 "undefined" 的 `TelemetryError` 代替 JS 的
//   `Promise.reject(undefined)`，因为 `ErasedValue` 的 Err 通道固定为 `TelemetryError`；
// - 三个 span 最终都是 status = error（error 详情不在本 case 的断言范围内）。
// 注意：Rust 的 `start_span` 回调总是返回 future，TS 的“同步 throw / 异步 reject”差异在这里
// 退化为同一种形态；`sync-error` / `async-error` 两个子场景名保留是为了与上游对应。
fn lifecycle_rejection_case(
    factory: &TelemetryAdapterFixtureFactory,
) -> TelemetryAdapterConformanceCase {
    let body: Arc<CaseBody> = Arc::new(|fixture: TelemetryAdapterFixture| {
        Box::pin(async move {
            // 留一份用于比较：闭包按值捕获另一份。TelemetryError 是值类型，没有实例身份。
            let sync_error = TelemetryError::new("sync");
            let sync_expected = sync_error.clone();
            let operation: BoxFuture<'_, Result<ErasedValue, TelemetryError>> =
                Box::pin(fixture.context.start_span(
                    span_options("sync-error"),
                    Box::new(move |_| {
                        let error = sync_error.clone();
                        Box::pin(async move { Err(error) })
                    }),
                ));
            rejects_with_same_value(operation, &sync_expected).await?;

            let async_error = TelemetryError::new("async");
            let async_expected = async_error.clone();
            let operation: BoxFuture<'_, Result<ErasedValue, TelemetryError>> =
                Box::pin(fixture.context.start_span(
                    span_options("async-error"),
                    Box::new(move |_| {
                        let error = async_error.clone();
                        Box::pin(async move { Err(error) })
                    }),
                ));
            rejects_with_same_value(operation, &async_expected).await?;

            // `undefined` 没有 Rust 对应值；用可识别的哨兵错误走同一条 rejection 通道。
            let undefined_error = TelemetryError::new("undefined");
            let undefined_expected = undefined_error.clone();
            let operation: BoxFuture<'_, Result<ErasedValue, TelemetryError>> =
                Box::pin(fixture.context.start_span(
                    span_options("undefined-error"),
                    Box::new(move |_| Box::pin(async move { Err(undefined_error.clone()) })),
                ));
            rejects_with_same_value(operation, &undefined_expected).await?;

            let spans = fixture.spans_snapshot().await;
            for name in ["sync-error", "async-error", "undefined-error"] {
                let span = find_span(&spans, name)?;
                expect(
                    matches!(span.status, SpanStatus::Error { .. }),
                    "Expected the recorded status to be error",
                )?;
            }
            Ok(())
        })
    });
    create_case(
        factory,
        "callback lifecycle",
        "preserves synchronous and asynchronous rejection values",
        body,
    )
}

// 状态遵循“最后一次显式写入优先”；只有回调以 Err 结束且从未显式设置状态时，适配器才自动写
// error（对应 TS 的 `try/catch/finally`）：
// - last-status：先 error 后 ok → 最终 ok；
// - explicit-before-throw：显式 ok 后返回 Err → 仍是 ok（自动 error 被抑制）；
// - explicit-before-rejection：显式 error 后返回 Err → 保留显式的 error 详情；
// - expected-failure：显式 error 后正常返回 false → 保留 error（返回值不是 rejection）。
fn status_last_explicit_case(
    factory: &TelemetryAdapterFixtureFactory,
) -> TelemetryAdapterConformanceCase {
    let body: Arc<CaseBody> = Arc::new(|fixture: TelemetryAdapterFixture| {
        Box::pin(async move {
            fixture
                .context
                .start_span_typed(span_options("last-status"), |span| {
                    Box::pin(async move {
                        span.set_status(error_status(Some("Expected"), Some("first")));
                        span.set_status(SpanStatus::Ok);
                    })
                })
                .await?;

            // 显式 ok 之后再失败：自动 error 不得覆盖显式值。
            let thrown = TelemetryError::new("after explicit status");
            let thrown_expected = thrown.clone();
            let operation: BoxFuture<'_, Result<ErasedValue, TelemetryError>> =
                Box::pin(fixture.context.start_span(
                    span_options("explicit-before-throw"),
                    Box::new(move |span| {
                        span.set_status(SpanStatus::Ok);
                        let error = thrown.clone();
                        Box::pin(async move { Err(error) })
                    }),
                ));
            rejects_with_same_value(operation, &thrown_expected).await?;

            // 显式 error 之后再失败：保留显式的 error 详情，不用 rejection 的 message 重写。
            let rejected = TelemetryError::new("after async explicit status");
            let rejected_expected = rejected.clone();
            let operation: BoxFuture<'_, Result<ErasedValue, TelemetryError>> =
                Box::pin(fixture.context.start_span(
                    span_options("explicit-before-rejection"),
                    Box::new(move |span| {
                        span.set_status(error_status(Some("Expected"), Some("async failure")));
                        let error = rejected.clone();
                        Box::pin(async move { Err(error) })
                    }),
                ));
            rejects_with_same_value(operation, &rejected_expected).await?;

            // 显式 error 后正常返回 false：这是“预期失败”，不是 rejection，状态不再改写。
            fixture
                .context
                .start_span_typed(span_options("expected-failure"), |span| {
                    Box::pin(async move {
                        span.set_status(error_status(Some("Expected"), Some("returned failure")));
                        false
                    })
                })
                .await?;

            let spans = fixture.spans_snapshot().await;
            expect_eq(&find_span(&spans, "last-status")?.status, &SpanStatus::Ok)?;
            expect_eq(
                &find_span(&spans, "explicit-before-throw")?.status,
                &SpanStatus::Ok,
            )?;
            expect_eq(
                &find_span(&spans, "explicit-before-rejection")?.status,
                &error_status(Some("Expected"), Some("async failure")),
            )?;
            expect_eq(
                &find_span(&spans, "expected-failure")?.status,
                &error_status(Some("Expected"), Some("returned failure")),
            )?;
            Ok(())
        })
    });
    create_case(
        factory,
        "status",
        "uses last explicit status without automatic overwrite",
        body,
    )
}

// 属性写入是“合并”而不是“替换”：
// - 起始属性与两次 set_attributes 合并；同名键原地覆盖，未出现的键保留；
// - 键序按首次插入（start、overwrite、count），对应 JS 对象的枚举顺序；
// - 事件按 add_event 调用顺序追加，属性是记录时的副本。
// TS 用例里依赖 `undefined` 的语义（值为 undefined 的键被忽略、已有键不会被 undefined 删除）
// 在 Rust 的 `SpanAttributes` 中无法表达，本移植只保留可表达的部分。
fn recording_merge_case(
    factory: &TelemetryAdapterFixtureFactory,
) -> TelemetryAdapterConformanceCase {
    let body: Arc<CaseBody> = Arc::new(|fixture: TelemetryAdapterFixture| {
        Box::pin(async move {
            let options = SpanOptions {
                name: "recording".to_string(),
                attributes: Some(attributes(&[
                    (
                        "start",
                        crate::index::AttributeValue::String("value".into()),
                    ),
                    (
                        "overwrite",
                        crate::index::AttributeValue::String("start".into()),
                    ),
                ])),
                links: None,
            };
            fixture
                .context
                .start_span_typed(options, |span| {
                    Box::pin(async move {
                        span.set_attributes(attributes(&[
                            ("count", crate::index::AttributeValue::Number(1.0)),
                            (
                                "overwrite",
                                crate::index::AttributeValue::String("middle".into()),
                            ),
                        ]));
                        // 第二次合并只带 overwrite：count 必须保留，overwrite 覆盖为 "end"。
                        span.set_attributes(attributes(&[(
                            "overwrite",
                            crate::index::AttributeValue::String("end".into()),
                        )]));
                        span.add_event(
                            "first",
                            Some(attributes(&[(
                                "index",
                                crate::index::AttributeValue::Number(1.0),
                            )])),
                        );
                        span.add_event(
                            "second",
                            Some(attributes(&[(
                                "index",
                                crate::index::AttributeValue::Number(2.0),
                            )])),
                        );
                    })
                })
                .await?;

            let spans = fixture.spans_snapshot().await;
            let span = find_span(&spans, "recording")?;
            let expected_attributes = attributes(&[
                (
                    "start",
                    crate::index::AttributeValue::String("value".into()),
                ),
                (
                    "overwrite",
                    crate::index::AttributeValue::String("end".into()),
                ),
                ("count", crate::index::AttributeValue::Number(1.0)),
            ]);
            expect_eq(&span.attributes, &expected_attributes)?;
            // 键序单独断言：`expect_eq` 对 IndexMap 忽略顺序，锁不住 JS 的枚举顺序。
            let keys: Vec<&str> = span.attributes.keys().map(String::as_str).collect();
            expect_eq(&keys, &vec!["start", "overwrite", "count"])?;
            let expected_events = vec![
                crate::memory::RecordedTelemetryEvent {
                    name: "first".to_string(),
                    attributes: attributes(&[("index", crate::index::AttributeValue::Number(1.0))]),
                },
                crate::memory::RecordedTelemetryEvent {
                    name: "second".to_string(),
                    attributes: attributes(&[("index", crate::index::AttributeValue::Number(2.0))]),
                },
            ];
            expect_eq(&span.events, &expected_events)?;
            Ok(())
        })
    });
    create_case(
        factory,
        "recording",
        "merges attributes and records ordered events",
        body,
    )
}

// 结算后的 span 在记录层面退化为惰性对象：
// - set_attributes / add_event / set_status 全部静默忽略，快照保持结算时的值；
// - 在它上面启动子 span 仍会“接纳”回调并返回结果，但不产生任何新记录。
// 这区分了“span 不再是记录目标”与“span 不可用”：已结算的 span 仍可作为 context 使用。
fn recording_inert_case(
    factory: &TelemetryAdapterFixtureFactory,
) -> TelemetryAdapterConformanceCase {
    let body: Arc<CaseBody> = Arc::new(|fixture: TelemetryAdapterFixture| {
        Box::pin(async move {
            let (settled_tx, settled_rx) =
                tokio::sync::oneshot::channel::<Arc<dyn TelemetrySpan>>();
            let options = SpanOptions {
                name: "settled".to_string(),
                attributes: Some(attributes(&[(
                    "value",
                    crate::index::AttributeValue::String("initial".into()),
                )])),
                links: None,
            };
            fixture
                .context
                .start_span_typed(options, move |span| {
                    Box::pin(async move {
                        let _ = settled_tx.send(span);
                    })
                })
                .await?;
            let captured = settled_rx
                .await
                .map_err(|_| TelemetryError::new("Expected callback span"))?;

            captured.set_attributes(attributes(&[(
                "value",
                crate::index::AttributeValue::String("late".into()),
            )]));
            captured.add_event(
                "late",
                Some(attributes(&[(
                    "value",
                    crate::index::AttributeValue::Boolean(true),
                )])),
            );
            captured.set_status(SpanStatus::Error { error: None });
            // 子 span 走惰性路径：回调执行并返回 7，但适配器不得记录它。
            let admitted = Arc::new(std::sync::atomic::AtomicBool::new(false));
            let admitted_for_cb = Arc::clone(&admitted);
            let mut child = Box::pin(captured.start_span_typed(
                span_options("late-child"),
                move |_| {
                    admitted_for_cb.store(true, std::sync::atomic::Ordering::SeqCst);
                    Box::pin(async move { 7 })
                },
            ));
            let first_poll = poll_once(&mut child);
            expect(
                admitted.load(std::sync::atomic::Ordering::SeqCst),
                "Expected the late child callback to be admitted",
            )?;
            let result = match first_poll {
                std::task::Poll::Pending => child.await,
                std::task::Poll::Ready(value) => value,
            };
            expect(result? == 7, "Expected the late child result")?;

            let spans = fixture.spans_snapshot().await;
            expect(
                spans.len() == 1,
                "Expected no span to be recorded for the late child",
            )?;
            let first = &spans[0];
            expect_eq(
                &first.attributes,
                &attributes(&[(
                    "value",
                    crate::index::AttributeValue::String("initial".into()),
                )]),
            )?;
            expect_eq(
                &first.events,
                &Vec::<crate::memory::RecordedTelemetryEvent>::new(),
            )?;
            expect_eq(&first.status, &SpanStatus::Ok)?;
            Ok(())
        })
    });
    create_case(
        factory,
        "recording",
        "makes calls after settlement inert",
        body,
    )
}

// 并发子 span 的父级与结算顺序：
// - 两个子 span 的 parent_id 都指向 parent，parent 自己是根；
// - second 同步完成，first 被信号量 gate 阻塞，因此 second 先结算；
// - end_sequence 是结算序号，必须严格递增：second < first < parent。
// Rust 的 future 是惰性的，所以 first 必须 `tokio::spawn` 才会开始执行并注册 span；
// gate 保证 first 一定在 second 结算之后才被放行。
fn parentage_case(factory: &TelemetryAdapterFixtureFactory) -> TelemetryAdapterConformanceCase {
    let body: Arc<CaseBody> = Arc::new(|fixture: TelemetryAdapterFixture| {
        Box::pin(async move {
            let gate = Arc::new(tokio::sync::Semaphore::new(0));
            let gate_for_first = Arc::clone(&gate);
            fixture
                .context
                .start_span_typed(span_options("parent"), move |parent| {
                    let gate = Arc::clone(&gate_for_first);
                    Box::pin(async move {
                        let first_parent = Arc::clone(&parent);
                        let first_gate = Arc::clone(&gate);
                        let first = async move {
                            first_parent
                                .start_span_typed(span_options("first-child"), move |_| {
                                    let gate = Arc::clone(&first_gate);
                                    Box::pin(async move {
                                        let _permit = gate.acquire().await.expect("semaphore");
                                    })
                                })
                                .await
                        };
                        // TS: `const first = parent.startSpan(...)` 立即开始，随后 `await second`，
                        // 释放 gate，最后 `await first`。Rust 的 future 惰性，因此先 spawn 让 first
                        // 记录并阻塞在 gate 上，再按同样的顺序推进。
                        let first_task = tokio::spawn(first);
                        let second_parent = Arc::clone(&parent);
                        let second_result = second_parent
                            .start_span_typed(span_options("second-child"), |_| {
                                Box::pin(async move { "done" })
                            })
                            .await;
                        second_result?;
                        gate.add_permits(1);
                        first_task
                            .await
                            .map_err(|error| TelemetryError::new(error.to_string()))??;
                        Ok::<(), TelemetryError>(())
                    })
                })
                .await??;
            // `first` 已在父回调内被 await（tokio::spawn + join 已覆盖）。

            let spans = fixture.spans_snapshot().await;
            let parent = find_span(&spans, "parent")?;
            let first = find_span(&spans, "first-child")?;
            let second = find_span(&spans, "second-child")?;
            expect_eq(&parent.parent_id, &None)?;
            expect_eq(&first.parent_id, &Some(parent.id))?;
            expect_eq(&second.parent_id, &Some(parent.id))?;
            // end_sequence 只在结算后出现；三个 span 都应当已经结算。
            let (Some(second_sequence), Some(first_sequence), Some(parent_sequence)) =
                (second.end_sequence, first.end_sequence, parent.end_sequence)
            else {
                return Err(TelemetryError::new("Expected endSequence on every span"));
            };
            expect(
                second_sequence < first_sequence,
                "Expected the second child to settle before the first",
            )?;
            expect(
                first_sequence < parent_sequence,
                "Expected the children to settle before the parent",
            )?;
            Ok(())
        })
    });
    create_case(
        factory,
        "parentage",
        "records nested and concurrent child relationships",
        body,
    )
}
