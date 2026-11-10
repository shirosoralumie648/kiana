//! `build.rs` 的单元测试（review round 1 的 finding 16；round 2 的 finding 7/8 扩展）。
//!
//! `build.rs` 是编译期门禁，`cargo test` 不会运行它；这里用 `#[path]` 把同一个源文件当作
//! 测试模块引入，直接对 `Date.parse` 的等价实现与模型字段校验做断言
//! （输入的接受/拒绝结果由 Node 26 实测确定）。

#[allow(dead_code)]
#[path = "../build.rs"]
mod build_script;

#[cfg(test)]
mod tests {
    use super::build_script::{parse_iso8601_ms, throw_validation_errors, validate_model_entry};
    use serde_json::{Map, Value, json};

    /// `Date.parse` 的接受/拒绝表（Node 26 实测；`None` = `NaN`）。
    ///
    /// 这里只列 UTC / 带偏移 / 非法形状；没有偏移的输入按本地时区解释，单独测（与机器时区无关）。
    #[test]
    fn parse_iso8601_ms_matches_date_parse() {
        let cases: &[(&str, Option<i64>)] = &[
            // 完整 ISO、小数秒（任意长度，截断到毫秒）。
            ("2026-10-26T11:07:41.777Z", Some(1_793_012_861_777)),
            ("2026-10-26T11:07:00Z", Some(1_793_012_820_000)),
            ("2026-10-26T11:07:00.7Z", Some(1_793_012_820_700)),
            ("2026-10-26T11:07:00.1234Z", Some(1_793_012_820_123)),
            ("2026-10-26T11:07:41.123456789012Z", Some(1_793_012_861_123)),
            // 只有日期（年/月/日）是 UTC 零点；`Z` 可省略也可保留。
            ("2026-10-26", Some(1_792_972_800_000)),
            ("2026-10", Some(1_790_812_800_000)),
            ("2026", Some(1_767_225_600_000)),
            ("2026-10-26Z", Some(1_792_972_800_000)),
            // 偏移：`±HH:mm`、`±HHmm`；大小写 `t`/`z`；空格代替 `T`；秒可省略。
            ("2026-10-26T11:07:00+00:00", Some(1_793_012_820_000)),
            ("2026-10-26T11:07:41+0200", Some(1_793_005_661_000)),
            ("2026-10-26T11:07:41+02:00", Some(1_793_005_661_000)),
            ("2026-10-26T11:07:41-0230", Some(1_793_021_861_000)),
            ("2026-10-26T11:07:41.777+02:00", Some(1_793_005_661_777)),
            ("2026-10-26t11:07:41z", Some(1_793_012_861_000)),
            ("2026-10-26 11:07:41Z", Some(1_793_012_861_000)),
            ("2026-10-26T11:07Z", Some(1_793_012_820_000)),
            // 扩展年份：`±YYYYYY`。
            ("+002026-10-26T11:07:41Z", Some(1_793_012_861_000)),
            ("-000001-01-01T00:00:00Z", Some(-62_198_755_200_000)),
            ("+275760-09-13T00:00:00Z", Some(8_640_000_000_000_000)),
            // 24 点：必须零分/秒/毫秒，等价次日零点。
            ("2026-10-26T24:00:00.000Z", Some(1_793_059_200_000)),
            ("2026-10-26T24:00:00Z", Some(1_793_059_200_000)),
            ("2026-10-26T24:00:01Z", None),
            ("2026-10-26T24:00:00.500Z", None),
            ("2026-10-26T25:00:00Z", None),
            ("2026-10-26T11:07:60Z", None),
            ("2026-10-26T11:60:00Z", None),
            // 月 1–12、日 1–31；月内溢出顺延（`04-31` → 5 月 1 日）。
            ("2026-13-01T00:00:00Z", None),
            ("2026-00-01T00:00:00Z", None),
            ("2026-10-32T00:00:00Z", None),
            ("2026-10-00T00:00:00Z", None),
            ("2026-04-31T00:00:00Z", Some(1_777_593_600_000)),
            ("2026-02-29T00:00:00Z", Some(1_772_323_200_000)),
            // 形状错误：空小数、只有小时、多余字符、非 ISO 的 legacy 形式。
            ("2026-10-26T11:07:00.Z", None),
            ("2026-10-26T11", None),
            ("2026-10-26T11Z", None),
            ("not a timestamp", None),
            ("", None),
            ("2026-10-26X", None),
            ("2026-1-2", None),
            ("Oct 26 2026", None),
            // 偏移校验：±HH 不完整、小时 24、分钟 60、多余秒。
            ("2026-10-26T11:07:41+02", None),
            ("2026-10-26T11:07:41+24:00", None),
            ("2026-10-26T11:07:41+00:60", None),
            ("2026-10-26T11:07:41+02:30:30", None),
            // `TimeClip` 的 ±8.64e15 ms 边界（含端点）。
            ("+275760-09-13T00:00:00.001Z", None),
            ("-271821-04-20T00:00:00Z", Some(-8_640_000_000_000_000)),
            ("-271821-04-19T23:59:59.999Z", None),
        ];
        for (input, expected) in cases {
            assert_eq!(parse_iso8601_ms(input), *expected, "Date.parse({input:?})");
        }
    }

