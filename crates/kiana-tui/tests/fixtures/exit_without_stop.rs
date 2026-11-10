//! `packages/tui/test/fixtures/exit-without-stop.ts` 的等价物（子进程 fixture）。
//!
//! TS 的 fixture 直接 `new TuiAltScreen(new ProcessTerminal(), false, undefined, { mouse: true })`
//! 后 `start()`，并可选地 `stop({ preserveScreen: true })`，然后 `process.exit(0)` 触发 Node 的
//! `exit` 事件处理器。
//!
//! **偏差（已记录）**：备用屏渲染器 `TuiAltScreen` 属于 `tui-alt-screen` 单元（本仓库里仍是
//! 骨架桩），因此本 fixture 用主屏渲染器 [`TuiMainScreen`] + 真实的 [`ProcessTerminal`]
//! 走**同一条退出路径**：`TuiBase` 的 exit 处理器 → `terminal.restoreOnExit(prefix, suffix)`。
//! 备用屏专属的序列（鼠标追踪 `?1000l/?1003l/?1006l`、焦点上报 `?1004l`、备用屏 `?1049l`）
//! 由该单元自己的 fixture 覆盖；本文件只断言主屏与终端层都能观察到的部分
//! （bracketed paste `?2004l` 恰好一次、Kitty 弹出在弃用粘贴之后）。
//!
//! 子进程协议：父测试用 `--exact exit_fixture_child` 重新执行本测试二进制，通过环境变量
//! `KIANA_EXIT_FIXTURE_MODE`（`exit` / `stop`）与 `KIANA_EXIT_FIXTURE_OUT`（输出文件）驱动。
//! 子进程把终端输出写进文件（没有真实 TTY），父测试读回文件断言序列计数。

#![allow(dead_code, reason = "fixture 只在被父测试文件包含时使用")]

use std::path::PathBuf;

use kiana_tui::main_screen::TuiMainScreen;
use kiana_tui::terminal::ProcessTerminal;
use kiana_tui::tui::{Component, Tui, TuiStopOptions};

/// fixture 的运行模式（TS `process.argv[2]`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FixtureMode {
    /// 进程退出但不调用 `stop()`。
    Exit,
    /// 正常 `stop({ preserveScreen: true })` 后再退出。
    Stop,
}

impl FixtureMode {
    pub fn as_str(self) -> &'static str {
        match self {
            FixtureMode::Exit => "exit",
            FixtureMode::Stop => "stop",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "exit" => Some(FixtureMode::Exit),
            "stop" => Some(FixtureMode::Stop),
            _ => None,
        }
    }
}

/// 一行固定内容（fixture 不需要更复杂的组件）。
struct FixtureContent;

impl Component for FixtureContent {
    fn render(&mut self, _width: usize) -> Vec<String> {
        vec!["fixture".to_string()]
    }

    fn handles_input(&self) -> bool {
        false
    }
}

/// 子进程入口：按模式跑 fixture，把终端输出写进 `out_path`，然后**直接退出**。
///
/// 与 TS 的 `process.exit(0)` 一致：不执行析构（`ProcessTerminal::write_raw` 每次写入都
/// flush，因此文件内容不会丢）。
pub fn run(mode: FixtureMode, out_path: &std::path::Path) -> ! {
    let file = std::fs::File::create(out_path).expect("create fixture output file");
    let terminal = ProcessTerminal::with_output(Box::new(file));
    let mut tui = TuiMainScreen::new(Box::new(terminal));
    tui.add_child(Box::new(FixtureContent));
    tui.start();
    tui.render_now(false);
    if mode == FixtureMode::Stop {
        tui.stop(TuiStopOptions {
            preserve_screen: true,
        });
    }
    // 宿主在进程退出时调用它（TS 的 `process.on("exit", …)`）；`stop()` 之后必须是空操作。
    tui.base_mut().run_exit_restore_handler();
    std::process::exit(0);
}

/// 子进程测试入口（父测试用 `--exact exit_fixture_child` 重新执行本二进制时命中）。
#[test]
fn exit_fixture_child() {
    let Some(mode) = std::env::var("KIANA_EXIT_FIXTURE_MODE").ok() else {
        // 父进程里的普通运行：不做任何事。
        return;
    };
    let Some(out_path) = std::env::var("KIANA_EXIT_FIXTURE_OUT").ok() else {
        return;
    };
    let Some(mode) = FixtureMode::parse(&mode) else {
        panic!("unknown KIANA_EXIT_FIXTURE_MODE: {mode}");
    };
    run(mode, &PathBuf::from(out_path));
}
