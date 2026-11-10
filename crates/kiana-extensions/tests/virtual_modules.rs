//! 22 个虚拟模块的安装、求值与桥成员测试（设计 §3.3、§3.6、§8.1–§8.6）。
//!
//! TS 对照：`packages/coding-agent/test/suite/regressions/11156-...`（身份）与
//! `test/extensions-discovery.test.ts` 的 `import ... from "@earendil-works/kiana-*"` 用例；
//! 类型断言部分见 `virtual_module_identity.rs`，typebox 语义见 `typebox.rs`。

#[path = "fixtures/virtual/support.rs"]
mod virtual_support;

use kiana_extensions::engine::JsEngine;
use kiana_extensions::virtual_modules;
use serde_json::{Value, json};
use virtual_support::VirtualHarness;

/// `virtual-modules.ts` 的键顺序（逐字）。
const KEYS: [&str; 22] = [
    "typebox",
    "typebox/compile",
    "typebox/value",
    "@sinclair/typebox",
    "@sinclair/typebox/compile",
    "@sinclair/typebox/value",
    "@earendil-works/kiana-agent-core",
    "@earendil-works/kiana-tui",
    "@earendil-works/kiana-ai",
    "@earendil-works/kiana-ai/compat",
    "@earendil-works/kiana-ai/models",
    "@earendil-works/kiana-ai/oauth",
    "@earendil-works/kiana-ai/providers/all",
    "@earendil-works/kiana-coding-agent",
    "@mariozechner/kiana-agent-core",
    "@mariozechner/kiana-tui",
    "@mariozechner/kiana-ai",
    "@mariozechner/kiana-ai/compat",
    "@mariozechner/kiana-ai/models",
    "@mariozechner/kiana-ai/oauth",
    "@mariozechner/kiana-ai/providers/all",
    "@mariozechner/kiana-coding-agent",
];

/// 设计 §8.6 冻结的 34 个 `@earendil-works/kiana-coding-agent` 运行期导出。
const CODING_AGENT_EXPORTS: [&str; 34] = [
    "BorderedLoader",
    "CONFIG_DIR_NAME",
    "CustomEditor",
    "DEFAULT_MAX_BYTES",
    "DEFAULT_MAX_LINES",
    "DynamicBorder",
    "VERSION",
    "convertToLlm",
    "createBashTool",
    "createBashToolDefinition",
    "createEditTool",
    "createEditToolDefinition",
    "createFindTool",
    "createFindToolDefinition",
    "createGrepTool",
    "createGrepToolDefinition",
    "createLsTool",
    "createLsToolDefinition",
    "createReadTool",
    "createReadToolDefinition",
    "createWriteTool",
    "createWriteToolDefinition",
    "defineTool",
    "formatReadLineRange",
    "formatSize",
    "getAgentDir",
    "getMarkdownTheme",
    "getSettingsListTheme",
    "keyHint",
    "parseFrontmatter",
    "serializeConversation",
    "truncateHead",
    "truncateLine",
    "withFileMutationQueue",
];

/// 设计 §5.1 列出的 19 个 kiana-tui 运行期符号。
const KIANA_TUI_CORPUS_EXPORTS: [&str; 19] = [
    "Box",
    "CURSOR_MARKER",
    "Container",
    "Editor",
    "Input",
    "Key",
    "Markdown",
    "SelectList",
    "SettingsList",
    "Spacer",
    "Text",
    "fuzzyFilter",
    "isKeyRelease",
    "matchesKey",
    "parseKey",
    "renderFakeCursor",
    "truncateToWidth",
    "visibleWidth",
    "wrapTextWithAnsi",
];

/// 设计 §5.1 额外从 kiana-tui namespace 暴露的 coding-agent 辅助符号。
const KIANA_TUI_CODING_AGENT_EXPORTS: [&str; 6] = [
    "BorderedLoader",
    "CustomEditor",
    "DynamicBorder",
    "getMarkdownTheme",
    "getSettingsListTheme",
    "keyHint",
];

/// 读取本单元生成的 TS 参照夹具。
fn fixture(name: &str) -> Value {
    let path = format!(
        "{}/tests/fixtures/virtual/{name}",
        env!("CARGO_MANIFEST_DIR")
    );
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("read fixture {path}: {error}"));
    serde_json::from_str(&text).expect("fixture is valid JSON")
}

#[test]
fn global_has_exactly_the_22_keys_in_typescript_order() {
    let harness = VirtualHarness::new();
    let keys: Vec<String> = harness.eval("Object.keys(globalThis.__kiana_virtual)");
    assert_eq!(keys, KEYS);
    let plain_object: bool =
        harness.eval("Object.getPrototypeOf(globalThis.__kiana_virtual) === Object.prototype");
    assert!(plain_object);
}

#[test]
fn install_without_a_loader_reports_the_missing_prerequisite() {
    // `install` 的前置条件：runtime 上已有模块 loader（§9.2；见 virtual_modules::install 文档）。
    let engine = JsEngine::new().expect("engine");
    kiana_extensions::engine::install(&engine).expect("prelude");
    let error = virtual_modules::install(&engine).expect_err("install must fail without a loader");
    let text = format!("{error}");
    assert!(
        text.contains("needs the runtime module loader"),
        "unexpected error text: {text}"
    );
}

#[test]
fn namespaces_are_cached_as_persistents() {
    let harness = VirtualHarness::new();
    assert_eq!(harness.engine.persistent_slot_count(), 10);
    let stored = harness
        .engine
        .take_persistent::<rquickjs::Object<'static>>("kiana.virtual.namespace.kiana-ai")
        .expect("kiana-ai namespace is cached");
    // 缓存对象与 `__kiana_virtual` 里的 namespace 是同一个 JS 对象。
    let same = harness.engine.with_js(|ctx| {
        let restored = stored.restore(&ctx).expect("restore persistent");
        ctx.globals().set("__restored_namespace", restored).unwrap();
        ctx.eval::<bool, _>(
            r#"globalThis.__restored_namespace === globalThis.__kiana_virtual["@earendil-works/kiana-ai"]"#,
        )
        .unwrap()
    });
    assert!(same);
}

#[test]
fn install_is_idempotent() {
    let harness = VirtualHarness::new();
    harness.eval::<()>("void (globalThis.__saved = globalThis.__kiana_virtual)");
    virtual_modules::install(&harness.engine).expect("second install");
    let same: bool = harness.eval("globalThis.__saved === globalThis.__kiana_virtual");
    assert!(same);
}

#[test]
fn every_key_imports() {
    let harness = VirtualHarness::new();
    for key in KEYS {
        harness.bind_module(key, "__imported_module");
        let kind: String = harness.eval("typeof globalThis.__imported_module");
        assert_eq!(kind, "object", "importing {key}");
    }
}

#[test]
fn importing_the_same_key_twice_returns_the_same_object() {
    let harness = VirtualHarness::new();
    let same: bool = harness.eval_promise(
        r#"Promise.all([import("typebox"), import("typebox")]).then(([a, b]) => a === b)"#,
    );
    assert!(same);
    let same_kiana: bool = harness.eval_promise(
        r#"Promise.all([
            import("@earendil-works/kiana-ai/models"),
            import("@earendil-works/kiana-ai/models"),
        ]).then(([a, b]) => a === b)"#,
    );
    assert!(same_kiana);
}

#[test]
fn typebox_is_the_real_embedded_package() {
    let harness = VirtualHarness::new();
    let expected = fixture("typebox_cases.json");
    harness.bind_module("typebox", "__tb");
    let type_kind: String = harness.eval("typeof globalThis.__tb.Type.Object");
    assert_eq!(type_kind, "function");
    let default_is_type: bool = harness.eval("globalThis.__tb.default === globalThis.__tb.Type");
    assert!(
        default_is_type,
        "typebox barrel default is the Type namespace"
    );
    let schema = harness.eval_json(
        r#"globalThis.__tb.Type.Object({
            name: globalThis.__tb.Type.String(),
            age: globalThis.__tb.Type.Optional(globalThis.__tb.Type.Number()),
            tags: globalThis.__tb.Type.Array(globalThis.__tb.Type.String()),
        })"#,
    );
    assert_eq!(schema, expected["schemaJson"]);
}

#[test]
fn typebox_compile_and_value_entries_work() {
    let harness = VirtualHarness::new();
    harness.bind_module("typebox", "__tb");
    harness.bind_module("typebox/compile", "__tb_compile");
    harness.bind_module("typebox/value", "__tb_value");
    let ok: bool = harness.eval(
        r#"
        (() => {
            const schema = globalThis.__tb.Type.Object({ name: globalThis.__tb.Type.String() });
            const compiled = globalThis.__tb_compile.Compile(schema);
            return compiled.Check({ name: "x" }) && !compiled.Check({});
        })()
        "#,
    );
    assert!(ok);
    let converted = harness.eval_json(
        r#"globalThis.__tb_value.Value.Convert(
            globalThis.__tb.Type.Object({
                name: globalThis.__tb.Type.String(),
                age: globalThis.__tb.Type.Number(),
            }),
            { name: 123, age: "42" },
        )"#,
    );
    assert_eq!(converted, json!({ "name": "123", "age": 42 }));
}

#[test]
fn typebox_aliases_share_the_same_namespace_objects() {
    let harness = VirtualHarness::new();
    for (a, b) in [
        ("typebox", "@sinclair/typebox"),
        ("typebox/compile", "@sinclair/typebox/compile"),
        ("typebox/value", "@sinclair/typebox/value"),
    ] {
        let same: bool = harness.eval(&format!(
            r#"globalThis.__kiana_virtual[{a:?}] === globalThis.__kiana_virtual[{b:?}]"#
        ));
        assert!(same, "{a} and {b} are the same namespace");
    }
    let distinct: bool = harness.eval(
        r#"globalThis.__kiana_virtual["typebox"] !== globalThis.__kiana_virtual["typebox/compile"]"#,
    );
    assert!(distinct);
}

#[test]
fn kiana_agent_core_is_an_empty_namespace() {
    let harness = VirtualHarness::new();
    let keys: Vec<String> = harness
        .eval(r#"Object.keys(globalThis.__kiana_virtual["@earendil-works/kiana-agent-core"])"#);
    assert!(keys.is_empty(), "expected no runtime exports, got {keys:?}");
}

#[test]
fn kiana_ai_js_values_match_the_typescript_reference() {
    let harness = VirtualHarness::new();
    harness.install_test_globals();
    let expected = fixture("js_cases.json");

    let string_enum = harness.eval_json(
        r#"globalThis.__kiana_virtual["@earendil-works/kiana-ai"].StringEnum(
            ["add", "sub"],
            { description: "op", default: "add" },
        )"#,
    );
    assert_eq!(string_enum, expected["stringEnum"]);

    let uuid = harness.eval_json(
        r#"(() => {
            const { uuidv7 } = globalThis.__kiana_virtual["@earendil-works/kiana-ai"];
            const fixed = uuidv7(1700000000000);
            const generated = [uuidv7(), uuidv7()];
            const shape = /^[0-9a-f]{8}-[0-9a-f]{4}-7[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/;
            return {
                fixed: [fixed.length, fixed[14]],
                generated: generated.map((id) => [id.length, id[14], shape.test(id)]),
                shape: shape.test(fixed),
                prefix: fixed.slice(0, 8),
            };
        })()"#,
    );
    assert_eq!(uuid["shape"], json!(true));
    // 随机字节由测试桩提供，序列位（variant）与 Node 夹具不同；版本/长度/形状必须一致。
    assert_eq!(uuid["fixed"], json!([36, "7"]));
    assert_eq!(uuid["generated"], json!([[36, "7", true], [36, "7", true]]));
    let fixture_fixed = expected["uuid"]["fixed"].as_str().expect("fixture uuid");
    assert_eq!(uuid["prefix"], fixture_fixed[..8]);

    let range_error = harness
        .try_eval::<()>(r#"globalThis.__kiana_virtual["@earendil-works/kiana-ai"].uuidv7(1.5)"#)
        .expect_err("non-integer timestamp");
    assert_eq!(range_error.0, "RangeError");
    assert_eq!(
        range_error.1,
        "UUIDv7 timestamp must be an integer between 0 and 281474976710655"
    );
}

