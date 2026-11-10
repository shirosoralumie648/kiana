//! utils/syntax-highlight.ts（highlight.js 10.7.3）。
//!
//! TS: `packages/coding-agent/src/utils/syntax-highlight.ts`（219 行）+ `utils/highlight-js.d.ts`。
//!
//! ```ts
//! export type HighlightFormatter = (text: string) => string;
//! export type HighlightTheme = Partial<Record<string, HighlightFormatter>>;
//! export interface HighlightOptions { language?; ignoreIllegals?; languageSubset?; theme? }
//! export function renderHighlightedHtml(html: string, theme?: HighlightTheme): string
//! export function highlight(code: string, options?: HighlightOptions): string
//! export function supportsLanguage(name: string): boolean
//! export function loadAllHighlightLanguages(): Promise<void>
//! ```
//!
//! ## 结构
//!
//! - `render_highlighted_html`：highlight.js HTML → ANSI 的逐字复刻（span 栈、scope→formatter
//!   解析、实体解码、`\n` 分段）。
//! - `highlight`：`language` → 该 grammar；否则 `highlightAuto(languageSubset)`；最后 render。
//! - `supports_language` / `load_all_highlight_languages`：与 `hljs.getLanguage` / 动态
//!   `import("highlight.js/lib/index.js")` 对应的两阶段注册表。
//! - 扫描器的每条规则都以 `\A(?:...)` 锚定在当前位置（见 `AnchoredRegex`）：非命中
//!   位置的代价是 O(1)，整体保持线性，与 highlight.js 的合并正则一致；绝不用
//!   `Regex::find_at` 从当前位置向后搜索（那是 O(n²)）。
//!
//! ## grammar 覆盖范围（spec/ca-cli.md §8.2 第 2 条）
//!
//! highlight.js 的 grammar 无法用 `syntect` 复刻（scope 名不同），本模块自实现一个
//! 兼容子集：21 种急切语言 + `html`/`xml` + `diff` + `plaintext` 由手写扫描器实现，
//! scope 名（`keyword`/`built_in`/`literal`/`type`/`string`/`subst`/`comment`/`number`/
//! `regexp`/`title`/`params`/`function`/`class`/`meta`/`attr`/`variable`/`symbol`/`tag`/
//! `name`/`addition`/`deletion`）与 highlight.js 一致；其余 167 种语言（目录里共 364 个
//! 名字，含别名）只进入静态目录，输出为 plaintext（不产生 span）。逐字保证的观察点见
//! `tests/syntax_highlight.rs`。
//!
//! ## 已知偏差（诚实记录）
//!
//! - `hljs.highlight` 对未注册语言抛 `Error("Unknown language: ...")`；Rust 的
//!   `highlight` 返回 `String`（§4.9 冻结签名），因此未注册语言回退为 plaintext。
//!   同理，`html`/`xml`/`diff`/`plaintext` 在 `load_all_highlight_languages()` 之前
//!   直接调用 `highlight` 仍会使用已实现的 grammar（TS 会抛错）；但 `supports_language`
//!   的可见性规则保持：loadAll 之前只认 21 种急切语言及其别名，因此
//!   `kiana-theme::highlightCode`（先查 `supports_language`）的启动期回退行为不变。
//! - 目录中未实现 grammar 的 167 种语言（`yaml`/`sql`/`toml`/`markdown`/`css`/
//!   `dockerfile`/`powershell`/...）：`load_all_highlight_languages()` 之后
//!   `supports_language` 为 `true`（与 TS 的全量注册一致），但 `highlight` 输出
//!   escaped plaintext、没有 span。这是 spec/ca-cli.md §8.2 第 2 条承认的最难项。
//! - `highlightAuto` 的 relevance 采用简化计分（span/关键字计数）；候选集只含已实现
//!   grammar，语言选择可能与 hljs 不同，未与完整 relevance 算法逐点对拍
//!   （`kiana-theme` 始终显式传语言，不走自动检测）。
//! - `ignoreIllegals` 按 highlight.js 的两种路径实现：`true`（`kiana-theme` 的固定用法）时
//!   字符串只由 end 词素结束，可以跨行；`false` 时字符串模式声明的 `illegal: /\n/`
//!   与 language 级别 `illegal`（python/java/go/c/cpp/rust/js/ts/csharp/ruby/groovy）
//!   会让 `highlight` 退化为**整段 escaped plaintext、无 span**（hljs 捕获 Illegal 后
//!   返回 `escape(code)`）。未建模的 illegal：json 顶层的 `/\S/`（它依赖 mode 结构，
//!   手写扫描器没有 OBJECT/ARRAY 模式）以及各语言嵌套模式（params、class 等）里的
//!   illegal；这些只影响 `ignoreIllegals: false` 的调用方。
//! - grammar 子集的 scope 细节仍与 hljs 有差距（文本一致、着色不同）：
//!   js/ts 的箭头函数（`(params) =>` / `IDENT =>`）、对象字面量键（`attr`）、模板串
//!   `${...}` 内部 token 与 python 装饰器参数（`meta` 内的 NUMBER/PARAMS/STRING）已按
//!   `FUNCTION`/object-attr/`SUBST`/`META` 模式对齐；仍有的近似：表达式里再嵌模板串时
//!   按第一个 `}` 截断（`match_subst` 不做嵌套解析）、装饰器参数里的 `#` 注释不单独成
//!   span；
//!   swift 的 `init`/`subscript`/运算符函数声明（hljs 的 `INIT_SUBSCRIPT`/
//!   `OPERATOR_DECLARATION` 模式）未单独建模，参数列表里的运算符（hljs 的
//!   `OPERATOR`）也没有 `operator` scope；`func` 的标题/泛型/参数列表已按
//!   `FUNCTION` 模式对齐（参数名 `params`、`_` 为 `keyword`、参数内类型 `type`）；
//!   go 的 `func` 接收者/名字/参数/返回值与 scala `def` 的尾部空白也已按 10.7.3
//!   的 `FUNCTION`/`METHOD` 模式对齐；
//!   其余语言的 CLASS/NAMESPACE 模式（`class`/`struct`/`namespace`/`enum`/`trait`、
//!   泛型、`extends`/`implements`、scala 的 `params`、kotlin 的 `type` 泛型参数、
//!   rust 的 `endsParent` 标题）已按 10.7.3 的 grammar 逐条对齐，见 `ClassBodyDef`；
//!   c 与 cpp 共用同一段 CLASS 规则（`enum class struct union`，`struct` 只经由该模式
//!   成为关键字），java 的 `record` 模式（class + title + params）与 csharp 的 `record`
//!   模式（无 className，头部标识符都是 title，TITLE_MODE 支持 `A.B` 限定名）也已实现。
//!   仍有的近似：typescript 顶层的 `@decorator`（hljs 的 `DECORATOR`，`meta`）未建模；
//!   cpp 的函数调用 `built_in`（hljs 的 `FUNCTION_DISPATCH`，classNameAliases 到
//!   `built_in`）未建模；ruby 的 `|...|` 块参数（`params`）与 `def self.x`/运算符方法
//!   标题（`RUBY_METHOD_RE` 的运算符分支）未建模。
//! - 已按 hljs 对齐的其余细节：php 双引号串 / csharp `$"..."` / scala `s"..."` /
//!   dart 三种引号串的 `subst`；php/typescript 的返回类型（`function f(): void`）；
//!   java/c/cpp 的 function span 延伸到 `{`/`;`/`=`（含 `throws`）；csharp 的 span 在
//!   `)` 结束；perl `sub` 的尾部空白；scala/swift 顶层大写标识符的 `type`。
//!   `tests/syntax_highlight_reference.rs` 用 TS 参考实现生成的 fixture 逐字节对拍
//!   20 种语言。
//! - gate 评审修复（均已按 10.7.3 对齐，见 `gate_regressions_match_the_typescript_reference`
//!   的 TS fixture）：js/ts 的 `function` 模式（泛型/返回类型里的标识符是 `title`，
//!   空参数列表产生零长度 `params` span）、method/constructor/getter-setter 模式、
//!   python `class P:` 的冒号属于 class span、kotlin `fun` 的泛型与参数类型上下文、
//!   kotlin class 的 supertype `type`、ruby kwarg symbol（`a:`）与 `def f?`、
//!   perl regexp（`/.../`、`m//`、`s///`）、groovy 的 attr 与三元模式、dart/kotlin
//!   `${...}` 内层 NUMBER/STRING、go 的 `C_NUMBER_RE`（下划线拆分）、c/cpp 的 `_t`
//!   类型与 function 推断顺序、csharp 的泛型返回类型与方法泛型标题。
//! - 带声明关键字的语言（`def`/`function`/`fn`/`func`/`fun`/`sub`，以及 bash 的
//!   `name() {`）与 C/Java/C++/C# 的 `(类型\s+)+名字(` 推断（hljs 的 `function` mode）
//!   都已实现；类型参数按各 grammar 的 `TYPE_IDENT_RE`/`GENERIC_IDENT_RE`/
//!   `TEMPLATE_ARGUMENT_RE` 处理（不支持嵌套尖括号，cpp 还接受 `std::vector` 这类
//!   `NAMESPACE_RE` 限定名）；跨多行或函数指针的声明不推断（按普通文本输出）。
//! - `html` 的 `<script>`/`<style>` 子语言嵌入（css/javascript）未实现，内容按普通
//!   文本输出。
//! - 面向 `kiana-theme`（阶段 3）观察点逐字实现：python docstring 每行 `string`、
//!   `@decorator` 的 `meta`（含缩进）、javascript `/re/g` 的 `regexp`、html 标签名的
//!   `name`、diff 的 `addition`/`deletion`、typescript `const`/`1` 的 `keyword`/`number`。
//!
//! ## 调用方
//!
//! - `kiana-theme::highlight_code`（`highlightCode` 的移植）与
//!   `kiana-extensions` 的 `highlight_code_text`：先用 `supports_language` 过滤语言，
//!   再以 `ignore_illegals: true` 和显式语言调用 `highlight`，最后按 `\n` 拆成逐行
//!   字符串。两者都不走自动检测。
//! - `kiana-interactive` 在启动界面刷出后 `spawn_local` 一个任务 await
//!   `load_all_highlight_languages()`，使其余目录语言名对 `supports_language` 可见。
//! - 测试：`tests/syntax_highlight.rs` 锁定行为用例；
//!   `tests/syntax_highlight_reference.rs` 用 TS 参考实现（highlight.js 10.7.3）
//!   生成的 fixture 做逐字节对拍。
//!
//! ## 执行流程
//!
//! `highlight` 分两步：
//!
//! 1. 生成 highlight.js 风格的 HTML：`language` 非空时按名字/别名查注册表，再按
//!    grammar 形态（`Kind`）分派到 `scan_generic` 或 json/xml/diff 专用扫描器；
//!    `plaintext`（含 `text`/`txt` 别名）不经过任何扫描器，直接整段转义为无 span 的
//!    HTML。`language` 为空（`None` 或空串）时 `highlight_auto` 对候选语言逐个扫描
//!    并按 relevance 取最高分。显式语言分支里，illegal 命中或语言未知都会让整段
//!    退化为 `escape_html(code)` 的无 span 文本（渲染时只可能命中 `default`
//!    formatter）。
//! 2. `render_highlighted_html` 把 HTML 里的 `<span class="hljs-*">` 栈翻译成主题
//!    formatter 的输出。返回值是**文本**（是否含 ANSI 由 formatter 决定），不是 HTML。
//!
//! ## 语言定义与扫描器
//!
//! 每种语言由 `LangDef` 描述（关键字表、注释、字符串、声明关键字、CLASS 模式、
//! language 级 illegal 等）；首次查询时 `compile_language` 把其中的正则源码编译成
//! `AnchoredRegex` 并放入全局 `Registry`。扫描不是 highlight.js 的 mode 树，而是
//! `scan_generic` 里一组按优先级排列的规则分支；`decls`/`class_bodies`/
//! `infer_functions` 等字段决定是否启用额外的子扫描器（声明、类头、C 系函数推断）。
//!
//! ## 全局状态与并发
//!
//! - `Registry` 由一个 `OnceLock` 持有：全部 grammar 的正则在第一次调用时编译并
//!   常驻，初始化后只读。`highlight` 本身只使用局部变量，没有共享可变状态。
//! - `ALL_LOADED` 是进程级 `AtomicBool`（`SeqCst`），只由
//!   `load_all_highlight_languages` 写入，也只在 `supports_language` 里读取。
//! - `HighlightTheme` 的 formatter 是 `Arc<dyn Fn + Send + Sync>`，主题表可以跨线程
//!   共享；扫描与渲染过程不加锁。
//!
//! ## 阅读顺序
//!
//! 先读 `render_highlighted_html` 及其辅助函数（HTML → 文本），再看注册表与语言目录，
//! 然后是声明式的 `LangDef`/`StrDef`/`ClassBodyDef` 数据表，接着是 `AnchoredRegex`
//! 与 `scan_generic` 主循环，之后是各语言的声明/类头扫描器与 json/xml/diff 专用
//! 扫描器，最后是 `highlight`/`highlight_auto`。文件末尾的关键字表是纯数据，不需要
//! 逐条阅读。

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};

use regex::Regex;

use crate::html::{DecodedHtmlEntity, decode_html_entity_at};

/// TS: `type HighlightFormatter = (text: string) => string`。
///
/// 主题里的单个 scope 格式化器：输入是**一行**已解码的纯文本（renderer 按 `\n` 切分
/// 文本缓冲，空行不调用 formatter），返回该行的最终文本（通常是带 ANSI 转义的字符串）。
/// formatter 由调用方提供，本模块不解释返回值，只把它拼回输出。
///
/// 类型要求 `Send + Sync`，因此主题表可以跨线程共享；`kiana-theme` 把整张表缓存在
/// `Mutex` 里，`Arc` 则让每次查表都只克隆一个引用计数。
pub type HighlightFormatter = Arc<dyn Fn(&str) -> String + Send + Sync>;

/// TS: `type HighlightTheme = Partial<Record<string, HighlightFormatter>>`。
///
/// 键是 highlight.js 的 scope 名（去掉 `hljs-` 前缀，如 `keyword`、`string`、
/// `meta-keyword`）。查找顺序由 `get_scope_formatter` 决定：精确匹配 → 第一个 `.`
/// 之前的前缀（`string.regexp` 回退到 `string`）→ 第一个 `-` 之前的前缀
/// （`meta-keyword` 回退到 `meta`）；整条 span 栈都没有命中时使用 `default` 键。
/// 栈中没有可用 formatter 且没有 `default` 时文本原样输出（不报错）。
pub type HighlightTheme = HashMap<String, HighlightFormatter>;

/// TS: `interface HighlightOptions`。
///
/// 一次 `highlight` 调用的参数集合。`Default` 表示全部缺省：无语言（走自动
/// 检测）、`ignore_illegals = false`、无候选子集、空主题（渲染阶段不着色）。
#[derive(Clone, Default)]
pub struct HighlightOptions {
    /// 显式语言名，大小写不敏感，可写语言名或别名（如 `js`/`javascript`）。
    /// `None` 或空串时走 `highlight_auto` 自动检测。
    pub language: Option<String>,
    /// 对应 highlight.js 的 `ignoreIllegals`。为 `false` 时，声明了 `illegal: /\n/`
    /// 的字符串遇到换行、或 language 级 illegal 命中，都会让整段结果退化为 escaped
    /// plaintext（没有 span；渲染阶段只有 `default` formatter 可能作用于它）；为
    /// `true` 时换行不会终止字符串。该字段只在显式 `language` 路径生效：`language`
    /// 为 `None` 或空串时走 `highlight_auto`，后者对每个候选固定以
    /// `ignore_illegals = false` 扫描（见 `highlight_auto`），这里的取值被忽略。
    /// `kiana-theme` 与 `kiana-extensions` 都固定传 `true`。未被建模的 illegal 见
    /// 文件级“已知偏差”。
    pub ignore_illegals: bool,
    /// 自动检测的候选语言子集（TS 的 `languageSubset`）；`None` 表示全部已实现
    /// grammar。名字先小写再查注册表，未知名字被忽略；无论是否给子集，`plaintext`
    /// 都因 `disable_autodetect` 不参与自动检测。显式指定 `language` 时该字段不生效。
    pub language_subset: Option<Vec<String>>,
    /// scope 名 → formatter。空表表示渲染阶段原样输出（renderer 仍会解码实体、
    /// 丢弃 span）。查找与回退规则见 `HighlightTheme`。
    pub theme: HighlightTheme,
}

const SPAN_CLOSE: &str = "</span>";
const HIGHLIGHT_CLASS_PREFIX: &str = "hljs-";

/// TS 模块里急切注册的 21 种语言（`eagerLanguages`，顺序逐字）。
///
/// 注册表初始化时就编译这些语言的 grammar；在 `load_all_highlight_languages()` 之前，
/// `supports_language` 只认它们及其别名。`tests/syntax_highlight.rs` 断言数量与首尾项。
pub const EAGER_LANGUAGES: &[&str] = &[
    "python",
    "java",
    "go",
    "javascript",
    "json",
    "cpp",
    "typescript",
    "php",
    "ruby",
    "c",
    "csharp",
    "nix",
    "bash",
    "rust",
    "scala",
    "kotlin",
    "swift",
    "dart",
    "groovy",
    "perl",
    "lua",
];

/// TS: `eagerLanguages` 的 Rust 访问入口（21 种，顺序固定）。
pub fn eager_languages() -> &'static [&'static str] {
    EAGER_LANGUAGES
}

// ---------------------------------------------------------------------------
// renderHighlightedHtml：highlight.js 的 HTML → 文本渲染器（逐字复刻）
// ---------------------------------------------------------------------------

/// JS `\s` 的精确字符集：WhiteSpace + LineTerminator。
///
/// 与 Rust regex 的 Unicode `\s`（White_Space）不同：JS 包含 U+FEFF（BOM）、
/// 不包含 U+0085（NEL）。renderer 的 class 正则必须逐字对齐 TS。
const JS_WS: &str = "[\t\n\u{0B}\u{0C}\r \u{00A0}\u{1680}\u{2000}-\u{200A}\u{2028}\u{2029}\u{202F}\u{205F}\u{3000}\u{FEFF}]";

/// TS: `/\sclass\s*=\s*(?:"([^"]*)"|'([^']*)')/`（JS 的 `\s`）。
fn class_attr_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(&format!(
            r#"{JS_WS}class{JS_WS}*={JS_WS}*(?:"([^"]*)"|'([^']*)')"#
        ))
        .expect("class attribute regex")
    })
}

/// TS: `classValue.split(/\s+/)`（JS 的 `\s`，含 BOM、不含 NEL）。
fn class_split_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(&format!("{JS_WS}+")).expect("class split regex"))
}

/// TS: `getScopeFromSpanTag(tag)` —— 取第一个 `hljs-` 前缀类的后缀。
fn get_scope_from_span_tag(tag: &str) -> Option<String> {
    let caps = class_attr_regex().captures(tag)?;
    // TS: `match?.[1] ?? match?.[2]` —— 第一组参与匹配时即使为空串也采用。
    let class_value = match (caps.get(1), caps.get(2)) {
        (Some(first), _) => first.as_str(),
        (None, Some(second)) => second.as_str(),
        (None, None) => return None,
    };
    if class_value.is_empty() {
        return None;
    }
    for class_name in class_split_regex().split(class_value) {
        if let Some(scope) = class_name.strip_prefix(HIGHLIGHT_CLASS_PREFIX) {
            return Some(scope.to_string());
        }
    }
    None
}

/// TS: `getScopeFormatter(scope, theme)` —— 精确 → `.` 前缀 → `-` 前缀。
fn get_scope_formatter(scope: &str, theme: &HighlightTheme) -> Option<HighlightFormatter> {
    if let Some(formatter) = theme.get(scope) {
        return Some(formatter.clone());
    }
    if let Some(dot_index) = scope.find('.')
        && let Some(formatter) = theme.get(&scope[..dot_index])
    {
        return Some(formatter.clone());
    }
    if let Some(dash_index) = scope.find('-')
        && let Some(formatter) = theme.get(&scope[..dash_index])
    {
        return Some(formatter.clone());
    }
    None
}

/// TS: `getActiveFormatter(scopes, theme)` —— 栈顶向下第一个命中，否则 `theme.default`。
fn get_active_formatter(
    scopes: &[Option<String>],
    theme: &HighlightTheme,
) -> Option<HighlightFormatter> {
    for scope in scopes.iter().rev() {
        let Some(scope) = scope.as_deref() else {
            continue;
        };
        if scope.is_empty() {
            continue;
        }
        if let Some(formatter) = get_scope_formatter(scope, theme) {
            return Some(formatter);
        }
    }
    theme.get("default").cloned()
}

/// TS: `isSpanOpenTagStart(html, index)`。
fn is_span_open_tag_start(html: &str, index: usize) -> bool {
    let rest = &html[index..];
    if !rest.starts_with("<span") {
        return false;
    }
    matches!(
        rest.as_bytes().get("<span".len()),
        Some(b'>') | Some(b' ') | Some(b'\t') | Some(b'\n') | Some(b'\r')
    )
}

/// TS: `flushText` —— 按 `\n` 分段，非空行才调用 formatter（空行原样）。
fn flush_text(
    output: &mut String,
    text_buffer: &mut String,
    scopes: &[Option<String>],
    theme: &HighlightTheme,
) {
    if text_buffer.is_empty() {
        return;
    }
    match get_active_formatter(scopes, theme) {
        Some(formatter) => {
            let mut first = true;
            for line in text_buffer.split('\n') {
                if !first {
                    output.push('\n');
                }
                first = false;
                if !line.is_empty() {
                    output.push_str(&formatter(line));
                }
            }
        }
        None => output.push_str(text_buffer),
    }
    text_buffer.clear();
}

/// TS: `decodeHtmlEntityAt(html, index)` 的线性等价实现。
///
/// `decode_html_entity_at` 只允许分号出现在 `&` 之后 16 个 UTF-16 code unit 内
/// （`semicolon - index > 16` 即失败），所以从 `&` 起截一个 17 code unit 的窗口再调用
/// 它与传入整个 HTML 完全等价；而它的实现每次都会把输入重新编码成 UTF-16，若直接把
/// 整串传进去，每个 `&` 都是 O(n)，大段 HTML 会退化为 O(n²)。TS 的
/// `indexOf(";", index + 1)` 同样只扫描这段窗口。窗口边界都落在字符边界上。
fn decode_entity_window(html: &str, byte_index: usize) -> Option<DecodedHtmlEntity> {
    let mut units = 0usize;
    let mut end = byte_index;
    for ch in html[byte_index..].chars() {
        if units >= 17 {
            break;
        }
        units += ch.len_utf16();
        end += ch.len_utf8();
    }
    decode_html_entity_at(&html[byte_index..end], 0)
}

/// TS: `renderHighlightedHtml(html, theme = {})`。
///
/// 索引语义：TS 的 `html[index]` / `html.slice` 按 **UTF-16 code unit** 计。这里按字节
/// 扫描（所有模式都是 ASCII）；实体解码通过 `decode_entity_window` 保持 TS 的
/// "最多 16 code unit" 语义。逐字符追加与 TS 逐个 code unit 追加的最终文本一致。
///
/// # 输入契约与失败模式
///
/// 期望输入是 highlight.js 生成的标记：scope span 形如
/// `<span class="hljs-<scope>">...</span>`。函数不做 HTML 结构校验：不带 class 的
/// `<span>` 会压入一个 `None` scope（该层文本继续使用栈中其余 scope 或 `default`），
/// 多余的 `</span>` 只是弹栈（空栈时无操作），未闭合标签不会报错。所有实体按
/// `crate::html::decode_html_entity_at` 的规则解码，无法解码的 `&` 原样保留。
/// 返回值中的 span 已全部消失；样式完全由 `theme` 决定，空主题时等价于“去标签 +
/// 解码实体”的纯文本。
pub fn render_highlighted_html(html: &str, theme: &HighlightTheme) -> String {
    let mut output = String::new();
    let mut text_buffer = String::new();
    let mut scopes: Vec<Option<String>> = Vec::new();
    let mut index = 0usize;

    while index < html.len() {
        if is_span_open_tag_start(html, index) {
            // TS: `html.indexOf(">", index + 5)`。
            if let Some(offset) = html[index + "<span".len()..].find('>') {
                let tag_end_index = index + "<span".len() + offset;
                flush_text(&mut output, &mut text_buffer, &scopes, theme);
                let tag = &html[index..=tag_end_index];
                let scope = get_scope_from_span_tag(tag);
                scopes.push(scope);
                index = tag_end_index + 1;
                continue;
            }
        }

        if html[index..].starts_with(SPAN_CLOSE) {
            flush_text(&mut output, &mut text_buffer, &scopes, theme);
            scopes.pop();
            index += SPAN_CLOSE.len();
            continue;
        }

        if html.as_bytes()[index] == b'&'
            && let Some(decoded) = decode_entity_window(html, index)
        {
            text_buffer.push_str(&decoded.text);
            // `decoded.length` 按 UTF-16 code unit 计；实体名虽是 ASCII，但
            // `Number.parseInt` 会跳过前导空白，实体内部可能含非 ASCII 空白
            // （如 `&#\u{3000}65;`），因此必须换算出字节下标。
            let mut consumed_bytes = 0usize;
            let mut consumed_units = 0usize;
            for ch in html[index..].chars() {
                if consumed_units >= decoded.length {
                    break;
                }
                consumed_units += ch.len_utf16();
                consumed_bytes += ch.len_utf8();
            }
            index += consumed_bytes;
            continue;
        }

        let ch = html[index..]
            .chars()
            .next()
            .expect("index is on a char boundary");
        text_buffer.push(ch);
        index += ch.len_utf8();
    }

    flush_text(&mut output, &mut text_buffer, &scopes, theme);
    output
}

// ---------------------------------------------------------------------------
// 语言目录（highlight.js 10.7.3）与两阶段注册表
// ---------------------------------------------------------------------------

/// highlight.js 10.7.3 的全部语言名 + 别名（`hljs.listLanguages()` 的 191 个名字
/// 加上各语言的 `aliases`，共 364 个）。用于 `load_all_highlight_languages()` 之后的
/// `supports_language`。
const ALL_LANGUAGE_NAMES: &[&str] = &[
    "1c",
    "abnf",
    "accesslog",
    "actionscript",
    "ada",
    "ado",
    "adoc",
    "ahk",
    "angelscript",
    "apache",
    "apacheconf",
    "applescript",
    "arcade",
    "arduino",
    "arm",
    "armasm",
    "as",
    "asc",
    "asciidoc",
    "aspectj",
    "atom",
    "autohotkey",
    "autoit",
    "avrasm",
    "awk",
    "axapta",
    "bash",
    "basic",
    "bat",
    "bf",
    "bind",
    "bnf",
    "brainfuck",
    "c",
    "c#",
    "c++",
    "c-like",
    "cal",
    "capnp",
    "capnproto",
    "cc",
    "ceylon",
    "cjs",
    "clean",
    "clj",
    "clojure",
    "clojure-repl",
    "cls",
    "cmake",
    "cmake.in",
    "cmd",
    "coffee",
    "coffeescript",
    "console",
    "coq",
    "cos",
    "cpp",
    "cr",
    "craftcms",
    "crm",
    "crmsh",
    "crystal",
    "cs",
    "csharp",
    "cson",
    "csp",
    "css",
    "cxx",
    "d",
    "dart",
    "dcl",
    "delphi",
    "dfm",
    "diff",
    "django",
    "dns",
    "do",
    "docker",
    "dockerfile",
    "dos",
    "dpr",
    "dsconfig",
    "dst",
    "dts",
    "dust",
    "ebnf",
    "elixir",
    "elm",
    "erb",
    "erl",
    "erlang",
    "erlang-repl",
    "excel",
    "f90",
    "f95",
    "feature",
    "fix",
    "flix",
    "fortran",
    "freepascal",
    "fs",
    "fsharp",
    "gams",
    "gauss",
    "gcode",
    "gemspec",
    "gherkin",
    "glsl",
    "gml",
    "gms",
    "go",
    "golang",
    "golo",
    "gradle",
    "graph",
    "groovy",
    "gss",
    "gyp",
    "h",
    "h++",
    "haml",
    "handlebars",
    "haskell",
    "haxe",
    "hbs",
    "hh",
    "hpp",
    "hs",
    "hsp",
    "html",
    "html.handlebars",
    "html.hbs",
    "htmlbars",
    "http",
    "https",
    "hx",
    "hxx",
    "hy",
    "hylang",
    "i7",
    "iced",
    "icl",
    "inform7",
    "ini",
    "ino",
    "instances",
    "ipython",
    "irb",
    "irpf90",
    "isbl",
    "java",
    "javascript",
    "jboss-cli",
    "jinja",
    "js",
    "json",
    "jsp",
    "jsx",
    "julia",
    "julia-repl",
    "k",
    "kdb",
    "kotlin",
    "kt",
    "kts",
    "lasso",
    "lassoscript",
    "latex",
    "lazarus",
    "ldif",
    "leaf",
    "less",
    "lfm",
    "lisp",
    "livecodeserver",
    "livescript",
    "llvm",
    "lpr",
    "ls",
    "lsl",
    "lua",
    "m",
    "mak",
    "make",
    "makefile",
    "markdown",
    "mathematica",
    "matlab",
    "maxima",
    "md",
    "mel",
    "mercury",
    "mikrotik",
    "mips",
    "mipsasm",
    "mizar",
    "mjs",
    "mk",
    "mkd",
    "mkdown",
    "ml",
    "mm",
    "mma",
    "mojolicious",
    "monkey",
    "moo",
    "moon",
    "moonscript",
    "mysql",
    "n1ql",
    "nc",
    "nginx",
    "nginxconf",
    "nim",
    "nix",
    "nixos",
    "node-repl",
    "nsis",
    "obj-c",
    "obj-c++",
    "objc",
    "objective-c++",
    "objectivec",
    "ocaml",
    "openscad",
    "oracle",
    "osascript",
    "oxygene",
    "p21",
    "parser3",
    "pas",
    "pascal",
    "patch",
    "pb",
    "pbi",
    "pcmk",
    "perl",
    "pf",
    "pf.conf",
    "pgsql",
    "php",
    "php-template",
    "php3",
    "php4",
    "php5",
    "php6",
    "php7",
    "php8",
    "pl",
    "plaintext",
    "plist",
    "pm",
    "podspec",
    "pony",
    "postgres",
    "postgresql",
    "powershell",
    "pp",
    "processing",
    "profile",
    "prolog",
    "properties",
    "protobuf",
    "ps",
    "ps1",
    "puppet",
    "purebasic",
    "py",
    "pycon",
    "python",
    "python-repl",
    "q",
    "qml",
    "qt",
    "r",
    "rb",
    "re",
    "reasonml",
    "rib",
    "roboconf",
    "routeros",
    "rs",
    "rsl",
    "rss",
    "ruby",
    "ruleslanguage",
    "rust",
    "sas",
    "scad",
    "scala",
    "scheme",
    "sci",
    "scilab",
    "scss",
    "sh",
    "shell",
    "smali",
    "smalltalk",
    "sml",
    "sqf",
    "sql",
    "sql_more",
    "st",
    "stan",
    "stanfuncs",
    "stata",
    "step",
    "step21",
    "stp",
    "styl",
    "stylus",
    "subunit",
    "sv",
    "svg",
    "svh",
    "swift",
    "taggerscript",
    "tao",
    "tap",
    "tcl",
    "tex",
    "text",
    "thor",
    "thrift",
    "tk",
    "toml",
    "tp",
    "ts",
    "tsx",
    "twig",
    "txt",
    "typescript",
    "v",
    "vala",
    "vb",
    "vbnet",
    "vbs",
    "vbscript",
    "vbscript-html",
    "verilog",
    "vhdl",
    "vim",
    "wildfly-cli",
    "wl",
    "wsf",
    "x++",
    "x86asm",
    "xhtml",
    "xjb",
    "xl",
    "xls",
    "xlsx",
    "xml",
    "xpath",
    "xq",
    "xquery",
    "xsd",
    "xsl",
    "yaml",
    "yml",
    "zep",
    "zephir",
    "zone",
    "zsh",
];

