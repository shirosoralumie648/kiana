//! 移植 `packages/coding-agent/test/session-selector-search.test.ts`（14 项）。

use kiana_interactive::components::session_selector_search::{
    NameFilter, SortMode, filter_and_sort_sessions,
};
use kiana_session_core::SessionInfo;

fn make_session(
    id: &str,
    name: Option<&str>,
    modified: f64,
    all_messages_text: &str,
) -> SessionInfo {
    SessionInfo {
        path: format!("/tmp/{id}.jsonl"),
        id: id.to_string(),
        cwd: String::new(),
        name: name.map(str::to_string),
        parent_session_path: None,
        created: 0.0,
        modified,
        message_count: 1,
        first_message: "(no messages)".to_string(),
        all_messages_text: all_messages_text.to_string(),
    }
}

fn ids(result: &[&SessionInfo]) -> Vec<String> {
    result.iter().map(|session| session.id.clone()).collect()
}

#[test]
fn filters_by_quoted_phrase_with_whitespace_normalization() {
    let sessions = [
        make_session(
            "a",
            None,
            date_ms(2026, 1, 1),
            "node\n\n   cve was discussed",
        ),
        make_session("b", None, date_ms(2026, 1, 2), "node something else"),
    ];
    let result =
        filter_and_sort_sessions(&sessions, "\"node cve\"", SortMode::Recent, NameFilter::All);
    assert_eq!(ids(&result), ["a"]);
}

#[test]
fn filters_by_regex_and_is_case_insensitive() {
    let sessions = [
        make_session("a", None, date_ms(2026, 1, 2), "Brave is great"),
        make_session("b", None, date_ms(2026, 1, 3), "bravery is not the same"),
    ];
    let result = filter_and_sort_sessions(
        &sessions,
        "re:\\bbrave\\b",
        SortMode::Recent,
        NameFilter::All,
    );
    assert_eq!(ids(&result), ["a"]);
}

#[test]
fn recent_sort_preserves_input_order() {
    let sessions = [
        make_session("newer", None, date_ms(2026, 1, 3), "brave"),
        make_session("older", None, date_ms(2026, 1, 1), "brave"),
        make_session("nomatch", None, date_ms(2026, 1, 4), "something else"),
    ];
    let result =
        filter_and_sort_sessions(&sessions, "\"brave\"", SortMode::Recent, NameFilter::All);
    assert_eq!(ids(&result), ["newer", "older"]);
}

#[test]
fn relevance_sort_orders_by_score_and_tie_breaks_by_modified_desc() {
    let sessions = [
        make_session("message", None, date_ms(2026, 1, 3), "brave"),
        make_session("name", Some("Brave work"), date_ms(2026, 1, 1), "xxxx"),
    ];
    let result =
        filter_and_sort_sessions(&sessions, "\"brave\"", SortMode::Relevance, NameFilter::All);
    assert_eq!(ids(&result), ["name", "message"]);

    let tie_sessions = [
        make_session("newer", None, date_ms(2026, 1, 3), "brave"),
        make_session("older", None, date_ms(2026, 1, 1), "brave"),
    ];
    let result = filter_and_sort_sessions(
        &tie_sessions,
        "\"brave\"",
        SortMode::Relevance,
        NameFilter::All,
    );
    assert_eq!(ids(&result), ["newer", "older"]);
}

#[test]
fn ignores_the_position_of_a_match_in_message_text() {
    let filler = "lorem ipsum dolor sit amet ".repeat(400);
    let sessions = [
        make_session(
            "early",
            None,
            date_ms(2026, 1, 1),
            &format!("opus {filler}"),
        ),
        make_session(
            "deep",
            None,
            date_ms(2026, 1, 2),
            &format!("{filler} we discussed the opus model"),
        ),
        make_session("scattered", None, date_ms(2026, 1, 3), &filler),
    ];
    let result = filter_and_sort_sessions(&sessions, "opus", SortMode::Relevance, NameFilter::All);
    assert_eq!(ids(&result), ["deep", "early"]);
}

