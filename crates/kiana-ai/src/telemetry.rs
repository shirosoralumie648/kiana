//! telemetry.ts。
//!
//! TS: `src/telemetry.ts`。`AI_TELEMETRY_SCHEMA`（version 1）保留 span/metric 定义，是稳定契约；
//! [`observe_model_request`] 返回 [`LazyStreamObserver`]，在流事件与终态结果上记录
//! span 事件、属性与低基数指标。
//!
//! 与 TS 的实现差异（Rust 侧可观察行为一致）：
//! - `WeakMap<TelemetryMetrics, instruments>` → 以 `Arc` 地址为键的 `Weak` 缓存；
//! - `performance.now()` → [`std::time::Instant`]；
//! - `void start(name, attrs, run)` 的「同步启动」用「先手动 poll 一次、再 spawn 等待
//!   finish」实现，保证 observer 第一次收到事件时 span 已就绪。
//!
//! ## 设计要点（为什么这样写）
//!
//! - schema 是可序列化的**数据**：`build_schema` 用 `IndexMap` 保留 TS 对象字面量的键序，
//!   黄金测试按 JS `JSON.stringify` 语义逐字节比较（`tests/telemetry.rs` 的黄金文件测试，
//!   用 `kiana_json::js_object::stringify` 比较字符串），因此字段与键的顺序都是契约，
//!   不能为「整理」而重排；`kiana-extensions` 的常量导出测试只把 JS `JSON.stringify` 的
//!   结果解析成 JSON `Value` 后比较值（不比较字节/键序）。
//! - 只有两个 span 名：普通请求 `kiana.ai.request`、deferred 轮询 `kiana.ai.poll`；
//!   两者复用同一份属性/事件/状态定义，仅 description 不同。
//! - [`observe_model_request`] 只构造 [`LazyStreamObserver`]，不自己驱动流：调用方
//!   （`models` 模块的 `stream`/`stream_simple`/`stream_deferred`）把 observer 交给
//!   `lazy_stream`，由后者在每个事件 push 之后与终态结果时回调本模块。
//! - 指标 instrument 按 metrics adapter 缓存（TS 的 `WeakMap` 语义）：同一 recorder 上的
//!   root 请求与其子请求共享同一组 instrument；缓存只持弱引用，不延长 adapter 与已记录
//!   遥测数据的生命周期。
//!
//! ## 一次观察的生命周期
//!
//! 1. 记录起始时刻，取 `provider`/`model` 基础属性与 metrics adapter（缺省为 noop）。
//! 2. 用 typed span starter 启动 span：回调把 span 写进共享槽位，然后挂起等待 `finish`。
//! 3. 第一个可见流事件 → `kiana.ai.first_chunk` 事件 + first-chunk 直方图；内容块结束
//!    事件 → `kiana.ai.block` 事件（记录块类型/下标/字符数，tool_call 另记工具名，
//!    不记录块内容）。
//! 4. 终态消息 → 写 end 属性、`error` 状态（错误文本截断到 500 个 UTF-16 码元）、
//!    request 直方图；非 pending/deferred 的轮询再写 4 个 token 计数与 cost 计数；
//!    最后通知 span 回调结束。
//!
//! ## 注意事项
//!
//! - 「第一个事件即 first chunk」依赖 span 在事件到达前已就绪：内存适配器在首次 poll 内
//!   同步调用回调；若自定义 starter 在回调前有真正的异步等待，早到的事件会被静默跳过
//!   （`first` 标记尚未消费，第一个可见事件仍会被记为 first chunk）。
//! - 请求级属性以及指标标签里的 `provider`/`model` 取自**调用方请求的模型**（认证/转换
//!   之前），不是实际发出请求时改写后的模型。
//! - [`observe_model_request`] 不返回错误：遥测后端的失败被忽略，不影响模型请求本身。

use std::collections::HashMap;
use std::future::Future;
use std::sync::{Arc, LazyLock, Mutex, Weak};
use std::time::Instant;

use indexmap::IndexMap;
use kiana_telemetry::index::{
    AttributeValue, Cardinality, SpanAttributes, SpanStatus, StatusError,
    TelemetryAttributeDefinition, TelemetryAttributeMetadata, TelemetryEventDefinition,
    TelemetryParentDefinition, TelemetryRequiredAttributeDefinition, TelemetrySchemaDefinition,
    TelemetrySpan, TelemetrySpanDefinition, TelemetryStatusDefinition, create_typed_span_starter,
    define_telemetry_schema,
};
use kiana_telemetry::metrics::{
    TelemetryCounter, TelemetryHistogram, TelemetryMetricDefinition, TelemetryMetrics,
    create_typed_metrics,
};
use kiana_telemetry::{noop_telemetry_context, noop_telemetry_metrics};

use crate::api::lazy::{LazyStreamObserver, spawn_forward};
use crate::types::{AssistantMessage, AssistantMessageEvent, Model, StopReason};

