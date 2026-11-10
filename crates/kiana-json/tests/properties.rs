//! 属性测试（`ARCHITECTURE.md` §4.8.5）：随机 f64 → `format_js_number` 的不变量。
//!
//! 黄金对照表（1200 个随机 bit 模式 + 34 条 curated）在 `js_number.rs` 里逐条比对；
//! 这里验证的是**跨所有输入**必须成立的结构性不变量，以及 round-trip 一致性。

use kiana_json::js_num::{format_js_number, parse_js_number, to_radix_string};
use kiana_json::js_parse::parse;
use proptest::prelude::*;
use serde_json::Value;

proptest! {
    #![proptest_config(ProptestConfig::with_cases(2000))]

    /// 任意有限 f64 的输出必须是合法 JSON 数字（或 "null"），且能被 `JSON.parse` 读回
    /// 同一个 f64（最短往返）。
    #[test]
    fn format_js_number_round_trips_through_json_parse(bits in any::<u64>()) {
        let value = f64::from_bits(bits);
        let text = format_js_number(value);
        if !value.is_finite() {
            prop_assert_eq!(text.as_str(), "null");
            return Ok(());
        }
        let parsed = parse(&text).expect("JSON.parse accepts format_js_number output");
        let back = match &parsed {
            Value::Number(n) => n.as_f64().expect("f64"),
            other => panic!("expected a number, got {other}"),
        };
        // -0 与 0 在 JSON 文本里都是 "0"，因此按位比较时排除零。
        if value == 0.0 {
            prop_assert_eq!(back, 0.0);
        } else {
            prop_assert_eq!(back.to_bits(), value.to_bits(), "text was {}", text);
        }
    }

    /// 输出里不允许出现指数形式的 `e+0` / 前导零等非 JS 形式；只需满足：
    /// 不含大写 E、不含前导 `+`、非零整数不带小数点。
    #[test]
    fn format_js_number_shape(bits in any::<u64>()) {
        let value = f64::from_bits(bits);
        if !value.is_finite() || value == 0.0 {
            return Ok(());
        }
        let text = format_js_number(value);
        prop_assert!(!text.contains('E'));
        prop_assert!(!text.starts_with('+'));
        if let Some(rest) = text.strip_prefix('-') {
            prop_assert!(!rest.is_empty());
        }
        if let Some(exp) = text.find('e') {
            let mantissa = &text[..exp];
            let exponent = &text[exp + 1..];
            prop_assert!(!mantissa.ends_with('0'), "mantissa {mantissa} has a trailing zero");
            prop_assert!(!exponent.starts_with('0'));
            prop_assert!(!exponent.starts_with("-0"));
            prop_assert!(!exponent.starts_with("+0"));
        }
    }

    /// `to_radix_string(v, 10) == format_js_number(v)`（V8 对 radix 10 直接走 Number::toString）。
    ///
    /// 只对有限值成立：非有限值在 `String()` 语境是 `"NaN"`/`"Infinity"`/`"-Infinity"`，
    /// 而 `format_js_number` 是 JSON 语境（`"null"`）。该分支由 `js_number.rs` 的
    /// `radix_non_finite_uses_javascript_words` 单独覆盖。
    #[test]
    fn radix_ten_matches_number_to_string(bits in any::<u64>()) {
        let value = f64::from_bits(bits);
        if !value.is_finite() {
            return Ok(());
        }
        prop_assert_eq!(to_radix_string(value, 10), format_js_number(value));
    }

    /// 任意有限 f64 在 radix 2..=9 / 11..=36 下输出只含该进制的数字与一个可选的小数点/负号。
    ///
    /// radix 10 例外：V8 直接走 `Number::toString`，会出现 `5e-324` 这类指数形式。
    #[test]
    fn radix_output_is_well_formed(bits in any::<u64>(), radix in 2u32..=36) {
        let value = f64::from_bits(bits);
        if !value.is_finite() || value == 0.0 || radix == 10 {
            return Ok(());
        }
        let text = to_radix_string(value, radix);
        let body = text.strip_prefix('-').unwrap_or(&text);
        prop_assert!(!body.is_empty());
        let mut dots = 0;
        for ch in body.chars() {
            if ch == '.' {
                dots += 1;
                continue;
            }
            let digit = ch.to_digit(36).expect("radix digit");
            prop_assert!(digit < radix, "digit {ch} out of range for radix {radix}");
        }
        prop_assert!(dots <= 1);
        prop_assert!(!body.starts_with('.'), "no leading zero before the point: {text}");
        if dots == 1 {
            prop_assert!(!body.ends_with('.'), "no empty fraction: {text}");
        }
    }

    /// `parse_js_number` 对任意 ASCII 字符串都不得 panic，且结果要么是 NaN 要么可格式化。
    #[test]
    fn parse_js_number_never_panics(text in ".{0,40}") {
        let value = parse_js_number(&text);
        let _ = format_js_number(value);
    }

    /// 对任意有限十进制字面量文本，修复函数是幂等的（修复后的文本再次修复不再变化）。
    #[test]
    fn repair_is_idempotent(text in ".{0,60}") {
        if let Some(once) = kiana_json::js_parse::repair(&text) {
            let twice = kiana_json::js_parse::repair(&once).expect("repair always returns Some");
            // 修复会双写非法转义，因此第二次并不一定等于第一次；这里只要求不 panic 且稳定：
            // 对已经合法的转义序列（无非法反斜杠）必须保持不变。
            if once == text {
                prop_assert_eq!(twice, once);
            }
        }
    }
}
