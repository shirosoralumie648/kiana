//! stdin-buffer.ts。
//!
//! `StdinBuffer` buffers input and emits complete sequences.
//!
//! This is necessary because stdin data events can arrive in partial chunks, especially for
//! escape sequences like mouse events. Without buffering, partial sequences can be
//! misinterpreted as regular keypresses.
//!
//! For example, the mouse SGR sequence `\x1b[<35;20;5m` might arrive as:
//! - Event 1: `\x1b`
//! - Event 2: `[<35`
//! - Event 3: `;20;5m`
//!
//! The buffer accumulates these until a complete sequence is detected. Call [`StdinBuffer::process`]
//! to feed input data.
//!
//! Based on code from OpenTUI (<https://github.com/anomalyco/opentui>) MIT License - Copyright (c)
//! 2025 opentui
//!
//! ## Rust 侧差异（与 TS 的 `setTimeout` 事件循环对应）
//!
//! TS 用 `setTimeout` 调度"未完成序列的超时刷新"；Rust 侧把到期时间存成 [`std::time::Instant`]，
//! 由宿主在事件循环里调用 [`StdinBuffer::poll_timeout`] 触发。这样超时行为可以由测试用注入的
//! `now` 精确驱动（TS 用 `node:test` 的 mock timers 做同一件事）。
//!
//! TS 的 `extractCompleteSequences` 按 **UTF-16 code unit** 切分（`remaining[0]`、
//! `slice(0, seqEnd)`、`afterEsc.length`），因此非 BMP 字符会被拆成两个孤立代理项、发出
//! **两个** `data` 事件（`process("\u{1F389}")` → `["\uD83C", "\uDF89"]`）。
//! Rust 的 `String` 不能表示孤立代理项，事件类型 `StdinEvent::Data(String)`（§2.7.4 的
//! Rust 设计）因此只能把非 BMP 字符按一个 code point、发出**一个**事件：
//!
//! - 对 BMP 文本（含中日韩）切分结果不变；
//! - 非 BMP 输入只差事件边界：TS 是两个代理项、Rust 是一个标量；下游按键解析把两者都当
//!   文本插入，编辑器里拼接出的最终文本相同（`ESC` + 非 BMP 同样只差这一层切分）。
//!
//! 这是 `String` 表示能力的被迫偏差（与 `kiana-json` 对孤立代理项的决策同类）；要逐事件对齐
//! 只能把事件载荷改成 WTF-8/字节类型，会破坏 §2.7.4 冻结的 `Data(String)` 契约。
//! 转义序列的**识别**不受影响：候选串按 `char` 累积，但 `is_complete_sequence` 的
//! `len`/`afterEsc.length` 仍按 UTF-16 计数，与 TS 等价。
//!
//! ## 这个模块的边界
//!
//! 终端输入是字节流：一次 `read` 只保证返回当前已到达的字节，不保证一个按键或转义序列
//! 完整。鼠标 SGR、Kitty CSI-u、OSC 颜色回复与括号粘贴都可能被内核或 SSH 拆成多块。
//! 本模块只做**重新切分**：输入是任意字节块，输出是 [`StdinEvent`] 列表。它不解释按键
//! 含义（那是 `crate::keys` 的职责），也不解析终端回复的语义（那分散在
//! `crate::terminal_colors`、`crate::program_status`、`crate::terminal` 等处）。
//!
//! ## 状态
//!
//! [`StdinBuffer`] 的字段分四组：
//!
//! - `buffer`：尚未凑成完整事件的候选文本（可能只是半截转义序列）；
//! - `paste_mode` / `paste_buffer`：是否处在 `ESC[200~ … ESC[201~` 之间，以及已累积的
//!   粘贴内容；
//! - `pending_kitty_printable_codepoint`：上一条事件若是“未修改的可打印 Kitty CSI-u”，
//!   记下其码点，用于吞掉终端随后补发的同一原始字符；
//! - `deadline` / `timeout_ms` / `escape_timeout_ms`：挂起的超时与两个等待窗口。
//!
//! ## 一次 `process` 的流程
//!
//! 1. 清掉上一次挂起的超时。
//! 2. 空输入且缓冲区为空时，立即发出一个 `Data("")` 事件（测试 `handles_empty_input` 锁定）。
//! 3. 追加数据。若当前在粘贴模式，整块并入 `paste_buffer`，只有看到 `ESC[201~` 才发出
//!    [`StdinEvent::Paste`]；结束标记之后的内容会被递归地重新处理。
//! 4. 非粘贴模式下先找 `ESC[200~`：标记之前的内容照常切分并发出，标记之后进入粘贴模式
//!    （同一块内就结束也可以）。
//! 5. 其余情况用 `extract_complete_sequences` 切出所有完整事件，剩余半截留在 `buffer`。
//! 6. 若 `buffer` 仍非空，按内容排超时：孤立 `ESC` 用 `escape_timeout_ms`，其它用
//!    `timeout_ms`；但**匹配终端回复头**（OSC 10/11/4、OSC 7501、XTVersion、Kitty graphics）
//!    的缓冲不排超时——这类回复可能延迟任意久，提前刷出会把 BEL 终止符变成 Ctrl+G。
//!
//! ## 超时如何执行
//!
//! 本模块不创建线程、不持有定时器对象：`deadline` 是绝对时刻（[`std::time::Instant`]），
//! 由宿主每轮调用 [`StdinBuffer::poll_timeout`] 检查是否到期。到期后取走整个缓冲区，
//! 并对控制字符串做“前两个字符恢复成 Alt 键、余下重新解析”的拆分（见该方法文档）。
//! 生产代码里 `crate::terminal::ProcessTerminal::poll` 每轮调用它；测试用虚拟时钟直接调用。
//!
//! ## 失败模式
//!
//! - 所有入口都不返回 `Result`：无法识别的字节一律当普通文本，最坏情况是事件被切得更细，
//!   不会丢失输入。会丢字节的位置有以下几处：括号粘贴的起止标记被剥掉（内容走 `Paste`
//!   事件）、Kitty 可打印序列之后的重复原始字符被去重、紧邻粘贴起点的半截序列
//!   （见 `process_inner` 内的说明）；此外 `extract_complete_sequences` 在匹配终端回复头时
//!   有两条直接推进游标、不产出事件的路径：(4) 回复尚未终结时遇到 CAN（`0x18`）或
//!   SUB（`0x1a`），已累积的回复前缀连同该字节整段丢弃（后续输入不受影响，测试
//!   `discards_a_canceled_reply_without_swallowing_subsequent_input` 锁定）；(5) 回复尚未
//!   终结时遇到新的 `ESC`（其后还有字符且不是 `\`，否则它就是回复自己的 ST 终止符），
//!   丢弃回复前缀并把这个 `ESC` 留给外层循环重新解析（测试
//!   `recovers_from_an_unterminated_reply_when_a_new_escape_sequence_arrives` 锁定）。
//! - 匹配回复头却永远等不到终止符时，`buffer` 会一直保留并随输入增长（超时被刻意跳过）。
//! - `process_inner` 在“粘贴结束标记之后还有内容”时递归自身；单次输入中的粘贴块数就是
//!   递归深度。
//! - 字节入口 `process` 对每次调用的数据独立解码：长度 >= 2 的块按 UTF-8 lossy 解码，
//!   单字节 `> 127` 的块走 `ESC + (byte - 128)` 的高字节转换。多字节字符被拆到两次调用
//!   之间时通常得到 U+FFFD 与 `ESC` + 字节的混合（两侧都是单字节 `> 127` 时则都是
//!   `ESC` + 字节，都不是 U+FFFD）。需要跨块保持多字节序列的调用方应先自行解码，再走
//!   字符串入口（`ProcessTerminal::poll_input` 就是这样做的）。

