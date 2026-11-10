//! `test/shell-kill-process-tree.test.ts`（#11252 的回归覆盖）。
//!
//! `kill_process_tree_with` 记录 kill/spawn 的调用序列，因此整组用例在 Linux 上可跑。

use std::cell::RefCell;

use kiana_core::shell::{KillSignal, Platform, kill_process_tree_with};

#[derive(Default)]
struct Recorder {
    calls: RefCell<Vec<(i32, KillSignal)>>,
    fail_stop: bool,
    fail_group_kill: bool,
    fail_single_kill: bool,
}

impl Recorder {
    fn kill(&self, pid: i32, signal: KillSignal) -> std::io::Result<()> {
        self.calls.borrow_mut().push((pid, signal));
        let group = pid < 0;
        if group && signal == KillSignal::SigStop && self.fail_stop {
            return Err(std::io::Error::other("stop failed"));
        }
        if group && signal == KillSignal::SigKill && self.fail_group_kill {
            return Err(std::io::Error::other("group kill failed"));
        }
        if !group && signal == KillSignal::SigKill && self.fail_single_kill {
            return Err(std::io::Error::other("process no longer exists"));
        }
        Ok(())
    }

    fn calls(&self) -> Vec<(i32, KillSignal)> {
        self.calls.borrow().clone()
    }
}

fn run(platform: Platform, recorder: &Recorder) {
    let spawn = |_program: &str, _args: &[String]| Ok(());
    kill_process_tree_with(
        1234,
        platform,
        &mut |pid, signal| recorder.kill(pid, signal),
        &spawn,
    );
}

#[test]
fn stops_the_macos_process_group_before_killing_it_without_resuming_it() {
    let recorder = Recorder::default();
    run(Platform::Darwin, &recorder);
    assert_eq!(
        recorder.calls(),
        vec![(-1234, KillSignal::SigStop), (-1234, KillSignal::SigKill)]
    );
}

#[test]
fn still_kills_the_group_when_stopping_fails() {
    let recorder = Recorder {
        fail_stop: true,
        ..Recorder::default()
    };
    run(Platform::Darwin, &recorder);
    assert_eq!(
        recorder.calls(),
        vec![(-1234, KillSignal::SigStop), (-1234, KillSignal::SigKill)]
    );
}

#[test]
fn falls_back_to_killing_the_shell_when_the_group_kill_fails() {
    let recorder = Recorder {
        fail_group_kill: true,
        ..Recorder::default()
    };
    run(Platform::Darwin, &recorder);
    assert_eq!(
        recorder.calls(),
        vec![
            (-1234, KillSignal::SigStop),
            (-1234, KillSignal::SigKill),
            (1234, KillSignal::SigKill),
        ]
    );
}

#[test]
fn tolerates_failure_of_both_group_signals_and_the_fallback() {
    let recorder = Recorder {
        fail_stop: true,
        fail_group_kill: true,
        fail_single_kill: true,
        ..Recorder::default()
    };
    run(Platform::Darwin, &recorder);
    assert_eq!(
        recorder.calls(),
        vec![
            (-1234, KillSignal::SigStop),
            (-1234, KillSignal::SigKill),
            (1234, KillSignal::SigKill),
        ]
    );
}

#[test]
fn keeps_linux_group_termination_unchanged() {
    let recorder = Recorder::default();
    run(Platform::Linux, &recorder);
    assert_eq!(recorder.calls(), vec![(-1234, KillSignal::SigKill)]);
}

#[test]
fn other_unix_platforms_only_kill_the_group() {
    let recorder = Recorder::default();
    run(Platform::Other, &recorder);
    assert_eq!(recorder.calls(), vec![(-1234, KillSignal::SigKill)]);
}

#[test]
fn windows_spawns_taskkill_without_calling_kill() {
    // 该用例只读 `SystemRoot`；显式固定它，避免依赖宿主环境。
    // SAFETY: 本文件没有别的测试读写 env，且这里立即恢复。
    let saved = std::env::var_os("SystemRoot");
    unsafe { std::env::set_var("SystemRoot", "C:\\Windows") };
    let recorder = Recorder::default();
    let spawns: RefCell<Vec<(String, Vec<String>)>> = RefCell::new(Vec::new());
    let spawn = |program: &str, args: &[String]| {
        spawns
            .borrow_mut()
            .push((program.to_string(), args.to_vec()));
        Ok(())
    };
    kill_process_tree_with(
        1234,
        Platform::Win32,
        &mut |pid, signal| recorder.kill(pid, signal),
        &spawn,
    );
    assert!(recorder.calls().is_empty());
    let spawns = spawns.into_inner();
    // SAFETY: 见上。
    unsafe {
        match saved {
            Some(value) => std::env::set_var("SystemRoot", value),
            None => std::env::remove_var("SystemRoot"),
        }
    }
    assert_eq!(spawns.len(), 1);
    assert_eq!(spawns[0].0, "C:\\Windows\\System32\\taskkill.exe");
    assert_eq!(spawns[0].1, vec!["/F", "/T", "/PID", "1234"]);
}
