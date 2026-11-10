//! identifier.ts：工具名 → 脚本里使用的标识符。
//!
//! TS 源：`packages/codemode/src/identifier.ts`（14 行）。
//!
//! ```ts
//! export function toCodemodeIdentifier(name: string): string {
//!   let identifier = "";
//!   for (const char of name) {            // 按 Unicode 码点迭代（for..of）
//!     const valid = identifier === "" ? /^[A-Za-z_$]$/.test(char) : /^[A-Za-z0-9_$]$/.test(char);
//!     identifier += valid ? char : "_";
//!   }
//!   return identifier === "" ? "_" : identifier;
//! }
//! ```
//!
//! # 为什么逐码点、为什么只认 ASCII
//!
//! TS 的 `for..of` 以 Unicode **码点**为迭代单位；Rust 的 `str::chars` 相同，所以 `"😀"`
//! 在两边都只产生一个 `_`（它占两个 UTF-16 码元，但不会被拆成两个）。
//! 正则 `[A-Za-z_$]` / `[A-Za-z0-9_$]` 只接受 ASCII，因此不能用 `char::is_alphabetic`：
//! `"héllo"` 里的 `é` 必须变成 `_`（测试 `counts_code_points_not_utf16_units` 锁定）。
//!
//! # 不变量与调用方
//!
//! 返回值非空，且一定是合法的 JavaScript 标识符（首字符 `[A-Za-z_$]`，其余
//! `[A-Za-z0-9_$]`）。`declarations` 的 `render_tool_signature` 把它拼进
//! `declare const tools` 的对象成员签名；顶层 `declare function` 的全局名直接用
//! `CodemodeTool::name`，不经过本函数。
//! `kiana-builtin-extensions` 与 `kiana-durable` 用它做工具/命名空间的查找键。

/// 脚本里代表工具的标识符：不合法字符逐**码点**变成 `_`（不是逐 UTF-16 码元）。
///
/// - `"mcp__docs__search"` → 原样；
/// - `"my-tool"` → `"my_tool"`；
/// - 首字符不能是数字（`"1a"` → `"_a"`）；
/// - 非 ASCII（含代理对，如 emoji）每个码点一个 `_`；
/// - 空串 → `"_"`。
///
/// TS: `identifier.ts#toCodemodeIdentifier`。
pub fn to_codemode_identifier(name: &str) -> String {
    let mut identifier = String::with_capacity(name.len());
    for ch in name.chars() {
        // 首字符只允许字母/`_`/`$`；后续位置额外允许数字。判定依据是“已经输出过字符”，
        // 所以首个输入字符被替换成 `_` 后，第二个字符按后续位置规则处理（`"1a"` → `"_a"`）。
        let valid = if identifier.is_empty() {
            ch.is_ascii_alphabetic() || ch == '_' || ch == '$'
        } else {
            ch.is_ascii_alphanumeric() || ch == '_' || ch == '$'
        };
        identifier.push(if valid { ch } else { '_' });
    }
    // 空输入没有可输出的字符；TS 同样补一个 `_`，保证调用方拿到合法标识符。
    if identifier.is_empty() {
        "_".to_string()
    } else {
        identifier
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_valid_identifiers() {
        assert_eq!(
            to_codemode_identifier("mcp__docs__search"),
            "mcp__docs__search"
        );
        assert_eq!(to_codemode_identifier("$ref"), "$ref");
        assert_eq!(to_codemode_identifier("_x1"), "_x1");
    }

    #[test]
    fn replaces_invalid_characters_with_underscores() {
        assert_eq!(to_codemode_identifier("my-tool"), "my_tool");
        assert_eq!(to_codemode_identifier("a.b"), "a_b");
        assert_eq!(to_codemode_identifier("a b"), "a_b");
        assert_eq!(to_codemode_identifier("1a"), "_a");
    }

    #[test]
    fn empty_name_becomes_underscore() {
        assert_eq!(to_codemode_identifier(""), "_");
    }

    #[test]
    fn counts_code_points_not_utf16_units() {
        // "😀" 是一个码点、两个 UTF-16 码元 → 恰好一个 "_"。
        assert_eq!(to_codemode_identifier("😀"), "_");
        assert_eq!(to_codemode_identifier("a😀b"), "a_b");
        // 组合字符逐码点处理："e" + U+0301 → "e_"
        assert_eq!(to_codemode_identifier("e\u{0301}"), "e_");
        // 非 ASCII 字母也不行。
        assert_eq!(to_codemode_identifier("héllo"), "h_llo");
    }

    #[test]
    fn first_character_is_treated_separately() {
        // 首字符是数字时变 "_"，之后的字符按非首位规则。
        assert_eq!(to_codemode_identifier("9lives"), "_lives");
        // 首位非法字符之后的合法字符保持。
        assert_eq!(to_codemode_identifier("-a"), "_a");
        assert_eq!(to_codemode_identifier("--a--"), "__a__");
    }
}
