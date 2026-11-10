//! 移植 `test/path-utils.test.ts`。

use std::path::{Path, PathBuf};

use kiana_tools::path_utils::{
    expand_path, path_exists, resolve_read_path, resolve_read_path_async, resolve_to_cwd,
};

#[test]
fn expands_tilde_to_home_directory() {
    assert!(!expand_path("~").contains('~'));
    assert!(!expand_path("~/Documents/file.txt").contains("~/"));
}

#[test]
fn keeps_tilde_prefixed_filenames_literal() {
    assert_eq!(expand_path("~draft.md"), "~draft.md");
    assert_eq!(expand_path("@~draft.md"), "~draft.md");
}

#[test]
fn normalizes_unicode_spaces() {
    let with_nbsp = "file\u{00A0}name.txt";
    assert_eq!(expand_path(with_nbsp), "file name.txt");
}

#[test]
fn resolves_absolute_paths_as_is() {
    let temp = tempfile::tempdir().expect("tempdir");
    let absolute = temp.path().join("absolute").join("path").join("file.txt");
    let result = resolve_to_cwd(
        &absolute.to_string_lossy(),
        &temp.path().join("some").join("cwd").to_string_lossy(),
    );
    assert_eq!(result, absolute);
}

#[test]
fn resolves_relative_paths_against_cwd() {
    let temp = tempfile::tempdir().expect("tempdir");
    let cwd = temp.path().join("cwd");
    let result = resolve_to_cwd("relative/file.txt", &cwd.to_string_lossy());
    assert_eq!(result, cwd.join("relative").join("file.txt"));
}

#[test]
fn resolves_tilde_prefixed_filenames_against_cwd() {
    let temp = tempfile::tempdir().expect("tempdir");
    let cwd = temp.path().join("kiana-path-utils-cwd");
    assert_eq!(
        resolve_to_cwd("~draft.md", &cwd.to_string_lossy()),
        cwd.join("~draft.md")
    );
    assert_eq!(
        resolve_to_cwd("@~draft.md", &cwd.to_string_lossy()),
        cwd.join("~draft.md")
    );
}

#[test]
fn resolves_existing_file_path() {
    let temp = tempfile::tempdir().expect("tempdir");
    std::fs::write(temp.path().join("test-file.txt"), "content").expect("write");
    let result = resolve_read_path("test-file.txt", &temp.path().to_string_lossy());
    assert_eq!(result, temp.path().join("test-file.txt"));
}

#[test]
fn resolves_nfc_input_to_nfd_file() {
    let temp = tempfile::tempdir().expect("tempdir");
    let nfd_name = "filee\u{0301}.txt";
    let nfc_name = "file\u{00E9}.txt";
    assert_ne!(nfd_name, nfc_name);
    std::fs::write(temp.path().join(nfd_name), "content").expect("write");
    let result = resolve_read_path(nfc_name, &temp.path().to_string_lossy());
    assert_eq!(result, temp.path().join(nfd_name));
}

#[test]
fn resolves_straight_quote_input_to_curly_quote_file() {
    let temp = tempfile::tempdir().expect("tempdir");
    let curly_name = "Capture d\u{2019}cran.txt";
    let straight_name = "Capture d'cran.txt";
    std::fs::write(temp.path().join(curly_name), "content").expect("write");
    let result = resolve_read_path(straight_name, &temp.path().to_string_lossy());
    assert_eq!(result, temp.path().join(curly_name));
}

#[test]
fn resolves_nfc_straight_quote_to_curly_quote_file() {
    let temp = tempfile::tempdir().expect("tempdir");
    let nfc_curly_name = "Capture d\u{2019}\u{00E9}cran.txt";
    let nfc_straight_name = "Capture d'\u{00E9}cran.txt";
    std::fs::write(temp.path().join(nfc_curly_name), "content").expect("write");
    let result = resolve_read_path(nfc_straight_name, &temp.path().to_string_lossy());
    assert_eq!(result, temp.path().join(nfc_curly_name));
}

#[test]
fn resolves_macos_screenshot_ampm_variant() {
    let temp = tempfile::tempdir().expect("tempdir");
    let macos_name = "Screenshot 2024-01-01 at 10.00.00\u{202F}AM.png";
    let user_name = "Screenshot 2024-01-01 at 10.00.00 AM.png";
    std::fs::write(temp.path().join(macos_name), "content").expect("write");
    let result = resolve_read_path(user_name, &temp.path().to_string_lossy());
    assert_eq!(result, temp.path().join(macos_name));
}

#[test]
fn resolves_macos_screenshot_lowercase_ampm_variant() {
    let temp = tempfile::tempdir().expect("tempdir");
    let macos_name = "Screenshot 2024-01-01 at 10.00.00\u{202F}am.png";
    let user_name = "Screenshot 2024-01-01 at 10.00.00 am.png";
    std::fs::write(temp.path().join(macos_name), "content").expect("write");
    let result = resolve_read_path(user_name, &temp.path().to_string_lossy());
    assert_eq!(result, temp.path().join(macos_name));
}

#[tokio::test]
async fn async_variants_behave_like_the_sync_versions() {
    let temp = tempfile::tempdir().expect("tempdir");
    let raw = temp.path().to_string_lossy();
    assert!(path_exists(temp.path()).await);
    assert!(!path_exists(&temp.path().join("missing")).await);

    let curly_name = "Capture d\u{2019}cran.txt";
    std::fs::write(temp.path().join(curly_name), "content").expect("write");
    let resolved = resolve_read_path_async("Capture d'cran.txt", &raw).await;
    assert_eq!(resolved, temp.path().join(curly_name));

    let plain = resolve_read_path_async("test-file.txt", &raw).await;
    assert_eq!(plain, temp.path().join("test-file.txt"));
}

#[test]
fn missing_file_returns_the_resolved_path() {
    let temp = tempfile::tempdir().expect("tempdir");
    let result = resolve_read_path("does-not-exist.txt", &temp.path().to_string_lossy());
    assert_eq!(result, temp.path().join("does-not-exist.txt"));
    assert!(Path::new(&result).is_absolute());
}

#[test]
fn expand_path_strips_at_prefix_and_normalizes_spaces_only() {
    assert_eq!(expand_path("@file name.txt"), "file name.txt");
    assert_eq!(expand_path("plain/path"), "plain/path");
    let _ = PathBuf::from("/");
}
