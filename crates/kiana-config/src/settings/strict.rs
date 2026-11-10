//! 严格配置诊断：未知字段 / 类型错误指到文件、行、列与 key path。
//!
//! `SettingsManager` 的旧读取路径是**宽松**的：任何字段类型不符都被保留在 `extra` 里，
//! 不会让整份 `settings.json` 失败。这对用户手写的旧 settings 是正确取舍，但管理员下发
//! 的 `managed-settings.json` / system / enterprise 文件如果有拼写错误，静默忽略会让
//! “策略明明配了却没生效”变得不可诊断。本模块因此提供一条**可选**的严格路径：
//!
//! - [`strict_settings_schema`]：settings + managed policy 的已知字段表；
//! - [`parse_strict_settings`]：解析 JSON（语法错误带行/列），再按 schema 校验；
//! - [`validate_strict_settings`]：对已有 [`Value`] 做校验；
//! - [`StrictDiagnostic`]：`file:line:column` + `key_path` + 人话消息。
//!
//! 设计参考：
//! - Claude Code `utils/settings/validation.ts` 对 managed settings 的严格校验（策略文件
//!   配错不能静默失效）；
//! - Codex `config/src/fingerprint.rs` 的路径式元数据（本模块用同样风格的 dotted path）。
//!
//! # 位置信息从哪里来
//!
//! `serde_json` 只在**语法错误**上给出行/列；字段级错误需要自己找位置。这里用一个
//! 与 `serde_json` 结果解耦的轻量扫描器 [`JsonSpanIndex`] 记录每个 key path 的 key
//! 与 value 起点。它不重复解析值语义，只做结构扫描；扫描失败（理论上不会发生在
//! `serde_json` 已接受的文件上）时退化为 `1:1`，诊断本身仍然有效。
//!
//! # 严格范围
//!
//! 顶层已知键来自 [`Settings::known_keys`]（旧 settings 的字段不会被误报），再加上
//! managed policy 的键。对 `permissions` / policy 对象内部启用 unknown-field 检查；
//! 其它嵌套对象（例如 `hooks` 的事件名）保持开放，避免把合法的自定义键当成错误。
//!
//! 这条路径**不替换** `SettingsManager` 的加载逻辑；只有调用方显式选择
//! [`crate::settings::layers::LayerLoadMode::Strict`] 时才会运行。

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::settings::key_path::{child_key_path, index_key_path};
use crate::settings::schema::Settings;

/// 诊断类别。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum StrictDiagnosticKind {
    /// JSON 语法错误。
    ParseError,
    /// schema 里没有这个字段。
    UnknownField,
    /// 字段类型不对（例如 `permissions` 写成数组）。
    InvalidType,
    /// 类型对但值不在允许集合里（例如 `disableBypassPermissionsMode: "yes"`）。
    InvalidValue,
}

impl StrictDiagnosticKind {
    /// 稳定字符串（事件/日志用）。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ParseError => "parse-error",
            Self::UnknownField => "unknown-field",
            Self::InvalidType => "invalid-type",
            Self::InvalidValue => "invalid-value",
        }
    }
}

/// 一条严格诊断。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StrictDiagnostic {
    /// 出问题的文件。
    pub file: PathBuf,
    /// 1-based 行号；无法定位时为 1。
    pub line: usize,
    /// 1-based 列号；无法定位时为 1。
    pub column: usize,
    /// dotted key path；根/语法错误时为空。
    pub key_path: String,
    /// 诊断类别。
    pub kind: StrictDiagnosticKind,
    /// 面向用户的说明（不含 file/line 前缀）。
    pub message: String,
}

impl std::fmt::Display for StrictDiagnostic {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "{}:{}:{}: {}",
            self.file.display(),
            self.line,
            self.column,
            self.message
        )?;
        if !self.key_path.is_empty() {
            write!(formatter, " (at {})", self.key_path)?;
        }
        Ok(())
    }
}

