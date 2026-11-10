//! True-CJS loader tests (`spec/design-extension-runtime.md` §3.5).
//!
//! The TS reference delegates CJS handling to jiti, so there is no standalone TS test to port;
//! these tests execute the real `js/cjs.js` + `wrap_module` template against vendored npm
//! fixtures (`shell-quote@1.12.0`, `ms@2.1.3`) and synthetic shapes. The two host primitives
//! (`__kiana_host.resolve` / `__kiana_host.read`) are installed by the tests themselves, so this file
//! has no compile-time dependency on the loader-host unit.
#![cfg(feature = "runtime")]

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use kiana_extensions::loader::cjs::{
    CJS_JS_SOURCE, is_cjs_module, js_string_literal, package_type_for, wrap_module,
};
use kiana_extensions::loader::resolver::{
    ModuleResolver, ResolutionKind, ResolverOptions, VirtualFs,
};
use rquickjs::loader::{ImportAttributes, Loader, Resolver as RqResolver};
use rquickjs::{
    CatchResultExt, CaughtError, Context, Ctx, Function, Module, Object, Runtime, Value,
};

fn fixture(relative: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/cjs")
        .join(relative)
}

fn fixture_str(relative: &str) -> String {
    fixture(relative).to_string_lossy().into_owned()
}

/// Evaluate `js/cjs.js`, install the synchronous host primitives, and run `test`.
fn with_cjs_runtime(test: impl for<'js> FnOnce(&Ctx<'js>)) {
    let rt = Runtime::new().expect("create runtime");
    let ctx = Context::full(&rt).expect("create context");
    ctx.with(|ctx| {
        ctx.eval::<(), _>(CJS_JS_SOURCE)
            .expect("js/cjs.js evaluates");
        install_host(&ctx).expect("host primitives install");
        test(&ctx);
    });
}

fn install_host<'js>(ctx: &Ctx<'js>) -> rquickjs::Result<()> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/cjs");
    let resolver = Rc::new(RefCell::new(ModuleResolver::new(ResolverOptions {
        extension_dir: Some(root),
        virtual_fs: VirtualFs::empty(),
    })));

    let host = Object::new(ctx.clone())?;
    let resolve_resolver = Rc::clone(&resolver);
    host.set(
        "resolve",
        Function::new(
            ctx.clone(),
            move |from: String, spec: String| -> rquickjs::Result<String> {
                let resolver = resolve_resolver.borrow();
                resolver
                    .resolve_specifier(&from, &spec, ResolutionKind::Require)
                    .map_err(|error| error.to_rquickjs_error())
            },
        ),
    )?;
    host.set(
        "read",
        Function::new(
            ctx.clone(),
            move |path: String| -> rquickjs::Result<Option<String>> {
                Ok(read_cjs_source(Path::new(&path)))
            },
        ),
    )?;
    ctx.globals().set("__kiana_host", host)
}

/// Test host `read`: `None` (ESM) exactly when [`is_cjs_module`] would not wrap the file, i.e. the
/// same policy the loader-host unit applies when serving `__kiana_host.read`.
fn read_cjs_source(path: &Path) -> Option<String> {
    let source = std::fs::read_to_string(path).ok()?;
    if path.extension().and_then(|ext| ext.to_str()) == Some("json") {
        return Some(source);
    }
    let package_type = package_type_for(path);
    is_cjs_module(&path.to_string_lossy(), &source, package_type.as_deref()).then_some(source)
}

fn module_source(path: &Path) -> String {
    module_source_named(&path.to_string_lossy(), path)
}

fn module_source_named(name: &str, path: &Path) -> String {
    let raw = std::fs::read_to_string(path).expect("fixture is readable");
    wrap_module(name, &raw)
}

/// Evaluate a wrapped CJS module as ESM and return its `default` export.
fn load_default<'js>(ctx: &Ctx<'js>, path: &Path) -> rquickjs::Result<Value<'js>> {
    load_default_named(ctx, &path.to_string_lossy(), path)
}

fn load_default_named<'js>(
    ctx: &Ctx<'js>,
    name: &str,
    path: &Path,
) -> rquickjs::Result<Value<'js>> {
    let module = Module::declare(ctx.clone(), name, module_source_named(name, path))?;
    let (module, promise) = module.eval()?;
    promise.finish::<()>()?;
    module.namespace()?.get("default")
}

/// Evaluate a wrapped CJS module and discard the result.
fn eval_module<'js>(ctx: &Ctx<'js>, path: &Path) -> rquickjs::Result<Value<'js>> {
    load_default(ctx, path)
}

/// A resolver/loader pair serving a fixed map of module names to sources, so ESM import linking
/// (`import { quote } from "<cjs path>"`) can be exercised against the real wrapper.
struct MapResolver {
    names: Vec<String>,
}

impl RqResolver for MapResolver {
    fn resolve<'js>(
        &mut self,
        _ctx: &Ctx<'js>,
        base: &str,
        name: &str,
        _attributes: Option<ImportAttributes<'js>>,
    ) -> rquickjs::Result<String> {
        if self.names.iter().any(|known| known == name) {
            Ok(name.to_string())
        } else {
            Err(rquickjs::Error::new_resolving_message(
                base,
                name,
                "not found",
            ))
        }
    }
}

struct MapLoader {
    sources: HashMap<String, String>,
}

impl Loader for MapLoader {
    fn load<'js>(
        &mut self,
        ctx: &Ctx<'js>,
        name: &str,
        _attributes: Option<ImportAttributes<'js>>,
    ) -> rquickjs::Result<Module<'js>> {
        let source = self
            .sources
            .get(name)
            .unwrap_or_else(|| panic!("unexpected module load: {name}"))
            .clone();
        Module::declare(ctx.clone(), name, source)
    }
}

