//! 面板命令路由（`/permissions`、`/mcp`、`/tasks`）。
//!
//! 设计文档 §9.1 把面板命令放在冻结的内置命令表之外：`dispatch_submit` 在 24 个内置
//! 命令（最后一个是 `/quit`）之后、`!`/`!!` bash 分支之前调用
//! [`InteractiveMode::try_dispatch_panel_command`]，命中即清空编辑器并返回。
//!
//! # 匹配规则
//!
//! - `/permissions`、`/tasks`：精确形式打开面板；带任何参数只显示 `Usage: /<name>`，
//!   不挂面板、也不落到模型。
//! - `/mcp`：只匹配**精确**命令并打开新面板。`/mcp login`、`/mcp logout`、
//!   `/mcp reconnect` 等仍由内建扩展处理（`session.prompt` 内拦截），保持既有行为；
//!   `/mcp` 不在 `kiana-session::panel_commands` 的 headless 注册表里，print 模式也不会
//!   被本路由抢走。
//!
//! 面板名与 24 个内置命令、扩展命令都无交集；autocomplete/冲突诊断由
//! `interactive_mode.rs` 的 `create_base_autocomplete_provider` 与
//! `get_built_in_command_conflict_diagnostics` 消费同一份注册表。
//!
//! # 为什么放在这里而不是 `commands/mod.rs` 内联
//!
//! 三个面板的挂载逻辑各自独立（快照组装、回调写回、异步拉取），保留在各自文件里；
//! 本文件只做一行分派，避免 `dispatch_submit` 继续膨胀。

use super::super::InteractiveMode;

impl InteractiveMode {
    /// 尝试把一条输入作为面板命令处理。
    ///
    /// 返回 `true` 表示已经处理（成功打开面板或显示用法），调用方必须清空编辑器并
    /// `return`；`false` 表示不是面板命令，继续走内置/bash/消息分支。
    ///
    /// `text` 已由 `dispatch_submit` 做 JS `trim`；这里仍按防御性再 trim 一次。
    pub(crate) fn try_dispatch_panel_command(&mut self, text: &str) -> bool {
        let text = text.trim();
        if text == "/permissions" {
            self.show_permissions_selector();
            return true;
        }
        if text.starts_with("/permissions ") {
            self.show_warning("Usage: /permissions");
            return true;
        }
        if text == "/tasks" {
            self.show_tasks_selector();
            return true;
        }
        if text.starts_with("/tasks ") {
            self.show_warning("Usage: /tasks");
            return true;
        }
        if text == "/mcp" {
            self.show_mcp_selector();
            return true;
        }
        false
    }
}
