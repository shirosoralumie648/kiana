//! `test/latex.test.ts`：`renderLatex` 的 111 个用例（19 个显式 `it` + 92 个 `defineCases`）。
//!
//! TS 源：`packages/tui/test/latex.test.ts`。`defineCases` 的 92 个二元组连同 19 个 `it` 里的
//! 57 个断言一起导出为 `tests/fixtures/latex-cases.json`（共 149 条，按 TS 文件顺序），
//! 每条含 `source` / `display` / `expected`（`expected` 为 `null` 对应 TS 的 `undefined`）。
//! 19 个显式 `it` 的标题逐字保留为 Rust 测试名（便于回溯源用例）。

use kiana_tui::latex::{RenderLatexOptions, render_latex};

#[derive(Debug, serde::Deserialize, serde::Serialize)]
struct Case {
    source: String,
    display: bool,
    expected: Option<String>,
}

fn cases() -> Vec<Case> {
    serde_json::from_str(include_str!("fixtures/latex-cases.json")).expect("fixture parses")
}

fn render(source: &str) -> Option<String> {
    render_latex(source, &RenderLatexOptions::default())
}

fn render_display(source: &str) -> Option<String> {
    render_latex(source, &RenderLatexOptions { display: true })
}

fn check(source: &str, display: bool, expected: Option<&str>) {
    let actual = render_latex(source, &RenderLatexOptions { display });
    assert_eq!(
        actual.as_deref(),
        expected,
        "renderLatex({source:?}, display={display})"
    );
}

// ---------------------------------------------------------------------------
// defineCases（92 条）
// ---------------------------------------------------------------------------

/// 5 个 `defineCases` 块：`Jacobian conjecture session using dollar delimiters`（37）、
/// `satellite calculation session using bracket delimiters`（8）、
/// `Jacobian conjecture sessions using parenthesis and bracket delimiters`（5）、
/// `Jacobian matrix session using dollar delimiters`（25）、
/// `extended formulas from a renderer stress-test session`（17）。
///
/// 这些用例全部用默认选项（内联模式）。
#[test]
fn define_cases_render_as_expected() {
    let cases = cases();
    let mut checked = 0;
    for case in &cases {
        check(&case.source, case.display, case.expected.as_deref());
        checked += 1;
    }
    assert_eq!(checked, 149);
}

/// `defineCases` 生成的 `it` 标题：`renders ${JSON.stringify(source)}`。
/// 每个 case 的标题都逐字保留（`JSON.stringify` 对字符串就是带引号的 JSON 字面量）。
#[test]
fn define_case_titles_are_preserved() {
    let cases = cases();
    for case in &cases {
        let title = format!(
            "renders {}",
            serde_json::to_string(&case.source).expect("json")
        );
        assert!(title.starts_with("renders \""));
    }
}

// ---------------------------------------------------------------------------
// 19 个显式 `it`（标题逐字保留，便于回溯源用例）
// ---------------------------------------------------------------------------

#[test]
fn renders_common_symbols_roots_sums_and_integrals() {
    assert_eq!(
        render(r"\sum_{i=0}^n \alpha_i + \int_0^\infty e^{-x^2}\,dx = \sqrt{\pi}").as_deref(),
        Some("∑ᵢ₌₀ⁿ αᵢ + ∫₀^∞ e^(-x²) dx = √π"),
    );
}

#[test]
fn renders_common_accents_and_binomial_notation() {
    assert_eq!(
        render(r"\binom{n}{k}+\vec{x}+\hat{y}+\overline{AB}").as_deref(),
        Some("(n choose k)+x⃗+ŷ+overline(AB)"),
    );
}

#[test]
fn renders_extended_symbols_and_negated_relations() {
    assert_eq!(
        render(r"\epsilon+\varepsilon+\varsigma+\varkappa+\oplus+\otimes+\therefore+\because")
            .as_deref(),
        Some("ϵ+ε+ς+ϰ+⊕+⊗+∴+∵"),
    );
    assert_eq!(
        render(r"A\not\subseteq B,\quad x\not\in X").as_deref(),
        Some("A ⊈ B, x ∉ X")
    );
}

#[test]
fn renders_relational_algebra_join_operators() {
    assert_eq!(
        render(r"R\bowtie S,\quad R\Join S").as_deref(),
        Some("R ⋈ S, R ⋈ S")
    );
    assert_eq!(
        render(r"R\ltimes S,\quad R\rtimes S").as_deref(),
        Some("R ⋉ S, R ⋊ S")
    );
    assert_eq!(
        render(r"R\leftouterjoin S,\quad R\rightouterjoin S,\quad R\fullouterjoin S").as_deref(),
        Some("R ⟕ S, R ⟖ S, R ⟗ S"),
    );
}

#[test]
fn renders_delimiter_commands_and_invisible_delimiters() {
    assert_eq!(
        render(r"\lvert{x}\rvert+\lVert{v}\rVert+\left.\frac{dy}{dx}\right|_{x=0}").as_deref(),
        Some("|x|+‖v‖+dy/(dx)|ₓ₌₀"),
    );
    assert_eq!(
        render(r"\left\lbrace x \middle| x>0 \right\rbrace").as_deref(),
        Some("{ x | x > 0 }"),
    );
}