use std::time::{Duration, Instant};

use regex::Regex;
use std::sync::OnceLock;

/// `ESC`。
///
/// 所有转义序列的起始字符；`extract_complete_sequences` 与 [`is_control_string`] 都以它
/// 做前缀判断。
pub const ESC: &str = "\x1b";
/// `DEFAULT_SEQUENCE_TIMEOUT_MS`。
///
/// 非孤立 ESC 的半截序列最多等待 50 ms；可被 [`StdinBufferOptions::timeout`] 覆盖。
pub const DEFAULT_SEQUENCE_TIMEOUT_MS: u64 = 50;
/// `DEFAULT_ESCAPE_TIMEOUT_MS`。
///
/// 单独一个 `ESC` 最多等待 10 ms，超时后按 Escape 键发出；可被
/// [`StdinBufferOptions::escape_timeout`] 覆盖。
pub const DEFAULT_ESCAPE_TIMEOUT_MS: u64 = 10;
/// `BRACKETED_PASTE_START`。
///
/// 开启括号粘贴（`ESC[?2004h`）后终端用它包裹粘贴内容。本模块发出
/// [`StdinEvent::Paste`] 时已剥掉起止标记；`ProcessTerminal` 分发时会重新补上
/// （见 `crate::terminal` 的 `dispatch_stdin_event`）。
pub const BRACKETED_PASTE_START: &str = "\x1b[200~";
/// `BRACKETED_PASTE_END`。
///
/// 粘贴结束标记；只有它完整到达才会产生 [`StdinEvent::Paste`]。
pub const BRACKETED_PASTE_END: &str = "\x1b[201~";

/// Color and program-status OSC replies, XTVersion, and Kitty graphics replies. Until their header
/// is complete, the input may be batched legacy Alt keystrokes.
///
/// 两个用途：(1) `extract_complete_sequences` 里判定一个控制字符串是否属于“可被 CAN/SUB
/// 或新 ESC 中止”的已知回复；(2) `process_inner` 里命中时不排超时（回复可能延迟任意久）。
/// 正则只认**开头**，不检查终止符。
///
/// TS：`/^\x1b(?:\](?:1[01];|4;\d{1,3};|7501;\?)|P>\||_G(?:[a-z]=\d+,)*[a-z]=\d+;)/`
fn terminal_reply_header_pattern() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN.get_or_init(|| {
        Regex::new(
            r"^\x1b(?:\](?:1[01];|4;[0-9]{1,3};|7501;\?)|P>\||_G(?:[a-z]=[0-9]+,)*[a-z]=[0-9]+;)",
        )
        .expect("static regex")
    })
}

/// `parseUnmodifiedKittyPrintableCodepoint` 的正则：
/// `/^\x1b\[(\d+)(?::\d*)?(?::\d+)?u$/`。
fn kitty_printable_pattern() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN.get_or_init(|| {
        Regex::new(r"^\x1b\[([0-9]+)(?::[0-9]*)?(?::[0-9]+)?u$").expect("static regex")
    })
}

