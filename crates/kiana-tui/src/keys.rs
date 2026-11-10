//! keys.ts / native-modifiers.ts。
//!
//! 终端按键输入处理：同时支持 legacy 序列与 Kitty keyboard protocol
//! （<https://sw.kovidgoyal.net/kitty/keyboard-protocol/>）。
//!
//! 符号键也受支持，但部分 ctrl+符号 与 ASCII 码重叠（例如 ctrl+[ = ESC），
//! 见 <https://sw.kovidgoyal.net/kitty/keyboard-protocol/#legacy-ctrl-mapping-of-ascii-keys>。
//! 这些组合仍可用于 ctrl+shift。
//!
//! API：
//! - [`matches_key`]：判断输入是否匹配键标识符；
//! - [`parse_key`]：解析输入并返回键标识符；
//! - [`Key`]：构造键标识符的辅助常量/函数；
//! - [`set_kitty_protocol_active`] / [`is_kitty_protocol_active`]：Kitty 协议状态（线程级）。
//!
//! `native-modifiers.ts` 的实现落在 [`crate::native`]；本文件保留其再导出面。
//!
//! # 为什么需要这个模块
//!
//! 同一次按键在不同终端、不同协商结果下会变成不同的字节序列：老式终端发 legacy 序列
//! （方向键 `\x1b[A`、Ctrl+C 的 `\x03`），Kitty keyboard protocol 或 xterm
//! modifyOtherKeys 则发带修饰符/事件类型的 CSI 序列（`\x1b[99;5u`、`\x1b[27;5;99~`）。
//! 上层只关心“这是不是绑定里写的 `ctrl+c`”，因此本模块把各种编码统一成字符串键标识符，
//! 并提供两个方向的操作：
//!
//! - [`matches_key`]：判定一条输入是否匹配某个键标识符（键位绑定分派）；
//! - [`parse_key`]：把一条输入还原成键标识符（调试展示、扩展 API）；
//! - [`decode_kitty_printable`] / [`decode_printable_key`]：只取可打印字符（文本输入），
//!   不判断绑定。
//!
//! # 被谁调用
//!
//! - `crate::tui` 在分发输入前用 [`matches_key`] 识别全局键，并用 [`is_key_release`]
//!   过滤 Kitty 的释放事件；
//! - `crate::keybindings` 的 `KeybindingsManager::matches` 对每个候选键调用
//!   [`matches_key`]；
//! - `crate::components::editor` / `crate::components::input` 用
//!   [`decode_printable_key`] / [`decode_kitty_printable`] 取要插入的字符；
//! - `crate::terminal` 在键盘协议协商成功、停止与 `drainInput` 时调用
//!   [`set_kitty_protocol_active`]；
//! - 下游 crate 也直接调用：[`matches_key`] / [`parse_key`] 被 `kiana-extensions` 的
//!   JS 桥接导出，`kiana-interactive` 用它识别快捷键。
//!
//! # 主要流程
//!
//! [`matches_key`] 先用 `parse_key_id` 把键标识符拆成“基键 + 修饰符位”，再按基键类型
//! 进入对应分支；各分支自行组合“显式协议序列”与“legacy 序列”的尝试顺序。
//! [`parse_key`] 则按固定顺序识别输入：CSI-u → modifyOtherKeys → legacy 表/字节。
//! 两者都依赖 [`is_kitty_protocol_active`]：只有在 Kitty 协议**未**激活时，带歧义的
//! legacy 字节（`\x1b\r`、`\x1b `、`\n` 等）才按 legacy 语义解释；显式 CSI 序列
//! 无论协议是否激活都会被识别（见单元测试
//! `matches_ctrl_alt_letter_via_csi_u_and_modify_other_keys_when_kitty_inactive`）。
//!
//! # 重要约束
//!
//! - **输入粒度**：`data` 被当作一条完整的按键序列整体比较（函数中大量 `data == "..."`），
//!   不做前缀匹配。拆包由 `crate::stdin` 的 `StdinBuffer` 完成，本模块假设上游已经切好。
//! - **修饰符位**：`shift=1`、`alt=2`、`ctrl=4`、`super=8`；Kitty 与 modifyOtherKeys 的
//!   修饰数字都是“位掩码 + 1”，所以解析后统一减一。Caps Lock / Num Lock 位（64/128）
//!   只在 Kitty 匹配、Shift 身份规范化、键名格式化与 `decode_modify_other_keys_printable`
//!   中被 `LOCK_MASK` 屏蔽；`matches_modify_other_keys` 与 `matches_printable_modify_other_keys`
//!   的修饰符相等比较都保留这两位。
//!   测试固定了这几位：`\x1b[107;9u` 是 super+k（8），
//!   `\x1b[107;13u` 是 ctrl+super+k（4+8）。
//! - **Kitty 状态是线程级**：`KITTY_PROTOCOL_ACTIVE` 是 `thread_local!`，在别的线程读取
//!   会得到默认值 `false`；测试因此用互斥量把用例串行化（见文件末尾 `kitty_guard`）。
//! - **键标识符是字符串**：修饰符顺序任意（`ctrl+shift+p` 与 `shift+ctrl+p` 等价），但
//!   [`parse_key`] 只输出一种规范顺序：shift、ctrl、alt、super。
//!
//! # 阅读顺序
//!
//! 1. 全局 Kitty 状态与 [`Key`]（键标识符词汇表）；
//! 2. 常量表与 `parse_key_id`（标识符如何拆解）；
//! 3. `parse_kitty_sequence` / `parse_modify_other_keys_sequence`（CSI 序列解析）；
//! 4. [`matches_key`]（匹配主流程）与 [`parse_key`]（反向格式化）；
//! 5. [`decode_kitty_printable`] / [`decode_printable_key`]（文本输入）。

use std::cell::Cell;

pub use crate::native::{ModifierKey, is_native_modifier_pressed};

// =============================================================================
// 全局 Kitty 协议状态
// =============================================================================

thread_local! {
    // 每个线程一份副本，默认 false。它不是进程级共享状态：写入线程之外调用
    // `is_kitty_protocol_active` 仍会得到 false（测试里显式串行化就是为了这点）。
    static KITTY_PROTOCOL_ACTIVE: Cell<bool> = const { Cell::new(false) };
}

/// 设置 Kitty keyboard protocol 状态（`setKittyProtocolActive`）。
///
/// 由 `crate::terminal` 的 `ProcessTerminal` 在协商成功、停止与 `drainInput` 时设置。
///
/// 注意：状态存放在 `thread_local!` 里，**不是**跨线程共享的进程级变量；在另一个线程
/// 调用 [`is_kitty_protocol_active`] 会读到该线程的默认值 `false`。因此写入与读取应发生在
/// 同一线程（TUI 的输入分发线程）。测试用互斥量串行化正是为了避免不同测试线程互相干扰。
pub fn set_kitty_protocol_active(active: bool) {
    KITTY_PROTOCOL_ACTIVE.with(|cell| cell.set(active));
}

/// 查询当前线程的 Kitty keyboard protocol 是否激活（`isKittyProtocolActive`）。
///
/// 返回值会影响 [`matches_key`] / [`parse_key`] 对歧义 legacy 字节的解释（详见模块文档
/// 的“主要流程”）。默认 `false`，即按老式终端语义解析。
pub fn is_kitty_protocol_active() -> bool {
    KITTY_PROTOCOL_ACTIVE.with(Cell::get)
}

// =============================================================================
// 键标识符
// =============================================================================

/// 键标识符，例如 `"escape"`、`"ctrl+c"`、`"ctrl+shift+p"`（TS `KeyId`）。
///
/// TS 的 `KeyId` 是字符串字面量联合；Rust 侧同样用字符串，因为修饰符顺序任意且运行时
/// 也可能来自用户配置（`KeybindingsConfig`）。[`Key`] 提供常用键的**常量**。
pub type KeyId = String;

/// 构造键标识符的辅助对象（TS `Key`）。
///
/// `Key::ctrl("c")` / `Key::ctrl_shift("p")` 返回与 TS 相同的字符串；Rust 没有模板字符串，
/// 所以返回 `String`（调用方需要 `&'static str` 时用 [`KEY`] 里的常量）。
///
/// 常量给出的是**规范的键标识符字符串**，可直接写进 `KeybindingsConfig`。
/// `Key::ESC` / `Key::RETURN` 分别是 `"escape"` / `"enter"` 的别名：两者经 `parse_key_id`
/// 映射到同一个哨兵字符，所以匹配行为完全相同，但 [`parse_key`] 只会输出
/// `"escape"` / `"enter"`。符号键常量的值就是符号本身（例如 [`Key::PLUS`] 是 `"+"`），
/// 因此 `Key::ctrl("+")` 得到 `"ctrl++"`；`parse_key_id` 专门允许键名部分恰为单个 `+`。
///
/// 这些构造函数只拼接字符串，不校验 `key` 是否合法；非法标识符会由 [`matches_key`] 一侧
/// 的 `parse_key_id` 拒绝（结果为 `false`）。
pub struct Key;

impl Key {
    // 特殊键
    /// Escape 键（`"escape"`）；[`parse_key`] 对单字节 ESC 输出这个标识符。
    pub const ESCAPE: &'static str = "escape";
    /// `"esc"`：Escape 的别名，与 `"escape"` 映射到同一哨兵字符。
    pub const ESC: &'static str = "esc";
    /// Enter 键（`"enter"`）；`\r`、Kitty 未激活时的 `\n`、小键盘 Enter 都归一到这里。
    pub const ENTER: &'static str = "enter";
    /// `"return"`：Enter 的别名，与 `"enter"` 等价。
    pub const RETURN: &'static str = "return";
    /// Tab 键（`"tab"`）；Shift+Tab 的 legacy 序列是 CSI `Z`。
    pub const TAB: &'static str = "tab";
    /// 空格键（`"space"`）；legacy Ctrl+Space 是 NUL 字节 `"\x00"`。
    pub const SPACE: &'static str = "space";
    /// 退格键（`"backspace"`）；legacy 的 0x7f/0x08 歧义见 `matches_raw_backspace`。
    pub const BACKSPACE: &'static str = "backspace";
    /// Delete 键（`"delete"`）。
    pub const DELETE: &'static str = "delete";
    /// Insert 键（`"insert"`）。
    pub const INSERT: &'static str = "insert";
    /// Clear 键（`"clear"`）；只有 legacy 序列与 shift/ctrl 修饰序列，没有 CSI-u 路径。
    pub const CLEAR: &'static str = "clear";
    /// Home 键（`"home"`）。
    pub const HOME: &'static str = "home";
    /// End 键（`"end"`）。
    pub const END: &'static str = "end";
    /// PageUp 键；规范拼写是驼峰 `"pageUp"`（匹配时 `parse_key_id` 先转小写，故 `"pageup"` 也可）。
    pub const PAGE_UP: &'static str = "pageUp";
    /// PageDown 键；规范拼写是驼峰 `"pageDown"`。
    pub const PAGE_DOWN: &'static str = "pageDown";
    /// 方向上键（`"up"`）。
    pub const UP: &'static str = "up";
    /// 方向下键（`"down"`）。
    pub const DOWN: &'static str = "down";
    /// 方向左键（`"left"`）。
    pub const LEFT: &'static str = "left";
    /// 方向右键（`"right"`）。
    pub const RIGHT: &'static str = "right";
    /// 功能键 F1（`"f1"`）；只支持无修饰的 legacy 序列。
    pub const F1: &'static str = "f1";
    /// 功能键 F2（`"f2"`）；只支持无修饰的 legacy 序列。
    pub const F2: &'static str = "f2";
    /// 功能键 F3（`"f3"`）；只支持无修饰的 legacy 序列。
    pub const F3: &'static str = "f3";
    /// 功能键 F4（`"f4"`）；只支持无修饰的 legacy 序列。
    pub const F4: &'static str = "f4";
    /// 功能键 F5（`"f5"`）；只支持无修饰的 legacy 序列。
    pub const F5: &'static str = "f5";
    /// 功能键 F6（`"f6"`）；只支持无修饰的 legacy 序列。
    pub const F6: &'static str = "f6";
    /// 功能键 F7（`"f7"`）；只支持无修饰的 legacy 序列。
    pub const F7: &'static str = "f7";
    /// 功能键 F8（`"f8"`）；只支持无修饰的 legacy 序列。
    pub const F8: &'static str = "f8";
    /// 功能键 F9（`"f9"`）；只支持无修饰的 legacy 序列。
    pub const F9: &'static str = "f9";
    /// 功能键 F10（`"f10"`）；只支持无修饰的 legacy 序列。
    pub const F10: &'static str = "f10";
    /// 功能键 F11（`"f11"`）；只支持无修饰的 legacy 序列。
    pub const F11: &'static str = "f11";
    /// 功能键 F12（`"f12"`）；只支持无修饰的 legacy 序列。
    pub const F12: &'static str = "f12";

    // 符号键（名称 → 字面量）
    // 值就是该键产生的字符，直接作为键标识符使用。
    /// 反引号键。
    pub const BACKTICK: &'static str = "`";
    /// 连字符/减号 `-`；物理上与 `_` 同键，legacy Ctrl 形式都映射到 0x1f。
    pub const HYPHEN: &'static str = "-";
    /// 等号 `=`；Shift 下的 `+` 通过 shifted key 或逻辑匹配消费隐含 Shift。
    pub const EQUALS: &'static str = "=";
    /// 左方括号 `[`；legacy Ctrl+[ 的字节与 ESC 相同（0x1b）。
    pub const LEFT_BRACKET: &'static str = "[";
    /// 右方括号 `]`；legacy Ctrl+] 是 0x1d。
    pub const RIGHT_BRACKET: &'static str = "]";
    /// 反斜杠 `\`；legacy Ctrl+\ 是 0x1c。
    pub const BACKSLASH: &'static str = "\\";
    /// 分号 `;`。
    pub const SEMICOLON: &'static str = ";";
    /// 单引号 `'`。
    pub const QUOTE: &'static str = "'";
    /// 逗号 `,`。
    pub const COMMA: &'static str = ",";
    /// 句点 `.`。
    pub const PERIOD: &'static str = ".";
    /// 斜杠 `/`；不在 `raw_ctrl_char` 的映射表里，Ctrl+/ 只能由 CSI-u/modifyOtherKeys 匹配。
    pub const SLASH: &'static str = "/";
    /// 感叹号 `!`。
    pub const EXCLAMATION: &'static str = "!";
    /// at 符号 `@`。
    pub const AT: &'static str = "@";
    /// 井号 `#`。
    pub const HASH: &'static str = "#";
    /// 美元符 `$`。
    pub const DOLLAR: &'static str = "$";
    /// 百分号 `%`。
    pub const PERCENT: &'static str = "%";
    /// 脱字符 `^`。
    pub const CARET: &'static str = "^";
    /// 和号 `&`。
    pub const AMPERSAND: &'static str = "&";
    /// 星号 `*`。
    pub const ASTERISK: &'static str = "*";
    /// 左圆括号 `(`。
    pub const LEFT_PAREN: &'static str = "(";
    /// 右圆括号 `)`。
    pub const RIGHT_PAREN: &'static str = ")";
    /// 下划线 `_`；与 `-` 同键，legacy Ctrl 形式均为 0x1f。
    pub const UNDERSCORE: &'static str = "_";
    /// 加号 `+`；`+` 在键标识符里是修饰符分隔符，但 `parse_key_id` 允许键名部分恰为单个 `+`。
    pub const PLUS: &'static str = "+";
    /// 竖线 `|`。
    pub const PIPE: &'static str = "|";
    /// 波浪号 `~`。
    pub const TILDE: &'static str = "~";
    /// 左花括号 `{`。
    pub const LEFT_BRACE: &'static str = "{";
    /// 右花括号 `}`。
    pub const RIGHT_BRACE: &'static str = "}";
    /// 冒号 `:`。
    pub const COLON: &'static str = ":";
    /// 小于号 `<`。
    pub const LESS_THAN: &'static str = "<";
    /// 大于号 `>`。
    pub const GREATER_THAN: &'static str = ">";
    /// 问号 `?`。
    pub const QUESTION: &'static str = "?";

    /// `ctrl(k)`。
    pub fn ctrl(key: &str) -> String {
        format!("ctrl+{key}")
    }

    /// `shift(k)`。
    pub fn shift(key: &str) -> String {
        format!("shift+{key}")
    }

    /// `alt(k)`。
    pub fn alt(key: &str) -> String {
        format!("alt+{key}")
    }

    /// `super(k)`。
    pub fn super_key(key: &str) -> String {
        format!("super+{key}")
    }

    /// `ctrlShift(k)`。
    pub fn ctrl_shift(key: &str) -> String {
        format!("ctrl+shift+{key}")
    }

    /// `shiftCtrl(k)`。
    pub fn shift_ctrl(key: &str) -> String {
        format!("shift+ctrl+{key}")
    }

    /// `ctrlAlt(k)`。
    pub fn ctrl_alt(key: &str) -> String {
        format!("ctrl+alt+{key}")
    }

    /// `altCtrl(k)`。
    pub fn alt_ctrl(key: &str) -> String {
        format!("alt+ctrl+{key}")
    }

