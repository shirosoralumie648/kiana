//! Hook stdout / HTTP body 的 JSON 契约解析。

use serde::Deserialize;
use serde_json::Value;

/// `hookSpecificOutput` 的宽松表示。
///
/// 不同事件只使用其中一部分字段；未识别的字段被忽略。
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HookSpecificOutput {
    /// 事件名。必须与当前事件一致，否则整个 `hookSpecificOutput` 被忽略并告警。
    pub hook_event_name: Option<String>,
    /// `allow` / `deny` / `ask`（`PreToolUse`）。
    pub permission_decision: Option<String>,
    /// 决策原因（`PreToolUse`）。
    pub permission_decision_reason: Option<String>,
    /// 工具参数整体替换（`PreToolUse`）。
    pub updated_input: Option<Value>,
    /// 注入模型上下文的附加文本。
    pub additional_context: Option<String>,
    /// `PermissionDenied`：是否允许模型重试。
    pub retry: Option<bool>,
}

/// Hook 的 JSON 输出。
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HookJsonOutput {
    /// `false` 表示结束当前回合；默认 `true`。
    #[serde(rename = "continue", default)]
    pub continue_execution: Option<bool>,
    /// `continue: false` 时展示给用户/模型的停止原因。
    #[serde(default)]
    pub stop_reason: Option<String>,
    /// 是否隐藏 stdout（不进入 transcript）。
    #[serde(default)]
    pub suppress_output: Option<bool>,
    /// 展示给用户的系统消息。
    #[serde(default)]
    pub system_message: Option<String>,
    /// 通用决策：`approve` / `allow` / `ask` / `block` / `deny`。
    #[serde(default)]
    pub decision: Option<String>,
    /// 决策原因。
    #[serde(default)]
    pub reason: Option<String>,
    /// 事件特定输出。
    #[serde(default)]
    pub hook_specific_output: Option<HookSpecificOutput>,
}

/// stdout 解析结果。
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, PartialEq)]
pub enum ParsedStdout {
    /// 空输出。
    Empty,
    /// 纯文本（不改变决策）。
    Plain(String),
    /// 合法 JSON。
    Json(HookJsonOutput),
    /// 以 `{` 开头但解析/反序列化失败。
    InvalidJson {
        /// 原始文本（已截断）。
        text: String,
        /// 错误信息。
        error: String,
    },
}

/// 解析 command hook 的 stdout / HTTP hook 的 body。
///
/// 只有 `trim` 后以 `{` 开头才尝试 JSON；其它内容原样作为纯文本。JSON 语法错误或
/// 字段类型不符都返回 [`ParsedStdout::InvalidJson`]，由调用方降级为警告。
pub fn parse_stdout(stdout: &str) -> ParsedStdout {
    let trimmed = stdout.trim();
    if trimmed.is_empty() {
        return ParsedStdout::Empty;
    }
    if !trimmed.starts_with('{') {
        return ParsedStdout::Plain(stdout.to_string());
    }
    match kiana_json::parse(trimmed) {
        Ok(value) => match serde_json::from_value::<HookJsonOutput>(value) {
            Ok(output) => ParsedStdout::Json(output),
            Err(error) => ParsedStdout::InvalidJson {
                text: trimmed.to_string(),
                error: format!("hook JSON does not match the output schema: {error}"),
            },
        },
        Err(error) => ParsedStdout::InvalidJson {
            text: trimmed.to_string(),
            error: format!("invalid hook JSON: {error}"),
        },
    }
}

/// XML 转义 `additionalContext`，防止 hook 注入 `<system>` 等标签。
///
/// 顺序必须先是 `&`，否则会二次转义。会话侧应使用
/// [`crate::HookOutcome::additional_context_xml`]，不要自己拼接原始文本。
pub fn escape_additional_context(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            other => escaped.push(other),
        }
    }
    escaped
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_plain_and_empty() {
        assert_eq!(parse_stdout(""), ParsedStdout::Empty);
        assert_eq!(parse_stdout("  \n"), ParsedStdout::Empty);
        assert_eq!(
            parse_stdout("hello world"),
            ParsedStdout::Plain("hello world".to_string())
        );
        // 以 `[` 开头的不是对象，按纯文本处理。
        assert_eq!(
            parse_stdout("[1,2]"),
            ParsedStdout::Plain("[1,2]".to_string())
        );
    }

    #[test]
    fn parses_full_json_contract() {
        let parsed = parse_stdout(
            r#"{
                "continue": false,
                "stopReason": "done",
                "suppressOutput": true,
                "systemMessage": "warn",
                "decision": "block",
                "reason": "no",
                "hookSpecificOutput": {
                    "hookEventName": "PreToolUse",
                    "permissionDecision": "deny",
                    "permissionDecisionReason": "policy",
                    "updatedInput": {"command": "ls"},
                    "additionalContext": "ctx"
                }
            }"#,
        );
        let ParsedStdout::Json(json) = parsed else {
            panic!("expected JSON: {parsed:?}");
        };
        assert_eq!(json.continue_execution, Some(false));
        assert_eq!(json.stop_reason.as_deref(), Some("done"));
        assert_eq!(json.suppress_output, Some(true));
        assert_eq!(json.system_message.as_deref(), Some("warn"));
        assert_eq!(json.decision.as_deref(), Some("block"));
        let specific = json.hook_specific_output.expect("specific output");
        assert_eq!(specific.hook_event_name.as_deref(), Some("PreToolUse"));
        assert_eq!(specific.permission_decision.as_deref(), Some("deny"));
        assert_eq!(
            specific.permission_decision_reason.as_deref(),
            Some("policy")
        );
        assert_eq!(specific.updated_input, Some(json!({"command": "ls"})));
        assert_eq!(specific.additional_context.as_deref(), Some("ctx"));
    }

    #[test]
    fn invalid_json_and_wrong_types_are_reported() {
        assert!(matches!(
            parse_stdout("{\"continue\": tru"),
            ParsedStdout::InvalidJson { .. }
        ));
        // 字段类型不符：continue 应为 bool。
        assert!(matches!(
            parse_stdout(r#"{"continue":"yes"}"#),
            ParsedStdout::InvalidJson { .. }
        ));
    }

    #[test]
    fn escapes_xml_in_the_right_order() {
        assert_eq!(
            escape_additional_context("a & b <tag> \"q\" 'x'"),
            "a &amp; b &lt;tag&gt; \"q\" 'x'"
        );
        assert_eq!(escape_additional_context("&lt;"), "&amp;lt;");
    }
}
