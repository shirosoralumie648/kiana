//! `interactive-mode.ts` 的 `/export` `/import` `/share` `/bug` `/copy` `/name`（§2.50.14）。
//!
//! # 这个文件负责什么
//!
//! 六个“会话/文本进出”类命令的 UI 侧入口：把用户输入解析成参数，调用
//! `kiana-session`/`kiana-session-core` 或子系统（`session_share`、`bug_report`）完成
//! 实际操作，再把结果写成状态行或错误。
//!
//! 生产路径的调用方只有 `commands/mod.rs`：斜杠命令在写死的匹配表里命中后转到这里；
//! `app.message.copy` 键位处理器也调用 `handle_copy_command`，但传入不同的
//! `CopyOptions`。`test_*` 方法另供集成测试直接调用。
//!
//! # 各命令的失败策略（差异是刻意的，不要统一）
//!
//! - `/export`：导出失败只 `show_error`，流程继续。
//! - `/import` 无参数或文件不存在：`show_error`，流程继续（测试
//!   `import_reports_a_missing_file_as_a_non_fatal_error` 锁定文件不存在不得走致命路径）。
//! - `/import` 遇到会话 cwd 缺失：额外弹一次确认框，用户同意后带 `cwd_override` 重试。
//! - `/import` 的其它错误：走 `handle_fatal_runtime_error`（打印错误、记录崩溃、退出
//!   进程）。
//! - `/share`、`/bug` 的 `Err`：走 `uncaught_crash`。这不是遗漏——TS 里这些
//!   `await` 的拒绝无人捕获，会被 `uncaughtException` 处理器转成崩溃；这里复刻同一条路径。
//!
//! # 注意事项
//!
//! - 本文件只做编排；实际导出/导入/分享/提交 bug 的行为与错误文案由被调用方决定。
//! - `fg` 依赖全局主题已初始化，未初始化会 panic（与 TS 在此处抛错一致）。

use std::rc::Rc;

use kiana_json::js_trim;
use kiana_session::agent_session_runtime::AgentSessionRuntimeError;
use kiana_session_core::session_cwd::format_missing_session_cwd_prompt;
use kiana_tui::components::spacer::Spacer;
use kiana_ui_kit::themed_text::ThemedText;

use crate::bug_report::{BugReportContext, report_bug};
use crate::interactive_mode::extension_ui::widgets::TuiArcAdapter;
use crate::session_share::{SessionShareContext, share_session};
use crate::tui_renderer::InteractiveTuiReference;

use super::super::InteractiveMode;
use super::{is_js_whitespace, js_trim_start};

/// 按主题给文本着色；主题未初始化时 panic（TS 在同样位置抛错）。
fn fg(color: kiana_theme::theme_tokens::ThemeColor, text: &str) -> String {
    kiana_theme::THEME
        .fg(color, text)
        .expect("theme must be initialized before rendering (TS throws here)")
}

impl InteractiveMode {
    /// `handleExportCommand(text)`。
    ///
    /// 参数经 `get_path_command_argument` 解析（无参数或空参数时为 `None`）。格式选择
    /// 只看路径后缀：以 `.jsonl` 结尾走 JSONL 导出，其它（包括无路径、`.html`）都走
    /// HTML 导出；此时把**当前主题名**传给导出器（`THEME.name()` 取不到时传 `None`，
    /// 由导出器再按设置回退）。实际写入文件与默认路径由 `kiana-session` 决定，返回的
    /// 是最终写出的路径。
    ///
    /// 失败只 `show_error`，不退出进程。
    pub(crate) async fn handle_export_command(&mut self, text: &str) {
        let output_path = get_path_command_argument(text, "/export");
        let session = self.session();
        let result = if output_path
            .as_deref()
            .is_some_and(|path| path.ends_with(".jsonl"))
        {
            session.export_to_jsonl(output_path.clone())
        } else {
            session.export_to_html(
                output_path.clone(),
                Some(kiana_session::agent_session::ExportThemeOptions {
                    theme_name: kiana_theme::THEME.name().ok().flatten(),
                }),
            )
        };
        match result {
            Ok(file_path) => self.show_status(&format!("Session exported to: {file_path}")),
            Err(error) => self.show_error(&format!("Failed to export session: {error}")),
        }
    }

