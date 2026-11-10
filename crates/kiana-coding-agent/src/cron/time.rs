//! cron 的时间基元：epoch 秒、UTC 民用日期换算、ISO 8601 与固定偏移时区。
//!
//! 为什么不用 `chrono`：`kiana-coding-agent` 的依赖清单里没有 chrono（只有 `kiana-core`
//! 内部使用）。本模块只需要 UTC 与固定偏移，Hinnant 的 civil/day 算法足够，且没有
//! 时区数据库依赖。
//!
//! 已知限制：不支持 IANA 时区名（如 `Asia/Shanghai`）。这需要 tzdata；当前只接受
//! `UTC`/`Z`/`+HH:MM`/`-HH:MM`/`Local`（固定偏移，取自 `localtime` 的此刻偏移）。
//! 无效时区在 job 校验阶段报错，不会静默按 UTC 运行。

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// 一天的秒数。
pub const SECS_PER_DAY: i64 = 86_400;

/// 时间解析错误。
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum TimeError {
    /// 空输入。
    #[error("timestamp is empty")]
    Empty,
    /// 结构不合法。
    #[error("invalid timestamp {input:?}: {message}")]
    Invalid {
        /// 原始输入。
        input: String,
        /// 原因。
        message: String,
    },
    /// 字段越界。
    #[error("timestamp {input:?} has out-of-range {field}: {value}")]
    OutOfRange {
        /// 原始输入。
        input: String,
        /// 字段名。
        field: &'static str,
        /// 值。
        value: i64,
    },
}

/// 当前 epoch 秒。
pub fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or(0)
}

