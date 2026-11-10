//! 虚拟模块身份测试（设计 §3.3；TS 回归 #11156）。
//!
//! TS 断言：`VIRTUAL_MODULES["@mariozechner/kiana-ai/models"]).toBe(VIRTUAL_MODULES["@earendil-works/kiana-ai/models"])`
//! 且 `typeof models.clampThinkingLevel === "function"`。Rust 侧同时覆盖注册表对象身份与 ESM
//! namespace 身份（多个公开键别名到同一规范模块名）。

#[path = "fixtures/virtual/support.rs"]
mod virtual_support;

use kiana_extensions::loader::resolver::VIRTUAL_MODULE_KEYS;
use kiana_extensions::virtual_modules;
use rquickjs::{Module, Object};
use virtual_support::VirtualHarness;

/// `virtual-modules.ts` 中指向同一对象的键组。
const SAME_OBJECT_GROUPS: &[&[&str]] = &[
    &["typebox", "@sinclair/typebox"],
    &["typebox/compile", "@sinclair/typebox/compile"],
    &["typebox/value", "@sinclair/typebox/value"],
    &[
        "@earendil-works/kiana-agent-core",
        "@mariozechner/kiana-agent-core",
    ],
    &["@earendil-works/kiana-tui", "@mariozechner/kiana-tui"],
    &[
        "@earendil-works/kiana-ai",
        "@earendil-works/kiana-ai/compat",
        "@mariozechner/kiana-ai",
        "@mariozechner/kiana-ai/compat",
    ],
    &[
        "@earendil-works/kiana-ai/models",
        "@mariozechner/kiana-ai/models",
    ],
    &[
        "@earendil-works/kiana-ai/oauth",
        "@mariozechner/kiana-ai/oauth",
    ],
    &[
        "@earendil-works/kiana-ai/providers/all",
        "@mariozechner/kiana-ai/providers/all",
    ],
    &[
        "@earendil-works/kiana-coding-agent",
        "@mariozechner/kiana-coding-agent",
    ],
];

#[test]
fn registry_objects_are_shared_per_group() {
    let harness = VirtualHarness::new();
    for group in SAME_OBJECT_GROUPS {
        let first = group[0];
        for other in &group[1..] {
            let same: bool = harness.eval(&format!(
                r#"globalThis.__kiana_virtual[{first:?}] === globalThis.__kiana_virtual[{other:?}]"#
            ));
            assert!(
                same,
                "{first} and {other} must be the same namespace object"
            );
        }
    }
}

#[test]
fn import_namespaces_are_shared_per_group() {
    let harness = VirtualHarness::new();
    for group in SAME_OBJECT_GROUPS {
        for window in group.windows(2) {
            let (a, b) = (window[0], window[1]);
            let same: bool = harness.eval_promise(&format!(
                r#"Promise.all([import({a:?}), import({b:?})]).then(([x, y]) => x === y)"#
            ));
            assert!(same, "import {a} and {b} must share one module namespace");
        }
    }
}

#[test]
fn distinct_namespaces_are_not_conflated() {
    let harness = VirtualHarness::new();
    let pairs = [
        ("typebox", "typebox/compile"),
        ("typebox", "typebox/value"),
        ("typebox/compile", "typebox/value"),
        (
            "@earendil-works/kiana-ai",
            "@earendil-works/kiana-ai/models",
        ),
        ("@earendil-works/kiana-ai", "@earendil-works/kiana-ai/oauth"),
        (
            "@earendil-works/kiana-ai",
            "@earendil-works/kiana-ai/providers/all",
        ),
        (
            "@earendil-works/kiana-ai/models",
            "@earendil-works/kiana-ai/oauth",
        ),
        (
            "@earendil-works/kiana-agent-core",
            "@earendil-works/kiana-coding-agent",
        ),
        (
            "@earendil-works/kiana-tui",
            "@earendil-works/kiana-coding-agent",
        ),
    ];
    for (a, b) in pairs {
        let distinct: bool = harness.eval(&format!(
            r#"globalThis.__kiana_virtual[{a:?}] !== globalThis.__kiana_virtual[{b:?}]"#
        ));
        assert!(distinct, "{a} and {b} must be different namespaces");
    }
}

#[test]
fn kiana_ai_type_is_the_same_typebox_type_namespace() {
    let harness = VirtualHarness::new();
    let same: bool = harness.eval(
        r#"globalThis.__kiana_virtual["@earendil-works/kiana-ai"].Type === globalThis.__kiana_virtual["typebox"].Type
            && globalThis.__kiana_virtual["@earendil-works/kiana-ai"].Type.Object === globalThis.__kiana_virtual["@sinclair/typebox"].Type.Object"#,
    );
    assert!(same, "kiana-ai Type must be the embedded typebox Type");
}

#[test]
fn kiana_ai_models_regression_11156() {
    let harness = VirtualHarness::new();
    // 两个 scope 指向同一个 models namespace（TS 的 toBe 断言）。
    let same: bool = harness.eval(
        r#"globalThis.__kiana_virtual["@mariozechner/kiana-ai/models"] === globalThis.__kiana_virtual["@earendil-works/kiana-ai/models"]"#,
    );
    assert!(same);
    // `typeof models.clampThinkingLevel === "function"`：桥未安装时也是函数（调用才报错）。
    let kind: String = harness.eval(
        r#"typeof globalThis.__kiana_virtual["@earendil-works/kiana-ai/models"].clampThinkingLevel"#,
    );
    assert_eq!(kind, "function");
    let same_import: bool = harness.eval_promise(
        r#"Promise.all([
            import("@earendil-works/kiana-ai/models"),
            import("@mariozechner/kiana-ai/models"),
        ]).then(([a, b]) => a === b && typeof a.clampThinkingLevel === "function")"#,
    );
    assert!(same_import);

    // 装入 kiana-ai 桥后，两个 scope 调用的是同一实现。
    virtual_modules::set_native_member(
        &harness.engine,
        "kiana-ai/models",
        "clampThinkingLevel",
        |ctx| {
            Ok(
                rquickjs::Function::new(ctx.clone(), |level: String| format!("clamped:{level}"))?
                    .into_value(),
            )
        },
    )
    .expect("set clampThinkingLevel");
    let results: Vec<String> = harness.eval(
        r#"[
            globalThis.__kiana_virtual["@earendil-works/kiana-ai/models"].clampThinkingLevel("high"),
            globalThis.__kiana_virtual["@mariozechner/kiana-ai/models"].clampThinkingLevel("high"),
        ]"#,
    );
    assert_eq!(results, vec!["clamped:high", "clamped:high"]);
}

