//! TUI 帧契约：`FrameProvider` → `TerminalFramePlan` → 一次终端写入。
//!
//! # 契约
//!
//! 应用层实现 [`FrameProvider`]：每帧接收当前 [`ViewportSize`]，返回
//! [`TerminalFramePlan`]——可选的 [`HistoryBatch`]（`append` 或 `replay`，单调 id）与
//! **完整可变 viewport**。本模块的 [`FrameWriter`] 是终端侧：
//!
//! 1. 只有 `id > accepted_history_id` 的 batch 才会被写入；重复/旧 batch 只重新 ack，
//!    不会二次输出。这样即使上一次 ack 丢失、provider 重发同一个 batch，scrollback
//!    也不会重复。
//! 2. 写出成功后（`TerminalSink::write_frame` 返回 `Ok`）才把 id 记为 accepted 并调用
//!    [`FrameProvider::acknowledge_history`]；写失败不 ack，下一帧 provider 仍会重发。
//! 3. viewport 锚定在剩余 history 之后：没有 history 的帧只对 viewport 做行级 diff；
//!    有 history 的帧先擦掉旧 viewport（避免终端的滚动把未完成帧推进 scrollback），
//!    再从旧 viewport 顶部顺序写 history 与 viewport。
//! 4. overlay 是屏幕坐标内容，单独合成进 viewport，永远不会进入 history。
//! 5. 光标标记（[`CURSOR_MARKER`]）在行重置之后提取，硬件光标定位序列位于
//!    synchronized-output 帧内（`ESC[?2026h` … `ESC[?2026l`）。
//! 6. `replay` 是一次原子写：先清屏（ED2+ED3），再写完整逻辑账本 + 最终 viewport；
//!    不会出现 block-by-block 的中间帧。
//!
//! # 宽度与失败
//!
//! 所有行在写出前按 viewport 宽度截断（ANSI-aware），超宽行不会 panic。写失败返回
//! [`FrameWriteError::Io`]，此时 accepted id、上一帧 viewport、committed 行都不更新。
//!
//! # 与参考实现的关系
//!
//! 契约取自 oh-my-pi 的 `TerminalFrameProvider` / `HistoryBatch` / `#emitPlanFrame`
//! （`packages/tui/src/tui.ts`），但去掉了图片预算、CPR 锚点恢复、multiplexer 探测等
//! 产品级细节；这里只保留可独立测试的帧语义。注释里提到的「golden」是
//! `tests/render_core_frame.rs` 锁定的转义序列。
//!
//! 本模块不读终端、不依赖全局主题；终端写入通过 [`TerminalSink`] 注入。

use kiana_tui::text::{truncate_to_width, visible_width};
use kiana_tui::tui::CURSOR_MARKER;

/// synchronized-output 开始序列。
pub const BEGIN_SYNCHRONIZED_OUTPUT: &str = "\x1b[?2026h";
/// synchronized-output 结束序列。
pub const END_SYNCHRONIZED_OUTPUT: &str = "\x1b[?2026l";
/// 进入备用屏并关闭自动换行。
pub const ENTER_ALT_SCREEN: &str = "\x1b[?1049h\x1b[?7l";
/// 退出备用屏并恢复自动换行。
pub const EXIT_ALT_SCREEN: &str = "\x1b[?7h\x1b[?1049l";
/// 清屏 + 清 scrollback（ED2 + ED3）。
pub const CLEAR_SCREEN_AND_SCROLLBACK: &str = "\r\x1b[H\x1b[2J\x1b[3J";

/// 终端尺寸（列 × 行）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ViewportSize {
    /// 列数。
    pub columns: usize,
    /// 行数。
    pub rows: usize,
}

impl ViewportSize {
    /// 构造并校验尺寸；0 行/列返回 `None`（调用方应跳过该帧）。
    pub fn new(columns: usize, rows: usize) -> Option<Self> {
        (columns > 0 && rows > 0).then_some(Self { columns, rows })
    }
}

/// history batch 的写入语义。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum HistoryBatchKind {
    /// 追加已定稿（或稳定 head）的行。
    #[default]
    Append,
    /// 完整逻辑账本；写端先清屏再重放。
    Replay,
}

/// 一次 history 追加或完整重放。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HistoryBatch {
    /// 单调递增 id；provider 在收到 ack 前保留并原样重发。
    pub id: u64,
    /// 行内容（可含 ANSI；写端会按宽度截断）。
    pub rows: Vec<String>,
    /// 写入语义。
    pub kind: HistoryBatchKind,
}

