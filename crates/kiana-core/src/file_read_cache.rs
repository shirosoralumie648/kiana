//! 压缩后恢复用的文件读取缓存。
//!
//! 设计来源（只借鉴行为，不复制代码）：
//! - `reference_project/ClaudeCode/src/utils/fileReadCache.ts` 与
//!   `reference_project/qwen-code/packages/core/src/services/fileReadCache.integration.test.ts`：
//!   压缩后按“最近读取的文件”重建 read state，未恢复的路径由 read gate 拒绝编辑；
//! - `reference_project/oh-my-pi/packages/coding-agent/src/session/session-maintenance.ts`：
//!   恢复内容按预算裁剪，缺失只 warning。
//!
//! # 职责边界
//!
//! 本模块只保存**可序列化的读取快照**（路径、内容指纹、行范围、捕获时间、内容
//! 预览），不直接认识 `kiana-tools` 的 `ReadStateRegistry`。恢复时由
//! `kiana-tools` 用快照里的 `content_hash` 与磁盘当前内容比对，再决定是否重新
//! arm read gate；不匹配则跳过并 warning（编辑会被 gate 拒绝，这是期望行为）。
//!
//! # 预算
//!
//! - 缓存最多保留 [`DEFAULT_MAX_ENTRIES`] 个不同路径（按最近访问去重）；
//! - 每个快照的 `content_excerpt` 最多 [`DEFAULT_PER_FILE_CHARS`] 字符；
//! - 全部快照的 excerpt 合计最多 [`DEFAULT_TOTAL_CHARS`] 字符，超出时淘汰最旧者。
//!
//! 这三个数值与任务规定的“最近 5 文件、总 50k、单文件 5k”中的缓存侧预算一致；
//! 恢复时的“取最近 5 个”由 [`FileReadCache::recent`] 显式指定。

use std::collections::VecDeque;
use std::path::PathBuf;

/// 默认缓存条目数。
pub const DEFAULT_MAX_ENTRIES: usize = 64;
/// 单文件内容预览上限（字符）。
pub const DEFAULT_PER_FILE_CHARS: usize = 5_000;
/// 全部内容预览上限（字符）。
pub const DEFAULT_TOTAL_CHARS: usize = 50_000;

/// 读取时覆盖的行范围（1-based，闭区间）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReadLineRange {
    /// 起始行。
    pub start: u64,
    /// 结束行。
    pub end: u64,
}

impl ReadLineRange {
    /// 构造。
    pub fn new(start: u64, end: u64) -> Self {
        Self { start, end }
    }
}

/// 一次读取的恢复快照。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileReadSnapshot {
    /// 绝对路径。
    pub path: PathBuf,
    /// 内容 FNV-1a 64 位指纹（与 `kiana-tools::read::fingerprint_bytes` 相同算法）。
    pub content_hash: u64,
    /// 文件总行数。
    pub line_count: u64,
    /// 模型实际看到过的行范围（已合并）。
    pub ranges: Vec<ReadLineRange>,
    /// 是否是可编辑文本。
    pub text: bool,
    /// 捕获时间（Unix 毫秒），用于“最近 N 个”排序。
    pub captured_at_ms: i64,
    /// 内容预览（由 [`FileReadCache`] 按预算裁剪）。
    pub content_excerpt: String,
}

impl FileReadSnapshot {
    /// 是否覆盖整个文件。
    pub fn covers_all_lines(&self) -> bool {
        self.text && self.covers(1, self.line_count.max(1))
    }

    /// 是否覆盖 `start..=end`。
    pub fn covers(&self, start: u64, end: u64) -> bool {
        self.ranges
            .iter()
            .any(|range| range.start <= start && end <= range.end)
    }
}

/// 最近读取缓存。
#[derive(Debug, Clone)]
pub struct FileReadCache {
    entries: VecDeque<FileReadSnapshot>,
    max_entries: usize,
    per_file_chars: usize,
    total_chars: usize,
}

impl Default for FileReadCache {
    fn default() -> Self {
        Self::new()
    }
}

impl FileReadCache {
    /// 使用默认预算。
    pub fn new() -> Self {
        Self::with_budget(
            DEFAULT_MAX_ENTRIES,
            DEFAULT_PER_FILE_CHARS,
            DEFAULT_TOTAL_CHARS,
        )
    }

    /// 自定义预算；参数为 0 时分别回退到默认值。
    pub fn with_budget(max_entries: usize, per_file_chars: usize, total_chars: usize) -> Self {
        Self {
            entries: VecDeque::new(),
            max_entries: if max_entries == 0 {
                DEFAULT_MAX_ENTRIES
            } else {
                max_entries
            },
            per_file_chars: if per_file_chars == 0 {
                DEFAULT_PER_FILE_CHARS
            } else {
                per_file_chars
            },
            total_chars: if total_chars == 0 {
                DEFAULT_TOTAL_CHARS
            } else {
                total_chars
            },
        }
    }

