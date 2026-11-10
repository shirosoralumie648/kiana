//! `interactive-mode.ts` 的事件订阅（`subscribeToAgent` / `handleEvent`，§2.50.5）。
//!
//! `AgentSession::subscribe` 的回调是 `Send + Sync`，不能直接触碰 UI：事件先转投
//! `tokio::sync::mpsc`，由 UI LocalSet 上的 `spawn_local` 任务 drain 后调用
//! `InteractiveMode::handle_event`。
//!
//! # 为什么需要这个桥接层
//!
//! 会话事件的监听器要求 `Send + Sync`——它可能在任意线程被调用；而 UI 组件是
//! `Rc<RefCell<..>>`，只能在本线程使用。因此监听器只做一件事：把事件 `clone` 进一个
//! **unbounded** channel。unbounded 是有意为之：监听器绝不能因为 UI 没来得及消费而
//! 阻塞发出事件的线程。channel 的消费端运行在 UI 所在的 LocalSet 上（`spawn_local`），
//! 逐条取出后交给 `handle_event`。
//!
//! # 执行流程
//!
//! 1. `subscribe_to_agent`（`session` 子模块在会话绑定时调用）建 channel、注册监听器、
//!    保存退订句柄（换绑会话时先退订旧会话再重订，见 `session.rs` 的
//!    `rebind_current_session`），最后启动 drain 任务；drain 任务通过 `Weak` 升级出
//!    模式实例，实例释放后任务退出。
//! 2. `handle_event` 是会话事件进入 UI 的消费点：先做懒初始化与公共刷新（footer 失效、
//!    程序状态上报），再按事件类型分发到组件与各处状态。
//! 3. 本文件还维护三类跨事件状态：
//!    * `pending_tools`：进行中工具调用的组件表（`MessageUpdate` 建行、`ToolExecution*`
//!      更新、`MessageEnd` / `AgentEnd` 收尾）；
//!    * 编辑器 escape 处理器的“暂存—恢复”对（压缩期 / 自动重试期会把 Esc 换成中止
//!      操作，特殊期结束时恢复原处理器）——见 `CompactionStart` / `AutoRetryStart` 分支；
//!    * `entries_rendered_by_boundary_compaction`：边界压缩已渲染条目的去重集合
//!      （`EntryAppended` 分支入口消费，见父模块同名字段的注释）。
//!
//! # 注意事项
//!
//! * 借用纪律继承自父模块（绝不在 `.await` 点持有 `inner.borrow()`）；`handle_event`
//!   内多处先把 `Rc` clone 出来再操作，就是为了把借用限制在同步短作用域内。
//! * 关停请求不走本文件：`AgentSettled` 分支把退出判断交给 `check_shutdown_requested`
//!   （`lifecycle` 子模块），本文件的分支只负责 UI。

use std::rc::Rc;
use std::sync::Arc;

use kiana_ai::types::{AssistantContentBlock, JsonValue, StopReason, UserContentBlock};
use kiana_ext_api::ui::WorkingIndicatorOptions;
use kiana_session::agent_session::{AgentSessionEvent, AgentSessionEventListener};
use kiana_session_core::messages::create_compaction_summary_message;
use kiana_session_core::session_manager::SessionEntry;
use kiana_theme::theme_tokens::ThemeColor;
use kiana_tui::components::spacer::Spacer;
use kiana_tui::components::text_component::StyleFn;
use kiana_tui::tui::Component;
use kiana_ui_kit::themed_text::ThemedText;

use crate::components::assistant_message::AssistantMessageComponent;
use crate::components::status_indicator::{
    BranchSummaryStatusIndicator, CompactionStatusIndicator, CompactionStatusReason,
    RetryStatusIndicator, WorkingStatusIndicator,
};
use crate::components::tool_execution::ToolExecutionResult;

use super::InteractiveMode;
use super::extension_ui::widgets::editor_supports_embedded_status as is_embedded_status;
use super::render::{AddMessageOptions, RenderOptions};

fn fg(color: ThemeColor, text: &str) -> String {
    kiana_theme::THEME
        .fg(color, text)
        .expect("theme must be initialized before rendering (TS throws here)")
}

// 事件桥接的前提：事件可以跨线程转发到 UI LocalSet。
// 编译期断言：若 `AgentSessionEvent` 将来失去 `Send + Clone`，本文件无法编译，
// 防止有人给监听器加阻塞或把 channel 换成非跨线程形式。
const _: fn() = || {
    fn assert_send<T: Send + Clone>() {}
    assert_send::<AgentSessionEvent>();
};

/// `new Date().toISOString()`。
fn iso_now() -> String {
    chrono::Utc::now()
        .format("%Y-%m-%dT%H:%M:%S%.3fZ")
        .to_string()
}

/// `CompactionReason` → 状态指示器原因。
fn compaction_status_reason(
    reason: kiana_ext_api::events::CompactionReason,
) -> CompactionStatusReason {
    match reason {
        kiana_ext_api::events::CompactionReason::Manual => CompactionStatusReason::Manual,
        kiana_ext_api::events::CompactionReason::Threshold => CompactionStatusReason::Threshold,
        kiana_ext_api::events::CompactionReason::Overflow => CompactionStatusReason::Overflow,
    }
}

/// 终端进度开关（`getShowTerminalProgress()`）。
fn show_terminal_progress(mode: &InteractiveMode) -> bool {
    mode.settings_manager().get_show_terminal_progress()
}

