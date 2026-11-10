//! `expand_prompt_template`。
//!
//! 逐字移植 `packages/coding-agent/src/core/prompt-templates.ts#expandPromptTemplate`：
//!
//! ```ts
//! if (!text.startsWith("/")) return text;
//! const match = text.match(/^\/([^\s]+)(?:\s+([\s\S]*))?$/);
//! if (!match) return text;
//! ```
//!
//! 模板名不含空白；名字后的第一个空白分隔符被吃掉，其余（可跨行）作为参数串。
//!
//! # 执行步骤
//!
//! 1. 首字符不是 `/` 时原样返回；
//! 2. 取 `/` 之后、第一个 JS 空白之前的子串作为模板名；名字为空（`/` 或 `/ x`）时
//!    原样返回；
//! 3. 跳过名字后的**全部**连续空白，余下内容（可含换行）作为参数串；
//! 4. 在 `templates` 中按 `name` 做**精确、大小写敏感**的比较，取第一个命中项；
//! 5. 用 [`parse_command_args`] 分词，再交给 [`substitute_args`] 代入正文。
//!
//! 未命中模板名时返回输入文本本身，不做任何修改。
//!
//! # 约束
//!
//! - 同名模板只有第一个生效（测试 `first_matching_template_wins`）；
//! - 模板正文中的占位符只替换一次，代入的参数值不会被再次展开（由
//!   [`substitute_args`] 保证）。

use crate::prompts::PromptTemplate;
use crate::prompts::args::{is_js_whitespace, parse_command_args};
use crate::prompts::substitute::substitute_args;

/// 展开匹配模板名的提示文本；未匹配时原样返回。TS: `prompt-templates.ts#expandPromptTemplate`。
///
/// - `text`：用户原始输入；只有以 `/` 开头且名字命中时才可能被替换。
/// - `templates`：调用方提供的候选列表，按给定顺序做第一个精确匹配，不排序。
/// - 返回值：新分配的 `String`；未命中时是 `text` 的拷贝。
pub fn expand_prompt_template(text: &str, templates: &[PromptTemplate]) -> String {
    if !text.starts_with('/') {
        return text.to_string();
    }

    let rest = &text[1..];
    // `[^\s]+`：至少一个非空白字符。
    let name_end = rest
        .char_indices()
        .find(|(_, character)| is_js_whitespace(*character))
        .map(|(index, _)| index)
        .unwrap_or(rest.len());
    if name_end == 0 {
        return text.to_string();
    }
    let template_name = &rest[..name_end];
    let after_name = &rest[name_end..];

    // `(?:\s+([\s\S]*))?`：若存在空白，至少吃掉一个，剩余全部作为参数串。
    let args_string = if after_name.is_empty() {
        ""
    } else {
        let first_non_space = after_name
            .char_indices()
            .find(|(_, character)| !is_js_whitespace(*character))
            .map(|(index, _)| index)
            .unwrap_or(after_name.len());
        &after_name[first_non_space..]
    };

    let Some(template) = templates
        .iter()
        .find(|template| template.name == template_name)
    else {
        // 名字合法但没有对应模板：按 TS 行为返回原文，而不是报错。
        // 也没有任何诊断或标记；调用方若要区分"未命中"，只能对比输入输出是否相同。
        return text.to_string();
    };
    let args = parse_command_args(args_string);
    substitute_args(&template.content, &args)
}

#[cfg(test)]
mod tests {
    use super::*;
    use kiana_core::source_info::{SyntheticSourceInfoOptions, create_synthetic_source_info};

    fn template(name: &str, content: &str) -> PromptTemplate {
        PromptTemplate {
            name: name.to_string(),
            description: "test".to_string(),
            argument_hint: None,
            content: content.to_string(),
            source_info: create_synthetic_source_info(
                "/tmp/template.md",
                SyntheticSourceInfoOptions {
                    source: "local".into(),
                    ..Default::default()
                },
            ),
            file_path: "/tmp/template.md".to_string(),
        }
    }

    #[test]
    fn returns_text_when_not_a_slash_command() {
        assert_eq!(expand_prompt_template("plain", &[]), "plain");
        assert_eq!(expand_prompt_template("/unknown", &[]), "/unknown");
    }

    #[test]
    fn expands_matching_template_with_quoted_args() {
        let templates = [template("review", "Review $2. Focus: ${1:-correctness}.")];
        assert_eq!(
            expand_prompt_template("/review \"\" src/index.ts", &templates),
            "Review src/index.ts. Focus: correctness."
        );
        assert_eq!(
            expand_prompt_template("/review '' src/index.ts", &templates),
            "Review src/index.ts. Focus: correctness."
        );
    }

    #[test]
    fn splits_template_arguments_on_unquoted_newlines() {
        let templates = [template("arg-test", "- arg1: $1\n- rest: ${@:2}")];
        assert_eq!(
            expand_prompt_template(
                "/arg-test label-2\n\nHere is some description #2.",
                &templates
            ),
            "- arg1: label-2\n- rest: Here is some description #2."
        );
        assert_eq!(
            expand_prompt_template("/arg-test\nlabel-2", &templates),
            "- arg1: label-2\n- rest: "
        );
    }

    #[test]
    fn first_matching_template_wins() {
        let templates = [template("a", "one"), template("a", "two")];
        assert_eq!(expand_prompt_template("/a", &templates), "one");
    }

    #[test]
    fn name_cannot_be_empty_or_start_with_space() {
        let templates = [template("a", "one")];
        assert_eq!(expand_prompt_template("/", &templates), "/");
        assert_eq!(expand_prompt_template("/ a", &templates), "/ a");
    }
}
