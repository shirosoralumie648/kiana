//! autocomplete.ts（858 行）：`CombinedAutocompleteProvider`。
//!
//! TS 源：`packages/tui/src/autocomplete.ts`。
//!
//! 一个 provider 同时处理三类补全，**顺序即语义**（`getSuggestions` 先 `@`、再斜杠命令、
//! 最后路径）：
//!
//! 1. `@` 开头的 token → 用 `fd` 做模糊文件搜索（`walkDirectoryWithFd`）；
//! 2. `/` 开头的行首文本 → 斜杠命令名 / 命令参数补全；
//! 3. 其它 → 同步 `readdir` 的路径补全。
//!
//! ## 与 JS 对齐的细节
//!
//! - **UTF-16 索引**：`cursorCol`/`prefix.length`/`item.value.length` 全部是 UTF-16 码元数
//!   （ARCHITECTURE.md §4.6），因此 `utf16_len` / `slice_prefix` / `slice_suffix` 是
//!   本模块所有切片与长度计算的基础。
//! - **`node:path` posix 语义**：`dirname`/`basename`/`join`/`normalize` 在本模块内按 Node
//!   的实现逐行重写（`path_dirname` 等）。`kiana-durable::env::path` 的唯一实现要到阶段 1 之后
//!   才可用，而 `kiana-tui` 是 L1 crate，不能反向依赖；`std::path` 的行为与 Node 不同
//!   （例如 `dirname("a/")`、`join("/a", "/b")`），不能直接替换。
//! - **`localeCompare`**：见 [`locale_compare`]（ICU4X collation，locale 从 `LC_ALL`/
//!   `LC_MESSAGES`/`LANG` 解析；`C`/`POSIX` → `en-US`，**空值** → `und`，都未设置 → `en-US`；
//!   ARCHITECTURE.md §8 第 27 条）。
//! - **`AbortSignal` → `CancellationToken`**（ARCHITECTURE.md §4.2）：每个 `await` 之后都要
//!   重新检查取消状态；`fd` 子进程被取消时以 SIGKILL 终止（`kill_on_drop`）。
//!
//! ## 在 crate 中的位置
//!
//! - 上游：`crate::components::editor::Editor` 通过 [`AutocompleteProvider`] trait 调用
//!   `get_suggestions` / `apply_completion` / `should_trigger_file_completion`。请求在飞
//!   期间 Editor 会把 provider 从自己的槽位里取出；取消时用令牌通知，应用选中项时则
//!   直接丢弃该 future 来取回 provider。
//! - 下游：`fd` 子进程（`tokio::process`）、同步 `std::fs::read_dir`、ICU4X collator。
//! - 构造方：`kiana-interactive` 的 interactive_mode 用斜杠命令表、会话 cwd 与 `fd` 路径
//!   构造 [`CombinedAutocompleteProvider`]；`kiana-extensions` 把 JS provider 包装成同一
//!   trait（本模块不感知 JS）。
//!
//! ## 一次 getSuggestions 的执行流程
//!
//! 1. 用 `cursor_col`（UTF-16 列）截取当前行光标前的文本；
//! 2. `@` token：解析出 `rawPrefix` 后走 `fd` 模糊搜索，结果为空 → `None`；
//! 3. 斜杠命令：只在 `force == false` 且 `trimStart` 后以 `/` 开头时处理；有空格走参数
//!    补全（调用命令自带的异步闭包），没有空格走命令名模糊匹配；`force == true` 时
//!    整段跳过；
//! 4. 其它情况：抽取路径前缀（`force` 时无条件抽取），同步 `readdir` 补全；
//! 5. 任一分支拿到空结果都返回 `None`，而不是空建议集。
//!
//! ## 注意事项
//!
//! - 所有长度与索引都是 **UTF-16 码元**：`cursor_col`、`prefix`、`item.value` 的长度以及
//!   `slice_*` / `utf16_len` 的返回值。调用方必须用同一单位传入 `cursor_col`。
//! - 本模块没有错误类型：目录不可读、`fd` 启动失败或非零退出、输出为空、locale 非法，
//!   全部折叠成“空建议 / `None`”。
//! - `process_locale_is_valid` 与进程默认 locale 的 collator 各自用 `OnceLock` 缓存，
//!   进程内第一次调用之后修改 `LC_ALL` / `LC_MESSAGES` / `LANG` 不再影响已有缓存。
//! - [`CombinedAutocompleteProvider::get_file_suggestions`] 是同步 `std::fs::read_dir`
//!   实现，`get_suggestions_impl` 在异步任务里直接调用（没有 `spawn_blocking`），
//!   因此会阻塞调用它的那个任务线程。
//! - 取消走 `CancellationToken`：`fd` 等待用 `select!` 与令牌竞争；future 被丢弃时
//!   `kill_on_drop(true)` 终止子进程。

use std::cmp::Ordering;
use std::collections::HashSet;
use std::future::Future;
use std::path::Path;
use std::pin::Pin;
use std::process::Stdio;
use std::sync::OnceLock;

use icu_collator::options::{CollatorOptions, Strength};
use icu_collator::{Collator, CollatorPreferences};
use icu_locale::Locale;

use crate::fuzzy::fuzzy_filter;
use crate::text::{is_autocomplete_separator, is_js_whitespace, trim_js};

// ============================================================================
// 公开类型（spec/tui-text.md §2.10.1，签名与 tui-components.md §5.1 一致）
// ============================================================================

/// 一条补全候选。TS: `interface AutocompleteItem`。
///
/// `value` 是写回编辑缓冲区的主体文本（文件候选可能带 `@`、引号或目录尾部 `/`；
/// 命令名候选是去掉 `/` 的名字），具体拼接方式由 apply_completion 按上下文决定。
/// `label` 用于下拉列表显示；`get_file_suggestions` 的排序（目录优先，再按
/// `locale_compare(&label)`）与 [`CombinedAutocompleteProvider::apply_completion_impl`]
/// 的目录判定也依赖它（目录候选以 `/` 结尾），修改 `label` 会同时影响排序和补全结果。
/// `description` 是可选说明。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AutocompleteItem {
    /// 应用补全时插入的文本；与 `prefix` 的拼接由
    /// [`AutocompleteProvider::apply_completion`] 按上下文决定。
    pub value: String,
    /// 列表显示名。本模块生成的目录候选以 `/` 结尾；
    /// [`CombinedAutocompleteProvider::apply_completion_impl`] 也靠这个后缀判断是否为目录。
    pub label: String,
    /// 次要说明；命令名补全里由 `argumentHint — description` 拼成。
    pub description: Option<String>,
}

impl AutocompleteItem {
    /// 构造没有 `description` 的候选。
    pub fn new(value: impl Into<String>, label: impl Into<String>) -> Self {
        Self {
            value: value.into(),
            label: label.into(),
            description: None,
        }
    }

    /// 链式设置 `description`（对应 TS 对象字面量的可选字段）。
    pub fn with_description(mut self, description: impl Into<String>) -> Self {
        self.description = Some(description.into());
        self
    }
}

/// 参数补全闭包的返回类型（TS 的 `Awaitable<AutocompleteItem[] | null>`）。
///
/// `getSuggestions` 对 `None` 与 `Some(vec![])` 一视同仁（都返回“无建议”）；保留两种
/// 表示只是为了贴合 TS 闭包的原始签名。future 必须 `Send`，因为
/// [`AutocompleteProvider::get_suggestions`] 返回的装箱 future 要求 `Send`。
pub type ArgumentCompletionsFuture<'a> =
    Pin<Box<dyn Future<Output = Option<Vec<AutocompleteItem>>> + Send + 'a>>;

/// 斜杠命令。TS: `interface SlashCommand`。
///
/// `getArgumentCompletions` 是可选成员；Rust 用 `Option<Box<dyn Fn>>` 保留“未提供”与
/// “提供了但返回 `None`”的区别（TS 的 `"getArgumentCompletions" in command` 探测，见
/// `spec/tui-components.md` §A.9 的同类约定）。
pub struct SlashCommand {
    /// 命令名，**不含**前导 `/`（补全时由调用方拼上）。
    pub name: String,
    /// 列表里显示的说明。
    pub description: Option<String>,
    /// 用法提示；命令名补全时与 `description` 合成 `"hint — desc"` 显示。
    pub argument_hint: Option<String>,
    /// 参数补全闭包：入参是 `/命令 ` 之后的原文，返回候选或 `None`。
    ///
    /// `None` 表示“此命令不提供参数补全”；`Some(f)` 里的 `f` 允许返回 `None`（当次没有
    /// 候选）。这正是 TS 用 `"getArgumentCompletions" in command` 探测、而不是直接调用
    /// 并接受 `null` 的原因。
    #[allow(clippy::type_complexity)]
    pub get_argument_completions:
        Option<Box<dyn for<'a> Fn(&'a str) -> ArgumentCompletionsFuture<'a> + Send + Sync>>,
}

impl std::fmt::Debug for SlashCommand {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SlashCommand")
            .field("name", &self.name)
            .field("description", &self.description)
            .field("argument_hint", &self.argument_hint)
            .field(
                "has_argument_completions",
                &self.get_argument_completions.is_some(),
            )
            .finish()
    }
}

impl SlashCommand {
    /// 只有名字的命令：无描述、无提示、不做参数补全。
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            description: None,
            argument_hint: None,
            get_argument_completions: None,
        }
    }

    /// 设置列表说明。
    pub fn with_description(mut self, description: impl Into<String>) -> Self {
        self.description = Some(description.into());
        self
    }

    /// 设置用法提示（例如参数格式）。
    pub fn with_argument_hint(mut self, hint: impl Into<String>) -> Self {
        self.argument_hint = Some(hint.into());
        self
    }

    /// 同步版参数补全：每次调用把 `f(prefix)` 的结果包成已完成的 future。
    pub fn with_argument_completions<F>(mut self, f: F) -> Self
    where
        F: Fn(&str) -> Option<Vec<AutocompleteItem>> + Send + Sync + 'static,
    {
        self.get_argument_completions = Some(Box::new(move |prefix: &str| {
            let items = f(prefix);
            Box::pin(std::future::ready(items)) as ArgumentCompletionsFuture<'_>
        }));
        self
    }

    /// 固定候选集（忽略参数前缀），对应测试里的常见写法。
    pub fn with_fixed_argument_completions(mut self, items: Vec<AutocompleteItem>) -> Self {
        self.get_argument_completions = Some(Box::new(move |_prefix: &str| {
            Box::pin(std::future::ready(Some(items.clone()))) as ArgumentCompletionsFuture<'_>
        }));
        self
    }
}

/// 命令表的一项：TS 的结构联合 `SlashCommand | AutocompleteItem`。
///
/// TS 用 `"name" in cmd` 判别；两者都是字面量对象，等价于“有 `name` 键”。这里用显式枚举，
/// 语义一致且不需要运行期探测。
#[derive(Debug)]
pub enum CommandEntry {
    /// 完整斜杠命令，可携带参数补全闭包。
    Slash(SlashCommand),
    /// 只有名字/描述的轻量项（TS 结构联合的另一支）；不能做参数补全。
    Item(AutocompleteItem),
}

impl CommandEntry {
    /// `"name" in cmd ? cmd.name : cmd.value`
    ///
    /// 两种变体都把它当作命令名；`Item` 用 `value` 而不是 `label`。
    pub fn name(&self) -> &str {
        match self {
            CommandEntry::Slash(cmd) => &cmd.name,
            CommandEntry::Item(item) => &item.value,
        }
    }

    /// 列表说明；`Item` 变体读 `AutocompleteItem::description`。
    pub fn description(&self) -> Option<&str> {
        match self {
            CommandEntry::Slash(cmd) => cmd.description.as_deref(),
            CommandEntry::Item(item) => item.description.as_deref(),
        }
    }

    /// `"argumentHint" in cmd && cmd.argumentHint ? ... : undefined`
    ///
    /// 只有 `Slash` 变体可能返回 `Some`；`Item` 永远没有参数提示。
    pub fn argument_hint(&self) -> Option<&str> {
        match self {
            CommandEntry::Slash(cmd) => cmd.argument_hint.as_deref(),
            CommandEntry::Item(_) => None,
        }
    }

    /// 参数补全闭包；只有 `Slash` 变体且构造时提供了闭包才返回 `Some`。
    pub fn get_argument_completions(
        &self,
    ) -> Option<&(dyn for<'a> Fn(&'a str) -> ArgumentCompletionsFuture<'a> + Send + Sync)> {
        match self {
            CommandEntry::Slash(cmd) => cmd.get_argument_completions.as_deref(),
            CommandEntry::Item(_) => None,
        }
    }
}

impl From<SlashCommand> for CommandEntry {
    fn from(value: SlashCommand) -> Self {
        CommandEntry::Slash(value)
    }
}

impl From<AutocompleteItem> for CommandEntry {
    fn from(value: AutocompleteItem) -> Self {
        CommandEntry::Item(value)
    }
}

/// 一次补全的建议集合。TS: `interface AutocompleteSuggestions`。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AutocompleteSuggestions {
    /// 候选列表，按 provider 决定的顺序展示（文件补全已排好序）。
    pub items: Vec<AutocompleteItem>,
    /// 匹配所用的前缀（例如 `"/"` 或 `"src/"`）。
    ///
    /// 应用候选时调用方必须把它原样传给 [`AutocompleteProvider::apply_completion`]；
    /// 本模块的实现按这个字符串的 UTF-16 长度回退光标。
    pub prefix: String,
}

/// `getSuggestions` 的选项。TS: `{ signal: AbortSignal; force?: boolean }`。
#[derive(Debug, Clone, Default)]
pub struct GetSuggestionsOptions {
    /// 取消令牌：`fd` 搜索在 `select!` 里与它竞争，取消后立刻返回空表。
    ///
    /// 参数补全的 `await` 不单独轮询该令牌（闭包签名没有 signal），只有外层丢弃整个
    /// future 时才会中断。
    pub signal: tokio_util::sync::CancellationToken,
    /// `true` 表示显式补全（Tab）：跳过斜杠命令分支，无条件尝试路径补全。
    pub force: bool,
}

impl GetSuggestionsOptions {
    /// 构造选项；`Default` 等价于“未取消 + 非强制”。
    pub fn new(signal: tokio_util::sync::CancellationToken, force: bool) -> Self {
        Self { signal, force }
    }
}

/// `applyCompletion` 的返回值。
///
/// 描述的是**整个编辑缓冲区**的新状态，而不是单行。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApplyCompletionResult {
    /// 替换后的完整缓冲区（传入的 `lines` 的副本，`cursor_line` 超界时会被补空行）。
    pub lines: Vec<String>,
    /// 应用后光标所在行（当前实现恒等于调用方传入的 `cursor_line`）。
    pub cursor_line: usize,
    /// 应用后光标的 UTF-16 列号。
    pub cursor_col: usize,
}

/// 补全提供者。TS: `interface AutocompleteProvider`。
///
/// 签名与 `spec/tui-components.md` §5.1 一致（`Editor` 消费的唯一契约）。
/// trait 要求 `Send`：Editor 把 provider 装箱保存，并在异步补全请求里跨 `await` 持有它；
/// 因此实现里的可变状态也必须能在请求期间被独占（`get_suggestions` 取 `&mut self`）。
pub trait AutocompleteProvider: Send {
    /// 返回一个只读副本，供宿主在 `get_suggestions` 持锁期间调用同步方法。
    ///
    /// 默认 `None`：大多数 provider 没有可安全共享的只读副本，宿主继续用
    /// `try_lock` 降级。JS provider 会返回一个只共享引用/回退句柄的新包装，因此
    /// `applyCompletion`/`triggerCharacters` 不会因为一次在飞的异步查询而失效。
    fn sync_snapshot(&self) -> Option<std::sync::Arc<dyn AutocompleteProvider>> {
        None
    }

    /// 自然触发本 provider 的字符（token 边界处）。
    ///
    /// 默认 `None`：不追加额外触发字符。Editor 仍保留自己的默认触发集（`@`/`#`），
    /// 传入的值只是并集的一部分。
    fn trigger_characters(&self) -> Option<Vec<char>> {
        None
    }

    /// 取当前光标处的补全建议；无建议时返回 `None`。
    ///
    /// `lines` 是整个缓冲区，`cursor_line` / `cursor_col` 中列号是 **UTF-16 码元**。
    /// 返回的 `prefix` 必须原样交给 [`AutocompleteProvider::apply_completion`]；
    /// 否则替换范围与光标位置都会算错。
    fn get_suggestions<'a>(
        &'a mut self,
        lines: &'a [String],
        cursor_line: usize,
        cursor_col: usize,
        options: &'a GetSuggestionsOptions,
    ) -> Pin<Box<dyn Future<Output = Option<AutocompleteSuggestions>> + Send + 'a>>;

    /// 应用选中的候选，返回新文本与光标位置。
    ///
    /// 约定：用 `prefix` 的 UTF-16 长度定位替换起点（`cursor_col - prefix 的码元数`）。
    /// 本文件的实现是把该点之前的文本、`item.value` 与光标后文本拼起来；斜杠命令
    /// 分支会额外保留 `/` 与一个空格。实现不假设 `item.value` 与 `prefix` 之间存在
    /// 前后缀关系。
    fn apply_completion(
        &self,
        lines: &[String],
        cursor_line: usize,
        cursor_col: usize,
        item: &AutocompleteItem,
        prefix: &str,
    ) -> ApplyCompletionResult;

    /// 显式 Tab 补全时是否应触发文件补全。
    ///
    /// Editor 在 `force` 请求前调用；返回 `false` 时连当前在飞的请求都不会取消。
    /// 默认 `true`。
    fn should_trigger_file_completion(
        &self,
        _lines: &[String],
        _cursor_line: usize,
        _cursor_col: usize,
    ) -> bool {
        true
    }
}

// ============================================================================
// UTF-16 工具（ARCHITECTURE.md §4.6）
// ============================================================================

/// `string.length`（UTF-16 码元数）。
pub(crate) fn utf16_len(text: &str) -> usize {
    text.encode_utf16().count()
}

/// 把 `text` 拆成 UTF-16 码元序列（辅助平面字符占两个码元）。
fn utf16_units(text: &str) -> Vec<u16> {
    text.encode_utf16().collect()
}

/// 码元序列 → 字符串；落单的代理写成 `U+FFFD`（与 `text.rs` 的同一约定）。
fn units_to_string(units: &[u16]) -> String {
    String::from_utf16_lossy(units)
}

/// JS `slice` 的索引归一化：负数从尾部计。
fn resolve_slice_index(index: i64, len: usize) -> usize {
    if index < 0 {
        let shifted = len as i64 + index;
        shifted.clamp(0, len as i64) as usize
    } else {
        (index as u64).min(len as u64) as usize
    }
}

/// `units.slice(start, end)`（UTF-16 语义，支持负索引），返回码元切片。
///
/// 保留为切片（而不是立刻转成 `String`）是为了让调用方能在**拼接之后**再解码：
/// 边界落在代理对中间时，两半拼回去仍是原字符（见 [`concat_utf16`]）。
fn slice_units(units: &[u16], start: i64, end: i64) -> &[u16] {
    let from = resolve_slice_index(start, units.len());
    let to = resolve_slice_index(end, units.len());
    if to <= from {
        return &[];
    }
    &units[from..to]
}

/// `text.slice(start, end)`（UTF-16 语义，支持负索引）。
fn js_slice(text: &str, start: i64, end: i64) -> String {
    units_to_string(slice_units(&utf16_units(text), start, end))
}

