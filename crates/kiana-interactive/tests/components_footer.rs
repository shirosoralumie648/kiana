//! Footer 的 TS 对拍测试。
//!
//! 移植自 `packages/coding-agent/test/footer-width.test.ts`。

mod common;

use std::sync::{Arc, Mutex, Once};

use indexmap::IndexMap;
use kiana_agent_core::AgentMessage;
use kiana_ai::types::{
    AssistantMessage, AssistantRole, Message, ModelThinkingLevel, StopReason, ToolResultMessage,
    ToolResultRole, Usage, UsageCost,
};
use kiana_core::event_bus::Unsubscribe;
use kiana_ext_api::types::ContextUsage;
use kiana_ext_api::ui::ReadonlyFooterDataProvider;
use kiana_permissions::PermissionMode;
use kiana_session_core::session_manager::{
    ReadonlySessionManager, SessionEntry, SessionHeader, SessionManager, SessionProjection,
    SessionTreeNode,
};
use kiana_tui::tui::Component;

use kiana_interactive::components::footer::{
    FooterComponent, FooterModel, FooterRoutedModel, FooterSession, format_cwd_for_footer,
    format_tokens,
};

// ---------------------------------------------------------------------------
// 夹具
// ---------------------------------------------------------------------------

/// 只读会话管理器替身：条目可以从测试里继续追加（模拟 leaf 追加）。
struct StaticManager {
    cwd: String,
    session_id: String,
    name: Option<String>,
    entries: Mutex<Vec<SessionEntry>>,
}

impl ReadonlySessionManager for StaticManager {
    fn get_cwd(&self) -> &str {
        &self.cwd
    }

    fn get_session_dir(&self) -> &str {
        ""
    }

    fn get_session_id(&self) -> &str {
        &self.session_id
    }

    fn get_session_file(&self) -> Option<&str> {
        None
    }

    fn get_leaf_id(&self) -> Option<&str> {
        None
    }

    fn get_leaf_entry(&self) -> Option<&SessionEntry> {
        None
    }

    fn get_entry(&self, _id: &str) -> Option<&SessionEntry> {
        None
    }

    fn get_label(&self, _id: &str) -> Option<&str> {
        None
    }

    fn get_branch(&self, _from_id: Option<&str>) -> Vec<SessionEntry> {
        Vec::new()
    }

    fn build_context_entries(&self) -> Vec<SessionEntry> {
        Vec::new()
    }

    fn build_session_projection(&self) -> SessionProjection {
        SessionProjection {
            entries: Vec::new(),
            messages: Vec::new(),
            thinking_level: String::new(),
            model: None,
        }
    }

    fn get_header(&self) -> Option<&SessionHeader> {
        None
    }

    fn get_entries(&self) -> Vec<SessionEntry> {
        self.entries.lock().unwrap().clone()
    }

    fn get_tree(&self) -> Vec<SessionTreeNode> {
        Vec::new()
    }

    fn get_session_name(&self) -> Option<String> {
        self.name.clone()
    }
}

struct MockSession {
    model: Option<FooterModel>,
    thinking_level: ModelThinkingLevel,
    manager: Arc<dyn ReadonlySessionManager>,
    context_usage: Option<ContextUsage>,
    routed: Option<FooterRoutedModel>,
    subscription: bool,
}

impl FooterSession for MockSession {
    fn model(&self) -> Option<FooterModel> {
        self.model.clone()
    }

    fn thinking_level(&self) -> ModelThinkingLevel {
        self.thinking_level
    }

    fn session_manager(&self) -> Arc<dyn ReadonlySessionManager> {
        Arc::clone(&self.manager)
    }

    fn get_context_usage(&self) -> Option<ContextUsage> {
        self.context_usage.clone()
    }

    fn routed_model(&self) -> Option<FooterRoutedModel> {
        self.routed.clone()
    }

    fn is_using_subscription(&self, _provider: &str) -> bool {
        self.subscription
    }
}

struct MockFooterData {
    branch: Option<String>,
    statuses: IndexMap<String, String>,
    provider_count: usize,
}

impl ReadonlyFooterDataProvider for MockFooterData {
    fn get_git_branch(&self) -> Option<String> {
        self.branch.clone()
    }

    fn get_extension_statuses(&self) -> &IndexMap<String, String> {
        &self.statuses
    }

    fn get_available_provider_count(&self) -> usize {
        self.provider_count
    }

