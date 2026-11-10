//! 移植 `packages/coding-agent/test/frontmatter.test.ts`。
//!
//! YAML 子集解析器与 npm `yaml` 2.9.0 的逐例对拍在 `src/frontmatter.rs` 的
//! `golden_tests::matches_yaml_package_on_corpus`（需要访问私有解析器）。

use kiana_core::frontmatter::{parse_frontmatter, strip_frontmatter};
use serde_json::{Value, json};

fn as_string(value: Option<&Value>) -> Option<&str> {
    value.and_then(Value::as_str)
}

#[test]
fn parses_keys_strips_quotes_and_returns_body() {
    let input =
        "---\nname: \"skill-name\"\ndescription: 'A desc'\nfoo-bar: value\n---\n\nBody text";
    let parsed = parse_frontmatter(input).expect("valid frontmatter");
    assert_eq!(
        as_string(parsed.frontmatter.get("name")),
        Some("skill-name")
    );
    assert_eq!(
        as_string(parsed.frontmatter.get("description")),
        Some("A desc")
    );
    assert_eq!(as_string(parsed.frontmatter.get("foo-bar")), Some("value"));
    assert_eq!(parsed.body, "Body text");
}

#[test]
fn normalizes_newlines_and_handles_crlf() {
    let input = "---\r\nname: test\r\n---\r\nLine one\r\nLine two";
    let parsed = parse_frontmatter(input).expect("valid frontmatter");
    assert_eq!(parsed.body, "Line one\nLine two");
    assert_eq!(as_string(parsed.frontmatter.get("name")), Some("test"));
}

#[test]
fn throws_on_invalid_yaml_frontmatter() {
    let input = "---\nfoo: [bar\n---\nBody";
    let error = parse_frontmatter(input).expect_err("invalid yaml must error");
    assert!(
        error.message().contains("at line 1, column 10"),
        "unexpected message: {}",
        error.message()
    );
}

#[test]
fn parses_pipe_multiline_yaml_syntax() {
    let input = "---\ndescription: |\n  Line one\n  Line two\n---\n\nBody";
    let parsed = parse_frontmatter(input).expect("valid frontmatter");
    assert_eq!(
        as_string(parsed.frontmatter.get("description")),
        Some("Line one\nLine two\n")
    );
    assert_eq!(parsed.body, "Body");
}

#[test]
fn returns_original_content_when_frontmatter_is_missing_or_unterminated() {
    let no_frontmatter = "Just text\nsecond line";
    let missing_end = "---\nname: test\nBody without terminator";
    let result_no_frontmatter = parse_frontmatter(no_frontmatter).expect("no yaml");
    let result_missing_end = parse_frontmatter(missing_end).expect("no yaml");
    assert_eq!(result_no_frontmatter.body, "Just text\nsecond line");
    assert_eq!(
        result_missing_end.body,
        "---\nname: test\nBody without terminator"
            .replace("\r\n", "\n")
            .replace('\r', "\n")
    );
    assert!(result_no_frontmatter.frontmatter.is_empty());
    assert!(result_missing_end.frontmatter.is_empty());
}

#[test]
fn returns_empty_object_for_empty_or_comment_only_frontmatter() {
    let input = "---\n# just a comment\n---\nBody";
    let parsed = parse_frontmatter(input).expect("valid frontmatter");
    assert!(parsed.frontmatter.is_empty());
    assert_eq!(parsed.body, "Body");
}

#[test]
fn removes_frontmatter_and_trims_body() {
    let input = "---\nkey: value\n---\n\nBody\n";
    assert_eq!(strip_frontmatter(input).expect("valid frontmatter"), "Body");
}

#[test]
fn returns_body_when_no_frontmatter_present() {
    let input = "\n  No frontmatter body  \n";
    assert_eq!(strip_frontmatter(input).expect("no frontmatter"), input);
}

#[test]
fn parses_yaml_1_2_types_used_by_skills() {
    let input = "---\nname: demo\ndescription: Test\ndisable-model-invocation: true\n---\nBody";
    let parsed = parse_frontmatter(input).expect("valid frontmatter");
    assert_eq!(
        parsed.frontmatter.get("disable-model-invocation"),
        Some(&Value::Bool(true))
    );

    // YAML 1.2：`yes` 是字符串，不是布尔（spec/ca-resources.md §6.1）。
    let input = "---\ndisable-model-invocation: yes\n---\nBody";
    let parsed = parse_frontmatter(input).expect("valid frontmatter");
    assert_eq!(
        parsed.frontmatter.get("disable-model-invocation"),
        Some(&Value::String("yes".to_string()))
    );

    let input = "---\ncount: 3\nratio: 0.5\nempty:\n---\nBody";
    let parsed = parse_frontmatter(input).expect("valid frontmatter");
    assert_eq!(
        parsed.frontmatter.get("count").and_then(Value::as_i64),
        Some(3)
    );
    assert_eq!(
        parsed.frontmatter.get("ratio").and_then(Value::as_f64),
        Some(0.5)
    );
    assert_eq!(parsed.frontmatter.get("empty"), Some(&Value::Null));
}

