//! transcript finality 账本：`active` / `settled` / `committed`。
//!
//! # 语义
//!
//! * **active**：还在变化，必须在 viewport 里实时重绘，永不被退休；
//! * **settled**：已经定稿但仍在 viewport 里（resize 时按当前宽度重排），只有容量压力
//!   才会退休；
//! * **committed**：写端已经接受并落入 scrollback，之后不再重绘。
//!
//! [`TranscriptLedger::peek_retirement`] 只在 **live 行数 > capacity** 时动作：从最老的
//! block 开始，只吃连续 `settled` 前缀，找到「退休后剩余 live 尾部能放进 capacity」的
//! 最短前缀；遇到第一个 `active`/`committed` 就停。batch 生成后保留到
//! [`TranscriptLedger::acknowledge_batch`] 成功，期间原 id 重发（不重新编号、不改变
//! 内容）。
//!
//! # 每帧预算
//!
//! 一次退休最多携带 [`DEFAULT_MAX_RETIRE_ROWS_PER_BATCH`] 行（至少一个 block）。压力
//! 还没解除时余下的 block 在后续帧继续退休，避免恢复长会话时一帧渲染整本账本阻塞输入。
//!
//! # 与参考实现的关系
//!
//! 三态生命周期与「容量压力才退休最短 settled 前缀」取自 oh-my-pi
//! `packages/tui/src/chrome/transcript-container.ts`；这里省略 append-only stable head、
//! 图片 row-split、native surface 等产品级细节，只保留可测试的核心状态机。

use crate::render_core::frame::{
    FrameProvider, HistoryBatch, HistoryBatchKind, TerminalFramePlan, ViewportSize,
};
use kiana_tui::text::truncate_to_width;

/// 单个 batch 默认最多退休的行数（每帧渲染预算）。
pub const DEFAULT_MAX_RETIRE_ROWS_PER_BATCH: usize = 256;

/// block 的 finality。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Finality {
    /// 可变，必须留在 viewport。
    Active,
    /// 已定稿，等容量压力退休。
    Settled,
    /// 已落入 scrollback。
    Committed,
}

/// 一条 transcript block（语义块，不是物理行）。
#[derive(Clone, Debug)]
pub struct TranscriptBlock {
    /// 稳定 id。
    pub id: u64,
    /// 当前 finality。
    pub finality: Finality,
    /// 逻辑行；渲染时按当前宽度截断。
    pub rows: Vec<String>,
}

impl TranscriptBlock {
    fn render(&self, width: usize) -> Vec<String> {
        self.rows
            .iter()
            .map(|row| truncate_to_width(row, width, "", false))
            .collect()
    }
}

/// 三态账本。
#[derive(Debug, Default)]
pub struct TranscriptLedger {
    blocks: Vec<TranscriptBlock>,
    next_block_id: u64,
    next_batch_id: u64,
    pending: Option<HistoryBatch>,
    pending_prefix_len: usize,
    pending_kind: HistoryBatchKind,
    replay_requested: bool,
    flush_requested: bool,
    committed_rows: Vec<String>,
    max_retire_rows_per_batch: usize,
}

impl TranscriptLedger {
    /// 新建账本；`max_retire_rows_per_batch` 使用默认预算。
    pub fn new() -> Self {
        Self {
            max_retire_rows_per_batch: DEFAULT_MAX_RETIRE_ROWS_PER_BATCH,
            ..Self::default()
        }
    }

    /// 调整每帧退休预算（测试用；至少为 1）。
    pub fn with_max_retire_rows(mut self, max_rows: usize) -> Self {
        self.max_retire_rows_per_batch = max_rows.max(1);
        self
    }

    /// 追加一个 active block，返回稳定 id（从 1 开始）。
    pub fn add_block(&mut self, rows: Vec<String>) -> u64 {
        self.next_block_id += 1;
        let id = self.next_block_id;
        self.blocks.push(TranscriptBlock {
            id,
            finality: Finality::Active,
            rows,
        });
        id
    }

    /// 把指定 block 标记为 settled；已 committed 的不能回退。返回是否发生变化。
    pub fn settle(&mut self, id: u64) -> bool {
        let Some(block) = self.blocks.iter_mut().find(|block| block.id == id) else {
            return false;
        };
        if block.finality == Finality::Active {
            block.finality = Finality::Settled;
            return true;
        }
        false
    }

    /// 把所有 active block 标记为 settled（turn 结束时调用）。
    pub fn settle_all(&mut self) -> usize {
        let mut changed = 0usize;
        for block in &mut self.blocks {
            if block.finality == Finality::Active {
                block.finality = Finality::Settled;
                changed += 1;
            }
        }
        changed
    }

