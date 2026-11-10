//! 子会话关联与 resume：parent/sidechain 元数据、消息去重。
//!
//! # 关联模型
//!
//! 子代理的会话是一个独立 `SessionManager`（内存或落盘）。它通过两条连接与父会话关联：
//!
//! 1. 子会话 header 的 `parentSession` 指向父会话 id；
//! 2. 子会话第一条 `kiana.subagent.link` 自定义条目记录完整的 [`SubagentLink`]
//!    （parent id、child id、sidechain id、agent 名、depth）。
//!
//! 父会话侧可以再追加 `kiana.subagent.spawned` 条目，记录“我启动过哪个子会话”；这不是
//! 恢复正确性的必要条件（子会话自带链接），但让父 transcript 能重建 spawn 树。
//!
//! # resume 不重复消息
//!
//! `append_message` 会比较当前 transcript 的最后一条消息：内容相同则跳过。resume 时
//! 调用方通常会重放“初始 prompt”，这条规则保证消息数不会翻倍；新消息照常追加。
//! 去重只比较**最后一条**，因此重复的 tool result（不同调用但内容相同）只要不相邻就
//! 不会被误删。

use kiana_agent_core::types::AgentMessage;
use kiana_ai::types::{Message, UserContent, UserMessage, UserRole};
use kiana_session_core::session_manager::{
    NewSessionOptions, SessionEntry, SessionManager, SessionManagerError,
};
use serde::{Deserialize, Serialize};

/// 子会话链接条目的 `customType`。
pub const SUBAGENT_LINK_CUSTOM_TYPE: &str = "kiana.subagent.link";
/// 父会话记录 spawn 的 `customType`。
pub const SUBAGENT_SPAWNED_CUSTOM_TYPE: &str = "kiana.subagent.spawned";

/// 子会话与父会话的关联。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SubagentLink {
    /// 父会话 id。
    pub parent_session_id: String,
    /// 子会话 id。
    pub child_session_id: String,
    /// sidechain id：同一父会话内可区分多个并行子代理。
    pub sidechain_id: String,
    /// agent 定义名。
    pub agent: String,
    /// 嵌套深度（直接子代理为 1）。
    pub depth: u32,
    /// 创建时间（Unix ms）。
    pub created_at_ms: i64,
}

impl SubagentLink {
    /// 新建。
    pub fn new(
        parent_session_id: impl Into<String>,
        child_session_id: impl Into<String>,
        sidechain_id: impl Into<String>,
        agent: impl Into<String>,
        depth: u32,
    ) -> Self {
        Self {
            parent_session_id: parent_session_id.into(),
            child_session_id: child_session_id.into(),
            sidechain_id: sidechain_id.into(),
            agent: agent.into(),
            depth,
            created_at_ms: now_ms(),
        }
    }
}

/// transcript 错误。
#[derive(Debug, thiserror::Error)]
pub enum TranscriptError {
    /// 会话管理器错误。
    #[error(transparent)]
    Session(#[from] SessionManagerError),
    /// resume 的文件里没有链接条目。
    #[error("session `{path}` has no `{custom_type}` entry")]
    MissingLink {
        /// 文件路径。
        path: String,
        /// 期望的 customType。
        custom_type: &'static str,
    },
    /// 链接条目损坏。
    #[error("invalid subagent link entry: {message}")]
    InvalidLink {
        /// 原因。
        message: String,
    },
}

/// 子会话 transcript。
#[derive(Debug)]
pub struct SubagentTranscript {
    manager: SessionManager,
    link: SubagentLink,
    /// 投影后的最后一条消息与消息数。
    ///
    /// `build_session_context` 会克隆整个会话；每条 `MessageEnd` 都重建一次会让
    /// 长运行变成 O(n²)。这里在创建/恢复时算一次，之后增量维护。
    last_message: Option<AgentMessage>,
    message_count: usize,
}

impl SubagentTranscript {
    /// 在内存中创建子会话（永不落盘）。
    pub fn create_in_memory(cwd: &str, link: SubagentLink) -> Result<Self, TranscriptError> {
        let manager = SessionManager::in_memory(
            cwd,
            Some(NewSessionOptions {
                id: Some(link.child_session_id.clone()),
                parent_session: Some(link.parent_session_id.clone()),
            }),
            None,
        )?;
        Self::install_link(manager, link)
    }

