//! 词分段回归：`Intl.Segmenter("word")`（Node 26.12.1 / ICU 78.3）的边界与 `isWordLike`。
//!
//! 期望值先在 Node 上跑出来（`[...new Intl.Segmenter(undefined, {granularity: "word"}).segment(t)]`），
//! 再逐段断言。覆盖三类 ICU4X 与 ICU 的差异：
//!
//! - **SA 脚本与拉丁/数字连写**：JS 里 `สวัสดีabc` 是**一个**词（UAX #29 把 SA 与字母/数字归入同一类），
//!   ICU4X 的词典模式却按脚本切开，需要再合回去。
//! - **词典段内部的词边界**：ICU 词典不认得的词整段给出，而 UAX #29 在这些脚本里仍有边界。
//! - **`_` / 中点字符 / 标记** 的连接规则：`snake_case_name` 是一段，`a.1` 是三段。

use kiana_tui::text::get_word_segments;

/// 断言 `(segment, isWordLike, index)` 三项。
fn assert_segments(text: &str, expected: &[(&str, bool, usize)]) {
    let actual: Vec<(String, bool, usize)> = get_word_segments(text)
        .into_iter()
        .map(|segment| (segment.segment, segment.is_word_like, segment.index))
        .collect();
    let expected: Vec<(String, bool, usize)> = expected
        .iter()
        .map(|&(segment, word_like, index)| (segment.to_string(), word_like, index))
        .collect();
    assert_eq!(actual, expected, "{text:?}");
}

// ---------------------------------------------------------------------------
// 词典模式：CJK 词
// ---------------------------------------------------------------------------

#[test]
fn cjk_words_come_from_the_dictionary() {
    assert_segments(
        "你好世界 test",
        &[
            ("你好", true, 0),
            ("世界", true, 2),
            (" ", false, 4),
            ("test", true, 5),
        ],
    );
    assert_segments("こんにちは", &[("こんにちは", true, 0)]);
}

/// 汉字/谚文/片假名与拉丁字母之间断开；平假名（词典词）与拉丁也断开。
#[test]
fn cjk_and_latin_break_apart() {
    assert_segments(
        "컴퓨터testfoo.bar你好世界",
        &[
            ("컴퓨터", true, 0),
            ("testfoo.bar", true, 3),
            ("你好", true, 14),
            ("世界", true, 16),
        ],
    );
    assert_segments("한국어abc", &[("한국어", true, 0), ("abc", true, 3)]);
    assert_segments("カタカナabc", &[("カタカナ", true, 0), ("abc", true, 4)]);
}

// ---------------------------------------------------------------------------
// SA（泰/老/高棉/缅）与字母、数字、`_` 连写
// ---------------------------------------------------------------------------

/// `สวัสดีabc` 在 JS 里是一段（不是 `สวัสดี` + `abc`）。
#[test]
fn sa_scripts_merge_with_latin_and_digits() {
    assert_segments("สวัสดีabc", &[("สวัสดีabc", true, 0)]);
    assert_segments("abcสวัสดี", &[("abcสวัสดี", true, 0)]);
    assert_segments(
        "สวัสดีabc def",
        &[("สวัสดีabc", true, 0), (" ", false, 9), ("def", true, 10)],
    );
}

/// 词典本身认得泰文词时，按词切（这些词在 ICU 78.3 与 ICU4X 的词典里一致）。
#[test]
fn thai_dictionary_words_are_segmented() {
    assert_segments("สวัสดีครับ", &[("สวัสดี", true, 0), ("ครับ", true, 6)]);
    assert_segments(
        "สมัครสมาชิกแล้ว",
        &[("สมัคร", true, 0), ("สมาชิก", true, 5), ("แล้ว", true, 11)],
    );
}

// ---------------------------------------------------------------------------
// `_` 与中点字符
// ---------------------------------------------------------------------------

#[test]
fn underscore_and_mid_letters_join_word_runs() {
    assert_segments("snake_case_name", &[("snake_case_name", true, 0)]);
    assert_segments("foo.bar", &[("foo.bar", true, 0)]);
    assert_segments("com.example.MyClass", &[("com.example.MyClass", true, 0)]);
    assert_segments("1.5", &[("1.5", true, 0)]);
}

/// 中点字符只连非 CJK 的字母/数字；`a.1` 与 `中.a` 都断开（JS 的 MidNum 规则只认"数字.数字"）。
#[test]
fn mid_letters_break_around_cjk_and_mixed_word_classes() {
    assert_segments("a.1", &[("a", true, 0), (".", false, 1), ("1", true, 2)]);
    assert_segments("中.a", &[("中", true, 0), (".", false, 1), ("a", true, 2)]);
    assert_segments("a.中", &[("a", true, 0), (".", false, 1), ("中", true, 2)]);
}

// ---------------------------------------------------------------------------
// 标点、空白与其它
// ---------------------------------------------------------------------------

