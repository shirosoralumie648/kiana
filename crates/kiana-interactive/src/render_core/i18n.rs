//! i18n：数据驱动的 UI 语言包。
//!
//! # 对外 API
//!
//! * [`Translator::t`]：`t(key, params)`，支持 `{{name}}` 插值；
//! * [`Translator::ta`]：取多行字符串数组；
//! * 语言包只从 **数据文件**（JSON，或本模块支持的 TOML 子集）读取，**不执行用户 JS**；
//! * 查找顺序：用户目录覆盖内置语言包，内置语言包字段缺失时回退英文内置包，最后回退
//!   key 本身（永不 panic）；
//! * [`check_parity`] / [`locale_ci`]：strict locale 必须与英文 key 集合一致，且
//!   [`MUST_TRANSLATE_KEYS`] 里的 key 必须真的翻译（不能与英文相同）；
//! * [`LanguageSettings`] 把 **UI locale** 与 **模型 output language** 分开：切换 UI
//!   语言不会改变模型回答语言，反之亦然。
//!
//! # 数据格式
//!
//! JSON：
//!
//! ```json
//! { "locale": "zh", "strict": true,
//!   "strings": { "approval.allow_once": "只允许一次", "hint": ["line1", "line2"] } }
//! ```
//!
//! TOML（子集：`key = "text"`、`key = ["a", "b"]`、`[strings]` 段、`#` 注释）：
//!
//! ```toml
//! locale = "zh"
//! [strings]
//! "approval.allow_once" = "只允许一次"
//! hint = ["line1", "line2"]
//! ```
//!
//! 只解析字符串与字符串数组；其它值类型被忽略，不执行任何代码。
//!
//! # 与参考实现的关系
//!
//! 行为参考 qwen-code `packages/cli/src/i18n/index.ts` / `languages.ts` /
//! `mustTranslateKeys.ts`（用户目录 > 内置 > 英文回退、strict parity、MUST_TRANSLATE）。
//! Kiana 把 JS 模块换成纯数据文件，并显式拆分 UI locale 与 output language。

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// 内置英文语言包（JSON）。
pub const BUILTIN_EN_JSON: &str = include_str!("../../assets/locales/en.json");
/// 内置简体中文语言包（JSON）。
pub const BUILTIN_ZH_JSON: &str = include_str!("../../assets/locales/zh.json");

/// strict locale 必须真正翻译的 key。
///
/// 这里只放高可见度的 UI 字符串；产品名、枚举值、文件名不列入，避免为了通过 CI 而
/// 制造无意义的“翻译”。
pub const MUST_TRANSLATE_KEYS: &[&str] = &[
    "approval.title",
    "approval.allow_once",
    "approval.allow_always",
    "approval.reject",
    "approval.reason",
    "approval.dangerous",
    "approval.risk_loading",
    "modal.approval_position",
    "modal.multi_select_hint",
    "modal.submit_hint",
    "pending.sending",
    "pending.timeout",
];

/// 支持的语言。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LanguageDefinition {
    /// 内部代码（如 `zh`）。
    pub code: &'static str,
    /// 标准 id（如 `zh-CN`）。
    pub id: &'static str,
    /// 英文名。
    pub full_name: &'static str,
    /// 本地名。
    pub native_name: &'static str,
    /// 是否要求与英文 key 完全对齐（strict）。
    pub strict_parity: bool,
}

