//! 极小的 YAML frontmatter 解析器（只覆盖 agent 定义需要的子集）。
//!
//! 为什么不引入 `serde_yaml`：本工作流的硬约束禁止修改根 `Cargo.toml`，而 workspace
//! 依赖表里没有 YAML crate；引入新依赖还会扩大许可审查面。参考实现（Claude Code、
//! qwen-code）本身也是手写 frontmatter 分割 + YAML 解析。这里实现一个**受限但严格**
//! 的解析器：
//!
//! - 顶层 `key: value`；
//! - 缩进的 block map / block sequence（`- item`）；
//! - 行内 `[a, b]` 与 `{k: v}`；
//! - 双引号/单引号字符串、布尔、null、整数、浮点；
//! - 行内注释（引号外的 `#`）。
//!
//! 不支持的 YAML 特性（锚点、标签、多行折叠 `|`/`>`、复杂键）会返回错误而不是猜测。
//! 这个取舍是有意的：agent 定义是用户可编辑的配置面，解析失败应显式报错并只跳过
//! 当前文件，而不是把半截配置当成有效定义。

use serde_json::Value as JsonValue;

/// 解析错误：带行号的可读消息。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FrontmatterError {
    /// 1-based 行号（相对于整个文档）。
    pub line: usize,
    /// 错误说明。
    pub message: String,
}

impl FrontmatterError {
    fn new(line: usize, message: impl Into<String>) -> Self {
        Self {
            line,
            message: message.into(),
        }
    }
}

impl std::fmt::Display for FrontmatterError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "line {}: {}", self.line, self.message)
    }
}

impl std::error::Error for FrontmatterError {}

/// 解析后的 frontmatter 文档。
#[derive(Debug, Clone, PartialEq)]
pub struct FrontmatterDocument {
    /// frontmatter map（顶层必须是 map；空 frontmatter 是空 map）。
    pub fields: Vec<(String, YamlValue)>,
    /// `---` 之后的正文（去除首尾空白）。
    pub body: String,
}

impl FrontmatterDocument {
    /// 按 key 查找顶层字段。
    pub fn get(&self, key: &str) -> Option<&YamlValue> {
        self.fields
            .iter()
            .find(|(name, _)| name == key)
            .map(|(_, value)| value)
    }
}

/// YAML 子集值。
#[derive(Debug, Clone, PartialEq)]
pub enum YamlValue {
    /// `null` / `~` / 空值。
    Null,
    /// 布尔。
    Bool(bool),
    /// 整数。
    Int(i64),
    /// 浮点。
    Float(f64),
    /// 字符串。
    Str(String),
    /// 序列。
    Seq(Vec<YamlValue>),
    /// 映射（保持插入序）。
    Map(Vec<(String, YamlValue)>),
}

impl YamlValue {
    /// 转成 `serde_json::Value`。
    pub fn to_json(&self) -> JsonValue {
        match self {
            Self::Null => JsonValue::Null,
            Self::Bool(value) => JsonValue::Bool(*value),
            Self::Int(value) => JsonValue::Number((*value).into()),
            Self::Float(value) => serde_json::Number::from_f64(*value)
                .map(JsonValue::Number)
                .unwrap_or(JsonValue::Null),
            Self::Str(value) => JsonValue::String(value.clone()),
            Self::Seq(items) => JsonValue::Array(items.iter().map(Self::to_json).collect()),
            Self::Map(entries) => {
                let mut object = serde_json::Map::new();
                for (key, value) in entries {
                    object.insert(key.clone(), value.to_json());
                }
                JsonValue::Object(object)
            }
        }
    }

    /// 是否为空值。
    pub fn is_null(&self) -> bool {
        matches!(self, Self::Null)
    }
}

#[derive(Debug, Clone)]
struct Line {
    number: usize,
    indent: usize,
    text: String,
}

