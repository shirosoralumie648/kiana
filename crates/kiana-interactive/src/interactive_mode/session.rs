//! `interactive-mode.ts` 的会话切换部分（§2.50.3 步骤 11 / §2.50.6）。
//!
//! 实现 `rebindCurrentSession`、`bindCurrentSessionExtensions`、`applyRuntimeSettings`
//! 以及扩展命令上下文需要的崩溃 / 信任 / 恢复辅助。
//!
//! # 为什么需要这个模块
//!
//! 会话替换（`/new`、`/resume`、fork，均由 `AgentSessionRuntime` 完成）发生后，交互模式
//! 必须换上一批"指向新会话"的运行期状态：事件订阅、设置驱动的 UI 开关、扩展绑定、主题
//! 注册表等。这些工作分散在多个子模块，本文件把它们编排成一个 rebind 流程，供两处调用：
//!
//! 1. `InteractiveMode::init()` 的步骤 10（`render_before_bind = false`）：先绑定扩展
//!    （loaded resources 显示在初始消息之前，见父模块 `init()` 的步骤注释），再订阅事件；
//! 2. 构造器挂到 `AgentSessionRuntime` 的 `rebind_session` 回调（会话替换后触发，
//!    `render_before_bind = true`）：先重建聊天并订阅事件，再绑定扩展。
//!
//! # `CURRENT_MODE` 线程局部
//!
//! 扩展回调签名要求 `Send + Sync`，无法捕获 `Rc<RefCell<..>>`。本仓库的调用约定是：
//! 扩展引擎与会话运行在同一个 LocalSet 线程上（对应 TS 的单线程 UI 语义），回调在该
//! 线程被同步调用时可以从线程局部槽 `CURRENT_MODE` 取回当前模式（`current_mode()`）。
//! `bind_current_session_extensions` 在绑定前通过 `install_current_mode()` 发布模式；
//! `reload.rs` 的 `before_session_start` 回调也复用同一机制。
//!
//! # 扩展命令上下文
//!
//! `bind_current_session_extensions` 构造一组闭包（`new_session` / `fork` /
//! `navigate_tree` / `switch_session` / `reload` / `wait_for_idle` /
//! `abort_handler` / `shutdown_handler` / `on_error`），经 `AgentSession::bind_extensions`
//! 存入会话。闭包内部通过 `current_mode()` 回到交互模式；模式已释放（升级 `Weak` 失败）
//! 时动作返回 `cancelled: true` 或直接忽略。
//!
//! # 崩溃报告
//!
//! `handle_fatal_runtime_error`（以 `!` 结束进程）编排本文件的三个辅助：
//! `get_crash_extension_hint` 从调用栈判定可疑扩展、`record_crash` 把崩溃追加进 crash log
//! （写失败只省略"如何报告"提示，不改变流程）、`crash_report_instructions` 生成报告指引文案。
//! 另有一组"是否值得提示 `/bug`"的辅助（`maybe_suggest_bug_report` /
//! `maybe_show_install_change_warning`），用于助手消息以错误结束的普通失败场景。
//!
//! # 注意事项
//!
//! * 本文件的 `InteractiveMode` 方法按 UI 线程语义编写（`Rc`/`RefCell` 非 `Send`）；
//!   借用纪律见父模块 `//!`（绝不在 `.await` 期间持有 `inner.borrow()`）。扩展回调通常
//!   也在同一线程被调用，但这并非编译期保证：`current_mode()` 在其它线程返回 `None`，
//!   调用方按"模式不可用"处理（`extension_ui` 因此另有跨线程快照回退）。
//! * `rebind_current_session` 在绑定扩展之前或之后订阅 agent 事件，两条路径都只调用一次
//!   `subscribe_to_agent`，且入口处先 `take()` 并调用旧订阅的取消函数；因此同一时刻只有
//!   **一个** agent 事件订阅。重复订阅会让同一事件被渲染两次（见
//!   `tests/interactive_mode_reload.rs` 的
//!   `replacement_rebind_keeps_exactly_one_agent_subscription`）；绑定期间若会话又被替换，
//!   本次调用提前返回，由替换触发的下一次 rebind 完成订阅。
//! * 文件末尾 `#[doc(hidden)]` 的方法与 `current_mode_for_test` 是集成测试接缝，
//!   不构成公开 API。

use std::rc::Rc;
use std::sync::Arc;

use kiana_core::config::{APP_NAME, VERSION};
use kiana_ext_api::LocalBoxFuture;
use kiana_ext_api::events::{ProjectTrustContext, ProjectTrustUi};
use kiana_ext_api::types::{ExtensionCommandContextActions, ExtensionMode, NewSessionResult};
use kiana_resources::crash_log::{
    CrashInput, CrashKind, ExtensionStackMetadata, crash_log_path, find_extension_stack_matches,
    record_crash,
};
use kiana_session::agent_session::{
    AgentSession, ExtensionBindings, NavigateTreeOptions as SessionNavigateTreeOptions,
};
use kiana_session::agent_session_runtime::{
    AgentSessionRuntimeError, ForkSessionOptions, NewSessionOptionsInput, SwitchSessionOptions,
};
use kiana_session_core::session_cwd::format_missing_session_cwd_prompt;
use kiana_theme::theme_tokens::ThemeColor;
use kiana_ui_kit::themed_text::ThemedText;

use super::{InteractiveMode, InteractiveModeInner};

/// 用当前主题的前景色渲染文本；主题未初始化时 panic（对应 TS 在同样位置的调用错误）。
fn fg(color: ThemeColor, text: &str) -> String {
    kiana_theme::THEME
        .fg(color, text)
        .expect("theme must be initialized before rendering (TS throws here)")
}

/// 在 UI 线程上对弱引用执行一段逻辑的便捷类型。
fn upgrade_mode(
    weak: &std::rc::Weak<std::cell::RefCell<InteractiveModeInner>>,
) -> Option<InteractiveMode> {
    weak.upgrade().map(|inner| InteractiveMode { inner })
}

thread_local! {
    /// 当前 UI 线程的交互模式（扩展回调签名要求 `Send + Sync`，无法捕获 `Weak<RefCell>`）。
    ///
    /// 扩展引擎与会话都运行在同一个 LocalSet 线程上（TS 的单线程 UI 语义）；回调在该线程
    /// 同步调用时从这里取回模式。
    static CURRENT_MODE: std::cell::RefCell<Option<std::rc::Weak<std::cell::RefCell<InteractiveModeInner>>>> =
        const { std::cell::RefCell::new(None) };
}