/// Run `test` in a runtime that can import the given module sources, then read `globalThis.__out`.
fn run_esm_with_sources(sources: Vec<(String, String)>, entry: &str) -> Result<String, String> {
    fn describe(_ctx: &Ctx<'_>, error: CaughtError) -> String {
        match error {
            CaughtError::Exception(exception) => exception
                .message()
                .unwrap_or_else(|| "<no message>".to_string()),
            other => format!("{other:?}"),
        }
    }
    let rt = Runtime::new().map_err(|error| error.to_string())?;
    let resolver = MapResolver {
        names: sources.iter().map(|(name, _)| name.clone()).collect(),
    };
    let loader = MapLoader {
        sources: sources.into_iter().collect(),
    };
    rt.set_loader(resolver, loader);
    let ctx = Context::full(&rt).map_err(|error| error.to_string())?;
    ctx.with(|ctx| {
        // The wrapper uses the same true-CJS loader as production (`__kiana_cjs_acquire`, nested
        // `require` through the host primitives).
        ctx.eval::<(), _>(CJS_JS_SOURCE)
            .map_err(|error| format!("cjs prelude: {error}"))?;
        install_host(&ctx).map_err(|error| format!("host: {error}"))?;
        let promise = Module::evaluate(ctx.clone(), "<test>", entry)
            .catch(&ctx)
            .map_err(|error| describe(&ctx, error))?;
        promise
            .finish::<()>()
            .catch(&ctx)
            .map_err(|error| describe(&ctx, error))?;
        ctx.eval("String(globalThis.__out)")
            .catch(&ctx)
            .map_err(|error| describe(&ctx, error))
    })
}

fn cache_object<'js>(ctx: &Ctx<'js>) -> Object<'js> {
    ctx.eval("globalThis.__kiana_cjs").expect("cache object")
}

// ---------------------------------------------------------------------------
// Source template (§3.5)
// ---------------------------------------------------------------------------

#[test]
fn template_preregisters_the_module_record_and_evaluates_with_new_function() {
    let source = module_source(&fixture("shapes/sdk_pattern.js"));
    assert!(source.starts_with("const __kiana_cjs = globalThis.__kiana_cjs_acquire("));
    assert!(source.contains(", false);\nconst module = __kiana_cjs[0];\n"));
    assert!(source.contains("const exports = module.exports;\n"));
    assert!(source.contains("const require = module.require;\n"));
    assert!(source.contains("if (__kiana_cjs[1]) {\n"));
    // The body is evaluated with `new Function`, so it stays sloppy like Node's CJS loader.
    assert!(source.contains(
        "new Function(\"module\", \"exports\", \"require\", \"__filename\", \"__dirname\", "
    ));
    assert!(source.contains(".call(module.exports, module, exports, require, "));
    assert!(source.contains("module.loaded = true;"));
    assert!(source.contains("globalThis.__kiana_cjs_delete("));
    // `.js` outside a type:module package is loaded natively by jiti (native interop rule).
    assert!(source.contains(
        "export default module.exports == null ? module.exports : \
         (module.exports.__esModule && typeof module.exports.default === \"function\" \
         ? module.exports.default : module.exports);"
    ));
    // Node-style named exports synthesized from `exports.version = ...`.
    assert!(source.contains(
        "const __kiana_export_0 = module.exports == null ? undefined : module.exports[\"version\"];\n"
    ));
    assert!(source.ends_with("export { __kiana_export_0 as version };\n"));
}

#[test]
fn template_embeds_raw_source_as_a_string_literal() {
    let raw = "exports.a = 1;\n// keep me\n";
    let source = wrap_module("/tmp/x/mod.js", raw);
    assert!(source.contains("\"exports.a = 1;\\n// keep me\\n\""));
    // The raw text only appears inside the `new Function` string literal.
    assert_eq!(
        source.matches("new Function(\"module\"").count(),
        1,
        "the body is evaluated once"
    );
}

#[test]
fn template_strips_bom_and_shebang_like_node() {
    let source = wrap_module(
        "/tmp/x/mod.js",
        "\u{feff}#!/usr/bin/env node\nexports.a = 1;\n",
    );
    assert!(source.contains("\"\\nexports.a = 1;\\n\""));
    assert!(!source.contains("#!/usr/bin/env node"));
    assert!(!source.contains('\u{feff}'));
}

#[test]
fn js_string_literal_matches_json_stringify() {
    assert_eq!(js_string_literal("a/b"), "\"a/b\"");
    assert_eq!(js_string_literal("a\"b"), "\"a\\\"b\"");
    assert_eq!(js_string_literal("a\\b"), "\"a\\\\b\"");
    assert_eq!(js_string_literal("a\nb\tc"), "\"a\\nb\\tc\"");
    assert_eq!(js_string_literal("\u{1}"), "\"\\u0001\"");
    assert_eq!(js_string_literal("扩展/🇺🇳"), "\"扩展/🇺🇳\"");
}

// ---------------------------------------------------------------------------
// CJS detection (§3.5)
// ---------------------------------------------------------------------------