/// 内置支持的语言表。
pub const SUPPORTED_LANGUAGES: &[LanguageDefinition] = &[
    LanguageDefinition {
        code: "en",
        id: "en-US",
        full_name: "English",
        native_name: "English",
        strict_parity: false,
    },
    LanguageDefinition {
        code: "zh",
        id: "zh-CN",
        full_name: "Chinese",
        native_name: "中文",
        strict_parity: true,
    },
    LanguageDefinition {
        code: "zh-TW",
        id: "zh-TW",
        full_name: "Traditional Chinese",
        native_name: "繁體中文",
        strict_parity: true,
    },
    LanguageDefinition {
        code: "ja",
        id: "ja-JP",
        full_name: "Japanese",
        native_name: "日本語",
        strict_parity: false,
    },
    LanguageDefinition {
        code: "de",
        id: "de-DE",
        full_name: "German",
        native_name: "Deutsch",
        strict_parity: false,
    },
    LanguageDefinition {
        code: "fr",
        id: "fr-FR",
        full_name: "French",
        native_name: "Français",
        strict_parity: false,
    },
    LanguageDefinition {
        code: "pt",
        id: "pt-BR",
        full_name: "Portuguese",
        native_name: "Português",
        strict_parity: false,
    },
    LanguageDefinition {
        code: "ru",
        id: "ru-RU",
        full_name: "Russian",
        native_name: "Русский",
        strict_parity: false,
    },
    LanguageDefinition {
        code: "ca",
        id: "ca-ES",
        full_name: "Catalan",
        native_name: "Català",
        strict_parity: false,
    },
];

/// 语言包值。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TranslationValue {
    /// 单行文本。
    Text(String),
    /// 多行字符串数组。
    Lines(Vec<String>),
}

impl TranslationValue {
    /// 作为单行文本（数组用换行连接）。
    pub fn as_text(&self) -> String {
        match self {
            Self::Text(text) => text.clone(),
            Self::Lines(lines) => lines.join("\n"),
        }
    }
}

/// 一个语言包。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LocalePack {
    /// locale 代码。
    pub locale: String,
    /// 是否 strict（CI 校验 key parity）。
    pub strict: bool,
    /// key → 值。
    pub strings: BTreeMap<String, TranslationValue>,
}

impl LocalePack {
    /// 空语言包。
    pub fn empty(locale: &str) -> Self {
        Self {
            locale: locale.to_string(),
            strict: false,
            strings: BTreeMap::new(),
        }
    }

    /// 取一个值。
    pub fn get(&self, key: &str) -> Option<&TranslationValue> {
        self.strings.get(key)
    }

    /// 合并另一语言包（`other` 覆盖同名 key）。
    pub fn overlay(&mut self, other: &LocalePack) {
        for (key, value) in &other.strings {
            self.strings.insert(key.clone(), value.clone());
        }
    }
}

/// 语言包解析/加载错误。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LocaleError {
    /// 文件路径（内置包为空）。
    pub path: String,
    /// 原因。
    pub message: String,
}

impl std::fmt::Display for LocaleError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.path.is_empty() {
            write!(f, "{}", self.message)
        } else {
            write!(f, "{}: {}", self.path, self.message)
        }
    }
}

impl std::error::Error for LocaleError {}

/// 语言包来源。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LocaleSource {
    /// 用户目录（最高优先级）。
    User,
    /// 内置目录 / 内置资源。
    Builtin,
    /// 英文回退。
    Fallback,
}

/// 加载结果。
#[derive(Clone, Debug)]
pub struct LoadedLocale {
    /// 合并后的语言包。
    pub pack: LocalePack,
    /// 实际来源（最高优先级命中的层）。
    pub source: LocaleSource,
    /// 被跳过的层与原因（损坏文件不覆盖内置）。
    pub diagnostics: Vec<String>,
}

/// 语言包加载器。
#[derive(Clone, Debug)]
pub struct LocaleLoader {
    /// 用户语言包目录（如 `~/.kiana/locales`）。
    pub user_dir: PathBuf,
    /// 内置语言包目录（发布时打包的 `locales/`）。
    pub builtin_dir: PathBuf,
}

impl LocaleLoader {
    /// 构造加载器。
    pub fn new(user_dir: impl Into<PathBuf>, builtin_dir: impl Into<PathBuf>) -> Self {
        Self {
            user_dir: user_dir.into(),
            builtin_dir: builtin_dir.into(),
        }
    }

