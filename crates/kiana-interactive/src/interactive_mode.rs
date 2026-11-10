//! modes/interactive/interactive-mode.ts —— 交互模式核心。
//!
//! 本文件由 §7.2 的拆分协议拆出，只包含 TS `interactive-mode.ts` 的**核心**：
//! 模块级辅助类型/函数/常量、`InteractiveModeOptions`、`InteractiveMode` 的字段与构造器、
//! 自动补全装配、启动通知、TUI 挂载/切换、`init()`、`run()`、包/tmux/changelog/安装遥测
//! 检查、路径显示辅助函数，以及 `stop()`（TS 7086–7105 行）。
//!
//! 其余方法在 [`session`]、[`events`]、[`render`]、[`lifecycle`]、[`resources`]、
//! [`reload`]、[`input`]、[`extension_ui`]、[`commands`] 子模块里按 TS 章节实现。拆出的
//! 子模块在自己的 `//!` 里说明边界；`commands/mod.rs` 用 `CONTRACT` 注释列出本文件
//! 对它的调用签名。
//!
//! # 借用纪律（所有子模块必须遵守）
//!
//! `InteractiveMode` 是 `Rc<RefCell<InteractiveModeInner>>` 的薄包装：
//!
//! * 构造器里挂到 `AgentSessionRuntime` 的回调（`set_before_session_invalidate` /
//!   `set_rebind_session`）会在会话切换**期间**被内联 `await`；调用方此时可能正持有
//!   `&mut InteractiveMode`。因此状态放在 `RefCell` 里，回调用 `Weak` 升级出新的包装层。
//! * **绝不能在 `.await` 期间持有 `inner.borrow()` / `borrow_mut()`**：回调与事件任务会在
//!   同一 LocalSet 上重入。需要跨 `await` 的状态先取出（例如 `theme_controller` 用
//!   `take()`），或只在同步的短作用域里借用。
//! * 组件句柄统一用 `Rc<RefCell<T>>`；挂到容器里的是 [`SharedComponentProxy`]，因此同一
//!   组件既在渲染树里、又能被宿主继续修改（对应 TS 的引用语义）。
//!
//! # 与上下游的关系
//!
//! * 生产代码里的唯一调用方是 `kiana-coding-agent`（`crates/kiana-coding-agent/src/app.rs`）：
//!   交互分支构造本类型后调用 `run()`；设置 `KIANA_STARTUP_BENCHMARK` 的基准路径只调用
//!   `init()` 再 `stop()`。
//! * crate 根（`lib.rs`）与 [`crate::index`] 再导出 [`InteractiveMode`] /
//!   [`InteractiveModeOptions`]；`format_crash_extension_hint` 与 `format_resume_command`
//!   也由 crate 根公开。
//! * [`session`]、[`events`]、[`render`]、[`lifecycle`] 等子模块用 `impl InteractiveMode`
//!   扩展同一个类型，并通过 `InteractiveModeInner` 的字段共享状态；字段改名会波及全部
//!   子模块。
//! * 所有终端读写都经 [`InteractiveRenderer`] / [`InteractiveTuiReference`]
//!   （定义在 `tui_renderer`）——`init()` 早期打印模型作用域的 `println!` 除外；
//!   本文件不直接拼装终端转义序列。
//!
//! # 一次典型的执行流程
//!
//! 1. 构造：[`InteractiveMode::new`] 按 TS 顺序建立渲染器、容器树、默认编辑器、footer 与
//!    主题控制器，最后把 `before_session_invalidate` / `rebind_session` 回调挂到
//!    `AgentSessionRuntime`。
//! 2. 初始化：`init()` 注册信号处理器、准备 changelog、装配 chat viewport 并挂载 TUI，
//!    等终端配色返回后构建 header，再安装 fd/rg、绑定按键、初始化扩展会话与初始消息。
//!    `is_initialized` 在 `ui.start()` 之后立即置 `true`，早于扩展初始化与初始消息渲染。
//! 3. 主循环：`run()` 启动 10ms 周期的 `ui.pump_events()` 任务与若干后台检查（模型目录、
//!    版本、包更新、tmux、启动告警），随后循环执行 `get_user_input()` →
//!    `session.prompt()`。
//! 4. 关停：信号处理器与退出命令经 `shutdown()` 调用 `stop()`；致命错误路径
//!    （`handle_fatal_runtime_error`）调用 `stop_with()`；uncaughtException 崩溃路径
//!    只直接 `ui.stop()` 后退出，死终端路径不写终端。正常退出路径
//!    （子模块的 `shutdown()`）随后结束进程（生产路径为 `std::process::exit`）。`run()` 的
//!    主循环本身没有退出分支。
//!
//! # 失败模式
//!
//! * `RefCell` 借用冲突会 panic（`BorrowMutError`）。这是本文件最主要的运行时风险，
//!   规则见上面的“借用纪律”。
//! * `switch_tui_mode` 在旧渲染器仍有 overlay 条目时返回 `false` 且不做任何切换；
//!   目标渲染器是全屏（viewport）而 `fullscreen_layout_root` 尚未由 `init()` 设置时，
//!   挂载/切换会 `expect` panic。
//! * 用户可见错误不走 `Result`：`run()` 把 `session.prompt()` 的错误经
//!   `prompt_error_message` 转成聊天区消息后继续循环。
//! * `fg` / `bold` 依赖全局主题已初始化，否则 panic（对应 TS 在同样位置的调用错误）。

#![allow(
    dead_code,
    reason = "拆分协议：interactive_mode.rs 是 §7.2 拆分的共享核心，后续单元在子模块里使用这些符号；dead_code 只在整 crate 合并后消失"
)]

use std::cell::{Cell, RefCell};
use std::future::Future;
use std::io::IsTerminal;
use std::pin::Pin;
use std::rc::{Rc, Weak};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use indexmap::{IndexMap, IndexSet};
use kiana_ai::types::{AssistantMessage, ImageContent, Model, ModelThinkingLevel};
use kiana_ai::utils::retry::is_retryable_assistant_error;
use kiana_config::keybindings::KeybindingsManager;
use kiana_config::settings::SettingsManager;
use kiana_config::settings::{FullscreenExitOutput, QuietStartup};
use kiana_core::changelog::{
    check_for_new_kiana_version, get_changelog_path, get_new_entries, normalize_changelog_links,
    parse_changelog,
};
use kiana_core::config::{APP_NAME, APP_TITLE, VERSION, get_agent_dir};
use kiana_core::http_dispatcher::shared_http_client;
use kiana_core::source_info::{SourceInfo, SourceScope};
use kiana_core::tools_manager::{ToolStatus, ensure_tool};
use kiana_ext_api::ui::{AutocompleteProviderFactory, EditorFactory, WorkingIndicatorOptions};
use kiana_resources::crash_log::{crash_log_path, take_unnotified_crash};
use kiana_resources::footer_data_provider::FooterDataProvider;
use kiana_resources::package::{DefaultPackageManager, PackageManagerOptions};
use kiana_session::agent_session::{
    AgentSession, AgentSessionError, AgentSessionUnsubscribe, PromptOptions, ScopedModel,
};
use kiana_session::agent_session_runtime::{AgentSessionRuntime, RebindSessionFn};
use kiana_session::agent_session_services::AgentSessionRuntimeDiagnostic;
use kiana_session_core::session_manager::{ReadonlySessionManager, SessionManager};
use kiana_theme::theme_controller::{
    InteractiveThemeController, ThemeControllerOptions, ThemeHost,
};
use kiana_theme::theme_tokens::ThemeColor;
use kiana_theme::{get_editor_theme, get_markdown_theme, set_registered_themes};
use kiana_tui::autocomplete::{
    ArgumentCompletionsFuture, AutocompleteItem, AutocompleteProvider,
    CombinedAutocompleteProvider, CommandEntry, SlashCommand,
};
use kiana_tui::components::editor::EditorTui;
use kiana_tui::components::markdown::MarkdownTheme;
use kiana_tui::components::scroll_view::ScrollView;
use kiana_tui::components::spacer::Spacer;
use kiana_tui::components::text_component::Text;
use kiana_tui::editor_component::EditorTextCallback;
use kiana_tui::layout_node::SharedComponent;
use kiana_tui::main_screen::TuiMainScreenRenderState;
use kiana_tui::terminal_colors::{
    ProgramStatus as TerminalProgramStatus, Terminal, TerminalColors,
};
use kiana_tui::tui::{
    Component, Container, Focusable, TuiMode, TuiMouseEvent, TuiMouseEventResult,
    TuiMouseEventType, TuiStopOptions,
};
use kiana_tui::{TerminalColorScheme, set_capability_overrides, set_keybindings};
use kiana_ui_kit::dynamic_border::DynamicBorder;
use kiana_ui_kit::keybinding_hints::{
    KeyTextFormatOptions, format_key_text, key_hint, key_text, raw_key_hint,
};
use kiana_ui_kit::kiana_logo::{kiana_logo_lines, kiana_wordmark, supports_kiana_logo};
use kiana_ui_kit::themed_text::ThemedText;

use crate::chat_viewport::{ChatViewportOptions, create_chat_viewport};
use crate::components::assistant_message::AssistantMessageComponent;
use crate::components::bash_execution::BashExecutionComponent;
use crate::components::custom_editor::{CustomEditor, CustomEditorOptions};
use crate::components::footer::FooterComponent;
use crate::components::mermaid::{
    GlobalMermaidTheme, MermaidTransformerOptions, create_mermaid_markdown_transformer,
};
use crate::components::oauth_selector::AuthSelectorProvider;
use crate::components::status_indicator::{IdleStatus, StatusIndicator};
use crate::components::tool_execution::ToolExecutionComponent;
use crate::model_search::get_model_search_text;
use crate::program_status_reporter::{
    BlockedStatus, GetSessionName, GetTerminal, ProgramStatusReporter,
};
use crate::tui_renderer::{
    InteractiveRenderer, InteractiveTuiOptions, InteractiveTuiReference, SharedComponentProxy,
};

// TS `export { createInteractiveTui, createInteractiveTuiReference } from "./tui-renderer.ts";`
pub use crate::tui_renderer::{create_interactive_tui, create_interactive_tui_reference};
use kiana_ext_api::{LocalBoxFuture, MarkdownTransformer};

// ---------------------------------------------------------------------------
// 子模块（拆分协议：这些文件由后续单元实现，本文件只声明）
// ---------------------------------------------------------------------------

pub mod commands;
pub mod events;
pub mod extension_ui;
pub mod input;
pub mod lifecycle;
pub mod reload;
pub mod render;
pub mod render_core_host;
pub mod resources;
pub mod session;

// ---------------------------------------------------------------------------
// 模块级类型与常量（TS 210–452 行）
// ---------------------------------------------------------------------------

/// 可展开/折叠的组件（TS `interface Expandable`）。
pub(crate) trait Expandable {
    /// `setExpanded(expanded)`。
    fn set_expanded(&mut self, expanded: bool);
}

/// `borderColor` 回调（与 `kiana_tui::editor_component::EditorBorderColor` 区分：扩展工厂/主题
/// 产生的颜色函数带 `Send + Sync`）。
pub(crate) type BorderColorFn = Box<dyn Fn(&str) -> String + Send + Sync>;

/// `this.autocompleteProvider` 的 Rust 适配。
///
/// TS 把同一个 provider 实例同时交给 `defaultEditor` 和扩展编辑器；Rust 的
/// `Box<dyn AutocompleteProvider>` 归编辑器独占（trait 方法需要 `&mut self`），无法共享，
/// 因此核心保存一个**重建工厂**：每次调用返回一份与当前设置等价的 provider。
/// `setCustomEditorComponent` 通过 [`InteractiveMode::autocomplete_provider_for_editor`] 取用。
pub(crate) type AutocompleteProviderBuilder = Rc<dyn Fn() -> Option<Box<dyn AutocompleteProvider>>>;

/// `onInputCallback?: (text: string) => void`。
pub(crate) type InputCallback = Box<dyn FnMut(&str)>;

/// 当前活动编辑器（TS `interface ActiveEditor extends EditorComponent`）。
///
/// 默认编辑器是 [`CustomEditor`]；扩展可以通过 `setEditorComponent` 换成任意
/// `Box<dyn EditorComponent>`。两种句柄都保留组件在容器里的**代理地址**，供
/// `setFocus(component)` 使用。
pub(crate) enum ActiveEditor {
    /// `this.defaultEditor`。
    Default {
        /// 默认编辑器（同时作为容器子项的共享句柄）。
        handle: Rc<RefCell<CustomEditor>>,
        /// 容器里代理组件的地址。
        address: usize,
    },
    /// 扩展提供的编辑器。
    Custom {
        /// 扩展编辑器（`Box` 共享句柄）。
        handle: Rc<RefCell<Box<dyn kiana_tui::editor_component::EditorComponent>>>,
        /// 容器里代理组件的地址。
        address: usize,
    },
}

impl ActiveEditor {
    /// 组件代理在渲染树里的地址（`setFocus(component)` 用）。
    pub(crate) fn address(&self) -> usize {
        match self {
            ActiveEditor::Default { address, .. } | ActiveEditor::Custom { address, .. } => {
                *address
            }
        }
    }

    /// `getText()`。
    pub(crate) fn get_text(&self) -> String {
        match self {
            ActiveEditor::Default { handle, .. } => handle.borrow().get_text(),
            ActiveEditor::Custom { handle, .. } => handle.borrow().get_text(),
        }
    }

    /// `setText(text)`。
    pub(crate) fn set_text(&self, text: &str) {
        match self {
            ActiveEditor::Default { handle, .. } => handle.borrow_mut().set_text(text),
            ActiveEditor::Custom { handle, .. } => handle.borrow_mut().set_text(text),
        }
    }

    /// `insertTextAtCursor(text)`。
    pub(crate) fn insert_text_at_cursor(&self, text: &str) {
        match self {
            ActiveEditor::Default { handle, .. } => handle.borrow_mut().insert_text_at_cursor(text),
            ActiveEditor::Custom { handle, .. } => handle.borrow_mut().insert_text_at_cursor(text),
        }
    }

    /// `getExpandedText()`。
    pub(crate) fn get_expanded_text(&self) -> String {
        match self {
            ActiveEditor::Default { handle, .. } => handle.borrow().get_expanded_text(),
            ActiveEditor::Custom { handle, .. } => handle.borrow().get_expanded_text(),
        }
    }

    /// `addToHistory(text)`。
    pub(crate) fn add_to_history(&self, text: &str) {
        match self {
            ActiveEditor::Default { handle, .. } => handle.borrow_mut().add_to_history(text),
            ActiveEditor::Custom { handle, .. } => handle.borrow_mut().add_to_history(text),
        }
    }

    /// `setAutocompleteProvider(provider)`。
    pub(crate) fn set_autocomplete_provider(&self, provider: Box<dyn AutocompleteProvider>) {
        match self {
            ActiveEditor::Default { handle, .. } => {
                handle.borrow_mut().set_autocomplete_provider(provider)
            }
            // TS 的 `this.editor.setAutocompleteProvider?.(provider)`：扩展编辑器实现
            // `EditorComponent::set_autocomplete_provider`（参数是 `Box<dyn Any>`），
            // `CustomEditor` 的 downcast 目标同样是 `Box<dyn AutocompleteProvider>`。
            ActiveEditor::Custom { handle, .. } => handle
                .borrow_mut()
                .set_autocomplete_provider(Box::new(provider)),
        }
    }

    /// `setPaddingX(padding)`。
    pub(crate) fn set_padding_x(&self, padding: usize) {
        match self {
            ActiveEditor::Default { handle, .. } => {
                handle.borrow_mut().set_padding_x(padding as f64)
            }
            ActiveEditor::Custom { handle, .. } => handle.borrow_mut().set_padding_x(padding),
        }
    }

    /// `setAutocompleteMaxVisible(n)`。
    pub(crate) fn set_autocomplete_max_visible(&self, max_visible: usize) {
        match self {
            ActiveEditor::Default { handle, .. } => handle
                .borrow_mut()
                .set_autocomplete_max_visible(max_visible as f64),
            ActiveEditor::Custom { handle, .. } => handle
                .borrow_mut()
                .set_autocomplete_max_visible(max_visible),
        }
    }

    /// `borderColor = fn`。
    pub(crate) fn set_border_color(&self, color: Option<BorderColorFn>) {
        match self {
            ActiveEditor::Default { handle, .. } => match color {
                Some(color) => handle.borrow_mut().set_border_color_fn(Arc::from(color)),
                None => handle
                    .borrow_mut()
                    .set_border_color_fn(Arc::new(|text: &str| text.to_string())),
            },
            ActiveEditor::Custom { handle, .. } => {
                let color: Option<kiana_tui::editor_component::EditorBorderColor> =
                    color.map(|color| color as kiana_tui::editor_component::EditorBorderColor);
                handle.borrow_mut().set_border_color(color)
            }
        }
    }

    /// `onSubmit = fn`。
    pub(crate) fn set_on_submit(&self, callback: Option<EditorTextCallback>) {
        match self {
            ActiveEditor::Default { handle, .. } => handle.borrow_mut().set_on_submit(callback),
            ActiveEditor::Custom { handle, .. } => handle.borrow_mut().set_on_submit(callback),
        }
    }

    /// `onChange = fn`。
    pub(crate) fn set_on_change(&self, callback: Option<EditorTextCallback>) {
        match self {
            ActiveEditor::Default { handle, .. } => handle.borrow_mut().set_on_change(callback),
            ActiveEditor::Custom { handle, .. } => handle.borrow_mut().set_on_change(callback),
        }
    }
}

/// 某个模式在压缩期间排队的消息（TS `CompactionQueuedMessage`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CompactionQueuedMessage {
    /// 文本。
    pub text: String,
    /// `"steer" | "followUp"`。
    pub mode: CompactionQueueMode,
}

/// [`CompactionQueuedMessage::mode`]。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CompactionQueueMode {
    /// `"steer"`。
    Steer,
    /// `"followUp"`。
    FollowUp,
}

/// 选择器身份 token（TS 里是 `object`，用引用相等判断过期）。
pub(crate) struct SelectorToken;

/// 扩展对话框栈的一项（TS 的内联对象）。
pub(crate) struct ExtensionDialog {
    /// 对话框组件。
    pub component: SharedComponent,
    /// 阻塞状态。
    pub blocked: Option<BlockedStatus>,
}

/// 扩展终端输入监听器的登记项（TS 的 `Set<{handler, unsubscribe}>` 元素）。
pub(crate) struct ExtensionTerminalInputSubscription {
    /// 处理器。
    pub handler: kiana_ext_api::ui::TerminalInputHandler,
    /// 退订句柄。
    pub unsubscribe: Box<dyn Fn() + Send + Sync>,
}

/// 内置 header 的两种形态（TS `Component | undefined`，静默启动时是空 `Text`）。
pub(crate) enum HeaderComponent {
    /// `BuiltInHeader`（可展开）。
    BuiltIn(Rc<RefCell<BuiltInHeader>>),
    /// 静默启动时的 `new Text("", 0, 0)`。
    Empty,
}

// EIO: tty reads/ioctls from an orphaned background process group, or writes after hangup.
// ENOTTY: the tty was revoked (macOS) and stdin is no longer a terminal.
const DEAD_TERMINAL_ERROR_CODES: [&str; 4] = ["EIO", "EPIPE", "ENOTCONN", "ENOTTY"];

/// `isDeadTerminalError(error)`。
pub(crate) fn is_dead_terminal_error_code(code: &str) -> bool {
    DEAD_TERMINAL_ERROR_CODES.contains(&code)
}

/// TS 常量（逐字）。
pub(crate) const ANTHROPIC_SUBSCRIPTION_AUTH_WARNING: &str = "Anthropic subscription auth is active. Third-party harness usage draws from extra usage and is billed per token, not your Claude plan limits. Manage extra usage at https://claude.ai/settings/usage. Disable this warning in /settings.";

/// TS 常量（逐字）。
pub(crate) const RADIUS_LOGIN_INTRO: &str =
    "Radius is a service crafted for Kiana by the builders of Kiana, Earendil Works";

/// `AUTH_TYPE_ORDER = { oauth: 0, api_key: 1 }` 的排序键。
pub(crate) fn auth_type_order(auth_type: kiana_ai::auth::types::AuthType) -> u8 {
    match auth_type {
        kiana_ai::auth::types::AuthType::OAuth => 0,
        kiana_ai::auth::types::AuthType::ApiKey => 1,
    }
}

/// `isAnthropicSubscriptionAuthKey(key)`。
pub(crate) fn is_anthropic_subscription_auth_key(api_key: Option<&str>) -> bool {
    api_key.is_some_and(|key| key.starts_with("sk-ant-oat"))
}

/// `isUnknownModel(model)`。
pub(crate) fn is_unknown_model(model: Option<&Model>) -> bool {
    model.is_some_and(|model| {
        model.provider == "unknown" && model.id == "unknown" && model.api == "unknown"
    })
}

/// `hasDefaultModelProvider(providerId)`。
pub(crate) fn has_default_model_provider(provider_id: &str) -> bool {
    kiana_config::models::resolver::DEFAULT_MODEL_PER_PROVIDER.contains_key(provider_id)
}

/// `llamaCppPostLoginGuidance(actionLabel, loadedModelCount)`。
pub(crate) fn llama_cpp_post_login_guidance(
    action_label: &str,
    loaded_model_count: usize,
) -> String {
    if loaded_model_count == 0 {
        format!(
            "{action_label}. No llama.cpp models are loaded. Use /llama to load a model, then /model to select it."
        )
    } else {
        format!(
            "{action_label}. Use /model to select a loaded llama.cpp model, or /llama to manage models."
        )
    }
}

/// `quoteIfNeeded(value)`：安全字符原样返回，否则单引号包裹并转义 `'`。
pub(crate) fn quote_if_needed(value: &str) -> String {
    let safe = !value.is_empty()
        && value.bytes().all(|byte| {
            byte.is_ascii_alphanumeric()
                || matches!(byte, b'_' | b'-' | b'.' | b'/' | b'~' | b':' | b'@')
        });
    if safe {
        value.to_string()
    } else {
        format!("'{}'", value.replace('\'', "'\\''"))
    }
}

