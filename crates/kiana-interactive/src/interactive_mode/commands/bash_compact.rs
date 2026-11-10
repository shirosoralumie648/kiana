//! `interactive-mode.ts` 的 bash 命令与 `/compact`（§2.50.17）。
//!
//! # 为什么这个文件存在
//!
//! 交互模式下用户用 `!`/`!!` 前缀直接执行 shell 命令（`!!` 表示结果不进入模型上下文），
//! 用 `/compact` 手动压缩会话历史。这两个入口在 `commands/mod.rs` 的提交分派表里匹配，
//! 实际逻辑集中在本文件。
//!
//! # bash 命令的执行流程
//!
//! 1. 向扩展 runner 发 `user_bash` 事件。handler 可以：返回完整结果（直接展示并记录，
//!    不再执行）、返回自定义 `BashOperations`（用它执行）、或什么都不返回（走本地
//!    shell）。事件派发出错时**不落回本地执行**（fail closed），直接返回。
//! 2. 挂载 `BashExecutionComponent`：会话正在流式响应时先进 pending 容器并登记到
//!    `pending_bash_components`（下一条用户消息提交时由 `flush_pending_bash_components`
//!    移入 chat），否则直接进 chat。
//! 3. `session.execute_bash` 执行命令，输出块通过 `on_chunk` 回调实时追加到组件。
//! 4. 完成 → `set_complete(...)` 收尾；失败 → `set_complete(None, false, ...)` 并
//!    `show_error`。两种结局最后都清掉 `bash_component` 并请求重绘。
//!
//! # 流式回调为什么要过线程局部表
//!
//! `execute_bash` 的回调类型 `BashChunkCallback` 是 `Arc<dyn Fn + Send + Sync>`，
//! 闭包不能捕获 UI 侧的 `Rc<RefCell<…>>`。本地 bash 的输出在本任务被 poll 时同步到达
//! （`kiana-tools` 的 `bash.rs`），与 UI 同在一个线程，因此把组件与模式的 `Weak` 登记
//! 进线程局部表 `LIVE_BASH`，回调里再升级借用（见 `LiveBashStream`）。
//!
//! # 调用方
//!
//! 仅 `commands/mod.rs`：`dispatch_submit` 的 `!`/`!!` 分支调用 `handle_bash_command`
//! （经 `tokio::task::spawn_local` 延后），`/compact` 分支调用 `handle_compact_command`。
//! `test_*` 方法是集成测试（`tests/interactive_mode_commands.rs`）的接缝。

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use kiana_ext_api::events::{UserBashEvent, UserBashEventResult, UserBashEventType};
use kiana_ext_api::tool_types::TruncationResult;
use kiana_session::agent_session::ExecuteBashOptions;
use kiana_tools::bash_executor::BashChunkCallback;

use crate::components::bash_execution::BashExecutionComponent;

use super::super::{InteractiveMode, InteractiveModeInner};

/// 流式输出目标：`executeBash` 的 `onChunk` 回调要求 `Send + Sync`，不能捕获
/// `Rc<RefCell<…>>`；bash 执行与回调都在同一个 UI 线程上（`execute_bash_with_operations`
/// 是内联 async），因此用线程局部登记表桥接。
///
/// `id` 是每次执行自增的序号：回调闭包捕获自己的 `bash_id`，只在表里登记的编号与它
/// 相同才写入。这样即使上一次执行的 component 尚未释放（`Weak` 还能升级），迟到的
/// 输出也不会被拼到新组件上。`component`/`mode` 都用 `Weak`，避免延长 UI 生命周期
/// （代价是模式或组件已释放时回调静默跳过，不再补渲染）。
struct LiveBashStream {
    id: u64,
    mode: std::rc::Weak<RefCell<InteractiveModeInner>>,
    component: std::rc::Weak<RefCell<BashExecutionComponent>>,
}

