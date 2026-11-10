//! `test/harness-telemetry.test.ts` 的 Rust 端口（schema 词表部分）。
//!
//! TS 测试用 `InMemoryTelemetryContext` 驱动完整的 `Harness`，断言 submit/phase span 的嵌套、
//! checkpoint、entry 事件、指标以及「不记录内容」。Harness 与任务循环属于其他单元，这里端口
//! 测试中对词表本身的断言：span/事件/指标的属性、取值、基数、单位与状态语义，以及词表里不存在
//! 任何内容属性。

use indexmap::IndexMap;
use kiana_durable::harness::telemetry::DURABLE_TELEMETRY_SCHEMA;
use kiana_telemetry::index::{
    Cardinality, TelemetryAttributeDefinition, TelemetryParentDefinition,
    TelemetryRequiredAttributeDefinition,
};
use kiana_telemetry::metrics::TelemetryMetricDefinition;

fn names<K: AsRef<str>>(map: &IndexMap<K, impl Sized>) -> Vec<String> {
    map.keys().map(|key| key.as_ref().to_owned()).collect()
}

fn attribute_names(map: &IndexMap<String, TelemetryRequiredAttributeDefinition>) -> Vec<String> {
    map.keys().cloned().collect()
}

fn string_values(attribute: &TelemetryRequiredAttributeDefinition) -> Option<Vec<String>> {
    match &attribute.definition {
        TelemetryAttributeDefinition::String { values, .. } => values.clone(),
        other => panic!("expected a string attribute, got {other:?}"),
    }
}

fn number_cardinality(attribute: &TelemetryRequiredAttributeDefinition) -> Option<Cardinality> {
    match &attribute.definition {
        TelemetryAttributeDefinition::Number { meta, .. } => meta.cardinality,
        other => panic!("expected a number attribute, got {other:?}"),
    }
}

fn description(definition: &TelemetryAttributeDefinition) -> &str {
    match definition {
        TelemetryAttributeDefinition::String { meta, .. }
        | TelemetryAttributeDefinition::Number { meta, .. }
        | TelemetryAttributeDefinition::Boolean { meta, .. }
        | TelemetryAttributeDefinition::StringArray { meta, .. }
        | TelemetryAttributeDefinition::NumberArray { meta, .. }
        | TelemetryAttributeDefinition::BooleanArray { meta, .. } => &meta.description,
    }
}

#[test]
fn submit_span_admission_attributes_and_result_values() {
    let schema = &*DURABLE_TELEMETRY_SCHEMA;
    let submit = &schema.spans["kiana.durable.submit"];
    assert_eq!(submit.parents, Some(TelemetryParentDefinition::Any));
    assert_eq!(
        attribute_names(&submit.start_attributes),
        vec!["submission.type".to_owned(), "conversation.id".to_owned()]
    );
    assert_eq!(
        string_values(&submit.start_attributes["submission.type"]).unwrap(),
        vec!["input".to_owned(), "write".to_owned()]
    );
    assert!(submit.start_attributes["submission.type"].required);
    assert_eq!(
        number_cardinality(&submit.start_attributes["conversation.id"]),
        Some(Cardinality::High)
    );
    assert_eq!(
        names(&submit.end_attributes),
        vec!["submission.id".to_owned(), "result".to_owned()]
    );
    let result = &submit.end_attributes["result"];
    let TelemetryAttributeDefinition::String { values, meta, .. } = result else {
        panic!("expected a string result attribute");
    };
    assert_eq!(
        values.as_ref().unwrap(),
        &vec!["queued".to_owned(), "placed".to_owned(), "done".to_owned()]
    );
    assert_eq!(
        meta.description,
        "Admission result; stale writes report done."
    );
    assert_eq!(submit.status.default, "ok");
    assert_eq!(submit.status.error_when, "Admission failed.");
    assert_eq!(
        names(submit.events.as_ref().unwrap()),
        vec![
            "kiana.durable.entry".to_owned(),
            "kiana.durable.submission".to_owned(),
            "kiana.durable.report".to_owned()
        ]
    );
}

