//! 模块解析：Node ESM + jiti 顺序（`spec/design-extension-runtime.md` §3.8），`oxc_resolver` 驱动。
//!
//! 解析顺序（§3.8；与 jiti 2.7 的实测顺序逐条对齐）：
//!
//! 0. URL specifier：`file:` 按 Node 的 `fileURLToPath` 转成绝对路径后走路径解析（jiti 的
//!    `jitiRequire` 也是先 `fileURLToPath` 再解析）；`data:` 原样返回（Node ESM 的解析结果就是
//!    它本身，由 loader 解码 payload）。两者都不再被当作裸包名（review finding 7）。
//! 1. 虚拟键精确匹配（22 个键，含 `typebox/compile` 等子路径；见 [`VIRTUAL_MODULE_KEYS`] 与
//!    [`VirtualFs`]）。从虚拟模块名出发的相对导入在虚拟表内解析（typebox 的 `build/*.mjs`）。
//! 2. `./`、`../`、`/` 按 jiti 的**文件→目录**顺序解析：
//!    精确文件 → `{spec}{ext}`（`.js`、`.mjs`、`.cjs`、`.ts`、`.tsx`、`.mts`、`.cts`、`.mtsx`、
//!    `.ctsx`）→ `{spec}/index{ext}` → Node CJS 兜底（`package.json` 的 `main`（含 legacy
//!    index 回退）、`{spec}.json/.node`、目录 `index.json/.node`）→ NodeNext 扩展名替换
//!    （`./x.js`→`./x.ts`、`.mjs`→`.mts`、`.cjs`→`.cts`，以及 `.jsx`→`.tsx`）。
//!    注意实测 jiti 2.7：无扩展名时 `.js` **先于** `.ts`（默认 `JITI_EXTENSIONS`），且相对目录的
//!    `index.*` 先于 `package.json` 的 `main`（Node 的目录 import 在 jiti 里先抛
//!    `ERR_UNSUPPORTED_DIR_IMPORT` 再按 index 探测），`exports` 字段不用于相对目录。
//! 3. 裸 specifier：`node:`/内建名 → `node:<name>` shim 键（§6.1）；否则从 `base` 目录逐级
//!    `node_modules`（`oxc_resolver` 处理 `exports` 条件、`#imports`、symlink）。字段只用 Node 的
//!    `main`（实测 jiti 2.7 忽略打包器专用的 `module` 字段）；条件顺序与 jiti 相同：import 先
//!    `import`/`node`/`default`、失败再 `require`；require 反之。
//! 4. 失败 → jiti/Node 的 `Cannot find module '<spec>'` + `Require stack:` + 导入方
//!    （`rquickjs::loader::Resolver` 实现直接抛 JS 异常；[`ResolveError`] 的
//!    `Error loading module '<spec>': <原因>` 文案保留给同步诊断接口）。
//!
//! `tsconfig` 的 `paths` 只在扩展目录里存在 `tsconfig.json` 时启用（§3.8）。
//!
//! # 跨单元契约：虚拟文件表 `VirtualFs`（合并契约，U4/U10 都依赖）
//!
//! [`VirtualFs`] 是「模块名 → 预置源码」的只读表，由 virtual-modules 单元（U10）填充，由
//! loader-runtime（U4）作为 [`ResolverOptions::virtual_fs`] 传给本解析器，并在 `Module::declare`
//! 时用同名键提供源码。**名字必须逐字节一致**（QuickJS 的模块缓存键就是名字）：
//!
//! - [`VirtualFile::name`] 是模块名，例如 `"typebox/build/index.mjs"`；相对导入以它为 `base`
//!   在表内解析，因此同一包内的多个文件共享一个命名空间。
//! - [`VirtualAlias`] 把公开入口键（`"typebox"`、`"@sinclair/typebox/compile"`…）映射到文件，
//!   使 `import "typebox/compile"` 得到 `"typebox/build/compile/index.mjs"`。
//! - 22 个 kiana-* 虚拟模块没有同名的文件条目：它们的源码由 U4/U10 以**键本身**为模块名声明。
//!   生产表同时为每个键生成了别名（`build.rs`），所以解析通常先命中别名目标文件；只有在表
//!   未收录该键时（例如 `VirtualFs::empty()` 的测试），才由 [`is_virtual_module_key`] 原样
//!   返回键本身。
//!
//! 本文件不实现 U4 的 `rquickjs::Loader`，也不实现 U10 的 691 个 typebox 文件表；只定义两者都
//! 依赖的注册表类型与解析规则。
//!
//! # 输入、输出与返回值
//!
//! [`ModuleResolver::resolve_specifier`] 的输入是「导入方模块名 `base` + 源码里书写的
//! specifier + 解析模式 [`ResolutionKind`]」，输出是一个**模块名**（QuickJS 模块缓存键），
//! 不是已打开的文件句柄：
//!
//! - 虚拟表命中 → 表里的规范文件名（[`VirtualFile::name`]）；
//! - 22 个虚拟键（表未收录该键时）→ 键本身；
//! - `node:`/内建名 → `node:<name>` shim 键；
//! - `data:` URL → 原 URL；
//! - 文件 → 绝对路径；`resolve_path_like` 在 unix 上还会 `canonicalize`。
//!
//! `base` 上的 `?v=<generation>-<seq>` 缓存键（§3.4）在解析前由 `module_path` 去掉，返回值也
//! **不带**该 query：把 query 扩散到依赖图是 loader 侧 `ScopedResolver` 的职责（见
//! `loader_impl` 的同名类型），不在本文件。
//!
//! # 调用方
//!
//! - `crate::loader::install` 构造 `ModuleResolver`（`extension_dir: None` + 生产虚拟表）并装进
//!   引擎，作为 `rquickjs::loader::Resolver`；
//! - `crate::loader::js_host` 为同步原语 `__kiana_host.resolve` 持有一个 `Rc<ModuleResolver>`
//!   （未显式设置时用 `ResolverOptions::default()` 惰性创建），按
//!   [`ResolutionKind::Require`] 调用；
//! - 测试通过 [`ResolverOptions::extension_dir`] 指向 fixture 目录，锁定 jiti 的探测顺序。
//!
//! # 执行流程
//!
//! 1. `file:` URL → 转成路径后走路径探测；`data:` URL → 原样返回；
//! 2. 虚拟键/别名精确匹配；虚拟命名空间内的相对导入只在表内解析；
//! 3. `node:`/内建名 → shim 键；
//! 4. 路径类 specifier → `resolve_path_like` 按 jiti 顺序逐项探测；
//! 5. 裸包名 → 两个 `oxc_resolver` 依次尝试（主条件集失败后用另一条件集）。
//!
//! [`ResolutionKind`] 只影响第 5 项（裸包名）；前四项对 import 与 require 完全相同。
//!
//! # 注意事项
//!
//! - **失败原因被折叠**：所有失败都返回同一个 `No such file or directory (os error 2)`，
//!   `oxc_resolver` 的原始原因（`exports` 不匹配等）被刻意丢弃（§3.8 第 4 步固定文案），
//!   调用方无法从错误里区分具体原因。
//! - **两级错误文案**：同步 API 返回 [`ResolveError`]，`Display` 形如
//!   `Error loading module '<spec>': <message>`；实现 `rquickjs::loader::Resolver` 的 `resolve`
//!   则直接抛 JS 异常 `Cannot find module '<spec>'` + `Require stack`（TS 侧 Node/jiti 的形状，
//!   见 `not_found_exception`）。
//! - **虚拟命名空间是封闭的**：`base` 是虚拟模块时，相对 specifier 若不在表内立刻失败，
//!   不会回落到宿主机文件系统。
//! - **缓存范围**：本类型自身不持有解析缓存；手写的路径探测（`resolve_path_like`）每次
//!   都重新访问文件系统。裸包名走 `oxc_resolver`，两个 `Resolver` 实例各持有一个
//!   `Arc<Cache>`（内部缓存 `(is_file, is_dir)` 元数据、package.json 等），同一实例的
//!   重复解析可能命中缓存。模块级去重由 QuickJS 的模块表（按返回的模块名）完成。
#![cfg(feature = "runtime")]

