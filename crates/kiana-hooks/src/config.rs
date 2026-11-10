//! Hooks 配置模型与宽松解析。
//!
//! 配置形态：`{ Event: [{ matcher, hooks: [...] }] }`。解析是**按条目宽松**的：
//! 未知事件、非法 matcher、非法 handler 都会生成诊断并跳过该条目，其它条目照常
//! 加载。这样用户 settings 里一个坏 hook 不会让整个会话失去全部 hooks。

use std::collections::BTreeMap;
use std::time::Duration;

use serde_json::Value;
use url::Url;

use crate::event::HookEvent;
use crate::matcher::HookMatcher;
use crate::ssrf;

/// Command hook 默认超时（秒）。
pub const DEFAULT_COMMAND_TIMEOUT: Duration = Duration::from_secs(30);
/// HTTP hook 默认超时（秒）。
pub const DEFAULT_HTTP_TIMEOUT: Duration = Duration::from_secs(30);
/// 配置允许的最大 timeout（秒），防止误写成毫秒（例如 30000 秒）把会话挂住。
const MAX_HOOK_TIMEOUT: Duration = Duration::from_secs(3600);

/// Command handler 配置。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandHookConfig {
    /// shell 命令（`/bin/sh -c` / Windows `cmd /C`）。
    pub command: String,
    /// 超时；默认 [`DEFAULT_COMMAND_TIMEOUT`]。
    pub timeout: Duration,
    /// `true` 时后台执行，结果只进审计，不参与本轮决策。
    pub is_async: bool,
}

/// HTTP handler 配置。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpHookConfig {
    /// POST 目标（http/https）。
    pub url: String,
    /// 额外 header；值中的 `$VAR` 只对 `allowed_env_vars` 生效。
    pub headers: Vec<(String, String)>,
    /// 允许在 header 值中插值的环境变量名。
    pub allowed_env_vars: Vec<String>,
    /// 超时；默认 [`DEFAULT_HTTP_TIMEOUT`]。
    pub timeout: Duration,
}

/// 单个 handler。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HookHandlerConfig {
    /// 本地命令。
    Command(CommandHookConfig),
    /// HTTP POST。
    Http(HttpHookConfig),
}

/// 一个 `{ matcher, hooks }` 组。
#[derive(Debug, Clone)]
pub struct HookMatcherGroup {
    /// 编译后的 matcher；`None` 等价于匹配全部。
    pub matcher: Option<HookMatcher>,
    /// 组内按声明顺序执行的 handler。
    pub hooks: Vec<HookHandlerConfig>,
}

/// 解析后的 hooks 配置。
#[derive(Debug, Clone, Default)]
pub struct HooksConfig {
    events: BTreeMap<HookEvent, Vec<HookMatcherGroup>>,
}

impl HooksConfig {
    /// 从 settings 的 `hooks` 对象解析。
    pub fn from_value(value: &Value) -> HookConfigLoad {
        let mut load = HookConfigLoad::default();
        let Some(object) = value.as_object() else {
            if !value.is_null() {
                load.diagnostics.push(HookConfigDiagnostic::new(
                    None,
                    "hooks must be an object keyed by event name",
                ));
            }
            return load;
        };

        for (name, groups_value) in object {
            let Some(event) = HookEvent::from_name(name) else {
                load.diagnostics.push(HookConfigDiagnostic::new(
                    None,
                    format!("unknown hook event {name:?}; entry ignored"),
                ));
                continue;
            };
            let Some(groups) = groups_value.as_array() else {
                load.diagnostics.push(HookConfigDiagnostic::new(
                    Some(event),
                    "event hooks must be an array; entry ignored",
                ));
                continue;
            };
            for (index, group_value) in groups.iter().enumerate() {
                if let Some(group) = parse_group(event, group_value, &mut load.diagnostics) {
                    load.config.events.entry(event).or_default().push(group);
                } else {
                    load.diagnostics.push(HookConfigDiagnostic::new(
                        Some(event),
                        format!("hooks[{index}] is invalid; entry ignored"),
                    ));
                }
            }
        }
        load
    }