    /// 没有偏移的日期时间按**本地时区**解释（`Date.parse` 的语义）；期望值用同一套时区换算得出，
    /// 因此测试与运行机器的时区无关。
    #[test]
    fn parse_iso8601_ms_uses_the_local_timezone_without_an_offset() {
        use chrono::{Local, TimeZone};

        fn local_ms(
            year: i32,
            month: u32,
            day: u32,
            hour: u32,
            minute: u32,
            second: u32,
            millis: u32,
        ) -> i64 {
            let naive = chrono::NaiveDate::from_ymd_opt(year, month, day)
                .expect("valid date")
                .and_hms_milli_opt(hour, minute, second, millis)
                .expect("valid time");
            Local
                .from_local_datetime(&naive)
                .earliest()
                .expect("local time exists")
                .timestamp_millis()
        }

        assert_eq!(
            parse_iso8601_ms("2026-10-26T11:07:41"),
            Some(local_ms(2026, 10, 26, 11, 7, 41, 0))
        );
        assert_eq!(
            parse_iso8601_ms("2026-10-26 11:07:41"),
            Some(local_ms(2026, 10, 26, 11, 7, 41, 0))
        );
        assert_eq!(
            parse_iso8601_ms("2026-10-26T11:07:41.777"),
            Some(local_ms(2026, 10, 26, 11, 7, 41, 777))
        );
        // `2026T11:07` / `2026-10T11:07`：V8 接受缺省的月/日。
        assert_eq!(
            parse_iso8601_ms("2026T11:07"),
            Some(local_ms(2026, 1, 1, 11, 7, 0, 0))
        );
        assert_eq!(
            parse_iso8601_ms("2026-10T11:07"),
            Some(local_ms(2026, 10, 1, 11, 7, 0, 0))
        );
        // 本地时间里的 24:00 同样等价次日零点。
        assert_eq!(
            parse_iso8601_ms("2026-10-26T24:00"),
            Some(local_ms(2026, 10, 27, 0, 0, 0, 0))
        );
        // 带偏移/`Z` 的输入不是本地时间（与本地时区无关的确定值）。
        assert_eq!(
            parse_iso8601_ms("2026-10-26T11:07:41Z"),
            Some(1_793_012_861_000)
        );
    }

    fn model() -> Value {
        json!({
            "type": "chat",
            "id": "gpt-5",
            "provider": "openai",
            "api": "openai-responses",
            "name": "GPT-5",
            "baseUrl": "https://api.openai.com/v1",
            "reasoning": true,
            "input": ["text", "image"],
            "cost": {
                "input": 1.25,
                "output": 10,
                "cacheRead": 0.125,
                "cacheWrite": 0,
            },
            "contextWindow": 400_000,
            "maxTokens": 128_000,
        })
    }