use std::fmt;
use std::path::{Path, PathBuf};

use oxc_resolver::{
    ResolveOptions, Resolver, TsconfigDiscovery, TsconfigOptions, TsconfigReferences,
};

use super::transpile::module_path;

/// The 22 virtual module keys, verbatim from `core/extensions/virtual-modules.ts` (§3.3).
///
/// Order matches the TS object literal so reports can diff it directly. Identical keys map to the
/// same module instance (single `Context` + one `Module::declare` per key).
///
/// Matching is whole-string: subpaths that are not listed (for example
/// `@earendil-works/kiana-ai/providers`) are not virtual and fall through to `node_modules`.
/// `virtual_modules` iterates this same array when it installs `__kiana_virtual`, so resolver and
/// installer share one key list.
pub const VIRTUAL_MODULE_KEYS: [&str; 22] = [
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

/// Whether `name` is one of the 22 bundled virtual modules (§3.3).
///
/// Exact, byte-for-byte comparison against [`VIRTUAL_MODULE_KEYS`]; there is no prefix or subpath
/// matching. [`ModuleResolver::resolve_specifier`] consults the [`VirtualFs`] table first for the
/// specifier and uses this predicate as the fallback for keys the table does not carry (for example
/// `VirtualFs::empty()` in tests); it also uses the predicate to recognize a virtual importer
/// `base`.
pub fn is_virtual_module_key(name: &str) -> bool {
    VIRTUAL_MODULE_KEYS.contains(&name)
}

/// Node builtin module names, as listed by `require("node:module").builtinModules` on Node 22.
///
/// `node:`-prefixed-only builtins (`node:test`, `node:sqlite`, …) are handled by the prefix rule in
/// [`builtin_module_key`] and do not need an entry here.
const NODE_BUILTINS: &[&str] = &[
    "_http_agent",
    "_http_client",
    "_http_common",
    "_http_incoming",
    "_http_outgoing",
    "_http_server",
    "_tls_common",
    "_tls_wrap",
    "assert",
    "assert/strict",
    "async_hooks",
    "buffer",
    "child_process",
    "cluster",
    "console",
    "constants",
    "crypto",
    "dgram",
    "diagnostics_channel",
    "dns",
    "dns/promises",
    "domain",
    "events",
    "fs",
    "fs/promises",
    "http",
    "http2",
    "https",
    "inspector",
    "inspector/promises",
    "module",
    "net",
    "os",
    "path",
    "path/posix",
    "path/win32",
    "perf_hooks",
    "process",
    "punycode",
    "querystring",
    "readline",
    "readline/promises",
    "repl",
    "stream",
    "stream/consumers",
    "stream/promises",
    "stream/web",
    "string_decoder",
    "sys",
    "timers",
    "timers/promises",
    "tls",
    "trace_events",
    "tty",
    "url",
    "util",
    "util/types",
    "v8",
    "vm",
    "wasi",
    "worker_threads",
    "zlib",
];

/// Map a bare or `node:`-prefixed specifier to the `node:<name>` shim module key (§6.1).
///
/// `fs` → `node:fs`, `fs/promises` → `node:fs/promises`, `node:fs` → `node:fs` (unchanged).
/// Returns `None` for non-builtins so the caller falls through to `node_modules` resolution.
///
/// Any explicit `node:` prefix is accepted unchanged, even for names absent from `NODE_BUILTINS`
/// (`node:sqlite` → `node:sqlite`): whether a shim source exists is a loader-side question, and
/// the loader reports `No such built-in module: <name>` for the ones it does not serve.
pub fn builtin_module_key(specifier: &str) -> Option<String> {
    if specifier.starts_with("node:") {
        return Some(specifier.to_string());
    }
    if NODE_BUILTINS.contains(&specifier) {
        return Some(format!("node:{specifier}"));
    }
    None
}

/// One read-only virtual file served from memory (U10 populates, U4 serves).
///
/// A `build.rs`-generated table looks like:
///
/// ```text
/// static TYPEBOX_FILES: &[VirtualFile] = &[
///     VirtualFile { name: "typebox/build/index.mjs", source: include_bytes!("../vendor/typebox/build/index.mjs") },
///     // …
/// ];
/// ```
///
/// `name` is the QuickJS module name: it must be byte-identical to the name the resolver returns
/// and the loader passes to `Module::declare`, otherwise the module cache gets two entries (for
/// typebox that also means two distinct `Type` objects). Duplicate names are not rejected at
/// construction; lookups take the first table entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VirtualFile {
    /// Module name; used verbatim as the QuickJS module name.
    ///
    /// Must be unique within the table: [`VirtualFs`] lookups take the first match in table order.
    pub name: &'static str,
    /// UTF-8 source bytes.
    ///
    /// The virtual-modules unit validates this when declaring sources (`embedded source is not
    /// valid UTF-8`); the loader's own path converts with `String::from_utf8_lossy`.
    pub source: &'static [u8],
}

/// Maps a public module key to a [`VirtualFile::name`] (e.g. `"typebox"` →
/// `"typebox/build/index.mjs"`).
///
/// Aliases are single-hop: [`VirtualFs::canonical_name`] looks the target up among [`VirtualFile`]
/// entries only, so a target that is itself an alias key resolves to nothing. If a table contains
/// both a file whose `name` equals an alias `key` and that alias, the file wins (files are searched
/// first).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VirtualAlias {
    /// Import specifier handled by the alias.
    pub key: &'static str,
    /// Target [`VirtualFile::name`]; must exist in the same table.
    ///
    /// A missing target is not reported at construction time: lookups simply return `None`.
    pub target: &'static str,
}