    /// 在指定目录创建持久化子会话。
    pub fn create_persistent(
        cwd: &str,
        session_dir: &str,
        link: SubagentLink,
    ) -> Result<Self, TranscriptError> {
        let manager = SessionManager::create(
            cwd,
            Some(session_dir),
            Some(NewSessionOptions {
                id: Some(link.child_session_id.clone()),
                parent_session: Some(link.parent_session_id.clone()),
            }),
        )?;
        Self::install_link(manager, link)
    }

    /// 打开已有子会话并恢复链接。
    pub fn resume(path: &str, cwd_override: Option<&str>) -> Result<Self, TranscriptError> {
        let manager = SessionManager::open(path, None, cwd_override)?;
        let link = find_link(&manager).ok_or_else(|| TranscriptError::MissingLink {
            path: path.to_string(),
            custom_type: SUBAGENT_LINK_CUSTOM_TYPE,
        })?;
        let context = manager.build_session_context();
        Ok(Self {
            manager,
            link,
            last_message: context.messages.last().cloned(),
            message_count: context.messages.len(),
        })
    }

    /// 打开已有文件；文件不存在时按 `link` 新建。
    pub fn open_or_create(
        path: &str,
        cwd: &str,
        link: SubagentLink,
    ) -> Result<Self, TranscriptError> {
        if std::path::Path::new(path).exists() {
            Self::resume(path, Some(cwd))
        } else {
            let session_dir = std::path::Path::new(path)
                .parent()
                .map(|parent| parent.to_string_lossy().to_string())
                .unwrap_or_default();
            Self::create_persistent(cwd, &session_dir, link)
        }
    }

    fn install_link(
        mut manager: SessionManager,
        link: SubagentLink,
    ) -> Result<Self, TranscriptError> {
        let data = serde_json::to_value(&link).map_err(|error| TranscriptError::InvalidLink {
            message: error.to_string(),
        })?;
        manager.append_custom_entry(SUBAGENT_LINK_CUSTOM_TYPE, Some(data))?;
        let context = manager.build_session_context();
        Ok(Self {
            manager,
            link,
            last_message: context.messages.last().cloned(),
            message_count: context.messages.len(),
        })
    }

    /// 链接元数据。
    pub fn link(&self) -> &SubagentLink {
        &self.link
    }

    /// 子会话 id。
    pub fn child_session_id(&self) -> &str {
        &self.link.child_session_id
    }

    /// 父会话 id。
    pub fn parent_session_id(&self) -> &str {
        &self.link.parent_session_id
    }

    /// sidechain id。
    pub fn sidechain_id(&self) -> &str {
        &self.link.sidechain_id
    }

    /// 会话文件路径（内存会话为 `None`）。
    pub fn session_file(&self) -> Option<String> {
        self.manager.get_session_file().map(str::to_string)
    }

    /// 当前投影消息。
    pub fn messages(&self) -> Vec<AgentMessage> {
        self.manager.build_session_context().messages
    }

    /// 消息数量。
    pub fn message_count(&self) -> usize {
        self.message_count
    }

    /// 是否已有消息。
    pub fn has_messages(&self) -> bool {
        self.message_count() > 0
    }

    /// 无条件追加消息，返回追加后的消息数量。
    ///
    /// 正常执行路径用这个；恢复时重放历史用 [`Self::append_message_if_new`]。
    pub fn append_message(&mut self, message: AgentMessage) -> Result<usize, TranscriptError> {
        self.manager.append_message(message.clone())?;
        self.last_message = Some(message);
        self.message_count = self.message_count.saturating_add(1);
        Ok(self.message_count)
    }