/// 字段的 JSON 类型/取值约束。
#[derive(Debug, Clone, PartialEq)]
pub enum FieldKind {
    /// 任意值。
    Any,
    /// JSON 布尔。
    Bool,
    /// JSON 字符串。
    String,
    /// JSON 数字。
    Number,
    /// JSON 对象；配合 [`FieldSchema::fields`] 做逐键校验。
    Object,
    /// JSON 数组；配合 [`FieldSchema::items`] 做逐项校验。
    Array,
    /// 字符串或字符串数组。
    StringOrArray,
    /// 字符串或布尔。
    StringOrBool,
    /// 布尔 `true` 或字符串 `"disable"`。
    DisableFlag,
    /// 允许的字符串集合。
    Enum(&'static [&'static str]),
}

/// 一个字段的 schema。
#[derive(Debug, Clone, PartialEq)]
pub struct FieldSchema {
    /// 类型约束。
    pub kind: FieldKind,
    /// 对象字段表（`FieldKind::Object` 时使用）。
    pub fields: BTreeMap<String, FieldSchema>,
    /// 数组元素 schema（`FieldKind::Array` 时使用）。
    pub items: Option<Box<FieldSchema>>,
    /// 对象是否允许 schema 之外的键。
    pub allow_unknown: bool,
}

impl FieldSchema {
    /// 接受任意值。
    pub fn any() -> Self {
        Self {
            kind: FieldKind::Any,
            fields: BTreeMap::new(),
            items: None,
            allow_unknown: true,
        }
    }

    /// JSON 布尔。
    pub fn boolean() -> Self {
        Self::leaf(FieldKind::Bool)
    }

    /// JSON 字符串。
    pub fn string() -> Self {
        Self::leaf(FieldKind::String)
    }

    /// JSON 数字。
    pub fn number() -> Self {
        Self::leaf(FieldKind::Number)
    }

    /// 字符串或字符串数组。
    pub fn string_or_array() -> Self {
        Self::leaf(FieldKind::StringOrArray)
    }

    /// 字符串或布尔。
    pub fn string_or_bool() -> Self {
        Self::leaf(FieldKind::StringOrBool)
    }

    /// 布尔 `true` 或字符串 `"disable"`。
    pub fn disable_flag() -> Self {
        Self::leaf(FieldKind::DisableFlag)
    }

    /// 字符串枚举。
    pub fn enum_of(values: &'static [&'static str]) -> Self {
        Self::leaf(FieldKind::Enum(values))
    }

    /// 对象；`fields` 之外出现未知键会被报告。
    pub fn object(fields: impl IntoIterator<Item = (&'static str, FieldSchema)>) -> Self {
        Self {
            kind: FieldKind::Object,
            fields: fields
                .into_iter()
                .map(|(key, schema)| (key.to_string(), schema))
                .collect(),
            items: None,
            allow_unknown: false,
        }
    }

    /// 对象，但接受任意未知键（例如 `hooks` 的事件名）。
    pub fn object_any() -> Self {
        Self {
            kind: FieldKind::Object,
            fields: BTreeMap::new(),
            items: None,
            allow_unknown: true,
        }
    }

    /// 数组。
    pub fn array(items: FieldSchema) -> Self {
        Self {
            kind: FieldKind::Array,
            fields: BTreeMap::new(),
            items: Some(Box::new(items)),
            allow_unknown: true,
        }
    }

    fn leaf(kind: FieldKind) -> Self {
        Self {
            kind,
            fields: BTreeMap::new(),
            items: None,
            allow_unknown: true,
        }
    }
}