static ALL_LOADED: AtomicBool = AtomicBool::new(false);

/// TS: `loadAllHighlightLanguages()`。
///
/// TS 在 `setImmediate` 里动态 `import("highlight.js/lib/index.js")`，失败也 resolve。
/// Rust 无动态导入：这里把目录中的其余语言名标记为可用（未实现 grammar 的走 plaintext
/// 回退），并保证可重复调用。
///
/// 实现上只写一个进程级 `AtomicBool`：没有 IO、没有 `await` 点，因此重复调用无副作用。调用后
/// `supports_language` 对目录中的名字返回 `true`，但 `highlight` 只能找到注册表里
/// 已实现的 grammar，其余名字仍按无 span 的 escaped plaintext 输出（文件级“已知偏差”
/// 第 2 条）。
pub async fn load_all_highlight_languages() {
    ALL_LOADED.store(true, Ordering::SeqCst);
}

/// TS: `supportsLanguage(name)` —— `hljs.getLanguage(name) !== undefined`。
///
/// `getLanguage` 先 `toLowerCase()` 再查名字表与别名表。可见性分两个阶段：
/// 模块加载时只注册 `eagerLanguages`（21 种，含别名）；`load_all_highlight_languages()`
/// 之后目录里的其余语言名（含 `html`/`xml`/`diff`/`plaintext` 及其别名）才可见。
///
/// 空串与目录外名字返回 `false`。注意这个名字只是“可见性”判断，不代表 `highlight`
/// 一定能产出 span：已注册但未实现 grammar 的目录语言在 loadAll 之后也返回 `true`。
pub fn supports_language(name: &str) -> bool {
    let lower = name.to_lowercase();
    let loaded = ALL_LOADED.load(Ordering::SeqCst);
    if let Some(&index) = registry().lookup.get(lower.as_str()) {
        // 已实现 grammar 但不属于 eagerLanguages 的语言（html/xml/diff/plaintext）
        // 在 TS 里来自 `highlight.js/lib/index.js`，loadAll 之前 `getLanguage` 不认识。
        return loaded || EAGER_LANGUAGES.contains(&registry().languages[index].name);
    }
    loaded && ALL_LANGUAGE_NAMES.contains(&lower.as_str())
}

/// 语言扫描器分派（grammar 形态）。
///
/// `highlight_language` 按它选择扫描器：`Plaintext`/`Xml`/`Diff`/`Json` 有专用实现，
/// 其余（含 `Python`/`Javascript`/`Typescript`）都走 `scan_generic`。后三者与
/// `Generic` 的差别体现在通用扫描器内部的语言特有分支：js/ts 的 method 与箭头函数、
/// python 空参数列表不产生 `params` span 等。
#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Generic,
    Python,
    Javascript,
    Typescript,
    Json,
    Xml,
    Diff,
    Plaintext,
}

/// 字符串内部的插值形式（对应 highlight.js 的 `subst`/`variable` 子模式）。
#[derive(Clone, Copy)]
enum SubstKind {
    /// javascript/typescript 模板串：`${...}` → `subst`。
    JsBrace,
    /// python f-string：`{...}` → `subst`。
    PythonBrace,
    /// ruby：`#{...}` → `subst`。
    RubyBrace,
    /// nix：`${...}` → `subst`。
    DollarBrace,
    /// bash：`$(...)` → `subst`。
    DollarParen,
    /// bash：`$` + `[\w\d#@]` 起的变量（`$0`/`$@`/`$#` 等位置参数）。
    BashDollarWord,
    /// bash/php/kotlin/dart：`$name` → 指定类。
    DollarWord,
    /// swift：`\(...)` → `subst`。
    SwiftParen,
    /// php：`{$` ... `}`（end 含在 span 内）→ `subst`。
    PhpBrace,
    /// kotlin：`${...}`（`subst`，内部嵌套 NUMBER/STRING）。
    KotlinBrace,
    /// dart：`${...}`（`subst`，内部嵌套 NUMBER/STRING 与 `true false null this is new super`）。
    DartBrace,
}

type KeywordDef = (&'static str, &'static str);
type BlockCommentDef = (&'static str, &'static str, bool);
type SubstDef = (SubstKind, &'static str);

/// 字符串规则（begin/end 为 regex 源码）。
struct StrDef {
    begin: &'static str,
    end: &'static str,
    /// highlight.js 的模式声明了 `illegal: /\n/`（仅当 `ignoreIllegals` 为 false 时
    /// 才生效：字符串里遇到换行会让整段代码退化为 escaped plaintext）。
    ///
    /// 注意这与「能否跨行」无关：`ignoreIllegals: true`（kiana-theme 的固定用法）下
    /// highlight.js 只有 end 词素能结束字符串，换行不会终止；`illegal` 只在
    /// `ignoreIllegals: false` 时抛出 Illegal 并让整个结果变成无 span 的转义文本。
    illegal_newline: bool,
    escapes: bool,
    class: &'static str,
    substs: &'static [SubstDef],
    /// json：字符串后跟 `:` 时输出 `attr` 而不是 `string`。
    attr_if_colon: bool,
}

/// 声明式关键字（`def`/`function`/`fn`…）的渲染方式。
#[derive(Clone, Copy)]
enum DeclStyle {
    /// 只含关键字 + 标题（rust `fn`、swift `func`、scala `def`、perl `sub`）。
    BareTitle,
    /// 关键字 + 标题 + 参数括号（go `func`、kotlin `fun`、lua `function`、ruby `def`）。
    Params,
    /// 关键字 + 标题 + 参数 + 尾随空白，止于 `{`/`;`（javascript/typescript/php）。
    ParamsTrailing,
    /// 关键字 + 标题 + 参数 + `:`（python）。
    ParamsColon,
}

/// 声明关键字（`def`/`function`/`fn`…）的渲染配置：关键字、外层 span 类名，
/// 以及标题/参数列表/尾部的处理方式（见 `DeclStyle`）。
struct DeclDef {
    keyword: &'static str,
    class: &'static str,
    style: DeclStyle,
}

/// highlight.js 的 CLASS / NAMESPACE 模式配置（`class`/`struct`/`namespace` 等关键字
/// 之后的头部一直扫描到 `end_chars`，头部里的标识符按 highlight.js 的 TITLE_MODE 着色）。
///
/// 对应 10.7.3 的：java `beginKeywords: 'class interface enum', end: /[{;=]/, contains:
/// [extends/implements, TITLE_MODE]`、csharp `class`/`namespace`（无 className，含
/// `GENERIC_MODIFIER`）、swift `struct protocol class extension enum`、kotlin `class
/// interface trait`（`<...>` 是 `type` 且止于 `[:(]`）、scala `class object trait type`
/// （`(...)` 是 `params`）、dart/groovy/php 的 class 模式、ruby `class module`（止于行尾）、
/// cpp `enum class struct union`（结束字符在 span 内）与 rust `struct/union/enum/trait`
/// （TITLE 带 `endsParent`，只取第一个标识符）。
#[derive(Clone, Copy)]
struct ClassBodyDef {
    keyword: &'static str,
    /// 外层 span 的类名；空串表示该模式没有 `className`（csharp / php namespace / rust type）。
    class: &'static str,
    /// 结束字符集合；`\n` 表示行尾（ruby / kotlin 的 `$`）。
    end_chars: &'static str,
    /// 结束字符是否进入 span（cpp / groovy / ruby 是；java / js / ts 否）。
    include_end: bool,
    /// 头部里的**所有**标识符都是 title；`false` 时只有第一个。
    title_all: bool,
    /// `( ... )` 是 `params` span（scala 的 `className: 'params'`）。
    params: bool,
    /// `< ... >` 内部是 `type` span（kotlin 的 `className: 'type'`，begin/end 被排除）。
    generics_type: bool,
    /// TITLE_MODE 带 `endsParent`（rust struct/type）：标题结束后整个模式立即结束。
    ends_after_title: bool,
    /// highlight.js 里以 `beginKeywords` 子模式出现的词（java 的 `extends implements`）：
    /// 在头部里输出 `keyword`，即使它们不在语言关键字表里。
    extra_keywords: &'static [&'static str],
    /// 头部里是否套用**语言关键字表**。highlight.js 的 CLASS 模式只有 swift 显式声明
    /// `keywords: KEYWORDS`；其余语言的头部只认各自的 `extra_keywords` 子模式，未命中的
    /// 标识符一律是 `title`（java 的 `keywords: 'class interface enum'` 也放进
    /// `extra_keywords`）。
    keyword_table: bool,
}

impl ClassBodyDef {
    const fn new(keyword: &'static str, class: &'static str, end_chars: &'static str) -> Self {
        Self {
            keyword,
            class,
            end_chars,
            include_end: false,
            title_all: true,
            params: false,
            generics_type: false,
            ends_after_title: false,
            extra_keywords: &[],
            keyword_table: false,
        }
    }

    const fn keyword_table(mut self) -> Self {
        self.keyword_table = true;
        self
    }

    const fn extra_keywords(mut self, words: &'static [&'static str]) -> Self {
        self.extra_keywords = words;
        self
    }

    const fn include_end(mut self) -> Self {
        self.include_end = true;
        self
    }

    const fn single_title(mut self) -> Self {
        self.title_all = false;
        self
    }

    const fn params(mut self) -> Self {
        self.params = true;
        self
    }

    const fn generics_type(mut self) -> Self {
        self.generics_type = true;
        self
    }

    const fn ends_after_title(mut self) -> Self {
        self.ends_after_title = true;
        self
    }
}

/// 语言静态定义（regex 以源码保存，编译期不构造 `Regex`）。
struct LangDef {
    name: &'static str,
    aliases: &'static [&'static str],
    kind: Kind,
    keywords: &'static [KeywordDef],
    /// 标识符 / 关键字匹配（对应 highlight.js 的 `keywords.$pattern`）。
    ident: &'static str,
    line_comments: &'static [&'static str],
    block_comments: &'static [BlockCommentDef],
    strings: &'static [StrDef],
    number: Option<&'static str>,
    /// `variable` 类 begin-only 规则（bash `$x`、php `$x`、ruby `@x`）。
    variables: &'static [&'static str],
    /// `symbol` 类 begin-only 规则（ruby `:sym`、rust `'a`）。
    symbols: &'static [&'static str],
    /// 整体匹配后输出指定类的 begin-only 规则（rust 字符字面量等）。
    specials: &'static [(&'static str, &'static str)],
    /// 整体匹配后按原文输出的规则（perl 的 `$x`：highlight.js 只消费不着色）。
    plain_specials: &'static [&'static str],
    decls: &'static [DeclDef],
    /// `class`/`struct`/`namespace` 等 CLASS 模式（见 [`ClassBodyDef`]）。
    class_bodies: &'static [ClassBodyDef],
    /// `@Decorator` → `meta`（java/kotlin/scala/swift/dart/groovy）。
    annotation: bool,
    /// highlight.js `beginKeywords` 模式的关键字（java 的 `new throw return else`）：
    /// 词首匹配产出 `keyword`，但前一个字符是 `.` 时整体跳过、回落到普通关键字表
    /// （`skipIfhasPrecedingDot`）。
    begin_keywords: &'static [&'static str],
    /// language 级别的 `illegal` 正则：`ignoreIllegals` 为 false 时，在没有其他规则
    /// 命中的位置命中即让整段代码退化为转义纯文本（highlight.js 的 Illegal 异常）。
    top_illegal: &'static [&'static str],
    /// 行首 `#include` 等 → `meta`（c/cpp）。
    preprocessor: bool,
    /// `#!/bin/bash` → `meta`。
    shebang: bool,
    /// 行首（允许缩进）`@decorator` → `meta`（python）。
    decorator: bool,
    /// nix：标识符后跟 `=` → `attr`。
    attr_assign: bool,
    /// javascript/typescript：`/re/g` → `regexp`。
    js_regexp: bool,
    /// C/Java/C++/C#：`返回类型 名字(` 形式的方法声明 → `function`/`title`/`params`。
    infer_functions: bool,
    /// scala 的顶层 `TYPE` 模式：`\b[A-Z][A-Za-z0-9_]*` → `type`。
    upper_type: bool,
    /// c/cpp 的 `CPP_PRIMITIVE_TYPES`：`\b[a-z\d_]*_t\b` → `keyword`（`size_t` 等）。
    primitive_types: bool,
    disable_autodetect: bool,
}

impl Default for LangDef {
    fn default() -> Self {
        Self {
            name: "",
            aliases: &[],
            kind: Kind::Generic,
            keywords: &[],
            ident: "[A-Za-z_][A-Za-z0-9_]*",
            line_comments: &[],
            block_comments: &[],
            strings: &[],
            number: None,
            variables: &[],
            symbols: &[],
            specials: &[],
            plain_specials: &[],
            decls: &[],
            class_bodies: &[],
            annotation: false,
            begin_keywords: &[],
            top_illegal: &[],
            preprocessor: false,
            shebang: false,
            decorator: false,
            attr_assign: false,
            js_regexp: false,
            infer_functions: false,
            upper_type: false,
            primitive_types: false,
            disable_autodetect: false,
        }
    }
}

// 常用正则源码。
const C_NUMBER: &str = r"\b(?:0[xX][0-9a-fA-F_]+|0[bB][01_]+|0[oO][0-7_]+|(?:\d[\d_]*(?:\.[\d_]*)?|\.\d[\d_]*)(?:[eE][+-]?[\d_]+)?)[uUlLfFdD]*";
const JS_NUMBER: &str = r"\b(?:0[xX][0-9a-fA-F_]+|0[bB][01_]+|0[oO][0-7_]+|\d[\d_]*(?:\.[\d_]*)?(?:[eE][+-]?[\d_]+)?)n?\b|\.\d[\d_]*(?:[eE][+-]?[\d_]+)?";
const PY_NUMBER: &str = r"\b(?:0[xX][0-9a-fA-F_]+|0[bB][01_]+|0[oO][0-7_]+|\d[\d_]*(?:\.[\d_]*)?(?:[eE][+-]?[\d_]+)?[jJlL]?)\b|\.\d[\d_]*(?:[eE][+-]?[\d_]+)?[jJ]?";
const JSON_NUMBER: &str = r"-?\b\d+(?:\.\d+)?(?:[eE][+-]?\d+)?\b";
const SIMPLE_NUMBER: &str = r"\b\d+(?:\.\d+)?";
// go.js：`hljs.C_NUMBER_RE + '[i]'` 与 `hljs.C_NUMBER_MODE`（`C_NUMBER_RE` 是
// `(-?)(\b0[xX][a-fA-F0-9]+|(\b\d+(\.\d*)?|\.\d+)([eE][-+]?\d+)?)`，**不含**
// 下划线或类型后缀；`1_000` 只匹配到 `1`）。第一变体的 `i` 后缀合并为可选的 `i?`。
const GO_NUMBER: &str = r"(-?)(?:\b0[xX][a-fA-F0-9]+|(?:\b\d+(?:\.\d*)?|\.\d+)(?:[eE][-+]?\d+)?)i?";
// rust.js 的数字：小数部分要求 `\.[0-9_]+`（`1.` 只匹配 `1`，tuple 字段 `x.0.y` 不会把点吃进 number）。
const RUST_NUMBER: &str = r"\b(?:0[xX][0-9a-fA-F_]+|0[bB][01_]+|0[oO][0-7_]+|\d[\d_]*(?:\.[0-9_]+)?(?:[eE][+-]?[0-9_]+)?)(?:[iu](?:8|16|32|64|128|size)|f(?:32|64))?";

const BLOCK_C: &[BlockCommentDef] = &[("/\\*", "\\*/", false)];
const BLOCK_C_NESTED: &[BlockCommentDef] = &[("/\\*", "\\*/", true)];

const STR_DQ: StrDef = StrDef {
    begin: "\"",
    end: "\"",
    illegal_newline: true,
    escapes: true,
    class: "string",
    substs: &[],
    attr_if_colon: false,
};
const STR_SQ: StrDef = StrDef {
    begin: "'",
    end: "'",
    illegal_newline: true,
    escapes: true,
    class: "string",
    substs: &[],
    attr_if_colon: false,
};
/// 不声明 `illegal: /\n/` 的引号字符串（php/python 带前缀/ruby/bash/perl/lua/swift/rust
/// 的对应模式）：`ignoreIllegals: false` 时换行也不构成非法输入。
const STR_DQ_LEGAL: StrDef = StrDef {
    begin: "\"",
    end: "\"",
    illegal_newline: false,
    escapes: true,
    class: "string",
    substs: &[],
    attr_if_colon: false,
};
const STR_SQ_LEGAL: StrDef = StrDef {
    begin: "'",
    end: "'",
    illegal_newline: false,
    escapes: true,
    class: "string",
    substs: &[],
    attr_if_colon: false,
};
const STR_BACKTICK_RAW: StrDef = StrDef {
    begin: "`",
    end: "`",
    illegal_newline: false,
    escapes: false,
    class: "string",
    substs: &[],
    attr_if_colon: false,
};
const STR_TRIPLE_DQ: StrDef = StrDef {
    begin: "\"\"\"",
    end: "\"\"\"",
    illegal_newline: false,
    escapes: true,
    class: "string",
    substs: &[],
    attr_if_colon: false,
};

const STRINGS_C: &[StrDef] = &[STR_DQ, STR_SQ];
/// php.js 的 SINGLE_QUOTED / DOUBLE_QUOTED 把 `illegal` 覆盖为 `null`；双引号串内含
/// `SUBST`（`\$\w+`）。
const STR_DQ_PHP: StrDef = StrDef {
    begin: "\"",
    end: "\"",
    illegal_newline: false,
    escapes: true,
    class: "string",
    // php.js SUBST 的两个变体：`\$\w+`（先）与 `\{\$` … `\}`。
    substs: &[
        (SubstKind::DollarWord, "subst"),
        (SubstKind::PhpBrace, "subst"),
    ],
    attr_if_colon: false,
};
const STRINGS_PHP: &[StrDef] = &[STR_DQ_PHP, STR_SQ_LEGAL];
const STRINGS_GO: &[StrDef] = &[STR_BACKTICK_RAW, STR_DQ, STR_SQ];
const STRINGS_JAVA: &[StrDef] = &[STR_DQ, STR_SQ];
const STRINGS_CSHARP: &[StrDef] = &[
    // csharp.js 的 INTERPOLATED_STRING：`$"` 起，内含 SUBST（`{...}`）。
    StrDef {
        begin: "\\$\"",
        end: "\"",
        illegal_newline: true,
        escapes: true,
        class: "string",
        substs: &[(SubstKind::PythonBrace, "subst")],
        attr_if_colon: false,
    },
    StrDef {
        begin: "@\"",
        end: "\"",
        illegal_newline: false,
        escapes: false,
        class: "string",
        substs: &[],
        attr_if_colon: false,
    },
    STR_DQ,
    STR_SQ,
];
const STRINGS_RUST: &[StrDef] = &[
    StrDef {
        begin: "b\"",
        end: "\"",
        illegal_newline: false,
        escapes: true,
        class: "string",
        substs: &[],
        attr_if_colon: false,
    },
    // rust.js 继承 QUOTE_STRING_MODE 但把 illegal 覆盖为 null。
    STR_DQ_LEGAL,
];
const STRINGS_JS: &[StrDef] = &[
    StrDef {
        begin: "`",
        end: "`",
        illegal_newline: false,
        escapes: true,
        class: "string",
        substs: &[(SubstKind::JsBrace, "subst")],
        attr_if_colon: false,
    },
    STR_DQ,
    STR_SQ,
];
const STRINGS_JSON: &[StrDef] = &[StrDef {
    begin: "\"",
    end: "\"",
    illegal_newline: true,
    escapes: true,
    class: "string",
    substs: &[],
    attr_if_colon: true,
}];
const STRINGS_PY: &[StrDef] = &[
    StrDef {
        begin: "(?:[fF][rR]|[rR][fF]|[fF])\"\"\"",
        end: "\"\"\"",
        illegal_newline: false,
        escapes: true,
        class: "string",
        substs: &[(SubstKind::PythonBrace, "subst")],
        attr_if_colon: false,
    },
    StrDef {
        begin: "(?:[fF][rR]|[rR][fF]|[fF])'''",
        end: "'''",
        illegal_newline: false,
        escapes: true,
        class: "string",
        substs: &[(SubstKind::PythonBrace, "subst")],
        attr_if_colon: false,
    },
    StrDef {
        begin: "(?:[uUbB]|[rR]|[bB][rR]|[rR][bB])?\"\"\"",
        end: "\"\"\"",
        illegal_newline: false,
        escapes: true,
        class: "string",
        substs: &[],
        attr_if_colon: false,
    },
    StrDef {
        begin: "(?:[uUbB]|[rR]|[bB][rR]|[rR][bB])?'''",
        end: "'''",
        illegal_newline: false,
        escapes: true,
        class: "string",
        substs: &[],
        attr_if_colon: false,
    },
    StrDef {
        begin: "(?:[fF][rR]|[rR][fF]|[fF])\"",
        end: "\"",
        illegal_newline: false,
        escapes: true,
        class: "string",
        substs: &[(SubstKind::PythonBrace, "subst")],
        attr_if_colon: false,
    },
    StrDef {
        begin: "(?:[fF][rR]|[rR][fF]|[fF])'",
        end: "'",
        illegal_newline: false,
        escapes: true,
        class: "string",
        substs: &[(SubstKind::PythonBrace, "subst")],
        attr_if_colon: false,
    },
    // python.js: /([uU]|[rR])'/、/([bB]|[bB][rR]|[rR][bB])'/ —— 裸 r/R 前缀也算 raw 串。
    StrDef {
        begin: "(?:[uUbB]|[rR]|[bB][rR]|[rR][bB])\"",
        end: "\"",
        illegal_newline: false,
        escapes: true,
        class: "string",
        substs: &[],
        attr_if_colon: false,
    },
    StrDef {
        begin: "(?:[uUbB]|[rR]|[bB][rR]|[rR][bB])'",
        end: "'",
        illegal_newline: false,
        escapes: true,
        class: "string",
        substs: &[],
        attr_if_colon: false,
    },
    STR_DQ,
    STR_SQ,
];
const STRINGS_BASH: &[StrDef] = &[
    StrDef {
        begin: "\\$'",
        end: "'",
        illegal_newline: false,
        escapes: true,
        class: "string",
        substs: &[],
        attr_if_colon: false,
    },
    StrDef {
        begin: "\"",
        end: "\"",
        illegal_newline: false,
        escapes: true,
        class: "string",
        substs: &[
            (SubstKind::DollarBrace, "variable"),
            (SubstKind::DollarParen, "subst"),
            (SubstKind::BashDollarWord, "variable"),
        ],
        attr_if_colon: false,
    },
    STR_SQ_LEGAL,
];
const STRINGS_RUBY: &[StrDef] = &[
    StrDef {
        begin: "\"",
        end: "\"",
        illegal_newline: false,
        escapes: true,
        class: "string",
        substs: &[(SubstKind::RubyBrace, "subst")],
        attr_if_colon: false,
    },
    STR_SQ_LEGAL,
];
const STRINGS_PERL: &[StrDef] = &[STR_DQ_LEGAL, STR_SQ_LEGAL];
const STRINGS_LUA: &[StrDef] = &[
    StrDef {
        begin: "\\[\\[",
        end: "\\]\\]",
        illegal_newline: false,
        escapes: false,
        class: "string",
        substs: &[],
        attr_if_colon: false,
    },
    // lua.js 用 hljs.APOS_STRING_MODE / QUOTE_STRING_MODE（illegal: /\n/）。
    STR_DQ,
    STR_SQ,
];
const STRINGS_NIX: &[StrDef] = &[
    StrDef {
        begin: "''",
        end: "''",
        illegal_newline: false,
        escapes: false,
        class: "string",
        substs: &[(SubstKind::DollarBrace, "subst")],
        attr_if_colon: false,
    },
    StrDef {
        begin: "\"",
        end: "\"",
        illegal_newline: false,
        escapes: true,
        class: "string",
        substs: &[(SubstKind::DollarBrace, "subst")],
        attr_if_colon: false,
    },
];
const STRINGS_SCALA: &[StrDef] = &[
    // scala.js 的 STRING 变体：`[a-z]+` 前缀的插值串（含 SUBST）。
    StrDef {
        begin: "[a-z]+\"\"\"",
        end: "\"\"\"",
        illegal_newline: false,
        escapes: true,
        class: "string",
        substs: &[
            (SubstKind::DollarBrace, "subst"),
            (SubstKind::DollarWord, "subst"),
        ],
        attr_if_colon: false,
    },
    StrDef {
        begin: "[a-z]+\"",
        end: "\"",
        illegal_newline: true,
        escapes: true,
        class: "string",
        substs: &[
            (SubstKind::DollarBrace, "subst"),
            (SubstKind::DollarWord, "subst"),
        ],
        attr_if_colon: false,
    },
    STR_TRIPLE_DQ,
    STR_DQ,
    STR_SQ,
];
const STRINGS_KOTLIN: &[StrDef] = &[
    STR_TRIPLE_DQ,
    StrDef {
        begin: "\"",
        end: "\"",
        illegal_newline: true,
        escapes: true,
        class: "string",
        substs: &[
            (SubstKind::KotlinBrace, "subst"),
            (SubstKind::DollarWord, "variable"),
        ],
        attr_if_colon: false,
    },
];
const STRINGS_SWIFT: &[StrDef] = &[
    STR_TRIPLE_DQ,
    StrDef {
        begin: "\"",
        end: "\"",
        illegal_newline: false,
        escapes: true,
        class: "string",
        substs: &[(SubstKind::SwiftParen, "subst")],
        attr_if_colon: false,
    },
];

/// dart.js 的 STRING：三种引号都含 SUBST（`\$[A-Za-z0-9_]+`）与 BRACED_SUBST（`${...}`）。
const STRINGS_DART: &[StrDef] = &[
    StrDef {
        begin: "'''",
        end: "'''",
        illegal_newline: false,
        escapes: true,
        class: "string",
        substs: &[
            (SubstKind::DartBrace, "subst"),
            (SubstKind::DollarWord, "subst"),
        ],
        attr_if_colon: false,
    },
    StrDef {
        begin: "\"\"\"",
        end: "\"\"\"",
        illegal_newline: false,
        escapes: true,
        class: "string",
        substs: &[
            (SubstKind::DartBrace, "subst"),
            (SubstKind::DollarWord, "subst"),
        ],
        attr_if_colon: false,
    },
    StrDef {
        begin: "\"",
        end: "\"",
        illegal_newline: true,
        escapes: true,
        class: "string",
        substs: &[
            (SubstKind::DartBrace, "subst"),
            (SubstKind::DollarWord, "subst"),
        ],
        attr_if_colon: false,
    },
    StrDef {
        begin: "'",
        end: "'",
        illegal_newline: true,
        escapes: true,
        class: "string",
        substs: &[
            (SubstKind::DartBrace, "subst"),
            (SubstKind::DollarWord, "subst"),
        ],
        attr_if_colon: false,
    },
];

const STRINGS_GROOVY: &[StrDef] = &[
    StrDef {
        begin: "'''",
        end: "'''",
        illegal_newline: false,
        escapes: true,
        class: "string",
        substs: &[],
        attr_if_colon: false,
    },
    STR_TRIPLE_DQ,
    STR_DQ,
    STR_SQ,
];