#[test]
fn assistant_message_event_stream_matches_the_typescript_reference() {
    let harness = VirtualHarness::new();
    harness.install_test_globals();
    let expected = fixture("js_cases.json");
    let actual: String = harness.eval_promise(
        r#"(async () => {
            const create = globalThis.__kiana_virtual["@earendil-works/kiana-ai"].createAssistantMessageEventStream;
            const drain = async (stream) => {
                const events = [];
                for await (const event of stream) events.push(event);
                return events;
            };
            const cases = {};
            {
                const stream = create();
                stream.push({ type: "text_start", contentIndex: 0 });
                stream.push({ type: "text_delta", contentIndex: 0, delta: "hi" });
                const collecting = drain(stream);
                stream.push({ type: "done", message: { role: "assistant", timestamp: 0, content: [{ type: "text", text: "hi" }], usage: {}, stopReason: "end" } });
                cases.done = { events: await collecting, result: await stream.result() };
            }
            {
                const stream = create();
                stream.push({ type: "error", error: { role: "assistant", timestamp: 0, content: [], usage: {}, stopReason: "error", errorMessage: "boom" } });
                cases.error = { result: await stream.result() };
            }
            {
                const stream = create();
                stream.push({ type: "text_start", contentIndex: 0 });
                const collecting = drain(stream);
                stream.end({ role: "assistant", timestamp: 0, content: [], usage: {}, stopReason: "end" });
                cases.end = { events: await collecting, result: await stream.result() };
            }
            return JSON.stringify(cases);
        })()"#,
    );
    let actual: Value = serde_json::from_str(&actual).expect("event stream JSON");
    assert_eq!(actual, expected["eventStream"]);
}

#[test]
fn tool_call_arguments_stream_matches_the_typescript_reference() {
    let harness = VirtualHarness::new();
    let expected = fixture("stream_cases.json");
    let actual = harness.eval_json(
        r#"(() => {
            const Stream = globalThis.__kiana_virtual["@earendil-works/kiana-ai"].ToolCallArgumentsStream;
            const stream = new Stream();
            const pieces = ['{"name": "ba', 'sh", "args": {"comm', 'and": "ls -la", "nested": [1, 2', '3]}}'];
            const steps = [];
            for (const piece of pieces) steps.push({ piece, args: stream.append(piece) });
            const replacement = '{"name": "bash", "args": {"command": "ls -la", "nested": [1, 2, 3]}}';
            steps.push({ replace: replacement });
            const replaced = stream.replace(replacement);
            const malformedStream = new Stream();
            malformedStream.append('{"a": 1');
            return {
                steps,
                replaced,
                text: stream.text,
                finish: stream.finish(),
                malformed: malformedStream.finish(),
            };
        })()"#,
    );
    assert_eq!(actual["steps"], expected["steps"]);
    assert_eq!(actual["replaced"], expected["replaced"]);
    assert_eq!(actual["text"], expected["text"]);
    assert_eq!(actual["finish"], expected["finish"]);
    assert_eq!(actual["malformed"], expected["malformed"]);
}

#[test]
fn kiana_tui_js_container_matches_the_typescript_reference() {
    let harness = VirtualHarness::new();
    let expected = fixture("container_cases.json");
    let actual = harness.eval_json(
        r#"(() => {
            const { Container } = globalThis.__kiana_virtual["@earendil-works/kiana-tui"];
            const text = (lines) => ({
                renders: [],
                invalidated: 0,
                render(width) { this.renders.push(width); return lines.map((line) => line + "|" + width); },
                invalidate() { this.invalidated++; },
            });
            const a = text(["a1", "a2"]);
            const b = text(["b1"]);
            const container = new Container();
            container.addChild(a);
            container.addChild(b);
            const first = { render: container.render(10), children: container.children.length };
            container.invalidate();
            const invalidation = { a: a.invalidated, b: b.invalidated };
            container.removeChild(a);
            const afterRemove = container.render(4);
            container.clear();
            const afterClear = { children: container.children.length, lines: container.render(3) };
            return { first, invalidation, afterRemove, afterClear, cursor: globalThis.__kiana_virtual["@earendil-works/kiana-tui"].CURSOR_MARKER };
        })()"#,
    );
    assert_eq!(actual["first"], expected["first"]);
    assert_eq!(actual["invalidation"], expected["invalidation"]);
    assert_eq!(actual["afterRemove"], expected["afterRemove"]);
    assert_eq!(actual["afterClear"], expected["afterClear"]);
    assert_eq!(actual["cursor"], expected["cursor"]);
}

#[test]
fn kiana_tui_render_fake_cursor_and_marked_stub() {
    let harness = VirtualHarness::new();
    let rendered: String = harness
        .eval(r#"globalThis.__kiana_virtual["@earendil-works/kiana-tui"].renderFakeCursor("abc")"#);
    assert_eq!(rendered, "\x1b_kiana:fc\x07abc\x1b_kiana:/fc\x07");
    let error = harness
        .try_eval::<()>(r#"new globalThis.__kiana_virtual["@earendil-works/kiana-tui"].Marked()"#)
        .expect_err("marked is not bundled");
    assert_eq!(error.0, "Error");
    assert_eq!(error.1, "marked is not bundled in kiana");
}

#[test]
fn kiana_tui_vstack_is_pure_js_and_hstack_uses_the_bridge() {
    let harness = VirtualHarness::new();
    let vstack = harness.eval_json(
        r#"(() => {
            const { VStack } = globalThis.__kiana_virtual["@earendil-works/kiana-tui"];
            const child = (lines) => ({ render: () => lines, invalidate() {} });
            const stack = new VStack([child(["a", "b"]), child(["c"])], { gap: 1 });
            return stack.render(20);
        })()"#,
    );
    assert_eq!(vstack, json!(["a", "b", "", "c"]));

    let error = harness
        .try_eval::<()>(
            r#"new globalThis.__kiana_virtual["@earendil-works/kiana-tui"].HStack([
                { render: () => ["x"], invalidate() {} },
            ]).render(10)"#,
        )
        .expect_err("HStack needs the native bridge for compositeTuiLine");
    assert_eq!(error.0, "Error");
    assert_eq!(
        error.1,
        "kiana: native bridge member \"kiana-tui.visibleWidth\" is not installed"
    );
}

#[test]
fn members_without_an_implementation_still_throw_a_clear_error_on_use() {
    let harness = VirtualHarness::new();
    let types: Vec<String> = harness.eval(
        r#"(() => {
            const ns = globalThis.__kiana_virtual["@earendil-works/kiana-ai"];
            return [
                typeof ns.InMemoryCredentialStore,
                typeof ns.registerImagesApiProvider,
                typeof globalThis.__kiana_virtual["@earendil-works/kiana-tui"].Key,
            ];
        })()"#,
    );
    assert_eq!(types, vec!["function", "function", "object"]);

    // 只有确实没有实现（由其它单元提供）的成员在**使用**时才抛错。
    let function_error = harness
        .try_eval::<()>(
            r#"new globalThis.__kiana_virtual["@earendil-works/kiana-ai"].InMemoryCredentialStore()"#,
        )
        .expect_err("bridge absent");
    assert_eq!(function_error.0, "Error");
    assert_eq!(
        function_error.1,
        "kiana: native bridge member \"kiana-ai.InMemoryCredentialStore\" is not installed"
    );

    let object_error = harness
        .try_eval::<()>(r#"globalThis.__kiana_virtual["@earendil-works/kiana-tui"].Key.up"#)
        .expect_err("bridge absent");
    assert_eq!(
        object_error.1,
        "kiana: native bridge member \"kiana-tui.Key\" is not installed"
    );

    // TS 的常量是模块自身的值：没有桥也可读（§8.6「直接值」）。
    let constants: Vec<String> = harness.eval(
        r#"(() => {
            const ca = globalThis.__kiana_virtual["@earendil-works/kiana-coding-agent"];
            return [ca.CONFIG_DIR_NAME, ca.VERSION, String(ca.DEFAULT_MAX_BYTES), String(ca.DEFAULT_MAX_LINES)];
        })()"#,
    );
    assert_eq!(constants, vec![".kiana", "1.4.0", "51200", "2000"]);
}

#[test]
fn native_members_are_forwarded_to_the_bridge() {
    let harness = VirtualHarness::new();
    let engine = &harness.engine;

    // Function member (canonical bridge id for the models namespace; compat shares the proxy).
    virtual_modules::set_native_member(engine, "kiana-ai/models", "calculateCost", |ctx| {
        Ok(
            rquickjs::Function::new(ctx.clone(), |model: String, usage: String| {
                format!("{model}:{usage}")
            })?
            .into_value(),
        )
    })
    .expect("set calculateCost");
    let value: String = harness
        .eval(r#"globalThis.__kiana_virtual["@earendil-works/kiana-ai"].calculateCost("m", "u")"#);
    assert_eq!(value, "m:u");
    harness.bind_module("@earendil-works/kiana-ai", "__kiana_ai_module");
    let via_import: String =
        harness.eval(r#"globalThis.__kiana_ai_module.calculateCost("m", "u")"#);
    assert_eq!(via_import, "m:u");

    // 兄弟兜底：实现方把共享成员装到 `kiana-ai` 而不是 `kiana-ai/models` 时也能读到。
    virtual_modules::set_native_member(engine, "kiana-ai", "isModelType", |ctx| {
        Ok(
            rquickjs::Function::new(ctx.clone(), |kind: String| format!("sibling:{kind}"))?
                .into_value(),
        )
    })
    .expect("set isModelType");
    let sibling: String = harness.eval(
        r#"globalThis.__kiana_virtual["@earendil-works/kiana-ai/models"].isModelType("chat")"#,
    );
    assert_eq!(sibling, "sibling:chat");

    // 常量是 Rust 直接值（不经桥），并且是 enumerable 的（与 ESM 导出面一致）。
    let dir: String = harness.eval(
        r#"globalThis.__kiana_virtual["@earendil-works/kiana-coding-agent"].CONFIG_DIR_NAME"#,
    );
    assert_eq!(dir, ".kiana");
    let has_key: bool = harness.eval(
        r#"Object.keys(globalThis.__kiana_virtual["@earendil-works/kiana-coding-agent"]).includes("CONFIG_DIR_NAME")"#,
    );
    assert!(has_key);

    // Class member: `extends` reads the proxied `prototype`, `super()` constructs the real class.
    harness.eval::<()>(
        r#"
        globalThis.__kiana_native["kiana-tui"] = globalThis.__kiana_native["kiana-tui"] || {};
        globalThis.__kiana_native["kiana-tui"].Text = class NativeText {
            constructor(value) { this.value = value; }
            render(width) { return [this.value + ":" + width]; }
        };
        "#,
    );
    let subclass = harness.eval_json(
        r#"(() => {
            const Base = globalThis.__kiana_virtual["@earendil-works/kiana-tui"].Text;
            class Sub extends Base {
                constructor(value) { super(value + "!"); }
            }
            const sub = new Sub("hi");
            const direct = new globalThis.__kiana_virtual["@earendil-works/kiana-tui"].Text("yo");
            return [
                sub.render(5),
                sub instanceof globalThis.__kiana_native["kiana-tui"].Text,
                direct.render(2),
                direct instanceof globalThis.__kiana_native["kiana-tui"].Text,
            ];
        })()"#,
    );
    assert_eq!(subclass, json!([["hi!:5"], true, ["yo:2"], true]));
}

