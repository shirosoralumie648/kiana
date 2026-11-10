//! `harness/telemetry.ts`：Durable 的无内容 span、事件与指标词表。
//!
//! TS: `packages/durable/src/harness/telemetry.ts`。ID 指向 durable 记录本身，从不指向内容。
//! 各定义按 TS 的字面量逐项转录（除产品前缀 `pi.`→`kiana.` 外，名称、描述、属性、取值、
//! 基数、单位全部一致）。
//!
//! # 这份文件的地位
//! schema 是纯数据：`define_telemetry_schema` 是恒等函数，本 crate 的运行时不会拿它校验
//! span/事件，真正的约束来自消费方按名查找：
//! - `harness/scheduler.rs` 从 `metrics` 里取 `kiana.durable.tasks.active` 与
//!   `kiana.durable.phase.duration`，缺失会在构造调度器时 panic；
//! - `harness/submissions.rs` 启动 `kiana.durable.submit`，`scheduler.rs` 启动
//!   `kiana.durable.phase` 并写 checkpoint 事件，`harness.rs` 写 `kiana.durable.report`
//!   事件与 `kiana.durable.reports` 计数器，`session.rs` 写 entry/submission 事件，
//!   `harness/tool.rs` 写 replay 事件；
//! - `kiana-telemetry/tests/fixtures/schema_json.json` 保存同一份词表的文本，黄金测试按
//!   语义比较。
//! 因此改名/改取值时必须同步改所有发射点与测试：指标查找会 panic，事件与属性则不会在本
//! crate 的运行时被校验发现。
//!
//! # 结构
//! - 两个 span：`kiana.durable.submit`（公开提交的接纳与提交）与 `kiana.durable.phase`
//!   （一次任务阶段调用），两者的 `parents` 都声明为 `Any`（允许任意父级）。
//! - 三个共用事件：`entry` / `submission` / `report`，两个 span 都声明；`phase` 另有
//!   `checkpoint` 与 `replay`。
//! - 三个指标：`reports`（counter）、`phase.duration`（histogram，单位 ms）、
//!   `tasks.active`（upDownCounter）。
//!
//! # 写作约定
//! 所有 `description`、属性名、取值与单位都是 TS 字面量的逐字转录（不改写、不翻译）；
//! `IndexMap` 保留声明顺序，测试按顺序断言，所以重排条目会改变序列化输出。start 属性带
//! `required` 维度，end 属性没有，因此 `optional()` 只取出定义本身。ID 只指向 durable
//! 记录，不要加内容属性（prompt/回答/参数）；`tests/harness_telemetry.rs` 会扫描属性名里的
//! 内容词，以及描述里的 `private-`/`gen_ai` 字样。

use std::sync::LazyLock;

use indexmap::IndexMap;
use kiana_telemetry::index::{
    Cardinality, TelemetryAttributeDefinition, TelemetryAttributeMetadata,
    TelemetryEventDefinition, TelemetryParentDefinition, TelemetryRequiredAttributeDefinition,
    TelemetrySchemaDefinition, TelemetrySpanDefinition, TelemetryStatusDefinition,
    define_telemetry_schema,
};
use kiana_telemetry::metrics::TelemetryMetricDefinition;

/// 构造一个 string 属性定义（start 属性/事件/指标属性使用）。
///
/// `values` 是允许取值域，`None` 表示不限定；`required` 只有在放进带 required 维度的表时
/// 才有意义。`sensitive`/`examples` 在 TS 的这份词表里都没有使用，固定为 `None`。
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

/// 构造一个 number 属性定义；本文件只用于 ID/序号类数值（多数标为高基数）。
fn number_attribute_with_cardinality(
    description: &str,
    required: bool,
    cardinality: Option<Cardinality>,
) -> TelemetryRequiredAttributeDefinition {
    TelemetryRequiredAttributeDefinition {
        definition: TelemetryAttributeDefinition::Number {
            meta: TelemetryAttributeMetadata {
                description: description.to_owned(),
                sensitive: None,
                cardinality,
            },
            values: None,
            examples: None,
        },
        required,
    }
}

