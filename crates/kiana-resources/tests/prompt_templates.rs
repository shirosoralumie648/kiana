//! 移植 `packages/coding-agent/test/prompt-templates.test.ts`（94 个测试）。
//!
//! describe 分布：`substituteArgs`（37）、`- positional defaults`（10）、
//! `- array slicing`（19）、`parseCommandArgs`（16）、`expandPromptTemplate`（3）、
//! 串联（3）、`loadPromptTemplates - argument-hint`（5）、`- diagnostics`（1）。

use std::path::Path;

use kiana_core::source_info::{SyntheticSourceInfoOptions, create_synthetic_source_info};
use kiana_resources::prompts::PromptTemplate;
use kiana_resources::prompts::args::parse_command_args;
use kiana_resources::prompts::expand::expand_prompt_template;
use kiana_resources::prompts::substitute::substitute_args;
use kiana_resources::prompts::{LoadPromptTemplatesOptions, load_prompt_templates};

fn args(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| value.to_string()).collect()
}

fn template(name: &str, content: &str) -> PromptTemplate {
    PromptTemplate {
        name: name.to_string(),
        description: "test".to_string(),
        argument_hint: None,
        content: content.to_string(),
        source_info: create_synthetic_source_info(
            "/tmp/template.md",
            SyntheticSourceInfoOptions {
                source: "local".to_string(),
                ..Default::default()
            },
        ),
        file_path: "/tmp/template.md".to_string(),
    }
}

// ============================================================================
// substituteArgs
// ============================================================================

#[test]
fn should_replace_arguments_and_at_with_all_args_joined() {
    assert_eq!(
        substitute_args("Test: $ARGUMENTS", &args(&["a", "b", "c"])),
        "Test: a b c"
    );
    assert_eq!(
        substitute_args("Test: $@", &args(&["a", "b", "c"])),
        "Test: a b c"
    );
    let values = args(&["foo", "bar", "baz"]);
    assert_eq!(
        substitute_args("Test: $@", &values),
        substitute_args("Test: $ARGUMENTS", &values)
    );
}

#[test]
fn should_not_recursively_substitute_patterns_in_argument_values() {
    assert_eq!(
        substitute_args("$ARGUMENTS", &args(&["$1", "$ARGUMENTS"])),
        "$1 $ARGUMENTS"
    );
    assert_eq!(substitute_args("$@", &args(&["$100", "$1"])), "$100 $1");
    assert_eq!(
        substitute_args("$ARGUMENTS", &args(&["$100", "$1"])),
        "$100 $1"
    );
}

#[test]
fn should_support_mixed_placeholders() {
    assert_eq!(
        substitute_args("$1: $ARGUMENTS", &args(&["prefix", "a", "b"])),
        "prefix: prefix a b"
    );
    assert_eq!(
        substitute_args("$1: $@", &args(&["prefix", "a", "b"])),
        "prefix: prefix a b"
    );
    assert_eq!(
        substitute_args("$1: $@ ($ARGUMENTS)", &args(&["first", "second", "third"])),
        "first: first second third (first second third)"
    );
}

#[test]
fn should_handle_empty_argument_arrays() {
    assert_eq!(substitute_args("Test: $ARGUMENTS", &[]), "Test: ");
    assert_eq!(substitute_args("Test: $@", &[]), "Test: ");
    assert_eq!(substitute_args("Test: $1", &[]), "Test: ");
}

#[test]
fn should_handle_multiple_occurrences() {
    assert_eq!(
        substitute_args("$ARGUMENTS and $ARGUMENTS", &args(&["a", "b"])),
        "a b and a b"
    );
    assert_eq!(
        substitute_args("$@ and $@", &args(&["a", "b"])),
        "a b and a b"
    );
    assert_eq!(
        substitute_args("$@ and $ARGUMENTS", &args(&["a", "b"])),
        "a b and a b"
    );
}

