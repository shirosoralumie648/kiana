//! `utils/kiana-user-agent.ts` 的移植测试（`test/kiana-user-agent.test.ts`）。
//!
//! TS 断言 `kiana/1.2.3 (<platform>; <node|bun version>; <arch>)`。Rust 进程没有
//! `process.versions`，运行时由 `kiana-ai` 探测宿主 `bun`/`node`；探测不到时是 `(browser)`。
//! 因此这里同时断言“与 `format_user_agent` 的直接调用一致”、前缀 `kiana/1.2.3 `，并按
//! `runtime_description()` 的 `Some`/`None` 两支断言完整格式。

use kiana_ai::utils::identity::{format_user_agent, node_arch, node_platform, runtime_description};
use kiana_ai::{ClientIdentity, UserAgentMode, UserAgentModeDetailed};
use kiana_core::changelog::get_kiana_user_agent;

fn detailed_identity(version: &str) -> ClientIdentity {
    ClientIdentity {
        user_agent: Some(UserAgentMode::Detailed(UserAgentModeDetailed::Detailed)),
        attribution: None,
        version: Some(version.to_string()),
    }
}

#[test]
fn formats_the_user_agent_expected_by_kiana_dev() {
    let user_agent = get_kiana_user_agent("1.2.3");
    assert_eq!(
        user_agent,
        format_user_agent(Some(&detailed_identity("1.2.3")))
    );
    assert!(
        user_agent.starts_with("kiana/1.2.3 "),
        "unexpected user agent {user_agent}"
    );

    match runtime_description() {
        Some(runtime) => {
            assert_eq!(
                user_agent,
                format!(
                    "kiana/1.2.3 ({}; {runtime}; {})",
                    node_platform(),
                    node_arch()
                )
            );
            let pattern = regex::Regex::new(r"^kiana/[^\s()]+ \([^;()]+;\s*[^;()]+;\s*[^()]+\)$")
                .expect("valid regex");
            assert!(
                pattern.is_match(&user_agent),
                "user agent does not match the TS expectation: {user_agent}"
            );
        }
        None => assert_eq!(user_agent, "kiana/1.2.3 (browser)"),
    }
}

#[test]
fn keeps_the_given_version_verbatim() {
    let user_agent = get_kiana_user_agent("0.0.0-test");
    assert!(user_agent.starts_with("kiana/0.0.0-test "));
}
