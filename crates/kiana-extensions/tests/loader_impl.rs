//! `rquickjs::Loader` 测试（`spec/design-extension-runtime.md` §3.1、§3.4、§3.5、§3.9）。
//!
//! 覆盖：
//! - TS/JS/CJS/JSON/虚拟模块/node shim 的完整流水线与 `import.meta`
//!   （`url`/`main`/`resolve`，`.js`/`.mjs` 另有 `dirname`/`filename`）；
//! - JSON 顶层键的具名导出（jiti 把 JSON 当 `require` 目标）；
//! - `data:` URL 的 Node 接受/拒绝集合与带 `code` 的异常；
//! - 语法错误与缺失模块的【逐字】错误前缀（依赖错误不叠加父模块前缀）；
//! - 真 CJS 的 `require` 桥（宿主原语由测试自己安装，见 `js/cjs.js` 的契约）、宿主缺失的
//!   明确错误与 `require.resolve.paths`；
//! - `.js`/`.mjs` 里自由 CJS 名字的注入（jiti 的 CJS 回退）；
//! - §3.9 的声明顺序（`declare_all` 后不再回调 loader）与跨 Runtime 的 per-Context 重绑定。
//!
//! 移植自 TS 参考的 `packages/coding-agent/test/extensions-discovery.test.ts` 中**只涉及模块加载**
//! 的用例（`resolves dependencies from extension's own node_modules`、
//! `reports errors for invalid extension code`、`loads extension with ...`），以及
//! `extension-factory-cache.test.ts` 的模块级一半（见 `module_cache.rs`）。
//! `9540-extension-loader-lazy.test.ts` / `8237-node-sea-extension-loading.test.ts` 在 Rust 侧
//! 不适用（没有 jiti/Babel 的惰性分支，见 `loader_impl` 的模块文档）。
#![cfg(feature = "runtime")]

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use kiana_extensions::loader::cjs::{is_cjs_module, package_type_for};
use kiana_extensions::loader::loader_impl::{
    ExtensionLoader, LoaderConfig, ModuleKind, ScopedResolver,
};
use kiana_extensions::loader::resolver::{
    ModuleResolver, ResolutionKind, ResolverOptions, VirtualAlias, VirtualFile, VirtualFs,
};
use rquickjs::loader::{ImportAttributes, Loader, Resolver};
use rquickjs::{Context, Ctx, Function, Object, Runtime};

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/loader")
}

fn fixture(relative: &str) -> String {
    fixtures().join(relative).to_string_lossy().into_owned()
}

/// typebox 的嵌入文件表（真实内容在 U10；这里只需要可解析的 ESM）。
static VIRTUAL_FILES: [VirtualFile; 2] = [
    VirtualFile {
        name: "typebox/build/index.mjs",
        source: b"export const Type = { Object: (shape) => ({ shape }) };\n",
    },
    VirtualFile {
        name: "typebox/build/compile/index.mjs",
        source: b"export const compile = (schema) => () => schema;\n",
    },
];

static VIRTUAL_ALIASES: [VirtualAlias; 1] = [VirtualAlias {
    key: "typebox",
    target: "typebox/build/index.mjs",
}];

fn virtual_fs() -> VirtualFs {
    VirtualFs::new(&VIRTUAL_FILES, &VIRTUAL_ALIASES)
}

struct Harness {
    _rt: Runtime,
    ctx: Context,
    loader: ExtensionLoader,
}

impl Harness {
    fn new() -> Self {
        Self::with_strict(false)
    }

    fn with_strict(strict: bool) -> Self {
        let rt = Runtime::new().expect("create runtime");
        let ctx = Context::full(&rt).expect("create context");
        let loader = ExtensionLoader::new(LoaderConfig {
            virtual_fs: virtual_fs(),
            strict_nocache: strict,
        });
        loader.register_source("node:path", "export const sep = \"/\";\n");
        loader.register_source(
            "@earendil-works/kiana-ai/models",
            "export const key = \"kiana-ai/models\";\n",
        );
        let resolver = ModuleResolver::new(ResolverOptions {
            extension_dir: Some(fixtures()),
            virtual_fs: virtual_fs(),
        });
        rt.set_loader(loader.resolver(resolver), loader.clone());
        Self {
            _rt: rt,
            ctx,
            loader,
        }
    }

    fn eval<T>(&self, source: &str) -> T
    where
        T: for<'js> rquickjs::FromJs<'js>,
    {
        self.ctx
            .with(|ctx| ctx.eval(source))
            .unwrap_or_else(|error| panic!("eval `{source}` failed: {error:?}"))
    }

    /// 声明 + 求值一个入口模块（名字可以是带 `?v=` 的真实路径）。
    fn load(&self, name: &str) -> rquickjs::Result<()> {
        self.ctx.with(|ctx| {
            let module = self.loader.load_module(&ctx, name)?;
            let (_module, promise) = module.eval()?;
            promise.finish::<()>()
        })
    }

    /// 求值并取一个具名导出。
    fn export<T>(&self, name: &str, export: &str) -> rquickjs::Result<T>
    where
        T: for<'js> rquickjs::FromJs<'js>,
    {
        self.ctx.with(|ctx| {
            let module = self.loader.load_module(&ctx, name)?;
            let (module, promise) = module.eval()?;
            promise.finish::<()>()?;
            module.namespace()?.get(export)
        })
    }

    fn default_export<T>(&self, name: &str) -> T
    where
        T: for<'js> rquickjs::FromJs<'js>,
    {
        self.export(name, "default")
            .unwrap_or_else(|error| panic!("load `{name}` failed: {error:?}"))
    }

    /// 默认导出必须是函数，并调用它（`Value<'js>` 不能跨 `with` 闭包，所以调用也在里面）。
    fn call_default<T>(&self, name: &str) -> rquickjs::Result<T>
    where
        T: for<'js> rquickjs::FromJs<'js>,
    {
        self.ctx.with(|ctx| {
            let module = self.loader.load_module(&ctx, name)?;
            let (module, promise) = module.eval()?;
            promise.finish::<()>()?;
            let default: Function = module.namespace()?.get("default")?;
            default.call(())
        })
    }

    /// 调用具名导出。
    fn call_export<T>(&self, name: &str, export: &str) -> rquickjs::Result<T>
    where
        T: for<'js> rquickjs::FromJs<'js>,
    {
        self.ctx.with(|ctx| {
            let module = self.loader.load_module(&ctx, name)?;
            let (module, promise) = module.eval()?;
            promise.finish::<()>()?;
            let function: Function = module.namespace()?.get(export)?;
            function.call(())
        })
    }

    /// 默认导出是对象时读一个字段。
    fn default_field<T>(&self, name: &str, field: &str) -> rquickjs::Result<T>
    where
        T: for<'js> rquickjs::FromJs<'js>,
    {
        self.ctx.with(|ctx| {
            let module = self.loader.load_module(&ctx, name)?;
            let (module, promise) = module.eval()?;
            promise.finish::<()>()?;
            let default: Object = module.namespace()?.get("default")?;
            default.get(field)
        })
    }

    /// 默认导出是否是函数。
    fn default_is_function(&self, name: &str) -> bool {
        self.ctx
            .with(|ctx| -> rquickjs::Result<bool> {
                let module = self.loader.load_module(&ctx, name)?;
                let (module, promise) = module.eval()?;
                promise.finish::<()>()?;
                let default: rquickjs::Value = module.namespace()?.get("default")?;
                Ok(default.is_function())
            })
            .unwrap_or_else(|error| panic!("load `{name}` failed: {error:?}"))
    }

    /// 模块加载失败时的错误文本。先捕获挂起异常（若有），避免污染后续求值。
    fn load_error(&self, name: &str) -> String {
        let error = self.load(name).expect_err("expected the module to fail");
        if error.is_exception() {
            let (message, _code) = self.catch_exception();
            message
        } else {
            error.to_string()
        }
    }

