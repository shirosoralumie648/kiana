//! `interactive-mode.ts` 的 `/changelog` `/hotkeys` `/new` `/debug` `/arminsayshi`
//! `/dementedelves`（§2.50.16）。
//!
//! # 这些命令的共同点
//!
//! 除了 `/new`（替换会话）与 `/debug`（写日志文件）外，它们都只往 chat 容器追加
//! 若干 UI 组件（`Spacer` + `DynamicBorder` + `Markdown`/自定义组件）然后
//! `request_render()`。`/new` 与 `/debug` 的失败路径见下文。
//!
//! # 两条需要注意的执行约束
//!
//! - **异步提示由事件驱动**：`/new` 成功时本文件打印 “✓ New session started”；失败时
//!   调 `handle_fatal_runtime_error`（记录崩溃并退出进程）。`new_session` 返回
//!   `cancelled = true`（扩展的 before-switch 处理器拦下替换）时什么也不做。
//! - **不能在输入分发期间重入渲染器**：`/debug` 与 `/arminsayshi` 都要访问渲染器，
//!   而按键回调可能已经在渲染器的借用栈上。两者都先 `try_borrow_mut`，失败就
//!   `spawn_local` 重新调用自身（见 `commands/mod.rs` 模块文档的“延后一拍”一节）。
//!
//! # 彩蛋命令
//!
//! `/arminsayshi` 先尝试 `play_armin_3d`：全屏模式下播放动画、已有 overlay 或播放
//! 成功时都返回 `true`，此时不再追加文本组件；主屏下返回 `false`，改追加
//! `ArminComponent`。`/dementedelves` 只追加 `EarendilAnnouncementComponent`。
//!
//! # 调用方与测试
//!
//! 斜杠命令由 `commands/mod.rs` 的 `dispatch_submit` 分派；`/debug` 还会被
//! `setup_key_handlers` 里注册的调试回调 `ui.onDebug` 直接调用（同一函数，两条入口）。
//! `test_*` 方法供 `tests/interactive_mode_commands.rs` 调用，只做转发。

use std::rc::Rc;

use kiana_core::changelog::{get_changelog_path, normalize_changelog_links, parse_changelog};
use kiana_core::config::{APP_NAME, get_debug_log_path};
use kiana_json::js_trim;
use kiana_theme::ThemeColor;
use kiana_tui::components::markdown::Markdown;
use kiana_tui::components::spacer::Spacer;
use kiana_ui_kit::dynamic_border::DynamicBorder;
use kiana_ui_kit::keybinding_hints::{KeyTextFormatOptions, format_key_text, key_display_text};
use kiana_ui_kit::themed_text::ThemedText;

use crate::components::armin::ArminComponent;
use crate::components::earendil_announcement::EarendilAnnouncementComponent;

use super::super::InteractiveMode;

/// 按主题给文本着色/加粗；主题未初始化时 panic（TS 在同样位置抛错）。
fn fg(color: ThemeColor, text: &str) -> String {
    kiana_theme::THEME
        .fg(color, text)
        .expect("theme must be initialized before rendering (TS throws here)")
}

/// 按主题加粗；主题未初始化时 panic。
fn bold(text: &str) -> String {
    kiana_theme::THEME
        .bold(text)
        .expect("theme must be initialized before rendering (TS throws here)")
}