    fn validate(value: &Value, model_key: &str) -> Vec<String> {
        let mut errors = Vec::new();
        validate_model_entry(value, "openai", model_key, "openai-responses", &mut errors);
        errors
    }

    fn mutated(model: &Value, key: &str, value: Value) -> Value {
        let Value::Object(mut object) = model.clone() else {
            unreachable!("model is an object");
        };
        match value {
            Value::Null => {
                object.remove(key);
            }
            value => {
                object.insert(key.to_string(), value);
            }
        }
        Value::Object(object)
    }

    fn object(model: &Value) -> &Map<String, Value> {
        let Value::Object(object) = model else {
            unreachable!("model is an object");
        };
        object
    }

    #[test]
    fn validate_model_value_accepts_a_consistent_entry() {
        assert!(validate(&model(), "chat:gpt-5").is_empty());
        // classifier 只要求 contextWindow（没有 reasoning/maxTokens）。
        let mut classifier = model();
        {
            let Value::Object(object) = &mut classifier else {
                unreachable!()
            };
            object.insert("type".into(), json!("classifier"));
            object.remove("reasoning");
            object.remove("maxTokens");
        }
        assert!(validate(&classifier, "classifier:gpt-5").is_empty());
        // image 要求 output 含 "image"，chat 的字段不再检查。
        let mut image = model();
        {
            let Value::Object(object) = &mut image else {
                unreachable!()
            };
            object.insert("type".into(), json!("image"));
            object.insert("output".into(), json!(["image"]));
            object.remove("reasoning");
            object.remove("maxTokens");
        }
        assert!(validate(&image, "image:gpt-5").is_empty());
    }

