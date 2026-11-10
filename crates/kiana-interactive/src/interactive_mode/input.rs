//! `interactive-mode.ts` 的输入处理：启动期提交、剪贴板粘贴、Ctrl+C/Ctrl+D/Ctrl+Z
//! （§2.50.7 / §2.50.8）。
//!
//! # 这个文件负责什么
//!
//! 编辑器按键分发之后、提交进会话之前的用户输入入口，由 `impl InteractiveMode` 的
//! 各 `handle_*` 方法组成，调用方有两类：
//!
//! * `interactive_mode.rs` 的 `init()`：`app.clear`（Ctrl+C）、`set_on_ctrl_d`、
//!   `on_submit`（启动期提交）；
//! * 右键粘贴回调：由构造器 `new()`（interactive_mode.rs:1539-1556）注册并传入
//!   `create_interactive_tui`，切 TUI 模式时随渲染器重建重新挂上。
//! * `commands` 子模块的按键绑定：粘贴（Ctrl+V）、Alt+Enter（follow-up）、
//!   dequeue、外置编辑器等动作处理器。
//!
//! # 主要流程
//!
//! * 粘贴有两条独立路径：Ctrl+V（`handle_clipboard_paste`）按“文件路径 → 图片（写
//!   临时文件）→ 文本”三级回退，把结果插入编辑器；右键（`handle_right_click_paste`）
//!   只读文本，且以括号粘贴转义序列投递给**当前焦点组件**（可能是编辑器，也可能是
//!   选择器等任何可获得焦点的组件）。两条路径的剪贴板读取都经 `ClipboardSource`
//!   trait，测试注入假实现。
//! * Ctrl+C 是带时间窗的两段式：500ms 内第二次按下才退出，第一次只清空编辑器并
//!   记录时间（状态存 `last_sigint_time`）；Ctrl+D 直接退出。
//! * 退出本身由 `lifecycle` 子模块的 `shutdown` 完成，本文件只负责触发。
//!
//! # 注意事项
//!
//! * 与 TS 的差异点都用行内注释标出（如“TS 在 resolve 之后才读 cursor”）；
//!   其中剪贴板分隔空白用的是 ECMAScript 的 `\s` 集合而非 Rust 的
//!   `char::is_whitespace`，见 `utf16_unit_is_whitespace` 与文件内测试。
//! * 剪贴板读取是 `async` 但不跨线程（`?Send`）：读取发生在 UI LocalSet 上，
//!   与组件操作共享同一执行流。
//! * 所有用户可见错误（粘贴失败、外置编辑器 I/O 失败）不返回 `Result`，而是
//!   `show_error` 或 `uncaught_crash`（`lifecycle` 子模块，对齐 TS 的
//!   unhandled rejection → uncaughtException 路径）。

use std::rc::Rc;

use kiana_core::clipboard::ClipboardTestIo;
use kiana_core::image_process::{
    ClipboardImage, extension_for_image_mime_type, read_clipboard_image_with,
};

use crate::external_editor::{
    ExternalEditorOptions, ExternalEditorResult, edit_in_external_editor,
};

use super::{CompactionQueueMode, InteractiveMode};

/// 剪贴板读取源（测试注入点；生产实现见 `ProcessClipboardSource`）。
///
/// 三个方法对应 TS `ClipboardManager` 的三个读取入口。错误统一用 `String` 承载
/// （内容是给用户看的文案），`Ok(None)` 表示“剪贴板里没有该类内容”，与读取失败
/// 是两个不同的结果。
#[allow(clippy::double_must_use)]
#[async_trait::async_trait(?Send)]
pub trait ClipboardSource {
    /// `readClipboardFilePaths()`。
    async fn read_file_paths(&self) -> Result<Option<Vec<String>>, String>;
    /// `readClipboardImage()`。
    async fn read_image(&self) -> Result<Option<ClipboardImage>, String>;
    /// `readClipboardText()`。
    async fn read_text(&self) -> Result<Option<String>, String>;
}