/// `formatCrashExtensionHint(extensionMatches)`（逐字，见 spec §2.50.1）。
///
/// 空字符串先被过滤；没有有效匹配时返回 `None`。返回文案按英文单复数选择
/// `extension/it` 或 `extensions/them`，并嵌入 [`APP_NAME`] 作为提示里的命令名。
/// 这是纯函数：不读取全局状态，测试可以直接比对字符串（见本文件 `tests`）。
pub fn format_crash_extension_hint(extension_matches: Option<&[String]>) -> Option<String> {
    let matches: Vec<&String> = extension_matches
        .map(|matches| {
            matches
                .iter()
                .filter(|item| !item.is_empty())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    if matches.is_empty() {
        return None;
    }
    let quoted: Vec<String> = matches.iter().map(|item| format!("`{item}`")).collect();
    let labels = if quoted.len() == 1 {
        quoted[0].clone()
    } else if quoted.len() == 2 {
        quoted.join(" and ")
    } else {
        format!(
            "{}, and {}",
            quoted[..quoted.len() - 1].join(", "),
            quoted[quoted.len() - 1]
        )
    };
    let noun = if matches.len() == 1 {
        "extension"
    } else {
        "extensions"
    };
    let pronoun = if matches.len() == 1 { "it" } else { "them" };
    Some(format!(
        "A stack frame came from loaded {noun} {labels}, which may be involved. Try disabling {pronoun} with `{APP_NAME} config`, or run `{APP_NAME} -ne` to confirm."
    ))
}

/// `formatResumeCommand` 的会话信息源。
///
/// TS 直接接收 `SessionManager`；测试用对象字面量伪造。Rust 用 trait 表达同一组方法
/// （`SessionManager` 与 `AgentSession` 都实现它）。
pub trait ResumeCommandSource {
    /// `isPersisted()`。
    fn is_persisted(&self) -> bool;
    /// `getSessionFile()`。
    fn get_session_file(&self) -> Option<String>;
    /// `getSessionId()`。
    fn get_session_id(&self) -> String;
    /// `getSessionDir()`。
    fn get_session_dir(&self) -> String;
    /// `usesDefaultSessionDir()`。
    fn uses_default_session_dir(&self) -> bool;
}

impl ResumeCommandSource for SessionManager {
    fn is_persisted(&self) -> bool {
        SessionManager::is_persisted(self)
    }

    fn get_session_file(&self) -> Option<String> {
        SessionManager::get_session_file(self).map(str::to_string)
    }

    fn get_session_id(&self) -> String {
        SessionManager::get_session_id(self).to_string()
    }

    fn get_session_dir(&self) -> String {
        SessionManager::get_session_dir(self).to_string()
    }

    fn uses_default_session_dir(&self) -> bool {
        SessionManager::uses_default_session_dir(self)
    }
}

impl ResumeCommandSource for AgentSession {
    fn is_persisted(&self) -> bool {
        self.with_session_manager_mut(|manager| manager.is_persisted())
    }

    fn get_session_file(&self) -> Option<String> {
        self.with_session_manager_mut(|manager| manager.get_session_file().map(str::to_string))
    }

    fn get_session_id(&self) -> String {
        self.session_id()
    }

    fn get_session_dir(&self) -> String {
        self.with_session_manager_mut(|manager| manager.get_session_dir().to_string())
    }

    fn uses_default_session_dir(&self) -> bool {
        self.with_session_manager_mut(|manager| manager.uses_default_session_dir())
    }
}

/// `formatResumeCommand(sessionManager)`（逐字）。
pub fn format_resume_command(session_manager: &dyn ResumeCommandSource) -> Option<String> {
    format_resume_command_with(session_manager, std::io::stdout().is_terminal())
}

/// [`format_resume_command`] 的可注入 TTY 版本（TS 测试通过改写 `process.stdout.isTTY`
/// 覆盖两条分支；Rust 的 `Stdout` 无法在运行期替换，因此把判定作为参数）。
///
/// 返回 `None` 的条件（按检查顺序）：stdout 不是 TTY、会话未持久化、取不到会话文件，
/// 或会话文件为空 / 在磁盘上不存在。成功时返回可直接粘贴到 shell 的命令；会话目录不是
/// 默认目录时额外拼 `--session-dir`（参数经 `quote_if_needed` 转义）。
pub fn format_resume_command_with(
    session_manager: &dyn ResumeCommandSource,
    stdout_is_tty: bool,
) -> Option<String> {
    if !stdout_is_tty {
        return None;
    }
    if !session_manager.is_persisted() {
        return None;
    }
    let session_file = session_manager.get_session_file()?;
    if session_file.is_empty() || !std::path::Path::new(&session_file).exists() {
        return None;
    }
    let mut args = vec![APP_NAME.to_string()];
    if !session_manager.uses_default_session_dir() {
        args.push("--session-dir".to_string());
        args.push(quote_if_needed(&session_manager.get_session_dir()));
    }
    args.push("--session".to_string());
    args.push(session_manager.get_session_id());
    Some(args.join(" "))
}

/// [`should_suggest_bug_report`] 的取消/中止词匹配。
///
/// TS `/\b(?:abort(?:ed)?|cancel(?:l?ed)?)\b/i`：JS 的 `\b` 是 ASCII 词边界，这里用
/// `(?-u:\b)` 关闭 Unicode 语义。
fn abort_or_cancel_regex() -> &'static regex::Regex {
    static REGEX: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(r"(?i)(?-u:\b)(?:abort(?:ed)?|cancel(?:l?ed)?)(?-u:\b)")
            .expect("abort/cancel regex is valid")
    });
    &REGEX
}

/// `maybeSuggestBugReport` 的纯判定部分：`stopReason === "error"`、不是可重试错误、且
/// 错误信息里没有 abort/cancel 词。
pub(crate) fn should_suggest_bug_report(message: &AssistantMessage) -> bool {
    if message.stop_reason != kiana_ai::types::StopReason::Error {
        return false;
    }
    if is_retryable_assistant_error(message) {
        return false;
    }
    let error_message = message.error_message.as_deref().unwrap_or("");
    !abort_or_cancel_regex().is_match(error_message)
}

// ---------------------------------------------------------------------------
// ExpandableText / BuiltInHeader（TS 230–278 行）
// ---------------------------------------------------------------------------

/// `class ExpandableText extends ThemedText implements Expandable`。
///
/// Rust 没有类继承：这里用组合包一层 [`ThemedText`]，展开状态由 `Rc<Cell<bool>>` 与
/// `ThemedText` 的惰性构建闭包共享。
pub(crate) struct ExpandableText {
    text: ThemedText,
    state: Rc<Cell<bool>>,
    collapsed: Rc<dyn Fn() -> String>,
    expanded: Rc<dyn Fn() -> String>,
    padding_x: usize,
    padding_y: usize,
}

impl ExpandableText {
    /// `constructor(getCollapsedText, getExpandedText, expanded = false, paddingX = 0, paddingY = 0)`。
    pub(crate) fn new(
        get_collapsed_text: Rc<dyn Fn() -> String>,
        get_expanded_text: Rc<dyn Fn() -> String>,
        expanded: bool,
        padding_x: usize,
        padding_y: usize,
    ) -> Self {
        let state = Rc::new(Cell::new(expanded));
        let closure_state = Rc::clone(&state);
        let closure_collapsed = Rc::clone(&get_collapsed_text);
        let closure_expanded = Rc::clone(&get_expanded_text);
        let text = ThemedText::with_padding(
            Box::new(move || {
                if closure_state.get() {
                    closure_expanded()
                } else {
                    closure_collapsed()
                }
            }),
            padding_x,
            padding_y,
        );
        ExpandableText {
            text,
            state,
            collapsed: get_collapsed_text,
            expanded: get_expanded_text,
            padding_x,
            padding_y,
        }
    }

    /// `setExpanded(expanded)`。
    pub(crate) fn set_expanded(&mut self, expanded: bool) {
        self.state.set(expanded);
        Component::invalidate(self);
    }

    /// 当前是否展开。
    pub(crate) fn expanded(&self) -> bool {
        self.state.get()
    }

    /// 折叠文本闭包。
    pub(crate) fn collapsed_text(&self) -> String {
        (self.collapsed)()
    }

    /// 展开文本闭包。
    pub(crate) fn expanded_text(&self) -> String {
        (self.expanded)()
    }

    /// 内边距。
    pub(crate) fn padding(&self) -> (usize, usize) {
        (self.padding_x, self.padding_y)
    }
}

impl Component for ExpandableText {
    fn render(&mut self, width: usize) -> Vec<String> {
        self.text.render(width)
    }

    fn handles_input(&self) -> bool {
        false
    }

    fn invalidate(&mut self) {
        self.text.invalidate();
    }

    /// TS `isExpandable(child)` 的结构判定的 Rust 对应物：容器子项（`&mut dyn Component`）
    /// 需要先向下转型才能调用 `set_expanded`（TS 走 `"setExpanded" in child`）。
    fn as_any(&self) -> Option<&dyn std::any::Any> {
        Some(self)
    }

    /// 可变向下转型（见 [`Self::as_any`]）。
    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }
}

/// 内置 header：点击 logo（前两行、留白一列之后）播放彩蛋。
///
/// TS `handleMouse` 的命中条件（逐字）：`type === "click"`、`y <= 1`、`1 <= x <= 4`、
/// 已注册回调；**不检查 `button`**。回调参数是屏幕绝对坐标换算出的 logo 左上角：
/// `column = screenX - x + 1`、`row = screenY - y`。
pub(crate) struct BuiltInHeader {
    inner: ExpandableText,
    /// `onLogoClick`。
    pub(crate) on_logo_click: Option<Box<dyn FnMut(usize, usize)>>,
}

impl BuiltInHeader {
    /// `new BuiltInHeader(..., expanded, paddingX = 1, paddingY = 0)`。
    pub(crate) fn new(
        get_collapsed_text: Rc<dyn Fn() -> String>,
        get_expanded_text: Rc<dyn Fn() -> String>,
        expanded: bool,
        padding_x: usize,
        padding_y: usize,
    ) -> Self {
        BuiltInHeader {
            inner: ExpandableText::new(
                get_collapsed_text,
                get_expanded_text,
                expanded,
                padding_x,
                padding_y,
            ),
            on_logo_click: None,
        }
    }
}

impl Component for BuiltInHeader {
    fn render(&mut self, width: usize) -> Vec<String> {
        self.inner.render(width)
    }

    fn handles_input(&self) -> bool {
        false
    }

    fn invalidate(&mut self) {
        self.inner.invalidate();
    }

    fn handle_mouse(&mut self, event: &TuiMouseEvent) -> Option<TuiMouseEventResult> {
        if event.event_type != TuiMouseEventType::Click || event.y > 1 || event.x < 1 || event.x > 4
        {
            return None;
        }
        let on_logo_click = self.on_logo_click.as_mut()?;
        let column = event.screen_x - event.x + 1;
        let row = event.screen_y - event.y;
        on_logo_click(
            usize::try_from(column).unwrap_or(0),
            usize::try_from(row).unwrap_or(0),
        );
        Some(TuiMouseEventResult {
            handled: Some(true),
            ..Default::default()
        })
    }
}

impl Expandable for BuiltInHeader {
    fn set_expanded(&mut self, expanded: bool) {
        self.inner.set_expanded(expanded);
    }
}

// ---------------------------------------------------------------------------
// InteractiveModeOptions（TS 429–452 行）
// ---------------------------------------------------------------------------

/// `InteractiveModeOptions`。
///
/// TS 的字段全部可选；Rust 用 `Option` 一一对应。`tui_mode` 使用
/// [`kiana_config::settings::TuiMode`]（设置文件的枚举），转换成渲染器的
/// [`kiana_tui::TuiMode`] 在构造器里完成。
#[derive(Default)]
pub struct InteractiveModeOptions {
    /// 迁移到 auth.json 的 provider（启动时告警）。
    pub migrated_providers: Option<Vec<String>>,
    /// TUI 初始化前收集的诊断。
    pub startup_diagnostics: Option<Vec<AgentSessionRuntimeDiagnostic>>,
    /// 会话模型无法恢复时的告警。
    pub model_fallback_message: Option<String>,
    /// reload 后要信任的 cwd。
    pub auto_trust_on_reload_cwd: Option<String>,
    /// 启动时要发送的初始消息。
    pub initial_message: Option<String>,
    /// 初始消息附带的图片。
    pub initial_images: Option<Vec<ImageContent>>,
    /// 初始消息之后继续发送的消息。
    pub initial_messages: Option<Vec<String>>,
    /// 强制显示详细启动信息。
    pub verbose: Option<bool>,
    /// TUI 布局模式。
    pub tui_mode: Option<kiana_config::settings::TuiMode>,
    /// 本次调用的初始主题设置。
    pub initial_theme_setting: Option<String>,
    /// 终端实现（缺省为进程终端）。
    pub terminal: Option<Box<dyn Terminal>>,
}

/// 把设置枚举转换成渲染器枚举。
fn renderer_tui_mode(mode: kiana_config::settings::TuiMode) -> TuiMode {
    match mode {
        kiana_config::settings::TuiMode::Regular => TuiMode::Regular,
        kiana_config::settings::TuiMode::Fullscreen => TuiMode::Fullscreen,
    }
}

/// 渲染器 TUI 模式 → 设置文件的 [`kiana_config::settings::TuiMode`]。
///
/// `switch_tui_mode` 末尾用它把新模式写回 `options.tui_mode`，让后续读取选项的代码
/// 看到切换后的值（方向与 `renderer_tui_mode` 相反）。
pub(crate) fn settings_tui_mode(mode: TuiMode) -> kiana_config::settings::TuiMode {
    match mode {
        TuiMode::Regular => kiana_config::settings::TuiMode::Regular,
        TuiMode::Fullscreen => kiana_config::settings::TuiMode::Fullscreen,
    }
}

// ---------------------------------------------------------------------------
// InteractiveMode 的状态
// ---------------------------------------------------------------------------

/// [`InteractiveMode`] 的全部字段（TS §2.50.2 字段表一一对应）。
///
/// 字段对子模块可见（`interactive_mode::*` 是 `crate::interactive_mode` 的后代模块），
/// 后续单元在各自文件里写 `impl InteractiveMode` 时通过 `self.inner.borrow_mut().field`
/// 访问。**借用纪律见文件头**。
pub(crate) struct InteractiveModeInner {
    // --- 运行时与渲染 ---
    /// `Rc` 共享：`ProgramStatusReporter` 的会话名 getter 与主题/设置的读取闭包需要
    /// 在**不借 `inner`** 的情况下读取实时 `runtimeHost.session`（见构造器）。
    pub(crate) runtime_host: Rc<AgentSessionRuntime>,
    pub(crate) renderer: Rc<RefCell<InteractiveRenderer>>,
    pub(crate) ui: InteractiveTuiReference,
    pub(crate) main_screen_render_state: Option<TuiMainScreenRenderState>,

    // --- 容器 ---
    pub(crate) loaded_resources_container: Rc<RefCell<Container>>,
    pub(crate) chat_container: Rc<RefCell<Container>>,
    pub(crate) document_container: Rc<RefCell<Container>>,
    pub(crate) transcript_scroll_view: Option<Rc<RefCell<ScrollView>>>,
    pub(crate) fullscreen_layout_root: Option<SharedComponent>,
    pub(crate) pending_messages_container: Rc<RefCell<Container>>,
    pub(crate) status_container: Rc<RefCell<Container>>,
    pub(crate) widget_container_above: Rc<RefCell<Container>>,
    pub(crate) widget_container_below: Rc<RefCell<Container>>,
    pub(crate) editor_container: Rc<RefCell<Container>>,
    pub(crate) footer_container: Rc<RefCell<Container>>,
    pub(crate) header_container: Rc<RefCell<Container>>,

    // --- 编辑器与自动补全 ---
    pub(crate) default_editor: Rc<RefCell<CustomEditor>>,
    /// 当前活动编辑器。扩展替换编辑器时，子模块需要同时替换 `editor_container` 的子项并
    /// 更新 [`ActiveEditor::address`]（[`ActiveEditor::Custom`] 分支保存的是新代理地址）。
    pub(crate) editor: ActiveEditor,
    pub(crate) editor_component_factory: Option<EditorFactory>,
    /// `this.autocompleteProvider` 的 Rust 等价物（重建工厂，见
    /// [`AutocompleteProviderBuilder`]）。`setup_autocomplete_provider` 每次重建后刷新它，
    /// 扩展编辑器替换时用 [`InteractiveMode::autocomplete_provider_for_editor`] 取一份新实例。
    pub(crate) autocomplete_provider: Option<AutocompleteProviderBuilder>,
    pub(crate) autocomplete_provider_wrappers: Vec<AutocompleteProviderFactory>,
    pub(crate) fd_path: Option<String>,
    pub(crate) active_selector_token: Option<Rc<SelectorToken>>,
    pub(crate) active_selector_dispose: Option<Box<dyn FnMut()>>,

    // --- footer ---
    pub(crate) footer: Rc<RefCell<FooterComponent>>,
    pub(crate) footer_data_provider: Arc<FooterDataProvider>,

    // --- 基础状态 ---
    pub(crate) keybindings: Arc<KeybindingsManager>,
    pub(crate) version: String,
    pub(crate) is_initialized: bool,
    pub(crate) on_input_callback: Option<InputCallback>,
    pub(crate) pending_user_inputs: Vec<String>,
    pub(crate) active_status_indicator: Option<Box<dyn StatusIndicator>>,
    pub(crate) active_working_indicator_embedded: bool,
    pub(crate) idle_status: IdleStatus,
    pub(crate) working_message: Option<String>,
    pub(crate) working_visible: bool,
    pub(crate) working_indicator_options: Option<WorkingIndicatorOptions>,
    /// TS `private readonly defaultWorkingMessage = "Working"`。
    pub(crate) default_working_message: String,
    /// TS `private readonly defaultHiddenThinkingLabel = "Thinking..."`。
    pub(crate) default_hidden_thinking_label: String,
    pub(crate) hidden_thinking_label: String,
    pub(crate) last_sigint_time: f64,
    pub(crate) last_escape_time: f64,
    pub(crate) changelog_markdown: Option<String>,
    pub(crate) startup_notices_shown: bool,
    pub(crate) anthropic_subscription_warning_shown: bool,

    // --- 状态行跟踪 ---
    pub(crate) last_status_spacer: Option<SharedComponent>,
    pub(crate) last_status_text: Option<SharedComponent>,
    pub(crate) last_status_message: String,
    pub(crate) managed_tool_status_started: bool,

    // --- 流式消息跟踪 ---
    pub(crate) streaming_component: Option<Rc<RefCell<AssistantMessageComponent>>>,
    /// 边界压缩时已经渲染过的条目 id：对应的 `EntryAppended` 事件随后到达时只移除标记、
    /// 不再重复渲染（消费点在 `events` 子模块）。
    pub(crate) entries_rendered_by_boundary_compaction: IndexSet<String>,
    pub(crate) streaming_message: Option<AssistantMessage>,

    // --- 工具执行跟踪 ---
    pub(crate) pending_tools: IndexMap<String, Rc<RefCell<ToolExecutionComponent>>>,

    // --- 显示选项 ---
    pub(crate) tool_output_expanded: bool,
    pub(crate) hide_thinking_block: bool,
    pub(crate) output_pad: usize,
    pub(crate) mermaid_markdown_transformer: MarkdownTransformer,

    // --- 技能命令 ---
    pub(crate) skill_commands: IndexMap<String, String>,

    // --- 订阅与信号 ---
    pub(crate) unsubscribe: Option<AgentSessionUnsubscribe>,
    /// 当前事件 drain 任务；重绑会话时先 abort，丢弃旧会话已排队但尚未处理的事件。
    pub(crate) event_task: Option<tokio::task::JoinHandle<()>>,
    pub(crate) signal_cleanup_handlers: Vec<Box<dyn FnMut()>>,

    // --- bash ---
    pub(crate) is_bash_mode: bool,
    pub(crate) bash_component: Option<Rc<RefCell<BashExecutionComponent>>>,
    pub(crate) pending_bash_components: Vec<Rc<RefCell<BashExecutionComponent>>>,

    // --- 压缩 / 重试 ---
    pub(crate) auto_compaction_escape_handler: Option<Box<dyn FnMut()>>,
    pub(crate) retry_escape_handler: Option<Box<dyn FnMut()>>,
    pub(crate) compaction_queued_messages: Vec<CompactionQueuedMessage>,

    // --- 关闭 ---
    pub(crate) shutdown_requested: bool,
    pub(crate) is_shutting_down: bool,

    // --- 程序状态（OSC 7501）---
    pub(crate) program_status: ProgramStatusReporter,

    // --- bug 提示 ---
    pub(crate) bug_report_hint_shown: bool,
    pub(crate) install_change_warning_shown: bool,

    // --- 扩展 UI ---
    pub(crate) extension_dialogs: Vec<ExtensionDialog>,
    /// 第 12 周渲染核心宿主：构造时读一次 `KIANA_NEXT_RENDER_CORE`，运行期不再读环境
    /// 变量。开关关闭时所有方法都是 no-op（默认渲染路径不变）。
    pub(crate) render_core_host: render_core_host::RenderCoreHost,
    pub(crate) extension_terminal_input_subscriptions: Vec<ExtensionTerminalInputSubscription>,
    pub(crate) extension_widgets_above: IndexMap<String, SharedComponent>,
    pub(crate) extension_widgets_below: IndexMap<String, SharedComponent>,
    pub(crate) custom_footer: Option<SharedComponent>,
    pub(crate) built_in_header: Option<HeaderComponent>,
    pub(crate) custom_header: Option<SharedComponent>,

    // --- 选项与主题 ---
    pub(crate) options: InteractiveModeOptions,
    pub(crate) on_right_click_paste: Rc<dyn Fn()>,
    pub(crate) auto_trust_on_reload_cwd: Option<String>,
    /// 主题控制器。`applyFromSettings` / `waitForTerminalColors` / `disableAutoSync` /
    /// `rebindTui` 期间会被 `take()` 成 `None`（避免跨 `await` 持有 `RefCell` 借用），
    /// 子模块不要假设它始终是 `Some`。
    pub(crate) theme_controller: Option<InteractiveThemeController>,

    // --- logo 点击队列（见 `BuiltInHeader::handle_mouse`）---
    /// logo 点击的延迟队列：`BuiltInHeader::handle_mouse` 在 TUI 事件分发（渲染器已被可变
    /// 借用）期间不能重入渲染器，因此先入队，由 `spawn_local` 任务在事件返回后播放动画。
    pub(crate) pending_logo_clicks: Rc<RefCell<Vec<(usize, usize)>>>,
    /// 是否已有一个排队的 logo 播放任务。
    pub(crate) logo_drain_scheduled: Rc<Cell<bool>>,

    /// Rust-only：git 分支监听器的退订句柄（TS 不保存句柄，监听器随 provider 释放）。
    pub(crate) branch_change_unsubscribe: Option<kiana_core::event_bus::Unsubscribe>,
}

/// `MAX_WIDGET_LINES`（TS `static readonly MAX_WIDGET_LINES = 10`）。
pub(crate) const MAX_WIDGET_LINES: usize = 10;

