//! `interactive-mode.ts` 的 `/reload`（§2.50.13）。
//!
//! 逐字移植 `handleReloadCommand`：先关闭扩展 UI、显示 reload box，然后
//! `session.reload()` 并重建聊天 / 主题 / 自动补全 / 资源列表。
//!
//! # 本文件负责的边界
//!
//! 只处理 `/reload` 命令的编排顺序；会话本身的重建在 `AgentSession::reload`（kiana-session），
//! 聊天消息的重建在 `rebuild_chat_from_messages`，编辑器槽的常规恢复在
//! `restore_editor_into_slot`（extension_ui 子模块）。本文件的关键额外工作是**编辑器捕获**：
//! reload 过程会临时把编辑器槽换成 reload box，失败时必须恢复的是「reload 开始前的那个
//! 编辑器」，而不是结束时 `this.editor` 可能已经变成的对象——这就是 `CapturedEditor`
//! 存在的原因。
//!
//! # 执行流程
//!
//! 1. 守卫：流式响应或压缩进行中时只提示稍后重试并返回（与 TS 的提示文案一致）。
//! 2. `reset_extension_ui` 卸掉所有扩展 UI 状态，然后捕获当前编辑器。
//! 3. 把 reload box 挂进编辑器槽并聚焦，`request_render(true)` 后 `yield_now` 让出一次调度。
//! 4. `session.reload(Some(before_session_start))`：回调保证聊天在扩展的
//!    `session_start(reason="reload")` 之前重建（regression #5943，见函数内注释）。
//! 5. 失败 → 恢复捕获的编辑器并报错返回；成功 → 依次重载键位/主题/补全/扩展快捷键/
//!    资源列表，最后 `restore_editor_into_slot` 按当前活动编辑器重建编辑器槽。
//!
//! 调用方：`commands` 子模块的 `/reload` 分发（`spawn_local` 出来的独立任务里调用）。

use std::rc::Rc;
use std::sync::Arc;

use kiana_theme::theme_tokens::ThemeColor;
use kiana_tui::components::spacer::Spacer;
use kiana_tui::tui::{Component, Container};
use kiana_ui_kit::dynamic_border::DynamicBorder;
use kiana_ui_kit::themed_text::ThemedText;

use super::Expandable;
use super::InteractiveMode;

/// 本文件专用的 `theme.fg`：主题未初始化时 panic（与父模块同名辅助一致，TS 在这里抛错）。
fn fg(color: ThemeColor, text: &str) -> String {
    kiana_theme::THEME
        .fg(color, text)
        .expect("theme must be initialized before rendering (TS throws here)")
}

/// 显示 reload box 之前的活动编辑器（TS `const previousEditor = this.editor`）。
///
/// 失败的 reload 用 `dismissReloadBox(previousEditor)` 恢复**这个**对象，而不是之后可能被
/// 扩展替换掉的 `this.editor`。
#[derive(Clone)]
enum CapturedEditor {
    Default(Rc<std::cell::RefCell<crate::components::custom_editor::CustomEditor>>),
    Custom(Rc<std::cell::RefCell<Box<dyn kiana_tui::editor_component::EditorComponent>>>),
}

/// 快照当前活动编辑器（默认编辑器或扩展自定义编辑器）的共享句柄，见文件级 `//!`。
fn capture_editor(mode: &InteractiveMode) -> CapturedEditor {
    match &mode.inner.borrow().editor {
        super::ActiveEditor::Default { handle, .. } => CapturedEditor::Default(Rc::clone(handle)),
        super::ActiveEditor::Custom { handle, .. } => CapturedEditor::Custom(Rc::clone(handle)),
    }
}

