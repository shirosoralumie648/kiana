//! Ported from the TS side's jiti/Babel TS-strip behaviour (`spec/design-extension-runtime.md`
//! §3.2). There is no standalone TS test for the transpiler: jiti covers it inside
//! `packages/coding-agent/test/extensions-discovery.test.ts`, so the fixtures here pin the oxc
//! pipeline instead (inputs under `tests/fixtures/transpile/<case>/`).
#![cfg(feature = "runtime")]

use std::fs;
use std::path::{Path, PathBuf};

use kiana_extensions::loader::transpile::{TranspileError, is_typescript, strip_bom, strip_types};

fn fixture_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/transpile")
}

fn fixture_cases() -> Vec<PathBuf> {
    let mut cases: Vec<PathBuf> = fs::read_dir(fixture_root())
        .expect("transpile fixtures exist")
        .map(|entry| entry.expect("readable dir entry").path())
        .filter(|path| path.is_dir())
        .collect();
    cases.sort();
    cases
}

struct Case {
    name: String,
    input_name: String,
    source: String,
    expected: String,
}

fn load_case(dir: &Path) -> Case {
    let input = if dir.join("input.ts").exists() {
        dir.join("input.ts")
    } else {
        dir.join("input.tsx")
    };
    let expected = if input.extension().is_some_and(|ext| ext == "tsx") {
        dir.join("expected.jsx")
    } else {
        dir.join("expected.js")
    };
    Case {
        name: dir
            .file_name()
            .expect("case dir has a name")
            .to_string_lossy()
            .into_owned(),
        input_name: input
            .file_name()
            .expect("input has a name")
            .to_string_lossy()
            .into_owned(),
        source: fs::read_to_string(&input).expect("fixture input is readable"),
        expected: fs::read_to_string(&expected).expect("fixture expectation is readable"),
    }
}

#[test]
fn strips_each_syntax_case_exactly() {
    let mut seen = 0;
    for dir in fixture_cases() {
        let case = load_case(&dir);
        let actual = strip_types(&case.input_name, &case.source)
            .unwrap_or_else(|error| panic!("case {} failed: {error}", case.name));
        assert_eq!(actual, case.expected, "case {}", case.name);
        seen += 1;
    }
    assert_eq!(seen, 9, "expected the 9 documented syntax fixtures");
}

#[test]
fn strips_types_annotations_and_erasable_syntax() {
    let case = load_case(&fixture_root().join("type_annotations"));
    let output = strip_types(&case.input_name, &case.source).expect("strips");
    for token in [
        "interface",
        "satisfies",
        "as const",
        "type Pair",
        ": number",
    ] {
        assert!(
            !output.contains(token),
            "`{token}` must be erased:\n{output}"
        );
    }
    assert!(output.contains("const pair = [point, point];"));
}

#[test]
fn emits_enums_and_namespaces_as_iife() {
    let case = load_case(&fixture_root().join("enum_namespace"));
    let output = strip_types(&case.input_name, &case.source).expect("strips");
    assert!(output.contains("var Color = /* @__PURE__ */ function(Color) {"));
    assert!(output.contains("Color[Color[\"Green\"] = 3] = \"Green\";"));
    assert!(output.contains("var Direction = /* @__PURE__ */ function(Direction) {"));
    assert!(output.contains("})(Geometry || (Geometry = {}));"));
}

#[test]
fn lowers_constructor_parameter_properties() {
    let case = load_case(&fixture_root().join("parameter_properties"));
    let output = strip_types(&case.input_name, &case.source).expect("strips");
    assert!(output.contains("this.name = name;"));
    assert!(output.contains("this.port = port;"));
    assert!(output.contains("this.enabled = enabled;"));
    assert!(!output.contains("public"));
    assert!(!output.contains("private"));
}

#[test]
fn drops_type_only_imports_and_exports() {
    let case = load_case(&fixture_root().join("type_only_imports"));
    let output = strip_types(&case.input_name, &case.source).expect("strips");
    assert!(output.starts_with("import { baz } from \"./other.ts\";"));
    assert!(!output.contains("Foo"));
    assert!(!output.contains("type Bar"));
    assert!(!output.contains("./types.ts"));
}

