//! 事件、来源与决策枚举。

use std::fmt;
use std::str::FromStr;

/// Hook 事件。
///
/// 变体名就是配置里的 JSON key，也是 stdin 里的 `hook_event_name`。
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
pub enum HookEvent {
    /// 工具执行前。可以 deny / allow / ask，或改写 `updatedInput`。
    PreToolUse,
    /// 工具执行成功后。
    PostToolUse,
    /// 工具执行失败后。
    PostToolUseFailure,
    /// 权限层拒绝工具调用后。
    PermissionDenied,
    /// 用户提交 prompt 时。
    UserPromptSubmit,
    /// 会话开始。
    SessionStart,
    /// 会话结束。
    SessionEnd,
    /// 回合即将结束（模型不再继续）时。
    Stop,
    /// 压缩前。
    PreCompact,
    /// 压缩后。
    PostCompact,
}

impl HookEvent {
    /// 全部事件，顺序稳定，便于 UI / 文档遍历。
    pub const ALL: [HookEvent; 10] = [
        HookEvent::PreToolUse,
        HookEvent::PostToolUse,
        HookEvent::PostToolUseFailure,
        HookEvent::PermissionDenied,
        HookEvent::UserPromptSubmit,
        HookEvent::SessionStart,
        HookEvent::SessionEnd,
        HookEvent::Stop,
        HookEvent::PreCompact,
        HookEvent::PostCompact,
    ];

    /// 配置 / stdin 中的名字。
    pub fn as_str(self) -> &'static str {
        match self {
            HookEvent::PreToolUse => "PreToolUse",
            HookEvent::PostToolUse => "PostToolUse",
            HookEvent::PostToolUseFailure => "PostToolUseFailure",
            HookEvent::PermissionDenied => "PermissionDenied",
            HookEvent::UserPromptSubmit => "UserPromptSubmit",
            HookEvent::SessionStart => "SessionStart",
            HookEvent::SessionEnd => "SessionEnd",
            HookEvent::Stop => "Stop",
            HookEvent::PreCompact => "PreCompact",
            HookEvent::PostCompact => "PostCompact",
        }
    }

    /// 按名字查找事件。先精确匹配，再忽略大小写匹配（配置写错大小写时不静默丢弃）。
    pub fn from_name(name: &str) -> Option<Self> {
        if let Some(event) = Self::ALL.into_iter().find(|event| event.as_str() == name) {
            return Some(event);
        }
        let lower = name.to_ascii_lowercase();
        Self::ALL
            .into_iter()
            .find(|event| event.as_str().to_ascii_lowercase() == lower)
    }

    /// matcher 匹配的 JSON 字段名；返回 `None` 表示该事件没有可匹配字段。
    pub fn matcher_field(self) -> Option<&'static str> {
        match self {
            HookEvent::PreToolUse
            | HookEvent::PostToolUse
            | HookEvent::PostToolUseFailure
            | HookEvent::PermissionDenied => Some("tool_name"),
            HookEvent::SessionStart => Some("source"),
            HookEvent::SessionEnd => Some("reason"),
            HookEvent::PreCompact | HookEvent::PostCompact => Some("trigger"),
            HookEvent::UserPromptSubmit | HookEvent::Stop => None,
        }
    }

    /// 退出码 2 是否表示“阻塞”（而不是只把 stderr 当反馈/警告）。
    pub fn blocks_on_exit_two(self) -> bool {
        matches!(
            self,
            HookEvent::PreToolUse
                | HookEvent::UserPromptSubmit
                | HookEvent::Stop
                | HookEvent::PreCompact
        )
    }

    /// 退出码 2 在非阻塞事件里是否把 stderr 作为模型可见反馈。
    pub fn feedback_on_exit_two(self) -> bool {
        matches!(
            self,
            HookEvent::PostToolUse | HookEvent::PostToolUseFailure | HookEvent::PermissionDenied
        )
    }

    /// 纯文本 stdout（退出码 0）是否作为 `additionalContext` 注入。
    ///
    /// `PreToolUse` 的纯文本只进审计（决策必须走 JSON），与 Claude Code 一致。
    pub fn plain_stdout_is_context(self) -> bool {
        matches!(
            self,
            HookEvent::UserPromptSubmit
                | HookEvent::SessionStart
                | HookEvent::PostToolUse
                | HookEvent::PostToolUseFailure
                | HookEvent::PreCompact
                | HookEvent::PostCompact
        )
    }

    /// `hookSpecificOutput.additionalContext` 是否适用。
    pub fn accepts_additional_context(self) -> bool {
        matches!(
            self,
            HookEvent::PreToolUse
                | HookEvent::UserPromptSubmit
                | HookEvent::SessionStart
                | HookEvent::PostToolUse
                | HookEvent::PostToolUseFailure
                | HookEvent::PreCompact
                | HookEvent::PostCompact
        )
    }

    /// 该事件是否必须有 `tool_name`（缺失时返回 `HookError::InvalidInput`）。
    pub fn requires_tool_name(self) -> bool {
        matches!(
            self,
            HookEvent::PreToolUse
                | HookEvent::PostToolUse
                | HookEvent::PostToolUseFailure
                | HookEvent::PermissionDenied
        )
    }
}

impl fmt::Display for HookEvent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for HookEvent {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::from_name(s).ok_or_else(|| format!("unknown hook event {s:?}"))
    }
}

/// Hook 配置来源。
///
/// 优先级数值越小越先执行：local (0) → project (1) → user (2)。
/// `Local` / `Project` 来自项目目录，项目未受信时必须整体丢弃。
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
pub enum HookSource {
    /// `<project>/.kiana/settings.local.json`。
    Local,
    /// `<project>/.kiana/settings.json`。
    Project,
    /// `~/.kiana/.../settings.json`（用户全局）。
    User,
}

impl HookSource {
    /// 执行顺序：数值小者先执行。
    pub fn priority(self) -> u8 {
        match self {
            HookSource::Local => 0,
            HookSource::Project => 1,
            HookSource::User => 2,
        }
    }

    /// 是否依赖项目受信。
    pub fn requires_trust(self) -> bool {
        matches!(self, HookSource::Local | HookSource::Project)
    }

    /// 配置 / 审计中的来源名。
    pub fn as_str(self) -> &'static str {
        match self {
            HookSource::Local => "local",
            HookSource::Project => "project",
            HookSource::User => "user",
        }
    }
}

impl fmt::Display for HookSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// 聚合后的权限决策。
///
/// 合并顺序为 `Deny > Ask > Allow > Passthrough`；任一 deny 胜出。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum HookDecision {
    /// 没有 hook 给出决策，交给权限层按原策略处理。
    #[default]
    Passthrough,
    /// 至少一个 hook 允许；权限层仍需继续求值（allow 不能绕过 deny 规则）。
    Allow,
    /// 至少一个 hook 要求询问用户。
    Ask,
    /// 至少一个 hook 拒绝。
    Deny,
}

impl HookDecision {
    /// 严重程度；用于 `merge`。
    pub fn rank(self) -> u8 {
        match self {
            HookDecision::Passthrough => 0,
            HookDecision::Allow => 1,
            HookDecision::Ask => 2,
            HookDecision::Deny => 3,
        }
    }

    /// 合并两个决策，返回更严格的那个。
    pub fn merge(self, other: Self) -> Self {
        if other.rank() > self.rank() {
            other
        } else {
            self
        }
    }

    /// 是否是 deny。
    pub fn is_denied(self) -> bool {
        matches!(self, HookDecision::Deny)
    }
}
