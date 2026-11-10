//! components/editor.ts（word wrap/paste/autocomplete/visual lines）。
//!
//! 多行编辑器：词感知换行、括号粘贴（大粘贴折叠为 `[paste #N …]` 标记）、历史浏览、
//! Emacs 风格 kill ring、撤销栈、字符跳转、粘性列、自动补全（防抖 + 快照校验 + 串行化）、
//! 上下滚动指示边框与鼠标命中。
//!
//! ## 与 TS 的表示差异
//!
//! 1. **UTF-16 偏移**：TS 里 `cursorCol`、`TextChunk.startIndex/endIndex`、paste marker
//!    区间、`line.length`/`slice` 全是 UTF-16 码元。本模块内部一律沿用 UTF-16 偏移，
//!    用 `utf16_slice` 系列辅助函数复刻 JS 的 `slice` 语义；落在代理对中间的切片用
//!    `U+FFFD` 代替孤立代理项（与 `crate::text` 的 `units_to_string` 约定一致）。
//! 2. **TUI 依赖**：[`EditorTui`] 只携带 Editor 真正用到的两件事——终端句柄
//!    （`rows`）与重绘回调（`requestRender`）。TS 的 `this.tui` 是完整 TUI 对象，但 Editor
//!    从不调用它的其它成员。
//! 3. **异步补全**：TS 的 provider 调用是 Promise。Rust 侧 [`Component`] 是同步 trait，
//!    因此请求在编辑器内以**可轮询 future** 的形式持有：`render` 开头与 [`Editor::tick`]
//!    会 pump 一次。TS 的 `startAutocompleteRequest` 总是 `await previousTask`（至少让出一个
//!    微任务），因此同一同步批次里的多次 `handleInput` 只有最后一次会真正调用 provider；
//!    Rust 用 `QueuedRequest` 复刻：`request_autocomplete` 只排队，provider 调用推迟到
//!    宿主处理完这一批输入后的 pump（帧边界）。future 就绪时结果立即按 TS 的快照校验规则
//!    应用；`getSuggestions` 尚未就绪时由宿主在下一次 tick/render 继续轮询。防抖（20ms）
//!    记录为绝对截止时间（[`Editor::autocomplete_deadline_ms`]），宿主据此安排唤醒
//!    （[`Editor::has_pending_autocomplete_work`] 表示是否还需要继续 tick）；宿主也可用
//!    [`Editor::set_timer`] 注入定时器，让到期的防抖自行请求重绘（TS 的 `setTimeout`）；
//!    测试可安装假时钟（[`Editor::set_clock`]）。
//!
//!    **宿主集成契约**（与 `Loader`/`TuiBase::set_timer` 相同）：组件内不持有异步运行时，
//!    20ms 防抖与在飞 future 都需要宿主驱动——要么在处理完输入后调用 [`Editor::tick`]
//!    （或渲染，[`Editor::render`] 开头会 pump），要么用 [`Editor::set_timer`] 注入真实定时器
//!    让到期的防抖请求一次重绘。默认 [`NoopTimer`] 不调度（`spec/tui-components.md` §8.2
//!    明确建议“把防抖实现为 deadline、由外部渲染循环推进”）；若宿主既不 tick 也不注入
//!    定时器，`@`/`#` 防抖补全永远不会调用 provider。
//!
//!    与 TS 的一处时序差异：TS 的 `render()` 从不推进补全状态，结果总在微任务里应用；
//!    Rust 的同步 `Component` 没有微任务边界，`render` 开头会 pump，因此 provider 的 future
//!    若**立即就绪**，菜单可能在同一帧内出现（TS 要等一个微任务）。真实事件循环里微任务
//!    先于下一帧排空，二者用户可见行为一致；仅同步测试的断言顺序会差一步。
//! 4. **provider 的临时移动**：TS 的 Promise 在 provider 上 `await`，期间 `applyCompletion`
//!    仍可调用。Rust 里请求 future 需要独占 `&mut provider`，因此 provider 在请求在飞期间
//!    被暂时移入 future（完成或被丢弃时由 `ProviderReturnGuard` 自动放回槽位）。在飞窗口内
//!    需要同步调用 provider（应用候选、`shouldTriggerFileCompletion`）时，先丢弃在飞请求
//!    取回 provider（`Editor::reclaim_provider`；丢失的结果本来也会被取消作废），因此
//!    在飞窗口内的同步调用看到的是最新 provider 状态，交互结果不受影响。
//! 5. **可覆写边框**：TS 的 `protected renderTopBorder/renderBottomBorder` 在 Rust 用
//!    [`Editor::set_top_border_renderer`]/[`Editor::set_bottom_border_renderer`] 安装回调
//!    表达（`CustomEditor` 的继承覆写点）。
//! 6. **`wordWrapLine` 的单字素保护**：若一个**单个字素**（宽字符/emoji/tab）的可见宽度
//!    大于 `maxWidth`，TS 会在"超宽原子段"分支里对自己无限递归直到抛出
//!    `RangeError: Maximum call stack size exceeded`。Rust 没有可抛出的 JS 异常，用同文案
//!    的 `panic!` 复现：`word_wrap_line("中", 1, _)` / 内容宽度 1 的 `Editor::render` 都会
//!    失败，调用方（应用）与 TS 一样终止。
//! 7. **paste registry 顺序**：`pastes` 用 `BTreeMap`（按 id 升序）。`getExpandedText` 的
//!    替换顺序因此是 id 升序，而 TS 是 `Map` 的插入顺序——只有"某个粘贴内容里恰好含有
//!    另一个有效 marker 的字面文本"时才有可观察差异；重编号路径要求升序，正好与此一致。
//! 8. **补全光标是无符号数**：`spec/tui-components.md` §5.1/§8.2 冻结的
//!    [`ApplyCompletionResult`] 用 `cursor_col: usize`。TS 的 `applyCompletion` 测试辅助函数
//!    会算出**负数** `cursorCol`（`cursorCol - prefix.length + item.value.length`），随后靠
//!    JS 的负数 `slice` 语义回绕；Rust 的 provider 契约无法表达负数（同样的算术会下溢），
//!    编辑器原样存储 provider 返回的 `usize`。自带 `CombinedAutocompleteProvider` 永远返回
//!    `>= 0`，因此只有自定义 provider 才会碰到；要逐字复刻需把该冻结契约改成有符号数。
//!
//! ## 文件结构（阅读顺序）
//!
//! 1. UTF-16 辅助函数（`utf16_*`）：本模块所有偏移运算的基础。
//! 2. paste marker 正则与 [`TextSegment`] / [`TextChunk`]：分段与换行共用的数据。
//! 3. [`word_wrap_line`]：无编辑器状态的纯函数，换行行为的唯一实现。
//! 4. [`Editor`] 的字段与多个 `impl` 块，按小节分为：依赖注入/访问器、历史、文本设置、
//!    render、`handleInput` 分派、基本编辑、视觉行/光标移动、kill ring/撤销、自动补全、
//!    分段。小节顺序即代码顺序，可跳读。
//! 5. [`Component`] / [`Focusable`] / [`EditorComponent`] 三个 trait 实现，均为薄转发。
//! 6. 文件末尾的 `tests` 只覆盖两个高风险点：触发/防抖正则的等价匹配器、paste marker
//!    重编号；编辑器其余行为在 `tests/editor.rs`、`tests/mouse_components.rs` 等集成测试。
//!
//! ## 一次按键的执行流程
//!
//! 1. 宿主把终端输入解码成字符串，经 [`Component`] 的 `handle_input` 进入
//!    [`Editor::handle_input_data`]。
//! 2. `handle_input_data` 按固定顺序分派（函数内 A–R 注释）。**多数分支命中后直接
//!    return**，例如补全菜单可见时 Tab 先应用候选而不是重新触发补全；例外见 A（字符跳转
//!    模式下遇到控制字符会清掉 `jump_mode` 后继续向下分派）与 E（`tui.select.confirm`
//!    在补全前缀以 `/` 开头时继续落到提交分支；无选中项或无 provider 时也会穿出整个 E
//!    分支）。方向键只有在首/末视觉行时才**可能**转入历史浏览（还有“编辑器为空 /
//!    正在浏览历史 / 光标在行首”等额外条件）。
//! 3. 编辑操作在改完状态后调用 `emit_change`，由 `on_change` 把整段文本交给宿主；提交时
//!    `submit_value` 先清空编辑器，再调用 `on_submit`（回调看到的是已清空的编辑器）。
//! 4. 自动补全只记录“排队项 / 防抖截止时间”，provider 的实际调用推迟到宿主下一次
//!    [`Editor::tick`] 或 `render` 时的 [`Editor::pump_autocomplete`]。
//!
//! ## 渲染流程
//!
//! [`Editor::render_editor`] 依次：pump 补全 → 限制 padding 并计算内容宽度 → 用布局宽度
//! 生成视觉行（[`word_wrap_line`]）→ 调整 `scroll_offset` 让光标可见 → 输出顶边框、可见行
//! （可见行用 `render_fake_cursor` 标出光标；焦点在编辑器时再额外输出 [`CURSOR_MARKER`]）、
//! 底边框、补全列表。
//! 返回的文本行由“左 padding + 内容 + 空格填充到内容宽度 + 右 padding”拼成，边框行使用
//! 完整的 `width`；编辑器不再对结果做二次裁剪（自定义边框回调可以返回任意内容）。
//!
//! ## 主要不变量
//!
//! - `state.lines` 至少有一行（构造函数、`set_text_internal`、提交后都保证）；
//!   `cursor_line` 是它的有效下标；`cursor_col` 是该行的 UTF-16 偏移，范围
//!   `0..=utf16_len(line)`。文本编辑都先按 UTF-16 切出光标前后缀再拼接，避免拆开代理对。
//!   该不变量由内置 `CombinedAutocompleteProvider` 与编辑器内部的编辑路径维持；自定义
//!   provider 返回的 [`ApplyCompletionResult`] 必须由 provider 实现保证有效（`lines`
//!   非空、`cursor_line`/`cursor_col` 在界内），`apply_completion` 原样写入、不做校验。
//! - 粘贴路径建立的 `pastes` 键与文本中的 `[paste #id ...]` 对应，且 id 从 1 连续；
//!   `paste_counter` 是当前最大 id。Backspace 删除 marker 时 registry、计数器和所有行中
//!   的标记会一起更新，撤销快照也包含这两者。其它删除操作（ForwardDelete、按行/按词
//!   删除）按普通字素/文本删除，没有 registry 维护逻辑，因此这种一一对应只在粘贴与
//!   Backspace 路径上保持。
//! - `autocomplete_start_token` 只增不减：防抖/排队项携带发起时的值，pump 启动前比对，
//!   过期项直接丢弃；`autocomplete_request_id` 则区分同一 token 下真正发出的请求。
//! - 请求在飞（`autocomplete_in_flight` 非空）时 provider 可能被 future 独占、槽位为
//!   `None`；此时 `provider_available()` 仍为真，但任何同步调用 provider 的路径必须先
//!   用内部的 `reclaim_provider` 取回（会丢弃该在飞请求）。
//!
//! ## 失败模式
//!
//! - 单个字素宽于 `max_width` 时 [`word_wrap_line`] 与渲染会 `panic!`（复刻 TS 的
//!   `RangeError`，见上文第 6 条）；panic 会从 `render` 冒到调用方，与 TS 的异常一致。
//! - 没有 provider 时所有补全入口静默返回，不报错；provider 的 future 若一直 `Pending`，
//!   需要宿主持续 tick，否则菜单不会出现。
//! - 括号粘贴只收到开始序列 `ESC[200~`、没有结束序列 `ESC[201~` 时，`is_in_paste` 会保持
//!   为真，后续输入全部并入粘贴缓冲，直到结束序列到达。

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::future::Future;
use std::pin::Pin;
use std::rc::Rc;
use std::sync::{Arc, LazyLock};
use std::task::{Context, Poll, Waker};

use regex::Regex;
use tokio_util::sync::CancellationToken;

use crate::autocomplete::{
    ApplyCompletionResult, AutocompleteItem, AutocompleteProvider, AutocompleteSuggestions,
    GetSuggestionsOptions,
};
use crate::components::select_list::{
    SelectItem, SelectList, SelectListLayoutOptions, SelectListTheme,
};
use crate::editor_component::{AutocompleteProviderObject, EditorBorderColor, EditorComponent};
use crate::keybindings::get_keybindings;
use crate::keys::{decode_printable_key, matches_key};
use crate::terminal_colors::Terminal;
use crate::text::{
    KillRing, PushOptions, UndoStack, WordNavigationOptions, WordSegment, cjk_break_matches,
    find_word_backward, find_word_forward, get_grapheme_segmenter, get_word_segments,
    is_autocomplete_separator, is_js_whitespace, is_whitespace_char, slice_by_column, trim_js,
    visible_width,
};
use crate::tui::{
    CURSOR_MARKER, Component, Focusable, NoopTimer, Timer, TuiMouseButton, TuiMouseEvent,
    TuiMouseEventResult, TuiMouseEventType, monotonic_clock, render_fake_cursor,
};

// ============================================================================
// UTF-16 辅助（JS `string.length` / `slice` / `indexOf` 语义）
// ============================================================================
//
// 编辑器的所有列号（`cursor_col`、`TextChunk.start_index/end_index`、paste marker 区间）
// 都是 UTF-16 偏移，因为 TS 原实现直接使用 JS 字符串下标。Rust 的 `&str` 以 UTF-8 字节
// 计，且不能切在字符中间，所以长度与切片必须先编码成 UTF-16 码元再做。这里只接受非负
// 下标；JS 的负数下标语义（从尾部计）由调用方在进入本节前自行处理。

/// `string.length`（UTF-16 码元数）。
fn utf16_len(text: &str) -> usize {
    text.encode_utf16().count()
}

/// 文本的 UTF-16 码元序列。
fn utf16_units(text: &str) -> Vec<u16> {
    text.encode_utf16().collect()
}

/// 码元序列 → 字符串；落单的代理写成 `U+FFFD`（与 `crate::text` 的约定一致）。
fn units_to_string(units: &[u16]) -> String {
    String::from_utf16_lossy(units)
}

/// `text.slice(start, end)`（UTF-16 语义；`end` 可超过长度）。
fn utf16_slice(text: &str, start: usize, end: usize) -> String {
    let units = utf16_units(text);
    let start = start.min(units.len());
    let end = end.min(units.len());
    if end <= start {
        return String::new();
    }
    units_to_string(&units[start..end])
}

/// `text.slice(0, end)`。
fn utf16_prefix(text: &str, end: usize) -> String {
    utf16_slice(text, 0, end)
}

/// `text.slice(start)`。
fn utf16_suffix(text: &str, start: usize) -> String {
    let units = utf16_units(text);
    let start = start.min(units.len());
    units_to_string(&units[start..])
}

/// `text.charCodeAt(index)`：越界或落在代理对中间时返回该码元本身。
fn utf16_unit_at(text: &str, index: usize) -> Option<u16> {
    text.encode_utf16().nth(index)
}

/// `text.indexOf(needle, from)`（UTF-16 码元匹配）。
///
/// `from` 是起始搜索位置；`needle` 为空时返回 `from.min(len)`（JS 的 `indexOf("", from)`）。
fn utf16_index_of(text: &str, needle: &str, from: usize) -> Option<usize> {
    let hay = utf16_units(text);
    let needle = utf16_units(needle);
    if needle.is_empty() {
        return Some(from.min(hay.len()));
    }
    if needle.len() > hay.len() {
        return None;
    }
    let max_start = hay.len() - needle.len();
    let mut index = from.min(hay.len());
    while index <= max_start {
        if hay[index..index + needle.len()] == needle[..] {
            return Some(index);
        }
        index += 1;
    }
    None
}

/// `text.lastIndexOf(needle, from)`（UTF-16 码元匹配；`None` = 不传 `fromIndex`）。
///
/// `from` 是允许的**匹配起点上限**，不是搜索窗口终点（JS `lastIndexOf` 的语义）：
/// needle 非空时从 `min(from, hay.len() - needle.len())` 向前找。`needle` 为空时直接
/// 返回 `from.unwrap_or(len).min(len)`（JS 的 `lastIndexOf("", from)`）。
fn utf16_last_index_of(text: &str, needle: &str, from: Option<usize>) -> Option<usize> {
    let hay = utf16_units(text);
    let needle = utf16_units(needle);
    let position = from.unwrap_or(hay.len()).min(hay.len());
    if needle.is_empty() {
        return Some(position);
    }
    if needle.len() > hay.len() {
        return None;
    }
    let max_start = hay.len() - needle.len();
    let mut index = position.min(max_start);
    loop {
        if hay[index..index + needle.len()] == needle[..] {
            return Some(index);
        }
        if index == 0 {
            return None;
        }
        index -= 1;
    }
}

// ============================================================================
// Paste marker
// ============================================================================

/// 匹配 `[paste #1 +123 lines]` / `[paste #2 1234 chars]` 的全局正则。
///
/// JS 的 `\d` 是 ASCII `[0-9]`，Rust `regex` 的 `\d` 是 Unicode 数字类，因此显式写 `[0-9]`。
/// 用 `captures_iter` 时按出现顺序产生**互不重叠**的匹配，[`marker_spans`] 依赖这一点。
static PASTE_MARKER_REGEX: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\[paste #([0-9]+)( (\+[0-9]+ lines|[0-9]+ chars))?\]")
        .expect("PASTE_MARKER_REGEX compiles")
});

/// 单段版本（`PASTE_MARKER_SINGLE`）。
static PASTE_MARKER_SINGLE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^\[paste #([0-9]+)( (\+[0-9]+ lines|[0-9]+ chars))?\]$")
        .expect("PASTE_MARKER_SINGLE compiles")
});

/// 判断一个段是否是 paste marker（即被 `segmentWithMarkers` 合并过的整段）。
///
/// 先要求 UTF-16 长度至少 10：最短的合法 marker `[paste #1]` 正好 10 个码元（TS 的
/// `isPasteMarker` 同样先检查长度，见 `spec/tui-components.md` §2.15.2）。
fn is_paste_marker(segment: &str) -> bool {
    utf16_len(segment) >= 10 && PASTE_MARKER_SINGLE.is_match(segment)
}

/// 取出 marker 的 id（`PASTE_MARKER_SINGLE` 的第一个捕获组）。
///
/// 段不是完整 marker 或数字溢出 `u64` 时返回 `None`；不会 panic。
fn paste_marker_id(segment: &str) -> Option<u64> {
    PASTE_MARKER_SINGLE
        .captures(segment)
        .and_then(|captures| captures.get(1))
        .and_then(|id| id.as_str().parse::<u64>().ok())
}

// ============================================================================
// 字素段（Intl.SegmentData 的等价物）
// ============================================================================

/// 一个字素段：`segment` 是文本，`index` 是 UTF-16 偏移。
///
/// 起点来自 `Intl.Segmenter` 的等价实现；对有效的 paste marker，`merge_markers` 会把
/// 其覆盖的多个基础段合并成一个 `TextSegment`（`segment` 为 marker 全文），使光标移动
/// 与删除把它当作原子段（不会落进 marker 内部）；换行先按整段计算宽度，段宽超过
/// `max_width` 时 [`word_wrap_line`] 会递归切分该段、产出多个显示块（编辑上仍是原子）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextSegment {
    /// 段文本。paste marker 段在这里是整个 marker 原文，而不是其中的各个字素。
    pub segment: String,
    /// UTF-16 偏移。
    pub index: usize,
}

/// `segmentWithMarkers`：把落在有效 paste marker 区间内的基础段合并成一个原子段。
///
/// `base` 返回按 `index` 升序的基础段（字素或词）。marker 的 id 不在 `valid_ids` 中时
/// 按普通文本处理。`base` 的索引必须与 `text` 的 UTF-16 偏移一致；本函数不校验。
///
/// 合并用一个只前进的 marker 游标：基础段和 marker 都按起点升序，因此单次线性扫描即可
/// （`marker_index` 从不回退）。
fn merge_markers<F>(text: &str, valid_ids: &[u64], base: F) -> Vec<TextSegment>
where
    F: FnOnce() -> Vec<(String, usize)>,
{
    // 快速路径：没有有效 id 或文本里没有 marker 前缀。
    if valid_ids.is_empty() || !text.contains("[paste #") {
        return base().into_iter().map(segment_from_pair).collect();
    }

    let markers = marker_spans(text, valid_ids);
    if markers.is_empty() {
        return base().into_iter().map(segment_from_pair).collect();
    }

    let base_segments = base();
    let mut result: Vec<TextSegment> = Vec::with_capacity(base_segments.len());
    let mut marker_index = 0usize;

    for (segment, index) in base_segments {
        // 跳过已经完全位于当前基础段之前的 marker；基础段升序遍历，游标无需回退。
        while marker_index < markers.len() && markers[marker_index].1 <= index {
            marker_index += 1;
        }
        let marker = markers.get(marker_index).copied();

        if let Some((start, end)) = marker
            && index >= start
            && index < end
        {
            // 段落在 marker 内：只有 marker 的首段产出合并段，其余丢弃。
            if index == start {
                result.push(TextSegment {
                    segment: utf16_slice(text, start, end),
                    index: start,
                });
            }
        } else {
            result.push(TextSegment { segment, index });
        }
    }

    result
}

fn segment_from_pair((segment, index): (String, usize)) -> TextSegment {
    TextSegment { segment, index }
}

// ============================================================================
// wordWrapLine
// ============================================================================

/// 换行后的一个文本块；`start_index`/`end_index` 是原行中的 UTF-16 偏移。
///
/// 同一个 [`word_wrap_line`] 返回的块首尾相接：`chunks[i].end_index` 等于
/// `chunks[i + 1].start_index`，第一块从 0 开始，最后一块到原行的 UTF-16 长度结束。
/// 因此 `utf16_slice(line, chunk.start_index, chunk.end_index)` 能还原每块，按顺序拼接
/// 即得到原行（`tests/editor.rs` 的 `word_wrap_splits_*` 用例就是这样校验的）。
/// `text` 就是该切片的结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextChunk {
    /// 块文本，等于 `utf16_slice(原行, start_index, end_index)`。
    pub text: String,
    /// 块首在原行中的 UTF-16 偏移（含）。
    pub start_index: usize,
    /// 块尾在原行中的 UTF-16 偏移（不含）。
    pub end_index: usize,
}

impl TextChunk {
    fn new(text: String, start_index: usize, end_index: usize) -> Self {
        TextChunk {
            text,
            start_index,
            end_index,
        }
    }
}

/// 把一行按词边界切成若干块（`wordWrapLine`）。
///
/// - `line` 为空或 `max_width == 0` → 单个空块（`start_index = end_index = 0`）。
/// - 整行可见宽度不超过 `max_width` → 原样一块（`end_index` 是 UTF-16 长度）。
/// - `pre_segmented` 提供时使用调用方的字素段（可含 paste marker 合并段），否则用
///   `Intl.Segmenter` 等价物。
///
/// 宽度按 [`visible_width`]（终端列数）计算，偏移一律是 UTF-16。断行优先在“换行机会”
/// 处（空白之后，或相邻字符一侧是 CJK），没有机会时才在当前位置强制断开；连续空白只在
/// 最后一个空白之后断开。返回值满足 [`TextChunk`] 的“首尾相接”不变量。
///
/// `pre_segmented` 必须是 `line` 的字素分段（`index` 为 UTF-16、升序）；本函数不校验，
/// 分段与文本不一致时切出的块会和原文对不上。
///
/// # Panics
///
/// 当**单个字素**的可见宽度大于 `max_width` 时 panic，文案是
/// `RangeError: Maximum call stack size exceeded`（复刻 TS 的无限递归，见模块文档第 6 条）。
/// 例：`word_wrap_line("中", 1, None)`。`max_width == 0` 走空块分支，不会 panic。
pub fn word_wrap_line(
    line: &str,
    max_width: usize,
    pre_segmented: Option<&[TextSegment]>,
) -> Vec<TextChunk> {
    if line.is_empty() || max_width == 0 {
        return vec![TextChunk::new(String::new(), 0, 0)];
    }

    let line_width = visible_width(line);
    if line_width <= max_width {
        return vec![TextChunk::new(line.to_string(), 0, utf16_len(line))];
    }

    let owned_segments: Vec<TextSegment>;
    let segments: &[TextSegment] = match pre_segmented {
        Some(segments) => segments,
        None => {
            owned_segments = get_grapheme_segmenter()
                .segment(line)
                .map(|segment| TextSegment {
                    segment: segment.segment.to_string(),
                    index: segment.index,
                })
                .collect();
            &owned_segments
        }
    };

    let mut chunks: Vec<TextChunk> = Vec::new();
    // `chunk_start` 是当前未完成块在原行中的起点，`current_width` 是
    // `chunk_start..当前段起点` 的可见宽度（不包含正在处理的段）。
    let mut current_width = 0usize;
    let mut chunk_start = 0usize;
    let mut wrap_opp_index: Option<usize> = None;
    let mut wrap_opp_width = 0usize;

    for (i, seg) in segments.iter().enumerate() {
        let grapheme = seg.segment.as_str();
        let g_width = visible_width(grapheme);
        let char_index = seg.index;
        let is_ws = !is_paste_marker(grapheme) && is_whitespace_char(grapheme);

        // 前进之前的溢出检查：current_width 尚未计入当前段，所以回退判定用
        // `current_width - wrap_opp_width + g_width` 表示“从最后换行机会起能否放下当前段”。
        if current_width + g_width > max_width {
            if let Some(opp_index) = wrap_opp_index
                && current_width.saturating_sub(wrap_opp_width) + g_width <= max_width
            {
                // 回退到最后一个换行机会。
                chunks.push(TextChunk::new(
                    utf16_slice(line, chunk_start, opp_index),
                    chunk_start,
                    opp_index,
                ));
                chunk_start = opp_index;
                current_width -= wrap_opp_width;
            } else if chunk_start < char_index {
                // 没有可用换行机会：强制在当前位置断行。
                chunks.push(TextChunk::new(
                    utf16_slice(line, chunk_start, char_index),
                    chunk_start,
                    char_index,
                ));
                chunk_start = char_index;
                current_width = 0;
            }
            wrap_opp_index = None;
        }

        if g_width > max_width {
            // 单个原子段比整行还宽（例如窄终端里的 paste marker）：递归按字素切分。
            //
            // 若该段本身只有一个字素（宽字符/emoji/tab 在极窄宽度下），再切分不可能：
            // TS 会对 `wordWrapLine(grapheme, maxWidth)` 无限递归，最终抛出
            // `RangeError: Maximum call stack size exceeded`（V8 栈深约 1 万帧）。
            // Rust 没有可抛出的 JS 异常，用同文案的 panic 复现"该输入必然失败"：
            // 与 TS 一样，异常会从 `render()` 一路冒到调用方、应用终止。
            let mut sub_segments = get_grapheme_segmenter().segment(grapheme);
            let is_single_grapheme = sub_segments.next().is_some() && sub_segments.next().is_none();
            if is_single_grapheme {
                panic!("RangeError: Maximum call stack size exceeded");
            }
            let sub_chunks = word_wrap_line(grapheme, max_width, None);
            for sub in &sub_chunks[..sub_chunks.len() - 1] {
                chunks.push(TextChunk::new(
                    sub.text.clone(),
                    char_index + sub.start_index,
                    char_index + sub.end_index,
                ));
            }
            let last = sub_chunks.last().expect("non-empty");
            chunk_start = char_index + last.start_index;
            current_width = visible_width(&last.text);
            wrap_opp_index = None;
            continue;
        }

        current_width += g_width;

        // 记录换行机会：空白后接非空白（连续空白合并，断点在最后一个空白之后），
        // 或任一侧是 CJK（CJK 允许任意相邻字符间断行）。断点记在 `next.index`，即在
        // 下一个段之前切开，当前空白留在上一块里。
        let next = segments.get(i + 1);
        if is_ws
            && let Some(next) = next
            && (is_paste_marker(&next.segment) || !is_whitespace_char(&next.segment))
        {
            wrap_opp_index = Some(next.index);
            wrap_opp_width = current_width;
        } else if !is_ws
            && let Some(next) = next
            && !is_whitespace_char(&next.segment)
        {
            let is_cjk = !is_paste_marker(grapheme) && cjk_break_matches(grapheme);
            let next_is_cjk = !is_paste_marker(&next.segment) && cjk_break_matches(&next.segment);
            if is_cjk || next_is_cjk {
                wrap_opp_index = Some(next.index);
                wrap_opp_width = current_width;
            }
        }
    }

    // 收尾块覆盖到行尾；在 `pre_segmented` 与文本一致的前提下，原行非空时它一定非空
    // （每次切分都把 chunk_start 推进到下一个段的起点，而末尾段不会再产生换行机会）。
    chunks.push(TextChunk::new(
        utf16_suffix(line, chunk_start),
        chunk_start,
        utf16_len(line),
    ));

    chunks
}