// 声明式关键字表。
const DECLS_PY: &[DeclDef] = &[
    DeclDef {
        keyword: "def",
        class: "function",
        style: DeclStyle::ParamsColon,
    },
    DeclDef {
        keyword: "class",
        class: "class",
        style: DeclStyle::ParamsColon,
    },
];
const DECLS_JS: &[DeclDef] = &[DeclDef {
    keyword: "function",
    class: "function",
    style: DeclStyle::ParamsTrailing,
}];
const DECLS_RUST: &[DeclDef] = &[DeclDef {
    keyword: "fn",
    class: "function",
    style: DeclStyle::BareTitle,
}];
const DECLS_GO: &[DeclDef] = &[DeclDef {
    keyword: "func",
    class: "function",
    style: DeclStyle::Params,
}];
const DECLS_KOTLIN: &[DeclDef] = &[DeclDef {
    keyword: "fun",
    class: "function",
    style: DeclStyle::Params,
}];
const DECLS_SWIFT: &[DeclDef] = &[DeclDef {
    keyword: "func",
    class: "function",
    style: DeclStyle::BareTitle,
}];
const DECLS_SCALA: &[DeclDef] = &[DeclDef {
    keyword: "def",
    class: "function",
    style: DeclStyle::BareTitle,
}];
const DECLS_RUBY: &[DeclDef] = &[DeclDef {
    keyword: "def",
    class: "function",
    style: DeclStyle::Params,
}];
const DECLS_PHP: &[DeclDef] = &[
    DeclDef {
        keyword: "function",
        class: "function",
        style: DeclStyle::ParamsTrailing,
    },
    // php.js: `beginKeywords: 'fn function'`（PHP 7.4 箭头函数）。
    DeclDef {
        keyword: "fn",
        class: "function",
        style: DeclStyle::ParamsTrailing,
    },
];
const DECLS_LUA: &[DeclDef] = &[DeclDef {
    keyword: "function",
    class: "function",
    style: DeclStyle::Params,
}];
const DECLS_PERL: &[DeclDef] = &[DeclDef {
    keyword: "sub",
    class: "function",
    style: DeclStyle::BareTitle,
}];
const DECLS_JAVA: &[DeclDef] = &[];
const DECLS_CSHARP: &[DeclDef] = &[];
const DECLS_GROOVY: &[DeclDef] = &[];
const DECLS_CPP: &[DeclDef] = &[];

// CLASS / NAMESPACE 模式（highlight.js 10.7.3 的逐个 grammar，见 `ClassBodyDef` 的文档）。
const CLASS_JS: &[ClassBodyDef] =
    &[ClassBodyDef::new("class", "class", "{;=").extra_keywords(JS_EXTENDS)];
const JAVA_EXTENDS: &[&str] = &["extends", "implements", "class", "interface", "enum"];
const JS_EXTENDS: &[&str] = &["extends"];
const DART_EXTENDS: &[&str] = &["extends", "implements"];
const SCALA_EXTENDS: &[&str] = &["extends", "with"];
const CSHARP_WHERE: &[&str] = &["where", "class"];
const KOTLIN_MODIFIERS: &[&str] = &["public", "protected", "internal", "private", "constructor"];
const CPP_FINAL: &[&str] = &["final", "class", "struct"];
const CLASS_JAVA: &[ClassBodyDef] = &[
    ClassBodyDef::new("class", "class", "{;=").extra_keywords(JAVA_EXTENDS),
    ClassBodyDef::new("interface", "class", "{;=").extra_keywords(JAVA_EXTENDS),
    ClassBodyDef::new("enum", "class", "{;=").extra_keywords(JAVA_EXTENDS),
];
const CLASS_DART: &[ClassBodyDef] = &[
    ClassBodyDef::new("class", "class", "{").extra_keywords(DART_EXTENDS),
    ClassBodyDef::new("interface", "class", "{").extra_keywords(DART_EXTENDS),
];
const CLASS_GROOVY: &[ClassBodyDef] = &[
    ClassBodyDef::new("class", "class", "{")
        .include_end()
        .extra_keywords(DART_EXTENDS),
    ClassBodyDef::new("interface", "class", "{")
        .include_end()
        .extra_keywords(DART_EXTENDS),
    ClassBodyDef::new("trait", "class", "{")
        .include_end()
        .extra_keywords(DART_EXTENDS),
    ClassBodyDef::new("enum", "class", "{")
        .include_end()
        .extra_keywords(DART_EXTENDS),
];
const CLASS_PHP: &[ClassBodyDef] = &[
    ClassBodyDef::new("class", "class", "{").extra_keywords(DART_EXTENDS),
    ClassBodyDef::new("interface", "class", "{").extra_keywords(DART_EXTENDS),
    ClassBodyDef::new("trait", "class", "{").extra_keywords(DART_EXTENDS),
    ClassBodyDef::new("enum", "class", "{").extra_keywords(DART_EXTENDS),
    ClassBodyDef::new("namespace", "", ";"),
    ClassBodyDef::new("use", "", ";"),
];
const CLASS_CSHARP: &[ClassBodyDef] = &[
    ClassBodyDef::new("class", "", "{;=").extra_keywords(CSHARP_WHERE),
    ClassBodyDef::new("interface", "", "{;=").extra_keywords(CSHARP_WHERE),
    ClassBodyDef::new("namespace", "", "{;="),
    // csharp.js 的 record 模式（与 namespace 同形，但没有 `where class` 子模式）：
    // TITLE_MODE 会把头部里的每个标识符都着成 title（`record R(int X);` 的 `int` 也是）。
    ClassBodyDef::new("record", "", "{;="),
];
const CLASS_SWIFT: &[ClassBodyDef] = &[
    ClassBodyDef::new("struct", "class", "{").keyword_table(),
    ClassBodyDef::new("protocol", "class", "{").keyword_table(),
    ClassBodyDef::new("class", "class", "{").keyword_table(),
    ClassBodyDef::new("extension", "class", "{").keyword_table(),
    ClassBodyDef::new("enum", "class", "{").keyword_table(),
];
const CLASS_KOTLIN: &[ClassBodyDef] = &[
    ClassBodyDef::new("class", "class", ":{(\n")
        .single_title()
        .generics_type()
        .extra_keywords(KOTLIN_MODIFIERS),
    ClassBodyDef::new("interface", "class", ":{(\n")
        .single_title()
        .generics_type()
        .extra_keywords(KOTLIN_MODIFIERS),
];
const CLASS_SCALA: &[ClassBodyDef] = &[
    ClassBodyDef::new("class", "class", ":={\n;[")
        .params()
        .extra_keywords(SCALA_EXTENDS),
    ClassBodyDef::new("object", "class", ":={\n;[")
        .params()
        .extra_keywords(SCALA_EXTENDS),
    ClassBodyDef::new("trait", "class", ":={\n;[")
        .params()
        .extra_keywords(SCALA_EXTENDS),
    ClassBodyDef::new("type", "class", ":={\n;[")
        .params()
        .extra_keywords(SCALA_EXTENDS),
];
const CLASS_RUBY: &[ClassBodyDef] = &[
    ClassBodyDef::new("class", "class", "\n;")
        .include_end()
        .single_title(),
    ClassBodyDef::new("module", "class", "\n;")
        .include_end()
        .single_title(),
];
const CLASS_CPP: &[ClassBodyDef] = &[
    ClassBodyDef::new("enum", "class", "{;:<>=}")
        .include_end()
        .extra_keywords(CPP_FINAL),
    ClassBodyDef::new("class", "class", "{;:<>=}")
        .include_end()
        .extra_keywords(CPP_FINAL),
    ClassBodyDef::new("struct", "class", "{;:<>=}")
        .include_end()
        .extra_keywords(CPP_FINAL),
    ClassBodyDef::new("union", "class", "{;:<>=}")
        .include_end()
        .extra_keywords(CPP_FINAL),
];
const CLASS_RUST: &[ClassBodyDef] = &[
    ClassBodyDef::new("struct", "class", "{")
        .single_title()
        .ends_after_title(),
    ClassBodyDef::new("union", "class", "{")
        .single_title()
        .ends_after_title(),
    ClassBodyDef::new("enum", "class", "{")
        .single_title()
        .ends_after_title(),
    ClassBodyDef::new("trait", "class", "{")
        .single_title()
        .ends_after_title(),
    ClassBodyDef::new("type", "class", ";")
        .single_title()
        .ends_after_title(),
];

const RUST_SYMBOLS: &[&str] = &["'[A-Za-z_][A-Za-z0-9_]*"];
/// ruby.js 的 `symbol`：`IDENT(!|\?)?:`（关键字参数 / hash key），然后是 `:(?!\s)`（`:sym`）。
const RUBY_SYMBOLS: &[&str] = &[
    "[A-Za-z_][A-Za-z0-9_]*[!?]?:",
    ":(?:[A-Za-z_][A-Za-z0-9_]*[!?=]?)",
];
const RUBY_VARIABLES: &[&str] = &["@@?[A-Za-z_][A-Za-z0-9_]*", "\\$[A-Za-z_][A-Za-z0-9_]*"];
const PHP_VARIABLES: &[&str] = &[r"\$+[A-Za-z_\u{7f}-\u{ff}][A-Za-z0-9_\u{7f}-\u{ff}]*"];
const BASH_VARIABLES: &[&str] = &["\\$\\{[^}\\n]*\\}", "\\$[A-Za-z0-9_#@][A-Za-z0-9_]*"];
const PERL_PLAIN_VARIABLES: &[&str] = &["[$@%][A-Za-z0-9_]+"];
/// php.js: `(::|->)+ident` 整体吞掉，避免把成员名当关键字（`$o->class`）。
const PHP_PLAIN_SPECIALS: &[&str] =
    &[r"(?:::|->)+[A-Za-z_\u{7f}-\u{ff}][A-Za-z0-9_\u{7f}-\u{ff}]*"];

const RUST_SPECIALS: &[(&str, &str)] = &[
    (r"'(?:\\.|[^\\'\n])'", "string"),
    ("#!?\\[[^\\]\\n]*\\]", "meta"),
];
/// php.js: `{ className: 'keyword', begin: /\$this\b/ }` 排在 VARIABLE 之前。
const PHP_SPECIALS: &[(&str, &str)] = &[("<\\?[pP][hH][pP]", "meta"), ("\\$this\\b", "keyword")];
/// swift.js 的 AVAILABLE_ATTRIBUTE：`@available` / `#available` → keyword。
const SWIFT_SPECIALS: &[(&str, &str)] = &[("(?:@|#)available", "keyword")];

// language 级别的 `illegal`（highlight.js 只在 `ignoreIllegals: false` 时抛错）。
// 逐条对应 10.7.3 的 language 定义；`#[^...]|#\z` 是 js/ts 的
// `#(?![$_A-z])` 在 regex crate（不支持 lookaround）下的等价写法。
const ILLEGAL_PYTHON: &[&str] = &["</|->|\\?|=>"];
const ILLEGAL_JAVA: &[&str] = &["</|#"];
const ILLEGAL_GO: &[&str] = &["</"];
const ILLEGAL_JS: &[&str] = &[r"#[^$_A-Za-z\[\]\\^`]|#\z"];
const ILLEGAL_C: &[&str] = &["</"];
const ILLEGAL_RUST: &[&str] = &["</"];
const ILLEGAL_RUBY: &[&str] = &["/\\*"];
const ILLEGAL_GROOVY: &[&str] = &["#|</"];
const ILLEGAL_CSHARP: &[&str] = &["::"];

/// java.js 的 `beginKeywords: 'new throw return else'`；`new` 不在关键字表里，
/// 其余三个虽然也在表里，但前有 `.` 时会被整体跳过（`skipIfhasPrecedingDot`）。
const BEGIN_KEYWORDS_JAVA: &[&str] = &["new", "throw", "return", "else"];

fn lang_defs() -> Vec<LangDef> {
    vec![
        LangDef {
            name: "python",
            aliases: &["py", "gyp", "ipython"],
            kind: Kind::Python,
            keywords: KW_PYTHON,
            ident: "[A-Za-z]\\w+|__\\w+__",
            line_comments: &["#"],
            strings: STRINGS_PY,
            number: Some(PY_NUMBER),
            decls: DECLS_PY,
            decorator: true,
            top_illegal: ILLEGAL_PYTHON,
            ..LangDef::default()
        },
        LangDef {
            name: "java",
            aliases: &["jsp"],
            keywords: KW_JAVA,
            ident: "\\w+",
            line_comments: &["//"],
            block_comments: BLOCK_C,
            strings: STRINGS_JAVA,
            number: Some(C_NUMBER),
            decls: DECLS_JAVA,
            class_bodies: CLASS_JAVA,
            annotation: true,
            begin_keywords: BEGIN_KEYWORDS_JAVA,
            top_illegal: ILLEGAL_JAVA,
            infer_functions: true,
            ..LangDef::default()
        },
        LangDef {
            name: "go",
            aliases: &["golang"],
            keywords: KW_GO,
            ident: "\\w+",
            line_comments: &["//"],
            block_comments: BLOCK_C,
            strings: STRINGS_GO,
            number: Some(GO_NUMBER),
            decls: DECLS_GO,
            top_illegal: ILLEGAL_GO,
            ..LangDef::default()
        },
        LangDef {
            name: "javascript",
            aliases: &["js", "jsx", "mjs", "cjs"],
            kind: Kind::Javascript,
            keywords: KW_JAVASCRIPT,
            ident: "[A-Za-z$_][0-9A-Za-z$_]*",
            line_comments: &["//"],
            block_comments: BLOCK_C,
            strings: STRINGS_JS,
            number: Some(JS_NUMBER),
            decls: DECLS_JS,
            class_bodies: CLASS_JS,
            js_regexp: true,
            top_illegal: ILLEGAL_JS,
            ..LangDef::default()
        },
        LangDef {
            name: "json",
            aliases: &[],
            kind: Kind::Json,
            keywords: KW_JSON,
            ident: "\\w+",
            line_comments: &["//"],
            block_comments: BLOCK_C,
            strings: STRINGS_JSON,
            number: Some(JSON_NUMBER),
            ..LangDef::default()
        },
        LangDef {
            name: "cpp",
            aliases: &["cc", "c++", "h++", "hpp", "hh", "hxx", "cxx"],
            keywords: KW_CPP,
            ident: "\\w+",
            line_comments: &["//"],
            block_comments: BLOCK_C,
            strings: STRINGS_C,
            number: Some(C_NUMBER),
            decls: DECLS_CPP,
            class_bodies: CLASS_CPP,
            primitive_types: true,
            preprocessor: true,
            top_illegal: ILLEGAL_C,
            infer_functions: true,
            ..LangDef::default()
        },
        LangDef {
            name: "typescript",
            aliases: &["ts", "tsx"],
            kind: Kind::Typescript,
            keywords: KW_TYPESCRIPT,
            ident: "[A-Za-z$_][0-9A-Za-z$_]*",
            line_comments: &["//"],
            block_comments: BLOCK_C,
            strings: STRINGS_JS,
            number: Some(JS_NUMBER),
            decls: DECLS_JS,
            class_bodies: CLASS_JS,
            js_regexp: true,
            top_illegal: ILLEGAL_JS,
            ..LangDef::default()
        },
        LangDef {
            name: "php",
            aliases: &["php3", "php4", "php5", "php6", "php7", "php8"],
            keywords: KW_PHP,
            ident: "\\w+",
            line_comments: &["//", "#"],
            block_comments: BLOCK_C,
            strings: STRINGS_PHP,
            number: Some(C_NUMBER),
            variables: PHP_VARIABLES,
            specials: PHP_SPECIALS,
            plain_specials: PHP_PLAIN_SPECIALS,
            decls: DECLS_PHP,
            class_bodies: CLASS_PHP,
            ..LangDef::default()
        },
        LangDef {
            name: "ruby",
            aliases: &["rb", "gemspec", "podspec", "thor", "irb"],
            keywords: KW_RUBY,
            ident: "[A-Za-z_][A-Za-z0-9_]*[?!]?",
            line_comments: &["#"],
            strings: STRINGS_RUBY,
            number: Some(SIMPLE_NUMBER),
            variables: RUBY_VARIABLES,
            symbols: RUBY_SYMBOLS,
            decls: DECLS_RUBY,
            class_bodies: CLASS_RUBY,
            top_illegal: ILLEGAL_RUBY,
            ..LangDef::default()
        },
        LangDef {
            name: "c",
            aliases: &["h"],
            keywords: KW_C,
            ident: "\\w+",
            line_comments: &["//"],
            block_comments: BLOCK_C,
            strings: STRINGS_C,
            number: Some(C_NUMBER),
            // c.js 与 cpp.js 共用同一段 CLASS 规则（`beginKeywords: 'enum class struct union'`，
            // end `[{;:<>=]`，contains `final class struct` + TITLE_MODE）；`struct` 只经由
            // 该模式成为关键字（不在 CPP_KEYWORDS 表里）。
            class_bodies: CLASS_CPP,
            primitive_types: true,
            preprocessor: true,
            top_illegal: ILLEGAL_C,
            infer_functions: true,
            ..LangDef::default()
        },
        LangDef {
            name: "csharp",
            aliases: &["cs", "c#"],
            keywords: KW_CSHARP,
            ident: "\\w+",
            line_comments: &["//"],
            block_comments: BLOCK_C,
            strings: STRINGS_CSHARP,
            number: Some(C_NUMBER),
            decls: DECLS_CSHARP,
            class_bodies: CLASS_CSHARP,
            top_illegal: ILLEGAL_CSHARP,
            infer_functions: true,
            ..LangDef::default()
        },
        LangDef {
            name: "nix",
            aliases: &["nixos"],
            keywords: KW_NIX,
            ident: "\\w+",
            line_comments: &["#"],
            block_comments: BLOCK_C,
            strings: STRINGS_NIX,
            number: Some(SIMPLE_NUMBER),
            attr_assign: true,
            ..LangDef::default()
        },
        LangDef {
            name: "bash",
            aliases: &["sh", "zsh"],
            keywords: KW_BASH,
            ident: "[A-Za-z_.][A-Za-z0-9_.-]*",
            line_comments: &["#"],
            strings: STRINGS_BASH,
            // highlight.js 的 bash grammar 只在算术上下文里标数字，顶层不标。
            number: None,
            variables: BASH_VARIABLES,
            shebang: true,
            ..LangDef::default()
        },
        LangDef {
            name: "rust",
            aliases: &["rs"],
            keywords: KW_RUST,
            ident: "[A-Za-z][A-Za-z0-9_]*!?",
            line_comments: &["//"],
            block_comments: BLOCK_C_NESTED,
            strings: STRINGS_RUST,
            number: Some(RUST_NUMBER),
            symbols: RUST_SYMBOLS,
            specials: RUST_SPECIALS,
            decls: DECLS_RUST,
            class_bodies: CLASS_RUST,
            top_illegal: ILLEGAL_RUST,
            ..LangDef::default()
        },
        LangDef {
            name: "scala",
            aliases: &[],
            keywords: KW_SCALA,
            // scala.js 的顶层 contains 里有 `TYPE`（`\b[A-Z][A-Za-z0-9_]*` → `type`）。
            upper_type: true,
            ident: "\\w+",
            line_comments: &["//"],
            block_comments: BLOCK_C,
            strings: STRINGS_SCALA,
            number: Some(C_NUMBER),
            decls: DECLS_SCALA,
            class_bodies: CLASS_SCALA,
            annotation: true,
            ..LangDef::default()
        },
        LangDef {
            name: "kotlin",
            aliases: &["kt", "kts"],
            keywords: KW_KOTLIN,
            ident: "\\w+",
            line_comments: &["//"],
            block_comments: BLOCK_C_NESTED,
            strings: STRINGS_KOTLIN,
            number: Some(C_NUMBER),
            decls: DECLS_KOTLIN,
            class_bodies: CLASS_KOTLIN,
            annotation: true,
            ..LangDef::default()
        },
        LangDef {
            name: "swift",
            aliases: &[],
            // swift.js 的顶层 contains 里有 `TYPE`（大写开头的标识符 → `type`）。
            upper_type: true,
            keywords: KW_SWIFT,
            // swift.js 的 `keywords.$pattern` 是 `\b\w+|#\w+`。这里把 `#` 限定在字母/下划线
            // 开头，避免把 `#65` 整个当成标识符吞掉（hljs 的 NUMBER 规则会在 `#` 之后命中）。
            ident: "#?[A-Za-z_]\\w*",
            line_comments: &["//"],
            block_comments: BLOCK_C_NESTED,
            strings: STRINGS_SWIFT,
            number: Some(C_NUMBER),
            decls: DECLS_SWIFT,
            class_bodies: CLASS_SWIFT,
            specials: SWIFT_SPECIALS,
            annotation: true,
            ..LangDef::default()
        },
        LangDef {
            name: "dart",
            aliases: &[],
            keywords: KW_DART,
            ident: "[A-Za-z][A-Za-z0-9_]*",
            line_comments: &["//"],
            block_comments: BLOCK_C_NESTED,
            strings: STRINGS_DART,
            number: Some(C_NUMBER),
            class_bodies: CLASS_DART,
            annotation: true,
            ..LangDef::default()
        },
        LangDef {
            name: "groovy",
            aliases: &[],
            keywords: KW_GROOVY,
            ident: "\\w+",
            line_comments: &["//"],
            block_comments: BLOCK_C,
            strings: STRINGS_GROOVY,
            number: Some(C_NUMBER),
            decls: DECLS_GROOVY,
            class_bodies: CLASS_GROOVY,
            annotation: true,
            top_illegal: ILLEGAL_GROOVY,
            ..LangDef::default()
        },
        LangDef {
            name: "perl",
            aliases: &["pl", "pm"],
            keywords: KW_PERL,
            ident: "[A-Za-z_][A-Za-z0-9_]*",
            line_comments: &["#"],
            strings: STRINGS_PERL,
            number: Some(SIMPLE_NUMBER),
            plain_specials: PERL_PLAIN_VARIABLES,
            decls: DECLS_PERL,
            ..LangDef::default()
        },
        LangDef {
            name: "lua",
            aliases: &[],
            keywords: KW_LUA,
            ident: "[A-Za-z_][A-Za-z0-9_]*",
            line_comments: &["--"],
            block_comments: &[("--\\[\\[", "\\]\\]", false)],
            strings: STRINGS_LUA,
            number: Some(SIMPLE_NUMBER),
            decls: DECLS_LUA,
            ..LangDef::default()
        },
        LangDef {
            name: "xml",
            aliases: &[
                "html", "xhtml", "rss", "atom", "xjb", "xsd", "xsl", "plist", "wsf", "svg",
            ],
            kind: Kind::Xml,
            ..LangDef::default()
        },
        LangDef {
            name: "diff",
            aliases: &["patch"],
            kind: Kind::Diff,
            ..LangDef::default()
        },
        LangDef {
            name: "plaintext",
            aliases: &["text", "txt"],
            kind: Kind::Plaintext,
            disable_autodetect: true,
            ..LangDef::default()
        },
    ]
}

// ---------------------------------------------------------------------------
// 编译后的语言对象与全局注册表
// ---------------------------------------------------------------------------

/// 关键字 → span 类名的查找表（区分大小写），由 `compile` 从
/// `(class, "空格分隔的词")` 列表展开；以下划线开头的类别在展开时被跳过。
struct KeywordTable(HashMap<&'static str, &'static str>);

impl KeywordTable {
    fn compile(defs: &'static [KeywordDef]) -> Self {
        let mut map = HashMap::new();
        for (class, words) in defs {
            // highlight.js：以下划线开头的类别只参与 relevance，不产生 span。
            if class.starts_with('_') {
                continue;
            }
            for word in words.split_whitespace() {
                map.insert(word, *class);
            }
        }
        Self(map)
    }

    fn get(&self, word: &str) -> Option<&'static str> {
        self.0.get(word).copied()
    }
}

/// 编译后的字符串规则：`begin`/`end` 是锚定正则，其余字段由 `StrDef` 原样复制。
struct CompiledStr {
    begin: AnchoredRegex,
    end: AnchoredRegex,
    illegal_newline: bool,
    escapes: bool,
    class: &'static str,
    substs: &'static [SubstDef],
    attr_if_colon: bool,
}

/// 运行时语言对象：`LangDef` 的静态表与正则编译结果。`name` 保存规范名（别名只出现在
/// `Registry::lookup` 里），所以语言特有分支（如 `lang.name == "swift"`）按规范名判断。
struct CompiledLanguage {
    name: &'static str,
    kind: Kind,
    keywords: KeywordTable,
    ident: AnchoredRegex,
    line_comments: Vec<&'static str>,
    block_comments: Vec<(AnchoredRegex, AnchoredRegex, bool)>,
    strings: Vec<CompiledStr>,
    number: Option<AnchoredRegex>,
    variables: Vec<AnchoredRegex>,
    symbols: Vec<AnchoredRegex>,
    specials: Vec<(AnchoredRegex, &'static str)>,
    plain_specials: Vec<AnchoredRegex>,
    decls: &'static [DeclDef],
    class_bodies: &'static [ClassBodyDef],
    annotation: bool,
    begin_keywords: &'static [&'static str],
    top_illegal: Vec<AnchoredRegex>,
    preprocessor: bool,
    shebang: bool,
    decorator: bool,
    attr_assign: bool,
    js_regexp: bool,
    infer_functions: bool,
    upper_type: bool,
    primitive_types: bool,
    disable_autodetect: bool,
}

/// 把 `LangDef` 的 regex 源码编译成 `AnchoredRegex`：字符串、注释、特殊 token、
/// 变量、符号与 illegal 的每条规则都在这里一次性编译。内置模式是常量，`expect` 只是
/// 对“模式可编译”的断言；写错时会在注册表初始化阶段 panic，而不是静默降级。
fn compile_language(def: &LangDef) -> CompiledLanguage {
    let compile = AnchoredRegex::new;
    let compile_str = |str_def: &StrDef| CompiledStr {
        begin: compile(str_def.begin),
        end: compile(str_def.end),
        illegal_newline: str_def.illegal_newline,
        escapes: str_def.escapes,
        class: str_def.class,
        substs: str_def.substs,
        attr_if_colon: str_def.attr_if_colon,
    };
    CompiledLanguage {
        name: def.name,
        kind: def.kind,
        keywords: KeywordTable::compile(def.keywords),
        ident: compile(def.ident),
        line_comments: def.line_comments.to_vec(),
        block_comments: def
            .block_comments
            .iter()
            .map(|(begin, end, nested)| (compile(begin), compile(end), *nested))
            .collect(),
        strings: def.strings.iter().map(compile_str).collect(),
        number: def.number.map(compile),
        variables: def.variables.iter().map(|re| compile(re)).collect(),
        symbols: def.symbols.iter().map(|re| compile(re)).collect(),
        specials: def
            .specials
            .iter()
            .map(|(re, class)| (compile(re), *class))
            .collect(),
        plain_specials: def.plain_specials.iter().map(|re| compile(re)).collect(),
        decls: def.decls,
        class_bodies: def.class_bodies,
        annotation: def.annotation,
        begin_keywords: def.begin_keywords,
        top_illegal: def.top_illegal.iter().map(|re| compile(re)).collect(),
        preprocessor: def.preprocessor,
        shebang: def.shebang,
        decorator: def.decorator,
        attr_assign: def.attr_assign,
        js_regexp: def.js_regexp,
        infer_functions: def.infer_functions,
        upper_type: def.upper_type,
        primitive_types: def.primitive_types,
        disable_autodetect: def.disable_autodetect,
    }
}

/// 全部已实现 grammar 与“名字/别名 → 下标”索引；由 `registry()` 惰性初始化。
struct Registry {
    languages: Vec<CompiledLanguage>,
    lookup: HashMap<&'static str, usize>,
}

static REGISTRY: OnceLock<Registry> = OnceLock::new();

/// 惰性构建全局注册表：`lang_defs()` 的静态定义只编译一次并常驻；`OnceLock` 保证
/// 并发首次调用也只初始化一次，之后所有查询都是只读的 `HashMap` 查找。
fn registry() -> &'static Registry {
    REGISTRY.get_or_init(|| {
        let defs = lang_defs();
        let mut languages = Vec::with_capacity(defs.len());
        let mut lookup = HashMap::new();
        for def in &defs {
            let index = languages.len();
            languages.push(compile_language(def));
            lookup.insert(def.name, index);
            for alias in def.aliases {
                lookup.insert(*alias, index);
            }
        }
        Registry { languages, lookup }
    })
}

// ---------------------------------------------------------------------------
// 通用扫描器（highlight.js mode 引擎的简化等价物）
// ---------------------------------------------------------------------------

/// 把文本按 highlight.js 的 `escapeHTML` 转义进输出。
fn escape_html_into(output: &mut String, text: &str) {
    for ch in text.chars() {
        match ch {
            '&' => output.push_str("&amp;"),
            '<' => output.push_str("&lt;"),
            '>' => output.push_str("&gt;"),
            '"' => output.push_str("&quot;"),
            '\'' => output.push_str("&#x27;"),
            _ => output.push(ch),
        }
    }
}

fn escape_html(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    escape_html_into(&mut out, text);
    out
}

/// 扫描器的输出缓冲：`out` 累积已转义的 HTML（`<span class="hljs-*">`），
/// `relevance` 累积自动检测分数。
///
/// `span` 在包一层 span 的同时把 relevance 加一；需要嵌套的场合（function/params/
/// subst 等）用 `open`/`close` 手工配对，relevance 由调用方按需显式累加。文本一律经
/// `plain`/`plain_char` 转义后写入。
struct Writer {
    out: String,
    relevance: u32,
}

impl Writer {
    fn new() -> Self {
        Self {
            out: String::new(),
            relevance: 0,
        }
    }

    fn plain(&mut self, text: &str) {
        escape_html_into(&mut self.out, text);
    }

    fn plain_char(&mut self, ch: char) {
        match ch {
            '&' => self.out.push_str("&amp;"),
            '<' => self.out.push_str("&lt;"),
            '>' => self.out.push_str("&gt;"),
            '"' => self.out.push_str("&quot;"),
            '\'' => self.out.push_str("&#x27;"),
            _ => self.out.push(ch),
        }
    }

    fn open(&mut self, class: &str) {
        self.out.push_str("<span class=\"hljs-");
        self.out.push_str(class);
        self.out.push_str("\">");
    }

    fn close(&mut self) {
        self.out.push_str("</span>");
    }

    fn span(&mut self, class: &str, text: &str) {
        self.open(class);
        self.plain(text);
        self.close();
        self.relevance += 1;
    }
}