#[test]
fn every_public_key_maps_to_the_expected_canonical_module() {
    let fs = virtual_modules::virtual_fs();
    let expected = [
        ("typebox", "typebox/build/index.mjs"),
        ("typebox/compile", "typebox/build/compile/index.mjs"),
        ("typebox/value", "typebox/build/value/index.mjs"),
        ("@sinclair/typebox", "typebox/build/index.mjs"),
        (
            "@sinclair/typebox/compile",
            "typebox/build/compile/index.mjs",
        ),
        ("@sinclair/typebox/value", "typebox/build/value/index.mjs"),
        (
            "@earendil-works/kiana-agent-core",
            "kiana-virtual/kiana-agent-core.js",
        ),
        (
            "@mariozechner/kiana-agent-core",
            "kiana-virtual/kiana-agent-core.js",
        ),
        ("@earendil-works/kiana-tui", "kiana-virtual/kiana-tui.js"),
        ("@mariozechner/kiana-tui", "kiana-virtual/kiana-tui.js"),
        ("@earendil-works/kiana-ai", "kiana-virtual/kiana-ai.js"),
        (
            "@earendil-works/kiana-ai/compat",
            "kiana-virtual/kiana-ai.js",
        ),
        ("@mariozechner/kiana-ai", "kiana-virtual/kiana-ai.js"),
        ("@mariozechner/kiana-ai/compat", "kiana-virtual/kiana-ai.js"),
        (
            "@earendil-works/kiana-ai/models",
            "kiana-virtual/kiana-ai/models.js",
        ),
        (
            "@mariozechner/kiana-ai/models",
            "kiana-virtual/kiana-ai/models.js",
        ),
        (
            "@earendil-works/kiana-ai/oauth",
            "kiana-virtual/kiana-ai/oauth.js",
        ),
        (
            "@mariozechner/kiana-ai/oauth",
            "kiana-virtual/kiana-ai/oauth.js",
        ),
        (
            "@earendil-works/kiana-ai/providers/all",
            "kiana-virtual/kiana-ai/providers-all.js",
        ),
        (
            "@mariozechner/kiana-ai/providers/all",
            "kiana-virtual/kiana-ai/providers-all.js",
        ),
        (
            "@earendil-works/kiana-coding-agent",
            "kiana-virtual/kiana-coding-agent.js",
        ),
        (
            "@mariozechner/kiana-coding-agent",
            "kiana-virtual/kiana-coding-agent.js",
        ),
    ];
    assert_eq!(expected.len(), 22);
    for (key, module) in expected {
        assert_eq!(
            fs.canonical_name(key),
            Some(module),
            "canonical module for {key}"
        );
        assert!(
            VIRTUAL_MODULE_KEYS.contains(&key),
            "{key} is one of the 22 keys"
        );
    }
}

#[test]
fn discovery_style_module_with_kiana_ai_subpaths_and_coding_agent_entry_loads() {
    // `extensions-discovery.test.ts` 的三个用例：coding-agent 入口、kiana-ai/oauth、kiana-ai/models。
    let harness = VirtualHarness::new();
    harness.engine.with_js(|ctx| {
        let source = r#"
            import { getAgentDir } from "@earendil-works/kiana-coding-agent";
            import * as oauth from "@earendil-works/kiana-ai/oauth";
            import { clampThinkingLevel } from "@earendil-works/kiana-ai/models";
            import { Type } from "typebox";
            export const ok =
                typeof getAgentDir === "function" &&
                typeof oauth === "object" &&
                typeof clampThinkingLevel === "function" &&
                Type.Object({})[Symbol.for("TypeBox.Kind")] === undefined;
            export default function factory(kiana) { kiana.registerCommand("test", { handler: async () => {} }); }
        "#;
        // 扩展入口的模块名带 ?v= 前缀（§3.4）；这里只需要一个唯一名字。
        let module = Module::declare(ctx.clone(), "extension.ts?v=1-1", source)
            .expect("declare discovery extension");
        let (module, promise) = module.eval().expect("evaluate discovery extension");
        promise.finish::<rquickjs::Value>().expect("module evaluated");
        let namespace: Object = module.namespace().expect("namespace");
        let ok: bool = namespace.get("ok").expect("ok export");
        assert!(ok);
        let factory: rquickjs::Function = namespace.get("default").expect("default factory");
        assert!(factory.is_function());
    });
}

#[test]
fn same_specifier_twice_is_the_same_namespace_object() {
    let harness = VirtualHarness::new();
    for key in [
        "typebox",
        "@earendil-works/kiana-ai",
        "@mariozechner/kiana-ai/models",
        "@earendil-works/kiana-agent-core",
    ] {
        let same: bool = harness.eval_promise(&format!(
            r#"Promise.all([import({key:?}), import({key:?})]).then(([a, b]) => a === b)"#
        ));
        assert!(same, "importing {key} twice");
    }
}
