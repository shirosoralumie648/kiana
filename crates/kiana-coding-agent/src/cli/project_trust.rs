//! `cli/project-trust.ts`（62 行）：启动期 `ProjectTrustContext`（选择器版）。
//!
//! TS 的 `ProjectTrustContext`（`core/extensions/types.ts`）是扩展侧视图：
//! `{ cwd, mode: "tui"|"print"|"json"|"rpc", hasUI, ui: { select, confirm, input, notify } }`。
//! Rust 侧同一概念存在**两套**类型：
//!
//! - [`kiana_ext_api::events::ProjectTrustContext`]：给扩展处理器用，四个 UI 回调返回
//!   [`kiana_ext_api::LocalBoxFuture`]（`!Send`，因为 TUI 组件基于 `Rc`）；
//! - [`kiana_config::trust::project_trust::ProjectTrustContext`]：给
//!   [`resolve_project_trusted`](kiana_config::trust::project_trust::resolve_project_trusted)
//!   的决策逻辑用，只有 `has_ui` + `Option<Arc<dyn ProjectTrustUi>>`，trait 的 `select`
//!   返回 `Send` future。
//!
//! [`create_project_trust_context_views`] 从同一组选项同时构造两份视图：
//! 扩展视图供 `emit_project_trust_event` 使用，kiana-config 视图由
//! [`ProjectTrustAdapter`] 把 `select` 转发回扩展视图的回调。适配器把 `!Send` 的
//! [`LocalBoxFuture`](kiana_ext_api::LocalBoxFuture) 包成 `Send`：所有调用都发生在同一个 UI 线程（`LocalSet`）上，
//! `Send` 只是 `async_trait` 的签名要求（与 `kiana-interactive` 的 `SendOverlayOptions`
//! 同一理由）。
//!
//! # 执行流程
//! 1. [`create_project_trust_context`] 用一份 [`ProjectTrustOptions`] 构造四个 UI 回调；
//!    每个对话框回调进入时先检查 `has_ui` 与 `mode`，不满足就立即返回
//!    `None`/`false`，不会创建 TUI。
//! 2. [`create_project_trust_context_views`] 再包装一个只实现 `select` 的
//!    [`ProjectTrustAdapter`]，供 `resolve_project_trusted` 的 `project_trust_context` 使用。
//! 3. [`ProjectTrustEmitterAdapter`] 把 kiana-config 的 `project_trust` 事件转发给
//!    `kiana_extensions::project_trust::emit_project_trust_event`，并把扩展决定映射回
//!    `"yes"`/其它字符串（下游只比较是否等于 `"yes"`）。
//!
//! # 注意事项
//! - 对话框每次都通过 `show_startup_selector`/`show_startup_input` 新建并销毁一个启动期
//!   TUI，因此调用方必须在 `LocalSet` 上执行（TUI 基于 `Rc`）；
//!   `ProjectTrustEmitterAdapter` 的 `unsafe impl Send/Sync` 只是 trait 签名适配，
//!   不构成并发保证。
//! - `notify` 在交互模式下是 no-op（由 TUI 自己展示），非交互模式按 `NotifyKind` 染色写
//!   stderr。
//! - `config` 视图的 `ui` 恒为 `Some`；`has_ui == false` 时由 kiana-config 的决策逻辑先
//!   短路，不会调用 `select`。
//!
//! TS 源：`packages/coding-agent/src/cli/project-trust.ts`、`spec/ca-cli.md` §2.16 / §5.4。

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use async_trait::async_trait;
use kiana_config::settings::SettingsManager;
use kiana_config::trust::project_trust::{
    AppMode, EmitProjectTrustResult, ProjectTrustContext as ConfigProjectTrustContext,
    ProjectTrustEmitter, ProjectTrustEventError, ProjectTrustEventOutcome,
    ProjectTrustUi as ConfigProjectTrustUi,
};
use kiana_ext_api::events::{
    ProjectTrustContext as ExtProjectTrustContext, ProjectTrustEventDecision,
    ProjectTrustEventType, ProjectTrustUi as ExtProjectTrustUi,
};
use kiana_ext_api::types::{ExtensionMode, LoadExtensionsResult};
use kiana_ext_api::ui::NotifyKind;

use crate::cli::setup::{Output, Style};
use crate::cli::startup_ui::{StartupSelectorOption, show_startup_input, show_startup_selector};

