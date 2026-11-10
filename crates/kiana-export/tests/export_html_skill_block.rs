//! 移植 `packages/coding-agent/test/export-html-skill-block.test.ts`。
//!
//! skill 块的结构化包裹由 `template.js` 的 `parseSkillBlock` 处理（Rust 不重写
//! viewer）；这里对逐字节内嵌的脚本做同样的标记断言。describe/it 标题保持逐字
//! （见测试函数上的注释）。

use regex::Regex;

use kiana_export::assets::TEMPLATE_JS;

fn matches(pattern: &str) -> bool {
    Regex::new(pattern)
        .expect("valid regex")
        .is_match(TEMPLATE_JS)
}

// it("strips skill wrapper XML from user message rendering")
#[test]
fn strips_skill_wrapper_xml_from_user_message_rendering() {
    // Skill commands store a structural wrapper in the raw user message:
    //   <skill name="..." location="...">\n...\n</skill>\n\nactual prompt
    // The export renderer must detect that wrapper and render only the user-visible prompt.
    assert!(matches(r"parseSkillBlock"));
    assert!(matches(r"skillBlock\.userMessage"));
}

// it("renders skill invocation and user message as separate sibling blocks")
#[test]
fn renders_skill_invocation_and_user_message_as_separate_sibling_blocks() {
    assert!(matches(r"skill-invocation"));
    assert!(matches(r"hasUserContent"));
}

// it("renders skill content as markdown, not raw text")
#[test]
fn renders_skill_content_as_markdown_not_raw_text() {
    assert!(matches(r"safeMarkedParse\(skillBlock\.content\)"));
}

// it("shows skill name and user message in the sidebar tree")
#[test]
fn shows_skill_name_and_user_message_in_the_sidebar_tree() {
    assert!(matches(r"tree-role-skill"));
}
