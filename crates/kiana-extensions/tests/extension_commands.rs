//! 命令收集与 `invocationName` 去重测试。
//!
//! 移植 `packages/coding-agent/test/extensions-runner.test.ts` 的
//! `describe("command collection")` 3 例，并补充 `name:n` 撞名回退（§2.3.9 / §8.6 第 7 条）。

use std::sync::Arc;

use indexmap::IndexMap;
use kiana_core::source_info::{SourceInfo, SourceOrigin, SourceScope};
use kiana_ext_api::types::CommandHandler;
use kiana_ext_api::{Extension, RegisteredCommand};
use kiana_extensions::runner::commands::{
    collect_registered_commands, get_command, get_command_diagnostics, resolve_commands,
    resolve_registered_commands,
};
use serde_json::{Value, json};

const COMMAND_CASES_JSON: &str = include_str!("fixtures/runner_registry/command_cases.json");

fn base_extension(path: &str) -> Extension {
    Extension {
        path: path.to_string(),
        resolved_path: path.to_string(),
        hidden: None,
        replaceable: None,
        source_info: SourceInfo {
            path: path.to_string(),
            source: "local".to_string(),
            scope: SourceScope::User,
            origin: SourceOrigin::TopLevel,
            base_dir: None,
        },
        handlers: IndexMap::new(),
        tools: IndexMap::new(),
        message_renderers: IndexMap::new(),
        tool_renderers: None,
        markdown_transformer: None,
        entry_renderers: None,
        commands: IndexMap::new(),
        flags: IndexMap::new(),
        shortcuts: IndexMap::new(),
    }
}

fn command(name: &str, description: &str) -> RegisteredCommand {
    let handler: CommandHandler = Arc::new(|_args, _ctx| Box::pin(async {}));
    RegisteredCommand {
        name: name.to_string(),
        source_info: SourceInfo {
            path: format!("<inline:{name}>"),
            source: "local".to_string(),
            scope: SourceScope::User,
            origin: SourceOrigin::TopLevel,
            base_dir: None,
        },
        description: Some(description.to_string()),
        get_argument_completions: None,
        handler,
    }
}

/// 一个扩展注册 `(name, description)` 列表（同名在同一个 `commands` Map 里是覆盖，
/// 与 TS `Map.set` 一致）。
fn extension(path: &str, commands: &[(&str, &str)]) -> Extension {
    let mut ext = base_extension(path);
    for (name, description) in commands {
        ext.commands
            .insert((*name).to_string(), command(name, description));
    }
    ext
}

// TS: "collects commands from multiple extensions"
#[test]
fn collects_commands_from_multiple_extensions() {
    let extensions = [
        extension("/extensions/cmd-a.ts", &[("cmd-a", "Test command")]),
        extension("/extensions/cmd-b.ts", &[("cmd-b", "Test command")]),
    ];

    let commands = resolve_registered_commands(&extensions);
    assert_eq!(commands.len(), 2);
    let mut names: Vec<&str> = commands
        .iter()
        .map(|command| command.name.as_str())
        .collect();
    names.sort_unstable();
    assert_eq!(names, ["cmd-a", "cmd-b"]);
    let mut invocation_names: Vec<&str> = commands
        .iter()
        .map(|command| command.invocation_name.as_str())
        .collect();
    invocation_names.sort_unstable();
    assert_eq!(invocation_names, ["cmd-a", "cmd-b"]);
    // 命令始终按扩展加载顺序 × 扩展内插入顺序排列。
    assert_eq!(
        commands
            .iter()
            .map(|command| command.name.as_str())
            .collect::<Vec<_>>(),
        ["cmd-a", "cmd-b"]
    );
    assert!(get_command_diagnostics().is_empty());
}

// TS: "gets command by invocation name"
#[test]
fn gets_command_by_invocation_name() {
    let extensions = [extension("/extensions/cmd.ts", &[("my-cmd", "My command")])];

    let cmd = get_command(&extensions, "my-cmd").expect("命令存在");
    assert_eq!(cmd.name, "my-cmd");
    assert_eq!(cmd.invocation_name, "my-cmd");
    assert_eq!(cmd.description.as_deref(), Some("My command"));

    assert!(get_command(&extensions, "not-exists").is_none());
}

