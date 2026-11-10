//! `test/keys.test.ts` 的逐条移植（61 个 it）。
//!
//! **并发**：`setKittyProtocolActive` 是进程内的 thread-local 全局；env 变量（`WT_SESSION`、
//! `SSH_*`）是进程级全局。TS 用 `node:test` 的默认串行执行 + `withEnv` 包装；Rust 的测试
//! 默认多线程，所以本文件所有用例都先取 [`GLOBAL_GUARD`] 互斥锁，保证 kitty 状态与 env
//! 变量不互相污染（语义与 TS 的串行执行一致）。
//!
//! TS 里 `setKittyProtocolActive(false)` 是每个用例末尾的手工复位；Rust 用 RAII guard
//! （[`KittyGuard`]）在 drop 时复位，断言内容不变。

use std::sync::{Mutex, MutexGuard, OnceLock};

use kiana_tui::keys::{
    Key, decode_kitty_printable, decode_printable_key, matches_key, parse_key,
    set_kitty_protocol_active,
};

fn global_guard() -> MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|error| error.into_inner())
}

/// 设置 kitty 状态并在 drop 时复位为 `false`：进程级全局状态必须逐用例清理，
/// RAII guard 在 panic 展开时同样会复位。
struct KittyGuard {
    _lock: MutexGuard<'static, ()>,
}

fn kitty(active: bool) -> KittyGuard {
    let lock = global_guard();
    set_kitty_protocol_active(active);
    KittyGuard { _lock: lock }
}

impl Drop for KittyGuard {
    fn drop(&mut self) {
        set_kitty_protocol_active(false);
    }
}

/// `withEnv(name, value, fn)` 的 Rust 等价物：设置/删除环境变量并在结束后恢复。
struct EnvGuard {
    name: &'static str,
    previous: Option<std::ffi::OsString>,
}

impl EnvGuard {
    fn set(name: &'static str, value: Option<&str>) -> Self {
        let previous = std::env::var_os(name);
        // SAFETY: 本文件的用例都持有 `GLOBAL_GUARD`，没有其它线程读写环境变量。
        unsafe {
            match value {
                Some(value) => std::env::set_var(name, value),
                None => std::env::remove_var(name),
            }
        }
        EnvGuard { name, previous }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        // SAFETY: 同上（guard 仍持有）。
        unsafe {
            match self.previous.take() {
                Some(value) => std::env::set_var(self.name, value),
                None => std::env::remove_var(self.name),
            }
        }
    }
}

// ============================================================================
// matchesKey / Kitty protocol with alternate keys (non-Latin layouts)
// ============================================================================

/// `should match Ctrl+c when pressing Ctrl+С (Cyrillic) with base layout key`
#[test]
fn matches_ctrl_c_with_cyrillic_base_layout_key() {
    let _kitty = kitty(true);
    // Cyrillic 'с' = codepoint 1089, Latin 'c' = codepoint 99
    // CSI 1089::99;5u (codepoint::base;modifier with ctrl=4, +1=5)
    assert!(matches_key("\x1b[1089::99;5u", "ctrl+c"));
}

/// `should match Ctrl+d when pressing Ctrl+В (Cyrillic) with base layout key`
#[test]
fn matches_ctrl_d_with_cyrillic_base_layout_key() {
    let _kitty = kitty(true);
    assert!(matches_key("\x1b[1074::100;5u", "ctrl+d"));
}

/// `should match Ctrl+z when pressing Ctrl+Я (Cyrillic) with base layout key`
#[test]
fn matches_ctrl_z_with_cyrillic_base_layout_key() {
    let _kitty = kitty(true);
    assert!(matches_key("\x1b[1103::122;5u", "ctrl+z"));
}

/// `should match Ctrl+Shift+p with base layout key`
#[test]
fn matches_ctrl_shift_p_with_base_layout_key() {
    let _kitty = kitty(true);
    // ctrl=4, shift=1, +1 = 6
    assert!(matches_key("\x1b[1079::112;6u", "ctrl+shift+p"));
}

/// `should still match direct codepoint when no base layout key`
#[test]
fn matches_direct_codepoint_without_base_layout_key() {
    let _kitty = kitty(true);
    assert!(matches_key("\x1b[99;5u", "ctrl+c"));
}

