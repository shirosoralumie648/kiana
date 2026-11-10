//! `utils/changelog.ts` 的移植测试（`test/changelog.test.ts` 的全部用例 + 源码行为补充）。
//!
//! 链接改写的期望值来自 TS 参考实现（`node` 直接跑 `normalizeChangelogLinks`），
//! `parseChangelog` 的 fixture 与输出也逐字取自参考实现。

use std::path::Path;

use kiana_core::changelog::{
    ChangelogEntry, compare_versions, get_new_entries, normalize_changelog_links, parse_changelog,
};

fn entry() -> ChangelogEntry {
    ChangelogEntry {
        major: 0,
        minor: 79,
        patch: 0,
        content: String::new(),
    }
}

#[test]
fn rewrites_package_relative_links_to_tag_pinned_source_links() {
    let markdown = [
        "[Project Trust](README.md#project-trust)",
        "[Extensions](docs/extensions.md#project_trust)",
        "[Examples](examples/extensions/)",
        "[Root README](../../README.md#supply-chain-hardening)",
    ]
    .join("\n");

    assert_eq!(
        normalize_changelog_links(&markdown, &entry()),
        [
            "[Project Trust](https://github.com/earendil-works/kiana/blob/v0.79.0/packages/coding-agent/README.md#project-trust)",
            "[Extensions](https://github.com/earendil-works/kiana/blob/v0.79.0/packages/coding-agent/docs/extensions.md#project_trust)",
            "[Examples](https://github.com/earendil-works/kiana/tree/v0.79.0/packages/coding-agent/examples/extensions/)",
            "[Root README](https://github.com/earendil-works/kiana/blob/v0.79.0/README.md#supply-chain-hardening)",
        ]
        .join("\n")
    );
}

#[test]
fn canonicalizes_old_repository_urls_without_changing_external_links() {
    let markdown = [
        "[#5167](https://github.com/earendil-works/kiana-mono/pull/5167)",
        "[#4163](https://github.com/badlogic/kiana-mono/issues/4163)",
        "[Agent README](https://github.com/badlogic/kiana-mono/blob/main/packages/agent/README.md)",
        "[External](https://example.com/docs)",
        "[Local anchor](#settings)",
    ]
    .join("\n");

    assert_eq!(
        normalize_changelog_links(&markdown, "0.79.0"),
        [
            "[#5167](https://github.com/earendil-works/kiana/pull/5167)",
            "[#4163](https://github.com/earendil-works/kiana/issues/4163)",
            "[Agent README](https://github.com/earendil-works/kiana/blob/v0.79.0/packages/agent/README.md)",
            "[External](https://example.com/docs)",
            "[Local anchor](#settings)",
        ]
        .join("\n")
    );
}

/// 期望值由 TS 参考实现逐条生成（`normalizeChangelogLinks(case, "1.2.3")`）。
#[test]
fn matches_reference_link_rewriting_table() {
    let cases = [
        (
            "[a](docs/x.md?raw=1#frag)",
            "[a](https://github.com/earendil-works/kiana/blob/v1.2.3/packages/coding-agent/docs/x.md?raw=1#frag)",
        ),
        (
            "[x](/packages/agent/README.md)",
            "[x](https://github.com/earendil-works/kiana/blob/v1.2.3/packages/agent/README.md)",
        ),
        ("[x](../../../README.md)", "[x](../../../README.md)"),
        (
            "[x](docs/new%20file.md)",
            "[x](https://github.com/earendil-works/kiana/blob/v1.2.3/packages/coding-agent/docs/new%2520file.md)",
        ),
        (
            "[x](foo)",
            "[x](https://github.com/earendil-works/kiana/tree/v1.2.3/packages/coding-agent/foo)",
        ),
        (
            "[x](./foo/)",
            "[x](https://github.com/earendil-works/kiana/tree/v1.2.3/packages/coding-agent/foo/)",
        ),
        (
            "[x](a\\b\\c.md)",
            "[x](https://github.com/earendil-works/kiana/blob/v1.2.3/packages/coding-agent/a/b/c.md)",
        ),
        (
            "![img](pic.png)",
            "![img](https://github.com/earendil-works/kiana/blob/v1.2.3/packages/coding-agent/pic.png)",
        ),
        (
            "[x](a.md \"title\")",
            "[x](https://github.com/earendil-works/kiana/blob/v1.2.3/packages/coding-agent/a.md \"title\")",
        ),
        ("[x](#anchor)", "[x](#anchor)"),
        ("[x](//example.com/a)", "[x](//example.com/a)"),
        (
            "[x](https://github.com/badlogic/kiana-mono)",
            "[x](https://github.com/earendil-works/kiana)",
        ),
        (
            "[x](https://github.com/badlogic/kiana-mono-other/a)",
            "[x](https://github.com/badlogic/kiana-mono-other/a)",
        ),
        (
            "[x](https://github.com/earendil-works/kiana-mono/tree/main/packages/agent/)",
            "[x](https://github.com/earendil-works/kiana/tree/v1.2.3/packages/agent/)",
        ),
        (
            "[x](docs/../README.md)",
            "[x](https://github.com/earendil-works/kiana/blob/v1.2.3/packages/coding-agent/README.md)",
        ),
        ("[x](?q=1)", "[x](?q=1)"),
        ("[x](#f?q)", "[x](#f?q)"),
        (
            "[x](https://github.com/earendil-works/kiana/blob/master/a/b.md)",
            "[x](https://github.com/earendil-works/kiana/blob/v1.2.3/a/b.md)",
        ),
        (
            "[x](https://github.com/earendil-works/kiana/blob/feature/a/b.md)",
            "[x](https://github.com/earendil-works/kiana/blob/feature/a/b.md)",
        ),
    ];

    for (input, expected) in cases {
        assert_eq!(
            normalize_changelog_links(input, "1.2.3"),
            expected,
            "input: {input}"
        );
    }
}

