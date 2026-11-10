//! 公开面验收（spec §2.2.15 / §3.3）：主入口与 `/portable` 的逐字导出清单。
//!
//! 这些断言是**编译期**的：任何一行 `pub use` 丢失（例如合并时漏删 `cfg(any())`）都会让
//! 本文件无法编译。每个名字都同时被「引用一次、当作类型用一次」，从而覆盖值与类型两半。

#![allow(dead_code)]

use std::marker::PhantomData;

/// 只要求 `T` 是一个类型（类型存在性检查）。
fn assert_type<T: ?Sized>() {
    let _ = PhantomData::<&T>;
}

/// spec §2.2.15 主入口的 22 个值导出 + 32 个类型导出。
#[test]
fn main_entry_exports_the_frozen_surface() {
    // --- 值：declarations ---
    assert_eq!(kiana_codemode::DEFAULT_INPUT_SCHEMA_MAX_CHARS, 16_000);
    let _ = kiana_codemode::render_declarations;
    let _ = kiana_codemode::render_tool_output_type;
    let _ = kiana_codemode::render_tool_sample;
    let _ = kiana_codemode::render_tool_signature;
    let _ = kiana_codemode::schema_to_type;
    // --- identifier ---
    assert_eq!(kiana_codemode::to_codemode_identifier("my-tool"), "my_tool");
    // --- runtime::execution / host ---
    assert_type::<kiana_codemode::CodemodeSandboxBase>();
    assert_type::<kiana_codemode::CodemodeSandbox>();
    // --- runtime::inline ---
    assert_eq!(kiana_codemode::DEFAULT_INTERRUPT_BUDGET, 100_000);
    assert_type::<kiana_codemode::InlineCodemodeSandbox>();
    // --- runtime::prelude-source ---
    assert_eq!(kiana_codemode::MAX_OUTPUT_CHARS, 16_777_216);
    assert_eq!(kiana_codemode::MAX_OUTPUT_ITEMS, 100_000);
    assert_eq!(kiana_codemode::MAX_STORE_TOTAL_CHARS, 1_048_576);
    assert_eq!(kiana_codemode::MAX_STORE_VALUE_CHARS, 262_144);
    // --- runtime::remote ---
    assert_type::<kiana_codemode::RemoteCodemodeSandbox>();
    let _ = kiana_codemode::serve_codemode_remote;
    // --- source ---
    assert_eq!(kiana_codemode::CODEMODE_OPTIONS_PREFIX, "// @options:");
    assert!(kiana_codemode::CODEMODE_SOURCE_GRAMMAR.contains("OPTIONS_LINE"));
    let error = kiana_codemode::CodemodeSourceError::new("boom");
    assert_eq!(error.name(), "CodemodeSourceError");
    assert_eq!(error.to_string(), "boom");
    assert!(kiana_codemode::parse_codemode_source("text('hi')").is_ok());
    // --- wasm ---
    let _ = kiana_codemode::load_quickjs_wasm;

    // --- 类型：§2.2.15 逐条 ---
    assert_type::<kiana_codemode::RenderDeclarationsOptions<'static>>();
    assert_type::<kiana_codemode::InlineCodemodeSandboxOptions>();
    assert_type::<dyn kiana_codemode::CodemodeRemote>();
    assert_type::<kiana_codemode::CodemodeRemoteExchange>();
    assert_type::<kiana_codemode::CodemodeRemoteMessage>();
    assert_type::<kiana_codemode::CodemodeRemoteReply>();
    assert_type::<kiana_codemode::CodemodeRemoteRequest>();
    assert_type::<kiana_codemode::CodemodeServeOptions>();
    assert_type::<kiana_codemode::RemoteCodemodeSandboxOptions>();
    assert_type::<kiana_codemode::CodemodeSourceOptions>();
    assert_type::<kiana_codemode::ParsedCodemodeSource>();
    assert_type::<kiana_codemode::CodemodeCall>();
    assert_type::<kiana_codemode::CodemodeCallStatus>();
    assert_type::<kiana_codemode::CodemodeDeclaredGlobal>();
    assert_type::<kiana_codemode::CodemodeDeclaredTool>();
    assert_type::<kiana_codemode::CodemodeDuring>();
    assert_type::<kiana_codemode::CodemodeError>();
    assert_type::<kiana_codemode::CodemodeErrorKind>();
    assert_type::<kiana_codemode::CodemodeExecuteOptions>();
    assert_type::<kiana_codemode::CodemodeHostCall>();
    assert_type::<kiana_codemode::CodemodeJsonSchema>();
    assert_type::<kiana_codemode::CodemodeOutcome>();
    assert_type::<kiana_codemode::CodemodeOutputItem>();
    assert_type::<kiana_codemode::CodemodeResult>();
    assert_type::<kiana_codemode::CodemodeRun>();
    assert_type::<kiana_codemode::CodemodeRunOptions>();
    assert_type::<kiana_codemode::CodemodeSandboxBaseOptions>();
    assert_type::<kiana_codemode::CodemodeSandboxOptions>();
    assert_type::<kiana_codemode::CodemodeStoreWrites>();
    assert_type::<kiana_codemode::CodemodeTool>();
    assert_type::<kiana_codemode::CodemodeToolContext>();
    assert_type::<kiana_codemode::CodemodeWasmModule>();
}