/// 真实进程剪贴板。
pub(crate) struct ProcessClipboardSource;

#[async_trait::async_trait(?Send)]
impl ClipboardSource for ProcessClipboardSource {
    async fn read_file_paths(&self) -> Result<Option<Vec<String>>, String> {
        let io = ClipboardTestIo::from_process();
        kiana_core::clipboard::read_clipboard_file_paths_with(&io)
            .await
            .map_err(|error| error.to_string())
    }

    async fn read_image(&self) -> Result<Option<ClipboardImage>, String> {
        let io = ClipboardTestIo::from_process();
        read_clipboard_image_with(&io)
            .await
            .map_err(|error| error.to_string())
    }

    async fn read_text(&self) -> Result<Option<String>, String> {
        let io = ClipboardTestIo::from_process();
        Ok(kiana_core::clipboard::read_clipboard_text_with(&io).await)
    }
}

/// `/\p{Cc}/u`。
///
/// 用途：粘贴的文件路径里若含控制字符（可包括转义序列前缀 ESC），视为可疑输入
/// 直接报错，而不是插入编辑器。
fn has_control_characters(value: &str) -> bool {
    static REGEX: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(r"\p{Cc}").expect("control character regex is valid")
    });
    REGEX.is_match(value)
}

/// JS 的 `/\s/.test(oneCodeUnit)`；索引越界（JS 的 `undefined`）返回 `None`。
///
/// 按 ECMAScript 的 `\s` 集合判定（[`kiana_tui::text::is_js_whitespace`]）：含 U+FEFF，
/// **不含** U+0085（Rust 的 `char::is_whitespace` 会多出它）；代理项按非空白处理。
fn utf16_unit_is_whitespace(line: &str, index: usize) -> Option<bool> {
    let units: Vec<u16> = line.encode_utf16().collect();
    let unit = *units.get(index)?;
    Some(match char::decode_utf16([unit]).next() {
        Some(Ok(character)) => kiana_tui::text::is_js_whitespace(character),
        _ => false,
    })
}

/// `handleClipboardPaste` 的路径拼接部分（`cursor` 为 `None` 等价于编辑器没有
/// `getCursor()`：TS 里 `currentLine` 为空串，因此不加任何空格）。
///
/// 拼接规则（对应 TS）：bash 模式下路径逐个引号包裹、空格分隔；普通模式换行分隔。
/// 再按光标前后字符决定补不补空格——**前后字符按 JS 语义取**：不是空白才补空格，
/// 越界（越出该行）视为“不需要补”；代理项按非空白处理（与 JS 一致，会补空格），
/// 与 `utf16_unit_is_whitespace` 的文档一致。
fn format_file_path_paste(
    file_paths: &[String],
    editor_text: &str,
    cursor: Option<(usize, usize)>,
    is_bash_mode: bool,
) -> String {
    let paths = if is_bash_mode {
        file_paths
            .iter()
            .map(|path| super::quote_if_needed(path))
            .collect::<Vec<_>>()
            .join(" ")
    } else {
        file_paths.join("\n")
    };
    let Some((line, col)) = cursor else {
        return paths;
    };
    let lines: Vec<&str> = editor_text.split('\n').collect();
    let current_line = lines.get(line).copied().unwrap_or("");
    // `const characterBeforeCursor = cursor && cursor.col > 0 ? currentLine[cursor.col - 1] : ""`
    let character_before_cursor = if col > 0 {
        utf16_unit_is_whitespace(current_line, col - 1)
    } else {
        None
    };
    // `const characterAfterCursor = cursor ? currentLine[cursor.col] : ""`
    let character_after_cursor = utf16_unit_is_whitespace(current_line, col);
    let leading_space = match character_before_cursor {
        Some(false) => " ",
        _ => "",
    };
    let trailing_space = match character_after_cursor {
        Some(false) => " ",
        _ => "",
    };
    format!("{leading_space}{paths}{trailing_space}")
}