/// 交互模式核心。
///
/// 字段都在 `InteractiveModeInner`：构造器里挂给 `AgentSessionRuntime` 的回调需要在
/// 会话切换期间重入同一对象，`RefCell` 提供 TS 的 `this` 引用语义。所有方法都是薄包装，
/// 只在同步短作用域内借用。
#[derive(Clone)]
pub struct InteractiveMode {
    pub(crate) inner: Rc<RefCell<InteractiveModeInner>>,
}

/// 构造器期间的自我引用槽：`Rc::new` 之前创建的闭包（主题控制器 / 右键粘贴）在运行时
/// 通过它升级出 `InteractiveMode`。
type WeakSlot = Rc<RefCell<Option<Weak<RefCell<InteractiveModeInner>>>>>;

/// 把 `Rc<RefCell<T>>` 变成共享组件句柄（等价于 `Container.addChild(component)` 保存引用）。
fn shared<T: Component + 'static>(handle: &Rc<RefCell<T>>) -> SharedComponent {
    Rc::clone(handle) as SharedComponent
}

/// 把共享句柄包成容器子项（渲染/输入/焦点全部转发）。
fn boxed_shared(handle: &SharedComponent) -> Box<dyn Component> {
    Box::new(SharedComponentProxy::new(Rc::clone(handle)))
}

fn new_container() -> Rc<RefCell<Container>> {
    Rc::new(RefCell::new(Container::new()))
}

/// 把共享容器挂到渲染器的顶层子项列表（`mountInteractiveTui` / `switchTuiMode`）。
///
/// 不能走 `Tui::add_child`：全屏渲染器会把每个子项再包成
/// `SharedHandle(OwnedComponent(..))`，而 [`SharedComponentProxy`] 只实现
/// `for_each_container_child`（不实现 `container_children`），`OwnedComponent` 的默认
/// `for_each_container_child` 在 `container_children()` 处就断了，导致
/// `focus_target_of_address` 找不到容器里的编辑器（`setFocus(editor)` 失效、键盘输入丢失）。
/// TS 的顶层子项就是组件引用；直接写入两种渲染器的 `TuiBase.children` 才能保留引用语义
/// （与 `TuiAltScreen::add_child_shared` 等价，但子项仍是本文件的代理类型，
/// `switchTuiMode` 的 `takeChildren`/重新挂载不会叠加第二层包装）。
fn mount_interactive_child(renderer: &mut InteractiveRenderer, component: Box<dyn Component>) {
    match renderer {
        InteractiveRenderer::Regular(main_screen) => main_screen.base_mut().add_child(component),
        InteractiveRenderer::Fullscreen(fullscreen) => fullscreen.base_mut().add_child(component),
    }
}

/// `theme.fg(color, text)`（主题未初始化时与 TS 一样视为调用错误）。
fn fg(color: ThemeColor, text: &str) -> String {
    kiana_theme::THEME
        .fg(color, text)
        .expect("theme must be initialized before rendering (TS throws here)")
}

/// `theme.bold(text)`。
fn bold(text: &str) -> String {
    kiana_theme::THEME
        .bold(text)
        .expect("theme must be initialized before rendering (TS throws here)")
}

/// 环境变量的 JS 真值判断（存在且非空）。
fn env_is_truthy(name: &str) -> bool {
    std::env::var_os(name).is_some_and(|value| !value.is_empty())
}

/// 锁 [`SettingsManager`] 共享槽（poison 也继续，避免路径上的 panic 改变行为）。
fn lock_settings(slot: &Mutex<SettingsManager>) -> MutexGuard<'_, SettingsManager> {
    slot.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// `process.cwd()`。
fn current_process_cwd() -> String {
    std::env::current_dir()
        .map(|path| path.to_string_lossy().into_owned())
        .unwrap_or_else(|_| ".".to_string())
}

/// `encodeURIComponent(value)`。
pub(crate) fn encode_uri_component(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.as_bytes() {
        let ch = *byte as char;
        if ch.is_ascii_alphanumeric()
            || matches!(ch, '-' | '_' | '.' | '!' | '~' | '*' | '\'' | '(' | ')')
        {
            out.push(ch);
        } else {
            out.push('%');
            out.push_str(&format!("{byte:02X}"));
        }
    }
    out
}

/// Node `path.basename(p)`（当前平台的路径分隔符语义）。
///
/// Windows 上 Node 的 `win32SplitPath` 把盘符（`C:`，仅 ASCII 字母 + `:`）视为 root：
/// `path.win32.basename("C:")`、`"C:\\"`、`"C:/"` 都返回 `""`，而 `"C:foo"` 返回 `"foo"`。
/// （`"1:foo"` / `"ab:cd"` 不是盘符，Node 返回完整字符串。）
pub(crate) fn path_basename(path: &str) -> String {
    if path.is_empty() {
        return String::new();
    }
    let bytes = path.as_bytes();
    let content_start = if cfg!(windows)
        && bytes.len() >= 2
        && bytes[1] == b':'
        && bytes[0].is_ascii_alphabetic()
    {
        2
    } else {
        0
    };
    let is_sep = |byte: u8| byte == b'/' || (cfg!(windows) && byte == b'\\');
    let mut end = bytes.len();
    while end > content_start && is_sep(bytes[end - 1]) {
        end -= 1;
    }
    let mut start = end;
    while start > content_start && !is_sep(bytes[start - 1]) {
        start -= 1;
    }
    path[start..end].to_string()
}

/// `new Date(value).toLocaleString()`（本地时区 + Node 默认 ICU 的 `en-US` 形状：
/// `M/D/YYYY, h:mm:ss AM/PM`）。
///
/// TS 的 `toLocaleString()` 使用**机器本地时区**（spec ca-interactive-mode 6.3 的
/// 时区风险行）；这里必须用 [`chrono::Local`]，用 UTC 会让非 UTC 用户看到错误日期/小时。
pub(crate) fn format_crash_timestamp(timestamp: Option<&str>) -> String {
    let Some(timestamp) = timestamp else {
        return "Invalid Date".to_string();
    };
    let parsed = chrono::DateTime::parse_from_rfc3339(timestamp)
        .map(|date| date.with_timezone(&chrono::Local))
        .or_else(|_| {
            chrono::NaiveDateTime::parse_from_str(timestamp, "%Y-%m-%dT%H:%M:%S%.fZ")
                .map(|naive| naive.and_utc().with_timezone(&chrono::Local))
        });
    match parsed {
        Ok(date) => date.format("%-m/%-d/%Y, %-I:%M:%S %p").to_string(),
        Err(_) => "Invalid Date".to_string(),
    }
}

// ---------------------------------------------------------------------------
// ThemeHost 适配：InteractiveRenderer 的两种渲染器都暴露 `base_mut()`
// ---------------------------------------------------------------------------

/// 把 [`InteractiveRenderer`] 适配成 [`ThemeHost`]。
struct RendererThemeHost {
    renderer: Rc<RefCell<InteractiveRenderer>>,
}

impl ThemeHost for RendererThemeHost {
    fn query_terminal_colors(
        &mut self,
        timeout_ms: u64,
        on_late_reply: Option<Box<dyn FnMut(TerminalColors)>>,
    ) -> Pin<Box<dyn Future<Output = TerminalColors> + 'static>> {
        match &mut *self.renderer.borrow_mut() {
            InteractiveRenderer::Regular(renderer) => renderer
                .base_mut()
                .query_terminal_colors_with_late_reply(timeout_ms, on_late_reply),
            InteractiveRenderer::Fullscreen(renderer) => renderer
                .base_mut()
                .query_terminal_colors_with_late_reply(timeout_ms, on_late_reply),
        }
    }

    fn set_scheme_notifications(&mut self, enabled: bool) {
        match &mut *self.renderer.borrow_mut() {
            InteractiveRenderer::Regular(renderer) => renderer
                .base_mut()
                .set_terminal_color_scheme_notifications(enabled),
            InteractiveRenderer::Fullscreen(renderer) => renderer
                .base_mut()
                .set_terminal_color_scheme_notifications(enabled),
        }
    }

    fn add_scheme_listener(&mut self, listener: Box<dyn FnMut(TerminalColorScheme)>) -> usize {
        match &mut *self.renderer.borrow_mut() {
            InteractiveRenderer::Regular(renderer) => renderer
                .base_mut()
                .on_terminal_color_scheme_change(listener),
            InteractiveRenderer::Fullscreen(renderer) => renderer
                .base_mut()
                .on_terminal_color_scheme_change(listener),
        }
    }

    fn remove_scheme_listener(&mut self, id: usize) {
        match &mut *self.renderer.borrow_mut() {
            InteractiveRenderer::Regular(renderer) => renderer
                .base_mut()
                .remove_terminal_color_scheme_listener(id),
            InteractiveRenderer::Fullscreen(renderer) => renderer
                .base_mut()
                .remove_terminal_color_scheme_listener(id),
        }
    }

    fn invalidate(&mut self) {
        self.renderer.borrow_mut().invalidate();
    }

    fn request_render(&mut self) {
        self.renderer.borrow_mut().request_render(false);
    }

    fn render_requested_handle(&self) -> Option<Rc<Cell<bool>>> {
        Some(self.renderer.borrow().render_requested_handle())
    }
}

// ---------------------------------------------------------------------------
// 共享终端：switchTuiMode 需要把同一个 Terminal 交给新渲染器
// ---------------------------------------------------------------------------

/// `Rc<RefCell<Box<dyn Terminal>>>` 的 `Terminal` 适配（转发全部调用）。
#[derive(Clone)]
struct SharedTerminal(Rc<RefCell<Box<dyn Terminal>>>);

impl Terminal for SharedTerminal {
    fn start(&mut self, on_input: Box<dyn FnMut(&str)>, on_resize: Box<dyn FnMut()>) {
        self.0.borrow_mut().start(on_input, on_resize);
    }

    fn stop(&mut self) {
        self.0.borrow_mut().stop();
    }

    fn restore_on_exit(&mut self, before: &str, after: &str) {
        self.0.borrow_mut().restore_on_exit(before, after);
    }

    fn poll_input(&mut self) -> bool {
        self.0.borrow_mut().poll_input()
    }

    fn poll_timers(&mut self, now: std::time::Instant) {
        self.0.borrow_mut().poll_timers(now);
    }

    fn poll_resize(&mut self) {
        self.0.borrow_mut().poll_resize();
    }

    #[allow(
        clippy::await_holding_refcell_ref,
        reason = "内部 future 借用共享终端直到完成；单线程 UI 上没有并发借用者，与 tui_renderer 的 query_terminal_colors 同一约定"
    )]
    fn drain_input(&mut self, max_ms: u64, idle_ms: u64) -> Pin<Box<dyn Future<Output = ()> + '_>> {
        let shared = Rc::clone(&self.0);
        Box::pin(async move {
            let mut terminal = shared.borrow_mut();
            terminal.drain_input(max_ms, idle_ms).await;
        })
    }

    fn write(&mut self, data: &str) {
        self.0.borrow_mut().write(data);
    }

    fn columns(&self) -> usize {
        self.0.borrow().columns()
    }

    fn rows(&self) -> usize {
        self.0.borrow().rows()
    }

    fn kitty_protocol_active(&self) -> bool {
        self.0.borrow().kitty_protocol_active()
    }

    fn move_by(&mut self, lines: isize) {
        self.0.borrow_mut().move_by(lines);
    }

    fn hide_cursor(&mut self) {
        self.0.borrow_mut().hide_cursor();
    }

    fn show_cursor(&mut self) {
        self.0.borrow_mut().show_cursor();
    }

    fn clear_line(&mut self) {
        self.0.borrow_mut().clear_line();
    }

    fn clear_from_cursor(&mut self) {
        self.0.borrow_mut().clear_from_cursor();
    }

    fn clear_screen(&mut self) {
        self.0.borrow_mut().clear_screen();
    }

    fn set_title(&mut self, title: &str) {
        self.0.borrow_mut().set_title(title);
    }

    fn set_progress(&mut self, active: bool) {
        self.0.borrow_mut().set_progress(active);
    }

    fn set_program_status(&mut self, status: &TerminalProgramStatus) {
        self.0.borrow_mut().set_program_status(status);
    }
}

/// 不写任何字节的终端（组件/焦点相关的单元测试用）。
struct NullTerminal;

impl Terminal for NullTerminal {
    fn start(&mut self, _on_input: Box<dyn FnMut(&str)>, _on_resize: Box<dyn FnMut()>) {}
    fn stop(&mut self) {}
    fn drain_input(
        &mut self,
        _max_ms: u64,
        _idle_ms: u64,
    ) -> Pin<Box<dyn Future<Output = ()> + '_>> {
        Box::pin(std::future::ready(()))
    }
    fn write(&mut self, _data: &str) {}
    fn columns(&self) -> usize {
        80
    }
    fn rows(&self) -> usize {
        24
    }
    fn kitty_protocol_active(&self) -> bool {
        false
    }
    fn move_by(&mut self, _lines: isize) {}
    fn hide_cursor(&mut self) {}
    fn show_cursor(&mut self) {}
    fn clear_line(&mut self) {}
    fn clear_from_cursor(&mut self) {}
    fn clear_screen(&mut self) {}
    fn set_title(&mut self, _title: &str) {}
    fn set_progress(&mut self, _active: bool) {}
    fn set_program_status(&mut self, _status: &TerminalProgramStatus) {}
}

// ---------------------------------------------------------------------------
// 自定义编辑器代理
// ---------------------------------------------------------------------------

/// `Rc<RefCell<Box<dyn EditorComponent>>>` 的容器代理。
pub(crate) struct EditorComponentProxy(
    Rc<RefCell<Box<dyn kiana_tui::editor_component::EditorComponent>>>,
);

impl EditorComponentProxy {
    /// 装箱代理并返回它在渲染树里的组件地址。
    ///
    /// 地址必须取**装箱后**的 `Box` 值（`Box` 移动时堆地址不变）；在装箱前对栈上的
    /// 临时值取地址会得到一个已失效的地址，`ui.setFocus(editor)` 永远解析不到组件，
    /// 键盘输入全部丢失。返回的 `Box` 直接交给 `Container::add_child`。
    pub(crate) fn new(
        handle: Rc<RefCell<Box<dyn kiana_tui::editor_component::EditorComponent>>>,
    ) -> (Box<Self>, usize) {
        let boxed = Box::new(EditorComponentProxy(handle));
        let address = boxed.as_ref() as *const Self as *const () as usize;
        (boxed, address)
    }
}

impl Component for EditorComponentProxy {
    fn render(&mut self, width: usize) -> Vec<String> {
        self.0.borrow_mut().render(width)
    }

    fn handle_input(&mut self, data: &str) {
        self.0.borrow_mut().handle_input(data);
    }

    fn handle_mouse(&mut self, event: &TuiMouseEvent) -> Option<TuiMouseEventResult> {
        self.0.borrow_mut().handle_mouse(event)
    }

    fn wants_key_release(&self) -> bool {
        self.0.borrow().wants_key_release()
    }

    fn handles_input(&self) -> bool {
        self.0.borrow().handles_input()
    }

    fn invalidate(&mut self) {
        self.0.borrow_mut().invalidate();
    }

    fn as_focusable(&mut self) -> Option<&mut dyn Focusable> {
        let focusable = self
            .0
            .try_borrow_mut()
            .map(|mut component| component.as_focusable().is_some())
            .unwrap_or(false);
        focusable.then_some(self as &mut dyn Focusable)
    }
}

impl Focusable for EditorComponentProxy {
    fn focused(&self) -> bool {
        self.0
            .borrow_mut()
            .as_focusable()
            .map(|focusable| focusable.focused())
            .unwrap_or(false)
    }

    fn set_focused(&mut self, focused: bool) {
        if let Some(focusable) = self.0.borrow_mut().as_focusable() {
            focusable.set_focused(focused);
        }
    }
}

/// `defaultEditor` 的容器代理（与 [`EditorComponentProxy`] 相同，只是类型具体）。
struct DefaultEditorProxy(Rc<RefCell<CustomEditor>>);

impl DefaultEditorProxy {
    /// 装箱代理并返回它在渲染树里的组件地址（语义同 [`EditorComponentProxy::new`]）。
    fn new(handle: Rc<RefCell<CustomEditor>>) -> (Box<Self>, usize) {
        let boxed = Box::new(DefaultEditorProxy(handle));
        let address = boxed.as_ref() as *const Self as *const () as usize;
        (boxed, address)
    }
}

impl Component for DefaultEditorProxy {
    fn render(&mut self, width: usize) -> Vec<String> {
        self.0.borrow_mut().render(width)
    }

    fn handle_input(&mut self, data: &str) {
        self.0.borrow_mut().handle_input(data);
    }

    fn handle_mouse(&mut self, event: &TuiMouseEvent) -> Option<TuiMouseEventResult> {
        self.0.borrow_mut().handle_mouse(event)
    }

    fn wants_key_release(&self) -> bool {
        self.0.borrow().wants_key_release()
    }

    fn handles_input(&self) -> bool {
        self.0.borrow().handles_input()
    }

    fn invalidate(&mut self) {
        self.0.borrow_mut().invalidate();
    }

    fn as_focusable(&mut self) -> Option<&mut dyn Focusable> {
        let focusable = self
            .0
            .try_borrow_mut()
            .map(|mut component| component.as_focusable().is_some())
            .unwrap_or(false);
        focusable.then_some(self as &mut dyn Focusable)
    }
}

impl Focusable for DefaultEditorProxy {
    fn focused(&self) -> bool {
        self.0
            .borrow_mut()
            .as_focusable()
            .map(|focusable| focusable.focused())
            .unwrap_or(false)
    }

    fn set_focused(&mut self, focused: bool) {
        if let Some(focusable) = self.0.borrow_mut().as_focusable() {
            focusable.set_focused(focused);
        }
    }
}

// ---------------------------------------------------------------------------
// 构造器（TS 610–676 行，顺序敏感）
// ---------------------------------------------------------------------------

