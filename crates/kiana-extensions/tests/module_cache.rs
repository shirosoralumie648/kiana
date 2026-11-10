//! `?v=<generation>-<seq>` 去缓存与模块表语义（`spec/design-extension-runtime.md` §3.4）。
//!
//! TS 的 `loader.ts` 用 `jiti(..., { moduleCache: false })`：每次 `jiti.import(entry)` 都重新
//! 执行模块。Rust 侧的等价物是给入口模块名加 `<abs path>?v=<gen>-<seq>`（QuickJS 的模块表按
//! 精确名字去重）。本文件锁定：
//!
//! - 同名文件带不同 query 各执行一次，入口里的相对依赖在同一代内共享（jiti 每次会重跑整棵图，
//!   设计 §3.4 记录了这处刻意差异；要逐条对齐时开 `KIANA_EXT_STRICT_NOCACHE=1`）；
//! - `KIANA_EXT_STRICT_NOCACHE=1` 时整棵文件图都带同一个 query（[`ScopedResolver`]）；
//! - 环回入口（`a → b → a`）时依赖里的 `./a.ts` 复用入口的版本化实例，入口体只执行一次；
//!   入口由 `load_module` 或 `Module::declare` 直接声明都成立（`ScopedResolver` 的入口映射）；
//! - CJS 入口（`__kiana_cjs_acquire(..., fresh=true)`）每代重新求值，干净名字只求值一次。
//!
//! 这是 `packages/coding-agent/test/suite/regressions/extension-factory-cache.test.ts` 的
//! **模块级一半**：`moduleLoads` 计数；工厂重跑与缓存键 `(path, cwd, generation)` 属于
//! loader-facade / factory_cache 单元。
#![cfg(feature = "runtime")]

use std::path::{Path, PathBuf};

use kiana_extensions::loader::loader_impl::{ExtensionLoader, LoaderConfig};
use kiana_extensions::loader::resolver::{ModuleResolver, ResolverOptions, VirtualFs};
use rquickjs::{Context, Runtime};

fn fixture(relative: &str) -> String {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/loader")
        .join(relative)
        .to_string_lossy()
        .into_owned()
}

struct Harness {
    _rt: Runtime,
    ctx: Context,
    loader: ExtensionLoader,
}

impl Harness {
    /// `strict` 对应 `KIANA_EXT_STRICT_NOCACHE=1`（§3.4）。
    fn new(strict: bool) -> Self {
        let rt = Runtime::new().expect("create runtime");
        let ctx = Context::full(&rt).expect("create context");
        let loader = ExtensionLoader::new(LoaderConfig {
            virtual_fs: VirtualFs::empty(),
            strict_nocache: strict,
        });
        let resolver = ModuleResolver::new(ResolverOptions {
            extension_dir: Some(
                PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/loader"),
            ),
            virtual_fs: VirtualFs::empty(),
        });
        rt.set_loader(loader.resolver(resolver), loader.clone());
        Self {
            _rt: rt,
            ctx,
            loader,
        }
    }

    fn load(&self, name: &str) {
        self.ctx
            .with(|ctx| -> rquickjs::Result<()> {
                let module = self.loader.load_module(&ctx, name)?;
                let (_module, promise) = module.eval()?;
                promise.finish::<()>()
            })
            .unwrap_or_else(|error| panic!("load `{name}` failed: {error:?}"));
    }

    fn global<T>(&self, expression: &str) -> T
    where
        T: for<'js> rquickjs::FromJs<'js>,
    {
        self.ctx
            .with(|ctx| ctx.eval(expression))
            .unwrap_or_else(|error| panic!("eval `{expression}` failed: {error:?}"))
    }
}

#[test]
fn query_busting_reexecutes_the_entry_but_shares_the_dependency() {
    let harness = Harness::new(false);
    let entry = fixture("basic/counter.ts");

    harness.load(&format!("{entry}?v=1-1"));
    assert_eq!(harness.global::<i32>("globalThis.__counterMarker.loads"), 1);

    harness.load(&format!("{entry}?v=2-1"));
    // 入口执行两次，依赖只执行一次（同一代共享）。
    assert_eq!(harness.global::<i32>("globalThis.__counterEntryLoads"), 2);
    assert_eq!(harness.global::<i32>("globalThis.__counterDepLoads"), 1);
    // 依赖导出的对象是同一个：两次入口求值都在它上面累加。
    assert_eq!(harness.global::<i32>("globalThis.__counterMarker.loads"), 2);
}

#[test]
fn redeclaring_a_clean_name_reexecutes_the_entry_but_shares_the_dependency() {
    // QuickJS 的模块表只在**解析导入**时按名字命中；门面再次 `Module::declare` 同名入口会得到
    // 一份新的模块定义（jiti 的 `moduleCache:false` 同样会重跑入口）。入口的相对依赖仍按干净
    // 名字共享。
    let harness = Harness::new(false);
    let entry = fixture("basic/counter.ts");
    harness.load(&entry);
    harness.load(&entry);
    assert_eq!(harness.global::<i32>("globalThis.__counterEntryLoads"), 2);
    assert_eq!(harness.global::<i32>("globalThis.__counterDepLoads"), 1);
    assert_eq!(harness.global::<i32>("globalThis.__counterMarker.loads"), 2);
}