/// 当前线程的交互模式（非 UI 线程返回 `None`）。
pub(crate) fn current_mode() -> Option<InteractiveMode> {
    CURRENT_MODE.with(|slot| {
        slot.borrow()
            .as_ref()
            .and_then(std::rc::Weak::upgrade)
            .map(|inner| InteractiveMode { inner })
    })
}

/// 当前 UI 线程的交互模式（`doc(hidden)` 测试接缝：扩展处理器在 reload/新会话期间读取 UI 状态）。
#[doc(hidden)]
pub fn current_mode_for_test() -> Option<InteractiveMode> {
    current_mode()
}

/// 安装 / 刷新线程局部里的当前模式。
///
/// 每次 `bind_current_session_extensions` 都重新发布一次（会话替换前后是同一个
/// `InteractiveModeInner`，这里只是覆盖成同一个 `Weak`）。槽里存 `Weak` 而非 `Rc`，
/// 因此不会延长模式寿命；模式被释放后 `current_mode()` 升级失败返回 `None`，
/// 不存在需要显式清理的过期强引用。
fn install_current_mode(mode: &InteractiveMode) {
    CURRENT_MODE.with(|slot| {
        *slot.borrow_mut() = Some(mode.downgrade());
    });
}

impl InteractiveMode {
    /// `applyRuntimeSettings()`。
    ///
    /// 把当前会话 `SettingsManager` 里的设置逐项应用到全局状态与 UI 组件。生产调用点只有
    /// 两个：`rebind_current_session`（换绑会话时）与 `reload` 完成后（`reload.rs`）。
    ///
    /// 覆盖范围：终端能力覆盖 → PNG 转码器 → HTTP idle 超时（设置值非法时跳过，
    /// dispatcher 保持原配置；配置失败本身也被忽略）→ 全屏滚动条 → 全屏模式下的
    /// copy_on_select 与 wheel_scroll_lines → footer（换绑会话、
    /// auto-compact 开关）与 footer 的 cwd → thinking 块隐藏、输出内边距 → 硬件光标与
    /// `clear_on_shrink`（`clear_on_shrink` 为假且没有活动状态指示器时清空状态区）→
    /// 编辑器内边距与补全可见数。新增设置项需要在这里显式应用，否则只在下次 rebind/reload
    /// 时生效。
    pub(crate) fn apply_runtime_settings(&mut self) {
        let settings_manager = self.settings_manager();
        kiana_tui::set_capability_overrides(settings_manager.get_terminal_capability_overrides());
        self.ensure_png_transcoder();
        if let Ok(timeout) = settings_manager.get_http_idle_timeout_ms() {
            let _ = kiana_core::http_dispatcher::configure_http_dispatcher(timeout as i64);
        }
        self.apply_fullscreen_scrollbar_setting();

        {
            let renderer = Rc::clone(&self.inner.borrow().renderer);
            let mut renderer = renderer.borrow_mut();
            if renderer.mode() == kiana_tui::TuiMode::Fullscreen {
                renderer.set_copy_on_select(settings_manager.get_fullscreen_copy_on_select());
                renderer
                    .set_wheel_scroll_lines(settings_manager.get_fullscreen_wheel_scroll_lines());
            }
        }

        let session = self.session();
        let cwd = session.session_manager().get_cwd().to_string();
        {
            let footer = Rc::clone(&self.inner.borrow().footer);
            footer.borrow_mut().set_session(session.clone());
            footer
                .borrow_mut()
                .set_auto_compact_enabled(session.auto_compaction_enabled());
        }
        self.inner.borrow().footer_data_provider.set_cwd(&cwd);
        {
            let mut inner = self.inner.borrow_mut();
            // `hide_thinking_block` / `output_pad` 是渲染期读取的缓存值，必须与设置保持
            // 一致，否则消息渲染沿用旧设置。`output_pad` 是 `i64`，负值归一为 0
            // （`usize::try_from` 失败即负数）。
            inner.hide_thinking_block = settings_manager.get_hide_thinking_block();
            inner.output_pad = usize::try_from(settings_manager.get_output_pad()).unwrap_or(0);
        }
        {
            let mut inner = self.inner.borrow_mut();
            inner
                .ui
                .set_show_hardware_cursor(settings_manager.get_show_hardware_cursor());
            let clear_on_shrink = settings_manager.get_clear_on_shrink();
            inner.ui.set_clear_on_shrink(clear_on_shrink);
            if !clear_on_shrink && inner.active_status_indicator.is_none() {
                let status = Rc::clone(&inner.status_container);
                status.borrow_mut().clear();
            }
        }

        let editor_padding_x = settings_manager.get_editor_padding_x();
        let autocomplete_max_visible = settings_manager.get_autocomplete_max_visible();
        {
            let inner = self.inner.borrow();
            inner
                .default_editor
                .borrow_mut()
                .set_padding_x(editor_padding_x);
            inner
                .default_editor
                .borrow_mut()
                .set_autocomplete_max_visible(autocomplete_max_visible);
            // 仅当活动编辑器不是默认编辑器时才走 `editor` 槽更新：`ActiveEditor::Default`
            // 的句柄就是 `default_editor` 同一实例，已在上面更新过（对应 TS 的
            // `this.editor !== this.defaultEditor` 判断）。
            if !matches!(inner.editor, super::ActiveEditor::Default { .. }) {
                inner
                    .editor
                    .set_padding_x(editor_padding_x.max(0.0) as usize);
                inner
                    .editor
                    .set_autocomplete_max_visible(autocomplete_max_visible.max(0.0) as usize);
            }
        }

        // `ui.locale` 的 translator 是构造期缓存。外部编辑 settings.json 后 `/reload`
        // 会重读设置；这里与其它设置项一起重建，否则界面文案会滞后到下次重启
        // （settings 面板内的切换走 `on_ui_locale_change`，已即时生效）。
        let ui_locale = settings_manager.get_ui_locale();
        self.apply_ui_locale(&ui_locale);
    }

