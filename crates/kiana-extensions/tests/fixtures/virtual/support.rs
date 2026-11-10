// Shared harness for the virtual-module integration tests.
//
// Lives under `fixtures/virtual/` on purpose: `tests/` only builds top-level `.rs` files, and the
// owned test paths for this unit are the three test targets plus this fixtures directory. The
// module is included with `#[path = "fixtures/virtual/support.rs"] mod virtual_support;`.
#![allow(dead_code)]

use std::collections::HashMap;

use kiana_extensions::engine::{self, JsEngine};
use kiana_extensions::loader::resolver::{ModuleResolver, ResolverOptions};
use kiana_extensions::virtual_modules;
use rquickjs::loader::{ImportAttributes, Loader, Resolver};
use rquickjs::{Ctx, FromJs, Module, Object};

/// 最小 TextEncoder/TextDecoder（node-globals 单元在真实运行时提供；typebox 的 hashing 需要）。
const TEXT_CODEC_STUB: &str = r##"
globalThis.TextEncoder = class TextEncoder {
    get encoding() { return "utf-8"; }
    encode(input = "") {
        const bytes = [];
        for (const char of String(input)) {
            const code = char.codePointAt(0);
            if (code < 0x80) bytes.push(code);
            else if (code < 0x800) bytes.push(0xc0 | (code >> 6), 0x80 | (code & 0x3f));
            else if (code < 0x10000) bytes.push(0xe0 | (code >> 12), 0x80 | ((code >> 6) & 0x3f), 0x80 | (code & 0x3f));
            else bytes.push(0xf0 | (code >> 18), 0x80 | ((code >> 12) & 0x3f), 0x80 | ((code >> 6) & 0x3f), 0x80 | (code & 0x3f));
        }
        return new Uint8Array(bytes);
    }
};
globalThis.TextDecoder = class TextDecoder {
    decode(bytes) { return Array.from(bytes, (byte) => String.fromCharCode(byte)).join(""); }
};
// typebox 的 schema/compile 无条件构造 `new URL(DefaultBase)`（以及解析 $id）；真实运行时由
// node-globals 单元提供 WHATWG URL。这里给出只覆盖测试用法的词法实现。
globalThis.URL = class URL {
    constructor(input, base) {
        const text = String(input);
        if (base === undefined) {
            this._href = text;
        } else {
            const baseText = base instanceof URL ? base.href : String(base);
            if (/^[a-z][a-z0-9+.-]*:/i.test(text)) this._href = text;
            else if (text.startsWith("#")) this._href = baseText.split("#")[0] + text;
            else if (text.startsWith("//")) this._href = baseText.split("//")[0] + text;
            else if (text.startsWith("/")) {
                const origin = baseText.match(/^[a-z][a-z0-9+.-]*:\/\/[^/]+/i);
                this._href = (origin ? origin[0] : "") + text;
            } else if (text === "") this._href = baseText;
            else this._href = baseText.replace(/[^/]*$/, "") + text;
        }
    }
    get href() { return this._href; }
    get hash() { const index = this._href.indexOf("#"); return index < 0 ? "" : this._href.slice(index); }
    get hostname() {
        const match = this._href.match(/^[a-z][a-z0-9+.-]*:\/\/([^/:?#]+)/i);
        return match ? match[1].toLowerCase() : "";
    }
    toString() { return this._href; }
    static canParse(value, base) {
        try { new URL(value, base); return true; } catch { return false; }
    }
};
"##;

/// 只服务虚拟模块的 resolver：公开键 → 规范模块名，虚拟包内相对导入按 `VirtualFs` 解析。
pub struct VirtualResolver;

impl Resolver for VirtualResolver {
    fn resolve<'js>(
        &mut self,
        _ctx: &Ctx<'js>,
        base: &str,
        name: &str,
        _attributes: Option<ImportAttributes<'js>>,
    ) -> rquickjs::Result<String> {
        let fs = virtual_modules::virtual_fs();
        if let Some(canonical) = fs.canonical_name(name) {
            return Ok(canonical.to_string());
        }
        if (name.starts_with("./") || name.starts_with("../"))
            && fs.canonical_name(base).is_some()
            && let Some(target) = fs.resolve_relative(base, name)
        {
            return Ok(target.to_string());
        }
        Ok(name.to_string())
    }
}

/// 从 `engine.register_module_source` 快照里取源；其它模块（扩展文件）不加载。
pub struct SourceLoader {
    sources: HashMap<String, &'static str>,
}

impl Loader for SourceLoader {
    fn load<'js>(
        &mut self,
        ctx: &Ctx<'js>,
        name: &str,
        _attributes: Option<ImportAttributes<'js>>,
    ) -> rquickjs::Result<Module<'js>> {
        let source = self.sources.get(name).copied().ok_or_else(|| {
            rquickjs::Error::new_loading_message(
                name.to_string(),
                "No such file or directory (os error 2)",
            )
        })?;
        Module::declare(ctx.clone(), name, source)
    }
}

/// 装配好的引擎（prelude + 虚拟模块 + 测试 resolver/loader）。
pub struct VirtualHarness {
    pub engine: JsEngine,
}

impl VirtualHarness {
    /// 建引擎并安装虚拟模块（permissive test resolver）。
    pub fn new() -> Self {
        Self::build(false)
    }

    /// 用**真实**的 [`ModuleResolver`] + `VirtualFs` 建引擎（与 loader-runtime 的装配一致）。
    ///
    /// 回归 `virtual_modules::install` 的安装前置条件：QuickJS 会把每个 import specifier 先交给
    /// 宿主的 normalize 回调，所以安装期内部使用的 `kiana-virtual/helpers/*.js` 也必须是
    /// `VirtualFs` 里的条目（`Module::declare` 的模块缓存查找发生在规范化之后）。
    pub fn new_with_real_resolver() -> Self {
        Self::build(true)
    }

