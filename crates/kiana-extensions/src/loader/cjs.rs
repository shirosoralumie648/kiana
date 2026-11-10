//! 真 CJS：预登记模块记录 + sloppy `new Function` 求值 + Node/jiti ESM 互操作
//! （`spec/design-extension-runtime.md` §3.5）。
//!
//! 一个 CJS 文件在模块图里表现为**一个 ESM 模块**：其 `default` 是 CJS 加载器求值后的
//! `module.exports`（jiti 的 interop 规则，见下），其具名导出是 Node `cjs-module-lexer` 能静态
//! 识别的 `exports.*` / `module.exports = { … }` 名字。包装器形态（`{name}` 是模块名，可能带
//! `?v=<gen>-<seq>`；`{path}`/`{dir}` 是去掉 query 的真实路径与目录）：
//!
//! ```js
//! const __kiana_cjs = globalThis.__kiana_cjs_acquire({path}, {dir}, {fresh});
//! const module = __kiana_cjs[0];
//! const exports = module.exports;
//! const require = module.require;
//! if (__kiana_cjs[1]) {
//!   try {
//!     (function (module, exports, require, __filename, __dirname) {
//!     new Function("module", "exports", "require", "__filename", "__dirname", {raw})
//!       .call(module.exports, module, exports, require, {path}, {dir});
//!     })(module, exports, require, {path}, {dir});
//!     module.loaded = true;
//!   } catch (error) { globalThis.__kiana_cjs_delete({path}, module); throw error; }
//! }
//! export default <jiti interop default>;
//! const __kiana_export_0 = module.exports["quote"];
//! export { __kiana_export_0 as quote };
//! ```
//!
//! 与设计文档 §3.5 片段相比的四处**有依据的**偏差（每处都对齐 jiti 2.7 / Node 的实测行为）：
//! 1. 模块体用 `new Function(...)` 求值（§3.5 第 4 条本身就这么规定），而不是把 `raw` 直接拼进
//!    IIFE。包装器是 ES 模块，直接拼接会让模块体变成严格模式：`implicitGlobal = 1`、`with (o)`
//!    等 sloppy-only 的合法 CJS 会抛错，而 jiti/Node 能跑。
//! 2. 模块记录由 [`CJS_JS_SOURCE`] 的 `__kiana_cjs_acquire` **预登记**进 `globalThis.__kiana_cjs`
//!    （键为真实绝对路径），并带 `id/filename/path/loaded/parent/children/paths/require`。子模块
//!    `require(entry)` 因此拿到同一个 `module.exports`，不会重复求值；`require.cache[__filename]`
//!    在入口求值期间就存在（Node 语义）。
//! 3. `default` 采用 jiti 2.7 的 interop，并区分 jiti 的两条加载路径（实测，见
//!    `native_cjs_interop`）：
//!    - **原生 CJS**（`.cjs`；`type != "module"` 的 `.js`）：jiti 用 Node 的原生 CJS 加载，
//!      `default` = `me.__esModule && typeof me.default === "function" ? me.default : me`；
//!    - **经 Babel 转换**（TS 扩展名、`.mjs`、`type == "module"` 的 `.js`）：
//!      `me.default == null ? me : (me.__esModule && typeof me.default.default === "function" ?
//!      me.default.default : me.default)`。
//!
//!   具名导出从源码静态提取（见 [`cjs_named_exports`]），使 `import { quote } from "shell-quote"`
//!   在 QuickJS 链接期成立——jiti 下这是 Node 的 CJS 具名导出检测，不是 ESM 包装。
//! 4. `__filename`/`__dirname`/`require` 都用**真实路径**（去掉 `?v=`），只有模块名保留 query。
//!
//! 递归的 `require` 由 JS 侧的 [`CJS_JS_SOURCE`]（`js/cjs.js`）实现；它只依赖两个同步宿主函数
//! `__kiana_host.resolve(from, spec)` 与 `__kiana_host.read(path)`（后者对 ESM 文件返回 `null`，触发
//! `ERR_REQUIRE_ESM`）。宿主函数由 loader-host 单元安装，本模块与其没有编译期依赖。
//!
//! `require` 的三类非文件目标由 `js/cjs.js` 在**不新增宿主依赖**的前提下分派（见该文件头）：
//! Node 内建（`node:*`）查 `globalThis.__kiana_node_shims`（可选回退 `__kiana_host.requireBuiltin`），
//! 22 个虚拟模块查 `globalThis.__kiana_virtual`，JSON 直接在 JS 侧 `JSON.parse`。
//!
//! # CJS 判定（jiti 2.7 的实测分派规则）
//!
//! jiti 对**所有** TS 文件做 Babel 转换并以 CJS 求值；`.js`/`.mjs` 则按 package `type` 与
//! 源码语法决定是原生 ESM、原生 CJS 还是转换。本移植的等价规则：
//!
//! - 含静态 `import`/`export`（`program_has_esm_syntax`，TS 的 `import =`/`export =` 除外）→ ESM。
//! - `.cjs` → CJS；TS 扩展名（且无 ESM 语法）→ CJS（jiti 对 TS 恒走 CJS 求值器）。
//! - `.mjs` → 只有出现 CJS 记号（自由 `require`/`module`/`exports`/`__dirname`/`__filename`）时
//!   才是 CJS：jiti 先原生 import，求值抛 `module is not defined` 后再回退到 CJS 求值。
//! - 其余（`.js`）：有 CJS 记号 → CJS；否则 `package.json` 的 `type == "module"` → ESM，缺省 →
//!   CJS（Node 原生 require 语义）。
//!
//! 记号检测基于 oxc 语义分析的 root unresolved references，注释/字符串里的 `require(` 不会被误判
//! （jiti 的字符启发式会，但 jiti 的 `.ts` 永远走 CJS，所以对 TS 无影响）。
//!
//! # 本文件在调用链中的位置
//!
//! “判定”和“包装”是两个独立入口，`loader::loader_impl` 按下面的顺序使用它们：
//! `ExtensionLoader::prepare_with` 读文件（`std::fs::read`）→ `transpile::strip_bom` →
//! [`is_cjs_module`]（判定用**原始源码**：TS 的 `export =` 在去类型后会消失）→
//! （TS 先 `transpile::strip_types`）→ [`wrap_module`]（TS 传去类型后的 JS）→ 宿主声明并求值。
//! CJS 运行时依赖的全局函数由 `ExtensionLoader::install_cjs_bridge` 求值 [`CJS_JS_SOURCE`]
//! 安装；本模块只产出文本，既不安装也不检查这些全局是否存在。
//!
//! 这些 `pub` 项被同 crate 的 `loader_impl` / `transpile` 与集成测试 `tests/cjs_loader.rs`
//! 直接调用，测试按生成文本的片段断言，因此改动模板的任何字符都会被两边观察到。
//!
//! # 生成流程与失败模式
//!
//! 1. [`wrap_module`] 先算真实路径（去 `?v=`）与目录，剥离 BOM/shebang，再把路径与源码转成
//!    JS 字符串字面量；
//! 2. 静态扫描具名导出（[`cjs_named_exports`]），并选择 `default` 的 interop 表达式
//!    （`native_cjs_interop`）；
//! 3. 拼接包装器：`__kiana_cjs_acquire` 预登记或复用模块记录，只有 `__kiana_cjs[1]` 为真时
//!    才用 `new Function` 执行 CJS 体；CJS 体抛错时 `__kiana_cjs_delete` 删除记录后重新抛出，
//!    调用方看到的是原始异常；
//! 4. 求值成功后按 `module.exports` 生成 `default` 与具名导出的**快照**。
//!
//! 本模块的函数都不返回 `Result`，可能失败的外部动作一律降级成“没有”：
//!
//! - 源码解析失败时 [`cjs_named_exports`] 返回空列表（具名导入会在链接期失败）；CJS 体的语法
//!   错误在 [`wrap_module`] 内不会暴露（`new Function` 要到求值时才编译），`loader_impl` 会在
//!   调用前用 `cjs_parse_error` 以同一函数体形态预解析并报出带行列的诊断；
//! - [`cjs_named_exports`] 为跟随 re-export 读取依赖文件，读不到就当作没有更多导出；
//! - [`is_cjs_module`] 不做文件 IO；源码解析失败时退回文本启发式（误判面见各函数说明）。
//!
//! # 重要约束
//!
//! - [`wrap_module`] 与 [`cjs_named_exports`] **不是纯函数**：前者为选 interop 规则会向上查找
//!   `package.json`（[`package_type_for`]），后者会直接 `std::fs::read_to_string` 依赖文件。
//!   二者都在调用线程同步做文件 IO：即使源码只是内存里的字符串，`wrap_module` 对 `.js`
//!   模块名仍会逐级读取 `package.json`；读失败就继续向上一级。文件系统布局因此会影响生成的
//!   文本（interop 规则与具名导出列表），测试需要用真实夹具才能覆盖（`tests/cjs_loader.rs`）。
//! - re-export 跟随只认字面量、相对或绝对的 `require("./x")` 形状；裸包名与变量/模板
//!   specifier 不会被跟随。扫描遍历整棵 AST 且不执行源码，所以条件分支里、甚至永不执行的
//!   代码里的 `exports.x = …` 也会被收集；运行期才能确定的导出名则拿不到。
#![cfg(feature = "runtime")]