/// Read-only table of embedded module sources plus entry aliases (cross-unit contract, see module
/// docs).
///
/// Cheap to copy (`&'static` slices only); the loader keeps one instance per generation.
///
/// Construction performs no validation: an alias whose target is absent, or duplicate file names,
/// are tolerated and degrade to `None` / first-match at lookup time. The production table is
/// generated by `build.rs`, which aliases all 22 keys to files in that same table; the invariants
/// that the targets exist and that the key set matches [`VIRTUAL_MODULE_KEYS`] are asserted by the
/// virtual-modules unit's tests, not by this type.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct VirtualFs {
    files: &'static [VirtualFile],
    aliases: &'static [VirtualAlias],
}

impl VirtualFs {
    /// Build a registry from static tables.
    ///
    /// Duplicate entries are permitted; [`VirtualFs::canonical_name`] and [`VirtualFs::lookup`]
    /// return the first match in table order.
    pub const fn new(files: &'static [VirtualFile], aliases: &'static [VirtualAlias]) -> Self {
        Self { files, aliases }
    }

    /// An empty registry (no embedded files).
    ///
    /// This is the state used by `ResolverOptions::default()` and by tests that only exercise
    /// filesystem resolution; in `ModuleResolver::resolve_specifier` the 22 virtual keys still
    /// resolve through `is_virtual_module_key` (as the key itself).
    pub const fn empty() -> Self {
        Self {
            files: &[],
            aliases: &[],
        }
    }

    /// All embedded files, in table order.
    pub const fn files(&self) -> &'static [VirtualFile] {
        self.files
    }

    /// All entry aliases, in table order.
    pub const fn aliases(&self) -> &'static [VirtualAlias] {
        self.aliases
    }

    /// Canonical module name for `name`: the file name itself, or the alias target when it names a
    /// file in this table. Returns `None` when `name` resolves to nothing in the table.
    ///
    /// Precedence: an exact [`VirtualFile::name`] match wins over an alias [`VirtualAlias::key`] of
    /// the same spelling. The input is compared literally — no `.`/`..` folding and no extension
    /// guessing (that is [`VirtualFs::resolve_relative`]'s job). The returned value is always the
    /// `name` of a table file.
    pub fn canonical_name(&self, name: &str) -> Option<&'static str> {
        if let Some(file) = self.files.iter().find(|file| file.name == name) {
            return Some(file.name);
        }
        let alias = self.aliases.iter().find(|alias| alias.key == name)?;
        self.files
            .iter()
            .find(|file| file.name == alias.target)
            .map(|file| file.name)
    }

    /// Source bytes for `name` (alias-aware).
    ///
    /// A missing alias target yields `None` rather than panicking. The bytes are returned exactly
    /// as stored; whether they form valid UTF-8 is the caller's concern.
    pub fn lookup(&self, name: &str) -> Option<&'static [u8]> {
        let canonical = self.canonical_name(name)?;
        self.files
            .iter()
            .find(|file| file.name == canonical)
            .map(|file| file.source)
    }

    /// Whether `name` is an exact virtual key (file name or alias key).
    ///
    /// Equivalent to `canonical_name(name).is_some()`; this is an O(table size) scan, not a hash
    /// lookup.
    pub fn is_virtual_key(&self, name: &str) -> bool {
        self.canonical_name(name).is_some()
    }

    /// Resolve a relative specifier (`./x`, `../x`) against a virtual module name.
    ///
    /// Virtual namespaces are flat: `base` is looked up in the table, its directory is joined with
    /// `specifier`, `.`/`..` are normalized lexically, and the result must be another table entry.
    /// Extension guessing does not apply (embedded packages always spell their extensions).
    ///
    /// `base` itself must be in the table (it is canonicalized first, so an alias key works). A
    /// `..` that would climb above the namespace root makes the whole resolution return `None`
    /// instead of escaping the namespace. The normalized name goes through
    /// [`VirtualFs::canonical_name`] again, so a result that happens to be an alias key is mapped to
    /// its target file.
    pub fn resolve_relative(&self, base: &str, specifier: &str) -> Option<&'static str> {
        let base = self.canonical_name(base)?;
        let dir = match base.rsplit_once('/') {
            Some((dir, _)) => dir,
            None => "",
        };
        let joined = if dir.is_empty() {
            specifier.to_string()
        } else {
            format!("{dir}/{specifier}")
        };
        let normalized = normalize_virtual_path(&joined)?;
        self.canonical_name(&normalized)
    }
}

/// Lexically normalize a `/`-separated virtual path; `None` when it escapes the namespace root.
///
/// Empty segments and `.` are dropped, `..` pops one segment (and fails if there is nothing to
/// pop), everything else is kept verbatim. No filesystem access happens here.
fn normalize_virtual_path(path: &str) -> Option<String> {
    let mut parts: Vec<&str> = Vec::new();
    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            part => parts.push(part),
        }
    }
    Some(parts.join("/"))
}

/// Which Node resolution mode a specifier is resolved for (§3.8 step 3, §3.5 item 2).
///
/// jiti tries the `import`/`node`/`default` conditions first for `import()` and the
/// `require`/`node`/`default` ones first for `require()`, then falls back to the other set (and
/// finally to Node's CJS resolver). Both modes ignore the bundler-only `module` field.
///
/// Only bare package specifiers consult this mode (step 3b of
/// [`ModuleResolver::resolve_specifier`]); virtual keys, `node:` keys and path-like specifiers are
/// resolved identically for both variants.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResolutionKind {
    /// ESM `import` / dynamic `import()`.
    Import,
    /// CJS `require()`.
    Require,
}

/// Resolution failure. `Display` is byte-identical to the rquickjs loader error text (§3.8 step 4).
///
/// Construction goes through the private `not_found` helper only, so `message()` is the fixed
/// `No such file or directory (os error 2)` text no matter which branch actually failed; the real
/// cause (missing file, bad `file:` URL, `exports` mismatch, …) is deliberately not recorded.
/// Fields are private and exposed through [`ResolveError::specifier`] and [`ResolveError::message`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolveError {
    specifier: String,
    message: String,
}

impl ResolveError {
    fn not_found(specifier: &str) -> Self {
        // Measured text of the rquickjs loader on a missing module (design §3.8 step 4).
        // 这是唯一的构造入口：上层的所有失败分支都汇到这里，因此原因必然相同。
        Self {
            specifier: specifier.to_string(),
            message: "No such file or directory (os error 2)".to_string(),
        }
    }

