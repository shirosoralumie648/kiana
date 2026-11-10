//! TS 去类型（oxc）：`spec/design-extension-runtime.md` §3.2。
//!
//! 输入 `.ts/.tsx/.mts/.cts`，输出 ESM 文本；与 jiti/Babel 路径等价（含 `import.meta`、装饰器、enum）。
//! 流水线与设计文档逐字一致：
//!
//! ```text
//! SourceType::from_path(...) -> Parser -> SemanticBuilder{enum_eval} -> Transformer{legacy decorators} -> Codegen
//! ```
//!
//! 两个硬约束（§3.2，实测）：
//! - `SemanticBuilder::with_enum_eval(true)` 是**必需**的：oxc 0.155 的 `Transformer` 在缺少
//!   enum 求值信息时会 panic。
//! - `Transformer` **不做** ESM→CJS 降级；CJS 由 [`super::cjs`] 的真 CJS 包装器处理。
//!
//! 与 `loader.ts`/jiti 的对应关系：jiti 用 Babel（`@babel/preset-typescript`）去类型；这里用 oxc。
//! 语义等价范围见设计文档 §3.2 的表格；JSX 可解析但本期不做变换（语料无 JSX）。
//!
//! # 在调用链中的位置
//!
//! 本模块只做文本变换：不读文件、不接触 `rquickjs`，因此可以直接单测（`tests/ts_transpile.rs`）。
//! 主要的生产调用方是 `loader::loader_impl` 的 `ExtensionLoader::prepare_with`：
//!
//! - TS 文件（[`is_typescript`] 为真）→ [`strip_types`]；失败经调用方的 `transpile_error`
//!   转成 `ParseError: <message>  \n <path>:<line>:<column>` 形态的加载错误；
//! - 原生 ESM 的 `.js`/`.mjs` → [`parse_error_message`] 做声明前语法预检（QuickJS 自己的
//!   SyntaxError 文案与行列口径和 Babel 不同）；
//! - 需要补 jiti 包装器绑定的 `.js`/`.mjs` → `inject_cjs_bindings`（`pub(crate)`）；
//! - [`strip_bom`] / `module_path` 还被 `loader`、`loader::resolver` 与 `loader::cjs` 共用。
//!
//! # 另一处文本启发式
//!
//! 判断“输出是否按 ESM 求值”时，除了顶层 ESM 语句还检查源码文本是否包含 `import.meta`
//! （它不是顶层语句，AST 扫描看不到）。注释或字符串里出现同样文本也会命中；该判定只决定
//! CJS 绑定前导是否**允许**注入，真正注入还要求存在未声明的自由引用。
//!
//! # jiti 的 CJS 绑定补齐（只作用于会以 ESM 求值的 TS 模块）
//!
//! jiti 把每个 TS 模块编译成 CJS，再放进
//! `function (exports, require, module, __filename, __dirname, …) { … }` 里求值。因此 TS 源码里的
//! 自由 `require` / `__filename` / `__dirname` / `module` / `exports` 在 jiti 下总是可用（例如语料的
//! `examples/extensions/notify.ts` 在 `type: module` 包里调用 `require("child_process")`，而
//! `exports.value = …` 与 `module.exports = …` 在 jiti 2.7 中对带 ESM 语法的 TS 模块同样生效）。
//! oxc 不做 ESM→CJS，本移植保留 ESM 输出，只在这五个名字**在求值代码里确实未被声明**时注入等价的
//! 模块级绑定。检测在类型擦除**之后**的代码上重新做（见 `unresolved_bindings`）：这样
//! `declare const __dirname: string` / `declare function require(…)` 这类**不产生运行期绑定**的
//! ambient 声明不会误抑制注入（jiti 的包装器参数不受它们影响）。
//!
//! 已知近似（架构决定的 ESM 输出使然，记录在案）：`module.exports = …` 只能改这个合成记录，
//! 不能替换 ESM 模块的 `default` 导出；jiti 的 CJS 输出可以。混合写法的模块（同时有 ESM 语法与
//! `module.exports` 赋值）极少见，语料中没有。
//!
//! legacy 装饰器会让 oxc 生成 `@oxc-project/runtime/helpers/*` 的 import；jiti/Babel 则把 helper
//! 内联进模块。为了让装饰器在只有本 crate 的运行时里也能加载（U10 不提供该包），这里把已知 helper
//! 的源码内联到模块顶部（来源：`@oxc-project/runtime@0.155.0`，MIT，函数体直接取自上游）。
#![cfg(feature = "runtime")]

use std::fmt;
use std::fmt::Write as _;
use std::path::Path;

