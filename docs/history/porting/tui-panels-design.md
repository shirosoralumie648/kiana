# TUI 面板设计（P0 / P1 / P2）

日期：2026-11-08
状态：设计稿，未实现
范围：`kiana-interactive` 的会话内面板、`kiana-session` / `kiana-permissions` / `kiana-config` 的配套读写 API、`kiana-builtin-extensions` 的 `/mcp` 增量、`kiana-rpc-print` 的 headless 文本回退。
参考：`reference_project/{qwen-code,gemini-cli,cline,codex}` 的只读源码，以及 Kiana 当前工作树（行号为写作时快照）。

> **这不是 TS 契约。** Kiana 的 TS 参照实现只有 24 个内置命令
> （`kiana-core::slash_commands::BUILTIN_SLASH_COMMANDS`，被
> `crates/kiana-core/tests/slash_commands.rs` 与 golden fixture 逐字锁定）。本文新增的
> `/permissions`、`/tasks`、`/language` 等是**产品增量**，不得写进那张冻结表；`/mcp` 已由内建扩展
> 注册（`crates/kiana-builtin-extensions/src/mcp/index.rs`），保持扩展所有权，不新增同名核心命令。
> 相关约束见 `CLAUDE.md` 硬性约束 1/2/3、`spec/ca-interactive-mode.md` §3.1、`spec/ca-resources.md`
> §2.5（`docs/slash-commands.md` 里的 `/llama`、`/mcp` 不在冻结表中）。

---

## 0. 结论摘要

1. **面板入口**：在 `dispatch_submit` 的内置命令链之后、`!`/`!!` bash 分支之前，增加一条独立的
   `try_dispatch_panel_command` 分支；命令元数据放在新的 `kiana-session::panel_commands` 注册表，
   不碰 `BUILTIN_SLASH_COMMANDS`。自动补全与冲突诊断消费同一份注册表。
2. **面板生命周期**：核心面板统一走 `InteractiveMode::show_selector`
   （`crates/kiana-interactive/src/interactive_mode/commands/mod.rs:224`），占编辑器槽位、选择器令牌、
   `done()` 延后恢复编辑器；与 `/settings` 相同。扩展对话框（`push_extension_dialog`）与选择器互斥，
   后到者会 dispose 先到者。
3. **`/permissions`**：新增 `permission_snapshot()` 与三个层面的持久化写入（User / Project / Local）
   和运行期同步；UI 用四标签页（Allow/Ask/Deny/Workspace）+ 内部视图状态机，照搬 qwen
   `PermissionsDialog` 的交互骨架。
4. **approval mode indicator**：不新增独立行，在 `FooterComponent` 第二行右侧追加可选段
   `… • <mode>`；`None` 时输出与今天逐字节一致，保护 footer golden。颜色映射沿用 qwen
   `approvalModeVisuals.ts`（read-only=Success、default=Dim、accept-edits=Warning、bypass=Error）。
5. **`/mcp`**：面板已存在且功能基本齐备（`mcp/index.rs` + `mcp/ui.rs`），P0 只做增量：footer 健康
   徽标（`N MCP(s) offline`）、把 `/mcp` 从新面板注册表中排除、补测试。**不复用**
   `kiana mcp list --json` 作为面板数据源（它会真连每台服务器、失败退出 1、与活会话状态重复）。
6. **`/tasks`**：后端缺“按会话列任务”的 API（journal 只有 `list_recoverable`，manager 的 `tasks`
   是私有字段）。P0 增加 `BackgroundTaskJournal::list_for_session`、`BackgroundTaskManager::list`、
   `AgentSession::list_background_tasks`，面板显示运行中/终态任务并支持取消；print 模式增加文本回退。
   当前生产只有 shell 后台任务（`background/tool.rs`），agent/monitor 任务由后续 producer 接入，面板
   按 `tool` 字段通用展示。

---

## 1. 参考项目模式总结

### 1.1 命令 → dialog（命令层只产出“意图”，dialog 自己持有状态）

| 参考文件 | 做法 | Kiana 落点 |
|---|---|---|
| `qwen-code/packages/cli/src/ui/commands/permissionsCommand.ts` | 22 行，返回 `{ type: 'dialog', dialog: 'permissions' }`，不碰 UI | `commands/permissions.rs` 只负责取快照 + `show_selector` |
| `qwen-code/packages/cli/src/ui/commands/approvalModeCommand.ts` | 无参数开 `approval-mode` dialog；带参数只改本会话模式并回 info 文本；非法参数报错列出 `APPROVAL_MODES` | 模式切换并入 `/permissions` 的 `ModeSelect` 视图；不新增 `/approval-mode` |
| `qwen-code/packages/cli/src/ui/commands/mcpCommand.ts` | `desc/nodesc/schema` 都开 `mcp` dialog；`auth/noauth` 只警告并提示用 dialog | Kiana 的 `/mcp` 由扩展实现，行为已对齐（见 §5） |
| `qwen-code/packages/cli/src/ui/commands/tasksCommand.ts` | **纯文本**命令，`supportedModes` 含 interactive/non_interactive/acp；三注册表合并、按 `startTime` 排序、状态标签、只对 agent/shell 给输出路径、`stripUnsafeCharacters` | `/tasks` 的 TUI 对话框 + 同一份文本格式器（§6） |
| `qwen-code/packages/cli/src/ui/commands/memoryCommand.ts`、`agentsCommand.ts`、`settingsCommand.ts` | memory/settings 直接开 dialog；agents 的 `manage`/`create` 开 `subagent_list`/`subagent_create` | P1 `/memory`、P2 `/agents` 的入口形态 |
| `qwen-code/packages/cli/src/ui/types.ts` + `slashCommandProcessor.ts` + `opentui-dialog-mount.tsx` | dialog 名是 union，processor 按 `result.dialog` 分发，mount 处 `case 'permissions'` 等挂组件 | Kiana 没有 dialog union：每个命令模块自己 `show_selector`，少一层中央 switch |

**设计含义**：命令模块只做“取数据 / 挂组件 / 处理完成回调”，视图状态（tab、input、confirm）全部封装在
组件内部。这与 Kiana `/settings` 的分层一致（`commands/settings.rs` 只组装
`SettingsConfig`/`SettingsCallbacks`，`components/settings_selector.rs` 管显示）。

### 1.2 footer / status-line 指示器

| 参考文件 | 做法 | Kiana 落点 |
|---|---|---|
| `qwen-code/packages/cli/src/ui/components/AutoAcceptIndicator.tsx` + `approvalModeVisuals.ts` | 文本：`plan mode` / `auto-accept edits` / `Auto mode` / `YOLO mode` / `⏸ Ask permissions`；颜色：plan=success、AUTO_EDIT=warning、AUTO=link、YOLO=error、DEFAULT=secondary；附 `(shift + tab to cycle)` | `FooterComponent` 第二行右侧可选段；文本直接用 Kiana 的稳定模式名（`read-only/default/accept-edits/bypass`），不引入 qwen 的五个名字 |
| `gemini-cli/packages/cli/src/ui/components/ApprovalModeIndicator.tsx`，挂在 `StatusRow.tsx`（row 2 左；`Footer.tsx` 组合状态行） | 同色系语义；auto-accept 附 `to plan` 的循环提示 | 同上；Kiana 当前没有任何“循环模式”键位，P0 不做 `shift+tab` |
| `qwen-code/packages/cli/src/ui/components/mcp/MCPHealthPill.tsx` | 只在 `disconnectedCount > 0` 时显示 `N MCP(s) offline`，`connecting` 不算；warning 色；仅展示，不接键盘 | Kiana 用扩展 `ui.setStatus("mcp", …)` 落到 footer 第三行（扩展状态行），文本前加 `⚠` 弥补无法按条着色 |
| `gemini-cli/packages/cli/src/ui/components/BackgroundTaskDisplay.tsx` | 运行中 shell 任务的 tab 条、进程列表、实时输出、日志路径、kill/list 键 | P1 备选：Kiana `/tasks` 先做静态列表 + `r` 刷新；实时跟随不在 P0 |
| `gemini-cli/packages/cli/src/ui/components/MemoryUsageDisplay.tsx` / `HookStatusDisplay.tsx` | 每 2s 刷新的资源指标 / 执行中 hook 名与进度 | 不照搬：Kiana footer 是冻结契约，第三条状态行已可承载扩展文本 |

### 1.3 schema 驱动的 settings

| 参考文件 | 做法 | 对 Kiana 的结论 |
|---|---|---|
| `qwen-code/packages/cli/src/ui/components/SettingsDialog.tsx` | `getDialogSettingKeys` + `getSettingDefinition`；条目 `{label,value,type,description,toggle}`；bool/enum 循环；User/Workspace 作用域；pending vs saved；需重启提示 | Kiana 没有设置元数据表；`/settings` 的条目顺序被 `components_settings_selector.rs` 的 TS golden 锁定。**P0/P1 不重构 `/settings`** |
| `codex-rs/tui/src/config_panel/mod.rs`、`input.rs`、`toggle.rs` | 手写 section catalog（Appearance/Notifications/Input/Sessions/TextSelection）+ `ConfigSettingValue::{Toggle,Editor,DependentToggle,NotificationTest,Unavailable}`；输入委托 `ListSelectionView`；toggle 映射到持久化编辑 | 若 P2 真要做，照 codex 的“手写 catalog”而不是生成式 schema，风险最低 |
| `gemini-cli/packages/cli/src/ui/components/FooterConfigDialog.tsx` | footer 项重排/显隐、预览、持久化、Esc 即保存 | `/statusline` 的参考；Kiana 先做只读清单（§7.9） |

### 1.4 headless 文本回退

| 参考文件 | 做法 | Kiana 落点 |
|---|---|---|
| `qwen-code/packages/cli/src/ui/commands/tasksCommand.ts` | 交互/非交互/ACP 全模式纯文本；三注册表合并；状态标签按任务类型；输出路径只给 agent/shell；对不可信文本做 `stripUnsafeCharacters` | `kiana-session::panel_commands::execute_headless`；print 模式在 `session.prompt` 之前拦截 |
| `qwen-code/packages/cli/src/ui/commands/languageCommand.ts` | 无 dialog，纯文本状态 + `ui`/`output` 子命令；接受 locale code / id / full name；非交互与 ACP 可用 | P1 `/language`：TUI 选择器 + headless 文本 |
| `qwen-code/packages/cli/src/ui/commands/mcpCommand.ts` + Kiana `mcp/index.rs::format_status` | TUI 开管理器；非 TUI 打印每服务器一行状态 | Kiana `/mcp` 已经如此（扩展命令在 `session.prompt` 内被拦截），不需要新核心命令 |
| `cline/apps/cli/src/tui/components/dialogs/tool-approval.tsx`、`codex-rs/tui/src/bottom_pane/approval_overlay.rs` | y/Enter 批准、n/Esc 拒绝；codex 用当前 + FIFO 队列、Esc 取消当前 | Kiana 审批已由 `render_core::modal::ModalArbiter` + `ApprovalCard` 实现（`interactive_mode/extension_ui/mod.rs:468`）；本设计不改审批卡 |

### 1.5 其它可复用骨架

- **qwen `PermissionsDialog.tsx`**：`TabId = allow|ask|deny|workspace`；`DialogView =
  rule-list|add-rule-input|add-rule-scope|delete-confirm|ws-dir-list|ws-add-dir-input|ws-remove-confirm`；
  列表首项 `Add rule…`；scope 项（Project `.qwen/settings.json` / User `~/.qwen/settings.json`）；
  删除确认展示 raw rule、描述、scope，执行时先 User 后 Workspace；workspace 首项 `Add directory…`，
  初始目录不可删，校验 `Directory does not exist.` / `Path is not a directory.` /
  `This directory is already in the workspace.` / 子目录拒收。
- **qwen `MCPManagementDialog.tsx`**：导航栈 `server-list/detail/tool-list/...`；`fetchServerData`
  合并 config + tool/prompt/resource registry + approval state + status；实时状态订阅；每步独立 footer；
  只有 server-list 的 Esc 关闭。
- **qwen `subagents/manage/AgentsManagerDialog.tsx`**：导航栈 + 列表/详情/编辑/删除；每步 header/footer。
- **qwen `StatsDialog.tsx`**：session/activity/efficiency 三 tab、`r` 循环 range、loading/error/retry。
- **gemini `HooksDialog.tsx`**：按事件分组、enabled/disabled、source/matcher/sequential/timeout、安全警告。
- **cline `command-palette-items.ts`**：动作目录带 shortcut/keywords/scoring —— 未来命令面板的输入，
  不是本设计的 P0。
- **cline `checkpoint-picker.tsx`**：窗口化列表 + 相对时间；`/rewind` 的 P2 参考。

---

## 2. Kiana 现状与硬约束

### 2.1 TUI 骨架（已核对）