    /// `shiftAlt(k)`。
    pub fn shift_alt(key: &str) -> String {
        format!("shift+alt+{key}")
    }

    /// `altShift(k)`。
    pub fn alt_shift(key: &str) -> String {
        format!("alt+shift+{key}")
    }

    /// `ctrlSuper(k)`。
    pub fn ctrl_super(key: &str) -> String {
        format!("ctrl+super+{key}")
    }

    /// `superCtrl(k)`。
    pub fn super_ctrl(key: &str) -> String {
        format!("super+ctrl+{key}")
    }

    /// `shiftSuper(k)`。
    pub fn shift_super(key: &str) -> String {
        format!("shift+super+{key}")
    }

    /// `superShift(k)`。
    pub fn super_shift(key: &str) -> String {
        format!("super+shift+{key}")
    }

    /// `altSuper(k)`。
    pub fn alt_super(key: &str) -> String {
        format!("alt+super+{key}")
    }

    /// `superAlt(k)`。
    pub fn super_alt(key: &str) -> String {
        format!("super+alt+{key}")
    }

    /// `ctrlShiftAlt(k)`。
    pub fn ctrl_shift_alt(key: &str) -> String {
        format!("ctrl+shift+alt+{key}")
    }

    /// `ctrlShiftSuper(k)`。
    pub fn ctrl_shift_super(key: &str) -> String {
        format!("ctrl+shift+super+{key}")
    }
}

/// [`Key`] 的别名值（TS 的 `Key` 既是命名空间又是对象；Rust 侧用常量 `KEY` 表达值形态）。
///
/// `Key` 是零大小类型，`KEY` 只是为了保留 TS 的“类型 + 值”两种形态；Rust 中仍通过
/// `Key::ESCAPE` 这样的类型路径访问常量。目前仓库内没有使用 `KEY` 的调用点。
pub const KEY: Key = Key;

// =============================================================================
// 常量
// =============================================================================

/// 31 个符号键（TS `SYMBOL_KEYS`）。
///
/// 用途有两个：`is_symbol_key` 判断“这个字符是不是符号键”；Kitty 的
/// shifted key 逻辑用它确认终端上报的移位码点是一个可识别的符号。内容必须与 [`Key`] 的
/// 符号常量保持一致。
pub(crate) const SYMBOL_KEYS: [char; 31] = [
    '`', '-', '=', '[', ']', '\\', ';', '\'', ',', '.', '/', '!', '@', '#', '$', '%', '^', '&',
    '*', '(', ')', '_', '+', '|', '~', '{', '}', ':', '<', '>', '?',
];

/// 20 个移位后产生的符号（TS `SHIFTED_SYMBOL_KEYS`）。
///
/// 它是 `SYMBOL_KEYS` 的子集。只用于 modifyOtherKeys 的“逻辑匹配”：该协议直接上报码点，
/// 没有单独的 shifted key 字段，因此要以“这个码点是不是 Shift 产生的符号”来判断并消费
/// Shift 位。
pub(crate) const SHIFTED_SYMBOL_KEYS: [char; 20] = [
    '~', '!', '@', '#', '$', '%', '^', '&', '*', '(', ')', '_', '+', '|', '{', '}', ':', '<', '>',
    '?',
];

/// `MODIFIER_NAMES`（注意顺序：shift、alt、ctrl、super）。
///
/// `parse_key_id` 按这个顺序逐个尝试剥离修饰符前缀；四个名字互不为前缀，所以顺序不影响
/// 能否解析，只影响同一位置的扫描次序。
pub(crate) const MODIFIER_NAMES: [&str; 4] = ["shift", "alt", "ctrl", "super"];

// 修饰符位掩码，与 Kitty / modifyOtherKeys 的约定一致（协议上报的是“掩码 + 1”）。
// 测试锁定了映射关系：`\x1b[107;9u` = super+k、`\x1b[107;13u` = ctrl+super+k。
pub(crate) const MODIFIER_SHIFT: u32 = 1;
pub(crate) const MODIFIER_ALT: u32 = 2;
pub(crate) const MODIFIER_CTRL: u32 = 4;
pub(crate) const MODIFIER_SUPER: u32 = 8;

/// Caps Lock + Num Lock。
///
/// Kitty 与 xterm 都用 64/128 表示这两个锁定位。它们不影响按键身份，但**并非所有比较
/// 路径**都会屏蔽：
///
/// - 会 `& !LOCK_MASK` 屏蔽：`matches_kitty_sequence`（含其逻辑匹配分支）、
///   `normalize_shifted_letter_identity_codepoint`、`format_key_name_with_modifiers`、
///   `decode_modify_other_keys_printable`；
/// - 不屏蔽：`matches_modify_other_keys` 直接做 `parsed.modifier == expected_modifier`；
///   `matches_printable_modify_other_keys` 的物理比较同样如此，其逻辑比较也只去掉 SHIFT
///   位、保留锁定位；`decode_kitty_printable` 只是把锁定位放进允许集合
///   `KITTY_PRINTABLE_ALLOWED_MODIFIERS`，并不清除它们。
///
/// 后果：带锁定位的 modifyOtherKeys 输入（例如 modifier 为 68 = ctrl|Caps Lock 的
/// `\x1b[27;69;99~`）不会匹配 `ctrl+c`；要屏蔽锁定位必须改走其它匹配路径或修改比较逻辑。
const LOCK_MASK: u32 = 64 + 128;

// 内部统一码点约定：可打印字符用 Unicode 码点；方向键与编辑键用负数哨兵，
// 避免与任何真实字符码点冲突（kitty_functional_key_equivalent 会把协议码点折到这些值）。
const CODEPOINT_ESCAPE: i32 = 27;
const CODEPOINT_TAB: i32 = 9;
const CODEPOINT_ENTER: i32 = 13;
const CODEPOINT_SPACE: i32 = 32;
const CODEPOINT_BACKSPACE: i32 = 127;
/// Numpad Enter (Kitty protocol)。
const CODEPOINT_KP_ENTER: i32 = 57414;

const ARROW_UP: i32 = -1;
const ARROW_DOWN: i32 = -2;
const ARROW_RIGHT: i32 = -3;
const ARROW_LEFT: i32 = -4;

const FUNCTIONAL_DELETE: i32 = -10;
const FUNCTIONAL_INSERT: i32 = -11;
const FUNCTIONAL_PAGE_UP: i32 = -12;
const FUNCTIONAL_PAGE_DOWN: i32 = -13;
const FUNCTIONAL_HOME: i32 = -14;
const FUNCTIONAL_END: i32 = -15;

/// `KITTY_FUNCTIONAL_KEY_EQUIVALENTS`：Kitty codepoint → 逻辑等价码点。
///
/// 覆盖 Kitty 的小键盘区（57399–57408 → ASCII `0`–`9`）、运算符键与编辑键（57409–57426）。
/// 57414（KP_ENTER）没有映射：Enter 的匹配用 `CODEPOINT_KP_ENTER` 单独处理，本表只表达
/// “小键盘数字/符号/编辑键”。未覆盖的码点返回 `None`，调用方保留原码点。
fn kitty_functional_key_equivalent(codepoint: i32) -> Option<i32> {
    Some(match codepoint {
        57399..=57408 => 48 + (codepoint - 57399), // KP_0..KP_9
        57409 => 46,                               // KP_DECIMAL -> .
        57410 => 47,                               // KP_DIVIDE -> /
        57411 => 42,                               // KP_MULTIPLY -> *
        57412 => 45,                               // KP_SUBTRACT -> -
        57413 => 43,                               // KP_ADD -> +
        57415 => 61,                               // KP_EQUAL -> =
        57416 => 44,                               // KP_SEPARATOR -> ,
        57417 => ARROW_LEFT,
        57418 => ARROW_RIGHT,
        57419 => ARROW_UP,
        57420 => ARROW_DOWN,
        57421 => FUNCTIONAL_PAGE_UP,
        57422 => FUNCTIONAL_PAGE_DOWN,
        57423 => FUNCTIONAL_HOME,
        57424 => FUNCTIONAL_END,
        57425 => FUNCTIONAL_INSERT,
        57426 => FUNCTIONAL_DELETE,
        _ => return None,
    })
}

/// 若码点在 Kitty 的等价表中则换成逻辑码点，否则原样返回。
fn normalize_kitty_functional_codepoint(codepoint: i32) -> i32 {
    kitty_functional_key_equivalent(codepoint).unwrap_or(codepoint)
}

/// 把“Shift 按下时上报的大写拉丁字母”折成小写，其余码点原样返回。
///
/// Kitty 在 Shift 按下时可能上报大写码点（`C` = 67），而键标识符里的基键总是小写
/// （`parse_key_id` 会先 `to_lowercase`），所以比较前需要统一身份。只处理 ASCII `A`–`Z`；
/// 锁定位在判断前被去掉，本函数只关心 SHIFT 位。
fn normalize_shifted_letter_identity_codepoint(codepoint: i32, modifier: u32) -> i32 {
    let effective_modifier = modifier & !LOCK_MASK;
    if (effective_modifier & MODIFIER_SHIFT) != 0 && (65..=90).contains(&codepoint) {
        return codepoint + 32;
    }
    codepoint
}

fn is_symbol_key(ch: char) -> bool {
    SYMBOL_KEYS.contains(&ch)
}

// TS 没有 `isShiftedSymbolKey`：`SHIFTED_SYMBOL_KEYS` 只作为集合常量直接传参
// （`getLogicalShiftedSymbolCandidate(..., SHIFTED_SYMBOL_KEYS)`），因此这里也不需要包装函数。

/// char → 码点（TS `charCodeAt`/`fromCharCode` 只在 BMP 内工作；这里用 `u32` 表示）。
fn char_from_codepoint(codepoint: i32) -> Option<char> {
    if !(0..=0x10FFFF).contains(&codepoint) {
        return None;
    }
    char::from_u32(codepoint as u32)
}

fn codepoint_is(allowed: &[char], codepoint: i32) -> bool {
    char_from_codepoint(codepoint).is_some_and(|ch| allowed.contains(&ch))
}

/// `getLogicalShiftedSymbolCandidate(codepoint, modifier, allowed)`。
///
/// 当 Shift 按下、且终端上报的码点属于 `allowed` 中的符号时，返回
/// `(符号本身, 去掉 Shift 的修饰符)`；否则返回 `None`。这是“逻辑匹配”的关键：用户把
/// Shift 当作输入 `+` 的手段，而不是绑定里的修饰符。`allowed` 由调用方选择——
/// Kitty 路径用 `SYMBOL_KEYS`，modifyOtherKeys 路径用 `SHIFTED_SYMBOL_KEYS`。
fn get_logical_shifted_symbol_candidate(
    codepoint: Option<i32>,
    modifier: u32,
    allowed: &[char],
) -> Option<(i32, u32)> {
    let codepoint = codepoint?;
    if (modifier & MODIFIER_SHIFT) == 0 {
        return None;
    }
    let normalized_codepoint = normalize_kitty_functional_codepoint(codepoint);
    if !codepoint_is(allowed, normalized_codepoint) {
        return None;
    }
    Some((normalized_codepoint, modifier & !MODIFIER_SHIFT))
}

// =============================================================================
// Legacy 序列
// =============================================================================

// 老式终端的按键编码表。同一逻辑键可能有多种变体（CSI 与 SS3、不同终端的私有扩展），
// 所以数组保存全部变体，匹配时做包含判断。凡是标注【逐字】的表，拼写都必须与真实终端的
// 按键序列一致，否则行为会漂移。
const LEGACY_UP: [&str; 2] = ["\x1b[A", "\x1bOA"];
const LEGACY_DOWN: [&str; 2] = ["\x1b[B", "\x1bOB"];
const LEGACY_RIGHT: [&str; 2] = ["\x1b[C", "\x1bOC"];
const LEGACY_LEFT: [&str; 2] = ["\x1b[D", "\x1bOD"];
const LEGACY_HOME: [&str; 4] = ["\x1b[H", "\x1bOH", "\x1b[1~", "\x1b[7~"];
const LEGACY_END: [&str; 4] = ["\x1b[F", "\x1bOF", "\x1b[4~", "\x1b[8~"];
const LEGACY_INSERT: [&str; 1] = ["\x1b[2~"];
const LEGACY_DELETE: [&str; 1] = ["\x1b[3~"];
const LEGACY_PAGE_UP: [&str; 2] = ["\x1b[5~", "\x1b[[5~"];
const LEGACY_PAGE_DOWN: [&str; 2] = ["\x1b[6~", "\x1b[[6~"];
const LEGACY_CLEAR: [&str; 2] = ["\x1b[E", "\x1bOE"];
const LEGACY_F1: [&str; 3] = ["\x1bOP", "\x1b[11~", "\x1b[[A"];
const LEGACY_F2: [&str; 3] = ["\x1bOQ", "\x1b[12~", "\x1b[[B"];
const LEGACY_F3: [&str; 3] = ["\x1bOR", "\x1b[13~", "\x1b[[C"];
const LEGACY_F4: [&str; 3] = ["\x1bOS", "\x1b[14~", "\x1b[[D"];
const LEGACY_F5: [&str; 2] = ["\x1b[15~", "\x1b[[E"];
const LEGACY_F6: [&str; 1] = ["\x1b[17~"];
const LEGACY_F7: [&str; 1] = ["\x1b[18~"];
const LEGACY_F8: [&str; 1] = ["\x1b[19~"];
const LEGACY_F9: [&str; 1] = ["\x1b[20~"];
const LEGACY_F10: [&str; 1] = ["\x1b[21~"];
const LEGACY_F11: [&str; 1] = ["\x1b[23~"];
const LEGACY_F12: [&str; 1] = ["\x1b[24~"];

/// `LEGACY_SHIFT_SEQUENCES`。
const LEGACY_SHIFT_UP: [&str; 1] = ["\x1b[a"];
const LEGACY_SHIFT_DOWN: [&str; 1] = ["\x1b[b"];
const LEGACY_SHIFT_RIGHT: [&str; 1] = ["\x1b[c"];
const LEGACY_SHIFT_LEFT: [&str; 1] = ["\x1b[d"];
const LEGACY_SHIFT_CLEAR: [&str; 1] = ["\x1b[e"];
const LEGACY_SHIFT_INSERT: [&str; 1] = ["\x1b[2$"];
const LEGACY_SHIFT_DELETE: [&str; 1] = ["\x1b[3$"];
const LEGACY_SHIFT_PAGE_UP: [&str; 1] = ["\x1b[5$"];
const LEGACY_SHIFT_PAGE_DOWN: [&str; 1] = ["\x1b[6$"];
const LEGACY_SHIFT_HOME: [&str; 1] = ["\x1b[7$"];
const LEGACY_SHIFT_END: [&str; 1] = ["\x1b[8$"];

/// `LEGACY_CTRL_SEQUENCES`。
const LEGACY_CTRL_UP: [&str; 1] = ["\x1bOa"];
const LEGACY_CTRL_DOWN: [&str; 1] = ["\x1bOb"];
const LEGACY_CTRL_RIGHT: [&str; 1] = ["\x1bOc"];
const LEGACY_CTRL_LEFT: [&str; 1] = ["\x1bOd"];
const LEGACY_CTRL_CLEAR: [&str; 1] = ["\x1bOe"];
const LEGACY_CTRL_INSERT: [&str; 1] = ["\x1b[2^"];
const LEGACY_CTRL_DELETE: [&str; 1] = ["\x1b[3^"];
const LEGACY_CTRL_PAGE_UP: [&str; 1] = ["\x1b[5^"];
const LEGACY_CTRL_PAGE_DOWN: [&str; 1] = ["\x1b[6^"];
const LEGACY_CTRL_HOME: [&str; 1] = ["\x1b[7^"];
const LEGACY_CTRL_END: [&str; 1] = ["\x1b[8^"];

/// `LEGACY_SHIFT_SEQUENCES` 覆盖的键（TS `LegacyModifierKey`）。
///
/// 它只是下面两组“shift / ctrl 变体表”的索引：Alt 与无修饰的序列不走这两张表，所以枚举里
/// 没有 alt 变体。`clear` 虽在枚举中，但 `matches_key` 对它走单独分支。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LegacyModifierKey {
    Up,
    Down,
    Right,
    Left,
    Clear,
    Insert,
    Delete,
    PageUp,
    PageDown,
    Home,
    End,
}

