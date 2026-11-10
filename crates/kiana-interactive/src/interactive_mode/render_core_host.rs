//! 第 12 周渲染核心的生产接线：乐观用户消息队列。
//!
//! [`crate::render_core::pending_user::PendingUserQueue`] 在 week 12 只作为独立模型存在。
//! 本模块把它接到交互模式：
//!
//! * 构造 `InteractiveMode` 时**读一次** [`crate::render_core::experimental_enabled`]
//!   （`KIANA_NEXT_RENDER_CORE`），运行期不再读环境变量，避免半新半旧；
//! * 用户提交时 `note_submission` 入队（显示文本 = 用户输入，匹配键 = 实际发送内容）；
//! * `MessageStart` 的 user 回显调用 `note_echo`：按 trimmed 内容与服务端时间戳围栏
//!   消费，避免同一条消息回显两次；
//! * 10ms 宿主循环调用 `tick`：150s 无回显转可重试错误；
//! * `update_pending_messages_display` 读取 `pending_lines`，把尚未回显的消息显示在
//!   排队消息容器里（与 steering/follow-up 同一区域）。
//!
//! 开关关闭时所有方法都是 no-op，默认渲染路径完全不变。

use std::path::PathBuf;

use kiana_agent_core::types::AgentMessage;
use kiana_ai::types::Message;

use crate::render_core::i18n::{LocaleLoader, LocalePack, Translator, builtin_pack};
use crate::render_core::pending_user::{PendingStatus, PendingUserQueue};

fn english_translator() -> Translator {
    let pack = builtin_pack("en").unwrap_or_else(|| LocalePack::empty("en"));
    Translator::new(pack.clone(), pack)
}

/// 渲染核心在交互模式里的宿主状态。
pub(crate) struct RenderCoreHost {
    enabled: bool,
    pending_users: PendingUserQueue,
    /// 最近一次提交的 pending id（prompt 失败时标记错误）。
    last_submission: Option<String>,
    /// UI 文案翻译器（默认英文；构造时按 settings 的 `ui.locale` / `language` 加载）。
    translator: Translator,
}

impl RenderCoreHost {
    /// 显式开关（单元测试/宿主注入）。
    pub(crate) fn new(enabled: bool) -> Self {
        Self {
            enabled,
            pending_users: PendingUserQueue::new(),
            last_submission: None,
            translator: english_translator(),
        }
    }

    /// 按 locale 加载翻译器（用户目录 > 内置资源 > 英文回退）。
    ///
    /// 用户目录是 `~/.kiana/locales`；内置目录留空，回退到编译进二进制的 en/zh 资源。
    pub(crate) fn with_locale(mut self, locale: &str, user_dir: PathBuf) -> Self {
        self.set_locale(locale, user_dir);
        self
    }

    /// 运行期切换 locale：用与 [`Self::with_locale`] 相同的加载逻辑重建翻译器。
    ///
    /// 只替换 `translator`，pending 队列等其它状态保持不变；调用方负责刷新已渲染的
    /// 文案（见 `InteractiveMode::apply_ui_locale`）。
    pub(crate) fn set_locale(&mut self, locale: &str, user_dir: PathBuf) {
        let loader = LocaleLoader::new(user_dir, PathBuf::new());
        self.translator = loader.load_translator(locale);
    }

    /// 当前翻译器的克隆（审批卡/模态的接线层持有自己的副本）。
    pub(crate) fn translator(&self) -> Translator {
        self.translator.clone()
    }

    /// 当前 UI locale。
    pub(crate) fn locale(&self) -> &str {
        self.translator.locale()
    }

    /// 读取一次开关并构造（默认开启；`KIANA_NEXT_RENDER_CORE=0` 关闭）。
    pub(crate) fn from_env() -> Self {
        Self::new(crate::render_core::pending_user_enabled())
    }

    /// 开关状态（构造时读取，运行期不变）。
    pub(crate) fn enabled(&self) -> bool {
        self.enabled
    }

    /// 记录一次用户提交；返回本地 pending id（开关关闭时为 `None`）。
    pub(crate) fn note_submission(
        &mut self,
        conversation_id: &str,
        text: &str,
        content: Option<&str>,
        now_ms: i64,
    ) -> Option<String> {
        if !self.enabled {
            return None;
        }
        let id = self.pending_users.enqueue(
            conversation_id,
            text,
            content,
            Vec::new(),
            Vec::new(),
            now_ms,
        );
        self.last_submission = Some(id.clone());
        Some(id)
    }

    /// 用户消息被服务端/会话回显：按内容与时间戳围栏消费。
    pub(crate) fn note_echo(&mut self, conversation_id: &str, message: &AgentMessage) -> bool {
        if !self.enabled {
            return false;
        }
        let Some(text) = user_message_text(message) else {
            return false;
        };
        let timestamp = message.timestamp().unwrap_or_default();
        let event_id = format!("user-message-{timestamp}");
        let consumed = self
            .pending_users
            .consume_matching(conversation_id, &text, &event_id, timestamp)
            .is_some();
        if consumed {
            self.last_submission = None;
        }
        consumed
    }

