//! `test/terminal.test.ts` 的移植（27 个 it）。
//!
//! TS 通过改写 `process.stdout.write` / `process.stdin.on` 建协商夹具；Rust 侧用
//! [`ProcessTerminal::with_output`]（捕获输出）与 [`ProcessTerminal::feed_input_str`]
//! （注入 stdin 数据）复现同一套行为。`KIANA_PROGRAM_STATUS` 是进程级环境变量，所有用例在
//! [`ENV_LOCK`] 下串行执行。

use std::io::Write;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use kiana_tui::keys::set_kitty_protocol_active;
use kiana_tui::terminal::{
    KeyboardProtocolNegotiationSequence, ProcessTerminal, node_set_timeout_ms,
    normalize_apple_terminal_input, normalize_native_shift_enter_input,
    parse_keyboard_protocol_negotiation_sequence, resolve_escape_timeout_ms,
    resolve_escape_timeout_ms_with,
};
use kiana_tui::terminal_colors::{ProgramStatus, ProgramStatusState, Terminal};

/// 环境变量是进程级的：同一测试二进制里的用例必须串行（TS 里 `process.env` 同样共享，
// 只是 node:test 顺序执行）。
static ENV_LOCK: Mutex<()> = Mutex::new(());

fn env_lock() -> MutexGuard<'static, ()> {
    ENV_LOCK.lock().unwrap_or_else(|poison| poison.into_inner())
}

/// 捕获 `write` 的 stdout 替身。
struct CaptureWriter(Arc<Mutex<Vec<String>>>);

impl Write for CaptureWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .push(String::from_utf8_lossy(buf).into_owned());
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// 协商夹具（TS 的 `setupNegotiation`）。
struct Harness {
    terminal: ProcessTerminal,
    writes: Arc<Mutex<Vec<String>>>,
    input: Arc<Mutex<Option<String>>>,
    cleaned: bool,
}

impl Harness {
    fn setup() -> Self {
        let writes = Arc::new(Mutex::new(Vec::new()));
        let mut terminal =
            ProcessTerminal::with_output(Box::new(CaptureWriter(Arc::clone(&writes))));
        let input = Arc::new(Mutex::new(None));
        let slot = Arc::clone(&input);
        terminal.set_input_handler(Box::new(move |data: &str| {
            *slot.lock().unwrap_or_else(|poison| poison.into_inner()) = Some(data.to_string());
        }));
        terminal.query_and_enable_kitty_protocol();
        Harness {
            terminal,
            writes,
            input,
            cleaned: false,
        }
    }

    fn send(&mut self, data: &str) {
        self.terminal.feed_input_str(data);
    }

    fn poll(&mut self, now: Instant) {
        self.terminal.poll(now);
    }

    fn input(&self) -> Option<String> {
        self.input
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .clone()
    }

    fn writes(&self) -> Vec<String> {
        self.writes
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .clone()
    }

    fn write_count(&self, sequence: &str) -> usize {
        self.writes()
            .iter()
            .filter(|write| *write == sequence)
            .count()
    }

    fn last_write(&self) -> Option<String> {
        self.writes().last().cloned()
    }

    fn cleanup(&mut self) {
        if self.cleaned {
            return;
        }
        self.cleaned = true;
        self.terminal.stop();
        set_kitty_protocol_active(false);
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        self.cleanup();
    }
}

// ---------------------------------------------------------------------------
// resolveEscapeTimeoutMs
// ---------------------------------------------------------------------------

fn resolve_from(pairs: &[(&str, &str)]) -> f64 {
    let pairs: Vec<(String, String)> = pairs
        .iter()
        .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
        .collect();
    resolve_escape_timeout_ms_with(&move |name: &str| {
        pairs
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.clone())
    })
}

#[test]
fn uses_kiana_tui_esc_timeout_when_configured() {
    assert_eq!(resolve_from(&[("KIANA_TUI_ESC_TIMEOUT", "80")]), 80.0);
    assert_eq!(
        resolve_from(&[("KIANA_TUI_ESC_TIMEOUT", "80"), ("SSH_TTY", "/dev/pts/1")]),
        80.0
    );
}