impl InteractiveMode {
    /// `constructor(runtimeHost, options = {})`。
    ///
    /// 对应 spec §2.50.3 的 11 步，顺序可观测（副作用按此顺序发生），改动需同步 spec。
    ///
    /// 构造前半段先建 `WeakSlot`，等 `Rc<InteractiveModeInner>` 建好后再回填；所有需要在
    /// 构造完成后回访自身的方法（右键粘贴、主题错误显示、主题变更回调）都通过它升级出
    /// 新的 [`InteractiveMode`] 包装层。返回前安装 runtime 的两个回调：
    /// `before_session_invalidate` 在会话失效前重置扩展 UI；`rebind_session` 更新共享的
    /// 设置槽，并异步执行 `rebind_current_session(true)` 后重新应用主题。
    #[allow(
        clippy::arc_with_non_send_sync,
        reason = "AgentSessionRuntime 的回调在 UI LocalSet 上运行，类型是 Arc<dyn Fn()>（无 Send/Sync 约束）；Weak<RefCell<..>> 无法跨线程"
    )]
    pub fn new(runtime_host: AgentSessionRuntime, options: InteractiveModeOptions) -> Self {
        let runtime_host = Rc::new(runtime_host);
        let session = runtime_host.session();
        let settings_manager = session.settings_manager();
        // 「当前会话的设置管理器」共享槽。
        //
        // TS 的 `getSettingsManager: () => this.settingsManager` 与 mermaid 的
        // `getMode: () => this.settingsManager.getMermaidRenderingMode()` 每次调用都读
        // **当前**会话；而每次会话替换（`/new`、`/resume`）都会创建新的 SettingsManager
        // （main.ts#createRuntime）。mermaid 变换器的 getMode 是 `Send + Sync` 闭包，
        // 不能捕获 `!Send` 的运行时句柄，因此用 `Arc<Mutex<..>>` 槽在 rebind 时跟随会话
        // （同一会话内 `/reload` 是对同一 manager 的原位 reload，捕获的克隆能看到新值）。
        let settings_slot: Arc<Mutex<SettingsManager>> =
            Arc::new(Mutex::new(settings_manager.clone()));

        // (1) runtimeHost + setCapabilityOverrides（构造期就调用一次）
        set_capability_overrides(settings_manager.get_terminal_capability_overrides());

        // (2) tuiMode 归一
        let mut options = options;
        let tui_mode_setting = options
            .tui_mode
            .unwrap_or_else(|| settings_manager.get_tui_mode());
        let tui_mode = renderer_tui_mode(tui_mode_setting);
        options.tui_mode = Some(tui_mode_setting);
        let auto_trust_on_reload_cwd = options.auto_trust_on_reload_cwd.clone();
        let initial_theme_setting = options.initial_theme_setting.clone();

        // (3) 运行期回调需要 `Weak`：先建槽，`Rc::new` 之后填入。
        let weak_slot: WeakSlot = Rc::new(RefCell::new(None));

        let on_right_click_paste: Rc<dyn Fn()> = {
            let weak_slot = Rc::clone(&weak_slot);
            Rc::new(move || {
                let upgraded = weak_slot.borrow().as_ref().and_then(|weak| weak.upgrade());
                let Some(inner) = upgraded else {
                    return;
                };
                let mut mode = InteractiveMode { inner };
                tokio::task::spawn_local(async move {
                    mode.handle_right_click_paste().await;
                });
            })
        };

        // (4) 渲染器与 UI 引用
        let terminal = options.terminal.take();
        let renderer = create_interactive_tui(InteractiveTuiOptions {
            tui_mode,
            show_hardware_cursor: settings_manager.get_show_hardware_cursor(),
            log_directory: get_agent_dir().to_string_lossy().into_owned(),
            terminal,
            on_right_click_paste: Some(Rc::clone(&on_right_click_paste)),
            fullscreen_copy_on_select: Some(settings_manager.get_fullscreen_copy_on_select()),
            fullscreen_wheel_scroll_lines: Some(
                settings_manager.get_fullscreen_wheel_scroll_lines(),
            ),
        });
        let mut ui = create_interactive_tui_reference(Rc::clone(&renderer));
        ui.set_clear_on_shrink(settings_manager.get_clear_on_shrink());

        // (5) 容器树（header / loaded resources / chat 进 document）
        let header_container = new_container();
        let loaded_resources_container = new_container();
        let chat_container = new_container();
        let document_container = new_container();
        for child in [
            &header_container,
            &loaded_resources_container,
            &chat_container,
        ] {
            let handle = shared(child);
            document_container
                .borrow_mut()
                .add_child(boxed_shared(&handle));
        }
        let pending_messages_container = new_container();
        let status_container = new_container();
        let widget_container_above = new_container();
        let widget_container_below = new_container();

        // (6) keybindings + 全局单例注入
        let keybindings = Arc::new(KeybindingsManager::create_default());
        set_keybindings(kiana_tui::keybindings::KeybindingsManager::new(
            kiana_config::keybindings::keybindings().clone(),
            keybindings.get_resolved_bindings(),
        ));

        // (7) 默认编辑器
        let editor_padding_x = settings_manager.get_editor_padding_x();
        let autocomplete_max_visible = settings_manager.get_autocomplete_max_visible();
        let default_editor = Rc::new(RefCell::new(CustomEditor::new(
            EditorTui::from_terminal(renderer.borrow().terminal()),
            get_editor_theme(),
            Arc::clone(&keybindings),
            CustomEditorOptions {
                padding_x: Some(editor_padding_x),
                autocomplete_max_visible: Some(autocomplete_max_visible),
                embed_working_status: true,
            },
        )));
        let editor_container = new_container();
        let (editor_proxy, editor_address) = DefaultEditorProxy::new(Rc::clone(&default_editor));
        editor_container.borrow_mut().add_child(editor_proxy);
        let editor = ActiveEditor::Default {
            handle: Rc::clone(&default_editor),
            address: editor_address,
        };

        // (8) footer
        let cwd = session.session_manager().get_cwd().to_string();
        let footer_data_provider = Arc::new(FooterDataProvider::new(cwd));
        let footer = Rc::new(RefCell::new(FooterComponent::new(
            session.clone(),
            Arc::clone(&footer_data_provider)
                as Arc<dyn kiana_ext_api::ui::ReadonlyFooterDataProvider>,
        )));
        footer
            .borrow_mut()
            .set_auto_compact_enabled(session.auto_compaction_enabled());
        let footer_container = new_container();
        let footer_shared = shared(&footer);
        footer_container
            .borrow_mut()
            .add_child(boxed_shared(&footer_shared));

        // (9) 显示设置
        let hide_thinking_block = settings_manager.get_hide_thinking_block();
        let output_pad = usize::try_from(settings_manager.get_output_pad()).unwrap_or(0);

        // (10) 注册资源加载器里的主题
        let _ = set_registered_themes(session.resource_loader().get_themes().themes);

        // (11) 主题控制器（TS 在构造器最后一步创建）
        let theme_controller = {
            let controller_settings = Arc::clone(&settings_slot);
            let error_slot = Rc::clone(&weak_slot);
            let changed_slot = Rc::clone(&weak_slot);
            let mut host = RendererThemeHost {
                renderer: Rc::clone(&renderer),
            };
            InteractiveThemeController::new(
                &mut host,
                ThemeControllerOptions {
                    get_theme_setting: Box::new(move || {
                        lock_settings(&controller_settings).get_theme_setting()
                    }),
                    show_error: Box::new(move |message| {
                        let upgraded = error_slot.borrow().as_ref().and_then(|weak| weak.upgrade());
                        if let Some(inner) = upgraded {
                            InteractiveMode { inner }.show_error(message);
                        }
                    }),
                    on_changed: Box::new(move || {
                        let upgraded = changed_slot
                            .borrow()
                            .as_ref()
                            .and_then(|weak| weak.upgrade());
                        if let Some(inner) = upgraded {
                            InteractiveMode { inner }.update_editor_border_color();
                        }
                    }),
                    initial_theme_setting,
                },
            )
        };

        // 程序状态报告器（OSC 7501）。
        //
        // TS：`getTerminal: () => this.ui.terminal`、`getSessionName: () => this.sessionManager.getSessionName()`。
        // `getTerminal` 直接持有渲染器句柄（`switchTuiMode` 是原地替换，句柄不变）。
        // `getSessionName` 直接共享 `Rc<AgentSessionRuntime>`：`report()` 可以在任意调用栈
        // 里执行（本移植里常常写成 `self.inner.borrow_mut().program_status.handle_event(..)`），
        // 若经 `inner` 或 `try_borrow` 读会话名，事件驱动的上报会静默丢掉会话名（TS 是普通
        // 属性读取，永远能取到）。`runtime_host.session()` 只短暂锁内部 state，与 TS 等价。
        let program_status = {
            let renderer_for_status = Rc::clone(&renderer);
            let get_terminal: GetTerminal =
                Box::new(move || renderer_for_status.borrow().terminal());
            let runtime_for_status = Rc::clone(&runtime_host);
            let get_session_name: GetSessionName = Box::new(move || {
                runtime_for_status
                    .session()
                    .session_manager()
                    .get_session_name()
            });
            ProgramStatusReporter::new(get_terminal, get_session_name)
        };

        // Mermaid transformer：getMode 每次读取当前会话的设置（TS 捕获 this.settingsManager）。
        let mermaid_settings = Arc::clone(&settings_slot);
        let mermaid_markdown_transformer =
            create_mermaid_markdown_transformer(MermaidTransformerOptions {
                get_mode: Box::new(move || {
                    lock_settings(&mermaid_settings).get_mermaid_rendering_mode()
                }),
                theme: Some(Arc::new(GlobalMermaidTheme)),
            });

        // TS `private hiddenThinkingLabel = this.defaultHiddenThinkingLabel`。
        let default_hidden_thinking_label = "Thinking...".to_string();
        let inner = Rc::new(RefCell::new(InteractiveModeInner {
            runtime_host,
            renderer,
            ui,
            main_screen_render_state: None,
            loaded_resources_container,
            chat_container,
            document_container,
            transcript_scroll_view: None,
            fullscreen_layout_root: None,
            pending_messages_container,
            status_container,
            widget_container_above,
            widget_container_below,
            editor_container,
            footer_container,
            header_container,
            default_editor,
            editor,
            editor_component_factory: None,
            autocomplete_provider: None,
            autocomplete_provider_wrappers: Vec::new(),
            fd_path: None,
            active_selector_token: None,
            active_selector_dispose: None,
            footer,
            footer_data_provider,
            keybindings,
            version: VERSION.to_string(),
            is_initialized: false,
            on_input_callback: None,
            pending_user_inputs: Vec::new(),
            active_status_indicator: None,
            active_working_indicator_embedded: false,
            idle_status: IdleStatus::new(),
            working_message: None,
            working_visible: true,
            working_indicator_options: None,
            default_working_message: "Working".to_string(),
            default_hidden_thinking_label: default_hidden_thinking_label.clone(),
            hidden_thinking_label: default_hidden_thinking_label,
            last_sigint_time: 0.0,
            last_escape_time: 0.0,
            changelog_markdown: None,
            startup_notices_shown: false,
            anthropic_subscription_warning_shown: false,
            last_status_spacer: None,
            last_status_text: None,
            last_status_message: String::new(),
            managed_tool_status_started: false,
            streaming_component: None,
            entries_rendered_by_boundary_compaction: IndexSet::new(),
            streaming_message: None,
            pending_tools: IndexMap::new(),
            tool_output_expanded: false,
            hide_thinking_block,
            output_pad,
            mermaid_markdown_transformer,
            skill_commands: IndexMap::new(),
            unsubscribe: None,
            event_task: None,
            signal_cleanup_handlers: Vec::new(),
            is_bash_mode: false,
            bash_component: None,
            pending_bash_components: Vec::new(),
            auto_compaction_escape_handler: None,
            retry_escape_handler: None,
            compaction_queued_messages: Vec::new(),
            shutdown_requested: false,
            is_shutting_down: false,
            program_status,
            bug_report_hint_shown: false,
            install_change_warning_shown: false,
            extension_dialogs: Vec::new(),
            render_core_host: {
                // UI locale 与模型回答语言分离：这里只影响界面文案；`ui.locale` 缺失时
                // `get_ui_locale` 自带旧键 `language` → `"en"` 的回退链。
                let locale = settings_manager.get_ui_locale();
                let user_dir = kiana_core::config::get_agent_dir().join("locales");
                render_core_host::RenderCoreHost::from_env().with_locale(&locale, user_dir)
            },
            extension_terminal_input_subscriptions: Vec::new(),
            extension_widgets_above: IndexMap::new(),
            extension_widgets_below: IndexMap::new(),
            custom_footer: None,
            built_in_header: None,
            custom_header: None,
            options,
            on_right_click_paste,
            auto_trust_on_reload_cwd,
            theme_controller: Some(theme_controller),
            pending_logo_clicks: Rc::new(RefCell::new(Vec::new())),
            logo_drain_scheduled: Rc::new(Cell::new(false)),
            branch_change_unsubscribe: None,
        }));
        *weak_slot.borrow_mut() = Some(Rc::downgrade(&inner));

        let mode = InteractiveMode { inner };

        // (3) runtimeHost 回调：beforeSessionInvalidate / rebindSession。
        {
            let weak = Rc::downgrade(&mode.inner);
            mode.inner
                .borrow()
                .runtime_host
                .set_before_session_invalidate(Some(Arc::new(move || {
                    let upgraded = weak.upgrade();
                    if let Some(inner) = upgraded {
                        InteractiveMode { inner }.reset_extension_ui();
                    }
                })));
        }
        {
            let weak = Rc::downgrade(&mode.inner);
            let rebind_settings = Arc::clone(&settings_slot);
            let rebind: RebindSessionFn = Arc::new(move |session: AgentSession| {
                // 会话替换后 TS 的 `this.settingsManager` 指向新会话的 manager；mermaid 的
                // getMode 是 Send+Sync 闭包，不能捕获运行时句柄，因此在这里同步共享槽
                // （在 `applyFromSettings` 之前更新，主题读取也跟随新会话）。
                *lock_settings(&rebind_settings) = session.settings_manager();
                let weak = weak.clone();
                let future: LocalBoxFuture<'static, ()> = Box::pin(async move {
                    let upgraded = weak.upgrade();
                    if let Some(inner) = upgraded {
                        let mut mode = InteractiveMode { inner };
                        mode.rebind_current_session(true).await;
                        mode.apply_theme_from_settings();
                    }
                });
                future
            });
            mode.inner
                .borrow()
                .runtime_host
                .set_rebind_session(Some(rebind));
        }

        mode
    }

    /// `Weak` 句柄（回调/派生任务用）。
    pub(crate) fn downgrade(&self) -> Weak<RefCell<InteractiveModeInner>> {
        Rc::downgrade(&self.inner)
    }

    /// `get session()`。
    pub fn session(&self) -> AgentSession {
        self.inner.borrow().runtime_host.session()
    }

    /// `get sessionManager()`。
    pub fn session_manager(&self) -> Arc<dyn ReadonlySessionManager> {
        self.session().session_manager()
    }

    /// `get settingsManager()`。
    pub fn settings_manager(&self) -> SettingsManager {
        self.session().settings_manager()
    }

    /// 当前已初始化。
    pub fn is_initialized(&self) -> bool {
        self.inner.borrow().is_initialized
    }

    /// 应用主题设置（TS `themeController.applyFromSettings()`；同步部分立即生效）。
    pub(crate) fn apply_theme_from_settings(&mut self) {
        let controller = self.inner.borrow_mut().theme_controller.take();
        let Some(mut controller) = controller else {
            return;
        };
        let renderer = Rc::clone(&self.inner.borrow().renderer);
        {
            let mut host = RendererThemeHost { renderer };
            let future = controller.apply_from_settings(&mut host);
            // 只执行同步部分（应用主题并**发起**终端颜色查询）；查询结果的驱动留给
            // init() 里的 wait_for_terminal_colors。kiana-theme 的
            // `apply_from_settings` 文档保证：丢弃返回的 future 不会丢结果，未就绪的查询
            // 仍留在控制器队列里。
            drop(future);
        }
        self.inner.borrow_mut().theme_controller = Some(controller);
    }

    /// `await themeController.waitForTerminalColors()`。
    async fn wait_for_terminal_colors(&mut self) {
        let controller = self.inner.borrow_mut().theme_controller.take();
        let Some(mut controller) = controller else {
            return;
        };
        let renderer = Rc::clone(&self.inner.borrow().renderer);
        {
            let mut host = RendererThemeHost { renderer };
            controller.wait_for_terminal_colors(&mut host).await;
        }
        self.inner.borrow_mut().theme_controller = Some(controller);
    }

    /// `themeController.disableAutoSync()`。
    pub(crate) fn disable_theme_auto_sync(&mut self) {
        let controller = self.inner.borrow_mut().theme_controller.take();
        let Some(mut controller) = controller else {
            return;
        };
        let renderer = Rc::clone(&self.inner.borrow().renderer);
        {
            let mut host = RendererThemeHost { renderer };
            controller.disable_auto_sync(&mut host);
        }
        self.inner.borrow_mut().theme_controller = Some(controller);
    }

    /// `themeController.rebindTui()`（`switchTuiMode` 之后调用）。
    pub(crate) fn rebind_theme_tui(&mut self) {
        let controller = self.inner.borrow_mut().theme_controller.take();
        let Some(mut controller) = controller else {
            return;
        };
        let renderer = Rc::clone(&self.inner.borrow().renderer);
        {
            let mut host = RendererThemeHost { renderer };
            controller.rebind_tui(&mut host);
        }
        self.inner.borrow_mut().theme_controller = Some(controller);
    }
}

// ---------------------------------------------------------------------------
// 启动 header 文案（TS init() 的 header 构造；逐字，见 spec §2.50.3）
// ---------------------------------------------------------------------------

/// 展开态指令（19 行）。
fn expanded_instructions() -> String {
    [
        key_hint("app.interrupt", "to interrupt"),
        key_hint("app.clear", "to clear"),
        raw_key_hint(&format!("{} twice", key_text("app.clear")), "to exit"),
        key_hint("app.exit", "to exit (empty)"),
        key_hint("app.suspend", "to suspend"),
        key_hint("tui.editor.deleteToLineEnd", "to delete to end"),
        key_hint("app.thinking.cycle", "to cycle thinking level"),
        raw_key_hint(
            &format!(
                "{}/{}",
                key_text("app.model.cycleForward"),
                key_text("app.model.cycleBackward")
            ),
            "to cycle models",
        ),
        key_hint("app.model.select", "to select model"),
        key_hint("app.tools.expand", "to expand tools"),
        key_hint("app.thinking.toggle", "to expand thinking"),
        key_hint("app.editor.external", "for external editor"),
        raw_key_hint("/", "for commands"),
        raw_key_hint("!", "to run bash"),
        raw_key_hint("!!", "to run bash (no context)"),
        key_hint("app.message.followUp", "to queue follow-up"),
        key_hint("app.message.dequeue", "to edit all queued messages"),
        key_hint(
            "app.clipboard.pasteImage",
            "to paste files on macOS, images, or text",
        ),
        raw_key_hint("drop files", "to attach"),
    ]
    .join("\n")
}

/// 折叠态指令（5 条，以 `theme.fg("muted", " · ")` 连接）。
fn compact_instructions() -> String {
    [
        key_hint("app.interrupt", "interrupt"),
        raw_key_hint(
            &format!("{}/{}", key_text("app.clear"), key_text("app.exit")),
            "clear/exit",
        ),
        raw_key_hint("/", "commands"),
        raw_key_hint("!", "bash"),
        key_hint("app.tools.expand", "more"),
    ]
    .join(&fg(ThemeColor::Muted, " · "))
}

/// `compactOnboarding()`。
fn compact_onboarding(show_details: bool) -> String {
    let suffix = if show_details {
        " and loaded resources"
    } else {
        ""
    };
    fg(
        ThemeColor::Dim,
        &format!(
            "Press {} to show full startup help{suffix}.",
            key_text("app.tools.expand")
        ),
    )
}

/// `onboarding()`。
fn onboarding() -> String {
    fg(
        ThemeColor::Dim,
        "Kiana can explain its own features and look up its docs. Ask it how to use or extend Kiana.",
    )
}

/// `withLogo(hints)`：有 logo 时两行（第一行带版本），否则 `kianaWordmark() vX` + hints。
fn header_with_logo(hints: &str, version: &str, show_logo: bool) -> String {
    let version_text = fg(ThemeColor::Dim, &format!("v{version}"));
    if !show_logo {
        return format!("{} {version_text}\n{hints}", kiana_wordmark());
    }
    let [top, bottom] = kiana_logo_lines();
    format!("{top} {version_text}\n{bottom} {hints}")
}

// ---------------------------------------------------------------------------
// 自动补全（TS 730–855 行）
// ---------------------------------------------------------------------------

/// 登录 provider 的补全项（TS `LoginProviderCompletionOption`）。
#[derive(Debug, Clone)]
pub(crate) struct LoginProviderCompletionOption {
    /// provider id。
    pub id: String,
    /// 展示名。
    pub name: String,
    /// 该 provider 支持的认证类型（按 `AUTH_TYPE_ORDER` 排序）。
    pub auth_types: Vec<kiana_ai::auth::types::AuthType>,
    /// OAuth 是否由订阅支撑。
    pub subscription: Option<bool>,
}

/// `createFuzzyAutocompleteItems(items, prefix, getSearchText, toAutocompleteItem)`。
///
/// 用 `kiana_tui::fuzzy_filter` 过滤后映射为补全项；过滤结果为空时返回 `None`
/// （TS 的 `undefined`，调用方据此不显示补全列表），而不是空数组。
pub(crate) fn create_fuzzy_autocomplete_items<T>(
    items: Vec<T>,
    prefix: &str,
    get_search_text: impl Fn(&T) -> String,
    to_autocomplete_item: impl Fn(&T) -> AutocompleteItem,
) -> Option<Vec<AutocompleteItem>> {
    let filtered = kiana_tui::fuzzy_filter(items, prefix, get_search_text);
    if filtered.is_empty() {
        return None;
    }
    Some(filtered.iter().map(to_autocomplete_item).collect())
}

/// `prefixAutocompleteDescription` 的纯逻辑（不含 sourceInfo → tag 的解析）。
///
/// `tag = None` 表示 built-in 来源（TS 返回原 `description`）；否则按 JS 真值拼接：
/// 空描述也返回 `[tag]`。
pub(crate) fn prefix_autocomplete_description_with(
    tag: Option<&str>,
    description: Option<&str>,
) -> Option<String> {
    match tag {
        None => description.map(str::to_string),
        Some(tag) => Some(match description {
            Some(description) if !description.is_empty() => {
                format!("[{tag}] {description}")
            }
            _ => format!("[{tag}]"),
        }),
    }
}

/// `getLoginProviderCompletionOptions(providerOptions)`。
pub(crate) fn get_login_provider_completion_options(
    provider_options: &[AuthSelectorProvider],
) -> Vec<LoginProviderCompletionOption> {
    let mut by_id: IndexMap<String, LoginProviderCompletionOption> = IndexMap::new();
    for provider in provider_options {
        if let Some(existing) = by_id.get_mut(&provider.id) {
            if !existing.auth_types.contains(&provider.auth_type) {
                existing.auth_types.push(provider.auth_type);
                existing
                    .auth_types
                    .sort_by_key(|auth_type| auth_type_order(*auth_type));
            }
            continue;
        }
        by_id.insert(
            provider.id.clone(),
            LoginProviderCompletionOption {
                id: provider.id.clone(),
                name: provider.name.clone(),
                auth_types: vec![provider.auth_type],
                subscription: provider.subscription,
            },
        );
    }
    let mut values: Vec<LoginProviderCompletionOption> = by_id.into_values().collect();
    // TS：`sort((a, b) => a.name.localeCompare(b.name))`——locale 感知（大小写不敏感的主权重），
    // 不是码点序。
    values.sort_by(|a, b| kiana_ai::model_catalog::locale_compare(&a.name, &b.name));
    values
}

/// `getLoginProviderSearchText(provider)`。
pub(crate) fn get_login_provider_search_text(provider: &LoginProviderCompletionOption) -> String {
    let auth_types = provider
        .auth_types
        .iter()
        .map(|auth_type| {
            format!(
                "{} {}",
                auth_type_literal(*auth_type),
                crate::components::oauth_selector::format_auth_selector_provider_type(
                    *auth_type,
                    provider.subscription
                )
            )
        })
        .collect::<Vec<_>>()
        .join(" ");
    format!("{} {} {}", provider.id, provider.name, auth_types)
}

/// `formatLoginProviderCompletionDescription(provider)`。
pub(crate) fn format_login_provider_completion_description(
    provider: &LoginProviderCompletionOption,
) -> String {
    let auth_types = provider
        .auth_types
        .iter()
        .map(|auth_type| {
            crate::components::oauth_selector::format_auth_selector_provider_type(
                *auth_type,
                provider.subscription,
            )
        })
        .collect::<Vec<_>>()
        .join("/");
    if provider.name == provider.id {
        auth_types
    } else {
        format!("{} · {}", provider.name, auth_types)
    }
}

fn auth_type_literal(auth_type: kiana_ai::auth::types::AuthType) -> &'static str {
    match auth_type {
        kiana_ai::auth::types::AuthType::OAuth => "oauth",
        kiana_ai::auth::types::AuthType::ApiKey => "api_key",
    }
}

/// `ThinkingLevel` 的字面量。
pub(crate) fn thinking_level_literal(level: ModelThinkingLevel) -> &'static str {
    match level {
        ModelThinkingLevel::Off => "off",
        ModelThinkingLevel::Minimal => "minimal",
        ModelThinkingLevel::Low => "low",
        ModelThinkingLevel::Medium => "medium",
        ModelThinkingLevel::High => "high",
        ModelThinkingLevel::Xhigh => "xhigh",
        ModelThinkingLevel::Max => "max",
    }
}

/// 把扩展命令的异步补全适配成编辑器要求的 `Send` future。
///
/// 扩展的 `LocalBoxFuture` 不是 `Send`；这里把它转投到 LocalSet 上执行，再用
/// `Send` 的 oneshot 把结果送回。编辑器只在 UI 线程（LocalSet）里轮询补全。
fn adapt_extension_argument_completions(
    function: kiana_ext_api::types::ArgumentCompletionsFn,
) -> Box<dyn for<'a> Fn(&'a str) -> ArgumentCompletionsFuture<'a> + Send + Sync> {
    Box::new(move |prefix: &str| {
        let function = Arc::clone(&function);
        let prefix = prefix.to_string();
        Box::pin(async move {
            let (sender, receiver) = tokio::sync::oneshot::channel();
            tokio::task::spawn_local(async move {
                let items = function(prefix).await;
                let _ = sender.send(items);
            });
            receiver.await.ok().flatten()
        })
    })
}

/// 给 provider 覆写 `triggerCharacters`（TS 直接给对象属性赋值；Rust 的 trait 只有
/// getter，因此用一层包装）。
struct TriggerCharactersProvider {
    inner: Box<dyn AutocompleteProvider>,
    triggers: Vec<char>,
}

impl AutocompleteProvider for TriggerCharactersProvider {
    fn trigger_characters(&self) -> Option<Vec<char>> {
        Some(self.triggers.clone())
    }