impl InteractiveMode {
    /// `handleChangelogCommand()`。
    ///
    /// 读取 package 目录下的 `CHANGELOG.md`（`get_changelog_path`）→ **反转**
    /// `parse_changelog` 返回的文件顺序 → 逐条把相对链接改写成锁定 tag 的 GitHub 链接
    /// → 以空行连接成一段 Markdown 渲染。
    ///
    /// 条目为空（文件缺失或读取失败时 `parse_changelog` 都返回空数组）时渲染固定文案
    /// `No changelog entries found.`，不报错。
    pub(crate) fn handle_changelog_command(&mut self) {
        let changelog_path = get_changelog_path();
        let mut all_entries = parse_changelog(&changelog_path);

        let changelog_markdown = if all_entries.is_empty() {
            "No changelog entries found.".to_string()
        } else {
            all_entries.reverse();
            all_entries
                .iter()
                .map(|entry| normalize_changelog_links(&entry.content, entry))
                .collect::<Vec<_>>()
                .join("\n\n")
        };

        let markdown_theme = self.get_markdown_theme_with_settings();
        let chat = Rc::clone(&self.inner.borrow().chat_container);
        {
            let mut chat = chat.borrow_mut();
            chat.add_child(Box::new(Spacer::new()));
            chat.add_child(Box::new(DynamicBorder::default()));
            chat.add_child(Box::new(ThemedText::with_padding(
                Box::new(|| bold(&fg(ThemeColor::Accent, "What's New"))),
                1,
                0,
            )));
            chat.add_child(Box::new(Spacer::new()));
            chat.add_child(Box::new(Markdown::new(
                &changelog_markdown,
                1,
                1,
                markdown_theme,
            )));
            chat.add_child(Box::new(DynamicBorder::default()));
        }
        self.request_render();
    }

    /// `getAppKeyDisplay(action)`。
    ///
    /// 与 `get_editor_key_display` 的实现相同：都直接查同一张全局键位表
    /// （`kiana-ui-kit` 的 `key_display_text`），两个名字沿用 TS 的拆分。
    fn get_app_key_display(&self, action: &'static str) -> String {
        key_display_text(action)
    }

    /// `getEditorKeyDisplay(action)`。见 `get_app_key_display`。
    fn get_editor_key_display(&self, action: &'static str) -> String {
        key_display_text(action)
    }