    fn on_branch_change(&self, _callback: Arc<dyn Fn() + Send + Sync>) -> Unsubscribe {
        Box::new(|| {})
    }
}

fn ensure_home() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        // SAFETY: 本测试二进制内所有 footer 测试都期望同一个 HOME，且只写入一次。
        unsafe { std::env::set_var("HOME", "/home/user") };
    });
}

fn usage(input: u64, output: u64, cache_read: u64, cache_write: u64, total_cost: f64) -> Usage {
    Usage {
        input,
        output,
        cache_read,
        cache_write,
        cache_write1h: None,
        reasoning: None,
        total_tokens: input + output,
        cost: UsageCost {
            input: 0.0,
            output: 0.0,
            cache_read: 0.0,
            cache_write: 0.0,
            total: total_cost,
            total_from_provider: None,
        },
    }
}

fn assistant_with_usage(usage: Usage) -> AssistantMessage {
    AssistantMessage {
        role: AssistantRole::Assistant,
        content: Vec::new(),
        api: "openai-responses".to_string(),
        provider: "openai".to_string(),
        model: "gpt-4o-mini".to_string(),
        response_model: None,
        response_id: None,
        provider_thinking_level: None,
        thinking_level: None,
        diagnostics: None,
        usage,
        stop_reason: StopReason::Stop,
        deferred: None,
        error_message: None,
        raw_stop_reason: None,
        end_turn: None,
        timestamp: 0,
        duration_ms: None,
    }
}

fn tool_result_with_usage(usage: Usage) -> ToolResultMessage {
    ToolResultMessage {
        role: ToolResultRole::ToolResult,
        tool_call_id: "call".to_string(),
        tool_name: "read".to_string(),
        content: Vec::new(),
        details: None,
        usage: Some(usage),
        nested_calls: None,
        is_error: false,
        timestamp: 0,
        duration_ms: None,
    }
}

struct SessionOptions {
    session_name: Option<&'static str>,
    model_id: &'static str,
    provider: &'static str,
    reasoning: bool,
    thinking_level: ModelThinkingLevel,
    assistant_usage: Option<Usage>,
    branch_usage: Option<Usage>,
    compaction_usage: Option<Usage>,
    tool_usage: Option<Usage>,
    using_subscription: bool,
    routed: Option<FooterRoutedModel>,
    context_usage: Option<ContextUsage>,
}

impl Default for SessionOptions {
    fn default() -> Self {
        SessionOptions {
            session_name: None,
            model_id: "test-model",
            provider: "test",
            reasoning: false,
            thinking_level: ModelThinkingLevel::Off,
            assistant_usage: None,
            branch_usage: None,
            compaction_usage: None,
            tool_usage: None,
            using_subscription: false,
            routed: None,
            context_usage: Some(ContextUsage {
                tokens: Some(24_600),
                context_window: 200_000,
                percent: Some(12.3),
            }),
        }
    }
}

fn create_session(options: SessionOptions) -> (Box<dyn FooterSession>, Arc<StaticManager>) {
    let mut seed = SessionManager::in_memory("/tmp/project", None, None).expect("in-memory");
    if let Some(name) = options.session_name {
        seed.append_session_info(name).expect("session info");
    }
    if let Some(usage) = options.assistant_usage {
        seed.append_message(AgentMessage::Standard(Message::Assistant(
            assistant_with_usage(usage),
        )))
        .expect("assistant message");
    }
    if let Some(usage) = options.branch_usage {
        seed.branch_with_summary(None, "branch", None, None, Some(usage))
            .expect("branch summary");
    }
    if let Some(usage) = options.compaction_usage {
        seed.append_compaction("compaction", None, 0.0, None, None, Some(usage))
            .expect("compaction");
    }
    if let Some(usage) = options.tool_usage {
        seed.append_message(AgentMessage::Standard(Message::ToolResult(
            tool_result_with_usage(usage),
        )))
        .expect("tool result");
    }
    let manager = Arc::new(StaticManager {
        cwd: "/tmp/project".to_string(),
        session_id: "test-session".to_string(),
        name: options.session_name.map(str::to_string),
        entries: Mutex::new(seed.get_entries()),
    });
    let session = MockSession {
        model: Some(FooterModel {
            id: options.model_id.to_string(),
            provider: options.provider.to_string(),
            context_window: Some(200_000),
            reasoning: options.reasoning,
        }),
        thinking_level: options.thinking_level,
        manager: Arc::clone(&manager) as Arc<dyn ReadonlySessionManager>,
        context_usage: options.context_usage,
        routed: options.routed,
        subscription: options.using_subscription,
    };
    (Box::new(session), manager)
}