#[test]
fn duplicate_keys_are_an_error_like_the_yaml_package() {
    let error =
        parse_frontmatter("---\nname: a\nname: b\n---\nBody").expect_err("duplicate keys error");
    assert!(error.message().contains("Map keys must be unique"));
}

#[test]
fn unterminated_flow_collections_report_the_ts_position() {
    let error = parse_frontmatter("---\ndescription: [unclosed bracket\n---\nBody")
        .expect_err("unclosed flow sequence errors");
    assert!(
        error.message().contains("at line 1, column 31"),
        "unexpected message: {}",
        error.message()
    );
}

#[test]
fn empty_yaml_string_returns_empty_frontmatter() {
    let parsed = parse_frontmatter("---\n---\nBody").expect("empty frontmatter");
    assert!(parsed.frontmatter.is_empty());
    assert_eq!(parsed.body, "Body");
}

#[test]
fn duplicate_keys_compare_yaml_values_not_object_keys() {
    // yaml 包按 YAML 值身份查重：`null` 键与 `""` 键不同（JS 对象里都写成 `""`，后者覆盖前者）。
    let parsed = parse_frontmatter("---\nnull: 1\n\"\": 2\n---\nBody").expect("no duplicate error");
    assert_eq!(
        parsed.frontmatter.get(""),
        Some(&Value::Number(2.into())),
        "last wins"
    );
    assert_eq!(parsed.frontmatter.len(), 1);
}

#[test]
fn non_finite_keys_use_js_number_strings() {
    let parsed = parse_frontmatter("---\n.inf: 1\n.nan: 2\n---\nBody").expect("valid frontmatter");
    assert_eq!(
        parsed.frontmatter.get("Infinity"),
        Some(&Value::Number(1.into()))
    );
    assert_eq!(
        parsed.frontmatter.get("NaN"),
        Some(&Value::Number(2.into()))
    );
}

#[test]
fn block_scalars_honor_explicit_indentation_indicators() {
    let parsed =
        parse_frontmatter("---\ndescription: |2\n    extra\n---\nBody").expect("valid frontmatter");
    assert_eq!(
        parsed.frontmatter.get("description"),
        Some(&Value::String("  extra\n".to_string()))
    );
}

#[test]
fn root_block_scalar_documents_degrade_to_empty_frontmatter() {
    // TS 会把这个文档解析成字符串（随后下游只按字段读）；Rust 的 frontmatter 是 map。
    let parsed = parse_frontmatter("---\n|\nx\n---\nBody").expect("root block scalar parses");
    assert!(parsed.frontmatter.is_empty());
    assert_eq!(parsed.body, "Body");
}

#[test]
fn block_scalars_keep_exact_trailing_newlines() {
    let parsed = parse_frontmatter("---\ndescription: |+\n  one\n  \n  \n---\nBody")
        .expect("valid frontmatter");
    assert_eq!(
        parsed.frontmatter.get("description"),
        Some(&Value::String("one\n\n".to_string()))
    );
}

#[test]
fn block_scalars_allow_tab_content() {
    let parsed =
        parse_frontmatter("---\ndescription: >-\n  \tTab\n---\nBody").expect("valid frontmatter");
    assert_eq!(
        parsed.frontmatter.get("description"),
        Some(&Value::String("\tTab".to_string()))
    );
}

#[test]
fn plain_scalars_stop_at_document_end_marker() {
    let parsed = parse_frontmatter("---\nname: alpha\n...\n---\nBody").expect("valid frontmatter");
    assert_eq!(
        parsed.frontmatter.get("name"),
        Some(&Value::String("alpha".to_string()))
    );
}

#[test]
fn multiline_implicit_keys_report_the_ts_error() {
    let error = parse_frontmatter("---\na: 1\nfoo\nbar: 2\n---\nBody")
        .expect_err("multiline implicit key errors");
    assert!(
        error
            .message()
            .contains("Implicit keys need to be on a single line at line 2, column 1"),
        "unexpected message: {}",
        error.message()
    );
}

#[test]
fn block_scalars_reject_more_indented_leading_blank_lines() {
    let error = parse_frontmatter("---\ndescription: |\n    \n  x\n---\nBody")
        .expect_err("leading blank indent errors");
    assert!(
        error.message().contains(
            "Block scalars with more-indented leading empty lines must use an explicit indentation indicator at line 3, column 3"
        ),
        "unexpected message: {}",
        error.message()
    );
}