/// 解析 `---` 包围的 frontmatter 文档。
pub fn parse_document(text: &str) -> Result<FrontmatterDocument, FrontmatterError> {
    let normalized = text.replace("\r\n", "\n").replace('\r', "\n");
    let normalized = normalized.strip_prefix('\u{feff}').unwrap_or(&normalized);
    let mut lines = normalized.split('\n').enumerate();

    let first = lines
        .next()
        .ok_or_else(|| FrontmatterError::new(1, "empty document"))?;
    if first.1.trim_end() != "---" {
        return Err(FrontmatterError::new(
            1,
            "agent file must start with a `---` frontmatter block",
        ));
    }

    let mut frontmatter_lines = Vec::new();
    let mut body_lines = Vec::new();
    let mut closed = false;
    for (index, raw) in lines {
        let number = index + 1;
        if !closed {
            if raw.trim_end() == "---" {
                closed = true;
                continue;
            }
            frontmatter_lines.push((number, raw.to_string()));
        } else {
            body_lines.push(raw.to_string());
        }
    }
    if !closed {
        return Err(FrontmatterError::new(
            normalized.lines().count(),
            "unterminated frontmatter: missing closing `---`",
        ));
    }

    let lines = prepare_lines(frontmatter_lines)?;
    let mut position = 0usize;
    let fields = if lines.is_empty() {
        Vec::new()
    } else {
        let start_indent = lines[0].indent;
        let value = parse_block(&lines, &mut position, start_indent)?;
        if position != lines.len() {
            return Err(FrontmatterError::new(
                lines[position].number,
                "unexpected content after frontmatter block",
            ));
        }
        match value {
            YamlValue::Map(entries) => entries,
            YamlValue::Null => Vec::new(),
            _ => {
                return Err(FrontmatterError::new(
                    lines[0].number,
                    "frontmatter must be a mapping of keys to values",
                ));
            }
        }
    };

    let body = body_lines.join("\n").trim().to_string();
    Ok(FrontmatterDocument { fields, body })
}

fn prepare_lines(raw: Vec<(usize, String)>) -> Result<Vec<Line>, FrontmatterError> {
    let mut lines = Vec::new();
    for (number, raw) in raw {
        // 参考实现会把 tab 折成两个空格；这里只处理行首缩进，避免用 tab 缩进的旧
        // 文件直接失效。值内部的 tab（例如 `description: a<TAB>b`）是内容的一部分，
        // 必须原样保留。
        let indent_end = raw
            .find(|character| character != ' ' && character != '\t')
            .unwrap_or(raw.len());
        let mut expanded = String::with_capacity(raw.len());
        for character in raw[..indent_end].chars() {
            if character == '\t' {
                expanded.push_str("  ");
            } else {
                expanded.push(character);
            }
        }
        expanded.push_str(&raw[indent_end..]);
        let without_comment = strip_comment(&expanded);
        let trimmed_end = without_comment.trim_end();
        let trimmed = trimmed_end.trim_start();
        if trimmed.is_empty() {
            continue;
        }
        let indent = trimmed_end.len() - trimmed.len();
        lines.push(Line {
            number,
            indent,
            text: trimmed.to_string(),
        });
    }
    Ok(lines)
}

/// 引号只在 token 起始位置开始，并且本行必须有闭合引号。
///
/// YAML 的引号标量必须从一个 token 的开头开始。若把任意 `'` 都当成引号，
/// `description: it's fine # note` 会把注释吞进“字符串”；若只看起始位置，
/// `handles the '90s syntax # note` 这种裸文本里的撇号也会开启引用态并吞掉注释。
/// 双引号使用同一规则。
fn opens_quote(character: char, previous: Option<char>, rest: &str) -> bool {
    matches!(character, '"' | '\'')
        && previous.is_none_or(|previous| {
            previous.is_whitespace() || matches!(previous, ':' | ',' | '[' | '{')
        })
        && rest.contains(character)
}