#[test]
fn define_tool_is_the_identity_function() {
    let harness = VirtualHarness::new();
    let identity: bool = harness.eval(
        r#"(() => {
            const defineTool = globalThis.__kiana_virtual["@earendil-works/kiana-coding-agent"].defineTool;
            const definition = { name: "x" };
            return defineTool(definition) === definition;
        })()"#,
    );
    assert!(identity);
}

#[test]
fn coding_agent_namespace_has_the_34_runtime_exports() {
    let harness = VirtualHarness::new();
    let mut keys: Vec<String> = harness
        .eval(r#"Object.keys(globalThis.__kiana_virtual["@earendil-works/kiana-coding-agent"])"#);
    keys.sort();
    let mut expected: Vec<&str> = CODING_AGENT_EXPORTS.to_vec();
    assert_eq!(keys.len(), 34);
    expected.sort_unstable();
    assert_eq!(keys, expected);
}

/// TS 运行期导出夹具（`exports.json`，由 upstream-reference 的真实模块生成；pair = 名字 + `typeof`）。
fn export_fixture(label: &str) -> Vec<(String, String)> {
    fixture("exports.json")[label]
        .as_array()
        .unwrap_or_else(|| panic!("fixture label {label}"))
        .iter()
        .map(|entry| {
            (
                entry[0].as_str().expect("export name").to_owned(),
                entry[1].as_str().expect("typeof").to_owned(),
            )
        })
        .collect()
}

/// 导入 `specifier`，返回排序后的 `(导出名, typeof)`；`default` 是 jiti 互操作加的，排除。
fn imported_surface(harness: &VirtualHarness, specifier: &str) -> Vec<(String, String)> {
    let script = format!(
        r#"import({specifier:?}).then((ns) => JSON.stringify(
            Object.keys(ns)
                .filter((key) => key !== "default")
                .map((key) => [key, typeof ns[key]]),
        ))"#
    );
    let text: String = harness.eval_promise(&script);
    let mut entries: Vec<(String, String)> = serde_json::from_str(&text).expect("surface JSON");
    entries.sort();
    entries
}

#[test]
fn kiana_ai_compat_surface_matches_the_typescript_compat_module() {
    let harness = VirtualHarness::new();
    for specifier in [
        "@earendil-works/kiana-ai",
        "@earendil-works/kiana-ai/compat",
        "@mariozechner/kiana-ai",
        "@mariozechner/kiana-ai/compat",
    ] {
        assert_eq!(
            imported_surface(&harness, specifier),
            export_fixture("compat"),
            "surface of {specifier}"
        );
    }
    assert_eq!(
        imported_surface(&harness, "@earendil-works/kiana-ai/models"),
        export_fixture("models"),
    );
    assert_eq!(
        imported_surface(&harness, "@earendil-works/kiana-ai/providers/all"),
        export_fixture("providersAll"),
    );
}

#[test]
fn kiana_tui_surface_matches_the_typescript_module_plus_design_extras() {
    let harness = VirtualHarness::new();
    let mut expected = export_fixture("tui");
    for name in KIANA_TUI_CODING_AGENT_EXPORTS {
        expected.push((name.to_owned(), "function".to_owned()));
    }
    expected.sort();
    let actual = imported_surface(&harness, "@earendil-works/kiana-tui");
    assert_eq!(actual, expected);
    // `packages/tui/src/index.ts` 不导出 `Stack`（只导出 VStack/HStack）：`import { Stack }`
    // 在 TS 里是链接错误，这里也必须不存在（review fix）。
    assert!(
        !actual.iter().any(|(name, _)| name == "Stack"),
        "kiana-tui must not export Stack"
    );
    // 设计 §5.1 的 19 个语料符号必须全部在导出面里；语料按函数/对象形状使用它们，缺失或形状不符都会让导入方失败。
    let names: Vec<&str> = actual.iter().map(|(name, _)| name.as_str()).collect();
    for symbol in KIANA_TUI_CORPUS_EXPORTS {
        assert!(names.contains(&symbol), "missing kiana-tui export {symbol}");
    }
    let key_kind = &actual
        .iter()
        .find(|(name, _)| name == "Key")
        .expect("Key")
        .1;
    assert_eq!(key_kind, "object");
}

#[test]
fn oauth_and_agent_core_are_empty_namespaces() {
    let harness = VirtualHarness::new();
    for specifier in [
        "@earendil-works/kiana-ai/oauth",
        "@mariozechner/kiana-ai/oauth",
        "@earendil-works/kiana-agent-core",
        "@mariozechner/kiana-agent-core",
    ] {
        assert!(
            imported_surface(&harness, specifier).is_empty(),
            "{specifier} has no runtime exports in TS"
        );
    }
}

#[test]
fn coding_agent_surface_is_the_design_34_exports() {
    let harness = VirtualHarness::new();
    let actual: Vec<String> = imported_surface(&harness, "@earendil-works/kiana-coding-agent")
        .into_iter()
        .map(|(name, _)| name)
        .collect();
    let mut expected: Vec<String> = CODING_AGENT_EXPORTS
        .iter()
        .map(|name| (*name).to_owned())
        .collect();
    expected.sort_unstable();
    assert_eq!(actual, expected);
}

#[test]
fn default_imports_match_the_jiti_interop() {
    // jiti 对没有 `default` 的模块把 namespace 作为 default（`probe_ext_default.ts`：91/138 个
    // 成员，值相同）。QuickJS 需要真实的 `export default`；TS 侧不存在 `default` 键。
    let harness = VirtualHarness::new();
    harness.engine.with_js(|ctx| {
        let source = r#"
            import tui from "@earendil-works/kiana-tui";
            import ai from "@earendil-works/kiana-ai";
            import models from "@earendil-works/kiana-ai/models";
            import ca from "@earendil-works/kiana-coding-agent";
            import core from "@earendil-works/kiana-agent-core";
            import typebox from "typebox";
            export const checks = [
                typeof tui === "object",
                tui.Container === globalThis.__kiana_virtual["@earendil-works/kiana-tui"].Container,
                typeof tui.hyperlink === "function",
                typeof ai === "object",
                ai.Type.Object === typebox.Object,
                typeof ai.stream === "function",
                typeof models.calculateCost === "function",
                ca.CONFIG_DIR_NAME === ".kiana",
                typeof core === "object",
                Object.keys(core).length === 0,
                typeof typebox.Object === "function",
                ai.Type === typebox,
            ];
        "#;
        let module = rquickjs::Module::declare(ctx.clone(), "default-imports.js?v=1", source)
            .expect("declare default-import module");
        let (_module, promise) = module.eval().expect("evaluate default-import module");
        promise
            .finish::<rquickjs::Value>()
            .expect("module finished");
        let namespace: rquickjs::Object = _module.namespace().expect("namespace");
        let checks: Vec<bool> = namespace.get("checks").expect("checks export");
        assert!(checks.iter().all(|ok| *ok), "{checks:?}");
    });
}

#[test]
fn shared_members_keep_the_typescript_object_identity() {
    // TS：compat 再导出 index.ts，index 再导出 models.ts，因此两个 namespace 里是同一个函数对象。
    let harness = VirtualHarness::new();
    let same: Vec<bool> = harness.eval(
        r#"(() => {
            const ai = globalThis.__kiana_virtual["@earendil-works/kiana-ai"];
            const models = globalThis.__kiana_virtual["@earendil-works/kiana-ai/models"];
            return [
                ai.calculateCost === models.calculateCost,
                ai.isModelType === models.isModelType,
                ai.clampThinkingLevel === models.clampThinkingLevel,
                ai.createModels === models.createModels,
                ai.AI_TELEMETRY_SCHEMA === models.AI_TELEMETRY_SCHEMA,
                ai.Type === globalThis.__kiana_virtual["typebox"].Type,
            ];
        })()"#,
    );
    assert!(same.iter().all(|equal| *equal), "{same:?}");
    let same_import: bool = harness.eval_promise(
        r#"Promise.all([
            import("@earendil-works/kiana-ai"),
            import("@earendil-works/kiana-ai/models"),
        ]).then(([ai, models]) =>
            ai.calculateCost === models.calculateCost &&
            ai.isModelType === models.isModelType &&
            ai.AI_TELEMETRY_SCHEMA === models.AI_TELEMETRY_SCHEMA
        )"#,
    );
    assert!(same_import);
}

#[test]
fn kiana_tui_hyperlink_matches_the_typescript_implementation() {
    // packages/tui/src/terminal-image.ts:745-747.
    let harness = VirtualHarness::new();
    let rendered: String = harness.eval(
        r#"globalThis.__kiana_virtual["@earendil-works/kiana-tui"].hyperlink("kiana", "https://kiana.dev")"#,
    );
    assert_eq!(
        rendered,
        "\x1b]8;;https://kiana.dev\x1b\\kiana\x1b]8;;\x1b\\"
    );
    harness.bind_module("@earendil-works/kiana-tui", "__tui_hyperlink");
    let via_import: String =
        harness.eval(r#"globalThis.__tui_hyperlink.hyperlink("x", "https://example.test/a")"#);
    assert_eq!(
        via_import,
        "\x1b]8;;https://example.test/a\x1b\\x\x1b]8;;\x1b\\"
    );
}

#[test]
fn install_succeeds_with_the_real_module_resolver() {
    // 回归：`install()` 通过 runtime 的 Resolver/Loader import 内部 helper；QuickJS 先规范化
    // specifier 再查模块缓存，所以 `kiana-virtual/helpers/*.js` 必须能由真实解析器解析。
    let harness = VirtualHarness::new_with_real_resolver();
    for key in KEYS {
        harness.bind_module(key, "__real_import");
        let kind: String = harness.eval("typeof globalThis.__real_import");
        assert_eq!(kind, "object", "import {key} through the real resolver");
    }
    harness.bind_module("typebox", "__real_typebox");
    harness.bind_module("typebox/compile", "__real_typebox_compile");
    let compiled: bool = harness.eval(
        r#"(() => {
            const schema = globalThis.__real_typebox.Type.Object({
                name: globalThis.__real_typebox.Type.String(),
            });
            return globalThis.__real_typebox_compile.Compile(schema).Check({ name: "x" });
        })()"#,
    );
    assert!(compiled);
}

