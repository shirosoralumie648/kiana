//! 供快照/黄金文件使用的 JS 语义 JSON 桥接。
//!
//! `Recorded*` 与 `Stored*` 结构体本身用 serde 派生（`serde_json` 的数字格式化与 JS 不同），
//! 因此任何要写成文本的地方都先转成 `serde_json::Value` 再走
//! [`kiana_json::js_object::stringify`]：数字按 ECMAScript `Number::toString` 输出、整数样键前置、
//! `NaN`/`Infinity` → `null`、`-0` → `0`。
//!
//! 主要使用者：`tests/propagation.rs` 的子进程 worker（打印 `getSpans()` JSON）与黄金文件。
//! TS 对应物是 `JSON.stringify(...)`（无缩进）。
//!
//! # 为什么需要这个文件
//!
//! 快照只用于测试/黄金文件断言。直接对 `Recorded*` / `Stored*` 调用 `serde_json::to_string`
//! 会走 serde_json 的浮点格式化（指数阈值与写法跟 ECMAScript `Number::toString` 不同），也无法
//! 保证对象键序，黄金文件就失去逐字节比较的意义。这里先把结构体投影成 `serde_json::Value`，
//! 再统一用 `kiana_json::js_object::stringify` 写出，把数字与键序的 JS 语义收敛到一处。
//!
//! # 边界与约定
//!
//! - 本模块只做“结构体 → `Value` → 文本”的投影：不校验快照的业务不变量，也不修改入参。
//! - 每个函数按快照结构体的字段声明顺序 `insert`；文本层由 `stringify` 再按 JS 规则输出
//!   （整数样键升序在前，其余保持插入顺序）。workspace 的 `serde_json` 开启了 `preserve_order`
//!   feature（根 `Cargo.toml`），`serde_json::Map` 才会保留插入顺序；关闭它键序会变成字典序。
//! - 所有 `f64` 经 `jsval::number_value`：非有限值写成 `null`（`JSON.stringify` 语义），
//!   有限值原样保留；`-0` 的文本在 `format_js_number` 层变成 `0`。
//! - 可选字段的处理并不统一，不能互换：缺失时**省略键**（TS 的 `undefined` 会被
//!   `JSON.stringify` 忽略）的位置有 `endSequence`、measurement 快照的
//!   `description`/`unit`/`boundaries`、`StoredMetric.unit`、`StoredSpan.parent_id`、
//!   `StoredSpanLink.attributes`，以及 `SpanStatus::Error { error: None }` 时的
//!   `status.error`（见 `jsval::status_to_value`）；缺失时写 `null` 的位置有
//!   `RecordedTelemetrySpan.parentId`、`parentContext`、measurement 的 `context`；
//!   `StoredMetric.kind` 缺失时写空字符串 `""`（第三类，见 [`stored_metrics_to_value`]）。
//!
//! # 调用关系
//!
//! 生产代码不依赖本模块；调用方是 `tests/propagation.rs`（子进程 worker 打印 JSON）、
//! `tests/store.rs`、`tests/memory_edges.rs` 与 `tests/memory_snapshots.rs`（断言快照/黄金文件）。
//!
//! # 阅读顺序
//!
//! 先看 `recorded_*`（`getSpans()`/`getMetrics()` 的直接快照），再看 `stored_*`（store 查询
//! 结果）；各类投影共用同一套 `jsval` 原语。

use serde_json::Value;

use crate::index::{StoredMetric, StoredTrace};
use crate::jsval;
use crate::memory::{RecordedTelemetryMeasurement, RecordedTelemetrySpan};