    #[test]
    fn validate_model_value_reports_every_field_error_like_ts() {
        let cases: Vec<(Value, &str, Vec<&str>)> = vec![
            // 键里的 id 部分（`type:id` 的冒号之后）与 value.id 比较。
            (
                mutated(&model(), "id", json!("gpt-4")),
                "chat:gpt-5",
                vec![
                    r#"openai/gpt-5 has id "gpt-4", expected "gpt-5""#,
                    "openai/chat:gpt-5 has mismatched type/id identity",
                ],
            ),
            (
                mutated(&model(), "provider", json!("anthropic")),
                "chat:gpt-5",
                vec![r#"openai/gpt-5 has provider "anthropic", expected "openai""#],
            ),
            (
                mutated(&model(), "provider", Value::Null),
                "chat:gpt-5",
                vec!["openai/gpt-5 has provider undefined, expected \"openai\""],
            ),
            (
                mutated(&model(), "api", json!("anthropic-messages")),
                "chat:gpt-5",
                vec![r#"openai/gpt-5 has api "anthropic-messages", expected "openai-responses""#],
            ),
            (
                mutated(&model(), "name", json!("")),
                "chat:gpt-5",
                vec!["openai/gpt-5 has no model name"],
            ),
            (
                mutated(&model(), "name", Value::Null),
                "chat:gpt-5",
                vec!["openai/gpt-5 has no model name"],
            ),
            (
                mutated(&model(), "baseUrl", Value::Null),
                "chat:gpt-5",
                vec!["openai/gpt-5 has no baseUrl string"],
            ),
            (
                mutated(&model(), "input", json!([])),
                "chat:gpt-5",
                vec!["openai/gpt-5 has invalid input modalities"],
            ),
            (
                mutated(&model(), "input", json!(["text", "audio"])),
                "chat:gpt-5",
                vec!["openai/gpt-5 has invalid input modalities"],
            ),
            (
                mutated(&model(), "input", json!("text")),
                "chat:gpt-5",
                vec!["openai/gpt-5 has invalid input modalities"],
            ),
            (
                mutated(&model(), "output", json!(["text"])),
                "chat:gpt-5",
                vec!["openai/gpt-5 has unsupported output modalities"],
            ),
            (
                mutated(&model(), "reasoning", json!("yes")),
                "chat:gpt-5",
                vec!["openai/gpt-5 has no reasoning boolean"],
            ),
            (
                mutated(&model(), "contextWindow", json!(0)),
                "chat:gpt-5",
                vec!["openai/gpt-5 has invalid contextWindow"],
            ),
            (
                mutated(&model(), "contextWindow", json!("1000")),
                "chat:gpt-5",
                vec!["openai/gpt-5 has invalid contextWindow"],
            ),
            (
                mutated(&model(), "maxTokens", json!(-1)),
                "chat:gpt-5",
                vec!["openai/gpt-5 has invalid maxTokens"],
            ),
            (
                mutated(&model(), "cost", Value::Null),
                "chat:gpt-5",
                vec!["openai/gpt-5 has invalid cost metadata"],
            ),
            (
                mutated(
                    &model(),
                    "cost",
                    json!({"input": "1", "output": 2, "cacheRead": 0, "cacheWrite": 0}),
                ),
                "chat:gpt-5",
                vec!["openai/gpt-5 has invalid cost.input"],
            ),
            // type 缺失/未知：报 type 错误（不跑 chat 的字段检查），身份也不匹配。
            (
                mutated(&model(), "type", Value::Null),
                "chat:gpt-5",
                vec![
                    r#"openai/gpt-5 has type undefined, expected "chat", "image", or "classifier""#,
                    "openai/chat:gpt-5 has mismatched type/id identity",
                ],
            ),
            (
                mutated(&model(), "type", json!("embedding")),
                "chat:gpt-5",
                vec![
                    r#"openai/gpt-5 has type "embedding", expected "chat", "image", or "classifier""#,
                    "openai/chat:gpt-5 has mismatched type/id identity",
                ],
            ),
            // image 缺少 output / output 不含 image；键用 image: 前缀，身份不报错。
            (
                {
                    let mut image = model();
                    let Value::Object(object) = &mut image else {
                        unreachable!()
                    };
                    object.insert("type".into(), json!("image"));
                    image
                },
                "image:gpt-5",
                vec!["openai/gpt-5 has invalid output modalities"],
            ),
            (
                {
                    let mut image = model();
                    let Value::Object(object) = &mut image else {
                        unreachable!()
                    };
                    object.insert("type".into(), json!("image"));
                    object.insert("output".into(), json!(["text"]));
                    image
                },
                "image:gpt-5",
                vec!["openai/gpt-5 has invalid output modalities"],
            ),
            // 非对象模型只报 "must be an object"（TS 的 isRecord 分支）。
            (
                json!("not a model"),
                "chat:gpt-5",
                vec!["openai/gpt-5 must be an object"],
            ),
        ];
        for (value, model_key, expected) in cases {
            assert_eq!(validate(&value, model_key), expected, "{model_key}");
        }
        // 合法模型没有误报（字段集合与检查结果无关）。
        assert_eq!(object(&model()).len(), 11);
    }

    fn panic_message(payload: Box<dyn std::any::Any + Send>) -> String {
        payload
            .downcast_ref::<String>()
            .cloned()
            .or_else(|| payload.downcast_ref::<&str>().map(|s| s.to_string()))
            .unwrap_or_default()
    }

    #[test]
    fn throw_validation_errors_matches_the_ts_aggregate() {
        let errors: Vec<String> = (0..32).map(|index| format!("error {index}")).collect();
        let payload =
            std::panic::catch_unwind(|| throw_validation_errors(&errors)).expect_err("panics");
        let message = panic_message(payload);
        assert!(message.starts_with("Invalid generated model data:\n  - error 0\n  - error 1\n"));
        assert!(message.contains("  - error 29"));
        assert!(!message.contains("error 30"));
        assert!(message.ends_with("\n  ... and 2 more"));

        let payload =
            std::panic::catch_unwind(|| throw_validation_errors(&errors[..2])).expect_err("panics");
        assert_eq!(
            panic_message(payload),
            "Invalid generated model data:\n  - error 0\n  - error 1"
        );
    }
}
