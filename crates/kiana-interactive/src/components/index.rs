//! `components/index.ts`（37 行）的再导出面。
//!
//! TS 的 barrel 是纯 `export ... from`，无运行时代码；本模块同样只有 `pub use`。与 TS 的
//! 差别只有一处：TS 用 camelCase 文件名/函数名，Rust 按 §4 约定用 snake_case，因此
//! `renderDiff` → [`render_diff`]、`keyHint` → [`key_hint`]、`truncateToVisualLines` →
//! [`truncate_to_visual_lines`] 等（映射见下表）。
//!
//! | TS（`components/index.ts`） | Rust |
//! |---|---|
//! | `ArminComponent` | [`ArminComponent`] |
//! | `AssistantMessageComponent` | [`AssistantMessageComponent`] |
//! | `BashExecutionComponent` | [`BashExecutionComponent`] |
//! | `BorderedLoader` | [`BorderedLoader`]（`kiana-ui-kit`） |
//! | `BranchSummaryMessageComponent` | [`BranchSummaryMessageComponent`] |
//! | `CompactionSummaryMessageComponent` | [`CompactionSummaryMessageComponent`] |
//! | `CustomEditor` / `CustomEditorOptions` | [`CustomEditor`] / [`CustomEditorOptions`] |
//! | `CustomMessageComponent` | [`CustomMessageComponent`] |
//! | `RenderDiffOptions` / `renderDiff` | [`RenderDiffOptions`] / [`render_diff`]（`kiana-ui-kit`） |
//! | `DynamicBorder` | [`DynamicBorder`]（`kiana-ui-kit`） |
//! | `ExtensionEditorComponent` | [`ExtensionEditorComponent`] |
//! | `ExtensionInputComponent` | [`ExtensionInputComponent`] |
//! | `ExtensionSelectorComponent` | [`ExtensionSelectorComponent`] |
//! | `FirstTimeSetupComponent` / `FirstTimeSetupOptions` / `FirstTimeSetupResult` | 同名 |
//! | `FooterComponent` | [`FooterComponent`] |
//! | `keyHint` / `keyText` / `rawKeyHint` | [`key_hint`] / [`key_text`] / [`raw_key_hint`]（`kiana-ui-kit`） |
//! | `LoginDialogComponent` | [`LoginDialogComponent`] |
//! | `ModelSelectorComponent` | [`ModelSelectorComponent`] |
//! | `OAuthSelectorComponent` | [`OAuthSelectorComponent`] |
//! | `ModelsCallbacks` / `ModelsConfig` / `ScopedModelsSelectorComponent` | 同名 |
//! | `SessionSelectorComponent` | [`SessionSelectorComponent`] |
//! | `SettingsCallbacks` / `SettingsConfig` / `SettingsSelectorComponent` | 同名 |
//! | `ShowImagesSelectorComponent` | [`ShowImagesSelectorComponent`] |
//! | `SkillInvocationMessageComponent` | [`SkillInvocationMessageComponent`] |
//! | `ThemeSelectorComponent` | [`ThemeSelectorComponent`] |
//! | `ThinkingSelectorComponent` | [`ThinkingSelectorComponent`] |
//! | `ToolExecutionComponent` / `ToolExecutionOptions` | 同名 |
//! | `TreeSelectorComponent` | [`TreeSelectorComponent`] |
//! | `TrustSelectorComponent` | [`TrustSelectorComponent`] |
//! | `UserMessageComponent` | [`UserMessageComponent`] |
//! | `UserMessageSelectorComponent` | [`UserMessageSelectorComponent`] |
//! | `VisualTruncateResult` / `truncateToVisualLines` | [`VisualTruncateResult`] / [`truncate_to_visual_lines`]（`kiana-ui-kit`） |
//!
//! TS 里**不在** barrel 中、但被 `interactive-mode.ts` 按路径直接 import 的公开件也在这里
//! 一并再导出（§2.1 明确要求它们“必须实现且可被同 crate 访问”；Rust 集成测试在 crate 外，
//! 因此以 `pub` 暴露，属**增量公开面**，不改变 TS barrel 的语义）：
//! `CustomEntryComponent`、`EarendilAnnouncementComponent`、`createMermaidMarkdownTransformer`、
//! `kianaLogoLines`/`kianaWordmark`/`supportsKianaLogo`、`CountdownTimer`、`createLoginMenuSelector`、
//! `SelectSubmenu`/`SteppedSubmenu`、`session-selector-search` 的查询函数、`createMarkdownTransform`、
//! `ThemedText`、`StatusIndicator` 层级/`IdleStatus`、`VisualLinePreview`、
//! `playKianaLogo3d`/`playArmin3d`/`playEasterEgg3d`、`isArminPixel`/`ARMIN_WIDTH`/`ARMIN_HEIGHT`、
//! `formatAuthSelectorProviderType`/`Status`、`formatTokens`/`formatCwdForFooter`、
//! `parseSearchQuery`/`matchSession`/`filterAndSortSessions`/`hasSessionName`。
//!
//! ## 维护约束
//!
//! 本模块没有任何实现，只有 `pub use`；`lib.rs` 用 `pub use components::index::*` 把它
//! 的全部名字再导出到 crate 根。因此：
//! - 新增一个 TS barrel 可见的组件时，必须在这里补再导出，否则 crate 根看不到该名字；
//! - 反过来，删除或改名会同时改变 crate 根的公开面；
//! - `tests/public_api.rs` 断言 crate 根路径与 `components::footer` 等深路径的
//!   `type_name` 相同，即它们解析到同一个类型；搬动库内定义会直接让该测试失败。
//!
//! 上表的 TS↔Rust 对应关系也要随再导出一起维护，避免文档与代码漂移。

