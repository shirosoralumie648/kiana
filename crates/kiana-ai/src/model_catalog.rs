//! model-catalog.ts + `packages/ai/scripts/**` + 仓库根 `scripts/model-catalog-protocol.ts`。
//!
//! TS 侧这些模块都**不是运行时依赖**：`model-catalog.ts` 只把生成的 `data/<provider>.json`
//! 摊平成 chat/image/classifier 三张表；`scripts/*.ts` 是目录生成器（`generate-models.ts`）、
//! 校验器（`check-model-data.ts`）、hydration（`hydrate-model-catalog.ts`）与若干纯函数
//! （`model-data.ts`、`openrouter-catalog.ts`、`ai-gateway-pricing.ts`、
//! `models-dev-reasoning-options.ts`、`openrouter-reasoning-options.ts`）；仓库根的
//! `scripts/model-catalog-protocol.ts` 是 kiana 与 kiana.dev 共享的目录分发协议。
//!
//! Rust 侧把它们收在本文件（`ARCHITECTURE.md` §5 的 `model_catalog.rs` 一行 + 本单元的
//! planner 约定），并保持稳定的错误文本与 JSON 字节（两者都是外部契约）：
//!
//! - [`flatten`]：`model-catalog.ts`。
//! - [`reasoning_options`]：`models-dev-reasoning-options.ts` / `openrouter-reasoning-options.ts`。
//! - [`gateway_pricing`]：`ai-gateway-pricing.ts`（`getAiGatewayCost`）。
//! - [`openrouter_catalog`]：`openrouter-catalog.ts`（`buildOpenRouterCatalog`）。
//! - [`model_data`]：`model-data.ts`（schema 版本、结构哈希、manifest、目录校验）。
//! - [`hydrate`]：`hydrate-model-catalog.ts`（`hydrateModelCatalog`）。
//! - [`check`]：`check-model-data.ts`（`check:model-data` 门禁的两行输出）。
//! - [`protocol`]：`scripts/model-catalog-protocol.ts`（键、版本比较、请求解析、revision 选择）。
//! - [`generator`]：`generate-models.ts` 的目录生成管线（严格校验 + 落盘/JSON 输出）。
//!
//! 关键约束见 `spec/ai-core.md` §2.4 与 §8：
//! - JS 对象键序（`IndexMap` + 「已存在键原位替换、新键追加」的赋值语义）；
//! - `JSON.stringify` 的数字格式化与键序（统一走 `kiana_json`）；
//! - `localeCompare`（`generate-models.ts` 的 JSON 输出键序、协议里的 prerelease 比较）固定为
//!   Node 默认 ICU 的 `en-US` 排序（[`locale_compare`]）；
//! - `Date.parse` / `Date.toISOString`（manifest 的 `generatedAt`）。
//!
//! 唯一有意偏差：TS 顶层 `catch` 用 `console.error(error)` 打印 `Error` 对象（含 stack），
//! Rust 侧只打印 `Display`（消息文本）。测试断言的都是 `error.message` 子串。
//!
//! # 为什么这些 TS 工具都放在一个文件里
//!
//! 这些模块大多不构成运行期依赖，但并非彼此孤立：生成/水合/校验脚本会导入 `model-data`
//! 等纯函数模块。它们共享同一组「与 Node/V8 逐字节对齐」的前提（对象键序、数字格式化、
//! UTF-16 排序、`Date.parse`），因此放在一起便于复用。本文件用内联 `mod` 分区，大体按
//! TS 文件拆分（[`reasoning_options`] 合并了 `models-dev-reasoning-options.ts` 与
//! `openrouter-reasoning-options.ts` 两个文件）。子模块与父模块同处一棵模块树，可以直接
//! 使用父模块的私有助手，因此仅把子模块拆成 `src/model_catalog/*.rs` 文件不会改变可见性；
//! 只有把它们提升为 `model_catalog` 之外的同级顶层模块时，才需要把 `collation_weights`
//! 这类共享助手改成 `pub(crate)`。
//!
//! # 两类使用者（边界不同）
//!
//! 1. **运行期共享助手**：`js_to_fixed`、`locale_compare`、`js_date_parse`、`sha256_hex`、
//!    `now_epoch_millis` 被 durable、session、session-core、interactive、builtin-extensions
//!    等 crate 直接调用；改动它们等于改动这些 crate 的显示、排序与时间解释语义。
//!    `js_parse_float`、`round_cost` 只由本 crate 的 `providers/openrouter_api.rs` 与
//!    生成器使用，不属于跨 crate 的共享面。
//! 2. **目录工具链**：`flatten`、`reasoning_options`、`gateway_pricing`、`model_data`、
//!    `openrouter_catalog`、`protocol`、`hydrate`、`check`、`generator`。仓库内目前的直接
//!    调用方只有 `crates/kiana-ai/tests/*`；运行期的模型表由 `build.rs` 在编译期从
//!    `crates/kiana-ai/data/` 生成（见 `src/providers/catalog.rs`）。两者是**两套独立实现**，
//!    只共享数据文件格式与 `spec/ai-core.md` §2.4 的约定；改目录工具链不会自动改变运行期表
//!    （第 1 类的 JS 助手除外，它们本来就是运行期代码）。
//!
//! # 生成管线（`generator::generate_models`）
//!
//! ```text
//! GeneratorInputs（调用方已取回的 JSON，TS 里是 fetch 的结果）
//!   -> build_all_models     合并各来源、补手工模型、应用临时覆盖
//!   -> generate_catalog     逐模型跑 metadata 应用器（顺序固定），按 provider 分组
//!   -> serialize + validate 写 staging -> validateModelDataDirectory -> rename 换入
//!   -> stdout 进度行 + 统计
//! ```
//!
//! 数据文件与 manifest 先写进 `providers/` 下的 staging，校验通过后才 rename 换入正式目录；
//! `*.models.ts` 分片与聚合器是原地重写，失败时用**写盘前**快照恢复；`--json-output` 则先
//! 删除目标目录再直接写目标位置（见 `generator::generate_models_after_catalog`）。
//!
//! # 注意事项
//!
//! - 错误文本是契约：`CatalogError` 的 `Display` 必须逐字等于 TS 的 `error.message`
//!   （测试断言子串，`check` 门禁原样打到 stderr）。构造错误时不要加前缀。
//! - 环境相关行为：无时区的 `js_date_parse` 走进程本地时区（libc `mktime`），
//!   `locale_compare` 固定为 Node 默认 ICU 的 `en-US` 顺序。需要确定性时改用
//!   [`js_date_parse_with_offset`]。
//! - `crates/kiana-ai/data/` 下的 JSON 不手改：它们由上游 `generate-models.ts` 生成，
//!   必须与 `.manifest.json` 的 sha256 和 structureHash 一致。
//!
//! # 阅读顺序
//!
//! 1. 先读本文件开头的 JS 语义助手：`gateway_pricing`、`model_data`、`openrouter_catalog`、
//!    `protocol`、`generator` 都会调用它们；`flatten`、`reasoning_options`、`hydrate`、
//!    `check` 不使用这些助手，只依赖错误类型与 `model_data` 等其余子模块。
//! 2. [`flatten`] -> [`model_data`] -> [`generator`]：数据文件从摊平、校验到生成的顺序。
//! 3. [`protocol`]、[`hydrate`]、[`check`]：分发协议、离线重放与 CI 门禁。
//! 4. `generator` 内部按 `build_all_models` -> `generate_catalog` ->
//!    `generate_models_after_catalog` 读。

use num_bigint::BigUint;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::cmp::Ordering;

use crate::error::{AiError, AiErrorKind};
use crate::types::{ModelThinkingLevel, ThinkingLevelMap};

// ---------------------------------------------------------------------------
// JS 语义助手（localeCompare / toFixed / Date）
// ---------------------------------------------------------------------------

// 这些函数被多个 crate 当运行期工具使用，不只是目录工具链。它们看起来可以用标准库
// “更简单地”重写，但 `str::trim`、`format!` 的小数格式、按字节序排序等常见写法都与
// JS 语义不同（U+FEFF / U+0085、半数取偶、UTF-16 码元序），替换会破坏逐字节一致。

/// `Intl.Collator` 的 `en-US` 主权重顺序（可打印 ASCII，来自 Node 26 的实测序列）。
///
/// Node 的默认 ICU 把标点、数字、字母按固定的主权重排序（`" _-,;:!?.'\"()[]{}@*/\\&#%`^+<=>|~$"`、
/// 数字、字母）。`generate-models.ts` 的 JSON 输出键序与 `model-catalog-protocol.ts` 的
/// prerelease 比较都走 `String.prototype.localeCompare`，默认区域是 Node 进程的 locale；本移植
/// 固定为 `en-US`（ASCII 模型 id 在 `zh-CN`/`en-US` 下顺序相同，见 `tests/fixtures/catalog-tooling`）。
///
/// TS: `examples 见 generate-models.ts:3440, model-catalog-protocol.ts:compareKianaVersions`。
const ASCII_COLLATION_ORDER: &str = " _-,;:!?.'\"()[]{}@*/\\&#%`^+<=>|~$0123456789aAbBcCdDeEfFgGhHiIjJkKlLmMnNoOpPqQrRsStTuUvVwWxXyYzZ";

/// 一个字符的 ICU 排序键：一次（字母/数字/标点）、二次（变音符）、三次（大小写）。
#[derive(Clone, Copy, PartialEq, Eq)]
struct CollationWeights {
    primary: u32,
    secondary: u32,
    tertiary: u32,
}

/// 拉丁字母（Latin-1 Supplement + Latin Extended-A）去掉变音符后的基础字母。
///
/// ICU 把 `é` 的一次权重等同 `e`、变音符放到二次权重，所以 `"é".localeCompare("f") < 0`、
/// `"Ö".localeCompare("z") < 0`。这里给出等价的分解表；未列出的字符回退到
/// `1000 + lowercase 码点`（排在全部 ASCII 之后）。
fn latin_base_letter(c: char) -> Option<(char, u32)> {
    let rank = 1 + (c as u32).wrapping_sub(0x00C0) % 0x100;
    let base = match c {
        'à'..='å' | 'ā' | 'ă' | 'ą' => 'a',
        'æ' => 'a',
        'ç' | 'ć' | 'ĉ' | 'ċ' | 'č' => 'c',
        'ď' | 'đ' | 'ð' => 'd',
        'è'..='ë' | 'ē' | 'ĕ' | 'ė' | 'ę' | 'ě' => 'e',
        'ĝ' | 'ğ' | 'ġ' | 'ģ' => 'g',
        'ĥ' | 'ħ' => 'h',
        'ì'..='ï' | 'ĩ' | 'ī' | 'ĭ' | 'į' | 'ı' => 'i',
        'ĵ' => 'j',
        'ķ' | 'ĸ' => 'k',
        'ĺ' | 'ļ' | 'ľ' | 'ŀ' | 'ł' => 'l',
        'ñ' | 'ń' | 'ņ' | 'ň' | 'ŉ' | 'ŋ' => 'n',
        'ò'..='ö' | 'ø' | 'ō' | 'ŏ' | 'ő' => 'o',
        'œ' => 'o',
        'ŕ' | 'ŗ' | 'ř' => 'r',
        'ś' | 'ŝ' | 'ş' | 'š' | 'ſ' => 's',
        'ß' => 's',
        'ţ' | 'ť' | 'ŧ' => 't',
        'þ' => 't',
        'ù'..='ü' | 'ũ' | 'ū' | 'ŭ' | 'ů' | 'ű' | 'ų' => 'u',
        'ŵ' => 'w',
        'ý' | 'ÿ' | 'ŷ' => 'y',
        'ź' | 'ż' | 'ž' => 'z',
        'ĳ' => 'i',
        _ => return None,
    };
    Some((base, rank))
}

/// 字符的三层排序键（见 [`CollationWeights`]）。
fn collation_weights(c: char) -> CollationWeights {
    let lower = c.to_lowercase().next().unwrap_or(c);
    let tertiary = u32::from(c.is_uppercase());
    if let Some(index) = ASCII_COLLATION_ORDER.find(lower) {
        return CollationWeights {
            primary: index as u32,
            secondary: 0,
            tertiary,
        };
    }
    if let Some((base, rank)) = latin_base_letter(lower)
        && let Some(index) = ASCII_COLLATION_ORDER.find(base)
    {
        return CollationWeights {
            primary: index as u32,
            secondary: rank,
            tertiary,
        };
    }
    CollationWeights {
        primary: 1000 + (lower as u32),
        secondary: 0,
        tertiary,
    }
}

/// `String.prototype.localeCompare` 的 `en-US` 等价物。
///
/// 与 ICU 一样分三层比较：一次权重（字母/数字/标点，忽略变音符与大小写）→ 二次权重
/// （变音符）→ 三次权重（小写先于大写）。ASCII 主权重表 `ASCII_COLLATION_ORDER` 来自
/// Node 26 的实测序列；拉丁扩展字母按基础字母参与一次权重（见 `latin_base_letter`）。
pub fn locale_compare(left: &str, right: &str) -> Ordering {
    let left_keys: Vec<CollationWeights> = left.chars().map(collation_weights).collect();
    let right_keys: Vec<CollationWeights> = right.chars().map(collation_weights).collect();
    let shared = left_keys.len().min(right_keys.len());
    for index in 0..shared {
        if left_keys[index].primary != right_keys[index].primary {
            return left_keys[index].primary.cmp(&right_keys[index].primary);
        }
    }
    if left_keys.len() != right_keys.len() {
        return left_keys.len().cmp(&right_keys.len());
    }
    for index in 0..shared {
        if left_keys[index].secondary != right_keys[index].secondary {
            return left_keys[index].secondary.cmp(&right_keys[index].secondary);
        }
    }
    for index in 0..shared {
        if left_keys[index].tertiary != right_keys[index].tertiary {
            return left_keys[index].tertiary.cmp(&right_keys[index].tertiary);
        }
    }
    Ordering::Equal
}

/// `Number.prototype.toFixed(digits)`（`|x| < 1e21`），返回字符串。
///
/// 用 f64 的精确十进制展开做「四舍五入、逢半远离零」，与 V8 的实现一致；`format!("{:.6}")` 的
/// 半数取偶在 `0.0078125` 这类可精确表示的值上会不同，所以不能直接用。
pub fn js_to_fixed(value: f64, digits: u32) -> String {
    if value.is_nan() {
        return "NaN".to_string();
    }
    if value.is_infinite() {
        // `Number.prototype.toFixed`：`(-Infinity).toFixed(6) === "-Infinity"`。
        return if value < 0.0 {
            "-Infinity".to_string()
        } else {
            "Infinity".to_string()
        };
    }
    // 规范：`if (x < 0) { s = "-"; x = -x }`——`-0` 不算负数（`(-0).toFixed(6) === "0.000000"`），
    // 但 `-1e-7` 四舍五入到 0 时仍保留负号（`"-0.000000"`）。
    let negative = value < 0.0;
    let magnitude = value.abs();
    if magnitude >= 1e21 {
        return kiana_json::js_num::format_js_number(value);
    }

    // 精确展开：|x| = mantissa * 2^exponent。
    let bits = magnitude.to_bits();
    let raw_exponent = ((bits >> 52) & 0x7ff) as i32;
    let fraction = bits & ((1u64 << 52) - 1);
    let (mantissa, exponent) = if raw_exponent == 0 {
        (fraction, -1074i32)
    } else {
        (fraction | (1u64 << 52), raw_exponent - 1075)
    };

    // value = numerator / 10^(scale) 的精确形式。
    let (mut numerator, scale) = if exponent >= 0 {
        (BigUint::from(mantissa) << (exponent as usize), 0u32)
    } else {
        let power = (-exponent) as u32;
        (
            BigUint::from(mantissa) * BigUint::from(5u32).pow(power),
            power,
        )
    };

    if scale <= digits {
        numerator *= BigUint::from(10u32).pow(digits - scale);
    } else {
        let divisor = BigUint::from(10u32).pow(scale - digits);
        let quotient = &numerator / &divisor;
        let remainder = &numerator % &divisor;
        // 半数远离零：2 * remainder >= divisor 时进位。
        numerator = if &remainder * 2u32 >= divisor {
            quotient + 1u32
        } else {
            quotient
        };
    }

    let mut text = numerator.to_str_radix(10);
    if digits > 0 {
        while text.len() <= digits as usize {
            text.insert(0, '0');
        }
        let split = text.len() - digits as usize;
        text.insert(split, '.');
    }
    // `toFixed` 在 `x < 0` 时总是加负号（即使四舍五入后为 0）。
    if negative {
        text.insert(0, '-');
    }
    text
}

/// `Number((value).toFixed(6))`（`generate-models.ts` 的 `roundCost`）。
pub fn round_cost(value: f64) -> f64 {
    js_to_fixed(value, 6).parse::<f64>().unwrap_or(f64::NAN)
}

/// `Number.parseFloat(value)`：前导空白后取最长的十进制/指数前缀；无有效数字为 `NaN`。
///
/// `"Infinity"`（可带符号、区分大小写）单独识别——`f64::from_str` 也认它，但下面的
/// 数字扫描会先把它当成"无有效数字"，所以必须在扫描前处理。
pub fn js_parse_float(text: &str) -> f64 {
    let trimmed = text.trim_start_matches(|c: char| {
        matches!(
            c,
            '\t' | '\n' | '\u{b}' | '\u{c}' | '\r' | ' ' | '\u{a0}' | '\u{1680}' | '\u{2000}'
                ..='\u{200a}'
                    | '\u{2028}'
                    | '\u{2029}'
                    | '\u{202f}'
                    | '\u{205f}'
                    | '\u{3000}'
                    | '\u{feff}'
        )
    });
    // `Number.parseFloat` 的 `StrNumericLiteral`：`Infinity` 是唯一的非数字字面量。
    if let Some(rest) = trimmed.strip_prefix('-') {
        if rest.starts_with("Infinity") {
            return f64::NEG_INFINITY;
        }
    } else if let Some(rest) = trimmed.strip_prefix('+') {
        if rest.starts_with("Infinity") {
            return f64::INFINITY;
        }
    } else if trimmed.starts_with("Infinity") {
        return f64::INFINITY;
    }
    let bytes = trimmed.as_bytes();
    let mut index = 0usize;
    let mut seen_digits = false;
    if index < bytes.len() && (bytes[index] == b'+' || bytes[index] == b'-') {
        index += 1;
    }
    let mut seen_dot = false;
    while index < bytes.len() {
        match bytes[index] {
            b'0'..=b'9' => {
                seen_digits = true;
                index += 1;
            }
            b'.' if !seen_dot => {
                seen_dot = true;
                index += 1;
            }
            _ => break,
        }
    }
    if !seen_digits {
        return f64::NAN;
    }
    let mantissa_end = index;
    // 指数部分只有在 `e`/`E` 后至少还有一位数字时才并入；否则只解析尾数前缀
    // （`parseFloat("1e") === 1`），残余字符被忽略。
    if index < bytes.len() && (bytes[index] == b'e' || bytes[index] == b'E') {
        let mut exponent_end = index + 1;
        if exponent_end < bytes.len()
            && (bytes[exponent_end] == b'+' || bytes[exponent_end] == b'-')
        {
            exponent_end += 1;
        }
        let digit_start = exponent_end;
        while exponent_end < bytes.len() && bytes[exponent_end].is_ascii_digit() {
            exponent_end += 1;
        }
        if exponent_end > digit_start {
            let literal = &trimmed[..exponent_end];
            return literal.parse::<f64>().unwrap_or(f64::NAN);
        }
    }
    trimmed[..mantissa_end].parse::<f64>().unwrap_or(f64::NAN)
}

fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    // Howard Hinnant 的 days_from_civil。
    let y = if month <= 2 { year - 1 } else { year };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}

fn floor_div(a: i64, b: i64) -> i64 {
    let q = a / b;
    if (a % b != 0) && ((a < 0) != (b < 0)) {
        q - 1
    } else {
        q
    }
}

/// `Date.parse(text)`：V8 `DateParser` 的逐行移植（ES5 日期时间串 + 传统解析器）。
///
/// V8 先尝试 ES5 ISO-8601 形式（`[±yyyyyy|yyyy][-MM[-DD]][THH:mm[:ss[.sss]][Z|±hh:mm]]`），
/// 不完整或失败时回落到 Safari 兼容的传统解析器（月份名、`am`/`pm`、时区名、`±hhmm` 等）。
/// 两者都失败返回 `None`（JS 的 `NaN`）；成功返回 `TimeClip` 之后的毫秒值。
///
/// 时区缺省时 V8 按**本地时区**解释（`DateCache::ToUTC`）；本移植通过 libc `mktime`
/// 取进程本地偏移。需要确定性的场景（测试语料）用 [`js_date_parse_with_offset`]。
///
/// TS: `model-data.ts:validateModelDataDirectory`（`Number.isNaN(Date.parse(generatedAt))`）。
pub fn js_date_parse(text: &str) -> Option<f64> {
    DateParser::new(text, LocalTimeMode::System).parse()
}

/// [`js_date_parse`] 的确定性变体：无时区的日期时间按固定本地偏移（毫秒，东为正）解释。
///
/// `None` 表示 UTC。用于测试与需要与 `TZ=UTC` 的 Node 逐字节对齐的场景（`TZ=UTC` 的
/// 语料见 `tests/fixtures/catalog-tooling/date-parse.json`）。
pub fn js_date_parse_with_offset(text: &str, offset_millis: Option<i32>) -> Option<f64> {
    DateParser::new(text, LocalTimeMode::Fixed(offset_millis.unwrap_or(0))).parse()
}

/// 无时区时间的解释方式。
#[derive(Clone, Copy)]
enum LocalTimeMode {
    /// 进程本地时区（V8 `DateCache::ToUTC`）。
    System,
    /// 固定偏移（毫秒，东为正）。
    Fixed(i32),
}

/// `DateParser` 的关键词类别（含 V8 的 `INVALID` 哨兵）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum KeywordType {
    Invalid,
    MonthName,
    TimeZoneName,
    TimeSeparator,
    AmPm,
}

/// `DateToken`。
#[derive(Debug, Clone, Copy, PartialEq)]
enum DateToken {
    /// 关键词：`(type, value, length)`。
    Keyword(KeywordType, i32, i32),
    /// 无符号数字：`(value, length)`（`length` 是源码中的数字位数）。
    Number(i32, i32),
    Symbol(char),
    WhiteSpace,
    Unknown,
    EndOfInput,
    Invalid,
}

/// `DateParser::kMaxSignificantDigits`。
const MAX_SIGNIFICANT_DIGITS: i32 = 9;
/// V8 `kMaxTimeInMs = 864000000 * 10000000`（±8.64e15）。
const MAX_TIME_IN_MS: f64 = 864_000_000.0 * 10_000_000.0;
/// V8 `kMaxTimeBeforeUTCInMs = kMaxTimeInMs + kMsPerMonth`。
const MAX_TIME_BEFORE_UTC_IN_MS: f64 = MAX_TIME_IN_MS + 86_400_000.0 * 30.0;

/// V8 `DateCache::ToUTC`：把本地墙上时间（年月日 + 日内毫秒）换算成 UTC 偏移毫秒。
///
/// V8 在 Linux 上通过 OS 的 `localtime`/`mktime` 规则取偏移（含 DST）；这里用 libc
/// `mktime`（`tm_isdst = -1` 表示由系统判定）。
fn system_local_offset_millis(
    year: i32,
    month0: i32,
    month_day: i32,
    time_millis: f64,
) -> Option<f64> {
    let whole_seconds = (time_millis / 1000.0).floor();
    let millis = time_millis - whole_seconds * 1000.0;
    let seconds = whole_seconds as i64;
    // SAFETY: `libc::tm` 是 POD；全零是合法的初值，其余字段随后逐个写入。
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    tm.tm_year = year - 1900;
    tm.tm_mon = month0;
    tm.tm_mday = month_day;
    tm.tm_hour = (seconds / 3600) as i32;
    tm.tm_min = ((seconds % 3600) / 60) as i32;
    tm.tm_sec = (seconds % 60) as i32;
    tm.tm_isdst = -1;
    // SAFETY: `tm` 指向上面构造好的有效 `libc::tm`，`mktime` 只读写该结构。
    let local_seconds = unsafe { libc::mktime(&mut tm) };
    // `mktime` 的 -1 既可能是错误，也可能是 1969-12-31T23:59:59 这一秒（时间戳 -1）。
    // 后者在 V8 的 ±8.64e15 范围内是合法值，因此不把 -1 当作错误。
    let wall_as_utc_millis =
        days_from_civil(i64::from(year), i64::from(month0) + 1, i64::from(month_day)) as f64
            * 86_400_000.0
            + time_millis;
    // `date -= offset_millis` 需要的是「墙钟时间 - UTC」的正偏移（东为正）。
    Some(wall_as_utc_millis - (local_seconds as f64 * 1000.0 + millis))
}

/// `IsWhiteSpace`（`\u0009 \u000b \u000c    ` + Unicode `Zs`）。
fn is_date_whitespace(unit: u16) -> bool {
    matches!(
        unit,
        0x0009 | 0x000B | 0x000C | 0x0020 | 0x00A0 | 0x1680 | 0x202F | 0x205F | 0x3000 | 0xFEFF
    ) || (0x2000..=0x200A).contains(&unit)
}

/// `IsWhiteSpaceOrLineTerminator`。
fn is_date_whitespace_or_line_terminator(unit: u16) -> bool {
    is_date_whitespace(unit) || matches!(unit, 0x000A | 0x000D | 0x2028 | 0x2029)
}

/// `KeywordTable::array`：`(prefix[3], type, value)`。
const KEYWORD_TABLE: [([u8; 3], KeywordType, i32); 27] = [
    (*b"jan", KeywordType::MonthName, 1),
    (*b"feb", KeywordType::MonthName, 2),
    (*b"mar", KeywordType::MonthName, 3),
    (*b"apr", KeywordType::MonthName, 4),
    (*b"may", KeywordType::MonthName, 5),
    (*b"jun", KeywordType::MonthName, 6),
    (*b"jul", KeywordType::MonthName, 7),
    (*b"aug", KeywordType::MonthName, 8),
    (*b"sep", KeywordType::MonthName, 9),
    (*b"oct", KeywordType::MonthName, 10),
    (*b"nov", KeywordType::MonthName, 11),
    (*b"dec", KeywordType::MonthName, 12),
    (*b"am\0", KeywordType::AmPm, 0),
    (*b"pm\0", KeywordType::AmPm, 12),
    (*b"ut\0", KeywordType::TimeZoneName, 0),
    (*b"utc", KeywordType::TimeZoneName, 0),
    (*b"z\0\0", KeywordType::TimeZoneName, 0),
    (*b"gmt", KeywordType::TimeZoneName, 0),
    (*b"cdt", KeywordType::TimeZoneName, -5),
    (*b"cst", KeywordType::TimeZoneName, -6),
    (*b"edt", KeywordType::TimeZoneName, -4),
    (*b"est", KeywordType::TimeZoneName, -5),
    (*b"mdt", KeywordType::TimeZoneName, -6),
    (*b"mst", KeywordType::TimeZoneName, -7),
    (*b"pdt", KeywordType::TimeZoneName, -7),
    (*b"pst", KeywordType::TimeZoneName, -8),
    (*b"t\0\0", KeywordType::TimeSeparator, 0),
];

/// `KeywordTable::Lookup(pre, len)`：命中返回该条目，未命中返回 `Invalid`。
fn keyword_lookup(prefix: [u32; 3], length: i32) -> (KeywordType, i32) {
    for (entry, keyword_type, value) in KEYWORD_TABLE {
        let matches_prefix = (0..3).all(|index| prefix[index] == u32::from(entry[index]));
        // 比关键词更长的词只允许用于月份名。
        if matches_prefix && (length <= 3 || keyword_type == KeywordType::MonthName) {
            return (keyword_type, value);
        }
    }
    (KeywordType::Invalid, 0)
}

/// `DayComposer`。
#[derive(Default)]
struct DayComposer {
    comp: [i32; 3],
    index: usize,
    named_month: Option<i32>,
    is_iso_date: bool,
}

impl DayComposer {
    fn is_empty(&self) -> bool {
        self.index == 0
    }

    fn add(&mut self, value: i32) -> bool {
        if self.index < 3 {
            self.comp[self.index] = value;
            self.index += 1;
            true
        } else {
            false
        }
    }

    fn is_month(value: i32) -> bool {
        (1..=12).contains(&value)
    }

    fn is_day(value: i32) -> bool {
        (1..=31).contains(&value)
    }

    /// `DayComposer::Write`；返回 `(year, month0, day)`。
    fn write(&self) -> Option<(i32, i32, i32)> {
        if self.index < 1 {
            return None;
        }
        let mut comp = self.comp;
        // 补位到 3 项；V8 的 `index_` 随之推进到 kSize，后续分支都按「已补满」判断。
        let mut index = self.index;
        while index < 3 {
            comp[index] = 1;
            index += 1;
        }

        // 缺省年 0（=> 2000），与 KJS 兼容。
        let (mut year, month, day) = if let Some(named_month) = self.named_month {
            if !Self::is_day(comp[0]) {
                // YMD, MYD, or YDM
                (comp[0], named_month, comp[1])
            } else {
                // DMY, MDY, or DYM
                (comp[1], named_month, comp[0])
            }
        } else if self.is_iso_date || !Self::is_day(comp[0]) {
            // YMD
            (comp[0], comp[1], comp[2])
        } else {
            // MD(Y)
            (comp[2], comp[0], comp[1])
        };

        if !self.is_iso_date {
            if (0..=49).contains(&year) {
                year += 2000;
            } else if (50..=99).contains(&year) {
                year += 1900;
            }
        }

        if !Self::is_month(month) || !Self::is_day(day) {
            return None;
        }
        Some((year, month - 1, day))
    }
}

/// `TimeComposer`。
#[derive(Default)]
struct TimeComposer {
    comp: [i32; 4],
    index: usize,
    hour_offset: Option<i32>,
}

impl TimeComposer {
    fn is_empty(&self) -> bool {
        self.index == 0
    }

    fn is_expecting(&self, value: i32) -> bool {
        (self.index == 1 && Self::is_minute(value))
            || (self.index == 2 && Self::is_second(value))
            || (self.index == 3 && Self::is_millisecond(value))
    }

    fn add(&mut self, value: i32) -> bool {
        if self.index < 4 {
            self.comp[self.index] = value;
            self.index += 1;
            true
        } else {
            false
        }
    }

    fn add_final(&mut self, value: i32) -> bool {
        if !self.add(value) {
            return false;
        }
        while self.index < 4 {
            self.comp[self.index] = 0;
            self.index += 1;
        }
        true
    }

    fn set_hour_offset(&mut self, value: i32) {
        self.hour_offset = Some(value);
    }

    fn is_minute(value: i32) -> bool {
        (0..=59).contains(&value)
    }

    fn is_hour(value: i32) -> bool {
        (0..=23).contains(&value)
    }

    fn is_second(value: i32) -> bool {
        (0..=59).contains(&value)
    }

    fn is_hour12(value: i32) -> bool {
        (0..=12).contains(&value)
    }

    fn is_millisecond(value: i32) -> bool {
        (0..=999).contains(&value)
    }

    /// `TimeComposer::Write`；返回当日毫秒数。
    fn write(&self) -> Option<f64> {
        let mut comp = self.comp;
        for slot in comp.iter_mut().skip(self.index) {
            *slot = 0;
        }

        let mut hour = comp[0];
        let minute = comp[1];
        let second = comp[2];
        let millisecond = comp[3];

        if let Some(hour_offset) = self.hour_offset {
            if !Self::is_hour12(hour) {
                return None;
            }
            hour %= 12;
            hour += hour_offset;
        }

        if !Self::is_hour(hour)
            || !Self::is_minute(minute)
            || !Self::is_second(second)
            || !Self::is_millisecond(millisecond)
        {
            // 只有分/秒/毫秒全为 0 时才允许第 24 小时。
            if hour != 24 || minute != 0 || second != 0 || millisecond != 0 {
                return None;
            }
        }

        Some(
            f64::from(hour) * 3_600_000.0
                + f64::from(minute) * 60_000.0
                + f64::from(second) * 1000.0
                + f64::from(millisecond),
        )
    }
}

/// `TimeZoneComposer`（`None` 表示 V8 的 `kNone`）。
#[derive(Default)]
struct TimeZoneComposer {
    sign: Option<i32>,
    hour: Option<i32>,
    minute: Option<i32>,
}

impl TimeZoneComposer {
    fn set(&mut self, offset_in_hours: i32) {
        let sign = if offset_in_hours < 0 { -1 } else { 1 };
        self.sign = Some(sign);
        self.hour = Some(offset_in_hours * sign);
        self.minute = Some(0);
    }

    fn set_sign(&mut self, sign: i32) {
        self.sign = Some(if sign < 0 { -1 } else { 1 });
    }

    fn set_absolute_hour(&mut self, hour: i32) {
        self.hour = Some(hour);
    }

    fn set_absolute_minute(&mut self, minute: Option<i32>) {
        self.minute = minute;
    }

    fn is_expecting(&self, value: i32) -> bool {
        self.hour.is_some() && self.minute.is_none() && TimeComposer::is_minute(value)
    }

    fn is_utc(&self) -> bool {
        self.hour == Some(0) && self.minute == Some(0)
    }

    fn is_empty(&self) -> bool {
        self.hour.is_none()
    }

    /// `TimeZoneComposer::Write`；返回 UTC 偏移秒数（`None` = 未指定，JS 的 NaN）。
    fn write(&self) -> Option<i32> {
        let sign = self.sign?;
        let hour = self.hour.unwrap_or(0);
        let minute = self.minute.unwrap_or(0);
        // V8 用无符号运算避免有符号溢出。
        let total_seconds_unsigned = (hour as u32)
            .wrapping_mul(3600)
            .wrapping_add((minute as u32).wrapping_mul(60));
        if total_seconds_unsigned > i32::MAX as u32 {
            return None;
        }
        let total_seconds = total_seconds_unsigned as i32;
        Some(if sign < 0 {
            -total_seconds
        } else {
            total_seconds
        })
    }
}

/// `DateParser`：`InputReader` + `DateStringTokenizer`（单 token 预读）。
struct DateParser {
    /// UTF-16 码元（V8 对两字节字符串同样按码元读取）。
    input: Vec<u16>,
    index: usize,
    next: DateToken,
    /// 无时区日期时间的本地时区解释方式。
    local: LocalTimeMode,
}

impl DateParser {
    fn new(text: &str, local: LocalTimeMode) -> DateParser {
        let mut parser = DateParser {
            input: text.encode_utf16().collect(),
            index: 0,
            next: DateToken::EndOfInput,
            local,
        };
        parser.next = parser.scan();
        parser
    }

    /// 当前码元（越界为 `0`）。
    fn ch(&self) -> u16 {
        self.input.get(self.index).copied().unwrap_or(0)
    }

    fn step(&mut self) {
        self.index += 1;
    }

    fn skip_char(&mut self, unit: u16) -> bool {
        if self.ch() == unit {
            self.step();
            true
        } else {
            false
        }
    }

    fn is_end(&self) -> bool {
        self.ch() == 0
    }

    fn is_ascii_digit(&self) -> bool {
        (u16::from(b'0')..=u16::from(b'9')).contains(&self.ch())
    }

    /// `IsAsciiAlphaOrAbove`：`ch >= 'A'`。
    fn is_ascii_alpha_or_above(&self) -> bool {
        self.ch() >= u16::from(b'A')
    }

    fn is_white_space_char(&self) -> bool {
        is_date_whitespace(self.ch())
    }

    /// `ReadUnsignedNumeral`：跳前导 0，最多累计 9 位有效数字。
    fn read_unsigned_numeral(&mut self) -> i32 {
        let mut value: i32 = 0;
        let mut digits: i32 = 0;
        while self.ch() == u16::from(b'0') {
            self.step();
        }
        while self.is_ascii_digit() {
            if digits < MAX_SIGNIFICANT_DIGITS {
                value = value * 10 + i32::from(self.ch() - u16::from(b'0'));
            }
            digits += 1;
            self.step();
        }
        value
    }

    /// `ReadWord`：读取 `>= 'A'` 且非空白的字符，写入 3 字节小写前缀，返回 `(prefix, len)`。
    fn read_word(&mut self) -> ([u32; 3], i32) {
        let mut prefix = [0u32; 3];
        let mut length: i32 = 0;
        while self.is_ascii_alpha_or_above() && !self.is_white_space_char() {
            if length < 3 {
                prefix[length as usize] = u32::from(self.ch()) | 0x20;
            }
            self.step();
            length += 1;
        }
        (prefix, length)
    }

    /// `SkipWhiteSpace`。
    fn skip_white_space(&mut self) -> bool {
        if is_date_whitespace_or_line_terminator(self.ch()) {
            self.step();
            true
        } else {
            false
        }
    }

    /// `SkipParentheses`：跳过一层（可嵌套）括号。
    fn skip_parentheses(&mut self) -> bool {
        if self.ch() != u16::from(b'(') {
            return false;
        }
        let mut balance = 0i32;
        loop {
            if self.ch() == u16::from(b')') {
                balance -= 1;
            } else if self.ch() == u16::from(b'(') {
                balance += 1;
            }
            self.step();
            if balance <= 0 || self.ch() == 0 {
                break;
            }
        }
        true
    }

    /// `DateStringTokenizer::Scan`。
    fn scan(&mut self) -> DateToken {
        let pre_pos = self.index;
        if self.is_end() {
            return DateToken::EndOfInput;
        }
        if self.is_ascii_digit() {
            let value = self.read_unsigned_numeral();
            let length = (self.index - pre_pos) as i32;
            return DateToken::Number(value, length);
        }
        if self.skip_char(u16::from(b':')) {
            return DateToken::Symbol(':');
        }
        if self.skip_char(u16::from(b'-')) {
            return DateToken::Symbol('-');
        }
        if self.skip_char(u16::from(b'+')) {
            return DateToken::Symbol('+');
        }
        if self.skip_char(u16::from(b'.')) {
            return DateToken::Symbol('.');
        }
        if self.skip_char(u16::from(b')')) {
            return DateToken::Symbol(')');
        }
        if self.is_ascii_alpha_or_above() && !self.is_white_space_char() {
            let (prefix, length) = self.read_word();
            let (keyword_type, value) = keyword_lookup(prefix, length);
            return DateToken::Keyword(keyword_type, value, length);
        }
        if self.skip_white_space() {
            return DateToken::WhiteSpace;
        }
        if self.skip_parentheses() {
            return DateToken::Unknown;
        }
        self.step();
        DateToken::Unknown
    }

    /// `scanner.Peek()`。
    fn peek(&self) -> DateToken {
        self.next
    }

    /// `scanner.Next()`：返回当前预读 token 并扫描下一个。
    fn advance(&mut self) -> DateToken {
        let token = self.next;
        self.next = self.scan();
        token
    }

    /// `scanner.SkipSymbol(symbol)`。
    fn skip_symbol(&mut self, symbol: char) -> bool {
        if is_symbol(self.next, symbol) {
            self.next = self.scan();
            true
        } else {
            false
        }
    }

    /// `ReadMilliseconds`：取数字的前三位有效数字。
    fn read_milliseconds(token: DateToken) -> i32 {
        let DateToken::Number(mut number, mut length) = token else {
            return -1;
        };
        if length < 3 {
            if length == 1 {
                number *= 100;
            } else if length == 2 {
                number *= 10;
            }
        } else if length > 3 {
            if length > MAX_SIGNIFICANT_DIGITS {
                length = MAX_SIGNIFICANT_DIGITS;
            }
            let mut factor = 1i32;
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

    /// 完整的 `DateParser::Parse`。
    fn parse(mut self) -> Option<f64> {
        let mut tz = TimeZoneComposer::default();
        let mut time = TimeComposer::default();
        let mut day = DayComposer::default();

        let next_unhandled = self.parse_es5_date_time(&mut day, &mut time, &mut tz);
        if next_unhandled == DateToken::Invalid {
            return None;
        }
        let mut has_read_number = !day.is_empty();

        // 传统解析器。
        let mut token = next_unhandled;
        while token != DateToken::EndOfInput {
            match token {
                DateToken::Number(value, _) => {
                    has_read_number = true;
                    if self.skip_symbol(':') {
                        if self.skip_symbol(':') {
                            // n + "::"
                            if !time.is_empty() {
                                return None;
                            }
                            time.add(value);
                            time.add(0);
                        } else {
                            // n + ":"
                            if !time.add(value) {
                                return None;
                            }
                            if is_symbol(self.peek(), '.') {
                                self.advance();
                            }
                        }
                    } else if is_symbol(self.peek(), '.') && time.is_expecting(value) {
                        self.advance();
                        time.add(value);
                        if !matches!(self.peek(), DateToken::Number(..)) {
                            return None;
                        }
                        let milliseconds = Self::read_milliseconds(self.advance());
                        if milliseconds < 0 {
                            return None;
                        }
                        time.add_final(milliseconds);
                    } else if tz.is_expecting(value) {
                        tz.set_absolute_minute(Some(value));
                    } else if time.is_expecting(value) {
                        time.add_final(value);
                        // 时间收尾后必须紧跟结束、空白、"Z" 或正负号。
                        let peek = self.peek();
                        if !matches!(peek, DateToken::EndOfInput | DateToken::WhiteSpace)
                            && !is_keyword_z(peek)
                            && !is_ascii_sign(peek)
                        {
                            return None;
                        }
                    } else {
                        if !day.add(value) {
                            return None;
                        }
                        self.skip_symbol('-');
                    }
                }
                DateToken::Keyword(keyword_type, value, _) => {
                    if keyword_type == KeywordType::AmPm && !time.is_empty() {
                        time.set_hour_offset(value);
                    } else if keyword_type == KeywordType::MonthName {
                        day.named_month = Some(value);
                        self.skip_symbol('-');
                    } else if keyword_type == KeywordType::TimeZoneName && has_read_number {
                        tz.set(value);
                    } else {
                        // 读到数字之后，垃圾词非法。
                        if has_read_number {
                            return None;
                        }
                        // 第一个数字必须与垃圾词之间隔开。
                        if matches!(self.peek(), DateToken::Number(..)) {
                            return None;
                        }
                    }
                }
                DateToken::Symbol(symbol)
                    if (symbol == '+' || symbol == '-') && (tz.is_utc() || !time.is_empty()) =>
                {
                    // 只在 UTC 或时间之后解析 UTC 偏移。
                    tz.set_sign(if symbol == '+' { 1 } else { -1 });
                    // 后续数字可以为空。
                    let mut value = 0i32;
                    let mut length = 0i32;
                    if let DateToken::Number(found, digits) = self.advance() {
                        length = digits;
                        value = found;
                    }
                    has_read_number = true;

                    if is_symbol(self.peek(), ':') {
                        tz.set_absolute_hour(value);
                        tz.set_absolute_minute(None);
                    } else if length == 2 || length == 1 {
                        // GMT-8 这类。
                        tz.set_absolute_hour(value);
                        tz.set_absolute_minute(Some(0));
                    } else if length == 4 || length == 3 {
                        // hhmm。
                        tz.set_absolute_hour(value / 100);
                        tz.set_absolute_minute(Some(value % 100));
                    } else {
                        // 不接受 GMT-12345。
                        return None;
                    }
                }
                DateToken::Symbol(symbol)
                    if (symbol == '+' || symbol == '-') && has_read_number =>
                {
                    return None;
                }
                DateToken::Symbol(')') if has_read_number => return None,
                _ => {}
            }
            token = self.advance();
        }

        let (year, month0, month_day) = day.write()?;
        let time_millis = time.write()?;
        let offset_seconds = tz.write();

        // MakeDay：年份必须落在 ±1e6。
        if !(-1_000_000.0..=1_000_000.0).contains(&f64::from(year)) {
            return None;
        }
        let day_number =
            days_from_civil(i64::from(year), i64::from(month0) + 1, i64::from(month_day));
        let mut date = day_number as f64 * 86_400_000.0 + time_millis;
        match offset_seconds {
            // 未指定时区：ES5 纯日期形式已由 `parse_es5_date_time` 固定为 UTC；其余形式
            // （ES5 日期时间、传统格式）V8 用进程本地时区（`DateCache::ToUTC`）解释。
            None => {
                if !(-MAX_TIME_BEFORE_UTC_IN_MS..=MAX_TIME_BEFORE_UTC_IN_MS).contains(&date) {
                    return None;
                }
                let offset_millis = match self.local {
                    LocalTimeMode::System => {
                        system_local_offset_millis(year, month0, month_day, time_millis)?
                    }
                    LocalTimeMode::Fixed(offset) => f64::from(offset),
                };
                date -= offset_millis;
            }
            Some(offset) => date -= f64::from(offset) * 1000.0,
        }
        // `DateCache::TryTimeClip`。
        if !(-MAX_TIME_IN_MS..=MAX_TIME_IN_MS).contains(&date) {
            return None;
        }
        Some(if date > 0.0 {
            date.floor()
        } else {
            date.ceil()
        })
    }

    /// `ParseES5DateTime`：返回首个未处理的 token（`Invalid` / `EndOfInput` / 已消费的 token）。
    fn parse_es5_date_time(
        &mut self,
        day: &mut DayComposer,
        time: &mut TimeComposer,
        tz: &mut TimeZoneComposer,
    ) -> DateToken {
        // 必选日期段：[('-'|'+')yy]yyyy[':'MM[':'DD]]。
        if is_ascii_sign(self.peek()) {
            // 保留符号 token，必要时交给传统解析器。
            let sign_token = self.advance();
            if !is_fixed_length_number(self.peek(), 6) {
                return sign_token;
            }
            let sign = if sign_token == DateToken::Symbol('+') {
                1
            } else {
                -1
            };
            let year = token_number(self.advance());
            if sign < 0 && year == 0 {
                return sign_token;
            }
            day.add(sign * year);
        } else if is_fixed_length_number(self.peek(), 4) {
            let year = token_number(self.advance());
            day.add(year);
        } else {
            return self.advance();
        }
        if self.skip_symbol('-') {
            if !is_fixed_length_number(self.peek(), 2)
                || !DayComposer::is_month(token_number(self.peek()))
            {
                return self.advance();
            }
            let month = token_number(self.advance());
            day.add(month);
            if self.skip_symbol('-') {
                if !is_fixed_length_number(self.peek(), 2)
                    || !DayComposer::is_day(token_number(self.peek()))
                {
                    return self.advance();
                }
                let month_day = token_number(self.advance());
                day.add(month_day);
            }
        }
        // 可选时间段：'T'HH':'mm[':'ss['.'sss]]Z。
        if !matches!(
            self.peek(),
            DateToken::Keyword(KeywordType::TimeSeparator, _, _)
        ) {
            if self.peek() != DateToken::EndOfInput {
                return self.advance();
            }
        } else {
            // ES5 时间段存在。
            self.advance();
            if !is_fixed_length_number(self.peek(), 2)
                || !(0..=24).contains(&token_number(self.peek()))
            {
                return DateToken::Invalid;
            }
            // 允许 24:00[:00[.000]]，其他 24 点开头的时间不允许。
            let hour_is_24 = token_number(self.peek()) == 24;
            let hour = token_number(self.advance());
            time.add(hour);
            if !self.skip_symbol(':') {
                return DateToken::Invalid;
            }
            if !is_fixed_length_number(self.peek(), 2)
                || !TimeComposer::is_minute(token_number(self.peek()))
                || (hour_is_24 && token_number(self.peek()) > 0)
            {
                return DateToken::Invalid;
            }
            let minute = token_number(self.advance());
            time.add(minute);
            if self.skip_symbol(':') {
                if !is_fixed_length_number(self.peek(), 2)
                    || !TimeComposer::is_second(token_number(self.peek()))
                    || (hour_is_24 && token_number(self.peek()) > 0)
                {
                    return DateToken::Invalid;
                }
                let second = token_number(self.advance());
                time.add(second);
                if self.skip_symbol('.') {
                    if !matches!(self.peek(), DateToken::Number(..))
                        || (hour_is_24 && token_number(self.peek()) > 0)
                    {
                        return DateToken::Invalid;
                    }
                    // 允许多于或少于三位数字。
                    let fraction = self.advance();
                    time.add(Self::read_milliseconds(fraction));
                }
            }
            // 可选时区：'Z' | ('+'|'-')hh':'mm。
            if is_keyword_z(self.peek()) {
                self.advance();
                tz.set(0);
            } else if is_symbol(self.peek(), '+') || is_symbol(self.peek(), '-') {
                let sign = if self.advance() == DateToken::Symbol('+') {
                    1
                } else {
                    -1
                };
                tz.set_sign(sign);
                if is_fixed_length_number(self.peek(), 4) {
                    // hhmm 扩展语法。
                    let hourmin = token_number(self.advance());
                    let hour = hourmin / 100;
                    let minute = hourmin % 100;
                    if !TimeComposer::is_hour(hour) || !TimeComposer::is_minute(minute) {
                        return DateToken::Invalid;
                    }
                    tz.set_absolute_hour(hour);
                    tz.set_absolute_minute(Some(minute));
                } else {
                    // hh:mm 标准语法。
                    if !is_fixed_length_number(self.peek(), 2)
                        || !TimeComposer::is_hour(token_number(self.peek()))
                    {
                        return DateToken::Invalid;
                    }
                    let hour = token_number(self.advance());
                    tz.set_absolute_hour(hour);
                    if !self.skip_symbol(':') {
                        return DateToken::Invalid;
                    }
                    if !is_fixed_length_number(self.peek(), 2)
                        || !TimeComposer::is_minute(token_number(self.peek()))
                    {
                        return DateToken::Invalid;
                    }
                    let minute = token_number(self.advance());
                    tz.set_absolute_minute(Some(minute));
                }
            }
            if self.peek() != DateToken::EndOfInput {
                return DateToken::Invalid;
            }
        }
        // 纯日期形式按 UTC 解释（时间段缺省时时区也缺省）。
        if tz.is_empty() && time.is_empty() {
            tz.set(0);
        }
        day.is_iso_date = true;
        DateToken::EndOfInput
    }
}

/// `DateToken::IsSymbol(symbol)`。
fn is_symbol(token: DateToken, symbol: char) -> bool {
    matches!(token, DateToken::Symbol(found) if found == symbol)
}

/// `DateToken::IsAsciiSign()`。
fn is_ascii_sign(token: DateToken) -> bool {
    matches!(token, DateToken::Symbol('+') | DateToken::Symbol('-'))
}

/// `DateToken::IsKeywordZ()`：`TIME_ZONE_NAME && length == 1 && value == 0`（即单词 `z`）。
fn is_keyword_z(token: DateToken) -> bool {
    matches!(token, DateToken::Keyword(KeywordType::TimeZoneName, 0, 1))
}

/// `DateToken::IsFixedLengthNumber(length)`。
fn is_fixed_length_number(token: DateToken, length: i32) -> bool {
    matches!(token, DateToken::Number(_, found) if found == length)
}

/// `DateToken::number()`。
fn token_number(token: DateToken) -> i32 {
    match token {
        DateToken::Number(value, _) => value,
        _ => 0,
    }
}

/// `new Date(ms).toISOString()`：毫秒精度、`Z` 后缀、4 位年份（超出 0..9999 时按扩展年份）。
pub fn js_date_to_iso_string(epoch_millis: i64) -> String {
    let days = floor_div(epoch_millis, 86_400_000);
    let mut remainder = epoch_millis - days * 86_400_000;
    let (year, month, day) = civil_from_days(days);
    let hours = remainder / 3_600_000;
    remainder %= 3_600_000;
    let minutes = remainder / 60_000;
    remainder %= 60_000;
    let seconds = remainder / 1000;
    let millis = remainder % 1000;
    if (0..=9999).contains(&year) {
        format!("{year:04}-{month:02}-{day:02}T{hours:02}:{minutes:02}:{seconds:02}.{millis:03}Z")
    } else {
        let sign = if year < 0 { '-' } else { '+' };
        format!(
            "{sign}{:06}-{month:02}-{day:02}T{hours:02}:{minutes:02}:{seconds:02}.{millis:03}Z",
            year.abs()
        )
    }
}

/// `Date.now()`（Unix 毫秒）。
pub fn now_epoch_millis() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(delta) => delta.as_millis() as i64,
        Err(_) => 0,
    }
}

/// `crypto.createHash("sha256").update(value, "utf8").digest("hex")`。
pub fn sha256_hex(value: &str) -> String {
    let digest = Sha256::digest(value.as_bytes());
    hex::encode(digest)
}

/// 生成器/协议共用的错误：`Display` 等于 TS `error.message`。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogError {
    /// 原始错误文本，会被逐字写入 stderr 或被测试按子串断言；不要在构造时拼接前缀。
    pub message: String,
}

impl CatalogError {
    /// 用 TS 侧的 `error.message` 文本构造；`Display` 原样输出该文本。
    pub fn new(message: impl Into<String>) -> Self {
        CatalogError {
            message: message.into(),
        }
    }
}

impl std::fmt::Display for CatalogError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for CatalogError {}

// 转成 crate 统一错误时只保留消息文本（kind = Other），上层看到的仍是 TS 原文。
impl From<CatalogError> for AiError {
    fn from(error: CatalogError) -> Self {
        AiError::new(AiErrorKind::Other, error.message)
    }
}

/// `JSON.stringify(value)` 的紧凑文本（JS 数字格式化 + JS 键序）。
pub(crate) fn stringify_json(value: &Value) -> String {
    kiana_json::js_object::stringify(value)
}

// ---------------------------------------------------------------------------
// model-catalog.ts：把 `data/<provider>.json` 摊平成三张表
// ---------------------------------------------------------------------------

/// `model-catalog.ts` 的运行时部分。
pub mod flatten {
    use indexmap::IndexMap;

    /// `ModelGroups`：api -> `"type:id"` -> model。
    pub type ModelGroups<V> = IndexMap<String, IndexMap<String, V>>;

    /// 摊平所需的两个字段（TS 里是 `(model as {type?: unknown}).type === type` 与 `model.id`）。
    pub trait CatalogModel {
        /// `model.type`（可能缺失）。
        fn catalog_type(&self) -> Option<&str>;
        /// `model.id` 的字符串形式（便利方法；展平结果使用 [`CatalogModel::catalog_id_value`]）。
        fn catalog_id(&self) -> Option<&str>;
        /// `model.id` 的运行时值。TS 的 `Object.fromEntries` 把任意值按 `String()` 转成键：
        /// 数字 `5` -> `"5"`，缺失 -> `"undefined"`，`null` -> `"null"`。
        ///
        /// 默认实现把 [`CatalogModel::catalog_id`] 包成字符串；需要数字/缺失 id 的实现可覆盖。
        fn catalog_id_value(&self) -> Option<serde_json::Value> {
            self.catalog_id()
                .map(|id| serde_json::Value::String(id.to_string()))
        }
    }

    impl CatalogModel for serde_json::Value {
        fn catalog_type(&self) -> Option<&str> {
            self.get("type").and_then(serde_json::Value::as_str)
        }
        fn catalog_id(&self) -> Option<&str> {
            self.get("id").and_then(serde_json::Value::as_str)
        }
        fn catalog_id_value(&self) -> Option<serde_json::Value> {
            self.get("id").cloned()
        }
    }

    impl CatalogModel for crate::types::Model {
        fn catalog_type(&self) -> Option<&str> {
            self.model_type.map(|kind| match kind {
                crate::types::ModelType::Chat => "chat",
                crate::types::ModelType::Image => "image",
                crate::types::ModelType::Classifier => "classifier",
            })
        }
        fn catalog_id(&self) -> Option<&str> {
            Some(self.id.as_str())
        }
    }

    /// `flattenModelCatalog(groups, type)`。
    ///
    /// `Object.fromEntries`：按 api 组顺序、组内插入序收集 `type` 匹配的模型，键为 `model.id`；
    /// 同一 id 后出现的覆盖先出现的值，但**保持首次出现的位置**（JS 属性赋值语义）。
    pub fn flatten_model_catalog<V: CatalogModel + Clone>(
        groups: &ModelGroups<V>,
        model_type: &str,
    ) -> IndexMap<String, V> {
        let mut result: IndexMap<String, V> = IndexMap::new();
        for models in groups.values() {
            for model in models.values() {
                if model.catalog_type() != Some(model_type) {
                    continue;
                }
                // TS 不检查 id 类型：`String(undefined) === "undefined"`。
                let id = model.catalog_id_value();
                result.insert(kiana_json::js_string::js_string(id.as_ref()), model.clone());
            }
        }
        order_like_js_object(result)
    }

    /// JS 普通对象的自有属性枚举顺序：数组下标键（0 <= n < 2^32-1 的规范十进制串）按数值
    /// 升序排在最前，其余字符串键保持插入顺序。`Object.fromEntries` 返回的对象同样如此。
    fn order_like_js_object<V>(entries: IndexMap<String, V>) -> IndexMap<String, V> {
        let mut indices: Vec<(u32, String, V)> = Vec::new();
        let mut others: Vec<(String, V)> = Vec::new();
        for (key, value) in entries {
            match js_array_index(&key) {
                Some(index) => indices.push((index, key, value)),
                None => others.push((key, value)),
            }
        }
        indices.sort_by_key(|(index, _, _)| *index);
        let mut ordered = IndexMap::with_capacity(indices.len() + others.len());
        for (_, key, value) in indices {
            ordered.insert(key, value);
        }
        for (key, value) in others {
            ordered.insert(key, value);
        }
        ordered
    }

    /// `Array.prototype` 的数组下标键判定（`ToString(ToUint32(key)) === key` 且 key != "4294967295"）。
    fn js_array_index(key: &str) -> Option<u32> {
        if key.is_empty() || (key.len() > 1 && key.starts_with('0')) {
            return None;
        }
        if !key.bytes().all(|byte| byte.is_ascii_digit()) {
            return None;
        }
        let value: u64 = key.parse().ok()?;
        (value < 4_294_967_295).then_some(value as u32)
    }

    /// `flattenChatModelCatalog(provider, groups)`（`provider` 参数 TS 侧仅用于类型推断）。
    pub fn flatten_chat_model_catalog<V: CatalogModel + Clone>(
        groups: &ModelGroups<V>,
    ) -> IndexMap<String, V> {
        flatten_model_catalog(groups, "chat")
    }

    /// `flattenImageModelCatalog(provider, groups)`。
    pub fn flatten_image_model_catalog<V: CatalogModel + Clone>(
        groups: &ModelGroups<V>,
    ) -> IndexMap<String, V> {
        flatten_model_catalog(groups, "image")
    }

    /// `flattenClassifierModelCatalog(provider, groups)`。
    pub fn flatten_classifier_model_catalog<V: CatalogModel + Clone>(
        groups: &ModelGroups<V>,
    ) -> IndexMap<String, V> {
        flatten_model_catalog(groups, "classifier")
    }
}

/// 把 `flatten` 子模块的公开项提升到 `model_catalog` 根，对应 TS `model-catalog.ts` 的导出面。
pub use flatten::{
    CatalogModel, ModelGroups, flatten_chat_model_catalog, flatten_classifier_model_catalog,
    flatten_image_model_catalog, flatten_model_catalog,
};

// ---------------------------------------------------------------------------
// models-dev-reasoning-options.ts / openrouter-reasoning-options.ts
// ---------------------------------------------------------------------------

/// `models-dev-reasoning-options.ts` + `openrouter-reasoning-options.ts`。
pub mod reasoning_options {
    use indexmap::IndexMap;
    use serde_json::Value;

    use crate::types::{ModelThinkingLevel, ThinkingLevel, ThinkingLevelMap};

    /// `THINKING_LEVELS`（不含 `"off"`）。
    pub const THINKING_LEVELS: [ThinkingLevel; 6] = [
        ThinkingLevel::Minimal,
        ThinkingLevel::Low,
        ThinkingLevel::Medium,
        ThinkingLevel::High,
        ThinkingLevel::Xhigh,
        ThinkingLevel::Max,
    ];

    fn level_name(level: ThinkingLevel) -> &'static str {
        match level {
            ThinkingLevel::Minimal => "minimal",
            ThinkingLevel::Low => "low",
            ThinkingLevel::Medium => "medium",
            ThinkingLevel::High => "high",
            ThinkingLevel::Xhigh => "xhigh",
            ThinkingLevel::Max => "max",
        }
    }

    /// `ModelsDevReasoningOption`。
    #[derive(Debug, Clone, PartialEq, Default)]
    pub enum ModelsDevReasoningOption {
        /// `{type:"toggle"}`。
        #[default]
        Toggle,
        /// `{type:"effort", values:[...]}`；`None` 表示 JSON `null` 元素。
        Effort { values: Vec<Option<String>> },
        /// `{type:"budget_tokens", min?, max?}`。
        BudgetTokens { min: Option<f64>, max: Option<f64> },
    }

    impl ModelsDevReasoningOption {
        /// 从 models.dev 的 JSON 形状解析；未知 `type` 视为非 effort/toggle（`flatMap` 中不贡献值）。
        pub fn from_json(value: &Value) -> Option<Self> {
            let object = value.as_object()?;
            match object.get("type").and_then(Value::as_str) {
                Some("toggle") => Some(ModelsDevReasoningOption::Toggle),
                Some("effort") => {
                    let values = object
                        .get("values")
                        .and_then(Value::as_array)
                        .map(|items| {
                            items
                                .iter()
                                .map(|item| item.as_str().map(str::to_string))
                                .collect::<Vec<Option<String>>>()
                        })
                        .unwrap_or_default();
                    Some(ModelsDevReasoningOption::Effort { values })
                }
                Some("budget_tokens") => Some(ModelsDevReasoningOption::BudgetTokens {
                    min: object.get("min").and_then(Value::as_f64),
                    max: object.get("max").and_then(Value::as_f64),
                }),
                _ => None,
            }
        }

        /// `option.type === "effort" ? option.values : []`。
        fn effort_values(&self) -> &[Option<String>] {
            match self {
                ModelsDevReasoningOption::Effort { values } => values,
                _ => &[],
            }
        }
    }

    /// `getEffortThinkingLevelMap(options)`。
    ///
    /// 收集所有 `effort` 值；空 -> `None`。`supported` 既不含任何 `THINKING_LEVELS` 也不含
    /// `"none"` -> `None`。否则 `off = supported.has("none") ? "none" : null`，每个 level
    /// 映射到自身或 `null`。`default` 与 JSON `null` 被有意忽略。
    pub fn get_effort_thinking_level_map(
        options: &[ModelsDevReasoningOption],
    ) -> Option<ThinkingLevelMap> {
        let mut effort_values: Vec<&str> = Vec::new();
        for option in options {
            for value in option.effort_values().iter().flatten() {
                effort_values.push(value);
            }
        }
        if effort_values.is_empty() {
            return None;
        }

        let supported: std::collections::HashSet<&str> = effort_values.into_iter().collect();
        let has_level = THINKING_LEVELS
            .iter()
            .any(|level| supported.contains(level_name(*level)));
        if !has_level && !supported.contains("none") {
            return None;
        }

        let mut map: ThinkingLevelMap = IndexMap::new();
        map.insert(
            ModelThinkingLevel::Off,
            if supported.contains("none") {
                Some("none".to_string())
            } else {
                None
            },
        );
        for level in THINKING_LEVELS {
            let name = level_name(level);
            map.insert(
                to_model_level(level),
                if supported.contains(name) {
                    Some(name.to_string())
                } else {
                    None
                },
            );
        }
        Some(map)
    }

    fn to_model_level(level: ThinkingLevel) -> ModelThinkingLevel {
        match level {
            ThinkingLevel::Minimal => ModelThinkingLevel::Minimal,
            ThinkingLevel::Low => ModelThinkingLevel::Low,
            ThinkingLevel::Medium => ModelThinkingLevel::Medium,
            ThinkingLevel::High => ModelThinkingLevel::High,
            ThinkingLevel::Xhigh => ModelThinkingLevel::Xhigh,
            ThinkingLevel::Max => ModelThinkingLevel::Max,
        }
    }

    /// `OpenRouterReasoningMetadata`。
    #[derive(Debug, Clone, Default, PartialEq, Eq)]
    pub struct OpenRouterReasoningMetadata {
        /// `reasoning.mandatory === true`：此时 `off` 只能映射为 `null`，不能显式传 `"none"`。
        pub mandatory: bool,
        /// 列表项给出的默认开关；当前实现只解析，不参与 thinkingLevelMap 构造。
        pub default_enabled: Option<bool>,
        /// 支持的努力级别原文（如 `"high"`、`"medium"`），保持上游顺序。
        pub supported_efforts: Vec<String>,
        /// 默认努力级别；当前实现只解析，不参与 thinkingLevelMap 构造。
        pub default_effort: Option<String>,
    }

    impl OpenRouterReasoningMetadata {
        /// 从 OpenRouter 列表项的 `reasoning` 字段解析。
        pub fn from_json(value: &Value) -> Option<Self> {
            let object = value.as_object()?;
            Some(OpenRouterReasoningMetadata {
                mandatory: object
                    .get("mandatory")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
                default_enabled: object.get("default_enabled").and_then(Value::as_bool),
                supported_efforts: object
                    .get("supported_efforts")
                    .and_then(Value::as_array)
                    .map(|items| {
                        items
                            .iter()
                            .filter_map(|item| item.as_str().map(str::to_string))
                            .collect()
                    })
                    .unwrap_or_default(),
                default_effort: object
                    .get("default_effort")
                    .and_then(Value::as_str)
                    .map(str::to_string),
            })
        }
    }

    /// `getOpenRouterThinkingLevelMap(reasoning)`。
    pub fn get_openrouter_thinking_level_map(
        reasoning: Option<&OpenRouterReasoningMetadata>,
    ) -> Option<ThinkingLevelMap> {
        let reasoning = reasoning?;
        if reasoning.supported_efforts.is_empty() {
            return if reasoning.mandatory {
                Some(off_null_map())
            } else {
                None
            };
        }

        let option = ModelsDevReasoningOption::Effort {
            values: reasoning
                .supported_efforts
                .iter()
                .cloned()
                .map(Some)
                .collect(),
        };
        let map = get_effort_thinking_level_map(std::slice::from_ref(&option));
        let mut map = match map {
            Some(map) => map,
            None => {
                return if reasoning.mandatory {
                    Some(off_null_map())
                } else {
                    None
                };
            }
        };
        // `{...map, off: ...}`：`off` 已在首位，赋值保持原位。
        map.insert(
            ModelThinkingLevel::Off,
            if reasoning.mandatory {
                None
            } else {
                Some("none".to_string())
            },
        );
        Some(map)
    }

    /// `"off"`/`"minimal"`/... → `ModelThinkingLevel`（未知为 `None`）。
    pub fn model_level_from_name(name: &str) -> Option<ModelThinkingLevel> {
        Some(match name {
            "off" => ModelThinkingLevel::Off,
            "minimal" => ModelThinkingLevel::Minimal,
            "low" => ModelThinkingLevel::Low,
            "medium" => ModelThinkingLevel::Medium,
            "high" => ModelThinkingLevel::High,
            "xhigh" => ModelThinkingLevel::Xhigh,
            "max" => ModelThinkingLevel::Max,
            _ => return None,
        })
    }

    fn off_null_map() -> ThinkingLevelMap {
        let mut map: ThinkingLevelMap = IndexMap::new();
        map.insert(ModelThinkingLevel::Off, None);
        map
    }
}

// ---------------------------------------------------------------------------
// ai-gateway-pricing.ts
// ---------------------------------------------------------------------------

/// `ai-gateway-pricing.ts`：Vercel AI Gateway 的 $/token 定价转 `ModelCost`。
pub mod gateway_pricing {
    use serde_json::Value;

    use crate::types::{ModelCost, ModelCostRates, ModelCostTier};

    /// `RATE_FIELDS`：`rate` 是 `ModelCostRates` 的字段，`tiers` 是 `AiGatewayPricing` 的数组字段。
    const RATE_FIELDS: [(&str, &str); 4] = [
        ("input", "input_tiers"),
        ("output", "output_tiers"),
        ("cacheRead", "input_cache_read_tiers"),
        ("cacheWrite", "input_cache_write_tiers"),
    ];

    /// `perMillion(value)`：`$ / token` -> `$ / 1M tokens`，保留 6 位小数。
    ///
    /// TS 用 `Number((parsed * 1e6).toFixed(6))`：**先乘**再四舍五入到 6 位。
    pub fn per_million(value: Option<&Value>) -> f64 {
        let parsed = match value {
            Some(Value::Number(number)) => number.as_f64().unwrap_or(f64::NAN),
            Some(Value::String(text)) => super::js_parse_float(text),
            Some(other) => super::js_parse_float(&kiana_json::js_string::js_string(Some(other))),
            None => 0.0,
        };
        if parsed.is_finite() {
            super::round_cost(parsed * 1_000_000.0)
        } else {
            0.0
        }
    }

    fn set_rate(tier: &mut ModelCostTier, name: &str, value: f64) {
        match name {
            "input" => tier.input = value,
            "output" => tier.output = value,
            "cacheRead" => tier.cache_read = value,
            _ => tier.cache_write = value,
        }
    }

    /// JS `ToNumber(value)`（JSON 可达值；数组走 `ToPrimitive` 的 `join(",")`）。
    pub(super) fn js_to_number(value: &Value) -> f64 {
        match value {
            Value::Null => 0.0,
            Value::Bool(flag) => {
                if *flag {
                    1.0
                } else {
                    0.0
                }
            }
            Value::Number(number) => number.as_f64().unwrap_or(f64::NAN),
            Value::String(text) => kiana_json::js_num::parse_js_number(text),
            Value::Array(_) => {
                kiana_json::js_num::parse_js_number(&kiana_json::js_string::js_string(Some(value)))
            }
            Value::Object(_) => f64::NAN,
        }
    }

    /// JS 关系比较 `left < right`（两个字符串按码元序，否则按 `ToNumber`）。
    fn js_less_than(left: &Value, right: &Value) -> bool {
        if let (Value::String(left), Value::String(right)) = (left, right) {
            // JS 字符串关系比较按 UTF-16 码元序列。
            return left.encode_utf16().cmp(right.encode_utf16()).is_lt();
        }
        js_to_number(left) < js_to_number(right)
    }

    /// JS 关系比较 `left <= right`（`NaN <= x` 为 false）。
    fn js_less_equal(left: &Value, right: &Value) -> bool {
        if let (Value::String(left), Value::String(right)) = (left, right) {
            return left.encode_utf16().cmp(right.encode_utf16()).is_le();
        }
        js_to_number(left) <= js_to_number(right)
    }

    /// JS 关系比较 `left > right`。
    fn js_greater_than(left: &Value, right: &Value) -> bool {
        if let (Value::String(left), Value::String(right)) = (left, right) {
            return left.encode_utf16().cmp(right.encode_utf16()).is_gt();
        }
        js_to_number(left) > js_to_number(right)
    }

    /// `Set` 使用的 SameValueZero（数字按数值，字符串按内容）。
    fn same_value_zero(left: &Value, right: &Value) -> bool {
        match (left, right) {
            (Value::Null, Value::Null) => true,
            (Value::Bool(left), Value::Bool(right)) => left == right,
            (Value::Number(left), Value::Number(right)) => {
                left.as_f64().unwrap_or(f64::NAN) == right.as_f64().unwrap_or(f64::NAN)
            }
            (Value::String(left), Value::String(right)) => left == right,
            _ => false,
        }
    }

    /// `Number(value)`：`_tiers` 字段不是数组时 TS 的 `for…of` 抛 `TypeError`；
    /// Rust 侧无法从纯函数返回该运行时异常，按“无档位”处理（见模块注释/报告）。
    fn bracket_list(pricing: Option<&Value>, field: &str) -> Vec<Value> {
        pricing
            .and_then(|value| value.get(field))
            .and_then(Value::as_array)
            .map(|items| items.to_vec())
            .unwrap_or_default()
    }

    /// `getAiGatewayCost(pricing)`。
    ///
    /// base 四费率默认 0。所有 tier 中 `min > 0`（JS 关系比较：字符串/布尔 `min` 也会参与）
    /// 的取值按 `Set` 语义去重、按 `a - b` 数值升序，每个 start 生成
    /// `{ inputTokensAbove: start - 1, ...base }`，并按每个 rate 自己的 bracket 覆盖；
    /// `tiers` 非空才返回带 `tiers` 的 cost。
    pub fn get_ai_gateway_cost(pricing: Option<&Value>) -> ModelCost {
        let base = ModelCostRates {
            input: per_million(pricing.and_then(|value| value.get("input"))),
            output: per_million(pricing.and_then(|value| value.get("output"))),
            cache_read: per_million(pricing.and_then(|value| value.get("input_cache_read"))),
            cache_write: per_million(pricing.and_then(|value| value.get("input_cache_write"))),
        };

        let mut starts: Vec<Value> = Vec::new();
        for (_, tiers_field) in RATE_FIELDS {
            for bracket in bracket_list(pricing, tiers_field) {
                // `bracket.min !== undefined && bracket.min > 0`（null 参与比较，`null > 0` 为 false）。
                let Some(min) = bracket.get("min") else {
                    continue;
                };
                if js_greater_than(min, &Value::from(0))
                    && !starts.iter().any(|seen| same_value_zero(seen, min))
                {
                    starts.push(min.clone());
                }
            }
        }
        // `(a, b) => a - b`：`NaN` 比较结果按 0 处理。
        starts.sort_by(|left, right| {
            (js_to_number(left) - js_to_number(right))
                .partial_cmp(&0.0)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        let mut tiers: Vec<ModelCostTier> = Vec::new();
        for start in &starts {
            let mut tier = ModelCostTier {
                input_tokens_above: js_to_number(start) - 1.0,
                input: base.input,
                output: base.output,
                cache_read: base.cache_read,
                cache_write: base.cache_write,
            };
            for (rate, tiers_field) in RATE_FIELDS {
                let bracket = bracket_list(pricing, tiers_field)
                    .into_iter()
                    .find(|candidate| {
                        // `(candidate.min ?? 0) <= start && (candidate.max === undefined || start < candidate.max)`
                        let min = match candidate.get("min") {
                            None | Some(Value::Null) => Value::from(0),
                            Some(min) => min.clone(),
                        };
                        let max_ok = match candidate.get("max") {
                            None => true,
                            Some(max) => js_less_than(start, max),
                        };
                        js_less_equal(&min, start) && max_ok
                    });
                // `if (bracket?.cost !== undefined)`：`cost: null` 也覆盖该档位（结果为 0）。
                if let Some(cost) = bracket.and_then(|candidate| candidate.get("cost").cloned()) {
                    set_rate(&mut tier, rate, per_million(Some(&cost)));
                }
            }
            tiers.push(tier);
        }

        ModelCost {
            input: base.input,
            output: base.output,
            cache_read: base.cache_read,
            cache_write: base.cache_write,
            tiers: if tiers.is_empty() { None } else { Some(tiers) },
        }
    }
}

// ---------------------------------------------------------------------------
// model-data.ts：数据目录 schema、哈希与校验
// ---------------------------------------------------------------------------

/// `scripts/model-data.ts`：生成数据的 schema、结构哈希与目录校验。
pub mod model_data {
    use std::path::{Path, PathBuf};

    use indexmap::IndexMap;
    use serde_json::Value;

    use super::{CatalogError, sha256_hex, stringify_json};

    /// `MODEL_DATA_SCHEMA_VERSION`。
    pub const MODEL_DATA_SCHEMA_VERSION: u64 = 6;
    /// `MODEL_DATA_MANIFEST_FILE`。
    pub const MODEL_DATA_MANIFEST_FILE: &str = ".manifest.json";

    /// `ModelDataStructure`：provider -> `"type:id"` -> api。
    pub type ModelDataStructure = IndexMap<String, IndexMap<String, String>>;

    /// `ModelDataManifest`。
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct ModelDataManifest {
        /// 目录 schema 版本，必须等于 [`MODEL_DATA_SCHEMA_VERSION`]。缺失时 [`ModelDataManifest::from_json`]
        /// 填 `u64::MAX`，让校验必然失败。
        pub schema_version: u64,
        /// 生成时间原文（ISO-8601）；是否合法由 `Date.parse` 另行判定。
        pub generated_at: String,
        /// 目录结构的 sha256（见 [`model_data_structure_hash`]）。
        pub structure_hash: String,
        /// 数据文件名 -> 文件内容的 sha256。
        pub files: IndexMap<String, String>,
    }

    impl ModelDataManifest {
        /// `JSON.stringify(manifest)` 的字段序（schemaVersion, generatedAt, structureHash, files）。
        pub fn to_json(&self) -> Value {
            let mut files = serde_json::Map::new();
            for (name, hash) in &self.files {
                files.insert(name.clone(), Value::String(hash.clone()));
            }
            serde_json::json!({
                "schemaVersion": self.schema_version,
                "generatedAt": self.generated_at,
                "structureHash": self.structure_hash,
                "files": Value::Object(files),
            })
        }

        /// 从 manifest JSON 解析；缺字段时用哨兵值（`schemaVersion` -> `u64::MAX`、字符串 -> 空串），
        /// 让后续校验能报出具体错误而不是静默通过。
        pub fn from_json(value: &Value) -> ModelDataManifest {
            let object = value.as_object();
            ModelDataManifest {
                schema_version: object
                    .and_then(|object| object.get("schemaVersion"))
                    .and_then(Value::as_u64)
                    .unwrap_or(u64::MAX),
                generated_at: object
                    .and_then(|object| object.get("generatedAt"))
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                structure_hash: object
                    .and_then(|object| object.get("structureHash"))
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                files: IndexMap::new(),
            }
        }
    }

    /// `MODEL_DATA_IMPORT_PATTERN`：`src/models.generated.ts` 的 provider 导入行。
    ///
    /// 逐字正则：`^import \{ [A-Z][A-Z0-9_]*_CLASSIFIER_MODELS, [A-Z][A-Z0-9_]*_IMAGE_MODELS,
    /// [A-Z][A-Z0-9_]*_MODELS \} from "\.\/providers\/([^"/]+)\.models\.ts";$`（`gm`）。
    pub const MODEL_DATA_IMPORT_PATTERN: &str = r#"^import \{ [A-Z][A-Z0-9_]*_CLASSIFIER_MODELS, [A-Z][A-Z0-9_]*_IMAGE_MODELS, [A-Z][A-Z0-9_]*_MODELS \} from "\./providers/([^"/]+)\.models\.ts";$"#;

    /// `Array.prototype.sort()` 的默认比较：UTF-16 码元序。
    pub fn code_unit_compare(left: &str, right: &str) -> std::cmp::Ordering {
        let left_units: Vec<u16> = left.encode_utf16().collect();
        let right_units: Vec<u16> = right.encode_utf16().collect();
        left_units.cmp(&right_units)
    }

    /// 按 UTF-16 码元序排序的 `(键, 值)` 序列（`sortedRecord`）。
    pub fn sorted_pairs<V: Clone>(
        entries: impl IntoIterator<Item = (String, V)>,
    ) -> IndexMap<String, V> {
        let mut pairs: Vec<(String, V)> = entries.into_iter().collect();
        pairs.sort_by(|a, b| code_unit_compare(&a.0, &b.0));
        pairs.into_iter().collect()
    }

    fn same_strings(a: &[String], b: &[String]) -> bool {
        a.len() == b.len() && a.iter().zip(b).all(|(x, y)| x == y)
    }

    fn describe_set_difference(expected: &[String], actual: &[String]) -> String {
        let expected_set: std::collections::HashSet<&String> = expected.iter().collect();
        let actual_set: std::collections::HashSet<&String> = actual.iter().collect();
        let missing: Vec<&str> = expected
            .iter()
            .filter(|value| !actual_set.contains(*value))
            .map(String::as_str)
            .collect();
        let extra: Vec<&str> = actual
            .iter()
            .filter(|value| !expected_set.contains(*value))
            .map(String::as_str)
            .collect();
        let mut parts: Vec<String> = Vec::new();
        if !missing.is_empty() {
            parts.push(format!("missing: {}", missing.join(", ")));
        }
        if !extra.is_empty() {
            parts.push(format!("extra: {}", extra.join(", ")));
        }
        parts.join("; ")
    }

    fn unique_sorted(values: impl IntoIterator<Item = String>) -> Vec<String> {
        let mut seen: Vec<String> = Vec::new();
        for value in values {
            if !seen.contains(&value) {
                seen.push(value);
            }
        }
        seen.sort_by(|a, b| code_unit_compare(a, b));
        seen
    }

    /// `assertExactModelIds(label, expected, actual)`。
    pub fn assert_exact_model_ids(
        label: &str,
        expected: impl IntoIterator<Item = String>,
        actual: impl IntoIterator<Item = String>,
    ) -> Result<(), CatalogError> {
        let expected_ids = unique_sorted(expected);
        let actual_ids = unique_sorted(actual);
        if same_strings(&expected_ids, &actual_ids) {
            return Ok(());
        }
        Err(CatalogError::new(format!(
            "{label} model IDs do not match ({})",
            describe_set_difference(&expected_ids, &actual_ids)
        )))
    }

    /// `isRecord(value)`。
    pub fn is_record(value: &Value) -> bool {
        value.is_object()
    }

    /// `isModalityList(value)`：非空数组且每项为 `"text"` 或 `"image"`。
    fn is_modality_list(value: &Value) -> bool {
        match value.as_array() {
            Some(items) => {
                !items.is_empty()
                    && items
                        .iter()
                        .all(|item| matches!(item.as_str(), Some("text" | "image")))
            }
            None => false,
        }
    }

    /// `JSON.stringify(value)`，`undefined`（`None`）渲染为字符串 `"undefined"`（模板插值语义）。
    pub(crate) fn js_stringify_or_undefined(value: Option<&Value>) -> String {
        match value {
            Some(value) => stringify_json(value),
            None => "undefined".to_string(),
        }
    }

    /// Node/libuv 的 `fs` 错误文本：`<CODE>: <strerror>, <syscall> '<path>'[ -> '<dest>']`。
    ///
    /// TS 里所有 `fs` 调用失败都会带上 syscall 与引号路径（`ENOENT: no such file or
    /// directory, open '<p>'`、`EISDIR: illegal operation on a directory, read`、
    /// `ENOENT: no such file or directory, scandir '<p>'`、`rename '<a>' -> '<b>'` 等），
    /// Rust `io::Error` 的 Display 缺这些信息，所以统一在此转写。
    pub(crate) fn node_fs_error(
        error: &std::io::Error,
        syscall: &str,
        path: &Path,
        dest: Option<&Path>,
    ) -> String {
        let (code, description) = match error.raw_os_error() {
            Some(libc::ENOENT) => ("ENOENT", "no such file or directory"),
            Some(libc::EACCES) => ("EACCES", "permission denied"),
            Some(libc::EPERM) => ("EPERM", "operation not permitted"),
            Some(libc::EEXIST) => ("EEXIST", "file already exists"),
            Some(libc::ENOTDIR) => ("ENOTDIR", "not a directory"),
            Some(libc::EISDIR) => ("EISDIR", "illegal operation on a directory"),
            Some(libc::EINVAL) => ("EINVAL", "invalid argument"),
            Some(libc::ENOSPC) => ("ENOSPC", "no space left on device"),
            Some(libc::EROFS) => ("EROFS", "read-only file system"),
            Some(libc::EXDEV) => ("EXDEV", "invalid cross-device link"),
            Some(libc::ENAMETOOLONG) => ("ENAMETOOLONG", "name too long"),
            Some(libc::ELOOP) => ("ELOOP", "too many symbolic links encountered"),
            _ => match error.kind() {
                std::io::ErrorKind::NotFound => ("ENOENT", "no such file or directory"),
                std::io::ErrorKind::PermissionDenied => ("EACCES", "permission denied"),
                std::io::ErrorKind::AlreadyExists => ("EEXIST", "file already exists"),
                std::io::ErrorKind::NotADirectory => ("ENOTDIR", "not a directory"),
                std::io::ErrorKind::IsADirectory => ("EISDIR", "illegal operation on a directory"),
                _ => ("EUNKNOWN", "unknown error"),
            },
        };
        // `readFileSync(dir)`：`EISDIR: illegal operation on a directory, read`（无路径）。
        if code == "EISDIR" && syscall == "read" {
            return format!("{code}: {description}, read");
        }
        let shown = path.display();
        match dest {
            Some(dest) => format!(
                "{code}: {description}, {syscall} '{shown}' -> '{}'",
                dest.display()
            ),
            None => format!("{code}: {description}, {syscall} '{shown}'"),
        }
    }

    /// `readFileSync(path, "utf8")`：无效 UTF-8 字节替换为 U+FFFD（Node 不抛错）。
    pub(crate) fn read_file_utf8(path: &Path) -> Result<String, std::io::Error> {
        let bytes = std::fs::read(path)?;
        Ok(String::from_utf8_lossy(&bytes).into_owned())
    }

    /// `readFileSync(path, "utf8")` 失败时的 Node 错误文本。
    ///
    /// 打开失败是 `open '<path>'`；目录会在 open 成功后 read 失败，libuv 报
    /// `EISDIR: illegal operation on a directory, read`（无路径）。
    pub(crate) fn node_read_error(path: &Path, error: &std::io::Error) -> String {
        if error.raw_os_error() == Some(libc::EISDIR) {
            node_fs_error(error, "read", path, None)
        } else {
            node_fs_error(error, "open", path, None)
        }
    }

    fn read_json_object(path: &Path, description: &str, errors: &mut Vec<String>) -> Option<Value> {
        let text = match read_file_utf8(path) {
            Ok(text) => text,
            Err(error) => {
                errors.push(format!(
                    "{description} is not valid JSON: {}",
                    node_read_error(path, &error)
                ));
                return None;
            }
        };
        let parsed = match kiana_json::js_parse::parse(&text) {
            Ok(parsed) => parsed,
            Err(error) => {
                errors.push(format!(
                    "{description} is not valid JSON: {}",
                    error.message
                ));
                return None;
            }
        };
        if !is_record(&parsed) {
            errors.push(format!("{description} must contain a JSON object"));
            return None;
        }
        Some(parsed)
    }

    fn read_provider_structure(
        path: &Path,
        provider_id: &str,
    ) -> Result<IndexMap<String, String>, CatalogError> {
        let mut errors: Vec<String> = Vec::new();
        let groups = match read_json_object(path, &format!("{provider_id}.json"), &mut errors) {
            Some(groups) => groups,
            None => return Err(CatalogError::new(errors.join("\n"))),
        };

        let mut models: IndexMap<String, String> = IndexMap::new();
        for (api, value) in groups.as_object().expect("is_record guarantees an object") {
            if !is_record(value) {
                return Err(CatalogError::new(format!(
                    "{} API group {} must be an object",
                    path.display(),
                    stringify_json(&Value::String(api.clone()))
                )));
            }
            for model_key in value.as_object().expect("checked above").keys() {
                if models.contains_key(model_key) {
                    return Err(CatalogError::new(format!(
                        "{} contains {model_key} in more than one API group",
                        path.display()
                    )));
                }
                models.insert(model_key.clone(), api.clone());
            }
        }
        if models.is_empty() {
            return Err(CatalogError::new(format!(
                "{} contains no generated model data",
                path.display()
            )));
        }
        Ok(sorted_pairs(models))
    }

    /// 把 JS 的四个行终止符（`\r\n`、`\r`、`\n`、`\u2028`、`\u2029`）归一化成 `\n`。
    ///
    /// JS `/gm` 的 `^`/`$` 把前四个都当行边界，Rust `(?m)` 只认 `\n`。
    fn normalize_js_line_terminators(text: &str) -> String {
        let mut out = String::with_capacity(text.len());
        let mut chars = text.chars().peekable();
        while let Some(c) = chars.next() {
            match c {
                '\r' => {
                    if chars.peek() == Some(&'\n') {
                        chars.next();
                    }
                    out.push('\n');
                }
                '\n' | '\u{2028}' | '\u{2029}' => out.push('\n'),
                _ => out.push(c),
            }
        }
        out
    }

    /// `readModelDataProviderIds(packageRoot)`：从 `src/models.generated.ts` 提取 provider id（排序）。
    pub fn read_model_data_provider_ids(package_root: &Path) -> Result<Vec<String>, CatalogError> {
        let aggregator_path = package_root.join("src").join("models.generated.ts");
        let aggregator = read_file_utf8(&aggregator_path)
            .map_err(|error| CatalogError::new(node_read_error(&aggregator_path, &error)))?;
        // JS `/gm` 的行终止符是 `\n`、`\r`、`\u2028`、`\u2029`（`$` 匹配在它们之前）；
        // Rust 的 `(?m)` 只认 `\n`。归一化行终止符后逐字复用同一模式，CRLF 也能匹配。
        let normalized = normalize_js_line_terminators(&aggregator);
        let pattern = regex::Regex::new(&format!("(?m){}", MODEL_DATA_IMPORT_PATTERN))
            .expect("MODEL_DATA_IMPORT_PATTERN is a valid regex");
        let mut provider_ids: Vec<String> = pattern
            .captures_iter(&normalized)
            .map(|captures| captures[1].to_string())
            .collect();
        provider_ids.sort_by(|a, b| code_unit_compare(a, b));
        if provider_ids.is_empty() {
            return Err(CatalogError::new(format!(
                "No generated provider imports found in {}",
                aggregator_path.display()
            )));
        }
        let unique: std::collections::HashSet<&String> = provider_ids.iter().collect();
        if unique.len() != provider_ids.len() {
            return Err(CatalogError::new(format!(
                "Generated model aggregator contains duplicate provider imports: {}",
                aggregator_path.display()
            )));
        }
        Ok(provider_ids)
    }

    /// `readModelDataStructure(packageRoot)`。
    ///
    /// 先读 `src/models.generated.ts` 的 provider 列表，要求 `src/providers/*.models.ts` 的分片
    /// 集合与它完全一致；再逐个解析 `data/<provider>.json`，得到 `"type:id" -> api` 结构。
    /// 任一文件缺失、JSON 非法、同一个 `"type:id"` 出现在多个 API 组或目录为空都返回错误。
    pub fn read_model_data_structure(
        package_root: &Path,
    ) -> Result<ModelDataStructure, CatalogError> {
        let providers_dir = package_root.join("src").join("providers");
        let data_dir = providers_dir.join("data");
        let provider_ids = read_model_data_provider_ids(package_root)?;
        let mut expected_shards: Vec<String> = provider_ids
            .iter()
            .map(|provider_id| format!("{provider_id}.models.ts"))
            .collect();
        expected_shards.sort_by(|a, b| code_unit_compare(a, b));
        let entries = std::fs::read_dir(&providers_dir).map_err(|error| {
            CatalogError::new(node_fs_error(&error, "scandir", &providers_dir, None))
        })?;
        let mut actual_shards: Vec<String> = entries
            .filter_map(Result::ok)
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .filter(|entry| entry.ends_with(".models.ts"))
            .collect();
        actual_shards.sort_by(|a, b| code_unit_compare(a, b));
        if !same_strings(&expected_shards, &actual_shards) {
            return Err(CatalogError::new(format!(
                "Generated model aggregator and provider shards do not match ({})",
                describe_set_difference(&expected_shards, &actual_shards)
            )));
        }

        let mut structure: Vec<(String, IndexMap<String, String>)> = Vec::new();
        for provider_id in &provider_ids {
            let path = data_dir.join(format!("{provider_id}.json"));
            structure.push((
                provider_id.clone(),
                read_provider_structure(&path, provider_id)?,
            ));
        }
        Ok(sorted_pairs(structure))
    }

    /// `modelDataStructureHash(structure)`：provider 与模型键双层排序后 `JSON.stringify`，再 sha256。
    pub fn model_data_structure_hash(structure: &ModelDataStructure) -> String {
        let mut normalized = serde_json::Map::new();
        for (provider_id, models) in sorted_pairs(structure.clone()) {
            let mut model_map = serde_json::Map::new();
            for (model_key, api) in sorted_pairs(models) {
                model_map.insert(model_key, Value::String(api));
            }
            normalized.insert(provider_id, Value::Object(model_map));
        }
        sha256_hex(&stringify_json(&Value::Object(normalized)))
    }

    /// `createModelDataManifest(structure, fileContents, generatedAt)`。
    pub fn create_model_data_manifest(
        structure: &ModelDataStructure,
        file_contents: &IndexMap<String, String>,
        generated_at: &str,
    ) -> ModelDataManifest {
        ModelDataManifest {
            schema_version: MODEL_DATA_SCHEMA_VERSION,
            generated_at: generated_at.to_string(),
            structure_hash: model_data_structure_hash(structure),
            files: sorted_pairs(
                file_contents
                    .iter()
                    .map(|(file, content)| (file.clone(), sha256_hex(content)))
                    .collect::<Vec<_>>(),
            ),
        }
    }

    fn validate_model_value(
        value: &Value,
        provider_id: &str,
        model_id: &str,
        expected_api: &str,
        errors: &mut Vec<String>,
    ) {
        let label = format!("{provider_id}/{model_id}");
        if !is_record(value) {
            errors.push(format!("{label} must be an object"));
            return;
        }
        let field = |name: &str| value.get(name);
        // 身份三元组必须与文件名/分组键一致：数据文件的键是 `"type:id"`，api 是外层分组键。
        if field("id") != Some(&Value::String(model_id.to_string())) {
            errors.push(format!(
                "{label} has id {}, expected {}",
                js_stringify_or_undefined(field("id")),
                stringify_json(&Value::String(model_id.to_string()))
            ));
        }
        if field("provider") != Some(&Value::String(provider_id.to_string())) {
            errors.push(format!(
                "{label} has provider {}, expected {}",
                js_stringify_or_undefined(field("provider")),
                stringify_json(&Value::String(provider_id.to_string()))
            ));
        }
        if field("api") != Some(&Value::String(expected_api.to_string())) {
            errors.push(format!(
                "{label} has api {}, expected {}",
                js_stringify_or_undefined(field("api")),
                stringify_json(&Value::String(expected_api.to_string()))
            ));
        }
        let name_ok = field("name")
            .and_then(Value::as_str)
            .map(|name| !name.is_empty())
            .unwrap_or(false);
        if !name_ok {
            errors.push(format!("{label} has no model name"));
        }
        if field("baseUrl").and_then(Value::as_str).is_none() {
            errors.push(format!("{label} has no baseUrl string"));
        }
        if !field("input").map(is_modality_list).unwrap_or(false) {
            errors.push(format!("{label} has invalid input modalities"));
        }
        let model_type = field("type").and_then(Value::as_str);
        // 只有 image 模型允许 `output`，且输出模态必须含 image；其他类型写了 output 即报错。
        if model_type == Some("image") {
            let output = field("output");
            let has_image_output = output
                .map(|value| match value.as_array() {
                    Some(items) => {
                        is_modality_list(value)
                            && items.iter().any(|item| item.as_str() == Some("image"))
                    }
                    None => false,
                })
                .unwrap_or(false);
            if !has_image_output {
                errors.push(format!("{label} has invalid output modalities"));
            }
        } else if field("output").is_some() {
            errors.push(format!("{label} has unsupported output modalities"));
        }
        let positive_number = |name: &str| -> bool {
            match field(name).and_then(Value::as_f64) {
                Some(number) => number.is_finite() && number > 0.0,
                None => false,
            }
        };
        match model_type {
            // 类型专属的必填数值：chat 需要 reasoning/contextWindow/maxTokens，
            // classifier 只需要 contextWindow；数值由 positive_number 要求有限且 > 0，
            // 但这里不检查上界。
            Some("chat") => {
                if field("reasoning").and_then(Value::as_bool).is_none() {
                    errors.push(format!("{label} has no reasoning boolean"));
                }
                if !positive_number("contextWindow") {
                    errors.push(format!("{label} has invalid contextWindow"));
                }
                if !positive_number("maxTokens") {
                    errors.push(format!("{label} has invalid maxTokens"));
                }
            }
            Some("classifier") => {
                if !positive_number("contextWindow") {
                    errors.push(format!("{label} has invalid contextWindow"));
                }
            }
            Some("image") => {}
            _ => {
                errors.push(format!(
                    "{label} has type {}, expected \"chat\", \"image\", or \"classifier\"",
                    js_stringify_or_undefined(field("type"))
                ));
            }
        }
        match field("cost") {
            Some(cost) if is_record(cost) => {
                for name in ["input", "output", "cacheRead", "cacheWrite"] {
                    let valid = cost
                        .get(name)
                        .and_then(Value::as_f64)
                        .map(f64::is_finite)
                        .unwrap_or(false);
                    if !valid {
                        errors.push(format!("{label} has invalid cost.{name}"));
                    }
                }
            }
            _ => errors.push(format!("{label} has invalid cost metadata")),
        }
    }

    /// `throwValidationErrors(errors)`：只显示前 30 条，多余追加 `\n  ... and N more`。
    pub fn throw_validation_errors(errors: &[String]) -> CatalogError {
        let visible = &errors[..errors.len().min(30)];
        let suffix = if errors.len() > visible.len() {
            format!("\n  ... and {} more", errors.len() - visible.len())
        } else {
            String::new()
        };
        let body: Vec<String> = visible.iter().map(|error| format!("  - {error}")).collect();
        CatalogError::new(format!(
            "Invalid generated model data:\n{}{suffix}",
            body.join("\n")
        ))
    }

    /// `validateModelDataDirectory(structure, dataDir)`。
    pub fn validate_model_data_directory(
        structure: &ModelDataStructure,
        data_dir: &Path,
    ) -> Result<(), CatalogError> {
        if !data_dir.is_dir() {
            return Err(CatalogError::new(format!(
                "Generated model data directory does not exist: {}",
                data_dir.display()
            )));
        }

        let mut errors: Vec<String> = Vec::new();
        // 第一层：数据文件集合必须与结构里的 provider 完全一致（manifest 自身不算数据文件）。
        let mut expected_files: Vec<String> = structure
            .keys()
            .map(|provider_id| format!("{provider_id}.json"))
            .collect();
        expected_files.sort_by(|a, b| code_unit_compare(a, b));
        let entries = std::fs::read_dir(data_dir)
            .map_err(|error| CatalogError::new(node_fs_error(&error, "scandir", data_dir, None)))?;
        let mut actual_files: Vec<String> = entries
            .filter_map(Result::ok)
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .filter(|entry| entry.ends_with(".json") && entry != MODEL_DATA_MANIFEST_FILE)
            .collect();
        actual_files.sort_by(|a, b| code_unit_compare(a, b));
        if !same_strings(&expected_files, &actual_files) {
            errors.push(format!(
                "provider data files do not match the generated catalog ({})",
                describe_set_difference(&expected_files, &actual_files)
            ));
        }

        // 第二层：manifest 的 schema 版本、generatedAt 可解析性、structureHash 与当前结构一致。
        let manifest_path = data_dir.join(MODEL_DATA_MANIFEST_FILE);
        let manifest_value = read_json_object(&manifest_path, "model data manifest", &mut errors);
        let manifest = manifest_value.as_ref().map(ModelDataManifest::from_json);
        let schema_version = manifest.as_ref().map(|manifest| manifest.schema_version);
        if schema_version != Some(MODEL_DATA_SCHEMA_VERSION) {
            let shown = manifest_value
                .as_ref()
                .and_then(|value| value.get("schemaVersion"))
                .map(stringify_json)
                .unwrap_or_else(|| "undefined".to_string());
            errors.push(format!(
                "model data schema is {shown}, expected {MODEL_DATA_SCHEMA_VERSION}"
            ));
        }
        let generated_at_ok = manifest_value
            .as_ref()
            .and_then(|value| value.get("generatedAt"))
            .and_then(Value::as_str)
            .map(|text| super::js_date_parse(text).is_some())
            .unwrap_or(false);
        if !generated_at_ok {
            errors.push("model data manifest has an invalid generation timestamp".to_string());
        }
        let expected_structure_hash = model_data_structure_hash(structure);
        let structure_hash_matches = manifest_value
            .as_ref()
            .and_then(|value| value.get("structureHash"))
            .and_then(Value::as_str)
            .map(|hash| hash == expected_structure_hash)
            .unwrap_or(false);
        if !structure_hash_matches {
            errors.push(
                "model data generation stamp does not match the generated catalog".to_string(),
            );
        }
        // manifest 的 files 键集合必须与数据文件集合一致；每一项的哈希在下面逐文件比对。
        let manifest_files: Option<IndexMap<String, String>> = manifest_value
            .as_ref()
            .and_then(|value| value.get("files"))
            .filter(|files| is_record(files))
            .map(|files| {
                files
                    .as_object()
                    .expect("checked")
                    .iter()
                    .map(|(name, hash)| {
                        (name.clone(), hash.as_str().unwrap_or_default().to_string())
                    })
                    .collect()
            });
        match &manifest_files {
            None => errors.push("model data manifest has no file hashes".to_string()),
            Some(manifest_files) => {
                let mut names: Vec<String> = manifest_files.keys().cloned().collect();
                names.sort_by(|a, b| code_unit_compare(a, b));
                if !same_strings(&expected_files, &names) {
                    errors.push(format!(
                        "manifest file hashes do not match provider data files ({})",
                        describe_set_difference(&expected_files, &names)
                    ));
                }
            }
        }

        // 第三层：逐文件比较字节哈希、解析 JSON、校验模型身份/类型字段，并核对模型集合与分组。
        // 所有问题累积到 errors，最后由 throwValidationErrors 一次性输出（只显示前 30 条）。
        for (provider_id, expected_models) in structure {
            let filename = format!("{provider_id}.json");
            let path = data_dir.join(&filename);
            if !path.exists() {
                continue;
            }
            let content = read_file_utf8(&path)
                .map_err(|error| CatalogError::new(node_read_error(&path, &error)))?;
            if let Some(manifest_files) = &manifest_files
                && manifest_files.get(&filename).map(String::as_str)
                    != Some(sha256_hex(&content).as_str())
            {
                errors.push(format!("{filename} does not match its manifest hash"));
            }
            let groups = match read_json_object(&path, &filename, &mut errors) {
                Some(groups) => groups,
                None => continue,
            };

            let mut actual_models: IndexMap<String, String> = IndexMap::new();
            for (api, value) in groups.as_object().expect("is_record guarantees an object") {
                if !is_record(value) {
                    errors.push(format!(
                        "{filename} API group {} must be an object",
                        stringify_json(&Value::String(api.clone()))
                    ));
                    continue;
                }
                for (model_key, model) in value.as_object().expect("checked above") {
                    if actual_models.contains_key(model_key) {
                        errors.push(format!(
                            "{provider_id}/{model_key} appears in more than one API group"
                        ));
                        continue;
                    }
                    actual_models.insert(model_key.clone(), api.clone());
                    let model_id = match model_key.find(':') {
                        Some(separator) => &model_key[separator + 1..],
                        None => model_key.as_str(),
                    };
                    validate_model_value(model, provider_id, model_id, api, &mut errors);
                    if is_record(model) {
                        let identity = format!(
                            "{}:{}",
                            model
                                .get("type")
                                .map(|value| kiana_json::js_string::js_string(Some(value)))
                                .unwrap_or_else(|| "undefined".to_string()),
                            model
                                .get("id")
                                .map(|value| kiana_json::js_string::js_string(Some(value)))
                                .unwrap_or_else(|| "undefined".to_string())
                        );
                        if model_key != &identity {
                            errors.push(format!(
                                "{provider_id}/{model_key} has mismatched type/id identity"
                            ));
                        }
                    }
                }
            }

            let mut expected_model_ids: Vec<String> = expected_models.keys().cloned().collect();
            expected_model_ids.sort_by(|a, b| code_unit_compare(a, b));
            let mut actual_model_ids: Vec<String> = actual_models.keys().cloned().collect();
            actual_model_ids.sort_by(|a, b| code_unit_compare(a, b));
            if !same_strings(&expected_model_ids, &actual_model_ids) {
                errors.push(format!(
                    "{filename} model IDs do not match the generated catalog ({})",
                    describe_set_difference(&expected_model_ids, &actual_model_ids)
                ));
            }
            for (model_id, expected_api) in expected_models {
                if let Some(actual_api) = actual_models.get(model_id)
                    && actual_api != expected_api
                {
                    errors.push(format!(
                        "{provider_id}/{model_id} is grouped under API {}, expected {}",
                        stringify_json(&Value::String(actual_api.clone())),
                        stringify_json(&Value::String(expected_api.clone()))
                    ));
                }
            }
        }

        if !errors.is_empty() {
            return Err(throw_validation_errors(&errors));
        }
        Ok(())
    }

    /// `validateGeneratedModelData(packageRoot)`。
    pub fn validate_generated_model_data(package_root: &Path) -> Result<(), CatalogError> {
        let structure = read_model_data_structure(package_root)?;
        validate_model_data_directory(
            &structure,
            &package_root.join("src").join("providers").join("data"),
        )
    }

    /// `ModelCatalogEntry`。
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct ModelCatalogEntry {
        /// 模型的 `type` 字段（`"chat"` / `"image"` / `"classifier"`）。
        pub entry_type: String,
        /// 模型 id（不含 `"type:"` 前缀）。
        pub id: String,
        /// 该模型所属的 API 名（数据文件里的分组键）。
        pub api: String,
    }

    /// `groupProviderModelData(providerId, models)` 的返回值。
    #[derive(Debug, Clone, Default, PartialEq)]
    pub struct GroupedProviderModelData {
        /// api -> `"type:id"` -> model。
        pub groups: IndexMap<String, IndexMap<String, Value>>,
        /// `"type:id"` -> api。
        pub structure: IndexMap<String, String>,
    }

    /// `groupProviderModelData(providerId, models)`：按 api（码元序）分组，组内保持输入顺序。
    pub fn group_provider_model_data(
        provider_id: &str,
        models: &[Value],
    ) -> Result<GroupedProviderModelData, CatalogError> {
        let mut apis: Vec<String> = Vec::new();
        for model in models {
            let api = model
                .get("api")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            if !apis.contains(&api) {
                apis.push(api);
            }
        }
        apis.sort_by(|a, b| code_unit_compare(a, b));

        let mut groups: IndexMap<String, IndexMap<String, Value>> = IndexMap::new();
        let mut structure: IndexMap<String, String> = IndexMap::new();
        for api in apis {
            let mut group: IndexMap<String, Value> = IndexMap::new();
            for model in models {
                if model.get("api").and_then(Value::as_str) != Some(api.as_str()) {
                    continue;
                }
                let entry_type = model
                    .get("type")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let id = model.get("id").and_then(Value::as_str).unwrap_or_default();
                let identity = format!("{entry_type}:{id}");
                if group.contains_key(&identity) {
                    return Err(CatalogError::new(format!(
                        "{provider_id}/{identity} has duplicate {api} catalog entries"
                    )));
                }
                group.insert(identity.clone(), model.clone());
                structure.insert(identity, api.clone());
            }
            groups.insert(api, group);
        }
        Ok(GroupedProviderModelData { groups, structure })
    }

    /// 把 `groups` 序列化成数据文件内容：`JSON.stringify(groups)` + `\n`。
    pub fn serialize_groups(groups: &IndexMap<String, IndexMap<String, Value>>) -> String {
        serialize_groups_pretty(groups, false)
    }

    /// 同 [`serialize_groups`]，但 `pretty` 对应 `--pretty`
    /// （`JSON.stringify(value, null, pretty ? 2 : undefined) + "\n"`）。
    ///
    /// `generate-models.ts` 用同一个 `serializeJson` 写数据分片，所以 `--pretty`
    /// 会同时影响分片与 `.manifest.json`；`hydrate-model-catalog.ts` 则始终紧凑。
    pub fn serialize_groups_pretty(
        groups: &IndexMap<String, IndexMap<String, Value>>,
        pretty: bool,
    ) -> String {
        let mut object = serde_json::Map::new();
        for (api, models) in groups {
            let mut inner = serde_json::Map::new();
            for (key, model) in models {
                inner.insert(key.clone(), model.clone());
            }
            object.insert(api.clone(), Value::Object(inner));
        }
        let value = Value::Object(object);
        if pretty {
            format!("{}\n", kiana_json::js_object::stringify_pretty(&value, 2))
        } else {
            format!("{}\n", stringify_json(&value))
        }
    }

    /// 供 `hydrate` 复用的固定 manifest 时间戳（公开目录无生成时间）。
    pub const HYDRATION_GENERATED_AT: &str = "1970-01-01T00:00:00.000Z";

    /// `packageRoot/src/providers/data/<provider_id>.json` 的路径。
    pub fn provider_data_path(package_root: &Path, provider_id: &str) -> PathBuf {
        package_root
            .join("src")
            .join("providers")
            .join("data")
            .join(format!("{provider_id}.json"))
    }
}

// ---------------------------------------------------------------------------
// openrouter-catalog.ts：从 OpenRouter 的列表构造目录
// ---------------------------------------------------------------------------

/// `openrouter-catalog.ts` + 其私有的 `openRouterCost`（`providers/openrouter-api.ts` 的定价转换
/// 属于 provider 单元；这里只保留目录生成所需的私有副本，避免跨单元重复实现）。
pub mod openrouter_catalog {
    use serde_json::{Map, Value, json};

    use super::reasoning_options::{
        OpenRouterReasoningMetadata, get_openrouter_thinking_level_map,
    };

    /// `openRouterCost`：把 OpenRouter 的 $/token 价格转成 kiana 的 $/1M。
    pub fn open_router_cost(pricing: Option<&Value>) -> Value {
        // `perMillion(value)` 执行 `Number.parseFloat(value ?? "")`：`value` 在类型上是
        // `string | undefined`，但运行时若上游给了数字，`parseFloat` 会先做 `String(value)`。
        // 因此数字按 JS 数字字符串解析（`1e-7` -> `"1e-7"`），字符串保持原样。
        let rate_of = |value: Option<&Value>, fallback: f64| -> f64 {
            match value {
                Some(Value::String(text)) => per_million(Some(text.as_str()), fallback),
                Some(Value::Number(number)) => {
                    let text =
                        kiana_json::js_num::format_js_number(number.as_f64().unwrap_or(f64::NAN));
                    per_million(Some(&text), fallback)
                }
                _ => per_million(None, fallback),
            }
        };
        let rate = |name: &str, fallback: f64| -> f64 {
            rate_of(pricing.and_then(|pricing| pricing.get(name)), fallback)
        };
        let base = [
            rate("prompt", 0.0),
            rate("completion", 0.0),
            rate("input_cache_read", 0.0),
            rate("input_cache_write", 0.0),
        ];

        let mut tiers: Vec<Value> = Vec::new();
        let overrides = pricing
            .and_then(|pricing| pricing.get("overrides"))
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        // 只收纯 token 档位的 override；带 utc_start/utc_end/utc_days 的时间窗口档位被跳过。
        for override_value in overrides {
            let Some(over) = override_value.as_object() else {
                continue;
            };
            let Some(min_prompt_tokens) = over.get("min_prompt_tokens") else {
                continue;
            };
            if over.contains_key("utc_start")
                || over.contains_key("utc_end")
                || over.contains_key("utc_days")
            {
                continue;
            }
            let tier_rate =
                |name: &str, fallback: f64| -> f64 { rate_of(over.get(name), fallback) };
            let mut tier = Map::new();
            tier.insert("inputTokensAbove".to_string(), min_prompt_tokens.clone());
            tier.insert("input".to_string(), json!(tier_rate("prompt", base[0])));
            tier.insert(
                "output".to_string(),
                json!(tier_rate("completion", base[1])),
            );
            tier.insert(
                "cacheRead".to_string(),
                json!(tier_rate("input_cache_read", base[2])),
            );
            tier.insert(
                "cacheWrite".to_string(),
                json!(tier_rate("input_cache_write", base[3])),
            );
            tiers.push(Value::Object(tier));
        }

        let mut cost = Map::new();
        cost.insert("input".to_string(), json!(base[0]));
        cost.insert("output".to_string(), json!(base[1]));
        cost.insert("cacheRead".to_string(), json!(base[2]));
        cost.insert("cacheWrite".to_string(), json!(base[3]));
        if !tiers.is_empty() {
            cost.insert("tiers".to_string(), Value::Array(tiers));
        }
        Value::Object(cost)
    }

    /// `perMillion(value, fallback)`（`Number.parseFloat(value ?? "")`）。
    fn per_million(value: Option<&str>, fallback: f64) -> f64 {
        let per_token = match value {
            Some(text) => super::js_parse_float(text),
            None => f64::NAN,
        };
        if per_token >= 0.0 {
            super::round_cost(per_token * 1_000_000.0)
        } else {
            fallback
        }
    }

    /// 取 `string[]` 中 `"text"`/`"image"` 的去重序列（保持首次出现顺序）。
    fn modalities(values: Option<&Value>) -> Vec<Value> {
        let mut result: Vec<Value> = Vec::new();
        if let Some(items) = values.and_then(Value::as_array) {
            for item in items {
                if matches!(item.as_str(), Some("text" | "image")) && !result.contains(item) {
                    result.push(item.clone());
                }
            }
        }
        result
    }

    /// `value.includes(needle)`：TS 的 `supported_parameters`/`modality` 类型上是数组或字符串，
    /// 字符串同样有 `String.prototype.includes`（子串匹配）。
    fn includes_string(values: Option<&Value>, needle: &str) -> bool {
        match values {
            Some(Value::Array(items)) => items.iter().any(|item| item.as_str() == Some(needle)),
            Some(Value::String(text)) => text.contains(needle),
            _ => false,
        }
    }

    /// JS 真值（`||` 左侧）。
    fn js_truthy(value: &Value) -> bool {
        match value {
            Value::Null => false,
            Value::Bool(flag) => *flag,
            Value::Number(number) => number
                .as_f64()
                .map(|number| number != 0.0 && !number.is_nan())
                .unwrap_or(true),
            Value::String(text) => !text.is_empty(),
            Value::Array(_) | Value::Object(_) => true,
        }
    }

    /// `value || fallback`（保留上游的非 number 值，例如字符串 `context_length`）。
    fn positive_or(value: Option<&Value>, fallback: Value) -> Value {
        match value {
            Some(value) if js_truthy(value) => value.clone(),
            _ => fallback,
        }
    }

    /// `buildOpenRouterCatalog(listed, imageListed, decisionListed)` 的返回值。
    #[derive(Debug, Clone, Default, PartialEq)]
    pub struct OpenRouterCatalog {
        /// 默认列表里 `supported_parameters` 含 `tools` 的 chat 模型。
        pub chat: Vec<Value>,
        /// `output_modalities=image` 列表里输出含 `image` 的模型（按 id 去重）。
        pub images: Vec<Value>,
        /// `output_modalities=decisions` 列表里输出含 `decisions` 的模型（按 id 去重）。
        pub classifiers: Vec<Value>,
    }

    /// `buildOpenRouterCatalog`：默认列表给 chat、`output_modalities=image` 给图像、
    /// `output_modalities=decisions` 给 classifier；同一 id 每类各一条。
    pub fn build_open_router_catalog(
        listed: &[Value],
        image_listed: &[Value],
        decision_listed: &[Value],
    ) -> OpenRouterCatalog {
        // chat 只收 `supported_parameters` 含 `tools` 的模型；`anthropic/*`（`:batch` 变体除外）
        // 走 anthropic-messages，其余走 openai-completions（两套 baseUrl 也因此不同）。
        let mut chat: Vec<Value> = Vec::new();
        for model in listed {
            if !includes_string(model.get("supported_parameters"), "tools") {
                continue;
            }
            let id = model
                .get("id")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            let name = model
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            let mut input: Vec<Value> = vec![Value::String("text".to_string())];
            let modality_has_image = includes_string(
                model
                    .get("architecture")
                    .and_then(|architecture| architecture.get("modality")),
                "image",
            );
            if modality_has_image {
                input.push(Value::String("image".to_string()));
            }

            let reasoning_metadata = model
                .get("reasoning")
                .and_then(OpenRouterReasoningMetadata::from_json);
            let thinking_level_map = get_openrouter_thinking_level_map(reasoning_metadata.as_ref());
            let use_anthropic_messages = id.starts_with("anthropic/") && !id.ends_with(":batch");

            let mut entry = Map::new();
            entry.insert("type".to_string(), Value::String("chat".to_string()));
            entry.insert("id".to_string(), Value::String(id.clone()));
            entry.insert("name".to_string(), Value::String(name));
            entry.insert(
                "api".to_string(),
                Value::String(
                    if use_anthropic_messages {
                        "anthropic-messages"
                    } else {
                        "openai-completions"
                    }
                    .to_string(),
                ),
            );
            entry.insert(
                "baseUrl".to_string(),
                Value::String(
                    if use_anthropic_messages {
                        "https://openrouter.ai/api"
                    } else {
                        "https://openrouter.ai/api/v1"
                    }
                    .to_string(),
                ),
            );
            entry.insert(
                "provider".to_string(),
                Value::String("openrouter".to_string()),
            );
            entry.insert(
                "reasoning".to_string(),
                Value::Bool(includes_string(
                    model.get("supported_parameters"),
                    "reasoning",
                )),
            );
            if let Some(map) = thinking_level_map {
                entry.insert(
                    "thinkingLevelMap".to_string(),
                    super::thinking_level_map_to_json(&map),
                );
            }
            entry.insert("input".to_string(), Value::Array(input));
            entry.insert("cost".to_string(), open_router_cost(model.get("pricing")));
            // `top_provider.context_length || model.context_length || 4096`：任何真值都原样保留
            // （上游可能给字符串），只有缺失/0/空串才回落。
            let context_window = positive_or(
                model
                    .get("top_provider")
                    .and_then(|top| top.get("context_length")),
                positive_or(model.get("context_length"), json!(4096)),
            );
            entry.insert("contextWindow".to_string(), context_window);
            let max_tokens = positive_or(
                model
                    .get("top_provider")
                    .and_then(|top| top.get("max_completion_tokens")),
                json!(4096),
            );
            entry.insert("maxTokens".to_string(), max_tokens);
            chat.push(Value::Object(entry));
        }

        // image 列表按 id 去重（先到先得），且输出模态必须含 image；
        // input 模态缺失时回落 `["text"]`，与 TS 的 `input.length ? input : ["text"]` 一致。
        let mut images: Vec<Value> = Vec::new();
        for model in image_listed {
            let id = model
                .get("id")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            if images
                .iter()
                .any(|entry| entry.get("id").and_then(Value::as_str) == Some(id.as_str()))
            {
                continue;
            }
            let output = modalities(
                model
                    .get("architecture")
                    .and_then(|architecture| architecture.get("output_modalities")),
            );
            if !output.iter().any(|value| value.as_str() == Some("image")) {
                continue;
            }
            let input = modalities(
                model
                    .get("architecture")
                    .and_then(|architecture| architecture.get("input_modalities")),
            );
            let mut entry = Map::new();
            entry.insert("type".to_string(), Value::String("image".to_string()));
            entry.insert("id".to_string(), Value::String(id));
            entry.insert(
                "name".to_string(),
                Value::String(
                    model
                        .get("name")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                ),
            );
            entry.insert(
                "api".to_string(),
                Value::String("openrouter-images".to_string()),
            );
            entry.insert(
                "provider".to_string(),
                Value::String("openrouter".to_string()),
            );
            entry.insert(
                "baseUrl".to_string(),
                Value::String("https://openrouter.ai/api/v1".to_string()),
            );
            entry.insert(
                "input".to_string(),
                Value::Array(if input.is_empty() {
                    vec![Value::String("text".to_string())]
                } else {
                    input
                }),
            );
            entry.insert("output".to_string(), Value::Array(output));
            entry.insert("cost".to_string(), open_router_cost(model.get("pricing")));
            images.push(Value::Object(entry));
        }

        // classifier 列表同样按 id 去重，只收输出模态含 decisions 的模型。
        let mut classifiers: Vec<Value> = Vec::new();
        for model in decision_listed {
            let id = model
                .get("id")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            if classifiers
                .iter()
                .any(|entry| entry.get("id").and_then(Value::as_str) == Some(id.as_str()))
            {
                continue;
            }
            let decision_outputs = includes_string(
                model
                    .get("architecture")
                    .and_then(|architecture| architecture.get("output_modalities")),
                "decisions",
            );
            if !decision_outputs {
                continue;
            }
            let input = modalities(
                model
                    .get("architecture")
                    .and_then(|architecture| architecture.get("input_modalities")),
            );
            let mut entry = Map::new();
            entry.insert("type".to_string(), Value::String("classifier".to_string()));
            entry.insert("id".to_string(), Value::String(id));
            entry.insert(
                "name".to_string(),
                Value::String(
                    model
                        .get("name")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                ),
            );
            entry.insert(
                "api".to_string(),
                Value::String("typesafe-system-one".to_string()),
            );
            entry.insert(
                "provider".to_string(),
                Value::String("openrouter".to_string()),
            );
            entry.insert(
                "baseUrl".to_string(),
                Value::String("https://openrouter.ai/api/v1".to_string()),
            );
            entry.insert(
                "input".to_string(),
                Value::Array(if input.is_empty() {
                    vec![Value::String("text".to_string())]
                } else {
                    input
                }),
            );
            entry.insert("cost".to_string(), open_router_cost(model.get("pricing")));
            let context_window = positive_or(
                model
                    .get("top_provider")
                    .and_then(|top| top.get("context_length")),
                positive_or(model.get("context_length"), json!(4096)),
            );
            entry.insert("contextWindow".to_string(), context_window);
            entry.insert(
                "compat".to_string(),
                json!({ "imageInput": "state-image-urls", "sendImageOptInHeader": false }),
            );
            classifiers.push(Value::Object(entry));
        }

        OpenRouterCatalog {
            chat,
            images,
            classifiers,
        }
    }
}

/// `ThinkingLevelMap` → JSON（键按 map 的插入序，`None` 为 null）。
pub fn thinking_level_map_to_json(map: &ThinkingLevelMap) -> Value {
    let mut object = serde_json::Map::new();
    for (level, value) in map {
        let key = match serde_json::to_value(level) {
            Ok(Value::String(key)) => key,
            _ => continue,
        };
        object.insert(key, value.clone().map(Value::String).unwrap_or(Value::Null));
    }
    Value::Object(object)
}

// ---------------------------------------------------------------------------
// scripts/model-catalog-protocol.ts：kiana 与 kiana.dev 共享的目录分发协议
// ---------------------------------------------------------------------------

/// 仓库根 `scripts/model-catalog-protocol.ts`（逐字复制到 kiana.dev）。
pub mod protocol {
    use serde_json::{Map, Value};

    use super::CatalogError;

    /// `MODEL_CATALOG_SCHEMA_VERSION`。
    pub const MODEL_CATALOG_SCHEMA_VERSION: u64 = 1;
    /// `MODEL_CATALOG_PREFIX`。
    pub const MODEL_CATALOG_PREFIX: &str = "models/v1";
    /// `MODEL_CATALOG_INDEX_KEY`。
    pub const MODEL_CATALOG_INDEX_KEY: &str = "models/v1/index.json";
    /// `MODEL_CATALOG_REVISION_RE` 的模式文本。
    pub const MODEL_CATALOG_REVISION_PATTERN: &str = "^sha256-[0-9a-f]{64}$";

    /// `MODEL_CATALOG_REVISION_RE`。
    pub fn is_model_catalog_revision(value: &str) -> bool {
        let Some(rest) = value.strip_prefix("sha256-") else {
            return false;
        };
        rest.len() == 64
            && rest
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    }

    /// `ModelCatalogArtifact`。
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum ModelCatalogArtifact {
        /// `models.json`：只有 chat 表。
        Models,
        /// `models.all.json`：chat + image + classifier。
        ModelsAll,
        /// `providers.json`：provider id 列表。
        Providers,
    }

    impl ModelCatalogArtifact {
        /// 文件名字面量。
        pub fn as_str(self) -> &'static str {
            match self {
                ModelCatalogArtifact::Models => "models.json",
                ModelCatalogArtifact::ModelsAll => "models.all.json",
                ModelCatalogArtifact::Providers => "providers.json",
            }
        }

        /// 从字面量解析；未知文件名返回 `None`（TS 类型层面不可达）。
        ///
        /// 命名避开 `from_str`：语义是「可失败解析」而非 `FromStr`（错误类型不是 `Err`）。
        pub fn parse(value: &str) -> Option<Self> {
            match value {
                "models.json" => Some(ModelCatalogArtifact::Models),
                "models.all.json" => Some(ModelCatalogArtifact::ModelsAll),
                "providers.json" => Some(ModelCatalogArtifact::Providers),
                _ => None,
            }
        }
    }

    /// `ModelCatalogRepresentation`。
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum ModelCatalogRepresentation {
        /// 旧版：`providers/<provider>.json`（只有 chat 表）。
        Legacy,
        /// 新版：`providers/<provider>.all.json`（全部模型类型）。
        Typed,
    }

    /// `getModelCatalogArtifactKey(revision, artifact)`。
    pub fn get_model_catalog_artifact_key(
        revision: &str,
        artifact: ModelCatalogArtifact,
    ) -> String {
        format!(
            "{MODEL_CATALOG_PREFIX}/revisions/{revision}/{}",
            artifact.as_str()
        )
    }

    /// `getModelCatalogProviderKey(revision, provider, representation)`。
    pub fn get_model_catalog_provider_key(
        revision: &str,
        provider: &str,
        representation: ModelCatalogRepresentation,
    ) -> String {
        let suffix = if representation == ModelCatalogRepresentation::Typed {
            ".all.json"
        } else {
            ".json"
        };
        format!("{MODEL_CATALOG_PREFIX}/revisions/{revision}/providers/{provider}{suffix}")
    }

    /// `[major, minor, patch, prerelease]`（`prerelease` 为空串表示正式版）。
    pub type ParsedKianaVersion = (u64, u64, u64, String);

    /// `parseKianaVersion(version)`：`KIANA_VERSION_RE` + `Number.isSafeInteger` 三重校验。
    ///
    /// `version.trim()` 是 JS `String.prototype.trim`：含 U+FEFF、不含 U+0085，
    /// 因此不能用 Rust 的 `str::trim`。
    pub fn parse_kiana_version(version: &str) -> Option<ParsedKianaVersion> {
        let trimmed = kiana_json::js_trim(version);
        let parsed = parse_kiana_version_pattern(trimmed)?;
        Some(parsed)
    }

    /// `KIANA_VERSION_RE = /^v?(\d+)\.(\d+)\.(\d+)(?:-([0-9A-Za-z.-]+))?(?:\+[0-9A-Za-z.-]+)?$/` 的手写匹配。
    ///
    /// 手写而非 `regex`：三段数字各自按 JS `Number()` 语义转成 f64 并检查 `Number.isSafeInteger`，
    /// 超过 `2^53-1` 的值必须被拒绝（`regex` + `u64` 会接受它们）。
    fn parse_kiana_version_pattern(text: &str) -> Option<ParsedKianaVersion> {
        let mut rest = text;
        if let Some(stripped) = rest.strip_prefix('v') {
            rest = stripped;
        }
        let (major, rest) = take_digits(rest)?;
        let rest = rest.strip_prefix('.')?;
        let (minor, rest) = take_digits(rest)?;
        let rest = rest.strip_prefix('.')?;
        let (patch, mut rest) = take_digits(rest)?;
        let mut prerelease = String::new();
        if let Some(stripped) = rest.strip_prefix('-') {
            let end = stripped
                .find(|c: char| !(c.is_ascii_alphanumeric() || c == '.' || c == '-'))
                .unwrap_or(stripped.len());
            if end == 0 {
                return None;
            }
            prerelease = stripped[..end].to_string();
            rest = &stripped[end..];
        }
        if let Some(stripped) = rest.strip_prefix('+') {
            let end = stripped
                .find(|c: char| !(c.is_ascii_alphanumeric() || c == '.' || c == '-'))
                .unwrap_or(stripped.len());
            if end == 0 {
                return None;
            }
            rest = &stripped[end..];
        }
        if !rest.is_empty() {
            return None;
        }
        Some((major, minor, patch, prerelease))
    }

    fn take_digits(text: &str) -> Option<(u64, &str)> {
        let end = text
            .find(|c: char| !c.is_ascii_digit())
            .unwrap_or(text.len());
        if end == 0 {
            return None;
        }
        let digits = &text[..end];
        let value: f64 = digits.parse::<f64>().ok()?;
        // `Number.isSafeInteger`：有限、整数、|x| <= 2^53-1。
        if !value.is_finite() || value.fract() != 0.0 || value.abs() > 9_007_199_254_740_991.0 {
            return None;
        }
        Some((value as u64, &text[end..]))
    }

    /// `isValidModelCatalogKianaVersion(version)`。
    pub fn is_valid_model_catalog_kiana_version(version: &str) -> bool {
        parse_kiana_version(version).is_some()
    }

    /// `requireKianaVersion(version)`。
    pub fn require_kiana_version(version: &str) -> Result<ParsedKianaVersion, CatalogError> {
        parse_kiana_version(version)
            .ok_or_else(|| CatalogError::new(format!("Invalid Kiana version: {version}")))
    }

    /// `compareKianaVersions(left, right)`：semver 优先级（返回 `left - right`，与 TS 一样是
    /// `number`，因此超大数字标识符的差是 `NaN`）。
    pub fn compare_kiana_versions(left: &ParsedKianaVersion, right: &ParsedKianaVersion) -> f64 {
        // 先比 major/minor/patch：TS 直接返回两数之差（可能超出 ±1）。
        for index in 0..3 {
            let left_part = [left.0, left.1, left.2][index] as f64;
            let right_part = [right.0, right.1, right.2][index] as f64;
            let difference = left_part - right_part;
            if difference != 0.0 {
                return difference;
            }
        }
        // 核心版本相同：正式版（prerelease 为空）高于任何预发布版，两者都为空则相等。
        let left_prerelease = &left.3;
        let right_prerelease = &right.3;
        if left_prerelease == right_prerelease {
            return 0.0;
        }
        if left_prerelease.is_empty() {
            return 1.0;
        }
        if right_prerelease.is_empty() {
            return -1.0;
        }
        let left_parts: Vec<&str> = left_prerelease.split('.').collect();
        let right_parts: Vec<&str> = right_prerelease.split('.').collect();
        // 预发布标识按点分段比较：数字段按数值（超长数字解析成 Infinity，差为 NaN），
        // 数字段低于字母段；两段都是字母时用 localeCompare（en-US）。
        let count = left_parts.len().max(right_parts.len());
        for index in 0..count {
            let left_part = left_parts.get(index);
            let right_part = right_parts.get(index);
            let (Some(left_part), Some(right_part)) = (left_part, right_part) else {
                return if left_part.is_none() { -1.0 } else { 1.0 };
            };
            if left_part == right_part {
                continue;
            }
            let left_number = numeric_part(left_part);
            let right_number = numeric_part(right_part);
            if let (Some(left_number), Some(right_number)) = (left_number, right_number) {
                // JS `Number(a) - Number(b)`：400 位数字两者都是 `Infinity`，差为 `NaN`。
                return left_number - right_number;
            }
            if left_number.is_some() {
                return -1.0;
            }
            if right_number.is_some() {
                return 1.0;
            }
            let ordering = super::locale_compare(left_part, right_part);
            if ordering != std::cmp::Ordering::Equal {
                return if ordering == std::cmp::Ordering::Less {
                    -1.0
                } else {
                    1.0
                };
            }
        }
        0.0
    }

    /// `Number(value)`（`/^\d+$/` 已保证是十进制数字串；超长串变成 `Infinity`）。
    fn numeric_part(value: &str) -> Option<f64> {
        if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
            return None;
        }
        Some(value.parse::<f64>().unwrap_or(f64::INFINITY))
    }

    /// `compareModelCatalogKianaVersions(left, right)`（TS 返回 `number`，可能是 `NaN`）。
    pub fn compare_model_catalog_kiana_versions(
        left: &str,
        right: &str,
    ) -> Result<f64, CatalogError> {
        let left = require_kiana_version(left)?;
        let right = require_kiana_version(right)?;
        Ok(compare_kiana_versions(&left, &right))
    }

    /// `MODEL_TYPE_RE = /^[A-Za-z0-9][A-Za-z0-9._-]*$/`。
    fn is_model_type(value: &str) -> bool {
        let mut bytes = value.bytes();
        match bytes.next() {
            Some(first) if first.is_ascii_alphanumeric() => {}
            _ => return false,
        }
        bytes.all(|byte| {
            byte.is_ascii_alphanumeric() || byte == b'.' || byte == b'_' || byte == b'-'
        })
    }

    /// `parseModelCatalogRepresentation(types)`。
    pub fn parse_model_catalog_representation(
        types: Option<&str>,
    ) -> Option<ModelCatalogRepresentation> {
        match types {
            None => Some(ModelCatalogRepresentation::Legacy),
            Some(types) => {
                if types.is_empty()
                    || types
                        .split(',')
                        .any(|model_type| !is_model_type(model_type))
                {
                    return None;
                }
                Some(ModelCatalogRepresentation::Typed)
            }
        }
    }

    /// JS `\s` 的字符类片段（`WhiteSpace` + `LineTerminator`）。
    ///
    /// 与 Rust `regex` 的 `\s` 差两个字符：JS 含 U+FEFF、不含 U+0085。
    const JS_WHITESPACE_CLASS: &str = r"\t\n\x{0B}\x{0C}\r \x{A0}\x{1680}\x{2000}-\x{200A}\x{2028}\x{2029}\x{202F}\x{205F}\x{3000}\x{FEFF}";

    /// `KIANA_USER_AGENT_RE`：`/^kiana\/([^\s()]+)(?: \([^;()]+(?:;\s*[^;()]+(?:;\s*[^()]+)?)?\))?$/i`。
    ///
    /// 前缀 `kiana/` 按 ASCII 大小写不敏感手工匹配（`/i`）；其余部分用 `regex` 匹配，但把 `\s`
    /// 换成显式的 JS 空白类（见 [`JS_WHITESPACE_CLASS`]），因为两个空白集合不同。返回捕获组 1。
    fn parse_kiana_user_agent(user_agent: &str) -> Option<&str> {
        const PREFIX: &str = "kiana/";
        if user_agent.len() < PREFIX.len()
            || !user_agent.as_bytes()[..PREFIX.len()].eq_ignore_ascii_case(PREFIX.as_bytes())
        {
            return None;
        }
        // 前缀是单字节 ASCII，字节下标 PREFIX.len() 一定是字符边界。
        let rest = &user_agent[PREFIX.len()..];

        static PATTERN: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
        let pattern = PATTERN.get_or_init(|| {
            let ws = JS_WHITESPACE_CLASS;
            regex::Regex::new(&format!(
                r"^([^{ws}()]+)(?: \([^;()]+(?:;[{ws}]*[^;()]+(?:;[{ws}]*[^()]+)?)?\))?$"
            ))
            .expect("KIANA_USER_AGENT_RE is a valid regex")
        });
        pattern.captures(rest).map(|captures| {
            let matched = captures.get(1).expect("group 1 always participates");
            let start = matched.start();
            let end = matched.end();
            &rest[start..end]
        })
    }

    /// `ModelCatalogRequest`。
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum ModelCatalogRequest {
        /// 目录请求：`kianaVersion` 为 `None` 表示未指定。
        Catalog {
            kiana_version: Option<String>,
            representation: ModelCatalogRepresentation,
        },
        /// 重定向到带显式 `kiana-version` 的同一 URL。
        Redirect { location: String },
        /// 参数非法。
        Invalid { error: String },
    }

    /// 解析后的 URL（`new URL(url)` 的小型等价物）。
    struct JsUrl {
        base: String,
        pairs: Vec<(String, String)>,
        fragment: Option<String>,
    }

    impl JsUrl {
        fn parse(url: &str) -> Option<JsUrl> {
            let parsed = url::Url::parse(url).ok()?;
            let base = parsed[..url::Position::AfterPath].to_string();
            let pairs = match parsed.query() {
                Some(query) => parse_query(query),
                None => Vec::new(),
            };
            let fragment = parsed.fragment().map(str::to_string);
            Some(JsUrl {
                base,
                pairs,
                fragment,
            })
        }

        fn get(&self, name: &str) -> Option<&str> {
            self.pairs
                .iter()
                .find(|(key, _)| key == name)
                .map(|(_, value)| value.as_str())
        }

        fn set(&mut self, name: &str, value: &str) {
            // URLSearchParams.set：替换第一个同名参数、删除其余同名项；不存在时追加到末尾。
            let mut first = true;
            let mut replaced: Vec<(String, String)> = Vec::new();
            for (key, existing) in self.pairs.drain(..) {
                if key == name {
                    if first {
                        replaced.push((key, value.to_string()));
                        first = false;
                    }
                    continue;
                }
                replaced.push((key, existing));
            }
            if first {
                replaced.push((name.to_string(), value.to_string()));
            }
            self.pairs = replaced;
        }

        fn to_href(&self) -> String {
            let query: Vec<String> = self
                .pairs
                .iter()
                .map(|(key, value)| format!("{}={}", form_encode(key), form_encode(value)))
                .collect();
            let mut out = self.base.clone();
            if !query.is_empty() {
                out.push('?');
                out.push_str(&query.join("&"));
            }
            if let Some(fragment) = &self.fragment {
                out.push('#');
                out.push_str(fragment);
            }
            out
        }
    }

    impl std::fmt::Display for JsUrl {
        fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter.write_str(&self.to_href())
        }
    }

    fn form_encode(value: &str) -> String {
        let mut out = String::with_capacity(value.len());
        for byte in value.as_bytes() {
            match byte {
                b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'*' | b'-' | b'.' | b'_' => {
                    out.push(*byte as char)
                }
                b' ' => out.push('+'),
                _ => out.push_str(&format!("%{byte:02X}")),
            }
        }
        out
    }

    fn form_decode(value: &str) -> String {
        let bytes = value.as_bytes();
        let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
        let mut index = 0;
        // 无效的 `%xx` 原样保留 `%`（URLSearchParams 的容错解码），不报错。
        while index < bytes.len() {
            match bytes[index] {
                b'+' => {
                    out.push(b' ');
                    index += 1;
                }
                b'%' if index + 2 < bytes.len() => {
                    let high = (bytes[index + 1] as char).to_digit(16);
                    let low = (bytes[index + 2] as char).to_digit(16);
                    match (high, low) {
                        (Some(high), Some(low)) => {
                            out.push((high * 16 + low) as u8);
                            index += 3;
                        }
                        _ => {
                            out.push(b'%');
                            index += 1;
                        }
                    }
                }
                byte => {
                    out.push(byte);
                    index += 1;
                }
            }
        }
        String::from_utf8_lossy(&out).into_owned()
    }

    fn parse_query(query: &str) -> Vec<(String, String)> {
        if query.is_empty() {
            return Vec::new();
        }
        query
            .split('&')
            .filter(|pair| !pair.is_empty())
            .map(|pair| match pair.split_once('=') {
                Some((key, value)) => (form_decode(key), form_decode(value)),
                None => (form_decode(pair), String::new()),
            })
            .collect()
    }

    /// `parseModelCatalogRequest(url, userAgent)`。
    ///
    /// 三种结果：`Catalog`（参数合法，版本可能缺省）、`Redirect`（URL 没有 `kiana-version`，
    /// 但 User-Agent 带合法版本时，把它写进查询串后重定向到同一地址）、`Invalid`（`types` 或
    /// `kiana-version` 不合法）。`new URL` 解析失败返回 `Invalid URL` 错误。
    pub fn parse_model_catalog_request(
        url: &str,
        user_agent: Option<&str>,
    ) -> Result<ModelCatalogRequest, CatalogError> {
        // `new URL(url)` 抛 `TypeError: Invalid URL`（消息不含输入）。
        let mut request_url = JsUrl::parse(url).ok_or_else(|| CatalogError::new("Invalid URL"))?;
        let representation = parse_model_catalog_representation(request_url.get("types"));
        let Some(representation) = representation else {
            return Ok(ModelCatalogRequest::Invalid {
                error: "Invalid model types.".to_string(),
            });
        };

        match request_url.get("kiana-version").map(str::to_string) {
            None => {
                let user_agent_version = user_agent.and_then(parse_kiana_user_agent);
                if let Some(version) = user_agent_version
                    && is_valid_model_catalog_kiana_version(version)
                {
                    request_url.set("kiana-version", version);
                    return Ok(ModelCatalogRequest::Redirect {
                        location: request_url.to_href(),
                    });
                }
                Ok(ModelCatalogRequest::Catalog {
                    kiana_version: None,
                    representation,
                })
            }
            Some(kiana_version) => {
                if !is_valid_model_catalog_kiana_version(&kiana_version) {
                    return Ok(ModelCatalogRequest::Invalid {
                        error: "Invalid Kiana version.".to_string(),
                    });
                }
                Ok(ModelCatalogRequest::Catalog {
                    kiana_version: Some(kiana_version),
                    representation,
                })
            }
        }
    }

    /// `ModelCatalogIndexEntry`。
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct ModelCatalogIndexEntry {
        /// 该目录要求的最低 Kiana 版本（semver 串，例如 `"v1.2.3"`）。
        pub minimum_kiana_version: String,
        /// 目录 revision，格式固定为 `sha256-<64 位小写十六进制>`。
        pub revision: String,
    }

    /// `ModelCatalogIndex`。
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct ModelCatalogIndex {
        /// 协议版本；`parse_model_catalog_index` 只接受 `1`。
        pub schema_version: u64,
        /// 未指定客户端版本时使用的 revision；必须出现在 `catalogs` 中。
        pub default_revision: String,
        /// 按 `minimumKianaVersion` 分档的可用目录；不能为空。
        pub catalogs: Vec<ModelCatalogIndexEntry>,
    }

    impl ModelCatalogIndex {
        /// `toJSON` 形状（`schemaVersion`、`defaultRevision`、`catalogs`）。
        pub fn to_json_value(&self) -> Value {
            let catalogs: Vec<Value> = self
                .catalogs
                .iter()
                .map(|entry| {
                    let mut object = Map::new();
                    object.insert(
                        "minimumKianaVersion".to_string(),
                        Value::String(entry.minimum_kiana_version.clone()),
                    );
                    object.insert(
                        "revision".to_string(),
                        Value::String(entry.revision.clone()),
                    );
                    Value::Object(object)
                })
                .collect();
            let mut root = Map::new();
            root.insert(
                "schemaVersion".to_string(),
                Value::from(self.schema_version),
            );
            root.insert(
                "defaultRevision".to_string(),
                Value::String(self.default_revision.clone()),
            );
            root.insert("catalogs".to_string(), Value::Array(catalogs));
            Value::Object(root)
        }
    }

    /// `parseModelCatalogIndex(value)`：只保留协议字段（丢弃发布元数据）。
    pub fn parse_model_catalog_index(value: &Value) -> Result<ModelCatalogIndex, CatalogError> {
        let invalid = || {
            CatalogError::new(format!(
                "Model catalog index is invalid: {MODEL_CATALOG_INDEX_KEY}"
            ))
        };
        let Some(object) = value.as_object() else {
            return Err(invalid());
        };

        // schemaVersion 必须能按数字读到 1（`1.0` 可接受；字符串 `"1"` 不接受）。
        let schema_version_ok =
            object.get("schemaVersion").map(|value| value.as_f64()) == Some(Some(1.0));
        if !schema_version_ok {
            return Err(invalid());
        }
        let Some(default_revision) = object.get("defaultRevision").and_then(Value::as_str) else {
            return Err(invalid());
        };
        if !is_model_catalog_revision(default_revision) {
            return Err(invalid());
        }
        let Some(catalogs) = object.get("catalogs").and_then(Value::as_array) else {
            return Err(invalid());
        };
        if catalogs.is_empty() {
            return Err(invalid());
        }
        // 每个条目都要有可解析的最低版本与合法 revision；最后要求 defaultRevision
        // 指向其中一个条目，否则客户端拿不到目录。
        for catalog in catalogs {
            let Some(catalog) = catalog.as_object() else {
                return Err(invalid());
            };
            let Some(minimum_kiana_version) =
                catalog.get("minimumKianaVersion").and_then(Value::as_str)
            else {
                return Err(invalid());
            };
            if parse_kiana_version(minimum_kiana_version).is_none() {
                return Err(invalid());
            }
            let Some(revision) = catalog.get("revision").and_then(Value::as_str) else {
                return Err(invalid());
            };
            if !is_model_catalog_revision(revision) {
                return Err(invalid());
            }
        }
        if !catalogs.iter().any(|catalog| {
            catalog.get("revision").and_then(Value::as_str) == Some(default_revision)
        }) {
            return Err(invalid());
        }

        Ok(ModelCatalogIndex {
            schema_version: MODEL_CATALOG_SCHEMA_VERSION,
            default_revision: default_revision.to_string(),
            catalogs: catalogs
                .iter()
                .map(|catalog| ModelCatalogIndexEntry {
                    minimum_kiana_version: catalog
                        .get("minimumKianaVersion")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                    revision: catalog
                        .get("revision")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                })
                .collect(),
        })
    }

    /// `selectModelCatalog(index, kianaVersion)`。
    ///
    /// 未指定版本时返回 `defaultRevision` 对应条目；指定版本时返回「最低版本 <= 请求版本」
    /// 中最低版本最大的条目；请求版本无法解析或没有条目满足时返回 `Ok(None)`。
    pub fn select_model_catalog(
        index: &ModelCatalogIndex,
        kiana_version: Option<&str>,
    ) -> Result<Option<ModelCatalogIndexEntry>, CatalogError> {
        let Some(kiana_version) = kiana_version else {
            return Ok(index
                .catalogs
                .iter()
                .find(|catalog| catalog.revision == index.default_revision)
                .cloned());
        };
        let Some(requested_version) = parse_kiana_version(kiana_version) else {
            return Ok(None);
        };

        // 在「最低版本 <= 请求版本」的条目里选最低版本最大的一个；版本比较可能是 NaN
        // （超长数字标识符），NaN 参与的比较恒为 false，该条目自然被跳过。
        let mut selected: Option<ModelCatalogIndexEntry> = None;
        let mut selected_version: Option<ParsedKianaVersion> = None;
        for catalog in &index.catalogs {
            let minimum_version = require_kiana_version(&catalog.minimum_kiana_version)?;
            // TS 的比较结果可能是 `NaN`：`NaN <= 0` 与 `NaN > 0` 都是 false，该条目被跳过。
            let within = compare_kiana_versions(&minimum_version, &requested_version) <= 0.0;
            let newer = match &selected_version {
                None => true,
                Some(previous) => compare_kiana_versions(&minimum_version, previous) > 0.0,
            };
            if within && newer {
                selected = Some(catalog.clone());
                selected_version = Some(minimum_version);
            }
        }
        Ok(selected)
    }
}

// ---------------------------------------------------------------------------
// hydrate-model-catalog.ts：用已发布的 models.all.json 离线重放数据文件
// ---------------------------------------------------------------------------

/// `hydrate-model-catalog.ts`。
pub mod hydrate {
    use std::path::Path;

    use indexmap::IndexMap;
    use serde_json::Value;

    use super::model_data::{
        HYDRATION_GENERATED_AT, MODEL_DATA_MANIFEST_FILE, ModelDataStructure,
        create_model_data_manifest, group_provider_model_data, node_fs_error, node_read_error,
        read_file_utf8, read_model_data_provider_ids, serialize_groups,
        validate_model_data_directory,
    };
    use super::{CatalogError, stringify_json};

    /// `hydrateModelCatalog(packageRoot, catalogPath, options)`。
    ///
    /// 读取已发布的 `models.all.json`，按 provider 重放 `providers/data/<id>.json` 与
    /// `.manifest.json`，再跑 [`super::model_data::validate_model_data_directory`]。
    /// 解析/校验失败原样返回；`validate_only` 对应 TS 的 `{ validateOnly: true }`：
    /// 暂存并校验后不替换。
    pub fn hydrate_model_catalog(
        package_root: &Path,
        catalog_path: &Path,
        validate_only: bool,
    ) -> Result<(), CatalogError> {
        // TS：`JSON.parse(readFileSync(catalogPath, "utf8"))`，两层都没有 try——
        // 读失败抛 Node 的 `ENOENT: ...`，解析失败抛 V8 的 `SyntaxError` 文本。
        let text = read_file_utf8(catalog_path)
            .map_err(|error| CatalogError::new(node_read_error(catalog_path, &error)))?;
        let catalog =
            kiana_json::js_parse::parse(&text).map_err(|error| CatalogError::new(error.message))?;
        if !catalog.is_object() {
            return Err(CatalogError::new("Model catalog must be an object"));
        }

        let mut files: IndexMap<String, String> = IndexMap::new();
        let mut structure: ModelDataStructure = IndexMap::new();
        for provider in read_model_data_provider_ids(package_root)? {
            let models = catalog.get(&provider);
            let Some(models) = models else {
                return Err(CatalogError::new(format!(
                    "Model catalog is missing provider: {provider}"
                )));
            };
            let items = match models.as_array() {
                Some(items) if !items.is_empty() => items,
                _ => {
                    return Err(CatalogError::new(format!(
                        "Model catalog has no typed model list for provider: {provider}"
                    )));
                }
            };
            let mut entries: Vec<Value> = Vec::with_capacity(items.len());
            for model in items {
                let valid = model.is_object()
                    && model.get("type").and_then(Value::as_str).is_some()
                    && model.get("id").and_then(Value::as_str).is_some()
                    && model
                        .get("api")
                        .and_then(Value::as_str)
                        .map(|api| !api.is_empty())
                        .unwrap_or(false);
                if !valid {
                    return Err(CatalogError::new(format!(
                        "Model catalog has an invalid entry for provider {provider}"
                    )));
                }
                entries.push(model.clone());
            }
            let grouped = group_provider_model_data(&provider, &entries)?;
            structure.insert(provider.clone(), grouped.structure);
            files.insert(
                format!("{provider}.json"),
                serialize_groups(&grouped.groups),
            );
        }

        // 公开目录没有生成时间；固定时间戳让相同输入产生相同字节。
        // 先写 staging 并跑完整目录校验，全部通过后才换入 providers/data：
        // 旧目录先 rename 到同级的备份目录，staging rename 成功后再删备份；换入失败时
        // 把备份放回，避免同时丢掉新旧两份目录。validate_only 只做前半段，不触碰
        // providers/data。
        let manifest = create_model_data_manifest(&structure, &files, HYDRATION_GENERATED_AT);
        let providers_dir = package_root.join("src").join("providers");
        let staging_root = create_staging_dir(&providers_dir, ".model-hydration-")?;
        let staged_data = staging_root.join("data");
        let result = (|| -> Result<(), CatalogError> {
            std::fs::create_dir(&staged_data).map_err(|error| {
                CatalogError::new(node_fs_error(&error, "mkdir", &staged_data, None))
            })?;
            for (name, content) in &files {
                let path = staged_data.join(name);
                std::fs::write(&path, content).map_err(|error| {
                    CatalogError::new(node_fs_error(&error, "open", &path, None))
                })?;
            }
            let manifest_path = staged_data.join(MODEL_DATA_MANIFEST_FILE);
            std::fs::write(
                &manifest_path,
                format!("{}\n", stringify_json(&manifest.to_json())),
            )
            .map_err(|error| {
                CatalogError::new(node_fs_error(&error, "open", &manifest_path, None))
            })?;
            validate_model_data_directory(&structure, &staged_data)?;
            if validate_only {
                return Ok(());
            }
            let data_dir = providers_dir.join("data");
            // 旧目录先移到同级备份目录（同一个文件系统内的 rename 是原子的），
            // 避免“先删旧、再换新”在换入失败时两份都丢。
            let backup_root = if data_dir.exists() {
                Some(create_staging_dir(&providers_dir, ".model-data-backup-")?)
            } else {
                None
            };
            let backup_data = backup_root.as_ref().map(|root| root.join("data"));
            if let Some(backup_data) = &backup_data
                && let Err(error) = std::fs::rename(&data_dir, backup_data)
            {
                if let Some(root) = &backup_root {
                    let _ = std::fs::remove_dir_all(root);
                }
                return Err(CatalogError::new(node_fs_error(
                    &error,
                    "rename",
                    &data_dir,
                    Some(backup_data),
                )));
            }
            match std::fs::rename(&staged_data, &data_dir) {
                Ok(()) => {
                    if let Some(root) = &backup_root {
                        let _ = std::fs::remove_dir_all(root);
                    }
                    Ok(())
                }
                Err(error) => {
                    let primary = CatalogError::new(node_fs_error(
                        &error,
                        "rename",
                        &staged_data,
                        Some(&data_dir),
                    ));
                    match (&backup_root, &backup_data) {
                        (Some(root), Some(backup_data)) => {
                            match std::fs::rename(backup_data, &data_dir) {
                                Ok(()) => {
                                    let _ = std::fs::remove_dir_all(root);
                                    Err(primary)
                                }
                                Err(restore_error) => Err(CatalogError::new(format!(
                                    "{}; failed to restore previous model data from {}: {}",
                                    primary.message,
                                    backup_data.display(),
                                    node_fs_error(
                                        &restore_error,
                                        "rename",
                                        backup_data,
                                        Some(&data_dir)
                                    )
                                ))),
                            }
                        }
                        _ => Err(primary),
                    }
                }
            }
        })();
        let _ = std::fs::remove_dir_all(&staging_root);
        result
    }

    /// `mkdtempSync(join(providersDir, prefix))`：创建 `<providersDir>/<prefix>XXXXXX`。
    ///
    /// `hydrate` 用 `.model-hydration-`，`generator` 用 `.model-generation-`。
    pub fn create_staging_dir(
        providers_dir: &Path,
        prefix: &str,
    ) -> Result<std::path::PathBuf, CatalogError> {
        const ALPHABET: &[u8] = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
        for _ in 0..64 {
            let mut suffix = String::with_capacity(6);
            for _ in 0..6 {
                let mut byte = [0u8; 1];
                getrandom::fill(&mut byte)
                    .map_err(|error| CatalogError::new(format!("random: {error}")))?;
                suffix.push(ALPHABET[byte[0] as usize % ALPHABET.len()] as char);
            }
            let candidate = providers_dir.join(format!("{prefix}{suffix}"));
            match std::fs::create_dir(&candidate) {
                Ok(()) => return Ok(candidate),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => {
                    let template = providers_dir.join(format!("{prefix}XXXXXX"));
                    return Err(CatalogError::new(node_fs_error(
                        &error, "mkdtemp", &template, None,
                    )));
                }
            }
        }
        Err(CatalogError::new(format!(
            "{}: could not create a staging directory",
            providers_dir.display()
        )))
    }
}

// ---------------------------------------------------------------------------
// check-model-data.ts：`check:model-data` 门禁
// ---------------------------------------------------------------------------

/// `check-model-data.ts` 的两行输出。
pub mod check {
    use std::path::Path;

    use super::CatalogError;
    use super::model_data::validate_generated_model_data;

    /// 成功时的 stdout 行。
    pub const SUCCESS_LINE: &str = "Generated model data is valid.";
    /// 失败时的第二次 `console.error` 参数及其行尾换行：文本本身以 `\n` 开头（打印出空行），
    /// `console.error` 再追加一个 `\n`。完整 stderr = `error.message + "\n" + FAILURE_HINT`。
    pub const FAILURE_HINT: &str = "\nModel data is missing or stale. Run `npm run hydrate:model-data` from the repository root.\n";

    /// `check-model-data.ts`：成功返回 stdout 文本；失败返回 `(stdout, stderr)`，其中 stdout
    /// 为空、stderr 为 `error.message + "\n" + FAILURE_HINT`（与两次 `console.*` 的输出一致）。
    pub fn check_model_data(package_root: &Path) -> Result<String, (String, String)> {
        match validate_generated_model_data(package_root) {
            Ok(()) => Ok(SUCCESS_LINE.to_string()),
            Err(error) => Err((String::new(), format!("{error}\n{FAILURE_HINT}"))),
        }
    }

    /// 失败时的完整 stderr 文本（`error.message` + [`FAILURE_HINT`]）。
    pub fn failure_output(error: &CatalogError) -> String {
        format!("{error}\n{FAILURE_HINT}")
    }
}

// ---------------------------------------------------------------------------
// generate-models.ts：目录生成管线
// ---------------------------------------------------------------------------

/// `generate-models.ts`：从上游目录数据构造 `providers/<id>.json`、`models.generated.ts`
/// 与 JSON 目录输出。
///
/// TS 的输入端是网络（`fetch`）；Rust 侧把已取回的 JSON 作为
/// [`GeneratorInputs`](crate::model_catalog::generator::GeneratorInputs) 传入，
/// 其余（选项解析、各 provider 的模型构造、元数据应用器、分组、序列化、校验、统计输出）
/// 与既有输出逐字节一致（数据文件是逐字节产物）。调用方传入已取回的 JSON，测试因此不需要
/// Node 也不需要网络。
pub mod generator {
    use std::collections::BTreeSet;
    use std::path::{Path, PathBuf};

    use indexmap::IndexMap;
    use serde_json::{Map, Value, json};

    use super::gateway_pricing::{get_ai_gateway_cost, js_to_number};
    use super::model_data::{
        MODEL_DATA_MANIFEST_FILE, ModelDataStructure, assert_exact_model_ids,
        create_model_data_manifest, group_provider_model_data, node_fs_error as node_fs_error_text,
        node_read_error, read_file_utf8, read_model_data_provider_ids, serialize_groups_pretty,
        validate_generated_model_data, validate_model_data_directory,
    };
    use super::openrouter_catalog::{OpenRouterCatalog, build_open_router_catalog};
    use super::reasoning_options::{ModelsDevReasoningOption, get_effort_thinking_level_map};
    use super::{CatalogError, js_date_to_iso_string, locale_compare, round_cost, stringify_json};

    /// Node 风格文件系统错误（见 [`super::model_data::node_fs_error`]）。
    fn node_fs_error(
        error: &std::io::Error,
        syscall: &str,
        path: &Path,
        dest: Option<&Path>,
    ) -> CatalogError {
        CatalogError::new(node_fs_error_text(error, syscall, path, dest))
    }

    // ------------------------------------------------------------------
    // CLI 选项
    // ------------------------------------------------------------------

    /// `readGeneratorOptions` 的返回值。
    #[derive(Debug, Clone, Default, PartialEq, Eq)]
    pub struct GeneratorOptions {
        /// 取回/加载失败时直接返回错误；关闭时写入 [`GeneratorState::stderr_lines`] 并降级继续
        /// （参数错误、写盘错误与校验失败始终返回）。
        pub strict: bool,
        /// 只生成 `providers/data/*.json` 与 manifest，不重写 `*.models.ts` 分片和聚合器。
        pub data_only: bool,
        /// 完全不写 `src/` 下的产物，只输出 `--json-output` 目录。
        pub json_only: bool,
        /// `resolve(--json-output)` 后的绝对路径。
        pub json_output_dir: Option<PathBuf>,
        /// `JSON.stringify(value, null, 2)`：影响数据分片与 manifest 的排版，不影响 TS 模板。
        pub pretty: bool,
    }

    /// `readGeneratorOptions(args)`：`process.argv.slice(2)`。
    ///
    /// 支持 `--strict`、`--data-only`、`--json-only`、`--pretty`、`--json-output <dir>`；
    /// 未知参数、缺值、`--json-only` 缺输出目录、`--data-only` 与 JSON 输出组合都返回与 TS
    /// 相同的错误文本。
    pub fn read_generator_options(args: &[String]) -> Result<GeneratorOptions, CatalogError> {
        let mut options = GeneratorOptions::default();
        let mut index = 0usize;
        while index < args.len() {
            let arg = args[index].as_str();
            match arg {
                "--strict" => options.strict = true,
                "--data-only" => options.data_only = true,
                "--json-only" => options.json_only = true,
                "--pretty" => options.pretty = true,
                "--json-output" => {
                    index += 1;
                    // TS `if (!value)`：缺值**或空串**都报错（空串会 resolve 成 CWD）。
                    let Some(value) = args.get(index) else {
                        return Err(CatalogError::new("--json-output requires a directory"));
                    };
                    if value.is_empty() {
                        return Err(CatalogError::new("--json-output requires a directory"));
                    }
                    options.json_output_dir = Some(resolve_path(value));
                }
                _ => return Err(CatalogError::new(format!("Unknown argument: {arg}"))),
            }
            index += 1;
        }
        if options.json_only && options.json_output_dir.is_none() {
            return Err(CatalogError::new("--json-only requires --json-output"));
        }
        if options.data_only && (options.json_only || options.json_output_dir.is_some()) {
            return Err(CatalogError::new(
                "--data-only cannot be combined with JSON catalog output",
            ));
        }
        Ok(options)
    }

    /// `path.resolve(value)`：相对路径按当前工作目录展开。
    fn resolve_path(value: &str) -> PathBuf {
        let path = PathBuf::from(value);
        if path.is_absolute() {
            return path;
        }
        match std::env::current_dir() {
            Ok(cwd) => normalize_path(&cwd.join(path)),
            Err(_) => path,
        }
    }

    fn normalize_path(path: &Path) -> PathBuf {
        let mut out = PathBuf::new();
        for component in path.components() {
            match component {
                std::path::Component::CurDir => {}
                std::path::Component::ParentDir => {
                    out.pop();
                }
                other => out.push(other.as_os_str()),
            }
        }
        out
    }

    // ------------------------------------------------------------------
    // 常量（逐字搬运自 generate-models.ts）
    // ------------------------------------------------------------------

    /// `COPILOT_STATIC_HEADERS`。
    pub const COPILOT_STATIC_HEADERS: [(&str, &str); 4] = [
        ("User-Agent", "GitHubCopilotChat/0.35.0"),
        ("Editor-Version", "vscode/1.107.0"),
        ("Editor-Plugin-Version", "copilot-chat/0.35.0"),
        ("Copilot-Integration-Id", "vscode-chat"),
    ];
    /// `TOGETHER_BASE_URL`。
    pub const TOGETHER_BASE_URL: &str = "https://api.together.ai/v1";
    /// `AI_GATEWAY_MODELS_URL`。
    pub const AI_GATEWAY_MODELS_URL: &str = "https://ai-gateway.vercel.sh/v1";
    /// `AI_GATEWAY_BASE_URL`。
    pub const AI_GATEWAY_BASE_URL: &str = "https://ai-gateway.vercel.sh";
    /// `AI_GATEWAY_TYPESAFE_BASE_URL`。
    pub const AI_GATEWAY_TYPESAFE_BASE_URL: &str = "https://ai-gateway.vercel.sh/typesafe/v1";
    /// `VERTEX_BASE_URL`：`{location}` 是区域占位符；上层把含占位符的 baseUrl 视为
    /// “未指定自定义地址”，请求时按区域重新拼。
    pub const VERTEX_BASE_URL: &str = "https://{location}-aiplatform.googleapis.com";
    /// `NVIDIA_BASE_URL`。
    pub const NVIDIA_BASE_URL: &str = "https://integrate.api.nvidia.com/v1";
    /// `CLOUDFLARE_WORKERS_AI_BASE_URL`（`src/api/cloudflare.ts`）。
    pub const CLOUDFLARE_WORKERS_AI_BASE_URL: &str =
        "https://api.cloudflare.com/client/v4/accounts/{CLOUDFLARE_ACCOUNT_ID}/ai/v1";
    /// `CLOUDFLARE_WORKERS_AI_REST_BASE_URL`。
    pub const CLOUDFLARE_WORKERS_AI_REST_BASE_URL: &str =
        "https://api.cloudflare.com/client/v4/accounts/{CLOUDFLARE_ACCOUNT_ID}/ai";
    /// `CLOUDFLARE_AI_GATEWAY_COMPAT_BASE_URL`。
    pub const CLOUDFLARE_AI_GATEWAY_COMPAT_BASE_URL: &str = "https://gateway.ai.cloudflare.com/v1/{CLOUDFLARE_ACCOUNT_ID}/{CLOUDFLARE_GATEWAY_ID}/compat";
    /// `CLOUDFLARE_AI_GATEWAY_OPENAI_BASE_URL`。
    pub const CLOUDFLARE_AI_GATEWAY_OPENAI_BASE_URL: &str = "https://gateway.ai.cloudflare.com/v1/{CLOUDFLARE_ACCOUNT_ID}/{CLOUDFLARE_GATEWAY_ID}/openai";
    /// `CLOUDFLARE_AI_GATEWAY_ANTHROPIC_BASE_URL`。
    pub const CLOUDFLARE_AI_GATEWAY_ANTHROPIC_BASE_URL: &str = "https://gateway.ai.cloudflare.com/v1/{CLOUDFLARE_ACCOUNT_ID}/{CLOUDFLARE_GATEWAY_ID}/anthropic";
    /// `DEFAULT_RADIUS_GATEWAY`。
    pub const DEFAULT_RADIUS_GATEWAY: &str = "https://radius.kiana.dev";
    /// `OPENAI_LONG_CONTEXT_INPUT_THRESHOLD`。
    pub const OPENAI_LONG_CONTEXT_INPUT_THRESHOLD: f64 = 272_000.0;
    /// `KIMI_K3_MAX_TOKENS`。
    pub const KIMI_K3_MAX_TOKENS: u64 = 131_072;
    /// `KIMI_K3_COST`：`[input, output, cacheRead, cacheWrite]`，单位 $/1M tokens。
    pub const KIMI_K3_COST: [f64; 4] = [3.0, 15.0, 0.3, 0.0];
    /// `DEFAULT_IMAGE_RESIZE`（`maxBytes: 4.5 * 1024 * 1024`）。
    pub const DEFAULT_IMAGE_RESIZE_MAX_BYTES: f64 = 4.5 * 1024.0 * 1024.0;
    /// `ANTHROPIC_PROMPT_CACHE`：`(retention 档位, 缓存秒数)`（short = 300s、long = 3600s）。
    pub const ANTHROPIC_PROMPT_CACHE: [(&str, f64); 2] = [("short", 300.0), ("long", 3600.0)];
    /// `AZURE_DEEPSEEK_V4_PRO_COST`：`[input, output, cacheRead, cacheWrite]`，单位 $/1M tokens。
    pub const AZURE_DEEPSEEK_V4_PRO_COST: [f64; 4] = [1.925, 3.828, 0.165, 0.0];
    /// `AZURE_CONTEXT_WINDOW_OVERRIDES`。
    pub const AZURE_CONTEXT_WINDOW_OVERRIDES: [(&str, f64); 5] = [
        ("gpt-5.4", 1_050_000.0),
        ("gpt-5.5", 1_050_000.0),
        ("gpt-5.6-luna", 1_050_000.0),
        ("gpt-5.6-sol", 1_050_000.0),
        ("gpt-5.6-terra", 1_050_000.0),
    ];

    // ------------------------------------------------------------------
    // JS 取值助手
    // ------------------------------------------------------------------

    /// `value || 0`（0/NaN/缺失都回落到 0；其余（含负数）原样）。
    pub fn js_number_or_zero(value: Option<&Value>) -> f64 {
        match value.and_then(Value::as_f64) {
            Some(number) if number != 0.0 && !number.is_nan() => number,
            _ => 0.0,
        }
    }

    /// JS 真值判定（`||` 与三元条件的左侧）。
    fn js_truthy(value: &Value) -> bool {
        match value {
            Value::Null => false,
            Value::Bool(flag) => *flag,
            Value::Number(number) => number
                .as_f64()
                .map(|number| number != 0.0 && !number.is_nan())
                .unwrap_or(true),
            Value::String(text) => !text.is_empty(),
            Value::Array(_) | Value::Object(_) => true,
        }
    }

    /// `value || fallback`（Value 版本；保留上游的非 number 值）。
    fn js_or_value(value: Option<&Value>, fallback: Value) -> Value {
        match value {
            Some(value) if js_truthy(value) => value.clone(),
            _ => fallback,
        }
    }

    /// `value ?? fallback`（Value 版本；只回落 `undefined`/`null`）。
    fn js_nullish_value(value: Option<&Value>, fallback: Value) -> Value {
        match value {
            None | Some(Value::Null) => fallback,
            Some(value) => value.clone(),
        }
    }

    fn nested<'a>(value: &'a Value, keys: &[&str]) -> Option<&'a Value> {
        let mut current = value;
        for key in keys {
            current = current.get(*key)?;
        }
        Some(current)
    }

    fn str_at<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
        value.get(key).and_then(Value::as_str)
    }

    /// `m.name || modelId`。
    fn model_name(model: &Value, model_id: &str) -> String {
        match str_at(model, "name") {
            Some(name) if !name.is_empty() => name.to_string(),
            _ => model_id.to_string(),
        }
    }

    /// `model.modalities?.input?.includes("image")`。
    fn input_has_image(model: &Value) -> bool {
        nested(model, &["modalities", "input"])
            .and_then(Value::as_array)
            .map(|items| items.iter().any(|item| item.as_str() == Some("image")))
            .unwrap_or(false)
    }

    /// `model.modalities?.input?.includes(value)`。
    fn input_includes(model: &Value, value: &str) -> bool {
        nested(model, &["modalities", "input"])
            .and_then(Value::as_array)
            .map(|items| items.iter().any(|item| item.as_str() == Some(value)))
            .unwrap_or(false)
    }

    /// `model.modalities?.output?.includes(value)`。
    fn output_includes(model: &Value, value: &str) -> bool {
        nested(model, &["modalities", "output"])
            .and_then(Value::as_array)
            .map(|items| items.iter().any(|item| item.as_str() == Some(value)))
            .unwrap_or(false)
    }

    fn input_modalities(has_image: bool) -> Value {
        if has_image {
            json!(["text", "image"])
        } else {
            json!(["text"])
        }
    }

    /// 成本对象（定义顺序 input/output/cacheRead/cacheWrite[+tiers]）。
    fn cost_object(rates: [f64; 4], tiers: Option<Vec<Value>>) -> Value {
        cost_object_values(
            [
                json!(rates[0]),
                json!(rates[1]),
                json!(rates[2]),
                json!(rates[3]),
            ],
            tiers,
        )
    }

    /// 成本对象（Value 版：费率保留上游的原始 JSON 类型，由校验器负责拒绝非 number）。
    fn cost_object_values(rates: [Value; 4], tiers: Option<Vec<Value>>) -> Value {
        let mut object = Map::new();
        object.insert("input".to_string(), rates[0].clone());
        object.insert("output".to_string(), rates[1].clone());
        object.insert("cacheRead".to_string(), rates[2].clone());
        object.insert("cacheWrite".to_string(), rates[3].clone());
        if let Some(tiers) = tiers
            && !tiers.is_empty()
        {
            object.insert("tiers".to_string(), Value::Array(tiers));
        }
        Value::Object(object)
    }

    /// `roundCost(value)`。
    pub fn round_cost_value(value: f64) -> f64 {
        round_cost(value)
    }

    /// `withOpenAiLongContextPricing(cost)`：追加 272k 以上的长上下文档位。
    ///
    /// 档位费率固定为 input/cacheRead/cacheWrite ×2、output ×1.5，各自 `roundCost` 到 6 位。
    pub fn with_open_ai_long_context_pricing(cost: &Value) -> Value {
        let base = [
            js_number_or_zero(cost.get("input")),
            js_number_or_zero(cost.get("output")),
            js_number_or_zero(cost.get("cacheRead")),
            js_number_or_zero(cost.get("cacheWrite")),
        ];
        let mut tier = Map::new();
        tier.insert(
            "inputTokensAbove".to_string(),
            json!(OPENAI_LONG_CONTEXT_INPUT_THRESHOLD),
        );
        tier.insert("input".to_string(), json!(round_cost(base[0] * 2.0)));
        tier.insert("output".to_string(), json!(round_cost(base[1] * 1.5)));
        tier.insert("cacheRead".to_string(), json!(round_cost(base[2] * 2.0)));
        tier.insert("cacheWrite".to_string(), json!(round_cost(base[3] * 2.0)));
        cost_object(base, Some(vec![Value::Object(tier)]))
    }

    /// `getModelsDevCost(cost)`：base 四费率 + `tier.type === "context"` 的档位。
    ///
    /// TS 用 `cost?.input || 0` / `tier.input ?? base.input`，**不做**类型归一化：
    /// 上游给字符串等非 number 值时原样进入生成结果，由 `validateModelDataDirectory`
    /// 报 `has invalid cost.input`。
    pub fn get_models_dev_cost(cost: Option<&Value>) -> Value {
        let base = [
            js_or_value(cost.and_then(|cost| cost.get("input")), json!(0)),
            js_or_value(cost.and_then(|cost| cost.get("output")), json!(0)),
            js_or_value(cost.and_then(|cost| cost.get("cache_read")), json!(0)),
            js_or_value(cost.and_then(|cost| cost.get("cache_write")), json!(0)),
        ];
        let mut tiers: Vec<Value> = Vec::new();
        if let Some(items) = cost
            .and_then(|cost| cost.get("tiers"))
            .and_then(Value::as_array)
        {
            for tier in items {
                let Some(context) = tier.get("tier") else {
                    continue;
                };
                if str_at(context, "type") != Some("context") {
                    continue;
                }
                // `context.size === undefined`：只有缺失才跳过，`null` 是合法（生成 null）。
                let Some(size) = context.get("size") else {
                    continue;
                };
                let mut entry = Map::new();
                entry.insert("inputTokensAbove".to_string(), size.clone());
                entry.insert(
                    "input".to_string(),
                    js_nullish_value(tier.get("input"), base[0].clone()),
                );
                entry.insert(
                    "output".to_string(),
                    js_nullish_value(tier.get("output"), base[1].clone()),
                );
                entry.insert(
                    "cacheRead".to_string(),
                    js_nullish_value(tier.get("cache_read"), base[2].clone()),
                );
                entry.insert(
                    "cacheWrite".to_string(),
                    js_nullish_value(tier.get("cache_write"), base[3].clone()),
                );
                tiers.push(Value::Object(entry));
            }
        }
        cost_object_values(base, Some(tiers))
    }

    /// `model.name`/`limit` 缺省时用的 `|| 4096`（保留上游的非 number 值）。
    fn limit_or(model: &Value, key: &str) -> Value {
        limit_or_default(model, key, 4096.0)
    }

    /// `m.limit?.[key] || fallback`。
    fn limit_or_default(model: &Value, key: &str, fallback: f64) -> Value {
        js_or_value(nested(model, &["limit", key]), json!(fallback))
    }

    // ------------------------------------------------------------------
    // 生成器输入
    // ------------------------------------------------------------------

    /// 从上游取回的全部目录数据（TS 里是 `fetch` 的结果）。
    ///
    /// 每个字段对应 TS 的一次请求；缺失（`None`/空）等价于该请求失败且非 `strict`。
    #[derive(Debug, Clone, Default)]
    pub struct GeneratorInputs {
        /// `packages/ai` 的绝对路径（`--data-only` 要读 `src/models.generated.ts`）。
        pub package_root: std::path::PathBuf,
        /// `https://models.dev/api.json` 的响应体。
        pub models_dev: Option<Value>,
        /// `https://models.dev/models.json?type=decision` 的响应体。
        pub models_dev_decision: Option<Value>,
        /// `https://openrouter.ai/api/v1/models`（默认列表）。
        pub open_router_listed: Vec<Value>,
        /// `?output_modalities=image`。
        pub open_router_image_listed: Vec<Value>,
        /// `?output_modalities=decisions`。
        pub open_router_decision_listed: Vec<Value>,
        /// `https://ai-gateway.vercel.sh/v1/models` 的响应体。
        pub ai_gateway: Option<Value>,
        /// `https://integrate.api.nvidia.com/v1/models` 的响应体（仅 id 归一化用）。
        pub nvidia_nim: Option<Value>,
        /// `https://radius.kiana.dev/v1/config` 的响应体。
        pub radius_config: Option<Value>,
        /// 各数据源的取回错误（`<source>` -> `error.message`），对应 TS `fetch` 抛出的异常。
        ///
        /// 值为 `None` 的字段与这里没有条目的组合表示“取回失败且消息未知”，默认消息是
        /// JS 全局 `fetch` 的网络错误 `TypeError: fetch failed`。
        pub fetch_errors: IndexMap<String, String>,
    }

    /// [`GeneratorInputs::fetch_errors`] 的键：models.dev 目录。
    pub const SOURCE_MODELS_DEV: &str = "models_dev";
    /// models.dev classifier 目录。
    pub const SOURCE_MODELS_DEV_CLASSIFIER: &str = "models_dev_classifier";
    /// OpenRouter（三个列表任一次失败都记在这里）。
    pub const SOURCE_OPEN_ROUTER: &str = "open_router";
    /// Vercel AI Gateway。
    pub const SOURCE_AI_GATEWAY: &str = "ai_gateway";
    /// NVIDIA NIM（仅在 models.dev 声明了 `nvidia.models` 时取回）。
    pub const SOURCE_NVIDIA_NIM: &str = "nvidia_nim";
    /// Radius。
    pub const SOURCE_RADIUS: &str = "radius";

    impl GeneratorInputs {
        /// 某个数据源失败时的 `error.message`（未记录时为 `TypeError: fetch failed`）。
        pub fn fetch_error(&self, source: &str) -> String {
            self.fetch_errors
                .get(source)
                .cloned()
                .unwrap_or_else(|| "TypeError: fetch failed".to_string())
        }
    }

    /// 生成过程中跨 provider 共享的状态（TS 的模块级 `modelsDevReasoningOptions`）。
    #[derive(Debug, Default)]
    pub struct GeneratorState {
        /// `modelsDevReasoningOptions`：`provider:id` -> options。
        pub models_dev_reasoning_options: IndexMap<String, Vec<ModelsDevReasoningOption>>,
        /// `loadModelsDevClassifierModels()` 的结果（在 `build_all_models` 中按 TS 顺序取回）。
        pub models_dev_classifier_models: Vec<Value>,
        /// TS 的 `console.log` 进度行（按发生顺序）。
        pub stdout_lines: Vec<String>,
        /// TS 的 `console.error` 行（非 strict 时降级的失败信息）。
        pub stderr_lines: Vec<String>,
    }

    impl GeneratorState {
        /// `recordModelsDevReasoningOptions(provider, id, sourceModel)`。
        fn record(&mut self, provider: &str, id: &str, source_model: &Value) {
            if let Some(options) = source_model
                .get("reasoning_options")
                .filter(|value| !value.is_null())
            {
                let parsed: Vec<ModelsDevReasoningOption> = options
                    .as_array()
                    .map(|items| {
                        items
                            .iter()
                            .filter_map(ModelsDevReasoningOption::from_json)
                            .collect()
                    })
                    .unwrap_or_default();
                self.models_dev_reasoning_options
                    .insert(format!("{provider}:{id}"), parsed);
            }
        }

        /// `modelsDevReasoningOptions.get(getModelKey(model))`。
        fn options_for(&self, model: &Value) -> Option<&Vec<ModelsDevReasoningOption>> {
            let key = format!(
                "{}:{}",
                str_at(model, "provider").unwrap_or_default(),
                str_at(model, "id").unwrap_or_default()
            );
            self.models_dev_reasoning_options.get(&key)
        }
    }

    /// `reasoning_options.some((option) => option.type === "effort")`。
    fn has_effort_option(model: &Value) -> bool {
        model
            .get("reasoning_options")
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .any(|item| str_at(item, "type") == Some("effort"))
            })
            .unwrap_or(false)
    }

    /// `reasoning_options.some((option) => option.type === "toggle")`。
    fn has_toggle_option(model: &Value) -> bool {
        model
            .get("reasoning_options")
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .any(|item| str_at(item, "type") == Some("toggle"))
            })
            .unwrap_or(false)
    }

    /// `model.reasoning_options ?? []` 的解析结果。
    fn reasoning_options(model: &Value) -> Vec<ModelsDevReasoningOption> {
        model
            .get("reasoning_options")
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(ModelsDevReasoningOption::from_json)
                    .collect()
            })
            .unwrap_or_default()
    }

    fn is_deprecated(model: &Value) -> bool {
        str_at(model, "status") == Some("deprecated")
    }

    fn tool_capable(model: &Value) -> bool {
        model.get("tool_call").and_then(Value::as_bool) == Some(true)
    }

    /// `Object.entries(data[provider]?.models ?? {})`。
    fn provider_models<'a>(catalog: &'a Value, provider: &str) -> Vec<(String, &'a Value)> {
        catalog
            .get(provider)
            .and_then(|provider| provider.get("models"))
            .and_then(Value::as_object)
            .map(|models| {
                models
                    .iter()
                    .map(|(id, model)| (id.clone(), model))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// `getBedrockBaseUrl(modelId)`。
    pub fn get_bedrock_base_url(model_id: &str) -> &'static str {
        if model_id.starts_with("eu.") {
            "https://bedrock-runtime.eu-central-1.amazonaws.com"
        } else {
            "https://bedrock-runtime.us-east-1.amazonaws.com"
        }
    }

    /// `normalizeNvidiaModelId(modelId)`：小写 + `_` -> `.`。
    pub fn normalize_nvidia_model_id(model_id: &str) -> String {
        model_id.to_lowercase().replace('_', ".")
    }

    /// `isKimiK3FamilyId(modelId)`：`/^kimi-k3(?:\.\d+)?(?:-|$)/`。
    pub fn is_kimi_k3_family_id(model_id: &str) -> bool {
        let Some(rest) = model_id.strip_prefix("kimi-k3") else {
            return false;
        };
        if rest.is_empty() || rest.starts_with('-') {
            return true;
        }
        let Some(rest) = rest.strip_prefix('.') else {
            return false;
        };
        let digits_end = rest
            .find(|c: char| !c.is_ascii_digit())
            .unwrap_or(rest.len());
        if digits_end == 0 {
            return false;
        }
        let tail = &rest[digits_end..];
        tail.is_empty() || tail.starts_with('-')
    }

    /// `isGemma4Model(modelId)`：`/gemma-?4/`（小写后）。
    fn is_gemma4_model(model_id: &str) -> bool {
        let lower = model_id.to_lowercase();
        if lower.contains("gemma-4") {
            return true;
        }
        let bytes = lower.as_bytes();
        let mut index = 0;
        while let Some(found) = lower[index..].find("gemma") {
            let position = index + found;
            let after = position + "gemma".len();
            let rest = &lower[after..];
            if rest.starts_with('4') || rest.starts_with("-4") {
                return true;
            }
            index = after;
            let _ = bytes;
        }
        false
    }

    /// `getGoogleThinkingLevelMap(modelId, reasoningOptions)`。
    fn get_google_thinking_level_map(
        model_id: &str,
        options: &[ModelsDevReasoningOption],
    ) -> Option<super::ThinkingLevelMap> {
        if let Some(map) = get_effort_thinking_level_map(options) {
            return Some(map);
        }
        if is_gemma4_model(model_id) {
            return Some(level_map(&[
                ("off", None),
                ("minimal", Some("MINIMAL")),
                ("low", None),
                ("medium", None),
                ("high", Some("HIGH")),
            ]));
        }
        None
    }

    // ------------------------------------------------------------------
    // thinkingLevelMap 合并
    // ------------------------------------------------------------------

    /// 构造 `ThinkingLevelMap`（保持给定键序）。
    pub fn level_map(pairs: &[(&str, Option<&str>)]) -> super::ThinkingLevelMap {
        let mut map: super::ThinkingLevelMap = IndexMap::new();
        for (level, value) in pairs {
            let Some(level) = super::reasoning_options::model_level_from_name(level) else {
                continue;
            };
            map.insert(level, value.map(str::to_string));
        }
        map
    }

    /// `mergeThinkingLevelMap(model, map)`：`{...model.thinkingLevelMap, ...map}`。
    fn merge_thinking_level_map(model: &mut Value, map: &super::ThinkingLevelMap) {
        let mut merged: super::ThinkingLevelMap =
            current_thinking_level_map(model).unwrap_or_default();
        for (level, value) in map {
            merged.insert(*level, value.clone());
        }
        set_thinking_level_map(model, &merged);
    }

    /// 读取 `model.thinkingLevelMap`（缺失/`null` 为 `None`）。
    pub fn current_thinking_level_map(model: &Value) -> Option<super::ThinkingLevelMap> {
        let object = model.get("thinkingLevelMap")?.as_object()?;
        let mut map: super::ThinkingLevelMap = IndexMap::new();
        for (key, value) in object {
            let Some(level) = super::reasoning_options::model_level_from_name(key) else {
                continue;
            };
            map.insert(
                level,
                match value {
                    Value::Null => None,
                    Value::String(text) => Some(text.clone()),
                    _ => None,
                },
            );
        }
        Some(map)
    }

    /// 写回 `model.thinkingLevelMap`（键序即插入序）。
    pub fn set_thinking_level_map(model: &mut Value, map: &super::ThinkingLevelMap) {
        let Some(object) = model.as_object_mut() else {
            return;
        };
        object.insert(
            "thinkingLevelMap".to_string(),
            super::thinking_level_map_to_json(map),
        );
    }

    fn has_thinking_level_map(model: &Value) -> bool {
        model
            .get("thinkingLevelMap")
            .map(|value| !value.is_null())
            .unwrap_or(false)
    }

    // ------------------------------------------------------------------
    // openai-completions compat
    // ------------------------------------------------------------------

    /// `OPENAI_COMPLETIONS_DEFAULT_COMPAT` 的键序与默认值。
    fn openai_completions_default(key: &str) -> Option<Value> {
        Some(match key {
            "supportsStore"
            | "supportsDeveloperRole"
            | "supportsReasoningEffort"
            | "supportsUsageInStreaming"
            | "supportsFinishReason"
            | "supportsLongCacheRetention" => Value::Bool(true),
            "requiresToolResultName"
            | "requiresAssistantAfterToolResult"
            | "requiresThinkingAsText"
            | "requiresReasoningContentOnAssistantMessages"
            | "zaiToolStream"
            | "supportsStrictMode"
            | "supportsOpenAIGrammarTools"
            | "supportsMidConvoSystemMessages"
            | "supportsMidConvoToolAdditions"
            | "sendSessionAffinityHeaders" => Value::Bool(false),
            "maxTokensField" => Value::String("max_completion_tokens".to_string()),
            "thinkingFormat" => Value::String("openai".to_string()),
            "openRouterRouting"
            | "vercelGatewayRouting"
            | "chatTemplateKwargs"
            | "chatTemplateArgs" => Value::Object(Map::new()),
            _ => return None,
        })
    }

    fn is_plain_empty_object(value: &Value) -> bool {
        value
            .as_object()
            .map(|object| object.is_empty())
            .unwrap_or(false)
    }

    /// `detectOpenAICompletionsCompat(model)`：返回按 TS 字面量键序构造的 compat。
    pub fn detect_openai_completions_compat(model: &Value) -> Map<String, Value> {
        let provider = str_at(model, "provider").unwrap_or_default();
        let base_url = str_at(model, "baseUrl").unwrap_or_default();
        let id = str_at(model, "id").unwrap_or_default();
        let base_url_lower = base_url.to_lowercase();

        // 先按 provider 名或 baseUrl 识别厂商；后面的布尔分支都由这些标志组合而成。
        let is_zai = provider == "zai"
            || provider == "zai-coding-cn"
            || base_url.contains("api.z.ai")
            || base_url.contains("open.bigmodel.cn");
        let is_together = provider == "together"
            || base_url.contains("api.together.ai")
            || base_url.contains("api.together.xyz");
        let is_moonshot = provider == "moonshotai"
            || provider == "moonshotai-cn"
            || base_url.contains("api.moonshot.");
        let is_open_router = provider == "openrouter" || base_url.contains("openrouter.ai");
        let is_cloudflare_workers_ai =
            provider == "cloudflare-workers-ai" || base_url.contains("api.cloudflare.com");
        let is_cloudflare_ai_gateway =
            provider == "cloudflare-ai-gateway" || base_url.contains("gateway.ai.cloudflare.com");
        let is_nvidia = provider == "nvidia" || base_url.contains("integrate.api.nvidia.com");
        let is_ant_ling = provider == "ant-ling" || base_url.contains("api.ant-ling.com");
        let is_cerebras = provider == "cerebras" || base_url.contains("cerebras.ai");
        let is_deep_seek = provider == "deepseek" || base_url_lower.contains("deepseek.com");

        // 这些网关缺少 OpenAI 的扩展能力。注意 `is_non_standard` 并不是所有键的总开关：
        // 它只控制 supportsStore（取反）与 supportsDeveloperRole；strict、reasoningEffort、
        // maxTokensField、thinkingFormat 等键由各自的独立条件判断。
        let is_non_standard = is_nvidia
            || is_cerebras
            || provider == "xai"
            || base_url.contains("api.x.ai")
            || is_together
            || base_url.contains("chutes.ai")
            || is_deep_seek
            || is_zai
            || is_moonshot
            || provider == "opencode"
            || base_url.contains("opencode.ai")
            || is_cloudflare_workers_ai
            || is_cloudflare_ai_gateway
            || is_ant_ling;

        // 这些网关要求请求体使用 `max_tokens`，而不是 OpenAI 的 `max_completion_tokens`。
        let use_max_tokens = base_url.contains("chutes.ai")
            || is_deep_seek
            || is_moonshot
            || is_cloudflare_ai_gateway
            || is_together
            || is_nvidia
            || is_ant_ling
            || is_zai;

        let is_grok = provider == "xai" || base_url.contains("api.x.ai");
        let is_open_router_developer_role_model =
            is_open_router && (id.starts_with("anthropic/") || id.starts_with("openai/"));
        let cache_control_format = if provider == "openrouter" && is_open_router_anthropic_id(id) {
            Some("anthropic")
        } else {
            None
        };

        // 键的插入顺序必须固定：生成的数据文件是逐字节产物，键序会被 JSON 序列化
        // 固化进 `<provider>.json`。
        let mut compat = Map::new();
        compat.insert("supportsStore".to_string(), Value::Bool(!is_non_standard));
        compat.insert(
            "supportsDeveloperRole".to_string(),
            Value::Bool(
                is_open_router_developer_role_model || (!is_non_standard && !is_open_router),
            ),
        );
        compat.insert(
            "supportsReasoningEffort".to_string(),
            Value::Bool(
                !is_grok
                    && !is_zai
                    && !is_moonshot
                    && !is_together
                    && !is_cloudflare_ai_gateway
                    && !is_nvidia
                    && !is_ant_ling,
            ),
        );
        compat.insert("supportsUsageInStreaming".to_string(), Value::Bool(true));
        compat.insert("supportsFinishReason".to_string(), Value::Bool(true));
        compat.insert(
            "maxTokensField".to_string(),
            Value::String(
                if use_max_tokens {
                    "max_tokens"
                } else {
                    "max_completion_tokens"
                }
                .to_string(),
            ),
        );
        compat.insert("requiresToolResultName".to_string(), Value::Bool(false));
        compat.insert(
            "requiresAssistantAfterToolResult".to_string(),
            Value::Bool(false),
        );
        compat.insert("requiresThinkingAsText".to_string(), Value::Bool(false));
        compat.insert(
            "requiresReasoningContentOnAssistantMessages".to_string(),
            Value::Bool(is_deep_seek),
        );
        compat.insert(
            "thinkingFormat".to_string(),
            Value::String(
                (if is_deep_seek {
                    "deepseek"
                } else if is_zai {
                    "zai"
                } else if is_together {
                    "together"
                } else if is_ant_ling {
                    "ant-ling"
                } else if is_open_router {
                    "openrouter"
                } else {
                    "openai"
                })
                .to_string(),
            ),
        );
        compat.insert("openRouterRouting".to_string(), Value::Object(Map::new()));
        compat.insert(
            "vercelGatewayRouting".to_string(),
            Value::Object(Map::new()),
        );
        compat.insert("chatTemplateKwargs".to_string(), Value::Object(Map::new()));
        compat.insert("chatTemplateArgs".to_string(), Value::Object(Map::new()));
        compat.insert("zaiToolStream".to_string(), Value::Bool(false));
        compat.insert(
            "supportsStrictMode".to_string(),
            Value::Bool(
                !is_moonshot
                    && !is_together
                    && !is_cloudflare_ai_gateway
                    && !is_nvidia
                    && !is_cerebras,
            ),
        );
        compat.insert("supportsOpenAIGrammarTools".to_string(), Value::Bool(false));
        compat.insert(
            "supportsMidConvoSystemMessages".to_string(),
            Value::Bool(false),
        );
        compat.insert(
            "supportsMidConvoToolAdditions".to_string(),
            Value::Bool(false),
        );
        if let Some(format) = cache_control_format {
            compat.insert(
                "cacheControlFormat".to_string(),
                Value::String(format.to_string()),
            );
        }
        compat.insert(
            "sendSessionAffinityHeaders".to_string(),
            Value::Bool(is_open_router),
        );
        compat.insert(
            "supportsLongCacheRetention".to_string(),
            Value::Bool(
                !(is_together
                    || is_cloudflare_workers_ai
                    || is_cloudflare_ai_gateway
                    || is_nvidia
                    || is_ant_ling),
            ),
        );
        compat
    }

    /// `/^~?anthropic\//.test(id)`。
    fn is_open_router_anthropic_id(id: &str) -> bool {
        id.strip_prefix('~').unwrap_or(id).starts_with("anthropic/")
    }

    /// `openAICompletionsCompatDelta(compat)`。
    pub fn open_ai_completions_compat_delta(compat: &Map<String, Value>) -> Map<String, Value> {
        let mut delta = Map::new();
        // 空对象的路由字段与默认空对象等价，直接跳过；其余只保留与默认值不同的键。
        for (key, value) in compat {
            let default_value = openai_completions_default(key);
            if is_plain_empty_object(value)
                && default_value
                    .as_ref()
                    .map(is_plain_empty_object)
                    .unwrap_or(false)
            {
                continue;
            }
            let same = match &default_value {
                Some(default_value) => default_value == value,
                None => false,
            };
            if !same {
                delta.insert(key.clone(), value.clone());
            }
        }
        delta
    }

    /// `{...base, ...over}`（覆盖已存在键原位替换、新键追加）。
    pub fn js_spread(base: &Map<String, Value>, over: &Map<String, Value>) -> Map<String, Value> {
        let mut result = base.clone();
        for (key, value) in over {
            result.insert(key.clone(), value.clone());
        }
        result
    }

    /// 读取 `model.compat`（缺失/`null`/非对象为空）。
    pub fn current_compat(model: &Value) -> Map<String, Value> {
        model
            .get("compat")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default()
    }

    /// 写回 `model.compat`；空对象等价 TS 的 `delete`。
    pub fn set_compat(model: &mut Value, compat: Map<String, Value>) {
        let Some(object) = model.as_object_mut() else {
            return;
        };
        if compat.is_empty() {
            object.remove("compat");
        } else {
            object.insert("compat".to_string(), Value::Object(compat));
        }
    }

    /// `mergeAnthropicMessagesCompat(model, compat)`。
    fn merge_anthropic_compat(model: &mut Value, compat: &Map<String, Value>) {
        let merged = js_spread(&current_compat(model), compat);
        set_compat(model, merged);
    }

    /// `mergeOpenAICompletionsCompat(model, compat)`。
    pub fn merge_openai_completions_compat(model: &mut Value, compat: &Map<String, Value>) {
        let merged = js_spread(&current_compat(model), compat);
        set_compat(model, merged);
    }

    /// `applyOpenAICompletionsCompatMetadata(model)`。
    fn apply_openai_completions_compat_metadata(model: &mut Value) {
        if str_at(model, "api") != Some("openai-completions") {
            return;
        }
        // 模型已有的 compat 覆盖检测结果（TS 的 `{...detected, ...current}`）：
        // 检测只补默认值，不推翻上游明确写死的字段。
        let detected = open_ai_completions_compat_delta(&detect_openai_completions_compat(model));
        let merged = js_spread(&detected, &current_compat(model));
        set_compat(model, merged);
    }

    // ------------------------------------------------------------------
    // Anthropic compat
    // ------------------------------------------------------------------

    const VERIFIED_ANTHROPIC_MID_CONVO_EFFORT_PROVIDERS: [&str; 2] = ["anthropic", "openrouter"];
    const MID_CONVO_EFFORT_UNSUPPORTED_ANTHROPIC_MODELS: [&str; 1] =
        ["openrouter:anthropic/claude-opus-5"];
    const ANTHROPIC_DISABLED_THINKING_MODELS: [&str; 3] =
        ["claude-opus-5", "claude-haiku-5-5", "claude-haiku-5.5"];
    const EAGER_TOOL_INPUT_STREAMING_UNSUPPORTED_ANTHROPIC_MODELS: [&str; 3] = [
        "github-copilot:claude-haiku-4.5",
        "github-copilot:claude-sonnet-4",
        "github-copilot:claude-sonnet-4.5",
    ];
    /// `ANTHROPIC_ALLOWED_FALLBACK_MODELS`（保持声明序）。
    const ANTHROPIC_ALLOWED_FALLBACK_MODELS: [(&str, [&str; 2]); 2] = [
        ("claude-fable-5", ["claude-opus-4-8", "claude-opus-5"]),
        ("claude-opus-5", ["claude-opus-4-8", ""]),
    ];

    fn contains_key(haystack: &[&str], needle: &str) -> bool {
        haystack.contains(&needle)
    }

    /// 去掉 `~anthropic/`/`anthropic/` 前缀并小写。
    fn strip_anthropic_prefix(model_id: &str) -> String {
        let lower = model_id.to_lowercase();
        let lower = lower.strip_prefix('~').unwrap_or(&lower);
        lower
            .strip_prefix("anthropic/")
            .unwrap_or(lower)
            .to_string()
    }

    /// `supportsAnthropicMidConvoEffort(modelId)`。
    pub fn supports_anthropic_mid_convo_effort(model_id: &str) -> bool {
        let id = strip_anthropic_prefix(model_id);
        let t = id.as_str();
        matches_claude_opus_5_or_55(t)
            || matches_claude_family_55(t, "sonnet")
            || matches_claude_family_55(t, "haiku")
            || matches_claude_fable_mythos_51(t)
    }

    fn matches_claude_opus_5_or_55(id: &str) -> bool {
        let Some(rest) = id.strip_prefix("claude-opus-") else {
            return false;
        };
        // `(?:5|5[.-]5)(?:-\d{8})?`
        let tail = if let Some(rest) = rest.strip_prefix("5.5") {
            rest
        } else if let Some(rest) = rest.strip_prefix("5-5") {
            rest
        } else if let Some(rest) = rest.strip_prefix('5') {
            rest
        } else {
            return false;
        };
        is_optional_dated_suffix(tail)
    }

    fn matches_claude_family_55(id: &str, family: &str) -> bool {
        let prefix = format!("claude-{family}-5");
        let Some(rest) = id.strip_prefix(&prefix) else {
            return false;
        };
        let tail = if let Some(rest) = rest.strip_prefix(".5") {
            rest
        } else if let Some(rest) = rest.strip_prefix("-5") {
            rest
        } else {
            return false;
        };
        is_optional_dated_suffix(tail)
    }

    fn matches_claude_fable_mythos_51(id: &str) -> bool {
        for family in ["fable", "mythos"] {
            let prefix = format!("claude-{family}-5");
            if let Some(rest) = id.strip_prefix(&prefix) {
                let tail = if let Some(rest) = rest.strip_prefix(".1") {
                    rest
                } else if let Some(rest) = rest.strip_prefix("-1") {
                    rest
                } else {
                    return false;
                };
                if is_optional_dated_suffix(tail) {
                    return true;
                }
            }
        }
        false
    }

    /// `-\d{8}` 之后必须结束。
    fn is_optional_dated_suffix(tail: &str) -> bool {
        if tail.is_empty() {
            return true;
        }
        let Some(digits) = tail.strip_prefix('-') else {
            return false;
        };
        digits.len() == 8 && digits.bytes().all(|byte| byte.is_ascii_digit())
    }

    /// `supportsAnthropicDisabledThinkingWithMidConvoEffort(modelId)`。
    pub fn supports_anthropic_disabled_thinking_with_mid_convo_effort(model_id: &str) -> bool {
        let stripped = strip_anthropic_prefix(model_id);
        let id = strip_dated_suffix(&stripped);
        contains_key(&ANTHROPIC_DISABLED_THINKING_MODELS, id)
    }

    fn strip_dated_suffix(value: &str) -> &str {
        let bytes = value.as_bytes();
        if bytes.len() < 9 {
            return value;
        }
        let split = bytes.len() - 9;
        if bytes[split] == b'-' && bytes[split + 1..].iter().all(|byte| byte.is_ascii_digit()) {
            &value[..split]
        } else {
            value
        }
    }

    /// `supportsAnthropicMidConvoSystemMessages(modelId)`。
    pub fn supports_anthropic_mid_convo_system_messages(model_id: &str) -> bool {
        matches_opus_48_or_5(model_id)
            || matches_claude_family_55(model_id, "sonnet")
            || matches_claude_family_55(model_id, "haiku")
            || matches_fable_mythos_5(model_id)
    }

    fn matches_opus_48_or_5(id: &str) -> bool {
        let Some(rest) = id.strip_prefix("claude-opus-") else {
            return false;
        };
        let tail = if let Some(rest) = rest.strip_prefix("4.8") {
            rest
        } else if let Some(rest) = rest.strip_prefix("4-8") {
            rest
        } else if let Some(rest) = rest.strip_prefix("5.5") {
            rest
        } else if let Some(rest) = rest.strip_prefix("5-5") {
            rest
        } else if let Some(rest) = rest.strip_prefix('5') {
            rest
        } else {
            return false;
        };
        is_optional_dated_suffix(tail)
    }

    fn matches_fable_mythos_5(id: &str) -> bool {
        for family in ["fable", "mythos"] {
            let prefix = format!("claude-{family}-5");
            if let Some(rest) = id.strip_prefix(&prefix) {
                let tail = if let Some(rest) = rest.strip_prefix(".1") {
                    rest
                } else if let Some(rest) = rest.strip_prefix("-1") {
                    rest
                } else {
                    rest
                };
                if is_optional_dated_suffix(tail) {
                    return true;
                }
            }
        }
        false
    }

    /// `isAnthropicAdaptiveThinkingModel(modelId)`。
    pub fn is_anthropic_adaptive_thinking_model(model_id: &str) -> bool {
        const NEEDLES: [&str; 16] = [
            "opus-4-6",
            "opus-4.6",
            "opus-4-7",
            "opus-4.7",
            "opus-4-8",
            "opus-4.8",
            "opus-5",
            "opus.5",
            "sonnet-4-6",
            "sonnet-4.6",
            "sonnet-5",
            "sonnet.5",
            "haiku-5",
            "haiku.5",
            "fable-5",
            "mythos-5",
        ];
        NEEDLES.iter().any(|needle| model_id.contains(needle))
    }

    /// `isAnthropicTemperatureUnsupportedModel(modelId)`。
    pub fn is_anthropic_temperature_unsupported_model(model_id: &str) -> bool {
        let id = model_id.to_lowercase();
        const NEEDLES: [&str; 10] = [
            "opus-4-7",
            "opus-4.7",
            "opus-4-8",
            "opus-4.8",
            "opus-5",
            "opus.5",
            "sonnet-5-5",
            "sonnet-5.5",
            "haiku-5-5",
            "haiku-5.5",
        ];
        NEEDLES.iter().any(|needle| id.contains(needle))
    }

    /// `getAnthropicMessagesCompat(provider, modelId)`；空对象等价 `undefined`。
    pub fn get_anthropic_messages_compat(provider: &str, model_id: &str) -> Map<String, Value> {
        let mut compat = Map::new();
        if contains_key(&VERIFIED_ANTHROPIC_MID_CONVO_EFFORT_PROVIDERS, provider)
            && supports_anthropic_mid_convo_effort(model_id)
            && !contains_key(
                &MID_CONVO_EFFORT_UNSUPPORTED_ANTHROPIC_MODELS,
                &format!("{provider}:{model_id}"),
            )
        {
            compat.insert("supportsMidConvoEffort".to_string(), Value::Bool(true));
        }
        if provider == "anthropic" && supports_anthropic_mid_convo_system_messages(model_id) {
            compat.insert(
                "supportsMidConvoSystemMessages".to_string(),
                Value::Bool(true),
            );
            compat.insert("supportsMidConvoToolChanges".to_string(), Value::Bool(true));
        }
        if (provider == "opencode" || provider == "github-copilot")
            && supports_anthropic_mid_convo_system_messages(model_id)
        {
            compat.insert(
                "supportsMidConvoSystemMessages".to_string(),
                Value::Bool(true),
            );
        }
        if contains_key(
            &EAGER_TOOL_INPUT_STREAMING_UNSUPPORTED_ANTHROPIC_MODELS,
            &format!("{provider}:{model_id}"),
        ) {
            compat.insert(
                "supportsEagerToolInputStreaming".to_string(),
                Value::Bool(false),
            );
        }
        if provider == "xiaomi" || provider.starts_with("xiaomi-token-plan-") {
            compat.insert("allowEmptySignature".to_string(), Value::Bool(true));
        }
        if (provider == "opencode" || provider == "opencode-go") && model_id == "qwen3.8-flash" {
            compat.insert("allowEmptySignature".to_string(), Value::Bool(true));
        }
        compat
    }

    /// `applyAnthropicMessagesCompatMetadata(model)`。
    fn apply_anthropic_messages_compat_metadata(model: &mut Value) {
        if str_at(model, "api") != Some("anthropic-messages") {
            return;
        }
        let provider = str_at(model, "provider").unwrap_or_default().to_string();
        let id = str_at(model, "id").unwrap_or_default().to_string();
        let compat = get_anthropic_messages_compat(&provider, &id);
        if compat.is_empty() {
            return;
        }
        merge_anthropic_compat(model, &compat);
        let supports_mid_convo_effort = compat
            .get("supportsMidConvoEffort")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        if supports_mid_convo_effort
            && !supports_anthropic_disabled_thinking_with_mid_convo_effort(&id)
        {
            merge_thinking_level_map(model, &level_map(&[("off", None)]));
        }
    }

    /// `isAnthropicFallbackMetadataModel(model)`。
    fn is_anthropic_fallback_metadata_model(model: &Value) -> bool {
        if str_at(model, "provider") != Some("anthropic")
            || str_at(model, "api") != Some("anthropic-messages")
        {
            return false;
        }
        let id = str_at(model, "id").unwrap_or_default();
        if ANTHROPIC_ALLOWED_FALLBACK_MODELS
            .iter()
            .any(|(key, _)| *key == id)
        {
            return true;
        }
        ANTHROPIC_ALLOWED_FALLBACK_MODELS
            .iter()
            .any(|(_, fallbacks)| fallbacks.contains(&id))
    }

    /// `applyAnthropicAllowedFallbackModelMetadata(models)`。
    ///
    /// 目标模型必须存在于本次列表；源模型支持 mid-convo effort 时，目标模型也必须支持，
    /// 否则该候选被跳过。`cost` 原样复制目标模型的 cost，不重新计算档位。
    pub fn apply_anthropic_allowed_fallback_model_metadata(models: &mut [Value]) {
        // 先建 id -> 模型索引，供 fallback 条目复制 provider/cost；改写集中在 updates，
        // 循环结束后统一写回，避免边遍历边改。
        let mut by_id: IndexMap<String, Value> = IndexMap::new();
        for model in models.iter() {
            by_id.insert(
                str_at(model, "id").unwrap_or_default().to_string(),
                model.clone(),
            );
        }
        let mut updates: Vec<(usize, Value)> = Vec::new();
        for (index, model) in models.iter().enumerate() {
            let id = str_at(model, "id").unwrap_or_default().to_string();
            let Some((_, fallback_ids)) = ANTHROPIC_ALLOWED_FALLBACK_MODELS
                .iter()
                .find(|(key, _)| *key == id)
            else {
                continue;
            };
            let supports_mid_convo_effort = current_compat(model)
                .get("supportsMidConvoEffort")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let mut allowed: Vec<Value> = Vec::new();
            for fallback_id in fallback_ids.iter().filter(|value| !value.is_empty()) {
                if supports_mid_convo_effort && !supports_anthropic_mid_convo_effort(fallback_id) {
                    continue;
                }
                let Some(fallback_model) = by_id.get(*fallback_id) else {
                    continue;
                };
                let mut entry = Map::new();
                entry.insert(
                    "provider".to_string(),
                    Value::String(
                        str_at(fallback_model, "provider")
                            .unwrap_or_default()
                            .to_string(),
                    ),
                );
                entry.insert(
                    "model".to_string(),
                    Value::String(str_at(fallback_model, "id").unwrap_or_default().to_string()),
                );
                entry.insert(
                    "cost".to_string(),
                    fallback_model.get("cost").cloned().unwrap_or(Value::Null),
                );
                allowed.push(Value::Object(entry));
            }
            if allowed.is_empty() {
                continue;
            }
            let mut updated = model.clone();
            let mut compat = Map::new();
            compat.insert("allowedFallbackModels".to_string(), Value::Array(allowed));
            merge_anthropic_compat(&mut updated, &compat);
            updates.push((index, updated));
        }
        for (index, updated) in updates {
            models[index] = updated;
        }
    }

    // ------------------------------------------------------------------
    // 其余 compat 元数据应用器
    // ------------------------------------------------------------------

    const OPENAI_TOOL_SEARCH_MODEL_IDS: [&str; 11] = [
        "gpt-5.4",
        "gpt-5.4-mini",
        "gpt-5.4-pro",
        "gpt-5.5",
        "gpt-5.6-sol",
        "gpt-5.6-terra",
        "gpt-5.6-luna",
        "gpt-6-astra",
        "gpt-6-sol",
        "gpt-6-luna",
        "gpt-6.1-sol",
    ];
    const OPENAI_CODEX_ADDITIONAL_TOOLS_MODEL_IDS: [&str; 7] = [
        "gpt-5.6-sol",
        "gpt-5.6-terra",
        "gpt-5.6-luna",
        "gpt-6-astra",
        "gpt-6-sol",
        "gpt-6-luna",
        "gpt-6.1-sol",
    ];
    const OPENAI_MID_CONVO_EFFORT_MODEL_IDS: [&str; 4] =
        ["gpt-6-astra", "gpt-6-sol", "gpt-6-luna", "gpt-6.1-sol"];
    const OPENAI_GRAMMAR_TOOL_PROVIDERS: [&str; 6] = [
        "openai",
        "openai-codex",
        "azure",
        "github-copilot",
        "opencode",
        "cloudflare-ai-gateway",
    ];
    const OPENAI_GRAMMAR_TOOL_APIS: [&str; 3] = [
        "openai-responses",
        "azure-openai-responses",
        "openai-codex-responses",
    ];
    const OPENAI_RESPONSES_PROXY_PROVIDERS: [&str; 3] =
        ["opencode", "opencode-go", "github-copilot"];
    const OPENAI_RESPONSES_NONE_REASONING_MODELS: [&str; 12] = [
        "gpt-5.1",
        "gpt-5.2",
        "gpt-5.3-codex",
        "gpt-5.4",
        "gpt-5.4-mini",
        "gpt-5.4-nano",
        "gpt-5.5",
        "gpt-5.6-sol",
        "gpt-5.6-terra",
        "gpt-5.6-luna",
        "gpt-6-sol",
        "gpt-6-luna",
    ];

    /// `applyStrictToolCompatMetadata(model)`。
    fn apply_strict_tool_compat_metadata(model: &mut Value) {
        let provider = str_at(model, "provider").unwrap_or_default();
        let api = str_at(model, "api").unwrap_or_default();
        if (provider == "openai" || provider == "cloudflare-ai-gateway")
            && api == "openai-responses"
        {
            let mut compat = Map::new();
            compat.insert("supportsStrictMode".to_string(), Value::Bool(true));
            let merged = js_spread(&current_compat(model), &compat);
            set_compat(model, merged);
        } else if provider == "anthropic" && api == "anthropic-messages" {
            let mut compat = Map::new();
            compat.insert("supportsStrictTools".to_string(), Value::Bool(true));
            merge_anthropic_compat(model, &compat);
        }
    }

    /// `/^gpt-(\d+)/` 的首个数字组。
    fn gpt_major_version(id: &str) -> Option<f64> {
        let rest = id.strip_prefix("gpt-")?;
        let end = rest
            .find(|c: char| !c.is_ascii_digit())
            .unwrap_or(rest.len());
        if end == 0 {
            return None;
        }
        rest[..end].parse::<f64>().ok()
    }

    /// `applyOpenAIGrammarToolCompatMetadata(model)`。
    fn apply_openai_grammar_tool_compat_metadata(model: &mut Value) {
        let api = str_at(model, "api").unwrap_or_default();
        let provider = str_at(model, "provider").unwrap_or_default();
        if !contains_key(&OPENAI_GRAMMAR_TOOL_APIS, api)
            || !contains_key(&OPENAI_GRAMMAR_TOOL_PROVIDERS, provider)
        {
            return;
        }
        let id = str_at(model, "id").unwrap_or_default();
        let Some(major) = gpt_major_version(id) else {
            return;
        };
        if major < 5.0 {
            return;
        }
        let mut compat = Map::new();
        compat.insert("supportsOpenAIGrammarTools".to_string(), Value::Bool(true));
        let merged = js_spread(&current_compat(model), &compat);
        set_compat(model, merged);
    }

    /// `applyOpenAIToolSearchMetadata(model)`。
    fn apply_openai_tool_search_metadata(model: &mut Value) {
        let provider = str_at(model, "provider").unwrap_or_default();
        let api = str_at(model, "api").unwrap_or_default();
        let id = str_at(model, "id").unwrap_or_default();
        let is_openai_responses = provider == "openai" && api == "openai-responses";
        let is_openai_codex = provider == "openai-codex" && api == "openai-codex-responses";
        if !(is_openai_responses || is_openai_codex)
            || !contains_key(&OPENAI_TOOL_SEARCH_MODEL_IDS, id)
        {
            return;
        }
        let supports_additional_tools = (is_openai_responses
            && contains_key(&OPENAI_TOOL_SEARCH_MODEL_IDS, id))
            || (is_openai_codex && contains_key(&OPENAI_CODEX_ADDITIONAL_TOOLS_MODEL_IDS, id));
        let mut compat = Map::new();
        if supports_additional_tools {
            compat.insert("supportsAdditionalTools".to_string(), Value::Bool(true));
        }
        compat.insert("supportsToolSearch".to_string(), Value::Bool(true));
        let merged = js_spread(&current_compat(model), &compat);
        set_compat(model, merged);
    }

    /// `applyOpenAICompletionsTranscriptMetadata(model)`。
    fn apply_openai_completions_transcript_metadata(model: &mut Value) {
        if str_at(model, "api") != Some("openai-completions") {
            return;
        }
        let provider = str_at(model, "provider").unwrap_or_default();
        let id = str_at(model, "id").unwrap_or_default();
        let is_kimi_k3 = (provider.starts_with("moonshot") && is_kimi_k3_family_id(id))
            || (provider == "fireworks" && id.contains("kimi-k3"))
            || ((provider == "opencode" || provider == "opencode-go") && is_kimi_k3_family_id(id));
        let is_moonshot_kimi_k2 = provider.starts_with("moonshot")
            && (id == "kimi-k2.6" || id == "kimi-k2.7-code" || id == "kimi-k2.7-code-highspeed");
        let is_text_only = is_moonshot_kimi_k2
            || (provider == "github-copilot" && id == "kimi-k3")
            || (provider == "deepseek" && id == "deepseek-v4-pro")
            || (provider == "openrouter"
                && id.starts_with("openai/")
                && contains_key(
                    &OPENAI_TOOL_SEARCH_MODEL_IDS,
                    id.strip_prefix("openai/").unwrap_or(""),
                ));
        if !is_kimi_k3 && !is_text_only {
            return;
        }
        let mut compat = Map::new();
        compat.insert(
            "supportsMidConvoSystemMessages".to_string(),
            Value::Bool(true),
        );
        if is_kimi_k3 {
            compat.insert(
                "supportsMidConvoToolAdditions".to_string(),
                Value::Bool(true),
            );
        }
        let merged = js_spread(&current_compat(model), &compat);
        set_compat(model, merged);
    }

    /// `applyOpenAIResponsesTranscriptMetadata(model)`。
    fn apply_openai_responses_transcript_metadata(model: &mut Value) {
        let provider = str_at(model, "provider").unwrap_or_default();
        let api = str_at(model, "api").unwrap_or_default();
        let id = str_at(model, "id").unwrap_or_default();
        let is_openai_responses = provider == "openai" && api == "openai-responses";
        let is_openai_codex = provider == "openai-codex" && api == "openai-codex-responses";
        let is_proxied_responses =
            contains_key(&OPENAI_RESPONSES_PROXY_PROVIDERS, provider) && api == "openai-responses";
        if !(is_openai_responses || is_openai_codex || is_proxied_responses)
            || !contains_key(&OPENAI_TOOL_SEARCH_MODEL_IDS, id)
        {
            return;
        }
        let mut compat = Map::new();
        compat.insert(
            "supportsMidConvoSystemMessages".to_string(),
            Value::Bool(true),
        );
        if is_proxied_responses {
            compat.insert("supportsAdditionalTools".to_string(), Value::Bool(true));
        }
        let merged = js_spread(&current_compat(model), &compat);
        set_compat(model, merged);
    }

    /// `applyOpenAIExplicitPromptCacheMetadata(model)`。
    fn apply_openai_explicit_prompt_cache_metadata(model: &mut Value) {
        if str_at(model, "provider") != Some("openai")
            || str_at(model, "api") != Some("openai-responses")
        {
            return;
        }
        let cache_write = js_number_or_zero(nested(model, &["cost", "cacheWrite"]));
        // `!(cacheWrite > 0)`（NaN 也走这条）——写成显式比较避免 clippy 的部分序告警。
        if cache_write <= 0.0 || cache_write.is_nan() {
            return;
        }
        let mut compat = Map::new();
        compat.insert(
            "supportsExplicitPromptCacheMode".to_string(),
            Value::Bool(true),
        );
        let merged = js_spread(&current_compat(model), &compat);
        set_compat(model, merged);
    }

    /// `applyOpenAIMidConvoEffortMetadata(model)`。
    fn apply_openai_mid_convo_effort_metadata(model: &mut Value) {
        if str_at(model, "provider") != Some("openai")
            || str_at(model, "api") != Some("openai-responses")
        {
            return;
        }
        let id = str_at(model, "id").unwrap_or_default();
        if !contains_key(&OPENAI_MID_CONVO_EFFORT_MODEL_IDS, id) {
            return;
        }
        let mut compat = Map::new();
        compat.insert("supportsMidConvoEffort".to_string(), Value::Bool(true));
        let merged = js_spread(&current_compat(model), &compat);
        set_compat(model, merged);
    }

    /// `applyPromptCacheMetadata(model)`：只有直连 Anthropic 拿到 `promptCache`。
    fn apply_prompt_cache_metadata(model: &mut Value) {
        if str_at(model, "provider") != Some("anthropic")
            || str_at(model, "api") != Some("anthropic-messages")
        {
            return;
        }
        let Some(object) = model.as_object_mut() else {
            return;
        };
        let mut cache = Map::new();
        for (key, value) in ANTHROPIC_PROMPT_CACHE {
            cache.insert(key.to_string(), json!(value));
        }
        object.insert("promptCache".to_string(), Value::Object(cache));
    }

    /// `applyImageInputMetadata(model)`。
    pub fn apply_image_input_metadata(model: &mut Value) {
        let input = model
            .get("input")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        if !input.iter().any(|value| value.as_str() == Some("image")) {
            return;
        }
        let provider = str_at(model, "provider").unwrap_or_default();
        let model_type = str_at(model, "type").unwrap_or_default();
        let context_window = js_number_or_zero(model.get("contextWindow"));

        // providerLimits（`undefined` 表示不写）。
        // 先给各家 provider 的硬限制；None 表示该厂商没有已知限制，不写 providerLimits。
        let provider_limits: Option<Value> = if provider == "anthropic" {
            let max_per_request = if model_type != "image" && context_window == 200_000.0 {
                100
            } else {
                600
            };
            Some(json!({
                "maxRequestBytes": 32 * 1024 * 1024,
                "images": { "maxPerRequest": max_per_request },
            }))
        } else if provider == "amazon-bedrock" {
            Some(json!({ "images": { "maxPerMessage": 20 } }))
        } else if provider == "openai" {
            Some(json!({
                "maxRequestBytes": 512 * 1024 * 1024,
                "images": { "maxPerRequest": 1500 },
            }))
        } else if provider == "google" {
            Some(json!({
                "maxRequestBytes": 20 * 1024 * 1024,
                "images": { "maxPerRequest": 3600 },
            }))
        } else {
            None
        };

        let existing = model
            .get("inputLimits")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        let configured_images = existing
            .get("images")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        let provider_images = provider_limits
            .as_ref()
            .and_then(|limits| limits.get("images"))
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();

        // 合并优先级：provider 默认 < 模型已有的 inputLimits；resize 先铺默认值，
        // 再让模型配置逐字段覆盖（所以下面合并 images 时先跳过配置里的 resize）。
        // `{...providerLimits, ...model.inputLimits, images: {...providerImages, ...configuredImages, resize}}`
        let mut limits = Map::new();
        if let Some(provider_limits) = &provider_limits {
            for (key, value) in provider_limits.as_object().into_iter().flatten() {
                limits.insert(key.clone(), value.clone());
            }
        }
        for (key, value) in &existing {
            limits.insert(key.clone(), value.clone());
        }
        let mut images = Map::new();
        for (key, value) in &provider_images {
            images.insert(key.clone(), value.clone());
        }
        for (key, value) in &configured_images {
            if key == "resize" {
                continue;
            }
            images.insert(key.clone(), value.clone());
        }
        let configured_resize = configured_images
            .get("resize")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        let mut resize = Map::new();
        resize.insert("maxWidth".to_string(), json!(2000));
        resize.insert("maxHeight".to_string(), json!(2000));
        resize.insert(
            "maxBytes".to_string(),
            json!(DEFAULT_IMAGE_RESIZE_MAX_BYTES),
        );
        resize.insert("jpegQuality".to_string(), json!(80));
        for (key, value) in &configured_resize {
            resize.insert(key.clone(), value.clone());
        }
        images.insert("resize".to_string(), Value::Object(resize));
        limits.insert("images".to_string(), Value::Object(images));

        if let Some(object) = model.as_object_mut() {
            object.insert("inputLimits".to_string(), Value::Object(limits));
        }
    }

    /// `supportsOpenAiXhigh(modelId)`。
    pub fn supports_open_ai_xhigh(model_id: &str) -> bool {
        [
            "gpt-5.2", "gpt-5.3", "gpt-5.4", "gpt-5.5", "gpt-5.6", "gpt-6",
        ]
        .iter()
        .any(|needle| model_id.contains(needle))
    }

    /// `supportsOpenAiMax(model)`。
    pub fn supports_open_ai_max(model: &Value) -> bool {
        let id = str_at(model, "id").unwrap_or_default();
        let api = str_at(model, "api").unwrap_or_default();
        (id.contains("gpt-5.6") || id.contains("gpt-6"))
            && matches!(
                api,
                "openai-responses"
                    | "azure-openai-responses"
                    | "openai-codex-responses"
                    | "openai-completions"
            )
    }

    // ------------------------------------------------------------------
    // applyThinkingLevelMetadata
    // ------------------------------------------------------------------

    const DEEPSEEK_V4_THINKING_LEVEL_MAP: [(&str, Option<&str>); 5] = [
        ("minimal", None),
        ("low", None),
        ("medium", None),
        ("high", Some("high")),
        ("max", Some("max")),
    ];
    const DEEPSEEK_V4_FLASH_THINKING_LEVEL_MAP: [(&str, Option<&str>); 5] = [
        ("minimal", None),
        ("low", Some("low")),
        ("medium", None),
        ("high", Some("high")),
        ("max", Some("max")),
    ];
    const AZURE_DEEPSEEK_V4_THINKING_LEVEL_MAP: [(&str, Option<&str>); 6] = [
        ("minimal", None),
        ("low", Some("low")),
        ("medium", Some("medium")),
        ("high", Some("high")),
        ("xhigh", None),
        ("max", None),
    ];
    const OPENCODE_GO_GLM52_THINKING_LEVEL_MAP: [(&str, Option<&str>); 6] = [
        ("off", None),
        ("minimal", None),
        ("low", None),
        ("medium", None),
        ("high", Some("high")),
        ("max", Some("max")),
    ];
    const ANT_LING_RING_THINKING_LEVEL_MAP: [(&str, Option<&str>); 6] = [
        ("off", None),
        ("minimal", None),
        ("low", None),
        ("medium", None),
        ("high", Some("high")),
        ("xhigh", Some("xhigh")),
    ];
    type ThinkingLevelOverrides = &'static [(&'static str, Option<&'static str>)];
    const GITHUB_COPILOT_THINKING_LEVEL_OVERRIDES: [(&str, ThinkingLevelOverrides); 3] = [
        ("claude-opus-4.8", &[("minimal", Some("low"))]),
        ("claude-opus-5", &[("minimal", Some("low"))]),
        (
            "claude-sonnet-4.6",
            &[("minimal", Some("low")), ("max", Some("max"))],
        ),
    ];
    const FIREWORKS_ADAPTIVE_THINKING_FALLBACK_MODELS: [&str; 5] = [
        "accounts/fireworks/models/deepseek-v4-flash-0731",
        "accounts/fireworks/models/deepseek-v4-flash-vision-exp",
        "accounts/fireworks/models/deepseek-v4-pro-0813",
        "accounts/fireworks/models/qwen3p8-max",
        "accounts/fireworks/models/qwen3p8-2p4t-a95b",
    ];

    /// `applyThinkingLevelMetadata(model)`。
    pub fn apply_thinking_level_metadata(model: &mut Value, state: &GeneratorState) {
        // 绝大多数分支按 key 合并覆盖、不整体替换（对应 TS 的 `{...model.thinkingLevelMap,
        // ...map}`），一个分支可以一次写入多个键；个别分支（如 Fireworks 的 qwen3p8-max）
        // 在模型没有既有映射时直接调用 set_thinking_level_map 整体写入。
        let id = str_at(model, "id").unwrap_or_default().to_string();
        let api = str_at(model, "api").unwrap_or_default().to_string();
        let provider = str_at(model, "provider").unwrap_or_default().to_string();
        let id = id.as_str();
        let api = api.as_str();
        let provider = provider.as_str();

        // 阶段 1：OpenAI 家族（GPT-5/6、Codex、Azure）的 off/xhigh/max 能力，以及 Claude
        // 4.6+/5 系列（opus/sonnet/haiku、fable-5）的 max/xhigh 映射。
        if (api == "openai-responses" || api == "azure-openai-responses") && id.starts_with("gpt-5")
        {
            merge_thinking_level_map(model, &level_map(&[("off", None)]));
        }
        if (id == "gpt-6-astra" || id == "gpt-6-sol" || id == "gpt-6-luna" || id == "gpt-6.1-sol")
            && (api == "openai-responses"
                || api == "azure-openai-responses"
                || api == "openai-codex-responses")
        {
            let off = if id == "gpt-6-astra" || id == "gpt-6.1-sol" {
                None
            } else {
                Some("none")
            };
            merge_thinking_level_map(
                model,
                &level_map(&[
                    ("off", off),
                    ("minimal", None),
                    ("low", Some("low")),
                    ("medium", Some("medium")),
                    ("high", Some("high")),
                    ("xhigh", Some("xhigh")),
                    ("max", Some("max")),
                ]),
            );
        }
        if provider == "github-copilot" && id.starts_with("gpt-5") {
            merge_thinking_level_map(model, &level_map(&[("minimal", Some("low"))]));
        }
        if api == "openai-responses"
            && provider == "openai"
            && contains_key(&OPENAI_RESPONSES_NONE_REASONING_MODELS, id)
        {
            merge_thinking_level_map(model, &level_map(&[("off", Some("none"))]));
        }
        if provider == "xai" && api == "openai-responses" && !has_thinking_level_map(model) {
            merge_thinking_level_map(model, &level_map(&[("off", None), ("minimal", None)]));
        }
        if supports_open_ai_xhigh(id) {
            merge_thinking_level_map(model, &level_map(&[("xhigh", Some("xhigh"))]));
        }
        if supports_open_ai_max(model) {
            merge_thinking_level_map(model, &level_map(&[("max", Some("max"))]));
        }
        if provider == "openai" && id == "gpt-5.5" {
            merge_thinking_level_map(model, &level_map(&[("minimal", None)]));
        }
        if id.ends_with("gpt-5.5-pro") {
            merge_thinking_level_map(
                model,
                &level_map(&[("off", None), ("minimal", None), ("low", None)]),
            );
        }
        if id.contains("opus-4-6")
            || id.contains("opus-4.6")
            || id.contains("sonnet-4-6")
            || id.contains("sonnet-4.6")
        {
            merge_thinking_level_map(model, &level_map(&[("max", Some("max"))]));
        }
        const XHIGH_MAX_NEEDLES: [&str; 10] = [
            "opus-4-7", "opus-4.7", "opus-4-8", "opus-4.8", "opus-5", "opus.5", "sonnet-5",
            "sonnet.5", "haiku-5", "haiku.5",
        ];
        if XHIGH_MAX_NEEDLES.iter().any(|needle| id.contains(needle)) {
            merge_thinking_level_map(
                model,
                &level_map(&[("xhigh", Some("xhigh")), ("max", Some("max"))]),
            );
        }
        if id.contains("fable-5") {
            merge_thinking_level_map(
                model,
                &level_map(&[
                    ("off", None),
                    ("xhigh", Some("xhigh")),
                    ("max", Some("max")),
                ]),
            );
        }
        // 阶段 2：Anthropic 家族（含 OpenRouter/GitHub Copilot 的代理）的 adaptive thinking
        // 与 temperature 兼容标记。
        if api == "anthropic-messages" && is_anthropic_adaptive_thinking_model(id) {
            let mut compat = Map::new();
            compat.insert("forceAdaptiveThinking".to_string(), Value::Bool(true));
            merge_anthropic_compat(model, &compat);
        }
        if api == "anthropic-messages" && is_anthropic_temperature_unsupported_model(id) {
            let mut compat = Map::new();
            compat.insert("supportsTemperature".to_string(), Value::Bool(false));
            merge_anthropic_compat(model, &compat);
        }
        // 阶段 3：DeepSeek V4 的 effort 映射随 provider 变化（OpenRouter/Azure/flash/默认四套）。
        if api == "openai-completions"
            && id.contains("deepseek-v4")
            && !has_thinking_level_map(model)
        {
            let map = if provider == "openrouter" {
                level_map(&[
                    ("minimal", None),
                    ("low", None),
                    ("medium", None),
                    ("high", Some("high")),
                    ("max", Some("max")),
                    ("xhigh", Some("xhigh")),
                ])
            } else if provider == "azure" {
                level_map(&AZURE_DEEPSEEK_V4_THINKING_LEVEL_MAP)
            } else if (provider == "deepseek"
                || provider == "opencode"
                || provider == "opencode-go")
                && id.contains("deepseek-v4-flash")
            {
                level_map(&DEEPSEEK_V4_FLASH_THINKING_LEVEL_MAP)
            } else {
                level_map(&DEEPSEEK_V4_THINKING_LEVEL_MAP)
            };
            merge_thinking_level_map(model, &map);
        }
        if provider == "groq" && id == "qwen/qwen3.6-27b" {
            merge_thinking_level_map(
                model,
                &level_map(&[
                    ("minimal", None),
                    ("low", None),
                    ("medium", None),
                    ("high", Some("default")),
                ]),
            );
        }
        if provider == "openai-codex" && supports_open_ai_xhigh(id) {
            merge_thinking_level_map(model, &level_map(&[("minimal", Some("low"))]));
        }
        if (provider == "moonshotai" || provider == "moonshotai-cn")
            && (id == "kimi-k2.7-code" || id == "kimi-k2.7-code-highspeed")
        {
            merge_thinking_level_map(model, &level_map(&[("off", None)]));
        }
        if provider == "openrouter" && id.starts_with("inception/mercury-2") {
            merge_thinking_level_map(model, &level_map(&[("off", None)]));
        }
        if provider == "openrouter" && id == "z-ai/glm-5.2" {
            merge_thinking_level_map(model, &level_map(&[("xhigh", Some("xhigh"))]));
        }
        // 阶段 4：Fireworks 的 adaptive/effort 回退（其后是 opencode-go、opencode、ant-ling、
        // github-copilot 等 provider 的逐条覆盖）。
        if provider == "fireworks" {
            let force_adaptive_thinking = current_compat(model)
                .get("forceAdaptiveThinking")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            if api == "anthropic-messages" && force_adaptive_thinking {
                if id == "accounts/fireworks/models/qwen3p8-max" && !has_thinking_level_map(model) {
                    let effort =
                        get_effort_thinking_level_map(&[ModelsDevReasoningOption::Effort {
                            values: vec![
                                Some("low".to_string()),
                                Some("medium".to_string()),
                                Some("xhigh".to_string()),
                            ],
                        }]);
                    if let Some(effort) = effort {
                        set_thinking_level_map(model, &effort);
                    }
                }
                let reasoning_options = state.options_for(model);
                if reasoning_options.map(|options| {
                    options
                        .iter()
                        .any(|option| matches!(option, ModelsDevReasoningOption::Toggle))
                }) == Some(true)
                    || id == "accounts/fireworks/models/qwen3p8-2p4t-a95b"
                {
                    merge_thinking_level_map(model, &level_map(&[("off", Some("none"))]));
                }
                if id == "accounts/fireworks/models/deepseek-v4-pro-0813" {
                    merge_thinking_level_map(model, &level_map(&[("low", Some("low"))]));
                }
            }
            if id.contains("glm-5p2") {
                merge_thinking_level_map(
                    model,
                    &level_map(&[
                        ("off", Some("none")),
                        ("minimal", None),
                        ("low", None),
                        ("medium", None),
                        ("max", Some("max")),
                    ]),
                );
            }
            if id.contains("kimi-k3") {
                merge_thinking_level_map(model, &level_map(&[("medium", None)]));
            }
        }
        if provider == "opencode-go" && id == "glm-5.2" {
            merge_thinking_level_map(model, &level_map(&OPENCODE_GO_GLM52_THINKING_LEVEL_MAP));
        }
        if provider == "opencode-go" && id == "kimi-k2.6" {
            merge_thinking_level_map(
                model,
                &level_map(&[("minimal", None), ("low", None), ("medium", None)]),
            );
        }
        if provider == "opencode" && id == "grok-build-0.1" {
            merge_thinking_level_map(
                model,
                &level_map(&[
                    ("off", None),
                    ("minimal", None),
                    ("low", None),
                    ("medium", None),
                ]),
            );
        }
        if provider == "ant-ling" && model.get("reasoning").and_then(Value::as_bool) == Some(true) {
            merge_thinking_level_map(model, &level_map(&ANT_LING_RING_THINKING_LEVEL_MAP));
        }
        if provider == "github-copilot"
            && let Some((_, override_map)) = GITHUB_COPILOT_THINKING_LEVEL_OVERRIDES
                .iter()
                .find(|(model_id, _)| *model_id == id)
        {
            merge_thinking_level_map(model, &level_map(override_map));
        }
    }

    /// `applyModelsDevReasoningOptionMetadata(model)`。
    pub fn apply_models_dev_reasoning_option_metadata(model: &mut Value, state: &GeneratorState) {
        let Some(options) = state.options_for(model) else {
            return;
        };
        if !supports_direct_reasoning_effort(model) {
            return;
        }
        if let Some(map) = get_effort_thinking_level_map(options) {
            merge_thinking_level_map(model, &map);
        }
    }

    /// `supportsDirectReasoningEffort(model)`。
    pub fn supports_direct_reasoning_effort(model: &Value) -> bool {
        let api = str_at(model, "api").unwrap_or_default();
        if api == "anthropic-messages" {
            return current_compat(model)
                .get("forceAdaptiveThinking")
                .and_then(Value::as_bool)
                .unwrap_or(false);
        }
        if api == "openai-responses"
            || api == "azure-openai-responses"
            || api == "openai-codex-responses"
        {
            return true;
        }
        if api != "openai-completions" {
            return false;
        }
        let detected = detect_openai_completions_compat(model);
        let compat = js_spread(&detected, &current_compat(model));
        let thinking_format = compat
            .get("thinkingFormat")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let supports = compat
            .get("supportsReasoningEffort")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        thinking_format == "openai" && supports
    }

    // ------------------------------------------------------------------
    // provider 级模型构造
    // ------------------------------------------------------------------

    /// 通用 chat 模型构造：TS 里各 provider 循环里那些几乎相同的对象字面量。
    struct ChatModelSpec<'a> {
        id: String,
        name: String,
        api: &'a str,
        provider: &'a str,
        base_url: &'a str,
        reasoning: bool,
        input: Value,
        cost: Value,
        /// `m.limit?.context || 4096` 的原始值：上游给非 number 时保持原样（TS 不做归一化，
        /// 由 `validateModelDataDirectory` 拒绝）。
        context_window: Value,
        max_tokens: Value,
    }

    /// TS 各 provider 的对象字面量字段序。
    ///
    /// JSON 对象字段序没有语义，但生成的数据文件是逐字节入库的产物：TS 直接
    /// `JSON.stringify` 字面量，字段序就是字面量里写下的顺序（`...spread` 先展开，
    /// 之后由 metadata 应用器补的键追加到末尾）。这里按字面量还原那份顺序。
    type ModelOrder = &'static [&'static str];

    /// `{ id, name, api, provider, baseUrl, reasoning, input, cost, contextWindow, maxTokens }`
    /// —— 大多数 provider 循环的字面量都是这个顺序。
    const DEFAULT_CHAT_ORDER: ModelOrder = &[
        "id",
        "name",
        "api",
        "provider",
        "baseUrl",
        "reasoning",
        "input",
        "cost",
        "contextWindow",
        "maxTokens",
    ];

    /// `{ id, name, api, baseUrl, provider, reasoning, input, cost, contextWindow, maxTokens }`
    /// —— `missingOpenAiModels` / `codexModels` / `deepseekModels` / `antLingModels` 等手写字面量。
    const BASE_URL_FIRST_ORDER: ModelOrder = &[
        "id",
        "name",
        "api",
        "baseUrl",
        "provider",
        "reasoning",
        "input",
        "cost",
        "contextWindow",
        "maxTokens",
    ];

    /// `{ id, name, api, baseUrl, provider, reasoning, input, compat, cost, contextWindow, maxTokens }`
    /// —— Vercel AI Gateway 的 anthropic-messages 字面量。
    const AI_GATEWAY_ORDER: ModelOrder = &[
        "id",
        "name",
        "api",
        "baseUrl",
        "provider",
        "reasoning",
        "input",
        "compat",
        "cost",
        "contextWindow",
        "maxTokens",
    ];

    /// `{ id, name, provider, reasoning, input, cost, contextWindow, maxTokens, api, baseUrl, compat }`
    /// —— Fireworks 的 `common` 先展开，`api`/`baseUrl`/`compat` 后写。
    const FIREWORKS_ORDER: ModelOrder = &[
        "id",
        "name",
        "provider",
        "reasoning",
        "input",
        "cost",
        "contextWindow",
        "maxTokens",
        "api",
        "baseUrl",
        "compat",
    ];

    /// `{ id, name, api, provider, baseUrl, headers, reasoning, input, cost, compat, contextWindow, maxTokens }`
    /// —— NVIDIA NIM。
    const HEADERS_EARLY_ORDER: ModelOrder = &[
        "id",
        "name",
        "api",
        "provider",
        "baseUrl",
        "headers",
        "reasoning",
        "input",
        "cost",
        "compat",
        "contextWindow",
        "maxTokens",
    ];

    /// `{ id, name, api, provider, baseUrl, reasoning, thinkingLevelMap, input, cost, contextWindow, maxTokens }`
    /// —— Google / Google Vertex / Mistral。
    const THINKING_EARLY_ORDER: ModelOrder = &[
        "id",
        "name",
        "api",
        "provider",
        "baseUrl",
        "reasoning",
        "thinkingLevelMap",
        "input",
        "cost",
        "contextWindow",
        "maxTokens",
    ];

    /// `{ id, name, api, provider, baseUrl, reasoning, input, cost, compat, contextWindow, maxTokens }`
    /// —— Hugging Face。
    const COMPAT_AFTER_COST_ORDER: ModelOrder = &[
        "id",
        "name",
        "api",
        "provider",
        "baseUrl",
        "reasoning",
        "input",
        "cost",
        "compat",
        "contextWindow",
        "maxTokens",
    ];

    /// `{ id, name, api, provider, baseUrl, reasoning, thinkingLevelMap, input, cost, compat, contextWindow, maxTokens }`
    /// —— Baseten / Together / Z.ai。
    const THINKING_COMPAT_ORDER: ModelOrder = &[
        "id",
        "name",
        "api",
        "provider",
        "baseUrl",
        "reasoning",
        "thinkingLevelMap",
        "input",
        "cost",
        "compat",
        "contextWindow",
        "maxTokens",
    ];

    /// `{ id, name, api, provider, baseUrl, compat, reasoning, input, cost, contextWindow, maxTokens }`
    /// —— xAI。
    const COMPAT_EARLY_ORDER: ModelOrder = &[
        "id",
        "name",
        "api",
        "provider",
        "baseUrl",
        "compat",
        "reasoning",
        "input",
        "cost",
        "contextWindow",
        "maxTokens",
    ];

    /// `{ id, name, api, provider, baseUrl, compat, reasoning, thinkingLevelMap, input, cost, contextWindow, maxTokens }`
    /// —— Xiaomi MiMo。
    const XIAOMI_ORDER: ModelOrder = &[
        "id",
        "name",
        "api",
        "provider",
        "baseUrl",
        "compat",
        "reasoning",
        "thinkingLevelMap",
        "input",
        "cost",
        "contextWindow",
        "maxTokens",
    ];

    /// `{ id, name, api, provider, baseUrl, compat, thinkingLevelMap, reasoning, input, cost, contextWindow, maxTokens }`
    /// —— Alibaba Token Plan。
    const QWEN_TOKEN_PLAN_ORDER: ModelOrder = &[
        "id",
        "name",
        "api",
        "provider",
        "baseUrl",
        "compat",
        "thinkingLevelMap",
        "reasoning",
        "input",
        "cost",
        "contextWindow",
        "maxTokens",
    ];

    /// `{ id, name, api, provider, baseUrl, compat, reasoning, input, cost, contextWindow, maxTokens, thinkingLevelMap }`
    /// —— Kimi Coding。
    const KIMI_CODING_ORDER: ModelOrder = &[
        "id",
        "name",
        "api",
        "provider",
        "baseUrl",
        "compat",
        "reasoning",
        "input",
        "cost",
        "contextWindow",
        "maxTokens",
        "thinkingLevelMap",
    ];

    /// `{ id, name, api, provider, baseUrl, reasoning, thinkingLevelMap, input, cost, compat, contextWindow, maxTokens }`
    /// —— OpenCode Zen / Go。
    const OPENCODE_ORDER: ModelOrder = &[
        "id",
        "name",
        "api",
        "provider",
        "baseUrl",
        "reasoning",
        "thinkingLevelMap",
        "input",
        "cost",
        "compat",
        "contextWindow",
        "maxTokens",
    ];

    /// `{ id, name, api, baseUrl, provider, reasoning, thinkingLevelMap, input, cost, contextWindow, maxTokens, compat }`
    /// —— `deepseekModels` 的 flash 条目。
    const DEEPSEEK_FLASH_ORDER: ModelOrder = &[
        "id",
        "name",
        "api",
        "baseUrl",
        "provider",
        "reasoning",
        "thinkingLevelMap",
        "input",
        "cost",
        "contextWindow",
        "maxTokens",
        "compat",
    ];

    /// 按 `order` 重建对象：`order` 里出现的键按该顺序写出（可选键缺失则跳过），
    /// 其余键保持原相对顺序接在后面——与 TS「字面量 + 后续赋值」的插入顺序一致。
    fn reorder_model(model: &mut Value, order: ModelOrder) {
        let Some(object) = model.as_object() else {
            return;
        };
        let mut rebuilt = Map::new();
        for key in order {
            if let Some(value) = object.get(*key) {
                rebuilt.insert((*key).to_string(), value.clone());
            }
        }
        for (key, value) in object {
            if !rebuilt.contains_key(key) {
                rebuilt.insert(key.clone(), value.clone());
            }
        }
        *model = Value::Object(rebuilt);
    }

    fn build_chat_model(spec: ChatModelSpec<'_>) -> Value {
        let mut object = Map::new();
        object.insert("id".to_string(), Value::String(spec.id));
        object.insert("name".to_string(), Value::String(spec.name));
        object.insert("api".to_string(), Value::String(spec.api.to_string()));
        object.insert(
            "provider".to_string(),
            Value::String(spec.provider.to_string()),
        );
        object.insert(
            "baseUrl".to_string(),
            Value::String(spec.base_url.to_string()),
        );
        object.insert("reasoning".to_string(), Value::Bool(spec.reasoning));
        object.insert("input".to_string(), spec.input);
        object.insert("cost".to_string(), spec.cost);
        object.insert("contextWindow".to_string(), spec.context_window);
        object.insert("maxTokens".to_string(), spec.max_tokens);
        Value::Object(object)
    }

    /// `getTogetherThinkingLevelMap(modelId, reasoning)`。
    pub fn get_together_thinking_level_map(
        model_id: &str,
        reasoning: bool,
    ) -> Option<super::ThinkingLevelMap> {
        if !reasoning {
            return None;
        }
        if model_id == "openai/gpt-oss-20b" || model_id == "openai/gpt-oss-120b" {
            return Some(level_map(&[("off", None), ("minimal", None)]));
        }
        if model_id == "deepseek-ai/DeepSeek-V4-Pro-0813" {
            return Some(level_map(&[
                ("minimal", None),
                ("low", None),
                ("medium", None),
                ("high", Some("high")),
                ("xhigh", None),
            ]));
        }
        Some(level_map(&[
            ("minimal", None),
            ("low", None),
            ("medium", None),
        ]))
    }

    /// `getTogetherCompat(modelId, reasoning)`。
    pub fn get_together_compat(model_id: &str, reasoning: bool) -> Map<String, Value> {
        let base = || {
            let mut compat = Map::new();
            compat.insert("supportsStore".to_string(), Value::Bool(false));
            compat.insert("supportsDeveloperRole".to_string(), Value::Bool(false));
            compat.insert("supportsReasoningEffort".to_string(), Value::Bool(false));
            compat.insert(
                "maxTokensField".to_string(),
                Value::String("max_tokens".to_string()),
            );
            compat.insert("supportsStrictMode".to_string(), Value::Bool(false));
            compat.insert("supportsLongCacheRetention".to_string(), Value::Bool(false));
            compat
        };
        if !reasoning {
            return base();
        }
        let is_effort = model_id == "openai/gpt-oss-20b" || model_id == "openai/gpt-oss-120b";
        let is_toggle_effort = model_id == "deepseek-ai/DeepSeek-V4-Pro-0813";
        if is_effort {
            let mut compat = base();
            compat.insert("supportsReasoningEffort".to_string(), Value::Bool(true));
            compat.insert(
                "thinkingFormat".to_string(),
                Value::String("openai".to_string()),
            );
            return compat;
        }
        if is_toggle_effort {
            let mut compat = base();
            compat.insert(
                "thinkingFormat".to_string(),
                Value::String("together".to_string()),
            );
            compat.insert("supportsReasoningEffort".to_string(), Value::Bool(true));
            return compat;
        }
        let mut compat = base();
        compat.insert(
            "thinkingFormat".to_string(),
            Value::String("together".to_string()),
        );
        compat
    }

    /// `processZaiModels(data)`。
    pub fn process_zai_models(data: &Value, state: &mut GeneratorState) -> Vec<Value> {
        let variants = [
            (
                "zai-coding-plan",
                "zai",
                "https://api.z.ai/api/coding/paas/v4",
            ),
            (
                "zhipuai-coding-plan",
                "zai-coding-cn",
                "https://open.bigmodel.cn/api/coding/paas/v4",
            ),
        ];
        let mut models: Vec<Value> = Vec::new();
        for (source, provider, base_url) in variants {
            for (model_id, model) in provider_models(data, source) {
                if !tool_capable(model) {
                    continue;
                }
                let supports_image = input_has_image(model);
                let mut thinking_level_map =
                    get_effort_thinking_level_map(&reasoning_options(model));
                let is_glm52 = model_id == "glm-5.2" || model_id == "glm-5.2-highspeed";
                if let (Some(map), true) = (thinking_level_map.as_mut(), is_glm52) {
                    map.insert(super::ModelThinkingLevel::Off, Some("none".to_string()));
                }
                let supports_reasoning_effort = thinking_level_map.is_some();
                // 两个变体分别来自 zai-coding-plan 与 zhipuai-coding-plan 两个分组；成本统一
                // 优先参考 data["zai"] 分组里同 id 的价目，取不到再退回条目自身的 cost。
                let reference_cost = data
                    .get("zai")
                    .and_then(|zai| zai.get("models"))
                    .and_then(|models| models.get(&model_id))
                    .and_then(|model| model.get("cost"))
                    .or_else(|| model.get("cost"));

                let mut built = build_chat_model(ChatModelSpec {
                    id: model_id.clone(),
                    name: model_name(model, &model_id),
                    api: "openai-completions",
                    provider,
                    base_url,
                    reasoning: model.get("reasoning").and_then(Value::as_bool) == Some(true),
                    input: input_modalities(supports_image),
                    cost: get_models_dev_cost(reference_cost),
                    context_window: limit_or(model, "context"),
                    max_tokens: limit_or(model, "output"),
                });
                if let Some(map) = &thinking_level_map {
                    set_thinking_level_map(&mut built, map);
                }
                let mut compat = Map::new();
                compat.insert("supportsDeveloperRole".to_string(), Value::Bool(false));
                compat.insert(
                    "thinkingFormat".to_string(),
                    Value::String("zai".to_string()),
                );
                if supports_reasoning_effort {
                    compat.insert("supportsReasoningEffort".to_string(), Value::Bool(true));
                }
                if !is_zai_tool_stream_unsupported(&model_id) {
                    compat.insert("zaiToolStream".to_string(), Value::Bool(true));
                }
                let merged = js_spread(&current_compat(&built), &compat);
                set_compat(&mut built, merged);
                reorder_model(&mut built, THINKING_COMPAT_ORDER);
                models.push(built);
                state.record(provider, &model_id, model);
            }
        }
        models
    }

    fn is_zai_tool_stream_unsupported(model_id: &str) -> bool {
        ["glm-4.5", "glm-4.5-air", "glm-4.5-flash", "glm-4.5v"].contains(&model_id)
    }

    /// `processBasetenModels(provider)`。
    pub fn process_baseten_models(provider: Option<&Value>) -> Vec<Value> {
        let Some(models_value) = provider.and_then(|provider| provider.get("models")) else {
            return Vec::new();
        };
        let Some(models_object) = models_value.as_object() else {
            return Vec::new();
        };
        let base_url = "https://inference.baseten.co/v1";
        let base_compat = || {
            let mut compat = Map::new();
            compat.insert("supportsStore".to_string(), Value::Bool(false));
            compat.insert("supportsDeveloperRole".to_string(), Value::Bool(false));
            compat.insert("supportsReasoningEffort".to_string(), Value::Bool(false));
            compat.insert("supportsUsageInStreaming".to_string(), Value::Bool(true));
            compat.insert(
                "maxTokensField".to_string(),
                Value::String("max_tokens".to_string()),
            );
            compat.insert("supportsStrictMode".to_string(), Value::Bool(true));
            compat.insert("sendSessionAffinityHeaders".to_string(), Value::Bool(true));
            compat.insert("supportsLongCacheRetention".to_string(), Value::Bool(false));
            compat
        };
        let reasoning_effort_compat = || {
            let mut compat = base_compat();
            compat.insert("supportsReasoningEffort".to_string(), Value::Bool(true));
            compat.insert(
                "thinkingFormat".to_string(),
                Value::String("openai".to_string()),
            );
            compat
        };
        let toggle_reasoning_compat = || {
            let mut compat = base_compat();
            compat.insert(
                "thinkingFormat".to_string(),
                Value::String("baseten".to_string()),
            );
            compat.insert(
                "chatTemplateArgs".to_string(),
                json!({ "enable_thinking": { "$var": "thinking.enabled" } }),
            );
            compat
        };
        let toggle_reasoning_effort_compat = || {
            let mut compat = reasoning_effort_compat();
            compat.insert(
                "thinkingFormat".to_string(),
                Value::String("baseten".to_string()),
            );
            compat.insert(
                "chatTemplateArgs".to_string(),
                json!({ "enable_thinking": { "$var": "thinking.enabled" } }),
            );
            compat
        };
        let toggle_thinking_level_map = || {
            level_map(&[
                ("off", Some("off")),
                ("minimal", None),
                ("low", None),
                ("medium", None),
                ("high", Some("high")),
                ("xhigh", None),
                ("max", None),
            ])
        };
        let glm52_thinking_level_map = || {
            level_map(&[
                ("off", Some("none")),
                ("minimal", None),
                ("low", None),
                ("medium", None),
                ("high", Some("high")),
                ("xhigh", None),
                ("max", Some("max")),
            ])
        };

        let mut models: Vec<Value> = Vec::new();
        for (model_id, model) in models_object {
            if is_deprecated(model) {
                continue;
            }
            let reasoning = model.get("reasoning").and_then(Value::as_bool) == Some(true);
            let options = reasoning_options(model);
            let is_glm52 = model_id == "zai-org/GLM-5.2" || model_id == "zai-org/GLM-5.2-Fast";
            let supports_toggle = has_toggle_option(model) || is_glm52;
            let supports_effort = has_effort_option(model) || is_glm52;
            // compat 由 reasoning_options 的组合决定：toggle+effort > 仅 toggle > 仅 effort >
            // 基础；`is_glm52` 两个标志都强制为 true（上游没给 options 也要按 effort 处理）。
            let compat = if supports_toggle && supports_effort {
                toggle_reasoning_effort_compat()
            } else if supports_toggle {
                toggle_reasoning_compat()
            } else if supports_effort {
                reasoning_effort_compat()
            } else {
                base_compat()
            };
            let thinking_level_map = if is_glm52 {
                Some(glm52_thinking_level_map())
            } else if supports_toggle {
                Some(toggle_thinking_level_map())
            } else {
                get_effort_thinking_level_map(&options)
            };
            let supports_image_input = !is_glm52 && input_has_image(model);

            let mut built = build_chat_model(ChatModelSpec {
                id: model_id.clone(),
                name: model_name(model, model_id),
                api: "openai-completions",
                provider: "baseten",
                base_url,
                reasoning,
                input: input_modalities(supports_image_input),
                cost: get_models_dev_cost(model.get("cost")),
                context_window: limit_or(model, "context"),
                max_tokens: limit_or(model, "output"),
            });
            if let Some(map) = &thinking_level_map {
                set_thinking_level_map(&mut built, map);
            }
            set_compat(&mut built, compat);
            reorder_model(&mut built, THINKING_COMPAT_ORDER);
            models.push(built);
        }
        models
    }

    /// `processGoogleModels(data)`。
    pub fn process_google_models(data: &Value) -> Vec<Value> {
        let mut models: Vec<Value> = Vec::new();
        let google_models = data.get("google").and_then(|google| google.get("models"));
        if let Some(google_models) = google_models
            && let Some(object) = google_models.as_object()
        {
            for (model_id, model) in object {
                if !tool_capable(model) {
                    continue;
                }
                let source = google_source(google_models, model_id, model);
                let thinking_level_map =
                    get_google_thinking_level_map(model_id, &reasoning_options(source));
                let mut built = build_chat_model(ChatModelSpec {
                    id: model_id.clone(),
                    name: model_name(model, model_id),
                    api: "google-generative-ai",
                    provider: "google",
                    base_url: "https://generativelanguage.googleapis.com/v1beta",
                    reasoning: source.get("reasoning").and_then(Value::as_bool) == Some(true),
                    input: input_modalities(input_has_image(source)),
                    cost: get_models_dev_cost(source.get("cost")),
                    context_window: limit_or(source, "context"),
                    max_tokens: limit_or(source, "output"),
                });
                if let Some(map) = thinking_level_map {
                    set_thinking_level_map(&mut built, &map);
                }
                reorder_model(&mut built, THINKING_EARLY_ORDER);
                models.push(built);
            }
        }

        let vertex_models = data
            .get("google-vertex")
            .and_then(|vertex| vertex.get("models"));
        if let Some(vertex_models) = vertex_models
            && let Some(object) = vertex_models.as_object()
        {
            for (model_id, model) in object {
                if !tool_capable(model) || !model_id.starts_with("gemini-") {
                    continue;
                }
                if model_id == "gemini-3.1-flash-lite-preview" {
                    continue;
                }
                let source = google_source(vertex_models, model_id, model);
                let thinking_level_map =
                    get_google_thinking_level_map(model_id, &reasoning_options(source));
                let cache_read = if model_id == "gemini-2.5-flash" {
                    // TS 对 `gemini-2.5-flash` 的 cacheRead 有硬编码覆盖；其余取上游值。
                    json!(0.03)
                } else {
                    js_or_value(nested(source, &["cost", "cache_read"]), json!(0))
                };
                let mut built = build_chat_model(ChatModelSpec {
                    id: model_id.clone(),
                    name: model_name(model, model_id),
                    api: "google-vertex",
                    provider: "google-vertex",
                    base_url: VERTEX_BASE_URL,
                    reasoning: source.get("reasoning").and_then(Value::as_bool) == Some(true),
                    input: input_modalities(input_has_image(source)),
                    cost: cost_object_values(
                        [
                            js_or_value(nested(source, &["cost", "input"]), json!(0)),
                            js_or_value(nested(source, &["cost", "output"]), json!(0)),
                            cache_read,
                            json!(0),
                        ],
                        None,
                    ),
                    context_window: limit_or(source, "context"),
                    max_tokens: limit_or(source, "output"),
                });
                if let Some(map) = thinking_level_map {
                    set_thinking_level_map(&mut built, &map);
                }
                reorder_model(&mut built, THINKING_EARLY_ORDER);
                models.push(built);
            }
        }
        models
    }

    /// `-latest` 别名优先取真实模型（`?? model`）。
    fn google_source<'a>(models: &'a Value, model_id: &str, model: &'a Value) -> &'a Value {
        let alias = match model_id {
            "gemini-flash-latest" => Some("gemini-3.5-flash"),
            "gemini-flash-lite-latest" => Some("gemini-3.1-flash-lite"),
            _ => None,
        };
        alias
            .and_then(|alias| models.get(alias))
            .filter(|value| !value.is_null())
            .unwrap_or(model)
    }

    /// `processFireworksModels(provider)`。
    pub fn process_fireworks_models(
        provider: Option<&Value>,
        state: &mut GeneratorState,
    ) -> Vec<Value> {
        let Some(models_value) = provider.and_then(|provider| provider.get("models")) else {
            return Vec::new();
        };
        let Some(models_object) = models_value.as_object() else {
            return Vec::new();
        };

        let anthropic_compat = || {
            let mut compat = Map::new();
            compat.insert("allowEmptySignature".to_string(), Value::Bool(true));
            compat.insert("sendSessionAffinityHeaders".to_string(), Value::Bool(true));
            compat.insert(
                "supportsEagerToolInputStreaming".to_string(),
                Value::Bool(false),
            );
            compat.insert(
                "supportsCacheControlOnTools".to_string(),
                Value::Bool(false),
            );
            compat.insert("supportsLongCacheRetention".to_string(), Value::Bool(false));
            compat
        };
        let openai_compat = || {
            let mut compat = Map::new();
            compat.insert("supportsStore".to_string(), Value::Bool(false));
            compat.insert("supportsDeveloperRole".to_string(), Value::Bool(false));
            compat.insert("sendSessionAffinityHeaders".to_string(), Value::Bool(true));
            compat.insert("supportsLongCacheRetention".to_string(), Value::Bool(false));
            compat
        };
        let kimi_k3_compat = || {
            let mut compat = openai_compat();
            compat.insert(
                "requiresReasoningContentOnAssistantMessages".to_string(),
                Value::Bool(true),
            );
            compat.insert(
                "thinkingFormat".to_string(),
                Value::String("openai".to_string()),
            );
            compat
        };

        let mut models: Vec<Value> = Vec::new();
        for (model_id, model) in models_object {
            if model.get("tool_call").and_then(Value::as_bool) != Some(true) {
                continue;
            }
            let input = input_modalities(input_has_image(model));
            let common = || ChatModelSpec {
                id: model_id.clone(),
                name: model_name(model, model_id),
                api: "",
                provider: "fireworks",
                base_url: "",
                reasoning: model.get("reasoning").and_then(Value::as_bool) == Some(true),
                input: input.clone(),
                cost: get_models_dev_cost(model.get("cost")),
                context_window: limit_or(model, "context"),
                max_tokens: limit_or(model, "output"),
            };

            // 模型族决定协议：`glm-*` 与 `kimi-k3` 走 openai-completions（compat 不同），
            // 其余走 anthropic-messages。
            let mut built = if model_id.contains("glm-") {
                let spec = ChatModelSpec {
                    api: "openai-completions",
                    base_url: "https://api.fireworks.ai/inference/v1",
                    ..common()
                };
                let mut built = build_chat_model(spec);
                set_compat(&mut built, openai_compat());
                reorder_model(&mut built, FIREWORKS_ORDER);
                built
            } else if model_id.contains("kimi-k3") {
                let spec = ChatModelSpec {
                    api: "openai-completions",
                    base_url: "https://api.fireworks.ai/inference/v1",
                    ..common()
                };
                let mut built = build_chat_model(spec);
                set_compat(&mut built, kimi_k3_compat());
                reorder_model(&mut built, FIREWORKS_ORDER);
                built
            } else {
                let spec = ChatModelSpec {
                    api: "anthropic-messages",
                    base_url: "https://api.fireworks.ai/inference",
                    ..common()
                };
                let mut built = build_chat_model(spec);
                let mut compat = anthropic_compat();
                if has_effort_option(model)
                    || FIREWORKS_ADAPTIVE_THINKING_FALLBACK_MODELS.contains(&model_id.as_str())
                {
                    compat.insert("forceAdaptiveThinking".to_string(), Value::Bool(true));
                }
                set_compat(&mut built, compat);
                reorder_model(&mut built, FIREWORKS_ORDER);
                built
            };
            let _ = &mut built;
            models.push(built);
            state.record("fireworks", model_id, model);
        }
        models
    }

    /// `createKimiCodingModels(models)`：把 models.dev 的 kimi-code-plan 条目转成
    /// `kimi-coding` provider 的模型；模型自身没给价格时用 `IMPLIED` 表回退。
    pub fn create_kimi_coding_models(models: &serde_json::Map<String, Value>) -> Vec<Value> {
        const IMPLIED: [(&str, [f64; 4]); 6] = [
            ("k3", KIMI_K3_COST),
            ("k3-256k", KIMI_K3_COST),
            ("k3.1-pro", [2.0, 10.0, 0.2, 0.0]),
            ("k3.1-pro-highspeed", [4.0, 20.0, 0.2, 0.0]),
            ("kimi-for-coding", [0.95, 4.0, 0.19, 0.0]),
            ("kimi-for-coding-highspeed", [1.9, 8.0, 0.38, 0.0]),
        ];
        let kimi_compat = || {
            let mut compat = Map::new();
            compat.insert("supportsStore".to_string(), Value::Bool(false));
            compat.insert("supportsDeveloperRole".to_string(), Value::Bool(false));
            compat.insert("supportsReasoningEffort".to_string(), Value::Bool(false));
            compat.insert(
                "maxTokensField".to_string(),
                Value::String("max_tokens".to_string()),
            );
            compat.insert("supportsStrictMode".to_string(), Value::Bool(false));
            compat.insert(
                "thinkingFormat".to_string(),
                Value::String("deepseek".to_string()),
            );
            compat
        };
        let kimi_k3_compat = || {
            let mut compat = kimi_compat();
            compat.insert(
                "requiresReasoningContentOnAssistantMessages".to_string(),
                Value::Bool(true),
            );
            compat.insert(
                "thinkingFormat".to_string(),
                Value::String("openai".to_string()),
            );
            compat.insert("supportsReasoningEffort".to_string(), Value::Bool(true));
            compat
        };

        let mut result: Vec<Value> = Vec::new();
        for (model_id, model) in models {
            if model.get("tool_call").and_then(Value::as_bool) != Some(true) {
                continue;
            }
            let is_k3_style = has_effort_option(model);
            // 未在 models.dev 给出价格的条目按 IMPLIED 表回退；模型自身的 cost 优先级更高。
            let implied_cost = IMPLIED
                .iter()
                .find(|(id, _)| id == model_id)
                .map(|(_, cost)| *cost);
            let effort_levels = if is_k3_style {
                get_effort_thinking_level_map(&reasoning_options(model))
            } else {
                None
            };

            let mut compat = if is_k3_style {
                kimi_k3_compat()
            } else {
                kimi_compat()
            };
            compat.insert(
                "supportsMidConvoSystemMessages".to_string(),
                Value::Bool(true),
            );
            if is_k3_style {
                compat.insert(
                    "supportsMidConvoToolAdditions".to_string(),
                    Value::Bool(true),
                );
            }

            let rate = |key: &str, fallback_index: usize| -> Value {
                let fallback = implied_cost
                    .map(|cost| json!(cost[fallback_index]))
                    .unwrap_or_else(|| json!(0));
                js_or_value(nested(model, &["cost", key]), fallback)
            };
            let mut built = build_chat_model(ChatModelSpec {
                id: model_id.clone(),
                name: model_name(model, model_id),
                api: "openai-completions",
                provider: "kimi-coding",
                base_url: "https://api.kimi.com/coding/v1",
                reasoning: model.get("reasoning").and_then(Value::as_bool) == Some(true),
                input: input_modalities(input_has_image(model)),
                cost: cost_object_values(
                    [
                        rate("input", 0),
                        rate("output", 1),
                        rate("cache_read", 2),
                        rate("cache_write", 3),
                    ],
                    None,
                ),
                context_window: limit_or(model, "context"),
                max_tokens: limit_or(model, "output"),
            });
            set_compat(&mut built, compat);
            // `{ ...effortLevels, off: null }`
            let mut map = effort_levels.unwrap_or_default();
            map.insert(super::ModelThinkingLevel::Off, None);
            set_thinking_level_map(&mut built, &map);
            reorder_model(&mut built, KIMI_CODING_ORDER);
            result.push(built);
        }
        result
    }

    // ------------------------------------------------------------------
    // loadModelsDevData
    // ------------------------------------------------------------------

    const BEDROCK_INFERENCE_PROFILE_ONLY_MODEL_IDS: [&str; 1] = ["anthropic.claude-opus-5"];
    const MODELS_DEV_OPENAI_UNSUPPORTED_MODEL_IDS: [&str; 1] = ["gpt-5.6"];
    const XAI_RESPONSES_COMPAT_LONG_CACHE_RETENTION: bool = false;
    const NVIDIA_NIM_UNSUPPORTED_MODELS: [&str; 18] = [
        "abacusai/dracarys-llama-3.1-70b-instruct",
        "bytedance/seed-oss-36b-instruct",
        "deepseek-ai/deepseek-v4-flash",
        "deepseek-ai/deepseek-v4-pro",
        "google/gemma-2-2b-it",
        "google/gemma-3n-e2b-it",
        "google/gemma-3n-e4b-it",
        "google/gemma-4-31b-it",
        "meta/llama-3.2-1b-instruct",
        "meta/llama-4-maverick-17b-128e-instruct",
        "microsoft/phi-4-mini-instruct",
        "minimaxai/minimax-m2.7",
        "mistralai/mistral-nemotron",
        "nvidia/nemotron-mini-4b-instruct",
        "qwen/qwen3-next-80b-a3b-instruct",
        "qwen/qwen3.5-397b-a17b",
        "sarvamai/sarvam-m",
        "upstage/solar-10.7b-instruct",
    ];
    const OPENCODE_OPENAI_COMPLETIONS_LONG_CACHE_RETENTION_UNSUPPORTED_MODELS: [&str; 6] = [
        "opencode:deepseek-v4-flash",
        "opencode:deepseek-v4-pro",
        "opencode:kimi-k2.5",
        "opencode:kimi-k2.6",
        "opencode:minimax-m2.7",
        "opencode-go:kimi-k2.6",
    ];
    const QWEN_TOKEN_PLAN_REASONING_EFFORT_FALLBACK_MODEL_IDS: [&str; 2] = ["glm-5", "glm-5.1"];
    const QWEN_TOKEN_PLAN_EXCLUDED_MODEL_IDS: [&str; 1] = ["qwen3.8-max-preview"];
    const QWEN_TOKEN_PLAN_PROVIDER_IDS: [&str; 3] = [
        "qwen-token-plan",
        "qwen-token-plan-cn",
        "qwen-token-plan-individual",
    ];
    const QWEN_TOKEN_PLAN_INDIVIDUAL_MODEL_IDS: [&str; 9] = [
        "deepseek-v4-flash-0731",
        "deepseek-v4-pro",
        "deepseek-v4-pro-0813",
        "glm-5.2",
        "qwen3.6-flash",
        "qwen3.7-max",
        "qwen3.7-plus",
        "qwen3.8-flash",
        "qwen3.8-max",
    ];
    const MINIMAX_HIDDEN_IDS: [&str; 4] = [
        "MiniMax-M2",
        "MiniMax-M2.1",
        "MiniMax-M2.5",
        "MiniMax-M2.5-highspeed",
    ];

    /// `loadModelsDevData()` 的 strict 门禁需要的上下文。
    pub struct LoadOptions<'a> {
        /// 打开时 NVIDIA 取回失败、qwen-token-plan-individual 白名单不匹配都会直接返回错误。
        pub strict: bool,
        /// 预留的 NVIDIA 响应；当前实现读取的是 [`GeneratorInputs::nvidia_nim`]，本字段未被使用。
        pub nvidia_nim: Option<&'a Value>,
    }

    /// `loadModelsDevData()`：把 models.dev 的目录展开成模型列表。
    ///
    /// `strict` 打开时 `qwen-token-plan-individual` 的白名单不匹配会返回错误
    /// （`assertExactModelIds`）。TS 的异常路径（`fetch` 失败）在 Rust 侧由 [`GeneratorInputs`]
    /// 的 `None` 表达：`models_dev` 缺失即返回空列表。
    pub fn load_models_dev_data(
        inputs: &GeneratorInputs,
        options: &LoadOptions<'_>,
        state: &mut GeneratorState,
    ) -> Result<Vec<Value>, CatalogError> {
        let Some(data) = inputs.models_dev.as_ref() else {
            return Ok(Vec::new());
        };
        if data.is_null() {
            return Err(CatalogError::new(
                "TypeError: Cannot read properties of null (reading 'nvidia')",
            ));
        }
        // 下面按 TS 的 provider 顺序展开：每个 provider 先按 tool_call/reasoning 等字段过滤，
        // 再用 limit/cost 原值构造模型，并把 reasoning_options 记入 state 供后续 metadata 使用。
        let mut models: Vec<Value> = Vec::new();
        // `data.nvidia?.models ? await fetchNvidiaNimModelIds() : new Map()`。
        let nvidia_nim_ids = if data
            .get("nvidia")
            .and_then(|nvidia| nvidia.get("models"))
            .map(js_truthy)
            .unwrap_or(false)
        {
            state
                .stdout_lines
                .push("Fetching models from NVIDIA NIM API...".to_string());
            match inputs.nvidia_nim.as_ref() {
                Some(response) => {
                    let count = response
                        .get("data")
                        .and_then(Value::as_array)
                        .map(|items| items.len())
                        .unwrap_or(0);
                    state
                        .stdout_lines
                        .push(format!("Fetched {count} model IDs from NVIDIA NIM"));
                    nvidia_nim_model_ids(Some(response), state)
                }
                None => {
                    let message = inputs.fetch_error(SOURCE_NVIDIA_NIM);
                    state
                        .stderr_lines
                        .push(format!("Failed to fetch NVIDIA NIM models: {message}"));
                    if options.strict {
                        return Err(CatalogError::new(message));
                    }
                    IndexMap::new()
                }
            }
        } else {
            IndexMap::new()
        };
        let _ = &nvidia_nim_ids;
        let _ = options;

        // Amazon Bedrock
        for (model_id, model) in provider_models(data, "amazon-bedrock") {
            if !tool_capable(model)
                || BEDROCK_INFERENCE_PROFILE_ONLY_MODEL_IDS.contains(&model_id.as_str())
            {
                continue;
            }
            if model_id.starts_with("ai21.jamba")
                || model_id.starts_with("mistral.mistral-7b-instruct-v0")
            {
                continue;
            }
            let mut built = build_chat_model(ChatModelSpec {
                id: model_id.clone(),
                name: model_name(model, &model_id),
                api: "bedrock-converse-stream",
                provider: "amazon-bedrock",
                base_url: get_bedrock_base_url(&model_id),
                reasoning: model.get("reasoning").and_then(Value::as_bool) == Some(true),
                input: input_modalities(input_has_image(model)),
                cost: get_models_dev_cost(model.get("cost")),
                context_window: limit_or(model, "context"),
                max_tokens: limit_or(model, "output"),
            });
            if model.get("structured_output").and_then(Value::as_bool) == Some(true) {
                let mut compat = Map::new();
                compat.insert("supportsStrictMode".to_string(), Value::Bool(true));
                set_compat(&mut built, compat);
            }
            models.push(built);
            state.record("amazon-bedrock", &model_id, model);
        }

        // Anthropic
        for (model_id, model) in provider_models(data, "anthropic") {
            if !tool_capable(model) {
                continue;
            }
            models.push(build_chat_model(ChatModelSpec {
                id: model_id.clone(),
                name: model_name(model, &model_id),
                api: "anthropic-messages",
                provider: "anthropic",
                base_url: "https://api.anthropic.com",
                reasoning: model.get("reasoning").and_then(Value::as_bool) == Some(true),
                input: input_modalities(input_has_image(model)),
                cost: get_models_dev_cost(model.get("cost")),
                context_window: limit_or(model, "context"),
                max_tokens: limit_or(model, "output"),
            }));
            state.record("anthropic", &model_id, model);
        }

        models.extend(process_google_models(data));

        // OpenAI
        for (model_id, model) in provider_models(data, "openai") {
            if !tool_capable(model)
                || MODELS_DEV_OPENAI_UNSUPPORTED_MODEL_IDS.contains(&model_id.as_str())
            {
                continue;
            }
            models.push(build_chat_model(ChatModelSpec {
                id: model_id.clone(),
                name: model_name(model, &model_id),
                api: "openai-responses",
                provider: "openai",
                base_url: "https://api.openai.com/v1",
                reasoning: model.get("reasoning").and_then(Value::as_bool) == Some(true),
                input: input_modalities(input_has_image(model)),
                cost: get_models_dev_cost(model.get("cost")),
                context_window: limit_or(model, "context"),
                max_tokens: limit_or(model, "output"),
            }));
            state.record("openai", &model_id, model);
        }

        // Groq / Cerebras
        for (provider, base_url) in [
            ("groq", "https://api.groq.com/openai/v1"),
            ("cerebras", "https://api.cerebras.ai/v1"),
        ] {
            for (model_id, model) in provider_models(data, provider) {
                if !tool_capable(model) {
                    continue;
                }
                models.push(build_chat_model(ChatModelSpec {
                    id: model_id.clone(),
                    name: model_name(model, &model_id),
                    api: "openai-completions",
                    provider,
                    base_url,
                    reasoning: model.get("reasoning").and_then(Value::as_bool) == Some(true),
                    input: input_modalities(input_has_image(model)),
                    cost: get_models_dev_cost(model.get("cost")),
                    context_window: limit_or(model, "context"),
                    max_tokens: limit_or(model, "output"),
                }));
                state.record(provider, &model_id, model);
            }
        }

        // Cloudflare Workers AI
        for (model_id, model) in provider_models(data, "cloudflare-workers-ai") {
            if !tool_capable(model) {
                continue;
            }
            let mut built = build_chat_model(ChatModelSpec {
                id: model_id.clone(),
                name: model_name(model, &model_id),
                api: "openai-completions",
                provider: "cloudflare-workers-ai",
                base_url: CLOUDFLARE_WORKERS_AI_BASE_URL,
                reasoning: model.get("reasoning").and_then(Value::as_bool) == Some(true),
                input: input_modalities(input_has_image(model)),
                cost: get_models_dev_cost(model.get("cost")),
                context_window: limit_or(model, "context"),
                max_tokens: limit_or(model, "output"),
            });
            let mut compat = Map::new();
            compat.insert("sendSessionAffinityHeaders".to_string(), Value::Bool(true));
            set_compat(&mut built, compat);
            models.push(built);
            state.record("cloudflare-workers-ai", &model_id, model);
        }

        // Cloudflare AI Gateway（含 workers-ai 前缀镜像）
        let mut cloudflare_ai_gateway_model_ids: BTreeSet<String> = BTreeSet::new();
        for (prefixed_id, model) in provider_models(data, "cloudflare-ai-gateway") {
            if !tool_capable(model) {
                continue;
            }
            // 键是 `"<upstream>/<model>"`：openai/anthropic 去掉前缀后转发，workers-ai 保留
            // 完整键；其他上游不在网关白名单内，直接跳过。
            let Some(slash) = prefixed_id.find('/') else {
                continue;
            };
            let upstream = &prefixed_id[..slash];
            let native_id = &prefixed_id[slash + 1..];
            let (api, base_url, id): (&str, &str, String) = if upstream == "openai" {
                (
                    "openai-responses",
                    CLOUDFLARE_AI_GATEWAY_OPENAI_BASE_URL,
                    native_id.to_string(),
                )
            } else if upstream == "anthropic" {
                (
                    "anthropic-messages",
                    CLOUDFLARE_AI_GATEWAY_ANTHROPIC_BASE_URL,
                    native_id.replace('.', "-"),
                )
            } else if upstream == "workers-ai" {
                (
                    "openai-completions",
                    CLOUDFLARE_AI_GATEWAY_COMPAT_BASE_URL,
                    prefixed_id.clone(),
                )
            } else {
                continue;
            };
            let compat = if upstream == "anthropic" || upstream == "workers-ai" {
                let mut compat = Map::new();
                compat.insert("sendSessionAffinityHeaders".to_string(), Value::Bool(true));
                Some(compat)
            } else {
                None
            };
            cloudflare_ai_gateway_model_ids.insert(id.clone());
            let mut built = build_chat_model(ChatModelSpec {
                id: id.clone(),
                name: model_name(model, &id),
                api,
                provider: "cloudflare-ai-gateway",
                base_url,
                reasoning: model.get("reasoning").and_then(Value::as_bool) == Some(true),
                input: input_modalities(input_has_image(model)),
                cost: get_models_dev_cost(model.get("cost")),
                context_window: limit_or(model, "context"),
                max_tokens: limit_or(model, "output"),
            });
            if let Some(compat) = compat {
                set_compat(&mut built, compat);
            }
            models.push(built);
            state.record("cloudflare-ai-gateway", &id, model);
        }
        if let Some(workers_models) = data
            .get("cloudflare-workers-ai")
            .and_then(|provider| provider.get("models"))
            .and_then(Value::as_object)
        {
            for (model_id, model) in workers_models {
                if !tool_capable(model) {
                    continue;
                }
                let id = format!("workers-ai/{model_id}");
                if cloudflare_ai_gateway_model_ids.contains(&id) {
                    continue;
                }
                cloudflare_ai_gateway_model_ids.insert(id.clone());
                let mut built = build_chat_model(ChatModelSpec {
                    id: id.clone(),
                    name: model_name(model, &id),
                    api: "openai-completions",
                    provider: "cloudflare-ai-gateway",
                    base_url: CLOUDFLARE_AI_GATEWAY_COMPAT_BASE_URL,
                    reasoning: model.get("reasoning").and_then(Value::as_bool) == Some(true),
                    input: input_modalities(input_has_image(model)),
                    cost: get_models_dev_cost(model.get("cost")),
                    context_window: limit_or(model, "context"),
                    max_tokens: limit_or(model, "output"),
                });
                let mut compat = Map::new();
                compat.insert("sendSessionAffinityHeaders".to_string(), Value::Bool(true));
                set_compat(&mut built, compat);
                models.push(built);
                state.record("cloudflare-ai-gateway", &id, model);
            }
        }

        // xAI
        for (model_id, model) in provider_models(data, "xai") {
            if !tool_capable(model) {
                continue;
            }
            let mut built = build_chat_model(ChatModelSpec {
                id: model_id.clone(),
                name: model_name(model, &model_id),
                api: "openai-responses",
                provider: "xai",
                base_url: "https://api.x.ai/v1",
                reasoning: model.get("reasoning").and_then(Value::as_bool) == Some(true),
                input: input_modalities(input_has_image(model)),
                cost: get_models_dev_cost(model.get("cost")),
                context_window: limit_or(model, "context"),
                max_tokens: limit_or(model, "output"),
            });
            let mut compat = Map::new();
            compat.insert(
                "supportsLongCacheRetention".to_string(),
                Value::Bool(XAI_RESPONSES_COMPAT_LONG_CACHE_RETENTION),
            );
            set_compat(&mut built, compat);
            reorder_model(&mut built, COMPAT_EARLY_ORDER);
            models.push(built);
            state.record("xai", &model_id, model);
        }

        // Meta
        for (model_id, model) in provider_models(data, "meta") {
            if !tool_capable(model) {
                continue;
            }
            models.push(build_chat_model(ChatModelSpec {
                id: model_id.clone(),
                name: model_name(model, &model_id),
                api: "openai-responses",
                provider: "meta",
                base_url: "https://api.meta.ai/v1",
                reasoning: model.get("reasoning").and_then(Value::as_bool) == Some(true),
                input: input_modalities(input_has_image(model)),
                cost: get_models_dev_cost(model.get("cost")),
                context_window: limit_or(model, "context"),
                max_tokens: limit_or(model, "output"),
            }));
            state.record("meta", &model_id, model);
        }

        models.extend(process_zai_models(data, state));

        // Mistral
        for (model_id, model) in provider_models(data, "mistral") {
            if !tool_capable(model) {
                continue;
            }
            let thinking_level_map = get_effort_thinking_level_map(&reasoning_options(model));
            let base_input = js_or_value(nested(model, &["cost", "input"]), json!(0));
            // `m.cost?.cache_read ?? (m.cost?.input ? roundCost(m.cost.input * 0.1) : 0)`
            let cache_read = match nested(model, &["cost", "cache_read"]) {
                Some(value) => value.clone(),
                None => {
                    if js_truthy(&base_input) {
                        json!(round_cost(js_to_number(&base_input) * 0.1))
                    } else {
                        json!(0)
                    }
                }
            };
            let mut built = build_chat_model(ChatModelSpec {
                id: model_id.clone(),
                name: model_name(model, &model_id),
                api: "mistral-conversations",
                provider: "mistral",
                base_url: "https://api.mistral.ai",
                reasoning: model.get("reasoning").and_then(Value::as_bool) == Some(true),
                input: input_modalities(input_has_image(model)),
                cost: cost_object_values(
                    [
                        base_input,
                        js_or_value(nested(model, &["cost", "output"]), json!(0)),
                        cache_read,
                        js_or_value(nested(model, &["cost", "cache_write"]), json!(0)),
                    ],
                    None,
                ),
                context_window: limit_or(model, "context"),
                max_tokens: limit_or(model, "output"),
            });
            if let Some(map) = &thinking_level_map {
                set_thinking_level_map(&mut built, map);
            }
            reorder_model(&mut built, THINKING_EARLY_ORDER);
            models.push(built);
        }

        // Hugging Face
        for (model_id, model) in provider_models(data, "huggingface") {
            if !tool_capable(model) {
                continue;
            }
            let mut built = build_chat_model(ChatModelSpec {
                id: model_id.clone(),
                name: model_name(model, &model_id),
                api: "openai-completions",
                provider: "huggingface",
                base_url: "https://router.huggingface.co/v1",
                reasoning: model.get("reasoning").and_then(Value::as_bool) == Some(true),
                input: input_modalities(input_has_image(model)),
                cost: get_models_dev_cost(model.get("cost")),
                context_window: limit_or(model, "context"),
                max_tokens: limit_or(model, "output"),
            });
            let mut compat = Map::new();
            compat.insert("supportsDeveloperRole".to_string(), Value::Bool(false));
            set_compat(&mut built, compat);
            reorder_model(&mut built, COMPAT_AFTER_COST_ORDER);
            models.push(built);
            state.record("huggingface", &model_id, model);
        }

        models.extend(process_fireworks_models(data.get("fireworks-ai"), state));

        // NVIDIA NIM
        for (model_id, model) in provider_models(data, "nvidia") {
            if !tool_capable(model)
                || !input_includes(model, "text")
                || !output_includes(model, "text")
            {
                continue;
            }
            // 只有 NVIDIA 实时列表里存在的模型才生成：先按原 id 查，再按「小写 + 下划线换点」
            // 的归一化 id 查（models.dev 与 NVIDIA API 的命名差一个大小写/分隔符）。
            let live_model_id = nvidia_nim_ids
                .get(&model_id)
                .or_else(|| nvidia_nim_ids.get(&normalize_nvidia_model_id(&model_id)));
            let Some(live_model_id) = live_model_id else {
                continue;
            };
            if NVIDIA_NIM_UNSUPPORTED_MODELS.contains(&live_model_id.as_str()) {
                continue;
            }
            let mut built = build_chat_model(ChatModelSpec {
                id: live_model_id.clone(),
                name: match str_at(model, "name") {
                    Some(name) if !name.is_empty() => name.to_string(),
                    _ => live_model_id.clone(),
                },
                api: "openai-completions",
                provider: "nvidia",
                base_url: NVIDIA_BASE_URL,
                reasoning: model.get("reasoning").and_then(Value::as_bool) == Some(true),
                input: input_modalities(input_has_image(model)),
                cost: get_models_dev_cost(model.get("cost")),
                context_window: limit_or(model, "context"),
                max_tokens: limit_or(model, "output"),
            });
            let mut headers = Map::new();
            headers.insert(
                "NVCF-POLL-SECONDS".to_string(),
                Value::String("3600".to_string()),
            );
            if let Some(object) = built.as_object_mut() {
                object.insert("headers".to_string(), Value::Object(headers));
            }
            let mut compat = Map::new();
            compat.insert("supportsStore".to_string(), Value::Bool(false));
            compat.insert("supportsDeveloperRole".to_string(), Value::Bool(false));
            compat.insert("supportsReasoningEffort".to_string(), Value::Bool(false));
            compat.insert(
                "maxTokensField".to_string(),
                Value::String("max_tokens".to_string()),
            );
            compat.insert("supportsStrictMode".to_string(), Value::Bool(false));
            compat.insert("supportsLongCacheRetention".to_string(), Value::Bool(false));
            set_compat(&mut built, compat);
            reorder_model(&mut built, HEADERS_EARLY_ORDER);
            models.push(built);
            state.record("nvidia", live_model_id, model);
        }

        // Together AI
        let together_provider = data
            .get("together")
            .filter(|value| !value.is_null())
            .or_else(|| data.get("togetherai").filter(|value| !value.is_null()))
            .or_else(|| data.get("together-ai").filter(|value| !value.is_null()));
        if let Some(together_provider) = together_provider
            && let Some(together_models) =
                together_provider.get("models").and_then(Value::as_object)
        {
            for (model_id, model) in together_models {
                if !tool_capable(model) || is_deprecated(model) {
                    continue;
                }
                let reasoning = model.get("reasoning").and_then(Value::as_bool) == Some(true);
                let thinking_level_map = get_together_thinking_level_map(model_id, reasoning);
                let mut built = build_chat_model(ChatModelSpec {
                    id: model_id.clone(),
                    name: model_name(model, model_id),
                    api: "openai-completions",
                    provider: "together",
                    base_url: TOGETHER_BASE_URL,
                    reasoning,
                    input: input_modalities(input_has_image(model)),
                    cost: get_models_dev_cost(model.get("cost")),
                    context_window: limit_or(model, "context"),
                    max_tokens: limit_or(model, "output"),
                });
                if let Some(map) = &thinking_level_map {
                    set_thinking_level_map(&mut built, map);
                }
                set_compat(&mut built, get_together_compat(model_id, reasoning));
                reorder_model(&mut built, THINKING_COMPAT_ORDER);
                models.push(built);
                state.record("together", model_id, model);
            }
        }

        models.extend(process_baseten_models(data.get("baseten")));

        // OpenCode Zen / Go
        let opencode_variants = [
            ("opencode", "opencode", "https://opencode.ai/inference"),
            (
                "opencode-go",
                "opencode-go",
                "https://opencode.ai/inference/go",
            ),
        ];
        for (key, provider, base_path) in opencode_variants {
            for (model_id, model) in provider_models(data, key) {
                if !tool_capable(model) || is_deprecated(model) {
                    continue;
                }
                let npm = nested(model, &["provider", "npm"]).and_then(Value::as_str);
                let mut api: &str;
                let mut base_url: String;
                let mut compat: Map<String, Value> = Map::new();

                if npm == Some("@ai-sdk/openai") {
                    api = "openai-responses";
                    base_url = format!("{base_path}/v1");
                    compat.insert(
                        "sessionAffinityFormat".to_string(),
                        Value::String("openai-nosession".to_string()),
                    );
                } else if npm == Some("@ai-sdk/anthropic") {
                    api = "anthropic-messages";
                    base_url = base_path.to_string();
                } else if npm == Some("@ai-sdk/google") {
                    api = "google-generative-ai";
                    base_url = format!("{base_path}/v1");
                } else if npm == Some("@ai-sdk/alibaba") {
                    api = "openai-completions";
                    base_url = format!("{base_path}/v1");
                    compat.insert(
                        "cacheControlFormat".to_string(),
                        Value::String("anthropic".to_string()),
                    );
                } else {
                    api = "openai-completions";
                    base_url = format!("{base_path}/v1");
                }

                if provider == "opencode" && model_id == "grok-build-0.1" {
                    compat.insert("supportsReasoningEffort".to_string(), Value::Bool(false));
                }
                if (provider == "opencode" || provider == "opencode-go") && model_id == "kimi-k2.6"
                {
                    compat.insert(
                        "thinkingFormat".to_string(),
                        Value::String("deepseek".to_string()),
                    );
                    compat.insert("supportsReasoningEffort".to_string(), Value::Bool(false));
                }
                if provider == "opencode-go"
                    && (model_id == "qwen3.5-plus" || model_id == "qwen3.6-plus")
                {
                    api = "openai-completions";
                    base_url = format!("{base_path}/v1");
                    compat.insert(
                        "thinkingFormat".to_string(),
                        Value::String("qwen".to_string()),
                    );
                }
                if api == "openai-completions" {
                    compat.insert(
                        "maxTokensField".to_string(),
                        Value::String("max_tokens".to_string()),
                    );
                    if OPENCODE_OPENAI_COMPLETIONS_LONG_CACHE_RETENTION_UNSUPPORTED_MODELS
                        .contains(&format!("{provider}:{model_id}").as_str())
                    {
                        compat.insert("supportsLongCacheRetention".to_string(), Value::Bool(false));
                    }
                }

                let thinking_level_map: Option<super::ThinkingLevelMap> =
                    if api == "google-generative-ai" {
                        get_google_thinking_level_map(&model_id, &reasoning_options(model))
                    } else if provider == "opencode-go" && model_id == "deepseek-v4.1-flash" {
                        get_effort_thinking_level_map(&reasoning_options(model))
                    } else {
                        None
                    };

                let mut built = build_chat_model(ChatModelSpec {
                    id: model_id.clone(),
                    name: model_name(model, &model_id),
                    api,
                    provider,
                    base_url: &base_url,
                    reasoning: model.get("reasoning").and_then(Value::as_bool) == Some(true),
                    input: input_modalities(input_has_image(model)),
                    cost: get_models_dev_cost(model.get("cost")),
                    context_window: limit_or(model, "context"),
                    max_tokens: limit_or(model, "output"),
                });
                if let Some(map) = &thinking_level_map {
                    set_thinking_level_map(&mut built, map);
                }
                if !compat.is_empty() {
                    set_compat(&mut built, compat);
                }
                // TS 字面量：`reasoning, ...[thinkingLevelMap], input, cost, ...[compat], contextWindow, maxTokens`。
                // anthropic-messages 分支的字面量没有 compat（由 metadata 追加），走默认顺序。
                let order = if api == "anthropic-messages" {
                    DEFAULT_CHAT_ORDER
                } else if thinking_level_map.is_some() {
                    OPENCODE_ORDER
                } else {
                    COMPAT_AFTER_COST_ORDER
                };
                reorder_model(&mut built, order);
                models.push(built);
                state.record(provider, &model_id, model);
            }
        }

        // GitHub Copilot
        for (model_id, model) in provider_models(data, "github-copilot") {
            if !tool_capable(model) || is_deprecated(model) {
                continue;
            }
            let is_copilot_claude = is_copilot_claude_id(&model_id);
            let needs_responses_api = model_id.starts_with("gpt-")
                || model_id.starts_with("grok-")
                || model_id.starts_with("oswe")
                || model_id.starts_with("mai-");
            let api = if is_copilot_claude {
                "anthropic-messages"
            } else if needs_responses_api {
                "openai-responses"
            } else {
                "openai-completions"
            };
            let anthropic_compat = if api == "anthropic-messages" {
                Some(get_anthropic_messages_compat("github-copilot", &model_id))
            } else {
                None
            };

            let mut built = build_chat_model(ChatModelSpec {
                id: model_id.clone(),
                name: model_name(model, &model_id),
                api,
                provider: "github-copilot",
                base_url: "https://api.individual.githubcopilot.com",
                reasoning: model.get("reasoning").and_then(Value::as_bool) == Some(true),
                input: input_modalities(input_has_image(model)),
                cost: get_models_dev_cost(model.get("cost")),
                context_window: limit_or_default(model, "context", 128_000.0),
                max_tokens: limit_or_default(model, "output", 8192.0),
            });
            {
                let object = built.as_object_mut().expect("built from an object");
                let mut headers = Map::new();
                for (key, value) in COPILOT_STATIC_HEADERS {
                    headers.insert(key.to_string(), Value::String(value.to_string()));
                }
                object.insert("headers".to_string(), Value::Object(headers));
            }
            if let Some(compat) = anthropic_compat
                && !compat.is_empty()
            {
                set_compat(&mut built, compat);
            }
            if api == "openai-completions" {
                let mut compat = Map::new();
                compat.insert("supportsStore".to_string(), Value::Bool(false));
                compat.insert("supportsDeveloperRole".to_string(), Value::Bool(false));
                compat.insert("supportsReasoningEffort".to_string(), Value::Bool(false));
                set_compat(&mut built, compat);
            }
            // TS 字面量把 `headers` 写在 `maxTokens` 之后、metadata 之前。
            reorder_model(&mut built, DEFAULT_CHAT_ORDER);
            models.push(built);
            state.record("github-copilot", &model_id, model);
        }

        // MiniMax
        for (key, provider, base_url) in [
            ("minimax", "minimax", "https://api.minimax.io/anthropic"),
            (
                "minimax-cn",
                "minimax-cn",
                "https://api.minimaxi.com/anthropic",
            ),
        ] {
            for (model_id, model) in provider_models(data, key) {
                if !tool_capable(model) || MINIMAX_HIDDEN_IDS.contains(&model_id.as_str()) {
                    continue;
                }
                let thinking_level_map = get_effort_thinking_level_map(&reasoning_options(model));
                let mut built = build_chat_model(ChatModelSpec {
                    id: model_id.clone(),
                    name: model_name(model, &model_id),
                    api: "anthropic-messages",
                    provider,
                    base_url,
                    reasoning: model.get("reasoning").and_then(Value::as_bool) == Some(true),
                    input: input_modalities(input_has_image(model)),
                    cost: get_models_dev_cost(model.get("cost")),
                    context_window: limit_or(model, "context"),
                    max_tokens: limit_or(model, "output"),
                });
                if let Some(map) = &thinking_level_map {
                    set_thinking_level_map(&mut built, map);
                    let mut compat = Map::new();
                    compat.insert("forceAdaptiveThinking".to_string(), Value::Bool(true));
                    set_compat(&mut built, compat);
                }
                models.push(built);
            }
        }

        // Kimi For Coding
        if let Some(kimi_models) = data
            .get("kimi-code-plan-global")
            .and_then(|provider| provider.get("models"))
            .and_then(Value::as_object)
        {
            models.extend(create_kimi_coding_models(kimi_models));
        }

        // Moonshot AI
        for (key, provider, base_url) in [
            ("moonshotai", "moonshotai", "https://api.moonshot.ai/v1"),
            (
                "moonshotai-cn",
                "moonshotai-cn",
                "https://api.moonshot.cn/v1",
            ),
        ] {
            for (model_id, model) in provider_models(data, key) {
                if !tool_capable(model) {
                    continue;
                }
                let is_kimi_k3 = is_kimi_k3_family_id(&model_id);
                let mut compat = if is_kimi_k3 {
                    let mut compat = kimi_openai_compat();
                    compat.insert(
                        "requiresReasoningContentOnAssistantMessages".to_string(),
                        Value::Bool(true),
                    );
                    compat.insert(
                        "thinkingFormat".to_string(),
                        Value::String("openai".to_string()),
                    );
                    compat.insert("supportsReasoningEffort".to_string(), Value::Bool(true));
                    compat
                } else {
                    kimi_openai_compat()
                };
                let cost = if model_id == "kimi-k3" {
                    cost_object(KIMI_K3_COST, None)
                } else {
                    cost_object_values(
                        [
                            js_or_value(nested(model, &["cost", "input"]), json!(0)),
                            js_or_value(nested(model, &["cost", "output"]), json!(0)),
                            js_or_value(nested(model, &["cost", "cache_read"]), json!(0)),
                            if is_kimi_k3 {
                                json!(0)
                            } else {
                                js_or_value(nested(model, &["cost", "cache_write"]), json!(0))
                            },
                        ],
                        None,
                    )
                };
                let mut built = build_chat_model(ChatModelSpec {
                    id: model_id.clone(),
                    name: model_name(model, &model_id),
                    api: "openai-completions",
                    provider,
                    base_url,
                    reasoning: is_kimi_k3
                        || model.get("reasoning").and_then(Value::as_bool) == Some(true),
                    input: input_modalities(input_has_image(model)),
                    cost,
                    context_window: limit_or(model, "context"),
                    max_tokens: limit_or(model, "output"),
                });
                compat.insert(
                    "maxTokensField".to_string(),
                    Value::String("max_tokens".to_string()),
                );
                set_compat(&mut built, compat);
                models.push(built);
                state.record(provider, &model_id, model);
            }
        }

        fn kimi_openai_compat() -> Map<String, Value> {
            let mut compat = Map::new();
            compat.insert("supportsStore".to_string(), Value::Bool(false));
            compat.insert("supportsDeveloperRole".to_string(), Value::Bool(false));
            compat.insert("supportsReasoningEffort".to_string(), Value::Bool(false));
            compat.insert(
                "maxTokensField".to_string(),
                Value::String("max_tokens".to_string()),
            );
            compat.insert("supportsStrictMode".to_string(), Value::Bool(false));
            compat.insert(
                "thinkingFormat".to_string(),
                Value::String("deepseek".to_string()),
            );
            compat
        }

        // Xiaomi MiMo
        let xiaomi_compat = || {
            let mut compat = Map::new();
            compat.insert("supportsReasoningEffort".to_string(), Value::Bool(false));
            compat.insert(
                "requiresReasoningContentOnAssistantMessages".to_string(),
                Value::Bool(true),
            );
            compat.insert(
                "thinkingFormat".to_string(),
                Value::String("deepseek".to_string()),
            );
            compat
        };
        for (source, provider, base_url) in [
            ("xiaomi", "xiaomi", "https://api.xiaomimimo.com/v1"),
            (
                "xiaomi-token-plan-cn",
                "xiaomi-token-plan-cn",
                "https://token-plan-cn.xiaomimimo.com/v1",
            ),
            (
                "xiaomi-token-plan-ams",
                "xiaomi-token-plan-ams",
                "https://token-plan-ams.xiaomimimo.com/v1",
            ),
            (
                "xiaomi-token-plan-sgp",
                "xiaomi-token-plan-sgp",
                "https://token-plan-sgp.xiaomimimo.com/v1",
            ),
        ] {
            for (model_id, model) in provider_models(data, source) {
                if !tool_capable(model) || is_deprecated(model) {
                    continue;
                }
                let reasoning = model.get("reasoning").and_then(Value::as_bool) == Some(true);
                let mut built = build_chat_model(ChatModelSpec {
                    id: model_id.clone(),
                    name: model_name(model, &model_id),
                    api: "openai-completions",
                    provider,
                    base_url,
                    reasoning,
                    input: input_modalities(input_has_image(model)),
                    cost: get_models_dev_cost(model.get("cost")),
                    context_window: limit_or(model, "context"),
                    max_tokens: limit_or(model, "output"),
                });
                if reasoning {
                    set_thinking_level_map(
                        &mut built,
                        &level_map(&[
                            ("off", Some("off")),
                            ("minimal", None),
                            ("low", None),
                            ("medium", None),
                            ("high", Some("high")),
                            ("xhigh", None),
                            ("max", None),
                        ]),
                    );
                }
                set_compat(&mut built, xiaomi_compat());
                reorder_model(&mut built, XIAOMI_ORDER);
                models.push(built);
            }
        }

        // Alibaba Cloud Model Studio Token Plan
        let qwen_token_plan_compat = || {
            let mut compat = Map::new();
            compat.insert(
                "thinkingFormat".to_string(),
                Value::String("qwen".to_string()),
            );
            compat.insert("supportsDeveloperRole".to_string(), Value::Bool(false));
            compat.insert("supportsStore".to_string(), Value::Bool(false));
            compat.insert("supportsReasoningEffort".to_string(), Value::Bool(true));
            compat
        };
        let qwen_token_plan_variants: [(&str, &str, &str, Option<&[&str]>); 3] = [
            (
                "alibaba-token-plan",
                "qwen-token-plan",
                "https://token-plan.ap-southeast-1.maas.aliyuncs.com/compatible-mode/v1",
                None,
            ),
            (
                "alibaba-token-plan",
                "qwen-token-plan-individual",
                "https://token-plan.ap-southeast-1.maas.aliyuncs.com/compatible-mode/v1",
                Some(&QWEN_TOKEN_PLAN_INDIVIDUAL_MODEL_IDS),
            ),
            (
                "alibaba-token-plan-cn",
                "qwen-token-plan-cn",
                "https://token-plan.cn-beijing.maas.aliyuncs.com/compatible-mode/v1",
                None,
            ),
        ];
        for (source, provider, base_url, model_ids) in qwen_token_plan_variants {
            let mut emitted: Vec<String> = Vec::new();
            for (model_id, model) in provider_models(data, source) {
                if !tool_capable(model) {
                    continue;
                }
                if QWEN_TOKEN_PLAN_EXCLUDED_MODEL_IDS.contains(&model_id.as_str()) {
                    continue;
                }
                if let Some(model_ids) = model_ids
                    && !model_ids.contains(&model_id.as_str())
                {
                    continue;
                }
                let thinking_level_map = get_effort_thinking_level_map(&reasoning_options(model))
                    .or_else(|| {
                        if QWEN_TOKEN_PLAN_REASONING_EFFORT_FALLBACK_MODEL_IDS
                            .contains(&model_id.as_str())
                        {
                            Some(level_map(&[
                                ("minimal", None),
                                ("low", None),
                                ("medium", None),
                                ("high", Some("high")),
                                ("xhigh", None),
                                ("max", Some("max")),
                            ]))
                        } else {
                            None
                        }
                    });
                let mut compat = qwen_token_plan_compat();
                if thinking_level_map.is_none() {
                    compat.insert("supportsReasoningEffort".to_string(), Value::Bool(false));
                }
                let mut built = build_chat_model(ChatModelSpec {
                    id: model_id.clone(),
                    name: model_name(model, &model_id),
                    api: "openai-completions",
                    provider,
                    base_url,
                    reasoning: model.get("reasoning").and_then(Value::as_bool) == Some(true),
                    input: input_modalities(input_has_image(model)),
                    cost: get_models_dev_cost(model.get("cost")),
                    context_window: limit_or(model, "context"),
                    max_tokens: limit_or(model, "output"),
                });
                if let Some(map) = &thinking_level_map {
                    set_thinking_level_map(&mut built, map);
                }
                set_compat(&mut built, compat);
                reorder_model(&mut built, QWEN_TOKEN_PLAN_ORDER);
                models.push(built);
                if model_ids.is_some() {
                    emitted.push(model_id.clone());
                }
            }
            // individual 变体是白名单：strict 时生成结果必须与 QWEN_TOKEN_PLAN_INDIVIDUAL_MODEL_IDS
            // 完全一致（多一个少一个都报错）。
            if let (Some(model_ids), true) = (model_ids, options.strict) {
                assert_exact_model_ids(
                    provider,
                    model_ids
                        .iter()
                        .map(|id| id.to_string())
                        .collect::<Vec<_>>(),
                    emitted,
                )?;
            }
        }

        Ok(models)
    }

    /// `data.nvidia?.models ? await fetchNvidiaNimModelIds() : new Map()`。
    fn nvidia_nim_model_ids(
        nvidia_nim: Option<&Value>,
        _state: &GeneratorState,
    ) -> IndexMap<String, String> {
        let mut ids: IndexMap<String, String> = IndexMap::new();
        let Some(items) = nvidia_nim
            .and_then(|value| value.get("data"))
            .and_then(Value::as_array)
        else {
            return ids;
        };
        for item in items {
            let Some(id) = str_at(item, "id") else {
                continue;
            };
            ids.insert(id.to_string(), id.to_string());
            ids.insert(normalize_nvidia_model_id(id), id.to_string());
        }
        ids
    }

    /// `isCopilotClaude`：`/^claude-(haiku|sonnet|opus|fable)-[45]([.\-]|$)/`。
    fn is_copilot_claude_id(model_id: &str) -> bool {
        let Some(rest) = model_id.strip_prefix("claude-") else {
            return false;
        };
        let mut matched_family = false;
        for family in ["haiku", "sonnet", "opus", "fable"] {
            if let Some(after) = rest.strip_prefix(family) {
                if let Some(version) = after.strip_prefix('-')
                    && (version.starts_with('4') || version.starts_with('5'))
                {
                    let tail = &version[1..];
                    if tail.is_empty() || tail.starts_with('.') || tail.starts_with('-') {
                        matched_family = true;
                    }
                }
                break;
            }
        }
        matched_family
    }

    /// `OPENAI_STANDARD_COSTS`（逐字）。每项：id -> `[input, output, cacheRead, cacheWrite]`。
    pub const OPENAI_STANDARD_COSTS: [(&str, [f64; 4]); 7] = [
        ("gpt-5.6-luna", [0.2, 1.2, 0.02, 0.25]),
        ("gpt-5.6-sol", [4.0, 20.0, 0.4, 5.0]),
        ("gpt-5.6-terra", [2.0, 12.0, 0.2, 2.5]),
        ("gpt-6-astra", [10.0, 50.0, 1.0, 12.5]),
        ("gpt-6-luna", [0.1, 0.5, 0.01, 0.125]),
        ("gpt-6-sol", [2.0, 10.0, 0.2, 2.5]),
        ("gpt-6.1-sol", [2.0, 10.0, 0.1, 2.5]),
    ];

    fn openai_standard_cost(id: &str) -> Option<Value> {
        OPENAI_STANDARD_COSTS
            .iter()
            .find(|(model_id, _)| *model_id == id)
            .map(|(_, rates)| cost_object(*rates, None))
    }

    fn openai_long_context_cost(id: &str) -> Option<Value> {
        openai_standard_cost(id).map(|cost| with_open_ai_long_context_pricing(&cost))
    }

    /// `XAI_BUILTIN_EXCLUDED_MODEL_IDS`。
    const XAI_BUILTIN_EXCLUDED_MODEL_IDS: [&str; 6] = [
        "grok-3",
        "grok-3-fast",
        "grok-4.20-0309-non-reasoning",
        "grok-4.20-0309-reasoning",
        "grok-build-0.1",
        "grok-code-fast-1",
    ];
    /// `GITHUB_COPILOT_EXTENDED_CONTEXT_MODELS`。
    const GITHUB_COPILOT_EXTENDED_CONTEXT_MODELS: [&str; 10] = [
        "claude-fable-5",
        "claude-opus-4.8",
        "claude-opus-5",
        "claude-opus-5.5",
        "claude-sonnet-4.6",
        "claude-sonnet-5",
        "gpt-5.3-codex",
        "gpt-6-astra",
        "gpt-6-luna",
        "gpt-6-sol",
    ];
    const OPENAI_SHORT_CONTEXT_CAPPED_MODEL_IDS: [&str; 9] = [
        "gpt-5.4",
        "gpt-5.5",
        "gpt-5.6-sol",
        "gpt-5.6-terra",
        "gpt-5.6-luna",
        "gpt-6-astra",
        "gpt-6-sol",
        "gpt-6-luna",
        "gpt-6.1-sol",
    ];
    const OPENAI_LONG_CONTEXT_PRICING_MODEL_IDS: [&str; 11] = [
        "gpt-5.4",
        "gpt-5.4-pro",
        "gpt-5.5",
        "gpt-5.5-pro",
        "gpt-5.6-sol",
        "gpt-5.6-terra",
        "gpt-5.6-luna",
        "gpt-6-astra",
        "gpt-6-sol",
        "gpt-6-luna",
        "gpt-6.1-sol",
    ];

    fn copilot_headers() -> Value {
        let mut headers = Map::new();
        for (key, value) in COPILOT_STATIC_HEADERS {
            headers.insert(key.to_string(), Value::String(value.to_string()));
        }
        Value::Object(headers)
    }

    /// `missingCopilotModels`：models.dev 尚未收录的 Copilot 条目。
    fn missing_copilot_models() -> Vec<Value> {
        let mut models: Vec<Value> = Vec::new();
        let mut opus = Map::new();
        opus.insert(
            "id".to_string(),
            Value::String("claude-opus-5.5".to_string()),
        );
        opus.insert(
            "name".to_string(),
            Value::String("Claude Opus 5.5".to_string()),
        );
        opus.insert(
            "api".to_string(),
            Value::String("anthropic-messages".to_string()),
        );
        opus.insert(
            "provider".to_string(),
            Value::String("github-copilot".to_string()),
        );
        opus.insert(
            "baseUrl".to_string(),
            Value::String("https://api.individual.githubcopilot.com".to_string()),
        );
        opus.insert("reasoning".to_string(), Value::Bool(true));
        opus.insert(
            "thinkingLevelMap".to_string(),
            super::thinking_level_map_to_json(&level_map(&[
                ("off", None),
                ("minimal", None),
                ("low", Some("low")),
                ("medium", Some("medium")),
                ("high", Some("high")),
                ("xhigh", Some("xhigh")),
                ("max", Some("max")),
            ])),
        );
        opus.insert("input".to_string(), json!(["text", "image"]));
        opus.insert("cost".to_string(), cost_object([4.0, 20.0, 0.2, 5.0], None));
        opus.insert("contextWindow".to_string(), json!(1_000_000));
        opus.insert("maxTokens".to_string(), json!(128_000));
        opus.insert("headers".to_string(), copilot_headers());
        let mut opus = Value::Object(opus);
        reorder_model(&mut opus, DEFAULT_CHAT_ORDER);
        models.push(opus);

        for (id, name) in [("gpt-6-sol", "GPT-6 Sol"), ("gpt-6-luna", "GPT-6 Luna")] {
            let mut object = Map::new();
            object.insert("id".to_string(), Value::String(id.to_string()));
            object.insert("name".to_string(), Value::String(name.to_string()));
            object.insert(
                "api".to_string(),
                Value::String("openai-responses".to_string()),
            );
            object.insert(
                "provider".to_string(),
                Value::String("github-copilot".to_string()),
            );
            object.insert(
                "baseUrl".to_string(),
                Value::String("https://api.individual.githubcopilot.com".to_string()),
            );
            object.insert("reasoning".to_string(), Value::Bool(true));
            object.insert("input".to_string(), json!(["text", "image"]));
            object.insert(
                "cost".to_string(),
                openai_long_context_cost(id).expect("OPENAI_STANDARD_COSTS covers gpt-6 models"),
            );
            object.insert("contextWindow".to_string(), json!(1_000_000));
            object.insert("maxTokens".to_string(), json!(128_000));
            object.insert("headers".to_string(), copilot_headers());
            let mut model = Value::Object(object);
            reorder_model(&mut model, DEFAULT_CHAT_ORDER);
            models.push(model);
        }
        models
    }

    /// `missingOpenAiModels`。
    fn missing_open_ai_models() -> Vec<Value> {
        let mut models: Vec<Value> = Vec::new();
        for (id, name) in [
            ("gpt-6.1-sol", "GPT-6.1 Sol"),
            ("gpt-6-astra", "GPT-6 Astra"),
            ("gpt-6-sol", "GPT-6 Sol"),
            ("gpt-6-luna", "GPT-6 Luna"),
            ("gpt-5.6-sol", "GPT-5.6 Sol"),
            ("gpt-5.6-terra", "GPT-5.6 Terra"),
            ("gpt-5.6-luna", "GPT-5.6 Luna"),
        ] {
            let mut object = Map::new();
            object.insert("id".to_string(), Value::String(id.to_string()));
            object.insert("name".to_string(), Value::String(name.to_string()));
            object.insert(
                "api".to_string(),
                Value::String("openai-responses".to_string()),
            );
            object.insert(
                "baseUrl".to_string(),
                Value::String("https://api.openai.com/v1".to_string()),
            );
            object.insert("provider".to_string(), Value::String("openai".to_string()));
            object.insert("reasoning".to_string(), Value::Bool(true));
            object.insert("input".to_string(), json!(["text", "image"]));
            object.insert(
                "cost".to_string(),
                openai_long_context_cost(id)
                    .expect("OPENAI_STANDARD_COSTS covers gpt-5.6/gpt-6 models"),
            );
            object.insert(
                "contextWindow".to_string(),
                json!(OPENAI_LONG_CONTEXT_INPUT_THRESHOLD),
            );
            object.insert("maxTokens".to_string(), json!(128_000));
            let mut model = Value::Object(object);
            reorder_model(&mut model, BASE_URL_FIRST_ORDER);
            models.push(model);
        }
        let mut chat_latest = Map::new();
        chat_latest.insert(
            "id".to_string(),
            Value::String("gpt-5-chat-latest".to_string()),
        );
        chat_latest.insert(
            "name".to_string(),
            Value::String("GPT-5 Chat Latest".to_string()),
        );
        chat_latest.insert(
            "api".to_string(),
            Value::String("openai-responses".to_string()),
        );
        chat_latest.insert(
            "baseUrl".to_string(),
            Value::String("https://api.openai.com/v1".to_string()),
        );
        chat_latest.insert("provider".to_string(), Value::String("openai".to_string()));
        chat_latest.insert("reasoning".to_string(), Value::Bool(false));
        chat_latest.insert("input".to_string(), json!(["text", "image"]));
        chat_latest.insert(
            "cost".to_string(),
            cost_object([1.25, 10.0, 0.125, 0.0], None),
        );
        chat_latest.insert("contextWindow".to_string(), json!(128_000));
        chat_latest.insert("maxTokens".to_string(), json!(16384));
        let mut chat_latest = Value::Object(chat_latest);
        reorder_model(&mut chat_latest, BASE_URL_FIRST_ORDER);
        models.push(chat_latest);
        models
    }

    fn deepseek_compat() -> Map<String, Value> {
        let mut compat = Map::new();
        compat.insert(
            "requiresReasoningContentOnAssistantMessages".to_string(),
            Value::Bool(true),
        );
        compat.insert(
            "thinkingFormat".to_string(),
            Value::String("deepseek".to_string()),
        );
        compat
    }

    /// `deepseekModels`。
    fn deepseek_models() -> Vec<Value> {
        let mut models: Vec<Value> = Vec::new();
        let mut flash = build_chat_model(ChatModelSpec {
            id: "deepseek-flash".to_string(),
            name: "DeepSeek V4.1 Flash".to_string(),
            api: "openai-completions",
            provider: "deepseek",
            base_url: "https://api.deepseek.com",
            reasoning: true,
            input: json!(["text", "image"]),
            cost: cost_object([0.3, 1.2, 0.006, 0.0], None),
            context_window: json!(1_000_000.0),
            max_tokens: json!(384_000.0),
        });
        set_thinking_level_map(
            &mut flash,
            &level_map(&DEEPSEEK_V4_FLASH_THINKING_LEVEL_MAP),
        );
        set_compat(&mut flash, deepseek_compat());
        reorder_model(&mut flash, DEEPSEEK_FLASH_ORDER);
        models.push(flash);

        let mut pro = build_chat_model(ChatModelSpec {
            id: "deepseek-v4-pro".to_string(),
            name: "DeepSeek V4 Pro".to_string(),
            api: "openai-completions",
            provider: "deepseek",
            base_url: "https://api.deepseek.com",
            reasoning: true,
            input: json!(["text"]),
            cost: cost_object([1.32, 3.96, 0.044, 0.0], None),
            context_window: json!(1_000_000.0),
            max_tokens: json!(384_000.0),
        });
        set_compat(&mut pro, deepseek_compat());
        reorder_model(&mut pro, BASE_URL_FIRST_ORDER);
        models.push(pro);
        models
    }

    /// `antLingModels`。
    fn ant_ling_models() -> Vec<Value> {
        let base_compat = || {
            let mut compat = Map::new();
            compat.insert("supportsStore".to_string(), Value::Bool(false));
            compat.insert("supportsDeveloperRole".to_string(), Value::Bool(false));
            compat.insert("supportsReasoningEffort".to_string(), Value::Bool(false));
            compat.insert(
                "maxTokensField".to_string(),
                Value::String("max_tokens".to_string()),
            );
            compat.insert("supportsLongCacheRetention".to_string(), Value::Bool(false));
            compat
        };
        let mut models: Vec<Value> = Vec::new();
        for (id, name, reasoning, rates) in [
            (
                "Ling-2.6-flash",
                "Ling 2.6 Flash",
                false,
                [0.01, 0.02, 0.0, 0.0],
            ),
            ("Ling-2.6-1T", "Ling 2.6 1T", false, [0.06, 0.25, 0.0, 0.0]),
            ("Ring-2.6-1T", "Ring 2.6 1T", true, [0.06, 0.25, 0.0, 0.0]),
        ] {
            let mut built = build_chat_model(ChatModelSpec {
                id: id.to_string(),
                name: name.to_string(),
                api: "openai-completions",
                provider: "ant-ling",
                base_url: "https://api.ant-ling.com/v1",
                reasoning,
                input: json!(["text"]),
                cost: cost_object(rates, None),
                context_window: json!(262_144.0),
                max_tokens: json!(65_536.0),
            });
            let mut compat = base_compat();
            if reasoning {
                compat.insert(
                    "thinkingFormat".to_string(),
                    Value::String("ant-ling".to_string()),
                );
            }
            set_compat(&mut built, compat);
            reorder_model(&mut built, BASE_URL_FIRST_ORDER);
            models.push(built);
        }
        models
    }

    /// `codexModels`。
    fn codex_models() -> Vec<Value> {
        const CODEX_BASE_URL: &str = "https://chatgpt.com/backend-api";
        const CODEX_CONTEXT: f64 = 272_000.0;
        const CODEX_GPT_61_SOL_CONTEXT: f64 = 500_000.0;
        const CODEX_MAX_TOKENS: f64 = 128_000.0;
        let mut models: Vec<Value> = Vec::new();
        for (id, name, context) in [
            ("gpt-6.1-sol", "GPT-6.1 Sol", CODEX_GPT_61_SOL_CONTEXT),
            ("gpt-6-astra", "GPT-6 Astra", CODEX_CONTEXT),
            ("gpt-6-sol", "GPT-6 Sol", CODEX_CONTEXT),
            ("gpt-6-luna", "GPT-6 Luna", CODEX_CONTEXT),
            ("gpt-5.6-luna", "GPT-5.6 Luna", CODEX_CONTEXT),
            ("gpt-5.6-sol", "GPT-5.6 Sol", CODEX_CONTEXT),
            ("gpt-5.6-terra", "GPT-5.6 Terra", CODEX_CONTEXT),
        ] {
            let mut object = Map::new();
            object.insert("id".to_string(), Value::String(id.to_string()));
            object.insert("name".to_string(), Value::String(name.to_string()));
            object.insert(
                "api".to_string(),
                Value::String("openai-codex-responses".to_string()),
            );
            object.insert(
                "provider".to_string(),
                Value::String("openai-codex".to_string()),
            );
            object.insert(
                "baseUrl".to_string(),
                Value::String(CODEX_BASE_URL.to_string()),
            );
            object.insert("reasoning".to_string(), Value::Bool(true));
            object.insert("input".to_string(), json!(["text", "image"]));
            object.insert(
                "cost".to_string(),
                openai_long_context_cost(id)
                    .expect("OPENAI_STANDARD_COSTS covers the codex models"),
            );
            object.insert("contextWindow".to_string(), json!(context));
            object.insert("maxTokens".to_string(), json!(CODEX_MAX_TOKENS));
            let mut model = Value::Object(object);
            reorder_model(&mut model, DEFAULT_CHAT_ORDER);
            models.push(model);
        }
        models
    }

    // ------------------------------------------------------------------
    // classifier 目录（硬编码）
    // ------------------------------------------------------------------

    /// `OPENCODE_CLASSIFIER_MODELS`。
    pub fn opencode_classifier_models() -> Vec<Value> {
        let mut models: Vec<Value> = Vec::new();
        for (id, name, input_cost) in [
            ("jev-1.13", "Jev 1.13", 0.042),
            ("jev-1.13-free", "Jev 1.13 Free", 0.0),
        ] {
            let mut object = Map::new();
            object.insert("type".to_string(), Value::String("classifier".to_string()));
            object.insert("id".to_string(), Value::String(id.to_string()));
            object.insert("name".to_string(), Value::String(name.to_string()));
            object.insert(
                "api".to_string(),
                Value::String("typesafe-system-one".to_string()),
            );
            object.insert(
                "provider".to_string(),
                Value::String("opencode".to_string()),
            );
            object.insert(
                "baseUrl".to_string(),
                Value::String("https://opencode.ai/inference/v1".to_string()),
            );
            object.insert("input".to_string(), json!(["text"]));
            object.insert(
                "cost".to_string(),
                cost_object([input_cost, 0.0, 0.0, 0.0], None),
            );
            object.insert("contextWindow".to_string(), json!(32_000));
            models.push(Value::Object(object));
        }
        models
    }

    /// `CLOUDFLARE_WORKERS_AI_CLASSIFIER_MODELS`。
    pub fn cloudflare_workers_ai_classifier_models() -> Vec<Value> {
        let entries: [(&str, &str, f64, f64, &str); 5] = [
            ("@cf/cloudflare/clef", "Clef", 0.24, 65_536.0, "images"),
            (
                "@cf/cloudflare/clef-flash",
                "Clef Flash",
                0.038,
                24_576.0,
                "images",
            ),
            (
                "@cf/cloudflare/clef-omni",
                "Clef Omni",
                0.15,
                64_000.0,
                "images",
            ),
            ("typesafe/jev", "Jev", 0.042, 32_000.0, "assets"),
            ("typesafe/jev-1.14.0", "Jev 1.14", 0.042, 32_000.0, "assets"),
        ];
        let mut models: Vec<Value> = Vec::new();
        for (id, name, input_cost, context_window, image_input) in entries {
            let mut object = Map::new();
            object.insert("type".to_string(), Value::String("classifier".to_string()));
            object.insert("id".to_string(), Value::String(id.to_string()));
            object.insert("name".to_string(), Value::String(name.to_string()));
            object.insert(
                "api".to_string(),
                Value::String("cloudflare-workers-ai-system-one".to_string()),
            );
            object.insert(
                "provider".to_string(),
                Value::String("cloudflare-workers-ai".to_string()),
            );
            object.insert(
                "baseUrl".to_string(),
                Value::String(CLOUDFLARE_WORKERS_AI_REST_BASE_URL.to_string()),
            );
            object.insert("input".to_string(), json!(["text", "image"]));
            object.insert(
                "cost".to_string(),
                cost_object([input_cost, 0.0, 0.0, 0.0], None),
            );
            object.insert("contextWindow".to_string(), json!(context_window));
            object.insert("compat".to_string(), json!({ "imageInput": image_input }));
            models.push(Value::Object(object));
        }
        models
    }

    /// `OPENAI_CLASSIFIER_MODELS`。
    pub fn openai_classifier_models() -> Vec<Value> {
        let mut object = Map::new();
        object.insert("type".to_string(), Value::String("classifier".to_string()));
        object.insert("id".to_string(), Value::String("gpt-6-luna".to_string()));
        object.insert("name".to_string(), Value::String("GPT-6 Luna".to_string()));
        object.insert(
            "api".to_string(),
            Value::String("openai-decisions".to_string()),
        );
        object.insert("provider".to_string(), Value::String("openai".to_string()));
        object.insert(
            "baseUrl".to_string(),
            Value::String("https://api.openai.com/v1".to_string()),
        );
        object.insert("input".to_string(), json!(["text", "image"]));
        object.insert(
            "cost".to_string(),
            with_open_ai_long_context_pricing(&cost_object([0.1, 0.0, 0.0, 0.0], None)),
        );
        object.insert("contextWindow".to_string(), json!(922_000));
        vec![Value::Object(object)]
    }

    /// `loadModelsDevClassifierModels()`：`typesafe/jev-latest`（`type === "decision"`）。
    pub fn load_models_dev_classifier_models(
        inputs: &GeneratorInputs,
    ) -> Result<Vec<Value>, CatalogError> {
        let Some(data) = inputs.models_dev_decision.as_ref() else {
            return Ok(Vec::new());
        };
        if data.is_null() {
            return Err(CatalogError::new(
                "TypeError: Cannot read properties of null (reading 'typesafe/jev-latest')",
            ));
        }
        let metadata = data.get("typesafe/jev-latest");
        let is_decision = metadata
            .map(|value| str_at(value, "type") == Some("decision"))
            .unwrap_or(false);
        let Some(metadata) = metadata.filter(|_| is_decision) else {
            return Err(CatalogError::new(
                "models.dev did not return decision model typesafe/jev-latest",
            ));
        };
        let mut object = Map::new();
        object.insert("type".to_string(), Value::String("classifier".to_string()));
        object.insert("id".to_string(), Value::String("jev-latest".to_string()));
        object.insert(
            "name".to_string(),
            Value::String(str_at(metadata, "name").unwrap_or_default().to_string()),
        );
        object.insert(
            "api".to_string(),
            Value::String("typesafe-system-one".to_string()),
        );
        object.insert(
            "provider".to_string(),
            Value::String("typesafe".to_string()),
        );
        object.insert(
            "baseUrl".to_string(),
            Value::String("https://api.typesafe.ai/v1/".to_string()),
        );
        object.insert("input".to_string(), json!(["text", "image"]));
        object.insert("cost".to_string(), cost_object([0.0, 0.0, 0.0, 0.0], None));
        object.insert(
            "contextWindow".to_string(),
            limit_or_default(metadata, "context", 64_000.0),
        );
        object.insert(
            "compat".to_string(),
            json!({ "imageInput": "assets", "sendImageOptInHeader": true }),
        );
        Ok(vec![Value::Object(object)])
    }

    // ------------------------------------------------------------------
    // AI Gateway / Radius
    // ------------------------------------------------------------------

    /// `fetchAiGatewayModels()` 的纯函数部分：把 `data.data` 转成 chat + classifier。
    pub fn ai_gateway_models(ai_gateway: Option<&Value>) -> (Vec<Value>, Vec<Value>) {
        let mut chat: Vec<Value> = Vec::new();
        let mut classifiers: Vec<Value> = Vec::new();
        let items = ai_gateway
            .and_then(|data| data.get("data"))
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let to_number = |value: Option<&Value>| -> f64 {
            // pricing 可能是数字或字符串；解析不出有限值时一律按 0。
            match value {
                Some(Value::Number(_)) => match value.and_then(Value::as_f64) {
                    Some(number) if number.is_finite() => number,
                    _ => 0.0,
                },
                Some(Value::String(text)) => {
                    let parsed = super::js_parse_float(text);
                    if parsed.is_finite() { parsed } else { 0.0 }
                }
                _ => 0.0,
            }
        };
        for model in &items {
            let id = str_at(model, "id").unwrap_or_default().to_string();
            // `type === "evaluation"` 的条目归 classifier（typesafe-system-one）；其余必须
            // 带 `tool-use` tag 才进 chat 表。
            if str_at(model, "type") == Some("evaluation") {
                let mut object = Map::new();
                object.insert("type".to_string(), Value::String("classifier".to_string()));
                object.insert("id".to_string(), Value::String(id.clone()));
                object.insert(
                    "name".to_string(),
                    Value::String(match str_at(model, "name") {
                        Some(name) if !name.is_empty() => name.to_string(),
                        _ => id.clone(),
                    }),
                );
                object.insert(
                    "api".to_string(),
                    Value::String("typesafe-system-one".to_string()),
                );
                object.insert(
                    "provider".to_string(),
                    Value::String("vercel-ai-gateway".to_string()),
                );
                object.insert(
                    "baseUrl".to_string(),
                    Value::String(AI_GATEWAY_TYPESAFE_BASE_URL.to_string()),
                );
                object.insert("input".to_string(), json!(["text"]));
                object.insert(
                    "cost".to_string(),
                    cost_object(
                        [
                            round_cost(
                                to_number(nested(model, &["pricing", "input"])) * 1_000_000.0,
                            ),
                            round_cost(
                                to_number(nested(model, &["pricing", "output"])) * 1_000_000.0,
                            ),
                            0.0,
                            0.0,
                        ],
                        None,
                    ),
                );
                object.insert(
                    "contextWindow".to_string(),
                    js_or_value(model.get("context_window"), json!(4096)),
                );
                classifiers.push(Value::Object(object));
                continue;
            }
            let tags: Vec<String> = model
                .get("tags")
                .and_then(Value::as_array)
                .map(|items| {
                    items
                        .iter()
                        .filter_map(|item| item.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default();
            if !tags.iter().any(|tag| tag == "tool-use") {
                continue;
            }
            let mut input = vec![Value::String("text".to_string())];
            if tags.iter().any(|tag| tag == "vision") {
                input.push(Value::String("image".to_string()));
            }
            let mut built = build_chat_model(ChatModelSpec {
                id: id.clone(),
                name: match str_at(model, "name") {
                    Some(name) if !name.is_empty() => name.to_string(),
                    _ => id.clone(),
                },
                api: "anthropic-messages",
                provider: "vercel-ai-gateway",
                base_url: AI_GATEWAY_BASE_URL,
                reasoning: tags.iter().any(|tag| tag == "reasoning"),
                input: Value::Array(input),
                cost: serde_json::to_value(get_ai_gateway_cost(model.get("pricing")))
                    .unwrap_or(Value::Null),
                context_window: js_or_value(model.get("context_window"), json!(4096)),
                max_tokens: js_or_value(model.get("max_tokens"), json!(4096)),
            });
            let mut compat = Map::new();
            compat.insert("allowEmptySignature".to_string(), Value::Bool(true));
            set_compat(&mut built, compat);
            reorder_model(&mut built, AI_GATEWAY_ORDER);
            chat.push(built);
        }
        (chat, classifiers)
    }

    /// `fetchRadiusModels()` 的纯函数部分：`getRadiusModelsFromConfig("radius", config)`。
    pub fn radius_models(radius_config: Option<&Value>) -> Result<Vec<Value>, CatalogError> {
        let Some(config) = radius_config else {
            return Ok(Vec::new());
        };
        let sanitized = sanitize_radius_gateway_config(config).ok_or_else(|| {
            CatalogError::new("Invalid Radius config from https://radius.kiana.dev")
        })?;
        let (base_url, models) = sanitized;
        if models.is_empty() {
            return Err(CatalogError::new("Radius API returned no models"));
        }
        Ok(models
            .into_iter()
            .map(|mut model| {
                if let Some(object) = model.as_object_mut() {
                    object.insert(
                        "api".to_string(),
                        Value::String("kiana-messages".to_string()),
                    );
                    object.insert("provider".to_string(), Value::String("radius".to_string()));
                    object.insert("baseUrl".to_string(), Value::String(base_url.clone()));
                }
                model
            })
            .collect())
    }

    fn sanitize_radius_gateway_config(config: &Value) -> Option<(String, Vec<Value>)> {
        let object = config.as_object()?;
        let base_url = object.get("baseUrl")?.as_str()?.to_string();
        let models = object.get("models")?.as_array()?;
        let valid: Vec<Value> = models
            .iter()
            .filter(|model| {
                model
                    .as_object()
                    .map(|model| {
                        model.get("id").and_then(Value::as_str).is_some()
                            && model.get("name").and_then(Value::as_str).is_some()
                            && model.get("reasoning").and_then(Value::as_bool).is_some()
                            && model.get("input").map(Value::is_array).unwrap_or(false)
                            && model.get("cost").map(Value::is_object).unwrap_or(false)
                            && model.get("contextWindow").and_then(Value::as_f64).is_some()
                            && model.get("maxTokens").and_then(Value::as_f64).is_some()
                    })
                    .unwrap_or(false)
            })
            .cloned()
            .collect();
        Some((base_url, valid))
    }

    // ------------------------------------------------------------------
    // 模型清单装配
    // ------------------------------------------------------------------

    /// 按 TS 的顺序装配 `allModels`（含所有临时覆盖与手工补充）。
    pub fn build_all_models(
        inputs: &GeneratorInputs,
        options: &GeneratorOptions,
        state: &mut GeneratorState,
    ) -> Result<Vec<Value>, CatalogError> {
        // TS `loadModelsDevData()`：`fetch` 与全部处理在同一个 try 里；失败时
        // `console.error("Failed to load models.dev data:", error)`，strict 重新抛出。
        state
            .stdout_lines
            .push("Fetching models from models.dev API...".to_string());
        let models_dev_models = if inputs.models_dev.is_none() {
            let message = inputs.fetch_error(SOURCE_MODELS_DEV);
            state
                .stderr_lines
                .push(format!("Failed to load models.dev data: {message}"));
            if options.strict {
                return Err(CatalogError::new(message));
            }
            Vec::new()
        } else {
            match load_models_dev_data(
                inputs,
                &LoadOptions {
                    strict: options.strict,
                    nvidia_nim: None,
                },
                state,
            ) {
                Ok(models) => {
                    state.stdout_lines.push(format!(
                        "Loaded {} tool-capable models from models.dev",
                        models.len()
                    ));
                    models
                }
                Err(error) => {
                    state
                        .stderr_lines
                        .push(format!("Failed to load models.dev data: {error}"));
                    if options.strict {
                        return Err(error);
                    }
                    Vec::new()
                }
            }
        };

        // TS `loadModelsDevClassifierModels()`：payload 缺失/非法与 fetch 失败都在 catch 里降级。
        state
            .stdout_lines
            .push("Fetching classifier models from models.dev API...".to_string());
        let classifier_result = if inputs.models_dev_decision.is_none() {
            Err(CatalogError::new(
                inputs.fetch_error(SOURCE_MODELS_DEV_CLASSIFIER),
            ))
        } else {
            load_models_dev_classifier_models(inputs)
        };
        state.models_dev_classifier_models = match classifier_result {
            Ok(models) => models,
            Err(error) => {
                state.stderr_lines.push(format!(
                    "Failed to load models.dev classifier data: {error}"
                ));
                if options.strict {
                    return Err(error);
                }
                Vec::new()
            }
        };

        // TS `fetchOpenRouterModels()`：三次 fetch 任一失败 -> 整个 catalog 为空。
        state
            .stdout_lines
            .push("Fetching models from OpenRouter API...".to_string());
        let open_router_catalog: OpenRouterCatalog =
            match inputs.fetch_errors.get(SOURCE_OPEN_ROUTER) {
                Some(message) => {
                    state
                        .stderr_lines
                        .push(format!("Failed to fetch OpenRouter models: {message}"));
                    if options.strict {
                        return Err(CatalogError::new(message.clone()));
                    }
                    OpenRouterCatalog::default()
                }
                None => {
                    let catalog = build_open_router_catalog(
                        &inputs.open_router_listed,
                        &inputs.open_router_image_listed,
                        &inputs.open_router_decision_listed,
                    );
                    state.stdout_lines.push(format!(
                    "Fetched {} tool-capable, {} image, and {} classifier models from OpenRouter",
                    catalog.chat.len(),
                    catalog.images.len(),
                    catalog.classifiers.len()
                ));
                    if options.strict && catalog.images.is_empty() {
                        let error =
                            CatalogError::new("OpenRouter API returned no usable image models");
                        // TS 的这个 throw 也在 try 里，会被自己的 catch 记录后（strict）重抛。
                        state
                            .stderr_lines
                            .push(format!("Failed to fetch OpenRouter models: {error}"));
                        return Err(error);
                    }
                    catalog
                }
            };

        // TS `fetchAiGatewayModels()`。
        state
            .stdout_lines
            .push("Fetching models from Vercel AI Gateway API...".to_string());
        let (ai_gateway_chat, _ai_gateway_classifiers) = match inputs.ai_gateway.as_ref() {
            Some(_) => {
                let (chat, classifiers) = ai_gateway_models(inputs.ai_gateway.as_ref());
                state.stdout_lines.push(format!(
                    "Fetched {} tool-capable and {} classifier models from Vercel AI Gateway",
                    chat.len(),
                    classifiers.len()
                ));
                (chat, classifiers)
            }
            None => {
                let message = inputs.fetch_error(SOURCE_AI_GATEWAY);
                state.stderr_lines.push(format!(
                    "Failed to fetch Vercel AI Gateway models: {message}"
                ));
                if options.strict {
                    return Err(CatalogError::new(message));
                }
                (Vec::new(), Vec::new())
            }
        };

        // TS `fetchRadiusModels()`：配置取回失败/配置非法/没有模型都在 catch 里降级。
        state
            .stdout_lines
            .push("Fetching models from Radius API...".to_string());
        let radius_models = match inputs.radius_config.as_ref() {
            None => {
                let message = inputs.fetch_error(SOURCE_RADIUS);
                state
                    .stderr_lines
                    .push(format!("Failed to fetch Radius models: {message}"));
                if options.strict {
                    return Err(CatalogError::new(message));
                }
                Vec::new()
            }
            Some(_) => match radius_models(inputs.radius_config.as_ref()) {
                Ok(models) => {
                    state
                        .stdout_lines
                        .push(format!("Fetched {} models from Radius", models.len()));
                    models
                }
                Err(error) => {
                    state
                        .stderr_lines
                        .push(format!("Failed to fetch Radius models: {error}"));
                    if options.strict {
                        return Err(error);
                    }
                    Vec::new()
                }
            },
        };

        // 合并顺序固定为 models.dev -> OpenRouter chat -> AI Gateway -> Radius；
        // 合并后统一剔除被本地覆盖的 xAI/OpenCode 条目。
        let mut all_models: Vec<Value> = Vec::new();
        all_models.extend(models_dev_models);
        all_models.extend(open_router_catalog.chat.iter().cloned());
        all_models.extend(ai_gateway_chat);
        all_models.extend(radius_models);
        all_models.retain(|model| {
            let provider = str_at(model, "provider").unwrap_or_default();
            let id = str_at(model, "id").unwrap_or_default();
            !(provider == "xai" && XAI_BUILTIN_EXCLUDED_MODEL_IDS.contains(&id))
                && !((provider == "opencode" || provider == "opencode-go")
                    && id == "gpt-5.3-codex-spark")
        });

        // 手工补充只在该 (provider,id) 尚不存在时追加，避免覆盖上游数据。
        for model in missing_copilot_models() {
            let provider = str_at(&model, "provider").unwrap_or_default();
            let id = str_at(&model, "id").unwrap_or_default();
            if !all_models.iter().any(|candidate| {
                str_at(candidate, "provider") == Some(provider)
                    && str_at(candidate, "id") == Some(id)
            }) {
                all_models.push(model);
            }
        }

        apply_temporary_overrides(&mut all_models);

        for model in missing_open_ai_models() {
            let provider = str_at(&model, "provider").unwrap_or_default();
            let id = str_at(&model, "id").unwrap_or_default();
            if !all_models.iter().any(|candidate| {
                str_at(candidate, "provider") == Some(provider)
                    && str_at(candidate, "id") == Some(id)
            }) {
                all_models.push(model);
            }
        }
        all_models.extend(deepseek_models());
        all_models.extend(ant_ling_models());

        // DeepSeek V4 的 compat 修正（Qwen Token Plan 三个 provider 除外）。
        let deepseek_compat_map = deepseek_compat();
        for candidate in all_models.iter_mut() {
            if str_at(candidate, "api") != Some("openai-completions") {
                continue;
            }
            let id = str_at(candidate, "id").unwrap_or_default().to_string();
            let provider = str_at(candidate, "provider")
                .unwrap_or_default()
                .to_string();
            if !id.contains("deepseek-v4")
                || QWEN_TOKEN_PLAN_PROVIDER_IDS.contains(&provider.as_str())
            {
                continue;
            }
            let preserves_native = provider == "openrouter" || provider == "opencode";
            let mut compat = Map::new();
            if preserves_native {
                compat.insert(
                    "requiresReasoningContentOnAssistantMessages".to_string(),
                    Value::Bool(true),
                );
            } else {
                compat = deepseek_compat_map.clone();
            }
            set_compat(candidate, js_spread(&current_compat(candidate), &compat));
        }

        all_models.extend(codex_models());

        // Mistral Medium 3.5
        if !all_models.iter().any(|model| {
            str_at(model, "provider") == Some("mistral")
                && str_at(model, "id") == Some("mistral-medium-3.5")
        }) {
            let mut built = build_chat_model(ChatModelSpec {
                id: "mistral-medium-3.5".to_string(),
                name: "Mistral Medium 3.5".to_string(),
                api: "mistral-conversations",
                provider: "mistral",
                base_url: "https://api.mistral.ai",
                reasoning: true,
                input: json!(["text", "image"]),
                cost: cost_object([1.5, 7.5, 0.0, 0.0], None),
                context_window: json!(262_144.0),
                max_tokens: json!(262_144.0),
            });
            if let Some(map) = get_effort_thinking_level_map(&[ModelsDevReasoningOption::Effort {
                values: vec![Some("none".to_string()), Some("high".to_string())],
            }]) {
                set_thinking_level_map(&mut built, &map);
            }
            reorder_model(&mut built, THINKING_EARLY_ORDER);
            all_models.push(built);
        }

        // openrouter/auto 别名
        if !all_models.iter().any(|model| {
            str_at(model, "provider") == Some("openrouter") && str_at(model, "id") == Some("auto")
        }) {
            all_models.push(build_chat_model(ChatModelSpec {
                id: "auto".to_string(),
                name: "Auto".to_string(),
                api: "openai-completions",
                provider: "openrouter",
                base_url: "https://openrouter.ai/api/v1",
                reasoning: true,
                input: json!(["text", "image"]),
                cost: cost_object([0.0, 0.0, 0.0, 0.0], None),
                context_window: json!(2_000_000.0),
                max_tokens: json!(30_000.0),
            }));
        }

        // openrouter/fusion 别名
        if !all_models.iter().any(|model| {
            str_at(model, "provider") == Some("openrouter")
                && str_at(model, "id") == Some("openrouter/fusion")
        }) {
            all_models.push(build_chat_model(ChatModelSpec {
                id: "openrouter/fusion".to_string(),
                name: "OpenRouter: Fusion".to_string(),
                api: "openai-completions",
                provider: "openrouter",
                base_url: "https://openrouter.ai/api/v1",
                reasoning: true,
                input: json!(["text"]),
                cost: cost_object([0.0, 0.0, 0.0, 0.0], None),
                context_window: json!(1_000_000.0),
                max_tokens: json!(30_000.0),
            }));
        }

        // Azure 派生：OpenAI Responses → Azure OpenAI Responses
        // Azure 条目是从 OpenAI/DeepSeek 模型派生的副本：必须放在上面所有覆盖之后，
        // 派生结果才能继承最终的 contextWindow 与 compat（Azure 规则会改写部分 compat 键）；
        // cost 不继承：Azure OpenAI 只用四个基础费率重建（丢弃 tiers），Azure DeepSeek
        // 直接替换为固定价目。
        let azure_openai_models: Vec<Value> = all_models
            .iter()
            .filter(|model| {
                str_at(model, "provider") == Some("openai")
                    && str_at(model, "api") == Some("openai-responses")
            })
            .map(|model| {
                let mut clone = model.clone();
                let id = str_at(model, "id").unwrap_or_default().to_string();
                let context_override = AZURE_CONTEXT_WINDOW_OVERRIDES
                    .iter()
                    .find(|(name, _)| *name == id)
                    .map(|(_, value)| *value);
                let cost = model.get("cost").cloned().unwrap_or(Value::Null);
                // TS `cost: { input: model.cost.input, ... }`：原样复制（含上游的非 number 值）。
                let base = [
                    cost.get("input").cloned().unwrap_or(json!(0)),
                    cost.get("output").cloned().unwrap_or(json!(0)),
                    cost.get("cacheRead").cloned().unwrap_or(json!(0)),
                    cost.get("cacheWrite").cloned().unwrap_or(json!(0)),
                ];
                if let Some(object) = clone.as_object_mut() {
                    object.insert(
                        "api".to_string(),
                        Value::String("azure-openai-responses".to_string()),
                    );
                    object.insert("provider".to_string(), Value::String("azure".to_string()));
                    object.insert("baseUrl".to_string(), Value::String(String::new()));
                    object.insert("cost".to_string(), cost_object_values(base, None));
                    object.insert(
                        "contextWindow".to_string(),
                        match context_override {
                            Some(value) => json!(value),
                            None => model.get("contextWindow").cloned().unwrap_or(Value::Null),
                        },
                    );
                }
                clone
            })
            .collect();
        all_models.extend(azure_openai_models);

        // Azure 派生：DeepSeek V4 Pro
        let azure_deepseek_models: Vec<Value> = all_models
            .iter()
            .filter(|model| {
                str_at(model, "provider") == Some("deepseek")
                    && str_at(model, "id") == Some("deepseek-v4-pro")
            })
            .map(|model| {
                let mut clone = model.clone();
                let mut compat = current_compat(model);
                compat.insert("supportsDeveloperRole".to_string(), Value::Bool(false));
                compat.insert(
                    "supportsMidConvoSystemMessages".to_string(),
                    Value::Bool(true),
                );
                compat.insert(
                    "thinkingFormat".to_string(),
                    Value::String("openai".to_string()),
                );
                compat.insert("supportsLongCacheRetention".to_string(), Value::Bool(false));
                if let Some(object) = clone.as_object_mut() {
                    object.insert("provider".to_string(), Value::String("azure".to_string()));
                    object.insert("baseUrl".to_string(), Value::String(String::new()));
                    object.insert(
                        "cost".to_string(),
                        cost_object(AZURE_DEEPSEEK_V4_PRO_COST, None),
                    );
                }
                set_compat(&mut clone, compat);
                clone
            })
            .collect();
        all_models.extend(azure_deepseek_models);

        Ok(all_models)
    }

    /// 对已知上游偏差做逐条原地覆盖（contextWindow、thinkingLevelMap、成本档位等）。
    ///
    /// 只在 `(provider, id)` 命中时改写已有模型，不新增条目；因此对同一批输入重复调用是
    /// 幂等的。覆盖必须在派生（Azure）之前执行，派生副本才能继承修正后的字段。
    fn apply_temporary_overrides(all_models: &mut [Value]) {
        for candidate in all_models.iter_mut() {
            let provider = str_at(candidate, "provider")
                .unwrap_or_default()
                .to_string();
            let id = str_at(candidate, "id").unwrap_or_default().to_string();

            if provider == "github-copilot"
                && GITHUB_COPILOT_EXTENDED_CONTEXT_MODELS.contains(&id.as_str())
            {
                set_number(candidate, "contextWindow", 1_000_000.0);
            }

            if (provider == "anthropic"
                && (id == "claude-opus-5-5"
                    || id == "claude-sonnet-5-5"
                    || id == "claude-haiku-5-5"))
                || (provider == "github-copilot" && id == "claude-opus-5.5")
            {
                let haiku = id == "claude-haiku-5-5";
                let mut pairs: Vec<(&str, Option<&str>)> = Vec::new();
                if !haiku {
                    pairs.push(("off", None));
                }
                pairs.extend([
                    ("minimal", None),
                    ("low", Some("low")),
                    ("medium", Some("medium")),
                    ("high", Some("high")),
                    ("xhigh", Some("xhigh")),
                    ("max", Some("max")),
                ]);
                merge_thinking_level_map(candidate, &level_map(&pairs));
            }

            if (provider == "anthropic" || provider == "opencode" || provider == "opencode-go")
                && (id == "claude-opus-4-6"
                    || id == "claude-sonnet-4-6"
                    || id == "claude-opus-4.6"
                    || id == "claude-sonnet-4.6")
            {
                set_number(candidate, "contextWindow", 1_000_000.0);
            }

            if (provider == "opencode" || provider == "opencode-go")
                && (id == "claude-sonnet-4-5" || id == "claude-sonnet-4")
            {
                set_number(candidate, "contextWindow", 200_000.0);
            }
            if (provider == "opencode" || provider == "opencode-go") && id == "gpt-5.4" {
                set_number(candidate, "contextWindow", 272_000.0);
                set_number(candidate, "maxTokens", 128_000.0);
            }
            if provider == "openai" && OPENAI_SHORT_CONTEXT_CAPPED_MODEL_IDS.contains(&id.as_str())
            {
                set_number(
                    candidate,
                    "contextWindow",
                    OPENAI_LONG_CONTEXT_INPUT_THRESHOLD,
                );
                set_number(candidate, "maxTokens", 128_000.0);
            }
            if provider == "openai" && OPENAI_LONG_CONTEXT_PRICING_MODEL_IDS.contains(&id.as_str())
            {
                let cost = openai_standard_cost(&id)
                    .unwrap_or_else(|| candidate.get("cost").cloned().unwrap_or(Value::Null));
                let priced = with_open_ai_long_context_pricing(&cost);
                if let Some(object) = candidate.as_object_mut() {
                    object.insert("cost".to_string(), priced);
                }
            }
            if provider == "cloudflare-ai-gateway"
                && let Some(cost) = openai_standard_cost(&id)
            {
                let priced = with_open_ai_long_context_pricing(&cost);
                if let Some(object) = candidate.as_object_mut() {
                    object.insert("cost".to_string(), priced);
                }
            }
            if provider == "openai" && id == "gpt-5-pro" {
                set_number(candidate, "maxTokens", 128_000.0);
            }
            if (provider == "openrouter" && id == "~moonshotai/kimi-latest")
                || ((provider == "openrouter" || provider == "vercel-ai-gateway")
                    && id.starts_with("moonshotai/")
                    && is_kimi_k3_family_id(&id["moonshotai/".len()..]))
            {
                set_number(candidate, "maxTokens", KIMI_K3_MAX_TOKENS as f64);
            }
            if provider == "openrouter" && id == "moonshotai/kimi-k2.5" {
                set_cost_rate(candidate, "input", 0.41);
                set_cost_rate(candidate, "output", 2.06);
                set_cost_rate(candidate, "cacheRead", 0.07);
                set_number(candidate, "maxTokens", 4096.0);
            }
            if provider == "openrouter" && id.starts_with("moonshotai/kimi-k2.6") {
                let mut compat = Map::new();
                compat.insert("supportsDeveloperRole".to_string(), Value::Bool(false));
                compat.insert(
                    "requiresReasoningContentOnAssistantMessages".to_string(),
                    Value::Bool(true),
                );
                set_compat(candidate, js_spread(&current_compat(candidate), &compat));
            }
            if provider == "openrouter" && id == "z-ai/glm-5" {
                set_cost_rate(candidate, "input", 0.6);
                set_cost_rate(candidate, "output", 1.9);
                set_cost_rate(candidate, "cacheRead", 0.119);
            }
        }
    }

    fn set_number(model: &mut Value, key: &str, value: f64) {
        if let Some(object) = model.as_object_mut() {
            object.insert(key.to_string(), json!(value));
        }
    }

    fn set_cost_rate(model: &mut Value, key: &str, value: f64) {
        if let Some(object) = model.as_object_mut()
            && let Some(cost) = object.get_mut("cost").and_then(Value::as_object_mut)
        {
            cost.insert(key.to_string(), json!(value));
        }
    }

    // ------------------------------------------------------------------
    // 生成结果
    // ------------------------------------------------------------------

    /// 一个 provider 的三张表（chat / image / classifier，均按 id 键控）。
    #[derive(Debug, Clone, Default)]
    pub struct ProviderCatalog {
        /// chat 模型，键为模型 id。
        pub chat: IndexMap<String, Value>,
        /// image 模型，键为模型 id。
        pub image: IndexMap<String, Value>,
        /// classifier 模型，键为模型 id。
        pub classifier: IndexMap<String, Value>,
    }

    /// `generateModels()` 的纯计算结果：`providers` 对象加派生输出。
    #[derive(Debug, Clone, Default)]
    pub struct GeneratedCatalog {
        /// 保持 TS 的对象插入序（provider 首次出现顺序）。
        pub providers: IndexMap<String, ProviderCatalog>,
        /// `Object.keys(providers).sort()`（码元序）。
        pub sorted_provider_ids: Vec<String>,
        /// `jsonChatProviders`（provider 内按 `localeCompare` 排序）。
        pub json_chat_providers: IndexMap<String, IndexMap<String, Value>>,
        /// 同 [`GeneratedCatalog::json_chat_providers`]，内容为 image 模型。
        pub json_image_providers: IndexMap<String, IndexMap<String, Value>>,
        /// 同 [`GeneratedCatalog::json_chat_providers`]，内容为 classifier 模型。
        pub json_classifier_providers: IndexMap<String, IndexMap<String, Value>>,
        /// provider -> 该 provider 的全部模型数组（chat、image、classifier 依次拼接）。
        pub json_all_providers: IndexMap<String, Vec<Value>>,
        /// 数据文件内容（`<provider>.json`）。
        pub file_contents: IndexMap<String, String>,
        /// provider -> `"type:id"` -> api，既写 manifest 的 structureHash，也用于校验。
        pub model_data_structure: ModelDataStructure,
        /// 分组后的数据（用于写盘与哈希）。
        pub generated_data_providers: IndexMap<String, IndexMap<String, IndexMap<String, Value>>>,
        /// `allModels.length`。
        pub total_models: usize,
        /// `allModels.filter((model) => model.reasoning).length`。
        pub reasoning_models: usize,
    }

    /// `generateModels()` 的模型装配与分组部分（不含落盘/输出）。
    pub fn generate_catalog(
        inputs: &GeneratorInputs,
        options: &GeneratorOptions,
        state: &mut GeneratorState,
    ) -> Result<GeneratedCatalog, CatalogError> {
        let mut all_models = build_all_models(inputs, options, state)?;

        // 元数据应用器（顺序即 TS 的调用顺序）。
        // 应用器之间有数据依赖：后面的步骤会读前面写入的 compat/thinkingLevelMap
        // （例如 supports_direct_reasoning_effort 依赖 forceAdaptiveThinking），不能换序。
        for model in all_models.iter_mut() {
            apply_openai_completions_compat_metadata(model);
            apply_anthropic_messages_compat_metadata(model);
            apply_models_dev_reasoning_option_metadata(model, state);
            apply_thinking_level_metadata(model, state);
            apply_strict_tool_compat_metadata(model);
            apply_openai_grammar_tool_compat_metadata(model);
            apply_openai_tool_search_metadata(model);
            apply_openai_completions_transcript_metadata(model);
            apply_openai_responses_transcript_metadata(model);
            apply_openai_explicit_prompt_cache_metadata(model);
            apply_openai_mid_convo_effort_metadata(model);
            apply_prompt_cache_metadata(model);
            apply_image_input_metadata(model);
        }
        // fallback 元数据需要看到同批其他模型（按 id 查表复制 provider/cost），因此先收集
        // 索引与副本、统一计算，再按原下标写回。
        let fallback_indices: Vec<usize> = all_models
            .iter()
            .enumerate()
            .filter(|(_, model)| is_anthropic_fallback_metadata_model(model))
            .map(|(index, _)| index)
            .collect();
        let mut fallback_models: Vec<Value> = fallback_indices
            .iter()
            .map(|index| all_models[*index].clone())
            .collect();
        apply_anthropic_allowed_fallback_model_metadata(&mut fallback_models);
        for (position, index) in fallback_indices.iter().enumerate() {
            all_models[*index] = fallback_models[position].clone();
        }

        // `fetchOpenRouterModels()` 失败时（非 strict），TS 得到的是空 catalog。
        let open_router_catalog = if inputs.fetch_errors.contains_key(SOURCE_OPEN_ROUTER) {
            OpenRouterCatalog::default()
        } else {
            build_open_router_catalog(
                &inputs.open_router_listed,
                &inputs.open_router_image_listed,
                &inputs.open_router_decision_listed,
            )
        };

        let total_models = all_models.len();
        let reasoning_models = all_models
            .iter()
            .filter(|model| model.get("reasoning").and_then(Value::as_bool) == Some(true))
            .count();

        // `providers[model.provider].chat[model.id] ??= {...model, type: "chat"}`
        // `providers[model.provider].chat[model.id] ??= {...model, type: "chat"}`：
        // 同一 (provider,id) 先出现者胜出，重复项被 `or_insert_with` 忽略。
        let mut providers: IndexMap<String, ProviderCatalog> = IndexMap::new();
        for model in &all_models {
            let provider = str_at(model, "provider").unwrap_or_default().to_string();
            let id = str_at(model, "id").unwrap_or_default().to_string();
            let entry = providers.entry(provider).or_default();
            entry.chat.entry(id).or_insert_with(|| {
                let mut chat = model.clone();
                if let Some(object) = chat.as_object_mut() {
                    object.insert("type".to_string(), Value::String("chat".to_string()));
                }
                chat
            });
        }
        for model in &open_router_catalog.images {
            let mut model = model.clone();
            apply_image_input_metadata(&mut model);
            let provider = str_at(&model, "provider").unwrap_or_default().to_string();
            let id = str_at(&model, "id").unwrap_or_default().to_string();
            let entry = providers.entry(provider).or_default();
            entry.image.entry(id).or_insert(model);
        }
        // classifier 来源顺序固定：models.dev -> OpenRouter -> AI Gateway -> 硬编码表；
        // 同样按 (provider,id) 去重、先到先得。
        let mut classifier_models: Vec<Value> = Vec::new();
        // `build_all_models` 已按 TS 顺序取回并做过 strict/非 strict 处理。
        classifier_models.extend(std::mem::take(&mut state.models_dev_classifier_models));
        classifier_models.extend(open_router_catalog.classifiers.iter().cloned());
        let (_, ai_gateway_classifiers) = ai_gateway_models(inputs.ai_gateway.as_ref());
        classifier_models.extend(ai_gateway_classifiers);
        classifier_models.extend(opencode_classifier_models());
        classifier_models.extend(cloudflare_workers_ai_classifier_models());
        classifier_models.extend(openai_classifier_models());
        for model in &classifier_models {
            let provider = str_at(model, "provider").unwrap_or_default().to_string();
            let id = str_at(model, "id").unwrap_or_default().to_string();
            let entry = providers.entry(provider).or_default();
            entry.classifier.entry(id).or_insert_with(|| model.clone());
        }

        let mut sorted_provider_ids: Vec<String> = providers.keys().cloned().collect();
        sorted_provider_ids.sort_by(|a, b| super::model_data::code_unit_compare(a, b));

        let mut json_chat_providers: IndexMap<String, IndexMap<String, Value>> = IndexMap::new();
        let mut json_image_providers: IndexMap<String, IndexMap<String, Value>> = IndexMap::new();
        let mut json_classifier_providers: IndexMap<String, IndexMap<String, Value>> =
            IndexMap::new();
        let mut json_all_providers: IndexMap<String, Vec<Value>> = IndexMap::new();
        for provider_id in &sorted_provider_ids {
            let catalog = &providers[provider_id];
            let chat = sort_by_locale(&catalog.chat);
            let image = sort_by_locale(&catalog.image);
            let classifier = sort_by_locale(&catalog.classifier);
            let mut all: Vec<Value> = Vec::new();
            all.extend(chat.values().cloned());
            all.extend(image.values().cloned());
            all.extend(classifier.values().cloned());
            json_chat_providers.insert(provider_id.clone(), chat);
            json_image_providers.insert(provider_id.clone(), image);
            json_classifier_providers.insert(provider_id.clone(), classifier);
            json_all_providers.insert(provider_id.clone(), all);
        }

        // --data-only 时只沿聚合器里已有的 provider 列表生成数据，不新增 provider 分片；
        // 列表里出现但本次没有模型的 provider 会直接报错，避免产出缺文件的目录。
        let generated_data_provider_ids: Vec<String> = if options.data_only {
            read_model_data_provider_ids(&inputs_package_root(inputs))?
        } else {
            sorted_provider_ids.clone()
        };
        let missing: Vec<&str> = generated_data_provider_ids
            .iter()
            .filter(|provider_id| !json_all_providers.contains_key(*provider_id))
            .map(String::as_str)
            .collect();
        if !missing.is_empty() {
            return Err(CatalogError::new(format!(
                "Cannot hydrate missing providers: {}",
                missing.join(", ")
            )));
        }

        let mut generated_data_providers: IndexMap<
            String,
            IndexMap<String, IndexMap<String, Value>>,
        > = IndexMap::new();
        let mut model_data_structure: ModelDataStructure = IndexMap::new();
        let mut file_contents: IndexMap<String, String> = IndexMap::new();
        for provider_id in &generated_data_provider_ids {
            let models = &json_all_providers[provider_id];
            let grouped = group_provider_model_data(provider_id, models)?;
            generated_data_providers.insert(provider_id.clone(), grouped.groups.clone());
            model_data_structure.insert(provider_id.clone(), grouped.structure);
            // TS：`serializeJson(generatedDataProviders[providerId])`，受 `--pretty` 影响。
            file_contents.insert(
                format!("{provider_id}.json"),
                serialize_groups_pretty(&grouped.groups, options.pretty),
            );
        }

        Ok(GeneratedCatalog {
            providers,
            sorted_provider_ids,
            json_chat_providers,
            json_image_providers,
            json_classifier_providers,
            json_all_providers,
            file_contents,
            model_data_structure,
            generated_data_providers,
            total_models,
            reasoning_models,
        })
    }

    /// `dataOnly` 时读 `readModelDataProviderIds(packageRoot)`；TS 里 `packageRoot` 是
    /// `scripts/..`，Rust 侧由 [`GeneratorInputs::package_root`] 提供。
    fn inputs_package_root(inputs: &GeneratorInputs) -> PathBuf {
        inputs.package_root.clone()
    }

    /// `Object.entries(record).sort(([a], [b]) => a.localeCompare(b))`。
    fn sort_by_locale(record: &IndexMap<String, Value>) -> IndexMap<String, Value> {
        let mut entries: Vec<(String, Value)> = record
            .iter()
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect();
        entries.sort_by(|left, right| locale_compare(&left.0, &right.0));
        entries.into_iter().collect()
    }

    // ------------------------------------------------------------------
    // 输出
    // ------------------------------------------------------------------

    /// `serializeJson(value) = JSON.stringify(value, null, pretty ? 2 : undefined) + "\n"`。
    pub fn serialize_json(value: &Value, pretty: bool) -> String {
        if pretty {
            format!("{}\n", kiana_json::js_object::stringify_pretty(value, 2))
        } else {
            format!("{}\n", stringify_json(value))
        }
    }

    fn map_to_json(map: &IndexMap<String, Value>) -> Value {
        let mut object = Map::new();
        for (key, value) in map {
            object.insert(key.clone(), value.clone());
        }
        Value::Object(object)
    }

    /// `generateModels()` 的完整执行：产出 stdout 行 + 落盘。
    ///
    /// TS 的日志顺序与路径替换语义保持一致；`write_data`/`write_generated_ts`/`write_json`
    /// 由 [`GeneratorOptions`] 与 `package_root` 决定。
    pub fn generate_models(
        inputs: &GeneratorInputs,
        options: &GeneratorOptions,
        state: &mut GeneratorState,
    ) -> Result<Vec<String>, CatalogError> {
        // TS 的 `console.log` 直接写 stdout：失败时此前打印的进度行仍然可见，所以错误
        // 返回前把已产生的行放回 `state.stdout_lines`。
        let mut output: Vec<String> = std::mem::take(&mut state.stdout_lines);
        let catalog = match generate_catalog(inputs, options, state) {
            Ok(catalog) => {
                output.append(&mut state.stdout_lines);
                catalog
            }
            Err(error) => {
                output.append(&mut state.stdout_lines);
                state.stdout_lines = output;
                return Err(error);
            }
        };
        match generate_models_after_catalog(inputs, options, &catalog, &mut output) {
            Ok(()) => Ok(output),
            Err(error) => {
                state.stdout_lines = output;
                Err(error)
            }
        }
    }

    /// [`generate_models`] 的落盘 / JSON 输出 / 统计阶段（catalog 已生成）。
    fn generate_models_after_catalog(
        inputs: &GeneratorInputs,
        options: &GeneratorOptions,
        catalog: &GeneratedCatalog,
        output: &mut Vec<String>,
    ) -> Result<(), CatalogError> {
        let generated_at = js_date_to_iso_string(super::now_epoch_millis());

        if !options.json_only {
            // 写盘分三段：先把数据文件与 manifest 写进 staging 并完整校验；再快照并重写
            // TS 分片/聚合器；最后 rename 换入 data。任一步失败都回滚已写过的 TS 文件。
            let package_root = &inputs.package_root;
            let providers_dir = package_root.join("src").join("providers");
            let aggregator_path = package_root.join("src").join("models.generated.ts");
            let staging_root =
                super::hydrate::create_staging_dir(&providers_dir, ".model-generation-")?;
            let staged_data_dir = staging_root.join("data");
            let previous_data_dir = staging_root.join("previous-data");
            let result = (|| -> Result<(), CatalogError> {
                std::fs::create_dir_all(&staged_data_dir)
                    .map_err(|error| node_fs_error(&error, "mkdir", &staged_data_dir, None))?;
                for (filename, content) in &catalog.file_contents {
                    let path = staged_data_dir.join(filename);
                    std::fs::write(&path, content)
                        .map_err(|error| node_fs_error(&error, "open", &path, None))?;
                }
                let manifest = create_model_data_manifest(
                    &catalog.model_data_structure,
                    &catalog.file_contents,
                    &generated_at,
                );
                let manifest_path = staged_data_dir.join(MODEL_DATA_MANIFEST_FILE);
                std::fs::write(
                    &manifest_path,
                    serialize_json(&manifest.to_json(), options.pretty),
                )
                .map_err(|error| node_fs_error(&error, "open", &manifest_path, None))?;
                validate_model_data_directory(&catalog.model_data_structure, &staged_data_dir)?;

                // TS `restoreGeneratedCatalog`：先在写盘前快照 *.models.ts 与聚合器，此后任何
                // 错误（写分片、删旧分片、写聚合器、rename data）都要回滚。
                let mut restore: Option<(IndexMap<String, String>, String)> = None;
                let write_phase = (|| -> Result<(), CatalogError> {
                    if !options.data_only {
                        let mut previous_shards: IndexMap<String, String> = IndexMap::new();
                        for entry in read_dir_names(&providers_dir)? {
                            if entry.ends_with(".models.ts") {
                                let path = providers_dir.join(&entry);
                                let content = read_file_utf8(&path).map_err(|error| {
                                    CatalogError::new(node_read_error(&path, &error))
                                })?;
                                previous_shards.insert(entry, content);
                            }
                        }
                        let previous_aggregator =
                            read_file_utf8(&aggregator_path).map_err(|error| {
                                CatalogError::new(node_read_error(&aggregator_path, &error))
                            })?;
                        restore = Some((previous_shards, previous_aggregator));

                        let shards = generate_provider_shards(&catalog.sorted_provider_ids);
                        let mut generated_shard_files: BTreeSet<String> = BTreeSet::new();
                        for (filename, content) in &shards {
                            let path = providers_dir.join(filename);
                            std::fs::write(&path, content)
                                .map_err(|error| node_fs_error(&error, "open", &path, None))?;
                            generated_shard_files.insert(filename.clone());
                        }
                        for entry in read_dir_names(&providers_dir)? {
                            if entry.ends_with(".models.ts")
                                && !generated_shard_files.contains(&entry)
                            {
                                let path = providers_dir.join(&entry);
                                std::fs::remove_file(&path).map_err(|error| {
                                    node_fs_error(&error, "unlink", &path, None)
                                })?;
                            }
                        }
                        let aggregator = generate_models_generated_ts(&catalog.sorted_provider_ids);
                        std::fs::write(&aggregator_path, aggregator).map_err(|error| {
                            node_fs_error(&error, "open", &aggregator_path, None)
                        })?;
                        output.push(
                            "Generated provider catalogs and src/models.generated.ts".to_string(),
                        );
                    }

                    let data_dir = providers_dir.join("data");
                    let had_previous_data = data_dir.exists();
                    if had_previous_data {
                        std::fs::rename(&data_dir, &previous_data_dir).map_err(|error| {
                            node_fs_error(&error, "rename", &data_dir, Some(&previous_data_dir))
                        })?;
                    }
                    // 换入失败时先删掉可能已换入的新 data，再把 previous-data 换回来；
                    // 之后仍由外层统一回滚 TS 分片/聚合器。
                    let swap = (|| -> Result<(), CatalogError> {
                        std::fs::rename(&staged_data_dir, &data_dir).map_err(|error| {
                            node_fs_error(&error, "rename", &staged_data_dir, Some(&data_dir))
                        })?;
                        validate_generated_model_data(package_root)?;
                        Ok(())
                    })();
                    if let Err(error) = swap {
                        let _ = std::fs::remove_dir_all(&data_dir);
                        if had_previous_data && previous_data_dir.exists() {
                            let _ = std::fs::rename(&previous_data_dir, &data_dir);
                        }
                        return Err(error);
                    }
                    Ok(())
                })();
                if let Err(error) = write_phase {
                    // TS 的 `restoreGeneratedCatalog?.()`（快照存在时）。
                    if let Some((shards, aggregator)) = &restore {
                        restore_generated_catalog(&providers_dir, package_root, shards, aggregator);
                    }
                    return Err(error);
                }
                output.push(
                    if options.data_only {
                        "Hydrated JSON model values under src/providers/data/"
                    } else {
                        "Generated JSON model values under src/providers/data/"
                    }
                    .to_string(),
                );
                Ok(())
            })();
            let _ = std::fs::remove_dir_all(&staging_root);
            result?;
        }

        // --json-output 是独立产物：先按 rmSync(recursive, force) 整体删除目标目录，
        // 再按 provider 写 models.json / models.all.json / providers.json 与单 provider 文件。
        if let Some(output_dir) = &options.json_output_dir {
            // `--json-output` 为空会解析成 CWD；readGeneratorOptions 已拒绝，这里再兜底，
            // 避免误删当前目录。
            if output_dir.as_os_str().is_empty() {
                return Err(CatalogError::new("--json-output requires a directory"));
            }
            let provider_output_dir = output_dir.join("providers");
            // `rmSync(dir, { recursive: true, force: true })`：已存在的普通文件也会被删除。
            remove_path_if_exists(output_dir)?;
            std::fs::create_dir_all(&provider_output_dir)
                .map_err(|error| node_fs_error(&error, "mkdir", &provider_output_dir, None))?;
            let chat_value = providers_map_to_json(&catalog.json_chat_providers);
            write_json_file(&output_dir.join("models.json"), &chat_value, options.pretty)?;
            let all_value = providers_all_to_json(&catalog.json_all_providers);
            write_json_file(
                &output_dir.join("models.all.json"),
                &all_value,
                options.pretty,
            )?;
            let providers_value = Value::Array(
                catalog
                    .sorted_provider_ids
                    .iter()
                    .map(|id| Value::String(id.clone()))
                    .collect(),
            );
            write_json_file(
                &output_dir.join("providers.json"),
                &providers_value,
                options.pretty,
            )?;
            for provider_id in &catalog.sorted_provider_ids {
                let chat = map_to_json(&catalog.json_chat_providers[provider_id]);
                write_json_file(
                    &provider_output_dir.join(format!("{provider_id}.json")),
                    &chat,
                    options.pretty,
                )?;
                let all = Value::Array(catalog.json_all_providers[provider_id].clone());
                write_json_file(
                    &provider_output_dir.join(format!("{provider_id}.all.json")),
                    &all,
                    options.pretty,
                )?;
            }
            output.push(format!(
                "Generated JSON model catalog under {}",
                output_dir.display()
            ));
        }

        output.push(String::new());
        output.push("Model Statistics:".to_string());
        output.push(format!(
            "  Total tool-capable models: {}",
            catalog.total_models
        ));
        output.push(format!(
            "  Reasoning-capable models: {}",
            catalog.reasoning_models
        ));
        for (provider, models) in &catalog.providers {
            output.push(format!(
                "  {provider}: {} chat models, {} image models, {} classifier models",
                models.chat.len(),
                models.image.len(),
                models.classifier.len()
            ));
        }
        Ok(())
    }

    fn write_json_file(path: &Path, value: &Value, pretty: bool) -> Result<(), CatalogError> {
        std::fs::write(path, serialize_json(value, pretty))
            .map_err(|error| node_fs_error(&error, "open", path, None))
    }

    /// `rmSync(path, { recursive: true, force: true })`：目录、文件、符号链接都可删除，
    /// 不存在时不报错。
    fn remove_path_if_exists(path: &Path) -> Result<(), CatalogError> {
        match std::fs::symlink_metadata(path) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(node_fs_error(&error, "lstat", path, None)),
            Ok(metadata) => {
                let result = if metadata.is_dir() {
                    std::fs::remove_dir_all(path)
                } else {
                    std::fs::remove_file(path)
                };
                result.map_err(|error| node_fs_error(&error, "rm", path, None))
            }
        }
    }

    fn providers_map_to_json(providers: &IndexMap<String, IndexMap<String, Value>>) -> Value {
        let mut root = Map::new();
        for (provider, models) in providers {
            root.insert(provider.clone(), map_to_json(models));
        }
        Value::Object(root)
    }

    fn providers_all_to_json(providers: &IndexMap<String, Vec<Value>>) -> Value {
        let mut root = Map::new();
        for (provider, models) in providers {
            root.insert(provider.clone(), Value::Array(models.clone()));
        }
        Value::Object(root)
    }

    fn read_dir_names(path: &Path) -> Result<Vec<String>, CatalogError> {
        let entries = std::fs::read_dir(path)
            .map_err(|error| node_fs_error(&error, "scandir", path, None))?;
        Ok(entries
            .filter_map(Result::ok)
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .collect())
    }

    fn restore_generated_catalog(
        providers_dir: &Path,
        package_root: &Path,
        shards: &IndexMap<String, String>,
        aggregator: &str,
    ) {
        // 尽力恢复：所有 IO 错误都被忽略，避免回滚失败掩盖最初的错误。
        if let Ok(entries) = std::fs::read_dir(providers_dir) {
            for entry in entries.filter_map(Result::ok) {
                let name = entry.file_name().to_string_lossy().into_owned();
                if name.ends_with(".models.ts") {
                    let _ = std::fs::remove_file(entry.path());
                }
            }
        }
        for (entry, content) in shards {
            let _ = std::fs::write(providers_dir.join(entry), content);
        }
        let _ = std::fs::write(
            package_root.join("src").join("models.generated.ts"),
            aggregator,
        );
    }

    /// `generatedHeader`。
    pub const GENERATED_HEADER: &str = "// This file is auto-generated by scripts/generate-models.ts\n// Do not edit manually - run 'npm run generate-models' to update\n\n";

    /// `${providerId.toUpperCase().replace(/[^A-Z0-9]+/g, "_")}`：**每个连续的非
    /// `[A-Z0-9]` 字符段**（/g 的正则）压成一个下划线。
    fn catalog_const_prefix(provider_id: &str) -> String {
        let upper = provider_id.to_uppercase();
        let mut out = String::with_capacity(upper.len() + 1);
        let mut in_separator_run = false;
        for ch in upper.chars() {
            if ch.is_ascii_uppercase() || ch.is_ascii_digit() {
                out.push(ch);
                in_separator_run = false;
            } else if !in_separator_run {
                out.push('_');
                in_separator_run = true;
            }
        }
        out
    }

    /// `${providerId.toUpperCase().replace(/[^A-Z0-9]+/g, "_")}_MODELS`。
    pub fn catalog_const_name(provider_id: &str) -> String {
        format!("{}_MODELS", catalog_const_prefix(provider_id))
    }

    /// `_IMAGE_MODELS` 变体。
    pub fn image_catalog_const_name(provider_id: &str) -> String {
        format!("{}_IMAGE_MODELS", catalog_const_prefix(provider_id))
    }

    /// `_CLASSIFIER_MODELS` 变体。
    pub fn classifier_catalog_const_name(provider_id: &str) -> String {
        format!("{}_CLASSIFIER_MODELS", catalog_const_prefix(provider_id))
    }

    /// `providers/<id>.models.ts` 的模板（逐字）。
    pub fn generate_provider_shard(provider_id: &str) -> String {
        let quoted = kiana_json::js_string::js_quote(provider_id);
        let mut output = String::from(GENERATED_HEADER);
        output.push_str(&format!(
            "import values from \"./data/{provider_id}.json\" with {{ type: \"json\" }};\n"
        ));
        output.push_str("import { flattenChatModelCatalog, flattenClassifierModelCatalog, flattenImageModelCatalog, type ChatModelCatalog, type ClassifierModelCatalog, type ImageModelCatalog } from \"../model-catalog.ts\";\n\n");
        output.push_str(&format!(
            "export const {}: ChatModelCatalog<typeof values, {quoted}> =\n\tflattenChatModelCatalog({quoted}, values);\n\n",
            catalog_const_name(provider_id)
        ));
        output.push_str(&format!(
            "export const {}: ImageModelCatalog<typeof values, {quoted}> =\n\tflattenImageModelCatalog({quoted}, values);\n\n",
            image_catalog_const_name(provider_id)
        ));
        output.push_str(&format!(
            "export const {}: ClassifierModelCatalog<typeof values, {quoted}> =\n\tflattenClassifierModelCatalog({quoted}, values);\n",
            classifier_catalog_const_name(provider_id)
        ));
        output
    }

    /// 所有 `providers/<id>.models.ts`（`sortedProviderIds` 顺序）。
    pub fn generate_provider_shards(sorted_provider_ids: &[String]) -> IndexMap<String, String> {
        let mut shards: IndexMap<String, String> = IndexMap::new();
        for provider_id in sorted_provider_ids {
            shards.insert(
                format!("{provider_id}.models.ts"),
                generate_provider_shard(provider_id),
            );
        }
        shards
    }

    /// `src/models.generated.ts` 的模板（逐字）。
    pub fn generate_models_generated_ts(sorted_provider_ids: &[String]) -> String {
        let mut output = String::from(GENERATED_HEADER);
        for provider_id in sorted_provider_ids {
            output.push_str(&format!(
                "import {{ {}, {}, {} }} from \"./providers/{provider_id}.models.ts\";\n",
                classifier_catalog_const_name(provider_id),
                image_catalog_const_name(provider_id),
                catalog_const_name(provider_id)
            ));
        }
        for (const_name, suffix) in [
            ("MODELS", ""),
            ("IMAGE_MODELS", "_IMAGE"),
            ("CLASSIFIER_MODELS", "_CLASSIFIER"),
        ] {
            output.push_str(&format!("\nexport const {const_name}: {{\n"));
            for provider_id in sorted_provider_ids {
                let quoted = kiana_json::js_string::js_quote(provider_id);
                let symbol = match suffix {
                    "" => catalog_const_name(provider_id),
                    "_IMAGE" => image_catalog_const_name(provider_id),
                    _ => classifier_catalog_const_name(provider_id),
                };
                output.push_str(&format!("\treadonly {quoted}: typeof {symbol};\n"));
            }
            output.push_str("} = {\n");
            for provider_id in sorted_provider_ids {
                let quoted = kiana_json::js_string::js_quote(provider_id);
                let symbol = match suffix {
                    "" => catalog_const_name(provider_id),
                    "_IMAGE" => image_catalog_const_name(provider_id),
                    _ => classifier_catalog_const_name(provider_id),
                };
                output.push_str(&format!("\t{quoted}: {symbol},\n"));
            }
            output.push_str("};\n");
        }
        output
    }
}

// ---------------------------------------------------------------------------
// Node `fs` 错误文本（libuv 的 `<CODE>: <strerror>, <syscall> '<path>'[ -> '<dest>']`）
// ---------------------------------------------------------------------------

// 锁定 `node_fs_error` 的 Node/libuv 文本格式（含 EISDIR 无路径、rename 的 ` -> ` 形式）。
#[cfg(test)]
mod fs_error_format {
    use std::path::Path;

    use super::model_data::node_fs_error;

    #[test]
    fn formats_open_and_read_errors_like_node() {
        let error = std::io::Error::from_raw_os_error(libc::ENOENT);
        assert_eq!(
            node_fs_error(
                &error,
                "open",
                Path::new("/p/src/models.generated.ts"),
                None
            ),
            "ENOENT: no such file or directory, open '/p/src/models.generated.ts'"
        );
        // `readFileSync(dir)` 的 EISDIR 不带路径。
        let error = std::io::Error::from_raw_os_error(libc::EISDIR);
        assert_eq!(
            node_fs_error(&error, "read", Path::new("/p/data/a.json"), None),
            "EISDIR: illegal operation on a directory, read"
        );
    }

    #[test]
    fn formats_scandir_mkdtemp_and_rename_errors_like_node() {
        let error = std::io::Error::from_raw_os_error(libc::ENOENT);
        assert_eq!(
            node_fs_error(&error, "scandir", Path::new("/p/src/providers"), None),
            "ENOENT: no such file or directory, scandir '/p/src/providers'"
        );
        assert_eq!(
            node_fs_error(
                &error,
                "mkdtemp",
                Path::new("/p/src/providers/.model-generation-XXXXXX"),
                None
            ),
            "ENOENT: no such file or directory, mkdtemp '/p/src/providers/.model-generation-XXXXXX'"
        );
        assert_eq!(
            node_fs_error(
                &error,
                "rename",
                Path::new("/p/src/providers/data"),
                Some(Path::new("/p/src/providers/staging/previous-data"))
            ),
            "ENOENT: no such file or directory, rename '/p/src/providers/data' -> '/p/src/providers/staging/previous-data'"
        );
        let error = std::io::Error::from_raw_os_error(libc::EACCES);
        assert_eq!(
            node_fs_error(&error, "open", Path::new("/p/out/models.json"), None),
            "EACCES: permission denied, open '/p/out/models.json'"
        );
    }
}