| 机制 | 位置 | 语义 |
|---|---|---|
| 命令分派 | `crates/kiana-interactive/src/interactive_mode/commands/mod.rs:874` `dispatch_submit` | 冻结的 if 链：24 个内置命令 → `!`/`!!` → 压缩中/流式中/普通消息 |
| 内置表 | `crates/kiana-core/src/slash_commands.rs` | 24 项，顺序即 UI 顺序；`/mcp`、`/llama` 由扩展注册，禁止加入 |
| 选择器挂载 | `commands/mod.rs:224` `show_selector` | 编辑器槽位只放一个组件；`SelectorFocus::{Component,Child,Address}`；`done()` 用 `spawn_local` 延后恢复编辑器 |
| `/settings` 样板 | `commands/settings.rs:332` `show_settings_selector`，`:966` `SelectorCreation` | 快照 + 回调；焦点 `SelectorFocus::Address(component_address(selector.get_settings_list()))` |
| 选择器清理 | `interactive_mode/extension_ui/mod.rs:1142` `dispose_active_selector` | 扩展对话框压栈前会先清理选择器 |
| 扩展对话框栈 | 同文件 `:1206` `push_extension_dialog`、`:1252` `show_top_extension_dialog`、`:1224` `remove_extension_dialog` | 也占用编辑器槽位；栈顶渲染 |
| footer | `crates/kiana-interactive/src/components/footer.rs:359`（结构）、`:493`（render）、`:693`（可选第三行扩展状态） | 固定 2 行，扩展状态非空时 3 行；`handles_input=false` |
| footer 挂载 | `interactive_mode.rs:1634` 构造，`:2669` `mount_interactive_tui` 七容器顺序 | document → pending → status → widget above → editor → widget below → footer |
| 模态仲裁（实验） | `render_core/modal.rs` | `ModalKind::{Approval,AskUser,Settings}`；与选择器不是同一套机制 |
| 自动补全 | `interactive_mode.rs:2355` `create_base_autocomplete_provider` | 从 `BUILTIN_SLASH_COMMANDS` 起，追加模板/扩展/技能命令 |
| 冲突诊断 | `interactive_mode.rs:2312` `get_built_in_command_conflict_diagnostics` | 只把扩展命令与 24 个内置名比较 |

### 2.2 后端能力矩阵

| 面板 | 现有后端 | 缺口 |
|---|---|---|
| `/permissions` | `PermissionEngine::policy()`（含 `mode`/`workspace_roots`/`rules`，`policy.rs:941`）、`set_mode`、`set_rules`、`add_workspace_root`；`AgentSession::set_permission_mode/rules/add_permission_directory`；`SettingsManager::update_project_permissions`；`permissions::append_permission_rule`/`append_rule_to_permissions` | 无规则列表快照/来源标记；无删除规则/目录；无 global 作用域写入；引擎无 `set_workspace_roots`；无 `session_allow` 计数 |
| approval indicator | `AgentSession::permission_mode()`；`FooterComponent` 已有右侧模型栏 | footer 无 mode 字段；无初始化/rebind 刷新 |
| `/mcp` | `McpManagerView` + `McpUi`（`mcp/ui.rs`）；`State::servers`；`server_menu`/`servers_menu`；`format_status`；CLI `mcp list --json` | 无 footer 健康徽标；面板已具备列/详情/工具/启用/禁用/登录/登出/重连 |
| `/tasks` | `BackgroundTaskManager`（start/cancel/finish/task/usage）；`BackgroundTaskJournal::{save,load,list_recoverable,finish}`；`MemoryTaskJournal::all`；`DurableTaskJournal` | 无按会话列全部记录的 API；`BackgroundTaskManager.tasks` 私有；`AgentSession` 无 list；任务无 kind 字段（仅 `tool`）；生产只有 shell producer |
| `/memory` | `MemoryService`（snapshot/list/forget/audit）；`SessionMemory` 为 `pub(crate)`；`AgentSession` 只暴露 `memory_enabled`/`memory_prompt_index` | 条目/审计/删除未暴露 |
| `/agents` | `AgentDefinitionLoader::with_default_roots(cwd).load()`；`spawn_named_subagent` 内部每次重建 | 无 session 级 list API（实现很简单） |
| `/hooks` | `HookRunner::registry()`（`RegisteredHook` 私有，只 `len/is_empty`）；`HookAuditRecord`；`AgentSession::hook_audit_records()` | 无公开 registry 列表；无 hook 配置诊断 accessor |
| `/language` | `SettingsManager::get_ui_locale/set_ui_locale`；`render_core::i18n::SUPPORTED_LANGUAGES`；`apply_ui_locale` | 基本齐全 |
| `/tools` | `get_all_tools` / `get_active_tool_names` / `set_active_tools_by_name` / `get_tool_definition` | 无持久化；面板可先会话内切换 |
| `/stats` | `get_session_stats`（`SessionStats`） | 只有 session 总量，无 activity/efficiency 分解 |
| `/context` | `get_context_usage`（tokens/window/percent） | 无分类明细/阈值信息 |
| `/rewind` | `checkpoint_manager()`、`latest_checkpoint`、`restore_checkpoint` | 无“对话回退”；无 checkpoint 列表；只能恢复最新文件快照 |
| `/statusline` | footer 固定结构 + 扩展状态 | 无 footer 项配置 |
| schema settings | `Settings::raw`/typed getters；`components_settings_selector.rs` 硬编码顺序 | 无元数据表；重构会破坏 golden |

### 2.3 三条不可违反的实现纪律

1. **JSON 写盘走 `kiana_json`**：新增的规则/目录写回必须复用
   `kiana_json::js_object::stringify_pretty(root, 2)`，并保留文件里其它键与键序
   （参考 `crates/kiana-config/src/settings/permissions.rs` 的读改写实现）。
2. **单线程 / `!Send`**：所有面板异步动作走 `tokio::task::spawn_local`；`show_selector` 的回调捕获
   `Weak<RefCell<InteractiveModeInner>>`，不要跨线程持有 `Rc`/`RefCell`。
3. **借用纪律**：回调在组件自己的 `handle_input` 借用栈上同步执行，不能假设“当前没有借用”。
   需要写编辑器/渲染器/会话的同步回调走 `run_after_dispatch` 或 `run_mode_callback`
   （`commands/mod.rs:81`、`commands/settings.rs` 的 `mode!` 宏）。

---

## 3. P0-A：`/permissions`

### 3.1 命令形态

| 输入 | 行为 |
|---|---|
| `/permissions` | TUI：挂载权限面板；print（`AppMode::Print` / `PrintOutputMode::Text`）：打印文本快照；RPC/JSON：P0 不拦截（见 §10 风险） |
| `/permissions <未知参数>` | TUI 状态行显示 `Usage: /permissions`；不挂面板 |

不新增 `/approval-mode`；模式切换在面板内（`m` 键或模式行）。`/permissions` 不属于
`BUILTIN_SLASH_COMMANDS`。

### 3.2 数据模型与来源

新增 `AgentSession::permission_snapshot() -> PermissionSnapshot`（实现放
`crates/kiana-session/src/permissions_api.rs`，`impl AgentSession`；只需在 `agent_session.rs` 增加一个
`pub(crate) fn permission_engine(&self) -> Option<Arc<PermissionEngine>>` accessor）：

```rust
pub struct PermissionSnapshot {
    pub mode: Option<PermissionMode>,          // None = 权限层关闭（off）
    pub bypass_disabled: bool,
    pub rules: Vec<PermissionRuleEntry>,
    pub workspace_roots: Vec<PathBuf>,         // 第一项是会话 cwd，不可删
    pub session_allow_count: usize,
    pub errors: Vec<String>,                   // 文件读取/解析错误（面板头部展示，不阻断其余数据）
}

pub struct PermissionRuleEntry {
    pub rule: PermissionRule,
    pub raw: serde_json::Value,                // to_settings_value() 的形态（写回/删除用）
    pub decision: RuleDecision,
    pub scopes: Vec<PermissionRuleScope>,      // 同一规则可能出现在多层
    pub removable: bool,                       // 仅 User/Project/Local 可删
}

pub enum PermissionRuleScope {
    User,        // <agent-dir>/settings.json
    Project,     // <cwd>/.kiana/settings.json（项目受信时）
    Local,       // <cwd>/.kiana/settings.local.json（项目受信时）
    ManagedOrCli // 文件里找不到来源：managed policy 或 --allow/--deny；不可在此删除
}
```

**快照算法**（写进函数文档，确定性可测）：

1. `engine = core.permissions`（克隆 `Arc`）；`mode = engine.mode()`、`bypass_disabled = engine.bypass_permissions_disabled()`。
2. 从 `engine.policy()` 取 `rules` 与 `workspace_roots`。
3. 读可编辑文件里的原始 `permissions`：
   - User：`settings_manager.get_global_settings().raw("permissions")`；
   - Project：`settings_manager.get_project_settings().raw("permissions")`（仅 `is_project_trusted()`）；
   - Local：`permissions::read_permissions_file(local_settings_path(cwd))`（仅受信）。
   对每层用与 `PermissionEngine::settings_engine` 相同的解析（`rules` 数组 + `allow/deny/ask` 简写）
   得到 `Vec<(scope, raw, PermissionRule)>`。简写字符串的 `raw` 是该字符串；结构化条目走新增公开的
   `kiana_permissions::parse_permission_rule(&Value) -> Option<PermissionRule>`（`rule_from_settings_value`
   的公开薄包装），保证与启动时的解析逐字一致。
4. 对每条有效规则（按 `to_settings_value()` 去重）：
   - 在所有文件层里找 `parsed == rule` 的项，收集 `scopes`；
   - 找不到来源时标 `ManagedOrCli`；
   - `removable = scopes 非空且全部 ∈ {User,Project,Local}`。
5. 工作区根：`engine.policy().workspace_roots` 第一项是 cwd；其余按“文件层里 `addDirs`/
   `workspaceRoots`/`additionalDirectories` 的原始字符串（按 `resolve_permission_path` 归一化后比较）”
   归因。找不到来源的根标 `ManagedOrCli`，不可删除。
6. `session_allow_count = engine.session_allow_count()`（新增，返回 `session_allow` 的 len）。

**为什么不在 UI 里直接读 settings 文件**：文件可能损坏、项目可能未受信、分层合并有去重与 `null`
语义；把解析集中在 session 层，UI 只消费快照，错误一次显示。

### 3.3 视图状态机

组件 `PermissionsDialog`（`components/permissions_dialog.rs`）持有：

```rust
enum RuleTab { Allow, Ask, Deny, Workspace }

enum PermissionsView {
    RuleList { tab: RuleTab },
    ModeSelect { selected: usize, return_tab: RuleTab },
    AddRuleInput { text: String, error: Option<String> },
    AddRuleScope { rule: PermissionRule, selected: usize, error: Option<String> },
    DeleteRuleConfirm { entry: PermissionRuleEntry, selected: usize, error: Option<String> },
    WorkspaceList,
    AddDirectoryInput { text: String, error: Option<String> },
    RemoveDirectoryConfirm { root: PathBuf, selected: usize, error: Option<String> },
}
```

组件字段：`snapshot: PermissionSnapshot`、`view`、`rule_query: String`、`ws_query: String`、
`selected_rule: usize`、`selected_dir: usize`、`notice: Option<PanelNotice>`。

渲染（`PanelCommon` 提供边框/标签/提示）：

```text
──────────────────────────────────────────────────────────
 Permissions
 Mode: default   (m to change)
 [Allow]  Ask  Deny  Workspace
   + Add rule…
 → tool:edit                     project
   command:cargo test            local (also in user)
 filter: git
 Enter select · Tab switch · Esc close
──────────────────────────────────────────────────────────
```

- Rule 行的 `label` 由 `PermissionRule::display_text()` 生成（新增，稳定文本）：
  `tool:<pattern>`、`command:<tokens joined by ' '>`、`path:<prefix>`、`exact:<command>`、
  `wildcard:<pattern>`、`approved:<inner>`。
- 描述列：scope 列表（`user, local`）+ `reason`（若有）。
- 第一条永远是 `+ Add rule…`（Workspace 标签下是 `+ Add directory…`）。
- 空标签页：`No <allow|ask|deny> rules.`；Workspace 至少显示 cwd。

### 3.4 键盘与焦点

面板组件实现 `Component` + `Focusable`，`handles_input() = true`，`as_focusable() -> Some(self)`；
挂载用 `SelectorFocus::Component`（`show_selector` 取编辑器槽最后一个子项地址）。