#[test]
fn keeps_text_outside_links_untouched_and_handles_multiple_links() {
    let markdown = "intro [a](a.md) middle [b](dir/) tail";
    assert_eq!(
        normalize_changelog_links(markdown, "1.0.0"),
        "intro [a](https://github.com/earendil-works/kiana/blob/v1.0.0/packages/coding-agent/a.md) middle [b](https://github.com/earendil-works/kiana/tree/v1.0.0/packages/coding-agent/dir/) tail"
    );
}

#[test]
fn parses_changelog_entries_including_the_heading_line() {
    let directory = tempfile::tempdir().expect("temp dir");
    let path = directory.path().join("CHANGELOG.md");
    std::fs::write(
        &path,
        "# Changelog\n\nSome intro.\n\n## [1.2.0] - 2026-01-01\n\n- Added thing\n- Fixed [link](docs/x.md#frag)\n\n## 1.1.0\n\n### Section\ntext\n\n## Not a version\n\nignored\n\n## [1.0.0] first release\nbody\n",
    )
    .expect("write changelog");

    assert_eq!(
        parse_changelog(&path),
        vec![
            ChangelogEntry {
                major: 1,
                minor: 2,
                patch: 0,
                content: "## [1.2.0] - 2026-01-01\n\n- Added thing\n- Fixed [link](docs/x.md#frag)"
                    .to_string(),
            },
            ChangelogEntry {
                major: 1,
                minor: 1,
                patch: 0,
                content: "## 1.1.0\n\n### Section\ntext".to_string(),
            },
            ChangelogEntry {
                major: 1,
                minor: 0,
                patch: 0,
                content: "## [1.0.0] first release\nbody".to_string(),
            },
        ]
    );
}

#[test]
fn returns_empty_for_missing_or_empty_changelog() {
    let directory = tempfile::tempdir().expect("temp dir");
    assert!(parse_changelog(&directory.path().join("missing.md")).is_empty());

    let path = directory.path().join("empty.md");
    std::fs::write(&path, "").expect("write empty");
    assert!(parse_changelog(&path).is_empty());
}

/// `existsSync` 对目录为真，`readFileSync` 抛 EISDIR（TS 记日志后返回 `[]`）。
#[test]
fn returns_empty_for_a_directory_path() {
    let directory = tempfile::tempdir().expect("temp dir");
    assert!(parse_changelog(directory.path()).is_empty());
}

#[test]
fn tolerates_invalid_utf8_like_node_utf8_decoding() {
    let directory = tempfile::tempdir().expect("temp dir");
    let path = directory.path().join("CHANGELOG.md");
    std::fs::write(&path, b"## 1.0.0\n\n- bad \xFF byte\n").expect("write bytes");
    let entries = parse_changelog(&path);
    assert_eq!(entries.len(), 1);
    assert!(entries[0].content.contains('\u{FFFD}'));
}

#[test]
fn compares_versions_by_major_minor_patch() {
    let a = ChangelogEntry {
        major: 1,
        minor: 2,
        patch: 3,
        content: String::new(),
    };
    let b = ChangelogEntry {
        major: 1,
        minor: 2,
        patch: 4,
        content: String::new(),
    };
    assert_eq!(compare_versions(&a, &b), -1);
    assert_eq!(compare_versions(&b, &a), 1);
    assert_eq!(compare_versions(&a, &a), 0);
    let c = ChangelogEntry {
        major: 2,
        minor: 0,
        patch: 0,
        content: String::new(),
    };
    assert_eq!(compare_versions(&c, &a), 1);
}

fn sample_entries() -> Vec<ChangelogEntry> {
    vec![
        ChangelogEntry {
            major: 0,
            minor: 80,
            patch: 0,
            content: "a".to_string(),
        },
        ChangelogEntry {
            major: 0,
            minor: 79,
            patch: 0,
            content: "b".to_string(),
        },
        ChangelogEntry {
            major: 0,
            minor: 78,
            patch: 0,
            content: "c".to_string(),
        },
    ]
}

#[test]
fn selects_entries_newer_than_last_version() {
    let entries = sample_entries();
    let version = |value: &str| -> Vec<String> {
        get_new_entries(&entries, value)
            .into_iter()
            .map(|entry| entry.content)
            .collect()
    };
    assert_eq!(version("0.79.0"), vec!["a"]);
    assert_eq!(version("0.79"), vec!["a"]);
    // `Number("abc")` 是 NaN → `|| 0` → 0.0，因此所有 minor > 0 的条目都算“更新”。
    assert_eq!(version("abc"), vec!["a", "b", "c"]);
    // 多余段被忽略。
    assert_eq!(version("0.79.0.5"), vec!["a"]);
    assert_eq!(version("0.80.0"), Vec::<String>::new());
}

#[test]
fn parse_changelog_accepts_path_like_objects() {
    let directory = tempfile::tempdir().expect("temp dir");
    let path = directory.path().join("CHANGELOG.md");
    std::fs::write(&path, "## 2.0.0\nline\n").expect("write");
    let entries = parse_changelog(Path::new(&path));
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].major, 2);
    assert_eq!(entries[0].content, "## 2.0.0\nline");
}
