//! `test/native-module-path.test.ts` 的移植（2 个 it）。
//!
//! TS 的 `join`/`resolve` 会规范化 `..`；Rust 的 `Path::join` 不会。为了比较“同一路径”，
//! 测试先把两边都按 `node:path` 的规则做词法规范化（[`node_join`]），这也是 TS 里
//! `assert.equal(join(...), candidates[0])` 的实际语义。

use std::path::{Component, Path, PathBuf};
use std::sync::Mutex;

use kiana_tui::native::get_native_module_candidates;
use kiana_tui::native::module_path::{NativeModuleCandidateOptions, TUI_PACKAGE_NAME};

/// `node:path.join`：拼接后词法规范化（`a/../b` → `b`，`/..` → `/`）。
fn node_join(parts: &[&Path]) -> PathBuf {
    let mut joined = PathBuf::new();
    for part in parts {
        joined.push(part);
    }
    normalize(&joined)
}

fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// TS 的 `resolve("virtual", ...)` 是 cwd 相关的绝对路径；这里用 `/virtual` 代替，
/// 断言只依赖相对结构。
fn virtual_path(path: &str) -> PathBuf {
    Path::new("/virtual").join(path)
}

/// `resolves native helpers from the installed TUI package when the module is bundled elsewhere`
#[test]
fn resolves_native_helpers_from_the_installed_tui_package_when_the_module_is_bundled_elsewhere() {
    let package_root = virtual_path("node_modules/@earendil-works/kiana-tui");
    let bundled_module = virtual_path("kiana-coding-agent/dist/bundle/chunks/chunk.js");
    let native_path = Path::new("native/win32/prebuilds/win32-arm64/win32-platform.node");

    let seen: Mutex<Vec<String>> = Mutex::new(Vec::new());
    let resolve_package = |specifier: &str| {
        seen.lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .push(specifier.to_string());
        Some(package_root.join("dist/index.js"))
    };
    let options = NativeModuleCandidateOptions {
        module_dir: Some(
            bundled_module
                .parent()
                .expect("chunk.js has a parent")
                .to_path_buf(),
        ),
        exec_path: Some(virtual_path("node/node.exe")),
        resolve_package: Some(&resolve_package),
    };

    let candidates = get_native_module_candidates(native_path, &options);
    let normalized: Vec<PathBuf> = candidates
        .iter()
        .map(|candidate| normalize(candidate))
        .collect();

    assert_eq!(
        seen.lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .as_slice(),
        [TUI_PACKAGE_NAME]
    );
    assert_eq!(
        normalized[0],
        node_join(&[&package_root, native_path]),
        "the installed package location comes first"
    );
    assert!(
        normalized.contains(&node_join(&[
            bundled_module.parent().expect("parent"),
            Path::new(".."),
            native_path,
        ])),
        "the bundled module's parent directory is also a candidate"
    );
}

/// `keeps standalone binary fallbacks when the TUI package is unavailable`
#[test]
fn keeps_standalone_binary_fallbacks_when_the_tui_package_is_unavailable() {
    let bundled_module = virtual_path("kiana/bundle/chunks/chunk.js");
    let exec_path = virtual_path("kiana/kiana.exe");
    let native_path = Path::new("native/darwin/prebuilds/darwin-arm64/darwin-platform.node");

    let resolve_called = Mutex::new(false);
    let resolve_package = |_: &str| {
        *resolve_called
            .lock()
            .unwrap_or_else(|poison| poison.into_inner()) = true;
        None // TS: throw new Error("not installed")
    };
    let options = NativeModuleCandidateOptions {
        module_dir: Some(
            bundled_module
                .parent()
                .expect("chunk.js has a parent")
                .to_path_buf(),
        ),
        exec_path: Some(exec_path.clone()),
        resolve_package: Some(&resolve_package),
    };

    let candidates = get_native_module_candidates(native_path, &options);
    let normalized: Vec<PathBuf> = candidates
        .iter()
        .map(|candidate| normalize(candidate))
        .collect();

    assert!(
        *resolve_called
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
    );
    assert_eq!(
        normalized,
        vec![
            node_join(&[
                bundled_module.parent().expect("parent"),
                Path::new(".."),
                native_path,
            ]),
            node_join(&[bundled_module.parent().expect("parent"), native_path]),
            node_join(&[exec_path.parent().expect("parent"), native_path]),
        ]
    );
}