use std::collections::HashSet;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use oxc_allocator::Allocator;
use oxc_ast::AstKind;
use oxc_ast::ast::{
    CallExpression, Expression, MemberExpression, ObjectExpression, ObjectPropertyKind, Program,
};
use oxc_parser::{ParseOptions, Parser};
use oxc_semantic::SemanticBuilder;
use oxc_span::SourceType;

/// `spec/design-extension-runtime.md` §3.5 的 `require` 实现，注册在 `globalThis.__kiana_make_require`。
///
/// 由 loader-host 单元决定何时求值（prelude 或 `Module::declare`）；本单元只用它做真实执行测试。
///
/// 源码在编译期由 `include_str!` 嵌入，改动 `js/cjs.js` 不需要重新生成 Rust 代码。一次求值安装
/// 的是**一组**全局：`globalThis.__kiana_cjs`（JS 侧模块缓存，键为真实绝对路径）、
/// `__kiana_cjs_acquire` / `__kiana_cjs_delete`（[`wrap_module`] 生成的包装器使用）与
/// `__kiana_make_require`（ESM 模块与包装器里的 `require`）。loader 的 `install_cjs_bridge` 在
/// 求值前先检查 `__kiana_cjs_acquire` 是否已是函数，已安装则直接复用。
///
/// 因此 [`wrap_module`] 产生的源码有一个**运行时前置条件**：本常量必须先被求值。本模块只产出
/// 文本，无法自行保证；缺失时包装器会在调用 `globalThis.__kiana_cjs_acquire` 处抛 `TypeError`
/// （`loader_impl` 用 `install_cjs_bridge` 显式保证安装，就是为了避免这种错误形态）。
pub const CJS_JS_SOURCE: &str = include_str!("../../js/cjs.js");