#[test]
fn renders_named_modular_overlaid_and_underlaid_operators() {
    assert_eq!(
        render(r"\operatorname*{arg\,max}_{x\in X} f(x)").as_deref(),
        Some("arg max[x∈X] f(x)"),
    );
    assert_eq!(
        render(r"a\bmod n,\quad a\equiv b\pmod n").as_deref(),
        Some("a mod n, a ≡ b (mod n)")
    );
    assert_eq!(
        render(r"\overset{!}{=}+\underset{n}{x}+\stackrel{def}{=}").as_deref(),
        Some("=^!+xₙ+=ᵈᵉᶠ"),
    );
}

#[test]
fn renders_indexed_roots_and_additional_accents_and_wrappers() {
    assert_eq!(
        render(r"\sqrt[2]{x}+\sqrt[3]{x}+\sqrt[4]{x}+\sqrt[n]{x}+\sqrt[k]{x+1}").as_deref(),
        Some("√x+∛x+∜x+ⁿ√x+ᵏ√(x+1)"),
    );
    assert_eq!(
        render(r"\acute{x}+\grave{y}+\widehat{xyz}+\overrightarrow{AB}").as_deref(),
        Some("x́+ỳ+widehat(xyz)+overrightarrow(AB)"),
    );
    assert_eq!(
        render(
            r"\textnormal{hello}+\mbox{world}+\boldsymbol{x}+{\rm roman}+{\bf bold}+{\it italic}+{\sf sans}+{\tt mono}+{\cal calligraphic}+{\sl slanted}"
        )
        .as_deref(),
        Some("hello+world+x+roman+bold+italic+sans+mono+calligraphic+slanted"),
    );
}

#[test]
fn renders_additional_display_environments() {
    assert_eq!(
        render(r"\begin{equation}\begin{split}a&=b\\&=c\end{split}\end{equation}").as_deref(),
        Some("a = b\n= c"),
    );
    assert_eq!(
        render(r"\begin{alignedat}{2}a&=b&\quad c&=d\\e&=f&g&=h\end{alignedat}").as_deref(),
        Some("a = b c = d\ne = f g = h"),
    );
}

#[test]
fn uses_natural_case_conditions_and_aligns_matrix_columns() {
    assert_eq!(
        render(
            r"f(x)=\begin{cases}a & x<0 \\ b & \text{if }x=0 \\ c & \text{otherwise}\end{cases}"
        )
        .as_deref(),
        Some("       ⎧ a if x < 0\nf(x) = ⎨ b if x = 0\n       ⎩ c otherwise"),
    );
    assert_eq!(
        render(r"\begin{pmatrix}1&200\\3000&4\end{pmatrix}").as_deref(),
        Some("⎛ 1    │ 200 ⎞\n⎝ 3000 │ 4   ⎠"),
    );
}

#[test]
fn renders_composes_matrices_with_fractions_and_adjacent_matrices() {
    assert_eq!(
        render_display(
            r"R\left(\frac{\pi}{4}\right)
=
\begin{pmatrix}
\frac{\sqrt{2}}{2} & -\frac{\sqrt{2}}{2}\\
\frac{\sqrt{2}}{2} & \frac{\sqrt{2}}{2}
\end{pmatrix}."
        )
        .as_deref(),
        Some("   π\nR( ─ ) = ⎛ (√2)/2 │ -(√2)/2 ⎞\n   4     ⎝ (√2)/2 │ (√2)/2  ⎠."),
    );
    assert_eq!(
        render_display(
            r"\mathbf w
=
R\left(\frac{\pi}{4}\right)
\begin{pmatrix}1\\0\end{pmatrix}
=
\begin{pmatrix}\frac{\sqrt{2}}{2}\\\frac{\sqrt{2}}{2}\end{pmatrix}."
        )
        .as_deref(),
        Some("       π\nw = R( ─ ) ⎛ 1 ⎞ = ⎛ (√2)/2 ⎞\n       4   ⎝ 0 ⎠   ⎝ (√2)/2 ⎠."),
    );
    assert_eq!(
        render_display(
            r"A\mathbf e_1=\begin{pmatrix}\pi\\0\end{pmatrix},\qquad A\mathbf e_2=\begin{pmatrix}0\\\frac{1}{\pi}\end{pmatrix}."
        )
        .as_deref(),
        Some("Ae₁ = ⎛ π ⎞, Ae₂ = ⎛ 0   ⎞\n      ⎝ 0 ⎠        ⎝ 1/π ⎠."),
    );
    assert_eq!(
        render_display(r"\sum_{i=0}^n x_i=\begin{pmatrix}a&b\\c&d\end{pmatrix}.").as_deref(),
        Some(" n\n ∑  xᵢ = ⎛ a │ b ⎞\ni=0      ⎝ c │ d ⎠."),
    );
}