#[test]
fn should_handle_special_characters_and_out_of_range() {
    assert_eq!(
        substitute_args("$1 $2: $ARGUMENTS", &args(&["arg100", "@user"])),
        "arg100 @user: arg100 @user"
    );
    assert_eq!(
        substitute_args("$1 $2 $3 $4 $5", &args(&["a", "b"])),
        "a b   "
    );
}

#[test]
fn should_handle_unicode_newlines_and_tabs() {
    assert_eq!(
        substitute_args("$ARGUMENTS", &args(&["日本語", "🎉", "café"])),
        "日本語 🎉 café"
    );
    assert_eq!(
        substitute_args("$1 $2", &args(&["line1\nline2", "tab\tthere"])),
        "line1\nline2 tab\tthere"
    );
}

#[test]
fn should_handle_consecutive_dollar_patterns_and_quoted_arguments() {
    assert_eq!(substitute_args("$1$2", &args(&["a", "b"])), "ab");
    assert_eq!(
        substitute_args("$ARGUMENTS", &args(&["first arg", "second arg"])),
        "first arg second arg"
    );
}

#[test]
fn should_handle_single_argument_placeholders() {
    assert_eq!(
        substitute_args("Test: $ARGUMENTS", &args(&["only"])),
        "Test: only"
    );
    assert_eq!(substitute_args("Test: $@", &args(&["only"])), "Test: only");
}

#[test]
fn should_handle_zero_index_and_decimal_patterns() {
    assert_eq!(substitute_args("$0", &args(&["a", "b"])), "");
    assert_eq!(substitute_args("$1.5", &args(&["a"])), "a.5");
}

#[test]
fn should_handle_arguments_as_part_of_words() {
    assert_eq!(
        substitute_args("pre$ARGUMENTS", &args(&["a", "b"])),
        "prea b"
    );
    assert_eq!(substitute_args("pre$@", &args(&["a", "b"])), "prea b");
    assert_eq!(substitute_args("x$ARGUMENTSy", &args(&["a"])), "xay");
}

#[test]
fn should_handle_empty_and_space_padded_arguments() {
    assert_eq!(
        substitute_args("$ARGUMENTS", &args(&["a", "", "c"])),
        "a  c"
    );
    assert_eq!(
        substitute_args("$ARGUMENTS", &args(&["  leading  ", "trailing  "])),
        "  leading   trailing  "
    );
    assert_eq!(
        substitute_args("Prefix $ARGUMENTS suffix", &args(&["ARGUMENTS"])),
        "Prefix ARGUMENTS suffix"
    );
}

#[test]
fn should_handle_non_matching_patterns_and_case() {
    assert_eq!(
        substitute_args("$A $$ $ $ARGS", &args(&["a"])),
        "$A $$ $ $ARGS"
    );
    assert_eq!(
        substitute_args("$arguments $Arguments $ARGUMENTS", &args(&["a", "b"])),
        "$arguments $Arguments a b"
    );
}

#[test]
fn should_handle_both_syntaxes_in_same_command_and_long_lists() {
    let values = args(&["x", "y", "z"]);
    assert_eq!(
        substitute_args("$@ and $ARGUMENTS", &values),
        substitute_args("$ARGUMENTS and $@", &values)
    );
    assert_eq!(
        substitute_args("$@ and $ARGUMENTS", &values),
        "x y z and x y z"
    );

    let values: Vec<String> = (0..100).map(|index| format!("arg{index}")).collect();
    assert_eq!(substitute_args("$ARGUMENTS", &values), values.join(" "));
}

#[test]
fn should_handle_multi_digit_placeholders() {
    assert_eq!(
        substitute_args("$1 $2 $3", &args(&["a", "b", "c"])),
        "a b c"
    );
    let values: Vec<String> = (0..15).map(|index| format!("val{index}")).collect();
    assert_eq!(substitute_args("$10 $12 $15", &values), "val9 val11 val14");
}

#[test]
fn should_preserve_literal_backslash_before_dollar() {
    assert_eq!(substitute_args("Price: \\$100", &[]), "Price: \\");
}