/// 一个扫描用正则的两种锚定形式。
///
/// `re_at` 需要「在字节下标 `index` 处命中」的语义。直接 `Regex::find_at` 会从
/// `index` 起向后搜索整个剩余文本：某个位置不命中时仍可能一路扫到串尾，扫描器
/// 在每个字符上调用十余条规则，整体退化为 O(n²)（16 KB 无引号的 Python 就要数秒，
/// 与 highlight.js 的线性合并正则差距巨大）。
///
/// 这里把每条规则预编译为两个只可能在切片起点尝试的变体：
///
/// - `at_start` = `\A(?:pat)`：`index == 0` 时直接在整串上匹配；
/// - `after_one` = `\A[\s\S](?:pat)`：先吃掉 `index` 前的一个字符再匹配，这样
///   `\b`/`\B` 仍然能看到前一个字符（正则 crate 会把切片起点当作字符串开头，
///   与真实下标的边界语义不同）。
///
/// 两者都锚定在切片起点，失败代价是 O(1)；命中时扫描消耗的长度与规则本身相同，
/// 因此整体保持线性。
struct AnchoredRegex {
    at_start: Regex,
    after_one: Regex,
}

impl AnchoredRegex {
    fn new(source: &str) -> Self {
        Self {
            at_start: Regex::new(&format!(r"\A(?:{source})")).expect("anchored highlight regex"),
            after_one: Regex::new(&format!(r"\A[\s\S](?:{source})"))
                .expect("anchored highlight regex"),
        }
    }
}

/// `re` 在 `index`（UTF-8 字节下标，必须是字符边界）处命中时返回结束下标。
fn re_at(re: &AnchoredRegex, text: &str, index: usize) -> Option<usize> {
    if index > text.len() {
        return None;
    }
    if index == 0 {
        return re.at_start.find(text).map(|m| m.end());
    }
    let prev_len = text[..index].chars().next_back()?.len_utf8();
    let start = index - prev_len;
    let m = re.after_one.find(&text[start..])?;
    Some(start + m.end())
}

fn title_ident_regex() -> &'static AnchoredRegex {
    static RE: OnceLock<AnchoredRegex> = OnceLock::new();
    RE.get_or_init(|| AnchoredRegex::new(r"[A-Za-z_$][A-Za-z0-9_$]*"))
}

/// lua.js 的 function 标题：`([_a-zA-Z]\w*\.)*([_a-zA-Z]\w*:)?[_a-zA-Z]\w*`。
fn lua_title_regex() -> &'static AnchoredRegex {
    static RE: OnceLock<AnchoredRegex> = OnceLock::new();
    RE.get_or_init(|| AnchoredRegex::new(r"(?:[_a-zA-Z]\w*\.)*(?:[_a-zA-Z]\w*:)?[_a-zA-Z]\w*"))
}

/// ruby.js 的 `def` 标题（`RUBY_METHOD_RE` 的标识符子集）：`ident[!?=]?`
/// （`def f?` / `def f!` / `def f=`）。
fn ruby_title_regex() -> &'static AnchoredRegex {
    static RE: OnceLock<AnchoredRegex> = OnceLock::new();
    RE.get_or_init(|| AnchoredRegex::new(r"[A-Za-z_][A-Za-z0-9_]*[!?=]?"))
}

fn char_len(text: &str, index: usize) -> usize {
    text[index..].chars().next().map_or(1, char::len_utf8)
}

/// 从 `index` 到下一个 `\n`（不含）或串尾；返回值要么是换行的下标，要么是
/// `text.len()`。
fn line_end(text: &str, index: usize) -> usize {
    text[index..]
        .find('\n')
        .map_or(text.len(), |offset| index + offset)
}

/// 只把 `\n` 当作行首边界（`index == 0` 也算）；`\r`/U+2028/U+2029 不算。需要 JS
/// 多行正则语义（`^`）的地方必须用 `js_line_start`，两者不可互换。
fn at_line_start(text: &str, index: usize) -> bool {
    index == 0 || text.as_bytes()[index - 1] == b'\n'
}

/// JS 正则多行模式里 `^`/`$` 认定的行终止符（含 `\r`、U+2028、U+2029）。
fn is_js_line_terminator(ch: char) -> bool {
    matches!(ch, '\n' | '\r' | '\u{2028}' | '\u{2029}')
}

/// JS 多行正则 `^` 的语义：串首或任意 JS 行终止符之后。
fn js_line_start(text: &str, index: usize) -> bool {
    index == 0
        || text[..index]
            .chars()
            .next_back()
            .is_some_and(is_js_line_terminator)
}

/// JS 正则 `\s` 的字符集（与 renderer 的 JS_WS 一致）。
fn is_js_whitespace(ch: char) -> bool {
    matches!(
        ch,
        '\t' | '\n' | '\u{0B}' | '\u{0C}' | '\r' | ' ' | '\u{00A0}' | '\u{1680}' | '\u{2000}'
            ..='\u{200A}'
                | '\u{2028}'
                | '\u{2029}'
                | '\u{202F}'
                | '\u{205F}'
                | '\u{3000}'
                | '\u{FEFF}'
    )
}

/// 从 `index` 起 JS 空白连续段的终点。
fn js_whitespace_run_end(text: &str, index: usize) -> usize {
    let mut p = index;
    while p < text.len() {
        let ch = text[p..].chars().next().expect("char boundary");
        if is_js_whitespace(ch) {
            p += ch.len_utf8();
        } else {
            break;
        }
    }
    p
}