// ============================================================================
// 滚动指示边框
// ============================================================================

/// `createScrollBorder(direction, hiddenLineCount, width)`。
///
/// 按可用宽度依次尝试三种形态：
/// 1. 标签（` ↑ N more `，前后各一空格）外还能余出 2 列：标签居中，两侧用 `─` 填满；
/// 2. 放得下左对齐的 `indicator`（`─── ↑ N more `）：右侧补 `─`；
/// 3. 都放不下：把 `indicator` 按列宽截到 `width - visible_width(ellipsis)` 后拼 `...`，
///    保证不切坏宽字符。
///
/// `width == 0` 时返回空串。返回的是未着色文本；调用方
/// [`Editor::base_top_border`]/[`Editor::base_bottom_border`] 再统一着色。
fn create_scroll_border(direction: &str, hidden_line_count: usize, width: usize) -> String {
    let available_width = width;
    let label = format!(" {direction} {hidden_line_count} more ");
    let label_width = visible_width(&label);
    if label_width + 2 <= available_width {
        let left_width = (available_width - label_width) / 2;
        return format!(
            "{}{}{}",
            "─".repeat(left_width),
            label,
            "─".repeat(available_width - left_width - label_width)
        );
    }

    let indicator = format!("─── {direction} {hidden_line_count} more ");
    let remaining = available_width.saturating_sub(visible_width(&indicator));
    // `remaining >= 0` 在 usize 上恒真（JS 的 `availableWidth - visibleWidth(indicator)` 可能为负，
    // 此时 `remaining >= 0` 为 false，落到省略号分支）。
    if visible_width(&indicator) <= available_width {
        return format!("{indicator}{}", "─".repeat(remaining));
    }

    let ellipsis: String = "...".chars().take(available_width).collect();
    let indicator_width = available_width.saturating_sub(visible_width(&ellipsis));
    format!(
        "{}{}",
        slice_by_column(&indicator, 0, indicator_width, true),
        ellipsis
    )
}

// ============================================================================
// 常量 / 类型
// ============================================================================

/// 斜杠命令下拉的布局（TS `SLASH_COMMAND_SELECT_LIST_LAYOUT`）。
const SLASH_COMMAND_MIN_PRIMARY_COLUMN_WIDTH: f64 = 12.0;
/// 见 [`SLASH_COMMAND_MIN_PRIMARY_COLUMN_WIDTH`]。
const SLASH_COMMAND_MAX_PRIMARY_COLUMN_WIDTH: f64 = 32.0;

/// 附件补全防抖毫秒数（TS `ATTACHMENT_AUTOCOMPLETE_DEBOUNCE_MS`）。
const ATTACHMENT_AUTOCOMPLETE_DEBOUNCE_MS: u64 = 20;
/// 默认触发字符（TS `DEFAULT_AUTOCOMPLETE_TRIGGER_CHARACTERS`）。
const DEFAULT_AUTOCOMPLETE_TRIGGER_CHARACTERS: [char; 2] = ['@', '#'];
/// 自动补全下拉默认最大可见行数。
const DEFAULT_AUTOCOMPLETE_MAX_VISIBLE: usize = 5;

/// 编辑器状态（TS `EditorState`）。
///
/// 不变量：`lines` 至少一行（构造函数、`set_text_internal`、提交后都保证）；
/// `cursor_line` 是 `lines` 的有效下标；`cursor_col` 是该行的 UTF-16 偏移，且不超过该行
/// 长度。整个结构会被 `EditorSnapshot` 克隆进撤销栈，所以任何改变文本或光标的操作都
/// 必须在改完后保持一致（不要只改一半）。这些不变量由内置 `CombinedAutocompleteProvider`
/// 与内部编辑路径维持；自定义 provider 返回的 `ApplyCompletionResult` 必须由 provider
/// 实现保证有效，编辑器原样写入、不做校验。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct EditorState {
    /// 逻辑行（不含换行符）；`get_text` 用 `\n` 把它们连起来。
    pub lines: Vec<String>,
    /// 光标所在行，是 `lines` 的下标。
    pub cursor_line: usize,
    /// UTF-16 码元偏移。
    pub cursor_col: usize,
}

/// 撤销快照：编辑器文本 + paste registry。
#[derive(Debug, Clone, PartialEq, Eq)]
struct EditorSnapshot {
    state: EditorState,
    pastes: BTreeMap<u64, String>,
    paste_counter: u64,
}

/// 一条布局行（TS `LayoutLine`）。
#[derive(Debug, Clone, PartialEq, Eq)]
struct LayoutLine {
    text: String,
    has_cursor: bool,
    cursor_pos: Option<usize>,
}

/// 视觉行（`buildVisualLineMap` 的元素）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct VisualLine {
    logical_line: usize,
    start_col: usize,
    length: usize,
}

/// 边框渲染回调（TS 的 `protected renderTopBorder/renderBottomBorder` 覆写点）。
///
/// 参数：`width`、隐藏行数、边框着色函数、**默认实现已着色好的边框**（即基类渲染结果，
/// 可直接返回或在其基础上拼接）。
pub type BorderRenderFn =
    Arc<dyn Fn(usize, usize, &dyn Fn(&str) -> String, &str) -> String + Send + Sync>;

/// 文本回调（TS `onSubmit` / `onChange` 的 `(text: string) => void`）。
///
/// 同步调用：编辑器在回调返回后才继续当前操作。没有 `Send` 约束，与 [`Editor`] 一样只能
/// 在创建它的线程上使用。
pub type EditorTextCallback = Box<dyn FnMut(&str)>;

/// 编辑器主题（TS `EditorTheme`）。
#[derive(Clone)]
pub struct EditorTheme {
    /// 边框着色：接收整条边框文本，返回带 ANSI 的字符串。[`EditorTheme::plain`] 使用恒等
    /// 函数（不加颜色）。
    pub border_color: Arc<dyn Fn(&str) -> String>,
    /// 补全下拉列表的主题（选中行高亮等），见 [`SelectListTheme`]。
    pub select_list: SelectListTheme,
}

impl EditorTheme {
    /// 恒等主题：边框不加 ANSI，列表用 [`SelectListTheme::plain`]。
    pub fn plain() -> Self {
        EditorTheme {
            border_color: Arc::new(|text: &str| text.to_string()),
            select_list: SelectListTheme::plain(),
        }
    }
}

impl std::fmt::Debug for EditorTheme {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EditorTheme")
            .field("border_color", &"<fn>")
            .field("select_list", &"<select list theme>")
            .finish()
    }
}

/// 编辑器选项（TS `EditorOptions`）。
///
/// 两个字段都在 [`Editor::new`] 里归一化；`None` 与非法值（NaN/∞）都落回默认值，不会
/// panic。对应的 setter 使用同一套规则。
#[derive(Debug, Clone, Default)]
pub struct EditorOptions {
    /// 水平内边距（列）。构造时 `max(0, floor(v))`；渲染时再限制为不超过
    /// `(width - 1) / 2`，避免两侧 padding 占满整行。
    pub padding_x: Option<f64>,
    /// 补全下拉的最大可见行数。构造时 `floor(v)` 后夹到 `[3, 20]`；`None`/非有限值用 5。
    pub autocomplete_max_visible: Option<f64>,
}

/// Editor 对 TUI 的最小依赖（TS `TUI` 中 Editor 只用 `terminal.rows` 与 `requestRender()`）。
#[derive(Clone)]
pub struct EditorTui {
    /// 终端句柄；编辑器只调用它的 `rows()`（可见行数上限与翻页大小）。
    pub terminal: Rc<RefCell<Box<dyn Terminal>>>,
    /// 请求宿主重绘；编辑器不会自己渲染，只在状态需要重新绘制时调用它。
    pub request_render: Rc<dyn Fn()>,
}

impl EditorTui {
    /// 显式提供终端与重绘回调。
    ///
    /// 两个成员都是 `Rc`，因此 [`Editor`]（持有本结构）不是 `Send`，只能在单个线程内使用。
    pub fn new(terminal: Rc<RefCell<Box<dyn Terminal>>>, request_render: Rc<dyn Fn()>) -> Self {
        EditorTui {
            terminal,
            request_render,
        }
    }

    /// 只给终端；重绘回调为 no-op（纯测试/一次性渲染场景）。
    ///
    /// 注意依赖 `requestRender` 的路径（设置 padding/最大可见数、补全结果落地）不会触发
    /// 重绘，宿主需要自行安排渲染。
    pub fn from_terminal(terminal: Rc<RefCell<Box<dyn Terminal>>>) -> Self {
        EditorTui {
            terminal,
            request_render: Rc::new(|| {}),
        }
    }

    fn rows(&self) -> usize {
        self.terminal.borrow().rows()
    }

    fn request_render(&self) {
        (self.request_render)();
    }
}

impl std::fmt::Debug for EditorTui {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EditorTui").finish_non_exhaustive()
    }
}

/// 撤销合并状态（TS `lastAction`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LastAction {
    Kill,
    Yank,
    TypeWord,
}

/// 字符跳转方向。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum JumpDirection {
    Forward,
    Backward,
}

/// 自动补全状态（TS `autocompleteState`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AutocompleteState {
    Regular,
    Force,
}

/// 一次补全请求的选项。
#[derive(Debug, Clone, Copy)]
struct RequestOptions {
    force: bool,
    explicit_tab: bool,
}

/// 防抖中的请求。
#[derive(Debug, Clone, Copy)]
struct PendingDebounce {
    start_token: u64,
    /// 绝对截止时间（毫秒，来自 [`Editor::clock`]）。
    deadline_ms: f64,
    options: RequestOptions,
}

/// 在飞的补全请求。
struct InFlightRequest {
    future: Pin<Box<dyn Future<Output = Option<AutocompleteSuggestions>>>>,
    request_id: u64,
    /// 发起时的 start token（TS 的 `startToken` 校验；被更新的请求淘汰时结果直接丢弃）。
    #[allow(dead_code)]
    start_token: u64,
    controller: CancellationToken,
    snapshot_text: String,
    snapshot_line: usize,
    snapshot_col: usize,
    force: bool,
    explicit_tab: bool,
}

/// 排队等待在飞请求结束后启动的补全请求（TS `autocompleteRequestTask` 链里最新的一项）。
///
/// TS 的 `startAutocompleteRequest` 把任务挂在 `previousTask` 之后，并在任务真正运行时才
/// 检查 `startToken`/取快照/调用 provider；同一同步批次里的旧任务会因 token 过期而空转，
/// 因此只保留最新一项即可。
#[derive(Debug, Clone, Copy)]
struct QueuedRequest {
    start_token: u64,
    options: RequestOptions,
}

/// provider 被在飞请求占用时推迟判定的 force 请求。
///
/// TS 的 `requestAutocomplete` 在请求时就调用 `shouldTriggerFileCompletion`（provider 永远
/// 可用）；Rust 的 provider 在请求在飞期间被 future 独占，无法调用。这里记下请求时的快照，
/// 等 provider 归位后按同一快照补做判定，效果与请求时判定一致：
/// - 返回 false → 在飞请求不受影响，其结果照常应用（TS 此时直接 return，也未取消它）；
/// - 返回 true → 丢弃在飞请求的结果并启动本次 force 请求（TS 此时 abort 了它）。
struct DeferredForceRequest {
    options: RequestOptions,
    lines: Vec<String>,
    cursor_line: usize,
    cursor_col: usize,
}

/// provider 的共享槽位：请求 future 暂存 provider，完成或丢弃时放回。
type ProviderCell = Rc<RefCell<Option<Box<dyn AutocompleteProvider>>>>;

/// 请求 future 结束（包括被丢弃）时把 provider 放回槽位。
struct ProviderReturnGuard {
    cell: ProviderCell,
    provider: Option<Box<dyn AutocompleteProvider>>,
}

impl Drop for ProviderReturnGuard {
    fn drop(&mut self) {
        if let Some(provider) = self.provider.take() {
            *self.cell.borrow_mut() = Some(provider);
        }
    }
}

/// 自动补全结果（`applyCompletion` 的输入项）。
fn to_select_item(item: &AutocompleteItem) -> SelectItem {
    let mut select = SelectItem::with_label(item.value.clone(), item.label.clone());
    if let Some(description) = &item.description {
        select = select.with_description(description.clone());
    }
    select
}

// ============================================================================
// Editor
// ============================================================================

/// 多行编辑器（TS `class Editor implements Component, Focusable`）。
pub struct Editor {
    state: EditorState,

    /// 获得焦点时在光标处输出 [`CURSOR_MARKER`]。
    focused: bool,

    tui: EditorTui,
    theme: EditorTheme,
    padding_x: usize,

    /// 最近一次 render 的布局宽度（光标导航与鼠标命中共用）。
    last_width: usize,
    rendered_visible_line_count: usize,
    rendered_autocomplete_height: usize,

    /// 垂直滚动偏移。
    scroll_offset: usize,

    /// 边框着色（TS `public borderColor`）。
    pub border_color: Arc<dyn Fn(&str) -> String>,
    /// TS `protected renderTopBorder` 的覆写点。
    top_border_renderer: Option<BorderRenderFn>,
    /// TS `protected renderBottomBorder` 的覆写点。
    bottom_border_renderer: Option<BorderRenderFn>,

    // --- 自动补全 ---
    autocomplete_provider: ProviderCell,
    autocomplete_trigger_characters: Vec<char>,
    autocomplete_list: Option<SelectList>,
    /// 下拉列表的 `onSelect` 回调无法借用 `&mut Editor`，因此先把选中项暂存在这里，
    /// 交互返回后由编辑器取走并应用（见 [`Editor::drain_autocomplete_selection`]）。
    autocomplete_selected: Rc<RefCell<Option<SelectItem>>>,
    autocomplete_state: Option<AutocompleteState>,
    autocomplete_prefix: String,
    autocomplete_max_visible: usize,
    autocomplete_controller: Option<CancellationToken>,
    autocomplete_in_flight: Option<InFlightRequest>,
    autocomplete_pending_debounce: Option<PendingDebounce>,
    autocomplete_queued: Option<QueuedRequest>,
    autocomplete_deferred_force: Option<DeferredForceRequest>,
    autocomplete_start_token: u64,
    autocomplete_request_id: u64,

    // --- paste 跟踪 ---
    pastes: BTreeMap<u64, String>,
    paste_counter: u64,

    // --- 括号粘贴缓冲 ---
    paste_buffer: String,
    is_in_paste: bool,

    // --- prompt 历史 ---
    history: Vec<String>,
    /// -1 = 未浏览；0 = 最近一条。
    history_index: isize,
    history_draft: Option<EditorState>,

    // --- kill ring ---
    kill_ring: KillRing,
    last_action: Option<LastAction>,

    // --- 字符跳转 ---
    jump_mode: Option<JumpDirection>,

    /// 垂直移动的粘性列。
    preferred_visual_col: Option<usize>,
    /// 光标吸附到原子段起点前的 `cursor_col`。
    snapped_from_cursor_col: Option<usize>,

    // --- 撤销 ---
    undo_stack: UndoStack<EditorSnapshot>,

    /// 提交回调（TS `onSubmit`）。
    pub on_submit: Option<EditorTextCallback>,
    /// 文本变化回调（TS `onChange`）。
    pub on_change: Option<EditorTextCallback>,
    /// 禁止提交（TS `public disableSubmit`）。
    pub disable_submit: bool,

    /// 时间源（毫秒，单调）。防抖截止时间用它计算；测试可安装假时钟。
    clock: Rc<dyn Fn() -> f64>,

    /// 防抖定时器（TS 的 `setTimeout`）。默认 [`NoopTimer`]：不调度，由宿主用
    /// [`Editor::autocomplete_deadline_ms`] + [`Editor::tick`] 驱动；宿主也可
    /// [`Editor::set_timer`] 注入实现，让到期的防抖主动请求一次重绘。
    timer: Box<dyn Timer>,
}

impl Editor {
    /// `constructor(tui, theme, options = {})`。
    ///
    /// 初始状态是单行空文本、光标 `(0, 0)`。`last_width` 先置 80（首次 render 之前的
    /// 光标移动/鼠标命中使用这个占位宽度），`rendered_visible_line_count` 置 1、
    /// `rendered_autocomplete_height` 置 0。选项归一化规则见 [`EditorOptions`]。
    pub fn new(tui: EditorTui, theme: EditorTheme, options: EditorOptions) -> Self {
        let padding_x = options
            .padding_x
            .map(|value| {
                if value.is_finite() {
                    value.max(0.0).floor() as usize
                } else {
                    0
                }
            })
            .unwrap_or(0);
        let autocomplete_max_visible = options
            .autocomplete_max_visible
            .map(|value| {
                if value.is_finite() {
                    let floored = value.floor();
                    floored.clamp(3.0, 20.0) as usize
                } else {
                    DEFAULT_AUTOCOMPLETE_MAX_VISIBLE
                }
            })
            .unwrap_or(DEFAULT_AUTOCOMPLETE_MAX_VISIBLE);
        let border_color = Arc::clone(&theme.border_color);
        Editor {
            state: EditorState {
                lines: vec![String::new()],
                cursor_line: 0,
                cursor_col: 0,
            },
            focused: false,
            tui,
            theme,
            padding_x,
            last_width: 80,
            rendered_visible_line_count: 1,
            rendered_autocomplete_height: 0,
            scroll_offset: 0,
            border_color,
            top_border_renderer: None,
            bottom_border_renderer: None,
            autocomplete_provider: Rc::new(RefCell::new(None)),
            autocomplete_trigger_characters: DEFAULT_AUTOCOMPLETE_TRIGGER_CHARACTERS.to_vec(),
            autocomplete_list: None,
            autocomplete_selected: Rc::new(RefCell::new(None)),
            autocomplete_state: None,
            autocomplete_prefix: String::new(),
            autocomplete_max_visible,
            autocomplete_controller: None,
            autocomplete_in_flight: None,
            autocomplete_pending_debounce: None,
            autocomplete_queued: None,
            autocomplete_deferred_force: None,
            autocomplete_start_token: 0,
            autocomplete_request_id: 0,
            pastes: BTreeMap::new(),
            paste_counter: 0,
            paste_buffer: String::new(),
            is_in_paste: false,
            history: Vec::new(),
            history_index: -1,
            history_draft: None,
            kill_ring: KillRing::new(),
            last_action: None,
            jump_mode: None,
            preferred_visual_col: None,
            snapped_from_cursor_col: None,
            undo_stack: UndoStack::new(),
            on_submit: None,
            on_change: None,
            disable_submit: false,
            clock: Rc::new(monotonic_clock),
            timer: Box::new(NoopTimer),
        }
    }

    // ------------------------------------------------------------------
    // 依赖注入 / 访问器
    // ------------------------------------------------------------------

    /// 安装时间源（毫秒，单调）。默认 [`monotonic_clock`]。
    ///
    /// 防抖截止时间在请求时算成“当前时钟 + 防抖时长”的绝对值，之后直接与时钟比较，因此
    /// 时钟应保持单调（或至少与安装时同一基准）；测试可用假时钟推进。
    pub fn set_clock(&mut self, clock: Rc<dyn Fn() -> f64>) {
        self.clock = clock;
    }

    /// 安装防抖定时器（TS 里 `setTimeout` 由 JS 事件循环提供）。
    ///
    /// 安装后，防抖请求到点会主动调用一次 `requestRender`，宿主随即渲染并由
    /// [`Editor::pump_autocomplete`] 启动请求——复刻 TS"20ms 后无需再按键就出现菜单"。
    /// 默认 [`NoopTimer`] 不调度，此时由宿主按 [`Editor::autocomplete_deadline_ms`] 安排
    /// [`Editor::tick`]。
    pub fn set_timer(&mut self, timer: Box<dyn Timer>) {
        self.timer.cancel();
        self.timer = timer;
        if let Some(pending) = self.autocomplete_pending_debounce {
            let delay_ms = (pending.deadline_ms - (self.clock)()).max(0.0).ceil() as u64;
            self.arm_debounce_timer(delay_ms);
        }
    }

    /// 安排防抖定时器：到点只请求一次重绘（真正的请求由 `render`/`tick` 的 pump 发起）。
    fn arm_debounce_timer(&mut self, delay_ms: u64) {
        let request_render = Rc::clone(&self.tui.request_render);
        self.timer.set(delay_ms, Box::new(move || request_render()));
    }

    /// 当前时间（毫秒）。
    pub fn now_ms(&self) -> f64 {
        (self.clock)()
    }

    /// TS `protected renderTopBorder` 的覆写点。
    ///
    /// `Some` 时替代默认上边框渲染；`None` 恢复默认实现。回调参数见 [`BorderRenderFn`]。
    pub fn set_top_border_renderer(&mut self, renderer: Option<BorderRenderFn>) {
        self.top_border_renderer = renderer;
    }

    /// TS `protected renderBottomBorder` 的覆写点。
    ///
    /// `Some` 时替代默认下边框渲染；`None` 恢复默认实现。
    pub fn set_bottom_border_renderer(&mut self, renderer: Option<BorderRenderFn>) {
        self.bottom_border_renderer = renderer;
    }

    /// `getPaddingX()`。
    pub fn get_padding_x(&self) -> usize {
        self.padding_x
    }

    /// `setPaddingX(padding)`。
    ///
    /// 非法值按 0；只有值真的变化时才请求重绘。渲染时实际使用的 padding 还会被
    /// `(width - 1) / 2` 限制（见 [`Editor::render_editor`]）。
    pub fn set_padding_x(&mut self, padding: f64) {
        let new_padding = if padding.is_finite() {
            padding.max(0.0).floor() as usize
        } else {
            0
        };
        if self.padding_x != new_padding {
            self.padding_x = new_padding;
            self.tui.request_render();
        }
    }

    /// `getAutocompleteMaxVisible()`。
    pub fn get_autocomplete_max_visible(&self) -> usize {
        self.autocomplete_max_visible
    }

    /// `setAutocompleteMaxVisible(maxVisible)`。
    ///
    /// 与构造选项同一套归一化：非有限值回到 5，其余 `floor` 后夹到 `[3, 20]`；只有值变化
    /// 时才请求重绘。
    pub fn set_autocomplete_max_visible(&mut self, max_visible: f64) {
        let new_max_visible = if max_visible.is_finite() {
            max_visible.floor().clamp(3.0, 20.0) as usize
        } else {
            DEFAULT_AUTOCOMPLETE_MAX_VISIBLE
        };
        if self.autocomplete_max_visible != new_max_visible {
            self.autocomplete_max_visible = new_max_visible;
            self.tui.request_render();
        }
    }

    /// `setAutocompleteProvider(provider)`。
    ///
    /// 先取消现有补全并丢弃在飞请求（其 `ProviderReturnGuard` 会把旧 provider 放回槽位，
    /// 随后立即被新 provider 覆盖；旧结果即便到达也会因取消信号/请求 id 与快照校验作废）。
    /// 触发字符是**默认 `@`/`#` 与 `provider.trigger_characters()` 的并集**，不是替换；
    /// 过滤规则见 `set_autocomplete_trigger_characters`（拒绝 `/`、空白、非单个 UTF-16
    /// 码元的字符和重复项）。
    pub fn set_autocomplete_provider(&mut self, provider: Box<dyn AutocompleteProvider>) {
        self.cancel_autocomplete();
        // 旧 provider 可能还被在飞 future 独占；先丢弃该 future 把它取回来，
        // 否则它的 Drop guard 稍后会把旧 provider 写回槽位、覆盖新 provider。
        self.autocomplete_in_flight = None;
        let trigger_characters = provider.trigger_characters().unwrap_or_default();
        *self.autocomplete_provider.borrow_mut() = Some(provider);
        self.set_autocomplete_trigger_characters(&trigger_characters);
    }

    /// 是否显示自动补全下拉（`isShowingAutocomplete()`）。
    ///
    /// 纯查询：不会推进防抖/在飞请求。宿主要让到期的防抖定时器生效，
    /// 需调用 [`Editor::tick`]（或触发一次 `render`）。
    pub fn is_showing_autocomplete(&self) -> bool {
        self.autocomplete_state.is_some()
    }

    /// 当前文本（`getText()`）。
    ///
    /// 用 `\n` 连接逻辑行；paste marker 保持字面形式，展开请用
    /// [`Editor::get_expanded_text`]。
    pub fn get_text(&self) -> String {
        self.state.lines.join("\n")
    }

    /// 展开 paste marker 后的文本（`getExpandedText()`）。
    ///
    /// 按 registry 中 id 升序逐个替换所有对应 marker；registry 里不存在的 id（手打的假
    /// marker）保持字面文本。顺序差异只在“某个粘贴内容里恰好含有另一个 marker 字面量”
    /// 时可见（见模块文档第 7 条）。
    pub fn get_expanded_text(&self) -> String {
        self.expand_paste_markers(&self.get_text())
    }

