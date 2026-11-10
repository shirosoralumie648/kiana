//! cron 调度表达式：`duration` / `every` / 5-field cron / ISO one-shot。
//!
//! 设计参照 `reference_project/ClaudeCode/src/utils/cron.ts` 的 5 字段展开与
//! `computeNextCronRun`：wildcard、`*/n`、`a-b`、`a-b/n`、逗号列表；DOM 与 DOW 同时受限时
//! 采用 vixie-cron 的“任一匹配”语义；向前最多搜索 366 天。
//!
//! 与参考实现的差异：Kiana 的 cron 是 **UTC 或固定偏移**（[`FixedOffset`]），不支持 IANA
//! 时区名与 DST 规则（没有 tzdata 依赖）。`duration`/`every` 都是秒级间隔：
//! - `duration`：相对上一次计划槽位（`slot + interval`）；
//! - `every`：对齐到 epoch 的整倍数（`floor(now/interval)*interval + interval`）。
//!
//! # Spec 形状
//!
//! ```json
//! {"duration": "90m"}
//! {"every": "15m"}
//! {"cron": "0 9 * * MON-FRI"}        // 名称仅作为文档示例；当前只接受数字
//! {"at": "2026-11-08T09:00:00Z"}
//! "0 9 * * 1-5"                       // 字符串：先按 5-field cron，再按 ISO
//! "30m"                               // 字符串：duration
//! ```
//!
//! 字符串的判定顺序：5 个空白分隔字段且 cron 解析成功 → cron；否则 ISO 8601 →
//! one-shot；否则带单位的 duration → duration；都失败则报错。

use serde::{Deserialize, Serialize};
use thiserror::Error;

use super::time::{FixedOffset, TimeError, civil_from_days, parse_iso8601, weekday_from_days};

/// 调度表达式错误。
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ScheduleError {
    /// 字段组合不合法。
    #[error("invalid schedule: {0}")]
    Invalid(String),
    /// 5-field cron 解析失败。
    #[error("invalid cron expression {expression:?}: {message}")]
    Cron {
        /// 原始表达式。
        expression: String,
        /// 原因。
        message: String,
    },
    /// 时间戳解析失败。
    #[error("invalid timestamp: {0}")]
    Time(#[from] TimeError),
    /// 不支持时区。
    #[error("unsupported timezone {0:?} (use UTC, Local, or a ±HH:MM offset)")]
    Timezone(String),
    /// 间隔必须大于 0。
    #[error("interval must be greater than zero")]
    ZeroInterval,
}

/// 用户书写的调度 spec。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ScheduleSpec {
    /// 对象形式：`{"duration"|"every"|"cron"|"at": "..."}`。
    Object(ScheduleObject),
    /// 字符串形式（cron / ISO / duration）。
    Text(String),
}

/// 对象形式的调度 spec；四个字段恰好有一个。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScheduleObject {
    /// 固定间隔，相对上一槽位。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration: Option<String>,
    /// 墙钟对齐间隔。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub every: Option<String>,
    /// 5-field cron。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cron: Option<String>,
    /// ISO 8601 one-shot。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub at: Option<String>,
}

impl ScheduleSpec {
    /// duration 形式。
    pub fn duration(input: impl Into<String>) -> Self {
        Self::Object(ScheduleObject {
            duration: Some(input.into()),
            ..Default::default()
        })
    }

    /// every 形式。
    pub fn every(input: impl Into<String>) -> Self {
        Self::Object(ScheduleObject {
            every: Some(input.into()),
            ..Default::default()
        })
    }

    /// cron 形式。
    pub fn cron(input: impl Into<String>) -> Self {
        Self::Object(ScheduleObject {
            cron: Some(input.into()),
            ..Default::default()
        })
    }

    /// one-shot 形式。
    pub fn at(input: impl Into<String>) -> Self {
        Self::Object(ScheduleObject {
            at: Some(input.into()),
            ..Default::default()
        })
    }
}