| 键 | RuleList | 输入视图 | 确认视图 | WorkspaceList |
|---|---|---|---|---|
| `Tab` / `Shift+Tab` / `←` / `→` | 切换 tab（Allow→Ask→Deny→Workspace→Allow） | 不拦截（交给 `Input`） | 不拦截 | 与 RuleList 相同 |
| `↑`/`↓`/`PageUp`/`PageDown` | 转发给 `SelectList` | 不拦截 | `↑↓` 选 Yes/No | 转发给 `SelectList` |
| `/` | 进入过滤模式（`filter_active = true`，query 为空） | 作为文本输入 | 忽略 | 同 RuleList |
| 可打印字符 | 仅 `filter_active` 时追加到 query 并 `set_filter`；否则忽略 | 交给 `Input` | 忽略 | 同 RuleList |
| `Backspace` | `filter_active` 时删 query 最后一个字符；否则忽略 | 交给 `Input` | 忽略 | 同 RuleList |
| `Enter` | 激活选中行（过滤中则只提交过滤、退出过滤模式） | 提交 | 确认/执行 | 激活选中行 |
| `Esc` | `filter_active`→清空并退出过滤；否则关闭面板 | 取消回上一视图 | 取消回列表 | `filter_active`→退出过滤；否则关闭 |
| `m` | 仅在非过滤模式打开 `ModeSelect` | 作为文本输入 | 忽略 | 仅在非过滤模式有效 |
| `r` | 仅在非过滤模式重新取快照并保持视图 | 忽略 | 忽略 | 仅在非过滤模式有效 |
| `d` | 仅在非过滤模式对选中规则打开删除确认（`+` 行除外） | 忽略 | 忽略 | 仅在非过滤模式对选中目录打开删除确认（cwd 除外） |

过滤模式是必要的：`m`/`r`/`d` 是命令键，若把可打印字符无条件当过滤输入，规则名里的这些字母会
误触发命令。P0 用 `/` 显式进入过滤，Esc 退出；输入视图不经过这层拦截，`Input` 自己处理所有字符。

鼠标：面板把事件 `y` 减去标题/tab/查询行高度后转发给列表；P0 只保证键盘，鼠标测试可选。

`SelectList` 用于列表（`crates/kiana-tui/src/components/select_list.rs`：`get_selected_item`/`on_select`/
`on_cancel`）；输入用 `kiana_tui::components::input::Input`（`InputOptions`/`get_value`）。

过滤实现细节：**不用** `SelectList::set_filter`（它按 `value` 的前缀匹配，且 value 若用稳定 id 就
无法按规则文本搜索）。面板自己保存全量 `rows`，query 变化时对 `display_text` 做大小写不敏感子串
过滤，然后用过滤结果重建一个 `SelectList`（`SelectItem.value` = 稳定 row id，`label` = display_text）；
选中项按 id 还原到 row。任务面板同理（按 description/task_id/tool 匹配）。

### 3.5 添加规则流程

1. RuleList 的 `+ Add rule…`（或 `a`）→ `AddRuleInput { text, error: None }`；提示
   `Enter permission rule (tool:<name>, command:<prefix>, path:<dir>, or a bare tool name)`。
2. Enter：
   - `parse_cli_rule(trimmed, decision)`（`crates/kiana-permissions/src/policy.rs:1383`）；
   - `None` → `error = "Malformed rule. Use tool:<name>, command:<prefix>, path:<dir>, or a bare tool name."`，
     保留输入；
   - `Some(rule)` → `AddRuleScope`。
3. `AddRuleScope` 选项（顺序照 qwen `PermissionsDialog` 的 scope 列表：Project 第一项）：
   - `Project settings` — `.kiana/settings.json`（未受信时禁用，描述 `Project is not trusted; cannot write.`）；
   - `Local settings` — `.kiana/settings.local.json`（未受信时禁用）；
   - `User settings` — `<agent-dir>/settings.json`。
   初始选中第一项（Project；未受信时回退 User）。
4. Enter：
   - **先做可写性检查**：`session.permissions_disabled()` 且当前无引擎 → 提示
     `Permission checks are disabled. Change the mode first.`，不写盘（fail-closed）。
   - **再写文件**（顺序不可反：写失败不能改运行期）：
     - Project：`settings_manager.update_project_permissions(|current| append_rule_to_permissions(...))`
       （`crates/kiana-config/src/settings/mod.rs:1333`；内部用 `append_rule_to_permissions` 去重）；
     - Local：`permissions::append_permission_rule(local_settings_path(cwd), &rule.to_settings_value())`；
     - User：新增 `SettingsManager::update_global_permissions(|current| ...)`（`settings/mod.rs`，
       实现同 project，但写 global 快照；复用 `append_rule_to_permissions`）。
     - 写后 `settings_manager.flush().await`，再 `drain_errors()`；有错误 → `notice.error`，**不**更新引擎。
   - **最后更新运行期**：按 `to_settings_value()` 去重后
     `session.set_permission_rules(现有有效规则 + 新规则)`（**不用** `add_permission_rules`，避免同一
     规则在多层重复时被追加两次；`set_permission_rules` 保留 CLI/managed 的有效规则）。
   - 成功 → 回 `RuleList`，`notice = "Added <display_text>"`，重新取快照。
5. `Esc` 任意一步回上一视图；`AddRuleScope` 的 Esc 回 `AddRuleInput` 并保留文本。

解析边界：`parse_cli_rule` 只接受非空；`command:` 文本按空白切 token；以 `*` 结尾生成
`CommandWildcard`；`tool:` 支持首尾 `*`；裸字符串若看起来像路径则 `PathPrefix`，含空白按 command，
否则按 tool。UI 不自行解释规则，错误文案只提示支持的四种前缀。

### 3.6 删除规则流程

1. 选中规则 → `d` 或 Enter → `DeleteRuleConfirm`：
   ```text
   Delete permission rule?
     tool:edit
     scope: project, local
     reason: <可选>
     [ ] No   [x] Yes
   ```
2. `entry.removable == false`：正文追加
   `This rule is enforced by managed policy or the CLI and cannot be removed here.`；
   Enter 只把该错误放进 `notice`。
3. 确认：
   1. 对 `scopes` 中每个文件作用域移除**所有**匹配 `raw` 的项：
      - 新增 `permissions::remove_rule_from_permissions(&mut Value, raw) -> Result<bool, _>`：
        在 `permissions.rules` 里按 `Value` 全等删除；若 `raw` 是字符串，则在 `allow/deny/ask`
        对应数组里删除等值字符串。写回用现有读改写路径（`O_NOFOLLOW` + `kiana_json`）。
      - Project 走 `settings_manager.update_project_permissions`；User 走
        `update_global_permissions`；Local 走 `permissions::update_permissions_file`。
   2. 全部写成功后才更新运行期：`session.set_permission_rules(snapshot.rules - 被删规则)`。
      `ManagedOrCli` 规则永远不进入这一步。
4. 任一层写失败：`notice.error = "Could not remove the rule: <error>"`，**不**更新运行期，
   保持确认视图让用户重试。
5. 成功 → 回列表 + 重新取快照（同一条规则可能在其它层还有定义，快照会显示剩余层）。

### 3.7 Workspace 目录

1. `WorkspaceList` 行：cwd（描述 `working directory (always included)`，不可删）+ 每个额外根
   （描述 scope，若可归因）+ 首项 `+ Add directory…`。
2. `AddDirectoryInput`：
   - Enter 校验顺序（与 qwen 一致）：空→忽略；`!path.exists()` → `Directory does not exist.`；
     `!path.is_dir()` → `Path is not a directory.`；已存在（归一化后相等）→
     `This directory is already in the workspace.`；是现有根的子孙 → `This directory is inside an
     already-included directory.`。
   - 归一化：相对路径按会话 cwd 解析；`~`/`$HOME` 用新增公开的
     `kiana_permissions::resolve_permission_path(base, path)`（把现有 `request.rs:496` 的
     `pub(crate) resolve_rule_path` 提升为公开薄包装）。
   - scope 选择与添加规则相同（Project/Local/User）。写 `addDirs` 数组
     （新增 `permissions::append_workspace_root`，去重）。
   - 运行期：`session.add_permission_directory(path)`（已存在，会同步引擎 + 沙箱 + 工具定义）。
3. `RemoveDirectoryConfirm`：cwd 不可删；`ManagedOrCli` 根不可删；若同一路径同时出现在
   不可归因来源（managed/CLI）与可编辑文件，拒绝删除（删文件会让你误以为已撤销，实际引擎里仍有）。
   确认后从所有归属文件层删除（新增 `remove_workspace_root`），再调用新增
   `session.set_permission_workspace_roots(new_roots)`（更新 `additional_writable_roots` + 引擎
   `policy.workspace_roots` + `refresh_execution_policy_and_tools()`）。
4. 文件添加/删除都必须 `kiana_json` 序列化并保留其它键序；项目未受信时 Project/Local 不可选。

### 3.8 模式切换（`ModeSelect`）

1. `m` 在 `RuleList` 打开；列表四项（`read-only`/`default`/`accept-edits`/`bypass`），
   每项描述用 `PermissionMode::description()`（`mode.rs:56`）。
2. `bypass` 且 `bypass_disabled == true`：该行描述 `Disabled by policy.`，Enter 显示
   `Bypass mode is disabled by policy.`，不写盘、不切模式。
3. 选中后进入 scope 选择（`Session only` / `Project` / `Local` / `User`；默认 `Session only`，
   与 qwen `/approval-mode` “只改会话”的命令语义一致，避免误持久化）。
4. 应用：`session.set_permission_mode(mode)`（会清空 session allow，并按模式刷新执行策略）；
   然后刷新 footer 指示器（`footer.set_permission_mode(Some(mode))`）。
5. 持久化作用域：写 `permissions.mode`（字符串 `mode.as_str()`）：
   - Project：`update_project_permissions`；Local：`update_permissions_file`；
   - User：`update_global_permissions`；
   - Session only：不写。
   写失败只报错，运行期模式已经切换（两者分离，错误文案说明“本次会话已切换，但未保存”）。
6. `disableBypassPermissionsMode` 是 managed policy：读 `engine.bypass_permissions_disabled()`，
   不做自己的解析。

### 3.9 错误处理总表

| 场景 | 表现 | 状态 |
|---|---|---|
| settings 里的 `permissions` 不是对象 / 规则条目解析失败 | `snapshot.errors` 显示为头部 warning；其余可读层照常展示 | 只读可用，写操作仍需用户确认 |
| 规则解析失败 | 输入视图错误行 | 保留输入，不关闭 |
| settings 写失败（损坏 / 未受信 / symlink 被拒） | `notice.error`，显示 `SettingsValueError` 文本 | 不更新运行期，视图保持 |
| `drain_errors()` 返回多条 | 取最后一条与本面板字段相关的；全部保留在 `notice` | 同上 |
| 项目未受信 | Project/Local scope 行禁用 + 描述 | 只能选 User |
| 权限层 disabled | 头部 `Mode: off`；添加规则前要求先切模式 | 不静默写盘 |
| 删除 managed/CLI 规则 | 确认视图警告；Enter 只提示 | 不调用 `set_permission_rules` |
| 删除后仍有低层同名规则 | 快照重新计算后仍显示该规则（scope 变为低层） | 正常 |
| 目录不存在/非目录/重复/子目录 | 输入视图错误行 | 保留输入 |
| 引擎/会话已释放（`Weak` 升级失败） | 静默返回，不写任何状态 | 选择器随宿主销毁 |

### 3.10 headless 文本

print 模式（仅 `AppMode::Print` / `PrintOutputMode::Text`；以及未来 RPC）输出：

```text
Permissions
  mode: default
  workspace roots: /repo, /repo/extra
  allow:
    tool:read
  ask:
    command:cargo publish
  deny:
    path:/etc
  session approvals: 2
```

- 规则一行一条，按 `Allow → Ask → Deny`、再按文件顺序；`ManagedOrCli` 规则在行尾标 `(managed/cli)`。
- 无规则时对应小节输出 `(none)`。
- 权限层关闭时 `mode: off`。
- 文本是本设计新增的稳定面，需新测试锁定；不写 golden fixture 目录（避免与 TS fixture 混淆）。

---

## 4. P0-B：approval mode indicator

### 4.1 放置

- **不改 footer 行数**。`FooterComponent` 第二行右侧当前是
  `<model> [• thinking <level>] [→ routed] [(provider) 前缀]`（`footer.rs:620-668`）。
  在 `right_side_without_provider` 的末尾追加 `" • " + mode_text`，随后继续走既有的
  provider 前缀回退与宽度截断逻辑。
- `mode = None`（引擎未启用）时不追加任何字符，输出与今天逐字节一致；这保护
  `crates/kiana-interactive/tests/fixtures/render_goldens_dark.json` 的 `footer_*` golden。
- 不在编辑器边框上叠加：`StatusIndicator` 的渲染契约是 line 0 空 / line 1 内容
  （`components/status_indicator.rs`），且工作/重试指示器频繁重绘，叠加持久模式会干扰既有状态测试。

### 4.2 文本与颜色