    /// `handleHotkeysCommand()`。
    ///
    /// 按当前生效键位配置把各动作渲染成 Markdown 表格（Navigation / Editing / Other
    /// 三张表），再追加一张 Extensions 表——仅当 `get_shortcuts` 返回非空时。模板字符串
    /// 首尾各有一个换行，渲染前用 `js_trim` 去掉，与 TS 的 `hotkeys.trim()` 一致。
    /// 表格逐字对齐 §2.50.16 的模板（测试 `hotkeys_command_lists_the_bash_shortcuts`
    /// 断言了其中几张表的关键行），改动文案需同步更新测试。
    ///
    /// `(Ctrl+Enter on Windows Terminal)` 后缀只在 `cfg!(windows)` 时出现，对应 TS 的
    /// `win32` 判断。Extensions 表里没有描述时退化为该快捷键所属扩展的路径
    /// （`shortcut.extension_path`）。
    pub(crate) fn handle_hotkeys_command(&mut self) {
        let cursor_up = self.get_editor_key_display("tui.editor.cursorUp");
        let cursor_down = self.get_editor_key_display("tui.editor.cursorDown");
        let cursor_left = self.get_editor_key_display("tui.editor.cursorLeft");
        let cursor_right = self.get_editor_key_display("tui.editor.cursorRight");
        let cursor_word_left = self.get_editor_key_display("tui.editor.cursorWordLeft");
        let cursor_word_right = self.get_editor_key_display("tui.editor.cursorWordRight");
        let cursor_line_start = self.get_editor_key_display("tui.editor.cursorLineStart");
        let cursor_line_end = self.get_editor_key_display("tui.editor.cursorLineEnd");
        let jump_forward = self.get_editor_key_display("tui.editor.jumpForward");
        let jump_backward = self.get_editor_key_display("tui.editor.jumpBackward");
        let page_up = self.get_editor_key_display("tui.editor.pageUp");
        let page_down = self.get_editor_key_display("tui.editor.pageDown");

        let submit = self.get_editor_key_display("tui.input.submit");
        let new_line = self.get_editor_key_display("tui.input.newLine");
        let delete_word_backward = self.get_editor_key_display("tui.editor.deleteWordBackward");
        let delete_word_forward = self.get_editor_key_display("tui.editor.deleteWordForward");
        let delete_to_line_start = self.get_editor_key_display("tui.editor.deleteToLineStart");
        let delete_to_line_end = self.get_editor_key_display("tui.editor.deleteToLineEnd");
        let yank = self.get_editor_key_display("tui.editor.yank");
        let yank_pop = self.get_editor_key_display("tui.editor.yankPop");
        let undo = self.get_editor_key_display("tui.editor.undo");
        let tab = self.get_editor_key_display("tui.input.tab");

        let interrupt = self.get_app_key_display("app.interrupt");
        let clear = self.get_app_key_display("app.clear");
        let exit = self.get_app_key_display("app.exit");
        let suspend = self.get_app_key_display("app.suspend");
        let cycle_thinking_level = self.get_app_key_display("app.thinking.cycle");
        let cycle_model_forward = self.get_app_key_display("app.model.cycleForward");
        let select_model = self.get_app_key_display("app.model.select");
        let expand_tools = self.get_app_key_display("app.tools.expand");
        let toggle_thinking = self.get_app_key_display("app.thinking.toggle");
        let external_editor = self.get_app_key_display("app.editor.external");
        let cycle_model_backward = self.get_app_key_display("app.model.cycleBackward");
        let copy_message = self.get_app_key_display("app.message.copy");
        let follow_up = self.get_app_key_display("app.message.followUp");
        let dequeue = self.get_app_key_display("app.message.dequeue");
        let paste_image = self.get_app_key_display("app.clipboard.pasteImage");

        let mut hotkeys = format!(
            "
**Navigation**
| Key | Action |
|-----|--------|
| `{cursor_up}` / `{cursor_down}` / `{cursor_left}` / `{cursor_right}` | Move cursor / browse history |
| `{cursor_word_left}` / `{cursor_word_right}` | Move by word |
| `{cursor_line_start}` | Start of line |
| `{cursor_line_end}` | End of line |
| `{jump_forward}` | Jump forward to character |
| `{jump_backward}` | Jump backward to character |
| `{page_up}` / `{page_down}` | Scroll by page |

**Editing**
| Key | Action |
|-----|--------|
| `{submit}` | Send message |
| `{new_line}` | New line{} |
| `{delete_word_backward}` | Delete word backwards |
| `{delete_word_forward}` | Delete word forwards |
| `{delete_to_line_start}` | Delete to start of line |
| `{delete_to_line_end}` | Delete to end of line |
| `{yank}` | Paste the most-recently-deleted text |
| `{yank_pop}` | Cycle through the deleted text after pasting |
| `{undo}` | Undo |

**Other**
| Key | Action |
|-----|--------|
| `{tab}` | Path completion / accept autocomplete |
| `{interrupt}` | Cancel autocomplete / abort streaming |
| `{clear}` | Clear editor (first) / exit (second) |
| `{exit}` | Exit (when editor is empty) |
| `{suspend}` | Suspend to background |
| `{cycle_thinking_level}` | Cycle thinking level |
| `{cycle_model_forward}` / `{cycle_model_backward}` | Cycle models |
| `{select_model}` | Open model selector |
| `{expand_tools}` | Toggle tool output expansion |
| `{toggle_thinking}` | Toggle thinking block visibility |
| `{external_editor}` | Edit message in external editor |
| `{copy_message}` | Copy selection or last assistant message |
| `{follow_up}` | Queue follow-up message |
| `{dequeue}` | Restore queued messages |
| `{paste_image}` | Paste files on macOS, images, or text from clipboard |
| `/` | Slash commands |
| `!` | Run bash command |
| `!!` | Run bash command (excluded from context)
",
            if cfg!(windows) {
                " (Ctrl+Enter on Windows Terminal)"
            } else {
                ""
            }
        );

        // 扩展注册的快捷键。
        let extension_runner = self.session().extension_runner();
        let keybindings = self.inner.borrow().keybindings.get_effective_config();
        let shortcuts = extension_runner.get_shortcuts(&keybindings);
        if !shortcuts.is_empty() {
            hotkeys.push_str(
                "
**Extensions**
| Key | Action |
|-----|--------|
",
            );
            for (key, shortcut) in &shortcuts {
                let description = shortcut
                    .description
                    .clone()
                    .unwrap_or_else(|| shortcut.extension_path.clone());
                let key_display = format_key_text(key, KeyTextFormatOptions { capitalize: true });
                hotkeys.push_str(&format!("| `{key_display}` | {description} |\n"));
            }
        }

        let markdown_theme = self.get_markdown_theme_with_settings();
        let chat = Rc::clone(&self.inner.borrow().chat_container);
        {
            let mut chat = chat.borrow_mut();
            chat.add_child(Box::new(Spacer::new()));
            chat.add_child(Box::new(DynamicBorder::default()));
            chat.add_child(Box::new(ThemedText::with_padding(
                Box::new(|| bold(&fg(ThemeColor::Accent, "Keyboard Shortcuts"))),
                1,
                0,
            )));
            chat.add_child(Box::new(Spacer::new()));
            chat.add_child(Box::new(Markdown::new(
                js_trim(&hotkeys),
                1,
                1,
                markdown_theme,
            )));
            chat.add_child(Box::new(DynamicBorder::default()));
        }
        self.request_render();
    }