#[test]
fn does_not_match_scattered_characters_in_session_ids() {
    let sessions = [
        make_session(
            "019e9024-aaaa-7bbb-8ccc-dddddddddddd",
            None,
            date_ms(2026, 1, 3),
            "unrelated",
        ),
        make_session(
            "01a1ffff-aaaa-7bbb-8ccc-dddddddddddd",
            Some("#1024 TDD bare env"),
            date_ms(2026, 1, 1),
            "unrelated",
        ),
    ];
    let result = filter_and_sort_sessions(&sessions, "1024", SortMode::Relevance, NameFilter::All);
    assert_eq!(result.len(), 1);
    assert_eq!(result[0].name.as_deref(), Some("#1024 TDD bare env"));

    let result =
        filter_and_sort_sessions(&sessions, "9024-aa", SortMode::Relevance, NameFilter::All);
    assert_eq!(ids(&result), ["019e9024-aaaa-7bbb-8ccc-dddddddddddd"]);
}

#[test]
fn ranks_name_text_then_message_text_then_scattered_name_matches() {
    let sessions = [
        make_session(
            "scattered-name",
            Some("c-a-c-h-e notes"),
            date_ms(2026, 1, 4),
            "unrelated",
        ),
        make_session(
            "message",
            Some("Other work"),
            date_ms(2026, 1, 3),
            "A discussion mentioning the cache.",
        ),
        make_session("name", Some("Cache work"), date_ms(2026, 1, 1), "Summary"),
    ];
    let result = filter_and_sort_sessions(&sessions, "cache", SortMode::Relevance, NameFilter::All);
    assert_eq!(ids(&result), ["name", "message", "scattered-name"]);
}

#[test]
fn matches_each_token_within_a_single_field() {
    let sessions = [make_session(
        "split",
        Some("Fuzzy"),
        date_ms(2026, 1, 1),
        "search ranking",
    )];
    let result = filter_and_sort_sessions(
        &sessions,
        "fuzzy ranking",
        SortMode::Relevance,
        NameFilter::All,
    );
    assert_eq!(ids(&result), ["split"]);
    assert!(
        filter_and_sort_sessions(&sessions, "zzysea", SortMode::Relevance, NameFilter::All)
            .is_empty()
    );
    assert!(
        filter_and_sort_sessions(
            &sessions,
            "re:fuzzy search",
            SortMode::Relevance,
            NameFilter::All
        )
        .is_empty()
    );
}

#[test]
fn ranks_regex_name_matches_first() {
    let sessions = [
        make_session("message", None, date_ms(2026, 1, 2), "brave"),
        make_session("name", Some("Brave"), date_ms(2026, 1, 1), "xxxx"),
    ];
    let result = filter_and_sort_sessions(
        &sessions,
        "re:\\bbrave\\b",
        SortMode::Relevance,
        NameFilter::All,
    );
    assert_eq!(ids(&result), ["name", "message"]);
}

#[test]
fn returns_empty_list_for_invalid_regex() {
    let sessions = [make_session("a", None, date_ms(2026, 1, 1), "brave")];
    for query in ["re:(", "re:{2}", "re:x{2,1}"] {
        let result = filter_and_sort_sessions(&sessions, query, SortMode::Recent, NameFilter::All);
        assert!(result.is_empty(), "{query}");
    }
}

/// JS 无 `u` 标志时 `\b` 是 **ASCII** 词边界：CJK/重音字母属于非单词字符。
#[test]
fn regex_word_boundary_has_js_ascii_semantics() {
    let cjk = [make_session(
        "cjk",
        Some("你好hello world"),
        date_ms(2026, 1, 1),
        "x",
    )];
    assert_eq!(
        ids(&filter_and_sort_sessions(
            &cjk,
            "re:\\bhello",
            SortMode::Recent,
            NameFilter::All
        )),
        ["cjk"]
    );

    let accent = [make_session(
        "accent",
        Some("éfoo"),
        date_ms(2026, 1, 1),
        "x",
    )];
    assert_eq!(
        ids(&filter_and_sort_sessions(
            &accent,
            "re:\\bfoo",
            SortMode::Recent,
            NameFilter::All
        )),
        ["accent"]
    );
    // 内部边界：`éfoo` 里的 `foo` 前面是重音字母（非单词字符），`\b` 成立。
    assert_eq!(
        ids(&filter_and_sort_sessions(
            &accent,
            "re:foo\\b",
            SortMode::Recent,
            NameFilter::All
        )),
        ["accent"]
    );
}