| `PermissionMode` | 文本 | 颜色（`ThemeColor`） |
|---|---|---|
| `ReadOnly` | `read-only` | `Success` |
| `Default` | `default` | `Dim` |
| `AcceptEdits` | `accept-edits` | `Warning` |
| `BypassPermissions` | `bypass` | `Error` + `bold` |

- 文本直接用 `PermissionMode::as_str()`（`mode.rs:46`），与 CLI `--permission-mode` 帮助一致，
  不引入 qwen 的 `plan mode` / `YOLO mode` 文案，也不加 `(shift + tab to cycle)`（Kiana 没有该键位）。
- 颜色段放在最右端并最后应用；footer 的 `dim_remainder` 外层 dim 会被内层 reset 截断，但模式段之后
  没有内容，不影响布局。实现时复用 `kiana_theme::THEME.fg/bold`。

### 4.3 API 与刷新

`FooterComponent`（`components/footer.rs`）：

```rust
pub struct FooterComponent {
    auto_compact_enabled: bool,
    session: Box<dyn FooterSession>,
    footer_data: Arc<dyn ReadonlyFooterDataProvider>,
    permission_mode: Option<PermissionMode>, // 新增，默认 None
}

impl FooterComponent {
    pub fn set_permission_mode(&mut self, mode: Option<PermissionMode>);
    pub fn permission_mode(&self) -> Option<PermissionMode>; // 测试接缝
}
```

- `FooterComponent::new(session, footer_data)` 在装箱前读一次 `session.permission_mode()`；
  `from_parts`（测试替身）保持 `None`，既有测试不受影响。
- `set_session(session)` 时同步重新读取。
- 刷新点：
  1. `InteractiveMode::new` 构造 footer 后（`interactive_mode.rs:1634`）——`new` 已读，无需额外调用；
  2. `interactive_mode/session.rs` 的会话 rebind 路径——调用 `footer.set_session(...)` 后自然刷新；
  3. `/permissions` 的 `ModeSelect` 应用后——显式 `set_permission_mode(Some(mode))` + `request_render()`；
  4. CLI `--permission-mode` 在会话创建前调用 `session.set_permission_mode`（`coding-agent/src/app.rs:2337`），
     因此 `new` 读到最终值。
- 类型依赖：`kiana-interactive` 目前不依赖 `kiana-permissions`（`ARCHITECTURE.md` §2.2 的边）。
  为避免新增依赖边，在 `crates/kiana-session/src/lib.rs` 增加再导出模块：
  ```rust
  pub mod permissions {
      pub use kiana_permissions::{
          ApprovalDecision, ApprovalScope, PermissionDecision, PermissionEngine, PermissionMode,
          PermissionPolicy, PermissionRule, RuleDecision, parse_cli_rule,
      };
  }
  ```
  UI 用 `kiana_session::permissions::PermissionMode`。若评审更倾向直接加依赖边，必须同步
  `ARCHITECTURE.md` 附录 A（本设计推荐前者，保持依赖图不变）。

### 4.4 测试

新增 `crates/kiana-interactive/tests/components_footer_permission_mode.rs`：

- `none_mode_keeps_existing_goldens`：`from_parts` + 不调用 setter，`render(120)` 与
  `golden("footer_basic")` 相等（`components_footer.rs` 的 `golden` 是私有函数，复制一份到新测试，
  不改旧文件）。
- `each_mode_appends_text_and_color`：`from_parts` + `set_permission_mode(Some(mode))`，四种模式，
  `strip_ansi` 后包含 `" • read-only"` 等，原始文本包含对应主题色转义。
- `narrow_width_truncates_mode_last`：宽度不足时模式段被截断而不是挤掉模型名。
- `rebind_refreshes_mode`：在 `tests/common` 的 `FauxFixture` 上创建真实会话，构造
  `FooterComponent::new` 后 `set_session` 到另一个设置了 `accept-edits` 的会话，断言输出变化；
  若会话构造成本过高，退化为 `set_permission_mode` 单测 + 在 `interactive_mode_lifecycle.rs`
  追加一个 rebind 用例。

---

## 5. P0-C：`/mcp`

### 5.1 现有实现清单（已核对）

`crates/kiana-builtin-extensions/src/mcp/index.rs`：

- 命令注册：`register_command("mcp", ...)`（搜索 `install_mcp_extension` / `/mcp` handler，约 `:3910`）。
- 无参数：TUI → `show_manager`（`:3288`）→ `show_mcp_manager`（`mcp/ui.rs:963`）→ `ctx.ui().custom(factory)`
  → `McpManagerView`；非 TUI → `format_status`（`:3319`）文本。
- 子命令：`login`/`logout`/`reconnect [server]`；多余参数打印 `MCP_USAGE`；未知动作打印用法。
- 菜单：`servers_menu`（`:2504`）→ `server_menu`（`:2554`）→ `show_tools`（`:2675`）。
  - 服务器列表排序：`attention_rank` 升序，其次 `locale_compare`；描述 `state · exposure · source`。
  - 服务器详情：`Enable`/`Enable in this project`/`Sign in`/`Tools`/`Reconnect`/`Sign out`/`Exposure`/
    `Disable`/`Disable in this project`；`details` 含 transport、scope/source、project override、State；
    `error` 拼接 `server.message` 与未连接时的连接错误。
  - 工具页：服务器默认 exposure + `[exposure]` 前缀标出覆盖项。
- 配置与持久化：`mcp/config.rs` 的 `load_mcp_config`/`update_mcp_server_config`；
  `McpServerScope::{Global,Project,Extension}`；项目未受信时不加载 `.kiana/mcp.json`。
- 实时状态：每台服务器有 `McpServerConnection`；`on_change` → `apply_connection_change` →
  `ConnectionEvent::Change` → 本地桥任务 `emit_change`（`:885`，在 `LocalSet` 上）。
- 状态字符串：`ServerState::{Connecting,Connected,Disconnected,NeedsAuth,Failed,Closed}`；
  `describe_state` 已含 tool/prompt 数（`:688`）。

### 5.2 数据源决策：进程内 `State::servers`，不用 `kiana mcp list --json`

| 维度 | `State::servers`（推荐） | `kiana mcp list --json`（不采用） |
|---|---|---|
| 连接 | 复用活会话的连接、OAuth 凭据、工具注册 | 每台服务器新建一个连接，结束后 `close()` |
| 副作用 | 无 | 会真的 spawn stdio 子进程 / 发 HTTP；OAuth 服务器可能触发刷新 |
| 状态 | 实时（`on_change`） | 一次性快照 |
| 退出语义 | 无 | 任一启用的服务器未连接 → exit 1；面板不能把“配置错误”当崩溃 |
| 数据 | state/tools/prompts/resources(`has_resources`)/exposure/disabled/config errors/override | `ServerReport`：name/scope/source/override/enabled/exposure/transport/state/tools/toolExposure/resources/error |
| 结论 | **面板与 `/mcp` 命令都用它** | 仅供 CLI/自动化；**不**在 TUI 面板里调用 |

补充：`mcp list --json` 的 `tools` 是工具名数组，没有 prompt 列表；面板已有的 `describe_state`
还包含 prompt 数，信息更全。

### 5.3 P0 规格

| 需求 | 现状 | P0 动作 |
|---|---|---|
| 列出 global/project `mcp.json` 的服务器 | 已有（`servers_menu`；`scope_or_source`；`override`） | 用测试锁定现有文案与排序 |
| 启用/禁用 | 已有 `enable`/`enable-project`/`disable`/`disable-project`，写回 `update_mcp_server_config` | 不变 |
| 工具数 | 已有（`describe_state` 与 `Tools` 项） | 不变 |
| 错误 | 已有（列表 `notices`/`error`、详情 `error`） | 不变 |
| 登录/登出 | 已有 `signin`/`signout`；OAuth 流程在 `mcp/ui.rs` 的 `redirect_url` | 不变 |
| 资源/提示数 | 只有 prompt 数（`describe_state`）与 `has_resources` 布尔 | P1：在详情 `details` 增加 `Resources: yes/no`；资源列表需要新 API，不在 P0 |
| footer 健康徽标 | **缺** | P0 实现（§5.4） |
| 与核心面板冲突 | `/mcp` 是扩展命令 | 新面板注册表明确排除 `mcp`；测试确保 `/mcp` 仍进入 `session.prompt` → 扩展 runner |

### 5.4 footer 健康徽标

- 位置：扩展状态行（`FooterComponent` 第三行，`footer.rs:693`），通过
  `ctx.ui().set_status("mcp", Some(text))` 写入；不新增 footer 字段。
- 计算：`offline = state.servers.iter().filter(|s| is_enabled_entry(&s.entry) &&
  s.connection.as_ref().is_some_and(|c| c.state() == ServerState::Disconnected)).count()`；
  **只统计 `Disconnected`**（与 qwen `MCPHealthPill` 一致；`Connecting`/`Failed`/`NeedsAuth` 都不计入；
  后两者已有自己的菜单动作与状态文案，重复计数会让徽标长期常驻）。
- 文本：`offline == 0` → `set_status("mcp", None)`；否则 `⚠ {offline} MCP{s} offline`
  （单数无 `s`）。
- 写入点：`emit_change(local)`（`mcp/index.rs:885`）内、listeners 之后。需要把
  `Arc<dyn ExtensionContextApi>`（`session_start` 已收到，`session_start(&local, &state, &options, ctx)`）
  存到 `LocalOnly` 的新字段 `ctx: RefCell<Option<Arc<dyn ExtensionContextApi>>>`，并在
  `session_shutdown` 清空。`set_status` 是 `UiRequest` 通道（Send），在 LocalSet 上调用安全。
- 颜色限制：扩展状态行统一 dim，无法按条着色；用 `⚠` 传达告警，文档中注明这是与 qwen 的差异。
- 状态 key 用 `mcp`（排序时与其它扩展状态按 locale 比较；`compare_status_keys` 不变）。

### 5.5 测试

- `crates/kiana-builtin-extensions/tests/mcp_health_status.rs`（新增）：
  - 0 离线：不写 status（或 `None`）；
  - 1/2 台 `Disconnected`：文本 `⚠ 1 MCP offline` / `⚠ 2 MCPs offline`；
  - `Connecting`/`NeedsAuth`/`Failed` 不计入（按 qwen 语义）；
  - 禁用服务器不计入；
  - `emit_change` 在 LocalSet 上调用 `set_status`，用记录型 fake UI context 断言。
- 既有 `mcp_manager_view.rs`、`mcp_extension.rs`、`mcp_command.rs` 不能改断言；新增测试独立文件。

---

## 6. P0-D：`/tasks`

### 6.1 后端清单与缺口

现状：

- `BackgroundTaskManager`（`crates/kiana-session/src/background/mod.rs:296`）：
  `start`/`cancel`/`finish_task`/`task(task_id)`/`usage_for_parent`/`usage_for_task`/`sweep_for_session`；
  `tasks: Mutex<HashMap<String, Arc<TaskSlot>>>` 只在 `Inner` 里，无枚举。
- `BackgroundTaskRecord`（`journal.rs:110`）：`task_id/parent_session_id/parent_turn_id/tool/description/
  status/terminal_seq/terminal/output_path/output_bytes/output_truncated/notified/attempts/created_at_ms/
  updated_at_ms/pid`。
- `BackgroundTaskJournal`（`journal.rs:319`）：`save/load/list_recoverable/finish/record_attempt/mark_notified/
  replay_pending_deliveries`。`MemoryTaskJournal::all()` 有全部记录；`DurableTaskJournal` 按会话文档存
  `records`，`all_sessions()` 私有，`list_recoverable` 会扫全部会话。
- 生产 producer：只有 `background/tool.rs` 包装的 `bash` 后台任务（`tool = "bash"`，
  `parent_turn_id = tool:<tool_call_id>`，`description` 取命令首行）。`AgentDefinition.background`
  目前只解析、未接入 manager；Kiana 没有 monitor 任务。

缺口与 P0 新增：

```rust
// journal.rs: 给 BackgroundTaskJournal 增加（两个 in-repo 实现都改）
async fn list_for_session(
    &self,
    parent_session_id: &str,
) -> Result<Vec<BackgroundTaskRecord>, BackgroundTaskError>;

// background/mod.rs: manager 委托 journal；不暴露内部 tasks 表
pub async fn list(
    &self,
    parent_session_id: &str,
) -> Result<Vec<BackgroundTaskRecord>, BackgroundTaskError>;

// agent_session.rs（或新文件 tasks_api.rs）
pub async fn list_background_tasks(&self) -> Result<Vec<crate::background::BackgroundTaskRecord>,
    crate::background::BackgroundTaskError>;
```

- `MemoryTaskJournal::list_for_session`：`all()` 后按 `parent_session_id` 过滤，按 `created_at_ms` 升序。
- `DurableTaskJournal::list_for_session`：新增私有 `session_slot(session_id)`（与 `all_sessions` 共用
  打开逻辑），只读目标会话文档的 `records`，`decode_record` 后过滤父会话；**不得**跨会话泄露记录。