    fn build(real_resolver: bool) -> Self {
        let engine = JsEngine::new().expect("create engine");
        engine::install(&engine).expect("install prelude_core");
        // node-globals 单元在真实装配里先于虚拟模块安装（§9.2）；typebox 的 hashing 模块在顶层
        // 构造 TextEncoder，这里给出最小 UTF-8 编解码器。
        engine
            .eval_script::<()>(TEXT_CODEC_STUB, "<text-codec-stub>")
            .expect("install TextEncoder/TextDecoder stub");
        // 与真实装配一致：loader 必须在 `virtual_modules::install` 之前就位（typebox 的导入环
        // 只能由 loader 在 import 时按需声明/链接）。
        virtual_modules::register_module_sources(&engine).expect("register virtual sources");
        let sources: HashMap<String, &'static str> = engine.module_sources().into_iter().collect();
        if real_resolver {
            let resolver = ModuleResolver::new(ResolverOptions {
                extension_dir: None,
                virtual_fs: virtual_modules::virtual_fs(),
            });
            engine.set_loader(resolver, SourceLoader { sources });
        } else {
            engine.set_loader(VirtualResolver, SourceLoader { sources });
        }
        if let Err(error) = virtual_modules::install(&engine) {
            let caught =
                engine.with_js(|ctx| engine::take_exception(&ctx, engine::JsErrorKind::Host));
            panic!(
                "install virtual modules: {error:?}: {}: {}\n{}",
                caught.name,
                caught.message,
                caught.stack.unwrap_or_default()
            );
        }
        Self { engine }
    }

    /// 全局脚本求值（panic 时带 JS 错误文本）。
    pub fn eval<T>(&self, source: &str) -> T
    where
        T: for<'js> FromJs<'js>,
    {
        match self.engine.eval_script_error::<T>(source, "<virtual-test>") {
            Ok(value) => value,
            Err(error) => panic!("eval failed: {}: {}", error.name, error.message),
        }
    }

    /// 全局脚本求值，错误物化为 `(name, message)`。
    pub fn try_eval<T>(&self, source: &str) -> Result<T, (String, String)>
    where
        T: for<'js> FromJs<'js>,
    {
        self.engine
            .eval_script_error::<T>(source, "<virtual-test>")
            .map_err(|error| (error.name, error.message))
    }

    /// `JSON.stringify(<expr>)` 后解析成 serde_json。
    pub fn eval_json(&self, expr: &str) -> serde_json::Value {
        let text: String = self.eval(&format!("JSON.stringify({expr})"));
        serde_json::from_str(&text).expect("valid JSON from JSON.stringify")
    }

    /// 求值 `JSON.stringify(<expr>)`，其中 expr 可以是表达式（顶层 await 允许）。
    pub fn eval_promise_json(&self, expr: &str) -> serde_json::Value {
        let text: String = self.eval_promise(&format!("JSON.stringify({expr})"));
        serde_json::from_str(&text).expect("valid JSON from JSON.stringify")
    }

    /// 求值一个 promise 表达式并泵到 settle。
    ///
    /// 与 `Ctx::eval_promise`（`JS_EVAL_FLAG_ASYNC`）不同，这里直接求值表达式并持有返回的
    /// Promise：async flag 会把脚本包成 async 函数，表达式语句的完成值会被丢弃，无法观察结果。
    pub fn eval_promise<T>(&self, source: &str) -> T
    where
        T: for<'js> FromJs<'js>,
    {
        self.engine.with_js(|ctx| {
            let promise: rquickjs::Promise = match ctx.eval(source) {
                Ok(promise) => promise,
                Err(error) => {
                    let caught = engine::take_exception(&ctx, engine::JsErrorKind::Host);
                    panic!("eval_promise failed: {error:?}: {}", caught.message)
                }
            };
            loop {
                if let Some(result) = promise.result::<T>() {
                    match result {
                        Ok(value) => return value,
                        Err(_) => {
                            let caught = engine::take_exception(&ctx, engine::JsErrorKind::Host);
                            panic!("promise rejected: {}", caught.message)
                        }
                    }
                }
                if !ctx.execute_pending_job() {
                    panic!("promise did not settle");
                }
            }
        })
    }

    /// 导入一个模块并把 namespace 绑到 `globalThis[global]`。
    pub fn bind_module(&self, specifier: &str, global: &str) {
        self.engine.with_js(|ctx| {
            let promise = Module::import(&ctx, specifier)
                .unwrap_or_else(|error| panic!("import {specifier}: {error:?}"));
            let namespace: Object = match promise.finish::<Object>() {
                Ok(namespace) => namespace,
                Err(_) => {
                    let caught = engine::take_exception(&ctx, engine::JsErrorKind::Host);
                    panic!("import {specifier} failed: {}", caught.message)
                }
            };
            ctx.globals()
                .set(global, namespace)
                .expect("bind namespace global");
        });
    }

    /// 安装一个 JS 全局（uuidv7/event-stream 依赖的 crypto/performance 等）。
    pub fn install_test_globals(&self) {
        self.eval::<()>(
            r#"
            globalThis.performance = globalThis.performance || { now: () => 0 };
            let __seed = 0x12345678;
            globalThis.crypto = {
                getRandomValues(array) {
                    for (let index = 0; index < array.length; index++) {
                        __seed = (__seed * 1664525 + 1013904223) >>> 0;
                        array[index] = __seed & 0xff;
                    }
                    return array;
                },
            };
            "#,
        );
    }
}