    /// `handleImportCommand(text)`。
    ///
    /// 流程：解析路径 → 弹确认框（替换当前会话是破坏性操作）→ `runtime.import_from_jsonl`
    /// → 按结果分支。
    ///
    /// 分支要点：
    /// - 无参数：只提示用法并返回，不弹框。
    /// - `MissingSessionCwd`：会话文件里记录的 cwd 已不存在。此时先经
    ///   `prompt_for_missing_import_cwd` 弹第二次确认；用户同意则用错误里携带的
    ///   `fallback_cwd` 作为 `cwd_override` 重试一次 `import_from_jsonl`，拒绝则视为取消。
    /// - `SessionImportFileNotFound`：作为普通错误展示（测试
    ///   `import_reports_a_missing_file_as_a_non_fatal_error` 锁定它不得进入致命路径）。
    /// - 其它错误：走 `handle_fatal_runtime_error`，它会记录崩溃并退出进程。
    ///
    /// 两次调用都返回 `SessionReplacementResult`；`cancelled` 为真表示替换被扩展的
    /// before-switch 处理器拦下（`import_from_jsonl` 内部返回），按取消提示，不算错误。
    pub(crate) async fn handle_import_command(&mut self, text: &str) {
        let Some(input_path) = get_path_command_argument(text, "/import") else {
            self.show_error("Usage: /import <path.jsonl>");
            return;
        };

        let confirmed = self
            .show_extension_confirm(
                "Import session",
                &format!("Replace current session with {input_path}?"),
                None,
            )
            .await;
        if !confirmed {
            self.show_status("Import cancelled");
            return;
        }

        self.clear_status_indicator(None);
        let runtime = Rc::clone(&self.inner.borrow().runtime_host);
        let result = runtime.import_from_jsonl(&input_path, None).await;
        match result {
            Ok(result) if result.cancelled => self.show_status("Import cancelled"),
            Ok(_) => self.show_status(&format!("Session imported from: {input_path}")),
            Err(AgentSessionRuntimeError::MissingSessionCwd(error)) => {
                let selected_cwd = self.prompt_for_missing_import_cwd(&error).await;
                let Some(selected_cwd) = selected_cwd else {
                    self.show_status("Import cancelled");
                    return;
                };
                let result = runtime
                    .import_from_jsonl(&input_path, Some(&selected_cwd))
                    .await;
                match result {
                    Ok(result) if result.cancelled => self.show_status("Import cancelled"),
                    Ok(_) => self.show_status(&format!("Session imported from: {input_path}")),
                    Err(error) => {
                        self.handle_fatal_runtime_error(
                            "Failed to import session",
                            &error.to_string(),
                            None,
                        )
                        .await;
                    }
                }
            }
            Err(AgentSessionRuntimeError::SessionImportFileNotFound(error)) => {
                self.show_error(&format!("Failed to import session: {error}"));
            }
            Err(error) => {
                self.handle_fatal_runtime_error(
                    "Failed to import session",
                    &error.to_string(),
                    None,
                )
                .await;
            }
        }
    }

    /// `handleShareCommand()`。
    ///
    /// 组装 `SessionShareContext`（会话、UI 句柄、编辑器槽位、状态/错误回调）后交给
    /// `session_share::share_session`。两个回调都只持有 `Weak`，分享期间模式被释放时
    /// 静默丢弃提示。
    ///
    /// 失败走 `uncaught_crash`（见模块文档：TS 侧是未捕获的 Promise 拒绝）。正常情况
    /// 下该路径会直接退出进程，`uncaught_crash` 之后的代码不会执行；只有在测试接管了
    /// 退出（`set_exit_hook_for_test`）它才返回，此时仍会调用 `restore_editor_into_slot`
    /// 把编辑器放回槽位。
    pub(crate) async fn handle_share_command(&mut self) {
        let session = self.session();
        let ui = self.tui_handle();
        let editor_container = Rc::clone(&self.inner.borrow().editor_container);
        let editor = self.current_editor_shared();
        let weak_status = self.downgrade();
        let show_status: Rc<dyn Fn(&str)> = Rc::new(move |message| {
            if let Some(inner) = weak_status.upgrade() {
                InteractiveMode { inner }.show_status(message);
            }
        });
        let weak_error = self.downgrade();
        let show_error: Rc<dyn Fn(&str)> = Rc::new(move |message| {
            if let Some(inner) = weak_error.upgrade() {
                InteractiveMode { inner }.show_error(message);
            }
        });
        let context = SessionShareContext {
            session,
            ui,
            editor_container,
            editor,
            show_status,
            show_error,
        };
        if let Err(error) = share_session(context).await {
            // TS：`handleShareCommand()` 的拒绝没有被捕获（`onSubmit` 调用它时不 await），
            // 变成 unhandled rejection → `uncaughtException` → `uncaughtCrash`（退出 1、
            // 记录崩溃）。这里直接走同一条崩溃路径，而不是静默吞掉错误。
            self.uncaught_crash(&error, None);
        }
        // `/share` 结束时由子系统恢复编辑器；刷新地址，避免后续焦点指向旧代理。
        self.restore_editor_into_slot();
    }