#[test]
fn coding_agent_constants_are_readable_without_any_bridge() {
    // TS：这些是模块自身的 `const`；import 后立即可读（不依赖其它单元的桥）。
    let harness = VirtualHarness::new();
    harness.bind_module("@earendil-works/kiana-coding-agent", "__ca_constants");
    let constants: Value = harness.eval_json(
        r#"[
            globalThis.__ca_constants.CONFIG_DIR_NAME,
            globalThis.__ca_constants.VERSION,
            globalThis.__ca_constants.DEFAULT_MAX_BYTES,
            globalThis.__ca_constants.DEFAULT_MAX_LINES,
        ]"#,
    );
    assert_eq!(constants, json!([".kiana", "1.4.0", 51200, 2000]));
    // 与 Rust 侧的唯一来源一致。
    assert_eq!(
        constants[0].as_str().unwrap(),
        kiana_core::config::CONFIG_DIR_NAME
    );
    assert_eq!(constants[1].as_str().unwrap(), kiana_core::config::VERSION);
    assert_eq!(
        constants[2].as_u64().unwrap() as usize,
        kiana_tools::truncate::DEFAULT_MAX_BYTES
    );
    assert_eq!(
        constants[3].as_u64().unwrap() as usize,
        kiana_tools::truncate::DEFAULT_MAX_LINES
    );
    let enumerable: bool = harness.eval(
        r#"Object.keys(globalThis.__kiana_virtual["@earendil-works/kiana-coding-agent"]).includes("VERSION")"#,
    );
    assert!(enumerable);
}

#[test]
fn kiana_ai_static_constants_match_the_rust_port() {
    let harness = VirtualHarness::new();
    harness.bind_module("@earendil-works/kiana-ai", "__ai_constants");
    let version: String = harness.eval("globalThis.__ai_constants.KIANA_AI_VERSION");
    assert_eq!(version, kiana_ai::utils::version::KIANA_AI_VERSION);
    let retry: f64 = harness.eval("globalThis.__ai_constants.DEFAULT_MAX_AGENT_RETRY_DELAY_MS");
    assert_eq!(
        retry,
        kiana_ai::utils::retry::DEFAULT_MAX_AGENT_RETRY_DELAY_MS as f64
    );
    for (export, expected) in [
        (
            "ANTHROPIC_API_KEY_ENV",
            kiana_ai::env_api_keys::ANTHROPIC_API_KEY_ENV,
        ),
        (
            "ANTHROPIC_AUTH_TOKEN_ENV",
            kiana_ai::env_api_keys::ANTHROPIC_AUTH_TOKEN_ENV,
        ),
        (
            "ANTHROPIC_FEDERATION_RULE_ID_ENV",
            kiana_ai::env_api_keys::ANTHROPIC_FEDERATION_RULE_ID_ENV,
        ),
        (
            "ANTHROPIC_IDENTITY_TOKEN_FILE_ENV",
            kiana_ai::env_api_keys::ANTHROPIC_IDENTITY_TOKEN_FILE_ENV,
        ),
        (
            "ANTHROPIC_OAUTH_TOKEN_ENV",
            kiana_ai::env_api_keys::ANTHROPIC_OAUTH_TOKEN_ENV,
        ),
        (
            "ANTHROPIC_ORGANIZATION_ID_ENV",
            kiana_ai::env_api_keys::ANTHROPIC_ORGANIZATION_ID_ENV,
        ),
        (
            "ANTHROPIC_SERVICE_ACCOUNT_ID_ENV",
            kiana_ai::env_api_keys::ANTHROPIC_SERVICE_ACCOUNT_ID_ENV,
        ),
        (
            "ANTHROPIC_WORKSPACE_ID_ENV",
            kiana_ai::env_api_keys::ANTHROPIC_WORKSPACE_ID_ENV,
        ),
    ] {
        let actual: String = harness.eval(&format!("globalThis.__ai_constants.{export}"));
        assert_eq!(actual, expected, "{export}");
    }
    let attribution: Value = harness.eval_json("globalThis.__ai_constants.KIANA_ATTRIBUTION");
    assert_eq!(
        attribution,
        serde_json::to_value(&*kiana_ai::utils::identity::KIANA_ATTRIBUTION)
            .expect("attribution JSON")
    );
    let schema: String =
        harness.eval("JSON.stringify(globalThis.__ai_constants.AI_TELEMETRY_SCHEMA)");
    let mut actual: Value = serde_json::from_str(&schema).expect("schema JSON");
    let mut expected: Value = serde_json::to_value(&*kiana_ai::telemetry::AI_TELEMETRY_SCHEMA)
        .expect("telemetry schema value");
    // JS `JSON.stringify(1)` is "1"; serde_json renders the f64 field as 1.0. Same value.
    actual["version"] = json!(1);
    expected["version"] = json!(1);
    assert_eq!(actual, expected);
}

#[test]
fn module_sources_are_registered_for_loader_runtimes() {
    let harness = VirtualHarness::new();
    let engine = &harness.engine;
    for key in KEYS {
        assert!(
            engine.module_source(key).is_some(),
            "public key {key} must be served by register_module_source"
        );
    }
    for module in [
        "typebox/build/index.mjs",
        "typebox/build/compile/index.mjs",
        "typebox/build/value/index.mjs",
        "kiana-virtual/kiana-agent-core.js",
        "kiana-virtual/kiana-tui.js",
        "kiana-virtual/kiana-ai.js",
        "kiana-virtual/kiana-ai/models.js",
        "kiana-virtual/kiana-ai/oauth.js",
        "kiana-virtual/kiana-ai/providers-all.js",
        "kiana-virtual/kiana-coding-agent.js",
        "kiana-virtual/helpers/bridge.js",
        "kiana-virtual/helpers/kiana-ai.js",
        "kiana-virtual/helpers/kiana-tui.js",
        "kiana-virtual/helpers/kiana-coding-agent.js",
    ] {
        assert!(
            engine.module_source(module).is_some(),
            "canonical module {module} must be registered"
        );
    }
    // typebox 公开键用相对导入安全的兜底 shim，而不是桶文件源。
    let typebox_source = engine.module_source("typebox").expect("typebox key source");
    assert!(typebox_source.starts_with("export * from \"typebox/build/index.mjs\";"));
}

#[test]
fn embedded_tables_have_the_expected_shape() {
    // 691 个 typebox `.mjs` + 7 个 kiana-* shim + 4 个安装期 helper 模块。
    assert_eq!(virtual_modules::virtual_files().len(), 691 + 11);
    for helper in [
        "kiana-virtual/helpers/bridge.js",
        "kiana-virtual/helpers/kiana-ai.js",
        "kiana-virtual/helpers/kiana-tui.js",
        "kiana-virtual/helpers/kiana-coding-agent.js",
    ] {
        assert!(
            virtual_modules::virtual_files()
                .iter()
                .any(|file| file.name == helper),
            "install helper {helper} must be resolvable through VirtualFs"
        );
        assert_eq!(
            virtual_modules::virtual_fs().canonical_name(helper),
            Some(helper)
        );
    }
    let typebox_files = virtual_modules::virtual_files()
        .iter()
        .filter(|file| file.name.starts_with("typebox/"))
        .count();
    assert_eq!(typebox_files, 691);
    for file in virtual_modules::virtual_files() {
        assert!(
            std::str::from_utf8(file.source).is_ok(),
            "{} is valid UTF-8",
            file.name
        );
        assert!(file.name.ends_with(".mjs") || file.name.ends_with(".js"));
    }
}

#[test]
fn typebox_namespaces_survive_gc() {
    let harness = VirtualHarness::new();
    harness.engine.run_gc();
    harness.bind_module("typebox", "__tb_after_gc");
    let ok: bool = harness.eval(
        r#"typeof globalThis.__tb_after_gc.Type.Object === "function"
            && JSON.stringify(globalThis.__tb_after_gc.Type.Object({})).includes('"type":"object"')"#,
    );
    assert!(ok);
}

#[test]
fn a_second_engine_gets_its_own_namespaces() {
    let first = VirtualHarness::new();
    let second = VirtualHarness::new();
    assert!(virtual_modules::is_installed(&first.engine));
    assert!(virtual_modules::is_installed(&second.engine));
    let distinct: bool = first.engine.with_js(|ctx| {
        let local: bool = ctx
            .eval::<bool, _>(
                "globalThis.__kiana_virtual !== undefined && typeof globalThis.__kiana_virtual.typebox.Type.Object === \"function\"",
            )
            .expect("typebox in the first engine");
        local
    });
    assert!(distinct);
    let _ = second;
}

#[test]
fn js_engine_is_reused_for_the_bridge_global() {
    let harness = VirtualHarness::new();
    let engine: &JsEngine = &harness.engine;
    let bridge_exists: bool = harness.eval("typeof globalThis.__kiana_native === \"object\"");
    assert!(bridge_exists);
    // set_native_member 会复用同一个桥对象。
    let before: bool = harness.eval("globalThis.__bridge_before = globalThis.__kiana_native; true");
    let _ = before;
    virtual_modules::set_native_member(engine, "kiana-ai", "marker", |ctx| {
        Ok(rquickjs::Value::new_undefined(ctx.clone()))
    })
    .expect("set marker");
    let same: bool = harness.eval(
        "globalThis.__bridge_before === globalThis.__kiana_native && \"marker\" in globalThis.__kiana_native[\"kiana-ai\"]",
    );
    assert!(same);
}

// ===========================================================================
// Rust-backed members + pure-JS helper ports (review fixes)
// ===========================================================================

/// Golden values produced by upstream-reference: `packages/ai/src/{compat,utils/*}.ts` and
/// `packages/coding-agent/src/core/{tools/*,messages,compaction/utils}.ts`.
fn native_fixture() -> Value {
    fixture("native_cases.json")
}

fn tool_fixture() -> Value {
    fixture("tool_cases.json")
}

