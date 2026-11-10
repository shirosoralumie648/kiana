//! `packages/agent/test/utils/get-current-time.ts`：测试用当前时间工具。
//!
//! TS 用 `date.toLocaleString("en-US", { timeZone, dateStyle: "full", timeStyle: "long" })`
//! 得到形如 `Monday, October 26, 2026 at 10:00:00 AM GMT+8` 的文本（ICU 输出）。Rust 侧用
//! ICU4X（`icu_datetime` 的 CLDR `en-US` 数据）逐字复刻同一格式：
//!
//! - `dateStyle: "full"`（ECMA-402：weekday、year、month、day）→ `fieldsets::YMDE::long()`。
//! - `timeStyle: "long"`（ECMA-402：hour、minute、second，`timeZoneName: "short"`）→
//!   `fieldsets::T::hms()` + `fieldsets::zone::SpecificShort`。
//! - 日期与时间之间是 CLDR 的 glue pattern（`en` 为 `{1} 'at' {0}`）。
//!
//! 时区 id 用 ICU4X `IanaParserExtended` 解析：与 ICU 一样大小写不敏感、支持 tzdb 别名
//! （`utc`、`asia/shanghai`、`US/Eastern`、`America/Atka`…），并拒绝 ICU 不认识的 id
//! （`Factory`、`posixrules`、`localtime`、`right/UTC`…）。偏移从系统 tzdata
//! （`/usr/share/zoneinfo`，可被 `TZDIR` 覆盖）的 TZif 文件解析。
//!
//! 与 Node 26（ICU 78 / CLDR 48）的两处数据差异在 [`apply_zone_name_compat`] 里按 CLDR 48
//! 修正（ICU4X 2.4 用的是 CLDR 49）：
//!
//! 1. `GMT` / `GMT0` / `GMT+0` / `GMT-0` / `Etc/GMT` 在 ICU 里是 UTC 单例，短名写 `UTC`
//!    （ICU4X 把它们映射到 `Etc/GMT` 元区，会写 `GMT`）。
//! 2. CLDR 48 里 `Europe_UK` 元区（London/Jersey/Isle_of_Man/Guernsey/Dublin）标准时的短名是
//!    `GMT`；CLDR 49 去掉了它，ICU4X 对零偏移退回本地化偏移 `GMT+0`。
//!
//! 其余 id 与 Node 参考实现一致，对拍覆盖在 `tests/fixtures/`：
//! `get_current_time.json`（498 个 id × 1990–2026 的 5 个时刻）、`get_current_time_local.json`
//! （tzdata 里 599 个名字 × 2 个时刻，走 `TZ=<名字>` 的本地分支）、
//! `get_current_time_future.json`（498 个 id × 2038–2100 的 4 个时刻，验证尾串外推）。
//!
//! 无时区参数时复刻 Node 的进程默认时区语义（`TZ` 环境变量 → `/etc/localtime` → UTC）。
//! Node 26（ICU 78）对 `TZ` 的处理逐类对拍如下：
//!
//! - 未设置：系统时区（`/etc/localtime` 的链接目标）。
//! - 空串、大小写不匹配或根本不存在的 id（`""`、`utc`、`asia/shanghai`、`Not/AZone`、
//!   `Europe/Paris `、`Etc/Unknown`）：`Etc/Unknown`（零偏移、显示 `GMT+0`）。
//! - 绝对路径：可读的 TZif 文件生效（时区名用本地化 `GMT±h`），否则同样是 `Etc/Unknown`。
//! - `right/`、`posix/` 前缀被忽略（`right/UTC` → `UTC`）。
//! - POSIX `std offset` 固定偏移（`EST5`、`FOO5`、`GMT+5`、`<ABC>5`、`UTC0`；偏移只有整小时、
//!   超过 24 小时按 24 小时钳制）：固定偏移，时区名是本地化 `GMT±h`。
//! - 其余 POSIX TZ 写法（DST 规则 `EST5EDT,M3.2.0,M11.1.0`、带分钟的偏移 `EST5:30`、
//!   名字后有多余字符）：Node 回退到系统时区（`/etc/localtime`），这里同样。
//! - `posixrules`：ICU 的 tzdata 把它当作只有标准时的 EST（Node 全年 `GMT-5`）。
//!
//! TZif 的 v2+ POSIX-TZ 尾串（如 `EST5EDT,M3.2.0,M11.1.0`）会被解析：最后一次显式
//! transition 之后的时刻按尾串规则外推（见 [`posix_offset_at`]），与 Node/glibc 一致。
//!
//! 已知差异（get_current_time 只格式化「现在」，不触发）：1970 年前的 CLDR 元区短名
//! （`format_time_at(1950-…, "America/New_York")`：Node 写 `GMT-4`，ICU4X 写 `EDT`）；
//! 少数病态 `TZ` 串（`A5`、`<+05>-5` 之类）的 `Etc/Unknown` / 系统时区归属。

use std::path::{Path, PathBuf};

use icu_datetime::DateTimeFormatter;
use icu_datetime::fieldsets::zone::SpecificShort;
use icu_datetime::fieldsets::{self, Combo, YMDET};
use icu_locale::locale;
use icu_time::zone::iana::IanaParserExtended;
use icu_time::zone::{UtcOffset, ZoneNameTimestamp};
use icu_time::{TimeZone, ZonedDateTime};

use kiana_agent_core::types::{
    AgentTool, AgentToolError, AgentToolExecute, AgentToolResult, BoxFuture, JsonValue,
};
use serde_json::json;

use super::text_block;

/// `GetCurrentTimeResult extends AgentToolResult<{ utcTimestamp: number }>`。
pub type GetCurrentTimeResult = AgentToolResult<JsonValue>;

/// `getCurrentTime(timezone?)`。
pub fn get_current_time(timezone: Option<&str>) -> Result<GetCurrentTimeResult, AgentToolError> {
    let utc_millis = now_unix_millis();
    // TS：`if (timezone)` —— 空串是假值，走本地时区分支。
    let text = match timezone.filter(|value| !value.is_empty()) {
        Some(timezone) => format_time_at(utc_millis, timezone),
        None => Ok(format_local_time_at(utc_millis)),
    }
    .map_err(|message| AgentToolError {
        message,
        name: None,
        stack: None,
    })?;
    Ok(GetCurrentTimeResult {
        content: vec![text_block(text)],
        details: json!({ "utcTimestamp": utc_millis }),
        structured_content: None,
        usage: None,
        is_error: None,
        terminate: None,
    })
}

/// 当前 UTC 毫秒（`Date.now()`）。
pub fn now_unix_millis() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0)
}

/// 指定时刻 + 指定 IANA 时区的 `en-US` 文本（`toLocaleString` 的逐字格式）。
///
/// 非法时区返回 `Invalid timezone: …` 错误文本（含同一时刻的 `toISOString()`）。
pub fn format_time_at(utc_millis: i64, timezone: &str) -> Result<String, String> {
    let parsed = zone_parser().parse(timezone);
    if parsed.time_zone == TimeZone::UNKNOWN {
        return Err(invalid_timezone_message(timezone, utc_millis));
    }
    let Some(zone) = load_zone(parsed.canonical) else {
        // 宿主没有 tzdata（或该 zone 只存在于 ICU 自带数据里）时无法得到偏移。
        return Err(invalid_timezone_message(timezone, utc_millis));
    };
    let offset = local_offset(&zone, utc_millis.div_euclid(1000));
    // ICU 把 `GMT`/`GMT0`/`GMT+0`/`GMT-0`/`Etc/GMT` 当作 UTC 单例（短名 `UTC`）。
    let time_zone = if is_utc_named_alias(timezone) {
        utc_time_zone()
    } else {
        parsed.time_zone
    };
    Ok(format_with_zone(
        utc_millis,
        offset,
        time_zone,
        parsed.canonical,
    ))
}

