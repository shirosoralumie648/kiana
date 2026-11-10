//! core/tools/render-utils.ts（TUI 渲染共享工具）。
//!
//! 所有函数都是 TS 导出面的逐字移植。关键 JS 语义：
//!
//! - `str` 区分「类型错误」（`None` -> `[invalid arg]`）与 `null`/`undefined`（`Some("")`）。
//! - [`format_read_line_range`] 必须精确返回 `""` / `:N` / `:N-M`（`end === 0` 时省略 `-0`）。
//! - [`format_tool_duration`] 的 `< 60s` 分支与 `formatSize` 共用 JS 风格的 `toFixed(1)`
//!   （`1250ms -> "1.3s"`）。
//! - [`format_tool_call_with_args`] 的折叠参数按 **UTF-16 code unit** 截到 97 + `...`。
//!
//! [`tool_timer_text`] 与 TS 一样在运行期间每秒调用一次 `context.invalidate()` 请求重绘：
//! TS 的 `setInterval` 句柄存在 `state.interval` 里，`RenderState` 只能存 JSON，因此用
//! `"interval": true` 作为存活标记，由后台线程按秒检查（见 `spawn_invalidate_interval`）。
//! 结果终结/出错时移除标记，线程随即退出；组件销毁（state 的 `Arc` 全部 drop）也会让线程退出，
//! 对应规格要求的「interval 必须随组件销毁清理」。
//!
//! # 调用方
//!
//! - `renderers/*`（bash/read/write/edit/ls/find/grep）用本模块组装工具行的调用头与结果文本；
//! - `kiana-interactive` 的工具执行组件复用 [`format_tool_call_with_args`] 与
//!   [`get_text_output`]，并提供 [`tool_timer_text`] 所需的 `invalidate` 回调；
//! - `kiana-builtin-extensions`（codemode、MCP 渲染器）也按 TS 的导出面直接调用；
//! - `kiana-extensions` 的 `@earendil-works/kiana-coding-agent` 虚拟模块通过桥接成员
//!   `formatReadLineRange` 调用本模块。
//!
//! # 并发与状态
//!
//! 渲染状态是 `Arc<Mutex<JsonValue>>`：渲染线程调用本模块的函数读写状态，计时器线程只读
//! `"interval"` 标记。加锁区间内只做 JSON 读写，不调用任何回调；`invalidate` 在锁外调用
//! （它会重新进入渲染路径，持锁调用会自锁）。键名 `startedAt`/`endedAt`/`interval` 是
//! `state` 的既有字段名，由 `start_tool_timer`/`tool_timer_text` 写入、后台线程只读
//! `"interval"`，改名必须三处同步。

use std::sync::Arc;
use std::time::Duration;

use kiana_agent_core::AgentToolResult;
use kiana_ai::types::{JsonValue, UserContentBlock};
use kiana_core::ansi::strip_ansi;
use kiana_core::shell::sanitize_binary_output;
use kiana_ext_api::tool_types::{RenderState, ToolRenderContext};
use kiana_theme::theme::Theme;
use kiana_theme::theme_tokens::ThemeColor;
use kiana_tui::{get_capabilities, get_image_dimensions, hyperlink, image_fallback};

use crate::truncate::{utf16_len, utf16_slice};

/// `COLLAPSED_ARGS_CHARS = 100`。
const COLLAPSED_ARGS_CHARS: usize = 100;

/// `setInterval(() => context.invalidate(), 1000)` 的周期。
const TOOL_TIMER_INTERVAL: Duration = Duration::from_secs(1);

/// `shortenPath(path)`：`os.homedir()` 前缀压缩成 `~/...`；非字符串返回空串。
pub fn shorten_path(path: Option<&JsonValue>) -> String {
    match path {
        Some(JsonValue::String(value)) => shorten_path_str(value),
        _ => String::new(),
    }
}