    /// `handleBugCommand(hint)`。
    ///
    /// 除会话与编辑器外，还额外准备 `EditorTui` 与一份键位配置快照
    /// `Rc<KeybindingsManager>`：`report_bug` 会用它们驱动一轮独立的编辑器交互
    /// （让用户选择/填写报告选项），所以终端句柄来自当前渲染器，与主编辑器共享同一个
    /// 终端。`hint` 是 `/bug` 后跟的提示文本。
    ///
    /// 结果交给 `finish_bug_command` 统一收尾。
    pub(crate) async fn handle_bug_command(&mut self, hint: Option<String>) {
        let session = self.session();
        let ui = self.tui_handle();
        let editor_container = Rc::clone(&self.inner.borrow().editor_container);
        let editor = self.current_editor_shared();
        let editor_tui = {
            let renderer = Rc::clone(&self.inner.borrow().renderer);
            let terminal = renderer.borrow().terminal();
            kiana_tui::components::editor::EditorTui::from_terminal(terminal)
        };
        let keybindings = Rc::new((*self.inner.borrow().keybindings).clone());
        let weak_status = self.downgrade();
        let show_status: Rc<dyn Fn(&str)> = Rc::new(move |message| {
            if let Some(inner) = weak_status.upgrade() {
                InteractiveMode { inner }.show_status(message);
            }
        });
        let weak_error = self.downgrade();
        let show_error: Rc<dyn Fn(&str)> = Rc::new(move |message| {
            if let Some(inner) = weak_error.upgrade() {
                InteractiveMode { inner }.show_error(message);
            }
        });
        let context = BugReportContext {
            session,
            ui,
            editor_container,
            editor,
            editor_tui,
            keybindings,
            show_status,
            show_error,
        };
        let result = report_bug(context, hint).await;
        self.finish_bug_command(result);
    }

    /// `/bug` 的结果处理。
    ///
    /// TS：`await reportBug(...)` 的拒绝没有被 `handleBugCommand` 捕获（编辑器在
    /// `onSubmit` 里不 await 它），变成 unhandled rejection → `uncaughtException`
    /// → `uncaughtCrash`（退出 1、记录崩溃）。必须走同一条崩溃路径，而不是静默吞掉；
    /// 只有成功路径才恢复编辑器（TS 里 `reportBug` 内部已经恢复，这里是 Rust 的显式
    /// 收尾，与之前的行为一致）。
    pub(crate) fn finish_bug_command(
        &mut self,
        result: Result<(), kiana_session_core::session_manager::SessionManagerError>,
    ) {
        match result {
            Ok(()) => self.restore_editor_into_slot(),
            Err(error) => self.uncaught_crash(&error, None),
        }
    }

    /// `handleCopyCommand(options?)`。
    ///
    /// 两条复制来源，优先级如下：
    /// 1. `prefer_selection` 为真、处于全屏模式、copy-on-select 关闭且当前确实有选区：
    ///    复制鼠标选区并直接返回（这条路径不显示任何提示）。
    /// 2. 否则取会话里最后一条 assistant 文本；为空时 `show_error("No agent messages to
    ///    copy yet.")`。
    ///
    /// 第 2 条成功后：`flash_confirmation` 为真且全屏时用瞬时 flash（`Copied!`），
    /// 否则写普通状态行。剪贴板写入失败把错误文本交给 `show_error`。
    ///
    /// `options` 的两个字段组合见 `super::CopyOptions`：`/copy` 用 `Default`（全 false），
    /// `app.message.copy` 键位传 `flash_confirmation: true, prefer_selection: true`。
    pub(crate) async fn handle_copy_command(&mut self, options: super::CopyOptions) {
        if options.prefer_selection
            && self.inner.borrow().ui.mode() == kiana_tui::tui::TuiMode::Fullscreen
            && !self.inner.borrow().ui.get_copy_on_select()
            && self.inner.borrow().ui.has_active_selection()
        {
            let renderer = Rc::clone(&self.inner.borrow().renderer);
            renderer.borrow_mut().copy_active_selection_to_clipboard();
            return;
        }

        let Some(text) = self.session().get_last_assistant_text() else {
            self.show_error("No agent messages to copy yet.");
            return;
        };

        match kiana_core::clipboard::copy_to_clipboard(&text).await {
            Ok(()) => {
                if options.flash_confirmation
                    && self.inner.borrow().ui.mode() == kiana_tui::tui::TuiMode::Fullscreen
                {
                    self.inner.borrow_mut().ui.flash("Copied!", None);
                } else {
                    self.show_status("Copied last agent message to clipboard");
                }
            }
            Err(error) => self.show_error(&error.to_string()),
        }
    }