    /// 加载 `locale` 对应的翻译器；找不到时回退英文，永不失败。
    pub fn load_translator(&self, locale: &str) -> Translator {
        let loaded = self.load(locale);
        let fallback = builtin_pack("en").unwrap_or_else(|| LocalePack::empty("en"));
        Translator::new(loaded.pack, fallback)
    }

    /// 加载并合并语言包。用户目录 > 内置目录 > 内置资源 > 英文回退。
    pub fn load(&self, locale: &str) -> LoadedLocale {
        let resolved = resolve_supported_locale(locale).unwrap_or(locale);
        let mut diagnostics = Vec::new();
        let mut pack = LocalePack::empty(resolved);
        let mut source = LocaleSource::Fallback;

        // 1. 内置资源（保证 en 永远可用）。
        if let Some(builtin) = builtin_pack(resolved) {
            pack.overlay(&builtin);
            source = LocaleSource::Builtin;
        } else if let Some(builtin) = builtin_pack("en") {
            pack.overlay(&builtin);
            source = LocaleSource::Fallback;
        }

        // 2. 内置目录（发布包里的 locales/）。
        if let Some(builtin_file) = find_locale_file(&self.builtin_dir, resolved) {
            match load_pack_file(&builtin_file, resolved) {
                Ok(file_pack) => {
                    pack.overlay(&file_pack);
                    source = LocaleSource::Builtin;
                }
                Err(error) => diagnostics.push(error.to_string()),
            }
        }

        // 3. 用户目录（覆盖内置）。
        if let Some(user_file) = find_locale_file(&self.user_dir, resolved) {
            match load_pack_file(&user_file, resolved) {
                Ok(user_pack) => {
                    pack.overlay(&user_pack);
                    source = LocaleSource::User;
                }
                Err(error) => diagnostics.push(error.to_string()),
            }
        }

        // 4. 英文回退层（字段级回退在 Translator 里做，这里只给 strict 包装上 en 缺失键）。
        if resolved != "en"
            && let Some(english) = builtin_pack("en")
        {
            for (key, value) in &english.strings {
                pack.strings
                    .entry(key.clone())
                    .or_insert_with(|| value.clone());
            }
        }
        pack.locale = resolved.to_string();
        if let Some(definition) = SUPPORTED_LANGUAGES
            .iter()
            .find(|definition| definition.code == resolved)
        {
            pack.strict = definition.strict_parity;
        }
        LoadedLocale {
            pack,
            source,
            diagnostics,
        }
    }
}

/// 内置资源里的语言包。
pub fn builtin_pack(locale: &str) -> Option<LocalePack> {
    match locale {
        "en" => parse_locale_json(BUILTIN_EN_JSON).ok(),
        "zh" => parse_locale_json(BUILTIN_ZH_JSON).ok(),
        _ => None,
    }
}