    /// 取走挂起异常：`(message, code)`（Node 的 `ERR_*` 形状）。
    fn catch_exception(&self) -> (String, Option<String>) {
        self.ctx.with(|ctx| {
            let value = ctx.catch();
            let exception = value
                .as_exception()
                .unwrap_or_else(|| panic!("expected a JS exception, got {value:?}"));
            let message = exception.message().unwrap_or_default();
            let code = exception
                .as_object()
                .get::<_, Option<String>>("code")
                .ok()
                .flatten();
            (message, code)
        })
    }

    /// 安装 loader-host 的两个同步原语（`js/cjs.js` 的契约；测试自己实现，不依赖 U3）。
    fn install_host_io(&self) {
        self.ctx.with(|ctx| install_host_io(&ctx));
    }
}

/// 在任意 Context 上安装 `__kiana_host.resolve` / `__kiana_host.read`（跨 Runtime 的重绑定测试也用）。
fn install_host_io(ctx: &Ctx<'_>) {
    let root = fixtures();
    let resolver = Rc::new(RefCell::new(ModuleResolver::new(ResolverOptions {
        extension_dir: Some(root),
        virtual_fs: VirtualFs::empty(),
    })));
    let host = Object::new(ctx.clone()).expect("__kiana_host object");
    let resolve_resolver = Rc::clone(&resolver);
    host.set(
        "resolve",
        Function::new(
            ctx.clone(),
            move |from: String, spec: String| -> rquickjs::Result<String> {
                resolve_resolver
                    .borrow()
                    .resolve_specifier(&from, &spec, ResolutionKind::Require)
                    .map_err(|error| error.to_rquickjs_error())
            },
        )
        .expect("resolve fn"),
    )
    .expect("set resolve");
    host.set(
        "read",
        Function::new(
            ctx.clone(),
            move |path: String| -> rquickjs::Result<Option<String>> {
                Ok(read_cjs_source(Path::new(&path)))
            },
        )
        .expect("read fn"),
    )
    .expect("set read");
    ctx.globals()
        .set("__kiana_host", host)
        .expect("install host");
}

/// Test host `read`：`None`（ESM）当且仅当 [`is_cjs_module`] 不会包装这个文件——与 loader-host
/// 单元的策略一致（`ERR_REQUIRE_ESM` 的来源）。
fn read_cjs_source(path: &Path) -> Option<String> {
    let source = std::fs::read_to_string(path).ok()?;
    if path.extension().and_then(|ext| ext.to_str()) == Some("json") {
        return Some(source);
    }
    let package_type = package_type_for(path);
    is_cjs_module(&path.to_string_lossy(), &source, package_type.as_deref()).then_some(source)
}

// ---------------------------------------------------------------------------
// TS / JS / ESM 流水线
// ---------------------------------------------------------------------------

#[test]
fn loads_typescript_entry_with_relative_dependency() {
    let harness = Harness::new();
    let entry = fixture("basic/entry.ts");
    assert!(harness.default_is_function(&entry));
    assert_eq!(
        harness.call_default::<i32>(&entry).expect("factory call"),
        82
    );

    assert_eq!(harness.eval::<i32>("globalThis.__loaderDepLoads"), 1);
    let doubled: i32 = harness.export(&entry, "doubled").unwrap();
    assert_eq!(doubled, 82);
    // 重复加载同一模块名不会重新执行（QuickJS 的模块表按名字去重）。
    assert_eq!(harness.eval::<i32>("globalThis.__loaderDepLoads"), 1);
}

#[test]
fn import_meta_url_is_the_real_file_url_without_the_query() {
    let harness = Harness::new();
    let path = fixture("basic/entry.js");
    let url: String = harness.export(&format!("{path}?v=1-1"), "url").unwrap();
    assert_eq!(url, format!("file://{path}"));
    assert!(!url.contains("?v="));

    // TS 文件同样（入口名带 query 时也是干净 URL）。
    let ts = fixture("basic/entry.ts");
    let url: String = harness.export(&format!("{ts}?v=2-1"), "url").unwrap();
    assert_eq!(url, format!("file://{ts}"));
}

#[test]
fn top_level_await_settles() {
    let harness = Harness::new();
    let value: i32 = harness.default_export(&fixture("tla/entry.ts"));
    assert_eq!(value, 7);
    let kind: String = harness.export(&fixture("tla/entry.ts"), "kind").unwrap();
    assert_eq!(kind, "tla");
}

#[test]
fn cyclic_imports_link() {
    let harness = Harness::new();
    let path = fixture("cycle/a.ts");
    assert_eq!(harness.call_export::<String>(&path, "getB").unwrap(), "b");
    let a: String = harness.export(&path, "aName").unwrap();
    assert_eq!(a, "a");
}

#[test]
fn json_modules_export_the_parsed_default() {
    let harness = Harness::new();
    let path = fixture("json/entry.js");
    let default: bool = harness.default_export(&path);
    assert!(default);
    let name: String = harness.export(&path, "name").unwrap();
    assert_eq!(name, "probe");
    let n: i32 = harness.export(&path, "n").unwrap();
    assert_eq!(n, 3);
}

#[test]
fn data_url_modules_are_decoded_by_the_loader() {
    let harness = Harness::new();
    let value: i32 = harness
        .export("data:text/javascript,export default 40%2B2;", "default")
        .unwrap();
    assert_eq!(value, 42);

    let base64: i32 = harness
        .export(
            "data:text/javascript;base64,ZXhwb3J0IGRlZmF1bHQgNDI7",
            "default",
        )
        .unwrap();
    assert_eq!(base64, 42);
}

/// Node 的 ESM `data:` 加载器接受 `BASE64` 大小写与 `DATA:` 方案（review finding 6）。
#[test]
fn data_url_case_handling_matches_node() {
    let harness = Harness::new();
    let upper_flag: i32 = harness
        .export(
            "data:text/javascript;BASE64,ZXhwb3J0IGRlZmF1bHQgNDI7",
            "default",
        )
        .unwrap();
    assert_eq!(upper_flag, 42);
    let upper_scheme: i32 = harness
        .export("DATA:text/javascript,export default 7;", "default")
        .unwrap();
    assert_eq!(upper_scheme, 7);
}

/// Node 拒绝非 JS 的 `data:` MIME 与缺省的 MIME（`ERR_UNKNOWN_MODULE_FORMAT`）以及缺少
/// `with { type: "json" }` 的 JSON（`ERR_IMPORT_ATTRIBUTE_MISSING`）（review finding 6）。
#[test]
fn data_url_rejections_carry_nodes_error_codes() {
    let harness = Harness::new();

    let error = harness
        .load("data:text/plain,export default 42")
        .expect_err("text/plain data URLs are not modules");
    assert!(error.is_exception(), "unexpected error: {error:?}");
    let (message, code) = harness.catch_exception();
    assert_eq!(code.as_deref(), Some("ERR_UNKNOWN_MODULE_FORMAT"));
    assert_eq!(
        message,
        "Unknown module format: text/plain for URL data:text/plain,export default 42"
    );

    let error = harness
        .load("data:;base64,ZXhwb3J0IGRlZmF1bHQgNDI7")
        .expect_err("a missing MIME type is rejected");
    assert!(error.is_exception());
    let (message, code) = harness.catch_exception();
    assert_eq!(code.as_deref(), Some("ERR_UNKNOWN_MODULE_FORMAT"));
    assert_eq!(
        message,
        "Unknown module format: null for URL data:;base64,ZXhwb3J0IGRlZmF1bHQgNDI7"
    );

    let error = harness
        .load("data:application/json,{\"a\":1}")
        .expect_err("JSON data URLs need the type attribute");
    assert!(error.is_exception());
    let (message, code) = harness.catch_exception();
    assert_eq!(code.as_deref(), Some("ERR_IMPORT_ATTRIBUTE_MISSING"));
    assert_eq!(
        message,
        "Module \"data:application/json,{\"a\":1}\" needs an import attribute of \"type: json\""
    );

    let error = harness
        .load("data:no-comma")
        .expect_err("a data URL without a comma is invalid");
    assert!(error.is_exception());
    let (message, code) = harness.catch_exception();
    assert_eq!(code.as_deref(), Some("ERR_INVALID_URL"));
    assert_eq!(message, "Invalid URL");
}