impl LegacyModifierKey {
    fn shift_sequences(self) -> &'static [&'static str] {
        match self {
            LegacyModifierKey::Up => &LEGACY_SHIFT_UP,
            LegacyModifierKey::Down => &LEGACY_SHIFT_DOWN,
            LegacyModifierKey::Right => &LEGACY_SHIFT_RIGHT,
            LegacyModifierKey::Left => &LEGACY_SHIFT_LEFT,
            LegacyModifierKey::Clear => &LEGACY_SHIFT_CLEAR,
            LegacyModifierKey::Insert => &LEGACY_SHIFT_INSERT,
            LegacyModifierKey::Delete => &LEGACY_SHIFT_DELETE,
            LegacyModifierKey::PageUp => &LEGACY_SHIFT_PAGE_UP,
            LegacyModifierKey::PageDown => &LEGACY_SHIFT_PAGE_DOWN,
            LegacyModifierKey::Home => &LEGACY_SHIFT_HOME,
            LegacyModifierKey::End => &LEGACY_SHIFT_END,
        }
    }

    fn ctrl_sequences(self) -> &'static [&'static str] {
        match self {
            LegacyModifierKey::Up => &LEGACY_CTRL_UP,
            LegacyModifierKey::Down => &LEGACY_CTRL_DOWN,
            LegacyModifierKey::Right => &LEGACY_CTRL_RIGHT,
            LegacyModifierKey::Left => &LEGACY_CTRL_LEFT,
            LegacyModifierKey::Clear => &LEGACY_CTRL_CLEAR,
            LegacyModifierKey::Insert => &LEGACY_CTRL_INSERT,
            LegacyModifierKey::Delete => &LEGACY_CTRL_DELETE,
            LegacyModifierKey::PageUp => &LEGACY_CTRL_PAGE_UP,
            LegacyModifierKey::PageDown => &LEGACY_CTRL_PAGE_DOWN,
            LegacyModifierKey::Home => &LEGACY_CTRL_HOME,
            LegacyModifierKey::End => &LEGACY_CTRL_END,
        }
    }
}

/// `LEGACY_SEQUENCE_KEY_IDS`（**【逐字】**顺序即源码顺序）。
///
/// 只供 [`parse_key`] 的 legacy 识别使用；[`matches_key`] 走的是上面按键拆分的
/// `LEGACY_*` 数组，二者内容有重叠但用途不同。58 条序列互不重复，所以首次命中即唯一命中。
const LEGACY_SEQUENCE_KEY_IDS: [(&str, &str); 58] = [
    ("\x1bOA", "up"),
    ("\x1bOB", "down"),
    ("\x1bOC", "right"),
    ("\x1bOD", "left"),
    ("\x1bOH", "home"),
    ("\x1bOF", "end"),
    ("\x1b[E", "clear"),
    ("\x1bOE", "clear"),
    ("\x1bOe", "ctrl+clear"),
    ("\x1b[e", "shift+clear"),
    ("\x1b[2~", "insert"),
    ("\x1b[2$", "shift+insert"),
    ("\x1b[2^", "ctrl+insert"),
    ("\x1b[3$", "shift+delete"),
    ("\x1b[3^", "ctrl+delete"),
    ("\x1b[[5~", "pageUp"),
    ("\x1b[[6~", "pageDown"),
    ("\x1b[a", "shift+up"),
    ("\x1b[b", "shift+down"),
    ("\x1b[c", "shift+right"),
    ("\x1b[d", "shift+left"),
    ("\x1bOa", "ctrl+up"),
    ("\x1bOb", "ctrl+down"),
    ("\x1bOc", "ctrl+right"),
    ("\x1bOd", "ctrl+left"),
    ("\x1b[5$", "shift+pageUp"),
    ("\x1b[6$", "shift+pageDown"),
    ("\x1b[7$", "shift+home"),
    ("\x1b[8$", "shift+end"),
    ("\x1b[5^", "ctrl+pageUp"),
    ("\x1b[6^", "ctrl+pageDown"),
    ("\x1b[7^", "ctrl+home"),
    ("\x1b[8^", "ctrl+end"),
    ("\x1bOP", "f1"),
    ("\x1bOQ", "f2"),
    ("\x1bOR", "f3"),
    ("\x1bOS", "f4"),
    ("\x1b[11~", "f1"),
    ("\x1b[12~", "f2"),
    ("\x1b[13~", "f3"),
    ("\x1b[14~", "f4"),
    ("\x1b[[A", "f1"),
    ("\x1b[[B", "f2"),
    ("\x1b[[C", "f3"),
    ("\x1b[[D", "f4"),
    ("\x1b[[E", "f5"),
    ("\x1b[15~", "f5"),
    ("\x1b[17~", "f6"),
    ("\x1b[18~", "f7"),
    ("\x1b[19~", "f8"),
    ("\x1b[20~", "f9"),
    ("\x1b[21~", "f10"),
    ("\x1b[23~", "f11"),
    ("\x1b[24~", "f12"),
    ("\x1bb", "alt+left"),
    ("\x1bf", "alt+right"),
    ("\x1bp", "alt+up"),
    ("\x1bn", "alt+down"),
];

/// 在 `LEGACY_SEQUENCE_KEY_IDS` 里线性查找输入对应的键标识符。
fn legacy_sequence_key_id(data: &str) -> Option<&'static str> {
    LEGACY_SEQUENCE_KEY_IDS
        .iter()
        .find(|(sequence, _)| *sequence == data)
        .map(|(_, id)| *id)
}

fn matches_legacy_sequence(data: &str, sequences: &[&str]) -> bool {
    sequences.contains(&data)
}

/// 仅当修饰符**恰好**是 shift 或 ctrl 时，查 `key` 对应的变体表；其他值直接 false。
///
/// 这些是 rxvt 风格的 `\x1b[a` / `\x1bOa` 序列（见测试 `matches_rxvt_modifier_sequences`），
/// 与 CSI 数字形式（如 `\x1b[1;5D`）在不同分支处理。
fn matches_legacy_modifier_sequence(data: &str, key: LegacyModifierKey, modifier: u32) -> bool {
    if modifier == MODIFIER_SHIFT {
        return matches_legacy_sequence(data, key.shift_sequences());
    }
    if modifier == MODIFIER_CTRL {
        return matches_legacy_sequence(data, key.ctrl_sequences());
    }
    false
}

// =============================================================================
// Kitty 协议解析
// =============================================================================

/// Kitty keyboard protocol（flag 2）的事件类型：1 = press、2 = repeat、3 = release。
///
/// flag 2 让终端在 CSI-u 序列的 `:` 后附加事件类型；未协商该 flag 时序列里没有这一组，
/// 解析结果一律按 [`KeyEventType::Press`] 处理（见 `event_type_from_i32`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyEventType {
    /// 按下（协议值 1，也是缺省值）。
    Press,
    /// 按住不放产生的自动重复（协议值 2）。
    Repeat,
    /// 松开按键（协议值 3）。
    Release,
}

impl KeyEventType {
    /// TS 的字面量拼写。
    ///
    /// 三个值分别是 `"press"` / `"repeat"` / `"release"`。
    pub fn as_str(self) -> &'static str {
        match self {
            KeyEventType::Press => "press",
            KeyEventType::Repeat => "repeat",
            KeyEventType::Release => "release",
        }
    }
}

/// `parse_kitty_sequence` 的解析结果。
///
/// 码点沿用内部约定：方向键/编辑键是负数哨兵，可打印键与小键盘键仍是协议码点
/// （小键盘数字等还要经过 `normalize_kitty_functional_codepoint` 才与绑定一致）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ParsedKittySequence {
    pub codepoint: i32,
    /// Shift 按下时终端的移位码点。
    pub shifted_key: Option<i32>,
    /// 标准 PC-101 布局下的键（非拉丁布局时上报）。
    pub base_layout_key: Option<i32>,
    /// 修饰符位掩码，**已经减一**（Kitty 上报的是“掩码 + 1”）。可能仍含 Caps/Num Lock 位。
    pub modifier: u32,
    /// 事件类型；序列未携带时是 `Press`。
    pub event_type: KeyEventType,
}

/// xterm modifyOtherKeys 序列 `\x1b[27;<mod>;<codepoint>~` 的解析结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ParsedModifyOtherKeysSequence {
    /// 协议上报的键码点。
    pub codepoint: i32,
    /// 修饰符位掩码，**已经减一**。
    pub modifier: u32,
}

thread_local! {
    /// TS `_lastEventType` 的移植：`parse_kitty_sequence` 每次成功解析都会写入。
    ///
    /// 当前**没有读取方**：[`is_key_release`] / [`is_key_repeat`] 是直接嗅探输入字符串，
    /// 不查这里；保留该状态不影响任何现有行为。
    static LAST_EVENT_TYPE: Cell<KeyEventType> = const { Cell::new(KeyEventType::Press) };
}

/// 判断一条输入序列是否为 Kitty 的“释放”事件（`isKeyRelease`）。
///
/// 实现是**模式嗅探**而不是查询历史状态：只要 `data` 含有 `:3u`、`:3~`、`:3A`… 这类
/// “事件类型 = 3”的片段就返回 true。它不校验整条序列的结构，所以任何包含这些片段的
/// 输入都可能命中；括号粘贴内容被显式排除（见函数体）。
///
/// 只在 Kitty keyboard protocol 带 flag 2 时有意义。
pub fn is_key_release(data: &str) -> bool {
    // 不要把括号粘贴内容当成按键释放，即使其中含 `:3F` 之类的模式
    // （例如 `90:62:3F:A5` 这样的蓝牙 MAC 地址）。`terminal.ts` 会在把粘贴内容交给 TUI 前
    // 重新包上括号粘贴标记，所以粘贴数据一定含 `\x1b[200~`。
    if data.contains("\x1b[200~") {
        return false;
    }
    // 快速判断：带 flag 2 的释放事件含 `:3`。格式：`\x1b[<codepoint>;<modifier>:3u`。
    [":3u", ":3~", ":3A", ":3B", ":3C", ":3D", ":3H", ":3F"]
        .iter()
        .any(|needle| data.contains(needle))
}

/// 判断一条输入序列是否为 Kitty 的“重复”事件（`isKeyRepeat`）。
///
/// 与 [`is_key_release`] 一样是模式嗅探：匹配 `:2u`、`:2~`、`:2A`… 等“事件类型 = 2”的
/// 片段，不校验整条序列；括号粘贴内容被排除。
///
/// 只在 Kitty keyboard protocol 带 flag 2 时有意义。
pub fn is_key_repeat(data: &str) -> bool {
    // 不要把括号粘贴内容当成按键重复，即使其中含 `:2F` 之类的模式（见 [`is_key_release`]）。
    if data.contains("\x1b[200~") {
        return false;
    }
    [":2u", ":2~", ":2A", ":2B", ":2C", ":2D", ":2H", ":2F"]
        .iter()
        .any(|needle| data.contains(needle))
}

// TS 的 `parseEventType(str | undefined)` 在这里拆成两步：四个 Kitty 正则的匹配器已经把捕获组
// 校验成纯数字并 `parseInt`，随后交给 [`event_type_from_i32`]。因此不再单独保留字符串版本。

/// JS 的 `parseInt(text, 10)`：跳过前导空白、可选正负号、读到非数字即停。
///
/// 没有可解析数字时返回 `None`（JS 是 `NaN`；调用处都走 `??`/比较，语义一致）。
/// 溢出时返回 `i64::MAX`，**不区分符号**（负溢出同样返回正的最大值）——这是有意的近似，
/// 调用点只解析码点/修饰符/键号这类小整数。返回 i32 的调用处直接 `as i32` 截断，
/// 本函数自身不做范围校验。
pub(crate) fn parse_js_int(text: &str) -> Option<i64> {
    let trimmed = text.trim_start_matches(|ch: char| ch.is_whitespace());
    let bytes = trimmed.as_bytes();
    let mut index = 0usize;
    let mut sign = 1i64;
    if index < bytes.len() && (bytes[index] == b'+' || bytes[index] == b'-') {
        if bytes[index] == b'-' {
            sign = -1;
        }
        index += 1;
    }
    let start = index;
    let mut value: i64 = 0;
    let mut overflow = false;
    while index < bytes.len() && bytes[index].is_ascii_digit() {
        let digit = i64::from(bytes[index] - b'0');
        match value.checked_mul(10).and_then(|v| v.checked_add(digit)) {
            Some(next) => value = next,
            None => overflow = true,
        }
        index += 1;
    }
    if index == start {
        return None;
    }
    if overflow {
        // JS 会返回一个双精度近似值；这里的调用点（码点/修饰符/键号）都在 i32 范围内，
        // 溢出到 i64 之上的输入不可能来自真实终端序列。
        return Some(i64::MAX);
    }
    Some(sign * value)
}

/// 从 `data` 中按正则匹配 CSI-u 序列的五个捕获组。
///
/// 正则（**【逐字】**）：`^\x1b\[(\d+)(?::(\d*))?(?::(\d+))?(?:;(\d+))?(?::(\d+))?u$`
///
/// 返回 `(codepoint, shifted, base, mod, event)`，未匹配的组为 `None`（空字符串的 shifted
/// 组也视为 `None`，与 TS 的 `g2.length > 0` 判断一致）。
///
/// 手写解析的接受语法：`codepoint[:shifted[:base]][;mod[:event]]`，首尾固定为 `\x1b[` 与
/// `u`。任何数字组必须**全部**是 ASCII 数字，否则整条拒绝；`codepoint` 与 `;mod` 不允许
/// 为空，`shifted` 允许为空（表示终端未上报），`base` 不允许为空；多余的分隔符也拒绝。
/// 这些规则与上面的正则逐组对应，避免引入第二套语法。
type CsiUCaptures = (i32, Option<i32>, Option<i32>, Option<i32>, Option<i32>);

fn match_csi_u(data: &str) -> Option<CsiUCaptures> {
    let rest = data.strip_prefix("\x1b[")?.strip_suffix('u')?;
    // rest = codepoint[:shifted[:base]][;mod[:event]]
    let (key_part, event_part) = match rest.split_once(';') {
        Some((key, event)) => (key, Some(event)),
        None => (rest, None),
    };
    let mut key_groups = key_part.splitn(3, ':');
    let codepoint_text = key_groups.next()?;
    if codepoint_text.is_empty() || !codepoint_text.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let codepoint = parse_js_int(codepoint_text)? as i32;
    let shifted_text = key_groups.next();
    let base_text = key_groups.next();
    if key_groups.next().is_some() {
        return None;
    }
    let shifted_key = match shifted_text {
        Some(text) if !text.is_empty() && text.bytes().all(|b| b.is_ascii_digit()) => {
            Some(parse_js_int(text)? as i32)
        }
        Some("") => None,
        Some(_) => return None,
        None => None,
    };
    let base_layout_key = match base_text {
        Some(text) if !text.is_empty() && text.bytes().all(|b| b.is_ascii_digit()) => {
            Some(parse_js_int(text)? as i32)
        }
        Some(_) => return None,
        None => None,
    };
    let (modifier_text, event_text) = match event_part {
        None => (None, None),
        Some(text) => match text.split_once(':') {
            Some((modifier, event)) => (Some(modifier), Some(event)),
            None => (Some(text), None),
        },
    };
    let modifier = match modifier_text {
        Some(text) if !text.is_empty() && text.bytes().all(|b| b.is_ascii_digit()) => {
            Some(parse_js_int(text)? as i32)
        }
        Some(_) => return None,
        None => None,
    };
    let event = match event_text {
        Some(text) if !text.is_empty() && text.bytes().all(|b| b.is_ascii_digit()) => {
            Some(parse_js_int(text)? as i32)
        }
        Some(_) => return None,
        None => None,
    };
    Some((codepoint, shifted_key, base_layout_key, modifier, event))
}

/// `parseKittySequence(data)`：依次尝试 4 个正则（**顺序不可变**）。
///
/// 四种格式按结尾字符区分：
/// 1. CSI-u：`\x1b[<cp>[:<shifted>[:<base>]][;<mod>[:<event>]]u`；
/// 2. 带修饰方向键：`\x1b[1;<mod>[:<event>]A/B/C/D`；
/// 3. 功能键区：`\x1b[<num>[;<mod>][:<event>]~`，但只认 2/3/5/6/7/8；F 键不在这里解析，
///    留给 legacy 表；
/// 4. 带修饰 Home/End：`\x1b[1;<mod>[:<event>]H/F`。
///
/// 命中任一格式且键号可识别时才写 `LAST_EVENT_TYPE` 并返回；功能键区里未映射的键号
/// （例如 `\x1b[15~` 的 15）不返回，而是继续尝试后续 Home/End 匹配器，全部失败时返回
/// `None`。修饰符统一减一（协议的 1 表示无修饰），事件类型缺省为
/// [`KeyEventType::Press`]；返回 `None` 时调用方继续走 legacy 或 modifyOtherKeys 路径。
pub(crate) fn parse_kitty_sequence(data: &str) -> Option<ParsedKittySequence> {
    // CSI-u（flag 1/2/4）：`\x1b[<codepoint>[:<shifted>[:<base>]][;<mod>[:<event>]]u`
    if let Some((codepoint, shifted_key, base_layout_key, modifier, event)) = match_csi_u(data) {
        // 协议用 1 表示“无修饰”，缺省组也按 1 处理，因此减一后 0 才是无修饰。
        let mod_value = modifier.unwrap_or(1);
        let event_type = event_type_from_i32(event);
        LAST_EVENT_TYPE.with(|cell| cell.set(event_type));
        return Some(ParsedKittySequence {
            codepoint,
            shifted_key,
            base_layout_key,
            modifier: (mod_value - 1) as u32,
            event_type,
        });
    }

    // 带修饰的方向键：`\x1b[1;<mod>A/B/C/D` 或 `\x1b[1;<mod>:<event>A/B/C/D`
    if let Some((mod_value, event, letter)) = match_arrow(data) {
        let event_type = event_type_from_i32(event);
        let codepoint = match letter {
            'A' => ARROW_UP,
            'B' => ARROW_DOWN,
            'C' => ARROW_RIGHT,
            'D' => ARROW_LEFT,
            _ => unreachable!("regex guarantees A/B/C/D"),
        };
        LAST_EVENT_TYPE.with(|cell| cell.set(event_type));
        return Some(ParsedKittySequence {
            codepoint,
            shifted_key: None,
            base_layout_key: None,
            modifier: (mod_value - 1) as u32,
            event_type,
        });
    }

    // 功能键：`\x1b[<num>~` / `\x1b[<num>;<mod>~` / `\x1b[<num>;<mod>:<event>~`
    if let Some((key_num, mod_value, event)) = match_functional(data) {
        let event_type = event_type_from_i32(event);
        // 只映射 2/3/5/6/7/8。其他数字（例如 15 = F5）得到 None，本分支不返回，
        // 继续尝试 Home/End 匹配器；F 键最终由 legacy 表处理。
        let codepoint = match key_num {
            2 => Some(FUNCTIONAL_INSERT),
            3 => Some(FUNCTIONAL_DELETE),
            5 => Some(FUNCTIONAL_PAGE_UP),
            6 => Some(FUNCTIONAL_PAGE_DOWN),
            7 => Some(FUNCTIONAL_HOME),
            8 => Some(FUNCTIONAL_END),
            _ => None,
        };
        if let Some(codepoint) = codepoint {
            LAST_EVENT_TYPE.with(|cell| cell.set(event_type));
            return Some(ParsedKittySequence {
                codepoint,
                shifted_key: None,
                base_layout_key: None,
                modifier: (mod_value.unwrap_or(1) - 1) as u32,
                event_type,
            });
        }
    }

    // 带修饰的 Home/End：`\x1b[1;<mod>H/F` 或 `\x1b[1;<mod>:<event>H/F`
    if let Some((mod_value, event, letter)) = match_home_end(data) {
        let event_type = event_type_from_i32(event);
        let codepoint = if letter == 'H' {
            FUNCTIONAL_HOME
        } else {
            FUNCTIONAL_END
        };
        LAST_EVENT_TYPE.with(|cell| cell.set(event_type));
        return Some(ParsedKittySequence {
            codepoint,
            shifted_key: None,
            base_layout_key: None,
            modifier: (mod_value - 1) as u32,
            event_type,
        });
    }

    None
}