    /// 查询 finality；不存在的 id 返回 `None`。
    pub fn finality(&self, id: u64) -> Option<Finality> {
        self.blocks
            .iter()
            .find(|block| block.id == id)
            .map(|block| block.finality)
    }

    /// 尚未 committed 的 block。
    pub fn live_blocks(&self) -> impl Iterator<Item = &TranscriptBlock> {
        self.blocks
            .iter()
            .filter(|block| block.finality != Finality::Committed)
    }

    /// 已提交到 scrollback 的行。
    pub fn committed_rows(&self) -> &[String] {
        &self.committed_rows
    }

    /// 当前等待 ack 的 batch id。
    pub fn pending_batch_id(&self) -> Option<u64> {
        self.pending.as_ref().map(|batch| batch.id)
    }

    /// live 行数（block 之间一个空行分隔）。
    pub fn live_row_count(&self, width: usize) -> usize {
        self.live_rows(width).len()
    }

    /// 完整 live viewport 行（block 之间一个空行分隔）。
    pub fn live_rows(&self, width: usize) -> Vec<String> {
        let live: Vec<&TranscriptBlock> = self.live_blocks().collect();
        let mut rows = Vec::new();
        for (index, block) in live.iter().enumerate() {
            if index > 0 {
                rows.push(String::new());
            }
            rows.extend(block.render(width));
        }
        rows
    }

    /// 请求下一帧返回完整账本 replay。
    pub fn begin_replay(&mut self) {
        self.replay_requested = true;
    }

    /// 请求下一帧退休所有连续 settled 前缀（关停用）。
    pub fn begin_flush(&mut self) {
        self.flush_requested = true;
    }

    /// 是否有待重放请求。
    pub fn replay_requested(&self) -> bool {
        self.replay_requested
    }

    /// 生成 replay batch（同一请求只生成一次 id；ack 后清除请求）。
    pub fn peek_replay(&mut self) -> Option<HistoryBatch> {
        if !self.replay_requested {
            return None;
        }
        if let Some(batch) = &self.pending
            && batch.kind == HistoryBatchKind::Replay
        {
            return Some(batch.clone());
        }
        self.next_batch_id += 1;
        let batch = HistoryBatch::replay(self.next_batch_id, self.committed_rows.clone());
        self.pending = Some(batch.clone());
        self.pending_kind = HistoryBatchKind::Replay;
        self.pending_prefix_len = 0;
        Some(batch)
    }

    /// 在容量压力下生成「最短 settled 前缀」batch；不需要退休时返回 `None`。
    ///
    /// 已有 pending batch 时原样返回同一个 id（provider 在 ack 前不得换 id）。
    pub fn peek_retirement(&mut self, width: usize, capacity: usize) -> Option<HistoryBatch> {
        if let Some(batch) = &self.pending
            && batch.kind == HistoryBatchKind::Append
        {
            return Some(batch.clone());
        }
        let live: Vec<&TranscriptBlock> = self.live_blocks().collect();
        if self.live_rows(width).len() <= capacity {
            return None;
        }

        // 找到最短 settled 前缀；遇到第一个非 settled 立即停止。
        let mut rows_after_prefix = self.live_rows(width).len();
        let mut found = None;
        for (index, block) in live.iter().enumerate() {
            if block.finality != Finality::Settled {
                break;
            }
            // 退休该 block：减去它的行，以及它与下一个 live block 之间的分隔空行。
            rows_after_prefix = rows_after_prefix.saturating_sub(block.render(width).len());
            if index + 1 < live.len() {
                rows_after_prefix = rows_after_prefix.saturating_sub(1);
            }
            if rows_after_prefix <= capacity {
                found = Some(index + 1);
                break;
            }
        }
        let mut prefix_len = found?;

        // 每帧预算：至少一个 block，最多 max_retire_rows_per_batch 行。
        let mut rows = 0usize;
        let mut capped = 0usize;
        for block in live.iter().take(prefix_len) {
            let block_rows = block.render(width).len();
            if capped > 0 && rows + block_rows > self.max_retire_rows_per_batch {
                break;
            }
            rows += block_rows;
            if capped > 0 {
                rows += 1; // 分隔空行
            }
            capped += 1;
        }
        prefix_len = capped.max(1).min(prefix_len);

        let rows = rows_for_prefix(&live, prefix_len, width);
        self.next_batch_id += 1;
        let batch = HistoryBatch::append(self.next_batch_id, rows);
        self.pending = Some(batch.clone());
        self.pending_prefix_len = prefix_len;
        self.pending_kind = HistoryBatchKind::Append;
        Some(batch)
    }

