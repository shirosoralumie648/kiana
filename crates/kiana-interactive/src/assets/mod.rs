//! modes/interactive/assets/。
//!
//! TS 源：`packages/coding-agent/src/modes/interactive/assets/`（只有一个二进制资源
//! `clankolas.png`，640x537，539 053 字节，sha256
//! `169acd0dfe6fbb8d8742ed24a3fc654fd0b2e2d4223c733249c5493723f1b72d`）。
//!
//! ## 与 TS 的差异
//!
//! TS 在运行时用 `getBundledInteractiveAssetPath("clankolas.png")` +
//! `fs.readFileSync(...).toString("base64")` 读文件（见 `earendil-announcement.ts`）。Rust
//! 侧用 `include_bytes!` 把资源编进二进制（`spec/ca-interactive-mode.md` §6.5 / §8.1 要求
//! 「与 TS 仓库字节一致」），因此：
//!
//! * 不存在「文件缺失/读错误 → `undefined`」的运行时分支；[`load_clankolas_base64`] 恒返回
//!   `Some`，但保留 `Option` 形状，让调用方与 TS 的可选图片逻辑一一对应。
//! * base64 在首次调用时编码并缓存（TS 的 `cachedImageBase64` + `attemptedImageLoad` 语义）。
//!
//! 编码是标准 alphabet（`A-Za-z0-9+/`、`=` 补齐、无换行），也就是通用的 base64 输出
//! 契约；这里不引 `base64` 依赖（只有这一处需要）。
//!
//! ## 为什么在编译期嵌入
//!
//! TS 需要在运行时按包内路径读文件，因此 TS 侧必须处理「路径不对 / 文件缺失 / 读失败」
//! 三种情况。Rust 侧改用 `include_bytes!`：路径在编译期相对本文件解析，资源缺失会直接
//! 让编译失败，而不是在运行时退化成无图。这样发行物是自包含的，也不依赖进程的当前工作
//! 目录；代价是资源内容被固化进二进制，换图必须重新编译。
//!
//! ## 执行流程
//!
//! 1. 调用方（`crate::components::earendil_announcement`）调用 [`load_clankolas_base64`]；
//! 2. 它转调 [`clankolas_png_base64`]，后者用 `OnceLock::get_or_init` 取得缓存；
//! 3. 首次调用执行 [`base64_encode`] 编码 [`CLANKOLAS_PNG`] 并写入缓存，之后所有调用
//!    返回同一份 `&'static str`；
//! 4. 调用方用 `Option` 判断是否渲染图片，把字符串交给 TUI 的图片组件。
//!
//! ## 约束与注意事项
//!
//! * 缓存只写一次、永不失效；`OnceLock` 保证初始化闭包最多执行一次，并发调用者会等到
//!   首个初始化完成后读到同一份数据，不会重复编码。
//! * 字节保真由测试锁定（长度、base64 长度与 PNG magic 前缀）；换资源前必须逐项核对，
//!   这些断言是有意为之的护栏，不应为了通过测试而放宽。
//! * [`base64_encode`] 是通用函数，不依赖 PNG；本模块只是它的一个调用方。

use std::sync::OnceLock;

/// `IMAGE_FILENAME`。
///
/// 仅作为文件名传给图片组件（调用方把它放进 `ImageOptions` 的 `filename` 字段），
/// 不再对应磁盘上的实际文件——图片内容已经在编译期嵌入 [`CLANKOLAS_PNG`]。
/// 集成测试 `constants_are_verbatim` 锁定该字面量（文件名是稳定契约，改动需同步测试）。
pub const CLANKOLAS_PNG_FILENAME: &str = "clankolas.png";

/// 内置的 `clankolas.png` 原始字节（与 TS 仓库字节一致）。
///
/// `include_bytes!` 的路径相对本文件解析，最终指向
/// `crates/kiana-interactive/assets/clankolas.png`。因此该常量在运行时不需要文件系统，
/// 其长度是编译期常量；当前为 539 053 字节，由测试
/// `bundled_png_is_byte_identical` 与集成测试共同锁定。
pub const CLANKOLAS_PNG: &[u8] = include_bytes!("../../assets/clankolas.png");