/// 编译后的调度。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CompiledSchedule {
    /// 相对间隔。
    Duration {
        /// 秒。
        seconds: u64,
    },
    /// 墙钟对齐间隔。
    Every {
        /// 秒。
        seconds: u64,
    },
    /// 5-field cron + 时区。
    Cron {
        /// 表达式。
        expression: CronExpression,
        /// 时区。
        timezone: FixedOffset,
    },
    /// 一次性时间点。
    OneShot {
        /// epoch 秒。
        at: i64,
    },
}

impl CompiledSchedule {
    /// 从 spec 编译；`timezone` 是 job 级时区（仅 cron 使用）。
    pub fn from_spec(spec: &ScheduleSpec, timezone: &str) -> Result<Self, ScheduleError> {
        match spec {
            ScheduleSpec::Text(text) => {
                let trimmed = text.trim();
                if trimmed.is_empty() {
                    return Err(ScheduleError::Invalid("schedule is empty".to_string()));
                }
                if trimmed.split_whitespace().count() == 5
                    && let Ok(expression) = parse_cron(trimmed)
                {
                    return Ok(CompiledSchedule::Cron {
                        expression,
                        timezone: parse_timezone(timezone)?,
                    });
                }
                if let Ok(at) = parse_iso8601(trimmed) {
                    return Ok(CompiledSchedule::OneShot { at });
                }
                if let Ok(seconds) = parse_interval(trimmed) {
                    return Ok(CompiledSchedule::Duration { seconds });
                }
                Err(ScheduleError::Invalid(format!(
                    "schedule {trimmed:?} is neither a cron expression, an ISO timestamp, nor a duration"
                )))
            }
            ScheduleSpec::Object(object) => {
                let fields = [
                    object.duration.is_some(),
                    object.every.is_some(),
                    object.cron.is_some(),
                    object.at.is_some(),
                ]
                .into_iter()
                .filter(|present| *present)
                .count();
                if fields != 1 {
                    return Err(ScheduleError::Invalid(
                        "exactly one of duration/every/cron/at is required".to_string(),
                    ));
                }
                if let Some(value) = &object.duration {
                    return Ok(CompiledSchedule::Duration {
                        seconds: parse_interval(value)?,
                    });
                }
                if let Some(value) = &object.every {
                    return Ok(CompiledSchedule::Every {
                        seconds: parse_interval(value)?,
                    });
                }
                if let Some(value) = &object.cron {
                    return Ok(CompiledSchedule::Cron {
                        expression: parse_cron(value)?,
                        timezone: parse_timezone(timezone)?,
                    });
                }
                let at = object.at.as_deref().unwrap_or_default();
                Ok(CompiledSchedule::OneShot {
                    at: parse_iso8601(at)?,
                })
            }
        }
    }

    /// 首次调度时间；one-shot 即使已过期也返回原时间（由 tick 判定 missed）。
    pub fn initial_next(&self, now: i64) -> Option<i64> {
        match self {
            CompiledSchedule::Duration { seconds } => Some(now.saturating_add(*seconds as i64)),
            CompiledSchedule::Every { seconds } => {
                let step = *seconds as i64;
                Some(now.div_euclid(step).saturating_add(1).saturating_mul(step))
            }
            CompiledSchedule::Cron {
                expression,
                timezone,
            } => expression.next_after(now, *timezone),
            CompiledSchedule::OneShot { at } => Some(*at),
        }
    }

    /// 严格晚于 `slot` 的下一个计划时间；one-shot 返回 `None`。
    pub fn next_after(&self, slot: i64) -> Option<i64> {
        match self {
            CompiledSchedule::Duration { seconds } => Some(slot.saturating_add(*seconds as i64)),
            CompiledSchedule::Every { seconds } => {
                let step = *seconds as i64;
                Some(slot.div_euclid(step).saturating_add(1).saturating_mul(step))
            }
            CompiledSchedule::Cron {
                expression,
                timezone,
            } => expression.next_after(slot, *timezone),
            CompiledSchedule::OneShot { .. } => None,
        }
    }

    /// 半周期（秒），用于 catch-up clamp；cron 用相邻两次的间隔估算。
    pub fn half_period_secs(&self, now: i64) -> Option<u64> {
        match self {
            CompiledSchedule::Duration { seconds } | CompiledSchedule::Every { seconds } => {
                Some((*seconds / 2).max(1))
            }
            CompiledSchedule::Cron {
                expression,
                timezone,
            } => {
                let first = expression.next_after(now, *timezone)?;
                let second = expression.next_after(first, *timezone)?;
                Some(((second - first).unsigned_abs() / 2).max(1))
            }
            CompiledSchedule::OneShot { .. } => None,
        }
    }

