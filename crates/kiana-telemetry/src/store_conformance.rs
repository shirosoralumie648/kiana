//! store-conformance.ts（TS 侧位于 `packages/telemetry/test/`，不是包导出）的 Rust 版。
//!
//! 六条共享读store检查，不含原始测量或后端持久化的假设；内存 store、chord 的 SQLite store、
//! durable 的 JSONL store 都应能注册同一组 case（ARCHITECTURE.md §4.8 第 4 条）。
//!
//! 与 TS 的差异（有意为之，见 spec §8）：
//! - `vi.spyOn(Date, "now")` → 由 fixture 自身提供时钟；本模块不注入时间。
//! - `await using` → [`StoreFixture::dispose`](crate::store_conformance::StoreFixture::dispose)；runner 在 case 结束后调用。
//! - 非字符串 carrier 值（`{...carrier, key: 42}`）在 Rust 的 `TelemetryCarrier`（字符串 map）中
//!   不可表示（冻结的 `TelemetryPropagation::extract` 只收 `Option<&TelemetryCarrier>`），
//!   因此由 fixture 的 [`StoreFixture::extract_non_string`](crate::store_conformance::StoreFixture::extract_non_string) 钩子提供该入口；钩子存在时断言
//!   `keys * 2` 条 fallback（与 TS 相同），缺失时只跑「缺失键」一半并断言 `keys` 条。
//!
//! TS: `packages/telemetry/test/store-conformance.ts`
//!
//! # 这个文件为什么存在
//!
//! 每个 store 适配器（内存 store、chord 的 SQLite store、durable 的 JSONL store）都必须在同一
//! 条契约下回答 `find_traces` / `get_trace` / `find_metrics` 三组问题。把检查集中在这里而不是
//! 复制进各适配器的 `tests/`，可以让所有实现跑同一组断言，也避免每个适配器各自解释一遍
//! 「settled 过滤、快照分离、游标分页、类型化属性相等」这些语义。
//!
//! # 执行流程
//!
//! 1. 调用方提供一个 [`StoreFixtureFactory`]；工厂每次被调用都应创建一个隔离的 [`StoreFixture`]。
//! 2. [`telemetry_store_conformance`] 用同一个工厂组装六个 [`StoreConformanceCase`]，
//!    此时不创建任何 fixture，也不做 IO。
//! 3. runner 逐条调用 `case.run`：向工厂要 fixture → 跑断言体 → `dispose`。断言失败以
//!    [`TelemetryError`] 返回（消息尽量保持 TS 原文，便于两边对照）。
//! 4. 六个 case 的标题是稳定契约；runner 可以用它做报告与过滤，改动标题需同步测试。
//!
//! # 共享 fixture 契约（本模块的隐含前提）
//!
//! - `context.metrics()` / `context.propagation()` 必须返回 `Some`；
//!   套件的 metrics/propagation 断言依赖这一点，缺失时直接 panic。
//! - `store` 可以是 context 本身（内存适配器如此），也可以是独立后端。
//! - `set_now` 必须影响**后续**记录的时间戳。若它是 no-op，第 1 条的 `started_ms == 20_000`
//!   与第 2 条的 `start_min` / `start_max` 断言会失败；第 6 条依赖它把测量固定到
//!   20_000/30_000 桶，同样无法按预期命中；第 3 条不会因 no-op 失败，但不再保证 25 条 trace
//!   的起点固定在 20_000/20_001，从而失去「起点全部并列」这一测试场景。
//! - 每条 case 在读取 store 前都会先 `flush`，把此前的写入推送到 store；第 1 条还会在 span
//!   未结算时 flush 一次，验证未结算记录仍不可见。套件不假设 `flush` 具体刷新什么。
//! - `extract_non_string` 缺失不会让第 4 条 case 失败，只会少跑「非字符串值」那一半 fallback
//!   （见该字段的文档）。
//!
//! # 注意事项
//!
//! - 本模块只通过 `crate::index` 的契约类型与 `metrics` 的指标契约访问适配器，不依赖具体
//!   store；新增适配器应复用本套件，而不是另写一份检查。
//! - 第 1 条修改的是从 `find_traces` 返回值 `clone` 出来的 trace（`page.traces.first()?.clone()`），
//!   第 6 条直接修改 `find_metrics` 返回的行；随后都重新读取，验证 store 仍返回原值。第 1 条
//!   修改的既然是副本，它锁定的是「重新读取仍是原值」，并不能证明返回值的修改不会回写后端。
//! - span 起止时间与事件时间由适配器时钟决定；套件只断言固定起点、`ended_ms >= started_ms`、
//!   事件时间非负等关系，不规定时钟实现。
//!
//! # 失败模式
//!
//! - 断言失败统一返回 [`TelemetryError`]（消息是 TS 断言的原文或补充说明）；case 在第一个
//!   失败处提前返回，后续断言不再执行。
//! - 工厂错误与 store 查询错误原样向上传播；套件不做重试，也不吞掉错误。
//! - `metrics` / `propagation` 能力缺失会 panic（fixture 违约），不会被降级为跳过。

use std::sync::Arc;

use crate::index::{
    BoxFuture, MetricQuery, SpanAttributes, SpanOptions, StoredMetric, StoredSpan,
    TelemetryContext, TelemetryContextExt, TelemetryError, TelemetryPropagation, TelemetryStore,
    TraceQuery,
};
use crate::metrics::TelemetryMetrics;

/// 非字符串 carrier 值的 `extract` 通道（见 [`StoreFixture::extract_non_string`]）。
///
/// 参数是「原始 carrier」与「要被破坏的键名」；实现应返回把该键的值换成数字 42 后执行
/// `extract` 的结果（TS: `extract({ ...carrier, [key]: 42 })`）。适配器无法表达这种
/// carrier 时返回 `None`，套件会跳过这一半。
///
/// 用 `Arc<dyn Fn ... + Send + Sync>` 包一层，是为了让 [`StoreFixture`] 能 `Clone`
/// （`Arc` 克隆共享同一个闭包），并让 fixture 可以跨 `await` 与 runner 共享。
pub type NonStringExtract = Arc<
    dyn Fn(&crate::index::TelemetryCarrier, &str) -> Option<Arc<dyn TelemetryContext>>
        + Send
        + Sync,
>;

