//! `packages/tui/test/autocomplete-skill-slash.test.ts`（7 个用例；函数文档保留标题以便对照）。
//!
//! TS 源：`descripe("CombinedAutocompleteProvider slash-command filter")`。命令表是
//! `skill:` 折叠逻辑（#9944 回归）的固定夹具。

use kiana_tui::autocomplete::{
    AutocompleteItem, AutocompleteProvider, CombinedAutocompleteProvider, CommandEntry,
    GetSuggestionsOptions, SlashCommand,
};
use tokio_util::sync::CancellationToken;

fn commands() -> Vec<CommandEntry> {
    vec![
        CommandEntry::Slash(
            SlashCommand::new("skill:deep-research").with_description("Multi-agent deep research"),
        ),
        CommandEntry::Slash(
            SlashCommand::new("skill:research-idea")
                .with_description("Refine a raw idea into a falsifiable seed"),
        ),
        CommandEntry::Slash(
            SlashCommand::new("skill:to-sidecar").with_description("Route work to a sidecar"),
        ),
        CommandEntry::Slash(
            SlashCommand::new("skill:brainstorm").with_description("Generate ideas"),
        ),
        CommandEntry::Slash(SlashCommand::new("model").with_description("Select the active model")),
    ]
}

async fn suggestions_for(prefix: &str) -> Vec<String> {
    let mut provider = CombinedAutocompleteProvider::new(commands(), current_dir(), None);
    let line = format!("/{prefix}");
    let options = GetSuggestionsOptions::new(CancellationToken::new(), false);
    let result = provider
        .get_suggestions(
            std::slice::from_ref(&line),
            0,
            line.chars().count(),
            &options,
        )
        .await
        .unwrap_or_else(|| panic!("expected suggestions for \"/{prefix}\""));
    result.items.into_iter().map(|item| item.value).collect()
}

fn current_dir() -> String {
    std::env::current_dir()
        .expect("cwd")
        .to_string_lossy()
        .into_owned()
}

#[tokio::test]
async fn ranks_skill_research_idea_first_for_query_idea() {
    let items = suggestions_for("idea").await;
    assert_eq!(items[0], "skill:research-idea");
    let position = |needle: &str| items.iter().position(|item| item == needle).unwrap();
    assert!(position("skill:deep-research") > position("skill:research-idea"));
}

#[tokio::test]
async fn keeps_ordinary_slash_commands_matching() {
    let items = suggestions_for("mod").await;
    assert!(items.iter().any(|item| item == "model"));
}

#[tokio::test]
async fn completes_commands_after_leading_whitespace_and_preserves_it() {
    let mut provider = CombinedAutocompleteProvider::new(
        vec![CommandEntry::Slash(SlashCommand::new("model"))],
        current_dir(),
        None,
    );

    let cases: &[(&str, &str)] = &[
        (" /", " /model "),
        ("  /mod", "  /model "),
        ("\t/mod", "\t/model "),
    ];
    for &(line, expected) in cases {
        let options = GetSuggestionsOptions::new(CancellationToken::new(), false);
        let result = provider
            .get_suggestions(&[line.to_string()], 0, line.chars().count(), &options)
            .await
            .expect("suggestions");
        assert_eq!(result.prefix, line.trim_start());
        assert_eq!(
            result
                .items
                .iter()
                .map(|item| item.value.as_str())
                .collect::<Vec<_>>(),
            vec!["model"]
        );

        let applied = provider.apply_completion(
            &[line.to_string()],
            0,
            line.chars().count(),
            &result.items[0],
            &result.prefix,
        );
        assert_eq!(applied.lines[0], expected);
        assert_eq!(applied.cursor_col, expected.chars().count());
    }
}

#[tokio::test]
async fn completes_command_arguments_after_leading_whitespace() {
    let command = SlashCommand::new("model").with_argument_completions(|prefix: &str| {
        // TS 在闭包里 `assert.equal(prefix, "son")`。
        assert_eq!(prefix, "son");
        Some(vec![AutocompleteItem::new("sonnet", "sonnet")])
    });
    let mut provider =
        CombinedAutocompleteProvider::new(vec![CommandEntry::Slash(command)], current_dir(), None);

    let line = "  /model son";
    let options = GetSuggestionsOptions::new(CancellationToken::new(), false);
    let result = provider
        .get_suggestions(&[line.to_string()], 0, line.chars().count(), &options)
        .await
        .expect("suggestions");
    assert_eq!(result.prefix, "son");

    let applied = provider.apply_completion(
        &[line.to_string()],
        0,
        line.chars().count(),
        &result.items[0],
        &result.prefix,
    );
    assert_eq!(applied.lines[0], "  /model sonnet");
}

#[tokio::test]
async fn keeps_explicit_skill_queries_working() {
    let items = suggestions_for("skill:side").await;
    assert!(items.iter().any(|item| item == "skill:to-sidecar"));
}

/// Regression test for #9944.
#[tokio::test]
async fn lists_skills_while_typing_the_skill_prefix() {
    let items = suggestions_for("skill").await;
    let filtered: Vec<String> = items
        .iter()
        .filter(|item| item.starts_with("skill:"))
        .cloned()
        .collect();
    let expected: Vec<String> = commands()
        .iter()
        .map(|command| command.name().to_string())
        .filter(|name| name.starts_with("skill:"))
        .collect();
    assert_eq!(filtered, expected);
}

#[tokio::test]
async fn keeps_fuzzy_skill_prefix_shorthand_working() {
    let items = suggestions_for("skbra").await;
    assert!(items.iter().any(|item| item == "skill:brainstorm"));
}