/// SGR 鼠标序列：`/^<\d+;\d+;\d+[Mm]$/`。
fn sgr_mouse_pattern() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN.get_or_init(|| Regex::new(r"^<[0-9]+;[0-9]+;[0-9]+[Mm]$").expect("static regex"))
}

/// [`StdinBuffer::process`] 发出的事件（TS 的 `data` / `paste` 事件）。
///
/// 事件顺序与输入顺序一致；`process` 返回的 `Vec` 就是一次输入块产生的全部事件。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StdinEvent {
    /// `data` 事件：一个完整序列（或单个普通字符）。
    ///
    /// 普通文本按字符逐个发出；非 BMP 字符是一个 code point 一个事件（见模块文档）。
    Data(String),
    /// `paste` 事件：括号粘贴里的内容（不含 `\x1b[200~` / `\x1b[201~`）。
    ///
    /// 只有结束标记完整到达才会产生；内容里可以包含换行与任意 Unicode。
    Paste(String),
}

/// `StdinBufferOptions`。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StdinBufferOptions {
    /// Maximum time to wait for an incomplete keyboard or mouse sequence (default: 50ms).
    ///
    /// 毫秒；`None` 时用 [`DEFAULT_SEQUENCE_TIMEOUT_MS`]。
    pub timeout: Option<u64>,
    /// Maximum time to wait after a lone ESC before treating it as Escape (default: 10ms).
    ///
    /// 毫秒；`None` 时用 [`DEFAULT_ESCAPE_TIMEOUT_MS`]。两个窗口互不影响：只有缓冲内容
    /// 恰好等于单个 `ESC` 时才使用这一项。
    pub escape_timeout: Option<u64>,
}

/// `isControlString(data)`：`ESC ]` / `ESC P` / `ESC _` 开头。
///
/// 只判断前缀，不验证终止符，也不区分具体是哪种控制字符串（OSC / DCS / APC）。
/// 超时与中止逻辑用它把“字符串类序列”与普通 CSI、SS3、Meta 序列区分开。
pub fn is_control_string(data: &str) -> bool {
    data.starts_with("\x1b]") || data.starts_with("\x1bP") || data.starts_with("\x1b_")
}

/// `data.length`（UTF-16 code unit 数）。
fn utf16_len(data: &str) -> usize {
    if data.is_ascii() {
        return data.len();
    }
    data.chars().map(char::len_utf16).sum()
}

/// `data.slice(start, end)`（UTF-16 code unit 索引）。ASCII 走字节切片。
///
/// 边界落在代理对中间时，JS 得到孤立代理项；Rust 的 `String` 无法表示，用 U+FFFD 代替——
/// 它的 UTF-16 宽度同样是 1，因此后续的长度判定保持不变。
fn utf16_slice(data: &str, start: usize, end: usize) -> String {
    if data.is_ascii() {
        let len = data.len();
        let start = start.min(len);
        let end = end.min(len);
        return data[start..end.max(start)].to_string();
    }
    let end = end.min(utf16_len(data));
    let mut out = String::new();
    let mut units = 0usize;
    for ch in data.chars() {
        let width = ch.len_utf16();
        let char_start = units;
        let char_end = units + width;
        units = char_end;
        if char_end <= start {
            continue;
        }
        if char_start >= end {
            break;
        }
        if char_start < start || char_end > end {
            // 切片边界切开了这个字符（JS 里是孤立代理项）。
            out.push('\u{fffd}');
            continue;
        }
        out.push(ch);
    }
    out
}

/// [`is_complete_sequence`] 的三态结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SequenceStatus {
    /// 已经可以作为一个事件发出。
    Complete,
    /// 目前是合法前缀，但可能还需要更多字节；应继续等待或排超时。
    Incomplete,
    /// 不以 `ESC` 开头，不是转义序列（调用方按普通字符处理）。
    NotEscape,
}

/// Check if a string is a complete escape sequence or needs more data.
///
/// 分派顺序与代码分支一致：CSI（`ESC [`，其中旧式鼠标 `ESC [M` 固定要满 6 个 UTF-16
/// 单元）→ OSC（`ESC ]`）→ DCS（`ESC P`）→ APC（`ESC _`）→ SS3（`ESC O` 加 1 个字符）
/// → Meta（`ESC` 加单个字符）→ 其它未知转义序列当作完整。
///
/// 所有长度判断都按 UTF-16 code unit（[`utf16_len`]），因为 TS 的 `length` 如此。
/// 该函数看的是“整个候选串是否完整”，不负责找边界；找边界的是
/// `extract_complete_sequences`（它从最短前缀开始试探，一般取第一个 `Complete` 作为事件
/// 边界；“候选恰为 `ESC ESC`”时有一条特例，见该函数文档）。
fn is_complete_sequence(data: &str) -> SequenceStatus {
    if !data.starts_with(ESC) {
        return SequenceStatus::NotEscape;
    }
    if utf16_len(data) == 1 {
        return SequenceStatus::Incomplete;
    }

    let after_esc = &data[1..];

    // CSI sequences: ESC [
    if after_esc.starts_with('[') {
        // Old-style mouse sequence: ESC[M + 3 bytes = 6 total.
        if after_esc.starts_with("[M") {
            return if utf16_len(data) >= 6 {
                SequenceStatus::Complete
            } else {
                SequenceStatus::Incomplete
            };
        }
        return is_complete_csi_sequence(data);
    }

    // OSC sequences: ESC ]
    if after_esc.starts_with(']') {
        return is_complete_osc_sequence(data);
    }

    // DCS sequences: ESC P ... ESC \ (includes XTVersion responses)
    if after_esc.starts_with('P') {
        return is_complete_dcs_sequence(data);
    }

    // APC sequences: ESC _ ... ESC \ (includes Kitty graphics responses)
    if after_esc.starts_with('_') {
        return is_complete_apc_sequence(data);
    }

    // SS3 sequences: ESC O followed by a single character
    if after_esc.starts_with('O') {
        return if utf16_len(after_esc) >= 2 {
            SequenceStatus::Complete
        } else {
            SequenceStatus::Incomplete
        };
    }

    // Meta key sequences: ESC followed by a single character
    if utf16_len(after_esc) == 1 {
        return SequenceStatus::Complete;
    }

    // Unknown escape sequence - treat as complete
    SequenceStatus::Complete
}

