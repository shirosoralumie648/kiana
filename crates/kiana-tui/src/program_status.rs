//! program-status.ts。
//!
//! Program Status Protocol (OSC 7501): a program tells the terminal whether it is idle, working,
//! blocked on the user, done, or failed. Only the root record is supported.
//!
//! Spec: <https://www.superlogical.com/rex/docs/build/program-status>
//!
//! "Only the root record is supported" 是行为约束：OSC 7501 允许嵌套/子记录，本实现只处理最外层
//! record，不对子记录做任何递归或去重。
//!
//! 类型 [`ProgramStatus`] 定义在 [`crate::terminal_colors`]（`ARCHITECTURE.md` §4.9 的冻结契约）；
//! 本文件只放编码函数，避免出现第二份类型定义。
//!
//! ## 编码流程（[`format_program_status`]）
//!
//! 1. `state` 永远是第一项；`app` 仅当匹配 `^[A-Za-z0-9_.+-]{1,32}$` 时才加入；`kind`
//!    仅当 `state == blocked` 且存在时才加入。
//! 2. `message` 先做控制字符替换（连续控制字符折叠成一个空格，范围 U+0000–U+001F、
//!    U+007F–U+009F），再按 JS `String.prototype.trim()` 的空白集合去首尾
//!    （`crate::text::trim_js`），最后按 UTF-8 字节截到 `MAX_MESSAGE_BYTES`（2048），
//!    切点不会切断字符。
//! 3. 非空消息用标准 base64（带 `=` 填充）编码为 `msg=`；所有项用 `:` 连接，外层是
//!    `ESC ] 7501 ; … ESC \`。
//!
//! 消息上限的依据：2048 字节的 base64 文本是 2732 字节，加上其它字段仍在
//! `tests/program_status.rs` 断言的 4096 字节序列长度内。
//!
//! ## 探测回复（[`is_program_status_reply`]）
//!
//! 终端对 [`PROGRAM_STATUS_QUERY`] 的回复必须整体匹配：前缀 `ESC ] 7501 ; ?`，正文不含
//! BEL/ESC，结尾为 ST（`ESC \`）或 BEL。`terminal.rs` 的 `handle_sequence` 在等待探测回复
//! 期间消费第一条匹配的序列（不转发给输入处理器），据此把终端标记为支持并补发缓存状态；
//! 之后的同类序列仍然只丢弃、不转发。
//!
//! ## 注意事项
//!
//! - `MAX_MESSAGE_BYTES` 是私有常量，本模块没有公开的消息长度配置，调用方无法调大。
//! - `app` 的长度按 **char** 计数；正则本身只允许 ASCII，因此与字节数一致。
//! - 本模块只编码最外层 record（见上），不解析任何收到的状态报告，也不处理嵌套子记录。

use crate::terminal_colors::{ProgramStatus, ProgramStatusKind, ProgramStatusState};
use crate::text::trim_js;

/// 特性探测查询。支持的终端会回送同样的 body（`PROGRAM_STATUS_QUERY`）。
pub const PROGRAM_STATUS_QUERY: &str = "\x1b]7501;?\x1b\\";

/// 解码后的 `msg` 上限；其 base64 编码仍在 2732 字节的编码上限之内。
const MAX_MESSAGE_BYTES: usize = 2048;

/// `isProgramStatusReply(sequence)`：是否为 [`PROGRAM_STATUS_QUERY`] 的回复。
///
/// 后续规范版本可在 `?` 之后追加字段。
///
/// 判定条件（三条同时满足）：
///
/// 1. 以 `ESC ] 7501 ; ?` 开头；
/// 2. 以 ST（`ESC \`）或 BEL 结尾（先尝试 ST，再尝试 BEL）；
/// 3. 终结符之前的正文不含 BEL 或 ESC。
///
/// 因此空回复（`\x1b]7501;?\x1b\\`）与 `?version=2` 之类的扩展字段都算回复；`state=…`
/// 形式的真实报告和其它 OSC 序列返回 `false`。纯字符串判断，不会 panic。
pub fn is_program_status_reply(sequence: &str) -> bool {
    // 前缀必须精确匹配：`?` 之后才是可扩展字段/正文。
    let Some(body) = sequence.strip_prefix("\x1b]7501;?") else {
        return false;
    };
    // 终结符二选一：ST（ESC \）或 BEL；两者都不是则不是完整序列。
    let body = if let Some(body) = body.strip_suffix("\x1b\\") {
        body
    } else if let Some(body) = body.strip_suffix('\x07') {
        body
    } else {
        return false;
    };
    // 正文不能再出现 BEL/ESC，否则说明回复被拼接进了别的输入。
    !body.contains(['\x07', '\x1b'])
}

/// `APP_PATTERN = /^[A-Za-z0-9_.+-]{1,32}$/`。
///
/// 长度按 `char` 计（1..=32），字符只允许 ASCII 字母数字以及 `_`、`.`、`+`、`-`；
/// 空串、含空格或非 ASCII 字符的名字都返回 `false`，调用方会静默省略 `app=` 项。
fn is_valid_app(app: &str) -> bool {
    // 长度按 char 计；字符集检查保证实际只会是 ASCII。
    let len = app.chars().count();
    (1..=32).contains(&len)
        && app
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '.' | '+' | '-'))
}

