//! `parse_command_args`。
//!
//! 逐字移植 `packages/coding-agent/src/core/prompt-templates.ts#parseCommandArgs`。
//! bash 风格引号解析，**不支持反斜杠转义**；空引号（`""` / `''`）产生一个空参数。
//!
//! # 解析规则
//!
//! 单趟扫描字符，用 `in_quote` 记录当前引号种类：
//!
//! 1. 引号外遇到 `"` 或 `'`：进入该种引号，并标记 token 已开始；
//! 2. 引号内遇到同种引号：退出引号；其他字符（包括另一种引号、换行、反斜杠）原样进入
//!    当前 token；
//! 3. 引号外遇到 `is_js_whitespace` 中的字符：结束当前 token；
//! 4. 扫描结束：若 token 已开始则压入结果（因此 `""` 会留下空串）。
//!
//! # 状态机
//!
//! `parse_command_args` 是一个两状态机（引号外 / 引号内）加三个扫描变量：
//!
//! | 状态 | 含义 | 迁移 |
//! |---|---|---|
//! | 引号外（`in_quote = None`） | 默认状态 | 遇 `"`/`'` 进入引号内；遇空白结束当前 token；遇其他字符累积进 token |
//! | 引号内（`in_quote = Some(q)`） | 正在引号 `q` 中 | 遇同种 `q` 回到引号外；其他任何字符（含换行、另一种引号、`\`）原样累积 |
//!
//! 不变量：`has_token` 为真时 `current` 可能是空串（带引号的空参数）；为假时 `current`
//! 必为空串。函数结束时若 `has_token` 仍为真，把 `current` 压入结果。
//!
//! # 与调用方的关系
//!
//! 本仓库内的调用方是 [`crate::prompts::expand::expand_prompt_template`]：它先用
//! [`crate::prompts::expand`] 模块文档所述的规则切出参数串，再交给本函数分词，最后由
//! [`crate::prompts::substitute::substitute_args`] 代入模板正文。
//!
//! # 边界
//!
//! - 无转义语义：`\"` 不会保护引号，反斜杠只是普通字符（测试 `backslashes_are_literal`
//!   锁定该行为）。
//! - 引号未闭合时不报错：剩余内容仍作为当前 token 返回。
//! - 空白集合沿用 JS 正则 `\s`（见 `is_js_whitespace`），而不是 Rust 的
//!   [`char::is_whitespace`]，两者在 U+FEFF / U+0085 上不同。

/// JS `/\\s/`（ECMA-262 WhiteSpace + LineTerminator）的字符集合。
///
/// 与 [`char::is_whitespace`] 的差异：包含 U+FEFF，且不包含 U+0085。
///
/// [`crate::prompts::expand`] 也用它切分 `/name args` 的名字与参数边界；两处必须共用
/// 这个集合，否则边界会与 TS 正则 `\s` 不一致。
pub(crate) fn is_js_whitespace(character: char) -> bool {
    matches!(
        character,
        '\u{0009}'
            | '\u{000A}'
            | '\u{000B}'
            | '\u{000C}'
            | '\u{000D}'
            | '\u{0020}'
            | '\u{00A0}'
            | '\u{1680}'
            | '\u{2000}'
            ..='\u{200A}'
                | '\u{2028}'
                | '\u{2029}'
                | '\u{202F}'
                | '\u{205F}'
                | '\u{3000}'
                | '\u{FEFF}'
    )
}

/// 解析命令参数（bash 风格引号）。
///
/// TS: `prompt-templates.ts#parseCommandArgs`。
///
/// 返回按出现顺序排列的参数列表；空输入返回空 `Vec`。引号只用于分组，不会出现在结果里。
/// 连续的引号外空白只起分隔作用；token 内部的空白与换行原样保留。
pub fn parse_command_args(args_string: &str) -> Vec<String> {
    let mut args: Vec<String> = Vec::new();
    let mut current = String::new();
    // 记录 token 是否已开始，使带引号的空串（`""` 或 `''`）保留为参数。
    let mut has_token = false;
    let mut in_quote: Option<char> = None;

    for character in args_string.chars() {
        match in_quote {
            Some(quote) => {
                if character == quote {
                    in_quote = None;
                } else {
                    current.push(character);
                }
            }
            None => {
                if character == '"' || character == '\'' {
                    in_quote = Some(character);
                    has_token = true;
                } else if is_js_whitespace(character) {
                    if has_token {
                        args.push(std::mem::take(&mut current));
                        has_token = false;
                    }
                } else {
                    current.push(character);
                    has_token = true;
                }
            }
        }
    }

    if has_token {
        args.push(current);
    }

    args
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_space_separated_arguments() {
        assert_eq!(parse_command_args("a b c"), vec!["a", "b", "c"]);
    }

    #[test]
    fn parses_quoted_arguments_with_spaces() {
        assert_eq!(
            parse_command_args("\"first arg\" second"),
            vec!["first arg", "second"]
        );
        assert_eq!(
            parse_command_args("'first arg' second"),
            vec!["first arg", "second"]
        );
        assert_eq!(
            parse_command_args("\"double\" 'single' \"double again\""),
            vec!["double", "single", "double again"]
        );
    }

    #[test]
    fn empty_and_whitespace_input() {
        assert!(parse_command_args("").is_empty());
        assert_eq!(parse_command_args("a  b   c"), vec!["a", "b", "c"]);
        assert_eq!(parse_command_args("a\tb\tc"), vec!["a", "b", "c"]);
    }

    #[test]
    fn quoted_empty_strings_are_kept() {
        assert_eq!(parse_command_args("\"\" \" \""), vec!["", " "]);
        assert_eq!(parse_command_args("a '' c"), vec!["a", "", "c"]);
        assert_eq!(parse_command_args("a \"\""), vec!["a", ""]);
    }

    #[test]
    fn special_and_unicode_characters() {
        assert_eq!(
            parse_command_args("$100 @user #tag"),
            vec!["$100", "@user", "#tag"]
        );
        assert_eq!(
            parse_command_args("日本語 🎉 café"),
            vec!["日本語", "🎉", "café"]
        );
    }

    #[test]
    fn newlines_inside_quotes_are_kept_and_unquoted_newlines_split() {
        assert_eq!(
            parse_command_args("\"line1\nline2\" second"),
            vec!["line1\nline2", "second"]
        );
        assert_eq!(
            parse_command_args("label-2\n\nHere is some description #2."),
            vec!["label-2", "Here", "is", "some", "description", "#2."]
        );
        assert_eq!(parse_command_args("a\n\n\tb  c"), vec!["a", "b", "c"]);
    }

    #[test]
    fn backslashes_are_literal() {
        assert_eq!(
            parse_command_args("\"quoted \\\"text\\\"\""),
            vec!["quoted \\text\\"]
        );
    }

    #[test]
    fn trims_leading_and_trailing_spaces() {
        assert_eq!(parse_command_args("a b c   "), vec!["a", "b", "c"]);
        assert_eq!(parse_command_args("   a b c"), vec!["a", "b", "c"]);
    }

    #[test]
    fn js_whitespace_set_includes_bom_and_excludes_nel() {
        assert!(is_js_whitespace('\u{FEFF}'));
        assert!(!is_js_whitespace('\u{0085}'));
    }
}