    fn get_suggestions<'a>(
        &'a mut self,
        lines: &'a [String],
        cursor_line: usize,
        cursor_col: usize,
        options: &'a kiana_tui::autocomplete::GetSuggestionsOptions,
    ) -> Pin<
        Box<
            dyn Future<Output = Option<kiana_tui::autocomplete::AutocompleteSuggestions>>
                + Send
                + 'a,
        >,
    > {
        self.inner
            .get_suggestions(lines, cursor_line, cursor_col, options)
    }

    fn apply_completion(
        &self,
        lines: &[String],
        cursor_line: usize,
        cursor_col: usize,
        item: &AutocompleteItem,
        prefix: &str,
    ) -> kiana_tui::autocomplete::ApplyCompletionResult {
        self.inner
            .apply_completion(lines, cursor_line, cursor_col, item, prefix)
    }

    fn should_trigger_file_completion(
        &self,
        lines: &[String],
        cursor_line: usize,
        cursor_col: usize,
    ) -> bool {
        self.inner
            .should_trigger_file_completion(lines, cursor_line, cursor_col)
    }
}

impl InteractiveMode {
    /// `getAutocompleteSourceTag(sourceInfo)`。
    ///
    /// 生成补全描述前缀的标签：来源是 `builtin` 时返回 `None`（调用方不再加前缀）；
    /// `auto` / `local` / `cli` 只返回作用域字母（`u` 用户、`p` 项目、`t` 临时）；
    /// `npm:` 返回 `<scope>:<原 source>`；git URL 返回 `<scope>:git:<host>/<path>[@ref]`；
    /// 其他来源只返回作用域字母。
    pub(crate) fn get_autocomplete_source_tag(
        &self,
        source_info: Option<&SourceInfo>,
    ) -> Option<String> {
        let source_info = source_info?;
        if source_info.source == "builtin" {
            return None;
        }
        let scope_prefix = match source_info.scope {
            SourceScope::User => "u",
            SourceScope::Project => "p",
            SourceScope::Temporary => "t",
        };
        let source = source_info.source.trim();
        if source == "auto" || source == "local" || source == "cli" {
            return Some(scope_prefix.to_string());
        }
        if let Some(rest) = source.strip_prefix("npm:") {
            let _ = rest;
            return Some(format!("{scope_prefix}:{source}"));
        }
        if let Some(git_source) = kiana_core::git::parse_git_url(source) {
            let r#ref = git_source
                .r#ref
                .map(|value| format!("@{value}"))
                .unwrap_or_default();
            return Some(format!(
                "{scope_prefix}:git:{}/{}{ref}",
                git_source.host, git_source.path
            ));
        }
        Some(scope_prefix.to_string())
    }

    /// `prefixAutocompleteDescription(description, sourceInfo)`。
    ///
    /// TS 用真值判断 `description ? \`[tag] ${description}\` : \`[tag]\``：`undefined` 与
    /// **空串**都落到 `[tag]`（没有尾随空格）。
    pub(crate) fn prefix_autocomplete_description(
        &self,
        description: Option<&str>,
        source_info: Option<&SourceInfo>,
    ) -> Option<String> {
        prefix_autocomplete_description_with(
            self.get_autocomplete_source_tag(source_info).as_deref(),
            description,
        )
    }

    /// `getBuiltInCommandConflictDiagnostics(extensionRunner)`。
    ///
    /// 把与内置斜杠命令重名的扩展命令转成 `Warning` 级资源诊断：扩展命令没有别名时
    /// 提示它会从补全中跳过，有别名时提示可用别名调用。调用方（资源列表）把结果并入
    /// 其他资源诊断一起展示。
    pub(crate) fn get_built_in_command_conflict_diagnostics(
        &self,
        extension_runner: &kiana_extensions::ExtensionRunner,
    ) -> Vec<kiana_core::diagnostics::ResourceDiagnostic> {
        let builtin_names: IndexSet<String> = kiana_core::slash_commands::BUILTIN_SLASH_COMMANDS
            .iter()
            .map(|command| command.name.clone())
            .chain(
                kiana_session::panel_commands::PANEL_COMMANDS
                    .iter()
                    .map(|command| command.name.to_string()),
            )
            .collect();
        extension_runner
            .get_registered_commands()
            .into_iter()
            .filter(|command| builtin_names.contains(&command.name))
            .map(|command| {
                let message = if command.invocation_name == command.name {
                    format!(
                        "Extension command '/{}' conflicts with built-in interactive command. Skipping in autocomplete.",
                        command.name
                    )
                } else {
                    format!(
                        "Extension command '/{}' conflicts with built-in interactive command. Available as '/{}'.",
                        command.name, command.invocation_name
                    )
                };
                kiana_core::diagnostics::ResourceDiagnostic {
                    kind: kiana_core::diagnostics::DiagnosticKind::Warning,
                    message,
                    path: Some(command.source_info.path.clone()),
                    collision: None,
                }
            })
            .collect()
    }

    /// `createBaseAutocompleteProvider()`。
    ///
    /// 依次组装内置斜杠命令、prompt 模板、扩展命令与技能命令，并为 `/model`、
    /// `/thinking`、`/login` 挂参数补全。扩展命令与内置命令同名时会被过滤掉（冲突诊断由
    /// [`Self::get_built_in_command_conflict_diagnostics`] 生成）；技能命令只在
    /// `get_enable_skill_commands()` 为真时生成，同时重建 `skill_commands` 映射
    /// （名称 → 文件路径）。
    ///
    /// `fd_path` 与 cwd 在这一刻被快照进 `CombinedAutocompleteProvider`。
    fn create_base_autocomplete_provider(&mut self) -> Box<dyn AutocompleteProvider> {
        let session = self.session();

        let mut slash_commands: Vec<SlashCommand> =
            kiana_core::slash_commands::BUILTIN_SLASH_COMMANDS
                .iter()
                .map(|command| {
                    let mut slash = SlashCommand::new(command.name.clone())
                        .with_description(command.description.clone());
                    if let Some(hint) = &command.argument_hint {
                        slash = slash.with_argument_hint(hint.clone());
                    }
                    slash
                })
                .collect();

        // 面板命令（`/permissions`、`/tasks`）：与内置命令共用同一份补全列表，
        // 但不进入 `BUILTIN_SLASH_COMMANDS`（TS 冻结表仍为 24 项）。`/mcp` 不在此表，
        // 由扩展注册。
        slash_commands.extend(kiana_session::panel_commands::PANEL_COMMANDS.iter().map(
            |command| {
                let mut slash = SlashCommand::new(command.name.to_string())
                    .with_description(command.description.to_string());
                if let Some(hint) = command.argument_hint {
                    slash = slash.with_argument_hint(hint.to_string());
                }
                slash
            },
        ));

        // `/model <prefix>`
        if let Some(model_command) = slash_commands.iter_mut().find(|c| c.name == "model") {
            let completion_session = session.clone();
            model_command.get_argument_completions = Some(Box::new(move |prefix: &str| {
                let models: Vec<Model> = {
                    let scoped = completion_session.scoped_models();
                    if !scoped.is_empty() {
                        scoped.into_iter().map(|scoped| scoped.model).collect()
                    } else {
                        completion_session.model_runtime().get_available_snapshot()
                    }
                };
                let items: Vec<Model> = models;
                if items.is_empty() {
                    return Box::pin(std::future::ready(None)) as ArgumentCompletionsFuture<'_>;
                }
                let completions = create_fuzzy_autocomplete_items(
                    items,
                    prefix,
                    |model| get_model_search_text(&model.id, &model.provider, Some(&model.name)),
                    |model| AutocompleteItem {
                        value: format!("{}/{}", model.provider, model.id),
                        label: model.id.clone(),
                        description: Some(model.provider.clone()),
                    },
                );
                Box::pin(std::future::ready(completions))
            }));
        }

        // `/thinking <level>`
        if let Some(thinking_command) = slash_commands.iter_mut().find(|c| c.name == "thinking") {
            let completion_session = session.clone();
            thinking_command.get_argument_completions = Some(Box::new(move |prefix: &str| {
                let levels: Vec<&'static str> = completion_session
                    .get_available_thinking_levels()
                    .into_iter()
                    .map(thinking_level_literal)
                    .collect();
                let completions = create_fuzzy_autocomplete_items(
                    levels,
                    prefix,
                    |level| level.to_string(),
                    |level| AutocompleteItem {
                        value: level.to_string(),
                        label: level.to_string(),
                        description: None,
                    },
                );
                Box::pin(std::future::ready(completions))
            }));
        }

        // `/login <provider>`
        if let Some(login_command) = slash_commands.iter_mut().find(|c| c.name == "login") {
            let model_runtime = session.model_runtime().clone();
            login_command.get_argument_completions = Some(Box::new(move |prefix: &str| {
                let providers = get_login_provider_completion_options(
                    &commands::auth::login_provider_options(&model_runtime, None),
                );
                let completions = create_fuzzy_autocomplete_items(
                    providers,
                    prefix,
                    get_login_provider_search_text,
                    |provider| AutocompleteItem {
                        value: provider.id.clone(),
                        label: provider.id.clone(),
                        description: Some(format_login_provider_completion_description(provider)),
                    },
                );
                Box::pin(std::future::ready(completions))
            }));
        }

        // Prompt 模板
        let template_commands: Vec<SlashCommand> = session
            .prompt_templates()
            .into_iter()
            .map(|template| {
                let mut slash = SlashCommand::new(template.name.clone()).with_description(
                    self.prefix_autocomplete_description(
                        Some(&template.description),
                        Some(&template.source_info),
                    )
                    .unwrap_or_default(),
                );
                if let Some(hint) = &template.argument_hint {
                    slash = slash.with_argument_hint(hint.clone());
                }
                slash
            })
            .collect();

        // 扩展命令
        let builtin_command_names: IndexSet<String> = slash_commands
            .iter()
            .map(|command| command.name.clone())
            .collect();
        let extension_commands: Vec<SlashCommand> = session
            .extension_runner()
            .get_registered_commands()
            .into_iter()
            .filter(|command| !builtin_command_names.contains(&command.name))
            .map(|command| {
                let mut slash = SlashCommand::new(command.invocation_name.clone());
                // TS 无条件调用 `prefixAutocompleteDescription(cmd.description, cmd.sourceInfo)`：
                // 没有描述时仍会得到 `[tag]`，只有 sourceInfo 为 builtin 时才是 undefined。
                if let Some(description) = self.prefix_autocomplete_description(
                    command.description.as_deref(),
                    Some(&command.source_info),
                ) {
                    slash = slash.with_description(description);
                }
                if let Some(completions) = command.get_argument_completions.clone() {
                    slash.get_argument_completions =
                        Some(adapt_extension_argument_completions(completions));
                }
                slash
            })
            .collect();

        // 技能命令
        self.inner.borrow_mut().skill_commands.clear();
        let mut skill_command_list: Vec<SlashCommand> = Vec::new();
        if self.settings_manager().get_enable_skill_commands() {
            let mut skill_commands = IndexMap::new();
            for skill in session.resource_loader().get_skills().skills {
                let command_name = format!("skill:{}", skill.name);
                skill_commands.insert(command_name.clone(), skill.file_path.clone());
                let mut slash = SlashCommand::new(command_name);
                if let Some(description) = self.prefix_autocomplete_description(
                    Some(&skill.description),
                    Some(&skill.source_info),
                ) {
                    slash = slash.with_description(description);
                }
                skill_command_list.push(slash);
            }
            self.inner.borrow_mut().skill_commands = skill_commands;
        }

        let mut commands: Vec<CommandEntry> = Vec::new();
        commands.extend(slash_commands.into_iter().map(CommandEntry::from));
        commands.extend(template_commands.into_iter().map(CommandEntry::from));
        commands.extend(extension_commands.into_iter().map(CommandEntry::from));
        commands.extend(skill_command_list.into_iter().map(CommandEntry::from));
        let cwd = session.session_manager().get_cwd().to_string();
        let fd_path = self.inner.borrow().fd_path.clone();
        Box::new(CombinedAutocompleteProvider::new(commands, cwd, fd_path))
    }

    /// 构建一份 provider（`createBaseAutocompleteProvider` + `autocompleteProviderWrappers`）。
    ///
    /// `triggerCharacters` 的并集（`[...new Set(...)]`，保持首次出现顺序）用
    /// [`TriggerCharactersProvider`] 包装表达。
    pub(crate) fn build_autocomplete_provider(&mut self) -> Box<dyn AutocompleteProvider> {
        let mut provider: Box<dyn AutocompleteProvider> = self.create_base_autocomplete_provider();
        let wrappers = self.inner.borrow().autocomplete_provider_wrappers.clone();
        let mut trigger_characters: Vec<char> = Vec::new();
        for wrap_provider in wrappers {
            provider = wrap_provider(provider);
            if let Some(characters) = provider.trigger_characters() {
                trigger_characters.extend(characters);
            }
        }
        if !trigger_characters.is_empty() {
            let mut unique: Vec<char> = Vec::new();
            for character in trigger_characters {
                if !unique.contains(&character) {
                    unique.push(character);
                }
            }
            provider = Box::new(TriggerCharactersProvider {
                inner: provider,
                triggers: unique,
            });
        }
        provider
    }

    /// `setupAutocompleteProvider()`。
    ///
    /// TS：`this.autocompleteProvider = provider; this.defaultEditor.setAutocompleteProvider(provider);
    /// if (this.editor !== this.defaultEditor) this.editor.setAutocompleteProvider?.(provider);`
    ///
    /// Rust 的 trait 对象不能共享：给扩展编辑器再构建一份等价实例，并把重建工厂存入
    /// `autocomplete_provider` 供后续替换编辑器使用。
    pub(crate) fn setup_autocomplete_provider(&mut self) {
        let provider = self.build_autocomplete_provider();
        self.default_editor()
            .borrow_mut()
            .set_autocomplete_provider(provider);

        // `this.editor !== this.defaultEditor` → 活动编辑器是扩展提供的。
        let custom_editor = match &self.inner.borrow().editor {
            ActiveEditor::Custom { handle, .. } => Some(Rc::clone(handle)),
            ActiveEditor::Default { .. } => None,
        };
        if let Some(handle) = custom_editor {
            let provider = self.build_autocomplete_provider();
            handle
                .borrow_mut()
                .set_autocomplete_provider(Box::new(provider));
        }

        let weak = self.downgrade();
        self.inner.borrow_mut().autocomplete_provider = Some(Rc::new(move || {
            let inner = weak.upgrade()?;
            let mut mode = InteractiveMode { inner };
            Some(mode.build_autocomplete_provider())
        }));
    }

    /// `setCustomEditorComponent` 用：为新编辑器取一份当前配置的 provider。
    ///
    /// 对应 TS 的 `newEditor.setAutocompleteProvider(this.autocompleteProvider)`；Rust 侧
    /// 用重建工厂产生等价实例（见 [`AutocompleteProviderBuilder`]）。未初始化自动补全时
    /// 返回 `None`（TS 的 `this.autocompleteProvider` 也是 `undefined`）。
    pub(crate) fn autocomplete_provider_for_editor(
        &mut self,
    ) -> Option<Box<dyn AutocompleteProvider>> {
        let builder = self.inner.borrow().autocomplete_provider.clone();
        builder.and_then(|build| build())
    }

    /// `this.defaultEditor` 的共享句柄。
    pub(crate) fn default_editor(&self) -> Rc<RefCell<CustomEditor>> {
        Rc::clone(&self.inner.borrow().default_editor)
    }

    /// `showStartupNoticesIfNeeded()`。
    ///
    /// 每个 `InteractiveMode` 实例只执行一次：先置 `startup_notices_shown`，即使没有
    /// changelog 也不会再执行。
    ///
    /// `get_collapse_changelog()` 为真时只显示一行“Updated to vX”，否则把 changelog 作为
    /// markdown 区块插入 chat 容器（前后各一条 [`DynamicBorder`]）。
    pub(crate) fn show_startup_notices_if_needed(&mut self) {
        if self.inner.borrow().startup_notices_shown {
            return;
        }
        self.inner.borrow_mut().startup_notices_shown = true;
        let changelog = self.inner.borrow().changelog_markdown.clone();
        let Some(changelog) = changelog.filter(|changelog| !changelog.is_empty()) else {
            return;
        };
        let collapse_changelog = self.settings_manager().get_collapse_changelog();
        let chat_container = Rc::clone(&self.inner.borrow().chat_container);
        {
            let mut chat = chat_container.borrow_mut();
            if !chat.children.is_empty() {
                chat.add_child(Box::new(Spacer::new()));
            }
            chat.add_child(Box::new(DynamicBorder::default()));
            if collapse_changelog {
                let latest_version = changelog_version_regex(&changelog)
                    .map(str::to_string)
                    .unwrap_or_else(|| self.inner.borrow().version.clone());
                let condensed = format!(
                    "Updated to v{latest_version}. Use {} to view full changelog.",
                    bold("/changelog")
                );
                chat.add_child(Box::new(Text::with_padding(condensed, 1, 0)));
            } else {
                chat.add_child(Box::new(ThemedText::with_padding(
                    Box::new(|| bold(&fg(ThemeColor::Accent, "What's New"))),
                    1,
                    0,
                )));
                chat.add_child(Box::new(Spacer::new()));
                chat.add_child(Box::new(kiana_tui::components::markdown::Markdown::new(
                    changelog.trim(),
                    1,
                    0,
                    self.get_markdown_theme_with_settings(),
                )));
                chat.add_child(Box::new(Spacer::new()));
            }
            chat.add_child(Box::new(DynamicBorder::default()));
        }
    }

    /// `getMarkdownThemeWithSettings()`。
    pub(crate) fn get_markdown_theme_with_settings(&self) -> MarkdownTheme {
        let mut theme = get_markdown_theme();
        theme.code_block_indent = Some(self.settings_manager().get_code_block_indent());
        theme
    }

    // -----------------------------------------------------------------------
    // TUI 挂载 / 停止 / 切换（TS 871–944 行）
    // -----------------------------------------------------------------------

    /// `mountInteractiveTui(tui, components)`。
    ///
    /// 按屏幕顺序挂七个顶层容器：document、pending messages、status、widgets above、
    /// editor、widgets below、footer。全屏（viewport）渲染器还要求
    /// `fullscreen_layout_root` 已由 `init()` 构建，否则 `expect` panic。
    pub(crate) fn mount_interactive_tui(&mut self) {
        let renderer = Rc::clone(&self.inner.borrow().renderer);
        let components: Vec<SharedComponent> = {
            let inner = self.inner.borrow();
            vec![
                shared(&inner.document_container),
                shared(&inner.pending_messages_container),
                shared(&inner.status_container),
                shared(&inner.widget_container_above),
                shared(&inner.editor_container),
                shared(&inner.widget_container_below),
                shared(&inner.footer_container),
            ]
        };
        let root = self.inner.borrow().fullscreen_layout_root.clone();
        let mut tui = renderer.borrow_mut();
        for component in components {
            mount_interactive_child(&mut tui, Box::new(SharedComponentProxy::new(component)));
        }
        if tui.is_viewport_tui() {
            let root = root.expect("Fullscreen layout is not initialized");
            tui.set_layout_root(Some(Box::new(SharedComponentProxy::new(root))));
        }
    }

    /// `stopInteractiveTui(fullscreenExitOutput)`。
    ///
    /// 全屏 + [`FullscreenExitOutput::Transcript`]：先逐个 `hide_overlay`（每次重新检查，
    /// 直到没有 overlay 条目），再切回主屏（不启动渲染器），最后 `render_now` 把最终画面
    /// 刷到主屏，让退出后终端里留下会话记录。其余情况不切模式，以
    /// `preserve_screen = 是否仍处于全屏` 调用 `ui.stop()`：全屏时按 `preserve_screen`
    /// 语义把渲染输出留给终端，主屏时把光标移到已渲染内容之后。
    pub(crate) fn stop_interactive_tui(&mut self, fullscreen_exit_output: FullscreenExitOutput) {
        let renderer = Rc::clone(&self.inner.borrow().renderer);
        let is_fullscreen = renderer.borrow().mode() == TuiMode::Fullscreen;
        if is_fullscreen && fullscreen_exit_output == FullscreenExitOutput::Transcript {
            loop {
                let has_entries = renderer.borrow().has_overlay_entries();
                if !has_entries {
                    break;
                }
                renderer.borrow_mut().hide_overlay();
            }
            self.switch_tui_mode(TuiMode::Regular, false, false);
            renderer.borrow_mut().render_now(false);
        }
        let preserve_screen = renderer.borrow().mode() == TuiMode::Fullscreen;
        self.inner
            .borrow_mut()
            .ui
            .stop(TuiStopOptions { preserve_screen });
    }