/// 构造一个 string 属性的 schema 定义（带 `required` 标记）。
///
/// `values` 是允许值清单，只进 schema（供文档/校验器使用），运行时不校验记录值；
/// `cardinality` 标记高基数属性（本 schema 里只有 `response.id` 用 `High`）。
fn string_attribute(
    description: &str,
    required: bool,
    values: Option<&[&str]>,
    cardinality: Option<Cardinality>,
) -> TelemetryRequiredAttributeDefinition {
    TelemetryRequiredAttributeDefinition {
        definition: TelemetryAttributeDefinition::String {
            meta: TelemetryAttributeMetadata {
                description: description.to_owned(),
                sensitive: None,
                cardinality,
            },
            values: values.map(|values| values.iter().map(|value| (*value).to_owned()).collect()),
            examples: None,
        },
        required,
    }
}

/// 构造一个 number 属性的 schema 定义。
///
/// 该 helper 不接收 values 参数，构造出的 number 属性 `values` 恒为 `None`
/// （`TelemetryAttributeDefinition::Number` 本身支持数值允许值清单）。
fn number_attribute(description: &str, required: bool) -> TelemetryRequiredAttributeDefinition {
    TelemetryRequiredAttributeDefinition {
        definition: TelemetryAttributeDefinition::Number {
            meta: TelemetryAttributeMetadata {
                description: description.to_owned(),
                sensitive: None,
                cardinality: None,
            },
            values: None,
            examples: None,
        },
        required,
    }
}

/// 去掉 `required` 包装，得到 end 属性用的定义。
///
/// schema 的 start 属性与事件属性都用带 `required` 的包装（`IndexMap` 的值是
/// `TelemetryRequiredAttributeDefinition`），只有 `end_attributes` 的类型没有 required
/// 维度，所以这里直接取出内部定义。
fn optional(attribute: TelemetryRequiredAttributeDefinition) -> TelemetryAttributeDefinition {
    attribute.definition
}

// 三个枚举值清单逐字进入 schema，顺序也是序列化结果的一部分。
// `STOP_VALUES` 必须与 [`stop_reason_name`] 的 7 个分支保持一致；`stop_reason_name` 用的是
// 手写字面量而非这张表，二者没有类型层面的联动，新增 `StopReason` 变体时要同时改两处。
const STOP_VALUES: [&str; 7] = [
    "pending", "stop", "length", "toolUse", "error", "aborted", "deferred",
];
const BLOCK_VALUES: [&str; 3] = ["text", "thinking", "tool_call"];
const TOKEN_KIND_VALUES: [&str; 4] = ["input", "output", "cache_read", "cache_write"];

/// 生成一个 span 的完整定义；`kiana.ai.request` 与 `kiana.ai.poll` 只靠 description 区分。
///
/// 属性表与事件表用 `IndexMap`，插入顺序就是序列化顺序（黄金测试按 JS 对象键序逐字节比较），
/// 调整顺序前先看 `tests/telemetry.rs` 的 schema 断言。
fn request_span_definition(description: &str) -> TelemetrySpanDefinition {
    let provider = string_attribute("Provider identifier.", true, None, None);
    let model = string_attribute("Requested model identifier.", true, None, None);
    let stop = string_attribute("Normalized stop reason.", false, Some(&STOP_VALUES), None);
    let api = string_attribute("API identifier.", true, None, None);
    let reasoning = string_attribute("Scalar requested reasoning level.", false, None, None);

    let mut start_attributes = IndexMap::new();
    start_attributes.insert("provider".into(), provider);
    start_attributes.insert("api".into(), api);
    start_attributes.insert("model".into(), model);
    start_attributes.insert("reasoning".into(), reasoning);

    let mut end_attributes = IndexMap::new();
    end_attributes.insert("stop".into(), optional(stop));
    end_attributes.insert(
        "usage.input".into(),
        optional(number_attribute("Uncached input tokens.", false)),
    );
    end_attributes.insert(
        "usage.output".into(),
        optional(number_attribute("Output tokens.", false)),
    );
    end_attributes.insert(
        "usage.cache_read".into(),
        optional(number_attribute("Cache-read tokens.", false)),
    );
    end_attributes.insert(
        "usage.cache_write".into(),
        optional(number_attribute("Cache-write tokens.", false)),
    );
    end_attributes.insert(
        "cost.total".into(),
        optional(number_attribute("Total request cost in USD.", false)),
    );
    end_attributes.insert(
        "response.id".into(),
        optional(string_attribute(
            "Provider response identifier.",
            false,
            None,
            Some(Cardinality::High),
        )),
    );

    let mut events = IndexMap::new();
    events.insert(
        "kiana.ai.first_chunk".into(),
        TelemetryEventDefinition {
            description: "First normalized stream event delivered.".into(),
            attributes: IndexMap::new(),
        },
    );
    let mut block_attributes = IndexMap::new();
    block_attributes.insert(
        "type".into(),
        string_attribute("Block kind.", true, Some(&BLOCK_VALUES), None),
    );
    block_attributes.insert(
        "index".into(),
        number_attribute("Content block index.", true),
    );
    block_attributes.insert(
        "chars".into(),
        number_attribute("UTF-16 character count.", true),
    );
    block_attributes.insert(
        "tool".into(),
        string_attribute("Tool name, for tool-call blocks only.", false, None, None),
    );
    events.insert(
        "kiana.ai.block".into(),
        TelemetryEventDefinition {
            description: "A completed content block; content is never recorded.".into(),
            attributes: block_attributes,
        },
    );

    TelemetrySpanDefinition {
        description: description.to_owned(),
        parents: Some(TelemetryParentDefinition::Any),
        start_attributes,
        end_attributes,
        events: Some(events),
        status: TelemetryStatusDefinition {
            default: "ok".into(),
            error_when: "Stop reason is error: provider_error with at most 500 error-message characters. Aborted stays ok."
                .into(),
        },
    }
}