#[test]
fn detects_cjs_files_like_jiti() {
    assert!(is_cjs_module("/x/a.cjs", "", Some("module")));
    // A `.mjs` with top-level CJS syntax is loaded (jiti: native import throws `module is not
    // defined`, then the CJS-evaluator fallback succeeds).
    assert!(is_cjs_module(
        "/x/a.mjs",
        "module.exports = { value: 1 };",
        None
    ));
    assert!(!is_cjs_module("/x/a.mjs", "export default 1;", None));
    assert!(!is_cjs_module("/x/a.mjs", "console.log(1);", None));
    // `.js` in a `"type": "module"` package is natively ESM; CJS syntax makes jiti's native
    // import fail and fall back to its CJS evaluator (measured), so it loads as CJS here too.
    assert!(is_cjs_module("/x/a.js", "require('x')", Some("module")));
    assert!(!is_cjs_module(
        "/x/a.js",
        "// mentions require(\nexport default value;",
        None
    ));
    assert!(is_cjs_module("/x/a.js", "require('x')", None));
    assert!(is_cjs_module(
        "/x/a.js",
        "module.exports = 1",
        Some("commonjs")
    ));
    // Plain `.js` in a CommonJS package is loaded by Node's CJS loader.
    assert!(is_cjs_module("/x/a.js", "console.log(1);", None));
    assert!(!is_cjs_module("/x/a.js", "console.log(1);", Some("module")));
    // ESM syntax wins over everything else.
    assert!(!is_cjs_module("/x/a.js", "export default 1;", None));
    assert!(!is_cjs_module("/x/a.ts", "import x from 'y';", None));
    // TypeScript without ESM syntax always runs through jiti's CJS evaluator.
    assert!(is_cjs_module("/x/a.ts", "export = 1;", Some("module")));
    assert!(is_cjs_module(
        "/x/a.ts",
        "import x = require('./x');",
        Some("module")
    ));
    assert!(is_cjs_module(
        "/x/a.ts",
        "const x: number = 1;",
        Some("module")
    ));
    // A cache-busting query does not change the extension check.
    assert!(is_cjs_module("/x/a.cjs?v=1-1", "", Some("module")));
}

#[test]
fn reads_the_nearest_package_type() {
    assert_eq!(package_type_for(&fixture("vendor/ms/package.json")), None);
    assert_eq!(
        package_type_for(&fixture("esm_pkg/package.json")).as_deref(),
        Some("module")
    );
    assert_eq!(
        package_type_for(&fixture("esm_pkg/main.js")).as_deref(),
        Some("module")
    );
}

// ---------------------------------------------------------------------------
// Execution: real npm packages (§10.4 item 1)
// ---------------------------------------------------------------------------

#[test]
fn executes_multi_file_cjs_packages_like_shell_quote() {
    with_cjs_runtime(|ctx| {
        let default = load_default(ctx, &fixture("vendor/shell-quote/index.js")).expect("loads");
        let exports = default.as_object().expect("module.exports is an object");

        let quote: Function = exports.get("quote").expect("quote export");
        let quoted: String = quote.call((vec!["a b"],)).expect("quote call");
        assert_eq!(quoted, "'a b'");
        let escaped: String = quote.call((vec!["a$b", "c"],)).expect("quote call");
        assert_eq!(escaped, "a\\$b c");

        let parse: Function = exports.get("parse").expect("parse export");
        let parts: Vec<String> = parse.call(("a b",)).expect("parse call");
        assert_eq!(parts, vec!["a".to_string(), "b".to_string()]);

        // §10.4 item 1: the cache must contain the child files, not just the entry. The entry
        // itself is evaluated by the ESM wrapper, so it is not part of __kiana_cjs.
        let cache = cache_object(ctx);
        for child in ["quote.js", "parse.js"] {
            let path = fixture_str(&format!("vendor/shell-quote/{child}"));
            assert!(
                cache.contains_key(path.as_str()).expect("cache lookup"),
                "cache is missing {child}"
            );
        }
    });
}

#[test]
fn executes_module_exports_function_packages_like_ms() {
    with_cjs_runtime(|ctx| {
        let default = load_default(ctx, &fixture("vendor/ms/index.js")).expect("loads");
        let ms = default.into_function().expect("ms exports a function");
        let millis: f64 = ms.call(("2 days",)).expect("ms call");
        assert_eq!(millis, 172_800_000.0);
        let formatted: String = ms.call((60_000.0,)).expect("ms call");
        assert_eq!(formatted, "1m");
        let invalid: Result<(), _> = ms.call((true,));
        assert!(invalid.is_err(), "ms throws for invalid input");
    });
}

#[test]
fn re_assigning_exports_at_module_top_level_works() {
    // The @anthropic-ai/sdk shape: `exports = module.exports = function (...) {}`. The body runs
    // inside `new Function`, so `exports` is a writable parameter binding (§3.5 item 6).
    with_cjs_runtime(|ctx| {
        let default = load_default(ctx, &fixture("shapes/sdk_pattern.js")).expect("loads");
        let double = default
            .into_function()
            .expect("module.exports is a function");
        let value: f64 = double.call((21.0,)).expect("call");
        assert_eq!(value, 42.0);
        assert_eq!(double.get::<_, String>("version").unwrap(), "1.0.0");
    });
}

#[test]
fn exports_property_shape_reaches_the_consumer() {
    with_cjs_runtime(|ctx| {
        let default = load_default(ctx, &fixture("shapes/exports_prop.js")).expect("loads");
        let exports = default.as_object().expect("object");
        let quote: Function = exports.get("quote").expect("quote");
        let out: String = quote.call((vec!["x y"],)).expect("call");
        assert_eq!(out, "'x y'");
    });
}

#[test]
fn top_level_this_is_module_exports() {
    with_cjs_runtime(|ctx| {
        let default = load_default(ctx, &fixture("shapes/this_alias.js")).expect("loads");
        let exports = default.as_object().expect("object");
        assert!(exports.get::<_, bool>("sameThis").unwrap());
        assert!(exports.get::<_, bool>("sameExports").unwrap());
    });
}

#[test]
fn require_api_exposes_resolve_cache_and_main() {
    with_cjs_runtime(|ctx| {
        let default = load_default(ctx, &fixture("shapes/require_api.js")).expect("loads");
        let exports = default.as_object().expect("object");
        let quote = fixture_str("shapes/quote.js");
        assert_eq!(exports.get::<_, String>("resolved").unwrap(), quote);
        assert!(exports.get::<_, bool>("cacheIsGlobal").unwrap());
        assert!(exports.get::<_, bool>("inCache").unwrap());
        assert!(exports.get::<_, bool>("loadedMatchesCache").unwrap());
        assert_eq!(exports.get::<_, String>("requireType").unwrap(), "function");
        assert_eq!(exports.get::<_, String>("resolveType").unwrap(), "function");
        let main: Value = exports.get("main").unwrap();
        assert!(main.is_undefined());
    });
}

