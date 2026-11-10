#![cfg(feature = "runtime")]

//! `rquickjs::Loader` 实现：`Module::declare`、`?v=<gen>-<seq>` 去缓存、真 CJS `require`
//! （`spec/design-extension-runtime.md` §3.1、§3.4、§3.5、§3.9；`ARCHITECTURE.md` §2.2）。
//!
//! TS 对应物是 `core/extensions/loader.ts` 的 `loadExtensionModule()`：它建一个
//! `moduleCache: false` 的 jiti，`jiti.import(extensionPath, { default: true })` 把入口模块
//! 转成 CJS 求值并取默认导出。Rust 侧一个 `Runtime` 只有一个 `Loader`（0.14 的
//! `Runtime::set_loader`），本模块就是它，负责把「模块名」变成已声明的 QuickJS 模块。
//!
//! # 流水线（§3.1）
//!
//! ```text
//! load(name)
//!   ├─ 已注册的 shim / 虚拟模块源（`register_source`，22 个 kiana-* 键与 node:* shim）
//!   ├─ VirtualFs 里的嵌入文件（typebox 691 个 .mjs；键名逐字节等于模块名）
//!   ├─ data: URL                      → 解码 payload → Module::declare（MIME/属性按 Node 的
//!   │                                   data: URL ESM 加载器校验，见下）
//!   ├─ node:<name>                    → 内建 shim 源（未注册则报 Node 的
//!   │                                   `No such built-in module: <name>`）
//!   └─ 文件路径（可能带 ?v=）
//!        ├─ .ts/.tsx/.mts/.cts/.mtsx/.ctsx → oxc 去类型（`transpile::strip_types`）
//!        ├─ .js/.mjs                  → 原文（自由的 CJS 绑定按 jiti 的求值器补齐，见下）
//!        ├─ .cjs 或 CJS 探测命中      → `cjs::wrap_module`（真 CJS 包装器；先做语法校验）
//!        ├─ .json                     → `export default` + 顶层键的具名导出（jiti/require 语义）
//!        └─ Module::declare(name, source) + `import.meta`（见下）
//! ```
//!
//! 混写 `export …` 与 `module.exports = …`/`exports.x = …` 的模块会先被改写成 jiti/Babel 的
//! CJS 视角（`rewrite_mixed_esm_exports`）：重新赋值后的 `module.exports` 是默认导出，
//! `exports.*` 键是具名导出（review finding 1）。
//!
//! # `import.meta`
//!
//! jiti 2.7（`tryNative: false`，即内嵌二进制的形态）有两条分支：
//!
//! - **Node 原生 ESM**（`.mjs`，以及 `"type": "module"` 包里的 `.js`：Node 的加载器直接
//!   `import()`）：`import.meta` 是 Node 的完整对象，键顺序 `dirname, filename, main,
//!   resolve, url`。
//! - **Babel 转换分支**（TS、普通 `.js`，以及原生求值抛 `ReferenceError` 后回退的 `.mjs`/
//!   type:module `.js`）：整体读取 `import.meta` 看到的是被当作 `data:` 模块求值的包装器的
//!   原生对象 `{main, resolve, url}`；Babel 把静态 `import.meta.url` 重写成真实路径的
//!   `pathToFileURL` 值（`.js` 的静态 `import.meta.dirname`/`.filename` 也重写，TS 的不重写）。
//!   本移植直接在 `meta` 上写 `main`、`resolve`、`url`，并为 `.js` 把 `dirname`/`filename`
//!   装成**不可枚举**属性：`Object.keys` 与 jiti 一致，静态成员读取仍然可用（review
//!   finding 3）。`resolve(spec)` 走 loader-host 的 `__kiana_host.resolve`。
//!
//! `url` 的百分号编码与 Node 的 `pathToFileURL` 逐字节一致（`file_url`，见 golden fixture），
//! 且始终是去掉 `?v=` 的真实 file URL（jiti 的静态 `import.meta.url` 也是这个值）。
//!
//! # `.js`/`.mjs` 里的自由 CJS 名字
//!
//! jiti 先原生 import `.mjs` / type:module `.js`；只有**求值期**抛 `ReferenceError`（例如顶层
//! `require(...)`）才回退到 Babel 的 CJS 转换。函数体里的 `require`、`typeof require` 这类特征
//! 检测不会触发回退，jiti 的 `require`/`__filename`/`__dirname` 保持 undefined（review
//! finding 4）。本移植用 `eager_cjs_reference` 近似这个条件；普通 `.js` 永远经 jiti 转换，
//! 因此包装器参数总是可用。注入的前导代码与 TS 分支同一套。
//!
//! 已知近似：jiti 的回退是**动态**的（try/catch 守卫、立即调用的 IIFE、以及
//! `if (typeof require !== "undefined") require(...)` 这类守卫里的调用会先抛错再回到转换
//! 分支），静态分析看不到控制流，这些情况按「求值期引用」注入。另有 `arguments`：jiti 的
//! CJS 求值器把它作为包装器实参对象提供（`typeof arguments === "object"`、`length === 7`），
//! ESM 顶层没有 `arguments` 绑定且严格模式禁止 `var arguments`，无法在保持 ESM 输出的前提下
//! 复刻（记录在单元报告里）。
//!
//! # `data:` URL
//!
//! Node 用两条独立的规则（`internal/modules/esm/get_format.js` 的
//! `getDataProtocolModuleFormat` + `internal/data_url.js` 的 `dataURLProcessor`）：
//!
//! - 格式：对序列化后的 pathname 匹配 `/^([^/]+\/[^;,]+)(?:;[^,]*)?,/`，捕获的 MIME 再用
//!   `mimeToFormat` 判定——JS 接受 `text|application/javascript`（大小写不敏感、允许首尾空白
//!   与 `;charset=utf-8`），JSON 只接受精确的 `application/json`；其它（包括 `foo` 这种没有
//!   `/` 的）报 `Unknown module format: <mime|null> for URL <url>`。
//! - payload：fragment 不参与解码；MIME 去首尾 ASCII 空白，`;` + 0..n 个空格 + `base64`
//!   （大小写不敏感）是 base64 标记；payload 先百分号解码，再按 forgiving-base64 解码（忽略
//!   ASCII 空白、容忍省略填充）。解码失败报 `Invalid URL`，且先于格式/属性检查。
//!
//! `application/json` 必须带 `with { type: "json" }`；属性错误的文案与 `code` 逐字对齐 Node
//! （`ERR_IMPORT_ATTRIBUTE_MISSING` / `ERR_IMPORT_ATTRIBUTE_UNSUPPORTED` /
//! `ERR_IMPORT_ATTRIBUTE_TYPE_INCOMPATIBLE` / `ERR_INVALID_URL`）。`import.meta.url` 与错误
//! 文案里的 URL 是序列化后的形式（scheme 小写、非 ASCII 百分号编码）。
//!
//! 刻意偏差：jiti 2.7 自己的解析器只认小写 `data:`，`DATA:...` 会报
//! `Cannot find module 'DATA:...'`；本移植按 Node 的 URL 语义（scheme 大小写不敏感）接受它
//! （Resolver §3.8 第 0 步同样如此）。
//!
//! 名字不做任何规范化：QuickJS 的模块表按**精确字符串**去重，`Resolver` 返回什么名字，
//! 这里就用什么名字 `Module::declare`（§3.4 的 `?v=` 因此天然生效）。
//!
//! # `moduleCache: false` 的等价（§3.4）
//!
//! 扩展入口用带代际的名字声明：`<真实绝对路径>?v=<generation>-<seq>`，取自
//! `JsEngine::scoped_module_name()`。推荐走 [`ExtensionLoader::load_module`]（它同时装上
//! `import.meta` 与 CJS 桥）；门面若改用 [`rquickjs::Module::declare`] 直接声明，则
//! `import.meta` 需要门面自己按 §3.4 设置。同名文件带不同 query 是 QuickJS 模块表里的不同
//! 条目，因此各执行一次；入口里的**相对依赖**由 Resolver 解析成干净的绝对路径（不带 query），
//! 同一代内共享。`import.meta.url` 始终是去掉 query 的真实 `file://` URL。
//!
//! `KIANA_EXT_STRICT_NOCACHE=1`（Rust 侧新增的调试开关，TS 无对应物）把入口的 query 扩散到整棵
//! 依赖图：见 [`ScopedResolver`]——它装饰任意 `Resolver`，把「`base` 带 `?v=` 时解析出的绝对
//! 路径」也加上同一个 query。这必须在**解析阶段**做（QuickJS 的模块表按解析结果查表）；在
//! `Loader::load` 里改名会让同一依赖被每个 importer 各声明一份，破坏同一张图内的单实例语义。
//! 门面用 [`ExtensionLoader::resolver`] 取到带开关的包装器即可。
//!
//! 默认模式下，依赖图环回入口（`a → b → a`）时依赖里的 `./a.ts` 会解析回入口的干净路径。
//! [`ScopedResolver`] 记住「干净路径 → 当前代版本化名字」，把它改写成入口已声明的名字，
//! 因此入口体只执行一次（jiti 在同一个 jiti 实例内同样只执行一次）。**门面必须用
//! [`ExtensionLoader::resolver`] / [`ExtensionLoader::install_on`] 安装 resolver**，环回复用
//! 才生效。
//!
//! # 真 CJS（§3.5）
//!
//! CJS 文件的模块源是 `cjs::wrap_module` 生成的 ESM 包装器：`default` 是 CJS 加载器求值后的
//! `module.exports`，另带静态可识别的具名导出。包装器使用三个全局函数（`js/cjs.js`）：
//! `__kiana_cjs_acquire` / `__kiana_cjs_delete` / `__kiana_make_require`。本模块在第一个 CJS 模块加载前
//! 确保 `js/cjs.js` 已求值（[`ExtensionLoader::install_cjs_bridge`]），并在缺失时报出明确错误。
//!
//! `require` 本身是同步的，走宿主函数 `__kiana_host.resolve(from, spec)` / `__kiana_host.read(path)`
//! （由 loader-host 单元安装；本模块与其**没有编译期依赖**，只按名字约定）。递归 `require` 的
//! 全部逻辑在 `js/cjs.js` 里，本模块只提供包装器与桥的安装。宿主原语缺失时第一次调用就报
//! `ERR_PI_HOST_MISSING`（消息点名缺失的 `__kiana_host.*`），不伪装成 `MODULE_NOT_FOUND` /
//! `ERR_REQUIRE_ESM`（review finding 8）。
//!
//! CJS 模块体在 `Module::declare` 前先用与 `new Function` 相同的函数体形态做一次 oxc 解析：
//! 语法错误带上真实路径与行列（`ParseError: … at <path>:<line>:<column>`），而不是求值期只有
//! 句法片段的 QuickJS 文案（review finding 10）。
//!
//! 刻意偏差（review finding 9）：jiti 的 `moduleCache: false` 让 `require.cache` 恒为空对象，
//! 且每次 `require()` 都返回新的 interop proxy（`dep === require("./dep.cjs")` 为 false）。
//! 设计 §3.5 第 3/7 条要求以绝对路径为键的共享 `__kiana_cjs` 缓存与 `require.cache` 指向它，
//! 本实现按设计执行（单实例、缓存可见），测试里注明这是记录在案的偏差。
//!
//! 刻意偏差（review finding 10）：`import.meta.resolve` 在 jiti 里有两条路径——静态
//! `import.meta.resolve(x)` 被改写成 `jitiESMResolve(x)`（相对路径正常解析、裸包名走 jiti 的
//! 解析器），动态读取 `const m = import.meta; m.resolve(x)` 拿到的是 data: URL 模块的 Node 原生
//! `resolve`（相对路径抛 `Failed to resolve module specifier … Invalid relative URL or base
//! scheme is not hierarchical`）。本移植只有一种实现：走宿主解析器并返回正确的 file URL（与
//! 静态调用的主路径一致）。动态读取的 data: URL 形态与裸包名的 jiti 特例不复制。
//!
//! # 声明顺序（§3.9）
//!
//! QuickJS 在 `Module::declare` 时就把模块挂进 `ctx->loaded_modules`（按名字）。22 个虚拟模块
//! 与 node:* shim 必须在第一个扩展求值前声明完毕，否则依赖它们的模块级 `const` 会处于 TDZ
//! （`X is not initialized`）。[`ExtensionLoader::declare_all`] 一次性声明注册表里的全部源
//! （只编译，不求值）；之后 QuickJS 按名字查表，不再回调 `Loader::load`。门面按 §9.2 第 5 步
//! 调用它。
//!
//! # jiti 的四个分支（§2.2.1 的 TS 逻辑）
//!
//! TS 按运行形态选策略：Bun 二进制 / Node SEA（`jiti/static`）/ TS 源码（`tsconfigPaths`）/
//! 普通 Node（`getAliases()`）。Rust 只有**一种**策略：内嵌虚拟模块 + 自有 oxc 去类型 + 自有
//! 解析器，因此：
//! - `8237-node-sea-extension-loading.test.ts`（SEA 分支）与
//!   `9540-extension-loader-lazy.test.ts`（jiti/Babel 惰性加载）在 Rust 侧**不适用**：
//!   没有 jiti、没有 Babel，也没有「静态 vs 动态入口」的区分。它们锁定的可观察行为
//!   （扩展工厂只在真正导入扩展时才需要模块系统；虚拟模块对象身份共享）由本模块与
//!   `virtual_modules` 单元的单实例模块表覆盖。
//! - `getAliases()` 的 22 项在 Rust 侧没有对应物：别名被虚拟模块取代（Resolver 直接把键
//!   解析成虚拟模块名），所以不存在 `react`/`zod` 之类的宿主别名表。
//!
//! # 错误文案
//!
//! 用户可见的模块名一律用 `display_name`（去掉内部 `?v=<generation>-<seq>` 缓存键）；jiti/
//! Node 的文案里从不出现这个 query（review finding 2）。
//!
//! - 读文件失败/模块缺失：jiti/Node 的 `Cannot find module '<resolved>'` +
//!   `Require stack:` + `- <loader>`（`PrepareError::Message` 直接抛成 JS 异常；设计 §3.8 的
//!   `Error loading module …` 只保留给 `node:` 内建等 rquickjs `Loading` 路径）。
//! - 语法错误：jiti/Babel 形状的 `ParseError: <诊断>  \n <path>:<line>:<column>`
//!   （行 1 基、列 0 基；消息文本来自 oxc）。
//! - 未注册的内建：`Error loading module '<name>': No such built-in module: <name>`（Node 的
//!   `ERR_UNKNOWN_BUILTIN_MODULE` 文案）。
//! - 依赖的加载失败在父模块 `declare` 期间抛出时保持**一级**文案（`Cannot find module …` /
//!   `ParseError: …` / `Error loading module '<dep>': …`），不再叠加父模块前缀（review finding 9）。
//! - `data:` URL 的错误按 Node 的异常类型/`code` 抛出（见上）。
//!
//! # 阅读顺序
//!
//! 建议按 [`LoaderConfig`] → [`PreparedModule`] / [`ModuleKind`] →
//! [`ExtensionLoader::prepare`]（§3.1 流水线的唯一实现）→ [`ExtensionLoader::load_module`] →
//! `declare_module` / `install_import_meta`（文件内私有函数）→ [`ScopedResolver`] 的顺序读。
//! `data:` URL 解码与混写 ESM 改写是两条独立支线，可以最后再看。
//!
//! # 每 Context 的状态
//!
//! [`ExtensionLoader`] 的状态分两层：`sources` 注册表与 [`LoaderConfig`] 由同一 loader 的所有
//! 克隆句柄共享；`declared`（该名字已声明过）与 CJS 桥标志在语义上是**每 Context** 的。
//! QuickJS 的模块表、`js/cjs.js` 装到 `globalThis` 上的函数都属于某个 `Context`；句柄被复用
//! 到新引擎（`/reload` 的销毁→重建）时，`bind_context` 用符号键
//! `Symbol.for("kiana.loader.<id>.context")` 检测出「换了 Context」并清空这两项，迫使
//! [`ExtensionLoader::declare_all`] / [`ExtensionLoader::install_cjs_bridge`] 重做。漏掉这一步
//! 会静默复用旧的「已声明」标记，第一次 CJS/TS 求值才报
//! `__kiana_cjs_acquire is not a function`。

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};