/// 构建 version 1 的完整 schema（两个 span + 四个指标）。
///
/// 两个 span 复用 [`request_span_definition`]；四个指标共享 `provider`/`model` 属性定义，
/// `stop` 与 `kind` 各自独立。所有 `IndexMap` 的插入顺序都会进入序列化结果，不要重排。
fn build_schema() -> TelemetrySchemaDefinition {
    let mut spans = IndexMap::new();
    spans.insert(
        "kiana.ai.request".into(),
        request_span_definition(
            "One logical model request, including authentication and provider setup.",
        ),
    );
    spans.insert(
        "kiana.ai.poll".into(),
        request_span_definition("One deferred-response poll, not the deferred request's lifetime."),
    );

    let provider = string_attribute("Provider identifier.", true, None, None);
    let model = string_attribute("Requested model identifier.", true, None, None);
    let stop = string_attribute("Normalized stop reason.", true, Some(&STOP_VALUES), None);
    let kind = string_attribute(
        "Token usage category.",
        true,
        Some(&TOKEN_KIND_VALUES),
        None,
    );

    let mut metrics = IndexMap::new();
    let mut duration_attributes = IndexMap::new();
    duration_attributes.insert("provider".into(), provider.clone());
    duration_attributes.insert("model".into(), model.clone());
    duration_attributes.insert("stop".into(), stop);
    metrics.insert(
        "kiana.ai.request.duration".into(),
        TelemetryMetricDefinition::Histogram {
            description: "Logical request or poll duration.".into(),
            unit: Some("ms".into()),
            attributes: duration_attributes,
            boundaries: None,
        },
    );
    let mut first_chunk_attributes = IndexMap::new();
    first_chunk_attributes.insert("provider".into(), provider.clone());
    first_chunk_attributes.insert("model".into(), model.clone());
    metrics.insert(
        "kiana.ai.first_chunk.duration".into(),
        TelemetryMetricDefinition::Histogram {
            description: "Time to the first normalized stream event.".into(),
            unit: Some("ms".into()),
            attributes: first_chunk_attributes,
            boundaries: None,
        },
    );
    let mut token_attributes = IndexMap::new();
    token_attributes.insert("provider".into(), provider.clone());
    token_attributes.insert("model".into(), model.clone());
    token_attributes.insert("kind".into(), kind);
    metrics.insert(
        "kiana.ai.tokens".into(),
        TelemetryMetricDefinition::Counter {
            description: "Reported model token usage; pending and deferred polls are excluded."
                .into(),
            unit: Some("token".into()),
            attributes: token_attributes,
        },
    );
    let mut cost_attributes = IndexMap::new();
    cost_attributes.insert("provider".into(), provider);
    cost_attributes.insert("model".into(), model);
    metrics.insert(
        "kiana.ai.cost".into(),
        TelemetryMetricDefinition::Counter {
            description: "Reported model cost; pending and deferred polls are excluded.".into(),
            unit: Some("USD".into()),
            attributes: cost_attributes,
        },
    );

    define_telemetry_schema(TelemetrySchemaDefinition {
        version: 1.0,
        spans,
        metrics: Some(metrics),
    })
}

/// Kiana-owned request metadata, bounded error status, events, and low-cardinality metrics.
///
/// TS: `src/telemetry.ts#AI_TELEMETRY_SCHEMA`
///
/// schema（version 1）声明两个 span（`kiana.ai.request` / `kiana.ai.poll`）与四个指标
/// （request/first-chunk 直方图、token/cost 计数器）。`models` 模块也重导出同一对象。
/// 它是**数据**：可被序列化后当常量暴露（`kiana-extensions` 以 `AI_TELEMETRY_SCHEMA`
/// 常量导出给扩展 JS），黄金测试按 JS `JSON.stringify` 语义逐字节比对，所以每个字段的
/// 文本与键序都属于对外契约。
///
/// 首次访问时由私有函数 `build_schema` 构建，之后只读共享；构建过程没有失败通道。
pub static AI_TELEMETRY_SCHEMA: LazyLock<TelemetrySchemaDefinition> = LazyLock::new(build_schema);

/// 一组按 schema 创建好的 instrument。
///
/// 四个字段对应 schema 的四个指标；`Arc` 让 observer 闭包与缓存中的弱引用共享同一组
/// instrument，而不必每次请求重新创建。
struct Instruments {
    request_duration: Arc<dyn TelemetryHistogram>,
    first_chunk_duration: Arc<dyn TelemetryHistogram>,
    tokens: Arc<dyn TelemetryCounter>,
    cost: Arc<dyn TelemetryCounter>,
}