#[test]
fn requires_json_modules() {
    with_cjs_runtime(|ctx| {
        let default = load_default(ctx, &fixture("json/main.js")).expect("loads");
        let exports = default.as_object().expect("object");
        assert_eq!(exports.get::<_, String>("name").unwrap(), "kiana");
        let nested: Vec<i64> = exports.get("nested").unwrap();
        assert_eq!(nested, vec![1, 2, 3]);
        assert!(
            cache_object(ctx)
                .contains_key(fixture_str("json/data.json").as_str())
                .unwrap()
        );
    });
}

#[test]
fn supports_circular_requires() {
    with_cjs_runtime(|ctx| {
        let default = load_default(ctx, &fixture("circular/a.js")).expect("loads");
        let exports = default.as_object().expect("object");
        assert_eq!(exports.get::<_, String>("name").unwrap(), "a");
        assert_eq!(exports.get::<_, String>("bName").unwrap(), "b");
        assert_eq!(exports.get::<_, String>("seenFromB").unwrap(), "a");
    });
}

#[test]
fn strips_a_shebang_like_node() {
    with_cjs_runtime(|ctx| {
        let default = load_default(ctx, &fixture("shapes/shebang.js")).expect("loads");
        assert_eq!(
            default.into_string().unwrap().to_string().unwrap(),
            "shebang ok"
        );
    });
}

#[test]
fn require_of_an_es_module_throws_err_require_esm() {
    with_cjs_runtime(|ctx| {
        let result: Result<Value, CaughtError> =
            eval_module(ctx, &fixture("esm/index.js")).catch(ctx);
        let CaughtError::Exception(exception) = result.expect_err("require must throw") else {
            panic!("expected a JS exception");
        };
        let object = exception.as_object();
        assert_eq!(object.get::<_, String>("code").unwrap(), "ERR_REQUIRE_ESM");
        let message = exception.message().expect("message");
        let target = fixture_str("esm/target.mjs");
        let from = fixture_str("esm/index.js");
        assert_eq!(
            message,
            format!(
                "require() of ES Module {target} from {from} not supported.\n\
                 Instead change the require of {target} in {from} to a dynamic import() which is available in all CommonJS modules."
            )
        );
    });
}

#[test]
fn require_of_an_esm_dependency_in_a_type_module_package_throws_err_require_esm() {
    // `esm_pkg/main.js` uses CJS syntax (so it is wrapped), but its `./dep.js` dependency has ESM
    // syntax; requiring an ES module is design-frozen to ERR_REQUIRE_ESM (§3.5 item 2).
    with_cjs_runtime(|ctx| {
        let result: Result<Value, CaughtError> =
            eval_module(ctx, &fixture("esm_pkg/main.js")).catch(ctx);
        let CaughtError::Exception(exception) = result.expect_err("require must throw") else {
            panic!("expected a JS exception");
        };
        let object = exception.as_object();
        assert_eq!(object.get::<_, String>("code").unwrap(), "ERR_REQUIRE_ESM");
    });
}

#[test]
fn failed_modules_are_evicted_from_the_cache() {
    with_cjs_runtime(|ctx| {
        let _: Result<Value, CaughtError> = eval_module(ctx, &fixture("esm/index.js")).catch(ctx);
        assert!(
            !cache_object(ctx)
                .contains_key(fixture_str("esm/index.js").as_str())
                .unwrap()
        );
    });
}

// ---------------------------------------------------------------------------
// ESM interop: named exports and jiti's `default` unwrapping (review finding 1)
// ---------------------------------------------------------------------------

#[test]
fn named_imports_link_against_a_wrapped_cjs_module() {
    // jiti: `import { quote } from "shell-quote"` works because Node detects CJS named exports.
    // The wrapper must declare them statically or QuickJS fails at link time with
    // `Could not find export 'quote'`.
    let path = fixture_str("vendor/shell-quote/index.js");
    let sources = vec![(
        path.clone(),
        module_source(&fixture("vendor/shell-quote/index.js")),
    )];
    let entry = format!(
        "import {{ quote, parse }} from {};\n\
         globalThis.__out = quote([\"a b\"]) + \"|\" + parse(\"a b\").join(\"/\");",
        js_string_literal(&path)
    );
    assert_eq!(
        run_esm_with_sources(sources, &entry).expect("links"),
        "'a b'|a/b"
    );
}

#[test]
fn default_import_unwraps_a_compiled_cjs_factory() {
    // esbuild/tsc CJS output: `exports.__esModule = true; exports.default = factory`.
    // jiti.import(path, { default: true }) yields `exports.default` (the function).
    let path = fixture_str("interop/compiled.cjs");
    let sources = vec![(
        path.clone(),
        module_source(&fixture("interop/compiled.cjs")),
    )];
    let entry = format!(
        "import factory, {{ named }} from {path};\n\
         import * as namespace from {path};\n\
         globalThis.__out = typeof factory + \":\" + factory(21) + \":\" + named + \":\" + namespace[\"dash-name\"];",
        path = js_string_literal(&path)
    );
    // Node's CJS namespace also exposes non-identifier names (`namespace["dash-name"]`).
    assert_eq!(
        run_esm_with_sources(sources, &entry).expect("links"),
        "function:42:5:7"
    );
}

#[test]
fn module_exports_function_is_the_default_export() {
    let path = fixture_str("vendor/ms/index.js");
    let sources = vec![(path.clone(), module_source(&fixture("vendor/ms/index.js")))];
    let entry = format!(
        "import ms from {};\n globalThis.__out = typeof ms + \":\" + ms(60000);",
        js_string_literal(&path)
    );
    assert_eq!(
        run_esm_with_sources(sources, &entry).expect("links"),
        "function:1m"
    );
}

