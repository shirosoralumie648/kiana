//! `runtime/prelude-source.ts`：沙箱内嵌的 prelude 脚本源码与输出/存储上限常量。
//!
//! TS 源：`packages/codemode/src/runtime/prelude-source.ts`（522 行）。TS 里
//! `PRELUDE_SOURCE` 是模板字面量，12 处 `${…}` 插值（5 个不同表达式）在求值时被替换；
//! 解析后的文本是 **17233 个 UTF-16 码元、全 ASCII**，TAB 缩进、`\n` 换行、无 BOM/CR。
//! Rust 侧把**解析后的同一文本**保存为 `js/prelude.js` 并 `include_str!`（见
//! ARCHITECTURE.md §8 第 22 条与 spec 附录 A），因此本模块的常量必须与基准文本逐字一致。
//!
//! 校验值（`tests/prelude.rs` 断言）：UTF-16 长度 17233、FNV-1a = `8c6c9242`、
//! SHA-256 = `57c8dc8c0e812117d82167fa751bc368f249d5aaee56d321066fc5280c785281`。
//!
//! `PRELUDE_SOURCE` 不作为任何公开入口导出（`index.ts`/`portable.ts` 都不导出它），
//! 但 Rust 侧 `pub`（crate 内与测试使用；spec §3.3 明说“可 `pub` 但不算公开 API”）。
//!
//! # 为什么用 `include_str!` 而不是 Rust 字符串字面量
//!
//! TS 的模板字面量在求值时替换 12 处插值（5 个不同表达式）。Rust 侧若手写等价字符串，转义、
//! TAB 或换行的任何差异都会改变 VM 实际 `eval` 的文本，并让下面这组长度/哈希校验值失配。
//! 因此这里嵌入**已插值**的文本，并用长度 + FNV-1a + SHA-256 三重锁定它与 TS 的等价性。
//!
//! # 修改禁忌
//!
//! - 不要格式化、重排或改动 `js/prelude.js` 的缩进；它必须保持全 ASCII、TAB 缩进、`\n`
//!   换行、无 BOM/CR（`tests/prelude.rs` 的 `prelude_is_ascii_with_tab_indentation_and_lf`
//!   断言）。
//! - 改文本必须同步更新 `PRELUDE_SOURCE_LEN`、`PRELUDE_HASH`、`PRELUDE_SOURCE_SHA256`，并
//!   核对 4 个 `MAX_*` 字面量是否仍各出现两次；否则 `tests/prelude.rs` 失败。
//!
//! # 常量与函数被谁使用
//!
//! - [`PRELUDE_SOURCE`]：`runtime/vm.rs` 在创建 VM 时 `eval` 它；
//! - [`PRELUDE_HASH`]：`runtime/vm.rs` 拼确定性指纹（`prelude {hash}` 一段）；
//! - [`js_length`]：`declarations.rs` 用 UTF-16 码元数判断 `maxChars` 预算；
//! - [`fnv1a`]、[`PRELUDE_SOURCE_LEN`]、[`PRELUDE_SOURCE_SHA256`]：本 crate 的运行时代码
//!   不读取，只由 `tests/prelude.rs` 与本文件的内联测试消费；
//! - 4 个 `MAX_*`：运行时代码不读取（限制写在 `js/prelude.js` 的字面量里），保留是为了与
//!   TS 导出面一致，并经 `prelude`/`index`/`portable` 再导出。

/// `MAX_STORE_VALUE_CHARS = 256 * 1024`：单个 `store()` 值的 JSON 字符上限。
pub const MAX_STORE_VALUE_CHARS: usize = 256 * 1024;

/// `MAX_STORE_TOTAL_CHARS = 1024 * 1024`：所有已存值的 JSON 字符总和上限。
pub const MAX_STORE_TOTAL_CHARS: usize = 1024 * 1024;

/// `MAX_OUTPUT_CHARS = 16 * 1024 * 1024`：输出文本的字符上限。
pub const MAX_OUTPUT_CHARS: usize = 16 * 1024 * 1024;

/// `MAX_OUTPUT_ITEMS = 100_000`：输出项（`text()`/`image()`/`console.*`）数量上限。
pub const MAX_OUTPUT_ITEMS: usize = 100_000;

/// `PRELUDE_SOURCE`：在脚本运行前于 VM 内 `eval` 的 JavaScript。
///
/// 求值结果是一个函数 `(bridge, toolsJson, globalsJson, storeJson) =>
/// { settle, run, stalled }`（见 spec §2.2.7 的行为规格）。
///
/// 文本来自 crate 根目录的 `js/prelude.js`（`include_str!`），不是 Rust 里手写的字符串；
/// 修改前先读模块文档的「修改禁忌」——任何字节变化都会改变 VM 实际 `eval` 的文本，并让
/// `PRELUDE_SOURCE_LEN`、`PRELUDE_HASH`、`PRELUDE_SOURCE_SHA256` 这组校验值失配。
pub const PRELUDE_SOURCE: &str = include_str!("../../js/prelude.js");