#[test]
fn punctuation_and_whitespace_are_not_word_like() {
    assert_segments(
        "hello world",
        &[("hello", true, 0), (" ", false, 5), ("world", true, 6)],
    );
    assert_segments(
        "path/to/file",
        &[
            ("path", true, 0),
            ("/", false, 4),
            ("to", true, 5),
            ("/", false, 7),
            ("file", true, 8),
        ],
    );
    assert_segments(
        "e=mc^2",
        &[
            ("e", true, 0),
            ("=", false, 1),
            ("mc", true, 2),
            ("^", false, 4),
            ("2", true, 5),
        ],
    );
}

/// 区域指示符对不是词（`isWordLike: false`），且按码点计两个 UTF-16 码元。
#[test]
fn flag_emoji_segments_are_not_word_like() {
    assert_segments("🇺🇸🇬🇧", &[("🇺🇸", false, 0), ("🇬🇧", false, 4)]);
}

// ---------------------------------------------------------------------------
// 评审回归（critical #2）：`isWordLike` 来自 ICU 规则状态标签，而不是"含 word-like 码点"
// ---------------------------------------------------------------------------

/// `_` 是 `ExtendNumLet`：单独一个没有标签（`isWordLike: false`），两个连写命中
/// rule 13a/13b 拿到 `{200}`（`true`）。旧实现用"任一码点在 word-like 区间"判定，
/// 两种情形都给出 `true`，于是 `findWordForward("/__/", 0)` 返回 4 而不是 1。
#[test]
fn double_underscore_is_word_like_and_single_is_not() {
    assert_segments("__", &[("__", true, 0)]);
    assert_segments("_", &[("_", false, 0)]);
}

/// 谚文音节 + 组合标记：ICU 的规则 999 分支没有标签，`isWordLike` 为 `false`。
/// 旧实现按"含谚文码点"给出 `true`。
#[test]
fn hangul_syllable_with_combining_mark_is_not_word_like() {
    assert_segments("한́", &[("한́", false, 0)]);
}

/// `가〮` 是 HangulSyllable + Extend（U+302E 不在 `$Han` 里，留在 `$Extend`），
/// 与后面的音节之间没有规则把两者连起来：两段，前段无标签。
#[test]
fn hangul_syllable_plus_mark_splits_from_a_following_syllable() {
    assert_segments("가〮나", &[("가〮", false, 0), ("나", true, 2)]);
}

/// 泰文 + 组合标记 + 拉丁：一个 word-like 段（`$ALetterPlus` 收 `$ComplexContext`，
/// `$ExFm` 收 `U+0301`）。
#[test]
fn thai_with_combining_mark_and_latin_is_one_word_like_segment() {
    assert_segments("ก́x", &[("ก́x", true, 0)]);
}

// ---------------------------------------------------------------------------
// 评审回归（critical #3）：片假名长音符 U+30FC 不被 `Script=Katakana` 覆盖
// ---------------------------------------------------------------------------

/// `ー`（U+30FC KATAKANA-HIRAGANA PROLONGED SOUND MARK）的 `Script` 是 `Common`，
/// 只在 `Script_Extensions` 里含片假名。ICU 的 `$Katakana` 用 `Script_Extensions`，
/// 所以整词不断；旧实现漏掉这 54 个码点，`コンピュータ` 被切成 `コンピュ`/`ータ`。
#[test]
fn katakana_words_with_prolonged_sound_mark_stay_whole() {
    assert_segments("コンピュータ", &[("コンピュータ", true, 0)]);
    assert_segments("キーボード", &[("キーボード", true, 0)]);
    assert_segments("コーヒー", &[("コーヒー", true, 0)]);
    assert_segments("データベース", &[("データベース", true, 0)]);
}

// ---------------------------------------------------------------------------
// 评审回归（critical #1）：泰文/日文按 ICU 词典段切，不由后处理拼接
// ---------------------------------------------------------------------------

/// ICU 78.3 的泰文词典把 `จากนั้น` 切成 `จาก`/`นั้น`（这两段在 ICU4X 的词典里也存在）。
#[test]
fn thai_words_split_where_the_rule_and_dictionary_engines_agree() {
    assert_segments("จากนั้น", &[("จาก", true, 0), ("นั้น", true, 3)]);
    assert_segments(
        "สมัครสมาชิกแล้ว",
        &[("สมัคร", true, 0), ("สมาชิก", true, 5), ("แล้ว", true, 11)],
    );
}

/// ICU 的 SA 引擎有"至少 N 个码点才切"的门槛（`*_MIN_WORD_SPAN`），泰文比别人多一档
/// （`ThaiBreakEngine` 用 `>=` 比较，恰好 `2 * MIN_WORD` 也提前返回）。
/// 短段整段保留，长段才采纳词典边界。
#[test]
fn short_thai_runs_are_not_subdivided() {
    // 3 个码点：低于门槛，整段（TS 也是整段）。
    assert_segments("จากน", &[("จากน", true, 0)]);
    // 5 个码点：达到门槛，采纳词典边界（TS 一致）。
    assert_segments("จากนั้น", &[("จาก", true, 0), ("นั้น", true, 3)]);
}