    /// `handleNameCommand(text)`。
    ///
    /// TS 原文是 `text.replace(/^\/name\s*/, "").trim()`；这里用 JS 空白集合模拟同一
    /// 语义（`\s` 含 U+FEFF，Rust 的 `trim` 不含，见 `is_js_whitespace`）。
    ///
    /// - 名字为空：有现有名字时在 chat 里显示 `Session name: <name>`（dim 色），
    ///   没有则提示用法 `Usage: /name <name>`。
    /// - 名字非空：写入会话，然后**回读**一次实际存储的名字。回读值与输入不同时
    ///   （例如 `\r\n` 被规范化成空格）额外给一条 warning，两处名字都用 JSON 字符串
    ///   形式展示（`JSON.stringify`；回读为 `None` 时字面量 `undefined`）。
    ///   测试 `name_normalization_warns_with_the_json_quoted_names` 锁定了这段文案。
    ///
    /// 本函数不调用 `clear_editor`；`dispatch_submit` 的 `/name` 分支在同步调用它之后
    /// 才清空编辑器。
    pub(crate) fn handle_name_command(&mut self, text: &str) {
        // `text.replace(/^\/name\s*/, "").trim()`：`\s` 与 `trim` 都是 JS 语义
        // （含 U+FEFF 等码点），这里显式按同一集合剥离。
        let rest = text.strip_prefix("/name").unwrap_or(text);
        let name = js_trim(js_trim_start(rest)).to_string();
        if name.is_empty() {
            let current_name = self.session_manager().get_session_name();
            match current_name.filter(|name| !name.is_empty()) {
                Some(current_name) => {
                    let chat = Rc::clone(&self.inner.borrow().chat_container);
                    {
                        let mut chat = chat.borrow_mut();
                        chat.add_child(Box::new(Spacer::new()));
                        chat.add_child(Box::new(ThemedText::with_padding(
                            Box::new(move || {
                                fg(
                                    kiana_theme::theme_tokens::ThemeColor::Dim,
                                    &format!("Session name: {current_name}"),
                                )
                            }),
                            1,
                            0,
                        )));
                    }
                }
                None => self.show_warning("Usage: /name <name>"),
            }
            self.request_render();
            return;
        }

        self.session().set_session_name(&name);
        let session_name = self.session_manager().get_session_name();
        if session_name.as_deref() != Some(name.as_str()) {
            // TS：`JSON.stringify(sessionName)`，`undefined` 渲染成字面量 `undefined`。
            let normalized = match session_name.as_deref() {
                Some(session_name) => json_stringify(session_name),
                None => "undefined".to_string(),
            };
            self.show_warning(&format!(
                "Session name was normalized from {} to {}",
                json_stringify(&name),
                normalized
            ));
        }
        let display_name = session_name.unwrap_or_else(|| name.clone());
        let chat = Rc::clone(&self.inner.borrow().chat_container);
        {
            let mut chat = chat.borrow_mut();
            chat.add_child(Box::new(Spacer::new()));
            chat.add_child(Box::new(ThemedText::with_padding(
                Box::new(move || {
                    fg(
                        kiana_theme::theme_tokens::ThemeColor::Dim,
                        &format!("Session name set: {display_name}"),
                    )
                }),
                1,
                0,
            )));
        }
        self.request_render();
    }