#[test]
fn phase_span_covers_task_invocations_and_their_checkpoints() {
    let schema = &*DURABLE_TELEMETRY_SCHEMA;
    let phase = &schema.spans["kiana.durable.phase"];
    assert_eq!(phase.parents, Some(TelemetryParentDefinition::Any));
    assert_eq!(
        attribute_names(&phase.start_attributes),
        vec![
            "task.id".to_owned(),
            "task.name".to_owned(),
            "phase".to_owned(),
            "kind".to_owned(),
            "attempt".to_owned(),
            "resumed".to_owned()
        ]
    );
    assert_eq!(
        number_cardinality(&phase.start_attributes["task.id"]),
        Some(Cardinality::High)
    );
    assert_eq!(
        string_values(&phase.start_attributes["kind"]).unwrap(),
        vec!["run".to_owned(), "abort".to_owned()]
    );
    assert!(!phase.start_attributes["attempt"].required);
    assert!(phase.start_attributes["resumed"].required);
    assert!(matches!(
        phase.start_attributes["resumed"].definition,
        TelemetryAttributeDefinition::Boolean { .. }
    ));

    assert_eq!(
        names(&phase.end_attributes),
        vec![
            "outcome".to_owned(),
            "tool.name".to_owned(),
            "tool.call_id".to_owned()
        ]
    );
    let TelemetryAttributeDefinition::String { meta, .. } = &phase.end_attributes["tool.call_id"]
    else {
        panic!("expected a string attribute");
    };
    assert_eq!(meta.cardinality, Some(Cardinality::High));

    let events = phase.events.as_ref().unwrap();
    assert_eq!(
        attribute_names(&events["kiana.durable.checkpoint"].attributes),
        vec!["phase".to_owned()]
    );
    assert_eq!(
        attribute_names(&events["kiana.durable.replay"].attributes),
        vec!["tool.name".to_owned()]
    );
    assert_eq!(phase.status.default, "ok");
    for reference in ["task_faulted", "task_failed", "task_orphaned"] {
        assert!(
            phase.status.error_when.contains(reference),
            "status error_when names {reference}"
        );
    }
    assert!(phase.status.error_when.contains("500 characters"));
    assert!(
        phase
            .status
            .error_when
            .contains("Aborted outcomes keep status ok.")
    );
}

#[test]
fn entry_and_submission_events_point_at_records_not_content() {
    let schema = &*DURABLE_TELEMETRY_SCHEMA;
    for span in schema.spans.values() {
        let events = span.events.as_ref().expect("events");
        let entry = &events["kiana.durable.entry"];
        assert_eq!(
            attribute_names(&entry.attributes),
            vec!["entry.id".to_owned(), "entry.kind".to_owned()]
        );
        assert_eq!(
            number_cardinality(&entry.attributes["entry.id"]),
            Some(Cardinality::High)
        );
        assert!(entry.attributes["entry.id"].required);
        assert!(entry.attributes["entry.kind"].required);
        assert!(matches!(
            entry.attributes["entry.kind"].definition,
            TelemetryAttributeDefinition::String { values: None, .. }
        ));

        let submission = &events["kiana.durable.submission"];
        assert_eq!(
            attribute_names(&submission.attributes),
            vec![
                "submission.id".to_owned(),
                "status".to_owned(),
                "answer.entry_id".to_owned()
            ]
        );
        assert_eq!(
            number_cardinality(&submission.attributes["submission.id"]),
            Some(Cardinality::High)
        );
        assert_eq!(
            string_values(&submission.attributes["status"]).unwrap(),
            vec!["done".to_owned(), "unanswered".to_owned()]
        );
        assert!(!submission.attributes["answer.entry_id"].required);
        assert_eq!(
            number_cardinality(&submission.attributes["answer.entry_id"]),
            Some(Cardinality::High)
        );
    }
}

#[test]
fn report_events_share_the_fixed_report_vocabulary() {
    let schema = &*DURABLE_TELEMETRY_SCHEMA;
    for span in schema.spans.values() {
        let report = &span.events.as_ref().unwrap()["kiana.durable.report"];
        assert_eq!(
            attribute_names(&report.attributes),
            vec![
                "type".to_owned(),
                "reason".to_owned(),
                "extension".to_owned(),
                "task.id".to_owned()
            ]
        );
        assert_eq!(
            string_values(&report.attributes["type"]).unwrap(),
            vec![
                "session_failed".to_owned(),
                "task_faulted".to_owned(),
                "task_parked".to_owned(),
                "migration_failed".to_owned(),
                "task_outdated".to_owned(),
                "task_error".to_owned(),
                "extension_error".to_owned(),
                "host_error".to_owned(),
                "listener_error".to_owned()
            ]
        );
        assert_eq!(
            string_values(&report.attributes["reason"]).unwrap(),
            vec![
                "threw".to_owned(),
                "restart_limit".to_owned(),
                "missing_task".to_owned(),
                "incompatible_task".to_owned()
            ]
        );
        assert!(report.attributes["type"].required);
        assert!(!report.attributes["reason"].required);
        assert!(!report.attributes["extension"].required);
        assert!(!report.attributes["task.id"].required);
    }
}

