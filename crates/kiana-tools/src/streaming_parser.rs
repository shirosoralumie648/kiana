//! `apply_patch` 补丁文本的增量解析器（设计借鉴 codex-rs/apply-patch 的 streaming parser，
//! 按 Kiana 的类型重写）。
//!
//! 两个用途：
//!
//! 1. [`parse_patch`] / [`parse_patch_with_mode`]：一次性把 `*** Begin Patch` …
//!    `*** End Patch` 解析成 [`Hunk`] 列表；
//! 2. [`StreamingPatchParser::push_delta`]：按任意字符边界喂入增量文本，返回已经能确定的
//!    [`PreviewEvent`]，供 TUI 在模型流式生成参数时实时预览（不需要等完整补丁）。
//!
//! # 补丁语法
//!
//! ```text
//! *** Begin Patch
//! *** Add File: path
//! +line
//! *** Delete File: path
//! *** Update File: path
//! *** Move to: new-path
//! @@ optional context
//!  context line
//! -removed line
//! +added line
//! *** End of File
//! *** End Patch
//! ```
//!
//! - `@@` 或 `@@ context` 开启一个新 chunk；没有 `@@` 时第一行变更会自动开启 chunk；
//! - `*** End of File` 只对当前 chunk 生效，表示 `old_lines` 必须锚定文件末尾；
//! - 空 pattern（只有 `@@` 或只有 `*** End of File`）是合法 no-op，对应「在 EOF 插入」；
//! - strict 模式要求首行/末行精确是 Begin/End；lenient 模式（默认）额外接受
//!   `<<EOF`、`<<'EOF'`、`<<"EOF"` heredoc 包装（GPT-4.1 的 local_shell 风格）。
//!
//! # 错误契约
//!
//! [`ParseError::InvalidPatchError`] 表示整体边界错误；
//! [`ParseError::InvalidHunkError`] 带 1-based 行号，文案明确指出哪一行不符合 hunk 语法。
//! 解析失败不会产生任何文件副作用（调用方在写盘前解析）。
//!
//! # 与参考实现的差异
//!
//! - 路径用 `String` 而不是 `PathBuf`：Kiana 的路径解析在工具层做（cwd/sandbox），
//!   解析器保持纯文本，便于 fixture 逐字节比较；
//! - 额外提供 [`PreviewEvent`]，参考实现的流式解析器只返回 hunk 快照；
//! - 不保留 `*** Environment ID:`（Kiana 没有对应概念），遇到会按非法 hunk 头拒绝。

use std::fmt;

/// `*** Begin Patch`。
pub const BEGIN_PATCH_MARKER: &str = "*** Begin Patch";
/// `*** End Patch`。
pub const END_PATCH_MARKER: &str = "*** End Patch";
/// `*** Add File: `。
pub const ADD_FILE_MARKER: &str = "*** Add File: ";
/// `*** Delete File: `。
pub const DELETE_FILE_MARKER: &str = "*** Delete File: ";
/// `*** Update File: `。
pub const UPDATE_FILE_MARKER: &str = "*** Update File: ";
/// `*** Move to: `。
pub const MOVE_TO_MARKER: &str = "*** Move to: ";
/// `*** End of File`。
pub const EOF_MARKER: &str = "*** End of File";
/// `@@ `。
pub const CHANGE_CONTEXT_MARKER: &str = "@@ ";
/// `@@`。
pub const EMPTY_CHANGE_CONTEXT_MARKER: &str = "@@";

/// 补丁解析错误。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ParseError {
    /// 整体边界/状态错误。
    #[error("invalid patch: {0}")]
    InvalidPatchError(String),
    /// 某一行的 hunk 语法错误；`line_number` 从 1 起。
    #[error("invalid hunk at line {line_number}, {message}")]
    InvalidHunkError {
        /// 具体原因。
        message: String,
        /// 1-based 行号。
        line_number: usize,
    },
}

/// 解析严格度。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ParseMode {
    /// 首行/末行必须精确是 `*** Begin Patch` / `*** End Patch`。
    Strict,
    /// 在 strict 基础上接受 heredoc 包装（默认）。
    #[default]
    Lenient,
}