/// Check if CSI sequence is complete. CSI sequences end with a final byte (0x40-0x7E).
///
/// 完整条件：`ESC [` 之后至少 1 个字符，且**最后一个字符**落在 CSI final byte 区间
/// `0x40..=0x7e`。参数未到齐时候选串以参数区间字符（`0x30..=0x3f`，如数字与 `;`）结尾，
/// 因此自然停在 `Incomplete`。
///
/// SGR 鼠标（payload 以 `<` 开头）另走严格判定：必须形如 `<B;X;Y` 后跟 `M` 或 `m`。
/// 这样 `ESC[<…` 在参数没到齐时不会因为末尾恰好是字母而提前成为事件。正则匹配与
/// 逐段数字检查是两条语法相同的接受路径。
fn is_complete_csi_sequence(data: &str) -> SequenceStatus {
    if !data.starts_with("\x1b[") {
        return SequenceStatus::Complete;
    }
    if utf16_len(data) < 3 {
        return SequenceStatus::Incomplete;
    }

    let payload = &data[2..];
    let last_char = payload.chars().next_back().expect("payload is non-empty");
    let last_char_code = last_char as u32;

    if (0x40..=0x7e).contains(&last_char_code) {
        // Special handling for SGR mouse sequences: ESC[<B;X;Ym or ESC[<B;X;YM
        if payload.starts_with('<') {
            if sgr_mouse_pattern().is_match(payload) {
                return SequenceStatus::Complete;
            }
            // If it ends with M or m but doesn't match the pattern, still incomplete.
            if last_char == 'M' || last_char == 'm' {
                let inner = utf16_slice(payload, 1, utf16_len(payload).saturating_sub(1));
                let parts: Vec<&str> = inner.split(';').collect();
                if parts.len() == 3
                    && parts
                        .iter()
                        .all(|part| !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit()))
                {
                    return SequenceStatus::Complete;
                }
            }
            return SequenceStatus::Incomplete;
        }
        return SequenceStatus::Complete;
    }

    SequenceStatus::Incomplete
}

/// Check if OSC sequence is complete: `ESC ] ... ST` where ST is `ESC \` or BEL.
///
/// 只检查**结尾**：中间出现 BEL 不算结束。前缀不符时返回 `Complete`（防御分支；正常
/// 调用链由 `is_complete_sequence` 保证前缀）。
fn is_complete_osc_sequence(data: &str) -> SequenceStatus {
    if !data.starts_with("\x1b]") {
        return SequenceStatus::Complete;
    }
    if data.ends_with("\x1b\\") || data.ends_with('\x07') {
        return SequenceStatus::Complete;
    }
    SequenceStatus::Incomplete
}

/// Check if DCS sequence is complete: `ESC P ... ST`.
///
/// 与 OSC 不同，DCS 只认 `ESC \`（ST），BEL 不能终止。前缀不符时返回 `Complete`
/// （防御分支）。
fn is_complete_dcs_sequence(data: &str) -> SequenceStatus {
    if !data.starts_with("\x1bP") {
        return SequenceStatus::Complete;
    }
    if data.ends_with("\x1b\\") {
        return SequenceStatus::Complete;
    }
    SequenceStatus::Incomplete
}

/// Check if APC sequence is complete: `ESC _ ... ST`.
///
/// 与 OSC 不同，APC 只认 `ESC \`（ST），BEL 不能终止。前缀不符时返回 `Complete`
/// （防御分支）。
fn is_complete_apc_sequence(data: &str) -> SequenceStatus {
    if !data.starts_with("\x1b_") {
        return SequenceStatus::Complete;
    }
    if data.ends_with("\x1b\\") {
        return SequenceStatus::Complete;
    }
    SequenceStatus::Incomplete
}

/// `parseUnmodifiedKittyPrintableCodepoint(sequence)`。
///
/// 只匹配未带修饰/事件类型的形式 `ESC [ <码点> u`（码点后可跟零到两段 `:数字` 子参数），
/// 且要求码点 >= 32：Escape（27）等控制键返回 `None`，因此永远不会被
/// `emit_data_sequence` 的重复字符去重吞掉。带 `;` 修饰符的序列（如 `ESC[97;1:3u`）
/// 不匹配。
fn parse_unmodified_kitty_printable_codepoint(sequence: &str) -> Option<u32> {
    let captures = kitty_printable_pattern().captures(sequence)?;
    let codepoint: u32 = captures.get(1)?.as_str().parse().ok()?;
    if codepoint >= 32 {
        Some(codepoint)
    } else {
        None
    }
}

