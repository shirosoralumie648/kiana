//! schema JSON 黄金向量：TS 的 `AI_TELEMETRY_SCHEMA` / `DURABLE_TELEMETRY_SCHEMA`。
//!
//! 冻结的 schema 结构体（`index.rs` / `metrics.rs`，ARCHITECTURE.md §4.9）用
//! `#[serde(flatten)]` + `#[serde(tag)]` 建模，字段顺序固定为结构体分组的顺序。TS 的键序则来自
//! 每个属性定义的**源码字面量**，同一个 schema 内并不统一：
//!
//! ```text
//! // packages/ai/src/telemetry.ts（description 在 required 之前）
//! { "type": "string", "description": "Provider identifier.", "required": true }
//! // packages/durable/src/harness/telemetry.ts（required 在 description 之前）
//! { "type": "string", "required": true, "values": ["input","write"], "description": "Submission type." }
//! ```
//!
//! 因此任何固定的结构体字段序最多只能逐字节匹配其中一个 schema（当前 AI schema 恰好整体一致，
//! DURABLE 在 `startAttributes` 处开始不同），不存在同时匹配两者的实现。TS 运行时也从不
//! `JSON.stringify` schema（`defineTelemetrySchema` 是恒等函数，唯一消费点是类型推断），
//! 所以这里断言的是**语义**契约：
//!
//! 1. TS 文本能被 Rust 结构体**无损反序列化**（未知/缺省字段不丢信息）；
//! 2. 再用 `kiana_json` 序列化出的 JSON 与 TS 文本**语义深相等**（`kiana_json::deep_equal`，忽略
//!    整数键的插入序差异）；
//! 3. 再次往返稳定（`stringify → parse → stringify` 幂等）；
//! 4. 数字按 JS 规则写出（`version: 1` 是 `1` 而不是 `1.0`）。
//!
//! 偏差记录：schema JSON 的键序不保证逐字节稳定（仅值集合相等）；AI schema 目前逐字节稳定，
//! 该事实在 [`ai_telemetry_schema_matches_ts_bytes`] 里固定下来，改动 key 序会立刻暴露。

use kiana_telemetry::TelemetrySchemaDefinition;
use serde_json::Value;

fn fixtures() -> Value {
    serde_json::from_str(include_str!("fixtures/schema_json.json")).expect("schema_json.json")
}

fn roundtrip(name: &str) {
    let fixtures = fixtures();
    let text = fixtures
        .get(name)
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("fixture {name}"));
    let original: Value = kiana_json::parse(text).expect("TS schema is valid JSON");
    let definition: TelemetrySchemaDefinition =
        serde_json::from_str(text).unwrap_or_else(|error| panic!("{name} deserializes: {error}"));
    let value = serde_json::to_value(&definition).expect("schema serializes");
    let rewritten = kiana_json::stringify(&value);
    let reparsed: Value = kiana_json::parse(&rewritten).expect("rewritten schema is valid JSON");

    assert!(
        kiana_json::deep_equal(&original, &reparsed),
        "{name} schema round-trips semantically"
    );

    // 幂等：再次往返不改变文本。
    let definition2: TelemetrySchemaDefinition =
        serde_json::from_str(&rewritten).expect("rewritten schema deserializes");
    let value2 = serde_json::to_value(&definition2).expect("reserializes");
    assert_eq!(
        kiana_json::stringify(&value2),
        rewritten,
        "{name} round-trip is stable"
    );

    // JS 数字格式：version 是 `1`，不是 `1.0`。
    let version_marker = rewritten
        .split("\"version\":")
        .nth(1)
        .expect("version field");
    assert!(
        version_marker.starts_with('1') && !version_marker.starts_with("1.0"),
        "{name} version uses JS number formatting: {version_marker:.20}"
    );
    // TS 的 `JSON.stringify` 只输出 `:` 与 `,`，不带缩进/换行（描述文本里可能有 ": "）。
    assert!(!rewritten.contains('\n'), "{name} is single-line");
    let outside_strings = outside_string_text(&rewritten);
    assert!(
        !outside_strings.contains(": "),
        "{name} has no pretty-print spacing"
    );
}

/// 去掉 JSON 字符串字面量后的文本（用于检查语法层是否被 pretty-print）。
fn outside_string_text(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut in_string = false;
    let mut escaped = false;
    for ch in text.chars() {
        if in_string {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                in_string = false;
            }
            continue;
        }
        if ch == '"' {
            in_string = true;
            continue;
        }
        out.push(ch);
    }
    out
}

#[test]
fn ai_telemetry_schema_round_trips() {
    roundtrip("ai");
}

#[test]
fn durable_telemetry_schema_round_trips() {
    roundtrip("durable");
}

/// AI schema 的序列化输出恰好逐字节稳定（结构体字段序 == 该 fixture 的字面量序）。
///
/// 这不是普遍契约（见模块文档），但它是当前实现的既有事实；固定下来，任何字段重排都会在此暴露，
/// 从而提醒重新评估 DURABLE 的键序取舍。
#[test]
fn ai_telemetry_schema_matches_ts_bytes() {
    assert_eq!(reserialize("ai"), ts_text("ai"));
}