// ---------------------------------------------------------------------------
// 评审回归（findings 1-11）：与 `yaml` 2.9.0 的逐例对拍。
// ---------------------------------------------------------------------------

#[test]
fn non_ascii_content_after_the_opening_marker_does_not_panic() {
    // finding 1（critical）：`---` 后紧跟多字节字符时，TS 的 UTF-16 slice 合法；
    // 旧实现按字节切 `normalized[4..end_index]` 会 panic。
    let parsed = parse_frontmatter("---é\n---\nBody").expect("é is a valid one-unit yaml string");
    assert!(parsed.frontmatter.is_empty());
    assert_eq!(parsed.body, "Body");

    // 星平面字符占 2 个 UTF-16 code unit，`slice(4, endIndex)` 会切出孤立低代理项；
    // Rust 无法表示，退化为空 map（非 mapping 文档的既有偏差），但绝不能 panic。
    let parsed = parse_frontmatter("---😀\n---\nBody").expect("astral char must not panic");
    assert!(parsed.frontmatter.is_empty());
    assert_eq!(parsed.body, "Body");
}

#[test]
fn flow_collections_span_lines_without_gaining_trailing_spaces() {
    // finding 2（major）：闭合括号可以在任意缩进；内容行才需要更深。
    let parsed =
        parse_frontmatter("---\nallowed-tools: [Read, Grep,\n  Glob,\n  Bash\n  ]\n---\nBody")
            .expect("multi-line flow sequence");
    assert_eq!(
        parsed.frontmatter.get("allowed-tools"),
        Some(&json!(["Read", "Grep", "Glob", "Bash"]))
    );

    let parsed = parse_frontmatter("---\na: [1, 2,\n  3\n  ]\n---\nBody")
        .expect("closing bracket indented past the parent");
    assert_eq!(parsed.frontmatter.get("a"), Some(&json!([1, 2, 3])));

    let parsed =
        parse_frontmatter("---\na: [1,\n  2,\n]\n---\nBody").expect("closing bracket at column 0");
    assert_eq!(parsed.frontmatter.get("a"), Some(&json!([1, 2])));

    // 嵌套 flow 集合的闭合括号按最内层缩进处理。
    let parsed = parse_frontmatter("---\na: [\n  [\n    1\n  ]\n]\n---\nBody")
        .expect("nested flow sequence");
    assert_eq!(parsed.frontmatter.get("a"), Some(&json!([[1]])));
}

#[test]
fn multiline_quoted_scalars_in_sequence_items_fold_like_the_yaml_package() {
    // finding 3（major）：序列项的跨行引号 scalar 用序列缩进判断续行。
    let parsed = parse_frontmatter("---\ntools:\n  - \"multi\n   line\"\n---\nBody")
        .expect("multi-line double-quoted sequence item");
    assert_eq!(
        parsed.frontmatter.get("tools"),
        Some(&json!(["multi line"]))
    );

    let parsed = parse_frontmatter("---\ntools:\n  - 'multi\n   line'\n---\nBody")
        .expect("multi-line single-quoted sequence item");
    assert_eq!(
        parsed.frontmatter.get("tools"),
        Some(&json!(["multi line"]))
    );

    let parsed = parse_frontmatter("---\ntools:\n  - \"multi\n\n   line\"\n---\nBody")
        .expect("blank line folds to a newline");
    assert_eq!(
        parsed.frontmatter.get("tools"),
        Some(&json!(["multi\nline"]))
    );

    // 序列项里的跨行引号键：位置在引号起点（不是上一行行尾）。
    let error = parse_frontmatter("---\nlist:\n  - \"a\n   b\": 1\n---\nBody")
        .expect_err("multiline implicit key errors");
    assert_eq!(
        error.message(),
        "Implicit keys need to be on a single line at line 2, column 5:\n\n  - \"a\n    ^\n"
    );
}

#[test]
fn plain_values_may_start_with_question_or_colon_when_not_followed_by_space() {
    // finding 4（major）：`?`/`:` 只有后跟空白/EOL 时才是指示符。
    let parsed = parse_frontmatter("---\ndescription: ?foo\n---\nBody").expect("?foo value");
    assert_eq!(parsed.frontmatter.get("description"), Some(&json!("?foo")));

    let parsed = parse_frontmatter("---\ndescription: :foo\n---\nBody").expect(":foo value");
    assert_eq!(parsed.frontmatter.get("description"), Some(&json!(":foo")));

    let parsed = parse_frontmatter("---\nname: :-)\n---\nBody").expect(":-) value");
    assert_eq!(parsed.frontmatter.get("name"), Some(&json!(":-)")));
}