impl InteractiveMode {
    /// `subscribeToAgent()`：事件经 mpsc 转发到 UI LocalSet。
    ///
    /// 调用时机：`session` 子模块的 `rebind_current_session` 在每次会话绑定后调用一次；
    /// 旧订阅的退订句柄由调用方先 `take()` 并执行，因此同一时刻只有一条有效通道。
    /// drain 任务持有 `Weak`：模式实例释放后 `upgrade()` 失败，任务自然退出；
    /// channel 里的残留事件随之丢弃，不再触碰已释放的 UI。
    pub(crate) fn subscribe_to_agent(&mut self) {
        let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel::<AgentSessionEvent>();
        let listener: AgentSessionEventListener = Arc::new(move |event: &AgentSessionEvent| {
            let _ = sender.send(event.clone());
        });
        let session = self.session();
        let unsubscribe = session.subscribe(listener);
        self.inner.borrow_mut().unsubscribe = Some(unsubscribe);

        let weak = self.downgrade();
        let task = tokio::task::spawn_local(async move {
            while let Some(event) = receiver.recv().await {
                let Some(inner) = weak.upgrade() else {
                    break;
                };
                let mut mode = InteractiveMode { inner };
                mode.handle_event(&event).await;
            }
        });
        // 旧任务必须在重绑时 abort：仅取消订阅不会清空已经进入 channel 的事件，
        // 旧 drain 任务会把它们应用到新会话（例如旧 AgentEnd 清掉新会话的 Working）。
        if let Some(previous) = self.inner.borrow_mut().event_task.replace(task) {
            previous.abort();
        }
    }