/// 无时区参数时的 `toLocaleString("en-US", { dateStyle: "full", timeStyle: "long" })`：
/// 使用运行时默认时区（`TZ` 环境变量 → `/etc/localtime` → `/etc/timezone` → UTC）。
pub fn format_local_time_at(utc_millis: i64) -> String {
    let timezone = std::env::var("TZ").ok();
    format_local_time_at_with(utc_millis, timezone.as_deref())
}

/// [`format_local_time_at`] 的显式 `TZ` 版本（测试用，避免改进程环境变量）。
fn format_local_time_at_with(utc_millis: i64, tz: Option<&str>) -> String {
    match resolve_local_zone(tz) {
        LocalZone::Iana(name) => {
            let parsed = zone_parser().parse(&name);
            let Some(zone) = load_zone(parsed.canonical) else {
                return format_with_zone(utc_millis, 0, parsed.time_zone, parsed.canonical);
            };
            let offset = local_offset(&zone, utc_millis.div_euclid(1000));
            format_with_zone(utc_millis, offset, parsed.time_zone, parsed.canonical)
        }
        LocalZone::OffsetOnly(path) => match load_zone_at(&path) {
            Some(zone) => {
                // 绝对路径 / ICU 叫不出名字的 zone 文件：Node 只用 TZif 里最后一个标准时
                // 类型，不跟随 DST（`TZ=/usr/share/zoneinfo/America/New_York` 全年 GMT-5，
                // `…/Australia/Sydney` 全年 GMT+10）。
                format_with_zone(
                    utc_millis,
                    zone.last_standard,
                    TimeZone::UNKNOWN,
                    "Etc/Unknown",
                )
            }
            // 解析不了（损坏/非 TZif）：Node 的 `Etc/Unknown` 回退。
            None => format_with_zone(utc_millis, 0, TimeZone::UNKNOWN, "Etc/Unknown"),
        },
        LocalZone::Fixed(offset) => {
            format_with_zone(utc_millis, offset, TimeZone::UNKNOWN, "Etc/Unknown")
        }
        LocalZone::Unknown => format_with_zone(utc_millis, 0, TimeZone::UNKNOWN, "Etc/Unknown"),
    }
}

// ---------------------------------------------------------------------------
// 默认时区（Node 的 `TZ` 语义）
// ---------------------------------------------------------------------------

/// `TZ`/系统默认时区的解析结果。
enum LocalZone {
    /// ICU 认识的 IANA id（可用 CLDR 短名）。
    Iana(String),
    /// 只有 tzdata、没有 CLDR 名的 zone（`TZ=EST5EDT`、`TZ=<绝对路径>`）：
    /// ICU 用本地化 GMT 偏移（`GMT-4`）显示，且只用 TZif 里最后一个标准时类型
    /// （Node：`TZ=/usr/share/zoneinfo/America/New_York` 全年 `GMT-5`）。
    OffsetOnly(PathBuf),
    /// `Etc/Unknown`（`TZ=""`、大小写不匹配的 id）：零偏移、显示 `GMT+0`。
    Unknown,
    /// POSIX `STDoffset`（无 DST）：固定偏移；`TZ=posixrules` 也走这里（ICU 的 tzdata
    /// 把它当作固定的 EST）。
    Fixed(i32),
}

fn resolve_local_zone(tz: Option<&str>) -> LocalZone {
    let Some(tz) = tz else {
        return system_zone();
    };
    // libc 允许 `TZ=:Asia/Tokyo`。
    let tz = tz.strip_prefix(':').unwrap_or(tz);
    if tz.is_empty() {
        return LocalZone::Unknown;
    }
    // ICU 的默认时区解析会忽略 `right/`、`posix/` 前缀（Node：TZ=right/UTC → UTC）。
    let tz = tz
        .strip_prefix("right/")
        .or_else(|| tz.strip_prefix("posix/"))
        .unwrap_or(tz);
    if tz.starts_with('/') {
        // 绝对路径只有在是可读文件时才生效；目录、不存在的路径在 Node 里是 `Etc/Unknown`
        // （不是系统时区）。
        return if Path::new(tz).is_file() {
            LocalZone::OffsetOnly(PathBuf::from(tz))
        } else {
            LocalZone::Unknown
        };
    }
    if tz == "posixrules" {
        // ICU 的 tzdata 里 `posixrules` 只有标准时 EST（Node 26 全年 `GMT-5`），
        // 不跟随 America/New_York 的 DST。
        return LocalZone::Fixed(-5 * 3600);
    }
    if let Some(path) = zone_path(tz)
        && path.is_file()
    {
        // 文件查找是大小写敏感的（Node：TZ=utc / TZ=asia/shanghai → Etc/Unknown），
        // 而 ICU4X 的解析器大小写不敏感，因此要求拼写与归一化结果严格相等。
        let parsed = zone_parser().parse(tz);
        if parsed.time_zone != TimeZone::UNKNOWN
            && (parsed.normalized == tz || parsed.canonical == tz)
        {
            return LocalZone::Iana(tz.to_string());
        }
        return LocalZone::OffsetOnly(path);
    }
    match classify_posix_tz(tz) {
        Some(PosixTzForm::Fixed(offset)) => LocalZone::Fixed(offset),
        // Node/ICU 无法表示带 DST 规则或带分钟偏移的 POSIX TZ 串：回退到系统时区。
        Some(PosixTzForm::Unsupported) => system_zone(),
        None => LocalZone::Unknown,
    }
}

/// 系统默认时区：`/etc/localtime` 的链接目标 → `/etc/timezone` → UTC。
fn system_zone() -> LocalZone {
    if let Ok(target) = std::fs::read_link("/etc/localtime")
        && let Some(name) = target
            .to_str()
            .and_then(|path| path.strip_prefix("/usr/share/zoneinfo/"))
        && zone_path(name).is_some_and(|path| path.is_file())
    {
        return LocalZone::Iana(name.to_string());
    }
    if let Ok(text) = std::fs::read_to_string("/etc/timezone") {
        let name = text.trim();
        if !name.is_empty() && zone_path(name).is_some_and(|path| path.is_file()) {
            return LocalZone::Iana(name.to_string());
        }
    }
    LocalZone::Iana("UTC".to_string())
}

/// [`classify_posix_tz`] 的结论。
enum PosixTzForm {
    /// POSIX `std offset` 固定偏移（秒，东正西负）：`EST5` → `-5h`，`UTC0` → `0`。
    ///
    /// POSIX 的符号与 ISO 相反：`EST5` 表示 UTC−5。
    Fixed(i32),
    /// 看起来是 POSIX TZ，但不是 ICU 能表示的固定偏移形式（DST 规则、`hh:mm` 偏移、
    /// 名字后有多余字符、3 位以上小时…）。Node 对这种串回退到系统时区。
    Unsupported,
}