#[test]
fn anchors_aliases_and_tags_resolve_like_the_yaml_package() {
    // finding 5（major）。
    let parsed = parse_frontmatter("---\nname: &anchor Some text\n---\nBody").expect("anchor");
    assert_eq!(parsed.frontmatter.get("name"), Some(&json!("Some text")));

    let parsed = parse_frontmatter("---\na: &x 1\nb: *x\n---\nBody").expect("alias");
    assert_eq!(parsed.frontmatter.get("a"), Some(&json!(1)));
    assert_eq!(parsed.frontmatter.get("b"), Some(&json!(1)));

    let parsed = parse_frontmatter("---\nname: !!str 123\n---\nBody").expect("!!str tag");
    assert_eq!(parsed.frontmatter.get("name"), Some(&json!("123")));

    let parsed = parse_frontmatter("---\nname: !!int 42\n---\nBody").expect("!!int tag");
    assert_eq!(parsed.frontmatter.get("name"), Some(&json!(42)));

    // 回归：空节点上的 tag 不能继承上一个 scalar（键名）的文本。
    let parsed = parse_frontmatter("---\nname: !bang\n---\nBody").expect("empty tagged node");
    assert_eq!(parsed.frontmatter.get("name"), Some(&json!("")));

    let parsed = parse_frontmatter("---\nname: !bang 7\n---\nBody").expect("custom tagged value");
    assert_eq!(parsed.frontmatter.get("name"), Some(&json!("7")));

    // `<<` 在 core schema 里不是 merge（yaml 包保留为普通键）。
    let parsed = parse_frontmatter("---\nbase: &b\n  x: 1\nchild:\n  <<: *b\n  y: 2\n---\nBody")
        .expect("merge-key shaped mapping");
    assert_eq!(
        parsed.frontmatter.get("child"),
        Some(&json!({"<<": {"x": 1}, "y": 2}))
    );
}

#[test]
fn unresolved_aliases_report_the_ts_error_without_a_position() {
    let error = parse_frontmatter("---\nname: *alias\n---\nBody").expect_err("unresolved alias");
    assert_eq!(
        error.message(),
        "Unresolved alias (the anchor must be set before the alias): alias"
    );
    assert_eq!(error.line(), 0);
    assert_eq!(error.column(), 0);
}

#[test]
fn overflowing_numeric_keys_stringify_like_js() {
    // finding 6（minor）：1e400 解析为 f64::INFINITY，JS 键是 "Infinity"。
    let parsed = parse_frontmatter("---\n1e400: v\n---\nBody").expect("overflowing key");
    assert_eq!(parsed.frontmatter.get("Infinity"), Some(&json!("v")));

    let parsed = parse_frontmatter("---\n-1e400: v\n---\nBody").expect("negative overflowing key");
    assert_eq!(parsed.frontmatter.get("-Infinity"), Some(&json!("v")));
}

#[test]
fn surrogate_pair_escapes_decode_to_the_astral_character() {
    // finding 7（minor）：`\uD83D\uDE00` 是合法的代理对，不是两个 U+FFFD。
    let parsed =
        parse_frontmatter("---\na: \"\\uD83D\\uDE00\"\n---\nBody").expect("surrogate pair");
    assert_eq!(
        parsed.frontmatter.get("a"),
        Some(&Value::String("\u{1F600}".to_string()))
    );
}

#[test]
fn escape_errors_match_the_yaml_package() {
    // finding 8（minor）。
    let error = parse_frontmatter("---\na: \"\\U00110000\"\n---\nBody")
        .expect_err("codepoint above U+10FFFF errors");
    assert_eq!(
        error.message(),
        "Invalid escape sequence \\U00110000 at line 1, column 5:\n\na: \"\\U00110000\"\n    ^\n"
    );

    // 短转义：消息包含实际读到的字符（含终止引号）。
    let error =
        parse_frontmatter("---\na: \"\\x4\"\n---\nBody").expect_err("short \\x escape errors");
    assert_eq!(
        error.message(),
        "Invalid escape sequence \\x4\" at line 1, column 5:\n\na: \"\\x4\"\n    ^\n"
    );

    // yaml 的 EOF 怪癖：文档以转义引号结尾时仍被接受。
    let parsed =
        parse_frontmatter("---\na: \"x\\\"\n---\nBody").expect("escaped quote at EOF is accepted");
    assert_eq!(parsed.frontmatter.get("a"), Some(&json!("x\"")));
}

#[test]
fn content_after_a_document_end_marker_is_a_multiple_documents_error() {
    // finding 9（minor）。
    let error = parse_frontmatter("---\n...\ndescription: x\n---\nBody")
        .expect_err("second document inside one frontmatter block");
    assert_eq!(
        error.message(),
        "Source contains multiple documents; please use YAML.parseAllDocuments() at line 2, column 1:\n\n...\ndescription: x\n^^^^^^^^^^^^^^\n"
    );
}

