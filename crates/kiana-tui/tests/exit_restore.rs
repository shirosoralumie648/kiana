//! `test/exit-restore.test.ts` 的移植（2 个 it + `TuiBase` 契约补充 + 备用屏退出恢复契约）。
//!
//! TS 用 `spawnSync` 跑一个真实进程（`fixtures/exit-without-stop.ts`），断言退出路径写出的
//! 终端恢复序列。Rust 侧同样起子进程：父测试用 `--exact` 重新执行本测试二进制，子进程按
//! `KIANA_EXIT_FIXTURE_MODE` 跑 `fixtures/exit_without_stop.rs`，把终端输出写进文件。
//!
//! **偏差（已记录）**：TS fixture 用的是 `TuiAltScreen` + `{ mouse: true }`，而备用屏渲染器属
//! `tui-alt-screen` 单元（本仓库仍是骨架桩），fixture 因此改跑主屏渲染器 + `ProcessTerminal`。
//!
//! 为满足 TS 的完整断言集合，`alt_screen_exit_restore_sequences_match_the_ts_contract` 用
//! 一个按 `tui-alt-screen.ts:431-441` 逐字拼串的生命周期钩子（[`AltScreenLikeHooks`]）驱动
//! 真实的 `TuiBase` → `ProcessTerminal` 退出管线，断言 `?1006l/?1004l/?1003l/?1000l/?2004l`
//! 与 Kitty 弹出各恰好一次、Kitty 弹出先于 `?1049l`、输出以 `?1049l?25h` 结尾。
//! 真正 `TuiAltScreen` 落地后，其 fixture 会用真实实现复跑同一组断言。

#[path = "common/mod.rs"]
mod common;

#[path = "fixtures/exit_without_stop.rs"]
mod exit_without_stop;

use std::path::PathBuf;
use std::process::{Command, Stdio};

use common::TestTerminal;
use exit_without_stop::FixtureMode;
use kiana_tui::terminal::ProcessTerminal;
use kiana_tui::tui::{TuiBase, TuiLifecycleHooks, TuiStopOptions};