/// 把 `TZ` 值按 POSIX 的 `std offset` 形式分类。
///
/// 逐类对拍 Node 26（ICU 78）：`EST5`/`FOO5`/`GMT+5`/`UTC0`/`<ABC>5`/`JST-9` 是固定偏移
/// （小时按 24 钳制：`ABC24`/`ABC99` → ±24h）；`EST5:30`（带分钟）、`ABC5x`（多余字符）、
/// `EST5EDT,M3.2.0,M11.1.0`（DST 规则）、`ABC123`（3 位小时）回退到系统时区；
/// `ABC`（没有偏移）、`A5`/`AB5`（名字不足 3 个字母）是非法 id（`Etc/Unknown`）。
fn classify_posix_tz(tz: &str) -> Option<PosixTzForm> {
    let bytes = tz.as_bytes();
    let first = *bytes.first()?;
    let mut position;
    if first == b'<' {
        // `<...>` 名字：ICU 只接受至少 3 个字母的名字。
        let end = tz.find('>')?;
        let name = &tz[1..end];
        if name.is_empty() {
            return None;
        }
        if !name.bytes().all(|byte| byte.is_ascii_alphabetic()) {
            // `<ABC5>`、`<+05>-5`：名字里带数字/符号，Node 当作坏 POSIX 串 → 系统时区。
            return Some(PosixTzForm::Unsupported);
        }
        if name.len() < 3 {
            return None;
        }
        position = end + 1;
    } else if first.is_ascii_alphabetic() {
        position = 0;
        while position < bytes.len() && bytes[position].is_ascii_alphabetic() {
            position += 1;
        }
        if position < 3 {
            // `A5`/`AB5` 之类短名字不是合法 POSIX 名；但若后面还有多余字符
            // （`A5B`、`AB5C`）Node 仍当作坏 POSIX 串 → 系统时区。
            let rest = &tz[position..];
            let digits = rest.trim_start_matches(['+', '-']);
            let digit_count = digits.bytes().take_while(u8::is_ascii_digit).count();
            if digit_count == 0 {
                return None;
            }
            let trailing = &digits[digit_count..];
            if digit_count > 2 || !trailing.is_empty() {
                return Some(PosixTzForm::Unsupported);
            }
            return None;
        }
    } else {
        return None;
    }

    // 偏移：可选符号 + 1–2 位小时（这里只认 ICU 支持的整小时形式）。
    let mut sign = 1i32;
    if let Some(byte) = bytes.get(position) {
        match byte {
            b'+' => position += 1,
            b'-' => {
                sign = -1;
                position += 1;
            }
            _ => {}
        }
    }
    let digits_start = position;
    let mut hours = 0i32;
    while position < bytes.len() && bytes[position].is_ascii_digit() {
        hours = hours * 10 + i32::from(bytes[position] - b'0');
        position += 1;
    }
    let digit_count = position - digits_start;
    if digit_count == 0 {
        // 名字后面没有偏移（`ABC`、`<ABC>`）→ 不是 TZ 串。
        return None;
    }
    if digit_count > 2 || position != bytes.len() {
        // 3 位以上小时，或偏移后面还有内容（`hh:mm`、DST 名与规则、空白…）。
        return Some(PosixTzForm::Unsupported);
    }
    let hours = hours.min(24);
    Some(PosixTzForm::Fixed(-sign * hours * 3600))
}

// ---------------------------------------------------------------------------
// 格式化（ICU4X）
// ---------------------------------------------------------------------------

/// `en-US` 的 `dateStyle: "full"` + `timeStyle: "long"` 字段集。
type FullDateTimeFormatter = DateTimeFormatter<Combo<YMDET, SpecificShort>>;

thread_local! {
    /// ICU4X 的 formatter 不是 `Sync`（baked data 里的 `Rc`），按线程缓存。
    static FORMATTER: std::cell::RefCell<Option<FullDateTimeFormatter>> =
        const { std::cell::RefCell::new(None) };
}

fn with_formatter<R>(f: impl FnOnce(&FullDateTimeFormatter) -> R) -> R {
    FORMATTER.with(|cell| {
        let mut slot = cell.borrow_mut();
        let formatter = slot.get_or_insert_with(|| {
            DateTimeFormatter::try_new(
                locale!("en-US").into(),
                fieldsets::YMDE::long()
                    .with_time_hms()
                    .with_zone(SpecificShort),
            )
            .expect("en-US CLDR data is compiled in")
        });
        f(formatter)
    })
}

/// ICU4X 的 IANA id 解析器（大小写不敏感 + tzdb 别名，与 ICU 一致）。
fn zone_parser() -> icu_time::zone::iana::IanaParserExtendedBorrowed<'static> {
    IanaParserExtended::new()
}

fn utc_time_zone() -> TimeZone {
    zone_parser().parse("UTC").time_zone
}

/// ICU 把 `GMT`/`GMT0`/`GMT+0`/`GMT-0`/`Etc/GMT` 当作 UTC 单例（短名 `UTC`）。
fn is_utc_named_alias(timezone: &str) -> bool {
    matches!(
        timezone.to_ascii_lowercase().as_str(),
        "gmt" | "gmt0" | "gmt+0" | "gmt-0" | "etc/gmt"
    )
}

/// CLDR 48（Node 26）里 `Europe_UK` 元区的标准时短名是 `GMT`；CLDR 49 去掉了它，
/// ICU4X 对零偏移回退成本地化偏移 `GMT+0`。按 canonical zone 修正。
const EUROPE_UK_ZONES: [&str; 5] = [
    "Europe/London",
    "Europe/Jersey",
    "Europe/Isle_of_Man",
    "Europe/Guernsey",
    "Europe/Dublin",
];

fn apply_zone_name_compat(text: &mut String, canonical: &str) {
    if EUROPE_UK_ZONES.contains(&canonical) && text.ends_with(" GMT+0") {
        text.truncate(text.len() - "GMT+0".len());
        text.push_str("GMT");
    }
}

/// 按 ICU 的 `timeZoneName: "short"` 规则格式化。
///
/// V8 把 CLDR 时间格式里的 U+202F（窄不换行空格）替换成普通空格，Node 输出逐字如此。
fn format_with_zone(
    utc_millis: i64,
    offset_seconds: i32,
    time_zone: TimeZone,
    canonical: &str,
) -> String {
    // `TZ=ABC24` / `ABC-99`：Node 把偏移钳到 ±24 小时，且不写时区名（`… at 2:00:00 AM `
    // 带尾随空格）。ICU4X 的 `UtcOffset` 表示不了 24 小时，所以把时刻平移后按零偏移
    // 格式化，再剪掉末尾的时区名。
    if offset_seconds.abs() >= 86_400 {
        let shifted = utc_millis + i64::from(offset_seconds) * 1000;
        let mut text = format_with_zone(shifted, 0, TimeZone::UNKNOWN, "Etc/Unknown");
        if let Some(space) = text.rfind(' ') {
            text.truncate(space + 1);
        }
        return text;
    }
    let offset = UtcOffset::try_from_seconds(offset_seconds).unwrap_or(UtcOffset::zero());
    let base = ZonedDateTime::from_epoch_milliseconds_and_utc_offset(utc_millis, offset);
    let zone = time_zone
        .with_offset(Some(offset))
        .with_zone_name_timestamp(ZoneNameTimestamp::from_epoch_seconds(
            utc_millis.div_euclid(1000),
        ));
    let zoned = ZonedDateTime {
        date: base.date,
        time: base.time,
        zone,
    };
    let mut text =
        with_formatter(|formatter| formatter.format(&zoned).to_string()).replace('\u{202F}', " ");
    apply_zone_name_compat(&mut text, canonical);
    text
}

