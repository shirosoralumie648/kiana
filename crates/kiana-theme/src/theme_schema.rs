//! theme-schema.ts（手写校验器，逐字复刻 typebox 1.3.27 / en_US 的错误文本）。
//!
//! TS: `packages/coding-agent/src/modes/interactive/theme/theme-schema.ts`（146 行）。
//!
//! TS 用 typebox 编译一份 JSON Schema 再 `Check`/`Errors`。Rust 侧不引入 typebox，而是按
//! 同一份 schema 手写校验：对象键序（`properties` 按声明顺序、`additionalProperties` 与
//! `Record` 按实例的 **JS 枚举顺序**：数组索引形式的键先按数值升序，其余按插入顺序）、
//! 错误分类与聚合格式是当前稳定契约（主题作者会看到这些消息），改动需同步测试。
//!
//! 与 typebox 对齐的细节（用 TS 参照实现跑过探针）：
//!
//! - 每个对象节点按 `type → required → additionalProperties → properties` 的顺序产出错误；
//! - `additionalProperties: false` 会为**每个**多余键先产出一条 `boolean` 错误
//!   （`schema is false`），再在对象路径上产出一条 `additionalProperties` 错误；
//! - `ColorValue = string | integer 0..255` 的 union 先跑 string 分支、再跑 integer 分支，
//!   两支都失败时追加一条 `anyOf` 错误；数值的 `minimum`/`maximum` 与 `integer` 类型检查
//!   互不短路（`-2.5` 同时得到 `must be integer` 和 `must be >= 0`）；
//! - 浮点整数值（`24.0`）算 integer，非整数值（`24.5`）不算；
//! - `required` 错误挂在**对象**路径上，`params.requiredProperties` 是缺失键的声明顺序列表；
//! - `Type.Literal("dark")` 编译成 `{ type: "string", const: "dark" }`，非字符串的
//!   `appearance` 会得到每个分支的 `must be string` + `must be equal to constant`；
//! - typebox 的 `Settings.Get().maxErrors = 8`：`ErrorContext.AddError()` 在错误数达到 8 后
//!   丢弃后续错误（anyOf 分支使用独立上下文，合并时仍然受父上下文容量限制）。因此每个文档
//!   最多 8 条错误 —— `/colors` 的 required 错误落在第 8 条之后时，缺失 token 块不会出现。
//!
//! `validateThemeJson()` 的聚合（spec §2.5）：`/colors` 上的 `required` 错误汇总成按字母
//! 排序的 missing-token 列表，其余错误逐行 `  - <path>: <message>`。注意标题后始终有一个
//! 换行，因此没有 missing-token 块时会出现连续三个 `\n`（`...":\n\n\nOther errors:`），
//! 这一点是当前稳定输出契约，改动需同步测试。
//!
//! ## 未安装校验器时的语义
//!
//! TS 的 `parseThemeJson()` 只是把 `JSON.parse` 的结果 cast 成 `ThemeJson`：变量解析发生在
//! `createTheme()`（颜色）与 `getThemeExportColors()`（导出）里。本模块因此把「文档视图」
//! 与「解析」分开：`theme_json_from_value` 只构建可表示视图并保留原始文档
//! (`ThemeJson::raw`)，`resolve_theme_colors_from_document` 与
//! `resolve_theme_export_from_document` 复刻 TS 的惰性/宽松行为（未引用的非法变量不影响
//! 加载、未知 `colors` 键抛 `Cannot read properties of undefined (reading 'slot')`、
//! `export` 段整段失败返回空）。安装校验器时这些分支不会触发。
//!
//! # 为什么手写而不是复用 schema 库
//!
//! 校验失败的消息是**用户可见文案**（主题作者会直接读到），而且 TS 侧的措辞由
//! typebox 1.3.27 编译出的 JSON Schema 决定。换一个通用 JSON Schema 校验器就会改变
//! 错误分类、顺序与格式，因此这里按同一份 schema 手写，并把 JS 特有的语义（对象键枚举
//! 顺序、`Object.prototype` 成员、`String.prototype.trim` 的空白集合、正则的 ASCII-only
//! `i`）显式实现。`validator_reports_keys_in_javascript_object_order`、
//! `caps_errors_at_typeboxes_max_errors_like_the_reference` 等测试逐字锁定这些行为。
//!
//! # 两条入口
//!
//! 1. **严格路径** `validate_theme_json()`：生产进程由 `kiana-coding-agent` 启动时调用
//!    `kiana_theme::set_theme_json_validator` 安装到 `theme.rs` 的全局钩子；失败时调用方
//!    拿到逐字错误消息，成功时得到 `ThemeJson` 视图（颜色仍未解析）。
//! 2. **未校验路径** `theme_json_from_value()`（TS `parseThemeJson()` 的纯 cast）：内置主题
//!    （`theme.rs` 的 `builtin_themes()`）和未安装校验器时的用户主题直接走它；随后
//!    `createTheme()` 的等价物 `resolve_theme_colors_for_create()` 与
//!    `resolve_theme_export_from_document()` 从 `raw` 做惰性求值（装了校验器时解析逻辑
//!    相同，只是输入已保证合法）。
//!
//! # 执行流程（严格路径）
//!
//! 1. `validate_theme_json()` 先做 TS 的 `name` 含 `/` 预检；
//! 2. `validate_document()` 自顶向下遍历，每个对象节点按
//!    `type → required → additionalProperties → properties` 的顺序产出错误；
//! 3. 错误在 `push_error()` 处按 `MAX_ERRORS = 8` 截断（anyOf 分支使用独立缓冲，
//!    合并时仍受父级容量限制）；
//! 4. `/colors` 的 `required` 错误被改写成按字母排序的 missing-token 列表，其余错误按
//!    产生顺序逐行输出。
//!
//! # 维护约束
//!
//! - `theme_tokens.rs` 的 token 声明顺序与 fallback 表直接决定 required/optional 集合与
//!   错误顺序；两者与 TS 不一致会立刻体现在错误文案里。
//! - `js_ordered_keys()` 依赖 `serde_json` 的 `preserve_order`（工作区 `Cargo.toml` 已开启）；
//!   关掉该 feature 后 `serde_json::Map` 会退化为按 key 排序的 `BTreeMap`，普通键的
//!   插入顺序将丢失。
//! - 任何“更合理”的校验顺序或措辞改动都会被测试逐字比对出来；修改前先对照 TS 参照实现。
//!
//! # 阅读顺序
//!
//! 先看 `ThemeColorValue` / `ThemeJson` / `ThemeError` 三个公开类型，再看
//! `validate_theme_json()` 与 `/colors` 校验，最后看“未校验路径”对应的
//! `theme_json_from_value()`、`resolve_*` 函数。

use std::fmt;

use indexmap::IndexMap;
use kiana_tui::TerminalColorScheme;
use serde_json::Value;

use crate::theme_tokens::{THEME_TOKENS, ThemeToken};

/// TS: `ValidatedThemeJson` 里的颜色值（`string | integer 0..255`）。
///
/// 这个类型只承载两种 JSON 可表示的形态，**不校验字符串内容**：`Str` 可以是变量名、
/// 空串（终端默认色的哨兵，见 [`ThemeColorValue::is_empty`]）或其他任意文本，真正的颜色
/// 语法解析发生在 `Theme::new()`（内部走 `parse_color`）；`Index` 的载荷是 `u8`，因此天然
/// 落在 0..=255，只有把 JSON 数字转成索引时才需要显式拒绝非整数/越界值（见
/// `color_from_resolved`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ThemeColorValue {
    /// 颜色字符串：`#RGB`/`#RRGGBB`、`oklch(...)`/`okhsl(...)`、变量引用，或 `""`（终端默认）。
    Str(String),
    /// 256 色索引（0-255）。
    Index(u8),
}

impl ThemeColorValue {
    /// 字符串形态（`Str`）。
    pub fn as_str(&self) -> Option<&str> {
        match self {
            ThemeColorValue::Str(value) => Some(value),
            ThemeColorValue::Index(_) => None,
        }
    }

    /// 数值形态（`Index`）。
    pub fn as_index(&self) -> Option<u8> {
        match self {
            ThemeColorValue::Str(_) => None,
            ThemeColorValue::Index(index) => Some(*index),
        }
    }

    /// 是否是空串（终端默认色）。
    pub fn is_empty(&self) -> bool {
        matches!(self, ThemeColorValue::Str(value) if value.is_empty())
    }
}

impl fmt::Display for ThemeColorValue {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ThemeColorValue::Str(value) => formatter.write_str(value),
            ThemeColorValue::Index(index) => write!(formatter, "{index}"),
        }
    }
}

/// TS: `ThemeJson["export"]`（HTML 导出用）。
///
/// 三个字段都可省略，主题可以只给出其中一部分。本结构是 `ThemeJson.export` 的展示视图，
/// 每个字段独立尽力转换；需要 TS `getThemeExportColors()` 的“任一项失败则整段为空”语义
/// 时使用 `resolve_theme_export_from_document()`。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ThemeExport {
    /// `pageBg`。
    pub page_bg: Option<ThemeColorValue>,
    /// `cardBg`。
    pub card_bg: Option<ThemeColorValue>,
    /// `infoBg`。
    pub info_bg: Option<ThemeColorValue>,
}

/// TS: `ValidatedThemeJson`（`Static<typeof ThemeJsonSchema>`）。
///
/// 这是**文档视图**而不是解析结果：`colors`/`vars` 里的变量引用尚未展开（变量名原样留在
/// [`ThemeColorValue::Str`] 里），无法表示的值被跳过；完整原文保留在 `raw`，供
/// `resolve_theme_colors_for_create()` 与 `resolve_theme_export_from_document()` 复刻
/// TS 的惰性解析。`colors` 只包含已知 token，键序与主题文件一致（`serde_json` 开启
/// `preserve_order`）。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ThemeJson {
    /// `$schema`。
    pub schema: Option<String>,
    /// 主题名（`Type.String({ pattern: "^[^/]+$" })`）。
    pub name: Option<String>,
    /// 主题面向的终端外观（省略时由颜色推导）。
    pub appearance: Option<TerminalColorScheme>,
    /// 原始 `name` 值（TS `createTheme()` 原样保存进 `Theme.name`，未校验路径下可以是任意
    /// JSON 值；`name` 只保留字符串形态）。
    pub(crate) raw_name: Option<Value>,
    /// 原始 `appearance` 值（TS `Theme.appearance` 原样返回；`appearance` 只保留字面量）。
    pub(crate) raw_appearance: Option<Value>,
    /// 可复用颜色变量。
    pub vars: IndexMap<String, ThemeColorValue>,
    /// token → 颜色（键序 = 主题文件里的顺序，仅保留已知 token）。
    pub colors: IndexMap<ThemeToken, ThemeColorValue>,
    /// HTML 导出颜色（尽力而为的视图；真正解析见 `resolve_theme_export_from_document`）。
    pub export: Option<ThemeExport>,
    /// TS `parseThemeJson()` 原样 cast 的文档。
    ///
    /// TS 的变量解析发生在 `createTheme()`（颜色）与 `getThemeExportColors()`（导出）里，
    /// 而不是解析 JSON 时；`loadThemeJson()` 只做 JSON.parse + 可选校验。为了复刻这个
    /// 时序（例如 `getThemeExportColors()` 不会因为颜色的变量引用坏了而返回 `{}`），
    /// 这里保留原始文档供两处解析使用。
    pub(crate) raw: Value,
}