/// `with { type: "json" }` 由 QuickJS 传给 loader，JSON data URL 只有 `default`（Node 语义）。
#[test]
fn data_url_json_with_the_type_attribute_loads() {
    let harness = Harness::new();
    harness
        .ctx
        .with(|ctx| -> rquickjs::Result<()> {
            let declared = rquickjs::Module::declare(
                ctx.clone(),
                "<json-attribute-entry>",
                "import data from \"data:application/json;base64,eyJhIjoxfQ==\" with { type: \"json\" };\n\
                 export default data.a;",
            )?;
            let (module, promise) = declared.eval()?;
            promise.finish::<()>()?;
            let value: i32 = module.namespace()?.get("default")?;
            assert_eq!(value, 1);
            Ok(())
        })
        .expect("JSON data URL with the type attribute");

    // Node's ESM JSON modules expose only `default` (no named top-level keys).
    let keys = harness
        .ctx
        .with(|ctx| -> rquickjs::Result<String> {
            let declared = rquickjs::Module::declare(
                ctx.clone(),
                "<json-attribute-namespace>",
                "import * as data from \"data:application/json;base64,eyJhIjoxfQ==\" with { type: \"json\" };\n\
                 export default Object.keys(data).sort().join(\",\");",
            )?;
            let (module, promise) = declared.eval()?;
            promise.finish::<()>()?;
            module.namespace()?.get("default")
        })
        .expect("JSON data URL namespace");
    assert_eq!(keys, "default");
}

/// jiti 把 TS/JSON 当 CJS：JSON 的顶层键同时是具名导出与 namespace 键（review finding 1）。
#[test]
fn json_modules_expose_top_level_keys_as_named_exports() {
    let harness = Harness::new();
    let path = fixture("json/named.ts");
    assert_eq!(harness.export::<String>(&path, "named").unwrap(), "probe");
    assert_eq!(harness.export::<i32>(&path, "num").unwrap(), 3);
    let default_name: String = harness.default_field(&path, "name").unwrap();
    assert_eq!(default_name, "probe");
    // Module namespace keys are sorted by the spec (`default`, `n`, `name`, `nested`).
    let keys: String = harness.export(&path, "keys").unwrap();
    assert_eq!(keys, "default,n,name,nested");
    // Arbitrary JSON keys become string export names; a top-level `"default"` key does not
    // shadow the whole-object `default` export (jiti: `namespace.default` is the object).
    assert_eq!(
        harness.export::<String>(&path, "weirdKeys").unwrap(),
        "a-b,base name,default"
    );
    assert_eq!(harness.export::<i32>(&path, "dashed").unwrap(), 2);
}

// ---------------------------------------------------------------------------
// 虚拟模块 / node shim
// ---------------------------------------------------------------------------

#[test]
fn module_can_import_virtual_keys_and_embedded_files() {
    let harness = Harness::new();
    let path = fixture("virtual/uses-virtual.ts");
    let is_object_builder: String = harness.export(&path, "isObjectBuilder").unwrap();
    assert_eq!(is_object_builder, "function");
    let models_key: String = harness.export(&path, "modelsKey").unwrap();
    assert_eq!(models_key, "kiana-ai/models");
}

#[test]
fn virtual_key_imports_share_one_namespace() {
    let harness = Harness::new();
    harness.load(&fixture("virtual/uses-virtual.ts")).unwrap();
    harness.load(&fixture("virtual/second.ts")).unwrap();
    // 两个模块导入同一个键 → 同一对象（§3.3 的身份要求）。
    assert!(harness.eval::<bool>("globalThis.__firstModels === globalThis.__secondModels"));
    let key: String = harness
        .export(&fixture("virtual/second.ts"), "key")
        .unwrap();
    assert_eq!(key, "kiana-ai/models");
}

/// §3.8 第 1 步先于第 3 步：虚拟键即使在被扩展的祖先 `node_modules` 里有同名包，也必须解析到
/// 宿主注入的虚拟模块（设计 §3.3 的对象身份要求）。TS 参考在源码运行形态下把
/// `virtualModules` 交给 jiti，但 jiti 2.7 不认这个选项，因此那条路径会加载物理包；
/// `extensions-discovery.test.ts` 的 "does not infer package ownership from ancestor manifests"
/// 锁的就是 TS 的这个行为。Rust 侧按设计选择虚拟模块优先（`getAliases()` 没有对应物）。
#[test]
fn virtual_keys_win_over_an_ancestor_node_modules_package() {
    let harness = Harness::new();
    let path = fixture("shadow/extension.ts");
    let key: String = harness.default_export(&path);
    assert_eq!(key, "kiana-ai/models");
}

#[test]
fn unregistered_virtual_key_reports_the_registration_contract() {
    let harness = Harness::new();
    // harness 只注册了 `@earendil-works/kiana-ai/models`。
    let error = harness
        .loader
        .prepare("@earendil-works/kiana-ai/oauth")
        .expect_err("unregistered virtual key must fail");
    let text = error.to_string();
    assert!(
        text.starts_with("Error loading module '@earendil-works/kiana-ai/oauth': "),
        "unexpected text: {text}"
    );
    assert!(
        text.contains("No source registered"),
        "unexpected text: {text}"
    );
}

#[test]
fn node_builtin_shims_are_served_from_the_registry() {
    let harness = Harness::new();
    let path = fixture("node/uses-node-path.ts");
    let separator: String = harness.default_export(&path);
    assert_eq!(separator, "/");
}

#[test]
fn unregistered_builtin_reports_the_node_wording() {
    let harness = Harness::new();
    let message = harness.load_error(&fixture("node/missing-node-builtin.ts"));
    assert!(
        message.contains("No such built-in module: node:not-registered"),
        "unexpected message: {message}"
    );
}

// ---------------------------------------------------------------------------
// 真 CJS（§3.5）
// ---------------------------------------------------------------------------

#[test]
fn cjs_entry_reassigning_module_exports_yields_a_function_default() {
    let harness = Harness::new();
    let path = fixture("cjs/entry.cjs");
    assert!(harness.default_is_function(&path));
    assert_eq!(harness.call_default::<String>(&path).unwrap(), "cjs-entry");
    let kind: String = harness.export(&path, "kind").unwrap();
    assert_eq!(kind, "cjs-entry");
}

#[test]
fn cjs_require_resolves_relative_files_through_the_host_bridge() {
    let harness = Harness::new();
    harness.install_host_io();
    let path = fixture("cjs/relative.cjs");
    assert_eq!(harness.default_field::<i32>(&path, "dep").unwrap(), 5);
}

#[test]
fn esm_entry_can_import_a_cjs_dependency() {
    // CJS 依赖会在 QuickJS 的模块加载回调里被声明；此时 CJS 桥可能是第一次安装
    // （`js/cjs.js` 的嵌套求值路径）。
    let harness = Harness::new();
    let path = fixture("cjs/imports-cjs.ts");
    let mode: String = harness.default_export(&path);
    assert_eq!(mode, "cjs");
    let quoted: String = harness.export(&path, "quoted").unwrap();
    assert_eq!(quoted, "hello");
}

#[test]
fn typescript_module_can_call_require_through_the_bridge() {
    // jiti 把 TS 编译成 CJS，自由的 `require` 总是可用（`type: module` 的 notify.ts 形状）。
    // Rust 侧由 transpile 注入 `var require = globalThis.__kiana_make_require(...)`。
    let harness = Harness::new();
    harness.install_host_io();
    let path = fixture("cjs/uses-require.ts");
    let value: i32 = harness.default_export(&path);
    assert_eq!(value, 5);
    let named: i32 = harness.export(&path, "value").unwrap();
    assert_eq!(named, 5);
    // §3.5 第 7 条：`require.resolve` 返回绝对路径，`require.cache` 指向 `__kiana_cjs`，
    // `require.main` 是 undefined。刻意偏差（review finding 9）：jiti 的 `moduleCache: false`
    // 让 `require.cache` 恒为空对象；共享缓存是设计 §3.5 第 3 条的要求，记录在单元报告里。
    let resolved: String = harness.export(&path, "resolved").unwrap();
    assert_eq!(resolved, fixture("cjs/dep.cjs"));
    assert!(harness.export::<bool>(&path, "mainIsUndefined").unwrap());
    assert!(harness.export::<bool>(&path, "cacheHasDep").unwrap());
}