/// 把协议的事件数字映射为枚举：2 → [`KeyEventType::Repeat`]、3 →
/// [`KeyEventType::Release`]，其余（包括缺省 `None` 与未知值）都按 Press 处理。
fn event_type_from_i32(event: Option<i32>) -> KeyEventType {
    match event {
        Some(2) => KeyEventType::Repeat,
        Some(3) => KeyEventType::Release,
        _ => KeyEventType::Press,
    }
}

/// `^\x1b\[1;(\d+)(?::(\d+))?([ABCD])$` → `(mod, event, letter)`。
///
/// 只接受固定首参 `1` 的形式 `\x1b[1;<mod>...`；这里的 `1` 是 CSI 的首个参数，
/// 不是修饰符值。
fn match_arrow(data: &str) -> Option<(i32, Option<i32>, char)> {
    let (mod_value, event, letter) = match_trailing_letter_sequence(data, "ABCD")?;
    Some((mod_value, event, letter))
}

/// `^\x1b\[1;(\d+)(?::(\d+))?([HF])$`。
fn match_home_end(data: &str) -> Option<(i32, Option<i32>, char)> {
    match_trailing_letter_sequence(data, "HF")
}

/// `match_arrow` 与 `match_home_end` 的共用实现：解析
/// `\x1b[1;<mod>[:<event>]<letter>`，`letter` 必须在 `letters` 中。
///
/// 修饰符与事件都要求非空且全为 ASCII 数字；`letter` 取最后一个字符，因此中间部分就是
/// `<mod>` 或 `<mod>:<event>`。
fn match_trailing_letter_sequence(data: &str, letters: &str) -> Option<(i32, Option<i32>, char)> {
    let rest = data.strip_prefix("\x1b[1;")?;
    let mut chars = rest.chars();
    let letter = chars.next_back()?;
    if !letters.contains(letter) {
        return None;
    }
    let middle = chars.as_str();
    // middle = `<mod>` 或 `<mod>:<event>`
    let (mod_text, event_text) = match middle.split_once(':') {
        Some((modifier, event)) => (modifier, Some(event)),
        None => (middle, None),
    };
    if mod_text.is_empty() || !mod_text.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let mod_value = parse_js_int(mod_text)? as i32;
    let event = match event_text {
        Some(text) if !text.is_empty() && text.bytes().all(|b| b.is_ascii_digit()) => {
            Some(parse_js_int(text)? as i32)
        }
        Some(_) => return None,
        None => None,
    };
    Some((mod_value, event, letter))
}

/// `^\x1b\[(\d+)(?:;(\d+))?(?::(\d+))?~$` → `(keyNum, mod, event)`。
///
/// 正则的分组顺序是 `<num>[;<mod>][:<event>]`：`<event>` 可以**不带修饰符**直接跟在
/// `:` 后面（`\x1b[2:3~` = insert release，`keys.ts:634-653`），因此先切 `<event>`，
/// 再切 `<mod>`。
fn match_functional(data: &str) -> Option<(i32, Option<i32>, Option<i32>)> {
    let rest = data.strip_prefix("\x1b[")?.strip_suffix('~')?;
    let (key_and_mod, event_text) = match rest.split_once(':') {
        Some((key_and_mod, event)) => (key_and_mod, Some(event)),
        None => (rest, None),
    };
    let (key_text, mod_text) = match key_and_mod.split_once(';') {
        Some((key, modifier)) => (key, Some(modifier)),
        None => (key_and_mod, None),
    };
    if key_text.is_empty() || !key_text.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let key_num = parse_js_int(key_text)? as i32;
    let modifier = match mod_text {
        Some(text) if !text.is_empty() && text.bytes().all(|b| b.is_ascii_digit()) => {
            Some(parse_js_int(text)? as i32)
        }
        Some(_) => return None,
        None => None,
    };
    let event = match event_text {
        Some(text) if !text.is_empty() && text.bytes().all(|b| b.is_ascii_digit()) => {
            Some(parse_js_int(text)? as i32)
        }
        Some(_) => return None,
        None => None,
    };
    Some((key_num, modifier, event))
}

/// 判断 `data` 是否表示“`expected_codepoint` + `expected_modifier`”这次按键。
///
/// 依次尝试三种等价性：
/// 1. **物理匹配**：屏蔽锁定位后修饰符相等，且规范化后的码点相等（小键盘码点先折叠，
///    Shift 上报的大写拉丁字母折成小写）；
/// 2. **逻辑匹配**：输入带 Shift 且终端给了 shifted key，而 shifted key 是符号时，把 Shift
///    视为“产生这个符号的手段”而非修饰符（见 `get_logical_shifted_symbol_candidate`）；
/// 3. **base layout 回退**：仅在两者修饰符相等，且已识别的码点不是拉丁字母或已知符号时，
///    用终端上报的 PC-101 键位匹配非拉丁布局（见下方注释）。
///
/// 第 3 步没有把数字列为“权威码点”，这一点与 `format_parsed_key` 不同：数字键也会参与
/// base layout 回退。返回值只表达是否匹配，不区分是哪一步命中的。
fn matches_kitty_sequence(data: &str, expected_codepoint: i32, expected_modifier: u32) -> bool {
    let Some(parsed) = parse_kitty_sequence(data) else {
        return false;
    };
    let actual_mod = parsed.modifier & !LOCK_MASK;
    let expected_mod = expected_modifier & !LOCK_MASK;

    let normalized_codepoint = normalize_shifted_letter_identity_codepoint(
        normalize_kitty_functional_codepoint(parsed.codepoint),
        parsed.modifier,
    );
    let normalized_expected_codepoint = normalize_shifted_letter_identity_codepoint(
        normalize_kitty_functional_codepoint(expected_codepoint),
        expected_modifier,
    );

    // 物理匹配：主码点与所有上报的修饰符都一致。
    if actual_mod == expected_mod && normalized_codepoint == normalized_expected_codepoint {
        return true;
    }

    // 逻辑匹配：Kitty 把 `+` 这类符号上报为 Shift + 物理键 `=`。绑定写的是产出的符号，
    // 因此消费掉这个隐含的 Shift，同时保留 Ctrl、Alt、Super。物理写法 `shift+=` 仍由上面的
    // 分支匹配。
    if let Some((codepoint, modifier)) =
        get_logical_shifted_symbol_candidate(parsed.shifted_key, actual_mod, &SYMBOL_KEYS)
        && (modifier & !LOCK_MASK) == expected_mod
        && codepoint == normalize_kitty_functional_codepoint(expected_codepoint)
    {
        return true;
    }

    if actual_mod != expected_mod {
        return false;
    }

    // 备用匹配：非拉丁键盘布局时用 base layout key。
    // 这允许 Ctrl+С（西里尔）在终端上报标准 PC-101 布局键时匹配 Ctrl+c（拉丁）。
    //
    // 只有当码点**不是**已识别的拉丁字母（a-z）或符号（/, -, [, ; 等）时才回退到
    // base layout key：码点已识别时它是权威的，与物理键位无关。否则重映射布局（Dvorak、
    // Colemak、xremap 等）会误匹配（Ctrl+K 可能误中 Ctrl+V、Ctrl+/ 可能误中 Ctrl+[）。
    if parsed.base_layout_key == Some(expected_codepoint) {
        let cp = normalized_codepoint;
        let is_latin_letter = (97..=122).contains(&cp);
        let is_known_symbol = char_from_codepoint(cp).is_some_and(is_symbol_key);
        if !is_latin_letter && !is_known_symbol {
            return true;
        }
    }

    false
}

/// `^\x1b\[27;(\d+);(\d+)~$`（xterm modifyOtherKeys，CSI 27 ; modifiers ; keycode ~）。
///
/// 两个数字组都必须非空且全为 ASCII 数字；修饰符减一后存入结果（协议 1 = 无修饰）。
/// 边界：这里不校验 `mod >= 1`。若输入是 `mod=0`，`mod_value - 1` 会让 `u32` 下溢
/// （调试构建 panic，发布构建按溢出策略回绕）。真实终端是否可能发 0 不由本函数控制。
fn parse_modify_other_keys_sequence(data: &str) -> Option<ParsedModifyOtherKeysSequence> {
    let rest = data.strip_prefix("\x1b[27;")?.strip_suffix('~')?;
    let (mod_text, codepoint_text) = rest.split_once(';')?;
    if mod_text.is_empty()
        || !mod_text.bytes().all(|b| b.is_ascii_digit())
        || codepoint_text.is_empty()
        || !codepoint_text.bytes().all(|b| b.is_ascii_digit())
    {
        return None;
    }
    let mod_value = parse_js_int(mod_text)? as u32;
    let codepoint = parse_js_int(codepoint_text)? as i32;
    Some(ParsedModifyOtherKeysSequence {
        codepoint,
        modifier: mod_value - 1,
    })
}

/// 精确比较 modifyOtherKeys 的码点与修饰符。
///
/// 这里**不**屏蔽 `LOCK_MASK`，也**不**做 Shift 符号的逻辑匹配。Shift 符号逻辑匹配只在
/// 可打印字符的 `matches_printable_modify_other_keys` 中进行；与之不同，锁定位在**两个**
/// 函数里都没有被屏蔽——`matches_printable_modify_other_keys` 的物理比较同样直接比较
/// `parsed.modifier == expected_modifier`，其逻辑比较也只去掉 SHIFT 位。因此若输入序列的
/// 修饰符带有锁定位或隐含 Shift，本函数会因精确比较而不命中；这些位是否出现取决于终端。
fn matches_modify_other_keys(data: &str, expected_keycode: i32, expected_modifier: u32) -> bool {
    match parse_modify_other_keys_sequence(data) {
        Some(parsed) => {
            parsed.codepoint == expected_keycode && parsed.modifier == expected_modifier
        }
        None => false,
    }
}

/// 是否是 Windows Terminal 会话：设置了 `WT_SESSION`，且 `SSH_CONNECTION`、`SSH_CLIENT`、
/// `SSH_TTY` 都未设置。
///
/// 这是判断原始 0x08 语义的启发式。每次调用都重新读取环境变量，不做缓存。
fn is_windows_terminal_session() -> bool {
    env_is_set("WT_SESSION")
        && !env_is_set("SSH_CONNECTION")
        && !env_is_set("SSH_CLIENT")
        && !env_is_set("SSH_TTY")
}

/// `Boolean(process.env[name])`：空字符串为 false。
pub(crate) fn env_is_set(name: &str) -> bool {
    std::env::var_os(name).is_some_and(|value| !value.is_empty())
}

/// 原始 0x08（BS）在 legacy 终端里有歧义。
///
/// - Windows Terminal 用它表示 Ctrl+Backspace；
/// - 某些 legacy 终端与 tmux 用它表示普通 Backspace。
///
/// 只要能拿到显式的 Kitty / CSI-u / modifyOtherKeys 序列就优先用它们；只有原始 BS 字节
/// 才走 Windows Terminal 的启发式。
///
/// 具体规则：`0x7f` 永远只按“无修饰的 Backspace”匹配；`0x08` 在 Windows Terminal 会话
/// 按 Ctrl+Backspace，其他环境按无修饰 Backspace。其他修饰符组合一律不匹配。
fn matches_raw_backspace(data: &str, expected_modifier: u32) -> bool {
    if data == "\x7f" {
        return expected_modifier == 0;
    }
    if data != "\x08" {
        return false;
    }
    if is_windows_terminal_session() {
        expected_modifier == MODIFIER_CTRL
    } else {
        expected_modifier == 0
    }
}

// =============================================================================
// 通用键匹配
// =============================================================================

/// 取键对应的控制字符。
///
/// 使用通用公式 `code & 0x1f`（屏蔽到低 5 位）。适用于字母 a-z（1-26）、
/// 符号 `[\]_`（27、28、29、31）；`-` 与 `_` 是同一物理键，因此映射到 31。
///
/// 其他字符（数字、`/`、`;` 等）返回 `None`：本实现没有为它们的 Ctrl 组合定义 legacy
/// 控制字节，只能靠 CSI-u / modifyOtherKeys 识别。
fn raw_ctrl_char(key: char) -> Option<char> {
    let lower = key.to_ascii_lowercase();
    let code = lower as u32;
    if (97..=122).contains(&code) || lower == '[' || lower == '\\' || lower == ']' || lower == '_' {
        return char::from_u32(code & 0x1f);
    }
    if lower == '-' {
        return char::from_u32(31);
    }
    None
}

fn is_digit_key(key: char) -> bool {
    key.is_ascii_digit()
}

/// 比较 modifyOtherKeys 序列与“可打印字符 + 修饰符”，支持 Shift 符号的逻辑匹配。
///
/// 先做物理比较：修饰符完全相等，且两侧都把“Shift 下的大写拉丁字母”折成小写后码点相同。
/// 再做逻辑比较：若输入带 Shift 且码点本身属于 `SHIFTED_SYMBOL_KEYS`（说明它就是 Shift
/// 产生的符号），消费 Shift 后与期望值比较。例如 `\x1b[27;2;43~`（Shift 上报 `+`）能匹配
/// 绑定 `+`（期望修饰符 0）；而 `\x1b[27;6;43~` 的 modifier 是 5（shift+ctrl），物理比较
/// 无法匹配期望修饰符 4 的 `ctrl++`，是由逻辑比较消费 Shift 后命中的。
/// 测试见 `matches_xterm_modify_other_keys_symbol_and_digit_combos`。
fn matches_printable_modify_other_keys(
    data: &str,
    expected_keycode: i32,
    expected_modifier: u32,
) -> bool {
    let Some(parsed) = parse_modify_other_keys_sequence(data) else {
        return false;
    };
    let physical_match = parsed.modifier == expected_modifier
        && normalize_shifted_letter_identity_codepoint(parsed.codepoint, parsed.modifier)
            == normalize_shifted_letter_identity_codepoint(expected_keycode, expected_modifier);
    if physical_match {
        return true;
    }
    match get_logical_shifted_symbol_candidate(
        Some(parsed.codepoint),
        parsed.modifier,
        &SHIFTED_SYMBOL_KEYS,
    ) {
        Some((codepoint, modifier)) => {
            modifier == expected_modifier && codepoint == expected_keycode
        }
        None => false,
    }
}

/// 把键名与修饰符位格式化成规范键标识符；遇到不支持的修饰位返回 `None`。
///
/// 输出顺序固定为 shift、ctrl、alt、super（`format_parsed_key` 依赖它保证
/// [`parse_key`] 的确定性输出）。锁定位先被屏蔽；hyper/meta 等其余位（本实现没有对应
/// 语义）会让整个标识符无法表示，因此返回 `None` 而不是静默丢弃。
fn format_key_name_with_modifiers(key_name: &str, modifier: u32) -> Option<String> {
    let mut mods: Vec<&'static str> = Vec::new();
    let effective_mod = modifier & !LOCK_MASK;
    let supported_modifier_mask = MODIFIER_SHIFT | MODIFIER_CTRL | MODIFIER_ALT | MODIFIER_SUPER;
    if (effective_mod & !supported_modifier_mask) != 0 {
        return None;
    }
    if effective_mod & MODIFIER_SHIFT != 0 {
        mods.push("shift");
    }
    if effective_mod & MODIFIER_CTRL != 0 {
        mods.push("ctrl");
    }
    if effective_mod & MODIFIER_ALT != 0 {
        mods.push("alt");
    }
    if effective_mod & MODIFIER_SUPER != 0 {
        mods.push("super");
    }
    if mods.is_empty() {
        Some(key_name.to_string())
    } else {
        Some(format!("{}+{}", mods.join("+"), key_name))
    }
}

