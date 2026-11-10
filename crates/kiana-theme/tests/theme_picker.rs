//! `theme-picker.test.ts` 的逐条移植。

mod common;

use common::{EnvGuard, builtin_json, lock, reset_theme_state, write_theme_file};
use kiana_theme::{
    ThemeInfo, get_available_themes, get_available_themes_with_paths, set_registered_themes,
};

#[test]
fn uses_custom_theme_content_names_instead_of_file_names() {
    let _lock = lock();
    reset_theme_state();
    let root = tempfile::tempdir().expect("temp dir");
    let agent_dir = root.path().join("agent");
    std::fs::create_dir_all(agent_dir.join("themes")).expect("themes dir");
    let _env = EnvGuard::set("KIANA_CODING_AGENT_DIR", &agent_dir);
    set_registered_themes(Vec::new()).expect("registered themes reset");

    let mut custom_theme = builtin_json("dark");
    custom_theme["name"] = serde_json::json!("bar");
    let theme_path = write_theme_file(
        &agent_dir.join("themes"),
        "foo.json",
        &serde_json::to_string_pretty(&custom_theme).expect("serialize"),
    );

    assert!(get_available_themes().contains(&"bar".to_string()));
    assert!(!get_available_themes().contains(&"foo".to_string()));
    assert!(get_available_themes_with_paths().contains(&ThemeInfo {
        name: "bar".to_string(),
        path: Some(theme_path),
    }));
    assert!(
        !get_available_themes_with_paths()
            .iter()
            .any(|theme| theme.name == "foo")
    );
    // `system` 永远排第一（TS 的 `system theme` 用例也断言这一点）。
    assert_eq!(get_available_themes()[0], "system");

    set_registered_themes(Vec::new()).expect("registered themes reset");
}

/// `getAvailableThemesWithPaths()` 用 `localeCompare` 排序：大小写不同的名字按
/// 大小写无关的主权重排序（`apple` 在 `Zebra` 之前），而不是字节序。
#[test]
fn sorts_names_with_locale_compare() {
    let _lock = lock();
    reset_theme_state();
    let root = tempfile::tempdir().expect("temp dir");
    let agent_dir = root.path().join("agent");
    std::fs::create_dir_all(agent_dir.join("themes")).expect("themes dir");
    let _env = EnvGuard::set("KIANA_CODING_AGENT_DIR", &agent_dir);
    set_registered_themes(Vec::new()).expect("registered themes reset");

    for (file, name) in [
        ("a.json", "apple"),
        ("b.json", "Zebra"),
        ("c.json", "apple2"),
        ("d.json", "Äpfel"),
        ("e.json", "apfel"),
    ] {
        let mut custom_theme = builtin_json("dark");
        custom_theme["name"] = serde_json::json!(name);
        write_theme_file(
            &agent_dir.join("themes"),
            file,
            &serde_json::to_string_pretty(&custom_theme).expect("serialize"),
        );
    }

    let names: Vec<String> = get_available_themes()
        .into_iter()
        .filter(|name| !matches!(name.as_str(), "system" | "dark" | "light"))
        .collect();
    // 与 Node 26 / ICU 的实测顺序一致（变音符号是二次权重：apfel < Äpfel < apple）。
    assert_eq!(names, vec!["apfel", "Äpfel", "apple", "apple2", "Zebra"]);

    set_registered_themes(Vec::new()).expect("registered themes reset");
}

/// 评审 finding（major）：TS 的 `getCustomThemeInfos()` 不捕获 `fs.readdirSync()` 的错误，
/// `getAvailableThemesWithPaths()` 会把 Node 的异常原样抛给调用方（ENOTDIR / EACCES）。
/// Rust 的公开签名（spec §5.2）返回 `Vec<ThemeInfo>`，无法返回错误：这里用 panic 承载
/// 与 Node `error.message` 相同的文案（当前稳定契约，改动需同步测试）。
#[test]
fn readdir_errors_surface_like_node() {
    let _lock = lock();
    reset_theme_state();
    let root = tempfile::tempdir().expect("temp dir");
    let agent_dir = root.path().join("agent");
    std::fs::create_dir_all(&agent_dir).expect("agent dir");
    let _env = EnvGuard::set("KIANA_CODING_AGENT_DIR", &agent_dir);
    set_registered_themes(Vec::new()).expect("registered themes reset");

    // `themes` 是普通文件：Node `fs.readdirSync()` → ENOTDIR: not a directory, scandir '<p>'。
    let themes_dir = agent_dir.join("themes");
    std::fs::write(&themes_dir, "x").expect("themes file");
    assert_eq!(
        panic_message(get_available_themes_with_paths),
        format!(
            "ENOTDIR: not a directory, scandir '{}'",
            themes_dir.display()
        )
    );

    // `themes` 是不可读目录：Node → EACCES: permission denied, scandir '<p>'。
    // root 等不执行权限检查的环境读得到，此时 TS 也不会抛，跳过该断言。
    std::fs::remove_file(&themes_dir).expect("remove themes file");
    std::fs::create_dir_all(&themes_dir).expect("themes dir");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&themes_dir, std::fs::Permissions::from_mode(0o000))
            .expect("chmod 000");
        let denied = std::fs::read_dir(&themes_dir).is_err();
        if denied {
            assert_eq!(
                panic_message(get_available_themes_with_paths),
                format!(
                    "EACCES: permission denied, scandir '{}'",
                    themes_dir.display()
                )
            );
        }
        std::fs::set_permissions(&themes_dir, std::fs::Permissions::from_mode(0o700))
            .expect("chmod 700");
    }

    set_registered_themes(Vec::new()).expect("registered themes reset");
}

/// 调用必须 panic，并返回 panic 载荷里的消息（`panic!("{error}")` 的 String 载荷）。
fn panic_message<T>(call: impl FnOnce() -> T) -> String {
    let payload = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(call)) {
        Ok(_) => panic!("expected the call to panic"),
        Err(payload) => payload,
    };
    payload
        .downcast_ref::<String>()
        .cloned()
        .or_else(|| {
            payload
                .downcast_ref::<&str>()
                .map(|text| (*text).to_string())
        })
        .expect("panic payload is a string")
}
