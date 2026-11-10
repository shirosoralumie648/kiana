//! `core/slash-commands.ts` 的公开 API 测试（TS 无对应测试文件，按 `ARCHITECTURE.md` §4.8 第 9 条新写）。
//!
//! 24 项表来自 `tests/fixtures/builtin_slash_commands.json`：该文件由 TS 参考实现
//! （`/home/ctyun/kiana-work/upstream-reference`）的 `BUILTIN_SLASH_COMMANDS` 直接 `JSON.stringify` 生成，
//! 保证名称、描述、参数提示在 fixture 与实现之间一致（改动内置表需同步更新 fixture）。
//!
//! TS: `packages/coding-agent/src/core/slash-commands.ts`。

use kiana_core::slash_commands::{
    BUILTIN_SLASH_COMMANDS, BuiltinSlashCommand, SlashCommandInfo, SlashCommandSource,
    builtin_slash_commands,
};
use kiana_core::source_info::{SourceInfo, SourceOrigin, SourceScope};
use serde_json::json;

const FIXTURE: &str = include_str!("fixtures/builtin_slash_commands.json");

#[test]
fn builtin_table_matches_the_ts_golden_fixture() {
    let expected: Vec<BuiltinSlashCommand> =
        serde_json::from_str(FIXTURE).expect("fixture is a BuiltinSlashCommand array");
    assert_eq!(expected.len(), 24);
    assert_eq!(&*BUILTIN_SLASH_COMMANDS, &expected);
    assert_eq!(builtin_slash_commands(), expected.as_slice());
}

#[test]
fn serialization_round_trips_the_fixture() {
    let fixture: serde_json::Value = serde_json::from_str(FIXTURE).expect("fixture");
    assert_eq!(
        serde_json::to_value(&*BUILTIN_SLASH_COMMANDS).expect("serialize"),
        fixture
    );
}

#[test]
fn quit_description_uses_app_name_at_runtime() {
    let quit = builtin_slash_commands()
        .iter()
        .find(|command| command.name == "quit")
        .expect("quit is built in");
    assert_eq!(
        quit.description,
        format!("Quit {}", kiana_core::config::APP_NAME)
    );
    assert_eq!(quit.description, "Quit kiana");
}

#[test]
fn docs_only_commands_are_not_in_the_table() {
    // docs/slash-commands.md lists /llama and /mcp, but they are registered by extensions.
    assert!(
        builtin_slash_commands()
            .iter()
            .all(|command| command.name != "llama" && command.name != "mcp")
    );
}

#[test]
fn slash_command_info_serializes_source_info_camel_case() {
    let info = SlashCommandInfo {
        name: "review".into(),
        description: Some("Review the changes".into()),
        source: SlashCommandSource::Extension,
        source_info: SourceInfo {
            path: "/x/ext.ts".into(),
            source: "local".into(),
            scope: SourceScope::User,
            origin: SourceOrigin::TopLevel,
            base_dir: None,
        },
    };
    assert_eq!(
        serde_json::to_string(&info).expect("serialize"),
        concat!(
            r#"{"name":"review","description":"Review the changes","source":"extension","#,
            r#""sourceInfo":{"path":"/x/ext.ts","source":"local","scope":"user","origin":"top-level"}}"#
        )
    );

    let without_description = SlashCommandInfo {
        description: None,
        ..info
    };
    let value = serde_json::to_value(&without_description).expect("serialize");
    assert_eq!(
        value,
        json!({
            "name": "review",
            "source": "extension",
            "sourceInfo": {
                "path": "/x/ext.ts",
                "source": "local",
                "scope": "user",
                "origin": "top-level"
            }
        })
    );
}

#[test]
fn slash_command_source_uses_the_ts_literals() {
    for (source, literal) in [
        (SlashCommandSource::Extension, "extension"),
        (SlashCommandSource::Prompt, "prompt"),
        (SlashCommandSource::Skill, "skill"),
    ] {
        assert_eq!(
            serde_json::to_value(source).expect("serialize"),
            json!(literal)
        );
    }
}