/// 键标识符拆解结果：基键（字符或哨兵）加四个布尔修饰符。
///
/// 用布尔而不是位掩码，是因为解析阶段要检测“同一修饰符重复出现”；掩码会丢掉这个信息。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ParsedKeyId {
    key: char,
    ctrl: bool,
    shift: bool,
    alt: bool,
    super_modifier: bool,
}

impl ParsedKeyId {
    /// 合并成与 Kitty 协议一致的修饰符位掩码。
    fn modifier(self) -> u32 {
        let mut modifier = 0u32;
        if self.shift {
            modifier |= MODIFIER_SHIFT;
        }
        if self.alt {
            modifier |= MODIFIER_ALT;
        }
        if self.ctrl {
            modifier |= MODIFIER_CTRL;
        }
        if self.super_modifier {
            modifier |= MODIFIER_SUPER;
        }
        modifier
    }
}

/// TS 版本用 `key.toLowerCase()` 后逐个剥离修饰符前缀；`key` 可能是多字符的特殊键名，
/// 也可能是单个字符（字母/数字/符号）。Rust 侧把「单个字符」与「特殊键名」分开：
/// 特殊键名经 [`single_char`] 映射到哨兵字符（TS 侧由类型系统保证只出现合法名字）。
///
/// 解析规则：
/// - 整个标识符先转小写，所以 `Ctrl+Shift+P` 与 `ctrl+shift+p` 等价；
/// - 每轮只剥离一个 `shift+` / `alt+` / `ctrl+` / `super+` 前缀，顺序任意；
/// - 同一修饰符出现两次（`ctrl+ctrl+c`）视为非法，返回 `None`；
/// - 剩余部分必须非空：要么是单个 ASCII 字母/数字/符号（转小写后判断），要么是特殊键名；
///   含 `+` 的多字符剩余部分非法，但单独一个 `+` 合法（对应 `ctrl++`）。
///
/// 返回 `None` 表示这个标识符永远无法匹配任何输入；[`matches_key`] 直接把它当 false。
fn parse_key_id(key_id: &str) -> Option<ParsedKeyId> {
    // TS 的 `toLowerCase()` 对 `pageUp` 得到 `pageup`，对 `PageUp` 亦然。
    let mut key: String = key_id.to_lowercase();
    let mut ctrl = false;
    let mut shift = false;
    let mut alt = false;
    let mut super_modifier = false;
    let mut seen: Vec<&str> = Vec::new();
    loop {
        let mut matched = None;
        for name in MODIFIER_NAMES {
            if let Some(rest) = key.strip_prefix(name)
                && let Some(rest) = rest.strip_prefix('+')
            {
                matched = Some((name, rest.to_string()));
                break;
            }
        }
        let Some((name, rest)) = matched else {
            break;
        };
        if seen.contains(&name) {
            return None;
        }
        seen.push(name);
        match name {
            "ctrl" => ctrl = true,
            "shift" => shift = true,
            "alt" => alt = true,
            "super" => super_modifier = true,
            _ => unreachable!("MODIFIER_NAMES only has those four"),
        }
        key = rest;
    }
    if key.is_empty() {
        return None;
    }
    if key.contains('+') && key != "+" {
        return None;
    }

    // 单字符键（TS 的 Letter/Digit/SymbolKey）或特殊键名。
    let key_char = single_char(&key)?;
    if !is_valid_single_character_key(key_char) && special_key_codepoint(key_char).is_none() {
        return None;
    }
    Some(ParsedKeyId {
        key: key_char,
        ctrl,
        shift,
        alt,
        super_modifier,
    })
}

/// 把标识符的键名部分还原成 `char`：单字符直接取；特殊键名映射到内部的哨兵字符。
///
/// 哨兵字符使用 Unicode 私用区，绝不与真实按键字符冲突。
///
/// 这里的键名必须是**小写**（`parse_key_id` 已统一转小写），所以表里写的是 `"pageup"`
/// 而不是 `"pageUp"`。单字符输入会原样返回，是否合法由调用方再用
/// `is_valid_single_character_key` 判断。
fn single_char(key: &str) -> Option<char> {
    let mut chars = key.chars();
    let first = chars.next()?;
    if chars.next().is_none() {
        return Some(first);
    }
    // 特殊键名 → 哨兵字符。
    Some(match key {
        "escape" | "esc" => SENTINEL_ESCAPE,
        "enter" | "return" => SENTINEL_ENTER,
        "tab" => SENTINEL_TAB,
        "space" => SENTINEL_SPACE,
        "backspace" => SENTINEL_BACKSPACE,
        "delete" => SENTINEL_DELETE,
        "insert" => SENTINEL_INSERT,
        "clear" => SENTINEL_CLEAR,
        "home" => SENTINEL_HOME,
        "end" => SENTINEL_END,
        "pageup" => SENTINEL_PAGE_UP,
        "pagedown" => SENTINEL_PAGE_DOWN,
        "up" => SENTINEL_UP,
        "down" => SENTINEL_DOWN,
        "left" => SENTINEL_LEFT,
        "right" => SENTINEL_RIGHT,
        "f1" => SENTINEL_F1,
        "f2" => SENTINEL_F2,
        "f3" => SENTINEL_F3,
        "f4" => SENTINEL_F4,
        "f5" => SENTINEL_F5,
        "f6" => SENTINEL_F6,
        "f7" => SENTINEL_F7,
        "f8" => SENTINEL_F8,
        "f9" => SENTINEL_F9,
        "f10" => SENTINEL_F10,
        "f11" => SENTINEL_F11,
        "f12" => SENTINEL_F12,
        _ => return None,
    })
}

// 哨兵字符：每个特殊键一个，位于 Unicode 私用区（U+10FF00 起），按上表顺序编号。
// 它们只在键标识符解析内部流转，不会出现在终端输入里，也不应作为公共 API 的字符返回。
const SENTINEL_ESCAPE: char = '\u{10FF00}';
const SENTINEL_ENTER: char = '\u{10FF01}';
const SENTINEL_TAB: char = '\u{10FF02}';
const SENTINEL_SPACE: char = '\u{10FF03}';
const SENTINEL_BACKSPACE: char = '\u{10FF04}';
const SENTINEL_DELETE: char = '\u{10FF05}';
const SENTINEL_INSERT: char = '\u{10FF06}';
const SENTINEL_CLEAR: char = '\u{10FF07}';
const SENTINEL_HOME: char = '\u{10FF08}';
const SENTINEL_END: char = '\u{10FF09}';
const SENTINEL_PAGE_UP: char = '\u{10FF0A}';
const SENTINEL_PAGE_DOWN: char = '\u{10FF0B}';
const SENTINEL_UP: char = '\u{10FF0C}';
const SENTINEL_DOWN: char = '\u{10FF0D}';
const SENTINEL_LEFT: char = '\u{10FF0E}';
const SENTINEL_RIGHT: char = '\u{10FF0F}';
const SENTINEL_F1: char = '\u{10FF10}';
const SENTINEL_F2: char = '\u{10FF11}';
const SENTINEL_F3: char = '\u{10FF12}';
const SENTINEL_F4: char = '\u{10FF13}';
const SENTINEL_F5: char = '\u{10FF14}';
const SENTINEL_F6: char = '\u{10FF15}';
const SENTINEL_F7: char = '\u{10FF16}';
const SENTINEL_F8: char = '\u{10FF17}';
const SENTINEL_F9: char = '\u{10FF18}';
const SENTINEL_F10: char = '\u{10FF19}';
const SENTINEL_F11: char = '\u{10FF1A}';
const SENTINEL_F12: char = '\u{10FF1B}';

/// 是否是特殊的哨兵键（`escape`/`enter`/方向键/功能键等）。
///
/// 只用于 `parse_key_id` 的合法性判断（`is_none()`）。功能键返回 `Some(0)` 只是占位：
/// 真实的匹配序列由 `function_key_sequences` 提供，码点值不参与比较。
fn special_key_codepoint(key: char) -> Option<i32> {
    if is_escape_key(key) {
        return Some(CODEPOINT_ESCAPE);
    }
    if is_enter_key(key) {
        return Some(CODEPOINT_ENTER);
    }
    if key == SENTINEL_TAB {
        return Some(CODEPOINT_TAB);
    }
    if key == SENTINEL_SPACE {
        return Some(CODEPOINT_SPACE);
    }
    if key == SENTINEL_BACKSPACE {
        return Some(CODEPOINT_BACKSPACE);
    }
    if function_key_sequences(key).is_some() {
        return Some(0);
    }
    sentinel_to_pair(key).map(|(codepoint, _)| codepoint)
}

/// TS `BaseKey` 中单字符的合法集合：字母 a-z、数字 0-9、31 个符号。
fn is_valid_single_character_key(ch: char) -> bool {
    ch.is_ascii_lowercase() || ch.is_ascii_digit() || is_symbol_key(ch)
}

fn is_escape_key(key: char) -> bool {
    key == SENTINEL_ESCAPE
}

fn is_enter_key(key: char) -> bool {
    key == SENTINEL_ENTER
}

/// 哨兵字符 →（逻辑码点, legacy 变体表索引）。
///
/// 只覆盖方向键、编辑键与 `clear`；escape/enter/tab/space/backspace 与功能键在
/// `matches_key` 里各有专门分支。`clear` 的码点是 0，仅作占位，实际匹配只用
/// `LegacyModifierKey::Clear` 查表。
fn sentinel_to_pair(key: char) -> Option<(i32, Option<LegacyModifierKey>)> {
    Some(match key {
        SENTINEL_INSERT => (FUNCTIONAL_INSERT, Some(LegacyModifierKey::Insert)),
        SENTINEL_DELETE => (FUNCTIONAL_DELETE, Some(LegacyModifierKey::Delete)),
        SENTINEL_HOME => (FUNCTIONAL_HOME, Some(LegacyModifierKey::Home)),
        SENTINEL_END => (FUNCTIONAL_END, Some(LegacyModifierKey::End)),
        SENTINEL_PAGE_UP => (FUNCTIONAL_PAGE_UP, Some(LegacyModifierKey::PageUp)),
        SENTINEL_PAGE_DOWN => (FUNCTIONAL_PAGE_DOWN, Some(LegacyModifierKey::PageDown)),
        SENTINEL_UP => (ARROW_UP, Some(LegacyModifierKey::Up)),
        SENTINEL_DOWN => (ARROW_DOWN, Some(LegacyModifierKey::Down)),
        SENTINEL_LEFT => (ARROW_LEFT, Some(LegacyModifierKey::Left)),
        SENTINEL_RIGHT => (ARROW_RIGHT, Some(LegacyModifierKey::Right)),
        SENTINEL_CLEAR => (0, Some(LegacyModifierKey::Clear)),
        _ => return None,
    })
}