/// `AppMode` → 扩展事件的 `ExtensionMode`。
///
/// TS: `options.mode === "interactive" ? "tui" : options.mode`。
pub fn extension_mode_for_app_mode(mode: AppMode) -> ExtensionMode {
    match mode {
        AppMode::Interactive => ExtensionMode::Tui,
        AppMode::Print => ExtensionMode::Print,
        AppMode::Json => ExtensionMode::Json,
        AppMode::Rpc => ExtensionMode::Rpc,
    }
}

/// `ui.select` 回调类型（TS `(title, options, opts?) => Promise<string | undefined>`）。
type SelectCallback = Arc<
    dyn Fn(
            String,
            Vec<String>,
            Option<kiana_ext_api::ui::ExtensionUIDialogOptions>,
        ) -> kiana_ext_api::LocalBoxFuture<'static, Option<String>>
        + Send
        + Sync,
>;

/// `ui.confirm` 回调类型。
type ConfirmCallback = Arc<
    dyn Fn(
            String,
            String,
            Option<kiana_ext_api::ui::ExtensionUIDialogOptions>,
        ) -> kiana_ext_api::LocalBoxFuture<'static, bool>
        + Send
        + Sync,
>;

/// `ui.input` 回调类型。
type InputCallback = Arc<
    dyn Fn(
            String,
            Option<String>,
            Option<kiana_ext_api::ui::ExtensionUIDialogOptions>,
        ) -> kiana_ext_api::LocalBoxFuture<'static, Option<String>>
        + Send
        + Sync,
>;

/// [`create_project_trust_context`] 的选项。
///
/// TS 的 `{cwd, mode, settingsManager, hasUI}`；`output` 是 Rust 侧显式化的进程 stderr
/// （TS 的 `console.error`）。
///
/// 三个对话框回调和 `notify` 各自持有这份选项的一个克隆；`cwd` 只写入扩展视图、
/// 不参与 UI 调用。`mode` 决定三个对话框是否真的展示，`has_ui` 是宿主声明的
/// “是否存在可用对话框 UI”。
#[derive(Clone)]
pub struct ProjectTrustOptions {
    /// 工作目录。
    pub cwd: String,
    /// 当前运行模式。
    pub mode: AppMode,
    /// 设置管理器（启动选择器需要）。
    pub settings_manager: SettingsManager,
    /// 是否有可用的对话框 UI。
    pub has_ui: bool,
    /// 非交互模式下 `notify` 的输出通道。
    pub output: Output,
}