- `AgentSession::list_background_tasks`：`background_task_manager()` 为 `None` → `Ok(Vec::new())`；
  否则 `manager.list(self.session_id().as_str())`。会话 id 走公共 `session_id()`。
- 记录排序：qwen 按 `startTime` 升序；Kiana 用 `created_at_ms` 升序。终态任务也显示（`list_for_session`
  包含已通知记录），与 `list_recoverable` 区分。
- 可选（不进 P0）：给 `BackgroundTaskRecord` 增加 `kind: Option<BackgroundTaskKind>`；目前用 `tool`
  映射：`bash|shell|execute_bash|powershell` → `shell`；`agent|task|subagent` → `agent`；其它 → `other`
  （直接显示 `tool`）。monitor：没有 producer，面板预留 `monitor` 标签但不造假数据。

### 6.2 面板规格

组件 `TasksDialog`（`components/tasks_dialog.rs`），数据在打开时通过
`spawn_local` 拉取（命令处理器异步）：

```rust
struct TaskRow {
    record: BackgroundTaskRecord,
    kind: TaskKind, // Agent | Shell | Other(String)
}
```

列表：

```text
──────────────────────────────────────────────────────────
 Background tasks (2)
 → ● running    shell   cargo test --all          task_8f…  00:12
   ✓ completed  shell   make build                task_8e…  01:03
   ✗ failed     agent   review the diff           task_8d…  00:41
 r refresh · Enter details · c cancel · Esc close
──────────────────────────────────────────────────────────
```

- 状态图标/颜色：running=Warning `●`；completed=Success `✓`；failed/interrupted=Error `✗`；
  cancelled/timed_out=Dim `○`。
- 行字段：`status · kind · description（单项截断） · task_id（前缀） · 相对时长`。
- 排序：`created_at_ms` 升序；运行中不置顶（照 qwen 的 startTime 序）。
- 空态：`No background tasks.`；manager 关闭：`Background tasks are not enabled for this session.`
- `r` 重新拉取（保持选中 task_id）；无实时订阅（P0 文档明确）。

详情视图（Enter）：

```text
Task task_8f...
  status: running
  tool: bash
  description: cargo test --all
  created: 2026-11-08 10:12:03 (12s ago)
  updated: ...
  output: /path/to/task_8f....output (12.3 KB, truncated: no)
  usage: in 1.2k · out 340 · cache read 0 · cache write 0
  terminal: <reason/exit code if terminal>
  pid: 12345
  [r] refresh   [c] cancel   [Esc] back
```

- 字段全部来自 `BackgroundTaskRecord`；`terminal` 为 `Some` 时追加
  `reason`/`exit_code`/`finished_at_ms`/`acceptance` 摘要。
- `c`：仅 `status == Running` 可取消；`session.cancel_background_task(task_id).await`；
  `Ok(true)` → `notice = "Cancelled <task_id>"` + 刷新；`Ok(false)` → `Task is already finished.`；
  `Err` → `notice.error = error.to_string()`。
- `Esc` 从详情回列表，再按一次关闭面板。
- 不读取输出文件内容（P1 可加“查看输出”，但需要分页/截断策略）；P0 只显示路径与大小。

键盘：`↑↓/PageUp/PageDown` 列表；`Enter` 详情；`r` 刷新；`c` 取消；`Esc` 返回/关闭；
`/` 进入过滤模式（规则同 `/permissions`：过滤模式下可打印字符进 query，`r`/`c` 不触发命令）。

### 6.3 headless 文本回退

`kiana-session::panel_commands::execute_headless(session, "/tasks")` 输出（qwen 风格，新增稳定面）：

```text
Background tasks (2):
  [running] shell  "cargo test --all"  task_8f...  output: /path/task_8f....output
  [completed] shell  "make build"  task_8e...  output: /path/task_8e....output
```

- 无任务：`No background tasks.`
- manager 关闭：`Background tasks are not enabled for this session.`
- 状态标签：`running/completed/failed/cancelled/timed_out/interrupted`；`kind` 映射同上；
  只对 `Agent`/`Shell` 显示 `output:`（与 qwen 一致；`Other` 不显示输出路径）。
- `description` 折叠为单行并去掉控制字符（复用 `sanitize_status_text` 同级的清洗逻辑，或新增
  `sanitize_task_text`）；`task_id` 保留完整 id（qwen 也保留完整 id）。
- print 模式拦截：只在 `mode == PrintOutputMode::Text`（`AppMode::Print`）下生效（`AppMode::Json`
  的输出有固定事件协议，写裸文本会破坏它；P0 不在 Json 模式拦截，面板命令会像未知斜杠命令一样进入模型）。
  在 `crates/kiana-rpc-print/src/print_mode.rs` 的 `try_body` 循环里，对每条 message 先调用
  `panel_commands::execute_headless(current_session(session_slot).agent_session(), message)`；
  `Some(Ok(text))` → `io.write_raw(text + "\n")` 并 `continue`；`Some(Err(msg))` → 写 stderr/退出码 1；
  `None` → 照旧 `prompt`。`initial_message` 也要同样处理（同一 Text 模式门槛）。
  **不得**拦截 24 个内置命令或 `/mcp`（注册表只含面板名）。

### 6.4 测试

- `crates/kiana-session/tests/background_list.rs`（新增）：
  - memory journal：两个父会话 + 终态/运行中记录，`list_for_session` 只返回本会话、含终态、按时间序；
  - durable journal：跨会话隔离；`list_recoverable` 行为不变（回归）；
  - `BackgroundTaskManager::list` 委托；
  - `AgentSession::list_background_tasks` 在 manager 关闭时返回空。
- `crates/kiana-interactive/tests/components_tasks_dialog.rs`（新增）：
  - 空态/有任务/详情/取消确认/刷新；
  - 状态标签与颜色（`strip_ansi`）；
  - 排序与过滤。
- `crates/kiana-interactive/tests/interactive_mode_tasks_command.rs`（新增）：
  - `/tasks` 挂载选择器、`/tasks foo` 显示用法、Esc 关闭；
  - 不改变其它命令分派（复用 `test_dispatch_submit`）。
- `crates/kiana-rpc-print/tests/print_mode_panel_commands.rs`（新增）：
  - `/tasks` 输出且不触发模型请求（用现有 faux 模型 fixture 断言无请求）；
  - `/mcp` 仍走扩展命令（构造一个注册 `/mcp` 的 fake extension runner 或复用现有 mcp fixture）。

---

## 7. P1 / P2 简要方案

优先级按“后端已就绪程度 + 用户价值”排序。每个面板都复用 §9 的注册/生命周期机制；
组件与命令文件独立，避免与 P0 冲突。

### 7.1 `/memory`（P1）

- 目标：列出 user/project 记忆条目、作用域、revision、描述；详情看正文；删除（forget，需理由）；
  查看最近审计；显示 auto-extract 开关与路径。
- 数据：`SessionMemory::{list_entries, forget, audit, auto_extract}` 目前 `pub(crate)`。新增
  `AgentSession::memory_snapshot(scope) -> Option<MemorySnapshot>`、`memory_entries(scope)`、
  `memory_audit(scope, limit)`、`memory_forget(scope, id, reason)`（实现 `memory_api.rs` + `core.memory`
  accessor）。
- UI：两个 tab（User/Project）；`Enter` 详情；`d` 删除确认（理由输入可选）；`r` 刷新。
- 限制：`auto_extract` 由 `MemoryWiringOptions`（env `KIANA_MEMORY_AUTO_EXTRACT`）在构造时决定，
  运行期不可切换；P1 只显示状态。qwen 的 auto-memory/auto-dream/auto-skill 三开关不对应 Kiana 后端。
- 文件：`components/memory_dialog.rs`、`commands/memory.rs`、后端 `memory_api.rs`；测试
  `components_memory_dialog.rs`、`agent_session_memory_panel.rs`。

### 7.2 `/agents`（P1）

- 目标：列出 `AgentDefinition`（name/description/source/model/permission_mode/tools/max_turns/background/
  file_path），详情查看；显示加载错误；`r` 重载。
- 数据：`AgentSession::agent_definitions() -> AgentDefinitionLoadReport`，实现即
  `AgentDefinitionLoader::with_default_roots(self.session_manager().get_cwd()).load()`（与
  `spawn_named_subagent` 当前行为一致）。
- 不做创建/编辑（qwen `subagent_create` 需要文件写入 UI 与模板校验，P2 另立设计）。
- 文件：`components/agents_dialog.rs`、`commands/agents.rs`、后端 `agents_api.rs`（或直接放
  `agent_session.rs`）；测试 `components_agents_dialog.rs`。

### 7.3 `/hooks`（P1）

- 目标：按事件分组列出已注册 hook 的 source/matcher/handler（command 或 http）/timeout/async；
  显示配置诊断；审计 tab 显示最近运行记录（event/outcome/reason）。
- 数据：`HookRunner::registry()` 已公开，但 `RegisteredHook` 私有、`HookRegistry` 只有 `len/is_empty`。
  新增 `RegisteredHookInfo { source: String, event: HookEvent, matcher: Option<String>,
  handler: String, is_async: bool, timeout_ms: u64 }` 与 `HookRegistry::entries()`；新增
  `AgentSession::hook_snapshot()`（`core.hook_runner` accessor）。诊断需在 `Core` 保存
  `hook_diagnostics: Vec<String>` 并暴露 accessor（当前只合并进 `config_diagnostics`）。
- 不做启停：Kiana 的 hook 没有 `enabled` 字段，启停等于改 settings；P2 再做写回。
- 文件：`components/hooks_dialog.rs`、`commands/hooks.rs`、`kiana-hooks/src/runner.rs`、
  `agent_session.rs`；测试 `components_hooks_dialog.rs`、`hooks_snapshot.rs`。

### 7.4 `/language`（P1，最易做）

- 目标：列出 `SUPPORTED_LANGUAGES`（native name / full name / id / code），当前 locale 标记；
  Enter 写入 `settings_manager.set_ui_locale(code)` 并调用 `InteractiveMode::apply_ui_locale(code)`；
  显示 `Language: <native> (<full>) [id]`。
- 数据完全就绪：`get_ui_locale`/`set_ui_locale`（`settings/mod.rs:1677/1707`）、
  `SUPPORTED_LANGUAGES`（`render_core/i18n.rs:84`）、`apply_ui_locale`
  （`interactive_mode/render_core_host.rs`）。
- headless：只打印当前 locale（`Language: zh`）。支持列表的权威数据 `SUPPORTED_LANGUAGES`
  在 `kiana-interactive::render_core::i18n`，而 `kiana-session::panel_commands` 不能反向依赖
  interactive；若 P1 要在 headless 列出全部语言，需要先把 `LanguageDefinition` 表移到
  `kiana-core`/`kiana-config` 并由 interactive 再导出（单独的小重构，进 P1 前置）。
  当前 P1 不阻塞在这一点上：TUI 选择器用 interactive 里的表，headless 只报当前值。
- 文件：`components/language_selector.rs`（可基于 `SettingsList`）、`commands/language.rs`；测试
  `components_language_selector.rs`。

### 7.5 `/tools`（P1）

- 目标：列出全部工具，active 标记、exposure、namespace、source；详情显示 description、parameters
  JSON（用 `kiana_json::stringify` 格式化）、guidelines；Space 切换 active。
- 数据：`get_all_tools()`/`get_active_tool_names()`/`set_active_tools_by_name()` 均已就绪。
- 限制：active 变更仅本会话（`set_active_tools_by_name` 是 session 状态）；P2 若要持久化需新增
  settings 键（不在 TS schema）。
- 文件：`components/tools_dialog.rs`、`commands/tools.rs`；测试 `components_tools_dialog.rs`。

### 7.6 `/stats`（P1）

- 目标：单页 `SessionStats`（session_file/id、消息/工具计数、tokens、cost、context_usage）；
  `r` 刷新。
- 数据：`get_session_stats()`（`agent_session.rs:12764`）。qwen 的 activity/efficiency 两 tab 无后端，
  P2 若要需新增按 turn 聚合的统计。
- 文件：`components/stats_dialog.rs`、`commands/stats.rs`；测试 `components_stats_dialog.rs`。

### 7.7 `/context`（P1）

- 目标：显示 `ContextUsage{tokens,context_window,percent}` 与进度条/阈值提示；`r` 刷新；
  `tokens=None` 显示 `?`。
- 数据：`get_context_usage()`；无分类明细。
- 文件：`components/context_dialog.rs`、`commands/context.rs`；测试 `components_context_dialog.rs`。

### 7.8 `/rewind`（P2）

- 目标（文件级）：显示最新 checkpoint 的时间/文件数（`checkpoint_manager().latest_checkpoint(cwd)`），
  确认后 `session.restore_checkpoint()`；展示 `RestoreReport` 的已恢复文件与 `skipped_user_edits`。