/// 剪贴板没有文件路径时的回退：图片（写临时文件）→ 文本。
///
/// 图片落盘到 `temp_dir`（文件名带随机 UUID 防碰撞），返回文件路径字符串——编辑器
/// 里插入的是“这个文件被当作附件引用”的路径文本。写盘失败转成 Node 风格的错误文案
/// （`session_share::node_write_file_error`），与 TS `fs.writeFileSync` 的异常消息
/// 逐字对齐（见文件内测试 `image_paste_reports_the_node_fs_error_shape`）。
async fn paste_image_or_text(
    source: &dyn ClipboardSource,
    temp_dir: &std::path::Path,
) -> Result<Option<String>, String> {
    let image = source.read_image().await?;
    if let Some(image) = image {
        let extension = extension_for_image_mime_type(&image.mime_type).unwrap_or("png");
        let file_name = format!("kiana-clipboard-{}.{extension}", uuid::Uuid::new_v4());
        let file_path = temp_dir.join(file_name);
        // TS `fs.writeFileSync` 抛出 Node 的 fs 异常（`ENOSPC: no space left on device, write`）。
        std::fs::write(&file_path, &image.bytes)
            .map_err(|error| crate::session_share::node_write_file_error(&error, &file_path))?;
        return Ok(Some(file_path.to_string_lossy().into_owned()));
    }

    source.read_text().await
}

/// `handleClipboardPaste` 的纯逻辑部分（按三级回退顺序读取并决定插入文本）。
///
/// 独立成无状态函数是为了可测（不依赖 UI）；UI 层的 `handle_clipboard_paste_with`
/// 复制了同样的判定再插入编辑器。返回 `Ok(Some(text))` 表示应插入 `text`；
/// `Ok(None)` 表示剪贴板为空。文件路径含控制字符按错误处理（防转义序列注入）。
#[doc(hidden)]
pub async fn clipboard_paste_text(
    source: &dyn ClipboardSource,
    editor_text: &str,
    cursor: (usize, usize),
    is_bash_mode: bool,
    temp_dir: &std::path::Path,
) -> Result<Option<String>, String> {
    let file_paths = source.read_file_paths().await?;
    if let Some(file_paths) = file_paths {
        if file_paths.iter().any(|path| has_control_characters(path)) {
            return Err("Clipboard file path contains control characters".to_string());
        }
        return Ok(Some(format_file_path_paste(
            &file_paths,
            editor_text,
            Some(cursor),
            is_bash_mode,
        )));
    }

    paste_image_or_text(source, temp_dir).await
}

impl InteractiveMode {
    /// `handleStartupSubmit(text)`。
    ///
    /// 启动流程未完成时用户就按了回车：把文本放进编辑器并提示，等启动完成后由
    /// 用户再提交。不做排队——与压缩队列不同，启动期的输入没有重放机制。
    pub(crate) fn handle_startup_submit(&mut self, text: String) {
        self.inner.borrow().editor.set_text(&text);
        self.show_status("Startup is still in progress");
    }

    /// `handleCtrlC()`：500 ms 内第二次退出，否则清空编辑器。
    ///
    /// 状态机（`last_sigint_time` 初始为 0.0）：
    ///
    /// | 状态 | 触发 | 转移 |
    /// |---|---|---|
    /// | 空闲（距上次 < 500ms 不成立） | Ctrl+C | 记录时间、清空编辑器（给用户反悔机会） |
    /// | 记录后 500ms 内 | Ctrl+C | 异步走 `shutdown(false)` 退出进程 |
    ///
    /// 500ms 窗口过后 `last_sigint_time` 不再被清回 0：时间差 `now - last` 自然超过
    /// 阈值，等价于回到“空闲”。退出放在 `spawn_local` 里——按键处理器是同步回调，
    /// 不能直接 `await` shutdown。
    pub(crate) fn handle_ctrl_c(&mut self) {
        let now = kiana_ai::model_catalog::now_epoch_millis() as f64;
        let last = self.inner.borrow().last_sigint_time;
        if now - last < 500.0 {
            let weak = self.downgrade();
            tokio::task::spawn_local(async move {
                if let Some(inner) = weak.upgrade() {
                    let mut mode = InteractiveMode { inner };
                    mode.shutdown(false).await;
                }
            });
        } else {
            self.clear_editor();
            self.inner.borrow_mut().last_sigint_time = now;
        }
    }