    /// `applyFullscreenScrollbarSetting()`。
    ///
    /// 把设置里的 `fullscreenScrollbar` 同步到聊天 transcript 的滚动视图。该视图由
    /// `init()` 的步骤 3（chat viewport 装配）写入；`init()` 之前或视图不可用时为
    /// `None`，本函数什么也不做。
    pub(crate) fn apply_fullscreen_scrollbar_setting(&self) {
        let scrollbar = self.settings_manager().get_fullscreen_scrollbar();
        if let Some(transcript) = self.inner.borrow().transcript_scroll_view.as_ref() {
            transcript.borrow().set_scrollbar(scrollbar);
        }
    }

    /// `rebindCurrentSession(options)`。
    ///
    /// 会话替换后把交互模式的运行期状态换绑到新会话。阶段：
    ///
    /// 1. 退订旧会话的 agent 事件并重置终端进度（`program_status.reset()`）；
    /// 2. 重新应用设置（新会话有自己的 `SettingsManager`）；
    /// 3. `render_before_bind = true` 时先重建聊天再订阅事件，最后才绑定扩展（来自
    ///    `rebind_session` 回调）。两步都必须先于绑定：绑定会 emit `session_start`，
    ///    聊天必须先重建好，否则处理器写入的输出会被随后的重建清掉（与 `reload.rs` 的
    ///    `#5943` 同一顺序要求）；订阅也必须就位，否则处理器发出的消息无法进入渲染
    ///    （`tests/interactive_mode_reload.rs` 的
    ///    `new_session_subscribes_before_session_start_sends_messages`）。
    ///    `false` 时这两步推迟到绑定之后（来自 `init()`：loaded resources 要显示在初始
    ///    消息之前）；
    /// 4. 绑定扩展；
    /// 5. 若绑定期间会话又被替换（`same_session` 为假），**到此为止**：订阅、渲染都由
    ///    替换触发的下一次 rebind 负责，本调用不再做任何事；
    /// 6. `render_before_bind = false` 时补订阅事件，最后刷新可用 provider 计数、
    ///    编辑器边框颜色与终端标题。
    pub(crate) async fn rebind_current_session(&mut self, render_before_bind: bool) {
        let session = self.session();

        if let Some(task) = self.inner.borrow_mut().event_task.take() {
            // 先停掉旧 drain 任务，再取消订阅：channel 里可能还排着旧会话的事件，
            // 否则它们会在新会话绑定后被 handle_event 应用。
            task.abort();
        }
        let unsubscribe = self.inner.borrow_mut().unsubscribe.take();
        if let Some(unsubscribe) = unsubscribe {
            unsubscribe();
        }
        self.inner.borrow_mut().program_status.reset();
        self.apply_runtime_settings();

        if render_before_bind {
            self.render_current_session_state();
            self.subscribe_to_agent();
        }

        self.bind_current_session_extensions().await;

        // 绑定期是 `await` 点，扩展回调（如 `new_session`）可能又触发了一次会话替换；
        // 那次替换会再走一遍 rebind，本调用退场，避免对旧状态做多余的订阅/刷新。
        if !same_session(&self.session(), &session) {
            return;
        }

        if !render_before_bind {
            self.subscribe_to_agent();
        }

        self.update_available_provider_count();
        self.update_editor_border_color();
        self.update_terminal_title();
    }

    /// `bindCurrentSessionExtensions()`。
    ///
    /// 构造扩展命令上下文的全部回调并绑定到当前会话，随后让会话发出
    /// `session_start` 事件（`bind_extensions` 内部）、注册会话加载到的主题、装配补全
    /// 与扩展快捷键、显示本次加载的资源与启动通知。
    ///
    /// 注意 `create_extension_ui_context` 会新建通道与 drain 任务、覆盖线程局部的
    /// `EVENT_SENDER` 并登记快照；每次 rebind 都会产生新的上下文，跨上下文的请求没有
    /// 全局顺序（详见 `extension_ui` 的模块文档）。
    ///
    /// # 回调的共同机制
    ///
    /// 每个闭包都不捕获 `self`（签名要求 `Send + Sync`），而是在调用时经
    /// `current_mode()` 从线程局部取回模式；`Weak` 升级失败说明模式已释放，动作按
    /// "已取消" 或 no-op 处理。需要**替换会话**的动作（`new_session` / `fork` /
    /// `switch_session`）走 `AgentSessionRuntime` 的统一入口，出错时经
    /// `handle_fatal_runtime_error` 直接退出进程（与 TS 把异常抛给顶层一致）；
    /// `navigate_tree` 不替换会话，见下。
    ///
    /// # 各回调的行为
    ///
    /// * `abort_handler`：接管会话的 abort。会话侧包装的 `abort` 闭包（`agent_session.rs`）
    ///   在已绑定本处理器时只调用它、不直接 `request_abort`；这里用
    ///   `restore_queued_messages_to_editor(true, None)` 恢复队列消息到编辑器，其中
    ///   `abort = true` 会再异步调用 `session.abort()` 真正中止（该调用走核心的 abort，
    ///   不经过这个处理器，因此不会递归）。
    /// * `wait_for_idle`：等待当前会话空闲；扩展侧对应 `ctx.waitForIdle()`。
    /// * `new_session` / `fork` / `switch_session`：委托给 runtime 的
    ///   `new_session` / `fork` / `switch_session`；fork 成功后把被 fork 的用户消息文本
    ///   放入编辑器（`selected_text` 仅在 `position: "before"` 时有值；`"at"` fork 时
    ///   编辑器被置空）；`switch_session` 直接复用 `handle_resume_session`（含
    ///   missing-cwd 的二次询问流程）。
    /// * `navigate_tree`：会话内树导航不走 runtime（同一会话，不替换）：成功后清空聊天、
    ///   重渲染初始消息、把目标用户消息的文本放回（仅当编辑器为空），再异步冲刷压缩队列。
    ///   流式或压缩进行中导航会话层直接报错（`TreeWhileStreaming` / `TreeWhileCompacting`）。
    /// * `reload`：等同 `/reload` 命令（`handle_reload_command`）。
    /// * `shutdown_handler`：只置 `shutdown_requested` 标志；会话空闲时立即排一次
    ///   `shutdown(false)`，否则由 `AgentSettled` 事件处理路径轮询该标志后再关停
    ///   （见 `lifecycle.rs` 的 `check_shutdown_requested` 与 `events.rs` 的事件分派）。
    /// * `on_error`：扩展运行期错误只显示，不中断进程（区别于 `handle_fatal_runtime_error`）。
    pub(crate) async fn bind_current_session_extensions(&mut self) {
        install_current_mode(self);
        let ui_context = self.create_extension_ui_context();
        let abort_handler: Arc<dyn Fn() + Send + Sync> = Arc::new(|| {
            if let Some(mut mode) = current_mode() {
                mode.restore_queued_messages_to_editor(true, None);
            }
        });

        let wait_for_idle = Arc::new(|| -> LocalBoxFuture<'static, ()> {
            Box::pin(async move {
                if let Some(mode) = current_mode() {
                    let session = mode.session();
                    session.wait_for_idle().await;
                }
            })
        });

