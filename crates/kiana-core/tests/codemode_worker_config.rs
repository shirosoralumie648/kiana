//! `test/codemode-worker-config.test.ts`（#10204）。

use kiana_core::config::{CodemodeRuntime, resolve_codemode_worker_specifier};

#[test]
fn uses_a_relative_source_entrypoint_in_bun_binaries() {
    assert_eq!(
        resolve_codemode_worker_specifier(
            CodemodeRuntime::BunBinary,
            "file:///B:/~BUN/root/config.js"
        ),
        Some("./src/extensions/codemode/worker.ts".to_string())
    );
}

#[test]
fn uses_the_emitted_worker_beside_the_bundled_node_module() {
    assert_eq!(
        resolve_codemode_worker_specifier(
            CodemodeRuntime::BundledNode,
            "file:///app/chunks/config.js"
        ),
        Some("file:///app/chunks/codemode-worker.js".to_string())
    );
}

#[test]
fn uses_the_kiana_codemode_worker_when_unbundled() {
    assert_eq!(
        resolve_codemode_worker_specifier(CodemodeRuntime::Unbundled, "file:///app/config.js"),
        None
    );
}