// --- kiana-ui-kit 的 9 个共用渲染小件（TS 文件在本目录，实现在 kiana-ui-kit） ---
pub use kiana_ui_kit::bordered_loader::BorderedLoader;
pub use kiana_ui_kit::countdown_timer::CountdownTimer;
pub use kiana_ui_kit::diff::{RenderDiffOptions, render_diff};
pub use kiana_ui_kit::dynamic_border::DynamicBorder;
pub use kiana_ui_kit::keybinding_hints::{key_hint, key_text, raw_key_hint};
pub use kiana_ui_kit::kiana_logo::{kiana_logo_lines, kiana_wordmark, supports_kiana_logo};
pub use kiana_ui_kit::themed_text::ThemedText;
pub use kiana_ui_kit::visual_truncate::{
    VisualLinePreview, VisualTruncateResult, truncate_to_visual_lines,
};

// --- 消息组件 ---
pub use crate::components::armin::ArminComponent;
pub use crate::components::assistant_message::AssistantMessageComponent;
pub use crate::components::bash_execution::BashExecutionComponent;
pub use crate::components::branch_summary_message::BranchSummaryMessageComponent;
pub use crate::components::compaction_summary_message::CompactionSummaryMessageComponent;
pub use crate::components::custom_entry::CustomEntryComponent;
pub use crate::components::custom_message::CustomMessageComponent;
pub use crate::components::earendil_announcement::EarendilAnnouncementComponent;
pub use crate::components::skill_invocation_message::SkillInvocationMessageComponent;
pub use crate::components::user_message::UserMessageComponent;

