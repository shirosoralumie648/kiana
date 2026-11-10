//! `core/slash-commands.ts`：`BUILTIN_SLASH_COMMANDS`（24 项，顺序即 UI 顺序）与
//! `SlashCommandInfo` / `SlashCommandSource` / `BuiltinSlashCommand`。
//!
//! `SlashCommandInfo.sourceInfo` 是 [`crate::source_info::SourceInfo`]，`quit` 的描述含
//! `APP_NAME`（[`crate::config`]）。`kiana-ext-api`（L7）的 `extensions/types.ts` 与
//! `kiana-resources` 都要用，故定义在此而非 `kiana-resources`（`ARCHITECTURE.md` §3.5、§3.7）。
//!
//! TS: `packages/coding-agent/src/core/slash-commands.ts`。
//!
//! ## 逐字性
//!
//! 表里 24 项的 `name` / `description` / `argumentHint` 是当前稳定契约（顺序即 UI 顺序），改动需同步 fixture 与测试。
//! `quit` 的 description 是 `Quit ${APP_NAME}`，在**首次访问**时运行期拼接（`APP_NAME` 可由
//! `package.json.kianaConfig.name` 覆盖，虽然当前取值是 `"kiana"`）。
//!
//! `docs/slash-commands.md` 里的 `/llama` 与 `/mcp` **不在**本表中：它们由扩展注册。不要添加。
//!
//! ## 设计原因与使用方式
//!
//! - 表用 `LazyLock<Vec<_>>` 而不是 `const`：`quit` 的 description 由 `format!` 在运行期
//!   分配 `String`，const/static 初始化器无法执行它（[`crate::config::APP_NAME`] 本身是
//!   编译期常量，问题在于拼接结果需要堆分配）；初始化只发生一次，
//!   `builtin_slash_commands()` 把同一份只读切片交给所有调用方。
//! - 本表**只有**内置命令。扩展注册的命令（`/llama`、`/mcp` 等）不在这里：
//!   `kiana-interactive` 的基座自动补全以本表为起点，并用本表的 `name` 集合在
//!   `get_built_in_command_conflict_diagnostics` 中检测扩展命令与内置命令重名。
//! - [`SlashCommandInfo`] 是扩展 API `getCommands()` 的返回元素；`kiana-rpc-print` 把它
//!   再导出为 `RpcSlashCommand`，camelCase 字段名是跨进程契约。`description` 为 `None`
//!   时整个键被省略（`skip_serializing_if`）。
//! - 改 24 项表中任何 `name` / `description` / `argument_hint` 都会让
//!   `crates/kiana-core/tests/slash_commands.rs` 与 fixture 的逐字对比失败。

use std::sync::LazyLock;

use serde::{Deserialize, Serialize};

use crate::source_info::SourceInfo;

/// 斜杠命令的来源。TS: `slash-commands.ts#SlashCommandSource`
/// （`"extension" | "prompt" | "skill"`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SlashCommandSource {
    /// `"extension"`：扩展注册的命令。
    Extension,
    /// `"prompt"`：prompt 模板。
    Prompt,
    /// `"skill"`：技能命令。
    Skill,
}

/// 一条可执行的斜杠命令（扩展/prompt/技能）。
/// TS: `slash-commands.ts#SlashCommandInfo`。
///
/// 字段顺序是跨进程序列化契约：`name`、`description`、`source`、`sourceInfo`。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SlashCommandInfo {
    /// 命令名（不含 `/`）。
    pub name: String,
    /// 描述（扩展可以不给）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// 来源种类。
    pub source: SlashCommandSource,
    /// 定义该命令的资源的来源信息。
    pub source_info: SourceInfo,
}

/// 内置斜杠命令表的一项。TS: `slash-commands.ts#BuiltinSlashCommand`。
///
/// 字段顺序是序列化契约：`name`、`description`、`argumentHint`。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BuiltinSlashCommand {
    /// 命令名（不含 `/`）。
    pub name: String,
    /// 描述（逐字，供自动补全与帮助使用）。
    pub description: String,
    /// 参数提示（如 `<provider/model>`）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub argument_hint: Option<String>,
}

fn command(name: &str, description: &str) -> BuiltinSlashCommand {
    BuiltinSlashCommand {
        name: name.to_string(),
        description: description.to_string(),
        argument_hint: None,
    }
}

fn command_with_hint(name: &str, description: &str, hint: &str) -> BuiltinSlashCommand {
    BuiltinSlashCommand {
        name: name.to_string(),
        description: description.to_string(),
        argument_hint: Some(hint.to_string()),
    }
}