    /// `handleClearCommand()`（`/new`）。
    ///
    /// 先清状态指示器，再请求 runtime 新建会话（`NewSessionOptions` 为 `None`，即使用
    /// 默认选项；实际参数与是否保留某些状态由 `agent_session_runtime::new_session`
    /// 决定）。三个结局：
    /// - `cancelled = true`：静默（`new_session` 内部在 before-switch 被拦下时返回该值）。
    /// - 成功：在 chat 里追加 “✓ New session started”。
    /// - 失败：`handle_fatal_runtime_error`，记录崩溃后退出进程。
    pub(crate) async fn handle_clear_command(&mut self) {
        self.clear_status_indicator(None);
        let runtime = Rc::clone(&self.inner.borrow().runtime_host);
        let result = runtime.new_session(None).await;
        match result {
            Ok(result) if result.cancelled => {}
            Ok(_) => {
                let chat = Rc::clone(&self.inner.borrow().chat_container);
                {
                    let mut chat = chat.borrow_mut();
                    chat.add_child(Box::new(Spacer::new()));
                    chat.add_child(Box::new(ThemedText::with_padding(
                        Box::new(|| fg(ThemeColor::Accent, "✓ New session started")),
                        1,
                        1,
                    )));
                }
                self.request_render();
            }
            Err(error) => {
                self.handle_fatal_runtime_error(
                    "Failed to create session",
                    &error.to_string(),
                    None,
                )
                .await;
            }
        }
    }