fn find_locale_file(dir: &Path, locale: &str) -> Option<PathBuf> {
    // 空目录表示“没有这一层”（生产代码把编译进二进制的内置资源当作内置层，
    // `builtin_dir` 留空）。`Path::new("").join("zh.json")` 会得到相对路径
    // `zh.json`，`is_file()` 于是按**进程 cwd** 探测——从任意仓库里启动都会加载
    // 仓库自带的同名语言包，覆盖编译进来的文案。必须显式跳过。
    if dir.as_os_str().is_empty() {
        return None;
    }
    for extension in ["json", "toml"] {
        let candidate = dir.join(format!("{locale}.{extension}"));
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

/// 按扩展名解析语言包文件。
pub fn load_pack_file(path: &Path, locale: &str) -> Result<LocalePack, LocaleError> {
    let content = std::fs::read_to_string(path).map_err(|error| LocaleError {
        path: path.display().to_string(),
        message: error.to_string(),
    })?;
    let extension = path
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or("");
    let mut pack = match extension {
        "toml" => parse_locale_toml(&content)?,
        _ => parse_locale_json(&content)?,
    };
    if pack.locale.is_empty() {
        pack.locale = locale.to_string();
    }
    Ok(pack)
}

/// 解析 JSON 语言包。
pub fn parse_locale_json(content: &str) -> Result<LocalePack, LocaleError> {
    let value: serde_json::Value = serde_json::from_str(content).map_err(|error| LocaleError {
        path: String::new(),
        message: format!("invalid JSON: {error}"),
    })?;
    let object = value.as_object().ok_or_else(|| LocaleError {
        path: String::new(),
        message: "locale pack must be a JSON object".to_string(),
    })?;
    let locale = object
        .get("locale")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("")
        .to_string();
    let strict = object
        .get("strict")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    let strings_value = object.get("strings");
    let mut strings = BTreeMap::new();
    if let Some(value) = strings_value {
        collect_strings(value, &mut strings)?;
    } else {
        // 兼容扁平格式：除 `locale` / `strict` 外的顶层键都是翻译。
        let mut flat = serde_json::Map::new();
        for (key, value) in object {
            if key != "locale" && key != "strict" {
                flat.insert(key.clone(), value.clone());
            }
        }
        collect_strings(&serde_json::Value::Object(flat), &mut strings)?;
    }
    Ok(LocalePack {
        locale,
        strict,
        strings,
    })
}

fn collect_strings(
    value: &serde_json::Value,
    strings: &mut BTreeMap<String, TranslationValue>,
) -> Result<(), LocaleError> {
    let object = value.as_object().ok_or_else(|| LocaleError {
        path: String::new(),
        message: "`strings` must be an object".to_string(),
    })?;
    for (key, value) in object {
        match value {
            serde_json::Value::String(text) => {
                strings.insert(key.clone(), TranslationValue::Text(text.clone()));
            }
            serde_json::Value::Array(lines) => {
                let mut collected = Vec::new();
                for line in lines {
                    let Some(line) = line.as_str() else {
                        return Err(LocaleError {
                            path: String::new(),
                            message: format!("array value for `{key}` must contain only strings"),
                        });
                    };
                    collected.push(line.to_string());
                }
                strings.insert(key.clone(), TranslationValue::Lines(collected));
            }
            _ => {
                // 非字符串值忽略（数据文件是声明式的，不做类型强制）。
            }
        }
    }
    Ok(())
}

/// 解析 TOML 子集语言包。
pub fn parse_locale_toml(content: &str) -> Result<LocalePack, LocaleError> {
    let mut pack = LocalePack::empty("");
    let mut section = String::new();
    let mut lines = content.lines().peekable();
    while let Some(raw_line) = lines.next() {
        let line = strip_toml_comment(raw_line).trim().to_string();
        if line.is_empty() {
            continue;
        }
        if line.starts_with('[') && line.ends_with(']') {
            section = line[1..line.len() - 1].trim().to_string();
            continue;
        }
        let Some(equal) = find_unquoted(&line, '=') else {
            continue;
        };
        let key = unquote_key(line[..equal].trim());
        let value_text = line[equal + 1..].trim();
        let mut value_text = value_text.to_string();
        if value_text.starts_with('[') && !value_text.ends_with(']') {
            for next in lines.by_ref() {
                value_text.push('\n');
                value_text.push_str(strip_toml_comment(next).trim());
                if value_text.trim_end().ends_with(']') {
                    break;
                }
            }
        }
        if section.is_empty() {
            match key.as_str() {
                "locale" => {
                    if let Some(text) = parse_toml_string(&value_text) {
                        pack.locale = text;
                    }
                }
                "strict" => {
                    pack.strict = value_text.trim() == "true";
                }
                _ => {}
            }
            continue;
        }
        if section != "strings" {
            continue;
        }
        match parse_toml_value(&value_text) {
            Some(TranslationValue::Text(text)) => {
                pack.strings.insert(key, TranslationValue::Text(text));
            }
            Some(TranslationValue::Lines(values)) => {
                pack.strings.insert(key, TranslationValue::Lines(values));
            }
            None => {}
        }
    }
    if pack.strings.is_empty() {
        return Err(LocaleError {
            path: String::new(),
            message: "TOML locale pack has no `[strings]` entries".to_string(),
        });
    }
    Ok(pack)
}

fn strip_toml_comment(line: &str) -> &str {
    let mut quote = None;
    let bytes = line.as_bytes();
    let mut index = 0usize;
    while index < bytes.len() {
        let byte = bytes[index];
        match quote {
            Some(active) => {
                if byte == active {
                    quote = None;
                }
            }
            None => match byte {
                b'"' | b'\'' => quote = Some(byte),
                b'#' => return &line[..index],
                _ => {}
            },
        }
        index += 1;
    }
    line
}

fn find_unquoted(line: &str, needle: char) -> Option<usize> {
    let mut quote = None;
    for (index, character) in line.char_indices() {
        match quote {
            Some(active) => {
                if character == active {
                    quote = None;
                }
            }
            None => {
                if character == '"' || character == '\'' {
                    quote = Some(character);
                } else if character == needle {
                    return Some(index);
                }
            }
        }
    }
    None
}

fn unquote_key(key: &str) -> String {
    let key = key.trim();
    if (key.starts_with('"') && key.ends_with('"'))
        || (key.starts_with('\'') && key.ends_with('\''))
    {
        key[1..key.len() - 1].to_string()
    } else {
        key.to_string()
    }
}

fn parse_toml_string(text: &str) -> Option<String> {
    match parse_toml_value(text) {
        Some(TranslationValue::Text(text)) => Some(text),
        _ => None,
    }
}

fn parse_toml_value(text: &str) -> Option<TranslationValue> {
    let text = text.trim();
    if let Some(inner) = text
        .strip_prefix('[')
        .and_then(|value| value.strip_suffix(']'))
    {
        let mut values = Vec::new();
        for part in split_toml_array(inner) {
            let value = parse_toml_value(part.trim())?;
            match value {
                TranslationValue::Text(text) => values.push(text),
                TranslationValue::Lines(lines) => values.extend(lines),
            }
        }
        return Some(TranslationValue::Lines(values));
    }
    parse_quoted_string(text).map(TranslationValue::Text)
}

fn split_toml_array(inner: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut start = 0usize;
    let mut quote = None;
    for (index, character) in inner.char_indices() {
        match quote {
            Some(active) => {
                if character == active {
                    quote = None;
                }
            }
            None => {
                if character == '"' || character == '\'' {
                    quote = Some(character);
                } else if character == ',' {
                    parts.push(&inner[start..index]);
                    start = index + 1;
                }
            }
        }
    }
    if start < inner.len() {
        parts.push(&inner[start..]);
    }
    parts
}

fn parse_quoted_string(text: &str) -> Option<String> {
    let text = text.trim();
    let quote = text.chars().next()?;
    if (quote != '"' && quote != '\'') || !text.ends_with(quote) || text.len() < 2 {
        return None;
    }
    let inner = &text[1..text.len() - 1];
    if quote == '\'' {
        return Some(inner.to_string());
    }
    let mut result = String::new();
    let mut characters = inner.chars();
    while let Some(character) = characters.next() {
        if character != '\\' {
            result.push(character);
            continue;
        }
        match characters.next()? {
            'n' => result.push('\n'),
            't' => result.push('\t'),
            'r' => result.push('\r'),
            '"' => result.push('"'),
            '\\' => result.push('\\'),
            other => result.push(other),
        }
    }
    Some(result)
}

/// 带 fallback 的翻译器。
#[derive(Clone, Debug)]
pub struct Translator {
    current: LocalePack,
    fallback: LocalePack,
}

impl Translator {
    /// 构造翻译器。
    pub fn new(current: LocalePack, fallback: LocalePack) -> Self {
        Self { current, fallback }
    }

    /// 当前 locale。
    pub fn locale(&self) -> &str {
        &self.current.locale
    }

    /// 文本翻译；缺失时回退英文，再回退 key。
    pub fn t(&self, key: &str, params: &[(&str, &str)]) -> String {
        let template = match self.current.get(key) {
            Some(TranslationValue::Text(text)) => text.clone(),
            Some(TranslationValue::Lines(_)) => self
                .fallback
                .get(key)
                .map(TranslationValue::as_text)
                .unwrap_or_else(|| key.to_string()),
            None => self
                .fallback
                .get(key)
                .map(TranslationValue::as_text)
                .unwrap_or_else(|| key.to_string()),
        };
        interpolate(&template, params)
    }

    /// 多行数组；当前包没有时回退英文，仍没有则返回空数组。
    pub fn ta(&self, key: &str) -> Vec<String> {
        match self.current.get(key) {
            Some(TranslationValue::Lines(lines)) => lines.clone(),
            Some(TranslationValue::Text(text)) => vec![text.clone()],
            None => match self.fallback.get(key) {
                Some(TranslationValue::Lines(lines)) => lines.clone(),
                Some(TranslationValue::Text(text)) => vec![text.clone()],
                None => Vec::new(),
            },
        }
    }
}

/// `{{name}}` 插值；未知占位符保持原样。
pub fn interpolate(template: &str, params: &[(&str, &str)]) -> String {
    let mut result = template.to_string();
    for (key, value) in params {
        result = result.replace(&format!("{{{{{key}}}}}"), value);
    }
    result
}

/// key parity 报告。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ParityReport {
    /// 参考包有、当前包缺的 key（排序）。
    pub missing: Vec<String>,
    /// 当前包多出的 key（排序）。
    pub extra: Vec<String>,
    /// `MUST_TRANSLATE_KEYS` 中与英文相同（或缺失）的 key。
    pub untranslated_must: Vec<String>,
}

impl ParityReport {
    /// 是否完全通过。
    pub fn is_ok(&self) -> bool {
        self.missing.is_empty() && self.extra.is_empty() && self.untranslated_must.is_empty()
    }
}

/// 计算 strict 语言包相对英文参考包的差异。
pub fn check_parity(strict: &LocalePack, reference: &LocalePack) -> ParityReport {
    let mut report = ParityReport::default();
    for key in reference.strings.keys() {
        if !strict.strings.contains_key(key) {
            report.missing.push(key.clone());
        }
    }
    for key in strict.strings.keys() {
        if !reference.strings.contains_key(key) {
            report.extra.push(key.clone());
        }
    }
    for key in MUST_TRANSLATE_KEYS {
        match (strict.get(key), reference.get(key)) {
            (Some(value), Some(english)) => {
                if value.as_text() == english.as_text() {
                    report.untranslated_must.push((*key).to_string());
                }
            }
            _ => report.untranslated_must.push((*key).to_string()),
        }
    }
    report
}

/// CI 错误（列出所有失败 key）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LocaleCiError {
    /// 失败的 locale。
    pub locale: String,
    /// 详情。
    pub report: ParityReport,
}