    /// 行副本（`getLines()`）；修改返回值不影响编辑器。
    pub fn get_lines(&self) -> Vec<String> {
        self.state.lines.clone()
    }

    /// 当前光标位置（`getCursor()`）；`col` 是 UTF-16 偏移。
    pub fn get_cursor(&self) -> (usize, usize) {
        (self.state.cursor_line, self.state.cursor_col)
    }

    /// 当前滚动偏移（测试/宿主用）。
    pub fn scroll_offset(&self) -> usize {
        self.scroll_offset
    }

    /// 最近一次 render 的可见行数（测试/宿主用）。
    pub fn rendered_visible_line_count(&self) -> usize {
        self.rendered_visible_line_count
    }

    /// 最近一次 render 的自动补全高度（测试/宿主用）。
    pub fn rendered_autocomplete_height(&self) -> usize {
        self.rendered_autocomplete_height
    }

    /// 当前布局宽度（`lastWidth`）。
    ///
    /// 这是视觉行使用的宽度，不是 `render(width)` 的入参：有 padding 时等于内容宽度，
    /// 无 padding 时还会再减 1 列给行尾光标（见 [`Editor::render_editor`]）。鼠标命中与
    /// 上下移动都按它重建视觉行，所以首次 render 之前是构造时的占位值 80。
    pub fn last_width(&self) -> usize {
        self.last_width
    }

    // ------------------------------------------------------------------
    // 历史
    // ------------------------------------------------------------------

    /// `addToHistory(text)`。
    ///
    /// 按 JS 的空白集合 trim；空串忽略；与**最近一条**相同则忽略（更早的重复仍可加入，
    /// 见 `tests/editor.rs` 的 history 用例）。历史从头插入，最多保留 100 条，超出时丢
    /// 最旧的一条。最近一条在 `history[0]`，所以上下浏览的索引 0 是最近输入。
    pub fn add_to_history(&mut self, text: &str) {
        let trimmed = trim_js(text);
        if trimmed.is_empty() {
            return;
        }
        if self.history.first().is_some_and(|first| first == trimmed) {
            return;
        }
        self.history.insert(0, trimmed.to_string());
        if self.history.len() > 100 {
            self.history.pop();
        }
    }

    /// `isEditorEmpty()`。
    fn is_editor_empty(&self) -> bool {
        self.state.lines.len() == 1 && self.state.lines[0].is_empty()
    }

    fn is_on_first_visual_line(&self) -> bool {
        let visual_lines = self.build_visual_line_map(self.last_width);
        self.find_current_visual_line(&visual_lines) == 0
    }

    fn is_on_last_visual_line(&self) -> bool {
        let visual_lines = self.build_visual_line_map(self.last_width);
        self.find_current_visual_line(&visual_lines) + 1 == visual_lines.len()
    }

    /// `navigateHistory(direction)`：Up(-1) 使索引增，Down(1) 使索引减。
    ///
    /// `history_index = -1` 表示“未浏览”，0 是最近一条，越往上索引越大。第一次从 -1 进入
    /// 历史时把当前编辑内容存进 `history_draft` 并 push 快照；回到 -1 时恢复草稿（没有草稿
    /// 则清空文本）。越过两端（`< -1` 或 `>= len`）是 no-op。Up 时光标放行首、Down 时放
    /// 行尾。
    fn navigate_history(&mut self, direction: isize) {
        self.last_action = None;
        if self.history.is_empty() {
            return;
        }

        let new_index = self.history_index - direction;
        if new_index < -1 || new_index >= self.history.len() as isize {
            return;
        }

        if self.history_index == -1 && new_index >= 0 {
            self.push_undo_snapshot();
            self.history_draft = Some(self.state.clone());
        }

        self.history_index = new_index;

        if self.history_index == -1 {
            let draft = self.history_draft.take();
            if let Some(draft) = draft {
                self.state = draft;
                self.preferred_visual_col = None;
                self.snapped_from_cursor_col = None;
                self.scroll_offset = 0;
                self.emit_change();
            } else {
                self.set_text_internal("", CursorPlacement::End);
            }
        } else {
            let text = self
                .history
                .get(self.history_index as usize)
                .cloned()
                .unwrap_or_default();
            let placement = if direction == -1 {
                CursorPlacement::Start
            } else {
                CursorPlacement::End
            };
            self.set_text_internal(&text, placement);
        }
    }

    /// `exitHistoryBrowsing()`。
    ///
    /// 只把 `history_index` 置 -1、丢弃草稿；不修改当前文本（与“按 Esc 退出历史但保留
    /// 当前条目”一致）。
    fn exit_history_browsing(&mut self) {
        self.history_index = -1;
        self.history_draft = None;
    }

    /// `setTextInternal(text, cursorPlacement)`：不重置历史状态。
    ///
    /// 按 `\n` 拆行（空文本仍会得到一行空串，维持 `lines` 非空不变量），光标按
    /// `Start`/`End` 放置，滚动归零，最后触发 `onChange`。调用方负责按场景先取消补全/
    /// 退出历史/push 快照：`set_text` 自身完成取消、退出并在文本变化时压栈；
    /// `navigate_history` 不取消补全、也不退出历史，只在首次从 `-1` 进入时压快照，
    /// 补全则由它的调用方按场景处理。
    fn set_text_internal(&mut self, text: &str, placement: CursorPlacement) {
        let lines: Vec<String> = text.split('\n').map(str::to_string).collect();
        self.state.lines = if lines.is_empty() {
            vec![String::new()]
        } else {
            lines
        };
        self.state.cursor_line = match placement {
            CursorPlacement::Start => 0,
            CursorPlacement::End => self.state.lines.len() - 1,
        };
        let col = match placement {
            CursorPlacement::Start => 0,
            CursorPlacement::End => self
                .state
                .lines
                .get(self.state.cursor_line)
                .map_or(0, |line| utf16_len(line)),
        };
        self.set_cursor_col(col);
        self.scroll_offset = 0;
        self.emit_change();
    }

    // ------------------------------------------------------------------
    // 文本设置 / 变更回调
    // ------------------------------------------------------------------

    fn emit_change(&mut self) {
        if self.on_change.is_some() {
            let text = self.get_text();
            if let Some(callback) = &mut self.on_change {
                callback(&text);
            }
        }
    }

    /// `setText(text)`。
    ///
    /// 取消补全、退出历史浏览；按 `normalize_text` 归一化换行与 tab；清空 paste registry
    /// 与计数器（旧 marker 中的内容不再能展开）；光标移到末尾。只有归一化后的文本与当前
    /// 文本不同才 push 撤销快照，但 `onChange` 总会触发。
    pub fn set_text(&mut self, text: &str) {
        self.cancel_autocomplete();
        self.last_action = None;
        self.exit_history_browsing();
        let normalized = normalize_text(text);
        if self.get_text() != normalized {
            self.push_undo_snapshot();
        }
        self.pastes.clear();
        self.paste_counter = 0;
        self.set_text_internal(&normalized, CursorPlacement::End);
    }

    /// `insertTextAtCursor(text)`：整体原子撤销。
    ///
    /// 先取消补全并 push 一次快照，再把文本（换行/tab 归一化后）插入光标处、光标落在插入
    /// 内容之后；`onChange` 触发。与逐字符输入不同，这里**不会**触发自动补全。
    pub fn insert_text_at_cursor(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        self.cancel_autocomplete();
        self.push_undo_snapshot();
        self.last_action = None;
        self.exit_history_browsing();
        self.insert_text_at_cursor_internal(text);
    }

    /// `insertTextAtCursorInternal(text)`：调用方负责撤销快照；不触发自动补全。
    ///
    /// 文本先按 `normalize_text` 归一化。单行时直接拼在光标前后；多行时把第一行拼到光标
    /// 前、最后一行拼到光标后、中间行整行插入，光标落在最后一行内容之后。空文本是 no-op。
    fn insert_text_at_cursor_internal(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }

        let normalized = normalize_text(text);
        let inserted_lines: Vec<String> = normalized.split('\n').map(str::to_string).collect();

        let current_line = self
            .state
            .lines
            .get(self.state.cursor_line)
            .cloned()
            .unwrap_or_default();
        let before_cursor = utf16_prefix(&current_line, self.state.cursor_col);
        let after_cursor = utf16_suffix(&current_line, self.state.cursor_col);

        if inserted_lines.len() == 1 {
            self.state.lines[self.state.cursor_line] =
                format!("{before_cursor}{normalized}{after_cursor}");
            let col = self.state.cursor_col + utf16_len(&normalized);
            self.set_cursor_col(col);
        } else {
            let first = inserted_lines.first().cloned().unwrap_or_default();
            let last = inserted_lines.last().cloned().unwrap_or_default();
            let mut next_lines: Vec<String> =
                Vec::with_capacity(self.state.lines.len() + inserted_lines.len().saturating_sub(1));
            next_lines.extend(self.state.lines[..self.state.cursor_line].iter().cloned());
            next_lines.push(format!("{before_cursor}{first}"));
            next_lines.extend(inserted_lines[1..inserted_lines.len() - 1].iter().cloned());
            next_lines.push(format!("{last}{after_cursor}"));
            next_lines.extend(
                self.state.lines[self.state.cursor_line + 1..]
                    .iter()
                    .cloned(),
            );
            self.state.lines = next_lines;
            self.state.cursor_line += inserted_lines.len() - 1;
            let col = utf16_len(&last);
            self.set_cursor_col(col);
        }

        self.emit_change();
    }

    // ------------------------------------------------------------------
    // render
    // ------------------------------------------------------------------

    /// TS `super.renderTopBorder(width, hiddenLineCount)`：默认实现（**不**经过覆写回调，
    /// 供自定义渲染器组合调用，对应 TS 覆写里的 `super.renderTopBorder(...)`）。
    ///
    /// `hidden_line_count == 0` 返回整行 `─`，否则返回 `create_scroll_border` 的
    /// `↑` 形态；最后整体经过 `border_color` 着色。
    pub fn base_top_border(&self, width: usize, hidden_line_count: usize) -> String {
        let border = if hidden_line_count > 0 {
            create_scroll_border("↑", hidden_line_count, width)
        } else {
            "─".repeat(width)
        };
        (self.border_color)(&border)
    }

    /// TS `super.renderBottomBorder(width, hiddenLineCount)`。
    ///
    /// 与 [`Editor::base_top_border`] 相同，只是滚动指示方向为 `↓`。
    pub fn base_bottom_border(&self, width: usize, hidden_line_count: usize) -> String {
        let border = if hidden_line_count > 0 {
            create_scroll_border("↓", hidden_line_count, width)
        } else {
            "─".repeat(width)
        };
        (self.border_color)(&border)
    }

    /// `protected renderTopBorder(width, hiddenLineCount)`。
    ///
    /// 安装了自定义渲染器时，把默认实现（已着色）作为 `base` 传入并返回其输出；否则直接
    /// 返回默认结果。对应 TS 子类覆写后调用 `super.renderTopBorder(...)` 的组合方式。
    fn render_top_border(&self, width: usize, hidden_line_count: usize) -> String {
        let base = self.base_top_border(width, hidden_line_count);
        match &self.top_border_renderer {
            Some(renderer) => renderer(width, hidden_line_count, self.border_color.as_ref(), &base),
            None => base,
        }
    }

    /// `protected renderBottomBorder(width, hiddenLineCount)`。
    ///
    /// 与 [`Editor::render_top_border`] 相同，只是使用底部渲染器与默认下边框。
    fn render_bottom_border(&self, width: usize, hidden_line_count: usize) -> String {
        let base = self.base_bottom_border(width, hidden_line_count);
        match &self.bottom_border_renderer {
            Some(renderer) => renderer(width, hidden_line_count, self.border_color.as_ref(), &base),
            None => base,
        }
    }

    /// `layoutText(contentWidth)`：逻辑行 → 视觉行。
    ///
    /// - 空文档（一行且为空）产出一条带光标的空行；否则每个逻辑行至少一条。
    /// - 行宽不超过 `content_width` 时原样一条；超宽时用 [`word_wrap_line`] 切分（分段带
    ///   paste marker 原子信息）。
    /// - 光标归属：当前行的**最后一块**接受 `cursor_col == end_index` 的行尾位置，其余块用
    ///   半开区间 `[start, end)`；`cursor_pos` 换算成块内相对偏移。
    ///
    /// `LayoutLine.text` 是显示文本（不含边框/padding），`cursor_pos` 是 UTF-16 相对偏移；
    /// [`Editor::render_editor`] 再按它切出伪光标。
    fn layout_text(&self, content_width: usize) -> Vec<LayoutLine> {
        let mut layout_lines: Vec<LayoutLine> = Vec::new();

        if self.state.lines.is_empty()
            || (self.state.lines.len() == 1 && self.state.lines[0].is_empty())
        {
            layout_lines.push(LayoutLine {
                text: String::new(),
                has_cursor: true,
                cursor_pos: Some(0),
            });
            return layout_lines;
        }

        for (i, line) in self.state.lines.iter().enumerate() {
            let line = line.as_str();
            let is_current_line = i == self.state.cursor_line;
            let line_visible_width = visible_width(line);

            if line_visible_width <= content_width {
                if is_current_line {
                    layout_lines.push(LayoutLine {
                        text: line.to_string(),
                        has_cursor: true,
                        cursor_pos: Some(self.state.cursor_col),
                    });
                } else {
                    layout_lines.push(LayoutLine {
                        text: line.to_string(),
                        has_cursor: false,
                        cursor_pos: None,
                    });
                }
            } else {
                let segments = self.segment_graphemes(line);
                let chunks = word_wrap_line(line, content_width, Some(&segments));

                for (chunk_index, chunk) in chunks.iter().enumerate() {
                    let cursor_pos = self.state.cursor_col;
                    let is_last_chunk = chunk_index == chunks.len() - 1;

                    let mut has_cursor_in_chunk = false;
                    let mut adjusted_cursor_pos = 0usize;

                    if is_current_line {
                        if is_last_chunk {
                            // 最后一块必须接受“行尾”位置（cursor_col == end_index），
                            // 否则光标停在行尾时没有块认领它。
                            has_cursor_in_chunk = cursor_pos >= chunk.start_index;
                            adjusted_cursor_pos = cursor_pos.saturating_sub(chunk.start_index);
                        } else {
                            has_cursor_in_chunk =
                                cursor_pos >= chunk.start_index && cursor_pos < chunk.end_index;
                            if has_cursor_in_chunk {
                                adjusted_cursor_pos = cursor_pos - chunk.start_index;
                                // 防御性夹取：相对偏移不能超过块文本长度。
                                let text_len = utf16_len(&chunk.text);
                                if adjusted_cursor_pos > text_len {
                                    adjusted_cursor_pos = text_len;
                                }
                            }
                        }
                    }

                    if has_cursor_in_chunk {
                        layout_lines.push(LayoutLine {
                            text: chunk.text.clone(),
                            has_cursor: true,
                            cursor_pos: Some(adjusted_cursor_pos),
                        });
                    } else {
                        layout_lines.push(LayoutLine {
                            text: chunk.text.clone(),
                            has_cursor: false,
                            cursor_pos: None,
                        });
                    }
                }
            }
        }

        layout_lines
    }

    /// 可见行数上限：终端高度的 30%，至少 5 行。翻页大小用同一公式（`page_scroll`）。
    fn max_visible_lines(&self) -> usize {
        let terminal_rows = self.tui.rows();
        ((terminal_rows as f64 * 0.3).floor() as usize).max(5)
    }

    /// `render(width)`：渲染成完整行列表（顶/底边框 + 可见文本 + 补全列表）。
    ///
    /// 副作用（因此需要 `&mut self`）：pump 补全（可能启动 provider 请求）、更新
    /// `last_width` / `scroll_offset` / `rendered_visible_line_count` /
    /// `rendered_autocomplete_height`，并在末尾应用鼠标在补全列表里选中的候选
    /// （`drain_autocomplete_selection`）。
    ///
    /// 宽度分配：`padding_x` 先限制到 `(width - 1) / 2`；无 padding 时再给行尾光标预留
    /// 1 列（`layout_width`）。文本行的组成是“左 padding + 内容 + 空格填充到内容宽度 +
    /// 右 padding”，边框行直接使用 `width`；本方法不返回后再裁剪。
    ///
    /// 光标：仅 `focused == true` 时输出 [`CURSOR_MARKER`]。光标落在字素上时用
    /// `render_fake_cursor` 包住该字素（宽度不变）；落在行尾时包住一个空格（宽度 +1，
    /// 超出内容宽度就占用右侧 padding 的一列）。
    ///
    /// # Panics
    ///
    /// 内容宽度为 1 且文本含宽字符时，[`word_wrap_line`] 会 panic（模块文档第 6 条）。
    pub fn render_editor(&mut self, width: usize) -> Vec<String> {
        self.pump_autocomplete();

        let max_padding = width.saturating_sub(1) / 2;
        let padding_x = self.padding_x.min(max_padding);
        let content_width = width.saturating_sub(padding_x * 2).max(1);

        // 有 padding 时光标可以溢出到 padding；没有 padding 时给光标预留 1 列。
        let layout_width = if padding_x == 0 {
            content_width.saturating_sub(1).max(1)
        } else {
            content_width.max(1)
        };

        self.last_width = layout_width;

        // 逻辑行 → 视觉行；cursor_line_index 是第一条带光标的视觉行。
        let layout_lines = self.layout_text(layout_width);
        let max_visible_lines = self.max_visible_lines();

        let cursor_line_index = layout_lines
            .iter()
            .position(|line| line.has_cursor)
            .unwrap_or(0);

        // 让光标行落在可视窗口内：向上越界就贴顶，向下越界就把底边对齐光标行。
        if cursor_line_index < self.scroll_offset {
            self.scroll_offset = cursor_line_index;
        } else if cursor_line_index >= self.scroll_offset + max_visible_lines {
            self.scroll_offset = cursor_line_index + 1 - max_visible_lines;
        }

        // 文档太短时不允许滚动到大量空白处。
        let max_scroll_offset = layout_lines.len().saturating_sub(max_visible_lines);
        self.scroll_offset = self.scroll_offset.min(max_scroll_offset);

        let visible_end = (self.scroll_offset + max_visible_lines).min(layout_lines.len());
        let visible_lines = &layout_lines[self.scroll_offset..visible_end];
        self.rendered_visible_line_count = visible_lines.len();

        let mut result: Vec<String> = Vec::new();
        let left_padding = " ".repeat(padding_x);
        let right_padding = left_padding.clone();

        result.push(self.render_top_border(width, self.scroll_offset));

        // 焦点决定是否把 CURSOR_MARKER 写进输出；TUI 用它定位硬件光标。
        let emit_cursor_marker = self.focused;

        for layout_line in visible_lines {
            let mut display_text = layout_line.text.clone();
            let mut line_visible_width = visible_width(&layout_line.text);
            let mut cursor_in_padding = false;

            if layout_line.has_cursor
                && let Some(cursor_pos) = layout_line.cursor_pos
            {
                let before = utf16_prefix(&display_text, cursor_pos);
                let after = utf16_suffix(&display_text, cursor_pos);

                let marker = if emit_cursor_marker {
                    CURSOR_MARKER
                } else {
                    ""
                };

                if !after.is_empty() {
                    // 光标落在某个字素上：用高亮版本替换它（不新增字符，宽度不变）。
                    let after_graphemes = self.segment_graphemes(&after);
                    let first_grapheme = after_graphemes
                        .first()
                        .map(|segment| segment.segment.clone())
                        .unwrap_or_default();
                    let rest_after = utf16_suffix(&after, utf16_len(&first_grapheme));
                    let cursor = render_fake_cursor(&first_grapheme);
                    display_text = format!("{before}{marker}{cursor}{rest_after}");
                    // 可见宽度不变：替换而非新增。
                } else {
                    // 光标在行尾：追加一个高亮空格。
                    let cursor = render_fake_cursor(" ");
                    display_text = format!("{before}{marker}{cursor}");
                    line_visible_width += 1;
                    if line_visible_width > content_width && padding_x > 0 {
                        cursor_in_padding = true;
                    }
                }
            }

            let padding = " ".repeat(content_width.saturating_sub(line_visible_width));
            let line_right_padding = if cursor_in_padding {
                utf16_suffix(&right_padding, 1)
            } else {
                right_padding.clone()
            };

            result.push(format!(
                "{left_padding}{display_text}{padding}{line_right_padding}"
            ));
        }

        let lines_below = layout_lines.len() - (self.scroll_offset + visible_lines.len());
        result.push(self.render_bottom_border(width, lines_below));

        self.rendered_autocomplete_height = 0;
        if self.autocomplete_state.is_some()
            && let Some(list) = &mut self.autocomplete_list
        {
            let autocomplete_result = list.render(content_width);
            self.rendered_autocomplete_height = autocomplete_result.len();
            for line in &autocomplete_result {
                let line_width = visible_width(line);
                let line_padding = " ".repeat(content_width.saturating_sub(line_width));
                result.push(format!("{left_padding}{line}{line_padding}{right_padding}"));
            }
        }

        // 鼠标点击列表产生的选中项在这里应用（见 `autocomplete_selected`）。
        self.drain_autocomplete_selection();

        result
    }

    /// `handleMouse(event)`：处理补全列表与文本区的左键单击。
    ///
    /// 坐标是相对本组件的 0 基坐标（见 [`TuiMouseEvent`] 的 `x`/`y`）。默认边框渲染器下
    /// 布局为：顶边框占 `y = 0`，文本占 `1..=rendered_visible_line_count`，底边框紧随其
    /// 后，补全列表从 `rendered_visible_line_count + 2` 开始（自定义边框回调若返回多行文本
    /// 会破坏这个假设）。
    ///
    /// 返回 `None` 表示不处理（press/drag/release、非左键，以及列表自身不处理的列表区
    /// 事件，均留给渲染器/上层）；列表区之外的 click 会被吞掉并请求焦点，即使点在边框或
    /// 空白处。列表区返回结果时会强制 `focus = true`（点列表也要让编辑器保持焦点）。
    pub fn handle_mouse_event(&mut self, event: &TuiMouseEvent) -> Option<TuiMouseEventResult> {
        // 列表区优先：先把坐标换算成 SelectList 的局部坐标（x 去掉 padding，y 去掉
        // 边框与文本区）。
        let autocomplete_start_row = self.rendered_visible_line_count + 2;
        if self.autocomplete_state.is_some()
            && self.rendered_autocomplete_height > 0
            && let Some(list) = &mut self.autocomplete_list
            && event.y >= autocomplete_start_row as isize
            && event.y < (autocomplete_start_row + self.rendered_autocomplete_height) as isize
        {
            let max_padding = event.width.saturating_sub(1) / 2;
            let padding_x = self.padding_x.min(max_padding);
            let content_width = event.width.saturating_sub(padding_x * 2).max(1);
            let mut list_event = event.clone();
            list_event.x = event.x - padding_x as isize;
            list_event.y = event.y - autocomplete_start_row as isize;
            list_event.width = content_width;
            list_event.height = self.rendered_autocomplete_height;
            let result = list.handle_mouse(&list_event);
            self.drain_autocomplete_selection();
            return result.map(|mut result| {
                result.focus = Some(true);
                result
            });
        }

        // press/drag/release 留给渲染器的文本选择；只有合成 click 用于定位光标。
        if event.event_type != TuiMouseEventType::Click || event.button != TuiMouseButton::Left {
            return None;
        }
        if event.y <= 0 || event.y > self.rendered_visible_line_count as isize {
            // 边框/列表区之外的点击：吞掉并请求焦点，但不移动光标。
            return Some(TuiMouseEventResult {
                handled: Some(true),
                focus: Some(true),
                ..Default::default()
            });
        }

        let visual_lines = self.build_visual_line_map(self.last_width);
        let visual_line_index = self.scroll_offset as isize + event.y - 1;
        if visual_line_index < 0 {
            return Some(TuiMouseEventResult {
                handled: Some(true),
                focus: Some(true),
                ..Default::default()
            });
        }
        let Some(visual_line) = visual_lines.get(visual_line_index as usize).copied() else {
            return Some(TuiMouseEventResult {
                handled: Some(true),
                focus: Some(true),
                ..Default::default()
            });
        };
        let logical_line = self
            .state
            .lines
            .get(visual_line.logical_line)
            .cloned()
            .unwrap_or_default();
        let chunk_end = visual_line.start_col + visual_line.length;
        let chunk = utf16_slice(&logical_line, visual_line.start_col, chunk_end);
        let max_padding = event.width.saturating_sub(1) / 2;
        let padding_x = self.padding_x.min(max_padding);
        let target_column = (event.x - padding_x as isize).max(0) as usize;
        let mut visible_column = 0usize;
        let mut target_index = utf16_len(&chunk);
        let mut last_grapheme_index = 0usize;
        for grapheme in self.segment_graphemes(&chunk) {
            let next_column = visible_column + visible_width(&grapheme.segment);
            last_grapheme_index = grapheme.index;
            if target_column < next_column {
                target_index = grapheme.index;
                break;
            }
            visible_column = next_column;
        }
        let is_last_segment = visual_line_index as usize + 1 == visual_lines.len()
            || visual_lines
                .get(visual_line_index as usize + 1)
                .is_none_or(|next| next.logical_line != visual_line.logical_line);
        // 在非最后视觉段上，点在行尾（target_index == len）要退到最后一个字素的起点，
        // 否则光标会跑到下一视觉行的行首。
        if !is_last_segment && target_index == utf16_len(&chunk) && !chunk.is_empty() {
            target_index = last_grapheme_index;
        }

        self.state.cursor_line = visual_line.logical_line;
        self.set_cursor_col(visual_line.start_col + target_index);
        self.last_action = None;
        self.exit_history_browsing();
        if self.autocomplete_state.is_some() {
            self.update_autocomplete();
        }
        Some(TuiMouseEventResult {
            handled: Some(true),
            focus: Some(true),
            ..Default::default()
        })
    }

    /// 渲染期内应用列表点击选中的条目（TS `onSelect`）。
    ///
    /// `SelectList` 的 `on_select` 回调拿不到 `&mut Editor`，只能把条目写进共享槽位；
    /// 本方法在 [`Editor::render_editor`] 末尾与鼠标处理之后取走槽位并应用。没有待处理项
    /// 时是 no-op。
    fn drain_autocomplete_selection(&mut self) {
        let selected = self.autocomplete_selected.borrow_mut().take();
        let Some(selected) = selected else {
            return;
        };
        self.push_undo_snapshot();
        self.last_action = None;
        let prefix = self.autocomplete_prefix.clone();
        if self.apply_completion_select_item(&selected, &prefix) {
            self.cancel_autocomplete();
            self.emit_change();
        }
    }
}