/// 一次 store 一致性检查用的 fixture。
///
/// `context` 同时暴露 metrics 与 propagation（TS 的
/// `TelemetryContext & { metrics; propagation }`）。
///
/// # 契约
///
/// - `context.metrics()` 与 `context.propagation()` 都必须返回 `Some`
///   （套件的 metrics/propagation 断言依赖这一点）。
/// - `store` 通常就是 `context` 的 store 视图（内存适配器让同一个对象实现四个契约），
///   也可以是独立后端；套件只通过 `TelemetryStore` 访问它。
/// - 所有字段都是 `Arc` / `Option`，`clone()` 共享同一份底层状态；`create_case` 把克隆交给
///   断言体，自己保留原 fixture 用于 `dispose`。
///
/// # 扩展点
///
/// 字段是 `pub` 的，下游测试可以替换它们做故障注入：`tests/store.rs` 就用包装过的 store
/// 改写继续 span 的 id、给 `dispose` 计数、包装 `extract_non_string` 记录调用次数。
/// 替换后仍需满足上面的契约。
pub struct StoreFixture {
    /// 启动 span 的入口，同时提供 metrics / propagation 能力。
    pub context: Arc<dyn TelemetryContext>,
    /// 被检查的 store 视图；内存适配器里它与 `context` 指向同一个对象。
    pub store: Arc<dyn TelemetryStore>,
    /// `flush()`：内存实现是 no-op，SQLite 是 commit。
    pub flush: Arc<dyn Fn() -> BoxFuture<'static, ()> + Send + Sync>,
    /// `[Symbol.asyncDispose]()`。
    pub dispose: Arc<dyn Fn() -> BoxFuture<'static, ()> + Send + Sync>,
    /// `vi.spyOn(Date, "now").mockReturnValue(ms)` 的等价物：把适配器的墙钟钉到给定毫秒值。
    ///
    /// 内存 store 通过 `InMemoryTelemetryContext::with_clock` 实现；不控制时间的适配器可传 no-op，
    /// 但那样第 1、2、6 条的时间断言无法按预期成立，第 3 条也会失去「起点全部并列」场景，
    /// 因此实现方应提供可控时钟。
    pub set_now: Arc<dyn Fn(f64) + Send + Sync>,
    /// `extract({ ...carrier, [key]: 42 })`（非字符串 carrier 值）的等价入口。
    ///
    /// TS 的 `TelemetryPropagation.extract` 在运行时接受任意值：任一必需键不是字符串就回退到
    /// 根。Rust 的 [`TelemetryCarrier`](crate::index::TelemetryCarrier) 是 `String` map，
    /// 「值不是字符串」穿不过冻结的 trait 签名，因此由 fixture 提供这条通道：给定原始 carrier
    /// 与被破坏的键，返回该适配器对「同一 carrier 但该键的值是数字 42」执行 `extract` 的结果。
    /// 返回 `None` 表示该适配器无法表达这种 carrier（此时套件只跑「缺失键」一半）。
    pub extract_non_string: Option<NonStringExtract>,
}

/// 手写 `Clone`：字段全是 `Arc`（或 `Option<Arc>`），克隆只复制句柄、不复制底层
/// store/context 状态。这是 `create_case` 能把同一个 fixture 同时交给断言体与 `dispose`
/// 的前提；若换成按值克隆实现，两边就会各自操作一份状态。
impl Clone for StoreFixture {
    fn clone(&self) -> Self {
        StoreFixture {
            context: Arc::clone(&self.context),
            store: Arc::clone(&self.store),
            flush: Arc::clone(&self.flush),
            dispose: Arc::clone(&self.dispose),
            set_now: Arc::clone(&self.set_now),
            extract_non_string: self.extract_non_string.clone(),
        }
    }
}

impl StoreFixture {
    /// 便捷构造：`flush` 与 `dispose` 都是 no-op。
    ///
    /// `set_now` 同样是 no-op、`extract_non_string` 为 `None`。因此它只适合两种情况：
    /// 适配器的墙钟本来就满足时间断言，或调用方随后自行替换这些字段（字段是 `pub` 的）。
    /// 需要控制时间、或要覆盖非字符串 carrier 分支的适配器，应直接构造结构体字面量。
    pub fn new(context: Arc<dyn TelemetryContext>, store: Arc<dyn TelemetryStore>) -> Self {
        StoreFixture {
            context,
            store,
            flush: Arc::new(|| Box::pin(async {})),
            dispose: Arc::new(|| Box::pin(async {})),
            set_now: Arc::new(|_| {}),
            extract_non_string: None,
        }
    }

    /// 把墙钟钉到给定毫秒值（`Date.now()` 的 spy）。
    ///
    /// 套件总在启动 span / 记录指标**之前**调用；已记录的时间戳不会因此改变。
    pub fn set_now_ms(&self, ms: f64) {
        (self.set_now)(ms);
    }

    /// 上下文上的 metrics 能力（fixture 契约要求存在）。
    ///
    /// # Panics
    ///
    /// `context.metrics()` 返回 `None` 时 panic —— 这是 fixture 契约违反，不是可恢复错误。
    pub fn metrics(&self) -> Arc<dyn TelemetryMetrics> {
        self.context
            .metrics()
            .expect("store fixtures must expose metrics on the context")
    }

    /// 上下文上的 propagation 能力（fixture 契约要求存在）。
    ///
    /// # Panics
    ///
    /// `context.propagation()` 返回 `None` 时 panic，原因同上。
    pub fn propagation(&self) -> Arc<dyn TelemetryPropagation> {
        self.context
            .propagation()
            .expect("store fixtures must expose propagation on the context")
    }
}

/// `TelemetryStoreFixtureFactory`：为一条 case 创建隔离 fixture 的异步工厂。
///
/// - 每次调用应返回**相互隔离**的 fixture。`remote_ids_case` 会在 runner 的 fixture 之外
///   额外创建 sender/receiver，并用 `receiver.store.get_trace(sent.trace_id)` 只有 1 个继续
///   span 证明 receiver 的 store 里没有 sender 的原始 span；runner 主 fixture 不在该断言范围内。
/// - 返回 `Err` 表示适配器无法创建 fixture；case 以该错误失败（而不是跳过）。
/// - `Send + Sync + 'static` 是 `create_case` 把工厂搬进 `Arc<dyn Fn() -> BoxFuture<'static, _>>`
///   的前提（`'static` 保证闭包不借用调用栈上的数据）。
pub type StoreFixtureFactory =
    Arc<dyn Fn() -> BoxFuture<'static, Result<StoreFixture, TelemetryError>> + Send + Sync>;

/// 一个运行器无关的 store 一致性 case。
///
/// 字段与 `testing` 模块的适配器一致性 case 同形：`group`/`name` 供 runner 报告，`run` 是
/// 可重复调用的异步闭包。`run` 不借用外部数据，因此可以交给任意测试运行器（包括按名称筛选）。
#[derive(Clone)]
pub struct StoreConformanceCase {
    /// 套件分组；本模块固定为 `"store"`。这是 Rust runner 的分组标签（`create_case` 固定写入，
    /// `tests/store.rs` 也断言它）；TS 侧没有对应字段，六个 `it` 由调用方传入的 `describe(name)`
    /// 包裹，实际注册名是 `in-memory telemetry store conformance`。
    pub group: &'static str,
    /// 用例标题，逐字来自 TS 的 `it(...)`；runner 用它报告失败。
    pub name: &'static str,
    /// 执行一次用例：向 factory 要 fixture、跑断言体、最后 dispose。
    ///
    /// 可重复调用；每次调用都会重新创建 fixture，因此同一 case 可以在多个测试里复用。
    pub run: Arc<dyn Fn() -> BoxFuture<'static, Result<(), TelemetryError>> + Send + Sync>,
}

