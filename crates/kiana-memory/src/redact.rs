//! Secret 脱敏。
//!
//! 记忆会进入 system prompt 并跨会话保留，所以任何写入路径都必须先脱敏。这里是**尽力
//! 而为**的启发式（与计划一致）：识别常见 API key/token、Authorization、密码赋值、
//! PEM 私钥。返回的 [`RedactionReport`] 只暴露命中次数与类别，不回显原文，避免审计日志
//! 成为新的泄漏点。
//!
//! 设计取舍：
//!
//! - 只做保守的「明显 secret」替换；普通单词（如 `token_budget`）不会被误伤。
//! - 替换后保留键名（`password=[REDACTED]`），便于模型知道这里曾经有敏感值。
//! - 脱敏不是权限控制：真正的权限/审批在 `service` 层，脱敏只是最后一道数据卫生。

use std::sync::LazyLock;

use regex::Regex;

/// 单条脱敏规则。
struct Rule {
    name: &'static str,
    pattern: Regex,
    replacement: &'static str,
}

fn rule(name: &'static str, pattern: &str, replacement: &'static str) -> Rule {
    Rule {
        name,
        pattern: Regex::new(pattern).expect("static redaction pattern"),
        replacement,
    }
}

static RULES: LazyLock<Vec<Rule>> = LazyLock::new(|| {
    vec![
        rule(
            "pem_private_key",
            r"(?s)-----BEGIN [A-Z0-9 ]*PRIVATE KEY-----.*?-----END [A-Z0-9 ]*PRIVATE KEY-----",
            "[REDACTED_PRIVATE_KEY]",
        ),
        rule(
            "anthropic_key",
            r"sk-ant-[A-Za-z0-9_\-]{20,}",
            "[REDACTED_API_KEY]",
        ),
        rule(
            "openai_key",
            r"\bsk-[A-Za-z0-9_\-]{20,}",
            "[REDACTED_API_KEY]",
        ),
        rule(
            "github_token",
            r"\bgh[pousr]_[A-Za-z0-9]{20,}",
            "[REDACTED_TOKEN]",
        ),
        rule(
            "slack_token",
            r"\bxox[baprs]-[A-Za-z0-9\-]{10,}",
            "[REDACTED_TOKEN]",
        ),
        rule(
            "google_api_key",
            r"\bAIza[0-9A-Za-z_\-]{35}",
            "[REDACTED_API_KEY]",
        ),
        rule(
            "aws_access_key",
            r"\bAKIA[0-9A-Z]{16}\b",
            "[REDACTED_AWS_KEY]",
        ),
        rule(
            "bearer_token",
            r"(?i)\bBearer\s+[A-Za-z0-9._~+/=\-]{12,}",
            "Bearer [REDACTED]",
        ),
        rule(
            "secret_assignment",
            r#"(?i)\b(api[_-]?key|access[_-]?token|auth[_-]?token|token|client[_-]?secret|secret|password|passwd)\b(\s*[:=]\s*)("[^"]*"|'[^']*'|[^\s,;]+)"#,
            "$1$2[REDACTED]",
        ),
        rule(
            "aws_secret_assignment",
            r"(?i)\baws_secret_access_key\b(\s*[:=]\s*)\S+",
            "aws_secret_access_key$1[REDACTED]",
        ),
    ]
});

/// 脱敏结果。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RedactionReport {
    /// 脱敏后的文本。
    pub text: String,
    /// 命中并替换的次数。
    pub replacements: usize,
    /// 命中的规则名（去重、稳定顺序）。
    pub kinds: Vec<&'static str>,
}

impl RedactionReport {
    /// 是否发生了脱敏。
    pub fn redacted(&self) -> bool {
        self.replacements > 0
    }
}

/// 对文本执行全部脱敏规则。
pub fn redact_secrets(input: &str) -> RedactionReport {
    if input.is_empty() {
        return RedactionReport {
            text: String::new(),
            replacements: 0,
            kinds: Vec::new(),
        };
    }
    let mut text = input.to_string();
    let mut replacements = 0usize;
    let mut kinds = Vec::new();
    for rule in RULES.iter() {
        let matches = rule.pattern.find_iter(&text).count();
        if matches == 0 {
            continue;
        }
        text = rule
            .pattern
            .replace_all(&text, rule.replacement)
            .into_owned();
        replacements += matches;
        if !kinds.contains(&rule.name) {
            kinds.push(rule.name);
        }
    }
    RedactionReport {
        text,
        replacements,
        kinds,
    }
}

/// 便捷函数：返回脱敏后的字符串。
pub fn redact(input: &str) -> String {
    redact_secrets(input).text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn common_secrets_are_redacted() {
        let report = redact_secrets(
            "key sk-ant-abcdefghijklmnopqrstuvwxyz012345 password=hunter2\n\
             Authorization: Bearer abcdefghijklmnopqrstuvwxyz\n\
             -----BEGIN RSA PRIVATE KEY-----\nMIIE\n-----END RSA PRIVATE KEY-----",
        );
        assert!(!report.text.contains("hunter2"));
        assert!(!report.text.contains("abcdefghijklmnopqrstuvwxyz"));
        assert!(!report.text.contains("MIIE"));
        assert!(report.replacements >= 3);
    }

    #[test]
    fn ordinary_words_are_untouched() {
        let report = redact_secrets("The token budget is 2000; password rotation is quarterly.");
        assert_eq!(report.replacements, 0);
        assert_eq!(
            report.text,
            "The token budget is 2000; password rotation is quarterly."
        );
    }
}