use indexmap::IndexMap;
use oxc_allocator::Allocator;
use oxc_ast::AstKind;
use oxc_ast::ast::{
    BindingPattern, CallExpression, Declaration, ExportDefaultDeclarationKind, Expression,
    ImportOrExportKind, MemberExpression, ModuleExportName, Program, Statement, UnaryOperator,
};
use oxc_parser::{ParseOptions, Parser};
use oxc_semantic::{Semantic, SemanticBuilder};
use oxc_span::{GetSpan, SourceType};
use rquickjs::context::EvalOptions;
use rquickjs::loader::{ImportAttributes, Loader, Resolver};
use rquickjs::module::Declared;
use rquickjs::{Ctx, Error, Exception, Function, Module, Result, Symbol, Value};

use super::cjs::{self, CJS_JS_SOURCE};
use super::resolver::VirtualFs;
use super::transpile;

/// 把入口的 `?v=` query 扩散到整棵依赖图的环境变量（§3.4；Rust 新增，TS 无对应物）。
///
/// 值为 `1` 时开启。只影响 [`ScopedResolver`]，在 [`ExtensionLoader`] 构造时读取一次。
pub const STRICT_NOCACHE_ENV: &str = "KIANA_EXT_STRICT_NOCACHE";

/// Loader 配置。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LoaderConfig {
    /// 嵌入文件表（typebox），由 virtual-modules 单元填充（跨单元契约，见 `resolver` 模块文档）。
    pub virtual_fs: VirtualFs,
    /// `KIANA_EXT_STRICT_NOCACHE=1` 的等价开关（§3.4），作用于 [`ExtensionLoader::resolver`]。
    pub strict_nocache: bool,
}

impl LoaderConfig {
    /// 用给定虚拟文件表建配置；`strict_nocache` 读环境变量。
    pub fn new(virtual_fs: VirtualFs) -> Self {
        Self {
            virtual_fs,
            strict_nocache: strict_nocache_from_env(),
        }
    }
}

impl Default for LoaderConfig {
    /// 空虚拟表；`strict_nocache` 读 [`STRICT_NOCACHE_ENV`]（保证环境开关在任何构造路径下生效）。
    fn default() -> Self {
        Self::new(VirtualFs::empty())
    }
}

/// `KIANA_EXT_STRICT_NOCACHE` 的值 → 开关。只有 `1` 开启（§3.4 的文档写法）。
///
/// 纯函数版本便于测试：进程环境在并行测试里不可靠。
pub fn strict_nocache_from_value(value: Option<&str>) -> bool {
    value == Some("1")
}

/// 读 [`STRICT_NOCACHE_ENV`]（未设置/非 `1` 为关闭）。
pub fn strict_nocache_from_env() -> bool {
    strict_nocache_from_value(std::env::var(STRICT_NOCACHE_ENV).ok().as_deref())
}

/// 模块源的类别（诊断/测试用；也是流水线分支的标签）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ModuleKind {
    /// 注册的 shim / 虚拟模块源（`register_source`）。
    Embedded,
    /// VirtualFs 里的嵌入文件（typebox）。
    VirtualFile,
    /// `.ts`/`.tsx`/`.mts`/`.cts`/`.mtsx`/`.ctsx` 去类型后的 ESM。
    TypeScript,
    /// `.js`/`.mjs` 的 ESM 路径。源不一定是原文：转换分支可能注入 CJS 绑定，混写模块还会
    /// 被改写（最终文本以 [`PreparedModule::source`] 为准）。
    JavaScript,
    /// 真 CJS 包装器（`cjs::wrap_module`）。
    Cjs,
    /// `.json` → `export default JSON.parse(...)`。
    Json,
    /// `data:` URL。
    DataUrl,
}

/// [`ExtensionLoader::prepare_with`] 的内部错误：loading 错误与 data: URL 的 Node 错误分开，
/// 后者在真正加载时要抛成带 `code` 的 JS 异常。
enum PrepareError {
    Loading(Error),
    DataUrl(DataUrlError),
    /// 与 jiti/Node 逐字一致的裸消息（`Cannot find module …` / `ParseError: …`）。
    ///
    /// rquickjs 的 `Loading` 文案固定带 `Error loading module '<name>': ` 前缀，TS 的
    /// jiti 错误没有该前缀，因此这类错误用 JS 异常直接抛出（`Exception::throw_message`）。
    Message(String),
}

impl From<Error> for PrepareError {
    fn from(error: Error) -> Self {
        PrepareError::Loading(error)
    }
}

/// 一个已经按 §3.1 流水线准备好、可以被 `Module::declare` 的模块。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparedModule {
    name: String,
    source: String,
    url: Option<String>,
    kind: ModuleKind,
    /// 源里注入了 jiti 的 CJS 绑定（自由 `require`/`module`/…），求值前必须装 `js/cjs.js`。
    cjs_bindings: bool,
    /// 由 Node 原生 ESM 加载器求值的文件（`.mjs`，以及 `"type": "module"` 包里的 `.js`）。
    ///
    /// jiti 对这些文件先走 `import()`：只有求值期抛 `ReferenceError`（例如顶层 `require(...)`）
    /// 才回退到 Babel 的 CJS 转换。原生分支的 `import.meta` 是 Node 的五个键
    /// （`dirname, filename, main, resolve, url`），转换分支只有 `{main, resolve, url}`（review
    /// finding 3）；`jiti.import(..., {default:true})` 的互操作规则也随之不同。
    native_esm: bool,
}

impl PreparedModule {
    /// 模块名（与 `Module::declare` 的 name 逐字节一致，可能带 `?v=`）。
    pub fn name(&self) -> &str {
        &self.name
    }

    /// 交给 QuickJS 的 ESM 源文本。
    pub fn source(&self) -> &str {
        &self.source
    }

    /// `import.meta.url`；只有真实文件与 `data:` URL 才有值。
    pub fn url(&self) -> Option<&str> {
        self.url.as_deref()
    }

    /// 命中的流水线分支。
    pub fn kind(&self) -> ModuleKind {
        self.kind
    }

    /// 是否走真 CJS 包装器。
    pub fn is_cjs(&self) -> bool {
        self.kind == ModuleKind::Cjs
    }

    /// 是否由 Node 原生 ESM 加载器求值（决定 `import.meta` 的键集，见 `install_import_meta`）。
    pub fn is_native_esm(&self) -> bool {
        self.native_esm
    }

    /// 源里是否存在需要 `js/cjs.js` 全局桥的绑定（CJS 包装器、TS 注入、`.js/.mjs` 注入）。
    pub fn needs_cjs_bridge(&self) -> bool {
        needs_cjs_bridge(self.kind) || self.cjs_bindings
    }
}

/// `data:` URL 的接受/拒绝集合（与 Node 的 ESM `data:` 加载器一致）。
///
/// [`ExtensionLoader::prepare`] 没有 `Ctx`，把失败映射成 rquickjs 的 loading 错误；
/// [`ExtensionLoader::load_module`] 用 [`DataUrlError::throw`] 抛出带 Node `code` 的 JS 异常。
#[derive(Debug, Clone, PartialEq, Eq)]
enum DataUrlError {
    /// `Unknown module format: <mime|null> for URL <url>`（RangeError，
    /// `ERR_UNKNOWN_MODULE_FORMAT`）。
    UnknownFormat { url: String, mime: Option<String> },
    /// `Module "<url>" needs an import attribute of "type: json"`（TypeError，
    /// `ERR_IMPORT_ATTRIBUTE_MISSING`）。
    MissingJsonAttribute { url: String },
    /// `Import attribute "type" with value "<value>" is not supported in <url>`（TypeError，
    /// `ERR_IMPORT_ATTRIBUTE_UNSUPPORTED`）。
    UnsupportedAttribute { url: String, value: String },
    /// `Module "<url>" is not of type "json"`（TypeError，
    /// `ERR_IMPORT_ATTRIBUTE_TYPE_INCOMPATIBLE`）。
    IncompatibleAttribute { url: String },
    /// Node 的 URL 解析失败：`Invalid URL`（TypeError，`ERR_INVALID_URL`）。
    InvalidUrl,
}

impl DataUrlError {
    /// Node 的 `code` 属性。
    fn code(&self) -> &'static str {
        match self {
            DataUrlError::UnknownFormat { .. } => "ERR_UNKNOWN_MODULE_FORMAT",
            DataUrlError::MissingJsonAttribute { .. } => "ERR_IMPORT_ATTRIBUTE_MISSING",
            DataUrlError::UnsupportedAttribute { .. } => "ERR_IMPORT_ATTRIBUTE_UNSUPPORTED",
            DataUrlError::IncompatibleAttribute { .. } => "ERR_IMPORT_ATTRIBUTE_TYPE_INCOMPATIBLE",
            DataUrlError::InvalidUrl => "ERR_INVALID_URL",
        }
    }

    /// Node 的构造函数名（`RangeError` / `TypeError`）。
    fn constructor(&self) -> &'static str {
        match self {
            DataUrlError::UnknownFormat { .. } => "RangeError",
            _ => "TypeError",
        }
    }

    /// 逐字错误文案（Node 26 实测）。
    fn message(&self) -> String {
        match self {
            DataUrlError::UnknownFormat { url, mime } => {
                let mime = mime.as_deref().unwrap_or("null");
                format!("Unknown module format: {mime} for URL {url}")
            }
            DataUrlError::MissingJsonAttribute { url } => {
                format!("Module \"{url}\" needs an import attribute of \"type: json\"")
            }
            DataUrlError::UnsupportedAttribute { url, value } => format!(
                "Import attribute \"type\" with value \"{value}\" is not supported in {url}"
            ),
            DataUrlError::IncompatibleAttribute { url } => {
                format!("Module \"{url}\" is not of type \"json\"")
            }
            DataUrlError::InvalidUrl => "Invalid URL".to_string(),
        }
    }

    /// `prepare` 用的 rquickjs loading 文案（data: URL 以自身为「模块名」）。
    fn loading_error(&self, name: &str) -> Error {
        Error::new_loading_message(name, self.message())
    }

    /// 抛成 JS 异常（带 `code`），与 Node 的错误对象形状一致。
    fn throw<'js>(&self, ctx: &Ctx<'js>) -> Error {
        let message = self.message();
        let constructor = self.constructor();
        let code = self.code();
        let result = (|| -> Result<Error> {
            // rquickjs 的 `Function` 没有 `new`；用一个一次性工厂函数构造正确的错误类型。
            let factory: Function = ctx.eval(format!(
                "(message, code) => {{ const error = new {constructor}(message); error.code = code; return error; }}"
            ))?;
            let error: Value = factory.call((message.clone(), code))?;
            Ok(ctx.throw(error))
        })();
        // 工厂本身求值/调用失败时退回无 `code` 的普通异常：至少保留 Node 的文案，而不是再抛
        // 一个无关的 rquickjs 错误。
        result.unwrap_or_else(|_| Exception::throw_message(ctx, &message))
    }
}

/// `rquickjs::Loader` 实现（§3.1），同时是门面共享的模块源注册表。
///
/// 内部是 `Rc<RefCell<..>>`：`Runtime::set_loader` 会拿走 loader 的所有权，门面要能继续
/// 注册源/预声明/声明入口模块，所以 [`ExtensionLoader`] 是**可克隆的句柄**，克隆共享同一份状态。
///
/// ```ignore
/// let loader = ExtensionLoader::new(LoaderConfig::new(virtual_fs));
/// loader.register_sources(node_shim_sources);
/// engine.with_js(|ctx| loader.declare_all(&ctx))?;      // §9.2 第 5 步
/// engine.set_loader(loader.resolver(ModuleResolver::new(opts)), loader.clone());
/// // 加载扩展入口：
/// engine.with_js(|ctx| {
///     let name = engine.scoped_module_name(resolved_path);
///     let module = loader.load_module(&ctx, &name)?;
///     let (module, promise) = module.eval()?;
///     promise.finish::<()>()?;
///     let default: Value = module.get("default")?;
///     // typeof default === "function" 检查之后由门面调用工厂
///     Ok(default)
/// })?;
/// ```
#[derive(Clone)]
pub struct ExtensionLoader {
    inner: Rc<RefCell<LoaderState>>,
}

/// [`ExtensionLoader`] 的共享状态；所有克隆句柄通过 `Rc<RefCell<..>>` 看到同一份。
///
/// 不变量：`sources` 用 `IndexMap` 保留注册顺序（`declare_all` 按它声明）；`declared`
/// 记录**当前 Context** 里已经/正在声明的名字，声明失败必须移除（见
/// [`ExtensionLoader::declare_source`]），否则重试会被静默跳过；`token == 0` 表示本句柄尚未
/// 绑定任何 Context。
struct LoaderState {
    config: LoaderConfig,
    /// 注册的模块源，按注册顺序（`declare_all` 的顺序因此确定）。
    sources: IndexMap<String, String>,
    /// `declare_all` 已声明过的名字（重复声明会在 QuickJS 里造出重复模块定义）。
    declared: HashSet<String>,
    /// `js/cjs.js` 是否已在本 Context 求值。
    cjs_bridge: bool,
    /// 本句柄的唯一 id（Context 标记的符号键用它区分多个句柄）。
    id: u64,
    /// 每绑定一个新 Context 递增；`bound` 是当前 Context 里记录的值。
    ///
    /// 不能只用 `Ctx` 指针判断：`/reload` 销毁旧引擎后，新 `JSContext` 可能拿到同一个
    /// 分配地址。这里把记号写在 `globalThis[Symbol.for("kiana.loader.<id>.context")]`（不可枚举，
    /// 不像普通全局那样出现在 `Object.keys(globalThis)` 里），新 Context 里没有该符号属性，
    /// 因此一定能识别出「换了 Context」并复位 per-Context 状态（review finding 3）。
    token: u32,
    bound: u32,
}

/// [`LoaderState::id`] 的进程内计数器。
static NEXT_LOADER_ID: AtomicU64 = AtomicU64::new(1);

impl Default for ExtensionLoader {
    fn default() -> Self {
        Self::new(LoaderConfig::default())
    }
}

impl ExtensionLoader {
    /// 新建 loader。`strict_nocache` 由调用方决定（[`LoaderConfig::new`] 读环境变量）。
    pub fn new(config: LoaderConfig) -> Self {
        Self {
            inner: Rc::new(RefCell::new(LoaderState {
                config,
                sources: IndexMap::new(),
                declared: HashSet::new(),
                cjs_bridge: false,
                id: NEXT_LOADER_ID.fetch_add(1, Ordering::Relaxed),
                token: 0,
                bound: 0,
            })),
        }
    }