/// Split accumulated buffer into complete sequences.
///
/// 返回 `(完整事件列表, 未完成的剩余缓冲)`：剩余缓冲一定是以 `ESC` 开头的后缀；全部消费完
/// 时为空串。
///
/// 切分策略：遇到 `ESC` 时从最短前缀（只有 `ESC`）开始逐字符扩大候选；一般规则是第一个
/// 被 `is_complete_sequence` 判为 `Complete` 的前缀就是一个事件，剩余部分回到外层循环继续。
///
/// 例外：候选恰为 `ESC ESC` 且其后紧邻的字符是 `[`、`]`、`O`、`P` 或 `_` 时，只把第一个
/// `ESC` 作为事件发出，第二个 `ESC` 留给外层循环重新解析（WezTerm 配合 Kitty 键盘协议把
/// Escape 的按下（裸 `ESC`）与释放（完整 Kitty CSI-u）拼在一起发送；测试
/// `splits_esc_esc_csi_into_standalone_esc_and_the_csi_sequence` 与
/// `still_emits_esc_esc_as_a_single_sequence_when_not_followed_by_a_new_escape` 锁定两种
/// 行为）。后随字符不在这个集合里或没有后随字符时，整个 `ESC ESC` 作为一个事件。
///
/// 不以 `ESC` 开头的部分逐个字符发出。
///
/// TS 的 `pos` / `seqEnd` 是 UTF-16 code unit 计数；Rust 侧按 `char` 计。转义序列的识别与
/// 边界不受影响（每个非 BMP 字符在两边都贡献同样的切片内容，`is_complete_sequence` 内部用
/// [`utf16_len`] 做长度判断）；唯一可观察差异是下面的非转义分支：TS 每次取一个 UTF-16 code
/// unit（非 BMP → 两个孤立代理项、两个事件），Rust 每次取一个 code point（一个事件），
/// 见模块文档。
fn extract_complete_sequences(buffer: &str) -> (Vec<String>, String) {
    let mut sequences: Vec<String> = Vec::new();
    let mut pos = 0usize;

    while pos < buffer.len() {
        let remaining = &buffer[pos..];

        if remaining.starts_with(ESC) {
            // 只有已知的终端回复头才启用下面的“CAN/SUB 或新 ESC 中止”规则；普通 OSC/DCS/APC
            // （例如窗口标题）仍只按各自的终止符判断。
            let terminal_reply =
                is_control_string(remaining) && terminal_reply_header_pattern().is_match(remaining);

            // Find the end of this escape sequence.
            // `seq_end` 是 chars 的下标；从 1（只有 ESC）开始，`Incomplete` 时扩大一位。
            let chars: Vec<char> = remaining.chars().collect();
            let mut seq_end = 1usize;
            while seq_end <= chars.len() {
                if terminal_reply && seq_end > 2 {
                    let ch = chars[seq_end - 1];
                    if ch == '\x18' || ch == '\x1a' {
                        // CAN and SUB cancel the string, not subsequent keyboard input.
                        pos += chars[..seq_end].iter().map(|c| c.len_utf8()).sum::<usize>();
                        break;
                    }
                    if ch == '\x1b' && seq_end < chars.len() && chars[seq_end] != '\\' {
                        // A new escape sequence aborts an unterminated reply. Keep a trailing ESC
                        // buffered until we can distinguish it from ST.
                        pos += chars[..seq_end - 1]
                            .iter()
                            .map(|c| c.len_utf8())
                            .sum::<usize>();
                        break;
                    }
                }
                let candidate: String = chars[..seq_end].iter().collect();
                match is_complete_sequence(&candidate) {
                    SequenceStatus::Complete => {
                        // WezTerm with enable_kitty_keyboard sends the Escape key press as a raw
                        // '\x1b' byte and the release as a full Kitty CSI-u sequence, concatenated
                        // as '\x1b\x1b[27;...u'. Emit only the first ESC and restart from the
                        // second if the following byte starts a new escape sequence.
                        if candidate == "\x1b\x1b" {
                            let next_char = chars.get(seq_end).copied();
                            if matches!(next_char, Some('[' | ']' | 'O' | 'P' | '_')) {
                                sequences.push(ESC.to_string());
                                pos += 1;
                                break;
                            }
                        }
                        sequences.push(candidate);
                        pos += chars[..seq_end].iter().map(|c| c.len_utf8()).sum::<usize>();
                        break;
                    }
                    SequenceStatus::Incomplete => seq_end += 1,
                    SequenceStatus::NotEscape => {
                        // Should not happen when starting with ESC.
                        sequences.push(candidate);
                        pos += chars[..seq_end].iter().map(|c| c.len_utf8()).sum::<usize>();
                        break;
                    }
                }
            }

            // 候选扩展到整个剩余缓冲仍不完整：整段留在 buffer，等下一次输入或超时。
            if seq_end > chars.len() {
                return (sequences, remaining.to_string());
            }
        } else {
            // Not an escape sequence - take a single character.
            let ch = remaining.chars().next().expect("non-empty remainder");
            sequences.push(ch.to_string());
            pos += ch.len_utf8();
        }
    }

    (sequences, String::new())
}

