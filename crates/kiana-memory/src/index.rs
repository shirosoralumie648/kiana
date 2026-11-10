//! `MEMORY.md` 索引：渲染、截断、解析与往返校验。
//!
//! 索引是注入 system prompt 的人读文件，因此必须**有界**：
//!
//! - 最多 200 行、25 KB（包含格式头与警告行）；
//! - 每条索引一行，整行最多 200 字符；
//! - 发生截断时只追加 **一条** `> WARNING:` 警告，说明丢掉了多少条。
//!
//! 格式（v1）：
//!
//! ```text
//! <!-- kiana-memory-format: 1 -->
//! <!-- revision: 7 -->
//! - [Short responses](topics/short-responses.md) — Use short responses when debugging
//! ```
//!
//! 解析是严格的：格式头、revision、条目行、可选警告行之外的任何内容都视为文件损坏。
//! 调用方在覆盖前必须能往返解析；否则备份 `.bak` 并拒绝写入（避免静默丢数据）。

use std::fmt;

use crate::config::MemoryLimits;
use crate::entry::is_valid_id;
use crate::error::MemoryError;

/// 当前索引格式版本。
pub const INDEX_FORMAT_VERSION: u32 = 1;
/// 版本头（必须逐字节匹配）。
pub const INDEX_FORMAT_HEADER: &str = "<!-- kiana-memory-format: 1 -->";
/// 截断警告前缀。
pub const INDEX_WARNING_PREFIX: &str = "> WARNING: MEMORY.md was truncated";

/// 索引里的一条（只保留展示所需字段）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IndexEntry {
    /// 条目 id。
    pub id: String,
    /// 显示名称。
    pub name: String,
    /// 一行描述。
    pub description: String,
}

impl IndexEntry {
    /// 构造索引条目。
    pub fn new(
        id: impl Into<String>,
        name: impl Into<String>,
        description: impl Into<String>,
    ) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            description: description.into(),
        }
    }
}

/// `render_index` 的结果。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IndexRender {
    /// 完整文件内容（以换行结尾）。
    pub content: String,
    /// 写入的 revision。
    pub revision: u64,
    /// 实际写入索引的条目数。
    pub entry_count: usize,
    /// 因行/字节/单条上限被丢弃的条目数。
    pub dropped: usize,
    /// 唯一的截断警告（未截断时为 `None`）。
    pub warning: Option<String>,
    /// 文件行数（含格式头/警告）。
    pub line_count: usize,
    /// 文件字节数。
    pub byte_count: usize,
}

/// 解析后的索引。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParsedIndex {
    /// 磁盘上的 revision。
    pub revision: u64,
    /// 条目（顺序即索引顺序）。
    pub entries: Vec<IndexEntry>,
    /// 截断警告（0 或 1 条）。
    pub warnings: Vec<String>,
}

/// 索引解析错误。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum IndexParseError {
    /// 文件声明的格式版本高于本实现。
    UnsupportedVersion(u32),
    /// 结构损坏。
    Corrupt(String),
}

impl fmt::Display for IndexParseError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedVersion(version) => {
                write!(formatter, "unsupported index format version {version}")
            }
            Self::Corrupt(message) => formatter.write_str(message),
        }
    }
}

/// 渲染单条索引行，保证不超过 `max_chars` 个字符。
///
/// 名称里的 `[`/`]` 会被替换成 `(`/`)`，否则会破坏 Markdown 链接；名称过长时先截断
/// 名称，再截断描述。结果始终是一行。
pub fn render_entry_line(entry: &IndexEntry, max_chars: usize) -> String {
    let path = format!("topics/{}.md", entry.id);
    let mut name: String = entry
        .name
        .replace('[', "(")
        .replace(']', ")")
        .replace(['\n', '\r'], " ");
    // 名称最多占 1/4 的行预算，给路径与描述留空间。
    let name_budget = (max_chars / 4).max(1);
    if name.chars().count() > name_budget {
        name = truncate_chars(&name, name_budget);
    }
    let prefix = format!("- [{name}]({path}) — ");
    let prefix_len = prefix.chars().count();
    if prefix_len >= max_chars {
        return truncate_chars(&format!("- [{name}]({path})"), max_chars);
    }
    let available = max_chars - prefix_len;
    let description = truncate_chars(&entry.description.replace(['\n', '\r'], " "), available);
    let mut line = format!("{prefix}{description}");
    if line.chars().count() > max_chars {
        line = truncate_chars(&line, max_chars);
    }
    line
}