    /// 是否一次性。
    pub fn is_one_shot(&self) -> bool {
        matches!(self, CompiledSchedule::OneShot { .. })
    }
}

fn parse_timezone(input: &str) -> Result<FixedOffset, ScheduleError> {
    FixedOffset::parse(input).ok_or_else(|| ScheduleError::Timezone(input.to_string()))
}

/// 间隔上限：必须能安全转换成 `i64`，且与 `now` 相加 / 对齐乘法不溢出。
pub const MAX_INTERVAL_SECS: u64 = i64::MAX as u64 / 2;

/// 校验解析出的秒数。
fn validate_interval(seconds: u64, input: &str) -> Result<u64, ScheduleError> {
    if seconds == 0 {
        return Err(ScheduleError::ZeroInterval);
    }
    if seconds > MAX_INTERVAL_SECS {
        // 超出 i64 的间隔会静默变成负数时间（例如 `as i64` 得到 i64::MIN），
        // 调度器会把任务当成永远过期。
        return Err(ScheduleError::Invalid(format!(
            "interval {input:?} is too large"
        )));
    }
    Ok(seconds)
}

/// 解析间隔：`30`（秒）、`45s`、`15m`、`2h`、`1d`，以及组合 `1h30m`。
pub fn parse_interval(input: &str) -> Result<u64, ScheduleError> {
    let value = input.trim().to_ascii_lowercase();
    if value.is_empty() {
        return Err(ScheduleError::Invalid("empty interval".to_string()));
    }
    if value.chars().all(|ch| ch.is_ascii_digit()) {
        let seconds = value
            .parse::<u64>()
            .map_err(|_| ScheduleError::Invalid(format!("invalid interval {input:?}")))?;
        return validate_interval(seconds, input);
    }
    let mut total = 0u64;
    let mut number = String::new();
    let mut saw_unit = false;
    for ch in value.chars() {
        if ch.is_ascii_digit() {
            number.push(ch);
            continue;
        }
        let multiplier = match ch {
            's' => 1u64,
            'm' => 60,
            'h' => 3_600,
            'd' => 86_400,
            _ => {
                return Err(ScheduleError::Invalid(format!(
                    "invalid interval unit {ch:?} in {input:?}"
                )));
            }
        };
        if number.is_empty() {
            return Err(ScheduleError::Invalid(format!(
                "missing number before {ch:?} in {input:?}"
            )));
        }
        let amount = number
            .parse::<u64>()
            .map_err(|_| ScheduleError::Invalid(format!("invalid number in {input:?}")))?;
        total = total.saturating_add(amount.saturating_mul(multiplier));
        number.clear();
        saw_unit = true;
    }
    if !number.is_empty() {
        return Err(ScheduleError::Invalid(format!(
            "missing unit after {number:?} in {input:?}"
        )));
    }
    if !saw_unit {
        return Err(ScheduleError::ZeroInterval);
    }
    validate_interval(total, input)
}

/// 解析后的 5-field cron。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CronExpression {
    /// 原始表达式。
    pub expression: String,
    minutes: Vec<u32>,
    hours: Vec<u32>,
    days_of_month: Vec<u32>,
    months: Vec<u32>,
    days_of_week: Vec<u32>,
    dom_restricted: bool,
    dow_restricted: bool,
}