#[test]
fn should_handle_no_placeholders_and_only_placeholders() {
    assert_eq!(
        substitute_args("Just plain text", &args(&["a", "b"])),
        "Just plain text"
    );
    assert_eq!(
        substitute_args("$1 $2 $@", &args(&["a", "b", "c"])),
        "a b a b c"
    );
}

// ============================================================================
// substituteArgs - positional defaults
// ============================================================================

#[test]
fn defaults_use_default_when_positional_arg_is_missing() {
    assert_eq!(
        substitute_args("List exactly ${1:-7} next steps", &[]),
        "List exactly 7 next steps"
    );
    assert_eq!(
        substitute_args("List exactly ${1:-7} next steps", &args(&["3"])),
        "List exactly 3 next steps"
    );
}

#[test]
fn defaults_support_all_arguments() {
    let template = "${@:-default}\n${ARGUMENTS:-default}";
    assert_eq!(substitute_args(template, &[]), "default\ndefault");
    assert_eq!(
        substitute_args(
            template,
            &args(&["This", "would", "be", "the", "arguments"])
        ),
        "This would be the arguments\nThis would be the arguments"
    );
}

#[test]
fn defaults_use_default_when_positional_arg_is_empty() {
    assert_eq!(
        substitute_args("Mode: ${1:-brief}", &args(&[""])),
        "Mode: brief"
    );
}

#[test]
fn defaults_support_multiple_positions() {
    assert_eq!(substitute_args("${1:-7} ${2:-brief}", &[]), "7 brief");
    assert_eq!(
        substitute_args("${1:-7} ${2:-brief}", &args(&["3"])),
        "3 brief"
    );
    assert_eq!(
        substitute_args("${1:-7} ${2:-brief}", &args(&["3", "verbose"])),
        "3 verbose"
    );
}

#[test]
fn defaults_are_not_recursive() {
    assert_eq!(
        substitute_args("${1:-7}", &args(&["$ARGUMENTS"])),
        "$ARGUMENTS"
    );
    assert_eq!(substitute_args("${1:-7}", &args(&["$1"])), "$1");
    assert_eq!(substitute_args("${1:-$ARGUMENTS}", &args(&["a", "b"])), "a");
    assert_eq!(
        substitute_args("${3:-$ARGUMENTS}", &args(&["a", "b"])),
        "$ARGUMENTS"
    );
}

#[test]
fn defaults_support_spaces_and_out_of_range_positions() {
    assert_eq!(substitute_args("${1:-seven steps}", &[]), "seven steps");
    assert_eq!(
        substitute_args("${3:-fallback}", &args(&["a", "b"])),
        "fallback"
    );
    assert_eq!(
        substitute_args("$1 ${2:-x} $ARGUMENTS", &args(&["a"])),
        "a x a"
    );
}

// ============================================================================
// substituteArgs - array slicing
// ============================================================================

#[test]
fn slices_from_index() {
    assert_eq!(
        substitute_args("${@:2}", &args(&["a", "b", "c", "d"])),
        "b c d"
    );
    assert_eq!(substitute_args("${@:1}", &args(&["a", "b", "c"])), "a b c");
    assert_eq!(
        substitute_args("${@:3}", &args(&["a", "b", "c", "d"])),
        "c d"
    );
}

#[test]
fn slices_with_length() {
    assert_eq!(
        substitute_args("${@:2:2}", &args(&["a", "b", "c", "d"])),
        "b c"
    );
    assert_eq!(substitute_args("${@:1:1}", &args(&["a", "b", "c"])), "a");
    assert_eq!(
        substitute_args("${@:3:1}", &args(&["a", "b", "c", "d"])),
        "c"
    );
    assert_eq!(
        substitute_args("${@:2:3}", &args(&["a", "b", "c", "d", "e"])),
        "b c d"
    );
}

#[test]
fn slices_handle_out_of_range_and_zero_length() {
    assert_eq!(substitute_args("${@:99}", &args(&["a", "b"])), "");
    assert_eq!(substitute_args("${@:5}", &args(&["a", "b"])), "");
    assert_eq!(substitute_args("${@:10:5}", &args(&["a", "b"])), "");
    assert_eq!(substitute_args("${@:2:0}", &args(&["a", "b", "c"])), "");
    assert_eq!(substitute_args("${@:1:0}", &args(&["a", "b"])), "");
}

