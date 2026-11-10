//! core/export-html/index.ts：占位符单次替换、base64 会话数据、主题变量与颜色推导。
//!
//! 导出产物是单文件自包含 HTML：`template.html` 的固定 DOM，内联 `template.css`
//! （4 处替换）与三个 vendored `<script>`，内联 `template.js`，以及
//! `<script id="session-data" type="application/json">{base64}</script>`。
//!
//! 导出产物的逐字节一致性（golden 夹具锁定）由三件事保证：
//!
//! 1. 资产逐字节内嵌（见 [`crate::assets`]）；
//! 2. 替换是**每个占位符恰好一次**、顺序固定，且按 JS
//!    `String.prototype.replace(string, string)` 的 GetSubstitution 语义处理 replacement
//!    里的 `$` 序列（`js_replace_first`；`vendor/highlight.min.js` 里的 `$&`/`$$`
//!    会让 TS 输出比原样插入长 12 字节）；
//! 3. 会话数据按 JS `JSON.stringify` 语义序列化（`session_data_json_string`，数字格式
//!    与键序与 JS 相同）后再 base64。
//!
//! # 两个导出入口与执行流程
//!
//! [`export_session_to_html`]（实时会话，可带 `AgentState` 子集与工具渲染器）与
//! [`export_from_file`]（只给会话文件路径）走同一条管线：
//!
//! 1. 从 `SessionManager` 取**原始 JSON** 的 header 与 entries（未知字段、键序保留，
//!    原因见 [`SessionData`]）；
//! 2. 只有 [`export_session_to_html`] 在调用方提供 [`ExportOptions::tool_renderer`]
//!    时才会对原始条目预渲染出 `renderedTools`（空映射按 `undefined` 省略键）；
//! 3. [`generate_html`] 读取模板资产、生成主题变量与颜色、把会话数据序列化并 base64，
//!    再按上面的固定顺序做单次替换；
//! 4. 输出路径缺省为 `kiana-session-<输入文件名>.html`，文件以**截断方式整份重写**。
//!
//! 调用方：`kiana-session` 的 `AgentSession::export_to_html`（TS `exportToHtml`，
//! 提供系统提示、工具清单与扩展工具渲染器）和 `kiana-coding-agent` 的 `--export`
//! 分支（只给路径，见该 crate 的 `src/app.rs`）。
//!
//! # 失败模式
//!
//! [`export_session_to_html`]：
//! - 会话在内存中 / 会话文件不存在：[`ExportError::CannotExportInMemorySession`] /
//!   [`ExportError::NothingToExport`]；
//! - 主题名无法解析：[`ExportError::Theme`]（消息透传 `kiana-theme`）；
//! - 模板资产：无论是否设置 `KIANA_PACKAGE_DIR` 都会做包目录探测（见下文
//!   [`generate_html`]）；设置了 `KIANA_PACKAGE_DIR` 后从磁盘读取资产，缺失时返回
//!   [`ExportError::Io`]，不会回退内嵌副本（`tests/export_html_package_override.rs`）；
//! - 预渲染遇到畸形条目（如 `message` 为 `null`）：[`ExportError::Session`]；
//! - 输出写入失败：[`ExportError::Io`]，消息按 Node `writeFileSync` 文案重建。
//!
//! [`export_from_file`]：
//! - 输入路径不存在：[`ExportError::FileNotFound`]（该入口先 `Path::exists` 检查再
//!   交给 `SessionManager::open`）；
//! - `SessionManager::open` 的读取/迁移失败：[`ExportError::Session`]。

use std::path::Path;

use base64::Engine as _;
use indexmap::IndexMap;
use kiana_ai::types::{JsonValue, UserContentBlock};
use kiana_core::config::APP_NAME;
use kiana_core::paths::{PathInputOptions, normalize_path, resolve_path};
use kiana_json::js_string::{is_truthy as js_truthy, js_quote, js_string};
use kiana_session_core::session_manager::{SessionManager, SessionManagerError};
use kiana_theme::theme::{get_resolved_theme_colors, get_theme_export_colors};
use serde::Serialize;

use crate::ansi_to_html::js_number_to_string;
use crate::assets::{
    HIGHLIGHT_MIN_JS, MARKED_CJK_FRIENDLY_MIN_JS, MARKED_MIN_JS, TEMPLATE_CSS, TEMPLATE_HTML,
    TEMPLATE_JS,
};

// `index` 再导出工具渲染器面（§5 的公开面）。
pub use crate::tool_renderer::{RenderedToolResult, ToolHtmlRenderer};

/// 模板直接渲染的工具集合（不走 TUI→ANSI→HTML 预渲染管线）。
///
/// TS: `index.ts#TEMPLATE_RENDERED_TOOLS`。`template.js` 只为这五个工具提供内联
/// HTML 分支；`grep`/`find`/`powershell` 与所有扩展工具都走预渲染（没有预渲染时
/// 退化为工具名 + `JSON.stringify(args, null, 2)` + 文本结果）。
const TEMPLATE_RENDERED_TOOLS: [&str; 5] = ["bash", "read", "write", "edit", "ls"];

/// `Object.prototype` 上的自有属性名（含 `__proto__` 访问器）。
///
/// TS 的 `renderedTools` 是普通对象字面量 `{}`，`renderedTools[id]` 的读取会走原型链：
/// `hasOwnProperty`/`constructor`/`toString`/`valueOf` 等键即使从未被赋值也返回真值，
/// 于是 `existing || !TEMPLATE_RENDERED_TOOLS.has(toolName)` 成立（模板内联渲染的工具
/// 也会调用 `renderResult`）。赋值时只有 `__proto__` 特殊：它是访问器，赋值改写原型而
/// **不会**创建自有可枚举键，`JSON.stringify` 因而看不到它。
const OBJECT_PROTOTYPE_PROPERTIES: [&str; 12] = [
    "__proto__",
    "constructor",
    "__defineGetter__",
    "__defineSetter__",
    "hasOwnProperty",
    "__lookupGetter__",
    "__lookupSetter__",
    "isPrototypeOf",
    "propertyIsEnumerable",
    "toLocaleString",
    "toString",
    "valueOf",
];

/// 该键是否命中 `Object.prototype`（读取结果为真值）。
fn is_object_prototype_property(key: &str) -> bool {
    OBJECT_PROTOTYPE_PROPERTIES.contains(&key)
}

/// 预渲染后的自定义工具 HTML。
///
/// TS: `index.ts#RenderedToolHtml`。键顺序即序列化顺序：`callHtml`、
/// `resultHtmlCollapsed`、`resultHtmlExpanded`；`None` 的键被省略。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RenderedToolHtml {
    /// 工具调用的 HTML（`renderCall` 的结果）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub call_html: Option<String>,
    /// 工具结果折叠视图的 HTML。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result_html_collapsed: Option<String>,
    /// 工具结果展开视图的 HTML。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result_html_expanded: Option<String>,
    /// `{...existing}` 展开「从原型继承到的字符串」时产生的字符索引键（`"0"`、`"1"`…）。
    ///
    /// 只有伪造 `__proto__` / `callHtml` 这类 toolCallId 时才可能非空；正常导出恒为空，
    /// 序列化时不产生任何键。`JSON.stringify` 会把整数键排到最前，与 TS 的对象一致。
    #[serde(flatten)]
    #[doc(hidden)]
    pub spread_character_keys: IndexMap<String, String>,
}

/// 导出时随会话数据一起写入的工具信息。
///
/// TS: `SessionData.tools` 的 `Pick<ToolDefinition, "name" | "description" | "parameters">`。
/// 字段顺序即 JSON 键顺序：`name`、`description`、`parameters`。
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ExportToolInfo {
    /// 工具名。
    pub name: String,
    /// 工具描述。
    pub description: String,
    /// 参数 schema（原样 JSON）。
    pub parameters: JsonValue,
}