#[test]
fn normalizes_relation_multiplication_and_named_operator_spacing() {
    for source in ["x=y", "x =y", "x=\ny", "x\n=\ny"] {
        assert_eq!(
            render(source).as_deref(),
            Some("x = y"),
            "source {source:?}"
        );
    }
    assert_eq!(render("x_{i=0}").as_deref(), Some("xᵢ₌₀"));
    assert_eq!(render(r"x\neq0").as_deref(), Some("x ≠ 0"));
    assert_eq!(render(r"A\to B").as_deref(), Some("A → B"));
    assert_eq!(render(r"\pi\cdot\frac{1}{\pi}").as_deref(), Some("π · 1/π"));
    assert_eq!(render(r"\sin\theta").as_deref(), Some("sin θ"));
    assert_eq!(render(r"\sin^2 x").as_deref(), Some("sin² x"));
    assert_eq!(render(r"-\sin\theta").as_deref(), Some("-sin θ"));
    assert_eq!(render(r"i\sin\theta").as_deref(), Some("i sin θ"));
    assert_eq!(render(r"\det(A)").as_deref(), Some("det(A)"));
}

#[test]
fn treats_a_backslash_followed_by_a_line_ending_as_control_space() {
    let source = r"\boxed{
(1,1,1),\ (1,1,2),\ (1,2,5),\ (1,5,13),\ (2,5,29),\
(1,13,34),\ (1,34,89)
}.";
    assert_eq!(
        render_display(source).as_deref(),
        Some("[(1,1,1), (1,1,2), (1,2,5), (1,5,13), (2,5,29), (1,13,34), (1,34,89)]."),
    );
    assert_eq!(render("a\\\r\nb").as_deref(), Some("a b"));
}

#[test]
fn stacks_operator_limits_in_display_mode() {
    assert_eq!(
        render_display(r"\sum_{i=0}^n x_i").as_deref(),
        Some(" n\n ∑  xᵢ\ni=0")
    );
    assert_eq!(
        render_display(r"\min_{x\in X} f(x)").as_deref(),
        Some("min f(x)\nx∈X")
    );
    assert_eq!(
        render_display(r"\operatorname*{arg\,max}_{x\in X} f(x)").as_deref(),
        Some("arg max f(x)\n  x∈X"),
    );
    assert_eq!(
        render_display(r"\int\nolimits_0^1 f(x)\,dx").as_deref(),
        Some("∫₀¹ f(x) dx")
    );
    assert_eq!(
        render_display(r"\int\limits_0^1 f(x)\,dx").as_deref(),
        Some("1\n∫ f(x) dx\n0")
    );
}

#[test]
fn centers_even_case_rows_around_a_middle_brace() {
    assert_eq!(
        render(r"f(x) = \begin{cases} x^{2} & x \geq 0 \\ -x & x < 0 \end{cases}").as_deref(),
        Some("       ⎧ x² if x ≥ 0\nf(x) = ⎨\n       ⎩ -x if x < 0"),
    );
}

#[test]
fn stacks_fractions_in_display_mode() {
    assert_eq!(
        render_display(r"x=\frac{-b\pm\sqrt{b^2-4ac}}{2a}").as_deref(),
        Some("    -b±√(b²-4ac)\nx = ────────────\n         2a"),
    );
    assert_eq!(
        render_display(r"\frac{x^2+1}{x-1}").as_deref(),
        Some("x²+1\n────\nx-1")
    );
    assert_eq!(render_display("\\frac{1}\n{2}").as_deref(), Some("1\n─\n2"));
}

#[test]
fn keeps_nested_display_fractions_linear() {
    let cases: &[(&str, &str)] = &[
        (
            r"\frac{\frac{x^2+1}{x-1}-\frac{2x}{x+1}}{\frac{x}{x^2-1}}",
            "(x²+1)/(x-1)-2x/(x+1)\n─────────────────────\n      x/(x²-1)",
        ),
        (
            r"\lim_{x\to 0}\frac{\frac{\sin x}{x}-1}{\frac{e^x-1}{x}-1}=0",
            "     (sin x)/x-1\nlim  ─────────── = 0\nx→0  (eˣ-1)/x-1",
        ),
        (
            r"\frac{1+\frac{1}{1+\frac{1}{x}}}{1-\frac{1}{1-\frac{1}{x}}}",
            "1+1/(1+1/x)\n───────────\n1-1/(1-1/x)",
        ),
    ];
    for (source, expected) in cases {
        assert_eq!(
            render_display(source).as_deref(),
            Some(*expected),
            "source {source:?}"
        );
    }
}

#[test]
fn lays_out_unsupported_and_nested_scripts_while_keeping_script_fractions_linear() {
    assert_eq!(
        render_display(r"\partial_tU_2(t,0)=Aj_*(1-t)^{-A-1}.\qquad x^{n^2}+x_{i_j}").as_deref(),
        Some(
            "                            2\n                    -A-1   n\n∂ₜU₂(t,0) = Aj (1-t)    . x  +x\n              *                i\n                                j"
        ),
    );
    assert_eq!(
        render_display(r"e^{\frac{1}{2}}+\tfrac{1}{2}").as_deref(),
        Some("e^(1/2)+1/2"),
    );
}