/// prelude 的 UTF-16 码元长度（`8c6c9242` 指纹与长度一起锁定等价性）。
///
/// prelude 全 ASCII，所以它同时等于 UTF-8 字节数；`tests/prelude.rs` 断言了这一等式。
pub const PRELUDE_SOURCE_LEN: usize = 17_233;

/// prelude 的 FNV-1a 哈希（8 位十六进制），进入确定性指纹。
pub const PRELUDE_HASH: &str = "8c6c9242";

/// prelude 的 SHA-256（小写十六进制）。
///
/// 运行时代码不读取；`tests/prelude.rs` 用它做比 FNV-1a 更强的逐字节锁定。
pub const PRELUDE_SOURCE_SHA256: &str =
    "57c8dc8c0e812117d82167fa751bc368f249d5aaee56d321066fc5280c785281";

/// `fnv1a(text)`：逐 **UTF-16 码元**（`charCodeAt`）的 FNV-1a，`Math.imul` 即 32 位回绕乘法，
/// 结果是无符号 32 位的 8 位十六进制（`(hash >>> 0).toString(16).padStart(8, "0")`）。
///
/// Rust 字符串按 UTF-8 存储，所以必须显式用 `encode_utf16()` 迭代，不能按 `char` 或字节遍历：
/// 代理对（如 `😀`）在 JS 里是两个码元，按码点算会得到不同哈希。`tests/prelude.rs` 与
/// `prelude_source.rs` 的内联测试用 Node 生成的值锁定这一点。
pub fn fnv1a(text: &str) -> String {
    let mut hash: u32 = 0x811c_9dc5;
    for unit in text.encode_utf16() {
        hash ^= u32::from(unit);
        hash = hash.wrapping_mul(0x0100_0193);
    }
    format!("{hash:08x}")
}

/// `text.length`（UTF-16 码元数）。
///
/// 与 `str::chars().count()`（码点数）和 `str::len()`（UTF-8 字节数）都不同：prelude 全 ASCII
/// 时三者相等，但 `declarations.rs` 用它判断 TS 语义的 `maxChars` 预算，必须按 UTF-16 计。
pub fn js_length(text: &str) -> usize {
    text.encode_utf16().count()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constants_match_typescript() {
        assert_eq!(MAX_STORE_VALUE_CHARS, 262_144);
        assert_eq!(MAX_STORE_TOTAL_CHARS, 1_048_576);
        assert_eq!(MAX_OUTPUT_CHARS, 16_777_216);
        assert_eq!(MAX_OUTPUT_ITEMS, 100_000);
    }

    #[test]
    fn prelude_is_byte_identical() {
        use sha2::{Digest, Sha256};
        assert_eq!(js_length(PRELUDE_SOURCE), PRELUDE_SOURCE_LEN);
        assert_eq!(fnv1a(PRELUDE_SOURCE), PRELUDE_HASH);
        let digest = Sha256::digest(PRELUDE_SOURCE.as_bytes());
        let hex: String = digest.iter().map(|byte| format!("{byte:02x}")).collect();
        assert_eq!(hex, PRELUDE_SOURCE_SHA256);
        // 全 ASCII → UTF-16 长度 == UTF-8 字节数。
        assert_eq!(PRELUDE_SOURCE.len(), PRELUDE_SOURCE_LEN);
        assert!(PRELUDE_SOURCE.is_ascii());
        assert!(!PRELUDE_SOURCE.contains('\r'));
    }

    #[test]
    fn prelude_has_the_documented_boundaries() {
        assert!(PRELUDE_SOURCE.starts_with(
            "(function (bridge, toolsJson, globalsJson, storeJson) {\n\t\"use strict\";\n\n\t// Freeze every object reachable"
        ));
        assert!(PRELUDE_SOURCE.ends_with("\t\t\treturn true;\n\t\t},\n\t};\n})"));
        assert_eq!(
            &PRELUDE_SOURCE[PRELUDE_SOURCE.len() - 10..],
            "\t},\n\t};\n})"
        );
        // 插值已替换为字面数字（模板里的 `${MAX_*}` 不再出现）。
        assert!(!PRELUDE_SOURCE.contains("${"));
        assert!(PRELUDE_SOURCE.contains("262144"));
        assert!(PRELUDE_SOURCE.contains("1048576"));
        assert!(PRELUDE_SOURCE.contains("16777216"));
        assert!(PRELUDE_SOURCE.contains("100000"));
    }

    #[test]
    fn fnv1a_matches_javascript_for_non_ascii() {
        // 由 Node 的同一实现生成（逐 UTF-16 码元），锁定非 ASCII 与代理对的处理。
        assert_eq!(fnv1a(""), "811c9dc5");
        assert_eq!(fnv1a("a"), "e40c292c");
        // 😀 是一个码点、两个 UTF-16 码元；Rust 无法写出孤立代理项，因此直接锁定 Node 的结果。
        assert_eq!(fnv1a("😀"), "cb31c4b8");
    }
}