/// 把「fixture 生命周期 + 断言体」封装成可重复运行的 [`StoreConformanceCase`]。
///
/// 生命周期顺序刻意与 TS 的 `await using fixture = await factory()` 对齐：
///
/// 1. `factory().await?` 失败时直接返回该错误（此时没有 fixture 可释放）；
/// 2. 断言体拿到 `fixture.clone()`（共享句柄）；
/// 3. 无论断言体返回 `Ok` 还是 `Err`，都先 `(fixture.dispose)().await` 再返回原结果。
///    `dispose` 返回 `BoxFuture<'static, ()>`，没有错误通道，因此不会覆盖断言结果。
///
/// 注意：这里只覆盖 `Result` 的失败路径；断言体若 panic，Rust 的展开会跳过 dispose。
fn create_case(
    factory: &StoreFixtureFactory,
    name: &'static str,
    test: Arc<dyn Fn(StoreFixture) -> BoxFuture<'static, Result<(), TelemetryError>> + Send + Sync>,
) -> StoreConformanceCase {
    let factory = Arc::clone(factory);
    StoreConformanceCase {
        group: "store",
        name,
        run: Arc::new(move || {
            let factory = Arc::clone(&factory);
            let test = Arc::clone(&test);
            Box::pin(async move {
                let fixture = factory().await?;
                let outcome = test(fixture.clone()).await;
                (fixture.dispose)().await;
                outcome
            })
        }),
    }
}

fn attributes(pairs: &[(&str, crate::index::AttributeValue)]) -> SpanAttributes {
    let mut map = SpanAttributes::new();
    for (key, value) in pairs {
        map.insert((*key).to_string(), value.clone());
    }
    map
}

fn span_options(name: &str) -> SpanOptions {
    SpanOptions {
        name: name.to_string(),
        attributes: None,
        links: None,
    }
}

/// 每个 trace 的「第一个 span」的名字。
///
/// 本套件假定（与内存适配器一致）`TracePage.traces[*].spans` 按 `startedMs` 升序、同刻按
/// span id 升序返回；因此 `spans.first()` 就是该 trace 最早的 span，也正是 `TraceQuery::name`
/// 与 `start_min` / `start_max` 的判定对象（见 `TraceQuery` 字段文档）。第 2 条 case 用这张
/// 列表把 store 结果压成可与 TS 的 `expect(...).toEqual([...])` 直接对照的形式。
fn first_span_names(page: &crate::index::TracePage) -> Vec<String> {
    page.traces
        .iter()
        .filter_map(|trace| trace.spans.first().map(|span| span.name.clone()))
        .collect()
}

fn find_trace_span<'a>(spans: &'a [StoredSpan], name: &str) -> Option<&'a StoredSpan> {
    spans.iter().find(|span| span.name == name)
}

/// `telemetryStoreConformance(factory)`：六条共享检查（标题逐字）。
///
/// 返回顺序固定，与 TS 的注册顺序一致：
/// 1. settled 快照；2. 过滤；3. 分页；4. 远端 id 与 link；5. 外部 id；6. 指标查询。
///
/// 六个 case 共用同一个 `factory`，但每条 case 在运行时会各自获取新 fixture；本函数只组装
/// 闭包，不调用工厂、不做 IO，因此工厂的错误只会在 `run` 执行时暴露。
pub fn telemetry_store_conformance(factory: StoreFixtureFactory) -> Vec<StoreConformanceCase> {
    vec![
        settled_snapshots_case(&factory),
        filters_case(&factory),
        paging_case(&factory),
        remote_ids_case(&factory),
        external_ids_case(&factory),
        metric_query_case(&factory),
    ]
}

/// case 1：只有**已结算**的 span 可见；快照完整，且修改返回值不影响后端。
///
/// 场景：`root` 记录属性与事件后停在 gate 上（recorded 但未 settled），随后启动 `child`。
/// gate 释放前 store 必须看不到任何 trace；释放并 `flush` 后 root/child 才可读。最后修改
/// 返回快照里的属性与事件属性，重新 `get_trace` 必须仍是原值。
///
/// TS: `returns only settled traces, complete span snapshots, and detached results`
fn settled_snapshots_case(factory: &StoreFixtureFactory) -> StoreConformanceCase {
    let body: Arc<
        dyn Fn(StoreFixture) -> BoxFuture<'static, Result<(), TelemetryError>> + Send + Sync,
    > = Arc::new(|fixture: StoreFixture| {
        Box::pin(async move {
            fixture.set_now_ms(20_000.0);
            let gate = Arc::new(tokio::sync::Semaphore::new(0));
            let gate_for_cb = Arc::clone(&gate);
            let context = Arc::clone(&fixture.context);
            let mut root_options = span_options("root");
            root_options.attributes = Some(attributes(&[(
                "tags",
                crate::index::AttributeValue::StringArray(vec!["original".to_string()]),
            )]));
            // TS: `const active = f.context.startSpan(...)` —— 调用即同步进入回调并 push recorded
            // span，然后停在 `await gate.promise`；`findTraces()` 断言的是**已记录但未结算**的
            // span 被 `settled` 过滤掉。Rust 的 future 惰性：先手动 poll 一次让回调被接纳并停在
            // gate 上（与一致性套件其余 case 的 `poll_once` 手法一致），断言时 span 必然已记录
            // 且仍未结算 —— 否则「空页」只说明一个 span 都没有，`settled` 过滤并未被检验。
            let mut active = Box::pin(async move {
                context
                    .start_span_typed(root_options, move |root| {
                        let gate = Arc::clone(&gate_for_cb);
                        Box::pin(async move {
                            root.add_event(
                                "event",
                                Some(attributes(&[(
                                    "tags",
                                    crate::index::AttributeValue::StringArray(vec![
                                        "original".to_string(),
                                    ]),
                                )])),
                            );
                            let _permit = gate.acquire().await.expect("semaphore");
                            root.start_span_typed(span_options("child"), |_| {
                                Box::pin(async move {})
                            })
                            .await
                        })
                    })
                    .await
            });
            let first_poll = crate::testing::poll_once(&mut active);
            crate::testing::expect(
                first_poll.is_pending(),
                "Expected the active span to wait for the gate",
            )?;
            (fixture.flush)().await;
            // 释放前：只有已结算 span 可见。
            let page = fixture.store.find_traces(None).await?;
            crate::testing::expect(page.traces.is_empty(), "Expected no settled traces yet")?;
            crate::testing::expect(
                fixture.store.get_trace("missing").await?.is_none(),
                "Expected getTrace(\"missing\") to be undefined",
            )?;
            // 释放 gate，让回调继续启动 child 并结算 root。`??` 依次展开外层
            // `start_span_typed` 的 `Result` 与回调返回的 `Result<(), TelemetryError>`。
            gate.add_permits(1);
            active.await??;
            (fixture.flush)().await;

            let page = fixture.store.find_traces(None).await?;
            // 返回的快照归调用方所有；这里先 `clone` 出第一份 trace 再就地修改，改的是副本，
            // 不是 `find_traces` 返回的 `page.traces[0]`。TS 直接修改返回对象，Rust 这版只能
            // 通过随后重新读取来观察「store 仍返回原值」。
            let mut trace = page
                .traces
                .first()
                .ok_or_else(|| TelemetryError::new("Expected one trace"))?
                .clone();
            let root = find_trace_span(&trace.spans, "root")
                .ok_or_else(|| TelemetryError::new("Expected recorded span root"))?
                .clone();
            let child = find_trace_span(&trace.spans, "child")
                .ok_or_else(|| TelemetryError::new("Expected recorded span child"))?
                .clone();
            // 只校验 id 的**形状**：36 字符、由十六进制与连字符组成的 scope，加冒号，再加
            // 非零开头的十进制本地序号（TS 正则 `/^[0-9a-f-]{36}:[1-9][0-9]*$/`）。
            // 套件不要求 scope 是合法 UUID，也不要求序号连续。
            let id_ok = {
                let mut parts = root.id.splitn(2, ':');
                let scope = parts.next().unwrap_or("");
                let span = parts.next().unwrap_or("");
                scope.len() == 36
                    && scope.chars().all(|c| c.is_ascii_hexdigit() || c == '-')
                    && !span.is_empty()
                    && span.chars().all(|c| c.is_ascii_digit())
                    && !span.starts_with('0')
            };
            crate::testing::expect(id_ok, "Expected id to match /^[0-9a-f-]{36}:[1-9][0-9]*$/")?;
            crate::testing::expect_str(&root.trace_id, &root.id)?;
            crate::testing::expect(
                root.parent_id.is_none(),
                "Expected the root to have no parent",
            )?;
            crate::testing::expect_eq(&child.parent_id, &Some(root.id.clone()))?;
            crate::testing::expect_str(&child.trace_id, &root.trace_id)?;
            crate::testing::expect(
                root.started_ms == 20_000.0,
                "Expected startedMs to be 20_000",
            )?;
            crate::testing::expect(
                root.ended_ms >= root.started_ms,
                "Expected endedMs to be at least startedMs",
            )?;
            crate::testing::expect(
                root.events
                    .first()
                    .is_some_and(|event| event.time_ms >= 0.0),
                "Expected a nonnegative event time",
            )?;

            // `root.attributes.tags = ["changed"]; root.events[0].attributes.tags = ["changed"];`
            // 修改的是**返回的快照**（`trace.spans` 里的那一份），修改后重新读取必须仍是原值。
            // 重新查询通过 `get_trace` 走 store，不经过刚才修改的副本。
            let root_index = trace
                .spans
                .iter()
                .position(|span| span.name == "root")
                .ok_or_else(|| TelemetryError::new("Expected recorded span root"))?;
            trace.spans[root_index].attributes = attributes(&[(
                "tags",
                crate::index::AttributeValue::StringArray(vec!["changed".to_string()]),
            )]);
            if let Some(event) = trace.spans[root_index].events.first_mut() {
                event.attributes = attributes(&[(
                    "tags",
                    crate::index::AttributeValue::StringArray(vec!["changed".to_string()]),
                )]);
            }
            let fresh = fixture
                .store
                .get_trace(&trace.trace_id)
                .await?
                .ok_or_else(|| TelemetryError::new("Expected the trace to remain readable"))?;
            let fresh_root = find_trace_span(&fresh.spans, "root")
                .ok_or_else(|| TelemetryError::new("Expected recorded span root"))?;
            crate::testing::expect_eq(
                &fresh_root.attributes,
                &attributes(&[(
                    "tags",
                    crate::index::AttributeValue::StringArray(vec!["original".to_string()]),
                )]),
            )?;
            crate::testing::expect_eq(
                &fresh_root
                    .events
                    .first()
                    .map(|event| event.attributes.clone())
                    .unwrap_or_default(),
                &attributes(&[(
                    "tags",
                    crate::index::AttributeValue::StringArray(vec!["original".to_string()]),
                )]),
            )?;
            Ok(())
        })
    });
    create_case(
        factory,
        "returns only settled traces, complete span snapshots, and detached results",
        body,
    )
}

