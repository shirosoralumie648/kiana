//! JSON 结构大纲：为超大 JSON 工具输出生成带**字节偏移/长度**的路径清单，
//! 让模型/调用方可以按区间回读，而不是整段重发。
//!
//! 设计来源（只借鉴行为，不复制代码）：
//! - `reference_project/oh-my-pi/packages/coding-agent/src/tools/read.ts` 的 JSON
//!   结构概览（大型 JSON 先给 shape，再按路径读取）；
//! - `reference_project/ClaudeCode/src/utils/toolResultStorage.ts` 的“大结果给引用 +
//!   预览”原则：本模块给的是可寻址的引用。
//!
//! # 输出契约
//!
//! [`JsonOutline`] 中的每个 [`OutlineEntry`] 都满足：
//!
//! ```text
//! let slice = &json_text[entry.offset .. entry.offset + entry.length];
//! serde_json::from_str::<serde_json::Value>(slice) // 成功
//! ```
//!
//! 即 offset/length 能切出一个**完整、合法**的 JSON 值。容器（object/array）的
//! offset/length 覆盖整个子树；标量覆盖其 token（字符串含引号）。
//!
//! # 边界
//!
//! - 输入必须是合法 JSON（先用 `serde_json` 校验）；错误返回
//!   [`OutlineError::InvalidJson`]。
//! - `max_depth`/`max_entries`/`max_children` 限制输出规模；超限只停止深入，
//!   已生成的 entry 仍然有效。
//! - offset 是 UTF-8 **字节**偏移；扫描器只按 ASCII 结构字符推进，非 ASCII 内容
//!   作为标量区间整体跳过，不会切坏字符边界。
//! - 数字/`true`/`false`/`null` 的 token 边界按 RFC 8259 扫描。

use std::fmt;

use serde_json::Value;
use thiserror::Error;

/// 大纲条目的值类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutlineKind {
    /// 对象。
    Object,
    /// 数组。
    Array,
    /// 字符串。
    String,
    /// 数字。
    Number,
    /// 布尔。
    Bool,
    /// null。
    Null,
}

impl OutlineKind {
    /// 单字符标记：`{ [ " # b n`。
    pub fn marker(self) -> char {
        match self {
            OutlineKind::Object => '{',
            OutlineKind::Array => '[',
            OutlineKind::String => '"',
            OutlineKind::Number => '#',
            OutlineKind::Bool => 'b',
            OutlineKind::Null => 'n',
        }
    }
}

/// 一条结构条目。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutlineEntry {
    /// JSONPath 风格路径：`$`、`$.key`、`$[0]`。键名含特殊字符时用 `$["..."]`。
    pub path: String,
    /// 值类型。
    pub kind: OutlineKind,
    /// 字节偏移（含）。
    pub offset: usize,
    /// 字节长度。
    pub length: usize,
}

impl OutlineEntry {
    /// 从原始 JSON 文本切出该 entry 对应的子串。
    pub fn slice<'a>(&self, text: &'a str) -> Option<&'a str> {
        let end = self.offset.checked_add(self.length)?;
        text.get(self.offset..end)
    }

    /// 子串是否可解析为 JSON。
    pub fn is_valid_json(&self, text: &str) -> bool {
        self.slice(text)
            .and_then(|slice| serde_json::from_str::<Value>(slice).ok())
            .is_some()
    }
}

/// 大纲。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JsonOutline {
    /// 条目（按出现顺序）。
    pub entries: Vec<OutlineEntry>,
    /// 输入总字节数。
    pub total_bytes: usize,
    /// 是否因为深度/条目/子项限制而截断。
    pub truncated: bool,
}

impl JsonOutline {
    /// 按路径查找第一条。
    pub fn find(&self, path: &str) -> Option<&OutlineEntry> {
        self.entries.iter().find(|entry| entry.path == path)
    }

    /// 渲染成模型可读的文本（每行 `path kind offset length`）。
    pub fn render(&self, max_entries: usize) -> String {
        let mut out = String::new();
        out.push_str(&format!(
            "JSON outline ({} bytes, {} entries):\n",
            self.total_bytes,
            self.entries.len()
        ));
        for entry in self.entries.iter().take(max_entries) {
            out.push_str(&format!(
                "{} {} {} {}\n",
                entry.path,
                entry.kind.marker(),
                entry.offset,
                entry.length
            ));
        }
        if self.entries.len() > max_entries {
            out.push_str(&format!(
                "... {} more entries\n",
                self.entries.len() - max_entries
            ));
        }
        out
    }
}

