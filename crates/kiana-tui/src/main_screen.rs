//! tui-main-screen.ts：主屏（滚动缓冲区）渲染器。
//!
//! `TuiMainScreen` 把渲染结果写进终端的**主屏**与滚动缓冲区：首次渲染直接输出，之后按
//! "首个/末个变化行"做差分输出；宽度/高度变化、`clearOnShrink`、视口上方变化等 8 类情形
//! 退回全量重绘（`\x1b[2J\x1b[H\x1b[3J`）。所有输出包在同步更新序列
//! `\x1b[?2026h` / `\x1b[?2026l` 之间，并按 1 MiB（**UTF-16 code unit**）分块写入终端。
//!
//! ## 与 TS 的对应关系
//!
//! | TS | Rust |
//! |---|---|
//! | `class BoundedTerminalWriter` | `BoundedTerminalWriter`（模块私有） |
//! | `parseKittyImageHeader` 等 | `parse_kitty_image_header` 等 |
//! | `class TuiMainScreen extends TuiBase` | [`TuiMainScreen`] **持有** [`TuiBase`]（组合） |
//! | `protected doRender()` | [`TuiMainScreen::do_render`] |
//! | `resetRenderState` / `beforeTerminalStop` 覆写 | [`TuiMainScreen::reset_render_state`] / `MainScreenHooks` |
//!
//! ## 组合而非继承
//!
//! `TuiBase` 的构造契约（`spec/tui-core.md` §2.9）把它做成"被持有"的结构体：渲染器在
//! [`TuiBase::set_lifecycle_hooks`] 注入的钩子里实现 TS 子类的 protected 覆写点。本模块的
//! 渲染状态放在 `Rc<RefCell<RenderState>>` 里，与钩子共享（`beforeTerminalStop` 需要读
//! `previousLines`，写由渲染器负责，而钩子对象无法借用宿主）。
//!
//! ## 已知偏差
//!
//! - 超宽行用 `panic!` 报告，载荷是**逐字相同**的错误文本（[`TuiMainScreen::do_render`] 的
//!   crash 分支）。`Tui::render_now` 没有错误通道，panic 是唯一能同步中止本帧的语义。
//! - TS 的 `fs.mkdirSync` / `fs.appendFileSync` / `fs.writeFileSync` 不捕获错误（失败会从
//!   `doRender()` 抛出并中止本帧）；Rust 侧同样中止，但形式是 `panic!`，文本按 Node 的
//!   `CODE: message, syscall 'path'` 格式重建（`panic_fs_error`）。
//! - `Container.mouseLayout` 持有组件**引用**，`removeChild` 之后仍能向已脱离树的组件分发；
//!   Rust 用 `TuiMainScreen::detached_children` 保活到下一次渲染来复现。两个无法完全复现的
//!   角落：直接经 [`Tui::children`] 裸改 `Vec` 移除的组件不会被保活（也就无法再分发）；被移除
//!   组件的 `focusTarget` 以地址表示，宿主用它解析焦点时找不到（`TuiBase::focus_target_of_address`
//!   只看挂载树），会退回调用方给的兜底目标。
//! - 环境变量的读取一律在渲染时现场进行，便于测试用环境变量隔离。
//!
//! ## 一帧的渲染流程
//!
//! [`TuiMainScreen::do_render`] 是唯一的渲染出口，分支顺序与 `spec/tui-core.md` §2.10.5
//! 的表格逐条对应：
//!
//! 1. `stopped` 为真时直接返回；否则先刷新 overlay handle 挂起的 `focused` 标志。
//! 2. 读终端宽高，并由上一帧的尺寸与视口顶推算本轮视口位置（高度变化时重新对齐）。
//! 3. `render_children` → `resolve_fake_cursors` → overlay 合成 → `extract_cursor_position`
//!    → `apply_line_resets`。光标标记必须在 `apply_line_resets` 之前提取（与 TS 的顺序
//!    一致：marker must be found first）。
//! 4. 阶段 3 先判前 4 类需要全量重绘的情形：首次渲染、宽度变化、高度变化（Termux 会话
//!    除外）、`clearOnShrink`。命中即写一帧 `full_render` 并返回。
//! 5. 否则进入行级差分：找首/末变化行 → 把受影响的 Kitty 图片块扩展到整块 → 需要时用
//!    CRLF 滚屏 → 只重写 `first_changed..=render_end`。差分过程中另有 4 类情形退回
//!    `full_render(true)`：全删除分支里的"删除行导致视口上移"与"待清行数超过屏高"、
//!    差分区间算完后的"首个变化行在视口上方"，以及阶段 5 输出循环里的"Kitty 图片预清
//!    会滚屏"。
//! 6. 更新渲染状态：`previous_lines`、光标行、视口顶、Kitty 图片 id 集合、尺寸。
//!
//! ## 状态与不变量
//!
//! - `RenderState` 由渲染器与 `MainScreenHooks` 共享（`beforeTerminalStop` 需要读
//!   `previous_lines`，而钩子对象拿不到宿主）。`previous_lines` 是下一帧差分比较的基准，
//!   正常路径上等于最后一帧计算出的内容行；[`TuiMainScreen::restore_render_state`] 会故意
//!   把图片行抹成空行，让这些图片在下一帧被当作变化重新输出。
//! - `cursor_row` 是内容最后一行的下标，供状态快照与调试转储；视口计算实际使用
//!   `previous_viewport_top` 与 `hardware_cursor_row`。`hardware_cursor_row` 是终端光标
//!   实际所在的内容行号；差分未重写到末行（`render_end` 小于末行下标且没有删除行）或随后
//!   按光标标记定位时，两者可能不同。
//! - `previous_width` / `previous_height` 的默认值 `0` 表示"从未渲染"（不算尺寸变化）；
//!   `reset_render_state` 写入哨兵 `-1`，使下一帧必然命中尺寸变化分支而全量重绘。
//! - 所有输出按 UTF-16 code unit 计 1 MiB 分块；每个正常结束的帧都必须 `flush`。
//!   中途改走 `full_render` 的分支会丢弃 writer 中尚未 flush 的内容（那些字节不会写到
//!   终端）；全量重绘会从头重写整屏，因此那部分缓冲无需保留。
//!
//! ## 调用方
//!
//! 宿主（`kiana-interactive`）用 [`TuiMainScreen::flush_pending_render`] 驱动单帧，并用
//! [`TuiMainScreen::capture_render_state`] / [`TuiMainScreen::restore_render_state`] 在
//! 主屏与全屏之间搬运渲染基准。扩展 UI（`kiana-extensions`）通过 `Component::as_any`
//! 向下转型后取 [`TuiMainScreen::terminal`]；`kiana-theme` 通过 [`TuiMainScreen::base_mut`]
//! 访问配色查询接口。

use std::cell::RefCell;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{SystemTime, UNIX_EPOCH};

use indexmap::IndexSet;

use crate::terminal_colors::Terminal;
use crate::terminal_image::{delete_kitty_image, is_image_line};
use crate::text::visible_width;
use crate::tui::{
    Component, FocusTarget, InputListenerId, OverlayHandle, OverlayOptions, Tui, TuiBase,
    TuiInputListener, TuiLifecycleHooks, TuiMode, TuiMouseEvent, TuiMouseEventResult,
    TuiStopOptions,
};

/// Kitty 图形序列前缀（`\x1b_G`）。
const KITTY_SEQUENCE_PREFIX: &str = "\x1b_G";
/// 单次 `terminal.write` 的最大长度（UTF-16 code unit 计）。
const MAX_RENDER_WRITE_CHARS: usize = 1024 * 1024;

/// 同步更新开始 / 结束（`\x1b[?2026h` / `\x1b[?2026l`）。
const BEGIN_SYNCHRONIZED_OUTPUT: &str = "\x1b[?2026h";
const END_SYNCHRONIZED_OUTPUT: &str = "\x1b[?2026l";

/// `resetRenderState()` 之后 `previousWidth` / `previousHeight` 的哨兵值。
const RESET_SIZE: i64 = -1;

// ============================================================================
// BoundedTerminalWriter
// ============================================================================

/// 按 1 MiB（UTF-16 code unit）分块写入终端，避免单次 `write` 过大。
///
/// `append` 填满当前块时自动 flush；超长输入按块边界切分，**绝不拆开 UTF-16 代理对**
/// （因此每个块都是合法 UTF-16，也就都是合法 UTF-8）。调用方自己追加同步更新序列；
/// 最后一次 `append` 之后必须调用 [`BoundedTerminalWriter::flush`]，否则缓冲区内容
/// **被丢弃**（TS 里未 flush 的 writer 实例被 GC 掉，效果相同；差分渲染的"中途改走全量
/// 重绘"分支依赖这一点）。
struct BoundedTerminalWriter {
    buffer: String,
    written_chars: usize,
    write: Box<dyn FnMut(&str)>,
}

impl BoundedTerminalWriter {
    fn new(write: Box<dyn FnMut(&str)>) -> Self {
        BoundedTerminalWriter {
            buffer: String::new(),
            written_chars: 0,
            write,
        }
    }

    /// `append(value)`：逐块填入；块满即写。
    fn append(&mut self, value: &str) {
        let units: Vec<u16> = value.encode_utf16().collect();
        let mut offset = 0usize;
        while offset < units.len() {
            let capacity = MAX_RENDER_WRITE_CHARS - utf16_len(&self.buffer);
            if capacity == 0 {
                self.flush();
                continue;
            }

            let mut end = units.len().min(offset + capacity);
            if end < units.len()
                && (0xd800..=0xdbff).contains(&units[end - 1])
                && (0xdc00..=0xdfff).contains(&units[end])
            {
                end -= 1;
            }
            if end == offset {
                self.flush();
                continue;
            }

            // 切分点落在 code unit 边界且不拆代理对，因此这里总能构造出合法字符串。
            self.buffer
                .push_str(&String::from_utf16_lossy(&units[offset..end]));
            offset = end;
            if utf16_len(&self.buffer) == MAX_RENDER_WRITE_CHARS {
                self.flush();
            }
        }
    }

    /// `flush()`：写出当前块（若有），只保留累计字符数。
    fn flush(&mut self) {
        if self.buffer.is_empty() {
            return;
        }
        let buffer = std::mem::take(&mut self.buffer);
        (self.write)(&buffer);
        self.written_chars += utf16_len(&buffer);
    }

    /// `get length()`：已写出 + 缓冲中的 UTF-16 code unit 数。
    fn length(&self) -> usize {
        self.written_chars + utf16_len(&self.buffer)
    }
}

/// `string.length`（UTF-16 code unit 数）。
fn utf16_len(value: &str) -> usize {
    value.encode_utf16().count()
}

// ============================================================================
// Kitty 图片头解析
// ============================================================================

/// `interface KittyImageHeader { ids: number[]; rows: number }`。
#[derive(Debug, Clone, PartialEq, Eq)]
struct KittyImageHeader {
    /// `i=` 参数里所有合法 id（同一行可携带多个）。
    ids: Vec<u64>,
    /// `r=` 参数（占用行数），缺省为 1。
    rows: u32,
}