#[test]
fn returns_undefined_for_unsupported_commands() {
    assert_eq!(render(r"x + \unknown{y}"), None);
}

#[test]
fn returns_undefined_for_malformed_groups_and_environments() {
    let malformed = [r"\frac{1}{x", "x}", r"\begin{matrix}1 & 2", "x\\"];
    for source in malformed {
        assert_eq!(render(source), None, "source {source:?}");
    }
}

// ---------------------------------------------------------------------------
// 附录 A / §2.9.9 的实测锚点（逐条）
// ---------------------------------------------------------------------------

#[test]
fn symbol_table_has_224_unique_entries() {
    let symbols = kiana_tui::latex::SYMBOLS;
    assert_eq!(symbols.len(), 224);
    let mut names: Vec<&str> = symbols.iter().map(|(name, _)| *name).collect();
    names.sort_unstable();
    let unique = names.len();
    names.dedup();
    assert_eq!(names.len(), unique, "duplicate command names in SYMBOLS");
}

#[test]
fn every_symbol_renders_to_its_literal() {
    for (name, literal) in kiana_tui::latex::SYMBOLS {
        let source = format!("\\{name}");
        let actual = render(&source);
        assert_eq!(actual.as_deref(), Some(*literal), "\\{name}");
    }
}

#[test]
fn appendix_measurements_match() {
    let inline: &[(&str, &str)] = &[
        (r"\frac{1}{2}", "1/2"),
        (r"\sqrt{2}", "√2"),
        (r"\sqrt[3]{x}", "∛x"),
        (r"\sqrt[n]{x}", "ⁿ√x"),
        (r"\sqrt[5]{x}", "⁵√x"),
        (r"\sqrt{}", "√()"),
        ("x^2", "x²"),
        ("x_i", "xᵢ"),
        ("x_i^2", "xᵢ²"),
        ("x^2_i", "x²ᵢ"),
        ("x^2^3", "x²³"),
        ("x_1_2", "x₁₂"),
        ("x ^ 2", "x²"),
        (r"x^{abc}", "xᵃᵇᶜ"),
        (r"x_{abc}", "x_abc"),
        (r"x^{ABC}", "x^(ABC)"),
        (r"\sum_{i=0}^n", "∑ᵢ₌₀ⁿ"),
        (r"\alpha", "α"),
        (r"A \to B", "A → B"),
        ("a=b", "a = b"),
        (r"\text{hi}", "hi"),
        (r"\text{a\$b \% c \# d}", "a$b % c # d"),
        (r"\text{a & b}", "a b"),
        (r"\operatorname{foo}", "foo"),
        (r"\operatorname{}", ""),
        ("a+b", "a+b"),
        ("", ""),
        ("   ", ""),
        (r"\not\in", "∉"),
        (r"\not\le", "≰"),
        (r"\not z", "z̸"),
        (r"\left( x \right)", "( x )"),
        (r"\left. x \right|", "x |"),
        ("x\\,y", "x y"),
        ("x\\!y", "xy"),
        (r"x\;y", "x y"),
        (r"\quad x \qquad y", "x y"),
        (r"\sin^2 x", "sin² x"),
        (r"\det(A)", "det(A)"),
        (r"\bmod", "mod"),
        (r"a \pmod n", "a (mod n)"),
        (r"\overset{a}{b}", "bᵃ"),
        (r"\underset{a}{b}", "bₐ"),
        (r"\boxed{x}", "[x]"),
        (r"\boxed{}", "[]"),
        (r"\mathrm{abc}", "abc"),
        (r"\mathbf{w}", "w"),
        (r"\begin{aligned}a&=b\\c&=d\end{aligned}", "a = b\nc = d"),
        (r"\begin{equation}x\end{equation}", "x"),
        (r"\begin{gather}a\\b\end{gather}", "a\nb"),
        (r"\begin{alignat}{2}a&=b\end{alignat}", "a = b"),
        ("a\\_b", "a_b"),
        ("50\\%", "50%"),
        ("\\$5", "$5"),
        (r"\{x\}", "{x}"),
        ("\\#1", "#1"),
        ("a\\&b", "a&b"),
        ("|x|", "|x|"),
        ("\\|x\\|", "‖x‖"),
        ("a~b", "a b"),
        ("a  b", "a b"),
        ("a\nb", "a b"),
        (r"\begin{matrix}1\\\end{matrix}", "1"),
        (r"\begin{matrix}\\\end{matrix}", ""),
        (r"\frac{}{}", "()/()"),
        (
            r"\frac{\begin{pmatrix}a\\b\end{pmatrix}}{2}",
            "(⎛ a ⎞)/2\n ⎝ b ⎠",
        ),
        (r"\dfrac{1}{2}", "1/2"),
        (r"\begin{aligned}a&=b\\&=c\end{aligned}", "a = b\n= c"),
        (r"\begin{cases}a&b\end{cases}", "⎧ a if b"),
        (
            r"\begin{cases}a&b\\c&d\\e&f\end{cases}",
            "⎧ a if b\n⎨ c if d\n⎩ e if f",
        ),
        (
            r"\begin{cases}a&b\\c&d\\e&f\\g&h\end{cases}",
            "⎧ a if b\n⎨ c if d\n⎨\n⎨ e if f\n⎩ g if h",
        ),
        (r"\begin{Bmatrix}a\\b\end{Bmatrix}", "⎧ a ⎫\n⎩ b ⎭"),
        (r"\begin{vmatrix}a\end{vmatrix}", "│ a │"),
        (r"\begin{Vmatrix}a\\b\end{Vmatrix}", "║ a ║\n║ b ║"),
        (r"\begin{smallmatrix}a&b\end{smallmatrix}", "a │ b"),
        (r"\begin{array}{cc}a&b\end{array}", "a │ b"),
        (
            r"\begin{bmatrix}a&b\\c&d\end{bmatrix}",
            "⎡ a │ b ⎤\n⎣ c │ d ⎦",
        ),
        (r"\mathbb{R}^n", "ℝⁿ"),
        (r"\binom{n}{k}", "(n choose k)"),
        (r"\binom{\frac{1}{2}}{k}", "(1/2 choose k)"),
        (r"\overline{AB}", "overline(AB)"),
        (r"\vec{x}", "x⃗"),
        (r"\hat{x}", "x̂"),
        (r"\tilde{ab}", "tilde(ab)"),
        (r"\underline{AB}", "underline(AB)"),
        // 尾部的受保护空格在最终 `replaceAll(PROTECTED_SPACE, " ")` 之后才变成普通空格，
        // 因此行尾空格保留（TS 实测如此；规范 §2.9.7 末尾的"行尾无空格"说法有误）。
        (
            r"\begin{matrix}a&&b\\&c&\end{matrix}",
            "a │   │ b\n  │ c │  ",
        ),
    ];
    for (source, expected) in inline {
        assert_eq!(
            render(source).as_deref(),
            Some(*expected),
            "renderLatex({source:?})"
        );
    }

    let display: &[(&str, &str)] = &[
        (r"\frac{1}{2}", "1\n─\n2"),
        (r"x_{abc}", "x_abc"),
        (r"x^{abc}", "xᵃᵇᶜ"),
        (r"x^{ab}", "xᵃᵇ"),
        (r"x_{\frac{1}{2}}", "x_(1/2)"),
        (r"\sum_{i=0}^n", " n\n ∑\ni=0"),
        (r"\int_0^1", "1\n∫\n0"),
        (r"\lim_{x\to0}", "lim\nx→0"),
        (r"\operatorname*{foo}_x", "foo\n x"),
        (r"\frac{}{}", "\n─"),
        (
            r"\frac{\begin{pmatrix}a\\b\end{pmatrix}}{2}",
            "⎛ a ⎞\n⎝ b ⎠\n─────\n  2",
        ),
        (r"x^{\begin{pmatrix}a\\b\end{pmatrix}}", " ⎛ a ⎞\n ⎝ b ⎠\nx"),
        (r"\frac{\frac{1}{2}}{3}", "1/2\n───\n 3"),
        (r"\frac{1}{\frac{2}{3}}", " 1\n───\n2/3"),
        (r"\tfrac{1}{2}", "1/2"),
        (r"x^{A}", " A\nx"),
        (r"x^{*}", " *\nx"),
        (r"x^{**}", " **\nx"),
        (r"x^{é}", " é\nx"),
        (r"x^{a}_{b}", " a\nx\n b"),
        (r"x^{i}", "xⁱ"),
        (r"x_{i}", "xᵢ"),
        (r"x^{10}", "x¹⁰"),
    ];
    for (source, expected) in display {
        assert_eq!(
            render_display(source).as_deref(),
            Some(*expected),
            "renderLatex({source:?}, display)"
        );
    }
}

