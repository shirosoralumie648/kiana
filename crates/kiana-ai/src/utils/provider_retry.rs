//! `utils/provider-retry.ts`——复刻 OpenAI / Anthropic SDK 的重试策略，但退避可中断。
//!
//! TS: `packages/ai/src/utils/provider-retry.ts`。
//!
//! SDK 自带的重试定时器不理会请求的 `AbortSignal`，因此调用方要用 `maxRetries: 0` 调 SDK，
//! 再用本模块包一层。provider 要求的延迟超过 `maxRetryDelayMs`（默认 60 秒）时**立即失败**；
//! 设为 0 可关闭该上限。
//!
//! 与 TS 的对应关系：
//! - `ProviderError`（`status` + `headers`）→ [`ProviderError`]；
//! - `Headers.get(...)` → 交给调用方的 [`HeaderLookup`] 闭包（Rust 侧 provider 的 HTTP 头表示
//!   各不相同，这里只借用“按键取值”这一层）；
//! - `Date.parse(retryAfter)` → [`parse_retry_after_date`]，按 V8 的 ES5 ISO + legacy 解析器完整移植。
//!
//! # 这个文件的两半
//!
//! 1. 上半：真正的重试策略（[`retry_provider_request`] 及其依赖）；
//! 2. 下半：V8 `Date.parse` 的逐函数移植，只服务 `retry-after` 响应头里的 HTTP 日期
//!    （TS 直接调用运行时自带的 `Date.parse`，Rust 标准库没有等价物）。
//!
//! 两半的失败语义不同：重试策略用 `Result` / `AiError`，日期解析用 `f64` 的 `NaN`（与 JS 一致）。
//!
//! # 被谁调用
//!
//! - `api/anthropic_messages.rs`、`api/openai_responses_shared.rs`、`api/classifier_shared.rs`
//!   用 [`retry_provider_request`] 包裹各自的 HTTP 尝试；
//! - `api/google_shared.rs` 只用 [`is_retryable_provider_error`] 自行决定是否重试；
//! - `api/openai_codex_responses.rs` 用 [`parse_retry_after_date`] 解析 `retry-after`。
//!
//! # 一次重试的执行流程
//!
//! 1. 调 `request()` 发一次请求；成功立即返回。
//! 2. 失败后先检查调用方的 signal：已中止就返回 `AbortError`（不让服务端错误掩盖取消）。
//! 3. 重试预算为 0、错误不可重试、或状态码在 `no_retry_statuses` 里 → 返回错误。
//! 4. 计算退避：`retry-after-ms` → `retry-after`（秒或 HTTP 日期）→ 指数退避 + 抖动。
//!    服务端要求的延迟超过 `max_retry_delay_ms` 时**不等待**，直接失败。
//! 5. 可中断地睡完退避，回到第 1 步。每次重试都是一次全新的 `request()` 调用。
//!
//! # 注意事项
//!
//! - 本模块**不设超时**；每次尝试的超时由调用方的 HTTP 层负责。
//! - 抖动来自系统随机源（`getrandom`），同一次失败的延迟不可复现；测试只校验区间。
//! - 日期解析中，不带时区的日期时间（ISO 的 `1994-11-06T08:49:37` 与 legacy 写法一样）
//!   按进程本地时区解释，因此同一字符串在不同机器上结果可能不同；但 ISO 纯日期形式
//!   （如 `1994-11-06`、`1994-06`）按 ES5 规则视为 UTC（`parse_es5_date_time` 阶段 6
//!   补 0 偏移），不随机器变化（这正是 Node 的行为；测试用固定偏移的闭包绕开它）。
//! - `retry-after` 的日期解析失败会得到 `NaN`，随后**回落到指数退避**，不会让请求失败。

use crate::utils::abort::AbortScope;
use std::time::{SystemTime, UNIX_EPOCH};

/// `DEFAULT_MAX_RETRY_DELAY_MS`：`max_retry_delay_ms` 缺省时的服务端延迟上限（毫秒）。
///
/// 它只约束服务端通过 `retry-after-ms` / `retry-after` 指定的延迟；指数退避的基值最大
/// 8 秒，不会触及这个上限。把 `max_retry_delay_ms` 设为 `Some(0)` 可关闭上限。
pub const DEFAULT_MAX_RETRY_DELAY_MS: u64 = 60_000;

/// provider SDK 错误（TS 的 `ProviderError`）：HTTP 状态 + 响应头 + 消息。
///
/// 调用方在每次尝试失败时构造它；[`retry_provider_request`] 只依赖这三个字段做重试判定，
/// 因此各 provider 不必把自家的完整错误类型带进重试层。
#[derive(Clone, Default)]
pub struct ProviderError {
    /// TS 的 `error.message`；重试耗尽后原样成为返回的 `AiError::message`，也是延迟超限
    /// 错误文本的结尾。
    pub message: String,
    /// HTTP 状态码；`None` 表示没有拿到响应（连接/传输层失败），按策略视为可重试。
    pub status: Option<u16>,
    /// 响应头取值（`Headers.get` 的小写键语义）。
    ///
    /// 本模块只会用小写字面量（`x-should-retry`、`retry-after-ms`、`retry-after`）调用闭包；
    /// 闭包自己负责大小写不敏感的匹配（调用方用 `eq_ignore_ascii_case` 或等价的大小写归一化，
    /// 如统一转小写）。
    pub header: HeaderLookup,
}

/// 响应头取值回调；`None` 表示该头不存在。
///
/// 用 `Arc<dyn Fn>` 而不是具体的头容器，是因为各 provider 的响应头表示不同，重试层只需要
/// “按键取值”这一层抽象。`Arc` 让 `ProviderError` 的 `Clone` 不复制底层数据；`Send + Sync`
/// 是该错误能随异步任务跨线程传递的前提。
pub type HeaderLookup = Option<std::sync::Arc<dyn Fn(&str) -> Option<String> + Send + Sync>>;

impl std::fmt::Debug for ProviderError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProviderError")
            .field("message", &self.message)
            .field("status", &self.status)
            .field("header", &self.header.is_some())
            .finish()
    }
}

impl ProviderError {
    fn header(&self, name: &str) -> Option<String> {
        self.header.as_ref().and_then(|lookup| lookup(name))
    }
}

/// `ProviderRetryOptions`：重试次数、服务端延迟上限、取消信号与状态码黑名单。
///
/// 所有字段都可省略；`Default::default()` 对应“全部省略”——只请求一次、上限 60 秒、
/// 不设置取消信号、没有状态码黑名单。
#[derive(Debug, Clone, Default)]
pub struct ProviderRetryOptions {
    /// **额外**尝试次数（不含第一次请求）；`None` 按 0 处理，即只请求一次。
    pub max_retries: Option<u64>,
    /// 服务端要求的延迟上限（毫秒）；`None` 用 [`DEFAULT_MAX_RETRY_DELAY_MS`]，`Some(0)` 关闭上限。
    ///
    /// 只检查 `retry-after-ms` / `retry-after` 给出的值，不检查指数退避。
    pub max_retry_delay_ms: Option<u64>,
    /// 中止信号；在每次决定是否重试之前、以及退避睡眠期间都会被检查。
    pub signal: Option<AbortScope>,
    /// 默认策略会重试、但这里要求立即失败的 HTTP 状态。
    ///
    /// 判定发生在 [`is_retryable_provider_error`] 之后；没有 status（`None`）的错误不受影响。
    pub no_retry_statuses: Vec<u16>,
}

/// `isRetryableProviderError(error)`：镜像 OpenAI/Anthropic SDK 的策略。
///
/// 判定顺序（靠前的规则优先）：
/// 1. 响应头 `x-should-retry` 精确等于 `"true"` / `"false"`（区分大小写）时直接决定
///    （等价于 typed `isRetryable`）；
/// 2. `status` 为 `None`（连接/传输层失败）→ 可重试；
/// 3. 否则使用请求策略的 [`crate::request_policy::RETRYABLE_STATUS_CODES`] 集合
///    （含 425/529，与 `request_policy::is_retryable_provider_error` 保持单一事实来源）。
///
/// `no_retry_statuses` **不在**这里判断：[`retry_provider_request`] 在本函数返回 `true`
/// 之后才应用它。
pub fn is_retryable_provider_error(error: &ProviderError) -> bool {
    let header_retry = match error.header("x-should-retry").as_deref() {
        Some("true") => Some(true),
        Some("false") => Some(false),
        _ => None,
    };
    // 把 HTTP 头当作 typed `isRetryable`，与 request_policy 的分类/重试口径一致。
    let typed = crate::request_policy::TypedProviderError {
        status: error.status,
        message: Some(error.message.as_str()),
        is_retryable: header_retry,
        ..crate::request_policy::TypedProviderError::default()
    };
    match error.status {
        // 传输层失败没有状态码：保持“可重试”的既有语义。
        None => header_retry.unwrap_or(true),
        Some(_) => crate::request_policy::is_retryable_provider_error(
            Some(&typed),
            &serde_json::json!({ "message": error.message }),
        ),
    }
}

/// JS 的 `Math.ceil(delayMs / 1000)`，按 JS 的 Number→String 规则格式化。
///
/// 这个字符串会逐字拼进延迟超限的错误消息，所以不能直接用 Rust 的整数格式化：输入是
/// `f64`（例如 `60000.5`），先除后向上取整得到 61，格式化规则也必须与 JS 相同。
fn ceil_seconds(ms: f64) -> String {
    kiana_json::js_num::format_js_number((ms / 1000.0).ceil())
}

/// `validateServerRetryDelayMs(delayMs, maxRetryDelayMs, providerErrorMessage)`。
///
/// 返回原延迟（毫秒）；超过上限时用 `Err(消息)` 表示——调用方会把消息原样变成 `AiError`。
/// 上限为 0 表示关闭检查（不是“最多等 0 毫秒”）。
fn validate_server_retry_delay_ms(
    delay_ms: f64,
    max_retry_delay_ms: Option<u64>,
    provider_error_message: &str,
) -> Result<f64, String> {
    let max_delay_ms = max_retry_delay_ms.unwrap_or(DEFAULT_MAX_RETRY_DELAY_MS);
    // 严格大于才拒绝：恰好等于上限的延迟可以等。错误文本是当前稳定契约，改动需同步测试。
    if max_delay_ms > 0 && delay_ms > max_delay_ms as f64 {
        return Err(format!(
            "Server requested {}s retry delay (max: {}s). {}",
            ceil_seconds(delay_ms),
            ceil_seconds(max_delay_ms as f64),
            provider_error_message
        ));
    }
    Ok(delay_ms)
}

/// `Math.random()` 的抖动。
///
/// 本文件没有注入随机源的入口；测试只断言结果落在 `[0.75, 1.0] * base` 区间内，
/// 不要求可复现（与 TS 的 `Math.random()` 一致）。
fn default_jitter() -> f64 {
    // 与 JS 的 `Math.random()` 同分布（[0, 1)）；不要求可复现。
    let mut bytes = [0u8; 8];
    if getrandom::fill(&mut bytes).is_err() {
        return 0.5;
    }
    // 取高 53 位再除以 2^53：double 的尾数正好 53 位，因此结果均匀落在 [0, 1) 且
    // 不会因整数转浮点产生额外舍入。系统随机源不可用时退化为 0.5（延迟取区间中点）。
    let value = u64::from_le_bytes(bytes) >> 11;
    value as f64 / (1u64 << 53) as f64
}