    /// The specifier that failed to resolve.
    ///
    /// This is the text as written by the importer: for a `file:` URL failure it is the URL, not
    /// the converted path, and it never includes the importer's `?v=` cache buster.
    pub fn specifier(&self) -> &str {
        &self.specifier
    }

    /// The underlying reason, without the `Error loading module '<spec>': ` prefix.
    pub fn message(&self) -> &str {
        &self.message
    }

    /// Convert to the rquickjs loading error with the same text (`Error loading module '<spec>':
    /// <message>`).
    ///
    /// The real `Resolver` impl does **not** use this: it throws a `Cannot find module ...` JS
    /// exception instead (`not_found_exception`). Today the callers are the stub resolvers in
    /// `tests/loader_impl.rs` and `tests/cjs_loader.rs`.
    pub fn to_rquickjs_error(&self) -> rquickjs::Error {
        rquickjs::Error::new_loading_message(self.specifier.clone(), self.message.clone())
    }
}

impl fmt::Display for ResolveError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "Error loading module '{}': {}",
            self.specifier, self.message
        )
    }
}

impl std::error::Error for ResolveError {}

/// Options for [`ModuleResolver`].
///
/// `Default` means "no extension directory, no embedded modules" — the same configuration
/// `loader::js_host` uses when it lazily builds a resolver for `__kiana_host.resolve`.
#[derive(Debug, Clone, Default)]
pub struct ResolverOptions {
    /// Extension directory. When `tsconfig.json` exists there, its `paths` are honored (§3.8).
    ///
    /// It is also the fallback resolution root when the importer name is not an absolute path
    /// (see `ModuleResolver::importer_dir`). `tsconfig.json` 的存在性只在
    /// [`ModuleResolver::new`] 检查一次；字段本身由 `importer_dir` 在每次解析时读取
    /// （外部无法修改它——`ModuleResolver` 拿到的是构造时拷贝的私有字段）。
    pub extension_dir: Option<PathBuf>,
    /// Embedded module table supplied by the virtual-modules unit (see module docs).
    ///
    /// Moved into [`ModuleResolver`] by value (`VirtualFs` is `Copy`); `Default` leaves it empty.
    pub virtual_fs: VirtualFs,
}

/// Node ESM + jiti module resolver (§3.8).
///
/// Implements [`rquickjs::loader::Resolver`] for ESM imports; CJS `require` shares the same rules
/// through [`ModuleResolver::resolve_specifier`] with [`ResolutionKind::Require`].
///
/// Construction is the only place the filesystem layout is inspected (`tsconfig.json` existence
/// and the extension list). Afterwards the type's own fields are immutable; hand-written path
/// probing (`resolve_path_like`) hits the filesystem again on every call, while bare package
/// names go through the two `oxc_resolver` instances, whose internal `Arc<Cache>` may serve
/// repeated resolutions from cache. The `&self` receiver is what
/// lets `loader::js_host` share it in an `Rc`.
///
/// The production assembly (`crate::loader::install`) builds it with `extension_dir: None`, so
/// tsconfig `paths` are only honored when a caller (tests, tools) passes `Some(dir)`.
pub struct ModuleResolver {
    virtual_fs: VirtualFs,
    extension_dir: Option<PathBuf>,
    import_resolver: Resolver,
    require_resolver: Resolver,
}

/// Extensions probed for extensionless/relative specifiers, in jiti 2.7's default order
/// (`JITI_EXTENSIONS`). `.js` comes before `.ts`; measured with `local.ts` + `local.js` present,
/// jiti loads `local.js`.
const JITI_EXTENSIONS: [&str; 9] = [
    ".js", ".mjs", ".cjs", ".ts", ".tsx", ".mts", ".cts", ".mtsx", ".ctsx",
];

/// Extensions Node's CJS resolver falls back to after jiti's own probing.
const NODE_CJS_EXTENSIONS: [&str; 3] = [".js", ".json", ".node"];

/// NodeNext-style `.js`/`.mjs`/`.cjs` specifier substitutions (`mod.js` → `mod.ts`, …).
const TS_EXTENSION_SUBSTITUTIONS: [(&str, &str); 6] = [
    (".js", ".ts"),
    (".jsx", ".tsx"),
    (".mjs", ".mts"),
    (".mjsx", ".mtsx"),
    (".cjs", ".cts"),
    (".cjsx", ".ctsx"),
];

impl ModuleResolver {
    /// Build a resolver. Two `oxc_resolver` instances are created (import/require conditions).
    ///
    /// `tsconfig` is enabled only when `{extension_dir}/tsconfig.json` is an existing regular file
    /// (`is_file()`); a directory of that name or a missing file leaves `oxc_resolver` at its
    /// default. The two inner resolvers differ **only** in `condition_names` order; both use
    /// `main_fields = ["main"]` and `main_files = ["index"]` (see `build_oxc_resolver`).
    pub fn new(options: ResolverOptions) -> Self {
        let tsconfig = options
            .extension_dir
            .as_ref()
            .map(|dir| dir.join("tsconfig.json"))
            .filter(|path| path.is_file());

        let extensions: Vec<String> = JITI_EXTENSIONS
            .iter()
            .map(|extension| (*extension).to_string())
            .collect();

        // Measured with jiti 2.7: the `module` field is ignored for both `import` and `require`;
        // only Node's `main` (and `exports`) count. `index` is the legacy `main_files` fallback.
        let import_resolver = build_oxc_resolver(
            &["import", "node", "default"],
            &["main"],
            extensions.clone(),
            tsconfig.clone(),
        );
        let require_resolver = build_oxc_resolver(
            &["require", "node", "default"],
            &["main"],
            extensions,
            tsconfig,
        );

        Self {
            virtual_fs: options.virtual_fs,
            extension_dir: options.extension_dir,
            import_resolver,
            require_resolver,
        }
    }

    /// The embedded module table used by this resolver.
    ///
    /// A reference to the value copied from `ResolverOptions::virtual_fs` at construction.
    /// `VirtualFs` itself is `Copy`, so copying it does not clone the underlying static slices.
    pub fn virtual_fs(&self) -> &VirtualFs {
        &self.virtual_fs
    }

