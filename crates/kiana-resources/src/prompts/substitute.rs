//! `substitute_args`（手写状态机，不用 regex；见 `ca-resources.md` §8.4）。
//!
//! 逐字移植 `packages/coding-agent/src/core/prompt-templates.ts#substituteArgs` 的单趟
//! `String.prototype.replace` 语义：
//!
//! ```js
//! /\$\{(\d+|ARGUMENTS|@):-([^}]*)\}|\$\{@:(\d+)(?::(\d+))?\}|\$(ARGUMENTS|@|\d+)/g
//! ```
//!
//! 替换结果**不再参与替换**（非递归），且分支优先级为 `${X:-default}` > `${@:N[:L]}` >
//! `$ARGUMENTS|$@|$N`。
//!
//! # 扫描方式
//!
//! [`substitute_args`] 反复查找 `$`：先把 `$` 之前的文本原样拷到输出，再让内部函数
//! `match_dollar` 尝试匹配三种模式。
//!
//! - 命中：写入替换文本，并从模式末尾继续扫描。替换文本不会被重新扫描，所以参数值里的
//!   `$1`、`${@:2}` 保持字面量。
//! - 未命中：输出一个 `$`，从下一个字符继续。
//!
//! 这等价于 JS 的一次全局 `String.prototype.replace`。三个分支的触发前缀（`${`、
//! `$ARGUMENTS`、`$@`、`$N`）互不重叠，任一位置至多一个分支可命中；按正则 alternation
//! 的顺序排列是为了与 TS 逐字对照。
//!
//! # 占位符语义
//!
//! ```text
//! $ARGUMENTS / $@        全部参数以单个空格连接
//! $N                     第 N 个参数（1 起）；越界或 $0 得到空串
//! ${N:-d} ${@:-d} ${ARGUMENTS:-d}
//!                        目标缺失或为空串时使用默认值 d
//! ${@:N} / ${@:N:L}      从第 N 个参数起取 L 个；省略 L 取到末尾，N=0 等同 N=1
//! ```
//!
//! # 字节偏移约定
//!
//! `match_dollar` 返回的消耗长度以**字节**计，由 ASCII 分隔符长度、ASCII 数字串长度
//! 和 `find('}')` 的字节下标累加得出，因此可以直接用于 `&candidate[consumed..]` 切片；
//! 多字节字符只会完整地落在未匹配片段或替换文本里。

use crate::prompts::args::is_js_whitespace;

/// 解析十进制数字串为 `u64`；溢出视为 JS 的 `Infinity`（`None`）。
///
/// 所有调用点都把 `None` 当作“超出范围”：作为下标时取不到参数，作为切片起点/长度时
/// 取到末尾或截断为空。
fn parse_digits(digits: &str) -> Option<u64> {
    digits.parse::<u64>().ok()
}

/// 按 `$` 模式替换参数。TS: `prompt-templates.ts#substituteArgs`。
///
/// `args` 是已分词的参数（通常来自 [`crate::prompts::args::parse_command_args`]）；传入
/// 空切片时 `$ARGUMENTS`/`$@` 替换为空串。返回新分配的 `String`，`content` 不被修改。
///
/// 单趟从左到右替换，未命中的 `$`（例如 `$$`、`$A`）原样保留；不做递归展开。
///
/// 失败模式：本函数不会失败——所有异常输入（越界下标、非数字的 `L`、未闭合的 `${`）
/// 都按"未命中该模式"或"空串"处理，与 JS 正则不匹配时的行为对齐。
pub fn substitute_args(content: &str, args: &[String]) -> String {
    let all_args = args.join(" ");
    let mut out = String::with_capacity(content.len());
    let mut rest = content;

    while let Some(offset) = rest.find('$') {
        out.push_str(&rest[..offset]);
        let candidate = &rest[offset..];

        if let Some((replacement, consumed)) = match_dollar(candidate, &all_args, args) {
            out.push_str(&replacement);
            rest = &candidate[consumed..];
        } else {
            // 当前位置的 `$` 不构成任何模式：原样输出并继续扫描后续字符。
            out.push('$');
            rest = &candidate[1..];
        }
    }
    out.push_str(rest);
    out
}

