//! 移植 `packages/coding-agent/test/suite/regressions/8337-utf8-bom-parsing.test.ts`
//! 的 frontmatter / splitBom 部分（SettingsManager 的其余断言属于 `kiana-config` 的测试）。

use kiana_core::frontmatter::parse_frontmatter;
use kiana_core::text::split_bom;

#[test]
fn loads_frontmatter_with_a_leading_bom() {
    let split = split_bom("\u{FEFF}content");
    assert_eq!(split.bom, "\u{FEFF}");
    assert_eq!(split.text, "content");

    let document = "---\nname: demo\ndescription: Test\n---\nBody";
    let parsed = parse_frontmatter(&format!("\u{FEFF}{document}")).expect("valid frontmatter");
    assert_eq!(
        parsed
            .frontmatter
            .get("name")
            .and_then(|value| value.as_str()),
        Some("demo")
    );
    assert_eq!(
        parsed
            .frontmatter
            .get("description")
            .and_then(|value| value.as_str()),
        Some("Test")
    );
    assert_eq!(parsed.body, "Body");

    // 无 frontmatter 时 body 也会去掉 BOM 并归一换行。
    let parsed = parse_frontmatter("\u{FEFF}plain\r\nbody").expect("no frontmatter");
    assert!(parsed.frontmatter.is_empty());
    assert_eq!(parsed.body, "plain\nbody");
}