/// 主题子系统的公开错误。
///
/// `Display` 逐字等于 TS 抛出的 `Error.message`（ARCHITECTURE §4.1），因此调用方可以直接
/// 把它拼进 `Failed to load theme …` 之类的用户可见文案。
///
/// 错误只有一条消息，没有种类或 `source`：需要分支的调用方只能比较文本；主题加载路径
/// （`theme.rs` 的 `load_theme*`）也因此把各种失败统一当作 `ThemeError` 向上返回。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThemeError {
    message: String,
}

impl ThemeError {
    /// 用一条逐字错误消息构造。
    pub fn new(message: impl Into<String>) -> Self {
        ThemeError {
            message: message.into(),
        }
    }

    /// 错误消息（TS: `error.message`）。
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for ThemeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for ThemeError {}

/// `/^ok(lch|hsl)\(/i`：直接可用的颜色字面量（变量解析时原样返回）。
///
/// JS 的非 unicode `i` 只对 ASCII 做大小写折叠（`/s/i.test("\u{17F}") === false`，Node
/// 26 实测），而 `regex` 的 `(?i)` 是 Unicode 感知的（会把 U+017F 折成 `s`），因此这里
/// 用 ASCII 比较而不是正则。
fn ok_lch_or_hsl(value: &str) -> bool {
    match value.get(..6) {
        Some(prefix) => {
            prefix.eq_ignore_ascii_case("oklch(") || prefix.eq_ignore_ascii_case("okhsl(")
        }
        None => false,
    }
}

// ---------------------------------------------------------------------------
// JS 对象/字符串语义（主题是 JS 模块的逐字移植，手写校验器与未校验路径都要跟 JS 一致）
// ---------------------------------------------------------------------------

/// `Object.prototype` 的自有属性名（`Object.getOwnPropertyNames(Object.prototype)`）。
///
/// 主题里的 `colors`/`vars` 都是 **普通对象**：`THEME_TOKENS[key]`、`value in vars` 这类
/// 查找会看到这些继承成员。JS 侧的行为（`theme.ts`）因此与“只认自有键”的 Rust 实现不同，
/// 这些名字在两处需要特殊处理（见 [`theme_json_colors`] 与 [`json_var_lookup`]）。
pub(crate) const OBJECT_PROTOTYPE_KEYS: [&str; 12] = [
    "constructor",
    "__defineGetter__",
    "__defineSetter__",
    "hasOwnProperty",
    "__lookupGetter__",
    "__lookupSetter__",
    "isPrototypeOf",
    "propertyIsEnumerable",
    "toString",
    "valueOf",
    "__proto__",
    "toLocaleString",
];

/// `Array.prototype` 的自有属性名（`Object.getOwnPropertyNames(Array.prototype)`）。
///
/// `length` 在这里只是为完整性保留：查找时它先按“数组长度”处理，不会走到继承成员分支。
const ARRAY_PROTOTYPE_KEYS: [&str; 41] = [
    "constructor",
    "at",
    "concat",
    "copyWithin",
    "fill",
    "find",
    "findIndex",
    "findLast",
    "findLastIndex",
    "lastIndexOf",
    "pop",
    "push",
    "reverse",
    "shift",
    "unshift",
    "slice",
    "sort",
    "splice",
    "includes",
    "indexOf",
    "join",
    "keys",
    "entries",
    "values",
    "forEach",
    "filter",
    "flat",
    "flatMap",
    "map",
    "every",
    "some",
    "reduce",
    "reduceRight",
    "toReversed",
    "toSorted",
    "toSpliced",
    "with",
    "toLocaleString",
    "toString",
    "length",
    "valueOf",
];

/// 名字是否是 `Object.prototype` 的成员（`name in {}` 恒为真）。
pub(crate) fn is_object_prototype_key(name: &str) -> bool {
    OBJECT_PROTOTYPE_KEYS.contains(&name)
}

/// 名字是否是数组实例上的继承成员（`name in []` 恒为真）。
fn is_array_prototype_key(name: &str) -> bool {
    ARRAY_PROTOTYPE_KEYS.contains(&name) || is_object_prototype_key(name)
}

/// JS 的“数组索引”属性名：`"0"`、`"1"`…`"4294967294"`（无前导零，< 2^32-1）。
fn js_array_index(key: &str) -> Option<u32> {
    if key.is_empty() || key.len() > 10 {
        return None;
    }
    let bytes = key.as_bytes();
    if bytes.len() > 1 && bytes[0] == b'0' {
        return None;
    }
    if !bytes.iter().all(u8::is_ascii_digit) {
        return None;
    }
    let index: u32 = key.parse().ok()?;
    (index != u32::MAX).then_some(index)
}

/// `Object.keys(object)` 的枚举顺序：数组索引形式的键按数值升序排在前面，其余键保持插入
/// 顺序（typebox 的 `additionalProperties`/`Record` 错误顺序、`Object.entries()` 都依赖它）。
pub(crate) fn js_ordered_keys(object: &serde_json::Map<String, Value>) -> Vec<&String> {
    // 规范数组索引形式的键在 JS 里总是最先枚举（按数值升序），其余键随后按插入顺序；
    // `serde_json` 开启 preserve_order 时 Map 保留插入顺序，正好提供后一组。
    let mut indices: Vec<(u32, &String)> = Vec::new();
    let mut others: Vec<&String> = Vec::new();
    for key in object.keys() {
        match js_array_index(key) {
            Some(index) => indices.push((index, key)),
            None => others.push(key),
        }
    }
    indices.sort_by_key(|(index, _)| *index);
    indices
        .into_iter()
        .map(|(_, key)| key)
        .chain(others)
        .collect()
}

/// `String.prototype.trim()`：去掉 ECMAScript 的 WhiteSpace + LineTerminator。
///
/// 与 Rust 的 [`str::trim`] 有两点不同：JS 去掉 U+FEFF（BOM）但**不**去掉 U+0085（NEL）；
/// Rust 恰好相反。自动主题设置（`"light/dark"`）与 `COLORFGBG` 的字段都按 JS 语义裁剪。
pub(crate) fn js_trim(text: &str) -> &str {
    fn is_js_whitespace(character: char) -> bool {
        matches!(
            character,
            '\u{0009}'
                | '\u{000A}'
                | '\u{000B}'
                | '\u{000C}'
                | '\u{000D}'
                | '\u{0020}'
                | '\u{00A0}'
                | '\u{1680}'
                | '\u{2000}'
                ..='\u{200A}'
                    | '\u{2028}'
                    | '\u{2029}'
                    | '\u{202F}'
                    | '\u{205F}'
                    | '\u{3000}'
                    | '\u{FEFF}'
        )
    }
    text.trim_matches(is_js_whitespace)
}

/// TS: `resolveVarRefs(value, vars, visited)` —— 把变量引用解析成字面颜色值。
///
/// 非字符串（`Index`）原样返回。字符串先做字面量短路，再查 `vars`（`IndexMap` 只含自有
/// 键；若名字是 `Object.prototype` 的成员，则复刻 JS 在函数值上访问 `.startsWith` 的
/// TypeError），命中后递归展开。`visited` 记录本次解析链上已展开的引用名，重复出现即报环。
pub(crate) fn resolve_var_refs(
    value: &ThemeColorValue,
    vars: &IndexMap<String, ThemeColorValue>,
    visited: &mut std::collections::HashSet<String>,
) -> Result<ThemeColorValue, ThemeError> {
    let ThemeColorValue::Str(text) = value else {
        return Ok(value.clone());
    };
    // 空串、`#hex`、`ok(lch|hsl)(…)` 已经是字面量，不参与变量查找。
    if text.is_empty() || text.starts_with('#') || ok_lch_or_hsl(text) {
        return Ok(value.clone());
    }
    if visited.contains(text) {
        return Err(ThemeError::new(format!(
            "Circular variable reference detected: {text}"
        )));
    }
    let Some(next) = vars.get(text) else {
        // `text in vars` 对普通对象会看到 `Object.prototype` 的继承成员（函数/对象），
        // 递归后会在 `value.startsWith` 上抛出 TypeError。
        if is_object_prototype_key(text) {
            return Err(ThemeError::new("value.startsWith is not a function"));
        }
        return Err(ThemeError::new(format!(
            "Variable reference not found: {text}"
        )));
    };
    visited.insert(text.clone());
    resolve_var_refs(next, vars, visited)
}

/// TS: `resolveThemeColors(colors, vars = {})` —— 对每个 entry 解析变量引用。
pub(crate) fn resolve_theme_colors(
    colors: &IndexMap<ThemeToken, ThemeColorValue>,
    vars: &IndexMap<String, ThemeColorValue>,
) -> Result<IndexMap<ThemeToken, ThemeColorValue>, ThemeError> {
    let mut resolved = IndexMap::with_capacity(colors.len());
    for (token, value) in colors {
        let mut visited = std::collections::HashSet::new();
        resolved.insert(*token, resolve_var_refs(value, vars, &mut visited)?);
    }
    Ok(resolved)
}

// =============================================================================
// 手写校验器（typebox 等价物）
// =============================================================================

/// typebox 的 `Settings.Get().maxErrors`（`build/system/settings/settings.mjs`）。
///
/// `ErrorContext.AddError()` 在错误数达到上限后丢弃后续错误（`AtCapacity():
/// errors.length >= maxErrors`），且 `ErrorSchema()` 在容量耗尽时直接短路返回。因此
/// 每个文档最多产生 8 条错误；`validateThemeJson()` 的 `Errors(json)` 看到的就是这 8 条。
/// anyOf 的分支各自使用**新的** `ErrorContext`（不受父上下文容量影响），分支失败后再
/// 逐条 `AddError` 合并回父上下文 —— 所以这里同样先完整收集分支错误，合并时再截断。
const MAX_ERRORS: usize = 8;

/// 一条 typebox 风格的校验错误。
#[derive(Debug, Clone)]
struct ValidationError {
    keyword: &'static str,
    instance_path: String,
    message: String,
    required_properties: Vec<String>,
}

impl ValidationError {
    fn new(
        keyword: &'static str,
        instance_path: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        ValidationError {
            keyword,
            instance_path: instance_path.into(),
            message: message.into(),
            required_properties: Vec::new(),
        }
    }
}

fn json_pointer_segment(key: &str) -> String {
    // typebox 直接用字符串拼接构造 instancePath，不做 JSON Pointer 转义（主题 token 名里
    // 没有 `/` 或 `~`，`vars` 的键可能任意；instancePath 按此稳定格式原样拼接）。
    key.to_string()
}

/// `/^[^/]+$/` 的等价值：非空且不含 `/`。
fn name_pattern_matches(name: &str) -> bool {
    !name.is_empty() && !name.contains('/')
}

/// JS 的 `Number.isInteger`。
fn is_js_integer(number: &serde_json::Number) -> bool {
    // 整数形态直接命中；浮点形态（`24.0`）要排除小数与非有限值（`-0.0` 也算整数）。
    if number.as_i64().is_some() || number.as_u64().is_some() {
        return true;
    }
    match number.as_f64() {
        Some(value) => value.is_finite() && value.fract() == 0.0,
        None => false,
    }
}

/// JS 模板字符串 `${value}` 的等价物（只用于错误文本，值来自 `serde_json::Value`）。
fn js_string_of(value: &Value) -> String {
    match value {
        Value::Null => "null".to_string(),
        Value::Bool(boolean) => boolean.to_string(),
        Value::Number(number) => match number.as_f64() {
            Some(value) => kiana_tui::colors::format_number(value),
            None => number.to_string(),
        },
        Value::String(text) => text.clone(),
        Value::Array(items) => items.iter().map(js_string_of).collect::<Vec<_>>().join(","),
        Value::Object(_) => "[object Object]".to_string(),
    }
}

/// `ErrorContext.AddError()`：达到 [`MAX_ERRORS`] 后丢弃。
fn push_error(errors: &mut Vec<ValidationError>, error: ValidationError) {
    if errors.len() < MAX_ERRORS {
        errors.push(error);
    }
}

fn push_type_error(errors: &mut Vec<ValidationError>, path: &str, kind: &str) {
    push_error(
        errors,
        ValidationError::new("type", path, format!("must be {kind}")),
    );
}

fn validate_string(value: &Value, path: &str, errors: &mut Vec<ValidationError>) {
    if !value.is_string() {
        push_type_error(errors, path, "string");
    }
}

/// `Type.Union([Type.String(...), Type.Integer({minimum:0, maximum:255})])`。
///
/// 与 typebox 的 `ErrorAnyOf` 一样：分支错误先收集，只有**所有**分支都失败时才合并进
/// 错误列表（通过时分支错误被丢弃），最后追加一条 `anyOf`。
fn validate_color_value(value: &Value, path: &str, errors: &mut Vec<ValidationError>) {
    // 两个分支各自收集错误，只有两支都失败才把分支错误合并进父列表；通过的文档不会
    // 留下分支噪声，失败的文档则保持 typebox 的 `type`/范围关键字顺序。
    let mut branch_errors = Vec::new();
    let string_ok = value.is_string();
    if !string_ok {
        push_type_error(&mut branch_errors, path, "string");
    }

    let mut integer_ok = false;
    let mut in_range = true;
    if let Value::Number(number) = value {
        integer_ok = is_js_integer(number);
        if !integer_ok {
            push_type_error(&mut branch_errors, path, "integer");
        }
        let numeric = number.as_f64().unwrap_or(f64::NAN);
        // typebox 的数值关键字顺序：maximum 在 minimum 之前；而且整数类型检查不短路
        // 范围检查，所以 `-2.5` 会同时得到 `must be integer` 和 `must be >= 0`。
        if numeric > 255.0 {
            branch_errors.push(ValidationError::new("maximum", path, "must be <= 255"));
            in_range = false;
        }
        if numeric < 0.0 {
            branch_errors.push(ValidationError::new("minimum", path, "must be >= 0"));
            in_range = false;
        }
    } else {
        push_type_error(&mut branch_errors, path, "integer");
    }

    if string_ok || (integer_ok && in_range) {
        return;
    }
    // 分支错误合并回父上下文时逐条走 `AddError`（受容量限制），最后追加 anyOf 汇总。
    for error in branch_errors {
        push_error(errors, error);
    }
    push_error(
        errors,
        ValidationError::new("anyOf", path, "must match a schema in anyOf"),
    );
}

/// `type: "dark" | "light"`（`Type.Union([Type.Literal("dark"), Type.Literal("light")])`）。
///
/// `Type.Literal("dark")` 编译出来的 schema 是 `{ type: "string", const: "dark" }`，错误顺序
/// 是每个分支先 `type` 再 `const`：所以非字符串输入会得到
/// `must be string, must be equal to constant` × 2 + `anyOf`（共 5 条），而只是不匹配的
/// 字符串只会得到 `const` × 2 + `anyOf`（共 3 条）。
fn validate_appearance(value: &Value, path: &str, errors: &mut Vec<ValidationError>) {
    let is_string = value.is_string();
    let dark = value.as_str() == Some("dark");
    let light = value.as_str() == Some("light");
    if dark || light {
        return;
    }
    for _ in 0..2 {
        if !is_string {
            push_type_error(errors, path, "string");
        }
        push_error(
            errors,
            ValidationError::new("const", path, "must be equal to constant"),
        );
    }
    push_error(
        errors,
        ValidationError::new("anyOf", path, "must match a schema in anyOf"),
    );
}

/// 收集对象上的多余键，产出 `boolean` 逐键错误 + `additionalProperties` 汇总错误。
fn validate_additional_properties(
    object: &serde_json::Map<String, Value>,
    path: &str,
    known: &[&str],
    errors: &mut Vec<ValidationError>,
) {
    let mut additional = Vec::new();
    // 两个阶段与 typebox 的 ErrorAdditionalProperties 一致：先给每个多余键记一条子路径上的
    // `schema is false`，全部结束后再在对象路径上记一条汇总错误。汇总错误同样占 8 条名额，
    // 可能因为上限而被丢弃。
    // 类型盒按 `Object.keys()` 的枚举顺序报错：数组索引形式的键先按数值升序，其余按插入序。
    for key in js_ordered_keys(object) {
        if known.contains(&key.as_str()) {
            continue;
        }
        let child = format!("{path}/{}", json_pointer_segment(key));
        push_error(
            errors,
            ValidationError::new("boolean", child, "schema is false"),
        );
        additional.push(key.clone());
    }
    if !additional.is_empty() {
        push_error(
            errors,
            ValidationError::new(
                "additionalProperties",
                path,
                "must not have additional properties",
            ),
        );
    }
}

/// `ThemeColorsSchema`：56 个 token 属性 + `additionalProperties: false`。
fn validate_colors(value: &Value, errors: &mut Vec<ValidationError>) {
    let path = "/colors";
    let Some(object) = value.as_object() else {
        push_type_error(errors, path, "object");
        return;
    };

    // schema 里的 `Type.Optional` 来自 descriptor.fallback：有 fallback 的 token 可以省略，
    // 其余都是 required。缺失列表保持 THEME_TOKENS 声明顺序（错误文本直接 join 它）。
    let required: Vec<&str> = THEME_TOKENS
        .iter()
        .zip(ThemeToken::ALL.iter())
        .filter(|(descriptor, _)| descriptor.fallback.is_none())
        .map(|(_, token)| token.as_str())
        .collect();
    let missing: Vec<&str> = required
        .into_iter()
        .filter(|name| !object.contains_key(*name))
        .collect();
    if !missing.is_empty() {
        let mut error = ValidationError::new(
            "required",
            path,
            format!("must have required properties {}", missing.join(", ")),
        );
        error.required_properties = missing.iter().map(|name| (*name).to_string()).collect();
        push_error(errors, error);
    }

    // 节点内的顺序与 schema 一致：required → additionalProperties → 各 token 属性
    // （声明顺序）。这个顺序决定错误在 8 条上限内的去留。
    let known: Vec<&str> = ThemeToken::ALL.iter().map(|token| token.as_str()).collect();
    validate_additional_properties(object, path, &known, errors);

    for token in ThemeToken::ALL {
        if let Some(item) = object.get(token.as_str()) {
            let child = format!("{path}/{}", token.as_str());
            validate_color_value(item, &child, errors);
        }
    }
}

/// `vars: Type.Record(Type.String(), ColorValueSchema)`。
///
/// typebox 把它编译成 `patternProperties: { "^.*$": … }`（没有 `additionalProperties:
/// false`）。JS 正则的 `.`（无 `s` 标志）不匹配行终止符，所以含 `\n`/`\r`/U+2028/U+2029
/// 的键**完全不参与校验**（Node 26 实测：`vars: {"a\\rb": true}` 通过）。
fn validate_vars(value: &Value, errors: &mut Vec<ValidationError>) {
    let path = "/vars";
    let Some(object) = value.as_object() else {
        push_type_error(errors, path, "object");
        return;
    };
    for key in js_ordered_keys(object) {
        if key.chars().any(is_js_line_terminator) {
            continue;
        }
        let item = &object[key];
        let child = format!("{path}/{}", json_pointer_segment(key));
        validate_color_value(item, &child, errors);
    }
}

/// JS 的行终止符（`LineTerminator`）：`.` 在非 `s` 正则里不匹配它们。
fn is_js_line_terminator(character: char) -> bool {
    matches!(character, '\n' | '\r' | '\u{2028}' | '\u{2029}')
}

/// `export: Type.Object({pageBg?, cardBg?, infoBg?}, {additionalProperties: false})`。
fn validate_export(value: &Value, errors: &mut Vec<ValidationError>) {
    let path = "/export";
    let Some(object) = value.as_object() else {
        push_type_error(errors, path, "object");
        return;
    };
    validate_additional_properties(object, path, &["pageBg", "cardBg", "infoBg"], errors);
    for key in ["pageBg", "cardBg", "infoBg"] {
        if let Some(item) = object.get(key) {
            let child = format!("{path}/{key}");
            validate_color_value(item, &child, errors);
        }
    }
}

/// 根对象：`ThemeJsonSchema`。
fn validate_document(value: &Value, errors: &mut Vec<ValidationError>) {
    let Some(object) = value.as_object() else {
        push_type_error(errors, "", "object");
        return;
    };

    // 根对象只有两个必填属性；appearance/vars/export/$schema 都是可选的。
    let required = ["name", "colors"];
    let missing: Vec<&str> = required
        .iter()
        .copied()
        .filter(|name| !object.contains_key(*name))
        .collect();
    if !missing.is_empty() {
        let mut error = ValidationError::new(
            "required",
            "",
            format!("must have required properties {}", missing.join(", ")),
        );
        error.required_properties = missing.iter().map(|name| (*name).to_string()).collect();
        push_error(errors, error);
    }

    // required 先于 additionalProperties，后者又先于 properties —— typebox 的固定顺序。
    validate_additional_properties(
        object,
        "",
        &["$schema", "name", "appearance", "vars", "colors", "export"],
        errors,
    );

    // 属性按 schema 的声明顺序校验；`$schema`/`name` 是字符串，appearance 是两值字面量
    // 联合，vars/colors/export 递归到各自的子校验器。
    if let Some(schema) = object.get("$schema") {
        validate_string(schema, "/$schema", errors);
    }
    if let Some(name) = object.get("name") {
        validate_string(name, "/name", errors);
        if let Some(text) = name.as_str()
            && !name_pattern_matches(text)
        {
            push_error(
                errors,
                ValidationError::new("pattern", "/name", "must match pattern \"^[^/]+$\""),
            );
        }
    }
    if let Some(appearance) = object.get("appearance") {
        validate_appearance(appearance, "/appearance", errors);
    }
    if let Some(vars) = object.get("vars") {
        validate_vars(vars, errors);
    }
    if let Some(colors) = object.get("colors") {
        validate_colors(colors, errors);
    }
    if let Some(export) = object.get("export") {
        validate_export(export, errors);
    }
}

/// TS: `validateThemeJson(label, json)` —— 校验一份主题文档，失败时抛出逐字消息。
///
/// `label` 只用于错误消息（调用方传主题名或文件路径），`json` 是已经过 `JSON.parse`
/// 的文档；函数不读磁盘、不修改入参。成功返回 [`ThemeJson`] 视图（颜色仍未解析），失败
/// 返回 [`ThemeError`]，其 `Display` 就是 TS 会抛出的完整消息。
///
/// 两个与 typebox 顺序相关的细节：
///
/// - `name` 是字符串且含 `/` 时**先于** schema 校验直接拒绝
///   （`rejects_names_with_slashes_before_schema_validation` 锁定该顺序）；
/// - schema 错误最多保留 8 条（等价于 typebox 的 `maxErrors`）；`/colors` 的 required
///   错误若排在第 8 条之后被丢弃，missing-token 块就不会出现
///   （`caps_errors_at_typeboxes_max_errors_like_the_reference`）。
pub fn validate_theme_json(label: &str, json: &Value) -> Result<ThemeJson, ThemeError> {
    // 预检只对“字符串形态的 name”生效：非字符串 name 会走 schema 的 type 错误。
    if let Some(object) = json.as_object()
        && let Some(Value::String(name)) = object.get("name")
        && name.contains('/')
    {
        return Err(ThemeError::new(format!(
            "Invalid theme name \"{name}\": theme names cannot contain \"/\" because it is reserved for automatic light/dark theme settings."
        )));
    }

    let mut errors = Vec::new();
    validate_document(json, &mut errors);
    if errors.is_empty() {
        return theme_json_from_value(json);
    }

    // 聚合规则来自 TS 的 validateThemeJson()：`/colors` 的 required 错误改写成 missing-token
    // 列表（去重后按字母排序），其余错误按产生顺序逐行拼接；路径为空表示根对象。
    let mut missing_colors: Vec<String> = Vec::new();
    let mut other_errors: Vec<String> = Vec::new();
    for error in &errors {
        if error.keyword == "required" && error.instance_path == "/colors" {
            for property in &error.required_properties {
                if !missing_colors.contains(property) {
                    missing_colors.push(property.clone());
                }
            }
            continue;
        }
        let path = if error.instance_path.is_empty() {
            "/"
        } else {
            error.instance_path.as_str()
        };
        other_errors.push(format!("  - {path}: {}", error.message));
    }

    // 标题行后始终保留一个换行；没有 missing-token 块时它与 "Other errors" 之间会有连续
    // 三个 `\n`，这是 TS 的逐字格式（见模块文档）。
    let mut message = format!("Invalid theme \"{label}\":\n");
    if !missing_colors.is_empty() {
        missing_colors.sort();
        message.push_str("\nMissing required color tokens:\n");
        message.push_str(
            &missing_colors
                .iter()
                .map(|color| format!("  - {color}"))
                .collect::<Vec<_>>()
                .join("\n"),
        );
        message.push_str("\n\nPlease add these colors to your theme's \"colors\" object.");
        message.push_str("\nSee the built-in themes (dark.json, light.json) for reference values.");
    }
    if !other_errors.is_empty() {
        message.push_str(&format!("\n\nOther errors:\n{}", other_errors.join("\n")));
    }
    Err(ThemeError::new(message))
}

/// 把一个主题 JSON 转成 [`ThemeJson`] 的**可表示视图**。
///
/// TS 的 `parseThemeJson()` 只是把 `JSON.parse` 的结果 cast 成 `ThemeJson`（安装校验器时
/// 还要先过 schema）；真正的解析发生在 `createTheme()`（颜色/变量）与
/// `getThemeExportColors()`（导出）里。Rust 的 `ThemeColorValue` 只能表示字符串与合法索引，
/// 所以这里跳过无法表示的条目，并把原始文档保存在 [`ThemeJson`] 的 `raw` 字段，供
/// `resolve_theme_colors_from_document()` / `resolve_theme_export_from_document()` 复刻
/// TS 的惰性解析（未知 token 的 TypeError、未引用非法变量不报错、export 整段失败等）。
///
/// 安装校验器时输入已经过严格校验，可表示视图就是完整文档。
///
/// # 边界
///
/// - 顶层不是对象时返回固定文案 `Invalid theme: expected an object with a "colors" map.`
///   （这条错误不带 `label`，与 `theme.rs` 未装校验器时的分支不同）；
/// - `vars`/`colors`/`export` 里无法表示的值被跳过，不会让整个转换失败；`name` 只保留
///   字符串形态，`appearance` 只认 `"dark"`/`"light"` 两个字面量，其他形态在视图中为
///   `None`，但原文保留在 `raw_name`/`raw_appearance`；
/// - `raw` 是完整克隆，`resolve_*` 依赖它复刻 TS 的惰性/宽松行为。
pub(crate) fn theme_json_from_value(value: &Value) -> Result<ThemeJson, ThemeError> {
    let Some(object) = value.as_object() else {
        return Err(ThemeError::new(
            "Invalid theme: expected an object with a \"colors\" map.",
        ));
    };

    // 字符串字段取字符串形态，类型化字段（appearance）只认两个合法字面量；原始 JSON 值
    // 一律另存，供未校验路径的调用方按 TS 语义读取。
    let schema = match object.get("$schema") {
        Some(Value::String(text)) => Some(text.clone()),
        _ => None,
    };
    let raw_name = object.get("name").cloned();
    let raw_appearance = object.get("appearance").cloned();
    let name = object
        .get("name")
        .and_then(Value::as_str)
        .map(str::to_string);
    let appearance = match object.get("appearance").and_then(Value::as_str) {
        Some("dark") => Some(TerminalColorScheme::Dark),
        Some("light") => Some(TerminalColorScheme::Light),
        _ => None,
    };

    // 变量表保留全部可表示条目（包括未被引用的），非字符串/非合法索引的值跳过；
    // 引用解析留到 createTheme 阶段，因此这里不能因为某个变量非法而失败。
    let mut vars = IndexMap::new();
    if let Some(Value::Object(entries)) = object.get("vars") {
        for (key, item) in entries {
            if let Some(converted) = representable_color(item) {
                vars.insert(key.clone(), converted);
            }
        }
    }

    // colors 只收已知 token（未知键在严格路径已被 schema 拒绝，未校验路径留到
    // splitThemeColors 的 TypeError）；插入顺序 = 文档顺序。
    let mut colors = IndexMap::new();
    if let Some(Value::Object(entries)) = object.get("colors") {
        for (key, item) in entries {
            let Some(token) = ThemeToken::from_str(key) else {
                continue;
            };
            if let Some(converted) = representable_color(item) {
                colors.insert(token, converted);
            }
        }
    }

    // export 视图逐字段尽力转换：某个字段不可表示只会变成 None，不影响同段的其他字段
    // （“整段失败”的语义在 resolve_theme_export_from_document()）。
    let export = match object.get("export") {
        Some(Value::Object(entries)) => {
            let field = |key: &str| entries.get(key).and_then(representable_color);
            Some(ThemeExport {
                page_bg: field("pageBg"),
                card_bg: field("cardBg"),
                info_bg: field("infoBg"),
            })
        }
        _ => None,
    };

    Ok(ThemeJson {
        schema,
        name,
        appearance,
        raw_name,
        raw_appearance,
        vars,
        colors,
        export,
        // 视图会丢信息（跳过非法值、只保留已知 token），原文是两处惰性解析的唯一依据。
        raw: value.clone(),
    })
}

/// 一个 JSON 值能否放进 [`ThemeColorValue`]（字符串原样；数字仅限合法索引）。
///
/// 与 `color_from_resolved()` 不同，这里把不可表示的值降级为 `None`（视图跳过），不报错：
/// `24.5`、`-1`、`256`、`true`、`null` 都不会进入 `ThemeJson` 视图，但原文仍在 `raw` 中，
/// 由未校验路径的解析函数决定是否报错。
fn representable_color(value: &Value) -> Option<ThemeColorValue> {
    match value {
        Value::String(text) => Some(ThemeColorValue::Str(text.clone())),
        Value::Number(number) => number
            .as_f64()
            .filter(|numeric| is_js_integer_value(*numeric) && (0.0..=255.0).contains(numeric))
            .map(|numeric| ThemeColorValue::Index(numeric as u8)),
        _ => None,
    }
}

/// `createTheme()` 的 `resolveThemeColors()` + `splitThemeColors()` + 构造器的逐 token
/// `parseColor`（不含 `withThemeTokenFallbacks()`；完整顺序见
/// [`resolve_theme_colors_for_create`]）。
///
/// 这是加载主题时真正会失败的一步（`loadThemeJson()` 本身不解析颜色）。生产路径用
/// [`resolve_theme_colors_for_create`]，本函数保留给单元测试逐阶段核对 TS 的中间步骤。
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn resolve_theme_colors_from_document(
    value: &Value,
) -> Result<IndexMap<ThemeToken, ThemeColorValue>, ThemeError> {
    let object = value.as_object();
    let colors = object.and_then(|object| object.get("colors"));
    let vars = object.and_then(|object| object.get("vars"));
    theme_json_colors(colors, vars)
}

/// TS: `getThemeExportColors()` 的 `export` 段解析（变量引用 + CSS 形态转换前的字面量）。
///
/// 任何一项无法解析都返回 `None`（TS 整个对象字面量在 `try` 里，失败返回 `{}`）。
pub(crate) fn resolve_theme_export_from_document(value: &Value) -> Option<ThemeExport> {
    let object = value.as_object()?;
    theme_json_export(object.get("export"), object.get("vars"))
}

/// JS 的 `Number.isInteger(value)`。
fn is_js_integer_value(value: f64) -> bool {
    value.is_finite() && value.fract() == 0.0
}

/// 变量引用解析的结果（JS 的 `string | number`）。
#[derive(Debug, Clone)]
enum ResolvedColor {
    /// 数字（还需要 `indexedColor()` 的范围检查）。
    Number(f64),
    /// 已经解析成字面量（`""`、`#…`、`ok(lch|hsl)(…)`）的字符串。
    Text(String),
}

/// TS: `resolveVarRefs(value, vars)`（未校验路径的原始 JSON 版本）。
///
/// 数字原样返回（范围检查在 `color_from_resolved()`）；字符串按“字面量短路 → 环检测 →
/// `in` 查找 → 递归”处理。`visited` 由调用方持有，记录本次解析链上已展开的引用名。
fn resolve_json_var_refs(
    value: &Value,
    vars: Option<&Value>,
    visited: &mut std::collections::HashSet<String>,
) -> Result<ResolvedColor, ThemeError> {
    // 数字是候选的 ANSI 索引；是不是“合法索引”要到构造器阶段才判断。
    if let Value::Number(number) = value {
        return Ok(ResolvedColor::Number(number.as_f64().unwrap_or(f64::NAN)));
    }
    let Some(text) = value.as_str() else {
        // `value.startsWith` 在 null 与非字符串上的 JS TypeError。
        return Err(ThemeError::new(match value {
            Value::Null => "Cannot read properties of null (reading 'startsWith')",
            _ => "value.startsWith is not a function",
        }));
    };
    // 空串、`#hex`、`ok(lch|hsl)(…)` 是字面量终点，不再查 vars。
    if text.is_empty() || text.starts_with('#') || ok_lch_or_hsl(text) {
        return Ok(ResolvedColor::Text(text.to_string()));
    }
    // 环检测看的是“已经展开过的引用名”：a→a、a→b→a 都会在第二次遇到重复名字时报错，
    // 消息里的名字就是重复出现的那个（见测试 resolves_variable_references）。
    if visited.contains(text) {
        return Err(ThemeError::new(format!(
            "Circular variable reference detected: {text}"
        )));
    }
    let Some(next) = json_var_lookup(vars, text)? else {
        return Err(ThemeError::new(format!(
            "Variable reference not found: {text}"
        )));
    };
    // 查到变量后才记录名字，这样自引用也能被下一轮命中。
    visited.insert(text.to_string());
    match next {
        VarLookup::Found(value) => resolve_json_var_refs(value, vars, visited),
        // `vars.length`（数组）是数字，作为颜色值时走 `indexedColor()` 的范围检查。
        VarLookup::Length(length) => Ok(ResolvedColor::Number(length as f64)),
        // 继承自 `Object.prototype`/`Array.prototype` 的函数或对象：递归一步后在
        // `value.startsWith` 上抛出 TypeError。
        VarLookup::Inherited => Err(ThemeError::new("value.startsWith is not a function")),
    }
}

/// [`json_var_lookup`] 的结果。
enum VarLookup<'a> {
    /// 自有属性（或数组索引）的取值。
    Found(&'a Value),
    /// 数组的 `length`（数字）。
    Length(usize),
    /// 只存在于原型链上的成员（函数/对象）。
    Inherited,
}

/// `text in vars` 的 JS 语义（未校验路径）。
///
/// 非对象的 `vars`（`null`/数字/布尔/字符串）上使用 `in` 会抛 TypeError；普通对象会看到
/// `Object.prototype` 的继承成员，数组还会看到 `Array.prototype` 的成员与 `length`。
fn json_var_lookup<'a>(
    vars: Option<&'a Value>,
    name: &str,
) -> Result<Option<VarLookup<'a>>, ThemeError> {
    match vars {
        // `resolveThemeColors(colors, vars = {})`：`undefined` 走**普通对象**默认值，
        // 因此 `Object.prototype` 的继承成员依然可见。
        None => Ok(is_object_prototype_key(name).then_some(VarLookup::Inherited)),
        Some(Value::Object(entries)) => {
            // 自有属性优先；命中不到才沿原型链看 `Object.prototype` 的成员。
            if let Some(value) = entries.get(name) {
                return Ok(Some(VarLookup::Found(value)));
            }
            if is_object_prototype_key(name) {
                return Ok(Some(VarLookup::Inherited));
            }
            Ok(None)
        }
        Some(Value::Array(items)) => {
            // 数组先处理 `length` 与规范索引（无前导零的十进制），再看 `Array.prototype`；
            // `"01"` 不是索引，会落到原型查找/未找到。
            if name == "length" {
                return Ok(Some(VarLookup::Length(items.len())));
            }
            if let Some(index) = js_array_index(name)
                && let Some(value) = items.get(index as usize)
            {
                return Ok(Some(VarLookup::Found(value)));
            }
            if is_array_prototype_key(name) {
                return Ok(Some(VarLookup::Inherited));
            }
            Ok(None)
        }
        // 原始值上没有原型链：`in` 直接抛 TypeError，消息里嵌入 JS 模板字符串化后的值。
        Some(other) => Err(ThemeError::new(format!(
            "Cannot use 'in' operator to search for '{name}' in {}",
            js_string_of(other)
        ))),
    }
}