#[test]
fn lowers_legacy_decorators_to_runtime_helpers() {
    let case = load_case(&fixture_root().join("decorators"));
    let output = strip_types(&case.input_name, &case.source).expect("strips");
    // Legacy decorators emit oxc runtime helpers. They are inlined (like jiti/Babel) so the module
    // has no `@oxc-project/runtime` dependency the loader would have to register.
    assert!(!output.contains("@oxc-project/runtime"));
    assert!(output.starts_with("const _decorate = function __decorate("));
    assert!(output.contains("_decorate([sealed], Widget)"));
    // The helper body is the upstream @oxc-project/runtime implementation.
    assert!(output.contains("typeof Reflect.decorate === \"function\""));
}

#[test]
fn keeps_import_meta_dynamic_import_and_top_level_await() {
    let case = load_case(&fixture_root().join("module_syntax"));
    let output = strip_types(&case.input_name, &case.source).expect("strips");
    assert!(output.contains("const meta = import.meta;"));
    assert!(output.contains("import(\"./lazy.ts\")"));
    assert!(output.contains("await Promise.resolve(1)"));
}

#[test]
fn parses_tsx_without_transforming_jsx() {
    let case = load_case(&fixture_root().join("tsx_parses"));
    assert_eq!(case.input_name, "input.tsx");
    let output = strip_types(&case.input_name, &case.source).expect("parses TSX");
    assert!(output.contains("<div className=\"x\">hello</div>"));
    assert!(!output.contains("jsx-runtime"));
}

#[test]
fn keeps_explicit_resource_management_statements() {
    // `using` / `await using` are plain JavaScript syntax (ES2026 proposal), not TypeScript syntax:
    // oxc keeps them, exactly like Babel without the explicit-resource-management plugin.
    let case = load_case(&fixture_root().join("explicit_resource_management"));
    let output = strip_types(&case.input_name, &case.source).expect("strips");
    assert!(output.contains("using shortLived = new Temporary();"));
    assert!(output.contains("await using awaited = new Temporary();"));
    assert!(!output.contains("dispose(): void"));
}

#[test]
fn stripped_output_evaluates_in_quickjs() {
    use rquickjs::{Context, Module, Runtime};

    for name in [
        "type_annotations",
        "enum_namespace",
        "parameter_properties",
        "rich_syntax",
    ] {
        let case = load_case(&fixture_root().join(name));
        let output = strip_types(&case.input_name, &case.source).expect("strips");
        let rt = Runtime::new().expect("runtime");
        let ctx = Context::full(&rt).expect("context");
        ctx.with(|ctx| {
            let module = Module::declare(ctx.clone(), case.name.clone(), output)
                .unwrap_or_else(|error| panic!("case {name} does not parse as JS: {error}"));
            let (_module, promise) = module.eval().expect("module evaluates");
            promise.finish::<()>().expect("module finishes");
        });
    }
}

#[test]
fn strips_utf8_bom_before_parsing() {
    let case = load_case(&fixture_root().join("type_annotations"));
    let with_bom = format!("\u{feff}{}", case.source);
    let plain = strip_types("input.ts", &case.source).expect("strips");
    let from_bom = strip_types("input.ts", &with_bom).expect("strips");
    assert_eq!(from_bom, plain);
    assert_eq!(strip_bom(&with_bom), case.source);
}

#[test]
fn reports_parse_diagnostics_with_the_module_name() {
    let error = strip_types("/tmp/broken.ts?v=1-1", "export default function( {").unwrap_err();
    assert_eq!(error.path(), "/tmp/broken.ts?v=1-1");
    assert!(!error.diagnostics().is_empty());
    assert!(matches!(error, TranspileError::Parse { .. }));
    let message = error.to_string();
    assert!(message.starts_with("Failed to parse /tmp/broken.ts?v=1-1: "));
}