use oxc_allocator::Allocator;
use oxc_ast::ast::{Ident, Program, Statement};
use oxc_codegen::Codegen;
use oxc_parser::{ParseOptions, Parser};
use oxc_semantic::SemanticBuilder;
use oxc_span::SourceType;
use oxc_transformer::{JsxOptions, TransformOptions, Transformer};

/// Extensions handled by the transpiler (design §3.1).
///
/// jiti 2.7's `JITI_EXTENSIONS` is `.js .mjs .cjs .ts .tsx .mts .cts .mtsx .ctsx`; the two
/// `.x` TypeScript forms are TSX variants and are transpiled like `.tsx` (review finding 7).
///
/// 判断用 `ends_with` 遍历，与数组顺序无关；`.mtsx`/`.ctsx` 是 jiti 扩展名表里的 TSX 变体，
/// oxc 的 `SourceType::from_path` 不认识它们，由 [`source_type_for`] 特判为 TSX。
pub const TYPESCRIPT_EXTENSIONS: [&str; 6] = [".ts", ".tsx", ".mts", ".cts", ".mtsx", ".ctsx"];

/// UTF-8 BOM (`\u{FEFF}`). Node and jiti strip it before compiling a module, so the loader strips
/// it before any branch (TS file, JS file or CJS wrapper).
///
/// 只在源码**开头**剥离一次（[`strip_bom`]）；出现在源码中间的 U+FEFF 是合法的空白字符
/// （ZWNBSP），不会导致解析错误，因此不做处理。单测 `strips_bom_once` 锁定其保留。
pub const UTF8_BOM: char = '\u{feff}';

/// Strip a single leading UTF-8 BOM. All other content is preserved byte for byte.
///
/// TS: `jiti` strips the BOM before handing source to Babel.
///
/// 只剥离一个前缀并返回借用切片；输入不含 BOM 时原样返回。
pub fn strip_bom(source: &str) -> &str {
    source.strip_prefix(UTF8_BOM).unwrap_or(source)
}

/// Whether `name` (a module name or path) is a TypeScript source the transpiler must handle.
///
/// A `?v=<generation>-<seq>` cache-busting query (design §3.4) is ignored: the module name is
/// `"<abs path>?v=3-7"`, and its source type is determined by the real path.
///
/// 先经 `module_path` 去掉 query，再按后缀逐字比较（大小写敏感，因此 `.TS` 不算）。
pub fn is_typescript(name: &str) -> bool {
    let path = module_path(name);
    TYPESCRIPT_EXTENSIONS.iter().any(|ext| path.ends_with(ext))
}

/// oxc [`SourceType`] for a TypeScript path.
///
/// oxc 0.155 does not know jiti's `.mtsx`/`.ctsx` extensions (`SourceType::from_path` fails), but
/// they are TSX forms, so they must be parsed with JSX enabled like `.tsx` (review finding 7).
///
/// 其余路径交给 `SourceType::from_path`，它不认识的扩展名回退 `SourceType::ts()`。
/// 本函数**不**剥离 `?v=`：调用方要传真实路径（[`strip_types`] 内部先调 `module_path`；
/// `parse_error_message` 的调用方传的也是去掉 query 的 `real`）。
pub fn source_type_for(path: &str) -> SourceType {
    if path.ends_with(".mtsx") || path.ends_with(".ctsx") {
        return SourceType::tsx();
    }
    SourceType::from_path(Path::new(path)).unwrap_or_else(|_| SourceType::ts())
}

/// Error raised when a module cannot be transpiled.
///
/// TS: jiti rethrows the Babel error from `jiti.import()`; `loader.ts` wraps it in
/// `Failed to load extension: <message>`. The parse wording is oxc's rather than Babel's (recorded
/// in the unit report); the diagnostic list is preserved in source order.
///
/// 只在 [`strip_types`] 中构造；[`parse_error_message`] 只返回字符串，不产生该错误类型。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TranspileError {
    /// The parser reported at least one diagnostic. Any diagnostic is a load error (§3.2).
    Parse {
        /// Module name passed to [`strip_types`], including any `?v=` suffix.
        path: String,
        /// oxc diagnostic messages (`OxcDiagnostic::to_string`), in source order.
        diagnostics: Vec<String>,
        /// First diagnostic in jiti/Babel's error shape (`<message>  \n <path>:<line>:<column>`).
        babel: Option<String>,
    },
}

impl TranspileError {
    /// The module name the failure belongs to.
    ///
    /// 即传给 [`strip_types`] 的 `name` 原文，可能带 `?v=`（测试 `/tmp/broken.ts?v=1-1` 锁定）。
    pub fn path(&self) -> &str {
        match self {
            TranspileError::Parse { path, .. } => path,
        }
    }

