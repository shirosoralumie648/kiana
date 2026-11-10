//! 记忆条目：类型、校验、frontmatter 往返。
//!
//! 主题文件格式（`topics/<id>.md`）：
//!
//! ```markdown
//! ---
//! id: "short-responses"
//! name: "Short responses"
//! description: "Use short responses when debugging"
//! type: feedback
//! revision: 3
//! derived_from: ["incident-42"]
//! source_key: "session-abc:4"
//! ---
//! Use short responses when debugging
//!
//! Why: The user prefers brevity in debug sessions.
//! How to apply: Keep replies to three sentences.
//! ```
//!
//! 约束：
//!
//! - `name` / `description` / `body` 必填；description 单行，写进索引时还会被截断。
//! - `feedback` / `project` 的正文必须有非空的 `Why:` 与 `How to apply:` 行（也接受
//!   `**Why:**` / `**How to apply:**` 这种 Markdown 强调写法），并且 Why 之前要有一条
//!   rule/fact 行。缺失时返回字段级错误（`body.why` / `body.how_to_apply`），调用方
//!   可以在写盘前定位到具体字段。
//! - `derived_from` 记录本条记忆由哪些条目整合而来；`forget` 用它做级联删除。
//! - `source_key` 是抽取幂等键（session + 消息区间）；重复抽取不会产生重复条目。

use serde::{Deserialize, Serialize};

use crate::error::{MemoryError, MemoryResult};

/// 记忆类型。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MemoryType {
    /// 用户角色、偏好、知识背景。
    User,
    /// 用户对工作方式的反馈（rule + Why + How to apply）。
    Feedback,
    /// 项目进行中的事实/决定（fact + Why + How to apply）。
    Project,
    /// 外部系统指针。
    Reference,
}

impl MemoryType {
    /// 稳定字符串。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Feedback => "feedback",
            Self::Project => "project",
            Self::Reference => "reference",
        }
    }

    /// 解析；未知值返回 `None`（旧文件缺 type 时降级为「无类型」）。
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "user" => Some(Self::User),
            "feedback" => Some(Self::Feedback),
            "project" => Some(Self::Project),
            "reference" => Some(Self::Reference),
            _ => None,
        }
    }

    /// 是否要求 rule/fact + Why + How to apply 结构。
    pub fn requires_why_and_how(self) -> bool {
        matches!(self, Self::Feedback | Self::Project)
    }
}

/// 调用方提交的新条目（没有 id/revision；由 store 分配）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MemoryEntryDraft {
    /// 显示名称（唯一）。
    pub name: String,
    /// 一行描述，用于索引与召回选择。
    pub description: String,
    /// 类型。
    pub mtype: MemoryType,
    /// 正文。
    pub body: String,
    /// 派生来源条目 id。
    pub derived_from: Vec<String>,
    /// 抽取幂等键。
    pub source_key: Option<String>,
}

impl MemoryEntryDraft {
    /// 构造一个没有 derived/source 的条目。
    pub fn new(
        name: impl Into<String>,
        description: impl Into<String>,
        mtype: MemoryType,
        body: impl Into<String>,
    ) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            mtype,
            body: body.into(),
            derived_from: Vec::new(),
            source_key: None,
        }
    }

    /// 字段级校验。不会写盘；失败时调用方可以只报告字段名。
    pub fn validate(&self) -> MemoryResult<()> {
        let display = if self.name.trim().is_empty() {
            "<unnamed>".to_string()
        } else {
            self.name.clone()
        };

        if self.name.trim().is_empty() {
            return Err(MemoryError::MissingField {
                entry: display,
                field: "name",
            });
        }
        if self.name.chars().count() > 120 {
            return Err(MemoryError::InvalidField {
                entry: display,
                field: "name",
                message: "name must be at most 120 characters".into(),
            });
        }
        if self.name.contains('\n') || self.name.contains('\r') {
            return Err(MemoryError::InvalidField {
                entry: display,
                field: "name",
                message: "name must be a single line".into(),
            });
        }

        if self.description.trim().is_empty() {
            return Err(MemoryError::MissingField {
                entry: display,
                field: "description",
            });
        }
        if self.description.chars().count() > 300 {
            return Err(MemoryError::InvalidField {
                entry: display,
                field: "description",
                message: "description must be at most 300 characters".into(),
            });
        }
        if self.description.contains('\n') || self.description.contains('\r') {
            return Err(MemoryError::InvalidField {
                entry: display,
                field: "description",
                message: "description must be a single line".into(),
            });
        }

        if self.body.trim().is_empty() {
            return Err(MemoryError::MissingField {
                entry: display,
                field: "body",
            });
        }

        for derived in &self.derived_from {
            if !is_valid_id(derived) {
                return Err(MemoryError::InvalidField {
                    entry: display,
                    field: "derived_from",
                    message: format!("`{derived}` is not a valid entry id"),
                });
            }
        }
        if let Some(source_key) = &self.source_key
            && (source_key.is_empty() || source_key.chars().count() > 256)
        {
            return Err(MemoryError::InvalidField {
                entry: display,
                field: "source_key",
                message: "source_key must be 1..=256 characters".into(),
            });
        }

        if self.mtype.requires_why_and_how() {
            validate_reasoned_body(&display, &self.body)?;
        }
        Ok(())
    }
}