    /// `handleCtrlD()`。
    ///
    /// Ctrl+D 即退出，没有 Ctrl+C 的确认窗口。
    pub(crate) fn handle_ctrl_d(&mut self) {
        let weak = self.downgrade();
        tokio::task::spawn_local(async move {
            if let Some(inner) = weak.upgrade() {
                let mut mode = InteractiveMode { inner };
                mode.shutdown(false).await;
            }
        });
    }

    /// `handleRightClickPaste()`。
    ///
    /// TS：`target = renderer.getFocusedComponent()`；`readClipboardText()`；若焦点未变且
    /// 有文本 → 以括号粘贴序列调用 **该组件** 的 `handleInput`。Rust 的组件身份用
    /// [`InteractiveRenderer::focused_component_identity`] 表达（焦点目标 + 组件地址），
    /// 因此焦点落在 overlay / 扩展自定义 UI / 选择器（任何可获得焦点的挂载组件）上时
    /// 同样会把粘贴投递给它。
    pub(crate) async fn handle_right_click_paste(&mut self) {
        self.handle_right_click_paste_with(&ProcessClipboardSource)
            .await;
    }

    /// [`Self::handle_right_click_paste`] 的剪贴板注入版本。
    ///
    /// 竞态防护：读剪贴板是 `await` 点，期间焦点可能已变。因此投递前重新查询焦点
    /// 身份，`target`/`address` 任一不同即放弃（TS 的引用相等判定在这里拆成两字段，
    /// 语义见 `tui_renderer.rs` 的 `FocusedComponentIdentity` 文档）。投递以括号粘贴
    /// 序列 `\x1b[200~...\x1b[201~` 包裹，让编辑器能把整段文本当作一次粘贴处理。
    pub(crate) async fn handle_right_click_paste_with(&mut self, source: &dyn ClipboardSource) {
        let renderer = Rc::clone(&self.inner.borrow().renderer);
        // TS: `const handleInput = target?.handleInput; if (!target || !handleInput) return;`
        let Some(target) = ({
            let renderer = renderer.borrow();
            renderer
                .focused_component_identity()
                .filter(|identity| identity.handles_input)
        }) else {
            return;
        };
        let Ok(Some(text)) = source.read_text().await else {
            return;
        };
        if text.is_empty() {
            return;
        }
        let sequence = format!("\x1b[200~{text}\x1b[201~");
        {
            let mut renderer = renderer.borrow_mut();
            // TS: `this.renderer.getFocusedComponent() !== target` → 放弃本次粘贴。
            // 地址相同即同一个组件分配（TS 的引用相等）；焦点目标同时校验，防止
            // 同一组件被移动到其他位置后误判。
            let current = renderer.focused_component_identity();
            if current.map(|identity| (identity.target, identity.address))
                != Some((target.target, target.address))
            {
                return;
            }
            if !renderer.dispatch_input_to_focused_component(&sequence) {
                return;
            }
        }
        self.request_render();
    }

    /// `handleClipboardPaste()`（Ctrl+V）。
    pub(crate) async fn handle_clipboard_paste(&mut self) {
        self.handle_clipboard_paste_with(&ProcessClipboardSource)
            .await;
    }