/// 渲染索引并按 [`MemoryLimits`] 截断。
///
/// 截断时**预留**格式头与一条警告行，因此返回内容同时满足 `max_index_lines` 与
/// `max_index_bytes`（除非头部本身已经超过预算）。
pub fn render_index(entries: &[IndexEntry], revision: u64, limits: &MemoryLimits) -> IndexRender {
    let header = format!("{INDEX_FORMAT_HEADER}\n<!-- revision: {revision} -->\n");
    let lines: Vec<String> = entries
        .iter()
        .map(|entry| render_entry_line(entry, limits.max_index_entry_chars))
        .collect();

    let header_lines = 2usize;
    let plain: String = {
        let mut content = header.clone();
        for line in &lines {
            content.push_str(line);
            content.push('\n');
        }
        content
    };
    if header_lines + lines.len() <= limits.max_index_lines && plain.len() <= limits.max_index_bytes
    {
        return IndexRender {
            line_count: header_lines + lines.len(),
            byte_count: plain.len(),
            content: plain,
            revision,
            entry_count: lines.len(),
            dropped: 0,
            warning: None,
        };
    }

    // 需要截断：预留 1 行警告。逐步减少条目直到行数与字节都满足预算。
    let max_entries_by_lines = limits
        .max_index_lines
        .saturating_sub(header_lines + 1)
        .min(lines.len());
    let mut allowed = max_entries_by_lines;
    loop {
        let dropped = lines.len() - allowed;
        let warning = format!(
            "{INDEX_WARNING_PREFIX} ({dropped} entries omitted); keep index entries short and move detail into topic files."
        );
        let mut content = String::new();
        content.push_str(&header);
        for line in lines.iter().take(allowed) {
            content.push_str(line);
            content.push('\n');
        }
        content.push_str(&warning);
        content.push('\n');
        let line_count = header_lines + allowed + 1;
        if (line_count <= limits.max_index_lines && content.len() <= limits.max_index_bytes)
            || allowed == 0
        {
            return IndexRender {
                line_count,
                byte_count: content.len(),
                content,
                revision,
                entry_count: allowed,
                dropped,
                warning: Some(warning),
            };
        }
        allowed -= 1;
    }
}

/// 解析索引文件。
pub fn parse_index(content: &str) -> Result<ParsedIndex, IndexParseError> {
    if content.is_empty() {
        return Err(IndexParseError::Corrupt("empty index file".into()));
    }
    let mut lines = content.split('\n');
    // `split('\n')` 对以换行结尾的文件会多出一个空串；只允许最后一个为空。
    let first = lines
        .next()
        .ok_or_else(|| IndexParseError::Corrupt("empty index file".into()))?;
    if first != INDEX_FORMAT_HEADER {
        if let Some(version) = parse_format_version(first)
            && version != INDEX_FORMAT_VERSION
        {
            return Err(IndexParseError::UnsupportedVersion(version));
        }
        return Err(IndexParseError::Corrupt(format!(
            "first line is not `{INDEX_FORMAT_HEADER}`"
        )));
    }
    let revision_line = lines
        .next()
        .ok_or_else(|| IndexParseError::Corrupt("missing revision line".into()))?;
    let revision = parse_revision_line(revision_line)
        .ok_or_else(|| IndexParseError::Corrupt("missing `<!-- revision: N -->` line".into()))?;

    let mut entries = Vec::new();
    let mut warnings = Vec::new();
    let raw_lines: Vec<&str> = lines.collect();
    for (index, line) in raw_lines.iter().enumerate() {
        if line.is_empty() {
            // 只允许文件末尾的一个换行造成的空串。
            if index + 1 == raw_lines.len() {
                continue;
            }
            return Err(IndexParseError::Corrupt(format!(
                "unexpected blank line at {}",
                index + 3
            )));
        }
        if line.starts_with(INDEX_WARNING_PREFIX) {
            if !warnings.is_empty() {
                return Err(IndexParseError::Corrupt(
                    "more than one truncation warning".into(),
                ));
            }
            if index + 1 != raw_lines.len()
                && !(index + 2 == raw_lines.len() && raw_lines[index + 1].is_empty())
            {
                return Err(IndexParseError::Corrupt(
                    "truncation warning must be the last line".into(),
                ));
            }
            warnings.push((*line).to_string());
            continue;
        }
        if !warnings.is_empty() {
            return Err(IndexParseError::Corrupt(
                "entry after truncation warning".into(),
            ));
        }
        let entry = parse_entry_line(line).map_err(|message| {
            IndexParseError::Corrupt(format!("line {}: {message}", index + 3))
        })?;
        // 往返校验：解析出的条目重新渲染必须逐字一致。
        let canonical = render_entry_line(&entry, usize::MAX);
        if canonical != *line {
            return Err(IndexParseError::Corrupt(format!(
                "line {} is not canonical: `{line}`",
                index + 3
            )));
        }
        entries.push(entry);
    }
    Ok(ParsedIndex {
        revision,
        entries,
        warnings,
    })
}