    /// 从完整 settings 对象读取 `hooks` 字段。
    pub fn from_settings(settings: &Value) -> HookConfigLoad {
        match settings.get("hooks") {
            Some(value) => Self::from_value(value),
            None => HookConfigLoad::default(),
        }
    }

    /// 某事件的 matcher 组（声明顺序）。
    pub fn event_groups(&self, event: HookEvent) -> &[HookMatcherGroup] {
        self.events.get(&event).map(Vec::as_slice).unwrap_or(&[])
    }

    /// 是否没有任何 hook。
    pub fn is_empty(&self) -> bool {
        self.events.values().all(Vec::is_empty)
    }

    /// 已配置的事件及其组数。
    pub fn event_counts(&self) -> impl Iterator<Item = (HookEvent, usize)> + '_ {
        self.events
            .iter()
            .map(|(event, groups)| (*event, groups.len()))
    }
}

/// 解析结果 + 诊断。
#[derive(Debug, Clone, Default)]
pub struct HookConfigLoad {
    /// 成功解析的配置。
    pub config: HooksConfig,
    /// 被跳过的条目 / 宽松解析发现的问题。
    pub diagnostics: Vec<HookConfigDiagnostic>,
}

impl HookConfigLoad {
    /// 是否没有任何可执行 hook。
    pub fn is_empty(&self) -> bool {
        self.config.is_empty()
    }
}

/// 配置诊断。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HookConfigDiagnostic {
    /// 关联事件（能定位时）。
    pub event: Option<HookEvent>,
    /// 人类可读原因。
    pub message: String,
}

impl HookConfigDiagnostic {
    fn new(event: Option<HookEvent>, message: impl Into<String>) -> Self {
        Self {
            event,
            message: message.into(),
        }
    }
}

fn parse_group(
    event: HookEvent,
    value: &Value,
    diagnostics: &mut Vec<HookConfigDiagnostic>,
) -> Option<HookMatcherGroup> {
    let object = value.as_object()?;

    let matcher = match object.get("matcher") {
        None | Some(Value::Null) => None,
        Some(Value::String(pattern)) => match HookMatcher::compile(Some(pattern)) {
            Ok(matcher) => Some(matcher),
            Err(error) => {
                diagnostics.push(HookConfigDiagnostic::new(
                    Some(event),
                    format!("invalid matcher: {error}"),
                ));
                return None;
            }
        },
        Some(_) => {
            diagnostics.push(HookConfigDiagnostic::new(
                Some(event),
                "matcher must be a string",
            ));
            return None;
        }
    };

    let Some(hooks_value) = object.get("hooks") else {
        diagnostics.push(HookConfigDiagnostic::new(
            Some(event),
            "matcher group is missing `hooks`",
        ));
        return None;
    };
    let Some(hooks_array) = hooks_value.as_array() else {
        diagnostics.push(HookConfigDiagnostic::new(
            Some(event),
            "`hooks` must be an array",
        ));
        return None;
    };

    let mut hooks = Vec::new();
    for (index, hook_value) in hooks_array.iter().enumerate() {
        match parse_handler(hook_value) {
            Ok(handler) => hooks.push(handler),
            Err(message) => diagnostics.push(HookConfigDiagnostic::new(
                Some(event),
                format!("hooks[{index}]: {message}; hook ignored"),
            )),
        }
    }

    if hooks.is_empty() {
        return None;
    }
    Some(HookMatcherGroup { matcher, hooks })
}

