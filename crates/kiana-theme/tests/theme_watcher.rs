//! `theme.ts` 的 `startThemeWatcher()` 热重载：回归测试（评审 finding，critical）。
//!
//! `reload_watched_theme()` 会重新读取被监视的主题文件。Node 的 `fs.watch` 建在 libuv 的
//! inotify 掩码上，读取（IN_OPEN / IN_CLOSE_NOWRITE）不会产生事件；而 `notify` 会把它们
//! 映射成 `EventKind::Access`。如果 `kiana-core::fs_watch` 把 Access 也转发成 `"change"`，
//! 去抖定时器会被自己的读取无限重新布防：`onThemeChange()` 以约 10 Hz 持续触发，
//! 交互模式随之反复 `invalidate()` / `requestRender()`。
//!
//! 一次写入只应产生一次去抖重载；之后回调计数必须保持不变。

mod common;

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use common::{EnvGuard, builtin_json, lock, reset_theme_state, write_theme_file};
use kiana_theme::{ThemeToken, current_theme, init_theme, on_theme_change, stop_theme_watcher};
use kiana_tui::parse_color;

#[test]
fn hot_reload_does_not_re_trigger_itself_after_a_write() {
    let _lock = lock();
    reset_theme_state();
    let root = tempfile::tempdir().expect("temp dir");
    let agent_dir = root.path().join("agent");
    let themes_dir = agent_dir.join("themes");
    std::fs::create_dir_all(&themes_dir).expect("themes dir");
    let _env = EnvGuard::set("KIANA_CODING_AGENT_DIR", &agent_dir);

    let name = "watched-theme";
    let mut document = builtin_json("dark");
    document["name"] = serde_json::json!(name);
    document["colors"]["accent"] = serde_json::json!("#112233");
    write_theme_file(
        &themes_dir,
        "watched-theme.json",
        &serde_json::to_string(&document).expect("json"),
    );

    init_theme(Some(name), true);
    assert_eq!(
        current_theme().expect("theme").color(ThemeToken::Accent),
        Some(parse_color("#112233").expect("color"))
    );

    let changes = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&changes);
    on_theme_change(Box::new(move || {
        counter.fetch_add(1, Ordering::SeqCst);
    }));

    // 改文件 → 等 100 ms 去抖后的重载（超时给足，避免 CI 抖动）。
    document["colors"]["accent"] = serde_json::json!("#ff0000");
    write_theme_file(
        &themes_dir,
        "watched-theme.json",
        &serde_json::to_string(&document).expect("json"),
    );

    let deadline = Instant::now() + Duration::from_secs(10);
    let expected = Some(parse_color("#ff0000").expect("color"));
    while current_theme().expect("theme").color(ThemeToken::Accent) != expected {
        assert!(
            Instant::now() < deadline,
            "theme file change was not hot-reloaded"
        );
        std::thread::sleep(Duration::from_millis(20));
    }

    let settled = changes.load(Ordering::SeqCst);
    assert!(settled >= 1, "on_theme_change must fire for the write");

    // 重载重新读取了主题文件；读取不是 Node `fs.watch` 的事件，不能再布防定时器。
    // 修复前这里每 100 ms 递增一次（700 ms 内约 7 次）。
    std::thread::sleep(Duration::from_millis(700));
    assert_eq!(
        changes.load(Ordering::SeqCst),
        settled,
        "reading the theme file must not schedule another reload (Node fs.watch ignores reads)"
    );

    stop_theme_watcher();
    // 清掉回调，避免影响同进程的其它用例。
    on_theme_change(Box::new(|| {}));
    reset_theme_state();
}