/// 已落盘条目。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MemoryEntry {
    /// 稳定 id（slug，同时是文件名）。
    pub id: String,
    /// 显示名称。
    pub name: String,
    /// 一行描述。
    pub description: String,
    /// 类型。
    pub mtype: MemoryType,
    /// 正文。
    pub body: String,
    /// 条目 revision（每次 replace/patch +1）。
    pub revision: u64,
    /// 派生来源。
    pub derived_from: Vec<String>,
    /// 抽取幂等键。
    pub source_key: Option<String>,
}

impl MemoryEntry {
    /// 由 draft 分配 id/revision。
    pub fn from_draft(draft: MemoryEntryDraft, id: String, revision: u64) -> Self {
        Self {
            id,
            name: draft.name,
            description: draft.description,
            mtype: draft.mtype,
            body: draft.body,
            revision,
            derived_from: draft.derived_from,
            source_key: draft.source_key,
        }
    }

    /// 转回 draft（用于 replace 后重新校验）。
    pub fn to_draft(&self) -> MemoryEntryDraft {
        MemoryEntryDraft {
            name: self.name.clone(),
            description: self.description.clone(),
            mtype: self.mtype,
            body: self.body.clone(),
            derived_from: self.derived_from.clone(),
            source_key: self.source_key.clone(),
        }
    }

    /// 字段校验。
    pub fn validate(&self) -> MemoryResult<()> {
        self.to_draft().validate()
    }

    /// 规范化文件字节。字段顺序即磁盘契约。
    pub fn to_file_bytes(&self) -> Vec<u8> {
        let mut out = String::new();
        out.push_str("---\n");
        out.push_str("id: ");
        out.push_str(&self.id);
        out.push('\n');
        out.push_str("name: ");
        out.push_str(&json_string(&self.name));
        out.push('\n');
        out.push_str("description: ");
        out.push_str(&json_string(&self.description));
        out.push('\n');
        out.push_str("type: ");
        out.push_str(self.mtype.as_str());
        out.push('\n');
        out.push_str("revision: ");
        out.push_str(&self.revision.to_string());
        out.push('\n');
        if !self.derived_from.is_empty() {
            out.push_str("derived_from: ");
            let values: Vec<String> = self.derived_from.iter().map(|v| json_string(v)).collect();
            out.push('[');
            out.push_str(&values.join(", "));
            out.push(']');
            out.push('\n');
        }
        if let Some(source_key) = &self.source_key {
            out.push_str("source_key: ");
            out.push_str(&json_string(source_key));
            out.push('\n');
        }
        out.push_str("---\n");
        out.push_str(&self.body);
        out.into_bytes()
    }