    /// `switchTuiMode(mode, restoreProgress = true, startRenderer = true)`。
    ///
    /// 目标模式与当前相同时直接返回 `true`；旧渲染器仍有 overlay 条目时返回 `false` 且
    /// 不做任何改动（调用方据此提示需要先关闭 overlay）。
    ///
    /// 切换过程：先解析旧渲染器的焦点组件地址（必须在 `take_children()` 之前，regular
    /// 渲染器的焦点是按路径保存的），取出子项/终端/光标设置/`on_debug`/主屏渲染状态，
    /// 停掉并清空旧渲染器；然后用同一个终端建新渲染器，重新挂载同一批组件并恢复焦点。
    /// `start_renderer` 为真时还会启动新渲染器、重绑主题与扩展终端输入监听，并按
    /// `restore_progress` 决定是否恢复终端进度指示。目标渲染器是全屏而
    /// `fullscreen_layout_root` 为 `None` 时 `expect` panic。
    pub(crate) fn switch_tui_mode(
        &mut self,
        mode: TuiMode,
        restore_progress: bool,
        start_renderer: bool,
    ) -> bool {
        let renderer = Rc::clone(&self.inner.borrow().renderer);
        if mode == renderer.borrow().mode() {
            return true;
        }
        if renderer.borrow().has_overlay_entries() {
            return false;
        }

        let settings_manager = self.settings_manager();
        let (
            components,
            focus_address,
            terminal,
            show_hardware_cursor,
            clear_on_shrink,
            on_debug,
            main_state,
        ) = {
            let mut previous = renderer.borrow_mut();
            // TS：`const focus = previousUi.getFocusedComponent()`——保存的是**组件引用**，
            // 而不是渲染器里的位置。Rust 的 [`FocusTarget`] 是挂载树里的位置，跨渲染器
            // 没有意义（全屏的 `LayoutRoot { path }` 在主屏里解析不到，输入会被静默丢弃）。
            // 因此先解析出组件地址，挂载到新渲染器后按地址重新定位焦点。
            //
            // 必须在 `take_children()` 之前解析：regular 渲染器的焦点是 `Path`，子项被
            // 取出后就无法再沿路径找到组件。
            let focus_address = previous
                .focused_component_identity()
                .map(|identity| identity.address);
            let components = previous.take_children();
            let terminal = previous.terminal();
            let show_hardware_cursor = previous.get_show_hardware_cursor();
            let clear_on_shrink = previous.get_clear_on_shrink();
            let on_debug = previous.take_on_debug();
            let main_state = previous.capture_render_state();
            previous.stop(TuiStopOptions {
                preserve_screen: true,
            });
            previous.set_focus(None);
            previous.clear();
            if previous.is_viewport_tui() {
                previous.set_layout_root(None);
            }
            (
                components,
                focus_address,
                terminal,
                show_hardware_cursor,
                clear_on_shrink,
                on_debug,
                main_state,
            )
        };
        if main_state.is_some() {
            // TS：`if (previousUi instanceof TuiMainScreen) this.mainScreenRenderState = ...`
            self.inner.borrow_mut().main_screen_render_state = main_state;
        }
        // 回到主屏时恢复**进入全屏前**保存的那一帧状态（TS 读字段而不是局部变量：
        // 全屏渲染器的 `captureRenderState()` 是 `undefined`，局部变量在回程方向为空）。
        let saved_main_state = self.inner.borrow().main_screen_render_state.clone();

        let next = create_interactive_tui(InteractiveTuiOptions {
            tui_mode: mode,
            show_hardware_cursor,
            log_directory: get_agent_dir().to_string_lossy().into_owned(),
            terminal: Some(Box::new(SharedTerminal(Rc::clone(&terminal)))),
            on_right_click_paste: Some(Rc::clone(&self.inner.borrow().on_right_click_paste)),
            fullscreen_copy_on_select: Some(settings_manager.get_fullscreen_copy_on_select()),
            fullscreen_wheel_scroll_lines: Some(
                settings_manager.get_fullscreen_wheel_scroll_lines(),
            ),
        });
        let next_value = Rc::try_unwrap(next)
            .ok()
            .expect("new renderer is uniquely owned")
            .into_inner();
        {
            let mut slot = renderer.borrow_mut();
            *slot = next_value;
            slot.set_clear_on_shrink(clear_on_shrink);
            slot.set_on_debug(on_debug);
            // TS：`if (nextUi instanceof TuiMainScreen && this.mainScreenRenderState)`。
            if matches!(*slot, InteractiveRenderer::Regular(_))
                && let Some(state) = saved_main_state
            {
                slot.restore_render_state(state);
            }
            for component in components {
                mount_interactive_child(&mut slot, component);
            }
            if slot.is_viewport_tui() {
                let root = self
                    .inner
                    .borrow()
                    .fullscreen_layout_root
                    .clone()
                    .expect("Fullscreen layout is not initialized");
                slot.set_layout_root(Some(Box::new(SharedComponentProxy::new(root))));
            }
            slot.invalidate();
            // TS：`nextUi.setFocus(focus)`。按组件地址在新挂载树里解析焦点目标；组件
            // 已脱离挂载树时 `focus_target_of_address` 返回 `None`，等价于无法再按引用
            // 聚焦（TS 里该引用仍会收到输入，是 Rust 焦点模型已文档化的差异）。
            let focus = focus_address.and_then(|address| slot.focus_target_of_address(address));
            slot.set_focus(focus);
        }
        self.inner.borrow_mut().options.tui_mode = Some(settings_tui_mode(mode));
        if !start_renderer {
            return true;
        }
        renderer.borrow_mut().start();
        self.rebind_theme_tui();
        self.rebind_extension_terminal_input_listeners();
        if restore_progress
            && settings_manager.get_show_terminal_progress()
            && (self.session().is_streaming() || self.session().is_compacting())
        {
            terminal.borrow_mut().set_progress(true);
        }
        true
    }
}

// ---------------------------------------------------------------------------
// init() / run()（TS 945–1262 行）
// ---------------------------------------------------------------------------

/// 重绘请求（`onThemeChange` / `onBranchChange` 回调跨线程投递到 UI 线程）。
#[derive(Debug, Clone, Copy)]
enum RepaintRequest {
    Theme,
    Branch,
}

/// TS `error instanceof Error ? error.message : "Unknown error occurred"`。
///
/// [`AgentSessionError`] 是 TS `Error` 的建模（`Display` 逐字等于 `error.message`），因此
/// 常规失败直接用 `Display`。空消息是唯一可区分的非 Error 形态：扩展抛出的空字符串经
/// 扩展层 `String(err)` 归一成空串，TS 走非 Error 分支显示回退串；这里保持一致。
fn prompt_error_message(error: &AgentSessionError) -> String {
    let message = error.to_string();
    if message.is_empty() {
        "Unknown error occurred".to_string()
    } else {
        message
    }
}

impl InteractiveMode {
    /// `init()`（spec §2.50.3 的 16 步，顺序敏感）。
    ///
    /// 幂等：`is_initialized` 为真时立即返回。该标志在 `ui.start()` 之后、扩展初始化与
    /// 初始消息渲染之前置 `true`，因此“已初始化”并不表示启动流程已全部完成。
    pub async fn init(&mut self) {
        if self.inner.borrow().is_initialized {
            return;
        }

        // 1. 信号处理器 + changelog
        self.register_signal_handlers();
        let changelog = self.get_changelog_for_display();
        self.inner.borrow_mut().changelog_markdown = changelog;

        // 2. Model scope（`--models` 时打印到 stdout）
        let scoped_models: Vec<ScopedModel> = self.session().scoped_models();
        if !scoped_models.is_empty() && self.should_show_startup_details() {
            let model_list = scoped_models
                .iter()
                .map(|scoped| {
                    let thinking = scoped
                        .thinking_level
                        .map(|level| format!(":{}", thinking_level_literal(level)))
                        .unwrap_or_default();
                    format!("{}{thinking}", scoped.model.id)
                })
                .collect::<Vec<_>>()
                .join(", ");
            let cycle_keys = self
                .inner
                .borrow()
                .keybindings
                .get_keys("app.model.cycleForward");
            let cycle_hint = if cycle_keys.is_empty() {
                String::new()
            } else {
                let joined = cycle_keys
                    .iter()
                    .map(|key| key.as_str())
                    .collect::<Vec<_>>()
                    .join("/");
                fg(
                    ThemeColor::Muted,
                    &format!(
                        " ({} to cycle)",
                        format_key_text(&joined, KeyTextFormatOptions { capitalize: true })
                    ),
                )
            };
            println!(
                "{}",
                fg(
                    ThemeColor::Dim,
                    &format!("Model scope: {model_list}{cycle_hint}")
                )
            );
        }

        // 3. widgets + chat viewport + mount
        self.render_widgets();
        let (
            document_shared,
            pending_shared,
            status_shared,
            widgets_above_shared,
            editor_shared,
            widgets_below_shared,
            footer_shared,
        ) = {
            let inner = self.inner.borrow();
            (
                shared(&inner.document_container),
                shared(&inner.pending_messages_container),
                shared(&inner.status_container),
                shared(&inner.widget_container_above),
                shared(&inner.editor_container),
                shared(&inner.widget_container_below),
                shared(&inner.footer_container),
            )
        };
        let scrollbar = self.settings_manager().get_fullscreen_scrollbar();
        let viewport = create_chat_viewport(ChatViewportOptions {
            document: document_shared,
            pending_messages: pending_shared,
            status: status_shared,
            widgets_above: Some(widgets_above_shared),
            editor: editor_shared,
            widgets_below: Some(widgets_below_shared),
            footer: footer_shared,
            scrollbar: Some(scrollbar),
            scrollbar_track_style: Some(Arc::new(|text: &str| {
                fg(ThemeColor::ScrollbarTrack, text)
            })),
            scrollbar_thumb_style: Some(Arc::new(|text: &str| {
                fg(ThemeColor::ScrollbarThumb, text)
            })),
        });
        let root: SharedComponent = Rc::new(RefCell::new(viewport.root));
        {
            let mut inner = self.inner.borrow_mut();
            inner.transcript_scroll_view = Some(viewport.transcript);
            inner.fullscreen_layout_root = Some(root);
        }
        self.mount_interactive_tui();

        // 4. 启动期编辑器回调（只处理中断、退出与提交反馈）
        {
            let default_editor = Rc::clone(&self.inner.borrow().default_editor);
            let weak = self.downgrade();
            default_editor.borrow_mut().set_action_handler(
                "app.clear",
                Box::new(move || {
                    if let Some(inner) = weak.upgrade() {
                        InteractiveMode { inner }.handle_ctrl_c();
                    }
                }),
            );
            let weak = self.downgrade();
            default_editor
                .borrow_mut()
                .set_on_ctrl_d(Some(Box::new(move || {
                    if let Some(inner) = weak.upgrade() {
                        InteractiveMode { inner }.handle_ctrl_d();
                    }
                })));
            let weak = self.downgrade();
            default_editor
                .borrow_mut()
                .set_on_submit(Some(Box::new(move |text: &str| {
                    if let Some(inner) = weak.upgrade() {
                        InteractiveMode { inner }.handle_startup_submit(text.to_string());
                    }
                })));
        }
        let editor_address = self.inner.borrow().editor.address();
        let focused = self
            .inner
            .borrow_mut()
            .ui
            .set_focus_to_address(editor_address);
        debug_assert!(focused, "editor proxy must be mounted before focus is set");

        // 5. 启动 UI
        self.inner.borrow_mut().ui.start();
        self.inner.borrow_mut().is_initialized = true;
        // 注意：`report()` 内部经 getter 读渲染器/会话名，但这些 getter 不再借
        // `self.inner`（见构造器），因此这里与 TS 一样可以在任意调用栈里上报。
        self.inner.borrow_mut().program_status.report();
        self.ensure_png_transcoder();

        // 6. 主题（header / 启动提示把颜色烘进文本，等待终端配色）
        self.apply_theme_from_settings();
        self.wait_for_terminal_colors().await;

        // 7. Header
        if self.should_show_startup_header() {
            let show_details = self.should_show_startup_details();
            let show_logo = supports_kiana_logo();
            let version = self.inner.borrow().version.clone();
            let expansion_state = self.get_startup_expansion_state();
            let version_collapsed = version.clone();
            let collapsed: Rc<dyn Fn() -> String> = Rc::new(move || {
                format!(
                    "{}\n{}\n\n{}",
                    header_with_logo(&compact_instructions(), &version_collapsed, show_logo),
                    compact_onboarding(show_details),
                    onboarding()
                )
            });
            let version_expanded = version.clone();
            let expanded: Rc<dyn Fn() -> String> = Rc::new(move || {
                format!(
                    "{}\n\n{}",
                    header_with_logo(&expanded_instructions(), &version_expanded, show_logo),
                    onboarding()
                )
            });
            let mut header = BuiltInHeader::new(collapsed, expanded, expansion_state, 1, 0);
            if show_logo {
                let queue = Rc::clone(&self.inner.borrow().pending_logo_clicks);
                let scheduled = Rc::clone(&self.inner.borrow().logo_drain_scheduled);
                let renderer = Rc::clone(&self.inner.borrow().renderer);
                header.on_logo_click = Some(Box::new(move |column, row| {
                    queue.borrow_mut().push((column, row));
                    if scheduled.get() {
                        return;
                    }
                    scheduled.set(true);
                    let queue = Rc::clone(&queue);
                    let scheduled = Rc::clone(&scheduled);
                    let renderer = Rc::clone(&renderer);
                    tokio::task::spawn_local(async move {
                        // 让当前 TUI 事件分发（以及渲染器的可变借用）先返回。
                        tokio::task::yield_now().await;
                        scheduled.set(false);
                        let clicks: Vec<(usize, usize)> = std::mem::take(&mut *queue.borrow_mut());
                        for (column, row) in clicks {
                            crate::components::easter_egg_3d::play_kiana_logo_3d(
                                &mut renderer.borrow_mut(),
                                column,
                                row,
                            );
                        }
                    });
                }));
            }
            let header = Rc::new(RefCell::new(header));
            let header_handle = shared(&header);
            {
                let container = Rc::clone(&self.inner.borrow().header_container);
                let mut container = container.borrow_mut();
                container.add_child(Box::new(Spacer::new()));
                container.add_child(Box::new(SharedComponentProxy::new(header_handle)));
                container.add_child(Box::new(Spacer::new()));
            }
            self.inner.borrow_mut().built_in_header = Some(HeaderComponent::BuiltIn(header));
        } else {
            let container = Rc::clone(&self.inner.borrow().header_container);
            container
                .borrow_mut()
                .add_child(Box::new(Text::with_padding("", 0, 0)));
            self.inner.borrow_mut().built_in_header = Some(HeaderComponent::Empty);
        }
        self.inner.borrow_mut().ui.request_render(false);

        // 8. fd / rg（挂载 TUI 之后安装，避免慢下载看起来像启动卡死）
        {
            let weak = self.downgrade();
            let mut fd_status = move |status: ToolStatus| {
                if let Some(inner) = weak.upgrade() {
                    InteractiveMode { inner }.show_managed_tool_status(status);
                }
            };
            let weak = self.downgrade();
            let mut rg_status = move |status: ToolStatus| {
                if let Some(inner) = weak.upgrade() {
                    InteractiveMode { inner }.show_managed_tool_status(status);
                }
            };
            let (fd_path, _rg_path) = futures::future::join(
                ensure_tool("fd", Some(&mut fd_status)),
                ensure_tool("rg", Some(&mut rg_status)),
            )
            .await;
            self.inner.borrow_mut().fd_path = fd_path;
        }

        // 9. 托管工具安装完成后才启用真正的按键处理
        self.setup_key_handlers();
        self.setup_editor_submit_handler();
        self.inner.borrow_mut().ui.request_render(false);

        // 10. 先初始化扩展（loaded resources 显示在消息之前）
        self.rebind_current_session(false).await;

        // 11. 初始消息在 loaded resources 之后
        self.render_initial_messages();

        // 12. 主题文件监听器
        let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel::<RepaintRequest>();
        {
            let weak = self.downgrade();
            tokio::task::spawn_local(async move {
                while let Some(request) = receiver.recv().await {
                    let Some(inner) = weak.upgrade() else {
                        break;
                    };
                    let mut mode = InteractiveMode { inner };
                    match request {
                        RepaintRequest::Theme => {
                            mode.inner.borrow_mut().ui.invalidate();
                            mode.update_editor_border_color();
                            mode.inner.borrow_mut().ui.request_render(false);
                        }
                        RepaintRequest::Branch => {
                            mode.inner.borrow_mut().ui.request_render(false);
                        }
                    }
                }
            });
        }
        let theme_sender = sender.clone();
        kiana_theme::on_theme_change(Box::new(move || {
            let _ = theme_sender.send(RepaintRequest::Theme);
        }));

        // 13. git 分支监听器
        let branch_sender = sender.clone();
        let footer_data_provider = Arc::clone(&self.inner.borrow().footer_data_provider);
        let branch_unsubscribe = footer_data_provider.on_branch_change(Arc::new(move || {
            let _ = branch_sender.send(RepaintRequest::Branch);
        }));
        self.inner.borrow_mut().branch_change_unsubscribe = Some(branch_unsubscribe);

        // 14. footer 的可用 provider 计数
        self.update_available_provider_count();

        // 15. 先 flush 完整启动状态，再加载剩余的语法高亮
        self.inner.borrow_mut().ui.render_now(false);
        {
            let weak = self.downgrade();
            tokio::task::spawn_local(async move {
                kiana_core::syntax_highlight::load_all_highlight_languages().await;
                let Some(inner) = weak.upgrade() else {
                    return;
                };
                let mode = InteractiveMode { inner };
                let initialized = mode.inner.borrow().is_initialized;
                if initialized {
                    mode.inner.borrow_mut().ui.invalidate();
                    mode.inner.borrow_mut().ui.request_render(false);
                }
            });
        }
    }

    /// `run()`：主入口（无限循环；单元测试不调用）。
    ///
    /// 首行先 `init()`，随后启动 10ms 周期的事件泵与若干后台检查，再进入
    /// `get_user_input()` → `session.prompt()` 的循环。循环没有退出分支：关停由子模块的
    /// `shutdown()` 结束进程（生产路径为 `std::process::exit`）。模型目录刷新与包更新
    /// 检查在 `KIANA_OFFLINE` 下跳过。
    pub async fn run(&mut self) {
        self.init().await;

        // 宿主渲染/输入循环：TS 的 Node 事件循环把它们拆成 `stdin.on("data")`、
        // `setTimeout` 与 `stdout.on("resize")` 回调；Rust 用一个轮询任务复刻，
        // 在 `get_user_input()`（oneshot）与 `session.prompt()` 等待期间持续运行。
        // 任务只在同步块内借用 `inner`，不在 await 点持有借用。
        {
            let weak = self.downgrade();
            tokio::task::spawn_local(async move {
                loop {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                    let Some(inner) = weak.upgrade() else {
                        break;
                    };
                    // 第 12 周渲染核心（实验开关）：150s watchdog。开关关闭时 tick 返回 0，
                    // 不触碰任何 UI 状态。
                    let ticked = {
                        let mut mode = InteractiveMode {
                            inner: std::rc::Rc::clone(&inner),
                        };
                        mode.tick_render_core()
                    };
                    if ticked > 0 {
                        let mut mode = InteractiveMode {
                            inner: std::rc::Rc::clone(&inner),
                        };
                        mode.update_pending_messages_display();
                        mode.request_render();
                    }
                    // 只在同步作用域内借用 `inner` 取句柄；`pump_events` 会分发组件
                    // 输入回调，回调里可能重入借用 `inner`。
                    let mut ui = {
                        let inner = inner.borrow();
                        if inner.is_shutting_down {
                            break;
                        }
                        inner.ui.clone()
                    };
                    ui.pump_events();
                }
            });
        }

        // 模型目录刷新：15 秒后放弃（TS 用 AbortController）。
        if !env_is_truthy("KIANA_OFFLINE") {
            let session = self.session();
            let weak = self.downgrade();
            tokio::task::spawn_local(async move {
                let runtime = session.model_runtime().clone();
                let result = tokio::time::timeout(
                    Duration::from_secs(15),
                    crate::model_catalog_refresh::refresh_model_catalogs(&runtime, None),
                )
                .await;
                if matches!(result, Ok(Ok(_)))
                    && let Some(inner) = weak.upgrade()
                {
                    InteractiveMode { inner }.update_available_provider_count();
                }
            });
        }

        // 版本检查
        {
            let version = self.inner.borrow().version.clone();
            let weak = self.downgrade();
            tokio::task::spawn_local(async move {
                let release = check_for_new_kiana_version(&version).await;
                if let Some(release) = release
                    && let Some(inner) = weak.upgrade()
                {
                    InteractiveMode { inner }.show_new_version_notification(release);
                }
            });
        }

        // 包更新检查
        {
            let weak = self.downgrade();
            tokio::task::spawn_local(async move {
                let Some(inner) = weak.upgrade() else {
                    return;
                };
                let mut mode = InteractiveMode { inner };
                let updates = mode.check_for_package_updates().await;
                if !updates.is_empty() {
                    mode.show_package_update_notification(updates);
                }
                let initialized = mode.inner.borrow().is_initialized;
                if cfg!(windows) && initialized {
                    mode.update_terminal_title();
                }
            });
        }

        // tmux 键盘提示
        {
            let weak = self.downgrade();
            tokio::task::spawn_local(async move {
                let Some(inner) = weak.upgrade() else {
                    return;
                };
                let mode = InteractiveMode { inner };
                if let Some(warning) = mode.check_tmux_keyboard_setup().await {
                    InteractiveMode {
                        inner: Rc::clone(&mode.inner),
                    }
                    .show_warning(&warning);
                }
            });
        }

        // 启动告警
        let (
            migrated_providers,
            startup_diagnostics,
            model_fallback_message,
            initial_message,
            initial_images,
            initial_messages,
        ) = {
            let mut inner = self.inner.borrow_mut();
            let options = &mut inner.options;
            (
                options.migrated_providers.take(),
                options.startup_diagnostics.take(),
                options.model_fallback_message.take(),
                options.initial_message.take(),
                options.initial_images.take(),
                options.initial_messages.take(),
            )
        };

        if let Some(diagnostics) = startup_diagnostics {
            for diagnostic in diagnostics {
                match diagnostic.kind {
                    kiana_config::settings::DiagnosticKind::Error => {
                        self.show_error(&diagnostic.message)
                    }
                    kiana_config::settings::DiagnosticKind::Warning => {
                        self.show_warning(&diagnostic.message)
                    }
                    kiana_config::settings::DiagnosticKind::Info => {
                        self.show_status(&diagnostic.message)
                    }
                }
            }
        }

        if let Some(migrated) = migrated_providers
            && !migrated.is_empty()
        {
            self.show_warning(&format!(
                "Migrated credentials to auth.json: {}",
                migrated.join(", ")
            ));
        }

        let models_json_error = self
            .session()
            .model_runtime()
            .get_error()
            .filter(|error| !error.is_empty());
        if let Some(error) = models_json_error {
            self.show_error(&format!("models.json error: {error}"));
        }

        if let Some(message) = model_fallback_message.filter(|message| !message.is_empty()) {
            self.show_warning(&message);
        }

        let crash = take_unnotified_crash(&crash_log_path(None), None);
        if let Some(crash) = crash {
            let when = format_crash_timestamp(crash.timestamp());
            self.show_warning(&format!(
                "{APP_NAME} crashed on {when} ({}). Run /bug to report it; the crash details are attached automatically.",
                crash.message().unwrap_or("")
            ));
        }

        {
            let weak = self.downgrade();
            tokio::task::spawn_local(async move {
                let Some(inner) = weak.upgrade() else {
                    return;
                };
                let mut mode = InteractiveMode { inner };
                mode.maybe_warn_about_anthropic_subscription_auth().await;
            });
        }

        if let Some(message) = initial_message.filter(|message| !message.is_empty()) {
            let session = self.session();
            let result = session
                .prompt(
                    message,
                    PromptOptions {
                        images: initial_images,
                        ..Default::default()
                    },
                )
                .await;
            if let Err(error) = result {
                self.show_error(&prompt_error_message(&error));
            }
        }

        if let Some(messages) = initial_messages {
            for message in messages {
                let session = self.session();
                let result = session.prompt(message, PromptOptions::default()).await;
                if let Err(error) = result {
                    self.show_error(&prompt_error_message(&error));
                }
            }
        }

        loop {
            let user_input = self.get_user_input().await;
            // 第 12 周渲染核心（实验开关）：提交后先显示乐观气泡；回显由 MessageStart
            // 的 user 分支消费，150s 无回显由 10ms 循环 tick 转为可重试错误。
            self.note_pending_user_submission(&user_input);
            self.update_pending_messages_display();
            let session = self.session();
            let result = session.prompt(user_input, PromptOptions::default()).await;
            if let Err(error) = result {
                let message = prompt_error_message(&error);
                if self.note_pending_user_error(&message) {
                    self.update_pending_messages_display();
                }
                self.show_error(&message);
            }
        }
    }