/// case 2：`find_traces` 的过滤语义。
///
/// 关键点（与 `TraceQuery` 字段文档一致）：
///
/// - `name` / `start_min` / `start_max` 只看 trace 中**最早**的 span（这里是根 span）；
/// - `errors: true` 只要 trace 内**任一** span 为 error 即为真（`failure` 是 `wanted` 的子 span）；
/// - `attributes` 的每个键值必须同时命中**同一个** span，并按类型严格相等
///   （布尔 `true` ≠ 数字 `1`）。
///
/// 结果顺序是起点倒序（新的在前），所以 `start_min: 20_000` 得到 `["new","wanted"]`。
///
/// TS: `filters by first-span name and start, any error, and all attributes on one span`
fn filters_case(factory: &StoreFixtureFactory) -> StoreConformanceCase {
    let body: Arc<
        dyn Fn(StoreFixture) -> BoxFuture<'static, Result<(), TelemetryError>> + Send + Sync,
    > = Arc::new(|fixture: StoreFixture| {
        Box::pin(async move {
            // 三条 trace 的首 span 起点依次是 10_000 / 20_000 / 40_000：
            // `old` 在 10_000 启动，回调内把墙钟推到 30_000 再启动两个子 span；
            // `wanted` 在 20_000 启动，回调内 20_001 启动带 error 的子 span `failure`；
            // `new` 在 40_000 启动。name 与起点过滤只看每条 trace 的首 span，因此这里故意让
            // 首 span 的起点与名字各不相同，再用下面的查询逐项区分。
            fixture.set_now_ms(10_000.0);
            let context = Arc::clone(&fixture.context);
            let fixture_for_old = fixture.clone();
            context
                .start_span_typed(span_options("old"), move |root| {
                    let fixture = fixture_for_old.clone();
                    Box::pin(async move {
                        fixture.set_now_ms(30_000.0);
                        let mut recent = span_options("recent-child");
                        recent.attributes = Some(attributes(&[(
                            "left",
                            crate::index::AttributeValue::Number(1.0),
                        )]));
                        root.start_span_typed(recent, |_| Box::pin(async move {}))
                            .await?;
                        let mut split = span_options("split");
                        split.attributes = Some(attributes(&[(
                            "right",
                            crate::index::AttributeValue::Number(2.0),
                        )]));
                        root.start_span_typed(split, |_| Box::pin(async move {}))
                            .await
                    })
                })
                .await??;
            fixture.set_now_ms(20_000.0);
            let fixture_for_wanted = fixture.clone();
            context
                .start_span_typed(span_options("wanted"), move |root| {
                    let fixture = fixture_for_wanted.clone();
                    Box::pin(async move {
                        fixture.set_now_ms(20_001.0);
                        let mut failure = span_options("failure");
                        let attrs = attributes(&[
                            ("left", crate::index::AttributeValue::Number(1.0)),
                            ("right", crate::index::AttributeValue::Number(2.0)),
                            ("valid", crate::index::AttributeValue::Boolean(true)),
                            (
                                "odd.\"key",
                                crate::index::AttributeValue::String("value".into()),
                            ),
                        ]);
                        failure.attributes = Some(attrs);
                        root.start_span_typed(failure, |child| {
                            Box::pin(async move {
                                child.set_status(crate::index::SpanStatus::Error { error: None })
                            })
                        })
                        .await
                    })
                })
                .await??;
            fixture.set_now_ms(40_000.0);
            let mut new_options = span_options("new");
            new_options.attributes = Some(attributes(&[(
                "valid",
                crate::index::AttributeValue::Number(1.0),
            )]));
            context
                .start_span_typed(new_options, |_| Box::pin(async move {}))
                .await?;
            (fixture.flush)().await;

            // 把一次 `find_traces` 的结果映射成「首 span 名列表」，对应上游断言
            // `expect(...map(t => t.spans[0].name)).toEqual([...])` 的行为。
            let names = |query: Option<TraceQuery>| {
                let store = Arc::clone(&fixture.store);
                async move {
                    let page = store.find_traces(query).await?;
                    Ok::<Vec<String>, TelemetryError>(first_span_names(&page))
                }
            };

            let mut attributes_query = SpanAttributes::new();
            attributes_query.insert(
                "left".to_string(),
                crate::index::AttributeValue::Number(1.0),
            );
            attributes_query.insert(
                "right".to_string(),
                crate::index::AttributeValue::Number(2.0),
            );
            // 组合过滤：name、属性、errors 与起止边界同时命中 `wanted`。属性与 error 都来自
            // 它的子 span `failure`，因此这条也证明属性/错误可以在 trace 内任意 span 上。
            crate::testing::expect_eq(
                &names(Some(TraceQuery {
                    name: Some("wanted".to_string()),
                    attributes: Some(attributes_query.clone()),
                    errors: Some(true),
                    start_min: Some(20_000.0),
                    start_max: Some(20_000.0),
                    limit: None,
                    cursor: None,
                }))
                .await?,
                &vec!["wanted".to_string()],
            )?;
            // name 只看首 span：`failure` 是 `wanted` 的子 span，不是首 span，所以结果为空。
            crate::testing::expect_eq(
                &names(Some(TraceQuery {
                    name: Some("failure".to_string()),
                    ..TraceQuery::default()
                }))
                .await?,
                &Vec::<String>::new(),
            )?;
            // 不带 name：`{left:1,right:2}` 在 `wanted` 的子 span 上命中，说明属性过滤
            // 不要求 span 是首 span；`old` 的两个子 span 各只有其中一个键，不满足「同一 span
            // 上全部相等」。
            crate::testing::expect_eq(
                &names(Some(TraceQuery {
                    attributes: Some(attributes_query),
                    ..TraceQuery::default()
                }))
                .await?,
                &vec!["wanted".to_string()],
            )?;
            // 带引号与点号的键 `odd."key` 按原样字符串比较，不做路径/转义解析。
            let mut odd_query = SpanAttributes::new();
            odd_query.insert(
                "valid".to_string(),
                crate::index::AttributeValue::Boolean(true),
            );
            odd_query.insert(
                "odd.\"key".to_string(),
                crate::index::AttributeValue::String("value".into()),
            );
            crate::testing::expect_eq(
                &names(Some(TraceQuery {
                    attributes: Some(odd_query),
                    ..TraceQuery::default()
                }))
                .await?,
                &vec!["wanted".to_string()],
            )?;
            // 类型化相等：数字 `1` 与布尔 `true` 不相等，因此只命中 `new`（`failure` 上是
            // 布尔 `true` 的 `valid`）。
            let mut valid_number = SpanAttributes::new();
            valid_number.insert(
                "valid".to_string(),
                crate::index::AttributeValue::Number(1.0),
            );
            crate::testing::expect_eq(
                &names(Some(TraceQuery {
                    attributes: Some(valid_number),
                    ..TraceQuery::default()
                }))
                .await?,
                &vec!["new".to_string()],
            )?;
            // trace 内任一 span 为 error 即命中整条 trace（`failure` 置了 error 状态）。
            crate::testing::expect_eq(
                &names(Some(TraceQuery {
                    errors: Some(true),
                    ..TraceQuery::default()
                }))
                .await?,
                &vec!["wanted".to_string()],
            )?;
            // 起点下界包含端点。结果按首 span 起点倒序：40_000 的 `new` 在前，
            // 20_000 的 `wanted` 在后，10_000 的 `old` 被排除。
            crate::testing::expect_eq(
                &names(Some(TraceQuery {
                    start_min: Some(20_000.0),
                    ..TraceQuery::default()
                }))
                .await?,
                &vec!["new".to_string(), "wanted".to_string()],
            )?;
            // 起点上界同样包含端点：20_000 的 `wanted` 与 40_000 的 `new` 都被排除，
            // 只剩 10_000 的 `old`。
            crate::testing::expect_eq(
                &names(Some(TraceQuery {
                    start_max: Some(10_000.0),
                    ..TraceQuery::default()
                }))
                .await?,
                &vec!["old".to_string()],
            )?;
            Ok(())
        })
    });
    create_case(
        factory,
        "filters by first-span name and start, any error, and all attributes on one span",
        body,
    )
}