    /// 严格解析完整主题文件；失败返回可供错误展示的消息。
    pub fn from_file_bytes(bytes: &[u8]) -> Result<Self, String> {
        let text = std::str::from_utf8(bytes).map_err(|error| format!("not UTF-8: {error}"))?;
        let (header, body) = split_frontmatter(text)?;
        let fields = parse_fields(&header)?;
        let id = required_field(&fields, "id")?;
        if !is_valid_id(&id) {
            return Err(format!("invalid id `{id}`"));
        }
        let name = required_field(&fields, "name")?;
        let description = required_field(&fields, "description")?;
        let type_value = required_field(&fields, "type")?;
        let mtype =
            MemoryType::parse(&type_value).ok_or_else(|| format!("unknown type `{type_value}`"))?;
        let revision = required_field(&fields, "revision")?
            .parse::<u64>()
            .map_err(|_| "revision is not an unsigned integer".to_string())?;
        let derived_from = match fields.get("derived_from") {
            Some(value) => parse_string_array(value)?,
            None => Vec::new(),
        };
        for derived in &derived_from {
            if !is_valid_id(derived) {
                return Err(format!("invalid derived_from id `{derived}`"));
            }
        }
        let source_key = fields.get("source_key").cloned();

        let entry = Self {
            id,
            name,
            description,
            mtype,
            body,
            revision,
            derived_from,
            source_key,
        };

        // 语义往返：解析 → 规范化 → 再解析，结构必须一致。这样既接受外部格式化，
        // 又拒绝会被规范化悄悄丢字段的输入。
        let canonical = entry.to_file_bytes();
        let reparsed = Self::from_file_bytes_unchecked(&canonical)?;
        if reparsed != entry {
            return Err("frontmatter does not round-trip".into());
        }
        entry.validate().map_err(|error| error.to_string())?;
        Ok(entry)
    }

    fn from_file_bytes_unchecked(bytes: &[u8]) -> Result<Self, String> {
        let text = std::str::from_utf8(bytes).map_err(|error| format!("not UTF-8: {error}"))?;
        let (header, body) = split_frontmatter(text)?;
        let fields = parse_fields(&header)?;
        let id = required_field(&fields, "id")?;
        let name = required_field(&fields, "name")?;
        let description = required_field(&fields, "description")?;
        let type_value = required_field(&fields, "type")?;
        let mtype =
            MemoryType::parse(&type_value).ok_or_else(|| format!("unknown type `{type_value}`"))?;
        let revision = required_field(&fields, "revision")?
            .parse::<u64>()
            .map_err(|_| "revision is not an unsigned integer".to_string())?;
        let derived_from = match fields.get("derived_from") {
            Some(value) => parse_string_array(value)?,
            None => Vec::new(),
        };
        let source_key = fields.get("source_key").cloned();
        Ok(Self {
            id,
            name,
            description,
            mtype,
            body,
            revision,
            derived_from,
            source_key,
        })
    }
}

/// 只解析前 N 行得到的条目头（召回 manifest 只含 frontmatter）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MemoryHeader {
    /// 条目 id。
    pub id: String,
    /// 显示名称（缺失时退回 id）。
    pub name: String,
    /// 描述（缺失时为空串）。
    pub description: String,
    /// 类型（未知/缺失为 `None`）。
    pub mtype: Option<MemoryType>,
    /// revision（缺失为 0）。
    pub revision: u64,
    /// 派生来源。
    pub derived_from: Vec<String>,
    /// 抽取幂等键。
    pub source_key: Option<String>,
}

impl MemoryHeader {
    /// 从文件前若干行（最多 30 行）解析 header。解析失败时返回错误消息。
    ///
    /// 该函数**宽松**：未知字段忽略、缺 name/description/revision 时用默认值；只有
    /// frontmatter 边界损坏或 type 明显非法才报错。召回扫描不应因为一个坏文件整体失败。
    pub fn from_prefix(prefix: &str, fallback_id: &str) -> Result<Self, String> {
        let (header, _) = split_frontmatter(prefix)?;
        let fields = parse_fields(&header)?;
        let id = fields
            .get("id")
            .cloned()
            .unwrap_or_else(|| fallback_id.to_string());
        if !is_valid_id(&id) {
            return Err(format!("invalid id `{id}`"));
        }
        let name = fields.get("name").cloned().unwrap_or_else(|| id.clone());
        let description = fields.get("description").cloned().unwrap_or_default();
        let mtype = fields
            .get("type")
            .and_then(|value| MemoryType::parse(value));
        let revision = fields
            .get("revision")
            .and_then(|value| value.parse::<u64>().ok())
            .unwrap_or_default();
        let derived_from = match fields.get("derived_from") {
            Some(value) => parse_string_array(value).unwrap_or_default(),
            None => Vec::new(),
        };
        let source_key = fields.get("source_key").cloned();
        Ok(Self {
            id,
            name,
            description,
            mtype,
            revision,
            derived_from,
            source_key,
        })
    }
}