/// 尝试在 `text`（以 `$` 开头）匹配三个分支之一。返回 `(替换文本, 消耗字节数)`。
///
/// 分支顺序与正则 alternation 顺序一致，便于逐字对照；由于三个分支的触发前缀
/// 互不重叠，重排不改变行为。返回 `None` 表示当前位置的 `$` 不是占位符，调用方应
/// 原样输出 `$` 后从下一个字符继续。
///
/// `consumed` 是字节数，调用方直接用于 `&candidate[consumed..]`。
fn match_dollar(text: &str, all_args: &str, args: &[String]) -> Option<(String, usize)> {
    if let Some(after_dollar) = text.strip_prefix("${") {
        // 分支 1：`${X:-default}`。
        if let Some((target, target_len, after_target)) = parse_default_target(after_dollar)
            && let Some(default) = after_target.strip_prefix(":-")
            && let Some(close) = default.find('}')
        {
            let default_value = &default[..close];
            let value = match target {
                DefaultTarget::All => Some(all_args),
                DefaultTarget::Position(digits) => parse_digits(digits)
                    .and_then(|index| index.checked_sub(1))
                    .and_then(|index| usize::try_from(index).ok())
                    .and_then(|index| args.get(index))
                    .map(String::as_str),
            };
            // JS 真值判断：目标缺失或为空串时都使用默认值。
            // 对应 TS 的 `value || default`：空串与缺失走同一路径，不是仅判缺失。
            let replacement = match value {
                Some(value) if !value.is_empty() => value,
                _ => default_value,
            };
            // `${` + target + `:-` + default + `}`。
            return Some((replacement.to_string(), 2 + target_len + 2 + close + 1));
        }

        // 分支 2：`${@:N}` / `${@:N:L}`。
        if let Some(after_at) = after_dollar.strip_prefix("@:")
            && let Some((start_digits, after_start)) = take_digits(after_at)
        {
            let (length_digits, rest) = match after_start.strip_prefix(':') {
                Some(after_colon) => match take_digits(after_colon) {
                    Some((length_digits, after_length)) => (Some(length_digits), after_length),
                    None => (None, after_start),
                },
                None => (None, after_start),
            };
            if rest.starts_with('}') {
                // 必须紧接 `}`；若 `:L` 的 L 不是数字（如 `${@:2:x}`），整个分支不成立，
                // 不会退回成 `${@:2}`。
                let start = slice_start(start_digits);
                // `end` 是半开边界：L 省略或溢出（含 JS Infinity）都取 `usize::MAX`，
                // 经 `min(args.len())` 截到末尾；L=0 时 end == start，切出空区间，
                // 替换为空串。
                let end = match length_digits {
                    Some(length_digits) => match parse_digits(length_digits) {
                        Some(length) if length <= usize::MAX as u64 => {
                            start.saturating_add(length as usize)
                        }
                        _ => usize::MAX,
                    },
                    None => usize::MAX,
                };
                // 两端先夹进 `0..=args.len()`，再保证 end >= start，避免越界切片。
                let take_start = start.min(args.len());
                let take_end = end.min(args.len()).max(take_start);
                let replacement = args[take_start..take_end].join(" ");
                let consumed = 2
                    + 1
                    + 1
                    + start_digits.len()
                    + length_digits.map_or(0, |digits| 1 + digits.len())
                    + 1;
                return Some((replacement, consumed));
            }
        }

        return None;
    }

    let after_dollar = text.strip_prefix('$')?;

    // 分支 3：`$ARGUMENTS` / `$@` / `$N`。
    // `$N` 允许任意长数字串：越界或 `$0`（`checked_sub(1)` 下溢）统一替换为空串，
    // 与 JS 数组越界取 `undefined` 再转字符串一致。
    if after_dollar.starts_with("ARGUMENTS") {
        return Some((all_args.to_string(), 1 + "ARGUMENTS".len()));
    }
    if after_dollar.starts_with('@') {
        return Some((all_args.to_string(), 2));
    }
    if let Some((digits, _rest)) = take_digits(after_dollar) {
        let replacement = parse_digits(digits)
            .and_then(|index| index.checked_sub(1))
            .and_then(|index| usize::try_from(index).ok())
            .and_then(|index| args.get(index))
            .cloned()
            .unwrap_or_default();
        return Some((replacement, 1 + digits.len()));
    }

    None
}

/// `${X:-...}` 的目标。
enum DefaultTarget<'a> {
    /// `${@:-...}` / `${ARGUMENTS:-...}`：全部参数。
    All,
    /// `${N:-...}`：第 N 个参数；保存原始十进制串（可能溢出，解析失败按缺失处理）。
    Position(&'a str),
}