fn footer_data(provider_count: usize) -> Arc<dyn ReadonlyFooterDataProvider> {
    Arc::new(MockFooterData {
        branch: Some("main".to_string()),
        statuses: IndexMap::new(),
        provider_count,
    })
}

fn golden(key: &str) -> Vec<String> {
    let raw = include_str!("fixtures/render_goldens_dark.json");
    let value: serde_json::Value = serde_json::from_str(raw).expect("golden json");
    value[key]
        .as_array()
        .unwrap_or_else(|| panic!("missing golden {key}"))
        .iter()
        .map(|line| line.as_str().unwrap().to_string())
        .collect()
}

fn strip(value: &str) -> String {
    kiana_core::ansi::strip_ansi(value)
}

// ---------------------------------------------------------------------------
// 测试
// ---------------------------------------------------------------------------

#[test]
fn format_cwd_does_not_abbreviate_sibling_paths_that_share_the_home_prefix() {
    assert_eq!(
        format_cwd_for_footer("/home/user2", Some("/home/user")),
        "/home/user2"
    );
}

#[test]
fn format_cwd_abbreviates_the_home_directory_and_descendants() {
    assert_eq!(format_cwd_for_footer("/home/user", Some("/home/user")), "~");
    assert_eq!(
        format_cwd_for_footer("/home/user/project", Some("/home/user")),
        "~/project"
    );
    assert_eq!(format_cwd_for_footer("/tmp/x", None), "/tmp/x");
}

#[test]
fn format_tokens_matches_the_ts_boundaries() {
    let expected = golden("format_tokens");
    let counts = [
        999u64, 1000, 9999, 10000, 999999, 1000000, 9999999, 10000000, 12345678,
    ];
    let actual: Vec<String> = counts.iter().map(|count| format_tokens(*count)).collect();
    assert_eq!(actual, expected);
}

#[test]
fn renders_basic_footer_like_the_ts_component() {
    ensure_home();
    kiana_theme::init_theme(Some("dark"), false);
    let (session, _manager) = create_session(SessionOptions {
        assistant_usage: Some(usage(12_345, 6_789, 50, 50, 1.234)),
        ..SessionOptions::default()
    });
    let mut footer = FooterComponent::from_parts(session, footer_data(1));
    assert_eq!(footer.render(120), golden("footer_basic"));
}

#[test]
fn renders_session_name_provider_suffix_and_sorted_extension_statuses() {
    ensure_home();
    kiana_theme::init_theme(Some("dark"), false);
    let (session, _manager) = create_session(SessionOptions {
        session_name: Some("my session"),
        model_id: "gpt-5",
        provider: "openai",
        reasoning: true,
        thinking_level: ModelThinkingLevel::High,
        ..SessionOptions::default()
    });
    let mut statuses = IndexMap::new();
    statuses.insert("zeta".to_string(), "Zed\nstatus".to_string());
    statuses.insert("alpha".to_string(), " Alpha   status ".to_string());
    let mut footer = FooterComponent::from_parts(
        session,
        Arc::new(MockFooterData {
            branch: Some("main".to_string()),
            statuses,
            provider_count: 2,
        }),
    );
    assert_eq!(footer.render(120), golden("footer_multi"));
    assert_eq!(footer.render(40), golden("footer_narrow"));
}

#[test]
fn shows_the_physical_model_a_virtual_model_routed_to() {
    ensure_home();
    kiana_theme::init_theme(Some("dark"), false);
    let (session, _manager) = create_session(SessionOptions {
        model_id: "auto",
        reasoning: true,
        thinking_level: ModelThinkingLevel::High,
        routed: Some(FooterRoutedModel {
            model_id: "gpt-5.6-luna".to_string(),
            thinking_level: Some(ModelThinkingLevel::Medium),
        }),
        ..SessionOptions::default()
    });
    let mut footer = FooterComponent::from_parts(session, footer_data(1));
    assert_eq!(footer.render(120), golden("footer_routed"));
    assert!(strip(&footer.render(120)[1]).contains("auto • high → gpt-5.6-luna • medium"));
}