#[test]
fn slices_handle_length_exceeding_array() {
    assert_eq!(substitute_args("${@:2:99}", &args(&["a", "b", "c"])), "b c");
    assert_eq!(substitute_args("${@:1:10}", &args(&["a", "b"])), "a b");
}

#[test]
fn slices_process_before_simple_at() {
    assert_eq!(
        substitute_args("${@:2} vs $@", &args(&["a", "b", "c"])),
        "b c vs a b c"
    );
    assert_eq!(
        substitute_args("First: ${@:1:1}, All: $@", &args(&["x", "y", "z"])),
        "First: x, All: x y z"
    );
}

#[test]
fn slices_do_not_recurse() {
    assert_eq!(
        substitute_args("${@:1}", &args(&["${@:2}", "test"])),
        "${@:2} test"
    );
    assert_eq!(
        substitute_args("${@:2}", &args(&["a", "${@:3}", "c"])),
        "${@:3} c"
    );
}

#[test]
fn slices_mix_with_positional_args() {
    assert_eq!(
        substitute_args("$1: ${@:2}", &args(&["cmd", "arg1", "arg2"])),
        "cmd: arg1 arg2"
    );
    assert_eq!(
        substitute_args("$1 $2 ${@:3}", &args(&["a", "b", "c", "d"])),
        "a b c d"
    );
}

#[test]
fn slices_treat_zero_as_all_args() {
    assert_eq!(substitute_args("${@:0}", &args(&["a", "b", "c"])), "a b c");
}

#[test]
fn slices_handle_empty_and_single_arg_arrays() {
    assert_eq!(substitute_args("${@:2}", &[]), "");
    assert_eq!(substitute_args("${@:1}", &[]), "");
    assert_eq!(substitute_args("${@:1}", &args(&["only"])), "only");
    assert_eq!(substitute_args("${@:2}", &args(&["only"])), "");
}

#[test]
fn slices_in_middle_of_text_and_multiple_slices() {
    assert_eq!(
        substitute_args("Process ${@:2} with $1", &args(&["tool", "file1", "file2"])),
        "Process file1 file2 with tool"
    );
    assert_eq!(
        substitute_args("${@:1:1} and ${@:2}", &args(&["a", "b", "c"])),
        "a and b c"
    );
    assert_eq!(
        substitute_args("${@:1:2} vs ${@:3:2}", &args(&["a", "b", "c", "d", "e"])),
        "a b vs c d"
    );
}

#[test]
fn slices_handle_quoted_special_and_unicode_arguments() {
    assert_eq!(
        substitute_args("${@:2}", &args(&["cmd", "first arg", "second arg"])),
        "first arg second arg"
    );
    assert_eq!(
        substitute_args("${@:2}", &args(&["cmd", "$100", "@user", "#tag"])),
        "$100 @user #tag"
    );
    assert_eq!(
        substitute_args("${@:1}", &args(&["日本語", "🎉", "café"])),
        "日本語 🎉 café"
    );
}

#[test]
fn slices_combine_with_positional_and_wildcard_placeholders() {
    let template = "Run $1 on ${@:2:2}, then process $@";
    let values = args(&["eslint", "file1.ts", "file2.ts", "file3.ts"]);
    assert_eq!(
        substitute_args(template, &values),
        "Run eslint on file1.ts file2.ts, then process eslint file1.ts file2.ts file3.ts"
    );
}

#[test]
fn slices_without_spacing_and_large_lengths() {
    assert_eq!(
        substitute_args("prefix${@:2}suffix", &args(&["a", "b", "c"])),
        "prefixb csuffix"
    );
    let values: Vec<String> = (0..10).map(|index| format!("arg{}", index + 1)).collect();
    assert_eq!(
        substitute_args("${@:5:100}", &values),
        "arg5 arg6 arg7 arg8 arg9 arg10"
    );
}