/// [`shorten_path`] 的字符串形态（渲染器已用 `str` 取到字符串时使用）。
///
/// 取不到 home 时原样返回。判断是**纯词法前缀**比较（`path.starts_with(home)`），不检查
/// 路径分隔符也不做规范化：home 为 `/home/u` 时 `/home/ubuntu/x` 也会被写成 `~buntu/x`
/// （TS 的 `path.startsWith(os.homedir())` 同样是这个行为，见
/// `spec/ca-tools-compaction-export.md` §2.16）。
pub fn shorten_path_str(path: &str) -> String {
    let Some(home) = kiana_tui::terminal_image::home_dir() else {
        return path.to_string();
    };
    let home = home.to_string_lossy();
    if path.starts_with(home.as_ref()) {
        format!("~{}", &path[home.len()..])
    } else {
        path.to_string()
    }
}

/// `linkPath(styledText, rawPath, cwd)`：终端支持超链接时用 OSC 8 包一层 `file://` URL。
///
/// 路径先按 `resolvePath(rawPath, cwd)` 解析再生成 `file://` URL；URL 编码是用户可见契约，
/// 必须保持稳定（`~ [ ] | ^ # ? %` 与空格都按 UTF-8 字节 `%XX`），因此走
/// [`kiana_tui::terminal_image::path_to_file_url`]（与 Node 26 对拍的实现），不用 `url::Url`。
///
/// 终端不支持超链接时直接返回 `styled_text`（连路径解析都不做）；`raw_path` 是绝对路径时
/// `cwd` 不参与解析。返回值是 OSC 8 序列，不支持该协议的终端可能吞掉链接文本，所以能力
/// 判断必须留在本函数内部。
pub fn link_path(styled_text: &str, raw_path: &str, cwd: &str) -> String {
    if !get_capabilities().hyperlinks {
        return styled_text.to_string();
    }
    let absolute_path = kiana_core::paths::resolve_path(
        raw_path,
        cwd,
        &kiana_core::paths::PathInputOptions::default(),
    );
    hyperlink(
        styled_text,
        &kiana_tui::terminal_image::path_to_file_url(&absolute_path),
    )
}

/// `str(value)`：字符串原样；`null`/`undefined` -> `Some("")`；其它类型 -> `None`。
///
/// `None` 是「参数类型错误」的判定入口：渲染成 `[invalid arg]`。
///
/// 严格 schema 会把省略的可选参数补成 `null`，所以 `None`（键不存在）与 `Null` 都表示
/// 「空值」；只有 number/boolean/object/array 才是真正的类型错误。
pub fn str(value: Option<&JsonValue>) -> Option<String> {
    match value {
        Some(JsonValue::String(value)) => Some(value.clone()),
        None | Some(JsonValue::Null) => Some(String::new()),
        Some(_) => None,
    }
}

/// JS 模板字符串里的 `${number}`（`String(value)`）：走 ECMAScript `Number::toString`；
/// 非有限值保留 `NaN` / `Infinity` / `-Infinity`（与 `JSON.stringify` 的 `null` 不同）。
fn js_number_text(value: f64) -> String {
    if value.is_nan() {
        return "NaN".to_string();
    }
    if value == f64::INFINITY {
        return "Infinity".to_string();
    }
    if value == f64::NEG_INFINITY {
        return "-Infinity".to_string();
    }
    kiana_json::js_num::format_js_number(value)
}

/// `parseLineNumber(value)`：数字字符串先 `Number()`；非有限数视为非法。
///
/// 空串与纯空白必须先排除：JS 的 `Number("")` 是 0，但 TS 的 `value.trim() !== ""` 守卫让
/// 字符串保持原样，最终被当作非法值（否则 `limit: ""` 会被显示成 0 行）。`"Infinity"` /
/// `"NaN"` 能通过 `Number()`，再由 `Number.isFinite` 判为非法。
fn parse_line_number(value: Option<&JsonValue>) -> Option<f64> {
    match value {
        None | Some(JsonValue::Null) => None,
        Some(JsonValue::String(text)) => {
            if kiana_json::js_string::js_trim(text).is_empty() {
                // TS：`value.trim() !== ""` 不成立 → 保持字符串 → 非 number → undefined。
                return None;
            }
            let value = kiana_json::js_num::parse_js_number(text);
            value.is_finite().then_some(value)
        }
        Some(other) => other.as_f64().filter(|value| value.is_finite()),
    }
}