#[test]
fn rust_backed_kiana_ai_models_match_the_typescript_reference() {
    let harness = VirtualHarness::new();
    let expected = native_fixture();
    let expected_models = &expected["modelsCases"];

    let cost = harness.eval_json(
        r#"(() => {
            const ai = globalThis.__kiana_virtual["@earendil-works/kiana-ai"];
            const model = {
                id: "claude-test", name: "Test", api: "anthropic-messages", provider: "anthropic",
                baseUrl: "https://example.test", input: ["text"],
                cost: { input: 3, output: 15, cacheRead: 0.3, cacheWrite: 3.75, tiers: [{ inputTokensAbove: 200000, input: 6, output: 22.5, cacheRead: 0.6, cacheWrite: 7.5 }] },
                contextWindow: 200000, maxTokens: 8192,
            };
            const usage = { input: 250000, output: 500, cacheRead: 100, cacheWrite: 50, cacheWrite1h: 10, totalTokens: 250650, cost: {} };
            const returned = ai.calculateCost(model, usage);
            return { returned, usageCost: usage.cost };
        })()"#,
    );
    assert_eq!(&cost, &expected_models["calculateCost"]);

    let types = harness.eval_json(
        r#"(() => {
            const ai = globalThis.__kiana_virtual["@earendil-works/kiana-ai"];
            const model = { id: "m", provider: "p", api: "anthropic-messages", cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0 }, reasoning: true };
            return [
                "chat",
                ai.getModelType(model),
                ai.getModelType({ ...model, type: "image" }),
            ];
        })()"#,
    );
    assert_eq!(types, expected_models["getModelType"]);

    let checks = harness.eval_json(
        r#"(() => {
            const ai = globalThis.__kiana_virtual["@earendil-works/kiana-ai"];
            const model = { id: "m", provider: "p", api: "anthropic-messages", cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0 } };
            return {
                isModelType: [ai.isModelType(model, "chat"), ai.isModelType(model, "image")],
                hasApi: [ai.hasApi(model, "anthropic-messages"), ai.hasApi(model, "openai-responses")],
                modelsAreEqual: [
                    ai.modelsAreEqual(model, { ...model }),
                    ai.modelsAreEqual(model, { ...model, id: "x" }),
                    ai.modelsAreEqual(null, model),
                ],
            };
        })()"#,
    );
    assert_eq!(checks["isModelType"], expected_models["isModelType"]);
    assert_eq!(checks["hasApi"], expected_models["hasApi"]);
    assert_eq!(checks["modelsAreEqual"], expected_models["modelsAreEqual"]);

    let clamp = harness.eval_json(
        r#"(() => {
            const ai = globalThis.__kiana_virtual["@earendil-works/kiana-ai"];
            const base = { id: "m", provider: "p", api: "a", cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0 } };
            return [
                ai.clampThinkingLevel(base, "high"),
                ai.clampThinkingLevel({ ...base, reasoning: true, thinkingLevelMap: { xhigh: null } }, "xhigh"),
                ai.clampThinkingLevel({ ...base, reasoning: true, thinkingLevelMap: { low: "l" } }, "max"),
                ai.clampThinkingLevel({ ...base, reasoning: false }, "medium"),
                ai.getSupportedThinkingLevels({ ...base, reasoning: true }),
                ai.getSupportedThinkingLevels({ ...base, reasoning: true, thinkingLevelMap: { xhigh: null, max: "m" } }),
            ];
        })()"#,
    );
    assert_eq!(clamp, expected_models["clamp"]);
}

#[test]
fn rust_backed_kiana_ai_helpers_match_the_typescript_reference() {
    let harness = VirtualHarness::new();
    let expected = native_fixture();

    let parsed = harness.eval_json(
        r#"(() => {
            const ai = globalThis.__kiana_virtual["@earendil-works/kiana-ai"];
            return ["", "  ", "{\"a\":1", "{\"a\":1}", "{\"name\":\"ba", "not json", "[1,2", "{\"x\":\"\\q\"}"]
                .map((text) => ai.parseStreamingJson(text));
        })()"#,
    );
    assert_eq!(parsed, expected["parseStreaming"]);

    let env = harness.eval_json(
        r#"(() => {
            const ai = globalThis.__kiana_virtual["@earendil-works/kiana-ai"];
            const env = { ANTHROPIC_API_KEY: "sk-a", ANTHROPIC_AUTH_TOKEN: "tok", OPENAI_API_KEY: "sk-o", GOOGLE_CLOUD_PROJECT: "", EMPTY: "" };
            return {
                findEnvKeys: [
                    ai.findEnvKeys("anthropic", env) ?? null,
                    ai.findEnvKeys("openai", env) ?? null,
                    ai.findEnvKeys("unknown-provider", env) ?? null,
                    ai.findEnvKeys("google-vertex", env) ?? null,
                ],
                getEnvApiKey: [
                    ai.getEnvApiKey("anthropic", env) ?? null,
                    ai.getEnvApiKey("openai", env) ?? null,
                    ai.getEnvApiKey("unknown-provider", env) ?? null,
                ],
            };
        })()"#,
    );
    assert_eq!(env, expected["envCases"]);
}

#[test]
fn rust_backed_coding_agent_helpers_match_the_typescript_reference() {
    let harness = VirtualHarness::new();
    let expected = native_fixture();
    let tools = &expected["tools"];

    let ranges = harness.eval_json(
        r#"(() => {
            const ca = globalThis.__kiana_virtual["@earendil-works/kiana-coding-agent"];
            return [
                ca.formatReadLineRange(undefined, undefined),
                ca.formatReadLineRange(null, null),
                ca.formatReadLineRange(5, null),
                ca.formatReadLineRange("5", "3"),
                ca.formatReadLineRange(1, 0),
                ca.formatReadLineRange("x", 3),
                ca.formatReadLineRange(1.5, 2),
                ca.formatReadLineRange(null, 4),
            ];
        })()"#,
    );
    assert_eq!(ranges, tools["formatReadLineRange"]);

    let truncated = harness.eval_json(
        r#"(() => {
            const ca = globalThis.__kiana_virtual["@earendil-works/kiana-coding-agent"];
            return [
                ca.truncateHead("a\nb\nc"),
                ca.truncateHead("a\nb\nc", { maxLines: 2 }),
                ca.truncateHead("abcdef", { maxBytes: 3 }),
                ca.truncateHead("abcdef\nx", { maxBytes: 3 }),
            ];
        })()"#,
    );
    assert_eq!(truncated, tools["truncateHead"]);

    let frontmatter = harness.eval_json(
        r#"(() => {
            const ca = globalThis.__kiana_virtual["@earendil-works/kiana-coding-agent"];
            return [
                ca.parseFrontmatter("hello"),
                ca.parseFrontmatter("---\na: 1\nb: [x, y]\n---\nbody\n"),
                ca.parseFrontmatter("---\nx: true\n---"),
            ];
        })()"#,
    );
    assert_eq!(frontmatter, tools["parseFrontmatter"]);

    let converted = harness.eval_json(
        r#"(() => {
            const ca = globalThis.__kiana_virtual["@earendil-works/kiana-coding-agent"];
            return ca.convertToLlm([
                { role: "user", content: [{ type: "text", text: "u" }], timestamp: 1 },
                { role: "bashExecution", command: "ls", output: "a", exitCode: 0, cancelled: false, truncated: false, timestamp: 2, excludeFromContext: false },
                { role: "bashExecution", command: "ls", output: "a", exitCode: 0, cancelled: false, truncated: false, timestamp: 3, excludeFromContext: true },
                { role: "custom", customType: "note", content: "hello", timestamp: 4 },
                { role: "branchSummary", summary: "sum", fromId: "x", timestamp: 5 },
                { role: "compactionSummary", summary: "csum", tokensBefore: 10, timestamp: 6 },
            ]);
        })()"#,
    );
    assert_eq!(converted, tools["convertToLlm"]);

    let sizes = harness.eval_json(
        r#"(() => {
            const ca = globalThis.__kiana_virtual["@earendil-works/kiana-coding-agent"];
            return [0, 512, 1023, 1024, 1536, 1048575, 1048576, 1572864, 1.5, -1, 1023.4].map((bytes) => ca.formatSize(bytes));
        })()"#,
    );
    assert_eq!(sizes, tools["formatSize"]);

    let lines = harness.eval_json(
        r#"(() => {
            const ca = globalThis.__kiana_virtual["@earendil-works/kiana-coding-agent"];
            return [
                ca.truncateLine("short"),
                ca.truncateLine("x".repeat(500)),
                ca.truncateLine("x".repeat(501)),
                ca.truncateLine("y".repeat(600), 10),
                ca.truncateLine("é".repeat(600)),
                ca.truncateLine("z".repeat(20), 0),
            ];
        })()"#,
    );
    assert_eq!(lines, tools["truncateLine"]);

    let dir: String = harness
        .eval(r#"globalThis.__kiana_virtual["@earendil-works/kiana-coding-agent"].getAgentDir()"#);
    assert_eq!(dir, kiana_core::config::get_agent_dir().to_string_lossy());
}