#[test]
fn injected_filename_and_dirname_use_the_clean_real_path() {
    let harness = Harness::new();
    let path = fixture("cjs/uses-filename.ts");
    let scoped = format!("{path}?v=5-1");
    let filename: String = harness.export(&scoped, "filename").unwrap();
    assert_eq!(filename, path);
    let dirname: String = harness.export(&scoped, "dirname").unwrap();
    assert_eq!(dirname, fixtures().join("cjs").to_string_lossy());
}

#[test]
fn cjs_require_of_esm_throws_err_require_esm() {
    let harness = Harness::new();
    harness.install_host_io();
    let error = harness
        .load(&fixture("cjs/requires-esm.cjs"))
        .expect_err("require(esm) must fail");
    assert!(error.is_exception(), "unexpected error: {error:?}");
    let (message, code) = harness.catch_exception();
    assert_eq!(code.as_deref(), Some("ERR_REQUIRE_ESM"));
    assert!(
        message.starts_with("require() of ES Module"),
        "unexpected message: {message}"
    );
}

#[test]
fn cjs_bridge_install_is_idempotent() {
    let harness = Harness::new();
    harness
        .ctx
        .with(|ctx| harness.loader.install_cjs_bridge(&ctx))
        .unwrap();
    harness
        .ctx
        .with(|ctx| harness.loader.install_cjs_bridge(&ctx))
        .unwrap();
    assert!(harness.eval::<bool>("typeof globalThis.__kiana_cjs_acquire === \"function\""));
    assert!(harness.eval::<bool>("typeof globalThis.__kiana_make_require === \"function\""));
}

// ---------------------------------------------------------------------------
// 错误文案
// ---------------------------------------------------------------------------

#[test]
fn missing_module_error_matches_the_jiti_wording() {
    let harness = Harness::new();
    // TS: jiti/Node throw `Cannot find module '<path>'` + Require stack（调用方是 loader.ts）。
    let message = harness.load_error("/definitely/not/here.js");
    assert_eq!(
        message,
        format!(
            "Cannot find module '/definitely/not/here.js'\nRequire stack:\n- {}",
            kiana_extensions::loader::loader_impl::loader_require_stack_path()
        )
    );
}

#[test]
fn syntax_error_matches_the_jiti_babel_shape() {
    let harness = Harness::new();
    let path = fixture("syntax/bad.ts");
    let message = harness.load_error(&path);
    // TS: `ParseError: <Babel 文案>  \n <path>:<line>:<col>`。
    let prefix = "ParseError: ";
    assert!(
        message.starts_with(prefix),
        "unexpected message: {message:?}"
    );
    let body = message.trim_start_matches(prefix);
    assert!(
        body.contains(&format!("\n {path}:")) || body.contains(&format!(" {path}:")),
        "path missing from {message:?}"
    );
    assert!(
        body.to_lowercase().contains("expected")
            || body.to_lowercase().contains("unexpected")
            || body.to_lowercase().contains("missing")
            || body.to_lowercase().contains("invalid"),
        "unexpected diagnostic: {body}"
    );
}

// ---------------------------------------------------------------------------
// 解析 / 声明顺序
// ---------------------------------------------------------------------------

#[test]
fn extension_resolves_dependencies_from_its_own_node_modules() {
    // 移植 `extensions-discovery.test.ts` 的 "resolves dependencies from extension's own
    // node_modules"（fixtures/loader/deps 自带 node_modules/probe-pkg）。
    let harness = Harness::new();
    let path = fixture("deps/extension.ts");
    assert_eq!(harness.call_default::<String>(&path).unwrap(), "probe-pkg");
    let dependency: String = harness.export(&path, "dependency").unwrap();
    assert_eq!(dependency, "probe-pkg");
}

/// 解析器把模块名原样返回（§3.9 的声明顺序测试用）。
struct IdentityResolver;

impl Resolver for IdentityResolver {
    fn resolve<'js>(
        &mut self,
        _ctx: &Ctx<'js>,
        _base: &str,
        name: &str,
        _attributes: Option<ImportAttributes<'js>>,
    ) -> rquickjs::Result<String> {
        Ok(name.to_string())
    }
}

#[test]
fn declare_all_puts_sources_in_the_module_table_before_evaluation() {
    // §3.9：声明过的模块在 QuickJS 的 loaded_modules 里，求值期不再回调 loader。
    let rt = Runtime::new().unwrap();
    let ctx = Context::full(&rt).unwrap();
    let loader = ExtensionLoader::new(LoaderConfig::default());
    loader.register_source("kiana:virtual/predeclared", "export const value = 21;\n");
    let declared = ctx
        .with(|ctx| loader.declare_all(&ctx))
        .expect("declare_all");
    assert_eq!(declared, 1);
    // 声明不写源文本缓存以外的东西；把注册表清空后导入仍必须成功。
    assert_eq!(
        loader.remove_source("kiana:virtual/predeclared").as_deref(),
        Some("export const value = 21;\n")
    );
    rt.set_loader(IdentityResolver, loader.clone());

    ctx.with(|ctx| {
        let source = r#"
            import { value } from "kiana:virtual/predeclared";
            export default value;
        "#;
        let declared = rquickjs::Module::declare(ctx.clone(), "<declared-entry>", source)
            .expect("declare entry");
        let (module, promise) = declared.eval().expect("evaluate");
        promise.finish::<()>().expect("settle");
        let value: i32 = module.get("default").expect("default export");
        assert_eq!(value, 21);
    });
}

#[test]
fn prepare_reports_the_pipeline_kind() {
    let harness = Harness::new();
    assert_eq!(
        harness
            .loader
            .prepare(&fixture("basic/entry.ts"))
            .unwrap()
            .kind(),
        ModuleKind::TypeScript
    );
    assert_eq!(
        harness
            .loader
            .prepare(&fixture("basic/entry.js"))
            .unwrap()
            .kind(),
        ModuleKind::JavaScript
    );
    assert_eq!(
        harness
            .loader
            .prepare(&fixture("cjs/entry.cjs"))
            .unwrap()
            .kind(),
        ModuleKind::Cjs
    );
    assert_eq!(
        harness
            .loader
            .prepare(&fixture("json/data.json"))
            .unwrap()
            .kind(),
        ModuleKind::Json
    );
    assert_eq!(
        harness
            .loader
            .prepare("typebox/build/index.mjs")
            .unwrap()
            .kind(),
        ModuleKind::VirtualFile
    );
    assert_eq!(
        harness
            .loader
            .prepare("@earendil-works/kiana-ai/models")
            .unwrap()
            .kind(),
        ModuleKind::Embedded
    );
    assert_eq!(
        harness
            .loader
            .prepare("data:text/javascript,export default 1;")
            .unwrap()
            .kind(),
        ModuleKind::DataUrl
    );
}

#[test]
fn scoped_resolver_is_transparent_when_not_strict() {
    let inner = ModuleResolver::new(ResolverOptions {
        extension_dir: Some(fixtures()),
        virtual_fs: VirtualFs::empty(),
    });
    let mut resolver = ScopedResolver::new(inner, false);
    let rt = Runtime::new().unwrap();
    let ctx = Context::full(&rt).unwrap();
    ctx.with(|ctx| {
        let resolved = resolver
            .resolve(
                &ctx,
                &format!("{}?v=1-1", fixture("basic/entry.ts")),
                "./dep.ts",
                None,
            )
            .unwrap();
        assert_eq!(resolved, fixture("basic/dep.ts"));
    });
}