// ============================================================================
// parseCommandArgs
// ============================================================================

#[test]
fn should_parse_simple_space_separated_arguments() {
    assert_eq!(parse_command_args("a b c"), vec!["a", "b", "c"]);
}

#[test]
fn should_parse_quoted_arguments() {
    assert_eq!(
        parse_command_args("\"first arg\" second"),
        vec!["first arg", "second"]
    );
    assert_eq!(
        parse_command_args("'first arg' second"),
        vec!["first arg", "second"]
    );
    assert_eq!(
        parse_command_args("\"double\" 'single' \"double again\""),
        vec!["double", "single", "double again"]
    );
}

#[test]
fn should_handle_empty_input_and_extra_spaces() {
    assert!(parse_command_args("").is_empty());
    assert_eq!(parse_command_args("a  b   c"), vec!["a", "b", "c"]);
}

#[test]
fn should_handle_tabs_as_separators() {
    assert_eq!(parse_command_args("a\tb\tc"), vec!["a", "b", "c"]);
}

#[test]
fn should_handle_quoted_empty_string() {
    assert_eq!(parse_command_args("\"\" \" \""), vec!["", " "]);
    assert_eq!(parse_command_args("a '' c"), vec!["a", "", "c"]);
    assert_eq!(parse_command_args("a \"\""), vec!["a", ""]);
}

#[test]
fn should_handle_special_and_unicode_characters() {
    assert_eq!(
        parse_command_args("$100 @user #tag"),
        vec!["$100", "@user", "#tag"]
    );
    assert_eq!(
        parse_command_args("日本語 🎉 café"),
        vec!["日本語", "🎉", "café"]
    );
}

#[test]
fn should_handle_newlines_in_quoted_arguments() {
    assert_eq!(
        parse_command_args("\"line1\nline2\" second"),
        vec!["line1\nline2", "second"]
    );
}

#[test]
fn should_treat_unquoted_newlines_as_separators() {
    assert_eq!(
        parse_command_args("label-2\n\nHere is some description #2."),
        vec!["label-2", "Here", "is", "some", "description", "#2."]
    );
}

#[test]
fn should_collapse_mixed_unquoted_whitespace() {
    assert_eq!(parse_command_args("a\n\n\tb  c"), vec!["a", "b", "c"]);
}

#[test]
fn should_handle_escaped_quotes_inside_quoted_strings() {
    assert_eq!(
        parse_command_args("\"quoted \\\"text\\\"\""),
        vec!["quoted \\text\\"]
    );
}

#[test]
fn should_handle_trailing_and_leading_spaces() {
    assert_eq!(parse_command_args("a b c   "), vec!["a", "b", "c"]);
    assert_eq!(parse_command_args("   a b c"), vec!["a", "b", "c"]);
}

// ============================================================================
// expandPromptTemplate
// ============================================================================

#[test]
fn should_use_positional_default_for_empty_quoted_argument() {
    let templates = [template("review", "Review $2. Focus: ${1:-correctness}.")];
    assert_eq!(
        expand_prompt_template("/review \"\" src/index.ts", &templates),
        "Review src/index.ts. Focus: correctness."
    );
    assert_eq!(
        expand_prompt_template("/review '' src/index.ts", &templates),
        "Review src/index.ts. Focus: correctness."
    );
}

#[test]
fn should_split_template_arguments_on_unquoted_newlines() {
    let templates = [template("arg-test", "- arg1: $1\n- rest: ${@:2}")];
    assert_eq!(
        expand_prompt_template(
            "/arg-test label-2\n\nHere is some description #2.",
            &templates
        ),
        "- arg1: label-2\n- rest: Here is some description #2."
    );
}

#[test]
fn should_support_template_command_separated_from_args_by_newline() {
    let templates = [template("arg-test", "arg1: $1")];
    assert_eq!(
        expand_prompt_template("/arg-test\nlabel-2", &templates),
        "arg1: label-2"
    );
}