/// JS `\s` 的字符集合（`Number.parseFloat` 的前导修剪）。
///
/// 与下方日期扫描器的 [`is_v8_white_space`] 不是同一个集合：这里包含换行符
/// （`\n`、`\r`、U+2028、U+2029），因为 `parseFloat` 允许跨行前导空白。
fn is_js_whitespace(ch: char) -> bool {
    matches!(
        ch,
        '\t' | '\n' | '\u{000b}' | '\u{000c}' | '\r' | ' ' | '\u{00a0}' | '\u{1680}' | '\u{2000}'
            ..='\u{200a}'
                | '\u{2028}'
                | '\u{2029}'
                | '\u{202f}'
                | '\u{205f}'
                | '\u{3000}'
                | '\u{feff}'
    )
}

/// JS 的 `Number.parseFloat`：取最长合法前缀，`NaN` 表示没有。
///
/// 不能直接用 Rust 的 `str::parse::<f64>()`：`parseFloat` 允许尾随垃圾（`"12abc"` → 12）、
/// 允许不完整的小数/指数（`"5."` → 5、`"1e"` → 1）、`"0x10"` 只取 `0`，并且只认区分大小写
/// 的 `Infinity`。这些差异会让 `retry-after` 的判定分叉，所以逐字符扫描出前缀 `[..end]`
/// 后仍交给标准库做十进制转换。
fn parse_float(text: &str) -> f64 {
    // 阶段 1：按 JS 的空白集合修剪前导空白（尾随部分由“最长前缀”自然丢弃）。
    let text = text.trim_start_matches(is_js_whitespace);
    let bytes = text.as_bytes();
    let len = bytes.len();
    let mut index = 0usize;
    let mut negative = false;
    // 阶段 2：可选符号。它属于最终前缀，因此 `text[..end]` 会把符号一起交给解析器。
    if index < len && (bytes[index] == b'+' || bytes[index] == b'-') {
        negative = bytes[index] == b'-';
        index += 1;
    }
    // `Infinity` 字面量（区分大小写）。
    if text[index..].starts_with("Infinity") {
        return if negative {
            f64::NEG_INFINITY
        } else {
            f64::INFINITY
        };
    }
    // 阶段 3：整数部分。`end` 记录“最长合法尾数”的结束位置，0 表示还没有任何合法尾数。
    let integer_start = index;
    while index < len && bytes[index].is_ascii_digit() {
        index += 1;
    }
    let has_integer = index > integer_start;
    let mut end = 0usize;
    // 阶段 4：小数部分。仅当点前有数字（`5.`）或点后至少一位（`.5`）时尾数成立；
    // 孤立的 `.` 不是数字。
    if index < len && bytes[index] == b'.' {
        index += 1;
        let fraction_start = index;
        while index < len && bytes[index].is_ascii_digit() {
            index += 1;
        }
        if has_integer || index > fraction_start {
            end = index;
        }
    } else if has_integer {
        end = index;
    }
    // 阶段 5：指数。只有尾数已经成立时才看 `e`/`E`；指数部分必须至少有一位数字，
    // 否则指数整体不被消费（`"1e"` 仍返回 1，而不是失败）。
    if end != 0 && index < len && (bytes[index] == b'e' || bytes[index] == b'E') {
        let mut cursor = index + 1;
        if cursor < len && (bytes[cursor] == b'+' || bytes[cursor] == b'-') {
            cursor += 1;
        }
        let exponent_start = cursor;
        while cursor < len && bytes[cursor].is_ascii_digit() {
            cursor += 1;
        }
        if cursor > exponent_start {
            end = cursor;
        }
    }
    // 阶段 6：`end` 只会停在 ASCII 字节之后，因此 `text[..end]` 一定是合法的 UTF-8 边界；
    // 标准库解析失败（理论上不该发生）时返回 NaN，与 JS 的“解析不了”同义。
    if end == 0 {
        return f64::NAN;
    }
    text[..end].parse::<f64>().unwrap_or(f64::NAN)
}

// ---------------------------------------------------------------------------
// JS `Date.parse`（V8 `DateParser` 的忠实移植）
// ---------------------------------------------------------------------------
//
// TS 用的是 `Date.parse(retryAfter)`。ES 规范只规定 ISO 8601；其余格式由实现决定，
// 因此这里按 V8（Node 运行时）的 `src/date/dateparser.{h,cc,cc-inl}` 逐函数移植：
// 先尝试 ES5 ISO 前缀，剩余部分交给 legacy 解析器（月份名、RFC 850、asctime 等）。
// 没有时区的文本按本地时间解释——本地偏移取自 `chrono::Local`（等价于 V8 的
// `DateCache::LocalOffsetInMs`）。
//
// 解析分四个阶段，与 V8 的类一一对应：
// 1. 词法：`InputReader` + `DateStringTokenizer` 把文本切成 `DateToken`（数字、符号、关键字、
//    空白、未知词、结束）；位置按 UTF-16 码元计，因此代理对会占两个位置。
// 2. ES5 ISO 前缀：`parse_es5_date_time` 尝试 `YYYY-MM-DDTHH:MM:SS.sssZ` 及其变体。
//    只有“看起来不是 ISO”时才把控制权交回 legacy 解析器；一旦确认是 ISO 但语法有错，直接
//    返回 `invalid`（例如 `1994-11-06T8:49:37Z` 不会退化成 legacy 解析）。
// 3. legacy 循环：`v8_date_parse` 按 token 类型填充三个 composer（日期、时间、时区），
//    支持 RFC 850 / asctime / 月份名 / `GMT+8` 等写法。
// 4. 组装：三个 composer 的 `write` 消歧后，经 ES 的 `MakeDay` / `MakeTime` 得到毫秒数，
//    再减去显式时区偏移或本地偏移，最后检查 `Date` 的取值范围（±8.64e15 ms）。
//
// 这些函数的 `None` 统一表示“这个字符串不是合法日期”，由 `parse_retry_after_date` 转成 NaN；
// 判定顺序与 V8 源码逐条对应，改动任何一步都可能让 fixture 里的基准值分叉。

use chrono::{Local, TimeZone};

/// V8 的 `kNone` 哨兵（`kMaxInt`）。
///
/// 各 composer 用它表示“这个分量还没被赋值”，以便区分“未赋值”与“赋值成了 0”。
const V8_NONE: i32 = i32::MAX;

/// V8 指针压缩下的 `Smi::kMaxValue`（时区偏移的秒数上限）。
const V8_SMI_MAX: i64 = (1 << 30) - 1;

/// `DateCache::kMaxTimeInMs`：JS `Date` 可表示的最大毫秒数（±100,000,000 天）。
const K_MAX_TIME_IN_MS: f64 = 8_640_000_000_000_000.0;

/// `DateCache::kMsPerMonth`：按 30 天计的月份长度，仅用于本地时区换算的容差窗口。
const K_MS_PER_MONTH: f64 = 2_592_000_000.0;

/// 关键字类别。判别值被直接存进 `DateToken::tag`，因此枚举顺序不能改。
#[derive(Clone, Copy, PartialEq, Eq)]
enum KeywordType {
    Invalid,
    MonthName,
    TimeZoneName,
    TimeSeparator,
    AmPm,
}

/// `KeywordTable::array`：前三个字符（小写）→ 类型/值。
///
/// 每一行的五列是 `(c0, c1, c2, kind, value)`：前三个字符不足三字母时用 0 填充
/// （`am`、`pm`、`t`、`z` 就是这种短关键字）。`value` 的含义随 `kind` 变化：
/// 月份名是 1..12，AM/PM 是小时的偏移（0 / 12），时区名是相对 UTC 的小时数。
/// 表顺序沿袭 V8，便于与上游逐条对照；当前条目没有两条共享相同的前三字符，
/// `matched == 3` 至多命中一条，所以重排不会改变任何输入的查找结果。
/// 新增条目时仍应与 V8 保持一致。
const KEYWORD_TABLE: &[(u8, u8, u8, KeywordType, i32)] = &[
    (b'j', b'a', b'n', KeywordType::MonthName, 1),
    (b'f', b'e', b'b', KeywordType::MonthName, 2),
    (b'm', b'a', b'r', KeywordType::MonthName, 3),
    (b'a', b'p', b'r', KeywordType::MonthName, 4),
    (b'm', b'a', b'y', KeywordType::MonthName, 5),
    (b'j', b'u', b'n', KeywordType::MonthName, 6),
    (b'j', b'u', b'l', KeywordType::MonthName, 7),
    (b'a', b'u', b'g', KeywordType::MonthName, 8),
    (b's', b'e', b'p', KeywordType::MonthName, 9),
    (b'o', b'c', b't', KeywordType::MonthName, 10),
    (b'n', b'o', b'v', KeywordType::MonthName, 11),
    (b'd', b'e', b'c', KeywordType::MonthName, 12),
    (b'a', b'm', 0, KeywordType::AmPm, 0),
    (b'p', b'm', 0, KeywordType::AmPm, 12),
    (b'u', b't', 0, KeywordType::TimeZoneName, 0),
    (b'u', b't', b'c', KeywordType::TimeZoneName, 0),
    (b'z', 0, 0, KeywordType::TimeZoneName, 0),
    (b'g', b'm', b't', KeywordType::TimeZoneName, 0),
    (b'c', b'd', b't', KeywordType::TimeZoneName, -5),
    (b'c', b's', b't', KeywordType::TimeZoneName, -6),
    (b'e', b'd', b't', KeywordType::TimeZoneName, -4),
    (b'e', b's', b't', KeywordType::TimeZoneName, -5),
    (b'm', b'd', b't', KeywordType::TimeZoneName, -6),
    (b'm', b's', b't', KeywordType::TimeZoneName, -7),
    (b'p', b'd', b't', KeywordType::TimeZoneName, -7),
    (b'p', b's', b't', KeywordType::TimeZoneName, -8),
    (b't', 0, 0, KeywordType::TimeSeparator, 0),
];

/// `KeywordTable::Lookup`。
///
/// `prefix` 是词的前三个 UTF-16 码元（不足三位已由 `read_word` 补 0），`len` 是完整词长。
/// 匹配满三位才命中，且要求 `len <= 3` 或该关键字是月份名：V8 允许完整月份名（`march`、
/// `september`）按前三个字母匹配，但 `gmtx` 这类更长的词不会被当成 `gmt`。
fn keyword_lookup(prefix: &[u32; 3], len: usize) -> (KeywordType, i32) {
    for &(c0, c1, c2, kind, value) in KEYWORD_TABLE {
        let entry = [u32::from(c0), u32::from(c1), u32::from(c2)];
        let mut matched = 0;
        while matched < 3 && prefix[matched] == entry[matched] {
            matched += 1;
        }
        // 比关键字更长的词只允许是月份名。
        if matched == 3 && (len <= 3 || kind == KeywordType::MonthName) {
            return (kind, value);
        }
    }
    (KeywordType::Invalid, 0)
}