    // -----------------------------------------------------------------------
    // run() 里启动的检查
    // -----------------------------------------------------------------------

    /// `this.keybindings.reload()`（TS `/reload`，interactive-mode.ts#handleReloadCommand）。
    ///
    /// TS 的 `KeybindingsManager` 是可变的单例：`reload()` 后 `this.keybindings`、默认编辑器
    /// 与 `kiana_tui` 全局单例看到的是同一份新绑定。Rust 的 [`CustomEditor`] 按构造器契约持有
    /// `Arc<KeybindingsManager>`（[`CustomEditor::new`]），`Arc` 无法原地 `reload()`，因此：
    /// 从同一配置文件重新加载一份等价 manager，并同时替换 `inner.keybindings`、全局单例与
    /// 默认编辑器里的句柄。
    ///
    /// 残余差异：扩展通过 [`EditorFactory`] 提供的编辑器在创建时拿到自己的
    /// `Arc<KeybindingsManager>`，而 `EditorComponent` 没有替换键位的接口，所以它们在
    /// `/reload` 后仍用旧绑定。要消除这个差异需要给 kiana-tui/kiana-ext-api 的冻结契约
    /// 加钩子（超出本单元范围）。
    pub(crate) fn reload_keybindings(&mut self) {
        let mut refreshed = (*self.inner.borrow().keybindings).clone();
        refreshed.reload();
        let refreshed = Arc::new(refreshed);
        set_keybindings(kiana_tui::keybindings::KeybindingsManager::new(
            kiana_config::keybindings::keybindings().clone(),
            refreshed.get_resolved_bindings(),
        ));
        self.inner.borrow_mut().keybindings = Arc::clone(&refreshed);
        let editor = Rc::clone(&self.inner.borrow().default_editor);
        editor.borrow_mut().set_keybindings(refreshed);
    }

    /// `checkForPackageUpdates()`（返回 `displayName` 数组；任何异常吞成空数组）。
    pub(crate) async fn check_for_package_updates(&self) -> Vec<String> {
        if env_is_truthy("KIANA_OFFLINE") {
            return Vec::new();
        }
        let session = self.session();
        let package_manager = DefaultPackageManager::new(PackageManagerOptions {
            cwd: session.session_manager().get_cwd().to_string(),
            agent_dir: get_agent_dir().to_string_lossy().into_owned(),
            settings_manager: session.settings_manager(),
            builtin_extensions: Vec::new(),
        });
        match package_manager.check_for_available_updates().await {
            Ok(updates) => updates
                .into_iter()
                .map(|update| update.display_name)
                .collect(),
            Err(_) => Vec::new(),
        }
    }

    /// `checkTmuxKeyboardSetup()`。
    pub(crate) async fn check_tmux_keyboard_setup(&self) -> Option<String> {
        if !env_is_truthy("TMUX") {
            return None;
        }
        let (extended_keys, extended_keys_format) = futures::future::join(
            run_tmux_show("extended-keys"),
            run_tmux_show("extended-keys-format"),
        )
        .await;

        // 无法查询（timeout / sandbox）时静默不提示
        let extended_keys = extended_keys?;
        if extended_keys != "on" && extended_keys != "always" {
            return Some("tmux extended-keys is off. Modified Enter keys may not work. Add `set -g extended-keys on` to ~/.tmux.conf and restart tmux.".to_string());
        }
        if extended_keys_format.as_deref() == Some("xterm") {
            return Some("tmux extended-keys-format is xterm. Kiana works best with csi-u. Add `set -g extended-keys-format csi-u` to ~/.tmux.conf and restart tmux.".to_string());
        }
        None
    }

    /// `getChangelogForDisplay()`。
    ///
    /// 只在会话还没有任何消息时可能返回内容（恢复已有会话不展示 changelog）。返回 `None`
    /// 的两种情况：设置里没有记录过版本（全新安装），或没有比记录版本更新的条目。
    ///
    /// **有副作用**：前一种情况会把当前版本写回设置并触发一次安装遥测（fire-and-forget）；
    /// 后一种情况只返回 `None`，不写设置也不上报。因此同一版本的后续启动不会再返回内容。
    pub(crate) fn get_changelog_for_display(&self) -> Option<String> {
        let session = self.session();
        if !session.messages().is_empty() {
            return None;
        }
        let settings_manager = session.settings_manager();
        let last_version = settings_manager.get_last_changelog_version();
        let changelog_path = get_changelog_path();
        let entries = parse_changelog(&changelog_path);
        let Some(last_version) = last_version.filter(|version| !version.is_empty()) else {
            // 全新安装：记录版本、发送遥测、不显示 changelog
            settings_manager.set_last_changelog_version(VERSION.to_string());
            self.report_install_telemetry(VERSION);
            return None;
        };
        let new_entries = get_new_entries(&entries, &last_version);
        if new_entries.is_empty() {
            return None;
        }
        settings_manager.set_last_changelog_version(VERSION.to_string());
        self.report_install_telemetry(VERSION);
        Some(
            new_entries
                .iter()
                .map(|entry| normalize_changelog_links(&entry.content, entry))
                .collect::<Vec<_>>()
                .join("\n\n"),
        )
    }

    /// `reportInstallTelemetry(version)`（fire-and-forget）。
    ///
    /// 不发送的情况：`KIANA_OFFLINE` 为真、设置里关闭了安装遥测，或取不到
    /// `api/report-install` 的基址。请求在 `spawn_local` 里发出，5 秒超时，响应被丢弃。
    pub(crate) fn report_install_telemetry(&self, version: &str) {
        if env_is_truthy("KIANA_OFFLINE") {
            return;
        }
        if !kiana_config::defaults::is_install_telemetry_enabled(&self.settings_manager()) {
            return;
        }
        let Some(report_url) = kiana_core::config::get_api_url("api/report-install") else {
            return;
        };
        let url = format!("{report_url}?version={}", encode_uri_component(version));
        let user_agent = kiana_core::changelog::get_kiana_user_agent(version);
        tokio::task::spawn_local(async move {
            let client = shared_http_client();
            let _ = client
                .get(url)
                .header("User-Agent", user_agent)
                .timeout(Duration::from_secs(5))
                .send()
                .await;
        });
    }

    // -----------------------------------------------------------------------
    // 显示 / 启动开关
    // -----------------------------------------------------------------------

    /// `formatDisplayPath(p)`。
    ///
    /// home 目录为空时原样返回；否则按**字符串前缀**匹配（不检查路径分量边界），命中后
    /// 把前缀替换成 `~`。
    pub(crate) fn format_display_path(&self, path: &str) -> String {
        let home = dirs::home_dir()
            .map(|home| home.to_string_lossy().into_owned())
            .unwrap_or_default();
        if !home.is_empty() && path.starts_with(&home) {
            return format!("~{}", &path[home.len()..]);
        }
        path.to_string()
    }

    /// `formatExtensionDisplayPath(path)`。
    pub(crate) fn format_extension_display_path(&self, path: &str) -> String {
        let result = self.format_display_path(path);
        let result = result.strip_suffix("/index.ts").unwrap_or(&result);
        let result = result.strip_suffix("/index.js").unwrap_or(result);
        result.to_string()
    }

    /// `formatContextPath(p)`。
    ///
    /// TS 用 `path.resolve` / `path.isAbsolute`，Node **从不**展开 `~`；这里因此关闭
    /// `resolve_path` 的 `expand_tilde`（默认开启）——`~/x` 在 TS 里就是 cwd 下名为 `~`
    /// 的目录，而不是 home。
    pub(crate) fn format_context_path(&self, path: &str) -> String {
        let options = node_path_options();
        let cwd = kiana_core::paths::resolve_path(
            self.session_manager().get_cwd(),
            &current_process_cwd(),
            &options,
        );
        let absolute_path = kiana_core::paths::resolve_path(path, &cwd, &options);
        match kiana_core::paths::get_cwd_relative_path(&absolute_path, &cwd) {
            Some(relative) => relative,
            None => self.format_display_path(&absolute_path),
        }
    }

    /// `getStartupExpansionState()`。
    pub(crate) fn get_startup_expansion_state(&self) -> bool {
        self.inner.borrow().options.verbose == Some(true)
            || self.inner.borrow().tool_output_expanded
    }

    /// `shouldShowStartupHeader()`。
    pub(crate) fn should_show_startup_header(&self) -> bool {
        if self.inner.borrow().options.verbose == Some(true) {
            return true;
        }
        !matches!(
            self.settings_manager().get_quiet_startup(),
            QuietStartup::Boolean(true)
        )
    }

    /// `shouldShowStartupDetails()`。
    pub(crate) fn should_show_startup_details(&self) -> bool {
        if self.inner.borrow().options.verbose == Some(true) {
            return true;
        }
        matches!(
            self.settings_manager().get_quiet_startup(),
            QuietStartup::Boolean(false)
        )
    }

    /// `isPackageSource(sourceInfo)`。
    pub(crate) fn is_package_source(&self, source_info: Option<&SourceInfo>) -> bool {
        let source = source_info.map(|info| info.source.as_str()).unwrap_or("");
        source.starts_with("npm:") || source.starts_with("git:")
    }

    /// `getShortPath(fullPath, sourceInfo)`。
    ///
    /// 按顺序尝试四种缩短方式，全部路径先统一成 `/` 分隔：
    /// 1. 包来源（`npm:` / `git:`）且有 `base_dir`：能从 `.../node_modules` 根算出相对
    ///    路径时直接返回；
    /// 2. 包来源且有 `base_dir`：`native_relative` 的结果在目录内（不以 `..` 开头、不是
    ///    绝对路径）时返回；
    /// 3. `npm:` 来源：取路径里 `node_modules/<pkg>/` 之后的部分；
    /// 4. `git:` 来源：取路径里 `git/<host>/<repo>/` 之后的部分。
    ///
    /// 都不匹配时退回 [`Self::format_display_path`]（home 目录替换为 `~`）。
    pub(crate) fn get_short_path(
        &self,
        full_path: &str,
        source_info: Option<&SourceInfo>,
    ) -> String {
        let normalized_full_path = full_path.replace('\\', "/");
        if let Some(info) = source_info
            && let Some(base_dir) = info.base_dir.as_deref()
            && self.is_package_source(Some(info))
        {
            let normalized_base_dir = base_dir.replace('\\', "/");
            if let Some(npm_root) = npm_root_of(&normalized_base_dir)
                && normalized_full_path.starts_with(&format!("{npm_root}/"))
            {
                return posix_relative(&normalized_base_dir, &normalized_full_path);
            }
            let relative = native_relative(base_dir, full_path);
            if !relative.is_empty()
                && relative != "."
                && !relative.starts_with("..")
                && !native_is_absolute(&relative)
            {
                return relative.replace('\\', "/");
            }
        }

        let source = source_info.map(|info| info.source.as_str()).unwrap_or("");
        if let Some(captures) = node_modules_match(&normalized_full_path)
            && source.starts_with("npm:")
        {
            return captures.to_string();
        }
        if let Some(captures) = git_path_match(&normalized_full_path)
            && source.starts_with("git:")
        {
            return captures.to_string();
        }
        self.format_display_path(full_path)
    }

    /// `updateTerminalTitle()`。
    pub(crate) fn update_terminal_title(&mut self) {
        let cwd_basename = path_basename(self.session_manager().get_cwd());
        let session_name = self
            .session_manager()
            .get_session_name()
            .filter(|name| !name.is_empty());
        let title = match session_name {
            Some(name) => format!("{APP_TITLE} - {name} - {cwd_basename}"),
            None => format!("{APP_TITLE} - {cwd_basename}"),
        };
        self.inner
            .borrow_mut()
            .ui
            .terminal()
            .borrow_mut()
            .set_title(&title);
    }

    // -----------------------------------------------------------------------
    // stop()（TS 7086–7105 行）
    // -----------------------------------------------------------------------

    /// `stop(fullscreenExitOutput = settingsManager.getFullscreenExitOutput())`。
    pub fn stop(&mut self) {
        let output = self.settings_manager().get_fullscreen_exit_output();
        self.stop_with(output);
    }

    /// 指定 `fullscreenExitOutput` 的 `stop()`。
    ///
    /// 顺序：释放活动选择器 → 关闭终端进度 → 清状态指示器 → 关闭主题自动同步 →
    /// 移除扩展终端输入监听 → `dispose` footer（no-op）与 footer data provider
    /// （停止刷新计时器与 git watcher）→ 退订会话事件 → 停止 TUI（仅当 `is_initialized`，随后把
    /// 它置回 `false`）→ 注销信号处理器。`is_shutting_down` 为真时**保留**信号处理器
    /// （原因见函数内 #5724 注释）。
    pub fn stop_with(&mut self, fullscreen_exit_output: FullscreenExitOutput) {
        self.dispose_active_selector();
        if self.settings_manager().get_show_terminal_progress() {
            self.inner
                .borrow()
                .ui
                .terminal()
                .borrow_mut()
                .set_progress(false);
        }
        self.clear_status_indicator(None);
        self.disable_theme_auto_sync();
        self.clear_extension_terminal_input_listeners();
        let footer = Rc::clone(&self.inner.borrow().footer);
        footer.borrow_mut().dispose();
        self.inner.borrow().footer_data_provider.dispose();
        if let Some(task) = self.inner.borrow_mut().event_task.take() {
            task.abort();
        }
        let unsubscribe = self.inner.borrow_mut().unsubscribe.take();
        if let Some(unsubscribe) = unsubscribe {
            unsubscribe();
        }
        if self.inner.borrow().is_initialized {
            self.stop_interactive_tui(fullscreen_exit_output);
            self.inner.borrow_mut().is_initialized = false;
        }
        // #5724：关停期间必须保持信号处理器注册（TS 的 `stop()` 从不移除它们；
        // signal-exit 会在同一 dispatch 内重发 SIGTERM/SIGHUP）。
        if !self.inner.borrow().is_shutting_down {
            self.unregister_signal_handlers();
        }
    }
}

// ---------------------------------------------------------------------------
// 模块级辅助
// ---------------------------------------------------------------------------

/// `## [1.2.3]` 的版本号匹配（TS `changelogMarkdown.match(/##\s+\[?(\d+\.\d+\.\d+)\]?/)`）。
fn changelog_version_regex(changelog: &str) -> Option<&str> {
    static REGEX: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(r"##\s+\[?(\d+\.\d+\.\d+)\]?").expect("changelog version regex is valid")
    });
    REGEX
        .captures(changelog)
        .and_then(|captures| captures.get(1))
        .map(|matched| matched.as_str())
}

/// `spawn("tmux", ["show", "-gv", option])`（2 秒超时、exit 0 才取 stdout.trim()）。
async fn run_tmux_show(option: &str) -> Option<String> {
    let mut command = tokio::process::Command::new("tmux");
    command
        .args(["show", "-gv", option])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true);
    match tokio::time::timeout(Duration::from_millis(2000), command.output()).await {
        Ok(Ok(output)) if output.status.success() => {
            Some(String::from_utf8_lossy(&output.stdout).trim().to_string())
        }
        _ => None,
    }
}

/// `^(.*\/node_modules)\/(@?[^/]+(?:\/[^/]+)?)$` 的第一捕获组。
fn npm_root_of(path: &str) -> Option<&str> {
    static REGEX: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(r"^(.*/node_modules)/(@?[^/]+(?:/[^/]+)?)$")
            .expect("npm root regex is valid")
    });
    REGEX
        .captures(path)
        .and_then(|captures| captures.get(1))
        .map(|matched| matched.as_str())
}

/// `node_modules\/(@?[^/]+(?:\/[^/]+)?)\/(.*)` 的第二捕获组。
fn node_modules_match(path: &str) -> Option<&str> {
    static REGEX: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(r"node_modules/(@?[^/]+(?:/[^/]+)?)/(.*)")
            .expect("node_modules regex is valid")
    });
    REGEX
        .captures(path)
        .and_then(|captures| captures.get(2))
        .map(|matched| matched.as_str())
}

/// `git\/[^/]+\/[^/]+\/(.*)` 的第一捕获组。
fn git_path_match(path: &str) -> Option<&str> {
    static REGEX: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(r"git/[^/]+/[^/]+/(.*)").expect("git path regex is valid")
    });
    REGEX
        .captures(path)
        .and_then(|captures| captures.get(1))
        .map(|matched| matched.as_str())
}

/// `path.posix.relative(from, to)`（两个参数都是 `/` 分隔的绝对路径）。
fn posix_relative(from: &str, to: &str) -> String {
    let from_parts: Vec<&str> = from.split('/').filter(|part| !part.is_empty()).collect();
    let to_parts: Vec<&str> = to.split('/').filter(|part| !part.is_empty()).collect();
    let mut common = 0;
    while common < from_parts.len()
        && common < to_parts.len()
        && from_parts[common] == to_parts[common]
    {
        common += 1;
    }
    let mut parts: Vec<String> = Vec::new();
    for _ in common..from_parts.len() {
        parts.push("..".to_string());
    }
    for part in &to_parts[common..] {
        parts.push((*part).to_string());
    }
    if parts.is_empty() {
        // Node `path.posix.relative(a, a) === ""`（不是 "."）。
        String::new()
    } else {
        parts.join("/")
    }
}

/// `path.resolve` 的等价选项：Node 的 `path` 不展开 `~`（`kiana_core::paths::resolve_path`
/// 的默认值会展开，必须显式关掉）。
fn node_path_options() -> kiana_core::paths::PathInputOptions {
    kiana_core::paths::PathInputOptions {
        expand_tilde: false,
        ..Default::default()
    }
}

/// `path.resolve(a)`（以进程 cwd 为基准）。
fn resolve_native(path: &str) -> String {
    kiana_core::paths::resolve_path(path, &current_process_cwd(), &node_path_options())
}

/// `path.relative(resolve(from), resolve(to))`（只覆盖 `to` 在 `from` 内的情况；
/// 不在内部时返回以 `..` 开头的路径）。
fn native_relative(from: &str, to: &str) -> String {
    let from = resolve_native(from);
    let to = resolve_native(to);
    let from_path = std::path::Path::new(&from);
    let to_path = std::path::Path::new(&to);
    match to_path.strip_prefix(from_path) {
        Ok(relative) => {
            let text = relative.to_string_lossy().into_owned();
            if text.is_empty() {
                ".".to_string()
            } else {
                text
            }
        }
        Err(_) => {
            let mut parts: Vec<String> = Vec::new();
            for _ in from_path.components().skip(1) {
                parts.push("..".to_string());
            }
            parts.push(to);
            parts.join(std::path::MAIN_SEPARATOR_STR)
        }
    }
}

/// `path.isAbsolute(p)`（当前平台）。
fn native_is_absolute(path: &str) -> bool {
    if cfg!(windows) {
        let bytes = path.as_bytes();
        path.starts_with('\\') || path.starts_with('/') || (bytes.len() >= 2 && bytes[1] == b':')
    } else {
        path.starts_with('/')
    }
}

/// 共享测试夹具（`tests/common/mod.rs`，support-subsystems 单元维护）：这里按路径引用
/// 同一份文件（不复制、不另建 `tests/support`），供需要完整 `AgentSessionRuntime` 的回归
/// 测试使用；纯函数测试不需要它。
#[cfg(test)]
#[path = "../tests/common/mod.rs"]
mod test_common;

#[cfg(test)]
mod tests {
    use super::test_common as common;
    use super::*;
    use kiana_ai::providers::faux::{FauxAssistantMessageOptions, faux_assistant_message};
    use kiana_ai::types::StopReason;
    use kiana_session::agent_session::AgentSessionEvent;
    use kiana_tui::tui::TuiMouseButton;

    fn error_message(message: &str) -> AssistantMessage {
        faux_assistant_message(
            "",
            FauxAssistantMessageOptions {
                stop_reason: Some(StopReason::Error),
                error_message: Some(message.to_string()),
                ..Default::default()
            },
        )
    }

    /// `interactive-mode-bug-report-hint.test.ts` case 2。
    #[test]
    fn does_not_suggest_reports_for_retryable_provider_failures() {
        let failures = [
            "500 Internal Server Error",
            "502 Bad Gateway",
            "503 Service Unavailable",
            "504 Gateway Timeout",
            "429 Too Many Requests",
            "Provider overloaded",
            "Network connection lost",
            "Request timed out",
        ];
        for failure in failures {
            assert!(
                !should_suggest_bug_report(&error_message(failure)),
                "unexpected bug hint for {failure}"
            );
        }
    }

    /// case 3：取消/中止不算崩溃。
    #[test]
    fn does_not_suggest_reports_for_cancellations() {
        assert!(!should_suggest_bug_report(&error_message(
            "This operation was aborted"
        )));
        assert!(!should_suggest_bug_report(&error_message(
            "Request cancelled"
        )));
        let aborted = faux_assistant_message(
            "",
            FauxAssistantMessageOptions {
                stop_reason: Some(StopReason::Aborted),
                ..Default::default()
            },
        );
        assert!(!should_suggest_bug_report(&aborted));
    }

    /// case 4：意外错误给出提示。
    #[test]
    fn suggests_reports_for_unexpected_errors() {
        assert!(should_suggest_bug_report(&error_message(
            "Unexpected internal state"
        )));
    }

    /// case 5：安装变更的判定在调用方（`maybeShowInstallChangeWarning`）；纯判定仍返回 true。
    ///
    /// TS 用 `vi.fn(() => true)` mock 该调用并断言 `suggestBugReport` 未被调用；Rust 的
    /// 组合方法（`maybe_suggest_bug_report`）在 `interactive_mode/render.rs`（im-runtime
    /// 单元），这条优先级断言只能在那里覆盖（本单元无法构造/mock 那个方法）。
    #[test]
    fn unexpected_module_errors_are_candidates_for_the_bug_hint() {
        assert!(should_suggest_bug_report(&error_message(
            "Cannot find module './worker.js'"
        )));
    }

    /// `formatCrashExtensionHint` 的多种匹配（case 1 在集成测试里）。
    #[test]
    fn formats_multiple_extension_matches() {
        assert_eq!(
            format_crash_extension_hint(Some(&["a".to_string(), "b".to_string()])).as_deref(),
            Some(
                "A stack frame came from loaded extensions `a` and `b`, which may be involved. Try disabling them with `kiana config`, or run `kiana -ne` to confirm."
            )
        );
        assert_eq!(
            format_crash_extension_hint(Some(&[
                "a".to_string(),
                "b".to_string(),
                "c".to_string(),
                "d".to_string(),
            ]))
            .as_deref(),
            Some(
                "A stack frame came from loaded extensions `a`, `b`, `c`, and `d`, which may be involved. Try disabling them with `kiana config`, or run `kiana -ne` to confirm."
            )
        );
        assert_eq!(format_crash_extension_hint(Some(&[])), None);
    }