/// 推进 YAML 引号状态；返回 `true` 表示该字符已被引号/转义消费。
///
/// - 单引号里的 `''` 是一个转义的单引号，不结束引用态；
/// - 双引号里的 `\` 转义下一个字符（`\"` 不结束引用态）。
fn scan_quote(
    quote: &mut Option<char>,
    escaped: &mut bool,
    character: char,
    rest: &str,
    previous: Option<char>,
) -> bool {
    match *quote {
        Some(active) => {
            if *escaped {
                *escaped = false;
                return true;
            }
            if active == '"' && character == '\\' {
                *escaped = true;
                return true;
            }
            if character == active {
                if active == '\'' && rest.starts_with('\'') {
                    // 成对单引号：把第二个也当作字符串内容跳过。
                    *escaped = true;
                    return true;
                }
                *quote = None;
            }
            true
        }
        None => {
            if opens_quote(character, previous, rest) {
                *quote = Some(character);
                true
            } else {
                false
            }
        }
    }
}

/// 去掉引号外的行内注释（`#` 前必须是空白或行首）。
fn strip_comment(line: &str) -> String {
    let mut quote: Option<char> = None;
    let mut escaped = false;
    let mut previous: Option<char> = None;
    for (index, character) in line.char_indices() {
        let rest = &line[index + character.len_utf8()..];
        if scan_quote(&mut quote, &mut escaped, character, rest, previous) {
            previous = Some(character);
            continue;
        }
        if character == '#' && (index == 0 || previous.is_none_or(char::is_whitespace)) {
            return line[..index].to_string();
        }
        previous = Some(character);
    }
    line.to_string()
}

fn parse_block(
    lines: &[Line],
    position: &mut usize,
    indent: usize,
) -> Result<YamlValue, FrontmatterError> {
    if *position >= lines.len() || lines[*position].indent < indent {
        return Ok(YamlValue::Null);
    }
    if lines[*position].indent > indent {
        return Err(FrontmatterError::new(
            lines[*position].number,
            "unexpected indentation",
        ));
    }
    if is_sequence_item(&lines[*position].text) {
        parse_sequence(lines, position, indent)
    } else {
        parse_mapping(lines, position, indent)
    }
}

fn is_sequence_item(text: &str) -> bool {
    text == "-" || text.starts_with("- ")
}

fn parse_mapping(
    lines: &[Line],
    position: &mut usize,
    indent: usize,
) -> Result<YamlValue, FrontmatterError> {
    let mut entries: Vec<(String, YamlValue)> = Vec::new();
    while *position < lines.len() {
        let line = &lines[*position];
        if line.indent < indent {
            break;
        }
        if line.indent > indent {
            return Err(FrontmatterError::new(line.number, "unexpected indentation"));
        }
        if is_sequence_item(&line.text) {
            break;
        }
        let (key, rest) = split_key(&line.text)
            .ok_or_else(|| FrontmatterError::new(line.number, "expected `key: value`"))?;
        if entries.iter().any(|(existing, _)| existing == &key) {
            return Err(FrontmatterError::new(
                line.number,
                format!("duplicate key `{key}`"),
            ));
        }
        *position += 1;
        let value = if rest.trim().is_empty() {
            if *position < lines.len() && lines[*position].indent > indent {
                let child_indent = lines[*position].indent;
                parse_block(lines, position, child_indent)?
            } else {
                YamlValue::Null
            }
        } else {
            parse_scalar(rest.trim(), line.number)?
        };
        entries.push((key, value));
    }
    Ok(YamlValue::Map(entries))
}