/// settings + managed policy 的严格 schema。
///
/// 顶层已知键取自 [`Settings::known_keys`]，因此旧 settings 的全部合法字段都被接受；
/// 这里额外补上 managed policy 键。`permissions` 与 policy 对象内部关闭 unknown 检查，
/// 其余嵌套对象保持开放（见模块文档“严格范围”）。
pub fn strict_settings_schema() -> FieldSchema {
    let mut fields: BTreeMap<String, FieldSchema> = Settings::known_keys()
        .iter()
        .map(|key| ((*key).to_string(), FieldSchema::any()))
        .collect();

    fields.insert(
        "permissions".to_string(),
        FieldSchema::object([
            ("mode", FieldSchema::string()),
            ("defaultMode", FieldSchema::string()),
            ("rules", FieldSchema::array(permission_rule_schema())),
            ("allow", FieldSchema::array(FieldSchema::any())),
            ("deny", FieldSchema::array(FieldSchema::any())),
            ("ask", FieldSchema::array(FieldSchema::any())),
            ("addDirs", FieldSchema::array(FieldSchema::string())),
            ("workspaceRoots", FieldSchema::array(FieldSchema::string())),
            ("disableBypassPermissionsMode", FieldSchema::disable_flag()),
            (
                "disable_bypass_permissions_mode",
                FieldSchema::disable_flag(),
            ),
            (
                "additionalDirectories",
                FieldSchema::array(FieldSchema::string()),
            ),
            (
                "enabledMcpjsonServers",
                FieldSchema::array(FieldSchema::string()),
            ),
            (
                "disabledMcpjsonServers",
                FieldSchema::array(FieldSchema::string()),
            ),
            // managed policy 键同样可以写在 `permissions` 里（`layers.rs` 的
            // `PolicySettings::from_value` 会同时查两处）。
            ("allowedMcpServers", FieldSchema::array(FieldSchema::any())),
            (
                "allowed_mcp_servers",
                FieldSchema::array(FieldSchema::any()),
            ),
            ("deniedMcpServers", FieldSchema::array(FieldSchema::any())),
            ("denied_mcp_servers", FieldSchema::array(FieldSchema::any())),
            (
                "allowManagedPermissionRulesOnly",
                FieldSchema::disable_flag(),
            ),
            (
                "allow_managed_permission_rules_only",
                FieldSchema::disable_flag(),
            ),
            ("allowManagedHooksOnly", FieldSchema::disable_flag()),
            ("allow_managed_hooks_only", FieldSchema::disable_flag()),
        ]),
    );
    // hooks 的事件名是用户自定义的（PreToolUse/PostToolUse/…），只约束最外层类型。
    fields.insert("hooks".to_string(), FieldSchema::object_any());
    // 这些键由运行时通过 `Settings::raw` 读取，但不在 `known_keys` 的声明表里；
    // 不补进来会让合法的 managed settings 报 unknown-field。
    for key in ["continuity", "cron"] {
        fields.insert(key.to_string(), FieldSchema::any());
    }

    for key in [
        "allowManagedPermissionRulesOnly",
        "allow_managed_permission_rules_only",
        "allowManagedHooksOnly",
        "allow_managed_hooks_only",
        "disableBypassPermissionsMode",
        "disable_bypass_permissions_mode",
    ] {
        fields.insert(key.to_string(), FieldSchema::disable_flag());
    }
    for key in [
        "allowedMcpServers",
        "allowed_mcp_servers",
        "deniedMcpServers",
        "denied_mcp_servers",
    ] {
        // 条目既可以是字符串（服务器名简写），也可以是对象；类型不符时由运行时策略
        // 解析负责 fail-closed，这里不因简写/别名拼写报 unknown-field。
        fields.insert(key.to_string(), FieldSchema::array(FieldSchema::any()));
    }

    FieldSchema {
        kind: FieldKind::Object,
        fields,
        items: None,
        allow_unknown: false,
    }
}

fn permission_rule_schema() -> FieldSchema {
    FieldSchema::object([
        ("tool", FieldSchema::string()),
        ("command", FieldSchema::string_or_array()),
        ("path", FieldSchema::string()),
        // 与 `rule_from_settings_value` 保持一致：路径规则可以限定动作。
        ("actions", FieldSchema::array(FieldSchema::string())),
        ("pattern", FieldSchema::string()),
        ("decision", FieldSchema::enum_of(&["allow", "ask", "deny"])),
        ("reason", FieldSchema::string()),
        ("exact", FieldSchema::boolean()),
        ("wildcard", FieldSchema::boolean()),
        // 审批生成的规则：`approved` 标记来源，`origin` 区分内建/自定义工具。
        ("approved", FieldSchema::boolean()),
        ("origin", FieldSchema::enum_of(&["builtin", "custom"])),
    ])
}

/// 对已经解析好的 settings 值做严格校验。
pub fn validate_strict_settings(
    file: &Path,
    text: &str,
    value: &Value,
    schema: &FieldSchema,
) -> Vec<StrictDiagnostic> {
    let spans = JsonSpanIndex::build(text);
    let mut diagnostics = Vec::new();
    validate_value(file, "", value, schema, &spans, &mut diagnostics);
    diagnostics
}