/// javascript.js 的 `begin: /^\s*['"]use (strict|asm)['"]/`（`^` 是 JS 多行语义，
/// `\s` 是 JS 的空白集，可跨空行）。
fn js_use_strict_regex() -> &'static AnchoredRegex {
    static RE: OnceLock<AnchoredRegex> = OnceLock::new();
    RE.get_or_init(|| AnchoredRegex::new(&format!(r#"{JS_WS}*['"]use (?:strict|asm)['"]"#)))
}

/// typescript.js 用 `^\s*['"]use strict['"]` 替换了 js 的版本（不含 asm）。
fn ts_use_strict_regex() -> &'static AnchoredRegex {
    static RE: OnceLock<AnchoredRegex> = OnceLock::new();
    RE.get_or_init(|| AnchoredRegex::new(&format!(r#"{JS_WS}*['"]use strict['"]"#)))
}

/// 只跳过空格、Tab 与 CR，返回其后第一个字符的字节下标；用于需要精确控制空白集合的
/// 场合。范围更宽的 Unicode 空白请用 `next_non_whitespace`。
fn next_non_space(text: &str, index: usize) -> usize {
    let mut p = index;
    while p < text.len() {
        match text.as_bytes()[p] {
            b' ' | b'\t' | b'\r' => p += 1,
            _ => break,
        }
    }
    p
}

/// 跳过 `char::is_whitespace` 认定的全部 Unicode 空白，返回其后第一个字符的下标。
/// 比 `next_non_space` 宽（后者只认空格/Tab/CR），两者不可互换。
fn next_non_whitespace(text: &str, index: usize) -> usize {
    let mut p = index;
    while p < text.len() {
        let ch = text[p..].chars().next().expect("char boundary");
        if ch.is_whitespace() {
            p += ch.len_utf8();
        } else {
            break;
        }
    }
    p
}

/// 从 `begin` 之后（`p`）扫描到配对的 `end`；`nested` 为真时按嵌套深度计数。
/// 未闭合的注释一直吃到串尾（与 highlight.js 的 end 永不命中时一致）。
fn scan_block_comment(
    text: &str,
    mut p: usize,
    begin: &AnchoredRegex,
    end: &AnchoredRegex,
    nested: bool,
) -> usize {
    let mut depth = 1usize;
    while p < text.len() {
        if let Some(after) = re_at(end, text, p) {
            depth -= 1;
            p = after;
            if depth == 0 {
                return p;
            }
            continue;
        }
        if nested && let Some(after) = re_at(begin, text, p) {
            depth += 1;
            p = after;
            continue;
        }
        p += char_len(text, p);
    }
    text.len()
}

/// 在字符串内容里尝试匹配一个插值/变量片段；命中时返回 `(结束下标, 类名, 形式)`，不写输出。
fn match_subst(
    text: &str,
    p: usize,
    substs: &[SubstDef],
) -> Option<(usize, &'static str, SubstKind)> {
    for (kind, class) in substs {
        match kind {
            SubstKind::JsBrace => {
                if text[p..].starts_with("${")
                    && let Some(offset) = text[p + 2..].find('}')
                {
                    return Some((p + 2 + offset + 1, class, *kind));
                }
            }
            SubstKind::PythonBrace => {
                if text.as_bytes()[p] == b'{'
                    && let Some(offset) = text[p + 1..].find('}')
                {
                    return Some((p + 1 + offset + 1, class, *kind));
                }
            }
            SubstKind::RubyBrace => {
                if text[p..].starts_with("#{")
                    && let Some(offset) = text[p + 2..].find('}')
                {
                    return Some((p + 2 + offset + 1, class, *kind));
                }
            }
            SubstKind::DollarBrace => {
                if text[p..].starts_with("${")
                    && let Some(offset) = text[p + 2..].find('}')
                {
                    return Some((p + 2 + offset + 1, class, *kind));
                }
            }
            SubstKind::KotlinBrace | SubstKind::DartBrace => {
                if text[p..].starts_with("${")
                    && let Some(offset) = text[p + 2..].find('}')
                {
                    return Some((p + 2 + offset + 1, class, *kind));
                }
            }
            SubstKind::PhpBrace => {
                // php.js SUBST 的 `\{\$` ... `\}`（end 含在 span 内）。
                if text[p..].starts_with("{$")
                    && let Some(offset) = text[p + 2..].find('}')
                {
                    return Some((p + 2 + offset + 1, class, *kind));
                }
            }
            SubstKind::DollarParen => {
                if text[p..].starts_with("$(") {
                    // bash.js SUBST: begin /\$\(/, end /\)/，contains BACKSLASH_ESCAPE
                    // 与 QUOTE_STRING；这里按同样的规则跳过转义与引号内的 `)`。
                    let mut q = p + 2;
                    while q < text.len() {
                        match text.as_bytes()[q] {
                            b'\\' => q += 2,
                            b')' => return Some((q + 1, class, *kind)),
                            b'"' => {
                                q += 1;
                                while q < text.len() {
                                    match text.as_bytes()[q] {
                                        b'\\' => q += 2,
                                        b'"' => {
                                            q += 1;
                                            break;
                                        }
                                        _ => q += 1,
                                    }
                                }
                            }
                            _ => q += 1,
                        }
                    }
                }
            }
            SubstKind::BashDollarWord => {
                if text.as_bytes()[p] == b'$' && p + 1 < text.len() {
                    // bash.js VAR: /\$[\w\d#@][\w\d_]*/
                    let next = text.as_bytes()[p + 1];
                    if next.is_ascii_alphanumeric() || matches!(next, b'_' | b'#' | b'@') {
                        let mut end = p + 2;
                        while end < text.len() {
                            let c = text.as_bytes()[end];
                            if c.is_ascii_alphanumeric() || c == b'_' {
                                end += 1;
                            } else {
                                break;
                            }
                        }
                        return Some((end, class, *kind));
                    }
                }
            }
            SubstKind::DollarWord => {
                if text.as_bytes()[p] == b'$' && p + 1 < text.len() {
                    let next = text.as_bytes()[p + 1];
                    if next.is_ascii_alphabetic() || next == b'_' {
                        let mut end = p + 2;
                        while end < text.len() {
                            let c = text[end..].chars().next().expect("char boundary");
                            if c.is_ascii_alphanumeric() || c == '_' {
                                end += c.len_utf8();
                            } else {
                                break;
                            }
                        }
                        return Some((end, class, *kind));
                    }
                }
            }
            SubstKind::SwiftParen => {
                if text[p..].starts_with("\\(")
                    && let Some(offset) = text[p + 2..].find(')')
                {
                    return Some((p + 2 + offset + 1, class, *kind));
                }
            }
        }
    }
    None
}

/// 扫描中命中了 highlight.js 的 illegal 词素。
///
/// highlight.js 在 `ignoreIllegals` 为 false 时抛出 Illegal 异常并返回**整段**
/// 转义文本（没有 span）；调用方据此回退为 `escape_html(code)`。
#[derive(Debug)]
struct IllegalMatch;

/// 扫描字符串内容直到 `end`；返回结束下标（`end` 之后）。内容写入 `writer`（不含外层 span）。
///
/// 换行本身永远不会结束字符串（highlight.js 的字符串模式只有 end 词素能结束它）；
/// `illegal_newline` 只在 `ignoreIllegals` 为 false 时把换行变成 Illegal。
fn scan_string_content(
    lang: &CompiledLanguage,
    text: &str,
    mut p: usize,
    str_def: &CompiledStr,
    writer: &mut Writer,
    ignore_illegals: bool,
) -> Result<usize, IllegalMatch> {
    let mut segment_start = p;
    while p < text.len() {
        let ch = text[p..].chars().next().expect("char boundary");
        if ch == '\n' && str_def.illegal_newline && !ignore_illegals {
            return Err(IllegalMatch);
        }
        if let Some(end) = re_at(&str_def.end, text, p) {
            // highlight.js 的 end 词素包含在 span 内。
            if segment_start < end {
                writer.plain(&text[segment_start..end]);
            }
            return Ok(end);
        }
        if let Some((after, class, kind)) = match_subst(text, p, str_def.substs) {
            if segment_start < p {
                writer.plain(&text[segment_start..p]);
            }
            match kind {
                // js/ts 模板串的 `${...}`：hljs 的 `SUBST` 模式 contains PARAMS_CONTAINS，
                // 内部 token 继续着色（数字/字符串/关键字…），它们嵌套在 subst span 内。
                SubstKind::JsBrace => {
                    debug_assert!(after >= p + 3, "js subst span is at least three bytes");
                    writer.open(class);
                    writer.relevance += 1;
                    writer.plain("${");
                    scan_inline(lang, &text[p + 2..after - 1], writer, ignore_illegals)?;
                    writer.plain_char('}');
                    writer.close();
                }
                // kotlin/dart 的 `${...}`：SUBST/BRACED_SUBST 只嵌套 NUMBER/STRING
                //（dart 另有 `true false null this is new super` 关键字）。
                SubstKind::KotlinBrace | SubstKind::DartBrace => {
                    debug_assert!(after >= p + 3, "subst span is at least three bytes");
                    writer.open(class);
                    writer.relevance += 1;
                    writer.plain("${");
                    scan_braced_subst(
                        lang,
                        &text[p + 2..after - 1],
                        writer,
                        ignore_illegals,
                        matches!(kind, SubstKind::DartBrace),
                    )?;
                    writer.plain_char('}');
                    writer.close();
                }
                _ => writer.span(class, &text[p..after]),
            }
            p = after;
            segment_start = p;
            continue;
        }
        // swift 的 `\(...)` 插值以反斜杠开头：必须先尝试 SUBST（`INTERPOLATION`），
        // 否则 `\(` 会被当成普通转义序列消费掉（`\(x)` 在 hljs 里是 subst span）。
        if str_def.escapes && ch == '\\' {
            p += ch.len_utf8();
            if p < text.len() {
                p += char_len(text, p);
            }
            continue;
        }
        p += ch.len_utf8();
    }
    if segment_start < p {
        writer.plain(&text[segment_start..p]);
    }
    Ok(p)
}

/// 按语言里的注册顺序尝试字符串规则，第一个 begin 命中的规则负责整个字符串。
/// 命中时先写 begin 文本、扫描内容（含嵌套的 `subst`）再关闭 span，返回结束下标。
///
/// 返回 `Some(Err(IllegalMatch))` 时输出已经写了一半，但调用方会丢弃整个 `Writer`，
/// 因此不需要回滚。
fn try_string(
    lang: &CompiledLanguage,
    text: &str,
    index: usize,
    writer: &mut Writer,
    ignore_illegals: bool,
) -> Option<Result<usize, IllegalMatch>> {
    for str_def in &lang.strings {
        let Some(begin_end) = re_at(&str_def.begin, text, index) else {
            continue;
        };
        writer.open(str_def.class);
        writer.relevance += 1;
        writer.plain(&text[index..begin_end]);
        let end = scan_string_content(lang, text, begin_end, str_def, writer, ignore_illegals);
        writer.close();
        return Some(end);
    }
    None
}

fn try_block_comment(
    lang: &CompiledLanguage,
    text: &str,
    index: usize,
    writer: &mut Writer,
) -> Option<usize> {
    for (begin, end, nested) in &lang.block_comments {
        if let Some(begin_end) = re_at(begin, text, index) {
            let comment_end = scan_block_comment(text, begin_end, begin, end, *nested);
            writer.span("comment", &text[index..comment_end]);
            return Some(comment_end);
        }
    }
    None
}

/// 读取参数括号 `( ... )` 的内容区间（不含括号）；返回 `(内容起点, 内容终点, 括号后位置)`。
///
/// 可以跨行：highlight.js 的 params 模式 end 是 `/\)/`，没有 `illegal: /\n/`，
/// 多行签名（`def f(\n a,\n b=1,\n):`）会得到一个跨行的 params span。
fn paren_content(text: &str, open: usize) -> Option<(usize, usize, usize)> {
    if text.as_bytes().get(open) != Some(&b'(') {
        return None;
    }
    let mut depth = 0usize;
    let mut p = open;
    while p < text.len() {
        let ch = text[p..].chars().next().expect("char boundary");
        match ch {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    return Some((open + 1, p, p + 1));
                }
            }
            _ => {}
        }
        p += ch.len_utf8();
    }
    None
}

/// 对一段内容做"内联"扫描：关键字、数字、字符串、注释（用于参数列表）。
fn scan_inline(
    lang: &CompiledLanguage,
    text: &str,
    writer: &mut Writer,
    ignore_illegals: bool,
) -> Result<(), IllegalMatch> {
    let mut i = 0usize;
    while i < text.len() {
        // 参数列表里的字符串和顶层一样，必须带起始定界符并开启 `string` span
        // （highlight.js 会在 `params` 内嵌套完整的 string mode）。
        match try_string(lang, text, i, writer, ignore_illegals) {
            Some(Ok(end)) => {
                i = end;
                continue;
            }
            Some(Err(illegal)) => return Err(illegal),
            None => {}
        }
        // 注释：只按各 grammar 的 params contains 实际包含的注释模式处理
        //（java/csharp/php 的 params 只含块注释；go 的 params 二者都没有）。
        if params_allow_line_comment(lang.name)
            && lang.line_comments.iter().any(|p| text[i..].starts_with(*p))
        {
            let end = line_end(text, i);
            writer.span("comment", &text[i..end]);
            i = end;
            continue;
        }
        if params_allow_block_comment(lang.name)
            && let Some(end) = try_block_comment(lang, text, i, writer)
        {
            i = end;
            continue;
        }
        // 整体特殊 token（c/cpp 的 `_t` 类型、rust 字符字面量等）。
        let mut matched = false;
        for (re, class) in &lang.specials {
            if let Some(end) = re_at(re, text, i) {
                writer.span(class, &text[i..end]);
                i = end;
                matched = true;
                break;
            }
        }
        if matched {
            continue;
        }
        // 变量（bash/php/ruby）：参数列表里的字符串/数字一样参与内联扫描。
        let mut matched = false;
        for re in &lang.variables {
            if let Some(end) = re_at(re, text, i) {
                writer.span("variable", &text[i..end]);
                i = end;
                matched = true;
                break;
            }
        }
        if matched {
            continue;
        }
        // 符号（ruby `a:` 关键字参数、`:sym`）。
        let mut matched = false;
        for re in &lang.symbols {
            if let Some(end) = re_at(re, text, i) {
                writer.span("symbol", &text[i..end]);
                i = end;
                matched = true;
                break;
            }
        }
        if matched {
            continue;
        }
        // js/ts 的正则字面量（PARAMS_CONTAINS 里有 REGEXP_MODE）。
        if lang.js_regexp
            && text.as_bytes()[i] == b'/'
            && regexp_allowed(text, i)
            && let Some(end) = try_js_regexp(text, i)
        {
            writer.span("regexp", &text[i..end]);
            i = end;
            continue;
        }
        if let Some(end) = lang.number.as_ref().and_then(|re| re_at(re, text, i)) {
            writer.span("number", &text[i..end]);
            i = end;
            continue;
        }
        if let Some(end) = re_at(&lang.ident, text, i) {
            let word = &text[i..end];
            // c/cpp 的 `CPP_PRIMITIVE_TYPES`：参数列表里的 `size_t` 也是 keyword。
            if lang.primitive_types && is_c_primitive_type(word) {
                writer.span("keyword", word);
                i = end;
                continue;
            }
            // swift/scala 的顶层 `TYPE` 模式：大写开头的标识符 → `type`
            //（与主扫描器一致；swift 的参数列表里也需要）。
            if lang.upper_type && word.starts_with(|ch: char| ch.is_uppercase()) {
                writer.span("type", word);
            } else if lang.decls.iter().any(|decl| decl.keyword == word) {
                writer.span("keyword", word);
            } else if let Some(class) = lang.keywords.get(word) {
                writer.span(class, word);
            } else {
                writer.plain(word);
            }
            i = end;
            continue;
        }
        let ch = text[i..].chars().next().expect("char boundary");
        writer.plain_char(ch);
        i += ch.len_utf8();
    }
    Ok(())
}

/// params/装饰器参数内部是否含行注释模式（hljs 各 grammar 的 params contains）。
fn params_allow_line_comment(name: &str) -> bool {
    matches!(
        name,
        "python" | "javascript" | "typescript" | "c" | "cpp" | "ruby" | "lua"
    )
}

/// params/装饰器参数内部是否含块注释模式。
fn params_allow_block_comment(name: &str) -> bool {
    matches!(
        name,
        "javascript" | "typescript" | "c" | "cpp" | "java" | "csharp" | "php" | "lua"
    )
}

/// kotlin/dart 的 `${...}` 内部：只嵌套 NUMBER 与 STRING（dart 另有
/// `true false null this is new super` 关键字）。其余文本保持 subst 文本。
fn scan_braced_subst(
    lang: &CompiledLanguage,
    text: &str,
    writer: &mut Writer,
    ignore_illegals: bool,
    dart: bool,
) -> Result<(), IllegalMatch> {
    let mut i = 0usize;
    while i < text.len() {
        match try_string(lang, text, i, writer, ignore_illegals) {
            Some(Ok(end)) => {
                i = end;
                continue;
            }
            Some(Err(illegal)) => return Err(illegal),
            None => {}
        }
        if let Some(end) = lang.number.as_ref().and_then(|re| re_at(re, text, i)) {
            writer.span("number", &text[i..end]);
            i = end;
            continue;
        }
        if dart && let Some(end) = re_at(&lang.ident, text, i) {
            let word = &text[i..end];
            if matches!(
                word,
                "true" | "false" | "null" | "this" | "is" | "new" | "super"
            ) {
                writer.span("keyword", word);
            } else {
                writer.plain(word);
            }
            i = end;
            continue;
        }
        let ch = text[i..].chars().next().expect("char boundary");
        writer.plain_char(ch);
        i += ch.len_utf8();
    }
    Ok(())
}

/// 渲染一条声明（`def foo(...):` / `function f(...)` / `fn main` …）。
///
/// 阶段：
/// 1. 写 `keyword` span（声明关键字）与中间空白，再预读标题（kotlin 跳过预读，由
///    自己的 `fun` 处理；lua/ruby 使用各自的标题模式）；
/// 2. 语言特有的尾部：swift/js/ts/kotlin 交给对应的 `scan_*` 函数；go 的接收者、
///    参数与返回值循环内联在本函数里。写完即返回；
/// 3. 其余语言按 `DeclStyle` 处理 `(...)` 参数列表（括号是否属于 `params` span 取决于
///    语言），再处理后缀：scala 的结束字符、perl 的空白与可选参数、ts/php 的返回类型、
///    php 的 `=>`、python 的 `:`。
///
/// 标题与参数列表都可能缺失（`fn()`、`function *gen`），这些分支在函数体内分别注明。
fn scan_decl(
    lang: &CompiledLanguage,
    text: &str,
    index: usize,
    ident_end: usize,
    decl: &DeclDef,
    writer: &mut Writer,
    ignore_illegals: bool,
) -> Result<usize, IllegalMatch> {
    writer.open(decl.class);
    writer.relevance += 1;
    writer.span("keyword", &text[index..ident_end]);
    let mut p = ident_end;
    let space_end = next_non_whitespace(text, p);
    writer.plain(&text[p..space_end]);
    p = space_end;

    // 标题用通用标识符模式（python 的 `$pattern` 不匹配单字符函数名，但 highlight.js 的
    // `TITLE_MODE` 用 `UNDERSCORE_IDENT_RE`）。标题可省略：`function (a)` / `fn($x)`。
    // lua.js 的 function 模式把 TITLE_MODE 的 begin 换成了
    // `([_a-zA-Z]\w*\.)*([_a-zA-Z]\w*:)?[_a-zA-Z]\w*`（`function M.foo` / `obj:method`）。
    let title_end = if lang.name == "kotlin" {
        // kotlin 的 `fun` 模式自己处理标题（`UNDERSCORE_IDENT_RE\s*\(` 的 lookahead，
        // 只有紧邻参数列表的标识符才是 title；`fun Foo.bar()` 的 `Foo` 是 function 文本）。
        None
    } else if lang.name == "lua" {
        re_at(lua_title_regex(), text, p)
    } else if lang.name == "ruby" {
        re_at(ruby_title_regex(), text, p)
    } else {
        re_at(title_ident_regex(), text, p)
    };
    if let Some(title_end) = title_end {
        writer.span("title", &text[p..title_end]);
        p = title_end;
    }

    // swift.js 的 FUNCTION 模式（`match: lookahead(/\bfunc\b/)`，无 end）：`func` 关键字、
    // 标题、泛型参数 `<...>` 与参数列表 `(...)` 都在同一个 function span 内；参数列表的
    // `endsParent` 让 function 在 `)` 之后结束，没有参数列表时 function 在标题后的空白
    // 之后结束（hljs 给无 end 模式隐式补上 `\B|\b`）。
    if lang.name == "swift" {
        p = scan_swift_function_tail(lang, text, p, writer, ignore_illegals)?;
        writer.close();
        return Ok(p);
    }

    // go.js 的 FUNCTION 模式：`beginKeywords: 'func'`，end `\s*(\{|$)`（excludeEnd），
    // contains 只有 TITLE_MODE 与 params。接收者、函数名、参数、返回值都在同一个
    // function span 内；组件之间的空白也是 function 文本，但结束 `{` 之前的空白不在。
    if lang.name == "go" {
        loop {
            let whitespace_end = next_non_whitespace(text, p);
            // hljs 的正则带 `m` 标志：`$` 在行尾（含 `\r`/`\n`/U+2028/U+2029）也命中，
            // 因此空白串里出现换行时结束位置就是 `p`（结束匹配整体被 excludeEnd）。
            let line_break = text[p..whitespace_end]
                .chars()
                .any(|ch| matches!(ch, '\n' | '\r' | '\u{2028}' | '\u{2029}'));
            let ends_here = whitespace_end >= text.len()
                || text.as_bytes().get(whitespace_end) == Some(&b'{')
                || line_break;
            if ends_here {
                break;
            }
            if let Some(title_end) = re_at(title_ident_regex(), text, p) {
                writer.span("title", &text[p..title_end]);
                p = title_end;
                continue;
            }
            if text.as_bytes().get(p) == Some(&b'(')
                && let Some((inner_start, inner_end, after)) = paren_content(text, p)
            {
                writer.open("params");
                writer.relevance += 1;
                writer.plain_char('(');
                if inner_start < inner_end {
                    scan_inline(lang, &text[inner_start..inner_end], writer, ignore_illegals)?;
                }
                writer.plain_char(')');
                writer.close();
                p = after;
                continue;
            }
            let ch = text[p..].chars().next().expect("char boundary");
            writer.plain_char(ch);
            p += ch.len_utf8();
        }
        writer.close();
        return Ok(p);
    }

    // javascript/typescript 的 FUNCTION 模式（见 [`scan_js_function_tail`]）。
    if matches!(lang.kind, Kind::Javascript | Kind::Typescript) {
        p = scan_js_function_tail(lang, text, p, writer, ignore_illegals)?;
        writer.close();
        return Ok(p);
    }

    // kotlin.js 的 `fun` 模式（见 [`scan_kotlin_function`]）。
    if lang.name == "kotlin" {
        p = scan_kotlin_function(lang, text, p, writer, ignore_illegals)?;
        writer.close();
        return Ok(p);
    }

    if matches!(
        decl.style,
        DeclStyle::Params | DeclStyle::ParamsTrailing | DeclStyle::ParamsColon
    ) {
        let open = next_non_whitespace(text, p);
        if let Some((inner_start, inner_end, after)) = paren_content(text, open) {
            // hljs 里 go/kotlin/ruby/lua 的 `params` span 含括号；python/swift/php 不含
            // （python 的空 `()` 变体还带 `skip: true`，不产生空 params span）。
            if matches!(lang.name, "go" | "kotlin" | "ruby" | "lua") {
                writer.plain(&text[p..open]);
                writer.open("params");
                writer.relevance += 1;
                writer.plain_char('(');
                if inner_start < inner_end {
                    scan_inline(lang, &text[inner_start..inner_end], writer, ignore_illegals)?;
                }
                writer.plain_char(')');
                writer.close();
            } else {
                // 括号本身在 `params` span 之外。
                writer.plain(&text[p..inner_start]);
                if inner_start < inner_end || lang.kind != Kind::Python {
                    writer.open("params");
                    writer.relevance += 1;
                    scan_inline(lang, &text[inner_start..inner_end], writer, ignore_illegals)?;
                    writer.close();
                }
                writer.plain(&text[inner_end..after]);
            }
            p = after;
        } else if title_end.is_some() {
            // ruby.js 的 def 模式 end 是 `$|;`（`;` 含在 span 内、行尾零宽）；
            // PARAMS 带 endsParent，所以只有无括号时才会走到这里。
            if lang.name == "ruby" {
                let mut q = p;
                while q < text.len() {
                    let ch = text[q..].chars().next().expect("char boundary");
                    if ch == '\n' {
                        break;
                    }
                    if ch == ';' {
                        writer.plain_char(';');
                        q += 1;
                        break;
                    }
                    writer.plain_char(ch);
                    q += ch.len_utf8();
                }
                writer.close();
                return Ok(q);
            }
            // 无括号声明（go 的方法接收者等）：标题后结束。
            // python 例外：hljs 的 def/class 模式 end 是 `/:/`（含在 span 内），
            // 无括号（`class P:`）时冒号仍属于 function/class span。
            if !matches!(decl.style, DeclStyle::ParamsColon) {
                writer.close();
                return Ok(p);
            }
        } else {
            // 既无标题也无括号（`function *gen`）：关键字 + 空白后结束。
            writer.close();
            return Ok(p);
        }
    }

    match decl.style {
        DeclStyle::BareTitle | DeclStyle::Params => {
            // scala.js 的 METHOD 模式（`beginKeywords: 'def'`）end 是 `[:={\[(\n;]`
            // （excludeEnd）：标题之后到结束字符之间的文本（通常是一个空格）仍在
            // function span 内（早先 BareTitle 在标题后立刻关闭 span）。
            if lang.name == "scala" {
                let mut q = p;
                while q < text.len() {
                    let ch = text[q..].chars().next().expect("char boundary");
                    if matches!(ch, ':' | '=' | '{' | '[' | '(' | '\n' | ';') {
                        break;
                    }
                    writer.plain_char(ch);
                    q += ch.len_utf8();
                }
                p = q;
                writer.close();
                return Ok(p);
            }
            // perl.js 的 sub 模式 end 是 `(\s*\(.*?\))?[;{]`（excludeEnd）：标题后的
            // 空白（以及可选的参数列表）仍属于 function span。
            if lang.name == "perl" {
                let mut q = p;
                let mut paren_seen = false;
                loop {
                    let ws = next_non_whitespace(text, q);
                    writer.plain(&text[q..ws]);
                    q = ws;
                    if !paren_seen
                        && text.as_bytes().get(q) == Some(&b'(')
                        && let Some((_, _, after)) = paren_content(text, q)
                    {
                        writer.plain(&text[q..after]);
                        q = after;
                        paren_seen = true;
                        continue;
                    }
                    break;
                }
                p = q;
            }
            writer.close();
            Ok(p)
        }
        DeclStyle::ParamsTrailing => {
            // typescript/php 的 function 模式在参数列表后还有返回类型：
            // `function run(a: string): Promise<void> {` / `function run(): void {`，
            // 返回类型标识符是 `title`，前缀 `: ` 与尾部空白都在 function span 内。
            if matches!(lang.name, "typescript" | "php")
                && text[next_non_space(text, p)..].starts_with(':')
            {
                let mut q = next_non_space(text, p);
                loop {
                    let ws = next_non_whitespace(text, q);
                    writer.plain(&text[q..ws]);
                    q = ws;
                    if q >= text.len() {
                        break;
                    }
                    let ch = text[q..].chars().next().expect("char boundary");
                    if matches!(ch, '{' | ';' | '=' | '\n') {
                        break;
                    }
                    if let Some(end) = re_at(&lang.ident, text, q) {
                        writer.span("title", &text[q..end]);
                        q = end;
                        continue;
                    }
                    writer.plain_char(ch);
                    q += ch.len_utf8();
                }
                writer.close();
                return Ok(q);
            }
            let space_end = next_non_whitespace(text, p)
                .min(text.len())
                .min(line_end(text, p));
            writer.plain(&text[p..space_end]);
            let mut end = space_end;
            // php.js 的 function 模式在 `=>` 处 endsParent：箭头和前面的空白都在 span 内。
            if lang.name == "php" && text[end..].starts_with("=>") {
                writer.plain("=>");
                end += 2;
            }
            writer.close();
            Ok(end)
        }
        DeclStyle::ParamsColon => {
            let mut q = p;
            let mut colon_end = None;
            while q < text.len() {
                let ch = text[q..].chars().next().expect("char boundary");
                if ch == ':' || ch == '\n' {
                    colon_end = Some((q, ch == ':'));
                    break;
                }
                q += ch.len_utf8();
            }
            match colon_end {
                Some((colon, true)) => {
                    if p < colon {
                        scan_inline(lang, &text[p..colon], writer, ignore_illegals)?;
                    }
                    writer.plain(&text[colon..colon + 1]);
                    writer.close();
                    Ok(colon + 1)
                }
                Some((newline, false)) => {
                    if p < newline {
                        scan_inline(lang, &text[p..newline], writer, ignore_illegals)?;
                    }
                    writer.close();
                    Ok(newline)
                }
                None => {
                    if p < text.len() {
                        scan_inline(lang, &text[p..], writer, ignore_illegals)?;
                    }
                    writer.close();
                    Ok(text.len())
                }
            }
        }
    }
}

/// javascript/typescript 的 `function` 模式（beginKeywords: 'function'，end `[{;]`
/// excludeEnd）在标题之后的部分。
///
/// contains 只有 `self`、TITLE_MODE（`IDENT_RE`：**每个**标识符都是 title）与 PARAMS
///（括号在 function span 内、内部是 params，`excludeBegin/End` 且空参数不产生 span）。
/// 因此泛型 `<T>`、返回类型 `: T`、箭头/生成器 `*` 都是 function 文本，其中的标识符
/// （包括 `void` 这样的 TS 关键字）都是 title。
fn scan_js_function_tail(
    lang: &CompiledLanguage,
    text: &str,
    mut p: usize,
    writer: &mut Writer,
    ignore_illegals: bool,
) -> Result<usize, IllegalMatch> {
    while p < text.len() {
        let ch = text[p..].chars().next().expect("char boundary");
        // end `[{;]`（excludeEnd）：结束字符不在 function span 内。
        if matches!(ch, '{' | ';') {
            return Ok(p);
        }
        // function mode 的 `illegal: /%/`。
        if ch == '%' && !ignore_illegals {
            return Err(IllegalMatch);
        }
        if ch == '(' {
            let Some((inner_start, inner_end, after)) = paren_content(text, p) else {
                writer.plain(&text[p..]);
                return Ok(text.len());
            };
            writer.plain_char('(');
            // hljs 的 PARAMS 是 excludeBegin/End 的独立模式：空参数列表也会产生一个
            // 零长度的 params span（渲染器据此在 `(`/`)` 之间 flush）。
            writer.open("params");
            writer.relevance += 1;
            if inner_start < inner_end {
                scan_inline(lang, &text[inner_start..inner_end], writer, ignore_illegals)?;
            }
            writer.close();
            writer.plain_char(')');
            p = after;
            continue;
        }
        if let Some(end) = re_at(title_ident_regex(), text, p) {
            writer.span("title", &text[p..end]);
            p = end;
            continue;
        }
        writer.plain_char(ch);
        p += ch.len_utf8();
    }
    Ok(p)
}

/// javascript/typescript 的「method」模式：
/// `begin: UNDERSCORE_IDENT_RE + '\(' + 配对括号 + '\)\s*\{'`，returnBegin，
/// className `function`，contains `[PARAMS, TITLE_MODE]`。模式没有 end（隐式
/// `\B|\b`），因此 function span 在 `)` 之后立即结束，`\s*{` 留在外面。
///
/// 触发词必须排在 hljs contains 里 method 之前的模式之外：`function`（function 模式）、
/// `while if switch catch for`（beginKeywords 阻止模式）与 value container 的
/// `return throw case`。
fn try_js_method(
    lang: &CompiledLanguage,
    text: &str,
    start: usize,
    ident_end: usize,
    writer: &mut Writer,
    ignore_illegals: bool,
) -> Result<Option<usize>, IllegalMatch> {
    if text.as_bytes().get(ident_end) != Some(&b'(') {
        return Ok(None);
    }
    let Some((inner_start, inner_end, after)) = paren_content(text, ident_end) else {
        return Ok(None);
    };
    let brace = next_non_whitespace(text, after);
    if text.as_bytes().get(brace) != Some(&b'{') {
        return Ok(None);
    }
    writer.open("function");
    writer.relevance += 1;
    writer.span("title", &text[start..ident_end]);
    writer.plain_char('(');
    writer.open("params");
    writer.relevance += 1;
    if inner_start < inner_end {
        scan_inline(lang, &text[inner_start..inner_end], writer, ignore_illegals)?;
    }
    writer.close();
    writer.plain_char(')');
    writer.close();
    Ok(Some(after))
}

/// javascript/typescript 的 constructor 模式：
/// `begin: /\b(?=constructor)/`，end `[{;]` excludeEnd，contains
/// `[TITLE_MODE(IDENT_RE), 'self', PARAMS]`，无 className。
/// 任何以 `constructor` 开头的标识符（`constructorFoo` 也是）都会进入该模式。
fn scan_js_constructor(
    lang: &CompiledLanguage,
    text: &str,
    start: usize,
    ident_end: usize,
    writer: &mut Writer,
    ignore_illegals: bool,
) -> Result<usize, IllegalMatch> {
    writer.span("title", &text[start..ident_end]);
    let mut p = ident_end;
    while p < text.len() {
        let ch = text[p..].chars().next().expect("char boundary");
        if matches!(ch, '{' | ';') {
            return Ok(p);
        }
        if ch == '(' {
            let Some((inner_start, inner_end, after)) = paren_content(text, p) else {
                writer.plain(&text[p..]);
                return Ok(text.len());
            };
            writer.plain_char('(');
            writer.open("params");
            writer.relevance += 1;
            if inner_start < inner_end {
                scan_inline(lang, &text[inner_start..inner_end], writer, ignore_illegals)?;
            }
            writer.close();
            writer.plain_char(')');
            p = after;
            continue;
        }
        if let Some(end) = re_at(&lang.ident, text, p) {
            writer.span("title", &text[p..end]);
            p = end;
            continue;
        }
        writer.plain_char(ch);
        p += ch.len_utf8();
    }
    Ok(p)
}

/// javascript/typescript 的 getter/setter 模式：
/// `begin: '(get|set)\s+(?=IDENT_RE\()'`，end `/\{/`，keywords `get set`，
/// contains `[TITLE_MODE(IDENT_RE), {begin:/\(\)/}, PARAMS]`，无 className。
/// 返回 `Some(结束下标)` 时已经输出了 `get`/`set` 关键字及其后模式内容。
fn try_js_get_set(
    lang: &CompiledLanguage,
    text: &str,
    word: &str,
    ident_end: usize,
    writer: &mut Writer,
    ignore_illegals: bool,
) -> Result<Option<usize>, IllegalMatch> {
    if !matches!(word, "get" | "set") {
        return Ok(None);
    }
    let ws = next_non_whitespace(text, ident_end);
    if ws == ident_end {
        return Ok(None);
    }
    let Some(name_end) = re_at(title_ident_regex(), text, ws) else {
        return Ok(None);
    };
    if text.as_bytes().get(name_end) != Some(&b'(') {
        return Ok(None);
    }
    writer.span("keyword", word);
    writer.plain(&text[ident_end..ws]);
    let mut p = ws;
    while p < text.len() {
        let ch = text[p..].chars().next().expect("char boundary");
        if ch == '{' {
            return Ok(Some(p));
        }
        // `{ begin: /\(\)/ }`：空括号整体作为普通文本消费。
        if text[p..].starts_with("()") {
            writer.plain("()");
            p += 2;
            continue;
        }
        if ch == '(' {
            let Some((inner_start, inner_end, after)) = paren_content(text, p) else {
                writer.plain(&text[p..]);
                return Ok(Some(text.len()));
            };
            writer.plain_char('(');
            writer.open("params");
            writer.relevance += 1;
            if inner_start < inner_end {
                scan_inline(lang, &text[inner_start..inner_end], writer, ignore_illegals)?;
            }
            writer.close();
            writer.plain_char(')');
            p = after;
            continue;
        }
        if let Some(end) = re_at(title_ident_regex(), text, p) {
            writer.span("title", &text[p..end]);
            p = end;
            continue;
        }
        writer.plain_char(ch);
        p += ch.len_utf8();
    }
    Ok(Some(p))
}

/// kotlin.js 的 `fun` 模式（beginKeywords: 'fun'，end `[(]|$`，excludeEnd，
/// returnBegin）：关键字之后、参数列表之前的空白、泛型 `<...>`（`type`，`reified`
/// 是 keyword）与标题（`IDENT\s*\(` 的 lookahead）都属于同一个 function span；
/// 参数列表 `(...)` 含括号且 `endsParent`，function span 在 `)` 后立即结束。
fn scan_kotlin_function(
    lang: &CompiledLanguage,
    text: &str,
    mut p: usize,
    writer: &mut Writer,
    ignore_illegals: bool,
) -> Result<usize, IllegalMatch> {
    loop {
        let ws_end = next_non_whitespace(text, p);
        writer.plain(&text[p..ws_end]);
        p = ws_end;
        if p >= text.len() {
            return Ok(p);
        }
        let ch = text[p..].chars().next().expect("char boundary");
        if ch == '<' {
            // `{ className: 'type', begin: /</, end: />/, keywords: 'reified' }`
            let Some(offset) = text[p + 1..].find('>') else {
                writer.plain(&text[p..]);
                return Ok(text.len());
            };
            let close = p + 1 + offset;
            writer.open("type");
            writer.relevance += 1;
            writer.plain_char('<');
            let inner = &text[p + 1..close];
            let mut q = 0usize;
            while q < inner.len() {
                if let Some(end) = re_at(&lang.ident, inner, q) {
                    let word = &inner[q..end];
                    if word == "reified" {
                        writer.span("keyword", word);
                    } else {
                        writer.plain(word);
                    }
                    q = end;
                    continue;
                }
                let c = inner[q..].chars().next().expect("char boundary");
                writer.plain_char(c);
                q += c.len_utf8();
            }
            writer.plain_char('>');
            writer.close();
            p = close + 1;
            continue;
        }
        if ch == '(' {
            let Some((inner_start, inner_end, after)) = paren_content(text, p) else {
                writer.plain(&text[p..]);
                return Ok(text.len());
            };
            writer.open("params");
            writer.relevance += 1;
            writer.plain_char('(');
            if inner_start < inner_end {
                scan_kotlin_params(lang, text, inner_start, inner_end, writer, ignore_illegals)?;
            }
            writer.plain_char(')');
            writer.close();
            // `endsParent`：参数列表结束后 function 立即结束。
            return Ok(after);
        }
        // 标题：`UNDERSCORE_IDENT_RE\s*\(`（returnBegin，只把标识符着成 title）。
        if let Some(end) = re_at(&lang.ident, text, p)
            .filter(|&end| text[next_non_whitespace(text, end)..].starts_with('('))
        {
            writer.span("title", &text[p..end]);
            p = end;
            continue;
        }
        // 其余文本按 function 模式的关键字表着色。
        if let Some(end) = re_at(&lang.ident, text, p) {
            let word = &text[p..end];
            if let Some(class) = lang.keywords.get(word) {
                writer.span(class, word);
            } else {
                writer.plain(word);
            }
            p = end;
            continue;
        }
        writer.plain_char(ch);
        p += ch.len_utf8();
    }
}

/// kotlin.js 的 `params` 模式内部：`:` 之后是参数类型上下文（直到 `=`/`,`/`/`
/// 或参数列表结束），其中标识符（`KOTLIN_PAREN_TYPE`）是 `type`，括号递归。
fn scan_kotlin_params(
    lang: &CompiledLanguage,
    text: &str,
    start: usize,
    end: usize,
    writer: &mut Writer,
    ignore_illegals: bool,
) -> Result<(), IllegalMatch> {
    let mut p = start;
    while p < end {
        let ch = text[p..].chars().next().expect("char boundary");
        if ch == ':' {
            writer.plain_char(':');
            p += 1;
            p = scan_kotlin_type(lang, text, p, end, writer);
            continue;
        }
        match try_string(lang, &text[..end], p, writer, ignore_illegals) {
            Some(Ok(after)) => {
                p = after;
                continue;
            }
            Some(Err(illegal)) => return Err(illegal),
            None => {}
        }
        if text[p..].starts_with("//") {
            let line = line_end(text, p).min(end);
            writer.span("comment", &text[p..line]);
            p = line;
            continue;
        }
        if text[p..].starts_with("/*") {
            let close = block_comment_end(text, p).min(end);
            writer.span("comment", &text[p..close]);
            p = close;
            continue;
        }
        if let Some(after) = re_at(&lang.ident, text, p)
            && after <= end
        {
            let word = &text[p..after];
            if let Some(class) = lang.keywords.get(word) {
                writer.span(class, word);
            } else {
                writer.plain(word);
            }
            p = after;
            continue;
        }
        if let Some(after) = lang.number.as_ref().and_then(|re| re_at(re, text, p))
            && after <= end
        {
            writer.span("number", &text[p..after]);
            p = after;
            continue;
        }
        writer.plain_char(ch);
        p += ch.len_utf8();
    }
    Ok(())
}

/// kotlin 参数类型上下文：`KOTLIN_PAREN_TYPE`（标识符 → `type`，括号递归）。
/// 结束于 `=`、`,`、`/`（`:` 模式声明的 end）或参数列表结尾的 `)`。
fn scan_kotlin_type(
    lang: &CompiledLanguage,
    text: &str,
    start: usize,
    end: usize,
    writer: &mut Writer,
) -> usize {
    let mut p = start;
    let mut depth = 0usize;
    while p < end {
        let ch = text[p..].chars().next().expect("char boundary");
        if depth == 0 && matches!(ch, '=' | ',' | '/' | ')') {
            return p;
        }
        if ch == '(' {
            depth += 1;
            writer.plain_char(ch);
            p += 1;
            continue;
        }
        if ch == ')' {
            if depth == 0 {
                return p;
            }
            depth -= 1;
            writer.plain_char(ch);
            p += 1;
            continue;
        }
        if let Some(after) = re_at(&lang.ident, &text[..end], p)
            && after <= end
        {
            writer.span("type", &text[p..after]);
            p = after;
            continue;
        }
        writer.plain_char(ch);
        p += ch.len_utf8();
    }
    p
}

/// javascript/typescript 的单参数箭头函数（hljs 的 `className: 'function'` 模式，
/// begin/end 都是 `\s*=>`，returnBegin）：标识符本身是 `params`。
fn try_js_arrow_ident(
    text: &str,
    start: usize,
    ident_end: usize,
    writer: &mut Writer,
) -> Option<usize> {
    let arrow = next_non_whitespace(text, ident_end);
    if !text[arrow..].starts_with("=>") || !js_arrow_allowed(text, start) {
        return None;
    }
    writer.open("function");
    writer.relevance += 1;
    writer.open("params");
    writer.plain(&text[start..ident_end]);
    writer.close();
    writer.plain(&text[ident_end..arrow]);
    writer.plain("=>");
    writer.close();
    Some(arrow + 2)
}

/// javascript/typescript 的括号参数箭头函数：`(...)\s*=>`（returnBegin；end `\s*=>`
/// 属于 function span）。括号本身是 function 文本，内部是 `params`（空/纯空白时 hljs
/// 的 `\(\s*\)` 变体 `skip: true`，不产生 params span）。
fn try_js_arrow_paren(
    lang: &CompiledLanguage,
    text: &str,
    index: usize,
    writer: &mut Writer,
    ignore_illegals: bool,
) -> Result<Option<usize>, IllegalMatch> {
    let Some((inner_start, inner_end, after)) = paren_content(text, index) else {
        return Ok(None);
    };
    let arrow = next_non_whitespace(text, after);
    if !text[arrow..].starts_with("=>") || !js_arrow_allowed(text, index) {
        return Ok(None);
    }
    writer.open("function");
    writer.relevance += 1;
    writer.plain_char('(');
    let all_whitespace = text[inner_start..inner_end]
        .chars()
        .all(|ch| ch.is_whitespace());
    if inner_start < inner_end && !all_whitespace {
        writer.open("params");
        writer.relevance += 1;
        scan_inline(lang, &text[inner_start..inner_end], writer, ignore_illegals)?;
        writer.close();
    }
    writer.plain_char(')');
    writer.plain(&text[after..arrow]);
    writer.plain("=>");
    writer.close();
    Ok(Some(arrow + 2))
}

/// javascript/typescript 的 "object attr container"（对象字面量键）。
///
/// hljs 的 begin 是 `[{,\n]\s*` + lookahead（可选注释 + `IDENT\s*:`），attr 子模式只
/// 覆盖标识符本身。这里按“标识符后（可跨空白）是 `:`，向前跳过空白后是 `{`、`,` 或
/// 换行”的近似判定（`IDENT_RE` 是 `[a-zA-Z]\w*`）。
fn js_object_attr_matches(text: &str, start: usize, ident_end: usize, word: &str) -> bool {
    let mut chars = word.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !first.is_ascii_alphabetic()
        || !word
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
    {
        return false;
    }
    let colon = next_non_whitespace(text, ident_end);
    if text.as_bytes().get(colon) != Some(&b':') {
        return false;
    }
    let mut i = start;
    let mut crossed_newline = false;
    while i > 0 {
        let Some(ch) = text[..i].chars().next_back() else {
            break;
        };
        if ch == '\n' {
            crossed_newline = true;
            i -= ch.len_utf8();
            continue;
        }
        if ch.is_whitespace() {
            i -= ch.len_utf8();
            continue;
        }
        // 前一个 token 是单词（标识符/数字/关键字）时，它后面的空白没有被任何模式吞掉。
        if ch.is_ascii_alphanumeric() || ch == '_' || ch == '$' {
            let mut word_start = i;
            while word_start > 0 {
                let Some(prev) = text[..word_start].chars().next_back() else {
                    break;
                };
                if prev.is_ascii_alphanumeric() || prev == '_' || prev == '$' {
                    word_start -= prev.len_utf8();
                } else {
                    break;
                }
            }
            // `return`/`throw`/`case` 的 value container begin 会连带 `\s*` 吞掉换行。
            if crossed_newline {
                return !matches!(&text[word_start..i], "return" | "throw" | "case");
            }
            return false;
        }
        if crossed_newline {
            // value container 的 begin（starter + `\s*`）会吞掉后面的空白（含换行），
            // 此时换行不再是 attr container 的锚点；`{`/`,` 自身仍是锚点。
            return ch == '{' || ch == ',' || !is_js_value_starter(ch);
        }
        return ch == '{' || ch == ',';
    }
    false
}

/// javascript.js 的 `RE_STARTERS_RE` 里的单字符（value container 的 begin 首字符）。
fn is_js_value_starter(ch: char) -> bool {
    matches!(
        ch,
        '!' | '%'
            | '&'
            | '*'
            | '+'
            | ','
            | '-'
            | '/'
            | ':'
            | ';'
            | '<'
            | '='
            | '>'
            | '?'
            | '['
            | '{'
            | '('
            | '^'
            | '|'
            | '~'
    )
}

/// 判断 hljs 的 value container 在该箭头函数之前是否仍然有效。
///
/// javascript.js 的 value container 从 `RE_STARTERS_RE` 的字符（或 `return`/`throw`/
/// `case`）开始，之后只有容器的子模式（空白、逗号、注释、箭头函数、JSX、正则）能让它
/// 继续；遇到 `async`/`await` 这类普通关键字时容器结束，后续箭头函数不再着色
/// （`const f = async (x) => x` 在 hljs 里没有 function span）。这里用向前看一个非空白
/// token 的近似：前一个字符是 starter，或前一个词是 return/throw/case。
fn js_arrow_allowed(text: &str, start: usize) -> bool {
    let mut i = start;
    while i > 0 {
        let Some(ch) = text[..i].chars().next_back() else {
            break;
        };
        if ch.is_whitespace() {
            i -= ch.len_utf8();
            continue;
        }
        if ch.is_ascii_alphanumeric() || ch == '_' || ch == '$' {
            let mut word_start = i;
            while word_start > 0 {
                let Some(prev) = text[..word_start].chars().next_back() else {
                    break;
                };
                if prev.is_ascii_alphanumeric() || prev == '_' || prev == '$' {
                    word_start -= prev.len_utf8();
                } else {
                    break;
                }
            }
            return matches!(&text[word_start..i], "return" | "throw" | "case");
        }
        return is_js_value_starter(ch);
    }
    false
}

/// swift.js 的 FUNCTION 模式在标题之后的部分：空白（属于 function span）、泛型参数
/// `<...>`（尖括号属于 function span，内部按语言规则扫描）与参数列表 `(...)`
/// （`endsParent`：参数列表结束后 function 立即结束）。没有参数列表时，函数在标题后的
/// 空白之后结束。
fn scan_swift_function_tail(
    lang: &CompiledLanguage,
    text: &str,
    mut p: usize,
    writer: &mut Writer,
    ignore_illegals: bool,
) -> Result<usize, IllegalMatch> {
    loop {
        // WHITESPACE 子模式（`match: /\s+/`）：function span 内的空白（含换行）。
        let whitespace_end = next_non_whitespace(text, p);
        writer.plain(&text[p..whitespace_end]);
        p = whitespace_end;
        if p >= text.len() {
            return Ok(p);
        }
        if text.as_bytes()[p] == b'<' {
            // GENERIC_PARAMETERS：`<`/`>` 是 function 文本，内部按语言规则扫描。
            match text[p + 1..].find('>') {
                Some(offset) => {
                    let close = p + 1 + offset;
                    writer.plain_char('<');
                    scan_inline(lang, &text[p + 1..close], writer, ignore_illegals)?;
                    writer.plain_char('>');
                    p = close + 1;
                }
                None => {
                    writer.plain_char('<');
                    scan_inline(lang, &text[p + 1..], writer, ignore_illegals)?;
                    return Ok(text.len());
                }
            }
            continue;
        }
        if text.as_bytes()[p] == b'(' {
            // FUNCTION_PARAMETERS：括号属于 function span，`endsParent` 让它到此结束。
            let Some((inner_start, inner_end, after)) = paren_content(text, p) else {
                writer.plain(&text[p..]);
                return Ok(text.len());
            };
            writer.plain_char('(');
            scan_swift_params(lang, text, inner_start, inner_end, writer, ignore_illegals)?;
            writer.plain_char(')');
            return Ok(after);
        }
        return Ok(p);
    }
}

/// swift.js 的 FUNCTION_PARAMETER_NAME：`ident\s*:` 或 `ident\s+ident\s*:` 的 lookahead
/// 之后，模式在 `:` 结束。返回冒号下标（名字区间是 `start..colon`）。
fn swift_param_name_colon(text: &str, start: usize, end: usize) -> Option<usize> {
    let first_end = re_at(swift_param_ident_regex(), text, start)?;
    if first_end > end {
        return None;
    }
    let first_ws = skip_ws_within(text, first_end, end);
    if first_ws < end && text.as_bytes().get(first_ws) == Some(&b':') {
        return Some(first_ws);
    }
    if first_ws == first_end {
        return None;
    }
    let second_end = re_at(swift_param_ident_regex(), text, first_ws)?;
    if second_end > end {
        return None;
    }
    let second_ws = skip_ws_within(text, second_end, end);
    if second_ws < end && text.as_bytes().get(second_ws) == Some(&b':') {
        return Some(second_ws);
    }
    None
}

fn skip_ws_within(text: &str, mut p: usize, end: usize) -> usize {
    while p < end {
        let ch = text[p..].chars().next().expect("char boundary");
        if !ch.is_whitespace() {
            break;
        }
        p += ch.len_utf8();
    }
    p
}

/// swift 参数列表内部：参数名（`params`，`_` 是 `keyword`）之外的部分用 [`scan_inline`]
/// 处理（类型、数字、字符串…）。字符串与注释区间不做名字检测。
fn scan_swift_params(
    lang: &CompiledLanguage,
    text: &str,
    start: usize,
    end: usize,
    writer: &mut Writer,
    ignore_illegals: bool,
) -> Result<(), IllegalMatch> {
    let mut segment_start = start;
    let mut p = start;
    while p < end {
        if let Some(after) = skip_swift_string(text, p) {
            p = after.min(end);
            continue;
        }
        if text[p..].starts_with("//") {
            p = line_end(text, p).min(end);
            continue;
        }
        if text[p..].starts_with("/*") {
            p = skip_block_comment(text, p).min(end);
            continue;
        }
        if let Some(colon) = swift_param_name_colon(text, p, end) {
            if segment_start < p {
                scan_inline(lang, &text[segment_start..p], writer, ignore_illegals)?;
            }
            let mut q = p;
            while q < colon {
                if let Some(name_end) = re_at(swift_param_ident_regex(), text, q) {
                    let word = &text[q..name_end];
                    if word == "_" {
                        writer.span("keyword", word);
                    } else {
                        writer.span("params", word);
                    }
                    q = name_end;
                    continue;
                }
                let ch = text[q..].chars().next().expect("char boundary");
                writer.plain_char(ch);
                q += ch.len_utf8();
            }
            // 名字模式的 end `:` 没有 className，冒号是 function span 内的文本。
            writer.plain_char(':');
            p = colon + 1;
            segment_start = p;
            continue;
        }
        p += char_len(text, p);
    }
    if segment_start < end {
        scan_inline(lang, &text[segment_start..end], writer, ignore_illegals)?;
    }
    Ok(())
}

/// swift 单行/三引号字符串的结束下标（够用于参数列表里的默认值；不做插值解析）。
fn skip_swift_string(text: &str, start: usize) -> Option<usize> {
    if text[start..].starts_with("\"\"\"") {
        let mut p = start + 3;
        while p < text.len() {
            if text[p..].starts_with("\"\"\"") {
                return Some(p + 3);
            }
            if text.as_bytes()[p] == b'\\' {
                p += 2;
            } else {
                p += char_len(text, p);
            }
        }
        return Some(text.len());
    }
    if text.as_bytes().get(start) == Some(&b'"') {
        let mut p = start + 1;
        while p < text.len() {
            match text.as_bytes()[p] {
                b'\\' => p += 2,
                b'"' => return Some(p + 1),
                _ => p += char_len(text, p),
            }
        }
        return Some(text.len());
    }
    None
}

/// swift 的块注释可以嵌套。
fn skip_block_comment(text: &str, start: usize) -> usize {
    let mut depth = 1usize;
    let mut p = start + 2;
    while p < text.len() {
        if text[p..].starts_with("/*") {
            depth += 1;
            p += 2;
            continue;
        }
        if text[p..].starts_with("*/") {
            depth -= 1;
            p += 2;
            if depth == 0 {
                return p;
            }
            continue;
        }
        p += char_len(text, p);
    }
    text.len()
}

/// swift.js 的参数名标识符（`identifier` 的 ASCII 近似，与模块内其余 swift 正则一致）。
fn swift_param_ident_regex() -> &'static AnchoredRegex {
    static RE: OnceLock<AnchoredRegex> = OnceLock::new();
    RE.get_or_init(|| AnchoredRegex::new(r"[A-Za-z_][A-Za-z0-9_]*"))
}

/// highlight.js 的 CLASS / NAMESPACE 模式：`class` 关键字之后的头部扫描。
///
/// 头部里的标识符按 TITLE_MODE 着色（`title_all` 为 false 时只有第一个），关键字按语言
/// 关键字表着色；`end_chars` 里的字符结束模式（`include_end` 决定结束字符是否进入 span，
/// 换行永远是零宽结束）。
fn scan_class_body(
    lang: &CompiledLanguage,
    text: &str,
    index: usize,
    ident_end: usize,
    def: &ClassBodyDef,
    writer: &mut Writer,
    _ignore_illegals: bool,
) -> Result<usize, IllegalMatch> {
    if !def.class.is_empty() {
        writer.open(def.class);
    }
    writer.relevance += 1;
    writer.span("keyword", &text[index..ident_end]);
    let mut p = ident_end;
    let mut titled = false;
    while p < text.len() {
        let ch = text[p..].chars().next().expect("char boundary");
        if def.ends_after_title && titled {
            break;
        }
        // kotlin.js 的 supertype 类型模式：`begin: /[,:]\s*/, end: /[<\(,]|$/,
        // excludeBegin, returnEnd`。它排在 CLASS 自身的 end（`[:\{(]|$`）之前，
        // 所以在 `:`/`,` 处优先进类型模式：`,` 后的内容直到 `<`/`(`/`,`/EOF 是 `type`。
        if lang.name == "kotlin" && matches!(ch, ':' | ',') {
            writer.plain_char(ch);
            p += ch.len_utf8();
            let ws = next_non_whitespace(text, p);
            writer.plain(&text[p..ws]);
            p = ws;
            let start = p;
            let mut q = p;
            while q < text.len() {
                let c = text[q..].chars().next().expect("char boundary");
                if matches!(c, '<' | '(' | ',') {
                    break;
                }
                q += c.len_utf8();
            }
            if start < q {
                writer.span("type", &text[start..q]);
            }
            p = q;
            continue;
        }
        if def.end_chars.contains(ch) {
            // `$|;`（ruby）与 `$`（kotlin）里的行尾是零宽断言：换行本身不进入 span。
            if ch != '\n' && def.include_end {
                writer.plain_char(ch);
                p += ch.len_utf8();
            }
            break;
        }
        // kotlin：`<...>` 的尖括号被 excludeBegin/End，内部是 `type` span。
        if def.generics_type && ch == '<' {
            writer.plain_char('<');
            p += 1;
            let mut q = p;
            while q < text.len() && text.as_bytes()[q] != b'>' {
                q += char_len(text, q);
            }
            if p < q {
                writer.span("type", &text[p..q]);
            }
            if q < text.len() {
                writer.plain_char('>');
                p = q + 1;
            } else {
                p = q;
            }
            continue;
        }
        // scala：`(...)` 是 `params` span（括号本身在外），内部大写开头的是 `type`。
        if def.params
            && ch == '('
            && let Some((inner_start, inner_end, after)) = paren_content(text, p)
        {
            writer.plain_char('(');
            writer.open("params");
            writer.relevance += 1;
            scan_scala_params(text, inner_start, inner_end, writer);
            writer.close();
            writer.plain_char(')');
            p = after;
            continue;
        }
        // csharp.js 的 TITLE_MODE 覆盖为 `[a-zA-Z](\.?\w)*`（`System.Int32` 是一个 title），
        // 其余语言继续用语言的 ident 模式。
        let ident_re = if lang.name == "csharp" {
            csharp_title_regex()
        } else {
            &lang.ident
        };
        if let Some(end) = re_at(ident_re, text, p) {
            let word = &text[p..end];
            if def.extra_keywords.contains(&word) {
                writer.span("keyword", word);
            } else if def.keyword_table
                && let Some(class) = lang.keywords.get(word)
            {
                writer.span(class, word);
            } else if def.title_all || !titled {
                writer.span("title", word);
                titled = true;
            } else {
                writer.plain(word);
            }
            p = end;
            continue;
        }
        writer.plain_char(ch);
        p += ch.len_utf8();
    }
    if !def.class.is_empty() {
        writer.close();
    }
    Ok(p)
}

/// java.js 的 `record` 模式（10.7.3）：
///
/// ```js
/// { className: 'class', begin: 'record\\s+' + UNDERSCORE_IDENT_RE + '\\s*\\(',
///   returnBegin: true, excludeEnd: true, end: /[{;=]/, keywords: KEYWORDS,
///   contains: [ {beginKeywords: "record"},
///               {begin: UNDERSCORE_IDENT_RE + '\\s*\\(', returnBegin: true, contains: [TITLE_MODE]},
///               {className: 'params', begin: /\(/, end: /\)/, keywords: KEYWORDS,
///                relevance: 0, contains: [C_BLOCK_COMMENT_MODE]},
///               C_LINE_COMMENT_MODE, C_BLOCK_COMMENT_MODE ] }
/// ```
///
/// 模式只在完整匹配 `record\s+标识符\s*\(` 时生效（否则返回 `None`，落回普通关键字/
/// 函数推断处理，例如 `record R;` 在 hljs 里是纯文本）。头部到 `{`/`;`/`=` 为止
/// （结束字符不在 span 内），params span **含**括号，其内部只认块注释与关键字表。
fn scan_java_record(
    lang: &CompiledLanguage,
    text: &str,
    index: usize,
    ident_end: usize,
    writer: &mut Writer,
) -> Option<usize> {
    let title_start = next_non_whitespace(text, ident_end);
    if title_start == ident_end {
        return None;
    }
    let title_end = re_at(java_record_title_regex(), text, title_start)?;
    let paren = next_non_whitespace(text, title_end);
    if text.as_bytes().get(paren) != Some(&b'(') {
        return None;
    }
    writer.open("class");
    writer.relevance += 1;
    writer.span("keyword", &text[index..ident_end]);
    writer.plain(&text[ident_end..title_start]);
    writer.span("title", &text[title_start..title_end]);
    writer.plain(&text[title_end..paren]);
    // params span（含括号）。
    writer.open("params");
    writer.relevance += 1;
    writer.plain_char('(');
    let mut p = paren + 1;
    while p < text.len() {
        if text[p..].starts_with("/*") {
            let end = block_comment_end(text, p);
            writer.span("comment", &text[p..end]);
            p = end;
            continue;
        }
        if text.as_bytes()[p] == b')' {
            writer.plain_char(')');
            p += 1;
            break;
        }
        if let Some(end) = re_at(&lang.ident, text, p) {
            let word = &text[p..end];
            if let Some(class) = lang.keywords.get(word) {
                writer.span(class, word);
            } else {
                writer.plain(word);
            }
            p = end;
            continue;
        }
        let ch = text[p..].chars().next().expect("char boundary");
        writer.plain_char(ch);
        p += ch.len_utf8();
    }
    writer.close();
    // 头部剩余部分，直到 `{`/`;`/`=`（excludeEnd）；行注释、块注释与关键字表。
    while p < text.len() {
        let ch = text[p..].chars().next().expect("char boundary");
        if matches!(ch, '{' | ';' | '=') {
            break;
        }
        if text[p..].starts_with("//") {
            let end = line_end(text, p);
            writer.span("comment", &text[p..end]);
            p = end;
            continue;
        }
        if text[p..].starts_with("/*") {
            let end = block_comment_end(text, p);
            writer.span("comment", &text[p..end]);
            p = end;
            continue;
        }
        if let Some(end) = re_at(&lang.ident, text, p) {
            let word = &text[p..end];
            if let Some(class) = lang.keywords.get(word) {
                writer.span(class, word);
            } else {
                writer.plain(word);
            }
            p = end;
            continue;
        }
        writer.plain_char(ch);
        p += ch.len_utf8();
    }
    writer.close();
    Some(p)
}

/// 从 `/*` 开始找到块注释的结束下标（未闭合时到字符串末尾，与 hljs 的 `$` 结尾一致）。
fn block_comment_end(text: &str, start: usize) -> usize {
    match text[start + 2..].find("*/") {
        Some(offset) => start + 2 + offset + 2,
        None => text.len(),
    }
}

/// java.js 的 `UNDERSCORE_IDENT_RE`（`[a-zA-Z_]\w*`，JS 的 `\w` 是 ASCII）。
fn java_record_title_regex() -> &'static AnchoredRegex {
    static RE: OnceLock<AnchoredRegex> = OnceLock::new();
    RE.get_or_init(|| AnchoredRegex::new(r"[a-zA-Z_][A-Za-z0-9_]*"))
}