/// `kiana-agent-core::AgentState` 在导出侧的可观察子集。
///
/// TS: `exportSessionToHtml(sm, state?: AgentState, options?)` 只读取
/// `state.systemPrompt` 与 `state.tools`。`kiana-agent-core` 不是本 crate 的允许依赖，
/// 因此由调用方（`kiana-session` 的 `exportToHtml`）把 `this.state` 适配成该结构。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ExportSessionState {
    /// 系统提示（省略键表示 undefined）。
    pub system_prompt: Option<String>,
    /// 可用工具（省略键表示 undefined）。
    pub tools: Option<Vec<ExportToolInfo>>,
}

/// HTML 导出选项。
///
/// TS: `index.ts#ExportOptions`，另支持旧签名（`options` 直接是输出路径字符串）：
/// 用 [`ExportOptions::from_output_path`] 或 `From<&str>` / `From<String>` 构造。
#[derive(Default)]
pub struct ExportOptions<'a> {
    /// 输出路径；`None` 或空串时使用默认文件名。
    pub output_path: Option<String>,
    /// 主题名；`None` 表示当前主题（`currentThemeName ?? system`）。
    pub theme_name: Option<String>,
    /// 自定义工具渲染器；`None` 表示不预渲染。
    ///
    /// 只有 [`export_session_to_html`] 使用它；[`export_from_file`] 没有
    /// `AgentState`，与 TS 一样完全不写 `renderedTools`（该字段在此入口被忽略）。
    pub tool_renderer: Option<&'a mut dyn ToolHtmlRenderer>,
}

impl ExportOptions<'_> {
    /// 构造只含输出路径的选项（TS 的 `options: string` 旧签名）。
    pub fn from_output_path(output_path: impl Into<String>) -> Self {
        Self {
            output_path: Some(output_path.into()),
            ..Default::default()
        }
    }
}

impl From<&str> for ExportOptions<'_> {
    fn from(output_path: &str) -> Self {
        Self::from_output_path(output_path)
    }
}

impl From<String> for ExportOptions<'_> {
    fn from(output_path: String) -> Self {
        Self::from_output_path(output_path)
    }
}

/// 导出会话数据（写入 base64 前的结构）。
///
/// TS: `index.ts#SessionData`。字段顺序即 `JSON.stringify` 键顺序：
/// `header`、`entries`、`leafId`、`systemPrompt`、`tools`、`renderedTools`。
/// `undefined` 的键被省略（`systemPrompt`/`tools`/`renderedTools` 未提供时、以及
/// `leafId` 为 `undefined` 时）；`header` 恒写出（无 header 时为 `null`）。
///
/// `header`/`entries` 使用 [`SessionManager::get_header_json`] /
/// [`SessionManager::get_entries_json`] 的**原始 JSON**：TS 导出的是 `JSON.parse`
/// 得到的普通对象，未知字段与键序会逐字进入 base64 载荷（typed 视图会丢失它们）。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionData {
    /// 会话头（无头时为 `null`）。
    pub header: JsonValue,
    /// 会话条目（文件顺序；原始 JSON，未知字段与键序保留）。
    pub entries: Vec<JsonValue>,
    /// 当前叶子条目 id（`null` 写出；`undefined` 省略键）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub leaf_id: Option<JsonValue>,
    /// 系统提示。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub system_prompt: Option<String>,
    /// 可用工具。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<ExportToolInfo>>,
    /// 预渲染的自定义工具 HTML，按 tool call id 插入序。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rendered_tools: Option<IndexMap<String, RenderedToolHtml>>,
}

/// HTML 导出错误。
///
/// 前三个变体的 `Display` 是当前稳定契约（改动需同步测试）；主题错误直接透传
/// `kiana-theme` 的消息（例如 `Theme not found: {name}`）。
#[derive(Debug)]
pub enum ExportError {
    /// TS: `Cannot export in-memory session to HTML`。
    CannotExportInMemorySession,
    /// TS: `Nothing to export yet - start a conversation first`。
    NothingToExport,
    /// TS: `File not found: {resolvedInputPath}`。
    FileNotFound {
        /// `resolvePath(inputPath)` 之后的绝对路径。
        path: String,
    },
    /// 主题加载失败（逐字透传 `kiana-theme` 的消息）。
    Theme {
        /// 错误消息。
        message: String,
    },
    /// 会话文件打开/迁移失败：TS 直接透出 `SessionManager.open` 的异常；也承载
    /// `preRenderCustomTools` 对畸形会话数据的 `TypeError`（例如
    /// `Cannot read properties of null (reading 'role')`）。非 fs 错误逐字透传
    /// `kiana-session-core` 的消息；`io::Error` 按 Node fs 文案重建（见
    /// `node_session_read_error`）。
    Session {
        /// 错误消息。
        message: String,
    },
    /// 文件系统失败：输出文件写出，或（设置了 `KIANA_PACKAGE_DIR` 时）读取模板资产。
    ///
    /// `Display` 是 Node fs 的逐字文案：写出失败对应 `writeFileSync`（`open '<path>'`
    /// 或 `write`），资产缺失对应 `open '<path>'`，例如
    /// `ENOENT: no such file or directory, open '<path>'`。消息由 `node_fs_error`
    /// 按 errno / `ErrorKind` 重建，无法映射时保留原 [`std::io::Error`] 的文案；
    /// `source()` 返回底层的 [`std::io::Error`]。
    Io(std::io::Error),
}

impl std::fmt::Display for ExportError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ExportError::CannotExportInMemorySession => {
                formatter.write_str("Cannot export in-memory session to HTML")
            }
            ExportError::NothingToExport => {
                formatter.write_str("Nothing to export yet - start a conversation first")
            }
            ExportError::FileNotFound { path } => write!(formatter, "File not found: {path}"),
            ExportError::Theme { message } | ExportError::Session { message } => {
                formatter.write_str(message)
            }
            ExportError::Io(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for ExportError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            ExportError::Io(error) => Some(error),
            _ => None,
        }
    }
}

// ---------------------------------------------------------------------------
// 颜色工具（内部，TS `parseColor` / `getLuminance` / `adjustBrightness` /
// `deriveExportColors`）
// ---------------------------------------------------------------------------

/// RGB 通道值（TS 的 `number`；`rgb()` 里可以出现超过 255 的数字）。
#[derive(Debug, Clone, Copy, PartialEq)]
struct Rgb {
    r: f64,
    g: f64,
    b: f64,
}

/// JS `\s`（`RegExp` 空白字符集合）。
fn is_js_whitespace(character: char) -> bool {
    matches!(
        character,
        '\u{0009}'..='\u{000d}'
            | ' '
            | '\u{00a0}'
            | '\u{1680}'
            | '\u{2000}'..='\u{200a}'
            | '\u{2028}'
            | '\u{2029}'
            | '\u{202f}'
            | '\u{205f}'
            | '\u{3000}'
            | '\u{feff}'
    )
}

/// TS `parseColor`：只接受 `#RRGGBB` 与 `rgb(r,g,b)`（允许空白），其余返回 `None`。
///
/// 不支持 alpha、`oklch()` 或命名颜色。
fn parse_color(color: &str) -> Option<Rgb> {
    let bytes = color.as_bytes();
    if bytes.len() == 7 && bytes[0] == b'#' && bytes[1..].iter().all(u8::is_ascii_hexdigit) {
        let r = u8::from_str_radix(&color[1..3], 16).ok()?;
        let g = u8::from_str_radix(&color[3..5], 16).ok()?;
        let b = u8::from_str_radix(&color[5..7], 16).ok()?;
        return Some(Rgb {
            r: f64::from(r),
            g: f64::from(g),
            b: f64::from(b),
        });
    }
    parse_rgb_function(color)
}