#[test]
fn js_transcript_and_text_helpers_match_the_typescript_reference() {
    let harness = VirtualHarness::new();
    let expected = native_fixture();
    let actual = harness.eval_json(
        r#"(() => {
            const ai = globalThis.__kiana_virtual["@earendil-works/kiana-ai"];
            const messages = [
                { role: "system", content: "base prompt", sections: { a: "one", b: null }, toolsAdded: [{ name: "read", description: "r", parameters: { type: "object" } }], timestamp: 7 },
                { role: "user", content: [{ type: "text", text: "hi" }], timestamp: 8 },
                { role: "system", content: [{ type: "text", text: "more" }], sections: { a: "two", c: "three" }, toolsRemoved: [{ name: "read" }], toolsAdded: [{ name: "bash", description: "b", parameters: { type: "object" } }], timestamp: 9 },
            ];
            return {
                transcript: {
                    createInitialSystemMessage: [ai.createInitialSystemMessage(undefined, undefined), ai.createInitialSystemMessage("p", []), ai.createInitialSystemMessage("", [{ name: "x" }])],
                    normalizeContext: ai.normalizeContext({ messages: [{ role: "user", content: "u", timestamp: 1 }] }),
                    getInitialSystemMessage: ai.getInitialSystemMessage(messages),
                    withoutInitialSystemMessage: ai.withoutInitialSystemMessage(messages),
                    getCurrentTools: ai.getCurrentTools(messages),
                    getCurrentSystemMessage: ai.getCurrentSystemMessage(messages),
                    getCurrentSystemPrompt: ai.getCurrentSystemPrompt(messages),
                    collapseSystemMessages: ai.collapseSystemMessages({ messages }),
                    resolveTranscriptTrue: ai.resolveTranscript({ messages }, true),
                    resolveTranscriptFalse: ai.resolveTranscript({ messages }, false),
                    toToolDeclaration: ai.toToolDeclaration({ name: "t", description: "d", parameters: { type: "object", properties: { x: { type: "string" } }, $id: undefined }, constrainedSampling: false }),
                    declarationsEqual: [ai.declarationsEqual({ name: "a", description: "d", parameters: { type: "object" } }, { name: "a", description: "d", parameters: { type: "object" } })],
                    getToolStateChanges: ai.getToolStateChanges([{ name: "a", description: "d", parameters: { type: "object" } }, { name: "b", description: "d", parameters: { type: "object", x: 1 } }], [{ name: "a", description: "d", parameters: { type: "object" } }, { name: "c", description: "d", parameters: { type: "object" } }]),
                    getDeclaredTools: ai.getDeclaredTools(messages),
                    hasToolRedefinitions: [ai.hasToolRedefinitions(messages)],
                    hasNonAdditiveToolChanges: [ai.hasNonAdditiveToolChanges(messages)],
                    resolveTranscriptTools: ["none", "additions", "changes"].map((support) => ai.resolveTranscriptTools(messages, support)),
                },
                text: {
                    contentText: [ai.contentText("s"), ai.contentText([{ type: "text", text: "a" }, { type: "image", data: "x" }, { type: "text", text: "b" }]), ai.contentText([{ type: "text", text: "a" }, { type: "text", text: "b" }], "|")],
                    getSystemMessageText: ai.getSystemMessageText({ role: "system", content: "base", sections: { a: "one", b: null, c: "" }, timestamp: 0 }),
                    renderSystemMessageUpdate: ai.renderSystemMessageUpdate({ role: "system", content: [{ type: "text", text: "x" }], sections: { a: "one", b: null }, timestamp: 0 }),
                },
            };
        })()"#,
    );
    assert_eq!(
        actual["transcript"]["createInitialSystemMessage"],
        expected["transcript"]["createInitialSystemMessage"]
    );
    assert_eq!(
        actual["transcript"]["normalizeContext"],
        expected["transcript"]["normalizeContext"]
    );
    assert_eq!(
        actual["transcript"]["getInitialSystemMessage"],
        expected["transcript"]["getInitialSystemMessage"]
    );
    assert_eq!(
        actual["transcript"]["withoutInitialSystemMessage"],
        expected["transcript"]["withoutInitialSystemMessage"]
    );
    assert_eq!(
        actual["transcript"]["getCurrentTools"],
        expected["transcript"]["getCurrentTools"]
    );
    assert_eq!(
        actual["transcript"]["getCurrentSystemMessage"],
        expected["transcript"]["getCurrentSystemMessage"]
    );
    assert_eq!(
        actual["transcript"]["getCurrentSystemPrompt"],
        expected["transcript"]["getCurrentSystemPrompt"]
    );
    assert_eq!(
        actual["transcript"]["collapseSystemMessages"],
        expected["transcript"]["collapseSystemMessages"]
    );
    assert_eq!(
        actual["transcript"]["resolveTranscriptTrue"],
        expected["transcript"]["resolveTranscriptTrue"]
    );
    assert_eq!(
        actual["transcript"]["resolveTranscriptFalse"],
        expected["transcript"]["resolveTranscriptFalse"]
    );
    assert_eq!(
        actual["transcript"]["toToolDeclaration"],
        expected["transcript"]["toToolDeclaration"]
    );
    assert_eq!(
        actual["transcript"]["declarationsEqual"],
        expected["transcript"]["declarationsEqual"]
    );
    assert_eq!(
        actual["transcript"]["getToolStateChanges"],
        expected["transcript"]["getToolStateChanges"]
    );
    assert_eq!(
        actual["transcript"]["getDeclaredTools"],
        expected["transcript"]["getDeclaredTools"]
    );
    assert_eq!(
        actual["transcript"]["hasToolRedefinitions"],
        expected["transcript"]["hasToolRedefinitions"]
    );
    assert_eq!(
        actual["transcript"]["hasNonAdditiveToolChanges"],
        expected["transcript"]["hasNonAdditiveToolChanges"]
    );
    assert_eq!(
        actual["transcript"]["resolveTranscriptTools"],
        expected["transcript"]["resolveTranscriptTools"]
    );
    assert_eq!(actual["text"], expected["text"]);
}

#[test]
fn js_overflow_and_diagnostic_helpers_match_the_typescript_reference() {
    let harness = VirtualHarness::new();
    let expected = native_fixture();

    let patterns: Vec<String> = harness.eval(
        r#"globalThis.__kiana_virtual["@earendil-works/kiana-ai"].getOverflowPatterns().map((pattern) => pattern.source)"#,
    );
    assert_eq!(json!(patterns), expected["overflow"]["patterns"]);

    let cases = harness.eval_json(
        r#"(() => {
            const ai = globalThis.__kiana_virtual["@earendil-works/kiana-ai"];
            const messages = [
                { stopReason: "error", errorMessage: "prompt is too long: 213462 tokens > 200000 maximum", provider: "anthropic", usage: { input: 0, cacheRead: 0, output: 0 } },
                { stopReason: "error", errorMessage: "Throttling error: Too many tokens", provider: "amazon-bedrock", usage: { input: 0, cacheRead: 0, output: 0 } },
                { stopReason: "stop", provider: "zai", usage: { input: 150, cacheRead: 100, output: 5 } },
                { stopReason: "length", provider: "xiaomi", usage: { input: 199000, cacheRead: 1000, output: 0 } },
            ];
            return messages.map((message) => [
                ai.isContextOverflow(message),
                ai.isContextOverflow(message, 200000),
                ai.isRecoverableLength(message, 1000),
            ]);
        })()"#,
    );
    assert_eq!(cases, expected["overflow"]["cases"]);

    let diagnostics = harness.eval_json(
        r#"(() => {
            const ai = globalThis.__kiana_virtual["@earendil-works/kiana-ai"];
            const error = new Error("boom");
            const plain = ai.extractDiagnosticError("plain");
            return {
                formatThrownValue: [ai.formatThrownValue(error), ai.formatThrownValue("s"), ai.formatThrownValue(5), ai.formatThrownValue(null)],
                plain,
                merged: ai.mergeClientIdentity({ userAgent: "short" }, undefined, { attribution: { name: "a", url: "u" }, version: "1.2.3" }),
                versions: [ai.getClientVersion(), ai.getClientVersion({ version: "9" })],
                attribution: [
                    ai.getAttributionHeaders({ provider: "openrouter", baseUrl: "x" }, { attribution: { name: "kiana", url: "https://kiana.dev", categories: ["cli-agent"] } }),
                    ai.getAttributionHeaders({ provider: "nvidia", baseUrl: "x" }, { attribution: { name: "kiana", url: "u" } }),
                    ai.getAttributionHeaders({ provider: "opencode", baseUrl: "x" }, { attribution: { name: "kiana", url: "u" } }),
                    ai.getAttributionHeaders({ provider: "other", baseUrl: "https://opencode.ai/v1" }, { attribution: { name: "kiana", url: "u" } }),
                ],
                opencode: [ai.getOpenCodeSessionHeaders({ provider: "opencode", baseUrl: "x" }, "sid") ?? null, ai.getOpenCodeSessionHeaders({ provider: "openai", baseUrl: "x" }, "sid") ?? null],
            };
        })()"#,
    );
    let expected_identity = &expected["identity"];
    assert_eq!(
        diagnostics["formatThrownValue"],
        expected_identity["formatThrownValue"]
    );
    assert_eq!(
        diagnostics["plain"]["name"],
        expected_identity["extractDiagnosticError"][1]["name"]
    );
    assert_eq!(
        diagnostics["plain"]["message"],
        expected_identity["extractDiagnosticError"][1]["message"]
    );
    assert_eq!(
        diagnostics["merged"],
        expected_identity["mergeClientIdentity"]
    );
    assert_eq!(
        diagnostics["versions"],
        expected_identity["getClientVersion"]
    );
    assert_eq!(
        diagnostics["attribution"],
        expected_identity["getAttributionHeaders"]
    );
    assert_eq!(
        diagnostics["opencode"],
        expected_identity["getOpenCodeSessionHeaders"]
    );
}

#[test]
fn compat_aliases_keep_the_typescript_object_identity() {
    // TS：`export const getModel = getBuiltinModel`（compat.ts）与 providers/all.ts 的导出是
    // 同一个函数对象（review fix）。
    let harness = VirtualHarness::new();
    let same: Vec<bool> = harness.eval(
        r#"(() => {
            const ai = globalThis.__kiana_virtual["@earendil-works/kiana-ai"];
            const all = globalThis.__kiana_virtual["@earendil-works/kiana-ai/providers/all"];
            return [
                ai.getModel === all.getBuiltinModel,
                ai.getModels === all.getBuiltinModels,
                ai.getProviders === all.getBuiltinProviders,
                ai.calculateCost === globalThis.__kiana_virtual["@earendil-works/kiana-ai/models"].calculateCost,
            ];
        })()"#,
    );
    assert!(same.iter().all(|equal| *equal), "{same:?}");
    let same_import: bool = harness.eval_promise(
        r#"Promise.all([
            import("@earendil-works/kiana-ai"),
            import("@earendil-works/kiana-ai/providers/all"),
        ]).then(([ai, all]) =>
            ai.getModel === all.getBuiltinModel &&
            ai.getModels === all.getBuiltinModels &&
            ai.getProviders === all.getBuiltinProviders
        )"#,
    );
    assert!(same_import);
}

#[test]
fn canonical_module_names_are_hidden_aliases_for_the_cjs_loader() {
    // 回归（review finding）：CJS 的 `require` 与 ESM 共用解析器，拿到的规范名必须命中
    // `__kiana_virtual`，同时 22 个公开键仍是**唯一**的可枚举键。
    let harness = VirtualHarness::new();
    let checks: Vec<bool> = harness.eval(
        r#"(() => {
            const v = globalThis.__kiana_virtual;
            return [
                v["kiana-virtual/kiana-ai.js"] === v["@earendil-works/kiana-ai"],
                v["kiana-virtual/kiana-ai/models.js"] === v["@earendil-works/kiana-ai/models"],
                v["kiana-virtual/kiana-coding-agent.js"] === v["@earendil-works/kiana-coding-agent"],
                v["typebox/build/index.mjs"] === v["typebox"],
                v["typebox/build/compile/index.mjs"] === v["typebox/compile"],
                Object.prototype.hasOwnProperty.call(v, "kiana-virtual/kiana-ai.js"),
                !Object.keys(v).includes("kiana-virtual/kiana-ai.js"),
                Object.getOwnPropertyDescriptor(v, "kiana-virtual/kiana-ai.js").enumerable === false,
                Object.keys(v).length === 22,
            ];
        })()"#,
    );
    assert!(checks.iter().all(|ok| *ok), "{checks:?}");
}

