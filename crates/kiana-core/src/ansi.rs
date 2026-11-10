//! utils/ansi.ts。
//!
//! TS: `packages/coding-agent/src/utils/ansi.ts`（82 行，派生自 ansi-regex / strip-ansi）。
//!
//! ```ts
//! const ST = "(?:\\u0007|\\u001B\\u005C|\\u009C)";
//! const OSC_START = "\\u001B\\]";
//! const CSI_START = "[\\u001B\\u009B][[\\]()#;?]*(?:\\d{1,4}(?:[;:]\\d{0,4})*)?";
//! const CSI_FINAL = "[\\dA-PR-TZcf-nq-uy=><~]";
//! const ansiRegex = new RegExp(`(?:${OSC_START}[\\s\\S]*?${ST})|${CSI_START}${CSI_FINAL}`, "g");
//! const unfinishedAnsiAtEndRegex = new RegExp(
//!   `(?:${OSC_START}(?:[^\\u0007\\u009C\\u001B]|\\u001B(?!\\\\))*|${CSI_START})$`,
//! );
//! const MAX_PENDING_ANSI_LENGTH = 256;
//! ```
//!
//! ## 正则方言（`spec/ca-cli.md` §2.21.1 / §8.2 第 8 条）
//!
//! - `\u009B` 写成 `\x{009B}`；`[\s\S]` 写成 `(?s:.)`；
//! - JS 的 `\d` 恒为 ASCII `[0-9]`（Rust `regex` 的 `\d` 是 Unicode `Nd`），显式展开；
//! - `unfinishedAnsiAtEndRegex` 含负向前瞻 `\u001B(?!\\)`，Rust `regex` 不支持前瞻，
//!   因此该正则**手写匹配**（等价语言：OSC 起始 + 到结尾都不含 BEL/0x9C/`ESC\` 的载荷，
//!   或恰好以 `CSI_START` 结尾）。
//!
//! ## UTF-16（`ARCHITECTURE.md` §4.6）
//!
//! `value.length` / `slice` 按 UTF-16 code unit：`MAX_PENDING_ANSI_LENGTH` 的窗口起点是
//! `max(0, len - 256)`（code unit）。实现先把字符串转成 `Vec<u16>` 再按窗口扫描；
//! 若切点落在代理对中间，把切点向后移一个 code unit（`split_at += 1`），让整个代理对留在
//! `complete`，不拆开代理对（§4.6 的「截断不拆代理对」；Rust 的 `String` 也不能表示孤立
//! 代理项）。按当前实现，匹配起点恒为 `ESC`（U+001B）或 `U+009B`，两者都是 BMP 码点、
//! 不可能落在代理对内部，所以“切点在代理对中间”只是防御性分支：
//! `crates/kiana-core/tests/fixtures/ansi_corpus.json` 的切点都在代理对边界上，未触发它。
//!
//! `stripAnsi` 对非 string 抛 `TypeError` 的分支在 `&str` API 下不可表示（文档说明）。
//!
//! ## 流式用法（调用方契约）
//!
//! [`split_incomplete_ansi_suffix`] 只负责找“末尾未完成的转义序列”，`complete` 里仍可能
//! 含有完整的 ANSI 序列，需要再调用 [`strip_ansi`] 或交给按 ANSI 解析的渲染器。典型顺序是
//! `pending + 下一块 → split → 处理 complete → 保留新的 pending`，最后由调用方决定
//! 流结束时是否丢弃 pending；`crates/kiana-core/tests/ansi_utils.rs` 的
//! `streaming_split_keeps_incomplete_escape_sequences_pending` 演示了该顺序。
//!
//! 判断只在最后 256 个 UTF-16 code unit 的窗口内进行，因此超过窗口仍不完整的序列会被
//! 当作普通文本立即放行（`overly_long_unterminated_sequences_are_not_held_back`，
//! 测试注释写明“更长的未终止序列不无限扣留”）。这是有界行为，不是漏匹配。

use std::sync::LazyLock;

use regex::Regex;

/// `MAX_PENDING_ANSI_LENGTH = 256`（TS 私有常量）。
const MAX_PENDING_ANSI_LENGTH: usize = 256;