/// `parseKittyImageHeader(line)`：解析 `\x1b_G<params>;` 里的 `i=` / `r=`。
///
/// `Number(value)` 的语义由 [`kiana_json::parse_js_number`] 提供（空串/空白 → 0，`1e3` → 1000，
/// `abc` → NaN）；`Number.isInteger` 排除小数、NaN 与 `Infinity`；值域 `(0, 0xffffffff]`。
fn parse_kitty_image_header(line: &str) -> Option<KittyImageHeader> {
    let sequence_start = line.find(KITTY_SEQUENCE_PREFIX)?;
    let params_start = sequence_start + KITTY_SEQUENCE_PREFIX.len();
    let params_end = params_start + line[params_start..].find(';')?;

    let mut ids = Vec::new();
    let mut rows: u32 = 1;
    for param in line[params_start..params_end].split(',') {
        // TS 用 `param.split("=", 2)`：JS 的 limit 只是**截断结果数组**，分割仍发生在每个
        // 分隔符上，因此 `"i=5=6"` 得到 `["i", "5"]`（而不是 Rust `splitn(2)` 的 `"5=6"`）。
        // 这里用无上限 `split` 再取前两段，复现同一语义。
        let mut parts = param.split('=');
        let key = parts.next().unwrap_or("");
        let Some(value) = parts.next() else {
            continue;
        };
        let number_value = kiana_json::parse_js_number(value);
        if !kiana_json::is_js_integer(number_value)
            || number_value <= 0.0
            || number_value > 0xffff_ffffu64 as f64
        {
            continue;
        }
        if key == "i" {
            ids.push(number_value as u64);
        } else if key == "r" {
            rows = number_value as u32;
        }
    }
    Some(KittyImageHeader { ids, rows })
}

/// `extractKittyImageIds(line)`。
fn extract_kitty_image_ids(line: &str) -> Vec<u64> {
    parse_kitty_image_header(line)
        .map(|header| header.ids)
        .unwrap_or_default()
}

/// `extractKittyImageRows(line)`。
fn extract_kitty_image_rows(line: &str) -> u32 {
    parse_kitty_image_header(line).map_or(1, |header| header.rows)
}

/// `isTermuxSession()`：`Boolean(process.env.TERMUX_VERSION)`（空串为假）。
fn is_termux_session() -> bool {
    std::env::var("TERMUX_VERSION").is_ok_and(|value| !value.is_empty())
}

/// 读取布尔环境变量开关：只认字面量 `"1"`（TS 的 `=== "1"` 语义）。变量缺失、空串或
/// 其他值都返回 `false`。
fn env_flag(name: &str) -> bool {
    std::env::var(name).is_ok_and(|value| value == "1")
}

// ============================================================================
// 渲染状态与快照
// ============================================================================

/// `interface TuiMainScreenRenderState`：7 个渲染状态字段的快照。
///
/// 由 [`TuiMainScreen::capture_render_state`] 生成、[`TuiMainScreen::restore_render_state`]
/// 消费，用于主屏渲染器被替换（主屏 ↔ 全屏切换）时让新渲染器沿用旧的差分基准。
/// 注意快照**不含** `previous_kitty_image_ids`：恢复时该集合总是被清空。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TuiMainScreenRenderState {
    /// 上一帧计算出的内容行（图片行在恢复时会被抹成空行）。
    pub previous_lines: Vec<String>,
    /// 上一帧的终端宽度；`0` = 从未渲染，`-1` = `reset_render_state` 的强制重绘哨兵。
    pub previous_width: i64,
    /// 上一帧的终端高度；哨兵含义同 [`TuiMainScreenRenderState::previous_width`]。
    pub previous_height: i64,
    /// 内容最后一行的下标（`new_lines.len() - 1`，空内容为 0）。
    pub cursor_row: usize,
    /// 终端光标实际所在的内容行号（差分未重写到末行或按光标标记定位后可能与
    /// `cursor_row` 不同）。
    pub hardware_cursor_row: usize,
    /// 历史渲染的最大行数（高水位），`clearOnShrink` 用它判断内容是否收缩。
    pub max_lines_rendered: usize,
    /// 上一帧视口顶部在内容缓冲区中的行号。
    pub previous_viewport_top: usize,
}

/// 渲染器私有状态（TS 的同名私有字段）。
#[derive(Debug, Default)]
struct RenderState {
    /// 上一帧输出到终端的内容行。空可能是从未渲染、`reset_render_state` 清空，或上一帧
    /// 内容本身为空；首帧不清屏路径要求它为空且 `previous_width` / `previous_height`
    /// 均为 0。
    previous_lines: Vec<String>,
    /// TS `Set<number>`：插入顺序即删除序列的输出顺序。
    previous_kitty_image_ids: IndexSet<u64>,
    /// 上一帧的终端宽度。`0` = 默认/从未渲染；`-1` = `reset_render_state` 的哨兵。
    previous_width: i64,
    /// 上一帧的终端高度。哨兵含义同 `previous_width`。
    previous_height: i64,
    /// 内容最后一行的下标，供状态快照与调试转储；视口计算实际使用
    /// `previous_viewport_top` 与 `hardware_cursor_row`，不读本字段。
    cursor_row: usize,
    /// 终端光标实际所在的内容行号（以内容顶部为 0，不是屏幕行）。
    hardware_cursor_row: usize,
    /// 历史渲染出现的最大行数（高水位）；清屏重绘时置为当前行数，`reset_render_state` 时置 0。
    max_lines_rendered: usize,
    /// 上一帧视口顶部在内容缓冲区中的行号（缓冲区包含滚动出去的历史行）。
    previous_viewport_top: usize,
}

/// `beforeTerminalStop` 覆写点：`TuiBase` 的钩子对象与渲染器共享状态。
///
/// 钩子由 [`TuiBase::set_lifecycle_hooks`] 持有，无法反向借用渲染器，因此这里只拿
/// `Rc` 句柄：`state` 用来读 `previous_lines` / `hardware_cursor_row`，`terminal` 用来
/// 写收尾序列。其余六个生命周期方法沿用 [`TuiLifecycleHooks`] 的默认实现。
struct MainScreenHooks {
    state: Rc<RefCell<RenderState>>,
    terminal: Rc<RefCell<Box<dyn Terminal>>>,
}

impl TuiLifecycleHooks for MainScreenHooks {
    /// TS `beforeTerminalStop(options)`：`preserve_screen` 时把画面原样留给下一个 TUI；
    /// 否则把光标移到已渲染内容之后并换行。
    ///
    /// 目标行是 `previous_lines.len()`（最后一行之下）。输出顺序是当前稳定契约
    /// （出处 `tui-main-screen.ts:168-176`）：先写一个空格，再按 `hardware_cursor_row` 计算行差
    /// 上下移动，最后写 `\r\n` 落到新行。`previous_lines` 为空（可能是从未渲染、
    /// `reset_render_state` 清空，或上一帧内容本身为空）时什么都不做。
    fn before_terminal_stop(&mut self, options: TuiStopOptions) {
        let state = self.state.borrow();
        if options.preserve_screen || state.previous_lines.is_empty() {
            return;
        }
        let target_row = state.previous_lines.len();
        let hardware_cursor_row = state.hardware_cursor_row;
        drop(state);

        let lines_to_move = target_row as isize - hardware_cursor_row as isize;
        let mut terminal = self.terminal.borrow_mut();
        terminal.write(" ");
        if lines_to_move > 0 {
            terminal.write(&format!("\x1b[{lines_to_move}B"));
        } else if lines_to_move < 0 {
            terminal.write(&format!("\x1b[{}A", -lines_to_move));
        }
        terminal.write("\r\n");
    }
}

// ============================================================================
// TuiMainScreen
// ============================================================================

/// 主屏 TUI（TS `class TuiMainScreen extends TuiBase implements TUI`，`mode = "regular"`）。
///
/// 采用组合而非继承：`base` 提供挂载树、焦点/overlay 状态机、输入分发、渲染调度与生命
/// 周期钩子；本类型只负责主屏差分渲染与鼠标命中的槽位计算。渲染状态放在共享的
/// `Rc<RefCell<RenderState>>` 里，因为 `beforeTerminalStop` 钩子需要读它（写由渲染器
/// 负责），却拿不到宿主引用（见 `MainScreenHooks`）。
///
/// 与备用屏渲染器的区别：输出写入终端主屏并保留滚动缓冲区，因此只能做"行级差分"；
/// 宽高变化、光标上方内容变化等会破坏坐标假设的情形必须退回全量重绘。
pub struct TuiMainScreen {
    base: TuiBase,
    state: Rc<RefCell<RenderState>>,
    /// `Container.mouseLayout` 的等价物：宽度 + 每个子项的 `(组件身份, 渲染高度)`。
    ///
    /// TS 缓存的是组件**引用**；Rust 用 [`component_identity`]（胖指针的数据指针 + vtable）
    /// 代替引用相等：`Box` 在 `Vec` 里换位不改变身份，零大小组件的不同具体类型靠 vtable 区分。
    mouse_layout: Option<(usize, Vec<(ComponentIdentity, usize)>)>,
    /// 已被 [`Tui::remove_child`] / [`Tui::clear_children`] 移出 `children`、但 `mouse_layout`
    /// 可能仍在引用的组件。
    ///
    /// TS 的 `mouseLayout` 持有组件引用，`removeChild` 只是 `splice` 掉 `children` 里的那一项，
    /// 因此下一次渲染之前到达的鼠标事件仍会交给**已脱离树**的组件（`tui.ts:383-403`）。
    /// Rust 用这个列表保活被移除的组件直到下一次渲染（`mouse_layout` 被替换时清空），
    /// 让同一次分发仍然能调用到它，并避免堆地址被新分配复用后误路由。
    detached_children: Vec<Box<dyn Component>>,
}

impl TuiMainScreen {
    /// `new TuiMainScreen(terminal)`：等价于 `with_options(terminal, None, None)`。
    pub fn new(terminal: Box<dyn Terminal>) -> Self {
        Self::with_options(terminal, None, None)
    }

    /// `new TuiMainScreen(terminal, showHardwareCursor?, logDirectory?)`。
    ///
    /// - `show_hardware_cursor`：`None` 按 `false` 处理（`TuiBase::new` 的 `unwrap_or(false)`）；
    ///   `true` 时每帧渲染后在焦点光标处显示真实光标，没有光标标记时隐藏（见
    ///   [`TuiMainScreen::do_render`] 末尾的定位逻辑）。
    /// - `log_directory`：`Some` 时同时启用两类调试文件——`KIANA_TUI_DEBUG_REDRAW=1` 下的
    ///   `kiana-tui-debug.log`，以及超宽行 crash 时的 `kiana-tui-crash.log`；`None` 时重绘
    ///   日志关闭，crash 日志回退到 `os.tmpdir()`。
    ///
    /// 构造时安装 `MainScreenHooks`（`beforeTerminalStop` 覆写点），并把共享的 `state`
    /// 同时交给渲染器与钩子。
    pub fn with_options(
        terminal: Box<dyn Terminal>,
        show_hardware_cursor: Option<bool>,
        log_directory: Option<String>,
    ) -> Self {
        let mut base = TuiBase::new(terminal, show_hardware_cursor, log_directory);
        let state = Rc::new(RefCell::new(RenderState::default()));
        base.set_lifecycle_hooks(Box::new(MainScreenHooks {
            state: Rc::clone(&state),
            terminal: Rc::clone(&base.terminal),
        }));
        TuiMainScreen {
            base,
            state,
            mouse_layout: None,
            detached_children: Vec::new(),
        }
    }

    /// 底层 [`TuiBase`]（overlay/焦点状态机、输入分发）。
    pub fn base(&self) -> &TuiBase {
        &self.base
    }

    /// 可变底层 [`TuiBase`]。
    pub fn base_mut(&mut self) -> &mut TuiBase {
        &mut self.base
    }

