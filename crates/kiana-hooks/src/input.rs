//! Hook stdin 的输入模型。

use serde::Serialize;
use serde_json::Value;

use crate::event::HookEvent;

/// 传给 hook 的事件输入。
///
/// 序列化后写入 command hook 的 stdin，或作为 HTTP hook 的 POST body。
/// `None` 字段不会出现在 JSON 里。
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct HookInput {
    /// 会话 ID。
    pub session_id: String,
    /// 会话记录文件路径。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub transcript_path: Option<String>,
    /// 项目根（runner 会强制覆盖成 `HookRunnerConfig::project_root`）。
    pub cwd: String,
    /// 事件名。
    pub hook_event_name: HookEvent,
    /// 当前权限模式（例如 `default` / `accept-edits` / `bypass`）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub permission_mode: Option<String>,
    /// 工具名（工具事件）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_name: Option<String>,
    /// 工具参数（工具事件；`updatedInput` 会沿 hook 顺序更新这个字段）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_input: Option<Value>,
    /// 工具结果（`PostToolUse`）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_response: Option<Value>,
    /// 工具错误文本（`PostToolUseFailure`）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// 用户提交的原始 prompt（`UserPromptSubmit`）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt: Option<String>,
    /// 会话启动来源，例如 `startup` / `resume` / `clear` / `compact`。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    /// 结束 / 拒绝原因（`SessionEnd` / `PermissionDenied`）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// 压缩触发方式：`manual` / `auto`。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trigger: Option<String>,
    /// 压缩自定义指令（`PreCompact`）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub custom_instructions: Option<String>,
}

impl HookInput {
    /// 创建带必填字段的输入。
    pub fn new(event: HookEvent, session_id: impl Into<String>, cwd: impl Into<String>) -> Self {
        Self {
            session_id: session_id.into(),
            transcript_path: None,
            cwd: cwd.into(),
            hook_event_name: event,
            permission_mode: None,
            tool_name: None,
            tool_input: None,
            tool_response: None,
            error: None,
            prompt: None,
            source: None,
            reason: None,
            trigger: None,
            custom_instructions: None,
        }
    }

    /// 设置会话记录路径。
    pub fn with_transcript_path(mut self, path: impl Into<String>) -> Self {
        self.transcript_path = Some(path.into());
        self
    }

    /// 设置权限模式。
    pub fn with_permission_mode(mut self, mode: impl Into<String>) -> Self {
        self.permission_mode = Some(mode.into());
        self
    }

    /// 设置工具名与参数。
    pub fn with_tool_call(
        mut self,
        tool_name: impl Into<String>,
        tool_input: impl Into<Value>,
    ) -> Self {
        self.tool_name = Some(tool_name.into());
        self.tool_input = Some(tool_input.into());
        self
    }

    /// 设置工具结果。
    pub fn with_tool_response(mut self, response: impl Into<Value>) -> Self {
        self.tool_response = Some(response.into());
        self
    }

    /// 设置工具错误文本。
    pub fn with_error(mut self, error: impl Into<String>) -> Self {
        self.error = Some(error.into());
        self
    }

    /// 设置用户 prompt。
    pub fn with_prompt(mut self, prompt: impl Into<String>) -> Self {
        self.prompt = Some(prompt.into());
        self
    }

    /// 设置会话启动来源。
    pub fn with_source(mut self, source: impl Into<String>) -> Self {
        self.source = Some(source.into());
        self
    }

    /// 设置结束 / 拒绝原因。
    pub fn with_reason(mut self, reason: impl Into<String>) -> Self {
        self.reason = Some(reason.into());
        self
    }

    /// 设置压缩触发方式。
    pub fn with_trigger(mut self, trigger: impl Into<String>) -> Self {
        self.trigger = Some(trigger.into());
        self
    }

    /// 设置压缩自定义指令。
    pub fn with_custom_instructions(mut self, instructions: impl Into<String>) -> Self {
        self.custom_instructions = Some(instructions.into());
        self
    }

    /// 当前事件 matcher 应该匹配的字段值。
    pub fn matcher_subject(&self) -> Option<&str> {
        match self.hook_event_name {
            HookEvent::PreToolUse
            | HookEvent::PostToolUse
            | HookEvent::PostToolUseFailure
            | HookEvent::PermissionDenied => self.tool_name.as_deref(),
            HookEvent::SessionStart => self.source.as_deref(),
            HookEvent::SessionEnd => self.reason.as_deref(),
            HookEvent::PreCompact | HookEvent::PostCompact => self.trigger.as_deref(),
            HookEvent::UserPromptSubmit | HookEvent::Stop => None,
        }
    }

    /// 序列化成 JS 语义的一行 JSON（不包含结尾换行）。
    pub(crate) fn to_json_string(&self) -> String {
        match serde_json::to_value(self) {
            Ok(value) => kiana_json::stringify(&value),
            // HookInput 只含 Serialize 的基础类型，这条路径实际不可达；保底返回空对象，
            // 避免因为内部序列化 bug 让整个会话 panic。
            Err(_) => "{}".to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn serializes_expected_fields_and_omits_none() {
        let input = HookInput::new(HookEvent::PreToolUse, "s1", "/project")
            .with_transcript_path("/tmp/t.jsonl")
            .with_tool_call("bash", json!({ "command": "ls" }));
        let value: Value = serde_json::from_str(&input.to_json_string()).unwrap();
        assert_eq!(value["session_id"], "s1");
        assert_eq!(value["cwd"], "/project");
        assert_eq!(value["hook_event_name"], "PreToolUse");
        assert_eq!(value["tool_name"], "bash");
        assert_eq!(value["tool_input"]["command"], "ls");
        assert!(value.get("tool_response").is_none());
        assert!(value.get("prompt").is_none());
    }

    #[test]
    fn matcher_subject_reports_event_field() {
        let tool = HookInput::new(HookEvent::PostToolUseFailure, "s", "/p")
            .with_tool_call("write", json!({}));
        assert_eq!(tool.matcher_subject(), Some("write"));

        let compact = HookInput::new(HookEvent::PreCompact, "s", "/p").with_trigger("manual");
        assert_eq!(compact.matcher_subject(), Some("manual"));

        let prompt = HookInput::new(HookEvent::UserPromptSubmit, "s", "/p").with_prompt("hi");
        assert_eq!(prompt.matcher_subject(), None);
    }
}