/// 解析并校验一份严格 settings 文件。
///
/// 语法错误返回 `Err`（带 `serde_json` 的 1-based 行列）；语法正确但字段有问题时返回
/// `Ok((value, diagnostics))`，调用方决定是警告还是拒绝加载。
pub fn parse_strict_settings(
    file: &Path,
    text: &str,
    schema: &FieldSchema,
) -> Result<(Value, Vec<StrictDiagnostic>), StrictDiagnostic> {
    let value: Value = serde_json::from_str(text).map_err(|error| StrictDiagnostic {
        file: file.to_path_buf(),
        line: error.line().max(1),
        column: error.column().max(1),
        key_path: String::new(),
        kind: StrictDiagnosticKind::ParseError,
        message: format!("invalid JSON: {error}"),
    })?;
    let diagnostics = validate_strict_settings(file, text, &value, schema);
    Ok((value, diagnostics))
}

fn validate_value(
    file: &Path,
    path: &str,
    value: &Value,
    schema: &FieldSchema,
    spans: &JsonSpanIndex,
    diagnostics: &mut Vec<StrictDiagnostic>,
) {
    if !matches_kind(&schema.kind, value) {
        let (kind, message) = type_error(&schema.kind, value);
        diagnostics.push(diagnostic_at(file, spans, path, false, kind, message));
        return;
    }

    match (&schema.kind, value) {
        (FieldKind::Object, Value::Object(object)) => {
            for (key, child_value) in object {
                let child_path = child_key_path(path, key);
                if let Some(child_schema) = schema.fields.get(key) {
                    validate_value(
                        file,
                        &child_path,
                        child_value,
                        child_schema,
                        spans,
                        diagnostics,
                    );
                } else if !schema.allow_unknown {
                    diagnostics.push(diagnostic_at(
                        file,
                        spans,
                        &child_path,
                        true,
                        StrictDiagnosticKind::UnknownField,
                        format!("unknown field \"{key}\""),
                    ));
                }
            }
        }
        (FieldKind::Array, Value::Array(items)) => {
            if let Some(item_schema) = &schema.items {
                for (index, item) in items.iter().enumerate() {
                    validate_value(
                        file,
                        &index_key_path(path, index),
                        item,
                        item_schema,
                        spans,
                        diagnostics,
                    );
                }
            }
        }
        (FieldKind::Enum(allowed), Value::String(text)) => {
            if !allowed.iter().any(|candidate| candidate == text) {
                let options = allowed
                    .iter()
                    .map(|candidate| format!("\"{candidate}\""))
                    .collect::<Vec<_>>()
                    .join(", ");
                diagnostics.push(diagnostic_at(
                    file,
                    spans,
                    path,
                    false,
                    StrictDiagnosticKind::InvalidValue,
                    format!("value \"{text}\" is not one of {options}"),
                ));
            }
        }
        (FieldKind::DisableFlag, Value::String(text)) => {
            // 运行时（policy.rs 的 disable_flag_enabled）同时接受 "true" 与 "disable"。
            if !text.eq_ignore_ascii_case("disable") && !text.eq_ignore_ascii_case("true") {
                diagnostics.push(diagnostic_at(
                    file,
                    spans,
                    path,
                    false,
                    StrictDiagnosticKind::InvalidValue,
                    "expected true, \"true\", or \"disable\"".to_string(),
                ));
            }
        }
        _ => {}
    }
}

fn matches_kind(kind: &FieldKind, value: &Value) -> bool {
    match kind {
        FieldKind::Any => true,
        FieldKind::Bool => value.is_boolean(),
        FieldKind::String => value.is_string(),
        FieldKind::Number => value.is_number(),
        FieldKind::Object => value.is_object(),
        FieldKind::Array => value.is_array(),
        FieldKind::StringOrArray => match value {
            Value::String(_) => true,
            Value::Array(items) => items.iter().all(Value::is_string),
            _ => false,
        },
        FieldKind::StringOrBool => value.is_string() || value.is_boolean(),
        FieldKind::DisableFlag => {
            value.is_boolean()
                || value.as_str().is_some_and(|text| {
                    text.eq_ignore_ascii_case("disable") || text.eq_ignore_ascii_case("true")
                })
        }
        FieldKind::Enum(_) => value.is_string(),
    }
}