/// `Object.entries(value)`：null/undefined 抛 TypeError；数组/字符串按索引展开；
/// 其他原始值没有条目。
///
/// 数组与字符串分支只为未校验路径的异常输入存在：它们的键随后都会走未知 token 检查，
/// 因而通常以 `Cannot read properties of undefined (reading 'slot')` 结束。
fn js_object_entries(value: &Value) -> Result<Vec<(String, Value)>, ThemeError> {
    match value {
        Value::Null => Err(ThemeError::new(
            "Cannot convert undefined or null to object",
        )),
        Value::Object(entries) => Ok(js_ordered_keys(entries)
            .into_iter()
            .map(|key| (key.clone(), entries[key].clone()))
            .collect()),
        Value::Array(items) => Ok(items
            .iter()
            .enumerate()
            .map(|(index, item)| (index.to_string(), item.clone()))
            .collect()),
        Value::String(text) => Ok(text
            .chars()
            .enumerate()
            .map(|(index, character)| (index.to_string(), Value::String(character.to_string())))
            .collect()),
        _ => Ok(Vec::new()),
    }
}

/// `colors` 段：先 `resolveThemeColors()`（变量引用/类型错误），再 `splitThemeColors()`
/// （未知 token 的 TypeError），最后按构造器顺序（前景 token 先、背景 token 后）校验索引。
///
/// 这里不含 `withThemeTokenFallbacks()` 的缺失检查（那是 `createTheme()` 的完整顺序的一
/// 部分，见 [`resolve_theme_colors_for_create`]）。
fn theme_json_colors(
    value: Option<&Value>,
    vars: Option<&Value>,
) -> Result<IndexMap<ThemeToken, ThemeColorValue>, ThemeError> {
    typed_theme_colors(resolved_color_entries(value, vars)?)
}