/// Buffers stdin input and emits complete sequences via the data event.
///
/// Handles partial escape sequences that arrive across multiple chunks.
///
/// 有状态：同一个实例必须跨多次 [`StdinBuffer::process`] / [`StdinBuffer::process_str`]
/// 复用，半截序列的拼接依赖实例里的 `buffer`。这些入口本身不回调外部代码，一次调用产生的
/// 事件全部通过返回的 `Vec` 交付。
pub struct StdinBuffer {
    /// 尚未凑成完整事件的候选文本；粘贴模式下会被移入 `paste_buffer`（`process_inner`
    /// 返回时这里为空）。
    buffer: String,
    /// `timeout`（`setTimeout` 的等价物）：到期时间。
    deadline: Option<Instant>,
    /// 非孤立 ESC 的半截序列等待窗口（毫秒）。
    timeout_ms: u64,
    /// 孤立 ESC 的等待窗口（毫秒）。
    escape_timeout_ms: u64,
    /// 是否处在括号粘贴中；为真时新数据先并入 `paste_buffer`。
    paste_mode: bool,
    /// 已收到的粘贴内容（不含起止标记）。
    paste_buffer: String,
    /// 上一条 Kitty 可打印 CSI-u 的码点，用于吞掉终端补发的同一字符。
    pending_kitty_printable_codepoint: Option<u32>,
}

impl Default for StdinBuffer {
    /// `new StdinBuffer()`：两个超时都用默认值（50 ms / 10 ms）。
    fn default() -> Self {
        Self::new(StdinBufferOptions::default())
    }
}

impl StdinBuffer {
    /// `new StdinBuffer(options)`。
    ///
    /// `timeout` / `escape_timeout` 为 `None` 时分别回退到
    /// [`DEFAULT_SEQUENCE_TIMEOUT_MS`] / [`DEFAULT_ESCAPE_TIMEOUT_MS`]。
    pub fn new(options: StdinBufferOptions) -> Self {
        StdinBuffer {
            buffer: String::new(),
            deadline: None,
            timeout_ms: options.timeout.unwrap_or(DEFAULT_SEQUENCE_TIMEOUT_MS),
            escape_timeout_ms: options.escape_timeout.unwrap_or(DEFAULT_ESCAPE_TIMEOUT_MS),
            paste_mode: false,
            paste_buffer: String::new(),
            pending_kitty_printable_codepoint: None,
        }
    }

    /// `process(data: string | Buffer)`——Buffer 入口（Node 的字节块）。
    ///
    /// 单字节 `> 127` 的 Buffer 走 `ESC + (byte - 128)` 的高字节转换（旧式
    /// Meta 编码）；其它按 UTF-8 解码（`Buffer.toString()` 的 lossy 语义）。
    ///
    /// 注意：每个调用**独立**解码——只有长度 >= 2 的块做 UTF-8 lossy 解码，单字节
    /// `> 127` 的块走上文的 `ESC + (byte - 128)` 高字节转换。因此多字节字符被拆到两次
    /// 调用之间时通常是一边得到 U+FFFD、另一边得到 `ESC` + 字节；两侧都是单字节 `> 127`
    /// 时则都是 `ESC` + 字节（UTF-8 的 `é`（`0xC3 0xA9`）拆成两块时分别得到 `ESC` + `C`
    /// 与 `ESC` + `)`）。
    /// `ProcessTerminal::poll_input` 因此先用跨块保持挂起的 UTF-8 解码器，再走字符串入口。
    pub fn process(&mut self, data: &[u8]) -> Vec<StdinEvent> {
        self.process_at(Instant::now(), data)
    }

    /// [`StdinBuffer::process`] 的显式时钟版本（测试用；便于注入时钟，无需真实计时器）。
    ///
    /// `now` 只影响超时排期，不影响事件内容。
    pub fn process_at(&mut self, now: Instant, data: &[u8]) -> Vec<StdinEvent> {
        if data.len() == 1 && data[0] > 127 {
            let byte = data[0] - 128;
            let text = format!("\x1b{}", char::from(byte));
            return self.process_str_at(now, &text);
        }
        let text = String::from_utf8_lossy(data).into_owned();
        self.process_str_at(now, &text)
    }

    /// `process(data: string)`——字符串入口。
    ///
    /// 入口约定：数据已经是完整的 UTF-8 文本，不再做字节级解码。
    pub fn process_str(&mut self, data: &str) -> Vec<StdinEvent> {
        self.process_str_at(Instant::now(), data)
    }

    /// [`StdinBuffer::process_str`] 的显式时钟版本。
    pub fn process_str_at(&mut self, now: Instant, data: &str) -> Vec<StdinEvent> {
        let mut events = Vec::new();
        self.process_inner(now, data, &mut events);
        events
    }