/// 老挝语的门槛是 `< 4`（与泰文差一档）。
#[test]
fn lao_runs_follow_the_lao_min_word_span() {
    assert_segments("ຈອນ", &[("ຈອນ", true, 0)]);
}

// ---------------------------------------------------------------------------
// 评审回归（major #1）：`UnhandledEngine` 按整个 Script 记忆，`Script=Common` 的
// `゛`/`゜` 之后紧跟的 `ー` 不再交给 `CjkBreakEngine`
// ---------------------------------------------------------------------------

/// `゛`/`゜`（U+309B/U+309C）的 `Script=Common`：`loadEngineFor` 造不出引擎，由
/// `UnhandledEngine` 接手；它按整个 Script 记忆，把紧随其后的 `ー`（同为 Common）
/// 一并吞掉，因此 `ー` 不会触发 `CjkBreakEngine` 的切分。
#[test]
fn marks_take_the_following_prolonged_sound_mark_with_them() {
    // ICU 78.3：`な` | `よ゛゜ーこ゛ぎ`（中间不切）。
    assert_segments(
        "なよ゛゜ーこ゛ぎ",
        &[("な", true, 0), ("よ゛゜ーこ゛ぎ", true, 1)],
    );
    assert_segments(
        "れわきべをぽだつせぬぴばざこ゜ーひだ",
        &[
            ("れ", true, 0),
            ("わき", true, 1),
            ("べ", true, 3),
            ("を", true, 4),
            ("ぽ", true, 5),
            ("だ", true, 6),
            ("つ", true, 7),
            ("せ", true, 8),
            ("ぬ", true, 9),
            ("ぴ", true, 10),
            ("ば", true, 11),
            ("ざこ゜ーひだ", true, 12),
        ],
    );
    assert_segments("あ゛ーい", &[("あ゛ーい", true, 0)]);
    assert_segments("゛ーこ", &[("゛ーこ", true, 0)]);
    assert_segments(
        "ﾏｲﾇｴｨｰﾃｶﾜﾈﾑｰｰﾖﾚﾉｲﾜ",
        &[
            ("ﾏｲ", true, 0),
            ("ﾇｴ", true, 2),
            ("ｨ", true, 4),
            ("ｰ", true, 5),
            ("ﾃ", true, 6),
            ("ｶ", true, 7),
            ("ﾜ", true, 8),
            ("ﾈﾑ", true, 9),
            ("ｰ", true, 11),
            ("ｰ", true, 12),
            ("ﾖ", true, 13),
            ("ﾚ", true, 14),
            ("ﾉｲ", true, 15),
            ("ﾜ", true, 17),
        ],
    );
}

/// `UnhandledEngine` 的 Script 集合是**每次 `segment()` 调用**（每个迭代器）各自的，
/// 且长度 <= 1 个 UTF-16 码元的规则段不会触发 `populateDictionary`：
/// 被空格/换行分开的 `゛` 不会影响后面的 `ー`。
#[test]
fn unhandled_state_is_per_segment_call_and_skips_tiny_rule_segments() {
    assert_segments(
        "゛ ーこ",
        &[
            ("゛", true, 0),
            (" ", false, 1),
            ("ー", true, 2),
            ("こ", true, 3),
        ],
    );
    // `゛゙`（゛ + 组合浊点）是 2 个码元的规则段：`UnhandledEngine` 已记住 Common，
    // 但组合点 `U+3099`（Script=Inherited）终止了吞噬；空格之后的 `ーこ` 整段保留。
    assert_segments(
        "゛゙ ーこ",
        &[("゛゙", true, 0), (" ", false, 2), ("ーこ", true, 3)],
    );
    // `゛い` 段先让 Cjk 引擎进入本迭代器的引擎列表（它排在 Unhandled 之前），
    // 后面的 `ー` 重新交给 Cjk：`ー`/`こ` 两段。
    assert_segments(
        "゛い ーこ",
        &[
            ("゛い", true, 0),
            (" ", false, 2),
            ("ー", true, 3),
            ("こ", true, 4),
        ],
    );
}

/// 黄金数据里的长音符切分：`ー` 作为 fSet 成员由 `CjkBreakEngine` 处理。
#[test]
fn prolonged_sound_mark_starts_a_cjk_run_when_no_unhandled_engine_intervenes() {
    assert_segments("ーこ", &[("ー", true, 0), ("こ", true, 1)]);
    assert_segments(
        "ざこーひだ",
        &[("ざこ", true, 0), ("ー", true, 2), ("ひだ", true, 3)],
    );
}
