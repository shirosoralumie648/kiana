//! metrics.ts：计数器/上下计数/直方图的类型化包装与 schema 定义。
//!
//! `createTypedMetrics` 在 TS 里是「用 schema 给仪器命名 + 类型推断」的零运行时开销包装：
//! 运行时只做 `definitions.get(name)` 查表并转发定义对象，**不做**任何校验。Rust 侧对应
//! `TypedMetrics::new(schema_metrics, metrics_backend)`，同样只查表、不拒绝未知名字或 kind
//! 不匹配（见 [`TypedMetrics`] 的文档）。
//!
//! # 与 TS 的一致性说明（exemplar 上下文）
//!
//! `memory.ts` 的 `createMeasurementRecorder` 返回的是**同一个 3 参函数**
//! `(value, attributes?, context?)`，三个工厂都拿它当 `add` / `record` 用；但 TS 的**类型**
//! （`metrics.ts` 的 `TelemetryCounter.add` / `TelemetryUpDownCounter.add`）只有 2 个参数，
//! 只有 `TelemetryHistogram.record` 把 `context?: TelemetryContext` 写进了公开契约。
//! 因此 counter / upDownCounter 的 exemplar 上下文在 TS 里只能绕过类型系统（`as any`）到达，
//! 不属于契约行为 —— Rust 的 trait 与声明面逐字对齐：`add` 2 参、`record` 3 参（含
//! `Option<&dyn TelemetryContext>`）。这是**有意的**不可表示差异，不是缺失。
//!
//! # 这个模块解决什么问题
//!
//! 本模块定义仪器选项、仪器 trait 与工厂 trait、schema 中的 `TelemetryMetricDefinition`
//! 及把 schema 名字绑定到后端的 `TypedMetrics`；指标 store 类型（`MetricKind`、
//! `StoredMetric`、`MetricQuery`、`TelemetryStore::find_metrics`）与 schema 基础类型
//! `TelemetryRequiredAttributeDefinition` 仍在 `index.rs`。后端（内存适配器、真实适配器、
//! no-op）与调用方共享这一组签名。
//!
//! # 线程模型与生命周期
//!
//! 所有 trait 都要求 `Send + Sync`，工厂返回 `Arc<dyn ...>`，仪器可以跨任务共享。trait
//! 不承诺并发记录之间的顺序或聚合语义，这些由具体后端决定（内存后端只是逐条追加测量）。
//! `TypedMetrics` 本身不持有可变状态，方法只读定义表并调用后端。
//!
//! # 阅读顺序
//!
//! 先看 `InstrumentOptions` / `HistogramOptions`，再看三个仪器 trait 与 `TelemetryMetrics`，
//! 最后看 `TelemetryMetricDefinition`（schema 形状）和 `TypedMetrics`（查表转发）。
//!
//! TS 源：`packages/telemetry/src/metrics.ts`。

use std::sync::Arc;

use indexmap::IndexMap;
use serde::{Deserialize, Serialize};

use crate::index::{SpanAttributes, TelemetryAttributeDefinition, TelemetryContext};

/// `InstrumentOptions`：创建仪器时携带的元数据（schema 中同名字段的运行时形态）。
///
/// 两个字段都可选；`None` 在 serde 层被省略，内存后端把它当作“未提供”。这些字段只随仪器
/// 创建传给后端，不参与记录校验或聚合。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct InstrumentOptions {
    /// 人类可读描述（TS `description?: string`），后端原样保存。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// 计量单位，例如 `"By"`、`"ms"`；后端原样保存，不做换算。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unit: Option<String>,
}

/// `HistogramOptions`：直方图专属选项（通用元数据 + 分桶边界提示）。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct HistogramOptions {
    /// 与 counter/upDownCounter 共用的 `description` / `unit`。
    #[serde(flatten)]
    pub instrument: InstrumentOptions,
    /// Bucket hints; callers supply finite, strictly increasing values.
    ///
    /// 实现只透传、不校验；调用方仍须按契约提供有限且严格递增的值。内存适配器把该数组
    /// 原样放进测量快照，不做实际分桶（`find_metrics` 返回的是逐条测量，不是桶计数）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub boundaries: Option<Vec<f64>>,
}

/// `TelemetryCounter`：单调累加的计数器仪器。
///
/// 实现通常以 `Arc<dyn TelemetryCounter>` 跨任务共享，因此要求 `Send + Sync`；`add` 取
/// `&self`，实现若需可变状态必须自行同步。契约层不规定负值/非有限值的处理。
pub trait TelemetryCounter: Send + Sync {
    /// 记录一次增量。`attributes = None` 表示“未提供属性”，与 `Some(空 map)` 的区分由
    /// 具体后端决定（内存适配器把两者都记为 `{}`）。无返回值、无错误通道。
    fn add(&self, value: f64, attributes: Option<SpanAttributes>);
}