    /// 终端句柄（与 [`TuiBase::terminal`] 同一对象）。
    pub fn terminal(&self) -> &Rc<RefCell<Box<dyn Terminal>>> {
        &self.base.terminal
    }

    /// `fullRedraws`：全量重绘次数。每次进入 `full_render` 时自增（含首次渲染）。
    pub fn full_redraws(&self) -> u64 {
        self.base.full_redraws()
    }

    /// `setShowHardwareCursor(enabled)`：改变时立即请求一帧（TS `TuiBase` 的同名方法）。
    pub fn set_show_hardware_cursor(&mut self, enabled: bool) {
        self.base.set_show_hardware_cursor(enabled);
    }

    /// `getShowHardwareCursor()`。
    pub fn get_show_hardware_cursor(&self) -> bool {
        self.base.get_show_hardware_cursor()
    }

    /// `setClearOnShrink(enabled)`（不触发渲染）。
    pub fn set_clear_on_shrink(&mut self, enabled: bool) {
        self.base.set_clear_on_shrink(enabled);
    }

    /// `getClearOnShrink()`。
    pub fn get_clear_on_shrink(&self) -> bool {
        self.base.get_clear_on_shrink()
    }

    /// `captureRenderState()`：7 个字段的副本（`previousLines` 浅拷贝）。
    ///
    /// 调用方是 `kiana-interactive` 的模式切换：离开主屏前抓一份基准，回来后
    /// [`TuiMainScreen::restore_render_state`] 填进新渲染器，让下一帧沿用旧基准做差分
    /// （而不是从空白状态开始）。`previous_kitty_image_ids` 不在快照里，恢复后由下一帧
    /// 重新收集。
    pub fn capture_render_state(&self) -> TuiMainScreenRenderState {
        let state = self.state.borrow();
        TuiMainScreenRenderState {
            previous_lines: state.previous_lines.clone(),
            previous_width: state.previous_width,
            previous_height: state.previous_height,
            cursor_row: state.cursor_row,
            hardware_cursor_row: state.hardware_cursor_row,
            max_lines_rendered: state.max_lines_rendered,
            previous_viewport_top: state.previous_viewport_top,
        }
    }

    /// `restoreRenderState(state)`：图片行还原成空行、清空 kitty id 集合，其余直接赋值。
    ///
    /// 清空 `previous_kitty_image_ids` 后，下一帧不会尝试删除"上一帧的图片"；把图片行抹成
    /// 空行后，若该位置的新内容仍是图片（或任何非空内容），差分会把它算作变化行并重新输出。
    pub fn restore_render_state(&mut self, state: TuiMainScreenRenderState) {
        let mut current = self.state.borrow_mut();
        current.previous_lines = state
            .previous_lines
            .into_iter()
            .map(|line| {
                if is_image_line(&line) {
                    String::new()
                } else {
                    line
                }
            })
            .collect();
        current.previous_kitty_image_ids = IndexSet::new();
        current.previous_width = state.previous_width;
        current.previous_height = state.previous_height;
        current.cursor_row = state.cursor_row;
        current.hardware_cursor_row = state.hardware_cursor_row;
        current.max_lines_rendered = state.max_lines_rendered;
        current.previous_viewport_top = state.previous_viewport_top;
    }

    /// `resetRenderState()` 覆写：尺寸哨兵置 -1，其余清零（**不动** `previousKittyImageIds`）。
    ///
    /// `-1` 与任何真实终端宽度都不等，因此下一帧的 `width_changed` 必为真，直接走带清屏的
    /// 全量重绘（`render_now(true)` / `request_render(true)` 的强制语义）。不清图片 id 集合
    /// 意味着那次清屏前仍会先删除上一帧登记的图片。
    pub fn reset_render_state(&mut self) {
        let mut state = self.state.borrow_mut();
        state.previous_lines.clear();
        state.previous_width = RESET_SIZE;
        state.previous_height = RESET_SIZE;
        state.cursor_row = 0;
        state.hardware_cursor_row = 0;
        state.max_lines_rendered = 0;
        state.previous_viewport_top = 0;
    }

    /// `requestRender(force)`：`force` 时先清渲染缓存，再走立即渲染路径。
    ///
    /// `force == false` 只置脏位并合并到下一帧（`TuiBase::request_render` 的节流语义）；
    /// `force == true` 先 [`TuiMainScreen::reset_render_state`]，再请求立即渲染——因此下一帧
    /// 必是带清屏的全量重绘。
    pub fn request_render(&mut self, force: bool) {
        if force {
            self.reset_render_state();
        }
        self.base.request_render(force);
    }

    /// `renderNow(force)`：`force` 时先清渲染缓存，然后立刻渲染一帧。
    ///
    /// 不经过渲染决策：无论是否到节流时间都同步调用 [`TuiMainScreen::do_render`]（后者在
    /// `stopped` 后自动早退）。`force == true` 的语义同 [`TuiMainScreen::request_render`]。
    pub fn render_now(&mut self, force: bool) {
        if force {
            self.reset_render_state();
        }
        self.base.begin_render_now(false);
        self.do_render();
    }

    /// 宿主渲染循环的单步：到点（`RenderDecision::Now`）就渲染一帧，返回是否渲染。
    ///
    /// TS 的 `scheduleRender()` + `setTimeout` 回调在 Rust 里由宿主驱动；本方法把"取决策 +
    /// 渲染"合成一步（测试里的 `wait_for_render` 先读 [`TuiBase::render_decision`] 再渲染，
    /// 以区分 `Idle` 与 `Wait`）。
    ///
    /// `Idle`（没请求或已停止）与 `Wait`（节流窗口未到期）都返回 `false` 且不渲染；
    /// 决策为 `Now` 时由 `take_render_decision_now` 消费请求并记录渲染时刻。宿主
    /// `kiana-interactive` 每轮 `pump_events` 调用一次，不会自行重排定时器。
    pub fn flush_pending_render(&mut self) -> bool {
        if self.base.take_render_decision_now() {
            self.do_render();
            true
        } else {
            false
        }
    }

    /// `render(width)`（TS 继承自 `Container`）：按顺序拼接子项的行，并记录鼠标布局。
    ///
    /// 只做"子项渲染 + 拼接 + 记录 `mouse_layout`"：不解析伪光标、不合成 overlay、不补行尾
    /// reset（这些步骤在 [`TuiMainScreen::do_render`] 里）。[`Component::render`] 的实现与
    /// 测试也直接用它。
    ///
    /// 副作用：每次调用都会**替换** `mouse_layout`，并清空 `detached_children`——被
    /// [`Tui::remove_child`] 保活的组件只活到下一次渲染为止。
    pub fn render_children(&mut self, width: usize) -> Vec<String> {
        let mut lines = Vec::new();
        let mut layout = Vec::with_capacity(self.base.children.len());
        for child in &mut self.base.children {
            let identity = component_identity(child.as_ref());
            let child_lines = child.render(width);
            layout.push((identity, child_lines.len()));
            lines.extend(child_lines);
        }
        // TS `this.mouseLayout = { width, children }`：旧缓存（以及它持有的组件引用）在这里
        // 被整体替换；保活到本轮的已移除组件随之释放。
        self.detached_children.clear();
        self.mouse_layout = Some((width, layout));
        lines
    }

    // ------------------------------------------------------------------
    // Kitty 行辅助
    // ------------------------------------------------------------------

    /// `collectKittyImageIds(lines)`。
    fn collect_kitty_image_ids(lines: &[String]) -> IndexSet<u64> {
        let mut ids = IndexSet::new();
        for line in lines {
            for id in extract_kitty_image_ids(line) {
                ids.insert(id);
            }
        }
        ids
    }