/// `formatReadLineRange(offset, limit)`：`:N` / `:N-M`；无法解析时返回空串。
///
/// `offset` 缺失/null 时从第 1 行开始；`limit` 缺失/null 时只输出起点。结束行是
/// `start + count - 1`（包含式区间），数字全部按 JS 的 `String(number)` 格式化。
///
/// 这里不做范围校验：负数、小数或 `start + count - 1 == 0` 都按 JS 原样处理。`end === 0`
/// 时只输出 `:start`（TS 用真假值判断 `end`，0 是假值）；其它非法输入（无法解析或非有限）
/// 一律返回空串，让调用方只显示路径。
pub fn format_read_line_range(offset: Option<&JsonValue>, limit: Option<&JsonValue>) -> String {
    let offset_null = offset.is_none_or(JsonValue::is_null);
    let limit_null = limit.is_none_or(JsonValue::is_null);
    // 严格 schema 把省略的可选参数表示成 null。
    if offset_null && limit_null {
        return String::new();
    }
    let start = if offset_null {
        1.0
    } else {
        match parse_line_number(offset) {
            Some(value) => value,
            None => return String::new(),
        }
    };
    if limit_null {
        return format!(":{}", js_number_text(start));
    }
    let Some(count) = parse_line_number(limit) else {
        return String::new();
    };
    let end = start + count - 1.0;
    if end == 0.0 {
        format!(":{}", js_number_text(start))
    } else {
        format!(":{}-{}", js_number_text(start), js_number_text(end))
    }
}

/// `replaceTabs(text)`：每个 `\t` 换成三个空格。
pub fn replace_tabs(text: &str) -> String {
    text.replace('\t', "   ")
}

/// `normalizeDisplayText(text)`：删除所有 `\r`。
pub fn normalize_display_text(text: &str) -> String {
    text.replace('\r', "")
}

/// 结果形状（供渲染器使用）。TS: `ToolRenderResultLike<TDetails>`。
///
/// TS 的渲染器从这个形状读取 `content`/`details`。Rust 渲染器直接接收
/// `AgentToolResult<JsonValue>`，本仓库当前没有构造或消费该类型的代码，这里保留它只是为了
/// 对齐 TS 的公开导出面；`details` 在 TS 里可能是 `undefined`，使用方需要自行放宽。
pub struct ToolRenderResultLike<TDetails> {
    /// 结果内容块。
    pub content: Vec<UserContentBlock>,
    /// 结构化详情。
    pub details: TDetails,
}

/// `getTextOutput(result, showImages)`：拼接文本块，必要时追加图片占位行。
///
/// 处理顺序固定：每个文本块先 `strip_ansi`、再 `sanitize_binary_output`（去掉控制字符）、
/// 最后删除所有 `\r`，然后用 `\n` 连接。`result` 为 `None` 时返回空串。
///
/// 只要存在图片块且（终端不支持图片 `capabilities.images.is_none()` 或 `show_images` 为
/// false），就为每张图片生成 `image_fallback` 占位行；只有 base64 数据与 MIME 都非空时才
/// 尝试嗅探尺寸（`get_image_dimensions` 按 MIME 分派，失败返回 `None`）。占位行之间用
/// `\n` 连接；文本为空时直接作为输出，否则在文本后追加一个 `\n`。
pub fn get_text_output(result: Option<&AgentToolResult<JsonValue>>, show_images: bool) -> String {
    let Some(result) = result else {
        return String::new();
    };

    let text_blocks: Vec<&str> = result
        .content
        .iter()
        .filter_map(|block| match block {
            UserContentBlock::Text(text) => Some(text.text.as_str()),
            UserContentBlock::Image(_) => None,
        })
        .collect();
    let image_blocks: Vec<&kiana_ai::types::ImageContent> = result
        .content
        .iter()
        .filter_map(|block| match block {
            UserContentBlock::Image(image) => Some(image),
            UserContentBlock::Text(_) => None,
        })
        .collect();

    let mut output = text_blocks
        .into_iter()
        .map(|text| sanitize_binary_output(&strip_ansi(text)).replace('\r', ""))
        .collect::<Vec<_>>()
        .join("\n");

    let capabilities = get_capabilities();
    if !image_blocks.is_empty() && (capabilities.images.is_none() || !show_images) {
        let image_indicators = image_blocks
            .into_iter()
            .map(|image| {
                let mime_type = image.mime_type.as_str();
                let dimensions = if !image.data.is_empty() && !image.mime_type.is_empty() {
                    get_image_dimensions(&image.data, mime_type)
                } else {
                    None
                };
                image_fallback(mime_type, dimensions, None)
            })
            .collect::<Vec<_>>()
            .join("\n");
        output = if output.is_empty() {
            image_indicators
        } else {
            format!("{output}\n{image_indicators}")
        };
    }

    output
}