/// TS: `createTheme()` 里颜色的完整顺序：
/// `resolveThemeColors()` → `withThemeTokenFallbacks()` → `splitThemeColors()` → 构造器。
///
/// 顺序决定错误优先级：变量/类型错误先于 fallback 缺失（例如
/// `Theme token scrollbarTrack has unresolved fallback muted`），两者又都先于未知 token 的
/// `Cannot read properties of undefined (reading 'slot')` 与非法索引。
///
/// 返回按“前景 token 在前、背景 token 在后”排列的新 map；调用方 `create_theme()` 随后按
/// slot 拆分并交给 `Theme::new()`。`colors`/`vars` 缺失时分别按 TS 的空表处理；必填项检查
/// 不在这里（严格路径由 schema，未校验路径由 `parse_theme_json()` 只检查 `colors` 是否存在）。
pub(crate) fn resolve_theme_colors_for_create(
    value: &Value,
) -> Result<IndexMap<ThemeToken, ThemeColorValue>, ThemeError> {
    let object = value.as_object();
    let colors = object.and_then(|object| object.get("colors"));
    let vars = object.and_then(|object| object.get("vars"));
    let resolved = resolved_color_entries(colors, vars)?;
    check_color_fallbacks(&resolved)?;
    typed_theme_colors(resolved)
}