#[test]
fn ignores_invalid_kiana_tui_esc_timeout_values() {
    assert_eq!(resolve_from(&[("KIANA_TUI_ESC_TIMEOUT", "abc")]), 10.0);
    assert_eq!(resolve_from(&[("KIANA_TUI_ESC_TIMEOUT", "0")]), 10.0);
    assert_eq!(resolve_from(&[("KIANA_TUI_ESC_TIMEOUT", "-5")]), 10.0);
    assert_eq!(resolve_from(&[("KIANA_TUI_ESC_TIMEOUT", "")]), 10.0);
}

#[test]
fn defaults_to_100ms_over_ssh() {
    assert_eq!(resolve_from(&[("SSH_CONNECTION", "10.0.0.1 22")]), 100.0);
    assert_eq!(resolve_from(&[("SSH_TTY", "/dev/pts/1")]), 100.0);
    // JS 的真值判断：非空字符串（包括 "0"）为真，空字符串为假。
    assert_eq!(resolve_from(&[("SSH_CONNECTION", "0")]), 100.0);
}

/// TS 用 `if (env.SSH_CONNECTION || env.SSH_TTY)` 的真值判断：空字符串等于未设置。
#[test]
fn treats_an_empty_ssh_environment_variable_as_unset() {
    assert_eq!(resolve_from(&[("SSH_CONNECTION", "")]), 10.0);
    assert_eq!(resolve_from(&[("SSH_TTY", "")]), 10.0);
    assert_eq!(
        resolve_from(&[("SSH_CONNECTION", ""), ("SSH_TTY", "")]),
        10.0
    );
    assert_eq!(
        resolve_from(&[("SSH_CONNECTION", ""), ("SSH_TTY", "/dev/pts/1")]),
        100.0
    );
}

/// `resolveEscapeTimeoutMs` 返回 TS 的原始数值：小数与超过 `setTimeout` 上限的值都原样返回，
/// 由调用方（Node `setTimeout`）做截断/钳制。
#[test]
fn returns_the_configured_number_without_truncating_or_clamping() {
    assert_eq!(resolve_from(&[("KIANA_TUI_ESC_TIMEOUT", "0.5")]), 0.5);
    assert_eq!(resolve_from(&[("KIANA_TUI_ESC_TIMEOUT", "80.5")]), 80.5);
    assert_eq!(
        resolve_from(&[("KIANA_TUI_ESC_TIMEOUT", "3000000000")]),
        3_000_000_000.0
    );
    // Node 的 `setTimeout` 转换：小数截断，超过 2^31-1 或小于 1 用 1ms。
    assert_eq!(node_set_timeout_ms(0.5), 1);
    assert_eq!(node_set_timeout_ms(1.5), 1);
    assert_eq!(node_set_timeout_ms(80.5), 80);
    assert_eq!(node_set_timeout_ms(3_000_000_000.0), 1);
    assert_eq!(node_set_timeout_ms(f64::INFINITY), 1);
}

#[test]
fn defaults_to_10ms_otherwise() {
    assert_eq!(resolve_from(&[]), 10.0);
}

/// `resolveEscapeTimeoutMs()` 无参版本读真实环境（默认 10ms / SSH 100ms）。
#[test]
fn resolve_escape_timeout_reads_the_process_environment() {
    let _lock = env_lock();
    let configured = std::env::var_os("KIANA_TUI_ESC_TIMEOUT")
        .map(|value| kiana_json::parse_js_number(&value.to_string_lossy()));
    let ssh = std::env::var_os("SSH_CONNECTION").is_some_and(|value| !value.is_empty())
        || std::env::var_os("SSH_TTY").is_some_and(|value| !value.is_empty());
    let expected = match configured {
        Some(value) if value.is_finite() && value > 0.0 => value,
        _ if ssh => 100.0,
        _ => 10.0,
    };
    assert_eq!(resolve_escape_timeout_ms(), expected);
}

// ---------------------------------------------------------------------------
// normalizeNativeShiftEnterInput
// ---------------------------------------------------------------------------