// `DateToken` 的 tag 编码：负数是特殊 token，非负数直接存 `KeywordType` 的判别值
// （所以 `is_keyword` 只需判断 `tag >= 0`，`keyword_type` 再把 1..4 映射回去）。
const TAG_INVALID: i32 = -6;
const TAG_UNKNOWN: i32 = -5;
const TAG_WHITESPACE: i32 = -4;
const TAG_NUMBER: i32 = -3;
const TAG_SYMBOL: i32 = -2;
const TAG_END: i32 = -1;

/// V8 的 `DateToken`：词法单元。
///
/// `length` 按 UTF-16 码元计（用于 `read_milliseconds` 的位数判断和数字定长检查）；
/// `value` 对数字是数值、对符号是 ASCII 码、对关键字是关键字表里的值，其余为 -1。
#[derive(Clone, Copy)]
struct DateToken {
    tag: i32,
    length: usize,
    value: i32,
}

impl DateToken {
    fn invalid() -> Self {
        Self {
            tag: TAG_INVALID,
            length: 0,
            value: -1,
        }
    }

    fn unknown() -> Self {
        Self {
            tag: TAG_UNKNOWN,
            length: 1,
            value: -1,
        }
    }

    fn white_space(length: usize) -> Self {
        Self {
            tag: TAG_WHITESPACE,
            length,
            value: -1,
        }
    }

    fn number(value: i32, length: usize) -> Self {
        Self {
            tag: TAG_NUMBER,
            length,
            value,
        }
    }

    fn symbol(symbol: u8) -> Self {
        Self {
            tag: TAG_SYMBOL,
            length: 1,
            value: i32::from(symbol),
        }
    }

    fn keyword(kind: KeywordType, value: i32, length: usize) -> Self {
        Self {
            tag: kind as i32,
            length,
            value,
        }
    }

    fn end_of_input() -> Self {
        Self {
            tag: TAG_END,
            length: 0,
            value: -1,
        }
    }

    fn is_invalid(self) -> bool {
        self.tag == TAG_INVALID
    }

    fn is_end_of_input(self) -> bool {
        self.tag == TAG_END
    }

    fn is_number(self) -> bool {
        self.tag == TAG_NUMBER
    }

    fn is_symbol(self, symbol: u8) -> bool {
        self.tag == TAG_SYMBOL && self.value == i32::from(symbol)
    }

    fn is_whitespace(self) -> bool {
        self.tag == TAG_WHITESPACE
    }

    /// V8 的 `IsKeyword`：`tag >= 0`。
    ///
    /// 未识别的词也由 `keyword()` 构造，其 tag 是 `KeywordType::Invalid` 的判别值 0，
    /// 因此同样算 keyword；`keyword_type` 随后返回 `Invalid`，由 legacy 循环按“垃圾词”
    /// 的规则处理（见 `v8_date_parse` 的关键字分支）。
    fn is_keyword(self) -> bool {
        self.tag >= 0
    }

    fn keyword_type(self) -> KeywordType {
        match self.tag {
            1 => KeywordType::MonthName,
            2 => KeywordType::TimeZoneName,
            3 => KeywordType::TimeSeparator,
            4 => KeywordType::AmPm,
            _ => KeywordType::Invalid,
        }
    }

    fn keyword_value(self) -> i32 {
        self.value
    }

    fn number_value(self) -> i32 {
        self.value
    }

    fn length(self) -> usize {
        self.length
    }

    fn is_fixed_length_number(self, length: usize) -> bool {
        self.is_number() && self.length == length
    }

    fn is_ascii_sign(self) -> bool {
        self.tag == TAG_SYMBOL && (self.value == i32::from(b'-') || self.value == i32::from(b'+'))
    }

    /// V8: `44 - value`（'+' = 43 → 1，'-' = 45 → -1）。
    fn ascii_sign(self) -> i32 {
        44 - self.value
    }

    fn is_keyword_z(self) -> bool {
        self.keyword_type() == KeywordType::TimeZoneName && self.length == 1 && self.value == 0
    }
}

/// V8 的 `InputReader`：按 UTF-16 码元前进，`0` 是输入结束哨兵。
///
/// 用 UTF-16 码元而不是 Rust 的 `char`，因为 JS 的字符串索引以码元为单位，代理对必须
/// 占两个位置，才能与 V8 的 token 长度、`Date.parse` 的边界判定对齐。
/// 代价是 `ch == 0` 同时充当结束哨兵：文本中字面出现的 U+0000 会让解析在此处提前结束。
struct InputReader {
    units: Vec<u32>,
    /// 已消费的码元数，也等于“当前字符之后的位置”（从 1 开始计数）。
    index: usize,
    ch: u32,
}

impl InputReader {
    /// 读入第一个码元（`new` 之后 `ch` 就是首字符；空串时为结束哨兵）。
    fn new(units: Vec<u32>) -> Self {
        let mut reader = Self {
            units,
            index: 0,
            ch: 0,
        };
        reader.next();
        reader
    }

    /// 前进一个码元；越过末尾后始终保持 `0`。
    fn next(&mut self) {
        self.ch = self.units.get(self.index).copied().unwrap_or(0);
        self.index += 1;
    }

    /// 当前位置（已消费的码元数）。token 长度用前后两次 `position()` 相减得到。
    fn position(&self) -> usize {
        self.index
    }

    /// `ReadUnsignedNumeral`：跳过前导零，累加最多 9 位有效数字。
    ///
    /// 超过 9 位的数字仍会被消费，但值只保留前 9 位有效数字（避免 i32 溢出）；
    /// 长度由调用方按消费的码元数单独计算，因此 `000123` 的 value 是 123、length 是 6。
    fn read_unsigned_numeral(&mut self) -> i32 {
        let mut number = 0i32;
        let mut digits = 0;
        while self.ch == u32::from(b'0') {
            self.next();
        }
        while self.is_ascii_digit() {
            if digits < 9 {
                number = number * 10 + (self.ch - u32::from(b'0')) as i32;
            }
            digits += 1;
            self.next();
        }
        number
    }

    /// `ReadWord`：读取 `>= 'A'` 且非空白的字符，前三个字符转小写。
    ///
    /// 判断条件是码元值而非“字母”：`[`、`{` 以及非 ASCII 字符也会被吞进词里（数字与
    /// `:`、`-` 等符号则因码元值小于 `'A'` 而截断词）。前三个码元按位或 `0x20` 得到小写
    /// （ASCII 字母的大小写位），不足三位时
    /// 把剩余槽位清零——`keyword_lookup` 依赖这个补零来匹配 `t`、`z`、`am` 等短关键字。
    fn read_word(&mut self, prefix: &mut [u32; 3]) -> usize {
        let mut len = 0usize;
        while self.ch >= u32::from(b'A') && !is_v8_white_space(self.ch) {
            if len < 3 {
                prefix[len] = self.ch | 0x20;
            }
            self.next();
            len += 1;
        }
        for slot in prefix.iter_mut().skip(len.min(3)) {
            *slot = 0;
        }
        len
    }

    fn skip(&mut self, symbol: u8) -> bool {
        if self.ch == u32::from(symbol) {
            self.next();
            true
        } else {
            false
        }
    }

    /// 消耗一个空白或换行符（V8 的 `SkipWhiteSpace` 一次只吃一个）。
    fn skip_white_space(&mut self) -> bool {
        if is_v8_white_space_or_line_terminator(self.ch) {
            self.next();
            true
        } else {
            false
        }
    }

    /// 消耗一个配对的 `(...)` 注释块，例如 `Mon (comment) 06 Nov`。
    ///
    /// 从 `(` 开始按括号深度扫描，直到深度回到 0；未闭合时会吃到输入末尾才返回。
    /// 调用方只把它当作一个“未知词”，因此注释内容不参与日期解析。
    fn skip_parentheses(&mut self) -> bool {
        if self.ch != u32::from(b'(') {
            return false;
        }
        let mut balance = 0i32;
        loop {
            if self.ch == u32::from(b')') {
                balance -= 1;
            } else if self.ch == u32::from(b'(') {
                balance += 1;
            }
            self.next();
            if !(balance > 0 && self.ch != 0) {
                break;
            }
        }
        true
    }

    fn is_end(&self) -> bool {
        self.ch == 0
    }

    fn is_ascii_digit(&self) -> bool {
        (u32::from(b'0')..=u32::from(b'9')).contains(&self.ch)
    }
}

/// `IsWhiteSpace`：Zs 类 + `\t\v\f` + U+FEFF（不含换行符）。
///
/// 与 [`is_js_whitespace`] 的差别在于这里**不含**换行符：V8 的词法把行终止符归到
/// `IsWhiteSpaceOrLineTerminator`，而 `ReadWord` 只按 `IsWhiteSpace` 截断词。
fn is_v8_white_space(ch: u32) -> bool {
    matches!(
        ch,
        0x0009 | 0x000b | 0x000c | 0x0020 | 0x00a0 | 0x1680 | 0x2000
            ..=0x200a | 0x202f | 0x205f | 0x3000 | 0xfeff
    )
}

/// `IsWhiteSpaceOrLineTerminator`。
fn is_v8_white_space_or_line_terminator(ch: u32) -> bool {
    is_v8_white_space(ch) || matches!(ch, 0x000a | 0x000d | 0x2028 | 0x2029)
}

/// `DateParser::Scan`：从当前位置切出一个 token。
///
/// 判定顺序固定：数字 → 单字符符号（`: - + . )`）→ 以 `>= 'A'` 开头的词 → 空白 →
/// 括号注释 → 其他单字符。顺序即优先级：`)` 先按符号处理，只有 `(` 才进入注释扫描；
/// 字母开头的词即使查不到关键字也返回 `Invalid` 关键字 token（而非 unknown），
/// 由 legacy 循环判断它是不是垃圾词。
fn scan(reader: &mut InputReader) -> DateToken {
    let pre_pos = reader.position();
    // EOF 必须在所有分支之前判定：末尾的 `skip` 都会失败，否则会一路落到 unknown。
    if reader.is_end() {
        return DateToken::end_of_input();
    }
    // 数字 token 的 length 按实际消费的码元数计（含前导零），与 value 的 9 位截断互相独立。
    if reader.is_ascii_digit() {
        let number = reader.read_unsigned_numeral();
        let length = reader.position() - pre_pos;
        return DateToken::number(number, length);
    }
    // 符号只认这五个；`-` 既用于日期分隔，也用于时区偏移；`+` 只用于时区偏移。
    if reader.skip(b':') {
        return DateToken::symbol(b':');
    }
    if reader.skip(b'-') {
        return DateToken::symbol(b'-');
    }
    if reader.skip(b'+') {
        return DateToken::symbol(b'+');
    }
    if reader.skip(b'.') {
        return DateToken::symbol(b'.');
    }
    if reader.skip(b')') {
        return DateToken::symbol(b')');
    }
    // 词的首字符判定：码元 >= 'A' 且不是空白（不要求是字母）。
    if reader.ch >= u32::from(b'A') && !is_v8_white_space(reader.ch) {
        let mut prefix = [0u32; 3];
        let length = reader.read_word(&mut prefix);
        let (kind, value) = keyword_lookup(&prefix, length);
        return DateToken::keyword(kind, value, length);
    }
    // 空白单独成 token：legacy 循环用“时间数字之后是否紧跟非法字符”来拒绝多余内容。
    if reader.skip_white_space() {
        return DateToken::white_space(reader.position() - pre_pos);
    }
    // '(' 开头的括号注释整体吞掉，只产出一个长度为 1 的 unknown。
    if reader.skip_parentheses() {
        return DateToken::unknown();
    }
    // 其余字符（如 '['、控制字符）消费一个后产出 unknown，保证扫描总能前进、不会死循环。
    reader.next();
    DateToken::unknown()
}