/// csharp.js 的 `TITLE_MODE`：`[a-zA-Z](\.?\w)*`（带点的限定名是一个 title）。
fn csharp_title_regex() -> &'static AnchoredRegex {
    static RE: OnceLock<AnchoredRegex> = OnceLock::new();
    RE.get_or_init(|| AnchoredRegex::new(r"[a-zA-Z](?:\.?[A-Za-z0-9_])*"))
}

/// scala CLASS 模式里 `(...)` 的内部：只有 `TYPE`（大写开头的标识符）产生 span，
/// 关键字按语言表着色，其余原样。
fn scan_scala_params(text: &str, start: usize, end: usize, writer: &mut Writer) {
    static UPPER_IDENT: std::sync::OnceLock<AnchoredRegex> = std::sync::OnceLock::new();
    static PLAIN_IDENT: std::sync::OnceLock<AnchoredRegex> = std::sync::OnceLock::new();
    let upper = UPPER_IDENT.get_or_init(|| AnchoredRegex::new(r"\b[A-Z][A-Za-z0-9_]*"));
    let plain = PLAIN_IDENT.get_or_init(|| AnchoredRegex::new(r"[A-Za-z_$][A-Za-z0-9_$]*"));
    let mut p = start;
    while p < end {
        let slice = &text[..end];
        if let Some(q) = re_at(upper, slice, p)
            && q <= end
        {
            writer.span("type", &text[p..q]);
            p = q;
            continue;
        }
        if let Some(q) = re_at(plain, slice, p)
            && q <= end
        {
            writer.plain(&text[p..q]);
            p = q;
            continue;
        }
        let ch = text[p..].chars().next().expect("char boundary");
        writer.plain_char(ch);
        p += ch.len_utf8();
    }
}

/// bash 的算术上下文 `$(( ... ))`（ARITHMETIC 模式没有 className）：
/// 内部按 radix number / NUMBER_MODE / VAR 着色。
fn try_bash_arithmetic(text: &str, index: usize, writer: &mut Writer) -> Option<usize> {
    if !text[index..].starts_with("$((") {
        return None;
    }
    let inner_start = index + 3;
    let close = text[inner_start..].find("))").map(|o| inner_start + o)?;
    static RADIX: OnceLock<AnchoredRegex> = OnceLock::new();
    static NUMBER: OnceLock<AnchoredRegex> = OnceLock::new();
    static VAR_BRACED: OnceLock<AnchoredRegex> = OnceLock::new();
    static VAR: OnceLock<AnchoredRegex> = OnceLock::new();
    let radix = RADIX.get_or_init(|| AnchoredRegex::new(r"\d+#[0-9a-f]+"));
    let number = NUMBER.get_or_init(|| AnchoredRegex::new(r"\b\d+(?:\.\d+)?"));
    let var_braced = VAR_BRACED.get_or_init(|| AnchoredRegex::new(r"\$\{[^}\n]*\}"));
    let var = VAR.get_or_init(|| AnchoredRegex::new(r"\$[A-Za-z0-9_#@][A-Za-z0-9_]*"));
    writer.plain("$((");
    let mut p = inner_start;
    while p < close {
        if let Some(end) = re_at(radix, text, p)
            .filter(|&end| end <= close)
            .or_else(|| re_at(number, text, p).filter(|&end| end <= close))
        {
            writer.span("number", &text[p..end]);
            p = end;
            continue;
        }
        if let Some(end) = re_at(var_braced, text, p)
            .filter(|&end| end <= close)
            .or_else(|| re_at(var, text, p).filter(|&end| end <= close))
        {
            writer.span("variable", &text[p..end]);
            p = end;
            continue;
        }
        let ch = text[p..].chars().next().expect("char boundary");
        writer.plain_char(ch);
        p += ch.len_utf8();
    }
    writer.plain("))");
    Some(close + 2)
}

/// bash 的 `name() {` 函数定义。
fn try_bash_function(
    lang: &CompiledLanguage,
    text: &str,
    index: usize,
    writer: &mut Writer,
) -> Option<usize> {
    if lang.name != "bash" {
        return None;
    }
    let ident_end = re_at(&lang.ident, text, index)?;
    let mut p = next_non_space(text, ident_end);
    if !text[p..].starts_with("()") {
        return None;
    }
    p += 2;
    let brace = next_non_space(text, p);
    if text.as_bytes().get(brace) != Some(&b'{') {
        return None;
    }
    writer.open("function");
    writer.relevance += 1;
    writer.span("title", &text[index..ident_end]);
    writer.close();
    Some(ident_end)
}

/// javascript 的 `/re/flags` 正则字面量。
fn try_js_regexp(text: &str, index: usize) -> Option<usize> {
    let bytes = text.as_bytes();
    let mut p = index + 1;
    let mut in_class = false;
    while p < bytes.len() {
        match bytes[p] {
            b'\n' => return None,
            b'\\' => p += 2,
            b'[' if !in_class => {
                in_class = true;
                p += 1;
            }
            b']' if in_class => {
                in_class = false;
                p += 1;
            }
            b'/' if !in_class => {
                p += 1;
                while p < bytes.len() && bytes[p].is_ascii_alphabetic() {
                    p += 1;
                }
                return Some(p);
            }
            _ => p += 1,
        }
    }
    None
}

/// perl.js 的 regexp 容器触发条件：`(\/\/|RE_STARTERS_RE|\b(split|return|print|
/// reverse|grep)\b)\s*`。向前看一个非空白 token：RE_STARTERS 的任一字符，或
/// split/return/print/reverse/grep 关键字，或串首。
fn perl_regexp_allowed(text: &str, index: usize) -> bool {
    let mut i = index;
    while i > 0 {
        let Some(ch) = text[..i].chars().next_back() else {
            break;
        };
        if ch.is_whitespace() {
            i -= ch.len_utf8();
            continue;
        }
        if "!%&*+,-/:;<=>?^|~([{".contains(ch) {
            return true;
        }
        if ch.is_alphanumeric() || ch == '_' {
            let mut start = i;
            while start > 0 {
                let Some(prev) = text[..start].chars().next_back() else {
                    break;
                };
                if prev.is_alphanumeric() || prev == '_' {
                    start -= prev.len_utf8();
                } else {
                    break;
                }
            }
            return matches!(
                &text[start..i],
                "split" | "return" | "print" | "reverse" | "grep"
            );
        }
        return false;
    }
    true
}

/// perl.js 的替换正则：`(?:s|tr|y)` + 定界符 + body + 定界符(+open) + body + 定界符
/// 再加修饰符（`PAIRED_DOUBLE_RE`）。同一字符定界符（`s/foo/bar/g`）使用三次同一个
/// 定界符；配对定界符（`s{foo}{bar}`）在中间连接 `}{`。
fn try_perl_substitution(text: &str, index: usize) -> Option<usize> {
    let bytes = text.as_bytes();
    let prefix = if text[index..].starts_with("tr") {
        2
    } else if matches!(bytes.get(index), Some(b's') | Some(b'y')) {
        1
    } else {
        return None;
    };
    let p = index + prefix;
    let open = *bytes.get(p)?;
    let (close, middle_char) = match open {
        b'!' | b'/' | b'|' | b'?' | b'\'' | b'"' | b'#' => (open, Some(open)),
        b'(' => (b')', None),
        b'[' => (b']', None),
        b'{' => (b'}', None),
        _ => return None,
    };
    let body = |start: usize, stop: &[u8]| -> Option<usize> {
        let mut q = start;
        while q < bytes.len() {
            let c = bytes[q];
            if c == b'\\' {
                if q + 1 >= bytes.len() {
                    return None;
                }
                q += 2;
                continue;
            }
            if bytes[q..].starts_with(stop) {
                return Some(q + stop.len());
            }
            if c == b'/' {
                return None;
            }
            q += 1;
        }
        None
    };
    let (first_stop, second_stop): (Vec<u8>, Vec<u8>) = match middle_char {
        Some(c) => (vec![c], vec![c]),
        None => (vec![close, open], vec![close]),
    };
    let after_first = body(p + 1, &first_stop)?;
    let close_end = body(after_first, &second_stop)?;
    let mut q = close_end;
    let mut flags = 0;
    while q < bytes.len() && flags < 12 && b"dualxmsipngr".contains(&bytes[q]) {
        q += 1;
        flags += 1;
    }
    Some(q)
}

/// perl.js 的 regexp 字面量：`(m|qr)?/.../modifiers`，以及 `m|qr` + 配对/单字符定界符
/// （`{}`/`()`/`[]`/`!`/`|`/`?`/`'`/`"`/`#`）。中间部分与 hljs 的
/// `(?:\\.|[^\\\/])*?` 一致：转义对跳过，未转义的 `/` 让匹配失败。
/// 返回整个字面量（含前缀、定界符与修饰符）的结束下标。
fn try_perl_regexp(text: &str, index: usize) -> Option<usize> {
    if !perl_regexp_allowed(text, index) {
        return None;
    }
    // 替换正则（`s/foo/bar/g`、`tr/a/b/`、`y/a/b/`）是容器里的第一个 regexp 变体。
    if let Some(end) = try_perl_substitution(text, index) {
        return Some(end);
    }
    let bytes = text.as_bytes();
    let (p, has_prefix) = if text[index..].starts_with("qr") {
        (index + 2, true)
    } else if bytes.get(index) == Some(&b'm') {
        (index + 1, true)
    } else {
        (index, false)
    };
    let delim = *bytes.get(p)?;
    let close = match delim {
        b'/' => b'/',
        b'(' if has_prefix => b')',
        b'[' if has_prefix => b']',
        b'{' if has_prefix => b'}',
        b'!' | b'|' | b'?' | b'\'' | b'"' | b'#' if has_prefix => delim,
        _ => return None,
    };
    let mut q = p + 1;
    while q < bytes.len() {
        let c = bytes[q];
        if c == b'\\' {
            if q + 1 >= bytes.len() {
                return None;
            }
            q += 2;
            continue;
        }
        if c == close {
            q += 1;
            // REGEX_MODIFIERS = /[dualxmsipngr]{0,12}/
            let mut flags = 0;
            while q < bytes.len() && flags < 12 && b"dualxmsipngr".contains(&bytes[q]) {
                q += 1;
                flags += 1;
            }
            return Some(q);
        }
        if c == b'/' {
            return None;
        }
        q += 1;
    }
    None
}

/// `/` 前是否允许正则字面量（简化版 javascript RE_STARTERS_RE）。
fn regexp_allowed(text: &str, index: usize) -> bool {
    let mut prefix = text[..index]
        .chars()
        .rev()
        .skip_while(|c| c.is_whitespace());
    match prefix.next() {
        None => true,
        Some(ch) => {
            if "=(:,[!&|?{};+-*%^<>~".contains(ch) {
                return true;
            }
            if ch.is_alphanumeric() || ch == '_' || ch == '$' {
                // 反向读取上一个标识符，检查是否是"正则前"关键字。
                let mut word = String::new();
                word.push(ch);
                for c in prefix {
                    if c.is_alphanumeric() || c == '_' || c == '$' {
                        word.insert(0, c);
                    } else {
                        break;
                    }
                }
                return matches!(
                    word.as_str(),
                    "return"
                        | "typeof"
                        | "instanceof"
                        | "in"
                        | "of"
                        | "new"
                        | "delete"
                        | "void"
                        | "throw"
                        | "case"
                        | "do"
                        | "else"
                        | "yield"
                        | "await"
                );
            }
            false
        }
    }
}

/// `parse_function_decl` 的拒绝词：序列以这些关键字开头时不视为
/// `返回类型 名字(` 声明（避免把 `return foo(`、`new Foo(` 之类的表达式着色成函数）。
fn is_reject_keyword(word: &str) -> bool {
    matches!(
        word,
        "return"
            | "new"
            | "throw"
            | "else"
            | "if"
            | "while"
            | "for"
            | "switch"
            | "catch"
            | "do"
            | "try"
            | "case"
            | "sizeof"
            | "typeof"
            | "await"
            | "yield",
    )
}

/// 跳过类型参数（`<...>`）；不在 `<` 处时原样返回。
///
/// - csharp.js 的 `TYPE_IDENT_RE` / java.js 的 `GENERIC_IDENT_RE` 只允许
///   `<IDENT(\s*,\s*IDENT)*>`（IDENT 为 `[a-zA-Z]\w*`，**不允许嵌套尖括号**，
///   所以 `Task<List<int>>` 不构成函数声明的返回类型）；
/// - cpp.js 的 `TEMPLATE_ARGUMENT_RE` 是 `<[^<>]+>`（除尖括号外任意字符，至少一个）。
fn skip_type_angle(lang: &str, text: &str, start: usize) -> Option<usize> {
    if text.as_bytes().get(start) != Some(&b'<') {
        return Some(start);
    }
    if lang == "cpp" {
        let mut q = start + 1;
        while q < text.len() {
            match text.as_bytes()[q] {
                b'>' => return (q > start + 1).then_some(q + 1),
                b'<' => return None,
                _ => q += 1,
            }
        }
        return None;
    }
    let bytes = text.as_bytes();
    let mut q = start + 1;
    let mut expect_ident = true;
    loop {
        while q < bytes.len() && bytes[q].is_ascii_whitespace() {
            q += 1;
        }
        if q >= bytes.len() {
            return None;
        }
        if expect_ident {
            if !bytes[q].is_ascii_alphabetic() {
                return None;
            }
            q += 1;
            while q < bytes.len() && (bytes[q].is_ascii_alphanumeric() || bytes[q] == b'_') {
                q += 1;
            }
            expect_ident = false;
        } else if bytes[q] == b',' {
            q += 1;
            expect_ident = true;
        } else if bytes[q] == b'>' {
            return Some(q + 1);
        } else {
            return None;
        }
    }
}

/// c.js/cpp.js 的 `CPP_PRIMITIVE_TYPES`（`\b[a-z\d_]*_t\b`）：整词都是小写/数字/下划线
/// 且以 `_t` 结尾。
fn is_c_primitive_type(word: &str) -> bool {
    word.ends_with("_t")
        && word
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
}

/// 解析出的 C 系函数声明区间。
struct ParsedFunction {
    title_start: usize,
    title_end: usize,
    open: usize,
    inner_start: usize,
    inner_end: usize,
    after: usize,
}

/// C 系语言的 `返回类型 名字(` 推断，对应 highlight.js java/c/cpp/csharp 的
/// `function` mode（begin 为 `(IDENT\s+)+IDENT\s*\(`）。从当前标识符开始**向前**
/// 解析至少两个空白分隔的 token，再遇到 `(`，才视为声明；`return`/`new`/`throw` 等
/// 表达式关键字开头的序列被拒绝（hljs 用单独的 guard mode 消费它们）。
fn parse_function_decl(
    lang: &CompiledLanguage,
    text: &str,
    start: usize,
) -> Option<ParsedFunction> {
    if !lang.infer_functions {
        return None;
    }
    let mut p = start;
    let mut count = 0usize;
    loop {
        let mut end = re_at(&lang.ident, text, p)?;
        // cpp.js 的 `NAMESPACE_RE`（`[a-zA-Z_]\w*::`）：`std::vector` 是一个 TYPE 词元。
        if lang.name == "cpp" {
            while text[end..].starts_with("::") {
                let Some(next_end) = re_at(&lang.ident, text, end + 2) else {
                    break;
                };
                end = next_end;
            }
        }
        let word = &text[p..end];
        if count == 0 && is_reject_keyword(word) {
            return None;
        }
        count += 1;
        // csharp/java/cpp 的类型参数：`Task<int> M(` / `List<String> f(`。
        let mut q = skip_type_angle(lang.name, text, end)?;
        // 类型与名字之间允许 `*` / `&`（`int *foo(`）。
        q = next_non_whitespace(text, q);
        while matches!(text.as_bytes().get(q), Some(b'*') | Some(b'&')) {
            q = next_non_whitespace(text, q + 1);
        }
        if text.as_bytes().get(q) == Some(&b'(') {
            if count < 2 {
                return None;
            }
            let (inner_start, inner_end, after) = paren_content(text, q)?;
            return Some(ParsedFunction {
                title_start: p,
                title_end: end,
                open: q,
                inner_start,
                inner_end,
                after,
            });
        }
        p = q;
        if p >= text.len() {
            return None;
        }
    }
}

/// 渲染推断出的 C 系函数声明（span 从返回类型开始）。
fn scan_inferred_function(
    lang: &CompiledLanguage,
    text: &str,
    start: usize,
    decl: &ParsedFunction,
    writer: &mut Writer,
    ignore_illegals: bool,
) -> Result<usize, IllegalMatch> {
    writer.open("function");
    writer.relevance += 1;
    scan_inline(
        lang,
        &text[start..decl.title_start],
        writer,
        ignore_illegals,
    )?;
    writer.span("title", &text[decl.title_start..decl.title_end]);
    if lang.name == "csharp" && text[decl.title_end..decl.open].starts_with('<') {
        // csharp.js 的 GENERIC_MODIFIER：`<`/`>` 是 function 文本，内部标识符按
        // TITLE_MODE 着色（`in`/`out` 是 beginKeywords → keyword）。
        let mut q = decl.title_end;
        writer.plain_char('<');
        q += 1;
        while q < decl.open && text.as_bytes()[q] != b'>' {
            if let Some(end) = re_at(&lang.ident, text, q) {
                let word = &text[q..end];
                if matches!(word, "in" | "out") {
                    writer.span("keyword", word);
                } else {
                    writer.span("title", word);
                }
                q = end;
                continue;
            }
            let ch = text[q..].chars().next().expect("char boundary");
            writer.plain_char(ch);
            q += ch.len_utf8();
        }
        if q < decl.open && text.as_bytes()[q] == b'>' {
            writer.plain_char('>');
            q += 1;
        }
        writer.plain(&text[q..decl.open]);
    } else {
        writer.plain(&text[decl.title_end..decl.open]);
    }
    // java/c/cpp 的 `params` span 含括号；csharp 只在括号内（空参数时是空 span）。
    let include_parens = lang.name != "csharp";
    if !include_parens {
        writer.plain_char('(');
    }
    writer.open("params");
    writer.relevance += 1;
    if include_parens {
        writer.plain_char('(');
    }
    if decl.inner_start < decl.inner_end {
        scan_inline(
            lang,
            &text[decl.inner_start..decl.inner_end],
            writer,
            ignore_illegals,
        )?;
    }
    if include_parens {
        writer.plain_char(')');
    }
    writer.close();
    if !include_parens {
        writer.plain_char(')');
    }
    // hljs 的 function 模式 `end: /[{;=]/`（excludeEnd）：`throws X`、返回类型等仍在
    // function span 内，直到 `{`/`;`/`=` 或行尾。csharp 的 end 是 `/\s*[{;=]/`，空白
    // 属于 end 词素（被排除），所以 span 在 `)` 之后立即结束。
    let mut p = decl.after;
    if lang.name == "csharp" {
        writer.close();
        return Ok(p);
    }
    while p < text.len() {
        let ch = text[p..].chars().next().expect("char boundary");
        if matches!(ch, '{' | ';' | '=' | '\n') {
            break;
        }
        if let Some(end) = re_at(&lang.ident, text, p) {
            let word = &text[p..end];
            if let Some(class) = lang.keywords.get(word) {
                writer.span(class, word);
            } else {
                writer.plain(word);
            }
            p = end;
            continue;
        }
        writer.plain_char(ch);
        p += ch.len_utf8();
    }
    writer.close();
    Ok(p)
}

/// rust.js 的 raw string 模式 `r(#*)"(.|\n)*?"\1(?!#)`（需要反向引用，单独实现）。
///
/// begin 命中但找不到配对 end 时 highlight.js 的模式会一直开到输入末尾，这里返回
/// `text.len()`。返回 `Some(结束下标)` 表示整个 raw 字面量（含 `r#` 前缀）应作为一个
/// `string` span 输出。
fn try_rust_raw_string(text: &str, index: usize) -> Option<usize> {
    let bytes = text.as_bytes();
    if bytes.get(index) != Some(&b'r') {
        return None;
    }
    let mut p = index + 1;
    while bytes.get(p) == Some(&b'#') {
        p += 1;
    }
    if bytes.get(p) != Some(&b'"') {
        return None;
    }
    let hashes = p - index - 1;
    let mut q = p + 1;
    while q < bytes.len() {
        if bytes[q] == b'"' {
            let mut r = q + 1;
            let mut count = 0;
            while count < hashes && bytes.get(r) == Some(&b'#') {
                r += 1;
                count += 1;
            }
            if count == hashes && bytes.get(r) != Some(&b'#') {
                return Some(r);
            }
        }
        q += 1;
    }
    Some(bytes.len())
}