impl HistoryBatch {
    /// 构造 append batch。
    pub fn append(id: u64, rows: Vec<String>) -> Self {
        Self {
            id,
            rows,
            kind: HistoryBatchKind::Append,
        }
    }

    /// 构造 replay batch。
    pub fn replay(id: u64, rows: Vec<String>) -> Self {
        Self {
            id,
            rows,
            kind: HistoryBatchKind::Replay,
        }
    }
}

/// 一帧的计划：可选 history + 完整可变 viewport。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TerminalFramePlan {
    /// 本帧要退休的 history；`None` 表示只更新 viewport。
    pub history: Option<HistoryBatch>,
    /// 完整可变屏幕内容（不是增量）。
    pub viewport: Vec<String>,
}

/// 每帧向 TUI 提供计划的接口。
///
/// 实现方负责 finality 与 id 的单调性；本模块只保证「接受 id 只写一次 + 写成功后 ack」。
pub trait FrameProvider {
    /// 生成一帧计划。
    fn render_frame(&mut self, viewport: ViewportSize) -> TerminalFramePlan;

    /// 写端确认某个 batch 已经被接受。默认空实现。
    fn acknowledge_history(&mut self, _id: u64) {}

    /// 请求下一次 `render_frame` 返回完整账本 `replay`。
    fn begin_history_replay(&mut self) {}

    /// 请求下一次 `render_frame` 返回所有可退休的 finalized 前缀（关停用）。
    fn begin_history_flush(&mut self) {}
}

/// 终端写入抽象；生产实现包 `ProcessTerminal::write`，测试实现收集字节。
pub trait TerminalSink {
    /// 写入一帧；返回 `Err` 时本帧视为未接受。
    fn write_frame(&mut self, bytes: &str) -> std::io::Result<()>;
}

/// 把字节追加进 `String` 的测试/内存 sink。
#[derive(Clone, Debug, Default)]
pub struct StringSink {
    /// 已写入的字节。
    pub output: String,
    /// 写入调用次数。
    pub calls: usize,
}

impl TerminalSink for StringSink {
    fn write_frame(&mut self, bytes: &str) -> std::io::Result<()> {
        self.calls += 1;
        self.output.push_str(bytes);
        Ok(())
    }
}

/// 一定失败的 sink（测试写失败路径）。
#[derive(Debug, Default)]
pub struct FailingSink {
    /// 收到的调用次数。
    pub calls: usize,
}

impl TerminalSink for FailingSink {
    fn write_frame(&mut self, _bytes: &str) -> std::io::Result<()> {
        self.calls += 1;
        Err(std::io::Error::other("simulated write failure"))
    }
}

/// 写端错误。
#[derive(Debug)]
pub enum FrameWriteError {
    /// 终端写入失败；本帧未 ack。
    Io(std::io::Error),
}

impl std::fmt::Display for FrameWriteError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FrameWriteError::Io(error) => write!(f, "terminal write failed: {error}"),
        }
    }
}

impl std::error::Error for FrameWriteError {}

/// 渲染模式。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TerminalMode {
    /// 主屏 / 内联模式（有 scrollback）。
    #[default]
    Inline,
    /// 备用屏（全屏）；history 永远不被接受。
    AltScreen,
}

/// 多路复用器/终端类型；只影响转义序列的固定 golden 与默认同步输出开关。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Multiplexer {
    /// 直连终端。
    #[default]
    Direct,
    /// tmux。
    Tmux,
    /// GNU screen。
    Screen,
}

impl Multiplexer {
    /// 该终端默认是否支持 synchronized output。
    pub fn supports_synchronized_output(self) -> bool {
        !matches!(self, Self::Screen)
    }
}

/// 写端选项。
#[derive(Clone, Copy, Debug)]
pub struct FrameWriterOptions {
    /// 主屏 / 备用屏。
    pub mode: TerminalMode,
    /// 终端类型。
    pub multiplexer: Multiplexer,
    /// 是否使用 synchronized output；`None` 按终端类型默认。
    pub synchronized_output: Option<bool>,
}

impl Default for FrameWriterOptions {
    fn default() -> Self {
        Self {
            mode: TerminalMode::Inline,
            multiplexer: Multiplexer::Direct,
            synchronized_output: None,
        }
    }
}

impl FrameWriterOptions {
    fn synchronized(self) -> bool {
        self.synchronized_output
            .unwrap_or_else(|| self.multiplexer.supports_synchronized_output())
    }
}