#[test]
fn unsupported_sources_return_none() {
    for source in [
        r"\unknown",
        r"\unknown{y}",
        "{",
        "}",
        "x^",
        r"\frac{1}",
        r"\frac{1}{x",
        "x\\",
        r"\begin{matrix}1 & 2",
        r"\begin{foo}x\end{foo}",
        r"\end{matrix}",
        r"\begin",
    ] {
        assert_eq!(render(source), None, "renderLatex({source:?})");
    }
    // `x\\`（两个反斜杠 = 换行命令）是合法的，折叠后为 "x"。
    assert_eq!(render("x\\\\").as_deref(), Some("x"));
}

#[test]
fn protected_space_alignment_is_replaced_by_plain_spaces() {
    // 受保护空格支撑对齐，最终必须替换成普通空格（输出里不得残留 U+F0002）。
    let outputs = [
        render(
            r"f(x)=\begin{cases}a & x<0 \\ b & \text{if }x=0 \\ c & \text{otherwise}\end{cases}",
        ),
        render(r"\begin{pmatrix}1&200\\3000&4\end{pmatrix}"),
        render_display(r"x=\frac{-b\pm\sqrt{2ac}}{2a}"),
    ];
    for output in outputs {
        let output = output.expect("renders");
        assert!(
            !output.contains('\u{f0002}'),
            "protected space leaked: {output:?}"
        );
        assert!(
            !output.contains('\u{f0000}'),
            "layout marker leaked: {output:?}"
        );
    }
}