    /// `handleEvent(event)`（§2.50.5 的逐事件表）。
    ///
    /// 会话事件 → UI 更新的单一消费点。每次调用先保证已初始化（`init` 是懒执行的：
    /// 事件可能在 `run()` 显式初始化之前到达），再做两件公共工作——footer 失效
    /// （footer 数据随任意事件变化）与 `program_status` 的终端状态上报——然后按事件
    /// 类型分发。未知事件静默忽略（TS 同样落进无操作的默认分支）。
    ///
    /// UI 错误不经过 `Result`：分支内部的失败直接以状态行/错误提示呈现（如
    /// `show_error`），异常路径只有内部不变量被破坏时的 `panic!`：压缩结束上下文缺
    /// 压缩条目，以及 MessageEnd 时流式消息缺失的 `expect`（见对应分支）。
    pub(crate) async fn handle_event(&mut self, event: &AgentSessionEvent) {
        if !self.inner.borrow().is_initialized {
            self.init().await;
        }

        {
            let footer = Rc::clone(&self.inner.borrow().footer);
            footer.borrow_mut().invalidate();
        }
        self.inner.borrow_mut().program_status.handle_event(event);

        match event {
            AgentSessionEvent::AgentStart => {
                self.inner.borrow_mut().pending_tools.clear();
                // agent_start 只恢复 retry 槽位；`auto_compaction_escape_handler` 不在
                // 此处触碰——压缩期的 Esc 拦截要维持到 compaction_end
                // （见 spec/ca-interactive-mode.md 的 `agent_start` 行）。
                let handler = self.inner.borrow_mut().retry_escape_handler.take();
                if let Some(handler) = handler {
                    let editor = Rc::clone(&self.inner.borrow().default_editor);
                    editor.borrow_mut().set_on_escape(Some(handler));
                }
            }
            AgentSessionEvent::TurnStart => {
                // 每轮开始：终端进度开关（若设置启用）+ Working 状态指示器的组装。
                // `working_visible` 为假时走 `clear_status_indicator(None)`——无条件清掉
                // 当前指示器（调用方语义：kind 参数为 `None` 表示不比对、直接清）。
                if show_terminal_progress(self) {
                    let terminal = self.inner.borrow().ui.terminal();
                    terminal.borrow_mut().set_progress(true);
                }
                let working_visible = self.inner.borrow().working_visible;
                if working_visible {
                    // 已有 Working 指示器时不重建：保留其动画进度，避免每轮闪烁。
                    let already_working = self
                        .inner
                        .borrow()
                        .active_status_indicator
                        .as_ref()
                        .is_some_and(|indicator| {
                            indicator.kind()
                                == crate::components::status_indicator::StatusIndicatorKind::Working
                        });
                    if !already_working {
                        self.show_working_status_indicator();
                    }
                } else {
                    self.clear_status_indicator(None);
                }
                self.request_render();
            }
            AgentSessionEvent::QueueUpdate { .. } => {
                self.update_pending_messages_display();
                self.request_render();
            }
            AgentSessionEvent::EntryAppended { entry } => {
                // 边界压缩时已渲染过的条目：只摘掉去重标记，不再渲染（否则同一条目会
                // 在聊天区出现两次）。标记在本事件下方 Compaction 分支重放“压缩点
                // 之后”条目时写入（events.rs:278-287）；CompactionEnd 的成功分支只调
                // `render_session_entries`，不经过该集合。
                if self
                    .inner
                    .borrow_mut()
                    .entries_rendered_by_boundary_compaction
                    .shift_remove(entry.id())
                {
                    return;
                }
                match entry {
                    SessionEntry::Custom(custom) => {
                        self.add_custom_entry_to_chat(custom);
                        self.request_render();
                    }
                    SessionEntry::Usage(usage) if usage.kind_name == "cache_warm" => {
                        self.add_cache_warming_usage(usage);
                        self.request_render();
                    }
                    SessionEntry::CustomMessage(custom) if custom.display => {
                        let message = kiana_session_core::messages::create_custom_message(
                            &custom.custom_type,
                            custom.content.clone(),
                            custom.display,
                            custom.details.clone(),
                            &custom.timestamp,
                        )
                        .to_agent_message();
                        self.add_message_to_chat(&message, AddMessageOptions::default());
                        self.request_render();
                    }
                    SessionEntry::Compaction(compaction) => {
                        // 边界压缩（会话树导航到更早的压缩点）到达：清空聊天区后按
                        // “压缩点之前的条目 + 摘要 + 压缩点之后的条目”重建。
                        // 只处理“追加的是当前上下文第一条”的情形；其余情况说明该压缩
                        // 条目不在上下文头部，直接忽略。
                        let manager = self.session_manager();
                        let entries = manager.build_context_entries();
                        if entries.first().map(SessionEntry::id) != Some(entry.id()) {
                            return;
                        }
                        {
                            let chat = Rc::clone(&self.inner.borrow().chat_container);
                            chat.borrow_mut().clear();
                        }
                        let branch = manager.get_branch(None);
                        // TS `branch.slice(compactionIndex + 1)`：找不到（-1）时从 0 开始，
                        // 即保留全部分支条目。
                        let compaction_index = branch
                            .iter()
                            .position(|candidate| candidate.id() == entry.id());
                        let entries_after_compaction: indexmap::IndexSet<String> = branch
                            .iter()
                            .skip(compaction_index.map_or(0, |index| index + 1))
                            .map(|candidate| candidate.id().to_string())
                            .collect();
                        // `entries[1..]`：`build_context_entries` 的首条是该压缩条目本身，
                        // 它由下面的摘要消息代替渲染，其余条目按“在压缩点之前 / 之后”
                        // 分两段重放。
                        let retained = &entries[1..];
                        let before: Vec<SessionEntry> = retained
                            .iter()
                            .filter(|candidate| !entries_after_compaction.contains(candidate.id()))
                            .cloned()
                            .collect();
                        self.render_session_entries(&before, RenderOptions::default());
                        let summary = create_compaction_summary_message(
                            &compaction.summary,
                            compaction.tokens_before,
                            &compaction.timestamp,
                        );
                        self.add_message_to_chat(
                            &summary.to_agent_message(),
                            AddMessageOptions::default(),
                        );
                        if let Some(usage) = &compaction.usage {
                            self.add_compaction_cost_notice("compaction", usage);
                        }
                        let after: Vec<SessionEntry> = retained
                            .iter()
                            .filter(|candidate| entries_after_compaction.contains(candidate.id()))
                            .cloned()
                            .collect();
                        self.render_session_entries(&after, RenderOptions::default());
                        // “压缩点之后”的条目已经渲染，标记它们：这些条目随后作为
                        // `EntryAppended` 事件到达时只摘标记、不重复渲染。
                        {
                            let mut inner = self.inner.borrow_mut();
                            for entry_id in entries_after_compaction {
                                inner
                                    .entries_rendered_by_boundary_compaction
                                    .insert(entry_id);
                            }
                        }
                        let footer = Rc::clone(&self.inner.borrow().footer);
                        footer.borrow_mut().invalidate();
                        self.request_render();
                    }
                    _ => {}
                }
            }
            AgentSessionEvent::SessionInfoChanged { .. } => {
                self.update_terminal_title();
                let footer = Rc::clone(&self.inner.borrow().footer);
                footer.borrow_mut().invalidate();
                self.request_render();
            }
            AgentSessionEvent::ThinkingLevelChanged { .. } => {
                let footer = Rc::clone(&self.inner.borrow().footer);
                footer.borrow_mut().invalidate();
                self.update_editor_border_color();
            }
            AgentSessionEvent::MessageStart { message } => match message.role() {
                "custom" => {
                    self.add_message_to_chat(message, AddMessageOptions::default());
                    self.request_render();
                }
                "user" => {
                    // 第 12 周渲染核心：服务端/会话回显消费乐观消息，避免同一输入
                    // 在待发区域与聊天区各出现一次。
                    let consumed = self.note_pending_user_echo(message);
                    self.add_message_to_chat(message, AddMessageOptions::default());
                    if consumed {
                        self.update_pending_messages_display();
                    }
                    self.request_render();
                }
                "assistant" => {
                    // 流式回复的开始：建一个 AssistantMessageComponent 挂进聊天区，
                    // 并把它记录为“当前流式组件”。后续 MessageUpdate 就地更新该组件
                    // （不再新建），MessageEnd 做最终刷新并清掉记录。
                    let Some(assistant) = message.as_assistant() else {
                        return;
                    };
                    let (hide, theme, label, output_pad) = {
                        let inner = self.inner.borrow();
                        (
                            inner.hide_thinking_block,
                            self.get_markdown_theme_with_settings(),
                            inner.hidden_thinking_label.clone(),
                            inner.output_pad,
                        )
                    };
                    let transformers = self.get_markdown_transformers();
                    let component =
                        Rc::new(std::cell::RefCell::new(AssistantMessageComponent::new(
                            None,
                            hide,
                            theme,
                            label,
                            output_pad,
                            transformers,
                            None,
                            None,
                        )));
                    component
                        .borrow_mut()
                        .update_content_streaming(assistant, true);
                    crate::interactive_mode::extension_ui::widgets::register_assistant_component(
                        &component,
                    );
                    {
                        let handle = super::shared(&component);
                        let chat = Rc::clone(&self.inner.borrow().chat_container);
                        chat.borrow_mut().add_child(super::boxed_shared(&handle));
                    }
                    {
                        let mut inner = self.inner.borrow_mut();
                        inner.streaming_component = Some(component);
                        inner.streaming_message = Some(assistant.clone());
                    }
                    self.request_render();
                }
                _ => {}
            },
            AgentSessionEvent::MessageUpdate { message, .. } => {
                // 流式增量的就地更新：文本进当前流式组件；工具调用进 `pending_tools`
                // （已有组件刷新参数，没有则新建一行）。角色不匹配（非 assistant 流）
                // 时静默忽略。
                let streaming = self.inner.borrow().streaming_component.clone();
                if let (Some(component), Some(assistant)) = (streaming, message.as_assistant())
                    && message.role() == "assistant"
                {
                    component
                        .borrow_mut()
                        .update_content_streaming(assistant, true);
                    self.inner.borrow_mut().streaming_message = Some(assistant.clone());
                    for content in &assistant.content {
                        let AssistantContentBlock::ToolCall(call) = content else {
                            continue;
                        };
                        let existing = self.inner.borrow().pending_tools.get(&call.id).cloned();
                        if let Some(component) = existing {
                            component
                                .borrow_mut()
                                .update_args(super::render::json_object_value(&call.arguments));
                        } else {
                            let component = self.create_tool_component(
                                &call.name,
                                &call.id,
                                super::render::json_object_value(&call.arguments),
                            );
                            self.add_tool_component_to_chat(&component);
                            self.inner
                                .borrow_mut()
                                .pending_tools
                                .insert(call.id.clone(), component);
                        }
                    }
                    self.request_render();
                }
            }
            AgentSessionEvent::MessageEnd { message } => {
                // 流式回复的收尾。user 消息的结束没有 UI 动作（组件在 MessageStart 已建）。
                if message.role() == "user" {
                    return;
                }
                let streaming = self.inner.borrow().streaming_component.clone();
                if let (Some(component), Some(assistant)) = (streaming, message.as_assistant())
                    && message.role() == "assistant"
                {
                    self.inner.borrow_mut().streaming_message = Some(assistant.clone());
                    // 中止时生成用户可见的错误文案，并写回流式消息体（组件用它渲染）。
                    let mut error_message: Option<String> = None;
                    if assistant.stop_reason == StopReason::Aborted {
                        let retry_attempt = self.session().retry_attempt();
                        let message = if retry_attempt > 0 {
                            format!(
                                "Aborted after {retry_attempt} retry attempt{}",
                                if retry_attempt > 1 { "s" } else { "" }
                            )
                        } else {
                            "Operation aborted".to_string()
                        };
                        error_message = Some(message.clone());
                        if let Some(streaming_message) =
                            &mut self.inner.borrow_mut().streaming_message
                        {
                            streaming_message.error_message = Some(message);
                        }
                    }
                    // 以 `streaming_message`（含刚写入的 error_message）做最终一次
                    // 非流式刷新；之后组件内容不再变化。
                    let final_message = self
                        .inner
                        .borrow()
                        .streaming_message
                        .clone()
                        .expect("streaming message is set");
                    component
                        .borrow_mut()
                        .update_content_streaming(&final_message, false);

                    if matches!(
                        final_message.stop_reason,
                        StopReason::Aborted | StopReason::Error
                    ) {
                        // 失败收尾：所有还没等到 ToolExecutionEnd 的工具行直接标错并
                        // 清空 `pending_tools`（它们的执行随本次运行一起终止了）。
                        let error_message = error_message
                            .or_else(|| {
                                final_message
                                    .error_message
                                    .clone()
                                    .filter(|value| !value.is_empty())
                            })
                            .unwrap_or_else(|| "Error".to_string());
                        let pending: Vec<_> = self
                            .inner
                            .borrow()
                            .pending_tools
                            .values()
                            .cloned()
                            .collect();
                        for tool in pending {
                            tool.borrow_mut().update_result(
                                ToolExecutionResult::new(
                                    vec![text_block(&error_message)],
                                    JsonValue::Null,
                                    true,
                                ),
                                false,
                            );
                        }
                        self.inner.borrow_mut().pending_tools.clear();
                        self.maybe_suggest_bug_report(&final_message);
                    } else {
                        // 正常收尾：工具行的参数流结束（锁定参数显示），再做各提示检查。
                        let pending: Vec<_> = self
                            .inner
                            .borrow()
                            .pending_tools
                            .values()
                            .cloned()
                            .collect();
                        for tool in pending {
                            tool.borrow_mut().set_args_complete();
                        }
                        self.maybe_show_thinking_drop_notice(&final_message);
                        self.maybe_show_cache_miss_notice(&final_message);
                    }
                    {
                        let mut inner = self.inner.borrow_mut();
                        inner.streaming_component = None;
                        inner.streaming_message = None;
                    }
                    let footer = Rc::clone(&self.inner.borrow().footer);
                    footer.borrow_mut().invalidate();
                }
                self.request_render();
            }
            AgentSessionEvent::BashExecutionUpdate { .. } => {}
            AgentSessionEvent::ToolExecutionStart {
                tool_call_id,
                tool_name,
                args,
                parent_tool_call_id,
            } => {
                // 嵌套调用（codemode）由父行显示。
                if parent_tool_call_id.is_some() {
                    return;
                }
                // 组件通常已在 MessageUpdate 建好（参数流式到达时）；这里是兜底：参数
                // 一步到位、没有流式增量的调用在执行开始时才建行。
                let existing = self.inner.borrow().pending_tools.get(tool_call_id).cloned();
                let component = match existing {
                    Some(component) => component,
                    None => {
                        let component =
                            self.create_tool_component(tool_name, tool_call_id, args.clone());
                        self.add_tool_component_to_chat(&component);
                        self.inner
                            .borrow_mut()
                            .pending_tools
                            .insert(tool_call_id.clone(), Rc::clone(&component));
                        component
                    }
                };
                component.borrow_mut().mark_execution_started();
                self.request_render();
            }
            AgentSessionEvent::ToolExecutionUpdate {
                tool_call_id,
                partial_result,
                ..
            } => {
                let component = self.inner.borrow().pending_tools.get(tool_call_id).cloned();
                if let Some(component) = component {
                    component
                        .borrow_mut()
                        .update_result(tool_result_from_json(partial_result, false, None), true);
                    self.request_render();
                }
            }
            AgentSessionEvent::ToolExecutionEnd {
                tool_call_id,
                result,
                is_error,
                duration_ms,
                ..
            } => {
                // 工具结果落定。先于取组件检查 `is_error`：安装相关工具失败时提示
                // “环境已变化”，与该行工具的渲染无关。
                if *is_error {
                    self.maybe_show_install_change_warning();
                }
                // 从 `pending_tools` 移除：此后该行不再被批量收尾逻辑触碰（结果已定）。
                let component = self
                    .inner
                    .borrow_mut()
                    .pending_tools
                    .shift_remove(tool_call_id);
                if let Some(component) = component {
                    component.borrow_mut().update_result(
                        tool_result_from_json(result, *is_error, *duration_ms),
                        false,
                    );
                    self.request_render();
                }
            }
            AgentSessionEvent::AgentEnd { .. } => {
                // 一轮 agent 运行结束：进度开关、Working 指示器、流式组件与工具行
                // 全部收尾。这里不移除聊天区里的助手消息——只有流式组件这个
                // “共享子项”需要从容器摘下（它的内容已随 MessageEnd 定格）。
                // `remove_shared_child` 用地址匹配代理指向的组件，而不是按位置猜。
                if show_terminal_progress(self) {
                    let terminal = self.inner.borrow().ui.terminal();
                    terminal.borrow_mut().set_progress(false);
                }
                self.clear_status_indicator(Some(
                    crate::components::status_indicator::StatusIndicatorKind::Working,
                ));
                let streaming = self.inner.borrow_mut().streaming_component.take();
                if let Some(streaming) = streaming {
                    let ptr = Rc::as_ptr(&streaming) as *const () as usize;
                    remove_shared_child(&self.inner.borrow().chat_container, ptr);
                    let mut inner = self.inner.borrow_mut();
                    inner.streaming_message = None;
                }
                self.inner.borrow_mut().pending_tools.clear();
                self.request_render();
            }
            AgentSessionEvent::AgentSettled { .. } => {
                // 运行彻底落定：这里是退出请求（扩展 shutdown 只置标志）被兑现的点，
                // 判断与关停逻辑在 lifecycle 子模块，本文件不参与。
                self.check_shutdown_requested().await;
            }
            AgentSessionEvent::CompactionStart { reason } => {
                // 压缩期开始：换掉编辑器的 Esc 处理器（Esc = 中止压缩），并显示压缩
                // 状态指示器。编辑器本身保持可用——用户仍可输入，提交的消息进压缩队列
                // （`queue_compaction_message`，render 子模块）。
                // 原处理器先暂存到 `auto_compaction_escape_handler`，CompactionEnd 恢复。
                if show_terminal_progress(self) {
                    let terminal = self.inner.borrow().ui.terminal();
                    terminal.borrow_mut().set_progress(true);
                }
                // 编辑器保持可用；压缩期间提交的消息排队。
                let previous = {
                    let editor = Rc::clone(&self.inner.borrow().default_editor);
                    editor.borrow_mut().take_on_escape()
                };
                self.inner.borrow_mut().auto_compaction_escape_handler = previous;
                {
                    let weak = self.downgrade();
                    let editor = Rc::clone(&self.inner.borrow().default_editor);
                    editor.borrow_mut().set_on_escape(Some(Box::new(move || {
                        if let Some(inner) = weak.upgrade() {
                            InteractiveMode { inner }.session().abort_compaction();
                        }
                    })));
                }
                // 内嵌着色（编辑器边框内嵌指示器时与边框同色）；普通模式下指示器用
                // 自身默认样式（`compaction_color_fn` 返回 `None`）。
                let color_fn = compaction_color_fn(self);
                let indicator = CompactionStatusIndicator::new(
                    Some(self.request_render_callback()),
                    compaction_status_reason(*reason),
                    color_fn,
                );
                self.show_status_indicator(Box::new(indicator));
                self.request_render();
            }
            AgentSessionEvent::CompactionEnd {
                reason,
                result,
                aborted,
                will_retry,
                error_message,
            } => {
                // 压缩期结束，按结果分四种收尾：
                // * aborted：按“手动 / 自动”选择错误或状态提示（用户视角的取消）；
                // * 成功（有 result）：清空聊天区，按新上下文重建（`entries[1..]` 跳过
                //   压缩条目本身，由摘要消息代替渲染）。上下文里缺压缩条目是内部不变量
                //   被破坏，与 TS 一样直接 panic（不静默降级）；
                // * 失败（有 error_message）：手动压缩走 `show_error`，自动压缩往聊天区
                //   插入错误文本。空串按 TS 语义视为“没有错误信息”，不渲染。
                // * 四种情况之后都排队重放压缩期收下的用户消息（见函数末尾）。
                if show_terminal_progress(self) {
                    let terminal = self.inner.borrow().ui.terminal();
                    terminal.borrow_mut().set_progress(false);
                }
                // 恢复压缩前暂存的编辑器 Esc 处理器（与 CompactionStart 配对）。
                let handler = self
                    .inner
                    .borrow_mut()
                    .auto_compaction_escape_handler
                    .take();
                if let Some(handler) = handler {
                    let editor = Rc::clone(&self.inner.borrow().default_editor);
                    editor.borrow_mut().set_on_escape(Some(handler));
                }
                self.clear_status_indicator(Some(
                    crate::components::status_indicator::StatusIndicatorKind::Compaction,
                ));
                if *aborted {
                    if *reason == kiana_ext_api::events::CompactionReason::Manual {
                        self.show_error("Compaction cancelled");
                    } else {
                        self.show_status("Auto-compaction cancelled");
                    }
                } else if let Some(result) = result {
                    // 上下文里缺压缩条目是内部不变量被破坏，与 TS 一样直接 panic。
                    let manager = self.session_manager();
                    let entries = manager.build_context_entries();
                    if entries.first().map(SessionEntry::kind) != Some("compaction") {
                        panic!("Completed compaction is missing from the session context");
                    }
                    {
                        let chat = Rc::clone(&self.inner.borrow().chat_container);
                        chat.borrow_mut().clear();
                    }
                    self.render_session_entries(&entries[1..], RenderOptions::default());
                    let summary = create_compaction_summary_message(
                        &result.summary,
                        result.tokens_before as f64,
                        &iso_now(),
                    );
                    self.add_message_to_chat(
                        &summary.to_agent_message(),
                        AddMessageOptions::default(),
                    );
                    if let Some(usage) = &result.usage {
                        self.add_compaction_cost_notice("compaction", usage);
                    }
                    let footer = Rc::clone(&self.inner.borrow().footer);
                    footer.borrow_mut().invalidate();
                } else if let Some(error_message) = error_message
                    .as_deref()
                    .filter(|message| !message.is_empty())
                {
                    // TS `else if (event.errorMessage)`：空串是假值，不渲染错误 UI。
                    if *reason == kiana_ext_api::events::CompactionReason::Manual {
                        self.show_error(error_message);
                    } else {
                        let chat = Rc::clone(&self.inner.borrow().chat_container);
                        let message = error_message.to_string();
                        let mut chat = chat.borrow_mut();
                        chat.add_child(Box::new(Spacer::new()));
                        chat.add_child(Box::new(ThemedText::with_padding(
                            Box::new(move || fg(ThemeColor::Error, &message)),
                            1,
                            0,
                        )));
                    }
                }
                // 重放压缩期排队的用户消息。`spawn_local` 而非直接 await：本事件处理
                // 还要走完剩余分支并请求重绘，重放放在之后独立执行；
                // `will_retry` 决定重放路径（见 `flush_compaction_queue` 的文档）。
                let weak = self.downgrade();
                let will_retry = *will_retry;
                tokio::task::spawn_local(async move {
                    if let Some(mut mode) = weak.upgrade().map(|inner| InteractiveMode { inner }) {
                        mode.flush_compaction_queue(will_retry).await;
                    }
                });
                self.request_render();
            }
            AgentSessionEvent::AutoRetryStart {
                attempt,
                max_attempts,
                delay_ms,
                ..
            } => {
                // 自动重试期开始：与压缩期同一套“换 Esc”机制，但暂存在独立的
                // `retry_escape_handler` 槽位——两期暂存槽位相互独立，恢复时互不
                // 干扰（见 AgentStart 分支只恢复 retry 槽位的注释）。
                let previous = {
                    let editor = Rc::clone(&self.inner.borrow().default_editor);
                    editor.borrow_mut().take_on_escape()
                };
                self.inner.borrow_mut().retry_escape_handler = previous;
                {
                    let weak = self.downgrade();
                    let editor = Rc::clone(&self.inner.borrow().default_editor);
                    editor.borrow_mut().set_on_escape(Some(Box::new(move || {
                        if let Some(inner) = weak.upgrade() {
                            InteractiveMode { inner }.session().abort_retry();
                        }
                    })));
                }
                let indicator = RetryStatusIndicator::new(
                    Some(self.request_render_callback()),
                    *attempt as u32,
                    *max_attempts as u32,
                    *delay_ms as f64,
                );
                self.show_status_indicator(Box::new(indicator));
                self.request_render();
            }
            AgentSessionEvent::AutoRetryEnd {
                success,
                attempt,
                final_error,
            } => {
                // 重试期结束：恢复 Esc 处理器、清掉 Retry 指示器；彻底失败时给出
                // 最终错误（TS 语义：空错误串按“Unknown error”展示）。
                let handler = self.inner.borrow_mut().retry_escape_handler.take();
                if let Some(handler) = handler {
                    let editor = Rc::clone(&self.inner.borrow().default_editor);
                    editor.borrow_mut().set_on_escape(Some(handler));
                }
                self.clear_status_indicator(Some(
                    crate::components::status_indicator::StatusIndicatorKind::Retry,
                ));
                if !success {
                    let final_error = final_error
                        .clone()
                        .filter(|value| !value.is_empty())
                        .unwrap_or_else(|| "Unknown error".to_string());
                    self.show_error(&format!(
                        "Retry failed after {attempt} attempts: {final_error}"
                    ));
                }
                self.request_render();
            }
            AgentSessionEvent::SummarizationRetryScheduled {
                attempt,
                max_attempts,
                delay_ms,
                error_message,
            } => {
                // 摘要（总结/压缩）调度重试：先展示触发原因，再显示倒计时指示器。
                // 与 AutoRetry 不同：这里不换编辑器 Esc 处理器。
                self.show_error(error_message);
                let indicator = RetryStatusIndicator::new(
                    Some(self.request_render_callback()),
                    *attempt as u32,
                    *max_attempts as u32,
                    *delay_ms as f64,
                );
                self.show_status_indicator(Box::new(indicator));
                self.request_render();
            }
            AgentSessionEvent::SummarizationRetryAttemptStart { source, reason } => {
                // 每次摘要重试开始：按来源（分支摘要 / 压缩）换成对应的指示器。
                // 先清掉旧 Retry 指示器再建新的（`show_status_indicator` 本身也会
                // dispose 旧指示器，这里显式清保证 kind 匹配逻辑走一遍）。
                self.clear_status_indicator(Some(
                    crate::components::status_indicator::StatusIndicatorKind::Retry,
                ));
                match source {
                    kiana_session::agent_session::SummarizationRetrySource::BranchSummary => {
                        let indicator =
                            BranchSummaryStatusIndicator::new(Some(self.request_render_callback()));
                        self.show_status_indicator(Box::new(indicator));
                    }
                    kiana_session::agent_session::SummarizationRetrySource::Compaction => {
                        let color_fn = compaction_color_fn(self);
                        let indicator = CompactionStatusIndicator::new(
                            Some(self.request_render_callback()),
                            compaction_status_reason(
                                reason.unwrap_or(kiana_ext_api::events::CompactionReason::Manual),
                            ),
                            color_fn,
                        );
                        self.show_status_indicator(Box::new(indicator));
                    }
                }
                self.request_render();
            }
            AgentSessionEvent::SummarizationRetryFinished => {
                self.clear_status_indicator(Some(
                    crate::components::status_indicator::StatusIndicatorKind::Retry,
                ));
                self.request_render();
            }
            AgentSessionEvent::HookNotification {
                message,
                level,
                hook_event,
            } => {
                // hook 的用户可见通道：systemMessage 是普通状态行，警告进入 warning
                // 样式。`suppressOutput` 的过滤已在会话层完成。
                if level == "warning" {
                    self.show_warning(&format!("{hook_event} hook: {message}"));
                } else {
                    self.show_status(message);
                }
                self.request_render();
            }
            AgentSessionEvent::Condensation { event } => {
                use kiana_agent_core::context_report::CondensationPhase;
                match event.phase {
                    CondensationPhase::Settled => {
                        self.show_status(&format!(
                            "Context compacted: saved {} tokens ({} -> {})",
                            event.saved_tokens, event.before_tokens, event.after_tokens
                        ));
                    }
                    CondensationPhase::Failed => {
                        self.show_warning(&format!(
                            "Compaction did not converge: {}",
                            event.error.as_deref().unwrap_or("unknown error")
                        ));
                    }
                    CondensationPhase::Started => {}
                }
                self.request_render();
            }
            _ => {}
        }
    }