thread_local! {
    // 当前（最近一次）bash 执行的流式目标；执行结束后置回 `None`。
    static LIVE_BASH: RefCell<Option<LiveBashStream>> = const { RefCell::new(None) };
    // 单调递增的执行编号，只增不减（用于区分新旧执行，不做回收）。
    static LIVE_BASH_ID: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

impl InteractiveMode {
    /// `handleBashCommand(command, excludeFromContext?)`。
    ///
    /// `command` 是已剥掉 `!`/`!!` 前缀并做 JS `trim` 的正文；`exclude_from_context`
    /// 为 `true` 时结果不进入模型上下文（`!!`）。
    ///
    /// 先向扩展 runner 发 `user_bash` 事件（携带当前会话 cwd），再交给
    /// `handle_bash_after_emit`。事件派发失败时**只放弃执行**：不挂组件、不跑本地
    /// shell、也不在这里显示错误（fail closed）。失败本身由扩展 runner 的 error sink
    /// 上报——测试 `user_bash_invalid_extension_result_fails_closed_from_the_editor`
    /// 断言了非法 handler 结果会出现在 error 日志里，且没有 `$ pwd` 组件。
    pub(crate) async fn handle_bash_command(
        &mut self,
        command: String,
        exclude_from_context: bool,
    ) {
        let extension_runner = self.session().extension_runner();
        let event = UserBashEvent {
            event_type: UserBashEventType::UserBash,
            command: command.clone(),
            exclude_from_context,
            cwd: self.session_manager().get_cwd().to_string(),
        };
        let emit = match extension_runner.emit_user_bash(event).await {
            Ok(result) => Ok(result),
            // 扩展 runner 已报告错误：不落回本地执行（fail closed）。
            Err(error) => Err(error.to_string()),
        };
        self.handle_bash_after_emit(command, exclude_from_context, emit)
            .await;
    }

    /// `emitUserBash` 之后的统一入口（`Err` = 扩展已报告错误，直接放弃执行）。
    ///
    /// 拆成独立函数是为了给测试一个不经过扩展 runner 的注入点
    /// （`test_handle_bash_command_failed_emit` 直接用 `Err` 调用）。
    async fn handle_bash_after_emit(
        &mut self,
        command: String,
        exclude_from_context: bool,
        emit: Result<Option<UserBashEventResult>, String>,
    ) {
        let event_result = match emit {
            Ok(result) => result,
            Err(_) => return,
        };
        self.handle_bash_event_result(command, exclude_from_context, event_result)
            .await;
    }

    /// `handleBashCommand` 中 `emitUserBash` 之后的部分。
    ///
    /// 三条分支，对应 §2.50.17 的第 2–6 步：
    /// - 扩展返回 `Result`：组件只用于展示，输出一次性写入，记录结果后返回；
    ///   不走 `execute_bash`。
    /// - 扩展返回 `Operations`：用扩展提供的操作执行（`operations` 透传给
    ///   `ExecuteBashOptions`）。
    /// - 什么都没返回：`operations` 为 `None`，由 `execute_bash` 自行创建本地操作。
    ///
    /// 组件挂到哪里取决于**开始执行时**的 `session.is_streaming()`：流式中挂 pending，
    /// 否则挂 chat。注意 `truncated` 为真时重新构造的 `TruncationResult` 只填
    /// `content`/`truncated`，其余字段取 `Default`（与 TS 只传 `{truncated:true,
    /// content}` 一致），因此统计类字段并不携带真实数值。
    async fn handle_bash_event_result(
        &mut self,
        command: String,
        exclude_from_context: bool,
        event_result: Option<UserBashEventResult>,
    ) {
        // 扩展返回完整结果：直接展示并记录。
        if let Some(UserBashEventResult::Result(result)) = event_result {
            let component = self.mount_bash_component(&command, exclude_from_context);
            {
                let mut component = component.borrow_mut();
                if !result.output.is_empty() {
                    component.append_output(&result.output);
                }
                component.set_complete(
                    result.exit_code,
                    result.cancelled,
                    result.truncated.then(|| TruncationResult {
                        content: result.output.clone(),
                        truncated: true,
                        ..Default::default()
                    }),
                    result.full_output_path.clone(),
                );
            }
            self.session()
                .record_bash_result(command, &result, Some(exclude_from_context));
            self.inner.borrow_mut().bash_component = None;
            self.request_render();
            return;
        }

        let operations = match event_result {
            Some(UserBashEventResult::Operations(operations)) => Some(operations),
            _ => None,
        };
        let is_deferred = self.session().is_streaming();
        let component =
            self.mount_bash_component_deferred(&command, exclude_from_context, is_deferred);
        self.request_render();

        // 登记流式目标后再开始执行：回调可能在 `execute_bash` 返回前就被调用。编号必须
        // 先自增再登记，回调闭包捕获的是本次编号，旧回调因编号不匹配而自行退出。
        let bash_id = LIVE_BASH_ID.with(|slot| {
            let id = slot.get() + 1;
            slot.set(id);
            id
        });
        LIVE_BASH.with(|slot| {
            *slot.borrow_mut() = Some(LiveBashStream {
                id: bash_id,
                mode: self.downgrade(),
                component: Rc::downgrade(&component),
            });
        });
        // 回调运行期间持有 `LIVE_BASH` 的不可变借用（`slot.borrow()`）：闭包内的任何
        // 代码都不能再借 `LIVE_BASH` 的可变借用，否则会 RefCell panic。因此这里只用
        // `try_borrow_mut` 访问组件——组件正被别处借用时宁可直接丢掉这一块输出，也不
        // 在渲染回调栈上 panic。`mode`/`component` 是 `Weak`，模式或组件已释放时静默跳过。
        let on_chunk: BashChunkCallback = Arc::new(move |chunk: &str| {
            LIVE_BASH.with(|slot| {
                let target = slot.borrow();
                let Some(target) = target.as_ref() else {
                    return;
                };
                if target.id != bash_id {
                    return;
                }
                let Some(component) = target.component.upgrade() else {
                    return;
                };
                if let Ok(mut component) = component.try_borrow_mut() {
                    component.append_output(chunk);
                }
                if let Some(inner) = target.mode.upgrade() {
                    InteractiveMode { inner }.request_render();
                }
            });
        });

        let result = self
            .session()
            .execute_bash(
                command,
                Some(on_chunk),
                Some(ExecuteBashOptions {
                    exclude_from_context: Some(exclude_from_context),
                    id: None,
                    operations,
                }),
            )
            .await;
        // 执行结束（无论成功失败）立刻注销流式目标，避免迟到的 chunk 写入已经收尾的组件。
        LIVE_BASH.with(|slot| *slot.borrow_mut() = None);

        // 收尾用的是 `inner.bash_component` 而不是上面的局部 `component`：二者指向同一个
        // 组件（`mount_bash_component_deferred` 里放进槽位的就是它）。成功时写入退出码/
        // 取消/截断信息；失败时把组件标成“无退出码、未取消、无截断”，与 TS 的
        // `setComplete(undefined, false)` 对应，再额外显示错误行。
        match result {
            Ok(result) => {
                let bash_component = self.inner.borrow().bash_component.clone();
                if let Some(bash_component) = bash_component {
                    let mut component = bash_component.borrow_mut();
                    component.set_complete(
                        result.exit_code,
                        result.cancelled,
                        result.truncated.then(|| TruncationResult {
                            content: result.output.clone(),
                            truncated: true,
                            ..Default::default()
                        }),
                        result.full_output_path.clone(),
                    );
                }
            }
            Err(error) => {
                let bash_component = self.inner.borrow().bash_component.clone();
                if let Some(bash_component) = bash_component {
                    bash_component
                        .borrow_mut()
                        .set_complete(None, false, None, None);
                }
                self.show_error(&format!("Bash command failed: {error}"));
            }
        }

        self.inner.borrow_mut().bash_component = None;
        self.request_render();
    }

    /// `handleCompactCommand(customInstructions?)`。
    ///
    /// 先清掉状态指示器（压缩开始后事件流会重新挂一个 compaction 指示器），再发起压缩。
    /// 返回值被丢弃：错误由 `CompactionEnd` 事件携带（`agent_session.rs` 的 `compact`
    /// 在失败分支里填 `error_message`），UI 在事件处理里 `show_error`，因此这里不再
    /// 重复提示。
    pub(crate) async fn handle_compact_command(&mut self, custom_instructions: Option<String>) {
        self.clear_status_indicator(None);
        // 错误通过事件呈现。
        let _ = self.session().compact(custom_instructions).await;
    }

    /// 创建并挂载 bash 组件（按是否流式决定挂到 pending 还是 chat）。
    ///
    /// 只被“扩展已给出完整结果”的分支使用；正常执行路径为了复用同一个
    /// `is_streaming` 判定值会直接调 `mount_bash_component_deferred`。
    fn mount_bash_component(
        &mut self,
        command: &str,
        exclude_from_context: bool,
    ) -> Rc<RefCell<BashExecutionComponent>> {
        let is_deferred = self.session().is_streaming();
        self.mount_bash_component_deferred(command, exclude_from_context, is_deferred)
    }

    /// 创建并挂载 bash 组件。
    ///
    /// `is_deferred` 为真时把组件同时加到 pending 容器与
    /// `pending_bash_components` 列表；之后用户提交下一条消息时
    /// `interactive_mode::render::flush_pending_bash_components` 依据该列表把组件从
    /// pending 移到 chat。该路径暂无直接测试覆盖：`bash_command_runs_and_clears_bash_mode`
    /// 走的是非流式（`is_streaming()` 为 false）的直接 chat 挂载，观察到的
    /// `$ echo hi` 并不经过 flush。
    ///
    /// 无论挂到哪里，都会把组件记到 `inner.bash_component` 并返回同一个 `Rc`：调用方
    /// 用返回值（完整结果分支里直接写入）或降级成 `Weak` 登记到 `LIVE_BASH`。执行结束
    /// 时代码从槽位重新取回组件收尾，再把槽位置回 `None`；槽位因此也是“当前正在执行的
    /// bash 组件”的唯一记录点。
    fn mount_bash_component_deferred(
        &mut self,
        command: &str,
        exclude_from_context: bool,
        is_deferred: bool,
    ) -> Rc<RefCell<BashExecutionComponent>> {
        let output_pad = self.inner.borrow().output_pad;
        let component = Rc::new(RefCell::new(BashExecutionComponent::new(
            command,
            Some(self.request_render_callback()),
            exclude_from_context,
            output_pad,
        )));
        if is_deferred {
            let pending = Rc::clone(&self.inner.borrow().pending_messages_container);
            pending.borrow_mut().add_child(Box::new(
                crate::tui_renderer::SharedComponentProxy::new(
                    Rc::clone(&component) as kiana_tui::layout_node::SharedComponent
                ),
            ));
            self.inner
                .borrow_mut()
                .pending_bash_components
                .push(Rc::clone(&component));
        } else {
            let chat = Rc::clone(&self.inner.borrow().chat_container);
            chat.borrow_mut()
                .add_child(Box::new(crate::tui_renderer::SharedComponentProxy::new(
                    Rc::clone(&component) as kiana_tui::layout_node::SharedComponent,
                )));
        }
        self.inner.borrow_mut().bash_component = Some(Rc::clone(&component));
        component
    }

    /// `handleBashCommand` 的测试接缝。
    #[doc(hidden)]
    pub async fn test_handle_bash_command(&mut self, command: &str, exclude_from_context: bool) {
        self.handle_bash_command(command.to_string(), exclude_from_context)
            .await;
    }

    /// `emit_user_bash` 抛错时 fail-closed（9068）：测试接缝。
    ///
    /// 直接以 `Err` 注入，绕过扩展 runner，用来断言“派发失败不执行本地 bash、不挂
    /// 组件”这一分支（真实路径见 `handle_bash_command` 的 `Err(error) => Err(...)`）。
    #[doc(hidden)]
    pub async fn test_handle_bash_command_failed_emit(
        &mut self,
        command: &str,
        exclude_from_context: bool,
    ) {
        self.handle_bash_after_emit(
            command.to_string(),
            exclude_from_context,
            Err("Routing failed".to_string()),
        )
        .await;
    }

    /// `handleCompactCommand` 的测试接缝。
    #[doc(hidden)]
    pub async fn test_handle_compact_command(&mut self, custom_instructions: Option<String>) {
        self.handle_compact_command(custom_instructions).await;
    }
}