/// 解析索引的 revision；损坏时返回 `None`。
pub fn index_revision(content: &str) -> Option<u64> {
    parse_index(content).ok().map(|parsed| parsed.revision)
}

fn parse_format_version(line: &str) -> Option<u32> {
    let rest = line.strip_prefix("<!-- kiana-memory-format: ")?;
    let number = rest.strip_suffix(" -->")?;
    number.trim().parse().ok()
}

fn parse_revision_line(line: &str) -> Option<u64> {
    let rest = line.strip_prefix("<!-- revision: ")?;
    let number = rest.strip_suffix(" -->")?;
    number.trim().parse().ok()
}

fn parse_entry_line(line: &str) -> Result<IndexEntry, String> {
    let rest = line
        .strip_prefix("- [")
        .ok_or_else(|| "entry must start with `- [`".to_string())?;
    let name_end = rest
        .find("](")
        .ok_or_else(|| "entry is missing `](`".to_string())?;
    let name = &rest[..name_end];
    let after_name = &rest[name_end + 2..];
    let path_end = after_name
        .find(')')
        .ok_or_else(|| "entry is missing `)`".to_string())?;
    let path = &after_name[..path_end];
    let tail = &after_name[path_end + 1..];
    let description = tail
        .strip_prefix(" — ")
        .ok_or_else(|| "entry is missing ` — ` before the description".to_string())?;
    let file = path
        .strip_prefix("topics/")
        .and_then(|value| value.strip_suffix(".md"))
        .ok_or_else(|| format!("entry path `{path}` is not `topics/<id>.md`"))?;
    if !is_valid_id(file) {
        return Err(format!("entry path has invalid id `{file}`"));
    }
    Ok(IndexEntry {
        id: file.to_string(),
        name: name.to_string(),
        description: description.to_string(),
    })
}

/// 把 `MemoryError` 从索引解析错误转换出来（附带备份路径）。
pub(crate) fn corrupt_index_error(
    path: std::path::PathBuf,
    error: IndexParseError,
    backup: Option<std::path::PathBuf>,
) -> MemoryError {
    MemoryError::CorruptIndex {
        path,
        message: error.to_string(),
        backup,
    }
}

/// 按字符截断并加省略号；`max == 0` 返回空串。
pub fn truncate_chars(value: &str, max: usize) -> String {
    if max == 0 {
        return String::new();
    }
    let count = value.chars().count();
    if count <= max {
        return value.to_string();
    }
    let mut out: String = value.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn limits() -> MemoryLimits {
        MemoryLimits::default()
    }

    #[test]
    fn empty_index_round_trips() {
        let render = render_index(&[], 3, &limits());
        assert_eq!(render.line_count, 2);
        let parsed = parse_index(&render.content).unwrap();
        assert_eq!(parsed.revision, 3);
        assert!(parsed.entries.is_empty());
    }

    #[test]
    fn truncation_has_exactly_one_warning_and_respects_caps() {
        let entries: Vec<IndexEntry> = (0..500)
            .map(|index| {
                IndexEntry::new(
                    format!("entry-{index}"),
                    format!("Entry {index}"),
                    "x".repeat(120),
                )
            })
            .collect();
        let render = render_index(&entries, 9, &limits());
        assert!(render.content.lines().count() <= 200);
        assert!(render.content.len() <= 25_000);
        assert_eq!(
            render
                .content
                .lines()
                .filter(|line| line.starts_with(INDEX_WARNING_PREFIX))
                .count(),
            1
        );
        assert!(render.dropped > 0);
        for line in render.content.lines().skip(2) {
            assert!(line.chars().count() <= 200, "line too long: {line}");
        }
        parse_index(&render.content).unwrap();
    }

    #[test]
    fn entry_lines_are_bounded() {
        let line = render_entry_line(&IndexEntry::new("x", "n".repeat(500), "d".repeat(500)), 200);
        assert!(line.chars().count() <= 200);
    }

    #[test]
    fn noncanonical_index_is_rejected() {
        let bad =
            format!("{INDEX_FORMAT_HEADER}\n<!-- revision: 1 -->\n- [x](topics/x.md) - desc\n");
        assert!(parse_index(&bad).is_err());
    }

    #[test]
    fn future_version_is_reported() {
        let future = "<!-- kiana-memory-format: 99 -->\n<!-- revision: 1 -->\n";
        assert_eq!(
            parse_index(future),
            Err(IndexParseError::UnsupportedVersion(99))
        );
    }
}