/// `setTextInternal` 的光标放置方式。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CursorPlacement {
    Start,
    End,
}

/// `normalizeText(text)`：CRLF/CR → LF，tab → 4 空格。
///
/// 必须先替换 `\r\n` 再替换孤立的 `\r`，否则 `\r\n` 会变成两个换行。
fn normalize_text(text: &str) -> String {
    text.replace("\r\n", "\n")
        .replace('\r', "\n")
        .replace('\t', "    ")
}

/// `trimStart()`（JS 空白集合）。
fn trim_start_js(text: &str) -> &str {
    text.trim_start_matches(is_js_whitespace)
}

/// `/[a-zA-Z0-9.\-_]/.test(text)`（未锚定：任一字符命中）。
fn contains_word_character(text: &str) -> bool {
    text.chars()
        .any(|ch| ch.is_ascii_alphanumeric() || ch == '.' || ch == '_' || ch == '-')
}

/// `/\w/.test(text)`。
fn is_word_character_js(text: &str) -> bool {
    text.chars()
        .any(|ch| ch.is_ascii_alphanumeric() || ch == '_')
}

/// `autocompleteTriggerPattern.test(text)` / `autocompleteDebouncePattern.test(text)` 的
/// 等价判定（对应 TS 的 `buildTriggerPattern` / `buildDebouncePattern`，见
/// `spec/tui-components.md` §2.15.2）。
///
/// `debounce` 为 true 时用 debounce 正则的形态（`@` 单独成支，其它触发字符的字符类排除
/// `@`）；否则用 trigger 正则。
///
/// 语义：token 起点是行首或分隔符（JS 空白或 CJK 标点，见
/// [`is_autocomplete_separator`]）之后，允许紧跟 0..n 个 `(`、`[`、`{`、`<` 或反引号；
/// 随后是 `@"…"`（要求到文本末尾都没有第二个 `"`，即“尚未闭合的引号路径”）
/// **或**一个触发字符 + 0..n 个非分隔符字符。
/// 这里手写判定而不用 `regex`：分隔符集合在 [`crate::text`] 里已有单字符判定
/// （`is_autocomplete_separator`），直接复用可避免两套正则方言漂移；正确性由本文件的
/// `TRIGGER_PATTERN_GOLDEN` 金表与 `tests/fixtures/editor_trigger_patterns.json` 的
/// TS 差分测试锁定。
fn autocomplete_pattern_matches(text: &str, trigger_chars: &[char], debounce: bool) -> bool {
    let chars: Vec<char> = text.chars().collect();

    // token 只能从行首或分隔符之后开始；starts 是全部候选起点（升序）。
    let mut starts: Vec<usize> = vec![0];
    for (index, ch) in chars.iter().enumerate() {
        if is_autocomplete_separator(*ch) {
            starts.push(index + 1);
        }
    }

    // `clean_after[i]` = `chars[i..]` 里没有分隔符。`suffix` 从触发字符的**下一个**字符
    // 开始匹配，所以判定用的是 `clean_after[k + 1]`（触发字符本身可能是分隔符——例如
    // 自定义触发字符是 CJK 标点 `，`，此时它正好落在 `run_end` 上，不能把它自己算进后缀）。
    let mut clean_after = vec![true; chars.len() + 1];
    for i in (0..chars.len()).rev() {
        clean_after[i] = clean_after[i + 1] && !is_autocomplete_separator(chars[i]);
    }

    for start in starts {
        // `[([{<`]*` 是**贪婪**量词：JS 正则引擎在后续分支匹配失败时会逐个交出括号字符
        // （回溯）。触发字符本身可能正是一个括号/反引号（例如自定义触发字符 `(`），
        // 此时必须允许回溯，否则 `"("`、`"x ("` 这类文本会被误判为不匹配。
        let mut run_end = start;
        while run_end < chars.len() && matches!(chars[run_end], '(' | '[' | '{' | '<' | '`') {
            run_end += 1;
        }

        for k in (start..=run_end).rev() {
            if k >= chars.len() {
                continue;
            }
            // 从最右端（离文本末尾最近）向左尝试，对应 JS 正则里贪婪 `[([{<`]*` 在
            // 后续分支失败时的回溯顺序。

            // `@"[^"]*`（引号包裹路径；内部允许空白与 CJK 标点）。
            if chars[k] == '@' && chars.get(k + 1) == Some(&'"') && !chars[k + 2..].contains(&'"') {
                return true;
            }

            if !clean_after[k + 1] {
                continue;
            }

            if debounce && chars[k] == '@' {
                // debounce 的 `@` 分支已在上面的引号形式里处理；这里只剩裸路径形式。
                return true;
            }

            // 非 debounce 时触发字符类里可以含 `@`；debounce 时 `@` 由上面的显式分支
            // 覆盖，不再进入“其它触发字符”类。
            if trigger_chars.contains(&chars[k]) && !(debounce && chars[k] == '@') {
                return true;
            }
        }
    }

    false
}

impl Editor {
    // ------------------------------------------------------------------
    // handleInput
    // ------------------------------------------------------------------

    /// `handleInput(data)` 全量分派。
    ///
    /// **不**在这里 pump 补全：TS 的请求总要经过微任务，同一同步批次（一次 stdin 读取里的
    /// 多个按键）只有最后一个请求会调用 provider。宿主应在处理完一批输入后用 `render`/
    /// [`Editor::tick`] 推进。
    ///
    /// 分派顺序见函数内 A–R 标记：字符跳转模式与括号粘贴会改变后续数据的解释方式，因此
    /// 排在最前；**多数分支命中后直接 return**，例外见 A（`jump_mode` 遇到控制字符时清除
    /// 后继续向下分派）与 E（斜杠命令确认会落到提交分支；无选中项或无 provider 时也会
    /// 穿出 E 分支）。方向键仅在首/末视觉行时**可能**转入历史（另需满足空编辑器/正在
    /// 浏览/光标在行首等条件）。本方法不调用 `requestRender`，宿主
    /// 在批次结束后渲染一次即可（渲染开头会 pump 补全）。
    pub fn handle_input_data(&mut self, data: &str) {
        let kb = get_keybindings();

        // A. 字符跳转模式（等待目标字符）。
        if let Some(direction) = self.jump_mode {
            if kb.matches(data, "tui.editor.jumpForward")
                || kb.matches(data, "tui.editor.jumpBackward")
            {
                self.jump_mode = None;
                return;
            }

            let printable = decode_printable_key(data).or_else(|| {
                if utf16_unit_at(data, 0).is_some_and(|unit| unit >= 32) {
                    Some(data.to_string())
                } else {
                    None
                }
            });
            if let Some(printable) = printable {
                self.jump_mode = None;
                self.jump_to_char(&printable, direction);
                return;
            }

            self.jump_mode = None;
        }

        // B. 括号粘贴。开始标记、内容和结束标记可能在同一次读取里，也可能分多次到达：
        // 用 paste_buffer 累积，见到结束标记才把内容交给 handle_paste；结束标记之后若还有
        // 普通按键，递归按普通输入处理。
        let mut data = data.to_string();
        if data.contains("\x1b[200~") {
            self.is_in_paste = true;
            self.paste_buffer.clear();
            data = data.replacen("\x1b[200~", "", 1);
        }

        if self.is_in_paste {
            self.paste_buffer.push_str(&data);
            if let Some(end_index) = self.paste_buffer.find("\x1b[201~") {
                let paste_content = self.paste_buffer[..end_index].to_string();
                if !paste_content.is_empty() {
                    self.handle_paste(&paste_content);
                }
                self.is_in_paste = false;
                let remaining = self.paste_buffer[end_index + 6..].to_string();
                self.paste_buffer.clear();
                if !remaining.is_empty() {
                    self.handle_input_data(&remaining);
                }
                return;
            }
            return;
        }

        // C. Ctrl+C 交给父级（退出/清空）。
        if kb.matches(&data, "tui.input.copy") {
            return;
        }

        // D. 撤销。
        if kb.matches(&data, "tui.editor.undo") {
            self.undo();
            return;
        }

        // E. 自动补全可见时的按键。
        if self.autocomplete_state.is_some() && self.autocomplete_list.is_some() {
            if kb.matches(&data, "tui.select.cancel") {
                self.cancel_autocomplete();
                return;
            }

            if kb.matches(&data, "tui.select.up") || kb.matches(&data, "tui.select.down") {
                if let Some(list) = &mut self.autocomplete_list {
                    list.handle_input(&data);
                }
                return;
            }

            if kb.matches(&data, "tui.input.tab") {
                let selected = self
                    .autocomplete_list
                    .as_ref()
                    .and_then(|list| list.get_selected_item().cloned());
                // TS 的 provider 在请求在飞期间仍然可用；`provider_available()` 覆盖该窗口，
                // `apply_completion` 会先丢弃在飞请求取回 provider。
                if let Some(selected) = selected
                    && self.provider_available()
                {
                    self.push_undo_snapshot();
                    self.last_action = None;
                    let prefix = self.autocomplete_prefix.clone();
                    if self.apply_completion_select_item(&selected, &prefix) {
                        self.cancel_autocomplete();
                        self.emit_change();
                    }
                }
                return;
            }

            if kb.matches(&data, "tui.select.confirm") {
                let selected = self
                    .autocomplete_list
                    .as_ref()
                    .and_then(|list| list.get_selected_item().cloned());
                if let Some(selected) = selected
                    && self.provider_available()
                {
                    self.push_undo_snapshot();
                    self.last_action = None;
                    let prefix = self.autocomplete_prefix.clone();
                    let applied = self.apply_completion_select_item(&selected, &prefix);

                    if prefix.starts_with('/') {
                        self.cancel_autocomplete();
                        // 继续落到提交分支。
                    } else {
                        self.cancel_autocomplete();
                        if applied {
                            self.emit_change();
                        }
                        return;
                    }
                }
            }
        }

        // F. Tab 触发补全。
        if kb.matches(&data, "tui.input.tab") && self.autocomplete_state.is_none() {
            self.handle_tab_completion();
            return;
        }

        // G. 删除。
        if kb.matches(&data, "tui.editor.deleteToLineEnd") {
            self.delete_to_end_of_line();
            return;
        }
        if kb.matches(&data, "tui.editor.deleteToLineStart") {
            self.delete_to_start_of_line();
            return;
        }
        if kb.matches(&data, "tui.editor.deleteWordBackward") {
            self.delete_word_backwards();
            return;
        }
        if kb.matches(&data, "tui.editor.deleteWordForward") {
            self.delete_word_forward();
            return;
        }
        if kb.matches(&data, "tui.editor.deleteCharBackward")
            || matches_key(&data, "shift+backspace")
        {
            self.handle_backspace();
            return;
        }
        if kb.matches(&data, "tui.editor.deleteCharForward") || matches_key(&data, "shift+delete") {
            self.handle_forward_delete();
            return;
        }

        // H. kill ring。
        if kb.matches(&data, "tui.editor.yank") {
            self.yank();
            return;
        }
        if kb.matches(&data, "tui.editor.yankPop") {
            self.yank_pop();
            return;
        }

        // I. 专用历史键始终浏览历史，而不是先移动光标。
        if kb.matches(&data, "tui.editor.historyPrevious") {
            self.cancel_autocomplete();
            self.navigate_history(-1);
            return;
        }
        if kb.matches(&data, "tui.editor.historyNext") {
            self.cancel_autocomplete();
            self.navigate_history(1);
            return;
        }

        // J. 光标移动。
        if kb.matches(&data, "tui.editor.cursorLineStart") {
            self.move_to_line_start();
            return;
        }
        if kb.matches(&data, "tui.editor.cursorLineEnd") {
            self.move_to_line_end();
            return;
        }
        if kb.matches(&data, "tui.editor.cursorWordLeft") {
            self.move_word_backwards();
            return;
        }
        if kb.matches(&data, "tui.editor.cursorWordRight") {
            self.move_word_forwards();
            return;
        }

        // K. 换行。终端对 Shift+Enter 的编码不统一（CSI-u 的 `\x1b[13;2~`、ESC+CR、
        // 以及带修饰的多字节序列），这里统一归为插入换行；单独的 Enter 走下面的提交分支。
        if kb.matches(&data, "tui.input.newLine")
            || (utf16_unit_at(&data, 0) == Some(10) && utf16_len(&data) > 1)
            || data == "\x1b\r"
            || data == "\x1b[13;2~"
            || (utf16_len(&data) > 1 && data.contains('\x1b') && data.contains('\r'))
            || (data == "\n")
        {
            if self.should_submit_on_backslash_enter(&data, kb) {
                self.handle_backspace();
                self.submit_value();
                return;
            }
            self.add_new_line();
            return;
        }

        // L. 提交（Enter）。行尾反斜杠是续行：删掉反斜杠并插入换行，不提交（TS 行为）；
        // `disable_submit` 时 Enter 完全被忽略。
        if kb.matches(&data, "tui.input.submit") {
            if self.disable_submit {
                return;
            }

            let current_line = self
                .state
                .lines
                .get(self.state.cursor_line)
                .cloned()
                .unwrap_or_default();
            if self.state.cursor_col > 0
                && utf16_unit_at(&current_line, self.state.cursor_col - 1) == Some(b'\\' as u16)
            {
                self.handle_backspace();
                self.add_new_line();
                return;
            }

            self.submit_value();
            return;
        }

        // M. 方向键（含历史）。
        if kb.matches(&data, "tui.editor.cursorUp") {
            if self.is_on_first_visual_line()
                && (self.is_editor_empty() || self.history_index > -1 || self.state.cursor_col == 0)
            {
                self.navigate_history(-1);
            } else if self.is_on_first_visual_line() {
                self.move_to_line_start();
            } else {
                self.move_cursor(-1, 0);
            }
            return;
        }
        if kb.matches(&data, "tui.editor.cursorDown") {
            if self.history_index > -1 && self.is_on_last_visual_line() {
                self.navigate_history(1);
            } else if self.is_on_last_visual_line() {
                self.move_to_line_end();
            } else {
                self.move_cursor(1, 0);
            }
            return;
        }
        if kb.matches(&data, "tui.editor.cursorRight") {
            self.move_cursor(0, 1);
            return;
        }
        if kb.matches(&data, "tui.editor.cursorLeft") {
            self.move_cursor(0, -1);
            return;
        }

        // N. 翻页。
        if kb.matches(&data, "tui.editor.pageUp") {
            self.page_scroll(-1);
            return;
        }
        if kb.matches(&data, "tui.editor.pageDown") {
            self.page_scroll(1);
            return;
        }

        // O. 字符跳转触发。
        if kb.matches(&data, "tui.editor.jumpForward") {
            self.jump_mode = Some(JumpDirection::Forward);
            return;
        }
        if kb.matches(&data, "tui.editor.jumpBackward") {
            self.jump_mode = Some(JumpDirection::Backward);
            return;
        }

        // P. Shift+Space 插入普通空格。
        if matches_key(&data, "shift+space") {
            self.insert_character(" ", false);
            return;
        }

        // Q. 可打印键（Kitty CSI-u）。
        if let Some(printable) = decode_printable_key(&data) {
            self.insert_character(&printable, false);
            return;
        }

        // R. 普通字符。
        if utf16_unit_at(&data, 0).is_some_and(|unit| unit >= 32) {
            self.insert_character(&data, false);
        }
    }

    // ------------------------------------------------------------------
    // 基本编辑操作
    // ------------------------------------------------------------------

    /// `insertCharacter(char, skipUndoCoalescing?)`。
    ///
    /// 撤销合并：连续的词字符共用一个快照（`LastAction::TypeWord`），遇到空白或其它动作
    /// 之后才重新 push；`skip_undo_coalescing == true` 时**完全跳过**撤销合并逻辑——既不
    /// push 快照，也不更新 `last_action`。当前三个调用方都传 false，因此该分支实际未使用，
    /// 参数为对齐 TS 保留。
    ///
    /// 插入后会尝试触发或刷新补全：菜单未显示时按“斜杠开头 / 触发字符 / 词字符或 CJK”
    /// 三条路径判定；菜单已显示时用 `update_autocomplete` 按新文本重新请求。
    fn insert_character(&mut self, character: &str, skip_undo_coalescing: bool) {
        self.exit_history_browsing();

        if !skip_undo_coalescing {
            if is_whitespace_char(character) || self.last_action != Some(LastAction::TypeWord) {
                self.push_undo_snapshot();
            }
            self.last_action = Some(LastAction::TypeWord);
        }

        let line = self
            .state
            .lines
            .get(self.state.cursor_line)
            .cloned()
            .unwrap_or_default();

        let before = utf16_prefix(&line, self.state.cursor_col);
        let after = utf16_suffix(&line, self.state.cursor_col);

        self.state.lines[self.state.cursor_line] = format!("{before}{character}{after}");
        let col = self.state.cursor_col + utf16_len(character);
        self.set_cursor_col(col);

        self.emit_change();

        if self.autocomplete_state.is_none() {
            if character == "/" && self.is_at_start_of_message() {
                self.try_trigger_autocomplete(false);
            } else if self
                .autocomplete_trigger_characters
                .iter()
                .any(|candidate| char_equals(*candidate, character))
            {
                let current_line = self
                    .state
                    .lines
                    .get(self.state.cursor_line)
                    .cloned()
                    .unwrap_or_default();
                let text_before_cursor = utf16_prefix(&current_line, self.state.cursor_col);
                if self.matches_trigger_pattern(&text_before_cursor) {
                    self.try_trigger_autocomplete(false);
                }
            } else if contains_word_character(character) || cjk_break_matches(character) {
                let current_line = self
                    .state
                    .lines
                    .get(self.state.cursor_line)
                    .cloned()
                    .unwrap_or_default();
                let text_before_cursor = utf16_prefix(&current_line, self.state.cursor_col);
                if self.is_in_slash_command_context(&text_before_cursor)
                    || self.matches_trigger_pattern(&text_before_cursor)
                {
                    self.try_trigger_autocomplete(false);
                }
            }
        } else {
            self.update_autocomplete();
        }
    }

    /// `addNewLine()`。
    ///
    /// 取消补全、退出历史浏览、push 快照，然后在光标处把当前行拆成两行，光标落到新行行首，
    /// `onChange` 触发。
    fn add_new_line(&mut self) {
        self.cancel_autocomplete();
        self.exit_history_browsing();
        self.last_action = None;

        self.push_undo_snapshot();

        let current_line = self
            .state
            .lines
            .get(self.state.cursor_line)
            .cloned()
            .unwrap_or_default();

        let before = utf16_prefix(&current_line, self.state.cursor_col);
        let after = utf16_suffix(&current_line, self.state.cursor_col);

        self.state.lines[self.state.cursor_line] = before;
        self.state.lines.insert(self.state.cursor_line + 1, after);

        self.state.cursor_line += 1;
        self.set_cursor_col(0);

        self.emit_change();
    }

    /// `shouldSubmitOnBackslashEnter(data, kb)`。
    ///
    /// 只有同时满足以下条件才把 `\` + Enter 视为提交：提交未禁用、按下的键**恰好**是
    /// `enter`、提交绑定里显式包含 `shift+enter`/`shift+return`、且光标前一个 UTF-16
    /// 码元是反斜杠。任一不满足都返回 false，交给后续换行逻辑。
    fn should_submit_on_backslash_enter(
        &self,
        data: &str,
        kb: &crate::keybindings::KeybindingsManager,
    ) -> bool {
        if self.disable_submit {
            return false;
        }
        if !matches_key(data, "enter") {
            return false;
        }
        let submit_keys = kb.get_keys("tui.input.submit");
        let has_shift_enter = submit_keys
            .iter()
            .any(|key| key == "shift+enter" || key == "shift+return");
        if !has_shift_enter {
            return false;
        }

        let current_line = self
            .state
            .lines
            .get(self.state.cursor_line)
            .cloned()
            .unwrap_or_default();
        self.state.cursor_col > 0
            && utf16_unit_at(&current_line, self.state.cursor_col - 1) == Some(b'\\' as u16)
    }

    /// `submitValue()`。
    ///
    /// 提交文本是**展开 marker 后**再按 JS 空白 trim 的结果。调用 `on_submit` 之前编辑器
    /// 已被重置：文本清空、paste registry/计数器清空、撤销栈清空、退出历史、滚动归零，
    /// 并先触发一次 `onChange`（此时文本为空）。因此提交回调里读到的编辑器状态是空文档。
    fn submit_value(&mut self) {
        self.cancel_autocomplete();
        let result = trim_js(&self.expand_paste_markers(&self.state.lines.join("\n"))).to_string();

        self.state = EditorState {
            lines: vec![String::new()],
            cursor_line: 0,
            cursor_col: 0,
        };
        self.pastes.clear();
        self.paste_counter = 0;
        self.exit_history_browsing();
        self.scroll_offset = 0;
        self.undo_stack.clear();
        self.last_action = None;

        self.emit_change();
        if let Some(callback) = &mut self.on_submit {
            callback(&result);
        }
    }

    /// `handlePaste(pastedText)`。
    ///
    /// 处理顺序：解码 tmux popup 的 CSI-u Ctrl 序列（`ESC[<code>;5u` → 控制字符）→
    /// `normalize_text`（CRLF/CR/tab）→ 按 UTF-16 码元过滤不可打印字符（保留换行）→
    /// 若粘贴内容以 `/`、`~`、`.` 开头且光标前是词字符，前置一个空格（避免和前面的词粘连）
    /// → 折叠判断。
    ///
    /// 折叠阈值：按 `\n` 切分**超过 10 行**，或 UTF-16 长度**超过 1000**。折叠时内容存入
    /// 内部的 `pastes` 表（id 递增），光标处只插入 `[paste #id +N lines]`（行数形态，N 是
    /// 切分后的行数）或 `[paste #id N chars]`（字符数形态）；小粘贴直接插入。方法开头只
    /// push 一次撤销快照，因此一次粘贴是一个撤销单元。
    fn handle_paste(&mut self, pasted_text: &str) {
        self.cancel_autocomplete();
        self.exit_history_browsing();
        self.last_action = None;

        self.push_undo_snapshot();

        // tmux popup 的 CSI-u Ctrl 序列解码。
        static CSI_U_CTRL: LazyLock<Regex> =
            LazyLock::new(|| Regex::new(r"\x1b\[([0-9]+);5u").expect("CSI-u regex compiles"));
        let decoded_text = CSI_U_CTRL
            .replace_all(pasted_text, |captures: &regex::Captures<'_>| {
                let Some(code) = captures.get(1) else {
                    return captures[0].to_string();
                };
                let Ok(code) = code.as_str().parse::<u32>() else {
                    return captures[0].to_string();
                };
                if (97..=122).contains(&code) {
                    char::from_u32(code - 96)
                        .map(|ch| ch.to_string())
                        .unwrap_or_else(|| captures[0].to_string())
                } else if (65..=90).contains(&code) {
                    char::from_u32(code - 64)
                        .map(|ch| ch.to_string())
                        .unwrap_or_else(|| captures[0].to_string())
                } else {
                    captures[0].to_string()
                }
            })
            .into_owned();

        let clean_text = normalize_text(&decoded_text);

        // 过滤不可打印字符（保留换行）；JS `split("")` 是 UTF-16 码元粒度。
        let filtered_text = {
            let units = utf16_units(&clean_text);
            let kept: Vec<u16> = units
                .into_iter()
                .filter(|unit| *unit == 0x0A || *unit >= 0x20)
                .collect();
            units_to_string(&kept)
        };

        // 粘贴路径（以 /、~、. 开头）且光标前一个字符是词字符时，前置一个空格。
        let mut filtered_text = filtered_text;
        if filtered_text.starts_with(['/', '~', '.']) {
            let current_line = self
                .state
                .lines
                .get(self.state.cursor_line)
                .cloned()
                .unwrap_or_default();
            let char_before_cursor = if self.state.cursor_col > 0 {
                utf16_unit_at(&current_line, self.state.cursor_col - 1)
                    .and_then(|unit| char::from_u32(u32::from(unit)))
                    .map(|ch| ch.to_string())
                    .unwrap_or_default()
            } else {
                String::new()
            };
            if !char_before_cursor.is_empty() && is_word_character_js(&char_before_cursor) {
                filtered_text = format!(" {filtered_text}");
            }
        }

        let pasted_lines: Vec<&str> = filtered_text.split('\n').collect();
        let total_chars = utf16_len(&filtered_text);
        if pasted_lines.len() > 10 || total_chars > 1000 {
            self.paste_counter += 1;
            let paste_id = self.paste_counter;
            self.pastes.insert(paste_id, filtered_text.clone());

            let marker = if pasted_lines.len() > 10 {
                format!("[paste #{paste_id} +{} lines]", pasted_lines.len())
            } else {
                format!("[paste #{paste_id} {total_chars} chars]")
            };
            self.insert_text_at_cursor_internal(&marker);
            return;
        }