/// `Array.from(new Set(...))`：重复项按首次出现保留。
#[test]
fn deduplicates_candidates_preserving_first_occurrence() {
    let module_dir = virtual_path("kiana/bundle");
    let options = NativeModuleCandidateOptions {
        module_dir: Some(module_dir.clone()),
        // moduleDir 与可执行文件目录相同：第 2、3 项必然重复。
        exec_path: Some(module_dir.join("kiana")),
        resolve_package: None,
    };

    let candidates = get_native_module_candidates(
        Path::new("native/linux/prebuilds/linux-x64/x.node"),
        &options,
    );
    let normalized: Vec<PathBuf> = candidates
        .iter()
        .map(|candidate| normalize(candidate))
        .collect();

    assert_eq!(
        normalized,
        vec![
            node_join(&[
                &module_dir,
                Path::new(".."),
                Path::new("native/linux/prebuilds/linux-x64/x.node")
            ]),
            node_join(&[
                &module_dir,
                Path::new("native/linux/prebuilds/linux-x64/x.node")
            ]),
        ]
    );
}

/// `resolvePackage` 拿到的是 `TUI_PACKAGE_NAME`（TS 里同样的断言），且 `None` 时只跳过第一项。
#[test]
fn resolve_package_none_skips_the_package_candidate() {
    let options = NativeModuleCandidateOptions {
        module_dir: Some(virtual_path("pkg/dist/tui")),
        exec_path: Some(virtual_path("usr/local/bin/kiana")),
        resolve_package: Some(&|_specifier: &str| None),
    };

    let candidates = get_native_module_candidates(Path::new("native/x.node"), &options);
    assert_eq!(candidates.len(), 3);
    assert_eq!(
        normalize(&candidates[0]),
        virtual_path("pkg/dist/native/x.node")
    );
}

/// 默认 `moduleDir` 是**可执行文件所在目录**（TS 的 `dirname(fileURLToPath(import.meta.url))`），
/// 不是可执行文件本身的路径；候选按 `join` 规范化后去重。
#[test]
fn default_module_dir_is_the_executable_directory() {
    let exe = std::env::current_exe().expect("current exe");
    let exe_dir = exe.parent().expect("current exe has a parent");
    let options = NativeModuleCandidateOptions {
        module_dir: None,
        exec_path: None,
        resolve_package: None,
    };
    let candidates = get_native_module_candidates(Path::new("native/x.node"), &options);

    // 没有已安装的包；`moduleDir/..`、`moduleDir`、可执行文件目录，其中后两项相同 → 2 项。
    assert_eq!(candidates.len(), 2);
    assert_eq!(
        candidates[0],
        node_join(&[exe_dir, Path::new(".."), Path::new("native/x.node")])
    );
    assert_eq!(
        candidates[1],
        node_join(&[exe_dir, Path::new("native/x.node")])
    );
}

/// `node:path.join` 的规范化：`..` 消解、相对路径开头的 `..` 保留、绝对路径片段不重置前缀。
#[test]
fn candidates_are_normalized_like_node_path_join() {
    let options = NativeModuleCandidateOptions {
        module_dir: Some(Path::new("/a/b/c").to_path_buf()),
        exec_path: Some("/usr/bin/kiana".into()),
        resolve_package: Some(&|_specifier: &str| {
            Some(Path::new("/pkg/dist/index.js").to_path_buf())
        }),
    };
    let candidates = get_native_module_candidates(Path::new("native/x.node"), &options);
    assert_eq!(
        candidates,
        vec![
            PathBuf::from("/pkg/native/x.node"),
            PathBuf::from("/a/b/native/x.node"),
            PathBuf::from("/a/b/c/native/x.node"),
            PathBuf::from("/usr/bin/native/x.node"),
        ]
    );
}