- **不做对话回退**：Kiana 没有 transcript rewind；qwen `RewindSelector` 的“回到某一轮”需要
  会话树截断/分支语义，属于新功能，明确不在本设计范围。
- 缺口：`CheckpointManager` 只能读最新 checkpoint（`latest_checkpoint`）；列多个 checkpoint 需新增
  枚举 API。P2 先做“最新一个”。
- 文件：`components/rewind_dialog.rs`、`commands/rewind.rs`；测试 `components_rewind_dialog.rs`。

### 7.9 `/statusline`（P2）

- 目标：只读列出 footer 当前项（pwd、token 统计、context、cost、model、thinking、mode、扩展状态），
  可见性/顺序配置为 P3。
- 缺口：`FooterComponent` 是硬编码 2/3 行 + golden 锁定；可配置化需要 refactor 成 item composer，并
  保证默认输出逐字节不变。风险高，P2 只做只读清单，配置化不做。
- 文件：`components/statusline_dialog.rs`、`commands/statusline.rs`；测试
  `components_statusline_dialog.rs`（只读渲染）。

### 7.10 schema 驱动 settings（P2，评估项）

- 结论：**不重构 `/settings`**。Kiana 的 `/settings` 顺序与文案被
  `crates/kiana-interactive/tests/components_settings_selector.rs` 的 TS golden 锁定；qwen 的
  `getDialogSettingKeys` 依赖一套不存在的元数据表。
- 若要服务新面板，新增一个**局部** `SettingDefinition` 表（label/description/type/values/scope），
  只被 P1/P2 面板消费；不替换 `SettingsConfig`/`SettingsCallbacks`。
- codex `config_panel` 的手写 catalog 是更现实的参考（`ConfigSettingValue` 的 Toggle/Editor/
  DependentToggle/NotificationTest/Unavailable 五态）。

---

## 8. 文件级改动清单

约定：

- **Panel agent**：只新增自己的组件/命令/后端模块与对应测试；**不**改共享文件。
- **Integrate agent**：共享注册面、`mod.rs`/`index.rs`、`interactive_mode.rs`、`session.rs`、
  跨 crate 接线、print 模式、冲突诊断、最终验证。
- 路径均相对仓库根 `/home/ctyun/code/kiana`。

### 8.1 新增文件

| 文件 | 负责 | 内容 | 依赖 |
|---|---|---|---|
| `crates/kiana-interactive/src/components/panel_common.rs` | Integrate | `PanelTab`/`TabBar`、边框/标题/提示/notice/query 行渲染助手、`PanelNotice` | 无 |
| `crates/kiana-interactive/src/components/permissions_dialog.rs` | Panel A | `PermissionsDialog`、视图状态机、键盘/鼠标 | `panel_common` |
| `crates/kiana-interactive/src/interactive_mode/commands/permissions.rs` | Panel A | `show_permissions_selector`、快照→组件、回调写回 | 后端 API |
| `crates/kiana-interactive/src/components/tasks_dialog.rs` | Panel D | `TasksDialog`、列表/详情/取消/刷新 | `panel_common` |
| `crates/kiana-interactive/src/interactive_mode/commands/tasks.rs` | Panel D | `show_tasks_selector`、异步拉取、取消 | 后端 API |
| `crates/kiana-interactive/src/interactive_mode/commands/panels.rs` | Integrate | `PANEL_COMMANDS` 的消费端、`try_dispatch_panel_command` 路由到各 `commands::<panel>` | 注册表 |
| `crates/kiana-session/src/panel_commands.rs` | Integrate | `PanelCommandInfo`/`PANEL_COMMANDS`、`is_panel_command`、`execute_headless`（权限/任务文本格式器） | kiana-session |
| `crates/kiana-session/src/permissions_api.rs` | Panel A（后端） | `impl AgentSession`：`permission_snapshot`、`set_permission_workspace_roots`，以及供命令层复用的“有效规则去重/替换”helper（文件写回本身在 `commands/permissions.rs` 调用 config API） | permissions/config |
| `crates/kiana-session/src/tasks_api.rs` | Panel D（后端） | `impl AgentSession`：`list_background_tasks` | background |
| `crates/kiana-session/src/memory_api.rs` | P1 Panel | 记忆快照/条目/审计/forget | memory |
| `crates/kiana-session/src/hooks_api.rs` | P1 Panel | `hook_snapshot` | hooks |
| `crates/kiana-session/src/agents_api.rs` | P1 Panel | `agent_definitions` | agent_definition |
| `crates/kiana-config/src/settings/permissions_edit.rs` | Panel A（后端） | `remove_rule_from_permissions`、`append_workspace_root`/`remove_workspace_root`、`update_permissions_file`（复用 `permissions.rs` 的读改写与 `O_NOFOLLOW`） | 已有 permissions.rs |
| `crates/kiana-interactive/tests/components_permissions_dialog.rs` | Panel A | 渲染/键盘/状态机 | — |
| `crates/kiana-interactive/tests/interactive_mode_permissions_command.rs` | Panel A | 命令挂载/用法/模式切换 | — |
| `crates/kiana-interactive/tests/components_footer_permission_mode.rs` | Panel B | 指示器 | — |
| `crates/kiana-interactive/tests/components_tasks_dialog.rs` | Panel D | 任务面板 | — |
| `crates/kiana-interactive/tests/interactive_mode_tasks_command.rs` | Panel D | 命令挂载 | — |
| `crates/kiana-session/tests/permissions_panel_api.rs` | Panel A（后端） | 快照/增删/作用域/trust/symlink/键序 | — |
| `crates/kiana-session/tests/background_list.rs` | Panel D（后端） | journal/manager/session 列表 | — |
| `crates/kiana-config/tests/permissions_edit.rs` | Panel A（后端） | 删除/目录写回/字节格式 | — |
| `crates/kiana-builtin-extensions/tests/mcp_health_status.rs` | Panel C | 健康徽标 | — |
| `crates/kiana-rpc-print/tests/print_mode_panel_commands.rs` | Panel D（后端） | headless 拦截 | — |
| `crates/kiana-interactive/tests/interactive_mode_panels_registry.rs` | Integrate | 注册表与补全/冲突/`/mcp` 不遮蔽 | — |

### 8.2 需要改的共享文件

除 `components/footer.rs`（Panel B 拥有）、`mcp/index.rs`（Panel C 拥有）与
`kiana-permissions/{policy,request}.rs`（Panel A 后端拥有）外，其余共享文件由 Integrate agent
统一修改；面板 agent 不得直接编辑它们，以免多个面板并发改同一文件冲突。

| 文件 | 改动 | 冲突风险 |
|---|---|---|
| `crates/kiana-interactive/src/components/mod.rs` | 声明 `panel_common`/`permissions_dialog`/`tasks_dialog`（及 P1 面板）模块 | 多面板都新增模块，集中由 Integrate 合并 |
| `crates/kiana-interactive/src/components/index.rs` | 再导出新组件（作为“增量公开面”，加独立注释段；不改变 TS barrel 语义） | 与 `public_api.rs` 配套；避免面板 agent 各自改 |
| `crates/kiana-interactive/src/interactive_mode/commands/mod.rs` | `pub mod panels; pub mod permissions; pub mod tasks;` + 在 `/quit` 之后、bash 之前插入 `if self.try_dispatch_panel_command(&text) { return; }`；更新模块文档 | **最高**：所有面板共用一行分发；禁止面板 agent 修改 |
| `crates/kiana-interactive/src/interactive_mode.rs` | ①`create_base_autocomplete_provider` 追加 `panel_command_completions()`；②`get_built_in_command_conflict_diagnostics` 把面板名并入比较集合；③footer 构造（`FooterComponent::new` 内部读取 mode，无需额外改动） | 高 |
| `crates/kiana-interactive/src/interactive_mode/session.rs` | 会话 rebind 后刷新 footer mode（`set_session` 内部完成即可，确认无遗漏） | 中 |
| `crates/kiana-interactive/src/components/footer.rs` | 新增 `permission_mode` 字段 + `set_permission_mode` + render 追加段 | Panel B 拥有；Integrate 评审 |
| `crates/kiana-interactive/src/lib.rs` | 无（组件通过 `components/index.rs` 到根；若不加 index 再导出则不需要） | 低 |
| `crates/kiana-session/src/lib.rs` | `pub mod panel_commands;`、`pub mod permissions { pub use kiana_permissions::… }`、新后端模块声明；`agent_session.rs` 增加 `pub(crate)` accessor（permission engine/hook runner/memory handle） | 高：集中改 |
| `crates/kiana-session/src/background/journal.rs` | trait 增 `list_for_session` + `MemoryTaskJournal` 实现 | 与 `durable.rs` 同步 |
| `crates/kiana-session/src/background/durable.rs` | `list_for_session` + `session_slot` 私有 helper | 中 |
| `crates/kiana-session/src/background/mod.rs` | `BackgroundTaskManager::list` | 中 |
| `crates/kiana-permissions/src/policy.rs` | `set_workspace_roots`、`session_allow_count`、`PermissionRule::display_text`、`parse_permission_rule` | Panel A 后端拥有 |
| `crates/kiana-permissions/src/request.rs` | `resolve_rule_path` 提升为 `pub fn resolve_permission_path`（保留内部调用） | 低 |
| `crates/kiana-config/src/settings/mod.rs` | `get_global_permissions`/`update_global_permissions`（复用 `mutate_global_and_save_if` 的 permissions extra） | 高：settings 核心，Integrate 写 |
| `crates/kiana-config/src/settings/permissions.rs` | 仅把内部 helper 暴露给 `permissions_edit.rs`（`pub(crate)`），不改变既有行为 | 低 |
| `crates/kiana-rpc-print/src/print_mode.rs` | `try_body` 的 message/initialMessage 前置面板命令拦截 | 高：冻结 print 语义，只加不改 |
| `crates/kiana-builtin-extensions/src/mcp/index.rs` | `LocalOnly.ctx`、`emit_change` 写 health status、`session_shutdown` 清空 | Panel C 拥有 |
| `docs/history/porting/README.md` | （可选）索引里加本设计文档一行 | 低 |

### 8.3 P1/P2 新增文件（按阶段）

每个面板的组件/命令/后端模块与测试由该面板的 Panel agent 编写；`components/mod.rs`、
`components/index.rs`、`commands/mod.rs` 的模块声明与 `PANEL_COMMANDS` 表项由 Integrate agent
在合入时统一添加。

| 面板 | 组件文件 | 命令文件 | 后端 | 测试 |
|---|---|---|---|---|
| `/memory` | `components/memory_dialog.rs` | `commands/memory.rs` | `memory_api.rs` | `components_memory_dialog.rs`、`agent_session_memory_panel.rs` |
| `/agents` | `components/agents_dialog.rs` | `commands/agents.rs` | `agents_api.rs` | `components_agents_dialog.rs` |
| `/hooks` | `components/hooks_dialog.rs` | `commands/hooks.rs` | `kiana-hooks/src/runner.rs`（`RegisteredHookInfo`/`entries`）+ `hooks_api.rs` | `components_hooks_dialog.rs`、`hooks_snapshot.rs` |
| `/language` | `components/language_selector.rs` | `commands/language.rs` | 无 | `components_language_selector.rs` |
| `/tools` | `components/tools_dialog.rs` | `commands/tools.rs` | 无 | `components_tools_dialog.rs` |
| `/stats` | `components/stats_dialog.rs` | `commands/stats.rs` | 无 | `components_stats_dialog.rs` |
| `/context` | `components/context_dialog.rs` | `commands/context.rs` | 无 | `components_context_dialog.rs` |
| `/rewind` | `components/rewind_dialog.rs` | `commands/rewind.rs` | 可选 `list_checkpoints` | `components_rewind_dialog.rs` |
| `/statusline` | `components/statusline_dialog.rs` | `commands/statusline.rs` | 无（只读） | `components_statusline_dialog.rs` |

每个 P1/P2 面板在 `PANEL_COMMANDS` 注册时同步加：模块声明（Integrate）、命令文件、组件文件、
`components/index.rs` 再导出（可选）、该面板的 headless 文本。

### 8.4 实施顺序与依赖

1. **Integrate-0**：`panel_common.rs`、`kiana-session::panel_commands` 骨架（P0 两个条目）、
   `commands/panels.rs` 分派钩子、`components/mod.rs` 声明占位、`interactive_mode.rs` 补全/冲突、
   `tests/interactive_mode_panels_registry.rs`。此步不改变任何现有行为。
2. **Panel A 后端**：permissions/config 写回 API + 测试（可与 1 并行，但合入在 1 之后）。
3. **Panel B**：footer 指示器（独立，可与 2 并行）。
4. **Panel A 前端**：permissions 面板 + 命令。
5. **Panel D 后端**：tasks 列表 API + print 拦截 + 文本格式器 + 测试。
6. **Panel D 前端**：tasks 面板 + 命令。
7. **Panel C**：MCP health status + 测试（完全独立，可最早并行）。
8. **Integrate-1**：全量接线检查、冲突诊断、print/RPC 行为回归、文档、跨面板测试。
9. **P1**：language → tools → stats → context → memory → agents → hooks（按依赖与风险）。
10. **P2**：rewind 只读 → statusline 只读 → schema 评估。