#[test]
fn tool_factories_return_the_typescript_agent_tool_shape() {
    let harness = VirtualHarness::new();
    let expected = tool_fixture();
    let actual = harness.eval_json(
        r#"(() => {
            const ca = globalThis.__kiana_virtual["@earendil-works/kiana-coding-agent"];
            const strip = (tool) => ({
                name: tool.name,
                label: tool.label,
                description: tool.description,
                parameters: tool.parameters,
                outputSchema: tool.outputSchema === undefined ? "<undefined>" : tool.outputSchema,
                constrainedSampling: tool.constrainedSampling === undefined ? "<undefined>" : tool.constrainedSampling,
                prepareArguments: tool.prepareArguments === undefined ? "<undefined>" : typeof tool.prepareArguments,
                executionMode: tool.executionMode === undefined ? "<undefined>" : tool.executionMode,
                promptSnippet: tool.promptSnippet,
                promptGuidelines: tool.promptGuidelines,
            });
            return {
                read: strip(ca.createReadTool("/tmp/kiana-tools-probe")),
                bash: strip(ca.createBashTool("/tmp/kiana-tools-probe")),
                edit: strip(ca.createEditTool("/tmp/kiana-tools-probe")),
                write: strip(ca.createWriteTool("/tmp/kiana-tools-probe")),
                find: strip(ca.createFindTool("/tmp/kiana-tools-probe")),
                grep: strip(ca.createGrepTool("/tmp/kiana-tools-probe")),
                ls: strip(ca.createLsTool("/tmp/kiana-tools-probe")),
            };
        })()"#,
    );
    for family in ["read", "bash", "edit", "write", "find", "grep", "ls"] {
        let expected_tool = &expected[family];
        let actual_tool = &actual[family];
        for field in [
            "name",
            "label",
            "description",
            "parameters",
            "outputSchema",
            "constrainedSampling",
            "prepareArguments",
            "executionMode",
        ] {
            assert_eq!(actual_tool[field], expected_tool[field], "{family}.{field}");
        }
    }
    // TS `createBashTool` 在 wrapToolDefinition 之后 Object.assign 了 prompt 元数据。
    assert_eq!(
        actual["bash"]["promptSnippet"],
        expected["bash"]["promptSnippet"]
    );
    assert_eq!(
        actual["bash"]["promptGuidelines"],
        expected["bash"]["promptGuidelines"]
    );

    let definition = harness.eval_json(
        r#"(() => {
            const ca = globalThis.__kiana_virtual["@earendil-works/kiana-coding-agent"];
            const tool = ca.createReadToolDefinition("/tmp/kiana-tools-probe");
            return {
                name: tool.name,
                promptSnippet: tool.promptSnippet,
                promptGuidelines: tool.promptGuidelines,
                parameters: tool.parameters,
                outputSchema: tool.outputSchema,
                constrainedSampling: tool.constrainedSampling,
                hasExecute: typeof tool.execute === "function",
                renderCall: tool.renderCall === undefined ? "<undefined>" : typeof tool.renderCall,
            };
        })()"#,
    );
    let expected_definition = &expected["readDefinition"];
    assert_eq!(definition["name"], expected_definition["name"]);
    assert_eq!(
        definition["promptSnippet"],
        expected_definition["promptSnippet"]
    );
    assert_eq!(
        definition["promptGuidelines"],
        expected_definition["promptGuidelines"]
    );
    assert_eq!(definition["parameters"], expected_definition["parameters"]);
    assert_eq!(
        definition["outputSchema"],
        expected_definition["outputSchema"]
    );
    assert_eq!(
        definition["constrainedSampling"],
        expected_definition["constrainedSampling"]
    );
    assert_eq!(definition["hasExecute"], json!(true));
    assert_eq!(definition["renderCall"], json!("<undefined>"));
}

#[tokio::test(flavor = "current_thread")]
async fn tool_execute_runs_the_rust_tool_through_the_host_bridge() {
    let harness = VirtualHarness::new();
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(dir.path().join("a.txt"), "line1\nline2\n").expect("write file");
    let cwd = dir.path().to_string_lossy().into_owned();

    let result = tokio::task::LocalSet::new()
        .run_until(async {
            harness.eval::<()>(&format!(
                r#"
                globalThis.__tool_result = undefined;
                const tool = globalThis.__kiana_virtual["@earendil-works/kiana-coding-agent"].createReadTool({cwd:?});
                globalThis.__tool = tool;
                tool.execute("call-1", {{ path: "a.txt" }}).then(
                    (value) => {{ globalThis.__tool_result = {{ ok: value }}; }},
                    (error) => {{ globalThis.__tool_result = {{ error: String((error && error.message) || error) }}; }},
                );
                "#
            ));
            let mut iterations = 0;
            while harness.eval::<bool>("globalThis.__tool_result === undefined") {
                harness.engine.try_drain_completions();
                harness.engine.pump_jobs();
                tokio::task::yield_now().await;
                iterations += 1;
                assert!(iterations < 20_000, "tool execute never settled");
            }
            harness.eval_json("globalThis.__tool_result")
        })
        .await;

    let content = &result["ok"]["content"];
    assert!(
        content
            .as_array()
            .is_some_and(|blocks| blocks.iter().any(|block| block["text"]
                .as_str()
                .is_some_and(|text| text.contains("line1")))),
        "unexpected read result: {result}"
    );
}

#[test]
fn rust_backed_tool_validation_matches_the_typescript_reference() {
    let harness = VirtualHarness::new();
    let expected = native_fixture();

    let actual = harness.eval_json(
        r#"(() => {
            const ai = globalThis.__kiana_virtual["@earendil-works/kiana-ai"];
            const readTool = {
                name: "read",
                description: "read a file",
                parameters: {
                    type: "object",
                    properties: {
                        count: { type: "integer" },
                        flag: { type: "boolean" },
                        text: { type: "string" },
                        opt: { type: "string" },
                    },
                    required: ["count"],
                },
            };
            const typeboxTool = {
                name: "boxed",
                description: "typebox schema",
                parameters: {
                    type: "object",
                    properties: { n: { type: "number" }, label: { type: "string" } },
                    required: ["n"],
                },
            };
            const calls = [
                { tool: readTool, call: { type: "toolCall", id: "1", name: "read", arguments: { count: "42", flag: "true", text: 5 } } },
                { tool: readTool, call: { type: "toolCall", id: "2", name: "read", arguments: { count: 1.5, opt: null } } },
                { tool: readTool, call: { type: "toolCall", id: "3", name: "read", arguments: { flag: "truthy" } } },
                { tool: readTool, call: { type: "toolCall", id: "4", name: "read", arguments: {}, argumentsError: "bad json" } },
                { tool: typeboxTool, call: { type: "toolCall", id: "5", name: "boxed", arguments: { n: "7", label: 3 } } },
                { tool: typeboxTool, call: { type: "toolCall", id: "6", name: "boxed", arguments: { label: "x" } } },
            ];
            const capture = (run) => {
                try {
                    return { ok: run() };
                } catch (error) {
                    return { error: String(error.message) };
                }
            };
            return {
                validateArguments: calls.map(({ tool, call }) => capture(() => ai.validateToolArguments(tool, call))),
                validateCalls: [
                    capture(() => ai.validateToolCall([readTool, typeboxTool], calls[0].call)),
                    capture(() => ai.validateToolCall([readTool], calls[4].call)),
                    capture(() => ai.validateToolCall([readTool], { type: "toolCall", id: "7", name: "missing", arguments: {} })),
                ],
            };
        })()"#,
    );
    assert_eq!(actual["validateArguments"], expected["validateArguments"]);
    assert_eq!(actual["validateCalls"], expected["validateCalls"]);
}

#[test]
fn js_retry_and_identity_wrappers_match_the_typescript_reference() {
    let harness = VirtualHarness::new();
    let expected = native_fixture();

    let retry = harness.eval_json(
        r#"(() => {
            const ai = globalThis.__kiana_virtual["@earendil-works/kiana-ai"];
            const delays = [
                { policy: { baseDelayMs: 100 }, attempt: 0 },
                { policy: { baseDelayMs: 100 }, attempt: 1 },
                { policy: { baseDelayMs: 100 }, attempt: 3 },
                { policy: { baseDelayMs: 100, maxAgentDelayMs: 250 }, attempt: 4 },
                { policy: { baseDelayMs: 1000 }, attempt: 200 },
                { policy: { baseDelayMs: 60000 }, attempt: 2 },
            ].map(({ policy, attempt }) => ai.retryDelayMs(policy, attempt));
            const messages = [
                "rate limit exceeded",
                "HTTP 503 Service Unavailable",
                "Request id abc12345 failed",
                "insufficient_quota",
                "billing hard limit reached",
                "Monthly usage limit reached",
                "overloaded_error",
                "fetch failed",
                "stream ended without a terminal response",
                "totally fine",
                "",
            ].map((errorMessage) => ai.isRetryableAssistantError({ stopReason: "error", errorMessage }));
            messages.push(
                ai.isRetryableAssistantError({ stopReason: "stop", errorMessage: "rate limit" }),
                ai.isRetryableAssistantError({ stopReason: "error" }),
            );
            return { delays, messages };
        })()"#,
    );
    assert_eq!(retry["delays"], expected["retryDelays"]);
    assert_eq!(retry["messages"], expected["retryMessages"]);

    let calls_text: String = harness.eval_promise(
        r#"(async () => {
            const ai = globalThis.__kiana_virtual["@earendil-works/kiana-ai"];
            const policy = { enabled: true, maxRetries: 2, baseDelayMs: 100 };

            const abortEvents = [];
            let abortAttempts = 0;
            const abortResult = await ai.retryAssistantCall(
                async () => {
                    abortAttempts++;
                    return { role: "assistant", stopReason: "error", errorMessage: "rate limit exceeded", provider: "p", model: "m" };
                },
                policy,
                AbortSignal.abort(),
                {
                    onRetryScheduled: (...args) => { abortEvents.push(["scheduled", ...args]); },
                    onRetryAttemptStart: () => { abortEvents.push(["start"]); },
                    onRetryFinished: (...args) => { abortEvents.push(["finished", ...args]); },
                },
            );

            const exhaustedEvents = [];
            let exhaustedAttempts = 0;
            const exhaustedResult = await ai.retryAssistantCall(
                async () => {
                    exhaustedAttempts++;
                    return { role: "assistant", stopReason: "error", errorMessage: "insufficient_quota", provider: "p", model: "m" };
                },
                policy,
                AbortSignal.abort(),
                {
                    onRetryScheduled: (...args) => { exhaustedEvents.push(["scheduled", ...args]); },
                    onRetryFinished: (...args) => { exhaustedEvents.push(["finished", ...args]); },
                },
            );

            const successEvents = [];
            const successResult = await ai.retryAssistantCall(
                async () => ({ role: "assistant", stopReason: "stop", provider: "p", model: "m" }),
                policy,
                undefined,
                {
                    onRetryFinished: (...args) => { successEvents.push(["finished", ...args]); },
                },
            );

            const disabledEvents = [];
            const disabledResult = await ai.retryAssistantCall(
                async () => ({ role: "assistant", stopReason: "error", errorMessage: "rate limit exceeded", provider: "p", model: "m" }),
                { enabled: false, maxRetries: 5, baseDelayMs: 1 },
                undefined,
                {
                    onRetryFinished: (...args) => { disabledEvents.push(["finished", ...args]); },
                },
            );

            return {
                abort: { attempts: abortAttempts, result: abortResult, events: abortEvents },
                exhausted: { attempts: exhaustedAttempts, result: exhaustedResult, events: exhaustedEvents },
                success: { result: successResult, events: successEvents },
                disabled: { result: disabledResult, events: disabledEvents },
            };
        })().then((value) => JSON.stringify(value))"#,
    );
    let calls: Value = serde_json::from_str(&calls_text).expect("retry calls JSON");
    assert_eq!(calls, expected["retryCalls"]);

    let identity = harness.eval_json(
        r#"(() => {
            const ai = globalThis.__kiana_virtual["@earendil-works/kiana-ai"];
            const baseModels = {
                getProviders: () => ["p"],
                getProvider: (id) => ({ id }),
                getModels: (provider) => [provider + "-m"],
                getModel: (provider, id) => ({ provider, id }),
                getModelsOfType: (type, provider) => [type + ":" + provider],
                getModelOfType: (type, provider, id) => type + ":" + provider + ":" + id,
                getAllModels: (provider) => ["all:" + provider],
                refresh: (options) => ({ refresh: options }),
                checkAuth: (providerId, options) => ({ providerId, options }),
                getAvailable: (providerId, options) => ({ providerId, options }),
                getAvailableOfType: (type, providerId, options) => ({ type, providerId, options }),
                getAllAvailable: (providerId, options) => ({ providerId, options }),
                getAuth: (providerId) => ({ providerId }),
                login: (providerId, type) => ({ providerId, type }),
                logout: (providerId, options) => ({ providerId, options }),
                stream: (model, context, options) => ({ model: model.id, identity: options?.identity, extra: options?.sessionId }),
                complete: (model, context, options) => ({ model: model.id, identity: options?.identity }),
                streamSimple: (model) => ({ model: model.id }),
                completeSimple: (model) => ({ model: model.id }),
                streamDeferred: (model) => ({ model: model.id }),
                fetchDeferred: (model) => ({ model: model.id }),
                cancelDeferred: (model) => ({ model: model.id }),
                generateImages: (model) => ({ model: model.id }),
                classify: (model) => ({ model: model.id }),
            };
            const mutableModels = {
                ...baseModels,
                setProvider: (provider) => ({ set: provider.id }),
                deleteProvider: (id) => ({ deleted: id }),
                clearProviders: () => ({ cleared: true }),
            };
            const identity = { version: "9", userAgent: "short", attribution: { name: "kiana", url: "https://kiana.dev" } };
            const readOnly = ai.withClientIdentity(baseModels, identity);
            const mutable = ai.withClientIdentity(mutableModels, identity);
            return {
                providers: readOnly.getProviders(),
                model: readOnly.getModel("openai", "gpt"),
                stream: readOnly.stream({ id: "m" }, {}, { sessionId: "s" }),
                streamOverride: readOnly.stream({ id: "m" }, {}, { identity: { version: "1", userAgent: "x" }, sessionId: "s" }),
                refresh: readOnly.refresh({ identity: { version: "2" } }),
                readOnlyKeys: ["setProvider", "deleteProvider", "clearProviders"].map((key) => key in readOnly),
                mutableKeys: ["setProvider", "deleteProvider", "clearProviders"].map((key) => key in mutable),
                setProvider: mutable.setProvider({ id: "p" }),
                deleteProvider: mutable.deleteProvider("x"),
                clearProviders: mutable.clearProviders(),
                streamMutable: mutable.streamSimple({ id: "sm" }, {}, undefined),
            };
        })()"#,
    );
    assert_eq!(identity, expected["identityCases"]);
}

