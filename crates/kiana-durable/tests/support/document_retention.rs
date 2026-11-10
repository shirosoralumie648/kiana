//! `test/setup-document-retention.ts` 的 Rust 等价物。
//!
//! vitest 用 `setupFiles` 在每个测试文件之前运行这段夹具；Rust 没有 setupFile，因此这里把
//! 同一段逻辑放进平台的初始化段（ELF `.init_array` / Mach-O `__mod_init_func`），让每个
//! 包含本模块的测试二进制在进入 `main` 之前运行一次：
//!
//! ```ts
//! const forced = process.env.KIANA_DURABLE_DOCUMENT_RETENTION_MS;
//! if (forced !== undefined) setDefaultDocumentRetention(Number(forced));
//! ```
//!
//! 这样 `KIANA_DURABLE_DOCUMENT_RETENTION_MS=0 cargo test -p kiana-durable` 就是 TS 的第二遍
//! `KIANA_DURABLE_DOCUMENT_RETENTION_MS=0 vitest --run`（spec/durable-harness.md §8.5 第 2 条）：
//! 所有用默认保留期的 Session 在每次提交后立即逐出文档，测试结果必须与第一遍一致。
//! `src/` 不读这个变量（它只是测试夹具的开关）。

/// `Number(raw)` 的数值结果；`NaN`/非数字返回 `None`（TS 里 `documentRetention(NaN)` 回落到
/// 默认值，因此这里保持默认）。小数毫秒按 `i64` 截断——夹具只用 0，`setDefaultDocumentRetention`
/// 的 Rust 钩子是毫秒整数。
pub(crate) fn parse_document_retention(raw: &str) -> Option<i64> {
    let trimmed = raw.trim();
    // `Number("") === 0`；`Number("abc") === NaN`。
    let value = if trimmed.is_empty() {
        0.0
    } else {
        trimmed.parse::<f64>().ok()?
    };
    if !value.is_finite() {
        return None;
    }
    Some(value as i64)
}

extern "C" fn apply_document_retention_env() {
    let Ok(raw) = std::env::var("KIANA_DURABLE_DOCUMENT_RETENTION_MS") else {
        return;
    };
    if let Some(ms) = parse_document_retention(&raw) {
        kiana_durable::session::set_default_document_retention(ms);
    }
}

#[used]
#[cfg_attr(
    any(target_os = "linux", target_os = "android"),
    unsafe(link_section = ".init_array")
)]
#[cfg_attr(
    target_vendor = "apple",
    unsafe(link_section = "__DATA,__mod_init_func")
)]
static DOCUMENT_RETENTION_ENV: extern "C" fn() = apply_document_retention_env;