/// `JSON.stringify(recordedTelemetrySpan)`。
///
/// 字段投影规则（与 TS 的 `getSpans()` 快照逐字段对应）：
/// - `id` / `parentId`：recorder 内的数字 id；`parentId` 为 `None`（根或外部父级）时写
///   `null`；同 scope 的父 id 按 `Number()` 语义解析，结果为 `NaN` 时同样写 `null`
///   （空串/纯空白解析为 0，`0x`/`0o`/`0b` 前缀按对应进制解析；见 `tests/memory_edges.rs`
///   的 `localIds` 用例与 `tests/fixtures/memory_edges.json`）；
/// - `context` / `parentContext`：本地 carrier（`kiana-memory-*` 键）；
/// - `links`：每项都带 `context` 与 `attributes` —— `RecordedTelemetryLink.attributes`
///   不是 `Option`，所以没有属性时也写成 `{}`；
/// - `events`：每项都带 `name` 与 `attributes`；
/// - `status`、`settled` 恒出现；`endSequence` 只在 span 已结算时出现。
///
/// 参数是借用，返回新构造的 `Value`，不会修改 span。非有限数字（例如 `Number()` 从 span id
/// 解析出的 `NaN` 父 id）在这里变成 `Value::Null`，最终文本即 `null`。
pub fn recorded_span_to_value(span: &RecordedTelemetrySpan) -> Value {
    let mut map = serde_json::Map::new();
    map.insert("id".to_string(), jsval::number_value(span.id));
    map.insert(
        "parentId".to_string(),
        match span.parent_id {
            Some(value) => jsval::number_value(value),
            None => Value::Null,
        },
    );
    map.insert(
        "context".to_string(),
        jsval::carrier_to_value(&span.context),
    );
    map.insert(
        "parentContext".to_string(),
        match &span.parent_context {
            Some(carrier) => jsval::carrier_to_value(carrier),
            None => Value::Null,
        },
    );
    map.insert(
        "links".to_string(),
        Value::Array(
            span.links
                .iter()
                .map(|link| {
                    let mut link_map = serde_json::Map::new();
                    link_map.insert(
                        "context".to_string(),
                        jsval::carrier_to_value(&link.context),
                    );
                    link_map.insert(
                        "attributes".to_string(),
                        jsval::attributes_to_value(&link.attributes),
                    );
                    Value::Object(link_map)
                })
                .collect(),
        ),
    );
    map.insert("name".to_string(), Value::String(span.name.clone()));
    map.insert(
        "attributes".to_string(),
        jsval::attributes_to_value(&span.attributes),
    );
    map.insert(
        "events".to_string(),
        Value::Array(
            span.events
                .iter()
                .map(|event| {
                    let mut event_map = serde_json::Map::new();
                    event_map.insert("name".to_string(), Value::String(event.name.clone()));
                    event_map.insert(
                        "attributes".to_string(),
                        jsval::attributes_to_value(&event.attributes),
                    );
                    Value::Object(event_map)
                })
                .collect(),
        ),
    );
    map.insert("status".to_string(), jsval::status_to_value(&span.status));
    map.insert("settled".to_string(), Value::Bool(span.settled));
    if let Some(sequence) = span.end_sequence {
        map.insert("endSequence".to_string(), jsval::number_value(sequence));
    }
    Value::Object(map)
}

/// `JSON.stringify(recordedTelemetrySpan[])`。
///
/// 数组顺序就是入参顺序（recorder 的记录顺序），不排序、不去重；空切片得到 `[]`。
pub fn recorded_spans_to_value(spans: &[RecordedTelemetrySpan]) -> Value {
    Value::Array(spans.iter().map(recorded_span_to_value).collect())
}

/// `JSON.stringify(getSpans())`。
///
/// 等价于 `value_to_json(&recorded_spans_to_value(spans))`：先投影再走 JS 语义文本化，
/// 非有限数字在文本中写作 `null`。返回紧凑 JSON（无空格、无换行）。
pub fn recorded_spans_to_json(spans: &[RecordedTelemetrySpan]) -> String {
    kiana_json::js_object::stringify(&recorded_spans_to_value(spans))
}