/// JS/TS 的 `$IDENT` / `.IDENT` 吞并规则（javascript.js 的 `.keyword()` hack）。
/// 返回标识符结束下标；调用方把整个区间当普通文本输出。
fn js_dotted_ident_end(text: &str, index: usize) -> Option<usize> {
    let bytes = text.as_bytes();
    let start = index + 1;
    let first = *bytes.get(start)?;
    if !(first.is_ascii_alphabetic() || matches!(first, b'$' | b'_')) {
        return None;
    }
    let mut p = start + 1;
    while p < bytes.len() {
        let c = bytes[p];
        if c.is_ascii_alphanumeric() || matches!(c, b'$' | b'_') {
            p += 1;
        } else {
            break;
        }
    }
    Some(p)
}

/// groovy.js 的三元表达式模式：`begin: /\?/, end: /:/`，contains
/// `COMMENT/STRING/REGEXP/NUMBER/self`。它排在 attr/class 等规则之前，三元内部的
/// 标识符不会被当作 map key（`x ? y : z` 的 `y` 不是 attr）。
fn scan_groovy_ternary(
    lang: &CompiledLanguage,
    text: &str,
    start: usize,
    writer: &mut Writer,
    ignore_illegals: bool,
) -> Result<usize, IllegalMatch> {
    // `self` 的嵌套三元用深度计数迭代实现（避免深嵌套输入递归爆栈）。
    let mut depth = 0usize;
    let mut p = start;
    while p < text.len() {
        let ch = text[p..].chars().next().expect("char boundary");
        if ch == '?' {
            depth += 1;
            writer.plain_char('?');
            p += 1;
            continue;
        }
        if ch == ':' {
            depth -= 1;
            writer.plain_char(':');
            p += 1;
            if depth == 0 {
                return Ok(p);
            }
            continue;
        }
        match try_string(lang, text, p, writer, ignore_illegals) {
            Some(Ok(end)) => {
                p = end;
                continue;
            }
            Some(Err(illegal)) => return Err(illegal),
            None => {}
        }
        if text[p..].starts_with("//") {
            let end = line_end(text, p);
            writer.span("comment", &text[p..end]);
            p = end;
            continue;
        }
        if text[p..].starts_with("/*") {
            let end = block_comment_end(text, p);
            writer.span("comment", &text[p..end]);
            p = end;
            continue;
        }
        if let Some(end) = lang.number.as_ref().and_then(|re| re_at(re, text, p)) {
            writer.span("number", &text[p..end]);
            p = end;
            continue;
        }
        writer.plain_char(ch);
        p += ch.len_utf8();
    }
    Ok(p)
}

/// 通用扫描器的主循环：从当前位置 `i` 逐条尝试规则，返回 `(html, relevance)`。
///
/// 分支顺序就是 grammar 的优先级（对应 highlight.js 合并正则里各模式的注册顺序），
/// **不能随意调换**。几个容易踩坑的例子：整体特殊 token 先于变量（php `$this`）、
/// 行注释先于块注释、字符串先于数字、class/decl/函数推断先于普通关键字表；
/// javascript 的 method/constructor/getter-setter 又必须排在普通标识符处理之前。
///
/// 所有规则都不命中时，把当前字符转义后原样输出并前进一个字符；换行没有特殊分支。
/// `IllegalMatch` 由字符串扫描或 language 级 illegal 规则向上抛出，调用方据此把
/// 整段结果回退为 escaped plaintext。
fn scan_generic(
    lang: &CompiledLanguage,
    text: &str,
    ignore_illegals: bool,
) -> Result<(String, u32), IllegalMatch> {
    let mut writer = Writer::new();
    let mut i = 0usize;
    // `use_strict` 的 `\s*` 可以跨空行；失败时记住空白段终点，避免每个行首重复扫描
    // 同一段空白（否则纯空白输入会退化 O(n²)）。
    let mut use_strict_skip_until = 0usize;
    while i < text.len() {
        let rest = &text[i..];

        // `#!/bin/bash`
        if lang.shebang && i == 0 && rest.starts_with("#!") {
            let end = line_end(text, i);
            writer.span("meta", &text[i..end]);
            i = end;
            continue;
        }

        // javascript/typescript 的 `'use strict'` / `'use asm'` 指令 → meta
        if matches!(lang.kind, Kind::Javascript | Kind::Typescript)
            && i >= use_strict_skip_until
            && js_line_start(text, i)
        {
            let directive = if lang.kind == Kind::Typescript {
                ts_use_strict_regex()
            } else {
                js_use_strict_regex()
            };
            if let Some(end) = re_at(directive, text, i) {
                writer.span("meta", &text[i..end]);
                i = end;
                continue;
            }
            use_strict_skip_until = js_whitespace_run_end(text, i);
        }

        // bash 的算术上下文 `$((...))`（优先于变量/注释/字符串规则）
        if lang.name == "bash"
            && let Some(end) = try_bash_arithmetic(text, i, &mut writer)
        {
            i = end;
            continue;
        }

        // swift.js 的 `import` 模式（`beginKeywords: 'import', end: /$/`，只含注释）：
        // 行首 `import` 之后的整行按普通文本输出（`import Foundation` 的 `Foundation`
        // 不会被顶层的 TYPE 规则着色）。
        if lang.name == "swift"
            && at_line_start(text, i)
            && text[i..].starts_with("import")
            && !text[i + 6..].starts_with(|c: char| c.is_alphanumeric() || c == '_')
        {
            let end = line_end(text, i);
            writer.span("keyword", "import");
            writer.plain(&text[i + 6..end]);
            i = end;
            continue;
        }

        // 行首 `#include` / `#define`（c/cpp）
        if lang.preprocessor && at_line_start(text, i) {
            let p = next_non_space(text, i);
            if text.as_bytes().get(p) == Some(&b'#') {
                // hljs 的 c/cpp 预处理 begin 是 `#\s*[a-z]+\b`，不含 `^\s*`：
                // 行首缩进是普通文本，只有 `#` 起的部分进入 meta span。
                writer.plain(&text[i..p]);
                i = scan_preprocessor(text, p, &mut writer);
                continue;
            }
        }

        // 行首 `@decorator`（python）。hljs 的 begin 是 `^[\t ]*@`，其中 `^` 是
        // JS 多行语义（行首 = 串首或 \n / \r /   /   之后），`$` 端点是
        // `(?=#)|$`（同样按 JS 行终止符）。缩进属于 meta span；更早的字符原样输出。
        if lang.decorator && js_line_start(text, i) {
            let mut p = i;
            while p < text.len() && matches!(text.as_bytes()[p], b' ' | b'\t') {
                p += 1;
            }
            if text.as_bytes().get(p) == Some(&b'@') {
                // meta 的 contains 是 NUMBER / PARAMS / STRING：`@app.route("/x")` 里的
                // 字符串与参数列表嵌套在 meta span 内（PARAMS 的空 `()` 变体 skip）。
                writer.open("meta");
                writer.relevance += 1;
                writer.plain(&text[i..p + 1]);
                let mut q = p + 1;
                while q < text.len() {
                    let ch = text[q..].chars().next().expect("char boundary");
                    if ch == '#' || is_js_line_terminator(ch) {
                        break;
                    }
                    match try_string(lang, text, q, &mut writer, ignore_illegals) {
                        Some(Ok(end)) => {
                            q = end;
                            continue;
                        }
                        Some(Err(illegal)) => return Err(illegal),
                        None => {}
                    }
                    if let Some(end) = lang.number.as_ref().and_then(|re| re_at(re, text, q)) {
                        writer.span("number", &text[q..end]);
                        q = end;
                        continue;
                    }
                    if text.as_bytes().get(q) == Some(&b'(')
                        && let Some((inner_start, inner_end, after)) = paren_content(text, q)
                    {
                        writer.plain_char('(');
                        let all_whitespace = text[inner_start..inner_end]
                            .chars()
                            .all(|ch| ch.is_whitespace());
                        if inner_start < inner_end && !all_whitespace {
                            writer.open("params");
                            writer.relevance += 1;
                            scan_inline(
                                lang,
                                &text[inner_start..inner_end],
                                &mut writer,
                                ignore_illegals,
                            )?;
                            writer.close();
                        } else if inner_start < inner_end {
                            writer.plain(&text[inner_start..inner_end]);
                        }
                        writer.plain_char(')');
                        q = after;
                        continue;
                    }
                    writer.plain_char(ch);
                    q += ch.len_utf8();
                }
                writer.close();
                i = q;
                continue;
            }
        }

        // 整体特殊 token：优先于 annotation/variable（php `$this`、swift `#available`、
        // rust 字符字面量必须先于 lifetime 符号）。
        let mut matched = false;
        for (re, class) in &lang.specials {
            if let Some(end) = re_at(re, text, i) {
                writer.span(class, &text[i..end]);
                i = end;
                matched = true;
                break;
            }
        }
        if matched {
            continue;
        }

        // `@Annotation`（java/kotlin/scala/swift/dart/groovy）
        if lang.annotation
            && rest.starts_with('@')
            && let Some(end) = re_at(&lang.ident, text, i + 1)
        {
            writer.span("meta", &text[i..end]);
            i = end;
            continue;
        }

        // javascript/typescript：`.IDENT` 整体吞掉，避免把属性名当关键字
        // （javascript.js 的 `{ begin: '\\.' + IDENT_RE }`）。
        if matches!(lang.kind, Kind::Javascript | Kind::Typescript)
            && text.as_bytes()[i] == b'.'
            && let Some(end) = js_dotted_ident_end(text, i)
        {
            writer.plain(&text[i..end]);
            i = end;
            continue;
        }

        // 变量（bash/php/ruby）
        let mut matched = false;
        for re in &lang.variables {
            if let Some(end) = re_at(re, text, i) {
                writer.span("variable", &text[i..end]);
                i = end;
                matched = true;
                break;
            }
        }
        if matched {
            continue;
        }

        // 仅消费不着色的 token（perl 变量）
        if let Some(end) = lang.plain_specials.iter().find_map(|re| re_at(re, text, i)) {
            writer.plain(&text[i..end]);
            i = end;
            continue;
        }

        // 符号（ruby `:sym`、rust `'a`）
        for re in &lang.symbols {
            if let Some(end) = re_at(re, text, i) {
                writer.span("symbol", &text[i..end]);
                i = end;
                matched = true;
                break;
            }
        }
        if matched {
            continue;
        }

        // bash 函数定义
        if let Some(end) = try_bash_function(lang, text, i, &mut writer) {
            i = end;
            continue;
        }

        // 行注释
        if lang.line_comments.iter().any(|p| rest.starts_with(*p)) {
            let end = line_end(text, i);
            writer.span("comment", &text[i..end]);
            i = end;
            continue;
        }

        // 块注释
        if let Some(end) = try_block_comment(lang, text, i, &mut writer) {
            i = end;
            continue;
        }

        // perl 的正则字面量（`m/.../`、`/.../`；容器由 RE_STARTERS 或
        // split/return/print/reverse/grep 触发）。先按首字节快速过滤，避免在每个
        // 字符上都做向前的上下文判定。
        if lang.name == "perl"
            && matches!(text.as_bytes()[i], b'/' | b'm' | b'q' | b's' | b't' | b'y')
            && let Some(end) = try_perl_regexp(text, i)
        {
            writer.span("regexp", &text[i..end]);
            i = end;
            continue;
        }

        // rust raw string（`r#"..."#`，需要反向引用）
        if lang.name == "rust"
            && let Some(end) = try_rust_raw_string(text, i)
        {
            writer.open("string");
            writer.relevance += 1;
            writer.plain(&text[i..end]);
            writer.close();
            i = end;
            continue;
        }

        // 正则字面量
        if lang.js_regexp
            && text.as_bytes()[i] == b'/'
            && regexp_allowed(text, i)
            && let Some(end) = try_js_regexp(text, i)
        {
            writer.span("regexp", &text[i..end]);
            i = end;
            continue;
        }

        // 字符串
        match try_string(lang, text, i, &mut writer, ignore_illegals) {
            Some(Ok(end)) => {
                i = end;
                continue;
            }
            Some(Err(illegal)) => return Err(illegal),
            None => {}
        }

        // 数字
        if let Some(re) = &lang.number
            && let Some(end) = re_at(re, text, i)
        {
            writer.span("number", &text[i..end]);
            i = end;
            continue;
        }

        // 标识符 / 声明关键字 / 普通关键字
        if let Some(end) = re_at(&lang.ident, text, i) {
            let word = &text[i..end];
            let preceded_by_dot = i > 0 && text.as_bytes()[i - 1] == b'.';
            // javascript/typescript 的箭头函数（单参数变体 `x => ...`）：hljs 的
            // `className: 'function'` 模式 `(IDENT\s*=>)`，contains 里 IDENT 是 `params`。
            if lang.js_regexp
                && let Some(after) = try_js_arrow_ident(text, i, end, &mut writer)
            {
                i = after;
                continue;
            }
            // javascript/typescript 的对象字面量键（hljs 的 "object attr container"：
            // `[{,\n]\s*` + lookahead `IDENT\s*:`，attr 只覆盖标识符本身）。
            if lang.js_regexp && js_object_attr_matches(text, i, end, word) {
                writer.span("attr", word);
                i = end;
                continue;
            }
            // hljs 的 beginKeywords 模式带 `skipIfhasPrecedingDot`：`Foo.class` 不进入
            // class 模式，字符回落到普通关键字处理。
            if !preceded_by_dot
                && let Some(body) = lang.class_bodies.iter().find(|body| body.keyword == word)
            {
                i = scan_class_body(lang, text, i, end, body, &mut writer, ignore_illegals)?;
                continue;
            }
            if !preceded_by_dot
                && let Some(decl) = lang.decls.iter().find(|decl| decl.keyword == word)
            {
                i = scan_decl(lang, text, i, end, decl, &mut writer, ignore_illegals)?;
                continue;
            }
            // javascript/typescript 的 method / constructor / getter-setter 模式。
            // hljs contains 顺序：function 模式与 `while if switch catch for` 在 method
            // 之前，method 在 class/constructor/get-set 之前。
            if matches!(lang.kind, Kind::Javascript | Kind::Typescript) {
                // value container（`return`/`throw`/`case`）与 while-if 的 beginKeywords
                // 模式先于 method 命中，这些词不进入 method 模式。
                const METHOD_BLOCKERS: &[&str] = &[
                    "while", "if", "switch", "catch", "for", "return", "throw", "case",
                ];
                if !METHOD_BLOCKERS.contains(&word)
                    && let Some(after) =
                        try_js_method(lang, text, i, end, &mut writer, ignore_illegals)?
                {
                    i = after;
                    continue;
                }
                if !preceded_by_dot && word.starts_with("constructor") {
                    i = scan_js_constructor(lang, text, i, end, &mut writer, ignore_illegals)?;
                    continue;
                }
                if let Some(after) =
                    try_js_get_set(lang, text, word, end, &mut writer, ignore_illegals)?
                {
                    i = after;
                    continue;
                }
            }
            // java.js 的 record 模式（`begin: 'record\s+' + UNDERSCORE_IDENT_RE + '\s*\('`，
            // returnBegin + excludeEnd + end `[{;=]`）：`record R(...)` 的头部是 class span，
            // `record` 是 keyword、标题是 title、参数列表是 params。`record` 不在 KW_JAVA
            // 里，之前会被 `(类型\s+)+名字\(` 的函数推断吞掉。
            if lang.name == "java"
                && word == "record"
                && let Some(after) = scan_java_record(lang, text, i, end, &mut writer)
            {
                i = after;
                continue;
            }
            // rust raw string 的 begin 可能落在当前标识符内部（`br#"..."#` 里的 `r`）：
            // hljs 的合并正则先匹配到 `r#"`，前缀 `b` 只是普通文本。
            if lang.name == "rust"
                && word.len() > 1
                && word.ends_with('r')
                && let Some(raw_end) = try_rust_raw_string(text, end - 1)
            {
                writer.plain(&text[i..end - 1]);
                writer.open("string");
                writer.relevance += 1;
                writer.plain(&text[end - 1..raw_end]);
                writer.close();
                i = raw_end;
                continue;
            }
            // rust.js: `{ begin: IDENT_RE + '::' }` 只带 built_in 关键字表：`crate::`、
            // `self::` 不着色，`Vec::` 仍按 built_in 着色（与我们普通关键字表一致）。
            // ident 带 `!` 后缀时（`println!`）不属于该模式的 begin。
            if lang.name == "rust" && !word.ends_with('!') && text[end..].starts_with("::") {
                if let Some(class) = lang.keywords.get(word).filter(|class| *class == "built_in") {
                    writer.span(class, word);
                } else {
                    writer.plain(word);
                }
                writer.plain("::");
                i = end + 2;
                continue;
            }
            if lang.upper_type && word.starts_with(|c: char| c.is_uppercase()) {
                writer.span("type", word);
                i = end;
                continue;
            }
            // groovy.js 的 map key / 命名参数：`IDENT[ \t]*:` → attr（冒号含在 span 内）。
            if lang.name == "groovy" {
                let mut colon = end;
                while matches!(text.as_bytes().get(colon), Some(b' ') | Some(b'\t')) {
                    colon += 1;
                }
                if text.as_bytes().get(colon) == Some(&b':') {
                    writer.span("attr", &text[i..colon + 1]);
                    i = colon + 1;
                    continue;
                }
            }
            if lang.begin_keywords.contains(&word) {
                if preceded_by_dot {
                    // `skipIfhasPrecedingDot`：回落到普通关键字表（`else` 在表里会着色，
                    // `new`/`return`/`throw` 不在表里保持普通文本，与 hljs 一致）。
                    if let Some(class) = lang.keywords.get(word) {
                        writer.span(class, word);
                    } else {
                        writer.plain(word);
                    }
                } else {
                    writer.span("keyword", word);
                }
                i = end;
                continue;
            }
            if let Some(parsed) = parse_function_decl(lang, text, i) {
                i = scan_inferred_function(lang, text, i, &parsed, &mut writer, ignore_illegals)?;
                continue;
            }
            // c/cpp 的 `CPP_PRIMITIVE_TYPES`（hljs contains 里 function 声明优先于
            // 该模式，所以放在函数推断之后）。
            if lang.primitive_types && is_c_primitive_type(word) {
                writer.span("keyword", word);
                i = end;
                continue;
            }
            if lang.attr_assign {
                let after_space = next_non_space(text, end);
                if text.as_bytes().get(after_space) == Some(&b'=') {
                    writer.span("attr", word);
                    i = end;
                    continue;
                }
            }
            if let Some(class) = lang.keywords.get(word) {
                writer.span(class, word);
                i = end;
                continue;
            }
            writer.plain(word);
            i = end;
            continue;
        }

        // javascript/typescript 的箭头函数（括号参数变体 `(a, b) => ...`）：
        // hljs 的 begin 是 `(...)\s*=>`（returnBegin，end `\s*=>` 属于 function span），
        // `(...)` 内部是 `params`（`excludeBegin/End`；空/纯空白参数列表 skip，不产生 span）。
        if lang.js_regexp
            && text.as_bytes()[i] == b'('
            && let Some(after) = try_js_arrow_paren(lang, text, i, &mut writer, ignore_illegals)?
        {
            i = after;
            continue;
        }

        // groovy.js 的三元表达式模式（阻止 `x ? y : z` 的 `y` 被当成 attr）。
        if lang.name == "groovy" && text.as_bytes()[i] == b'?' {
            i = scan_groovy_ternary(lang, text, i, &mut writer, ignore_illegals)?;
            continue;
        }

        // language 级别 illegal（仅在 ignoreIllegals=false 时抛错并整体回退）。
        if !ignore_illegals
            && lang
                .top_illegal
                .iter()
                .any(|re| re_at(re, text, i).is_some())
        {
            return Err(IllegalMatch);
        }

        let ch = rest.chars().next().expect("char boundary");
        writer.plain_char(ch);
        i += ch.len_utf8();
    }
    Ok((writer.out, writer.relevance))
}

/// c/cpp 的预处理指令（`#` 开始到行尾，含 `meta-keyword` 与 `meta-string`）。
fn scan_preprocessor(text: &str, hash: usize, writer: &mut Writer) -> usize {
    let end = line_end(text, hash);
    writer.open("meta");
    writer.relevance += 1;
    writer.plain_char('#');
    let mut p = hash + 1;
    while p < end && (text.as_bytes()[p] == b' ' || text.as_bytes()[p] == b'\t') {
        p += 1;
    }
    writer.plain(&text[hash + 1..p]);
    let word_start = p;
    while p < end && (text.as_bytes()[p].is_ascii_alphabetic() || text.as_bytes()[p] == b'_') {
        p += 1;
    }
    if p > word_start {
        writer.span("meta-keyword", &text[word_start..p]);
    }
    // 剩余部分：字符串/尖括号内容作为 meta-string，其余原样。
    while p < end {
        if text[p..].starts_with('<')
            && let Some(offset) = text[p..end].find('>')
        {
            let close = p + offset + 1;
            writer.span("meta-string", &text[p..close]);
            p = close;
            continue;
        }
        if text[p..].starts_with('"')
            && let Some(offset) = text[p + 1..end].find('"')
        {
            let close = p + 1 + offset + 1;
            writer.span("meta-string", &text[p..close]);
            p = close;
            continue;
        }
        let ch = text[p..].chars().next().expect("char boundary");
        writer.plain_char(ch);
        p += ch.len_utf8();
    }
    writer.close();
    end
}

// ---------------------------------------------------------------------------
// json / xml / diff / plaintext 专用扫描器
// ---------------------------------------------------------------------------

/// JSON 的专用扫描器：json grammar 没有声明/类/变量等模式，扫描顺序为注释
/// （`//`、`/* */`，LangDef 为 json 配置了这两种注释）、字符串、数字、标识符表
/// （`true`/`false`/`null` 为 `literal`），其余字符原样输出。
///
/// 字符串交给 `scan_json_string`：先扫描内容，再依据后随的 `:` 决定外层是 `attr`
/// 还是 `string`。json.js 顶层的 `illegal: /\S/` 未建模（见文件级“已知偏差”）。
fn scan_json(
    lang: &CompiledLanguage,
    text: &str,
    ignore_illegals: bool,
) -> Result<(String, u32), IllegalMatch> {
    let mut writer = Writer::new();
    let mut i = 0usize;
    while i < text.len() {
        let rest = &text[i..];
        if lang.line_comments.iter().any(|p| rest.starts_with(*p)) {
            let end = line_end(text, i);
            writer.span("comment", &text[i..end]);
            i = end;
            continue;
        }
        if let Some(end) = try_block_comment(lang, text, i, &mut writer) {
            i = end;
            continue;
        }
        if let Some(str_def) = lang.strings.first()
            && let Some(begin_end) = re_at(&str_def.begin, text, i)
        {
            i = scan_json_string(
                lang,
                text,
                i,
                begin_end,
                str_def,
                &mut writer,
                ignore_illegals,
            )?;
            continue;
        }
        if let Some(re) = &lang.number
            && let Some(end) = re_at(re, text, i)
        {
            writer.span("number", &text[i..end]);
            i = end;
            continue;
        }
        if let Some(end) = re_at(&lang.ident, text, i) {
            let word = &text[i..end];
            if let Some(class) = lang.keywords.get(word) {
                writer.span(class, word);
            } else {
                writer.plain(word);
            }
            i = end;
            continue;
        }
        let ch = rest.chars().next().expect("char boundary");
        writer.plain_char(ch);
        i += ch.len_utf8();
    }
    Ok((writer.out, writer.relevance))
}

/// json 字符串：先扫描内容到缓冲区，再看后随的 `:` 决定 `attr`/`string`。
fn scan_json_string(
    lang: &CompiledLanguage,
    text: &str,
    start: usize,
    begin_end: usize,
    str_def: &CompiledStr,
    writer: &mut Writer,
    ignore_illegals: bool,
) -> Result<usize, IllegalMatch> {
    let mut inner = Writer::new();
    let end = scan_string_content(lang, text, begin_end, str_def, &mut inner, ignore_illegals)?;
    let look = next_non_whitespace(text, end);
    let class = if str_def.attr_if_colon && text.as_bytes().get(look) == Some(&b':') {
        "attr"
    } else {
        str_def.class
    };
    writer.open(class);
    writer.relevance += 1;
    writer.plain(&text[start..begin_end]);
    writer.out.push_str(&inner.out);
    writer.close();
    Ok(end)
}

/// XML 名字（标签名/属性名）的结束下标：由 ASCII 字母、数字与 `_ . : -` 组成。
fn xml_tag_name_end(text: &str, start: usize) -> usize {
    let mut p = start;
    while p < text.len() {
        let ch = text[p..].chars().next().expect("char boundary");
        if ch.is_ascii_alphanumeric() || matches!(ch, '_' | '.' | ':' | '-') {
            p += ch.len_utf8();
        } else {
            break;
        }
    }
    p
}

/// XML/HTML 的标签级扫描器：注释、CDATA（不着色，原样输出）、`<!...>`/`<?...>`
/// 指令、开/闭标签（`tag` + `name`，属性名 `attr`、属性值 `string`）与实体
/// （`symbol`）。不做标签配对校验，也不嵌入 `<script>`/`<style>` 的子语言
/// （见文件级“已知偏差”）。
fn scan_xml(text: &str) -> (String, u32) {
    let mut writer = Writer::new();
    let mut i = 0usize;
    while i < text.len() {
        let rest = &text[i..];
        if rest.starts_with("<!--") {
            let end = text[i + 4..]
                .find("-->")
                .map_or(text.len(), |o| i + 4 + o + 3);
            writer.span("comment", &text[i..end]);
            i = end;
            continue;
        }
        // CDATA 模式（`<!\[CDATA\[` … `\]\]>`，xml.js 的 case_insensitive）只计 relevance，
        // 没有 className —— 不着色，内容原样输出。
        if rest.len() >= 9 && rest.as_bytes()[..9].eq_ignore_ascii_case(b"<![CDATA[") {
            let end = text[i + 9..]
                .find("]]>")
                .map_or(text.len(), |o| i + 9 + o + 3);
            writer.plain(&text[i..end]);
            i = end;
            continue;
        }
        if rest.starts_with("<!") || rest.starts_with("<?") {
            let end = text[i..].find('>').map_or(text.len(), |o| i + o + 1);
            writer.span("meta", &text[i..end]);
            i = end;
            continue;
        }
        if rest.starts_with('<')
            && !rest.starts_with("</")
            && let Some(end) = scan_xml_open_tag(text, i, &mut writer)
        {
            i = end;
            continue;
        }
        if rest.starts_with("</")
            && let Some(end) = scan_xml_close_tag(text, i, &mut writer)
        {
            i = end;
            continue;
        }
        if rest.starts_with('&')
            && let Some(end) = scan_xml_entity(text, i, &mut writer)
        {
            i = end;
            continue;
        }
        let ch = rest.chars().next().expect("char boundary");
        writer.plain_char(ch);
        i += ch.len_utf8();
    }
    (writer.out, writer.relevance)
}

/// XML 名字的首字符判定：ASCII 字母或 `_`。调用方只用它检查标签名的第一个字符。
fn is_xml_name_start(ch: char) -> bool {
    ch.is_ascii_alphabetic() || ch == '_'
}

/// `<tag ...>` 或 `<tag/>`。
fn scan_xml_open_tag(text: &str, start: usize, writer: &mut Writer) -> Option<usize> {
    let name_start = start + 1;
    if !text[name_start..]
        .chars()
        .next()
        .is_some_and(is_xml_name_start)
    {
        return None;
    }
    let name_end = xml_tag_name_end(text, name_start);
    // 标签名后必须是 `>`、`/>` 或空白（`<style` 的 lookahead）。
    match text.as_bytes().get(name_end) {
        Some(b'>') | Some(b'/') | Some(b' ') | Some(b'\t') | Some(b'\n') | Some(b'\r') => {}
        _ => return None,
    }
    writer.open("tag");
    writer.relevance += 1;
    writer.plain_char('<');
    writer.span("name", &text[name_start..name_end]);
    let mut p = name_end;
    while p < text.len() {
        let ch = text[p..].chars().next().expect("char boundary");
        if ch == '>' {
            writer.plain_char('>');
            writer.close();
            return Some(p + 1);
        }
        if ch == '/' && text[p..].starts_with("/>") {
            writer.plain("/>");
            writer.close();
            return Some(p + 2);
        }
        if ch.is_ascii_whitespace() || ch == '/' {
            writer.plain_char(ch);
            p += ch.len_utf8();
            continue;
        }
        if ch == '=' {
            writer.plain_char('=');
            p += 1;
            let value_start = next_non_whitespace(text, p);
            if value_start > p {
                writer.plain(&text[p..value_start]);
            }
            if text.as_bytes().get(value_start) == Some(&b'"')
                || text.as_bytes().get(value_start) == Some(&b'\'')
            {
                let quote = text.as_bytes()[value_start];
                let close = text[value_start + 1..]
                    .find(quote as char)
                    .map_or(text.len(), |o| value_start + 1 + o + 1);
                writer.span("string", &text[value_start..close]);
                p = close;
            } else {
                let value_end = text[value_start..]
                    .find(|c: char| c.is_whitespace() || c == '>')
                    .map_or(text.len(), |o| value_start + o);
                if value_end > value_start {
                    writer.span("string", &text[value_start..value_end]);
                }
                p = value_end;
            }
            continue;
        }
        if ch == '"' || ch == '\'' {
            let close = text[p + 1..].find(ch).map_or(text.len(), |o| p + 1 + o + 1);
            writer.span("string", &text[p..close]);
            p = close;
            continue;
        }
        // 属性名
        if ch.is_ascii_alphanumeric() || matches!(ch, '_' | '.' | ':' | '-') {
            let attr_end = xml_tag_name_end(text, p);
            writer.span("attr", &text[p..attr_end]);
            p = attr_end;
            continue;
        }
        writer.plain_char(ch);
        p += ch.len_utf8();
    }
    writer.close();
    Some(p)
}