    /// Resolve `specifier` as imported from module `base` (§3.8 order).
    ///
    /// `base` may be:
    /// - an absolute file name, possibly with a `?v=<generation>-<seq>` cache buster (§3.4);
    /// - a virtual module name (22 keys or [`VirtualFs`] entry);
    /// - a `node:<name>` shim key.
    ///
    /// `specifier` is the text exactly as written in the importing module, so `file:`/`data:` URLs
    /// and relative paths are all valid inputs.
    ///
    /// # Returns
    ///
    /// A **module name** (the QuickJS module cache key), not an opened handle: a table entry's
    /// canonical file name, the key itself for the 22 virtual keys and `node:` shims, the original
    /// URL for `data:`, or an absolute path for files. Path-like results come from
    /// `resolve_path_like` and are `canonicalize`d on unix; bare package results come from
    /// `oxc_resolver`. The returned name never carries a `?v=` query — adding it is
    /// `ScopedResolver`'s job.
    ///
    /// # Errors
    ///
    /// Every failure collapses into [`ResolveError`] with the fixed
    /// `No such file or directory (os error 2)` message: an invalid `file:` URL, a relative import
    /// escaping a virtual namespace, and both `oxc_resolver` attempts failing all look the same.
    /// `oxc_resolver`'s own reason is intentionally discarded (§3.8 step 4 fixes the text).
    ///
    /// [`ResolutionKind`] only changes the condition order for bare package specifiers; every
    /// other branch behaves identically for import and require.
    pub fn resolve_specifier(
        &self,
        base: &str,
        specifier: &str,
        kind: ResolutionKind,
    ) -> Result<String, ResolveError> {
        let base_name = module_path(base);

        // 0. URL specifiers (Node ESM semantics, review finding 7). `file:` URLs are converted to
        //    paths exactly like jiti's `fileURLToPath` and then follow the path rules below;
        //    `data:` URLs resolve to themselves and are decoded by the loader.
        //
        //    `?v=<generation>-<seq>` 只是 QuickJS 的模块缓存键（§3.4），不是文件名的一部分：
        //    上面先统一去掉，返回值也不再携带它。
        if is_file_url(specifier) {
            let path =
                file_url_to_path(specifier).ok_or_else(|| ResolveError::not_found(specifier))?;
            let path = path.to_string_lossy();
            // base 传空：`file:` 转换结果自带根路径，`resolve_path_like` 遇到绝对路径不会用
            // base 目录；转换失败与文件探测失败共用同一个 not_found 文案。
            return match resolve_path_like(Path::new(""), &path) {
                Some(path) => Ok(path.to_string_lossy().into_owned()),
                None => Err(ResolveError::not_found(specifier)),
            };
        }
        if is_data_url(specifier) {
            // data: URL 没有文件可查：原样返回，payload 由 loader 以 URL 本身为模块名解码。
            return Ok(specifier.to_string());
        }

        // 1. Virtual keys, exact match incl. subpaths (§3.8 step 1). Aliases win over the raw key
        // so that a package entry like "typebox" resolves to its real file inside the namespace.
        //
        // 生产表把 22 个公开键全部别名到表中的文件（该表由 `build.rs` 生成），因此第一个分支
        // 通常直接命中规范文件名；`is_virtual_module_key` 是表未收录该键时（精简表 / 测试）的
        // 兜底，此时返回键本身，由 loader 用同名 `Module::declare` 提供源码。
        if let Some(name) = self.virtual_fs.canonical_name(specifier) {
            return Ok(name.to_string());
        }
        if is_virtual_module_key(specifier) {
            return Ok(specifier.to_string());
        }

        let is_relative =
            specifier.starts_with("./") || specifier.starts_with("../") || specifier == ".";
        if is_relative {
            // Relative imports inside a virtual namespace stay in the embedded table.
            //
            // 命名空间是封闭的：表里找不到就立刻失败，不会进入下面的宿主机路径解析。
            // 本文件 mod tests 的 virtual_module_relative_import_fails_outside_table 与
            // tests/module_resolver.rs::virtual_keys_include_exact_subpaths_and_aliases
            // 锁定了该行为。
            if let Some(name) = self.virtual_fs.resolve_relative(base_name, specifier) {
                return Ok(name.to_string());
            }
            if self.virtual_fs.is_virtual_key(base_name) || is_virtual_module_key(base_name) {
                return Err(ResolveError::not_found(specifier));
            }
        }

        // 2. Node builtins → `node:<name>` shim keys (§6.1).
        //
        // 任何 `node:` 前缀都原样接受；对应的 shim 源是否存在由 loader 的源注册表决定。
        if let Some(key) = builtin_module_key(specifier) {
            return Ok(key);
        }

        let directory = self.importer_dir(base_name);

        // 3a. Path-like specifiers follow jiti's file → directory → CJS → NodeNext order.
        //
        // 这一类不交给 oxc：jiti 的目录语义是「index.* 先于 package.json main」，与
        // oxc_resolver 不同，所以顺序在 `resolve_path_like` 里手写。
        if is_path_like(specifier) {
            return match resolve_path_like(&directory, specifier) {
                Some(path) => Ok(path.to_string_lossy().into_owned()),
                None => Err(ResolveError::not_found(specifier)),
            };
        }

        // 3b. Bare specifiers through `oxc_resolver`, trying jiti's primary condition set first
        // and the other one second (jiti falls back before Node's CJS resolver).
        //
        // 第一次失败的具体原因被丢弃（还有第二次机会）；只有两次都失败才折叠成 `not_found`，
        // 因此调用方看不到 exports 不匹配之类的细节。
        let (primary, fallback) = match kind {
            ResolutionKind::Import => (&self.import_resolver, &self.require_resolver),
            ResolutionKind::Require => (&self.require_resolver, &self.import_resolver),
        };
        for resolver in [primary, fallback] {
            if let Ok(resolution) = resolver.resolve(&directory, specifier) {
                return Ok(resolution.path().to_string_lossy().into_owned());
            }
        }
        // Resolution failure is reported with the loader's wording; the underlying
        // `oxc_resolver` reason is intentionally not leaked (design §3.8 step 4 fixes the text).
        Err(ResolveError::not_found(specifier))
    }

    /// Directory used as the resolution root for imports from `base`.
    ///
    /// File module names are absolute paths (with an optional `?v=` suffix). Non-path bases
    /// (`<inline>`, `builtin:*`, `node:*`, virtual keys) fall back to the extension directory,
    /// then to the process working directory.
    ///
    /// `base` is expected to be query-free: [`ModuleResolver::resolve_specifier`] passes
    /// `module_path(base)`. Only the shape of the name is inspected — the importer itself is never
    /// stat'ed, so a stale absolute path still contributes its parent directory.
    fn importer_dir(&self, base: &str) -> PathBuf {
        // A `file:` URL importer (Node ESM can declare modules under URL names) resolves relative
        // specifiers against its path.
        let base_path = if is_file_url(base) {
            file_url_to_path(base)
        } else {
            Some(PathBuf::from(base))
        };
        if let Some(path) = base_path
            && path.is_absolute()
        {
            // 绝对路径只取父目录；文件系统根没有父目录，返回根自己。
            return match path.parent() {
                Some(parent) => parent.to_path_buf(),
                None => path,
            };
        }
        // 非绝对路径（虚拟键、`<inline>`、`builtin:*`、`node:*`、不可转换的 `file:` URL）：
        // 退回扩展目录，再退回进程工作目录。相对路径的 base 字符串不参与任何拼接——它本身
        // 只是导入方标识，解析根取扩展目录或 cwd。
        if let Some(dir) = &self.extension_dir {
            return dir.clone();
        }
        std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
    }
}

