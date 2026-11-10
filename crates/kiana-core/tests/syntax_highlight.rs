//! `packages/coding-agent/test/syntax-highlight.test.ts` —— `describe("syntax highlight renderer")`。
//!
//! 第二个 describe（`theme syntax highlighting`）属于 `kiana-theme`，只在
//! `crates/kiana-theme/tests/` 移植；本文件的 grammar 用例覆盖它依赖的观察点。

use std::sync::Arc;

use kiana_core::syntax_highlight::{
    HighlightFormatter, HighlightOptions, HighlightTheme, eager_languages, highlight,
    load_all_highlight_languages, render_highlighted_html, supports_language,
};

fn formatter<F>(f: F) -> HighlightFormatter
where
    F: Fn(&str) -> String + Send + Sync + 'static,
{
    Arc::new(f)
}

fn theme<const N: usize>(pairs: [(&str, HighlightFormatter); N]) -> HighlightTheme {
    pairs
        .into_iter()
        .map(|(name, formatter)| (name.to_string(), formatter))
        .collect()
}

fn marker(name: &'static str) -> HighlightFormatter {
    formatter(move |text| format!("[{name}:{text}]"))
}

fn highlight_with(code: &str, language: &str, theme: HighlightTheme) -> String {
    highlight(
        code,
        &HighlightOptions {
            language: Some(language.to_string()),
            ignore_illegals: true,
            theme,
            ..HighlightOptions::default()
        },
    )
}