---

## 9. 共享集成方案

### 9.1 命令注册

**决定**：新增 `kiana-session::panel_commands` 注册表，不改 `BUILTIN_SLASH_COMMANDS`。

```rust
// crates/kiana-session/src/panel_commands.rs
pub struct PanelCommandInfo {
    pub name: &'static str,          // 不含 '/'
    pub description: &'static str,
    pub argument_hint: Option<&'static str>,
    pub tui: bool,                   // false = 只有 headless 文本
}

pub const PANEL_COMMANDS: &[PanelCommandInfo] = &[
    PanelCommandInfo { name: "permissions", description: "Inspect and edit permission rules and workspace directories", argument_hint: None, tui: true },
    PanelCommandInfo { name: "tasks", description: "List and cancel background tasks", argument_hint: None, tui: true },
];

pub fn is_panel_command(name: &str) -> bool;
pub fn panel_command_names() -> impl Iterator<Item = &'static str>;
pub async fn execute_headless(session: &AgentSession, text: &str) -> Option<Result<String, String>>;
```

- 名字集合与 24 个内置名、`mcp`/`llama` 无交集；加一个测试断言（防止未来重名）：
  `PANEL_COMMANDS` ∩ `BUILTIN_SLASH_COMMANDS` = ∅，且不包含 `mcp`/`llama`。
- **`/mcp` 永远不进入此表**。
- 注册表放 `kiana-session` 而不是 `kiana-interactive`：headless 执行需要 `AgentSession`，
  `kiana-rpc-print` 也依赖 `kiana-session`；这样 interactive 与 print 共享同一份名字/描述/文本，
  避免 interactive → rpc-print 的反向依赖。
- **不进入** `kiana-ext-api` 的 `getCommands()` / `kiana-rpc-print` 的 `RpcSlashCommand` 列表
  （那是 TS 契约，由 `BUILTIN_SLASH_COMMANDS` + 扩展命令组成）；面板命令只在 TUI 自动补全与
  print 文本分派里出现。

**分派插入点**（`commands/mod.rs`）：在 `/quit` 分支之后、`// 阶段 2：bash 命令` 之前：

```rust
if self.try_dispatch_panel_command(text) {
    self.clear_editor();
    return;
}
```

- 放在内置链之后保证 24 个命令的匹配顺序完全不变；面板名与内置名无交集，所以位置不影响语义，
  但保留“先冻结表、后增量”的阅读顺序。
- `try_dispatch_panel_command` 只处理 `/name` 或 `/name ...`，精确匹配名字；未知参数由各面板返回
  usage，不落到模型。
- `text` 已经 `js_trim`；面板处理器自己再 trim 参数。

**自动补全**（`interactive_mode.rs:2355`）：在 `slash_commands` 构造完成后追加：

```rust
slash_commands.extend(panel_commands::PANEL_COMMANDS.iter().map(|command| {
    let mut slash = SlashCommand::new(command.name).with_description(command.description);
    if let Some(hint) = command.argument_hint { slash = slash.with_argument_hint(hint); }
    slash
}));
```

- 放在内置命令的专用补全（`/model`、`/thinking`、`/login`）之后、模板/扩展/技能命令之前或之后均可；
  建议紧跟内置表，保持“内置 + 面板”连续。
- `builtin_command_names`（扩展冲突过滤集合）是否包含面板名：**包含**，这样扩展注册同名命令不会
  在补全里出现两个；同时 `get_built_in_command_conflict_diagnostics` 要单独为面板名生成诊断，
  否则扩展作者看不到“被核心面板遮蔽”的提示。诊断文案沿用
  `Extension command '/x' conflicts with built-in interactive command.`（“built-in”在该语境下指
  core interactive command，不改文案以免破坏既有测试；新增测试只断言面板名也被检测）。

**headless**：

- `kiana-rpc-print` 在 `try_body` 的 `prompt` 之前调用 `execute_headless`；
- 只拦截 `PANEL_COMMANDS` 里的名字；24 个内置命令保持现状（print 模式仍会把未知内置命令发给模型，
  这是既有 TS 行为，不在本设计改变）；
- `/mcp` 继续由扩展命令在 `session.prompt` 内拦截，print 模式天然可用（`format_status`）。需要新增
  回归测试证明面板拦截没有抢走扩展命令。

### 9.2 组件声明与导出

- `components/mod.rs`：按字母序加 `pub mod panel_common; pub mod permissions_dialog; pub mod tasks_dialog;`
  （P1 同理）。
- `components/index.rs`：新增独立注释段：

  ```rust
  // --- TUI panels（非 TS barrel：本设计新增的增量公开面） ---
  pub use crate::components::permissions_dialog::PermissionsDialog;
  pub use crate::components::tasks_dialog::TasksDialog;
  pub use crate::components::panel_common::{PanelNotice, PanelTab, TabBar};
  ```

  `tests/public_api.rs` 追加 `type_name` 等值断言（新增测试函数，不改旧断言）。
- 若评审要求“index.rs 只镜像 TS barrel”，则**不**加 index 再导出，测试用深路径
  `kiana_interactive::components::permissions_dialog::PermissionsDialog`。两种都可执行；本设计推荐加，
  因为 `components/mod.rs` 的维护提示明确要求“新增组件需要同时声明模块 + 在 index.rs 再导出”。

### 9.3 footer 挂载

- 不改 `mount_interactive_tui` 的七容器顺序与数量；模式指示器是 `FooterComponent` 内部的一个可选项，
  因此 `FakeTui`/全屏布局/overlay 测试都不受影响。
- MCP 健康徽标走扩展状态行（第三条），只有在有离线服务器时才多一行；这与今天“扩展状态非空才三行”
  的既有语义一致，无需新挂载点。
- 若未来 P2 做 footer 可配置，才需要把 footer 拆成 item composer；P0/P1 不动。

### 9.4 dialog 生命周期

1. **单一选择器**：`show_selector` 在挂载新面板前 `dispose_active_selector()`；旧面板的
   `dispose` 执行一次。`done()` 通常是组件在 `handle_input` 调用栈里触发，因此必须用
   `spawn_local` 延后恢复编辑器；面板**不得**在 `render` 里调用 `done`。
2. **与扩展对话框互斥**：`push_extension_dialog` 会先 `dispose_active_selector()`；反过来，
   `show_selector` 不会清空 `extension_dialogs`。真实输入路径下扩展对话框占用编辑器槽，用户无法
   输入 `/permissions`；但如果将来增加程序化入口（例如扩展调用面板），必须先检查
   `inner.extension_dialogs.is_empty()`，否则关闭扩展对话框时 `show_top_extension_dialog` 会覆盖面板。
   本设计不新增程序化入口，仅记录该不变量。
3. **会话 rebind/重置**：`interactive_mode.rs:3749` 与扩展 reset 会 `dispose_active_selector()`；
   面板随会话切换自动关闭。面板数据是快照，不需要做“过期刷新”。
4. **Esc 优先级**：`handle_escape`（`commands/mod.rs:707`）不处理选择器；Esc 由组件的
   `handle_input` 消费。面板内部先处理“清 query / 回上一视图 / 关闭”，不要依赖全局 Esc 链。
5. **焦点**：面板组件实现 `Focusable` 并 `as_focusable() -> Some(self)`，用
   `SelectorFocus::Component`；不要用 `SelectorFocus::Address` 指向内部列表（面板自己转发键，
   需要自己接收焦点）。若面板把列表作为唯一可聚焦子项、自己不处理键，才用 `Address`。
6. **dispose**：P0 面板无定时器/订阅，`dispose: None`；`/tasks` 的异步拉取若在面板关闭后完成，
   通过 `Weak` 升级失败丢弃结果。

### 9.5 冲突点清单

| 冲突 | 处理 |
|---|---|
| `BUILTIN_SLASH_COMMANDS` 必须保持 24 项 | 面板命令走独立注册表；不改 `kiana-core`；`crates/kiana-core/tests/slash_commands.rs` 不动 |
| `/mcp` 是扩展命令 | 面板注册表排除 `mcp`；`dispatch_submit` 的面板分支不匹配 `mcp`；测试确保 `/mcp` 仍进入 `session.prompt` |
| 扩展命令与面板同名 | 面板优先；补全过滤扩展项；冲突诊断提示扩展作者 |
| footer golden | mode 段只在 `Some` 时输出；默认 `None`；不改行数 |
| `/settings` 顺序 golden | 不重构 `/settings`；P1 面板独立 |
| 扩展对话框 vs 选择器 | 共用编辑器槽；后到者占位；文档化“只能有一个” |
| `render_core::ModalArbiter` | P0 面板不依赖实验渲染核心；选择器路径在开关开/关下都工作（与 `/settings` 相同） |
| `RefCell` 重入 | 回调只捕获 `Weak`；写 UI/会话走 `run_after_dispatch`/`run_mode_callback`；不在 `render` 里 `done()` |
| 单线程 `LocalSet` | 所有异步用 `spawn_local`；`!Send` 状态不跨线程 |
| 设置写队列 | 写后 `flush().await` + `drain_errors()`；写失败不更新运行期 |
| 项目信任 | Project/Local 作用域在未受信时禁用；错误文案复用 `PROJECT_NOT_TRUSTED_WRITE_MESSAGE` |
| 符号链接 | 新写回复用 `permissions.rs` 的 `O_NOFOLLOW`/读改写；测试覆盖 symlink 拒写 |
| 规则去重/多层 | 快照按 `to_settings_value()` 去重并记录所有作用域；删除时删所有可编辑层的等值项 |
| `kiana-interactive` 无 `kiana-permissions` 依赖 | 用 `kiana-session::permissions` 再导出；不新增依赖边（若新增须改 `ARCHITECTURE.md` 附录 A） |
| 新 UI 文案与 i18n | P0 只英文；不与 `render_core/i18n.rs` 的 strict parity 交互（不动内置包）。P1 若要中文化，新增 key 并同步 en/zh（strict locale 必须 parity） |
| `public_api.rs` | 新组件若加入 crate 根，必须追加等价断言；否则保持深路径 |

### 9.6 数据刷新与借用纪律

- 面板打开时取一次快照；`r` 显式刷新。不要在 `render` 里读会话/设置（会持借用、拖慢帧）。
- 回调里需要异步（flush、cancel、list）时：先在同步部分改视图/`notice`，再 `spawn_local` 做异步，
  完成后用 `Weak` 升级改组件/`request_render`。
- 组件共享状态用 `Rc<RefCell<PanelState>>` 仅在面板内部；不要把它塞进 `InteractiveModeInner`。
- 面板打开期间的外部变化（别的入口改了权限/任务）不会自动反映；文档明说“`r` 刷新”。

---

## 10. 风险与不做项

### 10.1 风险

| 风险 | 影响 | 缓解 |
|---|---|---|
| 误把面板命令加入 `BUILTIN_SLASH_COMMANDS` | 破坏 TS golden 与 24 项契约 | 独立注册表 + 断言测试 + code review 检查 |
| footer 模式段破坏 golden | 大面积测试失败 | 默认 `None` 分支保持逐字节；新增测试单独文件 |
| `SettingsManager` 写队列异步失败 | 面板显示成功但未落盘 | 写后 `flush().await` + `drain_errors()`；失败不变更运行期 |
| 规则删除误删 managed/CLI 策略 | 权限放宽 | `ManagedOrCli` 不可删；删除前检查 `removable`；写文件成功后才 `set_permission_rules` |
| 规则跨层重复 | 删了高层仍生效，用户困惑 | 快照显示全部作用域；删除所有可编辑层的等值项 |
| durable journal 列表开销/跨会话泄露 | 面板卡顿或看到别人的任务 | 只读目标会话文档；`list_for_session` 严格按 `parent_session_id` 过滤；`spawn_local` + 上限保护（若记录超过 200 条，按创建时间取最近 200 并提示） |
| print 拦截抢走 `/mcp` 或内置命令 | 行为回归 | 只匹配 `PANEL_COMMANDS`；新增 `/mcp` 回归测试 |
| RPC/JSON 模式未拦截面板命令 | 命令被发给模型 | 文档明示 P0 不支持；P1 再补 RPC/JSON 通道（JSON 模式不能写裸文本，需要事件帧） |
| `/mcp` health 文本无颜色 | 视觉弱于 qwen | 用 `⚠`；在扩展状态行架构内不引入按条着色 |
| 新面板文案英文 | 中文 locale 下混排 | P0 接受；P1 加 i18n key（en/zh strict parity） |
| 面板数量增长导致 `commands/mod.rs` 膨胀 | 维护成本 | 每个面板独立文件；`panels.rs` 只做路由 |
| 选择器/扩展对话框抢占 | 面板被意外关闭 | 保持“只能一个”的不变量；不新增程序化入口 |
| `kiana-interactive` 公开面扩大 | API 维护成本 | 新组件作为“增量公开面”集中注释；或保持深路径不导出 |