    /// `handleDebugCommand()`。
    ///
    /// 把“当前渲染出的所有行（附可见宽度）+ 会话全部消息”写进固定的调试日志文件
    /// （`<agent-dir>/kiana-debug.log`，由 `get_debug_log_path` 决定），没有日期文件名，
    /// 每次覆盖。即使没有任何会话消息也照写。
    ///
    /// **重入保护**：本方法可能由 `TuiBase::handle_terminal_input`（TS
    /// `handleTerminalInput`）里的 `ui.onDebug` 触发，此时
    /// 渲染器已被可变借用。先 `try_borrow_mut`；失败则把整个动作 `spawn_local` 延后，
    /// 由下一次调度重跑本函数（仍被借用就继续延后，直到借用释放）。成功后才读取终端
    /// 尺寸并渲染，因此日志里的行内容是本次重绘的真实结果。
    ///
    /// **写文件失败 = 崩溃**：TS 里 `mkdirSync`/`writeFileSync` 没有 try/catch，异常会
    /// 经 `uncaughtException` 退出。这里用 `panic_any(NodeFsError)` 复刻：panic hook
    /// 按未捕获异常记录并退出，横幅打印 Node 风格的 `error.message`。创建父目录时
    /// 只在父路径非空且失败时才算错——`create_dir_all` 对已存在目录返回 `Ok`。
    pub(crate) fn handle_debug_command(&mut self) {
        // `ui.onDebug` 在 `TuiBase::handleInput` 里触发（渲染器已被借用）：延后一拍，
        // 避免在输入分发期间重入渲染器。
        if self.inner.borrow().renderer.try_borrow_mut().is_err() {
            let weak = self.downgrade();
            tokio::task::spawn_local(async move {
                if let Some(inner) = weak.upgrade() {
                    InteractiveMode { inner }.handle_debug_command();
                }
            });
            return;
        }
        let terminal = self.inner.borrow().ui.terminal();
        let (width, height) = {
            let terminal = terminal.borrow();
            (terminal.columns(), terminal.rows())
        };
        let all_lines = self.inner.borrow_mut().ui.render(width);

        // 日志头：时间戳、终端尺寸、总行数；随后是逐行转义后的渲染结果，再是会话的
        // JSONL 消息。时间戳格式 `%Y-%m-%dT%H:%M:%S%.3fZ` 对应 TS `toISOString()`
        // 的毫秒精度（chrono 的 `%.3f` 补足三位小数）。
        let debug_log_path = get_debug_log_path();
        let mut debug_lines: Vec<String> = Vec::new();
        debug_lines.push(format!(
            "Debug output at {}",
            chrono::Utc::now().format("%Y-%m-%dT%H:%M:%S%.3fZ")
        ));
        debug_lines.push(format!("Terminal: {width}x{height}"));
        debug_lines.push(format!("Total lines: {}", all_lines.len()));
        debug_lines.push(String::new());
        debug_lines.push("=== All rendered lines with visible widths ===".to_string());
        for (index, line) in all_lines.iter().enumerate() {
            let visible = kiana_tui::text::visible_width(line);
            let escaped = serde_json::to_string(line).unwrap_or_default();
            debug_lines.push(format!("[{index}] (w={visible}) {escaped}"));
        }
        debug_lines.push(String::new());
        debug_lines.push("=== Agent messages (JSONL) ===".to_string());
        for message in self.session().messages() {
            debug_lines.push(serde_json::to_string(&message).unwrap_or_default());
        }
        debug_lines.push(String::new());
        let debug_data = debug_lines.join("\n");

        // TS 的 `fs.mkdirSync` / `fs.writeFileSync` 没有 try/catch：失败即抛出，由
        // `uncaughtException` 处理器走崩溃路径（不会显示成功消息）。Rust 用携带 Node
        // 文案与原始 `io::Error` 的 panic 复刻同一行为（panic hook 会按未捕获异常记录并
        // 退出；横幅打印的也是 Node 的 `error.message` 而不是 `Box<dyn Any>`）。
        if let Some(parent) = debug_log_path.parent()
            && !parent.as_os_str().is_empty()
            && let Err(error) = std::fs::create_dir_all(parent)
        {
            std::panic::panic_any(crate::interactive_mode::lifecycle::NodeFsError::new(
                crate::session_share::node_mkdir_error(&error, parent),
                error,
            ));
        }
        if let Err(error) = std::fs::write(&debug_log_path, debug_data) {
            std::panic::panic_any(crate::interactive_mode::lifecycle::NodeFsError::new(
                crate::session_share::node_write_file_error(&error, &debug_log_path),
                error,
            ));
        }

        let path_label = debug_log_path.to_string_lossy().into_owned();
        let chat = Rc::clone(&self.inner.borrow().chat_container);
        {
            let mut chat = chat.borrow_mut();
            chat.add_child(Box::new(Spacer::new()));
            chat.add_child(Box::new(ThemedText::with_padding(
                Box::new(move || {
                    format!(
                        "{}\n{}",
                        fg(ThemeColor::Accent, "✓ Debug log written"),
                        fg(ThemeColor::Muted, &path_label)
                    )
                }),
                1,
                1,
            )));
        }
        self.request_render();
    }