/// 内置 PNG 的标准 base64 编码（首次调用时计算并缓存）。
///
/// 对应 TS 的模块级 `cachedImageBase64`（`fs.readFileSync(...).toString("base64")`）。
///
/// 返回 `&'static str`，指向 `OnceLock` 内保存的那一份字符串，调用方只能读取。
/// 首次调用会编码全部 539 053 字节（结果 718 740 个字符）；后续调用只读取缓存，
/// 因此可以放心地在每次构造组件时调用。缓存不会失效，也不存在替换内容的接口。
pub fn clankolas_png_base64() -> &'static str {
    static CACHE: OnceLock<String> = OnceLock::new();
    CACHE.get_or_init(|| base64_encode(CLANKOLAS_PNG))
}

/// `loadImageBase64()`（模块私有）的等价物。
///
/// TS 在首次调用时尝试读取文件：成功则缓存 base64，失败则缓存 `undefined` 且之后不再重试。
/// 内置资源不可能失败，这里恒为 `Some`；`Option` 只是与 TS 的返回类型对齐。
///
/// 调用方 `EarendilAnnouncementComponent::new` 用 `is_some()` 决定是否添加图片子组件，
/// 于是 TS 的「无图」分支在类型上仍然存在，只是本实现下永远走不到。
pub fn load_clankolas_base64() -> Option<&'static str> {
    Some(clankolas_png_base64())
}

/// 标准 base64 编码（RFC 4648 §4，`=` 补齐，无换行）。
///
/// 输入任意字节切片，输出长度恒为 `4 * ceil(input.len() / 3)` 的 `String`；空输入得到空串。
/// 编码表固定为标准 alphabet，不是 URL-safe 变体。该函数没有错误分支：所有输入都能编码，
/// 不会因为字节值而失败。
///
/// 它输出标准 base64（所有字节值都能编码），并由 RFC 4648 向量测试覆盖
/// （含 `\0`、`0xFF` 等非 ASCII 字节）。
pub fn base64_encode(input: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    // 每个 3 字节输入产生 4 个字符；预算容量避免编码过程中反复扩容。
    let mut output = String::with_capacity(input.len().div_ceil(3) * 4);
    // 先处理所有完整的 3 字节组：拼成 24 位后从高位起每 6 位查一次表。
    let (chunks, remainder) = input.as_chunks::<3>();
    for chunk in chunks {
        let bits = (u32::from(chunk[0]) << 16) | (u32::from(chunk[1]) << 8) | u32::from(chunk[2]);
        output.push(ALPHABET[(bits >> 18) as usize & 0x3f] as char);
        output.push(ALPHABET[(bits >> 12) as usize & 0x3f] as char);
        output.push(ALPHABET[(bits >> 6) as usize & 0x3f] as char);
        output.push(ALPHABET[bits as usize & 0x3f] as char);
    }
    // 尾部余 1 或 2 字节：不足的位补 0，再用 1 或 2 个 `=` 把输出凑满 4 字符一组。
    match remainder {
        [a] => {
            let bits = u32::from(*a) << 16;
            output.push(ALPHABET[(bits >> 18) as usize & 0x3f] as char);
            output.push(ALPHABET[(bits >> 12) as usize & 0x3f] as char);
            output.push('=');
            output.push('=');
        }
        [a, b] => {
            let bits = (u32::from(*a) << 16) | (u32::from(*b) << 8);
            output.push(ALPHABET[(bits >> 18) as usize & 0x3f] as char);
            output.push(ALPHABET[(bits >> 12) as usize & 0x3f] as char);
            output.push(ALPHABET[(bits >> 6) as usize & 0x3f] as char);
            output.push('=');
        }
        _ => {}
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_matches_rfc4648_vectors() {
        assert_eq!(base64_encode(b""), "");
        assert_eq!(base64_encode(b"f"), "Zg==");
        assert_eq!(base64_encode(b"fo"), "Zm8=");
        assert_eq!(base64_encode(b"foo"), "Zm9v");
        assert_eq!(base64_encode(b"foob"), "Zm9vYg==");
        assert_eq!(base64_encode(b"fooba"), "Zm9vYmE=");
        assert_eq!(base64_encode(b"foobar"), "Zm9vYmFy");
        assert_eq!(base64_encode(&[0, 255, 128]), "AP+A");
    }

    #[test]
    fn bundled_png_is_byte_identical() {
        assert_eq!(CLANKOLAS_PNG.len(), 539_053);
        // 标准 base64 的长度 = 4 * ceil(n / 3)。
        assert_eq!(clankolas_png_base64().len(), 718_740);
        assert!(clankolas_png_base64().starts_with("iVBORw0KGgo"));
        assert_eq!(CLANKOLAS_PNG_FILENAME, "clankolas.png");
    }
}