    /// `showWorkingStatusIndicator()` 的 `colorFn`（编辑器内嵌状态时随边框色）。
    pub(crate) fn working_indicator(
        &self,
        message: &str,
        options: Option<WorkingIndicatorOptions>,
    ) -> WorkingStatusIndicator {
        let color_fn = if is_embedded_status(self) {
            Some(compaction_color_style(self))
        } else {
            None
        };
        WorkingStatusIndicator::new(
            Some(self.request_render_callback()),
            message,
            options,
            color_fn,
        )
    }
}

/// 压缩 / 工作状态指示器在“编辑器内嵌”时的着色函数。
///
/// TS: `(this.editor.borderColor ?? theme.getThinkingBorderColor(thinkingLevel))(text)`。
/// 边框色在渲染期读取（`editor_border_color_style` 由 `updateEditorBorderColor` 更新），
/// bash 模式与自定义编辑器因此和内嵌状态的着色保持一致。
///
/// `fallback` 在构造时按当前主题取一次快照；运行期只有 `editor_border_color_style()`
/// 是动态的（对应 TS 每次调用都读 `this.editor.borderColor`）。
fn compaction_color_style(mode: &InteractiveMode) -> StyleFn {
    let level = super::thinking_level_literal(mode.session().thinking_level());
    let theme = kiana_theme::THEME
        .current()
        .expect("theme must be initialized before rendering (TS throws here)");
    let fallback = theme.get_thinking_border_color(level);
    Arc::new(
        move |text: &str| match super::extension_ui::widgets::editor_border_color_style() {
            Some(color) => color(text),
            None => fallback(text),
        },
    )
}