#[test]
fn rewrites_return_to_csi_u_shift_enter_when_native_shift_detection_is_enabled_and_shift_is_pressed()
 {
    assert_eq!(
        normalize_native_shift_enter_input("\r", true, true),
        "\x1b[13;2u"
    );
}

#[test]
fn leaves_return_unchanged_when_native_shift_detection_is_disabled() {
    assert_eq!(normalize_native_shift_enter_input("\r", false, true), "\r");
}

#[test]
fn leaves_return_unchanged_when_shift_is_not_pressed() {
    assert_eq!(normalize_native_shift_enter_input("\r", true, false), "\r");
}

#[test]
fn leaves_non_return_input_unchanged() {
    assert_eq!(
        normalize_native_shift_enter_input("\x1b[13;2u", true, true),
        "\x1b[13;2u"
    );
    assert_eq!(normalize_native_shift_enter_input("a", true, true), "a");
}

// ---------------------------------------------------------------------------
// normalizeAppleTerminalInput
// ---------------------------------------------------------------------------

#[test]
fn rewrites_apple_terminal_return_to_csi_u_shift_enter_when_shift_is_pressed() {
    assert_eq!(
        normalize_apple_terminal_input("\r", true, true),
        "\x1b[13;2u"
    );
}

#[test]
fn leaves_apple_terminal_return_unchanged_when_shift_is_not_pressed() {
    assert_eq!(normalize_apple_terminal_input("\r", true, false), "\r");
}

#[test]
fn leaves_non_apple_terminal_return_unchanged_when_shift_is_pressed() {
    assert_eq!(normalize_apple_terminal_input("\r", false, true), "\r");
}

#[test]
fn leaves_non_return_input_unchanged_in_apple_terminal_normalization() {
    assert_eq!(
        normalize_apple_terminal_input("\x1b[13;2u", true, true),
        "\x1b[13;2u"
    );
    assert_eq!(normalize_apple_terminal_input("a", true, true), "a");
}

// ---------------------------------------------------------------------------
// ProcessTerminal Kitty keyboard protocol negotiation
// ---------------------------------------------------------------------------

#[test]
fn queries_kitty_mode_before_enabling_modify_other_keys_fallback() {
    let _lock = env_lock();
    remove_env("KIANA_PROGRAM_STATUS");
    let harness = Harness::setup();
    assert_eq!(
        harness.writes()[0],
        "\x1b[>7u\x1b[?u\x1b]7501;?\x1b\\\x1b[c"
    );
    assert!(!harness.writes().iter().any(|write| write == "\x1b[>4;2m"));
    assert!(!harness.terminal.kitty_protocol_active());
}

#[test]
fn activates_kitty_mode_for_non_zero_negotiated_flags() {
    let _lock = env_lock();
    remove_env("KIANA_PROGRAM_STATUS");
    let mut harness = Harness::setup();
    harness.send("\x1b[?7u");

    assert_eq!(harness.input(), None);
    assert!(harness.terminal.kitty_protocol_active());
    assert!(!harness.writes().iter().any(|write| write == "\x1b[>4;2m"));
    assert!(!harness.writes().iter().any(|write| write == "\x1b[>4;0m"));

    harness.cleanup();
    assert_eq!(harness.write_count("\x1b[<u"), 1);
    assert!(!harness.writes().iter().any(|write| write == "\x1b[>4;0m"));
}

#[test]
fn falls_back_to_modify_other_keys_for_zero_kitty_flags() {
    let _lock = env_lock();
    remove_env("KIANA_PROGRAM_STATUS");
    let mut harness = Harness::setup();
    harness.send("\x1b[?0u");

    assert_eq!(harness.input(), None);
    assert!(!harness.terminal.kitty_protocol_active());
    assert_eq!(harness.write_count("\x1b[>4;2m"), 1);

    harness.cleanup();
    assert_eq!(harness.write_count("\x1b[>4;0m"), 1);
}