#[test]
fn includes_summary_and_tool_result_usage_in_the_total_cost() {
    ensure_home();
    kiana_theme::init_theme(Some("dark"), false);
    let (session, _manager) = create_session(SessionOptions {
        assistant_usage: Some(usage(100, 10, 0, 0, 0.5)),
        branch_usage: Some(usage(20, 5, 0, 0, 0.25)),
        compaction_usage: Some(usage(5, 2, 0, 0, 0.125)),
        tool_usage: Some(usage(15, 3, 0, 0, 0.375)),
        ..SessionOptions::default()
    });
    let mut footer = FooterComponent::from_parts(session, footer_data(1));
    assert!(strip(&footer.render(120)[1]).contains("$1.250"));
}

#[test]
fn updates_usage_totals_after_an_entry_is_appended() {
    ensure_home();
    kiana_theme::init_theme(Some("dark"), false);
    let usage = usage(10, 1, 0, 0, 0.5);
    let (session, manager) = create_session(SessionOptions {
        assistant_usage: Some(usage.clone()),
        ..SessionOptions::default()
    });
    let mut footer = FooterComponent::from_parts(session, footer_data(1));
    assert!(strip(&footer.render(120)[1]).contains("$0.500"));

    let extra = manager
        .entries
        .lock()
        .unwrap()
        .first()
        .cloned()
        .expect("seed entry");
    manager.entries.lock().unwrap().push(extra);
    assert!(strip(&footer.render(120)[1]).contains("$1.000"));
}

#[test]
fn shows_the_latest_cache_hit_rate_when_cache_usage_is_present() {
    ensure_home();
    kiana_theme::init_theme(Some("dark"), false);
    let (session, _manager) = create_session(SessionOptions {
        assistant_usage: Some(usage(100, 10, 50, 50, 0.001)),
        ..SessionOptions::default()
    });
    let mut footer = FooterComponent::from_parts(session, footer_data(1));
    assert!(strip(&footer.render(120)[1]).contains("CH25.0%"));
}

#[test]
fn marks_kimi_coding_costs_as_subscription_estimates() {
    ensure_home();
    kiana_theme::init_theme(Some("dark"), false);
    let (session, _manager) = create_session(SessionOptions {
        provider: "kimi-coding",
        assistant_usage: Some(usage(100, 10, 0, 0, 1.234)),
        ..SessionOptions::default()
    });
    let mut footer = FooterComponent::from_parts(session, footer_data(1));
    assert_eq!(footer.render(120), golden("footer_kimi"));
    assert!(strip(&footer.render(120)[1]).contains("$1.234 (sub)"));
}

#[test]
fn marks_explicitly_identified_subscription_auth() {
    ensure_home();
    kiana_theme::init_theme(Some("dark"), false);
    let (session, _manager) = create_session(SessionOptions {
        provider: "anthropic",
        using_subscription: true,
        ..SessionOptions::default()
    });
    let mut footer = FooterComponent::from_parts(session, footer_data(1));
    assert!(strip(&footer.render(120)[1]).contains("$0.000 (sub)"));
}

#[test]
fn does_not_mark_generic_oauth_sign_in_as_a_subscription() {
    ensure_home();
    kiana_theme::init_theme(Some("dark"), false);
    let (session, _manager) = create_session(SessionOptions {
        provider: "openrouter",
        assistant_usage: Some(usage(100, 10, 0, 0, 1.234)),
        ..SessionOptions::default()
    });
    let mut footer = FooterComponent::from_parts(session, footer_data(1));
    let stats = strip(&footer.render(120)[1]);
    assert!(stats.contains("$1.234"));
    assert!(!stats.contains("(sub)"));
}

#[test]
fn null_context_percent_renders_a_question_mark() {
    ensure_home();
    kiana_theme::init_theme(Some("dark"), false);
    let (session, _manager) = create_session(SessionOptions {
        context_usage: Some(ContextUsage {
            tokens: None,
            context_window: 200_000,
            percent: None,
        }),
        ..SessionOptions::default()
    });
    let mut footer = FooterComponent::from_parts(session, footer_data(1));
    assert_eq!(footer.render(120), golden("footer_null_percent"));
}