/// 一个补丁 hunk。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Hunk {
    /// `*** Add File: path` + 若干 `+line`。
    AddFile {
        /// 相对或绝对路径（调用方解析）。
        path: String,
        /// 文件内容（每行以 `\n` 结尾）。
        contents: String,
    },
    /// `*** Delete File: path`。
    DeleteFile {
        /// 相对或绝对路径。
        path: String,
    },
    /// `*** Update File: path`（可带 `*** Move to:`）。
    UpdateFile {
        /// 源路径。
        path: String,
        /// 移动目标；`None` 表示原地更新。
        move_path: Option<String>,
        /// 至少一个 chunk（解析器保证非空）。
        chunks: Vec<UpdateFileChunk>,
    },
}

impl Hunk {
    /// 该 hunk 作用/落盘的路径（move 取目标路径，与参考实现一致）。
    pub fn target_path(&self) -> &str {
        match self {
            Hunk::AddFile { path, .. } | Hunk::DeleteFile { path } => path,
            Hunk::UpdateFile {
                move_path: Some(path),
                ..
            } => path,
            Hunk::UpdateFile { path, .. } => path,
        }
    }

    /// hunk 类型短名（`add` / `delete` / `update`）。
    pub fn kind(&self) -> &'static str {
        match self {
            Hunk::AddFile { .. } => "add",
            Hunk::DeleteFile { .. } => "delete",
            Hunk::UpdateFile { .. } => "update",
        }
    }
}

/// 一个 update chunk。
///
/// `old_lines`/`new_lines` 包含上下文行：补丁里以空格开头的行同时进入两侧。
/// `context_line_indices` 记录这些上下文行在两侧的下标，应用时据此只替换真正变化的区段，
/// 未变化的行保留原文件的字节（包括尾随空白与行尾风格）。
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct UpdateFileChunk {
    /// `@@ context` 里的可选上下文（通常是函数/类定义行）。
    pub change_context: Option<String>,
    /// 必须匹配的旧行（含上下文）。
    pub old_lines: Vec<String>,
    /// 替换后的新行（含上下文）。
    pub new_lines: Vec<String>,
    /// `(old_index, new_index)`：解析期识别出的上下文行对。
    pub context_line_indices: Vec<(usize, usize)>,
    /// 是否以 `*** End of File` 锚定文件末尾。
    pub is_end_of_file: bool,
}

impl UpdateFileChunk {
    fn push_context_line(&mut self, line: String) {
        self.context_line_indices
            .push((self.old_lines.len(), self.new_lines.len()));
        self.old_lines.push(line.clone());
        self.new_lines.push(line);
    }
}

/// 流式预览事件。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PreviewEvent {
    /// 已看到 `*** Begin Patch`。
    StartedPatch,
    /// 新的 hunk 头已确定。
    HunkStarted {
        /// `add` / `delete` / `update`。
        kind: &'static str,
        /// 目标路径。
        path: String,
    },
    /// `*** Move to:`。
    MoveTo {
        /// 目标路径。
        path: String,
    },
    /// 一行补丁内容。
    Line {
        /// 行的角色。
        kind: PreviewLineKind,
        /// 去掉前缀标记后的文本。
        text: String,
    },
    /// `*** End of File`。
    EndOfFile,
    /// `*** End Patch`。
    Finished,
}

/// [`PreviewEvent::Line`] 的角色。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PreviewLineKind {
    /// `+`。
    Add,
    /// `-`。
    Remove,
    /// ` `（上下文）。
    Context,
}

impl fmt::Display for PreviewLineKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            PreviewLineKind::Add => "add",
            PreviewLineKind::Remove => "remove",
            PreviewLineKind::Context => "context",
        })
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
enum ParserMode {
    #[default]
    NotStarted,
    StartedPatch,
    AddFile,
    DeleteFile,
    UpdateFile {
        hunk_line_number: usize,
    },
    EndedPatch,
}

/// 增量解析器。
#[derive(Debug, Default, Clone)]
pub struct StreamingPatchParser {
    line_buffer: String,
    mode: ParserMode,
    hunks: Vec<Hunk>,
    line_number: usize,
}

impl StreamingPatchParser {
    /// 新建解析器。
    pub fn new() -> Self {
        Self::default()
    }

    /// 已解析出的 hunk 快照（用于 TUI 预览）。
    pub fn hunks(&self) -> &[Hunk] {
        &self.hunks
    }