/// case 3：分页必须完整、无重复，即使 trace 起点完全相同。
///
/// 25 条 trace 的根 span 都固定在 20_000、子 span 固定在 20_001，因此排序并列时必须有稳定
/// 的次序（内存适配器按 trace id 升序）；否则跨页的拼接顺序无法与一次性查询一致。
/// 默认页 20 条 + cursor，续页 5 条且无 cursor；`limit:100` 一次取全。
/// 再用 `name:"root", limit:3` 逐页拉取，拼出的 id 序列必须与一次性查询完全一致 ——
/// 这验证 cursor 在起点并列时不会漏项或重复。
///
/// TS: `pages complete traces without duplicates when trace starts tie`
fn paging_case(factory: &StoreFixtureFactory) -> StoreConformanceCase {
    let body: Arc<
        dyn Fn(StoreFixture) -> BoxFuture<'static, Result<(), TelemetryError>> + Send + Sync,
    > = Arc::new(|fixture: StoreFixture| {
        Box::pin(async move {
            // 25 条 trace：每次把根 span 钉在 20_000、子 span 钉在 20_001，制造「起点全部
            // 并列」的分页压力场景；`index` 属性让每条 trace 可区分。
            for index in 0..25 {
                fixture.set_now_ms(20_000.0);
                let mut options = span_options("root");
                options.attributes = Some(attributes(&[(
                    "index",
                    crate::index::AttributeValue::Number(index as f64),
                )]));
                let fixture_for_span = fixture.clone();
                fixture
                    .context
                    .start_span_typed(options, move |root| {
                        let fixture = fixture_for_span.clone();
                        Box::pin(async move {
                            fixture.set_now_ms(20_001.0);
                            root.start_span_typed(span_options("child"), |_| {
                                Box::pin(async move {})
                            })
                            .await
                        })
                    })
                    .await??;
            }
            (fixture.flush)().await;

            // 未指定 limit 时默认 20（见 `TraceQuery::limit` 字段文档），25 条必须分成 20 + 5。
            let first = fixture.store.find_traces(None).await?;
            crate::testing::expect(
                first.traces.len() == 20,
                "Expected the default page to hold 20 traces",
            )?;
            crate::testing::expect(
                first.cursor.is_some(),
                "Expected a cursor for the first page",
            )?;
            // 把第一页的 cursor 原样交回；对套件而言 cursor 是不透明字符串，只验证它能接上
            // 后续页，不解释其编码。
            let rest = fixture
                .store
                .find_traces(Some(TraceQuery {
                    cursor: first.cursor.clone(),
                    ..TraceQuery::default()
                }))
                .await?;
            crate::testing::expect(
                rest.traces.len() == 5,
                "Expected the second page to hold 5 traces",
            )?;
            crate::testing::expect(rest.cursor.is_none(), "Expected no cursor on the last page")?;
            let all = fixture
                .store
                .find_traces(Some(TraceQuery {
                    limit: Some(100),
                    ..TraceQuery::default()
                }))
                .await?
                .traces;
            // 两页拼接的 trace id 序列必须与 `limit:100` 的全量结果同序、无重复无遗漏
            // （本断言只比较 trace id，不逐字段比较 trace 内容）。
            let mut concatenated = first.traces.clone();
            concatenated.extend(rest.traces.clone());
            crate::testing::expect_eq(
                &concatenated
                    .iter()
                    .map(|trace| trace.trace_id.clone())
                    .collect::<Vec<_>>(),
                &all.iter()
                    .map(|trace| trace.trace_id.clone())
                    .collect::<Vec<_>>(),
            )?;
            crate::testing::expect(
                all.iter().all(|trace| trace.spans.len() == 2),
                "Expected every trace to hold two spans",
            )?;

            // 再按 limit:3 逐页拉到 cursor 为空；拼接出的 id 序列必须等于上面的全量序列。
            let mut ids: Vec<String> = Vec::new();
            let mut cursor: Option<String> = None;
            loop {
                let page = fixture
                    .store
                    .find_traces(Some(TraceQuery {
                        name: Some("root".to_string()),
                        limit: Some(3),
                        cursor: cursor.clone(),
                        ..TraceQuery::default()
                    }))
                    .await?;
                ids.extend(page.traces.iter().map(|trace| trace.trace_id.clone()));
                cursor = page.cursor;
                if cursor.is_none() {
                    break;
                }
            }
            crate::testing::expect_eq(
                &ids,
                &all.iter()
                    .map(|trace| trace.trace_id.clone())
                    .collect::<Vec<_>>(),
            )?;
            Ok(())
        })
    });
    create_case(
        factory,
        "pages complete traces without duplicates when trace starts tie",
        body,
    )
}