/// 大纲生成选项。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutlineOptions {
    /// 最大深度（根为 0）。默认 4。
    pub max_depth: usize,
    /// 最大条目数（含容器）。默认 256。
    pub max_entries: usize,
    /// 单个容器最多展开多少个子项。默认 64。
    pub max_children: usize,
}

impl Default for OutlineOptions {
    fn default() -> Self {
        Self {
            max_depth: 4,
            max_entries: 256,
            max_children: 64,
        }
    }
}

/// 大纲错误。
#[derive(Debug, Error)]
pub enum OutlineError {
    /// 输入不是合法 JSON。
    #[error("invalid JSON: {0}")]
    InvalidJson(String),
}

/// 生成结构大纲。
pub fn structural_outline(
    text: &str,
    options: &OutlineOptions,
) -> Result<JsonOutline, OutlineError> {
    serde_json::from_str::<Value>(text)
        .map_err(|error| OutlineError::InvalidJson(error.to_string()))?;
    let bytes = text.as_bytes();
    let mut scanner = Scanner {
        text,
        bytes,
        options,
        entries: Vec::new(),
        truncated: false,
    };
    let mut index = scanner.skip_ws(0);
    index = scanner.scan_value(index, "$", 0)?;
    // 合法 JSON 后只允许空白。
    if scanner.skip_ws(index) != bytes.len() {
        return Err(OutlineError::InvalidJson(
            "trailing characters after JSON value".to_string(),
        ));
    }
    Ok(JsonOutline {
        entries: scanner.entries,
        total_bytes: bytes.len(),
        truncated: scanner.truncated,
    })
}

struct Scanner<'a> {
    text: &'a str,
    bytes: &'a [u8],
    options: &'a OutlineOptions,
    entries: Vec<OutlineEntry>,
    truncated: bool,
}

impl<'a> Scanner<'a> {
    fn skip_ws(&self, mut index: usize) -> usize {
        while index < self.bytes.len() && matches!(self.bytes[index], b' ' | b'\t' | b'\n' | b'\r')
        {
            index += 1;
        }
        index
    }

    fn can_push(&mut self) -> bool {
        if self.entries.len() >= self.options.max_entries {
            self.truncated = true;
            false
        } else {
            true
        }
    }

    fn push(&mut self, path: &str, kind: OutlineKind, offset: usize, length: usize) {
        if self.can_push() {
            self.entries.push(OutlineEntry {
                path: path.to_string(),
                kind,
                offset,
                length,
            });
        }
    }

    /// 扫描一个值，返回其结束位置（不含）。
    fn scan_value(
        &mut self,
        index: usize,
        path: &str,
        depth: usize,
    ) -> Result<usize, OutlineError> {
        if index >= self.bytes.len() {
            return Err(OutlineError::InvalidJson(
                "unexpected end of input".to_string(),
            ));
        }
        match self.bytes[index] {
            b'{' => self.scan_object(index, path, depth),
            b'[' => self.scan_array(index, path, depth),
            b'"' => {
                let end = self.scan_string(index)?;
                self.push(path, OutlineKind::String, index, end - index);
                Ok(end)
            }
            b't' => self.scan_literal(index, b"true", path, OutlineKind::Bool),
            b'f' => self.scan_literal(index, b"false", path, OutlineKind::Bool),
            b'n' => self.scan_literal(index, b"null", path, OutlineKind::Null),
            b'-' | b'0'..=b'9' => self.scan_number(index, path),
            other => Err(OutlineError::InvalidJson(format!(
                "unexpected character {:?} at byte {index}",
                other as char
            ))),
        }
    }

    fn scan_literal(
        &mut self,
        index: usize,
        literal: &[u8],
        path: &str,
        kind: OutlineKind,
    ) -> Result<usize, OutlineError> {
        if self.bytes[index..].starts_with(literal) {
            let end = index + literal.len();
            self.push(path, kind, index, literal.len());
            Ok(end)
        } else {
            Err(OutlineError::InvalidJson(format!(
                "invalid literal at byte {index}"
            )))
        }
    }