const ST: &str = r"(?:\x{0007}|\x{001B}\x{005C}|\x{009C})";
const OSC_START: &str = r"\x{001B}\]";
const CSI_START: &str = r"[\x{001B}\x{009B}][\[\]()#;?]*(?:[0-9]{1,4}(?:[;:][0-9]{0,4})*)?";
const CSI_FINAL: &str = r"[0-9A-PR-TZcf-nq-uy=><~]";

/// TS: `ansiRegex`。
static ANSI_RE: LazyLock<Regex> = LazyLock::new(|| {
    let pattern = format!("(?:{OSC_START}(?s:.)*?{ST})|{CSI_START}{CSI_FINAL}");
    Regex::new(&pattern).expect("valid ansi regex")
});

/// TS: `splitIncompleteAnsiSuffix` 的返回值。
///
/// `complete` 可以立即 `strip_ansi`；`pending` 是末尾未完成的转义序列，应拼到下一块前面。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnsiSplit {
    /// 当前可以安全处理的前缀；其中仍可能含有完整的 ANSI 序列。
    pub complete: String,
    /// 末尾未完成的转义序列（没有则为空串）；调用方必须把它拼到下一块的前面。
    pub pending: String,
}

/// TS: `stripAnsi(value)` —— 去掉所有完整 ANSI 转义序列。
///
/// 不含 `ESC`（U+001B）与 `U+009B` 时原样返回（快速路径）。
///
/// 只删除**完整**的 OSC/CSI 序列：末尾被截断的序列（流式读取时的一块边界）不属于
/// `ANSI_RE` 的匹配范围，会原样留在结果里；这种输入应先用
/// [`split_incomplete_ansi_suffix`] 把 pending 留到下一块。
pub fn strip_ansi(value: &str) -> String {
    if !value.contains('\u{001B}') && !value.contains('\u{009B}') {
        return value.to_string();
    }
    ANSI_RE.replace_all(value, "").into_owned()
}

/// TS: `splitIncompleteAnsiSuffix(value)` —— 把流式文本拆成「现在可安全处理的前缀」与
/// 「末尾未完成的转义序列」。
///
/// 输入是**已经拼接好的当前累积文本**（调用方负责把上一轮的 `pending` 加回开头），
/// 输出见 [`AnsiSplit`]。切点按 UTF-16 code unit 定位。匹配起点恒为 `ESC`/`U+009B`
/// （BMP 码点），理论上不会落在代理对内部；代码仍保留“落点若在代理对中间就后移一个
/// code unit”的防御性调整（`ARCHITECTURE.md` §4.6 的「截断不拆代理对」）。
///
/// 没有 `ESC`/`U+009B` 时是快速路径：整串进 `complete`，`pending` 为空。
pub fn split_incomplete_ansi_suffix(value: &str) -> AnsiSplit {
    if !value.contains('\u{001B}') && !value.contains('\u{009B}') {
        return AnsiSplit {
            complete: value.to_string(),
            pending: String::new(),
        };
    }

    let units: Vec<u16> = value.encode_utf16().collect();
    let window_start = units.len().saturating_sub(MAX_PENDING_ANSI_LENGTH);
    let window = &units[window_start..];

    let Some(match_index) = find_unfinished_ansi_at_end(window) else {
        return AnsiSplit {
            complete: value.to_string(),
            pending: String::new(),
        };
    };

    let mut split_at = window_start + match_index;
    let byte_index = match utf16_byte_index(value, split_at) {
        Some(index) => index,
        None => {
            // 防御性分支（匹配起点恒为 ESC/0x9B，理论上不会落在代理对内部）：把切点后移
            // 到下一个 code unit 边界，让整个代理对留在 `complete`（`String` 不能表示
            // 孤立的代理项）。
            split_at += 1;
            utf16_byte_index(value, split_at).unwrap_or(value.len())
        }
    };

    AnsiSplit {
        complete: value[..byte_index].to_string(),
        pending: value[byte_index..].to_string(),
    }
}

/// `splitAt` 的 UTF-16 code unit 下标 → UTF-8 字节下标；落在代理对中间时返回 `None`。
fn utf16_byte_index(value: &str, target: usize) -> Option<usize> {
    let mut units = 0usize;
    for (byte_index, ch) in value.char_indices() {
        if units == target {
            return Some(byte_index);
        }
        units += ch.len_utf16();
        if units > target {
            return None;
        }
    }
    (units == target).then_some(value.len())
}