/// TS 的 `WeakMap<TelemetryMetrics, instruments>`。
///
/// TS 的 WeakMap 是 ephemeron：key（adapter）不可达后，value（instruments）也随之可回收。
/// Rust 侧用「Weak key + Weak value」复刻：只缓存弱引用，调用方（observer 闭包）持有强引用；
/// 每次查找前清理**任一端**已不可达的条目（`strong_count() == 0` 即视为死亡），避免进程级
/// 无限保留 adapter 与已记录的遥测数据。
type InstrumentCache = Mutex<HashMap<usize, (Weak<dyn TelemetryMetrics>, Weak<Instruments>)>>;

/// 进程级 instrument 缓存；锁的范围与失效策略见 [`instruments_for`]。
static INSTRUMENT_CACHE: LazyLock<InstrumentCache> = LazyLock::new(|| Mutex::new(HashMap::new()));

/// 缓存键：`Arc` 的数据指针，转换成 `usize`。
///
/// 这一步会丢掉 trait object 的 vtable 元数据，不同具体类型的 adapter 可能得到相同的键
/// （地址复用），所以查找时还会用 `Arc::ptr_eq` 复核升级后的 adapter 身份；键只用于缩小
/// 查找范围，不能单独作为身份。
fn metrics_key(adapter: &Arc<dyn TelemetryMetrics>) -> usize {
    Arc::as_ptr(adapter) as *const () as usize
}

/// 按当前 schema 的指标定义创建四个 instrument。
///
/// `TypedMetrics` 的查询当前恒返回 `Ok`（不做名字/kind 的运行时校验，未知名字只是得到
/// `None` 选项），所以这里的 `unwrap_or_else` 目前不会被触发；保留它是防御性回退——若
/// 未来查询开始返回错误，就退化为同名、无选项的 instrument，而不是 panic。
fn create_instruments(adapter: Arc<dyn TelemetryMetrics>) -> Arc<Instruments> {
    let definitions = AI_TELEMETRY_SCHEMA.metrics.clone().unwrap_or_default();
    let metrics = create_typed_metrics(definitions, Arc::clone(&adapter));
    let request_duration = metrics
        .histogram("kiana.ai.request.duration")
        .unwrap_or_else(|_| adapter.histogram("kiana.ai.request.duration", None));
    let first_chunk_duration = metrics
        .histogram("kiana.ai.first_chunk.duration")
        .unwrap_or_else(|_| adapter.histogram("kiana.ai.first_chunk.duration", None));
    let tokens = metrics
        .counter("kiana.ai.tokens")
        .unwrap_or_else(|_| adapter.counter("kiana.ai.tokens", None));
    let cost = metrics
        .counter("kiana.ai.cost")
        .unwrap_or_else(|_| adapter.counter("kiana.ai.cost", None));
    Arc::new(Instruments {
        request_duration,
        first_chunk_duration,
        tokens,
        cost,
    })
}

/// 取（或创建）某个 metrics adapter 对应的 instrument 组。
///
/// 查找前先 `retain` 掉键或值任一端已死亡的条目：WeakMap 语义要求 adapter 不可达后其
/// instrument 与已记录数据也能回收，否则进程级 map 会无限增长。
///
/// 注意锁的范围：`create_instruments` 在持有全局缓存锁时执行（它会调用 adapter 的
/// `histogram`/`counter`）。adapter 的工厂方法不能反过来调用本函数——`std::sync::Mutex`
/// 不可重入，同一线程重入会死锁，其他线程则会阻塞到创建完成。
fn instruments_for(adapter: &Arc<dyn TelemetryMetrics>) -> Arc<Instruments> {
    let key = metrics_key(adapter);
    let mut cache = INSTRUMENT_CACHE.lock().unwrap_or_else(|e| e.into_inner());
    // WeakMap 语义：任一端的对象已不可达，条目就可以整体丢弃（也防止 Arc 地址被复用后
    // 命中的旧条目返回错误的 instruments）。
    cache.retain(|_, (cached_adapter, cached_instruments)| {
        cached_adapter.strong_count() > 0 && cached_instruments.strong_count() > 0
    });
    if let Some((cached_adapter, cached_instruments)) = cache.get(&key)
        && cached_adapter
            .upgrade()
            .is_some_and(|existing| Arc::ptr_eq(&existing, adapter))
        && let Some(instruments) = cached_instruments.upgrade()
    {
        return instruments;
    }
    let instruments = create_instruments(Arc::clone(adapter));
    cache.insert(key, (Arc::downgrade(adapter), Arc::downgrade(&instruments)));
    instruments
}