        let new_session = Arc::new(|options: Option<kiana_ext_api::types::NewSessionOptions>| {
            Box::pin(async move {
                let Some(mut mode) = current_mode() else {
                    return NewSessionResult { cancelled: true };
                };
                mode.clear_status_indicator(None);
                let runtime = Rc::clone(&mode.inner.borrow().runtime_host);
                let options = options.map(|options| NewSessionOptionsInput {
                    parent_session: options.parent_session,
                    setup: options.setup,
                    with_session: options.with_session.map(|callback| {
                        callback as kiana_session::agent_session_runtime::ReplacedSessionCallback
                    }),
                });
                match runtime.new_session(options).await {
                    Ok(result) => NewSessionResult {
                        cancelled: result.cancelled,
                    },
                    Err(error) => {
                        mode.handle_fatal_runtime_error(
                            "Failed to create session",
                            &error.to_string(),
                            None,
                        )
                        .await
                    }
                }
            }) as LocalBoxFuture<'static, NewSessionResult>
        });

        let fork = Arc::new(
            |entry_id: String, options: Option<kiana_ext_api::types::ForkOptions>| {
                Box::pin(async move {
                    let Some(mut mode) = current_mode() else {
                        return NewSessionResult { cancelled: true };
                    };
                    let runtime = Rc::clone(&mode.inner.borrow().runtime_host);
                    let options = options.map(|options| ForkSessionOptions {
                        position: options.position,
                        with_session: options.with_session.map(|callback| {
                            callback
                                as kiana_session::agent_session_runtime::ReplacedSessionCallback
                        }),
                    });
                    match runtime.fork(&entry_id, options).await {
                        Ok(result) => {
                            if !result.cancelled {
                                mode.inner
                                    .borrow()
                                    .editor
                                    .set_text(result.selected_text.as_deref().unwrap_or(""));
                                mode.show_status("Forked to new session");
                            }
                            NewSessionResult {
                                cancelled: result.cancelled,
                            }
                        }
                        Err(error) => {
                            mode.handle_fatal_runtime_error(
                                "Failed to fork session",
                                &error.to_string(),
                                None,
                            )
                            .await
                        }
                    }
                }) as LocalBoxFuture<'static, NewSessionResult>
            },
        );

        let navigate_tree = Arc::new(
            |target_id: String, options: Option<kiana_ext_api::types::NavigateTreeOptions>| {
                Box::pin(async move {
                    let Some(mut mode) = current_mode() else {
                        return NewSessionResult { cancelled: true };
                    };
                    let session = mode.session();
                    let options = options.unwrap_or_default();
                    let result = session
                        .navigate_tree(
                            target_id,
                            SessionNavigateTreeOptions {
                                summarize: options.summarize,
                                custom_instructions: options.custom_instructions,
                                replace_instructions: options.replace_instructions,
                                label: options.label,
                            },
                        )
                        .await;
                    match result {
                        Ok(result) if result.cancelled => NewSessionResult { cancelled: true },
                        Ok(result) => {
                            let chat = Rc::clone(&mode.inner.borrow().chat_container);
                            chat.borrow_mut().clear();
                            mode.render_initial_messages();
                            if let Some(editor_text) = result.editor_text
                                && mode.inner.borrow().editor.get_text().trim().is_empty()
                            {
                                mode.inner.borrow().editor.set_text(&editor_text);
                            }
                            mode.show_status("Navigated to selected point");
                            let weak = mode.downgrade();
                            tokio::task::spawn_local(async move {
                                if let Some(mut mode) = upgrade_mode(&weak) {
                                    mode.flush_compaction_queue(false).await;
                                }
                            });
                            NewSessionResult { cancelled: false }
                        }
                        Err(error) => {
                            mode.handle_fatal_runtime_error(
                                "Failed to navigate session",
                                &error.to_string(),
                                None,
                            )
                            .await
                        }
                    }
                }) as LocalBoxFuture<'static, NewSessionResult>
            },
        );

        let switch_session = Arc::new(
            |session_path: String, options: Option<kiana_ext_api::types::SwitchSessionOptions>| {
                Box::pin(async move {
                    let Some(mut mode) = current_mode() else {
                        return NewSessionResult { cancelled: true };
                    };
                    mode.handle_resume_session(session_path, options).await
                }) as LocalBoxFuture<'static, NewSessionResult>
            },
        );