    /// 关停 flush：一次退休所有连续 settled block（受每帧预算限制）。
    pub fn peek_flush(&mut self, width: usize) -> Option<HistoryBatch> {
        if !self.flush_requested {
            return self.peek_retirement(width, 0);
        }
        if let Some(batch) = &self.pending
            && batch.kind == HistoryBatchKind::Append
        {
            return Some(batch.clone());
        }
        let live: Vec<&TranscriptBlock> = self.live_blocks().collect();
        let mut prefix_len = 0usize;
        let mut rows = 0usize;
        for block in &live {
            if block.finality != Finality::Settled {
                break;
            }
            let block_rows = block.render(width).len();
            if prefix_len > 0 && rows + block_rows > self.max_retire_rows_per_batch {
                break;
            }
            rows += block_rows;
            if prefix_len > 0 {
                rows += 1;
            }
            prefix_len += 1;
        }
        if prefix_len == 0 {
            return None;
        }
        let rows = rows_for_prefix(&live, prefix_len, width);
        self.next_batch_id += 1;
        let batch = HistoryBatch::append(self.next_batch_id, rows);
        self.pending = Some(batch.clone());
        self.pending_prefix_len = prefix_len;
        self.pending_kind = HistoryBatchKind::Append;
        Some(batch)
    }

    /// ack 一个 batch；id 不匹配时 no-op。返回是否推进了状态。
    pub fn acknowledge_batch(&mut self, id: u64) -> bool {
        let Some(batch) = self.pending.as_ref() else {
            return false;
        };
        if batch.id != id {
            return false;
        }
        let kind = self.pending_kind;
        let prefix_len = self.pending_prefix_len;
        let rows = batch.rows.clone();
        self.pending = None;

        match kind {
            HistoryBatchKind::Append => {
                let mut retired = 0usize;
                for block in &mut self.blocks {
                    if retired >= prefix_len {
                        break;
                    }
                    if block.finality == Finality::Settled {
                        block.finality = Finality::Committed;
                        retired += 1;
                    }
                }
                self.committed_rows.extend(rows);
                true
            }
            HistoryBatchKind::Replay => {
                // replay 的 rows 就是完整账本；committed_rows 已经是它，不重复追加。
                self.replay_requested = false;
                true
            }
        }
    }
}

/// 组装前缀 batch 的行：block 之间一个空行，最后再带一个尾部分隔行。
fn rows_for_prefix(live: &[&TranscriptBlock], prefix_len: usize, width: usize) -> Vec<String> {
    let mut rows = Vec::new();
    for (index, block) in live.iter().take(prefix_len).enumerate() {
        if index > 0 {
            rows.push(String::new());
        }
        rows.extend(block.render(width));
    }
    // 每个退休 block 拥有一个尾部分隔行，避免与后续 block 粘连。
    rows.push(String::new());
    rows
}

/// 一个把 [`TranscriptLedger`] 接到帧契约上的 provider。
#[derive(Debug)]
pub struct TranscriptFrameProvider {
    ledger: TranscriptLedger,
    flush: bool,
}

impl TranscriptFrameProvider {
    /// 包装账本。
    pub fn new(ledger: TranscriptLedger) -> Self {
        Self {
            ledger,
            flush: false,
        }
    }

    /// 账本引用。
    pub fn ledger(&self) -> &TranscriptLedger {
        &self.ledger
    }

    /// 可变账本引用。
    pub fn ledger_mut(&mut self) -> &mut TranscriptLedger {
        &mut self.ledger
    }
}

impl FrameProvider for TranscriptFrameProvider {
    fn render_frame(&mut self, size: ViewportSize) -> TerminalFramePlan {
        let width = size.columns;
        let capacity = size.rows;
        let history = if self.ledger.replay_requested() {
            self.ledger.peek_replay()
        } else if self.flush {
            self.ledger.peek_flush(width)
        } else {
            self.ledger.peek_retirement(width, capacity)
        };
        let mut viewport = self.ledger.live_rows(width);
        if viewport.len() > size.rows {
            viewport = viewport.split_off(viewport.len() - size.rows);
        }
        TerminalFramePlan { history, viewport }
    }

    fn acknowledge_history(&mut self, id: u64) {
        self.ledger.acknowledge_batch(id);
    }

    fn begin_history_replay(&mut self) {
        self.ledger.begin_replay();
    }

    fn begin_history_flush(&mut self) {
        self.flush = true;
        self.ledger.begin_flush();
    }
}