/// case 4：跨 store 的远端 trace 延续与 link。
///
/// 流程：
///
/// 1. sender 启动 `sender`，在 span 仍活跃时用 propagation 注入 carrier；
/// 2. carrier 经 JSON 往返（模拟 TS 里跨传输后的普通对象）后在 receiver 侧 `extract`；
/// 3. receiver 以远端父上下文启动 `receiver`：继承 trace id 与 parent id，但本地 span id 新分配；
/// 4. receiver 另起 `linked`，link 指向远端上下文（跨 trace 引用，不并入对方 trace）；
/// 5. 对 carrier 的每个键分别做「删除该键」与（钩子存在时）「值换成数字 42」的破坏，
///    两种都必须回退成根 span（缺少任一必需键就整体回退，不存在部分识别）。
///
/// 生命周期：本 case 自建 sender/receiver 两个 fixture，按 `await using` 的逆序
/// （receiver 先、sender 后）释放，失败路径同样释放；主 fixture 由 `create_case` 释放。
///
/// TS: `inherits remote trace IDs and preserves remote parents and cross-trace links`
fn remote_ids_case(factory: &StoreFixtureFactory) -> StoreConformanceCase {
    let factory_for_body = Arc::clone(factory);
    let body: Arc<
        dyn Fn(StoreFixture) -> BoxFuture<'static, Result<(), TelemetryError>> + Send + Sync,
    > = Arc::new(move |fixture: StoreFixture| {
        let factory = Arc::clone(&factory_for_body);
        Box::pin(async move {
            fixture.set_now_ms(20_000.0);
            // TS: `await using sender = await factory(); await using receiver = await factory()`。
            // 两个额外 fixture 由本 case 自己释放（含失败路径），见本函数末尾。
            let sender = factory().await?;
            let receiver = match factory().await {
                Ok(receiver) => receiver,
                Err(error) => {
                    (sender.dispose)().await;
                    return Err(error);
                }
            };
            let outcome: Result<(), TelemetryError> = async {
                // carrier 必须在 span 回调内部产生：回调是 `'static`、不能借用外部变量，
                // 所以用 oneshot 把注入结果送出来；inject 在回调返回前执行，此时 span 尚未结算。
                let (carrier_tx, carrier_rx) =
                    tokio::sync::oneshot::channel::<Option<crate::index::TelemetryCarrier>>();
                let sender_propagation = sender.propagation();
                sender
                    .context
                    .start_span_typed(span_options("sender"), move |span| {
                        let propagation = Arc::clone(&sender_propagation);
                        Box::pin(async move {
                            let context: Arc<dyn TelemetryContext> = span;
                            let _ = carrier_tx.send(propagation.inject(context.as_ref()));
                        })
                    })
                    .await?;
                let carrier = carrier_rx
                    .await
                    .map_err(|_| TelemetryError::new("Expected a carrier"))?
                    .ok_or_else(|| TelemetryError::new("Expected a carrier"))?;
                // flush 之后再读，确保记录已对 store 可见（内存实现是 no-op，SQLite 实现是 commit）。
                (sender.flush)().await;
                let sent = sender
                    .store
                    .find_traces(None)
                    .await?
                    .traces
                    .into_iter()
                    .next()
                    .ok_or_else(|| TelemetryError::new("Expected one sender trace"))?;

                // JSON 往返模拟 carrier 经过一次跨进程传输，同时验证键值可无损序列化。
                let receiver_propagation = receiver.propagation();
                let remote = receiver_propagation
                    .extract(Some(&crate::testing::json_roundtrip_carrier(&carrier)));
                remote
                    .start_span_typed(span_options("receiver"), |_| Box::pin(async move {}))
                    .await?;
                // link 只保存远端的 trace/span 标识与属性，不会把 receiver 并进远端的 trace。
                receiver
                    .context
                    .start_span_typed(
                        SpanOptions {
                            name: "linked".to_string(),
                            attributes: None,
                            links: Some(vec![crate::index::SpanLink {
                                context: remote.span_context_id(),
                                attributes: Some(attributes(&[(
                                    "reason",
                                    crate::index::AttributeValue::String("background".into()),
                                )])),
                            }]),
                        },
                        |_| Box::pin(async move {}),
                    )
                    .await?;
                (receiver.flush)().await;

                let continued =
                    receiver
                        .store
                        .get_trace(&sent.trace_id)
                        .await?
                        .ok_or_else(|| {
                            TelemetryError::new("Expected the remote trace to be continued")
                        })?;
                crate::testing::expect(
                    continued.spans.len() == 1,
                    "Expected exactly one continued span",
                )?;
                let continued_span = &continued.spans[0];
                crate::testing::expect_str(&continued_span.trace_id, &sent.trace_id)?;
                let sent_root_id = sent
                    .spans
                    .first()
                    .map(|span| span.id.clone())
                    .ok_or_else(|| TelemetryError::new("Expected the sender root span"))?;
                crate::testing::expect_eq(&continued_span.parent_id, &Some(sent_root_id.clone()))?;
                crate::testing::expect_str(&continued_span.name, "receiver")?;
                // TS: `expect(continued?.spans[0]?.id).not.toBe(sent.spans[0]!.id)` —— 继续的 span 必须
                // 得到新的本地 id（trace id 与 parent id 继承自远端，本地身份不继承）。
                crate::testing::expect(
                    continued_span.id != sent_root_id,
                    "Expected the continued span to get a fresh local id",
                )?;

                let linked_trace = receiver
                    .store
                    .find_traces(Some(TraceQuery {
                        name: Some("linked".to_string()),
                        ..TraceQuery::default()
                    }))
                    .await?
                    .traces
                    .into_iter()
                    .next()
                    .ok_or_else(|| TelemetryError::new("Expected the linked trace"))?;
                crate::testing::expect(
                    linked_trace.trace_id != sent.trace_id,
                    "Expected the linked trace to have its own trace id",
                )?;
                let linked_span = linked_trace
                    .spans
                    .first()
                    .ok_or_else(|| TelemetryError::new("Expected the linked span"))?;
                crate::testing::expect_eq(
                    &linked_span.links,
                    &vec![crate::index::StoredSpanLink {
                        trace_id: sent.trace_id.clone(),
                        span_id: sent_root_id,
                        attributes: Some(attributes(&[(
                            "reason",
                            crate::index::AttributeValue::String("background".into()),
                        )])),
                    }],
                )?;

                // 每个 carrier 键两轮：删掉该键，以及把该键的值换成数字 42（非字符串）。
                // TS: `for (const key of Object.keys(carrier)) { …extract(deleted)…; …extract(numeric)… }`
                // 并断言 `fallbacks.length === keys * 2`。
                //
                // Rust 的 `TelemetryCarrier` 是 `String` map，「值不是字符串」穿不过冻结的 trait
                // 签名，所以非字符串那一半走 fixture 的 `extract_non_string` 钩子（见字段文档）；
                // 钩子缺失时只跑缺失键一半，期望数相应为 `keys`。
                let keys: Vec<String> = carrier.keys().cloned().collect();
                let mut non_string_rounds = 0usize;
                for key in &keys {
                    let mut incomplete = carrier.clone();
                    incomplete.shift_remove(key);
                    receiver_propagation
                        .extract(Some(&incomplete))
                        .start_span_typed(span_options("fallback"), |_| Box::pin(async move {}))
                        .await?;
                    if let Some(parent) = receiver
                        .extract_non_string
                        .as_ref()
                        .and_then(|extract| extract(&carrier, key))
                    {
                        parent
                            .start_span_typed(span_options("fallback"), |_| Box::pin(async move {}))
                            .await?;
                        non_string_rounds += 1;
                    }
                }
                (receiver.flush)().await;
                let fallbacks = receiver
                    .store
                    .find_traces(Some(TraceQuery {
                        name: Some("fallback".to_string()),
                        ..TraceQuery::default()
                    }))
                    .await?
                    .traces;
                // 计数规则：有钩子时每键两条（删除键 + 非字符串值），共 `keys * 2`；
                // 无钩子时只有删除键那一半，共 `keys`。钩子只覆盖部分键视为 fixture 违约。
                let expected = if non_string_rounds > 0 {
                    crate::testing::expect(
                        non_string_rounds == keys.len(),
                        "Expected the non-string carrier hook to cover every key",
                    )?;
                    keys.len() * 2
                } else {
                    keys.len()
                };
                crate::testing::expect(
                    fallbacks.len() == expected,
                    "Expected one fallback trace per deleted or non-string carrier value",
                )?;
                // 回退得到的必须是新根 span：没有 parent，且 trace id 等于自身 id。
                crate::testing::expect(
                    fallbacks.iter().all(|trace| {
                        trace.spans.first().is_some_and(|span| {
                            span.parent_id.is_none() && span.trace_id == span.id
                        })
                    }),
                    "Expected every fallback to be a recorded root",
                )?;
                Ok(())
            }
            .await;
            // `await using` 的逆序释放：receiver 先、sender 后；失败时同样执行。
            (receiver.dispose)().await;
            (sender.dispose)().await;
            outcome
        })
    });
    create_case(
        factory,
        "inherits remote trace IDs and preserves remote parents and cross-trace links",
        body,
    )
}

