//! `cloudflare.ts` 的占位面测试（ARCHITECTURE.md §8 第 3 条：不移植）。
//!
//! 占位模块**无条件编译**（与骨架和 `kiana-durable`/`kiana-chord` 的同批占位一致），所以这些
//! 断言在默认 feature 下就运行；`cargo test -p kiana-codemode --features cloudflare` 结果相同。
//!
//! 保留与平台无关的常量，构造入口返回逐字的 not supported 错误；TS 的
//! `export * from "./portable.ts"` 由 `pub use crate::portable::*` 复刻，所以从
//! `kiana_codemode::cloudflare` 也必须拿到整个 portable 面。

use kiana_codemode::cloudflare::{
    CloudflareCodemodeUnsupported, DEFAULT_REMOTE_MEMORY_LIMIT_BYTES, UNSUPPORTED_MESSAGE,
    cloudflare_quickjs_wasm, create_inline_codemode_sandbox,
};

// spec §3.3：`DEFAULT_REMOTE_MEMORY_LIMIT_BYTES = 33554432`（仅 /cloudflare）。
#[test]
fn remote_memory_limit_matches_typescript() {
    assert_eq!(DEFAULT_REMOTE_MEMORY_LIMIT_BYTES, 33_554_432);
    assert_eq!(DEFAULT_REMOTE_MEMORY_LIMIT_BYTES, 32 * 1024 * 1024);
}

// ARCHITECTURE.md §8 第 3 条的逐字文案。
#[test]
fn unsupported_message_is_verbatim() {
    assert_eq!(
        UNSUPPORTED_MESSAGE,
        "Cloudflare Workers codemode is not supported in the Rust build"
    );
    let error = CloudflareCodemodeUnsupported;
    assert_eq!(error.to_string(), UNSUPPORTED_MESSAGE);
}

#[test]
fn entry_points_fail_with_the_verbatim_error() {
    let error = cloudflare_quickjs_wasm().expect_err("cloudflare wasm is not available");
    assert_eq!(
        error.to_string(),
        "Cloudflare Workers codemode is not supported in the Rust build"
    );

    let error =
        create_inline_codemode_sandbox().expect_err("cloudflare inline sandbox is not available");
    assert_eq!(
        error.to_string(),
        "Cloudflare Workers codemode is not supported in the Rust build"
    );
}

// TS: `export * from "./portable.ts"`。`/cloudflare` 的调用方可以直接用 portable 的名字。
#[test]
fn cloudflare_reexports_the_portable_surface() {
    use kiana_codemode::cloudflare::{
        MAX_OUTPUT_CHARS, MAX_OUTPUT_ITEMS, MAX_STORE_TOTAL_CHARS, MAX_STORE_VALUE_CHARS,
        to_codemode_identifier,
    };
    assert_eq!(MAX_STORE_VALUE_CHARS, 262_144);
    assert_eq!(MAX_STORE_TOTAL_CHARS, 1_048_576);
    assert_eq!(MAX_OUTPUT_CHARS, 16_777_216);
    assert_eq!(MAX_OUTPUT_ITEMS, 100_000);
    assert_eq!(to_codemode_identifier("a-b"), "a_b");

    // 类型面（只做名称存在性检查）。
    fn assert_portable_types(
        _: Option<kiana_codemode::cloudflare::CodemodeTool>,
        _: Option<kiana_codemode::cloudflare::CodemodeResult>,
        _: Option<kiana_codemode::cloudflare::VmStartData>,
        _: Option<kiana_codemode::cloudflare::HostToWorkerMessage>,
        _: Option<kiana_codemode::cloudflare::CodemodeWasmModule>,
    ) {
    }
    assert_portable_types(None, None, None, None, None);
}