    /// The diagnostics collected from the parser (empty for non-parse failures).
    ///
    /// 每条是 `OxcDiagnostic::to_string` 的结果，保留 oxc 原文；当前枚举只有解析一种失败，
    /// 所以实际不会为空。
    pub fn diagnostics(&self) -> &[String] {
        match self {
            TranspileError::Parse { diagnostics, .. } => diagnostics,
        }
    }

    /// First diagnostic in jiti/Babel's shape (no `ParseError: ` prefix).
    ///
    /// `Unexpected token  \n <path>:<line>:<column>`: Babel appends two spaces, a newline and a
    /// space before the `path:line:column` suffix; the column is 0-based (UTF-16 units).
    ///
    /// 诊断没有 label（拿不到位置）时只有 message 本身，没有后缀。
    pub fn babel_message(&self) -> Option<&str> {
        match self {
            TranspileError::Parse { babel, .. } => babel.as_deref(),
        }
    }
}

impl fmt::Display for TranspileError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TranspileError::Parse {
                path, diagnostics, ..
            } => {
                write!(f, "Failed to parse {path}")?;
                for (i, diagnostic) in diagnostics.iter().enumerate() {
                    if i == 0 {
                        write!(f, ": {diagnostic}")?;
                    } else {
                        write!(f, "\n{diagnostic}")?;
                    }
                }
                Ok(())
            }
        }
    }
}

impl std::error::Error for TranspileError {}

/// oxc 的第一条语法诊断，格式化为 jiti/Babel 的 `<message>  \n <path>:<line>:<col>`。
///
/// 用于 `.js`/`.mjs`（不经 [`strip_types`]）的语法预检：Node 的 jiti 用 Babel 解析，
/// QuickJS 自己的 SyntaxError 措辞与位置口径都不同。
///
/// 只报告第一条诊断（Babel 的 `ParseError` 只带一个位置）；诊断没有 label 时返回纯
/// `message`，不加 `path:line:column`。调用方负责加 `ParseError: ` 前缀
/// （`loader_impl` 对原生 `.js`/`.mjs` 的预检就是这样）。解析选项是
/// `ParseOptions::default()`，`source_type` 由路径决定（含 `.tsx` 的 JSX）。
pub fn parse_error_message(path: &str, source: &str) -> Option<String> {
    let alloc = Allocator::default();
    let ret = Parser::new(&alloc, source, source_type_for(path))
        .with_options(ParseOptions::default())
        .parse();
    let diagnostic = ret.diagnostics.first()?;
    Some(match diagnostic.labels.first() {
        Some(label) => {
            let (line, column) = line_column(source, label.span().start as usize);
            format!("{}  \n {path}:{line}:{column}", diagnostic.message)
        }
        None => diagnostic.message.to_string(),
    })
}

/// `(1 基行, 0 基 UTF-16 列)`——与 Babel/jiti 的 `loc`/`line:column` 口径一致。
///
/// 偏移超出源码长度时返回最后一行的末尾位置（循环自然结束），不会 panic。
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