    /// `process` 的共同实现：累积数据、处理粘贴模式、切分事件，并在需要时排超时。
    ///
    /// `events` 是累加器；递归调用共享同一个 `Vec`，因此事件顺序始终等于输入顺序。
    /// 递归只发生在“粘贴结束标记之后还有内容”时（见下方两处粘贴分支）。
    fn process_inner(&mut self, now: Instant, data: &str, events: &mut Vec<StdinEvent>) {
        // Clear any pending timeout.
        // 新数据让旧超时失效；常规路径会在本次调用末尾按新内容重新排期，粘贴模式分支、
        // 粘贴起点分支与命中终端回复头的分支则直接返回、不排期（见下）。
        self.deadline = None;

        if data.is_empty() && self.buffer.is_empty() {
            // 空 data 在空缓冲上也会发出一个空 data 事件（测试
            // `handles_empty_input` 锁定该行为）。
            self.emit_data_sequence("", events);
            return;
        }

        self.buffer.push_str(data);

        if self.paste_mode {
            // 粘贴模式不解析内容：整块并入 paste_buffer，只有结束标记到齐才产出 Paste。
            self.paste_buffer.push_str(&self.buffer);
            self.buffer.clear();

            if let Some(end_index) = self.paste_buffer.find(BRACKETED_PASTE_END) {
                // 结束标记前的内容是 Paste 载荷；标记之后的剩余内容递归重新处理，可能包含
                // 下一段粘贴或普通按键，因此不能丢。
                let pasted_content = self.paste_buffer[..end_index].to_string();
                let remaining =
                    self.paste_buffer[end_index + BRACKETED_PASTE_END.len()..].to_string();

                self.paste_mode = false;
                self.paste_buffer.clear();
                self.pending_kitty_printable_codepoint = None;

                events.push(StdinEvent::Paste(pasted_content));

                if !remaining.is_empty() {
                    self.process_inner(now, &remaining, events);
                }
            }
            return;
        }

        if let Some(start_index) = self.buffer.find(BRACKETED_PASTE_START) {
            // 起始标记之前的内容仍按普通输入切分并立即发出。
            if start_index > 0 {
                let before_paste = self.buffer[..start_index].to_string();
                // 注意：第二个返回值（未完成的半截序列）在这里被丢弃，因此紧邻粘贴起点的
                // 半截转义序列不会保留到粘贴结束之后。
                let (sequences, _) = extract_complete_sequences(&before_paste);
                for sequence in sequences {
                    self.emit_data_sequence(&sequence, events);
                }
            }

            self.pending_kitty_printable_codepoint = None;
            let rest = self.buffer[start_index + BRACKETED_PASTE_START.len()..].to_string();
            self.buffer.clear();
            self.paste_mode = true;
            self.paste_buffer = rest;

            if let Some(end_index) = self.paste_buffer.find(BRACKETED_PASTE_END) {
                // 起始与结束标记落在同一块里：立即产出 Paste，并递归处理其后的内容。
                let pasted_content = self.paste_buffer[..end_index].to_string();
                let remaining =
                    self.paste_buffer[end_index + BRACKETED_PASTE_END.len()..].to_string();

                self.paste_mode = false;
                self.paste_buffer.clear();
                self.pending_kitty_printable_codepoint = None;

                events.push(StdinEvent::Paste(pasted_content));

                if !remaining.is_empty() {
                    self.process_inner(now, &remaining, events);
                }
            }
            return;
        }

        // 常规路径：切出所有能确定边界的完整事件，只有未完成的尾部回写 buffer。
        let (sequences, remainder) = extract_complete_sequences(&self.buffer);
        self.buffer = remainder;

        for sequence in sequences {
            self.emit_data_sequence(&sequence, events);
        }

        if !self.buffer.is_empty() {
            // Only a recognizable reply header establishes a delimited string. Flushing it would
            // turn a delayed tail into text and an OSC's BEL terminator into Ctrl+G.
            if terminal_reply_header_pattern().is_match(&self.buffer) {
                return;
            }
            // 孤立 ESC 用更短的 `escape_timeout_ms`（默认 10 ms），其余半截序列用
            // `timeout_ms`（默认 50 ms）。窗口只影响何时把缓冲区当文本刷出，不影响已被判为
            // 完整的序列。
            let timeout_ms = if self.buffer == ESC {
                self.escape_timeout_ms
            } else {
                self.timeout_ms
            };
            self.deadline = Some(now + Duration::from_millis(timeout_ms));
        }
    }

    /// 发出一个 `Data` 事件，并维护 Kitty 重复字符去重。
    ///
    /// Kitty 键盘协议下，部分终端在发出“可打印键的 CSI-u 序列”后还会补发同一个原始字符。
    /// 若本条事件是单个 UTF-16 单元且码点等于上一条记录，就丢弃它（只吞一次，随后记录被
    /// 清空）；否则记录本条解析出的码点（不是未修饰 CSI-u 时记录 `None`）。
    ///
    /// 只有单个 UTF-16 单元的字符（BMP）参与去重：判定条件是 `utf16_len(sequence) == 1`。
    fn emit_data_sequence(&mut self, sequence: &str, events: &mut Vec<StdinEvent>) {
        let raw_codepoint = if utf16_len(sequence) == 1 {
            sequence.chars().next().map(|ch| ch as u32)
        } else {
            None
        };
        if let Some(raw_codepoint) = raw_codepoint
            && Some(raw_codepoint) == self.pending_kitty_printable_codepoint
        {
            self.pending_kitty_printable_codepoint = None;
            return;
        }

        self.pending_kitty_printable_codepoint =
            parse_unmodified_kitty_printable_codepoint(sequence);
        events.push(StdinEvent::Data(sequence.to_string()));
    }