// ---------------------------------------------------------------------------
// Sloppy evaluation + Node module record + require.resolve API (review findings 8-11)
// ---------------------------------------------------------------------------

#[test]
fn cjs_bodies_run_in_sloppy_mode_like_node() {
    with_cjs_runtime(|ctx| {
        // `implicitGlobalVar = 42` throws ReferenceError in strict mode.
        let default = load_default(ctx, &fixture("shapes/implicit_global.js")).expect("loads");
        let exports = default.as_object().expect("object");
        assert_eq!(exports.get::<_, f64>("value").unwrap(), 42.0);
        let global: f64 = ctx.eval("implicitGlobalVar").expect("global created");
        assert_eq!(global, 42.0);

        // `with` is a SyntaxError in strict mode.
        let default = load_default(ctx, &fixture("shapes/with_scope.js")).expect("loads");
        let exports = default.as_object().expect("object");
        assert_eq!(exports.get::<_, f64>("value").unwrap(), 1.0);
    });
}

#[test]
fn wrapped_entry_is_a_node_module_record() {
    with_cjs_runtime(|ctx| {
        let path = fixture_str("records/entry.js");
        let name = format!("{path}?v=1-1");
        let default = load_default_named(ctx, &name, &fixture("records/entry.js")).expect("loads");
        let exports = default.as_object().expect("object");
        let dir = fixture_str("records");

        assert!(exports.get::<_, bool>("selfInCache").unwrap());
        // `__filename`/`__dirname` are the real paths, without the `?v=` cache buster.
        assert_eq!(exports.get::<_, String>("file").unwrap(), path);
        assert_eq!(exports.get::<_, String>("dir").unwrap(), dir);
        assert_eq!(exports.get::<_, String>("moduleId").unwrap(), path);
        assert_eq!(exports.get::<_, String>("moduleFilename").unwrap(), path);
        assert_eq!(exports.get::<_, String>("modulePath").unwrap(), dir);
        assert!(!exports.get::<_, bool>("loadedDuringEval").unwrap());
        assert_eq!(exports.get::<_, String>("requireType").unwrap(), "function");
        assert_eq!(exports.get::<_, i32>("childrenBefore").unwrap(), 0);
        assert_eq!(exports.get::<_, i32>("childrenAfter").unwrap(), 1);
        assert!(exports.get::<_, bool>("childParentIsEntry").unwrap());
        // A child's `require(entry)` reuses the same record: one evaluation, same exports object.
        assert!(exports.get::<_, bool>("selfSame").unwrap());
        assert!(exports.get::<_, bool>("cacheExportsSame").unwrap());

        // The record stays registered after evaluation and is marked loaded (Node semantics).
        let cache = cache_object(ctx);
        let record: Object = cache.get(path.as_str()).expect("cache entry");
        assert!(record.get::<_, bool>("loaded").unwrap());
        let children: Vec<Object> = record.get("children").unwrap();
        assert_eq!(children.len(), 1);
    });
}

#[test]
fn require_resolve_exposes_paths_and_honours_the_paths_option() {
    with_cjs_runtime(|ctx| {
        let shapes = fixture("shapes");
        let paths = vec![shapes.to_string_lossy().into_owned()];
        ctx.globals()
            .set("__test_paths", paths)
            .expect("set test paths");

        let default = load_default(ctx, &fixture("shapes/require_paths.js")).expect("loads");
        let exports = default.as_object().expect("object");
        assert!(exports.get::<_, bool>("pathsIsArray").unwrap());
        assert_eq!(
            exports.get::<_, String>("firstSuffix").unwrap(),
            "/node_modules"
        );
        // Node returns `null` for core modules.
        let builtin_paths: Value = exports.get("builtinPaths").unwrap();
        assert!(builtin_paths.is_null());
        // `{ paths: [...] }` resolves relative to the supplied directories.
        assert_eq!(
            exports.get::<_, String>("resolvedWithPaths").unwrap(),
            fixture_str("shapes/quote.js")
        );
        // `require.resolve('fs')` returns the builtin name as written (Node behaviour).
        assert_eq!(exports.get::<_, String>("builtinResolve").unwrap(), "fs");
    });
}

#[test]
fn esm_typescript_gets_the_jiti_cjs_bindings() {
    // Review finding 2 (false negative): `examples/extensions/notify.ts` is ESM in a
    // `"type": "module"` package but calls `require()`. jiti provides a working `require`,
    // `__filename` and `__dirname` for every TS module; strip_types injects them.
    use kiana_extensions::loader::transpile::strip_types;

    with_cjs_runtime(|ctx| {
        let name = fixture_str("ts_require_probe.ts");
        let source = "export default function probe(): string {\n\
             return [typeof require, typeof require(\"./shapes/quote.js\"), typeof __filename, typeof __dirname].join(\",\");\n\
         }";
        let code = strip_types(&name, source).expect("strips");
        let module = Module::declare(ctx.clone(), name.clone(), code).expect("declares");
        let (module, promise) = module.eval().expect("evaluates");
        promise.finish::<()>().expect("finishes");
        let probe: Function = module
            .namespace()
            .expect("namespace")
            .get("default")
            .expect("default");
        let out: String = probe.call(()).expect("probe runs");
        assert_eq!(out, "function,function,string,string");
    });
}

