//! `sqlite/schema.ts`：把前缀用作 SQLite 标识符之前先校验。
//!
//! # 为什么需要
//! 前缀会被 `format!` 拼进 `CREATE TABLE`/`SELECT` 等语句（见 [`super::migrations`]），
//! 标识符不能用绑定参数代替。把字符集限制为 `[A-Za-z0-9_]` 且首字符非数字，既保证结果是
//! 合法 SQLite 标识符，也排除了引号、分号、空格等注入面；`sqlite_` 是 SQLite 保留给内部
//! 对象的前缀。
//!
//! # 边界
//! - 只接受 ASCII：`é`、中文等都会被拒绝（与 TS 的 `[A-Za-z_]` 字符类一致）。
//! - 空串拒绝；单个 `_` 合法。
//! - 保留前缀用 `to_ascii_lowercase` 做大小写不敏感比较，所以 `SQLITE_x` 也会被拒绝。
//! - 返回 `Err(String)` 而不是错误类型：同目录的 `migrations` 与 `kiana-durable` 各自把它
//!   包装成自己的错误类型；消息是稳定契约（见下），改动需同步测试。

/// 校验前缀（TS: `validateSqliteSchemaPrefix`）。
///
/// 合法：`^[A-Za-z_][A-Za-z0-9_]*$`；保留：忽略大小写以 `sqlite_` 开头。
/// 错误消息是稳定契约（前缀按 `JSON.stringify` 形式引用），改动需同步测试。
///
/// 成功返回 `()`；失败返回两种消息之一：`"Invalid SQLite schema prefix <引用>"` 或
/// `"SQLite schema prefix <引用> is reserved"`。没有错误码，调用方只能按消息区分。
pub fn validate_sqlite_schema_prefix(prefix: &str) -> Result<(), String> {
    if !is_valid_prefix(prefix) {
        return Err(format!(
            "Invalid SQLite schema prefix {}",
            kiana_json::js_string::js_quote(prefix)
        ));
    }
    if prefix.to_ascii_lowercase().starts_with("sqlite_") {
        return Err(format!(
            "SQLite schema prefix {} is reserved",
            kiana_json::js_string::js_quote(prefix)
        ));
    }
    Ok(())
}

/// `/^[A-Za-z_][A-Za-z0-9_]*$/`（只匹配 ASCII；`$` 在 JS 里不匹配尾部换行之外的东西，
/// 而字符集本身排除了换行）。
fn is_valid_prefix(prefix: &str) -> bool {
    let mut characters = prefix.chars();
    let Some(first) = characters.next() else {
        return false;
    };
    if !(first.is_ascii_alphabetic() || first == '_') {
        return false;
    }
    characters.all(|character| character.is_ascii_alphanumeric() || character == '_')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_identifiers() {
        for prefix in ["kiana_", "_private", "app2", "A", "_"] {
            assert!(validate_sqlite_schema_prefix(prefix).is_ok(), "{prefix}");
        }
    }

    #[test]
    fn rejects_invalid_identifiers_with_the_reference_message() {
        for prefix in [
            "",
            "1bad",
            "bad-prefix",
            "bad; DROP TABLE notes",
            "a b",
            "é",
        ] {
            let error = validate_sqlite_schema_prefix(prefix).expect_err(prefix);
            assert_eq!(
                error,
                format!(
                    "Invalid SQLite schema prefix {}",
                    kiana_json::js_string::js_quote(prefix)
                )
            );
        }
    }

    #[test]
    fn rejects_reserved_prefixes_case_insensitively() {
        for prefix in ["sqlite_app", "SQLITE_app", "Sqlite_x"] {
            let error = validate_sqlite_schema_prefix(prefix).expect_err(prefix);
            assert_eq!(
                error,
                format!(
                    "SQLite schema prefix {} is reserved",
                    kiana_json::js_string::js_quote(prefix)
                )
            );
        }
    }
}
