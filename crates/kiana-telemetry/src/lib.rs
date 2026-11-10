//! 对应 packages/telemetry：TelemetryContext/Span/Metrics/Store/Propagation 契约与内存实现。
//!
//! TS 源：packages/telemetry/src/{index,memory,metrics,noop,render}.ts, testing/*
//!
//! 三种入口与 TS 的 package.json `exports` 对应：
//! - crate 根 ≈ `@earendil-works/kiana-telemetry`（契约、no-op、内存适配器、schema/指标辅助）；
//! - [`render`] 子模块 ≈ `./render`（`render_trace`，不在根再导出）；
//! - [`testing`] 子模块 ≈ `./testing`（一致性套件，不在根再导出）。
//!
//! 共享约定见 `ARCHITECTURE.md` §4：本 crate 的公开错误是 [`TelemetryError`]（`Display` == `message`）；
//! 任何进入文本/JSON 的浮点都走 `kiana_json`；`TelemetryCarrier` 是普通字符串 map（durable 会持久化它）。
//!
//! 安全与可移植性（README 的原文承诺）：遥测是诊断而非持久状态，**不得**持久化
//! `TelemetryContext`/`TelemetrySpan`/backend-native trace 对象；`TelemetryCarrier` 可以跨传输或作为
//! 可选的关联性诊断数据，但不是应用状态；本 crate 不提供 ambient current-span 状态。
//! 进程级身份注册表（`memory.rs`）只做身份查询，不参与传播语义。
//!
//! # 这个 crate 解决什么问题
//!
//! 它定义一套**与后端无关**的遥测（telemetry：追踪、指标与跨进程关联）契约：如何开启与结束
//! 一个 span、如何记录指标、如何把当前调用的标识传给另一个进程，以及如何查询已经记录的数据。
//! 上层的模型请求、chord 会话、durable 任务只依赖这里的契约面（trait、数据类型，以及
//! `create_typed_metrics`/`create_typed_span_starter`/`start_span_try` 等辅助函数与 no-op 入口）。
//! 数据最终写到哪里由调用方注入的适配器决定（本仓库内提供的是内存实现与 no-op 实现）。这样
//! 业务代码不绑定某个可观测性厂商，测试也可以换成可断言的内存实现。
//!
//! # 在系统中的位置
//!
//! 依赖方向：本 crate 位于依赖图最底层（L0），在工作区内只依赖共享叶子 `kiana-json`（JS 语义
//! 的数字与字符串输出），不依赖任何业务 crate；`kiana-chord/tests/boundary.rs` 用测试固定了
//! 这条边界（其余依赖是 serde/tokio 等外部 crate）。
//! 主要调用方：
//! - `kiana-ai`：模型请求过程的 span 与指标；
//! - `kiana-chord`：会话上下文的遥测传递；
//! - `kiana-durable`：harness 任务、工具调用与 compaction 的追踪。
//!
//! # 核心概念与类型
//!
//! - [`TelemetryContext`]：创建 span 的入口，并携带可选的 metrics/propagation 能力。
//!   为避免泛型方法破坏对象安全（object safety，trait 可作为 `dyn` 使用），异步回调以
//!   擦除后的 `SpanCallback` 传递；日常使用 [`TelemetryContextExt`] 的泛型辅助方法。
//! - [`TelemetrySpan`]：span 本身，在 context 之上增加 `add_link`/`add_event`/
//!   `set_attributes`/`set_status`。
//! - [`TelemetryStore`]：只读查询面（`find_traces`/`get_trace`/`find_metrics`）。
//! - [`TelemetryPropagation`]：`inject`/`extract`，用 [`TelemetryCarrier`]（字符串 map）
//!   做跨进程关联；carrier 只承载关联信息，不是应用状态。
//! - [`TelemetryMetrics`]：counter / up-down counter / histogram 的同步工厂。
//! - [`InMemoryTelemetryContext`]：把 span 与 measurement 记在进程内的参考实现，同时实现
//!   context/propagation/metrics/store 四个契约，供测试与本地诊断使用。
//! - [`noop`]：丢弃全部记录的单例实现；[`metrics`] 里的 [`TypedMetrics`] 只按 schema 查表
//!   补充 `description`/`unit`/`boundaries`，不校验名字或 kind。
//!
//! # 一次典型调用
//!
//! 1. 调用方持有一个 `Arc<dyn TelemetryContext>`：生产代码由适配器提供，测试通常用
//!    [`InMemoryTelemetryContext`]（需要可控时间时用它的 `with_clock` 构造）。
//! 2. 用 [`TelemetryContextExt::start_span_typed`] 启动 span；回调 future 完成时 span 结算。
//!    注意：回调返回 `Err` 只有走 [`TelemetryContextExt::start_span_try`] 才会被当作“抛出”
//!    并标记为 error（除非回调已显式 `set_status`）；`start_span_typed` 原样返回回调的值
//!    （即使该值本身是 `Result::Err`）。
//! 3. 读取结果：内存实现用 `get_spans()` 取原始快照，或通过 [`TelemetryStore`] 查询
//!    trace/metric；[`render::render_trace`] 生成文本，[`json`] 生成 JS 语义 JSON。
//! 4. 跨进程时 `inject` 得到 carrier，对方 `extract` 还原父上下文；本 crate 没有 ambient
//!    current-span 状态，父级必须显式通过参数传递。
//!
//! # 边界与失败模式
//!
//! - 运行期遥测错误统一为 [`TelemetryError`]（`Display` == `message`）；[`TypedMetrics`] 的方法
//!   为签名兼容保留 [`TypedMetricsError`]，当前恒不产生。`name: None` 表示“rejection 值不是
//!   `Error` 实例”，与 TS 只记裸 `{ status: "error" }` 的分支对应。
//! - 取消：丢弃已 poll 过的 `start_span` future 不会结算 span（若 future 从未 poll，span 根本
//!   不会被创建）；这与 TS 中回调 promise 永不完成、`finally` 不执行的可观察结果一致——注意
//!   TS 丢弃 promise 本身不会取消 async 函数。未结算 span 不进 `find_traces`/`get_trace`，只有
//!   `get_spans()` 能看到。
//! - 内存实现的进程级身份注册表只增不减（TS 的 WeakMap 会被 GC），长驻进程需要知悉。
//! - 上面“安全与可移植性”的持久化禁令是核心约束：除 carrier 作为可选的关联诊断数据外，
//!   context/span/backend 原生 trace 对象都不得落盘。
//! - 公开签名在仓库根 `ARCHITECTURE.md` §4.9 中列为冻结契约：不要直接改签名；这些类型同时被
//!   kiana-ai、kiana-chord、kiana-durable 引用，改动会波及三个上层 crate。
//! - 输出保真：进入文本/JSON 的浮点与键序按 JS 语义处理（`kiana-json` 与私有
//!   `jsval`/`jsjson`/`timsort`），不要换成 Rust 默认的格式化或 `slice::sort_by`。
//!
//! # 阅读顺序
//!
//! `index`（契约与数据类型）→ `memory`（参考实现，含结算/取消语义）→ `metrics`/`noop`
//! → `render`/`json`（输出保真）→ `testing`/`store_conformance`（适配器一致性套件）。
//! 私有模块 `jsjson`/`jsval`/`timsort` 只在核对 JS 语义细节时阅读。