/// `^rgb\s*\(\s*(\d+)\s*,\s*(\d+)\s*,\s*(\d+)\s*\)$` 的手写解析器。
fn parse_rgb_function(color: &str) -> Option<Rgb> {
    let mut rest = color.strip_prefix("rgb")?;
    rest = skip_js_whitespace(rest);
    rest = rest.strip_prefix('(')?;
    rest = skip_js_whitespace(rest);
    let (r, after) = take_digits(rest)?;
    rest = skip_js_whitespace(after);
    rest = rest.strip_prefix(',')?;
    rest = skip_js_whitespace(rest);
    let (g, after) = take_digits(rest)?;
    rest = skip_js_whitespace(after);
    rest = rest.strip_prefix(',')?;
    rest = skip_js_whitespace(rest);
    let (b, after) = take_digits(rest)?;
    rest = skip_js_whitespace(after);
    rest = rest.strip_prefix(')')?;
    if !rest.is_empty() {
        return None;
    }
    Some(Rgb {
        r: parse_digits_as_number(r),
        g: parse_digits_as_number(g),
        b: parse_digits_as_number(b),
    })
}

fn skip_js_whitespace(text: &str) -> &str {
    text.trim_start_matches(is_js_whitespace)
}

fn take_digits(text: &str) -> Option<(&str, &str)> {
    let length = text
        .char_indices()
        .take_while(|(_, character)| character.is_ascii_digit())
        .count();
    if length == 0 {
        return None;
    }
    // `\d` 是 ASCII，按字节切分安全。
    Some((&text[..length], &text[length..]))
}

/// `Number.parseInt(digits, 10)`：超长数字串按 f64 舍入（可能得到 `inf`）。
fn parse_digits_as_number(digits: &str) -> f64 {
    digits.parse::<f64>().unwrap_or(0.0)
}

/// TS `getLuminance`：相对亮度（0-1，越大越亮）。
fn get_luminance(r: f64, g: f64, b: f64) -> f64 {
    fn to_linear(channel: f64) -> f64 {
        let s = channel / 255.0;
        if s <= 0.03928 {
            s / 12.92
        } else {
            ((s + 0.055) / 1.055).powf(2.4)
        }
    }
    0.2126 * to_linear(r) + 0.7152 * to_linear(g) + 0.0722 * to_linear(b)
}

/// TS `adjustBrightness`：解析失败原样返回；否则每通道
/// `min(255, max(0, round(c * factor)))`，输出 `rgb(r, g, b)`（逗号后一个空格）。
fn adjust_brightness(color: &str, factor: f64) -> String {
    let Some(parsed) = parse_color(color) else {
        return color.to_string();
    };
    let adjust = |channel: f64| (channel * factor).round().clamp(0.0, 255.0);
    format!(
        "rgb({}, {}, {})",
        js_number_to_string(adjust(parsed.r)),
        js_number_to_string(adjust(parsed.g)),
        js_number_to_string(adjust(parsed.b)),
    )
}

/// `deriveExportColors` 的结果。
#[derive(Debug, Clone, PartialEq, Eq)]
struct DerivedExportColors {
    page_bg: String,
    card_bg: String,
    info_bg: String,
}

/// TS `deriveExportColors`：从一个底色（通常是 `userMessageBg`）推导导出背景色。
fn derive_export_colors(base_color: &str) -> DerivedExportColors {
    let Some(parsed) = parse_color(base_color) else {
        return DerivedExportColors {
            page_bg: "rgb(24, 24, 30)".to_string(),
            card_bg: "rgb(30, 30, 36)".to_string(),
            info_bg: "rgb(60, 55, 40)".to_string(),
        };
    };

    let luminance = get_luminance(parsed.r, parsed.g, parsed.b);
    let is_light = luminance > 0.5;

    if is_light {
        DerivedExportColors {
            page_bg: adjust_brightness(base_color, 0.96),
            card_bg: base_color.to_string(),
            info_bg: format!(
                "rgb({}, {}, {})",
                js_number_to_string((parsed.r + 10.0).min(255.0)),
                js_number_to_string((parsed.g + 5.0).min(255.0)),
                js_number_to_string((parsed.b - 20.0).max(0.0)),
            ),
        }
    } else {
        DerivedExportColors {
            page_bg: adjust_brightness(base_color, 0.7),
            card_bg: adjust_brightness(base_color, 0.85),
            info_bg: format!(
                "rgb({}, {}, {})",
                js_number_to_string((parsed.r + 20.0).min(255.0)),
                js_number_to_string((parsed.g + 15.0).min(255.0)),
                js_number_to_string(parsed.b),
            ),
        }
    }
}

/// `colors.userMessageBg || "#343541"`（空串也是假值）。
fn user_message_background(colors: &IndexMap<String, String>) -> String {
    colors
        .get("userMessageBg")
        .filter(|value| !value.is_empty())
        .cloned()
        .unwrap_or_else(|| "#343541".to_string())
}

/// TS `generateThemeVars`：56 个主题 token + 3 个导出变量，`"\n      "` 连接。
///
/// 错误：主题名无法解析时返回 [`ExportError::Theme`]，消息逐字透传 `kiana-theme`
/// （测试锁定 `Theme not found: {name}`）。
fn generate_theme_vars(theme_name: Option<&str>) -> Result<String, ExportError> {
    let colors = get_resolved_theme_colors(theme_name).map_err(|error| ExportError::Theme {
        message: error.to_string(),
    })?;

    let mut lines: Vec<String> = Vec::with_capacity(colors.len() + 3);
    for (key, value) in &colors {
        lines.push(format!("--{key}: {value};"));
    }

    // 显式主题导出色优先，否则从 userMessageBg 推导（`??` 语义：Some("") 会透传）。
    let theme_export = get_theme_export_colors(theme_name);
    let derived = derive_export_colors(&user_message_background(&colors));

    lines.push(format!(
        "--exportPageBg: {};",
        theme_export.page_bg.unwrap_or(derived.page_bg)
    ));
    lines.push(format!(
        "--exportCardBg: {};",
        theme_export.card_bg.unwrap_or(derived.card_bg)
    ));
    lines.push(format!(
        "--exportInfoBg: {};",
        theme_export.info_bg.unwrap_or(derived.info_bg)
    ));

    Ok(lines.join("\n      "))
}

/// `renderedTools` 的值：普通对象（键序 = 首次赋值顺序；整数索引键由
/// [`kiana_json::js_object::stringify`] 排到最前）。
///
/// 只重建这一层浅对象（叶值是字符串），因此不触碰条目里可能极深的 JSON；
/// `spread_character_keys` 是 `{...existing}` 从字符串展开出的索引键，与命名键
/// 一起放进同一个对象，由 stringify 统一按 JS 键序输出。
fn rendered_tools_json_value(rendered_tools: &IndexMap<String, RenderedToolHtml>) -> JsonValue {
    let mut outer = serde_json::Map::new();
    for (tool_call_id, rendered) in rendered_tools {
        let mut fields = serde_json::Map::new();
        if let Some(call_html) = &rendered.call_html {
            fields.insert("callHtml".to_string(), JsonValue::String(call_html.clone()));
        }
        if let Some(collapsed) = &rendered.result_html_collapsed {
            fields.insert(
                "resultHtmlCollapsed".to_string(),
                JsonValue::String(collapsed.clone()),
            );
        }
        if let Some(expanded) = &rendered.result_html_expanded {
            fields.insert(
                "resultHtmlExpanded".to_string(),
                JsonValue::String(expanded.clone()),
            );
        }
        for (key, character) in &rendered.spread_character_keys {
            fields.insert(key.clone(), JsonValue::String(character.clone()));
        }
        outer.insert(tool_call_id.clone(), JsonValue::Object(fields));
    }
    JsonValue::Object(outer)
}