    /// `handleArminSaysHi()`。
    ///
    /// 先尝试交给 3D 彩蛋：`play_armin_3d` 只在全屏渲染器下播放（并返回 `true`）；
    /// 主屏返回 `false`，此时退回追加文本版 `ArminComponent`。已有 overlay 时它也返回
    /// `true`（视为已处理），并不会播放。
    ///
    /// **重入保护**：与 `handle_debug_command` 同理，输入分发路径可能已借用
    /// 渲染器；`try_borrow_mut` 失败就 `spawn_local` 延后重跑。借用成功后立即 `drop`，
    /// 后续追加组件不再持有渲染器借用。
    pub(crate) fn handle_armin_says_hi(&mut self) {
        let renderer = Rc::clone(&self.inner.borrow().renderer);
        // 输入分发路径可能已经借用了渲染器（`TuiBase::handleInput`）：延后一拍再播放。
        let Ok(mut renderer_borrow) = renderer.try_borrow_mut() else {
            let weak = self.downgrade();
            tokio::task::spawn_local(async move {
                if let Some(inner) = weak.upgrade() {
                    InteractiveMode { inner }.handle_armin_says_hi();
                }
            });
            return;
        };
        let played = crate::components::easter_egg_3d::play_armin_3d(&mut renderer_borrow);
        drop(renderer_borrow);
        if played {
            return;
        }
        let request_render = self.request_render_callback();
        let chat = Rc::clone(&self.inner.borrow().chat_container);
        {
            let mut chat = chat.borrow_mut();
            chat.add_child(Box::new(Spacer::new()));
            chat.add_child(Box::new(ArminComponent::new(Some(request_render))));
        }
        self.request_render();
    }

    /// `handleDementedDelves()`。
    ///
    /// 只需向 chat 追加 `EarendilAnnouncementComponent`，不触碰渲染器，因此没有延后
    /// 逻辑；追加后请求重绘。
    pub(crate) fn handle_demented_delves(&mut self) {
        let chat = Rc::clone(&self.inner.borrow().chat_container);
        {
            let mut chat = chat.borrow_mut();
            chat.add_child(Box::new(Spacer::new()));
            chat.add_child(Box::new(EarendilAnnouncementComponent::new()));
        }
        self.request_render();
    }

    /// `handleChangelogCommand()` 的测试接缝。
    #[doc(hidden)]
    pub fn test_handle_changelog_command(&mut self) {
        self.handle_changelog_command();
    }

    /// `handleHotkeysCommand()` 的测试接缝。
    #[doc(hidden)]
    pub fn test_handle_hotkeys_command(&mut self) {
        self.handle_hotkeys_command();
    }

    /// `handleDebugCommand()` 的测试接缝。
    #[doc(hidden)]
    pub fn test_handle_debug_command(&mut self) {
        self.handle_debug_command();
    }

    /// `handleDementedDelves()` 的测试接缝。
    #[doc(hidden)]
    pub fn test_handle_demented_delves(&mut self) {
        self.handle_demented_delves();
    }

    /// `handleClearCommand()` 的测试接缝。
    #[doc(hidden)]
    pub async fn test_handle_clear_command(&mut self) {
        self.handle_clear_command().await;
    }

    /// 应用名（`/hotkeys` 不直接使用，保留给测试做文案断言）。
    #[doc(hidden)]
    pub fn test_app_name(&self) -> &'static str {
        APP_NAME
    }
}