/// 内置斜杠命令（24 项，顺序即 UI 顺序）。TS: `slash-commands.ts#BUILTIN_SLASH_COMMANDS`。
///
/// `LazyLock`：`quit` 的描述由 `format!` 在运行期分配 `String`（[`crate::config::APP_NAME`]
/// 是编译期常量，但拼接结果无法放进 const/static 初始化器）。
///
/// 首次解引用时构造，`LazyLock` 保证多线程下只初始化一次；之后内容不可变（对外只暴露
/// 共享引用，没有获取 `&mut Vec` 的入口），因此调用方可以安全地长期持有其切片。
pub static BUILTIN_SLASH_COMMANDS: LazyLock<Vec<BuiltinSlashCommand>> = LazyLock::new(|| {
    // 顺序就是 UI 顺序；`tests/slash_commands.rs` 按名字逐项断言，调整顺序属于用户可见变更。
    vec![
        command("settings", "Open settings menu"),
        command_with_hint(
            "model",
            "Select model (opens selector UI)",
            "<provider/model>",
        ),
        command("tree", "Navigate session tree (switch branches)"),
        command_with_hint("thinking", "Set thinking level", "<level>"),
        command("scoped-models", "Enable/disable models for Ctrl+P cycling"),
        command(
            "export",
            "Export session (HTML default, or specify path: .html/.jsonl)",
        ),
        command("import", "Import and resume a session from a JSONL file"),
        command(
            "share",
            "Share session via a Radius artifact or a GitHub gist",
        ),
        command_with_hint(
            "bug",
            "Report a bug to the Kiana developers",
            "<description>",
        ),
        command("copy", "Copy last agent message to clipboard"),
        command("name", "Set session display name"),
        command("session", "Show session info and stats"),
        command("changelog", "Show changelog entries"),
        command("hotkeys", "Show all keyboard shortcuts"),
        command("fork", "Create a new fork from a previous user message"),
        command(
            "clone",
            "Duplicate the current session at the current position",
        ),
        command("trust", "Save project trust decision for future sessions"),
        command_with_hint("login", "Configure provider authentication", "<provider>"),
        command("logout", "Remove provider authentication"),
        command("new", "Start a new session"),
        command("compact", "Manually compact the session context"),
        command("resume", "Resume a different session"),
        command(
            "reload",
            "Reload keybindings, extensions, skills, prompts, themes, and context files",
        ),
        // 唯一在运行期拼描述的一项：`Quit ${APP_NAME}`（其余 23 项是固定文案）。
        command("quit", &format!("Quit {}", crate::config::APP_NAME)),
    ]
});

/// 内置斜杠命令表的只读视图（`LazyLock` 首次访问时构建）。
/// TS: `slash-commands.ts#BUILTIN_SLASH_COMMANDS`。
///
/// 返回 `'static` 切片，与 [`BUILTIN_SLASH_COMMANDS`] 指向同一存储；调用方无需克隆。
pub fn builtin_slash_commands() -> &'static [BuiltinSlashCommand] {
    BUILTIN_SLASH_COMMANDS.as_slice()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_has_the_24_ts_entries_in_order() {
        let names: Vec<&str> = builtin_slash_commands()
            .iter()
            .map(|c| c.name.as_str())
            .collect();
        assert_eq!(
            names,
            [
                "settings",
                "model",
                "tree",
                "thinking",
                "scoped-models",
                "export",
                "import",
                "share",
                "bug",
                "copy",
                "name",
                "session",
                "changelog",
                "hotkeys",
                "fork",
                "clone",
                "trust",
                "login",
                "logout",
                "new",
                "compact",
                "resume",
                "reload",
                "quit",
            ]
        );
    }

    #[test]
    fn argument_hints_match_the_ts_table() {
        let hints: Vec<(&str, Option<&str>)> = builtin_slash_commands()
            .iter()
            .filter(|c| c.argument_hint.is_some())
            .map(|c| (c.name.as_str(), c.argument_hint.as_deref()))
            .collect();
        assert_eq!(
            hints,
            [
                ("model", Some("<provider/model>")),
                ("thinking", Some("<level>")),
                ("bug", Some("<description>")),
                ("login", Some("<provider>")),
            ]
        );
    }

    #[test]
    fn quit_uses_app_name_and_docs_commands_are_absent() {
        let quit = builtin_slash_commands()
            .iter()
            .find(|c| c.name == "quit")
            .expect("quit");
        assert_eq!(
            quit.description,
            format!("Quit {}", crate::config::APP_NAME)
        );
        assert!(
            builtin_slash_commands()
                .iter()
                .all(|c| c.name != "llama" && c.name != "mcp")
        );
    }
}
