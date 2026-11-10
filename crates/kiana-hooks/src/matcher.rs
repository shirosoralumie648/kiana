//! matcher 编译与匹配。
//!
//! 匹配语义与 Claude Code / qwen-code 的证据一致：
//! - 空、`*`、`.*` → 匹配全部；
//! - 纯标识符（字母数字、`_`、`.`、`-`、`:`）与 `|` 分隔列表 → **精确名**匹配，
//!   不编译成正则（避免 `.` 被当成通配符）；
//! - 其它形式 → 正则（Rust `regex`，线性时间）；
//! - 事件没有可匹配字段时（`UserPromptSubmit` / `Stop`），任何 matcher 都视为匹配。

use regex::Regex;

/// matcher 编译错误。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum MatcherError {
    /// 正则语法错误。
    #[error("invalid hook matcher regex {pattern:?}: {message}")]
    InvalidRegex {
        /// 原始 matcher。
        pattern: String,
        /// 正则引擎的错误信息。
        message: String,
    },
}

#[derive(Debug, Clone)]
enum MatcherKind {
    All,
    Exact(Vec<String>),
    Regex(Regex),
}

/// 编译后的 matcher。`Clone` 代价低（`regex::Regex` 内部是 `Arc`）。
#[derive(Debug, Clone)]
pub struct HookMatcher {
    pattern: String,
    kind: MatcherKind,
}

impl HookMatcher {
    /// 编译 matcher。`None` / 空白 / `*` / `.*` 都表示匹配全部。
    pub fn compile(pattern: Option<&str>) -> Result<Self, MatcherError> {
        let raw = pattern.unwrap_or("").trim();
        if raw.is_empty() || raw == "*" || raw == ".*" {
            return Ok(Self {
                pattern: raw.to_string(),
                kind: MatcherKind::All,
            });
        }

        // 只含“名字字符”的 matcher 走精确匹配；`.` 允许出现，这样 `mcp.foo`
        // 不会因为正则的 `.` 通配而意外匹配 `mcpXfoo`。允许空白是为了
        // `bash | write` 这类带空格的列表；含 `*` 等字符时仍走正则。
        let simple = raw.chars().all(|ch| {
            ch.is_ascii_alphanumeric()
                || ch.is_whitespace()
                || matches!(ch, '_' | '|' | '-' | '.' | ':')
        });
        if simple {
            let parts: Vec<String> = raw
                .split('|')
                .map(str::trim)
                .filter(|part| !part.is_empty())
                .map(str::to_string)
                .collect();
            let kind = if parts.is_empty() {
                MatcherKind::All
            } else {
                MatcherKind::Exact(parts)
            };
            return Ok(Self {
                pattern: raw.to_string(),
                kind,
            });
        }

        let regex = Regex::new(raw).map_err(|error| MatcherError::InvalidRegex {
            pattern: raw.to_string(),
            message: error.to_string(),
        })?;
        Ok(Self {
            pattern: raw.to_string(),
            kind: MatcherKind::Regex(regex),
        })
    }

    /// 原始 matcher 文本（审计 / 诊断用）。
    pub fn pattern(&self) -> &str {
        &self.pattern
    }

    /// 是否匹配。
    ///
    /// `subject` 为 `None` 表示事件没有可匹配字段，或者调用方没有提供该字段；
    /// 两种情况都按“匹配全部”处理，避免漏跑 hook。
    pub fn matches(&self, subject: Option<&str>) -> bool {
        let Some(subject) = subject else {
            return true;
        };
        match &self.kind {
            MatcherKind::All => true,
            MatcherKind::Exact(names) => names.iter().any(|name| name == subject),
            MatcherKind::Regex(regex) => regex.is_match(subject),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn matcher(pattern: Option<&str>) -> HookMatcher {
        HookMatcher::compile(pattern).expect("matcher compiles")
    }

    #[test]
    fn empty_and_star_match_everything() {
        for pattern in [None, Some(""), Some("  "), Some("*"), Some(".*")] {
            assert!(matcher(pattern).matches(Some("bash")));
            assert!(matcher(pattern).matches(None));
        }
    }

    #[test]
    fn exact_and_pipe_list_do_not_treat_dot_as_wildcard() {
        let mcp = matcher(Some("mcp.foo"));
        assert!(mcp.matches(Some("mcp.foo")));
        assert!(!mcp.matches(Some("mcpXfoo")));

        let list = matcher(Some("bash | write"));
        assert!(list.matches(Some("bash")));
        assert!(list.matches(Some("write")));
        assert!(!list.matches(Some("read")));
    }

    #[test]
    fn regex_patterns_compile_and_match() {
        let regex = matcher(Some("^(bash|powershell)$"));
        assert!(regex.matches(Some("bash")));
        assert!(regex.matches(Some("powershell")));
        assert!(!regex.matches(Some("read")));

        let prefix = matcher(Some("mcp__.*"));
        assert!(prefix.matches(Some("mcp__fs__read")));
        assert!(!prefix.matches(Some("bash")));
    }

    #[test]
    fn invalid_regex_is_a_typed_error() {
        let error = HookMatcher::compile(Some("([")).expect_err("invalid regex");
        match error {
            MatcherError::InvalidRegex { pattern, .. } => assert_eq!(pattern, "(["),
        }
    }

    #[test]
    fn missing_subject_matches_all() {
        assert!(matcher(Some("^bash$")).matches(None));
    }
}