    /// `BuiltInHeader.handleMouse` 的命中条件与屏幕坐标换算（spec §2.50.1 逐字）。
    #[test]
    fn built_in_header_hit_test_uses_screen_coordinates() {
        let mut header = BuiltInHeader::new(
            Rc::new(|| "collapsed".to_string()),
            Rc::new(|| "expanded".to_string()),
            false,
            1,
            0,
        );
        let clicks: Rc<RefCell<Vec<(usize, usize)>>> = Rc::new(RefCell::new(Vec::new()));
        let clicks_for_callback = Rc::clone(&clicks);
        header.on_logo_click = Some(Box::new(move |column, row| {
            clicks_for_callback.borrow_mut().push((column, row));
        }));

        let event = |event_type: TuiMouseEventType, x: isize, y: isize| TuiMouseEvent {
            event_type,
            button: TuiMouseButton::Left,
            x,
            y,
            screen_x: 10,
            screen_y: 5,
            width: 80,
            height: 24,
            shift: false,
            alt: false,
            ctrl: false,
            wheel_delta: None,
            click_count: Some(1),
        };

        let result = header.handle_mouse(&event(TuiMouseEventType::Click, 1, 0));
        assert_eq!(result.and_then(|result| result.handled), Some(true));
        assert_eq!(clicks.borrow().as_slice(), &[(10, 5)]);

        // `x > 4` / `y > 1` / 非 click 都不命中。
        assert!(
            header
                .handle_mouse(&event(TuiMouseEventType::Click, 5, 0))
                .is_none()
        );
        assert!(
            header
                .handle_mouse(&event(TuiMouseEventType::Click, 1, 2))
                .is_none()
        );
        assert!(
            header
                .handle_mouse(&event(TuiMouseEventType::Press, 1, 0))
                .is_none()
        );
        assert_eq!(clicks.borrow().len(), 1);
    }

    /// 运行时单元用 `tokio::sync::mpsc::UnboundedSender<AgentSessionEvent>` 把
    /// `AgentSession::subscribe`（`Send + Sync` 回调）的事件转投到 UI LocalSet；
    /// 这里做编译期断言（spec §7.2 / planner 要求）。
    #[test]
    fn agent_session_event_is_clone_and_send() {
        fn assert_clone<T: Clone>() {}
        fn assert_send<T: Send>() {}
        assert_clone::<kiana_session::agent_session::AgentSessionEvent>();
        assert_send::<kiana_session::agent_session::AgentSessionEvent>();
    }

    /// `quoteIfNeeded` 的安全字符集合。
    #[test]
    fn quotes_only_when_needed() {
        assert_eq!(
            quote_if_needed("/tmp/custom-kiana_sessions.1"),
            "/tmp/custom-kiana_sessions.1"
        );
        assert_eq!(quote_if_needed("a:b@c~d"), "a:b@c~d");
        assert_eq!(quote_if_needed(""), "''");
        assert_eq!(quote_if_needed("a b"), "'a b'");
        assert_eq!(quote_if_needed("a'b"), "'a'\\''b'");
        assert_eq!(quote_if_needed("ümlaut"), "'ümlaut'");
    }

    /// `encodeURIComponent` 的保留字符。
    #[test]
    fn encodes_uri_components_like_javascript() {
        assert_eq!(encode_uri_component("1.4.0"), "1.4.0");
        assert_eq!(encode_uri_component("a b/c"), "a%20b%2Fc");
        assert_eq!(encode_uri_component("!*'()~-_.a"), "!*'()~-_.a");
        assert_eq!(encode_uri_component("ü"), "%C3%BC");
    }

    /// `new Date(crash.timestamp).toLocaleString()`：**本地时区**（review #4）。
    ///
    /// Node 26：`TZ=Asia/Shanghai node -e 'console.log(new Date("2026-11-03T20:05:09Z").toLocaleString("en-US"))'`
    /// → `11/4/2026, 4:05:09 AM`；UTC 渲染会得到 `11/3/2026, 8:05:09 PM`。
    #[test]
    fn formats_crash_timestamp_in_the_local_timezone() {
        static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let _guard = ENV_LOCK.lock().unwrap_or_else(|error| error.into_inner());
        let previous = std::env::var_os("TZ");
        // SAFETY: 该测试串行修改进程环境；`format_crash_timestamp` 立即读取，
        // 随后在本测试内恢复原值。
        unsafe { std::env::set_var("TZ", "Asia/Shanghai") };
        let formatted = format_crash_timestamp(Some("2026-11-03T20:05:09Z"));
        match previous {
            Some(value) => unsafe { std::env::set_var("TZ", value) },
            None => unsafe { std::env::remove_var("TZ") },
        }
        assert_eq!(formatted, "11/4/2026, 4:05:09 AM");
    }

    /// 无效 / 缺失时间戳与 JS `new Date(NaN).toLocaleString()` 一致。
    #[test]
    fn formats_invalid_crash_timestamps_as_invalid_date() {
        assert_eq!(format_crash_timestamp(None), "Invalid Date");
        assert_eq!(format_crash_timestamp(Some("not a date")), "Invalid Date");
    }

    /// JS 真值：空描述与缺失描述都只产生 `[tag]`（review #7）。
    #[test]
    fn prefixes_autocomplete_descriptions_with_js_truthiness() {
        assert_eq!(
            prefix_autocomplete_description_with(None, Some("hello")),
            Some("hello".to_string())
        );
        assert_eq!(
            prefix_autocomplete_description_with(None, Some("")),
            Some(String::new())
        );
        assert_eq!(
            prefix_autocomplete_description_with(Some("u"), Some("hello")),
            Some("[u] hello".to_string())
        );
        assert_eq!(
            prefix_autocomplete_description_with(Some("u"), Some("")),
            Some("[u]".to_string())
        );
        assert_eq!(
            prefix_autocomplete_description_with(Some("u"), None),
            Some("[u]".to_string())
        );
    }

    /// `/login` provider 排序走 `localeCompare`，不是码点序（review #6）。
    #[test]
    fn login_provider_options_use_locale_compare_order() {
        use kiana_ai::auth::types::AuthType;

        let provider = |id: &str, name: &str| AuthSelectorProvider {
            id: id.to_string(),
            name: name.to_string(),
            auth_type: AuthType::ApiKey,
            method: None,
            status: None,
            subscription: None,
        };
        let options = get_login_provider_completion_options(&[
            provider("zai", "Z.AI"),
            provider("xai", "xAI"),
            provider("lmstudio", "LM Studio"),
            provider("llamacpp", "llama.cpp"),
        ]);
        let names: Vec<&str> = options.iter().map(|option| option.name.as_str()).collect();
        assert_eq!(names, ["llama.cpp", "LM Studio", "xAI", "Z.AI"]);
    }

    /// 编辑器代理的焦点地址必须是 `Box` 的堆地址（review #2）。
    ///
    /// 装箱前的栈地址永远解析不到组件；`init()` 与
    /// `setCustomEditorComponent` 的 `setFocus(editor)` 都依赖它。
    #[test]
    fn editor_proxy_address_resolves_to_the_mounted_component() {
        static THEME: std::sync::Once = std::sync::Once::new();
        THEME.call_once(|| kiana_theme::init_theme(Some("dark"), false));

        let terminal: Rc<RefCell<Box<dyn Terminal>>> =
            Rc::new(RefCell::new(Box::new(NullTerminal)));
        let editor = Rc::new(RefCell::new(CustomEditor::new(
            EditorTui::from_terminal(Rc::clone(&terminal)),
            get_editor_theme(),
            Arc::new(KeybindingsManager::create_default()),
            CustomEditorOptions {
                padding_x: Some(0.0),
                autocomplete_max_visible: Some(5.0),
                embed_working_status: true,
            },
        )));

        // 旧实现取的是装箱前栈上临时值的地址，无法在挂载树里解析。
        let stack_proxy = DefaultEditorProxy(Rc::clone(&editor));
        let stale_address = &stack_proxy as *const DefaultEditorProxy as *const () as usize;

        let container = new_container();
        let (proxy, address) = DefaultEditorProxy::new(Rc::clone(&editor));
        container.borrow_mut().add_child(proxy);

        let mut ui = kiana_tui::TuiMainScreen::new(Box::new(NullTerminal));
        ui.base_mut().add_child(boxed_shared(&shared(&container)));

        assert!(
            ui.base().focus_target_of_address(address).is_some(),
            "boxed editor proxy address must resolve in the mount tree"
        );
        assert_eq!(ui.base().focus_target_of_address(stale_address), None);
    }

    /// 扩展提供的编辑器必须收到 provider（review #5：旧实现 `drop(provider)` 丢掉了）。
    #[test]
    fn custom_editor_receives_the_autocomplete_provider() {
        use kiana_tui::editor_component::{AutocompleteProviderObject, EditorComponent};

        struct RecordingEditor {
            received: Rc<Cell<bool>>,
        }
        impl Component for RecordingEditor {
            fn render(&mut self, _width: usize) -> Vec<String> {
                Vec::new()
            }
            fn handle_input(&mut self, _data: &str) {}
        }
        impl EditorComponent for RecordingEditor {
            fn get_text(&self) -> String {
                String::new()
            }
            fn set_text(&mut self, _text: &str) {}
            fn set_autocomplete_provider(&mut self, provider: AutocompleteProviderObject) {
                self.received
                    .set(provider.downcast::<Box<dyn AutocompleteProvider>>().is_ok());
            }
        }

        let received = Rc::new(Cell::new(false));
        let handle: Rc<RefCell<Box<dyn EditorComponent>>> =
            Rc::new(RefCell::new(Box::new(RecordingEditor {
                received: Rc::clone(&received),
            })));
        let editor = ActiveEditor::Custom {
            handle: Rc::clone(&handle),
            address: 0,
        };
        editor.set_autocomplete_provider(Box::new(CombinedAutocompleteProvider::new(
            Vec::new(),
            ".".to_string(),
            None,
        )));
        assert!(
            received.get(),
            "the active custom editor must receive the provider"
        );
    }

    // -----------------------------------------------------------------------
    // 审阅回归（review #1/#2/#3/#4/#5/#7/#8/#9/#10）
    // -----------------------------------------------------------------------

    /// `path.posix.relative(a, a)` 是 `""`（review #7）。
    #[test]
    fn posix_relative_returns_empty_for_equal_paths() {
        assert_eq!(posix_relative("/a", "/a"), "");
        assert_eq!(posix_relative("/a/", "/a"), "");
        assert_eq!(posix_relative("/a/b", "/a/b/c"), "c");
        assert_eq!(posix_relative("/a/b", "/a"), "..");
    }

    /// Node `path.posix.basename` / `path.win32.basename`（review #9）。
    #[test]
    fn path_basename_matches_node_posix() {
        assert_eq!(path_basename("/a/b/"), "b");
        assert_eq!(path_basename("/a/b"), "b");
        assert_eq!(path_basename("/"), "");
        assert_eq!(path_basename(""), "");
    }

    /// Windows 盘符 root 的 basename 是空串（Node `path.win32.basename("C:\\") === ""`）。
    #[cfg(windows)]
    #[test]
    fn path_basename_matches_node_win32_for_drive_roots() {
        assert_eq!(path_basename("C:\\"), "");
        assert_eq!(path_basename("C:"), "");
        assert_eq!(path_basename("C:/"), "");
        assert_eq!(path_basename("C:\\foo\\"), "foo");
        assert_eq!(path_basename("C:foo"), "foo");
        assert_eq!(path_basename("C::"), ":");
        // 非盘符（第二位是冒号但首位不是字母）保持整串。
        assert_eq!(path_basename("1:foo"), "1:foo");
        assert_eq!(path_basename("ab:cd"), "ab:cd");
    }

    /// `ExpandableText` 可从 `&mut dyn Component` 向下转型（review #5）。
    ///
    /// TS `isExpandable(child)` 对 `loadedResourcesContainer` / `chatContainer` 的子项做
    /// 结构判定；Rust 侧对应 `Component::as_any_mut`。
    #[test]
    fn expandable_text_can_be_downcast_from_a_component_reference() {
        let mut child: Box<dyn Component> = Box::new(ExpandableText::new(
            Rc::new(|| "collapsed".to_string()),
            Rc::new(|| "expanded".to_string()),
            false,
            1,
            0,
        ));
        let expandable = child
            .as_any_mut()
            .and_then(|any| any.downcast_mut::<ExpandableText>())
            .expect("ExpandableText must be downcastable");
        expandable.set_expanded(true);
        assert!(expandable.expanded());
        assert_eq!(expandable.expanded_text(), "expanded");
        assert!(child.as_any().is_some());
    }

    /// Node 的 `path.resolve` 从不展开 `~`（review #8）。
    #[test]
    fn node_path_options_do_not_expand_tilde() {
        let options = node_path_options();
        assert!(!options.expand_tilde);
        assert_eq!(
            kiana_core::paths::resolve_path("~/notes.md", "/tmp/project", &options),
            "/tmp/project/~/notes.md"
        );
    }

    /// TS `error instanceof Error ? error.message : "Unknown error occurred"`（review #10）。
    #[test]
    fn prompt_errors_use_the_ts_unknown_error_fallback() {
        assert_eq!(
            prompt_error_message(&AgentSessionError::other("boom")),
            "boom"
        );
        assert_eq!(
            prompt_error_message(&AgentSessionError::other("")),
            "Unknown error occurred"
        );
        assert_eq!(
            prompt_error_message(&AgentSessionError::NoModelSelected),
            AgentSessionError::NoModelSelected.to_string()
        );
    }

    /// 完整交互模式 + 录制终端的测试宿主。
    struct TestMode {
        mode: InteractiveMode,
        program_statuses: Rc<RefCell<Vec<kiana_tui::terminal_colors::ProgramStatus>>>,
        _engine: common::TestEngine,
        _temp_dir: tempfile::TempDir,
    }

    async fn create_test_mode() -> TestMode {
        let host = common::runtime_host(common::RuntimeHostOptions::new()).await;
        let common::RuntimeHost {
            runtime,
            engine,
            temp_dir,
            ..
        } = host;
        let terminal = common::TestTerminal::new();
        let program_statuses = terminal.program_status_handle();
        let mode = InteractiveMode::new(
            runtime,
            InteractiveModeOptions {
                terminal: Some(Box::new(terminal)),
                ..Default::default()
            },
        );
        TestMode {
            mode,
            program_statuses,
            _engine: engine,
            _temp_dir: temp_dir,
        }
    }

    fn init_test_theme() {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(|| {
            kiana_theme::init_theme(Some("dark"), false);
        });
    }

    /// review #1：事件驱动的 OSC 7501 上报必须带上会话名。
    ///
    /// im-runtime 的写法是 `self.inner.borrow_mut().program_status.handle_event(&event)`
    /// （`program_status` 不是 `Clone`）。旧实现从同一个 `InteractiveModeInner` 里
    /// `try_borrow` 读会话名；事件路径必然失败，working/done 丢掉 `message`。
    #[tokio::test(flavor = "current_thread")]
    async fn program_status_reports_the_session_name_while_inner_is_borrowed() {
        common::run_local(async {
            let test = create_test_mode().await;
            let mode = &test.mode;
            mode.session().set_session_name("Fix login");

            {
                // TS 字段表里的两个 readonly 默认值（review #4）。
                let inner = mode.inner.borrow();
                assert_eq!(inner.default_working_message, "Working");
                assert_eq!(inner.default_hidden_thinking_label, "Thinking...");
                assert_eq!(inner.hidden_thinking_label, "Thinking...");
            }

            let mut inner = mode.inner.borrow_mut();
            inner
                .program_status
                .handle_event(&kiana_session::agent_session::AgentSessionEvent::AgentStart);
            drop(inner);

            let statuses = test.program_statuses.borrow();
            let last = statuses.last().expect("agent_start must report a status");
            assert_eq!(
                last.state,
                kiana_tui::terminal_colors::ProgramStatusState::Working
            );
            assert_eq!(last.message.as_deref(), Some("Fix login"));
            assert_eq!(last.app.as_deref(), Some(APP_NAME));
        })
        .await;
    }

    /// review #2：mermaid 的 `getMode` 必须读**当前会话**的 SettingsManager。
    #[tokio::test(flavor = "current_thread")]
    async fn mermaid_transformer_follows_the_current_sessions_settings_manager() {
        common::run_local(async {
            init_test_theme();
            let test = create_test_mode().await;
            let mode = &test.mode;

            // 旧会话关闭渲染；新会话（新的 SettingsManager 实例）开启。
            mode.session()
                .settings_manager()
                .set_mermaid_rendering_mode(kiana_config::settings::MermaidRenderingMode::Off)
                .expect("set mermaid mode");
            let runtime = Rc::clone(&mode.inner.borrow().runtime_host);
            runtime.new_session(None).await.expect("new session");
            mode.session()
                .settings_manager()
                .set_mermaid_rendering_mode(kiana_config::settings::MermaidRenderingMode::Final)
                .expect("set mermaid mode");

            let transformer = Arc::clone(&mode.inner.borrow().mermaid_markdown_transformer);
            let markdown = "```mermaid\nflowchart LR\n  A --> B\n```";
            let context = kiana_ext_api::types::MarkdownTransformContext {
                available_width: 120,
                ..Default::default()
            };
            let rendered = transformer(markdown, &context);
            assert_ne!(
                rendered, markdown,
                "getMode must follow the new session's settings manager (mode=final)"
            );
        })
        .await;
    }

    /// review #3：`/reload` 后默认编辑器也必须看到新键位。
    ///
    /// TS 的 `KeybindingsManager` 是可变单例；Rust 侧编辑器持有 `Arc`，核心的
    /// [`InteractiveMode::reload_keybindings`] 负责替换 inner、全局与编辑器的句柄。
    #[tokio::test(flavor = "current_thread")]
    async fn reload_keybindings_updates_the_manager_the_global_and_the_editor() {
        common::run_local(async {
            let mut test = create_test_mode().await;
            let mode = &mut test.mode;

            let dir = tempfile::tempdir().expect("temp dir");
            let path = dir.path().join("keybindings.json");
            std::fs::write(&path, "{}").expect("write v1");
            mode.inner.borrow_mut().keybindings = Arc::new(KeybindingsManager::new(
                kiana_config::keybindings::KeybindingsConfig::new(),
                Some(path.clone()),
            ));
            // 让编辑器先持有同一份 manager。
            mode.reload_keybindings();

            let fired = Rc::new(Cell::new(0));
            {
                let editor = mode.default_editor();
                let fired = Rc::clone(&fired);
                editor.borrow_mut().set_action_handler(
                    "app.interrupt",
                    Box::new(move || fired.set(fired.get() + 1)),
                );
            }

            let press_ctrl_x = |mode: &InteractiveMode| {
                let editor = mode.default_editor();
                Component::handle_input(&mut *editor.borrow_mut(), "\x18");
            };
            press_ctrl_x(mode);
            assert_eq!(
                fired.get(),
                0,
                "ctrl+x must not be app.interrupt before the reload"
            );

            std::fs::write(&path, "{\"app.interrupt\":\"ctrl+x\"}").expect("write v2");
            mode.reload_keybindings();
            press_ctrl_x(mode);
            assert_eq!(
                fired.get(),
                1,
                "the default editor must use the reloaded bindings"
            );

            assert!(
                mode.inner
                    .borrow()
                    .keybindings
                    .matches("\x18", "app.interrupt")
            );
            assert!(kiana_tui::keybindings::get_keybindings().matches("\x18", "app.interrupt"));
        })
        .await;
    }

    /// 第 12 周渲染核心：实验开关在构造时读一次，之后环境变量变化不影响已构造的宿主。
    #[test]
    fn render_core_flag_is_read_once_at_construction() {
        static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let _guard = ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let previous = std::env::var("KIANA_NEXT_RENDER_CORE").ok();
        unsafe { std::env::set_var("KIANA_NEXT_RENDER_CORE", "1") };
        let host = render_core_host::RenderCoreHost::from_env();
        unsafe { std::env::set_var("KIANA_NEXT_RENDER_CORE", "0") };
        let enabled_after_change = host.enabled();
        match previous {
            Some(value) => unsafe { std::env::set_var("KIANA_NEXT_RENDER_CORE", value) },
            None => unsafe { std::env::remove_var("KIANA_NEXT_RENDER_CORE") },
        }
        assert!(host.enabled());
        assert!(
            enabled_after_change,
            "the flag must be captured once, not re-read per call"
        );
    }

    /// 第 12 周渲染核心的端到端接线：提交显示在待发区域，`MessageStart` 的 user 回显
    /// 消费它，两处不会同时出现同一条输入。
    #[tokio::test(flavor = "current_thread")]
    async fn render_core_pending_user_message_is_rendered_and_consumed_by_the_echo() {
        init_test_theme();
        common::run_local(async {
            let mut test = create_test_mode().await;
            // 环境变量在所有测试间共享；这里直接注入开关，避免并行测试互相影响。
            test.mode.inner.borrow_mut().render_core_host =
                render_core_host::RenderCoreHost::new(true);

            test.mode
                .note_pending_user_submission("hello from the user");
            assert_eq!(
                test.mode.pending_user_lines(),
                vec!["Sending... hello from the user".to_string()]
            );
            test.mode.update_pending_messages_display();
            let rendered = {
                let container = Rc::clone(&test.mode.inner.borrow().pending_messages_container);
                let mut container = container.borrow_mut();
                container
                    .render(120)
                    .iter()
                    .map(|line| kiana_tui::text::strip_terminal_sequences(line))
                    .collect::<Vec<_>>()
                    .join("\n")
            };
            assert!(
                rendered.contains("Sending... hello from the user"),
                "{rendered}"
            );

            test.mode
                .handle_event(&AgentSessionEvent::MessageStart {
                    message: common::user_msg("hello from the user"),
                })
                .await;
            assert!(test.mode.pending_user_lines().is_empty());
            let rendered = {
                let container = Rc::clone(&test.mode.inner.borrow().pending_messages_container);
                let mut container = container.borrow_mut();
                container
                    .render(120)
                    .iter()
                    .map(|line| kiana_tui::text::strip_terminal_sequences(line))
                    .collect::<Vec<_>>()
                    .join("\n")
            };
            assert!(
                !rendered.contains("Sending... hello from the user"),
                "{rendered}"
            );
        })
        .await;
    }
}