    /// `deleteKittyImages(ids)`：按集合顺序拼接每张图片的删除序列。
    fn delete_kitty_images<'a>(ids: impl IntoIterator<Item = &'a u64>) -> String {
        let mut buffer = String::new();
        for id in ids {
            buffer.push_str(&delete_kitty_image(*id));
        }
        buffer
    }

    /// `getKittyImageReservedRows(lines, index, maxIndex = lines.length - 1)`。
    ///
    /// 返回图片行 `index` 需要预留的物理行数：从 `r=` 参数出发（缺省 1），逐行向下数，
    /// 遇到图片行或可见宽度非 0 的行就停止，同时不超过 `max_index` 与数组末尾。
    /// `r <= 1` 时直接返回 1。
    fn get_kitty_image_reserved_rows(
        &self,
        lines: &[String],
        index: usize,
        max_index: Option<usize>,
    ) -> usize {
        let rows = extract_kitty_image_rows(lines.get(index).map_or("", String::as_str)) as usize;
        if rows <= 1 {
            return 1;
        }

        let max_index = max_index.unwrap_or_else(|| lines.len().saturating_sub(1));
        let max_rows = rows
            .min(max_index.saturating_sub(index).saturating_add(1))
            .min(lines.len().saturating_sub(index));
        let mut reserved_rows = 1;
        while reserved_rows < max_rows {
            let line = lines.get(index + reserved_rows).map_or("", String::as_str);
            if is_image_line(line) || visible_width(line) > 0 {
                break;
            }
            reserved_rows += 1;
        }
        reserved_rows
    }

    /// `expandChangedRangeForKittyImages(firstChanged, lastChanged, newLines)`。
    ///
    /// 把差分区扩展到完整的 Kitty 图片块：`previous_lines` 与新行两侧都扫。判定与 TS 相同：
    /// 图片块起点 `i >= first_changed`，或图片块跨过首变化行（`i <= last_changed &&
    /// block_end >= first_changed`）时，`expanded_first` 取更小值、`expanded_last` 取
    /// `block_end`。注意前一个条件会把首变化行**之后的所有**图片块都并入区间。
    fn expand_changed_range_for_kitty_images(
        &self,
        first_changed: isize,
        last_changed: isize,
        new_lines: &[String],
    ) -> (isize, isize) {
        let mut expanded_first = first_changed;
        let mut expanded_last = last_changed;
        let mut expand_for_lines = |lines: &[String]| {
            for (i, line) in lines.iter().enumerate() {
                if extract_kitty_image_ids(line).is_empty() {
                    continue;
                }
                let i = i as isize;
                let block_end = i + self.get_kitty_image_reserved_rows(
                    lines,
                    i as usize,
                    Some(lines.len().saturating_sub(1)),
                ) as isize
                    - 1;
                if i >= first_changed || (i <= last_changed && block_end >= first_changed) {
                    expanded_first = expanded_first.min(i);
                    expanded_last = expanded_last.max(block_end);
                }
            }
        };
        expand_for_lines(&self.state.borrow().previous_lines);
        expand_for_lines(new_lines);
        (expanded_first, expanded_last)
    }

    /// `deleteChangedKittyImages(firstChanged, lastChanged)`。
    ///
    /// 收集**上一帧** `[first_changed, min(last_changed, previous_lines.len()-1)]` 区间内
    /// 所有图片 id 的删除序列，供重画前先释放旧图片（连同图片数据）。区间为空或越界时返回
    /// 空串。
    fn delete_changed_kitty_images(&self, first_changed: isize, last_changed: isize) -> String {
        if first_changed < 0 || last_changed < first_changed {
            return String::new();
        }
        let previous_lines = &self.state.borrow().previous_lines;
        let max_line = (last_changed as usize).min(previous_lines.len().saturating_sub(1));
        let mut ids = IndexSet::new();
        for i in first_changed as usize..=max_line {
            for id in extract_kitty_image_ids(previous_lines.get(i).map_or("", String::as_str)) {
                ids.insert(id);
            }
        }
        Self::delete_kitty_images(ids.iter())
    }

    // ------------------------------------------------------------------
    // 渲染
    // ------------------------------------------------------------------

    /// `positionHardwareCursor(cursorPos, totalLines)`：为 IME 候选窗定位硬件光标。
    ///
    /// 目标行夹到 `[0, total_lines - 1]`，列用绝对列（`\x1b[{col+1}G`，1 起）。没有光标
    /// 位置或 `total_lines == 0` 时只隐藏光标并返回。上下移动是相对状态里
    /// `hardware_cursor_row` 的行差，函数结束会把它更新为目标行；有定位目标时最终显示还是
    /// 隐藏由 `show_hardware_cursor` 决定，无目标时一律隐藏。
    fn position_hardware_cursor(&mut self, cursor_pos: Option<(usize, usize)>, total_lines: usize) {
        let Some((row, col)) = cursor_pos else {
            self.base.terminal.borrow_mut().hide_cursor();
            return;
        };
        if total_lines == 0 {
            self.base.terminal.borrow_mut().hide_cursor();
            return;
        }

        let target_row = row.min(total_lines - 1);
        let target_col = col;

        let hardware_cursor_row = self.state.borrow().hardware_cursor_row;
        let row_delta = target_row as isize - hardware_cursor_row as isize;
        let mut buffer = String::new();
        if row_delta > 0 {
            buffer.push_str(&format!("\x1b[{row_delta}B"));
        } else if row_delta < 0 {
            buffer.push_str(&format!("\x1b[{}A", -row_delta));
        }
        // 绝对列（1 起）。
        buffer.push_str(&format!("\x1b[{}G", target_col + 1));

        self.base.terminal.borrow_mut().write(&buffer);
        self.state.borrow_mut().hardware_cursor_row = target_row;
        if self.base.get_show_hardware_cursor() {
            self.base.terminal.borrow_mut().show_cursor();
        } else {
            self.base.terminal.borrow_mut().hide_cursor();
        }
    }

    /// 全量重绘（`fullRender(clear)`）：逐字输出顺序见 `spec/tui-core.md` §2.10.5。
    ///
    /// `clear == true` 时先删掉上一帧登记的 Kitty 图片，再输出清屏序列
    /// （`\x1b[2J` 清可视区、`\x1b[H` 回 home、`\x1b[3J` 清滚动缓冲区）；`clear == false`
    /// 只用于首次渲染（假定终端初始干净）。整帧包在同步更新序列里，并按 1 MiB 分块写出。
    fn full_render(
        &mut self,
        clear: bool,
        new_lines: &[String],
        cursor_pos: Option<(usize, usize)>,
        width: usize,
        height: usize,
    ) {
        self.base.full_redraw_count += 1;
        let terminal = Rc::clone(&self.base.terminal);
        let mut output = BoundedTerminalWriter::new(Box::new(move |data: &str| {
            terminal.borrow_mut().write(data);
        }));
        output.append(BEGIN_SYNCHRONIZED_OUTPUT);
        if clear {
            // 清屏前先按 id 删除上一帧登记的 Kitty 图片。
            output.append(&Self::delete_kitty_images(
                self.state.borrow().previous_kitty_image_ids.iter(),
            ));
            // 清屏、回 home、再清滚动缓冲区。
            output.append("\x1b[2J\x1b[H\x1b[3J");
        }
        let mut i = 0;
        while i < new_lines.len() {
            if i > 0 {
                output.append("\r\n");
            }
            let line = &new_lines[i];
            let is_image = is_image_line(line);
            let image_reserved_rows = if is_image {
                self.get_kitty_image_reserved_rows(new_lines, i, None)
            } else {
                1
            };
            if image_reserved_rows > 1 && image_reserved_rows <= height {
                // 多行图片：先下移 reserved-1 行占住空间，再把光标移回图片首行输出图片
                // 序列（图片本身覆盖这些行），最后移回底部，后续内容从图片下方继续。
                for _ in 1..image_reserved_rows {
                    output.append("\r\n");
                }
                output.append(&format!("\x1b[{}A", image_reserved_rows - 1));
                output.append(line);
                output.append(&format!("\x1b[{}B", image_reserved_rows - 1));
                i += image_reserved_rows - 1;
            } else {
                // 预留行数超过屏高时没有空间可腾，按单行输出（图片序列本身照常写出）。
                output.append(line);
            }
            i += 1;
        }
        output.append(END_SYNCHRONIZED_OUTPUT);
        output.flush();

        let mut state = self.state.borrow_mut();
        // 整帧输出结束时终端光标位于最后写出的行（空内容时没有输出，光标保持原位）。
        state.cursor_row = new_lines.len().saturating_sub(1);
        state.hardware_cursor_row = state.cursor_row;
        // 清屏时重置高水位，否则只记录增长。
        if clear {
            state.max_lines_rendered = new_lines.len();
        } else {
            state.max_lines_rendered = state.max_lines_rendered.max(new_lines.len());
        }
        let buffer_length = height.max(new_lines.len());
        state.previous_viewport_top = buffer_length.saturating_sub(height);
        drop(state);

        // 以内容末行为起点定位硬件光标；随后才把本帧内容记为下一帧的差分基准。
        self.position_hardware_cursor(cursor_pos, new_lines.len());

        let mut state = self.state.borrow_mut();
        state.previous_lines = new_lines.to_vec();
        state.previous_kitty_image_ids = Self::collect_kitty_image_ids(new_lines);
        state.previous_width = width as i64;
        state.previous_height = height as i64;
    }

    /// `logRedraw(reason)` 的路径解析（仅当 `KIANA_TUI_DEBUG_REDRAW === "1"` 且有日志目录）。
    fn redraw_log_path(&self) -> Option<PathBuf> {
        if !env_flag("KIANA_TUI_DEBUG_REDRAW") {
            return None;
        }
        let directory = self.base.log_directory.as_ref()?;
        // TS：`path.join(redrawLogDirectory, "kiana-tui-debug.log")`。
        Some(PathBuf::from(node_path_join(
            directory,
            "kiana-tui-debug.log",
        )))
    }

    /// `logRedraw(reason)`：追加一行 `[ISO] fullRender: reason (prev=, new=, height=)`。
    fn log_redraw(&self, log_path: Option<&Path>, reason: &str, new_len: usize, height: usize) {
        let Some(log_path) = log_path else {
            return;
        };
        let msg = format!(
            "[{}] fullRender: {} (prev={}, new={}, height={})\n",
            iso_timestamp(),
            reason,
            self.state.borrow().previous_lines.len(),
            new_len,
            height,
        );
        if let Some(parent) = log_path
            .parent()
            .filter(|path| !path.as_os_str().is_empty())
        {
            // TS `fs.mkdirSync(..., { recursive: true })` 不捕获错误：失败会抛出并中止本帧。
            if let Err(error) = std::fs::create_dir_all(parent) {
                panic_fs_error(&error, "mkdir", parent);
            }
        }
        let file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(log_path);
        // TS `fs.appendFileSync`：打开失败记 `open`，写入失败记 `write`。
        let mut file = match file {
            Ok(file) => file,
            Err(error) => panic_fs_error(&error, "open", log_path),
        };
        if let Err(error) = file.write_all(msg.as_bytes()) {
            panic_fs_error(&error, "write", log_path);
        }
    }

    /// 超宽行 crash 分支：写 crash 日志、`stop()`、按 TS 的逐字错误消息 panic。
    ///
    /// 触发条件是差分路径里非图片行的 `visible_width(line) > width`。顺序固定为：
    /// 先写 `kiana-tui-crash.log`（文件系统错误会先 panic，不会走到 `stop()`），再调用
    /// `stop()` 收拾终端，最后 panic 逐字错误消息。日志内容包含时间、终端宽度、越界行号与
    /// 可见宽度，以及全部已渲染行（每行标注 `visible_width`）。
    fn crash_line_exceeds_width(
        &mut self,
        index: usize,
        line: &str,
        new_lines: &[String],
        width: usize,
    ) -> ! {
        // TS：`path.join(this.logDirectory ?? os.tmpdir(), "kiana-tui-crash.log")`
        // （`??` 只在 null/undefined 时回退，空串是合法目录；`path.join` 会折叠 `.`/`..`
        // 与重复分隔符，因此错误消息里的路径也必须用同一份结果）。
        let crash_directory = match self.base.log_directory.as_deref() {
            Some(directory) => directory.to_string(),
            None => os_tmpdir().to_string(),
        };
        let crash_log_path = node_path_join(&crash_directory, "kiana-tui-crash.log");
        let line_width = visible_width(line);
        let mut crash_data = String::new();
        crash_data.push_str(&format!("Crash at {}\n", iso_timestamp()));
        crash_data.push_str(&format!("Terminal width: {width}\n"));
        crash_data.push_str(&format!("Line {index} visible width: {line_width}\n"));
        crash_data.push('\n');
        crash_data.push_str("=== All rendered lines ===\n");
        for (idx, rendered) in new_lines.iter().enumerate() {
            if idx > 0 {
                crash_data.push('\n');
            }
            crash_data.push_str(&format!(
                "[{idx}] (w={}) {rendered}",
                visible_width(rendered)
            ));
        }
        // TS 在数组末尾追加 `""` 再 `join("\n")`：只贡献最后一行之后的**一个**换行。
        crash_data.push('\n');
        if let Some(parent) = Path::new(&crash_log_path).parent() {
            // TS `fs.mkdirSync` / `fs.writeFileSync` 不捕获错误：失败时先抛出文件系统错误
            // （而不是下面那行超宽错误），也不会走到 `stop()`。
            if let Err(error) = std::fs::create_dir_all(parent) {
                panic_fs_error(&error, "mkdir", parent);
            }
        }
        if let Err(error) = std::fs::write(&crash_log_path, crash_data) {
            panic_fs_error(&error, "open", Path::new(&crash_log_path));
        }

        // 抛出之前先收拾终端状态。
        self.base.stop(TuiStopOptions::default());

        let error_msg = [
            format!("Rendered line {index} exceeds terminal width ({line_width} > {width})."),
            String::new(),
            "This is likely caused by a custom TUI component not truncating its output."
                .to_string(),
            "Use visibleWidth() to measure and truncateToWidth() to truncate lines.".to_string(),
            String::new(),
            format!("Debug log written to: {crash_log_path}"),
        ]
        .join("\n");
        panic!("{error_msg}");
    }

    /// `doRender()`：主屏差分渲染（`spec/tui-core.md` §2.10.5 的逐句算法）。
    ///
    /// 阶段划分（`// 阶段 N` 标记与代码一一对应）：
    ///
    /// 1. 早退检查、焦点标志刷新、读尺寸并推算视口。
    /// 2. 文本管线：渲染子项 → 伪光标 → overlay → 提取光标 → 行尾 reset。
    /// 3. 全量重绘判定（阶段 3 内 4 个分支，按固定优先级：首次渲染、宽度变化、高度变化、
    ///    `clearOnShrink`；另有 4 类在阶段 4/5 触发）。
    /// 4. 差分区间计算与 Kitty 图片块扩展（无变化 / 全删除就在这里返回；全删除分支里的
    ///    "删除行导致视口上移"与"待清行数超过屏高"、以及差分区间算完后的"首个变化行在
    ///    视口上方"会退回全量重绘）。
    /// 5. 差分输出；必要时先滚屏，再重写 `first_changed..=render_end`（"Kitty 图片预清
    ///    会滚屏"在这里退回全量重绘）。
    /// 6. 回写光标行/视口顶等字段 → 定位硬件光标 → 覆盖 `previous_lines`、图片 id 与尺寸。
    ///
    /// 失败路径：超宽非图片行走 `crash_line_exceeds_width`（panic）；调试日志的文件系统
    /// 错误也以 panic 中止本帧。正常路径没有 `Result` 返回。
    pub fn do_render(&mut self) {
        if self.base.stopped.get() {
            return;
        }
        // Overlay handle（`hide()`/`setHidden()`/`unfocus()`/`focus()`）只改共享的焦点状态并
        // 置脏位；TS 里这些方法**同步**写 `component.focused`，所以紧随其后的这一帧必须看到
        // 新的焦点标志（`CURSOR_MARKER` 的产出、硬件光标定位都取决于它）。刷新必须发生在
        // `render_children` **之前**——`composite_overlays` 里的那次刷新在子项渲染之后，而且
        // overlay 栈为空时根本不会执行。
        self.base.flush_pending_focus_sync();
        // 阶段 1：读终端尺寸并推算视口。`previous_*` 为 0 表示从未渲染（不算尺寸变化）；
        // 高度变化时用"上一帧视口顶 + 上一帧高度"还原缓冲区总长，再按新高度重新对齐视口顶。
        let width = self.base.terminal.borrow().columns();
        let height = self.base.terminal.borrow().rows();
        let (previous_width, previous_height, previous_viewport_top) = {
            let state = self.state.borrow();
            (
                state.previous_width,
                state.previous_height,
                state.previous_viewport_top,
            )
        };
        let width_changed = previous_width != 0 && previous_width != width as i64;
        let height_changed = previous_height != 0 && previous_height != height as i64;
        let previous_buffer_length = if previous_height > 0 {
            previous_viewport_top.saturating_add(previous_height as usize)
        } else {
            height
        };
        let mut prev_viewport_top = if height_changed {
            previous_buffer_length.saturating_sub(height)
        } else {
            previous_viewport_top
        };
        let mut viewport_top = prev_viewport_top;
        let mut hardware_cursor_row = self.state.borrow().hardware_cursor_row;

        // 阶段 2：文本管线。渲染所有组件拿到新行；先解析伪光标，让 overlay 与行切分只看到
        // 普通 SGR。
        let rendered = self.render_children(width);
        let mut new_lines = self.base.resolve_fake_cursors(rendered);

        // overlay 合成（在差分比较之前）。
        if self.base.has_overlay_entries() {
            new_lines = self.base.composite_overlays(new_lines, width, height);
        }

        // 光标位置必须在补行尾 reset 之前提取（marker 要能被找到）。
        let cursor_pos = self.base.extract_cursor_position(&mut new_lines, height);

        new_lines = self.base.apply_line_resets(new_lines);

        let redraw_log_path = self.redraw_log_path();
        let new_len = new_lines.len();

        // 阶段 3：全量重绘判定，顺序即优先级（与 TS 的分支表一致，不能调换）。
        // 首次渲染：直接全量输出，不清屏（假定屏幕是干净的）。
        if self.state.borrow().previous_lines.is_empty() && !width_changed && !height_changed {
            self.log_redraw(redraw_log_path.as_deref(), "first render", new_len, height);
            self.full_render(false, &new_lines, cursor_pos, width, height);
            return;
        }

        // 宽度变化必须全量重绘（换行会变）。
        if width_changed {
            self.log_redraw(
                redraw_log_path.as_deref(),
                &format!("terminal width changed ({previous_width} -> {width})"),
                new_len,
                height,
            );
            self.full_render(true, &new_lines, cursor_pos, width, height);
            return;
        }

        // 高度变化通常也要全量重绘，但 Termux 的软键盘会改高度：全量重绘会让历史反复回放。
        if height_changed && !is_termux_session() {
            self.log_redraw(
                redraw_log_path.as_deref(),
                &format!("terminal height changed ({previous_height} -> {height})"),
                new_len,
                height,
            );
            self.full_render(true, &new_lines, cursor_pos, width, height);
            return;
        }

        // 内容收缩到工作区以下且没有 overlay：重绘以清掉空行（overlay 需要那片 padding）。
        if self.base.get_clear_on_shrink()
            && new_lines.len() < self.state.borrow().max_lines_rendered
            && !self.base.has_overlay_entries()
        {
            let max_lines_rendered = self.state.borrow().max_lines_rendered;
            self.log_redraw(
                redraw_log_path.as_deref(),
                &format!("clearOnShrink (maxLinesRendered={max_lines_rendered})"),
                new_len,
                height,
            );
            self.full_render(true, &new_lines, cursor_pos, width, height);
            return;
        }

        // 阶段 4：行级差分。按两侧较长的长度逐行比较，缺失的行按空串处理。
        // 找首/末变化行。
        let previous_len = self.state.borrow().previous_lines.len();
        let mut first_changed: isize = -1;
        let mut last_changed: isize = -1;
        let max_lines = new_lines.len().max(previous_len);
        for i in 0..max_lines {
            let old_line = if i < previous_len {
                self.state.borrow().previous_lines[i].clone()
            } else {
                String::new()
            };
            let new_line = if i < new_lines.len() {
                new_lines[i].clone()
            } else {
                String::new()
            };
            if old_line != new_line {
                if first_changed == -1 {
                    first_changed = i as isize;
                }
                last_changed = i as isize;
            }
        }
        // 尾部追加一定算变化：新增的空串与 old="" 比较不出差异，因此这里显式把区间推到末尾。
        let appended_lines = new_lines.len() > previous_len;
        if appended_lines {
            if first_changed == -1 {
                first_changed = previous_len as isize;
            }
            last_changed = new_lines.len() as isize - 1;
        }
        if first_changed != -1 {
            let (expanded_first, expanded_last) =
                self.expand_changed_range_for_kitty_images(first_changed, last_changed, &new_lines);
            first_changed = expanded_first;
            last_changed = expanded_last;
        }
        // appendStart：只在尾部追加且上一帧非空时，移动目标是旧内容最后一行，随后用 CRLF
        // 进入新行；其余情况定位到 firstChanged 并用 CR 回到行首。
        let append_start =
            appended_lines && first_changed == previous_len as isize && first_changed > 0;

        // 没有变化：仍要更新硬件光标位置（它可能移动了）。
        if first_changed == -1 {
            self.position_hardware_cursor(cursor_pos, new_lines.len());
            let mut state = self.state.borrow_mut();
            state.previous_viewport_top = prev_viewport_top;
            state.previous_height = height as i64;
            return;
        }

        // 变化全在已删除的行里（没有要渲染的内容，只需要清行）。这里创建的 writer 若在
        // 下面改走 full_render，会在未 flush 的情况下被丢弃——全量重绘会重写整屏。
        if first_changed >= new_lines.len() as isize {
            if previous_len > new_lines.len() {
                let terminal = Rc::clone(&self.base.terminal);
                let mut output = BoundedTerminalWriter::new(Box::new(move |data: &str| {
                    terminal.borrow_mut().write(data);
                }));
                output.append(BEGIN_SYNCHRONIZED_OUTPUT);
                output.append(&self.delete_changed_kitty_images(first_changed, last_changed));
                // 移到新内容末尾（空内容时夹到 0）。
                let target_row = new_lines.len().saturating_sub(1);
                if target_row < prev_viewport_top {
                    self.log_redraw(
                        redraw_log_path.as_deref(),
                        &format!(
                            "deleted lines moved viewport up ({target_row} < {prev_viewport_top})"
                        ),
                        new_len,
                        height,
                    );
                    self.full_render(true, &new_lines, cursor_pos, width, height);
                    return;
                }
                let line_diff = compute_line_diff(
                    target_row,
                    viewport_top,
                    hardware_cursor_row,
                    prev_viewport_top,
                );
                if line_diff > 0 {
                    output.append(&format!("\x1b[{line_diff}B"));
                } else if line_diff < 0 {
                    output.append(&format!("\x1b[{}A", -line_diff));
                }
                output.append("\r");
                // 不滚动地清掉多余行。
                let extra_lines = previous_len - new_lines.len();
                if extra_lines > height {
                    self.log_redraw(
                        redraw_log_path.as_deref(),
                        &format!("extraLines > height ({extra_lines} > {height})"),
                        new_len,
                        height,
                    );
                    self.full_render(true, &new_lines, cursor_pos, width, height);
                    return;
                }
                let clear_start_offset = if new_lines.is_empty() { 0 } else { 1 };
                if extra_lines > 0 && clear_start_offset > 0 {
                    output.append(&format!("\x1b[{clear_start_offset}B"));
                }
                for i in 0..extra_lines {
                    output.append("\r\x1b[2K");
                    if i < extra_lines - 1 {
                        output.append("\x1b[1B");
                    }
                }
                let move_back = (extra_lines as isize - 1 + clear_start_offset as isize).max(0);
                if move_back > 0 {
                    output.append(&format!("\x1b[{move_back}A"));
                }
                output.append(END_SYNCHRONIZED_OUTPUT);
                output.flush();
                let mut state = self.state.borrow_mut();
                state.cursor_row = target_row;
                state.hardware_cursor_row = target_row;
            }
            self.position_hardware_cursor(cursor_pos, new_lines.len());
            let mut state = self.state.borrow_mut();
            state.previous_lines = new_lines;
            let image_ids = Self::collect_kitty_image_ids(&state.previous_lines);
            state.previous_kitty_image_ids = image_ids;
            state.previous_width = width as i64;
            state.previous_height = height as i64;
            state.previous_viewport_top = prev_viewport_top;
            return;
        }

        // 差分渲染只能触及真正可见的区域：首个变化行在上一轮视口之上时必须全量重绘。
        if (first_changed as usize) < prev_viewport_top {
            self.log_redraw(
                redraw_log_path.as_deref(),
                &format!("firstChanged < viewportTop ({first_changed} < {prev_viewport_top})"),
                new_len,
                height,
            );
            self.full_render(true, &new_lines, cursor_pos, width, height);
            return;
        }

        // 阶段 5：从首个变化行渲染到最后一个变化行（`render_end`）；更新保持在同步输出内，
        // 按块写出。
        // 若目标行在上一帧视口底之下，只能靠 CRLF 滚屏把它带进视口；每滚一行，
        // prev_viewport_top 与 viewport_top 同步下移，hardware_cursor_row 直接记为滚到的目标行。
        let terminal = Rc::clone(&self.base.terminal);
        let mut output = BoundedTerminalWriter::new(Box::new(move |data: &str| {
            terminal.borrow_mut().write(data);
        }));
        output.append(BEGIN_SYNCHRONIZED_OUTPUT);
        output.append(&self.delete_changed_kitty_images(first_changed, last_changed));
        let prev_viewport_bottom = prev_viewport_top + height - 1;
        let move_target_row = if append_start {
            (first_changed - 1) as usize
        } else {
            first_changed as usize
        };
        if move_target_row > prev_viewport_bottom {
            let current_screen_row = (hardware_cursor_row as isize - prev_viewport_top as isize)
                .clamp(0, height as isize - 1) as usize;
            let move_to_bottom = height - 1 - current_screen_row;
            if move_to_bottom > 0 {
                output.append(&format!("\x1b[{move_to_bottom}B"));
            }
            let scroll = move_target_row - prev_viewport_bottom;
            for _ in 0..scroll {
                output.append("\r\n");
            }
            prev_viewport_top += scroll;
            viewport_top += scroll;
            hardware_cursor_row = move_target_row;
        }

        // 移到首个变化行（用 hardwareCursorRow 表示的实际位置）：目标行与当前光标行都换算成
        // "相对各自视口顶"的屏幕行，再求行差（正数向下、负数向上）。
        let line_diff = compute_line_diff(
            move_target_row,
            viewport_top,
            hardware_cursor_row,
            prev_viewport_top,
        );
        if line_diff > 0 {
            output.append(&format!("\x1b[{line_diff}B"));
        } else if line_diff < 0 {
            output.append(&format!("\x1b[{}A", -line_diff));
        }

        // 移到第 0 列。
        output.append(if append_start { "\r\n" } else { "\r" });

        // 只渲染变化行（firstChanged..=renderEnd），减少单行变化（如 spinner）时的闪烁。
        let render_end = (last_changed as usize).min(new_lines.len() - 1);
        let mut i = first_changed as usize;
        while i <= render_end {
            if i > first_changed as usize {
                output.append("\r\n");
            }
            let line = new_lines[i].clone();
            let image = is_image_line(&line);
            let image_reserved_rows = if image {
                self.get_kitty_image_reserved_rows(&new_lines, i, Some(render_end))
            } else {
                1
            };
            if image_reserved_rows > 1 {
                // 预留行会越过视口底部：继续输出会触发滚屏、破坏后续行坐标，改走全量重绘。
                let image_start_screen_row = i as isize - viewport_top as isize;
                if image_start_screen_row < 0
                    || image_start_screen_row + image_reserved_rows as isize > height as isize
                {
                    self.log_redraw(
                        redraw_log_path.as_deref(),
                        &format!(
                            "kitty image pre-clear would scroll ({image_start_screen_row} + {image_reserved_rows} > {height})"
                        ),
                        new_len,
                        height,
                    );
                    self.full_render(true, &new_lines, cursor_pos, width, height);
                    return;
                }

                output.append("\x1b[2K");
                for _ in 1..image_reserved_rows {
                    output.append("\r\n\x1b[2K");
                }
                output.append(&format!("\x1b[{}A", image_reserved_rows - 1));
                output.append(&line);
                output.append(&format!("\x1b[{}B", image_reserved_rows - 1));
                i += image_reserved_rows - 1;
            } else {
                output.append("\x1b[2K");
                if !image && visible_width(&line) > width {
                    self.crash_line_exceeds_width(i, &line, &new_lines, width);
                }
                output.append(&line);
            }
            i += 1;
        }

        // 记录渲染结束时的光标行。
        let mut final_cursor_row = render_end as isize;

        // 之前有更多行：清掉它们并把光标移回去。
        if previous_len > new_lines.len() {
            if (render_end as isize) < new_lines.len() as isize - 1 {
                let move_down = new_lines.len() - 1 - render_end;
                output.append(&format!("\x1b[{move_down}B"));
                final_cursor_row = new_lines.len() as isize - 1;
            }
            let extra_lines = previous_len - new_lines.len();
            for _ in new_lines.len()..previous_len {
                output.append("\r\n\x1b[2K");
            }
            output.append(&format!("\x1b[{extra_lines}A"));
        }

        output.append(END_SYNCHRONIZED_OUTPUT);

        if env_flag("KIANA_TUI_DEBUG") {
            write_debug_dump(
                &new_lines,
                &self.state.borrow().previous_lines,
                &DebugDump {
                    first_changed,
                    viewport_top,
                    cursor_row: self.state.borrow().cursor_row,
                    height,
                    line_diff,
                    hardware_cursor_row,
                    render_end,
                    final_cursor_row,
                    cursor_pos,
                    written_chars: output.length(),
                },
            );
        }

        output.flush();

        // 阶段 6：记录下一轮的光标位置：
        // cursorRow 是内容末尾的下标（只供状态快照与调试转储，不参与视口计算），
        // hardwareCursorRow 是终端的实际光标位置。
        let mut state = self.state.borrow_mut();
        state.cursor_row = new_lines.len().saturating_sub(1);
        state.hardware_cursor_row = final_cursor_row.max(0) as usize;
        state.max_lines_rendered = state.max_lines_rendered.max(new_lines.len());
        // 视口顶至少要让最后的硬件光标行仍在视口内（finalCursorRow - height + 1）。
        let viewport_floor = final_cursor_row - height as isize + 1;
        state.previous_viewport_top = prev_viewport_top.max(viewport_floor.max(0) as usize);
        drop(state);

        // 为 IME 定位硬件光标（用刚刚写回的 hardware_cursor_row 作起点），之后才覆盖
        // previous_lines。
        self.position_hardware_cursor(cursor_pos, new_lines.len());

        let mut state = self.state.borrow_mut();
        state.previous_lines = new_lines;
        let image_ids = Self::collect_kitty_image_ids(&state.previous_lines);
        state.previous_kitty_image_ids = image_ids;
        state.previous_width = width as i64;
        state.previous_height = height as i64;
    }
}