/// 屏幕坐标 overlay：单独合成，永不进入 history。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct OverlayFrame {
    /// 起始行（0 基）。
    pub row: usize,
    /// 起始列（0 基）。
    pub column: usize,
    /// overlay 行。
    pub lines: Vec<String>,
    /// 全屏覆盖（忽略 `row`/`column`，每行从 0 列开始）。
    pub full_screen: bool,
}

/// 一帧的写端结果（测试与诊断用）。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FrameOutput {
    /// 本帧实际写给终端的字节。
    pub bytes: String,
    /// 终端写入调用次数（正常路径恒为 1）。
    pub write_calls: usize,
    /// 内容重定位的光标移动次数（不含帧末硬件光标定位）。
    pub content_moves: usize,
    /// 被重写的 viewport 行数。
    pub line_rewrites: usize,
    /// 本帧写入的 history 行数。
    pub history_rows_written: usize,
    /// 写出成功后被接受的 history id（没有则为 `None`）。
    pub accepted_history_id: Option<u64>,
    /// 重复/旧 batch 被重新 ack 的次数。
    pub duplicate_acknowledgements: usize,
    /// 本帧后的 viewport 顶部行（0 基）。
    pub viewport_top: usize,
    /// 是否处于备用屏。
    pub alt_screen: bool,
    /// 硬件光标位置（行 / 列，0 基）及是否可见。
    pub cursor: Option<(usize, usize, bool)>,
    /// 写失败时的错误文本（此时 `bytes` 不写、不 ack）。
    pub error: Option<String>,
}

/// 帧写端。
///
/// 状态只包含渲染契约需要的东西：accepted id、上一帧 viewport、viewport 顶部与已提交行。
/// 不读取终端能力、不探测滚动位置。
pub struct FrameWriter {
    options: FrameWriterOptions,
    accepted_history_id: u64,
    previous_viewport: Vec<String>,
    previous_size: Option<ViewportSize>,
    viewport_top: usize,
    committed_rows: Vec<String>,
    overlay: Option<OverlayFrame>,
    alt_screen_started: bool,
    last_output: FrameOutput,
}

impl FrameWriter {
    /// 构造写端；accepted id 从 0 开始（provider 的 id 必须从 1 开始）。
    pub fn new(options: FrameWriterOptions) -> Self {
        Self {
            options,
            accepted_history_id: 0,
            previous_viewport: Vec::new(),
            previous_size: None,
            viewport_top: 0,
            committed_rows: Vec::new(),
            overlay: None,
            alt_screen_started: false,
            last_output: FrameOutput::default(),
        }
    }

    /// 当前已接受的最大 history id。
    pub fn accepted_history_id(&self) -> u64 {
        self.accepted_history_id
    }

    /// 已提交到 scrollback 的行（按接受顺序；replay 会整体替换）。
    pub fn committed_rows(&self) -> &[String] {
        &self.committed_rows
    }

    /// 上一帧的完整 viewport（已宽度截断、已合成 overlay）。
    pub fn viewport_snapshot(&self) -> &[String] {
        &self.previous_viewport
    }

    /// 上一帧的 viewport 顶部。
    pub fn viewport_top(&self) -> usize {
        self.viewport_top
    }

    /// 最近一帧的输出。
    pub fn last_output(&self) -> &FrameOutput {
        &self.last_output
    }

    /// 设置/清除 overlay；overlay 只影响下一帧的 viewport 合成。
    pub fn set_overlay(&mut self, overlay: Option<OverlayFrame>) {
        self.overlay = overlay;
    }

    /// 主动请求 provider 重放完整账本（例如用户执行 `/clear`）。
    pub fn request_replay(&mut self, provider: &mut dyn FrameProvider) {
        provider.begin_history_replay();
    }