#[test]
fn falls_back_to_modify_other_keys_for_device_attributes_without_kitty_flags() {
    let _lock = env_lock();
    remove_env("KIANA_PROGRAM_STATUS");
    let mut harness = Harness::setup();
    harness.send("\x1b[?62;4;52c");

    assert_eq!(harness.input(), None);
    assert!(!harness.terminal.kitty_protocol_active());
    assert_eq!(harness.write_count("\x1b[>4;2m"), 1);
}

#[test]
fn forwards_device_attributes_replies_that_answer_other_queries() {
    let _lock = env_lock();
    remove_env("KIANA_PROGRAM_STATUS");
    let mut harness = Harness::setup();
    harness.send("\x1b[?7u");
    harness.send("\x1b[?62;4;52c");
    assert_eq!(harness.input(), None);

    // The TUI's color query uses DA1 as its own sentinel.
    harness.send("\x1b[?62;4;52c");
    assert_eq!(harness.input().as_deref(), Some("\x1b[?62;4;52c"));
}

#[test]
fn forwards_normal_input_while_waiting_for_kitty_response() {
    let _lock = env_lock();
    remove_env("KIANA_PROGRAM_STATUS");
    let mut harness = Harness::setup();
    harness.send("a");

    assert_eq!(harness.input().as_deref(), Some("a"));
    assert!(!harness.terminal.kitty_protocol_active());
}

#[test]
fn tracks_split_kitty_confirmation() {
    let _lock = env_lock();
    remove_env("KIANA_PROGRAM_STATUS");
    let mut harness = Harness::setup();
    harness.send("\x1b[?7");
    harness.poll(Instant::now() + Duration::from_millis(10));
    assert_eq!(harness.input(), None);

    harness.send("u");
    assert!(harness.terminal.kitty_protocol_active());
    assert!(!harness.writes().iter().any(|write| write == "\x1b[>4;2m"));
}

/// #10607
mod program_status {
    use super::*;

    const WORKING: &str = "\x1b]7501;state=working:app=kiana\x1b\\";
    const CLEAR: &str = "\x1b]7501;state=clear\x1b\\";

    fn with_override<T>(value: Option<&str>, f: impl FnOnce() -> T) -> T {
        let previous = std::env::var_os("KIANA_PROGRAM_STATUS");
        unsafe {
            match value {
                Some(value) => std::env::set_var("KIANA_PROGRAM_STATUS", value),
                None => std::env::remove_var("KIANA_PROGRAM_STATUS"),
            }
        }
        let result = f();
        unsafe {
            match previous {
                Some(previous) => std::env::set_var("KIANA_PROGRAM_STATUS", previous),
                None => std::env::remove_var("KIANA_PROGRAM_STATUS"),
            }
        }
        result
    }

    fn working() -> ProgramStatus {
        ProgramStatus {
            state: ProgramStatusState::Working,
            app: Some("kiana".to_string()),
            kind: None,
            message: None,
        }
    }

    #[test]
    fn reports_the_latest_status_once_the_terminal_answers_the_query_before_da() {
        let _lock = env_lock();
        with_override(None, || {
            let mut harness = Harness::setup();
            harness.terminal.set_program_status(&working());
            assert!(!harness.writes().iter().any(|write| write == WORKING));

            harness.send("\x1b]7501;?\x1b\\");
            assert_eq!(harness.input(), None);
            assert_eq!(harness.write_count(WORKING), 1);
            harness.send("\x1b[?62;4;52c");

            harness.terminal.set_program_status(&ProgramStatus {
                state: ProgramStatusState::Done,
                app: None,
                kind: None,
                message: None,
            });
            assert_eq!(
                harness.last_write().as_deref(),
                Some("\x1b]7501;state=done\x1b\\")
            );
        });
    }

    #[test]
    fn reports_nothing_when_da_arrives_first_and_swallows_late_replies() {
        let _lock = env_lock();
        with_override(None, || {
            let mut harness = Harness::setup();
            harness.send("\x1b[?62;4;52c");
            harness.send("\x1b]7501;?\x07");
            harness.terminal.set_program_status(&working());

            assert_eq!(harness.input(), None);
            assert!(
                !harness
                    .writes()
                    .iter()
                    .any(|write| write.starts_with("\x1b]7501;state="))
            );
        });
    }