#[test]
fn scoped_resolver_applies_the_entry_query_to_file_dependencies() {
    let inner = ModuleResolver::new(ResolverOptions {
        extension_dir: Some(fixtures()),
        virtual_fs: VirtualFs::empty(),
    });
    let mut resolver = ScopedResolver::new(inner, true);
    let rt = Runtime::new().unwrap();
    let ctx = Context::full(&rt).unwrap();
    ctx.with(|ctx| {
        let entry = format!("{}?v=3-7", fixture("basic/entry.ts"));
        let resolved = resolver.resolve(&ctx, &entry, "./dep.ts", None).unwrap();
        assert_eq!(resolved, format!("{}?v=3-7", fixture("basic/dep.ts")));

        // 虚拟键与 node shim 不扩散（全代共享，§3.3）。
        let virtual_name = resolver
            .resolve(&ctx, &entry, "@earendil-works/kiana-ai/models", None)
            .unwrap();
        assert_eq!(virtual_name, "@earendil-works/kiana-ai/models");
        let builtin = resolver.resolve(&ctx, &entry, "node:path", None).unwrap();
        assert_eq!(builtin, "node:path");

        // 不带 query 的 base（干净名字）不扩散。
        let clean = resolver
            .resolve(&ctx, &fixture("basic/entry.ts"), "./dep.ts", None)
            .unwrap();
        assert_eq!(clean, fixture("basic/dep.ts"));

        // 已经在 query 里的 base 继续把同一 query 传给下一层（整棵图同一个 query）。
        let nested = resolver
            .resolve(
                &ctx,
                &format!("{}?v=3-7", fixture("basic/dep.ts")),
                "./entry.ts",
                None,
            )
            .unwrap();
        assert_eq!(nested, format!("{}?v=3-7", fixture("basic/entry.ts")));
    });
}

#[test]
fn install_on_wires_the_loader_into_an_engine() {
    // 门面接线的最小形态：`declare_all`（§9.2 第 5 步）+ `install_on` + 入口声明。
    use kiana_extensions::engine::JsEngine;

    let engine = JsEngine::new().expect("engine");
    let loader = ExtensionLoader::new(LoaderConfig::default());
    loader.register_source("kiana:virtual/hello", "export const value = 5;\n");
    engine
        .with_js(|ctx| loader.declare_all(&ctx))
        .expect("declare_all");

    let resolver = ModuleResolver::new(ResolverOptions {
        extension_dir: Some(fixtures()),
        virtual_fs: VirtualFs::empty(),
    });
    loader.install_on(&engine, resolver);

    engine
        .with_js(|ctx| -> rquickjs::Result<i32> {
            let module = loader.load_module(&ctx, &fixture("basic/entry.ts"))?;
            let (module, promise) = module.eval()?;
            promise.finish::<()>()?;
            module.get("doubled")
        })
        .expect("load through the engine-installed resolver/loader");
    assert_eq!(
        engine
            .eval_script::<i32>("globalThis.__loaderDepLoads", "<test>")
            .unwrap(),
        1
    );
}

// ---------------------------------------------------------------------------
// import.meta（jiti tryNative:false 的形态）
// ---------------------------------------------------------------------------

#[test]
fn import_meta_matches_jiti_for_transformed_typescript() {
    let harness = Harness::new();
    let path = fixture("meta/entry.ts");
    // jiti's Babel transform gives a transformed TS module the data:-URL import.meta object
    // `{main, resolve, url}`; dirname/filename are statically rewritten and absent from the object.
    assert_eq!(
        harness.export::<String>(&path, "keys").unwrap(),
        "main,resolve,url"
    );
    assert_eq!(
        harness.export::<String>(&path, "mainType").unwrap(),
        "boolean"
    );
    assert!(!harness.export::<bool>(&path, "main").unwrap());
    assert_eq!(
        harness.export::<String>(&path, "resolveType").unwrap(),
        "function"
    );
    assert_eq!(
        harness.export::<Option<String>>(&path, "dirname").unwrap(),
        None
    );
    assert_eq!(
        harness.export::<Option<String>>(&path, "filename").unwrap(),
        None
    );
    assert_eq!(
        harness.export::<String>(&path, "url").unwrap(),
        format!("file://{path}")
    );
}

#[test]
fn import_meta_has_dirname_and_filename_for_native_esm() {
    // Node 原生求值（`.mjs`、`"type": "module"` 包的 `.js`）有完整的五个键，顺序同 Node。
    for name in ["meta/entry.mjs", "esmpkg/meta.js"] {
        let harness = Harness::new();
        let path = fixture(name);
        assert_eq!(
            harness.export::<String>(&path, "keys").unwrap(),
            "dirname,filename,main,resolve,url"
        );
        assert_eq!(
            harness.export::<String>(&path, "dirname").unwrap(),
            Path::new(&path).parent().unwrap().to_string_lossy()
        );
        assert_eq!(harness.export::<String>(&path, "filename").unwrap(), path);
        assert_eq!(
            harness.export::<String>(&path, "url").unwrap(),
            format!("file://{path}")
        );
    }
}

/// jiti 转换分支（普通 `.js`）：裸 `import.meta` 只有 `{main, resolve, url}`，但静态
/// `import.meta.dirname`/`.filename` 被 Babel 重写成真实路径（review finding 3）。Rust 用
/// 不可枚举属性同时满足两者。
#[test]
fn import_meta_for_transformed_js_hides_dirname_and_filename() {
    let harness = Harness::new();
    let path = fixture("meta/entry.js");
    assert_eq!(
        harness.export::<String>(&path, "keys").unwrap(),
        "main,resolve,url"
    );
    assert_eq!(
        harness.export::<String>(&path, "dirname").unwrap(),
        fixtures().join("meta").to_string_lossy()
    );
    assert_eq!(harness.export::<String>(&path, "filename").unwrap(), path);
    assert_eq!(
        harness.export::<String>(&path, "url").unwrap(),
        format!("file://{path}")
    );
    // The hidden properties are readable but not enumerable (Object.keys above).
    assert!(!harness.export::<bool>(&path, "dirnameEnumerable").unwrap());
    assert!(!harness.export::<bool>(&path, "filenameEnumerable").unwrap());
}

#[test]
fn import_meta_resolve_uses_the_host_resolver() {
    let harness = Harness::new();
    harness.install_host_io();
    let path = fixture("meta/resolve.ts");
    assert_eq!(
        harness.export::<String>(&path, "resolved").unwrap(),
        format!("file://{}", fixture("meta/dep.ts"))
    );
}

#[test]
fn import_meta_resolve_without_the_host_is_a_clear_error() {
    let harness = Harness::new();
    let path = fixture("meta/resolve.ts");
    let error = harness
        .load(&path)
        .expect_err("import.meta.resolve needs __kiana_host.resolve");
    assert!(error.is_exception(), "unexpected error: {error:?}");
    let (message, _code) = harness.catch_exception();
    assert!(message.contains("__kiana_host.resolve"), "{message}");
}

// ---------------------------------------------------------------------------
// `.js`/`.mjs` 的 jiti CJS 绑定（review finding 7）
// ---------------------------------------------------------------------------

#[test]
fn esm_javascript_with_free_require_gets_the_cjs_bindings() {
    let harness = Harness::new();
    harness.install_host_io();
    let path = fixture("js/require-in-esm.js");
    assert_eq!(harness.default_export::<i32>(&path), 14);
    // 刻意偏差（review finding 9）：jiti 在 `moduleCache: false` 下每次 `require()` 返回新的
    // interop proxy（实测 `dep === require("./dep.cjs")` 为 false），`require.cache` 是空对象；
    // 设计 §3.5 第 3/7 条要求共享 `__kiana_cjs`，所以这里为 true。记录在单元报告里。
    assert!(harness.export::<bool>(&path, "sameInstance").unwrap());

    // jiti's `tryNative: false` evaluator defines `require` for every `.js` it transforms.
    let typeof_path = fixture("js/typeof-require.js");
    assert_eq!(harness.default_export::<String>(&typeof_path), "function");

    // All five jiti wrapper bindings are available when the ESM `.js` source uses them.
    let bindings = fixture("js/cjs-bindings.js");
    assert_eq!(
        harness.default_export::<String>(&bindings),
        fixtures().join("js").to_string_lossy()
    );
    assert_eq!(
        harness.export::<String>(&bindings, "filename").unwrap(),
        bindings
    );
    assert_eq!(
        harness.export::<String>(&bindings, "moduleType").unwrap(),
        "object"
    );
    assert_eq!(
        harness.export::<String>(&bindings, "exportsType").unwrap(),
        "object"
    );
    assert_eq!(
        harness.export::<String>(&bindings, "requireType").unwrap(),
        "function"
    );
}