/// `computeLineDiff(targetRow)`：目标行与当前屏幕行的差（正数向下）。
fn compute_line_diff(
    target_row: usize,
    viewport_top: usize,
    hardware_cursor_row: usize,
    prev_viewport_top: usize,
) -> isize {
    let current_screen_row = hardware_cursor_row as isize - prev_viewport_top as isize;
    let target_screen_row = target_row as isize - viewport_top as isize;
    target_screen_row - current_screen_row
}

/// 组件的稳定身份：胖指针的（数据指针, vtable 指针）。
///
/// TS 的 `mouseLayout` 存组件引用，按引用相等匹配；Rust 里：
/// - 数据指针区分不同的 `Box` 分配，`Box` 在 `Vec` 里换位不影响它；
/// - vtable 指针区分**不同的零大小组件类型**（`Box::new(Zst)` 的数据指针都是
///   `NonNull::dangling()`，只有 vtable 不同）。同一类型的两个零大小实例无法区分，
///   但零大小组件没有实例状态，对其中任意一个分发在可观察行为上相同。
type ComponentIdentity = (usize, usize);

fn component_identity(component: &dyn Component) -> ComponentIdentity {
    let raw: *const dyn Component = component;
    // SAFETY: `*const dyn Component` 与 `[usize; 2]` 在支持的平台（64 位）上同宽同布局：
    // 第一个字是数据指针，第二个字是 vtable 指针。
    let [data, vtable]: [usize; 2] = unsafe { std::mem::transmute(raw) };
    (data, vtable)
}