#[test]
fn malformed_shapes_that_the_yaml_package_rejects_are_errors() {
    // finding 10（minor）。
    let error = parse_frontmatter("---\n a: |\n b\n---\nBody")
        .expect_err("block scalar at parent indentation");
    assert_eq!(
        error.message(),
        "Implicit map keys need to be followed by map values at line 2, column 2:\n\n a: |\n b\n ^\n"
    );

    let error = parse_frontmatter("---\na: 1\n- 2: 3\n---\nBody").expect_err("stray sequence item");
    assert_eq!(
        error.message(),
        "Implicit keys need to be on a single line at line 2, column 1:\n\na: 1\n- 2: 3\n^\n"
    );

    let error = parse_frontmatter("---\n@foo: 1\n---\nBody").expect_err("reserved character key");
    assert_eq!(
        error.message(),
        "Plain value cannot start with reserved character @ at line 1, column 1:\n\n@foo: 1\n^\n"
    );

    let error = parse_frontmatter("---\na: [- ]\n---\nBody").expect_err("block seq in flow");
    assert_eq!(
        error.message(),
        "Block collections are not allowed within flow collections at line 1, column 5:\n\na: [- ]\n    ^^\n"
    );
}

#[test]
fn error_snippets_include_the_previous_line_and_use_utf16_columns() {
    // finding 11（minor）：yaml 在插入符前全是空格时会带上一行；列按 UTF-16 计。
    let error = parse_frontmatter("---\nname: a\nname: b\n---\nBody").expect_err("duplicate key");
    assert_eq!(
        error.message(),
        "Map keys must be unique at line 2, column 1:\n\nname: a\nname: b\n^\n"
    );

    let error = parse_frontmatter("---\nname: \"😀\n---\nBody").expect_err("unterminated quote");
    assert_eq!(
        error.message(),
        "Missing closing \"quote at line 1, column 10:\n\nname: \"😀\n         ^\n"
    );
}

// ---------------------------------------------------------------------------
// 评审回归（第二轮 findings 1-9）：全部期望值来自 `yaml` 2.9.0 的实测输出。
// ---------------------------------------------------------------------------

fn frontmatter_of(content: &str) -> Value {
    match parse_frontmatter(content) {
        Ok(parsed) => Value::Object(parsed.frontmatter),
        Err(error) => panic!("expected {content:?} to parse, got: {}", error.message()),
    }
}

fn error_message(content: &str) -> String {
    match parse_frontmatter(content) {
        Ok(_) => panic!("expected {content:?} to fail"),
        Err(error) => error.message().to_string(),
    }
}

#[test]
fn tab_only_blank_lines_and_tab_indented_comments_are_allowed() {
    // finding 1（major）：yaml 的 lexer 把只有空白/tab 的行与 tab 缩进的注释行当空白；
    // 只有真正的内容行用 tab 缩进才报 “Tabs are not allowed as indentation”。
    for (content, expected) in [
        ("---\n\t\na: 1\n---\nBody", json!({"a": 1})),
        ("---\n\t# c\na: 1\n---\nBody", json!({"a": 1})),
        ("---\n\t\n---\nBody", json!({})),
        ("---\na: 1\n\t\nb: 2\n---\nBody", json!({"a": 1, "b": 2})),
        ("---\na: 1\n\t# c\nb: 2\n---\nBody", json!({"a": 1, "b": 2})),
        (
            "---\na: 1\n \t# c\nb: 2\n---\nBody",
            json!({"a": 1, "b": 2}),
        ),
        ("---\na:\n  b: 1\n\t\n---\nBody", json!({"a": {"b": 1}})),
    ] {
        assert_eq!(frontmatter_of(content), expected, "content {content:?}");
    }

    assert_eq!(
        error_message("---\na:\n\tb: 1\n---\nBody"),
        "Tabs are not allowed as indentation at line 2, column 1:\n\na:\n\tb: 1\n^\n"
    );
    assert_eq!(
        error_message("---\n\tb: 1\n---\nBody"),
        "Tabs are not allowed as indentation at line 1, column 1:\n\n\tb: 1\n^\n"
    );
}

#[test]
fn tabs_inside_flow_collections_match_the_yaml_package() {
    // finding 1 的 flow 分支：闭合括号前的 tab 行、tab 空行都合法；tab 开头的续行
    // 内容按空格缩进判断，缩进不足时错误指向内容本身。
    assert_eq!(
        frontmatter_of("---\na: [\n\t]\n---\nBody"),
        json!({"a": []})
    );
    assert_eq!(
        frontmatter_of("---\na: [1,\n  \t\n  2]\n---\nBody"),
        json!({"a": [1, 2]})
    );
    assert_eq!(
        frontmatter_of("---\na: [x\n  \ty]\n---\nBody"),
        json!({"a": ["x y"]})
    );
    assert_eq!(
        error_message("---\na: [x\n\ty]\n---\nBody"),
        "Flow sequence in block collection must be sufficiently indented and end with a ] at line 2, column 2:\n\n\ty]\n ^\n"
    );
}