/// 匹配输入数据与键标识符（`matchesKey`）。
///
/// 支持的键标识符：`escape`、`tab`、`enter`、`backspace`、`delete`、`home`、`end`、`space`、
/// 方向键、`ctrl+c` 等修饰组合（修饰符顺序任意、可组合），以及单字母/数字/符号。
///
/// # 参数与返回
///
/// - `data`：一条**完整**的按键序列（见模块文档“输入粒度”）；空串或不完整序列返回 false。
/// - `key_id`：键标识符；大小写不敏感，修饰符顺序任意。
/// - 返回 `bool`；函数没有错误类型，“标识符无法解析”等同于“不匹配”。
///
/// # 失败模式
///
/// false 同时表示“这个键没被按下”和“这条编码本实现不支持”（例如带修饰的 F 键、
/// `ctrl+escape`）。若需要区分并展示按键本身，应由调用方改用 [`parse_key`]。
pub fn matches_key(data: &str, key_id: &str) -> bool {
    let Some(parsed) = parse_key_id(key_id) else {
        return false;
    };
    let key = parsed.key;
    let modifier = parsed.modifier();
    // 单次调用内只取一次快照，后续分支都基于同一个值判断 legacy 的歧义语义。
    let kitty_active = is_kitty_protocol_active();

    // Escape 只支持无修饰；ctrl+escape 等组合没有可匹配的编码，直接返回 false。
    if is_escape_key(key) {
        if modifier != 0 {
            return false;
        }
        return data == "\x1b"
            || matches_kitty_sequence(data, CODEPOINT_ESCAPE, 0)
            || matches_modify_other_keys(data, CODEPOINT_ESCAPE, 0);
    }

    // 未激活 Kitty 时，先接受两种 legacy 写法：ctrl+space = NUL，alt+space = ESC + 空格。
    if key == SENTINEL_SPACE {
        if !kitty_active {
            if modifier == MODIFIER_CTRL && data == "\x00" {
                return true;
            }
            if modifier == MODIFIER_ALT && data == "\x1b " {
                return true;
            }
        }
        if modifier == 0 {
            return data == " "
                || matches_kitty_sequence(data, CODEPOINT_SPACE, 0)
                || matches_modify_other_keys(data, CODEPOINT_SPACE, 0);
        }
        return matches_kitty_sequence(data, CODEPOINT_SPACE, modifier)
            || matches_modify_other_keys(data, CODEPOINT_SPACE, modifier);
    }

    // Shift+Tab 的 legacy 序列是 CSI Z。无修饰的 tab 这里只认 "\t" 与 Kitty 序列，
    // 不再尝试 modifyOtherKeys（现状如此，改动会扩大可匹配的输入）。
    if key == SENTINEL_TAB {
        if modifier == MODIFIER_SHIFT {
            return data == "\x1b[Z"
                || matches_kitty_sequence(data, CODEPOINT_TAB, MODIFIER_SHIFT)
                || matches_modify_other_keys(data, CODEPOINT_TAB, MODIFIER_SHIFT);
        }
        if modifier == 0 {
            return data == "\t" || matches_kitty_sequence(data, CODEPOINT_TAB, 0);
        }
        return matches_kitty_sequence(data, CODEPOINT_TAB, modifier)
            || matches_modify_other_keys(data, CODEPOINT_TAB, modifier);
    }

    // Enter 的来源很多：CSI-u 的 Enter/KP_Enter、modifyOtherKeys，以及含义随 Kitty 状态
    // 变化的 \x1b\r 与 \n（见各分支注释）。
    if is_enter_key(key) {
        if modifier == MODIFIER_SHIFT {
            // CSI u 序列（标准 Kitty 协议）
            if matches_kitty_sequence(data, CODEPOINT_ENTER, MODIFIER_SHIFT)
                || matches_kitty_sequence(data, CODEPOINT_KP_ENTER, MODIFIER_SHIFT)
            {
                return true;
            }
            // xterm modifyOtherKeys（未启用 Kitty 协议时的回退）
            if matches_modify_other_keys(data, CODEPOINT_ENTER, MODIFIER_SHIFT) {
                return true;
            }
            // Kitty 协议激活时，legacy 序列是终端自定义映射：
            // \x1b\r = Kitty 的 "map shift+enter send_text all \e\r"
            // \n = Ghostty 的 "keybind = shift+enter=text:\n"
            if kitty_active {
                return data == "\x1b\r" || data == "\n";
            }
            return false;
        }
        if modifier == MODIFIER_ALT {
            if matches_kitty_sequence(data, CODEPOINT_ENTER, MODIFIER_ALT)
                || matches_kitty_sequence(data, CODEPOINT_KP_ENTER, MODIFIER_ALT)
            {
                return true;
            }
            if matches_modify_other_keys(data, CODEPOINT_ENTER, MODIFIER_ALT) {
                return true;
            }
            // \x1b\r 只在 legacy 模式（未启用 Kitty）下是 alt+enter
            if !kitty_active {
                return data == "\x1b\r";
            }
            return false;
        }
        if modifier == 0 {
            return data == "\r"
                || (!kitty_active && data == "\n")
                || data == "\x1bOM" // SS3 M（某些终端的小键盘 enter）
                || matches_kitty_sequence(data, CODEPOINT_ENTER, 0)
                || matches_kitty_sequence(data, CODEPOINT_KP_ENTER, 0);
        }
        return matches_kitty_sequence(data, CODEPOINT_ENTER, modifier)
            || matches_kitty_sequence(data, CODEPOINT_KP_ENTER, modifier)
            || matches_modify_other_keys(data, CODEPOINT_ENTER, modifier);
    }

    if key == SENTINEL_BACKSPACE {
        // Alt+Backspace 的 legacy 形式：ESC 后跟 0x7f 或 0x08。
        if modifier == MODIFIER_ALT {
            if data == "\x1b\x7f" || data == "\x1b\u{8}" {
                return true;
            }
            return matches_kitty_sequence(data, CODEPOINT_BACKSPACE, MODIFIER_ALT)
                || matches_modify_other_keys(data, CODEPOINT_BACKSPACE, MODIFIER_ALT);
        }
        if modifier == MODIFIER_CTRL {
            // 原始 0x08 有歧义：Windows Terminal 上是 Ctrl+Backspace，其它终端上是普通
            // Backspace，同时还与 Ctrl+H 重叠。
            if matches_raw_backspace(data, MODIFIER_CTRL) {
                return true;
            }
            return matches_kitty_sequence(data, CODEPOINT_BACKSPACE, MODIFIER_CTRL)
                || matches_modify_other_keys(data, CODEPOINT_BACKSPACE, MODIFIER_CTRL);
        }
        if modifier == 0 {
            return matches_raw_backspace(data, 0)
                || matches_kitty_sequence(data, CODEPOINT_BACKSPACE, 0)
                || matches_modify_other_keys(data, CODEPOINT_BACKSPACE, 0);
        }
        return matches_kitty_sequence(data, CODEPOINT_BACKSPACE, modifier)
            || matches_modify_other_keys(data, CODEPOINT_BACKSPACE, modifier);
    }

    if let Some((codepoint, legacy_key)) = sentinel_to_pair(key) {
        // insert / delete / home / end / pageUp / pageDown / 方向键 / clear 共用同一分支结构。
        // 其中 clear 单独处理；其余按键先试带修饰的 legacy 变体，再试无修饰的 legacy 序列，
        // 最后用 Kitty 序列兜底；左右方向键在 alt/ctrl 下另有额外的 CSI 写法（见下方分支）。
        // 无修饰路径不尝试 modifyOtherKeys（只有 legacy 与 Kitty 两种来源）。
        if key == SENTINEL_CLEAR {
            // 本实现没有为 clear 提供 Kitty 编码：无修饰查 LEGACY_CLEAR，有修饰只查
            // shift/ctrl 变体表，其他修饰组合一律不匹配。
            if modifier == 0 {
                return matches_legacy_sequence(data, &LEGACY_CLEAR);
            }
            return matches_legacy_modifier_sequence(
                data,
                legacy_key.expect("clear has a legacy key"),
                modifier,
            );
        }
        if let Some(legacy_key) = legacy_key
            && modifier != 0
            && matches_legacy_modifier_sequence(data, legacy_key, modifier)
        {
            return true;
        }
        if modifier == 0 {
            // 每个键的无修饰 legacy 变体表；CLEAR 已在上面返回，其余哨兵不会被 default 命中。
            let legacy = match key {
                SENTINEL_INSERT => &LEGACY_INSERT[..],
                SENTINEL_DELETE => &LEGACY_DELETE[..],
                SENTINEL_HOME => &LEGACY_HOME[..],
                SENTINEL_END => &LEGACY_END[..],
                SENTINEL_PAGE_UP => &LEGACY_PAGE_UP[..],
                SENTINEL_PAGE_DOWN => &LEGACY_PAGE_DOWN[..],
                SENTINEL_UP => &LEGACY_UP[..],
                SENTINEL_DOWN => &LEGACY_DOWN[..],
                SENTINEL_LEFT => &LEGACY_LEFT[..],
                SENTINEL_RIGHT => &LEGACY_RIGHT[..],
                _ => &[][..],
            };
            return matches_legacy_sequence(data, legacy)
                || matches_kitty_sequence(data, codepoint, 0);
        }
        // alt+方向键的额外 legacy 写法。
        if modifier == MODIFIER_ALT && key == SENTINEL_UP && data == "\x1bp" {
            return true;
        }
        if modifier == MODIFIER_ALT && key == SENTINEL_DOWN && data == "\x1bn" {
            return true;
        }
        if modifier == MODIFIER_ALT && key == SENTINEL_LEFT {
            return data == "\x1b[1;3D"
                || (!kitty_active && data == "\x1bB")
                || data == "\x1bb"
                || matches_kitty_sequence(data, codepoint, MODIFIER_ALT);
        }
        if modifier == MODIFIER_ALT && key == SENTINEL_RIGHT {
            return data == "\x1b[1;3C"
                || (!kitty_active && data == "\x1bF")
                || data == "\x1bf"
                || matches_kitty_sequence(data, codepoint, MODIFIER_ALT);
        }
        if modifier == MODIFIER_CTRL && key == SENTINEL_LEFT {
            return data == "\x1b[1;5D"
                || matches_legacy_modifier_sequence(data, LegacyModifierKey::Left, MODIFIER_CTRL)
                || matches_kitty_sequence(data, codepoint, MODIFIER_CTRL);
        }
        if modifier == MODIFIER_CTRL && key == SENTINEL_RIGHT {
            return data == "\x1b[1;5C"
                || matches_legacy_modifier_sequence(data, LegacyModifierKey::Right, MODIFIER_CTRL)
                || matches_kitty_sequence(data, codepoint, MODIFIER_CTRL);
        }
        return matches_kitty_sequence(data, codepoint, modifier);
    }

    if let Some(function_key) = function_key_sequences(key) {
        // 功能键只查 legacy 表（表中也包含 `\x1b[15~` 这类 `~` 形式）；没有按 Kitty
        // 修饰符解析的路径，因此任何修饰组合（shift+f1、ctrl+f5…）都不匹配。
        if modifier != 0 {
            return false;
        }
        return matches_legacy_sequence(data, function_key);
    }

    // 单字母/数字/符号键。
    // - alt+字符、ctrl+alt+字符 的 ESC 前缀 legacy 形式只在 Kitty 未激活时尝试；
    // - ctrl+字符 的控制字节、以及无修饰字符本身不依赖协议状态，始终直接比较；
    // - 其余组合回退到 Kitty CSI-u 与 modifyOtherKeys。
    if is_valid_single_character_key(key) {
        let codepoint = key as i32;
        let raw_ctrl = raw_ctrl_char(key);
        let is_letter = key.is_ascii_lowercase();
        let is_digit = is_digit_key(key);

        if modifier == MODIFIER_CTRL + MODIFIER_ALT
            && !kitty_active
            && let Some(raw_ctrl) = raw_ctrl
            && data == format!("\x1b{raw_ctrl}")
        {
            // Legacy：ctrl+alt+key 是 ESC 后跟控制字符。若该 legacy 形式不匹配，
            // 继续尝试，以便识别 tmux 的 CSI-u 与 modifyOtherKeys 序列。
            return true;
        }

        if modifier == MODIFIER_ALT
            && !kitty_active
            && (is_letter || is_digit || is_symbol_key(key))
            && data == format!("\x1b{key}")
        {
            return true;
        }

        // Ctrl：先试 legacy 控制字符，再试 Kitty 与 modifyOtherKeys 的 printable 版本。
        if modifier == MODIFIER_CTRL {
            if let Some(raw_ctrl) = raw_ctrl
                && data == raw_ctrl.to_string()
            {
                return true;
            }
            return matches_kitty_sequence(data, codepoint, MODIFIER_CTRL)
                || matches_printable_modify_other_keys(data, codepoint, MODIFIER_CTRL);
        }

        // Ctrl+Shift 没有可用的 legacy 控制字节，只能由协议序列匹配。
        if modifier == MODIFIER_SHIFT + MODIFIER_CTRL {
            return matches_kitty_sequence(data, codepoint, MODIFIER_SHIFT + MODIFIER_CTRL)
                || matches_printable_modify_other_keys(
                    data,
                    codepoint,
                    MODIFIER_SHIFT + MODIFIER_CTRL,
                );
        }

        if modifier == MODIFIER_SHIFT {
            // Legacy：shift+字母 产生大写。
            if is_letter && data == key.to_ascii_uppercase().to_string() {
                return true;
            }
            return matches_kitty_sequence(data, codepoint, MODIFIER_SHIFT)
                || matches_printable_modify_other_keys(data, codepoint, MODIFIER_SHIFT);
        }

        // 其余修饰组合到这里只走 Kitty / modifyOtherKeys 协议序列；alt+字符 的 ESC 前缀
        // legacy 形式已在上方按 Kitty 状态尝试过。
        if modifier != 0 {
            return matches_kitty_sequence(data, codepoint, modifier)
                || matches_printable_modify_other_keys(data, codepoint, modifier);
        }

        // 同时检查原始字符与 Kitty 序列（release 事件需要）。
        return data == key.to_string()
            || matches_kitty_sequence(data, codepoint, 0)
            || matches_printable_modify_other_keys(data, codepoint, 0);
    }

    false
}

/// 功能键哨兵 → 对应的 legacy 变体表；非功能键返回 `None`。
///
/// 这些表用于 `matches_key` 的功能键分支与 `special_key_codepoint` 的合法性判断；
/// `parse_key` 使用独立的 `LEGACY_SEQUENCE_KEY_IDS` 表（内容有重叠）识别 legacy 序列，
/// 不调用本函数。Kitty 的 CSI-u 路径没有功能键支持。
fn function_key_sequences(key: char) -> Option<&'static [&'static str]> {
    Some(match key {
        SENTINEL_F1 => &LEGACY_F1,
        SENTINEL_F2 => &LEGACY_F2,
        SENTINEL_F3 => &LEGACY_F3,
        SENTINEL_F4 => &LEGACY_F4,
        SENTINEL_F5 => &LEGACY_F5,
        SENTINEL_F6 => &LEGACY_F6,
        SENTINEL_F7 => &LEGACY_F7,
        SENTINEL_F8 => &LEGACY_F8,
        SENTINEL_F9 => &LEGACY_F9,
        SENTINEL_F10 => &LEGACY_F10,
        SENTINEL_F11 => &LEGACY_F11,
        SENTINEL_F12 => &LEGACY_F12,
        _ => return None,
    })
}

/// 把解析出的码点与修饰符格式化成键标识符；无法表示时返回 `None`。
///
/// 步骤：
/// 1. 折叠小键盘功能码点，并把 Shift 上报的大写拉丁字母折成小写；
/// 2. 若码点不是拉丁字母/数字/已知符号，优先采用终端上报的 base layout key；
/// 3. 把特殊码点映射成 `escape` / `enter` / `pageUp` 等规范名字，普通字母数字符号转成
///    单字符；
/// 4. 交给 `format_key_name_with_modifiers` 按固定顺序拼接修饰符。
///
/// 与 `matches_kitty_sequence` 的差异：这里把数字也视为权威码点，不做 base layout 回退。
/// 无法识别的码点（例如码点 0）或含不支持的修饰位（例如 hyper）都会返回 `None`，
/// 宁可解析失败也不猜。
fn format_parsed_key(
    codepoint: i32,
    modifier: u32,
    base_layout_key: Option<i32>,
) -> Option<String> {
    let normalized_codepoint = normalize_kitty_functional_codepoint(codepoint);
    let identity_codepoint =
        normalize_shifted_letter_identity_codepoint(normalized_codepoint, modifier);

    // 只有当码点不是已识别的拉丁字母（a-z）、数字（0-9）或符号时才用 base layout key。
    // 这些情况下码点是权威的，与物理键位无关。
    // 注意匹配路径 `matches_kitty_sequence` 的同一判断**不包含数字**，两处行为不同。
    let is_latin_letter = (97..=122).contains(&identity_codepoint);
    let is_digit = (48..=57).contains(&identity_codepoint);
    let is_known_symbol = char_from_codepoint(identity_codepoint).is_some_and(is_symbol_key);
    let effective_codepoint = if is_latin_letter || is_digit || is_known_symbol {
        identity_codepoint
    } else {
        base_layout_key.unwrap_or(identity_codepoint)
    };

    // 特殊码点 → 规范键名；数字/小写字母/符号在末尾两个分支按字符输出。
    let key_name: String = if effective_codepoint == CODEPOINT_ESCAPE {
        "escape".into()
    } else if effective_codepoint == CODEPOINT_TAB {
        "tab".into()
    } else if effective_codepoint == CODEPOINT_ENTER || effective_codepoint == CODEPOINT_KP_ENTER {
        "enter".into()
    } else if effective_codepoint == CODEPOINT_SPACE {
        "space".into()
    } else if effective_codepoint == CODEPOINT_BACKSPACE {
        "backspace".into()
    } else if effective_codepoint == FUNCTIONAL_DELETE {
        "delete".into()
    } else if effective_codepoint == FUNCTIONAL_INSERT {
        "insert".into()
    } else if effective_codepoint == FUNCTIONAL_HOME {
        "home".into()
    } else if effective_codepoint == FUNCTIONAL_END {
        "end".into()
    } else if effective_codepoint == FUNCTIONAL_PAGE_UP {
        "pageUp".into()
    } else if effective_codepoint == FUNCTIONAL_PAGE_DOWN {
        "pageDown".into()
    } else if effective_codepoint == ARROW_UP {
        "up".into()
    } else if effective_codepoint == ARROW_DOWN {
        "down".into()
    } else if effective_codepoint == ARROW_LEFT {
        "left".into()
    } else if effective_codepoint == ARROW_RIGHT {
        "right".into()
    } else if (48..=57).contains(&effective_codepoint) || (97..=122).contains(&effective_codepoint)
    {
        // TS 是两条分支（`keys.ts:1284-1285`，数字 / 小写字母），体完全相同；合并成一条，
        // 语义不变（clippy `if_same_then_else`）。
        char_from_codepoint(effective_codepoint)?.to_string()
    } else if char_from_codepoint(effective_codepoint).is_some_and(is_symbol_key) {
        char_from_codepoint(effective_codepoint)?.to_string()
    } else {
        return None;
    };

    format_key_name_with_modifiers(&key_name, modifier)
}

/// 解析输入数据并返回键标识符（`parseKey`）。
///
/// 识别顺序固定：
/// 1. CSI-u（无论 Kitty 是否激活都先试）；
/// 2. xterm modifyOtherKeys；
/// 3. Kitty 激活时的歧义 legacy 字节（`\x1b\r` / `\n` → `shift+enter`）；
/// 4. `LEGACY_SEQUENCE_KEY_IDS` 表；
/// 5. 其余明确的 legacy 字节/序列；
/// 6. 单字节控制字符（Ctrl+字母）与可打印 ASCII。
///
/// 返回 `None` 表示这条输入没有可表达的键标识符（例如未知的多字节序列，或 Kitty 激活时
/// 按终端自定义映射解释的 `\x1ba`）。修饰符按 shift、ctrl、alt、super 的固定顺序输出，
/// 因此同一按键可能得到与配置文件写法不同但等价的字符串。
///
/// 注意：只识别**一条完整序列**；不消费剩余字符，也不负责拆包与超时。
pub fn parse_key(data: &str) -> Option<String> {
    // 两条显式协议路径都不依赖 Kitty 状态，所以即使协议未激活也会先尝试；
    // 未激活时它们仍按同样的规则解析 CSI 序列。
    if let Some(kitty) = parse_kitty_sequence(data) {
        let logical_shifted_symbol =
            get_logical_shifted_symbol_candidate(kitty.shifted_key, kitty.modifier, &SYMBOL_KEYS);
        return format_parsed_key(
            logical_shifted_symbol.map_or(kitty.codepoint, |(codepoint, _)| codepoint),
            logical_shifted_symbol.map_or(kitty.modifier, |(_, modifier)| modifier),
            kitty.base_layout_key,
        );
    }

    // modifyOtherKeys 没有 shifted key 字段，逻辑匹配只能用 SHIFTED_SYMBOL_KEYS 判断
    // “这个码点是不是 Shift 产生的符号”。
    if let Some(modify_other_keys) = parse_modify_other_keys_sequence(data) {
        let logical_shifted_symbol = get_logical_shifted_symbol_candidate(
            Some(modify_other_keys.codepoint),
            modify_other_keys.modifier,
            &SHIFTED_SYMBOL_KEYS,
        );
        return format_parsed_key(
            logical_shifted_symbol.map_or(modify_other_keys.codepoint, |(codepoint, _)| codepoint),
            logical_shifted_symbol.map_or(modify_other_keys.modifier, |(_, modifier)| modifier),
            None,
        );
    }

    let kitty_active = is_kitty_protocol_active();
    // 模式相关的 legacy 序列：Kitty 激活时，有歧义的序列按终端自定义映射解释：
    // - \x1b\r = shift+enter（Kitty 映射），不是 alt+enter
    // - \n = shift+enter（Ghostty 映射）
    if kitty_active && (data == "\x1b\r" || data == "\n") {
        return Some("shift+enter".into());
    }

    // 逐字表按 TS 顺序匹配；命中即返回，未命中再走下面的逐条判断。
    if let Some(id) = legacy_sequence_key_id(data) {
        return Some(id.to_string());
    }

    // Legacy 序列（Kitty 未激活时，或本就是无歧义的序列）。
    match data {
        "\x1b" => return Some("escape".into()),
        "\x1c" => return Some("ctrl+\\".into()),
        "\x1d" => return Some("ctrl+]".into()),
        "\x1f" => return Some("ctrl+-".into()),
        "\x1b\x1b" => return Some("ctrl+alt+[".into()),
        "\x1b\x1c" => return Some("ctrl+alt+\\".into()),
        "\x1b\x1d" => return Some("ctrl+alt+]".into()),
        "\x1b\x1f" => return Some("ctrl+alt+-".into()),
        "\t" => return Some("tab".into()),
        "\r" => return Some("enter".into()),
        "\x00" => return Some("ctrl+space".into()),
        " " => return Some("space".into()),
        "\x7f" => return Some("backspace".into()),
        "\x1b[Z" => return Some("shift+tab".into()),
        "\x1b\x7f" | "\x1b\u{8}" => return Some("alt+backspace".into()),
        _ => {}
    }
    if !kitty_active && data == "\n" {
        return Some("enter".into());
    }
    if data == "\x1bOM" {
        return Some("enter".into());
    }
    if data == "\x08" {
        return Some(if is_windows_terminal_session() {
            "ctrl+backspace".into()
        } else {
            "backspace".into()
        });
    }
    if !kitty_active && data == "\x1b\r" {
        return Some("alt+enter".into());
    }
    if !kitty_active && data == "\x1b " {
        return Some("alt+space".into());
    }
    if !kitty_active && data == "\x1bB" {
        return Some("alt+left".into());
    }
    if !kitty_active && data == "\x1bF" {
        return Some("alt+right".into());
    }
    // ESC + 单字节：1-26 是 Ctrl+Alt+字母（ESC 后跟控制字符）；小写字母/数字/符号是
    // Alt+字符（与 TS 的 `a-z` / `0-9` / `SYMBOL_KEYS` 判定一致）。大写字母等其余可打印
    // 字符不在这里识别，会继续落到后面的规则（最终通常返回 `None`）。
    if !kitty_active && data.len() == 2 && data.starts_with('\x1b') {
        let ch = data.chars().nth(1).expect("len 2");
        let code = ch as u32;
        if (1..=26).contains(&code) {
            return char::from_u32(code + 96).map(|ch| format!("ctrl+alt+{ch}"));
        }
        // Legacy alt+字母/数字/符号（ESC 后跟该键）。
        if ch.is_ascii_lowercase() || ch.is_ascii_digit() || is_symbol_key(ch) {
            return Some(format!("alt+{ch}"));
        }
    }

    // 无歧义的方向键/编辑键 legacy 序列（不在 `LEGACY_SEQUENCE_KEY_IDS` 表里的那些）。
    match data {
        "\x1b[A" => return Some("up".into()),
        "\x1b[B" => return Some("down".into()),
        "\x1b[C" => return Some("right".into()),
        "\x1b[D" => return Some("left".into()),
        "\x1b[H" | "\x1bOH" => return Some("home".into()),
        "\x1b[F" | "\x1bOF" => return Some("end".into()),
        "\x1b[3~" => return Some("delete".into()),
        "\x1b[5~" => return Some("pageUp".into()),
        "\x1b[6~" => return Some("pageDown".into()),
        _ => {}
    }

    // 原始 Ctrl+字母。
    // 单字节：1..=26 是 Ctrl+字母，32..=126 原样作为可打印字符；
    // 其余控制字节（DEL 等）已在上面处理，走到这里仍是单字节就返回 None。
    if data.chars().count() == 1 {
        let ch = data.chars().next().expect("len 1");
        let code = ch as u32;
        if (1..=26).contains(&code) {
            return char::from_u32(code + 96).map(|ch| format!("ctrl+{ch}"));
        }
        if (32..=126).contains(&code) {
            return Some(data.to_string());
        }
    }

    None
}