    /// 记录一次读取（同一路径以最新快照覆盖旧值；excerpt 超出预算时截断）。
    pub fn record(&mut self, mut snapshot: FileReadSnapshot) {
        snapshot.content_excerpt = truncate_chars(&snapshot.content_excerpt, self.per_file_chars);
        self.entries.retain(|entry| entry.path != snapshot.path);
        self.entries.push_back(snapshot);
        while self.entries.len() > self.max_entries {
            self.entries.pop_front();
        }
        self.enforce_total_budget();
    }

    fn enforce_total_budget(&mut self) {
        let mut total: usize = self
            .entries
            .iter()
            .map(|entry| entry.content_excerpt.chars().count())
            .sum();
        while total > self.total_chars {
            let Some(front) = self.entries.front() else {
                break;
            };
            let chars = front.content_excerpt.chars().count();
            self.entries.pop_front();
            total = total.saturating_sub(chars);
        }
    }

    /// 最近 `limit` 个快照（按捕获时间从新到旧）。
    pub fn recent(&self, limit: usize) -> Vec<FileReadSnapshot> {
        let mut snapshots: Vec<_> = self.entries.iter().cloned().collect();
        snapshots.sort_by(|left, right| right.captured_at_ms.cmp(&left.captured_at_ms));
        snapshots.truncate(limit);
        snapshots
    }

    /// 按路径查找。
    pub fn get(&self, path: &std::path::Path) -> Option<&FileReadSnapshot> {
        self.entries.iter().find(|entry| entry.path == path)
    }

    /// 全部快照（从旧到新）。
    pub fn all(&self) -> Vec<FileReadSnapshot> {
        self.entries.iter().cloned().collect()
    }

    /// 条目数。
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// 是否为空。
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// 清空（例如会话切换后）。
    pub fn clear(&mut self) {
        self.entries.clear();
    }
}

/// 按字符截断（不切坏 UTF-8）。
pub fn truncate_chars(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    text.chars().take(max_chars).collect()
}

// FNV-1a 64 位，与 kiana-tools 的 `fingerprint_bytes` 保持同一算法。
const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

/// 计算内容指纹；恢复快照与 read gate 使用同一算法。
pub fn fingerprint_bytes(bytes: &[u8]) -> u64 {
    let mut hash = FNV_OFFSET;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    hash
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snapshot(path: &str, at: i64, chars: usize) -> FileReadSnapshot {
        FileReadSnapshot {
            path: PathBuf::from(path),
            content_hash: fingerprint_bytes(path.as_bytes()),
            line_count: 10,
            ranges: vec![ReadLineRange::new(1, 10)],
            text: true,
            captured_at_ms: at,
            content_excerpt: "x".repeat(chars),
        }
    }

    #[test]
    fn dedupes_paths_and_keeps_newest() {
        let mut cache = FileReadCache::new();
        cache.record(snapshot("/a", 1, 10));
        cache.record(snapshot("/b", 2, 10));
        cache.record(snapshot("/a", 3, 12));
        assert_eq!(cache.len(), 2);
        assert_eq!(
            cache
                .get(std::path::Path::new("/a"))
                .unwrap()
                .captured_at_ms,
            3
        );
        let recent = cache.recent(2);
        assert_eq!(recent[0].path, PathBuf::from("/a"));
    }

    #[test]
    fn per_file_and_total_budgets_are_enforced() {
        let mut cache = FileReadCache::with_budget(10, 5, 12);
        cache.record(snapshot("/a", 1, 100));
        assert_eq!(
            cache
                .get(std::path::Path::new("/a"))
                .unwrap()
                .content_excerpt
                .chars()
                .count(),
            5
        );
        cache.record(snapshot("/b", 2, 100));
        cache.record(snapshot("/c", 3, 100));
        let total: usize = cache
            .all()
            .iter()
            .map(|entry| entry.content_excerpt.chars().count())
            .sum();
        assert!(total <= 12);
        // 最旧的被淘汰。
        assert!(cache.get(std::path::Path::new("/a")).is_none());
    }

    #[test]
    fn recent_orders_by_capture_time_not_insertion() {
        let mut cache = FileReadCache::new();
        cache.record(snapshot("/old", 1, 1));
        cache.record(snapshot("/new", 100, 1));
        cache.record(snapshot("/mid", 50, 1));
        let recent = cache.recent(3);
        let names: Vec<_> = recent
            .iter()
            .map(|entry| entry.path.to_string_lossy().to_string())
            .collect();
        assert_eq!(names, vec!["/new", "/mid", "/old"]);
    }

    #[test]
    fn fingerprint_matches_known_fnv_value() {
        assert_eq!(fingerprint_bytes(b""), FNV_OFFSET);
        assert_eq!(fingerprint_bytes(b"a"), 0xaf63dc4c8601ec8c);
    }
}