/// `JSON.stringify(sessionData)`（`generateHtml` 在 base64 之前的序列化）。
///
/// 不能走 `serde_json::to_value(&SessionData)`：那会递归遍历每个条目，模型可控的
/// 深嵌套 JSON（约 7,000 层）就会栈溢出并 abort 整个进程，而 TS 的 `JSON.stringify`
/// 到 30 万层仍成功。这里按 TS 对象字面量的键序手工拼接，条目逐个用**迭代**的
/// [`kiana_json::js_object::stringify`] 直接写出（借用原值，不克隆整棵树）：
///
/// ```text
/// {"header":…,"entries":[…],"leafId":…,"systemPrompt":…,"tools":[…],"renderedTools":{…}}
/// ```
///
/// `undefined` 的键（`None`）省略；`header` 恒写出（无 header 时为 `null`）。
fn session_data_json_string(session_data: &SessionData) -> String {
    let mut out = String::new();
    out.push_str("{\"header\":");
    out.push_str(&kiana_json::js_object::stringify(&session_data.header));
    out.push_str(",\"entries\":[");
    for (index, entry) in session_data.entries.iter().enumerate() {
        if index > 0 {
            out.push(',');
        }
        out.push_str(&kiana_json::js_object::stringify(entry));
    }
    out.push(']');
    if let Some(leaf_id) = &session_data.leaf_id {
        out.push_str(",\"leafId\":");
        out.push_str(&kiana_json::js_object::stringify(leaf_id));
    }
    if let Some(system_prompt) = &session_data.system_prompt {
        out.push_str(",\"systemPrompt\":");
        out.push_str(&js_quote(system_prompt));
    }
    if let Some(tools) = &session_data.tools {
        out.push_str(",\"tools\":[");
        for (index, tool) in tools.iter().enumerate() {
            if index > 0 {
                out.push(',');
            }
            // 键序：`name`、`description`、`parameters`（TS 的 `map` 字面量）。
            out.push_str("{\"name\":");
            out.push_str(&js_quote(&tool.name));
            out.push_str(",\"description\":");
            out.push_str(&js_quote(&tool.description));
            out.push_str(",\"parameters\":");
            out.push_str(&kiana_json::js_object::stringify(&tool.parameters));
            out.push('}');
        }
        out.push(']');
    }
    if let Some(rendered_tools) = &session_data.rendered_tools {
        out.push_str(",\"renderedTools\":");
        out.push_str(&kiana_json::js_object::stringify(
            &rendered_tools_json_value(rendered_tools),
        ));
    }
    out.push('}');
    out
}

/// 生成完整 HTML（TS `generateHtml`，内部函数）。
///
/// 替换顺序固定且每个占位符只替换第一次出现：
///
/// - CSS：`{{THEME_VARS}}`、`{{BODY_BG}}`、`{{CONTAINER_BG}}`、`{{INFO_BG}}`；
/// - 模板：`{{CSS}}`、`{{JS}}`、`{{SESSION_DATA}}`、`{{MARKED_JS}}`、
///   `{{MARKED_CJK_FRIENDLY_JS}}`、`{{HIGHLIGHT_JS}}`。
///
/// # 资产来源
///
/// 未设置 `KIANA_PACKAGE_DIR` 时使用 crate 内嵌的逐字节副本（[`crate::assets`]），
/// 不读取磁盘上的资产文件；设置后从 `get_export_template_dir()` 读盘，资产缺失直接
/// 报错而不会回退内嵌副本（`tests/export_html_package_override.rs` 锁定该行为）。
///
/// 注意“不读资产文件”不等于零磁盘 IO：本函数在检查 `KIANA_PACKAGE_DIR` 之前就无条件
/// 调用 `get_export_template_dir()`，该调用经 `get_package_dir()` →
/// `find_node_package_dir()` 用 `Path::exists()` 逐级探测 `package.json`，
/// `src_or_dist()` 还会检查 `<pkg>/src` 是否存在；未设置环境变量时同样发生这些磁盘
/// 元数据访问。
///
/// # 错误
///
/// - 主题名无法解析：[`ExportError::Theme`]；
/// - 设置了 `KIANA_PACKAGE_DIR` 且资产读取失败：[`ExportError::Io`]。资产缺失（ENOENT）
///   的文案如 `ENOENT: no such file or directory, open '<path>'`；其它 errno 由
///   `node_fs_error` 映射为对应的 Node 文案（如 EACCES/EISDIR），无法映射的错误
///   （例如 `read_to_string` 的 UTF-8 解码失败）保留原 `std::io::Error` 文案。
pub fn generate_html(
    session_data: &SessionData,
    theme_name: Option<&str>,
) -> Result<String, ExportError> {
    // KIANA_PACKAGE_DIR is the runtime override. The default Rust binary carries
    // the same assets even when no npm directory is co-located with the test
    // or release executable; an explicit package must surface missing files.
    let template_dir = kiana_core::config::get_export_template_dir();
    let use_embedded_assets = std::env::var_os("KIANA_PACKAGE_DIR").is_none();
    let read_asset = |relative: &str| {
        let path = template_dir.join(relative);
        if use_embedded_assets {
            return match relative {
                "template.html" => Ok(TEMPLATE_HTML.to_string()),
                "template.css" => Ok(TEMPLATE_CSS.to_string()),
                "template.js" => Ok(TEMPLATE_JS.to_string()),
                "vendor/marked.min.js" => Ok(MARKED_MIN_JS.to_string()),
                "vendor/marked-cjk-friendly.min.js" => Ok(MARKED_CJK_FRIENDLY_MIN_JS.to_string()),
                "vendor/highlight.min.js" => Ok(HIGHLIGHT_MIN_JS.to_string()),
                _ => unreachable!("known export asset"),
            };
        }
        std::fs::read_to_string(&path).map_err(|error| {
            ExportError::Io(node_fs_error(
                &path.to_string_lossy(),
                error,
                FsSyscall::Open,
            ))
        })
    };
    let template_html = read_asset("template.html")?;
    let template_css = read_asset("template.css")?;
    let template_js = read_asset("template.js")?;
    let marked_js = read_asset("vendor/marked.min.js")?;
    let marked_cjk_friendly_js = read_asset("vendor/marked-cjk-friendly.min.js")?;
    let highlight_js = read_asset("vendor/highlight.min.js")?;
    let theme_vars = generate_theme_vars(theme_name)?;
    let colors = get_resolved_theme_colors(theme_name).map_err(|error| ExportError::Theme {
        message: error.to_string(),
    })?;
    let theme_export = get_theme_export_colors(theme_name);
    let derived_export_colors = derive_export_colors(&user_message_background(&colors));
    let body_bg = theme_export
        .page_bg
        .unwrap_or(derived_export_colors.page_bg);
    let container_bg = theme_export
        .card_bg
        .unwrap_or(derived_export_colors.card_bg);
    let info_bg = theme_export
        .info_bg
        .unwrap_or(derived_export_colors.info_bg);

    // JS `JSON.stringify` 语义：键序 = 插入序（整数索引键在前），数字按 JS
    // `Number::toString`（`0.0` → `0`），`None` 键省略。手工拼接以避开
    // `serde_json::to_value` 的递归遍历（深嵌套会话会让进程栈溢出）。
    let session_data_json = session_data_json_string(session_data);
    let session_data_base64 =
        base64::engine::general_purpose::STANDARD.encode(session_data_json.as_bytes());

    let css = js_replace_first(&template_css, "{{THEME_VARS}}", &theme_vars);
    let css = js_replace_first(&css, "{{BODY_BG}}", &body_bg);
    let css = js_replace_first(&css, "{{CONTAINER_BG}}", &container_bg);
    let css = js_replace_first(&css, "{{INFO_BG}}", &info_bg);

    let html = js_replace_first(&template_html, "{{CSS}}", &css);
    let html = js_replace_first(&html, "{{JS}}", &template_js);
    let html = js_replace_first(&html, "{{SESSION_DATA}}", &session_data_base64);
    let html = js_replace_first(&html, "{{MARKED_JS}}", &marked_js);
    let html = js_replace_first(&html, "{{MARKED_CJK_FRIENDLY_JS}}", &marked_cjk_friendly_js);
    Ok(js_replace_first(&html, "{{HIGHLIGHT_JS}}", &highlight_js))
}