        self.insert_text_at_cursor_internal(&filtered_text);
    }

    /// `expandPasteMarkers(text)`。
    ///
    /// 按 `pastes` 的键（BTreeMap，升序）逐个构造 `[paste #id ...]` 正则并替换。只对
    /// registry 中存在的 id 做替换；手打一个未注册 id 的相似文本保持原样（注意：手打的
    /// 文本如果恰好复用了已注册的 id，也会被展开——实现只认 id，不追踪文本来源）。
    fn expand_paste_markers(&self, text: &str) -> String {
        let mut result = text.to_string();
        for (paste_id, paste_content) in &self.pastes {
            let pattern = format!(r"\[paste #{}( (\+[0-9]+ lines|[0-9]+ chars))?\]", paste_id);
            let Ok(regex) = Regex::new(&pattern) else {
                continue;
            };
            result = regex
                .replace_all(&result, regex::NoExpand(paste_content))
                .into_owned();
        }
        result
    }

    /// `handleBackspace()`。
    ///
    /// 三种情况：
    /// 1. 光标前是有效 paste marker 段：删除整个 marker，同时从 `pastes` 移除该 id、把更高
    ///    id 的 registry 与所有行中的标记减一（`renumber_paste_markers`），`paste_counter`
    ///    同步减一；
    /// 2. 行内有字符：删除光标前的**一个字素**（emoji/组合字符一次删完）；
    /// 3. 行首且不是首行：与上一行合并。
    ///
    /// 每种实际修改都先 push 撤销快照，修改后 `emit_change` 并刷新/触发补全。
    fn handle_backspace(&mut self) {
        self.exit_history_browsing();
        self.last_action = None;

        if self.state.cursor_col > 0 {
            self.push_undo_snapshot();

            let mut line = self
                .state
                .lines
                .get(self.state.cursor_line)
                .cloned()
                .unwrap_or_default();
            let before_cursor = utf16_prefix(&line, self.state.cursor_col);

            let graphemes = self.segment_graphemes(&before_cursor);
            let last_grapheme = graphemes.last().cloned();
            let grapheme_length = last_grapheme
                .as_ref()
                .map_or(1, |segment| utf16_len(&segment.segment));
            let is_pasted_segmented = last_grapheme
                .as_ref()
                .and_then(|segment| paste_marker_id(&segment.segment));

            if let Some(target_id) = is_pasted_segmented {
                self.pastes.remove(&target_id);
                self.paste_counter = self.paste_counter.saturating_sub(1);

                // 按升序把 registry 中 id > target 的条目下移一位。BTreeMap 的 range 天然
                // 升序，先收集 key 再改，避免边遍历边改。
                let higher_ids: Vec<u64> = self
                    .pastes
                    .range(target_id + 1..)
                    .map(|(id, _)| *id)
                    .collect();
                for id in higher_ids {
                    if let Some(content) = self.pastes.remove(&id) {
                        self.pastes.insert(id - 1, content);
                    }
                }

                // 文本中 id > target 的 marker 同步重编号。
                let lines = std::mem::take(&mut self.state.lines);
                self.state.lines = lines
                    .into_iter()
                    .map(|line| renumber_paste_markers(&line, target_id))
                    .collect();
            }

            line = self
                .state
                .lines
                .get(self.state.cursor_line)
                .cloned()
                .unwrap_or_default();

            let before = utf16_prefix(&line, self.state.cursor_col.saturating_sub(grapheme_length));
            let after = utf16_suffix(&line, self.state.cursor_col);

            self.state.lines[self.state.cursor_line] = format!("{before}{after}");
            let col = self.state.cursor_col.saturating_sub(grapheme_length);
            self.set_cursor_col(col);
        } else if self.state.cursor_line > 0 {
            self.push_undo_snapshot();

            let current_line = self
                .state
                .lines
                .get(self.state.cursor_line)
                .cloned()
                .unwrap_or_default();
            let previous_line = self
                .state
                .lines
                .get(self.state.cursor_line - 1)
                .cloned()
                .unwrap_or_default();

            self.state.lines[self.state.cursor_line - 1] = format!("{previous_line}{current_line}");
            self.state.lines.remove(self.state.cursor_line);

            self.state.cursor_line -= 1;
            let col = utf16_len(&previous_line);
            self.set_cursor_col(col);
        }

        self.emit_change();

        if self.autocomplete_state.is_some() {
            self.update_autocomplete();
        } else {
            let current_line = self
                .state
                .lines
                .get(self.state.cursor_line)
                .cloned()
                .unwrap_or_default();
            let text_before_cursor = utf16_prefix(&current_line, self.state.cursor_col);
            if self.is_in_slash_command_context(&text_before_cursor)
                || self.matches_trigger_pattern(&text_before_cursor)
            {
                self.try_trigger_autocomplete(false);
            }
        }
    }

    /// `handleForwardDelete()`。
    ///
    /// 与 Backspace 对称：行内删除光标后的**一个字素**，光标在行尾则与下一行合并；两种都
    /// 先 push 快照。
    ///
    /// 注意：这里不维护 paste registry。字素分段会把有效 marker 当作一个原子段，因此光标
    /// 在 marker 前按删除会删掉 marker 文本，但 `pastes` / `paste_counter` 不变（TS 的
    /// `handleForwardDelete` 同样如此）。
    fn handle_forward_delete(&mut self) {
        self.exit_history_browsing();
        self.last_action = None;

        let current_line = self
            .state
            .lines
            .get(self.state.cursor_line)
            .cloned()
            .unwrap_or_default();

        if self.state.cursor_col < utf16_len(&current_line) {
            self.push_undo_snapshot();

            let after_cursor = utf16_suffix(&current_line, self.state.cursor_col);
            let graphemes = self.segment_graphemes(&after_cursor);
            let grapheme_length = graphemes
                .first()
                .map_or(1, |segment| utf16_len(&segment.segment));

            let before = utf16_prefix(&current_line, self.state.cursor_col);
            let after = utf16_suffix(&current_line, self.state.cursor_col + grapheme_length);
            self.state.lines[self.state.cursor_line] = format!("{before}{after}");
        } else if self.state.cursor_line < self.state.lines.len() - 1 {
            self.push_undo_snapshot();

            let next_line = self
                .state
                .lines
                .get(self.state.cursor_line + 1)
                .cloned()
                .unwrap_or_default();
            self.state.lines[self.state.cursor_line] = format!("{current_line}{next_line}");
            self.state.lines.remove(self.state.cursor_line + 1);
        }

        self.emit_change();

        if self.autocomplete_state.is_some() {
            self.update_autocomplete();
        } else {
            let current_line = self
                .state
                .lines
                .get(self.state.cursor_line)
                .cloned()
                .unwrap_or_default();
            let text_before_cursor = utf16_prefix(&current_line, self.state.cursor_col);
            if self.is_in_slash_command_context(&text_before_cursor)
                || self.matches_trigger_pattern(&text_before_cursor)
            {
                self.try_trigger_autocomplete(false);
            }
        }
    }

    /// `setCursorCol(col)`：清空粘性列。
    ///
    /// 多数水平移动/编辑造成的光标改变走这里，避免 `preferred_visual_col` 残留导致下一次
    /// 上下移动跳回旧列；垂直移动另由 `move_to_visual_line` 维护 `snapped_from_cursor_col`。
    /// 但并非所有路径都经过它：`delete_yanked_text` 的多行分支直接写 `state.cursor_col`
    /// （TS 的 `deleteYankedText` 对应路径调用 `setCursorCol`，会清粘性列，见
    /// `spec/tui-components.md`）；`undo` 与历史草稿恢复整体替换 `state`，其中 `undo` 只清
    /// `preferred_visual_col`（与 TS 相同），历史草稿恢复两者都清。
    fn set_cursor_col(&mut self, col: usize) {
        self.state.cursor_col = col;
        self.preferred_visual_col = None;
        self.snapped_from_cursor_col = None;
    }

    /// `moveToLineStart()`。
    fn move_to_line_start(&mut self) {
        self.last_action = None;
        self.set_cursor_col(0);
    }

    /// `moveToLineEnd()`。
    fn move_to_line_end(&mut self) {
        self.last_action = None;
        let current_line = self
            .state
            .lines
            .get(self.state.cursor_line)
            .cloned()
            .unwrap_or_default();
        let col = utf16_len(&current_line);
        self.set_cursor_col(col);
    }

    /// `deleteToStartOfLine()`。
    ///
    /// 删除光标到行首的内容；已经在行首时改为与上一行合并（kill ring 里记录 `"\n"`）。
    /// 删除内容以 `prepend: true` 加入 kill ring，若上一次动作也是 kill 则与上一条累积，
    /// 因此连续 Ctrl+U 会合成一个条目。
    fn delete_to_start_of_line(&mut self) {
        self.exit_history_browsing();

        let current_line = self
            .state
            .lines
            .get(self.state.cursor_line)
            .cloned()
            .unwrap_or_default();

        if self.state.cursor_col > 0 {
            self.push_undo_snapshot();

            let deleted_text = utf16_prefix(&current_line, self.state.cursor_col);
            let accumulate = self.last_action == Some(LastAction::Kill);
            self.kill_ring.push(
                &deleted_text,
                PushOptions {
                    prepend: true,
                    accumulate,
                },
            );
            self.last_action = Some(LastAction::Kill);

            self.state.lines[self.state.cursor_line] =
                utf16_suffix(&current_line, self.state.cursor_col);
            self.set_cursor_col(0);
        } else if self.state.cursor_line > 0 {
            self.push_undo_snapshot();

            let accumulate = self.last_action == Some(LastAction::Kill);
            self.kill_ring.push(
                "\n",
                PushOptions {
                    prepend: true,
                    accumulate,
                },
            );
            self.last_action = Some(LastAction::Kill);

            let previous_line = self
                .state
                .lines
                .get(self.state.cursor_line - 1)
                .cloned()
                .unwrap_or_default();
            self.state.lines[self.state.cursor_line - 1] = format!("{previous_line}{current_line}");
            self.state.lines.remove(self.state.cursor_line);
            self.state.cursor_line -= 1;
            let col = utf16_len(&previous_line);
            self.set_cursor_col(col);
        }

        self.emit_change();
    }

    /// `deleteToEndOfLine()`。
    ///
    /// 删除光标到行尾的内容；已在行尾时改为与下一行合并（记录 `"\n"`）。内容以
    /// `prepend: false` 加入 kill ring，连续 kill 时追加到上一条末尾。
    fn delete_to_end_of_line(&mut self) {
        self.exit_history_browsing();

        let current_line = self
            .state
            .lines
            .get(self.state.cursor_line)
            .cloned()
            .unwrap_or_default();

        if self.state.cursor_col < utf16_len(&current_line) {
            self.push_undo_snapshot();

            let deleted_text = utf16_suffix(&current_line, self.state.cursor_col);
            let accumulate = self.last_action == Some(LastAction::Kill);
            self.kill_ring.push(
                &deleted_text,
                PushOptions {
                    prepend: false,
                    accumulate,
                },
            );
            self.last_action = Some(LastAction::Kill);

            self.state.lines[self.state.cursor_line] =
                utf16_prefix(&current_line, self.state.cursor_col);
        } else if self.state.cursor_line < self.state.lines.len() - 1 {
            self.push_undo_snapshot();

            let accumulate = self.last_action == Some(LastAction::Kill);
            self.kill_ring.push(
                "\n",
                PushOptions {
                    prepend: false,
                    accumulate,
                },
            );
            self.last_action = Some(LastAction::Kill);

            let next_line = self
                .state
                .lines
                .get(self.state.cursor_line + 1)
                .cloned()
                .unwrap_or_default();
            self.state.lines[self.state.cursor_line] = format!("{current_line}{next_line}");
            self.state.lines.remove(self.state.cursor_line + 1);
        }

        self.emit_change();
    }

    /// `deleteWordBackwards()`。
    ///
    /// 用 [`find_word_backward`] 求删除起点（marker 是原子段），把区间内容以
    /// `prepend: true` 放进 kill ring；光标在行首时退化为与上一行合并。删除后光标移到
    /// 删除起点。
    fn delete_word_backwards(&mut self) {
        self.exit_history_browsing();

        let current_line = self
            .state
            .lines
            .get(self.state.cursor_line)
            .cloned()
            .unwrap_or_default();

        if self.state.cursor_col == 0 {
            if self.state.cursor_line > 0 {
                self.push_undo_snapshot();

                let accumulate = self.last_action == Some(LastAction::Kill);
                self.kill_ring.push(
                    "\n",
                    PushOptions {
                        prepend: true,
                        accumulate,
                    },
                );
                self.last_action = Some(LastAction::Kill);

                let previous_line = self
                    .state
                    .lines
                    .get(self.state.cursor_line - 1)
                    .cloned()
                    .unwrap_or_default();
                self.state.lines[self.state.cursor_line - 1] =
                    format!("{previous_line}{current_line}");
                self.state.lines.remove(self.state.cursor_line);
                self.state.cursor_line -= 1;
                let col = utf16_len(&previous_line);
                self.set_cursor_col(col);
            }
        } else {
            self.push_undo_snapshot();

            let was_kill = self.last_action == Some(LastAction::Kill);

            let old_cursor_col = self.state.cursor_col;
            self.move_word_backwards();
            let delete_from = self.state.cursor_col;
            self.set_cursor_col(old_cursor_col);

            let deleted_text = utf16_slice(&current_line, delete_from, self.state.cursor_col);
            self.kill_ring.push(
                &deleted_text,
                PushOptions {
                    prepend: true,
                    accumulate: was_kill,
                },
            );
            self.last_action = Some(LastAction::Kill);

            self.state.lines[self.state.cursor_line] = format!(
                "{}{}",
                utf16_prefix(&current_line, delete_from),
                utf16_suffix(&current_line, self.state.cursor_col)
            );
            self.set_cursor_col(delete_from);
        }

        self.emit_change();
    }

    /// `deleteWordForward()`。
    ///
    /// 用 [`find_word_forward`] 求删除终点，内容以 `prepend: false` 追加到 kill ring；光标
    /// 已在行尾时改为与下一行合并。与 Backspace 版不同，这里删完光标停在原处。
    fn delete_word_forward(&mut self) {
        self.exit_history_browsing();

        let current_line = self
            .state
            .lines
            .get(self.state.cursor_line)
            .cloned()
            .unwrap_or_default();

        if self.state.cursor_col >= utf16_len(&current_line) {
            if self.state.cursor_line < self.state.lines.len() - 1 {
                self.push_undo_snapshot();

                let accumulate = self.last_action == Some(LastAction::Kill);
                self.kill_ring.push(
                    "\n",
                    PushOptions {
                        prepend: false,
                        accumulate,
                    },
                );
                self.last_action = Some(LastAction::Kill);

                let next_line = self
                    .state
                    .lines
                    .get(self.state.cursor_line + 1)
                    .cloned()
                    .unwrap_or_default();
                self.state.lines[self.state.cursor_line] = format!("{current_line}{next_line}");
                self.state.lines.remove(self.state.cursor_line + 1);
            }
        } else {
            self.push_undo_snapshot();

            let was_kill = self.last_action == Some(LastAction::Kill);

            let old_cursor_col = self.state.cursor_col;
            self.move_word_forwards();
            let delete_to = self.state.cursor_col;
            self.set_cursor_col(old_cursor_col);

            let deleted_text = utf16_slice(&current_line, self.state.cursor_col, delete_to);
            self.kill_ring.push(
                &deleted_text,
                PushOptions {
                    prepend: false,
                    accumulate: was_kill,
                },
            );
            self.last_action = Some(LastAction::Kill);

            self.state.lines[self.state.cursor_line] = format!(
                "{}{}",
                utf16_prefix(&current_line, self.state.cursor_col),
                utf16_suffix(&current_line, delete_to)
            );
        }

        self.emit_change();
    }

    /// `moveWordBackwards()`（编辑器版本：跨行 + paste marker 原子）。
    ///
    /// 行首时直接跳到上一行行尾；否则把 `is_paste_marker` 作为原子段交给
    /// [`find_word_backward`]，让 Ctrl+Left 把 marker 当作整体跳过，而不是停在它内部
    /// （前提是光标本身在 marker 之外）。
    fn move_word_backwards(&mut self) {
        self.last_action = None;
        let current_line = self
            .state
            .lines
            .get(self.state.cursor_line)
            .cloned()
            .unwrap_or_default();

        if self.state.cursor_col == 0 {
            if self.state.cursor_line > 0 {
                self.state.cursor_line -= 1;
                let prev_line = self
                    .state
                    .lines
                    .get(self.state.cursor_line)
                    .cloned()
                    .unwrap_or_default();
                let col = utf16_len(&prev_line);
                self.set_cursor_col(col);
            }
            return;
        }

        let new_col = {
            let segment_fn = |text: &str| self.segment_word_segments(text);
            let options = WordNavigationOptions {
                segment: Some(&segment_fn),
                is_atomic_segment: Some(&is_paste_marker),
            };
            find_word_backward(&current_line, self.state.cursor_col, Some(&options))
        };
        self.set_cursor_col(new_col);
    }

    /// `moveWordForwards()`（编辑器版本：跨行 + paste marker 原子）。
    ///
    /// 行尾时跳到下一行行首；否则用 [`find_word_forward`]，marker 作为整体跳过。
    fn move_word_forwards(&mut self) {
        self.last_action = None;
        let current_line = self
            .state
            .lines
            .get(self.state.cursor_line)
            .cloned()
            .unwrap_or_default();

        if self.state.cursor_col >= utf16_len(&current_line) {
            if self.state.cursor_line < self.state.lines.len() - 1 {
                self.state.cursor_line += 1;
                self.set_cursor_col(0);
            }
            return;
        }

        let new_col = {
            let segment_fn = |text: &str| self.segment_word_segments(text);
            let options = WordNavigationOptions {
                segment: Some(&segment_fn),
                is_atomic_segment: Some(&is_paste_marker),
            };
            find_word_forward(&current_line, self.state.cursor_col, Some(&options))
        };
        self.set_cursor_col(new_col);
    }
}

/// 把一行里 id > `target_id` 的 paste marker 重编号为 `id - 1`。
///
/// 全局替换一行内所有 marker；`id <= target_id` 的原样保留。没有后缀的 marker 会得到字面
/// `undefined`——TS 回调里未参与匹配的可选组是 `undefined`，模板字符串原样写出，必须逐字
/// 保留（见测试 `renumber_paste_markers_appends_undefined_suffix_like_ts`）。
fn renumber_paste_markers(line: &str, target_id: u64) -> String {
    PASTE_MARKER_REGEX
        .replace_all(line, |captures: &regex::Captures<'_>| {
            let Some(whole) = captures.get(0) else {
                return String::new();
            };
            let Some(id) = captures
                .get(1)
                .and_then(|id| id.as_str().parse::<u64>().ok())
            else {
                return whole.as_str().to_string();
            };
            if id <= target_id {
                return whole.as_str().to_string();
            }
            // TS 的替换回调直接拼接 `${suffixGroup}`：可选组未参与匹配时该值是
            // `undefined`，模板字符串会原样写出 "undefined"（`[paste #1undefined]`）。
            // 这是当前输出契约的一部分，必须逐字保留。
            let suffix = captures.get(2).map_or("undefined", |group| group.as_str());
            format!("[paste #{}", id - 1) + suffix + "]"
        })
        .into_owned()
}

/// `char === character`（TS 的整串比较）。
///
/// 把 `candidate` 编码成 UTF-8 后与整个 `character` 比较，所以多字符字符串（如 `"ab"`）
/// 永远不相等；只有单个字符的 UTF-8 表示与它逐字节相同才返回 true。
fn char_equals(candidate: char, character: &str) -> bool {
    let mut buffer = [0u8; 4];
    candidate.encode_utf8(&mut buffer) == character
}

impl Editor {
    // ------------------------------------------------------------------
    // 视觉行 / 光标移动
    // ------------------------------------------------------------------

    /// `buildVisualLineMap(width)`：把每个逻辑行映射为一条或多条视觉行。
    ///
    /// 空行产出一条 `length: 0` 的视觉行（光标可以停在这里）；可见宽度不超过 `width` 的
    /// 行产出一条；超宽行用 [`word_wrap_line`] 切分。`start_col`/`length` 是逻辑行内的
    /// UTF-16 区间，`length` 为 0 只出现在空行。
    ///
    /// `width` 必须与渲染时的布局宽度一致（即 `last_width`），否则鼠标命中与上下移动会
    /// 与实际显示错位。
    fn build_visual_line_map(&self, width: usize) -> Vec<VisualLine> {
        let mut visual_lines: Vec<VisualLine> = Vec::new();

        for (i, line) in self.state.lines.iter().enumerate() {
            let line = line.as_str();
            let line_vis_width = visible_width(line);
            if line.is_empty() {
                visual_lines.push(VisualLine {
                    logical_line: i,
                    start_col: 0,
                    length: 0,
                });
            } else if line_vis_width <= width {
                visual_lines.push(VisualLine {
                    logical_line: i,
                    start_col: 0,
                    length: utf16_len(line),
                });
            } else {
                let segments = self.segment_graphemes(line);
                let chunks = word_wrap_line(line, width, Some(&segments));
                for chunk in chunks {
                    visual_lines.push(VisualLine {
                        logical_line: i,
                        start_col: chunk.start_index,
                        length: chunk.end_index - chunk.start_index,
                    });
                }
            }
        }

        visual_lines
    }

    /// `findVisualLineAt(visualLines, line, col)`。
    ///
    /// 返回包含 `(line, col)` 的视觉段下标。`col` 等于段尾（`start + length`）时只匹配该
    /// 逻辑行的**最后一个**视觉段（行尾光标属于最后一段）；没有段匹配时回退到最后一个
    /// 视觉段的下标（防御性行为，正常状态不会发生）。
    fn find_visual_line_at(&self, visual_lines: &[VisualLine], line: usize, col: usize) -> usize {
        for (i, visual_line) in visual_lines.iter().enumerate() {
            if visual_line.logical_line != line {
                continue;
            }
            let offset = col as isize - visual_line.start_col as isize;
            let is_last_segment_of_line = i + 1 == visual_lines.len()
                || visual_lines[i + 1].logical_line != visual_line.logical_line;
            if offset >= 0
                && ((offset as usize) < visual_line.length
                    || (is_last_segment_of_line && offset as usize == visual_line.length))
            {
                return i;
            }
        }
        visual_lines.len().saturating_sub(1)
    }

    /// `findCurrentVisualLine(visualLines)`。
    fn find_current_visual_line(&self, visual_lines: &[VisualLine]) -> usize {
        self.find_visual_line_at(visual_lines, self.state.cursor_line, self.state.cursor_col)
    }

    /// `moveCursor(deltaLine, deltaCol)`。
    ///
    /// `delta_line` 按**视觉行**移动（先重建 `last_width` 下的视觉行映射），`delta_col`
    /// 按字素左右移动并可跨逻辑行。`delta_col > 0` 且已在末行末列时只记录
    /// `preferred_visual_col`，不改变光标；两端越界都是 no-op。移动后若补全菜单可见，会
    /// 按新位置重新请求。
    fn move_cursor(&mut self, delta_line: isize, delta_col: isize) {
        self.last_action = None;
        let visual_lines = self.build_visual_line_map(self.last_width);
        let current_visual_line = self.find_current_visual_line(&visual_lines);

        if delta_line != 0 {
            let target_visual_line = current_visual_line as isize + delta_line;

            if target_visual_line >= 0 && (target_visual_line as usize) < visual_lines.len() {
                self.move_to_visual_line(
                    &visual_lines,
                    current_visual_line,
                    target_visual_line as usize,
                );
            }
        }

        if delta_col != 0 {
            let current_line = self
                .state
                .lines
                .get(self.state.cursor_line)
                .cloned()
                .unwrap_or_default();
            let current_line_len = utf16_len(&current_line);

            if delta_col > 0 {
                // 右移一个 grapheme。
                if self.state.cursor_col < current_line_len {
                    let after_cursor = utf16_suffix(&current_line, self.state.cursor_col);
                    let graphemes = self.segment_graphemes(&after_cursor);
                    let first_len = graphemes
                        .first()
                        .map_or(1, |segment| utf16_len(&segment.segment));
                    let col = self.state.cursor_col + first_len;
                    self.set_cursor_col(col);
                } else if self.state.cursor_line < self.state.lines.len() - 1 {
                    self.state.cursor_line += 1;
                    self.set_cursor_col(0);
                } else {
                    // 末行末位：记住列，供上下移动使用。
                    if let Some(current_vl) = visual_lines.get(current_visual_line) {
                        self.preferred_visual_col =
                            Some(self.state.cursor_col.saturating_sub(current_vl.start_col));
                    }
                }
            } else {
                // 左移一个 grapheme。
                if self.state.cursor_col > 0 {
                    let before_cursor = utf16_prefix(&current_line, self.state.cursor_col);
                    let graphemes = self.segment_graphemes(&before_cursor);
                    let last_len = graphemes
                        .last()
                        .map_or(1, |segment| utf16_len(&segment.segment));
                    let col = self.state.cursor_col - last_len;
                    self.set_cursor_col(col);
                } else if self.state.cursor_line > 0 {
                    self.state.cursor_line -= 1;
                    let prev_line = self
                        .state
                        .lines
                        .get(self.state.cursor_line)
                        .cloned()
                        .unwrap_or_default();
                    let col = utf16_len(&prev_line);
                    self.set_cursor_col(col);
                }
            }
        }

        if self.autocomplete_state.is_some() {
            self.update_autocomplete();
        }
    }

    /// `moveToVisualLine(visualLines, currentVisualLine, targetVisualLine)`。
    ///
    /// 1. 计算源视觉列；若上次曾吸附到原子段起点，用吸附前的位置重新定位视觉行（终端
    ///    重排后仍能回到原列）。
    /// 2. 源/目标段的最大列：逻辑行的最后一段允许到 `length`（行尾），其余段只能到
    ///    `length - 1`，避免落到下一段开头。
    /// 3. 由 [`Editor::compute_vertical_move_column`] 决定目标列（粘性列规则）。
    /// 4. 目标列超过逻辑行长度时夹到行尾。
    /// 5. 若目标列落在多字素原子段（宽字符/emoji/paste marker）中间，吸附到段首；从段首
    ///    向下移动时跳过该原子段的所有续段，避免光标停在原子段内部。
    fn move_to_visual_line(
        &mut self,
        visual_lines: &[VisualLine],
        current_visual_line: usize,
        target_visual_line: usize,
    ) {
        let Some(current_vl) = visual_lines.get(current_visual_line).copied() else {
            return;
        };
        let Some(target_vl) = visual_lines.get(target_visual_line).copied() else {
            return;
        };

        // 光标曾吸附到段首时，用吸附前的位置解析视觉列（resize 重排后仍正确）。
        let current_visual_col = if let Some(snapped) = self.snapped_from_cursor_col {
            let vl_index = self.find_visual_line_at(visual_lines, current_vl.logical_line, snapped);
            snapped.saturating_sub(visual_lines[vl_index].start_col)
        } else {
            self.state.cursor_col.saturating_sub(current_vl.start_col)
        };

        // 逻辑行最后一个视觉段允许停在段尾（行尾光标）；其余段最多到 length-1。
        let is_last_source_segment = current_visual_line + 1 == visual_lines.len()
            || visual_lines[current_visual_line + 1].logical_line != current_vl.logical_line;
        let source_max_visual_col = if is_last_source_segment {
            current_vl.length
        } else {
            current_vl.length.saturating_sub(1)
        };

        let is_last_target_segment = target_visual_line + 1 == visual_lines.len()
            || visual_lines[target_visual_line + 1].logical_line != target_vl.logical_line;
        let target_max_visual_col = if is_last_target_segment {
            target_vl.length
        } else {
            target_vl.length.saturating_sub(1)
        };

        let move_to_visual_col = self.compute_vertical_move_column(
            current_visual_col,
            source_max_visual_col,
            target_max_visual_col,
        );

        self.state.cursor_line = target_vl.logical_line;
        let target_col = target_vl.start_col + move_to_visual_col;
        let logical_line = self
            .state
            .lines
            .get(target_vl.logical_line)
            .cloned()
            .unwrap_or_default();
        self.state.cursor_col = target_col.min(utf16_len(&logical_line));

        // 吸附到原子段边界（如 paste marker），避免光标落在多字素单位的中间。
        let segments = self.segment_graphemes(&logical_line);
        for segment in &segments {
            if segment.index > self.state.cursor_col {
                break;
            }
            let segment_len = utf16_len(&segment.segment);
            // 单码元段没有“中间”，无需吸附；多码元段（宽字符/emoji/组合序列/marker）
            // 落在其内部时才处理。
            if segment_len <= 1 {
                continue;
            }
            if self.state.cursor_col < segment.index + segment_len {
                let is_continuation = segment.index < target_vl.start_col;
                let is_moving_down = target_visual_line > current_visual_line;

                if is_continuation && is_moving_down {
                    // 段从上一个视觉行开始且向下移动：跳过所有续段。
                    let segment_end = segment.index + segment_len;
                    let mut next = target_visual_line + 1;
                    while next < visual_lines.len()
                        && visual_lines[next].logical_line == target_vl.logical_line
                        && visual_lines[next].start_col < segment_end
                    {
                        next += 1;
                    }
                    if next < visual_lines.len() {
                        self.move_to_visual_line(visual_lines, current_visual_line, next);
                        return;
                    }
                }

                // 记录吸附前的列，下一次垂直移动用它恢复用户想要的视觉列。
                self.snapped_from_cursor_col = Some(self.state.cursor_col);
                self.state.cursor_col = segment.index;
                return;
            }
        }

        self.snapped_from_cursor_col = None;
    }