/// The §3.5 source template for turning a CJS file into an ESM module.
///
/// `name` is the module name (as returned by the resolver, possibly with a `?v=` suffix);
/// `raw` is the CJS source text. For a `.ts`/`.cts` file, call
/// [`super::transpile::strip_types`] first and pass the JavaScript output; [`is_cjs_module`] is
/// evaluated on the *raw* source (TypeScript's `export =` disappears after transpiling).
///
/// The generated module pre-registers its record in `globalThis.__kiana_cjs`, evaluates the body with
/// `new Function` (sloppy, like Node), exports jiti's interop `default`, and synthesizes the named
/// exports statically detectable in `raw` (see [`cjs_named_exports`]). The UTF-8 BOM and `#!`
/// shebang are stripped first, like Node's `Module._compile`.
///
/// # 输出与副作用
///
/// 返回完整的 ESM 源码。除了 [`CJS_JS_SOURCE`] 安装的全局函数，生成的代码不依赖其它运行时前提；
/// 它也不在这里检查 CJS 体是否语法正确（`new Function` 要到求值时才编译）。本函数不进入 JS 引擎，
/// 但会在调用线程同步做文件 IO：[`cjs_named_exports`] 跟随 re-export，`native_cjs_interop`
/// （经由 [`package_type_for`]）为 `.js` 模块名向上查找 `package.json`。两处查找失败都按
/// “没有”处理，不会返回错误。
///
/// # 生成顺序（与输出的段落一一对应）
///
/// 1. `real` 是去掉 `?v=<gen>-<seq>` 的模块名，`dir` 是它的父目录；剥离 BOM/shebang 后的
///    `raw` 与这两个路径都用 [`js_string_literal`] 转义；
/// 2. `fresh = real != name`：只有入口名带 `?v=` 时才强制新建模块记录（jiti 的
///    `moduleCache: false`）。其余情况 `__kiana_cjs_acquire` 复用该路径已有的记录，这也是
///    `require` 与 `import` 看到同一个 `module.exports` 的原因；
/// 3. 只有 acquire 结果表示“新建”（`__kiana_cjs[1]`）时才执行 CJS 体。CJS 体抛错时先用
///    `__kiana_cjs_delete(path, module)` 删除记录再重新抛出，失败的模块不会留在缓存里；
/// 4. `export default` 在 CJS 体之后求值，读取的是此时的 `module.exports`，所以模块体里对
///    `module.exports` 的重新赋值会被看到；
/// 5. 每个具名导出生成一行
///    `const __kiana_export_N = module.exports == null ? undefined : module.exports["name"];`
///    与 `export { … as … }`。这些常量在模块求值时**快照**取值：此后对 `module.exports` 的
///    重新赋值、或新增的键不会反映到具名绑定上（导出对象内部的修改仍共享同一引用）。
///
/// # 边界
///
/// - CJS 体的 `this` 是 `module.exports`（`.call(module.exports, …)`），所以顶层
///   `this.x = 1` 等价于 `exports.x = 1`（`top_level_this_is_module_exports` 锁定）；
/// - `module.exports == null` 是合法的 CJS 结果：`default` 与具名导出都先判空再取属性；
/// - 具名导出列表为空时不生成 `export {}` 语句（模块仍由 `export default` 成立）；
/// - `raw` 是不可信的扩展源码，只作为 `new Function` 的**字符串参数**嵌入，不拼进包装器代码
///   本身；嵌入前经 [`js_string_literal`] 转义，因此引号、反斜杠、换行都无法改变包装器结构
///   （`template_embeds_raw_source_as_a_string_literal` 锁定这一点）。
pub fn wrap_module(name: &str, raw: &str) -> String {
    let real = super::transpile::module_path(name);
    let dir = module_dir(name);
    // BOM/shebang 在转义之前剥离：`new Function` 不像 Node 的模块编译器那样自动忽略它们。
    let raw = strip_bom_and_shebang(raw);
    let real_lit = js_string_literal(real);
    let dir_lit = js_string_literal(&dir);
    let raw_lit = js_string_literal(raw);
    // 只有带 `?v=` 的入口名才算“新模块”；子模块用真实路径复用 JS 侧缓存记录。
    let fresh = real != name;
    // 静态扫描（会为 re-export 读依赖文件）与 interop 规则选择都发生在拼接模板之前。
    let exports = cjs_named_exports(name, raw);
    let default_export = default_export_expression(name);

    let mut out = String::new();
    let _ = write!(
        out,
        "const __kiana_cjs = globalThis.__kiana_cjs_acquire({real_lit}, {dir_lit}, {fresh});\n\
         const module = __kiana_cjs[0];\n\
         const exports = module.exports;\n\
         const require = module.require;\n\
         if (__kiana_cjs[1]) {{\n\
         try {{\n\
         (function (module, exports, require, __filename, __dirname) {{\n\
         new Function(\"module\", \"exports\", \"require\", \"__filename\", \"__dirname\", {raw_lit})\
         .call(module.exports, module, exports, require, {real_lit}, {dir_lit});\n\
         }})(module, exports, require, {real_lit}, {dir_lit});\n\
         module.loaded = true;\n\
         }} catch (error) {{\n\
         globalThis.__kiana_cjs_delete({real_lit}, module);\n\
         throw error;\n\
         }}\n\
         }}\n\
         export default {default_export};\n"
    );
    // 每个名字先取一次值再导出：包装器提供的具名导出是快照，不跟踪之后对 module.exports 的
    // 写入；`module.exports = null` 合法，所以取属性前必须判空。
    for (index, export) in exports.iter().enumerate() {
        let _ = writeln!(
            out,
            "const __kiana_export_{index} = module.exports == null ? undefined : module.exports[{}];",
            js_string_literal(export)
        );
    }
    if !exports.is_empty() {
        out.push_str("export { ");
        for (index, export) in exports.iter().enumerate() {
            if index > 0 {
                out.push_str(", ");
            }
            let _ = write!(
                out,
                "__kiana_export_{index} as {}",
                export_specifier_name(export)
            );
        }
        out.push_str(" };\n");
    }
    out
}

/// jiti 2.7's `default` interop, as measured with kiana's options (`moduleCache: false`,
/// `tryNative: false`).
///
/// jiti returns `jiti.import(path, { default: true })` as `proxy.default ?? proxy` where the proxy
/// wraps either Node's ESM namespace of a native CJS file (`tryNative` path) or the transformed
/// `module.exports`. The two collapse to different expressions:
///
/// - native CJS (`.cjs`, and `.js` in a non-`"type": "module"` package):
///   `me.__esModule && typeof me.default === "function" ? me.default : me`
/// - Babel-transformed (TS extensions, `.mjs`, and `.js` in a `"type": "module"` package):
///   `me.default == null ? me : (me.__esModule && typeof me.default.default === "function"
///   ? me.default.default : me.default)`
///
/// Both are null-safe: jiti returns `null`/`undefined` unchanged (`jitiInteropDefault` bails out
/// on primitives). Measured against jiti 2.7 in `tests/cjs_loader.rs`.
///
/// 返回值是一段 JS 表达式文本，引用包装器里的 `module` 绑定，只能放进 [`wrap_module`] 生成的
/// 源码；它经由 `native_cjs_interop` 查 `package.json`，因此对 `.js` 名字有同步文件 IO。
fn default_export_expression(name: &str) -> String {
    if native_cjs_interop(name) {
        "module.exports == null ? module.exports : \
         (module.exports.__esModule && typeof module.exports.default === \"function\" \
         ? module.exports.default : module.exports)"
            .to_string()
    } else {
        "module.exports == null ? module.exports : \
         (module.exports.default == null ? module.exports : \
         (module.exports.__esModule && typeof module.exports.default.default === \"function\" \
         ? module.exports.default.default : module.exports.default))"
            .to_string()
    }
}

/// Whether jiti loads this file through Node's native CJS loader (its `default` interop then
/// follows the native rule) instead of its Babel transform.
///
/// Measured jiti 2.7 dispatch (`eval_evalModule`):
/// - `.cjs` is never transpiled (`forceTranspile` false);
/// - `.mjs` / a `.js` in a `"type": "module"` package are natively imported in async mode and fall
///   back to the transform when native import throws (the CJS-shaped files this loader wraps);
/// - a `.js` in a CommonJS package is loaded by `require`;
/// - every TS extension is transpiled.
///
/// The `?v=` cache buster is ignored when reading the extension / package type.
///
/// 该判断只决定 `default` 用哪条 interop 表达式，不决定是否包装（那由 [`is_cjs_module`] 决定）。
/// `.js` 的 package type 每次调用都重新查找：`package_type_for` 会向上逐级读文件，没有缓存。
fn native_cjs_interop(name: &str) -> bool {
    let path = super::transpile::module_path(name);
    if path.ends_with(".cjs") {
        return true;
    }
    if path.ends_with(".js") {
        return package_type_for(Path::new(path)).as_deref() != Some("module");
    }
    false
}