fn parse_sequence(
    lines: &[Line],
    position: &mut usize,
    indent: usize,
) -> Result<YamlValue, FrontmatterError> {
    let mut items = Vec::new();
    while *position < lines.len() {
        let line = &lines[*position];
        if line.indent != indent || !is_sequence_item(&line.text) {
            if line.indent > indent {
                return Err(FrontmatterError::new(line.number, "unexpected indentation"));
            }
            break;
        }
        let number = line.number;
        let rest = line.text[1..].trim_start().to_string();
        *position += 1;
        if rest.is_empty() {
            if *position < lines.len() && lines[*position].indent > indent {
                let child_indent = lines[*position].indent;
                items.push(parse_block(lines, position, child_indent)?);
            } else {
                items.push(YamlValue::Null);
            }
            continue;
        }
        if split_key(&rest).is_some() {
            // `- key: value`：把这一项及其后续缩进行重组为独立 block 再解析。
            let item_indent = indent + 2;
            let mut item_lines = vec![Line {
                number,
                indent: item_indent,
                text: rest,
            }];
            while *position < lines.len() && lines[*position].indent > indent {
                let line = &lines[*position];
                let delta = line.indent.saturating_sub(indent + 2);
                item_lines.push(Line {
                    number: line.number,
                    indent: item_indent + delta,
                    text: line.text.clone(),
                });
                *position += 1;
            }
            let mut item_position = 0usize;
            let item = parse_block(&item_lines, &mut item_position, item_indent)?;
            // 局部 `item_lines` 里可能还剩兄弟键：无缩进 block sequence（如
            // `exitCriteria:` 后面紧跟同缩进的 `- item`）会让映射解析提前结束，
            // 之后的行（`budget`/`maxTurns`）若被静默丢弃，硬门禁字段就消失了。
            // 解析器对不支持的 YAML 特性一律报错，这里同样 fail-closed。
            if item_position != item_lines.len() {
                return Err(FrontmatterError::new(
                    item_lines[item_position].number,
                    "unexpected content after sequence item",
                ));
            }
            items.push(item);
        } else {
            items.push(parse_scalar(&rest, number)?);
        }
    }
    Ok(YamlValue::Seq(items))
}

/// 在引号/括号之外找第一个 `:`；要求后面是空白或行尾。
fn split_key(text: &str) -> Option<(String, String)> {
    let mut quote: Option<char> = None;
    let mut escaped = false;
    let mut previous: Option<char> = None;
    let mut depth = 0usize;
    let characters: Vec<(usize, char)> = text.char_indices().collect();
    for (index, (byte_index, character)) in characters.iter().enumerate() {
        let rest = &text[byte_index + character.len_utf8()..];
        if scan_quote(&mut quote, &mut escaped, *character, rest, previous) {
            previous = Some(*character);
            continue;
        }
        match character {
            '[' | '{' => depth += 1,
            ']' | '}' => depth = depth.saturating_sub(1),
            ':' if depth == 0 => {
                let next = characters.get(index + 1).map(|(_, c)| *c);
                if next.is_none_or(|next| next.is_whitespace()) {
                    let key = text[..*byte_index].trim().to_string();
                    let value_from = byte_index + character.len_utf8();
                    return Some((key, text[value_from..].to_string()));
                }
            }
            _ => {}
        }
        previous = Some(*character);
    }
    None
}

/// inline list/map 的递归深度上限。
///
/// 1 MiB 的文件上限不约束嵌套层数；仓库提供的定义可以用几十万层 `[` 在会话启动
/// 时耗尽栈。超过该深度直接报解析错误。
const MAX_INLINE_NESTING: usize = 64;

fn parse_scalar(text: &str, line: usize) -> Result<YamlValue, FrontmatterError> {
    parse_scalar_at(text, line, 0)
}