/// `DateStringTokenizer`：带一个前瞻 token 的扫描器。
///
/// `next` 始终保存“下一个尚未消费的 token”，因此 `peek` 不推进、`next` 先返回再预扫描。
/// 这个前瞻让 ES5 解析器可以在不回溯的前提下判断当前 token 属于日期还是时间。
struct DateStringTokenizer {
    reader: InputReader,
    next: DateToken,
}

impl DateStringTokenizer {
    fn new(reader: InputReader) -> Self {
        let mut scanner = Self {
            reader,
            next: DateToken::end_of_input(),
        };
        scanner.next = scan(&mut scanner.reader);
        scanner
    }

    fn next(&mut self) -> DateToken {
        let result = self.next;
        self.next = scan(&mut self.reader);
        result
    }

    fn peek(&self) -> DateToken {
        self.next
    }

    fn skip_symbol(&mut self, symbol: u8) -> bool {
        if self.next.is_symbol(symbol) {
            self.next = scan(&mut self.reader);
            true
        } else {
            false
        }
    }
}

fn between(x: i32, lo: i32, hi: i32) -> bool {
    (lo..=hi).contains(&x)
}

fn is_month(x: i32) -> bool {
    between(x, 1, 12)
}

fn is_day(x: i32) -> bool {
    between(x, 1, 31)
}

fn is_hour(x: i32) -> bool {
    between(x, 0, 23)
}

fn is_minute(x: i32) -> bool {
    between(x, 0, 59)
}

fn is_second(x: i32) -> bool {
    between(x, 0, 59)
}

fn is_millisecond(x: i32) -> bool {
    between(x, 0, 999)
}

/// `DayComposer`：收集日期部分并消解 `D/M/Y` 顺序的歧义。
///
/// 词法层不知道 `11/06/94` 是“11 月 6 日”还是“6 月 11 日”，所以先把最多三个数字
/// 按出现顺序存进 `comp`，等 `write` 时结合月份名与 ISO 标志再决定各分量的角色。
struct DayComposer {
    /// 按出现顺序保存的数字（年/月/日待定）。
    comp: [i32; 3],
    /// `comp` 中已填写的个数。
    index: usize,
    /// 月份名给出的月份（1..12）；`V8_NONE` 表示文本里没有月份名。
    named_month: i32,
    /// 由 ES5 ISO 路径设置：禁止两位年份窗口，并按固定 YMD 解释。
    is_iso_date: bool,
}

impl DayComposer {
    fn new() -> Self {
        Self {
            comp: [0; 3],
            index: 0,
            named_month: V8_NONE,
            is_iso_date: false,
        }
    }

    fn is_empty(&self) -> bool {
        self.index == 0
    }

    fn add(&mut self, number: i32) -> bool {
        if self.index < 3 {
            self.comp[self.index] = number;
            self.index += 1;
            true
        } else {
            false
        }
    }

    fn set_named_month(&mut self, month: i32) {
        self.named_month = month;
    }

    fn set_iso_date(&mut self) {
        self.is_iso_date = true;
    }

    /// `DayComposer::Write`：返回 `(year, month0, day)`；`None` 表示日期不完整或越界。
    ///
    /// 消歧规则（与 V8 相同）：
    /// - 无月份名、ISO 或第一个数字不是合法“日” → `YMD`；
    /// - 无月份名、第一个数字是 1..31 → `MD(Y)`（美式 `MM/DD/YY`）；
    /// - 有月份名、第一个数字不是合法“日” → 年在前（`1994 Nov 6`）；
    /// - 有月份名、第一个数字是合法“日” → 日在前（`6 Nov 1994`）。
    fn write(&mut self) -> Option<(i32, i32, i32)> {
        // 一个数字都没有：这不是日期（例如只有时区名的输入）。
        if self.index < 1 {
            return None;
        }
        // 日与月默认 1（此时 index_ 一定被补到 3，V8 的 `index_ == 1` 分支因此不可达）。
        while self.index < 3 {
            self.comp[self.index] = 1;
            self.index += 1;
        }

        // 月份名或 ISO 标志已经固定了月份的位置，剩下的只是“年在前还是日在前”。
        let (mut year, month, day) = if self.named_month == V8_NONE {
            if self.is_iso_date || !is_day(self.comp[0]) {
                // YMD
                (self.comp[0], self.comp[1], self.comp[2])
            } else {
                // MD(Y)
                (self.comp[2], self.comp[0], self.comp[1])
            }
        } else if !is_day(self.comp[0]) {
            // YMD、MYD 或 YDM
            (self.comp[0], self.named_month, self.comp[1])
        } else {
            // DMY、MDY 或 DYM
            (self.comp[1], self.named_month, self.comp[0])
        };

        // 两位年份窗口只适用于非 ISO 文本：0..49 → 2000..2049，50..99 → 1950..1999。
        // ISO 的 `0094` 必须保持公元 94 年，不能被改写成 1994。
        if !self.is_iso_date {
            if (0..=49).contains(&year) {
                year += 2000;
            } else if (50..=99).contains(&year) {
                year += 1900;
            }
        }

        // 只校验范围，不校验该月实际天数（2 月 30 日交给 MakeDay 的日期加法滚动）。
        if !is_month(month) || !is_day(day) {
            return None;
        }
        Some((year, month - 1, day))
    }
}

/// `TimeComposer`：收集时/分/秒/毫秒，并应用 AM/PM 偏移。
///
/// `comp[0]` 是小时，往后依次是分、秒、毫秒；`index` 是已填写的个数。
struct TimeComposer {
    comp: [i32; 4],
    index: usize,
    /// AM/PM 给出的小时偏移（`am` = 0，`pm` = 12）；`V8_NONE` 表示没出现 AM/PM。
    hour_offset: i32,
}

impl TimeComposer {
    fn new() -> Self {
        Self {
            comp: [0; 4],
            index: 0,
            hour_offset: V8_NONE,
        }
    }

    fn is_empty(&self) -> bool {
        self.index == 0
    }

    /// 下一个数字是否“看起来是该位置的合法值”：1 → 分，2 → 秒，3 → 毫秒。
    ///
    /// legacy 循环在把数字归给日期还是时间时会用到它；只看位置与取值范围，不消费 token。
    fn is_expecting(&self, number: i32) -> bool {
        (self.index == 1 && is_minute(number))
            || (self.index == 2 && is_second(number))
            || (self.index == 3 && is_millisecond(number))
    }

    fn add(&mut self, number: i32) -> bool {
        if self.index < 4 {
            self.comp[self.index] = number;
            self.index += 1;
            true
        } else {
            false
        }
    }

    /// 添加最后一个时间数字，并把缺省的分/秒/毫秒补 0。
    ///
    /// 用于“时间到此结束”（例如 `10:30` 之后不再有 `:SS`）的场景：补零后
    /// `write` 才能统一按四分量校验。
    fn add_final(&mut self, number: i32) -> bool {
        if !self.add(number) {
            return false;
        }
        while self.index < 4 {
            self.comp[self.index] = 0;
            self.index += 1;
        }
        true
    }

    fn set_hour_offset(&mut self, offset: i32) {
        self.hour_offset = offset;
    }

    /// `TimeComposer::Write`：返回 `(hour, minute, second, millisecond)`。
    ///
    /// AM/PM 先折叠进小时（`12 am` → 0 点，`12 pm` → 12 点，`1 pm` → 13 点）；
    /// 随后统一校验范围，唯一例外是 ISO 允许的 `24:00:00.000`（表示当天结束）。
    fn write(&mut self) -> Option<(i32, i32, i32, i32)> {
        // 未出现的时间分量补 0（`10:30` 等价于 `10:30:00.000`）。
        while self.index < 4 {
            self.comp[self.index] = 0;
            self.index += 1;
        }
        let mut hour = self.comp[0];
        let minute = self.comp[1];
        let second = self.comp[2];
        let millisecond = self.comp[3];

        // 带 AM/PM 时小时只接受 0..12：先对 12 取模，再加 0（AM）或 12（PM）。
        // 因此 12 点不会被折叠成 0 点，而 0 点也能通过（V8 的判定范围如此）。
        if self.hour_offset != V8_NONE {
            if !between(hour, 0, 12) {
                return None;
            }
            hour %= 12;
            hour += self.hour_offset;
        }

        // 24 点只允许精确的 24:00:00.000；24:00:00.001 会被拒绝。
        let valid =
            is_hour(hour) && is_minute(minute) && is_second(second) && is_millisecond(millisecond);
        if !valid && !(hour == 24 && minute == 0 && second == 0 && millisecond == 0) {
            return None;
        }
        Some((hour, minute, second, millisecond))
    }
}

/// `TimeZoneComposer`：收集时区信息（命名时区或 `±HH:MM` 偏移）。
///
/// 三个字段都用 `V8_NONE` 表示“尚未设置”，因此“UTC（偏移 0）”与“没写时区”可以区分开：
/// 前者 `sign` 已设置且 hour/minute 为 0，后者 `sign` 仍是 `V8_NONE`。
struct TimeZoneComposer {
    /// 偏移方向：+1 / -1；`V8_NONE` 表示没有时区信息。
    sign: i32,
    /// 偏移的绝对小时数；`V8_NONE` 表示未给出。
    hour: i32,
    /// 偏移的绝对分钟数；`V8_NONE` 表示未给出（后续数字或 0 补齐）。
    minute: i32,
}

impl TimeZoneComposer {
    fn new() -> Self {
        Self {
            sign: V8_NONE,
            hour: V8_NONE,
            minute: V8_NONE,
        }
    }

    /// 由命名时区（`GMT`、`UTC`、`EST`…）设置固定小时偏移。
    ///
    /// `offset_in_hours` 的符号来自关键字表（例如 `EST` = -5），这里把它拆成
    /// “方向 + 绝对值”，与 `±HH:MM` 的后续设置共用同一套字段。
    fn set(&mut self, offset_in_hours: i32) {
        self.sign = if offset_in_hours < 0 { -1 } else { 1 };
        self.hour = offset_in_hours * self.sign;
        self.minute = 0;
    }