/// Strip TypeScript syntax from `source` and return ESM JavaScript text.
///
/// The pipeline is the frozen one from `spec/design-extension-runtime.md` §3.2. One
/// [`Allocator`] per file; the AST never escapes this function.
///
/// `name` 可带 `?v=`：解析用的扩展名经 `module_path` 去掉 query 后交给 [`source_type_for`]，
/// 错误里的 `path` 字段却保留调用方传入的原文。函数内部先 [`strip_bom`]。
///
/// 无论输入形态如何，返回值都是 ESM 文本（本函数不做 ESM→CJS）；需要 CJS 求值时由
/// `loader::cjs::wrap_module` 负责包装。除解析错误外没有其它失败形态：变换器诊断被忽略，
/// 未解析引用分析失败时选择不注入前导（见 `unresolved_bindings`）。
///
/// Errors: every parser diagnostic is fatal ([`TranspileError::Parse`]).
pub fn strip_types(name: &str, source: &str) -> Result<String, TranspileError> {
    let source = strip_bom(source);
    let path = module_path(name);
    let source_type = source_type_for(path);

    // 阶段 1：解析。source_type 由真实路径决定（`.tsx`/`.mtsx`/`.ctsx` 开 JSX），
    // 诊断一个都不放过；同时把第一条整理成 Babel 风格文案（`babel`）供调用方使用。
    let alloc = Allocator::default();
    let ret = Parser::new(&alloc, source, source_type)
        .with_options(ParseOptions::default())
        .parse();
    if !ret.diagnostics.is_empty() {
        let diagnostics: Vec<String> = ret.diagnostics.iter().map(ToString::to_string).collect();
        let babel = ret.diagnostics.first().map(|diagnostic| {
            let message = diagnostic.message.as_ref();
            let position = diagnostic.labels.first().map(|label| label.span().start);
            match position {
                Some(offset) => {
                    let (line, column) = line_column(source, offset as usize);
                    format!("{message}  \n {path}:{line}:{column}")
                }
                None => message.to_owned(),
            }
        });
        return Err(TranspileError::Parse {
            path: name.to_string(),
            diagnostics,
            babel,
        });
    }

    let mut program = ret.program;
    // 阶段 2：判定输出是否按 ESM 求值。CJS 形态的 TS（`export =` / `import x = require`）
    // 不注入 jiti 包装器绑定：那份代码会交给真 CJS 包装器，由函数参数提供同名绑定。
    // `import.meta` 不是顶层语句，AST 扫描看不到，只能靠文本检查补上（见模块文档）。
    // jiti only exposes the CJS bindings to modules it evaluates as CJS; a TS module with ESM
    // syntax still gets them because Babel's transform wraps it in jiti's CJS evaluator.
    let is_esm_output = program_has_esm_syntax(&program) || source.contains("import.meta");
    // 阶段 3：语义分析。
    // MANDATORY: `Transformer` panics in oxc 0.155 without enum evaluation (design §3.2).
    let semantic = SemanticBuilder::new()
        .with_enum_eval(true)
        .build(&program)
        .semantic;
    let scoping = semantic.into_scoping();

    // 阶段 4：变换（去类型 + legacy 装饰器）。
    let mut options = TransformOptions::default();
    // jiti/Babel run legacy (experimental) decorators (`experimentalDecorators`).
    options.decorator.legacy = true;
    // No JSX transform in scope (§3.2: "JSX/TSX 可解析但本期不做变换"). Babel without preset-react
    // leaves JSX untouched, which is what jiti does; emitting a `react/jsx-runtime` import would
    // invent a dependency the TS reference never asks for.
    options.jsx = JsxOptions::disable();
    let transformer = Transformer::new(&alloc, Path::new(path), &options);
    // Transformer diagnostics are not load errors (§3.2: only parser diagnostics are).
    let _ = transformer.build_with_scoping(scoping, &mut program);

    // 阶段 5：codegen，并把 oxc runtime helper 的 import 内联成模块顶部的定义。
    let mut code = Codegen::new().build(&program).code;
    code = inline_oxc_runtime_helpers(code);
    if is_esm_output {
        // 阶段 6：按需注入 jiti 包装器的 CJS 绑定（只对未被声明、且在输出代码里仍自由的
        // `require` / `__filename` / `__dirname` / `module` / `exports` 生成前导）。
        // Analyse the *emitted* code, not the TS source: type erasure turns ambient declarations
        // (`declare const __dirname: string`) into no binding at all, so the pre-transform
        // scoping would wrongly think the name is still declared (review finding 9).
        let bindings = unresolved_bindings(path, &code);
        let prelude = cjs_bindings_prelude(name, bindings);
        if !prelude.is_empty() {
            // A `#!` line must stay the first line of the module.
            let insert_at = if code.starts_with("#!") {
                code.find('\n').map_or(code.len(), |index| index + 1)
            } else {
                0
            };
            code.insert_str(insert_at, &prelude);
        }
    }
    Ok(code)
}

/// Give an ESM `.js`/`.mjs` source the CJS bindings jiti's evaluator would provide.
///
/// jiti native-imports a `.js`/`.mjs` first and falls back to its Babel CJS transform when the
/// module throws `require is not defined` at evaluation. This port keeps the module ESM and only
/// injects the wrapper bindings the source references without declaring them (the same analysis
/// and prelude [`strip_types`] uses for TypeScript).
///
/// Returns `None` when no binding is needed; returns `Some` only when a prelude was inserted.
///
/// 输入已经是 JavaScript（不再经过 oxc 变换），但仍会先 [`strip_bom`]，并在需要时把前导插到
/// shebang 之后。本函数**不**判断模块最终按 ESM 还是 CJS 求值：调用方只在“不交给真 CJS
/// 包装器”的分支调用它（`loader_impl` 对原生 ESM 还要先做 `eager_cjs_reference` 判定）。
pub(crate) fn inject_cjs_bindings(name: &str, source: &str) -> Option<String> {
    let source = strip_bom(source);
    let path = module_path(name);
    let bindings = unresolved_bindings(path, source);
    if !bindings.any() {
        return None;
    }
    let prelude = cjs_bindings_prelude(name, bindings);
    if prelude.is_empty() {
        return None;
    }
    let insert_at = if source.starts_with("#!") {
        source.find('\n').map_or(source.len(), |index| index + 1)
    } else {
        0
    };
    let mut code = String::with_capacity(source.len() + prelude.len());
    code.push_str(&source[..insert_at]);
    code.push_str(&prelude);
    code.push_str(&source[insert_at..]);
    Some(code)
}

