//! `utils/windows-self-update.ts` 的移植测试（TS 无对应测试；按 §4.8 第 9 条覆盖公开 API）。
//!
//! `quarantineWindowsNativeDependencies` 在 Rust 侧是文档化的全平台 no-op（没有 Node 的
//! `process.report.sharedObjects`）；`cleanupWindowsSelfUpdateQuarantine` 是可直接观察的
//! `rm -rf <node_modules>/.kiana-native-quarantine`。

use std::path::Path;

use kiana_core::changelog::{
    cleanup_windows_self_update_quarantine, quarantine_windows_native_dependencies,
};

fn node_modules_quarantine(root: &Path) -> std::path::PathBuf {
    root.join("node_modules").join(".kiana-native-quarantine")
}

#[test]
fn removes_the_quarantine_directory_under_node_modules() {
    let directory = tempfile::tempdir().expect("temp dir");
    let package_dir = directory.path().join("node_modules").join("pkg");
    std::fs::create_dir_all(package_dir.join("lib")).expect("package dir");
    let quarantine = node_modules_quarantine(directory.path());
    std::fs::create_dir_all(quarantine.join("run-1")).expect("quarantine");
    std::fs::write(quarantine.join("run-1").join("addon.node"), b"binary").expect("file");

    cleanup_windows_self_update_quarantine(&package_dir);

    assert!(!quarantine.exists());
    // 只删隔离区，不动 node_modules 的其它内容。
    assert!(directory.path().join("node_modules").exists());
    assert!(package_dir.join("lib").exists());
}

#[test]
fn finds_node_modules_case_insensitively() {
    let directory = tempfile::tempdir().expect("temp dir");
    let node_modules = directory.path().join("NODE_MODULES");
    let package_dir = node_modules.join("pkg");
    std::fs::create_dir_all(&package_dir).expect("package dir");
    let quarantine = node_modules.join(".kiana-native-quarantine");
    std::fs::create_dir_all(&quarantine).expect("quarantine");

    cleanup_windows_self_update_quarantine(&package_dir);

    assert!(!quarantine.exists());
}

#[test]
fn removes_a_regular_file_at_the_quarantine_path() {
    let directory = tempfile::tempdir().expect("temp dir");
    let package_dir = directory.path().join("node_modules").join("pkg");
    std::fs::create_dir_all(&package_dir).expect("package dir");
    let quarantine = node_modules_quarantine(directory.path());
    // `rmSync(path, { recursive: true, force: true })` 也会删除同名的普通文件；
    // `remove_dir_all` 只会返回 ENOTDIR 并把它留在原地。
    std::fs::write(&quarantine, b"stray").expect("file at quarantine path");

    cleanup_windows_self_update_quarantine(&package_dir);

    assert!(!quarantine.exists());
    assert!(directory.path().join("node_modules").exists());
}

#[test]
fn ignores_missing_quarantine_and_paths_without_node_modules() {
    let directory = tempfile::tempdir().expect("temp dir");
    let package_dir = directory.path().join("plain").join("package");
    std::fs::create_dir_all(&package_dir).expect("package dir");
    let marker = package_dir.join("keep.txt");
    std::fs::write(&marker, b"keep").expect("marker");

    // 没有 node_modules 祖先 → 直接返回；没有隔离区 → 删除是 no-op。
    cleanup_windows_self_update_quarantine(&package_dir);

    assert!(marker.exists());
}

#[test]
fn resolves_dotdot_lexically_before_walking_up() {
    // TS 用 `path.resolve(packageDir)`：`<root>/node_modules/../y` 解析为 `<root>/y`，
    // 其祖先里没有 node_modules，因此不会碰 `<root>/node_modules/.kiana-native-quarantine`。
    // （`std::path::absolute` 保留 `..`，会把 `node_modules/..` 的 file_name 当成
    //  `node_modules` 的父级中的 `..` 后继续向上，最终错误地命中 node_modules。）
    let directory = tempfile::tempdir().expect("temp dir");
    let node_modules = directory.path().join("node_modules");
    let quarantine = node_modules.join(".kiana-native-quarantine");
    std::fs::create_dir_all(&quarantine).expect("quarantine");
    let sibling = directory.path().join("y");
    std::fs::create_dir_all(&sibling).expect("sibling");

    let package_dir = node_modules.join("..").join("y");
    cleanup_windows_self_update_quarantine(&package_dir);

    assert!(quarantine.exists());
}

#[test]
fn quarantine_is_a_documented_no_op() {
    let directory = tempfile::tempdir().expect("temp dir");
    let package_dir = directory.path().join("node_modules").join("pkg");
    std::fs::create_dir_all(&package_dir).expect("package dir");
    let native = package_dir.join("native.node");
    std::fs::write(&native, b"binary").expect("native addon");

    quarantine_windows_native_dependencies(&package_dir);

    // Rust 进程没有 JS native addon：loadedFiles 恒为空 → TS 也会提前返回。
    assert!(native.exists());
    assert!(!node_modules_quarantine(directory.path()).exists());
}