fn invalid_timezone_message(timezone: &str, utc_millis: i64) -> String {
    format!(
        "Invalid timezone: {timezone}. Current UTC time: {}",
        iso_string(utc_millis)
    )
}

// ---------------------------------------------------------------------------
// `Date.prototype.toISOString()`
// ---------------------------------------------------------------------------

/// `Date.prototype.toISOString()`：`YYYY-MM-DDTHH:mm:ss.sssZ`。
pub fn iso_string(utc_millis: i64) -> String {
    let seconds = utc_millis.div_euclid(1000);
    let millis = utc_millis.rem_euclid(1000);
    let days = seconds.div_euclid(86_400);
    let seconds_of_day = seconds.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    let hour = seconds_of_day / 3600;
    let minute = (seconds_of_day % 3600) / 60;
    let second = seconds_of_day % 60;
    if (0..=9999).contains(&year) {
        format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}.{millis:03}Z")
    } else {
        // ECMAScript 扩展年份：`±YYYYYY`。
        let sign = if year < 0 { "-" } else { "+" };
        format!(
            "{sign}{:06}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}.{millis:03}Z",
            year.abs()
        )
    }
}

/// `civil_from_days`（Howard Hinnant）：自 1970-01-01 起的天数 → `(年, 月, 日)`。
fn civil_from_days(days_since_epoch: i64) -> (i64, u32, u32) {
    let z = days_since_epoch + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let day_of_era = (z - era * 146_097) as u64; // [0, 146096]
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year = year_of_era as i64 + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = (day_of_year - (153 * month_prime + 2) / 5 + 1) as u32;
    let month = if month_prime < 10 {
        month_prime + 3
    } else {
        month_prime - 9
    } as u32;
    (if month <= 2 { year + 1 } else { year }, month, day)
}

// ---------------------------------------------------------------------------
// tzdata（TZif）读取
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct ZoneType {
    /// UTC 偏移（秒）。
    offset: i32,
}

#[derive(Debug)]
struct ZoneData {
    /// `(UTC 秒, 类型下标)`，按时间升序。
    transitions: Vec<(i64, u8)>,
    types: Vec<ZoneType>,
    /// 首个非 DST 类型（RFC 8536：首个 transition 之前使用它）。
    first_standard: u8,
    /// 最后一个非 DST 类型的偏移：Node 对「文件路径 / ICU 不认识的名字」只用它。
    last_standard: i32,
    /// v2+ 文件末尾的 POSIX TZ 尾串（RFC 8536 §3.3），用来外推最后一个 transition 之后的规则。
    footer: Option<PosixTz>,
}

fn zone_path(timezone: &str) -> Option<PathBuf> {
    if timezone.is_empty() || timezone.starts_with('/') {
        return None;
    }
    let root = std::env::var("TZDIR").unwrap_or_else(|_| "/usr/share/zoneinfo".to_string());
    let mut path = PathBuf::from(root);
    for component in timezone.split('/') {
        if component.is_empty() || component == "." || component == ".." {
            return None;
        }
        path.push(component);
    }
    Some(path)
}

fn load_zone(timezone: &str) -> Option<ZoneData> {
    load_zone_at(&zone_path(timezone)?)
}

fn load_zone_at(path: &Path) -> Option<ZoneData> {
    let data = std::fs::read(path).ok()?;
    parse_tzif(&data)
}