/// Node `cjs-module-lexer`-style named exports: `exports.x =`, `module.exports.x =`,
/// `module.exports = { x, "y": … }`, `Object.defineProperty(exports, "x", …)`, esbuild's
/// `__export(obj, { x: … })` and re-exports (`__exportStar(require("./x"), exports)`,
/// `module.exports = require("./x")`, including tslib's `tslib_1.__exportStar`).
///
/// `default` is omitted: the wrapper emits it as the ESM `default` export. Other names, including
/// non-identifiers (`exports["a-b"]`), become string export specifiers, which both QuickJS and
/// Node's CJS namespace expose. A source that does not parse yields no named exports, which is
/// also what a broken module would observe.
///
/// # 行为与边界
///
/// - 纯静态：不执行源码，也不做作用域分析（`require` / `module` / `exports` 只按标识符名匹配，
///   被局部变量遮蔽时同样会被当成记号）；
/// - 只跟随**字面量**、相对或绝对的 re-export（`module.exports = require("./x")`、
///   `__exportStar(require("./x"), …)`）；裸包名与动态 specifier 直接跳过；
/// - 跟随 re-export 时在调用线程用 `std::fs::read_to_string` 读取目标；读不到、目标无法解析或
///   超过深度上限都静默截断，不报错；
/// - `default` 与空名字被丢弃，其余名字去重后返回；顺序取决于 `semantic.nodes()` 的遍历顺序，
///   测试样例按源码出现顺序断言；
/// - `name` 可带 `?v=`：内部用真实路径决定 `SourceType`（未知扩展名按 TS 解析），并作为
///   re-export 去重键。
///
/// 本函数同时被 [`wrap_module`] 与 `loader_impl` 的 `rewrite_mixed_esm_exports` 使用：后者对
/// ESM/TS 混写模块也靠它补出 CJS 写入产生的名字。
pub fn cjs_named_exports(name: &str, source: &str) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    let mut visited = HashSet::new();
    collect_named_exports(name, source, &mut names, &mut visited, 0);
    names
}

/// Depth guard for re-export chains (zod/tslib style barrel files).
///
/// 超过 8 层的链被静默截断，不报错；`visited` 会先按真实路径挡住正常循环，这个上限针对的是
/// 病态或非常深的 barrel 链。
const MAX_REEXPORT_DEPTH: usize = 8;

/// 递归扫描一个文件，把静态可识别的导出名追加到 `names`；`visited` 按真实路径去重，
/// `depth` 每次递归 +1。
fn collect_named_exports(
    name: &str,
    source: &str,
    names: &mut Vec<String>,
    visited: &mut HashSet<PathBuf>,
    depth: usize,
) {
    // 深度上限与 visited 是两道独立防线：前者挡住过长的链，后者挡住循环（同一文件只扫一次）。
    if depth > MAX_REEXPORT_DEPTH {
        return;
    }
    let path = super::transpile::module_path(name);
    if !visited.insert(PathBuf::from(path)) {
        return;
    }
    // 真实路径决定解析语法；不认识的扩展名按 TS 兜底。
    let source_type = SourceType::from_path(Path::new(path)).unwrap_or_else(|_| SourceType::ts());
    let alloc = Allocator::default();
    let ret = Parser::new(&alloc, source, source_type)
        .with_options(ParseOptions::default())
        .parse();
    // 有任何语法诊断就整体放弃：本文件贡献 0 个名字，不返回部分结果。
    if !ret.diagnostics.is_empty() {
        return;
    }
    let semantic = SemanticBuilder::new()
        .with_build_nodes(true)
        .build(&ret.program)
        .semantic;

    // 节点表覆盖整棵 AST（含函数体内部），所以嵌套的 `exports.x = …` 也会被收集；
    // scoping 结果在这里不参与判断。
    for node in semantic.nodes().iter() {
        match node.kind() {
            AstKind::AssignmentExpression(assignment) => {
                let Some(member) = assignment
                    .left
                    .as_simple_assignment_target()
                    .and_then(|target| target.as_member_expression())
                else {
                    continue;
                };
                // 赋值形态：`module.exports = {…}` 取对象键；`module.exports = require("…")`
                // 跟随 re-export；`exports.x` / `module.exports.x` 取静态属性名。
                if is_module_exports(member) {
                    match &assignment.right {
                        Expression::ObjectExpression(object) => collect_object_keys(object, names),
                        right => {
                            collect_reexport(path, right, names, visited, depth);
                        }
                    }
                } else if (is_exports_object(member.object())
                    || member
                        .object()
                        .as_member_expression()
                        .is_some_and(is_module_exports))
                    && let Some(property) =
                        member.static_property_name().and_then(|name| name.as_str())
                {
                    push_export_name(names, property);
                }
            }
            AstKind::CallExpression(call) => {
                // 调用形态：`Object.defineProperty(exports, "x", …)`、esbuild/tsup 的 `__export`、
                // tslib 的 `__exportStar`（后者会递归读取目标文件）。
                collect_define_property(call, names);
                collect_esbuild_export(call, names);
                if let Some(specifier) = export_star_specifier(call)
                    && let Some(target) = reexport_source(path, specifier)
                    && let Ok(source) = std::fs::read_to_string(&target)
                {
                    collect_named_exports(
                        &target.to_string_lossy(),
                        &source,
                        names,
                        visited,
                        depth + 1,
                    );
                }
            }
            _ => {}
        }
    }
}

/// Follow `module.exports = require("./x")` re-exports (Node's lexer does the same).
///
/// 只接受字面量 specifier；目标不可解析或不可读时静默返回。`visited` 与 `depth` 由调用链共享，
/// 因此展开的依赖也计入同一个深度预算和同一份去重集合。
fn collect_reexport(
    importer: &str,
    expression: &Expression<'_>,
    names: &mut Vec<String>,
    visited: &mut HashSet<PathBuf>,
    depth: usize,
) {
    let Some(specifier) = require_specifier(expression) else {
        return;
    };
    if let Some(target) = reexport_source(importer, specifier)
        && let Ok(source) = std::fs::read_to_string(&target)
    {
        collect_named_exports(
            &target.to_string_lossy(),
            &source,
            names,
            visited,
            depth + 1,
        );
    }
}

/// `require("<spec>")` literal specifier.
///
/// 纯语法匹配：只检查被调者是名为 `require` 的标识符，不区分它是否被局部变量遮蔽，也不要求
/// 恰好一个参数（多出的参数被忽略）。
fn require_specifier<'a>(expression: &'a Expression<'_>) -> Option<&'a str> {
    let Expression::CallExpression(call) = expression else {
        return None;
    };
    if !matches!(&call.callee, Expression::Identifier(identifier) if identifier.name == "require") {
        return None;
    }
    let Expression::StringLiteral(specifier) = call.arguments.first()?.as_expression()? else {
        return None;
    };
    specifier.value.as_str()
}