    #[test]
    fn skips_the_query_when_kiana_program_status_is_set() {
        let _lock = env_lock();
        with_override(Some("1"), || {
            let mut harness = Harness::setup();
            assert_eq!(harness.writes()[0], "\x1b[>7u\x1b[?u\x1b[c");
            harness.terminal.set_program_status(&working());
            assert_eq!(harness.last_write().as_deref(), Some(WORKING));
        });
        with_override(Some("0"), || {
            let mut harness = Harness::setup();
            assert_eq!(harness.writes()[0], "\x1b[>7u\x1b[?u\x1b[c");
            harness.terminal.set_program_status(&working());
            assert!(!harness.writes().iter().any(|write| write == WORKING));
        });
    }

    #[test]
    fn does_not_let_a_da_reply_from_before_a_restart_end_the_new_query() {
        let _lock = env_lock();
        with_override(None, || {
            let mut harness = Harness::setup();
            harness.terminal.set_program_status(&working());
            harness.terminal.stop();
            harness.terminal.query_and_enable_kitty_protocol();

            // The first start's replies arrive late: its DA, then the second start's reply and DA.
            harness.send("\x1b[?62;4;52c");
            harness.send("\x1b]7501;?\x1b\\");
            harness.send("\x1b[?62;4;52c");
            assert_eq!(harness.last_write().as_deref(), Some(WORKING));
            assert_eq!(harness.input(), None);
        });
    }

    #[test]
    fn clears_the_status_on_stop_and_reports_it_again_after_restart() {
        let _lock = env_lock();
        with_override(None, || {
            let mut harness = Harness::setup();
            harness.terminal.set_program_status(&working());
            harness.send("\x1b]7501;?\x1b\\");
            harness.terminal.stop();
            assert!(harness.writes().iter().any(|write| write == CLEAR));

            // Stopped: nothing is written until the restarted terminal confirms support again.
            let writes_before_restart = harness.writes().len();
            harness.terminal.set_program_status(&working());
            assert_eq!(harness.writes().len(), writes_before_restart);

            harness.terminal.query_and_enable_kitty_protocol();
            harness.send("\x1b]7501;?\x1b\\");
            assert_eq!(harness.last_write().as_deref(), Some(WORKING));
        });
    }
}

#[test]
fn replays_buffered_csi_prefix_input_when_it_is_not_a_kitty_response() {
    let _lock = env_lock();
    remove_env("KIANA_PROGRAM_STATUS");
    let mut harness = Harness::setup();
    harness.send("\x1b[");
    assert_eq!(harness.input(), None);

    // StdinBuffer 的序列超时（50ms）把 "\x1b[" 交给 ProcessTerminal，后者再等 150ms 判断它
    // 不是 Kitty 回复。
    std::thread::sleep(Duration::from_millis(60));
    harness.poll(Instant::now());
    assert_eq!(harness.input(), None);

    std::thread::sleep(Duration::from_millis(160));
    harness.poll(Instant::now());
    assert_eq!(harness.input().as_deref(), Some("\x1b["));
}

// ---------------------------------------------------------------------------
// ProcessTerminal progress
// ---------------------------------------------------------------------------

#[test]
fn writes_a_valid_osc_9_4_clear_sequence() {
    let writes = Arc::new(Mutex::new(Vec::new()));
    let mut terminal = ProcessTerminal::with_output(Box::new(CaptureWriter(Arc::clone(&writes))));
    terminal.set_progress(false);
    let writes = writes
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
        .clone();
    assert_eq!(writes, ["\x1b]9;4;0\x07"]);
}

// ---------------------------------------------------------------------------
// ProcessTerminal dimensions
// ---------------------------------------------------------------------------