    /// 是否已看到 `*** End Patch`。
    pub fn is_finished(&self) -> bool {
        self.mode == ParserMode::EndedPatch
    }

    /// 喂入一段增量文本（可以落在任意字符/行中间），返回已确定的新事件。
    ///
    /// 只有完整的行会产生事件；未结束的行留在内部缓冲区，等下一次 `push_delta` 或
    /// [`Self::finish`] 处理。
    pub fn push_delta(&mut self, delta: &str) -> Result<Vec<PreviewEvent>, ParseError> {
        let mut events = Vec::new();
        for character in delta.chars() {
            if character == '\n' {
                let mut line = std::mem::take(&mut self.line_buffer);
                if line.ends_with('\r') {
                    line.pop();
                }
                self.line_number += 1;
                self.process_line(&line, &mut events)?;
            } else {
                self.line_buffer.push(character);
            }
        }
        Ok(events)
    }

    /// 结束解析，返回全部 hunk。
    ///
    /// 与 [`Self::push_delta`] 不同，结尾没有换行的最后一行也会被处理；必须已经看到
    /// `*** End Patch`，否则返回 `The last line of the patch must be '*** End Patch'`。
    pub fn finish(&mut self) -> Result<Vec<Hunk>, ParseError> {
        if !self.line_buffer.is_empty() {
            let mut line = std::mem::take(&mut self.line_buffer);
            if line.ends_with('\r') {
                line.pop();
            }
            self.line_number += 1;
            if line.trim() == END_PATCH_MARKER {
                self.ensure_update_hunk_is_not_empty(line.trim())?;
                self.mode = ParserMode::EndedPatch;
            } else {
                let mut events = Vec::new();
                self.process_line(&line, &mut events)?;
            }
        }
        if self.mode != ParserMode::EndedPatch {
            return Err(ParseError::InvalidPatchError(
                "The last line of the patch must be '*** End Patch'".to_string(),
            ));
        }
        Ok(self.hunks.clone())
    }

    fn ensure_update_hunk_is_not_empty(&self, next_line: &str) -> Result<(), ParseError> {
        if let Some(Hunk::UpdateFile { path, chunks, .. }) = self.hunks.last()
            && let ParserMode::UpdateFile { hunk_line_number } = self.mode
        {
            if chunks.is_empty() {
                return Err(ParseError::InvalidHunkError {
                    message: format!("Update file hunk for path '{path}' is empty"),
                    line_number: hunk_line_number,
                });
            }
            if chunks
                .last()
                .is_some_and(|chunk| chunk.old_lines.is_empty() && chunk.new_lines.is_empty())
            {
                let message = if next_line == END_PATCH_MARKER {
                    "Update hunk does not contain any lines".to_string()
                } else {
                    format!(
                        "Unexpected line found in update hunk: '{next_line}'. Every line should \
                         start with ' ' (context line), '+' (added line), or '-' (removed line)"
                    )
                };
                return Err(ParseError::InvalidHunkError {
                    message,
                    line_number: self.line_number,
                });
            }
        }
        Ok(())
    }

    /// 处理 hunk 头与 `*** End Patch`；返回 true 表示该行已被消费。
    fn handle_hunk_headers(
        &mut self,
        trimmed: &str,
        events: &mut Vec<PreviewEvent>,
    ) -> Result<bool, ParseError> {
        if trimmed == END_PATCH_MARKER {
            self.ensure_update_hunk_is_not_empty(trimmed)?;
            self.mode = ParserMode::EndedPatch;
            events.push(PreviewEvent::Finished);
            return Ok(true);
        }
        if let Some(path) = trimmed.strip_prefix(ADD_FILE_MARKER) {
            self.ensure_update_hunk_is_not_empty(trimmed)?;
            self.hunks.push(Hunk::AddFile {
                path: path.to_string(),
                contents: String::new(),
            });
            self.mode = ParserMode::AddFile;
            events.push(PreviewEvent::HunkStarted {
                kind: "add",
                path: path.to_string(),
            });
            return Ok(true);
        }
        if let Some(path) = trimmed.strip_prefix(DELETE_FILE_MARKER) {
            self.ensure_update_hunk_is_not_empty(trimmed)?;
            self.hunks.push(Hunk::DeleteFile {
                path: path.to_string(),
            });
            self.mode = ParserMode::DeleteFile;
            events.push(PreviewEvent::HunkStarted {
                kind: "delete",
                path: path.to_string(),
            });
            return Ok(true);
        }
        if let Some(path) = trimmed.strip_prefix(UPDATE_FILE_MARKER) {
            self.ensure_update_hunk_is_not_empty(trimmed)?;
            self.hunks.push(Hunk::UpdateFile {
                path: path.to_string(),
                move_path: None,
                chunks: Vec::new(),
            });
            self.mode = ParserMode::UpdateFile {
                hunk_line_number: self.line_number,
            };
            events.push(PreviewEvent::HunkStarted {
                kind: "update",
                path: path.to_string(),
            });
            return Ok(true);
        }
        Ok(false)
    }