/// `__exportStar(require("<spec>"), …)` (also `tslib_1.__exportStar`).
fn export_star_specifier<'a>(call: &'a CallExpression<'_>) -> Option<&'a str> {
    let is_export_star = match &call.callee {
        Expression::Identifier(identifier) => identifier.name == "__exportStar",
        callee => {
            callee
                .as_member_expression()
                .and_then(|member| member.static_property_name())
                .and_then(|name| name.as_str())
                == Some("__exportStar")
        }
    };
    if !is_export_star {
        return None;
    }
    require_specifier(call.arguments.first()?.as_expression()?)
}

/// Resolve a relative re-export target (Node probe order, JSON included for barrel files).
///
/// 实际探测顺序：先试精确路径，再按 `EXTENSIONS` 的固定顺序追加扩展名，最后按同一顺序试
/// `index.<ext>`。它不读目录 `package.json` 的 `main`，所以只覆盖 barrel 文件常见的布局，
/// 不是 Node 的完整解析算法。非相对、非绝对的 specifier（裸包名、`node:`）一律返回 `None`。
fn reexport_source(importer: &str, specifier: &str) -> Option<PathBuf> {
    if !(specifier.starts_with("./")
        || specifier.starts_with("../")
        || Path::new(specifier).is_absolute())
    {
        return None;
    }
    let target = if Path::new(specifier).is_absolute() {
        PathBuf::from(specifier)
    } else {
        Path::new(importer).parent()?.join(specifier)
    };
    if target.is_file() {
        return Some(target);
    }
    const EXTENSIONS: [&str; 7] = [".js", ".cjs", ".mjs", ".ts", ".cts", ".mts", ".json"];
    for extension in EXTENSIONS {
        let mut candidate = target.as_os_str().to_os_string();
        candidate.push(extension);
        let candidate = PathBuf::from(candidate);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    for extension in EXTENSIONS {
        let candidate = target.join(format!("index{extension}"));
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

/// `true` when `expression` is the free identifier `exports`.
fn is_exports_object(expression: &Expression<'_>) -> bool {
    matches!(expression, Expression::Identifier(identifier) if identifier.name == "exports")
}

/// `true` when `member` is `module.exports`.
fn is_module_exports(member: &MemberExpression<'_>) -> bool {
    member.static_property_name().and_then(|name| name.as_str()) == Some("exports")
        && matches!(member.object(), Expression::Identifier(identifier) if identifier.name == "module")
}

/// `Object.defineProperty(exports | module.exports, "name", …)`.
///
/// 属性名必须是字符串字面量（数字、模板字符串等其它表达式都不收集）；`__esModule` 也会作为
/// 具名导出被收集（`detects_named_exports_like_cjs_module_lexer` 锁定）。第三个参数
/// （descriptor）不执行，只取名字。
fn collect_define_property(call: &CallExpression<'_>, names: &mut Vec<String>) {
    let Some(callee) = call.callee.as_member_expression() else {
        return;
    };
    if callee.static_property_name().and_then(|name| name.as_str()) != Some("defineProperty") {
        return;
    }
    if !matches!(callee.object(), Expression::Identifier(identifier) if identifier.name == "Object")
    {
        return;
    }
    let (Some(target), Some(key)) = (call.arguments.first(), call.arguments.get(1)) else {
        return;
    };
    let target = target.as_expression();
    let target_is_exports = target.is_some_and(|expression| {
        is_exports_object(expression)
            || expression
                .as_member_expression()
                .is_some_and(is_module_exports)
    });
    if !target_is_exports {
        return;
    }
    if let Some(Expression::StringLiteral(key)) = key.as_expression()
        && let Some(name) = key.value.as_str()
    {
        push_export_name(names, name);
    }
}

/// esbuild/tsup: `__export(target, { name: () => name, … })`.
///
/// 只认裸标识符 `__export`（`obj.__export(...)` 不认）；每个对象字面量参数的键都收集，不检查
/// 第一个参数是不是 exports。
fn collect_esbuild_export(call: &CallExpression<'_>, names: &mut Vec<String>) {
    let is_export_helper = matches!(
        &call.callee,
        Expression::Identifier(identifier) if identifier.name == "__export"
    );
    if !is_export_helper {
        return;
    }
    for argument in &call.arguments {
        if let Some(Expression::ObjectExpression(object)) = argument.as_expression() {
            collect_object_keys(object, names);
        }
    }
}

/// Collect statically named properties of an object literal.
///
/// 展开（spread）与计算属性跳过，其余非计算键都收集；`default` 与空名字由 `push_export_name`
/// 过滤。
fn collect_object_keys(object: &ObjectExpression<'_>, names: &mut Vec<String>) {
    for property in &object.properties {
        let ObjectPropertyKind::ObjectProperty(property) = property else {
            continue;
        };
        if property.computed {
            continue;
        }
        if let Some(name) = property.key.static_name()
            && let Some(name) = name.as_str()
        {
            push_export_name(names, name);
        }
    }
}

/// Add `name` unless it is the reserved `default` export.
///
/// 线性查找去重，保留首次出现的顺序。
fn push_export_name(names: &mut Vec<String>, name: &str) {
    if name == "default" || name.is_empty() {
        return;
    }
    if !names.iter().any(|existing| existing == name) {
        names.push(name.to_string());
    }
}

/// Export specifier spelling: identifiers stay bare, anything else becomes a string literal
/// (`export { x as "a-b" }`, which QuickJS supports and Node's CJS namespace also exposes).
fn export_specifier_name(name: &str) -> String {
    if is_identifier(name) {
        name.to_string()
    } else {
        js_string_literal(name)
    }
}

/// ASCII JavaScript identifier check.
///
/// 只认 ASCII 字母、数字、下划线和美元符号，且首字符不能是数字；非 ASCII 名字因此走字符串
/// 字面量分支。
fn is_identifier(name: &str) -> bool {
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !(first.is_ascii_alphabetic() || first == '_' || first == '$') {
        return false;
    }
    chars.all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '$')
}

/// Strip a leading UTF-8 BOM and a `#!` shebang line, like Node's `Module._compile`.
///
/// 顺序是先剥 BOM（否则 `#!` 不在开头），再剥 shebang。shebang 的切片从第一个换行符**本身**
/// 开始，所以换行被保留、后续源码的行号不因删掉首行而前移；整份文件只有 shebang 时返回空串。
/// 只有位于最开头的 `#!` 会被处理，前面有空白时按普通源码看待。
fn strip_bom_and_shebang(source: &str) -> &str {
    let source = super::transpile::strip_bom(source);
    if source.starts_with("#!") {
        match source.find('\n') {
            Some(index) => &source[index..],
            None => "",
        }
    } else {
        source
    }
}

/// `__dirname` for a module name: the parent directory, with the `?v=` suffix ignored.
///
/// TS: `path.dirname(name)` in the §3.5 loader snippet.
///
/// 只做字符串层面的父目录计算，不解析符号链接、不做 canonicalize。单段相对名（如 `a.js`）的
/// 父路径是空串，根路径没有父路径，两种情况都返回 `""`。结果会作为 `__dirname` 传给 CJS 体。
pub fn module_dir(name: &str) -> String {
    let path = super::transpile::module_path(name);
    match Path::new(path).parent() {
        Some(parent) => parent.to_string_lossy().into_owned(),
        None => String::new(),
    }
}

/// Whether this file must be loaded through the true-CJS loader (§3.5, jiti's dispatch rules).
///
/// See the module docs for the full cascade; in short: static ESM syntax wins, `.cjs` and
/// TS-without-ESM-syntax are always CJS, `.mjs` needs an actual CJS marker, and `.js` falls back to
/// the nearest `package.json` `type`.
///
/// # 参数与不变量
///
/// - `name` 可带 `?v=`，扩展名判定前会剥掉；
/// - `source` 是调用方读到的文件文本；本函数不读文件、不执行源码；
/// - `package_type` 由调用方查好（通常来自 [`package_type_for`]）；本函数只在最后的后备分支
///   使用它，`loader_impl` 实际只对 `.js` 名字传非 `None`。传 `None` 等价于 `"commonjs"`，
///   即 `.js` 默认走原生 CJS；
/// - 源码解析失败时退回文本启发式，这时字符串/注释里的 `require(`、`export` 会参与判断，
///   语义与解析成功时不同。
///
/// 静态 ESM 语法优先于一切扩展名：文件名是 `.cjs` 但含 `export default` 时同样返回 `false`。
pub fn is_cjs_module(name: &str, source: &str, package_type: Option<&str>) -> bool {
    let path = super::transpile::module_path(name);
    let (has_esm, has_cjs) = match analyze_module_syntax(name, source) {
        Some(syntax) => (syntax.has_esm, syntax.has_cjs),
        None => (heuristic_has_esm(source), heuristic_has_cjs(source)),
    };

    if has_esm {
        return false;
    }
    if path.ends_with(".cjs") {
        return true;
    }
    if super::transpile::is_typescript(name) {
        // jiti transforms every TS file and evaluates it in its CJS wrapper, even without any
        // CJS marker (`module.exports` stays `{}`).
        return true;
    }
    if path.ends_with(".mjs") {
        // jiti first native-imports a `.mjs`; only a ReferenceError such as `module is not
        // defined` triggers the CJS fallback, so a CJS marker is required.
        return has_cjs;
    }
    if has_cjs {
        return true;
    }
    // Native Node: `.js` in a `"type": "module"` package is ESM, everything else is CJS.
    package_type != Some("module")
}

/// Static ESM/CJS shape of one source file.
struct ModuleSyntax {
    /// 顶层有静态 `import`/`export` 声明，或源码文本里出现 `import.meta`。
    has_esm: bool,
    /// 根作用域中存在未解析的 `require`/`module`/`exports`/`__dirname`/`__filename` 引用。
    has_cjs: bool,
}

/// Parse `source` and classify it. `None` when the source does not parse (callers fall back to
/// jiti's textual heuristics, which is also what jiti itself effectively does for broken files).
///
/// `has_esm` 由两部分取或：`program_has_static_esm(...)` 与**文本包含** `import.meta`（不区分
/// 字符串与注释，所以只在注释里提到 `import.meta` 的 `.cjs` 也会被判成 ESM）。`has_cjs` 来自
/// oxc 的 root unresolved references，被局部声明遮蔽的 `require` 等不计入。`SourceType` 由真实
/// 路径决定，未知扩展名回退为 TS 解析。
fn analyze_module_syntax(name: &str, source: &str) -> Option<ModuleSyntax> {
    let path = super::transpile::module_path(name);
    let source_type = SourceType::from_path(Path::new(path)).unwrap_or_else(|_| SourceType::ts());
    let alloc = Allocator::default();
    let ret = Parser::new(&alloc, source, source_type)
        .with_options(ParseOptions::default())
        .parse();
    if !ret.diagnostics.is_empty() {
        return None;
    }
    let has_esm_body = program_has_static_esm(&ret.program);
    let semantic = SemanticBuilder::new().build(&ret.program).semantic;
    let unresolved = semantic.scoping().root_unresolved_references();
    let has_cjs = ["require", "module", "exports", "__dirname", "__filename"]
        .iter()
        .any(|name| unresolved.contains_key(*name));
    Some(ModuleSyntax {
        has_esm: has_esm_body || source.contains("import.meta"),
        has_cjs,
    })
}

/// Static `import`/`export` declarations (TypeScript's `import =` / `export =` are CJS forms).
fn program_has_static_esm(program: &Program<'_>) -> bool {
    super::transpile::program_has_esm_syntax(program)
}

/// jiti's `hasESMSyntax` regex, implemented token-wise for unparsable sources.
///
/// 作为解析失败时的兜底，判断是宽松的：`import`/`export` 只用前一个字节是否为标识符字节来判
/// 词边界，不区分字符串与注释。`import` 后若跟标识符，只在**后续最多 512 字节**内查找
/// ` from `（更长的 import 子句会漏检）。窗口末端向下取整到 UTF-8 字符边界，避免切进
/// 多字节字符。
fn heuristic_has_esm(source: &str) -> bool {
    if source.contains("import.meta") {
        return true;
    }
    let bytes = source.as_bytes();
    let mut index = 0;
    while let Some(offset) = source[index..].find("import") {
        let start = index + offset;
        index = start + "import".len();
        if start > 0 && is_identifier_byte(bytes[start - 1]) {
            continue;
        }
        let rest = source[index..].trim_start();
        if matches!(rest.chars().next(), Some('"' | '\'' | '*' | '{')) {
            return true;
        }
        if rest
            .chars()
            .next()
            .is_some_and(|ch| ch.is_alphanumeric() || ch == '_' || ch == '$')
        {
            let mut window_end = rest.len().min(512);
            while window_end > 0 && !rest.is_char_boundary(window_end) {
                window_end -= 1;
            }
            let window = &rest[..window_end];
            if window.contains(" from ") {
                return true;
            }
        }
    }
    let mut index = 0;
    while let Some(offset) = source[index..].find("export") {
        let start = index + offset;
        index = start + "export".len();
        if start > 0 && is_identifier_byte(bytes[start - 1]) {
            continue;
        }
        let rest = source[index..].trim_start();
        if matches!(rest.chars().next(), Some('*' | '{')) {
            return true;
        }
        for keyword in [
            "default", "class", "type", "function", "const", "var", "let", "async",
        ] {
            if let Some(after) = rest.strip_prefix(keyword)
                && matches!(
                    after.chars().next(),
                    Some(' ' | '\t' | '\n' | '\r' | '(' | '*')
                )
            {
                return true;
            }
        }
    }
    false
}

fn is_identifier_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'$'
}

/// Textual CJS marker, used only when the source does not parse.
///
/// 同样是文本包含：注释/字符串里的 `require(`、`exports.` 也算；不识别 `__dirname`/`__filename`，
/// 也不接受 `require (` 这种中间带空白的写法。
fn heuristic_has_cjs(source: &str) -> bool {
    source.contains("require(")
        || source.contains("module.exports")
        || source.contains("exports.")
        || source.contains("exports[")
}

/// `type` field of the nearest `package.json` above `file`, if any.
///
/// Node stops at the first `package.json`; a manifest without `type` yields `None` (=="commonjs").
///
/// # 输入与边界
///
/// - `file` 必须是文件系统路径：本函数**不剥** `?v=`，带 query 的名字会被当成文件名。调用方
///   （`loader_impl`、`native_cjs_interop`）都先经 `transpile::module_path`；
/// - `file` 是目录时从它自己开始，否则从父目录开始，逐级向上直到根；每一级都尝试
///   `std::fs::read_to_string`，**读失败（不存在或无权限）就继续向上一级**，因此“不可读的
///   package.json”与“没有 package.json”在结果上不可区分；
/// - 找到的第一个 `package.json` 若不能解析成 JSON，立即返回 `None`，不再向上找（与“缺
///   `type`”同结果）。Node 遇到坏 manifest 会抛 `ERR_INVALID_PACKAGE_CONFIG`，本函数的返回
///   类型无法表达这个错误；
/// - 返回拥有所有权的字段值，调用方只在本次加载中使用。
pub fn package_type_for(file: &Path) -> Option<String> {
    let mut dir = if file.is_dir() {
        Some(file)
    } else {
        file.parent()
    };
    while let Some(current) = dir {
        let manifest = current.join("package.json");
        if let Ok(text) = std::fs::read_to_string(&manifest) {
            // The first manifest wins; an unparsable one is treated as "no type" (Node would
            // throw ERR_INVALID_PACKAGE_CONFIG, which the caller cannot represent here).
            let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) else {
                return None;
            };
            return value
                .get("type")
                .and_then(|value| value.as_str())
                .map(ToString::to_string);
        }
        dir = current.parent();
    }
    None
}