/// JS 的 `\d`/`\w` 只认 ASCII；Rust 默认是 Unicode 类别。
#[test]
fn regex_digit_and_word_classes_have_js_ascii_semantics() {
    let arabic = [make_session(
        "arabic",
        None,
        date_ms(2026, 1, 1),
        "number ٣ here",
    )];
    assert!(
        filter_and_sort_sessions(&arabic, "re:\\d", SortMode::Recent, NameFilter::All).is_empty()
    );
    // 只含阿拉伯数字的会话：`\w` / `^\w+$` 都不应命中（id 也只用非 ASCII 字符）。
    let only_digit = [make_session("٣", None, date_ms(2026, 1, 1), "٣")];
    assert!(
        filter_and_sort_sessions(&only_digit, "re:\\w", SortMode::Recent, NameFilter::All)
            .is_empty()
    );
    assert!(
        filter_and_sort_sessions(&only_digit, "re:^\\w+$", SortMode::Recent, NameFilter::All)
            .is_empty()
    );
    // ASCII 部分仍正常命中。
    assert_eq!(
        ids(&filter_and_sort_sessions(
            &arabic,
            "re:\\w+",
            SortMode::Recent,
            NameFilter::All
        )),
        ["arabic"]
    );
}

/// `\p{...}` 在无 `u` 标志的 JS 里是 identity escape（字面 `p{L}`），不是 Unicode 属性。
#[test]
fn regex_property_escape_is_literal_like_js() {
    let literal = [make_session("literal", None, date_ms(2026, 1, 1), "p{L}")];
    let letters = [make_session("letters", None, date_ms(2026, 1, 1), "abc")];
    assert_eq!(
        ids(&filter_and_sort_sessions(
            &literal,
            "re:\\p{L}",
            SortMode::Recent,
            NameFilter::All
        )),
        ["literal"]
    );
    assert!(
        filter_and_sort_sessions(&letters, "re:\\p{L}", SortMode::Recent, NameFilter::All)
            .is_empty()
    );
}

/// 纯内联标志 `(?i)` 在 JS 里是 SyntaxError（整个查询无匹配）；Rust 默认接受它。
#[test]
fn regex_bare_inline_flag_is_invalid_like_js() {
    let sessions = [make_session("a", None, date_ms(2026, 1, 1), "ABC")];
    assert!(
        filter_and_sort_sessions(&sessions, "re:(?i)abc", SortMode::Recent, NameFilter::All)
            .is_empty()
    );
    // Rust 独有、JS 会抛错的构造同样必须返回空。
    assert!(
        filter_and_sort_sessions(&sessions, "re:(?P<n>a)", SortMode::Recent, NameFilter::All)
            .is_empty()
    );
    assert!(
        filter_and_sort_sessions(&sessions, "re:(?>a)", SortMode::Recent, NameFilter::All)
            .is_empty()
    );
    // JS 支持的修饰符组 `(?i:...)` 仍然有效。
    assert_eq!(
        ids(&filter_and_sort_sessions(
            &sessions,
            "re:(?i:abc)",
            SortMode::Recent,
            NameFilter::All
        )),
        ["a"]
    );
}

/// `.` 不匹配行终止符；无 `m` 标志的 `$` 只匹配输入末尾（JS 不会停在末尾换行之前）。
#[test]
fn regex_dot_and_dollar_follow_js() {
    let newline = [make_session("nl", None, date_ms(2026, 1, 1), "a\nb")];
    assert!(
        filter_and_sort_sessions(&newline, "re:a.b", SortMode::Recent, NameFilter::All).is_empty()
    );
    let trailing = [make_session("trail", None, date_ms(2026, 1, 1), "brave\n")];
    // `new RegExp("^brave$", "i").test("brave\n") === false`。
    assert!(
        filter_and_sort_sessions(&trailing, "re:^brave$", SortMode::Recent, NameFilter::All)
            .is_empty()
    );
    assert!(
        filter_and_sort_sessions(&trailing, "re:brave$", SortMode::Recent, NameFilter::All)
            .is_empty()
    );
    // 整个字段恰好以 "brave\n" 结尾时，`\n` 也必须被模式消费。
    assert_eq!(
        ids(&filter_and_sort_sessions(
            &trailing,
            "re:^brave\\n$",
            SortMode::Recent,
            NameFilter::All
        )),
        ["trail"]
    );
    // `m` 标志下行尾断言仍然生效。
    assert_eq!(
        ids(&filter_and_sort_sessions(
            &trailing,
            "re:(?m:^brave$)",
            SortMode::Recent,
            NameFilter::All
        )),
        ["trail"]
    );
}