fn is_intermediate(unit: u16) -> bool {
    // `[`, `]`, `(`, `)`, `#`, `;`, `?`（对应 `CSI_START` 里的字符类 `[\[\]()#;?]`）
    matches!(unit, 0x5B | 0x5D | 0x28 | 0x29 | 0x23 | 0x3B | 0x3F)
}

fn is_ascii_digit(unit: u16) -> bool {
    (b'0' as u16..=b'9' as u16).contains(&unit)
}

/// `(?:[0-9]{1,4}(?:[;:][0-9]{0,4})*)?$`（空串合法）：判断 CSI 的参数字段是否恰好
/// 延续到 `units` 末尾。允许 0 个参数；否则首组为 1..4 位 ASCII 数字，后续每组由
/// `;`/`:` 引出、可有 0..4 位数字（组数不限）。
fn params_tail_matches(units: &[u16]) -> bool {
    if units.is_empty() {
        return true;
    }
    let mut index = 0usize;
    let mut digits = 0usize;
    while index < units.len() && is_ascii_digit(units[index]) && digits < 4 {
        index += 1;
        digits += 1;
    }
    if digits == 0 {
        return false;
    }
    loop {
        if index == units.len() {
            return true;
        }
        if units[index] != b';' as u16 && units[index] != b':' as u16 {
            return false;
        }
        index += 1;
        let mut trailing = 0usize;
        while index < units.len() && is_ascii_digit(units[index]) && trailing < 4 {
            index += 1;
            trailing += 1;
        }
    }
}

/// `CSI_START$`：整个 `units`（从候选起点到窗口末尾）必须恰好是一条不完整的 CSI。
///
/// `CSI_START` 的结构是「起始字节 + 任意个中间字节 + 可选参数」，参数之后还没有 final
/// 字节，所以尾部只能是中间字节或参数字符；循环依次尝试“现在就到参数段”和“再消费一个
/// 中间字节”，对应正则的回溯选择。
fn csi_start_matches_exact(units: &[u16]) -> bool {
    if units.is_empty() || (units[0] != 0x001B && units[0] != 0x009B) {
        return false;
    }
    let mut index = 1usize;
    loop {
        if params_tail_matches(&units[index..]) {
            return true;
        }
        if index < units.len() && is_intermediate(units[index]) {
            index += 1;
        } else {
            return false;
        }
    }
}

/// `unfinishedAnsiAtEndRegex.exec(window)` 的等价实现：返回最左匹配的起点（相对
/// `window` 的 UTF-16 code unit 偏移）；`None` 表示末尾没有未完成的转义序列。
///
/// `window` 是完整文本最后最多 256 个 code unit 的切片（窗口外的前缀不参与判断）。
/// 两个分支分别对应正则的两个 alternative（OSC 未终止 / 恰好以 CSI_START 结尾），
/// 都取各自最左起点，再返回较小者，等价于正则 alternation 的最左优先。
fn find_unfinished_ansi_at_end(window: &[u16]) -> Option<usize> {
    // OSC 分支：`OSC_START PAYLOAD* $`，PAYLOAD 不含 BEL/0x9C/`ESC\`。
    // 设最后一个禁止字符（BEL/0x9C，或 `ESC\` 的 ESC）在 `last_break`，
    // 则 OSC 起点必须满足 `i + 2 > last_break`，即 `i >= last_break - 1`。
    let mut last_break: Option<usize> = None;
    for index in 0..window.len() {
        let unit = window[index];
        let is_break = unit == 0x0007
            || unit == 0x009C
            || (unit == 0x001B
                && index + 1 < window.len()
                && window[index + 1] == u16::from(b'\\'));
        if is_break {
            last_break = Some(index);
        }
    }

    let osc_search_start = match last_break {
        Some(index) => index.saturating_sub(1),
        None => 0,
    };
    let osc_start = (osc_search_start..window.len()).find(|&index| {
        window[index] == 0x001B && index + 1 < window.len() && window[index + 1] == u16::from(b']')
    });

    let csi_start = (0..window.len()).find(|&index| {
        (window[index] == 0x001B || window[index] == 0x009B)
            && csi_start_matches_exact(&window[index..])
    });

    match (osc_start, csi_start) {
        (Some(osc), Some(csi)) => Some(osc.min(csi)),
        (Some(index), None) | (None, Some(index)) => Some(index),
        (None, None) => None,
    }
}