/// 构造一个 boolean 属性定义；TS 词表里只有 phase span 的 `resumed` 使用。
fn boolean_attribute(description: &str, required: bool) -> TelemetryRequiredAttributeDefinition {
    TelemetryRequiredAttributeDefinition {
        definition: TelemetryAttributeDefinition::Boolean {
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

/// 把带 `required` 的属性定义降为 end 属性定义。
///
/// `TelemetrySpanDefinition::end_attributes` 的值类型是 `TelemetryAttributeDefinition`，
/// 没有 required 维度；TS 里 end 属性也从不声明 `required`，所以这里只是取出内层定义。
fn optional(attribute: TelemetryRequiredAttributeDefinition) -> TelemetryAttributeDefinition {
    attribute.definition
}

/// 按给定顺序构造属性表。
///
/// 用 `IndexMap` 保留声明顺序：schema 会被序列化，测试也按这个顺序断言，重排条目会改变输出。
fn attributes(
    entries: Vec<(&str, TelemetryRequiredAttributeDefinition)>,
) -> IndexMap<String, TelemetryRequiredAttributeDefinition> {
    entries
        .into_iter()
        .map(|(name, attribute)| (name.to_owned(), attribute))
        .collect()
}

/// 报告事件的 `type` 允许值：与 `ReportInfo` 的九个变体一一对应（映射见
/// `harness.rs` 的 `report_kind`），顺序与 TS 字面量一致。
const REPORT_TYPES: [&str; 9] = [
    "session_failed",
    "task_faulted",
    "task_parked",
    "migration_failed",
    "task_outdated",
    "task_error",
    "extension_error",
    "host_error",
    "listener_error",
];
/// 固定报告原因（`reason`）的允许值；只在若干 `ReportInfo` 变体上出现。
const REPORT_REASONS: [&str; 4] = [
    "threw",
    "restart_limit",
    "missing_task",
    "incompatible_task",
];

/// `kiana.durable.entry` 事件：只记录条目 ID 与 kind，内容仍留在 transcript 里。
fn entry_event() -> TelemetryEventDefinition {
    TelemetryEventDefinition {
        description: "A transcript entry was committed; content remains in the transcript."
            .to_owned(),
        attributes: attributes(vec![
            (
                "entry.id",
                number_attribute_with_cardinality(
                    "Transcript entry ID.",
                    true,
                    Some(Cardinality::High),
                ),
            ),
            (
                "entry.kind",
                string_attribute("Entry kind.", true, None, None),
            ),
        ]),
    }
}

/// `reportAttributes`（报告事件与 `kiana.durable.reports` 指标共用）。
///
/// 这里定义完整四项；指标只按同一份 `type` 取值表重建自己的属性（见 [`metrics`]）。
fn report_attributes() -> IndexMap<String, TelemetryRequiredAttributeDefinition> {
    attributes(vec![
        (
            "type",
            string_attribute("ReportInfo type.", true, Some(&REPORT_TYPES), None),
        ),
        (
            "reason",
            string_attribute(
                "Fixed report reason, when present.",
                false,
                Some(&REPORT_REASONS),
                None,
            ),
        ),
        (
            "extension",
            string_attribute(
                "Registered extension name, when present.",
                false,
                None,
                None,
            ),
        ),
        (
            "task.id",
            number_attribute_with_cardinality(
                "Task ID, when present.",
                false,
                Some(Cardinality::High),
            ),
        ),
    ])
}

/// `kiana.durable.report` 事件：只记录报告类型与可选上下文，不记录错误 payload 或内容。
fn report_event() -> TelemetryEventDefinition {
    TelemetryEventDefinition {
        description: "A report was raised; no error payload is recorded.".to_owned(),
        attributes: report_attributes(),
    }
}

/// `kiana.durable.submission` 事件：一次提交落定，`status` 只有 `done`/`unanswered`。
fn submission_event() -> TelemetryEventDefinition {
    TelemetryEventDefinition {
        description: "A submission was settled.".to_owned(),
        attributes: attributes(vec![
            (
                "submission.id",
                number_attribute_with_cardinality("Submission ID.", true, Some(Cardinality::High)),
            ),
            (
                "status",
                string_attribute("Settlement.", true, Some(&["done", "unanswered"]), None),
            ),
            (
                "answer.entry_id",
                number_attribute_with_cardinality(
                    "Answer entry ID.",
                    false,
                    Some(Cardinality::High),
                ),
            ),
        ]),
    }
}

/// `kiana.durable.submit` span：公开提交的接纳与它引发的提交。
///
/// - start：`submission.type`（input/write）与 `conversation.id`，两者都必填；
/// - end：`submission.id`（被接纳的提交，可缺省）与 `result`（queued/placed/done，
///   其中 stale write 也报 `done`）；
/// - events：entry/submission/report 三个共用事件；
/// - status：默认 `ok`，`error_when` 是“接纳失败”的文本约定（运行时不做求值）。
fn submit_span() -> TelemetrySpanDefinition {
    let mut end_attributes = IndexMap::new();
    // end 属性没有 required 维度；这两个属性都可缺省，所以统一经 `optional` 降级。
    end_attributes.insert(
        "submission.id".to_owned(),
        optional(number_attribute_with_cardinality(
            "Admitted submission ID.",
            false,
            Some(Cardinality::High),
        )),
    );
    end_attributes.insert(
        "result".to_owned(),
        optional(string_attribute(
            "Admission result; stale writes report done.",
            false,
            Some(&["queued", "placed", "done"]),
            None,
        )),
    );

    // 三个共用事件定义在函数外，两个 span 都复用同一组构造。
    let mut events = IndexMap::new();
    events.insert("kiana.durable.entry".to_owned(), entry_event());
    events.insert("kiana.durable.submission".to_owned(), submission_event());
    events.insert("kiana.durable.report".to_owned(), report_event());

    TelemetrySpanDefinition {
        description: "Public submission admission and its commit. Inputs start root traces linked to their callers; writes are children. Internal admissions emit phase events only.".to_owned(),
        parents: Some(TelemetryParentDefinition::Any),
        start_attributes: attributes(vec![
            (
                "submission.type",
                string_attribute(
                    "Submission type.",
                    true,
                    Some(&["input", "write"]),
                    None,
                ),
            ),
            (
                "conversation.id",
                number_attribute_with_cardinality(
                    "Conversation ID.",
                    true,
                    Some(Cardinality::High),
                ),
            ),
        ]),
        end_attributes,
        events: Some(events),
        status: TelemetryStatusDefinition {
            default: "ok".to_owned(),
            error_when: "Admission failed.".to_owned(),
        },
    }
}

/// `kiana.durable.phase` span：一次任务阶段调用及其决策提交（不含等待/重启）。
///
/// - start：任务 ID/名称、phase、kind（run/abort）、可选 attempt、resumed；
/// - end：`outcome`（下一 phase、waiting、sleeping、done、faulted、aborted 等）与本次
///   调用的工具名/调用 ID，在 schema 中全部可缺省；
/// - events：三个共用事件，外加 `checkpoint`（运行中 checkpoint 的 phase 变化）与
///   `replay`（嵌套工具调用由存储的结果应答）；
/// - status：默认 `ok`；`error_when` 的文本声称 faulted/failed/orphaned 分别改用
///   task_faulted/task_failed/task_orphaned 并把诊断截到 500 字符，aborted 仍是 ok。
fn phase_span() -> TelemetrySpanDefinition {
    let mut end_attributes = IndexMap::new();
    // end 属性没有 required 维度；这些属性都可缺省，所以统一经 `optional` 降级。
    end_attributes.insert(
        "outcome".to_owned(),
        optional(string_attribute(
            "next=<phase>, waiting, sleeping, done, faulted, or aborted.",
            false,
            None,
            None,
        )),
    );
    end_attributes.insert(
        "tool.name".to_owned(),
        optional(string_attribute("Called tool name.", false, None, None)),
    );
    end_attributes.insert(
        "tool.call_id".to_owned(),
        optional(string_attribute(
            "Transcript tool call ID.",
            false,
            None,
            Some(Cardinality::High),
        )),
    );

    // checkpoint 事件只带新 phase；replay 事件只带工具名（不携带参数/结果）。
    let mut checkpoint_attributes = IndexMap::new();
    checkpoint_attributes.insert(
        "phase".to_owned(),
        string_attribute("Committed checkpoint phase.", true, None, None),
    );
    let mut replay_attributes = IndexMap::new();
    replay_attributes.insert(
        "tool.name".to_owned(),
        string_attribute("Called tool name.", true, None, None),
    );

    let mut events = IndexMap::new();
    events.insert("kiana.durable.entry".to_owned(), entry_event());
    events.insert("kiana.durable.submission".to_owned(), submission_event());
    events.insert("kiana.durable.report".to_owned(), report_event());
    events.insert(
        "kiana.durable.checkpoint".to_owned(),
        TelemetryEventDefinition {
            description: "The running checkpoint's phase changed while the invocation continued."
                .to_owned(),
            attributes: checkpoint_attributes,
        },
    );
    events.insert(
        "kiana.durable.replay".to_owned(),
        TelemetryEventDefinition {
            description: "A nested tool call was answered from its stored result.".to_owned(),
            attributes: replay_attributes,
        },
    );

    TelemetrySpanDefinition {
        description:
            "One task phase invocation, including its decision commit, never a wait or restart."
                .to_owned(),
        parents: Some(TelemetryParentDefinition::Any),
        start_attributes: attributes(vec![
            (
                "task.id",
                number_attribute_with_cardinality("Task ID.", true, Some(Cardinality::High)),
            ),
            (
                "task.name",
                string_attribute("Registered task name.", true, None, None),
            ),
            (
                "phase",
                string_attribute("Invoked checkpoint phase.", true, None, None),
            ),
            (
                "kind",
                string_attribute(
                    "Invocation kind.",
                    true,
                    Some(&["run", "abort"]),
                    None,
                ),
            ),
            (
                "attempt",
                number_attribute_with_cardinality("Checkpoint attempt, when present.", false, None),
            ),
            (
                "resumed",
                boolean_attribute("First invocation of a task loaded in this process.", true),
            ),
        ]),
        end_attributes,
        events: Some(events),
        status: TelemetryStatusDefinition {
            default: "ok".to_owned(),
            error_when: "Faulted, failed, and orphaned outcomes use task_faulted, task_failed, and task_orphaned with a diagnostic capped at 500 characters. Aborted outcomes keep status ok.".to_owned(),
        },
    }
}

/// 指标词表：报告计数、阶段耗时直方图、活跃任务上下计数。
///
/// 名字与 kind 被 `harness.rs`/`scheduler.rs` 按名查找，顺序会被测试与序列化固定：
/// - `kiana.durable.reports`：counter，只按 `type` 分组；
/// - `kiana.durable.phase.duration`：histogram，单位 ms，属性 task/phase/outcome；
/// - `kiana.durable.tasks.active`：upDownCounter，属性是注册任务名（描述声明在任务落定
///   或关闭时递减）。
fn metrics() -> IndexMap<String, TelemetryMetricDefinition> {
    // 指标只复用 `type` 一个属性；reason/extension/task.id 只属于报告事件。
    let mut reports_attributes = IndexMap::new();
    reports_attributes.insert(
        "type".to_owned(),
        string_attribute("ReportInfo type.", true, Some(&REPORT_TYPES), None),
    );

    let mut metrics = IndexMap::new();
    metrics.insert(
        "kiana.durable.reports".to_owned(),
        TelemetryMetricDefinition::Counter {
            description: "Reports raised, grouped by their fixed ReportInfo type.".to_owned(),
            unit: None,
            attributes: reports_attributes,
        },
    );
    metrics.insert(
        "kiana.durable.phase.duration".to_owned(),
        TelemetryMetricDefinition::Histogram {
            description: "Duration of a phase invocation and its decision commit.".to_owned(),
            unit: Some("ms".to_owned()),
            attributes: attributes(vec![
                (
                    "task",
                    string_attribute("Registered task name.", true, None, None),
                ),
                (
                    "phase",
                    string_attribute("Invoked checkpoint phase.", true, None, None),
                ),
                (
                    "outcome",
                    string_attribute("Phase outcome.", true, None, None),
                ),
            ]),
            boundaries: None,
        },
    );
    metrics.insert(
        "kiana.durable.tasks.active".to_owned(),
        TelemetryMetricDefinition::UpDownCounter {
            description:
                "Live tasks held by this Harness; decremented at terminal settlement or close."
                    .to_owned(),
            unit: None,
            attributes: attributes(vec![(
                "task",
                string_attribute("Registered task name.", true, None, None),
            )]),
        },
    );
    metrics
}

/// 组装整份 schema：版本 1.0，先 submit 后 phase，指标可选表。
///
/// `spans`/`metrics` 的插入顺序就是序列化顺序。span 名顺序由本文件 `mod tests` 的
/// `schema_matches_the_ts_vocabulary` 断言；`tests/harness_telemetry.rs` 断言指标名与属性
/// 顺序（`kiana-telemetry` 的黄金向量只按语义比较，不要求键序逐字节一致）。
fn build_schema() -> TelemetrySchemaDefinition {
    let mut spans = IndexMap::new();
    spans.insert("kiana.durable.submit".to_owned(), submit_span());
    spans.insert("kiana.durable.phase".to_owned(), phase_span());
    define_telemetry_schema(TelemetrySchemaDefinition {
        version: 1.0,
        spans,
        metrics: Some(metrics()),
    })
}

/// Durable 的无内容 span、事件与指标词表；ID 指向 durable 记录，从不指向内容。
///
/// TS: `DURABLE_TELEMETRY_SCHEMA`
///
/// 第一次解引用时构建（`LazyLock`），之后全进程共享同一份不可变 schema。内容与 TS 词表
/// 逐项对应；改这里之前先确认所有发射点（见模块文档）、`tests/harness_telemetry.rs` 的
/// 词表断言，以及 `kiana-telemetry/tests/schema_golden.rs` 的黄金向量。
pub static DURABLE_TELEMETRY_SCHEMA: LazyLock<TelemetrySchemaDefinition> =
    LazyLock::new(build_schema);

// 锁定词表本身：span/事件的属性名与顺序、取值域、基数、单位、状态文本。发射行为不在本
// 文件测试范围内（见 tests/harness_telemetry.rs 的模块说明）；“不含内容属性”的约束也由该
// 文件的 no_attribute_names_or_descriptions_reference_recorded_content 覆盖，本文件没有
// 对应断言。
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schema_matches_the_ts_vocabulary() {
        let schema = &*DURABLE_TELEMETRY_SCHEMA;
        assert_eq!(schema.version, 1.0);
        let span_names: Vec<&str> = schema.spans.keys().map(String::as_str).collect();
        assert_eq!(
            span_names,
            vec!["kiana.durable.submit", "kiana.durable.phase"]
        );

        let submit = &schema.spans["kiana.durable.submit"];
        assert_eq!(
            submit
                .start_attributes
                .keys()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            vec!["submission.type", "conversation.id"]
        );
        assert_eq!(
            submit.status,
            TelemetryStatusDefinition {
                default: "ok".to_owned(),
                error_when: "Admission failed.".to_owned(),
            }
        );

        let phase = &schema.spans["kiana.durable.phase"];
        let phase_events: Vec<&str> = phase
            .events
            .as_ref()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(
            phase_events,
            vec![
                "kiana.durable.entry",
                "kiana.durable.submission",
                "kiana.durable.report",
                "kiana.durable.checkpoint",
                "kiana.durable.replay",
            ]
        );

        let metrics = schema.metrics.as_ref().unwrap();
        let metric_names: Vec<&str> = metrics.keys().map(String::as_str).collect();
        assert_eq!(
            metric_names,
            vec![
                "kiana.durable.reports",
                "kiana.durable.phase.duration",
                "kiana.durable.tasks.active",
            ]
        );
        match &metrics["kiana.durable.phase.duration"] {
            TelemetryMetricDefinition::Histogram { unit, .. } => {
                assert_eq!(unit.as_deref(), Some("ms"));
            }
            other => panic!("expected histogram, got {other:?}"),
        }
        match &metrics["kiana.durable.reports"] {
            TelemetryMetricDefinition::Counter { attributes, .. } => {
                match &attributes["type"].definition {
                    TelemetryAttributeDefinition::String { values, .. } => {
                        assert_eq!(values.as_ref().unwrap().len(), 9);
                        assert_eq!(values.as_ref().unwrap()[0], "session_failed");
                    }
                    other => panic!("expected string attribute, got {other:?}"),
                }
            }
            other => panic!("expected counter, got {other:?}"),
        }
    }

    #[test]
    fn report_event_carries_the_shared_report_attributes() {
        let schema = &*DURABLE_TELEMETRY_SCHEMA;
        let report = &schema.spans["kiana.durable.submit"]
            .events
            .as_ref()
            .unwrap()["kiana.durable.report"];
        let names: Vec<&str> = report.attributes.keys().map(String::as_str).collect();
        assert_eq!(names, vec!["type", "reason", "extension", "task.id"]);
        match &report.attributes["reason"].definition {
            TelemetryAttributeDefinition::String { values, .. } => {
                assert_eq!(
                    values.as_ref().unwrap(),
                    &vec![
                        "threw".to_owned(),
                        "restart_limit".to_owned(),
                        "missing_task".to_owned(),
                        "incompatible_task".to_owned()
                    ]
                );
            }
            other => panic!("expected string attribute, got {other:?}"),
        }
        assert!(!report.attributes["task.id"].required);
        // TS 声明 `cardinality: "high"`（telemetry.ts:34）。
        match &report.attributes["task.id"].definition {
            TelemetryAttributeDefinition::Number { meta, .. } => {
                assert_eq!(meta.cardinality, Some(Cardinality::High));
            }
            other => panic!("expected number attribute, got {other:?}"),
        }
        assert!(report.attributes["type"].required);
    }
}