/// Quote `value` as a JavaScript string literal, matching `JSON.stringify`'s escaping.
///
/// The §3.5 snippet uses Rust's `{:?}`; for path-like ASCII values the two are identical, but
/// `{:?}` emits `\u{..}` for control characters while JS requires `\u00xx`, so the loader uses
/// this helper for both `name` and `dir`. Non-ASCII text is emitted raw, like `JSON.stringify`.
///
/// 具体规则：`"` 与 `\` 反斜杠转义；`\b`/`\f`/`\n`/`\r`/`\t` 用短转义；其余小于 `0x20` 的控制
/// 字符写成小写四位 `\u00xx`；其它字符（含非 ASCII、`'`、`U+2028`/`U+2029`）原样输出。始终用
/// 双引号包裹，所以 `'` 无需转义。输入是 Rust 的 `String`（保证 UTF-8），不会出现孤立代理项。
pub fn js_string_literal(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for ch in value.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\u{08}' => out.push_str("\\b"),
            '\u{0c}' => out.push_str("\\f"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            ch if (ch as u32) < 0x20 => {
                // Writing to a String cannot fail.
                let _ = write!(out, "\\u{:04x}", ch as u32);
            }
            ch => out.push(ch),
        }
    }
    out.push('"');
    out
}

// 单元测试锁定的是本文件生成的**文本形状**与判定表：模板的每一段、两种 interop 表达式、
// `?v=` 不进入路径、判定优先级与 `package_type_for` 的向上查找。真实求值（循环 require、
// Node 错误形状、interop 的实际结果）在 `tests/cjs_loader.rs`，那里执行 js/cjs.js + 夹具。
#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn heuristic_esm_window_does_not_split_multibyte_characters() {
        let mut source = String::from("import ");
        source.push_str(&"a".repeat(511));
        source.push('é');
        source.push_str(" from 'x';");
        // 修复前会在第 512 字节切进 `é` 并 panic；现在窗口向下取整到字符边界。
        assert!(!heuristic_has_esm(&source));
    }

    #[test]
    fn wraps_with_exact_template() {
        let wrapped = wrap_module("/tmp/ext/mod.js", "exports.x = 1;");
        assert!(wrapped.starts_with(
            "const __kiana_cjs = globalThis.__kiana_cjs_acquire(\"/tmp/ext/mod.js\", \"/tmp/ext\", false);\n"
        ));
        assert!(wrapped.contains("const module = __kiana_cjs[0];\n"));
        assert!(wrapped.contains("const exports = module.exports;\n"));
        assert!(wrapped.contains("const require = module.require;\n"));
        assert!(wrapped.contains("if (__kiana_cjs[1]) {\n"));
        assert!(wrapped.contains(
            "new Function(\"module\", \"exports\", \"require\", \"__filename\", \"__dirname\", \"exports.x = 1;\")\
             .call(module.exports, module, exports, require, \"/tmp/ext/mod.js\", \"/tmp/ext\");"
        ));
        // `.js` in a CommonJS package is loaded natively by jiti: native interop rule.
        assert!(wrapped.contains(
            "export default module.exports == null ? module.exports : \
             (module.exports.__esModule && typeof module.exports.default === \"function\" \
             ? module.exports.default : module.exports);\n"
        ));
        assert!(!wrapped.contains(".default.default"));
        assert!(wrapped.ends_with(
            "const __kiana_export_0 = module.exports == null ? undefined : module.exports[\"x\"];\n\
             export { __kiana_export_0 as x };\n"
        ));
    }

    #[test]
    fn selects_the_interop_rule_by_jiti_loading_path() {
        // Native CJS: `.cjs` always, `.js` only outside a `"type": "module"` package.
        assert!(native_cjs_interop("/tmp/ext/mod.cjs"));
        assert!(native_cjs_interop("/tmp/ext/mod.cjs?v=1-1"));
        assert!(native_cjs_interop("/tmp/ext/mod.js"));
        // Transformed by Babel: TS extensions and `.mjs`.
        assert!(!native_cjs_interop("/tmp/ext/mod.ts"));
        assert!(!native_cjs_interop("/tmp/ext/mod.cts?v=2-3"));
        assert!(!native_cjs_interop("/tmp/ext/mod.mts"));
        assert!(!native_cjs_interop("/tmp/ext/mod.mjs"));

        let native = wrap_module("/tmp/ext/a.js", "module.exports = 1;");
        assert!(native.contains(
            "(module.exports.__esModule && typeof module.exports.default === \"function\""
        ));
        let transformed = wrap_module("/tmp/ext/a.ts", "module.exports = 1;");
        assert!(transformed.contains(
            "module.exports.default == null ? module.exports : \
             (module.exports.__esModule && typeof module.exports.default.default === \"function\" \
             ? module.exports.default.default : module.exports.default)"
        ));
    }

    #[test]
    fn type_module_js_files_use_the_transformed_interop_rule() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/cjs");
        let path = root.join("esm_pkg/cjs_shape.js");
        let name = path.to_string_lossy().into_owned();
        assert!(
            !native_cjs_interop(&name),
            "a `.js` in a type:module package is transformed by jiti"
        );
        let wrapped = wrap_module(&name, "module.exports = { default: 1 };");
        assert!(wrapped.contains("module.exports.default == null ? module.exports : "));
    }

    #[test]
    fn cache_buster_marks_the_module_as_fresh_and_is_not_part_of_paths() {
        let wrapped = wrap_module("/tmp/ext/mod.js?v=3-7", "module.exports = 1;");
        assert!(wrapped.starts_with(
            "const __kiana_cjs = globalThis.__kiana_cjs_acquire(\"/tmp/ext/mod.js\", \"/tmp/ext\", true);\n"
        ));
        assert!(!wrapped.contains("mod.js?v=3-7"));
    }

    #[test]
    fn quotes_name_and_dir_as_js_literals() {
        let wrapped = wrap_module("/tmp/a b\"c/mod.js", "module.exports = 1;");
        assert!(
            wrapped.contains(
                "__kiana_cjs_acquire(\"/tmp/a b\\\"c/mod.js\", \"/tmp/a b\\\"c\", false)"
            )
        );
        // Backslashes also survive the round trip as JS escapes (Windows module names).
        assert_eq!(js_string_literal("C:\\ext\\a.js"), "\"C:\\\\ext\\\\a.js\"");
    }

    #[test]
    fn js_string_literal_matches_json_stringify() {
        assert_eq!(js_string_literal("a/b"), "\"a/b\"");
        assert_eq!(js_string_literal("a\"b"), "\"a\\\"b\"");
        assert_eq!(js_string_literal("a\nb\tc"), "\"a\\nb\\tc\"");
        assert_eq!(js_string_literal("\u{1}"), "\"\\u0001\"");
        // Non-ASCII stays raw, like JSON.stringify.
        assert_eq!(js_string_literal("扩展/🇺🇳"), "\"扩展/🇺🇳\"");
    }

    #[test]
    fn module_dir_ignores_cache_buster() {
        assert_eq!(module_dir("/tmp/ext/mod.js?v=2-9"), "/tmp/ext");
        assert_eq!(module_dir("/tmp/ext/mod.js"), "/tmp/ext");
    }

    #[test]
    fn detects_named_exports_like_cjs_module_lexer() {
        let names = cjs_named_exports(
            "/x/a.js",
            "exports.quote = 1; exports['parse'] = 2; module.exports.deep = 3;\n\
             Object.defineProperty(exports, '__esModule', { value: true });\n\
             Object.defineProperty(exports, 'toFile', { get: () => 1 });\n\
             exports.default = 4;",
        );
        assert_eq!(
            names,
            vec!["quote", "parse", "deep", "__esModule", "toFile"]
        );
    }

    #[test]
    fn detects_object_exports_and_esbuild_helper() {
        let names = cjs_named_exports(
            "/x/a.js",
            "module.exports = { alpha, beta: 1, \"gamma-delta\": 2, default: 3 };\n\
             __export(index_exports, { z: () => z, object: () => object });",
        );
        assert_eq!(names, vec!["alpha", "beta", "gamma-delta", "z", "object"]);
    }

    #[test]
    fn ignores_mentions_in_comments_and_strings() {
        let names = cjs_named_exports(
            "/x/a.js",
            "// exports.fake = 1;\nconst s = 'exports.other = 1';\nexports.real = 1;",
        );
        assert_eq!(names, vec!["real"]);
    }

    #[test]
    fn detects_cjs_files_like_jiti() {
        assert!(is_cjs_module("/x/a.cjs", "", Some("module")));
        // A `.mjs` with top-level CJS syntax falls back to jiti's CJS evaluator.
        assert!(is_cjs_module(
            "/x/a.mjs",
            "module.exports = { value: 1 };",
            None
        ));
        assert!(!is_cjs_module("/x/a.mjs", "export default 1;", None));
        // `.js` in a `"type": "module"` package is ESM unless it has CJS syntax (jiti falls back).
        assert!(!is_cjs_module(
            "/x/a.js",
            "export default 1;",
            Some("module")
        ));
        assert!(is_cjs_module("/x/a.js", "require('x')", Some("module")));
        assert!(is_cjs_module("/x/a.js", "require('x')", None));
        // Comments/strings do not make an ESM file CJS.
        assert!(!is_cjs_module(
            "/x/a.js",
            "// mentions require(\nexport default value;",
            None
        ));
        assert!(is_cjs_module(
            "/x/a.js",
            "module.exports = 1",
            Some("commonjs")
        ));
        // Plain `.js` in a CommonJS package is loaded by Node's CJS loader (empty exports).
        assert!(is_cjs_module("/x/a.js", "console.log(1);", None));
        assert!(!is_cjs_module("/x/a.js", "console.log(1);", Some("module")));
        // TypeScript always goes through jiti's CJS evaluator unless it has ESM syntax.
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
        // Declaration exports and re-export-from are ESM (review finding 1).
        assert!(!is_cjs_module("/x/a.ts", "import x from 'y';", None));
        assert!(!is_cjs_module("/x/a.ts", "export default 1;", None));
        for source in [
            "export const a = 1;",
            "export let a = 1;",
            "export var a = 1;",
            "export function f() {}",
            "export class C {}",
            "export enum E { A }",
            "export const enum E { A }",
            "export interface I { x: number }",
            "export type T = 1;",
            "export namespace N { export const x = 1; }",
            "export { a } from './x';",
            "export { a };",
            "export * from './x';",
            "export { a } from './x';\nmodule.exports = 1;",
        ] {
            assert!(
                !is_cjs_module("/x/a.ts", source, Some("module")),
                "`{source}` must be ESM"
            );
        }
        // A cache-busting query does not change the extension check.
        assert!(is_cjs_module("/x/a.cjs?v=1-1", "", Some("module")));
    }

    #[test]
    fn reads_the_nearest_package_type() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/cjs");
        assert_eq!(package_type_for(&root.join("vendor/ms/package.json")), None);
        assert_eq!(
            package_type_for(&root.join("esm_pkg/package.json")).as_deref(),
            Some("module")
        );
        assert_eq!(
            package_type_for(&root.join("esm_pkg/main.js")).as_deref(),
            Some("module")
        );
    }
}