    fn scan_number(&mut self, index: usize, path: &str) -> Result<usize, OutlineError> {
        let mut cursor = index;
        if self.bytes.get(cursor) == Some(&b'-') {
            cursor += 1;
        }
        let int_start = cursor;
        while self
            .bytes
            .get(cursor)
            .is_some_and(|byte| byte.is_ascii_digit())
        {
            cursor += 1;
        }
        if cursor == int_start {
            return Err(OutlineError::InvalidJson(format!(
                "invalid number at byte {index}"
            )));
        }
        if self.bytes.get(cursor) == Some(&b'.') {
            cursor += 1;
            while self
                .bytes
                .get(cursor)
                .is_some_and(|byte| byte.is_ascii_digit())
            {
                cursor += 1;
            }
        }
        if matches!(self.bytes.get(cursor), Some(b'e' | b'E')) {
            cursor += 1;
            if matches!(self.bytes.get(cursor), Some(b'+' | b'-')) {
                cursor += 1;
            }
            while self
                .bytes
                .get(cursor)
                .is_some_and(|byte| byte.is_ascii_digit())
            {
                cursor += 1;
            }
        }
        self.push(path, OutlineKind::Number, index, cursor - index);
        Ok(cursor)
    }

    /// 扫描字符串，返回结束引号之后的位置。
    fn scan_string(&self, index: usize) -> Result<usize, OutlineError> {
        let mut cursor = index + 1;
        while cursor < self.bytes.len() {
            match self.bytes[cursor] {
                b'\\' => cursor += 2,
                b'"' => return Ok(cursor + 1),
                _ => cursor += 1,
            }
        }
        Err(OutlineError::InvalidJson(format!(
            "unterminated string at byte {index}"
        )))
    }

    fn scan_object(
        &mut self,
        index: usize,
        path: &str,
        depth: usize,
    ) -> Result<usize, OutlineError> {
        let mut cursor = self.skip_ws(index + 1);
        let mut child_count = 0usize;
        let mut first = true;
        loop {
            if self.bytes.get(cursor) == Some(&b'}') {
                cursor += 1;
                break;
            }
            if !first {
                if self.bytes.get(cursor) != Some(&b',') {
                    return Err(OutlineError::InvalidJson(format!(
                        "expected ',' in object at byte {cursor}"
                    )));
                }
                cursor = self.skip_ws(cursor + 1);
            }
            first = false;
            if self.bytes.get(cursor) != Some(&b'"') {
                return Err(OutlineError::InvalidJson(format!(
                    "expected object key at byte {cursor}"
                )));
            }
            let key_end = self.scan_string(cursor)?;
            let key = serde_json::from_str::<String>(&self.text[cursor..key_end])
                .unwrap_or_else(|_| self.text[cursor + 1..key_end.saturating_sub(1)].to_string());
            cursor = self.skip_ws(key_end);
            if self.bytes.get(cursor) != Some(&b':') {
                return Err(OutlineError::InvalidJson(format!(
                    "expected ':' in object at byte {cursor}"
                )));
            }
            cursor = self.skip_ws(cursor + 1);
            let child_path = object_path(path, &key);
            let child_count_exceeded = child_count >= self.options.max_children;
            if child_count_exceeded {
                self.truncated = true;
                // 仍然扫描过值以找到对象结尾，但不生成 entry。
                cursor = self.scan_value_without_entry(cursor)?;
            } else {
                let child_depth = depth.saturating_add(1);
                if child_depth <= self.options.max_depth {
                    cursor = self.scan_value(cursor, &child_path, child_depth)?;
                } else {
                    self.truncated = true;
                    cursor = self.scan_value_without_entry(cursor)?;
                }
                child_count += 1;
            }
            cursor = self.skip_ws(cursor);
        }
        self.push(path, OutlineKind::Object, index, cursor - index);
        Ok(cursor)
    }