/// `renderedTools` 普通对象的原型语义。
///
/// TS 的 `renderedTools` 是对象字面量 `{}`：读取走原型链（`Object.prototype` 的成员，
/// 以及 `__proto__` 赋值换上的自定义原型对象），赋值只有 `__proto__` 走访问器（改写
/// 原型、不建自有键）。导出只序列化自有可枚举键，因此把「自有键」与「自定义原型」分开。
#[derive(Debug, Default)]
struct RenderedToolsMap {
    /// 自有可枚举键（顺序 = 首次赋值顺序）。
    own: IndexMap<String, RenderedToolHtml>,
    /// `renderedTools["__proto__"] = {...}` 换上的原型对象。
    proto: Option<RenderedToolHtml>,
}

/// `renderedTools[key]` 读到的一个值。
///
/// 真值判断与对象展开的语义不同：继承到的字符串 `""` 是假值，展开则按字符建索引键。
#[derive(Debug, Clone)]
enum ExistingTool {
    /// 自有键 / `Object.prototype` 成员 / `__proto__` getter：按对象处理。
    Object(RenderedToolHtml),
    /// 从自定义原型继承到的字符串字段（`callHtml` 等）。
    String(String),
}

impl ExistingTool {
    /// JS 真值：对象恒真；字符串非空为真。
    fn is_truthy(&self) -> bool {
        match self {
            ExistingTool::Object(_) => true,
            ExistingTool::String(value) => !value.is_empty(),
        }
    }

    /// `{...existing}` 的自有可枚举字段。
    ///
    /// 字符串按索引展开成字符键（`{"0":"a",...}`）；JS 按 UTF-16 code unit 展开，
    /// 这里按 Unicode 标量展开（孤立代理项无法用 Rust `String` 表示，与 §8 决定 31 一致）。
    fn spread(&self) -> RenderedToolHtml {
        match self {
            ExistingTool::Object(object) => object.clone(),
            ExistingTool::String(value) => RenderedToolHtml {
                spread_character_keys: value
                    .chars()
                    .enumerate()
                    .map(|(index, character)| (index.to_string(), character.to_string()))
                    .collect(),
                ..Default::default()
            },
        }
    }
}

impl RenderedToolsMap {
    /// `renderedTools[key]`（原型链读取）。
    fn read(&self, key: &str) -> Option<ExistingTool> {
        if let Some(own) = self.own.get(key) {
            return Some(ExistingTool::Object(own.clone()));
        }
        if key == "__proto__" {
            // `Object.prototype.__proto__` 的 getter 返回当前原型（未换原型时是
            // Object.prototype）；结果总是对象（真值）。
            return Some(ExistingTool::Object(self.proto.clone().unwrap_or_default()));
        }
        if let Some(value) = self.proto.as_ref().and_then(|proto| match key {
            "callHtml" => proto.call_html.clone(),
            "resultHtmlCollapsed" => proto.result_html_collapsed.clone(),
            "resultHtmlExpanded" => proto.result_html_expanded.clone(),
            // 原型对象上只有这三类自有键：命名字段与字符串展开出的索引键。
            _ => proto.spread_character_keys.get(key).cloned(),
        }) {
            return Some(ExistingTool::String(value));
        }
        is_object_prototype_property(key).then(|| ExistingTool::Object(RenderedToolHtml::default()))
    }

    /// `renderedTools[key] = value`：`__proto__` 走访问器，其余建自有键。
    fn write(&mut self, key: &str, value: RenderedToolHtml) {
        if key == "__proto__" {
            self.proto = Some(value);
        } else {
            self.own.insert(key.to_string(), value);
        }
    }
}

/// `msg.content` → `Vec<UserContentBlock>`（TS 原样把数组传给渲染器）。
///
/// 跨 crate 的渲染器接口冻结为 `&[UserContentBlock]`，因此只保留能解析成
/// `TextContent`/`ImageContent` 的块；未知块类型与未知字段无法穿过该接口
/// （残余偏差，见模块报告）。
fn user_content_blocks(value: Option<&JsonValue>) -> Vec<UserContentBlock> {
    let Some(JsonValue::Array(blocks)) = value else {
        return Vec::new();
    };
    blocks
        .iter()
        .filter_map(|block| serde_json::from_value(block.clone()).ok())
        .collect()
}

/// V8 解引用 `null` / `undefined` 的 `TypeError.message`。
///
/// TS 的 `preRenderCustomTools` 对 `msg.role`、`block.type` 不做防护：`undefined`
/// 与 `null` 都会抛出（原始值/数组的属性访问只得到 `undefined`，不抛）。
fn js_null_deref_error(receiver: Option<&JsonValue>, property: &str) -> ExportError {
    // 只在 `entry.message` 缺失（undefined）或为 null、以及内容块为 null 时调用。
    let receiver_name = match receiver {
        Some(JsonValue::Null) => "null",
        _ => "undefined",
    };
    ExportError::Session {
        message: format!("Cannot read properties of {receiver_name} (reading '{property}')"),
    }
}

/// 预渲染自定义工具（TS `preRenderCustomTools`，内部函数）。
///
/// 按 TS 原样遍历**原始 JSON 条目**（`sm.getEntries()` 是 `JSON.parse` 的结果，
/// 不经过 typed 视图）：typed 模型无法解析的消息（未知内容块、缺省字段）仍会走到
/// 渲染器。`TEMPLATE_RENDERED_TOOLS` 里的工具不预渲染；已存在的 `renderedTools[id]`
/// 在合并结果时保留 `callHtml`。
///
/// `renderedTools` 在 TS 里是普通对象，这里复刻其原型语义（见 [`RenderedToolsMap`]）；
/// 字符串展开按 Unicode 标量而非 UTF-16 code unit（孤立代理项不可表示，§8 决定 31）。
///
/// TS 对 `msg.role` / `block.type` 的**无防护**解引用在畸形数据上是可观察行为：
/// `message` 缺失或为 `null`、assistant 内容数组里出现 `null` 块时抛出
/// `TypeError` 并中止导出（[`ExportError::Session`] 承载逐字消息）。
fn pre_render_custom_tools(
    entries: &[JsonValue],
    tool_renderer: &mut dyn ToolHtmlRenderer,
) -> Result<IndexMap<String, RenderedToolHtml>, ExportError> {
    let mut rendered_tools = RenderedToolsMap::default();

    for entry in entries {
        if entry.get("type").and_then(JsonValue::as_str) != Some("message") {
            continue;
        }
        // TS: `const msg = entry.message;` 后立刻读 `msg.role`。
        let Some(message) = entry.get("message").filter(|message| !message.is_null()) else {
            return Err(js_null_deref_error(entry.get("message"), "role"));
        };
        match message.get("role").and_then(JsonValue::as_str) {
            Some("assistant") => {
                // TS: `Array.isArray(msg.content)`。
                let Some(content) = message.get("content").and_then(JsonValue::as_array) else {
                    continue;
                };
                for block in content {
                    // TS 不跳过 `null` 块：`block.type` 直接抛 TypeError。
                    if block.is_null() {
                        return Err(js_null_deref_error(Some(block), "type"));
                    }
                    if block.get("type").and_then(JsonValue::as_str) != Some("toolCall") {
                        continue;
                    }
                    let name_value = block.get("name");
                    // `TEMPLATE_RENDERED_TOOLS.has(block.name)`：只有字符串能命中集合
                    // （非字符串值在 Set 里永远不相等）。
                    if name_value
                        .and_then(JsonValue::as_str)
                        .is_some_and(|name| TEMPLATE_RENDERED_TOOLS.contains(&name))
                    {
                        continue;
                    }
                    // TS 把 id/name 原值作为对象键与渲染器入参；Rust 的渲染器接口是
                    // `&str`，按 JS `String()` 语义转换（对象键本来就是 ToPropertyKey）。
                    let tool_call_id = js_string(block.get("id"));
                    let tool_name = js_string(name_value);
                    // 借用原始值（TS 也原样传引用）：克隆深嵌套 `arguments` 会递归。
                    let null_arguments = JsonValue::Null;
                    let arguments = block.get("arguments").unwrap_or(&null_arguments);
                    let call_html = tool_renderer.render_call(&tool_call_id, &tool_name, arguments);
                    // TS 的 `if (callHtml)`：空串为假值，不写入。
                    if let Some(call_html) = call_html.filter(|html| !html.is_empty()) {
                        rendered_tools.write(
                            &tool_call_id,
                            RenderedToolHtml {
                                call_html: Some(call_html),
                                ..Default::default()
                            },
                        );
                    }
                }
            }
            Some("toolResult") => {
                let tool_call_id_value = message.get("toolCallId");
                // TS `msg.toolCallId &&`：`undefined`/`null`/`""`/`0`/`false` 跳过。
                if !js_truthy(tool_call_id_value) {
                    continue;
                }
                let tool_call_id = js_string(tool_call_id_value);
                // TS `msg.toolName || ""`（空串与其它假值都变成空串）。
                let tool_name = match message.get("toolName") {
                    Some(value) if js_truthy(Some(value)) => js_string(Some(value)),
                    _ => String::new(),
                };
                let existing = rendered_tools.read(&tool_call_id);
                // `existing || !TEMPLATE_RENDERED_TOOLS.has(toolName)`
                let existing_truthy = existing.as_ref().is_some_and(ExistingTool::is_truthy);
                if !existing_truthy && TEMPLATE_RENDERED_TOOLS.contains(&tool_name.as_str()) {
                    continue;
                }
                let content = user_content_blocks(message.get("content"));
                // 借用 `details`（TS 原样传引用）；克隆深嵌套值会递归。
                let null_details = JsonValue::Null;
                let details = message.get("details").unwrap_or(&null_details);
                let is_error = js_truthy(message.get("isError"));
                let rendered = tool_renderer.render_result(
                    &tool_call_id,
                    &tool_name,
                    &content,
                    details,
                    is_error,
                );
                if let Some(RenderedToolResult {
                    collapsed,
                    expanded,
                }) = rendered
                {
                    // `{...existing, resultHtmlCollapsed, resultHtmlExpanded}`：展开
                    // 保留 `callHtml`，两个结果字段覆盖。
                    let mut value = existing
                        .as_ref()
                        .map(ExistingTool::spread)
                        .unwrap_or_default();
                    value.result_html_collapsed = collapsed;
                    value.result_html_expanded = Some(expanded);
                    rendered_tools.write(&tool_call_id, value);
                }
            }
            _ => {}
        }
    }

    Ok(rendered_tools.own)
}