### 10.2 不做项（非目标）

1. **不修改** `reference_project/**`、`docs/history/porting/plan.md`；不 commit/push/tag。
2. **不修改** `BUILTIN_SLASH_COMMANDS`、`crates/kiana-core/tests/slash_commands.rs`、任何 TS golden
   fixture 或既有测试断言；不删除测试、不新增 `#[ignore]`、不放宽断言。
3. **不新增** crate 依赖或升级版本；若确需新增依赖边，必须同步 `ARCHITECTURE.md` 附录 A（本设计
   通过 `kiana-session::permissions` 再导出避免）。
4. **不重构** `/settings` 为 schema 驱动（P2 仅评估）；不改 footer 行数/固定布局。
5. **不重写**渲染核心；面板不依赖 `render_core`（`KIANA_NEXT_RENDER_CORE` 开或关都工作）。
6. **不做**对话级 rewind、transcript 编辑、多 checkpoint 列表（P2 只做最新文件 checkpoint）。
7. **不做**任务输出实时跟随/日志 tail（P0 只显示路径与大小）；不做 monitor 任务后端（当前不存在）。
8. **不做** agent 创建/编辑、memory 自动抽取开关、hook 启停、statusline 可配置（均为后续阶段）。
9. **不做** RPC/JSON 模式的面板协议（P0 仅 print 文本回退）。
10. **不做**鼠标优先交互；P0 以键盘为准，鼠标作为兼容转发。

---

## 11. 验收与测试命令

设计落地后的验证清单（实现阶段执行；本设计提交本身只新增 Markdown，不应触发 Rust 构建）：

```bash
# 1. 单 crate 快速检查（按依赖顺序）
CARGO_TARGET_DIR=/data/cargo-target-sandbox cargo check -p kiana-permissions
CARGO_TARGET_DIR=/data/cargo-target-sandbox cargo check -p kiana-config
CARGO_TARGET_DIR=/data/cargo-target-sandbox cargo check -p kiana-session
CARGO_TARGET_DIR=/data/cargo-target-sandbox cargo check -p kiana-builtin-extensions
CARGO_TARGET_DIR=/data/cargo-target-sandbox cargo check -p kiana-interactive
CARGO_TARGET_DIR=/data/cargo-target-sandbox cargo check -p kiana-rpc-print
CARGO_TARGET_DIR=/data/cargo-target-sandbox cargo check -p kiana-coding-agent

# 2. 关键回归（必须全绿）
CARGO_TARGET_DIR=/data/cargo-target-sandbox cargo test -p kiana-core --test slash_commands
CARGO_TARGET_DIR=/data/cargo-target-sandbox cargo test -p kiana-interactive --test components_footer
CARGO_TARGET_DIR=/data/cargo-target-sandbox cargo test -p kiana-interactive --test components_settings_selector
CARGO_TARGET_DIR=/data/cargo-target-sandbox cargo test -p kiana-session --test permissions_integration
CARGO_TARGET_DIR=/data/cargo-target-sandbox cargo test -p kiana-session --test background
CARGO_TARGET_DIR=/data/cargo-target-sandbox cargo test -p kiana-builtin-extensions --test mcp_manager_view
CARGO_TARGET_DIR=/data/cargo-target-sandbox cargo test -p kiana-builtin-extensions --test mcp_extension
CARGO_TARGET_DIR=/data/cargo-target-sandbox cargo test -p kiana-builtin-extensions --test mcp_command
CARGO_TARGET_DIR=/data/cargo-target-sandbox cargo test -p kiana-rpc-print --test print_mode_parity

# 3. 新面板测试（示例）
CARGO_TARGET_DIR=/data/cargo-target-sandbox cargo test -p kiana-interactive --test components_permissions_dialog
CARGO_TARGET_DIR=/data/cargo-target-sandbox cargo test -p kiana-interactive --test interactive_mode_panels_registry
CARGO_TARGET_DIR=/data/cargo-target-sandbox cargo test -p kiana-session --test permissions_panel_api
CARGO_TARGET_DIR=/data/cargo-target-sandbox cargo test -p kiana-session --test background_list
CARGO_TARGET_DIR=/data/cargo-target-sandbox cargo test -p kiana-rpc-print --test print_mode_panel_commands

# 4. 格式/风格（只检查，不用 cargo fmt 改文件）
cargo fmt --check -p kiana-interactive -p kiana-session -p kiana-config -p kiana-permissions
cargo clippy -p kiana-interactive -p kiana-session
python3 scripts/check_comment_only_diff.py --baseline <dir>   # 若改动含注释/文档混合
```

验收标准：

- `BUILTIN_SLASH_COMMANDS` 仍为 24 项，fixture 逐字相等；
- footer 既有 golden 在未设置 mode 时逐字节相等；
- `/mcp` 仍由扩展处理（TUI 管理器 + headless `format_status`）；
- print 模式 `/tasks` 不产生模型请求；
- 权限写回在未受信项目、损坏 JSON、符号链接路径上 fail-closed；
- 任务列表不跨会话泄露，终态任务可见，取消语义与 `cancel_background_task` 一致。

---

## 附录 A：新增 API 签名汇总

### A.1 `kiana-permissions`

```rust
// policy.rs
impl PermissionRule {
    /// 稳定的人类可读形态（面板/文本输出用）。
    pub fn display_text(&self) -> String;
}
/// 把 settings 里的单条规则对象解析为 `PermissionRule`（与启动路径同一实现）。
/// 放在 `kiana-permissions`，避免 session/UI 复制解析逻辑。
pub fn parse_permission_rule(entry: &serde_json::Value) -> Option<PermissionRule>;
impl PermissionEngine {
    /// 替换全部工作区根（第一个通常是会话 cwd），并归一化 `~`/相对路径。
    pub fn set_workspace_roots(&self, roots: Vec<PathBuf>);
    /// 本会话“总是允许”记录数。
    pub fn session_allow_count(&self) -> usize;
}
// request.rs：把现有 pub(crate) resolve_rule_path 提升为公开薄包装
pub fn resolve_permission_path(base: &Path, path: &Path) -> PathBuf;
```

### A.2 `kiana-config`

```rust
// settings/mod.rs
impl SettingsManager {
    pub fn get_global_permissions(&self) -> Option<serde_json::Value>;
    pub fn update_global_permissions(
        &self,
        update: impl FnOnce(Option<&serde_json::Value>) -> serde_json::Value,
    ) -> Result<(), SettingsValueError>;
}
// settings/permissions_edit.rs（新）
pub fn update_permissions_file(
    path: &Path,
    update: impl FnOnce(&mut serde_json::Value) -> Result<(), SettingsValueError>,
) -> Result<(), SettingsValueError>;
pub fn remove_rule_from_permissions(
    permissions: &mut serde_json::Value,
    raw: &serde_json::Value,
) -> Result<bool, SettingsValueError>;
pub fn append_workspace_root(
    permissions: &mut serde_json::Value,
    root: &str,
) -> Result<bool, SettingsValueError>;
pub fn remove_workspace_root(
    permissions: &mut serde_json::Value,
    root: &str,
) -> Result<bool, SettingsValueError>;
```

### A.3 `kiana-session`

```rust
// permissions_api.rs（新，impl AgentSession）
impl AgentSession {
    pub fn permission_snapshot(&self) -> PermissionSnapshot;
    /// 运行期替换工作区根（同步 additional_writable_roots + 引擎 + 沙箱策略）。
    pub fn set_permission_workspace_roots(&self, roots: Vec<PathBuf>);
}
// agent_session.rs（新增 pub(crate) accessor）
pub(crate) fn permission_engine(&self) -> Option<Arc<PermissionEngine>>;

// tasks_api.rs（新，impl AgentSession）
impl AgentSession {
    pub async fn list_background_tasks(
        &self,
    ) -> Result<Vec<crate::background::BackgroundTaskRecord>, crate::background::BackgroundTaskError>;
}

// background/journal.rs
#[async_trait]
impl BackgroundTaskJournal {
    async fn list_for_session(
        &self,
        parent_session_id: &str,
    ) -> Result<Vec<BackgroundTaskRecord>, BackgroundTaskError>;
}
// background/mod.rs
impl BackgroundTaskManager {
    pub async fn list(
        &self,
        parent_session_id: &str,
    ) -> Result<Vec<BackgroundTaskRecord>, BackgroundTaskError>;
}

// panel_commands.rs（新）
pub struct PanelCommandInfo { … }
pub const PANEL_COMMANDS: &[PanelCommandInfo];
pub fn is_panel_command(name: &str) -> bool;
pub async fn execute_headless(
    session: &AgentSession,
    text: &str,
) -> Option<Result<String, String>>;
```

### A.4 `kiana-interactive`

```rust
// components/footer.rs
impl FooterComponent {
    pub fn set_permission_mode(&mut self, mode: Option<PermissionMode>);
    pub fn permission_mode(&self) -> Option<PermissionMode>;
}
// commands/panels.rs（新，pub(crate)）
impl InteractiveMode {
    pub(crate) fn try_dispatch_panel_command(&mut self, text: &str) -> bool;
}
// components/panel_common.rs（新）
pub struct PanelTab { pub id: &'static str, pub label: String }
pub enum PanelNoticeKind { Info, Warning, Error }
pub struct PanelNotice { pub kind: PanelNoticeKind, pub text: String }
pub struct TabBar { … } // render / handle_input / active
pub fn render_panel_frame(…, width: usize) -> Vec<String>;
// components/permissions_dialog.rs / tasks_dialog.rs
pub struct PermissionsDialog { … }
pub struct TasksDialog { … }
```

---

## 附录 B：参考文件索引（本设计直接引用）

- qwen-code：
  - `packages/cli/src/ui/commands/{permissionsCommand,approvalModeCommand,mcpCommand,tasksCommand,agentsCommand,memoryCommand,languageCommand,settingsCommand}.ts`
  - `packages/cli/src/ui/components/{PermissionsDialog,ApprovalModeDialog,MemoryDialog,StatsDialog,StatusLineDialog,SettingsDialog,AutoAcceptIndicator}.tsx`
  - `packages/cli/src/ui/components/approvalModeVisuals.ts`
  - `packages/cli/src/ui/components/mcp/{MCPManagementDialog,MCPHealthPill}.tsx`
  - `packages/cli/src/ui/components/subagents/manage/AgentsManagerDialog.tsx`
  - `packages/cli/src/ui/components/hooks/HooksManagementDialog.tsx`
  - `packages/cli/src/ui/components/RewindSelector.tsx`
  - `packages/cli/src/ui/types.ts`、`packages/cli/src/ui/hooks/slashCommandProcessor.ts`、
    `packages/cli/src/ui/opentui/opentui-dialog-mount.tsx`（dialog union/分发/挂载）
- gemini-cli：
  - `packages/cli/src/ui/components/{ApprovalModeIndicator,BackgroundTaskDisplay,MemoryUsageDisplay,HookStatusDisplay,HooksDialog,FooterConfigDialog}.tsx`
  - `packages/cli/src/ui/components/StatusRow.tsx`、`Footer.tsx`（挂载点）
- cline：
  - `apps/cli/src/tui/components/dialogs/command-palette-items.ts`
  - `apps/cli/src/tui/components/dialogs/{mcp-manager-dialog,tool-approval,checkpoint-picker,config-dialogs}.tsx`
- codex：
  - `codex-rs/tui/src/bottom_pane/approval_overlay.rs`
  - `codex-rs/tui/src/config_panel/{mod,input,render,toggle}.rs`
- Kiana（现状锚点）：
  - `crates/kiana-interactive/src/interactive_mode/commands/mod.rs`
  - `crates/kiana-interactive/src/interactive_mode/commands/settings.rs`
  - `crates/kiana-interactive/src/components/{footer,status_indicator,settings_selector}.rs`
  - `crates/kiana-interactive/src/interactive_mode/{extension_ui/mod.rs,render.rs,session.rs}`
  - `crates/kiana-session/src/{agent_session.rs,background/*,memory_wiring.rs,agent_definition/mod.rs}`
  - `crates/kiana-permissions/src/{policy.rs,mode.rs,approval.rs}`
  - `crates/kiana-config/src/settings/{mod.rs,permissions.rs,schema.rs,layers.rs}`
  - `crates/kiana-builtin-extensions/src/mcp/{index.rs,ui.rs,cli.rs,config.rs,runtime.rs}`
  - `crates/kiana-rpc-print/src/print_mode.rs`
  - `crates/kiana-core/src/slash_commands.rs`、`crates/kiana-core/tests/slash_commands.rs`