/// 生成稳定 slug。输出只含 `[a-z0-9-]`，最多 48 字符；空名字退回 `memory`。
pub fn slugify(name: &str) -> String {
    let mut out = String::new();
    let mut last_dash = false;
    for ch in name.chars().flat_map(char::to_lowercase) {
        if ch.is_ascii_alphanumeric() {
            out.push(ch);
            last_dash = false;
        } else if !last_dash && !out.is_empty() {
            out.push('-');
            last_dash = true;
        }
        if out.len() >= 48 {
            break;
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    if out.is_empty() {
        "memory".to_string()
    } else {
        out
    }
}

/// 校验 id：`[a-z0-9][a-z0-9-]{0,63}`。
pub fn is_valid_id(id: &str) -> bool {
    if id.is_empty() || id.len() > 64 {
        return false;
    }
    let mut chars = id.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !(first.is_ascii_lowercase() || first.is_ascii_digit()) {
        return false;
    }
    chars.all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '-')
}

/// 内容摘要（sha256 hex）。用于 tombstone 的正文指纹，不保存正文。
pub fn content_digest(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(bytes);
    let mut out = String::with_capacity(digest.len() * 2);
    for byte in digest {
        use std::fmt::Write as _;
        let _ = write!(out, "{byte:02x}");
    }
    out
}

/// 查找正文中 Why/How 行：返回 `(value, line_index)`。
fn find_labeled_line<'a>(body: &'a str, labels: &[&str]) -> Option<(&'a str, usize)> {
    for (index, raw_line) in body.lines().enumerate() {
        let line = raw_line.trim();
        if line.is_empty() {
            continue;
        }
        // 允许 `- Why: ...` / `* Why: ...` / `**Why:** ...`。
        let mut stripped = line;
        for prefix in ["- ", "* ", "+ "] {
            if let Some(rest) = stripped.strip_prefix(prefix) {
                stripped = rest.trim_start();
                break;
            }
        }
        let mut normalized = stripped;
        if let Some(rest) = normalized.strip_prefix("**") {
            normalized = rest;
        }
        let lowered = normalized.to_ascii_lowercase();
        for label in labels {
            let Some(rest) = lowered.strip_prefix(label) else {
                continue;
            };
            // label 后必须紧跟冒号（允许 `Why:**` 这种强调写法）。
            if !rest.trim_start().starts_with(':') {
                continue;
            }
            // 从原始行里截取冒号后的文本（保留大小写）。
            let Some(value) = original_after_colon(stripped) else {
                continue;
            };
            let value = value.strip_prefix("**").unwrap_or(value).trim();
            let value = value.strip_suffix("**").unwrap_or(value).trim();
            if !value.is_empty() {
                return Some((value, index));
            }
        }
    }
    None
}

fn original_after_colon(line: &str) -> Option<&str> {
    let index = line.find(':')?;
    Some(line[index + 1..].trim())
}

fn validate_reasoned_body(entry: &str, body: &str) -> MemoryResult<()> {
    let why = find_labeled_line(body, &["why"]);
    let Some((_why_value, why_index)) = why else {
        return Err(MemoryError::MissingField {
            entry: entry.to_string(),
            field: "body.why",
        });
    };
    let how = find_labeled_line(body, &["how to apply", "how_to_apply", "how-to-apply"]);
    let Some(_) = how else {
        return Err(MemoryError::MissingField {
            entry: entry.to_string(),
            field: "body.how_to_apply",
        });
    };

    // rule/fact 必须在 Why 之前：first non-empty line 不能本身就是 Why/How。
    let first = body
        .lines()
        .enumerate()
        .find(|(_, line)| !line.trim().is_empty());
    if let Some((index, line)) = first {
        let lowered = line.trim().to_ascii_lowercase();
        let is_label = lowered.starts_with("why")
            || lowered.starts_with("**why")
            || lowered.starts_with("how to apply")
            || lowered.starts_with("**how to apply")
            || lowered.starts_with("how_to_apply")
            || lowered.starts_with("how-to-apply");
        if is_label || index >= why_index {
            return Err(MemoryError::InvalidField {
                entry: entry.to_string(),
                field: "body",
                message: "body must lead with the rule/fact before `Why:`".into(),
            });
        }
    }

    Ok(())
}

/// 拆分 frontmatter 与正文。
fn split_frontmatter(text: &str) -> Result<(String, String), String> {
    let mut lines = text.split_inclusive('\n');
    let Some(first) = lines.next() else {
        return Err("empty file".into());
    };
    if first.trim_end_matches(['\r', '\n']) != "---" {
        return Err("missing opening `---`".into());
    }
    let mut header = String::new();
    let mut consumed = first.len();
    let mut closed = false;
    for line in lines {
        let trimmed = line.trim_end_matches(['\r', '\n']);
        if trimmed.trim_end() == "---" {
            consumed += line.len();
            closed = true;
            break;
        }
        header.push_str(line);
        consumed += line.len();
    }
    if !closed {
        return Err("missing closing `---`".into());
    }
    let body = text[consumed..].to_string();
    Ok((header, body))
}