    /// `flush(): string[]`：立即取出缓冲区内容（不重新解析）。
    ///
    /// 总会清掉挂起超时。非空缓冲返回恰好一个元素（整个剩余缓冲）并清掉 Kitty 去重记录；
    /// 空缓冲返回空 `Vec`，且不改动 `pending_kitty_printable_codepoint`。两条路径都**不**
    /// 退出粘贴模式、也不清 `paste_buffer`。
    pub fn flush(&mut self) -> Vec<String> {
        self.deadline = None;

        if self.buffer.is_empty() {
            return Vec::new();
        }

        let sequences = vec![std::mem::take(&mut self.buffer)];
        self.pending_kitty_printable_codepoint = None;
        sequences
    }

    /// `clear(): void`。
    ///
    /// 丢弃缓冲、粘贴状态与挂起超时，不发出任何事件。
    pub fn clear(&mut self) {
        self.deadline = None;
        self.buffer.clear();
        self.paste_mode = false;
        self.paste_buffer.clear();
        self.pending_kitty_printable_codepoint = None;
    }

    /// `getBuffer(): string`。
    ///
    /// 只返回未完成的候选缓冲，不含粘贴模式下已累积的内容。
    pub fn get_buffer(&self) -> &str {
        &self.buffer
    }

    /// `destroy(): void`。
    ///
    /// 等价于 [`StdinBuffer::clear`]；`ProcessTerminal::stop` 用它清掉残留输入。
    pub fn destroy(&mut self) {
        self.clear();
    }

    /// 当前挂起的超时到期时间（TS 的 `timeout` 句柄；宿主据此排程）。
    ///
    /// 没有挂起超时时返回 `None`。纯读取：调用它不会推进或清除任何状态。
    pub fn pending_timeout_deadline(&self) -> Option<Instant> {
        self.deadline
    }

    /// 超时回调的等价物：`now >= deadline` 时执行 TS `setTimeout` 里的刷新逻辑。
    ///
    /// TS 的回调先 `flush()`，再对每条被刷出的序列：长度 > 2 且是控制字符串时，先发出前
    /// 两个字符（让 `ESC ]` 变成 Alt+]），再把余下部分交给 `process_inner` 重新解析；其余
    /// 原样发出 `Data`。
    ///
    /// 补充细节：未设置 deadline 或尚未到期时返回空 `Vec`，状态不变。到期后先由
    /// [`StdinBuffer::flush`] 取出整个缓冲并清掉 deadline，再按上面的规则逐条处理。
    /// `split_at(2)` 按字节切分是安全的：`is_control_string` 保证前两个字节是 ASCII 的
    /// `ESC` 加上 `]` / `P` / `_` 之一，不会落在多字节字符中间。重新解析可能再次排超时
    /// （余下部分仍然不完整时）。
    pub fn poll_timeout(&mut self, now: Instant) -> Vec<StdinEvent> {
        let mut events = Vec::new();
        let Some(deadline) = self.deadline else {
            return events;
        };
        if now < deadline {
            return events;
        }

        let flushed = self.flush();
        for sequence in flushed {
            // 长度用字节数判断；控制字符串的前两个字符是 ASCII，所以与 TS 的字符长度判定
            // 在本分支上结果相同（`ESC]` 恰为 2，任何附加字符都会让它超过 2）。
            if sequence.len() > 2 && is_control_string(&sequence) {
                // Recover the Alt key and reparse its tail so batched text, Enter, and control
                // keys are not swallowed as one event.
                let (head, tail) = sequence.split_at(2);
                self.emit_data_sequence(head, &mut events);
                let tail = tail.to_string();
                self.process_inner(now, &tail, &mut events);
            } else {
                self.emit_data_sequence(&sequence, &mut events);
            }
        }
        events
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utf16_slice_matches_js_semantics() {
        assert_eq!(utf16_slice("abcdef", 1, 3), "bc");
        assert_eq!(utf16_slice("abcdef", 1, 99), "bcdef");
        assert_eq!(utf16_slice("aé", 1, 2), "é");
        assert_eq!(utf16_slice("a🎉", 1, 3), "🎉");
        assert_eq!(utf16_slice("a🎉", 1, 2), "\u{fffd}");
    }

    #[test]
    fn complete_sequence_checks() {
        assert_eq!(is_complete_sequence("a"), SequenceStatus::NotEscape);
        assert_eq!(is_complete_sequence("\x1b"), SequenceStatus::Incomplete);
        assert_eq!(is_complete_sequence("\x1b["), SequenceStatus::Incomplete);
        assert_eq!(is_complete_sequence("\x1b[A"), SequenceStatus::Complete);
        assert_eq!(
            is_complete_sequence("\x1b[<35;20;5m"),
            SequenceStatus::Complete
        );
        assert_eq!(
            is_complete_sequence("\x1b[<35;20;5"),
            SequenceStatus::Incomplete
        );
        assert_eq!(
            is_complete_sequence("\x1b]0;t\x07"),
            SequenceStatus::Complete
        );
        assert_eq!(
            is_complete_sequence("\x1bP>|x\x1b\\"),
            SequenceStatus::Complete
        );
        assert_eq!(
            is_complete_sequence("\x1b_Gi=1;OK\x1b\\"),
            SequenceStatus::Complete
        );
    }
}