    /// 从 `+` / `-` token 设置方向，等待后续数字补上小时/分钟。
    fn set_sign(&mut self, sign: i32) {
        self.sign = if sign < 0 { -1 } else { 1 };
    }

    fn set_absolute_hour(&mut self, hour: i32) {
        self.hour = hour;
    }

    fn set_absolute_minute(&mut self, minute: i32) {
        self.minute = minute;
    }

    /// 下一个数字是否是 `GMT+5:30` 里的分钟：已经有时、还没有分，且值在 0..59。
    fn is_expecting(&self, number: i32) -> bool {
        self.hour != V8_NONE && self.minute == V8_NONE && is_minute(number)
    }

    /// 当前是否已确定为 UTC（偏移 0）。legacy 循环用它决定 `UTC+8` 里的 `+` 能否开启时区。
    fn is_utc(&self) -> bool {
        self.hour == 0 && self.minute == 0
    }

    fn is_empty(&self) -> bool {
        self.hour == V8_NONE
    }

    /// 外层 `None` 表示解析失败；内层 `None` 表示没有时区（按本地时间解释）。
    ///
    /// 成功时返回**相对 UTC 的秒数**（东正西负）；缺省的小时/分钟按 0 补齐。
    fn write(&self) -> Option<Option<i64>> {
        // 没有出现任何时区 token：交给本地时区解释。
        if self.sign == V8_NONE {
            return Some(None);
        }
        let hour = if self.hour == V8_NONE { 0 } else { self.hour };
        let minute = if self.minute == V8_NONE {
            0
        } else {
            self.minute
        };
        // V8 用 Smi 存秒数，超出范围视为非法；按“绝对小时/分钟”先算再套方向。
        let total = i64::from(hour) * 3600 + i64::from(minute) * 60;
        if total > V8_SMI_MAX {
            return None;
        }
        Some(Some(if self.sign < 0 { -total } else { total }))
    }
}

/// `ReadMilliseconds`：按原始数字的位数把小数秒缩放成毫秒。
///
/// 只取前三位、**截断**而非四舍五入：`.1` → 100、`.12` → 120、`.123456` → 123。
/// `length` 是扫描时消费的码元数（含前导零），`number` 是去掉前导零后的数值；
/// 超过 9 位按 9 位处理，避免缩放因子超出 i32。
fn read_milliseconds(token: DateToken) -> i32 {
    let mut number = token.number_value();
    let mut length = token.length();
    if length < 3 {
        if length == 1 {
            number *= 100;
        } else if length == 2 {
            number *= 10;
        }
    } else if length > 3 {
        if length > 9 {
            length = 9;
        }
        let mut factor = 1;
        loop {
            factor *= 10;
            length -= 1;
            if length <= 3 {
                break;
            }
        }
        number /= factor;
    }
    number
}

/// `ParseES5DateTime`：解析 ISO 前缀，返回交给 legacy 解析器的第一个 token。
///
/// 返回值有三种含义，调用方 `v8_date_parse` 必须区分：
/// - `end_of_input`：整串都按 ISO 解析完毕；
/// - `invalid`：确实是 ISO 形状但语法非法（如时间部分写成单数字小时），整次解析必须失败；
/// - 其他 token：前缀“不像 ISO”，该 token 尚未被消费，legacy 解析器从它继续
///   （RFC 850、asctime、`Nov 6 1994` 等格式走这条路）。
///
/// `day` / `time` / `tz` 与 legacy 循环**共享**：ISO 前缀成功时填写它们，legacy 循环
/// 随后在同一批 composer 上继续累加，因此两级的字段语义必须保持一致。
fn parse_es5_date_time(
    scanner: &mut DateStringTokenizer,
    day: &mut DayComposer,
    time: &mut TimeComposer,
    tz: &mut TimeZoneComposer,
) -> DateToken {
    // 阶段 1：年。两种写法：带符号的 6 位扩展年（`+002024`），或 4 位年（`2024`）。
    // 不符合这两种形状时，把第一个 token 原样交回 legacy 解析器（例如 `Sun, 06 Nov...`）。
    if scanner.peek().is_ascii_sign() {
        // 保留符号 token，以便 ES5 失败时交回 legacy 解析器。
        let sign_token = scanner.next();
        if !scanner.peek().is_fixed_length_number(6) {
            return sign_token;
        }
        let sign = sign_token.ascii_sign();
        let year = scanner.next().number_value();
        // `-000000` 不是合法的扩展年（V8 同样拒绝），交回 legacy。
        if sign < 0 && year == 0 {
            return sign_token;
        }
        let _ = day.add(sign * year);
    } else if scanner.peek().is_fixed_length_number(4) {
        let _ = day.add(scanner.next().number_value());
    } else {
        return scanner.next();
    }

    // 阶段 2：可选的 `-MM[-DD]`。月份/日必须是 2 位且落在合法范围；任一不满足就把该
    // token 交回 legacy（此时它可能解释成 `1994-1-1` 这种非 ISO 写法）。
    if scanner.skip_symbol(b'-') {
        let peek = scanner.peek();
        if !peek.is_fixed_length_number(2) || !is_month(peek.number_value()) {
            return scanner.next();
        }
        let _ = day.add(scanner.next().number_value());
        if scanner.skip_symbol(b'-') {
            let peek = scanner.peek();
            if !peek.is_fixed_length_number(2) || !is_day(peek.number_value()) {
                return scanner.next();
            }
            let _ = day.add(scanner.next().number_value());
        }
    }

    // 阶段 3：日期与时间的分界。没有 `T`（TimeSeparator）且还有剩余输入 → 不是 ISO，
    // 交回 legacy；只有“日期到此结束”才继续按 ISO 处理。
    if scanner.peek().keyword_type() != KeywordType::TimeSeparator {
        if !scanner.peek().is_end_of_input() {
            return scanner.next();
        }
    } else {
        // 阶段 4：时间。从这里开始已经确认是 ISO 形状，任何错误都返回 invalid，
        // 不再回退 legacy——这正是 `1994-11-06T8:49:37Z` 被判为非法日期的方式。
        scanner.next();
        let peek = scanner.peek();
        // 小时必须恰好两位且在 0..24；24 只允许配 00:00.000（在下面逐段检查）。
        if !peek.is_fixed_length_number(2) || !between(peek.number_value(), 0, 24) {
            return DateToken::invalid();
        }
        let hour_is_24 = peek.number_value() == 24;
        let _ = time.add(scanner.next().number_value());
        if !scanner.skip_symbol(b':') {
            return DateToken::invalid();
        }
        // 分钟必须两位且在 0..59；24 点必须为 0 分。
        let peek = scanner.peek();
        if !peek.is_fixed_length_number(2)
            || !is_minute(peek.number_value())
            || (hour_is_24 && peek.number_value() > 0)
        {
            return DateToken::invalid();
        }
        let _ = time.add(scanner.next().number_value());
        if scanner.skip_symbol(b':') {
            // 秒同样必须两位、0..59；24 点必须为 0 秒。
            let peek = scanner.peek();
            if !peek.is_fixed_length_number(2)
                || !is_second(peek.number_value())
                || (hour_is_24 && peek.number_value() > 0)
            {
                return DateToken::invalid();
            }
            let _ = time.add(scanner.next().number_value());
            if scanner.skip_symbol(b'.') {
                // 小数秒允许任意位数字（read_milliseconds 只取前三位）；24 点时
                // 按原始数值判断必须为 0。
                let peek = scanner.peek();
                if !peek.is_number() || (hour_is_24 && peek.number_value() > 0) {
                    return DateToken::invalid();
                }
                let token = scanner.next();
                let _ = time.add(read_milliseconds(token));
            }
        }

        // 阶段 5：时区。`Z` 表示 UTC；`+HHMM` / `+HH:MM` 两种写法都接受，缺少 `:` 时
        // 必须恰好 4 位数字，否则要求 `HH:MM`。
        if scanner.peek().is_keyword_z() {
            scanner.next();
            tz.set(0);
        } else if scanner.peek().is_symbol(b'+') || scanner.peek().is_symbol(b'-') {
            let sign_token = scanner.next();
            tz.set_sign(if sign_token.is_symbol(b'+') { 1 } else { -1 });
            if scanner.peek().is_fixed_length_number(4) {
                // hhmm 扩展语法。
                let hourmin = scanner.next().number_value();
                let hour = hourmin / 100;
                let minute = hourmin % 100;
                if !is_hour(hour) || !is_minute(minute) {
                    return DateToken::invalid();
                }
                tz.set_absolute_hour(hour);
                tz.set_absolute_minute(minute);
            } else {
                // hh:mm 标准语法。
                let peek = scanner.peek();
                if !peek.is_fixed_length_number(2) || !is_hour(peek.number_value()) {
                    return DateToken::invalid();
                }
                tz.set_absolute_hour(scanner.next().number_value());
                if !scanner.skip_symbol(b':') {
                    return DateToken::invalid();
                }
                let peek = scanner.peek();
                if !peek.is_fixed_length_number(2) || !is_minute(peek.number_value()) {
                    return DateToken::invalid();
                }
                tz.set_absolute_minute(scanner.next().number_value());
            }
        }
        // ISO 时间之后不允许再有多余内容（空白也不行）。
        if !scanner.peek().is_end_of_input() {
            return DateToken::invalid();
        }
    }

    // 阶段 6：收尾。ISO 规定只有日期的形式（`1994-11-06`）按 UTC 解释，因此没有时区时
    // 显式补一个 0 偏移；`set_iso_date` 关闭两位年份窗口。只有时间的文本在阶段 1 就已
    // 交回 legacy，不会走到这里。
    if tz.is_empty() && time.is_empty() {
        tz.set(0);
    }
    day.set_iso_date();
    DateToken::end_of_input()
}