    /// 把 per-Context 状态绑定到 `ctx`；换 Context 时复位（`declared` 与 `cjs_bridge`）。
    ///
    /// QuickJS 的模块表与 `js/cjs.js` 的全局桥都是每 Context 的；句柄被克隆后装到新引擎
    /// （`/reload` 的销毁→重建）上时必须重新声明/重新求值，否则 `declare_all` 会静默跳过、
    /// 第一次 CJS/TS 求值报 `__kiana_cjs_acquire is not a function`（review finding 3）。
    fn bind_context(&self, ctx: &Ctx<'_>) -> Result<()> {
        // 借用只覆盖两个标量的读取：`RefCell` 的借用不可重入，不能跨后面的 JS 调用持有。
        let (id, bound) = {
            let state = self.inner.borrow();
            (state.id, state.bound)
        };
        // 不可枚举的 Context 标记（见 `LoaderState::token`）。
        let marker = Symbol::new_global(ctx.clone(), format!("kiana.loader.{id}.context"))?;
        let current: Option<u32> = ctx.globals().get(marker.clone())?;
        if bound != 0 && current == Some(bound) {
            return Ok(());
        }
        // `bound == 0`（从未绑定）或标记值不同（新 Context）都会走到这里：递增 token 并清空
        // per-Context 状态；同一 Context 的重复调用在上面提前返回。
        let mut state = self.inner.borrow_mut();
        state.token = state.token.wrapping_add(1);
        state.bound = state.token;
        state.declared.clear();
        state.cjs_bridge = false;
        let token = state.bound;
        drop(state);
        ctx.globals().set(marker, token)?;
        Ok(())
    }

    /// 配置（拷贝）。
    pub fn config(&self) -> LoaderConfig {
        self.inner.borrow().config
    }

    /// 嵌入文件表。
    pub fn virtual_fs(&self) -> VirtualFs {
        self.inner.borrow().config.virtual_fs
    }

    /// `KIANA_EXT_STRICT_NOCACHE` 开关当前是否打开。
    pub fn strict_nocache(&self) -> bool {
        self.inner.borrow().config.strict_nocache
    }

    /// 用当前开关装饰一个 `Resolver`（§3.4 的严格去缓存）。开关关闭时行为与内层完全一致。
    pub fn resolver<R>(&self, inner: R) -> ScopedResolver<R> {
        ScopedResolver::new(inner, self.strict_nocache())
    }

    /// 注册一个模块源（虚拟模块键、node:* shim）。返回被替换的旧源。
    ///
    /// 同名重复注册以最后一次为准（与 `JsEngine::register_module_source` 一致）。
    pub fn register_source(
        &self,
        name: impl Into<String>,
        source: impl Into<String>,
    ) -> Option<String> {
        self.inner
            .borrow_mut()
            .sources
            .insert(name.into(), source.into())
    }

    /// 批量注册；顺序保留（`declare_all` 按注册顺序声明）。
    pub fn register_sources<I, N, S>(&self, sources: I)
    where
        I: IntoIterator<Item = (N, S)>,
        N: Into<String>,
        S: Into<String>,
    {
        let mut state = self.inner.borrow_mut();
        for (name, source) in sources {
            state.sources.insert(name.into(), source.into());
        }
    }

    /// 已注册的源（克隆；源文本一般只有几十 KB）。
    pub fn source(&self, name: &str) -> Option<String> {
        self.inner.borrow().sources.get(name).cloned()
    }

    /// 删除一个已注册的源，返回旧值。
    pub fn remove_source(&self, name: &str) -> Option<String> {
        self.inner.borrow_mut().sources.shift_remove(name)
    }

    /// 已注册的源数量。
    pub fn source_count(&self) -> usize {
        self.inner.borrow().sources.len()
    }

    /// 已注册的源名字，按注册顺序。
    pub fn source_names(&self) -> Vec<String> {
        self.inner.borrow().sources.keys().cloned().collect()
    }

    /// 按 §3.1 把模块名解析成可声明的源文本（不接触 QuickJS，可用于诊断与测试）。
    ///
    /// `name` 是 `Resolver` 的输出：虚拟键、`node:<name>`、`data:` URL 或绝对文件路径
    /// （可带 `?v=<generation>-<seq>`）。`data:` URL 的 import 属性按「没有属性」处理；
    /// 带属性的真实加载走 [`ExtensionLoader::load_module`]。
    pub fn prepare(&self, name: &str) -> Result<PreparedModule> {
        self.prepare_with(name, None).map_err(|error| match error {
            PrepareError::Loading(error) => error,
            PrepareError::DataUrl(error) => error.loading_error(name),
            // 诊断 API 没有 `Ctx`：退回 rquickjs 的 loading 文案（真正的加载路径用裸消息）。
            PrepareError::Message(message) => Error::new_loading_message(name, message),
        })
    }

    /// [`ExtensionLoader::prepare`] 的带 import 属性版本。
    ///
    /// `attributes` 只影响 `data:` URL（Node 对 `application/json` 要求 `with { type: "json" }`）；
    /// 文件模块的 JSON 与 jiti 一样两种写法都接受。
    fn prepare_with<'js>(
        &self,
        name: &str,
        attributes: Option<&ImportAttributes<'js>>,
    ) -> std::result::Result<PreparedModule, PrepareError> {
        // 1. 注册的 shim / 虚拟模块源。它们永远是 ESM 原文（§3.3/§6 的源由 U10/U3 提供）。
        if let Some(source) = self.source(name) {
            return Ok(PreparedModule {
                name: name.to_string(),
                source: transpile::strip_bom(&source).to_string(),
                url: None,
                kind: ModuleKind::Embedded,
                cjs_bindings: false,
                native_esm: false,
            });
        }

        // 2. 嵌入文件（typebox）。键名就是模块名，原样 declare 才能共享模块缓存与
        //    `Symbol.for("TypeBox.Kind")`（§3.6）。
        if let Some(bytes) = self.virtual_fs().lookup(name) {
            let text = String::from_utf8_lossy(bytes);
            return Ok(PreparedModule {
                name: name.to_string(),
                source: transpile::strip_bom(&text).to_string(),
                url: None,
                kind: ModuleKind::VirtualFile,
                cjs_bindings: false,
                native_esm: false,
            });
        }

        // 3. data: URL（Resolver 原样返回，§3.8 第 0 步「由 loader 解码 payload」）。
        //    方案名大小写不敏感（WHATWG URL；Node 接受 `DATA:`）。
        if let Some(rest) = strip_data_scheme(name) {
            let attribute_type = match attributes {
                Some(attributes) => attributes.get_type()?,
                None => None,
            };
            return prepare_data_url(name, rest, attribute_type).map_err(PrepareError::DataUrl);
        }

        // 4. 22 个 kiana-* 虚拟键没有文件形态：源必须由 virtual-modules 单元注册。
        if super::resolver::is_virtual_module_key(name) {
            return Err(PrepareError::Loading(Error::new_loading_message(
                display_name(name),
                "No source registered for this virtual module",
            )));
        }

        // 5. Node 内建：shim 源由 loader-host 注册；没有注册时按 Node 的
        //    ERR_UNKNOWN_BUILTIN_MODULE 文案报错（Rust 侧的 shim 表比 Node 小，§6.1）。
        if name.starts_with("node:") {
            return Err(PrepareError::Loading(Error::new_loading_message(
                display_name(name),
                format!("No such built-in module: {name}"),
            )));
        }

        // 6. 文件模块。
        let real = transpile::module_path(name);
        // 用户可见的错误只带真实路径：`?v=<gen>-<seq>` 是内部缓存键，jiti/Node 的文案里没有
        // 它（review finding 2）。
        // 任何读失败（不存在、权限不足、路径中间是文件等）都折叠成这一条文案，具体 errno
        // 被丢弃；`from_utf8_lossy` 则把非法 UTF-8 字节替换成 U+FFFD，而不是报错。
        let bytes = std::fs::read(real).map_err(|_| {
            // jiti/Node: `Cannot find module '<resolved>'>` + Require stack（调用方是 loader）。
            PrepareError::Message(format!(
                "Cannot find module '{real}'\nRequire stack:\n- {}",
                loader_require_stack_path()
            ))
        })?;
        let text = String::from_utf8_lossy(&bytes);
        let raw = transpile::strip_bom(&text);
        let url = file_url(real);

        // JSON：jiti 把 JSON 当 require 的目标（`default` 是解析后的对象，顶层键同时是具名
        // 导出），Node 的 ESM JSON 模块只有 `default`。具名导出在链接期必须存在，所以从
        // Rust 侧解析出的顶层键生成静态 `export { … as … }`（review finding 1）。
        if real.ends_with(".json") {
            return Ok(PreparedModule {
                name: name.to_string(),
                source: json_module_source(raw, true),
                url,
                kind: ModuleKind::Json,
                cjs_bindings: false,
                native_esm: false,
            });
        }

        // `package.json` 的 `type` 只影响 `.js`（Node 的原生分派）。
        let package_type = if real.ends_with(".js") {
            cjs::package_type_for(Path::new(real))
        } else {
            None
        };

        // 分派顺序与 jiti/Node 一致：TS 先去类型（`is_cjs_module` 仍看**原始**源），其余先做
        // CJS 探测；只有非 CJS 的 `.js`/`.mjs` 才继续走 ESM 分支。
        let mut native_esm = false;
        let (source, kind, cjs_bindings) = if transpile::is_typescript(real) {
            let stripped = transpile::strip_types(name, raw)
                .map_err(|error| transpile_error(display_name(name), error))?;
            if cjs::is_cjs_module(name, raw, package_type.as_deref()) {
                (cjs::wrap_module(name, &stripped), ModuleKind::Cjs, false)
            } else {
                // jiti 用 Babel 把 ESM 语法改写成对同一个 module 记录的 CJS 写入；因此
                // `export …` 与 `module.exports = …` 混写时默认导入是重新赋值后的
                // `module.exports`（review finding 1）。没有 CJS 写入时原样返回。
                let source = rewrite_mixed_esm_exports(name, &stripped).unwrap_or(stripped);
                (source, ModuleKind::TypeScript, false)
            }
        } else if cjs::is_cjs_module(name, raw, package_type.as_deref()) {
            // `new Function` 的语法错误没有模块名与位置（review finding 10）；在 Rust 侧用与
            // `new Function` 相同的「函数体」形态预解析，把诊断连同真实路径/行列带出来。
            if let Some(diagnostic) = cjs_parse_error(name, raw) {
                return Err(PrepareError::Loading(Error::new_loading_message(
                    display_name(name),
                    diagnostic,
                )));
            }
            (cjs::wrap_module(name, raw), ModuleKind::Cjs, false)
        } else {
            // jiti 对 `.mjs` 与 `"type": "module"` 包的 `.js` 先走 Node 原生 `import()`：只有
            // 求值期抛 `ReferenceError`（顶层 `require(...)` 之类）才回退到 Babel 的 CJS 转换。
            // 延迟引用（函数体里的 `require`、`typeof require`）在原生存活，jiti 不会注入包装器
            // 参数，所以这里也不能注入（review finding 4）。其余 `.js` 永远经 jiti 转换，包装器
            // 参数总是可用。
            let native = real.ends_with(".mjs")
                || (real.ends_with(".js") && package_type.as_deref() == Some("module"));
            // jiti 对原生 ESM 也先用 Babel 解析：语法错误按 `ParseError: …` 报出
            // （QuickJS 的 SyntaxError 文案与行列口径不同）。
            if native && let Some(message) = transpile::parse_error_message(real, raw) {
                return Err(PrepareError::Message(format!("ParseError: {message}")));
            }
            let inject = !native || eager_cjs_reference(real, raw);
            let (source, cjs_bindings) = if inject {
                match inject_esm_cjs_bindings(name, raw) {
                    Some(source) => (source, true),
                    None => (raw.to_string(), false),
                }
            } else {
                (raw.to_string(), false)
            };
            if native && !cjs_bindings {
                native_esm = true;
                (source, ModuleKind::JavaScript, false)
            } else {
                let source = rewrite_mixed_esm_exports(name, &source).unwrap_or(source);
                (source, ModuleKind::JavaScript, cjs_bindings)
            }
        };

        Ok(PreparedModule {
            name: name.to_string(),
            source,
            url,
            kind,
            cjs_bindings,
            native_esm,
        })
    }

    /// 声明一个模块（`Loader::load` 的实现本体；门面加载扩展入口也用它）。
    ///
    /// `name` 可以是带 `?v=` 的入口名：声明出的模块名与 `name` 逐字节一致，`import.meta.url`
    /// 用去掉 query 的真实文件 URL。
    pub fn load_module<'js>(&self, ctx: &Ctx<'js>, name: &str) -> Result<Module<'js, Declared>> {
        self.load_module_with(ctx, name, None)
    }

    /// [`ExtensionLoader::load_module`] 的带 import 属性版本（`Loader::load` 的实现本体）。
    fn load_module_with<'js>(
        &self,
        ctx: &Ctx<'js>,
        name: &str,
        attributes: Option<&ImportAttributes<'js>>,
    ) -> Result<Module<'js, Declared>> {
        // 顺序固定：先绑定 Context 并生成源（纯 Rust），再确保 CJS 桥可用，最后 declare 并
        // 写 `import.meta`。桥只在**求值**时被包装器/注入代码调用；放在 declare 之前是为了让
        // 随后由 declare 递归加载（编译）出来的 CJS 依赖在求值时也能拿到全局函数。
        self.bind_context(ctx)?;
        let prepared = self
            .prepare_with(name, attributes)
            .map_err(|error| match error {
                PrepareError::Loading(error) => error,
                // Node 的 data: URL 错误是带 code 的 JS 异常，不是 rquickjs 的 loading 错误。
                PrepareError::DataUrl(error) => error.throw(ctx),
                // 裸消息（jiti 的 `Cannot find module …` / `ParseError: …`）直接作为异常文案。
                PrepareError::Message(message) => Exception::throw_message(ctx, &message),
            })?;
        if prepared.needs_cjs_bridge() {
            // §3.5：CJS 包装器与注入的 `require` 绑定都依赖 js/cjs.js 的全局函数；
            // 缺了要给出明确错误而不是 `undefined is not a function`。
            self.install_cjs_bridge(ctx)?;
        }

        let PreparedModule {
            name: module_name,
            source,
            url,
            kind,
            native_esm,
            ..
        } = prepared;
        let module = declare_module(ctx, &module_name, source)?;
        install_import_meta(ctx, &module, &module_name, url.as_deref(), kind, native_esm)?;
        Ok(module)
    }

    /// 求值 `js/cjs.js`（幂等）：装上 `__kiana_cjs_acquire` / `__kiana_cjs_delete` /
    /// `__kiana_make_require` 与 JS 侧 CJS 缓存（§3.5）。
    ///
    /// 递归 `require` 只依赖 `__kiana_host.resolve(from, spec)` 与 `__kiana_host.read(path)`
    /// （loader-host 单元安装）。宿主原语缺失时 `js/cjs.js` 在**第一次调用**它们时抛出明确的
    /// `ERR_PI_HOST_MISSING`（不会伪装成 `MODULE_NOT_FOUND`），不影响不 require 的 CJS 模块。
    ///
    /// 同时安装 `globalThis.__kiana_module_global_paths`（Node 的全局模块目录）供
    /// `require.resolve.paths` 使用（[`node_global_paths`]）。
    pub fn install_cjs_bridge<'js>(&self, ctx: &Ctx<'js>) -> Result<()> {
        self.bind_context(ctx)?;
        if self.inner.borrow().cjs_bridge {
            // 本 Context 里桥已就位（上次安装成功或探测到全局函数时置位；换 Context 时
            // `bind_context` 会把它清回 false）。
            return Ok(());
        }
        let globals = ctx.globals();
        let existing: Value = globals.get("__kiana_cjs_acquire")?;
        if existing.is_function() {
            // 桥挂在 `globalThis` 上：同一 Context 里已经有 `__kiana_cjs_acquire`（例如另一个
            // [`ExtensionLoader`] 装过）时直接复用。这里不会重新求值 `js/cjs.js`，因此
            // `__kiana_module_global_paths` 保持对方写下的值。
            self.inner.borrow_mut().cjs_bridge = true;
            return Ok(());
        }
        globals.set("__kiana_module_global_paths", node_global_paths())?;

        // 以全局脚本求值：`js/cjs.js` 只定义全局函数与缓存，不导出模块；filename 仅用于
        // 错误/堆栈显示。
        let mut options = EvalOptions::default();
        options.global = true;
        options.filename = Some("kiana:cjs".to_string());
        match ctx.eval_with_options::<(), _>(CJS_JS_SOURCE, options) {
            Ok(()) => {
                self.inner.borrow_mut().cjs_bridge = true;
                Ok(())
            }
            Err(error) => {
                // 求值失败时 QuickJS 的异常对象处于挂起状态：先 `ctx.catch()` 取走它，否则拼出
                // 来的消息只有 rquickjs 的通用文案。
                let message = if error.is_exception() {
                    let exception = ctx.catch();
                    exception
                        .as_exception()
                        .and_then(|exception| exception.message())
                        .unwrap_or_else(|| error.to_string())
                } else {
                    error.to_string()
                };
                Err(Error::new_loading_message(
                    "js/cjs.js",
                    format!("CJS require bridge failed to install: {message}"),
                ))
            }
        }
    }

    /// 声明注册表里的全部模块源（只编译、不求值；§3.9 的声明顺序）。
    ///
    /// 返回本次新声明的数量。之后 QuickJS 按名字查 `ctx->loaded_modules`，不会再回调
    /// `Loader::load`，所以依赖这些模块的扩展不会在求值期才遇到 TDZ。单次求值的源
    /// （typebox）不在这里声明：它们是惰性加载的独立文件（§3.6）。
    pub fn declare_all<'js>(&self, ctx: &Ctx<'js>) -> Result<usize> {
        self.bind_context(ctx)?;
        // 先快照名字：`declare_source` 会再次借用同一个 RefCell，遍历时不能持有 `sources`。
        let names = self.source_names();
        let mut declared = 0;
        for name in names {
            if self.declare_source(ctx, &name)? {
                declared += 1;
            }
        }
        Ok(declared)
    }

    /// 声明单个已注册的源；未注册或已声明返回 `false`。
    pub fn declare_source<'js>(&self, ctx: &Ctx<'js>, name: &str) -> Result<bool> {
        self.bind_context(ctx)?;
        let Some(source) = self.source(name) else {
            return Ok(false);
        };
        // 同一名字只声明一次：`declared` 里已有该名字时返回 `false`，避免对同一名字重复
        // `declare_all`/`declare_source` 在 QuickJS 里造出重复模块定义；编译失败则移除占位，
        // 保证下一次重试真的会重试。循环依赖由 QuickJS 的已加载模块表处理，不经过本函数。
        if !self.inner.borrow_mut().declared.insert(name.to_string()) {
            return Ok(false);
        }
        match declare_module(ctx, name, source) {
            Ok(_) => Ok(true),
            Err(error) => {
                // 失败不要留下「已声明」标记，否则重试会静默跳过。
                self.inner.borrow_mut().declared.remove(name);
                Err(error)
            }
        }
    }

    /// 注册 + 一次性声明（§9.2 第 5 步的便捷形式，`declare_all(&self, sources)`）。
    pub fn declare_sources<'js, I, N, S>(&self, ctx: &Ctx<'js>, sources: I) -> Result<usize>
    where
        I: IntoIterator<Item = (N, S)>,
        N: Into<String>,
        S: Into<String>,
    {
        self.register_sources(sources);
        self.declare_all(ctx)
    }

    /// §9.2 第 4–5 步的 loader 侧安装：CJS 桥 + 全部注册源（两步都幂等，可重复调用）。
    pub fn install<'js>(&self, ctx: &Ctx<'js>) -> Result<usize> {
        self.install_cjs_bridge(ctx)?;
        self.declare_all(ctx)
    }

    /// 把本 loader 与 `resolver` 一起装到引擎上（`Runtime::set_loader` 的便捷形式）。
    ///
    /// 用同一个 [`ExtensionLoader`] 句柄包装 resolver，保证 [`ScopedResolver`] 的严格开关与
    /// 实际加载用的是同一份状态；门面仍应保留该句柄用于 `declare_all` / 入口声明。
    /// **必须在 `with_js` 之外调用**（见 `JsEngine::set_loader`）。
    pub fn install_on<R>(&self, engine: &crate::engine::JsEngine, resolver: R)
    where
        R: Resolver + 'static,
    {
        engine.set_loader(self.resolver(resolver), self.clone());
    }
}