fn type_error(kind: &FieldKind, value: &Value) -> (StrictDiagnosticKind, String) {
    let expected = match kind {
        FieldKind::Any => "any value",
        FieldKind::Bool => "boolean",
        FieldKind::String => "string",
        FieldKind::Number => "number",
        FieldKind::Object => "object",
        FieldKind::Array => "array",
        FieldKind::StringOrArray => "string or array of strings",
        FieldKind::StringOrBool => "string or boolean",
        FieldKind::DisableFlag => "boolean, \"true\", or \"disable\"",
        FieldKind::Enum(_) => "string",
    };
    (
        StrictDiagnosticKind::InvalidType,
        format!("expected {expected}, found {}", describe_value(value)),
    )
}

fn describe_value(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

fn diagnostic_at(
    file: &Path,
    spans: &JsonSpanIndex,
    path: &str,
    use_key_span: bool,
    kind: StrictDiagnosticKind,
    message: String,
) -> StrictDiagnostic {
    let location = spans
        .location(path)
        .map(|span| if use_key_span { span.key } else { span.value })
        .unwrap_or_default();
    StrictDiagnostic {
        file: file.to_path_buf(),
        line: location.line,
        column: location.column,
        key_path: path.to_string(),
        kind,
        message,
    }
}

/// 1-based 行列。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Location {
    line: usize,
    column: usize,
}

impl Default for Location {
    fn default() -> Self {
        Self { line: 1, column: 1 }
    }
}

/// 一个 key path 的 key 起点与 value 起点。
#[derive(Debug, Clone, Copy, Default)]
struct Span {
    key: Location,
    value: Location,
}

/// 轻量 JSON 结构扫描：只记录每个 key path 的起止位置，不理解值语义。
struct JsonSpanIndex {
    spans: BTreeMap<String, Span>,
}

impl JsonSpanIndex {
    fn build(text: &str) -> Self {
        let mut scanner = Scanner::new(text);
        let mut spans = BTreeMap::new();
        if scanner.skip_value("", &mut spans).is_none() {
            // serde_json 已经接受这份文本，理论上不会走到这里；退化为 1:1 而不是丢诊断。
            return Self {
                spans: BTreeMap::new(),
            };
        }
        Self { spans }
    }

    fn location(&self, path: &str) -> Option<Span> {
        self.spans.get(path).copied()
    }
}

struct Scanner {
    characters: Vec<char>,
    index: usize,
    line: usize,
    column: usize,
}

impl Scanner {
    fn new(text: &str) -> Self {
        Self {
            characters: text.chars().collect(),
            index: 0,
            line: 1,
            column: 1,
        }
    }

    fn peek(&self) -> Option<char> {
        self.characters.get(self.index).copied()
    }

    fn bump(&mut self) -> Option<char> {
        let character = self.peek()?;
        self.index += 1;
        if character == '\n' {
            self.line += 1;
            self.column = 1;
        } else {
            self.column += 1;
        }
        Some(character)
    }

    fn location(&self) -> Location {
        Location {
            line: self.line,
            column: self.column,
        }
    }

    fn skip_whitespace(&mut self) {
        while self
            .peek()
            .is_some_and(|character| character.is_whitespace())
        {
            self.bump();
        }
    }

    /// 跳过任意 JSON 值，同时记录 `path` 的 value 起点和所有子路径的起点。
    fn skip_value(&mut self, path: &str, spans: &mut BTreeMap<String, Span>) -> Option<()> {
        self.skip_whitespace();
        spans.entry(path.to_string()).or_default().value = self.location();
        match self.peek()? {
            '"' => {
                self.parse_string()?;
                Some(())
            }
            '{' => {
                self.bump();
                self.skip_whitespace();
                if self.peek() == Some('}') {
                    self.bump();
                    return Some(());
                }
                loop {
                    self.skip_whitespace();
                    let key_location = self.location();
                    let key = self.parse_string()?;
                    self.skip_whitespace();
                    if self.bump()? != ':' {
                        return None;
                    }
                    let child = child_key_path(path, &key);
                    spans.entry(child.clone()).or_default().key = key_location;
                    self.skip_value(&child, spans)?;
                    self.skip_whitespace();
                    match self.bump()? {
                        ',' => continue,
                        '}' => return Some(()),
                        _ => return None,
                    }
                }
            }
            '[' => {
                self.bump();
                self.skip_whitespace();
                if self.peek() == Some(']') {
                    self.bump();
                    return Some(());
                }
                let mut index = 0usize;
                loop {
                    let child = index_key_path(path, index);
                    self.skip_value(&child, spans)?;
                    index += 1;
                    self.skip_whitespace();
                    match self.bump()? {
                        ',' => continue,
                        ']' => return Some(()),
                        _ => return None,
                    }
                }
            }
            't' => self.skip_literal("true"),
            'f' => self.skip_literal("false"),
            'n' => self.skip_literal("null"),
            _ => {
                while self.peek().is_some_and(|character| {
                    character.is_ascii_digit() || "+-.eE".contains(character)
                }) {
                    self.bump();
                }
                Some(())
            }
        }
    }

