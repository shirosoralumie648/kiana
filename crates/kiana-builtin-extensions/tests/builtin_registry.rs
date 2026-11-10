//! `extensions/index.ts`：`builtInExtensions` 的顺序、名字与标志位，
//! 以及四个工厂都能用假的 `ExtensionApi` 调用。

// 假 `ExtensionApi` 是单线程的（`ExtensionApi` 本身 `!Send`），`Arc` 只用于工厂签名。
#![allow(clippy::arc_with_non_send_sync)]

use std::sync::Arc;

use kiana_builtin_extensions::builtin_extensions;
use kiana_ext_api::types::{ExtensionApi, InlineExtension};

#[path = "mcp_extension_support/mod.rs"]
mod mcp_extension_support;

use mcp_extension_support::FakeExtensionApi;

#[test]
#[allow(clippy::arc_with_non_send_sync)] // 假 ExtensionApi 故意是单线程的（ExtensionApi 本身 !Send）。
fn registry_order_names_and_flags_match_the_ts_list() {
    let extensions = builtin_extensions();
    assert_eq!(extensions.len(), 4);
    let expected: [(&str, Option<bool>, Option<bool>); 4] = [
        ("llama.cpp", None, Some(true)),
        ("codemode", Some(true), Some(true)),
        ("tool-search", Some(true), Some(true)),
        ("mcp", Some(true), Some(true)),
    ];
    for (extension, (name, replaceable, builtin)) in extensions.iter().zip(expected) {
        let InlineExtension::Named {
            name: actual_name,
            hidden,
            replaceable: actual_replaceable,
            builtin: actual_builtin,
            ..
        } = extension
        else {
            panic!("extensions/index.ts must use named inline extensions");
        };
        assert_eq!(actual_name, name);
        assert_eq!(*hidden, None);
        assert_eq!(*actual_replaceable, replaceable);
        assert_eq!(*actual_builtin, builtin);
    }
}

#[test]
fn every_factory_can_be_invoked_with_a_fake_extension_api() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");
    let local = tokio::task::LocalSet::new();
    runtime.block_on(local.run_until(async {
        for extension in builtin_extensions() {
            let InlineExtension::Named { name, factory, .. } = extension else {
                panic!("named extension");
            };
            let api = Arc::new(FakeExtensionApi::new());
            factory(api.clone() as Arc<dyn ExtensionApi>).await;
            match name.as_str() {
                "llama.cpp" => {
                    assert!(api.command_names().iter().any(|command| command == "llama"));
                    assert_eq!(*api.providers.borrow(), 1);
                }
                "codemode" => {
                    assert!(api.tool_names().iter().any(|tool| tool == "codemode"));
                }
                "tool-search" => {
                    assert!(api.tool_names().iter().any(|tool| tool == "tool_search"));
                }
                "mcp" => {
                    assert!(api.command_names().iter().any(|command| command == "mcp"));
                    assert!(!api.tool_renderers.borrow().is_empty());
                }
                other => panic!("unexpected builtin extension {other}"),
            }
        }
    }));
}