/// DURABLE schema 的差异**只是键序**（值集合、字节长度都相同）：TS 字面量把 `required` 写在
/// `description` 之前、把 `unit` 写在 `description` 之前，而冻结结构体的字段序来自结构体定义
/// （`attributes` 的 `required` 在 `#[serde(flatten)]` 之后、`TelemetryMetricDefinition::Histogram`
/// 的 `description` 在 `unit` 之前）。
///
/// 该文件是 `serializable data format`（不是运行时输出）：TS 的 `defineTelemetrySchema` 是恒等
/// 函数，仓库内没有任何代码对 schema 做 `JSON.stringify`，所以只按语义断言。
#[test]
fn durable_telemetry_schema_differs_from_ts_only_in_key_order() {
    let rust_text = reserialize("durable");
    let ts_text = ts_text("durable");
    assert_eq!(rust_text.len(), ts_text.len(), "same serialized length");
    assert_ne!(rust_text, ts_text);
    // 值完全相同（`deep_equal` 只忽略键序）：文本不同 + 长度相同 + 值相同 ⇒ 差异只是键序。
    let rust_value: Value = kiana_json::parse(&rust_text).expect("rust schema parses");
    let ts_value: Value = kiana_json::parse(&ts_text).expect("ts schema parses");
    assert!(kiana_json::deep_equal(&rust_value, &ts_value));
    // `required` 的数量与取值分布一致。
    for marker in ["\"required\":true", "\"required\":false"] {
        assert_eq!(
            rust_text.matches(marker).count(),
            ts_text.matches(marker).count(),
            "same number of {marker}"
        );
    }
}

fn ts_text(name: &str) -> String {
    fixtures()
        .get(name)
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("fixture {name}"))
        .to_string()
}

fn reserialize(name: &str) -> String {
    let definition: TelemetrySchemaDefinition =
        serde_json::from_str(&ts_text(name)).unwrap_or_else(|error| panic!("{name}: {error}"));
    let value = serde_json::to_value(&definition).expect("schema serializes");
    kiana_json::stringify(&value)
}

#[test]
fn schema_version_serializes_like_javascript() {
    let definition: TelemetrySchemaDefinition =
        serde_json::from_str(r#"{"version":1,"spans":{}}"#).expect("minimal schema");
    let value = serde_json::to_value(&definition).expect("serializes");
    assert_eq!(kiana_json::stringify(&value), r#"{"version":1,"spans":{}}"#);
}

/// 数组类型的属性定义使用 TS 的 `elementValues` 键（index.ts:141-143、README.md:482），
/// 不是 Rust 字段名的 snake_case。
///
/// 回归：`element_values` 曾缺少 `#[serde(rename = "elementValues")]`，导致 TS 生成的 schema
/// 被静默丢失该字段、再序列化时写成 `element_values`（两个方向都错）。
#[test]
fn array_attribute_definitions_use_element_values() {
    use kiana_telemetry::{TelemetryAttributeDefinition, TelemetryRequiredAttributeDefinition};

    let cases = [
        (
            r#"{"type":"string[]","description":"d","elementValues":["a"],"required":true}"#,
            r#"["a"]"#,
        ),
        (
            r#"{"type":"number[]","description":"d","elementValues":[1,2.5],"required":true}"#,
            r#"[1,2.5]"#,
        ),
        (
            r#"{"type":"boolean[]","description":"d","elementValues":[true],"required":true}"#,
            r#"[true]"#,
        ),
    ];
    for (text, expected_values) in cases {
        let definition: TelemetryRequiredAttributeDefinition =
            serde_json::from_str(text).unwrap_or_else(|error| panic!("{text}: {error}"));
        let value = serde_json::to_value(&definition).expect("serializes");
        let rewritten = kiana_json::stringify(&value);
        assert!(
            rewritten.contains(&format!("\"elementValues\":{expected_values}")),
            "{text} keeps elementValues: {rewritten}"
        );
        assert!(!rewritten.contains("element_values"), "{rewritten}");
        let original: Value = kiana_json::parse(text).expect("valid JSON");
        let reparsed: Value = kiana_json::parse(&rewritten).expect("valid JSON");
        assert!(
            kiana_json::deep_equal(&original, &reparsed),
            "{text} round-trips"
        );
    }

    // 标量定义继续使用 `values`，数组定义不把 `values` 当作别名。
    let definition: TelemetryRequiredAttributeDefinition = serde_json::from_str(
        r#"{"type":"string","description":"d","values":["a"],"required":false}"#,
    )
    .expect("scalar definition");
    let rewritten = kiana_json::stringify(&serde_json::to_value(&definition).expect("serializes"));
    assert_eq!(
        rewritten,
        r#"{"type":"string","description":"d","values":["a"],"required":false}"#
    );

    // schema 内嵌时同样保留 TS 键名（含 endAttributes 里不带 required 的定义）。
    let schema: TelemetrySchemaDefinition = serde_json::from_str(
        r#"{"version":1,"spans":{"s":{"description":"d","parents":{"kind":"any"},
            "startAttributes":{"a":{"type":"string[]","description":"d","elementValues":["x"],"required":true}},
            "endAttributes":{"b":{"type":"number[]","description":"e","elementValues":[3]}},
            "status":{"default":"ok","errorWhen":"x"}}}}"#,
    )
    .expect("schema with array attributes");
    let rewritten = kiana_json::stringify(&serde_json::to_value(&schema).expect("serializes"));
    assert!(
        rewritten.contains("\"elementValues\":[\"x\"]"),
        "{rewritten}"
    );
    assert!(rewritten.contains("\"elementValues\":[3]"), "{rewritten}");
    assert!(!rewritten.contains("element_values"), "{rewritten}");
    // 反序列化后字段确实被保留（不是静默丢弃）。
    let span = &schema.spans["s"];
    match &span.start_attributes["a"].definition {
        TelemetryAttributeDefinition::StringArray { element_values, .. } => assert_eq!(
            element_values.as_deref(),
            Some(["x".to_string()].as_slice())
        ),
        other => panic!("unexpected definition {other:?}"),
    }
}