/// `</tag>`。
fn scan_xml_close_tag(text: &str, start: usize, writer: &mut Writer) -> Option<usize> {
    let name_start = start + 2;
    if !text[name_start..]
        .chars()
        .next()
        .is_some_and(is_xml_name_start)
    {
        return None;
    }
    let name_end = xml_tag_name_end(text, name_start);
    if text.as_bytes().get(name_end) != Some(&b'>') {
        return None;
    }
    writer.open("tag");
    writer.relevance += 1;
    writer.plain("</");
    writer.span("name", &text[name_start..name_end]);
    writer.plain_char('>');
    writer.close();
    Some(name_end + 1)
}

/// XML 实体：`&name;`、`&#123;`、`&#x1f;` 输出为 `symbol`。名字部分只接受 ASCII
/// 字母、十进制数字或十六进制数字，无法识别时返回 `None`，调用方按普通 `&` 输出。
fn scan_xml_entity(text: &str, start: usize, writer: &mut Writer) -> Option<usize> {
    let semicolon = text[start..].find(';').map(|o| start + o)?;
    let entity = &text[start + 1..semicolon];
    // xml.js: `&[a-z]+;|&#[0-9]+;|&#x[a-f0-9]+;`，language 带 case_insensitive。
    let named = !entity.is_empty() && entity.chars().all(|c| c.is_ascii_alphabetic());
    let decimal = entity
        .strip_prefix('#')
        .is_some_and(|digits| !digits.is_empty() && digits.chars().all(|c| c.is_ascii_digit()));
    let hex = (entity.starts_with("#x") || entity.starts_with("#X"))
        && entity[2..].chars().all(|c| c.is_ascii_hexdigit())
        && entity.len() > 2;
    if !(named || decimal || hex) {
        return None;
    }
    writer.span("symbol", &text[start..=semicolon]);
    Some(semicolon + 1)
}

/// diff/patch 的行扫描器：每行只按一个类别处理，判断顺序固定为
/// meta（hunk 头与范围行）→ comment（`diff --git`、`Index:`、`+++`/`---` 文件头等）
/// → `+` addition / `-` deletion / `!` addition → 其他原样。因此 `+++ b/file` 是
/// comment 而不是 addition。行尾的 `\n` 逐行补回；输入末尾没有换行时不补。
fn scan_diff(text: &str) -> (String, u32) {
    let mut writer = Writer::new();
    let mut i = 0usize;
    while i < text.len() {
        let end = line_end(text, i);
        let line = &text[i..end];
        if let Some(meta_end) = diff_meta_end(line) {
            // diff.js 的 meta 模式没有 end：hunk 头只匹配到第二个 `@@`，其后的
            // 函数名上下文是普通文本；`*** n,m ****` / `--- n,m ----` 由 `$` 覆盖整行。
            if meta_end > 0 {
                writer.span("meta", &line[..meta_end]);
            }
            if meta_end < line.len() {
                writer.plain(&line[meta_end..]);
            }
        } else if is_diff_comment(line) {
            writer.span("comment", line);
        } else if line.starts_with('+') {
            writer.span("addition", line);
        } else if line.starts_with('-') {
            writer.span("deletion", line);
        } else if line.starts_with('!') {
            writer.span("addition", line);
        } else {
            writer.plain(line);
        }
        i = end;
        if i < text.len() {
            writer.plain_char('\n');
            i += 1;
        }
    }
    (writer.out, writer.relevance)
}

/// 返回 meta 前缀的字节长度（整行时等于 `line.len()`）。
fn diff_meta_end(line: &str) -> Option<usize> {
    static HUNK: OnceLock<Regex> = OnceLock::new();
    static OLD_RANGE: OnceLock<Regex> = OnceLock::new();
    static NEW_RANGE: OnceLock<Regex> = OnceLock::new();
    let hunk = HUNK.get_or_init(|| Regex::new(r"^@@ +-\d+,\d+ +\+\d+,\d+ +@@").expect("hunk"));
    let old_range =
        OLD_RANGE.get_or_init(|| Regex::new(r"^\*\*\* +\d+,\d+ +\*\*\*\*$").expect("old"));
    let new_range = NEW_RANGE.get_or_init(|| Regex::new(r"^--- +\d+,\d+ +----$").expect("new"));
    if let Some(found) = hunk.find(line) {
        return Some(found.end());
    }
    if old_range.is_match(line) || new_range.is_match(line) {
        return Some(line.len());
    }
    None
}

/// 判定一行是否按 `comment` 输出：`Index: `、行首 `index`、`===`、`---`、`*** `、
/// `+++`、单独的分隔行 `***************` 与 `diff --git`。
fn is_diff_comment(line: &str) -> bool {
    static DIFF_GIT: OnceLock<Regex> = OnceLock::new();
    static INDEX: OnceLock<Regex> = OnceLock::new();
    let diff_git = DIFF_GIT.get_or_init(|| Regex::new(r"^diff --git").expect("diff --git"));
    let index = INDEX.get_or_init(|| Regex::new(r"^index").expect("index"));
    line.starts_with("Index: ")
        || index.is_match(line)
        || line.starts_with("===")
        || line.starts_with("---")
        || line.starts_with("*** ")
        || line.starts_with("+++")
        || line == "***************"
        || diff_git.is_match(line)
}

// ---------------------------------------------------------------------------
// highlight / highlightAuto
// ---------------------------------------------------------------------------

/// 按 grammar 形态分派扫描器：plaintext 直接整段转义；xml/diff/json 走各自的
/// 专用扫描器；`Python`/`Javascript`/`Typescript`/`Generic` 统一交给 `scan_generic`
/// （语言差异由 `CompiledLanguage` 的字段与 `Kind` 分支控制）。
///
/// 返回 `(html, relevance)`；`IllegalMatch` 原样上抛，由 `highlight` 或
/// `highlight_auto` 决定回退方式（整段 escaped plaintext、relevance 0）。
fn highlight_language(
    lang: &CompiledLanguage,
    code: &str,
    ignore_illegals: bool,
) -> Result<(String, u32), IllegalMatch> {
    match lang.kind {
        Kind::Plaintext => Ok((escape_html(code), 0)),
        Kind::Xml => Ok(scan_xml(code)),
        Kind::Diff => Ok(scan_diff(code)),
        Kind::Json => scan_json(lang, code, ignore_illegals),
        Kind::Python | Kind::Javascript | Kind::Typescript | Kind::Generic => {
            scan_generic(lang, code, ignore_illegals)
        }
    }
}

/// 按语言名或别名查已编译的 grammar；先 `to_lowercase()`，与 hljs 的 `getLanguage`
/// 一致。只查注册表里已实现的 grammar，不查 `ALL_LANGUAGE_NAMES` 目录，因此目录中
/// 未实现的语言返回 `None`（`highlight` 据此回退为 plaintext）。
fn lookup_language(name: &str) -> Option<&'static CompiledLanguage> {
    let lower = name.to_lowercase();
    let reg = registry();
    reg.lookup
        .get(lower.as_str())
        .map(|&index| &reg.languages[index])
}

/// TS: `hljs.highlightAuto(code, languageSubset)` —— 简化 relevance 计分。
///
/// 候选遍历：`language_subset` 为 `None` 时使用全部已实现 grammar（按注册表声明
/// 顺序），`Some` 时按子集给出的顺序查找，未知名字被忽略；`disable_autodetect` 为真的
/// 语言（`plaintext`）始终被排除。每个候选都以 `ignore_illegals = false` 扫描，
/// illegal 候选得到 escaped plaintext、relevance 0。比较用严格的 `>`，平局保留先出现
/// 的候选；初始成绩是 relevance 0 的 escaped plaintext，因此没有任何候选得分时返回
/// 纯文本。
fn highlight_auto(code: &str, language_subset: Option<&[String]>) -> String {
    let reg = registry();
    let mut candidates: Vec<&CompiledLanguage> = Vec::new();
    match language_subset {
        Some(names) => {
            for name in names {
                let lower = name.to_lowercase();
                if let Some(&index) = reg.lookup.get(lower.as_str()) {
                    let lang = &reg.languages[index];
                    if !lang.disable_autodetect {
                        candidates.push(lang);
                    }
                }
            }
        }
        None => {
            for lang in &reg.languages {
                if !lang.disable_autodetect {
                    candidates.push(lang);
                }
            }
        }
    }

    // plaintext 永远是最低分候选（relevance 0），与 hljs 的 `results.unshift(plaintext)` 一致。
    let mut best_html = escape_html(code);
    let mut best_relevance = 0u32;
    for lang in candidates {
        // hljs.highlightAuto 内部固定用 `ignoreIllegals=false`：illegal 的候选得到
        // 整段 escaped plaintext、relevance 0（core.js 的 highlightAuto）。
        let (html, relevance) = match highlight_language(lang, code, false) {
            Ok(result) => result,
            Err(_) => (escape_html(code), 0),
        };
        if relevance > best_relevance {
            best_relevance = relevance;
            best_html = html;
        }
    }
    best_html
}

/// TS: `highlight(code, options = {})`。
///
/// # 参数与返回
///
/// - `code`：完整源码，可含任意换行；换行在扫描与渲染阶段都保留。
/// - `options.language`：非空时按名字/别名查注册表；空串与 `None` 都走自动检测。
///   未知语言不会报错：整段变成没有 span 的 escaped plaintext（最终渲染只可能命中
///   `default` formatter；TS 会抛 `Error("Unknown language")`）。
/// - `options.language_subset`：只在自动检测路径使用。
/// - `options.ignore_illegals`：仅在显式 `language` 路径控制 illegal 是否让整段
///   回退；自动检测路径由 `highlight_auto` 固定以 `false` 扫描，此字段不生效。
///   详见 `HighlightOptions`。
/// - `options.theme`：渲染阶段的 scope → formatter 表；空表时返回去标签的纯文本。
///
/// # 失败模式
///
/// 返回 `String` 而不是 `Result`。显式 `language` 路径上，扫描失败（`IllegalMatch`，
/// 见 `scan_string_content`）或语言名未知都会退化为 `escape_html(code)`，颜色整体
/// 丢失。自动检测路径（`language` 为 `None` 或空串，见 `highlight_auto`）不同：每个
/// 候选独立扫描，某个候选的 `Err` 只把它自己记成 `(escape_html(code), 0)` 并继续比较；
/// 只要还有候选的 relevance 大于 0，返回的就是该候选生成的 HTML，颜色不会因单个候选
/// 失败而整体丢失；全部候选都失败或都得 0 分时结果才是纯文本。内部大量
/// `expect` 用于断言下标落在字符边界和内置正则可编译，属于实现不变量；不满足时是
/// 代码 bug 而不是可预期的输入错误。
pub fn highlight(code: &str, options: &HighlightOptions) -> String {
    // TS 用 `options.language ? ... : ...`；空字符串是 falsy → 走 highlightAuto。
    let html = match options.language.as_deref().filter(|name| !name.is_empty()) {
        Some(language) => match lookup_language(language) {
            Some(lang) => match highlight_language(lang, code, options.ignore_illegals) {
                Ok((html, _)) => html,
                // highlight.js 捕获 Illegal 后返回整段转义文本（没有 span）。
                Err(_) => escape_html(code),
            },
            // TS 会抛 `Error("Unknown language: ...")`；签名冻结为 String，回退 plaintext。
            None => escape_html(code),
        },
        None => highlight_auto(code, options.language_subset.as_deref()),
    };
    render_highlighted_html(&html, &options.theme)
}

// ---------------------------------------------------------------------------
// 关键字表（由 highlight.js 10.7.3 的 language 定义生成）
// ---------------------------------------------------------------------------
//
// 纯数据：每组是 (span 类名, 空格分隔的词表)，由 `KeywordTable::compile` 展开成
// “词 → 类”的映射。类名以下划线开头（如 cpp 的 `_relevance_hints`）时只在上游
// highlight.js 的 relevance 里生效，本实现直接跳过（不产生 span，也不参与计分）。
// 修改词表会直接改变着色结果，必须与 10.7.3 的对应 language 定义逐词对照，不要凭
// “看起来像关键字”增删。

const KW_PYTHON: &[(&str, &str)] = &[
    (
        "keyword",
        "and as assert async await break class continue def del elif else except finally for from global if import in is lambda nonlocal not or pass raise return try while with yield",
    ),
    (
        "built_in",
        "__import__ abs all any ascii bin bool breakpoint bytearray bytes callable chr classmethod compile complex delattr dict dir divmod enumerate eval exec filter float format frozenset getattr globals hasattr hash help hex id input int isinstance issubclass iter len list locals map max memoryview min next object oct open ord pow print property range repr reversed round set setattr slice sorted staticmethod str sum super tuple type vars zip",
    ),
    (
        "literal",
        "__debug__ Ellipsis False None NotImplemented True",
    ),
    (
        "type",
        "Any Callable Coroutine Dict List Literal Generic Optional Sequence Set Tuple Type Union",
    ),
];
const KW_JAVA: &[(&str, &str)] = &[(
    "keyword",
    "false synchronized int abstract float private char boolean var static null if const for true while long strictfp finally protected import native final void enum else break transient catch instanceof byte super volatile case assert short package default double public try this switch continue throws protected public private module requires exports do",
)];
const KW_GO: &[(&str, &str)] = &[
    (
        "keyword",
        "break default func interface select case map struct chan else goto package switch const fallthrough if range type continue for import return var go defer bool byte complex64 complex128 float32 float64 int8 int16 int32 int64 string uint8 uint16 uint32 uint64 int uint uintptr rune",
    ),
    ("literal", "true false iota nil"),
    (
        "built_in",
        "append cap close complex copy imag len make new panic print println real recover delete",
    ),
];
const KW_JAVASCRIPT: &[(&str, &str)] = &[
    (
        "keyword",
        "as in of if for while finally var new function do return void else break catch instanceof with throw case default try switch continue typeof delete let yield const class debugger async await static import from export extends",
    ),
    ("literal", "true false null undefined NaN Infinity"),
    (
        "built_in",
        "setInterval setTimeout clearInterval clearTimeout require exports eval isFinite isNaN parseFloat parseInt decodeURI decodeURIComponent encodeURI encodeURIComponent escape unescape arguments this super console window document localStorage module global Intl DataView Number Math Date String RegExp Object Function Boolean Error Symbol Set Map WeakSet WeakMap Proxy Reflect JSON Promise Float64Array Int16Array Int32Array Int8Array Uint16Array Uint32Array Float32Array Array Uint8Array Uint8ClampedArray ArrayBuffer BigInt64Array BigUint64Array BigInt EvalError InternalError RangeError ReferenceError SyntaxError TypeError URIError",
    ),
];
const KW_JSON: &[(&str, &str)] = &[("literal", "true false null")];
const KW_CPP: &[(&str, &str)] = &[
    (
        "keyword",
        "int float while private char char8_t char16_t char32_t catch import module export virtual operator sizeof dynamic_cast typedef const_cast const for static_cast union namespace unsigned long volatile static protected bool template mutable if public friend do goto auto void enum else break extern using asm case typeid wchar_t short reinterpret_cast default double register explicit signed typename try this switch continue inline delete alignas alignof constexpr consteval constinit decltype concept co_await co_return co_yield requires noexcept static_assert thread_local restrict final override atomic_bool atomic_char atomic_schar atomic_uchar atomic_short atomic_ushort atomic_int atomic_uint atomic_long atomic_ulong atomic_llong atomic_ullong new throw return and and_eq bitand bitor compl not not_eq or or_eq xor xor_eq",
    ),
    ("built_in", "_Bool _Complex _Imaginary"),
    (
        "_relevance_hints",
        "asin atan2 atan calloc ceil cosh cos exit exp fabs floor fmod fprintf fputs free frexp auto_ptr deque list queue stack vector map set pair bitset multiset multimap unordered_set fscanf future isalnum isalpha iscntrl isdigit isgraph islower isprint ispunct isspace isupper isxdigit tolower toupper labs ldexp log10 log malloc realloc memchr memcmp memcpy memset modf pow printf putchar puts scanf sinh sin snprintf sprintf sqrt sscanf strcat strchr strcmp strcpy strcspn strlen strncat strncmp strncpy strpbrk strrchr strspn strstr tanh tan unordered_map unordered_multiset unordered_multimap priority_queue make_pair array shared_ptr abort terminate abs acos vfprintf vprintf vsprintf endl initializer_list unique_ptr complex imaginary std string wstring cin cout cerr clog stdin stdout stderr stringstream istringstream ostringstream",
    ),
    ("literal", "true false nullptr NULL"),
];
const KW_TYPESCRIPT: &[(&str, &str)] = &[
    (
        "keyword",
        "as in of if for while finally var new function do return void else break catch instanceof with throw case default try switch continue typeof delete let yield const class debugger async await static import from export extends type namespace typedef interface public private protected implements declare abstract readonly",
    ),
    ("literal", "true false null undefined NaN Infinity"),
    (
        "built_in",
        "setInterval setTimeout clearInterval clearTimeout require exports eval isFinite isNaN parseFloat parseInt decodeURI decodeURIComponent encodeURI encodeURIComponent escape unescape arguments this super console window document localStorage module global Intl DataView Number Math Date String RegExp Object Function Boolean Error Symbol Set Map WeakSet WeakMap Proxy Reflect JSON Promise Float64Array Int16Array Int32Array Int8Array Uint16Array Uint32Array Float32Array Array Uint8Array Uint8ClampedArray ArrayBuffer BigInt64Array BigUint64Array BigInt EvalError InternalError RangeError ReferenceError SyntaxError TypeError URIError any void number boolean string object never enum",
    ),
];
const KW_PHP: &[(&str, &str)] = &[
    (
        "keyword",
        "__CLASS__ __DIR__ __FILE__ __FUNCTION__ __LINE__ __METHOD__ __NAMESPACE__ __TRAIT__ die echo exit include include_once print require require_once array abstract and as binary bool boolean break callable case catch class clone const continue declare default do double else elseif empty enddeclare endfor endforeach endif endswitch endwhile enum eval extends final finally float for foreach from global goto if implements instanceof insteadof int integer interface isset iterable list match mixed new object or private protected public real return string switch throw trait try unset use var void while xor yield",
    ),
    ("literal", "false null true"),
    (
        "built_in",
        "Error AppendIterator ArgumentCountError ArithmeticError ArrayIterator ArrayObject AssertionError BadFunctionCallException BadMethodCallException CachingIterator CallbackFilterIterator CompileError Countable DirectoryIterator DivisionByZeroError DomainException EmptyIterator ErrorException Exception FilesystemIterator FilterIterator GlobIterator InfiniteIterator InvalidArgumentException IteratorIterator LengthException LimitIterator LogicException MultipleIterator NoRewindIterator OutOfBoundsException OutOfRangeException OuterIterator OverflowException ParentIterator ParseError RangeException RecursiveArrayIterator RecursiveCachingIterator RecursiveCallbackFilterIterator RecursiveDirectoryIterator RecursiveFilterIterator RecursiveIterator RecursiveIteratorIterator RecursiveRegexIterator RecursiveTreeIterator RegexIterator RuntimeException SeekableIterator SplDoublyLinkedList SplFileInfo SplFileObject SplFixedArray SplHeap SplMaxHeap SplMinHeap SplObjectStorage SplObserver SplObserver SplPriorityQueue SplQueue SplStack SplSubject SplSubject SplTempFileObject TypeError UnderflowException UnexpectedValueException UnhandledMatchError ArrayAccess Closure Generator Iterator IteratorAggregate Serializable Stringable Throwable Traversable WeakReference WeakMap Directory __PHP_Incomplete_Class parent php_user_filter self static stdClass",
    ),
];
const KW_RUBY: &[(&str, &str)] = &[
    (
        "keyword",
        "and then defined module in return redo if BEGIN retry end for self when next until do begin unless END rescue else break undef not super class case require yield alias while ensure elsif or include attr_reader attr_writer attr_accessor __FILE__",
    ),
    ("built_in", "proc lambda"),
    ("literal", "true false nil"),
];
const KW_C: &[(&str, &str)] = &[
    (
        "keyword",
        "int float while private char char8_t char16_t char32_t catch import module export virtual operator sizeof dynamic_cast typedef const_cast const for static_cast union namespace unsigned long volatile static protected bool template mutable if public friend do goto auto void enum else break extern using asm case typeid wchar_t short reinterpret_cast default double register explicit signed typename try this switch continue inline delete alignas alignof constexpr consteval constinit decltype concept co_await co_return co_yield requires noexcept static_assert thread_local restrict final override atomic_bool atomic_char atomic_schar atomic_uchar atomic_short atomic_ushort atomic_int atomic_uint atomic_long atomic_ulong atomic_llong atomic_ullong new throw return and and_eq bitand bitor compl not not_eq or or_eq xor xor_eq",
    ),
    (
        "built_in",
        "std string wstring cin cout cerr clog stdin stdout stderr stringstream istringstream ostringstream auto_ptr deque list queue stack vector map set pair bitset multiset multimap unordered_set unordered_map unordered_multiset unordered_multimap priority_queue make_pair array shared_ptr abort terminate abs acos asin atan2 atan calloc ceil cosh cos exit exp fabs floor fmod fprintf fputs free frexp fscanf future isalnum isalpha iscntrl isdigit isgraph islower isprint ispunct isspace isupper isxdigit tolower toupper labs ldexp log10 log malloc realloc memchr memcmp memcpy memset modf pow printf putchar puts scanf sinh sin snprintf sprintf sqrt sscanf strcat strchr strcmp strcpy strcspn strlen strncat strncmp strncpy strpbrk strrchr strspn strstr tanh tan vfprintf vprintf vsprintf endl initializer_list unique_ptr _Bool complex _Complex imaginary _Imaginary",
    ),
    ("literal", "true false nullptr NULL"),
];
const KW_CSHARP: &[(&str, &str)] = &[
    (
        "keyword",
        "abstract as base break case class const continue do else event explicit extern finally fixed for foreach goto if implicit in interface internal is lock namespace new operator out override params private protected public readonly record ref return sealed sizeof stackalloc static struct switch this throw try typeof unchecked unsafe using virtual void volatile while add alias and ascending async await by descending equals from get global group init into join let nameof not notnull on or orderby partial remove select set unmanaged value var when where with yield",
    ),
    (
        "built_in",
        "bool byte char decimal delegate double dynamic enum float int long nint nuint object sbyte short string ulong uint ushort",
    ),
    ("literal", "default false null true"),
];
const KW_NIX: &[(&str, &str)] = &[
    ("keyword", "rec with let in inherit assert if else then"),
    ("literal", "true false or and null"),
    (
        "built_in",
        "import abort baseNameOf dirOf isNull builtins map removeAttrs throw toString derivation",
    ),
];
const KW_BASH: &[(&str, &str)] = &[
    (
        "keyword",
        "if then else elif fi for while in do done case esac function",
    ),
    ("literal", "true false"),
    (
        "built_in",
        "break cd continue eval exec exit export getopts hash pwd readonly return shift test times trap umask unset alias bind builtin caller command declare echo enable help let local logout mapfile printf read readarray source type typeset ulimit unalias set shopt autoload bg bindkey bye cap chdir clone comparguments compcall compctl compdescribe compfiles compgroups compquote comptags comptry compvalues dirs disable disown echotc echoti emulate fc fg float functions getcap getln history integer jobs kill limit log noglob popd print pushd pushln rehash sched setcap setopt stat suspend ttyctl unfunction unhash unlimit unsetopt vared wait whence where which zcompile zformat zftp zle zmodload zparseopts zprof zpty zregexparse zsocket zstyle ztcp",
    ),
];
const KW_RUST: &[(&str, &str)] = &[
    (
        "keyword",
        "abstract as async await become box break const continue crate do dyn else enum extern false final fn for if impl in let loop macro match mod move mut override priv pub ref return self Self static struct super trait true try type typeof unsafe unsized use virtual where while yield",
    ),
    ("literal", "true false Some None Ok Err"),
    (
        "built_in",
        "drop i8 i16 i32 i64 i128 isize u8 u16 u32 u64 u128 usize f32 f64 str char bool Box Option Result String Vec Copy Send Sized Sync Drop Fn FnMut FnOnce ToOwned Clone Debug PartialEq PartialOrd Eq Ord AsRef AsMut Into From Default Iterator Extend IntoIterator DoubleEndedIterator ExactSizeIterator SliceConcatExt ToString assert! assert_eq! bitflags! bytes! cfg! col! concat! concat_idents! debug_assert! debug_assert_eq! env! panic! file! format! format_args! include_bin! include_str! line! local_data_key! module_path! option_env! print! println! select! stringify! try! unimplemented! unreachable! vec! write! writeln! macro_rules! assert_ne! debug_assert_ne!",
    ),
];
const KW_SCALA: &[(&str, &str)] = &[
    ("literal", "true false null"),
    (
        "keyword",
        "type yield lazy override def with val var sealed abstract private trait object if forSome for while throw finally protected extends import final return else break new catch super class case package default try this match continue throws implicit",
    ),
];
const KW_KOTLIN: &[(&str, &str)] = &[
    (
        "keyword",
        "abstract as val var vararg get set class object open private protected public noinline crossinline dynamic final enum if else do while for when throw try catch finally import package is in fun override companion reified inline lateinit init interface annotation data sealed internal infix operator out by constructor super tailrec where const inner suspend typealias external expect actual break continue return this",
    ),
    (
        "built_in",
        "Byte Short Char Int Long Boolean Float Double Void Unit Nothing",
    ),
    ("literal", "true false null"),
];
const KW_SWIFT: &[(&str, &str)] = &[
    (
        "keyword",
        "associatedtype async await as break case catch class continue convenience default defer deinit didSet do dynamic else enum extension fallthrough fileprivate final for func get guard if import indirect infix inout internal in is lazy let mutating nonmutating open operator optional override postfix precedencegroup prefix private protocol public repeat required rethrows return set some static struct subscript super switch throws throw try typealias unowned var weak where while willSet _ #colorLiteral #column #dsohandle #else #elseif #endif #error #file #fileID #fileLiteral #filePath #function #if #imageLiteral #keyPath #line #selector #sourceLocation #warn_unqualified_access #warning",
    ),
    ("literal", "false nil true"),
];
const KW_DART: &[(&str, &str)] = &[
    (
        "keyword",
        "abstract as assert async await break case catch class const continue covariant default deferred do dynamic else enum export extends extension external factory false final finally for Function get hide if implements import in inferface is late library mixin new null on operator part required rethrow return set show static super switch sync this throw true try typedef var void while with yield",
    ),
    (
        "built_in",
        "Comparable DateTime Duration Function Iterable Iterator List Map Match Object Pattern RegExp Set Stopwatch String StringBuffer StringSink Symbol Type Uri bool double int num Element ElementList Comparable? DateTime? Duration? Function? Iterable? Iterator? List? Map? Match? Object? Pattern? RegExp? Set? Stopwatch? String? StringBuffer? StringSink? Symbol? Type? Uri? bool? double? int? num? Element? ElementList? Never Null dynamic print document querySelector querySelectorAll window",
    ),
];
const KW_GROOVY: &[(&str, &str)] = &[
    ("built_in", "this super"),
    ("literal", "true false null"),
    (
        "keyword",
        "byte short char int long boolean float double void def as in assert trait abstract static volatile transient public private protected synchronized final class interface enum if else for while switch case break default continue throw throws try catch finally implements extends new import package return instanceof",
    ),
];
const KW_PERL: &[(&str, &str)] = &[(
    "keyword",
    "abs accept alarm and atan2 bind binmode bless break caller chdir chmod chomp chop chown chr chroot close closedir connect continue cos crypt dbmclose dbmopen defined delete die do dump each else elsif endgrent endhostent endnetent endprotoent endpwent endservent eof eval exec exists exit exp fcntl fileno flock for foreach fork format formline getc getgrent getgrgid getgrnam gethostbyaddr gethostbyname gethostent getlogin getnetbyaddr getnetbyname getnetent getpeername getpgrp getpriority getprotobyname getprotobynumber getprotoent getpwent getpwnam getpwuid getservbyname getservbyport getservent getsockname getsockopt given glob gmtime goto grep gt hex if index int ioctl join keys kill last lc lcfirst length link listen local localtime log lstat lt ma map mkdir msgctl msgget msgrcv msgsnd my ne next no not oct open opendir or ord our pack package pipe pop pos print printf prototype push q qq quotemeta qw qx rand read readdir readline readlink readpipe recv redo ref rename require reset return reverse rewinddir rindex rmdir say scalar seek seekdir select semctl semget semop send setgrent sethostent setnetent setpgrp setpriority setprotoent setpwent setservent setsockopt shift shmctl shmget shmread shmwrite shutdown sin sleep socket socketpair sort splice split sprintf sqrt srand stat state study sub substr symlink syscall sysopen sysread sysseek system syswrite tell telldir tie tied time times tr truncate uc ucfirst umask undef unless unlink unpack unshift untie until use utime values vec wait waitpid wantarray warn when while write x xor y",
)];
const KW_LUA: &[(&str, &str)] = &[
    ("literal", "true false nil"),
    (
        "keyword",
        "and break do else elseif end for goto if in local not or repeat return then until while",
    ),
    (
        "built_in",
        "_G _ENV _VERSION __index __newindex __mode __call __metatable __tostring __len __gc __add __sub __mul __div __mod __pow __concat __unm __eq __lt __le assert collectgarbage dofile error getfenv getmetatable ipairs load loadfile loadstring module next pairs pcall print rawequal rawget rawset require select setfenv setmetatable tonumber tostring type unpack xpcall arg self coroutine resume yield status wrap create running debug getupvalue debug sethook getmetatable gethook setmetatable setlocal traceback setfenv getinfo setupvalue getlocal getregistry getfenv io lines write close flush open output type read stderr stdin input stdout popen tmpfile math log max acos huge ldexp kiana cos tanh pow deg tan cosh sinh random randomseed frexp ceil floor rad abs sqrt modf asin min mod fmod log10 atan2 exp sin atan os exit setlocale date getenv difftime remove time clock tmpname rename execute package preload loadlib loaded loaders cpath config path seeall string sub upper len gfind rep find match char dump gmatch reverse byte format gsub lower table setn insert getn foreachi maxn foreach concat sort remove",
    ),
];