// ============================================================================
// Component / Tui 实现
// ============================================================================

impl Component for TuiMainScreen {
    /// 等价于 [`TuiMainScreen::render_children`]：只拼接子项，不合成 overlay、不解析光标。
    fn render(&mut self, width: usize) -> Vec<String> {
        self.render_children(width)
    }

    fn handle_input(&mut self, data: &str) {
        self.base.handle_terminal_input(data);
    }

    /// TS `TuiBase` 继承 `Container.handleMouse`：按子项渲染高度分发，命中后转发。
    ///
    /// 缓存按**组件身份**（`component_identity`）而不是槽位下标索引：`addChild` 插入/重排
    /// 之后、下一次渲染之前的事件仍会路由给原来的子项。命中项已被 `removeChild` 移出
    /// `children` 时，仍像 TS 一样把它交给缓存里的（由 `detached_children` 保活的）组件；
    /// 唯一例外是"被删子树正持有焦点"——那个 `Box` 由 base 的脱离焦点槽接管，不在
    /// `detached_children` 里，这类命中会视为未命中（见 [`Tui::remove_child`] 的说明）。
    /// TS 的 `TuiBase` **没有** `handleInput`（`"handleInput" in tui === false`），因此
    /// `Container.handleMouse` 里"把焦点收回给自己"的分支不成立（`tui.ts:383-403`）。
    ///
    /// 命中测试的上界是 `event.height`；缓存宽度与 `event.width` 不一致时会现场渲染每个
    /// 子项求高度（与 TS 相同，**不**写回 `mouse_layout`，下一次正式渲染才更新缓存）。
    /// 命中子项的局部事件只改 `y` 与 `height`，`x`/`screen_x`/`screen_y` 原样传递。
    fn handle_mouse(&mut self, event: &TuiMouseEvent) -> Option<TuiMouseEventResult> {
        if event.y < 0 || event.y as usize >= event.height {
            return None;
        }
        // 宽度未变时复用上一轮布局；否则现场渲染求高度（与 TS 相同）。
        let layout: Vec<(ComponentIdentity, usize)> = match &self.mouse_layout {
            Some((width, layout)) if *width == event.width => layout.clone(),
            _ => self
                .base
                .children
                .iter_mut()
                .map(|child| {
                    let identity = component_identity(child.as_ref());
                    (identity, child.render(event.width).len())
                })
                .collect(),
        };
        let mut child_y: isize = 0;
        for (identity, height) in layout {
            if event.y >= child_y && event.y < child_y + height as isize {
                let mut child_event = event.clone();
                child_event.y = event.y - child_y;
                child_event.height = height;
                if let Some(index) = self
                    .base
                    .children
                    .iter()
                    .position(|child| component_identity(child.as_ref()) == identity)
                {
                    return crate::tui::dispatch_mouse_event(
                        self.base.children[index].as_mut(),
                        &child_event,
                    );
                }
                if let Some(index) = self
                    .detached_children
                    .iter()
                    .position(|child| component_identity(child.as_ref()) == identity)
                {
                    // TS 的 `mouseLayout` 持有引用，所以命中已移除组件时仍会调用它的
                    // `handleMouse`（返回值/`focusTarget` 照常传出）。`detached_children`
                    // 让这一调用的对象在 Rust 里也还活着。
                    return crate::tui::dispatch_mouse_event(
                        self.detached_children[index].as_mut(),
                        &child_event,
                    );
                }
                // 组件是被 `Tui::children()` 裸改 `Vec` 移除的（没有经过 remove_child，
                // 无法保活）：没有可交付的对象，视为未命中并继续向下找。
                child_y += height as isize;
                continue;
            }
            child_y += height as isize;
        }
        None
    }

    fn invalidate(&mut self) {
        // 与 TS `Container.invalidate` 一致：只向下传递，**不动** `mouseLayout`
        // （`tui.ts:377-381`）。
        self.base.invalidate_all();
    }

    /// TS 的 `TuiMainScreen` **没有** `handleInput`（输入经 `TuiBase.handleTerminalInput`），
    /// 因此 `Container.handleMouse` 不把它当作"转发型容器"，也不收回子控件的焦点目标。
    fn handles_input(&self) -> bool {
        false
    }

    /// 向下转型：`Tui` trait 不暴露宿主终端，扩展 UI 层用它取回 [`TuiMainScreen::terminal`]
    /// （`tui.terminal`）。
    fn as_any(&self) -> Option<&dyn std::any::Any> {
        Some(self)
    }

    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }
}

impl Tui for TuiMainScreen {
    /// 主屏渲染器固定为 `TuiMode::Regular`（全屏是 `TuiAltScreen` 的 `Fullscreen`）。
    fn mode(&self) -> TuiMode {
        TuiMode::Regular
    }