fn compaction_color_fn(mode: &InteractiveMode) -> Option<StyleFn> {
    if !is_embedded_status(mode) {
        return None;
    }
    Some(compaction_color_style(mode))
}

/// 文本内容块。
fn text_block(text: &str) -> UserContentBlock {
    UserContentBlock::Text(kiana_ai::types::TextContent {
        kind_text: kiana_ai::types::TextKind::Text,
        text: text.to_string(),
        text_signature: None,
    })
}

/// `{...result, isError, durationMs}`（`content`/`details` 来自 JSON 对象）。
///
/// 工具结果以 JSON 对象在会话层流转；这里抽取出组件需要的字段。`content` 解析失败
/// 或缺失时按空内容处理（工具行仍会渲染，只是没有输出体），不当作错误。
fn tool_result_from_json(
    value: &JsonValue,
    is_error: bool,
    duration_ms: Option<f64>,
) -> ToolExecutionResult {
    let content = value
        .get("content")
        .cloned()
        .and_then(|content| serde_json::from_value::<Vec<UserContentBlock>>(content).ok())
        .unwrap_or_default();
    let details = value.get("details").cloned().unwrap_or(JsonValue::Null);
    ToolExecutionResult::new(content, details, is_error).with_duration_ms(duration_ms)
}

/// 从容器里移除共享句柄指向的子项。
///
/// 挂进容器的是 `SharedComponentProxy`（宿主与渲染树共享同一组件），移除时不能按
/// 位置猜，只能遍历子项、把每个代理内组件的分配地址与目标地址比对。找不到是正常
/// 情形（目标可能已被其它路径移除），静默不动。
fn remove_shared_child(container: &Rc<std::cell::RefCell<kiana_tui::tui::Container>>, ptr: usize) {
    let mut container = container.borrow_mut();
    if let Some(index) = container.children.iter().position(|child| {
        child
            .as_any()
            .and_then(|any| any.downcast_ref::<crate::tui_renderer::SharedComponentProxy>())
            .map(|proxy| Rc::as_ptr(proxy.component()) as *const () as usize)
            == Some(ptr)
    }) {
        container.remove_child_at(index);
    }
}