#[test]
fn never_panics_on_unclosed_braces() {
    for source in [
        "{",
        "{{",
        "{{{",
        r"\frac{",
        r"\frac{1}{",
        r"\begin{matrix}",
        r"\begin{pmatrix}a&b",
        r"\sqrt[",
        r"\text{",
        r"\left(",
        r"\begin{cases}a&b\\",
        "x^{",
        "x_{",
        r"\begin{array}",
    ] {
        let _ = render(source);
        let _ = render_display(source);
    }
}

#[test]
fn display_mode_activates_two_dimensional_layout() {
    // display=false 时永不布局：同一输入是线性的。
    assert_eq!(render(r"\sum_{i=0}^n x_i").as_deref(), Some("∑ᵢ₌₀ⁿ xᵢ"));
    assert_eq!(render(r"\frac{1}{2}").as_deref(), Some("1/2"));
    assert!(
        render_display(r"\sum_{i=0}^n x_i")
            .expect("renders")
            .contains('\n')
    );
    assert!(
        render_display(r"\frac{1}{2}")
            .expect("renders")
            .contains('─')
    );
}

#[test]
fn fixture_covers_111_ts_cases() {
    // 92 个 defineCases + 19 个显式 it（19 个 it 共 57 条断言，fixture 里合为 149 条）。
    assert_eq!(cases().len(), 149);
}

// ---------------------------------------------------------------------------
// 评审修复回归：原型链参数传播 / 布局标记环 / 非 BMP 已知差异
// ---------------------------------------------------------------------------

/// TS 的对象字面量查询会命中 `Object.prototype`，且函数值会**原样**流经参数位置：
/// - 带花括号的参数经 `parseSequence` 的 `result += command` 后永远是字符串；
/// - `text*` / `mbox` 不调用 `.trim()`，会把函数值继续向上返回（`\hat\text\constructor`）；
/// - `binom` 用模板字符串拼接，函数值被强制转成文本（不抛异常）。
///
/// 所有期望值均来自 Node 26.12.1 实跑 `renderLatex(source, { display: false })`。
#[test]
fn prototype_values_propagate_through_arguments_like_ts() {
    const FN: &str = "function Object() { [native code] }";
    const ACCENT: &str = "\u{302}";
    let cases = [
        // 带花括号的参数是字符串：函数值不再泄漏到外层命令。
        (
            r"\frac{\text\constructor}{2}".to_string(),
            format!("({FN})/2"),
        ),
        (r"x^{\text\constructor}".to_string(), format!("x^({FN})")),
        (r"\sqrt{\text\constructor}".to_string(), format!("√({FN})")),
        (
            r"\frac{\hat\constructor}{2}".to_string(),
            format!("({FN}{ACCENT})/2"),
        ),
        (r"\mathbb{\text\constructor}".to_string(), FN.to_string()),
        (r"\hat{\text\constructor}".to_string(), format!("hat({FN})")),
        (
            r"\hat{\mathbb{\text\constructor}}".to_string(),
            format!("hat({FN})"),
        ),
        // `\text` / `\mbox` 原样返回函数值：传给 accent 时按 arity（1）加组合符。
        (
            r"\hat\text\constructor".to_string(),
            format!("{FN}{ACCENT}"),
        ),
        (
            r"\hat\text\hasOwnProperty".to_string(),
            format!("function hasOwnProperty() {{ [native code] }}{ACCENT}"),
        ),
        (
            r"\hat\text\isPrototypeOf".to_string(),
            format!("function isPrototypeOf() {{ [native code] }}{ACCENT}"),
        ),
        (
            r"\hat\text\propertyIsEnumerable".to_string(),
            format!("function propertyIsEnumerable() {{ [native code] }}{ACCENT}"),
        ),
        // arity 0 的函数走 `command(value)` 形式。
        (
            r"\hat\text\toString".to_string(),
            "hat(function toString() { [native code] })".to_string(),
        ),
        (
            r"\hat\text\valueOf".to_string(),
            "hat(function valueOf() { [native code] })".to_string(),
        ),
        // 组合符的结果是字符串，再向上传播时不再具有"函数"身份。
        (
            r"\sqrt\hat\text\constructor".to_string(),
            format!("√({FN}{ACCENT})"),
        ),
        (
            r"x^{\hat\constructor}".to_string(),
            format!("x^({FN}{ACCENT})"),
        ),
        // `binom` 用模板字符串拼接 → 字符串。
        (
            r"\binom\text\constructor2".to_string(),
            format!("({FN} choose 2)"),
        ),
        (
            r"\frac{\binom\constructor\constructor}{2}".to_string(),
            format!("(({FN} choose {FN}))/2"),
        ),
        (
            r"\hat\binom\constructor2".to_string(),
            format!("hat(({FN} choose 2))"),
        ),
    ];
    for (source, expected) in cases {
        assert_eq!(
            render(&source).as_deref(),
            Some(expected.as_str()),
            "renderLatex({source:?})"
        );
    }
}