#[test]
fn falls_back_to_columns_and_lines_before_default_dimensions() {
    let _lock = env_lock();
    let previous_columns = std::env::var_os("COLUMNS");
    let previous_lines = std::env::var_os("LINES");
    unsafe {
        std::env::set_var("COLUMNS", "123");
        std::env::set_var("LINES", "45");
    }

    // `with_output` 不查真实 TTY 尺寸（TS 里把 `process.stdout.columns` 设为 undefined）。
    let terminal =
        ProcessTerminal::with_output(Box::new(CaptureWriter(Arc::new(Mutex::new(Vec::new())))));
    let columns = terminal.columns();
    let rows = terminal.rows();

    unsafe {
        match previous_columns {
            Some(value) => std::env::set_var("COLUMNS", value),
            None => std::env::remove_var("COLUMNS"),
        }
        match previous_lines {
            Some(value) => std::env::set_var("LINES", value),
            None => std::env::remove_var("LINES"),
        }
    }

    assert_eq!(columns, 123);
    assert_eq!(rows, 45);
}

/// TS 的 getter 返回 `number`（`-5`、`45.5`、`Infinity` 都原样返回）；Rust 的 `Terminal`
/// 契约是 `usize`（ARCHITECTURE §4.9 冻结），因此只能取最接近的整数：小数截断、负数落到 0、
/// `+Infinity` 映射到 `usize::MAX`。这里锁定该近似，防止无意中回退成“非有限值用默认值”。
#[test]
fn env_dimensions_map_js_numbers_onto_usize() {
    let _lock = env_lock();
    let previous_columns = std::env::var_os("COLUMNS");
    let previous_lines = std::env::var_os("LINES");

    let measure = |columns: &str, lines: &str| {
        unsafe {
            std::env::set_var("COLUMNS", columns);
            std::env::set_var("LINES", lines);
        }
        let terminal =
            ProcessTerminal::with_output(Box::new(CaptureWriter(Arc::new(Mutex::new(Vec::new())))));
        (terminal.columns(), terminal.rows())
    };

    let fractional = measure("45.5", "-5");
    let infinite = measure("Infinity", "-Infinity");

    unsafe {
        match previous_columns {
            Some(value) => std::env::set_var("COLUMNS", value),
            None => std::env::remove_var("COLUMNS"),
        }
        match previous_lines {
            Some(value) => std::env::set_var("LINES", value),
            None => std::env::remove_var("LINES"),
        }
    }

    assert_eq!(fractional, (45, 0));
    assert_eq!(infinite, (usize::MAX, 0));
}

// ---------------------------------------------------------------------------
// 辅助
// ---------------------------------------------------------------------------

fn remove_env(name: &str) {
    // SAFETY: 所有环境变量操作都在 ENV_LOCK 下串行。
    unsafe { std::env::remove_var(name) };
}

/// `parseKeyboardProtocolNegotiationSequence` 的逐字契约（TS 未单独覆盖，这里锁定行为）。
#[test]
fn parses_keyboard_protocol_negotiation_sequences() {
    assert_eq!(
        parse_keyboard_protocol_negotiation_sequence("\x1b[?7u"),
        Some(KeyboardProtocolNegotiationSequence::KittyFlags(7))
    );
    assert_eq!(
        parse_keyboard_protocol_negotiation_sequence("\x1b[?0u"),
        Some(KeyboardProtocolNegotiationSequence::KittyFlags(0))
    );
    assert_eq!(
        parse_keyboard_protocol_negotiation_sequence("\x1b[?62;4;52c"),
        Some(KeyboardProtocolNegotiationSequence::DeviceAttributes)
    );
    assert_eq!(
        parse_keyboard_protocol_negotiation_sequence("\x1b[?c"),
        Some(KeyboardProtocolNegotiationSequence::DeviceAttributes)
    );
    assert_eq!(parse_keyboard_protocol_negotiation_sequence("\x1b[?"), None);
    assert_eq!(parse_keyboard_protocol_negotiation_sequence("\x1b[A"), None);
    assert_eq!(parse_keyboard_protocol_negotiation_sequence("a"), None);
    assert_eq!(
        parse_keyboard_protocol_negotiation_sequence("\x1b[?7"),
        None
    );
}

// ---------------------------------------------------------------------------
// KIANA_TUI_WRITE_LOG
// ---------------------------------------------------------------------------