        let reload = Arc::new(|| -> LocalBoxFuture<'static, ()> {
            Box::pin(async move {
                if let Some(mut mode) = current_mode() {
                    mode.handle_reload_command().await;
                }
            })
        });

        let command_context_actions = ExtensionCommandContextActions {
            wait_for_idle,
            new_session,
            fork,
            navigate_tree,
            switch_session,
            reload,
        };

        let shutdown_handler: kiana_extensions::ShutdownHandler = Arc::new(|| {
            let Some(mode) = current_mode() else {
                return;
            };
            // 只置标志不直接关停：若会话正在忙，立即 shutdown 会打断进行中的请求；
            // AgentSettled 事件路径（events.rs → lifecycle.rs 的 check_shutdown_requested）
            // 会在回合结束后兜底执行。空闲则可以立即排定关停。
            mode.inner.borrow_mut().shutdown_requested = true;
            let idle = mode.session().is_idle();
            if idle {
                let weak = mode.downgrade();
                tokio::task::spawn_local(async move {
                    if let Some(mut mode) = upgrade_mode(&weak) {
                        mode.shutdown(false).await;
                    }
                });
            }
        });

        let on_error: kiana_extensions::ExtensionErrorListener =
            Arc::new(|error: kiana_ext_api::types::ExtensionError| {
                if let Some(mut mode) = current_mode() {
                    mode.show_extension_error(
                        &error.extension_path,
                        &error.error,
                        error.stack.as_deref(),
                    );
                }
            });

        let session = self.session();
        // 绑定只是把各回调写进会话状态；随后 `bind_extensions` 会 emit 并 await
        // `session_start` 事件，扩展处理器可能立刻产生聊天输出。因此本函数对聊天区而言
        // 不是"纯准备"步骤：`rebind_current_session` 用它之前的重建（`true`）或调用方
        // `init()` 随后的 `render_initial_messages`（`false`）来保证顺序。
        session
            .bind_extensions(ExtensionBindings {
                ui_context: Some(ui_context),
                mode: Some(ExtensionMode::Tui),
                command_context_actions: Some(command_context_actions),
                abort_handler: Some(abort_handler),
                shutdown_handler: Some(shutdown_handler),
                on_error: Some(on_error),
            })
            .await;

        let themes = session.resource_loader().get_themes().themes;
        let _ = kiana_theme::set_registered_themes(themes);
        self.setup_autocomplete_provider();

        let extension_runner = session.extension_runner();
        self.setup_extension_shortcuts(extension_runner);
        self.show_loaded_resources(false, true);
        self.show_startup_notices_if_needed();
    }

    /// `handleFatalRuntimeError(prefix, error)`：报告并退出进程（返回 `!`）。
    ///
    /// 执行顺序：显示错误 → 用调用栈判定可疑扩展并在聊天区提示 → 追加 crash 记录、
    /// 成功时再显示"如何报告"提示 → 停主题监听 → 按 Transcript 模式退出 TUI →
    /// 进程以退出码 1 结束。返回类型是 `!`，因此所有调用点（本文件的
    /// `new_session` / `fork` / `navigate_tree` / `switch_session` 回调与
    /// `handle_resume_session`）都不需要再处理错误分支。
    ///
    /// TS 把原始 `Error` 交给 `getCrashExtensionHint` / `recordCrash`（`error.stack`）。
    /// Rust 的运行时错误类型不携带回溯，调用方也没有可透传的栈；这里在报告点捕获一份
    /// `std::backtrace::Backtrace` 作为 `Error.stack` 的等价物（首行与 JS 一样是
    /// `Error: <message>`，供 `findExtensionStackMatches` 跳过）。调用方显式传入 `stack`
    /// 时（`stack` 参数非 `None`）原样使用，不另捕获。
    pub(crate) async fn handle_fatal_runtime_error(
        &mut self,
        prefix: &str,
        message: &str,
        stack: Option<&str>,
    ) -> ! {
        let captured_stack;
        let stack = match stack {
            Some(stack) => Some(stack),
            None => {
                captured_stack = format!(
                    "Error: {message}\n{}",
                    std::backtrace::Backtrace::force_capture()
                );
                Some(captured_stack.as_str())
            }
        };
        self.show_error(&format!("{prefix}: {message}"));
        let extension_hint = self.get_crash_extension_hint(stack);
        if let Some(hint) = extension_hint {
            let output_pad = self.inner.borrow().output_pad;
            let chat = Rc::clone(&self.inner.borrow().chat_container);
            chat.borrow_mut()
                .add_child(Box::new(ThemedText::with_padding(
                    Box::new(move || fg(ThemeColor::Warning, &hint)),
                    output_pad,
                    0,
                )));
        }
        if self.record_crash(CrashKind::FatalError, message, stack) {
            let instructions = self.crash_report_instructions();
            let output_pad = self.inner.borrow().output_pad;
            let chat = Rc::clone(&self.inner.borrow().chat_container);
            chat.borrow_mut()
                .add_child(Box::new(ThemedText::with_padding(
                    Box::new(move || fg(ThemeColor::Muted, &instructions)),
                    output_pad,
                    0,
                )));
        }
        kiana_theme::stop_theme_watcher();
        self.stop_with(kiana_config::settings::FullscreenExitOutput::Transcript);
        std::process::exit(1);
    }

    /// `getCrashExtensionHint(error)`。
    ///
    /// 把当前加载的每个扩展转成栈匹配所需的最小元数据（显示路径 / 解析路径 / 来源），
    /// 交给 `find_extension_stack_matches` 在调用栈里找出现过的扩展，再由
    /// `format_crash_extension_hint` 生成提示文案；没有匹配时返回 `None`。
    pub(crate) fn get_crash_extension_hint(&self, stack: Option<&str>) -> Option<String> {
        let extensions = self.session().resource_loader().get_extensions().extensions;
        let metadata: Vec<ExtensionStackMetadata> = extensions
            .iter()
            .map(ExtensionStackMetadata::from)
            .collect();
        let matches = find_extension_stack_matches(stack, &metadata);
        super::format_crash_extension_hint(Some(&matches))
    }

    /// `recordCrash(kind, error)`。
    ///
    /// 把崩溃记录（时间戳、版本、kind、message、stack、会话文件、cwd）追加到 crash log
    /// 并裁剪到上限。返回写入是否成功：失败时调用方（`handle_fatal_runtime_error`）省略
    /// "如何报告"提示，崩溃处理流程本身不受影响。`error_name` 固定为 `"Error"`，
    /// 与 TS 运行时异常的名字对齐。
    pub(crate) fn record_crash(&self, kind: CrashKind, message: &str, stack: Option<&str>) -> bool {
        let session = self.session();
        let session_file = session.session_file();
        let cwd = session.session_manager().get_cwd().to_string();
        record_crash(
            CrashInput {
                kind,
                message,
                error_name: Some("Error"),
                stack,
                session_file: session_file.as_deref(),
                cwd: &cwd,
            },
            &crash_log_path(None),
        )
        .is_some()
    }

    /// `crashReportInstructions()`。
    ///
    /// 生成"如何报告这次崩溃"的一行指引：会话文件存在（可恢复）时建议先用
    /// `{APP_NAME} -r` 恢复会话再运行 `/bug`，否则建议直接重启后运行 `/bug`。
    pub(crate) fn crash_report_instructions(&self) -> String {
        let resume = if self.session().session_file().is_some() {
            format!("run `{APP_NAME} -r` to resume the session, then")
        } else {
            "start kiana and".to_string()
        };
        format!(
            "To report this crash: {resume} run /bug. The crash details are attached automatically."
        )
    }

    /// `suggestBugReport()`。
    ///
    /// 在聊天区显示一次 `/bug` 提示。`bug_report_hint_shown` 是 `InteractiveModeInner` 的
    /// 字段，因此每个交互模式实例只显示一次，重复调用直接返回。
    pub(crate) fn suggest_bug_report(&mut self) {
        if self.inner.borrow().bug_report_hint_shown {
            return;
        }
        self.inner.borrow_mut().bug_report_hint_shown = true;
        let output_pad = self.inner.borrow().output_pad;
        let line =
            format!("If this looks like a {APP_NAME} bug, /bug sends a report to the developers.");
        let chat = Rc::clone(&self.inner.borrow().chat_container);
        chat.borrow_mut()
            .add_child(Box::new(ThemedText::with_padding(
                Box::new(move || fg(ThemeColor::Muted, &line)),
                output_pad,
                0,
            )));
        self.request_render();
    }

    /// `maybeSuggestBugReport(message)`。
    ///
    /// 助手消息以错误结束时考虑提示 `/bug`。两个前置过滤：可重试的错误不提示
    /// （`should_suggest_bug_report`，含中止/取消类错误）；"安装已变化"警告已覆盖
    /// 同类问题时优先显示该警告并跳过 `/bug` 提示（返回 `true` 表示被它处理）。
    pub(crate) fn maybe_suggest_bug_report(&mut self, message: &kiana_ai::types::AssistantMessage) {
        if !super::should_suggest_bug_report(message) {
            return;
        }
        if self.maybe_show_install_change_warning() {
            return;
        }
        self.suggest_bug_report();
    }

    /// `maybeShowInstallChangeWarning()`：返回 true 表示该错误已经被该提示处理。
    ///
    /// 当检测到安装目录自本进程启动后发生变化（升级或被移除/替换）时，显示一次性警告并
    /// 返回 `true`，调用方因此不再显示同一条错误消息（`maybe_suggest_bug_report` 用它
    /// 短路）：运行中的代码与磁盘安装不一致时，后续错误多半由这一原因导致。
    /// `install_change_warning_shown` 置位后，后续调用直接返回 `true`（仍视为"已处理"，
    /// 不重复显示）；没有检测到变化时返回 `false`，错误按正常路径显示。
    pub(crate) fn maybe_show_install_change_warning(&mut self) -> bool {
        if self.inner.borrow().install_change_warning_shown {
            return true;
        }
        let Some(change) = kiana_core::config::detect_install_change(None) else {
            return false;
        };
        self.inner.borrow_mut().install_change_warning_shown = true;
        let cause = match change {
            kiana_core::config::InstallChange::Updated { version } => format!(
                "{APP_NAME} was updated to {version} while this session was running ({VERSION})"
            ),
            kiana_core::config::InstallChange::Removed => {
                format!(
                    "The {APP_NAME} installation this session runs from was removed or replaced"
                )
            }
        };
        let session = self.session();
        let resume_command = super::format_resume_command(&session);
        let restart = match resume_command {
            Some(resume_command) => {
                format!("Restart with `{resume_command}` to continue this session.")
            }
            None => format!("Restart {APP_NAME}."),
        };
        self.show_warning(&format!(
            "{cause}. Features that load code on demand can fail until restart. {restart}"
        ));
        true
    }

    /// `renderCurrentSessionState()`。
    ///
    /// 清空聊天区、loaded resources、待发消息三个容器与相关的流式/待处理状态
    /// （压缩队列、流式组件、pending tools），再从头重放会话历史（`render_initial_messages`）。
    /// 目前唯一的调用方是 `rebind_current_session` 的 `render_before_bind` 分支；
    /// reload 走的是 `reload.rs` 自己的重建路径（`rebuild_chat_from_messages`）。
    pub(crate) fn render_current_session_state(&mut self) {
        {
            let inner = self.inner.borrow();
            inner.loaded_resources_container.borrow_mut().clear();
            inner.chat_container.borrow_mut().clear();
            inner.pending_messages_container.borrow_mut().clear();
        }
        {
            let mut inner = self.inner.borrow_mut();
            inner.compaction_queued_messages.clear();
            inner.streaming_component = None;
            inner.streaming_message = None;
            inner.pending_tools.clear();
            // 第 12 周渲染核心：乐观消息按会话隔离，切换会话时清空避免串台。
            inner.render_core_host.clear();
        }
        self.render_initial_messages();
    }

    /// `getRegisteredToolDefinition(toolName)`。
    ///
    /// 解析某个工具的渲染器链：先取会话注册表里的工具定义（`get_tool_definition`
    /// 内部会先冲刷待处理的工具刷新），再把它连同内置渲染器作为链基底交给扩展
    /// `tool_renderers` resolver——扩展可以覆盖或包装内置渲染器。没有定义、没有扩展
    /// 渲染器时按各自的 `Option` 语义回退。
    pub(crate) fn get_registered_tool_definition(
        &self,
        tool_name: &str,
    ) -> Option<kiana_ext_api::tool_types::ToolRenderers> {
        let session = self.session();
        let runner = session.extension_runner();
        let definition = session.get_tool_definition(tool_name);
        runner.resolve_tool_renderers(tool_name, &|| {
            kiana_tools::renderers::with_built_in_renderers(
                tool_name,
                definition
                    .as_ref()
                    .map(crate::components::tool_execution::renderers_from_definition),
            )
        })
    }

    /// `createProjectTrustContext(cwd)`。
    ///
    /// 构造项目信任检查用的 UI 上下文：`select` / `confirm` / `input` 全部转发给扩展
    /// UI 上下文（信任弹窗与普通扩展弹窗走同一套对话框），`notify` 直接转发。`mode`
    /// 固定为 `ExtensionMode::Tui`，`has_ui = true`——本函数只在交互模式里被
    /// `handle_resume_session` 的信任上下文工厂调用；非交互路径（如 Print 模式）用
    /// `kiana-coding-agent` 自己的信任上下文，不经过这里。
    pub(crate) fn create_project_trust_context(&mut self, cwd: &str) -> ProjectTrustContext {
        let ui = self.create_extension_ui_context();
        let select_ui = Arc::clone(&ui);
        let select = Arc::new(
            move |title: String,
                  options: Vec<String>,
                  opts: Option<kiana_ext_api::ui::ExtensionUIDialogOptions>| {
                let ui = Arc::clone(&select_ui);
                Box::pin(async move { ui.select(&title, &options, opts).await })
                    as LocalBoxFuture<'static, Option<String>>
            },
        );
        let confirm_ui = Arc::clone(&ui);
        let confirm = Arc::new(
            move |title: String,
                  message: String,
                  opts: Option<kiana_ext_api::ui::ExtensionUIDialogOptions>| {
                let ui = Arc::clone(&confirm_ui);
                Box::pin(async move { ui.confirm(&title, &message, opts).await })
                    as LocalBoxFuture<'static, bool>
            },
        );
        let input_ui = Arc::clone(&ui);
        let input = Arc::new(
            move |title: String,
                  placeholder: Option<String>,
                  opts: Option<kiana_ext_api::ui::ExtensionUIDialogOptions>| {
                let ui = Arc::clone(&input_ui);
                Box::pin(async move { ui.input(&title, placeholder.as_deref(), opts).await })
                    as LocalBoxFuture<'static, Option<String>>
            },
        );
        let notify_ui = Arc::clone(&ui);
        let notify = Arc::new(
            move |message: String, kind: Option<kiana_ext_api::ui::NotifyKind>| {
                notify_ui.notify(&message, kind);
            },
        );
        ProjectTrustContext {
            cwd: cwd.to_string(),
            mode: ExtensionMode::Tui,
            has_ui: true,
            ui: ProjectTrustUi {
                select,
                confirm,
                input,
                notify,
            },
        }
    }

    /// `promptForMissingSessionCwd(error)`。
    ///
    /// 会话文件记录的 cwd 已不存在时弹确认框（Yes/No，消息由
    /// `format_missing_session_cwd_prompt` 生成，含原 cwd 与 fallback cwd）：确认后返回
    /// fallback cwd（作为重试 `switch_session` 的 `cwd_override`），取消返回 `None`。
    async fn prompt_for_missing_session_cwd(
        &mut self,
        error: &kiana_session_core::session_cwd::MissingSessionCwdError,
    ) -> Option<String> {
        let message = format_missing_session_cwd_prompt(&error.issue);
        let confirmed = self
            .show_extension_confirm("Session cwd not found", &message, None)
            .await;
        if confirmed {
            Some(error.issue.fallback_cwd.clone())
        } else {
            None
        }
    }

    /// `handleResumeSession(sessionPath, options)`。
    ///
    /// 恢复指定会话文件。两个调用点：`/resume` 的会话选择器（`commands/session.rs`）
    /// 与扩展命令上下文的 `switch_session` 回调（见 `bind_current_session_extensions`）。
    /// 流程：
    ///
    /// 1. 清状态指示器，用 runtime 的 `switch_session` 替换会话；trust 上下文工厂在
    ///    模式存活时走真实 UI 弹窗，模式已释放时退化为静默默认值（select→`None`、
    ///    confirm→`false`、input→`None`、notify 丢弃），保证不 panic。当信任解析走到
    ///    "询问 select" 分支时，`select` 返回 `None` 会让 `resolve_project_trusted`
    ///    以 `Ok(false)`（不信任）收场（见 `kiana-config/src/trust/project_trust.rs`）。
    /// 2. 被信任/切换回调取消 → 返回 `cancelled: true`；成功 → 显示 "Resumed session"。
    /// 3. `MissingSessionCwd` 错误是**可恢复**的唯一分支：先弹确认框，用户选择继续时
    ///    携带 `cwd_override` 重试一次 switch（工厂与选项同样重建）；取消则返回
    ///    `cancelled: true` 并显示 "Resume cancelled"。
    /// 4. 其余错误调用 `handle_fatal_runtime_error` 退出进程（不会返回到本函数的调用方）。
    ///
    /// 注意两次重试共用同一个 `with_session` 回调：`Option::map` 消耗所有权，所以
    /// 第一次先 `clone()`。
    #[allow(
        clippy::arc_with_non_send_sync,
        reason = "ProjectTrustContextFactory 的回调捕获 Weak<RefCell<..>>（UI 单线程），trait 类型本身不要求 Send/Sync"
    )]
    pub(crate) async fn handle_resume_session(
        &mut self,
        session_path: String,
        options: Option<kiana_ext_api::types::SwitchSessionOptions>,
    ) -> NewSessionResult {
        self.clear_status_indicator(None);
        let runtime = Rc::clone(&self.inner.borrow().runtime_host);
        let with_session = options.and_then(|options| options.with_session);
        let project_trust_weak = self.downgrade();
        let project_trust_factory: kiana_session::agent_session_runtime::ProjectTrustContextFactory =
            Arc::new(move |cwd: &str| {
                let mut context = ProjectTrustContext {
                    cwd: cwd.to_string(),
                    mode: ExtensionMode::Tui,
                    has_ui: true,
                    ui: ProjectTrustUi {
                        select: Arc::new(|_, _, _| Box::pin(async { None })),
                        confirm: Arc::new(|_, _, _| Box::pin(async { false })),
                        input: Arc::new(|_, _, _| Box::pin(async { None })),
                        notify: Arc::new(|_, _| {}),
                    },
                };
                if let Some(mut mode) = upgrade_mode(&project_trust_weak) {
                    context = mode.create_project_trust_context(cwd);
                }
                context
            });
        let result = runtime
            .switch_session(
                &session_path,
                Some(SwitchSessionOptions {
                    cwd_override: None,
                    with_session: with_session.clone().map(|callback| {
                        callback as kiana_session::agent_session_runtime::ReplacedSessionCallback
                    }),
                    project_trust_context_factory: Some(project_trust_factory),
                }),
            )
            .await;
        match result {
            Ok(result) if result.cancelled => NewSessionResult { cancelled: true },
            Ok(_) => {
                self.show_status("Resumed session");
                NewSessionResult { cancelled: false }
            }
            Err(AgentSessionRuntimeError::MissingSessionCwd(error)) => {
                let selected_cwd = self.prompt_for_missing_session_cwd(&error).await;
                let Some(selected_cwd) = selected_cwd else {
                    self.show_status("Resume cancelled");
                    return NewSessionResult { cancelled: true };
                };
                let project_trust_weak = self.downgrade();
                let project_trust_factory: kiana_session::agent_session_runtime::ProjectTrustContextFactory =
                    Arc::new(move |cwd: &str| {
                        let mut context = ProjectTrustContext {
                            cwd: cwd.to_string(),
                            mode: ExtensionMode::Tui,
                            has_ui: true,
                            ui: ProjectTrustUi {
                                select: Arc::new(|_, _, _| Box::pin(async { None })),
                                confirm: Arc::new(|_, _, _| Box::pin(async { false })),
                                input: Arc::new(|_, _, _| Box::pin(async { None })),
                                notify: Arc::new(|_, _| {}),
                            },
                        };
                        if let Some(mut mode) = upgrade_mode(&project_trust_weak) {
                            context = mode.create_project_trust_context(cwd);
                        }
                        context
                    });
                let result = runtime
                    .switch_session(
                        &session_path,
                        Some(SwitchSessionOptions {
                            cwd_override: Some(selected_cwd),
                            with_session: with_session.map(|callback| {
                                callback as kiana_session::agent_session_runtime::ReplacedSessionCallback
                            }),
                            project_trust_context_factory: Some(project_trust_factory),
                        }),
                    )
                    .await;
                match result {
                    Ok(result) if result.cancelled => NewSessionResult { cancelled: true },
                    Ok(_) => {
                        self.show_status("Resumed session in current cwd");
                        NewSessionResult { cancelled: false }
                    }
                    Err(error) => {
                        self.handle_fatal_runtime_error(
                            "Failed to resume session",
                            &error.to_string(),
                            None,
                        )
                        .await
                    }
                }
            }
            Err(error) => {
                self.handle_fatal_runtime_error(
                    "Failed to resume session",
                    &error.to_string(),
                    None,
                )
                .await
            }
        }
    }

    /// `maybeSaveImplicitProjectTrustAfterReload()`。
    ///
    /// reload 完成后把"本次启动可自动信任"的 cwd 持久化进信任存储（`set(&cwd, Some(true))`）。
    /// 只在满足全部条件时执行：
    ///
    /// * 启动选项给了 `auto_trust_on_reload_cwd` 且等于当前 cwd——该候选由 `app.rs`
    ///   决定：用户没有显式覆盖信任、且**启动时**项目不含需要信任的资源时才有值；
    /// * 项目当前已被信任（运行期判定）；
    /// * 项目现在存在需要信任的资源——启动时的判断与 reload 之后可能不同，因此这里
    ///   重新检查一次而不是沿用启动时的结论。
    ///
    /// 写入成功或信任记录已存在时清空候选（避免后续 reload 再走一遍；注意记录已存在的
    /// 分支返回 `false`，因此不附加 "saved project trust" 文案），写入成功返回 `true`
    /// 供 reload 的完成状态文案附加该提示；读取/写入错误只显示警告并返回 `false`，
    /// 不中断 reload。
    pub(crate) fn maybe_save_implicit_project_trust_after_reload(&mut self) -> bool {
        let cwd = self.session_manager().get_cwd().to_string();
        if self.inner.borrow().auto_trust_on_reload_cwd.as_deref() != Some(cwd.as_str()) {
            return false;
        }
        let settings_manager = self.settings_manager();
        if !settings_manager.is_project_trusted()
            || !kiana_config::trust::manager::has_trust_requiring_project_resources(&cwd)
        {
            return false;
        }
        let agent_dir = self
            .inner
            .borrow()
            .runtime_host
            .services()
            .agent_dir
            .clone();
        let trust_store = kiana_config::trust::manager::ProjectTrustStore::new(&agent_dir);
        match trust_store.get(&cwd) {
            Ok(Some(_)) => {
                self.inner.borrow_mut().auto_trust_on_reload_cwd = None;
                false
            }
            Ok(None) => match trust_store.set(&cwd, Some(true)) {
                Ok(()) => {
                    self.inner.borrow_mut().auto_trust_on_reload_cwd = None;
                    true
                }
                Err(error) => {
                    self.show_warning(&format!(
                        "Could not save project trust after reload: {error}"
                    ));
                    false
                }
            },
            Err(error) => {
                self.show_warning(&format!(
                    "Could not save project trust after reload: {error}"
                ));
                false
            }
        }
    }
}