// ---------------------------------------------------------------------------
// 集成测试接缝（doc(hidden)；与 `kiana-session` 的 `*_for_test` 同一约定）
// ---------------------------------------------------------------------------

#[doc(hidden)]
impl InteractiveMode {
    /// 渲染 chat 容器并去掉 ANSI（测试断言用）。
    pub fn chat_render_text(&self) -> String {
        self.chat_render_raw()
            .lines()
            .map(kiana_tui::text::strip_terminal_sequences)
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// 渲染 chat 容器（保留 ANSI）。
    pub fn chat_render_raw(&self) -> String {
        let chat = Rc::clone(&self.inner.borrow().chat_container);
        let mut chat = chat.borrow_mut();
        chat.render(120).join("\n")
    }

    /// chat 容器的直接子项数量。
    pub fn chat_child_count(&self) -> usize {
        self.inner.borrow().chat_container.borrow().children.len()
    }

    /// 调用 `renderSessionEntries`（测试用）。
    pub fn test_render_entries(&mut self, entries: &[SessionEntry]) {
        self.render_session_entries(entries, RenderOptions::default());
    }

    /// 当前 `pendingTools` 数量。
    pub fn test_pending_tool_count(&self) -> usize {
        self.inner.borrow().pending_tools.len()
    }

    /// 调用 `handleEvent`（测试用）。
    pub async fn test_handle_event(&mut self, event: &AgentSessionEvent) {
        self.handle_event(event).await;
    }
}

#[doc(hidden)]
impl InteractiveMode {
    /// `showStatus` 的测试接缝。
    pub fn test_show_status(&mut self, message: &str) {
        self.show_status(message);
    }