fn parse_scalar_at(text: &str, line: usize, depth: usize) -> Result<YamlValue, FrontmatterError> {
    if depth > MAX_INLINE_NESTING {
        return Err(FrontmatterError::new(
            line,
            format!("inline nesting exceeds {MAX_INLINE_NESTING} levels"),
        ));
    }
    let text = text.trim();
    if text.is_empty() {
        return Ok(YamlValue::Null);
    }
    if let Some(inner) = text.strip_prefix('"') {
        let Some(inner) = inner.strip_suffix('"') else {
            return Err(FrontmatterError::new(
                line,
                "unterminated double-quoted string",
            ));
        };
        let decoded: String = serde_json::from_str(&format!("\"{inner}\"")).map_err(|error| {
            FrontmatterError::new(line, format!("invalid double-quoted string: {error}"))
        })?;
        return Ok(YamlValue::Str(decoded));
    }
    if let Some(inner) = text.strip_prefix('\'') {
        let Some(inner) = inner.strip_suffix('\'') else {
            return Err(FrontmatterError::new(
                line,
                "unterminated single-quoted string",
            ));
        };
        return Ok(YamlValue::Str(inner.replace("''", "'")));
    }
    if text.starts_with('[') {
        let Some(inner) = text.strip_prefix('[').and_then(|v| v.strip_suffix(']')) else {
            return Err(FrontmatterError::new(line, "unterminated inline list"));
        };
        let items = split_top_level(inner, ',')
            .into_iter()
            .filter(|item| !item.trim().is_empty())
            .map(|item| parse_scalar_at(item.trim(), line, depth + 1))
            .collect::<Result<Vec<_>, _>>()?;
        return Ok(YamlValue::Seq(items));
    }
    if text.starts_with('{') {
        let Some(inner) = text.strip_prefix('{').and_then(|v| v.strip_suffix('}')) else {
            return Err(FrontmatterError::new(line, "unterminated inline map"));
        };
        let mut entries = Vec::new();
        for item in split_top_level(inner, ',') {
            let item = item.trim();
            if item.is_empty() {
                continue;
            }
            let (key, value) = split_key(item).ok_or_else(|| {
                FrontmatterError::new(line, "expected `key: value` in inline map")
            })?;
            if entries.iter().any(|(existing, _)| existing == &key) {
                return Err(FrontmatterError::new(
                    line,
                    format!("duplicate key `{key}`"),
                ));
            }
            entries.push((key, parse_scalar_at(value.trim(), line, depth + 1)?));
        }
        return Ok(YamlValue::Map(entries));
    }
    // 未识别的 YAML 特性必须报错，不能静默回落成普通字符串：`!!seq [...]` 被当成
    // 字符串后会被拆成无效工具 pattern，`&anchor`/`|` 也会让定义语义悄悄改变。
    //
    // `*` 是例外：Kiana 的 frontmatter 用裸 `*` 表示“全部工具”
    // （`tools: *`），这是参照实现的既有语义，不是 YAML alias。只有 `*name`
    // 这种别名形态才拒绝。
    let unsupported = match text.chars().next() {
        Some('&') => Some("anchor"),
        Some('*')
            if text[1..]
                .chars()
                .next()
                .is_some_and(|character| character.is_alphanumeric() || character == '_') =>
        {
            Some("alias")
        }
        Some('!') => Some("tag"),
        _ if matches!(text, "|" | ">" | "|-" | "|+" | ">-" | ">+") => Some("block scalar"),
        _ => None,
    };
    if let Some(kind) = unsupported {
        return Err(FrontmatterError::new(
            line,
            format!("unsupported YAML {kind}"),
        ));
    }
    match text.to_ascii_lowercase().as_str() {
        "null" | "~" => return Ok(YamlValue::Null),
        "true" | "yes" | "on" => return Ok(YamlValue::Bool(true)),
        "false" | "no" | "off" => return Ok(YamlValue::Bool(false)),
        _ => {}
    }
    if let Ok(value) = text.parse::<i64>() {
        return Ok(YamlValue::Int(value));
    }
    if let Ok(value) = text.parse::<f64>()
        && text.contains('.')
    {
        return Ok(YamlValue::Float(value));
    }
    Ok(YamlValue::Str(text.to_string()))
}