// TS: "suffixes duplicate extension commands in insertion order"
#[test]
fn suffixes_duplicate_extension_commands_in_insertion_order() {
    let extensions = [
        extension("/extensions/cmd-a.ts", &[("shared-cmd", "First command")]),
        extension("/extensions/cmd-b.ts", &[("shared-cmd", "Second command")]),
    ];

    let commands = resolve_registered_commands(&extensions);
    let diagnostics = get_command_diagnostics();

    assert_eq!(commands.len(), 2);
    assert_eq!(
        commands
            .iter()
            .map(|command| command.name.as_str())
            .collect::<Vec<_>>(),
        ["shared-cmd", "shared-cmd"]
    );
    assert_eq!(
        commands
            .iter()
            .map(|command| command.invocation_name.as_str())
            .collect::<Vec<_>>(),
        ["shared-cmd:1", "shared-cmd:2"]
    );
    assert_eq!(
        commands
            .iter()
            .map(|command| command.description.as_deref())
            .collect::<Vec<_>>(),
        [Some("First command"), Some("Second command")]
    );
    assert!(diagnostics.is_empty());
    assert_eq!(
        get_command(&extensions, "shared-cmd:1")
            .expect("第一条")
            .description
            .as_deref(),
        Some("First command")
    );
    assert_eq!(
        get_command(&extensions, "shared-cmd:2")
            .expect("第二条")
            .description
            .as_deref(),
        Some("Second command")
    );
    // `getCommand` 只匹配 invocationName：同名命令不再有裸名字。
    assert!(get_command(&extensions, "shared-cmd").is_none());
}

/// 撞上“字面量命令名”时向后找空位：`x:1` 被占用后重复的 `x` 用 `x:2`，
/// 那条字面量命令本身得到 `x:1:2`。
#[test]
fn suffixes_until_the_invocation_name_is_free() {
    let extensions = [
        extension(
            "/extensions/one.ts",
            &[("x", "first x"), ("x:1", "literal x:1")],
        ),
        extension("/extensions/two.ts", &[("x", "second x")]),
    ];

    let resolved = resolve_registered_commands(&extensions);
    assert_eq!(
        resolved
            .iter()
            .map(|command| command.invocation_name.as_str())
            .collect::<Vec<_>>(),
        ["x:1", "x:1:2", "x:2"]
    );
    assert_eq!(
        resolved
            .iter()
            .map(|command| command.description.as_deref())
            .collect::<Vec<_>>(),
        [Some("first x"), Some("literal x:1"), Some("second x")]
    );

    // 三条命令都能按各自的 invocationName 找回。
    assert_eq!(
        get_command(&extensions, "x:1")
            .expect("第一条 x")
            .description
            .as_deref(),
        Some("first x")
    );
    assert_eq!(
        get_command(&extensions, "x:1:2")
            .expect("字面量 x:1")
            .description
            .as_deref(),
        Some("literal x:1")
    );
    assert_eq!(
        get_command(&extensions, "x:2")
            .expect("第二条 x")
            .description
            .as_deref(),
        Some("second x")
    );
    assert!(get_command(&extensions, "x").is_none());
}

#[test]
fn three_duplicates_get_consecutive_suffixes() {
    let extensions = [
        extension("/extensions/a.ts", &[("dup", "one")]),
        extension("/extensions/b.ts", &[("dup", "two")]),
        extension("/extensions/c.ts", &[("dup", "three")]),
    ];
    assert_eq!(
        resolve_registered_commands(&extensions)
            .iter()
            .map(|command| command.invocation_name.as_str())
            .collect::<Vec<_>>(),
        ["dup:1", "dup:2", "dup:3"]
    );
}