/// Build one `oxc_resolver` with the §3.8 settings.
///
/// Only `extensions`, `condition_names`, `main_fields`, `main_files` (`["index"]`) and the optional
/// `tsconfig` are set; everything else keeps `ResolveOptions::default()`. `main_fields` contains
/// `main` only because jiti 2.7 never reads the bundler-only `module` field (see
/// `ModuleResolver::new`). The optional `tsconfig` is registered as manual discovery with
/// `TsconfigReferences::Disabled`.
fn build_oxc_resolver(
    condition_names: &[&str],
    main_fields: &[&str],
    extensions: Vec<String>,
    tsconfig: Option<PathBuf>,
) -> Resolver {
    let mut options = ResolveOptions {
        extensions,
        condition_names: condition_names
            .iter()
            .map(|name| (*name).to_string())
            .collect(),
        main_fields: main_fields
            .iter()
            .map(|field| (*field).to_string())
            .collect(),
        main_files: vec!["index".to_string()],
        ..ResolveOptions::default()
    };
    if let Some(config_file) = tsconfig {
        options.tsconfig = Some(TsconfigDiscovery::Manual(TsconfigOptions {
            config_file,
            references: TsconfigReferences::Disabled,
        }));
    }
    Resolver::new(options)
}

/// Whether `specifier` starts with `scheme` (ASCII case-insensitive, like WHATWG URLs).
///
/// Uses a char-boundary check so a specifier whose fifth byte falls inside a multi-byte UTF-8
/// character cannot panic the slice.
fn has_scheme(specifier: &str, scheme: &str) -> bool {
    specifier.len() >= scheme.len()
        && specifier.is_char_boundary(scheme.len())
        && specifier[..scheme.len()].eq_ignore_ascii_case(scheme)
}

/// Whether `specifier` is a `file:` URL.
fn is_file_url(specifier: &str) -> bool {
    has_scheme(specifier, "file:")
}

/// Whether `specifier` is a `data:` URL.
fn is_data_url(specifier: &str) -> bool {
    has_scheme(specifier, "data:")
}

/// Convert a `file:` URL to a filesystem path, mirroring Node's `fileURLToPath`.
///
/// Supported forms: `file:///abs/path`, `file://localhost/abs/path`, `file:/abs/path` and
/// percent-encoded paths. Query strings and fragments are not part of the pathname and are
/// ignored (Node semantics). A remote authority on unix, an encoded path separator or invalid
/// percent escapes yield `None` (Node throws `ERR_INVALID_FILE_URL_*` there).
///
/// Windows drive paths (`file:///C:/x`) keep forward slashes: the rest of the loader treats module
/// names verbatim and Rust's `Path` accepts `/` on Windows (design §3.4).
///
/// The authority rule is platform-independent: any non-empty authority other than `localhost`
/// yields `None`, so UNC shares are not handled here. The returned path is literal — no `..`
/// folding and no existence check. Percent escapes are decoded byte-wise and the result must be
/// valid UTF-8; a decoded NUL byte is rejected as well.
fn file_url_to_path(url: &str) -> Option<PathBuf> {
    let rest = url.get("file:".len()..)?;
    // `fileURLToPath` uses the URL pathname: `?search` and `#hash` are dropped.
    let rest = rest.split(['?', '#']).next().unwrap_or(rest);
    let path = if let Some(after_slashes) = rest.strip_prefix("//") {
        let (authority, path) = match after_slashes.split_once('/') {
            Some((authority, tail)) => (authority, format!("/{tail}")),
            None => (after_slashes, "/".to_string()),
        };
        if authority.is_empty() || authority.eq_ignore_ascii_case("localhost") {
            path
        } else {
            return None;
        }
    } else if rest.starts_with('/') {
        rest.to_string()
    } else {
        // `file:relative` is not a valid absolute file URL.
        return None;
    };

    // Node rejects encoded path separators in `file:` URLs.
    if path.contains("%2f") || path.contains("%2F") || path.contains("%5c") || path.contains("%5C")
    {
        return None;
    }
    let decoded = percent_decode(&path)?;
    let text = String::from_utf8(decoded).ok()?;
    if text.contains('\0') {
        return None;
    }
    // `file:///C:/x` has pathname `/C:/x`; Windows paths must not keep the URL's leading slash.
    #[cfg(windows)]
    if let Some(stripped) = text.strip_prefix('/')
        && stripped.as_bytes().get(1) == Some(&b':')
    {
        return Some(PathBuf::from(stripped));
    }
    Some(PathBuf::from(text))
}

/// Decode `%XX` escapes into bytes; `None` for a truncated or non-hex escape.
///
/// Non-`%` bytes are copied as-is (including multi-byte UTF-8 sequences), so the caller still has
/// to validate UTF-8; `%00` decodes to a NUL byte, which `file_url_to_path` then rejects.
fn percent_decode(value: &str) -> Option<Vec<u8>> {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            let high = hex_value(*bytes.get(index + 1)?)?;
            let low = hex_value(*bytes.get(index + 2)?)?;
            out.push(high * 16 + low);
            index += 3;
        } else {
            out.push(bytes[index]);
            index += 1;
        }
    }
    Some(out)
}

/// Value of one ASCII hex digit.
fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

/// Whether a specifier is a filesystem path rather than a bare package name.
///
/// Covers `.`/`..`, `./x`, `../x`, `/x` and platform-absolute paths (`Path::is_absolute`, so a
/// Windows `C:\x` counts too). This is broader than the `is_relative` check inside
/// [`ModuleResolver::resolve_specifier`]: that one decides whether a specifier must stay inside a
/// virtual namespace, this one decides whether the filesystem probe is used.
fn is_path_like(specifier: &str) -> bool {
    specifier.starts_with("./")
        || specifier.starts_with("../")
        || specifier.starts_with('/')
        || specifier == "."
        || specifier == ".."
        || Path::new(specifier).is_absolute()
}