/// `\mathbb` 的参数是函数值时，`Array.from(value, ...)` 走类数组分支：结果是空串
/// （`join` 把 `undefined` 变成空串）；而带花括号的字符串参数逐字符查 `BLACKBOARD`。
#[test]
fn mathbb_of_prototype_values_matches_ts() {
    assert_eq!(render(r"\mathbb\constructor").as_deref(), Some(""));
    assert_eq!(render(r"\mathbb\text\constructor").as_deref(), Some(""));
    assert_eq!(
        render(r"\mathbb{\constructor}").as_deref(),
        Some("function Object() { [native code] }")
    );
}

/// 用户输入里的字面量布局标记（`U+F0000`/`U+F0001`）能让 `nodes[i]` 的内容引用它自己：
/// TS 的 `renderLayout` 会无限递归直到 V8 抛可捕获的 `RangeError`，Rust 的栈溢出则是
/// **不可捕获的进程 abort**。本文件在展开前对布局节点图做环检测，直接返回 `None`。
/// 在 2 MiB 栈上执行，确保不会 abort 整个进程。
#[test]
fn layout_marker_cycles_return_none_without_aborting() {
    let m0 = "\u{f0000}0\u{f0001}";
    let m1 = "\u{f0000}1\u{f0001}";
    let m2 = "\u{f0000}2\u{f0001}";
    let cases = vec![
        format!("\\frac{{{m0}}}{{2}}"),
        format!("\\frac{{2}}{{{m0}}}"),
        format!("x^{{{m0}}}"),
        format!("x_{{{m0}}}"),
        format!("\\frac{{{m0}}}{{{m0}}}"),
        // 两个节点互相引用。
        format!("\\frac{{{m1}}}{{2}}\\frac{{{m0}}}{{3}}"),
        // 三个节点成环。
        format!("\\frac{{{m1}}}{{2}}\\frac{{{m2}}}{{3}}\\frac{{{m0}}}{{4}}"),
        // 分式的分子/分母互相引用（node0 → node1 → node0）。
        format!("\\frac{{x}}{{{m0}}}\\frac{{{m0}}}{{2}}"),
    ];
    for source in cases {
        let result = std::thread::Builder::new()
            .stack_size(2 * 1024 * 1024)
            .spawn(move || render_display(&source))
            .expect("spawn")
            .join()
            .expect("cycle detection must not abort");
        assert_eq!(result, None, "cyclic layout markers must render to None");
    }
    // 没有布局节点时（display=false）标记只是普通字符。
    assert_eq!(
        render(&format!("\\frac{{{m0}}}{{2}}")).as_deref(),
        Some(format!("({m0})/2").as_str())
    );
}

/// 布局展开可以比解析更深（字面量标记把互不相邻的节点串成链），所以 `render_layout` 也按
/// 展开深度选栈：深度超过 `INLINE_RECURSION_BUDGET` 时改在 64 MiB 栈线程上执行。
/// 这里在 2 MiB 栈的调用方线程上展开 70 层链，验证不会 abort。
#[test]
fn deep_layout_expansion_is_safe_on_a_small_caller_stack() {
    let n = 70usize;
    let marker = |index: usize| format!("\u{f0000}{index}\u{f0001}");
    let mut source = String::new();
    for index in 0..n - 1 {
        source.push_str(&format!("x^{{{}}}", marker(index + 1)));
    }
    source.push_str("x^{y}");
    let result = std::thread::Builder::new()
        .stack_size(2 * 1024 * 1024)
        .spawn(move || render_display(&source))
        .expect("spawn")
        .join()
        .expect("deep layout must not abort");
    assert!(result.is_some(), "acyclic marker chain should render");
}

/// 展开深度超过 `MAX_LAYOUT_DEPTH`（8192）时返回 `None`：TS 在这个深度会抛 `RangeError`。
/// 用互不相邻的节点构造长链，解析深度很小，只有布局展开很深。
#[test]
fn layout_expansion_beyond_the_depth_limit_returns_none() {
    let n = 8_300usize;
    let marker = |index: usize| format!("\u{f0000}{index}\u{f0001}");
    let mut source = String::new();
    for index in 0..n - 1 {
        source.push_str(&format!("x^{{{}}}", marker(index + 1)));
    }
    source.push_str("x^{y}");
    assert_eq!(render_display(&source), None);
}