    /// 直接暴露底层 `Vec`。**裸改**（`push`/`remove`/重排）不会刷新 `TuiBase` 的挂载快照，
    /// 被移除的组件也不会进入 `detached_children` 保活列表，并且会让 `mouse_layout` 与真实
    /// 子项错位；结构化增删请走 [`Tui::add_child`] / [`Tui::remove_child`] /
    /// [`Tui::remove_child_at`]。
    fn children(&mut self) -> &mut Vec<Box<dyn Component>> {
        &mut self.base.children
    }

    fn add_child(&mut self, child: Box<dyn Component>) {
        self.base.add_child(child);
    }

    /// `removeChild(component)`：把组件移出 `children`，但**保活**到下一次渲染。
    ///
    /// TS 的 `Container.removeChild` 只 `splice` 数组；`mouseLayout` 仍持有引用，所以下一次
    /// 渲染之前到达的鼠标事件还能命中它。Rust 用 `TuiMainScreen::detached_children` 保存
    /// 被移除的 `Box`，复现同一语义（并防止堆地址被复用后误路由）。
    ///
    /// 例外：被删子树**正持有焦点**时，[`TuiBase::remove_child_at`] 返回 `None` 并把这棵
    /// 子树留作 base 的"脱离焦点"（按键仍能投递），因此它不在 `detached_children` 里，
    /// 之后到达的鼠标事件也找不到它（`handle_mouse` 只查 `children` 与
    /// `detached_children`）。找不到匹配项时本方法为 no-op。
    fn remove_child(&mut self, child: &dyn Component) {
        let identity = component_identity(child);
        let index = self
            .base
            .children
            .iter()
            .position(|candidate| component_identity(candidate.as_ref()) == identity);
        if let Some(index) = index
            && let Some(removed) = self.base.remove_child_at(index)
        {
            self.detached_children.push(removed);
        }
    }

    /// 按下标移除（[`Tui::remove_child_at`]）：与 [`Tui::remove_child`] 相同的焦点/子项 id
    /// 维护，但把组件交还给调用方（扩展 UI 用它摘下 JS 组件并释放引用）。
    ///
    /// 返回 `None` 的两种情形：下标越界；或该子树正持有焦点——此时组件被留在 base 的
    /// 脱离焦点槽里（要继续接收按键），调用方拿不到所有权。
    fn remove_child_at(&mut self, index: usize) -> Option<Box<dyn Component>> {
        self.base.remove_child_at(index)
    }

    /// `clear()`：逐个 `remove_child_at(0)` 移出并保活，与 TS 一样**不动** `mouseLayout`
    /// （缓存里的组件继续保活到下一次渲染）。
    ///
    /// 循环次数取清空前的子项数，而不是以 `Option` 为继续条件：`TuiBase::remove_child_at`
    /// 在子树持有焦点时会把该子树移到 `DetachedFocus` 并返回 `None`，但**已经**把它从
    /// `children` 里移除；若此时退出循环，剩余子项会留在 `children` 里。按固定次数继续
    /// 才能复现 TS 的 `children = []`。
    fn clear_children(&mut self) {
        let remaining = self.base.children.len();
        for _ in 0..remaining {
            // 聚焦子树由 remove_child_at 移入 DetachedFocus（继续接收按键），返回 None；
            // 它已经从 children 移除，继续循环即可。
            if let Some(child) = self.base.remove_child_at(0) {
                self.detached_children.push(child);
            }
        }
    }

    fn focus(&self) -> Option<FocusTarget> {
        self.base.focused_component()
    }

    fn set_focus(&mut self, target: Option<FocusTarget>) {
        self.base.set_focus(target);
    }

    fn show_overlay(
        &mut self,
        component: Box<dyn Component>,
        options: OverlayOptions,
    ) -> Box<dyn OverlayHandle> {
        self.base.show_overlay(component, options)
    }

    fn hide_overlay(&mut self) {
        self.base.hide_overlay();
    }

    fn has_overlay(&self) -> bool {
        self.base.has_overlay()
    }

    fn start(&mut self) {
        self.base.start();
    }

    fn stop(&mut self, options: TuiStopOptions) {
        self.base.stop(options);
    }

    fn render_now(&mut self, force: bool) {
        TuiMainScreen::render_now(self, force);
    }

    fn request_render(&mut self, force: bool) {
        TuiMainScreen::request_render(self, force);
    }

    fn add_input_listener(&mut self, listener: TuiInputListener) -> InputListenerId {
        self.base.add_input_listener(listener)
    }

    fn remove_input_listener(&mut self, id: InputListenerId) {
        self.base.remove_input_listener(id);
    }

    fn query_terminal_colors(
        &mut self,
        timeout_ms: u64,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = crate::terminal_colors::TerminalColors> + '_>,
    > {
        self.base.query_terminal_colors(timeout_ms)
    }
}

// ============================================================================
// 调试/时间辅助
// ============================================================================

/// `KIANA_TUI_DEBUG === "1"` 时写 `/tmp/tui/render-<Date.now()>-<random>.log`。
fn write_debug_dump(new_lines: &[String], previous_lines: &[String], dump: &DebugDump) {
    let debug_dir = PathBuf::from("/tmp/tui");
    // TS 不捕获 `fs.mkdirSync` / `fs.writeFileSync` 的错误：失败会抛出并中止本帧。
    if let Err(error) = std::fs::create_dir_all(&debug_dir) {
        panic_fs_error(&error, "mkdir", &debug_dir);
    }
    let debug_path = debug_dir.join(format!("render-{}-{}.log", now_millis(), random_base36()));
    let cursor_pos_text = match dump.cursor_pos {
        Some((row, col)) => format!("{{\"row\":{row},\"col\":{col}}}"),
        None => "null".to_string(),
    };
    let lines_json = |lines: &[String]| {
        let value = serde_json::Value::Array(
            lines
                .iter()
                .map(|line| serde_json::Value::String(line.clone()))
                .collect(),
        );
        kiana_json::stringify_pretty(&value, 2)
    };
    let debug_data = [
        format!("firstChanged: {}", dump.first_changed),
        format!("viewportTop: {}", dump.viewport_top),
        format!("cursorRow: {}", dump.cursor_row),
        format!("height: {}", dump.height),
        format!("lineDiff: {}", dump.line_diff),
        format!("hardwareCursorRow: {}", dump.hardware_cursor_row),
        format!("renderEnd: {}", dump.render_end),
        format!("finalCursorRow: {}", dump.final_cursor_row),
        format!("cursorPos: {cursor_pos_text}"),
        format!("newLines.length: {}", new_lines.len()),
        format!("previousLines.length: {}", previous_lines.len()),
        String::new(),
        "=== newLines ===".to_string(),
        lines_json(new_lines),
        String::new(),
        "=== previousLines ===".to_string(),
        lines_json(previous_lines),
        String::new(),
        "=== buffer ===".to_string(),
        format!("[{} chars written in bounded chunks]", dump.written_chars),
    ]
    .join("\n");
    if let Err(error) = std::fs::write(&debug_path, debug_data) {
        panic_fs_error(&error, "open", &debug_path);
    }
}

/// 文件系统错误 → Node `fs` 同步 API 抛出的文本：`CODE: message, syscall 'path'`。
///
/// TS 的 `fs.mkdirSync` / `fs.appendFileSync` / `fs.writeFileSync` 都不捕获错误，失败会从
/// `doRender()` 抛出并中止本帧。Rust 的 [`TuiMainScreen::do_render`] 没有错误通道，因此用
/// 同样语义的 `panic!` 中止（见模块文档的"已知偏差"）。
///
/// 只映射常见的 [`std::io::ErrorKind`]；其他种类直接 panic 该错误的 `Display` 文本，
/// 此时文本形式与 Node 不同（不是 `CODE: message, syscall` 格式）。
fn panic_fs_error(error: &std::io::Error, syscall: &str, path: &Path) -> ! {
    let (code, message) = match error.kind() {
        std::io::ErrorKind::NotFound => ("ENOENT", "no such file or directory"),
        std::io::ErrorKind::PermissionDenied => ("EACCES", "permission denied"),
        std::io::ErrorKind::AlreadyExists => ("EEXIST", "file already exists"),
        std::io::ErrorKind::IsADirectory => ("EISDIR", "illegal operation on a directory"),
        std::io::ErrorKind::NotADirectory => ("ENOTDIR", "not a directory"),
        std::io::ErrorKind::ReadOnlyFilesystem => ("EROFS", "read-only file system"),
        std::io::ErrorKind::StorageFull => ("ENOSPC", "no space left on device"),
        _ => {
            panic!("{error}");
        }
    };
    panic!("{code}: {message}, {syscall} '{}'", path.to_string_lossy());
}

/// `KIANA_TUI_DEBUG` 日志用到的渲染中间量。
struct DebugDump {
    first_changed: isize,
    viewport_top: usize,
    cursor_row: usize,
    height: usize,
    line_diff: isize,
    hardware_cursor_row: usize,
    render_end: usize,
    final_cursor_row: isize,
    cursor_pos: Option<(usize, usize)>,
    written_chars: usize,
}

/// `Date.now()`。
fn now_millis() -> i64 {
    match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(duration) => duration.as_millis() as i64,
        Err(error) => -(error.duration().as_millis() as i64),
    }
}

/// `Math.random().toString(36).slice(2)` 的等价物（仅用于调试文件名）。
fn random_base36() -> String {
    const DIGITS: &[u8; 36] = b"0123456789abcdefghijklmnopqrstuvwxyz";
    let mut bytes = [0u8; 10];
    if getrandom::fill(&mut bytes).is_err() {
        let nanos = now_millis();
        for (index, byte) in bytes.iter_mut().enumerate() {
            *byte = ((nanos >> (index % 8)) as u8) ^ (index as u8 * 37);
        }
    }
    bytes
        .iter()
        .map(|byte| DIGITS[*byte as usize % 36] as char)
        .collect()
}

/// `new Date().toISOString()`：`YYYY-MM-DDTHH:mm:ss.sssZ`（UTC，毫秒恒 3 位）。
fn iso_timestamp() -> String {
    let millis_total = now_millis();
    let millis = millis_total.rem_euclid(1000);
    let seconds = millis_total.div_euclid(1000);
    let days = seconds.div_euclid(86_400);
    let remainder = seconds.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    let hour = remainder / 3_600;
    let minute = (remainder % 3_600) / 60;
    let second = remainder % 60;
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}.{millis:03}Z")
}

/// Howard Hinnant 的 `civil_from_days`（与 `terminal.rs` 的本地时间换算一致）。
fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let day_of_era = z - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let mut year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_prime + 2) / 5 + 1;
    let month = if month_prime < 10 {
        month_prime + 3
    } else {
        month_prime - 9
    };
    if month <= 2 {
        year += 1;
    }
    (year, month, day)
}

/// `os.tmpdir()`（Node `lib/os.js`）：POSIX 依次读 `TMPDIR` / `TMP` / `TEMP`，都未设置
/// （或为空串，Node 用真值判断）时回退 `/tmp`；Windows 依次读 `TEMP` / `TMP`，
/// 再回退 `SystemRoot`/`windir` 下的 `temp`。
fn os_tmpdir() -> &'static str {
    #[cfg(windows)]
    {
        for key in ["TEMP", "TMP"] {
            if let Ok(value) = std::env::var(key)
                && !value.is_empty()
            {
                return Box::leak(value.into_boxed_str());
            }
        }
        for key in ["SystemRoot", "windir"] {
            if let Ok(value) = std::env::var(key)
                && !value.is_empty()
            {
                return Box::leak(format!("{value}\\temp").into_boxed_str());
            }
        }
        "C:\\temp"
    }
    #[cfg(not(windows))]
    {
        for key in ["TMPDIR", "TMP", "TEMP"] {
            if let Ok(value) = std::env::var(key)
                && !value.is_empty()
            {
                return Box::leak(value.into_boxed_str());
            }
        }
        "/tmp"
    }
}