impl InteractiveMode {
    /// `dismissReloadBox(editor)`：把捕获的编辑器挂回编辑器槽并聚焦。
    ///
    /// 与 `restore_editor_into_slot` 的区别：后者按**当前** `ActiveEditor` 重建并更新其中
    /// 的地址字段；这里只恢复 reload 开始前捕获的句柄，不回写 `ActiveEditor` 的地址。
    fn restore_captured_editor_into_slot(&mut self, captured: &CapturedEditor) {
        let container = Rc::clone(&self.inner.borrow().editor_container);
        container.borrow_mut().clear();
        let (proxy, address): (Box<dyn Component>, usize) = match captured {
            CapturedEditor::Default(handle) => {
                let (proxy, address) = super::DefaultEditorProxy::new(Rc::clone(handle));
                (proxy, address)
            }
            CapturedEditor::Custom(handle) => {
                let (proxy, address) = super::EditorComponentProxy::new(Rc::clone(handle));
                (proxy, address)
            }
        };
        container.borrow_mut().add_child(proxy);
        self.inner.borrow_mut().ui.set_focus_to_address(address);
    }

    /// `handleReloadCommand()`。
    ///
    /// 成功路径的重建顺序（键位 → header 展开状态 → 主题 → 运行时设置 → 补全 →
    /// 扩展快捷键 → 资源列表 → 项目信任 → models.json 错误提示）逐项对应 TS；末尾
    /// `restore_editor_into_slot` 用**当前**活动编辑器重建编辑器槽，替换掉第 3 步挂入的
    /// reload box。
    pub(crate) async fn handle_reload_command(&mut self) {
        if self.session().is_streaming() {
            self.show_warning("Wait for the current response to finish before reloading.");
            return;
        }
        if self.session().is_compacting() {
            self.show_warning("Wait for compaction to finish before reloading.");
            return;
        }

        self.reset_extension_ui();
        let previous_editor = capture_editor(self);

        let reload_box = Rc::new(std::cell::RefCell::new(Container::new()));
        {
            let border: kiana_tui::components::text_component::StyleFn =
                std::sync::Arc::new(|text: &str| fg(ThemeColor::Border, text));
            let mut container = reload_box.borrow_mut();
            container.add_child(Box::new(DynamicBorder::new(std::sync::Arc::clone(&border))));
            container.add_child(Box::new(Spacer::new()));
            container.add_child(Box::new(ThemedText::with_padding(
                Box::new(|| {
                    fg(
                        ThemeColor::Muted,
                        "Reloading keybindings, extensions, skills, prompts, themes, and context files...",
                    )
                }),
                1,
                0,
            )));
            container.add_child(Box::new(Spacer::new()));
            container.add_child(Box::new(DynamicBorder::new(border)));
        }

        // reload box 挂进编辑器槽：清空 → 挂共享容器 → 记下新子项的指针地址用于聚焦
        // （`set_focus_to_address` 按挂载树里的组件地址解析焦点，见 `tui_renderer`）。
        let editor_container = Rc::clone(&self.inner.borrow().editor_container);
        {
            let mut container = editor_container.borrow_mut();
            container.clear();
            let shared: super::SharedComponent = Rc::clone(&reload_box) as super::SharedComponent;
            container.add_child(Box::new(super::SharedComponentProxy::new(shared)));
        }
        let focus_address = {
            let container = editor_container.borrow();
            container.children.last().map(|child| {
                child.as_ref() as *const dyn kiana_tui::tui::Component as *const () as usize
            })
        };
        if let Some(address) = focus_address {
            self.inner.borrow_mut().ui.set_focus_to_address(address);
        }
        self.inner.borrow_mut().ui.request_render(true);
        tokio::task::yield_now().await;

        // TS: `session.reload({ beforeSessionStart: restoreChatBeforeSessionStart })` ——
        // 聊天必须在扩展的 `session_start(reason="reload")` 处理器运行**之前**重建，
        // 否则处理器产生的聊天输出会被随后的重建清掉（regression #5943）。
        let chat_restored = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let before_session_start: Arc<dyn Fn() + Send + Sync> = {
            let flag = Arc::clone(&chat_restored);
            // 回调签名要求 `Send + Sync`，不能捕获 `Weak<RefCell<..>>`；`reload()` 在 UI 线程
            // 上同步调用回调，因此通过线程局部句柄取回当前模式（与扩展回调同一机制）。
            Arc::new(move || {
                if flag.swap(true, std::sync::atomic::Ordering::SeqCst) {
                    return;
                }
                if let Some(mut mode) = super::session::current_mode() {
                    mode.restore_chat_before_session_start();
                }
            })
        };
        let result = self.session().reload(Some(before_session_start)).await;
        if let Err(error) = result {
            self.restore_captured_editor_into_slot(&previous_editor);
            self.show_error(&format!("Reload failed: {error}"));
            return;
        }

        // 没有绑定扩展的会话不会触发回调；这里补一次（guard 让重复调用成为 no-op）。
        if !chat_restored.swap(true, std::sync::atomic::Ordering::SeqCst) {
            self.restore_chat_before_session_start();
        }
        // 成功后继续：重载键位 → header 展开状态（内置与扩展自定义 header 两条路）→
        // 重新注册主题 → 应用运行时设置与主题 → 重建补全与扩展快捷键 → 刷新资源列表。
        self.reload_keybindings();
        if let Some(super::HeaderComponent::BuiltIn(handle)) = &self.inner.borrow().built_in_header
        {
            let expanded = self.inner.borrow().tool_output_expanded;
            handle.borrow_mut().set_expanded(expanded);
        }
        if let Some(custom) = self.inner.borrow().custom_header.clone()
            && let Ok(mut custom) = custom.try_borrow_mut()
            && let Some(any) = custom.as_any_mut()
            && let Some(expandable) = any.downcast_mut::<super::ExpandableText>()
        {
            expandable.set_expanded(self.inner.borrow().tool_output_expanded);
        }
        let themes = self.session().resource_loader().get_themes().themes;
        let _ = kiana_theme::set_registered_themes(themes);
        self.apply_runtime_settings();
        self.apply_theme_from_settings();
        self.setup_autocomplete_provider();
        let runner = self.session().extension_runner();
        self.setup_extension_shortcuts(runner);
        self.show_loaded_resources(false, true);
        let saved_implicit_project_trust = self.maybe_save_implicit_project_trust_after_reload();
        if let Some(error) = self.session().model_runtime().get_error() {
            self.show_error(&format!("models.json error: {error}"));
        }
        self.show_status(if saved_implicit_project_trust {
            "Reloaded keybindings, extensions, skills, prompts, themes, and context files; saved project trust"
        } else {
            "Reloaded keybindings, extensions, skills, prompts, themes, and context files"
        });
        self.restore_editor_into_slot();
    }