fn be_u32(data: &[u8], at: usize) -> Option<u32> {
    let bytes = data.get(at..at + 4)?;
    Some(u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
}

fn block_len(counts: &[u32; 6], time_size: usize) -> Option<usize> {
    let isutcnt = counts[0] as usize;
    let isstdcnt = counts[1] as usize;
    let leapcnt = counts[2] as usize;
    let timecnt = counts[3] as usize;
    let typecnt = counts[4] as usize;
    let charcnt = counts[5] as usize;
    timecnt
        .checked_mul(time_size)?
        .checked_add(timecnt)?
        .checked_add(typecnt.checked_mul(6)?)?
        .checked_add(charcnt)?
        .checked_add(leapcnt.checked_mul(time_size + 4)?)?
        .checked_add(isstdcnt)?
        .checked_add(isutcnt)
}

fn read_counts(data: &[u8], at: usize) -> Option<[u32; 6]> {
    Some([
        be_u32(data, at)?,
        be_u32(data, at + 4)?,
        be_u32(data, at + 8)?,
        be_u32(data, at + 12)?,
        be_u32(data, at + 16)?,
        be_u32(data, at + 20)?,
    ])
}

fn parse_tzif(data: &[u8]) -> Option<ZoneData> {
    if data.len() < 44 || &data[..4] != b"TZif" {
        return None;
    }
    let version = data[4];
    let counts_v1 = read_counts(data, 20)?;
    let mut position: usize = 44;
    let (counts, time_size) = if version >= b'2' {
        position = position.checked_add(block_len(&counts_v1, 4)?)?;
        if data.len() < position + 44 || &data[position..position + 4] != b"TZif" {
            return None;
        }
        let counts_v2 = read_counts(data, position + 20)?;
        position += 44;
        (counts_v2, 8usize)
    } else {
        (counts_v1, 4usize)
    };

    let timecnt = counts[3] as usize;
    let typecnt = counts[4] as usize;
    let charcnt = counts[5] as usize;
    let leapcnt = counts[2] as usize;
    let isstdcnt = counts[1] as usize;
    let isutcnt = counts[0] as usize;

    let times_start = position;
    let indices_start = times_start.checked_add(timecnt.checked_mul(time_size)?)?;
    let types_start = indices_start.checked_add(timecnt)?;
    let chars_start = types_start.checked_add(typecnt.checked_mul(6)?)?;
    let block_end = chars_start
        .checked_add(charcnt)?
        .checked_add(leapcnt.checked_mul(time_size + 4)?)?
        .checked_add(isstdcnt)?
        .checked_add(isutcnt)?;
    if data.len() < block_end {
        return None;
    }

    let mut transitions = Vec::with_capacity(timecnt);
    for index in 0..timecnt {
        let at = times_start + index * time_size;
        let bytes = data.get(at..at + time_size)?;
        let moment = if time_size == 8 {
            i64::from_be_bytes([
                bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
            ])
        } else {
            i64::from(i32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
        };
        transitions.push((moment, data[indices_start + index]));
    }

    let mut types = Vec::with_capacity(typecnt);
    let mut first_standard = None;
    let mut last_standard = None;
    for index in 0..typecnt {
        let at = types_start + index * 6;
        let bytes = data.get(at..at + 6)?;
        let offset = i32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
        let is_dst = bytes[4] != 0;
        if !is_dst {
            if first_standard.is_none() {
                first_standard = Some(index as u8);
            }
            last_standard = Some(offset);
        }
        types.push(ZoneType { offset });
    }
    let first_standard = first_standard.unwrap_or(0);
    let last_standard = last_standard.unwrap_or_else(|| types.last().map_or(0, |kind| kind.offset));

    if types.is_empty() {
        return None;
    }
    // v2+ 的数据块之后是 RFC 8536 §3.3 的尾串：`\n` + POSIX TZ 串 + `\n` + NUL
    // （空串表示没有后续规则）。
    let footer = if version >= b'2' {
        data.get(block_end..).and_then(|rest| {
            // 尾串前后各有一个换行；RFC 8536 说文件以 NUL 结尾，但 zic 生成的
            // tzdata 文件常常没有这个 NUL，所以两种都接受。
            let end = rest
                .iter()
                .position(|&byte| byte == 0)
                .unwrap_or(rest.len());
            std::str::from_utf8(&rest[..end])
                .ok()
                .map(str::trim)
                .and_then(parse_posix_tz)
        })
    } else {
        None
    };
    Some(ZoneData {
        transitions,
        types,
        first_standard,
        last_standard,
        footer,
    })
}

fn local_type(zone: &ZoneData, utc_seconds: i64) -> &ZoneType {
    let index = zone
        .transitions
        .partition_point(|(moment, _)| *moment <= utc_seconds);
    if index == 0 {
        return zone
            .types
            .get(zone.first_standard as usize)
            .unwrap_or(&zone.types[0]);
    }
    zone.types
        .get(zone.transitions[index - 1].1 as usize)
        .unwrap_or(&zone.types[0])
}

/// 指定 UTC 秒的偏移：最后一个 transition 之后按尾串规则外推（glibc/ICU 的行为），
/// 否则用显式 transition 表。
fn local_offset(zone: &ZoneData, utc_seconds: i64) -> i32 {
    if let Some(footer) = &zone.footer
        && zone
            .transitions
            .last()
            .is_none_or(|&(last_moment, _)| utc_seconds > last_moment)
    {
        return posix_offset_at(footer, utc_seconds);
    }
    local_type(zone, utc_seconds).offset
}

// ---------------------------------------------------------------------------
// POSIX TZ 规则（TZif 尾串）
// ---------------------------------------------------------------------------

/// TZif v2+ 尾串里的 POSIX TZ 规格（RFC 8536 §3.3）：`std offset [dst [offset][,start[/time],end[/time]]]`。
#[derive(Debug, Clone)]
struct PosixTz {
    /// 标准时偏移（秒，东正西负）。
    std_offset: i32,
    dst: Option<PosixDst>,
}

#[derive(Debug, Clone)]
struct PosixDst {
    /// DST 期间偏移（秒）。
    offset: i32,
    start: PosixTransition,
    end: PosixTransition,
}

#[derive(Debug, Clone)]
struct PosixTransition {
    date: PosixDate,
    /// 规则时刻（当地秒；`/24` 之类可跨日）。
    time: i32,
}

#[derive(Debug, Clone, Copy)]
enum PosixDate {
    /// `Jn`：1–365，2 月 29 日不计数。
    Julian(u32),
    /// `n`：0–365，2 月 29 日计数。
    DayOfYear(u32),
    /// `Mm.w.d`：`w` = 1–5（5 = 当月最后一个），`d` = 0（周日）–6。
    MonthWeekDay { month: u32, week: u32, weekday: u32 },
}

/// 解析 TZif 尾串；空串或无法解析时返回 `None`（沿用显式表的最后一个类型）。
fn parse_posix_tz(text: &str) -> Option<PosixTz> {
    if text.is_empty() {
        return None;
    }
    let mut parser = PosixTzParser {
        bytes: text.as_bytes(),
        position: 0,
    };
    parser.name()?;
    let std_offset = parser.offset()?;
    if parser.at_end() {
        return Some(PosixTz {
            std_offset,
            dst: None,
        });
    }
    parser.name()?;
    let offset = if parser.at_end() || parser.peek() == Some(b',') {
        std_offset + 3600
    } else {
        parser.offset()?
    };
    if parser.at_end() {
        // 没有规则：glibc 会用默认的 US 规则，但 tzdata 的尾串总是带规则；按标准时处理。
        return Some(PosixTz {
            std_offset,
            dst: None,
        });
    }
    if parser.peek() != Some(b',') {
        return None;
    }
    parser.position += 1;
    let start = parser.transition()?;
    if parser.peek() != Some(b',') {
        return None;
    }
    parser.position += 1;
    let end = parser.transition()?;
    if !parser.at_end() {
        return None;
    }
    Some(PosixTz {
        std_offset,
        dst: Some(PosixDst { offset, start, end }),
    })
}

struct PosixTzParser<'a> {
    bytes: &'a [u8],
    position: usize,
}

impl PosixTzParser<'_> {
    fn at_end(&self) -> bool {
        self.position >= self.bytes.len()
    }

    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.position).copied()
    }

    fn name(&mut self) -> Option<()> {
        match self.peek()? {
            b'<' => {
                let start = self.position + 1;
                let end = start
                    + self
                        .bytes
                        .get(start..)?
                        .iter()
                        .position(|&byte| byte == b'>')?;
                if end == start {
                    return None;
                }
                self.position = end + 1;
                Some(())
            }
            byte if byte.is_ascii_alphabetic() => {
                let start = self.position;
                while self.peek().is_some_and(|byte| byte.is_ascii_alphabetic()) {
                    self.position += 1;
                }
                (self.position - start >= 3).then_some(())
            }
            _ => None,
        }
    }

    fn offset(&mut self) -> Option<i32> {
        let mut sign = 1i32;
        match self.peek()? {
            b'+' => self.position += 1,
            b'-' => {
                sign = -1;
                self.position += 1;
            }
            _ => {}
        }
        let mut seconds = self.number(1, 3)? * 3600;
        if self.peek() == Some(b':') {
            self.position += 1;
            seconds += self.number(1, 2)? * 60;
            if self.peek() == Some(b':') {
                self.position += 1;
                seconds += self.number(1, 2)?;
            }
        }
        // POSIX 的符号与 ISO 相反：`EST5` 表示 UTC−5。
        Some(-sign * seconds)
    }

    fn number(&mut self, min_digits: usize, max_digits: usize) -> Option<i32> {
        let start = self.position;
        while self.position - start < max_digits
            && self.peek().is_some_and(|byte| byte.is_ascii_digit())
        {
            self.position += 1;
        }
        if self.position - start < min_digits {
            return None;
        }
        std::str::from_utf8(&self.bytes[start..self.position])
            .ok()?
            .parse()
            .ok()
    }

    fn transition(&mut self) -> Option<PosixTransition> {
        let date = self.date()?;
        let time = if self.peek() == Some(b'/') {
            self.position += 1;
            let mut sign = 1i32;
            match self.peek() {
                Some(b'+') => self.position += 1,
                Some(b'-') => {
                    sign = -1;
                    self.position += 1;
                }
                _ => {}
            }
            let mut seconds = self.number(1, 3)? * 3600;
            if self.peek() == Some(b':') {
                self.position += 1;
                seconds += self.number(1, 2)? * 60;
                if self.peek() == Some(b':') {
                    self.position += 1;
                    seconds += self.number(1, 2)?;
                }
            }
            sign * seconds
        } else {
            // 默认 02:00（RFC 8536 §3.3.1）。
            2 * 3600
        };
        Some(PosixTransition { date, time })
    }

    fn date(&mut self) -> Option<PosixDate> {
        match self.peek()? {
            b'J' => {
                self.position += 1;
                Some(PosixDate::Julian(self.number(1, 3)? as u32))
            }
            b'M' => {
                self.position += 1;
                let month = self.number(1, 2)? as u32;
                if self.peek() != Some(b'.') {
                    return None;
                }
                self.position += 1;
                let week = self.number(1, 1)? as u32;
                if self.peek() != Some(b'.') {
                    return None;
                }
                self.position += 1;
                let weekday = self.number(1, 1)? as u32;
                Some(PosixDate::MonthWeekDay {
                    month,
                    week,
                    weekday,
                })
            }
            byte if byte.is_ascii_digit() => Some(PosixDate::DayOfYear(self.number(1, 3)? as u32)),
            _ => None,
        }
    }
}