impl std::fmt::Display for LocaleCiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "locale `{}` failed parity", self.locale)?;
        if !self.report.missing.is_empty() {
            write!(f, "; missing keys: {}", self.report.missing.join(", "))?;
        }
        if !self.report.extra.is_empty() {
            write!(f, "; extra keys: {}", self.report.extra.join(", "))?;
        }
        if !self.report.untranslated_must.is_empty() {
            write!(
                f,
                "; untranslated MUST_TRANSLATE keys: {}",
                self.report.untranslated_must.join(", ")
            )?;
        }
        Ok(())
    }
}

impl std::error::Error for LocaleCiError {}

/// CI 入口：只检查 `strict` 语言包；失败返回包含全部 key 的错误。
pub fn locale_ci(packs: &[LocalePack], reference: &LocalePack) -> Result<(), LocaleCiError> {
    for pack in packs.iter().filter(|pack| pack.strict) {
        let report = check_parity(pack, reference);
        if !report.is_ok() {
            return Err(LocaleCiError {
                locale: pack.locale.clone(),
                report,
            });
        }
    }
    Ok(())
}

/// UI locale 与模型输出语言。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LanguageSettings {
    /// UI 语言（如 `zh`）。
    pub ui_locale: String,
    /// 模型回答语言（如 `zh`）；`None` 表示跟随模型默认。
    pub output_language: Option<String>,
}

