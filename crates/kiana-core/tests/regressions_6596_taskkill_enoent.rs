//! `test/suite/regressions/6596-taskkill-enoent.test.ts`。
//!
//! #6596：Windows 上 `taskkill` 的 spawn 失败会异步发出 `error` 事件；Node 的
//! `child_process` 在没有监听器时会抛未捕获异常。TS 用 `child.once("error", () => {})` 吞掉。
//! #9490：`SystemRoot` 缺失/为空时必须保留绝对路径回退 `C:\Windows`。

use std::cell::RefCell;
use std::io;
use std::sync::Mutex;

use kiana_core::shell::{Platform, kill_process_tree_with};

static ENV_LOCK: Mutex<()> = Mutex::new(());

fn set_system_root(value: Option<&str>) {
    // SAFETY: 本文件的所有 env 修改都在 `ENV_LOCK` 之下串行执行。
    unsafe {
        match value {
            Some(value) => std::env::set_var("SystemRoot", value),
            None => std::env::remove_var("SystemRoot"),
        }
    }
}

struct SystemRootGuard(Option<std::ffi::OsString>);

impl SystemRootGuard {
    fn capture() -> Self {
        Self(std::env::var_os("SystemRoot"))
    }
}

impl Drop for SystemRootGuard {
    fn drop(&mut self) {
        // SAFETY: 同上（`ENV_LOCK` 在 drop 顺序里仍然有效，因为 guard 先于锁释放）。
        unsafe {
            match &self.0 {
                Some(value) => std::env::set_var("SystemRoot", value),
                None => std::env::remove_var("SystemRoot"),
            }
        }
    }
}

#[test]
fn uses_system32_taskkill_and_consumes_its_spawn_error() {
    let _lock = ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let _saved = SystemRootGuard::capture();

    let cases: &[(Option<&str>, &str)] = &[
        (Some("D:\\CustomWindows"), "D:\\CustomWindows"),
        (None, "C:\\Windows"),
        (Some(""), "C:\\Windows"),
    ];

    for (system_root, expected_root) in cases {
        set_system_root(*system_root);
        let spawns: RefCell<Vec<(String, Vec<String>)>> = RefCell::new(Vec::new());
        let spawn = |program: &str, args: &[String]| -> io::Result<()> {
            spawns
                .borrow_mut()
                .push((program.to_string(), args.to_vec()));
            // 模拟 `spawn taskkill ENOENT`：错误必须被吞掉（不 panic）。
            Err(io::Error::other("spawn taskkill ENOENT"))
        };
        kill_process_tree_with(1234, Platform::Win32, &mut |_pid, _signal| Ok(()), &spawn);

        let spawns = spawns.into_inner();
        assert_eq!(
            spawns,
            vec![(
                format!("{expected_root}\\System32\\taskkill.exe"),
                vec![
                    "/F".to_string(),
                    "/T".to_string(),
                    "/PID".to_string(),
                    "1234".to_string()
                ]
            )],
            "SystemRoot={system_root:?}"
        );
    }
}