/// ES `MakeDay`（含月份归一化与日期溢出）。
///
/// 输入是 ES 语义的 `Number`：`month` 是 0 基且可以越界（`12` → 次年 1 月，`-1` → 上一年
/// 12 月），`date` 可以越界（2 月 30 日会滚到 3 月）。返回“自 1970-01-01 起的天数”，
/// 失败为 `None`。
///
/// 先把年折算成“距 1970 年的天数”，再用平年/闰年的“月首天数表”加上月份偏移，
/// 最后 `-1 + date` 把 1 基的日转成 0 基的天数。年份平移 `K_YEAR_DELTA`（399999）沿用了
/// V8 的写法：平移后常规年份区间的中间值为正，整数除法（向零截断）才与 ES 的向下取整一致。
fn make_day(year: f64, month: f64, date: f64) -> Option<f64> {
    const K_MIN_YEAR: f64 = -1_000_000.0;
    const K_MAX_YEAR: f64 = 1_000_000.0;
    const K_MIN_MONTH: f64 = -10_000_000.0;
    const K_MAX_MONTH: f64 = 10_000_000.0;
    if !(K_MIN_YEAR..=K_MAX_YEAR).contains(&year)
        || !(K_MIN_MONTH..=K_MAX_MONTH).contains(&month)
        || !date.is_finite()
    {
        return None;
    }
    // 月份归一化：Rust 的整数除法向零截断，负月份（`m % 12 < 0`）要显式回补一年。
    let mut y = year as i32;
    let mut m = month as i32;
    y += m / 12;
    m %= 12;
    if m < 0 {
        m += 12;
        y -= 1;
    }

    const K_YEAR_DELTA: i32 = 399_999;
    const K_BASE_DAY: i32 = 365 * (1970 + K_YEAR_DELTA) + (1970 + K_YEAR_DELTA) / 4
        - (1970 + K_YEAR_DELTA) / 100
        + (1970 + K_YEAR_DELTA) / 400;
    const DAY_FROM_MONTH_COMMON: [i32; 12] =
        [0, 31, 59, 90, 120, 151, 181, 212, 243, 273, 304, 334];
    const DAY_FROM_MONTH_LEAP: [i32; 12] = [0, 31, 60, 91, 121, 152, 182, 213, 244, 274, 305, 335];

    let shifted_year = y + K_YEAR_DELTA;
    let mut day_from_year = 365 * shifted_year + shifted_year / 4 - shifted_year / 100
        + shifted_year / 400
        - K_BASE_DAY;
    let leap = y % 4 == 0 && (y % 100 != 0 || y % 400 == 0);
    let month_days = if leap {
        DAY_FROM_MONTH_LEAP
    } else {
        DAY_FROM_MONTH_COMMON
    };
    day_from_year += month_days[m as usize];

    Some(f64::from(day_from_year - 1) + date.trunc())
}

/// ES `MakeTime`：把时/分/秒/毫秒换算成毫秒数。
///
/// 每个分量先 `trunc()` 取整（ES 的 `ToInteger` 语义，截断而非四舍五入）；任一分量不是
/// 有限数就返回 `None`。这里不做范围检查——范围由调用方 `TimeComposer::write` 负责。
fn make_time(hour: f64, minute: f64, second: f64, millisecond: f64) -> Option<f64> {
    if hour.is_finite() && minute.is_finite() && second.is_finite() && millisecond.is_finite() {
        Some(
            hour.trunc() * 3_600_000.0
                + minute.trunc() * 60_000.0
                + second.trunc() * 1000.0
                + millisecond.trunc(),
        )
    } else {
        None
    }
}

/// 系统本地时区的偏移（毫秒，东为正）；`local_ms` 是“把本地墙上时间当作 UTC”的毫秒数。
///
/// 做法与 V8 的 `DateCache::LocalOffsetInMs` 对应：把 `local_ms` 当成 UTC 构造一个
/// `NaiveDateTime`，再让 `chrono::Local` 按本地时区解释它，取解释结果的 UTC 偏移。
///
/// 边界：
/// - `local_ms` 超出 i64 或 chrono 的表示范围 → `None`（最终得到 NaN）；
/// - 夏令时回拨造成的歧义 → 取较早的偏移（`Ambiguous` 的第一个值）；
/// - 夏令时跳变造成的“不存在的墙上时间” → 退回“把同一数值当作 UTC 时刻”所在时区的偏移；
///   这是一个近似值，但保证总能给出一个偏移（V8/ICU 在缺口时也会返回某个偏移）。
fn system_local_offset_ms(local_ms: f64) -> Option<f64> {
    if local_ms < i64::MIN as f64 || local_ms > i64::MAX as f64 {
        return None;
    }
    let millis = local_ms as i64;
    let naive = chrono::DateTime::<chrono::Utc>::from_timestamp_millis(millis)?.naive_utc();
    match Local.from_local_datetime(&naive) {
        chrono::LocalResult::Single(dt) => Some(f64::from(dt.offset().local_minus_utc()) * 1000.0),
        chrono::LocalResult::Ambiguous(earlier, _) => {
            Some(f64::from(earlier.offset().local_minus_utc()) * 1000.0)
        }
        chrono::LocalResult::None => {
            // DST 跳变缺口：按 UTC 时刻查出所在时区的偏移（V8/ICU 也返回一个偏移）。
            let utc = chrono::DateTime::<chrono::Utc>::from_timestamp_millis(millis)?;
            Some(f64::from(utc.with_timezone(&Local).offset().local_minus_utc()) * 1000.0)
        }
    }
}

/// V8 `DateParser::Parse` + `ParseDateTimeString`。
///
/// 输入是待解析文本和一个“本地偏移查询”闭包（测试注入固定偏移，生产环境传
/// [`system_local_offset_ms`]）。返回 Unix 毫秒时间戳；`None` 表示不是合法日期。
///
/// 整体是两级解析：先让 `parse_es5_date_time` 尝试 ISO 前缀；若前缀不像 ISO，则对同一个
/// token 流继续跑 legacy 状态机。两级共享三个 composer，因此 ISO 已读到的年/月/日不会
/// 被 legacy 循环重复消费。
fn v8_date_parse<F>(text: &str, local_offset_ms: &mut F) -> Option<f64>
where
    F: FnMut(f64) -> Option<f64>,
{
    // 按 UTF-16 码元切片，保持与 JS 字符串索引一致（代理对占两个位置）。
    let units: Vec<u32> = text.encode_utf16().map(u32::from).collect();
    let mut scanner = DateStringTokenizer::new(InputReader::new(units));
    let mut day = DayComposer::new();
    let mut time = TimeComposer::new();
    let mut tz = TimeZoneComposer::new();

    // 第一级：ISO 前缀。`invalid` 是“确认是 ISO 但写错了”，必须整体失败；
    // 其他 token 则是 legacy 解析的起点（可能已经是 end_of_input）。
    let next_unhandled = parse_es5_date_time(&mut scanner, &mut day, &mut time, &mut tz);
    if next_unhandled.is_invalid() {
        return None;
    }
    // ISO 前缀可能已经读过日期数字，垃圾词规则据此判断“能否出现在第一个数字之前”。
    let mut has_read_number = !day.is_empty();
    let mut token = next_unhandled;

    // 第二级：legacy 状态机。每个 token 只走一条分支，分支内部再决定消费多少后续 token。
    while !token.is_end_of_input() {
        // 数字的归属先看后续符号（`:`/`.`），再看 composer 状态：`n:` → 时间分量；
        // `n::` → 小时 + 省略的分钟；带 `.` 且时间正期待该位置 → 小数秒；能延续时区/时间
        // 的数字归对应 composer（`tz.is_expecting`/`time.is_expecting`，如 `10:30` 里的 30、
        // `GMT+5:30` 里的 30）；其余才作为日期数字（年/月/日的角色由 `DayComposer::write`
        // 稍后消歧）。
        if token.is_number() {
            has_read_number = true;
            let number = token.number_value();
            if scanner.skip_symbol(b':') {
                if scanner.skip_symbol(b':') {
                    // number + "::"：`10::` 表示 10 时 00 分，且时间之前不能已有分量。
                    if !time.is_empty() {
                        return None;
                    }
                    let _ = time.add(number);
                    let _ = time.add(0);
                } else {
                    // number + ":"：常规的“小时:”开头；后面若紧跟 '.'，把点留给小数秒分支。
                    if !time.add(number) {
                        return None;
                    }
                    if scanner.peek().is_symbol(b'.') {
                        scanner.next();
                    }
                }
            } else if scanner.skip_symbol(b'.') && time.is_expecting(number) {
                let _ = time.add(number);
                if !scanner.peek().is_number() {
                    return None;
                }
                let milliseconds = read_milliseconds(scanner.next());
                let _ = time.add_final(milliseconds);
            } else if tz.is_expecting(number) {
                // `GMT+5:30` 中 ':' 之后的分钟数。
                tz.set_absolute_minute(number);
            } else if time.is_expecting(number) {
                // 时间到这里结束（缺省分/秒/毫秒补 0），随后必须是分隔符或输入结束。
                let _ = time.add_final(number);
                // 结束时间后必须紧跟结尾、空白、"Z"、'+' 或 '-'。
                let peek = scanner.peek();
                if !peek.is_end_of_input()
                    && !peek.is_whitespace()
                    && !peek.is_keyword_z()
                    && !peek.is_ascii_sign()
                {
                    return None;
                }
            } else {
                // 日期部分最多三个数字；`-` 只是分隔符，允许 `1994-11-06` 被逐段读出。
                if !day.add(number) {
                    return None;
                }
                scanner.skip_symbol(b'-');
            }
        } else if token.is_keyword() {
            // 关键字：AM/PM 只在已有时间时生效；月份名随时可设；时区名只在读过数字后
            // 生效；其余（未知词、位置不对的关键字）落到垃圾词规则。
            let kind = token.keyword_type();
            let value = token.keyword_value();
            if kind == KeywordType::AmPm && !time.is_empty() {
                time.set_hour_offset(value);
            } else if kind == KeywordType::MonthName {
                day.set_named_month(value);
                scanner.skip_symbol(b'-');
            } else if kind == KeywordType::TimeZoneName && has_read_number {
                tz.set(value);
            } else {
                // 读到数字后不允许出现无法识别的词。
                if has_read_number {
                    return None;
                }
                // 第一个数字必须与垃圾词之间有空白或其他分隔符。
                if scanner.peek().is_number() {
                    return None;
                }
            }
        } else if token.is_ascii_sign() && (tz.is_utc() || !time.is_empty()) {
            // UTC 或时间之后才能解析 UTC 偏移；否则 '+'/'-' 是非法残留（见下一个分支）。
            tz.set_sign(token.ascii_sign());
            let mut number = 0i32;
            let mut length = 0usize;
            // 数字只有在紧跟 `:` 时才可省略（如 `GMT+:30` 表示 +00:30，分钟由后续数字补上）；
            // `GMT+` 这种完全省略不满足任何长度分支，最终被末尾的 else 拒绝。
            if scanner.peek().is_number() {
                let next_token = scanner.next();
                length = next_token.length();
                number = next_token.number_value();
            }
            has_read_number = true;

            if scanner.peek().is_symbol(b':') {
                // `+5:30`：小时已给出，分钟留给后续数字（由 tz.is_expecting 识别）。
                tz.set_absolute_hour(number);
                tz.set_absolute_minute(V8_NONE);
            } else if length == 1 || length == 2 {
                // GMT-8 这类形式。
                tz.set_absolute_hour(number);
                tz.set_absolute_minute(0);
            } else if length == 3 || length == 4 {
                // hhmm 形式。
                tz.set_absolute_hour(number / 100);
                tz.set_absolute_minute(number % 100);
            } else {
                // 0 位（`GMT+` 后既没有数字也没有 `:`）或 5 位以上的数字既不是小时也不是 hhmm。
                return None;
            }
        } else if (token.is_ascii_sign() || token.is_symbol(b')')) && has_read_number {
            // 读到数字后多余的符号或 ')' 非法。
            return None;
        }

        token = scanner.next();
    }

    // 三个 composer 依次定稿；任一返回 None 都表示日期/时间/时区不完整或越界。
    let (year, month, day_of_month) = day.write()?;
    let (hour, minute, second, millisecond) = time.write()?;
    let offset_seconds = tz.write()?;

    // ES 的 MakeDay 给出“天数”，MakeTime 给出“当天毫秒”，两者拼成时间戳。
    let day_ms = make_day(f64::from(year), f64::from(month), f64::from(day_of_month))?;
    let time_ms = make_time(
        f64::from(hour),
        f64::from(minute),
        f64::from(second),
        f64::from(millisecond),
    )?;
    let mut date = day_ms * 86_400_000.0 + time_ms;

    if let Some(offset_seconds) = offset_seconds {
        // 显式时区：`offset_seconds` 是 UTC 偏移（东正），减去它把墙上时间转成 UTC 时间戳。
        date -= offset_seconds as f64 * 1000.0;
    } else {
        // 无时区：先把值限制在一个略宽的窗口内，避免把超出 i64 的数交给本地偏移查询；
        // 再用“本地时间 ↔ UTC”的偏移做换算（日期是按本地墙上时间读出的）。
        let max_before_utc = K_MAX_TIME_IN_MS + K_MS_PER_MONTH;
        if !(-max_before_utc..=max_before_utc).contains(&date) {
            return None;
        }
        date -= local_offset_ms(date)?;
    }

    // 换算后必须仍在 JS `Date` 的可表示范围内（±8.64e15 ms），否则 Date.parse 返回 NaN。
    if !(-K_MAX_TIME_IN_MS..=K_MAX_TIME_IN_MS).contains(&date) {
        return None;
    }
    // `trunc` 让返回值保持整数毫秒（浮点拼接可能留下小数位）。
    Some(date.trunc())
}