/// JS 的「数组索引」键判定：`0`、`1`、…、`4294967294`（无前导零）。
///
/// 与 `kiana_json::js_object` 内部的判定一致；这里只用来复刻 `Object.entries` 的顶层遍历顺序
/// （整数索引键升序在前，其余键保持插入序）。
fn js_array_index(key: &str) -> Option<u32> {
    if key.is_empty() || key.len() > 10 {
        return None;
    }
    if key.len() > 1 && key.starts_with('0') {
        return None;
    }
    if !key.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    let value: u32 = key.parse().ok()?;
    if value == u32::MAX {
        return None;
    }
    Some(value)
}

/// `Object.entries(args)` 的键序：整数索引键按数值升序，其余键按插入序。
fn js_object_entries(map: &serde_json::Map<String, JsonValue>) -> Vec<(&str, &JsonValue)> {
    let mut indices: Vec<(u32, &str, &JsonValue)> = Vec::new();
    let mut others: Vec<(&str, &JsonValue)> = Vec::new();
    for (key, value) in map {
        match js_array_index(key) {
            Some(index) => indices.push((index, key.as_str(), value)),
            None => others.push((key.as_str(), value)),
        }
    }
    // 同一对象里索引键唯一，排序稳定性无关紧要。
    indices.sort_by_key(|(index, _, _)| *index);
    let mut entries = Vec::with_capacity(map.len());
    entries.extend(indices.into_iter().map(|(_, key, value)| (key, value)));
    entries.extend(others);
    entries
}

/// `formatToolCallWithArgs(title, args, theme, expanded)`：通用 fallback 头部。
///
/// 折叠时是标题行上的 `key=value` 对（按 UTF-16 截到 97 + `...`）；展开时每个参数一行
/// `key: value`，字符串原样、其它值用 `JSON.stringify(value, null, 2)`。
///
/// 细节：
/// - `args` 为 `None`/`null` 或空对象时只返回标题；非对象参数按唯一的 `args` 键呈现；
/// - 参数顺序复刻 `Object.entries`（整数索引键升序在前，其余键保持插入序，见
///   `js_object_entries`）；
/// - 折叠分支的阈值是 **UTF-16 code unit**：完整 `pairs` 超过 100 时取前 97 个码元再加
///   `...`（`COLLAPSED_ARGS_CHARS - 3`）；
/// - 展开分支先删除 `\r`、把制表符换成 3 个空格，续行统一缩进 4 个空格；
/// - 标题用 `ToolTitle` 色加粗，参数部分用 `Muted` 色。
pub fn format_tool_call_with_args(
    title: &str,
    args: Option<&JsonValue>,
    theme: &Theme,
    expanded: bool,
) -> String {
    let header = theme.fg(ThemeColor::ToolTitle, &theme.bold(title));
    let Some(args) = args.filter(|args| !args.is_null()) else {
        return header;
    };
    let entries: Vec<(&str, &JsonValue)> = match args {
        JsonValue::Object(map) => js_object_entries(map),
        other => vec![("args", other)],
    };
    if entries.is_empty() {
        return header;
    }

    if expanded {
        let lines = entries
            .iter()
            .map(|(key, value)| {
                let text = match value {
                    JsonValue::String(text) => text.clone(),
                    other => kiana_json::js_object::stringify_pretty(other, 2),
                };
                let text = replace_tabs(&normalize_display_text(&text));
                let text = text.split('\n').collect::<Vec<_>>().join("\n    ");
                format!("  {key}: {text}")
            })
            .collect::<Vec<_>>()
            .join("\n");
        return format!("{header}\n{}", theme.fg(ThemeColor::Muted, &lines));
    }

    let pairs = entries
        .iter()
        .map(|(key, value)| format!("{key}={}", kiana_json::js_object::stringify(value)))
        .collect::<Vec<_>>()
        .join(" ");
    let preview = if utf16_len(&pairs) > COLLAPSED_ARGS_CHARS {
        format!("{}...", utf16_slice(&pairs, 0, COLLAPSED_ARGS_CHARS - 3))
    } else {
        pairs
    };
    format!("{header} {}", theme.fg(ThemeColor::Muted, &preview))
}