    /// `promptForMissingSessionCwd(error)`（`/import` 的本地副本：im-runtime 的同名方法是私有的）。
    ///
    /// 确认文案由 `format_missing_session_cwd_prompt` 生成（含旧 cwd 与将要继续使用的
    /// cwd）。只有用户确认才返回 `Some(fallback_cwd)`；拒绝返回 `None`，调用方按取消处理。
    async fn prompt_for_missing_import_cwd(
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

    /// `ui` 的 `Rc<RefCell<dyn Tui>>` 适配句柄（`/share`、`/bug` 的上下文参数）。
    ///
    /// 每次都新建一个 `TuiArcAdapter` 包住当前渲染器的 `InteractiveTuiReference`，
    /// 因此拿到的句柄只在本次命令期间有效；渲染器仍由 `inner` 持有 `Rc`，不会悬空。
    fn tui_handle(&self) -> Rc<std::cell::RefCell<dyn kiana_tui::tui::Tui>> {
        let renderer = Rc::clone(&self.inner.borrow().renderer);
        Rc::new(std::cell::RefCell::new(TuiArcAdapter::new(
            InteractiveTuiReference::new(renderer),
        )))
    }

    /// `handleExportCommand` 的测试接缝。
    #[doc(hidden)]
    pub async fn test_handle_export_command(&mut self, text: &str) {
        self.handle_export_command(text).await;
    }

    /// `handleImportCommand` 的测试接缝。
    #[doc(hidden)]
    pub async fn test_handle_import_command(&mut self, text: &str) {
        self.handle_import_command(text).await;
    }

    /// `handleCopyCommand` 的测试接缝：把两个选项拆成参数传入（`CopyOptions` 是
    /// crate 内类型，集成测试无法直接构造）。
    #[doc(hidden)]
    pub async fn test_handle_copy_command(
        &mut self,
        flash_confirmation: bool,
        prefer_selection: bool,
    ) {
        self.handle_copy_command(super::CopyOptions {
            flash_confirmation,
            prefer_selection,
        })
        .await;
    }

    /// `handleNameCommand` 的测试接缝。
    #[doc(hidden)]
    pub fn test_handle_name_command(&mut self, text: &str) {
        self.handle_name_command(text);
    }

    /// `handleBugCommand` 拒绝路径的测试接缝。
    #[doc(hidden)]
    pub fn test_finish_bug_command(
        &mut self,
        result: Result<(), kiana_session_core::session_manager::SessionManagerError>,
    ) {
        self.finish_bug_command(result);
    }
}

/// `JSON.stringify(value)`（字符串）。
///
/// `serde_json` 与 `JSON.stringify` 对本函数接受的 `&str` 输出一致（转义引号、反斜杠、
/// 控制字符）。序列化本身不会失败，`unwrap_or_else` 只是兜底，用最小合法 JSON 字符串
/// 保证提示文案不会 panic。
fn json_stringify(value: &str) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| format!("\"{value}\""))
}

/// `getPathCommandArgument(text, command)`：命令后第一个参数（支持引号）。
///
/// 解析规则（与 §2.50.14 及测试 `enforces_command_token_boundaries` 一致）：
/// - `text` 等于命令本身，或以“命令 + 一个空格”开头；否则返回 `None`。因此
///   `/important …` 不会被当成 `/import` 的参数，`/import\t…`（制表符分隔）也不会匹配；
/// - 命令后的前导 JS 空白被剥掉；剩余为空 → `None`；
/// - 首个字符是 `"` 或 `'` 时取到下一个同种引号为止（找不到闭合引号 → `None`），
///   返回**去掉引号**的内容；
/// - 否则截到第一个 JS 空白（`is_js_whitespace`）之前，整段返回，因此不含引号的路径
///   里可以出现撇号（`john's/…`）。
///
/// 返回新分配的 `String`（调用方需要持有它跨 `await`）。不校验路径是否存在、也不做
/// 任何规范化，这些交给下游的导出/导入函数。
pub(crate) fn get_path_command_argument(text: &str, command: &str) -> Option<String> {
    if text == command {
        return None;
    }
    let prefix = format!("{command} ");
    let rest = text.strip_prefix(&prefix)?;
    let args_string = js_trim_start(rest);
    if args_string.is_empty() {
        return None;
    }

    let first_char = args_string.chars().next()?;
    if first_char == '"' || first_char == '\'' {
        let rest = &args_string[first_char.len_utf8()..];
        let closing_quote_index = rest.find(first_char)?;
        return Some(rest[..closing_quote_index].to_string());
    }

    let first_whitespace_index = args_string
        .char_indices()
        .find(|(_, ch)| is_js_whitespace(*ch))
        .map(|(index, _)| index);
    match first_whitespace_index {
        None => Some(args_string.to_string()),
        Some(index) => Some(args_string[..index].to_string()),
    }
}