/// `path.join(dir, name)`（Node POSIX 语义）：空参数被丢弃，随后 `normalize` 折叠 `.`、
/// `..` 与重复分隔符；全部为空时结果是 `"."`。
///
/// TS 的 crash 日志路径依赖它（`logDirectory` 可能是 `"./logs"`、`"logs/"`、空串等），
/// 因此这里逐字复现 Node 的结果，而不是 `PathBuf::join` 的"原样拼接"。
fn node_path_join(directory: &str, name: &str) -> String {
    let mut joined = String::new();
    for part in [directory, name] {
        if part.is_empty() {
            continue;
        }
        if !joined.is_empty() {
            joined.push('/');
        }
        joined.push_str(part);
    }
    if joined.is_empty() {
        return ".".to_string();
    }
    normalize_posix_path(&joined)
}

/// `path.posix.normalize(path)`：保留开头的一个 `/`，折叠重复分隔符、`.` 与 `..`
/// （相对路径里越过根的 `..` 保留在开头）。
fn normalize_posix_path(path: &str) -> String {
    let absolute = path.starts_with('/');
    let mut segments: Vec<&str> = Vec::new();
    for segment in path.split('/') {
        match segment {
            "" | "." => {}
            ".." => match segments.last() {
                Some(last) if *last != ".." => {
                    segments.pop();
                }
                _ if absolute => {}
                _ => segments.push(".."),
            },
            other => segments.push(other),
        }
    }
    let body = segments.join("/");
    match (absolute, body.is_empty()) {
        (true, true) => "/".to_string(),
        (true, false) => format!("/{body}"),
        (false, true) => ".".to_string(),
        (false, false) => body,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bounded_writer_flushes_full_chunks_and_reports_length() {
        let writes = Rc::new(RefCell::new(Vec::<String>::new()));
        let sink = Rc::clone(&writes);
        let mut writer = BoundedTerminalWriter::new(Box::new(move |data: &str| {
            sink.borrow_mut().push(data.to_string());
        }));
        writer.append("abc");
        writer.append("def");
        assert_eq!(writer.length(), 6);
        writer.flush();
        writer.flush();
        assert_eq!(writer.length(), 6);
        assert_eq!(writes.borrow().join(""), "abcdef");
    }

    #[test]
    fn bounded_writer_never_splits_surrogate_pairs() {
        // 每个 emoji 在 UTF-16 里占 2 个 code unit；分块不得把它们拆开。
        let emoji = "\u{1F600}";
        let chunk = emoji.repeat(MAX_RENDER_WRITE_CHARS / 2 + 1);
        let writes: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
        let sink = Rc::clone(&writes);
        let mut writer = BoundedTerminalWriter::new(Box::new(move |data: &str| {
            sink.borrow_mut().push(data.to_string());
        }));
        writer.append(&chunk);
        writer.flush();
        let writes = writes.borrow();
        let total: usize = writes.iter().map(|write| utf16_len(write)).sum();
        assert_eq!(total, utf16_len(&chunk));
        for write in writes.iter() {
            assert!(utf16_len(write) <= MAX_RENDER_WRITE_CHARS);
        }
        assert!(writes.len() >= 2);
    }

    #[test]
    fn kitty_header_parsing_matches_js_number_semantics() {
        let header = parse_kitty_image_header("\x1b_Ga=T,f=100,i=42,r=3;data\x1b\\").unwrap();
        assert_eq!(
            header,
            KittyImageHeader {
                ids: vec![42],
                rows: 3
            }
        );

        // `Number(value)`：空串 → 0（被过滤），`1e3` → 1000（接受），`abc` → NaN（过滤）。
        let header = parse_kitty_image_header("\x1b_Gi=,i=1e3,i=abc,i=2.5,i=0;i=1\x1b\\").unwrap();
        assert_eq!(header.ids, vec![1000]);

        // 缺少 `;` 或前缀 → `undefined`。
        assert!(parse_kitty_image_header("plain").is_none());
        assert!(parse_kitty_image_header("\x1b_Gi=1").is_none());
        // `r` 只取最后一个合法值，缺省为 1。
        assert_eq!(extract_kitty_image_rows("\x1b_Gi=1;x"), 1);
    }

    /// JS `split("=", 2)` 的 limit 只截断结果数组，仍在**每个** `=` 处分割：`"i=5=6"`
    /// 解析出的 id 是 5（不是 `Number("5=6") == NaN` 被丢弃）。
    #[test]
    fn kitty_header_uses_js_split_limit_semantics() {
        let header = parse_kitty_image_header("\x1b_Ga=T,f=100,i=5=6,q=2;AAAA\x1b\\").unwrap();
        assert_eq!(header.ids, vec![5]);
        // 普通参数不受影响；`a=1=2=3` 只取 "1"。
        assert_eq!(
            parse_kitty_image_header("\x1b_Gi=1=2,i=7;x\x1b\\")
                .unwrap()
                .ids,
            vec![1, 7]
        );
    }

    /// Node `path.join` 的 POSIX 语义（用 node 26 逐条核对过的输入/输出）。
    #[test]
    fn node_path_join_matches_node_posix() {
        let cases = [
            ("/tmp", "/tmp/kiana-tui-crash.log"),
            ("/tmp/", "/tmp/kiana-tui-crash.log"),
            ("/a//b", "/a/b/kiana-tui-crash.log"),
            ("./logs", "logs/kiana-tui-crash.log"),
            ("/a/./b/../c", "/a/c/kiana-tui-crash.log"),
            ("", "kiana-tui-crash.log"),
            (".", "kiana-tui-crash.log"),
            ("../logs", "../logs/kiana-tui-crash.log"),
            ("/a/../../b", "/b/kiana-tui-crash.log"),
            ("logs/", "logs/kiana-tui-crash.log"),
            ("//", "/kiana-tui-crash.log"),
            ("a//b//", "a/b/kiana-tui-crash.log"),
        ];
        for (directory, expected) in cases {
            assert_eq!(
                node_path_join(directory, "kiana-tui-crash.log"),
                expected,
                "path.join({directory:?}, ...)"
            );
        }
    }

    /// Node `os.tmpdir()` 的 POSIX 回退顺序：`TMPDIR` → `TMP` → `TEMP` → `/tmp`（空串视为未设置）。
    /// 该测试改写进程环境变量，独占运行以免与其他读写同一变量的测试互相干扰。
    #[test]
    fn os_tmpdir_falls_back_in_node_order() {
        static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let _guard = ENV_LOCK.lock().unwrap_or_else(|poison| poison.into_inner());
        let saved: Vec<(&str, Option<String>)> = ["TMPDIR", "TMP", "TEMP"]
            .into_iter()
            .map(|name| (name, std::env::var(name).ok()))
            .collect();
        let restore = || {
            for (name, value) in &saved {
                match value {
                    Some(value) => unsafe { std::env::set_var(name, value) },
                    None => unsafe { std::env::remove_var(name) },
                }
            }
        };

        // 全部未设置 → /tmp。
        for name in ["TMPDIR", "TMP", "TEMP"] {
            unsafe { std::env::remove_var(name) };
        }
        assert_eq!(os_tmpdir(), "/tmp");
        // TMPDIR 为空串 → 落到 TMP。
        unsafe {
            std::env::set_var("TMPDIR", "");
            std::env::set_var("TMP", "/from-tmp");
        }
        assert_eq!(os_tmpdir(), "/from-tmp");
        // TMPDIR 优先。
        unsafe { std::env::set_var("TMPDIR", "/from-tmpdir") };
        assert_eq!(os_tmpdir(), "/from-tmpdir");
        // TEMP 是最后的环境回退。
        unsafe {
            std::env::remove_var("TMPDIR");
            std::env::remove_var("TMP");
            std::env::set_var("TEMP", "/from-temp");
        }
        assert_eq!(os_tmpdir(), "/from-temp");

        restore();
    }

    #[test]
    fn snapshot_round_trips_and_nulls_image_lines() {
        let mut tui = TuiMainScreen::new(Box::new(StubTerminal));
        tui.state.borrow_mut().previous_lines = vec![
            "plain".to_string(),
            "\x1b_Ga=T,f=100,i=7;data\x1b\\".to_string(),
        ];
        tui.state.borrow_mut().previous_width = 40;
        tui.state.borrow_mut().previous_height = 10;
        tui.state.borrow_mut().cursor_row = 1;
        tui.state.borrow_mut().hardware_cursor_row = 1;
        tui.state.borrow_mut().max_lines_rendered = 2;
        tui.state.borrow_mut().previous_viewport_top = 0;
        tui.state.borrow_mut().previous_kitty_image_ids.insert(7);

        let snapshot = tui.capture_render_state();
        assert_eq!(snapshot.previous_width, 40);
        assert_eq!(snapshot.previous_lines.len(), 2);

        tui.reset_render_state();
        assert!(tui.state.borrow().previous_lines.is_empty());
        assert_eq!(tui.state.borrow().previous_width, RESET_SIZE);
        // `resetRenderState` 不清 kitty id 集合：保留上一帧的 id，下一帧才能识别并清理它们。
        assert!(!tui.state.borrow().previous_kitty_image_ids.is_empty());

        tui.restore_render_state(snapshot);
        assert_eq!(
            tui.state.borrow().previous_lines,
            vec!["plain".to_string(), String::new()],
            "image lines are restored as empty lines"
        );
        assert!(tui.state.borrow().previous_kitty_image_ids.is_empty());
        assert_eq!(tui.state.borrow().previous_width, 40);
        assert_eq!(tui.state.borrow().max_lines_rendered, 2);
    }

    /// 最小终端桩（快照测试不渲染）。
    struct StubTerminal;

    impl Terminal for StubTerminal {
        fn start(&mut self, _on_input: Box<dyn FnMut(&str)>, _on_resize: Box<dyn FnMut()>) {}
        fn stop(&mut self) {}
        fn drain_input(
            &mut self,
            _max_ms: u64,
            _idle_ms: u64,
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + '_>> {
            Box::pin(async {})
        }
        fn write(&mut self, _data: &str) {}
        fn columns(&self) -> usize {
            80
        }
        fn rows(&self) -> usize {
            24
        }
        fn kitty_protocol_active(&self) -> bool {
            false
        }
        fn move_by(&mut self, _lines: isize) {}
        fn hide_cursor(&mut self) {}
        fn show_cursor(&mut self) {}
        fn clear_line(&mut self) {}
        fn clear_from_cursor(&mut self) {}
        fn clear_screen(&mut self) {}
        fn set_title(&mut self, _title: &str) {}
        fn set_progress(&mut self, _active: bool) {}
        fn set_program_status(&mut self, _status: &crate::terminal_colors::ProgramStatus) {}
    }

    #[test]
    fn iso_timestamp_matches_javascript_format() {
        // 只校验格式：`YYYY-MM-DDTHH:mm:ss.sssZ`。
        let timestamp = iso_timestamp();
        assert_eq!(timestamp.len(), 24);
        assert!(timestamp.ends_with('Z'));
        assert_eq!(&timestamp[4..5], "-");
        assert_eq!(&timestamp[10..11], "T");
        assert_eq!(&timestamp[19..20], ".");
    }
}