/// `observeModelRequest` 的入参（TS `options` 中参与遥测的字段）。
///
/// 只有这两个字段影响遥测；api key、headers、signal 等其余请求选项不会进入 span 属性。
#[derive(Clone, Default)]
pub struct ObserveModelRequestOptions {
    /// 显式父 span。
    ///
    /// 它同时决定指标后端：先取该上下文的 `metrics()`，为 `None` 时退回 noop metrics
    /// （此时 span 仍照常记录，只是没有指标）。为 `None` 时 span 挂在 noop 上下文上。
    pub telemetry: Option<Arc<dyn TelemetrySpan>>,
    /// `typeof options.reasoning === "string"` 时记录到 start 属性。
    ///
    /// TS 会忽略传给 `reasoning` 的非字符串值（provider 原生 reasoning 对象）；Rust 的
    /// `Option<String>` 在类型层面已经表达了这一点，`None` 就不写这个键。
    pub reasoning: Option<String>,
}

/// 把 `(键, 值)` 列表转成 span 属性表；保持调用方给出的顺序。
///
/// InMemory recorder 保留属性插入序，测试据此比对 TS 对象字面量的键序（例如 start 属性
/// 必须是 provider、model、api、reasoning、……）。
fn attributes(pairs: &[(&str, AttributeValue)]) -> SpanAttributes {
    pairs
        .iter()
        .map(|(key, value)| ((*key).to_owned(), value.clone()))
        .collect()
}

/// 请求级基础属性：`provider` 与 `model`。
///
/// 取自**调用方请求的模型**（认证/转换之前）；duration/token/cost 指标都用它作标签，
/// 因此一次请求的所有遥测共享同一组 provider/model。
fn model_attributes(model: &Model) -> SpanAttributes {
    attributes(&[
        ("provider", AttributeValue::String(model.provider.clone())),
        ("model", AttributeValue::String(model.id.clone())),
    ])
}