/// `formatToolDuration(ms)`：`4.2s`、`1m 30s`、`2h 3m 4s`。
///
/// 三段分支：`seconds < 60` 用 `toFixed(1)`（JS 的中点远离零，`1250ms -> "1.3s"`）；
/// 否则对总秒数取 `floor` 后拆出分钟与秒；分钟再达到 60 时继续拆出小时。分支判断发生在
/// 取整之前，所以 `59_999ms` 经 `toFixed(1)` 得到 `"60.0s"`，而 `60_000ms` 走分钟分支
/// 得到 `"1m 0s"`。
pub fn format_tool_duration(ms: f64) -> String {
    let seconds = ms / 1000.0;
    if seconds < 60.0 {
        return format!("{}s", kiana_ai::model_catalog::js_to_fixed(seconds, 1));
    }

    let total_seconds = seconds.floor();
    let minutes = (total_seconds / 60.0).floor();
    let remainder = total_seconds % 60.0;
    if minutes < 60.0 {
        return format!(
            "{}m {}s",
            js_number_text(minutes),
            js_number_text(remainder)
        );
    }

    format!(
        "{}h {}m {}s",
        js_number_text((minutes / 60.0).floor()),
        js_number_text(minutes % 60.0),
        js_number_text(remainder)
    )
}

/// `Date.now()`（毫秒，纪元起的整数）。
fn now_millis() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as f64)
        .unwrap_or(0.0)
}

/// 取渲染状态锁；中毒时取回内部值继续。状态只是普通 JSON，没有需要因 panic 丢弃的跨键
/// 不变量，恢复比让整个工具行永久无法渲染更合适。
fn lock_state(
    state: &kiana_ext_api::tool_types::RenderState,
) -> std::sync::MutexGuard<'_, JsonValue> {
    state
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// 保证渲染状态是一个 JSON 对象。
fn state_object(state: &mut JsonValue) -> &mut serde_json::Map<String, JsonValue> {
    if !state.is_object() {
        *state = JsonValue::Object(serde_json::Map::new());
    }
    match state {
        JsonValue::Object(map) => map,
        _ => unreachable!("state was just replaced with an object"),
    }
}

/// `setInterval(() => context.invalidate(), 1000)`（TS: `toolTimerText`）。
///
/// `RenderState` 不能保存 `setInterval` 句柄，所以把「interval 存活」编码为状态里的
/// `"interval": true` 标记：线程每秒检查一次标记，标记消失（结果终结/出错）或 state 被销毁
/// （组件销毁）后立即退出。回调在锁外执行，`invalidate` 触发的重绘重新进入渲染函数时不会死锁。
///
/// 线程只持有 `Weak`，不阻止组件销毁；它先 `sleep` 再尝试 `upgrade`，所以启动后的第一个 tick
/// 在 1 秒后，首帧不会立即多一次重绘。线程创建失败被忽略：计时文本仍会随下一次按需渲染
/// 更新，只是不再每秒主动请求重绘。
fn spawn_invalidate_interval(state: &RenderState, invalidate: Arc<dyn Fn() + Send + Sync>) {
    let state = Arc::downgrade(state);
    let spawned = std::thread::Builder::new()
        .name("kiana-tool-timer".to_string())
        .spawn(move || {
            loop {
                std::thread::sleep(TOOL_TIMER_INTERVAL);
                let Some(state) = state.upgrade() else {
                    return;
                };
                let active = {
                    let state = state
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner());
                    kiana_json::is_truthy(state.get("interval"))
                };
                if !active {
                    return;
                }
                invalidate();
            }
        });
    // 线程创建失败时只是停止每秒重绘（按需渲染仍然工作），与 setInterval 资源耗尽一样罕见。
    let _ = spawned;
}