#[tokio::test]
async fn loads_the_twenty_most_common_languages_at_startup_and_defers_the_rest() {
    let eager_languages = [
        "python",
        "java",
        "go",
        "javascript",
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
    let eager_languages_loaded_at_startup =
        eager_languages.iter().all(|name| supports_language(name));
    let uncommon_language_loaded_at_startup = supports_language("ada");
    assert!(eager_languages_loaded_at_startup);
    assert!(!uncommon_language_loaded_at_startup);
    // TS 里 `html`/`xml`/`diff`/`plaintext` 都不在 eagerLanguages 中，只有
    // `loadAllHighlightLanguages()` 的动态 import 完成后 `hljs.getLanguage` 才认识它们。
    for deferred in [
        "html",
        "xml",
        "svg",
        "diff",
        "patch",
        "plaintext",
        "text",
        "txt",
    ] {
        assert!(
            !supports_language(deferred),
            "{deferred} must stay unknown until load_all_highlight_languages()"
        );
    }
    load_all_highlight_languages().await;
    assert!(supports_language("ada"));
    for deferred in [
        "html",
        "xml",
        "svg",
        "diff",
        "patch",
        "plaintext",
        "text",
        "txt",
    ] {
        assert!(supports_language(deferred), "{deferred} after loadAll");
    }
}

#[test]
fn renders_highlighted_spans_with_the_provided_theme() {
    let rendered = render_highlighted_html(
        r#"<span class="hljs-keyword">const</span> value"#,
        &theme([("keyword", marker("keyword"))]),
    );
    assert_eq!(rendered, "[keyword:const] value");
}

#[test]
fn decodes_html_entities_emitted_by_highlight_js() {
    let rendered = render_highlighted_html(
        "&lt;tag attr=&quot;value&quot;&gt;&amp;#x41;&#65;&lt;/tag&gt;",
        &HighlightTheme::new(),
    );
    assert_eq!(rendered, "<tag attr=\"value\">&#x41;A</tag>");
}

#[test]
fn inherits_parent_formatting_for_unmapped_nested_scopes() {
    let interpolation = format!("${}{}", "{", "x}");
    let rendered = render_highlighted_html(
        &format!(
            r#"<span class="hljs-string">a<span class="hljs-subst">{interpolation}</span>b</span>"#
        ),
        &theme([("string", marker("string"))]),
    );
    assert_eq!(
        rendered,
        format!("[string:a][string:{interpolation}][string:b]")
    );
}

#[test]
fn keeps_parent_formatting_across_unscoped_nested_spans() {
    let rendered = render_highlighted_html(
        r#"<span class="hljs-string">a<span class="language-xml">b</span>c</span>"#,
        &theme([("string", marker("string"))]),
    );
    assert_eq!(rendered, "[string:a][string:b][string:c]");
}

#[test]
fn highlights_code_through_highlight_js() {
    assert!(supports_language("typescript"));
    let rendered = highlight(
        "const value = 1",
        &HighlightOptions {
            language: Some("typescript".to_string()),
            ignore_illegals: true,
            theme: theme([("keyword", marker("keyword")), ("number", marker("number"))]),
            ..HighlightOptions::default()
        },
    );
    assert!(rendered.contains("[keyword:const]"));
    assert!(rendered.contains("[number:1]"));
}

// ---------------------------------------------------------------------------
// renderer edge cases
// ---------------------------------------------------------------------------

#[test]
fn leaves_empty_lines_unformatted() {
    let rendered = render_highlighted_html(
        "<span class=\"hljs-string\">a\n\nb</span>",
        &theme([("string", marker("s"))]),
    );
    assert_eq!(rendered, "[s:a]\n\n[s:b]");
}

#[test]
fn resolves_scope_by_dot_dash_prefix_and_default() {
    let rendered = render_highlighted_html(
        r#"<span class="hljs-string.regexp">a</span><span class="hljs-meta-keyword">b</span><span class="hljs-unknown">c</span>"#,
        &theme([
            ("string", marker("string")),
            ("meta", marker("meta")),
            ("default", marker("default")),
        ]),
    );
    assert_eq!(rendered, "[string:a][meta:b][default:c]");
}

#[test]
fn ignores_tags_that_are_not_span_openers() {
    let rendered = render_highlighted_html("<spanner>x</spanner>", &HighlightTheme::new());
    assert_eq!(rendered, "<spanner>x</spanner>");
}

#[test]
fn handles_span_openers_without_a_class() {
    let rendered = render_highlighted_html(r#"<span>a</span>"#, &theme([("default", marker("d"))]));
    assert_eq!(rendered, "[d:a]");
}

#[test]
fn decodes_entities_at_utf16_indices() {
    let rendered = render_highlighted_html("😀&amp;🙂&#65;", &HighlightTheme::new());
    assert_eq!(rendered, "😀&🙂A");
}

#[test]
fn leaves_unknown_and_overlong_entities_untouched() {
    let rendered = render_highlighted_html("&nbsp;&amp&#123456789012345;", &HighlightTheme::new());
    assert_eq!(rendered, "&nbsp;&amp&#123456789012345;");
}

#[test]
fn formats_text_after_unclosed_span_tag_verbatim() {
    let rendered = render_highlighted_html("<span class", &HighlightTheme::new());
    assert_eq!(rendered, "<span class");
}

#[test]
fn formats_across_adjacent_spans() {
    let rendered = render_highlighted_html(
        r#"<span class="hljs-keyword">a</span><span class="hljs-number">b</span>"#,
        &theme([("keyword", marker("k")), ("number", marker("n"))]),
    );
    assert_eq!(rendered, "[k:a][n:b]");
}

// ---------------------------------------------------------------------------
// 注册表 / 语言目录
// ---------------------------------------------------------------------------

#[test]
fn eager_languages_lists_twenty_one_names() {
    assert_eq!(eager_languages().len(), 21);
    assert_eq!(eager_languages()[0], "python");
    assert_eq!(eager_languages()[20], "lua");
    assert!(eager_languages().iter().all(|name| supports_language(name)));
}

#[test]
fn supports_language_accepts_aliases_and_case() {
    assert!(supports_language("PYTHON"));
    assert!(supports_language("py"));
    assert!(supports_language("typescript"));
    assert!(supports_language("ts"));
    // `html`/`xml`/`diff`（及其别名）只有 `load_all_highlight_languages()` 之后才可见，
    // 由上面的异步用例覆盖；这里只断言急切语言与其别名。
    assert!(supports_language("javascript"));
    assert!(supports_language("js"));
    assert!(supports_language("cpp"));
    assert!(supports_language("c++"));
    assert!(!supports_language("not-a-language"));
    assert!(!supports_language(""));
}

// ---------------------------------------------------------------------------
// grammar 观察点（kiana-theme 的 `highlightCode` 依赖）
// ---------------------------------------------------------------------------

#[test]
fn colors_each_line_of_python_docstrings_independently() {
    let code = "\"\"\"\nline one\n\nline two\n\"\"\"\nafter";
    let rendered = highlight_with(code, "python", theme([("string", marker("string"))]));
    assert_eq!(
        rendered,
        "[string:\"\"\"]\n[string:line one]\n\n[string:line two]\n[string:\"\"\"]\nafter"
    );
}

#[test]
fn marks_python_decorators_as_meta() {
    let rendered = highlight_with("@decorator", "python", theme([("meta", marker("muted"))]));
    assert_eq!(rendered, "[muted:@decorator]");
}

#[test]
fn marks_javascript_regexp_literals() {
    let rendered = highlight_with(
        "const re = /foo+/gi;",
        "javascript",
        theme([("regexp", marker("string"))]),
    );
    assert_eq!(rendered, "const re = [string:/foo+/gi];");
}

#[test]
fn colors_diff_additions_and_deletions() {
    let rendered = highlight_with(
        "-old\n+new\n",
        "diff",
        theme([
            ("addition", marker("added")),
            ("deletion", marker("removed")),
        ]),
    );
    assert_eq!(rendered, "[removed:-old]\n[added:+new]\n");
}

#[test]
fn marks_html_tag_names() {
    let rendered = highlight_with("<div></div>", "html", theme([("name", marker("keyword"))]));
    assert_eq!(rendered, "<[keyword:div]></[keyword:div]>");
}

#[test]
fn colors_json_attribute_names_and_literals() {
    let rendered = highlight_with(
        r#"{"a": 1, "b": true}"#,
        "json",
        theme([
            ("attr", marker("attr")),
            ("number", marker("number")),
            ("literal", marker("literal")),
        ]),
    );
    assert_eq!(
        rendered,
        "{[attr:\"a\"]: [number:1], [attr:\"b\"]: [literal:true]}"
    );
}

#[test]
fn highlights_declarations_with_title_and_params() {
    let rendered = highlight_with(
        "def foo(a, b=1):\n    return a  # hi\n",
        "python",
        theme([
            ("keyword", marker("k")),
            ("title", marker("t")),
            ("params", marker("p")),
            ("number", marker("n")),
            ("comment", marker("c")),
        ]),
    );
    assert_eq!(
        rendered,
        "[k:def] [t:foo]([p:a, b=][n:1]):\n    [k:return] a  [c:# hi]\n"
    );
}

#[test]
fn highlights_c_enum_struct_and_union_class_headers() {
    // c.js 的 CLASS 规则（`beginKeywords: 'enum class struct union'`）与 cpp.js 相同；
    // `struct` 只经由该模式成为关键字。此前 C 的 class mode 未接线。
    let rendered = highlight_with(
        "struct Foo { int x; };\nstruct Foo *p;",
        "c",
        theme([
            ("keyword", marker("k")),
            ("class", marker("c")),
            ("title", marker("t")),
        ]),
    );
    assert_eq!(
        rendered,
        "[k:struct][c: ][t:Foo][c: {] [k:int] x; };\n[k:struct][c: ][t:Foo][c: *][t:p][c:;]"
    );
}

#[test]
fn highlights_java_record_headers_as_class_params_and_title() {
    // java.js 的 record 模式：class span + keyword `record` + title + params（含括号）。
    let rendered = highlight_with(
        "record R(int x) implements I {}",
        "java",
        theme([
            ("keyword", marker("k")),
            ("class", marker("c")),
            ("title", marker("t")),
            ("params", marker("p")),
        ]),
    );
    assert_eq!(
        rendered,
        "[k:record][c: ][t:R][p:(][k:int][p: x)][c: implements I ]{}"
    );
    // 没有参数列表时不进入 record 模式（hljs 的 begin 要求 `record\s+IDENT\s*\(`）。
    assert_eq!(
        highlight_with("record R;", "java", theme([("keyword", marker("k"))])),
        "record R;"
    );
}

#[test]
fn highlights_csharp_record_headers_with_title_mode() {
    // csharp.js 的 record 模式没有 className；头部里的每个标识符都是 title。
    let rendered = highlight_with(
        "record R(System.Int32 X);",
        "csharp",
        theme([("keyword", marker("k")), ("title", marker("t"))]),
    );
    assert_eq!(rendered, "[k:record] [t:R]([t:System.Int32] [t:X]);");
}

#[test]
fn highlights_go_function_receiver_results_in_one_function_span() {
    // go.js 的 FUNCTION：`beginKeywords: 'func'`，end `\s*(\{|$)`（excludeEnd），
    // contains 只有 TITLE_MODE 与 params；接收者/名字/参数/返回值都属于同一个 function。
    let rendered = highlight_with(
        "func (s *Server) Handle(req *Request) (*Response, error) {",
        "go",
        theme([
            ("keyword", marker("k")),
            ("function", marker("f")),
            ("title", marker("t")),
            ("params", marker("p")),
        ]),
    );
    assert_eq!(
        rendered,
        "[k:func][f: ][p:(s *Server)][f: ][t:Handle][p:(req *Request)][f: ][p:(*Response, error)] {"
    );
}

#[test]
fn highlights_scala_method_trailing_space_inside_the_function_span() {
    // scala.js 的 METHOD end 是 `[:={\[(\n;]`（excludeEnd）：`def f = 1` 里 `f` 之后的
    // 空格属于 function span，结束字符 `=` 不在 span 内。
    let rendered = highlight_with(
        "def f = 1",
        "scala",
        theme([
            ("keyword", marker("k")),
            ("function", marker("f")),
            ("title", marker("t")),
            ("number", marker("n")),
        ]),
    );
    assert_eq!(rendered, "[k:def][f: ][t:f][f: ]= [n:1]");
    // 参数列表 / 泛型 / 返回类型冒号会让 function 在它们之前结束。
    let with_params = highlight_with(
        "def f(x: Int) = x",
        "scala",
        theme([
            ("keyword", marker("k")),
            ("function", marker("f")),
            ("title", marker("t")),
            ("type", marker("ty")),
        ]),
    );
    assert_eq!(with_params, "[k:def][f: ][t:f](x: [ty:Int]) = x");
}

#[test]
fn highlights_swift_function_parameters_inside_the_function_span() {
    // swift.js 的 FUNCTION：`func`/标题/泛型/参数列表都在 function span 内；参数名是
    // `params`（`_` 是 keyword），参数里的类型/泛型类型是 `type`。参数列表的
    // `endsParent` 让 function 在 `)` 之后结束。
    let rendered = highlight_with(
        "func run(a: Int, b: String) -> Int { return a }",
        "swift",
        theme([
            ("keyword", marker("k")),
            ("function", marker("f")),
            ("title", marker("t")),
            ("params", marker("p")),
            ("type", marker("ty")),
        ]),
    );
    assert_eq!(
        rendered,
        "[k:func][f: ][t:run][f:(][p:a][f:: ][ty:Int][f:, ][p:b][f:: ][ty:String][f:)] -> [ty:Int] { [k:return] a }"
    );
    let empty = highlight_with(
        "func run() {}",
        "swift",
        theme([
            ("keyword", marker("k")),
            ("title", marker("t")),
            ("function", marker("f")),
        ]),
    );
    assert_eq!(empty, "[k:func][f: ][t:run][f:()] {}");
}

#[test]
fn highlights_javascript_arrow_functions_and_template_substitutions() {
    // javascript.js 的箭头函数模式（`(params) =>` / `IDENT =>`，function span 含 `=>`）
    // 与模板串 `${...}` 内部 token 的嵌套（subst contains PARAMS_CONTAINS）。
    let rendered = highlight_with(
        "const f = (x) => x + 1;\nconst g = y => y;\nconst s = `a ${x + 1} b`;",
        "javascript",
        theme([
            ("keyword", marker("k")),
            ("function", marker("f")),
            ("params", marker("p")),
            ("number", marker("n")),
            ("string", marker("s")),
            ("subst", marker("u")),
        ]),
    );
    assert_eq!(
        rendered,
        "[k:const] f = [f:(][p:x][f:) =>] x + [n:1];\n\
         [k:const] g = [p:y][f: =>] y;\n\
         [k:const] s = [s:`a ][u:${x + ][n:1][u:}][s: b`];"
    );
    // hljs 的 value container 在普通关键字处结束：`async` 之后的箭头不再着色。
    assert_eq!(
        highlight_with(
            "const f = async (x) => x;",
            "javascript",
            theme([("keyword", marker("k")), ("function", marker("f"))]),
        ),
        "[k:const] f = [k:async] (x) => x;"
    );
}

#[test]
fn highlights_rust_function_and_built_in_types() {
    let rendered = highlight_with(
        "fn main() { let x: i32 = 1; }",
        "rust",
        theme([
            ("keyword", marker("k")),
            ("title", marker("t")),
            ("built_in", marker("b")),
            ("number", marker("n")),
        ]),
    );
    assert_eq!(
        rendered,
        "[k:fn] [t:main]() { [k:let] x: [b:i32] = [n:1]; }"
    );
}

#[test]
fn highlights_bash_variables_and_built_ins() {
    let rendered = highlight_with(
        "echo \"hi $x\"",
        "bash",
        theme([
            ("built_in", marker("b")),
            ("string", marker("s")),
            ("variable", marker("v")),
        ]),
    );
    assert_eq!(rendered, "[b:echo] [s:\"hi ][v:$x][s:\"]");
}

// ---------------------------------------------------------------------------
// 回退与自动检测
// ---------------------------------------------------------------------------

#[test]
fn falls_back_to_plaintext_for_unknown_languages() {
    let rendered = highlight_with(
        "<tag> & \"quoted\"",
        "not-a-language",
        HighlightTheme::new(),
    );
    assert_eq!(rendered, "<tag> & \"quoted\"");
}

#[test]
fn falls_back_to_plaintext_for_catalog_only_languages() {
    // `ada` 只在 `load_all_highlight_languages` 之后进入目录，没有 grammar。
    let rendered = highlight_with("with Ada.Text_IO;", "ada", HighlightTheme::new());
    assert_eq!(rendered, "with Ada.Text_IO;");
}

#[test]
fn highlight_round_trips_with_an_empty_theme() {
    let cases = [
        (
            "python",
            "\"\"\"doc\"\"\"\n@dec\ndef f(a=1):  # c\n    return '<&>'",
        ),
        (
            "typescript",
            "const x: number = 1; // hi\nconst s = `a${x}b`;",
        ),
        ("javascript", "const re = /foo+/gi; <div> & 'x'"),
        ("html", "<div class=\"a\">&amp;</div>"),
        ("diff", "--- a/f\n+++ b/f\n@@ -1,2 +1,2 @@\n-old\n+new\n"),
        ("json", "{\"a\": \"b\\\"c\", \"n\": -1.5e3}"),
        (
            "rust",
            "fn f<'a>(x: &'a str) -> String { x.to_owned() } // é",
        ),
        ("bash", "echo \"hi $x\" && echo 'a&b'"),
        ("ruby", "def f(a = 1)\n  \"x#{a}\"\nend"),
    ];
    for (language, code) in cases {
        let rendered = highlight_with(code, language, HighlightTheme::new());
        assert_eq!(rendered, code, "round trip failed for {language}");
    }
}

#[test]
fn highlight_without_language_uses_auto_detection() {
    let code = "fn main() { let x = 1; }";
    let rendered = highlight(
        code,
        &HighlightOptions {
            language_subset: Some(vec!["rust".to_string()]),
            theme: theme([("keyword", marker("k"))]),
            ..HighlightOptions::default()
        },
    );
    assert!(rendered.contains("[k:fn]"));
}

#[test]
fn auto_detection_ignores_unknown_language_subset_entries() {
    let rendered = highlight(
        "def foo():\n    pass\n",
        &HighlightOptions {
            language_subset: Some(vec!["not-a-language".to_string()]),
            ..HighlightOptions::default()
        },
    );
    assert_eq!(rendered, "def foo():\n    pass\n");
}

#[test]
fn decodes_entities_with_leading_whitespace_inside_the_name() {
    // `Number.parseInt` 会跳过前导空白（含 U+3000/U+FEFF），解码长度按 UTF-16 计。
    let cases = [
        ("&#\u{3000} 65;x", "Ax"),
        ("&#\u{3000}65;x", "Ax"),
        ("&#x\u{3000}41;y", "Ay"),
        ("&#\u{FEFF}65;z", "Az"),
    ];
    for (input, expected) in cases {
        assert_eq!(
            render_highlighted_html(input, &HighlightTheme::new()),
            expected,
            "{input:?}"
        );
    }
}

#[test]
fn infers_c_family_function_declarations() {
    let rendered = highlight_with(
        "int main(void) {\n  return 0;\n}\n",
        "c",
        theme([
            ("keyword", marker("k")),
            ("title", marker("t")),
            ("params", marker("p")),
        ]),
    );
    assert!(rendered.contains("[k:int]"), "{rendered}");
    assert!(rendered.contains("[t:main]"), "{rendered}");
    assert!(rendered.contains("[p:(]"), "{rendered}");
    assert!(rendered.contains("[k:void]"), "{rendered}");

    let java = highlight_with(
        "@Override public void run() {}\n",
        "java",
        theme([("keyword", marker("k")), ("title", marker("t"))]),
    );
    assert!(java.contains("[t:run]"), "{java}");
    assert!(java.contains("[k:void]"), "{java}");
}

#[test]
fn does_not_infer_function_declarations_at_call_sites() {
    let theme = theme([("title", marker("t")), ("params", marker("p"))]);
    let java = highlight_with("System.out.println(\"hi\");", "java", theme.clone());
    assert!(!java.contains("[t:"), "{java}");
    let call = highlight_with("int x = foo(1);", "c", theme.clone());
    assert!(!call.contains("[t:"), "{call}");
    let ret = highlight_with("return foo(1);", "c", theme);
    assert!(!ret.contains("[t:"), "{ret}");
}

// ---------------------------------------------------------------------------
// 回归：参数列表中的字符串（调用方不得吞掉起始定界符）
// ---------------------------------------------------------------------------

#[test]
fn highlights_strings_inside_parameter_lists() {
    let rendered = highlight_with(
        "def greet(name=\"World\"):\n    return name\n",
        "python",
        theme([
            ("keyword", marker("k")),
            ("title", marker("t")),
            ("params", marker("p")),
            ("string", marker("s")),
        ]),
    );
    assert_eq!(
        rendered,
        "[k:def] [t:greet]([p:name=][s:\"World\"]):\n    [k:return] name\n"
    );
}

#[test]
fn round_trips_default_string_parameters() {
    let cases = [
        ("python", "def greet(name=\"World\"):\n    return name\n"),
        ("python", "def f(x=f\"a{b}\"):\n    return x"),
        ("c", "void f(const char *s = \"abc\")"),
        ("cpp", "void f(const char *s = \"abc\")"),
        ("java", "void f(String s = \"abc\")"),
        ("ruby", "def f(a = \"x\")\nend"),
        ("kotlin", "fun f(a: String = \"x\") {}"),
        ("python", "a = f\"x{y}\""),
    ];
    for (language, code) in cases {
        assert_eq!(
            highlight_with(code, language, HighlightTheme::new()),
            code,
            "round trip failed for {language}: {code:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// 回归：声明关键字后没有标识符时不得重复输出空白
// ---------------------------------------------------------------------------

#[test]
fn keeps_text_after_decl_keywords_without_a_title() {
    let cases = [
        ("javascript", "const f = function (a) { return a; };"),
        ("javascript", "function *gen() {}"),
        ("javascript", "const f = function* (a) {}"),
        ("ruby", "class << self\nend"),
        ("lua", "local f = function () end"),
        ("perl", "my $f = sub { return 1; };"),
        ("kotlin", "fun <T> foo() {}"),
        ("scala", "def <T> foo() = 1"),
    ];
    for (language, code) in cases {
        assert_eq!(
            highlight_with(code, language, HighlightTheme::new()),
            code,
            "round trip failed for {language}: {code:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// 回归：python 装饰器的缩进属于 meta span（hljs begin 为 `^[\t ]*@`）
// ---------------------------------------------------------------------------

#[test]
fn keeps_decorator_indentation_inside_the_meta_span() {
    let rendered = highlight_with(
        "    @property\ndef f(self): pass",
        "python",
        theme([("meta", marker("m"))]),
    );
    assert_eq!(rendered, "[m:    @property]\ndef f(self): pass");

    let tabbed = highlight_with(
        "\t@decorator\ndef f(): pass",
        "python",
        theme([("meta", marker("m"))]),
    );
    assert_eq!(tabbed, "[m:\t@decorator]\ndef f(): pass");

    let nested = highlight_with(
        "def f():\n    @wraps(g)\n    def g(): pass",
        "python",
        theme([("meta", marker("m"))]),
    );
    // hljs 的 meta contains PARAMS：`g` 在嵌套的 params span 里。主题只映射 meta 时，
    // 未映射的 params 继承 meta 的 formatter，但文本按 span 边界分成三段（分段是稳定渲染契约，改动需同步测试）。
    assert_eq!(
        nested,
        "def f():\n[m:    @wraps(][m:g][m:)]\n    def g(): pass"
    );

    // 空主题也必须逐字往返（缩进不能丢）。
    let code = "    @property\ndef f(self): pass";
    assert_eq!(highlight_with(code, "python", HighlightTheme::new()), code);
}

#[test]
fn nests_decorator_arguments_inside_the_meta_span() {
    // python.js 的 meta contains NUMBER / PARAMS / STRING；PARAMS 不含括号本身且没有
    // 自己的文本时不会产生可见的 params 段（渲染输出是稳定契约，改动需同步测试）。
    let rendered = highlight_with(
        "@app.route(\"/x\")\ndef f(): pass",
        "python",
        theme([
            ("meta", marker("m")),
            ("string", marker("s")),
            ("number", marker("n")),
            ("params", marker("p")),
        ]),
    );
    assert_eq!(rendered, "[m:@app.route(][s:\"/x\"][m:)]\ndef f(): pass");
    let with_params = highlight_with(
        "@dec(1, 2)\ndef f(): pass",
        "python",
        theme([
            ("meta", marker("m")),
            ("number", marker("n")),
            ("params", marker("p")),
        ]),
    );
    assert_eq!(with_params, "[m:@dec(][n:1][p:, ][n:2][m:)]\ndef f(): pass");
}

#[test]
fn highlights_javascript_object_literal_keys_as_attr() {
    // javascript.js 的 "object attr container"：`{`/`,`/换行 之后的 `IDENT:` 键是 attr。
    let rendered = highlight_with(
        "const o = {a: 1, b};",
        "javascript",
        theme([
            ("keyword", marker("k")),
            ("attr", marker("a")),
            ("number", marker("n")),
        ]),
    );
    assert_eq!(rendered, "[k:const] o = {[a:a]: [n:1], b};");
    // 普通标签/关键字不受影响（`switch` 的 `case a:` 不是对象键）。
    let switch = highlight_with(
        "switch (x) { case a: break; }",
        "javascript",
        theme([("keyword", marker("k")), ("attr", marker("a"))]),
    );
    assert_eq!(switch, "[k:switch] (x) { [k:case] a: [k:break]; }");
}

// ---------------------------------------------------------------------------
// 回归：c/cpp 预处理指令前的空白不属于 meta span（hljs begin 不含 `^\s*`）
// ---------------------------------------------------------------------------

#[test]
fn keeps_leading_whitespace_before_preprocessor_directives() {
    let rendered = highlight_with(
        "  #include <stdio.h>\nint main(){}",
        "c",
        theme([
            ("meta", marker("m")),
            ("meta-keyword", marker("mk")),
            ("meta-string", marker("ms")),
        ]),
    );
    assert_eq!(
        rendered,
        "  [m:#][mk:include][m: ][ms:<stdio.h>]\nint main(){}"
    );

    let tabbed = highlight_with(
        "\t#include <stdio.h>\nint main(){}",
        "c",
        theme([("meta", marker("m"))]),
    );
    assert!(tabbed.starts_with("\t[m:#"), "{tabbed:?}");

    let define = highlight_with(
        "  #define X 1\n",
        "cpp",
        theme([("meta", marker("m")), ("meta-keyword", marker("mk"))]),
    );
    assert_eq!(define, "  [m:#][mk:define][m: X 1]\n");

    let code = "  #include <stdio.h>\nint main(){}";
    assert_eq!(highlight_with(code, "c", HighlightTheme::new()), code);
}

// ---------------------------------------------------------------------------
// 回归：实体解码不得对整个 HTML 做二次方扫描
// ---------------------------------------------------------------------------

#[test]
fn renders_thousands_of_entities_linearly() {
    let mut html = String::new();
    for _ in 0..4000 {
        html.push_str("&amp;");
    }
    let rendered = render_highlighted_html(&html, &HighlightTheme::new());
    assert_eq!(rendered, "&".repeat(4000));
}

// ---------------------------------------------------------------------------
// 回归：扫描器必须线性（旧实现用 find_at 在每个位置向后搜索，O(n²)）
// ---------------------------------------------------------------------------

#[test]
fn highlights_large_quote_free_input_in_linear_time() {
    // 无引号、无注释的大段 Python：旧实现每个位置都要把 10 条字符串 begin 正则
    // 向后搜索到串尾，32 KB 需要数十秒（debug）；线性实现应远低于阈值。
    let code = "a ".repeat(32 * 1024);
    let started = std::time::Instant::now();
    let rendered = highlight_with(&code, "python", HighlightTheme::new());
    let elapsed = started.elapsed();
    assert_eq!(rendered, code);
    assert!(
        elapsed < std::time::Duration::from_secs(8),
        "highlighting 64 KB of quote-free Python took {elapsed:?}"
    );
}

// ---------------------------------------------------------------------------
// 回归：字符串跨行（hljs 的 illegal 只在 ignoreIllegals=false 时终止）
// ---------------------------------------------------------------------------

#[test]
fn keeps_string_spans_open_across_newlines() {
    let cases = [
        (
            "php",
            "$s = \"<div>\n  echo hidden\n</div>\";",
            "$s = [s:\"<div>]\n[s:  echo hidden]\n[s:</div>\"];",
        ),
        (
            "bash",
            "MSG=\"hello\nworld\"",
            "MSG=[s:\"hello]\n[s:world\"]",
        ),
        (
            "perl",
            "\"hello\nprint world\"",
            "[s:\"hello]\n[s:print world\"]",
        ),
        ("ruby", "s = \"a\nb\"", "s = [s:\"a]\n[s:b\"]"),
        ("swift", "let s = \"a\nb\"", "let s = [s:\"a]\n[s:b\"]"),
    ];
    for (language, code, expected) in cases {
        assert_eq!(
            highlight_with(code, language, theme([("string", marker("s"))])),
            expected,
            "{language}"
        );
    }
}

#[test]
fn rust_quote_strings_never_trigger_illegal_newlines() {
    // rust.js 把 QUOTE_STRING_MODE 的 illegal 覆盖成 null。
    let rendered = highlight(
        "let s = \"a\nb\";",
        &HighlightOptions {
            language: Some("rust".to_string()),
            ignore_illegals: false,
            theme: theme([("string", marker("s"))]),
            ..HighlightOptions::default()
        },
    );
    assert_eq!(rendered, "let s = [s:\"a]\n[s:b\"];");
}

// ---------------------------------------------------------------------------
// 回归：ignoreIllegals=false 时 illegal 词素让整段退化为转义纯文本
// ---------------------------------------------------------------------------

fn highlight_illegal_strict(code: &str, language: &str) -> String {
    highlight(
        code,
        &HighlightOptions {
            language: Some(language.to_string()),
            ignore_illegals: false,
            theme: theme([
                ("keyword", marker("k")),
                ("string", marker("s")),
                ("number", marker("n")),
                ("comment", marker("c")),
            ]),
            ..HighlightOptions::default()
        },
    )
}

#[test]
fn falls_back_to_escaped_plaintext_for_illegal_input() {
    let cases = [
        ("java", "s = \"a\nb\";"),
        ("java", "</"),
        ("java", "a # b"),
        ("python", "a -> b"),
        ("python", "a ? b"),
        ("python", "a => b"),
        ("python", "a </ b"),
        ("go", "</"),
        ("c", "</"),
        ("cpp", "</"),
        ("rust", "</"),
        ("javascript", "a # b"),
        ("typescript", "a # b"),
        ("csharp", "a::b"),
        ("ruby", "a /* b"),
        ("groovy", "a # b"),
        ("groovy", "a </ b"),
        ("kotlin", "val s = \"a\nb\""),
        ("scala", "val s = \"a\nb\""),
        ("dart", "var s = \"a\nb\";"),
        ("lua", "x = \"a\nb\""),
        ("json", "{\"a\": \"b\nc\"}"),
        // 合法的 `#` 跟随标识符字符（js/ts 的 `#(?![$_A-z])`）
        ("javascript", "a #b"),
        ("javascript", "a #$b"),
    ];
    for (language, code) in cases {
        assert_eq!(
            highlight_illegal_strict(code, language),
            code,
            "{language} should fall back to plaintext for {code:?}"
        );
    }
}

#[test]
fn keeps_highlighting_when_no_illegal_lexeme_is_present() {
    let rendered = highlight_illegal_strict("int x = 1; // hi", "java");
    assert_eq!(rendered, "[k:int] x = [n:1]; [c:// hi]");
    // python 的 `->` 在字符串里不是 illegal（字符串模式优先）。
    let rendered = highlight_illegal_strict("x = 'a -> b'", "python");
    assert_eq!(rendered, "x = [s:'a -> b']");
}

// ---------------------------------------------------------------------------
// 回归：python 装饰器不能吞掉行首的 \r / 非缩进空白
// ---------------------------------------------------------------------------

#[test]
fn preserves_characters_before_python_decorators() {
    let meta = || theme([("meta", marker("m"))]);
    assert_eq!(
        highlight_with("\r@decorator\ndef f(): pass\n", "python", meta()),
        "\r[m:@decorator]\ndef f(): pass\n"
    );
    assert_eq!(highlight_with(" \r@d\n", "python", meta()), " \r[m:@d]\n");
    // \r 是 JS 的行终止符：它之后的空格属于 meta span。
    assert_eq!(
        highlight_with("\t\r  @d\n", "python", meta()),
        "\t\r[m:  @d]\n"
    );
}

// ---------------------------------------------------------------------------
// 回归：成员/路径位置上的关键字不参与关键字着色
// ---------------------------------------------------------------------------

#[test]
fn suppresses_keywords_after_dots_and_path_separators() {
    let kw = || {
        theme([
            ("keyword", marker("k")),
            ("built_in", marker("b")),
            ("variable", marker("v")),
            ("class", marker("c")),
            ("title", marker("t")),
        ])
    };
    assert_eq!(
        highlight_with("const t = data.type;", "javascript", kw()),
        "[k:const] t = data.type;"
    );
    assert_eq!(
        highlight_with("x.type; obj.interface; b.declare;", "typescript", kw()),
        "x.type; obj.interface; b.declare;"
    );
    assert_eq!(
        highlight_with("obj.function(1); obj.class;", "javascript", kw()),
        "obj.function(1); obj.class;"
    );
    assert_eq!(
        highlight_with(
            "use crate::{a,b}; self::foo(); super::bar(); Self::new();",
            "rust",
            kw()
        ),
        "[k:use] crate::{a,b}; self::foo(); super::bar(); Self::new();"
    );
    // `Vec` 在 ident:: 模式里仍按 built_in 着色；`.` 后面的普通 ident 不受影响。
    assert_eq!(
        highlight_with("Vec::new(); crate::x;", "rust", kw()),
        "[b:Vec]::new(); crate::x;"
    );
    assert_eq!(
        highlight_with(
            "Foo.class; foo.new(); x.return; y.else; z.throw;",
            "java",
            kw()
        ),
        "Foo.class; foo.new(); x.return; y.[k:else]; z.throw;"
    );
    assert_eq!(
        highlight_with("$o->class; $x = $this->foo();", "php", kw()),
        "[v:$o]->class; [v:$x] = [k:$this]->foo();"
    );
}

// ---------------------------------------------------------------------------
// 回归：java `new throw return else` 的 beginKeywords 模式
// ---------------------------------------------------------------------------

#[test]
fn highlights_java_expression_keywords() {
    assert_eq!(
        highlight_with(
            "Object o = new Object(); return x; throw y; else {}",
            "java",
            theme([("keyword", marker("k"))])
        ),
        "Object o = [k:new] Object(); [k:return] x; [k:throw] y; [k:else] {}"
    );
}

// ---------------------------------------------------------------------------
// 回归：php `$this` 关键字与 `fn` 箭头函数
// ---------------------------------------------------------------------------

#[test]
fn highlights_php_this_and_arrow_functions() {
    let scopes = || {
        theme([
            ("function", marker("f")),
            ("keyword", marker("k")),
            ("params", marker("p")),
            ("variable", marker("v")),
            ("title", marker("t")),
        ])
    };
    assert_eq!(
        highlight_with("$f = fn($x) => $x;", "php", scopes()),
        "[v:$f] = [k:fn][f:(][v:$x][f:) =>] [v:$x];"
    );
    assert_eq!(
        highlight_with("function foo($a) { return $a; }", "php", scopes()),
        "[k:function][f: ][t:foo][f:(][v:$a][f:) ]{ [k:return] [v:$a]; }"
    );
}

// ---------------------------------------------------------------------------
// 回归：rust raw string 与 tuple 字段数字
// ---------------------------------------------------------------------------

#[test]
fn highlights_rust_raw_strings_and_tuple_field_numbers() {
    assert_eq!(
        highlight_with(
            "let s = r#\"raw \"quoted\" text\"#;",
            "rust",
            theme([("string", marker("s"))])
        ),
        "let s = [s:r#\"raw \"quoted\" text\"#];"
    );
    // hljs 的合并正则会先匹配到标识符内部的 raw string 起点（`b` 只是普通文本）。
    assert_eq!(
        highlight_with(
            "let t = br#\"x\"#;",
            "rust",
            theme([("string", marker("s"))])
        ),
        "let t = b[s:r#\"x\"#];"
    );
    assert_eq!(
        highlight_with(
            "offset.0.into_u64()",
            "rust",
            theme([("number", marker("n"))])
        ),
        "offset.[n:0].into_u64()"
    );
    assert_eq!(
        highlight_with("let x = 1.;", "rust", theme([("number", marker("n"))])),
        "let x = [n:1].;"
    );
}

// ---------------------------------------------------------------------------
// 回归：bash 的位置参数、命令替换与算术数字
// ---------------------------------------------------------------------------

#[test]
fn highlights_bash_variables_substitutions_and_arithmetic() {
    assert_eq!(
        highlight_with(
            "echo \"usage: $0 $@ $#\"",
            "bash",
            theme([("string", marker("s")), ("variable", marker("v"))])
        ),
        "echo [s:\"usage: ][v:$0][s: ][v:$@][s: ][v:$#][s:\"]"
    );
    assert_eq!(
        highlight_with(
            "x=\"$(date)\"",
            "bash",
            theme([("string", marker("s")), ("subst", marker("u"))])
        ),
        "x=[s:\"][u:$(date)][s:\"]"
    );
    assert_eq!(
        highlight_with("$((1+2))", "bash", theme([("number", marker("n"))])),
        "$(([n:1]+[n:2]))"
    );
}

// ---------------------------------------------------------------------------
// 回归：python raw 串前缀与跨行参数列表
// ---------------------------------------------------------------------------

#[test]
fn highlights_python_raw_strings_and_multiline_parameter_lists() {
    assert_eq!(
        highlight_with(
            "r\"raw\"; R\"more\"; rb\"x\"; b\"y\"",
            "python",
            theme([("string", marker("s"))])
        ),
        "[s:r\"raw\"]; [s:R\"more\"]; [s:rb\"x\"]; [s:b\"y\"]"
    );
    assert_eq!(
        highlight_with(
            "def f(\n    a,\n    b=1,\n):\n    return a\n",
            "python",
            theme([("params", marker("p")), ("number", marker("n"))])
        ),
        "def f(\n[p:    a,]\n[p:    b=][n:1][p:,]\n):\n    return a\n"
    );
}

// ---------------------------------------------------------------------------
// 回归：swift 的 `#` 指令
// ---------------------------------------------------------------------------

#[test]
fn highlights_swift_hash_directives() {
    let rendered = highlight_with(
        "#available(iOS 13, *)\n#if DEBUG\n#endif\n#selector(foo)",
        "swift",
        theme([("keyword", marker("k"))]),
    );
    assert!(rendered.starts_with("[k:#available]("), "{rendered}");
    assert!(rendered.contains("\n[k:#if] DEBUG"), "{rendered}");
    assert!(rendered.contains("\n[k:#endif]\n"), "{rendered}");
    assert!(rendered.contains("[k:#selector](foo)"), "{rendered}");
}

// ---------------------------------------------------------------------------
// 回归：xml 的 CDATA 无 span、实体正则与 hljs 一致
// ---------------------------------------------------------------------------

#[test]
fn matches_xml_cdata_and_entity_behavior() {
    let meta = || theme([("meta", marker("m"))]);
    assert_eq!(
        highlight_with("<![CDATA[raw]]>", "xml", meta()),
        "<![CDATA[raw]]>"
    );
    assert_eq!(
        highlight_with("<![cdata[raw]]>", "xml", meta()),
        "<![cdata[raw]]>"
    );
    let symbol = || theme([("symbol", marker("y"))]);
    assert_eq!(
        highlight_with("&amp; &#65; &#x41; &#X41; &#x; &#; &AMP;", "xml", symbol()),
        "[y:&amp;] [y:&#65;] [y:&#x41;] [y:&#X41;] &#x; &#; [y:&AMP;]"
    );
}

// ---------------------------------------------------------------------------
// 回归：class 属性正则使用 JS 的 \s 集合（含 U+FEFF，不含 U+0085）
// ---------------------------------------------------------------------------

#[test]
fn matches_js_whitespace_semantics_in_class_attributes() {
    let theme = theme([("keyword", marker("k")), ("default", marker("d"))]);
    let cases = [
        ("<span \u{FEFF}class=\"hljs-keyword\">a</span>", "[k:a]"),
        ("<span \u{85}class=\"hljs-keyword\">a</span>", "[d:a]"),
        ("<span class=\"hljs-keyword\u{85}foo\">a</span>", "[d:a]"),
        ("<span class=\"hljs-keyword\u{FEFF}foo\">a</span>", "[k:a]"),
        (
            "<span\u{2028}class=\"hljs-keyword\">a</span>",
            "[d:<span\u{2028}class=\"hljs-keyword\">a]",
        ),
        (
            "<span\u{A0}class=\"hljs-keyword\">a</span>",
            "[d:<span\u{A0}class=\"hljs-keyword\">a]",
        ),
        ("<span class=\"hljs-keyword\u{2029}foo\">a</span>", "[k:a]"),
        ("<span class=\"hljs-keyword\u{3000}foo\">a</span>", "[k:a]"),
        ("<span class=\"hljs-keyword\u{A0}foo\">a</span>", "[k:a]"),
    ];
    for (html, expected) in cases {
        assert_eq!(render_highlighted_html(html, &theme), expected, "{html:?}");
    }
}

// ---------------------------------------------------------------------------
// 回归：diff hunk 头的 meta span 止于第二个 `@@`
// ---------------------------------------------------------------------------

#[test]
fn stops_diff_hunk_meta_at_the_second_at_at() {
    assert_eq!(
        highlight_with(
            "@@ -1,2 +1,2 @@ func name\n-old\n+new\n",
            "diff",
            theme([("meta", marker("m"))])
        ),
        "[m:@@ -1,2 +1,2 @@] func name\n-old\n+new\n"
    );
    assert_eq!(
        highlight_with(
            "*** 1,2 ****\n--- 1,2 ----\n",
            "diff",
            theme([("meta", marker("m"))])
        ),
        "[m:*** 1,2 ****]\n[m:--- 1,2 ----]\n"
    );
}

// ---------------------------------------------------------------------------
// 回归：js/ts 的 `'use strict'` / `'use asm'` 指令 → meta
// ---------------------------------------------------------------------------

#[test]
fn marks_use_strict_directives_as_meta() {
    let meta = || theme([("meta", marker("m"))]);
    assert_eq!(
        highlight_with("'use strict';\n", "javascript", meta()),
        "[m:'use strict'];\n"
    );
    assert_eq!(
        highlight_with("var x = 1;\n'use strict';\n", "javascript", meta()),
        "var x = 1;\n[m:'use strict'];\n"
    );
    assert_eq!(
        highlight_with("  'use strict';", "javascript", meta()),
        "[m:  'use strict'];"
    );
    // typescript 用不含 asm 的变体。
    assert_eq!(
        highlight_with("'use asm';", "typescript", meta()),
        "'use asm';"
    );
}