/// 尾串规则在指定 UTC 秒的偏移。
fn posix_offset_at(tz: &PosixTz, utc_seconds: i64) -> i32 {
    let Some(dst) = &tz.dst else {
        return tz.std_offset;
    };
    // 用标准时近似出当地年份；前后各取一年，覆盖南半球跨年的规则。
    let year = civil_from_days((utc_seconds + i64::from(tz.std_offset)).div_euclid(86_400)).0;
    let mut last: Option<(i64, bool)> = None;
    for candidate in [year - 1, year, year + 1] {
        let start = transition_utc(candidate, &dst.start, tz.std_offset);
        let end = transition_utc(candidate, &dst.end, dst.offset);
        for (instant, is_dst) in [(start, true), (end, false)] {
            if instant <= utc_seconds && last.is_none_or(|(best, _)| instant >= best) {
                last = Some((instant, is_dst));
            }
        }
    }
    match last {
        Some((_, true)) => dst.offset,
        _ => tz.std_offset,
    }
}

/// transition 的 UTC 秒：当地日期 + 规则时刻，减去切换前的偏移。
fn transition_utc(year: i64, transition: &PosixTransition, offset_before: i32) -> i64 {
    posix_date_days(year, transition.date) * 86_400 + i64::from(transition.time)
        - i64::from(offset_before)
}

/// 规则日期在 `year` 年里自 1970-01-01 起的天数。
fn posix_date_days(year: i64, date: PosixDate) -> i64 {
    let january_first = days_from_civil(year, 1, 1);
    match date {
        PosixDate::Julian(day) => {
            let leap_day = if is_leap_year(year) && day >= 60 {
                1
            } else {
                0
            };
            january_first + i64::from(day) - 1 + leap_day
        }
        PosixDate::DayOfYear(day) => january_first + i64::from(day),
        PosixDate::MonthWeekDay {
            month,
            week,
            weekday,
        } => {
            let first = days_from_civil(year, month, 1);
            // 1970-01-01 是周四（`0` = 周日）。
            let first_weekday = (first + 4).rem_euclid(7);
            let mut day =
                1 + (i64::from(weekday) - first_weekday).rem_euclid(7) + i64::from(week - 1) * 7;
            if day > i64::from(days_in_month(year, month)) {
                day -= 7;
            }
            first + day - 1
        }
    }
}

fn is_leap_year(year: i64) -> bool {
    (year % 4 == 0 && year % 100 != 0) || year % 400 == 0
}

fn days_in_month(year: i64, month: u32) -> u32 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        _ => {
            if is_leap_year(year) {
                29
            } else {
                28
            }
        }
    }
}

/// `days_from_civil`（Howard Hinnant）：`(年, 月, 日)` → 自 1970-01-01 起的天数。
fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let year_of_era = year - era * 400;
    let month_prime = i64::from(if month > 2 { month - 3 } else { month + 9 });
    let day_of_year = (153 * month_prime + 2) / 5 + i64::from(day) - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

// ---------------------------------------------------------------------------
// 工具
// ---------------------------------------------------------------------------

struct GetCurrentTimeExecute;

impl AgentToolExecute for GetCurrentTimeExecute {
    fn execute<'a>(
        &'a self,
        _tool_call_id: &'a str,
        params: JsonValue,
        _signal: Option<&'a tokio_util::sync::CancellationToken>,
        _on_update: Option<kiana_agent_core::types::AgentToolUpdateCallback>,
    ) -> BoxFuture<'a, Result<AgentToolResult<JsonValue>, AgentToolError>> {
        Box::pin(async move {
            let timezone = params
                .get("timezone")
                .and_then(JsonValue::as_str)
                .map(str::to_string);
            get_current_time(timezone.as_deref())
        })
    }
}