/// JS 的 `Date.parse`：返回毫秒时间戳，失败为 `NaN`。
///
/// 完整移植 V8 的 ES5 ISO + legacy 解析器（二者共享月份/时区/时间状态机）。
/// 不带时区的日期时间按进程本地时区解释（ISO 的 `1994-11-06T08:49:37` 与 legacy 写法
/// 一样）；ISO 纯日期形式（如 `1994-11-06`）例外，按 ES5 规则视为 UTC。与 Node 一致。
///
/// 返回值是整数毫秒（可能为负，表示 1970 年之前）；无法解析或超出 JS `Date` 范围时为
/// `NaN`。本函数只被 `retry-after` 的回退分支和 Codex 的响应头解析使用。
///
/// 行为基准：`tests/fixtures/utils/date_parse.json` 的 318 条用例（分别在 UTC 与
/// Asia/Shanghai 下记录期望值），对照测试见本文件 `mod tests` 的
/// `matches_v8_date_parse_in_utc` / `matches_v8_date_parse_with_eight_hour_local_offset`
/// （两者都由 `load_date_parse_cases` 读取该 fixture）。
pub fn parse_retry_after_date(text: &str) -> f64 {
    let mut local_offset = system_local_offset_ms as fn(f64) -> Option<f64>;
    v8_date_parse(text, &mut local_offset).unwrap_or(f64::NAN)
}

/// 从 `SystemTime` 取毫秒（与 JS 的 `Date.now()` 同义）。
///
/// 时钟早于 Unix 纪元（理论上不会发生）时返回 `NaN`；`as_millis()` 本身截断亚毫秒，
/// 与 JS 的整数毫秒语义一致。
fn now_ms() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(f64::NAN, |duration| duration.as_millis() as f64)
}

/// `getRetryDelayMs(error, retryIndex, maxRetryDelayMs)`：按优先级计算本次退避（毫秒）。
///
/// 优先级：`retry-after-ms` → `retry-after`（秒数或 HTTP 日期）→ 指数退避。
/// 服务端给出的值先过 [`validate_server_retry_delay_ms`]：超过上限时返回 `Err(消息)`
/// （调用方据此立即失败）；解析不出有限数时继续尝试下一个来源，最后回落到指数退避。
///
/// `retry_index` 是 0 基的重试序号（第一次重试为 0）。指数退避的基值是
/// `min(0.5 * 2^index, 8)` 秒（0.5s、1s、2s、4s，之后封顶 8s），再乘
/// `(1 - random*0.25)`，所以实际延迟落在基值的 75%..100%。
fn get_retry_delay_ms(
    error: &ProviderError,
    retry_index: u32,
    max_retry_delay_ms: Option<u64>,
) -> Result<f64, String> {
    // 第一优先级：`retry-after-ms`（毫秒）。空字符串按“头不存在”处理——对应 TS 里
    // `if (retryAfterMs)` 的真值判断（`"0"` 非空，仍会被解析为 0）。
    if let Some(retry_after_ms) = error
        .header("retry-after-ms")
        .filter(|value| !value.is_empty())
    {
        let value = parse_float(&retry_after_ms);
        // `is_finite` 同时排除 NaN 与 ±Infinity（对应 TS 的 `Number.isFinite`）。
        if value.is_finite() {
            return validate_server_retry_delay_ms(value, max_retry_delay_ms, &error.message);
        }
    }

    // 第二优先级：`retry-after`。先按秒数解析；不是数字时按 HTTP 日期解释，延迟为
    // “服务端日期 - 现在”。日期在过去时延迟为负，后续睡眠会夹到 0。
    if let Some(retry_after) = error
        .header("retry-after")
        .filter(|value| !value.is_empty())
    {
        let seconds = parse_float(&retry_after);
        let delay_ms = if seconds.is_nan() {
            parse_retry_after_date(&retry_after) - now_ms()
        } else {
            seconds * 1000.0
        };
        if delay_ms.is_finite() {
            return validate_server_retry_delay_ms(delay_ms, max_retry_delay_ms, &error.message);
        }
    }

    // 兜底：指数退避。`powi` 的指数是 i32，超过 2^31 的重试序号在实践中不可达。
    let exponential_delay = (0.5 * 2f64.powi(retry_index as i32)).min(8.0) * 1000.0;
    Ok(exponential_delay * (1.0 - default_jitter() * 0.25))
}

/// `createAbortError()`：`name = "AbortError"` 的 `"Request aborted"`。
///
/// 这是重试层专用的取消错误：调用方应在第一次尝试后、退避睡眠期间发现取消时返回它。
/// 消息与 `utils::abort` 的两种缺省 reason 都不同（`"This operation was aborted"` /
/// `"The operation was aborted"`），因此下游只能按 `name == "AbortError"` 或
/// `kind == Aborted` 判断取消，不能比较消息文本。
pub fn create_abort_error() -> crate::error::AiError {
    crate::error::AiError::new(crate::error::AiErrorKind::Aborted, "Request aborted")
        .with_name("AbortError")
}

/// `abortableSleep(ms, signal)`：睡 `ms` 毫秒，期间可被 `signal` 打断。
///
/// 没有 signal 时退化为普通 `tokio::time::sleep`；有 signal 时先做一次“已中止”预检查
/// （对应 TS 在 `new Promise` 里立即 reject），再用 `select!` 让取消与定时器赛跑。
/// 唯一的错误是 `AbortError`；负延迟按 0 处理，小数毫秒向零截断为整毫秒
/// （`Duration::from_millis` 只接受整数）。
// `AiError` 的尺寸由 `ARCHITECTURE.md` §4.1 的公开错误契约固定。
#[allow(clippy::result_large_err)]
async fn abortable_sleep(
    ms: f64,
    signal: Option<&AbortScope>,
) -> Result<(), crate::error::AiError> {
    let Some(scope) = signal else {
        tokio::time::sleep(std::time::Duration::from_millis(ms.max(0.0) as u64)).await;
        return Ok(());
    };
    if scope.is_aborted() {
        return Err(create_abort_error());
    }
    let token = scope.token();
    // 预检查与 select 之间不存在丢唤醒：`CancellationToken::cancelled()` 在令牌已取消时
    // 会立即完成，因此这里即使被抢先取消也会立刻返回 AbortError。
    // 取消时 select 丢弃 sleep future，定时器随之中止；不需要额外的清理。
    tokio::select! {
        _ = tokio::time::sleep(std::time::Duration::from_millis(ms.max(0.0) as u64)) => Ok(()),
        _ = token.cancelled() => Err(create_abort_error()),
    }
}

/// `retryProviderRequest(request, options)`。
///
/// 每次重试都是一次全新的 SDK 请求，因此 `X-Stainless-Retry-Count` 保持为 0。
///
/// # 参数与调用约定
///
/// - `request`：`FnMut`，每次尝试调用一次；它必须自己构造新请求（不得复用已被消费的
///   body/流）。调用只会在上一次的 future 完成后发生，因此不会并发发起两次尝试。
/// - `options`：见 [`ProviderRetryOptions`]；`max_retries` 缺省为 0，即只尝试一次。
///
/// # 返回与错误
///
/// - 任意一次尝试成功 → 立即返回 `Ok`（即使此时 signal 已中止）。
/// - 重试预算耗尽、错误不可重试、或状态码命中 `no_retry_statuses` → 返回
///   `AiErrorKind::Other`，消息是 provider 的原文。
/// - 服务端要求的延迟超过上限 → `AiErrorKind::Other`，消息是
///   `Server requested {n}s retry delay (max: {m}s). {message}`。
/// - signal 在失败后被检测到已中止，或退避睡眠被打断 → `AiErrorKind::Aborted` 的
///   `"Request aborted"`（`name = "AbortError"`）。
///
/// 本函数不设超时，也不主动取消已经发出的请求；这些由调用方的 HTTP 层负责。
/// 注意取消**只在一次尝试失败之后**检查：signal 在首次请求前已中止时，首次请求仍会发出。
// `AiError` 的尺寸由 `ARCHITECTURE.md` §4.1 的公开错误契约固定，签名保持不变。
#[allow(clippy::result_large_err)]
pub async fn retry_provider_request<T, F, Fut>(
    mut request: F,
    options: ProviderRetryOptions,
) -> Result<T, crate::error::AiError>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<T, ProviderError>>,
{
    let max_retries = options.max_retries.unwrap_or(0);
    let mut retries_remaining = max_retries;

    loop {
        match request().await {
            Ok(value) => return Ok(value),
            Err(error) => {
                // 取消优先于重试策略：调用方主动中止时，不应该再看到服务端的错误。
                if options
                    .signal
                    .as_ref()
                    .is_some_and(|scope| scope.is_aborted())
                {
                    return Err(create_abort_error());
                }
                // 预算耗尽或错误本身不可重试：返回最后一次 provider 错误（消息原样）。
                if retries_remaining == 0 || !is_retryable_provider_error(&error) {
                    return Err(provider_error_to_ai_error(&error));
                }
                // 黑名单只在“默认可重试”之后生效；无 status 的错误不受它影响。
                if let Some(status) = error.status
                    && options.no_retry_statuses.contains(&status)
                {
                    return Err(provider_error_to_ai_error(&error));
                }
                // 序号必须在扣减预算之前算：0 表示第一次重试（对应指数退避的 0.5s 基数）。
                let retry_index = (max_retries - retries_remaining) as u32;
                retries_remaining -= 1;
                let delay =
                    match get_retry_delay_ms(&error, retry_index, options.max_retry_delay_ms) {
                        Ok(delay) => delay,
                        Err(message) => {
                            // 服务端要求的等待超过上限：不睡眠，直接把消息作为错误返回。
                            return Err(crate::error::AiError::new(
                                crate::error::AiErrorKind::Other,
                                message,
                            ));
                        }
                    };
                // 睡眠被取消时返回 AbortError（`?` 直接向上传播）。
                abortable_sleep(delay, options.signal.as_ref()).await?;
            }
        }
    }
}