/// `TelemetryUpDownCounter`：可增可减的计数器仪器。
///
/// 与 [`TelemetryCounter`] 的唯一区别是语义（允许负增量）；接口与线程约束相同。
pub trait TelemetryUpDownCounter: Send + Sync {
    /// 记录一次增量（可为负）。其余约定同 [`TelemetryCounter::add`]。
    fn add(&self, value: f64, attributes: Option<SpanAttributes>);
}

/// `TelemetryHistogram`：记录观测值分布的仪器。
///
/// `record` 比另外两个仪器多一个 `context` 参数：只在这里，TS 的公开契约把
/// `context?: TelemetryContext` 写进签名（exemplar 关联）。实现可以忽略它。
pub trait TelemetryHistogram: Send + Sync {
    /// 记录一次观测值。`context` 是可选 exemplar 上下文；内存后端会把它解析成 carrier
    /// 写入测量快照（未注册的上下文解析为 `None`）。边界/有限性不由本层校验。
    fn record(
        &self,
        value: f64,
        attributes: Option<SpanAttributes>,
        context: Option<&dyn TelemetryContext>,
    );
}

/// `TelemetryMetrics`：创建仪器的工厂（同步、对象安全）。
///
/// 每次调用都转发给实现；是否复用同名的仪器实例由后端决定（内存后端每次新建，no-op
/// 后端始终返回共享单例）。`options = None` 表示调用方未提供元数据。
pub trait TelemetryMetrics: Send + Sync {
    /// 创建/获取一个计数器。
    fn counter(&self, name: &str, options: Option<InstrumentOptions>) -> Arc<dyn TelemetryCounter>;
    /// 创建/获取一个上下计数器。
    fn up_down_counter(
        &self,
        name: &str,
        options: Option<InstrumentOptions>,
    ) -> Arc<dyn TelemetryUpDownCounter>;
    /// 创建/获取一个直方图；`options` 额外承载 `boundaries`。
    fn histogram(
        &self,
        name: &str,
        options: Option<HistogramOptions>,
    ) -> Arc<dyn TelemetryHistogram>;
}

/// `TelemetryMetricDefinition`：`kind` 判别同时决定 `boundaries` 是否允许出现。
///
/// 这是 schema 的静态形状：`attributes` 是带 `required` 标记的属性定义表（条目可以是
/// `required: false`），运行时不会拿它校验实际记录；`boundaries` 只存在于 `Histogram`
/// 变体。serde 用 `kind` 标签序列化为 `"counter"` / `"upDownCounter"` / `"histogram"`。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum TelemetryMetricDefinition {
    /// 计数器定义：`description` 与 `attributes` 必填（`attributes` 可为空表），`unit` 可选。
    #[serde(rename = "counter")]
    Counter {
        /// 仪器描述（schema 必填）。
        description: String,
        /// 计量单位；`None` 时序列化省略。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        unit: Option<String>,
        /// 该 metric 声明的属性定义（可含必填标记），仅作 schema 描述。
        attributes: IndexMap<String, crate::index::TelemetryRequiredAttributeDefinition>,
    },
    /// 上下计数器定义；字段形状与 [`TelemetryMetricDefinition::Counter`] 相同
    /// （`description` 与 `attributes` 必填，`attributes` 可为空表，`unit` 可选）。
    #[serde(rename = "upDownCounter")]
    UpDownCounter {
        /// 仪器描述（schema 必填）。
        description: String,
        /// 计量单位；`None` 时序列化省略。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        unit: Option<String>,
        /// 该 metric 声明的属性定义，仅作 schema 描述。
        attributes: IndexMap<String, crate::index::TelemetryRequiredAttributeDefinition>,
    },
    /// 直方图定义；额外的 `boundaries` 与 [`HistogramOptions::boundaries`] 同义。
    #[serde(rename = "histogram")]
    Histogram {
        /// 仪器描述（schema 必填）。
        description: String,
        /// 计量单位；`None` 时序列化省略。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        unit: Option<String>,
        /// 该 metric 声明的属性定义，仅作 schema 描述。
        attributes: IndexMap<String, crate::index::TelemetryRequiredAttributeDefinition>,
        /// 分桶边界提示；只有直方图变体允许携带。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        boundaries: Option<Vec<f64>>,
    },
}