    fn invalid_header_error(&self, trimmed: &str) -> ParseError {
        ParseError::InvalidHunkError {
            message: format!(
                "'{trimmed}' is not a valid hunk header. Valid hunk headers: \
                 '*** Add File: {{path}}', '*** Delete File: {{path}}', \
                 '*** Update File: {{path}}'"
            ),
            line_number: self.line_number,
        }
    }

    fn process_line(
        &mut self,
        line: &str,
        events: &mut Vec<PreviewEvent>,
    ) -> Result<(), ParseError> {
        let trimmed = line.trim();
        match self.mode {
            ParserMode::NotStarted => {
                if trimmed == BEGIN_PATCH_MARKER {
                    self.mode = ParserMode::StartedPatch;
                    events.push(PreviewEvent::StartedPatch);
                    return Ok(());
                }
                Err(ParseError::InvalidPatchError(
                    "The first line of the patch must be '*** Begin Patch'".to_string(),
                ))
            }
            ParserMode::StartedPatch => {
                if self.handle_hunk_headers(trimmed, events)? {
                    return Ok(());
                }
                Err(self.invalid_header_error(trimmed))
            }
            ParserMode::AddFile => {
                if self.handle_hunk_headers(trimmed, events)? {
                    return Ok(());
                }
                if let Some(line_to_add) = line.strip_prefix('+')
                    && let Some(Hunk::AddFile { contents, .. }) = self.hunks.last_mut()
                {
                    contents.push_str(line_to_add);
                    contents.push('\n');
                    events.push(PreviewEvent::Line {
                        kind: PreviewLineKind::Add,
                        text: line_to_add.to_string(),
                    });
                    return Ok(());
                }
                Err(self.invalid_header_error(trimmed))
            }
            ParserMode::DeleteFile => {
                if self.handle_hunk_headers(trimmed, events)? {
                    return Ok(());
                }
                Err(self.invalid_header_error(trimmed))
            }
            ParserMode::UpdateFile { .. } => {
                let update_line = line.trim_end();
                if self.handle_hunk_headers(update_line, events)? {
                    return Ok(());
                }

                let Some(Hunk::UpdateFile {
                    move_path, chunks, ..
                }) = self.hunks.last_mut()
                else {
                    return Err(self.invalid_header_error(trimmed));
                };

                // `*** End of File` 之后只允许新的 `@@` 头或空行。
                if chunks.last().is_some_and(|chunk| chunk.is_end_of_file) {
                    if update_line.is_empty() {
                        return Ok(());
                    }
                    if update_line != EMPTY_CHANGE_CONTEXT_MARKER
                        && !update_line.starts_with(CHANGE_CONTEXT_MARKER)
                    {
                        return Err(ParseError::InvalidHunkError {
                            message: format!(
                                "Expected update hunk to start with a @@ context marker, got: \
                                 '{line}'"
                            ),
                            line_number: self.line_number,
                        });
                    }
                }

                if chunks.is_empty()
                    && move_path.is_none()
                    && let Some(move_to_path) = update_line.strip_prefix(MOVE_TO_MARKER)
                {
                    *move_path = Some(move_to_path.to_string());
                    events.push(PreviewEvent::MoveTo {
                        path: move_to_path.to_string(),
                    });
                    return Ok(());
                }

                if (update_line == EMPTY_CHANGE_CONTEXT_MARKER
                    || update_line.starts_with(CHANGE_CONTEXT_MARKER))
                    && chunks.last().is_some_and(|chunk| {
                        chunk.old_lines.is_empty() && chunk.new_lines.is_empty()
                    })
                {
                    return Err(ParseError::InvalidHunkError {
                        message: format!(
                            "Unexpected line found in update hunk: '{line}'. Every line should \
                             start with ' ' (context line), '+' (added line), or '-' (removed \
                             line)"
                        ),
                        line_number: self.line_number,
                    });
                }

                if update_line == EMPTY_CHANGE_CONTEXT_MARKER {
                    chunks.push(UpdateFileChunk::default());
                    return Ok(());
                }

                if let Some(change_context) = update_line.strip_prefix(CHANGE_CONTEXT_MARKER) {
                    chunks.push(UpdateFileChunk {
                        change_context: Some(change_context.to_string()),
                        ..UpdateFileChunk::default()
                    });
                    return Ok(());
                }

                if update_line == EOF_MARKER {
                    if chunks.last().is_some_and(|chunk| {
                        chunk.old_lines.is_empty() && chunk.new_lines.is_empty()
                    }) {
                        return Err(ParseError::InvalidHunkError {
                            message: "Update hunk does not contain any lines".to_string(),
                            line_number: self.line_number,
                        });
                    }
                    if let Some(chunk) = chunks.last_mut() {
                        chunk.is_end_of_file = true;
                    }
                    events.push(PreviewEvent::EndOfFile);
                    return Ok(());
                }

                if line.is_empty() {
                    if chunks.is_empty() {
                        chunks.push(UpdateFileChunk::default());
                    }
                    if let Some(chunk) = chunks.last_mut() {
                        chunk.push_context_line(String::new());
                    }
                    events.push(PreviewEvent::Line {
                        kind: PreviewLineKind::Context,
                        text: String::new(),
                    });
                    return Ok(());
                }

                if let Some(text) = line.strip_prefix(' ') {
                    if chunks.is_empty() {
                        chunks.push(UpdateFileChunk::default());
                    }
                    if let Some(chunk) = chunks.last_mut() {
                        chunk.push_context_line(text.to_string());
                    }
                    events.push(PreviewEvent::Line {
                        kind: PreviewLineKind::Context,
                        text: text.to_string(),
                    });
                    return Ok(());
                }

                if let Some(text) = line.strip_prefix('+') {
                    if chunks.is_empty() {
                        chunks.push(UpdateFileChunk::default());
                    }
                    if let Some(chunk) = chunks.last_mut() {
                        chunk.new_lines.push(text.to_string());
                    }
                    events.push(PreviewEvent::Line {
                        kind: PreviewLineKind::Add,
                        text: text.to_string(),
                    });
                    return Ok(());
                }

                if let Some(text) = line.strip_prefix('-') {
                    if chunks.is_empty() {
                        chunks.push(UpdateFileChunk::default());
                    }
                    if let Some(chunk) = chunks.last_mut() {
                        chunk.old_lines.push(text.to_string());
                    }
                    events.push(PreviewEvent::Line {
                        kind: PreviewLineKind::Remove,
                        text: text.to_string(),
                    });
                    return Ok(());
                }

                if chunks
                    .last()
                    .is_some_and(|chunk| !chunk.old_lines.is_empty() || !chunk.new_lines.is_empty())
                {
                    return Err(ParseError::InvalidHunkError {
                        message: format!(
                            "Expected update hunk to start with a @@ context marker, got: '{line}'"
                        ),
                        line_number: self.line_number,
                    });
                }
                Err(ParseError::InvalidHunkError {
                    message: format!(
                        "Unexpected line found in update hunk: '{line}'. Every line should start \
                         with ' ' (context line), '+' (added line), or '-' (removed line)"
                    ),
                    line_number: self.line_number,
                })
            }
            ParserMode::EndedPatch => {
                if trimmed.is_empty() {
                    Ok(())
                } else {
                    Err(ParseError::InvalidPatchError(
                        "The last line of the patch must be '*** End Patch'".to_string(),
                    ))
                }
            }
        }
    }
}