/// `year-month-day` → 距 1970-01-01 的天数（Hinnant）。
pub fn days_from_civil(year: i32, month: u32, day: u32) -> i64 {
    let year = year as i64 - if month <= 2 { 1 } else { 0 };
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let yoe = year - era * 400; // [0, 399]
    let month = month as i64;
    let day = day as i64;
    let doy = (153 * (month + if month > 2 { -3 } else { 9 }) + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// 距 1970-01-01 的天数 → `(year, month, day)`（Hinnant）。
pub fn civil_from_days(days: i64) -> (i32, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097; // [0, 146096]
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = mp + if mp < 10 { 3 } else { -9 };
    let year = year + if month <= 2 { 1 } else { 0 };
    (year as i32, month as u32, day as u32)
}

/// 距 1970-01-01 的天数 → 星期（0 = Sunday）。
pub fn weekday_from_days(days: i64) -> u32 {
    (days + 4).rem_euclid(7) as u32
}

/// 固定偏移时区（分钟，东为正）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct FixedOffset {
    minutes: i32,
}

impl FixedOffset {
    /// 固定偏移（分钟）。
    pub fn from_minutes(minutes: i32) -> Option<Self> {
        if (-24 * 60..=24 * 60).contains(&minutes) {
            Some(Self { minutes })
        } else {
            None
        }
    }

    /// UTC。
    pub fn utc() -> Self {
        Self { minutes: 0 }
    }

    /// 偏移秒数。
    pub fn seconds(self) -> i64 {
        i64::from(self.minutes) * 60
    }

    /// 偏移分钟。
    pub fn minutes(self) -> i32 {
        self.minutes
    }

    /// 解析 `UTC`/`Z`/`+08:00`/`-05:30`/`Local`。
    pub fn parse(input: &str) -> Option<Self> {
        let value = input.trim();
        if value.is_empty() || value.eq_ignore_ascii_case("utc") || value.eq_ignore_ascii_case("z")
        {
            return Some(Self::utc());
        }
        if value.eq_ignore_ascii_case("local") {
            return local_offset();
        }
        let sign = match value.as_bytes().first()? {
            b'+' => 1,
            b'-' => -1,
            _ => return None,
        };
        let body = &value[1..];
        let (hours, minutes) = match body.split_once(':') {
            Some((hours, minutes)) => (hours.parse::<i32>().ok()?, minutes.parse::<i32>().ok()?),
            None => {
                // `+08` 或 `+0800`。先确认全是 ASCII 数字再切片：非 ASCII 的 4 字节
                // 输入（例如 `+€a`）会让 `body[..2]` 落在字符边界外并 panic。
                if body.len() == 4 && body.bytes().all(|byte| byte.is_ascii_digit()) {
                    (
                        body[..2].parse::<i32>().ok()?,
                        body[2..].parse::<i32>().ok()?,
                    )
                } else {
                    (body.parse::<i32>().ok()?, 0)
                }
            }
        };
        if !(0..=23).contains(&hours) || !(0..=59).contains(&minutes) {
            return None;
        }
        Self::from_minutes(sign * (hours * 60 + minutes))
    }
}

fn local_offset() -> Option<FixedOffset> {
    #[cfg(unix)]
    {
        // SAFETY: `localtime_r` 写入调用方提供的 `tm`，不持有全局状态。
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .ok()?
            .as_secs() as libc::time_t;
        let mut tm: libc::tm = unsafe { std::mem::zeroed() };
        let pointer = unsafe { libc::localtime_r(&now, &mut tm) };
        if pointer.is_null() {
            return None;
        }
        FixedOffset::from_minutes(tm.tm_gmtoff as i32 / 60)
    }
    #[cfg(not(unix))]
    {
        None
    }
}

/// 解析 ISO 8601 时间戳，返回 epoch 秒。
///
/// 接受：`YYYY-MM-DD`、`YYYY-MM-DDTHH:MM[:SS][.fff][Z|±HH:MM]`、空格代替 `T`。
/// 无偏移时按 UTC 解释。
pub fn parse_iso8601(input: &str) -> Result<i64, TimeError> {
    let raw = input.trim();
    if raw.is_empty() {
        return Err(TimeError::Empty);
    }
    let invalid = |message: &str| TimeError::Invalid {
        input: raw.to_string(),
        message: message.to_string(),
    };
    let (date_part, time_part) = match raw.find(['T', ' ']) {
        Some(index) => (&raw[..index], Some(&raw[index + 1..])),
        None => (raw, None),
    };
    let mut date_fields = date_part.split('-');
    let year = date_fields
        .next()
        .and_then(|value| value.parse::<i32>().ok())
        .ok_or_else(|| invalid("missing year"))?;
    let month = date_fields
        .next()
        .and_then(|value| value.parse::<u32>().ok())
        .ok_or_else(|| invalid("missing month"))?;
    let day = date_fields
        .next()
        .and_then(|value| value.parse::<u32>().ok())
        .ok_or_else(|| invalid("missing day"))?;
    if date_fields.next().is_some() {
        return Err(invalid("too many date fields"));
    }
    if !(1..=12).contains(&month) {
        return Err(TimeError::OutOfRange {
            input: raw.to_string(),
            field: "month",
            value: i64::from(month),
        });
    }
    let max_day = days_in_month(year, month);
    if day == 0 || day > max_day {
        return Err(TimeError::OutOfRange {
            input: raw.to_string(),
            field: "day",
            value: i64::from(day),
        });
    }

    let (hour, minute, second, offset) = match time_part {
        None => (0, 0, 0, FixedOffset::utc()),
        Some(time) => parse_time_of_day(time, raw)?,
    };
    Ok(days_from_civil(year, month, day) * SECS_PER_DAY
        + i64::from(hour) * 3_600
        + i64::from(minute) * 60
        + i64::from(second)
        - offset.seconds())
}

fn parse_time_of_day(time: &str, raw: &str) -> Result<(u32, u32, u32, FixedOffset), TimeError> {
    let invalid = |message: &str| TimeError::Invalid {
        input: raw.to_string(),
        message: message.to_string(),
    };
    if time.is_empty() {
        return Err(invalid("empty time"));
    }
    let (body, offset) = if time.ends_with('Z') || time.ends_with('z') {
        (&time[..time.len() - 1], FixedOffset::utc())
    } else if let Some(index) = time.rfind(['+', '-']) {
        let offset =
            FixedOffset::parse(&time[index..]).ok_or_else(|| invalid("invalid UTC offset"))?;
        (&time[..index], offset)
    } else {
        (time, FixedOffset::utc())
    };
    // 秒的小数部分对 cron 无意义，直接丢弃。
    let body = body.split('.').next().unwrap_or(body);
    let mut fields = body.split(':');
    let hour = fields
        .next()
        .and_then(|value| value.parse::<u32>().ok())
        .ok_or_else(|| invalid("missing hour"))?;
    let minute = fields
        .next()
        .and_then(|value| value.parse::<u32>().ok())
        .ok_or_else(|| invalid("missing minute"))?;
    let second = fields
        .next()
        .map(|value| value.parse::<u32>())
        .transpose()
        .map_err(|_| invalid("invalid seconds"))?
        .unwrap_or(0);
    if fields.next().is_some() {
        return Err(invalid("too many time fields"));
    }
    if hour > 23 {
        return Err(TimeError::OutOfRange {
            input: raw.to_string(),
            field: "hour",
            value: i64::from(hour),
        });
    }
    if minute > 59 {
        return Err(TimeError::OutOfRange {
            input: raw.to_string(),
            field: "minute",
            value: i64::from(minute),
        });
    }
    if second > 59 {
        return Err(TimeError::OutOfRange {
            input: raw.to_string(),
            field: "second",
            value: i64::from(second),
        });
    }
    Ok((hour, minute, second, offset))
}

fn days_in_month(year: i32, month: u32) -> u32 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if is_leap_year(year) => 29,
        2 => 28,
        _ => 0,
    }
}