#[test]
fn cjs_require_reaches_node_builtins_and_virtual_modules() {
    // Review finding 2: jiti's require returns the real builtin; this loader dispatches `node:*`
    // keys to `globalThis.__kiana_node_shims` (the loader-host contract) and virtual module keys to
    // `globalThis.__kiana_virtual` (design §3.3).
    with_cjs_runtime(|ctx| {
        ctx.eval::<(), _>(
            "globalThis.__kiana_node_shims = {\n\
               'node:net': { createServer: function () {} },\n\
               'node:events': { EventEmitter: function () {} },\n\
             };\n\
             globalThis.__kiana_virtual = { typebox: { Type: { Object: 7 } } };",
        )
        .expect("install shim tables");

        let default = load_default(ctx, &fixture("shapes/require_builtins.js")).expect("loads");
        let exports = default.as_object().expect("object");
        assert!(exports.get::<_, bool>("sameInstance").unwrap());
        assert_eq!(
            exports.get::<_, String>("hasCreateServer").unwrap(),
            "function"
        );
        assert_eq!(
            exports.get::<_, String>("hasEventEmitter").unwrap(),
            "function"
        );
        assert!(exports.get::<_, bool>("virtualSame").unwrap());
        assert_eq!(exports.get::<_, i32>("namespaceValue").unwrap(), 7);
        // Node's require.resolve returns the request as written.
        assert_eq!(exports.get::<_, String>("resolvedBare").unwrap(), "fs");
        assert_eq!(exports.get::<_, String>("resolvedNode").unwrap(), "node:fs");
        // ...and resolve.paths is null for builtins.
        let paths: Option<Vec<String>> = exports.get("builtinPaths").unwrap();
        assert!(paths.is_none());
    });
}

#[test]
fn cjs_require_accepts_host_synthesized_builtin_source() {
    // Hosts that do not publish a shim table may instead synthesize CJS text for `node:*`
    // through `__kiana_host.read` (documented fallback alongside `__kiana_node_shims`).
    with_cjs_runtime(|ctx| {
        ctx.eval::<(), _>(
            "globalThis.__kiana_node_shims = Object.create(null);\n\
             const originalRead = __kiana_host.read;\n\
             __kiana_host.read = function (path) {\n\
               if (path === 'node:events') {\n\
                 return 'module.exports = { EventEmitter: function EventEmitter() {} };';\n\
               }\n\
               return originalRead(path);\n\
             };",
        )
        .expect("install synthesized builtin reader");

        let default =
            load_default(ctx, &fixture("shapes/require_builtin_source.js")).expect("loads");
        let exports = default.as_object().expect("object");
        assert!(exports.get::<_, bool>("same").unwrap());
        assert_eq!(
            exports.get::<_, String>("hasEventEmitter").unwrap(),
            "function"
        );
    });
}

#[test]
fn require_failures_use_nodes_module_not_found_shape() {
    // Review finding 6: `err.code === 'MODULE_NOT_FOUND'` and `Cannot find module '<spec>'`.
    with_cjs_runtime(|ctx| {
        let default = load_default(ctx, &fixture("shapes/require_missing.js")).expect("loads");
        let exports = default.as_object().expect("object");
        assert!(!exports.get::<_, bool>("ok").unwrap());
        assert_eq!(exports.get::<_, String>("name").unwrap(), "Error");
        assert_eq!(
            exports.get::<_, String>("code").unwrap(),
            "MODULE_NOT_FOUND"
        );
        let requirer = fixture_str("shapes/require_missing.js");
        assert_eq!(
            exports.get::<_, String>("message").unwrap(),
            format!("Cannot find module './definitely-missing'\nRequire stack:\n- {requirer}")
        );
    });
}

#[test]
fn require_argument_errors_match_node() {
    // Review finding 8: ERR_INVALID_ARG_TYPE with Node's received-type wording.
    with_cjs_runtime(|ctx| {
        let default = load_default(ctx, &fixture("shapes/require_invalid_arg.js")).expect("loads");
        let entries = default.into_array().expect("array");
        let mut rendered = Vec::new();
        for index in 0..entries.len() {
            let entry = entries
                .get::<Value>(index)
                .expect("array entry")
                .into_object()
                .expect("object");
            let name: String = entry.get("name").unwrap();
            let code: String = entry.get("code").unwrap();
            let message: String = entry.get("message").unwrap();
            rendered.push(format!("{name}|{code}|{message}"));
        }
        assert_eq!(
            rendered,
            vec![
                "TypeError|ERR_INVALID_ARG_TYPE|The \"id\" argument must be of type string. Received type number (5)"
                    .to_string(),
                "TypeError|ERR_INVALID_ARG_TYPE|The \"id\" argument must be of type string. Received null"
                    .to_string(),
                "TypeError|ERR_INVALID_ARG_TYPE|The \"id\" argument must be of type string. Received undefined"
                    .to_string(),
                "TypeError|ERR_INVALID_ARG_TYPE|The \"id\" argument must be of type string. Received type boolean (true)"
                    .to_string(),
                "TypeError|ERR_INVALID_ARG_TYPE|The \"id\" argument must be of type string. Received an instance of Object"
                    .to_string(),
                "TypeError|ERR_INVALID_ARG_TYPE|The \"id\" argument must be of type string. Received an instance of Array"
                    .to_string(),
                "TypeError|ERR_INVALID_ARG_TYPE|The \"request\" argument must be of type string. Received type number (5)"
                    .to_string(),
                "TypeError|ERR_INVALID_ARG_TYPE|The \"request\" argument must be of type string. Received type number (5)"
                    .to_string(),
            ]
        );
    });
}

#[test]
fn require_of_typescript_esm_dependencies_is_refused_by_design() {
    // Design §3.5 item 2 freezes `ERR_REQUIRE_ESM` for ES modules. jiti 2.7 would transpile the
    // `.ts` dependency and return its exports; that needs an ESM→CJS transform the architecture
    // does not have, so this stays an intentional divergence (see the unit report).
    with_cjs_runtime(|ctx| {
        let result: Result<Value, CaughtError> =
            eval_module(ctx, &fixture("ts_esm/main.js")).catch(ctx);
        let CaughtError::Exception(exception) = result.expect_err("require must throw") else {
            panic!("expected a JS exception");
        };
        let object = exception.as_object();
        assert_eq!(object.get::<_, String>("code").unwrap(), "ERR_REQUIRE_ESM");
        assert!(
            exception
                .message()
                .expect("message")
                .contains("ts_esm/dep.ts")
        );
    });
}