// =============================================================================
// Kitty CSI-u 可打印字符解码
// =============================================================================

/// 可打印字符允许的修饰位：Shift 与两个锁定位；其余位（Ctrl/Alt/Super/hyper…）一律拒绝。
///
/// 位掩码让 `decode_kitty_printable` 能一次性排除所有不支持的组合。
const KITTY_PRINTABLE_ALLOWED_MODIFIERS: u32 = MODIFIER_SHIFT | LOCK_MASK;

/// 把 Kitty CSI-u 序列解码成可打印字符（`decodeKittyPrintable`）。
///
/// Kitty flag 1（disambiguate）激活时，终端会为所有按键（包括普通可打印字符）发 CSI-u 序列；
/// 本函数从中取出可打印字符。只接受无修饰或仅 Shift 的按键；拒绝 Ctrl、Alt 与不支持的
/// 修饰符组合（那些交给键位匹配）。Shift 按下且终端上报了移位码点时优先用它。
///
/// 只处理 CSI-u，不处理 modifyOtherKeys（后者由 `decode_modify_other_keys_printable`
/// 负责）。事件类型字段被忽略：释放事件同样会解出字符，直接调用本函数的代码需要自己用
/// [`is_key_release`] 过滤；主 TUI 分发路径在 `crate::tui` 里已先做过滤（除非组件声明
/// 需要释放事件）。码点小于 32 的一律丢弃，代理区码点变成 U+FFFD。
pub fn decode_kitty_printable(data: &str) -> Option<String> {
    let (codepoint, shifted_key, _base, modifier, _event) = match_csi_u(data)?;
    let mod_value = modifier.unwrap_or(1);
    let modifier = mod_value.wrapping_sub(1) as u32;

    // 只接受无修饰或仅 Shift 的可打印 CSI-u 输入；拒绝不支持的修饰位（例如 Super/Meta），
    // 避免把纯修饰键事件插入字符。
    if (modifier & !KITTY_PRINTABLE_ALLOWED_MODIFIERS) != 0 {
        return None;
    }
    if modifier & (MODIFIER_ALT | MODIFIER_CTRL) != 0 {
        return None;
    }

    // Shift 按下时优先用移位码点。
    let mut effective_codepoint = codepoint;
    if modifier & MODIFIER_SHIFT != 0
        && let Some(shifted) = shifted_key
    {
        effective_codepoint = shifted;
    }
    effective_codepoint = normalize_kitty_functional_codepoint(effective_codepoint);
    // 丢弃控制字符或非法码点。
    if effective_codepoint < 32 {
        return None;
    }
    char_from_code_point(u32::try_from(effective_codepoint).ok()?).map(|ch| ch.to_string())
}

/// 从 modifyOtherKeys 序列解出可打印字符。
///
/// 先屏蔽锁定位，只接受无修饰或仅 Shift 的按键；拒绝 Ctrl/Alt/Super 与小于 32 的控制
/// 字符。该协议没有 shifted key 字段，所以直接使用上报的码点（测试里 `\x1b[27;2;69~`
/// 解出 `"E"`，`\x1b[27;2;196~` 解出 `"Ä"`）。
fn decode_modify_other_keys_printable(data: &str) -> Option<String> {
    let parsed = parse_modify_other_keys_sequence(data)?;
    let modifier = parsed.modifier & !LOCK_MASK;
    if (modifier & !MODIFIER_SHIFT) != 0 {
        return None;
    }
    if parsed.codepoint < 32 {
        return None;
    }
    char_from_code_point(u32::try_from(parsed.codepoint).ok()?).map(|ch| ch.to_string())
}

/// TS `String.fromCodePoint(codepoint)` 的 Rust 等价物。
///
/// JS 接受代理区码点（0xD800–0xDFFF）并返回一个 UTF-16 码元；Rust 的 `char` 不能表示孤立
/// 代理。Node 把这个码元输出成 UTF-8 时得到 U+FFFD（EF BF BD），因此这里用 U+FFFD 代替，
/// 其余范围与 `char::from_u32` 相同（大于 0x10FFFF 仍返回 `None`）。
fn char_from_code_point(codepoint: u32) -> Option<char> {
    if (0xD800..=0xDFFF).contains(&codepoint) {
        return Some('\u{FFFD}');
    }
    char::from_u32(codepoint)
}

/// 解码可打印按键（`decodePrintableKey`）：先试 CSI-u，再试 xterm modifyOtherKeys。
///
/// 两种格式以结尾字符区分（`u` 与 `~`），不会同时匹配。解析与 Kitty 协议是否激活无关。
/// 返回 `None` 表示这不是一条可打印按键，调用方应回退到原始字符处理
/// （参见 `crate::components::editor` 的字符跳转分支）。
pub fn decode_printable_key(data: &str) -> Option<String> {
    decode_kitty_printable(data).or_else(|| decode_modify_other_keys_printable(data))
}

// 单元测试覆盖：CSI-u / modifyOtherKeys / legacy 三类输入的匹配与解析、Shift 符号的
// 逻辑匹配、base layout 回退，以及括号粘贴内容不被误判为 release/repeat。
// 更完整的 TS 用例移植（61 个 it）在 `tests/keys.rs`；那里也用互斥量串行化全局状态。
#[cfg(test)]
mod tests {
    use super::*;

    /// 全局 Kitty 状态是线程级的，而 Rust 测试默认多线程；用互斥量串行化。
    fn kitty_guard(active: bool) -> KitkatGuard {
        static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let guard = LOCK.lock().unwrap_or_else(|error| error.into_inner());
        set_kitty_protocol_active(active);
        KitkatGuard { _guard: guard }
    }

    struct KitkatGuard {
        _guard: std::sync::MutexGuard<'static, ()>,
    }

    impl Drop for KitkatGuard {
        fn drop(&mut self) {
            set_kitty_protocol_active(false);
        }
    }

    #[test]
    fn matches_ctrl_c_with_cyrillic_base_layout_key() {
        let _guard = kitty_guard(true);
        let cyrillic_ctrl_c = "\x1b[1089::99;5u";
        assert!(matches_key(cyrillic_ctrl_c, "ctrl+c"));
    }

    #[test]
    fn matches_ctrl_d_and_ctrl_z_with_cyrillic_base_layout_key() {
        let _guard = kitty_guard(true);
        assert!(matches_key("\x1b[1074::100;5u", "ctrl+d"));
        assert!(matches_key("\x1b[1103::122;5u", "ctrl+z"));
    }

    #[test]
    fn matches_ctrl_shift_p_with_base_layout_key() {
        let _guard = kitty_guard(true);
        assert!(matches_key("\x1b[1079::112;6u", "ctrl+shift+p"));
    }

    #[test]
    fn matches_direct_codepoint_when_no_base_layout_key() {
        let _guard = kitty_guard(true);
        assert!(matches_key("\x1b[99;5u", "ctrl+c"));
    }

    #[test]
    fn matches_super_modified_kitty_bindings_including_combined_modifiers() {
        let _guard = kitty_guard(true);
        assert!(matches_key("\x1b[107;9u", "super+k"));
        assert!(matches_key("\x1b[13;9u", "super+enter"));
        assert!(matches_key("\x1b[107;13u", &Key::ctrl_super("k")));
        assert!(matches_key("\x1b[107;13u", "ctrl+super+k"));
        assert!(matches_key("\x1b[107;14u", "ctrl+shift+super+k"));
        assert!(!matches_key("\x1b[107;13u", "super+k"));
        assert_eq!(parse_key("\x1b[107;9u").as_deref(), Some("super+k"));
        assert_eq!(parse_key("\x1b[13;9u").as_deref(), Some("super+enter"));
        assert_eq!(parse_key("\x1b[107;13u").as_deref(), Some("ctrl+super+k"));
        assert_eq!(
            parse_key("\x1b[107;14u").as_deref(),
            Some("shift+ctrl+super+k")
        );
    }

    #[test]
    fn matches_digit_bindings_via_kitty_csi_u() {
        let _guard = kitty_guard(true);
        assert!(matches_key("\x1b[49u", "1"));
        assert!(matches_key("\x1b[49;5u", "ctrl+1"));
        assert!(!matches_key("\x1b[49;5u", "ctrl+2"));
        assert_eq!(parse_key("\x1b[49u").as_deref(), Some("1"));
        assert_eq!(parse_key("\x1b[49;5u").as_deref(), Some("ctrl+1"));
    }

    #[test]
    fn normalizes_kitty_keypad_functional_keys() {
        let _guard = kitty_guard(true);
        assert!(matches_key("\x1b[57400u", "1"));
        assert!(matches_key("\x1b[57410u", "/"));
        assert!(matches_key("\x1b[57417u", "left"));
        assert!(matches_key("\x1b[57426u", "delete"));
        for (sequence, expected) in [
            ("\x1b[57399u", "0"),
            ("\x1b[57409u", "."),
            ("\x1b[57413u", "+"),
            ("\x1b[57416u", ","),
            ("\x1b[57417u", "left"),
            ("\x1b[57418u", "right"),
            ("\x1b[57419u", "up"),
            ("\x1b[57420u", "down"),
            ("\x1b[57421u", "pageUp"),
            ("\x1b[57422u", "pageDown"),
            ("\x1b[57423u", "home"),
            ("\x1b[57424u", "end"),
            ("\x1b[57425u", "insert"),
            ("\x1b[57426u", "delete"),
        ] {
            assert_eq!(parse_key(sequence).as_deref(), Some(expected), "{sequence}");
        }
    }

    #[test]
    fn matches_shifted_symbols_by_their_produced_character() {
        let _guard = kitty_guard(true);
        assert!(matches_key("\x1b[61:43;2u", "+"));
        assert!(matches_key("\x1b[61:43;2u", "shift+="));
        assert_eq!(parse_key("\x1b[61:43;2u").as_deref(), Some("+"));
        assert!(matches_key("\x1b[61:43;6u", "ctrl++"));
        assert_eq!(parse_key("\x1b[61:43;6u").as_deref(), Some("ctrl++"));
    }

    #[test]
    fn handles_shifted_key_in_format() {
        let _guard = kitty_guard(true);
        let shifted_key = "\x1b[99:67:99;2u";
        assert!(matches_key(shifted_key, "shift+c"));
    }

    #[test]
    fn handles_event_type_in_format() {
        let _guard = kitty_guard(true);
        let release_event = "\x1b[1089::99;5:3u";
        assert!(matches_key(release_event, "ctrl+c"));
    }

    #[test]
    fn handles_full_format_with_shifted_base_and_event() {
        let _guard = kitty_guard(true);
        let full_format = "\x1b[1089:1057:99;6:2u";
        assert!(matches_key(full_format, "ctrl+shift+c"));
    }

    #[test]
    fn prefers_codepoint_for_latin_letters_even_when_base_layout_differs() {
        let _guard = kitty_guard(true);
        let dvorak_ctrl_k = "\x1b[107::118;5u";
        assert!(matches_key(dvorak_ctrl_k, "ctrl+k"));
        assert!(!matches_key(dvorak_ctrl_k, "ctrl+v"));
    }

    #[test]
    fn prefers_codepoint_for_symbol_keys_even_when_base_layout_differs() {
        let _guard = kitty_guard(true);
        let dvorak_ctrl_slash = "\x1b[47::91;5u";
        assert!(matches_key(dvorak_ctrl_slash, "ctrl+/"));
        assert!(!matches_key(dvorak_ctrl_slash, "ctrl+["));
    }

    #[test]
    fn does_not_match_wrong_key_or_modifiers_even_with_base_layout() {
        let _guard = kitty_guard(true);
        let cyrillic_ctrl_c = "\x1b[1089::99;5u";
        assert!(!matches_key(cyrillic_ctrl_c, "ctrl+d"));
        assert!(!matches_key(cyrillic_ctrl_c, "ctrl+shift+c"));
    }

    // ---- modifyOtherKeys ----

    #[test]
    fn matches_xterm_modify_other_keys_ctrl_letters() {
        let _guard = kitty_guard(false);
        assert!(matches_key("\x1b[27;5;99~", "ctrl+c"));
        assert_eq!(parse_key("\x1b[27;5;99~").as_deref(), Some("ctrl+c"));
        assert!(matches_key("\x1b[27;5;100~", "ctrl+d"));
        assert_eq!(parse_key("\x1b[27;5;100~").as_deref(), Some("ctrl+d"));
        assert!(matches_key("\x1b[27;5;122~", "ctrl+z"));
        assert_eq!(parse_key("\x1b[27;5;122~").as_deref(), Some("ctrl+z"));
    }

    #[test]
    fn matches_xterm_modify_other_keys_enter_variants() {
        let _guard = kitty_guard(false);
        assert!(matches_key("\x1b[27;5;13~", "ctrl+enter"));
        assert!(matches_key("\x1b[27;2;13~", "shift+enter"));
        assert!(matches_key("\x1b[27;3;13~", "alt+enter"));
        assert_eq!(parse_key("\x1b[27;5;13~").as_deref(), Some("ctrl+enter"));
        assert_eq!(parse_key("\x1b[27;2;13~").as_deref(), Some("shift+enter"));
        assert_eq!(parse_key("\x1b[27;3;13~").as_deref(), Some("alt+enter"));
    }

    #[test]
    fn matches_xterm_modify_other_keys_tab_variants() {
        let _guard = kitty_guard(false);
        assert!(matches_key("\x1b[27;2;9~", "shift+tab"));
        assert!(matches_key("\x1b[27;5;9~", "ctrl+tab"));
        assert!(matches_key("\x1b[27;3;9~", "alt+tab"));
        assert_eq!(parse_key("\x1b[27;2;9~").as_deref(), Some("shift+tab"));
        assert_eq!(parse_key("\x1b[27;5;9~").as_deref(), Some("ctrl+tab"));
        assert_eq!(parse_key("\x1b[27;3;9~").as_deref(), Some("alt+tab"));
    }

    #[test]
    fn matches_xterm_modify_other_keys_backspace_variants() {
        let _guard = kitty_guard(false);
        assert!(matches_key("\x1b[27;1;127~", "backspace"));
        assert!(matches_key("\x1b[27;5;127~", "ctrl+backspace"));
        assert!(matches_key("\x1b[27;3;127~", "alt+backspace"));
        assert_eq!(parse_key("\x1b[27;1;127~").as_deref(), Some("backspace"));
        assert_eq!(
            parse_key("\x1b[27;5;127~").as_deref(),
            Some("ctrl+backspace")
        );
        assert_eq!(
            parse_key("\x1b[27;3;127~").as_deref(),
            Some("alt+backspace")
        );
    }