// QuickJS 在链接/加载模块时回调这里的 `load`：`name` 是 Resolver 的输出，`attributes` 来自
// 源码里的 `import … with { type: "json" }`（已由 rquickjs 解析）。实现与门面的
// [`ExtensionLoader::load_module`] 走同一条 `load_module_with`。
impl Loader for ExtensionLoader {
    fn load<'js>(
        &mut self,
        ctx: &Ctx<'js>,
        name: &str,
        attributes: Option<ImportAttributes<'js>>,
    ) -> Result<Module<'js, Declared>> {
        self.load_module_with(ctx, name, attributes.as_ref())
    }
}

/// 带 §3.4 严格去缓存的 `Resolver` 装饰器。
///
/// 默认（关闭）时透明转发。开启时：若导入者模块名带 `?v=<generation>-<seq>`（入口名），
/// 且内层解析出的是**绝对文件路径**（不是虚拟键、`node:`、`data:`，也不带 query），则把同一个
/// query 加到解析结果上。效果：入口的整棵文件依赖图按代重新执行，而 QuickJS 的模块表仍按
/// 解析结果去重（同一张图内共享单实例）。
///
/// 只在解析阶段改名才能同时满足「整图重跑」和「图内单实例」：在 `Loader::load` 里改名会让
/// QuickJS 找不到已声明的模块（查表用的是解析结果），每个 importer 各造一份重复定义。
///
/// # 环回入口（review finding 2）
///
/// 默认模式下来自某个版本化入口（`<path>?v=<gen>-<seq>`）的解析结果会记住「干净路径 → 带
/// query 的名字」。当依赖图里某个模块又导入回入口本身（`a → b → a`）时，返回入口已声明的
/// 版本化名字，QuickJS 因此复用同一个模块实例（jiti 在一个 jiti 实例内同样只执行一次）。
/// 没有这层映射时，`a?v=1-1` 与 `./a.ts` 是两个模块表条目，入口体会执行两次。
///
/// 支持任意 `Resolver` 装饰；入口的 `Module::declare` 名字必须带 `?v=`（门面用
/// `JsEngine::scoped_module_name`），解析入口的静态导入时 `base` 就是该名字。
#[derive(Debug, Clone)]
pub struct ScopedResolver<R> {
    /// 被装饰的解析器；每次解析先交给它，错误原样上抛。
    inner: R,
    /// `KIANA_EXT_STRICT_NOCACHE=1`（见 [`STRICT_NOCACHE_ENV`]）时为真。
    strict: bool,
    /// 干净绝对路径 → 当前代已声明的版本化模块名。
    entries: HashMap<String, String>,
}

impl<R> ScopedResolver<R> {
    /// 包装 `inner`；`strict` 对应 `KIANA_EXT_STRICT_NOCACHE=1`。
    pub fn new(inner: R, strict: bool) -> Self {
        Self {
            inner,
            strict,
            entries: HashMap::new(),
        }
    }

    /// 已记住的入口映射（诊断/测试用）：干净路径 → 版本化名字。
    pub fn entry_aliases(&self) -> &HashMap<String, String> {
        &self.entries
    }

    /// 内层 resolver 的只读引用。
    pub fn inner(&self) -> &R {
        &self.inner
    }

    /// 内层 resolver 的可变引用。
    pub fn inner_mut(&mut self) -> &mut R {
        &mut self.inner
    }

    /// 取回内层 resolver。
    pub fn into_inner(self) -> R {
        self.inner
    }

    /// 严格开关。
    pub fn is_strict(&self) -> bool {
        self.strict
    }
}

impl<R: Resolver> Resolver for ScopedResolver<R> {
    fn resolve<'js>(
        &mut self,
        ctx: &Ctx<'js>,
        base: &str,
        name: &str,
        attributes: Option<ImportAttributes<'js>>,
    ) -> Result<String> {
        // 先让内层解析器完成全部工作（相对路径、node_modules、虚拟键都在那里处理）；本层只在
        // 结果上做两种纯字符串改写。
        let resolved = self.inner.resolve(ctx, base, name, attributes)?;
        if let Some(query) = version_query(base) {
            // 入口（或严格模式下的任意文件模块）以版本化名字出现在 `base`：记住它的干净路径。
            let clean = transpile::module_path(base);
            if Path::new(clean).is_absolute() {
                self.entries.insert(clean.to_string(), base.to_string());
            }
            // 严格模式：只给**绝对文件路径**追加 query。虚拟键、`node:`、`data:` 与已经带
            // query 的结果保持原样（它们不是按代重新求值的文件模块）。
            if self.strict
                && version_query(&resolved).is_none()
                && Path::new(&resolved).is_absolute()
            {
                return Ok(format!("{resolved}?{query}"));
            }
        }
        // 依赖图环回入口：用入口已声明的版本化名字，复用同一个模块实例。
        if version_query(&resolved).is_none()
            && let Some(entry) = self.entries.get(&resolved)
        {
            return Ok(entry.clone());
        }
        Ok(resolved)
    }
}

/// `name` 里的 `?v=<generation>-<seq>` query（返回值形如 `"v=3-7"`）。
///
/// 只认 loader 自己的缓存键：普通文件名里的 `?`（unix 合法字符）不参与。
fn version_query(name: &str) -> Option<&str> {
    let (_, query) = name.split_once('?')?;
    query.starts_with("v=").then_some(query)
}

/// 这个模块是否需要 `js/cjs.js` 的全局桥。
///
/// - CJS 包装器直接调用 `__kiana_cjs_acquire` / `__kiana_cjs_delete` / `__kiana_make_require`；
/// - TS 模块（`transpile::strip_types`）在检测到自由 `require` 时会注入
///   `var require = globalThis.__kiana_make_require(<real>, <dir>)`
///   （`type: module` 里的 `notify.ts` 就是这样调用 `require("child_process")` 的）。
///
/// `.js`/`.mjs` ESM 在注入 CJS 绑定时由 [`PreparedModule::needs_cjs_bridge`] 追加判断。
///
/// 这里按 `kind` 保守判定：所有 `ModuleKind::TypeScript` 都返回 `true`，即使该文件没有自由
/// `require`（`transpile::strip_types` 只在有未解析绑定时才生成前导，且要求输出仍是 ESM）。
/// 重复安装桥是幂等的（[`ExtensionLoader::install_cjs_bridge`] 会提前返回），因此不需要额外
/// 的标志。
fn needs_cjs_bridge(kind: ModuleKind) -> bool {
    matches!(kind, ModuleKind::Cjs | ModuleKind::TypeScript)
}

/// `Module::declare`，失败时保证错误里出现模块名。
///
/// 这里的 `Module::declare` 在编译期就会解析静态导入（本 QuickJS 构建），所以依赖的加载失败
/// 会作为**异常**从 `declare` 抛回。依赖自己的错误已经是 rquickjs 的一级文案
/// `Error loading module '<dep>': …`：原样把异常重新抛出，别再加一层父模块前缀
/// （review finding 9：`Error loading module '<entry>': Error loading module '<dep>': …`）。
fn declare_module<'js>(
    ctx: &Ctx<'js>,
    name: &str,
    source: String,
) -> Result<Module<'js, Declared>> {
    match Module::declare(ctx.clone(), name, source) {
        Ok(module) => Ok(module),
        Err(error) if error.is_exception() => {
            // 取走挂起异常（QuickJS 的 SyntaxError 文案在 `message` 里，模块名不在其中）。
            let exception = ctx.catch();
            let message = exception
                .as_exception()
                .and_then(|exception| exception.message())
                .unwrap_or_else(|| error.to_string());
            if is_nested_load_error(&message) {
                return Err(ctx.throw(exception));
            }
            // 入口的模块名带内部 `?v=` 缓存键，用户可见的文案只写真实路径（review finding 2）。
            Err(Error::new_loading_message(display_name(name), message))
        }
        Err(error) => Err(error),
    }
}

/// 用户可见文案里的模块名：去掉 loader 自己的 `?v=<generation>-<seq>` 缓存键。
///
/// `data:` URL 的 payload 里可能有普通 `?`，只有 [`version_query`] 认得的 query 才截断。
fn display_name(name: &str) -> &str {
    if version_query(name).is_some() {
        transpile::module_path(name)
    } else {
        name
    }
}

/// 依赖（而不是本模块）的加载错误：rquickjs 的 loading/resolving 文案已经带了模块名。
fn is_nested_load_error(message: &str) -> bool {
    message.starts_with("Error loading module '")
        || message.starts_with("Error resolving module '")
        // jiti/Node 形状的依赖错误（resolver 的 `Cannot find module …`、依赖自己的
        // `ParseError: …`）：保持一级文案，不再加父模块前缀。
        || message.starts_with("Cannot find module '")
        || message.starts_with("ParseError: ")
}

/// 语法错误 → jiti/Babel 形状的裸消息（`ParseError: <message>  \n <path>:<line>:<col>`）。
///
/// 取第一条诊断：Babel 的 `ParseError` 只带一个位置。没有位置信息时退回诊断列表原文。
fn transpile_error(_name: &str, error: transpile::TranspileError) -> PrepareError {
    match error.babel_message() {
        Some(babel) => PrepareError::Message(format!("ParseError: {babel}")),
        None => PrepareError::Message(format!("ParseError: {}", error.diagnostics().join("\n"))),
    }
}

/// `Require stack` 里的调用方路径。
///
/// Node 的 jiti 错误打印的是 loader 模块的安装路径；Rust 没有对应的运行时文件，使用本模块的
/// 编译期源码路径作为稳定等价物（查找该扩展入口的用户可据此定位加载器）。
pub fn loader_require_stack_path() -> &'static str {
    concat!(env!("CARGO_MANIFEST_DIR"), "/src/loader/mod.rs")
}

/// 给 `.js`/`.mjs` ESM 源补上 jiti 包装器提供的 CJS 绑定（自由 `require` 等）。
///
/// jiti 先原生 import；求值抛 `require is not defined` 时回退到 Babel 的 CJS 转换。本移植保持
/// ESM 输出，只在检测到未声明的自由引用时注入与 TS 分支同一套前导代码（review finding 7）。
fn inject_esm_cjs_bindings(name: &str, source: &str) -> Option<String> {
    transpile::inject_cjs_bindings(name, source)
}