/// Whether the program's top level contains static ESM syntax.
///
/// Mirrors jiti's `hasESMSyntax` intent: `import`/`export` declarations make the module ESM.
/// TypeScript-only forms (`import x = require(...)`, `export = x`) are CJS, not ESM.
///
/// oxc 0.155 splits the ESTree `ExportNamedDeclaration` into
/// [`Statement::ExportDeclaration`] (`export const|let|var|function|class|enum|interface|type X`)
/// and [`Statement::ExportFromDeclaration`] (`export { a } from "./x"`); both are the most common
/// ESM forms and must be matched (review finding 1).
///
/// 只看顶层语句：`import.meta` 不是语句，检测不到（[`strip_types`] 另做文本检查）；
/// TS 的 `export =` / `import x = require(...)` 是 CJS 形态，不算 ESM。单测
/// `declaration_exports_are_esm_syntax` 逐条锁定这些归类。
pub(crate) fn program_has_esm_syntax(program: &Program<'_>) -> bool {
    program.body.iter().any(|statement| {
        matches!(
            statement,
            Statement::ImportDeclaration(_)
                | Statement::ExportAllDeclaration(_)
                | Statement::ExportDefaultDeclaration(_)
                | Statement::ExportNamedDeclaration(_)
                | Statement::ExportDeclaration(_)
                | Statement::ExportFromDeclaration(_)
        )
    })
}

/// Which jiti CJS bindings the emitted module references but does not declare itself.
#[derive(Debug, Clone, Copy, Default)]
struct CjsBindings {
    require: bool,
    filename: bool,
    dirname: bool,
    module: bool,
    exports: bool,
    /// Whether the emitted code already declares a root-level `module` binding of its own.
    module_declared: bool,
}

impl CjsBindings {
    /// Whether any binding was requested (cheap early-out for the textual pre-filter).
    fn any(self) -> bool {
        self.require || self.filename || self.dirname || self.module || self.exports
    }
}

/// Names jiti's CJS evaluator always provides as wrapper parameters.
const JITI_BINDING_NAMES: [&str; 5] = ["require", "__filename", "__dirname", "module", "exports"];

/// Which jiti CJS bindings `code` references at module scope without declaring them.
///
/// `code` is the emitted JavaScript (after type erasure), so ambient TS declarations no longer
/// contribute a binding: `declare const __dirname: string` disappears and the reference becomes
/// unresolved again, exactly like under jiti's wrapper (review finding 9).
///
/// A textual pre-filter keeps the extra parse off most modules; when the emitted code does not
/// mention any of the five names at all there is nothing to inject.
///
/// 预过滤只是省一次解析：命中子串不代表真的引用（注释/字符串也会命中），是否注入最终由 oxc
/// 的语义作用域决定。二次解析失败（正常 codegen 不应发生）时返回默认值：宁可不注入，也不
/// 凭猜测插入绑定。
fn unresolved_bindings(path: &str, code: &str) -> CjsBindings {
    if !JITI_BINDING_NAMES.iter().any(|name| code.contains(name)) {
        return CjsBindings::default();
    }
    // JSX survives the transform (no JSX lowering in scope), so parse `.tsx` output as JSX.
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
    let ret = Parser::new(&alloc, code, source_type)
        .with_options(ParseOptions::default())
        .parse();
    if !ret.diagnostics.is_empty() {
        // The code came from our own codegen; a parse failure means an exotic construct. Do not
        // inject anything rather than guessing (the previous behaviour).
        return CjsBindings::default();
    }
    let semantic = SemanticBuilder::new().build(&ret.program).semantic;
    let scoping = semantic.scoping();
    let unresolved = scoping.root_unresolved_references();
    let has = |name: &str| unresolved.contains_key(name);
    CjsBindings {
        require: has("require"),
        filename: has("__filename"),
        dirname: has("__dirname"),
        module: has("module"),
        exports: has("exports"),
        // `exports` needs `module.exports`; only reuse a declaration the user wrote themselves.
        module_declared: scoping.get_root_binding(Ident::from("module")).is_some(),
    }
}