// --- 选择器 / 面板 ---
pub use crate::components::custom_editor::{CustomEditor, CustomEditorOptions};
pub use crate::components::extension_editor::ExtensionEditorComponent;
pub use crate::components::extension_input::ExtensionInputComponent;
pub use crate::components::extension_selector::ExtensionSelectorComponent;
pub use crate::components::first_time_setup::{
    FirstTimeSetupComponent, FirstTimeSetupOptions, FirstTimeSetupResult,
};
pub use crate::components::login_dialog::LoginDialogComponent;
pub use crate::components::model_selector::ModelSelectorComponent;
pub use crate::components::oauth_selector::{
    AuthSelectorProvider, OAuthSelectorComponent, format_auth_selector_provider_status,
    format_auth_selector_provider_type,
};
pub use crate::components::radius_login_selector::{RadiusOption, create_login_menu_selector};
pub use crate::components::scoped_models_selector::{
    ModelsCallbacks, ModelsConfig, ScopedModelsSelectorComponent,
};
pub use crate::components::session_selector::SessionSelectorComponent;
pub use crate::components::settings_selector::{
    SettingsCallbacks, SettingsConfig, SettingsSelectorComponent,
};
pub use crate::components::settings_submenu::{SelectSubmenu, SteppedSubmenu};
pub use crate::components::show_images_selector::ShowImagesSelectorComponent;
pub use crate::components::theme_selector::ThemeSelectorComponent;
pub use crate::components::thinking_selector::ThinkingSelectorComponent;
pub use crate::components::tool_execution::{ToolExecutionComponent, ToolExecutionOptions};
pub use crate::components::tree_selector::TreeSelectorComponent;
pub use crate::components::trust_selector::TrustSelectorComponent;
pub use crate::components::user_message_selector::UserMessageSelectorComponent;

// --- 基础设施 ---
pub use crate::components::footer::{FooterComponent, format_cwd_for_footer, format_tokens};
pub use crate::components::markdown_transform::create_markdown_transform;
pub use crate::components::mermaid::create_mermaid_markdown_transformer;
pub use crate::components::status_indicator::{
    BaseStatusIndicator, BranchSummaryStatusIndicator, CompactionStatusIndicator, IdleStatus,
    RetryStatusIndicator, StatusIndicator, WorkingStatusIndicator,
};

// --- 3D 彩蛋（`easter-egg-3d.ts` + `easter-egg-3d.lazy.ts`）---
pub use crate::components::armin::{ARMIN_HEIGHT, ARMIN_WIDTH, is_armin_pixel};
pub use crate::components::easter_egg_3d::{
    EasterEgg3d, EasterEgg3dAnimation, play_armin_3d, play_easter_egg_3d, play_kiana_logo_3d,
};

// --- 会话搜索（`session-selector-search.ts`）---
pub use crate::components::session_selector_search::{
    MatchResult, NameFilter, ParsedSearchQuery, SortMode, filter_and_sort_sessions,
    has_session_name, match_session, parse_search_query,
};

// --- TUI panels（非 TS barrel：`tui-panels-design.md` 新增的增量公开面） ---
//
// TS 参照实现没有 `/permissions`、`/mcp` 独立面板与 `/tasks` 对话框；这三组组件是
// 产品增量（见设计文档 §3/§5/§6）。它们不改变 `components/index.ts` 的 barrel 语义，
// 只在 crate 根提供与深路径等价的类型，方便集成测试与后续面板复用。
pub use crate::components::approval_mode_indicator::{
    ApprovalModeIndicator, PermissionModeVisual, permission_mode_visual,
};
pub use crate::components::mcp_dialog::{
    McpDialog, McpDialogCallbacks, McpDialogOptions, McpDialogSnapshot, McpDialogView, McpNotice,
    McpNoticeKind, McpServerAction, McpServerRow, scope_label, server_actions,
    write_mcp_server_enabled,
};
pub use crate::components::permissions_dialog::{
    ModePersistenceScope, PanelNotice, PermissionRuleEntry, PermissionRuleScope,
    PermissionRuleScopeChoice, PermissionSettingsLayer, PermissionSnapshot,
    PermissionSnapshotInput, PermissionsCallbacks, PermissionsDialog, PermissionsDialogOptions,
    PermissionsView, RuleTab, collect_settings_layers, display_rule_text,
    parse_settings_rule_value, rule_reason,
};
pub use crate::components::tasks_dialog::{
    TaskKind, TasksDialog, TasksDialogCallbacks, TasksDialogOptions, TasksDialogRow,
    TasksDialogSnapshot, TasksDialogView, TasksDumpMode, TasksNotice, TasksNoticeKind,
    format_task_duration, format_tasks_dump, format_tasks_dump_for_mode, load_tasks_snapshot,
    status_visual,
};