#[test]
fn declaration_exports_load_as_esm_modules() {
    // Review finding 1: `export function` / `export interface` are the most common ESM forms.
    // The plan-mode example (`utils.ts`) must classify as ESM, strip, and evaluate.
    use kiana_extensions::loader::transpile::strip_types;

    with_cjs_runtime(|ctx| {
        let name = fixture_str("ts_decl_exports.ts");
        let source = "export function isSafeCommand(command: string): boolean {\n\
                      \treturn command.length > 0;\n\
                      }\n\
                      export interface Marker {\n\
                      \tvalue: number;\n\
                      }\n\
                      export enum Mode { A = 1 }\n";
        assert!(!is_cjs_module(&name, source, Some("module")));
        let code = strip_types(&name, source).expect("strips");
        assert!(code.contains("export function isSafeCommand"));
        let module = Module::declare(ctx.clone(), name.clone(), code).expect("declares");
        let (module, promise) = module.eval().expect("evaluates");
        promise.finish::<()>().expect("finishes");
        let namespace = module.namespace().expect("namespace");
        let is_safe: Function = namespace.get("isSafeCommand").expect("named export");
        let safe: bool = is_safe.call(("ls -la",)).expect("call");
        assert!(safe);
        assert!(namespace.contains_key("Mode").expect("enum export"));
    });
}

#[test]
fn type_only_declarations_mixed_with_module_exports_evaluate_as_esm() {
    // Regression: before the ExportDeclaration fix this was classified CJS and `export {};` was
    // fed to `new Function` (SyntaxError). It now loads as ESM with jiti's `module`/`exports`
    // bindings, so evaluation must not throw.
    use kiana_extensions::loader::transpile::strip_types;

    with_cjs_runtime(|ctx| {
        for (name, source) in [
            (
                fixture_str("ts_type_only.ts"),
                "export type Marker = 1;\nmodule.exports = 1;",
            ),
            (
                fixture_str("ts_type_only_reexport.ts"),
                "export type { X } from './y.js';\nmodule.exports = 1;",
            ),
        ] {
            let code = strip_types(&name, source).expect("strips");
            let module =
                Module::declare(ctx.clone(), name.clone(), code.clone()).expect("declares");
            let (module, promise) = module.eval().expect("evaluates");
            promise.finish::<()>().expect("finishes");
            let namespace = module.namespace().expect("namespace");
            assert!(
                !namespace.contains_key("default").expect("namespace lookup"),
                "unexpected default export for {name}; code={code:?}"
            );
        }
    });
}

#[test]
fn esm_typescript_gets_module_and_exports_bindings() {
    // Review finding 3: jiti provides `module`/`exports` to every transformed module, including
    // ones with ESM syntax.
    use kiana_extensions::loader::transpile::strip_types;

    with_cjs_runtime(|ctx| {
        let name = fixture_str("ts_module_exports.ts");
        let source = "import type { A } from './unused.js';\n\
                      exports.value = 42;\n\
                      module.exports.extra = 7;\n\
                      export default (exports as any).value + (module as any).exports.extra;";
        let code = strip_types(&name, source).expect("strips");
        assert!(code.contains("var module = {"));
        assert!(code.contains("var exports = module.exports;"));
        let module = Module::declare(ctx.clone(), name.clone(), code).expect("declares");
        let (module, promise) = module.eval().expect("evaluates");
        promise.finish::<()>().expect("finishes");
        let default: f64 = module
            .namespace()
            .expect("namespace")
            .get("default")
            .expect("default");
        assert_eq!(default, 49.0);
    });

    with_cjs_runtime(|ctx| {
        let name = fixture_str("ts_module_reassign.ts");
        let source = "module.exports = { x: 5 };\n\
                      export default (module as any).exports.x;";
        let code = strip_types(&name, source).expect("strips");
        let module = Module::declare(ctx.clone(), name.clone(), code).expect("declares");
        let (module, promise) = module.eval().expect("evaluates");
        promise.finish::<()>().expect("finishes");
        let default: f64 = module
            .namespace()
            .expect("namespace")
            .get("default")
            .expect("default");
        assert_eq!(default, 5.0);
    });
}

#[test]
fn ambient_declarations_do_not_suppress_the_jiti_bindings() {
    // Review finding 9: `declare` creates no runtime binding, so `__dirname` still needs jiti's
    // wrapper parameter (the same file without `declare` already worked).
    use kiana_extensions::loader::transpile::strip_types;

    with_cjs_runtime(|ctx| {
        let name = fixture_str("ts_ambient.ts");
        let source = "declare const __dirname: string;\n\
                      declare function require(s: string): any;\n\
                      export default function probe(): string {\n\
                      \treturn [__dirname, typeof require, __filename].join('|');\n\
                      }";
        let code = strip_types(&name, source).expect("strips");
        assert!(code.contains("var __dirname = "));
        assert!(code.contains("var require = globalThis.__kiana_make_require("));
        let module = Module::declare(ctx.clone(), name.clone(), code).expect("declares");
        let (module, promise) = module.eval().expect("evaluates");
        promise.finish::<()>().expect("finishes");
        let probe: Function = module
            .namespace()
            .expect("namespace")
            .get("default")
            .expect("default");
        let out: String = probe.call(()).expect("probe runs");
        let dir = fixture("ts_ambient.ts")
            .parent()
            .expect("fixture parent")
            .to_string_lossy()
            .into_owned();
        assert_eq!(out, format!("{dir}|function|{name}"));
    });
}