/// 阶段 1：TS 的 `resolveThemeColors(colors, vars)` —— 按 JS 枚举顺序解析每个值。
fn resolved_color_entries(
    value: Option<&Value>,
    vars: Option<&Value>,
) -> Result<Vec<(String, ResolvedColor)>, ThemeError> {
    let entries = match value {
        Some(value) => js_object_entries(value)?,
        // `parseThemeJson()` 保证 `"colors" in json`；直接调用时按空对象处理。
        None => Vec::new(),
    };
    let mut resolved = Vec::with_capacity(entries.len());
    // 任一条目解析失败立即返回：JS 的 map 回调抛错会中断整个 createTheme()，后面的条目
    // 不会再产生错误。每个条目使用独立的 visited 集合。
    for (key, item) in entries {
        let mut visited = std::collections::HashSet::new();
        resolved.push((key, resolve_json_var_refs(&item, vars, &mut visited)?));
    }
    Ok(resolved)
}

/// 阶段 2：TS 的 `withThemeTokenFallbacks()` 的失败检查。
///
/// 只检查 fallback 是否可解析；值的补全发生在 `Theme::new()` 的
/// `with_theme_token_fallbacks()`（两处遍历顺序相同）。
fn check_color_fallbacks(resolved: &[(String, ResolvedColor)]) -> Result<(), ThemeError> {
    let present: std::collections::HashSet<&str> =
        resolved.iter().map(|(key, _)| key.as_str()).collect();
    // 遍历顺序 = THEME_TOKENS 声明顺序，第一个缺失的 fallback 决定错误文案（与 TS 的
    // 逐项 throw 一致）。只看键是否存在：阶段 1 已保证每个键的值可解析（索引范围检查
    // 留到阶段 3 的构造器）。
    for (index, descriptor) in THEME_TOKENS.iter().enumerate() {
        let Some(fallback) = descriptor.fallback else {
            continue;
        };
        let token = ThemeToken::ALL[index];
        if present.contains(token.as_str()) || present.contains(fallback.as_str()) {
            continue;
        }
        return Err(ThemeError::new(format!(
            "Theme token {token} has unresolved fallback {fallback}"
        )));
    }
    Ok(())
}

/// 阶段 3：`splitThemeColors()`（未知 token 的 TypeError）＋构造器（索引校验）。
fn typed_theme_colors(
    resolved: Vec<(String, ResolvedColor)>,
) -> Result<IndexMap<ThemeToken, ThemeColorValue>, ThemeError> {
    let mut foregrounds = Vec::new();
    let mut backgrounds = Vec::new();
    // 第一遍按 JS 枚举顺序分类，同时做 splitThemeColors() 的未知键检查：任何一个未知
    // token 都会先于构造器的索引错误抛出（测试 create_theme_error_order_matches_the_reference）。
    for (key, value) in resolved {
        let Some(token) = ThemeToken::from_str(&key) else {
            // `THEME_TOKENS[key]` 会看到 `Object.prototype` 的继承成员：它们的 `.slot` 是
            // undefined，于是被当成前景 token（`"__proto__"` 还会因为赋值走原型 setter
            // 被丢弃）。TS 里主题照常加载；这些条目没有对应的 `ThemeToken`，Rust 无法表示，
            // 这里接受文档并跳过它们（真实 token 的解析结果不受影响）。
            if is_object_prototype_key(&key) {
                continue;
            }
            return Err(ThemeError::new(
                "Cannot read properties of undefined (reading 'slot')",
            ));
        };
        if token.is_background() {
            backgrounds.push((token, value));
        } else {
            foregrounds.push((token, value));
        }
    }

    // 第二遍才做索引校验：构造器先处理全部前景 token、再处理背景 token，非法索引的
    // 报错顺序依赖这个分组顺序。
    let mut colors = IndexMap::with_capacity(foregrounds.len() + backgrounds.len());
    for (token, value) in foregrounds.into_iter().chain(backgrounds) {
        colors.insert(token, color_from_resolved(value)?);
    }
    Ok(colors)
}

/// `ResolvedColor` → [`ThemeColorValue`]：数字走 `indexedColor()` 的逐字校验。
///
/// 与 `representable_color()` 使用同一组范围判定，但失败时返回 TS 的逐字错误而不是跳过；
/// 数字格式化走 `kiana_tui::colors::format_number`（ECMAScript 数字格式），与 TS 消息里的
/// 数字写法对齐。
fn color_from_resolved(value: ResolvedColor) -> Result<ThemeColorValue, ThemeError> {
    match value {
        ResolvedColor::Text(text) => Ok(ThemeColorValue::Str(text)),
        ResolvedColor::Number(numeric) => {
            if is_js_integer_value(numeric) && (0.0..=255.0).contains(&numeric) {
                Ok(ThemeColorValue::Index(numeric as u8))
            } else {
                Err(ThemeError::new(format!(
                    "ANSI color index must be an integer from 0 to 255: {}",
                    kiana_tui::colors::format_number(numeric)
                )))
            }
        }
    }
}

/// `export` 段。TS 的 `getThemeExportColors()` 把整个对象字面量的求值放在 `try` 里，任何
/// 一项抛错都返回 `{}`，所以这里一遇到无法解析的值就返回 `None`（等价于空对象）。
fn theme_json_export(value: Option<&Value>, vars: Option<&Value>) -> Option<ThemeExport> {
    let Some(Value::Object(entries)) = value else {
        return None;
    };
    // `getThemeExportColors()` 用 `themeJson.vars ?? {}`：null 视作空表。
    let vars = match vars {
        Some(Value::Null) | None => None,
        other => other,
    };
    let field = |key: &str| -> Option<Option<ThemeColorValue>> {
        let Some(item) = entries.get(key) else {
            // 字段缺省是合法的“未设置”，与解析失败（外层 None）区分。
            return Some(None);
        };
        let mut visited = std::collections::HashSet::new();
        match resolve_json_var_refs(item, vars, &mut visited) {
            Ok(resolved) => color_from_resolved(resolved).ok().map(Some),
            Err(_) => None,
        }
    };
    // 三个字段全部成功才返回 Some；任一字段解析/范围失败都会让整段变成 None，
    // 与 TS 把对象字面量整体放进 try 的行为一致。
    Some(ThemeExport {
        page_bg: field("pageBg")?,
        card_bg: field("cardBg")?,
        info_bg: field("infoBg")?,
    })
}

#[cfg(test)]
mod tests {
    // 这里的断言锁定冻结契约：错误文案、错误产生顺序、8 条上限，以及 JS 的
    // 键枚举/原型链语义。改动校验器或错误文本时这些测试应当失败，不要反过来放宽断言。
    use super::*;