/// JS 的字面量 `$` 只在输入末尾成立：正则翻译不能把它改成「末尾行终止符之前」。
#[test]
fn regex_dollar_does_not_match_before_trailing_newline() {
    let sessions = [
        make_session("plain", None, date_ms(2026, 1, 1), "brave"),
        make_session("trail", None, date_ms(2026, 1, 2), "brave\n"),
        make_session("crlf", None, date_ms(2026, 1, 3), "brave\r\n"),
    ];
    assert_eq!(
        ids(&filter_and_sort_sessions(
            &sessions,
            "re:^brave$",
            SortMode::Recent,
            NameFilter::All
        )),
        ["plain"]
    );
}

/// 与 JS 相同的 SyntaxError：这些模式在 TS 里 `new RegExp` 抛错，选择器返回空列表。
#[test]
fn regex_rejects_patterns_js_rejects() {
    let sessions = [make_session(
        "a",
        None,
        date_ms(2026, 1, 1),
        "aaaa brave\n $ * .? a-b",
    )];
    for query in [
        "re:a**",
        "re:a*+",
        "re:a??*",
        "re:$*",
        "re:^?",
        "re:\\b*",
        "re:\\B?",
        "re:.?*",
        "re:a{2}{3}",
        "re:{2}",
        "re:a|*",
        "re:[a--b]",
        "re:[a--]",
        "re:[a-\\-]",
        "re:a{2,1}",
        "re:(?<=a)*",
    ] {
        assert!(
            filter_and_sort_sessions(&sessions, query, SortMode::Recent, NameFilter::All)
                .is_empty(),
            "{query}"
        );
    }
}

/// JS 非 `u` 模式的大小写折叠 = 单码点大写映射（`ſ`/`K` 不参与）。
#[test]
fn regex_case_folding_matches_js_canonicalize() {
    let sessions = [
        make_session("long-s", None, date_ms(2026, 1, 1), "ſ"),
        make_session("kelvin", None, date_ms(2026, 1, 2), "\u{212A}"),
        make_session("ascii-s", None, date_ms(2026, 1, 3), "s"),
        make_session("ascii-S", None, date_ms(2026, 1, 4), "S"),
        make_session("ascii-k", None, date_ms(2026, 1, 5), "k"),
        make_session("ascii-K", None, date_ms(2026, 1, 6), "K"),
        make_session("accent", None, date_ms(2026, 1, 7), "É"),
        make_session("micro", None, date_ms(2026, 1, 8), "μ"),
        make_session("sigma", None, date_ms(2026, 1, 9), "ς"),
        make_session("sigma-cap", None, date_ms(2026, 1, 10), "Σ"),
    ];
    // `/[a-z]/i` 不匹配 ſ 与 Kelvin 符号，只匹配 ASCII 字母。
    assert_eq!(
        ids(&filter_and_sort_sessions(
            &sessions,
            "re:^[a-z]$",
            SortMode::Recent,
            NameFilter::All
        )),
        ["ascii-s", "ascii-S", "ascii-k", "ascii-K"]
    );
    // ASCII `s`/`k` 只折叠到 ASCII 的大写。
    assert_eq!(
        ids(&filter_and_sort_sessions(
            &sessions,
            "re:^s$",
            SortMode::Recent,
            NameFilter::All
        )),
        ["ascii-s", "ascii-S"]
    );
    // 非 ASCII 的单码点大写仍然折叠（é/É、µ/μ/Μ、σ/ς/Σ）。
    assert_eq!(
        ids(&filter_and_sort_sessions(
            &sessions,
            "re:^é$",
            SortMode::Recent,
            NameFilter::All
        )),
        ["accent"]
    );
    assert_eq!(
        ids(&filter_and_sort_sessions(
            &sessions,
            "re:^µ$",
            SortMode::Recent,
            NameFilter::All
        )),
        ["micro"]
    );
    assert_eq!(
        ids(&filter_and_sort_sessions(
            &sessions,
            "re:^σ$",
            SortMode::Recent,
            NameFilter::All
        )),
        ["sigma", "sigma-cap"]
    );
    // 字符类同样做闭包：[µ] 匹配 μ，[σ] 匹配 ς/Σ。
    assert_eq!(
        ids(&filter_and_sort_sessions(
            &sessions,
            "re:^[µ]$",
            SortMode::Recent,
            NameFilter::All
        )),
        ["micro"]
    );
    assert_eq!(
        ids(&filter_and_sort_sessions(
            &sessions,
            "re:^[σ]$",
            SortMode::Recent,
            NameFilter::All
        )),
        ["sigma", "sigma-cap"]
    );
}