    /// 追加消息；与最后一条内容相同（忽略时间戳）时跳过。
    ///
    /// 返回追加后的消息数量。
    pub fn append_message_if_new(
        &mut self,
        message: AgentMessage,
    ) -> Result<usize, TranscriptError> {
        if let Some(last) = &self.last_message
            && messages_equal(last, &message)
        {
            return Ok(self.message_count);
        }
        self.append_message(message)
    }

    /// 追加用户文本消息。
    pub fn append_user_text(&mut self, text: impl Into<String>) -> Result<usize, TranscriptError> {
        self.append_message(user_text_message(text))
    }

    /// 如果 transcript 为空，追加初始 prompt；返回是否写入。
    pub fn seed_initial_prompt(
        &mut self,
        prompt: impl Into<String>,
    ) -> Result<bool, TranscriptError> {
        if self.has_messages() {
            return Ok(false);
        }
        self.append_user_text(prompt)?;
        Ok(true)
    }

    /// 读取底层会话管理器（只读用途）。
    pub fn manager(&self) -> &SessionManager {
        &self.manager
    }
}

/// 在父会话里记录一次 spawn。
pub fn record_spawn_in_parent(
    parent: &mut SessionManager,
    link: &SubagentLink,
) -> Result<String, TranscriptError> {
    let data = serde_json::to_value(link).map_err(|error| TranscriptError::InvalidLink {
        message: error.to_string(),
    })?;
    Ok(parent.append_custom_entry(SUBAGENT_SPAWNED_CUSTOM_TYPE, Some(data))?)
}

fn find_link(manager: &SessionManager) -> Option<SubagentLink> {
    for entry in manager.get_entries() {
        if let SessionEntry::Custom(custom) = entry
            && custom.custom_type == SUBAGENT_LINK_CUSTOM_TYPE
        {
            return custom
                .data
                .and_then(|data| serde_json::from_value::<SubagentLink>(data).ok());
        }
    }
    None
}

fn messages_equal(left: &AgentMessage, right: &AgentMessage) -> bool {
    fn canonical(message: &AgentMessage) -> Option<serde_json::Value> {
        let mut value = serde_json::to_value(message).ok()?;
        // 时间戳是写入时刻，不是消息身份；重放时允许不同。
        if let Some(object) = value.as_object_mut() {
            object.remove("timestamp");
        }
        Some(value)
    }
    match (canonical(left), canonical(right)) {
        (Some(left), Some(right)) => left == right,
        _ => false,
    }
}

fn user_text_message(text: impl Into<String>) -> AgentMessage {
    AgentMessage::Standard(Message::User(UserMessage {
        role: UserRole::User,
        content: UserContent::Text(text.into()),
        timestamp: now_ms(),
    }))
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn link(child: &str) -> SubagentLink {
        SubagentLink::new("parent-1", child, format!("side-{child}"), "explore", 1)
    }

    #[test]
    fn in_memory_transcript_records_parent_and_sidechain_link() {
        let mut transcript =
            SubagentTranscript::create_in_memory("/tmp/work", link("child-1")).expect("create");
        assert_eq!(transcript.parent_session_id(), "parent-1");
        assert_eq!(transcript.sidechain_id(), "side-child-1");
        transcript.append_user_text("hello").expect("append");
        assert_eq!(transcript.message_count(), 1);
        let stored = find_link(transcript.manager()).expect("link entry");
        assert_eq!(stored, *transcript.link());
    }

    #[test]
    fn duplicate_tail_messages_are_skipped() {
        let mut transcript =
            SubagentTranscript::create_in_memory("/tmp/work", link("child-2")).expect("create");
        transcript.append_user_text("same").expect("append");
        transcript
            .append_message_if_new(user_text_message("same"))
            .expect("append duplicate");
        assert_eq!(transcript.message_count(), 1);
        transcript.append_user_text("different").expect("append");
        assert_eq!(transcript.message_count(), 2);
    }

    #[test]
    fn resume_reuses_transcript_without_duplicating_messages() {
        let temp = tempfile::tempdir().expect("tempdir");
        let dir = temp.path().join("sessions");
        std::fs::create_dir_all(&dir).expect("mkdir");
        let path = {
            let mut transcript = SubagentTranscript::create_persistent(
                temp.path().to_str().unwrap(),
                dir.to_str().unwrap(),
                link("child-3"),
            )
            .expect("create");
            transcript
                .append_user_text("initial prompt")
                .expect("prompt");
            transcript.append_user_text("follow-up").expect("follow-up");
            transcript.session_file().expect("session file")
        };
        let before = std::fs::read_to_string(&path).expect("read file");
        let mut resumed =
            SubagentTranscript::resume(&path, Some(temp.path().to_str().unwrap())).expect("resume");
        assert_eq!(resumed.message_count(), 2);
        // resume 后再次调用 seed_initial_prompt：已有消息，不应重放。
        assert!(!resumed.seed_initial_prompt("initial prompt").expect("seed"));
        assert_eq!(resumed.message_count(), 2);
        resumed.append_user_text("new message").expect("new");
        assert_eq!(resumed.message_count(), 3);
        assert!(std::fs::read_to_string(&path).expect("read file").len() > before.len());
    }

    #[test]
    fn incremental_count_matches_the_projection_over_many_appends() {
        let mut transcript =
            SubagentTranscript::create_in_memory("/tmp/work", link("child-many")).expect("create");
        for index in 0..100 {
            transcript
                .append_user_text(format!("message {index}"))
                .expect("append");
        }
        assert_eq!(transcript.message_count(), 100);
        assert_eq!(transcript.messages().len(), 100);
        // 尾部去重走缓存，不需要重建整个投影。
        transcript
            .append_message_if_new(user_text_message("message 99"))
            .expect("duplicate tail");
        assert_eq!(transcript.message_count(), 100);
        assert_eq!(transcript.messages().len(), 100);
    }

    #[test]
    fn seed_initial_prompt_only_fires_once() {
        let mut transcript =
            SubagentTranscript::create_in_memory("/tmp/work", link("child-4")).expect("create");
        assert!(transcript.seed_initial_prompt("start").expect("seed"));
        assert!(!transcript.seed_initial_prompt("start").expect("seed again"));
        assert_eq!(transcript.message_count(), 1);
    }

    #[test]
    fn resume_without_link_entry_fails() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("plain.jsonl");
        let mut manager = SessionManager::open(
            path.to_str().unwrap(),
            None,
            Some(temp.path().to_str().unwrap()),
        )
        .expect("open");
        manager
            .append_message(user_text_message("no link"))
            .expect("append");
        let error = SubagentTranscript::resume(path.to_str().unwrap(), None).unwrap_err();
        assert!(matches!(error, TranscriptError::MissingLink { .. }));
    }

    #[test]
    fn parent_spawn_record_is_readable() {
        let mut parent = SessionManager::in_memory("/tmp/work", None, None).expect("session");
        let link = link("child-5");
        record_spawn_in_parent(&mut parent, &link).expect("record");
        let entries = parent.get_entries();
        let custom = entries
            .iter()
            .find_map(|entry| match entry {
                SessionEntry::Custom(custom)
                    if custom.custom_type == SUBAGENT_SPAWNED_CUSTOM_TYPE =>
                {
                    Some(custom)
                }
                _ => None,
            })
            .expect("spawn entry");
        let data: serde_json::Value = custom.data.clone().expect("data");
        assert_eq!(data["childSessionId"], "child-5");
    }
}