/// 当前工作目录字符串（TS `process.cwd()`）。
///
/// 读取失败时退化为空串；`resolve_path` 对空基准目录会再按进程当前目录解析。
fn current_dir_string() -> String {
    std::env::current_dir()
        .map(|path| path.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// `node:path.basename(path, ext)` 的平台语义（Linux 只用 `/`；Windows 还认 `\`）。
fn node_basename(path: &str, ext: &str) -> String {
    // Node 的特殊情形：整个 path 恰好等于 ext 时返回空串（`basename(".jsonl",
    // ".jsonl")` 与 `basename("bc", "bc")`）。带目录前缀时不触发该规则
    // （`basename("/bc", "bc") === "bc"`）。
    if !ext.is_empty() && path == ext {
        return String::new();
    }
    let is_separator = |byte: u8| byte == b'/' || (cfg!(windows) && byte == b'\\');
    let bytes = path.as_bytes();
    let mut end = bytes.len();
    while end > 0 && is_separator(bytes[end - 1]) {
        end -= 1;
    }
    if end == 0 {
        return String::new();
    }
    let mut start = end;
    while start > 0 && !is_separator(bytes[start - 1]) {
        start -= 1;
    }
    let mut name = &path[start..end];
    // 去掉后缀只在 basename 比 ext 更长时发生：`basename("a/bc", "bc") === "bc"`。
    if !ext.is_empty() && name.len() > ext.len() && name.ends_with(ext) {
        name = &name[..name.len() - ext.len()];
    }
    name.to_string()
}

/// `String.prototype.replace(searchString, replacement)` 的单次替换语义。
///
/// TS 的 `generateHtml` 用**字符串**作为 search，JS 仍会对 replacement 里的 `$`
/// 做 GetSubstitution：`$$` → `$`、`$&` → 匹配文本、`` $` `` → 匹配前的内容、
/// `$'` → 匹配后的内容；`$1`/`$<name>` 在字符串模式下保持字面量（没有捕获组）。
///
/// 这不是可有可无的细节：`template.js` 里有 1 处 `$$`，`vendor/highlight.min.js`
/// 里有 1 处 `$$` 与 1 处 `$&`，因此 TS 导出的 HTML 比“原样插入”多 12 字节。
pub(crate) fn js_replace_first(input: &str, pattern: &str, replacement: &str) -> String {
    let Some(position) = input.find(pattern) else {
        return input.to_string();
    };
    let match_end = position + pattern.len();
    let mut result = String::with_capacity(input.len() + replacement.len());
    result.push_str(&input[..position]);

    let mut characters = replacement.chars().peekable();
    while let Some(character) = characters.next() {
        if character != '$' {
            result.push(character);
            continue;
        }
        match characters.peek() {
            Some('$') => {
                characters.next();
                result.push('$');
            }
            Some('&') => {
                characters.next();
                result.push_str(pattern);
            }
            Some('`') => {
                characters.next();
                result.push_str(&input[..position]);
            }
            Some('\'') => {
                characters.next();
                result.push_str(&input[match_end..]);
            }
            _ => result.push('$'),
        }
    }

    result.push_str(&input[match_end..]);
    result
}

/// Node fs 错误发生在哪个系统调用上（决定 `error.message` 的后缀）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FsSyscall {
    /// `open(path, ...)`：`"<code>: <description>, open '<path>'"`。
    Open,
    /// `write(fd, ...)`：`"<code>: <description>, write"`（没有路径）。
    Write,
    /// `read(fd, ...)`：`"<code>: <description>, read"`（没有路径）。
    Read,
}

/// Node `fs` 错误文案：`"<CODE>: <description>, <syscall> ['<path>']"`。
///
/// Node 的 fs 错误是 errno 名 + libuv 的 `uv_strerror` 描述 + 出错的系统调用（open 带
/// 引号路径）；TS 的 CLI 会把 `error.message` 原样打印成 `Error: ENOENT: …`。Rust 的
/// [`std::io::Error`] Display 只有 `"No such file or directory (os error 2)"`，因此在
/// fs 边界按 Node 的形态重建；用 [`std::io::Error::new`] 保留 `kind()`，无法映射时
/// 回退原错误。
fn node_fs_error(path: &str, error: std::io::Error, syscall: FsSyscall) -> std::io::Error {
    let Some((code, description)) = node_error_code_and_description(&error) else {
        return error;
    };
    let message = match syscall {
        FsSyscall::Open => format!("{code}: {description}, open '{path}'"),
        FsSyscall::Write => format!("{code}: {description}, write"),
        FsSyscall::Read => format!("{code}: {description}, read"),
    };
    std::io::Error::new(error.kind(), message)
}

/// Node `fs.writeFileSync(path, data, "utf8")`。
///
/// 分两步以复刻 Node 的错误文案：`open(2)` 失败带 `open '<path>'`，`write(2)` 失败
/// 只写 `write`（例如 `/dev/full` 的 `ENOSPC: no space left on device, write`）。
/// Node 的 `writeFileSync` 也先 open 再 write（close 错误忽略）。
///
/// 以 create + truncate 打开，整份覆盖：旧文件更长时不会残留尾部字节。
fn node_write_file_sync(path: &str, contents: &str) -> std::io::Result<()> {
    use std::io::Write as _;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(path)
        .map_err(|error| node_fs_error(path, error, FsSyscall::Open))?;
    file.write_all(contents.as_bytes())
        .map_err(|error| node_fs_error(path, error, FsSyscall::Write))
}

/// `SessionManager::open` 暴露的读侧 fs 错误按 Node 文案重建。
///
/// TS 的实现是 `openSync` + `readSync`：打开失败是 `open '<path>'`，对目录 `readSync`
/// 失败是 `EISDIR: illegal operation on a directory, read`（without path）。Rust 的
/// `io::Error` 只保留 errno，因此 EISDIR 归到 read，其余归到 open。
fn node_session_read_error(path: &str, error: std::io::Error) -> std::io::Error {
    let syscall = if error.kind() == std::io::ErrorKind::IsADirectory {
        FsSyscall::Read
    } else {
        FsSyscall::Open
    };
    node_fs_error(path, error, syscall)
}

/// errno → libuv `uv_strerror()` 的 `(code, description)`。
fn node_error_code_and_description(error: &std::io::Error) -> Option<(&'static str, &'static str)> {
    if let Some(mapped) = node_open_errno(error.raw_os_error()) {
        return Some(mapped);
    }
    // 非 Linux（或无法拿到 errno）时按可移植的 `ErrorKind` 兜底。
    use std::io::ErrorKind;
    match error.kind() {
        ErrorKind::NotFound => Some(("ENOENT", "no such file or directory")),
        ErrorKind::PermissionDenied => Some(("EACCES", "permission denied")),
        ErrorKind::IsADirectory => Some(("EISDIR", "illegal operation on a directory")),
        ErrorKind::NotADirectory => Some(("ENOTDIR", "not a directory")),
        ErrorKind::AlreadyExists => Some(("EEXIST", "file already exists")),
        ErrorKind::StorageFull => Some(("ENOSPC", "no space left on device")),
        ErrorKind::ReadOnlyFilesystem => Some(("EROFS", "read-only file system")),
        ErrorKind::FileTooLarge => Some(("EFBIG", "file too large")),
        ErrorKind::TooManyLinks => Some(("EMLINK", "too many links")),
        ErrorKind::ResourceBusy => Some(("EBUSY", "resource busy or locked")),
        ErrorKind::CrossesDevices => Some(("EXDEV", "cross-device link not permitted")),
        _ => None,
    }
}

/// Linux errno → libuv `uv_strerror()` 文案（open/write/read 共用同一张表）。
#[cfg(target_os = "linux")]
fn node_open_errno(errno: Option<i32>) -> Option<(&'static str, &'static str)> {
    Some(match errno? {
        1 => ("EPERM", "operation not permitted"),
        2 => ("ENOENT", "no such file or directory"),
        5 => ("EIO", "i/o error"),
        13 => ("EACCES", "permission denied"),
        16 => ("EBUSY", "resource busy or locked"),
        17 => ("EEXIST", "file already exists"),
        18 => ("EXDEV", "cross-device link not permitted"),
        20 => ("ENOTDIR", "not a directory"),
        21 => ("EISDIR", "illegal operation on a directory"),
        22 => ("EINVAL", "invalid argument"),
        23 => ("ENFILE", "file table overflow"),
        24 => ("EMFILE", "too many open files"),
        26 => ("ETXTBSY", "text file is busy"),
        27 => ("EFBIG", "file too large"),
        28 => ("ENOSPC", "no space left on device"),
        30 => ("EROFS", "read-only file system"),
        31 => ("EMLINK", "too many links"),
        32 => ("EPIPE", "broken pipe"),
        36 => ("ENAMETOOLONG", "name too long"),
        40 => ("ELOOP", "too many symbolic links encountered"),
        122 => ("EDQUOT", "disk quota exceeded"),
        _ => return None,
    })
}

#[cfg(not(target_os = "linux"))]
fn node_open_errno(_errno: Option<i32>) -> Option<(&'static str, &'static str)> {
    None
}

/// TS `sm.getLeafId()` 的原始值。
///
/// TS 的 `leafId` 是 `_buildIndex` 里最后一条非 header 条目的 `id`（任何类型都原样
/// 保存），被 `branch()`/`resetLeaf()` 改写后一定是字符串或 `null`。
///
/// [`SessionManager::get_leaf_id_json`] 直接返回原值（字符串/`null`/数字等）；
/// `None` 表示 `id` 缺省（`undefined`，`JSON.stringify` 省略键）。为防御该视图把
/// 非字符串 id 也归并为 `undefined`，这里在 `None` 时回退到原始条目：此时 leaf 只能
/// 来自 `_buildIndex`（否则不会是 `None`），取最后一条非 header 条目的 `id` 即可。
fn export_leaf_id(session_manager: &SessionManager, entries: &[JsonValue]) -> Option<JsonValue> {
    match session_manager.get_leaf_id_json() {
        Some(value) => Some(value),
        None => entries
            .iter()
            .rev()
            .find(|entry| entry.get("type").and_then(JsonValue::as_str) != Some("session"))
            .and_then(|entry| entry.get("id"))
            .cloned(),
    }
}

/// 输出路径：`opts.outputPath ? normalizePath(...) : 默认名`。
///
/// `normalizePath` 只做 `~` 展开与 Unicode 归一（不做 resolve），因此相对路径原样写出。
fn resolve_output_path(output_path: Option<&str>, fallback_base: &str) -> String {
    match output_path.filter(|value| !value.is_empty()) {
        Some(path) => normalize_path(path, &PathInputOptions::default()),
        None => format!("{APP_NAME}-session-{fallback_base}.html"),
    }
}

/// 导出会话到 HTML（TS `exportSessionToHtml`）。
///
/// - `session_manager` 必须关联一个真实存在的会话文件，否则返回
///   [`ExportError::CannotExportInMemorySession`] / [`ExportError::NothingToExport`]；
/// - `state` 提供 `systemPrompt` 与 `tools`（可选）；`state` 本身或其中某个字段为
///   `None` 时，对应键从载荷中省略，而不是写成 `null`；
/// - `options` 可以是 [`ExportOptions`] 或由路径字符串转换而来；
/// - 提供 `options.tool_renderer` 时先对原始条目预渲染：结果为空映射时省略
///   `renderedTools` 键（TS 的 `undefined`），否则按 tool call id 的插入顺序写出；
///   整数索引形式的 id（如 `"2"`、`"10"`）按 JS `JSON.stringify` 语义排在最前并按
///   数值升序（键序由 [`kiana_json::js_object::stringify`] 的 `ordered_keys` 决定，
///   `tests/golden/prerender.payload.json` 第一行可见该顺序）。
///
/// 预渲染会对条目里的 `message` / `block` 做 TS 同样的无防护解引用，畸形数据返回
/// [`ExportError::Session`]（V8 `TypeError` 文案），此时不会写文件。
///
/// 返回实际写出（`normalizePath` 之后、未 resolve）的路径。
pub fn export_session_to_html(
    session_manager: &SessionManager,
    state: Option<&ExportSessionState>,
    options: Option<ExportOptions<'_>>,
) -> Result<String, ExportError> {
    let mut options = options.unwrap_or_default();

    let session_file = session_manager
        .get_session_file()
        .map(str::to_string)
        .ok_or(ExportError::CannotExportInMemorySession)?;
    if !Path::new(&session_file).exists() {
        return Err(ExportError::NothingToExport);
    }

    // TS 的 `sm.getEntries()` 是 `JSON.parse` 得到的原始对象：预渲染与载荷都走它。
    let entries = session_manager.get_entries_json();

    let rendered_tools = match options.tool_renderer.as_deref_mut() {
        Some(tool_renderer) => {
            let rendered = pre_render_custom_tools(&entries, tool_renderer)?;
            // TS：空对象 → `undefined`，`JSON.stringify` 因而省略 `renderedTools` 键。
            if rendered.is_empty() {
                None
            } else {
                Some(rendered)
            }
        }
        None => None,
    };

    let leaf_id = export_leaf_id(session_manager, &entries);
    let session_data = SessionData {
        header: session_manager.get_header_json(),
        entries,
        leaf_id,
        system_prompt: state.and_then(|state| state.system_prompt.clone()),
        tools: state.and_then(|state| state.tools.as_ref()).cloned(),
        rendered_tools,
    };

    let html = generate_html(&session_data, options.theme_name.as_deref())?;

    let output_path = resolve_output_path(
        options.output_path.as_deref(),
        &node_basename(&session_file, ".jsonl"),
    );
    node_write_file_sync(&output_path, &html).map_err(ExportError::Io)?;
    Ok(output_path)
}

/// 导出任意会话文件到 HTML（TS `exportFromFile`）。
///
/// `input_path` 先经过 `resolvePath`（相对路径按进程 cwd 绝对化、`~` 展开）；
/// 文件不存在时返回 [`ExportError::FileNotFound`]。输出路径的默认名基于
/// **resolved** 输入路径的文件名。
///
/// 该入口没有 `AgentState`：载荷固定省略 `systemPrompt` / `tools` /
/// `renderedTools` 键，[`ExportOptions::tool_renderer`] 即使提供也不生效。
/// `SessionManager::open` 在加载 v1/v2 会话时可能立即迁移并重写输入文件
/// （见该方法文档）。
pub fn export_from_file(
    input_path: &str,
    options: Option<ExportOptions<'_>>,
) -> Result<String, ExportError> {
    let options = options.unwrap_or_default();
    let resolved_input_path = resolve_path(
        input_path,
        &current_dir_string(),
        &PathInputOptions::default(),
    );
    if !Path::new(&resolved_input_path).exists() {
        return Err(ExportError::FileNotFound {
            path: resolved_input_path,
        });
    }

    let session_manager =
        SessionManager::open(&resolved_input_path, None, None).map_err(|error| {
            let message = match error {
                // TS 的 `openSync`/`readSync` 异常：`error.message` 由 Node 生成。
                SessionManagerError::Io(io_error) => {
                    node_session_read_error(&resolved_input_path, io_error).to_string()
                }
                other => other.to_string(),
            };
            ExportError::Session { message }
        })?;

    let entries = session_manager.get_entries_json();
    let leaf_id = export_leaf_id(&session_manager, &entries);
    let session_data = SessionData {
        header: session_manager.get_header_json(),
        entries,
        leaf_id,
        system_prompt: None,
        tools: None,
        rendered_tools: None,
    };

    let html = generate_html(&session_data, options.theme_name.as_deref())?;

    let output_path = resolve_output_path(
        options.output_path.as_deref(),
        &node_basename(&resolved_input_path, ".jsonl"),
    );
    node_write_file_sync(&output_path, &html).map_err(ExportError::Io)?;
    Ok(output_path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_color_accepts_hex_and_rgb_only() {
        assert_eq!(
            parse_color("#343541"),
            Some(Rgb {
                r: 52.0,
                g: 53.0,
                b: 65.0
            })
        );
        assert_eq!(
            parse_color("rgb(24, 24, 30)"),
            Some(Rgb {
                r: 24.0,
                g: 24.0,
                b: 30.0
            })
        );
        assert_eq!(
            parse_color("rgb( 1 ,\t2 ,\n3 )"),
            Some(Rgb {
                r: 1.0,
                g: 2.0,
                b: 3.0
            })
        );
        assert!(parse_color("#fff").is_none());
        assert!(parse_color("#1234567").is_none());
        assert!(parse_color("#12345g").is_none());
        assert!(parse_color("rgba(1,2,3,1)").is_none());
        assert!(parse_color("oklch(0.7 0.1 200)").is_none());
        assert!(parse_color("red").is_none());
        assert!(parse_color("rgb(1,2,3) ").is_none());
    }

    #[test]
    fn derive_export_colors_matches_verified_vectors() {
        let dark = derive_export_colors("#343541");
        assert_eq!(dark.page_bg, "rgb(36, 37, 46)");
        assert_eq!(dark.card_bg, "rgb(44, 45, 55)");
        assert_eq!(dark.info_bg, "rgb(72, 68, 65)");

        let light = derive_export_colors("#ffffff");
        assert_eq!(light.page_bg, "rgb(245, 245, 245)");
        assert_eq!(light.card_bg, "#ffffff");
        assert_eq!(light.info_bg, "rgb(255, 255, 235)");

        let unknown = derive_export_colors("oklch(0.7 0.1 200)");
        assert_eq!(unknown.page_bg, "rgb(24, 24, 30)");
        assert_eq!(unknown.card_bg, "rgb(30, 30, 36)");
        assert_eq!(unknown.info_bg, "rgb(60, 55, 40)");
    }

    #[test]
    fn adjust_brightness_formats_like_ts() {
        assert_eq!(adjust_brightness("#343541", 0.7), "rgb(36, 37, 46)");
        assert_eq!(adjust_brightness("nope", 0.5), "nope");
    }

    #[test]
    fn node_basename_matches_node_vectors() {
        assert_eq!(node_basename("session.jsonl", ".jsonl"), "session");
        assert_eq!(node_basename(".jsonl", ".jsonl"), "");
        assert_eq!(node_basename("/.jsonl", ".jsonl"), ".jsonl");
        assert_eq!(node_basename("a/.jsonl", ".jsonl"), ".jsonl");
        assert_eq!(node_basename("bc", "bc"), "");
        assert_eq!(node_basename("/bc", "bc"), "bc");
        assert_eq!(node_basename("a/bc", "bc"), "bc");
        assert_eq!(node_basename("b.jsonl.jsonl", ".jsonl"), "b.jsonl");
        assert_eq!(node_basename("b", ".jsonl"), "b");
        assert_eq!(node_basename("a/b/c.jsonl", ".jsonl"), "c");
        assert_eq!(node_basename("/foo/bar/", ".jsonl"), "bar");
        assert_eq!(node_basename("/", ".jsonl"), "");
    }

    #[test]
    fn generate_theme_vars_dark_is_56_plus_3_lines() {
        // 明确主题：不依赖全局 currentThemeName。
        let vars = generate_theme_vars(Some("dark")).expect("dark theme");
        let lines: Vec<&str> = vars.split("\n      ").collect();
        assert_eq!(lines.len(), 59, "56 token lines + 3 export lines");
        assert!(lines[0].starts_with("--accent: "), "{}", lines[0]);
        assert!(
            vars.ends_with("--exportInfoBg: #4e2f1b;"),
            "unexpected tail: {}",
            &vars[vars.len() - 64..]
        );
        assert!(vars.contains("--exportPageBg: #21252c;"));
        assert!(vars.contains("--exportCardBg: #282c34;"));
    }

    #[test]
    fn unknown_theme_propagates_theme_message() {
        let error = generate_theme_vars(Some("no-such-theme")).expect_err("missing theme");
        assert_eq!(error.to_string(), "Theme not found: no-such-theme");
    }

    #[test]
    fn js_replace_first_matches_node_vectors() {
        // 以下期望值由 Node 26 实测（`"abc".replace("b", ...)` 等）。
        assert_eq!(js_replace_first("abc", "b", "$1$&$$$`$'"), "a$1b$acc");
        assert_eq!(js_replace_first("abc", "b", "$0$x$<n>"), "a$0$x$<n>c");
        assert_eq!(js_replace_first("abc", "b", "$$&"), "a$&c");
        assert_eq!(js_replace_first("abc", "b", "$"), "a$c");
        assert_eq!(js_replace_first("x{{P}}y", "{{P}}", "a$&b"), "xa{{P}}by");
        assert_eq!(js_replace_first("x{{P}}y", "{{P}}", "a$`b"), "xaxby");
        assert_eq!(js_replace_first("x{{P}}y", "{{P}}", "a$'b"), "xayby");
        // 没有匹配时原样返回。
        assert_eq!(js_replace_first("abc", "z", "$&"), "abc");
    }

    #[test]
    fn output_path_uses_normalize_path_without_resolving() {
        let path = resolve_output_path(Some("../relative/out.html"), "session");
        assert_eq!(path, "../relative/out.html");
        assert_eq!(
            resolve_output_path(Some(""), "session"),
            "kiana-session-session.html"
        );
        assert_eq!(
            resolve_output_path(None, "b.jsonl"),
            "kiana-session-b.jsonl.html"
        );
    }
}