/// jiti 包装器提供的自由名字（`function (exports, require, module, __filename, __dirname, …)`）。
const JITI_WRAPPER_NAMES: [&str; 5] = ["require", "__filename", "__dirname", "module", "exports"];

/// 原生 ESM 求值是否会在**求值期**因自由的 CJS 名字抛 `ReferenceError`（jiti 的回退条件）。
///
/// jiti 对 `.mjs` 与 `"type": "module"` 包的 `.js` 先走 Node 原生 `import()`；只有求值抛异常
/// 才回退到 Babel 的 CJS 转换（review finding 4）。函数体里的 `require`（延迟调用）和
/// `typeof require` 这类特征检测都不会触发回退，因此 `require`/`__filename`/`__dirname` 必须
/// 保持 undefined。
///
/// 近似：「不在函数/箭头函数体内、且不是 `typeof` 直接操作数」的未解析引用视为求值期引用
/// （`typeof` 对未声明名字不抛错，不能算）。`try { require(…) } catch {}` 与立即调用的 IIFE
/// 看不到控制流，仍按求值期处理；jiti 的动态回退无法在单次求值里复刻（见单元报告）。
fn eager_cjs_reference(path: &str, source: &str) -> bool {
    if !JITI_WRAPPER_NAMES.iter().any(|name| source.contains(name)) {
        return false;
    }
    let source_type = SourceType::from_path(Path::new(path)).unwrap_or_else(|_| SourceType::mjs());
    let alloc = Allocator::default();
    let ret = Parser::new(&alloc, source, source_type)
        .with_options(ParseOptions::default())
        .parse();
    if !ret.diagnostics.is_empty() {
        // 语法错误交给 Module::declare；沿用旧行为（注入绑定）不会改变诊断。
        return true;
    }
    let semantic = SemanticBuilder::new()
        .with_build_nodes(true)
        .build(&ret.program)
        .semantic;
    let nodes = semantic.nodes();
    for node in nodes.iter() {
        let AstKind::IdentifierReference(identifier) = node.kind() else {
            continue;
        };
        if !JITI_WRAPPER_NAMES.contains(&identifier.name.as_str()) {
            continue;
        }
        // 局部声明遮蔽的引用（`function f(require) {}` 等）与 CJS 无关。
        if !semantic.is_unresolved_reference(node.id()) {
            continue;
        }
        // 函数/箭头函数体内的引用只在调用时求值，不会让原生 import 失败。
        if nodes.ancestor_kinds(node.id()).any(|kind| {
            matches!(
                kind,
                AstKind::Function(_) | AstKind::ArrowFunctionExpression(_)
            )
        }) {
            continue;
        }
        if is_typeof_operand(nodes, node.id()) {
            continue;
        }
        return true;
    }
    false
}

/// 引用是否是 `typeof x` 的直接操作数（含括号）。`typeof` 对未声明名字不抛 `ReferenceError`。
fn is_typeof_operand(nodes: &oxc_semantic::AstNodes<'_>, node_id: oxc_semantic::NodeId) -> bool {
    let mut current = node_id;
    loop {
        let parent = nodes.parent_node(current);
        match parent.kind() {
            AstKind::ParenthesizedExpression(_) => current = parent.id(),
            AstKind::UnaryExpression(unary) => return unary.operator == UnaryOperator::Typeof,
            _ => return false,
        }
    }
}

/// 把「同时有 ESM 导出语法与 `module.exports`/`exports.*` 写入」的模块改写成 jiti/Babel 的
/// CJS 视角（review finding 1）。
///
/// jiti 用 Babel 把每个 TS/JS 模块转成 CJS：`export …` 变成对同一个 `module.exports` 记录的
/// 写入，`module.exports = …` 重新赋值后默认导入就是新值，`exports.*` 键是具名导出。Rust 保留
/// ESM 输出，因此对混写模块做一次等价改写：
///
/// - 顶层 `export const/let/var/function/class` → 去掉 `export` 关键字 + 原位
///   `module.exports["x"] = x;`；
/// - `export default <expr>` → `module.exports["default"] = <expr>;`；
/// - `export { a as b }` → `module.exports["b"] = a;`；
/// - `export … from "m"` / `export * from "m"` → 命名空间导入 + 写入；
/// - 末尾 `export default <jiti interop>`，并把静态可识别的名字（原 ESM 导出 + CJS 写入）作为
///   从最终 `module.exports` 读取的具名导出。重新赋值 `module.exports` 后原 ESM 名字读到
///   `undefined`，与 jiti 的 CJS 语义一致。
///
/// 返回 `None`：没有 CJS 导出写入（纯 ESM 模块完全不受影响）、解析失败（JSX 等无法降到 ESM
/// 的语法）或没有可改写内容。改写保持源顺序与副作用顺序。
fn rewrite_mixed_esm_exports(name: &str, source: &str) -> Option<String> {
    // 便宜的前置过滤：没有这两个词就不可能有 CJS 导出写入（绝大多数模块）。
    if !source.contains("exports") && !source.contains("module") {
        return None;
    }
    let path = transpile::module_path(name);
    let source_type = if path.ends_with(".tsx")
        || path.ends_with(".jsx")
        || path.ends_with(".mtsx")
        || path.ends_with(".ctsx")
    {
        SourceType::jsx()
    } else {
        SourceType::mjs()
    };
    let alloc = Allocator::default();
    let ret = Parser::new(&alloc, source, source_type)
        .with_options(ParseOptions::default())
        .parse();
    if !ret.diagnostics.is_empty() {
        return None;
    }
    let program = ret.program;
    // 改写依赖 `transpile::cjs_bindings_prelude` 生成的 `var module = { … }` 记录：没有它时
    // `module`/`exports` 只可能是用户自己的局部变量（例如函数参数 `function f(exports)`），
    // 那些写入与 CJS 导出无关（review finding 1 的防回归）。
    let record_end = generated_module_record_end(&program)?;
    let semantic = SemanticBuilder::new()
        .with_build_nodes(true)
        .build(&program)
        .semantic;
    if !assigns_cjs_exports(&semantic) {
        return None;
    }

    let mut editor = SourceEditor::default();
    let mut esm_names: Vec<String> = Vec::new();
    let mut imports: Vec<String> = Vec::new();
    let mut reexport_counter = 0usize;
    use std::fmt::Write as _;

    for statement in &program.body {
        match statement {
            Statement::ExportDeclaration(declaration) => {
                let mut names = Vec::new();
                collect_declaration_names(&declaration.declaration, &mut names);
                let statement_start = declaration.span.start as usize;
                let declaration_start = declaration.declaration.span().start as usize;
                // 删除语句起点到声明起点之间的文本（`export` 关键字与其后的空白）；两端重合
                // 时没有可删内容，跳过以免产生空区间编辑。
                if declaration_start <= statement_start {
                    continue;
                }
                editor.remove(statement_start, declaration_start);
                let mut writes = String::new();
                for local in &names {
                    let _ = writeln!(
                        writes,
                        "module.exports[{}] = {};",
                        cjs::js_string_literal(local),
                        local
                    );
                    push_export_name(&mut esm_names, local);
                }
                editor.insert(declaration.span.end as usize, writes);
            }
            Statement::ExportDefaultDeclaration(declaration) => {
                let statement_start = declaration.span.start as usize;
                let statement_end = declaration.span.end as usize;
                match &declaration.declaration {
                    ExportDefaultDeclarationKind::FunctionDeclaration(function) => {
                        if let Some(id) = &function.id {
                            let local = id.name.to_string();
                            editor.remove(statement_start, function.span.start as usize);
                            editor.insert(
                                statement_end,
                                format!("module.exports[\"default\"] = {};\n", local),
                            );
                        } else {
                            let text = source
                                .get(function.span.start as usize..function.span.end as usize)
                                .unwrap_or_default();
                            editor.replace(
                                statement_start,
                                statement_end,
                                format!("module.exports[\"default\"] = {text};\n"),
                            );
                        }
                    }
                    ExportDefaultDeclarationKind::ClassDeclaration(class) => {
                        if let Some(id) = &class.id {
                            let local = id.name.to_string();
                            editor.remove(statement_start, class.span.start as usize);
                            editor.insert(
                                statement_end,
                                format!("module.exports[\"default\"] = {};\n", local),
                            );
                        } else {
                            let text = source
                                .get(class.span.start as usize..class.span.end as usize)
                                .unwrap_or_default();
                            editor.replace(
                                statement_start,
                                statement_end,
                                format!("module.exports[\"default\"] = {text};\n"),
                            );
                        }
                    }
                    // `export default <expression>`（INHERIT 的 Expression 变体）以及
                    // `export default interface X {}` 之类的纯类型声明。
                    other => {
                        if let Some(expression) = other.as_expression() {
                            let text = source
                                .get(
                                    expression.span().start as usize
                                        ..expression.span().end as usize,
                                )
                                .unwrap_or_default();
                            editor.replace(
                                statement_start,
                                statement_end,
                                format!("module.exports[\"default\"] = {text};\n"),
                            );
                        } else {
                            editor.remove(statement_start, statement_end);
                        }
                    }
                }
            }
            Statement::ExportNamedDeclaration(declaration) => {
                let mut writes = String::new();
                for specifier in &declaration.specifiers {
                    if specifier.export_kind == ImportOrExportKind::Type {
                        continue;
                    }
                    let Some(exported) = module_export_name(&specifier.exported) else {
                        continue;
                    };
                    let Some(local) = module_export_name(&specifier.local) else {
                        continue;
                    };
                    let _ = writeln!(
                        writes,
                        "module.exports[{}] = {};",
                        cjs::js_string_literal(&exported),
                        local
                    );
                    push_export_name(&mut esm_names, &exported);
                }
                editor.replace(
                    declaration.span.start as usize,
                    declaration.span.end as usize,
                    writes,
                );
            }
            Statement::ExportFromDeclaration(declaration) => {
                let namespace = format!("__kiana_reexport_ns_{reexport_counter}");
                reexport_counter += 1;
                imports.push(format!(
                    "import * as {namespace} from {};",
                    cjs::js_string_literal(declaration.source.value.as_str().unwrap_or_default())
                ));
                let mut writes = String::new();
                for specifier in &declaration.specifiers {
                    if specifier.export_kind == ImportOrExportKind::Type {
                        continue;
                    }
                    let Some(exported) = module_export_name(&specifier.exported) else {
                        continue;
                    };
                    let Some(local) = module_export_name(&specifier.local) else {
                        continue;
                    };
                    let _ = writeln!(
                        writes,
                        "module.exports[{}] = {namespace}[{}];",
                        cjs::js_string_literal(&exported),
                        cjs::js_string_literal(&local)
                    );
                    push_export_name(&mut esm_names, &exported);
                }
                editor.replace(
                    declaration.span.start as usize,
                    declaration.span.end as usize,
                    writes,
                );
            }
            Statement::ExportAllDeclaration(declaration) => {
                let namespace = format!("__kiana_reexport_ns_{reexport_counter}");
                reexport_counter += 1;
                imports.push(format!(
                    "import * as {namespace} from {};",
                    cjs::js_string_literal(declaration.source.value.as_str().unwrap_or_default())
                ));
                let writes = if let Some(exported) = &declaration.exported {
                    let Some(exported) = module_export_name(exported) else {
                        continue;
                    };
                    push_export_name(&mut esm_names, &exported);
                    format!(
                        "module.exports[{}] = {namespace};\n",
                        cjs::js_string_literal(&exported)
                    )
                } else {
                    // `export * from` 的运行期键在链接期不可知；复制到 module.exports 后，
                    // 具名导出由静态可识别的名字覆盖（混写模块里的 export * 极少见）。
                    format!(
                        "{{\nconst __kiana_keys = Object.keys({namespace});\n\
                         for (let __kiana_i = 0; __kiana_i < __kiana_keys.length; __kiana_i++) {{\n\
                         const __kiana_key = __kiana_keys[__kiana_i];\n\
                         if (__kiana_key !== \"default\" && __kiana_key !== \"__esModule\") \
                         module.exports[__kiana_key] = {namespace}[__kiana_key];\n\
                         }}\n}}\n"
                    )
                };
                editor.replace(
                    declaration.span.start as usize,
                    declaration.span.end as usize,
                    writes,
                );
            }
            _ => {}
        }
    }

    // CJS 写入静态可识别的名字（`module.exports = {…}`、`exports.x = …` 等）。jiti 的
    // namespace 就是这份记录。
    for name in cjs::cjs_named_exports(name, source) {
        push_export_name(&mut esm_names, &name);
    }

    // Babel 在模块顶部给原 exports 对象加不可枚举的 `__esModule`；重新赋值 module.exports
    // 会丢弃它（jiti 的 interop 依赖这个区别）。
    editor.insert(
        record_end,
        "\nObject.defineProperty(module.exports, \"__esModule\", { value: true });\n".to_string(),
    );

    // import 声明可以在模块顶层任意位置；放在 hashbang 之后最稳妥（求值前完成链接）。
    if !imports.is_empty() {
        let insert_at = match &program.hashbang {
            Some(hashbang) => {
                let end = hashbang.span.end as usize;
                source[end..]
                    .find('\n')
                    .map_or(source.len(), |offset| end + offset + 1)
            }
            None => 0,
        };
        editor.insert(insert_at, format!("{}\n", imports.join("\n")));
    }

    // 尾部代码在模块体全部执行完后运行：此时 `module.exports` 是最终值。具名导出在链接期必须
    // 是静态名字，所以先为每个名字生成一个 const 快照，再用 `export { … as … }` 暴露。
    let mut tail = String::from("const __kiana_cjs_final = module.exports;\n");
    let _ = writeln!(
        tail,
        "export default {};",
        jiti_interop_default("__kiana_cjs_final")
    );
    for (index, name) in esm_names.iter().enumerate() {
        let _ = writeln!(
            tail,
            "const __kiana_cjs_export_{index} = __kiana_cjs_final == null ? undefined : \
             __kiana_cjs_final[{}];",
            cjs::js_string_literal(name)
        );
    }
    if !esm_names.is_empty() {
        tail.push_str("export { ");
        for (index, name) in esm_names.iter().enumerate() {
            if index > 0 {
                tail.push_str(", ");
            }
            let _ = write!(
                tail,
                "__kiana_cjs_export_{index} as {}",
                cjs::js_string_literal(name)
            );
        }
        tail.push_str(" };\n");
    }

    let mut rewritten = editor.finish(source);
    rewritten.push('\n');
    rewritten.push_str(&tail);
    Some(rewritten)
}

/// jiti 2.7 对 Babel 转换模块的 `default` 互操作（与 `cjs::wrap_module` 的转换分支同一规则）。
///
/// `jitiInteropDefault` 的实测语义：`me.default == null ? me : me.__esModule &&
/// typeof me.default.default === "function" ? me.default.default : me.default`。
fn jiti_interop_default(expression: &str) -> String {
    format!(
        "({expression} == null ? {expression} : \
         ({expression}.default == null ? {expression} : \
         ({expression}.__esModule && typeof {expression}.default.default === \"function\" \
         ? {expression}.default.default : {expression}.default)))"
    )
}