impl TelemetryMetricDefinition {
    /// `kind` 字面量。
    pub fn kind(&self) -> &'static str {
        match self {
            TelemetryMetricDefinition::Counter { .. } => "counter",
            TelemetryMetricDefinition::UpDownCounter { .. } => "upDownCounter",
            TelemetryMetricDefinition::Histogram { .. } => "histogram",
        }
    }
}

/// schema 里的 metric 名 → 定义（插入序）。
///
/// 用 `IndexMap` 保留 schema 声明顺序；重复插入同名键时保留首次位置、更新值，
/// 因此“后出现的 schema 覆盖同名 metric”由调用方的合并顺序决定。
pub type TelemetryMetricDefinitions = IndexMap<String, TelemetryMetricDefinition>;

/// `createTypedMetrics` 的 Rust 等价物：把 schema 的 metric 名与真实后端绑定。
///
/// # 运行时行为（与 TS 逐条对齐）
///
/// TS 的 `createTypedMetrics` 只做 `definitions.get(name)` 查表，然后无条件调用
/// `metrics.counter(name, definition)`：
///
/// ```text
/// counter: (name) => metrics.counter(name, definitions.get(name))
/// ```
///
/// 因此运行时**没有**任何校验：未知名字得到 `undefined` 选项并返回可用的仪表；kind 不匹配只是
/// 把另一个 kind 的定义对象当选项传下去（内存适配器只读 `description` / `unit` / histogram 的
/// `boundaries`，于是记为带该元数据的目标 kind 测量）。这里同样不做拒绝：查不到定义时传
/// `None`，查到就把定义的 `description` / `unit`（以及 histogram 的 `boundaries`）转发给目标工厂。
///
/// 保留 `Result` 返回类型只为源码兼容（§4.9 的签名形状与下游调用点），当前实现恒为 `Ok`；
/// [`TypedMetricsError`] 因此不再由本类型产生，但仍是公开错误类型（ARCHITECTURE §4 的错误清单）。
///
/// 无法表达的部分：TS 把**整个定义对象**（含 `kind`、`attributes`、`boundaries`）交给
/// `metrics.counter/upDownCounter/histogram`，自定义适配器可以读到全部字段；Rust 的
/// [`TelemetryMetrics`] 工厂签名冻结为 `Option<InstrumentOptions>`（histogram 为
/// `Option<HistogramOptions>`），只承载 `description` / `unit` / `boundaries`。
///
/// # 字段与不变量
///
/// - `definitions`：构造后不再修改的名字→定义表；查不到即视为“未知名字”；
/// - `backend`：所有工厂调用都原样转发给它，本类型不缓存仪器。
///
/// 结构本身没有 `Clone`，但方法只读字段并以 `&self` 调用后端；放进 `Arc` 后可在任务间共享，
/// 前提是后端满足 trait 的 `Send + Sync` 约束。
pub struct TypedMetrics {
    definitions: TelemetryMetricDefinitions,
    backend: Arc<dyn TelemetryMetrics>,
}

/// `createTypedMetrics` 的运行时校验错误（历史类型）。
///
/// TS 没有运行时校验，本实现也不再产生该错误；保留导出以维持 ARCHITECTURE §4 的错误类型
/// 清单与既有调用点的源码兼容。
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum TypedMetricsError {
    /// 未知 metric 名（字段 `name`；历史路径，当前实现不会返回）。
    #[error("metric `{name}` is not defined in the schema")]
    UnknownMetric { name: String },
    /// kind 不匹配（历史路径；当前实现不会返回）。
    #[error("metric `{name}` is a {actual}, not a {expected}")]
    KindMismatch {
        /// metric 名字。
        name: String,
        /// 期望的 kind 字面量（`"counter"` / `"upDownCounter"` / `"histogram"`）。
        expected: &'static str,
        /// 实际查到的 kind 字面量。
        actual: &'static str,
    },
}

impl TypedMetrics {
    /// `createTypedMetrics(schema, metrics)`：保存定义表与后端。
    ///
    /// TS 的“schema 中每个 metric 必须先有定义”是**编译期**约束；本构造函数不做任何运行时
    /// 校验、不返回错误，未知名字在查询工厂时才会以 `None` 选项的形式体现。`definitions`
    /// 中的重复名字由 `IndexMap` 的插入语义决定（保留首次位置、更新值），调用方负责合并策略。
    pub fn new(
        definitions: TelemetryMetricDefinitions,
        backend: Arc<dyn TelemetryMetrics>,
    ) -> Self {
        TypedMetrics {
            definitions,
            backend,
        }
    }