/// `startToolTimer(context)`：执行开始时记录 `startedAt`。
///
/// 幂等：同一 tool call 的多次渲染共享状态，已有 `"startedAt"`（包括值为 null 的旧状态）时
/// 不覆盖，也不会清掉已有的 `"endedAt"`；只有首次写入 `startedAt` 时才顺带移除旧的
/// `"endedAt"`。`execution_started` 为 false（还没开始执行）时完全不写状态，避免在排队
/// 阶段就启动计时。
pub fn start_tool_timer(context: &ToolRenderContext) {
    if !context.execution_started {
        return;
    }
    let mut state = lock_state(&context.state);
    let object = state_object(&mut state);
    if !object.contains_key("startedAt") {
        object.insert("startedAt".to_string(), JsonValue::from(now_millis()));
        object.remove("endedAt");
    }
}

/// `toolTimerText(context, isPartial)`：`Elapsed 4.2s` / `Took 4.2s`。
///
/// 最终结果的 `durationMs` 优先（单调且能跨重载保留）；否则用渲染器自己的时钟。
///
/// # 执行阶段
///
/// 1. 持锁读 `"startedAt"`；当 `startedAt` 存在、`is_partial` 为真且 `"interval"` 尚不真值时，
///    写入 `"interval": true`。判断与写入在同一把锁内，并发调用只会有一个负责启动线程。
/// 2. 当 `!is_partial || context.is_error` 时进入终结分支：`"endedAt"` 只在缺失时写入
///    （保留第一次观察到的结束时间），并删除 `"interval"` 让后台线程停止请求重绘。出错时
///    即使 `is_partial` 仍为真也按终结处理，但返回的标签仍是 `Elapsed`。
/// 3. 锁外启动后台线程（见 `spawn_invalidate_interval`），避免重入渲染路径时死锁。
/// 4. 非 partial 且 `context.duration_ms` 存在时直接返回 `Took <durationMs>`；否则必须有
///    `startedAt` 才能计算，`endedAt` 缺失时用当前时间代替。两者都没有时返回 `None`。
pub fn tool_timer_text(context: &ToolRenderContext, is_partial: bool) -> Option<String> {
    let (started_at, ended_at, start_interval) = {
        let mut state = lock_state(&context.state);
        let object = state_object(&mut state);

        let started_at = object.get("startedAt").and_then(JsonValue::as_f64);
        // `state.startedAt !== undefined && isPartial && !state.interval`
        let start_interval =
            started_at.is_some() && is_partial && !kiana_json::is_truthy(object.get("interval"));
        if start_interval {
            object.insert("interval".to_string(), JsonValue::Bool(true));
        }
        if !is_partial || context.is_error {
            if !object.contains_key("endedAt") {
                object.insert("endedAt".to_string(), JsonValue::from(now_millis()));
            }
            // `clearInterval(state.interval)`：移除存活标记，后台线程在下一秒退出。
            object.remove("interval");
        }
        let ended_at = object.get("endedAt").and_then(JsonValue::as_f64);
        (started_at, ended_at, start_interval)
    };
    if start_interval {
        spawn_invalidate_interval(&context.state, Arc::clone(&context.invalidate));
    }

    if !is_partial && let Some(duration_ms) = context.duration_ms {
        return Some(format!("Took {}", format_tool_duration(duration_ms as f64)));
    }

    let started_at = started_at?;
    let end_time = ended_at.unwrap_or_else(now_millis);
    Some(format!(
        "{} {}",
        if is_partial { "Elapsed" } else { "Took" },
        format_tool_duration(end_time - started_at)
    ))
}