#[test]
fn property_only_sequence_items_do_not_swallow_the_next_item() {
    // finding 2（major）：`- &x` / `- !!str` 是完整的序列项（null / ""），
    // 下一行同缩进的 `- b` 是兄弟项而不是它的值。
    for (content, expected) in [
        (
            "---\na:\n  - &x\n  - b\n---\nBody",
            json!({"a": [null, "b"]}),
        ),
        (
            "---\na:\n  - !!str\n  - b\n---\nBody",
            json!({"a": ["", "b"]}),
        ),
        (
            "---\na:\n  - !tag\n  - b\n---\nBody",
            json!({"a": ["", "b"]}),
        ),
        ("---\na:\n  - !\n  - b\n---\nBody", json!({"a": ["", "b"]})),
        (
            "---\na:\n  - &x 1\n  - b\n---\nBody",
            json!({"a": [1, "b"]}),
        ),
        ("---\na:\n  - &x\n---\nBody", json!({"a": [null]})),
    ] {
        assert_eq!(frontmatter_of(content), expected, "content {content:?}");
    }
}

#[test]
fn timestamp_fractional_seconds_are_truncated_not_added() {
    // finding 3（minor）：毫秒取小数部分前 3 位，秒数本身不能因为四舍五入/重复计数
    // 而进位（`.1` 必须是 100ms，`.999` 不能变成下一秒）。
    for (content, expected) in [
        (
            "---\na: !!timestamp 2001-12-15T02:59:43.1Z\n---\nBody",
            "2001-12-15T02:59:43.100Z",
        ),
        (
            "---\na: !!timestamp 2001-12-15T02:59:43.999Z\n---\nBody",
            "2001-12-15T02:59:43.999Z",
        ),
        (
            "---\na: !!timestamp 2001-12-15T02:59:43.123456Z\n---\nBody",
            "2001-12-15T02:59:43.123Z",
        ),
        (
            "---\na: !!timestamp 2001-12-15T02:59:43Z\n---\nBody",
            "2001-12-15T02:59:43.000Z",
        ),
        (
            "---\na: !!timestamp 2001-12-15T02:59:43.1+02:00\n---\nBody",
            "2001-12-15T00:59:43.100Z",
        ),
        (
            "---\na: !!timestamp 2001-12-15\n---\nBody",
            "2001-12-15T00:00:00.000Z",
        ),
        (
            "---\na: !!timestamp 2001-12-15 02:59:43.5 -5\n---\nBody",
            "2001-12-15T07:59:43.500Z",
        ),
    ] {
        let frontmatter = frontmatter_of(content);
        assert_eq!(
            frontmatter.get("a"),
            Some(&json!(expected)),
            "content {content:?}"
        );
    }

    // core schema 没有默认的 timestamp tag：未加 tag 的同形 scalar 仍是字符串。
    assert_eq!(
        frontmatter_of("---\na: 2001-12-15T02:59:43.1Z\n---\nBody"),
        json!({"a": "2001-12-15T02:59:43.1Z"})
    );

    assert_eq!(
        error_message("---\na: !!timestamp not-a-date\n---\nBody"),
        "!!timestamp expects a date, starting with yyyy-mm-dd at line 1, column 4:\n\na: !!timestamp not-a-date\n   ^^^^^^^^^^^\n"
    );
}

#[test]
fn explicit_float_tag_requires_a_dot_or_exponent() {
    // finding 4（minor）：`!!float` 只接受 floatNaN / floatExp / float 三个 test；
    // 裸整数 `1`、带符号整数 `+2` 都匹配不上，tag 无法解析（warning）而保持字符串。
    assert_eq!(
        frontmatter_of("---\na: !!float 1\n---\nBody"),
        json!({"a": "1"})
    );
    assert_eq!(
        frontmatter_of("---\na: !!float '+2'\n---\nBody"),
        json!({"a": "+2"})
    );
    assert_eq!(
        frontmatter_of("---\na: !!float 1.5\n---\nBody"),
        json!({"a": 1.5})
    );
    assert_eq!(
        frontmatter_of("---\na: !!float 1e3\n---\nBody"),
        json!({"a": 1000})
    );
    assert_eq!(
        frontmatter_of("---\na: !!float .5\n---\nBody"),
        json!({"a": 0.5})
    );
    // serde_json 无法表示 Infinity；JS 的 JSON.stringify 也把它写成 null。
    assert_eq!(
        frontmatter_of("---\na: !!float -.inf\n---\nBody"),
        json!({"a": null})
    );
}