/// case 5：接受任意外部字符串身份，不强制 ID 格式。
///
/// 把 carrier 的每个值替换成 `external-<key>` 后 `extract`，再 `inject` 必须得到同一组键值
/// （身份被当作不透明字符串保存）；以该父上下文启动的 span，其 trace id 与 parent id 都保留
/// `external-` 前缀，说明传播层与 store 没有重新格式化、也没有拒绝非本地 id。
///
/// TS: `accepts external string identities without imposing an ID format`
fn external_ids_case(factory: &StoreFixtureFactory) -> StoreConformanceCase {
    let body: Arc<
        dyn Fn(StoreFixture) -> BoxFuture<'static, Result<(), TelemetryError>> + Send + Sync,
    > = Arc::new(|fixture: StoreFixture| {
        Box::pin(async move {
            let propagation = fixture.propagation();
            let (carrier_tx, carrier_rx) =
                tokio::sync::oneshot::channel::<Option<crate::index::TelemetryCarrier>>();
            let capture = Arc::clone(&propagation);
            fixture
                .context
                .start_span_typed(span_options("source"), move |span| {
                    let propagation = Arc::clone(&capture);
                    Box::pin(async move {
                        let context: Arc<dyn TelemetryContext> = span;
                        let _ = carrier_tx.send(propagation.inject(context.as_ref()));
                    })
                })
                .await?;
            let carrier = carrier_rx
                .await
                .map_err(|_| TelemetryError::new("Expected a carrier"))?
                .ok_or_else(|| TelemetryError::new("Expected a carrier"))?;
            // 键不变、值全部换成任意外部字符串，模拟其他系统生成的 id。
            let mut external = crate::index::TelemetryCarrier::new();
            for key in carrier.keys() {
                external.insert(key.clone(), format!("external-{key}"));
            }
            let parent = propagation.extract(Some(&external));
            // extract 识别出的父上下文必须能再次 inject 出同一 carrier：外部字符串原样往返。
            crate::testing::expect_eq(
                &propagation.inject(parent.as_ref()),
                &Some(external.clone()),
            )?;
            parent
                .start_span_typed(span_options("continued"), |_| Box::pin(async move {}))
                .await?;
            (fixture.flush)().await;
            let trace = fixture
                .store
                .find_traces(Some(TraceQuery {
                    name: Some("continued".to_string()),
                    ..TraceQuery::default()
                }))
                .await?
                .traces
                .into_iter()
                .next()
                .ok_or_else(|| TelemetryError::new("Expected the continued trace"))?;
            crate::testing::expect(
                trace.trace_id.starts_with("external-"),
                "Expected the external trace id to be preserved",
            )?;
            let span = trace
                .spans
                .first()
                .ok_or_else(|| TelemetryError::new("Expected the continued span"))?;
            crate::testing::expect(
                span.parent_id
                    .as_deref()
                    .is_some_and(|id| id.starts_with("external-")),
                "Expected the external parent id to be preserved",
            )?;
            Ok(())
        })
    });
    create_case(
        factory,
        "accepts external string identities without imposing an ID format",
        body,
    )
}