/// `CONTROL_CHARACTERS = /[\u0000-\u001f\u007f-\u009f]+/g` 的替换为单个空格。
///
/// 连续的一段控制字符只产生一个空格（对应正则的 `+`），其它字符原样保留。这样消息里
/// 不会残留会提前终止 OSC 序列或让终端丢弃报告的字节（见 `tests/program_status.rs`）。
fn replace_control_characters(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut in_run = false;
    for ch in text.chars() {
        if ch.is_ascii_control() || ('\u{7f}'..='\u{9f}').contains(&ch) {
            // 只在进入一段控制字符时补一个空格，连续的控制字符因此折叠成一个。
            if !in_run {
                out.push(' ');
                in_run = true;
            }
        } else {
            out.push(ch);
            in_run = false;
        }
    }
    out
}

/// `truncateUtf8(text, maxBytes)`：按 UTF-8 字节数截断，切点落在 code point 边界。
///
/// 逐 `char` 累加 `len_utf8()`，一旦加上下一个字符会超过 `max_bytes` 就停止。返回新
/// `String`：不追加省略号，也不会切出半个 UTF-8 序列；输入本身不超限时直接克隆。
fn truncate_utf8(text: &str, max_bytes: usize) -> String {
    if text.len() <= max_bytes {
        return text.to_string();
    }
    let mut bytes = 0usize;
    let mut end = 0usize;
    for ch in text.chars() {
        let size = ch.len_utf8();
        if bytes + size > max_bytes {
            break;
        }
        bytes += size;
        end += ch.len_utf8();
    }
    // `end` 是按字节累加的，切点必然落在 char 边界。
    text[..end].to_string()
}

/// `formatProgramStatus(status)`：编码一条 OSC 7501 状态报告。
///
/// 终端会丢弃文本中含控制字符的报告，因此控制字符先被替换为空格。
///
/// 参数与结果：
/// - `status` 只读借用，不会被修改；
/// - 返回完整的 OSC 序列（固定以 ST `ESC \` 结束，而不是 BEL）；
/// - 项顺序固定为 `state`、`app`、`kind`、`msg`，用 `:` 连接；非法/不适用的项**整项省略**；
/// - 无错误路径：base64 编码不会失败，空消息直接省略 `msg=`。
pub fn format_program_status(status: &ProgramStatus) -> String {
    // state 永远是第一项；枚举封闭，state_name 必有返回值。
    let mut pairs = vec![format!("state={}", state_name(status.state))];
    // app 非法（空、超长、含非法字符）时整项省略，与 TS 的“静默忽略”一致。
    if let Some(app) = status.app.as_deref()
        && is_valid_app(app)
    {
        pairs.push(format!("app={app}"));
    }
    // kind 只在 blocked 状态有意义；其它状态即使带了 kind 也不输出。
    if status.state == ProgramStatusState::Blocked
        && let Some(kind) = status.kind
    {
        pairs.push(format!("kind={}", kind_name(kind)));
    }
    // 清洗顺序不能换：控制字符 → JS trim → 按 UTF-8 字节截断。
    let raw_message = status.message.as_deref().unwrap_or("");
    let message = truncate_utf8(
        trim_js(&replace_control_characters(raw_message)),
        MAX_MESSAGE_BYTES,
    );
    if !message.is_empty() {
        use base64::Engine as _;
        // 标准 base64（含 `=` 填充），与 `Buffer.toString("base64")` 的输出一致。
        let encoded = base64::engine::general_purpose::STANDARD.encode(message.as_bytes());
        pairs.push(format!("msg={encoded}"));
    }
    // 固定以 ST 结束；BEL 只出现在“识别回复”一侧，这里不产生。
    format!("\x1b]7501;{}\x1b\\", pairs.join(":"))
}

/// `ProgramStatus.state` 的协议拼写。
///
/// 枚举是封闭的，因此没有兜底字符串；返回值直接进入 `state=` 项（小写，与 serde 的
/// `rename_all = "lowercase"` 一致）。
pub fn state_name(state: ProgramStatusState) -> &'static str {
    match state {
        ProgramStatusState::Idle => "idle",
        ProgramStatusState::Working => "working",
        ProgramStatusState::Blocked => "blocked",
        ProgramStatusState::Done => "done",
        ProgramStatusState::Error => "error",
        ProgramStatusState::Clear => "clear",
    }
}

/// `ProgramStatus.kind` 的协议拼写。
///
/// 只在 `state == blocked` 时由 [`format_program_status`] 调用；返回小写拼写。
pub fn kind_name(kind: ProgramStatusKind) -> &'static str {
    match kind {
        ProgramStatusKind::Permission => "permission",
        ProgramStatusKind::Question => "question",
        ProgramStatusKind::Auth => "auth",
    }
}