fn parse_handler(value: &Value) -> Result<HookHandlerConfig, String> {
    let object = value
        .as_object()
        .ok_or_else(|| "hook must be an object".to_string())?;
    let kind = match object.get("type") {
        None => "command",
        Some(Value::String(kind)) => kind.as_str(),
        Some(_) => return Err("`type` must be a string".to_string()),
    };
    match kind {
        "command" => {
            let command = object
                .get("command")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|command| !command.is_empty())
                .ok_or_else(|| "command hook requires a non-empty `command`".to_string())?;
            let timeout = parse_timeout(object.get("timeout"), DEFAULT_COMMAND_TIMEOUT)?;
            let is_async = match object.get("async") {
                None | Some(Value::Null) => false,
                Some(Value::Bool(value)) => *value,
                Some(_) => return Err("`async` must be a boolean".to_string()),
            };
            Ok(HookHandlerConfig::Command(CommandHookConfig {
                command: command.to_string(),
                timeout,
                is_async,
            }))
        }
        "http" => {
            let url = object
                .get("url")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|url| !url.is_empty())
                .ok_or_else(|| "http hook requires a non-empty `url`".to_string())?;
            let parsed: Url =
                ssrf::validate_url(url).map_err(|error| format!("invalid http url: {error}"))?;

            let headers = parse_headers(object.get("headers"))?;
            let allowed_env_vars = parse_allowed_env_vars(object)?;
            let timeout = parse_timeout(object.get("timeout"), DEFAULT_HTTP_TIMEOUT)?;
            Ok(HookHandlerConfig::Http(HttpHookConfig {
                url: parsed.to_string(),
                headers,
                allowed_env_vars,
                timeout,
            }))
        }
        other => Err(format!("unsupported hook type {other:?}")),
    }
}

fn parse_headers(value: Option<&Value>) -> Result<Vec<(String, String)>, String> {
    let Some(value) = value else {
        return Ok(Vec::new());
    };
    if value.is_null() {
        return Ok(Vec::new());
    }
    let object = value
        .as_object()
        .ok_or_else(|| "`headers` must be an object of string values".to_string())?;
    let mut headers = Vec::with_capacity(object.len());
    for (name, value) in object {
        let value = value
            .as_str()
            .ok_or_else(|| format!("header {name:?} must be a string"))?;
        headers.push((name.clone(), value.to_string()));
    }
    Ok(headers)
}

fn parse_allowed_env_vars(object: &serde_json::Map<String, Value>) -> Result<Vec<String>, String> {
    let value = object
        .get("allowedEnvVars")
        .or_else(|| object.get("allowed_env_vars"));
    let Some(value) = value else {
        return Ok(Vec::new());
    };
    if value.is_null() {
        return Ok(Vec::new());
    }
    let array = value
        .as_array()
        .ok_or_else(|| "`allowedEnvVars` must be an array of strings".to_string())?;
    let mut names = Vec::with_capacity(array.len());
    for name in array {
        let name = name
            .as_str()
            .ok_or_else(|| "`allowedEnvVars` entries must be strings".to_string())?;
        names.push(name.to_string());
    }
    Ok(names)
}

