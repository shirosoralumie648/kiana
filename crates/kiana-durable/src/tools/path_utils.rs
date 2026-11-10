//! `tools/path-utils.ts`：工具路径的 Unicode 空白归一化与 read 的容错路径变体。
//!
//! # 为什么需要它
//! 工具输入里可能出现 `@src/main.rs` 这样的前缀，或文件名里带有不换行空格、组合字符
//! （NFD）与弯引号。工具层在把路径交给环境之前统一做一次归一化/回退，避免这些"看起来
//! 一样"的字符被环境当成另一个路径而报"文件不存在"。
//!
//! # 两个入口
//! - [`resolve_tool_path`]：`write`/`edit` 使用。只做空白归一化、去掉一个前导 `@`，再调用
//!   `ExecutionEnv::absolute_path`；不检查存在性。
//! - [`resolve_read_tool_path`]：`read` 使用。先得到绝对路径，再按固定顺序尝试 AM/PM
//!   窄空格、NFD、弯引号、NFD+弯引号变体，返回第一个 `exists` 为真的路径；全都不存在时
//!   返回原绝对路径。
//!
//! # 约束与错误
//! - 候选去重保持首次出现的顺序，因此原路径总是第一个被检查。
//! - `absolute_path` 与 `exists` 的错误都会向上传播（`?`），不会被当成"不存在"吞掉。
//! - 变体只改变传给环境的字符串，不会改写磁盘上的文件。
//!
//! TS 把 NFD/弯引号回退用于 macOS 文件名变体（见 `spec/durable-rest.md` §2.25.3）；这里
//! 的候选列表与顺序与那张表逐项对应。

use std::collections::HashSet;

use kiana_chord::context::Context;
use unicode_normalization::UnicodeNormalization;

use crate::env::ExecutionEnv;
use crate::types::BoxError;

/// `UNICODE_SPACES = /[  -   　]/g`。
fn is_unicode_space(character: char) -> bool {
    matches!(
        character,
        '\u{00A0}' | '\u{2000}'..='\u{200A}' | '\u{202F}' | '\u{205F}' | '\u{3000}'
    )
}

/// `NARROW_NO_BREAK_SPACE`。
const NARROW_NO_BREAK_SPACE: char = '\u{202F}';

/// `normalizeToolPath`：Unicode 空白换成普通空格，去掉模型常加的 `@` 前缀。
///
/// 空白集合固定为（U+00A0、U+2000–U+200A、U+202F、U+205F、U+3000）；
/// `@` 只去掉一个。其余字符（包括相对路径的 `.`/`..`）原样保留。
fn normalize_tool_path(path: &str) -> String {
    let normalized: String = path
        .chars()
        .map(|character| {
            if is_unicode_space(character) {
                ' '
            } else {
                character
            }
        })
        .collect();
    match normalized.strip_prefix('@') {
        Some(rest) => rest.to_owned(),
        None => normalized,
    }
}

/// `resolveToolPath`。
///
/// 先 `normalize_tool_path`（Unicode 空白 → ASCII 空格、去掉一个 `@`），再调用
/// `env.absolute_path`；相对路径按环境的 cwd 解析。这里**不检查文件是否存在**，也不做
/// 符号链接解析——存在性由后续的读写操作报告。
///
/// # 错误
/// 只透传 `ExecutionEnv::absolute_path` 的 `FileError`（装箱为 `BoxError`）。
pub async fn resolve_tool_path(
    env: &dyn ExecutionEnv,
    path: &str,
    context: &dyn Context,
) -> Result<String, BoxError> {
    env.absolute_path(&normalize_tool_path(path), context)
        .await
        .map_err(BoxError::from)
}

/// `resolveReadToolPath`：先取绝对路径，再依次尝试窄空格、NFD、弯引号等变体，
/// 返回第一个存在的；都不存在时返回原路径。
///
/// # 候选顺序
/// 去重后依次为：原绝对路径 → `" (AM|PM)."` 里的空格换成 U+202F（窄不换行空格）→ 整条
/// 路径 NFD → `'` 换成 U+2019 → NFD 后再换弯引号（见 `read_path_variants`）。第一个
/// `env.exists` 为真的候选立即返回。
///
/// # 错误
/// `resolve_tool_path` 或任一 `exists` 返回错误时立即向上传播；"不存在"只是 `false`，
/// 会继续尝试下一个候选。
pub async fn resolve_read_tool_path(
    env: &dyn ExecutionEnv,
    path: &str,
    context: &dyn Context,
) -> Result<String, BoxError> {
    let resolved = resolve_tool_path(env, path, context).await?;
    // 去重但保留首次出现的位置：原路径永远最先检查，重复候选只查一次。
    let mut seen: HashSet<String> = HashSet::new();
    for variant in read_path_variants(&resolved) {
        if !seen.insert(variant.clone()) {
            continue;
        }
        if env.exists(&variant, context).await? {
            return Ok(variant);
        }
    }
    Ok(resolved)
}

/// `resolveReadToolPath` 的候选列表（去重交给调用方）。
///
/// 顺序即尝试顺序，不要重排：原路径必须在最前，NFD 与弯引号的组合必须在各自单变换之后；
/// 对纯 ASCII 路径，后四个变体大多等于原路径，由调用方的 `HashSet` 去掉。
fn read_path_variants(resolved: &str) -> Vec<String> {
    vec![
        resolved.to_owned(),
        replace_ampm_span(resolved),
        resolved.nfd().collect(),
        resolved.replace('\'', "\u{2019}"),
        resolved.nfd().collect::<String>().replace('\'', "\u{2019}"),
    ]
}

/// `resolved.replace(/ (AM|PM)\./gi, " $1.")`。
///
/// 只匹配"空格 + AM/PM（大小写不敏感）+ 句点"这四个字符，替换后仍是四个字符，仅把普通
/// 空格换成 `NARROW_NO_BREAK_SPACE`。用 `Vec<char>` 按字符下标比较，避免在 UTF-8 字节上
/// 切片时踩到多字节字符边界。
fn replace_ampm_span(path: &str) -> String {
    let characters: Vec<char> = path.chars().collect();
    let mut output = String::with_capacity(path.len());
    let mut index = 0usize;
    while index < characters.len() {
        let character = characters[index];
        if character == ' '
            && index + 3 < characters.len()
            && matches!(characters[index + 3], '.')
            && is_am_pm(characters[index + 1], characters[index + 2])
        {
            output.push(NARROW_NO_BREAK_SPACE);
            output.push(characters[index + 1]);
            output.push(characters[index + 2]);
            output.push('.');
            index += 4;
            continue;
        }
        output.push(character);
        index += 1;
    }
    output
}

fn is_am_pm(first: char, second: char) -> bool {
    let first = first.to_ascii_lowercase();
    let second = second.to_ascii_lowercase();
    (first == 'a' && second == 'm') || (first == 'p' && second == 'm')
}