#[test]
fn accepts_mts_and_cts_sources() {
    let mts = strip_types("a.mts", "export const x: number = 1;").expect("mts strips");
    assert_eq!(mts, "export const x = 1;\n");
    let cts = strip_types("a.cts", "const x: number = 1; module.exports = x;").expect("cts strips");
    assert_eq!(cts, "const x = 1;\nmodule.exports = x;\n");
    // TypeScript's CJS-only `export =` is lowered to `module.exports`; the loader classifies such
    // files as CJS from the raw source before transpiling (see loader::cjs::is_cjs_module).
    let export_equals = strip_types("a.ts", "const x = 1; export = x;").expect("export= strips");
    assert!(export_equals.contains("module.exports = x;"));
}

#[test]
fn injects_the_jiti_cjs_bindings_for_esm_modules() {
    // jiti compiles every TS module to CJS and evaluates it with `require`, `__filename` and
    // `__dirname` in scope (corpus: `examples/extensions/notify.ts` calls `require()` inside a
    // `"type": "module"` package).
    let output = strip_types(
        "/tmp/ext/notify.ts",
        "export default function notify(): string { return typeof require + __dirname + __filename; }",
    )
    .expect("strips");
    assert!(output.starts_with(
        "var require = globalThis.__kiana_make_require(\"/tmp/ext/notify.ts\", \"/tmp/ext\");\n"
    ));
    assert!(output.contains("var __filename = \"/tmp/ext/notify.ts\";\n"));
    assert!(output.contains("var __dirname = \"/tmp/ext\";\n"));
}

#[test]
fn does_not_shadow_a_locally_declared_require() {
    // Babel renames the user binding (`_require`); our injection must not create a duplicate
    // top-level `const require`.
    let output = strip_types(
        "/tmp/ext/create.ts",
        "import { createRequire } from \"node:module\";\n\
         const require = createRequire(import.meta.url);\n\
         export default typeof require;",
    )
    .expect("strips");
    assert!(!output.contains("globalThis.__kiana_make_require"));
    assert!(output.contains("const require = createRequire(import.meta.url);"));
}

#[test]
fn cjs_style_typescript_gets_no_esm_prelude() {
    // Files without ESM syntax are wrapped by loader::cjs, whose module scope already provides
    // `require`; injecting another binding would collide with the wrapper parameter.
    let output = strip_types(
        "/tmp/ext/legacy.ts",
        "const fs = require(\"node:fs\");\nexports.fs = fs;",
    )
    .expect("strips");
    assert!(!output.contains("globalThis.__kiana_make_require"));
    assert!(output.contains("require(\"node:fs\")"));
}

#[test]
fn recognises_typescript_extensions() {
    assert!(is_typescript("a.ts"));
    assert!(is_typescript("a.tsx"));
    assert!(is_typescript("a.mts"));
    assert!(is_typescript("a.cts"));
    assert!(!is_typescript("a.js"));
}

/// Locate the read-only TS reference checkout if it is next to this workspace (or via
/// `KIANA_REFERENCE_ROOT`). The corpus test is skipped in checkouts that do not vendor it.
fn corpus_root() -> Option<PathBuf> {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut candidates =
        vec![manifest.join("../../../kiana/packages/coding-agent/examples/extensions")];
    if let Some(root) = std::env::var_os("KIANA_REFERENCE_ROOT") {
        candidates.push(PathBuf::from(root).join("packages/coding-agent/examples/extensions"));
    }
    candidates.into_iter().find(|path| path.is_dir())
}

fn collect_ts_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_ts_files(&path, out);
        } else if path.extension().is_some_and(|ext| ext == "ts") {
            out.push(path);
        }
    }
}

#[test]
fn strips_the_example_corpus_without_diagnostics() {
    let Some(root) = corpus_root() else {
        eprintln!("skipping corpus strip: TS reference not found next to the workspace");
        return;
    };
    let mut files = Vec::new();
    collect_ts_files(&root, &mut files);
    files.sort();
    assert!(
        files.len() >= 86,
        "expected the 86-file example corpus, found {}",
        files.len()
    );
    let mut failures = Vec::new();
    for file in &files {
        let source = fs::read_to_string(file).expect("corpus file is readable");
        let name = file.to_string_lossy();
        if let Err(error) = strip_types(&name, &source) {
            failures.push(format!("{name}: {error}"));
        }
    }
    assert!(
        failures.is_empty(),
        "corpus strip failures:\n{}",
        failures.join("\n")
    );
}