/// 解析补丁（默认 lenient）。
pub fn parse_patch(patch: &str) -> Result<Vec<Hunk>, ParseError> {
    parse_patch_with_mode(patch, ParseMode::Lenient)
}

/// 按指定严格度解析补丁。
pub fn parse_patch_with_mode(patch: &str, mode: ParseMode) -> Result<Vec<Hunk>, ParseError> {
    let lines: Vec<&str> = patch.lines().collect();
    let first = lines.iter().position(|line| !line.trim().is_empty());
    let last = lines.iter().rposition(|line| !line.trim().is_empty());
    let (Some(first), Some(last)) = (first, last) else {
        return Err(ParseError::InvalidPatchError(
            "The first line of the patch must be '*** Begin Patch'".to_string(),
        ));
    };
    let mut body = &lines[first..=last];

    // 边界检查：strict 要求首尾精确；lenient 额外剥掉 heredoc 包裹。
    let first_line = body[0].trim();
    if first_line != BEGIN_PATCH_MARKER {
        more_lenient_boundary(body, mode)?;
        // `more_lenient_boundary` 只在 lenient 下成功，并已经剥掉 heredoc 行。
        body = &body[1..body.len() - 1];
    }
    if body.is_empty() || body[0].trim() != BEGIN_PATCH_MARKER {
        return Err(ParseError::InvalidPatchError(
            "The first line of the patch must be '*** Begin Patch'".to_string(),
        ));
    }
    if body[body.len() - 1].trim() != END_PATCH_MARKER {
        return Err(ParseError::InvalidPatchError(
            "The last line of the patch must be '*** End Patch'".to_string(),
        ));
    }

    let mut parser = StreamingPatchParser::new();
    let text = body.join("\n");
    parser.push_delta(&text)?;
    // `push_delta` 只处理完整的行；最后一行没有换行时补一个，交给正常状态机处理。
    if !parser.is_finished() {
        parser.push_delta("\n")?;
    }
    parser.finish()
}

