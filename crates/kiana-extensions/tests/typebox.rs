//! 内嵌 typebox 1.3.27 的语义测试（设计 §3.6、§3.7、§8.5）。
//!
//! TS 参照没有 typebox 自身的测试：扩展只用 `Type.Object/Optional/Array/...` 与
//! `Compile(schema).Check/Errors`、`Value.Convert`，因此这里用
//! `packages/ai/src/utils/validation.ts` 同款调用生成黄金夹具（`fixtures/virtual/typebox_cases.json`，
//! 由 upstream-reference 里的真实包产出），逐字比较 JSON（对象键顺序也属于可观察行为）。

#[path = "fixtures/virtual/support.rs"]
mod virtual_support;

use serde_json::Value;
use virtual_support::VirtualHarness;

fn fixture() -> Value {
    let path = format!(
        "{}/tests/fixtures/virtual/typebox_cases.json",
        env!("CARGO_MANIFEST_DIR")
    );
    let text = std::fs::read_to_string(&path).expect("read typebox fixture");
    serde_json::from_str(&text).expect("valid typebox fixture")
}

/// 创建一个与夹具相同的 schema，并返回 `Compile` 结果在 `globalThis.__compiled`。
fn prepare(harness: &VirtualHarness) {
    harness.bind_module("typebox", "__tb");
    harness.bind_module("typebox/compile", "__tb_compile");
    harness.bind_module("typebox/value", "__tb_value");
    harness.eval::<()>(
        r#"
        globalThis.__schema = globalThis.__tb.Type.Object({
            name: globalThis.__tb.Type.String(),
            age: globalThis.__tb.Type.Optional(globalThis.__tb.Type.Number()),
            tags: globalThis.__tb.Type.Array(globalThis.__tb.Type.String()),
        });
        globalThis.__compiled = globalThis.__tb_compile.Compile(globalThis.__schema);
        "#,
    );
}

fn assert_json_text(actual: String, expected: &Value, label: &str) {
    let expected = serde_json::to_string(expected).expect("serialize expected");
    assert_eq!(actual, expected, "{label}");
}