    fn dark() -> Value {
        serde_json::from_str(crate::assets::DARK_JSON).expect("dark.json parses")
    }

    fn with(mut value: Value, edit: impl FnOnce(&mut serde_json::Map<String, Value>)) -> Value {
        if let Value::Object(object) = &mut value {
            edit(object);
        }
        value
    }

    #[test]
    fn accepts_the_builtin_document_and_optional_tokens() {
        let theme = validate_theme_json("custom", &dark()).expect("dark.json is valid");
        assert_eq!(theme.name.as_deref(), Some("dark"));
        assert_eq!(theme.appearance, Some(TerminalColorScheme::Dark));
        assert_eq!(
            theme.schema.as_deref(),
            Some("https://kiana.dev/schemas/theme.schema.json")
        );
        // `ThemeJson` 是 JSON 文档的可表示视图（TS 的 `parseThemeJson()` 只是 cast）：
        // `"violet"` 保持原样，解析发生在 createTheme。
        assert_eq!(
            theme.colors.get(&ThemeToken::Accent),
            Some(&ThemeColorValue::Str("violet".to_string()))
        );
        assert_eq!(
            theme
                .export
                .as_ref()
                .and_then(|export| export.page_bg.clone()),
            Some(ThemeColorValue::Str("okhsl(262 14% 16%)".to_string()))
        );
        // 整数值的浮点数算 integer。
        let with_float = with(dark(), |object| {
            let colors = object.get_mut("colors").expect("colors");
            if let Value::Object(colors) = colors {
                colors.insert("accent".to_string(), serde_json::json!(24.0));
            }
        });
        assert!(validate_theme_json("custom", &with_float).is_ok());
        // 未知的可选 token 名（拼写错误）被拒绝。
        let typo = with(dark(), |object| {
            let colors = object.get_mut("colors").expect("colors");
            if let Value::Object(colors) = colors {
                colors.insert("scrollbarThmb".to_string(), serde_json::json!(""));
            }
        });
        let message = validate_theme_json("invalid", &typo)
            .expect_err("typo is rejected")
            .to_string();
        assert_eq!(
            message,
            "Invalid theme \"invalid\":\n\n\nOther errors:\n  - /colors/scrollbarThmb: schema is false\n  - /colors: must not have additional properties"
        );
    }

    #[test]
    fn rejects_names_with_slashes_before_schema_validation() {
        let document = with(dark(), |object| {
            object.insert("name".to_string(), serde_json::json!("invalid/name"));
        });
        let message = validate_theme_json("invalid", &document)
            .expect_err("slash names are rejected")
            .to_string();
        assert_eq!(
            message,
            "Invalid theme name \"invalid/name\": theme names cannot contain \"/\" because it is reserved for automatic light/dark theme settings."
        );
    }

    #[test]
    fn aggregates_missing_tokens_in_alphabetical_order() {
        let document = with(dark(), |object| {
            object.insert("colors".to_string(), serde_json::json!({}));
        });
        let message = validate_theme_json("invalid", &document)
            .expect_err("empty colors is rejected")
            .to_string();
        let expected = "Invalid theme \"invalid\":\n\nMissing required color tokens:\n  - accent\n  - bashMode\n  - border\n  - borderAccent\n  - borderMuted\n  - customMessageBg\n  - customMessageLabel\n  - customMessageText\n  - dim\n  - error\n  - mdCode\n  - mdCodeBlock\n  - mdCodeBlockBorder\n  - mdHeading\n  - mdHr\n  - mdLink\n  - mdLinkUrl\n  - mdListBullet\n  - mdQuote\n  - mdQuoteBorder\n  - muted\n  - selectedBg\n  - success\n  - syntaxComment\n  - syntaxFunction\n  - syntaxKeyword\n  - syntaxNumber\n  - syntaxOperator\n  - syntaxPunctuation\n  - syntaxString\n  - syntaxType\n  - syntaxVariable\n  - text\n  - thinkingHigh\n  - thinkingLow\n  - thinkingMedium\n  - thinkingMinimal\n  - thinkingOff\n  - thinkingText\n  - thinkingXhigh\n  - toolDiffAdded\n  - toolDiffContext\n  - toolDiffRemoved\n  - toolErrorBg\n  - toolOutput\n  - toolPendingBg\n  - toolSuccessBg\n  - toolTitle\n  - userMessageBg\n  - userMessageText\n  - warning\n\nPlease add these colors to your theme's \"colors\" object.\nSee the built-in themes (dark.json, light.json) for reference values.";
        assert_eq!(message, expected);
    }

    #[test]
    fn reports_missing_colors_and_other_errors_together() {
        let document = with(dark(), |object| {
            object.insert("colors".to_string(), serde_json::json!({}));
            object.insert("unexpected".to_string(), serde_json::json!(true));
        });
        let message = validate_theme_json("invalid", &document)
            .expect_err("invalid document")
            .to_string();
        assert!(message.ends_with(
            "\n\nPlease add these colors to your theme's \"colors\" object.\nSee the built-in themes (dark.json, light.json) for reference values.\n\nOther errors:\n  - /unexpected: schema is false\n  - /: must not have additional properties"
        ));
    }

    #[test]
    fn reports_union_and_range_errors_in_typebox_order() {
        let case = |value: Value| {
            let document = with(dark(), |object| {
                let colors = object.get_mut("colors").expect("colors");
                if let Value::Object(colors) = colors {
                    colors.insert("accent".to_string(), value.clone());
                }
            });
            validate_theme_json("invalid", &document)
                .expect_err("rejected")
                .to_string()
        };
        assert_eq!(
            case(serde_json::json!(true)),
            "Invalid theme \"invalid\":\n\n\nOther errors:\n  - /colors/accent: must be string\n  - /colors/accent: must be integer\n  - /colors/accent: must match a schema in anyOf"
        );
        assert_eq!(
            case(serde_json::json!(-2.5)),
            "Invalid theme \"invalid\":\n\n\nOther errors:\n  - /colors/accent: must be string\n  - /colors/accent: must be integer\n  - /colors/accent: must be >= 0\n  - /colors/accent: must match a schema in anyOf"
        );
        assert_eq!(
            case(serde_json::json!(256)),
            "Invalid theme \"invalid\":\n\n\nOther errors:\n  - /colors/accent: must be string\n  - /colors/accent: must be <= 255\n  - /colors/accent: must match a schema in anyOf"
        );
        assert_eq!(
            case(serde_json::json!(24.5)),
            "Invalid theme \"invalid\":\n\n\nOther errors:\n  - /colors/accent: must be string\n  - /colors/accent: must be integer\n  - /colors/accent: must match a schema in anyOf"
        );
    }

    #[test]
    fn reports_root_and_export_errors() {
        let message = validate_theme_json("invalid", &serde_json::json!([]))
            .expect_err("array is not a theme")
            .to_string();
        assert_eq!(
            message,
            "Invalid theme \"invalid\":\n\n\nOther errors:\n  - /: must be object"
        );

        let message = validate_theme_json("invalid", &serde_json::json!({}))
            .expect_err("empty object")
            .to_string();
        assert_eq!(
            message,
            "Invalid theme \"invalid\":\n\n\nOther errors:\n  - /: must have required properties name, colors"
        );

        let document = with(dark(), |object| {
            object.insert(
                "export".to_string(),
                serde_json::json!({ "unexpected": "" }),
            );
        });
        let message = validate_theme_json("invalid", &document)
            .expect_err("export typo")
            .to_string();
        assert_eq!(
            message,
            "Invalid theme \"invalid\":\n\n\nOther errors:\n  - /export/unexpected: schema is false\n  - /export: must not have additional properties"
        );

        let document = with(dark(), |object| {
            object.insert("appearance".to_string(), serde_json::json!("blue"));
        });
        let message = validate_theme_json("invalid", &document)
            .expect_err("bad appearance")
            .to_string();
        assert_eq!(
            message,
            "Invalid theme \"invalid\":\n\n\nOther errors:\n  - /appearance: must be equal to constant\n  - /appearance: must be equal to constant\n  - /appearance: must match a schema in anyOf"
        );

        let document = with(dark(), |object| {
            object.insert("name".to_string(), serde_json::json!(""));
        });
        let message = validate_theme_json("invalid", &document)
            .expect_err("empty name")
            .to_string();
        assert_eq!(
            message,
            "Invalid theme \"invalid\":\n\n\nOther errors:\n  - /name: must match pattern \"^[^/]+$\""
        );

        let document = with(dark(), |object| {
            object.insert("vars".to_string(), serde_json::json!([]));
        });
        let message = validate_theme_json("invalid", &document)
            .expect_err("vars array")
            .to_string();
        assert_eq!(
            message,
            "Invalid theme \"invalid\":\n\n\nOther errors:\n  - /vars: must be object"
        );
    }

    #[test]
    fn caps_errors_at_typeboxes_max_errors_like_the_reference() {
        // 所有 colors 缺失 + 5 个非法 vars：`/` 的 required 错误排第一，之后是 vars，
        // 8 条后截断，缺失 token 块不出现（`/colors` 的 required 根本没被保留）。
        let document = with(dark(), |object| {
            object.remove("colors");
            object.insert(
                "vars".to_string(),
                serde_json::json!({ "a": true, "b": null, "c": [1], "d": {}, "e": 1.5 }),
            );
        });
        let message = validate_theme_json("invalid", &document)
            .expect_err("invalid")
            .to_string();
        assert_eq!(
            message,
            "Invalid theme \"invalid\":\n\n\nOther errors:\n  - /: must have required properties colors\n  - /vars/a: must be string\n  - /vars/a: must be integer\n  - /vars/a: must match a schema in anyOf\n  - /vars/b: must be string\n  - /vars/b: must be integer\n  - /vars/b: must match a schema in anyOf\n  - /vars/c: must be string"
        );

        // 大量非法颜色：只保留前 8 条（按 schema 声明顺序：accent、border、borderAccent）。
        let document = with(dark(), |object| {
            let colors = object.get_mut("colors").expect("colors");
            if let Value::Object(colors) = colors {
                for (index, key) in colors
                    .keys()
                    .cloned()
                    .collect::<Vec<_>>()
                    .iter()
                    .enumerate()
                {
                    if index < 20 {
                        colors.insert(key.clone(), serde_json::json!(true));
                    }
                }
            }
        });
        let message = validate_theme_json("invalid", &document)
            .expect_err("invalid")
            .to_string();
        assert_eq!(
            message,
            "Invalid theme \"invalid\":\n\n\nOther errors:\n  - /colors/accent: must be string\n  - /colors/accent: must be integer\n  - /colors/accent: must match a schema in anyOf\n  - /colors/border: must be string\n  - /colors/border: must be integer\n  - /colors/border: must match a schema in anyOf\n  - /colors/borderAccent: must be string\n  - /colors/borderAccent: must be integer"
        );

        // 根级错误先占满容量：缺失 token 块不出现。
        let message = validate_theme_json(
            "invalid",
            &serde_json::json!({
                "name": 5,
                "appearance": true,
                "vars": { "a": true, "b": null },
                "colors": {},
                "export": { "pageBg": true, "extra": 1 },
                "unexpected": 2,
            }),
        )
        .expect_err("invalid")
        .to_string();
        assert_eq!(
            message,
            "Invalid theme \"invalid\":\n\n\nOther errors:\n  - /unexpected: schema is false\n  - /: must not have additional properties\n  - /name: must be string\n  - /appearance: must be string\n  - /appearance: must be equal to constant\n  - /appearance: must be string\n  - /appearance: must be equal to constant\n  - /appearance: must match a schema in anyOf"
        );

        // required(/colors) 落在前 8 条里时，缺失 token 块正常出现，其余错误保留 7 条。
        let document = with(dark(), |object| {
            let colors = object.get_mut("colors").expect("colors");
            if let Value::Object(colors) = colors {
                colors.remove("borderMuted");
                for key in ["accent", "border", "borderAccent"] {
                    colors.insert(key.to_string(), serde_json::json!(true));
                }
            }
        });
        let message = validate_theme_json("invalid", &document)
            .expect_err("invalid")
            .to_string();
        assert_eq!(
            message,
            "Invalid theme \"invalid\":\n\nMissing required color tokens:\n  - borderMuted\n\nPlease add these colors to your theme's \"colors\" object.\nSee the built-in themes (dark.json, light.json) for reference values.\n\nOther errors:\n  - /colors/accent: must be string\n  - /colors/accent: must be integer\n  - /colors/accent: must match a schema in anyOf\n  - /colors/border: must be string\n  - /colors/border: must be integer\n  - /colors/border: must match a schema in anyOf\n  - /colors/borderAccent: must be string"
        );
    }