#[test]
fn js_lazy_and_auth_helpers_match_the_typescript_reference() {
    let harness = VirtualHarness::new();
    harness.install_test_globals();
    let expected = native_fixture();

    let auth_text: String = harness.eval_promise(
        r#"(async () => {
            const ai = globalThis.__kiana_virtual["@earendil-works/kiana-ai"];
            const signal = new AbortController().signal;
            const auth = ai.envApiKeyAuth("Test Provider", ["A_KEY", "B_KEY"]);
            const withValues = { env: async (name) => ({ A_KEY: "", B_KEY: "sk-b" })[name] };
            const withoutValues = { env: async () => undefined };
            const authCases = {
                name: auth.name,
                stored: (await auth.resolve({ ctx: withValues, credential: { type: "api_key", key: "sk-stored", env: { REGION: "eu" } }, signal })) ?? null,
                envVar: (await auth.resolve({ ctx: withValues, credential: undefined, signal })) ?? null,
                none: (await auth.resolve({ ctx: withoutValues, credential: undefined, signal })) ?? null,
                login: await auth.login({ signal, prompt: async (question) => ({ ...question, answer: "sk-login" }) }, undefined),
            };

            let oauthLoads = 0;
            const oauth = ai.lazyOAuth({
                name: "Test OAuth",
                isSubscription: true,
                loginLabel: "Sign in",
                load: async () => {
                    oauthLoads++;
                    return {
                        name: "Test OAuth",
                        login: async () => ({ type: "oauth", value: "login" }),
                        refresh: async () => ({ type: "oauth", value: "refresh" }),
                        toAuth: async () => ({ apiKey: "oauth-key" }),
                    };
                },
            });
            const oauthCases = {
                loadsBefore: oauthLoads,
                name: oauth.name,
                isSubscription: oauth.isSubscription,
                loginLabel: oauth.loginLabel,
                login: await oauth.login({ signal, prompt: async () => "" }, undefined),
                refresh: await oauth.refresh({ type: "oauth", access: "x", refresh: "y", expires: 0 }, signal),
                toAuth: await oauth.toAuth({ type: "oauth", access: "x", refresh: "y", expires: 0 }),
                loadsAfter: oauthLoads,
            };

            return { authCases, oauthCases };
        })().then((value) => JSON.stringify(value))"#,
    );
    let auth: Value = serde_json::from_str(&auth_text).expect("auth JSON");
    assert_eq!(auth["authCases"], expected["authCases"]);
    assert_eq!(auth["oauthCases"], expected["oauthCases"]);

    let lazy_text: String = harness.eval_promise(
        r#"(async () => {
            const ai = globalThis.__kiana_virtual["@earendil-works/kiana-ai"];
            const model = { id: "m", api: "test-api", provider: "p" };
            const message = {
                role: "assistant",
                content: [{ type: "text", text: "hi" }],
                api: "test-api",
                provider: "p",
                model: "m",
                usage: { input: 1, output: 2, cacheRead: 0, cacheWrite: 0, totalTokens: 3, cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, total: 0 } },
                stopReason: "stop",
                timestamp: 1,
            };
            let apiLoads = 0;
            const api = ai.lazyApi(async () => {
                apiLoads++;
                return {
                    stream: () => (async function* () {
                        yield { type: "start", partial: message };
                        yield { type: "done", reason: "stop", message };
                    })(),
                    streamSimple: () => (async function* () {
                        yield { type: "text_delta", contentIndex: 0, delta: "a", partial: message };
                        yield { type: "done", reason: "stop", message };
                    })(),
                    fetchDeferred: async () => (async function* () {
                        yield { type: "done", reason: "stop", message };
                    })(),
                    cancelDeferred: async () => "cancelled",
                };
            }, { fetchDeferred: true, cancelDeferred: true });
            const plain = { stream: () => (async function* () {})(), streamSimple: () => (async function* () {}) };
            const withDeferred = ai.lazyApi(async () => plain, { fetchDeferred: true, cancelDeferred: true });
            const withoutDeferred = ai.lazyApi(async () => plain);

            const collect = async (stream) => {
                const events = [];
                for await (const event of stream) events.push(event.type);
                return events;
            };
            const observerEvents = [];
            const observedResults = [];
            const events = await collect(api.stream(model, {}, {}));
            const simpleEvents = await collect(api.streamSimple(model, {}, {}));

            const failing = ai.lazyApi(async () => {
                throw new Error("setup boom");
            });
            const failingEvents = [];
            for await (const event of failing.stream(model, {}, {})) {
                failingEvents.push(
                    event.type === "error"
                        ? { type: event.type, message: event.error.errorMessage, stopReason: event.error.stopReason }
                        : { type: event.type },
                );
            }

            const directObserverEvents = [];
            const directObservedResults = [];
            const directStream = ai.lazyStream(
                model,
                async () => (async function* () {
                    yield { type: "start", partial: message };
                    yield { type: "done", reason: "stop", message };
                })(),
                {
                    onEvent: (event) => directObserverEvents.push(event.type),
                    onResult: (result) => directObservedResults.push(result?.model ?? null),
                },
            );
            const directEvents = await collect(directStream);

            return {
                apiLoadsAfterFirstUse: apiLoads,
                directEvents,
                directObserverEvents,
                directObservedResults,
                hasDeferred: ["fetchDeferred", "cancelDeferred"].map((key) => typeof withDeferred[key]),
                hasDeferredWhenNotRequested: ["fetchDeferred", "cancelDeferred"].map((key) => typeof withoutDeferred[key]),
                events,
                simpleEvents,
                observerEvents,
                observedResults,
                failingEvents,
            };
        })().then((value) => JSON.stringify(value))"#,
    );
    let lazy: Value = serde_json::from_str(&lazy_text).expect("lazy JSON");
    assert_eq!(lazy, expected["lazyCases"]);
}

#[test]
fn js_images_registry_and_session_cleanup_match_the_typescript_reference() {
    let harness = VirtualHarness::new();
    let expected = native_fixture();

    let text: String = harness.eval_promise(
        r#"(async () => {
            const ai = globalThis.__kiana_virtual["@earendil-works/kiana-ai"];
            const captureAsync = async (run) => {
                try {
                    return { ok: await run() };
                } catch (error) {
                    return { error: String(error instanceof Error ? error.message : error) };
                }
            };

            ai.registerImagesApiProvider({
                api: "test-images",
                generateImages: async (model, context, options) => ({
                    api: model.api,
                    provider: model.provider,
                    model: model.id,
                    output: [],
                    stopReason: "stop",
                    timestamp: 1,
                    seenOptions: options,
                }),
            });

            const imagesCases = {
                providerApi: ai.getImagesApiProvider("test-images")?.api ?? null,
                unknown: ai.getImagesApiProvider("nope") ?? null,
                generated: await ai.generateImages({ api: "test-images", provider: "p", id: "m" }, {}, { n: 1 }),
                mismatch: await captureAsync(() => ai.generateImages({ api: "other", provider: "p", id: "m" }, {}, {})),
                wrappedMismatch: await captureAsync(async () =>
                    ai.getImagesApiProvider("test-images").generateImages({ api: "other", provider: "p", id: "m" }, {}, {}),
                ),
                missing: await captureAsync(() => ai.generateImages({ api: "nope", provider: "p", id: "m" }, {}, {})),
            };

            const cleanupSeen = [];
            const unregisterFirst = ai.registerSessionResourceCleanup((sessionId) => {
                cleanupSeen.push(["a", sessionId ?? null]);
            });
            ai.registerSessionResourceCleanup((sessionId) => {
                cleanupSeen.push(["b", sessionId ?? null]);
                throw new Error("boom");
            });
            const aggregateResult = await captureAsync(async () => {
                ai.cleanupSessionResources("s1");
            });
            unregisterFirst();
            const afterUnregister = await captureAsync(async () => {
                ai.cleanupSessionResources("s2");
            });

            return {
                imagesCases,
                cleanupCases: {
                    seen: cleanupSeen,
                    aggregateMessage: aggregateResult.error ?? null,
                    afterUnregister,
                },
            };
        })().then((value) => JSON.stringify(value))"#,
    );
    let actual: Value = serde_json::from_str(&text).expect("images JSON");
    assert_eq!(actual["imagesCases"], expected["imagesCases"]);
    assert_eq!(actual["cleanupCases"], expected["cleanupCases"]);
}