/// Annex B 的 legacy octal / identity escape 语义。
#[test]
fn regex_legacy_octal_and_identity_escapes() {
    let newline = [make_session("nl", None, date_ms(2026, 1, 1), "a\nb")];
    assert_eq!(
        ids(&filter_and_sort_sessions(
            &newline,
            "re:\\012",
            SortMode::Recent,
            NameFilter::All
        )),
        ["nl"]
    );
    let control = [make_session("ctl", None, date_ms(2026, 1, 1), "\u{1}")];
    assert_eq!(
        ids(&filter_and_sort_sessions(
            &control,
            "re:\\1",
            SortMode::Recent,
            NameFilter::All
        )),
        ["ctl"]
    );
    // `\400` = `\40`（空格）+ 字面 "0"；`\777` = `\77`（"?"）+ 字面 "7"。
    let space_zero = [make_session("sz", None, date_ms(2026, 1, 1), "x 0y")];
    assert_eq!(
        ids(&filter_and_sort_sessions(
            &space_zero,
            "re:\\400",
            SortMode::Recent,
            NameFilter::All
        )),
        ["sz"]
    );
    let question_seven = [make_session("qs", None, date_ms(2026, 1, 1), "?7")];
    assert_eq!(
        ids(&filter_and_sort_sessions(
            &question_seven,
            "re:\\777",
            SortMode::Recent,
            NameFilter::All
        )),
        ["qs"]
    );
    // `\8` / `\9` 是字面数字。
    let digits = [make_session("digits", None, date_ms(2026, 1, 1), "89")];
    assert_eq!(
        ids(&filter_and_sort_sessions(
            &digits,
            "re:\\8\\9",
            SortMode::Recent,
            NameFilter::All
        )),
        ["digits"]
    );
    // 无具名组时 `\k<a>` 是 identity escape：字面 "k<a>"。
    let literal = [make_session("lit", None, date_ms(2026, 1, 1), "k<a>")];
    assert_eq!(
        ids(&filter_and_sort_sessions(
            &literal,
            "re:\\k<a>",
            SortMode::Recent,
            NameFilter::All
        )),
        ["lit"]
    );
}

/// 字符类区间的 JS 解析：类转义让 `-` 成为字面量，区间顺序按 UTF-16 code unit 校验。
#[test]
fn regex_class_range_parsing_matches_js() {
    let sessions = [
        make_session("dash", None, date_ms(2026, 1, 1), "-"),
        make_session("digit", None, date_ms(2026, 1, 2), "5"),
        make_session("letter", None, date_ms(2026, 1, 3), "b"),
        make_session("range", None, date_ms(2026, 1, 4), "`"),
    ];
    // `[a-\d]` = {a, -, digits}：`-` 不构成区间，`b` 不在其中。
    assert_eq!(
        ids(&filter_and_sort_sessions(
            &sessions,
            "re:^[a-\\d]$",
            SortMode::Recent,
            NameFilter::All
        )),
        ["dash", "digit"]
    );
    // `[--a]` 是 '-' 到 'a' 的合法区间。
    assert_eq!(
        ids(&filter_and_sort_sessions(
            &sessions,
            "re:^[--a]$",
            SortMode::Recent,
            NameFilter::All
        )),
        ["dash", "digit", "letter", "range"]
    );
}

/// 修饰符组的 JS 校验：`(?i-:a)` 合法，`(?i-i:a)` / `(?-:a)` 是 SyntaxError。
#[test]
fn regex_modifier_groups_match_js() {
    let sessions = [make_session("a", None, date_ms(2026, 1, 1), "aA")];
    assert_eq!(
        ids(&filter_and_sort_sessions(
            &sessions,
            "re:(?i-:a)",
            SortMode::Recent,
            NameFilter::All
        )),
        ["a"]
    );
    assert!(
        filter_and_sort_sessions(&sessions, "re:(?i-i:a)", SortMode::Recent, NameFilter::All)
            .is_empty()
    );
    assert!(
        filter_and_sort_sessions(&sessions, "re:(?-:a)", SortMode::Recent, NameFilter::All)
            .is_empty()
    );
    // `(?-i:...)` 在顶层 `i` 的基础上关闭折叠。
    let lower = [make_session("lower", None, date_ms(2026, 1, 1), "a")];
    assert!(
        filter_and_sort_sessions(&lower, "re:(?-i:A)", SortMode::Recent, NameFilter::All)
            .is_empty()
    );
    assert_eq!(
        ids(&filter_and_sort_sessions(
            &lower,
            "re:(?i:A)",
            SortMode::Recent,
            NameFilter::All
        )),
        ["lower"]
    );
}