    /// 重载前重建聊天（`restoreChatBeforeSessionStart`）。
    ///
    /// 读取设置管理器里的 `hideThinkingBlock` / `outputPad` 写回模式状态，再整体重建聊天
    /// 视图。TS 同时刷新 `hideThinkingBlock` / `outputPad`，使扩展 `session_start` 处理器
    /// 看到的已经是重载后的设置。
    fn restore_chat_before_session_start(&mut self) {
        let (hide, output_pad) = {
            let settings_manager = self.settings_manager();
            (
                settings_manager.get_hide_thinking_block(),
                usize::try_from(settings_manager.get_output_pad()).unwrap_or(0),
            )
        };
        {
            let mut inner = self.inner.borrow_mut();
            inner.hide_thinking_block = hide;
            inner.output_pad = output_pad;
        }
        self.rebuild_chat_from_messages();
    }
}

#[doc(hidden)]
impl InteractiveMode {
    /// `handleReloadCommand` 的测试接缝。
    pub async fn test_handle_reload_command(&mut self) {
        self.handle_reload_command().await;
    }

    /// `restoreChatBeforeSessionStart` 的测试接缝（reload 钩子）。
    pub fn test_restore_chat_before_session_start(&mut self) {
        self.restore_chat_before_session_start();
    }

    /// 当前 `hideThinkingBlock` 字段。
    pub fn test_hide_thinking_block(&self) -> bool {
        self.inner.borrow().hide_thinking_block
    }
}