impl CronExpression {
    /// 严格晚于 `from` 的下一个匹配时间（UTC 秒）；最多向前搜索 366 天。
    pub fn next_after(&self, from: i64, timezone: FixedOffset) -> Option<i64> {
        let offset = timezone.seconds();
        let local = from + offset;
        let mut candidate = local.div_euclid(60) * 60 + 60;
        let limit = candidate + 366 * 86_400;
        while candidate <= limit {
            let days = candidate.div_euclid(86_400);
            let seconds_of_day = candidate.rem_euclid(86_400);
            let hour = (seconds_of_day / 3_600) as u32;
            let minute = ((seconds_of_day % 3_600) / 60) as u32;
            if self.hours.binary_search(&hour).is_ok()
                && self.minutes.binary_search(&minute).is_ok()
            {
                let (_year, month, day) = civil_from_days(days);
                let weekday = weekday_from_days(days);
                let dom_match = self.days_of_month.binary_search(&day).is_ok();
                let dow_match = self.days_of_week.binary_search(&weekday).is_ok();
                let day_match = if self.dom_restricted && self.dow_restricted {
                    dom_match || dow_match
                } else if self.dom_restricted {
                    dom_match
                } else if self.dow_restricted {
                    dow_match
                } else {
                    true
                };
                if self.months.binary_search(&month).is_ok() && day_match {
                    return Some(candidate - offset);
                }
            }
            candidate += 60;
        }
        None
    }
}

/// 解析 5-field cron。
pub fn parse_cron(expression: &str) -> Result<CronExpression, ScheduleError> {
    let parts: Vec<&str> = expression.split_whitespace().collect();
    if parts.len() != 5 {
        return Err(ScheduleError::Cron {
            expression: expression.to_string(),
            message: format!("expected 5 fields, got {}", parts.len()),
        });
    }
    let minutes = expand_field(parts[0], 0, 59, false).map_err(|message| ScheduleError::Cron {
        expression: expression.to_string(),
        message: format!("minute: {message}"),
    })?;
    let hours = expand_field(parts[1], 0, 23, false).map_err(|message| ScheduleError::Cron {
        expression: expression.to_string(),
        message: format!("hour: {message}"),
    })?;
    let days_of_month =
        expand_field(parts[2], 1, 31, false).map_err(|message| ScheduleError::Cron {
            expression: expression.to_string(),
            message: format!("day-of-month: {message}"),
        })?;
    let months = expand_field(parts[3], 1, 12, false).map_err(|message| ScheduleError::Cron {
        expression: expression.to_string(),
        message: format!("month: {message}"),
    })?;
    let days_of_week =
        expand_field(parts[4], 0, 6, true).map_err(|message| ScheduleError::Cron {
            expression: expression.to_string(),
            message: format!("day-of-week: {message}"),
        })?;
    // 通配判定看展开后的基数，而不是字面 `*`：`*/1`、`1-31`、`0-6` 都覆盖整个
    // 字段，与 `*` 等价（参照实现用 `dayOfMonth.length === 31` / `dow.length === 7`）。
    // 否则 “DOM 或 DOW 任一满足” 的分支会在错误的日期触发。
    let dom_restricted = days_of_month.len() != 31;
    let dow_restricted = days_of_week.len() != 7;
    Ok(CronExpression {
        expression: expression.to_string(),
        minutes,
        hours,
        days_of_month,
        months,
        days_of_week,
        dom_restricted,
        dow_restricted,
    })
}