pub mod capture;
pub mod index;
mod jsjson;
pub mod json;
mod jsval;
pub mod memory;
pub mod metrics;
pub mod names;
pub mod noop;
pub mod otel;
pub mod render;
pub mod store_conformance;
pub mod testing;
mod timsort;
pub mod w3c;

// 契约层：根再导出契约类型（对应 TS 包根的主要导出面）。`BoxFuture`/`ErasedValue`
// 等类型别名与“仅类型层面”的名字不在根导出，需要时从 `index` 模块引用；
// `SpanCallback` 已在下面的再导出列表中。
pub use index::{
    AttributeValue, Cardinality, MetricKind, MetricQuery, SpanAttributes, SpanCallback,
    SpanContextId, SpanLink, SpanLinkContext, SpanOptions, SpanStatus, StatusError, StoredMetric,
    StoredSpan, StoredSpanEvent, StoredSpanLink, StoredTrace, TelemetryAttributeDefinition,
    TelemetryAttributeMetadata, TelemetryAttributeType, TelemetryCarrier, TelemetryContext,
    TelemetryContextExt, TelemetryError, TelemetryEventDefinition, TelemetryParentDefinition,
    TelemetryPropagation, TelemetryRequiredAttributeDefinition, TelemetrySchemaDefinition,
    TelemetrySpan, TelemetrySpanDefinition, TelemetryStatusDefinition, TelemetryStore, TracePage,
    TraceQuery, TypedSpanStarter, create_typed_span_starter, define_telemetry_schema,
    start_span_dyn,
};
// 参考后端：内存实现、可注入的 `Clock` 与 `Recorded*` 快照。根只再导出自由函数
// `start_span_try`；`memory::start_span_typed` 与 `memory::start_child_span` 仍在模块内可用。
pub use memory::{
    Clock, InMemoryTelemetryContext, RecordedTelemetryEvent, RecordedTelemetryLink,
    RecordedTelemetryMeasurement, RecordedTelemetrySpan, SystemClock, start_span_try,
};
// 指标层：仪表 trait、schema 定义与 `TypedMetrics`（运行时只查表，不做名字/kind 校验）。
pub use metrics::{
    HistogramOptions, InstrumentOptions, TelemetryCounter, TelemetryHistogram,
    TelemetryMetricAttributeDefinition, TelemetryMetricDefinition, TelemetryMetrics,
    TelemetryUpDownCounter, TypedMetrics, TypedMetricsError, TypedTelemetryMetrics,
    create_typed_metrics,
};
// no-op 单例：未配置遥测时的回退实现，所有记录方法都是空操作。
pub use noop::{
    NOOP_TELEMETRY_CONTEXT_SENTINEL, noop_telemetry_context, noop_telemetry_context_as_context,
    noop_telemetry_metrics, noop_telemetry_propagation,
};

// 会话指标名与低基数标签（`names`）以及 OTLP 导出 / W3C trace context。
pub use capture::{CAPTURE_MESSAGE_CONTENT_ENV, ContentCapture};
pub use names as metric_names;
pub use otel::{ExportOutcome, OtlpConfig, OtlpError, OtlpExporter, OtlpProtocol, log_record};
pub use w3c::{
    SpanId, TRACE_FLAG_SAMPLED, TRACEPARENT, TRACEPARENT_ENV, TRACESTATE, TRACESTATE_ENV,
    TraceContext, TraceContextError, TraceId, TraceParent, validate_tracestate,
};