    fn skip_literal(&mut self, literal: &str) -> Option<()> {
        for expected in literal.chars() {
            if self.bump()? != expected {
                return None;
            }
        }
        Some(())
    }

    fn parse_string(&mut self) -> Option<String> {
        if self.peek()? != '"' {
            return None;
        }
        self.bump();
        let mut output = String::new();
        loop {
            match self.bump()? {
                '"' => return Some(output),
                '\\' => match self.bump()? {
                    '"' => output.push('"'),
                    '\\' => output.push('\\'),
                    '/' => output.push('/'),
                    'b' => output.push('\u{0008}'),
                    'f' => output.push('\u{000C}'),
                    'n' => output.push('\n'),
                    'r' => output.push('\r'),
                    't' => output.push('\t'),
                    'u' => {
                        let first = self.read_hex4()?;
                        let code = if (0xD800..=0xDBFF).contains(&first) {
                            // 配对代理项；不配对时用 U+FFFD 占位（只需定位，不需要逐字还原）。
                            if self.peek() == Some('\\')
                                && self.characters.get(self.index + 1) == Some(&'u')
                            {
                                self.bump();
                                self.bump();
                                let second = self.read_hex4()?;
                                if (0xDC00..=0xDFFF).contains(&second) {
                                    0x1_0000
                                        + ((first as u32 - 0xD800) << 10)
                                        + (second as u32 - 0xDC00)
                                } else {
                                    0xFFFD
                                }
                            } else {
                                0xFFFD
                            }
                        } else if (0xDC00..=0xDFFF).contains(&first) {
                            0xFFFD
                        } else {
                            first as u32
                        };
                        output.push(char::from_u32(code).unwrap_or('\u{FFFD}'));
                    }
                    _ => return None,
                },
                character => output.push(character),
            }
        }
    }

    fn read_hex4(&mut self) -> Option<u16> {
        let mut value = 0u16;
        for _ in 0..4 {
            let digit = self.bump()?.to_digit(16)?;
            value = value.checked_mul(16)?.checked_add(digit as u16)?;
        }
        Some(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn diagnostics(text: &str) -> Vec<StrictDiagnostic> {
        let schema = strict_settings_schema();
        let (_, diagnostics) =
            parse_strict_settings(Path::new("managed.json"), text, &schema).expect("valid JSON");
        diagnostics
    }

    #[test]
    fn reports_unknown_field_with_line_column_and_key_path() {
        let text =
            "{\n  \"permissions\": {\n    \"mode\": \"default\",\n    \"allowed\": []\n  }\n}";
        let diagnostics = diagnostics(text);
        assert_eq!(diagnostics.len(), 1);
        let diagnostic = &diagnostics[0];
        assert_eq!(diagnostic.kind, StrictDiagnosticKind::UnknownField);
        assert_eq!(diagnostic.key_path, "permissions.allowed");
        assert_eq!(diagnostic.line, 4);
        assert_eq!(diagnostic.column, 5);
        assert!(diagnostic.message.contains("allowed"));
    }

    #[test]
    fn reports_type_error_at_the_value_position() {
        let text = "{\n  \"permissions\": []\n}";
        let diagnostics = diagnostics(text);
        assert_eq!(diagnostics.len(), 1);
        let diagnostic = &diagnostics[0];
        assert_eq!(diagnostic.kind, StrictDiagnosticKind::InvalidType);
        assert_eq!(diagnostic.key_path, "permissions");
        assert_eq!(diagnostic.line, 2);
        assert_eq!(diagnostic.column, 18);
        assert_eq!(diagnostic.message, "expected object, found array");
    }

    #[test]
    fn reports_invalid_enum_value_and_nested_unknown_rule_field() {
        let text = r#"{
  "permissions": {
    "rules": [
      {"tool": "bash", "decision": "sometimes", "exactt": true}
    ]
  }
}"#;
        let diagnostics = diagnostics(text);
        assert_eq!(diagnostics.len(), 2);
        assert!(diagnostics.iter().any(|diagnostic| diagnostic.kind
            == StrictDiagnosticKind::UnknownField
            && diagnostic.key_path == "permissions.rules.0.exactt"));
        assert!(diagnostics.iter().any(|diagnostic| {
            diagnostic.kind == StrictDiagnosticKind::InvalidValue
                && diagnostic.key_path == "permissions.rules.0.decision"
        }));
    }

    #[test]
    fn policy_keys_are_type_checked() {
        let text = r#"{"allowManagedPermissionRulesOnly": "yes"}"#;
        let diagnostics = diagnostics(text);
        assert_eq!(diagnostics.len(), 1);
        assert_eq!(diagnostics[0].kind, StrictDiagnosticKind::InvalidType);
        assert_eq!(diagnostics[0].key_path, "allowManagedPermissionRulesOnly");
    }

    #[test]
    fn legacy_settings_fields_are_not_unknown() {
        let text = r#"{
  "theme": "dark",
  "lastChangelogVersion": "1.0.0",
  "compaction": {"enabled": true, "reserveTokens": 100},
  "hooks": {"PreToolUse": [{"matcher": "bash", "hooks": [{"type": "command"}]}]}
}"#;
        assert!(diagnostics(text).is_empty());
    }