    /// [`Self::handle_clipboard_paste`] 的剪贴板注入版本。
    ///
    /// 三级回退的 UI 侧实现（与 [`clipboard_paste_text`] 同判定、多错误提示）：
    /// 读路径失败立即 `show_error` 返回；有路径则拼好后插入编辑器；没有路径再试
    /// 图片 / 文本回退，插入或报错。控制字符检查失败同样走 `show_error`。
    pub(crate) async fn handle_clipboard_paste_with(&mut self, source: &dyn ClipboardSource) {
        let file_paths = match source.read_file_paths().await {
            Ok(file_paths) => file_paths,
            Err(error) => {
                self.show_error(&format!("Failed to paste from clipboard: {error}"));
                return;
            }
        };
        if let Some(file_paths) = file_paths {
            // TS 在 `readClipboardFilePaths()` resolve 之后才读 `isBashMode` / `getCursor()`。
            // 这个顺序有可观察差别：await 期间用户可能切换 bash 模式或移动光标，
            // Rust 必须保持同一取值时点。
            if file_paths.iter().any(|path| has_control_characters(path)) {
                self.show_error(
                    "Failed to paste from clipboard: Clipboard file path contains control characters",
                );
                return;
            }
            let (editor_text, cursor, is_bash_mode) = {
                let inner = self.inner.borrow();
                let cursor = match &inner.editor {
                    super::ActiveEditor::Default { handle, .. } => {
                        Some(handle.borrow().get_cursor())
                    }
                    // 扩展编辑器没有 getCursor 接口：TS 此时 `cursor` 为 undefined，
                    // 不产生任何前后空格。
                    super::ActiveEditor::Custom { .. } => None,
                };
                (inner.editor.get_text(), cursor, inner.is_bash_mode)
            };
            let text = format_file_path_paste(&file_paths, &editor_text, cursor, is_bash_mode);
            self.inner.borrow().editor.insert_text_at_cursor(&text);
            self.request_render();
            return;
        }

        let temp_dir = std::env::temp_dir();
        match paste_image_or_text(source, &temp_dir).await {
            Ok(Some(text)) => {
                self.inner.borrow().editor.insert_text_at_cursor(&text);
                self.request_render();
            }
            Ok(None) => {}
            Err(error) => {
                self.show_error(&format!("Failed to paste from clipboard: {error}"));
            }
        }
    }

    /// `handleFollowUp()`（Alt+Enter）。
    ///
    /// 按“压缩中 → 流式中 → 空闲”三态分流：
    ///
    /// * 压缩中：扩展命令直接 `prompt`，普通消息进压缩队列
    ///   （压缩结束后由 `flush_compaction_queue` 重放）；
    /// * 流式中：带 `StreamingBehavior::FollowUp` 提交，进会话的 follow-up 队列；
    /// * 空闲：把文本交给默认编辑器的 `on_submit`（与手动回车同路径）。没有
    ///   `on_submit` 时**不清空编辑器**——否则输入会被静默丢弃。
    ///
    /// `on_submit` 取出后放回：它是编辑器的长期属性，不是一次性回调；先 `take` 是
    /// 为了绕过 `RefCell` 的借用冲突（调用回调可能重入编辑器）。
    pub(crate) async fn handle_follow_up(&mut self) {
        let text = self
            .inner
            .borrow()
            .editor
            .get_expanded_text()
            .trim()
            .to_string();
        if text.is_empty() {
            return;
        }
        let is_compacting = self.session().is_compacting();
        if is_compacting {
            if self.is_extension_command(&text) {
                // TS: `this.editor.addToHistory?.(text)`（活动编辑器）。
                self.inner.borrow().editor.add_to_history(&text);
                self.inner.borrow().editor.set_text("");
                let session = self.session();
                let _ = session.prompt(text, Default::default()).await;
            } else {
                self.queue_compaction_message(&text, CompactionQueueMode::FollowUp);
            }
            return;
        }
        if self.session().is_streaming() {
            self.inner.borrow().editor.add_to_history(&text);
            self.inner.borrow().editor.set_text("");
            let session = self.session();
            let _ = session
                .prompt(
                    text,
                    kiana_session::agent_session::PromptOptions {
                        streaming_behavior: Some(
                            kiana_ext_api::events::StreamingBehavior::FollowUp,
                        ),
                        ..Default::default()
                    },
                )
                .await;
            self.update_pending_messages_display();
            self.request_render();
        } else {
            // TS: `else if (this.editor.onSubmit) { this.editor.setText(""); this.editor.onSubmit(text); }`
            // —— 没有提交处理器时**不清空**编辑器（否则输入被静默丢弃）。
            let callback = {
                let editor_handle = self.default_editor();
                let mut editor_handle = editor_handle.borrow_mut();
                editor_handle.editor_mut().on_submit.take()
            };
            if let Some(mut callback) = callback {
                self.inner.borrow().editor.set_text("");
                callback(&text);
                // TS 的 onSubmit 是属性；这里调用后放回（保持后续提交可用）。
                self.default_editor().borrow_mut().editor_mut().on_submit = Some(callback);
            }
        }
    }