fn parse_timeout(value: Option<&Value>, default: Duration) -> Result<Duration, String> {
    let Some(value) = value else {
        return Ok(default);
    };
    if value.is_null() {
        return Ok(default);
    }
    let Some(seconds) = value.as_f64() else {
        return Err("`timeout` must be a number of seconds".to_string());
    };
    if !seconds.is_finite() || seconds <= 0.0 {
        return Err("`timeout` must be a positive number of seconds".to_string());
    }
    let timeout = Duration::from_secs_f64(seconds);
    if timeout > MAX_HOOK_TIMEOUT {
        return Err(format!(
            "`timeout` must be at most {} seconds",
            MAX_HOOK_TIMEOUT.as_secs()
        ));
    }
    Ok(timeout)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_command_and_http_handlers() {
        let load = HooksConfig::from_value(&json!({
            "PreToolUse": [{
                "matcher": "bash|write",
                "hooks": [
                    {"type": "command", "command": "guard.sh", "timeout": 5, "async": true},
                    {"type": "http", "url": "http://127.0.0.1:8080/hook",
                     "headers": {"Authorization": "Bearer $TOKEN"},
                     "allowedEnvVars": ["TOKEN"]}
                ]
            }]
        }));
        assert!(load.diagnostics.is_empty(), "{:?}", load.diagnostics);
        let groups = load.config.event_groups(HookEvent::PreToolUse);
        assert_eq!(groups.len(), 1);
        assert!(groups[0].matcher.as_ref().unwrap().matches(Some("bash")));
        assert_eq!(groups[0].hooks.len(), 2);
        match &groups[0].hooks[0] {
            HookHandlerConfig::Command(command) => {
                assert_eq!(command.command, "guard.sh");
                assert_eq!(command.timeout, Duration::from_secs(5));
                assert!(command.is_async);
            }
            other => panic!("unexpected handler: {other:?}"),
        }
        match &groups[0].hooks[1] {
            HookHandlerConfig::Http(http) => {
                assert_eq!(http.allowed_env_vars, vec!["TOKEN"]);
                assert_eq!(http.timeout, DEFAULT_HTTP_TIMEOUT);
            }
            other => panic!("unexpected handler: {other:?}"),
        }
    }

    #[test]
    fn defaults_type_to_command_and_timeout_to_30s() {
        let load = HooksConfig::from_value(&json!({
            "Stop": [{"hooks": [{"command": "notify"}]}]
        }));
        assert!(load.diagnostics.is_empty());
        let group = &load.config.event_groups(HookEvent::Stop)[0];
        match &group.hooks[0] {
            HookHandlerConfig::Command(command) => {
                assert_eq!(command.timeout, DEFAULT_COMMAND_TIMEOUT);
                assert!(!command.is_async);
            }
            other => panic!("unexpected handler: {other:?}"),
        }
    }

    #[test]
    fn bad_entries_are_skipped_with_diagnostics() {
        let load = HooksConfig::from_value(&json!({
            "NotAnEvent": [],
            "PreToolUse": [
                {"matcher": "([", "hooks": [{"command": "never"}]},
                {"matcher": 3, "hooks": [{"command": "never"}]},
                {"hooks": [{"type": "mystery"}, {"command": ""}, {"command": "ok"}]}
            ]
        }));
        assert_eq!(load.config.event_groups(HookEvent::PreToolUse).len(), 1);
        let group = &load.config.event_groups(HookEvent::PreToolUse)[0];
        assert_eq!(group.hooks.len(), 1);
        assert!(load.diagnostics.len() >= 4, "{:?}", load.diagnostics);
    }

    #[test]
    fn http_url_is_validated_at_load_time() {
        for url in [
            "file:///etc/passwd",
            "http://169.254.169.254/latest",
            "http://10.0.0.1/hook",
        ] {
            let load = HooksConfig::from_value(&json!({
                "SessionStart": [{"hooks": [{"type": "http", "url": url}]}]
            }));
            assert!(
                load.config.event_groups(HookEvent::SessionStart).is_empty(),
                "{url} should be rejected"
            );
            assert!(!load.diagnostics.is_empty());
        }
    }

    #[test]
    fn timeout_must_be_positive_and_bounded() {
        let load = HooksConfig::from_value(&json!({
            "Stop": [{"hooks": [
                {"command": "a", "timeout": 0},
                {"command": "b", "timeout": -1},
                {"command": "c", "timeout": 4000},
                {"command": "d", "timeout": "30"}
            ]}]
        }));
        assert!(load.config.event_groups(HookEvent::Stop).is_empty());
        // 4 条无效 handler + 1 条“组内没有有效 hook”。
        assert!(load.diagnostics.len() >= 4, "{:?}", load.diagnostics);
    }

    #[test]
    fn from_settings_reads_hooks_key() {
        let load = HooksConfig::from_settings(&json!({
            "model": "x",
            "hooks": {"SessionEnd": [{"hooks": [{"command": "cleanup"}]}]}
        }));
        assert_eq!(load.config.event_groups(HookEvent::SessionEnd).len(), 1);
    }
}