// ============================================================================
// parseCommandArgs + substituteArgs integration
// ============================================================================

#[test]
fn should_parse_and_substitute_together() {
    let values = parse_command_args("Button \"onClick handler\" \"disabled support\"");
    assert_eq!(
        substitute_args("Create component $1 with features: $ARGUMENTS", &values),
        "Create component Button with features: Button onClick handler disabled support"
    );
    assert_eq!(
        substitute_args(
            "Create a React component named $1 with features: $ARGUMENTS",
            &values
        ),
        "Create a React component named Button with features: Button onClick handler disabled support"
    );
}

#[test]
fn should_produce_same_result_with_at_and_arguments() {
    let values = parse_command_args("feature1 feature2 feature3");
    assert_eq!(
        substitute_args("Implement: $@", &values),
        substitute_args("Implement: $ARGUMENTS", &values)
    );
}

// ============================================================================
// loadPromptTemplates
// ============================================================================

fn write_file(dir: &Path, name: &str, content: &str) {
    std::fs::create_dir_all(dir).expect("create dir");
    std::fs::write(dir.join(name), content).expect("write template");
}

fn load_from(dir: &Path) -> kiana_resources::prompts::LoadPromptTemplatesResult {
    load_prompt_templates(LoadPromptTemplatesOptions {
        cwd: std::env::current_dir()
            .unwrap()
            .to_string_lossy()
            .into_owned(),
        agent_dir: kiana_core::config::get_agent_dir()
            .to_string_lossy()
            .into_owned(),
        prompt_paths: vec![dir.to_string_lossy().into_owned()],
        include_defaults: false,
    })
}

#[test]
fn should_parse_required_argument_hint_from_frontmatter() {
    let temp = tempfile::tempdir().expect("tempdir");
    write_file(
        temp.path(),
        "pr.md",
        "---\ndescription: Review PRs from URLs with structured issue and code analysis\nargument-hint: \"<PR-URL>\"\n---\nYou are given one or more GitHub PR URLs: $@",
    );
    let result = load_from(temp.path());
    let pr = result
        .templates
        .iter()
        .find(|template| template.name == "pr")
        .expect("pr");
    assert_eq!(pr.argument_hint.as_deref(), Some("<PR-URL>"));
    assert_eq!(
        pr.description,
        "Review PRs from URLs with structured issue and code analysis"
    );
}

#[test]
fn should_parse_optional_argument_hint_from_frontmatter() {
    let temp = tempfile::tempdir().expect("tempdir");
    write_file(
        temp.path(),
        "wr.md",
        "---\ndescription: Finish the current task end-to-end with changelog, commit, and push\nargument-hint: \"[instructions]\"\n---\nWrap it. Additional instructions: $ARGUMENTS",
    );
    let result = load_from(temp.path());
    let wr = result
        .templates
        .iter()
        .find(|template| template.name == "wr")
        .expect("wr");
    assert_eq!(wr.argument_hint.as_deref(), Some("[instructions]"));
    assert_eq!(
        wr.description,
        "Finish the current task end-to-end with changelog, commit, and push"
    );
}

#[test]
fn should_leave_argument_hint_absent_when_not_specified() {
    let temp = tempfile::tempdir().expect("tempdir");
    write_file(
        temp.path(),
        "cl.md",
        "---\ndescription: Audit changelog entries before release\n---\nAudit changelog entries for all commits since the last release.",
    );
    let result = load_from(temp.path());
    let cl = result
        .templates
        .iter()
        .find(|template| template.name == "cl")
        .expect("cl");
    assert_eq!(cl.argument_hint, None);
}

#[test]
fn should_ignore_empty_argument_hint() {
    let temp = tempfile::tempdir().expect("tempdir");
    write_file(
        temp.path(),
        "empty-hint.md",
        "---\ndescription: A command with empty hint\nargument-hint: \"\"\n---\nDo something",
    );
    let result = load_from(temp.path());
    let template = result
        .templates
        .iter()
        .find(|template| template.name == "empty-hint")
        .expect("empty-hint");
    assert_eq!(template.argument_hint, None);
}

