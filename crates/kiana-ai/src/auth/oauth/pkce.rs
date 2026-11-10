//! auth/oauth/pkce.ts。
//!
//! PKCE 工具（RFC 7636 S256），使用密码学安全随机源。
//!
//! TS 用 Web Crypto：`crypto.getRandomValues(new Uint8Array(32))` 生成 verifier，
//! `crypto.subtle.digest("SHA-256", TextEncoder().encode(verifier))` 生成 challenge，
//! 两者都按 base64url（`+`→`-`、`/`→`_`、去掉所有 `=` 填充）编码。
//!
//! TS: `src/auth/oauth/pkce.ts`
//!
//! # 为什么需要它
//!
//! OAuth 2.0 的 PKCE（Proof Key for Code Exchange，RFC 7636）解决的问题是：授权码要经过
//! 浏览器重定向，可能被同一台机器上的恶意应用截获；如果兑换 token 只需要授权码，截获者
//! 就能冒用。PKCE 让客户端先生成一个一次性的秘密 `verifier`，授权请求只发送它的 SHA-256
//! 摘要 `challenge`；兑换时再提交 `verifier`，服务端重新计算摘要并比对。只拿到授权码、
//! 没有 `verifier` 的攻击者无法完成兑换。
//!
//! 本模块只负责生成这对值；把 `challenge` 放进授权 URL、把 `verifier` 放进 token 兑换
//! 请求，由各 provider 流程自己完成（`auth/oauth/openai_codex.rs`、`openrouter.rs`、
//! `anthropic.rs`、`openai_chatgpt.rs`、`radius.rs`）。
//!
//! # 数据形状与不变量
//!
//! - `verifier`：32 字节操作系统随机数经 base64url（去填充）编码，恒为 43 个字符。
//!   RFC 7636 §4.1 允许 43–128 个 unreserved 字符，43 是长度下界，也是
//!   `ceil(32*8/6)` 的结果。
//! - `challenge`：对 **`verifier` 的 ASCII 字节**（不是原始随机字节）做 SHA-256，再做
//!   base64url 编码，同样恒为 43 个字符。所有调用方都声明 `code_challenge_method=S256`。
//! - 两个值都只含 `A-Z a-z 0-9 - _`，可直接放进 URL 查询参数，无需二次转义。
//!
//! # 失败模式
//!
//! `generate_pkce` 取不到系统随机源时 panic（`expect`）。登录链路没有把它设计成可恢复的
//! `AuthError`：这属于操作系统级别的故障，处理方式与同目录 `openai_codex.rs` 的
//! `create_state` 一致。
//!
//! # 注意事项
//!
//! - `verifier` 是秘密，不要写日志。多数流程只在 token 兑换请求体里提交它；例外是
//!   Anthropic，它把 `verifier` 同时用作 `state` 放进授权 URL（spec/ai-auth.md §2.10）。
//! - 本模块不检查 verifier 的字符集（值由本模块自己生成），也不提供 RFC 7636 的 `plain`
//!   方法——服务端与客户端都固定使用 S256。
//! - [`base64url_encode`] 还是本 crate 外唯一被使用的函数：`kiana-builtin-extensions` 的
//!   MCP OAuth 用它对 SHA-256 摘要的前 9 字节编码，得到 12 字符的回调标识。

use sha2::{Digest, Sha256};

/// PKCE 的 verifier/challenge 对。
///
/// `generate_pkce` 的返回值，构造后不再变化。两个字段都是 43 字符的 base64url（见模块
/// 文档的不变量）；调用方通常把 `challenge` 发给授权端点、把 `verifier` 留给 token 兑换，
/// 例外是 Anthropic 把 `verifier` 同时当作 `state` 发给授权端点（spec/ai-auth.md §2.10）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pkce {
    /// 43 字符的 base64url（32 字节随机数，无填充）。
    ///
    /// 秘密值：多数流程的授权请求里只出现 `challenge`，这个字段只在兑换 token 时提交给
    /// 服务端；例外是 Anthropic，它把 `verifier` 同时用作 `state` 放进授权 URL
    /// （spec/ai-auth.md §2.10）。任何情况下都不要写日志。
    pub verifier: String,
    /// `base64url(SHA-256(verifier))`，43 字符。
    ///
    /// 公开值：可以出现在授权 URL 的查询参数里；服务端在兑换时用收到的 `verifier`
    /// 重新计算摘要并比对。
    pub challenge: String,
}