/// 进程级 `KIANA_TUI_WRITE_LOG` 的唯一临时文件（同一测试二进制内串行）。
fn unique_temp_path(name: &str) -> std::path::PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    std::env::temp_dir().join(format!("kiana-tui-{name}-{}-{nanos}", std::process::id()))
}

/// TS 只在公开的 `write()` 里追加 `KIANA_TUI_WRITE_LOG`；`start()`/`stop()`/`setProgress()`/
/// `setTitle()`/光标与模式序列都直接写 stdout，不进入日志。
#[test]
fn write_log_records_only_the_public_write() {
    let _lock = env_lock();
    let log_path = unique_temp_path("write-log");
    let previous = std::env::var_os("KIANA_TUI_WRITE_LOG");
    // SAFETY: 环境变量在 ENV_LOCK 下串行修改。
    unsafe { std::env::set_var("KIANA_TUI_WRITE_LOG", &log_path) };

    let writes = Arc::new(Mutex::new(Vec::new()));
    let mut terminal = ProcessTerminal::with_output(Box::new(CaptureWriter(Arc::clone(&writes))));
    terminal.query_and_enable_kitty_protocol();
    terminal.set_title("title");
    terminal.hide_cursor();
    terminal.show_cursor();
    terminal.clear_line();
    terminal.clear_from_cursor();
    terminal.clear_screen();
    terminal.move_by(2);
    terminal.set_progress(true);
    terminal.set_progress(false);
    terminal.write("RENDER");
    terminal.stop();

    let logged = std::fs::read(&log_path).unwrap_or_default();
    unsafe {
        match previous {
            Some(value) => std::env::set_var("KIANA_TUI_WRITE_LOG", value),
            None => std::env::remove_var("KIANA_TUI_WRITE_LOG"),
        }
    }
    let _ = std::fs::remove_file(&log_path);

    assert_eq!(
        String::from_utf8_lossy(&logged),
        "RENDER",
        "only data passed to the public write() may be logged"
    );
    // stdout 仍然收到全部序列（日志与输出互不影响）。
    let stdout: String = writes
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
        .concat();
    assert!(stdout.starts_with("\x1b[>7u"));
    assert!(stdout.contains("\x1b]0;title\x07"));
    assert!(stdout.contains("RENDER"));
}

// ---------------------------------------------------------------------------
// restoreOnExit
// ---------------------------------------------------------------------------

/// `restoreOnExit(before, after)` 的字节顺序：(before) → 进度清除 → 关闭括号粘贴 → Kitty pop →
/// 关闭 modifyOtherKeys → (after)。Kitty pop 必须早于离开备用屏（`after`），否则备用屏退出后
/// 的按键释放仍会生成新的 Kitty 序列。
#[test]
fn restore_on_exit_writes_the_mode_resets_between_before_and_after() {
    let writes = Arc::new(Mutex::new(Vec::new()));
    let mut terminal = ProcessTerminal::with_output(Box::new(CaptureWriter(Arc::clone(&writes))));
    // 模拟 TuiAltScreen 退出时的进程状态。
    terminal.query_and_enable_kitty_protocol(); // keyboardProtocolPushed = true
    terminal.set_progress(true); // progressInterval 存在
    terminal.feed_input_str("\x1b[?0u"); // 无 Kitty flags → modifyOtherKeys 开启
    assert!(terminal.modify_other_keys_active());

    let before_len = writes
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
        .len();
    // TuiAltScreen 的退出恢复前缀（鼠标/焦点关闭 + 显示光标）与后缀（离开备用屏 + 显示光标）。
    let before = "\x1b[?1006l\x1b[?1004l\x1b[?1003l\x1b[?1000l\x1b[?25h";
    let after = "\x1b[?1049l\x1b[?25h";
    terminal.restore_on_exit(before, after);

    let restored: String = writes
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
        .iter()
        .skip(before_len)
        .cloned()
        .collect();
    assert_eq!(
        restored,
        format!("{before}\x1b]9;4;0\x07\x1b[?2004l\x1b[<u\x1b[>4;0m{after}")
    );
    for sequence in [
        "\x1b[?1006l",
        "\x1b[?1004l",
        "\x1b[?1003l",
        "\x1b[?1000l",
        "\x1b[<u",
        "\x1b[?2004l",
    ] {
        assert_eq!(
            restored.matches(sequence).count(),
            1,
            "{sequence:?} must appear exactly once"
        );
    }
    assert!(
        restored.find("\x1b[<u").expect("kitty pop present")
            < restored
                .find("\x1b[?1049l")
                .expect("alt screen exit present"),
        "Kitty pop must precede leaving the alternate screen"
    );
    assert!(restored.ends_with("\x1b[?1049l\x1b[?25h"));
}

