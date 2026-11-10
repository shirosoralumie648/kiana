//! `utils/paths.ts` 的移植测试（`test/paths.test.ts`）。
//!
//! 每个测试文件自带 helper，不建跨单元共享模块（§7.0 并行规则）。

use std::fs;
use std::path::{Path, PathBuf};

use kiana_core::paths::{
    PathInputOptions, canonicalize_path, format_path_relative_to_cwd_or_absolute,
    get_cwd_relative_path, get_file_revision, is_local_path, normalize_path,
    normalize_windows_shell_path, resolve_path,
};
use tempfile::TempDir;

fn temp_dir() -> TempDir {
    tempfile::Builder::new()
        .prefix("kiana-paths-")
        .tempdir()
        .expect("temp dir")
}

fn s(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

fn default_options() -> PathInputOptions {
    PathInputOptions::default()
}

// =============================================================================
// canonicalizePath
// =============================================================================

#[test]
fn canonicalize_returns_real_path_for_regular_file() {
    let dir = temp_dir();
    let file = dir.path().join("file.txt");
    fs::write(&file, "hello").unwrap();
    assert_eq!(
        canonicalize_path(&s(&file)),
        s(&fs::canonicalize(&file).unwrap())
    );
}

#[cfg(unix)]
#[test]
fn canonicalize_resolves_symlinks() {
    use std::os::unix::fs::symlink;
    let dir = temp_dir();
    let target = dir.path().join("target.txt");
    let link = dir.path().join("link.txt");
    fs::write(&target, "hello").unwrap();
    symlink(&target, &link).unwrap();
    assert_eq!(
        canonicalize_path(&s(&link)),
        s(&fs::canonicalize(&target).unwrap())
    );

    let target_dir = dir.path().join("target-dir");
    let link_dir = dir.path().join("link-dir");
    fs::create_dir(&target_dir).unwrap();
    symlink(&target_dir, &link_dir).unwrap();
    assert_eq!(
        canonicalize_path(&s(&link_dir)),
        s(&fs::canonicalize(&target_dir).unwrap())
    );
}

#[test]
fn canonicalize_falls_back_to_raw_path_when_missing() {
    let dir = temp_dir();
    let nonexistent = dir.path().join("no-such-file");
    assert_eq!(canonicalize_path(&s(&nonexistent)), s(&nonexistent));
}

#[cfg(unix)]
#[test]
fn canonicalize_falls_back_for_dangling_symlink() {
    use std::os::unix::fs::symlink;
    let dir = temp_dir();
    let target = dir.path().join("target.txt");
    let link = dir.path().join("link.txt");
    symlink(&target, &link).unwrap();
    assert_eq!(canonicalize_path(&s(&link)), s(&link));
}

// =============================================================================
// getCwdRelativePath
// =============================================================================

#[test]
fn cwd_relative_keeps_names_starting_with_dots() {
    let cwd = std::env::temp_dir().join("kiana-paths-cwd");
    let file = cwd.join("..config").join("AGENTS.md");
    let expected = Path::new("..config").join("AGENTS.md");
    assert_eq!(
        get_cwd_relative_path(&s(&file), &s(&cwd)),
        Some(s(&expected))
    );
}

#[test]
fn cwd_relative_rejects_parent_traversal() {
    let cwd = std::env::temp_dir().join("kiana-paths-cwd");
    let file = cwd.join("..").join("AGENTS.md");
    assert_eq!(get_cwd_relative_path(&s(&file), &s(&cwd)), None);
}

#[test]
fn cwd_relative_returns_dot_for_the_cwd_itself() {
    let cwd = std::env::temp_dir().join("kiana-paths-cwd");
    assert_eq!(
        get_cwd_relative_path(&s(&cwd), &s(&cwd)),
        Some(".".to_string())
    );
}

#[test]
fn format_relative_path_uses_forward_slashes() {
    let cwd = std::env::temp_dir().join("kiana-paths-cwd");
    let file = cwd.join("sub").join("file.txt");
    assert_eq!(
        format_path_relative_to_cwd_or_absolute(&s(&file), &s(&cwd)),
        "sub/file.txt"
    );
    // `formatPathRelativeToCwdOrAbsolute` 返回**解析后**的绝对路径。
    let outside = cwd.parent().expect("parent dir").join("other.txt");
    assert_eq!(
        format_path_relative_to_cwd_or_absolute(&s(&outside), &s(&cwd)),
        s(&outside)
    );
}

// =============================================================================
// resolvePath / normalizePath
// =============================================================================

#[test]
fn expands_only_home_tilde_shortcuts() {
    let home = dirs_home();
    assert_eq!(normalize_path("~", &default_options()), home);
    assert_eq!(
        normalize_path("~/file.txt", &default_options()),
        join_string(&home, "file.txt")
    );
    let cwd = std::env::temp_dir().join("kiana-paths-cwd");
    assert_eq!(
        resolve_path("~draft.md", &s(&cwd), &default_options()),
        join_string(&s(&cwd), "~draft.md")
    );
    assert_eq!(normalize_path("~draft.md", &default_options()), "~draft.md");
}

#[test]
fn resolves_relative_paths_against_the_base_directory() {
    let cwd = std::env::temp_dir().join("kiana-paths-cwd");
    assert_eq!(
        resolve_path("subdir/file.txt", &s(&cwd), &default_options()),
        join_string(&s(&cwd), "subdir/file.txt")
    );
    let base_url = url::Url::from_directory_path(&cwd).unwrap().to_string();
    assert_eq!(
        resolve_path("subdir/file.txt", &base_url, &default_options()),
        join_string(&s(&cwd), "subdir/file.txt")
    );
}

// Node 的 path.resolve 在最后把 cwd 拼到前面时结果仍是绝对路径；Rust 曾经丢掉前导 `/`。
#[test]
fn resolves_relative_or_empty_bases_to_absolute_paths() {
    let process_cwd = std::env::current_dir().expect("cwd");
    let process_cwd = s(&process_cwd);
    assert_eq!(
        resolve_path("x", "rel/base", &default_options()),
        join_string(&process_cwd, "rel/base/x")
    );
    assert_eq!(
        resolve_path("x", "", &default_options()),
        join_string(&process_cwd, "x")
    );
    assert_eq!(
        resolve_path("", "rel/base", &default_options()),
        join_string(&process_cwd, "rel/base")
    );
    // 下游：相对 cwd 的展示路径必须仍能算出相对形式，而不是被破坏的绝对路径。
    assert_eq!(
        format_path_relative_to_cwd_or_absolute("x", "rel/base"),
        "x"
    );
}

#[test]
fn accepts_file_urls() {
    let dir = temp_dir();
    let file_path = dir.path().join("file with spaces.txt");
    let file_url = url::Url::from_file_path(&file_path).unwrap().to_string();
    let base = dir.path().join("base");
    assert_eq!(
        resolve_path(&file_url, &s(&base), &default_options()),
        s(&file_path)
    );
}

#[test]
fn invalid_file_urls_are_returned_unchanged() {
    // 强制偏差：TS 抛 URIError，Rust 签名不可失败 → 原样返回（见模块文档）。
    for invalid in [
        "file:///%E0%A4%A",    // 非法百分号转义
        "file:///%FF",         // 解码后不是合法 UTF-8
        "file:///a%2Fb",       // 编码过的 '/'
        "file:///tmp/x%ZZ",    // pathname 里的非法转义
        "file://host/share/x", // 非 localhost 主机
    ] {
        assert_eq!(
            resolve_path(invalid, "/tmp", &default_options()),
            invalid,
            "resolve_path({invalid:?})"
        );
        assert_eq!(
            normalize_path(invalid, &default_options()),
            invalid,
            "normalize_path({invalid:?})"
        );
    }
}

// Node 的 fileURLToPath 只解码 pathname：query/fragment 里的 `%` 与 `%2F` 不影响结果。
#[test]
fn file_url_checks_and_decodes_only_the_pathname() {
    for url in [
        "file:///tmp/x?q=%zz",
        "file:///tmp/x#frag%zz",
        "file:///tmp/x?a=%2F",
        "file:///tmp/x?a=100%",
    ] {
        assert_eq!(
            resolve_path(url, "/tmp", &default_options()),
            "/tmp/x",
            "resolve_path({url:?})"
        );
        assert_eq!(
            normalize_path(url, &default_options()),
            "/tmp/x",
            "normalize_path({url:?})"
        );
    }
}

#[cfg(unix)]
#[test]
fn file_urls_follow_node_whatwg_pathnames() {
    // 额外前导斜杠与首段盘符的 `|` 归一（url::Url::to_file_path 会折叠它们）。
    assert_eq!(normalize_path("file:////x", &default_options()), "//x");
    // resolvePath 之后还会走 path.resolve，它的 normalize 会把 `//` 折叠成 `/`。
    assert_eq!(resolve_path("file:////x", "/tmp", &default_options()), "/x");
    assert_eq!(normalize_path("file:///c|/x", &default_options()), "/c:/x");
    assert_eq!(normalize_path("file://C|", &default_options()), "/C:");
    // `file:////c|/x` 有额外前导斜杠，首段不是盘符，不做 `|` 归一。
    assert_eq!(
        normalize_path("file:////c|/x", &default_options()),
        "//c|/x"
    );
    assert_eq!(
        normalize_path("file:///a%2fb", &default_options()),
        "file:///a%2fb"
    );
}

#[cfg(unix)]
#[test]
fn preserves_posix_absolute_paths_with_literal_percent_sequences() {
    let dir = temp_dir();
    for file_path in [
        dir.path().join("report%2026.md"),
        dir.path().join("foo%2Fbar"),
        dir.path().join("malformed%A.md"),
    ] {
        assert_eq!(
            resolve_path(
                &s(&file_path),
                &s(&dir.path().join("base")),
                &default_options()
            ),
            s(&file_path)
        );
    }
}

#[test]
fn trim_is_opt_in() {
    let input = "  /tmp/x  ";
    assert_eq!(normalize_path(input, &default_options()), input);
    let trimmed = PathInputOptions {
        trim: true,
        ..Default::default()
    };
    assert_eq!(normalize_path(input, &trimmed), "/tmp/x");
}

#[test]
fn strip_at_prefix_and_unicode_spaces() {
    let options = PathInputOptions {
        strip_at_prefix: true,
        normalize_unicode_spaces: true,
        ..Default::default()
    };
    assert_eq!(normalize_path("@a\u{00A0}b", &options), "a b");
    assert_eq!(normalize_path("a b", &default_options()), "a b");
}

#[test]
fn expand_tilde_can_be_disabled_and_home_dir_overridden() {
    let options = PathInputOptions {
        expand_tilde: false,
        ..Default::default()
    };
    assert_eq!(normalize_path("~/x", &options), "~/x");

    let options = PathInputOptions {
        home_dir: Some(PathBuf::from("/custom/home")),
        ..Default::default()
    };
    assert_eq!(normalize_path("~", &options), "/custom/home");
    assert_eq!(normalize_path("~/x", &options), "/custom/home/x");
}

#[test]
fn normalizes_dot_and_dotdot_segments_lexically() {
    let cwd = "/a/b";
    assert_eq!(resolve_path("../c", cwd, &default_options()), "/a/c");
    assert_eq!(resolve_path("./c/../d", cwd, &default_options()), "/a/b/d");
    assert_eq!(resolve_path("", cwd, &default_options()), "/a/b");
    assert_eq!(resolve_path("/x//y/", cwd, &default_options()), "/x/y");
}

// =============================================================================
// normalizeWindowsShellPath
// =============================================================================

#[test]
fn converts_git_bash_msys_cygwin_and_wsl_drive_paths() {
    assert_eq!(
        normalize_windows_shell_path("/c/Users/example/project"),
        "C:\\Users\\example\\project"
    );
    assert_eq!(normalize_windows_shell_path("/cygdrive/d/work"), "D:\\work");
    assert_eq!(normalize_windows_shell_path("/mnt/e/source"), "E:\\source");
    assert_eq!(normalize_windows_shell_path("/c"), "C:\\");
}

#[test]
fn leaves_other_windows_path_forms_unchanged() {
    for path in [
        "C:/Users/example",
        "C:\\Users\\example",
        "//server/share/file",
        "/c/Users\\example",
        "relative/file",
        "/tmp/file",
    ] {
        assert_eq!(normalize_windows_shell_path(path), path);
    }
}

// =============================================================================
// isLocalPath
// =============================================================================

#[test]
fn is_local_path_cases() {
    assert!(is_local_path("my-package"));
    assert!(is_local_path("./foo"));
    assert!(is_local_path("file:///tmp/foo"));
    assert!(!is_local_path("npm:package"));
    assert!(!is_local_path("git://repo"));
    assert!(!is_local_path("https://example.com"));
    assert!(!is_local_path("github:user/repo"));
    assert!(!is_local_path("ssh:host"));
    assert!(!is_local_path("builtin:foo"));
    assert!(is_local_path("  ./foo  "));
}

// =============================================================================
// getFileRevision
// =============================================================================

#[test]
fn file_revision_matches_dev_ino_size_mtime_ctime_shape() {
    let dir = temp_dir();
    let file = dir.path().join("revision.txt");
    fs::write(&file, "hello").unwrap();
    let revision = get_file_revision(&s(&file)).expect("revision");
    let parts: Vec<&str> = revision.split(':').collect();
    assert_eq!(parts.len(), 5, "revision was {revision}");
    for part in &parts {
        assert!(
            part.parse::<i128>().is_ok(),
            "non-numeric revision part {part:?} in {revision}"
        );
    }
    assert_eq!(parts[2], "5");
}

#[test]
fn file_revision_is_none_for_missing_paths() {
    let dir = temp_dir();
    assert_eq!(get_file_revision(&s(&dir.path().join("missing"))), None);
}

// =============================================================================
// helpers
// =============================================================================

fn dirs_home() -> String {
    dirs::home_dir()
        .map(|path| path.to_string_lossy().into_owned())
        .unwrap_or_default()
}

fn join_string(base: &str, part: &str) -> String {
    PathBuf::from(base)
        .join(part)
        .to_string_lossy()
        .into_owned()
}