#[test]
fn should_preserve_argument_hint_with_special_characters() {
    let temp = tempfile::tempdir().expect("tempdir");
    write_file(
        temp.path(),
        "is.md",
        "---\ndescription: Analyze GitHub issues (bugs or feature requests)\nargument-hint: \"<issue>\"\n---\nAnalyze GitHub issue(s): $ARGUMENTS",
    );
    let result = load_from(temp.path());
    let is = result
        .templates
        .iter()
        .find(|template| template.name == "is")
        .expect("is");
    assert_eq!(is.argument_hint.as_deref(), Some("<issue>"));
}

#[test]
fn should_report_invalid_yaml_frontmatter_and_keep_valid_siblings() {
    let temp = tempfile::tempdir().expect("tempdir");
    write_file(
        temp.path(),
        "invalid.md",
        "---\ndescription: Broken: unquoted colon\n---\nDo something.\n",
    );
    write_file(temp.path(), "valid.md", "Valid prompt content.");
    let result = load_from(temp.path());
    assert_eq!(
        result
            .templates
            .iter()
            .map(|template| template.name.as_str())
            .collect::<Vec<_>>(),
        vec!["valid"]
    );
    assert_eq!(result.diagnostics.len(), 1);
    let diagnostic = &result.diagnostics[0];
    assert_eq!(
        diagnostic.kind,
        kiana_core::diagnostics::DiagnosticKind::Warning
    );
    assert_eq!(
        diagnostic.path.as_deref(),
        Some(temp.path().join("invalid.md").to_string_lossy().as_ref())
    );
    assert!(diagnostic.message.contains("line 1, column 14"));
}

#[test]
fn should_load_templates_in_sorted_readdir_order() {
    let temp = tempfile::tempdir().expect("tempdir");
    write_file(temp.path(), "zeta.md", "zeta $ARGUMENTS");
    write_file(temp.path(), "alpha.md", "alpha $ARGUMENTS");
    write_file(temp.path(), "mike.md", "mike $ARGUMENTS");
    let result = load_from(temp.path());
    assert_eq!(
        result
            .templates
            .iter()
            .map(|template| template.name.as_str())
            .collect::<Vec<_>>(),
        vec!["alpha", "mike", "zeta"]
    );
}

// ============================================================================
// 回归：首行描述按 UTF-16 截断（不拆代理对）
// ============================================================================

#[test]
fn first_line_description_backs_off_at_a_surrogate_pair() {
    let temp = tempfile::tempdir().expect("tempdir");
    let line = format!("{}😀zzz", "a".repeat(59));
    write_file(temp.path(), "astral.md", &line);
    let result = load_from(temp.path());
    let template = result
        .templates
        .iter()
        .find(|template| template.name == "astral")
        .expect("astral");
    // TS 的 slice(0, 60) 会留下孤立高代理项；Rust 的 String 无法表示，按
    // ARCHITECTURE §4.6 延后到 59 个 code unit 再截断。
    assert_eq!(template.description, format!("{}...", "a".repeat(59)));
}

#[test]
fn first_line_description_keeps_a_complete_surrogate_pair_at_the_cut() {
    let temp = tempfile::tempdir().expect("tempdir");
    let exact = "😀".repeat(30);
    write_file(temp.path(), "exact.md", &exact);
    let result = load_from(temp.path());
    let template = result
        .templates
        .iter()
        .find(|template| template.name == "exact")
        .expect("exact");
    // 恰好 60 个 UTF-16 code unit：不截断、不加 "..."。
    assert_eq!(template.description, exact);

    let longer = format!("{}x", "😀".repeat(30));
    write_file(temp.path(), "longer.md", &longer);
    let result = load_from(temp.path());
    let template = result
        .templates
        .iter()
        .find(|template| template.name == "longer")
        .expect("longer");
    // 第 60 个 code unit 正好是代理对的末尾：完整保留 30 个 emoji + "..."。
    assert_eq!(template.description, format!("{}...", "😀".repeat(30)));
}