/// Module-level bindings jiti's CJS evaluator would provide to a TS module, injected only for
/// unresolved names and only in ESM output (see module docs).
///
/// 生成顺序固定为 `module` 记录 → `exports` → `require` → `__filename` → `__dirname`：
/// `var exports = module.exports` 必须在这条记录赋值之后执行。路径字符串经
/// `cjs::js_string_literal` 转义；`require` 的绑定在**求值期**调用
/// `globalThis.__kiana_make_require`，依赖 loader 已安装 CJS 桥（TS 模块总是需要该桥，见
/// `loader_impl::needs_cjs_bridge`）。前导只包含被引用且未解析的名字，因此可能只生成其中
/// 几行；空字符串表示无需注入。
fn cjs_bindings_prelude(name: &str, bindings: CjsBindings) -> String {
    if !bindings.any() {
        return String::new();
    }
    let real = module_path(name);
    let dir = super::cjs::module_dir(name);
    let real_lit = super::cjs::js_string_literal(real);
    let dir_lit = super::cjs::js_string_literal(&dir);
    let mut prelude = String::new();
    // `exports` is initialised from `module.exports`, so the record is needed whenever either
    // name is referenced and the module does not already declare `module` itself.
    let needs_module_record = bindings.module || (bindings.exports && !bindings.module_declared);
    if needs_module_record {
        // jiti's CJS `module` record, minus `require` (which is only injected when referenced):
        // building it must not call into the CJS loader.
        let _ = writeln!(
            prelude,
            "var module = {{ id: {real_lit}, path: {dir_lit}, filename: {real_lit}, \
             exports: {{}}, loaded: false, children: [], parent: undefined }};"
        );
    }
    if bindings.exports {
        let _ = writeln!(prelude, "var exports = module.exports;");
    }
    if bindings.require {
        let _ = writeln!(
            prelude,
            "var require = globalThis.__kiana_make_require({real_lit}, {dir_lit});"
        );
    }
    if bindings.filename {
        let _ = writeln!(prelude, "var __filename = {real_lit};");
    }
    if bindings.dirname {
        let _ = writeln!(prelude, "var __dirname = {dir_lit};");
    }
    prelude
}

/// Helper import emitted by oxc for legacy decorators (`HelperLoaderMode::Runtime`).
const OXC_RUNTIME_HELPER_PREFIX: &str = "@oxc-project/runtime/helpers/";

/// Replace `import <local> from "@oxc-project/runtime/helpers/<name>";` with an inline
/// definition of the helper (jiti/Babel inline these too).
///
/// Unknown helpers are left untouched: the loader then reports the missing module exactly like
/// before. See the module docs for provenance (MIT, `@oxc-project/runtime@0.155.0`).
///
/// 匹配是逐行文本级的（不重新解析 AST），只识别 oxc codegen 生成的那一种单行 import 形态；
/// 内联定义集中放在输出最前面（preamble），其余行保持原顺序。注意 preamble 无条件置于
/// 最前，不会绕到 shebang 之后：若同一文件既有 shebang 又有 helper import，shebang 将不再
/// 位于首行（现有单测只分别覆盖这两种情况，没有组合用例）。
fn inline_oxc_runtime_helpers(code: String) -> String {
    let mut preamble = String::new();
    let mut rest = String::with_capacity(code.len());
    for line in code.split_inclusive('\n') {
        match parse_runtime_helper_import(line) {
            Some((local, helper)) => match runtime_helper_source(helper) {
                Some(body) => {
                    let _ = writeln!(preamble, "const {local} = {body};");
                }
                // Unknown helper: keep the import so the loader reports the missing module.
                None => rest.push_str(line),
            },
            None => rest.push_str(line),
        }
    }
    if preamble.is_empty() {
        rest
    } else {
        preamble.push_str(&rest);
        preamble
    }
}

/// Parse one `import <local> from "@oxc-project/runtime/helpers/<name>";` line.
///
/// 只接受双引号、行尾分号、`from` 两侧以单空格分隔的单行形态（即 oxc codegen 的输出）；
/// `import` 与本地名两侧的空白会被 `trim` 容忍。多行、单引号或无分号的写法返回 `None`，
/// 原行保留给加载器报错。
fn parse_runtime_helper_import(line: &str) -> Option<(&str, &str)> {
    let trimmed = line.trim();
    let rest = trimmed.strip_prefix("import ")?;
    let (local, from) = rest.split_once(" from ")?;
    let specifier = from.strip_suffix(';')?.trim();
    let helper = specifier
        .strip_prefix('"')
        .and_then(|value| value.strip_suffix('"'))
        .and_then(|value| value.strip_prefix(OXC_RUNTIME_HELPER_PREFIX))?;
    Some((local.trim(), helper))
}