// ---------------------------------------------------------------------------
// 宿主原语缺失 / 错误文案（review findings 8–10）
// ---------------------------------------------------------------------------

#[test]
fn missing_host_primitives_report_a_clear_error() {
    let harness = Harness::new();
    let error = harness
        .load(&fixture("cjs/relative.cjs"))
        .expect_err("require without __kiana_host must fail");
    assert!(error.is_exception(), "unexpected error: {error:?}");
    let (message, code) = harness.catch_exception();
    assert_eq!(code.as_deref(), Some("ERR_PI_HOST_MISSING"));
    assert!(message.contains("__kiana_host.resolve"), "{message}");
    assert!(!message.contains("Cannot find module"), "{message}");
}

#[test]
fn missing_host_read_reports_a_clear_error() {
    let harness = Harness::new();
    harness.install_host_io();
    harness
        .ctx
        .with(|ctx| ctx.eval::<(), _>("__kiana_host.read = undefined;"))
        .expect("remove __kiana_host.read");
    let error = harness
        .load(&fixture("cjs/relative.cjs"))
        .expect_err("require without __kiana_host.read must fail");
    assert!(error.is_exception(), "unexpected error: {error:?}");
    let (message, code) = harness.catch_exception();
    assert_eq!(code.as_deref(), Some("ERR_PI_HOST_MISSING"));
    assert!(message.contains("__kiana_host.read"), "{message}");
    assert!(!message.contains("ERR_REQUIRE_ESM"), "{message}");
}

#[test]
fn dependency_load_errors_are_not_double_wrapped() {
    let harness = Harness::new();
    // TS: 依赖缺失保持 jiti 的一级文案，Require stack 指向导入方文件。
    let importer = fixture("errors/missing-dep.ts");
    assert_eq!(
        harness.load_error(&fixture("errors/missing-dep.ts")),
        format!("Cannot find module './definitely-missing.ts'\nRequire stack:\n- {importer}")
    );

    let dep = fixture("errors/broken-dep.ts");
    let message = harness.load_error(&fixture("errors/broken-dep-entry.ts"));
    assert!(
        message.starts_with("ParseError: "),
        "unexpected message: {message}"
    );
    assert!(
        message.contains(&format!(" {dep}:")),
        "dependency path missing from {message}: {dep}"
    );
    assert_eq!(
        message.matches("ParseError: ").count(),
        1,
        "the parent module must not add a second prefix: {message}"
    );
}

#[test]
fn cjs_syntax_errors_name_the_file_and_position() {
    let harness = Harness::new();
    let path = fixture("cjs/broken.cjs");
    let error = harness.load(&path).expect_err("broken CJS must fail");
    assert!(
        !error.is_exception(),
        "parse failures are reported as load errors, got {error:?}"
    );
    let text = error.to_string();
    assert!(
        text.starts_with(&format!("Error loading module '{path}': ParseError: ")),
        "unexpected text: {text}"
    );
    assert!(text.contains(&format!(" at {path}:2:")), "{text}");
}

// ---------------------------------------------------------------------------
// require.resolve.paths（review finding 11）
// ---------------------------------------------------------------------------

/// Node's `Module._nodeModulePaths(dir)`.
fn node_module_paths(dir: &Path) -> Vec<String> {
    dir.ancestors()
        .filter(|ancestor| {
            ancestor.file_name().and_then(|name| name.to_str()) != Some("node_modules")
        })
        .map(|ancestor| ancestor.join("node_modules").to_string_lossy().into_owned())
        .collect()
}

#[test]
fn require_resolve_paths_match_node() {
    let harness = Harness::new();
    harness.install_host_io();
    let path = fixture("cjs/resolve-paths.ts");

    let relative: Vec<String> = harness.export(&path, "relative").unwrap();
    assert_eq!(
        relative,
        vec![fixtures().join("cjs").to_string_lossy().into_owned()]
    );
    let builtin: Option<Vec<String>> = harness.export(&path, "builtin").unwrap();
    assert_eq!(builtin, None);

    let mut expected = node_module_paths(&fixtures().join("cjs"));
    expected.extend(kiana_extensions::loader::loader_impl::node_global_paths());
    let bare: Vec<String> = harness.export(&path, "bare").unwrap();
    assert_eq!(bare, expected);
    // Node 26 treats absolute requests through the parent's lookup paths as well.
    let absolute: Vec<String> = harness.export(&path, "absolute").unwrap();
    assert_eq!(absolute, expected);
    // `_resolveLookupPaths` treats anything starting with `..` as relative but `.foo` as bare.
    let dotdot: Vec<String> = harness.export(&path, "dotdot").unwrap();
    assert_eq!(dotdot, relative);
    let dotfile: Vec<String> = harness.export(&path, "dotfile").unwrap();
    assert_eq!(dotfile, expected);
}

// ---------------------------------------------------------------------------
// 跨 Runtime 的重绑定（review finding 3）
// ---------------------------------------------------------------------------

#[test]
fn loader_rebinds_per_context_state_on_a_new_runtime() {
    let loader = ExtensionLoader::new(LoaderConfig::default());
    loader.register_source("kiana:virtual/predeclared", "export const value = 21;\n");

    // 第 1 代：声明源 + 求值 CJS 桥（句柄与 `Runtime::set_loader` 共享状态）。
    let rt1 = Runtime::new().unwrap();
    let ctx1 = Context::full(&rt1).unwrap();
    assert_eq!(ctx1.with(|ctx| loader.declare_all(&ctx)).unwrap(), 1);
    ctx1.with(|ctx| install_host_io(&ctx));
    let resolver1 = ModuleResolver::new(ResolverOptions {
        extension_dir: Some(fixtures()),
        virtual_fs: VirtualFs::empty(),
    });
    rt1.set_loader(loader.resolver(resolver1), loader.clone());
    ctx1.with(|ctx| -> rquickjs::Result<()> {
        let module = loader.load_module(&ctx, &fixture("cjs/relative.cjs"))?;
        let (_module, promise) = module.eval()?;
        promise.finish::<()>()?;
        assert!(ctx.eval::<bool, _>("typeof __kiana_cjs_acquire === 'function'")?);
        Ok(())
    })
    .expect("first generation");

    // 第 2 代（`/reload` 的销毁→重建）：同一个句柄必须在新 Context 里重新声明/装桥。
    let rt2 = Runtime::new().unwrap();
    let ctx2 = Context::full(&rt2).unwrap();
    assert_eq!(
        ctx2.with(|ctx| loader.declare_all(&ctx)).unwrap(),
        1,
        "declare_all must not skip a fresh Context"
    );
    ctx2.with(|ctx| install_host_io(&ctx));
    let resolver2 = ModuleResolver::new(ResolverOptions {
        extension_dir: Some(fixtures()),
        virtual_fs: VirtualFs::empty(),
    });
    rt2.set_loader(loader.resolver(resolver2), loader.clone());
    ctx2.with(|ctx| -> rquickjs::Result<()> {
        // The first CJS load installs the bridge lazily; before the fix the stale
        // per-handle flag skipped it and this threw `__kiana_cjs_acquire is not a function`.
        let module = loader.load_module(&ctx, &fixture("cjs/relative.cjs"))?;
        let (module, promise) = module.eval()?;
        promise.finish::<()>()?;
        assert!(
            ctx.eval::<bool, _>("typeof __kiana_cjs_acquire === 'function'")?,
            "the CJS bridge must be re-evaluated in the new Context"
        );
        let default: Object = module.namespace()?.get("default")?;
        assert_eq!(default.get::<_, i32>("dep")?, 5);
        Ok(())
    })
    .expect("second generation");
}

// ---------------------------------------------------------------------------
// 混写 ESM + module.exports（review finding 1）
// ---------------------------------------------------------------------------