/// 跑一次 fixture 子进程，返回它写出的终端输出。
fn run_fixture(mode: FixtureMode) -> String {
    let out_path = std::env::temp_dir().join(format!(
        "kiana-exit-fixture-{}-{}-{}.out",
        std::process::id(),
        mode.as_str(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or(0)
    ));
    let executable = std::env::current_exe().expect("test binary path");
    let output = Command::new(executable)
        .args([
            "--exact",
            "exit_without_stop::exit_fixture_child",
            "--nocapture",
        ])
        .env("KIANA_EXIT_FIXTURE_MODE", mode.as_str())
        .env("KIANA_EXIT_FIXTURE_OUT", &out_path)
        .env_remove("TMUX")
        .env_remove("STY")
        .env_remove("ZELLIJ")
        .env("TERM", "xterm-256color")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("spawn fixture child");
    assert!(
        output.status.success(),
        "fixture failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    std::fs::read_to_string(&out_path).expect("fixture output")
}

/// `count(haystack, needle)`。
fn count(haystack: &str, needle: &str) -> usize {
    haystack.matches(needle).count()
}

/// `restores terminal modes when the process exits without stop()`
///
/// 回归 #11170 的主屏等价物：跳过 `stop()` 的退出路径必须恢复终端模式。主屏可观察的部分是
/// 分组粘贴复位、Kitty 键盘协议弹出（都在 `restoreOnExit` 里，顺序固定）以及输出结尾；
/// 备用屏专属的 `?1000l/?1003l/?1006l/?1004l/?1049l` 由下面的 alt-screen 契约测试覆盖。
#[test]
fn restores_terminal_modes_when_the_process_exits_without_stop() {
    let output = run_fixture(FixtureMode::Exit);
    assert_eq!(count(&output, "\x1b[?2004l"), 1, "paste reset exactly once");
    // 关闭 bracketed paste 由 `restoreOnExit` 写出（`stop()` 从未运行）。
    assert_eq!(count(&output, "\x1b[?2004h"), 1, "start enables paste once");
    // `restoreOnExit`：`before` + `?2004l` + （Kitty 已协商时）`\x1b[<u` + `after`。
    assert_eq!(count(&output, "\x1b[<u"), 1, "Kitty pop exactly once");
    let kitty_pop = output.find("\x1b[<u").expect("Kitty pop present");
    assert!(
        output.find("\x1b[?2004l").unwrap() < kitty_pop,
        "Kitty pop must follow the paste reset"
    );
    // 主屏的 `getExitRestoreSuffix()` 是空串：输出以 Kitty 弹出结束。
    assert!(
        output.ends_with("\x1b[<u"),
        "restore tail: {:?}",
        &output[output.len().saturating_sub(40)..]
    );
}

/// `does not restore twice after a normal stop()`
#[test]
fn does_not_restore_twice_after_a_normal_stop() {
    let output = run_fixture(FixtureMode::Stop);
    assert_eq!(count(&output, "\x1b[?2004l"), 1, "paste reset exactly once");
    assert_eq!(count(&output, "\x1b[?2004h"), 1, "start enables paste once");
}

// ============================================================================
// `TuiBase` 契约补充（不经过子进程）
// ============================================================================

/// `restoreOnExit(before, after)` 的调用记录。
type RestoreCalls = std::rc::Rc<std::cell::RefCell<Vec<(String, String)>>>;

/// 建一个 `TuiBase` 并返回 `restoreOnExit` 的调用记录。
fn base(color_scheme_notifications: bool) -> (TuiBase, RestoreCalls) {
    let terminal = TestTerminal::with_size(80, 24);
    let calls = terminal.restore_calls_handle();
    let mut base = TuiBase::new(Box::new(terminal), None, None);
    base.set_terminal_color_scheme_notifications(color_scheme_notifications);
    (base, calls)
}

/// 退出处理器只在 `start()` 之后挂载，退出路径只恢复一次。
#[test]
fn tui_base_restores_terminal_modes_when_the_process_exits_without_stop() {
    let (mut base, calls) = base(false);
    base.start();
    assert!(base.exit_restore_registered());

    base.run_exit_restore_handler();
    // 只恢复一次（TS 断言每个恢复序列出现 1 次）。
    assert_eq!(calls.borrow().len(), 1);
    assert_eq!(
        calls.borrow()[0],
        // `getExitRestorePrefix()` 默认 = 光标显示；`getExitRestoreSuffix()` 默认空。
        ("\x1b[?25h".to_string(), String::new())
    );
}

/// 前缀会带上已开启的终端明暗通知复位（`getExitRestorePrefix()`）。
#[test]
fn exit_restore_prefix_includes_color_scheme_notification_reset() {
    let (mut base, calls) = base(true);
    base.start();
    base.run_exit_restore_handler();
    assert_eq!(
        calls.borrow()[0],
        ("\x1b[?2031l\x1b[?25h".to_string(), String::new())
    );
}

/// `stop()` 之后处理器注销，退出路径不会再恢复第二次。
#[test]
fn tui_base_does_not_restore_twice_after_a_normal_stop() {
    let (mut base, calls) = base(false);
    base.start();
    base.stop(TuiStopOptions::default());
    // `stop()` 注销退出处理器（TS `process.off("exit", …)`）。
    assert!(!base.exit_restore_registered());

    base.run_exit_restore_handler();
    assert!(calls.borrow().is_empty());
}

/// `PathBuf` 只用于让 fixture 的临时文件名可见（避免未使用导入告警）。
#[allow(dead_code)]
fn unused_path_hint(path: PathBuf) -> PathBuf {
    path
}

// ============================================================================
// 备用屏退出恢复契约（TS `exit-restore.test.ts` 的完整断言集合）
// ============================================================================

/// `tui-alt-screen.ts:431-441` 的 `getExitRestorePrefix()` / `getExitRestoreSuffix()`
/// （备用屏活跃 + 鼠标开启）的等价物。
///
/// 真正的 `TuiAltScreen` 属于 `tui-alt-screen` 单元（本仓库仍是骨架桩），这里用一个
/// 生命周期钩子按该文件逐字拼出同样的序列，从而验证 `TuiBase` → `ProcessTerminal` 的退出
/// 恢复管线（顺序、次数、结尾）符合下列断言的契约；`TuiAltScreen` 落地后其自己的 fixture 会
/// 用真实实现复跑同一组断言。
struct AltScreenLikeHooks {
    alt_screen_active: bool,
}

impl TuiLifecycleHooks for AltScreenLikeHooks {
    fn exit_restore_prefix(&mut self, base: &str) -> String {
        if !self.alt_screen_active {
            return base.to_string();
        }
        // `${END_SYNCHRONIZED_OUTPUT}${DISABLE_MOUSE}${ENABLE_AUTOWRAP}${super.getExitRestorePrefix()}`
        format!("\x1b[?2026l\x1b[?1006l\x1b[?1004l\x1b[?1003l\x1b[?1002l\x1b[?1000l\x1b[?7h{base}")
    }

    fn exit_restore_suffix(&mut self, base: &str) -> String {
        if !self.alt_screen_active {
            return base.to_string();
        }
        // `${super.getExitRestoreSuffix()}${EXIT_ALT_SCREEN}\x1b[?25h`
        format!("{base}\x1b[?1049l\x1b[?25h")
    }
}

/// `restores terminal modes when the process exits without stop()`（TS 第一个 it）的完整断言：
/// 每个模式复位恰好一次、Kitty 弹出在离开备用屏之前、输出以 `?1049l?25h` 结尾。
#[test]
fn alt_screen_exit_restore_sequences_match_the_ts_contract() {
    let out_path = std::env::temp_dir().join(format!(
        "kiana-alt-exit-restore-{}-{}.out",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or(0)
    ));
    let file = std::fs::File::create(&out_path).expect("create fixture output file");
    let terminal = ProcessTerminal::with_output(Box::new(file));
    let mut base = TuiBase::new(Box::new(terminal), None, None);
    base.set_lifecycle_hooks(Box::new(AltScreenLikeHooks {
        alt_screen_active: true,
    }));

    base.start();
    // 宿主在进程退出时调用（TS 的 `process.on("exit", …)`）。
    base.run_exit_restore_handler();
    let output = std::fs::read_to_string(&out_path).expect("fixture output");
    let _ = std::fs::remove_file(&out_path);
    // `stop()` 只用于恢复本进程的终端状态（`start()` 可能改过 raw mode）。
    base.stop(TuiStopOptions::default());

    for sequence in [
        "\x1b[?1006l",
        "\x1b[?1004l",
        "\x1b[?1003l",
        "\x1b[?1000l",
        "\x1b[<u",
        "\x1b[?2004l",
    ] {
        assert_eq!(count(&output, sequence), 1, "sequence {sequence:?}");
    }
    let kitty_pop = output.find("\x1b[<u").expect("Kitty pop present");
    let leave_alt_screen = output.find("\x1b[?1049l").expect("alt screen exit present");
    assert!(
        kitty_pop < leave_alt_screen,
        "Kitty pop must precede leaving alt screen: {output:?}"
    );
    assert!(
        output.ends_with("\x1b[?1049l\x1b[?25h"),
        "unexpected restore tail: {:?}",
        &output[output.len().saturating_sub(40)..]
    );
}