/// `JSON.stringify(recordedTelemetryMeasurement)`。
///
/// `description` / `unit` / `boundaries` 为 `None` 时整个键省略（对应 TS 的 `undefined`），
/// 而不是写成 `null`；`context` 为 `None`（没有 exemplar 上下文）时写 `null`。
/// `kind` 的三个变体分别映射为 `"counter"` / `"upDownCounter"` / `"histogram"`。
pub fn recorded_measurement_to_value(measurement: &RecordedTelemetryMeasurement) -> Value {
    let mut map = serde_json::Map::new();
    map.insert(
        "kind".to_string(),
        Value::String(
            match measurement.kind {
                crate::index::MetricKind::Counter => "counter",
                crate::index::MetricKind::UpDownCounter => "upDownCounter",
                crate::index::MetricKind::Histogram => "histogram",
            }
            .to_string(),
        ),
    );
    map.insert("name".to_string(), Value::String(measurement.name.clone()));
    if let Some(description) = &measurement.description {
        map.insert(
            "description".to_string(),
            Value::String(description.clone()),
        );
    }
    if let Some(unit) = &measurement.unit {
        map.insert("unit".to_string(), Value::String(unit.clone()));
    }
    if let Some(boundaries) = &measurement.boundaries {
        map.insert(
            "boundaries".to_string(),
            Value::Array(
                boundaries
                    .iter()
                    .map(|value| jsval::number_value(*value))
                    .collect(),
            ),
        );
    }
    map.insert("value".to_string(), jsval::number_value(measurement.value));
    map.insert(
        "attributes".to_string(),
        jsval::attributes_to_value(&measurement.attributes),
    );
    map.insert(
        "context".to_string(),
        match &measurement.context {
            Some(carrier) => jsval::carrier_to_value(carrier),
            None => Value::Null,
        },
    );
    Value::Object(map)
}

/// `JSON.stringify(recordedTelemetryMeasurement[])`。
///
/// 保持 recorder 的测量写入顺序（内存后端是追加顺序），不做聚合或排序。
pub fn recorded_measurements_to_value(measurements: &[RecordedTelemetryMeasurement]) -> Value {
    Value::Array(
        measurements
            .iter()
            .map(recorded_measurement_to_value)
            .collect(),
    )
}

/// `JSON.stringify(storedMetric[])`。
///
/// 每个元素的键固定为 `intervalMs`、`name`、`kind`、`unit?`、`attributes`、`count`、`sum`、
/// `min`、`max`。`kind` 为 `None` 时本投影写出空字符串 `""`（既不是省略也不是 `null`）；
/// `unit` 为 `None` 时省略键。`count`/`sum`/`min`/`max` 都是 `f64`，非有限值写成 `null`。
pub fn stored_metrics_to_value(metrics: &[StoredMetric]) -> Value {
    Value::Array(
        metrics
            .iter()
            .map(|metric| {
                let mut map = serde_json::Map::new();
                map.insert(
                    "intervalMs".to_string(),
                    jsval::number_value(metric.interval_ms),
                );
                map.insert("name".to_string(), Value::String(metric.name.clone()));
                map.insert(
                    "kind".to_string(),
                    Value::String(
                        match metric.kind {
                            Some(crate::index::MetricKind::Counter) => "counter",
                            Some(crate::index::MetricKind::UpDownCounter) => "upDownCounter",
                            Some(crate::index::MetricKind::Histogram) => "histogram",
                            None => "",
                        }
                        .to_string(),
                    ),
                );
                if let Some(unit) = &metric.unit {
                    map.insert("unit".to_string(), Value::String(unit.clone()));
                }
                map.insert(
                    "attributes".to_string(),
                    jsval::attributes_to_value(&metric.attributes),
                );
                map.insert("count".to_string(), jsval::number_value(metric.count));
                map.insert("sum".to_string(), jsval::number_value(metric.sum));
                map.insert("min".to_string(), jsval::number_value(metric.min));
                map.insert("max".to_string(), jsval::number_value(metric.max));
                Value::Object(map)
            })
            .collect(),
    )
}