    /// `handleDequeue()`。
    ///
    /// 把会话 steering / follow-up 队列里的消息整体取回到编辑器（`\n\n` 连接），
    /// 让用户重新编辑后再提交。恢复条数决定提示文案（0 条时提示“没有排队消息”）。
    pub(crate) fn handle_dequeue(&mut self) {
        let restored = self.restore_queued_messages_to_editor(false, None);
        if restored == 0 {
            self.show_status("No queued messages to restore");
        } else {
            self.show_status(&format!(
                "Restored {restored} queued message{} to editor",
                if restored > 1 { "s" } else { "" }
            ));
        }
    }

    /// `handleOpenExternalEditor()`。
    ///
    /// 阶段：取编辑器命令与当前展开文本 → 停掉 TUI（把终端还给外部进程）→
    /// 阻塞式等待编辑器结束（`edit_in_external_editor`）→
    /// [`Self::finish_open_external_editor`] 恢复 TUI 并处理结果。
    pub(crate) async fn handle_open_external_editor(&mut self) {
        let editor_cmd = self.settings_manager().get_external_editor_command();
        let content = self.inner.borrow().editor.get_expanded_text();
        {
            let mut inner = self.inner.borrow_mut();
            inner.ui.stop(kiana_tui::tui::TuiStopOptions::default());
        }
        let result = edit_in_external_editor(ExternalEditorOptions {
            command: editor_cmd,
            content,
        })
        .await;
        self.finish_open_external_editor(result);
    }

    /// `handleOpenExternalEditor` 的 `try/finally` + 拒绝传播部分。
    ///
    /// 成功且 `Complete` 才回写文本（用户放弃编辑时保留原内容）；随后无论成败都
    /// 重启 TUI（TS 的 `finally` 语义）。I/O 失败对齐 TS 的 unhandled rejection：
    /// 不是 `show_error`，而是走 `uncaught_crash` 记崩溃并按崩溃路径退出。
    fn finish_open_external_editor(&mut self, result: std::io::Result<ExternalEditorResult>) {
        if let Ok(ExternalEditorResult::Complete { content }) = &result {
            self.inner.borrow().editor.set_text(content);
        }
        // TS 的 `finally { this.ui.start(); this.ui.requestRender(true); }`：无论成功、
        // 失败还是拒绝都恢复 TUI，然后拒绝再传播出去。
        {
            let mut inner = self.inner.borrow_mut();
            inner.ui.start();
            inner.ui.request_render(true);
        }
        if let Err(error) = result {
            // TS：`void this.handleOpenExternalEditor()` 不等待该 Promise，`mkdtempSync`/
            // `writeFileSync`/`readFileSync` 的异常成为 unhandled rejection →
            // `uncaughtException` → `uncaughtCrash`（退出 1、记录崩溃）。
            self.uncaught_crash(&error, None);
        }
    }
}

#[doc(hidden)]
impl InteractiveMode {
    /// `handleCtrlC` 的测试接缝。
    pub fn test_handle_ctrl_c(&mut self) {
        self.handle_ctrl_c();
    }

    /// `handleCtrlD` 的测试接缝。
    pub fn test_handle_ctrl_d(&mut self) {
        self.handle_ctrl_d();
    }

    /// `handleStartupSubmit` 的测试接缝。
    pub fn test_handle_startup_submit(&mut self, text: String) {
        self.handle_startup_submit(text);
    }