/// JS 字符串拼接的一个片段：一段文本或一段 UTF-16 码元切片。
enum Utf16Piece<'a> {
    Units(&'a [u16]),
    Text(&'a str),
}

/// 按 UTF-16 码元拼接后整体解码，等价于 JS 的 `a + b + ...`。
///
/// TS 的 `String.prototype.slice` 在代理对中间切开时，两个片段各自保留落单的代理；
/// Rust 的 `String` 无法表示落单代理。先拼接再解码可以让“切开又拼回”的字符还原，
/// 只有最终仍落单的代理才变成 `U+FFFD`（[`units_to_string`] 的约定）。
fn concat_utf16(pieces: &[Utf16Piece<'_>]) -> String {
    let mut units: Vec<u16> = Vec::new();
    for piece in pieces {
        match piece {
            Utf16Piece::Units(slice) => units.extend_from_slice(slice),
            Utf16Piece::Text(text) => units.extend(text.encode_utf16()),
        }
    }
    units_to_string(&units)
}

/// `text.slice(0, end)`。
fn slice_prefix(text: &str, end: i64) -> String {
    js_slice(text, 0, end)
}

/// `text.slice(start)`。
fn slice_suffix(text: &str, start: i64) -> String {
    js_slice(text, start, i64::MAX)
}

/// `String.prototype.trimStart()`（JS 空白集合）。
fn trim_start_js(text: &str) -> &str {
    text.trim_start_matches(is_js_whitespace)
}

// ============================================================================
// 模块级常量（spec §2.10.2）
// ============================================================================

/// `PATH_DELIMITERS = new Set([" ", "\t", '"', "'", "="])`
fn is_path_delimiter(ch: char) -> bool {
    matches!(ch, ' ' | '\t' | '"' | '\'' | '=')
}

/// `PATH_WRAPPERS = { "(": ")", "[": "]", "{": "}", "<": ">", "`": "`" }`
fn path_wrapper_closer(ch: char) -> Option<char> {
    match ch {
        '(' => Some(')'),
        '[' => Some(']'),
        '{' => Some('}'),
        '<' => Some('>'),
        '`' => Some('`'),
        _ => None,
    }
}

/// `tokenStartRegex.test(text)`：`autocompleteBoundaryRegex` + `$`。
///
/// 该正则的真实含义是“**字符串的最后一个字符**是分隔符，或字符串为空”
/// （`^` 分支只在空串成立；JS 的 `$` 只匹配输入末尾），见 spec §2.10.2。
fn token_start_matches(text: &str) -> bool {
    match text.chars().last() {
        None => true,
        Some(ch) => is_autocomplete_separator(ch),
    }
}

/// `toDisplayPath(value)`：`value.replace(/\\/g, "/")`。
fn to_display_path(value: &str) -> String {
    value.replace('\\', "/")
}

/// `escapeRegex(value)`：`value.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")`。
fn escape_regex(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for ch in value.chars() {
        if matches!(
            ch,
            '.' | '*' | '+' | '?' | '^' | '$' | '{' | '}' | '(' | ')' | '|' | '[' | ']' | '\\'
        ) {
            out.push('\\');
        }
        out.push(ch);
    }
    out
}

/// `buildFdPathQuery(query)`：把用户输入变成 fd 的正则（逐字移植）。
fn build_fd_path_query(query: &str) -> String {
    let normalized = to_display_path(query);
    if !normalized.contains('/') {
        return escape_regex(&normalized);
    }

    let has_trailing_separator = normalized.ends_with('/');
    let trimmed = normalized.trim_matches('/');
    if trimmed.is_empty() {
        return normalized;
    }

    let separator_pattern = "[\\\\/]";
    let segments: Vec<String> = trimmed
        .split('/')
        .filter(|segment| !segment.is_empty())
        .map(escape_regex)
        .collect();

    let mut pattern = segments.join(separator_pattern);
    if has_trailing_separator {
        pattern.push_str(separator_pattern);
    }
    pattern
}

/// `findLastDelimiter(text)`：最后一个分隔符的**最后一个 UTF-16 码元**索引（-1 表示没有）。
///
/// TS 按码点迭代但累加 `character.length`（1 或 2），因此代理对字符命中时 `lastDelimiter`
/// 指向低代理。Rust 侧同一算法用 `encode_utf16().count()` 复现。
fn find_last_delimiter(text: &str) -> i64 {
    let mut last_delimiter: i64 = -1;
    let mut index: i64 = 0;
    for character in text.chars() {
        index += character.len_utf16() as i64;
        if is_path_delimiter(character) || is_autocomplete_separator(character) {
            last_delimiter = index - 1;
        }
    }
    last_delimiter
}

/// `stripLeadingWrappers(token)`：剥掉开括号 / 反引号前缀，但保留“token 内已有对应闭符”的。
///
/// 例：`"(~/Dev"` → `"~/Dev"`、`` "`src/ma" `` → `"src/ma"`；而 `"app/[slug]/pa"`、
/// `"(group)/pa"` 保持不变。
fn strip_leading_wrappers(token: &str) -> String {
    let mut result = token;
    while !result.is_empty() {
        let first = result.chars().next().expect("non-empty");
        let Some(closer) = path_wrapper_closer(first) else {
            break;
        };
        // `result.includes(closer, 1)`：从 UTF-16 索引 1 起查找。
        // `first` 恒为 BMP 字符（开括号全在 BMP），所以从第 2 个码元起 == 跳过 1 个 char。
        let rest: String = result.chars().skip(1).collect();
        if rest.contains(closer) {
            break;
        }
        result = &result[first.len_utf8()..];
    }
    result.to_string()
}

/// `findUnclosedQuoteStart(text)`：最后一个未闭合 `"` 的 UTF-16 索引（不处理转义）。
fn find_unclosed_quote_start(text: &str) -> Option<usize> {
    let mut in_quotes = false;
    let mut quote_start: isize = -1;
    for (index, unit) in text.encode_utf16().enumerate() {
        if unit == u16::from(b'"') {
            in_quotes = !in_quotes;
            if in_quotes {
                quote_start = index as isize;
            }
        }
    }
    if in_quotes {
        Some(quote_start as usize)
    } else {
        None
    }
}

/// `text[index - 1]`（UTF-16 码元）——非 BMP 位置返回 `None`（代理码元不会命中任何集合）。
fn utf16_unit_before(text: &str, index: usize) -> Option<char> {
    if index == 0 {
        return None;
    }
    let units: Vec<u16> = text.encode_utf16().collect();
    units
        .get(index - 1)
        .copied()
        .and_then(|unit| char::from_u32(u32::from(unit)))
}

/// `isTokenStart(text, index)`：`index` 处是否是新 token 的起点。
fn is_token_start(text: &str, index: usize) -> bool {
    // 先把紧邻 index 的 wrapper 开符（`(`/`[`/`{`/`<`/反引号）并入 token：前一码元
    // 只要是开符就继续回退 start。之后判断 start 是否落在 token 边界：
    // 前一码元是 PATH_DELIMITERS，或 start 之前的字符属于 autocomplete 分隔符集合
    // （`start == 0` 时后一条件按空串成立）。
    let mut start = index;
    while start > 0 {
        match utf16_unit_before(text, start) {
            Some(ch) if path_wrapper_closer(ch).is_some() => start -= 1,
            _ => break,
        }
    }
    if utf16_unit_before(text, start).is_some_and(is_path_delimiter) {
        return true;
    }
    token_start_matches(&slice_prefix(text, start as i64))
}

/// `extractQuotedPrefix(text)`：未闭合引号 token 的前缀（含 `"`；`@"` 时含 `@`）。
fn extract_quoted_prefix(text: &str) -> Option<String> {
    let quote_start = find_unclosed_quote_start(text)?;

    if quote_start > 0 && utf16_unit_before(text, quote_start) == Some('@') {
        if !is_token_start(text, quote_start - 1) {
            return None;
        }
        return Some(slice_suffix(text, quote_start as i64 - 1));
    }

    if !is_token_start(text, quote_start) {
        return None;
    }

    Some(slice_suffix(text, quote_start as i64))
}

/// `parsePathPrefix(prefix)` 的结果。
#[derive(Debug, Clone, PartialEq, Eq)]
struct ParsedPathPrefix {
    raw_prefix: String,
    is_at_prefix: bool,
    is_quoted_prefix: bool,
}

/// `parsePathPrefix(prefix)`。
fn parse_path_prefix(prefix: &str) -> ParsedPathPrefix {
    if let Some(rest) = prefix.strip_prefix("@\"") {
        return ParsedPathPrefix {
            raw_prefix: rest.to_string(),
            is_at_prefix: true,
            is_quoted_prefix: true,
        };
    }
    if let Some(rest) = prefix.strip_prefix('"') {
        return ParsedPathPrefix {
            raw_prefix: rest.to_string(),
            is_at_prefix: false,
            is_quoted_prefix: true,
        };
    }
    if let Some(rest) = prefix.strip_prefix('@') {
        return ParsedPathPrefix {
            raw_prefix: rest.to_string(),
            is_at_prefix: true,
            is_quoted_prefix: false,
        };
    }
    ParsedPathPrefix {
        raw_prefix: prefix.to_string(),
        is_at_prefix: false,
        is_quoted_prefix: false,
    }
}

/// `buildCompletionValue(path, {isDirectory, isAtPrefix, isQuotedPrefix})`。
///
/// 值需要加引号的条件：原前缀就以引号开头，或路径里含 autocomplete 分隔符。
/// `is_autocomplete_separator` 的定义是 JS 空白（空格、tab、换行、NBSP 等）或 CJK 标点；
/// `"`、`'`、`=` 属于 `PATH_DELIMITERS`，单独出现在路径里**不会**触发加引号。
/// `@` 前缀时引号跟在 `@` 后面（`@"path"`）。
///
/// `isDirectory` 在 TS 里**未被使用**（路径值本身已带尾部 `/`）；这里保留参数以对齐签名语义。
fn build_completion_value(
    path: &str,
    _is_directory: bool,
    is_at_prefix: bool,
    is_quoted_prefix: bool,
) -> String {
    let needs_quotes = is_quoted_prefix || path.chars().any(is_autocomplete_separator);
    let prefix = if is_at_prefix { "@" } else { "" };

    if !needs_quotes {
        return format!("{prefix}{path}");
    }

    format!("{prefix}\"{path}\"")
}

// ============================================================================
// node:path posix（Node 22+ 的 `lib/path.js` 逐行重写）
// ============================================================================

/// `path.posix.dirname(path)`。
pub(crate) fn path_dirname(path: &str) -> String {
    if path.is_empty() {
        return ".".to_string();
    }
    let units: Vec<u16> = path.encode_utf16().collect();
    let slash = u16::from(b'/');
    let has_root = units[0] == slash;
    let mut end: isize = -1;
    let mut matched_slash = true;

    let mut i = units.len() as isize - 1;
    while i >= 1 {
        if units[i as usize] == slash {
            if !matched_slash {
                end = i;
                break;
            }
        } else {
            matched_slash = false;
        }
        i -= 1;
    }

    if end == -1 {
        return if has_root { "/" } else { "." }.to_string();
    }
    if has_root && end == 1 {
        return "//".to_string();
    }
    slice_prefix(path, end as i64)
}

/// `path.posix.basename(path)`（无 `suffix` 参数）。
pub(crate) fn path_basename(path: &str) -> String {
    let units: Vec<u16> = path.encode_utf16().collect();
    let slash = u16::from(b'/');
    let mut start: isize = 0;
    let mut end: isize = -1;
    let mut matched_slash = true;

    let mut i = units.len() as isize - 1;
    while i >= 0 {
        if units[i as usize] == slash {
            if !matched_slash {
                start = i + 1;
                break;
            }
        } else if end == -1 {
            matched_slash = false;
            end = i + 1;
        }
        i -= 1;
    }

    if end == -1 {
        return String::new();
    }
    js_slice(path, start as i64, end as i64)
}

/// `normalizeString(path, allowAboveRoot, ...)`（posix 专用，逐行照搬 Node 的 `lib/path.js`）。
fn normalize_string(path: &str, allow_above_root: bool) -> String {
    let units: Vec<u16> = path.encode_utf16().collect();
    let slash = u16::from(b'/');
    let dot = u16::from(b'.');
    let mut res: Vec<u16> = Vec::new();
    let mut last_segment_length: isize = 0;
    let mut last_slash: isize = -1;
    let mut dots: isize = 0;
    let mut code: u16 = 0;
    let len = units.len() as isize;

    // 扫描模型（与 Node 的 `normalizeString` 相同）：按 `/` 切段，维护三个状态：
    // - `last_slash`：上一段结束的位置；
    // - `last_segment_length`：`res` 中最后一段的长度（供 `..` 回退使用）；
    // - `dots`：当前段已累积的 `.` 个数（`-1` 表示本段不是纯点号）。
    // 遇到分隔符时按段处理：空段（连续斜杠）跳过、`.` 跳过、`..` 弹出 `res` 的最后
    // 一段（回退到根之后，是否允许继续向上由 `allow_above_root` 决定：相对路径会追加
    // `..`，绝对路径不会）、其它段原样追加。循环条件 `i <= len` 与末尾补的虚拟 `/`
    // 让最后一段也走同一段收尾逻辑。
    let mut i: isize = 0;
    while i <= len {
        if i < len {
            code = units[i as usize];
        } else if code == slash {
            break;
        } else {
            code = slash;
        }

        if code == slash {
            if last_slash == i - 1 || dots == 1 {
                // NOOP
            } else if dots == 2 {
                let res_len = res.len();
                let ends_with_dots =
                    res_len >= 2 && res[res_len - 1] == dot && res[res_len - 2] == dot;
                if res_len < 2 || last_segment_length != 2 || !ends_with_dots {
                    if res_len > 2 {
                        let last_slash_index = res_len as isize - last_segment_length - 1;
                        if last_slash_index == -1 {
                            res.clear();
                            last_segment_length = 0;
                        } else {
                            res.truncate(last_slash_index as usize);
                            let last_index = res
                                .iter()
                                .rposition(|&unit| unit == slash)
                                .map(|index| index as isize)
                                .unwrap_or(-1);
                            last_segment_length = res.len() as isize - 1 - last_index;
                        }
                        last_slash = i;
                        dots = 0;
                        i += 1;
                        continue;
                    } else if res_len != 0 {
                        res.clear();
                        last_segment_length = 0;
                        last_slash = i;
                        dots = 0;
                        i += 1;
                        continue;
                    }
                }
                if allow_above_root {
                    if !res.is_empty() {
                        res.push(slash);
                    }
                    res.push(dot);
                    res.push(dot);
                    last_segment_length = 2;
                }
            } else {
                if !res.is_empty() {
                    res.push(slash);
                }
                let segment = js_slice(path, last_slash as i64 + 1, i as i64);
                res.extend(segment.encode_utf16());
                last_segment_length = i - last_slash - 1;
            }
            last_slash = i;
            dots = 0;
        } else if code == dot && dots != -1 {
            dots += 1;
        } else {
            dots = -1;
        }
        i += 1;
    }

    units_to_string(&res)
}

/// `path.posix.normalize(path)`。
pub(crate) fn path_normalize(path: &str) -> String {
    if path.is_empty() {
        return ".".to_string();
    }
    let units: Vec<u16> = path.encode_utf16().collect();
    let is_absolute = units[0] == u16::from(b'/');
    let trailing_separator = units[units.len() - 1] == u16::from(b'/');

    let normalized = normalize_string(path, !is_absolute);

    if normalized.is_empty() {
        if is_absolute {
            return "/".to_string();
        }
        return if trailing_separator { "./" } else { "." }.to_string();
    }

    let with_trailing = if trailing_separator {
        format!("{normalized}/")
    } else {
        normalized
    };

    if is_absolute {
        format!("/{with_trailing}")
    } else {
        with_trailing
    }
}

/// `path.posix.join(a, b)`：空串参数被丢弃，其余以 `/` 连接后 `normalize`。
pub(crate) fn path_join(a: &str, b: &str) -> String {
    let mut parts: Vec<&str> = Vec::new();
    if !a.is_empty() {
        parts.push(a);
    }
    if !b.is_empty() {
        parts.push(b);
    }
    if parts.is_empty() {
        return ".".to_string();
    }
    path_normalize(&parts.join("/"))
}

// ============================================================================
// localeCompare（ARCHITECTURE.md §6.4 / §8 第 27 条）
// ============================================================================
//
// TS 的 `a.localeCompare(b)` 使用 **运行时默认 locale** 的 collation。Node 26（ICU 78.3）的
// 默认 locale 由 `LC_ALL` → `LC_MESSAGES` → `LANG` 决定（`Intl` 初始化时写进 ICU 默认
// locale），所以要让排序逐位一致，必须同时复刻 **locale 解析** 与 **collation 选择**。
//
// ## locale 解析（对 Node 26.12.1 实测得到的规则）
//
// - 三个键里**第一个存在的**胜出，值即使为空串 / 纯空白也算：`LC_ALL=""` + `LANG=ja_JP`
//   → `und`（不是 `ja-JP`）；三个键都未设置 → ICU 的编译期默认 `en-US`。
// - 值**不 trim**：`"ja_JP.UTF-8 "` → `ja-JP`；`" ja_JP.UTF-8 "` → `und-JP`（前导空格让语言段
//   失效，后面的地区仍会被解析）。
// - POSIX 结构 `language[_territory][.codeset][@modifier]`：`.` 起的 codeset 及其后内容全部
//   丢弃；`@` 取**最后一段**（`en_US@x@y` → 只有 `y`），段内含 `=` 时是 keyword（不产生
//   变体），否则按 `_` 拆成变体。
// - 大小写敏感的关键字：整段恰好是 `C` / `POSIX` 时按 `en-US` 处理并带上 `posix` 标记
//   （`C@euro` → `en-US-x-lvariant-euro-posix`）；`c`/`posix` 不是关键字。
// - 3 字母语言代码按 ISO 639-2 → 639-1 折叠（`jpn` → `ja`），但 Node 的 ICU 数据**不**折叠
//   部分旧代码（`ger`/`fre`/`chi`/`dut`/`cze`/… 不在 [`LANGUAGE_ALIASES`] 里，保持原样）；
//   2 字母旧代码 `in`/`iw`/`ji`/`jw`/`mo` 折叠。
// - 地区：2 字母（大写 + 别名）、3 位数字、或 ISO 3166-1 alpha-3（别名表内的才有效），
//   见 [`REGION_ALIASES`]；其余 3 字母字段被丢弃。
// - 4 字母字段（语言之后）是脚本，首字母大写；5~8 位字母数字或首位是数字的 4 位字段是
//   BCP-47 变体；1~4 位（非上述）是 ICU 的 “localvar”，渲染成 `-x-lvariant-*`；其余丢弃。
//   变体按字典序排序（Node 对 `en_US_efgh_abcd` 与 `en_US_abcd_efgh` 都输出 `abcd-efgh`）。
//
// ## collation 选择
//
// ICU4X 2.4.0 在**默认选项**下与 Node 的 ICU 78.3 逐位一致（en-US / und / zh-CN / zh-TW /
// ja-JP / th-TH / de-DE / sr-Latn-RS 等 150 个 locale 的全部样例对比）。三个坑：
//
// 1. **不要**显式设 `alternate_handling: NonIgnorable`：ICU4X 的默认（`None`，跟随 locale
//    数据）才与 Node 一致。Thai 的默认是 shifted，显式 NonIgnorable 会让 `!` < `"`，而 Node
//    在 `th-TH` 下把两者视为相等（552/1260 标点对顺序不同）。spec §6.4 建议的
//    `NonIgnorable` 与运行时行为不符，以运行时为准。
// 2. **不要**用 `LocaleCanonicalizer` 规范化：Node 从环境拿到 `chi-CN` 时，collation 查表
//    仍是 root 数据（拉丁 < CJK），只有显式 `new Intl.Collator("chi-CN")` 才会先规范化成
//    `zh-CN`；ICU4X 的 `Collator` 对 `chi-CN` 同样落在 root 数据上，规范化反而会引入差异。
// 3. `sh`：Node 的 locale 字符串保留 `sh-RS`，但 collation 数据按 `sr-Latn` 取；ICU4X 没有
//    `sh` 的数据，所以查 collator 前映射成 `sr-Latn`。
//
// 无法建立 collator（例如 `english`/`posix` 这类非 BCP-47 语言）时回退 en-US collator
// （Node 对这些输入拿到的是 root 数据，root 与 en-US 在所有实测样本上一致）。

/// 3 字母语言代码 → Node（ICU）使用的短代码。对 Node 26.12.1 全量探测 17576 个组合得到，
/// 另含 2 字母旧代码 `in`/`iw`/`ji`/`jw`/`mo`。表外的代码原样保留。
const LANGUAGE_ALIASES: &[(&str, &str)] = &[
    ("aam", "aas"),
    ("aar", "aa"),
    ("abk", "ab"),
    ("adp", "dz"),
    ("afr", "af"),
    ("aka", "ak"),
    ("amh", "am"),
    ("ara", "ar"),
    ("arg", "an"),
    ("asm", "as"),
    ("aue", "ktz"),
    ("ava", "av"),
    ("ave", "ae"),
    ("aym", "ay"),
    ("ayx", "nun"),
    ("aze", "az"),
    ("bak", "ba"),
    ("bam", "bm"),
    ("bel", "be"),
    ("ben", "bn"),
    ("bgm", "bcg"),
    ("bis", "bi"),
    ("bjd", "drl"),
    ("bod", "bo"),
    ("bos", "bs"),
    ("bre", "br"),
    ("bul", "bg"),
    ("cat", "ca"),
    ("ccq", "rki"),
    ("ces", "cs"),
    ("cha", "ch"),
    ("che", "ce"),
    ("chu", "cu"),
    ("chv", "cv"),
    ("cjr", "mom"),
    ("cka", "cmr"),
    ("cmk", "xch"),
    ("cor", "kw"),
    ("cos", "co"),
    ("coy", "pij"),
    ("cqu", "quh"),
    ("cre", "cr"),
    ("cym", "cy"),
    ("dan", "da"),
    ("deu", "de"),
    ("div", "dv"),
    ("drh", "khk"),
    ("drw", "prs"),
    ("dzo", "dz"),
    ("ell", "el"),
    ("eng", "en"),
    ("epo", "eo"),
    ("est", "et"),
    ("eus", "eu"),
    ("ewe", "ee"),
    ("fao", "fo"),
    ("fas", "fa"),
    ("fij", "fj"),
    ("fin", "fi"),
    ("fra", "fr"),
    ("fry", "fy"),
    ("ful", "ff"),
    ("gav", "dev"),
    ("gfx", "vaj"),
    ("ggn", "gvr"),
    ("gla", "gd"),
    ("gle", "ga"),
    ("glg", "gl"),
    ("glv", "gv"),
    ("grn", "gn"),
    ("gti", "nyc"),
    ("guj", "gu"),
    ("guv", "duz"),
    ("hat", "ht"),
    ("hau", "ha"),
    ("heb", "he"),
    ("her", "hz"),
    ("hin", "hi"),
    ("hmo", "ho"),
    ("hrr", "jal"),
    ("hrv", "hr"),
    ("hun", "hu"),
    ("hye", "hy"),
    ("ibi", "opa"),
    ("ibo", "ig"),
    ("ido", "io"),
    ("iii", "ii"),
    ("iku", "iu"),
    ("ile", "ie"),
    ("ilw", "gal"),
    ("in", "id"),
    ("ina", "ia"),
    ("ind", "id"),
    ("ipk", "ik"),
    ("isl", "is"),
    ("ita", "it"),
    ("iw", "he"),
    ("jav", "jv"),
    ("jaw", "jv"),
    ("jeg", "oyb"),
    ("ji", "yi"),
    ("jpn", "ja"),
    ("jw", "jv"),
    ("kal", "kl"),
    ("kan", "kn"),
    ("kas", "ks"),
    ("kat", "ka"),
    ("kau", "kr"),
    ("kaz", "kk"),
    ("kgc", "tdf"),
    ("kgh", "kml"),
    ("khm", "km"),
    ("kik", "ki"),
    ("kin", "rw"),
    ("kir", "ky"),
    ("koj", "kwv"),
    ("kom", "kv"),
    ("kon", "kg"),
    ("kor", "ko"),
    ("krm", "bmf"),
    ("ktr", "dtp"),
    ("kua", "kj"),
    ("kur", "ku"),
    ("kvs", "gdj"),
    ("kwq", "yam"),
    ("kxe", "tvd"),
    ("kzj", "dtp"),
    ("kzt", "dtp"),
    ("lao", "lo"),
    ("lat", "la"),
    ("lav", "lv"),
    ("lii", "raq"),
    ("lim", "li"),
    ("lin", "ln"),
    ("lit", "lt"),
    ("lmm", "rmx"),
    ("ltz", "lb"),
    ("lub", "lu"),
    ("lug", "lg"),
    ("mah", "mh"),
    ("mal", "ml"),
    ("mar", "mr"),
    ("meg", "cir"),
    ("mkd", "mk"),
    ("mlg", "mg"),
    ("mlt", "mt"),
    ("mo", "ro"),
    ("mol", "ro"),
    ("mon", "mn"),
    ("mri", "mi"),
    ("msa", "ms"),
    ("mst", "mry"),
    ("mwj", "vaj"),
    ("mya", "my"),
    ("myt", "mry"),
    ("nad", "xny"),
    ("nau", "na"),
    ("nav", "nv"),
    ("nbl", "nr"),
    ("ncp", "kdz"),
    ("nde", "nd"),
    ("ndo", "ng"),
    ("nep", "ne"),
    ("nld", "nl"),
    ("nno", "nn"),
    ("nnx", "ngv"),
    ("nob", "nb"),
    ("nor", "no"),
    ("nts", "pij"),
    ("nya", "ny"),
    ("oci", "oc"),
    ("oji", "oj"),
    ("ori", "or"),
    ("orm", "om"),
    ("oss", "os"),
    ("oun", "vaj"),
    ("pan", "pa"),
    ("pcr", "adx"),
    ("pli", "pi"),
    ("pmc", "huw"),
    ("pmu", "phr"),
    ("pol", "pl"),
    ("por", "pt"),
    ("ppa", "bfy"),
    ("ppr", "lcq"),
    ("pry", "prt"),
    ("pus", "ps"),
    ("puz", "pub"),
    ("que", "qu"),
    ("roh", "rm"),
    ("ron", "ro"),
    ("run", "rn"),
    ("rus", "ru"),
    ("sag", "sg"),
    ("san", "sa"),
    ("sca", "hle"),
    ("sin", "si"),
    ("skk", "oyb"),
    ("slk", "sk"),
    ("slv", "sl"),
    ("sme", "se"),
    ("smo", "sm"),
    ("sna", "sn"),
    ("snd", "sd"),
    ("som", "so"),
    ("sot", "st"),
    ("spa", "es"),
    ("sqi", "sq"),
    ("srd", "sc"),
    ("srp", "sr"),
    ("ssw", "ss"),
    ("sun", "su"),
    ("swa", "sw"),
    ("swe", "sv"),
    ("tah", "ty"),
    ("tam", "ta"),
    ("tat", "tt"),
    ("tdu", "dtp"),
    ("tel", "te"),
    ("tgk", "tg"),
    ("tgl", "tl"),
    ("tha", "th"),
    ("thc", "tpo"),
    ("thx", "oyb"),
    ("tie", "ras"),
    ("tir", "ti"),
    ("tkk", "twm"),
    ("tlw", "weo"),
    ("tmp", "tyj"),
    ("tne", "kak"),
    ("tnf", "prs"),
    ("ton", "to"),
    ("tsf", "taj"),
    ("tsn", "tn"),
    ("tso", "ts"),
    ("tuk", "tk"),
    ("tur", "tr"),
    ("twi", "tw"),
    ("uig", "ug"),
    ("ukr", "uk"),
    ("uok", "ema"),
    ("urd", "ur"),
    ("uzb", "uz"),
    ("ven", "ve"),
    ("vie", "vi"),
    ("vol", "vo"),
    ("wln", "wa"),
    ("wol", "wo"),
    ("xba", "cax"),
    ("xho", "xh"),
    ("xia", "acn"),
    ("xkh", "waw"),
    ("xsj", "suj"),
    ("ybd", "rki"),
    ("yid", "yi"),
    ("yma", "lrr"),
    ("ymt", "mtm"),
    ("yor", "yo"),
    ("yos", "zom"),
    ("yuu", "yug"),
    ("zha", "za"),
    ("zho", "zh"),
    ("zul", "zu"),
];

/// 地区代码 → Node（ICU）使用的地区。对 Node 26.12.1 全量探测 2 字母 / 3 字母 / 3 位数字
/// 组合得到。key 可以是 alpha-2 旧代码（`BU`）、alpha-3（`BUR`）或数字代码。表外的 3 字母
/// 地区被丢弃，2 字母地区原样（大写）保留。
const REGION_ALIASES: &[(&str, &str)] = &[
    ("ABW", "AW"),
    ("AFG", "AF"),
    ("AGO", "AO"),
    ("AIA", "AI"),
    ("ALA", "AX"),
    ("ALB", "AL"),
    ("AND", "AD"),
    ("ANT", "AN"),
    ("ARE", "AE"),
    ("ARG", "AR"),
    ("ARM", "AM"),
    ("ASM", "AS"),
    ("ATA", "AQ"),
    ("ATF", "TF"),
    ("ATG", "AG"),
    ("AUS", "AU"),
    ("AUT", "AT"),
    ("AZE", "AZ"),
    ("BDI", "BI"),
    ("BEL", "BE"),
    ("BEN", "BJ"),
    ("BES", "BQ"),
    ("BFA", "BF"),
    ("BGD", "BD"),
    ("BGR", "BG"),
    ("BHR", "BH"),
    ("BHS", "BS"),
    ("BIH", "BA"),
    ("BLM", "BL"),
    ("BLR", "BY"),
    ("BLZ", "BZ"),
    ("BMU", "BM"),
    ("BOL", "BO"),
    ("BRA", "BR"),
    ("BRB", "BB"),
    ("BRN", "BN"),
    ("BTN", "BT"),
    ("BU", "MM"),
    ("BUR", "MM"),
    ("BVT", "BV"),
    ("BWA", "BW"),
    ("CAF", "CF"),
    ("CAN", "CA"),
    ("CCK", "CC"),
    ("CHE", "CH"),
    ("CHL", "CL"),
    ("CHN", "CN"),
    ("CIV", "CI"),
    ("CMR", "CM"),
    ("COD", "CD"),
    ("COG", "CG"),
    ("COK", "CK"),
    ("COL", "CO"),
    ("COM", "KM"),
    ("CPV", "CV"),
    ("CRI", "CR"),
    ("CRQ", "CQ"),
    ("CUB", "CU"),
    ("CUW", "CW"),
    ("CXR", "CX"),
    ("CYM", "KY"),
    ("CYP", "CY"),
    ("CZE", "CZ"),
    ("DD", "DE"),
    ("DEU", "DE"),
    ("DGA", "DG"),
    ("DJI", "DJ"),
    ("DMA", "DM"),
    ("DNK", "DK"),
    ("DOM", "DO"),
    ("DZA", "DZ"),
    ("ECU", "EC"),
    ("EGY", "EG"),
    ("ERI", "ER"),
    ("ESH", "EH"),
    ("ESP", "ES"),
    ("EST", "EE"),
    ("ETH", "ET"),
    ("FIN", "FI"),
    ("FJI", "FJ"),
    ("FLK", "FK"),
    ("FRA", "FR"),
    ("FRO", "FO"),
    ("FSM", "FM"),
    ("FX", "FR"),
    ("FXX", "FR"),
    ("GAB", "GA"),
    ("GBR", "GB"),
    ("GEO", "GE"),
    ("GGY", "GG"),
    ("GHA", "GH"),
    ("GIB", "GI"),
    ("GIN", "GN"),
    ("GLP", "GP"),
    ("GMB", "GM"),
    ("GNB", "GW"),
    ("GNQ", "GQ"),
    ("GRC", "GR"),
    ("GRD", "GD"),
    ("GRL", "GL"),
    ("GTM", "GT"),
    ("GUF", "GF"),
    ("GUM", "GU"),
    ("GUY", "GY"),
    ("HKG", "HK"),
    ("HMD", "HM"),
    ("HND", "HN"),
    ("HRV", "HR"),
    ("HTI", "HT"),
    ("HUN", "HU"),
    ("IDN", "ID"),
    ("IMN", "IM"),
    ("IND", "IN"),
    ("IOT", "IO"),
    ("IRL", "IE"),
    ("IRN", "IR"),
    ("IRQ", "IQ"),
    ("ISL", "IS"),
    ("ISR", "IL"),
    ("ITA", "IT"),
    ("JAM", "JM"),
    ("JEY", "JE"),
    ("JOR", "JO"),
    ("JPN", "JP"),
    ("KAZ", "KZ"),
    ("KEN", "KE"),
    ("KGZ", "KG"),
    ("KHM", "KH"),
    ("KIR", "KI"),
    ("KNA", "KN"),
    ("KOR", "KR"),
    ("KWT", "KW"),
    ("LAO", "LA"),
    ("LBN", "LB"),
    ("LBR", "LR"),
    ("LBY", "LY"),
    ("LCA", "LC"),
    ("LIE", "LI"),
    ("LKA", "LK"),
    ("LSO", "LS"),
    ("LTU", "LT"),
    ("LUX", "LU"),
    ("LVA", "LV"),
    ("MAC", "MO"),
    ("MAF", "MF"),
    ("MAR", "MA"),
    ("MCO", "MC"),
    ("MDA", "MD"),
    ("MDG", "MG"),
    ("MDV", "MV"),
    ("MEX", "MX"),
    ("MHL", "MH"),
    ("MKD", "MK"),
    ("MLI", "ML"),
    ("MLT", "MT"),
    ("MMR", "MM"),
    ("MNE", "ME"),
    ("MNG", "MN"),
    ("MNP", "MP"),
    ("MOZ", "MZ"),
    ("MRT", "MR"),
    ("MSR", "MS"),
    ("MTQ", "MQ"),
    ("MUS", "MU"),
    ("MWI", "MW"),
    ("MYS", "MY"),
    ("MYT", "YT"),
    ("NAM", "NA"),
    ("NCL", "NC"),
    ("NER", "NE"),
    ("NFK", "NF"),
    ("NGA", "NG"),
    ("NIC", "NI"),
    ("NIU", "NU"),
    ("NLD", "NL"),
    ("NOR", "NO"),
    ("NPL", "NP"),
    ("NRU", "NR"),
    ("NZL", "NZ"),
    ("OMN", "OM"),
    ("PAK", "PK"),
    ("PAN", "PA"),
    ("PCN", "PN"),
    ("PER", "PE"),
    ("PHL", "PH"),
    ("PLW", "PW"),
    ("PNG", "PG"),
    ("POL", "PL"),
    ("PRI", "PR"),
    ("PRK", "KP"),
    ("PRT", "PT"),
    ("PRY", "PY"),
    ("PSE", "PS"),
    ("PYF", "PF"),
    ("QAT", "QA"),
    ("REU", "RE"),
    ("ROM", "RO"),
    ("ROU", "RO"),
    ("RUS", "RU"),
    ("RWA", "RW"),
    ("SAU", "SA"),
    ("SCG", "CS"),
    ("SDN", "SD"),
    ("SEN", "SN"),
    ("SGP", "SG"),
    ("SGS", "GS"),
    ("SHN", "SH"),
    ("SJM", "SJ"),
    ("SLB", "SB"),
    ("SLE", "SL"),
    ("SLV", "SV"),
    ("SMR", "SM"),
    ("SOM", "SO"),
    ("SPM", "PM"),
    ("SRB", "RS"),
    ("SSD", "SS"),
    ("STP", "ST"),
    ("SUN", "SU"),
    ("SUR", "SR"),
    ("SVK", "SK"),
    ("SVN", "SI"),
    ("SWE", "SE"),
    ("SWZ", "SZ"),
    ("SXM", "SX"),
    ("SYC", "SC"),
    ("SYR", "SY"),
    ("TCA", "TC"),
    ("TCD", "TD"),
    ("TGO", "TG"),
    ("THA", "TH"),
    ("TJK", "TJ"),
    ("TKL", "TK"),
    ("TKM", "TM"),
    ("TLS", "TL"),
    ("TMP", "TL"),
    ("TON", "TO"),
    ("TP", "TL"),
    ("TTO", "TT"),
    ("TUN", "TN"),
    ("TUR", "TR"),
    ("TUV", "TV"),
    ("TWN", "TW"),
    ("TZA", "TZ"),
    ("UGA", "UG"),
    ("UKR", "UA"),
    ("UMI", "UM"),
    ("URY", "UY"),
    ("USA", "US"),
    ("UZB", "UZ"),
    ("VAT", "VA"),
    ("VCT", "VC"),
    ("VEN", "VE"),
    ("VGB", "VG"),
    ("VIR", "VI"),
    ("VNM", "VN"),
    ("VUT", "VU"),
    ("WLF", "WF"),
    ("WSM", "WS"),
    ("XEA", "EA"),
    ("XIC", "IC"),
    ("XKK", "XK"),
    ("YD", "YE"),
    ("YEM", "YE"),
    ("YMD", "YE"),
    ("YUG", "YU"),
    ("ZAF", "ZA"),
    ("ZAR", "CD"),
    ("ZMB", "ZM"),
    ("ZR", "CD"),
    ("ZWE", "ZW"),
];

/// locale 变体：[`Variant::Bcp`] 直接渲染成 `-variant`，[`Variant::Local`] 收集进
/// `-x-lvariant-*`，[`Variant::Posix`] 是 `posix` 标记（渲染规则见 [`render_locale`]）。
#[derive(Debug, Clone, PartialEq, Eq)]
enum Variant {
    Bcp(String),
    Local(String),
    Posix,
}

/// IANA “grandfathered” 语言标签 → Node 26.12.1（ICU 78.3）的默认 locale 解析结果。
///
/// 实测：`LC_ALL=<tag> node -p 'Intl.DateTimeFormat().resolvedOptions().locale'`。
/// 匹配时大小写不敏感、`_` 与 `-` 等价、忽略 `.codeset`；`@modifier` 保留并追加到
/// 替换结果（`zh-hakka@euro` → `hak-x-lvariant-euro`）。
///
/// 只列出常规解析会给出错误结果的条目：`en-GB-oed`、`sgn-BE-*`、`cel-gaulish` 由通用
/// 解析规则天然得到 Node 的结果，不需要别名。
const GRANDFATHERED_LOCALE_ALIASES: &[(&str, &str)] = &[
    ("art-lojban", "jbo"),
    ("i-ami", "ami"),
    ("i-bnn", "bnn"),
    ("i-default", "en"),
    ("i-enochian", "und"),
    ("i-hak", "hak"),
    ("i-klingon", "tlh"),
    ("i-lux", "lb"),
    ("i-mingo", "see"),
    ("i-navajo", "nv"),
    ("i-pwn", "pwn"),
    ("i-tao", "tao"),
    ("i-tay", "tay"),
    ("i-tsu", "tsu"),
    ("no-bok", "no"),
    ("no-nyn", "no"),
    ("zh-guoyu", "zh"),
    ("zh-hakka", "hak"),
    ("zh-min", "zh"),
    ("zh-min-nan", "nan"),
    ("zh-xiang", "hsn"),
];

/// `main`（已去掉 codeset）对应的 grandfathered 别名。
fn grandfathered_locale_alias(main: &str) -> Option<&'static str> {
    let normalized = main.to_ascii_lowercase().replace('_', "-");
    GRANDFATHERED_LOCALE_ALIASES
        .iter()
        .find(|(tag, _)| *tag == normalized)
        .map(|(_, target)| *target)
}

/// 名为 `value` 的 locale 值 → BCP-47 标记（Node 26 的 `Intl` 默认 locale）。
///
/// 空值 → `und`（ICU 的 root locale）。合法但 ICU4X 不认识的语言标记（`english`/`posix`
/// 等）原样返回，由 [`collator_for_locale`] 回退到 en-US。
///
/// 实现覆盖 POSIX 形式（`lang[_territory][.codeset][@modifier]`）与常见 BCP-47 形式
/// （`en-US`、`sr-Latn-RS`、`en-US-u-co-phonebk`、`en-US-x-lvariant-euro`）。`posix`
/// 关键字的渲染细节属于近似，只影响本函数返回的标记字符串。
///
/// ICU 拒绝的畸形变体组合（如 `en_US_abcd_abcd`，Node 直接抛 `RangeError`）不在这里
/// 处理，见 [`locale_is_node_valid`]。
fn normalize_locale(value: &str) -> String {
    if value.is_empty() {
        return "und".to_string();
    }

    // 阶段 1：拆出 `@modifier`（取最后一个 `@` 之后的整段）；主段的 codeset 从第一个
    // `.` 起全部丢弃。
    let at_index = value.find('@');
    let main_raw = match at_index {
        Some(index) => &value[..index],
        None => value,
    };
    // codeset（`.` 起）及之后的内容全部丢弃。
    let main = main_raw.split('.').next().unwrap_or(main_raw);
    let modifier = value.rsplit_once('@').map(|(_, rest)| rest).unwrap_or("");

    // 阶段 2：`C` / `POSIX` 关键字（大小写敏感，且整段没有分隔符）。
    if main == "C" || main == "POSIX" {
        if at_index.is_none() {
            // 没有 `@` → 普通 `en-US`。
            return "en-US".to_string();
        }
        let mut variants = parse_modifier_variants(modifier);
        if variants.is_empty() {
            // `C@` / `C@x=y`（keyword modifier）→ ICU 的 POSIX 标记。
            return "en-US-u-va-posix".to_string();
        }
        variants.push(Variant::Posix);
        // 单字符 modifier 是“localvar 槽位竞争”的特例：`posix` 排在前面当 BCP 变体
        // （`C@x` → `en-US-posix-x-lvariant-x`）；更长时按普通 localvar 排序
        // （`C@euro` → `en-US-x-lvariant-euro-posix`）。
        let posix_as_bcp = variants
            .iter()
            .any(|variant| matches!(variant, Variant::Local(value) if value.len() == 1));
        return render_locale("en", None, Some("US"), variants, posix_as_bcp);
    }

    // `c`（小写）单独出现时也是 en-US（ICU 对 `C` 的大小写无关别名）；带 modifier 时按
    // 普通语言解析（`c@euro` → `und-x-lvariant-euro`）。
    if main == "c" && at_index.is_none() {
        return "en-US".to_string();
    }

    // 阶段 3：IANA grandfathered 标签。ICU 在常规解析前做别名替换（`zh-min-nan` →
    // `nan`、`i-klingon` → `tlh`），`@modifier` 保留并追加到替换结果。
    let main = grandfathered_locale_alias(main).unwrap_or(main);

    // 阶段 4：按 `_`/`-` 切字段，依次消耗语言、脚本、地区三个槽位；剩下的全部当变体。
    let fields: Vec<&str> = main.split(['_', '-']).collect();
    let mut index = 0;

    // 语言槽位：第一个字段总被消耗；`und` 表示“无有效语言”。单字符字段不是语言，且它
    // 会让后面的字段直接进变体槽位（`a_b` → `und-x-lvariant-b`，`x-y` → `und`）。
    let mut language = "und".to_string();
    if let Some(field) = fields.first()
        && is_valid_language(field)
    {
        language = canonical_language(field);
    }
    if !fields.is_empty() {
        index = 1;
    }
    // 首字段是单字符 `x`（BCP-47 private-use singleton）时整段作废（`x-y` → `und`）。
    let language_is_und = language == "und";

    // 脚本槽位：语言之后恰好 4 个 ASCII 字母。
    let mut script: Option<String> = None;
    if let Some(field) = fields.get(index)
        && is_script(field)
    {
        script = Some(title_case(field));
        index += 1;
    }

    // 地区槽位：只要字段是 2~3 位字母数字就消耗掉，非法时丢弃（`en_abc` → `en`、
    // `en_12` → `en`、`12_34` → `und`）。
    if language_is_und && fields.first() == Some(&"x") {
        return "und".to_string();
    }

    let mut region: Option<String> = None;
    if let Some(field) = fields.get(index)
        && looks_like_region(field)
    {
        region = canonical_region(field);
        index += 1;
    }

    // 阶段 5：主段剩余字段与 `@modifier` 各自解析成变体（两者对单字符字段的规则不同），
    // 合并后统一交给 render_locale。
    let mut variants = parse_variant_fields(&fields[index..]);
    let modifier_variants = parse_modifier_variants(modifier);
    // modifier 是独立的一组：单字符规则也单独套用。
    variants.extend(modifier_variants);
    render_locale(
        &language,
        script.as_deref(),
        region.as_deref(),
        variants,
        false,
    )
}

/// 字段是否落在地区槽位：2~3 位字母数字（含纯数字）。
fn looks_like_region(field: &str) -> bool {
    (field.len() == 2 || field.len() == 3) && field.chars().all(|ch| ch.is_ascii_alphanumeric())
}

/// 主变体段（地区之后的所有字段）。
///
/// ICU 的宽松规则：
/// - 单字符字段是 BCP-47 singleton / 非法变体：`u-co-*` 折叠保留（Node 的 locale 字符串
///   保留 collation 扩展），`x-lvariant-*` 原样保留，其余整段丢弃。
/// - 组内出现单字符字段且不是唯一字段 → 整组作废（`en_US_a_b`/`en_US_abcd_1` → `en-US`）；
///   `en_US_1` 这类唯一字段仍按本地变体保留。
/// - 其它字段按 [`canonical_variant`] 分类，9 位以上丢弃。
fn parse_variant_fields(fields: &[&str]) -> Vec<Variant> {
    if fields.is_empty() {
        return Vec::new();
    }

    // singleton / 扩展形式。
    if let Some(first) = fields.first()
        && first.len() == 1
        && fields.len() > 1
    {
        let rest = &fields[1..];
        return match (*first, rest.first().copied()) {
            ("u", Some("co")) => {
                let mut extension = String::from("u-co");
                for part in &rest[1..] {
                    extension.push('-');
                    extension.push_str(part);
                }
                vec![Variant::Bcp(extension)]
            }
            ("x", Some("lvariant")) => {
                let mut extension = String::new();
                for part in &rest[1..] {
                    if part.is_empty() {
                        continue;
                    }
                    if !extension.is_empty() {
                        extension.push('-');
                    }
                    extension.push_str(part);
                }
                if extension.is_empty() {
                    Vec::new()
                } else {
                    vec![Variant::Local(extension)]
                }
            }
            _ => Vec::new(),
        };
    }

    let mut classified: Vec<Option<Variant>> = fields
        .iter()
        .map(|field| canonical_variant(field))
        .collect();

    // 单字符字段（非 singleton 扩展）只有在它是唯一字段、或前面已有 BCP 变体时才合法；
    // 否则整组作废（`en_US_a_b`、`en_US_12_1`、`en_US_abcd_1` 都是这种）。
    if fields.len() > 1 {
        let mut seen_bcp = false;
        for (index, field) in fields.iter().enumerate() {
            if field.len() == 1 {
                if !seen_bcp {
                    return Vec::new();
                }
                classified[index] = Some(Variant::Local(field.to_ascii_lowercase()));
            }
            if matches!(classified[index], Some(Variant::Bcp(_))) {
                seen_bcp = true;
            }
        }
    }

    classified.into_iter().flatten().collect()
}

/// `@modifier` → 变体（`=` 形式是 keyword，不产生变体；按 `_` 拆段，`.` 之后丢弃）。
fn parse_modifier_variants(modifier: &str) -> Vec<Variant> {
    let modifier = modifier.split('.').next().unwrap_or(modifier);
    if modifier.is_empty() || modifier.contains('=') {
        return Vec::new();
    }
    // 与主变体段不同，modifier 里的单字符字段只要**不在最前面**就保留
    // （`de_DE@euro_x` → `euro-x`），出现在最前面则整段作废（`en_US@i_abcdefgh` → 空）。
    let fields: Vec<&str> = modifier
        .split('_')
        .filter(|field| !field.is_empty())
        .collect();
    match fields.first() {
        Some(first) if first.len() == 1 && fields.len() > 1 => Vec::new(),
        _ => fields
            .iter()
            .filter_map(|field| canonical_variant(field))
            .collect(),
    }
}

/// 一个变体组里参与 ICU 去重检查的变体（小写，`(值, 是否为 BCP 变体)`）。
///
/// 参与检查的是 localvar（1~4 位，单字符除外）与 BCP 变体（5~8 位或首位是数字的
/// 4 位）。ICU 会整体丢弃“不合法”的变体组：组里出现两个及以上单字符字段，或单字符
/// 字段不在末尾（实测 `de_DE@euro_x` 保留，`de_DE@euro_x_y`、`de_DE@x_euro_euro`、
/// `en_US_abcd_x_y` 被丢弃）。被丢弃的组既不参与去重也不报错，因此返回 `None`。
fn valid_variant_uses(fields: &[&str]) -> Option<Vec<(String, bool)>> {
    let single_positions: Vec<usize> = fields
        .iter()
        .enumerate()
        .filter(|(_, field)| field.len() == 1)
        .map(|(index, _)| index)
        .collect();
    if let Some(&last) = single_positions.last()
        && (single_positions.len() > 1 || last + 1 != fields.len())
    {
        return None;
    }
    let mut variants = Vec::new();
    for field in fields {
        match canonical_variant(field) {
            Some(Variant::Local(value)) if field.len() >= 2 => variants.push((value, false)),
            Some(Variant::Bcp(value)) => variants.push((value, true)),
            _ => {}
        }
    }
    Some(variants)
}

/// Node/ICU 是否接受该 locale 作为**默认 locale**（`Intl` 初始化不抛 `RangeError`）。
///
/// 实测（Node 26.12.1）：ICU 在重复的 **localvar** 变体上抛
/// `RangeError: Internal error. Icu error.`（`en_US_abcd_abcd`、`en_US_abcd@abcd`、
/// `de_DE@euro_euro`）。重复的 BCP 变体只有在它**前面**出现过 localvar 时才抛
/// （`en_US_abcd@1901_1901` 抛；`en_US_1901_1901`、`en_US_1901_1901_abcd` 去重后正常）。
/// 主变体段与 `@modifier` 的变体按出现顺序合并后检查（跨组重复同样生效，
/// 如 `en_US_abcd@efgh_abcd`）；被丢弃的变体组不参与。
///
/// 这是对 ICU locale 规范化的近似，完整复刻 ICU 对畸形输入的行为不在范围内。
fn locale_is_node_valid(value: &str) -> bool {
    if value.is_empty() {
        return true;
    }

    let at_index = value.find('@');
    let main_raw = match at_index {
        Some(index) => &value[..index],
        None => value,
    };
    let main_raw = main_raw.split('.').next().unwrap_or(main_raw);
    let modifier = value.rsplit_once('@').map(|(_, rest)| rest).unwrap_or("");

    // `C` / `POSIX` / `c` 关键字由 ICU 特殊处理，总是合法。
    if main_raw == "C" || main_raw == "POSIX" || (main_raw == "c" && at_index.is_none()) {
        return true;
    }

    let main = grandfathered_locale_alias(main_raw).unwrap_or(main_raw);
    let fields: Vec<&str> = main.split(['_', '-']).collect();
    if fields.first() == Some(&"x") {
        // 首字段是 private-use singleton：整段作废为 `und`。
        return true;
    }

    // 与 `normalize_locale` 相同的槽位消耗：语言、脚本、地区。
    let mut index = usize::from(!fields.is_empty());
    if let Some(field) = fields.get(index)
        && is_script(field)
    {
        index += 1;
    }
    if let Some(field) = fields.get(index)
        && looks_like_region(field)
    {
        index += 1;
    }

    // 只收集参与 ICU 去重检查的变体；被整体丢弃的变体组由 valid_variant_uses 返回 None，
    // 不参与后面的重复判断。
    let mut variants: Vec<(String, bool)> = Vec::new();
    if let Some(mut main_variants) = valid_variant_uses(&fields[index..]) {
        variants.append(&mut main_variants);
    }

    let modifier = modifier.split('.').next().unwrap_or(modifier);
    if !modifier.is_empty() && !modifier.contains('=') {
        let modifier_fields: Vec<&str> = modifier
            .split('_')
            .filter(|field| !field.is_empty())
            .collect();
        if let Some(mut modifier_variants) = valid_variant_uses(&modifier_fields) {
            variants.append(&mut modifier_variants);
        }
    }

    // localvar 重复（大小写不敏感）总是非法。
    let mut local_values: Vec<&str> = variants
        .iter()
        .filter(|(_, is_bcp)| !is_bcp)
        .map(|(value, _)| value.as_str())
        .collect();
    local_values.sort_unstable();
    if local_values.windows(2).any(|pair| pair[0] == pair[1]) {
        return false;
    }

    // BCP 变体重复：先找该值最后一次出现的下标；只有当它确实重复、且更靠前的位置上
    // 出现过 localvar 时才非法（实测规则，见函数文档）。
    let local_positions: Vec<usize> = variants
        .iter()
        .enumerate()
        .filter(|(_, (_, is_bcp))| !is_bcp)
        .map(|(index, _)| index)
        .collect();
    for (position, (value, is_bcp)) in variants.iter().enumerate() {
        if !is_bcp {
            continue;
        }
        let last_occurrence = variants
            .iter()
            .rposition(|(other, other_is_bcp)| *other_is_bcp && other == value)
            .unwrap_or(position);
        if last_occurrence > position
            && local_positions.iter().any(|&local| local < last_occurrence)
        {
            return false;
        }
    }

    true
}

/// 语言段：2~8 个 ASCII 字母（ICU 同时接受 2~3 字母代码与 `english` 这类 4~8 字母
/// legacy 代码；4 字母字段只有在**语言槽位**才是语言，在语言之后是脚本）。
fn is_valid_language(field: &str) -> bool {
    let len = field.len();
    (2..=8).contains(&len) && field.chars().all(|ch| ch.is_ascii_alphabetic())
}

/// 语言段规范形（小写 + [`LANGUAGE_ALIASES`]）。
fn canonical_language(field: &str) -> String {
    let lower = field.to_ascii_lowercase();
    LANGUAGE_ALIASES
        .iter()
        .find(|(from, _)| *from == lower)
        .map(|(_, to)| (*to).to_string())
        .unwrap_or(lower)
}

/// 脚本段：恰好 4 个 ASCII 字母。
fn is_script(field: &str) -> bool {
    field.len() == 4 && field.chars().all(|ch| ch.is_ascii_alphabetic())
}

/// `Abcd` 形式。
fn title_case(field: &str) -> String {
    let lower = field.to_ascii_lowercase();
    let mut chars = lower.chars();
    match chars.next() {
        Some(first) => format!("{}{}", first.to_ascii_uppercase(), chars.as_str()),
        None => String::new(),
    }
}

/// 地区段：2 个字母（大写 + 别名）、3 位数字，或 ISO 3166-1 alpha-3（别名表内的才有效）。
fn canonical_region(field: &str) -> Option<String> {
    let all_alpha = field.chars().all(|ch| ch.is_ascii_alphabetic());
    if field.len() == 2 && all_alpha {
        let upper = field.to_ascii_uppercase();
        return Some(
            REGION_ALIASES
                .iter()
                .find(|(from, _)| *from == upper)
                .map(|(_, to)| (*to).to_string())
                .unwrap_or(upper),
        );
    }
    if field.len() == 3 && field.chars().all(|ch| ch.is_ascii_digit()) {
        return Some(field.to_string());
    }
    if field.len() == 3 && all_alpha {
        let upper = field.to_ascii_uppercase();
        return REGION_ALIASES
            .iter()
            .find(|(from, _)| *from == upper)
            .map(|(_, to)| (*to).to_string());
    }
    None
}

/// 变体分类：5~8 位字母数字或首位是数字的 4 位 → [`Variant::Bcp`]；1~4 位其余 →
/// [`Variant::Local`]（ICU 的 localvar）；`posix` 单独标记；9 位以上 → `None`（丢弃）。
fn canonical_variant(field: &str) -> Option<Variant> {
    if field.is_empty() || !field.chars().all(|ch| ch.is_ascii_alphanumeric()) {
        return None;
    }
    let lower = field.to_ascii_lowercase();
    if lower == "posix" {
        return Some(Variant::Posix);
    }
    let len = field.len();
    if (5..=8).contains(&len)
        || (len == 4 && field.chars().next().is_some_and(|ch| ch.is_ascii_digit()))
    {
        return Some(Variant::Bcp(lower));
    }
    if (1..=4).contains(&len) {
        return Some(Variant::Local(lower));
    }
    None
}

/// 组装 BCP-47 标记：`lang[-Script][-REGION][-variant...][-x-lvariant-...]`。
///
/// `posix` 标记见 [`normalize_locale`] 的说明：`en-US` 的 `posix` 是默认值，直接省略；
/// 其它语言渲染成 `-u-va-posix`；与 BCP 变体并存时作为普通变体（`en-US-1901-posix`）；
/// 与本地变体并存时并入 `-x-lvariant-*`（`en-US-x-lvariant-abcd-posix`）。
fn render_locale(
    language: &str,
    script: Option<&str>,
    region: Option<&str>,
    variants: Vec<Variant>,
    posix_as_bcp: bool,
) -> String {
    let mut bcp_variants: Vec<String> = Vec::new();
    let mut local_variants: Vec<String> = Vec::new();
    let mut has_posix = posix_as_bcp;
    for variant in variants {
        match variant {
            Variant::Bcp(value) => bcp_variants.push(value),
            Variant::Local(value) => local_variants.push(value),
            Variant::Posix => has_posix = true,
        }
    }
    // Node 输出的变体按字典序排列（见模块文档的实测规则），所以两类变体分别排序并去重；
    // `posix_as_bcp` 的 posix 也并入 BCP 变体一起排序。
    bcp_variants.sort();
    bcp_variants.dedup();
    local_variants.sort();
    local_variants.dedup();

    if posix_as_bcp {
        bcp_variants.push("posix".to_string());
        bcp_variants.sort();
        has_posix = false;
    }

    let mut posix_suffix = "";
    if has_posix {
        if bcp_variants.is_empty() && local_variants.is_empty() {
            if language == "en" && region == Some("US") {
                // `en-US` 的默认就是 POSIX，直接省略（`en_US_posix` → `en-US`）。
            } else {
                posix_suffix = "-u-va-posix";
            }
        } else if !bcp_variants.is_empty() {
            bcp_variants.push("posix".to_string());
            bcp_variants.sort();
        } else {
            local_variants.push("posix".to_string());
            local_variants.sort();
        }
    }

    let mut tag = String::from(language);
    if let Some(script) = script {
        tag.push('-');
        tag.push_str(script);
    }
    if let Some(region) = region {
        tag.push('-');
        tag.push_str(region);
    }
    for variant in bcp_variants {
        tag.push('-');
        tag.push_str(&variant);
    }
    if !local_variants.is_empty() {
        tag.push_str("-x-lvariant-");
        tag.push_str(&local_variants.join("-"));
    }
    tag.push_str(posix_suffix);
    tag
}

/// 查 collator 前对 locale 做的最后一层映射（Node 在 collation 查表时的等价类）。
fn collation_locale(locale: &str) -> &str {
    // Node 的默认 locale 字符串保留 `sh-*`，但 collation 数据按 `sr-Latn` 取。
    if locale == "sh" || locale.starts_with("sh-") {
        "sr-Latn"
    } else {
        locale
    }
}

/// Node（ICU 78.3）的 collation 数据是**语言优先**回退：带 script 的标签只有在该
/// language-script 组合有专用数据时才使用 script，否则退回语言级数据
/// （`ja-Latn` → `ja`、`zh-Latn` → `zh`）。ICU4X 的 collator 数据标记用
/// `LocaleFallbackPriority::Script`（`ja-Latn` → `und-Latn` → `und`），会把这些标签
/// 落到 root。对 Node 26.12.1 全量探测（34 种语言 × 33 种 script）后，语言级与
/// 脚本级排序不同的组合只有：`bs-Cyrl`、`kk-Arab`、`sr-Latn`、`zh-Hant`
/// （`zh-Hans` 与 `zh` 相同）。其余 script 在查表前丢弃。
fn collation_keeps_script(language: &str, script: &str) -> bool {
    matches!(
        (language, script),
        ("bs", "Cyrl") | ("kk", "Arab") | ("sr", "Latn") | ("zh", "Hant")
    )
}

fn collator_for_locale(locale: &str) -> Option<icu_collator::CollatorBorrowed<'static>> {
    let mut parsed: Locale = match collation_locale(locale).parse() {
        Ok(locale) => locale,
        // ICU4X 不认识的语言标记（`english`/`posix` 等）：Node 用 root 数据，root 与 en-US
        // 在所有实测样本上一致。
        Err(_) => "en-US".parse().expect("en-US parses"),
    };
    if let Some(script) = parsed.id.script
        && !collation_keeps_script(parsed.id.language.as_str(), script.as_str())
    {
        parsed.id.script = None;
    }
    let preferences = CollatorPreferences::from(&parsed);
    let mut options = CollatorOptions::default();
    // 第三级比较让大小写与重音参与排序（测试 locale_compare_matches_icu_expectations
    // 锁定了 `"a" < "A"`、`"a" < "á"`）。
    options.strength = Some(Strength::Tertiary);
    // `alternate_handling` 保持 `None`（= 跟随 locale 数据），见文件头注释。
    Collator::try_new(preferences, options).ok()
}