#[test]
fn named_imports_follow_tslib_style_reexports() {
    // zod/tslib barrel shape: `__exportStar(require("./external.cjs"), exports)`. Node's
    // cjs-module-lexer follows these, so named imports must link.
    let path = fixture_str("interop/reexport/index.cjs");
    let sources = vec![(
        path.clone(),
        module_source(&fixture("interop/reexport/index.cjs")),
    )];
    let entry = format!(
        "import {{ z, string, object }} from {};\n\
         globalThis.__out = z + \":\" + string() + \":\" + object();",
        js_string_literal(&path)
    );
    assert_eq!(
        run_esm_with_sources(sources, &entry).expect("links"),
        "1:s:o"
    );
}

#[test]
fn native_cjs_default_interop_matches_jiti() {
    // Measured jiti 2.7 with kiana's options (`moduleCache: false`, `tryNative: false`) for a native
    // CJS file (`.js` outside a type:module package, `.cjs`):
    // `me.__esModule && typeof me.default === "function" ? me.default : me`.
    let nonfn_path = fixture_str("interop/nonfn_default.js");
    let sources = vec![(
        nonfn_path.clone(),
        module_source(&fixture("interop/nonfn_default.js")),
    )];
    let entry = format!(
        "import value from {};\n globalThis.__out = typeof value + \":\" + value.default + \":\" + value.other;",
        js_string_literal(&nonfn_path)
    );
    // No `__esModule`: the whole module.exports object stays the default (not `42`).
    assert_eq!(
        run_esm_with_sources(sources, &entry).expect("links"),
        "object:42:1"
    );

    // `module.exports = function outer; module.exports.default = 5` → the function, not `5`.
    let fn_path = fixture_str("interop/native_fn_default.js");
    let sources = vec![(
        fn_path.clone(),
        module_source(&fixture("interop/native_fn_default.js")),
    )];
    let entry = format!(
        "import value from {};\n globalThis.__out = typeof value + \":\" + value();",
        js_string_literal(&fn_path)
    );
    assert_eq!(
        run_esm_with_sources(sources, &entry).expect("links"),
        "function:outer"
    );

    // `__esModule` + a function `default` unwraps to `default` (compiled CJS factory).
    let compiled_path = fixture_str("interop/compiled.cjs");
    let sources = vec![(
        compiled_path.clone(),
        module_source(&fixture("interop/compiled.cjs")),
    )];
    let entry = format!(
        "import factory from {};\n globalThis.__out = typeof factory + \":\" + factory(2);",
        js_string_literal(&compiled_path)
    );
    assert_eq!(
        run_esm_with_sources(sources, &entry).expect("links"),
        "function:4"
    );
}

#[test]
fn transformed_default_interop_matches_jiti() {
    // Measured jiti 2.7 for Babel-transformed modules (TS extensions, `.mjs`, type:module `.js`):
    // `me.default == null ? me : (me.__esModule && typeof me.default.default === "function"
    // ? me.default.default : me.default)`.
    use kiana_extensions::loader::transpile::strip_types;

    let source_for = |relative: &str| {
        let path = fixture(relative);
        let name = path.to_string_lossy().into_owned();
        let raw = std::fs::read_to_string(&path).expect("fixture is readable");
        let code = strip_types(&name, &raw).expect("strips");
        let source = wrap_module(&name, &code);
        (name, source)
    };

    // `.ts`: `{ default: { default: fn } }` without `__esModule` → `me.default` (the object).
    let (nested_path, nested_source) = source_for("interop/ts_nested_default.ts");
    let entry = format!(
        "import value from {};\n globalThis.__out = typeof value + \":\" + value.default();",
        js_string_literal(&nested_path)
    );
    assert_eq!(
        run_esm_with_sources(vec![(nested_path, nested_source)], &entry).expect("links"),
        "object:7"
    );

    // `.ts` with an explicit `__esModule` → `me.default.default` (the function).
    let (esm_path, esm_source) = source_for("interop/ts_esmodule_nested.ts");
    let entry = format!(
        "import value from {};\n globalThis.__out = typeof value + \":\" + value();",
        js_string_literal(&esm_path)
    );
    assert_eq!(
        run_esm_with_sources(vec![(esm_path, esm_source)], &entry).expect("links"),
        "function:8"
    );

    // `.ts` with `default: null` → the whole module.exports object.
    let (null_path, null_source) = source_for("interop/ts_null_default.ts");
    let entry = format!(
        "import value from {};\n globalThis.__out = typeof value + \":\" + String(value.default) + \":\" + value.named;",
        js_string_literal(&null_path)
    );
    assert_eq!(
        run_esm_with_sources(vec![(null_path, null_source)], &entry).expect("links"),
        "object:null:n"
    );

    // `.ts`: `default` is not a function but non-null → `me.default` (5), not the function.
    let (fn_path, fn_source) = source_for("interop/ts_fn_default.ts");
    let entry = format!(
        "import value from {};\n globalThis.__out = typeof value + \":\" + value;",
        js_string_literal(&fn_path)
    );
    assert_eq!(
        run_esm_with_sources(vec![(fn_path, fn_source)], &entry).expect("links"),
        "number:5"
    );

    // `.ts`: non-null `default: 42` → `me.default` (42).
    let (nonfn_path, nonfn_source) = source_for("interop/ts_nonfn_default.ts");
    let entry = format!(
        "import value from {};\n globalThis.__out = typeof value + \":\" + value;",
        js_string_literal(&nonfn_path)
    );
    assert_eq!(
        run_esm_with_sources(vec![(nonfn_path, nonfn_source)], &entry).expect("links"),
        "number:42"
    );

    // A `.js` in a `"type": "module"` package is transformed by jiti, so the same rule applies.
    let (type_module_path, type_module_source) = source_for("esm_pkg/cjs_shape.js");
    let entry = format!(
        "import value from {};\n globalThis.__out = typeof value + \":\" + value.default();",
        js_string_literal(&type_module_path)
    );
    assert_eq!(
        run_esm_with_sources(vec![(type_module_path, type_module_source)], &entry).expect("links"),
        "object:9"
    );
}