    /// `computeVerticalMoveColumn(currentVisualCol, sourceMaxVisualCol, targetMaxVisualCol)`。
    ///
    /// 粘性列决策表（行为契约，调整需同步回归测试）：
    ///
    /// | P | S | T | U | 场景 | Set Preferred | Move To |
    /// |---|---|---|---|---|---|---|
    /// | 0 | * | 0 | - | 开始导航，目标够长 | null | current |
    /// | 0 | * | 1 | - | 开始导航，目标更短 | current | target end |
    /// | 1 | 0 | 0 | 0 | 被夹取，目标容得下 preferred | null | preferred |
    /// | 1 | 0 | 0 | 1 | 被夹取，目标更长但仍容不下 preferred | keep | target end |
    /// | 1 | 0 | 1 | - | 被夹取，目标更短 | keep | target end |
    /// | 1 | 1 | 0 | - | 重排后，目标容得下当前列 | null | current |
    /// | 1 | 1 | 1 | - | 重排后，目标比当前列短 | current | target end |
    fn compute_vertical_move_column(
        &mut self,
        current_visual_col: usize,
        source_max_visual_col: usize,
        target_max_visual_col: usize,
    ) -> usize {
        let has_preferred = self.preferred_visual_col.is_some(); // P
        let cursor_in_middle = current_visual_col < source_max_visual_col; // S
        let target_too_short = target_max_visual_col < current_visual_col; // T

        if !has_preferred || cursor_in_middle {
            if target_too_short {
                // Cases 2 and 7
                self.preferred_visual_col = Some(current_visual_col);
                return target_max_visual_col;
            }

            // Cases 1 and 6
            self.preferred_visual_col = None;
            return current_visual_col;
        }

        let preferred = self.preferred_visual_col.expect("checked is_some");
        let target_cant_fit_preferred = target_max_visual_col < preferred; // U
        if target_too_short || target_cant_fit_preferred {
            // Cases 4 and 5
            return target_max_visual_col;
        }

        // Case 3
        self.preferred_visual_col = None;
        preferred
    }

    /// `pageScroll(direction)`。
    ///
    /// 页大小是 `max(5, floor(terminal_rows * 0.3))`，与可见行上限 `max_visible_lines`
    /// 使用同一公式。目标视觉行夹到文档首尾后走
    /// `move_to_visual_line`，因此保留粘性列与原子段吸附语义。`direction` 为 -1/1。
    fn page_scroll(&mut self, direction: isize) {
        self.last_action = None;
        let terminal_rows = self.tui.rows();
        let page_size = ((terminal_rows as f64 * 0.3).floor() as isize).max(5);

        let visual_lines = self.build_visual_line_map(self.last_width);
        let current_visual_line = self.find_current_visual_line(&visual_lines);
        let target_visual_line = (current_visual_line as isize + direction * page_size)
            .clamp(0, visual_lines.len().saturating_sub(1) as isize)
            as usize;

        self.move_to_visual_line(&visual_lines, current_visual_line, target_visual_line);
    }

    /// `jumpToChar(char, direction)`。
    ///
    /// 当前行前向从 `cursor_col + 1` 开始找、后向从 `cursor_col - 1` 开始找；找不到就逐行
    /// 向外扩展，直到文档首/尾。**不环绕**：到边界仍未找到时光标不动。匹配是按 UTF-16
    /// 码元子串比较（`utf16_index_of` / `utf16_last_index_of`），大小写敏感。
    fn jump_to_char(&mut self, target: &str, direction: JumpDirection) {
        self.last_action = None;
        let is_forward = direction == JumpDirection::Forward;
        let lines = &self.state.lines;

        let cursor_line = self.state.cursor_line;
        let end: isize = if is_forward { lines.len() as isize } else { -1 };
        let step: isize = if is_forward { 1 } else { -1 };

        let mut line_index = cursor_line as isize;
        while line_index != end {
            let line = lines.get(line_index as usize).cloned().unwrap_or_default();
            let is_current_line = line_index as usize == cursor_line;

            let found = if is_forward {
                let search_from = if is_current_line {
                    Some(self.state.cursor_col + 1)
                } else {
                    None
                };
                utf16_index_of(&line, target, search_from.unwrap_or(0))
            } else {
                let search_from = if is_current_line {
                    Some(self.state.cursor_col.saturating_sub(1))
                } else {
                    None
                };
                utf16_last_index_of(&line, target, search_from)
            };

            if let Some(index) = found {
                self.state.cursor_line = line_index as usize;
                self.set_cursor_col(index);
                return;
            }

            line_index += step;
        }
    }

    // ------------------------------------------------------------------
    // kill ring / 撤销
    // ------------------------------------------------------------------

    /// `yank()`。
    ///
    /// 把 kill ring 最新一条插入光标处；push 撤销快照，并把 `last_action` 置为 `Yank`，
    /// 供随后的 `yank_pop` 判断。环为空时 no-op。
    fn yank(&mut self) {
        if self.kill_ring.is_empty() {
            return;
        }

        self.push_undo_snapshot();

        let text = self.kill_ring.peek().unwrap_or("").to_string();
        self.insert_yanked_text(&text);

        self.last_action = Some(LastAction::Yank);
    }

    /// `yankPop()`。
    ///
    /// 仅在上一个动作是 `LastAction::Yank` 且环内至少两条时生效：删除上次插入的文本、
    /// 旋转 kill ring、再插入新的环顶。连续 Alt+Y 会继续旋转，直到其它动作打断
    /// （`last_action` 被清空）。
    fn yank_pop(&mut self) {
        if self.last_action != Some(LastAction::Yank) || self.kill_ring.len() <= 1 {
            return;
        }

        self.push_undo_snapshot();

        self.delete_yanked_text();

        self.kill_ring.rotate();

        let text = self.kill_ring.peek().unwrap_or("").to_string();
        self.insert_yanked_text(&text);

        self.last_action = Some(LastAction::Yank);
    }

    /// `insertYankedText(text)`。
    ///
    /// 单行文本插在光标处；多行文本把第一行拼到光标前、最后一行拼到光标后，中间行整行
    /// 插入，光标落在最后一行的插入内容之后。会 `emit_change`，但不 push 快照（调用方
    /// `yank`/`yank_pop` 已经 push）。
    fn insert_yanked_text(&mut self, text: &str) {
        self.exit_history_browsing();
        let lines: Vec<String> = text.split('\n').map(str::to_string).collect();

        if lines.len() == 1 {
            let current_line = self
                .state
                .lines
                .get(self.state.cursor_line)
                .cloned()
                .unwrap_or_default();
            let before = utf16_prefix(&current_line, self.state.cursor_col);
            let after = utf16_suffix(&current_line, self.state.cursor_col);
            self.state.lines[self.state.cursor_line] = format!("{before}{text}{after}");
            let col = self.state.cursor_col + utf16_len(text);
            self.set_cursor_col(col);
        } else {
            let current_line = self
                .state
                .lines
                .get(self.state.cursor_line)
                .cloned()
                .unwrap_or_default();
            let before = utf16_prefix(&current_line, self.state.cursor_col);
            let after = utf16_suffix(&current_line, self.state.cursor_col);

            let first = lines.first().cloned().unwrap_or_default();
            self.state.lines[self.state.cursor_line] = format!("{before}{first}");

            for i in 1..lines.len().saturating_sub(1) {
                let line = lines.get(i).cloned().unwrap_or_default();
                self.state.lines.insert(self.state.cursor_line + i, line);
            }

            let last_line_index = self.state.cursor_line + lines.len() - 1;
            let last = lines.last().cloned().unwrap_or_default();
            self.state
                .lines
                .insert(last_line_index, format!("{last}{after}"));

            self.state.cursor_line = last_line_index;
            let col = utf16_len(&last);
            self.set_cursor_col(col);
        }

        self.emit_change();
    }

    /// `deleteYankedText()`。
    ///
    /// 删除“上一次插入的文本”，依赖调用约定：只在 `yank_pop` 里、光标仍停在插入内容末尾
    /// 时调用。单行时从光标回退 `utf16_len(text)`；多行时定位到起始行、按行数做 splice。
    fn delete_yanked_text(&mut self) {
        let Some(yanked_text) = self.kill_ring.peek().map(str::to_string) else {
            return;
        };

        let yank_lines: Vec<String> = yanked_text.split('\n').map(str::to_string).collect();

        if yank_lines.len() == 1 {
            let current_line = self
                .state
                .lines
                .get(self.state.cursor_line)
                .cloned()
                .unwrap_or_default();
            let delete_len = utf16_len(&yanked_text);
            let before = utf16_prefix(
                &current_line,
                self.state.cursor_col.saturating_sub(delete_len),
            );
            let after = utf16_suffix(&current_line, self.state.cursor_col);
            self.state.lines[self.state.cursor_line] = format!("{before}{after}");
            let col = self.state.cursor_col.saturating_sub(delete_len);
            self.set_cursor_col(col);
        } else {
            let start_line = self.state.cursor_line.saturating_sub(yank_lines.len() - 1);
            let start_line_text = self
                .state
                .lines
                .get(start_line)
                .cloned()
                .unwrap_or_default();
            let first_len = yank_lines.first().map_or(0, |line| utf16_len(line));
            let start_col = utf16_len(&start_line_text).saturating_sub(first_len);

            let current_line = self
                .state
                .lines
                .get(self.state.cursor_line)
                .cloned()
                .unwrap_or_default();
            let after_cursor = utf16_suffix(&current_line, self.state.cursor_col);
            let before_yank = utf16_prefix(&start_line_text, start_col);

            self.state.lines.splice(
                start_line..start_line + yank_lines.len(),
                std::iter::once(format!("{before_yank}{after_cursor}")),
            );

            self.state.cursor_line = start_line;
            self.state.cursor_col = start_col;
        }

        self.emit_change();
    }

    /// `pushUndoSnapshot()`。
    ///
    /// 快照包含文本/光标、paste registry 与 `paste_counter`——三者必须一起恢复，否则
    /// 撤销后 marker 会指向错误的内容。何时压栈由各操作决定（例如连续打字会复用同一个
    /// 快照）；`UndoStack` 只存克隆，不做去重/合并。
    fn push_undo_snapshot(&mut self) {
        let snapshot = EditorSnapshot {
            state: self.state.clone(),
            pastes: self.pastes.clone(),
            paste_counter: self.paste_counter,
        };
        self.undo_stack.push(&snapshot);
    }

    /// `undo()`。
    ///
    /// 先退出历史浏览；栈空时 no-op。恢复文本、光标、paste registry 与计数器，清空
    /// `last_action` 与粘性列，然后 `emit_change`。不触发自动补全。
    fn undo(&mut self) {
        self.exit_history_browsing();
        let Some(snapshot) = self.undo_stack.pop() else {
            return;
        };
        self.state = snapshot.state;
        self.pastes = snapshot.pastes;
        self.paste_counter = snapshot.paste_counter;
        self.last_action = None;
        self.preferred_visual_col = None;
        self.emit_change();
    }

    // ------------------------------------------------------------------
    // 自动补全
    // ------------------------------------------------------------------
    //
    // 请求侧的状态由几个 Option 字段组合表示（菜单 UI 的 `autocomplete_state` 是另一回事，
    // 请求在飞时旧菜单仍可显示）：
    //
    //   idle           : 所有请求字段为 None
    //   debouncing     : `autocomplete_pending_debounce = Some`（等待 20ms 截止）
    //   queued         : `autocomplete_queued = Some`（等待 pump 启动）
    //   in_flight      : `autocomplete_in_flight = Some`（provider 暂存在 future 里）
    //   deferred_force : `autocomplete_deferred_force = Some`（等 provider 归位后补判谓词）
    //
    // 正常转换：debouncing --到期--> queued --pump--> in_flight --Ready--> 应用或丢弃。
    // `cancel_autocomplete_request` 清掉防抖/排队/推迟项并递增 token；在飞 future 本身留在
    // 原地继续被轮询，直到它自己就绪后被快照校验丢弃。

    fn has_provider(&self) -> bool {
        self.autocomplete_provider.borrow().is_some()
    }

    /// provider 可用：要么在槽位里，要么被请求 future 临时持有（TS 里 provider 始终存在）。
    fn provider_available(&self) -> bool {
        self.has_provider() || self.autocomplete_in_flight.is_some()
    }

    fn provider_should_trigger_file_completion(&self) -> bool {
        self.provider_should_trigger_file_completion_at(
            &self.state.lines,
            self.state.cursor_line,
            self.state.cursor_col,
        )
    }

    /// 用指定快照调用 `shouldTriggerFileCompletion`（deferred force 用请求时的快照）。
    fn provider_should_trigger_file_completion_at(
        &self,
        lines: &[String],
        cursor_line: usize,
        cursor_col: usize,
    ) -> bool {
        let cell = self.autocomplete_provider.borrow();
        let Some(provider) = cell.as_ref() else {
            return false;
        };
        provider.should_trigger_file_completion(lines, cursor_line, cursor_col)
    }

    /// 取回被在飞请求独占的 provider（丢弃该请求）。
    ///
    /// TS 的 `applyCompletion` 是同步调用，请求在飞期间也能执行；Rust 只能用丢弃 future 的
    /// 方式取回 provider。被丢弃请求的结果本来也会被随后的 `cancelAutocomplete()` 作废，
    /// 因此可观察行为一致。
    fn reclaim_provider(&mut self) {
        if self.has_provider() {
            return;
        }
        if let Some(controller) = self.autocomplete_controller.take() {
            controller.cancel();
        }
        // Drop future → `ProviderReturnGuard` 把 provider 放回槽位。
        self.autocomplete_in_flight = None;
    }

    /// `applyCompletion`：返回是否真的应用。
    ///
    /// 先 `reclaim_provider`：若 provider 被在飞 future 独占就丢弃该请求取回。随后同步调用
    /// `provider.apply_completion` 并用返回值整体替换文本/光标。没有 provider 时返回 false。
    /// 本方法不 push 快照、不 `emit_change`、不取消补全——这些由调用方按场景决定。
    fn apply_completion(&mut self, item: &AutocompleteItem, prefix: &str) -> bool {
        self.reclaim_provider();
        let result: ApplyCompletionResult = {
            let cell = self.autocomplete_provider.borrow();
            let Some(provider) = cell.as_ref() else {
                return false;
            };
            provider.apply_completion(
                &self.state.lines,
                self.state.cursor_line,
                self.state.cursor_col,
                item,
                prefix,
            )
        };
        self.state.lines = result.lines;
        self.state.cursor_line = result.cursor_line;
        self.set_cursor_col(result.cursor_col);
        true
    }

    /// 应用下拉列表里的选中项（`SelectItem` → `AutocompleteItem`）。
    fn apply_completion_select_item(&mut self, item: &SelectItem, prefix: &str) -> bool {
        let autocomplete_item = AutocompleteItem {
            value: item.value.clone(),
            label: item.label.clone(),
            description: item.description.clone(),
        };
        self.apply_completion(&autocomplete_item, prefix)
    }

    /// `getBestAutocompleteMatchIndex(items, prefix)`。
    ///
    /// 优先返回 `value == prefix` 的精确匹配；否则返回第一个 `starts_with(prefix)` 的下标；
    /// `prefix` 为空或无匹配时返回 `None`。用于打开菜单时预选条目。
    fn get_best_autocomplete_match_index(
        &self,
        items: &[AutocompleteItem],
        prefix: &str,
    ) -> Option<usize> {
        if prefix.is_empty() {
            return None;
        }

        let mut first_prefix_index: Option<usize> = None;
        for (i, item) in items.iter().enumerate() {
            if item.value == prefix {
                return Some(i);
            }
            if first_prefix_index.is_none() && item.value.starts_with(prefix) {
                first_prefix_index = Some(i);
            }
        }

        first_prefix_index
    }

    /// `createAutocompleteList(prefix, items)`。
    ///
    /// 斜杠命令（prefix 以 `/` 开头）使用 12/32 列的主列宽布局，其余用默认布局。
    /// `on_select` 不能借用编辑器，因此只把选中项写进共享的 `autocomplete_selected`
    /// 槽位，由 `drain_autocomplete_selection` 在渲染/鼠标处理后取走应用。
    fn create_autocomplete_list(&self, prefix: &str, items: &[AutocompleteItem]) -> SelectList {
        let layout = if prefix.starts_with('/') {
            SelectListLayoutOptions {
                min_primary_column_width: Some(SLASH_COMMAND_MIN_PRIMARY_COLUMN_WIDTH),
                max_primary_column_width: Some(SLASH_COMMAND_MAX_PRIMARY_COLUMN_WIDTH),
                truncate_primary: None,
            }
        } else {
            SelectListLayoutOptions::default()
        };
        let select_items: Vec<SelectItem> = items.iter().map(to_select_item).collect();
        let mut list = SelectList::new(
            select_items,
            self.autocomplete_max_visible,
            self.theme.select_list.clone(),
            layout,
        );
        let selected = Rc::clone(&self.autocomplete_selected);
        list.on_select = Some(Box::new(move |item: &SelectItem| {
            *selected.borrow_mut() = Some(item.clone());
        }));
        list
    }

    /// `tryTriggerAutocomplete(explicitTab)`。
    fn try_trigger_autocomplete(&mut self, explicit_tab: bool) {
        self.request_autocomplete(RequestOptions {
            force: false,
            explicit_tab,
        });
    }

    /// `handleTabCompletion()`。
    ///
    /// Tab 在两种上下文里含义不同：光标位于第一行（`is_slash_menu_allowed()`）、光标前
    /// 文本经 `trim_start_js` 后以 `/` 开头**且不含 ASCII 空格**时，触发斜杠命令补全（普通
    /// 请求 + `explicit_tab`）；否则强制文件补全（`force`，会先问 provider 的
    /// `should_trigger_file_completion`）。没有可用 provider 时直接返回。
    fn handle_tab_completion(&mut self) {
        if !self.provider_available() {
            return;
        }

        let current_line = self
            .state
            .lines
            .get(self.state.cursor_line)
            .cloned()
            .unwrap_or_default();
        let before_cursor = utf16_prefix(&current_line, self.state.cursor_col);

        if self.is_in_slash_command_context(&before_cursor)
            && !trim_start_js(&before_cursor).contains(' ')
        {
            self.handle_slash_command_completion();
        } else {
            self.force_file_autocomplete(true);
        }
    }

    /// `handleSlashCommandCompletion()`。
    ///
    /// 与文件补全不同，这里 `force: false`：不经过 `should_trigger_file_completion`，直接
    /// 排队（`explicit_tab` 让 `get_autocomplete_debounce_ms` 返回 0，不防抖）。
    fn handle_slash_command_completion(&mut self) {
        self.request_autocomplete(RequestOptions {
            force: false,
            explicit_tab: true,
        });
    }

    /// `forceFileAutocomplete(explicitTab)`。
    fn force_file_autocomplete(&mut self, explicit_tab: bool) {
        self.request_autocomplete(RequestOptions {
            force: true,
            explicit_tab,
        });
    }

    /// `requestAutocomplete(options)`。
    ///
    /// 执行顺序如下，顺序本身影响结果：
    /// 1. 没有可用 provider 直接返回（在飞请求独占 provider 时仍算“可用”）。
    /// 2. `force` 时先问 `should_trigger_file_completion`。谓词在取消旧请求**之前**执行，
    ///    返回 false 时连在飞请求都不取消；provider 此刻被独占则把快照记进
    ///    `autocomplete_deferred_force`，等结果落地后补判。
    /// 3. `cancel_autocomplete_request`（内部已经递增一次 token）+ 再 `start_token += 1`
    ///    （让旧排队项过期）。
    /// 4. `explicit_tab`/`force` 不防抖；其余情况若 debounce 正则命中当前文本，记一个
    ///    绝对截止时间并安排定时器，否则直接放入 `autocomplete_queued`。
    ///
    /// 排队项不会立即调用 provider：真正启动在 [`Editor::pump_autocomplete`]。
    fn request_autocomplete(&mut self, options: RequestOptions) {
        // TS：`if (!this.autocompleteProvider) return;`（provider 在请求在飞期间仍然存在）。
        if !self.provider_available() {
            return;
        }

        if options.force {
            if self.has_provider() {
                // 谓词检查在 cancelAutocompleteRequest **之前**（TS 顺序）：返回 false 时
                // 连在飞请求都不取消。
                if !self.provider_should_trigger_file_completion() {
                    return;
                }
            } else {
                // provider 被在飞请求独占，无法此刻调用谓词；记下请求时的快照，
                // 等该请求结束、provider 归位后由 `finish_autocomplete_request` 补做判定。
                self.autocomplete_deferred_force = Some(DeferredForceRequest {
                    options,
                    lines: self.state.lines.clone(),
                    cursor_line: self.state.cursor_line,
                    cursor_col: self.state.cursor_col,
                });
                return;
            }
        }

        self.cancel_autocomplete_request();
        self.autocomplete_start_token += 1;
        let start_token = self.autocomplete_start_token;

        let debounce_ms = self.get_autocomplete_debounce_ms(options);
        if debounce_ms > 0 {
            let deadline_ms = (self.clock)() + debounce_ms as f64;
            self.autocomplete_pending_debounce = Some(PendingDebounce {
                start_token,
                deadline_ms,
                options,
            });
            // TS：`setTimeout(() => { timer = undefined; startAutocompleteRequest(...) }, ms)`。
            // 到点请求一次重绘；宿主渲染时 pump 会把到期的防抖请求排进队列并启动。
            self.arm_debounce_timer(debounce_ms);
            return;
        }

        // TS 的 `startAutocompleteRequest` 在 `await previousTask` 处至少让出一个微任务，
        // 因此请求只排队，真正调用 provider 留到 pump（宿主处理完这一批输入之后）。
        self.autocomplete_queued = Some(QueuedRequest {
            start_token,
            options,
        });
    }

    /// `startAutocompleteRequest(startToken, options)`：发起一次（可轮询的）请求。
    ///
    /// 由 [`Editor::pump_autocomplete`] 在轮到队列项时调用（对应 TS 里 `await previousTask`
    /// 之后执行的任务体）；返回是否真的启动。token 过期或没有 provider 时返回 false。
    ///
    /// 启动时记录文本/行/列快照，并把 provider 从共享槽位移入 future；`ProviderReturnGuard`
    /// 保证 future 完成**或被丢弃**时把 provider 放回槽位。取消信号通过
    /// [`CancellationToken`] 传给 provider 的 `get_suggestions`。
    fn start_autocomplete_request(&mut self, start_token: u64, options: RequestOptions) -> bool {
        if start_token != self.autocomplete_start_token || !self.has_provider() {
            return false;
        }

        let controller = CancellationToken::new();
        self.autocomplete_controller = Some(controller.clone());
        self.autocomplete_request_id += 1;
        let request_id = self.autocomplete_request_id;
        let snapshot_text = self.get_text();
        let snapshot_line = self.state.cursor_line;
        let snapshot_col = self.state.cursor_col;

        let lines = self.state.lines.clone();
        let cell = Rc::clone(&self.autocomplete_provider);
        let signal = controller.clone();
        let get_options = GetSuggestionsOptions::new(signal, options.force);
        let future: Pin<Box<dyn Future<Output = Option<AutocompleteSuggestions>>>> =
            Box::pin(async move {
                let taken = cell.borrow_mut().take();
                let mut guard = ProviderReturnGuard {
                    cell,
                    provider: taken,
                };
                let provider = guard.provider.as_mut()?;
                provider
                    .get_suggestions(&lines, snapshot_line, snapshot_col, &get_options)
                    .await
            });

        self.autocomplete_in_flight = Some(InFlightRequest {
            future,
            request_id,
            start_token,
            controller,
            snapshot_text,
            snapshot_line,
            snapshot_col,
            force: options.force,
            explicit_tab: options.explicit_tab,
        });
        true
    }

    /// pump：推进防抖计时、轮询在飞请求、按 TS 的串行化链启动排队请求。
    ///
    /// TS 把每个请求排在 `autocompleteRequestTask` 之后，任务真正运行时才检查 startToken
    /// 并调用 provider；因此同一同步批次里的多次 `handleInput` 只有最后一个请求会到达
    /// provider。Rust 没有微任务：宿主在**一批输入处理完之后**调用本方法（`render` 开头与
    /// [`Editor::tick`]）即可得到同样的合并效果。
    ///
    /// 每次 poll 使用 `Waker::noop()`：future 无法通过 waker 唤醒宿主，“继续推进”的责任
    /// 完全在宿主——重复调用本方法直到 [`Editor::has_pending_autocomplete_work`] 为 false。
    /// 循环的每一轮要么消费一个排队项、要么完成一个在飞请求，要么直接返回，不会空转。
    pub fn pump_autocomplete(&mut self) {
        loop {
            // 1. 防抖到期：定时器回调 → startAutocompleteRequest（排队）。
            if let Some(pending) = self.autocomplete_pending_debounce
                && (self.clock)() >= pending.deadline_ms
            {
                self.autocomplete_pending_debounce = None;
                // 已由 tick 抢先处理：取消定时器，避免它到点再请求一次多余的重绘。
                self.timer.cancel();
                self.autocomplete_queued = Some(QueuedRequest {
                    start_token: pending.start_token,
                    options: pending.options,
                });
            }

            // 2. 在飞请求：轮询直到就绪。未就绪时队列只能等（TS 的链同样 await 前一个
            //    promise）。
            if let Some(inflight) = self.autocomplete_in_flight.as_mut() {
                let waker = Waker::noop();
                let mut context = Context::from_waker(waker);
                match inflight.future.as_mut().poll(&mut context) {
                    Poll::Pending => return,
                    Poll::Ready(result) => {
                        let inflight = self
                            .autocomplete_in_flight
                            .take()
                            .expect("in-flight request checked above");
                        self.handle_autocomplete_result(inflight, result);
                        continue;
                    }
                }
            }

            // 3. 启动队列里的下一项；已被更新的请求淘汰（token 过期）时直接丢弃。
            let Some(queued) = self.autocomplete_queued.take() else {
                return;
            };
            if !self.start_autocomplete_request(queued.start_token, queued.options) {
                continue;
            }
        }
    }