/// 解析 `${X:-...}` 的 `X`：`\d+|ARGUMENTS|@`（正则顺序优先）。返回目标、其字节长度与剩余文本。
///
/// 检查顺序为 `ARGUMENTS` → `@` → 数字；三者前缀互不重叠，因此与正则 alternation 的
/// 结果一致。返回的长度用于计算消耗字节数。
fn parse_default_target(text: &str) -> Option<(DefaultTarget<'_>, usize, &str)> {
    if let Some(rest) = text.strip_prefix("ARGUMENTS") {
        return Some((DefaultTarget::All, "ARGUMENTS".len(), rest));
    }
    if let Some(rest) = text.strip_prefix('@') {
        return Some((DefaultTarget::All, 1, rest));
    }
    take_digits(text).map(|(digits, rest)| (DefaultTarget::Position(digits), digits.len(), rest))
}

/// 取开头连续的 ASCII 数字（至少一个）。
///
/// 返回 `(数字串, 剩余文本)`；下标来自字节扫描但只切在 ASCII 边界上，UTF-8 安全。
fn take_digits(text: &str) -> Option<(&str, &str)> {
    let end = text
        .bytes()
        .position(|byte| !byte.is_ascii_digit())
        .unwrap_or(text.len());
    if end == 0 {
        None
    } else {
        Some((&text[..end], &text[end..]))
    }
}

/// `${@:N}` 的 `start`：`parseInt(N, 10) - 1`，负数按 0（`${@:0}` 等价 `${@:1}`）。
///
/// 溢出（JS 的 `Infinity`）返回 `usize::MAX`，调用方用 `min(args.len())` 后得到空切片。
fn slice_start(digits: &str) -> usize {
    match parse_digits(digits) {
        Some(0) => 0,
        Some(value) => usize::try_from(value - 1).unwrap_or(usize::MAX),
        // 溢出 = Infinity：`slice(Infinity)` 为空。
        None => usize::MAX,
    }
}

// `is_js_whitespace` 在本模块没有直接使用，但保证它与 args 的状态机同源。
// 用函数指针常量引用它：既保留上面的 import，又让签名变化时在这里先编译失败。
#[allow(dead_code)]
const _: fn(char) -> bool = is_js_whitespace;

#[cfg(test)]
mod tests {
    use super::*;