#[test]
fn metrics_cover_reports_phase_duration_and_active_tasks() {
    let schema = &*DURABLE_TELEMETRY_SCHEMA;
    let metrics = schema.metrics.as_ref().expect("metrics");
    assert_eq!(
        names(metrics),
        vec![
            "kiana.durable.reports".to_owned(),
            "kiana.durable.phase.duration".to_owned(),
            "kiana.durable.tasks.active".to_owned()
        ]
    );
    let TelemetryMetricDefinition::Counter {
        description,
        unit,
        attributes,
    } = &metrics["kiana.durable.reports"]
    else {
        panic!("expected a counter");
    };
    assert_eq!(unit, &None);
    assert!(description.contains("ReportInfo type"));
    assert_eq!(string_values(&attributes["type"]).unwrap().len(), 9);
    assert!(attributes["type"].required);

    let TelemetryMetricDefinition::Histogram {
        unit,
        attributes,
        boundaries,
        ..
    } = &metrics["kiana.durable.phase.duration"]
    else {
        panic!("expected a histogram");
    };
    assert_eq!(unit.as_deref(), Some("ms"));
    assert!(boundaries.is_none());
    assert_eq!(
        attribute_names(attributes),
        vec!["task".to_owned(), "phase".to_owned(), "outcome".to_owned()]
    );

    let TelemetryMetricDefinition::UpDownCounter { attributes, .. } =
        &metrics["kiana.durable.tasks.active"]
    else {
        panic!("expected an up-down counter");
    };
    assert_eq!(attribute_names(attributes), vec!["task".to_owned()]);
}

#[test]
fn no_attribute_names_or_descriptions_reference_recorded_content() {
    // TS 测试断言导出的 span 里不出现输入、回答、工具参数或输出。词表层面的等价检查：没有属性
    // 承载内容，只有对记录的引用。
    let schema = &*DURABLE_TELEMETRY_SCHEMA;
    let mut every_name: Vec<String> = Vec::new();
    let mut every_description: Vec<String> = Vec::new();
    for span in schema.spans.values() {
        every_description.push(span.description.clone());
        for (name, attribute) in &span.start_attributes {
            every_name.push(name.clone());
            every_description.push(description(&attribute.definition).to_owned());
        }
        for definition in span.end_attributes.values() {
            every_description.push(description(definition).to_owned());
        }
        for event in span.events.iter().flat_map(|events| events.values()) {
            every_description.push(event.description.clone());
            for (name, attribute) in &event.attributes {
                every_name.push(name.clone());
                every_description.push(description(&attribute.definition).to_owned());
            }
        }
    }
    for metric in schema.metrics.iter().flat_map(|metrics| metrics.values()) {
        match metric {
            TelemetryMetricDefinition::Counter {
                description: metric_description,
                attributes,
                ..
            }
            | TelemetryMetricDefinition::UpDownCounter {
                description: metric_description,
                attributes,
                ..
            }
            | TelemetryMetricDefinition::Histogram {
                description: metric_description,
                attributes,
                ..
            } => {
                every_description.push(metric_description.clone());
                for (name, attribute) in attributes {
                    every_name.push(name.clone());
                    every_description.push(description(&attribute.definition).to_owned());
                }
            }
        }
    }
    for name in &every_name {
        for banned in [
            "content",
            "message",
            "argument",
            "prompt",
            "output",
            "answer.text",
        ] {
            assert!(
                !name.contains(banned),
                "attribute {name} looks like recorded content"
            );
        }
    }
    for description in &every_description {
        for banned in ["private-", "gen_ai"] {
            assert!(
                !description.contains(banned),
                "description mentions {banned}"
            );
        }
    }
    // ID 属性都指向 durable 记录。
    for name in &every_name {
        if name.ends_with(".id") || name == "submission.id" || name == "entry.id" {
            assert!(!name.contains("answer.text"));
        }
    }
}