/// `JSON.stringify(storedTrace)`。
///
/// span 的 `parentId` 与 link 的 `attributes` 在缺失时**省略键**（TS 的 `undefined` 不会
/// 出现在 `JSON.stringify` 结果里）；`startedMs` / `endedMs` / 事件 `timeMs` 走 JS 数字语义
/// （非有限 → `null`）。span 数组保持 `StoredTrace.spans` 的顺序，不重新排序。
pub fn stored_trace_to_value(trace: &StoredTrace) -> Value {
    let mut map = serde_json::Map::new();
    map.insert("traceId".to_string(), Value::String(trace.trace_id.clone()));
    map.insert(
        "spans".to_string(),
        Value::Array(
            trace
                .spans
                .iter()
                .map(|span| {
                    let mut span_map = serde_json::Map::new();
                    span_map.insert("id".to_string(), Value::String(span.id.clone()));
                    span_map.insert("traceId".to_string(), Value::String(span.trace_id.clone()));
                    if let Some(parent) = &span.parent_id {
                        span_map.insert("parentId".to_string(), Value::String(parent.clone()));
                    }
                    span_map.insert("name".to_string(), Value::String(span.name.clone()));
                    span_map.insert(
                        "startedMs".to_string(),
                        jsval::number_value(span.started_ms),
                    );
                    span_map.insert("endedMs".to_string(), jsval::number_value(span.ended_ms));
                    span_map.insert(
                        "attributes".to_string(),
                        jsval::attributes_to_value(&span.attributes),
                    );
                    span_map.insert(
                        "events".to_string(),
                        Value::Array(
                            span.events
                                .iter()
                                .map(|event| {
                                    let mut event_map = serde_json::Map::new();
                                    event_map.insert(
                                        "name".to_string(),
                                        Value::String(event.name.clone()),
                                    );
                                    event_map.insert(
                                        "timeMs".to_string(),
                                        jsval::number_value(event.time_ms),
                                    );
                                    event_map.insert(
                                        "attributes".to_string(),
                                        jsval::attributes_to_value(&event.attributes),
                                    );
                                    Value::Object(event_map)
                                })
                                .collect(),
                        ),
                    );
                    span_map.insert(
                        "links".to_string(),
                        Value::Array(
                            span.links
                                .iter()
                                .map(|link| {
                                    let mut link_map = serde_json::Map::new();
                                    link_map.insert(
                                        "traceId".to_string(),
                                        Value::String(link.trace_id.clone()),
                                    );
                                    link_map.insert(
                                        "spanId".to_string(),
                                        Value::String(link.span_id.clone()),
                                    );
                                    if let Some(attributes) = &link.attributes {
                                        link_map.insert(
                                            "attributes".to_string(),
                                            jsval::attributes_to_value(attributes),
                                        );
                                    }
                                    Value::Object(link_map)
                                })
                                .collect(),
                        ),
                    );
                    span_map.insert("status".to_string(), jsval::status_to_value(&span.status));
                    Value::Object(span_map)
                })
                .collect(),
        ),
    );
    Value::Object(map)
}

/// `JSON.stringify(storedTrace[])`。
///
/// 数组顺序保持入参顺序（例如 `findTraces` 的返回顺序），空切片得到 `[]`。
pub fn stored_traces_to_value(traces: &[StoredTrace]) -> Value {
    Value::Array(traces.iter().map(stored_trace_to_value).collect())
}

/// `JSON.stringify(telemetrySchema)`：schema 的全部字段都很简单，直接复用 serde 再按键序重排。
///
/// 输入已经是 `serde_json::Value`，本函数不复制也不修改它；键序由
/// `kiana_json::js_object::stringify` 在写出时统一套用 JS 规则（整数样键升序在前，其余保持
/// `Value` 的插入顺序）。调用方负责先把 schema 序列化成 `Value`——直接用 serde 派生时，
/// 字段顺序是 Rust 结构体声明顺序，和 TS 对象的属性顺序不一定相同。
pub fn value_to_json(value: &Value) -> String {
    kiana_json::js_object::stringify(value)
}