/// Inline source for the helpers oxc's TypeScript/decorator transforms can emit.
///
/// 只覆盖装饰器变换会生成的三种；新增 helper 必须在这里登记，否则 import 会原样保留，并在
/// 加载期报告找不到 `@oxc-project/runtime`（本 crate 不提供该包）。
fn runtime_helper_source(helper: &str) -> Option<&'static str> {
    match helper {
        "decorate" => Some(OXC_DECORATE_HELPER),
        "decorateParam" => Some(OXC_DECORATE_PARAM_HELPER),
        "decorateMetadata" => Some(OXC_DECORATE_METADATA_HELPER),
        _ => None,
    }
}

/// `@oxc-project/runtime@0.155.0` `helpers/decorate.js` (MIT), function body verbatim.
const OXC_DECORATE_HELPER: &str = "function __decorate(decorators, target, key, desc) {
	var c = arguments.length,
		r = c < 3 ? target : desc === null ? (desc = Object.getOwnPropertyDescriptor(target, key)) : desc,
		d;
	if (typeof Reflect === \"object\" && typeof Reflect.decorate === \"function\")
		r = Reflect.decorate(decorators, target, key, desc);
	else
		for (var i = decorators.length - 1; i >= 0; i--)
			if ((d = decorators[i]))
				r = (c < 3 ? d(r) : c > 3 ? d(target, key, r) : d(target, key)) || r;
	return c > 3 && r && Object.defineProperty(target, key, r), r;
}";

/// `@oxc-project/runtime@0.155.0` `helpers/decorateParam.js` (MIT), function body verbatim.
const OXC_DECORATE_PARAM_HELPER: &str = "function __decorateParam(paramIndex, decorator) {
	return function (target, key) {
		decorator(target, key, paramIndex);
	};
}";

/// `@oxc-project/runtime@0.155.0` `helpers/decorateMetadata.js` (MIT), function body verbatim.
const OXC_DECORATE_METADATA_HELPER: &str = "function __decorateMetadata(k, v) {
	if (typeof Reflect === \"object\" && typeof Reflect.metadata === \"function\") return Reflect.metadata(k, v);
}";