/// provider 错误 → `AiError`（保留 SDK 的 `error.message`）。
///
/// 只保留消息：`AiError` 没有 status/headers 字段，重试层也不向上传递它们。
/// 需要完整失败的调用方应在闭包里另存一份（例如 `openai_responses_shared` 的
/// `last_failure`），不要指望这个转换保留细节。
fn provider_error_to_ai_error(error: &ProviderError) -> crate::error::AiError {
    crate::error::AiError::new(crate::error::AiErrorKind::Other, error.message.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn error_with(
        status: Option<u16>,
        headers: Vec<(&'static str, &'static str)>,
        message: &str,
    ) -> ProviderError {
        let lookup: HeaderLookup = Some(std::sync::Arc::new(move |name: &str| {
            headers
                .iter()
                .find(|(key, _)| key.eq_ignore_ascii_case(name))
                .map(|(_, value)| (*value).to_string())
        }));
        ProviderError {
            message: message.to_string(),
            status,
            header: lookup,
        }
    }

    #[test]
    fn honours_the_should_retry_header() {
        let error = error_with(Some(400), vec![("x-should-retry", "true")], "bad");
        assert!(is_retryable_provider_error(&error));
        let error = error_with(Some(500), vec![("x-should-retry", "false")], "bad");
        assert!(!is_retryable_provider_error(&error));
    }

    #[test]
    fn retries_sdk_statuses() {
        for status in [408, 409, 429, 500, 502, 503, 504, 529] {
            assert!(
                is_retryable_provider_error(&error_with(Some(status), vec![], "x")),
                "{status}"
            );
        }
        for status in [400, 401, 403, 404, 422, 501] {
            assert!(
                !is_retryable_provider_error(&error_with(Some(status), vec![], "x")),
                "{status}"
            );
        }
        assert!(is_retryable_provider_error(&error_with(
            None,
            vec![],
            "network"
        )));
        // request_policy 集合里的非 SDK 状态码也必须可重试。
        assert!(is_retryable_provider_error(&error_with(
            Some(425),
            vec![],
            "x"
        )));
    }

    #[test]
    fn typed_should_retry_false_overrides_a_retryable_status() {
        let error = error_with(Some(500), vec![("x-should-retry", "false")], "bad");
        assert!(!is_retryable_provider_error(&error));
    }

    #[test]
    fn caps_server_requested_delays() {
        let error = error_with(Some(429), vec![("retry-after-ms", "120000")], "slow down");
        let failure = get_retry_delay_ms(&error, 0, None).expect_err("over the cap");
        assert_eq!(
            failure,
            "Server requested 120s retry delay (max: 60s). slow down"
        );
        // TS 对浮点数做 `Math.ceil(delayMs / 1000)`；先截断成整数会得到 60s。
        for (header, value) in [("retry-after-ms", "60000.5"), ("retry-after", "60.0005")] {
            let error = error_with(Some(429), vec![(header, value)], "slow down");
            assert_eq!(
                get_retry_delay_ms(&error, 0, None).expect_err("over the cap"),
                "Server requested 61s retry delay (max: 60s). slow down"
            );
        }
        // 关闭上限（0）时不报错。
        assert_eq!(
            get_retry_delay_ms(&error, 0, Some(0)).expect("no cap"),
            120_000.0
        );
    }

    #[test]
    fn uses_retry_after_seconds_and_dates() {
        let error = error_with(Some(429), vec![("retry-after", "2.5")], "slow down");
        assert_eq!(
            get_retry_delay_ms(&error, 0, None).expect("seconds"),
            2_500.0
        );

        let error = error_with(Some(429), vec![("retry-after", "bad-date")], "slow down");
        let delay = get_retry_delay_ms(&error, 0, Some(0)).expect("date");
        assert!(!delay.is_nan());
    }

    #[test]
    fn parses_dates_like_date_parse() {
        assert_eq!(
            parse_retry_after_date("Sun, 06 Nov 1994 08:49:37 GMT"),
            784_111_777_000.0
        );
        assert_eq!(
            parse_retry_after_date("1994-11-06T08:49:37Z"),
            784_111_777_000.0
        );
        assert_eq!(
            parse_retry_after_date("1994-11-06T08:49:37.000Z"),
            784_111_777_000.0
        );
        assert!(parse_retry_after_date("not a date").is_nan());
        // RFC 850 / ES5 变体 / 小数秒 / 小写 t、z / hhmm 偏移（全部带显式时区）。
        for (text, expected) in [
            ("Sunday, 06-Nov-94 08:49:37 GMT", 784_111_777_000.0),
            ("Sunday, 06-Nov-1994 08:49:37 GMT", 784_111_777_000.0),
            ("1994-11-06t08:49:37z", 784_111_777_000.0),
            ("1994-11-06T08:49:37.1Z", 784_111_777_100.0),
            ("1994-11-06T08:49:37.123Z", 784_111_777_123.0),
            ("1994-11-06T08:49:37.123456Z", 784_111_777_123.0),
            ("1994-11-06T08:49:37+0100", 784_108_177_000.0),
            ("1994-11-06T08:49:37-05:00", 784_129_777_000.0),
        ] {
            assert_eq!(
                parse_retry_after_date(text),
                expected,
                "Date.parse({text:?})"
            );
        }
        // 没有两位小时时 ES5 解析失败，不会被 legacy 解析器接受。
        assert!(parse_retry_after_date("1994-11-06T8:49:37Z").is_nan());
    }

    /// 用固定本地偏移调用 V8 解析器（+480 分钟 = Asia/Shanghai 的现代偏移）。
    fn parse_with_local_offset(text: &str, offset_minutes: i32) -> f64 {
        let offset_ms = f64::from(offset_minutes) * 60_000.0;
        let mut local_offset = move |_local_ms: f64| Some(offset_ms);
        v8_date_parse(text, &mut local_offset).unwrap_or(f64::NAN)
    }

    fn load_date_parse_cases() -> Vec<(String, Option<f64>, Option<f64>)> {
        let path = format!(
            "{}/tests/fixtures/utils/date_parse.json",
            env!("CARGO_MANIFEST_DIR")
        );
        let value: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(path).expect("fixture readable"))
                .expect("fixture is valid JSON");
        value["cases"]
            .as_array()
            .expect("cases")
            .iter()
            .map(|case| {
                (
                    case["text"].as_str().expect("text").to_string(),
                    case["utc"].as_f64(),
                    case["sh"].as_f64(),
                )
            })
            .collect()
    }

    /// 318 条与 Node `Date.parse` 逐字对齐的基准（TZ=UTC）。
    #[test]
    fn matches_v8_date_parse_in_utc() {
        let cases = load_date_parse_cases();
        assert!(cases.len() >= 300, "fixture size {}", cases.len());
        for (text, expected, _) in cases {
            let actual = parse_with_local_offset(&text, 0);
            match expected {
                Some(expected) => {
                    assert_eq!(actual, expected, "Date.parse({text:?})");
                }
                None => assert!(actual.is_nan(), "Date.parse({text:?}) = {actual}"),
            }
        }
    }

    /// 无时区文本必须按本地时区解释（TZ=Asia/Shanghai 的基准）。
    #[test]
    fn matches_v8_date_parse_with_eight_hour_local_offset() {
        let cases = load_date_parse_cases();
        let mut checked = 0;
        for (text, utc, shanghai) in cases {
            let Some(utc) = utc else {
                assert!(shanghai.is_none(), "Date.parse({text:?})");
                continue;
            };
            let shanghai = shanghai.expect("valid UTC implies valid Shanghai");
            let difference = utc - shanghai;
            // 跳过上海历史上的 LMT（+08:05:43）与夏令时（+09:00）。
            if difference != 0.0 && difference != 28_800_000.0 {
                continue;
            }
            assert_eq!(
                parse_with_local_offset(&text, 480),
                shanghai,
                "Date.parse({text:?}) with TZ=+08:00"
            );
            checked += 1;
        }
        assert!(checked >= 220, "checked {checked}");
    }

    /// 评审给出的无时区取值（TZ=Asia/Shanghai）。
    #[test]
    fn interprets_zoneless_dates_in_the_local_timezone() {
        for (text, expected) in [
            ("1994-11-06T08:49:37", 784_082_977_000.0),
            ("1994-11-06 08:49:37", 784_082_977_000.0),
            ("Sun Nov  6 08:49:37 1994", 784_082_977_000.0),
            ("1994-11-06t08:49:37", 784_082_977_000.0),
            ("1994-11-6", 784_051_200_000.0),
            ("Nov 6, 1994", 784_051_200_000.0),
            (" 1994-11-06 ", 784_051_200_000.0),
            ("1994-06-1", 770_400_000_000.0),
        ] {
            assert_eq!(
                parse_with_local_offset(text, 480),
                expected,
                "Date.parse({text:?}) with TZ=+08:00"
            );
        }
    }

    #[test]
    fn parses_floats_like_number_parse_float() {
        assert_eq!(parse_float("2.5"), 2.5);
        assert_eq!(parse_float(" 2.5s"), 2.5);
        assert_eq!(parse_float("2"), 2.0);
        assert!(parse_float("abc").is_nan());
        assert!(parse_float("").is_nan());
        // 指数可以跟在只有整数部分或只有小数部分的尾数之后。
        assert_eq!(parse_float(".5e3"), 500.0);
        assert_eq!(parse_float("5.e3"), 5000.0);
        assert_eq!(parse_float("1e"), 1.0);
        assert_eq!(parse_float("0x10"), 0.0);
        // JS 的前导空白包含 U+FEFF。
        assert_eq!(parse_float("\u{feff}2.5s"), 2.5);
        // `Infinity` 字面量区分大小写，可带符号。
        assert_eq!(parse_float("+Infinity"), f64::INFINITY);
        assert_eq!(parse_float("-Infinityx"), f64::NEG_INFINITY);
        assert!(parse_float("infinity").is_nan());
    }

    #[test]
    fn exponential_fallback_grows_to_eight_seconds() {
        let error = error_with(Some(500), vec![], "boom");
        // `min(0.5 * 2^index, 8) * 1000` 之后再乘 `(1 - rand * 0.25)`：区间是 [0.75 * base, base]。
        for index in 0..5 {
            let base = (0.5 * 2f64.powi(index.min(4) as i32)).min(8.0) * 1000.0;
            let delay = get_retry_delay_ms(&error, index, Some(0)).expect("fallback");
            assert!(
                (base * 0.75..=base).contains(&delay),
                "index {index}: {delay} not in [{}, {base}]",
                base * 0.75
            );
        }
        let delay = get_retry_delay_ms(&error, 10, Some(0)).expect("fallback");
        assert!((6_000.0..=8_000.0).contains(&delay));
    }
}