/// Resolve a path-like specifier with jiti 2.7's exact probe order (see module docs).
///
/// Absolute specifiers ignore `base_dir`; relative ones are joined onto it. Every step is a plain
/// `is_file()`/`is_dir()` check and the first hit is returned through `realpath`. Returns `None`
/// when nothing matches; the caller maps that to [`ResolveError`] via `not_found`.
///
/// The order is load-bearing and pinned by `tests/module_resolver.rs` on a fixture tree; the
/// comments on each step record the measured jiti 2.7 behavior they implement.
fn resolve_path_like(base_dir: &Path, specifier: &str) -> Option<PathBuf> {
    let target = if Path::new(specifier).is_absolute() {
        PathBuf::from(specifier)
    } else {
        base_dir.join(specifier)
    };

    // 1. Exact file (jiti `_resolve` checks `isAbsolute` + `statSync().isFile()` first, and the
    // ESM resolver's finalize step realpaths it).
    //    带扩展名的 specifier 也先走这一步；只有精确文件不存在时才进入下面的追加探测。
    if target.is_file() {
        return Some(realpath(&target));
    }

    // 2. `{spec}{extension}` in jiti's extension order.
    //    `append_extension` 是纯字符串拼接：`./x.js` 在这里会探测 `x.js.js` 等，而不是把
    //    `.js` 换成 `.ts`（后者是第 5 步的职责）。
    for extension in JITI_EXTENSIONS {
        let candidate = append_extension(&target, extension);
        if candidate.is_file() {
            return Some(realpath(&candidate));
        }
    }

    // 3. `{spec}/index{extension}` (before `package.json` `main`, unlike oxc_resolver).
    for extension in JITI_EXTENSIONS {
        let candidate = target.join(format!("index{extension}"));
        if candidate.is_file() {
            return Some(realpath(&candidate));
        }
    }

    // 4. Node's CJS fallback (`nativeRequire.resolve` in jiti): legacy main, `.json`/`.node`
    //    extension probing, directory index files.
    //
    //    目标不存在时 `is_file()`/`is_dir()` 都为假，走 else 分支再拼一次
    //    `.js/.json/.node`：`.js` 与第 2 步重复，新增的只有 `.json`/`.node`。
    if target.is_dir() {
        if let Some(main) = legacy_main(&target) {
            return Some(realpath(&main));
        }
        for extension in [".js", ".json", ".node"] {
            let candidate = target.join(format!("index{extension}"));
            if candidate.is_file() {
                return Some(realpath(&candidate));
            }
        }
    } else {
        for extension in NODE_CJS_EXTENSIONS {
            let candidate = append_extension(&target, extension);
            if candidate.is_file() {
                return Some(realpath(&candidate));
            }
        }
    }

    // 5. NodeNext-style extension substitution (`./mod.js` → `./mod.ts` when `mod.js` is absent).
    //
    //    替换基于原始 specifier（不是第 1 步 join 出来的 `target`），因此绝对/相对性要重新
    //    判断；替换后只查精确文件，不再做 index / package.json 兜底。
    let substituted = substitute_ts_extension(specifier)?;
    let candidate = if Path::new(&substituted).is_absolute() {
        PathBuf::from(substituted)
    } else {
        base_dir.join(substituted)
    };
    candidate.is_file().then(|| realpath(&candidate))
}

/// `path + extension` (never `with_extension`: `./x.js` + `.ts` must not become `./x.ts`).
fn append_extension(path: &Path, extension: &str) -> PathBuf {
    let mut candidate = path.as_os_str().to_os_string();
    candidate.push(extension);
    PathBuf::from(candidate)
}