fn is_leap_year(year: i32) -> bool {
    (year % 4 == 0 && year % 100 != 0) || year % 400 == 0
}

/// 格式化为 `YYYY-MM-DDTHH:MM:SSZ`（UTC）。
pub fn format_iso8601(epoch_secs: i64) -> String {
    let days = epoch_secs.div_euclid(SECS_PER_DAY);
    let seconds = epoch_secs.rem_euclid(SECS_PER_DAY);
    let (year, month, day) = civil_from_days(days);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        seconds / 3_600,
        (seconds % 3_600) / 60,
        seconds % 60
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn civil_conversions_round_trip() {
        for days in [-10_000i64, -1, 0, 1, 19_000, 100_000] {
            let (year, month, day) = civil_from_days(days);
            assert_eq!(days_from_civil(year, month, day), days, "days={days}");
        }
        assert_eq!(days_from_civil(1970, 1, 1), 0);
        assert_eq!(weekday_from_days(0), 4, "1970-01-01 was Thursday");
    }

    #[test]
    fn malformed_offsets_do_not_panic_on_char_boundaries() {
        // 4 字节但第二字节不是 ASCII 数字：旧实现会在 `body[..2]` 处 panic。
        assert_eq!(FixedOffset::parse("+€a"), None);
        assert_eq!(FixedOffset::parse("+\u{4e00}1"), None);
        assert!(parse_iso8601("2026-11-07T09:30:00+€a").is_err());
    }

    #[test]
    fn parses_iso_variants() {
        assert_eq!(parse_iso8601("1970-01-01T00:00:00Z").expect("epoch"), 0);
        assert_eq!(parse_iso8601("1970-01-02").expect("date only"), 86_400);
        assert_eq!(
            parse_iso8601("2026-11-07T09:30:15+08:00").expect("offset"),
            parse_iso8601("2026-11-07T01:30:15Z").expect("utc")
        );
        assert_eq!(
            parse_iso8601("2026-11-07 09:30").expect("space"),
            parse_iso8601("2026-11-07T09:30:00Z").expect("utc")
        );
        assert_eq!(
            parse_iso8601("2026-11-07T09:30:00.123Z").expect("fraction"),
            parse_iso8601("2026-11-07T09:30:00Z").expect("utc")
        );
    }

    #[test]
    fn rejects_invalid_timestamps() {
        assert!(parse_iso8601("").is_err());
        assert!(parse_iso8601("2026-13-01").is_err());
        assert!(parse_iso8601("2026-02-30").is_err());
        assert!(parse_iso8601("2026-11-07T24:00:00Z").is_err());
        assert!(parse_iso8601("2026-11-07T09:61:00Z").is_err());
    }

    #[test]
    fn formats_utc() {
        assert_eq!(format_iso8601(0), "1970-01-01T00:00:00Z");
        assert_eq!(
            format_iso8601(parse_iso8601("2026-11-07T09:30:15Z").expect("ts")),
            "2026-11-07T09:30:15Z"
        );
    }

    #[test]
    fn fixed_offsets_parse() {
        assert_eq!(FixedOffset::parse("UTC"), Some(FixedOffset::utc()));
        assert_eq!(
            FixedOffset::parse("+08:00").expect("offset").seconds(),
            28_800
        );
        assert_eq!(
            FixedOffset::parse("-05:30").expect("offset").seconds(),
            -19_800
        );
        assert_eq!(FixedOffset::parse("+0800").expect("offset").minutes(), 480);
        assert!(FixedOffset::parse("Asia/Shanghai").is_none());
        assert!(FixedOffset::parse("+25:00").is_none());
    }
}