#[test]
fn unique_names_keep_their_plain_invocation_name() {
    let extensions = [
        extension("/extensions/a.ts", &[("alpha", "a"), ("beta", "b")]),
        extension("/extensions/b.ts", &[("gamma", "c")]),
    ];
    assert_eq!(
        resolve_registered_commands(&extensions)
            .iter()
            .map(|command| command.invocation_name.as_str())
            .collect::<Vec<_>>(),
        ["alpha", "beta", "gamma"]
    );
}

#[test]
fn resolve_commands_works_on_an_already_collected_list() {
    let collected = collect_registered_commands(&[
        extension("/extensions/a.ts", &[("same", "one")]),
        extension("/extensions/b.ts", &[("same", "two")]),
    ]);
    assert_eq!(collected.len(), 2);
    let resolved = resolve_commands(&collected);
    assert_eq!(
        resolved
            .iter()
            .map(|command| command.invocation_name.as_str())
            .collect::<Vec<_>>(),
        ["same:1", "same:2"]
    );
}

#[test]
fn empty_extension_list_resolves_to_no_commands() {
    assert!(resolve_registered_commands(&[]).is_empty());
    assert!(get_command(&[], "anything").is_none());
    assert!(collect_registered_commands(&[]).is_empty());
}

// ---------------------------------------------------------------------------
// TS 黄金夹具（由 ExtensionRunner.getRegisteredCommands()/getCommand() 生成）
// ---------------------------------------------------------------------------

fn command_cases() -> Value {
    serde_json::from_str(COMMAND_CASES_JSON).expect("fixture parses")
}

fn assert_golden(case: &str, extensions: &[Extension]) {
    let cases = command_cases();
    let actual = Value::Array(
        resolve_registered_commands(extensions)
            .iter()
            .map(|command| json!([command.name, command.invocation_name, command.description]))
            .collect(),
    );
    assert_eq!(actual, cases[case], "command resolution for {case}");
}

#[test]
fn matches_the_ts_golden_command_resolution() {
    assert_golden(
        "collects-from-multiple-extensions",
        &[
            extension("/extensions/cmd-a.ts", &[("cmd-a", "Test command")]),
            extension("/extensions/cmd-b.ts", &[("cmd-b", "Test command")]),
        ],
    );
    assert_golden(
        "single-command",
        &[extension("/extensions/cmd.ts", &[("my-cmd", "My command")])],
    );
    assert_golden(
        "duplicate-in-insertion-order",
        &[
            extension("/extensions/cmd-a.ts", &[("shared-cmd", "First command")]),
            extension("/extensions/cmd-b.ts", &[("shared-cmd", "Second command")]),
        ],
    );
    assert_golden(
        "literal-name-collision",
        &[
            extension(
                "/extensions/one.ts",
                &[("x", "first x"), ("x:1", "literal x:1")],
            ),
            extension("/extensions/two.ts", &[("x", "second x")]),
        ],
    );
    assert_golden(
        "three-duplicates",
        &[
            extension("/extensions/a.ts", &[("dup", "one")]),
            extension("/extensions/b.ts", &[("dup", "two")]),
            extension("/extensions/c.ts", &[("dup", "three")]),
        ],
    );
    assert_golden(
        "unique-names",
        &[
            extension("/extensions/a.ts", &[("alpha", "a"), ("beta", "b")]),
            extension("/extensions/b.ts", &[("gamma", "c")]),
        ],
    );
}

#[test]
fn get_command_golden_lookup() {
    let extensions = [
        extension(
            "/extensions/one.ts",
            &[("x", "first x"), ("x:1", "literal x:1")],
        ),
        extension("/extensions/two.ts", &[("x", "second x")]),
    ];
    let cases = command_cases();
    for entry in cases["get-command-lookup"]
        .as_array()
        .expect("lookup array")
    {
        let name = entry[0].as_str().expect("name");
        let expected = entry[1].as_str().map(str::to_string);
        let actual = get_command(&extensions, name).and_then(|command| command.description);
        assert_eq!(actual, expected, "getCommand({name})");
    }
}