#[test]
fn high_context_usage_is_colored_as_an_error() {
    ensure_home();
    kiana_theme::init_theme(Some("dark"), false);
    let (session, _manager) = create_session(SessionOptions {
        context_usage: Some(ContextUsage {
            tokens: Some(190_000),
            context_window: 200_000,
            percent: Some(95.0),
        }),
        ..SessionOptions::default()
    });
    let mut footer = FooterComponent::from_parts(session, footer_data(1));
    assert_eq!(footer.render(120), golden("footer_high_percent"));
}

#[test]
fn keeps_all_lines_within_width_for_wide_session_names() {
    ensure_home();
    kiana_theme::init_theme(Some("dark"), false);
    let width = 93;
    let (session, _manager) = create_session(SessionOptions {
        session_name: Some(
            "한글한글한글한글한글한글한글한글한글한글한글한글한글한글한글한글한글한글한글한글한글한글한글한글한글한글한글한글한글한글",
        ),
        ..SessionOptions::default()
    });
    let mut footer = FooterComponent::from_parts(session, footer_data(1));
    for line in footer.render(width) {
        assert!(
            kiana_tui::visible_width(&line) <= width,
            "line too wide: {line:?}"
        );
    }
}

#[test]
fn keeps_stats_line_within_width_for_wide_model_and_provider_names() {
    ensure_home();
    kiana_theme::init_theme(Some("dark"), false);
    let width = 60;
    let (session, _manager) = create_session(SessionOptions {
        model_id: "模模模模模模模模模模模模模模模模模模模模模模模模模模模模模模",
        provider: "공급자",
        reasoning: true,
        thinking_level: ModelThinkingLevel::High,
        assistant_usage: Some(usage(12_345, 6_789, 0, 0, 1.234)),
        ..SessionOptions::default()
    });
    let mut footer = FooterComponent::from_parts(session, footer_data(2));
    for line in footer.render(width) {
        assert!(
            kiana_tui::visible_width(&line) <= width,
            "line too wide: {line:?}"
        );
    }
}

#[test]
fn no_context_usage_renders_zero_percent_with_the_model_window() {
    ensure_home();
    kiana_theme::init_theme(Some("dark"), false);
    let (session, _manager) = create_session(SessionOptions {
        context_usage: None,
        ..SessionOptions::default()
    });
    let mut footer = FooterComponent::from_parts(session, footer_data(1));
    assert!(strip(&footer.render(120)[1]).contains("0.0%/200k (auto)"));
}

#[test]
fn auto_compact_indicator_can_be_disabled() {
    ensure_home();
    kiana_theme::init_theme(Some("dark"), false);
    let (session, _manager) = create_session(SessionOptions::default());
    let mut footer = FooterComponent::from_parts(session, footer_data(1));
    footer.set_auto_compact_enabled(false);
    assert!(!strip(&footer.render(120)[1]).contains("(auto)"));
}

#[test]
fn permission_mode_none_keeps_existing_goldens() {
    ensure_home();
    kiana_theme::init_theme(Some("dark"), false);
    let (session, _manager) = create_session(SessionOptions {
        assistant_usage: Some(usage(12_345, 6_789, 50, 50, 1.234)),
        ..SessionOptions::default()
    });
    let mut footer = FooterComponent::from_parts(session, footer_data(1));
    assert_eq!(footer.permission_mode(), None);
    assert_eq!(footer.render(120), golden("footer_basic"));
}

#[test]
fn each_permission_mode_appends_text_and_color() {
    ensure_home();
    kiana_theme::init_theme(Some("dark"), false);
    let (session, _manager) = create_session(SessionOptions::default());
    let mut footer = FooterComponent::from_parts(session, footer_data(1));
    for (mode, text) in [
        (PermissionMode::ReadOnly, "read-only"),
        (PermissionMode::Default, "default"),
        (PermissionMode::AcceptEdits, "accept-edits"),
        (PermissionMode::BypassPermissions, "bypass"),
    ] {
        footer.set_permission_mode(Some(mode));
        assert_eq!(footer.permission_mode(), Some(mode));
        let line = footer.render(120)[1].clone();
        let visible = strip(&line);
        assert!(
            visible.ends_with(&format!(" • {text}")),
            "missing mode segment {text:?} in {visible:?}"
        );
        // 原始行带主题转义（颜色或加粗），保证不是纯文本段。
        assert!(
            line.contains('\x1b'),
            "mode segment should be styled: {line:?}"
        );
    }
    footer.set_permission_mode(None);
    assert!(!strip(&footer.render(120)[1]).contains(" • read-only"));
}