/// 进程默认 locale 的原始值：`LC_ALL` → `LC_MESSAGES` → `LANG` 中第一个存在的变量
/// （值即使为空也胜出）。
fn process_locale_env_value() -> Option<String> {
    for key in ["LC_ALL", "LC_MESSAGES", "LANG"] {
        if let Some(value) = std::env::var_os(key) {
            return Some(value.to_string_lossy().into_owned());
        }
    }
    None
}

/// 进程默认 locale（BCP-47 标记），按 `normalize_locale` 的规则解析。
///
/// 取 `LC_ALL` → `LC_MESSAGES` → `LANG` 中**第一个存在的**变量（值即使为空也胜出）；
/// 三个都不存在时用 `en-US`（ICU 的编译期默认）。
pub fn resolve_process_locale() -> String {
    match process_locale_env_value() {
        Some(value) => normalize_locale(&value),
        None => "en-US".to_string(),
    }
}

/// 进程默认 locale 是否被 Node/ICU 接受。
///
/// `false` 时 `String.prototype.localeCompare` 抛 `RangeError`，TS 的
/// `getFileSuggestions` / `getFuzzyFileSuggestions` 因外层 `try/catch` 返回空列表。
/// 三个环境变量都不存在时视为有效（Node 用编译期默认 locale）；结果用 `OnceLock`
/// 在进程内缓存，第一次调用之后修改环境变量不会改变返回值。
pub fn process_locale_is_valid() -> bool {
    static VALID: OnceLock<bool> = OnceLock::new();
    *VALID
        .get_or_init(|| process_locale_env_value().is_none_or(|value| locale_is_node_valid(&value)))
}