    /// `definitions.get(name)` 之后的内存适配器可观察部分：`description` + `unit`。
    ///
    /// 返回 `None` 仅表示名字不在定义表中。命中时 `description` 总是 `Some`（定义里必填，
    /// 即使为空串），`unit` 原样透传。`kind` / `attributes` 不进入 `InstrumentOptions`，
    /// 因为工厂签名无法承载它们（见类型级文档的最后一段）。
    fn instrument_options(&self, name: &str) -> Option<InstrumentOptions> {
        let def = self.definitions.get(name)?;
        let (description, unit) = match def {
            TelemetryMetricDefinition::Counter {
                description, unit, ..
            }
            | TelemetryMetricDefinition::UpDownCounter {
                description, unit, ..
            }
            | TelemetryMetricDefinition::Histogram {
                description, unit, ..
            } => (description, unit),
        };
        Some(InstrumentOptions {
            description: Some(description.clone()),
            unit: unit.clone(),
        })
    }

    /// 目标工厂是 histogram 时读的 `boundaries`（只有 histogram 定义才有）。
    ///
    /// 非 histogram 定义与“histogram 但未声明 boundaries”都返回 `None`，这两者在
    /// [`HistogramOptions`] 里不可区分。
    fn histogram_boundaries(&self, name: &str) -> Option<Vec<f64>> {
        match self.definitions.get(name)? {
            TelemetryMetricDefinition::Histogram { boundaries, .. } => boundaries.clone(),
            _ => None,
        }
    }

    /// `metrics.counter(name, definitions.get(name))`（未知名字 → `undefined` 选项）。
    ///
    /// 恒返回 `Ok`：`Result` 只为冻结签名与既有调用点保留，[`TypedMetricsError`] 不会由本方法
    /// 产生。每次调用都会转发给后端工厂，本类型不缓存返回的仪器。
    pub fn counter(&self, name: &str) -> Result<Arc<dyn TelemetryCounter>, TypedMetricsError> {
        Ok(self.backend.counter(name, self.instrument_options(name)))
    }

    /// `metrics.upDownCounter(name, definitions.get(name))`。
    ///
    /// 与 [`TypedMetrics::counter`] 一样不做名字/kind 校验，恒返回 `Ok`。
    pub fn up_down_counter(
        &self,
        name: &str,
    ) -> Result<Arc<dyn TelemetryUpDownCounter>, TypedMetricsError> {
        Ok(self
            .backend
            .up_down_counter(name, self.instrument_options(name)))
    }

    /// `metrics.histogram(name, definitions.get(name))`。
    ///
    /// 命中 histogram 定义时额外带上 `boundaries`；命中其他 kind 时 `boundaries` 为 `None`，
    /// 但仍然把该定义的 `description`/`unit` 转发下去（kind 不匹配不是错误）。恒返回 `Ok`。
    pub fn histogram(&self, name: &str) -> Result<Arc<dyn TelemetryHistogram>, TypedMetricsError> {
        let options = self
            .instrument_options(name)
            .map(|instrument| HistogramOptions {
                instrument,
                boundaries: self.histogram_boundaries(name),
            });
        Ok(self.backend.histogram(name, options))
    }
}

/// `createTypedMetrics` 的入口名（TS 同名函数）：等价于 [`TypedMetrics::new`]，
/// 不校验、不返回 `Result`。
pub fn create_typed_metrics(
    definitions: TelemetryMetricDefinitions,
    backend: Arc<dyn TelemetryMetrics>,
) -> TypedMetrics {
    TypedMetrics::new(definitions, backend)
}

/// schema 中 `attributes` 的类型别名，供各单元直接引用。
///
/// 等价于 [`crate::index::TelemetryAttributeDefinition`]；保留别名只是为了让 metrics
/// 相关签名在名字上与 TS 对齐。
pub type TelemetryMetricAttributeDefinition = TelemetryAttributeDefinition;

/// TS 的 `TypedTelemetryMetrics`：schema 约束下的 metrics 工厂。
///
/// TS 用条件类型把三个工厂的名字/kind/属性钉死；Rust 侧是 [`TypedMetrics`]。
/// **注意**：运行时没有任何校验——构造与工厂查询都不拒绝未知名字或 kind 不匹配，返回的
/// `Result` 只保留签名形状、当前恒为 `Ok`。也就是说 TS 的约束在 Rust 中只剩类型文档价值，
/// 见 spec §25 第 4 条与 `tests/metrics.rs` 中对未知名字/kind 不匹配的断言。
pub type TypedTelemetryMetrics = TypedMetrics;