impl LanguageSettings {
    /// 构造。
    pub fn new(ui_locale: impl Into<String>, output_language: Option<String>) -> Self {
        Self {
            ui_locale: ui_locale.into(),
            output_language,
        }
    }

    /// 模型 system prompt 的语言规则；`None` 表示不需要追加。
    pub fn output_rule(&self) -> Option<String> {
        self.output_language
            .as_deref()
            .and_then(output_language_system_rule)
    }
}

/// 生成「必须用某语言回答」的 system prompt 片段。
///
/// 实现位于 `kiana-resources`（会话层与 TUI 共用的唯一来源），这里再导出以保持
/// `render_core::i18n::output_language_system_rule` 的公开路径不变。
pub use kiana_resources::system_prompt::output_language_system_rule;

/// 把任意输入规范成受支持的 locale 代码；不认识时返回 `None`。
pub fn resolve_supported_locale(input: &str) -> Option<&'static str> {
    let candidate = input.trim().replace('_', "-").to_ascii_lowercase();
    let candidate = candidate
        .split('.')
        .next()
        .unwrap_or(&candidate)
        .to_string();
    let candidate = candidate
        .split('@')
        .next()
        .unwrap_or(&candidate)
        .to_string();
    // 更长的 `zh-TW` / `zh-HK` 必须先于 `zh` 前缀匹配。
    if candidate.starts_with("zh-tw") || candidate.starts_with("zh-hk") {
        return Some("zh-TW");
    }
    for definition in SUPPORTED_LANGUAGES {
        let code = definition.code.to_ascii_lowercase();
        let id = definition.id.to_ascii_lowercase();
        if candidate == code
            || candidate == id
            || candidate.starts_with(&format!("{code}-"))
            || candidate.starts_with(&format!("{id}-"))
        {
            return Some(definition.code);
        }
    }
    None
}