/// `should match super-modified Kitty bindings, including combined modifiers`
#[test]
fn matches_super_modified_kitty_bindings_including_combined_modifiers() {
    let _kitty = kitty(true);
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

/// `should match digit bindings via Kitty CSI-u`
#[test]
fn matches_digit_bindings_via_kitty_csi_u() {
    let _kitty = kitty(true);
    assert!(matches_key("\x1b[49u", "1"));
    assert!(matches_key("\x1b[49;5u", "ctrl+1"));
    assert!(!matches_key("\x1b[49;5u", "ctrl+2"));
    assert_eq!(parse_key("\x1b[49u").as_deref(), Some("1"));
    assert_eq!(parse_key("\x1b[49;5u").as_deref(), Some("ctrl+1"));
}

/// `should normalize Kitty keypad functional keys to logical digits, symbols, and navigation`
#[test]
fn normalizes_kitty_keypad_functional_keys() {
    let _kitty = kitty(true);
    assert!(matches_key("\x1b[57400u", "1"));
    assert!(matches_key("\x1b[57410u", "/"));
    assert!(matches_key("\x1b[57417u", "left"));
    assert!(matches_key("\x1b[57426u", "delete"));
    assert_eq!(parse_key("\x1b[57399u").as_deref(), Some("0"));
    assert_eq!(parse_key("\x1b[57409u").as_deref(), Some("."));
    assert_eq!(parse_key("\x1b[57413u").as_deref(), Some("+"));
    assert_eq!(parse_key("\x1b[57416u").as_deref(), Some(","));
    assert_eq!(parse_key("\x1b[57417u").as_deref(), Some("left"));
    assert_eq!(parse_key("\x1b[57418u").as_deref(), Some("right"));
    assert_eq!(parse_key("\x1b[57419u").as_deref(), Some("up"));
    assert_eq!(parse_key("\x1b[57420u").as_deref(), Some("down"));
    assert_eq!(parse_key("\x1b[57421u").as_deref(), Some("pageUp"));
    assert_eq!(parse_key("\x1b[57422u").as_deref(), Some("pageDown"));
    assert_eq!(parse_key("\x1b[57423u").as_deref(), Some("home"));
    assert_eq!(parse_key("\x1b[57424u").as_deref(), Some("end"));
    assert_eq!(parse_key("\x1b[57425u").as_deref(), Some("insert"));
    assert_eq!(parse_key("\x1b[57426u").as_deref(), Some("delete"));
}

/// `should match shifted symbols by their produced character`
#[test]
fn matches_shifted_symbols_by_their_produced_character() {
    let _kitty = kitty(true);
    assert!(matches_key("\x1b[61:43;2u", "+"));
    assert!(matches_key("\x1b[61:43;2u", "shift+="));
    assert_eq!(parse_key("\x1b[61:43;2u").as_deref(), Some("+"));
    assert!(matches_key("\x1b[61:43;6u", "ctrl++"));
    assert_eq!(parse_key("\x1b[61:43;6u").as_deref(), Some("ctrl++"));
}

/// `should handle shifted key in format`
#[test]
fn handles_shifted_key_in_format() {
    let _kitty = kitty(true);
    // CSI codepoint:shifted:base;modifier u —— shift modifier = 1, +1 = 2
    assert!(matches_key("\x1b[99:67:99;2u", "shift+c"));
}

/// `should handle event type in format`
#[test]
fn handles_event_type_in_format() {
    let _kitty = kitty(true);
    // CSI codepoint::base;modifier:event u —— release event (3)
    assert!(matches_key("\x1b[1089::99;5:3u", "ctrl+c"));
}

/// `should handle full format with shifted key, base key, and event type`
#[test]
fn handles_full_format_with_shifted_base_and_event_type() {
    let _kitty = kitty(true);
    // CSI codepoint:shifted:base;modifier:event u —— ctrl=4, shift=1, +1 = 6; repeat = 2
    assert!(matches_key("\x1b[1089:1057:99;6:2u", "ctrl+shift+c"));
}

/// `should prefer codepoint for Latin letters even when base layout differs`
#[test]
fn prefers_codepoint_for_latin_letters_when_base_layout_differs() {
    let _kitty = kitty(true);
    assert!(matches_key("\x1b[107::118;5u", "ctrl+k"));
    assert!(!matches_key("\x1b[107::118;5u", "ctrl+v"));
}

/// `should prefer codepoint for symbol keys even when base layout differs`
#[test]
fn prefers_codepoint_for_symbol_keys_when_base_layout_differs() {
    let _kitty = kitty(true);
    assert!(matches_key("\x1b[47::91;5u", "ctrl+/"));
    assert!(!matches_key("\x1b[47::91;5u", "ctrl+["));
}

/// `should not match wrong key even with base layout`
#[test]
fn does_not_match_wrong_key_even_with_base_layout() {
    let _kitty = kitty(true);
    assert!(!matches_key("\x1b[1089::99;5u", "ctrl+d"));
}

/// `should not match wrong modifiers even with base layout`
#[test]
fn does_not_match_wrong_modifiers_even_with_base_layout() {
    let _kitty = kitty(true);
    assert!(!matches_key("\x1b[1089::99;5u", "ctrl+shift+c"));
}

// ============================================================================
// modifyOtherKeys matching
// ============================================================================

/// `should match xterm modifyOtherKeys Ctrl+c`
#[test]
fn matches_xterm_modify_other_keys_ctrl_c() {
    let _kitty = kitty(false);
    assert!(matches_key("\x1b[27;5;99~", "ctrl+c"));
    assert_eq!(parse_key("\x1b[27;5;99~").as_deref(), Some("ctrl+c"));
}

/// `should match xterm modifyOtherKeys Ctrl+d`
#[test]
fn matches_xterm_modify_other_keys_ctrl_d() {
    let _kitty = kitty(false);
    assert!(matches_key("\x1b[27;5;100~", "ctrl+d"));
    assert_eq!(parse_key("\x1b[27;5;100~").as_deref(), Some("ctrl+d"));
}

/// `should match xterm modifyOtherKeys Ctrl+z`
#[test]
fn matches_xterm_modify_other_keys_ctrl_z() {
    let _kitty = kitty(false);
    assert!(matches_key("\x1b[27;5;122~", "ctrl+z"));
    assert_eq!(parse_key("\x1b[27;5;122~").as_deref(), Some("ctrl+z"));
}

/// `should match xterm modifyOtherKeys Enter variants`
#[test]
fn matches_xterm_modify_other_keys_enter_variants() {
    let _kitty = kitty(false);
    assert!(matches_key("\x1b[27;5;13~", "ctrl+enter"));
    assert!(matches_key("\x1b[27;2;13~", "shift+enter"));
    assert!(matches_key("\x1b[27;3;13~", "alt+enter"));
    assert_eq!(parse_key("\x1b[27;5;13~").as_deref(), Some("ctrl+enter"));
    assert_eq!(parse_key("\x1b[27;2;13~").as_deref(), Some("shift+enter"));
    assert_eq!(parse_key("\x1b[27;3;13~").as_deref(), Some("alt+enter"));
}

/// `should match xterm modifyOtherKeys Tab variants`
#[test]
fn matches_xterm_modify_other_keys_tab_variants() {
    let _kitty = kitty(false);
    assert!(matches_key("\x1b[27;2;9~", "shift+tab"));
    assert!(matches_key("\x1b[27;5;9~", "ctrl+tab"));
    assert!(matches_key("\x1b[27;3;9~", "alt+tab"));
    assert_eq!(parse_key("\x1b[27;2;9~").as_deref(), Some("shift+tab"));
    assert_eq!(parse_key("\x1b[27;5;9~").as_deref(), Some("ctrl+tab"));
    assert_eq!(parse_key("\x1b[27;3;9~").as_deref(), Some("alt+tab"));
}

/// `should match xterm modifyOtherKeys Backspace variants`
#[test]
fn matches_xterm_modify_other_keys_backspace_variants() {
    let _kitty = kitty(false);
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

/// `should match xterm modifyOtherKeys Escape`
#[test]
fn matches_xterm_modify_other_keys_escape() {
    let _kitty = kitty(false);
    assert!(matches_key("\x1b[27;1;27~", "escape"));
    assert_eq!(parse_key("\x1b[27;1;27~").as_deref(), Some("escape"));
}

/// `should match xterm modifyOtherKeys Space variants`
#[test]
fn matches_xterm_modify_other_keys_space_variants() {
    let _kitty = kitty(false);
    assert!(matches_key("\x1b[27;1;32~", "space"));
    assert!(matches_key("\x1b[27;5;32~", "ctrl+space"));
    assert_eq!(parse_key("\x1b[27;1;32~").as_deref(), Some("space"));
    assert_eq!(parse_key("\x1b[27;5;32~").as_deref(), Some("ctrl+space"));
}

/// `should match xterm modifyOtherKeys symbol combos`
#[test]
fn matches_xterm_modify_other_keys_symbol_combos() {
    let _kitty = kitty(false);
    assert!(matches_key("\x1b[27;5;47~", "ctrl+/"));
    assert_eq!(parse_key("\x1b[27;5;47~").as_deref(), Some("ctrl+/"));
    assert!(matches_key("\x1b[27;6;43~", "ctrl++"));
    assert_eq!(parse_key("\x1b[27;6;43~").as_deref(), Some("ctrl++"));
}

/// `should match xterm modifyOtherKeys digit combos`
#[test]
fn matches_xterm_modify_other_keys_digit_combos() {
    let _kitty = kitty(false);
    assert!(matches_key("\x1b[27;5;49~", "ctrl+1"));
    assert!(matches_key("\x1b[27;2;49~", "shift+1"));
    assert_eq!(parse_key("\x1b[27;5;49~").as_deref(), Some("ctrl+1"));
    assert_eq!(parse_key("\x1b[27;2;49~").as_deref(), Some("shift+1"));
}

/// `should match xterm modifyOtherKeys shifted uppercase letters`
#[test]
fn matches_xterm_modify_other_keys_shifted_uppercase_letters() {
    let _kitty = kitty(false);
    assert!(matches_key("\x1b[27;2;69~", "shift+e"));
    assert!(matches_key("\x1b[27;6;69~", "ctrl+shift+e"));
    assert_eq!(parse_key("\x1b[27;2;69~").as_deref(), Some("shift+e"));
    assert_eq!(parse_key("\x1b[27;6;69~").as_deref(), Some("shift+ctrl+e"));
}

/// `should match Ctrl+Alt+letter via CSI-u when kitty inactive`
#[test]
fn matches_ctrl_alt_letter_via_csi_u_when_kitty_inactive() {
    let _kitty = kitty(false);
    assert!(matches_key("\x1b[104;7u", "ctrl+alt+h"));
    assert_eq!(parse_key("\x1b[104;7u").as_deref(), Some("ctrl+alt+h"));
}

/// `should match Ctrl+Alt+letter via xterm modifyOtherKeys`
#[test]
fn matches_ctrl_alt_letter_via_xterm_modify_other_keys() {
    let _kitty = kitty(false);
    assert!(matches_key("\x1b[27;7;104~", "ctrl+alt+h"));
    assert_eq!(parse_key("\x1b[27;7;104~").as_deref(), Some("ctrl+alt+h"));
}

// ============================================================================
// Legacy key matching
// ============================================================================

/// `should match legacy Ctrl+c`
#[test]
fn matches_legacy_ctrl_c() {
    let _kitty = kitty(false);
    assert!(matches_key("\x03", "ctrl+c"));
}

/// `should match legacy Ctrl+d`
#[test]
fn matches_legacy_ctrl_d() {
    let _kitty = kitty(false);
    assert!(matches_key("\x04", "ctrl+d"));
}

/// `should match escape key`
#[test]
fn matches_escape_key() {
    let _guard = global_guard();
    assert!(matches_key("\x1b", "escape"));
}

/// `should match legacy linefeed as enter`
#[test]
fn matches_legacy_linefeed_as_enter() {
    let _kitty = kitty(false);
    assert!(matches_key("\n", "enter"));
    assert_eq!(parse_key("\n").as_deref(), Some("enter"));
}

/// `should treat linefeed as shift+enter when kitty active`
#[test]
fn treats_linefeed_as_shift_enter_when_kitty_active() {
    let _kitty = kitty(true);
    assert!(matches_key("\n", "shift+enter"));
    assert!(!matches_key("\n", "enter"));
    assert_eq!(parse_key("\n").as_deref(), Some("shift+enter"));
}

/// `should parse ctrl+space`
#[test]
fn parses_ctrl_space() {
    let _kitty = kitty(false);
    assert!(matches_key("\x00", "ctrl+space"));
    assert_eq!(parse_key("\x00").as_deref(), Some("ctrl+space"));
}

/// `should match legacy Ctrl+symbol`
#[test]
fn matches_legacy_ctrl_symbol() {
    let _kitty = kitty(false);
    // Ctrl+\ = 28, Ctrl+] = 29, Ctrl+_ = 31（与 Ctrl+- 同一物理键）。
    assert!(matches_key("\x1c", "ctrl+\\"));
    assert_eq!(parse_key("\x1c").as_deref(), Some("ctrl+\\"));
    assert!(matches_key("\x1d", "ctrl+]"));
    assert_eq!(parse_key("\x1d").as_deref(), Some("ctrl+]"));
    assert!(matches_key("\x1f", "ctrl+_"));
    assert!(matches_key("\x1f", "ctrl+-"));
    assert_eq!(parse_key("\x1f").as_deref(), Some("ctrl+-"));
}

/// `should match legacy Ctrl+Alt+symbol`
#[test]
fn matches_legacy_ctrl_alt_symbol() {
    let _kitty = kitty(false);
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

/// `should treat raw 0x08 as plain backspace outside Windows Terminal`
#[test]
fn treats_raw_0x08_as_plain_backspace_outside_windows_terminal() {
    let _kitty = kitty(false);
    let _env = EnvGuard::set("WT_SESSION", None);
    assert!(matches_key("\x7f", "backspace"));
    assert!(!matches_key("\x7f", "ctrl+backspace"));
    assert_eq!(parse_key("\x7f").as_deref(), Some("backspace"));
    assert!(matches_key("\x08", "backspace"));
    assert!(!matches_key("\x08", "ctrl+backspace"));
    assert_eq!(parse_key("\x08").as_deref(), Some("backspace"));
    assert!(matches_key("\x08", "ctrl+h"));
}

/// `should treat raw 0x08 as ctrl+backspace in local Windows Terminal`
#[test]
fn treats_raw_0x08_as_ctrl_backspace_in_local_windows_terminal() {
    let _kitty = kitty(false);
    let _wt = EnvGuard::set("WT_SESSION", Some("test-session"));
    let _connection = EnvGuard::set("SSH_CONNECTION", None);
    let _client = EnvGuard::set("SSH_CLIENT", None);
    let _tty = EnvGuard::set("SSH_TTY", None);
    assert!(matches_key("\x08", "ctrl+backspace"));
    assert!(!matches_key("\x08", "backspace"));
    assert_eq!(parse_key("\x08").as_deref(), Some("ctrl+backspace"));
    assert!(matches_key("\x08", "ctrl+h"));
}

/// `should treat raw 0x08 as plain backspace in Windows Terminal over SSH`
#[test]
fn treats_raw_0x08_as_plain_backspace_in_windows_terminal_over_ssh() {
    let _kitty = kitty(false);
    let _wt = EnvGuard::set("WT_SESSION", Some("test-session"));
    let _connection = EnvGuard::set("SSH_CONNECTION", Some("1 2 3 4"));
    let _client = EnvGuard::set("SSH_CLIENT", Some("1 2 3"));
    let _tty = EnvGuard::set("SSH_TTY", Some("/dev/pts/1"));
    assert!(!matches_key("\x08", "ctrl+backspace"));
    assert!(matches_key("\x08", "backspace"));
    assert_eq!(parse_key("\x08").as_deref(), Some("backspace"));
    assert!(matches_key("\x08", "ctrl+h"));
}

/// `should parse legacy alt-prefixed sequences when kitty inactive`
#[test]
fn parses_legacy_alt_prefixed_sequences() {
    let _kitty = kitty(false);
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
    assert_eq!(parse_key("\x1b "), None);
    assert!(matches_key("\x1b\u{8}", "alt+backspace"));
    assert_eq!(parse_key("\x1b\u{8}").as_deref(), Some("alt+backspace"));
    assert!(!matches_key("\x1b\x03", "ctrl+alt+c"));
    assert_eq!(parse_key("\x1b\x03"), None);
    assert!(!matches_key("\x1bB", "alt+left"));
    assert_eq!(parse_key("\x1bB"), None);
    assert!(!matches_key("\x1bF", "alt+right"));
    assert_eq!(parse_key("\x1bF"), None);
    assert!(!matches_key("\x1ba", "alt+a"));
    assert_eq!(parse_key("\x1ba"), None);
    assert!(!matches_key("\x1b1", "alt+1"));
    assert_eq!(parse_key("\x1b1"), None);
    assert!(!matches_key("\x1b,", "alt+,"));
    assert_eq!(parse_key("\x1b,"), None);
    assert!(!matches_key("\x1b.", "alt+."));
    assert_eq!(parse_key("\x1b."), None);
    assert!(!matches_key("\x1by", "alt+y"));
    assert_eq!(parse_key("\x1by"), None);
}

/// `should match arrow keys`
#[test]
fn matches_arrow_keys() {
    let _guard = global_guard();
    assert!(matches_key("\x1b[A", "up"));
    assert!(matches_key("\x1b[B", "down"));
    assert!(matches_key("\x1b[C", "right"));
    assert!(matches_key("\x1b[D", "left"));
}

/// `should match SS3 arrows and home/end`
#[test]
fn matches_ss3_arrows_and_home_end() {
    let _guard = global_guard();
    assert!(matches_key("\x1bOA", "up"));
    assert!(matches_key("\x1bOB", "down"));
    assert!(matches_key("\x1bOC", "right"));
    assert!(matches_key("\x1bOD", "left"));
    assert!(matches_key("\x1bOH", "home"));
    assert!(matches_key("\x1bOF", "end"));
}

/// `should match xterm Ctrl-modified viewport navigation`
#[test]
fn matches_xterm_ctrl_modified_viewport_navigation() {
    let _guard = global_guard();
    assert!(matches_key("\x1b[1;5H", "ctrl+home"));
    assert!(matches_key("\x1b[1;5F", "ctrl+end"));
    assert!(matches_key("\x1b[5;5~", "ctrl+pageUp"));
    assert!(matches_key("\x1b[6;5~", "ctrl+pageDown"));
    assert_eq!(parse_key("\x1b[1;5H").as_deref(), Some("ctrl+home"));
    assert_eq!(parse_key("\x1b[1;5F").as_deref(), Some("ctrl+end"));
    assert_eq!(parse_key("\x1b[5;5~").as_deref(), Some("ctrl+pageUp"));
    assert_eq!(parse_key("\x1b[6;5~").as_deref(), Some("ctrl+pageDown"));
}

/// `should match legacy function keys and clear`
#[test]
fn matches_legacy_function_keys_and_clear() {
    let _guard = global_guard();
    assert!(matches_key("\x1bOP", "f1"));
    assert!(matches_key("\x1b[24~", "f12"));
    assert!(matches_key("\x1b[E", "clear"));
}

/// `should match alt+arrows`
#[test]
fn matches_alt_arrows() {
    let _guard = global_guard();
    assert!(matches_key("\x1bp", "alt+up"));
    assert!(!matches_key("\x1bp", "up"));
}

/// `should match rxvt modifier sequences`
#[test]
fn matches_rxvt_modifier_sequences() {
    let _guard = global_guard();
    assert!(matches_key("\x1b[a", "shift+up"));
    assert!(matches_key("\x1bOa", "ctrl+up"));
    assert!(matches_key("\x1b[2$", "shift+insert"));
    assert!(matches_key("\x1b[2^", "ctrl+insert"));
    assert!(matches_key("\x1b[7$", "shift+home"));
}

// ============================================================================
// decodeKittyPrintable
// ============================================================================

/// `should decode Kitty keypad functional keys to printable characters`
#[test]
fn decodes_kitty_keypad_functional_keys_to_printable_characters() {
    let _guard = global_guard();
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

// ============================================================================
// decodePrintableKey
// ============================================================================

/// `should decode printable xterm modifyOtherKeys sequences`
#[test]
fn decodes_printable_xterm_modify_other_keys_sequences() {
    let _guard = global_guard();
    assert_eq!(decode_printable_key("\x1b[27;2;69~").as_deref(), Some("E"));
    assert_eq!(decode_printable_key("\x1b[27;2;196~").as_deref(), Some("Ä"));
    assert_eq!(decode_printable_key("\x1b[27;2;32~").as_deref(), Some(" "));
    assert_eq!(decode_printable_key("\x1b[27;2;13~"), None);
    assert_eq!(decode_printable_key("\x1b[27;6;69~"), None);
}

// ============================================================================
// parseKey —— Kitty protocol with alternate keys
// ============================================================================

/// `should return Latin key name when base layout key is present`
#[test]
fn parse_key_returns_latin_name_when_base_layout_present() {
    let _kitty = kitty(true);
    assert_eq!(parse_key("\x1b[1089::99;5u").as_deref(), Some("ctrl+c"));
}

/// `should prefer codepoint for Latin letters when base layout differs`
#[test]
fn parse_key_prefers_codepoint_for_latin_letters() {
    let _kitty = kitty(true);
    assert_eq!(parse_key("\x1b[107::118;5u").as_deref(), Some("ctrl+k"));
}

/// `should prefer codepoint for symbol keys when base layout differs`
#[test]
fn parse_key_prefers_codepoint_for_symbol_keys() {
    let _kitty = kitty(true);
    assert_eq!(parse_key("\x1b[47::91;5u").as_deref(), Some("ctrl+/"));
}

/// `should return key name from codepoint when no base layout`
#[test]
fn parse_key_returns_name_from_codepoint_without_base_layout() {
    let _kitty = kitty(true);
    assert_eq!(parse_key("\x1b[99;5u").as_deref(), Some("ctrl+c"));
}

/// `should parse shifted uppercase CSI-u letters as shift+letter`
#[test]
fn parse_key_parses_shifted_uppercase_csi_u_letters() {
    let _kitty = kitty(true);
    assert!(matches_key("\x1b[69;2u", "shift+e"));
    assert_eq!(parse_key("\x1b[69;2u").as_deref(), Some("shift+e"));
}

/// `should ignore Kitty CSI-u with unsupported modifiers`
#[test]
fn parse_key_ignores_kitty_csi_u_with_unsupported_modifiers() {
    let _kitty = kitty(true);
    assert_eq!(parse_key("\x1b[99;17u"), None);
}

// ============================================================================
// parseKey —— Legacy key parsing
// ============================================================================

/// `should parse legacy Ctrl+letter`
#[test]
fn parse_key_parses_legacy_ctrl_letter() {
    let _kitty = kitty(false);
    assert_eq!(parse_key("\x03").as_deref(), Some("ctrl+c"));
    assert_eq!(parse_key("\x04").as_deref(), Some("ctrl+d"));
}

/// `should parse special keys`
#[test]
fn parse_key_parses_special_keys() {
    let _guard = global_guard();
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
}

/// `should parse arrow keys`
#[test]
fn parse_key_parses_arrow_keys() {
    let _guard = global_guard();
    assert_eq!(parse_key("\x1b[A").as_deref(), Some("up"));
    assert_eq!(parse_key("\x1b[B").as_deref(), Some("down"));
    assert_eq!(parse_key("\x1b[C").as_deref(), Some("right"));
    assert_eq!(parse_key("\x1b[D").as_deref(), Some("left"));
}

/// `should parse SS3 arrows and home/end`
#[test]
fn parse_key_parses_ss3_arrows_and_home_end() {
    let _guard = global_guard();
    assert_eq!(parse_key("\x1bOA").as_deref(), Some("up"));
    assert_eq!(parse_key("\x1bOB").as_deref(), Some("down"));
    assert_eq!(parse_key("\x1bOC").as_deref(), Some("right"));
    assert_eq!(parse_key("\x1bOD").as_deref(), Some("left"));
    assert_eq!(parse_key("\x1bOH").as_deref(), Some("home"));
    assert_eq!(parse_key("\x1bOF").as_deref(), Some("end"));
}

/// `should parse legacy function and modifier sequences`
#[test]
fn parse_key_parses_legacy_function_and_modifier_sequences() {
    let _guard = global_guard();
    assert_eq!(parse_key("\x1bOP").as_deref(), Some("f1"));
    assert_eq!(parse_key("\x1b[24~").as_deref(), Some("f12"));
    assert_eq!(parse_key("\x1b[E").as_deref(), Some("clear"));
    assert_eq!(parse_key("\x1b[2^").as_deref(), Some("ctrl+insert"));
    assert_eq!(parse_key("\x1bp").as_deref(), Some("alt+up"));
}

/// `should parse double bracket pageUp`
#[test]
fn parse_key_parses_double_bracket_page_up() {
    let _guard = global_guard();
    assert_eq!(parse_key("\x1b[[5~").as_deref(), Some("pageUp"));
}

// ============================================================================
// 回归：功能键的事件类型可以不带修饰符（`keys.ts` 的 funcMatch 正则）
// ============================================================================

/// `keys.ts:634-653`：`^\x1b\[(\d+)(?:;(\d+))?(?::(\d+))?~$` —— `:<event>` 直接跟在
/// 键号后面（`\x1b[2:3~` = insert release）。此前 Rust 的 `match_functional` 先按 `;`
/// 切分，把 `2:3` 当成键号而整体拒绝。
#[test]
fn parses_functional_key_event_type_without_modifier() {
    let _kitty = kitty(true);
    // TS 探针：parseKey("\x1b[2:3~") === "insert"、isKeyRelease === true。
    assert_eq!(parse_key("\x1b[2:3~").as_deref(), Some("insert"));
    assert!(matches_key("\x1b[2:3~", "insert"));
    assert!(kiana_tui::keys::is_key_release("\x1b[2:3~"));
    assert_eq!(parse_key("\x1b[2:1~").as_deref(), Some("insert"));
    assert_eq!(parse_key("\x1b[2:2~").as_deref(), Some("insert"));
    assert!(kiana_tui::keys::is_key_repeat("\x1b[2:2~"));
    assert_eq!(parse_key("\x1b[3:3~").as_deref(), Some("delete"));
    assert_eq!(parse_key("\x1b[5:3~").as_deref(), Some("pageUp"));

    // 带修饰符的既有形式不受影响。
    assert_eq!(parse_key("\x1b[2;5~").as_deref(), Some("ctrl+insert"));
    assert_eq!(parse_key("\x1b[2;5:3~").as_deref(), Some("ctrl+insert"));
    assert!(kiana_tui::keys::is_key_release("\x1b[2;5:3~"));

    // 正则不接受的形式仍然拒绝。
    assert_eq!(parse_key("\x1b[2:3:4~"), None);
    assert_eq!(parse_key("\x1b[2;:3~"), None);
    assert_eq!(parse_key("\x1b[2:~"), None);
}

// ============================================================================
// 回归：代理区码点（JS `String.fromCodePoint` 接受，Rust `char` 不能表示）
// ============================================================================

/// TS 的 `String.fromCodePoint(0xD800)` 返回一个孤立代理码元；Node 以 UTF-8 输出该码元
/// 时得到 EF BF BD（U+FFFD）。Rust 侧用 U+FFFD 表示，且只拒绝 <32 与 >0x10FFFF。
#[test]
fn decodes_lone_surrogate_code_points_as_replacement_characters() {
    let _guard = global_guard();
    assert_eq!(
        decode_kitty_printable("\x1b[55296u").as_deref(),
        Some("\u{FFFD}")
    );
    assert_eq!(
        decode_kitty_printable("\x1b[56320u").as_deref(),
        Some("\u{FFFD}")
    );
    assert_eq!(
        decode_printable_key("\x1b[55296u").as_deref(),
        Some("\u{FFFD}")
    );
    // xterm modifyOtherKeys 路径同样接受代理区码点。
    assert_eq!(
        decode_printable_key("\x1b[27;1;55296~").as_deref(),
        Some("\u{FFFD}")
    );

    // `String.fromCodePoint(0x110000)` 抛 RangeError → undefined。
    assert_eq!(decode_kitty_printable("\x1b[1114112u"), None);
    assert_eq!(decode_printable_key("\x1b[27;1;1114112~"), None);
    // 正常码点与边界值不受影响。
    assert_eq!(decode_kitty_printable("\x1b[97u").as_deref(), Some("a"));
    assert_eq!(
        decode_kitty_printable("\x1b[1114111u").as_deref(),
        Some("\u{10FFFF}")
    );
}