    /// 渲染并写出一个完整帧。
    ///
    /// 失败时返回 [`FrameWriteError::Io`]；此时 accepted id、committed 行与上一帧
    /// viewport 都不变，provider 会在下一帧重发同一个 batch。
    pub fn render(
        &mut self,
        provider: &mut dyn FrameProvider,
        size: ViewportSize,
        sink: &mut dyn TerminalSink,
    ) -> Result<FrameOutput, FrameWriteError> {
        let plan = provider.render_frame(size);
        let mut output = FrameOutput {
            alt_screen: matches!(self.options.mode, TerminalMode::AltScreen),
            ..FrameOutput::default()
        };

        // 1. history 接受判定。备用屏永不接受/不 ack history（历史只能在主屏）。
        let mut history = None;
        if let Some(batch) = plan.history {
            if matches!(self.options.mode, TerminalMode::AltScreen) {
                // 备用屏：不写、不 ack，provider 继续保留。
            } else if batch.id > self.accepted_history_id {
                history = Some(batch);
            } else {
                // 旧/重复 batch：重新 ack 让 provider 释放，绝不二次写。
                provider.acknowledge_history(batch.id);
                output.duplicate_acknowledgements += 1;
            }
        }

        // 2. viewport 宽度截断 + 行数裁剪（保留尾部）。
        let mut viewport = fit_rows(&plan.viewport, size);
        if let Some(overlay) = self.overlay.as_ref() {
            viewport = compose_overlay(&viewport, overlay, size);
        }

        let mut cursor = extract_cursor(&mut viewport);
        let mut bytes = String::new();
        if matches!(self.options.mode, TerminalMode::AltScreen) && !self.alt_screen_started {
            bytes.push_str(ENTER_ALT_SCREEN);
            self.alt_screen_started = true;
        }
        if self.options.synchronized() {
            bytes.push_str(BEGIN_SYNCHRONIZED_OUTPUT);
        }

        let history_rows: Vec<String> = history
            .as_ref()
            .map(|batch| fit_width(&batch.rows, size))
            .unwrap_or_default();
        let replay = history
            .as_ref()
            .is_some_and(|batch| batch.kind == HistoryBatchKind::Replay);

        if matches!(self.options.mode, TerminalMode::AltScreen) {
            // 备用屏每帧全量写：先用 home + 清行，再逐行写 viewport。
            bytes.push_str("\x1b[H");
            write_rows(&mut bytes, &viewport, size.columns, false);
            let top = 0usize;
            output.viewport_top = top;
        } else if replay {
            bytes.push_str(CLEAR_SCREEN_AND_SCROLLBACK);
            write_rows(&mut bytes, &history_rows, size.columns, false);
            write_rows(
                &mut bytes,
                &viewport,
                size.columns,
                !history_rows.is_empty(),
            );
            output.history_rows_written = history_rows.len();
        } else if history.is_some()
            || self.previous_size != Some(size)
            || (self.previous_viewport.is_empty() && !viewport.is_empty())
        {
            // 全量重绘：history 先行，viewport 跟随。先擦掉旧 viewport，防止终端的
            // 滚动把未完成帧推入 scrollback。
            let start_top = if self.previous_size == Some(size) {
                self.viewport_top
            } else {
                0
            };
            if !self.previous_viewport.is_empty() {
                bytes.push_str(&erase_below(start_top));
            }
            let new_top = start_top
                .saturating_add(history_rows.len())
                .min(size.rows.saturating_sub(viewport.len()));
            bytes.push_str(&format!("\x1b[{};1H", start_top + 1));
            write_rows(&mut bytes, &history_rows, size.columns, false);
            write_rows(
                &mut bytes,
                &viewport,
                size.columns,
                !history_rows.is_empty(),
            );
            let end_row = new_top + viewport.len();
            if end_row < size.rows {
                bytes.push_str(&format!("\x1b[{};1H\x1b[J", end_row + 1));
            }
            output.history_rows_written = history_rows.len();
            output.viewport_top = new_top;
        } else {
            // 行级 diff：只重写变化行。单字符变化 → 1 次内容移动 + 1 行重写。
            let top = self.viewport_top;
            let max_len = self.previous_viewport.len().max(viewport.len());
            for index in 0..max_len {
                let changed = match (self.previous_viewport.get(index), viewport.get(index)) {
                    (Some(old), Some(new)) => old != new,
                    (None, Some(_)) | (Some(_), None) => true,
                    (None, None) => false,
                };
                if !changed {
                    continue;
                }
                bytes.push_str(&format!("\x1b[{};1H", top + index + 1));
                output.content_moves += 1;
                output.line_rewrites += 1;
                if let Some(row) = viewport.get(index) {
                    bytes.push_str(row);
                    bytes.push_str("\x1b[K");
                } else {
                    bytes.push_str("\x1b[K");
                }
            }
            if self.previous_viewport.len() > viewport.len() {
                bytes.push_str(&format!("\x1b[{};1H\x1b[J", top + viewport.len() + 1));
            }
            output.viewport_top = top;
        }

        // 3. 硬件光标必须在本帧的 synchronized-output 区域内落位。
        if cursor.is_none() {
            cursor = Some((output.viewport_top, 0, false));
        }
        if let Some((row, column, visible)) = cursor {
            let row = row.min(size.rows.saturating_sub(1));
            let column = column.min(size.columns.saturating_sub(1));
            bytes.push_str(&format!("\x1b[{};{}H", row + 1, column + 1));
            bytes.push_str(if visible { "\x1b[?25h" } else { "\x1b[?25l" });
            output.cursor = Some((row, column, visible));
        }
        if self.options.synchronized() {
            bytes.push_str(END_SYNCHRONIZED_OUTPUT);
        }

        // 4. 写出成功后才提交状态并 ack。
        if let Err(error) = sink.write_frame(&bytes) {
            output.error = Some(error.to_string());
            self.last_output = output;
            return Err(FrameWriteError::Io(error));
        }
        output.write_calls = 1;
        output.bytes = bytes;

        if let Some(batch) = history {
            output.accepted_history_id = Some(batch.id);
            self.accepted_history_id = batch.id;
            if batch.kind == HistoryBatchKind::Replay {
                self.committed_rows = history_rows.clone();
            } else {
                self.committed_rows.extend(history_rows.iter().cloned());
            }
            provider.acknowledge_history(batch.id);
        }
        self.previous_viewport = viewport;
        self.previous_size = Some(size);
        self.viewport_top = output.viewport_top;
        self.last_output = output.clone();
        Ok(output)
    }