    /// prompt 失败：把最近一次提交标成可重试错误。
    pub(crate) fn note_prompt_error(&mut self, message: &str) -> bool {
        if !self.enabled {
            return false;
        }
        let Some(id) = self.last_submission.take() else {
            return false;
        };
        self.pending_users.mark_error(&id, message);
        true
    }

    /// 推进 watchdog；返回本次变为错误的条数。
    pub(crate) fn tick(&mut self, now_ms: i64) -> usize {
        if !self.enabled {
            return 0;
        }
        self.pending_users.tick(now_ms)
    }

    /// 清空队列（会话切换）。
    pub(crate) fn clear(&mut self) {
        self.pending_users.clear();
        self.last_submission = None;
    }

    /// 某个会话尚未回显的消息展示行。
    pub(crate) fn pending_lines(&self, conversation_id: &str) -> Vec<String> {
        if !self.enabled {
            return Vec::new();
        }
        self.pending_users
            .messages_for(conversation_id)
            .into_iter()
            .map(|pending| match &pending.status {
                PendingStatus::Sending => format!(
                    "{} {}",
                    self.translator.t("pending.sending", &[]),
                    pending.text
                ),
                PendingStatus::Error { message } => format!(
                    "{} {} ({})",
                    self.translator
                        .t("pending.failed", &[("error", message.as_str())]),
                    pending.text,
                    self.translator.t("pending.retry", &[])
                ),
            })
            .collect()
    }
}

impl crate::interactive_mode::InteractiveMode {
    /// 记录用户提交（`run()` 主循环在 `session.prompt` 之前调用）。
    pub(crate) fn note_pending_user_submission(&mut self, text: &str) {
        let conversation_id = self.session().session_id();
        let now = kiana_ai::model_catalog::now_epoch_millis() as i64;
        self.inner.borrow_mut().render_core_host.note_submission(
            &conversation_id,
            text,
            Some(text),
            now,
        );
    }

    /// 消费 user 消息回显；返回是否命中了 pending 条目（命中时需要刷新展示）。
    pub(crate) fn note_pending_user_echo(&mut self, message: &AgentMessage) -> bool {
        let conversation_id = self.session().session_id();
        self.inner
            .borrow_mut()
            .render_core_host
            .note_echo(&conversation_id, message)
    }

    /// `session.prompt` 失败：把最近一次提交标为可重试错误。
    pub(crate) fn note_pending_user_error(&mut self, message: &str) -> bool {
        self.inner
            .borrow_mut()
            .render_core_host
            .note_prompt_error(message)
    }

    /// 10ms 宿主循环：推进 watchdog，返回本次变为错误的条数。
    pub(crate) fn tick_render_core(&mut self) -> usize {
        let now = kiana_ai::model_catalog::now_epoch_millis() as i64;
        self.inner.borrow_mut().render_core_host.tick(now)
    }

    /// 当前会话尚未回显的用户消息展示行。
    pub(crate) fn pending_user_lines(&self) -> Vec<String> {
        let conversation_id = self.session().session_id();
        self.inner
            .borrow()
            .render_core_host
            .pending_lines(&conversation_id)
    }

    /// 切换 UI 语言：重建 translator、刷新 pending 展示、同步已存在的审批队列。
    ///
    /// 只影响界面文案，不改模型回答语言（`ui.locale` 与 output language 分离）。
    /// 用户语言包目录与构造时一致：`<agent_dir>/locales`。
    pub(crate) fn apply_ui_locale(&mut self, locale: &str) {
        let translator = {
            let mut inner = self.inner.borrow_mut();
            let user_dir = kiana_core::config::get_agent_dir().join("locales");
            inner.render_core_host.set_locale(locale, user_dir);
            inner.render_core_host.translator()
        };
        self.update_pending_messages_display();
        super::extension_ui::set_approval_queue_translator(&translator);
    }

    /// 测试接缝：构造时读到的实验开关。
    pub fn test_render_core_enabled(&self) -> bool {
        self.inner.borrow().render_core_host.enabled()
    }

    /// 测试接缝：切换 UI locale。
    pub fn test_apply_ui_locale(&mut self, locale: &str) {
        self.apply_ui_locale(locale);
    }

    /// 测试接缝：当前翻译器使用的 locale。
    pub fn test_ui_locale(&self) -> String {
        self.inner.borrow().render_core_host.locale().to_string()
    }

    /// 测试接缝：记录一条 pending 用户消息。
    pub fn test_note_pending_user_submission(&mut self, text: &str) {
        self.note_pending_user_submission(text);
    }

    /// 测试接缝：当前会话尚未回显的用户消息展示行。
    pub fn test_pending_user_lines(&self) -> Vec<String> {
        self.pending_user_lines()
    }
}