/// `createProjectTrustContext(options)`：构造扩展侧的项目信任上下文。
///
/// 返回的四个回调共享同一份选项克隆：
/// - `select`/`input`：`!has_ui` 或 `mode != Interactive` 时直接返回 `None`，否则弹启动期
///   选择器/输入框；`select` 的候选值是标签本身（`label == value`）；
/// - `confirm`：同样短路返回 `false`；否则以 `"{title}\n{message}"` 为标题展示固定的
///   Yes/No 两项，非 “Yes”（含取消、未知标签）都得到 `false`；
/// - `notify`：交互模式 no-op（TUI 自己展示）；非交互模式按 kind 染色（Error 红、
///   Warning 黄、Info/未指定 青）写 [`Output`] 的 stderr。
///
/// 三个对话框回调都忽略第三个参数 `ExtensionUIDialogOptions`（其中的 abort signal /
/// timeout 不会关闭对话框）。回调类型是 `Send + Sync`，但产生的 future 是 `!Send`，
/// 只能在创建它的 UI 线程上 poll。
pub fn create_project_trust_context(options: ProjectTrustOptions) -> ExtProjectTrustContext {
    let mode = options.mode;
    let has_ui = options.has_ui;

    let select_options = options.clone();
    let select: SelectCallback = Arc::new(move |title, labels, _dialog| {
        let options = select_options.clone();
        Box::pin(async move {
            if !options.has_ui {
                return None;
            }
            if options.mode != AppMode::Interactive {
                return None;
            }
            let entries = labels
                .into_iter()
                .map(|label| StartupSelectorOption {
                    label: label.clone(),
                    value: label,
                })
                .collect();
            show_startup_selector(&options.settings_manager, &title, entries).await
        })
    });

    let confirm_options = options.clone();
    let confirm: ConfirmCallback = Arc::new(move |title, message, _dialog| {
        let options = confirm_options.clone();
        Box::pin(async move {
            if !options.has_ui {
                return false;
            }
            if options.mode != AppMode::Interactive {
                return false;
            }
            show_startup_selector(
                &options.settings_manager,
                &format!("{title}\n{message}"),
                vec![
                    StartupSelectorOption {
                        label: "Yes".to_string(),
                        value: true,
                    },
                    StartupSelectorOption {
                        label: "No".to_string(),
                        value: false,
                    },
                ],
            )
            .await
            .unwrap_or(false)
        })
    });

    let input_options = options.clone();
    let input: InputCallback = Arc::new(move |title, placeholder, _dialog| {
        let options = input_options.clone();
        Box::pin(async move {
            if !options.has_ui {
                return None;
            }
            if options.mode != AppMode::Interactive {
                return None;
            }
            show_startup_input(&options.settings_manager, &title, placeholder.as_deref()).await
        })
    });

    let notify_options = options.clone();
    let notify: Arc<dyn Fn(String, Option<NotifyKind>) + Send + Sync> =
        Arc::new(move |message, kind| {
            if notify_options.mode == AppMode::Interactive {
                // TS：交互模式下由 TUI 处理，`notify` 是 no-op。
                return;
            }
            let style = Style::detect();
            let text = match kind {
                Some(NotifyKind::Error) => style.red(&message),
                Some(NotifyKind::Warning) => style.yellow(&message),
                None | Some(NotifyKind::Info) => style.cyan(&message),
            };
            notify_options.output.write_line_stderr(&text);
        });

    ExtProjectTrustContext {
        cwd: options.cwd,
        mode: extension_mode_for_app_mode(mode),
        has_ui,
        ui: ExtProjectTrustUi {
            select,
            confirm,
            input,
            notify,
        },
    }
}