#[test]
fn schema_json_and_own_symbols_match_the_typescript_reference() {
    let harness = VirtualHarness::new();
    let expected = fixture();
    prepare(&harness);
    assert_json_text(
        harness.eval("JSON.stringify(globalThis.__schema)"),
        &expected["schemaJson"],
        "Type.Object JSON",
    );
    assert_eq!(
        harness.eval::<u32>("Object.getOwnPropertySymbols(globalThis.__schema).length"),
        expected["ownSymbols"].as_u64().unwrap() as u32
    );
    // typebox 1.3.27 不再使用 Symbol.for("TypeBox.Kind")：与 TS 参照一致（0 个自有 symbol）。
    // 设计 §3.7 的符号判断表述针对旧版 typebox；真实 Node 运行时同样走 coerceWithJsonSchema 回退。
    let kind: String = harness.eval(r#"String(globalThis.__schema[Symbol.for("TypeBox.Kind")])"#);
    assert_eq!(kind, "undefined");
}

#[test]
fn compile_check_matches_the_typescript_reference() {
    let harness = VirtualHarness::new();
    let expected = fixture();
    prepare(&harness);
    let inputs = expected["checkInputs"].as_array().expect("inputs");
    let results = expected["checkResults"].as_array().expect("results");
    for (input, result) in inputs.iter().zip(results) {
        let source = format!(
            "JSON.stringify(globalThis.__compiled.Check({}))",
            serde_json::to_string(input).unwrap()
        );
        let actual: String = harness.eval(&source);
        assert_eq!(
            actual,
            serde_json::to_string(result).unwrap(),
            "Check({input})"
        );
    }
}

#[test]
fn compile_errors_match_the_typescript_reference_verbatim() {
    let harness = VirtualHarness::new();
    let expected = fixture();
    prepare(&harness);
    let inputs = expected["errorsFor"].as_array().expect("errorsFor");
    let errors = expected["errors"].as_array().expect("errors");
    for (input, expected_errors) in inputs.iter().zip(errors) {
        let source = format!(
            "JSON.stringify(globalThis.__compiled.Errors({}))",
            serde_json::to_string(input).unwrap()
        );
        assert_json_text(harness.eval(&source), expected_errors, "Errors()");
    }
}

#[test]
fn value_convert_and_check_match_the_typescript_reference() {
    let harness = VirtualHarness::new();
    let expected = fixture();
    prepare(&harness);
    let convert_inputs = expected["convertInputs"].as_array().expect("convertInputs");
    let convert = expected["convert"].as_array().expect("convert");
    for (input, expected_value) in convert_inputs.iter().zip(convert) {
        let literal = serde_json::to_string(&serde_json::to_string(input).unwrap()).unwrap();
        let source = format!(
            "JSON.stringify(globalThis.__tb_value.Value.Convert(globalThis.__schema, JSON.parse({literal})))"
        );
        assert_json_text(harness.eval(&source), expected_value, "Value.Convert");
    }
    let check_inputs = expected["valueCheckInputs"]
        .as_array()
        .expect("valueCheckInputs");
    let check_results = expected["valueCheckResults"]
        .as_array()
        .expect("valueCheckResults");
    for (input, expected_result) in check_inputs.iter().zip(check_results) {
        let source = format!(
            "JSON.stringify(globalThis.__tb_value.Value.Check(globalThis.__schema, {}))",
            serde_json::to_string(input).unwrap()
        );
        assert_eq!(
            harness.eval::<String>(&source),
            serde_json::to_string(expected_result).unwrap()
        );
    }
}

#[test]
fn string_enum_helper_matches_the_typescript_reference() {
    let harness = VirtualHarness::new();
    let path = format!(
        "{}/tests/fixtures/virtual/js_cases.json",
        env!("CARGO_MANIFEST_DIR")
    );
    let expected: Value =
        serde_json::from_str(&std::fs::read_to_string(&path).expect("read js_cases fixture"))
            .expect("valid js_cases fixture");
    harness.bind_module("@earendil-works/kiana-ai", "__kiana_ai");
    let actual: String = harness.eval(
        r#"JSON.stringify(globalThis.__kiana_ai.StringEnum(["add", "sub"], { description: "op", default: "add" }))"#,
    );
    assert_json_text(actual, &expected["stringEnum"], "StringEnum JSON");
}

#[test]
fn typebox_is_evaluated_once_shared_by_all_aliases() {
    let harness = VirtualHarness::new();
    harness.bind_module("typebox", "__tb");
    harness.bind_module("@sinclair/typebox", "__tb_legacy");
    harness.bind_module("typebox/compile", "__tb_compile");
    let same_object: bool =
        harness.eval("globalThis.__tb.Type.Object === globalThis.__tb_legacy.Type.Object");
    assert!(
        same_object,
        "same Type namespace object across package scopes"
    );
    // Compile 与 Type 来自同一依赖域：用 Type 构造的 schema 能被 Compile 直接识别。
    let works: bool = harness.eval(
        r#"(() => {
            const compiled = globalThis.__tb_compile.Compile(globalThis.__tb.Type.Object({ n: globalThis.__tb.Type.Number() }));
            return compiled.Check({ n: 1 });
        })()"#,
    );
    assert!(works);
}

#[test]
fn compile_is_cached_per_schema_object_by_callers() {
    // `validation.ts` 用 WeakMap<schema, Compile> 缓存；这里验证同一个 schema 的 Compile 结果稳定
    // （typebox 自己不做缓存，但重复 Compile 必须给出相同判定）。
    let harness = VirtualHarness::new();
    prepare(&harness);
    let stable: bool = harness.eval(
        r#"(() => {
            const a = globalThis.__tb_compile.Compile(globalThis.__schema);
            const b = globalThis.__tb_compile.Compile(globalThis.__schema);
            return a.Check({ name: "x", tags: [] }) === b.Check({ name: "x", tags: [] })
                && JSON.stringify(a.Errors({})) === JSON.stringify(b.Errors({}));
        })()"#,
    );
    assert!(stable);
}
