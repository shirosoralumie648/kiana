//! `validate_name` / `validate_description`（JS UTF-16 长度）。
//!
//! 逐字移植 `packages/coding-agent/src/core/skills.ts#validateName` /
//! `#validateDescription`。长度按 JS `string.length`（UTF-16 code unit）计算
//! （`ARCHITECTURE.md` §4.6）。
//!
//! # 为什么是 UTF-16
//! 上限与错误消息都按 UTF-16 code unit 计数，这是稳定契约（测试逐字断言）。JS 的
//! `string.length` 数的是 UTF-16 code unit，补充平面字符（如 emoji）算 2；若用 Rust 的 `chars().count()` 会少算，因此统一走
//! `encode_utf16().count()`。
//!
//! # 调用方与后果
//! `crate::skills::discover::load_skill_from_file` 把这里返回的每条错误转成
//! `ResourceDiagnostic` warning。名字非法**不会**阻止技能加载；description 缺失才会让
//! 该文件不产生技能。
//!
//! # 可同时命中
//! `validate_name` 的四条检查相互独立，按固定顺序追加，因此一个名字可能返回多条错误。

use serde_json::Value;

/// 技能名最大长度（UTF-16 code unit）。TS: `skills.ts#MAX_NAME_LENGTH`。
///
/// 比较的是 UTF-16 code unit 数，不是字节数或 Unicode 标量数；超出不会阻止加载，
/// 调用方只会把它转成 warning。
pub const MAX_NAME_LENGTH: usize = 64;

/// 技能描述最大长度（UTF-16 code unit）。TS: `skills.ts#MAX_DESCRIPTION_LENGTH`。
///
/// 与 [`MAX_NAME_LENGTH`] 同样的单位；超出不会阻止加载，调用方只会把它转成 warning。
pub const MAX_DESCRIPTION_LENGTH: usize = 1024;

/// JS `string.length`：UTF-16 code unit 数。
///
/// 一个补充平面字符（如 emoji）在 UTF-16 中占两个 code unit，因此这里计 2。
fn utf16_len(text: &str) -> usize {
    text.encode_utf16().count()
}

/// JS `/^[a-z0-9-]+$/`（无 `m`，`$` 只匹配输入末尾）是否匹配。
///
/// `+` 要求至少一个字符，所以空串返回 `false`；逐 `char` 检查与该正则等价，因为字符集
/// 之外的字符（包括 `\n`）都会让匹配失败。
fn matches_name_character_class(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// 校验技能名。返回全部错误消息（空数组表示合法）。
///
/// 四条检查相互独立、顺序固定（可同时命中）。TS: `skills.ts#validateName`。
///
/// 顺序为：长度上限 → 字符集 `^[a-z0-9-]+$` → 首尾连字符 → 连续连字符。字符集检查同时
/// 承担“非空”约束：空名字会命中 “invalid characters”。因为对应 JS 正则没有 `m` 标志、
/// `$` 只匹配输入末尾，含换行的名字也会被判非法（测试
/// `name_regex_requires_the_end_of_input_like_js` 锁定）。长度超限不会短路后面的检查。
pub fn validate_name(name: &str) -> Vec<String> {
    let mut errors = Vec::new();
    let length = utf16_len(name);

    if length > MAX_NAME_LENGTH {
        errors.push(format!(
            "name exceeds {MAX_NAME_LENGTH} characters ({length})"
        ));
    }

    if !matches_name_character_class(name) {
        errors.push(
            "name contains invalid characters (must be lowercase a-z, 0-9, hyphens only)"
                .to_string(),
        );
    }

    if name.starts_with('-') || name.ends_with('-') {
        errors.push("name must not start or end with a hyphen".to_string());
    }

    if name.contains("--") {
        errors.push("name must not contain consecutive hyphens".to_string());
    }

    errors
}

/// 校验技能描述。`description` 按 TS 的 `unknown` 处理。
///
/// TS: `skills.ts#validateDescription`。
///
/// - `None`、非字符串 JSON 值、经 JS `trim` 后为空的字符串都会得到
///   `"description is required"`；
/// - 长度检查与必填检查互斥（`else if`）：只有非空字符串才会检查
///   [`MAX_DESCRIPTION_LENGTH`]，因此一次最多返回一条错误。
pub fn validate_description(description: Option<&Value>) -> Vec<String> {
    let mut errors = Vec::new();
    let is_non_empty_string = description
        .and_then(Value::as_str)
        .is_some_and(|text| !kiana_json::js_string::js_trim(text).is_empty());

    // 必填与超长互斥：缺 description 时不再算长度，所以最多只报一条。
    if !is_non_empty_string {
        errors.push("description is required".to_string());
    } else if let Some(text) = description.and_then(Value::as_str) {
        let length = utf16_len(text);
        if length > MAX_DESCRIPTION_LENGTH {
            errors.push(format!(
                "description exceeds {MAX_DESCRIPTION_LENGTH} characters ({length})"
            ));
        }
    }

    errors
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn name_length_counts_utf16_units() {
        // 32 个 emoji = 64 个 UTF-16 code unit：不超过上限。
        let name = "a".to_string() + &"😀".repeat(31);
        assert_eq!(name.encode_utf16().count(), 63);
        assert!(
            !validate_name(&name)
                .iter()
                .any(|error| error.starts_with("name exceeds"))
        );
        // 33 个 emoji = 66 个 code unit。
        let long = "😀".repeat(33);
        assert!(validate_name(&long)[0].starts_with("name exceeds 64 characters (66)"));
    }

    #[test]
    fn name_checks_are_independent_and_ordered() {
        assert_eq!(
            validate_name("-bad--name-"),
            vec![
                "name must not start or end with a hyphen".to_string(),
                "name must not contain consecutive hyphens".to_string(),
            ]
        );
        assert_eq!(
            validate_name("UPPER"),
            vec![
                "name contains invalid characters (must be lowercase a-z, 0-9, hyphens only)"
                    .to_string()
            ]
        );
    }

    #[test]
    fn name_regex_requires_the_end_of_input_like_js() {
        assert!(!validate_name("abc\n").is_empty());
        assert!(!validate_name("abc\n\n").is_empty());
    }

    #[test]
    fn description_requires_a_non_blank_string() {
        assert_eq!(
            validate_description(None),
            vec!["description is required".to_string()]
        );
        assert_eq!(
            validate_description(Some(&Value::String("  ".into()))),
            vec!["description is required".to_string()]
        );
        assert!(validate_description(Some(&Value::String("ok".into()))).is_empty());
        let long = "x".repeat(1025);
        assert_eq!(
            validate_description(Some(&Value::String(long))),
            vec!["description exceeds 1024 characters (1025)".to_string()]
        );
    }
}