/// 按顶层分隔符切分（忽略引号与括号内的分隔符）。
fn split_top_level(text: &str, separator: char) -> Vec<&str> {
    let mut pieces = Vec::new();
    let mut start = 0usize;
    let mut quote: Option<char> = None;
    let mut escaped = false;
    let mut previous: Option<char> = None;
    let mut depth = 0usize;
    for (index, character) in text.char_indices() {
        let rest = &text[index + character.len_utf8()..];
        if scan_quote(&mut quote, &mut escaped, character, rest, previous) {
            previous = Some(character);
            continue;
        }
        match character {
            '[' | '{' => depth += 1,
            ']' | '}' => depth = depth.saturating_sub(1),
            _ if character == separator && depth == 0 => {
                pieces.push(&text[start..index]);
                start = index + character.len_utf8();
            }
            _ => {}
        }
        previous = Some(character);
    }
    pieces.push(&text[start..]);
    pieces
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_flat_fields_and_body() {
        let document = parse_document(
            "---\nname: explore\ndescription: Read-only search agent\nmaxTurns: 12\n---\nYou are an explorer.\n",
        )
        .expect("document parses");
        assert_eq!(
            document.get("name"),
            Some(&YamlValue::Str("explore".into()))
        );
        assert_eq!(document.get("maxTurns"), Some(&YamlValue::Int(12)));
        assert_eq!(document.body, "You are an explorer.");
    }

    #[test]
    fn parses_inline_and_block_lists() {
        let document = parse_document(
            "---\ntools: [read, grep, \"find\"]\ndisallowedTools:\n  - bash\n  - write\n---\nprompt\n",
        )
        .expect("document parses");
        assert_eq!(
            document.get("tools"),
            Some(&YamlValue::Seq(vec![
                YamlValue::Str("read".into()),
                YamlValue::Str("grep".into()),
                YamlValue::Str("find".into()),
            ]))
        );
        assert_eq!(
            document.get("disallowedTools"),
            Some(&YamlValue::Seq(vec![
                YamlValue::Str("bash".into()),
                YamlValue::Str("write".into()),
            ]))
        );
    }

    #[test]
    fn parses_nested_maps_for_hooks_and_mcp_servers() {
        let document = parse_document(
            "---\nname: x\nhooks:\n  PreToolUse:\n    - matcher: Bash\n      command: echo hi\nmcpServers:\n  - slack\n  - github:\n      command: gh\n---\nbody\n",
        )
        .expect("document parses");
        let hooks = document.get("hooks").expect("hooks");
        assert!(matches!(hooks, YamlValue::Map(entries) if entries[0].0 == "PreToolUse"));
    }

    #[test]
    fn quotes_and_comments_are_handled() {
        let document = parse_document(
            "---\ndescription: \"a # not a comment\" # real comment\nname: 'it''s'\n---\nbody\n",
        )
        .expect("document parses");
        assert_eq!(
            document.get("description"),
            Some(&YamlValue::Str("a # not a comment".into()))
        );
        assert_eq!(document.get("name"), Some(&YamlValue::Str("it's".into())));
    }

    #[test]
    fn duplicate_keys_and_missing_delimiters_error() {
        let duplicate = parse_document("---\nname: a\nname: b\n---\nbody").unwrap_err();
        assert!(duplicate.message.contains("duplicate key"));
        let unterminated = parse_document("---\nname: a\nbody").unwrap_err();
        assert!(unterminated.message.contains("unterminated"));
        let missing = parse_document("name: a\nbody").unwrap_err();
        assert!(missing.message.contains("must start"));
    }

    /// 回归（round-17 finding #8）：裸文本里的撇号不是引号，不能吞掉行内注释。
    #[test]
    fn apostrophes_in_plain_scalars_do_not_open_quotes() {
        let document = parse_document(
            "---\ndescription: it's fine # trailing comment\nnote: don't stop # another\n---\nbody",
        )
        .expect("apostrophes are plain characters");
        assert_eq!(
            document.get("description"),
            Some(&YamlValue::Str("it's fine".into()))
        );
        assert_eq!(
            document.get("note"),
            Some(&YamlValue::Str("don't stop".into()))
        );

        // 引号只在 token 起始位置开始：真正包裹起来的 `#` 仍受保护。
        let quoted = parse_document(
            "---\ndescription: 'has # inside' # outside\ntags: [it's, 'a # b']\n---\nbody",
        )
        .expect("quoted scalars still protect `#`");
        assert_eq!(
            quoted.get("description"),
            Some(&YamlValue::Str("has # inside".into()))
        );
        assert_eq!(
            quoted.get("tags"),
            Some(&YamlValue::Seq(vec![
                YamlValue::Str("it's".into()),
                YamlValue::Str("a # b".into()),
            ]))
        );
    }

    /// 回归（round-18 finding #3）：引号内的成对单引号与反斜杠转义必须被扫描器
    /// 识别，转义后的引号不能提前结束引用态。
    #[test]
    fn escaped_quotes_inside_scalars_do_not_end_the_string() {
        let document = parse_document(
            "---\ndescription: 'It''s the #1 fix' # real comment\ntitle: \"a \\\" # b\" # another\n---\nbody",
        )
        .expect("escaped quotes parse");
        assert_eq!(
            document.get("description"),
            Some(&YamlValue::Str("It's the #1 fix".into()))
        );
        assert_eq!(
            document.get("title"),
            Some(&YamlValue::Str("a \" # b".into()))
        );
    }

    /// 回归（round-18 finding #3 续）：token 起始但本行没有闭合引号的撇号
    /// （`'90s`）不是引号，不能吞掉行内注释。
    #[test]
    fn an_unclosed_quote_at_a_token_start_stays_plain_text() {
        let document =
            parse_document("---\ndescription: handles the '90s syntax # note\n---\nbody")
                .expect("a lone apostrophe does not open a quote");
        assert_eq!(
            document.get("description"),
            Some(&YamlValue::Str("handles the '90s syntax".into()))
        );
    }

    /// 回归（round-18 finding #10）：tab 只在行首缩进处展开；值内部的 tab 是内容。
    #[test]
    fn tabs_inside_scalar_values_are_preserved() {
        let document = parse_document("---\ndescription: a\tb\ntags:\n\t- x\n---\nbody")
            .expect("document parses");
        assert_eq!(
            document.get("description"),
            Some(&YamlValue::Str("a\tb".into()))
        );
        assert_eq!(
            document.get("tags"),
            Some(&YamlValue::Seq(vec![YamlValue::Str("x".into())]))
        );
    }

    /// 回归（round-18 finding #11）：未实现的 YAML 特性（anchor/alias/tag/block
    /// scalar）必须显式报错，不能静默回落成普通字符串。
    #[test]
    fn unsupported_yaml_features_are_rejected() {
        for (field, kind) in [
            ("description: &shared value", "anchor"),
            ("description: *shared", "alias"),
            ("description: !!str value", "tag"),
            ("description: |", "block scalar"),
            ("description: >", "block scalar"),
        ] {
            let source = format!("---\n{field}\n---\nbody");
            let error = parse_document(&source).expect_err(field);
            assert!(error.message.contains(kind), "{field}: {error}");
        }

        // 裸 `*` 是 Kiana 的“全部工具”通配符，不是 YAML alias。
        let document = parse_document("---\ntools: *\n---\nbody").expect("wildcard");
        assert_eq!(document.get("tools"), Some(&YamlValue::Str("*".into())));
    }

    /// 回归（round-17 finding #9）：inline map 的重复键不能后值静默覆盖前值。
    #[test]
    fn duplicate_inline_map_keys_are_rejected() {
        let error = parse_document("---\nmeta: {a: 1, a: 2}\n---\nbody").unwrap_err();
        assert!(error.message.contains("duplicate key `a`"), "{error}");
    }

    /// 回归（round-16 finding #2）：`- key: value` 重解析后剩下的“无缩进 block
    /// sequence 之后的兄弟键”不能被静默丢弃。丢掉 `budget`/`maxTurns` 会让
    /// `exitCriteria` 变成空列表，硬门禁被跳过；解析器必须报错。
    #[test]
    fn indentationless_sequences_inside_items_are_rejected_not_silently_truncated() {
        let error = parse_document(
            "---\nstages:\n  - name: review\n    exitCriteria:\n    - tests_passed\n    budget:\n      maxTurns: 5\n---\nbody",
        )
        .expect_err("leftover sibling keys must fail the parse");
        assert!(
            error
                .message
                .contains("unexpected content after sequence item"),
            "{error}"
        );
    }

    #[test]
    fn unsupported_yaml_features_error_instead_of_guessing() {
        let error = parse_document("---\nname: a\ntools:\n  - |\n    read\n---\nbody").unwrap_err();
        assert!(error.message.contains("block scalar"), "{error}");
    }

    #[test]
    fn deeply_nested_inline_values_are_rejected_without_overflowing_the_stack() {
        let depth = MAX_INLINE_NESTING + 5;
        let value = format!("{}x{}", "[".repeat(depth), "]".repeat(depth));
        let document = format!("---\nname: a\nmeta: {value}\n---\nbody");
        let error = parse_document(&document).expect_err("nesting depth must be bounded");
        assert!(error.message.contains("inline nesting exceeds"), "{error}");
    }
}