/// Node's legacy `package.json` `main` resolution for a directory.
///
/// Probe order: the literal `main` file → `main` + `.js/.json/.node` → `{main}/index.*` →
/// `{directory}/index.*` (last-resort fallback, measured on Node). Reading the manifest, parsing
/// it, and finding a non-string or empty `main` all collapse into `None`; the error is dropped and
/// the caller simply continues probing (a broken `package.json` is therefore indistinguishable
/// from a missing `main` here).
fn legacy_main(directory: &Path) -> Option<PathBuf> {
    let manifest = std::fs::read_to_string(directory.join("package.json")).ok()?;
    let value: serde_json::Value = serde_json::from_str(&manifest).ok()?;
    let main = value.get("main").and_then(|value| value.as_str())?;
    if main.is_empty() {
        return None;
    }
    let main_path = directory.join(main);
    if main_path.is_file() {
        return Some(main_path);
    }
    for extension in NODE_CJS_EXTENSIONS {
        let candidate = append_extension(&main_path, extension);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    for extension in NODE_CJS_EXTENSIONS {
        let candidate = main_path.join(format!("index{extension}"));
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    // Node falls back to `{directory}/index.*` when `main` does not resolve (measured).
    for extension in NODE_CJS_EXTENSIONS {
        let candidate = directory.join(format!("index{extension}"));
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

/// `.js` → `.ts`, `.mjs` → `.mts`, `.cjs` → `.cts` (and the `.jsx` variants), like jiti's
/// `t.replace(/\.(c|m)?j(sx?)$/, ".$1t$2")`.
///
/// Only the final suffix is considered; a specifier ending in anything else returns `None`, which
/// makes the whole path resolution fail. The substitution is purely lexical — the caller checks
/// whether the substituted file exists.
fn substitute_ts_extension(specifier: &str) -> Option<String> {
    for (from, to) in TS_EXTENSION_SUBSTITUTIONS {
        if let Some(stem) = specifier.strip_suffix(from) {
            return Some(format!("{stem}{to}"));
        }
    }
    None
}

/// Canonical absolute path (jiti realpaths resolved files); lexical fallback when the file
/// system cannot canonicalize (e.g. Windows `\\?\` paths are left alone).
///
/// On unix `canonicalize` also resolves symlinks and removes `.`/`..`, so the returned module name
/// can differ textually from the path that was probed (the `node_modules` symlink test asserts
/// this). A canonicalize failure (raced deletion, permissions) silently returns the input path;
/// whether that path can actually be read is decided later by the loader's `fs::read`.
fn realpath(path: &Path) -> PathBuf {
    #[cfg(unix)]
    {
        std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
    }
    #[cfg(not(unix))]
    {
        path.to_path_buf()
    }
}

impl rquickjs::loader::Resolver for ModuleResolver {
    /// ESM 导入入口：一律按 [`ResolutionKind::Import`] 解析，导入属性（`_attributes`，例如
    /// `with { type: "json" }`）在这里被忽略；`data:` URL 的属性由 loader 的 prepare 阶段读取。
    ///
    /// 解析失败时不返回 [`ResolveError`] 的 `Error loading module ...` 文案，而是把
    /// `Cannot find module '<spec>'` + `Require stack` 作为 JS 异常抛到 `ctx` 上（TS 侧
    /// Node/jiti 的形状）；`ctx` 只用于抛这个异常。`&mut self` 是 trait 要求的形态，实现本身
    /// 不修改状态。
    fn resolve<'js>(
        &mut self,
        ctx: &rquickjs::Ctx<'js>,
        base: &str,
        name: &str,
        _attributes: Option<rquickjs::loader::ImportAttributes<'js>>,
    ) -> rquickjs::Result<String> {
        self.resolve_specifier(base, name, ResolutionKind::Import)
            .map_err(|_| not_found_exception(ctx, base, name))
    }
}

/// jiti/Node 的解析失败文案：`Cannot find module '<specifier as written>'` + Require stack。
///
/// TS 侧由 Node 的 `Module._resolveFilename`/jiti 抛出（`<specifier>` 保持源里的写法，栈顶是
/// 导入方文件）；rquickjs 的 `Error loading module '<resolved>': …` 形状不同，因此在 resolver
/// 里直接抛 JS 异常。
///
/// `Exception::throw_message` 会把消息作为挂起异常挂到 `ctx` 上，返回的 `rquickjs::Error` 只供
/// 调用方用 `?`/`map_err` 继续传播。这条路径不使用 [`ResolveError::to_rquickjs_error`] 的
/// loading 文案，两者不可混用。
pub(crate) fn not_found_exception(
    ctx: &rquickjs::Ctx<'_>,
    base: &str,
    specifier: &str,
) -> rquickjs::Error {
    use rquickjs::Exception;
    Exception::throw_message(
        ctx,
        &format!(
            "Cannot find module '{specifier}'\nRequire stack:\n- {}",
            require_stack_importer(base)
        ),
    )
}

/// `Require stack` 的导入方：真实文件用其干净路径；虚拟/内建模块退回加载器源码路径。
///
/// `base` 的 `?v=<generation>-<seq>` 由 `module_path` 去掉；判断只依据 `Path::is_absolute()`，
/// 因此 `file:` URL 形式的 base（`file:///…`）在 unix 上不算绝对路径，同样退回加载器路径。
pub(crate) fn require_stack_importer(base: &str) -> String {
    let clean = module_path(base);
    if std::path::Path::new(clean).is_absolute() {
        clean.to_string()
    } else {
        super::loader_impl::loader_require_stack_path().to_string()
    }
}

// 内联单元测试只覆盖不需要 fixture 文件树的路径：虚拟表查找/别名/相对导入、22 键的精确匹配、
// 内建名映射，以及固定错误文案。真实文件系统的探测顺序（jiti 的 index/main/扩展名优先级）由
// `tests/module_resolver.rs` 的 fixture 覆盖。
#[cfg(test)]
mod tests {
    use super::*;

    static FILES: [VirtualFile; 3] = [
        VirtualFile {
            name: "typebox/build/index.mjs",
            source: b"export const a = 1;",
        },
        VirtualFile {
            name: "typebox/build/_index.mjs",
            source: b"export const b = 1;",
        },
        VirtualFile {
            name: "typebox/build/compile/index.mjs",
            source: b"export const c = 1;",
        },
    ];
    static ALIASES: [VirtualAlias; 2] = [
        VirtualAlias {
            key: "typebox",
            target: "typebox/build/index.mjs",
        },
        VirtualAlias {
            key: "typebox/compile",
            target: "typebox/build/compile/index.mjs",
        },
    ];

    fn fs() -> VirtualFs {
        VirtualFs::new(&FILES, &ALIASES)
    }

    #[test]
    fn virtual_table_lookups() {
        let vfs = fs();
        assert_eq!(
            vfs.canonical_name("typebox"),
            Some("typebox/build/index.mjs")
        );
        assert_eq!(vfs.lookup("typebox"), Some(&b"export const a = 1;"[..]));
        assert_eq!(
            vfs.lookup("typebox/build/_index.mjs"),
            Some(&b"export const b = 1;"[..])
        );
        assert!(vfs.is_virtual_key("typebox/compile"));
        assert!(!vfs.is_virtual_key("typebox/unknown"));
        assert_eq!(
            vfs.resolve_relative("typebox", "./_index.mjs"),
            Some("typebox/build/_index.mjs")
        );
        assert_eq!(
            vfs.resolve_relative("typebox/build/index.mjs", "./compile/index.mjs"),
            Some("typebox/build/compile/index.mjs")
        );
        assert_eq!(vfs.resolve_relative("typebox", "../escape.mjs"), None);
    }

    #[test]
    fn virtual_keys_are_exact() {
        assert!(is_virtual_module_key(
            "@earendil-works/kiana-ai/providers/all"
        ));
        assert!(!is_virtual_module_key("@earendil-works/kiana-ai/providers"));
        assert!(!is_virtual_module_key("typebox/build/index.mjs"));
    }

    #[test]
    fn builtins_map_to_shim_keys() {
        assert_eq!(builtin_module_key("fs").as_deref(), Some("node:fs"));
        assert_eq!(
            builtin_module_key("fs/promises").as_deref(),
            Some("node:fs/promises")
        );
        assert_eq!(
            builtin_module_key("node:path").as_deref(),
            Some("node:path")
        );
        assert_eq!(
            builtin_module_key("node:sqlite").as_deref(),
            Some("node:sqlite")
        );
        assert_eq!(builtin_module_key("ms"), None);
        assert_eq!(builtin_module_key("@earendil-works/kiana-ai"), None);
    }

    #[test]
    fn resolve_error_display_matches_loader_wording() {
        let error = ResolveError::not_found("./missing");
        assert_eq!(
            error.to_string(),
            "Error loading module './missing': No such file or directory (os error 2)"
        );
    }

    #[test]
    fn virtual_resolution_needs_no_filesystem() {
        let resolver = ModuleResolver::new(ResolverOptions {
            extension_dir: None,
            virtual_fs: fs(),
        });
        assert_eq!(
            resolver
                .resolve_specifier("/tmp/x.ts?v=1-1", "typebox", ResolutionKind::Import)
                .unwrap(),
            "typebox/build/index.mjs"
        );
        assert_eq!(
            resolver
                .resolve_specifier(
                    "typebox/build/index.mjs",
                    "./_index.mjs",
                    ResolutionKind::Import
                )
                .unwrap(),
            "typebox/build/_index.mjs"
        );
        assert_eq!(
            resolver
                .resolve_specifier(
                    "/tmp/x.ts",
                    "@earendil-works/kiana-ai/models",
                    ResolutionKind::Import
                )
                .unwrap(),
            "@earendil-works/kiana-ai/models"
        );
        assert_eq!(
            resolver
                .resolve_specifier("/tmp/x.ts", "node:fs", ResolutionKind::Import)
                .unwrap(),
            "node:fs"
        );
        assert_eq!(
            resolver
                .resolve_specifier("/tmp/x.ts?v=9-1", "./nope", ResolutionKind::Import)
                .unwrap_err()
                .to_string(),
            "Error loading module './nope': No such file or directory (os error 2)"
        );
    }

    #[test]
    fn virtual_module_relative_import_fails_outside_table() {
        let resolver = ModuleResolver::new(ResolverOptions {
            extension_dir: None,
            virtual_fs: VirtualFs::empty(),
        });
        assert!(
            resolver
                .resolve_specifier(
                    "@earendil-works/kiana-tui",
                    "./x.js",
                    ResolutionKind::Import
                )
                .is_err()
        );
    }
}