/// case 6：指标查询的名称、区间、类型化属性相等与快照分离。
///
/// 指标按 10_000 ms 时间桶归档（spec: `intervalMs = Math.floor(Date.now() / 10_000) * 10_000`），
/// 因此 20_001 的测量落在 20_000 桶、30_001 的落在 30_000 桶。断言覆盖：
///
/// - 名称与属性的组合过滤（`other` 指标、`write` 属性被排除）；
/// - 区间边界**包含**端点（`start_min` / `start_max` 都是闭区间，比较对象是 `interval_ms`）；
/// - 属性按类型严格相等（`valid: 1` 只命中 `Number(1)`，不命中 `Boolean(true)`）；
/// - 返回行是快照，修改后重新查询不受影响。
///
/// TS: `queries metric names, inclusive interval bounds, typed attribute equality, and detached values`
fn metric_query_case(factory: &StoreFixtureFactory) -> StoreConformanceCase {
    let body: Arc<
        dyn Fn(StoreFixture) -> BoxFuture<'static, Result<(), TelemetryError>> + Send + Sync,
    > = Arc::new(|fixture: StoreFixture| {
        Box::pin(async move {
            // 记录时刻 20_001：`hits` 的前三笔测量与 `other` 的 100 都落在 20_000 桶。
            fixture.set_now_ms(20_001.0);
            let metrics = fixture.metrics();
            let counter = metrics.counter(
                "hits",
                Some(crate::metrics::InstrumentOptions {
                    description: None,
                    unit: Some("By".to_string()),
                }),
            );
            let read_valid = attributes(&[
                (
                    "method",
                    crate::index::AttributeValue::String("read".into()),
                ),
                ("valid", crate::index::AttributeValue::Boolean(true)),
            ]);
            // 第二笔与第一笔属性相同，但插入顺序相反；后面的 `count == 2` 要求匹配忽略
            // 属性插入顺序（与 TS 对象按值比较一致）。
            let mut valid_then_method =
                attributes(&[("valid", crate::index::AttributeValue::Boolean(true))]);
            valid_then_method.insert(
                "method".to_string(),
                crate::index::AttributeValue::String("read".into()),
            );
            counter.add(2.0, Some(read_valid.clone()));
            counter.add(3.0, Some(valid_then_method));
            counter.add(
                11.0,
                Some(attributes(&[
                    (
                        "method",
                        crate::index::AttributeValue::String("write".into()),
                    ),
                    ("valid", crate::index::AttributeValue::Number(1.0)),
                ])),
            );
            metrics
                .counter("other", None)
                .add(100.0, Some(read_valid.clone()));
            // 30_001 的 7 落在 30_000 桶；之后的 40_001 只用来 flush，不再记录测量。
            fixture.set_now_ms(30_001.0);
            counter.add(7.0, Some(read_valid));
            fixture.set_now_ms(40_001.0);
            (fixture.flush)().await;

            let mut query_attributes = SpanAttributes::new();
            query_attributes.insert(
                "method".to_string(),
                crate::index::AttributeValue::String("read".into()),
            );
            query_attributes.insert(
                "valid".to_string(),
                crate::index::AttributeValue::Boolean(true),
            );
            // 查询只覆盖 `hits` 的 20_000 桶，并要求属性 {method:"read", valid:true}：
            // `other` 名称不同、`write` 属性不同、30_000 桶的 7 与区间不符，都应被排除。
            let query = MetricQuery {
                name: "hits".to_string(),
                start_min: Some(20_000.0),
                start_max: Some(20_000.0),
                attributes: Some(query_attributes),
            };
            let mut rows: Vec<StoredMetric> = fixture.store.find_metrics(query.clone()).await?;
            // 对返回行求和、取极值，而不是要求固定行数：逐笔记录与预聚合的 store 应得到同样结果。
            let count: f64 = rows.iter().map(|row| row.count).sum();
            let sum: f64 = rows.iter().map(|row| row.sum).sum();
            crate::testing::expect(count == 2.0, "Expected two matching measurements")?;
            crate::testing::expect(sum == 5.0, "Expected the matching sum to be 5")?;
            let min = rows.iter().map(|row| row.min).fold(f64::INFINITY, f64::min);
            let max = rows
                .iter()
                .map(|row| row.max)
                .fold(f64::NEG_INFINITY, f64::max);
            crate::testing::expect(min == 2.0 && max == 3.0, "Expected min 2 and max 3")?;
            // 每行都必须是 20_000 桶的 Counter、单位 `By`：说明过滤比较的是 interval_ms。
            crate::testing::expect(
                rows.iter().all(|row| {
                    row.kind == Some(crate::index::MetricKind::Counter)
                        && row.unit.as_deref() == Some("By")
                        && row.interval_ms == 20_000.0
                }),
                "Expected counter rows in the 20000 interval with unit By",
            )?;
            // `rows[0].attributes.method = "changed"`：修改**返回的快照**不影响后续读取。
            if let Some(first) = rows.first_mut() {
                first.attributes.insert(
                    "method".to_string(),
                    crate::index::AttributeValue::String("changed".into()),
                );
            }
            let reread = fixture.store.find_metrics(query.clone()).await?;
            crate::testing::expect_eq(
                &reread
                    .first()
                    .and_then(|row| row.attributes.get("method"))
                    .cloned(),
                &Some(crate::index::AttributeValue::String("read".into())),
            )?;
            // 未知指标名返回空列表，而不是错误。
            crate::testing::expect(
                fixture
                    .store
                    .find_metrics(MetricQuery {
                        name: "missing".to_string(),
                        start_min: None,
                        start_max: None,
                        attributes: None,
                    })
                    .await?
                    .is_empty(),
                "Expected no rows for an unknown metric name",
            )?;
            // 类型化相等：布尔 `true` 与数字 `1` 不相等，因此只累计 11 那笔。
            let mut valid_number = SpanAttributes::new();
            valid_number.insert(
                "valid".to_string(),
                crate::index::AttributeValue::Number(1.0),
            );
            let sum: f64 = fixture
                .store
                .find_metrics(MetricQuery {
                    name: "hits".to_string(),
                    start_min: None,
                    start_max: None,
                    attributes: Some(valid_number),
                })
                .await?
                .iter()
                .map(|row| row.sum)
                .sum();
            crate::testing::expect(
                sum == 11.0,
                "Expected typed attribute equality to select 11",
            )?;
            // start_min 是闭区间下界：只留 30_000 桶的 7。
            let sum: f64 = fixture
                .store
                .find_metrics(MetricQuery {
                    name: "hits".to_string(),
                    start_min: Some(30_000.0),
                    start_max: None,
                    attributes: None,
                })
                .await?
                .iter()
                .map(|row| row.sum)
                .sum();
            crate::testing::expect(sum == 7.0, "Expected the 30000 interval to hold 7")?;
            Ok(())
        })
    });
    create_case(
        factory,
        "queries metric names, inclusive interval bounds, typed attribute equality, and detached values",
        body,
    )
}