    #[test]
    fn reports_literal_union_type_errors_for_non_string_appearance() {
        for value in [
            serde_json::json!(5),
            serde_json::Value::Null,
            serde_json::json!(true),
            serde_json::json!(["dark"]),
            serde_json::json!({}),
        ] {
            let document = with(dark(), |object| {
                object.insert("appearance".to_string(), value.clone());
            });
            assert_eq!(
                validate_theme_json("invalid", &document)
                    .expect_err("rejected")
                    .to_string(),
                "Invalid theme \"invalid\":\n\n\nOther errors:\n  - /appearance: must be string\n  - /appearance: must be equal to constant\n  - /appearance: must be string\n  - /appearance: must be equal to constant\n  - /appearance: must match a schema in anyOf"
            );
        }
    }

    #[test]
    fn resolves_variable_references() {
        let mut vars = IndexMap::new();
        vars.insert(
            "deep".to_string(),
            ThemeColorValue::Str("#123456".to_string()),
        );
        vars.insert(
            "alias".to_string(),
            ThemeColorValue::Str("deep".to_string()),
        );
        vars.insert(
            "cycleA".to_string(),
            ThemeColorValue::Str("cycleB".to_string()),
        );
        vars.insert(
            "cycleB".to_string(),
            ThemeColorValue::Str("cycleA".to_string()),
        );

        let resolve = |value: &str| {
            let mut visited = std::collections::HashSet::new();
            resolve_var_refs(
                &ThemeColorValue::Str(value.to_string()),
                &vars,
                &mut visited,
            )
        };
        assert_eq!(
            resolve("alias").expect("alias resolves"),
            ThemeColorValue::Str("#123456".to_string())
        );
        assert_eq!(
            resolve("#abc").expect("literal hex"),
            ThemeColorValue::Str("#abc".to_string())
        );
        assert_eq!(
            resolve("okhsl(1 2% 3%)").expect("literal okhsl"),
            ThemeColorValue::Str("okhsl(1 2% 3%)".to_string())
        );
        assert_eq!(
            resolve("").expect("empty literal"),
            ThemeColorValue::Str(String::new())
        );
        assert_eq!(
            resolve("missing").expect_err("missing").to_string(),
            "Variable reference not found: missing"
        );
        assert_eq!(
            resolve("cycleA").expect_err("cycle").to_string(),
            "Circular variable reference detected: cycleA"
        );
        // 数字原样返回。
        let mut visited = std::collections::HashSet::new();
        assert_eq!(
            resolve_var_refs(&ThemeColorValue::Index(4), &vars, &mut visited)
                .expect("index literal"),
            ThemeColorValue::Index(4)
        );
    }

    #[test]
    fn converts_documents_into_typed_theme_json() {
        let value = serde_json::json!({
            "name": "demo",
            "colors": { "accent": "#fff", "selectedBg": 3 },
            "vars": { "brand": 12 },
            "export": { "pageBg": "", "infoBg": 5 },
        });
        let theme = theme_json_from_value(&value).expect("converts");
        assert_eq!(theme.name.as_deref(), Some("demo"));
        assert_eq!(
            theme.colors.get(&ThemeToken::Accent),
            Some(&ThemeColorValue::Str("#fff".to_string()))
        );
        assert_eq!(
            theme.colors.get(&ThemeToken::SelectedBg),
            Some(&ThemeColorValue::Index(3))
        );
        assert_eq!(theme.colors.len(), 2);
        assert_eq!(theme.vars.get("brand"), Some(&ThemeColorValue::Index(12)));
        let export = theme.export.expect("export");
        assert_eq!(export.page_bg, Some(ThemeColorValue::Str(String::new())));
        assert_eq!(export.card_bg, None);
        assert_eq!(export.info_bg, Some(ThemeColorValue::Index(5)));

        // `theme_json_from_value()` 只给出可表示视图：无法表示的值被跳过，真正的解析在
        // `resolve_theme_colors_from_document()`（对应 TS 的 `createTheme()`）。
        let bad = serde_json::json!({ "colors": { "accent": 24.5 } });
        assert_eq!(
            theme_json_from_value(&bad).expect("parses").colors.len(),
            0,
            "fractional indices are not representable"
        );
        assert_eq!(
            resolve_theme_colors_from_document(&bad).expect_err("fractional index"),
            ThemeError::new("ANSI color index must be an integer from 0 to 255: 24.5")
        );
        let bad = serde_json::json!({ "colors": { "accent": null } });
        assert_eq!(
            resolve_theme_colors_from_document(&bad).expect_err("invalid value"),
            ThemeError::new("Cannot read properties of null (reading 'startsWith')")
        );
        let bad = serde_json::json!({ "colors": { "accent": true } });
        assert_eq!(
            resolve_theme_colors_from_document(&bad).expect_err("non-string value"),
            ThemeError::new("value.startsWith is not a function")
        );
        // 未知的 colors 键在 splitThemeColors() 里抛 TypeError（未校验路径）；变量解析先于
        // 键检查执行（所以值必须是字面量才能到达 slot 检查）。
        let bad = serde_json::json!({ "colors": { "unknownToken": "#fff" } });
        assert_eq!(
            resolve_theme_colors_from_document(&bad).expect_err("unknown token"),
            ThemeError::new("Cannot read properties of undefined (reading 'slot')")
        );
        // 变量惰性解析：未被引用的非法变量不影响加载，被引用时才报 TS 的错误。
        let lazy = serde_json::json!({
            "colors": { "accent": "#fff" },
            "vars": { "unused": null, "used": null },
        });
        assert!(resolve_theme_colors_from_document(&lazy).is_ok());
        let referenced = serde_json::json!({
            "colors": { "accent": "used" },
            "vars": { "used": null },
        });
        assert_eq!(
            resolve_theme_colors_from_document(&referenced).expect_err("referenced invalid var"),
            ThemeError::new("Cannot read properties of null (reading 'startsWith')")
        );
        // 非对象的 vars 上使用 `in` 的 TypeError。
        let bad_vars = serde_json::json!({
            "colors": { "accent": "violet" },
            "vars": null,
        });
        assert_eq!(
            resolve_theme_colors_from_document(&bad_vars).expect_err("null vars"),
            ThemeError::new("Cannot use 'in' operator to search for 'violet' in null")
        );
        // `colors` 为 null：`Object.entries(null)`。
        let null_colors = serde_json::json!({ "colors": null });
        assert_eq!(
            resolve_theme_colors_from_document(&null_colors).expect_err("null colors"),
            ThemeError::new("Cannot convert undefined or null to object")
        );
        // export 段任何一项无法解析 → 整段不可用（TS 的 catch 返回 {}）。
        let bad_export = serde_json::json!({
            "colors": { "accent": "#fff" },
            "export": { "pageBg": "missing-var", "cardBg": "#112233" },
        });
        assert_eq!(resolve_theme_export_from_document(&bad_export), None);
        // okhsl 字面量原样保留（解析失败只发生在 getThemeExportColors 的 CSS 转换里）。
        let raw_okhsl = serde_json::json!({
            "colors": { "accent": "#fff" },
            "export": { "pageBg": "okhsl(bad)", "cardBg": "#112233" },
        });
        let export = resolve_theme_export_from_document(&raw_okhsl).expect("export resolves");
        assert_eq!(
            export.page_bg,
            Some(ThemeColorValue::Str("okhsl(bad)".to_string()))
        );
        // 变量引用会解析成字面量（递归）。
        let chained = serde_json::json!({
            "colors": { "accent": "alias" },
            "vars": { "alias": "deep", "deep": "#123456" },
            "export": { "pageBg": "alias" },
        });
        assert_eq!(
            resolve_theme_colors_from_document(&chained)
                .expect("resolves")
                .get(&ThemeToken::Accent),
            Some(&ThemeColorValue::Str("#123456".to_string()))
        );
        assert_eq!(
            resolve_theme_export_from_document(&chained)
                .expect("resolves")
                .page_bg,
            Some(ThemeColorValue::Str("#123456".to_string()))
        );
    }

    /// 回归：typebox 按 `Object.keys()` 枚举实例，数组索引形式的键先按数值升序。
    #[test]
    fn validator_reports_keys_in_javascript_object_order() {
        let document = with(dark(), |object| {
            object.insert("zzz".to_string(), serde_json::json!(1));
            object.insert("10".to_string(), serde_json::json!(2));
            object.insert("2".to_string(), serde_json::json!(3));
        });
        assert_eq!(
            validate_theme_json("custom", &document)
                .expect_err("additional properties")
                .to_string(),
            "Invalid theme \"custom\":\n\n\nOther errors:\n  - /2: schema is false\n  - /10: schema is false\n  - /zzz: schema is false\n  - /: must not have additional properties"
        );

        let document = with(dark(), |object| {
            let colors = object.get_mut("colors").expect("colors");
            if let Value::Object(colors) = colors {
                colors.insert("zzz".to_string(), serde_json::json!("#fff"));
                colors.insert("10".to_string(), serde_json::json!("#fff"));
                colors.insert("2".to_string(), serde_json::json!("#fff"));
            }
        });
        assert_eq!(
            validate_theme_json("custom", &document)
                .expect_err("additional colors")
                .to_string(),
            "Invalid theme \"custom\":\n\n\nOther errors:\n  - /colors/2: schema is false\n  - /colors/10: schema is false\n  - /colors/zzz: schema is false\n  - /colors: must not have additional properties"
        );

        let document = with(dark(), |object| {
            let export = object.get_mut("export").expect("export");
            if let Value::Object(export) = export {
                export.insert("zzz".to_string(), serde_json::json!("#fff"));
                export.insert("10".to_string(), serde_json::json!("#fff"));
                export.insert("2".to_string(), serde_json::json!("#fff"));
            }
        });
        assert_eq!(
            validate_theme_json("custom", &document)
                .expect_err("additional export")
                .to_string(),
            "Invalid theme \"custom\":\n\n\nOther errors:\n  - /export/2: schema is false\n  - /export/10: schema is false\n  - /export/zzz: schema is false\n  - /export: must not have additional properties"
        );

        // `"02"` 不是规范的数组索引：排在索引键之后、按插入顺序；错误在第 8 条截断。
        let document = with(dark(), |object| {
            let vars = object.get_mut("vars").expect("vars");
            if let Value::Object(vars) = vars {
                vars.insert("b".to_string(), Value::Null);
                vars.insert("1".to_string(), Value::Null);
                vars.insert("02".to_string(), Value::Null);
                vars.insert("2".to_string(), Value::Null);
            }
        });
        assert_eq!(
            validate_theme_json("custom", &document)
                .expect_err("invalid vars")
                .to_string(),
            "Invalid theme \"custom\":\n\n\nOther errors:\n  - /vars/1: must be string\n  - /vars/1: must be integer\n  - /vars/1: must match a schema in anyOf\n  - /vars/2: must be string\n  - /vars/2: must be integer\n  - /vars/2: must match a schema in anyOf\n  - /vars/b: must be string\n  - /vars/b: must be integer"
        );
    }