/// jiti 用 Babel 把 ESM 语法改写成对同一个 CJS 记录的写入；`module.exports` 重新赋值后默认
/// 导入就是新值，`exports.*` 是具名导出。
#[test]
fn mixed_esm_and_module_exports_follow_jiti_cjs_semantics() {
    let harness = Harness::new();

    // `module.exports = <function>` 覆盖 ESM 导出：入口默认导出就是该函数。
    let a = fixture("mixed/a.ts");
    assert!(harness.default_is_function(&a));
    assert_eq!(
        harness.call_default::<String>(&a).unwrap(),
        "from-module-exports"
    );
    // 重新赋值丢弃之前的 ESM 名字（jiti 的 CJS 语义：读到 undefined）。
    assert_eq!(harness.export::<Option<i32>>(&a, "x").unwrap(), None);

    // `module.exports.foo = …` 保留记录：默认导出是 {x, foo}，两者都是具名导出。
    let b = fixture("mixed/b.ts");
    assert_eq!(harness.default_field::<i32>(&b, "x").unwrap(), 1);
    assert_eq!(harness.default_field::<String>(&b, "foo").unwrap(), "bar");
    assert_eq!(harness.export::<i32>(&b, "x").unwrap(), 1);
    assert_eq!(harness.export::<String>(&b, "foo").unwrap(), "bar");

    // ESM `export default` 写入记录的 `default` 键：属性写入不会覆盖它。
    let c = fixture("mixed/c.ts");
    assert_eq!(harness.default_export::<String>(&c), "esm-default");
    assert_eq!(harness.export::<i32>(&c, "x").unwrap(), 1);
    assert_eq!(harness.export::<String>(&c, "foo").unwrap(), "bar");
}

#[test]
fn mixed_modules_interop_with_static_imports() {
    let harness = Harness::new();
    let path = fixture("mixed/consumer.ts");
    // 默认导入重新赋值后的 module.exports。
    assert_eq!(
        harness.export::<String>(&path, "aType").unwrap(),
        "function"
    );
    assert_eq!(
        harness.export::<String>(&path, "aValue").unwrap(),
        "from-module-exports"
    );
    // 被重新赋值丢弃的名字仍可在链接期导入，运行期是 undefined（jiti 同样不抛链接错误）。
    assert_eq!(
        harness.export::<String>(&path, "aNamed").unwrap(),
        "undefined"
    );
    assert_eq!(harness.export::<i32>(&path, "bxValue").unwrap(), 1);
    assert_eq!(harness.export::<String>(&path, "fooValue").unwrap(), "bar");
    assert_eq!(
        harness.export::<String>(&path, "cDefault").unwrap(),
        "esm-default"
    );
    assert_eq!(harness.export::<i32>(&path, "cxValue").unwrap(), 1);
    assert_eq!(harness.export::<String>(&path, "cfooValue").unwrap(), "bar");
}

#[test]
fn mixed_module_named_and_reexports() {
    let harness = Harness::new();
    let named = fixture("mixed/named.ts");
    assert_eq!(harness.export::<i32>(&named, "alpha").unwrap(), 1);
    assert_eq!(harness.export::<i32>(&named, "gamma").unwrap(), 2);
    assert_eq!(harness.export::<i32>(&named, "alpha-alias").unwrap(), 1);
    assert_eq!(harness.export::<String>(&named, "extra").unwrap(), "e");

    let reexports = fixture("mixed/reexports.ts");
    assert_eq!(harness.export::<i32>(&reexports, "fromDep").unwrap(), 5);
    assert_eq!(harness.export::<String>(&reexports, "mark").unwrap(), "m");

    // `export enum` 被 oxc 降级成 `export let E = …`，改写后仍是具名导出。
    let consumer = fixture("mixed/enum-consumer.ts");
    assert_eq!(harness.export::<i32>(&consumer, "a").unwrap(), 1);
    assert_eq!(
        harness.export::<String>(&consumer, "kindValue").unwrap(),
        "enum"
    );
    assert_eq!(
        harness.export::<String>(&consumer, "tagValue").unwrap(),
        "t"
    );
    assert_eq!(harness.export::<i32>(&consumer, "default").unwrap(), 1);
}

/// 局部名字 `exports`（函数参数）不是 CJS 导出记录：没有生成的 `module` 记录时不得改写。
#[test]
fn local_exports_binding_is_not_treated_as_cjs() {
    let harness = Harness::new();
    let path = fixture("mixed/local-exports.ts");
    let prepared = harness.loader.prepare(&path).unwrap();
    assert!(
        !prepared.source().contains("__kiana_cjs_final"),
        "pure ESM module was rewritten: {}",
        prepared.source()
    );
    assert!(prepared.source().contains("export default"));
    assert_eq!(harness.default_export::<String>(&path), "local");
}

/// `export default <function>` 随后被 `module.exports = …` 覆盖：jiti 的默认导入是后者。
#[test]
fn mixed_module_reassigned_module_exports_wins_over_esm_default() {
    let harness = Harness::new();
    let path = fixture("mixed/default-fn.ts");
    assert_eq!(harness.call_default::<String>(&path).unwrap(), "replaced");

    // 普通 `.js` 同样经 jiti 的 Babel 转换（r2/mixed/c.js 实测 default 是函数）。
    let js = fixture("mixed/js-entry.js");
    assert_eq!(harness.call_default::<String>(&js).unwrap(), "js-mixed");
    assert_eq!(harness.export::<Option<i32>>(&js, "x").unwrap(), None);

    // 原生 ESM 里的 `module.exports` 在求值期抛 ReferenceError → jiti 回退到 CJS 转换。
    for (name, expected) in [
        ("esmpkg/mixed.js", "esmpkg-mixed"),
        ("esmpkg/mixed.mjs", "mjs-mixed"),
    ] {
        let path = fixture(name);
        assert_eq!(harness.call_default::<String>(&path).unwrap(), expected);
        assert_eq!(harness.export::<Option<i32>>(&path, "x").unwrap(), None);
    }

    // Babel 的 `__esModule` 标记让 interop 解开一层嵌套 default（jiti 实测返回 inner）。
    let double_default = fixture("mixed/double-default.ts");
    assert_eq!(
        harness.call_default::<String>(&double_default).unwrap(),
        "inner"
    );
    assert_eq!(
        harness.export::<String>(&double_default, "foo").unwrap(),
        "bar"
    );
}

/// 没有 ESM `export` 语法、但写了 `module.exports` 且使用 `import.meta` 的 TS 模块：jiti 把它当
/// ESM 转换（`import.meta` 让 jiti 走 ESM 语法检测），默认导出仍是 module.exports。
#[test]
fn mixed_module_without_export_statements_still_gets_a_default() {
    let harness = Harness::new();
    let path = fixture("mixed/meta-only.ts");
    let value: String = harness.call_default(&path).unwrap();
    assert_eq!(value, format!("file://{path}"));
}

// ---------------------------------------------------------------------------
// 入口诊断不带内部 `?v=` 缓存键（review finding 2）
// ---------------------------------------------------------------------------

#[test]
fn entry_diagnostics_do_not_leak_the_version_query() {
    let harness = Harness::new();
    let path = fixture("syntax/bad.ts");
    let scoped = format!("{path}?v=1-3");
    let text = harness.load_error(&scoped);
    assert!(text.starts_with("ParseError: "), "unexpected text: {text}");
    assert!(
        text.contains(&format!(" {path}:")),
        "real path missing from {text}"
    );
    assert!(!text.contains("?v="), "cache buster leaked: {text}");

    // 读文件失败同样只写真实路径（jiti 的 Cannot find module 文案）。
    let missing = fixture("missing/entry.ts");
    let scoped = format!("{missing}?v=2-1");
    let text = harness.load_error(&scoped);
    assert_eq!(
        text,
        format!(
            "Cannot find module '{missing}'\nRequire stack:\n- {}",
            kiana_extensions::loader::loader_impl::loader_require_stack_path()
        )
    );
    assert!(!text.contains("?v="), "cache buster leaked: {text}");

    // `Module::declare` 阶段的语法错误（未经过流水线预解析的 `.js`）也一样。
    let js = fixture("syntax/bad-entry.js");
    let scoped = format!("{js}?v=4-2");
    let error = harness
        .load(&scoped)
        .expect_err("invalid JavaScript must fail");
    let message = if error.is_exception() {
        harness.catch_exception().0
    } else {
        error.to_string()
    };
    assert!(
        message.contains(&format!("'{js}'")),
        "real path missing from {message}"
    );
    assert!(!message.contains("?v="), "cache buster leaked: {message}");
}