// ---------------------------------------------------------------------------
// drainInput
// ---------------------------------------------------------------------------

/// 最小 `block_on`：`TimerSleep` 用后台线程 + waker 唤醒，不依赖任何 executor。
fn block_on<F: std::future::Future>(future: F) -> F::Output {
    use std::sync::Arc as StdArc;
    use std::task::{Context, Poll, Wake, Waker};

    struct Parker(std::thread::Thread);
    impl Wake for Parker {
        fn wake(self: StdArc<Self>) {
            self.0.unpark();
        }
    }

    let waker = Waker::from(StdArc::new(Parker(std::thread::current())));
    let mut context = Context::from_waker(&waker);
    let mut future = Box::pin(future);
    loop {
        match future.as_mut().poll(&mut context) {
            Poll::Ready(value) => return value,
            Poll::Pending => std::thread::park(),
        }
    }
}

/// `drainInput(maxMs, idleMs)`：空闲 `idleMs` 后返回；没有输入时约 1 个 idle 周期。
#[test]
fn drain_input_stops_after_the_idle_window() {
    let mut terminal =
        ProcessTerminal::with_output(Box::new(CaptureWriter(Arc::new(Mutex::new(Vec::new())))));
    let started = Instant::now();
    block_on(terminal.drain_input(1000, 30));
    let elapsed = started.elapsed();
    assert!(
        elapsed >= Duration::from_millis(25),
        "must wait the idle window, took {elapsed:?}"
    );
    assert!(
        elapsed < Duration::from_millis(500),
        "must not wait for maxMs, took {elapsed:?}"
    );
}

/// `maxMs` 是硬上限：即使 `idleMs` 更大，也必须在 `maxMs` 后返回（TS 的 `timeLeft <= 0`）。
#[test]
fn drain_input_honors_the_maximum_duration() {
    let mut terminal =
        ProcessTerminal::with_output(Box::new(CaptureWriter(Arc::new(Mutex::new(Vec::new())))));
    let started = Instant::now();
    block_on(terminal.drain_input(40, 1000));
    let elapsed = started.elapsed();
    assert!(
        elapsed >= Duration::from_millis(30),
        "must wait until the deadline, took {elapsed:?}"
    );
    assert!(
        elapsed < Duration::from_millis(500),
        "must not sleep a full idle window, took {elapsed:?}"
    );
}

/// drainInput 会先关闭 Kitty 协议（避免迟到的按键释放生成新序列），并恢复输入处理器。
#[test]
fn drain_input_disables_kitty_and_restores_the_input_handler() {
    let _lock = env_lock();
    remove_env("KIANA_PROGRAM_STATUS");
    let mut terminal =
        ProcessTerminal::with_output(Box::new(CaptureWriter(Arc::new(Mutex::new(Vec::new())))));
    let input = Arc::new(Mutex::new(None));
    let slot = Arc::clone(&input);
    terminal.set_input_handler(Box::new(move |data: &str| {
        *slot.lock().unwrap_or_else(|poison| poison.into_inner()) = Some(data.to_string());
    }));
    terminal.query_and_enable_kitty_protocol();
    terminal.feed_input_str("\x1b[?7u");
    assert!(terminal.kitty_protocol_active());

    block_on(terminal.drain_input(100, 10));

    assert!(!terminal.kitty_protocol_active());
    // 处理器在 drain 结束后恢复：新输入仍然分发。
    terminal.feed_input_str("a");
    assert_eq!(
        input
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .as_deref(),
        Some("a")
    );
    terminal.stop();
    set_kitty_protocol_active(false);
}