/// `base64urlEncode(bytes)`：标准 base64 后替换 `+`/`/` 并去掉 `=`。
///
/// 输出无填充：每 3 字节输出 4 个字符，末尾不足 3 字节时只输出 2 或 3 个字符
/// （`b"a"` → `"YQ"`，`b"ab"` → `"YWI"`）；空输入得到空串。
///
/// 实现上先按标准字母表 `A-Za-z0-9+/` 编码、最后整体替换，是为了与 TS 的
/// `btoa(...).replace(/\+/g,"-").replace(/\//g,"_").replace(/=/g,"")` 逐步对齐；
/// 直接使用 base64url 字母表在结果上等价。
///
/// 除 PKCE 外，`kiana-builtin-extensions` 的 MCP OAuth 也用它把 SHA-256 摘要的前 9 字节
/// 编码成 12 字符的回调标识，因此本函数是本模块唯一被 crate 外使用的项。
///
/// TS: `src/auth/oauth/pkce.ts#base64urlEncode`
pub fn base64url_encode(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        // 每 3 字节拼成一个 24 位整数，再按 6 位一段查字母表；不足 3 字节时缺的字节
        // 按 0 参与移位，但下面少输出 1–2 个字符，等价于省掉 `=` 填充。
        let b0 = chunk[0] as u32;
        let b1 = chunk.get(1).copied().unwrap_or(0) as u32;
        let b2 = chunk.get(2).copied().unwrap_or(0) as u32;
        let triple = (b0 << 16) | (b1 << 8) | b2;
        out.push(ALPHABET[((triple >> 18) & 0x3f) as usize] as char);
        out.push(ALPHABET[((triple >> 12) & 0x3f) as usize] as char);
        if chunk.len() > 1 {
            out.push(ALPHABET[((triple >> 6) & 0x3f) as usize] as char);
        }
        if chunk.len() > 2 {
            out.push(ALPHABET[(triple & 0x3f) as usize] as char);
        }
    }
    // 标准字母表把下标 62/63 映射为 `+`/`/`；base64url 要求换成 `-`/`_` 且不带填充。
    out.replace('+', "-").replace('/', "_")
}

/// Generate PKCE code verifier and challenge。
///
/// 步骤：`getrandom` 取 32 字节 → base64url 得到 `verifier`；再对 `verifier` 的 ASCII
/// 字节做 SHA-256 → base64url 得到 `challenge`。无参数，每次调用都重新取随机数，
/// 因此连续两次的 `verifier` 不同（测试 `verifiers_are_unique` 锁定）。
///
/// 随机源必须是密码学安全的：这里直接向操作系统要熵，失败即 panic；没有时间戳/计数器
/// 之类的回退路径，因为可预测的 verifier 会让 PKCE 失去防护意义。
///
/// TS: `src/auth/oauth/pkce.ts#generatePKCE`
pub fn generate_pkce() -> Pkce {
    // 32 字节 → base64url 43 字符，正好满足 RFC 7636 对 verifier 长度的最小要求。
    let mut verifier_bytes = [0u8; 32];
    getrandom::fill(&mut verifier_bytes).expect("operating system random source");
    let verifier = base64url_encode(&verifier_bytes);
    // challenge 必须对**编码后的 verifier 字符串**做摘要（RFC 7636 §4.2），不是对随机
    // 原始字节；服务端收到 verifier 后也按同样方式复算。
    let challenge = base64url_encode(&Sha256::digest(verifier.as_bytes()));
    Pkce {
        verifier,
        challenge,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine as _;

    #[test]
    fn base64url_encodes_without_padding_or_plus_slash() {
        assert_eq!(base64url_encode(b""), "");
        assert_eq!(base64url_encode(b"a"), "YQ");
        assert_eq!(base64url_encode(b"ab"), "YWI");
        assert_eq!(base64url_encode(b"abc"), "YWJj");
        // 0xFB 0xFF 产生 `+` 与 `/`（标准 base64），base64url 必须替换。
        assert_eq!(base64url_encode(&[0xfb, 0xff]), "-_8");
        assert_eq!(base64url_encode(&[0x00, 0x00, 0x00]), "AAAA");
    }

    #[test]
    fn base64url_matches_the_standard_engine() {
        let bytes: Vec<u8> = (0u8..=255).collect();
        let standard = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(&bytes);
        assert_eq!(base64url_encode(&bytes), standard);
    }

    #[test]
    fn generates_rfc7636_s256_pairs() {
        let pkce = generate_pkce();
        assert_eq!(
            pkce.verifier.len(),
            43,
            "32 random bytes → 43 base64url chars"
        );
        assert_eq!(pkce.challenge.len(), 43);
        for value in [&pkce.verifier, &pkce.challenge] {
            assert!(
                value
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'),
                "not base64url: {value}"
            );
        }
        let expected = base64url_encode(&Sha256::digest(pkce.verifier.as_bytes()));
        assert_eq!(pkce.challenge, expected);
    }

    #[test]
    fn verifiers_are_unique() {
        let first = generate_pkce();
        let second = generate_pkce();
        assert_ne!(first.verifier, second.verifier);
    }
}