/// `this.session !== session`（Rust 的 `AgentSession` 是共享核心的克隆：会话 id 唯一标识一次
/// 会话，切换会话后 id 必变；同一会话的 `/reload` 保持 id）。
///
/// 供 `rebind_current_session` 在绑定扩展后检测"绑定期间会话是否又被替换"：id 不同
/// 说明本调用的订阅/渲染会让位给新触发的 rebind。
fn same_session(a: &AgentSession, b: &AgentSession) -> bool {
    a.session_id() == b.session_id()
}

#[doc(hidden)]
impl InteractiveMode {
    /// `handleFatalRuntimeError(prefix, error)` 的测试接缝（进程会以 1 退出）。
    ///
    /// `tests/interactive_mode_lifecycle.rs` 在子进程里调用它，验证崩溃记录与退出码
    /// 1；因为函数不返回，测试只能在子进程里跑。正常代码不应调用。
    pub async fn test_handle_fatal_runtime_error(&mut self, prefix: &str, message: &str) -> ! {
        self.handle_fatal_runtime_error(prefix, message, None).await
    }

    /// 通过运行时创建一个新会话（触发真实的 `rebindSession` 流程；测试接缝）。
    ///
    /// `tests/interactive_mode_reload.rs` 用它制造"会话替换后的 rebind"，验证旧订阅
    /// 被移除、事件只渲染一次。返回值被丢弃——测试关心的是 rebind 的副作用而非结果。
    pub async fn test_new_session(&mut self) {
        let runtime = Rc::clone(&self.inner.borrow().runtime_host);
        let _ = runtime
            .new_session(Some(
                kiana_session::agent_session_runtime::NewSessionOptionsInput::default(),
            ))
            .await;
    }

    /// 当前会话 id（测试用）。
    ///
    /// `tests/interactive_mode_reload.rs` 用它断言 `test_new_session` 确实换掉了会话
    /// （替换后 id 必须变化）；这也是 `same_session` 判定"是否同一会话"的同一依据。
    pub fn test_session_id(&self) -> String {
        self.session()
            .session_manager()
            .get_session_id()
            .to_string()
    }
}