    fn args(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| value.to_string()).collect()
    }

    #[test]
    fn replaces_all_and_positional_placeholders() {
        assert_eq!(
            substitute_args("Test: $ARGUMENTS", &args(&["a", "b", "c"])),
            "Test: a b c"
        );
        assert_eq!(
            substitute_args("Test: $@", &args(&["a", "b", "c"])),
            "Test: a b c"
        );
        assert_eq!(
            substitute_args("$1: $ARGUMENTS", &args(&["prefix", "a", "b"])),
            "prefix: prefix a b"
        );
        assert_eq!(
            substitute_args("$1 $2 $3", &args(&["a", "b", "c"])),
            "a b c"
        );
        assert_eq!(
            substitute_args("$1 $2 $3 $4 $5", &args(&["a", "b"])),
            "a b   "
        );
        assert_eq!(substitute_args("$0", &args(&["a", "b"])), "");
        assert_eq!(substitute_args("$1.5", &args(&["a"])), "a.5");
        assert_eq!(substitute_args("$1$2", &args(&["a", "b"])), "ab");
        assert_eq!(
            substitute_args("$1 $2 $@", &args(&["a", "b", "c"])),
            "a b a b c"
        );
    }

    #[test]
    fn does_not_substitute_recursively() {
        assert_eq!(
            substitute_args("$ARGUMENTS", &args(&["$1", "$ARGUMENTS"])),
            "$1 $ARGUMENTS"
        );
        assert_eq!(substitute_args("$@", &args(&["$100", "$1"])), "$100 $1");
        assert_eq!(substitute_args("${1:-$ARGUMENTS}", &args(&["a", "b"])), "a");
        assert_eq!(
            substitute_args("${3:-$ARGUMENTS}", &args(&["a", "b"])),
            "$ARGUMENTS"
        );
    }

    #[test]
    fn handles_non_matching_dollars_and_case() {
        assert_eq!(
            substitute_args("$A $$ $ $ARGS", &args(&["a"])),
            "$A $$ $ $ARGS"
        );
        assert_eq!(
            substitute_args("$arguments $Arguments $ARGUMENTS", &args(&["a", "b"])),
            "$arguments $Arguments a b"
        );
        assert_eq!(substitute_args("Price: \\$100", &[]), "Price: \\");
    }

    #[test]
    fn placeholders_inside_words_are_replaced() {
        assert_eq!(
            substitute_args("pre$ARGUMENTS", &args(&["a", "b"])),
            "prea b"
        );
        assert_eq!(substitute_args("pre$@", &args(&["a", "b"])), "prea b");
        assert_eq!(substitute_args("x$ARGUMENTSy", &args(&["a"])), "xay");
    }

    #[test]
    fn defaults_use_truthiness() {
        assert_eq!(substitute_args("${1:-7}", &args(&["3"])), "3");
        assert_eq!(substitute_args("${1:-7}", &[]), "7");
        assert_eq!(substitute_args("${1:-brief}", &args(&[""])), "brief");
        assert_eq!(substitute_args("${@:-default}", &[]), "default");
        assert_eq!(
            substitute_args("${@:-default}", &args(&["This", "would"])),
            "This would"
        );
        assert_eq!(substitute_args("${ARGUMENTS:-default}", &[]), "default");
        assert_eq!(substitute_args("${1:-seven steps}", &[]), "seven steps");
        assert_eq!(
            substitute_args("${3:-fallback}", &args(&["a", "b"])),
            "fallback"
        );
        assert_eq!(
            substitute_args("${1:-7} ${2:-brief}", &args(&["3"])),
            "3 brief"
        );
        assert_eq!(
            substitute_args("$1 ${2:-x} $ARGUMENTS", &args(&["a"])),
            "a x a"
        );
    }

    #[test]
    fn slices_are_bash_style() {
        assert_eq!(
            substitute_args("${@:2}", &args(&["a", "b", "c", "d"])),
            "b c d"
        );
        assert_eq!(substitute_args("${@:1}", &args(&["a", "b", "c"])), "a b c");
        assert_eq!(
            substitute_args("${@:2:2}", &args(&["a", "b", "c", "d"])),
            "b c"
        );
        assert_eq!(substitute_args("${@:0}", &args(&["a", "b", "c"])), "a b c");
        assert_eq!(substitute_args("${@:99}", &args(&["a", "b"])), "");
        assert_eq!(substitute_args("${@:2:0}", &args(&["a", "b", "c"])), "");
        assert_eq!(substitute_args("${@:2:99}", &args(&["a", "b", "c"])), "b c");
        assert_eq!(
            substitute_args("${@:2} vs $@", &args(&["a", "b", "c"])),
            "b c vs a b c"
        );
        assert_eq!(
            substitute_args("prefix${@:2}suffix", &args(&["a", "b", "c"])),
            "prefixb csuffix"
        );
        assert_eq!(
            substitute_args("${@:1:1} and ${@:2}", &args(&["a", "b", "c"])),
            "a and b c"
        );
    }

    #[test]
    fn slices_do_not_recurse() {
        assert_eq!(
            substitute_args("${@:1}", &args(&["${@:2}", "test"])),
            "${@:2} test"
        );
        assert_eq!(
            substitute_args("${@:2}", &args(&["a", "${@:3}", "c"])),
            "${@:3} c"
        );
    }

    #[test]
    fn unicode_and_whitespace_arguments_are_preserved() {
        assert_eq!(
            substitute_args("$ARGUMENTS", &args(&["日本語", "🎉", "café"])),
            "日本語 🎉 café"
        );
        assert_eq!(
            substitute_args("$1 $2", &args(&["line1\nline2", "tab\tthere"])),
            "line1\nline2 tab\tthere"
        );
        assert_eq!(
            substitute_args("$ARGUMENTS", &args(&["a", "", "c"])),
            "a  c"
        );
        assert_eq!(
            substitute_args("$ARGUMENTS", &args(&["  leading  ", "trailing  "])),
            "  leading   trailing  "
        );
    }

    #[test]
    fn long_argument_lists_and_multiple_digits() {
        let values: Vec<String> = (0..100).map(|index| format!("arg{index}")).collect();
        assert_eq!(substitute_args("$ARGUMENTS", &values), values.join(" "));

        let values: Vec<String> = (0..15).map(|index| format!("val{index}")).collect();
        assert_eq!(substitute_args("$10 $12 $15", &values), "val9 val11 val14");
    }

    #[test]
    fn overlapping_patterns_follow_regex_alternation_order() {
        // `${1:-x}` 优先于 `$1`；`${@:1}` 不是默认值分支，也不是 `$@`。
        assert_eq!(substitute_args("${1:-x}", &args(&["y"])), "y");
        assert_eq!(substitute_args("${@:1}", &args(&["y", "z"])), "y z");
        // `${1}` 没有 `:-`，也不是 `${@:...}`；`$` 不匹配任何分支。
        assert_eq!(substitute_args("${1}", &args(&["y"])), "${1}");
    }
}
