//! 移植 `packages/coding-agent/test/git-ssh-url.test.ts`，并用 TS 参考实现生成的黄金语料
//! 锁定 `hosted-git-info` 的怪异行为（`test/fixtures/git_url_corpus.json`）。

use kiana_core::git::{GitSource, parse_git_url};
use serde_json::Value;

fn source(repo: &str, host: &str, path: &str) -> GitSource {
    GitSource {
        source_type: "git".to_string(),
        repo: repo.to_string(),
        host: host.to_string(),
        path: path.to_string(),
        r#ref: None,
        pinned: false,
    }
}

fn with_ref(mut source: GitSource, r#ref: &str) -> GitSource {
    source.r#ref = Some(r#ref.to_string());
    source.pinned = true;
    source
}

#[test]
fn should_parse_https_url() {
    let result = parse_git_url("https://github.com/user/repo").expect("https url parses");
    assert_eq!(
        result,
        source("https://github.com/user/repo", "github.com", "user/repo")
    );
}

#[test]
fn should_parse_ssh_url() {
    let result = parse_git_url("ssh://git@github.com/user/repo").expect("ssh url parses");
    assert_eq!(
        result,
        source("ssh://git@github.com/user/repo", "github.com", "user/repo")
    );
}

#[test]
fn should_parse_protocol_url_with_ref() {
    let result = parse_git_url("https://github.com/user/repo@v1.0.0").expect("url with ref parses");
    assert_eq!(
        result,
        with_ref(
            source("https://github.com/user/repo", "github.com", "user/repo"),
            "v1.0.0"
        )
    );
}

#[test]
fn should_parse_git_at_host_path_with_git_prefix() {
    let result = parse_git_url("git:git@github.com:user/repo").expect("scp-like url parses");
    assert_eq!(
        result,
        source("git@github.com:user/repo", "github.com", "user/repo")
    );
}

#[test]
fn should_parse_host_path_shorthand_with_git_prefix() {
    let result = parse_git_url("git:github.com/user/repo").expect("shorthand parses");
    assert_eq!(
        result,
        source("https://github.com/user/repo", "github.com", "user/repo")
    );
}

#[test]
fn should_parse_shorthand_with_ref_and_git_prefix() {
    let result =
        parse_git_url("git:git@github.com:user/repo@v1.0.0").expect("shorthand with ref parses");
    assert_eq!(
        result,
        with_ref(
            source("git@github.com:user/repo", "github.com", "user/repo"),
            "v1.0.0"
        )
    );
}

#[test]
fn should_reject_unsafe_git_install_path_inputs() {
    for source_text in [
        "git:git@evil.example:../../victim/repo",
        "https://evil.example/..%2F..%2Fvictim/repo",
        "https://evil.example/..%2F..%2Fvictim/repo%",
        "git:git@evil.example:/absolute/repo",
        "git:git@evil.example:user\\repo/name",
        "git:git@evil.example:user/repo\0name",
    ] {
        assert_eq!(
            parse_git_url(source_text),
            None,
            "expected null for {source_text:?}"
        );
    }
}

#[test]
fn should_reject_git_at_host_path_without_git_prefix() {
    assert_eq!(parse_git_url("git@github.com:user/repo"), None);
}

#[test]
fn should_reject_host_path_shorthand_without_git_prefix() {
    assert_eq!(parse_git_url("github.com/user/repo"), None);
}

#[test]
fn should_reject_user_repo_shorthand() {
    assert_eq!(parse_git_url("user/repo"), None);
}

#[test]
fn scp_like_host_part_allows_line_breaks_like_the_ts_regex() {
    // 评审回归（finding：SCP_LIKE_RE 用了 `[^:\r\n]`）：TS 的 `/^git@([^:]+):(.+)$/`
    // 里 `[^:]` 允许换行/回车（否定字符类包含行终止符），只有 `(.+)` 排除它们。
    let result = parse_git_url("git:git@host\n:user/repo").expect("newline in host is allowed");
    assert_eq!(
        result,
        source("git@host\n:user/repo", "host\n", "user/repo")
    );

    let result = parse_git_url("git:git@ho\rst:user/repo").expect("carriage return in host");
    assert_eq!(
        result,
        source("git@ho\rst:user/repo", "ho\rst", "user/repo")
    );

    // 路径部分不能含行终止符（JS 的 `.` 不匹配 \n/\r/U+2028/U+2029）。
    assert_eq!(parse_git_url("git:git@host:user/repo\nname"), None);
    assert_eq!(parse_git_url("git:git@host:user\u{2028}repo"), None);
}

/// 黄金语料：全部来自 TS `parseGitUrl` 的真实输出（含 `hosted-git-info` 的怪癖）。
#[test]
fn matches_typescript_reference_corpus() {
    let fixture: Value = serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/git_url_corpus.json"
    )))
    .expect("valid git url corpus fixture");

    let mut failures = Vec::new();
    for case in fixture.as_array().expect("fixture is an array") {
        let input = case["input"].as_str().expect("input string");
        let expected = case["result"].clone();
        let actual = match parse_git_url(input) {
            Some(parsed) => serde_json::to_value(&parsed).expect("GitSource serializes"),
            None => Value::Null,
        };
        if actual != expected {
            failures.push(format!("{input:?}: expected {expected}, got {actual}"));
        }
    }

    assert!(
        failures.is_empty(),
        "{} git url case(s) failed:\n{}",
        failures.len(),
        failures.join("\n")
    );
}