    /// 显式 tick（宿主的定时器到期后调用）：等价于 [`Editor::pump_autocomplete`]。
    ///
    /// 宿主应重复调用直到 [`Editor::has_pending_autocomplete_work`] 返回 false；在飞 future
    /// 未就绪时调用会立即返回，就绪时则继续处理结果并可能启动下一项。
    pub fn tick(&mut self) {
        self.pump_autocomplete();
    }

    /// 是否还有待推进的补全工作（防抖中/在飞/排队中/待补判的 force 请求）。
    ///
    /// 宿主事件循环可据此决定是否调用 [`Editor::tick`] 继续轮询（TS 的 Promise 与
    /// setTimeout 由 JS 事件循环自动推进，Rust 由宿主驱动）。注意它**不**表示菜单是否
    /// 可见——`is_showing_autocomplete` 才是 UI 状态。
    pub fn has_pending_autocomplete_work(&self) -> bool {
        self.autocomplete_pending_debounce.is_some()
            || self.autocomplete_in_flight.is_some()
            || self.autocomplete_queued.is_some()
            || self.autocomplete_deferred_force.is_some()
    }

    /// 未到期的防抖截止时间（绝对毫秒）；宿主据此安排下一次唤醒。没有待触发的防抖时返回
    /// `None`（排队/在飞请求只能靠 [`Editor::tick`] 轮询，没有可计算的唤醒时刻）。
    pub fn autocomplete_deadline_ms(&self) -> Option<f64> {
        self.autocomplete_pending_debounce
            .map(|pending| pending.deadline_ms)
    }

    /// `getAutocompleteDebounceMs(options)`。
    ///
    /// `explicit_tab` 或 `force` 恒为 0（立即排队）；否则仅当光标前文本命中 debounce 正则
    /// 时返回 20ms，其余触发路径也为 0。
    fn get_autocomplete_debounce_ms(&self, options: RequestOptions) -> u64 {
        if options.explicit_tab || options.force {
            return 0;
        }

        let current_line = self
            .state
            .lines
            .get(self.state.cursor_line)
            .cloned()
            .unwrap_or_default();
        let text_before_cursor = utf16_prefix(&current_line, self.state.cursor_col);
        if self.matches_debounce_pattern(&text_before_cursor) {
            ATTACHMENT_AUTOCOMPLETE_DEBOUNCE_MS
        } else {
            0
        }
    }

    /// `runAutocompleteRequest` 的结果处理。
    ///
    /// 顺序即语义：
    /// 1. 清掉 controller（请求已结束）；
    /// 2. 若存在被推迟的 force 请求，用请求时的快照补做谓词判定——true 则丢弃本次结果并
    ///    重新排队，false 则继续处理本次结果；
    /// 3. 快照校验（取消信号/request id/文本/光标），不匹配直接丢弃；
    /// 4. 无建议或空列表 → 关闭菜单并请求重绘；
    /// 5. `force && explicit_tab` 且只有一个候选 → 直接应用，不显示菜单；
    /// 6. 其余情况创建列表并显示。
    fn handle_autocomplete_result(
        &mut self,
        inflight: InFlightRequest,
        suggestions: Option<AutocompleteSuggestions>,
    ) {
        // 1. 结果已落地：当前 controller 引用失效。
        self.autocomplete_controller = None;

        // 2. 补做被推迟的 force 谓词判定（provider 已被 guard 放回槽位）。
        if let Some(deferred) = self.autocomplete_deferred_force.take() {
            let should_trigger = self.provider_should_trigger_file_completion_at(
                &deferred.lines,
                deferred.cursor_line,
                deferred.cursor_col,
            );
            if should_trigger {
                // TS：谓词为 true → 请求时 cancelAutocompleteRequest()（abort + token++），
                // 在飞请求的结果因 abort 被丢弃，随后 ++token 并排队新请求。
                self.cancel_autocomplete_request();
                self.autocomplete_start_token += 1;
                let start_token = self.autocomplete_start_token;
                self.autocomplete_queued = Some(QueuedRequest {
                    start_token,
                    options: deferred.options,
                });
                return;
            }
            // 谓词为 false：在飞请求不受影响，继续按常规处理其结果。
        }

        // 3. 快照校验：请求期间文本/光标变过或请求已被取消，结果作废。
        if !self.is_autocomplete_request_current(&inflight) {
            return;
        }

        // 4. 没有候选：关菜单并重绘（用户可能正看着旧列表）。
        let Some(suggestions) = suggestions else {
            self.cancel_autocomplete();
            self.tui.request_render();
            return;
        };
        if suggestions.items.is_empty() {
            self.cancel_autocomplete();
            self.tui.request_render();
            return;
        }

        // 5. Tab 强制文件补全且只有唯一候选：直接替换，不弹菜单（TS 的快捷路径）。
        if inflight.force && inflight.explicit_tab && suggestions.items.len() == 1 {
            let item = suggestions.items[0].clone();
            self.push_undo_snapshot();
            self.last_action = None;
            if self.apply_completion(&item, &suggestions.prefix) {
                self.emit_change();
            }
            self.tui.request_render();
            return;
        }

        let state = if inflight.force {
            AutocompleteState::Force
        } else {
            AutocompleteState::Regular
        };
        self.apply_autocomplete_suggestions(suggestions, state);
        self.tui.request_render();
    }

    /// `isAutocompleteRequestCurrent(...)`。
    ///
    /// 同时校验四项，任一不满足都视为过期：取消信号未触发、`request_id` 仍是当前请求、
    /// 文本与发起时完全一致、光标行列与发起时一致。
    fn is_autocomplete_request_current(&self, inflight: &InFlightRequest) -> bool {
        !inflight.controller.is_cancelled()
            && inflight.request_id == self.autocomplete_request_id
            && self.get_text() == inflight.snapshot_text
            && self.state.cursor_line == inflight.snapshot_line
            && self.state.cursor_col == inflight.snapshot_col
    }

    /// `applyAutocompleteSuggestions(suggestions, state)`。
    ///
    /// 保存 provider 返回的 prefix，创建下拉列表，并用
    /// `get_best_autocomplete_match_index` 预选精确/前缀匹配项；最后把菜单状态设为
    /// `state`（`Force` 决定后续按键是否强制重新请求）。不请求重绘，由调用方负责。
    fn apply_autocomplete_suggestions(
        &mut self,
        suggestions: AutocompleteSuggestions,
        state: AutocompleteState,
    ) {
        self.autocomplete_prefix = suggestions.prefix.clone();
        self.autocomplete_list =
            Some(self.create_autocomplete_list(&suggestions.prefix, &suggestions.items));

        if let Some(best_match_index) =
            self.get_best_autocomplete_match_index(&suggestions.items, &suggestions.prefix)
            && let Some(list) = &mut self.autocomplete_list
        {
            list.set_selected_index(best_match_index as isize);
        }

        self.autocomplete_state = Some(state);
    }

    /// `cancelAutocompleteRequest()`。
    ///
    /// TS 只 abort 信号、递增 token、清防抖定时器；在飞 promise 不会被丢弃，它自己 settle
    /// 后因 abort/快照校验被丢弃。Rust 保持同一语义：future 留在 `autocomplete_in_flight`
    /// 里继续由 pump 轮询，新请求排在它之后（[`QueuedRequest`]）。需要同步取回 provider 的
    /// 路径（应用候选、谓词判定）走 [`Editor::reclaim_provider`]。
    fn cancel_autocomplete_request(&mut self) {
        self.autocomplete_start_token += 1;
        self.autocomplete_pending_debounce = None;
        self.autocomplete_queued = None;
        self.autocomplete_deferred_force = None;
        // TS 的 `clearTimeout(this.autocompleteDebounceTimer)`。
        self.timer.cancel();
        if let Some(controller) = self.autocomplete_controller.take() {
            controller.cancel();
        }
    }

    /// `clearAutocompleteUi()`。
    ///
    /// 只清菜单状态/列表/prefix，不碰在飞请求与 token；文本不受影响。
    fn clear_autocomplete_ui(&mut self) {
        self.autocomplete_state = None;
        self.autocomplete_list = None;
        self.autocomplete_prefix.clear();
    }

    /// `cancelAutocomplete()`。
    ///
    /// 取消请求（含防抖/排队/推迟项、abort 在飞请求）并清空菜单 UI；不修改文本。
    fn cancel_autocomplete(&mut self) {
        self.cancel_autocomplete_request();
        self.clear_autocomplete_ui();
    }

    /// `updateAutocomplete()`。
    ///
    /// 菜单未显示或 provider 不可用时 no-op；否则用当前文本重新 `requestAutocomplete`，
    /// `force` 沿用当前菜单状态（`Force` 菜单在继续输入时仍走强制文件补全语义）。
    fn update_autocomplete(&mut self) {
        let Some(state) = self.autocomplete_state else {
            return;
        };
        if !self.provider_available() {
            return;
        }
        self.request_autocomplete(RequestOptions {
            force: state == AutocompleteState::Force,
            explicit_tab: false,
        });
    }

    /// `setAutocompleteTriggerCharacters(triggerCharacters)`。
    ///
    /// 从默认集合 `['@', '#']` 出发**追加**，不是替换：跳过 `/`（斜杠命令有独立入口）、
    /// JS 空白、UTF-16 长度不为 1 的字符（与 TS 的 `character.length !== 1` 过滤保持一致：
    /// TS 侧会跳过非单码元字符串）以及已存在的重复项。因此即使 provider 返回空列表，
    /// `@`/`#` 仍可触发。
    fn set_autocomplete_trigger_characters(&mut self, trigger_characters: &[char]) {
        let mut next: Vec<char> = DEFAULT_AUTOCOMPLETE_TRIGGER_CHARACTERS.to_vec();
        for &character in trigger_characters {
            let mut buffer = [0u8; 4];
            let encoded = character.encode_utf8(&mut buffer);
            if character.len_utf16() != 1
                || character == '/'
                || is_whitespace_char(encoded)
                || next.contains(&character)
            {
                continue;
            }
            next.push(character);
        }
        self.autocomplete_trigger_characters = next;
    }

    /// `autocompleteTriggerPattern.test(text)`（trigger 形态，见
    /// [`autocomplete_pattern_matches`]）。
    fn matches_trigger_pattern(&self, text: &str) -> bool {
        autocomplete_pattern_matches(text, &self.autocomplete_trigger_characters, false)
    }

    /// `autocompleteDebouncePattern.test(text)`（debounce 形态：`@` 单独成支）。
    fn matches_debounce_pattern(&self, text: &str) -> bool {
        autocomplete_pattern_matches(text, &self.autocomplete_trigger_characters, true)
    }

    /// `isSlashMenuAllowed()`：斜杠菜单只允许出现在第一行。
    ///
    /// 斜杠命令必须位于消息开头，因此后续行即使以 `/` 开头也不会弹出菜单。
    fn is_slash_menu_allowed(&self) -> bool {
        self.state.cursor_line == 0
    }

    /// `isAtStartOfMessage()`。
    ///
    /// 第一行且光标前（按 JS 空白 trim）为空或恰好是 `/`。`insert_character` 用它在输入
    /// 第一个 `/` 时立即触发斜杠菜单。
    fn is_at_start_of_message(&self) -> bool {
        if !self.is_slash_menu_allowed() {
            return false;
        }
        let current_line = self
            .state
            .lines
            .get(self.state.cursor_line)
            .cloned()
            .unwrap_or_default();
        let before_cursor = utf16_prefix(&current_line, self.state.cursor_col);
        let trimmed = trim_js(&before_cursor);
        trimmed.is_empty() || trimmed == "/"
    }

    /// `isInSlashCommandContext(textBeforeCursor)`。
    ///
    /// 第一行、且光标前文本 trim 掉前导空白后以 `/` 开头（不要求后面没有空格，参数补全也
    /// 属于该上下文）。
    fn is_in_slash_command_context(&self, text_before_cursor: &str) -> bool {
        self.is_slash_menu_allowed() && trim_start_js(text_before_cursor).starts_with('/')
    }

    // ------------------------------------------------------------------
    // 分段（marker 感知）
    // ------------------------------------------------------------------

    /// `validPasteIds()`。
    fn valid_paste_ids(&self) -> Vec<u64> {
        self.pastes.keys().copied().collect()
    }

    /// `segment(text, "grapheme")`：marker 感知的字素段。
    ///
    /// 有效 marker（id 在 `pastes` 里）覆盖的区间会合并成一个原子段，供光标移动/删除/换行
    /// 使用；`index` 是 UTF-16 偏移。文本里没有有效 marker 时直接返回底层字素分段。
    fn segment_graphemes(&self, text: &str) -> Vec<TextSegment> {
        let valid_ids = self.valid_paste_ids();
        merge_markers(text, &valid_ids, || {
            get_grapheme_segmenter()
                .segment(text)
                .map(|segment| (segment.segment.to_string(), segment.index))
                .collect()
        })
    }

    /// `segment(text, "word")`：marker 感知的词段（供
    /// [`find_word_backward`]/[`find_word_forward`] 使用）。
    ///
    /// 有效 marker 合并成 `is_word_like: false` 的单个段，`find_word_*` 通过
    /// `is_atomic_segment` 把它整体跳过；其它段保留 `get_word_segments` 的结果。
    fn segment_word_segments(&self, text: &str) -> Vec<WordSegment> {
        let valid_ids = self.valid_paste_ids();
        let base = get_word_segments(text);
        if valid_ids.is_empty() || !text.contains("[paste #") {
            return base;
        }
        let markers = marker_spans(text, &valid_ids);
        if markers.is_empty() {
            return base;
        }

        let mut result: Vec<WordSegment> = Vec::with_capacity(base.len());
        let mut marker_index = 0usize;
        for segment in base {
            while marker_index < markers.len() && markers[marker_index].1 <= segment.index {
                marker_index += 1;
            }
            let marker = markers.get(marker_index).copied();
            if let Some((start, end)) = marker
                && segment.index >= start
                && segment.index < end
            {
                if segment.index == start {
                    result.push(WordSegment {
                        segment: utf16_slice(text, start, end),
                        index: start,
                        input: text.to_string(),
                        is_word_like: false,
                    });
                }
            } else {
                result.push(segment);
            }
        }
        result
    }
}

/// 找出文本里所有 id ∈ `valid_ids` 的 marker 区间（UTF-16 偏移）。
///
/// 返回按起点升序、互不重叠的 `(start, end)`（半开区间）；`end` 是 marker 最后一个码元
/// 之后。id 不在 `valid_ids` 里的匹配被跳过，因此手打的假 marker 不会变成原子段。
fn marker_spans(text: &str, valid_ids: &[u64]) -> Vec<(usize, usize)> {
    let mut markers: Vec<(usize, usize)> = Vec::new();
    for captures in PASTE_MARKER_REGEX.captures_iter(text) {
        let Some(whole) = captures.get(0) else {
            continue;
        };
        let Some(id) = captures.get(1) else {
            continue;
        };
        let Ok(id) = id.as_str().parse::<u64>() else {
            continue;
        };
        if !valid_ids.contains(&id) {
            continue;
        }
        let start = utf16_len(&text[..whole.start()]);
        let end = start + utf16_len(whole.as_str());
        markers.push((start, end));
    }
    markers
}

// ============================================================================
// trait 实现
// ============================================================================

impl Component for Editor {
    fn render(&mut self, width: usize) -> Vec<String> {
        self.render_editor(width)
    }

    fn handle_input(&mut self, data: &str) {
        self.handle_input_data(data);
    }

    fn handle_mouse(&mut self, event: &TuiMouseEvent) -> Option<TuiMouseEventResult> {
        self.handle_mouse_event(event)
    }

    fn invalidate(&mut self) {
        // 编辑器没有渲染缓存。
    }

    /// TS 的 `class Editor implements Component, Focusable`：`isFocusable()` 依据
    /// `"focused" in component` 判定为真，因此点击编辑器可以把焦点交给它。
    fn as_focusable(&mut self) -> Option<&mut dyn Focusable> {
        Some(self)
    }
}

impl Focusable for Editor {
    fn focused(&self) -> bool {
        self.focused
    }

    fn set_focused(&mut self, focused: bool) {
        // 焦点只影响渲染时是否写 CURSOR_MARKER；键盘事件的路由由宿主完成，本实现不转发。
        self.focused = focused;
    }
}

// ============================================================================
// EditorComponent（TS 里 `class Editor implements Component, Focusable` 结构化满足
// `editor-component.ts` 的 `EditorComponent`；下游可把内置编辑器直接当接口用）
// ============================================================================

impl EditorComponent for Editor {
    fn get_text(&self) -> String {
        Editor::get_text(self)
    }

    fn set_text(&mut self, text: &str) {
        Editor::set_text(self, text);
    }

    fn set_on_submit(&mut self, callback: Option<EditorTextCallback>) {
        self.on_submit = callback;
    }

    fn set_on_change(&mut self, callback: Option<EditorTextCallback>) {
        self.on_change = callback;
    }

    fn add_to_history(&mut self, text: &str) {
        Editor::add_to_history(self, text);
    }

    fn insert_text_at_cursor(&mut self, text: &str) {
        Editor::insert_text_at_cursor(self, text);
    }

    fn get_expanded_text(&self) -> String {
        Editor::get_expanded_text(self)
    }

    fn set_padding_x(&mut self, padding: usize) {
        Editor::set_padding_x(self, padding as f64);
    }

    fn set_autocomplete_max_visible(&mut self, max_visible: usize) {
        Editor::set_autocomplete_max_visible(self, max_visible as f64);
    }

    fn set_autocomplete_provider(&mut self, provider: AutocompleteProviderObject) {
        // 载荷约定为 `Box<dyn AutocompleteProvider>`（见
        // [`crate::editor_component::AutocompleteProviderObject`]）；无法识别时忽略。
        // 注意：具体类型上的调用会命中同名 inherent 方法（接受 `Box<dyn AutocompleteProvider>`）。
        if let Ok(provider) = provider.downcast::<Box<dyn AutocompleteProvider>>() {
            Editor::set_autocomplete_provider(self, *provider);
        }
    }

    fn set_border_color(&mut self, color: Option<EditorBorderColor>) {
        // `None` 表示不设置颜色：恢复恒等着色（不加 ANSI）。
        self.border_color = match color {
            Some(color) => Arc::from(color),
            None => Arc::new(|text: &str| text.to_string()),
        };
    }

    fn border_color(&self, text: &str) -> String {
        (self.border_color)(text)
    }
}

// ============================================================================
// 单元测试：自动补全触发/防抖正则 + paste marker 重编号
// ============================================================================
//
// 这里只放“纯函数 vs TS 行为”的锁定测试；编辑器交互（换行、撤销、补全时序、鼠标）在
// `tests/editor.rs` 与 `tests/mouse_components.rs`。金表与 fixture 的期望值都来自 TS 参考
// 实现，**不要手改期望值来迁就 Rust 实现**——那会把移植偏差固化成规范；应当先确认 TS
// 的真实输出，再改实现。

#[cfg(test)]
mod tests {
    use super::*;

