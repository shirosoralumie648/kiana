//! `packages/ai/test/oauth-page.test.ts` 等对 OAuth 回调页的逐字节断言。
//!
//! 期望值来自 `tests/fixtures/utils/oauth_page.json`，由 TS 参照实现（`upstream-reference`）生成；
//! 生成脚本见 `/home/ctyun/kiana-work/scratch/kiana-ai__utils/gen_oauth_golden.ts`。

use kiana_ai::utils::oauth_page::{oauth_error_html, oauth_success_html};
use serde_json::Value;

fn load_fixture() -> Value {
    let path = format!(
        "{}/tests/fixtures/utils/oauth_page.json",
        env!("CARGO_MANIFEST_DIR")
    );
    serde_json::from_str(&std::fs::read_to_string(path).expect("fixture readable"))
        .expect("fixture is valid JSON")
}

fn expected(fixture: &Value, key: &str) -> String {
    fixture[key].as_str().expect("fixture string").to_string()
}

#[test]
fn renders_the_same_bytes_as_the_typescript_page() {
    let fixture = load_fixture();
    assert_eq!(
        oauth_success_html("You can close this window."),
        expected(&fixture, "success")
    );
    assert_eq!(
        oauth_success_html("<b>Done</b> & \"thanks\" 'ok'"),
        expected(&fixture, "success_escaped")
    );
    assert_eq!(
        oauth_error_html("Sign in failed.", None),
        expected(&fixture, "error")
    );
    assert_eq!(
        oauth_error_html("Sign in failed.", Some("state mismatch\nsecond line")),
        expected(&fixture, "error_details")
    );
    assert_eq!(
        oauth_error_html("Sign in failed.", Some("")),
        expected(&fixture, "error_empty_details")
    );
}