/// spec §2.2.15 `/portable`：无 Node 内建的入口。
#[test]
fn portable_entry_exports_the_frozen_surface() {
    use kiana_codemode::portable;

    assert_eq!(portable::to_codemode_identifier("a.b"), "a_b");
    assert_type::<portable::CodemodeSandboxBase>();
    assert_eq!(portable::DEFAULT_INTERRUPT_BUDGET, 100_000);
    assert_type::<portable::InlineCodemodeSandbox>();
    assert_eq!(portable::MAX_OUTPUT_CHARS, 16_777_216);
    assert_eq!(portable::MAX_OUTPUT_ITEMS, 100_000);
    assert_eq!(portable::MAX_STORE_TOTAL_CHARS, 1_048_576);
    assert_eq!(portable::MAX_STORE_VALUE_CHARS, 262_144);
    assert_type::<portable::HostToWorkerMessage>();
    assert_type::<portable::VmStartData>();
    assert_type::<portable::WorkerToHostMessage>();
    assert_type::<portable::RemoteCodemodeSandbox>();
    let _ = portable::serve_codemode_remote;

    // `/portable` 的 `Codemode*` 类型面：除 `CodemodeSandboxOptions` 外的全部。
    assert_type::<portable::InlineCodemodeSandboxOptions>();
    assert_type::<dyn portable::CodemodeRemote>();
    assert_type::<portable::CodemodeRemoteExchange>();
    assert_type::<portable::CodemodeRemoteMessage>();
    assert_type::<portable::CodemodeRemoteReply>();
    assert_type::<portable::CodemodeRemoteRequest>();
    assert_type::<portable::CodemodeServeOptions>();
    assert_type::<portable::RemoteCodemodeSandboxOptions>();
    assert_type::<portable::CodemodeCall>();
    assert_type::<portable::CodemodeCallStatus>();
    assert_type::<portable::CodemodeDeclaredGlobal>();
    assert_type::<portable::CodemodeDeclaredTool>();
    assert_type::<portable::CodemodeDuring>();
    assert_type::<portable::CodemodeError>();
    assert_type::<portable::CodemodeErrorKind>();
    assert_type::<portable::CodemodeExecuteOptions>();
    assert_type::<portable::CodemodeHostCall>();
    assert_type::<portable::CodemodeJsonSchema>();
    assert_type::<portable::CodemodeOutcome>();
    assert_type::<portable::CodemodeOutputItem>();
    assert_type::<portable::CodemodeResult>();
    assert_type::<portable::CodemodeRun>();
    assert_type::<portable::CodemodeRunOptions>();
    assert_type::<portable::CodemodeSandboxBaseOptions>();
    assert_type::<portable::CodemodeStoreWrites>();
    assert_type::<portable::CodemodeTool>();
    assert_type::<portable::CodemodeToolContext>();
    assert_type::<portable::CodemodeWasmModule>();
}

/// spec §2.2.5：`loadQuickJSWasm(path?)` 的加载失败沿用 Node 的逐字文本；当前稳定契约，
/// 改动需同步本测试。
#[tokio::test]
async fn load_quickjs_wasm_reports_the_node_error_text() {
    // 含 NUL 的路径：Node 的 `fs` 在读文件之前就拒绝（`ERR_INVALID_ARG_VALUE`）。
    let error = kiana_codemode::load_quickjs_wasm(Some(std::path::Path::new("bad\0path")))
        .await
        .expect_err("a path with a NUL byte must fail");
    assert!(
        error.to_string().starts_with(
            "The argument 'path' must be a string, Uint8Array, or URL without null bytes. Received "
        ),
        "{error}"
    );
}
