//! 分层配置的 key path 拼装。
//!
//! 分层配置的 `origins()`、严格诊断与数组合并表都用同一种路径写法：
//!
//! - 对象成员用 `.` 连接：`permissions.rules`；
//! - 数组元素追加下标：`permissions.rules.0`；
//! - key 自身的 `\` 与 `.` 分别转义成 `\\` 与 `\.`，避免 `a.b` 这样的键与嵌套路径
//!   混淆。配置键通常都是标识符，转义只是防御性处理。
//!
//! 这是一份纯字符串工具，不依赖 `serde_json`，也不做校验。

/// 拼一个对象成员的 key path。
pub(crate) fn child_key_path(parent: &str, key: &str) -> String {
    let mut escaped = String::with_capacity(key.len());
    for character in key.chars() {
        match character {
            '\\' => escaped.push_str("\\\\"),
            '.' => escaped.push_str("\\."),
            _ => escaped.push(character),
        }
    }
    if parent.is_empty() {
        escaped
    } else {
        format!("{parent}.{escaped}")
    }
}

/// 拼一个数组下标的 key path。
pub(crate) fn index_key_path(parent: &str, index: usize) -> String {
    if parent.is_empty() {
        index.to_string()
    } else {
        format!("{parent}.{index}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nested_paths_are_dot_separated() {
        assert_eq!(child_key_path("", "permissions"), "permissions");
        assert_eq!(child_key_path("permissions", "rules"), "permissions.rules");
        assert_eq!(
            index_key_path("permissions.rules", 2),
            "permissions.rules.2"
        );
        assert_eq!(index_key_path("", 0), "0");
    }

    #[test]
    fn dots_and_backslashes_in_keys_are_escaped() {
        assert_eq!(child_key_path("", "a.b"), "a\\.b");
        assert_eq!(child_key_path("", "a\\b"), "a\\\\b");
        assert_eq!(child_key_path("root", "a.b"), "root.a\\.b");
    }
}