/// 添加一个具名导出（`default` 由 `export default` 承担；`__esModule` 是 Babel 的不可枚举
/// 标记，不属于 jiti 的 namespace 键）。
fn push_export_name(names: &mut Vec<String>, name: &str) {
    if name.is_empty() || name == "default" || name == "__esModule" {
        return;
    }
    if !names.iter().any(|existing| existing == name) {
        names.push(name.to_string());
    }
}

/// `ModuleExportName` → 字符串（标识符或字符串字面量写法）。
fn module_export_name(name: &ModuleExportName<'_>) -> Option<String> {
    match name {
        ModuleExportName::IdentifierName(identifier) => Some(identifier.name.to_string()),
        ModuleExportName::IdentifierReference(identifier) => Some(identifier.name.to_string()),
        ModuleExportName::StringLiteral(literal) => literal.value.as_str().map(ToString::to_string),
    }
}

/// 顶层 `export` 声明引入的运行时绑定名。
fn collect_declaration_names(declaration: &Declaration<'_>, names: &mut Vec<String>) {
    match declaration {
        Declaration::VariableDeclaration(variable) => {
            for declarator in &variable.declarations {
                collect_binding_names(&declarator.id, names);
            }
        }
        Declaration::FunctionDeclaration(function) => {
            if let Some(id) = &function.id {
                names.push(id.name.to_string());
            }
        }
        Declaration::ClassDeclaration(class) => {
            if let Some(id) = &class.id {
                names.push(id.name.to_string());
            }
        }
        _ => {}
    }
}

/// 解构模式里的全部绑定名。
fn collect_binding_names(pattern: &BindingPattern<'_>, names: &mut Vec<String>) {
    match pattern {
        BindingPattern::BindingIdentifier(identifier) => names.push(identifier.name.to_string()),
        BindingPattern::AssignmentPattern(assignment) => {
            collect_binding_names(&assignment.left, names);
        }
        BindingPattern::ObjectPattern(object) => {
            for property in &object.properties {
                collect_binding_names(&property.value, names);
            }
            if let Some(rest) = &object.rest {
                collect_binding_names(&rest.argument, names);
            }
        }
        BindingPattern::ArrayPattern(array) => {
            for element in array.elements.iter().flatten() {
                collect_binding_names(element, names);
            }
            if let Some(rest) = &array.rest {
                collect_binding_names(&rest.argument, names);
            }
        }
    }
}

/// 是否存在对 CJS 导出面的写入（`module.exports = …`、`module.exports.x = …`、`exports.x = …`）。
///
/// 只检查赋值表达式的左侧成员访问，不区分作用域层次（函数体里的写入也算）；解构赋值、
/// `Object.assign(module.exports, …)` 与 `exports.x++` 不是这种形态，不在这里判定。
fn assigns_cjs_exports(semantic: &Semantic<'_>) -> bool {
    for node in semantic.nodes().iter() {
        let AstKind::AssignmentExpression(assignment) = node.kind() else {
            continue;
        };
        let Some(member) = assignment
            .left
            .as_simple_assignment_target()
            .and_then(|target| target.as_member_expression())
        else {
            continue;
        };
        if is_module_exports(member) {
            return true;
        }
        let object = member.object();
        let writes_exports = matches!(object, Expression::Identifier(identifier) if identifier.name == "exports")
            || object.as_member_expression().is_some_and(is_module_exports);
        if writes_exports && member.static_property_name().is_some() {
            return true;
        }
    }
    false
}

/// `true` when `member` is `module.exports`.
fn is_module_exports(member: &MemberExpression<'_>) -> bool {
    member.static_property_name().and_then(|name| name.as_str()) == Some("exports")
        && matches!(member.object(), Expression::Identifier(identifier) if identifier.name == "module")
}

/// 生成的前导 `var module = { … }` 记录的结束位置（没有记录时 `None`）。
fn generated_module_record_end(program: &Program<'_>) -> Option<usize> {
    // 只在前导段里找：`cjs_bindings_prelude` 把这些 `var` 生成在源开头，遇到第一条不是生成
    // 前导的语句就停止。
    for statement in &program.body {
        if is_generated_module_record(statement) {
            return Some(statement.span().end as usize);
        }
        if !is_generated_cjs_prelude(statement) {
            break;
        }
    }
    None
}

/// 是否是 `cjs_bindings_prelude` 生成的 `var module = { id, path, filename, exports, loaded,
/// children, parent };`（7 个属性的对象字面量）。
fn is_generated_module_record(statement: &Statement<'_>) -> bool {
    let Statement::VariableDeclaration(variable) = statement else {
        return false;
    };
    if variable.declarations.len() != 1 {
        return false;
    }
    let declarator = &variable.declarations[0];
    let BindingPattern::BindingIdentifier(identifier) = &declarator.id else {
        return false;
    };
    identifier.name.as_str() == "module"
        && matches!(&declarator.init, Some(Expression::ObjectExpression(object)) if object.properties.len() == 7)
}

/// 是否是 `cjs_bindings_prelude` 生成的顶层 `var` 语句。
fn is_generated_cjs_prelude(statement: &Statement<'_>) -> bool {
    let Statement::VariableDeclaration(variable) = statement else {
        return false;
    };
    if variable.declarations.len() != 1 {
        return false;
    }
    let declarator = &variable.declarations[0];
    let BindingPattern::BindingIdentifier(identifier) = &declarator.id else {
        return false;
    };
    let Some(init) = &declarator.init else {
        return false;
    };
    match identifier.name.as_str() {
        "exports" => init.as_member_expression().is_some_and(is_module_exports),
        "require" => matches!(init, Expression::CallExpression(call) if is_make_require_call(call)),
        "__filename" | "__dirname" => matches!(init, Expression::StringLiteral(_)),
        "module" => {
            matches!(init, Expression::ObjectExpression(object) if object.properties.len() == 7)
        }
        _ => false,
    }
}

/// `globalThis.__kiana_make_require(...)`。
fn is_make_require_call(call: &CallExpression<'_>) -> bool {
    call.callee
        .as_member_expression()
        .is_some_and(|member| {
            member.static_property_name().and_then(|name| name.as_str()) == Some("__kiana_make_require")
                && matches!(member.object(), Expression::Identifier(identifier) if identifier.name == "globalThis")
        })
}

/// 按字节区间改写源文本（区间来自 oxc 的 span）。
#[derive(Default)]
struct SourceEditor {
    edits: Vec<(usize, usize, String)>,
}

impl SourceEditor {
    fn remove(&mut self, start: usize, end: usize) {
        self.edits.push((start, end, String::new()));
    }

    fn insert(&mut self, at: usize, text: String) {
        self.edits.push((at, at, text));
    }

    fn replace(&mut self, start: usize, end: usize, text: String) {
        self.edits.push((start, end, text));
    }

    fn finish(mut self, source: &str) -> String {
        // 稳定排序：同一位置的多个插入保持 push 顺序；按 (start, end) 排序让游标只前进。
        self.edits.sort_by_key(|(start, end, _)| (*start, *end));
        let mut out = String::with_capacity(source.len() + 256);
        let mut cursor = 0;
        for (start, end, text) in self.edits {
            if start < cursor || end > source.len() {
                // 重叠的改写不会出现（每个语句一条），防御性跳过。
                continue;
            }
            out.push_str(&source[cursor..start]);
            out.push_str(&text);
            cursor = end;
        }
        out.push_str(&source[cursor..]);
        out
    }
}

/// 按 `new Function` 的语义预解析 CJS 模块体，返回带真实路径/位置的诊断。
///
/// `Module._compile` 与 `new Function` 都把模块体当**函数体**求值，所以顶层 `return`、sloppy
/// `with` 等合法；这里用同样的包装形态（`function (module, exports, require, __filename,
/// __dirname) { … }`）解析，避免误报。`source` 已去 BOM（`prepare` 做的）。
///
/// 失败文案：`ParseError: <oxc 诊断> at <真实路径>:<行>:<列>`（行 1 基、列 0 基，与 Babel/jiti
/// 的位置口径一致）。jiti 的 `ParseError: <Babel 文案> <path>:<line>:<col>` 是同一形状，具体
/// 措辞按 §3.2 记录为 oxc 的（review finding 10：至少要带文件名与位置）。
fn cjs_parse_error(name: &str, source: &str) -> Option<String> {
    /// `new Function` 的参数顺序（`cjs::wrap_module` 相同）。
    const PREFIX: &str =
        "function __kiana_cjs_module(module, exports, require, __filename, __dirname) {\n";
    const SUFFIX: &str = "\n}";
    // `cjs::wrap_module` strips a `#!` line before `new Function`; do the same here (keeping the
    // newline so line numbers stay identical to the evaluated module).
    let source = match source.strip_prefix("#!") {
        Some(rest) => match rest.find('\n') {
            Some(index) => &rest[index..],
            None => "",
        },
        None => source,
    };
    let wrapped = format!("{PREFIX}{source}{SUFFIX}");
    let source_type = oxc_span::SourceType::from_path(Path::new("module.cjs"))
        .unwrap_or_else(|_| oxc_span::SourceType::cjs());
    let alloc = oxc_allocator::Allocator::default();
    let ret = oxc_parser::Parser::new(&alloc, &wrapped, source_type)
        .with_options(oxc_parser::ParseOptions::default())
        .parse();
    let diagnostic = ret.diagnostics.first()?;
    let real = transpile::module_path(name);
    let mut out = format!("ParseError: {diagnostic}");
    if let Some(label) = diagnostic.labels.first() {
        let offset = label.span().start as usize;
        if let Some(offset) = offset.checked_sub(PREFIX.len()) {
            let (line, column) = line_column(source, offset);
            use std::fmt::Write as _;
            let _ = write!(out, " at {real}:{line}:{column}");
        }
    }
    Some(out)
}

/// `source` 里 `offset` 处的 (1 基行, 0 基 UTF-16 列)，与 JS 引擎/Babel 的列口径一致。
fn line_column(source: &str, offset: usize) -> (usize, usize) {
    let mut line = 1;
    let mut column = 0;
    for (index, ch) in source.char_indices() {
        if index >= offset {
            break;
        }
        if ch == '\n' {
            line += 1;
            column = 0;
        } else {
            column += ch.len_utf16();
        }
    }
    (line, column)
}

/// 给 `Module` 装上 `import.meta`（见模块文档）。
///
/// jiti 2.7（`tryNative: false`，即内嵌二进制的形态）有两条完全不同的 `import.meta`：
///
/// - **Node 原生分支**（`.mjs`，以及 `"type": "module"` 包里的 `.js`）：Node 自己的对象，键按
///   Node 的顺序是 `dirname, filename, main, resolve, url`；
/// - **Babel 转换分支**（TS、普通 `.js`、回退到转换的 `.mjs`/type:module `.js`）：Babel 把
///   `import.meta.url` / `.resolve` 重写成字面量/`jitiESMResolve`，整体读取 `import.meta` 时
///   拿到的是被当作 `data:` 模块求值的包装器的原生对象 `{main, resolve, url}`（review
///   finding 3）。`.js` 的静态 `import.meta.dirname`/`.filename` 也被重写成字面量，但在裸对象
///   上不可见，所以这里把它们装成**不可枚举**属性，同时满足 `Object.keys` 与静态读取。
///   TS 的 `dirname`/`filename` 不被重写（jiti 实测为 `undefined`），因此不装。
///
/// `url` 始终是去掉 `?v=` 的真实 `file://` URL（jiti 的静态 `import.meta.url` 也是这个值）。
fn install_import_meta<'js>(
    ctx: &Ctx<'js>,
    module: &Module<'js, Declared>,
    name: &str,
    url: Option<&str>,
    kind: ModuleKind,
    native_esm: bool,
) -> Result<()> {
    let Some(url) = url else {
        // 没有 URL 的模块（虚拟模块/注册源）不安装 import.meta。
        return Ok(());
    };
    // 属性写在求值之前；模块体读到的 import.meta 就是这里的对象。
    let meta = module.meta()?;
    let importer = if kind == ModuleKind::DataUrl {
        // data: URL 的 payload 里可能有 `?`，不能用 `module_path` 截断。
        url.to_string()
    } else {
        transpile::module_path(name).to_string()
    };
    // `importer` 用去掉 `?v=` 的干净路径（data: URL 例外：payload 里可能有 `?`，不能截断），
    // 作为 `__kiana_host.resolve(from, spec)` 的第一个参数。
    let resolve = Function::new(
        ctx.clone(),
        move |ctx: Ctx<'js>, specifier: String| -> Result<String> {
            import_meta_resolve(&ctx, &importer, &specifier)
        },
    )?;
    if native_esm {
        let path = transpile::module_path(name);
        if let Some(parent) = Path::new(path).parent() {
            meta.set("dirname", parent.to_string_lossy().into_owned())?;
        }
        meta.set("filename", path)?;
        meta.set("main", false)?;
        meta.set("resolve", resolve)?;
        meta.set("url", url)?;
        return Ok(());
    }
    // 转换分支：Node 的 data: 模块与 jiti 的 CJS 包装器对象都按 `main, resolve, url` 排列。
    meta.set("main", false)?;
    meta.set("resolve", resolve)?;
    meta.set("url", url)?;
    if kind == ModuleKind::JavaScript {
        // Babel 只重写静态成员访问；裸对象上保持隐藏（review finding 3）。
        let path = transpile::module_path(name);
        if let Some(parent) = Path::new(path).parent() {
            meta.prop(
                "dirname",
                rquickjs::object::Property::from(parent.to_string_lossy().into_owned())
                    .writable()
                    .configurable(),
            )?;
        }
        meta.prop(
            "filename",
            rquickjs::object::Property::from(path.to_string())
                .writable()
                .configurable(),
        )?;
    }
    Ok(())
}

/// `import.meta.resolve(specifier)`：走 loader-host 的 `__kiana_host.resolve(importer, specifier)`，
/// 绝对路径结果转成 `file://` URL。
///
/// jiti 的**静态** `import.meta.resolve(x)` 被改写成 `jitiESMResolve(x)`，相对路径同样得到
/// file URL；动态读取 `import.meta.resolve` 时拿到的是 data: URL 模块的 Node 原生 resolver，
/// 相对路径会抛 `Failed to resolve module specifier …`（review finding 10）。本移植不区分
/// 静态/动态访问，统一给出可用的 file URL——这是记录在案的刻意偏差（见模块文档）。
fn import_meta_resolve<'js>(ctx: &Ctx<'js>, importer: &str, specifier: &str) -> Result<String> {
    // `__kiana_host` 由 loader-host 单元装到全局；本模块没有编译期依赖，缺了就报明确错误。
    let host: Value = ctx.globals().get("__kiana_host")?;
    let Some(host) = host.as_object() else {
        return Err(host_primitive_missing(
            ctx,
            "import.meta.resolve requires the __kiana_host.resolve host primitive",
        ));
    };
    let resolve: Value = host.get("resolve")?;
    let Some(resolve) = resolve.as_function() else {
        return Err(host_primitive_missing(
            ctx,
            "import.meta.resolve requires the __kiana_host.resolve host primitive",
        ));
    };
    let resolved: String = resolve.call((importer.to_string(), specifier.to_string()))?;
    if Path::new(&resolved).is_absolute() {
        Ok(file_url(&resolved).unwrap_or(resolved))
    } else {
        Ok(resolved)
    }
}

/// 宿主原语缺失时的明确错误（planner 契约：不要伪装成 `MODULE_NOT_FOUND`）。
fn host_primitive_missing<'js>(ctx: &Ctx<'js>, what: &str) -> Error {
    Exception::throw_message(
        ctx,
        &format!("{what} (loader-host primitives are not installed)"),
    )
}