/// `invalidArgText(theme)`：`[invalid arg]`。
pub fn invalid_arg_text(theme: &Theme) -> String {
    theme.fg(ThemeColor::Error, "[invalid arg]")
}

/// [`render_tool_path`] 的选项。TS 的内联对象 `{ emptyFallback?: string }`。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RenderToolPathOptions {
    /// `rawPath` 为空字符串时使用的替代文本。
    pub empty_fallback: Option<String>,
}

/// `renderToolPath(rawPath, theme, cwd, options?)`。
///
/// `raw_path` 为 `None` 对应 TS 的 `null`（类型错误）→ `[invalid arg]`；`Some("")` 对应
/// `null`/`undefined` → `emptyFallback` 或 `...`。
///
/// 空串先替换成 `empty_fallback`，替换后仍为空（包括 fallback 是空串的情况）时输出
/// `ToolOutput` 色的 `...`；非空值经 [`shorten_path_str`] 压缩 home 前缀、用 `Accent` 色
/// 着色，再交给 [`link_path`] 按终端能力决定是否包 OSC 8 链接。
pub fn render_tool_path(
    raw_path: Option<&str>,
    theme: &Theme,
    cwd: &str,
    options: Option<&RenderToolPathOptions>,
) -> String {
    let Some(raw_path) = raw_path else {
        return invalid_arg_text(theme);
    };
    let value = if raw_path.is_empty() {
        options
            .and_then(|options| options.empty_fallback.as_deref())
            .unwrap_or("")
    } else {
        raw_path
    };
    if value.is_empty() {
        return theme.fg(ThemeColor::ToolOutput, "...");
    }
    link_path(
        &theme.fg(ThemeColor::Accent, &shorten_path_str(value)),
        value,
        cwd,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn str_distinguishes_types() {
        assert_eq!(str(None), Some(String::new()));
        assert_eq!(str(Some(&JsonValue::Null)), Some(String::new()));
        assert_eq!(str(Some(&JsonValue::String("x".into()))), Some("x".into()));
        assert_eq!(str(Some(&JsonValue::from(1))), None);
    }

    #[test]
    fn line_ranges_match_the_ts_cases() {
        let cases: Vec<(Option<JsonValue>, Option<JsonValue>, &str)> = vec![
            (
                Some(JsonValue::from(25)),
                Some(JsonValue::from(13)),
                ":25-37",
            ),
            (
                Some(JsonValue::String("25".into())),
                Some(JsonValue::String("13".into())),
                ":25-37",
            ),
            (
                Some(JsonValue::Null),
                Some(JsonValue::String("13".into())),
                ":1-13",
            ),
            (
                Some(JsonValue::String("25".into())),
                Some(JsonValue::Null),
                ":25",
            ),
            (None, None, ""),
            (
                Some(JsonValue::String("abc".into())),
                Some(JsonValue::from(13)),
                "",
            ),
            (
                Some(JsonValue::from(25)),
                Some(JsonValue::String(String::new())),
                "",
            ),
            (
                Some(JsonValue::from(25)),
                Some(JsonValue::String("Infinity".into())),
                "",
            ),
        ];
        for (offset, limit, expected) in cases {
            assert_eq!(
                format_read_line_range(offset.as_ref(), limit.as_ref()),
                expected
            );
        }
    }

    #[test]
    fn durations_match_the_ts_table() {
        let cases = [
            (0.0, "0.0s"),
            (4_200.0, "4.2s"),
            (59_900.0, "59.9s"),
            (59_999.0, "60.0s"),
            (60_000.0, "1m 0s"),
            (90_900.0, "1m 30s"),
            (1_592_200.0, "26m 32s"),
            (3_599_999.0, "59m 59s"),
            (3_600_000.0, "1h 0m 0s"),
            (7_384_900.0, "2h 3m 4s"),
            (1_250.0, "1.3s"),
        ];
        for (ms, expected) in cases {
            assert_eq!(format_tool_duration(ms), expected, "{ms}");
        }
    }
}