    /// 停止备用屏（如果开过）；主屏模式无操作。
    pub fn stop(&mut self, sink: &mut dyn TerminalSink) -> std::io::Result<()> {
        if self.alt_screen_started {
            self.alt_screen_started = false;
            return sink.write_frame(EXIT_ALT_SCREEN);
        }
        Ok(())
    }
}

/// 按宽度截断并只保留最后 `size.rows` 行（viewport 用；history 绝不丢行）。
fn fit_rows(rows: &[String], size: ViewportSize) -> Vec<String> {
    let start = rows.len().saturating_sub(size.rows);
    fit_width(&rows[start..], size)
}

/// 只按宽度截断，不裁剪行数。
fn fit_width(rows: &[String], size: ViewportSize) -> Vec<String> {
    rows.iter()
        .map(|row| truncate_to_width(row, size.columns, "", false))
        .collect()
}

/// 擦除从 `top` 行到屏幕底部（含旧 viewport 区域）。
fn erase_below(top: usize) -> String {
    format!("\x1b[{};1H\r\x1b[J", top + 1)
}

/// 顺序写行；`leading_newline` 为真时每行前加 CRLF（history 从旧 viewport 顶部开始）。
fn write_rows(bytes: &mut String, rows: &[String], columns: usize, leading_newline: bool) {
    for (index, row) in rows.iter().enumerate() {
        if index > 0 || leading_newline {
            bytes.push_str("\r\n");
        }
        bytes.push_str(row);
        let width = visible_width(row);
        if width < columns {
            bytes.push_str("\x1b[K");
        }
    }
}

/// 提取最后一个光标标记并剥掉所有标记；返回 (行, 列, 可见)。
fn extract_cursor(rows: &mut [String]) -> Option<(usize, usize, bool)> {
    let mut found = None;
    for (row_index, row) in rows.iter_mut().enumerate() {
        while let Some(index) = row.find(CURSOR_MARKER) {
            let prefix = &row[..index];
            let column = visible_width(prefix);
            found = Some((row_index, column, true));
            row.replace_range(index..index + CURSOR_MARKER.len(), "");
        }
    }
    found
}

/// 把 overlay 合成进 viewport；overlay 行不进入 history。
fn compose_overlay(base: &[String], overlay: &OverlayFrame, size: ViewportSize) -> Vec<String> {
    let mut result = base.to_vec();
    if overlay.full_screen {
        return fit_rows(&overlay.lines, size);
    }
    let needed = overlay.row + overlay.lines.len();
    while result.len() < needed {
        result.push(String::new());
    }
    for (offset, line) in overlay.lines.iter().enumerate() {
        let index = overlay.row + offset;
        if index >= result.len() {
            break;
        }
        let current = &result[index];
        let left = kiana_tui::text::slice_by_column(current, 0, overlay.column, true);
        let left_width = visible_width(&left);
        let mut composed = left;
        if left_width < overlay.column {
            composed.push_str(&" ".repeat(overlay.column - left_width));
        }
        composed.push_str(line);
        let overlay_width = visible_width(line);
        let right_start = overlay.column + overlay_width;
        let right = kiana_tui::text::slice_by_column(current, right_start, size.columns, false);
        composed.push_str(&right);
        result[index] = truncate_to_width(&composed, size.columns, "", false);
    }
    result
}