/// 进程默认 locale 的 collator（首次调用时建立）。
fn process_collator() -> Option<&'static icu_collator::CollatorBorrowed<'static>> {
    static COLLATOR: OnceLock<Option<icu_collator::CollatorBorrowed<'static>>> = OnceLock::new();
    COLLATOR
        .get_or_init(|| collator_for_locale(&resolve_process_locale()))
        .as_ref()
}

/// `a.localeCompare(b)`：运行时默认 locale 的 ICU collation。
///
/// 无法建立 collator 时回退字节序（ARCHITECTURE.md §8 第 27 条的约定）。
pub fn locale_compare(a: &str, b: &str) -> Ordering {
    match process_collator() {
        Some(collator) => collator.compare(a, b),
        None => a.cmp(b),
    }
}

/// 指定 locale 的 `localeCompare`（与进程环境无关的确定性断言使用）。
///
/// `locale` 先按 `normalize_locale` 的规则解析（`en_US.UTF-8` → `en-US`，空串 →
/// `und`），因此可以直接传 POSIX 形式。无法建立 collator 时与 [`locale_compare`]
/// 一样回退到字节序。
pub fn locale_compare_with(locale: &str, a: &str, b: &str) -> Ordering {
    match collator_for_locale(&normalize_locale(locale)) {
        Some(collator) => collator.compare(a, b),
        None => a.cmp(b),
    }
}

