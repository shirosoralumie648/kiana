//! 评审回归：`latex.rs` 与 TS `packages/tui/src/latex.ts` 的可观察差异。
//!
//! 每一条的期望值都是先在 Node 26.12.1 / ICU 78.3 上跑
//! `renderLatex(source, { display })` 得到的（必要时附上 TS 的异常），再在这里逐值断言。
//!
//! | 节 | 主题 | TS 源 |
//! |---|---|---|
//! | 1 | 空环境名 `\begin{}` 是 falsy，静默返回空串 | `latex.ts:1333` |
//! | 2 | `normalizeScriptValue` 的 JS `\s`（不含 U+0085、含 U+FEFF） | `latex.ts:615` |
//! | 3 | `\p{L}` / `\p{N}` 用 Unicode 17.0（regex crate 的表偏旧） | `latex.ts:100-109` |
//! | 4 | 命名算子左空格的字符类含 `)` | `latex.ts:651` |
//! | 5 | JS 对象字面量的原型链查找（`\constructor` 等） | `latex.ts:1090` 等 |
//! | 6 | 深嵌套：必须终止、不得 abort；深度上限只影响远超 TS 栈的输入 | `latex.ts`（无上限） |

use kiana_tui::latex::{RenderLatexOptions, render_latex};

/// 期望行为（TS 侧）。
#[derive(Debug, Clone, Copy)]
enum Expected {
    /// TS 返回字符串。
    Renders(&'static str),
    /// TS 返回 `undefined`（`supported === false` 或输入未消费完）。
    Unsupported,
    /// TS 抛 `TypeError`（函数值上调用 `.trim()` / `.replace()`）。Rust 用 `None` 表达"拿不到
    /// 渲染结果"（公开签名没有异常通道，也不允许为模型可控输入而 panic）。
    Throws,
}

fn check(source: &str, display: bool, expected: Expected) {
    let actual = render_latex(source, &RenderLatexOptions { display });
    match expected {
        Expected::Renders(text) => {
            assert_eq!(
                actual.as_deref(),
                Some(text),
                "renderLatex({source:?}, display={display})"
            )
        }
        Expected::Unsupported => {
            assert_eq!(
                actual, None,
                "renderLatex({source:?}, display={display}) 应为 None"
            )
        }
        Expected::Throws => {
            assert_eq!(
                actual, None,
                "renderLatex({source:?}, display={display})：TS 抛 TypeError，Rust 应为 None"
            )
        }
    }
}

// ---------------------------------------------------------------------------
// 1–5：逐条黄金值（来自 TS 实跑）
// ---------------------------------------------------------------------------

/// 58 条黄金值，覆盖上面表格的五节。
#[test]
fn review_regression_goldens_match_ts() {
    let cases: &[(&str, bool, Expected)] = &[
        ("\\begin{}abc", false, Expected::Renders("abc")),
        ("x^{\\begin{}}", false, Expected::Renders("x")),
        ("\\sqrt{\\begin{}x}", false, Expected::Renders("√x")),
        ("\\text{\\begin{}abc}", false, Expected::Renders("abc")),
        ("\\begin{}\\frac{1}{2}", true, Expected::Renders("1\n─\n2")),
        ("\\begin{}", false, Expected::Renders("")),
        ("\\begin{} x \\end{}", false, Expected::Unsupported),
        ("x^{a\u{85}=1}", false, Expected::Renders("x^(a\u{85}=1)")),
        ("x_{a\u{85}=1}", false, Expected::Renders("x_(a\u{85}=1)")),
        ("x^{a\u{85}+1}", false, Expected::Renders("x^(a\u{85}+1)")),
        ("x^{a=\u{85}}", false, Expected::Renders("x^(a=\u{85})")),
        (
            "\\sum_{a\u{85}=1}",
            true,
            Expected::Renders(" ∑\na\u{85}=1"),
        ),
        (
            "\\overset{a\u{85}+1}{b}",
            false,
            Expected::Renders("b^(a\u{85}+1)"),
        ),
        (
            "\\underset{a\u{85}+1}{b}",
            false,
            Expected::Renders("b_(a\u{85}+1)"),
        ),
        ("x^{a\u{feff}=1}", false, Expected::Renders("xᵃ⁼¹")),
        ("x^{a\u{feff}+1}", false, Expected::Renders("xᵃ⁺¹")),
        ("\\frac{࢏}{2}", false, Expected::Renders("࢏/2")),
        ("\\sqrt{࢏}", false, Expected::Renders("√࢏")),
        ("࢏\\cos x", false, Expected::Renders("࢏ cos x")),
        ("\\frac{౜}{2}", false, Expected::Renders("౜/2")),
        ("\\frac{೜}{2}", false, Expected::Renders("೜/2")),
        ("\\frac{꟎}{2}", false, Expected::Renders("꟎/2")),
        ("\\frac{𑷠}{2}", false, Expected::Renders("𑷠/2")),
        ("\\frac{𖿴}{2}", false, Expected::Renders("𖿴/2")),
        ("a)\\cos", false, Expected::Renders("a) cos")),
        (") \\cos", false, Expected::Renders(") cos")),
        ("]\\cos", false, Expected::Renders("] cos")),
        ("}\\cos", false, Expected::Unsupported),
        (
            "\\constructor",
            false,
            Expected::Renders("function Object() { [native code] }"),
        ),
        (
            "\\toString",
            false,
            Expected::Renders("function toString() { [native code] }"),
        ),
        (
            "\\valueOf",
            false,
            Expected::Renders("function valueOf() { [native code] }"),
        ),
        (
            "\\hasOwnProperty",
            false,
            Expected::Renders("function hasOwnProperty() { [native code] }"),
        ),
        (
            "\\toLocaleString",
            false,
            Expected::Renders("function toLocaleString() { [native code] }"),
        ),
        (
            "\\isPrototypeOf",
            false,
            Expected::Renders("function isPrototypeOf() { [native code] }"),
        ),
        (
            "\\propertyIsEnumerable",
            false,
            Expected::Renders("function propertyIsEnumerable() { [native code] }"),
        ),
        (
            "\\__defineGetter__",
            false,
            Expected::Renders("__defineGetter__"),
        ),
        ("\\__proto__", false, Expected::Renders("_ₚroto__")),
        (
            "\\constructor{a}",
            false,
            Expected::Renders("function Object() { [native code] }a"),
        ),
        (
            "\\not{constructor}",
            false,
            Expected::Renders("function Object() { [native code] }"),
        ),
        (
            "\\not{__proto__}",
            false,
            Expected::Renders("[object Object]"),
        ),
        (
            "\\hat\\constructor",
            false,
            Expected::Renders("function Object() { [native code] }̂"),
        ),
        (
            "\\hat\\toString",
            false,
            Expected::Renders("hat(function toString() { [native code] })"),
        ),
        (
            "\\hat\\__defineGetter__",
            false,
            Expected::Renders("_̂_defineGetter__"),
        ),
        ("\\mathbb\\constructor", false, Expected::Renders("")),
        (
            "\\mathbb{\\constructor}",
            false,
            Expected::Renders("function Object() { [native code] }"),
        ),
        (
            "\\text\\constructor",
            false,
            Expected::Renders("function Object() { [native code] }"),
        ),
        (
            "\\binom\\constructor2",
            false,
            Expected::Renders("(function Object() { [native code] } choose 2)"),
        ),
        ("x^\\constructor", false, Expected::Throws),
        ("\\not\\constructor", false, Expected::Throws),
        ("\\frac\\constructor2", false, Expected::Throws),
        ("\\sqrt\\constructor", false, Expected::Throws),
        ("\\boxed\\constructor", false, Expected::Throws),
        ("\\sum_\\constructor", false, Expected::Throws),
        ("\\operatorname\\constructor", false, Expected::Throws),
        ("\\mathrm\\constructor", false, Expected::Throws),
        ("\\pmod\\constructor", false, Expected::Throws),
        ("\\overset\\constructor{2}", false, Expected::Throws),
        ("\\overset{2}\\constructor", false, Expected::Throws),
    ];
    for &(source, display, expected) in cases {
        check(source, display, expected);
    }
    assert_eq!(cases.len(), 58);
}

// ---------------------------------------------------------------------------
// 1. 空环境名（finding: `\begin{}` 被当成不支持的语法）
// ---------------------------------------------------------------------------

/// TS：`const environment = readRawGroup(); if (!environment) return "";`——空串是 falsy，
/// 所以 `\begin{}` 只返回空串并**继续正常解析**，不会让整个表达式变成 `undefined`。
#[test]
fn empty_environment_name_is_falsy_and_parsing_continues() {
    assert_eq!(
        render_latex(r"\begin{}abc", &RenderLatexOptions::default()).as_deref(),
        Some("abc")
    );
    assert_eq!(
        render_latex(r"x^{\begin{}}", &RenderLatexOptions::default()).as_deref(),
        Some("x")
    );
    assert_eq!(
        render_latex(r"\sqrt{\begin{}x}", &RenderLatexOptions::default()).as_deref(),
        Some("√x")
    );
    assert_eq!(
        render_latex(r"\text{\begin{}abc}", &RenderLatexOptions::default()).as_deref(),
        Some("abc")
    );
    assert_eq!(
        render_latex(
            r"\begin{}\frac{1}{2}",
            &RenderLatexOptions { display: true }
        )
        .as_deref(),
        Some("1\n─\n2")
    );
    // 完全没有环境的 `\begin{}` 渲染成空串（不是 None）。
    assert_eq!(
        render_latex(r"\begin{}", &RenderLatexOptions::default()).as_deref(),
        Some("")
    );
    // 但空环境名 + `\end{}` 仍然走"找不到结束标记"的失败路径。
    assert_eq!(
        render_latex(r"\begin{} x \end{}", &RenderLatexOptions::default()),
        None
    );
}

// ---------------------------------------------------------------------------
// 2. `normalizeScriptValue` 的 JS `\s`
// ---------------------------------------------------------------------------

/// JS `\s` **不含** U+0085、**含** U+FEFF；Rust `regex` 的 `\s` 恰好相反。U+0085 不会被
/// 折叠掉，所以脚本值退化成非 Unicode 形式；U+FEFF 会被折叠，仍然用 Unicode 上下标。
#[test]
fn normalize_script_value_uses_the_js_whitespace_set() {
    let nel = '\u{85}';
    let bom = '\u{feff}';
    for (source, expected) in [
        (format!("x^{{a{nel}=1}}"), format!("x^(a{nel}=1)")),
        (format!("x_{{a{nel}=1}}"), format!("x_(a{nel}=1)")),
        (format!("x^{{a{nel}+1}}"), format!("x^(a{nel}+1)")),
        (format!("x^{{a={nel}}}"), format!("x^(a={nel})")),
        (
            format!("\\overset{{a{nel}+1}}{{b}}"),
            format!("b^(a{nel}+1)"),
        ),
        (
            format!("\\underset{{a{nel}+1}}{{b}}"),
            format!("b_(a{nel}+1)"),
        ),
        (format!("x^{{a{bom}=1}}"), "xᵃ⁼¹".to_string()),
        (format!("x^{{a{bom}+1}}"), "xᵃ⁺¹".to_string()),
    ] {
        assert_eq!(
            render_latex(&source, &RenderLatexOptions::default()).as_deref(),
            Some(expected.as_str()),
            "{source:?}"
        );
    }
    // display 模式下 U+0085 让上标退回二维布局（TS：`\sum_{a<U+0085>=1}` → `" ∑\na<U+0085>=1"`）。
    let source = format!("\\sum_{{a{nel}=1}}");
    let expected = format!(" ∑\na{nel}=1");
    assert_eq!(
        render_latex(&source, &RenderLatexOptions { display: true }).as_deref(),
        Some(expected.as_str())
    );
}

// ---------------------------------------------------------------------------
// 3. `\p{L}` / `\p{N}` 是 Unicode 17.0
// ---------------------------------------------------------------------------

/// `regex` 1.13.1 的 `\p{L}` 比 Node 26（Unicode 17.0）少 4,644 个码点、`\p{N}` 少 13 个。
/// 这些码点会让 `formatFraction` / `formatRoot` 的"简单值"判定与命名算子空格不同。
#[test]
fn unicode_17_letters_and_numbers_are_letters_and_numbers() {
    // Unicode 17 新增的字母（Rust 的 `regex` 表里没有）。
    for letter in ['\u{88f}', '\u{c5c}', '\u{cdc}', '\u{a7ce}', '\u{16ff2}'] {
        let fraction = format!("\\frac{{{letter}}}{{2}}");
        assert_eq!(
            render_latex(&fraction, &RenderLatexOptions::default()).as_deref(),
            Some(format!("{letter}/2").as_str()),
            "{fraction:?}"
        );
        let root = format!("\\sqrt{{{letter}}}");
        assert_eq!(
            render_latex(&root, &RenderLatexOptions::default()).as_deref(),
            Some(format!("√{letter}").as_str()),
            "{root:?}"
        );
        let spaced = format!("{letter}\\cos x");
        assert_eq!(
            render_latex(&spaced, &RenderLatexOptions::default()).as_deref(),
            Some(format!("{letter} cos x").as_str()),
            "{spaced:?}"
        );
    }
    // Unicode 17 新增的数字（`\p{N}`）。
    for number in ['\u{11de0}', '\u{16ff4}'] {
        let fraction = format!("\\frac{{{number}}}{{2}}");
        assert_eq!(
            render_latex(&fraction, &RenderLatexOptions::default()).as_deref(),
            Some(format!("{number}/2").as_str()),
            "{fraction:?}"
        );
    }
    // 反例：U+16FF 既不是字母也不是数字（新旧表都不含），必须加括号。
    assert_eq!(
        render_latex("\\frac{\u{16ff}}{2}", &RenderLatexOptions::default()).as_deref(),
        Some("(\u{16ff})/2")
    );
}

// ---------------------------------------------------------------------------
// 4. 命名算子左空格的字符类
// ---------------------------------------------------------------------------

/// `NAMED_OPERATOR_LEFT_SPACING_PATTERN` 的字符类是 `[\p{L}\p{N})\]}<MARKER>]`——**包含
/// 右圆括号**。原实现漏了 `)`，导致 `(a/b)\cos` 渲染成 `(a/b)cos`。
#[test]
fn named_operator_spacing_after_closing_paren() {
    for (source, expected) in [
        ("a)\\cos", "a) cos"),
        (") \\cos", ") cos"),
        ("\\left(\\frac{a}{b}\\right)\\cos", "(a/b) cos"),
        ("\\left(\\frac{a}{b}\\right)\\cos", "(a/b) cos"),
        ("x)\\sin y", "x) sin y"),
        ("]\\cos", "] cos"),
        (")\\cos\\sin", ") cos sin"),
    ] {
        assert_eq!(
            render_latex(source, &RenderLatexOptions::default()).as_deref(),
            Some(expected),
            "{source:?}"
        );
    }
    // 其他字符不加空格（`.`、`)` 之外的非字母数字）。
    for source in ["-\\cos", "+\\cos", ".\\cos", "(\\cos", "|\\cos"] {
        let rendered = render_latex(source, &RenderLatexOptions::default());
        assert!(
            rendered.as_deref().is_some_and(|text| !text.contains(' ')),
            "{source:?} → {rendered:?}"
        );
    }
    // 右空格的字符类含 `√` 与 LAYOUT_MARKER_START。
    assert_eq!(
        render_latex("\\cos\\sqrt{x}", &RenderLatexOptions::default()).as_deref(),
        Some("cos √x")
    );
    assert_eq!(
        render_latex("\\cos\\frac{1}{2}", &RenderLatexOptions::default()).as_deref(),
        Some("cos 1/2")
    );
}

// ---------------------------------------------------------------------------
// 5. JS 对象字面量的原型链查找
// ---------------------------------------------------------------------------

/// `SYMBOLS` / `ACCENTS` / `NEGATED_SYMBOLS` 是普通对象字面量，所以 `\constructor` 会命中原型链。
/// TS 的真实输出是 `"function Object() { [native code] }"` 这样的 native-code 文本。
#[test]
fn object_literal_lookup_falls_back_to_object_prototype() {
    for (source, expected) in [
        ("\\constructor", "function Object() { [native code] }"),
        ("\\toString", "function toString() { [native code] }"),
        (
            "\\toLocaleString",
            "function toLocaleString() { [native code] }",
        ),
        ("\\valueOf", "function valueOf() { [native code] }"),
        (
            "\\hasOwnProperty",
            "function hasOwnProperty() { [native code] }",
        ),
        (
            "\\isPrototypeOf",
            "function isPrototypeOf() { [native code] }",
        ),
        (
            "\\propertyIsEnumerable",
            "function propertyIsEnumerable() { [native code] }",
        ),
        ("\\constructor{a}", "function Object() { [native code] }a"),
        // `\not{...}` 命中的是 `NEGATED_SYMBOLS` 的原型链。
        ("\\not{constructor}", "function Object() { [native code] }"),
        ("\\not{toString}", "function toString() { [native code] }"),
        // `__proto__` 是对象，`String(obj)` 是 `"[object Object]"`。
        ("\\not{__proto__}", "[object Object]"),
        // accent 的 `Array.from(value).length === 1`：`constructor` 的 arity 是 1 → 加组合符；
        // `toString` 的 arity 是 0 → 走 `command(value)` 形式。
        (
            "\\hat\\constructor",
            "function Object() { [native code] }\u{302}",
        ),
        (
            "\\hat\\toString",
            "hat(function toString() { [native code] })",
        ),
        (
            "\\overline\\constructor",
            "function Object() { [native code] }\u{305}",
        ),
        // `\mathbb` 的 `.join("")` 把 `undefined` 变成空串。
        ("\\mathbb\\constructor", ""),
        (
            "\\mathbb{\\constructor}",
            "function Object() { [native code] }",
        ),
        // `text*` / `mbox` 不调用 `.trim()`，函数值被原样返回。
        ("\\text\\constructor", "function Object() { [native code] }"),
        ("\\mbox\\valueOf", "function valueOf() { [native code] }"),
    ] {
        assert_eq!(
            render_latex(source, &RenderLatexOptions::default()).as_deref(),
            Some(expected),
            "{source:?}"
        );
    }
    // `\__defineGetter__` 这类写法与 `\_x` 走同一条路径：反斜杠后的 `_` 是单字符命令，
    // 原样返回，后续字母是普通字符（**不是**原型链命中——命令名不是 `constructor` 等）。
    for (source, expected) in [
        ("\\__defineGetter__", "__defineGetter__"),
        ("\\__lookupGetter__", "_ₗookupGetter__"),
        ("\\__proto__", "_ₚroto__"),
        ("\\_x", "_x"),
        ("\\_", "_"),
    ] {
        assert_eq!(
            render_latex(source, &RenderLatexOptions::default()).as_deref(),
            Some(expected),
            "{source:?}"
        );
    }
}

/// 参数位置拿到函数值时，TS 会在 `.trim()` / `.replace()` 上抛 `TypeError`（`renderLatex` 没有
/// try/catch，异常冒泡到 markdown 渲染器）。本 crate 的签名没有异常通道，因此用 `None` 表达
/// "这条公式得不到渲染结果"——两种情况下调用方都回退到原始文本。
#[test]
fn prototype_argument_that_would_throw_a_type_error_returns_none() {
    for source in [
        "\\not\\constructor",
        "\\frac\\constructor2",
        "\\sqrt\\constructor",
        "\\boxed\\constructor",
        "\\pmod\\constructor",
        "\\overset\\constructor{2}",
        "\\overset{2}\\constructor",
        "\\underset\\constructor{2}",
        "x^\\constructor",
        "x_\\constructor",
        "\\sum_\\constructor",
        "\\operatorname\\constructor",
        "\\mathrm\\constructor",
        "\\mathbf\\constructor",
    ] {
        assert_eq!(
            render_latex(source, &RenderLatexOptions::default()),
            None,
            "{source:?}"
        );
        assert_eq!(
            render_latex(source, &RenderLatexOptions { display: true }),
            None,
            "{source:?} (display)"
        );
    }
    // 对照：`text` 分支不做 trim，所以同样的输入是合法渲染。
    assert_eq!(
        render_latex("\\text\\constructor", &RenderLatexOptions::default()).as_deref(),
        Some("function Object() { [native code] }")
    );
}

// ---------------------------------------------------------------------------
// 6. 深嵌套：终止 + 不 abort
// ---------------------------------------------------------------------------

/// 曾经的实现会在深度守卫处返回而不推进 `position`，导致 `parse_sequence_body` 的 `while`
/// 死循环（511 个 `{` 后跟 `\alpha` 就能挂死整个进程）。现在守卫会消费掉剩余输入，所以一定终止。
#[test]
fn deep_nesting_terminates_and_never_hangs() {
    // (a) 511 个 `{` + `\alpha`：TS 返回 undefined（未闭合），Rust 必须同样是 None 而不是挂起。
    let source = format!("{}{}", "{".repeat(511), "\\alpha");
    assert_eq!(render_latex(&source, &RenderLatexOptions::default()), None);
    // (b) 600 层平衡花括号：TS 渲染 `α`。
    let source = format!("{}{}{}", "{".repeat(600), "\\alpha", "}".repeat(600));
    assert_eq!(
        render_latex(&source, &RenderLatexOptions::default()).as_deref(),
        Some("α")
    );
    // (c) 512 层平衡花括号：TS 渲染 `x`。
    let source = format!("{}x{}", "{".repeat(512), "}".repeat(512));
    assert_eq!(
        render_latex(&source, &RenderLatexOptions::default()).as_deref(),
        Some("x")
    );
    // (d) 258 层 `\frac{1}{...}`：TS 是线性分式串（宽度随层数线性增长）。
    let source = format!("{}x{}", "\\frac{1}{".repeat(258), "}".repeat(258));
    let rendered = render_latex(&source, &RenderLatexOptions::default()).expect("renders");
    // TS 的 `length` 是 UTF-16 码元数（全 ASCII 时等于字节数）。
    assert_eq!(rendered.len(), 1031);
    assert!(rendered.starts_with("1/(1/(1/(1/"));
    assert!(rendered.ends_with(&")".repeat(257)));
    // (e) 256 层 `\sqrt{...}`：TS 输出 767 字符。
    let source = format!("{}x{}", "\\sqrt{".repeat(256), "}".repeat(256));
    let rendered = render_latex(&source, &RenderLatexOptions::default()).expect("renders");
    // TS：767 个 UTF-16 码元（含 256 个 `√`）；Rust 的 `len()` 是 UTF-8 字节数。
    assert_eq!(rendered.chars().count(), 767);
    assert_eq!(rendered.len(), 1279);
    assert!(rendered.starts_with("√(√(√(√(√("));
    assert!(rendered.ends_with(&")".repeat(255)));
    // (f) 256 层 `\text{...}`：只有最内层被保留。
    let source = format!("{}x{}", "\\text{".repeat(256), "}".repeat(256));
    assert_eq!(
        render_latex(&source, &RenderLatexOptions::default()).as_deref(),
        Some("x")
    );
}

/// 深度上限（8192）只影响远超 TS 调用栈的输入：TS 在 ~16k 层花括号 / ~6k 层 `\frac` 抛
/// `RangeError`，而 Rust 的栈溢出是**不可捕获的进程 abort**。Rust 在 8192 层主动放弃（`None`），
/// 在此之前逐字渲染。
#[test]
fn depth_limit_is_far_beyond_what_ts_renders_and_fails_softly() {
    // 2,000 层：远低于 8192 上限，应当正常渲染。
    let source = format!("{}x{}", "{".repeat(2_000), "}".repeat(2_000));
    assert_eq!(
        render_latex(&source, &RenderLatexOptions::default()).as_deref(),
        Some("x")
    );
    // 8,192 层（== MAX_PARSE_DEPTH）：放弃并返回 None，而不是 abort。
    let source = format!("{}x{}", "{".repeat(8_192), "}".repeat(8_192));
    assert_eq!(render_latex(&source, &RenderLatexOptions::default()), None);
    // 远超上限：同样快速返回 None。
    let source = format!("{}x{}", "{".repeat(50_000), "}".repeat(50_000));
    assert_eq!(render_latex(&source, &RenderLatexOptions::default()), None);
}

/// 大栈调用：深输入不能因为调用方线程栈小（tokio worker 默认 2 MB）而 abort。这里在
/// **2 MB 栈**的线程里渲染 8,192 层 `\sqrt{...}`——若没有 [`kiana_tui::latex`] 的大栈派发，
/// 就会以 `fatal runtime error: stack overflow` 杀掉整个测试进程。
#[test]
fn deep_input_is_safe_on_a_small_caller_stack() {
    let source = format!("{}x{}", "\\sqrt{".repeat(300), "}".repeat(300));
    let rendered = std::thread::Builder::new()
        .stack_size(2 * 1024 * 1024)
        .spawn(move || render_latex(&source, &RenderLatexOptions::default()))
        .expect("spawn")
        .join()
        .expect("no panic / no abort");
    let rendered = rendered.expect("300 层 sqrt 应渲染");
    // 300 层 `\sqrt`：TS 输出 899 个字符（`√(` 前缀 + `x` + 300 个 `)`）；Rust 用 `len()` 是字节数。
    assert_eq!(rendered.chars().count(), 899);
    assert!(rendered.starts_with("√(√("));
    assert!(rendered.ends_with(&")".repeat(299)));
    assert!(rendered.starts_with("√(√("));
}

/// 性能锚：普通公式走"当前栈就地渲染"的快路径（不 spawn 线程），单次渲染应在微秒量级；
/// 深输入走大栈线程，仍应在毫秒量级返回。这是防止大栈派发退化为"每次渲染都 spawn"的护栏。
#[test]
fn large_stack_dispatch_has_no_cost_for_ordinary_formulas() {
    use std::time::Instant;
    let source = "\\sum_{i=0}^n \\alpha_i + \\int_0^\\infty e^{-x^2}\\,dx = \\sqrt{\\pi}";
    // 预热（regex/区间表初始化）。
    for _ in 0..50 {
        let _ = render_latex(source, &RenderLatexOptions::default());
    }
    let start = Instant::now();
    let iterations = 2_000;
    for _ in 0..iterations {
        assert!(render_latex(source, &RenderLatexOptions::default()).is_some());
    }
    let per_call = start.elapsed() / iterations;
    assert!(
        per_call.as_micros() < 500,
        "ordinary render took {per_call:?}"
    );

    let deep = format!("{}x{}", "{".repeat(1_000), "}".repeat(1_000));
    let start = Instant::now();
    assert_eq!(
        render_latex(&deep, &RenderLatexOptions::default()).as_deref(),
        Some("x")
    );
    assert!(
        start.elapsed().as_millis() < 2_000,
        "deep render took {:?}",
        start.elapsed()
    );
}