/// 从环境变量（`KIANA_LANG` > `LC_ALL` > `LC_MESSAGES` > `LANG`）检测 UI locale。
pub fn detect_locale_from_env() -> String {
    let candidate = ["KIANA_LANG", "LC_ALL", "LC_MESSAGES", "LANG"]
        .iter()
        .filter_map(|name| std::env::var(name).ok())
        .find(|value| !value.trim().is_empty());
    candidate
        .as_deref()
        .and_then(resolve_supported_locale)
        .unwrap_or("en")
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_builtin_dir_does_not_probe_the_working_directory() {
        // 空路径 join 出来的是相对路径，`is_file()` 会按 cwd 探测；必须显式拒绝。
        assert_eq!(find_locale_file(Path::new(""), "zh"), None);
        assert_eq!(find_locale_file(Path::new(""), "en"), None);
    }

    #[test]
    fn empty_builtin_dir_falls_back_to_the_compiled_pack() {
        let loader = LocaleLoader::new(PathBuf::from("/nonexistent-kiana-locales"), PathBuf::new());
        let loaded = loader.load("zh");
        assert_eq!(loaded.source, LocaleSource::Builtin);
        assert_eq!(loaded.pack.locale, "zh");
        assert!(loaded.diagnostics.is_empty());
    }
}