    /// `(text, trigger_chars, debounce, expected)`。
    ///
    /// 期望值由 TS 参考实现的 `buildTriggerPattern` / `buildDebouncePattern`
    /// （`packages/tui/src/components/editor.ts`）逐例求值生成，覆盖括号前缀的回溯、分隔符（空白 / CJK 标点）、引号包裹路径与 `@` 分支。
    /// 这是一张大金表，只用于全表比对，不逐项注释；新增用例应从 TS 引擎求值后追加。
    const TRIGGER_PATTERN_GOLDEN: &[(&str, &[char], bool, bool)] = &[
        ("", &['@', '#'], false, false),
        ("", &['@', '#'], true, false),
        ("(", &['@', '#'], false, false),
        ("(", &['@', '#'], true, false),
        ("((", &['@', '#'], false, false),
        ("((", &['@', '#'], true, false),
        ("([{<`", &['@', '#'], false, false),
        ("([{<`", &['@', '#'], true, false),
        (")", &['@', '#'], false, false),
        (")", &['@', '#'], true, false),
        ("x(", &['@', '#'], false, false),
        ("x(", &['@', '#'], true, false),
        ("x (", &['@', '#'], false, false),
        ("x (", &['@', '#'], true, false),
        ("x，(", &['@', '#'], false, false),
        ("x，(", &['@', '#'], true, false),
        ("x ((", &['@', '#'], false, false),
        ("x ((", &['@', '#'], true, false),
        ("(((", &['@', '#'], false, false),
        ("(((", &['@', '#'], true, false),
        ("(", &['@', '#'], false, false),
        ("(", &['@', '#'], true, false),
        ("( @", &['@', '#'], false, true),
        ("( @", &['@', '#'], true, true),
        ("@", &['@', '#'], false, true),
        ("@", &['@', '#'], true, true),
        ("a@", &['@', '#'], false, false),
        ("a@", &['@', '#'], true, false),
        ("x @", &['@', '#'], false, true),
        ("x @", &['@', '#'], true, true),
        ("x@", &['@', '#'], false, false),
        ("x@", &['@', '#'], true, false),
        ("@a", &['@', '#'], false, true),
        ("@a", &['@', '#'], true, true),
        ("@a b", &['@', '#'], false, false),
        ("@a b", &['@', '#'], true, false),
        ("@\"a b", &['@', '#'], false, true),
        ("@\"a b", &['@', '#'], true, true),
        ("@\"a b\"", &['@', '#'], false, false),
        ("@\"a b\"", &['@', '#'], true, false),
        ("@\"a b", &['@', '#'], false, true),
        ("@\"a b", &['@', '#'], true, true),
        ("(\"", &['@', '#'], false, false),
        ("(\"", &['@', '#'], true, false),
        ("#", &['@', '#'], false, true),
        ("#", &['@', '#'], true, true),
        ("x #", &['@', '#'], false, true),
        ("x #", &['@', '#'], true, true),
        ("#ha", &['@', '#'], false, true),
        ("#ha", &['@', '#'], true, true),
        ("#ha sh", &['@', '#'], false, false),
        ("#ha sh", &['@', '#'], true, false),
        ("x，", &['@', '#'], false, false),
        ("x，", &['@', '#'], true, false),
        ("x，@", &['@', '#'], false, true),
        ("x，@", &['@', '#'], true, true),
        ("x，[", &['@', '#'], false, false),
        ("x，[", &['@', '#'], true, false),
        ("<`", &['@', '#'], false, false),
        ("<`", &['@', '#'], true, false),
        ("`<", &['@', '#'], false, false),
        ("`<", &['@', '#'], true, false),
        ("([{<`@", &['@', '#'], false, true),
        ("([{<`@", &['@', '#'], true, true),
        ("foo (bar", &['@', '#'], false, false),
        ("foo (bar", &['@', '#'], true, false),
        ("foo(bar", &['@', '#'], false, false),
        ("foo(bar", &['@', '#'], true, false),
        ("/cmd ", &['@', '#'], false, false),
        ("/cmd ", &['@', '#'], true, false),
        (" /cmd @", &['@', '#'], false, true),
        (" /cmd @", &['@', '#'], true, true),
        ("　@", &['@', '#'], false, true),
        ("　@", &['@', '#'], true, true),
        ("　(", &['@', '#'], false, false),
        ("　(", &['@', '#'], true, false),
        ("a\t(", &['@', '#'], false, false),
        ("a\t(", &['@', '#'], true, false),
        ("a\n(", &['@', '#'], false, false),
        ("a\n(", &['@', '#'], true, false),
        // 触发字符本身是分隔符（CJK 标点）：`suffix` 为空时不能把触发字符自己算成后缀里的
        // 分隔符。以下期望值来自 TS 引擎（`setAutocompleteTriggerCharacters(["，"])` 后的
        // `autocompleteTriggerPattern` / `autocompleteDebouncePattern`）。
        ("，", &['@', '#', '，'], false, true),
        ("，", &['@', '#', '，'], true, true),
        ("a ，", &['@', '#', '，'], false, true),
        ("a ，", &['@', '#', '，'], true, true),
        ("x，", &['@', '#', '，'], false, false),
        ("x，", &['@', '#', '，'], true, false),
        ("，(", &['@', '#', '，'], false, true),
        ("，(", &['@', '#', '，'], true, true),
        ("a，(", &['@', '#', '，'], false, false),
        ("a，(", &['@', '#', '，'], true, false),
        (" ，", &['@', '#', '，'], false, true),
        (" ，", &['@', '#', '，'], true, true),
        ("，，", &['@', '#', '，'], false, true),
        ("，，", &['@', '#', '，'], true, true),
        ("(，", &['@', '#', '，'], false, true),
        ("(，", &['@', '#', '，'], true, true),
        ("，@", &['@', '#', '，'], false, true),
        ("，@", &['@', '#', '，'], true, true),
        ("@，", &['@', '#', '，'], false, false),
        ("@，", &['@', '#', '，'], true, false),
        ("，a", &['@', '#', '，'], false, true),
        ("，a", &['@', '#', '，'], true, true),
        ("a，a", &['@', '#', '，'], false, false),
        ("a，a", &['@', '#', '，'], true, false),
        ("", &['('], false, false),
        ("", &['('], true, false),
        ("(", &['('], false, true),
        ("(", &['('], true, true),
        ("((", &['('], false, true),
        ("((", &['('], true, true),
        ("([{<`", &['('], false, true),
        ("([{<`", &['('], true, true),
        (")", &['('], false, false),
        (")", &['('], true, false),
        ("x(", &['('], false, false),
        ("x(", &['('], true, false),
        ("x (", &['('], false, true),
        ("x (", &['('], true, true),
        ("x，(", &['('], false, true),
        ("x，(", &['('], true, true),
        ("x ((", &['('], false, true),
        ("x ((", &['('], true, true),
        ("(((", &['('], false, true),
        ("(((", &['('], true, true),
        ("(", &['('], false, true),
        ("(", &['('], true, true),
        ("( @", &['('], false, false),
        ("( @", &['('], true, true),
        ("@", &['('], false, false),
        ("@", &['('], true, true),
        ("a@", &['('], false, false),
        ("a@", &['('], true, false),
        ("x @", &['('], false, false),
        ("x @", &['('], true, true),
        ("x@", &['('], false, false),
        ("x@", &['('], true, false),
        ("@a", &['('], false, false),
        ("@a", &['('], true, true),
        ("@a b", &['('], false, false),
        ("@a b", &['('], true, false),
        ("@\"a b", &['('], false, true),
        ("@\"a b", &['('], true, true),
        ("@\"a b\"", &['('], false, false),
        ("@\"a b\"", &['('], true, false),
        ("@\"a b", &['('], false, true),
        ("@\"a b", &['('], true, true),
        ("(\"", &['('], false, true),
        ("(\"", &['('], true, true),
        ("#", &['('], false, false),
        ("#", &['('], true, false),
        ("x #", &['('], false, false),
        ("x #", &['('], true, false),
        ("#ha", &['('], false, false),
        ("#ha", &['('], true, false),
        ("#ha sh", &['('], false, false),
        ("#ha sh", &['('], true, false),
        ("x，", &['('], false, false),
        ("x，", &['('], true, false),
        ("x，@", &['('], false, false),
        ("x，@", &['('], true, true),
        ("x，[", &['('], false, false),
        ("x，[", &['('], true, false),
        ("<`", &['('], false, false),
        ("<`", &['('], true, false),
        ("`<", &['('], false, false),
        ("`<", &['('], true, false),
        ("([{<`@", &['('], false, true),
        ("([{<`@", &['('], true, true),
        ("foo (bar", &['('], false, true),
        ("foo (bar", &['('], true, true),
        ("foo(bar", &['('], false, false),
        ("foo(bar", &['('], true, false),
        ("/cmd ", &['('], false, false),
        ("/cmd ", &['('], true, false),
        (" /cmd @", &['('], false, false),
        (" /cmd @", &['('], true, true),
        ("　@", &['('], false, false),
        ("　@", &['('], true, true),
        ("　(", &['('], false, true),
        ("　(", &['('], true, true),
        ("a\t(", &['('], false, true),
        ("a\t(", &['('], true, true),
        ("a\n(", &['('], false, true),
        ("a\n(", &['('], true, true),
        ("", &['[', '@'], false, false),
        ("", &['[', '@'], true, false),
        ("(", &['[', '@'], false, false),
        ("(", &['[', '@'], true, false),
        ("((", &['[', '@'], false, false),
        ("((", &['[', '@'], true, false),
        ("([{<`", &['[', '@'], false, true),
        ("([{<`", &['[', '@'], true, true),
        (")", &['[', '@'], false, false),
        (")", &['[', '@'], true, false),
        ("x(", &['[', '@'], false, false),
        ("x(", &['[', '@'], true, false),
        ("x (", &['[', '@'], false, false),
        ("x (", &['[', '@'], true, false),
        ("x，(", &['[', '@'], false, false),
        ("x，(", &['[', '@'], true, false),
        ("x ((", &['[', '@'], false, false),
        ("x ((", &['[', '@'], true, false),
        ("(((", &['[', '@'], false, false),
        ("(((", &['[', '@'], true, false),
        ("(", &['[', '@'], false, false),
        ("(", &['[', '@'], true, false),
        ("( @", &['[', '@'], false, true),
        ("( @", &['[', '@'], true, true),
        ("@", &['[', '@'], false, true),
        ("@", &['[', '@'], true, true),
        ("a@", &['[', '@'], false, false),
        ("a@", &['[', '@'], true, false),
        ("x @", &['[', '@'], false, true),
        ("x @", &['[', '@'], true, true),
        ("x@", &['[', '@'], false, false),
        ("x@", &['[', '@'], true, false),
        ("@a", &['[', '@'], false, true),
        ("@a", &['[', '@'], true, true),
        ("@a b", &['[', '@'], false, false),
        ("@a b", &['[', '@'], true, false),
        ("@\"a b", &['[', '@'], false, true),
        ("@\"a b", &['[', '@'], true, true),
        ("@\"a b\"", &['[', '@'], false, false),
        ("@\"a b\"", &['[', '@'], true, false),
        ("@\"a b", &['[', '@'], false, true),
        ("@\"a b", &['[', '@'], true, true),
        ("(\"", &['[', '@'], false, false),
        ("(\"", &['[', '@'], true, false),
        ("#", &['[', '@'], false, false),
        ("#", &['[', '@'], true, false),
        ("x #", &['[', '@'], false, false),
        ("x #", &['[', '@'], true, false),
        ("#ha", &['[', '@'], false, false),
        ("#ha", &['[', '@'], true, false),
        ("#ha sh", &['[', '@'], false, false),
        ("#ha sh", &['[', '@'], true, false),
        ("x，", &['[', '@'], false, false),
        ("x，", &['[', '@'], true, false),
        ("x，@", &['[', '@'], false, true),
        ("x，@", &['[', '@'], true, true),
        ("x，[", &['[', '@'], false, true),
        ("x，[", &['[', '@'], true, true),
        ("<`", &['[', '@'], false, false),
        ("<`", &['[', '@'], true, false),
        ("`<", &['[', '@'], false, false),
        ("`<", &['[', '@'], true, false),
        ("([{<`@", &['[', '@'], false, true),
        ("([{<`@", &['[', '@'], true, true),
        ("foo (bar", &['[', '@'], false, false),
        ("foo (bar", &['[', '@'], true, false),
        ("foo(bar", &['[', '@'], false, false),
        ("foo(bar", &['[', '@'], true, false),
        ("/cmd ", &['[', '@'], false, false),
        ("/cmd ", &['[', '@'], true, false),
        (" /cmd @", &['[', '@'], false, true),
        (" /cmd @", &['[', '@'], true, true),
        ("　@", &['[', '@'], false, true),
        ("　@", &['[', '@'], true, true),
        ("　(", &['[', '@'], false, false),
        ("　(", &['[', '@'], true, false),
        ("a\t(", &['[', '@'], false, false),
        ("a\t(", &['[', '@'], true, false),
        ("a\n(", &['[', '@'], false, false),
        ("a\n(", &['[', '@'], true, false),
        ("", &['{', '<', '#'], false, false),
        ("", &['{', '<', '#'], true, false),
        ("(", &['{', '<', '#'], false, false),
        ("(", &['{', '<', '#'], true, false),
        ("((", &['{', '<', '#'], false, false),
        ("((", &['{', '<', '#'], true, false),
        ("([{<`", &['{', '<', '#'], false, true),
        ("([{<`", &['{', '<', '#'], true, true),
        (")", &['{', '<', '#'], false, false),
        (")", &['{', '<', '#'], true, false),
        ("x(", &['{', '<', '#'], false, false),
        ("x(", &['{', '<', '#'], true, false),
        ("x (", &['{', '<', '#'], false, false),
        ("x (", &['{', '<', '#'], true, false),
        ("x，(", &['{', '<', '#'], false, false),
        ("x，(", &['{', '<', '#'], true, false),
        ("x ((", &['{', '<', '#'], false, false),
        ("x ((", &['{', '<', '#'], true, false),
        ("(((", &['{', '<', '#'], false, false),
        ("(((", &['{', '<', '#'], true, false),
        ("(", &['{', '<', '#'], false, false),
        ("(", &['{', '<', '#'], true, false),
        ("( @", &['{', '<', '#'], false, false),
        ("( @", &['{', '<', '#'], true, true),
        ("@", &['{', '<', '#'], false, false),
        ("@", &['{', '<', '#'], true, true),
        ("a@", &['{', '<', '#'], false, false),
        ("a@", &['{', '<', '#'], true, false),
        ("x @", &['{', '<', '#'], false, false),
        ("x @", &['{', '<', '#'], true, true),
        ("x@", &['{', '<', '#'], false, false),
        ("x@", &['{', '<', '#'], true, false),
        ("@a", &['{', '<', '#'], false, false),
        ("@a", &['{', '<', '#'], true, true),
        ("@a b", &['{', '<', '#'], false, false),
        ("@a b", &['{', '<', '#'], true, false),
        ("@\"a b", &['{', '<', '#'], false, true),
        ("@\"a b", &['{', '<', '#'], true, true),
        ("@\"a b\"", &['{', '<', '#'], false, false),
        ("@\"a b\"", &['{', '<', '#'], true, false),
        ("@\"a b", &['{', '<', '#'], false, true),
        ("@\"a b", &['{', '<', '#'], true, true),
        ("(\"", &['{', '<', '#'], false, false),
        ("(\"", &['{', '<', '#'], true, false),
        ("#", &['{', '<', '#'], false, true),
        ("#", &['{', '<', '#'], true, true),
        ("x #", &['{', '<', '#'], false, true),
        ("x #", &['{', '<', '#'], true, true),
        ("#ha", &['{', '<', '#'], false, true),
        ("#ha", &['{', '<', '#'], true, true),
        ("#ha sh", &['{', '<', '#'], false, false),
        ("#ha sh", &['{', '<', '#'], true, false),
        ("x，", &['{', '<', '#'], false, false),
        ("x，", &['{', '<', '#'], true, false),
        ("x，@", &['{', '<', '#'], false, false),
        ("x，@", &['{', '<', '#'], true, true),
        ("x，[", &['{', '<', '#'], false, false),
        ("x，[", &['{', '<', '#'], true, false),
        ("<`", &['{', '<', '#'], false, true),
        ("<`", &['{', '<', '#'], true, true),
        ("`<", &['{', '<', '#'], false, true),
        ("`<", &['{', '<', '#'], true, true),
        ("([{<`@", &['{', '<', '#'], false, true),
        ("([{<`@", &['{', '<', '#'], true, true),
        ("foo (bar", &['{', '<', '#'], false, false),
        ("foo (bar", &['{', '<', '#'], true, false),
        ("foo(bar", &['{', '<', '#'], false, false),
        ("foo(bar", &['{', '<', '#'], true, false),
        ("/cmd ", &['{', '<', '#'], false, false),
        ("/cmd ", &['{', '<', '#'], true, false),
        (" /cmd @", &['{', '<', '#'], false, false),
        (" /cmd @", &['{', '<', '#'], true, true),
        ("　@", &['{', '<', '#'], false, false),
        ("　@", &['{', '<', '#'], true, true),
        ("　(", &['{', '<', '#'], false, false),
        ("　(", &['{', '<', '#'], true, false),
        ("a\t(", &['{', '<', '#'], false, false),
        ("a\t(", &['{', '<', '#'], true, false),
        ("a\n(", &['{', '<', '#'], false, false),
        ("a\n(", &['{', '<', '#'], true, false),
        ("", &['`'], false, false),
        ("", &['`'], true, false),
        ("(", &['`'], false, false),
        ("(", &['`'], true, false),
        ("((", &['`'], false, false),
        ("((", &['`'], true, false),
        ("([{<`", &['`'], false, true),
        ("([{<`", &['`'], true, true),
        (")", &['`'], false, false),
        (")", &['`'], true, false),
        ("x(", &['`'], false, false),
        ("x(", &['`'], true, false),
        ("x (", &['`'], false, false),
        ("x (", &['`'], true, false),
        ("x，(", &['`'], false, false),
        ("x，(", &['`'], true, false),
        ("x ((", &['`'], false, false),
        ("x ((", &['`'], true, false),
        ("(((", &['`'], false, false),
        ("(((", &['`'], true, false),
        ("(", &['`'], false, false),
        ("(", &['`'], true, false),
        ("( @", &['`'], false, false),
        ("( @", &['`'], true, true),
        ("@", &['`'], false, false),
        ("@", &['`'], true, true),
        ("a@", &['`'], false, false),
        ("a@", &['`'], true, false),
        ("x @", &['`'], false, false),
        ("x @", &['`'], true, true),
        ("x@", &['`'], false, false),
        ("x@", &['`'], true, false),
        ("@a", &['`'], false, false),
        ("@a", &['`'], true, true),
        ("@a b", &['`'], false, false),
        ("@a b", &['`'], true, false),
        ("@\"a b", &['`'], false, true),
        ("@\"a b", &['`'], true, true),
        ("@\"a b\"", &['`'], false, false),
        ("@\"a b\"", &['`'], true, false),
        ("@\"a b", &['`'], false, true),
        ("@\"a b", &['`'], true, true),
        ("(\"", &['`'], false, false),
        ("(\"", &['`'], true, false),
        ("#", &['`'], false, false),
        ("#", &['`'], true, false),
        ("x #", &['`'], false, false),
        ("x #", &['`'], true, false),
        ("#ha", &['`'], false, false),
        ("#ha", &['`'], true, false),
        ("#ha sh", &['`'], false, false),
        ("#ha sh", &['`'], true, false),
        ("x，", &['`'], false, false),
        ("x，", &['`'], true, false),
        ("x，@", &['`'], false, false),
        ("x，@", &['`'], true, true),
        ("x，[", &['`'], false, false),
        ("x，[", &['`'], true, false),
        ("<`", &['`'], false, true),
        ("<`", &['`'], true, true),
        ("`<", &['`'], false, true),
        ("`<", &['`'], true, true),
        ("([{<`@", &['`'], false, true),
        ("([{<`@", &['`'], true, true),
        ("foo (bar", &['`'], false, false),
        ("foo (bar", &['`'], true, false),
        ("foo(bar", &['`'], false, false),
        ("foo(bar", &['`'], true, false),
        ("/cmd ", &['`'], false, false),
        ("/cmd ", &['`'], true, false),
        (" /cmd @", &['`'], false, false),
        (" /cmd @", &['`'], true, true),
        ("　@", &['`'], false, false),
        ("　@", &['`'], true, true),
        ("　(", &['`'], false, false),
        ("　(", &['`'], true, false),
        ("a\t(", &['`'], false, false),
        ("a\t(", &['`'], true, false),
        ("a\n(", &['`'], false, false),
        ("a\n(", &['`'], true, false),
        ("", &['(', '[', '{', '<', '`', '@', '#'], false, false),
        ("", &['(', '[', '{', '<', '`', '@', '#'], true, false),
        ("(", &['(', '[', '{', '<', '`', '@', '#'], false, true),
        ("(", &['(', '[', '{', '<', '`', '@', '#'], true, true),
        ("((", &['(', '[', '{', '<', '`', '@', '#'], false, true),
        ("((", &['(', '[', '{', '<', '`', '@', '#'], true, true),
        ("([{<`", &['(', '[', '{', '<', '`', '@', '#'], false, true),
        ("([{<`", &['(', '[', '{', '<', '`', '@', '#'], true, true),
        (")", &['(', '[', '{', '<', '`', '@', '#'], false, false),
        (")", &['(', '[', '{', '<', '`', '@', '#'], true, false),
        ("x(", &['(', '[', '{', '<', '`', '@', '#'], false, false),
        ("x(", &['(', '[', '{', '<', '`', '@', '#'], true, false),
        ("x (", &['(', '[', '{', '<', '`', '@', '#'], false, true),
        ("x (", &['(', '[', '{', '<', '`', '@', '#'], true, true),
        ("x，(", &['(', '[', '{', '<', '`', '@', '#'], false, true),
        ("x，(", &['(', '[', '{', '<', '`', '@', '#'], true, true),
        ("x ((", &['(', '[', '{', '<', '`', '@', '#'], false, true),
        ("x ((", &['(', '[', '{', '<', '`', '@', '#'], true, true),
        ("(((", &['(', '[', '{', '<', '`', '@', '#'], false, true),
        ("(((", &['(', '[', '{', '<', '`', '@', '#'], true, true),
        ("(", &['(', '[', '{', '<', '`', '@', '#'], false, true),
        ("(", &['(', '[', '{', '<', '`', '@', '#'], true, true),
        ("( @", &['(', '[', '{', '<', '`', '@', '#'], false, true),
        ("( @", &['(', '[', '{', '<', '`', '@', '#'], true, true),
        ("@", &['(', '[', '{', '<', '`', '@', '#'], false, true),
        ("@", &['(', '[', '{', '<', '`', '@', '#'], true, true),
        ("a@", &['(', '[', '{', '<', '`', '@', '#'], false, false),
        ("a@", &['(', '[', '{', '<', '`', '@', '#'], true, false),
        ("x @", &['(', '[', '{', '<', '`', '@', '#'], false, true),
        ("x @", &['(', '[', '{', '<', '`', '@', '#'], true, true),
        ("x@", &['(', '[', '{', '<', '`', '@', '#'], false, false),
        ("x@", &['(', '[', '{', '<', '`', '@', '#'], true, false),
        ("@a", &['(', '[', '{', '<', '`', '@', '#'], false, true),
        ("@a", &['(', '[', '{', '<', '`', '@', '#'], true, true),
        ("@a b", &['(', '[', '{', '<', '`', '@', '#'], false, false),
        ("@a b", &['(', '[', '{', '<', '`', '@', '#'], true, false),
        ("@\"a b", &['(', '[', '{', '<', '`', '@', '#'], false, true),
        ("@\"a b", &['(', '[', '{', '<', '`', '@', '#'], true, true),
        (
            "@\"a b\"",
            &['(', '[', '{', '<', '`', '@', '#'],
            false,
            false,
        ),
        (
            "@\"a b\"",
            &['(', '[', '{', '<', '`', '@', '#'],
            true,
            false,
        ),
        ("@\"a b", &['(', '[', '{', '<', '`', '@', '#'], false, true),
        ("@\"a b", &['(', '[', '{', '<', '`', '@', '#'], true, true),
        ("(\"", &['(', '[', '{', '<', '`', '@', '#'], false, true),
        ("(\"", &['(', '[', '{', '<', '`', '@', '#'], true, true),
        ("#", &['(', '[', '{', '<', '`', '@', '#'], false, true),
        ("#", &['(', '[', '{', '<', '`', '@', '#'], true, true),
        ("x #", &['(', '[', '{', '<', '`', '@', '#'], false, true),
        ("x #", &['(', '[', '{', '<', '`', '@', '#'], true, true),
        ("#ha", &['(', '[', '{', '<', '`', '@', '#'], false, true),
        ("#ha", &['(', '[', '{', '<', '`', '@', '#'], true, true),
        ("#ha sh", &['(', '[', '{', '<', '`', '@', '#'], false, false),
        ("#ha sh", &['(', '[', '{', '<', '`', '@', '#'], true, false),
        ("x，", &['(', '[', '{', '<', '`', '@', '#'], false, false),
        ("x，", &['(', '[', '{', '<', '`', '@', '#'], true, false),
        ("x，@", &['(', '[', '{', '<', '`', '@', '#'], false, true),
        ("x，@", &['(', '[', '{', '<', '`', '@', '#'], true, true),
        ("x，[", &['(', '[', '{', '<', '`', '@', '#'], false, true),
        ("x，[", &['(', '[', '{', '<', '`', '@', '#'], true, true),
        ("<`", &['(', '[', '{', '<', '`', '@', '#'], false, true),
        ("<`", &['(', '[', '{', '<', '`', '@', '#'], true, true),
        ("`<", &['(', '[', '{', '<', '`', '@', '#'], false, true),
        ("`<", &['(', '[', '{', '<', '`', '@', '#'], true, true),
        ("([{<`@", &['(', '[', '{', '<', '`', '@', '#'], false, true),
        ("([{<`@", &['(', '[', '{', '<', '`', '@', '#'], true, true),
        (
            "foo (bar",
            &['(', '[', '{', '<', '`', '@', '#'],
            false,
            true,
        ),
        ("foo (bar", &['(', '[', '{', '<', '`', '@', '#'], true, true),
        (
            "foo(bar",
            &['(', '[', '{', '<', '`', '@', '#'],
            false,
            false,
        ),
        ("foo(bar", &['(', '[', '{', '<', '`', '@', '#'], true, false),
        ("/cmd ", &['(', '[', '{', '<', '`', '@', '#'], false, false),
        ("/cmd ", &['(', '[', '{', '<', '`', '@', '#'], true, false),
        (" /cmd @", &['(', '[', '{', '<', '`', '@', '#'], false, true),
        (" /cmd @", &['(', '[', '{', '<', '`', '@', '#'], true, true),
        ("　@", &['(', '[', '{', '<', '`', '@', '#'], false, true),
        ("　@", &['(', '[', '{', '<', '`', '@', '#'], true, true),
        ("　(", &['(', '[', '{', '<', '`', '@', '#'], false, true),
        ("　(", &['(', '[', '{', '<', '`', '@', '#'], true, true),
        ("a\t(", &['(', '[', '{', '<', '`', '@', '#'], false, true),
        ("a\t(", &['(', '[', '{', '<', '`', '@', '#'], true, true),
        ("a\n(", &['(', '[', '{', '<', '`', '@', '#'], false, true),
        ("a\n(", &['(', '[', '{', '<', '`', '@', '#'], true, true),
        ("", &['$'], false, false),
        ("", &['$'], true, false),
        ("(", &['$'], false, false),
        ("(", &['$'], true, false),
        ("((", &['$'], false, false),
        ("((", &['$'], true, false),
        ("([{<`", &['$'], false, false),
        ("([{<`", &['$'], true, false),
        (")", &['$'], false, false),
        (")", &['$'], true, false),
        ("x(", &['$'], false, false),
        ("x(", &['$'], true, false),
        ("x (", &['$'], false, false),
        ("x (", &['$'], true, false),
        ("x，(", &['$'], false, false),
        ("x，(", &['$'], true, false),
        ("x ((", &['$'], false, false),
        ("x ((", &['$'], true, false),
        ("(((", &['$'], false, false),
        ("(((", &['$'], true, false),
        ("(", &['$'], false, false),
        ("(", &['$'], true, false),
        ("( @", &['$'], false, false),
        ("( @", &['$'], true, true),
        ("@", &['$'], false, false),
        ("@", &['$'], true, true),
        ("a@", &['$'], false, false),
        ("a@", &['$'], true, false),
        ("x @", &['$'], false, false),
        ("x @", &['$'], true, true),
        ("x@", &['$'], false, false),
        ("x@", &['$'], true, false),
        ("@a", &['$'], false, false),
        ("@a", &['$'], true, true),
        ("@a b", &['$'], false, false),
        ("@a b", &['$'], true, false),
        ("@\"a b", &['$'], false, true),
        ("@\"a b", &['$'], true, true),
        ("@\"a b\"", &['$'], false, false),
        ("@\"a b\"", &['$'], true, false),
        ("@\"a b", &['$'], false, true),
        ("@\"a b", &['$'], true, true),
        ("(\"", &['$'], false, false),
        ("(\"", &['$'], true, false),
        ("#", &['$'], false, false),
        ("#", &['$'], true, false),
        ("x #", &['$'], false, false),
        ("x #", &['$'], true, false),
        ("#ha", &['$'], false, false),
        ("#ha", &['$'], true, false),
        ("#ha sh", &['$'], false, false),
        ("#ha sh", &['$'], true, false),
        ("x，", &['$'], false, false),
        ("x，", &['$'], true, false),
        ("x，@", &['$'], false, false),
        ("x，@", &['$'], true, true),
        ("x，[", &['$'], false, false),
        ("x，[", &['$'], true, false),
        ("<`", &['$'], false, false),
        ("<`", &['$'], true, false),
        ("`<", &['$'], false, false),
        ("`<", &['$'], true, false),
        ("([{<`@", &['$'], false, false),
        ("([{<`@", &['$'], true, true),
        ("foo (bar", &['$'], false, false),
        ("foo (bar", &['$'], true, false),
        ("foo(bar", &['$'], false, false),
        ("foo(bar", &['$'], true, false),
        ("/cmd ", &['$'], false, false),
        ("/cmd ", &['$'], true, false),
        (" /cmd @", &['$'], false, false),
        (" /cmd @", &['$'], true, true),
        ("　@", &['$'], false, false),
        ("　@", &['$'], true, true),
        ("　(", &['$'], false, false),
        ("　(", &['$'], true, false),
        ("a\t(", &['$'], false, false),
        ("a\t(", &['$'], true, false),
        ("a\n(", &['$'], false, false),
        ("a\n(", &['$'], true, false),
    ];

    #[test]
    fn autocomplete_pattern_matches_ts_golden_table() {
        for &(text, triggers, debounce, expected) in TRIGGER_PATTERN_GOLDEN {
            assert_eq!(
                autocomplete_pattern_matches(text, triggers, debounce),
                expected,
                "text={text:?} triggers={triggers:?} debounce={debounce}"
            );
        }
    }

    /// 用 TS 引擎（真实的 `Editor.autocompleteTriggerPattern` / `autocompleteDebouncePattern`）
    /// 对同一语料逐条比对，防止手写匹配器与正则语义漂移。语料与期望值由
    /// `/home/ctyun/kiana-work/upstream-reference` 的 TS 代码生成（见 fixture 生成脚本说明）。
    #[test]
    fn autocomplete_pattern_matches_ts_engine_differential() {
        let raw = include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/editor_trigger_patterns.json"
        ));
        let doc: serde_json::Value = serde_json::from_str(raw).expect("fixture 必须是合法 JSON");
        let corpus: Vec<&str> = doc["corpus"]
            .as_array()
            .expect("fixture.corpus")
            .iter()
            .map(|value| value.as_str().expect("corpus 项是字符串"))
            .collect();
        let groups = doc["groups"].as_array().expect("fixture.groups");

        for group in groups {
            let triggers: Vec<char> = group["triggers"]
                .as_array()
                .expect("group.triggers")
                .iter()
                .map(|value| {
                    let text = value.as_str().expect("trigger 是字符串");
                    let mut chars = text.chars();
                    let first = chars.next().expect("trigger 非空");
                    assert!(chars.next().is_none(), "trigger 必须是单个字符");
                    first
                })
                .collect();
            let debounce = group["debounce"].as_bool().expect("group.debounce");
            let expected: std::collections::HashSet<usize> = group["matches"]
                .as_array()
                .expect("group.matches")
                .iter()
                .map(|value| value.as_u64().expect("match 索引") as usize)
                .collect();

            for (index, text) in corpus.iter().enumerate() {
                assert_eq!(
                    autocomplete_pattern_matches(text, &triggers, debounce),
                    expected.contains(&index),
                    "text={text:?} triggers={triggers:?} debounce={debounce}"
                );
            }
        }
    }

    #[test]
    fn renumber_paste_markers_appends_undefined_suffix_like_ts() {
        // TS：`line.replace(PASTE_MARKER_REGEX, (m, id, suffix) => `[paste #${x - 1}${suffix}]`)`
        // 对没有 " +N lines"/" N chars" 后缀的 marker，`suffix` 是 undefined → 字面 "undefined"。
        assert_eq!(
            renumber_paste_markers("[paste #2]", 1),
            "[paste #1undefined]"
        );
        assert_eq!(
            renumber_paste_markers("[paste #3 +4 lines] and [paste #2]", 1),
            "[paste #2 +4 lines] and [paste #1undefined]"
        );
        assert_eq!(
            renumber_paste_markers("[paste #2 12 chars]", 1),
            "[paste #1 12 chars]"
        );
        // id <= target 的 marker 原样保留（含无后缀形式）。
        assert_eq!(renumber_paste_markers("[paste #1]", 1), "[paste #1]");
        assert_eq!(
            renumber_paste_markers("[paste #1 +9 lines][paste #5]", 4),
            "[paste #1 +9 lines][paste #4undefined]"
        );
    }
}