#[test]
fn integers_beyond_i64_range_stay_js_doubles() {
    // finding 5（minor）：`2^63` 不能被饱和转换成 i64::MAX；它和 `2^63-1` 都是同一个
    // JS Number，键文本按 ECMAScript Number::toString 是 9223372036854776000。
    for content in [
        "---\n9223372036854775808: v\n---\nBody",
        "---\n9223372036854775807: v\n---\nBody",
    ] {
        let frontmatter = frontmatter_of(content);
        assert_eq!(
            frontmatter.get("9223372036854776000"),
            Some(&json!("v")),
            "content {content:?}"
        );
    }

    let parsed = parse_frontmatter("---\na: 9223372036854775808\n---\nBody")
        .expect("2^63 as a value parses");
    assert_eq!(
        parsed.frontmatter.get("a").and_then(Value::as_f64),
        Some(9_223_372_036_854_775_808.0)
    );
    // 值的 JSON 文本必须走 workspace 的 JS 序列化（serde_json 会给指数形式）。
    assert_eq!(
        kiana_json::js_object::stringify(&Value::Object(parsed.frontmatter)),
        "{\"a\":9223372036854776000}"
    );
}

#[test]
fn plain_scalars_cannot_start_with_flow_end_tokens() {
    // finding 6（minor）：`]`/`}` 在块上下文里是 flow-end token（哪怕在 tag/anchor 后），
    // 不能当作 plain scalar 的开头。
    assert_eq!(
        error_message("---\na: ]foo\n---\nBody"),
        "Unexpected flow-seq-end token in YAML stream: \"]\" at line 1, column 4:\n\na: ]foo\n   ^\n"
    );
    assert_eq!(
        error_message("---\na: }foo\n---\nBody"),
        "Unexpected flow-map-end token in YAML stream: \"}\" at line 1, column 4:\n\na: }foo\n   ^\n"
    );
    assert_eq!(
        error_message("---\na: !tag ]foo\n---\nBody"),
        "Unexpected flow-seq-end token in YAML stream: \"]\" at line 1, column 9:\n\na: !tag ]foo\n        ^\n"
    );
    assert_eq!(
        error_message("---\na: &x ]foo\n---\nBody"),
        "Unexpected flow-seq-end token in YAML stream: \"]\" at line 1, column 7:\n\na: &x ]foo\n      ^\n"
    );
}

#[test]
fn duplicate_properties_alias_props_and_unknown_tag_handles_error() {
    // finding 6（minor）：yaml 的 resolveProps / ALIAS_PROPS / tagName 行为。
    assert_eq!(
        error_message("---\na: &x &y 1\n---\nBody"),
        "A node can have at most one anchor at line 1, column 7:\n\na: &x &y 1\n      ^^\n"
    );
    assert_eq!(
        error_message("---\na: !!str !!int 1\n---\nBody"),
        "A node can have at most one tag at line 1, column 10:\n\na: !!str !!int 1\n         ^^^^^\n"
    );
    assert_eq!(
        error_message("---\na: &x 1\nb: &y *x\n---\nBody"),
        "An alias node must not specify any properties at line 2, column 7:\n\nb: &y *x\n      ^^\n"
    );
    assert_eq!(
        error_message("---\na: !foo!bar 1\n---\nBody"),
        "Could not resolve tag: !foo!bar at line 1, column 4:\n\na: !foo!bar 1\n   ^^^^^^^^\n"
    );
    assert_eq!(
        error_message("---\na: !! 1\n---\nBody"),
        "The !! tag has no suffix at line 1, column 4:\n\na: !! 1\n   ^^\n"
    );
}

#[test]
fn verbatim_tags_match_the_yaml_package() {
    // finding 6/7 相邻：`!<>` 的 tagName 是空串（falsy），compose 视作没有 tag；
    // verbatim tag token 在空白处结束，所以缺少 `>` 时插入符只覆盖 tag token 本身。
    assert_eq!(frontmatter_of("---\na: !<> 1\n---\nBody"), json!({"a": 1}));
    assert_eq!(
        frontmatter_of("---\na: !<> \"1\"\n---\nBody"),
        json!({"a": "1"})
    );
    assert_eq!(
        frontmatter_of("---\na: !<> true\n---\nBody"),
        json!({"a": true})
    );
    assert_eq!(
        frontmatter_of("---\na: !<> ~\n---\nBody"),
        json!({"a": null})
    );
    assert_eq!(
        frontmatter_of("---\na: !<> [1]\n---\nBody"),
        json!({"a": [1]})
    );
    assert_eq!(frontmatter_of("---\na: !<>\n---\nBody"), json!({"a": null}));

    assert_eq!(
        frontmatter_of("---\na: !<tag:yaml.org,2002:int> 42\n---\nBody"),
        json!({"a": 42})
    );
    assert_eq!(
        frontmatter_of("---\na: !<tag:yaml.org,2002:str> 42\n---\nBody"),
        json!({"a": "42"})
    );

    assert_eq!(
        error_message("---\na: !<tag:yaml.org,2002:str 1\n---\nBody"),
        "Verbatim tags must end with a > at line 1, column 4:\n\na: !<tag:yaml.org,2002:str 1\n   ^^^^^^^^^^^^^^^^^^^^^^^\n"
    );
    assert_eq!(
        error_message("---\na: !<abc def> 1\n---\nBody"),
        "Verbatim tags must end with a > at line 1, column 4:\n\na: !<abc def> 1\n   ^^^^^\n"
    );
    assert_eq!(
        error_message("---\na: !<!> 1\n---\nBody"),
        "Verbatim tags aren't resolved, so !<!> is invalid. at line 1, column 4:\n\na: !<!> 1\n   ^^^^\n"
    );
    assert_eq!(
        error_message("---\na: !<!!> 1\n---\nBody"),
        "Verbatim tags aren't resolved, so !<!!> is invalid. at line 1, column 4:\n\na: !<!!> 1\n   ^^^^^\n"
    );
}

