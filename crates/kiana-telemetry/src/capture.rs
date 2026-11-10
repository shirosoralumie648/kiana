//! 内容捕获开关（GenAI 语义约定）。
//!
//! Prompt、tool 参数/结果默认**不记录**：遥测是诊断，不应把用户内容写进 trace/log。
//! 只有显式设置
//!
//! ```text
//! OTEL_INSTRUMENTATION_GENAI_CAPTURE_MESSAGE_CONTENT=true
//! ```
//!
//! 才允许捕获。变量值大小写不敏感，只有 `true`（去掉首尾空白）打开；其它值、
//! 空串、未设置一律关闭。这与 OpenTelemetry GenAI 语义约定的环境变量一致。
//!
//! `ContentCapture` 是纯值类型，调用方可以在启动时解析一次并传给 `SessionTelemetry`；
//! 测试也可以构造 `ContentCapture::enabled()` 而不改进程环境。

/// 内容捕获环境变量名。
pub const CAPTURE_MESSAGE_CONTENT_ENV: &str = "OTEL_INSTRUMENTATION_GENAI_CAPTURE_MESSAGE_CONTENT";

/// 内容捕获开关。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ContentCapture {
    enabled: bool,
}

impl ContentCapture {
    /// 默认关闭。
    pub fn disabled() -> Self {
        ContentCapture { enabled: false }
    }

    /// 显式打开。
    pub fn enabled() -> Self {
        ContentCapture { enabled: true }
    }

    /// 从环境变量解析。
    pub fn from_env() -> Self {
        ContentCapture::from_value(std::env::var(CAPTURE_MESSAGE_CONTENT_ENV).ok().as_deref())
    }

    /// 从字符串值解析（`true` 大小写不敏感；其它值关闭）。
    pub fn from_value(value: Option<&str>) -> Self {
        match value {
            Some(value) if value.trim().eq_ignore_ascii_case("true") => ContentCapture::enabled(),
            _ => ContentCapture::disabled(),
        }
    }

    /// 是否开启。
    pub fn is_enabled(self) -> bool {
        self.enabled
    }

    /// 开启时返回内容本身，关闭时返回 `None`。
    pub fn maybe<'a>(self, value: &'a str) -> Option<&'a str> {
        if self.enabled { Some(value) } else { None }
    }

    /// 开启时返回内容的克隆，关闭时返回 `None`。
    pub fn maybe_string(self, value: &str) -> Option<String> {
        if self.enabled {
            Some(value.to_string())
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_true_enables_capture() {
        assert!(ContentCapture::from_value(Some("true")).is_enabled());
        assert!(ContentCapture::from_value(Some("TRUE")).is_enabled());
        assert!(ContentCapture::from_value(Some(" true ")).is_enabled());
        for value in [None, Some(""), Some("1"), Some("yes"), Some("false")] {
            assert!(!ContentCapture::from_value(value).is_enabled(), "{value:?}");
        }
    }

    #[test]
    fn disabled_never_returns_content() {
        let capture = ContentCapture::disabled();
        assert_eq!(capture.maybe("secret prompt"), None);
        assert_eq!(capture.maybe_string("secret prompt"), None);
        let capture = ContentCapture::enabled();
        assert_eq!(capture.maybe("secret prompt"), Some("secret prompt"));
        assert_eq!(
            capture.maybe_string("secret prompt"),
            Some("secret prompt".to_string())
        );
    }
}