// ---------------------------------------------------------------------------
// 原生 ESM 与 jiti 的 CJS 回退（review finding 4）
// ---------------------------------------------------------------------------

#[test]
fn native_esm_modules_do_not_get_cjs_bindings() {
    for name in ["esmpkg/lazy.js", "esmpkg/lazy.mjs"] {
        let harness = Harness::new();
        let path = fixture(name);
        // 延迟引用不会让 Node 原生 import 失败：jiti 不回退到 CJS 转换。
        assert_eq!(
            harness.export::<String>(&path, "hasRequire").unwrap(),
            "undefined"
        );
        assert_eq!(
            harness.export::<String>(&path, "filenameType").unwrap(),
            "undefined"
        );
        assert_eq!(
            harness.export::<String>(&path, "keys").unwrap(),
            "dirname,filename,main,resolve,url"
        );
        let error = harness
            .call_export::<i32>(&path, "getDep")
            .expect_err("require must stay undefined");
        assert!(error.is_exception(), "unexpected error: {error:?}");
        let (message, _) = harness.catch_exception();
        assert!(message.contains("require is not defined"), "{message}");
    }
}

/// 求值期的 `require(...)` 会让原生 import 抛 `ReferenceError`，jiti 回退到 CJS 转换：包装器
/// 参数可用，`import.meta` 变成转换分支的三键对象。
#[test]
fn native_esm_falls_back_to_cjs_for_evaluation_time_require() {
    let harness = Harness::new();
    harness.install_host_io();
    let path = fixture("esmpkg/top-require.js");
    assert_eq!(harness.default_export::<i32>(&path), 18);
    assert_eq!(
        harness.export::<String>(&path, "keys").unwrap(),
        "main,resolve,url"
    );
}

/// 非 `"type": "module"` 包里的 `.js` 永远经过 jiti 转换：即使 `require` 只在延迟函数里出现，
/// 包装器参数也可用。
#[test]
fn transformed_js_always_gets_cjs_bindings() {
    let harness = Harness::new();
    harness.install_host_io();
    let path = fixture("js/lazy-require.js");
    assert_eq!(harness.default_export::<String>(&path), "function");
    assert_eq!(harness.call_export::<i32>(&path, "getDep").unwrap(), 7);
}

// ---------------------------------------------------------------------------
// Node 的全局模块目录（review finding 5）
// ---------------------------------------------------------------------------

#[cfg(not(windows))]
#[test]
fn node_global_paths_follow_node_init_paths() {
    use kiana_extensions::loader::loader_impl::node_global_paths_from;
    let exec = Path::new("/opt/node/bin/node");
    let home = Path::new("/home/probe");
    assert_eq!(
        node_global_paths_from(Some("/one:rel::/two"), Some(home), Some(exec)),
        vec![
            "/one",
            // Node 26 不 resolve NODE_PATH 项（`NODE_PATH=rel` 保留 `rel`）。
            "rel",
            "/two",
            "/home/probe/.node_modules",
            "/home/probe/.node_libraries",
            "/opt/node/lib/node",
        ]
    );
    // 空 HOME 与未设置等价（Node 的 `if (homeDir)`）。
    assert_eq!(
        node_global_paths_from(None, Some(Path::new("")), Some(exec)),
        vec!["/opt/node/lib/node"]
    );
    assert_eq!(
        node_global_paths_from(None, None, Some(exec)),
        vec!["/opt/node/lib/node"]
    );
    assert!(node_global_paths_from(None, None, None).is_empty());
}

// ---------------------------------------------------------------------------
// data: URL 的 Node 边界（review finding 6）
// ---------------------------------------------------------------------------

#[test]
fn data_url_edge_cases_follow_node() {
    let harness = Harness::new();

    // fragment 不参与解码（dataURLProcessor 用 exclude-fragment 序列化）。
    let value: i32 = harness
        .export("data:text/javascript,export default 1#frag", "default")
        .unwrap();
    assert_eq!(value, 1);

    // base64 标记允许空格；payload 忽略 ASCII 空白、容忍省略填充、先做百分号解码。
    for (url, expected) in [
        ("data:text/javascript; base64,ZXhwb3J0IGRlZmF1bHQgNDI7", 42),
        ("data:text/javascript;base64 ,ZXhwb3J0IGRlZmF1bHQgNDI7", 42),
        ("data:text/javascript;base64,ZXhw b3J0IGRlZmF1bHQgNDI7", 42),
        ("data:text/javascript;base64,ZXhw\tb3J0IGRlZmF1bHQgNDI7", 42),
        ("data:text/javascript;base64,ZXhwb3J0IGRlZmF1bHQgNzs", 7),
        ("data:text/javascript;base64,%5AXhwb3J0IGRlZmF1bHQgNDI7", 42),
    ] {
        let value: i32 = harness.export(url, "default").unwrap();
        assert_eq!(value, expected, "{url}");
    }

    // MIME 允许首尾空白、参数与 `;charset=utf-8`（大小写不敏感）。
    for (url, expected) in [
        ("data: text/javascript,export default 43", 43),
        ("data:text/javascript ;base64,ZXhwb3J0IGRlZmF1bHQgNDI7", 42),
        ("data:text/javascript;charset=utf-8,export default 44", 44),
        ("data:text/javascript ; charset=utf8 ,export default 45", 45),
        ("data:TEXT/JAVASCRIPT,export default 46", 46),
    ] {
        let value: i32 = harness.export(url, "default").unwrap();
        assert_eq!(value, expected, "{url}");
    }

    // 没有 `/` 的 MIME 在 Node 里是 `null`；捕获的 MIME 原样出现在消息里（含尾部空格）。
    for (url, expected) in [
        (
            "data:foo,export default 1",
            "Unknown module format: null for URL data:foo,export default 1",
        ),
        (
            "data:text/plain ,1",
            "Unknown module format: text/plain  for URL data:text/plain ,1",
        ),
        (
            "DATA:text/plain,1",
            "Unknown module format: text/plain for URL data:text/plain,1",
        ),
        (
            "data:foo/bar ,1",
            "Unknown module format: foo/bar  for URL data:foo/bar ,1",
        ),
    ] {
        let error = harness.load(url).expect_err("non-JS MIME must be rejected");
        assert!(error.is_exception(), "unexpected error: {error:?}");
        let (message, code) = harness.catch_exception();
        assert_eq!(code.as_deref(), Some("ERR_UNKNOWN_MODULE_FORMAT"));
        assert_eq!(message, expected);
    }

    // import.meta.url 是 URL 序列化后的值：scheme 小写。
    let url: String = harness
        .export(
            "DATA:text/javascript,export default import.meta.url",
            "default",
        )
        .unwrap();
    assert_eq!(url, "data:text/javascript,export default import.meta.url");
}

// ---------------------------------------------------------------------------
// jiti 的 TSX 变体扩展名（review finding 7）
// ---------------------------------------------------------------------------

#[test]
fn tsx_variant_extensions_are_transpiled() {
    let harness = Harness::new();
    let mtsx = fixture("exts/entry.mtsx");
    assert_eq!(harness.call_default::<i32>(&mtsx).unwrap(), 42);
    assert_eq!(harness.export::<i32>(&mtsx, "x").unwrap(), 1);

    let ctsx = fixture("exts/entry.ctsx");
    assert_eq!(harness.default_export::<i32>(&ctsx), 1);
    assert_eq!(harness.export::<i32>(&ctsx, "x").unwrap(), 1);
}

#[test]
fn loader_impl_is_a_rquickjs_loader() {
    // 编译期契约：`ExtensionLoader` 可被 `Runtime::set_loader` 直接接收。
    fn assert_loader<L: Loader + 'static>(_loader: L) {}
    assert_loader(ExtensionLoader::default());
}