/// Node 的全局模块目录（`Module.globalPaths`），供 `require.resolve.paths` 使用。
///
/// 与 Node 26 的 `_initPaths()` 一致：
///
/// 1. `NODE_PATH` 的分隔符（POSIX `:` / Windows `;`）切片**原样**排在最前（空项过滤；
///    Node 26 不做 `path.resolve`——实测 `NODE_PATH=rel` 保留 `rel`）；
/// 2. `$HOME/.node_modules`、`$HOME/.node_libraries`（`HOME` 为空串时视为未设置并省略；
///    Node 用 `if (homeDir)`，`HOME=` 是 falsy）；
/// 3. `<prefix>/lib/node`，`<prefix> = dirname(dirname(process.execPath))`——在 Rust 宿主里用
///    `std::env::current_exe()` 代替 `process.execPath`。
///
/// review finding 5：旧实现漏了 1，并把空 `HOME` 变成了相对路径 `.node_modules`。
pub fn node_global_paths() -> Vec<String> {
    let node_path = std::env::var_os("NODE_PATH").and_then(|value| value.into_string().ok());
    let home = home_dir();
    let exec_path = std::env::current_exe().ok();
    node_global_paths_from(node_path.as_deref(), home.as_deref(), exec_path.as_deref())
}

/// [`node_global_paths`] 的纯函数版本（测试不依赖进程环境）。
pub fn node_global_paths_from(
    node_path: Option<&str>,
    home: Option<&Path>,
    exec_path: Option<&Path>,
) -> Vec<String> {
    // 构造顺序与 Node 的 `_initPaths()` 相反：先 push 最低优先级的前缀目录，之后用
    // `insert(0, …)` 把 home 与 NODE_PATH 依次插到前面。
    let mut paths = Vec::new();
    if let Some(exec_path) = exec_path
        && let Some(prefix) = exec_path.parent().and_then(Path::parent)
    {
        paths.push(absolute_path(&prefix.join("lib").join("node")));
    }
    if let Some(home) = home.filter(|home| !home.as_os_str().is_empty()) {
        paths.insert(0, absolute_path(&home.join(".node_libraries")));
        paths.insert(0, absolute_path(&home.join(".node_modules")));
    }
    if let Some(node_path) = node_path {
        // NODE_PATH 的条目原样保留：Node 26 不对它们做 `path.resolve`，相对项仍是相对路径；
        // 空项（连续分隔符）被过滤。
        let delimiter = if cfg!(windows) { ';' } else { ':' };
        let mut entries: Vec<String> = node_path
            .split(delimiter)
            .filter(|entry| !entry.is_empty())
            .map(ToString::to_string)
            .collect();
        entries.append(&mut paths);
        paths = entries;
    }
    paths
}

/// Node `path.resolve` 的近似（仅用于构造绝对路径）：相对路径接到 cwd 上。
///
/// POSIX 上 `std::path::absolute` 按 POSIX 语义解析中间组件，但**保留 `..`**（Node 的
/// `path.resolve` 会把它归一化掉）；本函数不做额外处理。当前调用方只传 home 目录与可执行
/// 文件前缀目录。
fn absolute_path(path: &Path) -> String {
    std::path::absolute(path)
        .unwrap_or_else(|_| path.to_path_buf())
        .to_string_lossy()
        .into_owned()
}

/// Node 的 `HOME`（Windows 上优先 `USERPROFILE`，与 Node 的 `_initPaths` 相同）。
///
/// 空串视为未设置：`HOME=` 在 Node 里是 falsy，会丢掉 home 路径（review finding 5）。
fn home_dir() -> Option<PathBuf> {
    let value = {
        #[cfg(windows)]
        {
            std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME"))
        }
        #[cfg(not(windows))]
        {
            std::env::var_os("HOME")
        }
    }?;
    if value.is_empty() {
        return None;
    }
    Some(PathBuf::from(value))
}

/// `import.meta.url` 的值：真实绝对路径的 `file://` URL，与 Node 的 `pathToFileURL(path).href`
/// 逐字节一致。
///
/// Node（WHATWG URL 的 file 序列化）保留的字符集是
/// `A-Z a-z 0-9 ! $ & ' ( ) * + , - . / : ; = @ _`；其余 ASCII（含 `~ [ ] ^ | { }` 空白、
/// `# ? % " < > \` `` ` ``）与非 ASCII 一律按 UTF-8 字节 `%XX`（大写十六进制）编码。
/// `url::Url::from_file_path` 的 WHATWG 路径集合会漏掉 `~ [ ] ^ |`（review finding 5）。
fn file_url(path: &str) -> Option<String> {
    if !Path::new(path).is_absolute() {
        return None;
    }
    let mut out = String::with_capacity(path.len() + 8);
    out.push_str("file://");
    // Windows 的盘符路径（`C:\x`）在 URL 里写作 `/C:/x`；Node 在 win32 上也把 `\` 当分隔符。
    #[cfg(windows)]
    {
        let path = path.replace('\\', "/");
        if !path.starts_with('/') {
            out.push('/');
        }
        push_percent_encoded_path(&mut out, &path);
    }
    #[cfg(not(windows))]
    push_percent_encoded_path(&mut out, path);
    Some(out)
}

/// Node `pathToFileURL` 的路径字符编码（见 [`file_url`]）。
fn push_percent_encoded_path(out: &mut String, path: &str) {
    for byte in path.bytes() {
        if is_url_path_safe(byte) {
            out.push(byte as char);
        } else {
            use std::fmt::Write as _;
            let _ = write!(out, "%{byte:02X}");
        }
    }
}

/// `pathToFileURL` 原样保留的字节（Node 26 实测，见 `tests/fixtures/loader/golden.json`）。
fn is_url_path_safe(byte: u8) -> bool {
    byte.is_ascii_alphanumeric()
        || matches!(
            byte,
            b'!' | b'$'
                | b'&'
                | b'\''
                | b'('
                | b')'
                | b'*'
                | b'+'
                | b','
                | b'-'
                | b'.'
                | b'/'
                | b':'
                | b';'
                | b'='
                | b'@'
                | b'_'
        )
}

/// `.json` 的模块源。用 `JSON.parse` 而不是直接嵌表达式：JSON 里的 U+2028/U+2029 与
/// `__proto__` 等边缘情况都按 JSON 语义处理。
///
/// `named` 为真时把顶层对象的键同时输出为具名导出（jiti 把 JSON 当 `require()` 目标：
/// `import { name } from "./data.json"` 与 `import * as ns` 都能看到顶层键，review
/// finding 1）。`data:application/json` 走 Node 的 ESM JSON 语义，只有 `default`。
fn json_module_source(text: &str, named: bool) -> String {
    let literal = cjs::js_string_literal(text);
    let mut out =
        format!("const __kiana_json = JSON.parse({literal});\nexport default __kiana_json;\n");
    if !named {
        return out;
    }
    let names = json_named_exports(text);
    for (index, name) in names.iter().enumerate() {
        use std::fmt::Write as _;
        let _ = writeln!(
            out,
            "const __kiana_json_export_{index} = __kiana_json == null ? undefined : __kiana_json[{}];",
            cjs::js_string_literal(name)
        );
    }
    if !names.is_empty() {
        out.push_str("export { ");
        for (index, name) in names.iter().enumerate() {
            if index > 0 {
                out.push_str(", ");
            }
            use std::fmt::Write as _;
            let _ = write!(
                out,
                "__kiana_json_export_{index} as {}",
                cjs::js_string_literal(name)
            );
        }
        out.push_str(" };\n");
    }
    out
}

/// JSON 顶层键（`default` 除外）：jiti/`require` 的具名导出面。
///
/// 对象取全部键（含 `"a-b"`、`"__proto__"` 这类字符串键）；数组取下标（jiti 的 wildcard
/// 互操作同样暴露 `"0"`,`"1"`）；原始值只有 `default`。顶层的 `"default"` 键不产生具名导出
/// （jiti 里 `namespace.default` 是整个对象，不是该键的值）。
fn json_named_exports(text: &str) -> Vec<String> {
    match serde_json::from_str::<serde_json::Value>(text) {
        Ok(serde_json::Value::Object(map)) => map
            .keys()
            .filter(|key| key.as_str() != "default")
            .cloned()
            .collect(),
        Ok(serde_json::Value::Array(items)) => {
            (0..items.len()).map(|index| index.to_string()).collect()
        }
        _ => Vec::new(),
    }
}

/// `data:` 的方案名（大小写不敏感，WHATWG URL；`DATA:` 与 `data:` 等价）。
fn strip_data_scheme(name: &str) -> Option<&str> {
    let prefix = name.get(.."data:".len())?;
    prefix
        .eq_ignore_ascii_case("data:")
        .then(|| &name["data:".len()..])
}

/// `data:` URL 的解码（`data:[<mediatype>][;base64],<payload>`），接受/拒绝集合与 Node 26 的
/// ESM `data:` 加载器逐条一致（review finding 6）。
///
/// Node 用两条彼此独立的规则：
///
/// 1. **格式判定**（`getDataProtocolModuleFormat`）直接对序列化后的 URL pathname 做
///    `/^([^/]+\/[^;,]+)(?:;[^,]*)?,/`，捕获的 MIME 交给 `mimeToFormat`：JS 接受
///    `text|application/javascript` 加可选的 `charset=utf-8`（大小写不敏感、允许空白），
///    JSON 只接受精确的 `application/json`；其余（含 `foo` 这种没有 `/` 的）报
///    `Unknown module format: <mime|null> for URL <url>`。
/// 2. **payload 解码**（WHATWG `dataURLProcessor`）：fragment 不参与（URL 序列化排除它），
///    MIME 去首尾 ASCII 空白，`;` + 0..n 个空格 + `base64`（大小写不敏感）是 base64 标记；
///    payload 先百分号解码，再按 forgiving-base64（忽略 ASCII 空白、容忍省略填充）解码；失败
///    报 `Invalid URL`，且先于格式/属性错误（Node 的 URL 解析发生在模块加载器之前）。
fn prepare_data_url(
    name: &str,
    rest: &str,
    attribute_type: Option<String>,
) -> std::result::Result<PreparedModule, DataUrlError> {
    // 阶段 1：URL 规范化/序列化；错误文案与 `import.meta.url` 用的都是序列化后的形式。
    // WHATWG URL 解析：去掉首尾 C0 控制字符/空格，删除所有制表/换行符，scheme 小写，
    // fragment 单独保存（序列化时保留、解码时排除）。
    let normalized = normalize_url_input(rest);
    let (path, fragment) = match normalized.split_once('#') {
        Some((path, fragment)) => (path, Some(fragment)),
        None => (normalized.as_str(), None),
    };
    let path = serialize_data_url_path(path);
    let url = match fragment {
        Some(fragment) => format!("data:{path}#{fragment}"),
        None => format!("data:{path}"),
    };

    // 阶段 2：URL 语法检查。没有逗号时 Node 的 URL 解析器直接失败（`Invalid URL`）。
    let Some(comma) = path.find(',') else {
        return Err(DataUrlError::InvalidUrl);
    };

    let mime = node_data_mime(&path);
    let format = data_url_format(mime);

    // 阶段 3：解码 payload。payload 解码先于格式检查：Node 在模块加载器看到 URL 之前就完成了
    // URL 解析，所以解码失败（`Invalid URL`）会盖过 `Unknown module format`。
    let raw_mime = &path[..comma];
    let is_base64 = has_base64_flag(trim_ascii_whitespace(raw_mime));
    let payload = &path[comma + 1..];
    let decoded = percent_encoding::percent_decode_str(payload).collect::<Vec<u8>>();
    let bytes = if is_base64 {
        forgiving_base64(&decoded).ok_or(DataUrlError::InvalidUrl)?
    } else {
        decoded
    };

    let is_javascript = format == DataUrlFormat::Javascript;
    let is_json = format == DataUrlFormat::Json;
    if !is_javascript && !is_json {
        return Err(DataUrlError::UnknownFormat {
            url,
            mime: mime.map(ToString::to_string),
        });
    }
    // 阶段 4：import 属性检查。Node 只认 `type: "json"`：JSON 模块必须显式声明，JS 模块带上
    // 它反而报 TYPE_INCOMPATIBLE，其它值报 UNSUPPORTED。
    match attribute_type.as_deref() {
        None if is_json => {
            return Err(DataUrlError::MissingJsonAttribute { url });
        }
        Some("json") if is_javascript => {
            return Err(DataUrlError::IncompatibleAttribute { url });
        }
        Some(value) if value != "json" => {
            return Err(DataUrlError::UnsupportedAttribute {
                url,
                value: value.to_string(),
            });
        }
        _ => {}
    }
    // 阶段 5：生成模块源。JS payload 原样交给 QuickJS（语法错误在 declare/eval 阶段才暴露）；
    // JSON 走 Node 的 ESM JSON 语义，只有 default，不生成具名导出。
    let text = String::from_utf8_lossy(&bytes);
    let (source, kind) = if is_json {
        (json_module_source(&text, false), ModuleKind::Json)
    } else {
        (text.into_owned(), ModuleKind::DataUrl)
    };
    Ok(PreparedModule {
        name: name.to_string(),
        source,
        url: Some(url),
        kind,
        cjs_bindings: false,
        native_esm: false,
    })
}

/// [`prepare_data_url`] 的格式判定结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DataUrlFormat {
    /// `text/javascript` / `application/javascript`（可带 `charset=utf-8` 参数）。
    Javascript,
    /// 精确的 `application/json`（大小写敏感、不允许空白）。
    Json,
    /// 其它或缺失的 MIME：Node 报 `Unknown module format`。
    Unknown,
}

/// Node 的 `getDataProtocolModuleFormat`：在 URL pathname 上匹配
/// `/^([^/]+\/[^;,]+)(?:;[^,]*)?,/`，返回捕获的 MIME（原样保留空白与大小写）。
fn node_data_mime(path: &str) -> Option<&str> {
    let slash = path.find('/')?;
    if slash == 0 {
        return None;
    }
    let after_slash = &path[slash + 1..];
    let end = after_slash.find([';', ','])?;
    if end == 0 {
        return None;
    }
    let mime = &path[..slash + 1 + end];
    let tail = &after_slash[end..];
    // `(?:;[^,]*)?,`：逗号前可以有任意一段 `;…` 参数。
    if tail.starts_with(',') || (tail.starts_with(';') && tail.contains(',')) {
        Some(mime)
    } else {
        None
    }
}

/// Node 的 `mimeToFormat`（只覆盖 JS/JSON；wasm/text 在本运行时没有对应模块）。
fn data_url_format(mime: Option<&str>) -> DataUrlFormat {
    let Some(mime) = mime else {
        return DataUrlFormat::Unknown;
    };
    if is_javascript_mime(mime) {
        return DataUrlFormat::Javascript;
    }
    // Node 用严格的 `mime === 'application/json'`（大小写敏感、不允许空白）。
    if mime == "application/json" {
        return DataUrlFormat::Json;
    }
    DataUrlFormat::Unknown
}