// ============================================================================
// fd 目录遍历（spec §2.10.3）
// ============================================================================

/// `walkDirectoryWithFd` 的一条结果。
#[derive(Debug, Clone, PartialEq, Eq)]
struct FdEntry {
    path: String,
    is_directory: bool,
}

/// `walkDirectoryWithFd` 的 argv（顺序敏感，逐字照搬 TS）。
fn build_fd_args(
    base_dir: &str,
    query: &str,
    max_results: usize,
    max_depth: Option<usize>,
) -> Vec<String> {
    // 参数顺序是 TS 的逐字顺序（单元测试 fd_args_match_ts_order 锁定）：
    // 固定选项 → 可选 `--max-depth` → 含 `/` 时追加 `--full-path` → 非空查询的正则。
    // `.git` 用三条 exclude 覆盖目录本身及其内容。
    let mut args: Vec<String> = vec![
        "--base-directory".into(),
        base_dir.into(),
        "--max-results".into(),
        max_results.to_string(),
        "--type".into(),
        "f".into(),
        "--type".into(),
        "d".into(),
        "--follow".into(),
        "--hidden".into(),
        "--exclude".into(),
        ".git".into(),
        "--exclude".into(),
        ".git/*".into(),
        "--exclude".into(),
        ".git/**".into(),
    ];

    if let Some(depth) = max_depth {
        args.push("--max-depth".into());
        args.push(depth.to_string());
    }

    if to_display_path(query).contains('/') {
        args.push("--full-path".into());
    }

    if !query.is_empty() {
        args.push(build_fd_path_query(query));
    }

    args
}

/// 解析 fd 的 stdout（`stdout.trim().split("\n").filter(Boolean)` + `.git` 过滤）。
fn parse_fd_stdout(stdout: &str) -> Vec<FdEntry> {
    // 每行一条相对路径；末尾 `/` 是目录标记（测试锁定 `src/` → `is_directory`）。
    // `.git` 过滤在**去掉尾斜杠之后**做，只认 `.git`、`.git/…` 或包含 `/.git/` 的路径：
    // 因此 `foo/.git/` 归一化为 `foo/.git` 后不会命中，仍被保留（TS 的逐字行为，
    // 测试 parse_fd_stdout_filters_git_and_marks_directories 已锁定）。
    let trimmed = trim_js(stdout);
    if trimmed.is_empty() {
        return Vec::new();
    }

    let mut results = Vec::new();
    for line in trimmed.split('\n').filter(|line| !line.is_empty()) {
        let display_line = to_display_path(line);
        let has_trailing_separator = display_line.ends_with('/');
        let normalized_path = if has_trailing_separator {
            &display_line[..display_line.len() - 1]
        } else {
            display_line.as_str()
        };
        if normalized_path == ".git"
            || normalized_path.starts_with(".git/")
            || normalized_path.contains("/.git/")
        {
            continue;
        }

        results.push(FdEntry {
            path: display_line,
            is_directory: has_trailing_separator,
        });
    }
    results
}