    /// `showError` 的测试接缝。
    pub fn test_show_error(&mut self, message: &str) {
        self.show_error(message);
    }

    /// `showWarning` 的测试接缝。
    pub fn test_show_warning(&mut self, message: &str) {
        self.show_warning(message);
    }
}

#[doc(hidden)]
impl InteractiveMode {
    /// 把条目 id 放进 `entriesRenderedByBoundaryCompaction`。
    pub fn test_mark_boundary_entry(&mut self, id: &str) {
        self.inner
            .borrow_mut()
            .entries_rendered_by_boundary_compaction
            .insert(id.to_string());
    }

    /// 调用 `maybeShowThinkingDropNotice`。
    pub fn test_maybe_show_thinking_drop(&mut self, message: &kiana_ai::types::AssistantMessage) {
        self.maybe_show_thinking_drop_notice(message);
    }

    /// `restoreQueuedMessagesToEditor({ abort })` 的测试接缝。
    pub fn test_restore_queued_messages_to_editor(&mut self, abort: bool) -> usize {
        self.restore_queued_messages_to_editor(abort, None)
    }

    /// `showManagedToolStatus(status)` 的测试接缝。
    pub fn test_show_managed_tool_status(
        &mut self,
        kind: kiana_core::tools_manager::ToolStatusKind,
        message: &str,
    ) {
        self.show_managed_tool_status(kiana_core::tools_manager::ToolStatus {
            kind,
            message: message.to_string(),
        });
    }
}