/// `observeModelRequest(requestedModel, options, poll)`。
///
/// TS: `src/telemetry.ts#observeModelRequest`
///
/// # 参数
///
/// - `requested_model`：调用方请求的模型（认证/转换之前的版本）。span 的 `provider`/
///   `model`/`api` 属性，以及各指标标签里的 `provider`/`model`，都取自它，而不是认证后
///   改写过的模型。
/// - `options`：父 span 与可选的 reasoning 名；见 [`ObserveModelRequestOptions`]。
/// - `poll`：`true` 表示这是一次 deferred 响应轮询——span 名改为 `kiana.ai.poll`，且
///   stop reason 为 pending/deferred 的轮询不记 token 与 cost（避免重复计费）。
///
/// # 返回与生命周期
///
/// 返回的 [`LazyStreamObserver`] 由 `lazy_stream` 驱动：`on_event` 每收到一个流事件调用
/// 一次，`on_result` 在流结束时至多调用一次（参数可能是 `None`，表示流没有终态消息）。
/// 本函数不返回错误，遥测后端的失败不会影响模型请求。
///
/// # 失败模式与边界
///
/// span 的启动是「先同步 poll 一次、再交给后台任务」：内存适配器在首次 poll 内就执行
/// 回调，因此返回前 span 已就绪；若自定义 starter 在调用回调前需要真正的异步等待，那么
/// 那之前到达的事件会被跳过（span 槽位仍为空时 `on_event` 直接返回，且不消费 `first`
/// 标记）。`on_result` 通过一次性 `finish` 通道关闭 span，重复调用只会发送一次。
pub fn observe_model_request(
    requested_model: &Model,
    options: ObserveModelRequestOptions,
    poll: bool,
) -> LazyStreamObserver {
    // 阶段 1：起始时刻、基础属性与指标后端。`started` 在创建 instrument 之前取，与 TS 的
    // `performance.now()` 位置一致；没有 metrics 上下文时退回 noop（span 仍记录）。
    let started = Instant::now();
    let base_attributes = model_attributes(requested_model);
    let adapter = options
        .telemetry
        .as_ref()
        .and_then(|telemetry| telemetry.metrics())
        .unwrap_or_else(noop_telemetry_metrics);
    let instruments = instruments_for(&adapter);

    // 阶段 2：启动 span。span 槽位与 finish 通道把「回调内的 span」和「流事件/终态」两个
    // 阶段连接起来：回调先写入 span 再挂起等待 finish，observer 侧只读槽位。
    let span_slot: Arc<Mutex<Option<Arc<dyn TelemetrySpan>>>> = Arc::new(Mutex::new(None));
    let (finish_tx, finish_rx) = tokio::sync::oneshot::channel::<()>();
    let finish_tx = Mutex::new(Some(finish_tx));
    let span_slot_for_run = Arc::clone(&span_slot);

    let mut start_attributes = IndexMap::new();
    // 顺序与 TS 的 `startAttributes = { ...attributes, api, ...{reasoning} }` 一致：
    // provider、model、api、reasoning（InMemory recorder 保留插入序）。
    start_attributes.insert(
        "provider".into(),
        AttributeValue::String(requested_model.provider.clone()),
    );
    start_attributes.insert(
        "model".into(),
        AttributeValue::String(requested_model.id.clone()),
    );
    start_attributes.insert(
        "api".into(),
        AttributeValue::String(requested_model.api.clone()),
    );
    if let Some(reasoning) = &options.reasoning {
        start_attributes.insert(
            "reasoning".into(),
            AttributeValue::String(reasoning.clone()),
        );
    }

    // 父级：显式 telemetry 上下文或 noop。`create_typed_span_starter` 只绑定上下文，
    // 传入的 schema 切片当前被忽略（兼容 TS 签名的占位）；属性名与必填性由本模块自己保证。
    let parent = options
        .telemetry
        .clone()
        .unwrap_or_else(noop_telemetry_context);
    let starter = create_typed_span_starter(parent, std::slice::from_ref(&*AI_TELEMETRY_SCHEMA));
    let name = if poll {
        "kiana.ai.poll"
    } else {
        "kiana.ai.request"
    }
    .to_owned();
    let mut span_future = Box::pin(async move {
        starter
            .start(
                &name,
                start_attributes,
                move |span: Arc<dyn TelemetrySpan>, _child| {
                    // 回调执行即代表 span 已创建：先写槽位供 observer 使用，再返回一个等待
                    // finish 的 future，让 span 直到终态结果才结算。
                    *span_slot_for_run.lock().unwrap_or_else(|e| e.into_inner()) = Some(span);
                    Box::pin(async move {
                        let _ = finish_rx.await;
                    })
                },
            )
            .await
    });
    // `void start(...)`：把 span 的启动同步推进到回调执行（callback 同步写入 span_slot），
    // 再把 future 交给后台任务等待 finish。这里只手动 poll 一次：内存适配器在这一步就会
    // 调用回调；若 starter 在回调前有真正的异步等待，一次 poll 不足以让槽位就绪，早到的
    // 事件会被 observer 跳过（见函数文档的失败模式）。
    {
        let waker = futures::task::noop_waker();
        let mut context = std::task::Context::from_waker(&waker);
        let _ = span_future.as_mut().poll(&mut context);
    }
    spawn_forward(async move {
        let _ = span_future.await;
    });

    // 阶段 3：构造 observer 的两个回调。`on_event` 与 `on_result` 各自持有所需的共享状态；
    // `first`/`tool_chars` 只在事件回调里使用，`span_slot` 两侧都要读，`instruments` 由
    // 两个回调共享。
    let first = Arc::new(std::sync::atomic::AtomicBool::new(true));
    let tool_chars: Arc<Mutex<HashMap<usize, usize>>> = Arc::new(Mutex::new(HashMap::new()));
    let instruments_for_events = Arc::clone(&instruments);
    let base_attributes_for_events = base_attributes.clone();
    let span_slot_for_events = Arc::clone(&span_slot);
    let started_for_events = started;

    LazyStreamObserver {
        on_event: Some(Box::new(move |event: &AssistantMessageEvent| {
            let span = span_slot_for_events
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone();
            // span 尚未就绪（starter 异步启动或流已先于 span 结束）：静默跳过本事件。
            // 注意这里不消费 `first`，因此第一个「可见」事件仍会被记为 first chunk。
            let Some(span) = span else {
                return;
            };
            // TS 里任何第一个流事件（包括 `start`）都触发 first_chunk；`swap` 保证只触发一次。
            if first.swap(false, std::sync::atomic::Ordering::SeqCst) {
                span.add_event("kiana.ai.first_chunk", None);
                instruments_for_events.first_chunk_duration.record(
                    started_for_events.elapsed().as_secs_f64() * 1000.0,
                    Some(base_attributes_for_events.clone()),
                    Some(span.as_ref()),
                );
            }
            // 内容块结束事件统一映射为 `kiana.ai.block`：记录块类型、下标与字符数；
            // tool_call 块另记工具名（不记工具参数内容）。文本/思考/工具参数的内容
            // 本身永不进入遥测（schema 声明的隐私约束）。
            match event {
                AssistantMessageEvent::ToolCallDelta {
                    content_index,
                    delta,
                    ..
                } => {
                    // 工具参数是分片到达的：只按 contentIndex 累计每个分片的 UTF-16 码元数
                    // （等价 JS 的 `delta.length`），不解析参数；结束事件时取走并删除。
                    let mut tool_chars = tool_chars.lock().unwrap_or_else(|e| e.into_inner());
                    *tool_chars.entry(*content_index).or_insert(0) += delta.encode_utf16().count();
                }
                AssistantMessageEvent::TextEnd {
                    content_index,
                    content,
                    ..
                } => {
                    span.add_event(
                        "kiana.ai.block",
                        Some(attributes(&[
                            ("type", AttributeValue::String("text".into())),
                            ("index", AttributeValue::Number(*content_index as f64)),
                            (
                                "chars",
                                AttributeValue::Number(content.encode_utf16().count() as f64),
                            ),
                        ])),
                    );
                }
                AssistantMessageEvent::ThinkingEnd {
                    content_index,
                    content,
                    ..
                } => {
                    span.add_event(
                        "kiana.ai.block",
                        Some(attributes(&[
                            ("type", AttributeValue::String("thinking".into())),
                            ("index", AttributeValue::Number(*content_index as f64)),
                            (
                                "chars",
                                AttributeValue::Number(content.encode_utf16().count() as f64),
                            ),
                        ])),
                    );
                }
                AssistantMessageEvent::ToolCallEnd {
                    content_index,
                    tool_call,
                    ..
                } => {
                    // 没有对应 delta 的工具块按 0 计；`remove` 同时清掉该索引的累计值。
                    let chars = tool_chars
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .remove(content_index)
                        .unwrap_or(0);
                    span.add_event(
                        "kiana.ai.block",
                        Some(attributes(&[
                            ("type", AttributeValue::String("tool_call".into())),
                            ("index", AttributeValue::Number(*content_index as f64)),
                            ("chars", AttributeValue::Number(chars as f64)),
                            ("tool", AttributeValue::String(tool_call.name.clone())),
                        ])),
                    );
                }
                _ => {}
            }
        })),
        on_result: Some(Box::new(move |message: Option<&AssistantMessage>| {
            let span = span_slot.lock().unwrap_or_else(|e| e.into_inner()).clone();
            // 只有「有终态消息」且「span 已就绪」才写结束属性；无结果时仅关闭 span。
            if let (Some(message), Some(span)) = (message, span) {
                // 键序与 TS 的 `setAttributes` 一致：stop、usage.*、cost.total、可选
                // response.id（InMemory recorder 保留插入序，测试会比对键序）。
                let mut end_attributes = IndexMap::new();
                end_attributes.insert(
                    "stop".into(),
                    AttributeValue::String(stop_reason_name(message.stop_reason)),
                );
                end_attributes.insert(
                    "usage.input".into(),
                    AttributeValue::Number(message.usage.input as f64),
                );
                end_attributes.insert(
                    "usage.output".into(),
                    AttributeValue::Number(message.usage.output as f64),
                );
                end_attributes.insert(
                    "usage.cache_read".into(),
                    AttributeValue::Number(message.usage.cache_read as f64),
                );
                end_attributes.insert(
                    "usage.cache_write".into(),
                    AttributeValue::Number(message.usage.cache_write as f64),
                );
                end_attributes.insert(
                    "cost.total".into(),
                    AttributeValue::Number(message.usage.cost.total),
                );
                if let Some(response_id) = &message.response_id {
                    end_attributes.insert(
                        "response.id".into(),
                        AttributeValue::String(response_id.clone()),
                    );
                }
                span.set_attributes(end_attributes);
                // 只有 `error` 终止才把 span 标成 error；`aborted` 保持 schema 的默认 ok。
                // 错误文本只写入 `status.error.message`（截断到 500 个 UTF-16 码元，对应
                // schema errorWhen 的约定），不写入 span 属性。
                if message.stop_reason == StopReason::Error {
                    span.set_status(SpanStatus::Error {
                        error: Some(StatusError {
                            name: "provider_error".into(),
                            message: owned_utf16_slice(
                                message.error_message.as_deref().unwrap_or(""),
                                0,
                                500,
                            ),
                        }),
                    });
                }
                // 时长对每次逻辑请求/轮询都记录（pending 的轮询也算一次），stop 作为标签。
                let mut duration_attributes = base_attributes.clone();
                duration_attributes.insert(
                    "stop".into(),
                    AttributeValue::String(stop_reason_name(message.stop_reason)),
                );
                instruments.request_duration.record(
                    started.elapsed().as_secs_f64() * 1000.0,
                    Some(duration_attributes),
                    Some(span.as_ref()),
                );
                // token/cost 只在最终结果上记录：普通请求总是记；deferred 轮询必须已经离开
                // pending/deferred，否则同一个逻辑请求会被每次轮询重复计费（schema 的
                // "pending and deferred polls are excluded"）。
                if !poll
                    || (message.stop_reason != StopReason::Pending
                        && message.stop_reason != StopReason::Deferred)
                {
                    let token_pairs = [
                        ("input", message.usage.input),
                        ("output", message.usage.output),
                        ("cache_read", message.usage.cache_read),
                        ("cache_write", message.usage.cache_write),
                    ];
                    for (kind, value) in token_pairs {
                        let mut token_attributes = base_attributes.clone();
                        token_attributes
                            .insert("kind".into(), AttributeValue::String(kind.to_owned()));
                        instruments.tokens.add(value as f64, Some(token_attributes));
                    }
                    instruments
                        .cost
                        .add(message.usage.cost.total, Some(base_attributes.clone()));
                }
            }
            // `take()` 让重复的 `on_result`（或没有消息的结束）只发送一次 finish；接收端
            // 是后台等待 span 结算的任务，可能已因取消被丢弃，发送失败无需处理。
            if let Some(finish_tx) = finish_tx.lock().unwrap_or_else(|e| e.into_inner()).take() {
                let _ = finish_tx.send(());
            }
        })),
    }
}