#[test]
fn strict_nocache_reexecutes_the_whole_dependency_graph() {
    let harness = Harness::new(true);
    let entry = fixture("basic/counter.ts");

    harness.load(&format!("{entry}?v=1-1"));
    harness.load(&format!("{entry}?v=2-1"));

    assert_eq!(harness.global::<i32>("globalThis.__counterEntryLoads"), 2);
    // 整棵图带同一个 query：依赖也重新执行，每代一个实例（marker.loads 各为 1）。
    assert_eq!(harness.global::<i32>("globalThis.__counterDepLoads"), 2);
    assert_eq!(harness.global::<i32>("globalThis.__counterMarker.loads"), 1);
}

#[test]
fn default_mode_keeps_the_dependency_shared_even_across_generations() {
    let harness = Harness::new(false);
    let entry = fixture("basic/counter.ts");
    harness.load(&format!("{entry}?v=1-1"));
    harness.load(&format!("{entry}?v=2-1"));
    // 与 `strict_nocache_reexecutes_the_whole_dependency_graph` 成对：默认模式下依赖只跑一次。
    assert_eq!(harness.global::<i32>("globalThis.__counterDepLoads"), 1);
}

#[test]
fn cyclic_import_back_to_the_entry_reuses_the_entry_instance() {
    // review finding 2: `a → b → a` 环回入口时，`b` 的 `./a.ts` 必须解析回入口已声明的
    // `<a>?v=1-1`，否则入口体会执行两次（jiti 在一个 jiti 实例内只执行一次）。
    let harness = Harness::new(false);
    let entry = fixture("cycle_entry/a.ts");

    harness.load(&format!("{entry}?v=1-1"));
    assert_eq!(harness.global::<i32>("globalThis.__cycleEntryLoads"), 1);
    assert_eq!(harness.global::<i32>("globalThis.__cycleDepLoads"), 1);
    // 环里的绑定指向同一个入口实例（没有第二份 `helperValue`；读全局避免再次 declare）。
    assert_eq!(harness.global::<String>("globalThis.__cycleFromB"), "b-11");

    // 下一代：入口重新执行，相对依赖仍按设计 §3.4 共享。
    harness.load(&format!("{entry}?v=2-1"));
    assert_eq!(harness.global::<i32>("globalThis.__cycleEntryLoads"), 2);
    assert_eq!(harness.global::<i32>("globalThis.__cycleDepLoads"), 1);
    assert_eq!(harness.global::<String>("globalThis.__cycleFromB"), "b-11");
}

#[test]
fn cyclic_entry_declared_directly_reuses_the_entry_instance() {
    // 门面路径：入口由 `Module::declare` 直接声明（名字来自 `scoped_module_name`），本 loader
    // 只服务依赖。`ScopedResolver` 的入口映射必须在没有 `load_module` 参与时也生效。
    let harness = Harness::new(false);
    let entry = fixture("cycle_entry/entry.js");
    let name = format!("{entry}?v=1-1");
    let source = std::fs::read_to_string(&entry).expect("entry source");
    harness
        .ctx
        .with(|ctx| -> rquickjs::Result<()> {
            let declared = rquickjs::Module::declare(ctx.clone(), name.clone(), source)?;
            let (_module, promise) = declared.eval()?;
            promise.finish::<()>()
        })
        .expect("declare the entry directly");

    assert_eq!(
        harness.global::<i32>("globalThis.__directCycleEntryLoads"),
        1
    );
    assert_eq!(harness.global::<i32>("globalThis.__directCycleDepLoads"), 1);
    assert_eq!(
        harness.global::<String>("globalThis.__directCycleFromB"),
        "b-11"
    );
}

#[test]
fn cjs_entry_is_fresh_per_generation() {
    let harness = Harness::new(false);
    let entry = fixture("cjs/counter.cjs");

    harness.load(&format!("{entry}?v=1-1"));
    assert_eq!(harness.global::<i32>("globalThis.__cjsEntryLoads"), 1);
    assert_eq!(harness.global::<i32>("globalThis.__cjsLast.loads"), 1);

    harness.load(&format!("{entry}?v=2-1"));
    // fresh=true：新的模块记录，模块体重新求值（jiti 的 moduleCache:false）。
    assert_eq!(harness.global::<i32>("globalThis.__cjsEntryLoads"), 2);
    assert_eq!(harness.global::<i32>("globalThis.__cjsLast.loads"), 2);
}

#[test]
fn cjs_clean_name_is_evaluated_once() {
    let harness = Harness::new(false);
    let entry = fixture("cjs/counter.cjs");
    harness.load(&entry);
    assert_eq!(harness.global::<i32>("globalThis.__cjsLast.loads"), 1);
    harness.load(&entry);
    // 干净名字没有 `fresh`：`__kiana_cjs_acquire` 复用同一条模块记录。
    assert_eq!(harness.global::<i32>("globalThis.__cjsLast.loads"), 1);
    assert_eq!(harness.global::<i32>("globalThis.__cjsEntryLoads"), 1);
}