/// `\x{...}` / `\u{...}` 在 JS 里是 identity escape + 量词（`xx` 等），不是码点转义。
#[test]
fn regex_brace_hex_escape_is_identity_plus_quantifier() {
    let sessions = [make_session("xx", None, date_ms(2026, 1, 1), "xx")];
    assert_eq!(
        ids(&filter_and_sort_sessions(
            &sessions,
            "re:\\x{2}",
            SortMode::Recent,
            NameFilter::All
        )),
        ["xx"]
    );
}

/// 字符类内的 JS 方言：`\D` 的补集语义、`\w`/`\s` 的 ASCII/JS 集合、`\B` 的 ASCII 词边界。
#[test]
fn regex_character_class_dialect_matches_js() {
    let arabic_digit = [make_session("٣", None, date_ms(2026, 1, 1), "٣")];
    assert_eq!(
        ids(&filter_and_sort_sessions(
            &arabic_digit,
            "re:[\\D]",
            SortMode::Recent,
            NameFilter::All
        )),
        ["٣"]
    );
    let hyphen = [make_session("a-b", None, date_ms(2026, 1, 1), "a-b")];
    assert_eq!(
        ids(&filter_and_sort_sessions(
            &hyphen,
            "re:^[\\w-]+$",
            SortMode::Recent,
            NameFilter::All
        )),
        ["a-b"]
    );
    let bom = [make_session("x", None, date_ms(2026, 1, 1), "a\u{feff}b")];
    assert_eq!(
        ids(&filter_and_sort_sessions(
            &bom,
            "re:[\\s]",
            SortMode::Recent,
            NameFilter::All
        )),
        ["x"]
    );
    // U+0085 不在 JS 的 `\s` 集合里。
    let nel = [make_session("x", None, date_ms(2026, 1, 1), "a\u{0085}b")];
    assert!(
        filter_and_sort_sessions(&nel, "re:[\\s]", SortMode::Recent, NameFilter::All).is_empty()
    );
    let accent = [make_session("é", None, date_ms(2026, 1, 1), "é")];
    assert_eq!(
        ids(&filter_and_sort_sessions(
            &accent,
            "re:\\B",
            SortMode::Recent,
            NameFilter::All
        )),
        ["é"]
    );
}

fn named_filter_sessions() -> Vec<SessionInfo> {
    vec![
        make_session(
            "named1",
            Some("My Project"),
            date_ms(2026, 1, 3),
            "blueberry",
        ),
        make_session(
            "named2",
            Some("Another Named"),
            date_ms(2026, 1, 2),
            "blueberry",
        ),
        make_session("other1", None, date_ms(2026, 1, 4), "blueberry"),
        make_session("other2", None, date_ms(2026, 1, 1), "blueberry"),
    ]
}

#[test]
fn name_filter_all_returns_everything() {
    let sessions = named_filter_sessions();
    let result = filter_and_sort_sessions(&sessions, "", SortMode::Recent, NameFilter::All);
    assert_eq!(ids(&result), ["named1", "named2", "other1", "other2"]);
}

#[test]
fn name_filter_named_returns_only_named() {
    let sessions = named_filter_sessions();
    let result = filter_and_sort_sessions(&sessions, "", SortMode::Recent, NameFilter::Named);
    assert_eq!(ids(&result), ["named1", "named2"]);
}

#[test]
fn name_filter_applies_before_search() {
    let sessions = named_filter_sessions();
    let result =
        filter_and_sort_sessions(&sessions, "blueberry", SortMode::Recent, NameFilter::Named);
    assert_eq!(ids(&result), ["named1", "named2"]);
}

#[test]
fn name_filter_excludes_whitespace_only_names() {
    let sessions = [
        make_session("whitespace", Some("   "), date_ms(2026, 1, 1), "test"),
        make_session("empty", Some(""), date_ms(2026, 1, 2), "test"),
        make_session("named", Some("Real Name"), date_ms(2026, 1, 3), "test"),
    ];
    let result = filter_and_sort_sessions(&sessions, "", SortMode::Recent, NameFilter::Named);
    assert_eq!(ids(&result), ["named"]);
}

/// `Date.UTC(...)` 的毫秒值（测试只需要稳定的大小关系）。
fn date_ms(year: i32, month: u32, day: u32) -> f64 {
    use chrono::TimeZone;
    chrono::Utc
        .with_ymd_and_hms(year, month, day, 0, 0, 0)
        .single()
        .expect("valid test date")
        .timestamp_millis() as f64
}