/// `/^\s*(text|application)\/javascript\s*(;\s*charset=utf-?8\s*)?$/i`（Node 的 `mimeToFormat`）。
fn is_javascript_mime(mime: &str) -> bool {
    let lower = mime.to_ascii_lowercase();
    let trimmed = lower.trim_matches(is_js_whitespace);
    for base in ["text/javascript", "application/javascript"] {
        let Some(rest) = trimmed.strip_prefix(base) else {
            continue;
        };
        let rest = rest.trim_start_matches(is_js_whitespace);
        if rest.is_empty() {
            return true;
        }
        if let Some(parameters) = rest.strip_prefix(';') {
            let parameters = parameters.trim_matches(is_js_whitespace);
            if parameters == "charset=utf-8" || parameters == "charset=utf8" {
                return true;
            }
        }
    }
    false
}

/// JS 正则里 `\s` 的近似（含 `\u{feff}`，JS 的 `\s` 包含它）。
fn is_js_whitespace(ch: char) -> bool {
    ch.is_whitespace() || ch == '\u{feff}'
}

/// WHATWG 的 ASCII 空白（`\t \n \f \r ` 空格）。
fn is_ascii_whitespace_byte(byte: u8) -> bool {
    matches!(byte, 0x09 | 0x0a | 0x0c | 0x0d | 0x20)
}

/// 去掉首尾 WHATWG ASCII 空白。
fn trim_ascii_whitespace(value: &str) -> &str {
    value.trim_matches(|ch: char| matches!(ch, '\t' | '\n' | '\u{0c}' | '\r' | ' '))
}

/// `dataURLProcessor` 的 base64 标记：`;` + 0..n 个空格 + `base64`（大小写不敏感）结尾。
fn has_base64_flag(mime: &str) -> bool {
    let Some(split) = mime.len().checked_sub(6) else {
        return false;
    };
    if !mime.is_char_boundary(split) || !mime[split..].eq_ignore_ascii_case("base64") {
        return false;
    }
    mime[..split].trim_end_matches(' ').ends_with(';')
}

/// WHATWG 的 forgiving-base64（忽略 ASCII 空白、容忍省略填充、拒绝非法字符）。
fn forgiving_base64(bytes: &[u8]) -> Option<Vec<u8>> {
    // 1. 去掉 ASCII 空白（WHATWG 允许 payload 折行/带空格）。
    let filtered: Vec<u8> = bytes
        .iter()
        .copied()
        .filter(|byte| !is_ascii_whitespace_byte(*byte))
        .collect();
    // 2. 只有长度是 4 的倍数时才把末尾的 `=` 当填充，最多吃掉两个；剩余长度 % 4 == 1 无法
    //    解码（base64 的最小单元是 2 个字符）。
    let mut data_length = filtered.len();
    if data_length.is_multiple_of(4) && filtered.last() == Some(&b'=') {
        data_length -= 1;
        if filtered.get(data_length.wrapping_sub(1)) == Some(&b'=') {
            data_length -= 1;
        }
    }
    if data_length % 4 == 1 {
        return None;
    }
    // 3. 非法字符（包括内嵌的 `=`）一律失败；用无填充引擎解码。
    let data = &filtered[..data_length];
    if data
        .iter()
        .any(|byte| !(byte.is_ascii_alphanumeric() || matches!(byte, b'+' | b'/')))
    {
        return None;
    }
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD_NO_PAD
        .decode(data)
        .ok()
}

/// WHATWG URL 解析的输入预处理：去掉首尾 C0 控制字符/空格，删除所有制表/换行符。
fn normalize_url_input(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for ch in input.chars() {
        if matches!(ch, '\t' | '\n' | '\r') {
            continue;
        }
        out.push(ch);
    }
    out.trim_matches(|ch: char| (ch as u32) <= 0x20).to_string()
}

/// URL 序列化的 opaque path：ASCII 原样保留，C0 控制字符与 `> U+007E` 按 UTF-8 字节
/// 百分号编码（Node 的 `new URL("data:…").pathname`）。
fn serialize_data_url_path(path: &str) -> String {
    let mut out = String::with_capacity(path.len());
    for ch in path.chars() {
        let code = ch as u32;
        if (0x20..=0x7e).contains(&code) {
            out.push(ch);
        } else {
            let mut buffer = [0u8; 4];
            for byte in ch.encode_utf8(&mut buffer).bytes() {
                use std::fmt::Write as _;
                let _ = write!(out, "%{byte:02X}");
            }
        }
    }
    out
}

// 纯函数单测：用 Node 26 的 golden fixture（`tests/fixtures/loader/golden.json`）与逐字文案
// 锁定 `file_url`、data: URL 解码/格式/属性规则、JSON 具名导出、CJS 解析错误的行列与全局
// 路径规则。需要 QuickJS 的端到端行为在 `tests/loader_impl.rs`。
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strict_nocache_env_value_is_exactly_one() {
        assert!(strict_nocache_from_value(Some("1")));
        assert!(!strict_nocache_from_value(Some("0")));
        assert!(!strict_nocache_from_value(Some("true")));
        assert!(!strict_nocache_from_value(None));
    }

    #[test]
    fn version_query_only_matches_the_cache_buster() {
        assert_eq!(version_query("/a/b.ts?v=3-7"), Some("v=3-7"));
        assert_eq!(version_query("/a/b.ts?x=1"), None);
        assert_eq!(version_query("/a/b.ts"), None);
        assert_eq!(version_query("data:text/javascript,1"), None);
    }

    #[test]
    fn file_urls_match_node_path_to_file_url() {
        // Golden values from `pathToFileURL` (Node 26); see tests/fixtures/loader/golden.json.
        let golden: serde_json::Value =
            serde_json::from_str(include_str!("../../tests/fixtures/loader/golden.json"))
                .expect("golden.json");
        let paths = golden["node_path_to_file_url"].as_array().expect("paths");
        let urls = golden["node_path_to_file_url_expected"]
            .as_array()
            .expect("urls");
        assert_eq!(paths.len(), urls.len());
        for (path, url) in paths.iter().zip(urls) {
            let path = path.as_str().expect("path string");
            let url = url.as_str().expect("url string");
            assert_eq!(file_url(path).as_deref(), Some(url), "path {path}");
        }
        assert_eq!(file_url("relative.ts"), None);
    }

    #[test]
    fn data_urls_decode_base64_and_percent_payloads() {
        let module = prepare_data_url(
            "data:text/javascript;base64,ZXhwb3J0IGRlZmF1bHQgNDI7",
            "text/javascript;base64,ZXhwb3J0IGRlZmF1bHQgNDI7",
            None,
        )
        .expect("base64 data url");
        assert_eq!(module.source(), "export default 42;");
        assert_eq!(module.kind(), ModuleKind::DataUrl);
        assert_eq!(
            module.url(),
            Some("data:text/javascript;base64,ZXhwb3J0IGRlZmF1bHQgNDI7")
        );

        // WHATWG: the base64 flag is case-insensitive and must be the last MIME item.
        let upper = prepare_data_url(
            "data:text/javascript;BASE64,ZXhwb3J0IGRlZmF1bHQgNDI7",
            "text/javascript;BASE64,ZXhwb3J0IGRlZmF1bHQgNDI7",
            None,
        )
        .expect("uppercase base64");
        assert_eq!(upper.source(), "export default 42;");
        let trailing = prepare_data_url(
            "data:text/javascript;charset=utf-8;base64,ZXhwb3J0IGRlZmF1bHQgNDI7",
            "text/javascript;charset=utf-8;base64,ZXhwb3J0IGRlZmF1bHQgNDI7",
            None,
        )
        .expect("base64 after parameters");
        assert_eq!(trailing.source(), "export default 42;");
        let not_last = prepare_data_url(
            "data:text/javascript;base64;charset=utf-8,ZXhwb3J0IGRlZmF1bHQgNDI7",
            "text/javascript;base64;charset=utf-8,ZXhwb3J0IGRlZmF1bHQgNDI7",
            None,
        )
        .expect("not a base64 payload");
        // Percent-decoded as source text; the payload is not valid JS, but decoding succeeded.
        assert_eq!(not_last.source(), "ZXhwb3J0IGRlZmF1bHQgNDI7");

        let module = prepare_data_url(
            "data:text/javascript,export default 40%2B2;",
            "text/javascript,export default 40%2B2;",
            None,
        )
        .expect("percent data url");
        assert_eq!(module.source(), "export default 40+2;");

        let json = prepare_data_url(
            "data:application/json,{\"a\":1}",
            "application/json,{\"a\":1}",
            Some("json".to_string()),
        )
        .expect("json data url");
        assert_eq!(
            json.source(),
            "const __kiana_json = JSON.parse(\"{\\\"a\\\":1}\");\nexport default __kiana_json;\n"
        );
        assert_eq!(json.kind(), ModuleKind::Json);

        assert_eq!(
            prepare_data_url("data:no-comma", "no-comma", None),
            Err(DataUrlError::InvalidUrl)
        );
        assert_eq!(
            prepare_data_url(
                "data:text/javascript;base64,!!!",
                "text/javascript;base64,!!!",
                None
            ),
            Err(DataUrlError::InvalidUrl)
        );
        // Node's URL parser fails on the payload before the module format is examined.
        assert_eq!(
            prepare_data_url("data:text/plain;base64,!!!", "text/plain;base64,!!!", None),
            Err(DataUrlError::InvalidUrl)
        );
    }

    #[test]
    fn data_urls_follow_node_format_and_attribute_rules() {
        // Unknown/absent MIME → ERR_UNKNOWN_MODULE_FORMAT with the exact Node wording.
        let error = prepare_data_url(
            "data:text/plain,export default 42",
            "text/plain,export default 42",
            None,
        )
        .unwrap_err();
        assert_eq!(
            error.message(),
            "Unknown module format: text/plain for URL data:text/plain,export default 42"
        );
        assert_eq!(error.code(), "ERR_UNKNOWN_MODULE_FORMAT");
        assert_eq!(error.constructor(), "RangeError");
        let error = prepare_data_url("data:;base64,AAAA", ";base64,AAAA", None).unwrap_err();
        assert_eq!(
            error.message(),
            "Unknown module format: null for URL data:;base64,AAAA"
        );
        // MIME matching is case-insensitive.
        assert!(
            prepare_data_url(
                "data:TEXT/JAVASCRIPT,export default 41;",
                "TEXT/JAVASCRIPT,export default 41;",
                None,
            )
            .is_ok()
        );
        // application/json requires `with { type: "json" }`.
        let error = prepare_data_url(
            "data:application/json,{\"a\":1}",
            "application/json,{\"a\":1}",
            None,
        )
        .unwrap_err();
        assert_eq!(error.code(), "ERR_IMPORT_ATTRIBUTE_MISSING");
        assert_eq!(
            error.message(),
            "Module \"data:application/json,{\"a\":1}\" needs an import attribute of \"type: json\""
        );
        // A JSON attribute on a JS module is incompatible (`type: "json"` is the only one Node
        // knows); any other value is unsupported.
        let error = prepare_data_url(
            "data:text/javascript,export default 1;",
            "text/javascript,export default 1;",
            Some("json".to_string()),
        )
        .unwrap_err();
        assert_eq!(error.code(), "ERR_IMPORT_ATTRIBUTE_TYPE_INCOMPATIBLE");
        let error = prepare_data_url(
            "data:application/json,{\"a\":1}",
            "application/json,{\"a\":1}",
            Some("text".to_string()),
        )
        .unwrap_err();
        assert_eq!(error.code(), "ERR_IMPORT_ATTRIBUTE_UNSUPPORTED");
        // The data: scheme is case-insensitive (WHATWG URL).
        assert_eq!(
            strip_data_scheme("DATA:text/javascript,1"),
            Some("text/javascript,1")
        );
        assert_eq!(strip_data_scheme("database:x"), None);
    }

    #[test]
    fn json_sources_are_parsed_not_embedded() {
        assert_eq!(
            json_module_source("{\"a\":1}", false),
            "const __kiana_json = JSON.parse(\"{\\\"a\\\":1}\");\nexport default __kiana_json;\n"
        );
        // jiti/require semantics: top-level object keys are named exports; `default` is not.
        assert_eq!(
            json_module_source("{\"name\":\"probe\",\"default\":1}", true),
            "const __kiana_json = JSON.parse(\"{\\\"name\\\":\\\"probe\\\",\\\"default\\\":1}\");\n\
             export default __kiana_json;\n\
             const __kiana_json_export_0 = __kiana_json == null ? undefined : __kiana_json[\"name\"];\n\
             export { __kiana_json_export_0 as \"name\" };\n"
        );
        // Arrays expose their indices (jiti's wildcard interop); primitives only `default`.
        assert_eq!(
            json_module_source("[1,2]", true),
            "const __kiana_json = JSON.parse(\"[1,2]\");\n\
             export default __kiana_json;\n\
             const __kiana_json_export_0 = __kiana_json == null ? undefined : __kiana_json[\"0\"];\n\
             const __kiana_json_export_1 = __kiana_json == null ? undefined : __kiana_json[\"1\"];\n\
             export { __kiana_json_export_0 as \"0\", __kiana_json_export_1 as \"1\" };\n"
        );
        assert_eq!(
            json_module_source("42", true),
            "const __kiana_json = JSON.parse(\"42\");\nexport default __kiana_json;\n"
        );
        assert!(
            json_named_exports("{\"a\":1,\"b\":2}")
                .iter()
                .eq(["a", "b"])
        );
        assert!(json_named_exports("not json").is_empty());
    }

    #[test]
    fn file_url_percent_encodes_like_node() {
        // Node keeps `! $ & ' ( ) * + , - . / : ; = @ _` and alphanumerics; everything else is
        // percent-encoded per UTF-8 byte (see golden.json for the full measured table).
        assert_eq!(
            file_url("/tmp/sp[a]c^e|{x}~/meta.ts").as_deref(),
            Some("file:///tmp/sp%5Ba%5Dc%5Ee%7C%7Bx%7D%7E/meta.ts")
        );
        assert_eq!(file_url("/tmp/a b").as_deref(), Some("file:///tmp/a%20b"));
        assert_eq!(file_url("/tmp/a~b").as_deref(), Some("file:///tmp/a%7Eb"));
        assert_eq!(
            file_url("/tmp/扩展").as_deref(),
            Some("file:///tmp/%E6%89%A9%E5%B1%95")
        );
        assert_eq!(file_url("/tmp/a'b").as_deref(), Some("file:///tmp/a'b"));
    }

    #[test]
    fn cjs_parse_errors_carry_the_file_and_position() {
        // `new Function` semantics: a top-level `return` is legal in a CJS module body.
        assert_eq!(cjs_parse_error("/ext/a.cjs", "return 1;"), None);
        assert_eq!(cjs_parse_error("/ext/a.cjs", "with (o) { x; }"), None);
        let error =
            cjs_parse_error("/ext/bad.cjs", "exports.a = 1;\nconst x = ;\n").expect("error");
        assert!(error.starts_with("ParseError: "), "{error}");
        assert!(error.contains(" at /ext/bad.cjs:2:"), "{error}");
    }

    #[test]
    fn node_global_paths_match_the_node_rule() {
        let paths = node_global_paths();
        if let Some(home) = home_dir() {
            assert!(paths.contains(&home.join(".node_modules").to_string_lossy().into_owned()));
            assert!(paths.contains(&home.join(".node_libraries").to_string_lossy().into_owned()));
        }
        if let Ok(exec_path) = std::env::current_exe() {
            let prefix = exec_path.parent().and_then(Path::parent).unwrap();
            assert!(
                paths.contains(
                    &prefix
                        .join("lib")
                        .join("node")
                        .to_string_lossy()
                        .into_owned()
                )
            );
        }
    }
}