/// `StopReason` 的 TS 字面量（`done` 事件的 reason 子集用 camelCase）。
///
/// 返回值必须与 `STOP_VALUES` 使用同一组字符串：schema 的 `stop` 属性只允许那 7 个值。
fn stop_reason_name(reason: StopReason) -> String {
    match reason {
        StopReason::Pending => "pending",
        StopReason::Stop => "stop",
        StopReason::Length => "length",
        StopReason::ToolUse => "toolUse",
        StopReason::Error => "error",
        StopReason::Aborted => "aborted",
        StopReason::Deferred => "deferred",
        StopReason::TurnCapped => "turnCapped",
        StopReason::TokenCapped => "tokenCapped",
        StopReason::CostCapped => "costCapped",
        StopReason::TimeCapped => "timeCapped",
    }
    .to_owned()
}

/// `ownedSlice(text, 0, end)`：严格按 UTF-16 code unit 截断（`String.prototype.slice`）。
///
/// TS 在边界落在代理对中间时会留下孤立的高代理项（JS 字符串可以表示）；Rust 的 `String`
/// 不能，`from_utf16_lossy` 把孤立代理项替换为 U+FFFD。截断长度按 UTF-16 码元计算
/// （不超过 `end - start` 个码元），满足 schema 的 "at most 500 error-message characters"。
///
/// `start`/`end` 按 UTF-16 码元下标解释；越界会被夹到 `0..=len`，`end < start` 时输出空串
/// （与 `String.prototype.slice` 的边界行为一致）。
fn owned_utf16_slice(text: &str, start: usize, end: usize) -> String {
    let units: Vec<u16> = text.encode_utf16().collect();
    let start = start.min(units.len());
    let end = end.min(units.len()).max(start);
    String::from_utf16_lossy(&units[start..end])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schema_matches_ts_shape() {
        let schema = &*AI_TELEMETRY_SCHEMA;
        assert_eq!(schema.version, 1.0);
        let request = &schema.spans["kiana.ai.request"];
        assert_eq!(
            request.description,
            "One logical model request, including authentication and provider setup."
        );
        assert!(request.start_attributes["provider"].required);
        assert_eq!(
            schema.spans["kiana.ai.poll"].description,
            "One deferred-response poll, not the deferred request's lifetime."
        );
        let metrics = schema.metrics.as_ref().unwrap();
        assert_eq!(metrics.len(), 4);
        match &metrics["kiana.ai.tokens"] {
            TelemetryMetricDefinition::Counter { unit, .. } => {
                assert_eq!(unit.as_deref(), Some("token"))
            }
            _ => panic!("kiana.ai.tokens must be a counter"),
        }
    }

    #[test]
    fn instrument_cache_is_keyed_by_the_metrics_object_identity() {
        use kiana_telemetry::index::TelemetryContext;

        let first = kiana_telemetry::memory::InMemoryTelemetryContext::new();
        let second = kiana_telemetry::memory::InMemoryTelemetryContext::new();
        let first_metrics = first.metrics().expect("metrics");
        let second_metrics = second.metrics().expect("metrics");

        let a = instruments_for(&first_metrics);
        let b = instruments_for(&first_metrics);
        assert!(
            Arc::ptr_eq(&a, &b),
            "same adapter reuses grouped instruments"
        );
        let other = instruments_for(&second_metrics);
        assert!(
            !Arc::ptr_eq(&a, &other),
            "each adapter gets its own instruments"
        );
    }

    #[test]
    fn instrument_cache_does_not_keep_dead_adapters_or_their_data_alive() {
        use kiana_telemetry::index::TelemetryContext;

        let telemetry = kiana_telemetry::memory::InMemoryTelemetryContext::new();
        let metrics = telemetry.metrics().expect("metrics");
        let key = metrics_key(&metrics);
        let instruments = instruments_for(&metrics);
        // 先持锁再释放，避免并行测试在释放与断言之间命中同一 Arc 地址。
        let cache = INSTRUMENT_CACHE.lock().unwrap_or_else(|e| e.into_inner());
        assert!(cache.contains_key(&key));

        // 与 TS 的 WeakMap 一样：observer 释放 instruments、调用方释放 adapter 后，缓存不得
        // 继续持有它们（否则 Recorder/已记录数据会活到进程结束）。
        drop(instruments);
        drop(metrics);
        drop(telemetry);
        if let Some((cached_adapter, cached_instruments)) = cache.get(&key) {
            assert_eq!(cached_adapter.strong_count(), 0);
            assert_eq!(cached_instruments.strong_count(), 0);
        }
    }

    #[test]
    fn utf16_slice_truncates_exactly_like_the_ts_slice() {
        assert_eq!(owned_utf16_slice("abc", 0, 500), "abc");
        // 😀 = 两个 code unit；TS 的 `slice` 在中间截断会留下孤立高代理项，
        // Rust 无法表示，`from_utf16_lossy` 用 U+FFFD 替代，长度保持 2。
        let split = owned_utf16_slice("a😀b", 0, 2);
        assert_eq!(split.encode_utf16().count(), 2);
        assert_eq!(split, format!("a{}", '\u{FFFD}'));
        assert_eq!(owned_utf16_slice("a😀b", 0, 1), "a");
        assert_eq!(owned_utf16_slice("a😀b", 0, 3), "a😀");
        assert_eq!(owned_utf16_slice("a😀b", 4, 9), "");

        // schema 的 "at most 500"：绝不因代理对而超过 500 个码元。
        let message = format!("{}😀tail", "x".repeat(499));
        let bounded = owned_utf16_slice(&message, 0, 500);
        assert_eq!(bounded.encode_utf16().count(), 500);
        assert!(bounded.ends_with('\u{FFFD}'));
    }
}