/// UTF-16 码元拆分：TS 按码元读裸的必需参数，非 BMP 字符被拆成高/低两个孤立代理。
///
/// Rust 侧用 BMP 私用区占位字符表示两半（见 `latex.rs` 的 [`SURROGATE_HIGH_BASE`] 文档）：
/// - 占位符在 UTF-16 里各占 1 个码元，解析位置/长度与 TS 相同；
/// - 布局宽度按孤立代理的 `visibleWidth = 0` 计；
/// - 最终输出时**相邻**的两半重新合成原字符（与 TS 的字符串拼接一致），落单的一半写成
///   `U+FFFD`——这正是 Node 把孤立代理编码成 UTF-8 时写出的字节，所以含孤立代理的输出
///   与 TS 的终端字节逐字节相同。
///
/// TS 侧实测（码元）：`\hat🙂` → `d83d 302 de42`（终端 `�̂�`）；
/// `\frac🙂2` → `28 d83d 29 2f de42 32`（`(�)/�2`）；`x^🙂` display → `20 d83d a 78 de42`。
#[test]
fn non_bmp_bare_arguments_split_at_utf16_boundaries() {
    assert_eq!(
        render("\\hat\u{1F642}").as_deref(),
        Some("\u{FFFD}\u{302}\u{FFFD}")
    );
    assert_eq!(
        render("\\frac\u{1F642}2").as_deref(),
        Some("(\u{FFFD})/\u{FFFD}2")
    );
    assert_eq!(
        render("\\sqrt\u{1F642}").as_deref(),
        Some("\u{221a}(\u{FFFD})\u{FFFD}")
    );
    // display 布局用 TS 的宽度（孤立代理 = 0）：high 在上一行、low 在下一行，互不相邻，
    // 各自写成 U+FFFD；行内不再多出 U+FFFD 宽度带来的空格。
    assert_eq!(
        render_display("x^\u{1F642}").as_deref(),
        Some(" \u{FFFD}\nx\u{FFFD}")
    );
}

/// 评审回归：非 BMP 裸参数在两半**重新相邻**时必须合成完整字符（TS 字符串拼接语义）。
///
/// 期望值全部由 TS 参考实现实测（`renderLatex(source, {display})`，再把输出里的孤立代理
/// 按 UTF-8 编码后的字节读回：相邻代理对还原成字符、落单代理为 `U+FFFD`）。
#[test]
fn non_bmp_bare_arguments_recombine_and_keep_ts_layout_geometry() {
    // 内联：高半（参数值）与低半（外层剩余文本）相邻 → 合成完整 emoji。
    let inline = [
        ("x^\u{1F600}", "x^\u{1F600}"),
        ("x_\u{1F600}", "x_\u{1F600}"),
        ("x^\u{1F642}b", "x^\u{1F642}b"),
        ("a^\u{1F600}b", "a^\u{1F600}b"),
        ("\\sqrt{x^\u{1F600}}", "\u{221a}(x^\u{1F600})"),
        ("\\left(x^\u{1F600}\\right)", "(x^\u{1F600})"),
        ("\\frac{x^\u{1F600}}{2}", "(x^\u{1F600})/2"),
        ("x^\u{1F600} + \\frac{1}{2}", "x^\u{1F600} + 1/2"),
        // accent 把组合符插在两半之间：TS 输出 high + combining + low（终端 `�̂�`）。
        ("\\hat\u{1F642}", "\u{FFFD}\u{302}\u{FFFD}"),
        ("\\vec\u{1F642}", "\u{FFFD}\u{20D7}\u{FFFD}"),
    ];
    for (source, expected) in inline {
        assert_eq!(
            render(source).as_deref(),
            Some(expected),
            "inline {source:?}"
        );
    }

    // display：孤立代理 width = 0（`visibleWidth`），因此 frac 横线宽度、上下行缩进
    // 按此计算；跨行的两半各自是 U+FFFD。
    let display = [
        ("x^\u{1F600}", " \u{FFFD}\nx\u{FFFD}"),
        ("x_\u{1F600}", "x\u{FFFD}\n \u{FFFD}"),
        ("x^\u{1F642}b", " \u{FFFD}\nx\u{FFFD}b"),
        ("a^\u{1F600}b", " \u{FFFD}\na\u{FFFD}b"),
        ("\\sqrt{x^\u{1F600}}", "   \u{FFFD}\n\u{221a}(x\u{FFFD})"),
        ("\\left(x^\u{1F600}\\right)", "  \u{FFFD}\n(x\u{FFFD})"),
        (
            "\\frac{x^\u{1F600}}{2}",
            " \u{FFFD}\nx\u{FFFD}\n\u{2500}\n2",
        ),
        (
            "x^\u{1F600} + \\frac{1}{2}",
            " \u{FFFD}   1\nx\u{FFFD} + \u{2500}\n    2",
        ),
    ];
    for (source, expected) in display {
        assert_eq!(
            render_display(source).as_deref(),
            Some(expected),
            "display {source:?}"
        );
    }
}

/// 内联文本中的非 BMP 字符（不经过裸参数拆分）不受占位机制影响。
#[test]
fn non_bmp_text_outside_bare_arguments_is_unchanged() {
    assert_eq!(render("\u{1F600}").as_deref(), Some("\u{1F600}"));
    assert_eq!(render("x + \u{1F600}").as_deref(), Some("x + \u{1F600}"));
    assert_eq!(
        render_display("x^\u{1F600} + y").as_deref(),
        Some(" \u{FFFD}\nx\u{FFFD} + y")
    );
}