    fn scan_array(
        &mut self,
        index: usize,
        path: &str,
        depth: usize,
    ) -> Result<usize, OutlineError> {
        let mut cursor = self.skip_ws(index + 1);
        let mut child_index = 0usize;
        let mut first = true;
        loop {
            if self.bytes.get(cursor) == Some(&b']') {
                cursor += 1;
                break;
            }
            if !first {
                if self.bytes.get(cursor) != Some(&b',') {
                    return Err(OutlineError::InvalidJson(format!(
                        "expected ',' in array at byte {cursor}"
                    )));
                }
                cursor = self.skip_ws(cursor + 1);
            }
            first = false;
            let child_path = format!("{path}[{child_index}]");
            let child_count_exceeded = child_index >= self.options.max_children;
            if child_count_exceeded {
                self.truncated = true;
                cursor = self.scan_value_without_entry(cursor)?;
            } else {
                let child_depth = depth.saturating_add(1);
                if child_depth <= self.options.max_depth {
                    cursor = self.scan_value(cursor, &child_path, child_depth)?;
                } else {
                    self.truncated = true;
                    cursor = self.scan_value_without_entry(cursor)?;
                }
                child_index += 1;
            }
            cursor = self.skip_ws(cursor);
        }
        self.push(path, OutlineKind::Array, index, cursor - index);
        Ok(cursor)
    }

    /// 跳过值但不生成 entry（用于深度/子项超限）。
    fn scan_value_without_entry(&mut self, index: usize) -> Result<usize, OutlineError> {
        let saved_entries = self.entries.len();
        let end = self.scan_value(index, "$", usize::MAX)?;
        self.entries.truncate(saved_entries);
        Ok(end)
    }
}

fn object_path(parent: &str, key: &str) -> String {
    if key.is_empty()
        || !key
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '-')
    {
        format!("{parent}[{:?}]", key)
    } else {
        format!("{parent}.{key}")
    }
}

impl fmt::Display for JsonOutline {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.render(usize::MAX))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"{"name":"kiana","count":3,"nested":{"a":[1,2,{"b":true}],"c":null},"unicode":"汉字","flag":false}"#;

    #[test]
    fn every_entry_slices_valid_json() {
        let outline = structural_outline(SAMPLE, &OutlineOptions::default()).unwrap();
        assert!(!outline.entries.is_empty());
        for entry in &outline.entries {
            assert!(
                entry.is_valid_json(SAMPLE),
                "{} ({:?} {} {}) does not slice valid JSON: {:?}",
                entry.path,
                entry.kind,
                entry.offset,
                entry.length,
                entry.slice(SAMPLE)
            );
        }
    }

    #[test]
    fn offsets_and_lengths_are_exact() {
        let outline = structural_outline(SAMPLE, &OutlineOptions::default()).unwrap();
        let root = outline.find("$").unwrap();
        assert_eq!(root.kind, OutlineKind::Object);
        assert_eq!(root.offset, 0);
        assert_eq!(root.length, SAMPLE.len());
        let count = outline.find("$.count").unwrap();
        assert_eq!(count.slice(SAMPLE), Some("3"));
        let unicode = outline.find("$.unicode").unwrap();
        assert_eq!(unicode.slice(SAMPLE), Some("\"汉字\""));
        let bool_value = outline.find("$.nested.a[2].b").unwrap();
        assert_eq!(bool_value.slice(SAMPLE), Some("true"));
    }

    #[test]
    fn depth_limit_still_produces_valid_prefix() {
        let options = OutlineOptions {
            max_depth: 1,
            ..OutlineOptions::default()
        };
        let outline = structural_outline(SAMPLE, &options).unwrap();
        assert!(outline.truncated);
        assert!(outline.find("$.nested").is_some());
        assert!(outline.find("$.nested.a").is_none());
        for entry in &outline.entries {
            assert!(entry.is_valid_json(SAMPLE));
        }
    }

    #[test]
    fn large_json_outline_is_small() {
        let mut value = String::from("[");
        for index in 0..10_000 {
            if index > 0 {
                value.push(',');
            }
            value.push_str(&format!(r#"{{"id":{index},"name":"item-{index}"}}"#));
        }
        value.push(']');
        let outline = structural_outline(&value, &OutlineOptions::default()).unwrap();
        assert!(outline.total_bytes > 300_000);
        assert!(outline.entries.len() <= 256);
        assert!(outline.render(usize::MAX).len() < 20_000);
    }

    #[test]
    fn invalid_json_is_rejected() {
        assert!(matches!(
            structural_outline("{", &OutlineOptions::default()),
            Err(OutlineError::InvalidJson(_))
        ));
        assert!(matches!(
            structural_outline("1 2", &OutlineOptions::default()),
            Err(OutlineError::InvalidJson(_))
        ));
    }
}