    #[test]
    fn syntax_errors_carry_one_based_line_and_column() {
        let schema = strict_settings_schema();
        let error = parse_strict_settings(Path::new("managed.json"), "{\n  \"x\": }", &schema)
            .expect_err("syntax error");
        assert_eq!(error.kind, StrictDiagnosticKind::ParseError);
        assert_eq!(error.line, 2);
        assert!(error.column > 1);
    }

    #[test]
    fn escaped_keys_are_located_through_the_escaped_path() {
        let text = r#"{"a.b": 1}"#;
        let schema = FieldSchema {
            kind: FieldKind::Object,
            fields: BTreeMap::new(),
            items: None,
            allow_unknown: false,
        };
        let value: Value = serde_json::from_str(text).expect("json");
        let diagnostics =
            validate_strict_settings(Path::new("managed.json"), text, &value, &schema);
        assert_eq!(diagnostics.len(), 1);
        assert_eq!(diagnostics[0].key_path, "a\\.b");
        assert_eq!(diagnostics[0].column, 2);
    }

    #[test]
    fn nested_policy_keys_and_extra_sections_are_not_unknown_fields() {
        let value = json!({
            "permissions": {
                "allowedMcpServers": ["docs"],
                "deniedMcpServers": [{"server_name": "evil"}],
                "allowManagedPermissionRulesOnly": true,
                "allow_managed_hooks_only": "disable",
                "disableBypassPermissionsMode": "true"
            },
            "allowedMcpServers": ["docs"],
            "disableBypassPermissionsMode": "disable",
            "continuity": {"enabled": true},
            "cron": {"enabled": true, "intervalSecs": 30}
        });
        let text = serde_json::to_string(&value).expect("json");
        let schema = strict_settings_schema();
        let (_, diagnostics) =
            parse_strict_settings(Path::new("managed.json"), &text, &schema).expect("valid");
        assert!(diagnostics.is_empty(), "{diagnostics:?}");
    }

    #[test]
    fn valid_policy_document_has_no_diagnostics() {
        let value = json!({
            "permissions": {
                "mode": "default",
                "rules": [{"tool": "bash", "decision": "deny"}],
                "disableBypassPermissionsMode": "disable"
            },
            "allowManagedPermissionRulesOnly": true,
            "allowedMcpServers": [{"serverName": "docs"}],
            "deniedMcpServers": [{"serverUrl": "https://*.example.com/*"}]
        });
        let text = serde_json::to_string(&value).expect("json");
        let schema = strict_settings_schema();
        let (_, diagnostics) =
            parse_strict_settings(Path::new("managed.json"), &text, &schema).expect("valid");
        assert!(diagnostics.is_empty());
    }
}