fn expand_field(field: &str, min: u32, max: u32, sunday_alias: bool) -> Result<Vec<u32>, String> {
    let mut values = Vec::new();
    let effective_max = if sunday_alias { 7 } else { max };
    for part in field.split(',') {
        let part = part.trim();
        if part.is_empty() {
            return Err("empty list element".to_string());
        }
        let (range, step) = match part.split_once('/') {
            Some((range, step)) => {
                // 参照实现只接受 `*/step` 与 `lo-hi/step`；`N/step` 是非法写法，
                // 不能把 step 静默丢掉当成单个 N（否则 `5/10` 会变成每小时第 5 分钟）。
                if range != "*" && !range.contains('-') {
                    return Err(format!(
                        "step requires `*` or a range before `/`, got {part:?}"
                    ));
                }
                (
                    range,
                    step.parse::<u32>()
                        .map_err(|_| format!("invalid step {step:?}"))?,
                )
            }
            None => (part, 1),
        };
        if step == 0 {
            return Err("step must be greater than zero".to_string());
        }
        let (low, high) = if range == "*" {
            (min, effective_max)
        } else if let Some((low, high)) = range.split_once('-') {
            (
                low.parse::<u32>()
                    .map_err(|_| format!("invalid range start {low:?}"))?,
                high.parse::<u32>()
                    .map_err(|_| format!("invalid range end {high:?}"))?,
            )
        } else {
            let value = range
                .parse::<u32>()
                .map_err(|_| format!("invalid value {range:?}"))?;
            (value, value)
        };
        if low < min || high > effective_max || low > high {
            return Err(format!(
                "range {low}-{high} is outside {min}-{effective_max}"
            ));
        }
        let mut value = low;
        while value <= high {
            let normalized = if sunday_alias && value == 7 { 0 } else { value };
            values.push(normalized);
            // 用户可提供任意 u32 step；饱和加法避免 debug/test 构建 panic，
            // 也会在超出 high 后自然结束循环。
            value = value.saturating_add(step);
        }
    }
    values.sort_unstable();
    values.dedup();
    if values.is_empty() {
        return Err("field expands to no values".to_string());
    }
    Ok(values)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(text: &str) -> i64 {
        parse_iso8601(text).expect("timestamp")
    }

    #[test]
    fn parses_intervals_with_units_and_combinations() {
        assert_eq!(parse_interval("30").expect("seconds"), 30);
        assert_eq!(parse_interval("45s").expect("seconds"), 45);
        assert_eq!(parse_interval("15m").expect("minutes"), 900);
        assert_eq!(parse_interval("2h").expect("hours"), 7_200);
        assert_eq!(parse_interval("1d").expect("days"), 86_400);
        assert_eq!(parse_interval("1h30m").expect("combined"), 5_400);
        assert!(parse_interval("0m").is_err());
        assert!(parse_interval("abc").is_err());
        // 超出 i64 的间隔必须被拒绝，不能静默回绕成负时间。
        assert!(parse_interval("9223372036854775808s").is_err());
        assert!(parse_interval("99999999999999999999").is_err());
        assert_eq!(
            parse_interval(&format!("{MAX_INTERVAL_SECS}s")).expect("max"),
            MAX_INTERVAL_SECS
        );
    }

    #[test]
    fn cron_next_run_respects_fields() {
        let expression = parse_cron("*/15 * * * *").expect("cron");
        let start = at("2026-11-07T10:07:30Z");
        let next = expression
            .next_after(start, FixedOffset::utc())
            .expect("next");
        assert_eq!(next, at("2026-11-07T10:15:00Z"));
    }

    #[test]
    fn cron_weekday_range_and_dom_or_semantics() {
        // 2026-11-07 是周六；下一个工作日 09:00 是周一 2026-11-09。
        let expression = parse_cron("0 9 * * 1-5").expect("cron");
        let next = expression
            .next_after(at("2026-11-07T10:00:00Z"), FixedOffset::utc())
            .expect("next");
        assert_eq!(next, at("2026-11-09T09:00:00Z"));

        // DOM=1 或 DOW=1（周一）：11 月 1 日是周日，两者都满足其一。
        let expression = parse_cron("0 0 1 * 1").expect("cron");
        let next = expression
            .next_after(at("2026-11-01T00:00:00Z"), FixedOffset::utc())
            .expect("next");
        assert_eq!(next, at("2026-11-02T00:00:00Z"), "Monday matches DOW");
    }

    #[test]
    fn cron_full_range_fields_count_as_wildcards() {
        // DOW `0-6` 覆盖整周，等价于 `*`：DOM=1 仍然只在该月 1 号触发，不能被
        // “DOM 或 DOW” 语义放大成每天触发（2026-11-01 已过，下一次是 12-01）。
        let expression = parse_cron("0 9 1 * 0-6").expect("cron");
        let next = expression
            .next_after(at("2026-11-07T10:00:00Z"), FixedOffset::utc())
            .expect("next");
        assert_eq!(next, at("2026-12-01T09:00:00Z"));

        // DOM `*/1` 展开成 1-31，同样是通配：只有 DOW=1-5 生效。
        let expression = parse_cron("0 9 */1 * 1-5").expect("cron");
        let next = expression
            .next_after(at("2026-11-07T10:00:00Z"), FixedOffset::utc())
            .expect("next");
        assert_eq!(next, at("2026-11-09T09:00:00Z"));
    }

    #[test]
    fn cron_steps_require_star_or_range_and_never_overflow() {
        // 参照实现不接受裸 `N/step`：不能把 step 丢掉后当成单个 N。
        assert!(parse_cron("5/10 * * * *").is_err());

        // 超大 step 必须安全结束，而不是在 debug/test 构建里溢出 panic。
        let expression = parse_cron("1-59/4294967295 * * * *").expect("huge step");
        let next = expression
            .next_after(at("2026-11-07T10:07:30Z"), FixedOffset::utc())
            .expect("next");
        assert_eq!(next, at("2026-11-07T11:01:00Z"));
    }

    #[test]
    fn cron_timezone_offset_applies() {
        let expression = parse_cron("0 9 * * *").expect("cron");
        let offset = FixedOffset::parse("+08:00").expect("offset");
        let next = expression
            .next_after(at("2026-11-07T00:00:00Z"), offset)
            .expect("next");
        assert_eq!(next, at("2026-11-07T01:00:00Z"));
    }

    #[test]
    fn cron_rejects_invalid_expressions() {
        for expression in [
            "0 9 * *",
            "60 * * * *",
            "* 24 * * *",
            "*/0 * * * *",
            "0 9 * * * *",
        ] {
            assert!(parse_cron(expression).is_err(), "{expression}");
        }
    }

    #[test]
    fn schedule_specs_compile_to_expected_shapes() {
        let compiled =
            CompiledSchedule::from_spec(&ScheduleSpec::duration("90m"), "UTC").expect("duration");
        assert_eq!(compiled, CompiledSchedule::Duration { seconds: 5_400 });

        let compiled =
            CompiledSchedule::from_spec(&ScheduleSpec::every("15m"), "UTC").expect("every");
        assert_eq!(compiled, CompiledSchedule::Every { seconds: 900 });

        let compiled =
            CompiledSchedule::from_spec(&ScheduleSpec::cron("0 9 * * 1-5"), "UTC").expect("cron");
        assert!(matches!(compiled, CompiledSchedule::Cron { .. }));
        assert_eq!(
            compiled.half_period_secs(at("2026-11-07T10:00:00Z")),
            Some(43_200)
        );

        let compiled =
            CompiledSchedule::from_spec(&ScheduleSpec::at("2026-11-08T09:00:00Z"), "UTC")
                .expect("one shot");
        assert_eq!(
            compiled,
            CompiledSchedule::OneShot {
                at: at("2026-11-08T09:00:00Z")
            }
        );
        assert!(compiled.is_one_shot());

        // 字符串自动判定。
        assert!(matches!(
            CompiledSchedule::from_spec(&ScheduleSpec::Text("*/5 * * * *".into()), "UTC")
                .expect("cron text"),
            CompiledSchedule::Cron { .. }
        ));
        assert!(matches!(
            CompiledSchedule::from_spec(&ScheduleSpec::Text("2026-11-08T09:00:00Z".into()), "UTC")
                .expect("iso text"),
            CompiledSchedule::OneShot { .. }
        ));
        assert_eq!(
            CompiledSchedule::from_spec(&ScheduleSpec::Text("30m".into()), "UTC")
                .expect("duration"),
            CompiledSchedule::Duration { seconds: 1_800 }
        );
    }

    #[test]
    fn every_aligns_to_epoch_multiples() {
        let compiled = CompiledSchedule::Every { seconds: 900 };
        assert_eq!(compiled.initial_next(100).expect("next"), 900);
        assert_eq!(compiled.initial_next(900).expect("next"), 1_800);
        assert_eq!(compiled.next_after(900).expect("next"), 1_800);
    }

    #[test]
    fn invalid_object_combination_is_rejected() {
        let object = ScheduleObject {
            duration: Some("1h".into()),
            every: Some("1h".into()),
            ..ScheduleObject::default()
        };
        let error = CompiledSchedule::from_spec(&ScheduleSpec::Object(object), "UTC")
            .expect_err("two fields");
        assert!(error.to_string().contains("exactly one"), "{error}");
    }

    #[test]
    fn unsupported_timezone_is_an_error() {
        let error = CompiledSchedule::from_spec(&ScheduleSpec::cron("0 9 * * *"), "Asia/Shanghai")
            .expect_err("iana unsupported");
        assert!(
            error.to_string().contains("unsupported timezone"),
            "{error}"
        );
    }
}