/// 解析 `key: value` 行；重复/未知字段报错。
fn parse_fields(header: &str) -> Result<std::collections::BTreeMap<String, String>, String> {
    let mut map = std::collections::BTreeMap::new();
    for (index, raw_line) in header.lines().enumerate() {
        let line = raw_line.trim_end_matches('\r');
        if line.trim().is_empty() {
            return Err(format!(
                "blank line inside frontmatter (line {})",
                index + 1
            ));
        }
        let Some(colon) = line.find(':') else {
            return Err(format!("expected `key: value` on line {}", index + 1));
        };
        let key = line[..colon].trim();
        let value = line[colon + 1..].trim();
        if key.is_empty() {
            return Err(format!("empty key on line {}", index + 1));
        }
        if !matches!(
            key,
            "id" | "name" | "description" | "type" | "revision" | "derived_from" | "source_key"
        ) {
            return Err(format!("unknown frontmatter field `{key}`"));
        }
        if map.contains_key(key) {
            return Err(format!("duplicate frontmatter field `{key}`"));
        }
        let parsed = if value.starts_with('"') {
            serde_json::from_str::<String>(value)
                .map_err(|error| format!("invalid quoted value for `{key}`: {error}"))?
        } else {
            value.to_string()
        };
        map.insert(key.to_string(), parsed);
    }
    Ok(map)
}

fn required_field(
    fields: &std::collections::BTreeMap<String, String>,
    key: &str,
) -> Result<String, String> {
    fields
        .get(key)
        .cloned()
        .ok_or_else(|| format!("missing frontmatter field `{key}`"))
}

fn parse_string_array(value: &str) -> Result<Vec<String>, String> {
    let trimmed = value.trim();
    if trimmed.starts_with('[') {
        serde_json::from_str::<Vec<String>>(trimmed)
            .map_err(|error| format!("invalid derived_from array: {error}"))
    } else if trimmed.is_empty() {
        Ok(Vec::new())
    } else {
        Ok(vec![trimmed.to_string()])
    }
}

fn json_string(value: &str) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "\"\"".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slug_is_stable_and_bounded() {
        assert_eq!(slugify("Short responses!"), "short-responses");
        assert_eq!(slugify("  !!! "), "memory");
        assert!(slugify(&"x".repeat(200)).len() <= 48);
    }

    #[test]
    fn feedback_requires_why_and_how() {
        let draft = MemoryEntryDraft::new("t", "d", MemoryType::Feedback, "rule only");
        assert!(matches!(
            draft.validate(),
            Err(MemoryError::MissingField {
                field: "body.why",
                ..
            })
        ));
        let draft = MemoryEntryDraft::new("t", "d", MemoryType::Feedback, "rule\nWhy: because\n");
        assert!(matches!(
            draft.validate(),
            Err(MemoryError::MissingField {
                field: "body.how_to_apply",
                ..
            })
        ));
        let draft = MemoryEntryDraft::new(
            "t",
            "d",
            MemoryType::Feedback,
            "rule\nWhy: because\nHow to apply: here",
        );
        assert!(draft.validate().is_ok());
    }

    #[test]
    fn markdown_emphasis_labels_are_accepted() {
        let draft = MemoryEntryDraft::new(
            "t",
            "d",
            MemoryType::Project,
            "fact\n**Why:** reason\n**How to apply:** context",
        );
        assert!(draft.validate().is_ok());
    }

    #[test]
    fn file_bytes_round_trip() {
        let entry = MemoryEntry {
            id: "foo".into(),
            name: "Foo: bar".into(),
            description: "desc \"quoted\"".into(),
            mtype: MemoryType::Feedback,
            body: "rule\nWhy: x\nHow to apply: y".into(),
            revision: 2,
            derived_from: vec!["base".into()],
            source_key: Some("session-1:0".into()),
        };
        let bytes = entry.to_file_bytes();
        let parsed = MemoryEntry::from_file_bytes(&bytes).unwrap();
        assert_eq!(parsed, entry);
    }
}