/// `walkDirectoryWithFd(baseDir, fdPath, query, maxResults, signal, maxDepth?)`。
async fn walk_directory_with_fd(
    base_dir: &str,
    fd_path: &str,
    query: &str,
    max_results: usize,
    signal: &tokio_util::sync::CancellationToken,
    max_depth: Option<usize>,
) -> Vec<FdEntry> {
    if signal.is_cancelled() {
        return Vec::new();
    }

    let args = build_fd_args(base_dir, query, max_results, max_depth);
    let child = tokio::process::Command::new(fd_path)
        .args(&args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        // 取消时丢弃 future 即 SIGKILL 子进程（TS 的 `child.kill("SIGKILL")`）。
        .kill_on_drop(true)
        .spawn();

    let child = match child {
        Ok(child) => child,
        Err(_) => return Vec::new(),
    };

    // 与取消令牌竞争：取消分支胜出时 `child` 在此被丢弃，由 `kill_on_drop(true)` 终止
    // 子进程。`wait_with_output` 的 IO 错误与非零退出码同样折叠成空表；只有成功退出才
    // 解析 stdout。
    tokio::select! {
        _ = signal.cancelled() => Vec::new(),
        output = child.wait_with_output() => {
            match output {
                Ok(output) if output.status.success() => {
                    let stdout = String::from_utf8_lossy(&output.stdout);
                    parse_fd_stdout(&stdout)
                }
                // `code !== 0` 或 spawn 期间的 IO 错误 → []
                _ => Vec::new(),
            }
        }
    }
}

// ============================================================================
// CombinedAutocompleteProvider
// ============================================================================

/// 同时处理斜杠命令与文件路径的 provider。TS: `class CombinedAutocompleteProvider`。
///
/// 三个字段就是全部输入：`commands` 是命令表（空前缀时命令名补全保留表顺序），
/// `base_path` 是相对路径的解析根（交互模式传入会话 cwd），`fd_path` 是 `fd` 可执行
/// 文件路径；为 `None` 时 `@` 模糊补全一律返回空。
pub struct CombinedAutocompleteProvider {
    commands: Vec<CommandEntry>,
    base_path: String,
    fd_path: Option<String>,
}

impl CombinedAutocompleteProvider {
    /// `new CombinedAutocompleteProvider(commands = [], basePath, fdPath = null)`。
    ///
    /// `base_path` 作为相对路径的解析根；`fd_path` 为 `None` 时 `@` 模糊补全不可用
    /// （同步 `readdir` 的普通路径补全不受影响）。
    pub fn new(
        commands: Vec<CommandEntry>,
        base_path: impl Into<String>,
        fd_path: Option<String>,
    ) -> Self {
        Self {
            commands,
            base_path: base_path.into(),
            fd_path,
        }
    }

    /// 相对路径的解析根（构造时传入的会话 cwd）。
    pub fn base_path(&self) -> &str {
        &self.base_path
    }

    /// `fd` 可执行文件路径；`None` 表示未配置（`@` 模糊补全会返回空）。
    pub fn fd_path(&self) -> Option<&str> {
        self.fd_path.as_deref()
    }

    // ------------------------------------------------------------------
    // getSuggestions（spec §2.10.4）
    // ------------------------------------------------------------------

    /// `getSuggestions` 的实现（trait 方法把它的 future 装箱）。
    async fn get_suggestions_impl(
        &mut self,
        lines: &[String],
        cursor_line: usize,
        cursor_col: usize,
        options: &GetSuggestionsOptions,
    ) -> Option<AutocompleteSuggestions> {
        let current_line = lines.get(cursor_line).map(String::as_str).unwrap_or("");
        // 越界的 cursor_line 按空行处理（TS 的 `lines[cursorLine] || ""`）；列号按 UTF-16
        // 截断，因此光标落在代理对中间时得到 U+FFFD，与 JS 的 slice 结果对应。
        let text_before_cursor = slice_prefix(current_line, cursor_col as i64);

        // (1) `@` 前缀优先于斜杠命令：`@` 出现在任意 token 边界都先尝试文件补全。
        if let Some(at_prefix) = self.extract_at_prefix(&text_before_cursor) {
            // `@` 与可能的开引号属于 prefix 的一部分：搜索用剥掉前缀的 `rawPrefix`，
            // 返回给调用方的 prefix 仍是原文，apply_completion 才能正确回退光标。
            let parsed = parse_path_prefix(&at_prefix);
            let suggestions = self
                .get_fuzzy_file_suggestions(
                    &parsed.raw_prefix,
                    parsed.is_quoted_prefix,
                    &options.signal,
                )
                .await;
            if suggestions.is_empty() {
                return None;
            }

            return Some(AutocompleteSuggestions {
                items: suggestions,
                prefix: at_prefix,
            });
        }

        // (2) 斜杠命令（`force` 时整段跳过）。
        let command_text = trim_start_js(&text_before_cursor).to_string();
        if !options.force && command_text.starts_with('/') {
            // 只看第一个空格：没有空格 → 命令名补全；有空格 → 参数补全。命令名是
            // `/` 之后到空格之前的部分，参数从空格之后原样取（含后续空格）。
            let space_index = find_char_index(&command_text, ' ');

            if space_index == -1 {
                // 局部 `prefix` 是模糊匹配查询串（去掉前导 `/`）；返回给调用方的
                // `AutocompleteSuggestions::prefix` 是完整的 `command_text`（保留 `/`）。
                let prefix = slice_suffix(&command_text, 1);
                let command_items: Vec<CommandItem> = self
                    .commands
                    .iter()
                    .map(|cmd| {
                        let name = cmd.name().to_string();
                        let hint = cmd.argument_hint();
                        let desc = cmd.description().unwrap_or("");
                        // 下拉说明：argumentHint 与 description 都存在时拼成
                        // `"hint — desc"`，否则用非空的那一个；都为空 → None。
                        let full_desc = match hint {
                            Some(hint) if !hint.is_empty() => {
                                if desc.is_empty() {
                                    hint.to_string()
                                } else {
                                    format!("{hint} \u{2014} {desc}")
                                }
                            }
                            _ => desc.to_string(),
                        };
                        CommandItem {
                            name: name.clone(),
                            label: name,
                            description: if full_desc.is_empty() {
                                None
                            } else {
                                Some(full_desc)
                            },
                        }
                    })
                    .collect();

                // `skill:` 命令做两轮匹配（#9944 回归的语义，夹具见
                // `tests/autocomplete_skill_slash.rs`）：
                // 1. 所有命令先按“去掉 `skill:` 前缀”的名字做模糊匹配，命中项排前面；
                // 2. 未在第一轮出现的 `skill:` 命令再按完整名匹配，追加到后面。
                // 因此输入 `idea` 能命中 `skill:research-idea`，输入 `skill:idea` 也能命中。
                let bare_name_matches = fuzzy_filter(command_items.clone(), &prefix, |item| {
                    if let Some(rest) = item.name.strip_prefix("skill:") {
                        rest.to_string()
                    } else {
                        item.name.clone()
                    }
                });
                let bare_name_match_set: HashSet<&str> = bare_name_matches
                    .iter()
                    .map(|item| item.name.as_str())
                    .collect();
                let full_name_only_matches = fuzzy_filter(
                    command_items
                        .iter()
                        .filter(|item| {
                            item.name.starts_with("skill:")
                                && !bare_name_match_set.contains(item.name.as_str())
                        })
                        .cloned()
                        .collect(),
                    &prefix,
                    |item| item.name.clone(),
                );
                let filtered: Vec<AutocompleteItem> = bare_name_matches
                    .into_iter()
                    .chain(full_name_only_matches)
                    .map(|item| AutocompleteItem {
                        value: item.name,
                        label: item.label,
                        description: item.description,
                    })
                    .collect();

                if filtered.is_empty() {
                    return None;
                }

                // prefix 已经 trimStart：前导空白不在其中，apply_completion 通过
                // “prefix 之前只剩空白”识别行首命令补全。
                return Some(AutocompleteSuggestions {
                    items: filtered,
                    prefix: command_text.clone(),
                });
            }

            let command_name = slice_suffix(&slice_prefix(&command_text, space_index), 1);
            let argument_text = slice_suffix(&command_text, space_index + 1);

            // 命令名必须精确匹配；找不到命令、或命令没有提供参数补全闭包，都返回 None
            // （对应 TS 的 `find` + 属性存在性探测）。
            let command = self
                .commands
                .iter()
                .find(|cmd| cmd.name() == command_name)?;
            let get_completions = command.get_argument_completions()?;

            // 闭包返回 `None` 与空列表都算“无建议”。
            let items = get_completions(&argument_text).await?;
            if items.is_empty() {
                return None;
            }

            return Some(AutocompleteSuggestions {
                items,
                prefix: argument_text,
            });
        }

        // (3) 路径前缀（Tab 的 force 路径也在这里）：force 时无条件抽取，否则只接受
        // 含 `/`、以 `.` 开头或以 `~/` 开头的前缀。这里走同步 readdir，不使用 fd。
        let path_match = self.extract_path_prefix(&text_before_cursor, options.force);
        let path_match = path_match?;

        let suggestions = self.get_file_suggestions(&path_match);
        if suggestions.is_empty() {
            return None;
        }

        Some(AutocompleteSuggestions {
            items: suggestions,
            prefix: path_match,
        })
    }

    // ------------------------------------------------------------------
    // applyCompletion（spec §2.10.5）
    // ------------------------------------------------------------------

    /// `applyCompletion`。
    ///
    /// 输入整个缓冲区与当前光标；`prefix` 必须是 [`AutocompleteSuggestions::prefix`] 的原值。
    /// 返回替换后的缓冲区与光标（UTF-16 列）。没有失败路径：任何上下文都至少返回原文本的
    /// 副本。在路径/参数上下文里，`item.value` 为空字符串等价于删掉 `prefix`；斜杠命令
    /// 上下文则仍会保留 `/` 与一个空格。
    pub fn apply_completion_impl(
        &self,
        lines: &[String],
        cursor_line: usize,
        cursor_col: usize,
        item: &AutocompleteItem,
        prefix: &str,
    ) -> ApplyCompletionResult {
        let current_line = lines.get(cursor_line).map(String::as_str).unwrap_or("");
        let prefix_len = utf16_len(prefix) as i64;
        // `beforePrefix` / `afterCursor` 保留为 UTF-16 码元切片：TS 先 slice 再用 `+`
        // 拼回，落在代理对中间的边界不会损坏字符（逐个切片解码会产生 U+FFFD）。
        let current_units = utf16_units(current_line);
        let before_prefix_units = slice_units(&current_units, 0, cursor_col as i64 - prefix_len);
        let after_cursor_units = slice_units(&current_units, cursor_col as i64, i64::MAX);
        let is_quoted_prefix = prefix.starts_with('"') || prefix.starts_with("@\"");
        let has_leading_quote_after_cursor = after_cursor_units.first() == Some(&u16::from(b'"'));
        let has_trailing_quote_in_item = item.value.ends_with('"');
        let adjusted_after_cursor_units =
            if is_quoted_prefix && has_trailing_quote_in_item && has_leading_quote_after_cursor {
                // 条件保证 `after_cursor_units` 非空。
                &after_cursor_units[1..]
            } else {
                after_cursor_units
            };

        let before_prefix_len = before_prefix_units.len();
        let item_value_len = utf16_len(&item.value);

        let mut new_lines: Vec<String> = lines.to_vec();
        // 防御性补行：cursor_line 越界时先把缓冲区扩到该行（正常调用方不会触发）。
        if new_lines.len() <= cursor_line {
            new_lines.resize(cursor_line + 1, String::new());
        }

        // 上下文 1：斜杠命令名补全。要求 prefix 以 `/` 开头、prefix 之前全是空白，
        // 且 `/` 之后没有第二个 `/`（否则可能是路径）。结果固定补一个空格，
        // 所以光标是 before + value + 2（`/` 与空格各占一列）。
        let is_slash_command = prefix.starts_with('/')
            && trim_js(&units_to_string(before_prefix_units)).is_empty()
            && !slice_suffix(prefix, 1).contains('/');
        if is_slash_command {
            let new_line = concat_utf16(&[
                Utf16Piece::Units(before_prefix_units),
                Utf16Piece::Text("/"),
                Utf16Piece::Text(&item.value),
                Utf16Piece::Text(" "),
                Utf16Piece::Units(adjusted_after_cursor_units),
            ]);
            new_lines[cursor_line] = new_line;
            return ApplyCompletionResult {
                lines: new_lines,
                cursor_line,
                cursor_col: before_prefix_len + item_value_len + 2,
            };
        }

        // 上下文 2：`@` 文件附件补全。文件补一个空格结束 token；目录不补，方便继续
        // 输入下一层。目录候选带尾引号时（如 `@"dir/"`）光标要停在闭引号之前，故 -1。
        if prefix.starts_with('@') {
            let is_directory = item.label.ends_with('/');
            let suffix = if is_directory { "" } else { " " };
            let new_line = concat_utf16(&[
                Utf16Piece::Units(before_prefix_units),
                Utf16Piece::Text(&item.value),
                Utf16Piece::Text(suffix),
                Utf16Piece::Units(adjusted_after_cursor_units),
            ]);
            new_lines[cursor_line] = new_line;

            let has_trailing_quote = item.value.ends_with('"');
            let cursor_offset = if is_directory && has_trailing_quote {
                item_value_len - 1
            } else {
                item_value_len
            };

            return ApplyCompletionResult {
                lines: new_lines,
                cursor_line,
                cursor_col: before_prefix_len + cursor_offset + utf16_len(suffix),
            };
        }

        // 上下文 3：斜杠命令参数（也会兜住“带空格的路径”这类输入）。判定只看光标前
        // 文本是否同时含 `/` 与空格；这里不额外补空格。
        let text_before_cursor = slice_prefix(current_line, cursor_col as i64);
        if text_before_cursor.contains('/') && text_before_cursor.contains(' ') {
            let new_line = concat_utf16(&[
                Utf16Piece::Units(before_prefix_units),
                Utf16Piece::Text(&item.value),
                Utf16Piece::Units(adjusted_after_cursor_units),
            ]);
            new_lines[cursor_line] = new_line;

            let is_directory = item.label.ends_with('/');
            let has_trailing_quote = item.value.ends_with('"');
            let cursor_offset = if is_directory && has_trailing_quote {
                item_value_len - 1
            } else {
                item_value_len
            };
            return ApplyCompletionResult {
                lines: new_lines,
                cursor_line,
                cursor_col: before_prefix_len + cursor_offset,
            };
        }

        // 上下文 4：普通文件路径。与上下文 3 相同，不额外补空格；目录且带尾引号时
        // 同样把光标放在闭引号之前。
        let new_line = concat_utf16(&[
            Utf16Piece::Units(before_prefix_units),
            Utf16Piece::Text(&item.value),
            Utf16Piece::Units(adjusted_after_cursor_units),
        ]);
        new_lines[cursor_line] = new_line;

        let is_directory = item.label.ends_with('/');
        let has_trailing_quote = item.value.ends_with('"');
        let cursor_offset = if is_directory && has_trailing_quote {
            item_value_len - 1
        } else {
            item_value_len
        };
        ApplyCompletionResult {
            lines: new_lines,
            cursor_line,
            cursor_col: before_prefix_len + cursor_offset,
        }
    }

    // ------------------------------------------------------------------
    // 前缀提取（spec §2.10.6）
    // ------------------------------------------------------------------

    /// `extractAtPrefix(text)`。
    fn extract_at_prefix(&self, text: &str) -> Option<String> {
        // 未闭合引号内的空格不是分隔符，所以先尝试整段引号 token；但只有 `@"` 形式
        // 属于 @ 补全，普通 `"…` 留给路径补全。
        if let Some(quoted_prefix) = extract_quoted_prefix(text)
            && quoted_prefix.starts_with("@\"")
        {
            return Some(quoted_prefix);
        }

        // 否则取最后一个分隔符之后的 token，再剥掉开括号/反引号前缀
        // （`(~/x` → `~/x`）。
        let last_delimiter_index = find_last_delimiter(text);
        let raw_token = if last_delimiter_index == -1 {
            text.to_string()
        } else {
            slice_suffix(text, last_delimiter_index + 1)
        };
        let token = strip_leading_wrappers(&raw_token);

        if token.starts_with('@') {
            return Some(token);
        }

        None
    }

    /// `extractPathPrefix(text, forceExtract = false)`。
    fn extract_path_prefix(&self, text: &str, force_extract: bool) -> Option<String> {
        // 任何未闭合引号 token 都直接当作路径前缀（普通 `"` 与 `@"` 都算）。
        if let Some(quoted_prefix) = extract_quoted_prefix(text) {
            return Some(quoted_prefix);
        }

        let last_delimiter_index = find_last_delimiter(text);
        let raw_token = if last_delimiter_index == -1 {
            text.to_string()
        } else {
            slice_suffix(text, last_delimiter_index + 1)
        };
        let path_prefix = strip_leading_wrappers(&raw_token);

        // force（显式 Tab）无条件接受当前 token；自然触发只接受“看起来像路径”的三种
        // 形态，避免在普通单词上弹出文件列表。
        if force_extract {
            return Some(path_prefix);
        }

        if path_prefix.contains('/')
            || path_prefix.starts_with('.')
            || path_prefix.starts_with("~/")
        {
            return Some(path_prefix);
        }

        // 空白或 CJK 标点后的空前缀：列出当前目录；`text` 为空串时不触发（此时光标前
        // 没有任何字符，也就没有 token）。
        if path_prefix.is_empty() && !text.is_empty() && token_start_matches(text) {
            return Some(path_prefix);
        }

        None
    }

    /// `expandHomePath(path)`：`~/` → 家目录，保留尾部 `/`。
    ///
    /// `HOME` 为空串时 `path_join` 会丢掉空段（`~/x` → `x`、`~/` → `./`），测试
    /// `empty_home_expands_tilde_like_node` 锁定了与 Node `join(os.homedir(), rest)`
    /// 一致的行为。
    fn expand_home_path(&self, path: &str) -> String {
        if let Some(rest) = path.strip_prefix("~/") {
            let expanded = path_join(&homedir(), rest);
            if path.ends_with('/') && !expanded.ends_with('/') {
                return format!("{expanded}/");
            }
            return expanded;
        } else if path == "~" {
            return homedir();
        }
        path.to_string()
    }

    // ------------------------------------------------------------------
    // resolveScopedFuzzyQuery / scopedPathForDisplay（spec §2.10.8）
    // ------------------------------------------------------------------

    /// `resolveScopedFuzzyQuery(rawQuery)`。
    ///
    /// `raw_query` 先经 `to_display_path` 归一化（`\` → `/`），再按归一化结果里的最后一个
    /// `/` 切成“目录部分 + 搜索词”；归一化后没有 `/` 才返回 `None`，调用方会退回
    /// `base_path` + 完整 query（原始 query 只含 `\`，如 `src\ma`，归一化后仍会进入切分分支）。
    /// 目录不存在或不是目录同样返回 `None`。
    fn resolve_scoped_fuzzy_query(&self, raw_query: &str) -> Option<ScopedFuzzyQuery> {
        let normalized_query = to_display_path(raw_query);
        // `lastIndexOf("/")` 是 **UTF-16 码元**索引；`str::rfind` 返回的是 UTF-8 字节偏移，
        // 遇到非 ASCII（如 CJK 目录名）时两者不一致，后续 `slice_prefix`/`slice_suffix`
        // 会把路径切错。
        let slash_index = normalized_query
            .encode_utf16()
            .enumerate()
            .filter(|&(_, unit)| unit == u16::from(b'/'))
            .map(|(index, _)| index)
            .last()?;

        let display_base = slice_prefix(&normalized_query, slash_index as i64 + 1);
        let query = slice_suffix(&normalized_query, slash_index as i64 + 1);

        // 目录部分：`~/` 展开家目录，绝对路径原样使用，其余拼到 `base_path`（会话 cwd）。
        let base_dir = if display_base.starts_with("~/") {
            self.expand_home_path(&display_base)
        } else if display_base.starts_with('/') {
            display_base.clone()
        } else {
            path_join(&self.base_path, &display_base)
        };

        // 目录必须真实存在；`Path::is_dir` 会跟随符号链接（与 TS 的 `statSync` 一致）。
        if !Path::new(&base_dir).is_dir() {
            return None;
        }

        Some(ScopedFuzzyQuery {
            base_dir,
            query,
            display_base,
        })
    }

    /// `scopedPathForDisplay(displayBase, relativePath)`。
    fn scoped_path_for_display(&self, display_base: &str, relative_path: &str) -> String {
        let normalized_relative_path = to_display_path(relative_path);
        if display_base == "/" {
            return format!("/{normalized_relative_path}");
        }
        format!(
            "{}{}",
            to_display_path(display_base),
            normalized_relative_path
        )
    }

    // ------------------------------------------------------------------
    // getFileSuggestions（同步 readdir，spec §2.10.7）
    // ------------------------------------------------------------------

    /// `getFileSuggestions(prefix)`：同步 `readdir` 的路径补全。
    ///
    /// 目录不存在 / 不可读 → 空表（TS 的 `try { … } catch { return [] }`）。
    /// 返回候选已排序（目录优先，再按 `label` 的 `localeCompare`），且**不截断**数量
    /// ——20 条上限只用于 fd 模糊搜索。进程默认 locale 非法时也返回空表。
    pub fn get_file_suggestions(&self, prefix: &str) -> Vec<AutocompleteItem> {
        let parsed = parse_path_prefix(prefix);
        let raw_prefix = parsed.raw_prefix;
        let is_at_prefix = parsed.is_at_prefix;
        let is_quoted_prefix = parsed.is_quoted_prefix;

        let mut expanded_prefix = raw_prefix.clone();
        if expanded_prefix.starts_with('~') {
            expanded_prefix = self.expand_home_path(&expanded_prefix);
        }

        // 这些前缀都表示“列出目录本身、过滤词为空”：`@` 后直接触发、`./`、`../`、`~`、
        // `~/`、`/`。其中 `(is_at_prefix && raw_prefix.is_empty())` 与第一条
        // `raw_prefix.is_empty()` 重复，保留是为了让 `@` 前缀的意图在条件列表里自描述。
        let is_root_prefix = raw_prefix.is_empty()
            || raw_prefix == "./"
            || raw_prefix == "../"
            || raw_prefix == "~"
            || raw_prefix == "~/"
            || raw_prefix == "/"
            || (is_at_prefix && raw_prefix.is_empty());

        let search_dir: String;
        let search_prefix: String;

        // 目录 / 过滤词的选择：以 `/` 结尾（或根前缀）→ 搜索目录就是前缀本身；
        // 否则拆成 `dirname` + `basename`。`~/` 开头或展开后是绝对路径时直接用，
        // 其余相对路径拼到 `base_path` 下。
        if is_root_prefix || raw_prefix.ends_with('/') {
            search_dir = if raw_prefix.starts_with('~') || expanded_prefix.starts_with('/') {
                expanded_prefix.clone()
            } else {
                path_join(&self.base_path, &expanded_prefix)
            };
            search_prefix = String::new();
        } else {
            let dir = path_dirname(&expanded_prefix);
            let file = path_basename(&expanded_prefix);
            search_dir = if raw_prefix.starts_with('~') || expanded_prefix.starts_with('/') {
                dir
            } else {
                path_join(&self.base_path, &dir)
            };
            search_prefix = file;
        }

        // 目录不存在、无权限、不是目录等任何 read_dir 错误 → 空表（TS 的 try/catch）。
        let entries = match std::fs::read_dir(&search_dir) {
            Ok(entries) => entries,
            Err(_) => return Vec::new(),
        };

        // 过滤是大小写不敏感的简单前缀比较：两边都做 locale 无关的 Unicode 小写
        // （`to_lowercase` / TS 的 `toLowerCase`）；`flatten()` 跳过单个目录项的读取错误。
        let lower_search_prefix = search_prefix.to_lowercase();
        let mut suggestions: Vec<AutocompleteItem> = Vec::new();

        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if !name.to_lowercase().starts_with(&lower_search_prefix) {
                continue;
            }

            // 目录判定：`entry.isDirectory()`；否则符号链接再 `statSync` 一次。
            let file_type = match entry.file_type() {
                Ok(file_type) => file_type,
                Err(_) => continue,
            };
            let mut is_directory = file_type.is_dir();
            if !is_directory && file_type.is_symlink() {
                let full_path = path_join(&search_dir, &name);
                is_directory = Path::new(&full_path).is_dir();
            }

            // 展示路径按用户输入的原形态重建，不能直接用搜索目录的 join 结果：
            // `~/`、`/abs/`、`./rel/` 等前缀要原样保留，否则补全后文本会变成另一种
            // 路径形态。分支顺序决定匹配优先级：先目录形态、再含分隔符的路径、最后裸名称。
            let display_prefix = raw_prefix.as_str();
            let relative_path: String;

            if display_prefix.ends_with('/') {
                relative_path = format!("{display_prefix}{name}");
            } else if display_prefix.contains('/') || display_prefix.contains('\\') {
                if let Some(home_relative_dir) = display_prefix.strip_prefix("~/") {
                    let dir = path_dirname(home_relative_dir);
                    relative_path = if dir == "." {
                        format!("~/{name}")
                    } else {
                        format!("~/{}", path_join(&dir, &name))
                    };
                } else if display_prefix.starts_with('/') {
                    let dir = path_dirname(display_prefix);
                    relative_path = if dir == "/" {
                        format!("/{name}")
                    } else {
                        format!("{dir}/{name}")
                    };
                } else {
                    let joined = path_join(&path_dirname(display_prefix), &name);
                    // `path.join` 会把 `./` 前缀规范化掉，TS 显式补回。
                    if display_prefix.starts_with("./") && !joined.starts_with("./") {
                        relative_path = format!("./{joined}");
                    } else {
                        relative_path = joined;
                    }
                }
            } else if display_prefix.starts_with('~') {
                relative_path = format!("~/{name}");
            } else {
                relative_path = name.clone();
            }

            let relative_path = to_display_path(&relative_path);
            let path_value = if is_directory {
                format!("{relative_path}/")
            } else {
                relative_path
            };
            let value =
                build_completion_value(&path_value, is_directory, is_at_prefix, is_quoted_prefix);

            suggestions.push(AutocompleteItem {
                value,
                label: if is_directory {
                    format!("{name}/")
                } else {
                    name
                },
                description: None,
            });
        }

        // Node 在默认 locale 非法时由 `localeCompare` 抛 `RangeError`，被外层 try/catch
        // 吞掉 → 整个函数返回 `[]`（`getSuggestions` 随后返回 null）。
        if !process_locale_is_valid() {
            return Vec::new();
        }

        // 目录优先，再按 `localeCompare(label)`（V8 与 Rust 的排序都是稳定的）。
        suggestions.sort_by(|a, b| {
            let a_is_dir = a.label.ends_with('/');
            let b_is_dir = b.label.ends_with('/');
            if a_is_dir && !b_is_dir {
                return Ordering::Less;
            }
            if !a_is_dir && b_is_dir {
                return Ordering::Greater;
            }
            locale_compare(&a.label, &b.label)
        });

        suggestions
    }

    // ------------------------------------------------------------------
    // scoreEntry / getBaseDirSuggestions / getFuzzyFileSuggestions（spec §2.10.9）
    // ------------------------------------------------------------------

    /// `scoreEntry(filePath, query, isDirectory)`：越高越好（目录在非零分上加 10）。
    ///
    /// 分档：文件名完全相等 100、前缀匹配 80、文件名包含 50、全路径包含 30、否则 0。
    /// 目录加成只作用于已有分数的条目，不会把 0 分条目变成候选。
    fn score_entry(&self, file_path: &str, query: &str, is_directory: bool) -> i32 {
        let file_name = path_basename(file_path);
        let lower_file_name = file_name.to_lowercase();
        let lower_query = query.to_lowercase();

        let mut score = 0;

        if lower_file_name == lower_query {
            score = 100;
        } else if lower_file_name.starts_with(&lower_query) {
            score = 80;
        } else if lower_file_name.contains(&lower_query) {
            score = 50;
        } else if file_path.to_lowercase().contains(&lower_query) {
            score = 30;
        }

        if is_directory && score > 0 {
            score += 10;
        }

        score
    }

    /// `getBaseDirSuggestions(baseDir, query, signal)`：`--max-depth 1` 的直接子项。
    ///
    /// 固定最多 100 条；`fd_path` 缺失或令牌已取消时返回空表（调用方无法区分
    /// “没有结果”与“没装 fd”，两者都表现为空建议）。
    async fn get_base_dir_suggestions(
        &self,
        base_dir: &str,
        query: &str,
        signal: &tokio_util::sync::CancellationToken,
    ) -> Vec<FdEntry> {
        let Some(fd_path) = self.fd_path.as_deref() else {
            return Vec::new();
        };
        if signal.is_cancelled() {
            return Vec::new();
        }

        walk_directory_with_fd(base_dir, fd_path, query, 100, signal, Some(1)).await
    }

    /// `getFuzzyFileSuggestions(query, {isQuotedPrefix, signal})`。
    ///
    /// 两次 fd 调用**串行**：先 `--max-depth 1` 取直接子项，再全递归；合并时按 `path`
    /// 字符串（含尾部 `/`）去重，先到的直接子项保留。打分后按“分数降序 → 深度升序 →
    /// UTF-16 长度升序 → localeCompare”排序，只取前 20 条。
    async fn get_fuzzy_file_suggestions(
        &self,
        query: &str,
        is_quoted_prefix: bool,
        signal: &tokio_util::sync::CancellationToken,
    ) -> Vec<AutocompleteItem> {
        let Some(fd_path) = self.fd_path.as_deref() else {
            return Vec::new();
        };
        if signal.is_cancelled() {
            return Vec::new();
        }

        // 查询里带 `/` 且目录存在时，fd 以该目录为根、只搜最后一段；否则以 `base_path`
        // 为根搜索整个 query。
        let scoped_query = self.resolve_scoped_fuzzy_query(query);
        let fd_base_dir = match &scoped_query {
            Some(scoped) => scoped.base_dir.clone(),
            None => self.base_path.clone(),
        };
        let fd_query = match &scoped_query {
            Some(scoped) => scoped.query.clone(),
            None => query.to_string(),
        };

        let base_dir_entries = self
            .get_base_dir_suggestions(&fd_base_dir, &fd_query, signal)
            .await;
        let recursive_entries =
            walk_directory_with_fd(&fd_base_dir, fd_path, &fd_query, 100, signal, None).await;

        let mut seen_paths: HashSet<String> = HashSet::new();
        let mut entries: Vec<FdEntry> = Vec::new();
        for entry in &base_dir_entries {
            seen_paths.insert(entry.path.clone());
            entries.push(entry.clone());
        }
        for entry in recursive_entries {
            if !seen_paths.insert(entry.path.clone()) {
                continue;
            }
            entries.push(entry);
        }

        if signal.is_cancelled() {
            return Vec::new();
        }

        let mut scored_entries: Vec<ScoredEntry> = entries
            .into_iter()
            .map(|entry| {
                let score = if fd_query.is_empty() {
                    1
                } else {
                    self.score_entry(&entry.path, &fd_query, entry.is_directory)
                };
                ScoredEntry { entry, score }
            })
            .filter(|entry| entry.score > 0)
            .collect();

        // 与 `getFileSuggestions` 相同：`localeCompare` 抛出被外层 try/catch 吞掉 → `[]`。
        if !process_locale_is_valid() {
            return Vec::new();
        }

        // 排序键依次是：分数降序 → 路径深度升序（`split('/')` 去掉空段，尾部 `/` 不计
        // 深度）→ UTF-16 长度升序 → localeCompare。
        scored_entries.sort_by(|a, b| {
            let score_diff = b.score.cmp(&a.score);
            if score_diff != Ordering::Equal {
                return score_diff;
            }

            let a_depth = to_display_path(&a.entry.path)
                .split('/')
                .filter(|segment| !segment.is_empty())
                .count();
            let b_depth = to_display_path(&b.entry.path)
                .split('/')
                .filter(|segment| !segment.is_empty())
                .count();
            let depth_diff = a_depth.cmp(&b_depth);
            if depth_diff != Ordering::Equal {
                return depth_diff;
            }

            let length_diff = utf16_len(&a.entry.path).cmp(&utf16_len(&b.entry.path));
            if length_diff != Ordering::Equal {
                return length_diff;
            }

            locale_compare(&a.entry.path, &b.entry.path)
        });

        // 截断在排序之后：只保留前 20 条。目录路径先去掉尾部 `/` 再做 basename 与显示
        // 路径，最后按目录补回。
        let mut suggestions: Vec<AutocompleteItem> = Vec::new();
        for scored in scored_entries.iter().take(20) {
            let entry_path = &scored.entry.path;
            let is_directory = scored.entry.is_directory;
            let path_without_slash = if is_directory {
                slice_prefix(entry_path, utf16_len(entry_path) as i64 - 1)
            } else {
                entry_path.clone()
            };
            let display_path = match &scoped_query {
                Some(scoped) => {
                    self.scoped_path_for_display(&scoped.display_base, &path_without_slash)
                }
                None => path_without_slash.clone(),
            };
            let entry_name = path_basename(&path_without_slash);
            let completion_path = if is_directory {
                format!("{display_path}/")
            } else {
                display_path.clone()
            };
            // 模糊补全只服务 `@`，所以 isAtPrefix 固定为 true。
            let value =
                build_completion_value(&completion_path, is_directory, true, is_quoted_prefix);

            suggestions.push(AutocompleteItem {
                value,
                label: if is_directory {
                    format!("{entry_name}/")
                } else {
                    entry_name
                },
                description: Some(display_path),
            });
        }

        suggestions
    }

    // ------------------------------------------------------------------
    // shouldTriggerFileCompletion（spec §2.10.10）
    // ------------------------------------------------------------------

    /// `shouldTriggerFileCompletion(lines, cursorLine, cursorCol)`。
    ///
    /// 默认 `true`。唯一的 `false` 情形是：光标前文本经 `trim_js` 去掉首尾 JS 空白后以
    /// `/` 开头，且剩余文本不含空格（`contains(' ')` 只认 ASCII 空格）。行尾空白会被
    /// trim 掉，所以 `/cmd   ` 这样的行同样返回 `false`。Editor 在 `force` 请求前调用它，
    /// 返回 `false` 时连在飞请求都不取消。
    pub fn should_trigger_file_completion_impl(
        &self,
        lines: &[String],
        cursor_line: usize,
        cursor_col: usize,
    ) -> bool {
        let current_line = lines.get(cursor_line).map(String::as_str).unwrap_or("");
        let text_before_cursor = slice_prefix(current_line, cursor_col as i64);

        // 行首的斜杠命令（尚未输入空格）不触发文件补全。
        let trimmed = trim_js(&text_before_cursor);
        if trimmed.starts_with('/') && !trimmed.contains(' ') {
            return false;
        }

        true
    }
}

/// `fuzzyFilter` 的命令项（`{name, label, description?}`）。
#[derive(Debug, Clone, PartialEq, Eq)]
struct CommandItem {
    name: String,
    label: String,
    description: Option<String>,
}

/// [`CombinedAutocompleteProvider::resolve_scoped_fuzzy_query`] 的结果。
#[derive(Debug, Clone, PartialEq, Eq)]
struct ScopedFuzzyQuery {
    base_dir: String,
    query: String,
    display_base: String,
}

/// 打分后的 fd 条目。
#[derive(Debug, Clone, PartialEq, Eq)]
struct ScoredEntry {
    entry: FdEntry,
    score: i32,
}

/// `text.indexOf(needle)`（UTF-16 索引；-1 表示没有）。`needle` 恒为单码元字符。
fn find_char_index(text: &str, needle: char) -> i64 {
    let needle_unit = {
        let mut buf = [0u16; 2];
        let encoded = needle.encode_utf16(&mut buf);
        encoded[0]
    };
    for (index, unit) in (0_i64..).zip(text.encode_utf16()) {
        if unit == needle_unit {
            return index;
        }
    }
    -1
}

/// `os.homedir()`。
///
/// Node 的语义（`lib/os.js`）：**已设置**的 `HOME`（即使为空串）原样返回，只有 `HOME`
/// 未设置时才回退 passwd 条目。`std::env::home_dir()` 则把空 `HOME` 当“未设置”并回退到
/// passwd，因此必须先自己检查 `HOME` 是否**存在**。
fn homedir() -> String {
    if let Some(home) = std::env::var_os("HOME") {
        return home.to_string_lossy().into_owned();
    }
    #[allow(deprecated)]
    let fallback = std::env::home_dir();
    match fallback {
        Some(path) => path.to_string_lossy().into_owned(),
        None => String::new(),
    }
}

// ============================================================================
// trait 实现
// ============================================================================

impl AutocompleteProvider for CombinedAutocompleteProvider {
    fn get_suggestions<'a>(
        &'a mut self,
        lines: &'a [String],
        cursor_line: usize,
        cursor_col: usize,
        options: &'a GetSuggestionsOptions,
    ) -> Pin<Box<dyn Future<Output = Option<AutocompleteSuggestions>> + Send + 'a>> {
        Box::pin(self.get_suggestions_impl(lines, cursor_line, cursor_col, options))
    }

    fn apply_completion(
        &self,
        lines: &[String],
        cursor_line: usize,
        cursor_col: usize,
        item: &AutocompleteItem,
        prefix: &str,
    ) -> ApplyCompletionResult {
        self.apply_completion_impl(lines, cursor_line, cursor_col, item, prefix)
    }

    fn should_trigger_file_completion(
        &self,
        lines: &[String],
        cursor_line: usize,
        cursor_col: usize,
    ) -> bool {
        self.should_trigger_file_completion_impl(lines, cursor_line, cursor_col)
    }
}