/// 从标准 user 消息里取文本内容。
fn user_message_text(message: &AgentMessage) -> Option<String> {
    let Message::User(user) = message.as_standard()? else {
        return None;
    };
    Some(match &user.content {
        kiana_ai::types::UserContent::Text(text) => text.clone(),
        kiana_ai::types::UserContent::Blocks(blocks) => blocks
            .iter()
            .filter_map(|block| match block {
                kiana_ai::types::UserContentBlock::Text(text) => Some(text.text.clone()),
                kiana_ai::types::UserContentBlock::Image(_) => None,
            })
            .collect::<Vec<_>>()
            .join("\n"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use kiana_ai::types::{
        TextContent, TextKind, UserContent, UserContentBlock, UserMessage, UserRole,
    };

    fn user_message(text: &str, timestamp: i64) -> AgentMessage {
        AgentMessage::Standard(Message::User(UserMessage {
            role: UserRole::User,
            content: UserContent::Text(text.to_string()),
            timestamp,
        }))
    }

    #[test]
    fn disabled_host_is_a_noop() {
        let mut host = RenderCoreHost::new(false);
        assert!(host.note_submission("s", "hello", None, 10).is_none());
        assert!(host.pending_lines("s").is_empty());
        assert_eq!(host.tick(1_000_000), 0);
        assert!(!host.note_echo("s", &user_message("hello", 20)));
    }

    #[test]
    fn echo_consumes_the_matching_pending_message() {
        let mut host = RenderCoreHost::new(true);
        let id = host
            .note_submission("s", "hello", None, 10)
            .expect("pending id");
        assert_eq!(
            host.pending_lines("s"),
            vec!["Sending... hello".to_string()]
        );
        assert!(host.note_echo("s", &user_message("hello", 20)));
        assert!(host.pending_lines("s").is_empty());
        // 第二次回显（断线重放）不会重复消费。
        assert!(!host.note_echo("s", &user_message("hello", 30)));
        assert!(!id.is_empty());
    }

    #[test]
    fn locale_changes_pending_wording() {
        let mut host =
            RenderCoreHost::new(true).with_locale("zh", PathBuf::from("/nonexistent-locales"));
        assert_eq!(host.locale(), "zh");
        host.note_submission("s", "你好", None, 10)
            .expect("pending");
        let lines = host.pending_lines("s");
        assert!(lines[0].contains("发送中"), "{lines:?}");
        assert!(lines[0].contains("你好"), "{lines:?}");
    }

    #[test]
    fn set_locale_rebuilds_the_translator_in_place() {
        let mut host =
            RenderCoreHost::new(true).with_locale("en", PathBuf::from("/nonexistent-locales"));
        host.note_submission("s", "hello", None, 10)
            .expect("pending");
        assert_eq!(
            host.pending_lines("s"),
            vec!["Sending... hello".to_string()]
        );
        host.set_locale("zh", PathBuf::from("/nonexistent-locales"));
        assert_eq!(host.locale(), "zh");
        assert_eq!(host.translator().locale(), "zh");
        let lines = host.pending_lines("s");
        assert!(lines[0].contains("发送中……"), "{lines:?}");
    }

    #[test]
    fn prompt_error_marks_the_last_submission_retryable() {
        let mut host = RenderCoreHost::new(true);
        host.note_submission("s", "hello", None, 10)
            .expect("pending id");
        assert!(host.note_prompt_error("network down"));
        let lines = host.pending_lines("s");
        assert_eq!(lines.len(), 1);
        assert!(
            lines[0].contains("Failed to send: network down"),
            "{lines:?}"
        );
        assert!(lines[0].contains("hello"), "{lines:?}");
        assert!(lines[0].contains("Retry"), "{lines:?}");
    }

    #[test]
    fn tick_turns_a_stale_submission_into_a_retryable_error() {
        let mut host = RenderCoreHost::new(true);
        host.note_submission("s", "hello", None, 10)
            .expect("pending id");
        assert_eq!(host.tick(10 + 150_000), 1);
        let lines = host.pending_lines("s");
        assert!(lines[0].contains("Send timed out"), "{lines:?}");
    }

    #[test]
    fn lines_are_scoped_to_the_conversation() {
        let mut host = RenderCoreHost::new(true);
        host.note_submission("a", "first", None, 10);
        host.note_submission("b", "second", None, 11);
        assert_eq!(
            host.pending_lines("a"),
            vec!["Sending... first".to_string()]
        );
        assert_eq!(
            host.pending_lines("b"),
            vec!["Sending... second".to_string()]
        );
        host.clear();
        assert!(host.pending_lines("a").is_empty());
    }

    #[test]
    fn block_user_content_is_reconstructed_for_matching() {
        let message = AgentMessage::Standard(Message::User(UserMessage {
            role: UserRole::User,
            content: UserContent::Blocks(vec![
                UserContentBlock::Text(TextContent {
                    kind_text: TextKind::Text,
                    text: "line one".to_string(),
                    text_signature: None,
                }),
                UserContentBlock::Text(TextContent {
                    kind_text: TextKind::Text,
                    text: "line two".to_string(),
                    text_signature: None,
                }),
            ]),
            timestamp: 5,
        }));
        let mut host = RenderCoreHost::new(true);
        host.note_submission("s", "line one\nline two", None, 1);
        assert!(host.note_echo("s", &message));
    }
}