/// Strip a `?v=<generation>-<seq>` suffix used as the module cache buster (design §3.4).
///
/// Windows paths never contain `?`, so splitting on the first `?` is safe there too; the file
/// system paths themselves keep their native separators (only file URLs use forward slashes).
///
/// 在**第一个** `?` 处截断，不校验后缀确实是 `?v=`。Unix 文件名允许含 `?`，这类路径在这里
/// 会被截断；用户可见文案的调用方（`loader_impl` 的 `display_name`）先用 `version_query`
/// 判断，避免把文件名里的 `?` 当成缓存键。`data:` URL 的 payload 也可能含 `?`，调用方需自行
/// 避免对它使用本函数。
pub fn module_path(name: &str) -> &str {
    match name.split_once('?') {
        Some((path, _)) => path,
        None => name,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_bom_once() {
        assert_eq!(strip_bom("\u{feff}const a = 1;"), "const a = 1;");
        // Only a leading BOM; inner BOMs are legal identifier characters and must survive.
        assert_eq!(strip_bom("const \u{feff} = 1;"), "const \u{feff} = 1;");
    }

    #[test]
    fn detects_typescript_names() {
        assert!(is_typescript("a.ts"));
        assert!(is_typescript("a.tsx"));
        assert!(is_typescript("a.mts"));
        assert!(is_typescript("a.cts"));
        // jiti's extension list includes the TSX variants (review finding 7).
        assert!(is_typescript("a.mtsx"));
        assert!(is_typescript("a.ctsx"));
        assert!(is_typescript("/tmp/x/a.mtsx?v=3-7"));
        assert!(!is_typescript("a.js"));
        assert!(!is_typescript("a.mjs"));
        assert!(!is_typescript("a.cjs"));
    }

    #[test]
    fn module_path_strips_query() {
        assert_eq!(module_path("/a/b.ts?v=1-2"), "/a/b.ts");
        assert_eq!(module_path("/a/b.ts"), "/a/b.ts");
    }

    #[test]
    fn parse_error_keeps_diagnostics() {
        let err = strip_types("broken.ts", "export default function( {").unwrap_err();
        assert_eq!(err.path(), "broken.ts");
        assert!(!err.diagnostics().is_empty());
        assert!(err.to_string().starts_with("Failed to parse broken.ts: "));
    }

    fn is_esm_source(source: &str) -> bool {
        let alloc = Allocator::default();
        let ret = Parser::new(&alloc, source, SourceType::ts())
            .with_options(ParseOptions::default())
            .parse();
        assert!(ret.diagnostics.is_empty(), "{source}");
        program_has_esm_syntax(&ret.program)
    }

    #[test]
    fn declaration_exports_are_esm_syntax() {
        // Review finding 1: oxc 0.155 puts all `export <declaration>` forms in
        // `Statement::ExportDeclaration` and re-export-from in `Statement::ExportFromDeclaration`.
        for source in [
            "export const a = 1;",
            "export let a = 1;",
            "export var a = 1;",
            "export function f() {}",
            "export async function f() {}",
            "export class C {}",
            "export abstract class C {}",
            "export enum E { A }",
            "export const enum E { A }",
            "export interface I { x: number }",
            "export type T = 1;",
            "export namespace N { export const x = 1; }",
            "export declare const x: number;",
            "export { a };",
            "export { a } from \"./x\";",
            "export type { T } from \"./x\";",
            "export * from \"./x\";",
            "export default 1;",
            "export {};",
            "import \"./side-effect\";",
            "import type { A } from \"./x\";",
        ] {
            assert!(is_esm_source(source), "`{source}` must be ESM syntax");
        }
        // TypeScript's CJS forms are not ESM.
        for source in [
            "export = 1;",
            "import x = require(\"./x\");",
            "module.exports = 1;",
            "const x: number = 1;",
            "exports.x = 1;",
        ] {
            assert!(!is_esm_source(source), "`{source}` must not be ESM syntax");
        }
    }

    #[test]
    fn injects_module_and_exports_for_esm_output() {
        let code = strip_types("/ext/a.ts", "exports.value = 42;\nexport default 1;").unwrap();
        assert!(
            code.starts_with(
                "var module = { id: \"/ext/a.ts\", path: \"/ext\", filename: \"/ext/a.ts\", \
                 exports: {}, loaded: false, children: [], parent: undefined };\n\
                 var exports = module.exports;\n"
            ),
            "module/exports prelude missing: {code:?}"
        );
        assert!(code.contains("exports.value = 42;"));
        // `require` is not referenced, so it is not injected.
        assert!(!code.contains("__kiana_make_require"));
    }

    #[test]
    fn injects_each_esm_binding_independently() {
        let code = strip_types(
            "/ext/a.ts?v=1-1",
            "const f = __filename;\nconst d = __dirname;\nconst r = require(\"ms\");\nexport { f, d, r };",
        )
        .unwrap();
        assert!(code.starts_with(
            "var require = globalThis.__kiana_make_require(\"/ext/a.ts\", \"/ext\");\n\
             var __filename = \"/ext/a.ts\";\n\
             var __dirname = \"/ext\";\n"
        ));
        assert!(!code.contains("var module"));
        assert!(!code.contains("var exports"));
    }

    #[test]
    fn ambient_declarations_do_not_suppress_binding_injection() {
        // Review finding 9: `declare` produces no runtime binding, so the emitted code still
        // needs jiti's wrapper parameters.
        let code = strip_types(
            "/ext/a.ts",
            "declare const __dirname: string;\n\
             declare function require(s: string): any;\n\
             export default function f(): string { return __dirname + typeof require; }",
        )
        .unwrap();
        assert!(code.starts_with(
            "var require = globalThis.__kiana_make_require(\"/ext/a.ts\", \"/ext\");\n"
        ));
        assert!(code.contains("var __dirname = \"/ext\";"));
        assert!(!code.contains("declare"));
    }

    #[test]
    fn keeps_a_hashbang_on_the_first_line() {
        let code = strip_types(
            "/ext/a.ts",
            "#!/usr/bin/env node\nexport default function f(): string { return __filename; }",
        )
        .unwrap();
        assert!(code.starts_with("#!/usr/bin/env node\nvar __filename = \"/ext/a.ts\";\n"));
    }

    #[test]
    fn cjs_shaped_ts_output_gets_no_prelude() {
        // The true-CJS wrapper supplies all five bindings as `new Function` parameters; injecting
        // a synthetic record here would shadow the real module record.
        let code = strip_types(
            "/ext/a.ts",
            "const x: number = require(\"y\");\nmodule.exports = x;",
        )
        .unwrap();
        assert_eq!(code, "const x = require(\"y\");\nmodule.exports = x;\n");
    }
}