/// `which fd`（Windows 是 `where fd`）：返回 stdout 第一行（trim 后），失败返回 `None`。
///
/// TS 的 `resolveFdPath` 在 `test/autocomplete.test.ts` 里，是 fd 用例是否运行的门槛；
/// 这里作为公开工具函数保留（Rust 测试与下游 `kiana-interactive` 复用同一逻辑）。
pub fn resolve_fd_path() -> Option<String> {
    let command = if cfg!(windows) { "where" } else { "which" };
    let output = std::process::Command::new(command)
        .arg("fd")
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let first_line = stdout
        .split('\n')
        .map(|line| line.strip_suffix('\r').unwrap_or(line))
        .find(|line| !line.is_empty())?;
    let trimmed = trim_js(first_line).to_string();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed)
    }
}

// ============================================================================
// 单元测试（node:path 移植的黄金值由 Node 26.12.1 生成）
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    /// 改写进程环境变量的测试共用一把锁。
    ///
    /// 必须是**模块级唯一**的 `static`：若每个测试各声明一个同名 `static`，它们是不同的锁，
    /// 两个测试仍会并发改写 `HOME`（曾导致 `empty_home_expands_tilde_like_node` 偶发读到
    /// 另一个测试写入的 `" "`，得到 `" /x"`）。
    static ENV_MUTEX: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// 黄金值来自 `node -p "require('path').posix.dirname/basename/normalize(...)"`。
    const PATH_CASES: &[(&str, &str, &str, &str)] = &[
        ("", ".", "", "."),
        (".", ".", ".", "."),
        ("./", ".", ".", "./"),
        ("..", ".", "..", ".."),
        ("../", ".", "..", "../"),
        ("~", ".", "~", "~"),
        ("~/", ".", "~", "~/"),
        ("~x", ".", "~x", "~x"),
        ("a", ".", "a", "a"),
        ("a/", ".", "a", "a/"),
        ("a//", ".", "a", "a/"),
        ("/", "/", "", "/"),
        ("//", "/", "", "/"),
        ("///", "/", "", "/"),
        ("/a", "/", "a", "/a"),
        ("/a/", "/", "a", "/a/"),
        ("/a//b", "/a/", "b", "/a/b"),
        ("/a//b/", "/a/", "b", "/a/b/"),
        ("a/b", "a", "b", "a/b"),
        ("a/b/", "a", "b", "a/b/"),
        ("a//b", "a/", "b", "a/b"),
        ("./a", ".", "a", "a"),
        ("./a/b", "./a", "b", "a/b"),
        ("./a/b/", "./a", "b", "a/b/"),
        ("a/./b", "a/.", "b", "a/b"),
        ("a/../b", "a/..", "b", "b"),
        ("../..", "..", "..", "../.."),
        ("/..", "/", "..", "/"),
        ("~/Dev", "~", "Dev", "~/Dev"),
        ("~/Dev/", "~", "Dev", "~/Dev/"),
        ("~/Dev/x", "~/Dev", "x", "~/Dev/x"),
        ("文档/说", "文档", "说", "文档/说"),
        ("文档/", ".", "文档", "文档/"),
        ("...", ".", "...", "..."),
        ("a/...", "a", "...", "a/..."),
        ("/a/.../b", "/a/...", "b", "/a/.../b"),
    ];

    #[test]
    fn posix_dirname_basename_normalize_match_node() {
        for &(input, dirname, basename, normalize) in PATH_CASES {
            assert_eq!(path_dirname(input), dirname, "dirname({input:?})");
            assert_eq!(path_basename(input), basename, "basename({input:?})");
            assert_eq!(path_normalize(input), normalize, "normalize({input:?})");
        }
    }

    /// `node -p "require('path').posix.join(a, b)"` 的黄金值。
    const JOIN_CASES: &[(&str, &str, &str)] = &[
        ("/base", "", "/base"),
        ("/base", "./", "/base/"),
        ("/base", "~/x", "/base/~/x"),
        ("/base", "a/", "/base/a/"),
        ("/base", "../outside/", "/outside/"),
        ("/base", "文档", "/base/文档"),
        ("/base", "src", "/base/src"),
        ("/base", "./src", "/base/src"),
        ("/base", "a//b", "/base/a/b"),
        ("/base", "a/./b", "/base/a/b"),
        ("/base", ".", "/base"),
        ("/base", "/abs", "/base/abs"),
        ("/base", "a/b", "/base/a/b"),
        ("/base", "..", "/"),
        ("", "x", "x"),
        ("x", "", "x"),
        ("/", "", "/"),
        ("", "", "."),
    ];

    #[test]
    fn posix_join_matches_node() {
        for &(a, b, expected) in JOIN_CASES {
            assert_eq!(path_join(a, b), expected, "join({a:?}, {b:?})");
        }
    }

    #[test]
    fn utf16_slice_follows_js_semantics() {
        assert_eq!(slash_len_for_test("😀"), 2);
        assert_eq!(slice_prefix("😀ab", 1), "\u{FFFD}");
        assert_eq!(slice_suffix("😀ab", 2), "ab");
        assert_eq!(slice_prefix("abc", -1), "ab");
        assert_eq!(slice_suffix("abc", -1), "c");
    }

    fn slash_len_for_test(text: &str) -> usize {
        utf16_len(text)
    }

    #[test]
    fn build_fd_path_query_matches_ts() {
        assert_eq!(build_fd_path_query("a"), "a");
        assert_eq!(build_fd_path_query("pages/["), "pages[\\\\/]\\[");
        assert_eq!(build_fd_path_query("a/"), "a[\\\\/]");
        assert_eq!(build_fd_path_query("/"), "/");
        assert_eq!(build_fd_path_query("docs/a.md"), "docs[\\\\/]a\\.md");
        assert_eq!(build_fd_path_query("a\\b"), "a[\\\\/]b");
    }

    #[test]
    fn token_start_matches_checks_last_character() {
        assert!(token_start_matches(""));
        assert!(token_start_matches(" "));
        assert!(token_start_matches("x "));
        assert!(!token_start_matches("x"));
        assert!(!token_start_matches(" x"));
        assert!(!token_start_matches("，x"));
        assert!(token_start_matches("，"));
        assert!(!token_start_matches("a b"));
        assert!(!token_start_matches("。x"));
        assert!(!token_start_matches("@"));
        assert!(!token_start_matches("a/"));
    }

    #[test]
    fn strip_leading_wrappers_keeps_closed_pairs() {
        assert_eq!(strip_leading_wrappers("(~/Dev"), "~/Dev");
        assert_eq!(strip_leading_wrappers("`src/ma"), "src/ma");
        assert_eq!(strip_leading_wrappers("app/[slug]/pa"), "app/[slug]/pa");
        assert_eq!(strip_leading_wrappers("(group)/pa"), "(group)/pa");
        assert_eq!(strip_leading_wrappers("((x"), "x");
        assert_eq!(strip_leading_wrappers(""), "");
    }

    #[test]
    fn escape_regex_escapes_js_metacharacters() {
        assert_eq!(escape_regex("abc"), "abc");
        assert_eq!(escape_regex("a.b*c"), "a\\.b\\*c");
        assert_eq!(escape_regex("[id]"), "\\[id\\]");
        assert_eq!(escape_regex("(g)$^"), "\\(g\\)\\$\\^");
        assert_eq!(escape_regex("a-b_c"), "a-b_c");
    }

    const LOCALE_CASES: &[(&str, &str)] = &[
        ("", "und"),
        ("\t", "und"),
        ("\t ", "und"),
        ("\tja_JP", "und-JP"),
        (" ", "und"),
        ("  ", "und"),
        (" C", "und"),
        (" ja_JP", "und-JP"),
        (" ja_JP.UTF-8 ", "und-JP"),
        (" x", "und"),
        (" x ", "und"),
        (".UTF-8", "und"),
        ("12_34", "und"),
        ("419", "und"),
        ("@euro", "und-x-lvariant-euro"),
        ("@x", "und-x-lvariant-x"),
        ("ABW", "abw"),
        ("ALA", "ala"),
        ("AN", "an"),
        ("ANT", "ant"),
        ("ATC", "atc"),
        ("ATF", "atf"),
        ("BES", "bes"),
        ("BLM", "blm"),
        ("BU", "bu"),
        ("BVT", "bvt"),
        ("C", "en-US"),
        ("C ", "und"),
        ("C.UTF-8", "en-US"),
        ("C.utf8", "en-US"),
        ("C@", "en-US-u-va-posix"),
        ("C@@x", "en-US-posix-x-lvariant-x"),
        ("C@euro", "en-US-x-lvariant-euro-posix"),
        ("CCK", "cck"),
        ("CRQ", "crq"),
        ("CS", "cs"),
        ("CXR", "cxr"),
        ("C_JP", "und-JP"),
        ("C_US", "und-US"),
        ("DD", "dd"),
        ("EN", "en"),
        ("EN_us", "en-US"),
        ("EU", "eu"),
        ("En", "en"),
        ("FX", "fx"),
        ("HMD", "hmd"),
        ("IOT", "iot"),
        ("NT", "nt"),
        ("POSIX", "en-US"),
        ("POSIX.UTF-8", "en-US"),
        ("POSIX@euro", "en-US-x-lvariant-euro-posix"),
        ("POSIX_US", "posix-US"),
        ("Posix", "posix"),
        ("QO", "qo"),
        ("SGS", "sgs"),
        ("SJM", "sjm"),
        ("SU", "su"),
        ("TKL", "tkl"),
        ("TP", "tp"),
        ("UK", "uk"),
        ("UMI", "umi"),
        ("UN", "un"),
        ("VAT", "vat"),
        ("YD", "yd"),
        ("ZR", "zr"),
        ("ZZ", "zz"),
        ("_", "und"),
        ("_JP", "und-JP"),
        ("__", "und"),
        ("a_b", "und-x-lvariant-b"),
        ("aaa_JP", "aaa-JP"),
        ("aam_TZ", "aas-TZ"),
        ("aam_XX", "aas-XX"),
        ("ab_cd", "ab-CD"),
        ("abcd", "abcd"),
        ("abcde", "abcde"),
        ("abcdefgh", "abcdefgh"),
        ("abcdefghi", "und"),
        ("az_Latn_AZ", "az-Latn-AZ"),
        ("bel_BY", "be-BY"),
        ("ben_IN", "bn-IN"),
        ("bos_BA", "bs-BA"),
        ("bul_BG", "bg-BG"),
        ("c", "en-US"),
        ("c.UTF-8", "en-US"),
        ("c@euro", "und-x-lvariant-euro"),
        ("c_JP", "und-JP"),
        ("ca_ES@valencia", "ca-ES-valencia"),
        ("cat_ES", "ca-ES"),
        ("ces_CZ", "cs-CZ"),
        ("chi_CN", "chi-CN"),
        ("cym_GB", "cy-GB"),
        ("dan_DK", "da-DK"),
        ("de@euro", "de-x-lvariant-euro"),
        ("de_DE-u-co-phonebk", "de-DE-u-co-phonebk"),
        ("de_DE.UTF-8.extra", "de-DE"),
        ("de_DE.UTF-8@euro", "de-DE-x-lvariant-euro"),
        ("de_DE@", "de-DE"),
        ("de_DE@euro", "de-DE-x-lvariant-euro"),
        ("de_DE@euro.UTF-8", "de-DE-x-lvariant-euro"),
        ("de_DE@euro@x", "de-DE-x-lvariant-x"),
        ("de_DE@euro_x", "de-DE-x-lvariant-euro-x"),
        ("de_DE@phonebook", "de-DE"),
        ("de_DE_1901", "de-DE-1901"),
        ("de_DE_1901.UTF-8", "de-DE-1901"),
        ("deu_DE", "de-DE"),
        ("drh_MN", "khk-MN"),
        ("e", "und"),
        ("ell_GR", "el-GR"),
        ("en", "en"),
        ("en US", "und"),
        ("en-US-abcdefgh", "en-US-abcdefgh"),
        ("en-US-ca", "en-US-x-lvariant-ca"),
        ("en-US-euro", "en-US-x-lvariant-euro"),
        ("en-US-posix", "en-US"),
        ("en-US-u-ca-gregory", "en-US"),
        ("en-US-x-foo", "en-US"),
        ("en-US-x-lvariant-euro", "en-US-x-lvariant-euro"),
        ("en_ABCD", "en-Abcd"),
        ("en_US", "en-US"),
        ("en_US extra", "en"),
        ("en_US-u-ca-gregory", "en-US"),
        ("en_US-u-va-posix", "en-US"),
        ("en_US.", "en-US"),
        ("en_US.ISO-8859-1", "en-US"),
        ("en_US.ISO8859-1", "en-US"),
        ("en_US.UTF-8", "en-US"),
        ("en_US.UTF-8 extra", "en-US"),
        ("en_US.utf8", "en-US"),
        ("en_US@", "en-US"),
        ("en_US@12", "en-US-x-lvariant-12"),
        ("en_US@1abc", "en-US-1abc"),
        ("en_US@@x", "en-US-x-lvariant-x"),
        ("en_US@EURO", "en-US-x-lvariant-euro"),
        ("en_US@a=b", "en-US"),
        ("en_US@abcd", "en-US-x-lvariant-abcd"),
        ("en_US@abcde", "en-US-abcde"),
        ("en_US@abcdefgh", "en-US-abcdefgh"),
        ("en_US@abcdefgh_ijkl", "en-US-abcdefgh-x-lvariant-ijkl"),
        ("en_US@abcdefghi", "en-US"),
        ("en_US@collation=phonebook", "en-US"),
        ("en_US@i_abcdefgh", "en-US"),
        ("en_US@x@", "en-US"),
        ("en_US@x@y", "en-US-x-lvariant-y"),
        ("en_US_12", "en-US-x-lvariant-12"),
        ("en_US_123", "en-US-x-lvariant-123"),
        ("en_US_1234", "en-US-1234"),
        ("en_US_12345", "en-US-12345"),
        ("en_US_1901_1902", "en-US-1901-1902"),
        ("en_US_1901_abcd", "en-US-1901-x-lvariant-abcd"),
        ("en_US_1901_edfg", "en-US-1901-x-lvariant-edfg"),
        ("en_US_1902_1901", "en-US-1901-1902"),
        ("en_US_1_abcd", "en-US"),
        ("en_US_1abc", "en-US-1abc"),
        ("en_US_ABCD_efgh", "en-US-x-lvariant-abcd-efgh"),
        ("en_US_POSIX", "en-US"),
        ("en_US_a", "en-US-x-lvariant-a"),
        ("en_US_a1b2c3d4", "en-US-a1b2c3d4"),
        ("en_US_abcd1", "en-US-abcd1"),
        ("en_US_abcd_efgh", "en-US-x-lvariant-abcd-efgh"),
        ("en_US_abcdefgh_1901", "en-US-1901-abcdefgh"),
        ("en_US_abcdefgh_ijkl", "en-US-abcdefgh-x-lvariant-ijkl"),
        ("en_US_euro", "en-US-x-lvariant-euro"),
        ("en_US_extra", "en-US-extra"),
        ("en_US_posix", "en-US"),
        ("en_ab_cd", "en-AB-x-lvariant-cd"),
        ("en_abc_abcd", "en-x-lvariant-abcd"),
        ("en_abc_def", "en-x-lvariant-def"),
        ("en_us", "en-US"),
        ("eng", "en"),
        ("eng_US", "en-US"),
        ("engl", "engl"),
        ("english", "english"),
        ("engs", "engs"),
        ("enx", "enx"),
        ("es_419", "es-419"),
        ("est_EE", "et-EE"),
        ("eus_ES", "eu-ES"),
        ("fao_FO", "fo-FO"),
        ("fas_IR", "fa-IR"),
        ("fil_PH", "fil-PH"),
        ("fin_FI", "fi-FI"),
        ("fra_FR", "fr-FR"),
        ("fre_FR", "fre-FR"),
        ("ger_DE", "ger-DE"),
        ("gle_IE", "ga-IE"),
        ("glg_ES", "gl-ES"),
        ("guj_IN", "gu-IN"),
        ("heb_IL", "he-IL"),
        ("hin_IN", "hi-IN"),
        ("hrv_HR", "hr-HR"),
        ("hun_HU", "hu-HU"),
        ("hye_AM", "hy-AM"),
        ("in_ID", "id-ID"),
        ("ind_ID", "id-ID"),
        ("isl_IS", "is-IS"),
        ("ita_IT", "it-IT"),
        ("iw_IL", "he-IL"),
        ("ja_JP\t", "ja"),
        ("ja_JP ", "ja"),
        ("ja_JP.UTF-8", "ja-JP"),
        ("ja_JP.UTF-8 ", "ja-JP"),
        ("ja_JP.UTF-8@mod", "ja-JP-x-lvariant-mod"),
        ("ja_JP.UTF8", "ja-JP"),
        ("ja_JP.eucJP", "ja-JP"),
        ("jaw_ID", "jv-ID"),
        ("ji_US", "yi-US"),
        ("jpn_JP", "ja-JP"),
        ("jw_ID", "jv-ID"),
        ("kan_IN", "kn-IN"),
        ("kat_GE", "ka-GE"),
        ("kaz_KZ", "kk-KZ"),
        ("kor_KR", "ko-KR"),
        ("lav_LV", "lv-LV"),
        ("lit_LT", "lt-LT"),
        ("mal_IN", "ml-IN"),
        ("mar_IN", "mr-IN"),
        ("mkd_MK", "mk-MK"),
        ("mo_MD", "ro-MD"),
        ("mon_MN", "mn-MN"),
        ("msa_MY", "ms-MY"),
        ("nb_NO", "nb-NO"),
        ("nep_NP", "ne-NP"),
        ("nld_NL", "nl-NL"),
        ("nno_NO", "nn-NO"),
        ("no_NO", "no-NO"),
        ("nob_NO", "nb-NO"),
        ("pOsix", "posix"),
        ("pan_IN", "pa-IN"),
        ("pol_PL", "pl-PL"),
        ("por_PT", "pt-PT"),
        ("posix", "posix"),
        ("posix@euro", "posix-x-lvariant-euro"),
        ("ron_RO", "ro-RO"),
        ("rus_RU", "ru-RU"),
        ("sh_RS", "sh-RS"),
        ("slk_SK", "sk-SK"),
        ("slv_SI", "sl-SI"),
        ("spa_ES", "es-ES"),
        ("sqi_AL", "sq-AL"),
        ("sr_Cyrl", "sr-Cyrl"),
        ("sr_Latn_RS", "sr-Latn-RS"),
        ("sr_RS.UTF-8@latin", "sr-RS-latin"),
        ("sr_RS@latin", "sr-RS-latin"),
        ("sr_RS_Latn", "sr-RS-x-lvariant-latn"),
        ("srp_RS", "sr-RS"),
        ("swa_KE", "sw-KE"),
        ("swe_SE", "sv-SE"),
        ("tam_IN", "ta-IN"),
        ("tel_IN", "te-IN"),
        ("tgl_PH", "tl-PH"),
        ("th_TH", "th-TH"),
        ("th_TH.UTF-8", "th-TH"),
        ("tha_TH", "th-TH"),
        ("tl_PH", "tl-PH"),
        ("tnf_AF", "prs-AF"),
        ("tur_TR", "tr-TR"),
        ("twi_GH", "tw-GH"),
        ("ukr_UA", "uk-UA"),
        ("und", "und"),
        ("und-US", "und-US"),
        ("urd_PK", "ur-PK"),
        ("vie_VN", "vi-VN"),
        ("wil_AU", "wil-AU"),
        ("x ", "und"),
        ("x y", "und"),
        ("x-y", "und"),
        ("x.y", "und"),
        ("yue_HK", "yue-HK"),
        ("zh_CN", "zh-CN"),
        ("zh_Hans", "zh-Hans"),
        ("zh_Hans_CN", "zh-Hans-CN"),
        ("zh_Hant_HK", "zh-Hant-HK"),
        ("zh_Hant_TW", "zh-Hant-TW"),
        ("zh_TW.UTF-8@collation=phonebook", "zh-TW"),
        ("zho_CN", "zh-CN"),
        ("zul_ZA", "zu-ZA"),
        ("zzz_ZZ", "zzz-ZZ"),
    ];
    /// 278 个 POSIX locale 值 → Node 26.12.1 的 `new Intl.Collator().resolvedOptions().locale`。
    ///
    /// 黄金值由 `LC_ALL=<value> node -p ...` 逐个生成（`env -i`，无其它环境变量）。
    #[test]
    fn normalize_locale_matches_node() {
        let mut failures = Vec::new();
        for &(input, expected) in LOCALE_CASES {
            let got = normalize_locale(input);
            if got != expected {
                failures.push(format!("{input:?}: node={expected:?} rust={got:?}"));
            }
        }
        assert!(
            failures.is_empty(),
            "{} mismatches:\n{}",
            failures.len(),
            failures.join("\n")
        );
    }

    /// 环境变量优先级：第一个**存在**的键胜出（空值也算），都不存在时 `en-US`。
    #[test]
    fn resolve_process_locale_env_priority_matches_node() {
        let _guard = ENV_MUTEX.lock().unwrap_or_else(|error| error.into_inner());

        struct Restore(Vec<(&'static str, Option<std::ffi::OsString>)>);
        impl Drop for Restore {
            fn drop(&mut self) {
                for (key, value) in &self.0 {
                    match value {
                        Some(value) => unsafe { std::env::set_var(key, value) },
                        None => unsafe { std::env::remove_var(key) },
                    }
                }
            }
        }
        let keys = ["LC_ALL", "LC_MESSAGES", "LANG"];
        let saved: Vec<(&'static str, Option<std::ffi::OsString>)> = keys
            .iter()
            .map(|key| (*key, std::env::var_os(key)))
            .collect();
        let _restore = Restore(saved);

        let set = |lc_all: Option<&str>, lc_messages: Option<&str>, lang: Option<&str>| {
            for (key, value) in [
                ("LC_ALL", lc_all),
                ("LC_MESSAGES", lc_messages),
                ("LANG", lang),
            ] {
                match value {
                    Some(value) => unsafe { std::env::set_var(key, value) },
                    None => unsafe { std::env::remove_var(key) },
                }
            }
        };

        // 三个都未设置 → en-US（ICU 的编译期默认）。
        set(None, None, None);
        assert_eq!(resolve_process_locale(), "en-US");

        // 已设置的空 LC_ALL 会**胜出**（Node：`und`，不落到 LANG）。
        set(Some(""), None, Some("ja_JP.UTF-8"));
        assert_eq!(resolve_process_locale(), "und");

        // 纯空白的 LC_ALL 同样胜出。
        set(Some(" "), None, Some("ja_JP.UTF-8"));
        assert_eq!(resolve_process_locale(), "und");

        // LC_MESSAGES 优先于 LANG。
        set(None, Some(""), Some("ja_JP.UTF-8"));
        assert_eq!(resolve_process_locale(), "und");
        set(None, Some("de_DE.UTF-8"), Some("ja_JP.UTF-8"));
        assert_eq!(resolve_process_locale(), "de-DE");

        // LC_ALL 优先于 LC_MESSAGES 与 LANG。
        set(
            Some("ja_JP.UTF-8"),
            Some("de_DE.UTF-8"),
            Some("fr_FR.UTF-8"),
        );
        assert_eq!(resolve_process_locale(), "ja-JP");

        // 值不 trim：前导空格让语言段失效，尾部空格不影响。
        set(Some(" ja_JP.UTF-8 "), None, None);
        assert_eq!(resolve_process_locale(), "und-JP");
        set(Some("ja_JP.UTF-8 "), None, None);
        assert_eq!(resolve_process_locale(), "ja-JP");

        // `C` / `POSIX` 关键字。
        set(Some("C"), None, Some("ja_JP.UTF-8"));
        assert_eq!(resolve_process_locale(), "en-US");
        set(Some("POSIX"), None, None);
        assert_eq!(resolve_process_locale(), "en-US");
        set(Some("c"), None, None);
        assert_eq!(resolve_process_locale(), "en-US");
        set(Some("posix"), None, None);
        assert_eq!(resolve_process_locale(), "posix");
        set(Some("C@euro"), None, None);
        assert_eq!(resolve_process_locale(), "en-US-x-lvariant-euro-posix");
        set(Some("c@euro"), None, None);
        assert_eq!(resolve_process_locale(), "und-x-lvariant-euro");
    }

    /// Node 的 `Intl.Collator` 在 `th-TH` 下把 `!` 与 `"` 视为相等（shifted 默认）：
    /// 显式 `alternate_handling: NonIgnorable` 会破坏这一点（回归 #1）。
    #[test]
    fn thai_locale_ignores_punctuation_variants_like_node() {
        assert_eq!(
            locale_compare_with("th_TH.UTF-8", "!", "\""),
            Ordering::Equal
        );
        assert_eq!(locale_compare_with("th_TH", "!", "-"), Ordering::Equal);
        // 非 Thai locale 下标点参与比较。
        assert_eq!(
            locale_compare_with("en_US.UTF-8", "!", "\""),
            Ordering::Less
        );
    }

    /// `ja-JP` 与 `und` 的假名处理不同（Node 在 ja-JP 下 kana 折叠，在 root 下不折叠）。
    #[test]
    fn japanese_and_root_locales_sort_kana_differently() {
        assert_eq!(
            locale_compare_with("ja_JP.UTF-8", "あ", "ア"),
            Ordering::Equal
        );
        assert_eq!(locale_compare_with("ja_JP", "あ", "ア"), Ordering::Equal);
        assert_eq!(
            locale_compare_with("en_US.UTF-8", "あ", "ア"),
            Ordering::Less
        );
        assert_eq!(locale_compare_with("", "あ", "ア"), Ordering::Less);
        // 空 LC_ALL（→ und）与 en-US 一致。
        assert_eq!(locale_compare_with(" ", "あ", "ア"), Ordering::Less);
    }

    /// `zh-CN` 把 CJK 排在拉丁之前，root/en-US 相反（Node 实测）。
    #[test]
    fn chinese_locale_reorders_cjk_before_latin() {
        assert_eq!(
            locale_compare_with("zh_CN.UTF-8", "文档", "z"),
            Ordering::Less
        );
        assert_eq!(
            locale_compare_with("zh_Hans_CN", "a", "文档"),
            Ordering::Greater
        );
        assert_eq!(
            locale_compare_with("en_US.UTF-8", "文档", "z"),
            Ordering::Greater
        );
        assert_eq!(
            locale_compare_with("en_US.UTF-8", "a", "文档"),
            Ordering::Less
        );
        // Node 不对 POSIX 的旧 3 字母代码做折叠（`chi-CN` 保持 root 数据）。
        assert_eq!(
            locale_compare_with("chi_CN", "文档", "z"),
            Ordering::Greater
        );
    }

    /// 旧语言代码 `sh` 在 Node 下按 `sr-Latn` 排序（`č` < `ć`），与 `sr-RS`（`č` > `ć`）不同。
    #[test]
    fn sh_locale_uses_sr_latn_collation() {
        assert_eq!(locale_compare_with("sh_RS", "č", "ć"), Ordering::Less);
        assert_eq!(locale_compare_with("sr_Latn_RS", "č", "ć"), Ordering::Less);
        assert_eq!(locale_compare_with("sr_RS", "č", "ć"), Ordering::Greater);
    }

    /// `@modifier` 相关的 locale 不影响排序（Node 只改 `resolvedOptions().locale`）。
    #[test]
    fn modifiers_do_not_change_collation() {
        assert_eq!(
            locale_compare_with("de_DE.UTF-8@euro", "ä", "a"),
            locale_compare_with("de_DE.UTF-8", "ä", "a")
        );
        assert_eq!(
            locale_compare_with("sr_RS.UTF-8@latin", "č", "ć"),
            locale_compare_with("sr_RS.UTF-8", "č", "ć")
        );
    }

    /// 带 script 的 locale 按 Node（ICU 语言优先回退）取语言级数据，而不是落回 root。
    ///
    /// 黄金值：`LC_ALL=<tag> node -e 'new Intl.Collator().compare(...)'`（Node 26.12.1）。
    #[test]
    fn script_qualified_locales_fall_back_to_language_data_like_node() {
        // ja-Latn / ja-Latn-JP 使用日语 collation（kana 折叠 → 0）。
        assert_eq!(locale_compare_with("ja-Latn", "あ", "ア"), Ordering::Equal);
        assert_eq!(
            locale_compare_with("ja-Latn-JP", "あ", "ア"),
            Ordering::Equal
        );
        assert_eq!(
            locale_compare_with("ja-Latn", "あ", "ア"),
            locale_compare_with("ja", "あ", "ア")
        );
        // zh-Latn / ko-Latn 使用中文 / 韩文 collation（CJK 在拉丁之前）。
        assert_eq!(
            locale_compare_with("zh-Latn", "中文", "abc"),
            Ordering::Less
        );
        assert_eq!(
            locale_compare_with("ko-Latn", "中文", "abc"),
            Ordering::Less
        );
        assert_eq!(
            locale_compare_with("ru-Latn", "中文", "abc"),
            Ordering::Greater
        );
        // 有脚本级数据的组合必须保留 script（Node 实测 sr-Latn / bs-Cyrl / kk-Arab / zh-Hant）。
        assert_eq!(locale_compare_with("sr-Latn", "č", "ć"), Ordering::Less);
        assert_eq!(locale_compare_with("sr-Cyrl", "č", "ć"), Ordering::Greater);
        assert_eq!(locale_compare_with("bs-Cyrl", "č", "ć"), Ordering::Greater);
        assert_eq!(locale_compare_with("bs-Latn", "č", "ć"), Ordering::Less);
        // zh-Hant 使用自己的数据：`中` < `文`，而 zh（简体数据）相反（Node 实测）。
        assert_eq!(locale_compare_with("zh-Hant", "中", "文"), Ordering::Less);
        assert_eq!(locale_compare_with("zh", "中", "文"), Ordering::Greater);
        // 未知 script 的标签也不能落到 root 的字节序回退（hr-Cyrl 用 hr 数据：č < ć）。
        assert_eq!(locale_compare_with("hr-Cyrl", "č", "ć"), Ordering::Less);
    }

    /// IANA grandfathered 标签 → Node 26.12.1 的默认 locale 解析结果。
    ///
    /// 黄金值：`LC_ALL=<value> node -p 'Intl.DateTimeFormat().resolvedOptions().locale'`。
    #[test]
    fn grandfathered_locale_aliases_match_node() {
        let cases = [
            ("zh-min-nan", "nan"),
            ("zh-hakka", "hak"),
            ("zh-xiang", "hsn"),
            ("i-klingon", "tlh"),
            ("i-lux", "lb"),
            ("i-default", "en"),
            ("i-enochian", "und"),
            ("i-mingo", "see"),
            ("i-navajo", "nv"),
            ("no-bok", "no"),
            ("no-nyn", "no"),
            ("zh-guoyu", "zh"),
            ("zh-min", "zh"),
            ("art-lojban", "jbo"),
            // 大小写 / 分隔符 / codeset / modifier。
            ("ZH-MIN-NAN", "nan"),
            ("zh_min_nan.UTF-8", "nan"),
            ("zh-hakka@euro", "hak-x-lvariant-euro"),
            ("no-bok@euro", "no-x-lvariant-euro"),
            // 通用解析本来就能得到 Node 结果，别名表不覆盖。
            ("en-GB-oed", "en-GB-x-lvariant-oed"),
            ("cel-gaulish", "cel-gaulish"),
        ];
        let mut failures = Vec::new();
        for (input, expected) in cases {
            let got = normalize_locale(input);
            if got != expected {
                failures.push(format!("{input:?}: node={expected:?} rust={got:?}"));
            }
        }
        assert!(failures.is_empty(), "{}", failures.join("\n"));
        // `zh-min-nan` 在 Node 下解析为 `nan`，排序用 root（拉丁在 CJK 之前）。
        assert_eq!(
            locale_compare_with("zh-min-nan", "中文", "abc"),
            Ordering::Greater
        );
    }

    /// `locale_is_node_valid`：Node 的 `Intl` 初始化是否接受该 locale。
    ///
    /// 黄金值：`LC_ALL=<value> node -e 'new Intl.DateTimeFormat()'`（抛
    /// `RangeError: Internal error. Icu error.` 的记 false）。
    #[test]
    fn locale_is_node_valid_matches_node() {
        let invalid = [
            "en_US_abcd_abcd",
            "en_US_abcd_ABCD",
            "en_US_abcd_efgh_abcd",
            "en_US_abcd-efgh-abcd",
            "en-US-abcd-abcd",
            "en_US_abcd@abcd",
            "en_US@abcd_abcd",
            "en_US@euro_euro",
            "de_DE@euro_euro",
            "en_US_aaa_aaa",
            "en_US_ab_ab",
            "en_US_12_12",
            "en_US_abc_abc_abcde",
            "en_US_abcd_abcd_x",
            "de_DE@euro_euro_x",
            "de_DE@ab_ab_x",
            "de_DE@euro_ab_ab",
            "de_DE@ab_ab_euro",
            "de_DE@euro_euro_euro",
            "en_US_abcd@efgh_abcd",
            "en_US_abcd@1901_abcd",
            "en_US_abcd@abcd_efgh",
            "en_US_abcd@abcd_x",
            "en_US_abcd@abcd_abcd",
            "en_US_abcd_1901_abcd",
            "en_US_abcd_abcd_1901",
            "de_DE_de_de",
            // BCP 变体重复且前面出现过 localvar。
            "en_US_abcd@1901_1901",
            "en_US_abcd_1901_1901",
            "en_US_abcd_1901@1901",
            "en_US_abcde@abcd_abcd",
            "en_US_abcd@abcde_abcde",
            "en_US_1901@abcd_abcd",
            "en_US_1901_abcd@1901",
            "en_US_abcd_abcd@1901_1901",
            "en_US_abcd@1901_1901_x",
        ];
        let valid = [
            "en_US_abcde_abcde",
            "en_US_1901_1901",
            "en_US_abcd_efgh",
            "en_US_a_a",
            "en_US_1_1",
            "en_US_POSIX_POSIX",
            "en_US_en_US",
            "en_US_abcd_x",
            "en_US_abcd_x_y",
            "en_US_abcd_x_abcd_y",
            "en_US_abcd_abcd_x_y",
            "de_DE@euro_x",
            "de_DE@euro_x_y",
            "de_DE@x_euro_euro",
            "de_DE@euro_x_euro",
            "de_DE@ab_x_ab",
            "en_US_abcd@x_abcd",
            "en_US@euro_x_euro",
            "en_US@1901_1901",
            "en_US_1901@1901",
            "en_US_abcd@1901_x_1901",
            "en_US_abcd@abcd_x_y",
            "de_DE_1901@1901",
            "de_DE@1901_1901",
            "en_US_1901_1901_abcd",
            "en_US_1901_1901@abcd",
            "en_US_1901_1901@x",
            "en_US_1901_1901_x",
            "en_US_abcde_abcde_x",
            "en_US_1901@abcd",
            "en_US_abcd@1901_abcde",
            "zh-min-nan",
            "ja-Latn",
            "en-US",
            "",
            "C",
            "POSIX",
            "en_US_123456789",
            "en_US_abcdefgh_abcdefgh",
        ];
        let mut failures = Vec::new();
        for value in invalid {
            if locale_is_node_valid(value) {
                failures.push(format!("{value:?}: node throws, rust accepts"));
            }
        }
        for value in valid {
            if !locale_is_node_valid(value) {
                failures.push(format!("{value:?}: node accepts, rust rejects"));
            }
        }
        assert!(failures.is_empty(), "{}", failures.join("\n"));
    }
    #[test]
    fn locale_compare_matches_icu_expectations() {
        assert_eq!(locale_compare_with("en-US", "a", "A"), Ordering::Less);
        assert_eq!(locale_compare_with("en-US", "a", "á"), Ordering::Less);
        assert_eq!(locale_compare_with("en-US", "a.txt", "ab"), Ordering::Less);
        assert_eq!(locale_compare_with("en-US", "a b", "ab"), Ordering::Less);
        assert_eq!(locale_compare_with("en-US", "a", "1"), Ordering::Greater);
        assert_eq!(locale_compare_with("en-US", "a", "说"), Ordering::Less);
        assert_eq!(locale_compare_with("zh", "a", "说"), Ordering::Greater);
    }

    /// `os.homedir()`：**已设置**的 `HOME`（即使为空串）原样返回；只有未设置时回退
    /// passwd 条目。`std::env::home_dir()` 会把空 `HOME` 当“未设置”，必须自己先看 `HOME`。
    #[test]
    fn homedir_matches_node_os_homedir() {
        let _guard = ENV_MUTEX.lock().unwrap_or_else(|error| error.into_inner());
        let saved = std::env::var_os("HOME");

        unsafe { std::env::set_var("HOME", "") };
        assert_eq!(homedir(), "", "HOME=\"\" must not fall back to passwd");

        unsafe { std::env::set_var("HOME", "/tmp/fake-home") };
        assert_eq!(homedir(), "/tmp/fake-home");

        unsafe { std::env::set_var("HOME", " ") };
        assert_eq!(homedir(), " ");

        // `HOME` 未设置 → passwd 条目（非空；具体值依赖机器）。
        unsafe { std::env::remove_var("HOME") };
        let fallback = homedir();
        assert!(!fallback.is_empty(), "unset HOME falls back to passwd");

        match saved {
            Some(value) => unsafe { std::env::set_var("HOME", value) },
            None => unsafe { std::env::remove_var("HOME") },
        }
    }

    /// `HOME=""` 时 `expandHomePath` 与 Node 的 `join(os.homedir(), rest)` 一致：
    /// `~/` → `./`（保留尾斜杠）、`~/x` → `x`、`~` → `""`。
    #[test]
    fn empty_home_expands_tilde_like_node() {
        let _guard = ENV_MUTEX.lock().unwrap_or_else(|error| error.into_inner());
        let saved = std::env::var_os("HOME");

        unsafe { std::env::set_var("HOME", "") };
        let provider = CombinedAutocompleteProvider::new(Vec::<CommandEntry>::new(), "/base", None);
        assert_eq!(provider.expand_home_path("~/"), "./");
        assert_eq!(provider.expand_home_path("~/x"), "x");
        assert_eq!(provider.expand_home_path("~/a/b"), "a/b");
        assert_eq!(provider.expand_home_path("~"), "");

        unsafe { std::env::set_var("HOME", "/home/user") };
        assert_eq!(provider.expand_home_path("~/"), "/home/user/");
        assert_eq!(provider.expand_home_path("~/x"), "/home/user/x");
        assert_eq!(provider.expand_home_path("~"), "/home/user");

        match saved {
            Some(value) => unsafe { std::env::set_var("HOME", value) },
            None => unsafe { std::env::remove_var("HOME") },
        }
    }

    /// fd 的 argv 顺序与追加规则（spec §2.10.3，顺序敏感）。
    #[test]
    fn fd_args_match_ts_order() {
        let args = build_fd_args("/base", "", 100, Some(1));
        assert_eq!(
            args,
            vec![
                "--base-directory",
                "/base",
                "--max-results",
                "100",
                "--type",
                "f",
                "--type",
                "d",
                "--follow",
                "--hidden",
                "--exclude",
                ".git",
                "--exclude",
                ".git/*",
                "--exclude",
                ".git/**",
                "--max-depth",
                "1",
            ]
        );

        // 查询含 `/` → 追加 `--full-path`，再追加正则。
        let args = build_fd_args("/base", "src/ma", 100, None);
        assert_eq!(&args[args.len() - 2..], ["--full-path", "src[\\\\/]ma"]);

        // 无 maxDepth、空查询 → 只到 exclude 三项为止。
        let args = build_fd_args("/base", "", 20, None);
        assert_eq!(args.len(), 16);
        assert_eq!(args[args.len() - 1], ".git/**");
    }

    /// 取消信号发生在 spawn 之前时直接返回空表（TS 的 `if (signal.aborted) resolve([])`）。
    #[tokio::test]
    async fn walk_directory_returns_empty_when_already_cancelled() {
        let signal = tokio_util::sync::CancellationToken::new();
        signal.cancel();
        let entries =
            walk_directory_with_fd("/tmp", "/definitely/not/fd", "", 100, &signal, None).await;
        assert!(entries.is_empty());
    }

    /// fd 不存在 / 不可执行 → 空表（TS 的 `child.on("error", () => finish([]))`）。
    #[tokio::test]
    async fn walk_directory_returns_empty_when_fd_is_missing() {
        let signal = tokio_util::sync::CancellationToken::new();
        let entries =
            walk_directory_with_fd("/tmp", "/definitely/not/fd", "x", 100, &signal, None).await;
        assert!(entries.is_empty());
    }

    #[test]
    fn parse_fd_stdout_filters_git_and_marks_directories() {
        let entries = parse_fd_stdout("src/\nREADME.md\n.git/config\n.git/\nfoo/.git/\n");
        // `foo/.git/` 归一化后是 `foo/.git`，不含 `"/.git/"`，TS 同样保留（逐字行为）。
        assert_eq!(
            entries,
            vec![
                FdEntry {
                    path: "src/".into(),
                    is_directory: true
                },
                FdEntry {
                    path: "README.md".into(),
                    is_directory: false
                },
                FdEntry {
                    path: "foo/.git/".into(),
                    is_directory: true
                },
            ]
        );
        assert!(parse_fd_stdout("   ").is_empty());
    }
}