/// 把 `!Send` 的 [`kiana_ext_api::LocalBoxFuture`] 包装成 `Send`。
///
/// SAFETY: 值只在创建它的 UI 线程（`LocalSet`）上被 poll；`Send` 仅用于满足
/// `async_trait` 的签名。调用方（[`ProjectTrustAdapter`]）从不把 future 交给其它线程。
struct SendLocalFuture<T>(kiana_ext_api::LocalBoxFuture<'static, T>);

// SAFETY: 见上；所有 poll 都发生在创建 future 的同一线程。
unsafe impl<T> Send for SendLocalFuture<T> {}

impl<T> Future for SendLocalFuture<T> {
    type Output = T;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<T> {
        self.0.as_mut().poll(cx)
    }
}

/// 把任意 `!Send` future 包装成 `Send`（只在 UI 线程 poll）。
struct SendFuture<F>(F);

// SAFETY: 与 [`SendLocalFuture`] 相同：future 只在创建它的 UI 线程上被 poll。
unsafe impl<F> Send for SendFuture<F> {}

impl<F: Future + Unpin> Future for SendFuture<F> {
    type Output = F::Output;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<F::Output> {
        Pin::new(&mut self.0).poll(cx)
    }
}

/// kiana-config 侧 [`ProjectTrustUi`](ConfigProjectTrustUi) 的适配器：`select` 转发到扩展视图。
///
/// kiana-config 的 trait 只有 `select`；`confirm`/`input`/`notify` 不参与它的
/// “询问是否信任”的决策路径，因此不在此暴露。
pub struct ProjectTrustAdapter {
    ui: ExtProjectTrustUi,
}

impl ProjectTrustAdapter {
    /// 从扩展视图构造（克隆 `Arc`，与扩展侧共享同一组回调）。
    pub fn new(ui: ExtProjectTrustUi) -> Self {
        Self { ui }
    }
}

#[async_trait]
impl ConfigProjectTrustUi for ProjectTrustAdapter {
    async fn select(&self, title: &str, options: Vec<String>) -> Option<String> {
        // `SendLocalFuture` 把扩展回调的 `!Send` future 适配成 trait 要求的 `Send` future；
        // 实际 poll 仍发生在当前 UI 线程。
        SendLocalFuture((self.ui.select)(title.to_string(), options, None)).await
    }
}

/// 同一组选项生成的两种项目信任上下文视图。
pub struct ProjectTrustContextViews {
    /// 扩展处理器使用的上下文（`emit_project_trust_event` 的 `ctx`）。
    pub extension: ExtProjectTrustContext,
    /// `resolve_project_trusted` 使用的上下文。
    pub config: ConfigProjectTrustContext,
}

/// 从同一组选项同时构造扩展视图与 kiana-config 视图。
///
/// `createCommandSettingsManager`（package-manager-cli）与 app 的
/// `ResourceLoaderReloadOptions.resolve_project_trust` 都从这里取。
///
/// 两个视图必须来自同一份选项，否则扩展看到的 `mode`/`has_ui` 会与决策逻辑不一致。
/// `config.ui` 恒为 `Some(ProjectTrustAdapter)`（即使 `has_ui == false`）；kiana-config
/// 在需要询问用户前先检查 `has_ui`，为 false 时不会走到 `select`。
pub fn create_project_trust_context_views(
    options: ProjectTrustOptions,
) -> ProjectTrustContextViews {
    let extension = create_project_trust_context(options.clone());
    let config = ConfigProjectTrustContext {
        has_ui: options.has_ui,
        ui: Some(Arc::new(ProjectTrustAdapter::new(extension.ui.clone()))),
    };
    ProjectTrustContextViews { extension, config }
}

/// [`ProjectTrustEmitter`] 的适配器：把 [`LoadExtensionsResult`] + 扩展视图打包成
/// kiana-config 决策逻辑需要的发送器。
///
/// TS: `resolveProjectTrusted({ extensionsResult, projectTrustContext, ... })` 内部直接调用
/// `emitProjectTrustEvent(extensionsResult, {type:"project_trust", cwd}, context)`。
///
/// 行为：
/// - `extensions` 为空时直接返回默认结果，不触碰 JS 运行时；
/// - 否则逐个分发给扩展的 `project_trust` 处理器，扩展返回的 `trusted` 只区分是否
///   [`ProjectTrustEventDecision::Yes`]（映射成 `"yes"`/`"no"`），`remember` 原样透传；
/// - 每个扩展的错误转成 [`ProjectTrustEventError`] 列表，由 kiana-config 逐条回调
///   `on_extension_error`（结果与错误可以同时非空）。
pub struct ProjectTrustEmitterAdapter {
    result: LoadExtensionsResult,
    context: ExtProjectTrustContext,
}

// SAFETY: kiana-config 的 [`ProjectTrustEmitter`] 要求 `Send + Sync`，但
// [`LoadExtensionsResult`] 持有 rquickjs 的单线程值（`Rc`/裸指针）。与 TS 一样，项目信任
// 只在同一个 UI 线程（`LocalSet`）上评估：adapter 在调用线程创建、在调用线程被 poll，
// 从不跨线程移动。`Send + Sync` 只是 trait 的签名要求（同 `kiana-interactive` 的
// `SendOverlayOptions`）。
unsafe impl Send for ProjectTrustEmitterAdapter {}
// SAFETY: 见上。
unsafe impl Sync for ProjectTrustEmitterAdapter {}

impl ProjectTrustEmitterAdapter {
    /// 构造：`result` 是预信任阶段加载的扩展，`context` 来自
    /// [`create_project_trust_context_views`] 的 `extension`。
    pub fn new(result: LoadExtensionsResult, context: ExtProjectTrustContext) -> Self {
        Self { result, context }
    }
}

#[async_trait]
impl ProjectTrustEmitter for ProjectTrustEmitterAdapter {
    async fn emit_project_trust(
        &self,
        event: kiana_config::trust::project_trust::ProjectTrustEvent,
    ) -> EmitProjectTrustResult {
        // TS 的 `emitProjectTrustEvent` 遍历扩展列表：没有扩展时不会碰运行时。
        if self.result.extensions.is_empty() {
            return EmitProjectTrustResult::default();
        }
        let outcome = SendFuture(Box::pin(
            kiana_extensions::project_trust::emit_project_trust_event(
                &self.result,
                kiana_ext_api::events::ProjectTrustEvent {
                    event_type: ProjectTrustEventType::ProjectTrust,
                    cwd: event.cwd,
                },
                self.context.clone(),
            ),
        ))
        .await;
        EmitProjectTrustResult {
            result: outcome.result.map(|result| ProjectTrustEventOutcome {
                // `resolve_project_trusted` 只比较 `trusted == "yes"`。
                trusted: if result.trusted == ProjectTrustEventDecision::Yes {
                    "yes".to_string()
                } else {
                    "no".to_string()
                },
                remember: result.remember,
            }),
            errors: outcome
                .errors
                .into_iter()
                .map(|error| ProjectTrustEventError {
                    extension_path: error.extension_path,
                    error: error.error,
                })
                .collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_app_modes_to_extension_modes() {
        assert_eq!(
            extension_mode_for_app_mode(AppMode::Interactive),
            ExtensionMode::Tui
        );
        assert_eq!(
            extension_mode_for_app_mode(AppMode::Print),
            ExtensionMode::Print
        );
        assert_eq!(
            extension_mode_for_app_mode(AppMode::Json),
            ExtensionMode::Json
        );
        assert_eq!(
            extension_mode_for_app_mode(AppMode::Rpc),
            ExtensionMode::Rpc
        );
    }

    #[test]
    fn non_interactive_select_confirm_input_short_circuit() {
        let settings_manager = SettingsManager::create_with_options(
            "/tmp",
            "/tmp/does-not-exist-agent",
            kiana_config::settings::SettingsManagerCreateOptions {
                project_trusted: Some(false),
            },
        );
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("runtime");
        let local = tokio::task::LocalSet::new();
        local.block_on(&runtime, async move {
            let context = create_project_trust_context(ProjectTrustOptions {
                cwd: "/tmp".to_string(),
                mode: AppMode::Print,
                settings_manager,
                has_ui: false,
                output: Output::capture(),
            });
            assert_eq!(context.mode, ExtensionMode::Print);
            assert!(!context.has_ui);
            assert_eq!(
                (context.ui.select)("Title".to_string(), vec!["A".to_string()], None).await,
                None
            );
            assert!(!(context.ui.confirm)("Title".to_string(), "Message".to_string(), None).await);
            assert_eq!(
                (context.ui.input)("Title".to_string(), None, None).await,
                None
            );
        });
    }

    #[test]
    fn non_interactive_notify_writes_to_stderr() {
        let settings_manager = SettingsManager::create_with_options(
            "/tmp",
            "/tmp/does-not-exist-agent",
            kiana_config::settings::SettingsManagerCreateOptions {
                project_trusted: Some(false),
            },
        );
        let output = Output::capture();
        let context = create_project_trust_context(ProjectTrustOptions {
            cwd: "/tmp".to_string(),
            mode: AppMode::Json,
            settings_manager,
            has_ui: false,
            output: output.clone(),
        });
        (context.ui.notify)("hello".to_string(), Some(NotifyKind::Error));
        (context.ui.notify)("warn".to_string(), Some(NotifyKind::Warning));
        (context.ui.notify)("info".to_string(), None);
        let stderr = output.captured_stderr();
        assert!(stderr.contains("hello"));
        assert!(stderr.contains("warn"));
        assert!(stderr.contains("info"));
    }

    #[test]
    fn interactive_notify_is_a_no_op() {
        let settings_manager = SettingsManager::create_with_options(
            "/tmp",
            "/tmp/does-not-exist-agent",
            kiana_config::settings::SettingsManagerCreateOptions {
                project_trusted: Some(false),
            },
        );
        let output = Output::capture();
        let context = create_project_trust_context(ProjectTrustOptions {
            cwd: "/tmp".to_string(),
            mode: AppMode::Interactive,
            settings_manager,
            has_ui: true,
            output: output.clone(),
        });
        (context.ui.notify)("hello".to_string(), Some(NotifyKind::Error));
        assert_eq!(output.captured_stderr(), "");
    }

    #[test]
    fn views_share_has_ui_and_mode() {
        let settings_manager = SettingsManager::create_with_options(
            "/tmp",
            "/tmp/does-not-exist-agent",
            kiana_config::settings::SettingsManagerCreateOptions {
                project_trusted: Some(false),
            },
        );
        let views = create_project_trust_context_views(ProjectTrustOptions {
            cwd: "/work".to_string(),
            mode: AppMode::Rpc,
            settings_manager,
            has_ui: false,
            output: Output::capture(),
        });
        assert_eq!(views.extension.cwd, "/work");
        assert_eq!(views.extension.mode, ExtensionMode::Rpc);
        assert!(!views.config.has_ui);
        assert!(views.config.ui.is_some());
    }
}