    /// 回归：`value in vars` 的 JS 语义（原型成员、数组 `length`、规范索引）。
    #[test]
    fn variable_lookup_uses_javascript_in_semantics() {
        let accent = |document: &Value| {
            resolve_theme_colors_from_document(document)
                .expect("resolves")
                .get(&ThemeToken::Accent)
                .cloned()
        };

        // 只存在于 `Object.prototype` 的成员：递归后在 `value.startsWith` 上抛 TypeError。
        for name in [
            "constructor",
            "toString",
            "hasOwnProperty",
            "valueOf",
            "__proto__",
            "isPrototypeOf",
        ] {
            let document = serde_json::json!({ "colors": { "accent": name } });
            assert_eq!(
                resolve_theme_colors_from_document(&document),
                Err(ThemeError::new("value.startsWith is not a function")),
                "name: {name}"
            );
        }

        // 数组 `vars`：`length` 是数字（走 indexedColor），索引按规范数字字符串查找。
        let document = serde_json::json!({
            "colors": { "accent": "length" },
            "vars": ["#112233", "#445566"],
        });
        assert_eq!(accent(&document), Some(ThemeColorValue::Index(2)));
        let document = serde_json::json!({ "colors": { "accent": "length" }, "vars": [] });
        assert_eq!(accent(&document), Some(ThemeColorValue::Index(0)));
        let document = serde_json::json!({
            "colors": { "accent": "1" },
            "vars": ["#112233", "#445566"],
        });
        assert_eq!(
            accent(&document),
            Some(ThemeColorValue::Str("#445566".to_string()))
        );
        // 前导零不是数组索引（`"01" in []` 为 false）。
        let document = serde_json::json!({
            "colors": { "accent": "01" },
            "vars": ["#112233", "#445566"],
        });
        assert_eq!(
            resolve_theme_colors_from_document(&document),
            Err(ThemeError::new("Variable reference not found: 01"))
        );
        // `Array.prototype` 的继承成员。
        for name in ["push", "toString", "constructor", "__proto__", "map"] {
            let document = serde_json::json!({
                "colors": { "accent": name },
                "vars": ["#112233"],
            });
            assert_eq!(
                resolve_theme_colors_from_document(&document),
                Err(ThemeError::new("value.startsWith is not a function")),
                "name: {name}"
            );
        }

        // 自有键遮蔽原型成员。
        let document = serde_json::json!({
            "colors": { "accent": "constructor" },
            "vars": { "constructor": "#112233" },
        });
        assert_eq!(
            accent(&document),
            Some(ThemeColorValue::Str("#112233".to_string()))
        );
    }

    /// 回归：`createTheme()` 的错误优先级锁定为下面这条链
    /// （`resolveThemeColors` → `withThemeTokenFallbacks` → `splitThemeColors` → `parseColor`）。
    #[test]
    fn create_theme_error_order_matches_the_reference() {
        // fallback 缺失先于未知 token 的 slot TypeError。
        let document = serde_json::json!({ "colors": { "unknownToken": "#fff" } });
        assert_eq!(
            resolve_theme_colors_for_create(&document),
            Err(ThemeError::new(
                "Theme token scrollbarTrack has unresolved fallback muted"
            ))
        );
        // 变量解析错误又先于 fallback 检查。
        let document = serde_json::json!({ "colors": { "accent": "missing" } });
        assert_eq!(
            resolve_theme_colors_for_create(&document),
            Err(ThemeError::new("Variable reference not found: missing"))
        );
        // fallback 都能解析后，才轮到未知 token。
        let document = serde_json::json!({
            "colors": {
                "unknownToken": "#fff",
                "muted": "#fff",
                "text": "",
                "selectedBg": "#000",
                "thinkingXhigh": "#fff",
            },
        });
        assert_eq!(
            resolve_theme_colors_for_create(&document),
            Err(ThemeError::new(
                "Cannot read properties of undefined (reading 'slot')"
            ))
        );
        // 未知 token 先于非法索引（split 在构造器之前）。
        let document = serde_json::json!({
            "colors": {
                "accent": 24.5,
                "unknownToken": "#fff",
                "muted": "#fff",
                "text": "",
                "selectedBg": "#000",
                "thinkingXhigh": "#fff",
            },
        });
        assert_eq!(
            resolve_theme_colors_for_create(&document),
            Err(ThemeError::new(
                "Cannot read properties of undefined (reading 'slot')"
            ))
        );
        // 没有未知 token 时才是构造器的索引错误。
        let document = serde_json::json!({
            "colors": { "accent": 24.5, "muted": "#fff", "text": "", "selectedBg": "#000", "thinkingXhigh": "#fff" },
        });
        assert_eq!(
            resolve_theme_colors_for_create(&document),
            Err(ThemeError::new(
                "ANSI color index must be an integer from 0 to 255: 24.5"
            ))
        );
    }

    /// 回归：未校验路径下，`colors` 里以 `Object.prototype` 成员命名的键被 JS 当成前景
    /// token（`__proto__` 则因赋值走原型 setter 被丢弃）；Rust 接受文档并跳过这些不可表示
    /// 的条目，真正的未知键仍然抛 TypeError。
    #[test]
    fn unvalidated_colors_accept_prototype_named_keys() {
        for key in ["toString", "constructor", "valueOf", "hasOwnProperty"] {
            let mut colors = serde_json::Map::new();
            colors.insert("accent".to_string(), serde_json::json!("#ffffff"));
            colors.insert(key.to_string(), serde_json::json!("#112233"));
            let document = Value::Object({
                let mut object = serde_json::Map::new();
                object.insert("colors".to_string(), Value::Object(colors));
                object
            });
            let resolved =
                resolve_theme_colors_from_document(&document).expect("prototype key accepted");
            assert_eq!(
                resolved.get(&ThemeToken::Accent),
                Some(&ThemeColorValue::Str("#ffffff".to_string())),
                "key: {key}"
            );
            assert_eq!(resolved.len(), 1, "key: {key}");
        }

        let document = serde_json::json!({ "colors": { "notAToken": "#112233" } });
        assert_eq!(
            resolve_theme_colors_from_document(&document),
            Err(ThemeError::new(
                "Cannot read properties of undefined (reading 'slot')"
            ))
        );
    }

    /// 回归：有类型路径（`resolveThemeColors`）同样看到 `Object.prototype` 成员。
    #[test]
    fn typed_var_resolution_uses_object_prototype_members() {
        let colors = IndexMap::from([(
            ThemeToken::Accent,
            ThemeColorValue::Str("constructor".to_string()),
        )]);
        let vars = IndexMap::new();
        assert_eq!(
            resolve_theme_colors(&colors, &vars),
            Err(ThemeError::new("value.startsWith is not a function"))
        );

        let vars = IndexMap::from([(
            "constructor".to_string(),
            ThemeColorValue::Str("#112233".to_string()),
        )]);
        assert_eq!(
            resolve_theme_colors(&colors, &vars)
                .expect("own vars shadow the prototype")
                .get(&ThemeToken::Accent),
            Some(&ThemeColorValue::Str("#112233".to_string()))
        );
    }

    /// 回归（gate 评审 7）：JS 的非 unicode `/i` 只折叠 ASCII。`/^ok(lch|hsl)\(/i` 不认
    /// `okhſl(`（U+017F LONG S），`regex` 的 Unicode `(?i)` 会认——这里用 ASCII 比较。
    #[test]
    fn ok_lch_or_hsl_is_ascii_case_insensitive_only() {
        for literal in [
            "oklch(50% 0.1 250)",
            "OKLCH(50% 0.1 250)",
            "OkHsL(50 0.5 250)",
            "okLCH(",
        ] {
            assert!(ok_lch_or_hsl(literal), "{literal:?}");
        }
        assert!(!ok_lch_or_hsl("oklch"));
        assert!(!ok_lch_or_hsl("okhsl 50"));
        // Node 26.12.1：`/s/i.test("\u{17F}") === false`；Kelvin 符号同理不折叠。
        let long_s = "okh\u{17f}l(50% 50% 50%)";
        assert!(!ok_lch_or_hsl(long_s));
        assert!(!ok_lch_or_hsl("o\u{212a}hsl(50% 50% 50%)"));

        // `resolveVarRefs()` 把 `okhſl(...)` 当变量引用：有同名 vars 条目就解析成它的值。
        let vars = IndexMap::from([(
            long_s.to_string(),
            ThemeColorValue::Str("#123456".to_string()),
        )]);
        assert_eq!(
            resolve_var_refs(
                &ThemeColorValue::Str(long_s.to_string()),
                &vars,
                &mut std::collections::HashSet::new()
            ),
            Ok(ThemeColorValue::Str("#123456".to_string()))
        );
        assert_eq!(
            resolve_var_refs(
                &ThemeColorValue::Str(long_s.to_string()),
                &IndexMap::new(),
                &mut std::collections::HashSet::new()
            ),
            Err(ThemeError::new(format!(
                "Variable reference not found: {long_s}"
            )))
        );

        // 端到端：schema 通过 + createTheme 的颜色解析得到变量值。
        let document = with(dark(), |object| {
            if let Some(Value::Object(vars)) = object.get_mut("vars") {
                vars.insert(long_s.to_string(), serde_json::json!("#123456"));
            }
            if let Some(Value::Object(colors)) = object.get_mut("colors") {
                colors.insert("accent".to_string(), serde_json::json!(long_s));
            }
        });
        let theme = validate_theme_json("custom", &document).expect("schema-valid");
        assert_eq!(
            resolve_theme_colors_for_create(&theme.raw)
                .expect("resolves the variable")
                .get(&ThemeToken::Accent),
            Some(&ThemeColorValue::Str("#123456".to_string()))
        );
    }

    /// 回归（gate 评审 8）：typebox 的 `Type.Record(Type.String(), …)` 是
    /// `patternProperties: {"^.*$": …}`；JS 的 `.` 不匹配行终止符，因此含 `\n`/`\r`/
    /// U+2028/U+2029 的 vars 键完全不参与校验。
    #[test]
    fn vars_keys_with_line_terminators_are_not_validated() {
        let document_with_key = |key: &str| {
            with(dark(), |object| {
                object.insert(
                    "vars".to_string(),
                    Value::Object(serde_json::Map::from_iter([(
                        key.to_string(),
                        serde_json::json!(true),
                    )])),
                );
            })
        };
        for key in ["a\rb", "a\nb", "a\u{2028}b", "a\u{2029}b"] {
            assert!(
                validate_theme_json("ok", &document_with_key(key)).is_ok(),
                "key: {key:?}"
            );
        }
        // 不含行终止符的键照常校验（bool 既不是 string 也不是 integer）。
        let message = validate_theme_json("invalid", &document_with_key("ab"))
            .expect_err("validated")
            .to_string();
        assert_eq!(
            message,
            "Invalid theme \"invalid\":\n\n\nOther errors:\n  - /vars/ab: must be string\n  - /vars/ab: must be integer\n  - /vars/ab: must match a schema in anyOf"
        );
    }
}