#[test]
fn explicit_block_keys_and_flow_collection_keys_parse_like_the_yaml_package() {
    // finding 7（minor）：显式 key、flow 集合 key、key 上的 anchor、!!pairs。
    for (content, expected) in [
        ("---\n? a\n: 1\n---\nBody", json!({"a": 1})),
        ("---\n? a\n : 1\n---\nBody", json!({"a": 1})),
        ("---\n? a\n: 1\nb: 2\n---\nBody", json!({"a": 1, "b": 2})),
        ("---\n? [1, 2]\n: v\n---\nBody", json!({"[ 1, 2 ]": "v"})),
        ("---\n? |\n  text\n: v\n---\nBody", json!({"text\n": "v"})),
        ("---\n? a\n: - 1\n  - 2\n---\nBody", json!({"a": [1, 2]})),
        ("---\n[1]: v\n---\nBody", json!({"[ 1 ]": "v"})),
        ("---\n{a: 1}: v\n---\nBody", json!({"{ a: 1 }": "v"})),
        (
            "---\na:\n  - [1]: v\n  - b\n---\nBody",
            json!({"a": [{"[ 1 ]": "v"}, "b"]}),
        ),
        ("---\n&x a: 1\nb: *x\n---\nBody", json!({"a": 1, "b": "a"})),
        (
            "---\na: !!pairs [1]\n---\nBody",
            json!({"a": [{"1": null}]}),
        ),
        (
            "---\na: !!pairs [[1, 2]]\n---\nBody",
            json!({"a": [{"[ 1, 2 ]": null}]}),
        ),
        (
            "---\na: !!pairs [{x: 1}]\n---\nBody",
            json!({"a": [{"x": 1}]}),
        ),
    ] {
        assert_eq!(frontmatter_of(content), expected, "content {content:?}");
    }
}

#[test]
fn flow_explicit_key_pairs_match_the_yaml_package() {
    // `?` 在 flow 序列里即使没有 `:` 也构成 pair（值为 null）；后跟 flow 指示符
    // （`,[]{}`）时同样是 indicator。
    for (content, expected) in [
        ("---\na: [? ]\n---\nBody", json!({"a": [{"": null}]})),
        ("---\na: [? a]\n---\nBody", json!({"a": [{"a": null}]})),
        ("---\na: [?]\n---\nBody", json!({"a": [{"": null}]})),
        ("---\na: [?, 1]\n---\nBody", json!({"a": [{"": null}, 1]})),
        (
            "---\na: [? a, b]\n---\nBody",
            json!({"a": [{"a": null}, "b"]}),
        ),
        (
            "---\na: [? [1, 2]]\n---\nBody",
            json!({"a": [{"[ 1, 2 ]": null}]}),
        ),
        ("---\na: [? a : b]\n---\nBody", json!({"a": [{"a": "b"}]})),
    ] {
        assert_eq!(frontmatter_of(content), expected, "content {content:?}");
    }
}

#[test]
fn self_referential_aliases_are_an_error_not_a_panic() {
    // yaml 会构造循环对象（`a: &x [*x]` 在 TS 里解析成功，但 JSON.stringify 会抛
    // circular）；serde_json::Value 是树，无法表示循环。这里只能报未解析别名，
    // 关键是不能 panic —— 见模块文档的已知偏差。
    for content in [
        "---\na: &x [*x]\n---\nBody",
        "---\na: &x\n  b: *x\n---\nBody",
    ] {
        let message = error_message(content);
        assert!(
            message.starts_with("Unresolved alias (the anchor must be set before the alias): x"),
            "content {content:?} gave {message:?}"
        );
    }
}