    #[test]
    fn matches_xterm_modify_other_keys_escape_and_space() {
        let _guard = kitty_guard(false);
        assert!(matches_key("\x1b[27;1;27~", "escape"));
        assert_eq!(parse_key("\x1b[27;1;27~").as_deref(), Some("escape"));
        assert!(matches_key("\x1b[27;1;32~", "space"));
        assert!(matches_key("\x1b[27;5;32~", "ctrl+space"));
        assert_eq!(parse_key("\x1b[27;1;32~").as_deref(), Some("space"));
        assert_eq!(parse_key("\x1b[27;5;32~").as_deref(), Some("ctrl+space"));
    }

    #[test]
    fn matches_xterm_modify_other_keys_symbol_and_digit_combos() {
        let _guard = kitty_guard(false);
        assert!(matches_key("\x1b[27;5;47~", "ctrl+/"));
        assert_eq!(parse_key("\x1b[27;5;47~").as_deref(), Some("ctrl+/"));
        assert!(matches_key("\x1b[27;6;43~", "ctrl++"));
        assert_eq!(parse_key("\x1b[27;6;43~").as_deref(), Some("ctrl++"));
        assert!(matches_key("\x1b[27;5;49~", "ctrl+1"));
        assert!(matches_key("\x1b[27;2;49~", "shift+1"));
        assert_eq!(parse_key("\x1b[27;5;49~").as_deref(), Some("ctrl+1"));
        assert_eq!(parse_key("\x1b[27;2;49~").as_deref(), Some("shift+1"));
    }

    #[test]
    fn matches_xterm_modify_other_keys_shifted_uppercase_letters() {
        let _guard = kitty_guard(false);
        assert!(matches_key("\x1b[27;2;69~", "shift+e"));
        assert!(matches_key("\x1b[27;6;69~", "ctrl+shift+e"));
        assert_eq!(parse_key("\x1b[27;2;69~").as_deref(), Some("shift+e"));
        assert_eq!(parse_key("\x1b[27;6;69~").as_deref(), Some("shift+ctrl+e"));
    }

    #[test]
    fn matches_ctrl_alt_letter_via_csi_u_and_modify_other_keys_when_kitty_inactive() {
        let _guard = kitty_guard(false);
        assert!(matches_key("\x1b[104;7u", "ctrl+alt+h"));
        assert_eq!(parse_key("\x1b[104;7u").as_deref(), Some("ctrl+alt+h"));
        assert!(matches_key("\x1b[27;7;104~", "ctrl+alt+h"));
        assert_eq!(parse_key("\x1b[27;7;104~").as_deref(), Some("ctrl+alt+h"));
    }

    // ---- Legacy ----

    #[test]
    fn matches_legacy_ctrl_letters() {
        let _guard = kitty_guard(false);
        assert!(matches_key("\x03", "ctrl+c"));
        assert!(matches_key("\x04", "ctrl+d"));
    }

    #[test]
    fn matches_escape_key() {
        drop(kitty_guard(false));
        assert!(matches_key("\x1b", "escape"));
    }

    #[test]
    fn matches_legacy_linefeed_as_enter_and_as_shift_enter_with_kitty() {
        let _guard = kitty_guard(false);
        assert!(matches_key("\n", "enter"));
        assert_eq!(parse_key("\n").as_deref(), Some("enter"));

        set_kitty_protocol_active(true);
        assert!(matches_key("\n", "shift+enter"));
        assert!(!matches_key("\n", "enter"));
        assert_eq!(parse_key("\n").as_deref(), Some("shift+enter"));
    }

    #[test]
    fn parses_ctrl_space() {
        let _guard = kitty_guard(false);
        assert!(matches_key("\x00", "ctrl+space"));
        assert_eq!(parse_key("\x00").as_deref(), Some("ctrl+space"));
    }

    #[test]
    fn matches_legacy_ctrl_symbol() {
        let _guard = kitty_guard(false);
        assert!(matches_key("\x1c", "ctrl+\\"));
        assert_eq!(parse_key("\x1c").as_deref(), Some("ctrl+\\"));
        assert!(matches_key("\x1d", "ctrl+]"));
        assert_eq!(parse_key("\x1d").as_deref(), Some("ctrl+]"));
        assert!(matches_key("\x1f", "ctrl+_"));
        assert!(matches_key("\x1f", "ctrl+-"));
        assert_eq!(parse_key("\x1f").as_deref(), Some("ctrl+-"));
    }

    #[test]
    fn matches_legacy_ctrl_alt_symbol() {
        let _guard = kitty_guard(false);
        assert!(matches_key("\x1b\x1b", "ctrl+alt+["));
        assert_eq!(parse_key("\x1b\x1b").as_deref(), Some("ctrl+alt+["));
        assert!(matches_key("\x1b\x1c", "ctrl+alt+\\"));
        assert_eq!(parse_key("\x1b\x1c").as_deref(), Some("ctrl+alt+\\"));
        assert!(matches_key("\x1b\x1d", "ctrl+alt+]"));
        assert_eq!(parse_key("\x1b\x1d").as_deref(), Some("ctrl+alt+]"));
        assert!(matches_key("\x1b\x1f", "ctrl+alt+_"));
        assert!(matches_key("\x1b\x1f", "ctrl+alt+-"));
        assert_eq!(parse_key("\x1b\x1f").as_deref(), Some("ctrl+alt+-"));
    }

    #[test]
    fn treats_raw_0x08_as_plain_backspace_outside_windows_terminal() {
        let _guard = kitty_guard(false);
        // SAFETY: this test mutates the process environment; the guard serializes key tests.
        assert!(std::env::var_os("WT_SESSION").is_none());
        assert!(matches_key("\x7f", "backspace"));
        assert!(!matches_key("\x7f", "ctrl+backspace"));
        assert_eq!(parse_key("\x7f").as_deref(), Some("backspace"));
        assert!(matches_key("\x08", "backspace"));
        assert!(!matches_key("\x08", "ctrl+backspace"));
        assert_eq!(parse_key("\x08").as_deref(), Some("backspace"));
        assert!(matches_key("\x08", "ctrl+h"));
    }

    #[test]
    fn parses_legacy_alt_prefixed_sequences_when_kitty_inactive() {
        let _guard = kitty_guard(false);
        assert!(matches_key("\x1b ", "alt+space"));
        assert_eq!(parse_key("\x1b ").as_deref(), Some("alt+space"));
        assert!(matches_key("\x1b\u{8}", "alt+backspace"));
        assert_eq!(parse_key("\x1b\u{8}").as_deref(), Some("alt+backspace"));
        assert!(matches_key("\x1b\x03", "ctrl+alt+c"));
        assert_eq!(parse_key("\x1b\x03").as_deref(), Some("ctrl+alt+c"));
        assert!(matches_key("\x1bB", "alt+left"));
        assert_eq!(parse_key("\x1bB").as_deref(), Some("alt+left"));
        assert!(matches_key("\x1bF", "alt+right"));
        assert_eq!(parse_key("\x1bF").as_deref(), Some("alt+right"));
        assert!(matches_key("\x1ba", "alt+a"));
        assert_eq!(parse_key("\x1ba").as_deref(), Some("alt+a"));
        assert!(matches_key("\x1b1", "alt+1"));
        assert_eq!(parse_key("\x1b1").as_deref(), Some("alt+1"));
        assert!(matches_key("\x1b,", "alt+,"));
        assert_eq!(parse_key("\x1b,").as_deref(), Some("alt+,"));
        assert!(matches_key("\x1b.", "alt+."));
        assert_eq!(parse_key("\x1b.").as_deref(), Some("alt+."));
        assert!(matches_key("\x1by", "alt+y"));
        assert_eq!(parse_key("\x1by").as_deref(), Some("alt+y"));
        assert!(matches_key("\x1bz", "alt+z"));
        assert_eq!(parse_key("\x1bz").as_deref(), Some("alt+z"));

        set_kitty_protocol_active(true);
        assert!(!matches_key("\x1b ", "alt+space"));
        assert_eq!(parse_key("\x1b ").as_deref(), None);
        assert!(matches_key("\x1b\u{8}", "alt+backspace"));
        assert_eq!(parse_key("\x1b\u{8}").as_deref(), Some("alt+backspace"));
        assert!(!matches_key("\x1b\x03", "ctrl+alt+c"));
        assert_eq!(parse_key("\x1b\x03").as_deref(), None);
        assert!(!matches_key("\x1bB", "alt+left"));
        assert_eq!(parse_key("\x1bB").as_deref(), None);
        assert!(!matches_key("\x1bF", "alt+right"));
        assert_eq!(parse_key("\x1bF").as_deref(), None);
        assert!(!matches_key("\x1ba", "alt+a"));
        assert_eq!(parse_key("\x1ba").as_deref(), None);
        assert!(!matches_key("\x1b1", "alt+1"));
        assert_eq!(parse_key("\x1b1").as_deref(), None);
        assert!(!matches_key("\x1b,", "alt+,"));
        assert_eq!(parse_key("\x1b,").as_deref(), None);
        assert!(!matches_key("\x1b.", "alt+."));
        assert_eq!(parse_key("\x1b.").as_deref(), None);
        assert!(!matches_key("\x1by", "alt+y"));
        assert_eq!(parse_key("\x1by").as_deref(), None);
    }

    #[test]
    fn matches_arrows_ss3_and_modifier_variants() {
        drop(kitty_guard(false));
        assert!(matches_key("\x1b[A", "up"));
        assert!(matches_key("\x1b[B", "down"));
        assert!(matches_key("\x1b[C", "right"));
        assert!(matches_key("\x1b[D", "left"));
        assert!(matches_key("\x1bOA", "up"));
        assert!(matches_key("\x1bOB", "down"));
        assert!(matches_key("\x1bOC", "right"));
        assert!(matches_key("\x1bOD", "left"));
        assert!(matches_key("\x1bOH", "home"));
        assert!(matches_key("\x1bOF", "end"));
    }

    #[test]
    fn matches_xterm_ctrl_modified_viewport_navigation() {
        drop(kitty_guard(false));
        assert!(matches_key("\x1b[1;5H", "ctrl+home"));
        assert!(matches_key("\x1b[1;5F", "ctrl+end"));
        assert!(matches_key("\x1b[5;5~", "ctrl+pageUp"));
        assert!(matches_key("\x1b[6;5~", "ctrl+pageDown"));
        assert_eq!(parse_key("\x1b[1;5H").as_deref(), Some("ctrl+home"));
        assert_eq!(parse_key("\x1b[1;5F").as_deref(), Some("ctrl+end"));
        assert_eq!(parse_key("\x1b[5;5~").as_deref(), Some("ctrl+pageUp"));
        assert_eq!(parse_key("\x1b[6;5~").as_deref(), Some("ctrl+pageDown"));
    }

    #[test]
    fn matches_legacy_function_keys_clear_and_alt_arrows() {
        drop(kitty_guard(false));
        assert!(matches_key("\x1bOP", "f1"));
        assert!(matches_key("\x1b[24~", "f12"));
        assert!(matches_key("\x1b[E", "clear"));
        assert!(matches_key("\x1bp", "alt+up"));
        assert!(!matches_key("\x1bp", "up"));
    }

    #[test]
    fn matches_rxvt_modifier_sequences() {
        drop(kitty_guard(false));
        assert!(matches_key("\x1b[a", "shift+up"));
        assert!(matches_key("\x1bOa", "ctrl+up"));
        assert!(matches_key("\x1b[2$", "shift+insert"));
        assert!(matches_key("\x1b[2^", "ctrl+insert"));
        assert!(matches_key("\x1b[7$", "shift+home"));
    }

    // ---- decodeKittyPrintable / decodePrintableKey ----

    #[test]
    fn decodes_kitty_keypad_functional_keys_to_printable_characters() {
        assert_eq!(decode_kitty_printable("\x1b[57399u").as_deref(), Some("0"));
        assert_eq!(decode_kitty_printable("\x1b[57400u").as_deref(), Some("1"));
        assert_eq!(decode_kitty_printable("\x1b[57409u").as_deref(), Some("."));
        assert_eq!(decode_kitty_printable("\x1b[57410u").as_deref(), Some("/"));
        assert_eq!(decode_kitty_printable("\x1b[57411u").as_deref(), Some("*"));
        assert_eq!(decode_kitty_printable("\x1b[57412u").as_deref(), Some("-"));
        assert_eq!(decode_kitty_printable("\x1b[57413u").as_deref(), Some("+"));
        assert_eq!(decode_kitty_printable("\x1b[57415u").as_deref(), Some("="));
        assert_eq!(decode_kitty_printable("\x1b[57416u").as_deref(), Some(","));
        assert_eq!(decode_kitty_printable("\x1b[57417u"), None);
    }

    #[test]
    fn decodes_printable_xterm_modify_other_keys_sequences() {
        assert_eq!(decode_printable_key("\x1b[27;2;69~").as_deref(), Some("E"));
        assert_eq!(decode_printable_key("\x1b[27;2;196~").as_deref(), Some("Ä"));
        assert_eq!(decode_printable_key("\x1b[27;2;32~").as_deref(), Some(" "));
        assert_eq!(decode_printable_key("\x1b[27;2;13~"), None);
        assert_eq!(decode_printable_key("\x1b[27;6;69~"), None);
    }

    // ---- parseKey ----

    #[test]
    fn parse_key_prefers_latin_codepoint_and_rejects_unsupported_modifiers() {
        let _guard = kitty_guard(true);
        assert_eq!(parse_key("\x1b[1089::99;5u").as_deref(), Some("ctrl+c"));
        assert_eq!(parse_key("\x1b[107::118;5u").as_deref(), Some("ctrl+k"));
        assert_eq!(parse_key("\x1b[47::91;5u").as_deref(), Some("ctrl+/"));
        assert_eq!(parse_key("\x1b[99;5u").as_deref(), Some("ctrl+c"));
        assert!(matches_key("\x1b[69;2u", "shift+e"));
        assert_eq!(parse_key("\x1b[69;2u").as_deref(), Some("shift+e"));
        assert_eq!(parse_key("\x1b[99;17u"), None);
    }

    #[test]
    fn parse_key_handles_legacy_sequences() {
        let _guard = kitty_guard(false);
        assert_eq!(parse_key("\x03").as_deref(), Some("ctrl+c"));
        assert_eq!(parse_key("\x04").as_deref(), Some("ctrl+d"));
        assert_eq!(parse_key("\x1b").as_deref(), Some("escape"));
        assert_eq!(parse_key("\t").as_deref(), Some("tab"));
        assert_eq!(parse_key("\r").as_deref(), Some("enter"));
        assert_eq!(parse_key("\n").as_deref(), Some("enter"));
        assert_eq!(parse_key("\x00").as_deref(), Some("ctrl+space"));
        assert_eq!(parse_key(" ").as_deref(), Some("space"));
        assert_eq!(parse_key("1").as_deref(), Some("1"));
        assert!(matches_key("1", "1"));
        assert_eq!(parse_key("+").as_deref(), Some("+"));
        assert!(matches_key("+", "+"));
        assert_eq!(parse_key("\x1b[A").as_deref(), Some("up"));
        assert_eq!(parse_key("\x1b[B").as_deref(), Some("down"));
        assert_eq!(parse_key("\x1b[C").as_deref(), Some("right"));
        assert_eq!(parse_key("\x1b[D").as_deref(), Some("left"));
        assert_eq!(parse_key("\x1bOA").as_deref(), Some("up"));
        assert_eq!(parse_key("\x1bOB").as_deref(), Some("down"));
        assert_eq!(parse_key("\x1bOC").as_deref(), Some("right"));
        assert_eq!(parse_key("\x1bOD").as_deref(), Some("left"));
        assert_eq!(parse_key("\x1bOH").as_deref(), Some("home"));
        assert_eq!(parse_key("\x1bOF").as_deref(), Some("end"));
        assert_eq!(parse_key("\x1bOP").as_deref(), Some("f1"));
        assert_eq!(parse_key("\x1b[24~").as_deref(), Some("f12"));
        assert_eq!(parse_key("\x1b[E").as_deref(), Some("clear"));
        assert_eq!(parse_key("\x1b[2^").as_deref(), Some("ctrl+insert"));
        assert_eq!(parse_key("\x1bp").as_deref(), Some("alt+up"));
        assert_eq!(parse_key("\x1b[[5~").as_deref(), Some("pageUp"));
    }

    #[test]
    fn is_key_release_and_repeat_ignore_bracketed_paste() {
        assert!(is_key_release("\x1b[99;5:3u"));
        assert!(is_key_release("\x1b[3;5:3~"));
        assert!(!is_key_release("\x1b[200~90:62:3F:A5\x1b[201~"));
        assert!(is_key_repeat("\x1b[99;5:2u"));
        assert!(!is_key_repeat("\x1b[200~90:62:2F:A5\x1b[201~"));
        assert!(!is_key_release("\x1b[99;5u"));
    }
}