/// strict 失败后的 lenient 检查：`<<EOF`/`<<'EOF'`/`<<"EOF"` 包裹。
fn more_lenient_boundary(body: &[&str], mode: ParseMode) -> Result<(), ParseError> {
    if mode == ParseMode::Strict {
        return Err(ParseError::InvalidPatchError(
            "The first line of the patch must be '*** Begin Patch'".to_string(),
        ));
    }
    let first = body[0].trim();
    let last = body[body.len() - 1].trim();
    let heredoc_start = matches!(first, "<<EOF" | "<<'EOF'" | "<<\"EOF\"");
    if !heredoc_start || !last.ends_with("EOF") || body.len() < 4 {
        return Err(ParseError::InvalidPatchError(
            "The first line of the patch must be '*** Begin Patch'".to_string(),
        ));
    }
    if body[1].trim() != BEGIN_PATCH_MARKER {
        return Err(ParseError::InvalidPatchError(
            "The first line of the patch must be '*** Begin Patch'".to_string(),
        ));
    }
    if body[body.len() - 2].trim() != END_PATCH_MARKER {
        return Err(ParseError::InvalidPatchError(
            "The last line of the patch must be '*** End Patch'".to_string(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_add_delete_update_and_move() {
        let patch = "*** Begin Patch\n\
                     *** Add File: path/add.py\n\
                     +abc\n\
                     +def\n\
                     *** Delete File: path/delete.py\n\
                     *** Update File: path/update.py\n\
                     *** Move to: path/update2.py\n\
                     @@ def f():\n\
                     -    pass\n\
                     +    return 123\n\
                     *** End Patch";
        let hunks = parse_patch(patch).expect("parse");
        assert_eq!(
            hunks,
            vec![
                Hunk::AddFile {
                    path: "path/add.py".into(),
                    contents: "abc\ndef\n".into()
                },
                Hunk::DeleteFile {
                    path: "path/delete.py".into()
                },
                Hunk::UpdateFile {
                    path: "path/update.py".into(),
                    move_path: Some("path/update2.py".into()),
                    chunks: vec![UpdateFileChunk {
                        change_context: Some("def f():".into()),
                        old_lines: vec!["    pass".into()],
                        new_lines: vec!["    return 123".into()],
                        context_line_indices: vec![],
                        is_end_of_file: false,
                    }]
                },
            ]
        );
    }

    #[test]
    fn context_lines_are_recorded_on_both_sides() {
        let patch = r#"*** Begin Patch
*** Update File: f.py
@@
 import foo
+bar
*** End Patch"#;
        let hunks = parse_patch(patch).expect("parse");
        let Hunk::UpdateFile { chunks, .. } = &hunks[0] else {
            panic!("update");
        };
        assert_eq!(chunks[0].old_lines, vec!["import foo"]);
        assert_eq!(chunks[0].new_lines, vec!["import foo", "bar"]);
        assert_eq!(chunks[0].context_line_indices, vec![(0, 0)]);
    }

    #[test]
    fn empty_patch_is_allowed() {
        assert_eq!(
            parse_patch("*** Begin Patch\n*** End Patch").unwrap(),
            vec![]
        );
    }

    #[test]
    fn empty_update_hunk_is_rejected() {
        let error = parse_patch("*** Begin Patch\n*** Update File: f\n*** End Patch").unwrap_err();
        assert!(
            matches!(error, ParseError::InvalidHunkError { .. }),
            "{error}"
        );
        assert!(error.to_string().contains("is empty"));
    }

    #[test]
    fn strict_mode_rejects_heredoc_wrapper() {
        let patch = "<<'EOF'\n*** Begin Patch\n*** Add File: f\n+x\n*** End Patch\nEOF";
        assert!(parse_patch_with_mode(patch, ParseMode::Strict).is_err());
        let hunks = parse_patch_with_mode(patch, ParseMode::Lenient).expect("lenient");
        assert_eq!(hunks.len(), 1);
    }

    #[test]
    fn streaming_parser_emits_preview_before_end_patch() {
        let mut parser = StreamingPatchParser::new();
        let events = parser
            .push_delta("*** Begin Patch\n*** Add File: f.txt\n+hello\n+wor")
            .expect("delta");
        assert!(events.contains(&PreviewEvent::StartedPatch));
        assert!(events.contains(&PreviewEvent::HunkStarted {
            kind: "add",
            path: "f.txt".into()
        }));
        assert!(events.contains(&PreviewEvent::Line {
            kind: PreviewLineKind::Add,
            text: "hello".into()
        }));
        // 半行不产生事件。
        assert!(!events.iter().any(|event| matches!(
            event,
            PreviewEvent::Line { text, .. } if text == "wor"
        )));
        assert_eq!(parser.hunks().len(), 1);
        let rest = parser.push_delta("ld\n*** End Patch\n").expect("rest");
        assert!(rest.contains(&PreviewEvent::Finished));
        parse_finish_is_ok(&mut parser);
    }

    fn parse_finish_is_ok(parser: &mut StreamingPatchParser) {
        let hunks = parser.finish().expect("finish");
        assert_eq!(
            hunks,
            vec![Hunk::AddFile {
                path: "f.txt".into(),
                contents: "hello\nworld\n".into()
            }]
        );
    }

    #[test]
    fn streaming_parser_accepts_arbitrary_char_boundaries() {
        let patch = "*** Begin Patch\n*** Update File: f.py\n@@\n-old\n+new\n*** End Patch\n";
        let mut parser = StreamingPatchParser::new();
        for character in patch.chars() {
            parser.push_delta(&character.to_string()).expect("char");
        }
        let hunks = parser.finish().expect("finish");
        assert_eq!(hunks.len(), 1);
    }

    #[test]
    fn finish_requires_end_patch() {
        let mut parser = StreamingPatchParser::new();
        parser
            .push_delta("*** Begin Patch\n*** Add File: f\n+x\n")
            .unwrap();
        let error = parser.finish().unwrap_err();
        assert!(error.to_string().contains("*** End Patch"));
    }

    #[test]
    fn eof_marker_marks_the_last_chunk() {
        let patch =
            "*** Begin Patch\n*** Update File: f.txt\n@@\n+quux\n*** End of File\n*** End Patch";
        let hunks = parse_patch(patch).unwrap();
        let Hunk::UpdateFile { chunks, .. } = &hunks[0] else {
            panic!("update");
        };
        assert!(chunks[0].is_end_of_file);
        assert!(chunks[0].old_lines.is_empty());
        assert_eq!(chunks[0].new_lines, vec!["quux"]);
    }
}