/// `getCurrentTimeTool`：`label: "Current Time"`、`name: "get_current_time"`、
/// `description: "Get the current date and time"`。
pub fn get_current_time_tool() -> AgentTool {
    AgentTool {
        name: "get_current_time".to_string(),
        label: "Current Time".to_string(),
        description: "Get the current date and time".to_string(),
        parameters: json!({
            "type": "object",
            "properties": {
                "timezone": {
                    "type": "string",
                    "description": "Optional timezone (e.g., 'America/New_York', 'Europe/London')"
                }
            }
        }),
        constrained_sampling: None,
        output_schema: None,
        prepare_arguments: None,
        execution_mode: None,
        replay: None,
        execute: std::sync::Arc::new(GetCurrentTimeExecute),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 2026-10-26T02:00:00.000Z。
    const INSTANT: i64 = 1_792_980_000_000;

    #[test]
    fn matches_the_typescript_golden_fixture() {
        // `tests/fixtures/get_current_time.json` 由 TS 参考实现（Node 26 / ICU 78）用
        // `toLocaleString("en-US", { timeZone, dateStyle: "full", timeStyle: "long" })`
        // 对 418 个 canonical zone + 别名 × 5 个时刻生成。
        let fixture: Vec<serde_json::Value> =
            serde_json::from_str(include_str!("../fixtures/get_current_time.json"))
                .expect("fixture");
        assert!(fixture.len() >= 2000, "fixture too small");
        for case in fixture {
            let utc_millis = case["utcMillis"].as_i64().expect("utcMillis");
            let timezone = case["timezone"].as_str().expect("timezone");
            let expected = case["text"].as_str().expect("text");
            assert_eq!(
                format_time_at(utc_millis, timezone).expect("valid timezone"),
                expected,
                "zone {timezone} at {utc_millis}"
            );
        }
    }

    #[test]
    fn extrapolates_posix_rules_after_the_last_transition() {
        // `get_current_time_future.json`：TS 参考实现（Node 26 / ICU 78）对同一批 498 个 id ×
        // 4 个时刻（2038、2040、2100 的冬/夏）生成。tzdata 的显式 transition 表止于 2037，
        // 之后的偏移来自 TZif 尾串（POSIX TZ 规则），不解析尾串会全部停在最后一个类型上。
        let fixture: Vec<serde_json::Value> =
            serde_json::from_str(include_str!("../fixtures/get_current_time_future.json"))
                .expect("fixture");
        assert!(fixture.len() >= 1900, "fixture too small");
        for case in fixture {
            let utc_millis = case["utcMillis"].as_i64().expect("utcMillis");
            let timezone = case["timezone"].as_str().expect("timezone");
            let expected = case["text"].as_str().expect("text");
            assert_eq!(
                format_time_at(utc_millis, timezone).expect("valid timezone"),
                expected,
                "zone {timezone} at {utc_millis}"
            );
        }
    }

    #[test]
    fn matches_icu_for_the_zones_that_are_not_in_the_old_fixture() {
        // 回归：CLDR 元区/UTC 别名的 34 个 id 曾与 ICU 不一致（America/Havana 等）。
        for (zone, expected) in [
            (
                "America/Havana",
                "Sunday, October 25, 2026 at 10:00:00 PM GMT-4",
            ),
            ("EST", "Sunday, October 25, 2026 at 9:00:00 PM EST"),
            ("HST", "Sunday, October 25, 2026 at 4:00:00 PM HST"),
            ("MST", "Sunday, October 25, 2026 at 7:00:00 PM MST"),
            ("WET", "Monday, October 26, 2026 at 2:00:00 AM GMT+0"),
            ("Portugal", "Monday, October 26, 2026 at 2:00:00 AM GMT+0"),
            (
                "Europe/Lisbon",
                "Monday, October 26, 2026 at 2:00:00 AM GMT+0",
            ),
            (
                "Atlantic/Azores",
                "Monday, October 26, 2026 at 1:00:00 AM GMT-1",
            ),
            (
                "Africa/Casablanca",
                "Monday, October 26, 2026 at 2:00:00 AM GMT+0",
            ),
            ("Etc/GMT+0", "Monday, October 26, 2026 at 2:00:00 AM GMT"),
            ("Etc/GMT-0", "Monday, October 26, 2026 at 2:00:00 AM GMT"),
            ("Etc/GMT0", "Monday, October 26, 2026 at 2:00:00 AM GMT"),
            ("Etc/UCT", "Monday, October 26, 2026 at 2:00:00 AM UTC"),
            ("Etc/GMT", "Monday, October 26, 2026 at 2:00:00 AM UTC"),
            (
                "America/Dawson",
                "Sunday, October 25, 2026 at 7:00:00 PM GMT-7",
            ),
            (
                "America/Whitehorse",
                "Sunday, October 25, 2026 at 7:00:00 PM GMT-7",
            ),
            (
                "America/Atka",
                "Sunday, October 25, 2026 at 5:00:00 PM HADT",
            ),
            (
                "America/Adak",
                "Sunday, October 25, 2026 at 5:00:00 PM HADT",
            ),
            (
                "Pacific/Honolulu",
                "Sunday, October 25, 2026 at 4:00:00 PM HST",
            ),
            (
                "America/Mazatlan",
                "Sunday, October 25, 2026 at 7:00:00 PM GMT-7",
            ),
            (
                "America/Hermosillo",
                "Sunday, October 25, 2026 at 7:00:00 PM GMT-7",
            ),
            ("Navajo", "Sunday, October 25, 2026 at 8:00:00 PM MDT"),
            ("Jamaica", "Sunday, October 25, 2026 at 9:00:00 PM EST"),
            (
                "Canada/Yukon",
                "Sunday, October 25, 2026 at 7:00:00 PM GMT-7",
            ),
            (
                "Mexico/BajaSur",
                "Sunday, October 25, 2026 at 7:00:00 PM GMT-7",
            ),
            ("US/Aleutian", "Sunday, October 25, 2026 at 5:00:00 PM HADT"),
        ] {
            assert_eq!(
                format_time_at(INSTANT, zone).expect("valid timezone"),
                expected,
                "zone {zone}"
            );
        }
    }

    #[test]
    fn accepts_icu_case_insensitive_and_alias_ids() {
        assert_eq!(
            format_time_at(INSTANT, "utc").unwrap(),
            "Monday, October 26, 2026 at 2:00:00 AM UTC"
        );
        assert_eq!(
            format_time_at(INSTANT, "asia/shanghai").unwrap(),
            "Monday, October 26, 2026 at 10:00:00 AM GMT+8"
        );
        assert_eq!(
            format_time_at(INSTANT, "US/Eastern").unwrap(),
            "Sunday, October 25, 2026 at 10:00:00 PM EDT"
        );
        assert_eq!(
            format_time_at(INSTANT, "gmt").unwrap(),
            "Monday, October 26, 2026 at 2:00:00 AM UTC"
        );
        assert_eq!(
            format_time_at(INSTANT, "Eire").unwrap(),
            "Monday, October 26, 2026 at 2:00:00 AM GMT"
        );
    }

    #[test]
    fn formats_fixed_instants_like_icu_en_us() {
        assert_eq!(
            format_time_at(INSTANT, "Asia/Shanghai").unwrap(),
            "Monday, October 26, 2026 at 10:00:00 AM GMT+8"
        );
        assert_eq!(
            format_time_at(INSTANT, "America/New_York").unwrap(),
            "Sunday, October 25, 2026 at 10:00:00 PM EDT"
        );
        assert_eq!(
            format_time_at(INSTANT, "UTC").unwrap(),
            "Monday, October 26, 2026 at 2:00:00 AM UTC"
        );
        assert_eq!(
            format_time_at(INSTANT, "Europe/London").unwrap(),
            "Monday, October 26, 2026 at 2:00:00 AM GMT"
        );
        assert_eq!(
            format_time_at(INSTANT, "Europe/Lisbon").unwrap(),
            "Monday, October 26, 2026 at 2:00:00 AM GMT+0"
        );
        assert_eq!(
            format_time_at(INSTANT, "Asia/Kolkata").unwrap(),
            "Monday, October 26, 2026 at 7:30:00 AM GMT+5:30"
        );
        assert_eq!(
            format_time_at(INSTANT, "Etc/GMT+5").unwrap(),
            "Sunday, October 25, 2026 at 9:00:00 PM GMT-5"
        );
    }

    #[test]
    fn rejects_invalid_timezones_with_the_verbatim_message() {
        for invalid in [
            "Not/AZone",
            "Factory",
            "posixrules",
            "localtime",
            "right/UTC",
            "posix/UTC",
            "../etc/passwd",
            "/etc/passwd",
        ] {
            let error = format_time_at(INSTANT, invalid).unwrap_err();
            assert_eq!(
                error,
                format!("Invalid timezone: {invalid}. Current UTC time: 2026-10-26T02:00:00.000Z"),
                "zone {invalid}"
            );
        }
    }

    #[test]
    fn iso_string_matches_javascript() {
        assert_eq!(iso_string(0), "1970-01-01T00:00:00.000Z");
        assert_eq!(iso_string(INSTANT), "2026-10-26T02:00:00.000Z");
        assert_eq!(iso_string(1_709_251_199_000), "2024-02-29T23:59:59.000Z");
        assert_eq!(iso_string(-1), "1969-12-31T23:59:59.999Z");
    }

    #[test]
    fn get_current_time_reports_details_and_non_empty_text() {
        let result = get_current_time(Some("Asia/Shanghai")).expect("valid timezone");
        assert_eq!(result.content.len(), 1);
        let timestamp = result.details["utcTimestamp"].as_i64().expect("timestamp");
        assert!((timestamp - now_unix_millis()).abs() < 5_000);
        let text = match &result.content[0] {
            kiana_ai::types::UserContentBlock::Text(text) => text.text.clone(),
            other => panic!("expected text block, got {other:?}"),
        };
        assert!(
            text.contains(" GMT+8") || text.contains(" UTC"),
            "unexpected text: {text}"
        );
    }

    #[test]
    fn local_branch_uses_the_runtime_timezone() {
        // 只断言形状（与具体机器时区无关）：`Weekday, Month D, YYYY at h:mm:ss AM/PM <zone>`。
        let text = format_local_time_at(INSTANT);
        assert!(text.contains(", 2026 at "), "unexpected text: {text}");
        let time = text.split(" at ").nth(1).expect("time part");
        let (clock, zone) = time.split_once(' ').expect("clock and zone");
        let mut clock_parts = clock.split(':');
        let hour: u32 = clock_parts.next().expect("hour").parse().expect("hour");
        assert!((1..=12).contains(&hour), "unexpected hour: {clock}");
        assert_eq!(
            clock_parts.next().map(str::len),
            Some(2),
            "minutes: {clock}"
        );
        assert_eq!(
            clock_parts.next().map(str::len),
            Some(2),
            "seconds: {clock}"
        );
        assert!(
            time.contains(" AM ") || time.contains(" PM "),
            "unexpected text: {text}"
        );
        assert!(!zone.is_empty(), "missing zone name: {text}");
    }

    #[test]
    fn local_branch_matches_node_for_every_zone_name_in_tzdata() {
        // `get_current_time_local.json`：TS 参考实现（Node 26 / ICU 78）对 tzdata 里全部
        // 599 个 TZif 名（含别名，`right/`、`posix/` 目录除外）× 2 个时刻，用 `TZ=<名字>`
        // 跑 `toLocaleString` 生成。除 `posixrules`（在 [`resolve_local_zone`] 里按 ICU
        // 的 tzdata 特判）外，本地时区分支与「显式 timeZone」分支对这些名字必须给出相同结果。
        let fixture: Vec<serde_json::Value> =
            serde_json::from_str(include_str!("../fixtures/get_current_time_local.json"))
                .expect("fixture");
        assert!(fixture.len() >= 1000, "fixture too small");
        for case in fixture {
            let utc_millis = case["utcMillis"].as_i64().expect("utcMillis");
            let timezone = case["timezone"].as_str().expect("timezone");
            let expected = case["text"].as_str().expect("text");
            assert_eq!(
                format_local_time_at_with(utc_millis, Some(timezone)),
                expected,
                "TZ={timezone} at {utc_millis}"
            );
        }
    }

    #[test]
    fn local_branch_matches_nodes_tz_semantics() {
        // TZ="" → Etc/Unknown（零偏移、`GMT+0`）。
        assert_eq!(
            format_local_time_at_with(INSTANT, Some("")),
            "Monday, October 26, 2026 at 2:00:00 AM GMT+0"
        );
        // TZ=<IANA id>（大小写敏感；小写 `utc` 是 Etc/Unknown）。
        assert_eq!(
            format_local_time_at_with(INSTANT, Some("Asia/Tokyo")),
            "Monday, October 26, 2026 at 11:00:00 AM GMT+9"
        );
        assert_eq!(
            format_local_time_at_with(INSTANT, Some("utc")),
            "Monday, October 26, 2026 at 2:00:00 AM GMT+0"
        );
        assert_eq!(
            format_local_time_at_with(INSTANT, Some(":Asia/Tokyo")),
            "Monday, October 26, 2026 at 11:00:00 AM GMT+9"
        );
        // `right/`、`posix/` 前缀被忽略。
        assert_eq!(
            format_local_time_at_with(INSTANT, Some("right/UTC")),
            "Monday, October 26, 2026 at 2:00:00 AM UTC"
        );
        // 带 DST 规则的 POSIX TZ 串回退到系统时区。
        assert_eq!(
            format_local_time_at_with(INSTANT, Some("EST5EDT,M3.2.0,M11.1.0")),
            format_local_time_at_with(INSTANT, None)
        );
        // `STDoffset` 形式的 POSIX TZ 串是固定偏移。
        assert_eq!(
            format_local_time_at_with(INSTANT, Some("EST5")),
            "Sunday, October 25, 2026 at 9:00:00 PM GMT-5"
        );
        assert_eq!(
            format_local_time_at_with(INSTANT, Some("UTC0")),
            "Monday, October 26, 2026 at 2:00:00 AM GMT+0"
        );
        // POSIX 名/符号/`<...>` 名、>24 小时钳制（Node 不写时区名，留一个尾随空格）。
        for (tz, expected) in [
            ("FOO5", "Sunday, October 25, 2026 at 9:00:00 PM GMT-5"),
            ("GMT+5", "Sunday, October 25, 2026 at 9:00:00 PM GMT-5"),
            ("ABC+5", "Sunday, October 25, 2026 at 9:00:00 PM GMT-5"),
            ("ABC-5", "Monday, October 26, 2026 at 7:00:00 AM GMT+5"),
            ("<ABC>5", "Sunday, October 25, 2026 at 9:00:00 PM GMT-5"),
            ("ABC24", "Sunday, October 25, 2026 at 2:00:00 AM "),
            ("ABC-24", "Tuesday, October 27, 2026 at 2:00:00 AM "),
        ] {
            assert_eq!(
                format_local_time_at_with(INSTANT, Some(tz)),
                expected,
                "TZ={tz}"
            );
        }
        // 无效 id（大小写不匹配、不存在的 zone、非文件的绝对路径、多余字符）→ `Etc/Unknown`，
        // 而不是系统时区。
        for tz in [
            "asia/shanghai",
            "ASIA/SHANGHAI",
            "Not/AZone",
            "Etc/Unknown",
            "Europe/Paris ",
            "Europe/Paris/Extra",
            "ABC",
            "A5",
            "ABC:5",
            "/tmp",
            "/nonexistent",
        ] {
            assert_eq!(
                format_local_time_at_with(INSTANT, Some(tz)),
                "Monday, October 26, 2026 at 2:00:00 AM GMT+0",
                "TZ={tz}"
            );
        }
        // 绝对路径：可读的 TZif 文件生效，但只用最后一个标准时类型（不跟随 DST）。
        for (path, expected) in [
            (
                "/usr/share/zoneinfo/Asia/Tokyo",
                "Monday, October 26, 2026 at 11:00:00 AM GMT+9",
            ),
            (
                "/usr/share/zoneinfo/America/New_York",
                "Sunday, October 25, 2026 at 9:00:00 PM GMT-5",
            ),
        ] {
            assert_eq!(
                format_local_time_at_with(INSTANT, Some(path)),
                expected,
                "TZ={path}"
            );
        }
        // ICU 的 tzdata 把 `posixrules` 当作只有标准时的 EST。
        assert_eq!(
            format_local_time_at_with(INSTANT, Some("posixrules")),
            "Sunday, October 25, 2026 at 9:00:00 PM GMT-5"
        );
        // 名字前缀像 POSIX TZ 但 ICU 不支持的形式 → 系统时区。
        for tz in ["ABC5:30", "ABC5x", "ABC123", "EST5EDT4,M3.2.0,M11.1.0"] {
            assert_eq!(
                format_local_time_at_with(INSTANT, Some(tz)),
                format_local_time_at_with(INSTANT, None),
                "TZ={tz}"
            );
        }
        // ICU 认识的别名/id 走 CLDR 短名。
        assert_eq!(
            format_local_time_at_with(INSTANT, Some("CET")),
            "Monday, October 26, 2026 at 3:00:00 AM GMT+1"
        );
        assert_eq!(
            format_local_time_at_with(INSTANT, Some("EST5EDT")),
            "Sunday, October 25, 2026 at 10:00:00 PM GMT-4"
        );
    }

    #[test]
    fn schema_and_labels_match_the_typescript_tool() {
        let tool = get_current_time_tool();
        assert_eq!(tool.name, "get_current_time");
        assert_eq!(tool.label, "Current Time");
        assert_eq!(tool.description, "Get the current date and time");
        // TypeBox 的 `JSON.stringify(Type.Object({ timezone: Type.Optional(...) }))`：
        // 可选属性不产生 `required`。
        assert_eq!(
            serde_json::to_string(&tool.parameters).expect("serialize"),
            r#"{"type":"object","properties":{"timezone":{"type":"string","description":"Optional timezone (e.g., 'America/New_York', 'Europe/London')"}}}"#
        );
        assert_eq!(
            tool.parameters,
            json!({
                "type": "object",
                "properties": {
                    "timezone": {
                        "type": "string",
                        "description": "Optional timezone (e.g., 'America/New_York', 'Europe/London')"
                    }
                }
            })
        );
    }
}