    /// `handleClipboardPaste` 的测试接缝（走真实剪贴板读取）。
    pub async fn test_handle_clipboard_paste(&mut self) {
        self.handle_clipboard_paste().await;
    }

    /// `handleClipboardPaste` 的注入剪贴板测试接缝。
    pub async fn test_handle_clipboard_paste_with(&mut self, source: &dyn ClipboardSource) {
        self.handle_clipboard_paste_with(source).await;
    }

    /// `handleRightClickPaste` 的注入剪贴板测试接缝。
    pub async fn test_handle_right_click_paste_with(&mut self, source: &dyn ClipboardSource) {
        self.handle_right_click_paste_with(source).await;
    }

    /// `handleFollowUp()` 的测试接缝。
    pub async fn test_handle_follow_up(&mut self) {
        self.handle_follow_up().await;
    }

    /// `handleOpenExternalEditor()` 的测试接缝。
    pub async fn test_handle_open_external_editor(&mut self) {
        self.handle_open_external_editor().await;
    }

    /// [`Self::finish_open_external_editor`] 的测试接缝（注入 I/O 结果）。
    pub fn test_finish_open_external_editor(
        &mut self,
        result: std::io::Result<ExternalEditorResult>,
    ) {
        self.finish_open_external_editor(result);
    }

    /// 当前编辑器文本。
    pub fn test_editor_text(&self) -> String {
        self.inner.borrow().editor.get_text()
    }

    /// 设置编辑器文本。
    pub fn test_set_editor_text(&mut self, text: &str) {
        self.inner.borrow().editor.set_text(text);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct StaticImage(ClipboardImage);

    #[async_trait::async_trait(?Send)]
    impl ClipboardSource for StaticImage {
        async fn read_file_paths(&self) -> Result<Option<Vec<String>>, String> {
            Ok(None)
        }

        async fn read_image(&self) -> Result<Option<ClipboardImage>, String> {
            Ok(Some(self.0.clone()))
        }

        async fn read_text(&self) -> Result<Option<String>, String> {
            Ok(None)
        }
    }

    /// TS `fs.writeFileSync` 抛出 Node 的 fs 异常；Rust 侧必须给出同样的
    /// `<CODE>: <description>, open '<path>'` 文案（不能带 `(os error N)` 或重复的 kind 前缀）。
    #[tokio::test]
    async fn image_paste_reports_the_node_fs_error_shape() {
        let dir = tempfile::tempdir().expect("temp dir");
        let missing = dir.path().join("missing");
        let source = StaticImage(ClipboardImage {
            bytes: vec![1, 2, 3],
            mime_type: "image/png".to_string(),
        });

        let error = paste_image_or_text(&source, &missing)
            .await
            .expect_err("writing into a missing directory must fail");
        assert!(
            error.starts_with("ENOENT: no such file or directory, open '"),
            "{error}"
        );
        assert!(error.contains("missing/kiana-clipboard-"), "{error}");
        assert!(error.ends_with(".png'"), "{error}");
    }

    /// TS 的 `/\s/.test(character)`：U+0085（NEL）不是空白（Rust 的 `char::is_whitespace`
    /// 会把它当成空白），U+FEFF 是空白。
    #[test]
    fn clipboard_path_separator_uses_the_ecmascript_whitespace_set() {
        let paths = vec!["/tmp/file.png".to_string()];

        // 光标前的 U+0085：TS 补一个前导空格，Rust 不得把它当空白而省略。
        assert_eq!(
            format_file_path_paste(&paths, "a\u{85}", Some((0, 2)), false),
            " /tmp/file.png"
        );
        // 光标前的 U+FEFF：ECMAScript 视为空白，不补空格。
        assert_eq!(
            format_file_path_paste(&paths, "a\u{feff}", Some((0, 2)), false),
            "/tmp/file.png"
        );
        // 光标后的 U+0085：补一个尾随空格。
        assert_eq!(
            format_file_path_paste(&paths, "\u{85}b", Some((0, 0)), false),
            "/tmp/file.png "
        );
    }
}
