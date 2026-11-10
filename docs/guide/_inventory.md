# Kiana 用户文档事实底座（_inventory）

> 本文件是后续所有用户文档的唯一事实来源。写作用词以本文件为准；本文件没有列出的行为，
> 一律不得写成“已支持”。
>
> 生成日期：2026-11-08
> 代码目录：`/home/ctyun/code/kiana`（仓库当前没有 git commit / tag）
> 核对二进制：`/home/ctyun/code/kiana/dist/v0.1.0-alpha/kiana`
> 平台：Linux x86_64

## 0. 快照、版本与标记

| 项 | 值 | 来源 |
| --- | --- | --- |
| `kiana --version` | `1.4.0` | 运行 `dist/v0.1.0-alpha/kiana --version` |
| workspace 包版本 | `0.1.0` | `Cargo.toml` `[workspace.package].version` |
| 发布目录名 | `v0.1.0-alpha` | 目录名，仅本地产物标识 |
| 版本差异说明 | `--version` 输出 `1.4.0` 来自 TS 兼容常量，是有意保留的差异，不是构建错误 | `crates/kiana-core/src/config.rs:162`；`docs/release/v0.1.0-alpha.md` §1 |

状态标记：

- `[VERIFIED]`：已用 `--help`、实际运行命令或源码行核对。
- `[PARTIAL]`：能力存在，但有明确边界、未接线部分或仅部分路径生效。
- `[NOT IMPLEMENTED]`：代码/文档明确说明尚未实现，写作时不得写成已完成。
- `[UNVERIFIED]`：无法从当前代码或帮助文本确认；用户文档不得据此下结论。
- `[INTERNAL]`：仅内部、注入或测试使用，不应写进用户手册（除非专门写“调试/开发”章节）。

引用简写约定：`phase-*.md`、`fix-passes/*.md`、`wiring/*.md`、`handoffs/*.md` 均相对
`docs/history/porting/`；`args.rs`、`app.rs`、`schema.rs`、`mod.rs`、`commands/mod.rs` 等
短名在同一小节首次出现时会给出完整路径，未给出时以该小节开头的完整路径为准。

---

## 1. CLI 命令表

主程序：`kiana [options] [--] [@files...] [messages...]`
来源：`kiana --help`（`crates/kiana-coding-agent/src/cli/args.rs:729-932`）。

| 命令（精确拼写） | 完整用法 | 作用 | 来源 |
| --- | --- | --- | --- |
| `kiana` | `kiana [options] [--] [@files...] [messages...]` | 交互式 / 非交互式主程序 | `--help` |
| `kiana install` | `kiana install <source> [-l] [--approve\|--no-approve]` | 安装包（extension/package）并写入 settings | `kiana install --help` |
| `kiana remove` | `kiana remove <source> [-l] [--approve\|--no-approve]` | 从 settings 移除包及其 source | `kiana remove --help` |
| `kiana uninstall` | `kiana uninstall <source> [-l]` | `remove` 的别名 | `kiana uninstall --help` |
| `kiana update` | `kiana update [source\|self\|kiana] [--self\|--extensions\|--models\|--all] [--extension <source>] [--approve\|--no-approve] [--force]` | 更新 kiana、扩展或模型目录 | `kiana update --help` |
| `kiana list` | `kiana list [--approve\|--no-approve]` | 列出用户与项目 settings 中安装的包 | `kiana list --help` |
| `kiana config` | `kiana config [-l] [--approve\|--no-approve]` | 打开资源启用/禁用 TUI；`Tab` 切换 global/project 作用域 | `kiana config --help` |
| `kiana auth` | `kiana auth <command>` | 打印凭证或检查 provider 是否就绪 | `kiana auth --help`；裸 `kiana auth` 打印帮助且退出码 0（实测） |
| `kiana mcp` | `kiana mcp <command>` | 管理 MCP 服务器 | `kiana mcp --help`；裸 `kiana mcp` 打印帮助且退出码 0（实测） |
| `kiana <command> --help` | — | 显示上述子命令帮助 | `--help` 的 Commands 段 |

补充（实测/源码）：

- `kiana install` 无参数时输出 `Missing install source.` 与用法，退出码 1。
- `kiana-rpc` 是独立二进制（`dist/v0.1.0-alpha/kiana-rpc`）；它等价于在用户参数前强制加
  `--mode rpc`，用户显式 `--mode` 仍可覆盖（`crates/kiana-coding-agent/src/bin/kiana-rpc.rs:111-120`；
  `crates/kiana-coding-agent/src/cli/args.rs:151-154`）。`--help` 输出与 `kiana` 相同（实测）。
- `--serve` 模式提供 daemon 端点：`GET /events`（SSE 或 WebSocket）、`GET /events/latest?limit=N`、
  `GET /healthz`；默认只监听 loopback（见 §2、§8）。

### 1.1 `kiana auth` 子命令

来源：`kiana auth --help`（`crates/kiana-coding-agent/src/cli/auth_command.rs:152-163`）。

| 子命令 | 用法 | 说明 |
| --- | --- | --- |
| `print-api-key` | `kiana auth print-api-key [--provider <provider>] [--model <model>]` | 打印匹配 provider 的 API key |
| `print-bearer-token` | `kiana auth print-bearer-token [--provider <provider>] [--model <model>] [--min-expiry <duration>]` | 打印 OAuth bearer token；过期会刷新 |
| `check` | `kiana auth check [--provider <provider>] [--model <model>] [--json] [--credentials] [--no-refresh]` | 检查 provider 就绪状态；默认刷新过期 OAuth，`--no-refresh` 禁止刷新；`--credentials` 输出凭证或把它包含在 JSON 中 |

共同约束（帮助原文）：`auth` 命令至少需要 `--provider` 或 `--model` 之一。
`--min-expiry` 只允许用于 `print-bearer-token`，格式为 `<数字><ms|s|m|h>`（如 `30m`、`1h`），
默认 `30m`（`30 * 60_000` ms）。源码：`crates/kiana-coding-agent/src/cli/auth_command.rs:210-227,268-287`；
`crates/kiana-coding-agent/src/cli/credential_print.rs:44-48`。
`--json` / `--credentials` / `--no-refresh` 只允许用于 `check`（`auth_command.rs:226-240`）。

### 1.2 `kiana mcp` 子命令

来源：`kiana mcp --help`；实现入口 `crates/kiana-builtin-extensions/src/mcp/cli.rs:615`。

| 子命令 | 用法 | 说明 |
| --- | --- | --- |
| `add` | `kiana mcp add <server> [options] -- <command> [args...]` 或 `kiana mcp add <server> [options] --url <url>` | 新增/替换 `mcp.json` 中的服务器 |
| `remove` | `kiana mcp remove <server> [-l]` | 从 `mcp.json` 移除服务器 |
| `list` | `kiana mcp list [--json]` | 显示状态、工具与错误；失败时退出码 1 |
| `login` | `kiana mcp login <server> [--timeout <seconds>]` | 浏览器 OAuth 登录；`--timeout` 默认 300 秒 |
| `logout` | `kiana mcp logout <server>` | 删除已保存的 OAuth 凭证 |

`add` 选项（精确拼写）：`--url <url>`、`--env <KEY=VALUE>`（可重复）、`--cwd <dir>`、
`--header <KEY=VALUE>`（可重复）、`--bearer-token-env-var <NAME>`、`--oauth-client-id <id>`、
`--oauth-client-secret <secret>`、`--oauth-callback-port <port>`、`--oauth-client-name <name>`、
`--exposure <mode>`（`codemode` 默认 / `deferred` / `direct` / `hidden`）、`--description <text>`。
`add` 与 `remove` 共用 `-l, --local`（写当前项目的 `.kiana/mcp.json` 而不是全局文件）。

读取位置：`<agentDir>/mcp.json`；项目受信任时再读 `<cwd>/.kiana/mcp.json`
（`crates/kiana-builtin-extensions/src/mcp/config.rs:50-52,556-582`）。

---

## 2. CLI flag 表

### 2.1 `--help` 中公开的选项

来源：`kiana --help`；模板见 `crates/kiana-coding-agent/src/cli/args.rs:745-798`。

| Flag（精确拼写与别名） | 取值 | 默认 / 说明 | 来源 |
| --- | --- | --- | --- |
| `--provider <name>` | 字符串 | 配合 `--model` 搜索 provider；要求同时给 `--model` | `--help` |
| `--model <pattern>` | 模型 pattern 或 ID；支持 `provider/id` 与可选 `:<thinking>` | 无默认；示例 `sonnet:high` | `--help` |
| `--api-key <key>` | 字符串 | 默认从环境变量读取 | `--help` |
| `--system-prompt <text>` | 文本 | 默认使用 coding assistant prompt | `--help` |
| `--append-system-prompt <text>` | 文本或文件内容 | 可多次使用 | `--help` |
| `--mode <mode>` | `text` / `json` / `rpc` | 默认 `text` | `--help` |
| `--print`, `-p` | 无 | 非交互模式：处理 prompt 后退出 | `--help` |
| `--continue`, `-c` | 无 | 继续上一个会话 | `--help` |
| `--resume`, `-r` | 无 | 选择要恢复的会话 | `--help` |
| `--session <path\|id>` | 会话文件或部分 UUID | 使用指定会话 | `--help` |
| `--session-id <id>` | 精确项目会话 ID | 不存在则创建 | `--help` |
| `--fork <path\|id>` | 会话文件或部分 UUID | 复制为新会话 | `--help` |
| `--session-dir <dir>` | 目录 | 覆盖会话目录；优先级见 §6 | `--help` |
| `--no-session` | 无 | 不保存会话（临时） | `--help` |
| `--name`, `-n <name>` | 名称 | 设置会话显示名；空白值会报错 | `--help`；`args.rs:276-279,391-397` |
| `--models <patterns>` | 逗号分隔的 pattern | 供 Ctrl+P 循环；支持 glob 与模糊匹配 | `--help` |
| `--no-tools`, `-nt` | 无 | 禁用所有工具（内建与扩展） | `--help` |
| `--no-builtin-tools`, `-nbt` | 无 | 禁内建工具，保留扩展/自定义工具 | `--help` |
| `--tools`, `-t <tools>` | 逗号分隔 allowlist / pattern | MCP 工具默认保留，除非条目以 `mcp__` 开头；仅 `+name`/`-name` 条目增删默认工具 | `--help` |
| `--exclude-tools`, `-xt <tools>` | 逗号分隔 denylist / pattern | 作用于所有工具，包括 MCP | `--help` |
| `--permission-mode <mode>` | `read-only` / `default` / `accept-edits` / `bypass` | 无默认；未给时沿用 settings/env | `--help`；`args.rs:460-495` |
| `--allow <rule>` | 权限规则 | 可多次使用 | `--help` |
| `--deny <rule>` | 权限规则 | 可多次使用 | `--help` |
| `--add-dir <dir>` | 目录 | 追加可写工作区根；可多次使用 | `--help` |
| `--thinking <level>` | `off` / `minimal` / `low` / `medium` / `high` / `xhigh` / `max` | 无默认；非法值仅警告 | `--help`；`args.rs:249-251,542-554` |
| `--extension`, `-e <path>` | 扩展文件或 `builtin:<name>` | 可多次使用 | `--help` |
| `--no-extensions`, `-ne` | 无 | 关闭扩展发现与内建扩展；显式 `-e` 仍生效 | `--help` |
| `--no-mcp` | 无 | 关闭内建 MCP 支持：不连接服务器、不暴露 MCP 工具 | `--help` |
| `--skill <path>` | skill 文件或目录 | 可多次使用 | `--help` |
| `--no-skills`, `-ns` | 无 | 关闭 skills 发现与加载 | `--help` |
| `--prompt-template <path>` | prompt 模板文件或目录 | 可多次使用 | `--help` |
| `--no-prompt-templates`, `-np` | 无 | 关闭 prompt template 发现与加载 | `--help` |
| `--theme <path>` | 主题文件或目录 | 可多次使用 | `--help` |
| `--use-theme <name[/name]>` | 主题名；`name[/name]` 形式 | 本次运行的初始交互主题 | `--help` |
| `--no-themes` | 无 | 关闭主题发现与加载 | `--help` |
| `--no-context-files`, `-nc` | 无 | 关闭 `AGENTS.md` 与 `CLAUDE.md` 发现与加载 | `--help` |
| `--export <file> [output]` | 输入会话文件；可选输出路径 | 导出为 HTML 后退出；输出路径取自 `--export <file>` 后的第一个位置参数（`parsed.messages.first()`） | `--help`；`crates/kiana-coding-agent/src/app.rs:360-380` |
| `--list-models [search]` | 可选搜索串 | 列出可用模型；搜索串不能以 `-`/`@` 开头，否则退化为无搜索 | `--help`；`args.rs:616-623` |
| `--verbose` | 无 | 强制 verbose 启动，覆盖 `quietStartup` setting | `--help` |
| `--tui-mode <mode>` | `fullscreen`（默认）/ `regular` | 无 setting 时默认 fullscreen | `--help`；`crates/kiana-config/src/settings/defaults.rs:230` |
| `--serve` | 无 | 运行 daemon 事件服务（SSE/WebSocket/REST），收到 SIGINT 退出 | `--help` |
| `--serve-addr <host:port>` | 地址 | 默认 `127.0.0.1:0` | `--help` |
| `--serve-token <token>` | 字符串 | 覆盖自动生成的 daemon bearer token | `--help` |
| `--approve`, `-a` | 无 | 本次运行信任项目本地文件 | `--help` |
| `--no-approve`, `-na` | 无 | 本次运行忽略项目本地文件 | `--help` |
| `--offline` | 无 | 关闭启动期网络操作，等价于 `KIANA_OFFLINE=1` | `--help`；`app.rs:231-240`（注意：实际生效路径是扫描原始 argv，不是 `Args.offline` 字段） |
| `--` | 无 | 结束选项解析，其后 token 全部按消息/`@file` 处理 | `--help` |
| `--help`, `-h` | 无 | 显示帮助 | `--help` |
| `--version`, `-v` | 无 | 显示版本号 | `--help` |

补充：`--permission-mode=<value>`、`--allow=<value>`、`--deny=<value>`、`--add-dir=<value>`
的 `=` 形式也被接受（`args.rs:460-541`）。`--permission-mode` 的取值在代码里还接受别名
`readonly`/`plan`/`ro`、`ask`、`acceptedits`/`accept`/`workspace-write`/`workspace`、
`bypass-permissions`/`bypasspermissions`/`danger-full-access`/`danger`/`full-access`
（`crates/kiana-permissions/src/mode.rs:32-43`）；这些别名未写进 `--help`。

### 2.2 `--help` 未列出、但代码接受的选项

这些是内部/未文档化选项，用户文档不得当作稳定公开接口；若提到，必须注明“未在 `--help` 中列出”。

| Flag | 取值 | 效果 | 默认 | 来源 |
| --- | --- | --- | --- | --- |
| `--max-turns <n>` | 正整数 | 覆盖 `settings.budget.maxTurns` | 未给时读 settings | `args.rs:429-434`；`app.rs:1867-1873` |
| `--max-tokens <n>` | 正整数 | 覆盖累计 token 预算 | 未给时读 settings | `args.rs:435-440`；`app.rs:1867-1873` |
| `--max-cost-usd <usd>` | 正数 | 覆盖累计费用预算 | 未给时读 settings | `args.rs:441-446`；`app.rs:1867-1873` |
| `--max-wall-clock-ms <ms>` | 正整数 | 覆盖墙钟预算 | 未给时读 settings | `args.rs:447-455`；`app.rs:1867-1873` |
| `--no-loop-guard` | 无 | 关闭 loop guard | 未给时读 settings | `args.rs:456-457`；`app.rs:1875-1880` |
| `--loop-guard-stop` | 无 | loop guard 命中即硬停（`stop_on_detection`） | 未给时读 settings | `args.rs:458-459`；`app.rs:1881-1885` |

### 2.3 解析边界（写文档时避免错误承诺）

- `--name` / `-n` 缺值会报 `--name requires a value`；它会把下一项（即使以 `-` 开头）当作值消费
  （`args.rs:391-397`，`args.rs:315-321` 注释）。
- `--print` 只把“非 `@` 开头且（非 `-` 开头或以 `---` 开头）”的下一项当作消息
  （`args.rs:555-565`）。
- 未知长选项不会立即报错，而是作为扩展 flag 暂存；未知短选项直接报 `Unknown option`
  （`args.rs:666-696`）。
- `--mode`、`--use-theme`、`--tui-mode` 缺值时直接报错且不消费下一项
  （`args.rs:351-368,597-647`）。

---

## 3. Slash 命令表

### 3.1 内建斜杠命令（24 项，顺序即 UI 顺序）

事实来源：`crates/kiana-core/src/slash_commands.rs:110-156`；分派：`crates/kiana-interactive/src/interactive_mode/commands/mod.rs:874-1122`。
这些命令是自动补全与扩展命令重名冲突诊断的基表（`crates/kiana-interactive/src/interactive_mode.rs:2316,2465-2468`）；TUI 面板命令（`/permissions`、`/tasks`、`/mcp`）由独立 `PANEL_COMMANDS` 注册表追加到自动补全与诊断，不改变这 24 项冻结表。

| # | 命令 | 描述（逐字） | 参数提示 | 分派位置 |
| --- | --- | --- | --- | --- |
| 1 | `/settings` | Open settings menu | — | `commands/mod.rs:882-886` |
| 2 | `/model` | Select model (opens selector UI) | `<provider/model>` | `commands/mod.rs:895-910` |
| 3 | `/tree` | Navigate session tree (switch branches) | — | `commands/mod.rs:1017-1021` |
| 4 | `/thinking` | Set thinking level | `<level>` | `commands/mod.rs:911-920` |
| 5 | `/scoped-models` | Enable/disable models for Ctrl+P cycling | — | `commands/mod.rs:887-894` |
| 6 | `/export` | Export session (HTML default, or specify path: .html/.jsonl) | — | `commands/mod.rs:921-932` |
| 7 | `/import` | Import and resume a session from a JSONL file | — | `commands/mod.rs:933-944` |
| 8 | `/share` | Share session via a Radius artifact or a GitHub gist | — | `commands/mod.rs:945-955` |
| 9 | `/bug` | Report a bug to the Kiana developers | `<description>` | `commands/mod.rs:956-970` |
| 10 | `/copy` | Copy last agent message to clipboard | — | `commands/mod.rs:971-981` |
| 11 | `/name` | Set session display name | — | `commands/mod.rs:982-986` |
| 12 | `/session` | Show session info and stats | — | `commands/mod.rs:987-991` |
| 13 | `/changelog` | Show changelog entries | — | `commands/mod.rs:992-996` |
| 14 | `/hotkeys` | Show all keyboard shortcuts | — | `commands/mod.rs:997-1001` |
| 15 | `/fork` | Create a new fork from a previous user message | — | `commands/mod.rs:1002-1006` |
| 16 | `/clone` | Duplicate the current session at the current position | — | `commands/mod.rs:1007-1016` |
| 17 | `/trust` | Save project trust decision for future sessions | — | `commands/mod.rs:1022-1026` |
| 18 | `/login` | Configure provider authentication | `<provider>` | `commands/mod.rs:1027-1043` |
| 19 | `/logout` | Remove provider authentication | — | `commands/mod.rs:1044-1055` |
| 20 | `/new` | Start a new session | — | `commands/mod.rs:1056-1065` |
| 21 | `/compact` | Manually compact the session context | — | `commands/mod.rs:1066-1082` |
| 22 | `/resume` | Resume a different session | — | `commands/mod.rs:1108-1112` |
| 23 | `/reload` | Reload keybindings, extensions, skills, prompts, themes, and context files | — | `commands/mod.rs:1083-1092` |
| 24 | `/quit` | Quit `kiana`（描述在运行期拼接 APP_NAME） | — | `commands/mod.rs:1113-1122`；`slash_commands.rs:154-155` |

`/quit` 描述逐字为 `Quit kiana`（`APP_NAME` 当前为 `kiana`）。

### 3.2 不在 24 项表、但用户可触发的命令

| 命令 | 状态 | 说明 | 来源 |
| --- | --- | --- | --- |
| `/permissions` | `[VERIFIED]` | TUI 权限面板：Allow/Ask/Deny/Workspace，规则增删、模式切换、工作区目录管理；`--print` 文本模式输出权限文本 | `crates/kiana-interactive/src/interactive_mode/commands/panels.rs`；`crates/kiana-session/src/panel_commands.rs` |
| `/tasks` | `[VERIFIED]` | TUI 任务面板：后台任务列表、详情、刷新、取消；`--print` 文本模式输出任务 dump | 同上 |
| `/debug` | `[INTERNAL]` | 调试命令；在分派表中，但**不在** 24 项内建表，也不进自动补全 | `commands/mod.rs:1093-1096`；`commands/misc.rs:328` |
| `/mcp` | `[VERIFIED]` | 无参数时由 TUI 面板接管；带 login/logout/reconnect 等子命令时由内建 MCP 扩展处理 | `crates/kiana-builtin-extensions/src/mcp/index.rs:4227`；`commands/panels.rs` |
| `/llama` | `[VERIFIED]` | 由内建 `llama.cpp` 扩展注册；仅交互模式可用（非 TUI 模式会提示） | `crates/kiana-builtin-extensions/src/llama/index.rs:652`；`crates/kiana-builtin-extensions/src/index.rs:73-79` |
| `/skill:<name> [args]` | `[VERIFIED]` | 技能命令；校验 `user-invocable` 后激活，`allowed-tools` 生效 | `crates/kiana-session/src/agent_session.rs:7929-7934,10782`；注册见 `crates/kiana-interactive/src/interactive_mode.rs:2498-2510` |
| `/skill:deactivate <name>` / `/skill:off <name>` | `[VERIFIED]` | 显式停用技能 | `crates/kiana-session/src/agent_session.rs:7933-7934` |
| `/skill:<name>` 自动补全 | `[PARTIAL]` | 只有 `enableSkillCommands` 为真时注册技能命令（settings 默认 true） | `interactive_mode.rs:2492-2510`；`settings/defaults.rs:201` |
| `/<template-name> [args]` | `[VERIFIED]` | prompt template 展开；名字精确、大小写敏感，未命中时原样返回 | `crates/kiana-resources/src/prompts/expand.rs:39-79`；命令注册 `agent_session.rs:10769-10776` |

扩展命令的来源种类：`extension`、`prompt`、`skill`（`crates/kiana-core/src/slash_commands.rs:39-50`）。
`/llama` 与 `/mcp` **不在** 24 项内建表（`slash_commands.rs:16,24-26`）。

### 3.3 明确排除的彩蛋

- `/arminsayshi`：分派于 `commands/mod.rs:1098-1102`，不写入用户文档。
- `/dementedelves`：分派于 `commands/mod.rs:1103-1107`，不写入用户文档。

### 3.4 其他输入模式

- `!<command>`：执行 bash 命令。
- `!!<command>`：执行 bash 命令，并把该命令排除出上下文（`exclude_from_context`）。
  来源：`commands/mod.rs:1124-1153`。
- 没有发现 `/help` 斜杠命令的实现：24 项内建表与分派表都没有 `help`。因此不要写“在 TUI 输入
  `/help` 查看帮助”。CLI 帮助请使用 `kiana --help` 或 `kiana <command> --help`。
  （依据：`crates/kiana-core/src/slash_commands.rs:110-156`、`commands/mod.rs:874-1122` 中没有 `/help` 分支。）

---

## 4. 环境变量表

### 4.1 `--help` 的 Environment Variables 段（KIANA_* 用户变量）

来源：`kiana --help`（`crates/kiana-coding-agent/src/cli/args.rs:870-921`）。

| 变量 | 作用 | 默认 / 取值 | 来源 |
| --- | --- | --- | --- |
| `KIANA_CODING_AGENT_DIR` | 配置目录 | 默认 `~/.kiana/agent`；非空时经 `~` 展开后使用；空串视为未设置 | `--help`；`crates/kiana-core/src/config.rs:1632-1647` |
| `KIANA_CODING_AGENT_SESSION_DIR` | 会话存储目录 | 被 `--session-dir` 覆盖；空串视为未设置 | `--help`；`crates/kiana-coding-agent/src/app.rs:447-463` |
| `KIANA_API_BASE_URL` | Kiana 自建服务根地址 | 无默认；未设置时相关网络功能不可用；末尾 `/` 会被去掉 | `--help`；`crates/kiana-core/src/config.rs:1574-1583` |
| `KIANA_INSTALLER_API_BASE` | 托管更新 release API | 默认 `KIANA_API_BASE_URL + /api/installer/releases` | `--help`；`crates/kiana-coding-agent/src/package_manager_cli.rs:220-232` |
| `KIANA_PACKAGE_DIR` | 覆盖 package 目录（Nix/Guix store 等） | 未设置时由可执行文件位置推断 | `--help`；`crates/kiana-core/src/config.rs:503-508` |
| `KIANA_OFFLINE` | 关闭启动期网络操作 | `1` / `true` / `yes`（大小写不敏感；不 trim）；空串/其他值为 false | `--help`；`app.rs:231-240`；`crates/kiana-resources/src/package/mod.rs:117-130` |
| `KIANA_TELEMETRY` | 覆盖安装遥测 | 只要已定义（且为合法 UTF-8）就覆盖 settings；`1`/`true`/`yes` 为开，空串或其他值为关 | `--help`；`crates/kiana-config/src/defaults.rs:127-161` |
| `KIANA_SHARE_VIEWER_URL` | `/share` 查看器链接的 base URL | 优先使用；否则 `KIANA_API_BASE_URL/session/`；都没有则不可用 | `--help`；`crates/kiana-core/src/config.rs:1604-1614` |

`KIANA_API_BASE_URL` 的派生端点（README 已列）：模型目录 `/api/models/...`、版本检查
`/api/latest-version`、安装遥测 `/api/report-install`、MCP CIMD `/oauth/...`、分享查看器
`/session/`、托管安装器 `/api/installer/releases`（`README.md` 的“网络端点与品牌隔离”一节）。

### 4.2 Provider API key 环境变量（`--help` 原文分组）

以下名称与说明逐字来自 `--help`（`args.rs:870-913`）。具体 provider 的鉴权优先级没有在本表之外
逐项验证，写 provider 专章时需按 `spec/ai-auth.md` 复核。

| 变量 | 说明 |
| --- | --- |
| `ANTHROPIC_AUTH_TOKEN` | Anthropic bearer auth token |
| `ANTHROPIC_API_KEY` | Anthropic Claude API key |
| `ANTHROPIC_OAUTH_TOKEN` | Anthropic OAuth token（API key 的替代） |
| `ANT_LING_API_KEY` | Ant Ling API key |
| `OPENAI_API_KEY` | OpenAI GPT API key |
| `AZURE_OPENAI_API_KEY` | Azure OpenAI API key |
| `AZURE_OPENAI_BASE_URL` | Azure OpenAI/Cognitive Services base URL（如 `https://{resource}.openai.azure.com`） |
| `AZURE_OPENAI_RESOURCE_NAME` | Azure OpenAI resource name（base URL 的替代） |
| `AZURE_OPENAI_API_VERSION` | Azure OpenAI API version（默认 `v1`） |
| `AZURE_OPENAI_DEPLOYMENT_NAME_MAP` | Azure OpenAI `model=deployment` 映射（逗号分隔） |
| `DEEPSEEK_API_KEY` | DeepSeek API key |
| `NVIDIA_API_KEY` | NVIDIA NIM API key |
| `GEMINI_API_KEY` | Google Gemini API key |
| `GROQ_API_KEY` | Groq API key |
| `CEREBRAS_API_KEY` | Cerebras API key |
| `XAI_API_KEY` | xAI Grok API key |
| `FIREWORKS_API_KEY` | Fireworks API key |
| `TOGETHER_API_KEY` | Together AI API key |
| `BASETEN_API_KEY` | Baseten API key |
| `OPENROUTER_API_KEY` | OpenRouter API key |
| `AI_GATEWAY_API_KEY` | Vercel AI Gateway API key |
| `ZAI_API_KEY` | ZAI Coding Plan API key（Global） |
| `ZAI_CODING_CN_API_KEY` | ZAI Coding Plan API key（China） |
| `MISTRAL_API_KEY` | Mistral API key |
| `MINIMAX_API_KEY` | MiniMax API key |
| `MOONSHOT_API_KEY` | Moonshot AI API key |
| `OPENCODE_API_KEY` | OpenCode Zen/OpenCode Go API key |
| `KIMI_API_KEY` | Kimi For Coding API key |
| `META_API_KEY` | Meta Model API key |
| `CLOUDFLARE_API_KEY` | Cloudflare API token（Workers AI 与 AI Gateway） |
| `CLOUDFLARE_ACCOUNT_ID` | Cloudflare account id（两者都需要） |
| `CLOUDFLARE_GATEWAY_ID` | Cloudflare AI Gateway slug（AI Gateway 需要） |
| `QWEN_TOKEN_PLAN_API_KEY` | Qwen Token Plan API key（国际区） |
| `QWEN_TOKEN_PLAN_CN_API_KEY` | Qwen Token Plan API key（中国区） |
| `XIAOMI_API_KEY` | Xiaomi MiMo API key（api.xiaomimimo.com 计费） |
| `XIAOMI_TOKEN_PLAN_CN_API_KEY` | Xiaomi MiMo Token Plan API key（中国区） |
| `XIAOMI_TOKEN_PLAN_AMS_API_KEY` | Xiaomi MiMo Token Plan API key（Amsterdam 区） |
| `XIAOMI_TOKEN_PLAN_SGP_API_KEY` | Xiaomi MiMo Token Plan API key（Singapore 区） |
| `AWS_PROFILE` | Amazon Bedrock AWS profile |
| `AWS_ACCESS_KEY_ID` | Amazon Bedrock access key |
| `AWS_SECRET_ACCESS_KEY` | Amazon Bedrock secret key |
| `AWS_BEARER_TOKEN_BEDROCK` | Bedrock API key（bearer token） |
| `AWS_REGION` | Amazon Bedrock region（如 `us-east-1`） |

### 4.3 代码中存在、但不在 `--help` Environment Variables 段的 KIANA_* 变量

| 变量 | 作用与取值 | 状态 | 来源 |
| --- | --- | --- | --- |
| `KIANA_LANG` | UI locale 检测；优先级 `KIANA_LANG` > `LC_ALL` > `LC_MESSAGES` > `LANG`；空值跳过 | `[VERIFIED]` | `crates/kiana-interactive/src/render_core/i18n.rs:860-869` |
| `KIANA_OUTPUT_LANGUAGE` | 模型回答语言；显式配置 > 该环境变量 > settings；与 UI locale 分离 | `[VERIFIED]` | `crates/kiana-session/src/agent_session.rs:2328-2336` |
| `KIANA_EXPERIMENTAL` | 实验特性开关；只认字符串 `"1"`（`"0"`/`""`/`"true"` 均为 false，大小写敏感） | `[INTERNAL]` | `crates/kiana-config/src/defaults.rs:75-80` |
| `KIANA_MEMORY` | `1`/`true`/`yes`/`on` 启用持久记忆（trim、大小写不敏感）；默认关闭 | `[VERIFIED]` | `crates/kiana-session/src/memory_wiring.rs:77-82,99-107` |
| `KIANA_MEMORY_AUTO_EXTRACT` | 同上，打开自动抽取；默认关闭 | `[VERIFIED]` | `crates/kiana-session/src/memory_wiring.rs:80-82` |
| `KIANA_PERMISSIONS` | 权限模式：`read-only`/`default`/`accept-edits`/`bypass`/`off`；空值按未设置；未知值按 `default`（fail-closed） | `[VERIFIED]` | `crates/kiana-permissions/src/policy.rs:899-930`；`crates/kiana-permissions/src/lib.rs:66` |
| `KIANA_PERMISSION_MODE` | `KIANA_PERMISSIONS` 的别名；两者都设置时按数组顺序先命中者优先 | `[VERIFIED]` | `crates/kiana-permissions/src/policy.rs:899-910` |
| `KIANA_SANDBOX` | 后端选择：`none`/`off`/`disabled`/`false` 关闭；`bwrap`/`bubblewrap`、`landlock` 显式选择；其它/未设置自动探测 | `[VERIFIED]` | `crates/kiana-sandbox/src/lib.rs:176-215`；`crates/kiana-sandbox/src/execution_policy.rs:496-505` |
| `KIANA_SANDBOX_POLICY` | 沙箱策略：`read-only`/`workspace-write`/`danger-full-access`；空值=不启用；无法识别=拒绝所有执行（fail-closed） | `[VERIFIED]` | `crates/kiana-sandbox/src/execution_policy.rs:271-300` |
| `KIANA_BWRAP` | 显式指定 `bwrap` 可执行文件路径 | `[VERIFIED]` | `crates/kiana-sandbox/src/bwrap.rs:193-200` |
| `KIANA_IDE` | IDE 集成开关；默认启用；`0`/`false`/`no`/`off` 关闭 | `[VERIFIED]` | `crates/kiana-coding-agent/src/app.rs:2752-2760` |
| `KIANA_IDE_DIR` | IDE lockfile 目录覆盖；默认 `~/.kiana/ide` | `[VERIFIED]` | `crates/kiana-coding-agent/src/ide/startup.rs:75-85` |
| `KIANA_CRON` | cron 开关；settings 显式配置时以 settings 为准；否则 `1`/`true`/`yes`/`on` 启用，默认目录 `<agentDir>/cron`、30s tick | `[VERIFIED]` | `crates/kiana-coding-agent/src/app.rs:2588-2630` |
| `KIANA_CONTINUITY` | `1`/`true`/`yes`/`on` 启用 continuity；默认关闭 | `[VERIFIED]` | `crates/kiana-session/src/continuity.rs:77-105` |
| `KIANA_CONTINUITY_DIR` | continuity 状态目录；默认 `<agentDir>/continuity` | `[VERIFIED]` | `crates/kiana-session/src/continuity.rs:84-92` |
| `KIANA_CONTINUITY_EXECUTION` | 单独关闭 execution 子功能（`0`/`false`/`off`） | `[VERIFIED]` | `crates/kiana-session/src/continuity.rs:95-99` |
| `KIANA_CONTINUITY_CHECKPOINTS` | 单独关闭 checkpoint 子功能 | `[VERIFIED]` | `crates/kiana-session/src/continuity.rs:95-99` |
| `KIANA_CONTINUITY_BACKGROUND` | 单独关闭 background 子功能 | `[VERIFIED]` | `crates/kiana-session/src/continuity.rs:95-99` |
| `KIANA_CONTINUITY_ACCEPTANCE` | 单独关闭 acceptance 子功能 | `[VERIFIED]` | `crates/kiana-session/src/continuity.rs:95-99` |
| `KIANA_CODEMODE_MAX_WORKERS` | codemode 最大 worker 数；正整数（JS `Number()` 语义），否则默认 2 | `[VERIFIED]` | `crates/kiana-builtin-extensions/src/codemode/execute.rs:113,267-276` |
| `KIANA_CLEAR_ON_SHRINK` | `terminal.clearOnShrink` 的环境回退；只认 `"1"`；settings 优先 | `[VERIFIED]` | `crates/kiana-config/src/settings/mod.rs:2750-2760` |
| `KIANA_HARDWARE_CURSOR` | `showHardwareCursor` 的环境回退；只认 `"1"`；settings 优先 | `[VERIFIED]` | `crates/kiana-config/src/settings/mod.rs:3088-3096` |
| `KIANA_HYPERLINKS` | `"1"`/`"0"` 强制终端超链接能力 | `[VERIFIED]` | `crates/kiana-tui/src/terminal_image.rs:1177-1191` |
| `KIANA_IMAGE_PROTOCOL` | `kitty`/`iterm2` 强制图片协议；`none`/`0` 关闭图片 | `[VERIFIED]` | `crates/kiana-tui/src/terminal_image.rs:1178,1193-1198` |
| `KIANA_TRUE_COLOR` | `"1"`/`"0"` 强制真彩 | `[VERIFIED]` | `crates/kiana-tui/src/terminal_image.rs:1179,1201` |
| `KIANA_PROGRAM_STATUS` | `"1"` 视为支持程序状态并跳过查询；`"0"` 视为不支持并跳过查询；其它/未设置会发查询等待回复 | `[INTERNAL]` | `crates/kiana-tui/src/terminal.rs:941-958` |
| `KIANA_NEXT_RENDER_CORE` | 实验渲染核心开关：`1`/`true`/`yes`/`on` 开启（默认关）；但乐观用户消息队列默认开启，只有 `0`/`false`/`no`/`off` 关闭 | `[PARTIAL]` | `crates/kiana-interactive/src/render_core/mod.rs:44-73`；详见 §8.1 |
| `KIANA_OAUTH_CALLBACK_HOST` | OAuth 回调监听 host；默认 `127.0.0.1` | `[VERIFIED]` | `crates/kiana-ai/src/auth/oauth/mod.rs:790-799` |
| `KIANA_RADIUS_GATEWAY` | Radius 网关地址覆盖；每次调用重新读取；默认 `https://radius.kiana.dev` | `[VERIFIED]` | `crates/kiana-config/src/defaults.rs:93-115` |
| `KIANA_MANAGED_INSTALL_ROOT` | 托管安装根目录；需满足 `releases` 布局与 `managed-install.json` 标记 | `[INTERNAL]` | `crates/kiana-coding-agent/src/package_manager_cli.rs:390-420` |
| `KIANA_QUICKJS_WASM` | 覆盖 QuickJS wasm 路径；首次结果永久缓存 | `[INTERNAL]` | `crates/kiana-core/src/config.rs:1352-1370` |
| `KIANA_TIMING` | 启动计时；只认 `"1"`，首次求值后缓存 | `[INTERNAL]` | `crates/kiana-core/src/timings.rs:83-86` |
| `KIANA_STARTUP_BENCHMARK` | 启动基准；真值开启，只支持交互模式，否则退出 1 | `[INTERNAL]` | `crates/kiana-coding-agent/src/app.rs:735-740` |
| `KIANA_TUI_DEBUG` | TUI 调试日志；`"1"` 时写 `/tmp/tui/render-*.log` | `[INTERNAL]` | `crates/kiana-tui/src/main_screen.rs:1321,1634` |
| `KIANA_TUI_DEBUG_REDRAW` | `"1"` 时写 redraw 调试日志 | `[INTERNAL]` | `crates/kiana-tui/src/main_screen.rs:825-827` |
| `KIANA_TUI_WRITE_LOG` | 终端写日志：值为已存在目录时生成 `tui-<时间戳>-<pid>.log`，否则当作文件路径 | `[INTERNAL]` | `crates/kiana-tui/src/terminal.rs:1924-1945` |
| `KIANA_TUI_ESC_TIMEOUT` | Escape 超时毫秒数；有限且 `>0` 时使用；否则 SSH 100ms、其他 10ms | `[INTERNAL]` | `crates/kiana-tui/src/terminal.rs:300-315` |
| `KIANA_SKIP_VERSION_CHECK` | 任意非空值跳过版本检查；offline 模式会自动设置 | `[INTERNAL]` | `crates/kiana-core/src/changelog.rs:971-992`；`app.rs:239` |

### 4.4 启动时注入给子进程的变量

| 变量 | 注入位置 / 值 | 来源 |
| --- | --- | --- |
| `KIANA_CODING_AGENT` | 启动时设为 `"true"` | `crates/kiana-coding-agent/src/cli/setup.rs:754` |
| `AI_AGENT` | 启动时设为 `"kiana"` | `crates/kiana-coding-agent/src/cli/setup.rs:755` |
| `KIANA_SESSION_ID` | 注入 bash 子进程（`expose_session_environment` 且上下文有效时） | `crates/kiana-tools/src/bash.rs:1015-1045` |
| `KIANA_SESSION_FILE` | 同上；仅当会话文件非空 | `crates/kiana-tools/src/bash.rs:1015-1045` |
| `KIANA_PROVIDER` | 同上；当前模型存在时 | `crates/kiana-tools/src/bash.rs:1015-1045` |
| `KIANA_MODEL` | 同上；当前模型 ID | `crates/kiana-tools/src/bash.rs:1015-1045` |
| `KIANA_REASONING_LEVEL` | 同上；当前 thinking level 存在时 | `crates/kiana-tools/src/bash.rs:1015-1045` |
| `KIANA_PROJECT_DIR` | 注入 command hook 子进程 | `crates/kiana-hooks/src/command.rs:145-165`；`crates/kiana-hooks/src/lib.rs:105-110` |
| `KIANA_HOOK_EVENT` | 注入 command hook 子进程 | 同上 |
| `KIANA_HOOK_SOURCE` | 注入 command hook 子进程 | 同上 |
| `KIANA_HOOK_DEPTH` | 注入 command hook 子进程（当前深度 + 1） | 同上 |

### 4.5 测试/构建专用（不得写入用户文档）

以下前缀/名称只用于测试、构建或故障注入，`[INTERNAL]`：
`KIANA_TEST_*`、`KIANA_FAKE_*`、`KIANA_EVAL_*`、`KIANA_BENCH_*`、`KIANA_*_CHILD`、
`KIANA_AI_TEST_*`、`KIANA_ENV_*`、`KIANA_BASH_PREPARE_*`、`KIANA_TUI_*_CHILD`、
`KIANA_RPC_CLIENT_TEST_VAR`、`KIANA_STORAGE_MEMORY_*`、`KIANA_STARTUP_TUI_FATAL_CHILD` 等。
`KIANA_MODEL_DATA_DIR` 只影响 `kiana-ai` 的构建脚本（`crates/kiana-ai/build.rs:284-295`）。

注意：`KIANA_ATTRIBUTION` 与 `KIANA_AI_VERSION` 在本仓库是**常量**，不是环境变量：
`crates/kiana-ai/src/utils/identity.rs:90-95`、`crates/kiana-ai/src/index.rs:106,176`。
`KIANA_USER_AGENT_RE` 也是测试用正则名，不是环境变量（`crates/kiana-ai/src/model_catalog.rs:3599-3619`）。

---

## 5. Settings 键表

### 5.1 顶层键（运行期 `Settings`，58 项）

事实来源：`crates/kiana-config/src/settings/schema.rs:893-961`（声明顺序与 JSON 名逐字）；
`Settings::known_keys()` 由宏生成（`schema.rs:196-205`）。测试锁定 58 项：
`crates/kiana-config/tests/config_schemas.rs:707-781`。

默认值列只写 `SETTINGS_DEFAULTS` 中明确存在的项（`crates/kiana-config/src/settings/defaults.rs:171-235`）；
“—”表示该键没有可确认的全局默认值，由 getter 或调用方决定，用户文档不要擅自补默认。

| # | JSON 键 | 类型 | 已知默认 | 来源 |
| --- | --- | --- | --- | --- |
| 1 | `$schema` | string | — | `schema.rs:903` |
| 2 | `lastChangelogVersion` | string | — | `schema.rs:904` |
| 3 | `defaultProvider` | string | — | `schema.rs:905` |
| 4 | `defaultModel` | string | — | `schema.rs:906` |
| 5 | `defaultThinkingLevel` | `off`/`minimal`/`low`/`medium`/`high`/`xhigh`/`max` | — | `schema.rs:907` |
| 6 | `modelThinkingLevels` | record<string, 同上枚举> | — | `schema.rs:908` |
| 7 | `transport` | `auto`/`sse`/`websocket`/`websocket-cached` | `auto` | `schema.rs:909`；`defaults.rs:172` |
| 8 | `sessionShareDestination` | `auto`/`gist`/`radius` | `auto` | `schema.rs:910`；`defaults.rs:173` |
| 9 | `steeringMode` | `all`/`one-at-a-time` | `one-at-a-time` | `schema.rs:911`；`defaults.rs:174` |
| 10 | `followUpMode` | `all`/`one-at-a-time` | `one-at-a-time` | `schema.rs:912`；`defaults.rs:175` |
| 11 | `theme` | string | — | `schema.rs:913` |
| 12 | `compaction` | object（见 §5.2） | enabled true / reserveTokens 16384 / keepRecentTokens 20000 | `schema.rs:914`；`defaults.rs:176-180` |
| 13 | `branchSummary` | object（见 §5.2） | reserveTokens 16384 / skipPrompt false | `schema.rs:915`；`defaults.rs:181-184` |
| 14 | `retry` | object（见 §5.2） | enabled true / maxRetries 3 / baseDelayMs 2000 / provider.maxRetryDelayMs 60000 | `schema.rs:916`；`defaults.rs:185-193` |
| 15 | `hideThinkingBlock` | bool | false | `schema.rs:917`；`defaults.rs:194` |
| 16 | `showCacheMissNotices` | bool | false | `schema.rs:918`；`defaults.rs:195` |
| 17 | `externalEditor` | string | — | `schema.rs:919` |
| 18 | `shellPath` | string | — | `schema.rs:920` |
| 19 | `quietStartup` | bool 或 `"header"` | `false` | `schema.rs:921`；`defaults.rs:196` |
| 20 | `defaultProjectTrust` | `ask`/`always`/`never` | `ask` | `schema.rs:922`；`defaults.rs:197` |
| 21 | `shellCommandPrefix` | string | — | `schema.rs:923` |
| 22 | `npmCommand` | string[] | — | `schema.rs:924` |
| 23 | `collapseChangelog` | bool | false | `schema.rs:925`；`defaults.rs:198` |
| 24 | `enableInstallTelemetry` | bool | true | `schema.rs:926`；`defaults.rs:199` |
| 25 | `enableAnalytics` | bool | false | `schema.rs:927`；`defaults.rs:200` |
| 26 | `trackingId` | string | — | `schema.rs:928` |
| 27 | `packages` | `(string 或 package 对象)[]` | — | `schema.rs:929` |
| 28 | `extensions` | string[] | — | `schema.rs:930` |
| 29 | `deviceId` | string | — | `schema.rs:931` |
| 30 | `skills` | string[]（运行期） | — | `schema.rs:932` |
| 31 | `prompts` | string[] | — | `schema.rs:933` |
| 32 | `themes` | string[] | — | `schema.rs:934` |
| 33 | `enableSkillCommands` | bool | true | `schema.rs:935`；`defaults.rs:201` |
| 34 | `terminal` | object（见 §5.2） | showImages true / imageWidthCells 60 / clearOnShrink false / showTerminalProgress false | `schema.rs:936`；`defaults.rs:202-207` |
| 35 | `images` | object（见 §5.2） | autoResize true / blockImages false | `schema.rs:937`；`defaults.rs:208-211` |
| 36 | `ui` | object（见 §5.2） | — | `schema.rs:938` |
| 37 | `enabledModels` | string[] | — | `schema.rs:939` |
| 38 | `defaultTools` | string[] | — | `schema.rs:940` |
| 39 | `doubleEscapeAction` | `fork`/`tree`/`none` | `tree` | `schema.rs:941`；`defaults.rs:212` |
| 40 | `treeFilterMode` | `default`/`no-tools`/`user-only`/`labeled-only`/`all` | `default` | `schema.rs:942`；`defaults.rs:213` |
| 41 | `thinkingBudgets` | object（见 §5.2） | — | `schema.rs:943` |
| 42 | `editorPaddingX` | number | `0` | `schema.rs:944`；`defaults.rs:214` |
| 43 | `outputPad` | number | `1` | `schema.rs:945`；`defaults.rs:215` |
| 44 | `autocompleteMaxVisible` | number | `5` | `schema.rs:946`；`defaults.rs:216` |
| 45 | `showHardwareCursor` | bool | false（`KIANA_HARDWARE_CURSOR=1` 可回退为 true） | `schema.rs:947`；`settings/mod.rs:3088-3096` |
| 46 | `markdown` | object（见 §5.2） | codeBlockIndent 两个空格 / mermaid `streaming` | `schema.rs:948`；`defaults.rs:217-221` |
| 47 | `warnings` | object（见 §5.2） | anthropicExtraUsage true | `schema.rs:949`；`defaults.rs:222-224` |
| 48 | `codemode` | object（见 §5.2） | mode `on` / inlineBudget 3000 | `schema.rs:950`；`defaults.rs:225-228` |
| 49 | `sessionDir` | string | — | `schema.rs:951` |
| 50 | `httpProxy` | string | — | `schema.rs:952` |
| 51 | `httpIdleTimeoutMs` | number 或 `"disabled"` | — | `schema.rs:953` |
| 52 | `cacheWarming` | `off`/`streaming`/`idle` | `streaming` | `schema.rs:954`；`defaults.rs:229` |
| 53 | `websocketConnectTimeoutMs` | number 或 `"disabled"` | — | `schema.rs:955` |
| 54 | `tuiMode` | `regular`/`fullscreen` | `fullscreen` | `schema.rs:956`；`defaults.rs:230` |
| 55 | `fullscreenExitOutput` | `transcript`/`resume-hint` | `transcript` | `schema.rs:957`；`defaults.rs:231` |
| 56 | `fullscreenScrollbar` | `auto`/`always`/`hidden` | `auto` | `schema.rs:958`；`defaults.rs:232` |
| 57 | `fullscreenCopyOnSelect` | bool | true | `schema.rs:959`；`defaults.rs:233` |
| 58 | `fullscreenWheelScrollLines` | number 或 `"auto"` | `auto` | `schema.rs:960`；`defaults.rs:234` |

### 5.2 常用嵌套键

来源：同一 schema 文件；只列 schema 明确声明的字段。

| 父键 | 嵌套键 | 类型 | 已知默认 / 说明 | 来源 |
| --- | --- | --- | --- | --- |
| `compaction` | `enabled` | bool | true | `schema.rs:621-629`；`defaults.rs:176-180` |
| `compaction` | `reserveTokens` | number | 16384 | 同上 |
| `compaction` | `keepRecentTokens` | number | 20000 | 同上 |
| `compaction` | `modelOverrides` | record<`provider/id`, object> | 键为精确 `${provider}/${id}`，不做 glob | `schema.rs:607-617,629` |
| `compaction.modelOverrides[]` | `reserveTokens` / `keepRecentTokens` | number | 缺省表示沿用父级 | `schema.rs:607-611` |
| `branchSummary` | `reserveTokens` | number | 16384 | `schema.rs:652-656`；`defaults.rs:181-184` |
| `branchSummary` | `skipPrompt` | bool | false | 同上 |
| `retry` | `enabled` | bool | true | `schema.rs:672-680`；`defaults.rs:185-193` |
| `retry` | `maxRetries` | number | 3 | 同上 |
| `retry` | `baseDelayMs` | number | 2000 | 同上 |
| `retry` | `maxAgentDelayMs` | number | 取 `DEFAULT_MAX_AGENT_RETRY_DELAY_MS` | `defaults.rs:189` |
| `retry` | `provider` | object | — | `schema.rs:660-670,680` |
| `retry.provider` | `timeoutMs` | number | — | `schema.rs:660-665` |
| `retry.provider` | `maxRetries` | number | — | 同上 |
| `retry.provider` | `maxRetryDelayMs` | number | 60000 | `defaults.rs:190-192` |
| `terminal` | `showImages` | bool | true | `schema.rs:700-708`；`defaults.rs:202-207` |
| `terminal` | `imageWidthCells` | number | 60 | 同上 |
| `terminal` | `clearOnShrink` | bool | false；`KIANA_CLEAR_ON_SHRINK=1` 是回退 | 同上；`settings/mod.rs:2750-2760` |
| `terminal` | `showTerminalProgress` | bool | false | 同上 |
| `terminal` | `hyperlinks` | bool 或 `"auto"` | — | `schema.rs:705,450-490` |
| `terminal` | `images` | `kitty`/`iterm2`/`auto`/`false` | —（`true` 非法） | `schema.rs:706,489-520` |
| `terminal` | `trueColor` | bool 或 `"auto"` | — | `schema.rs:707,450-490` |
| `images` | `autoResize` | bool | true | `schema.rs:713-716`；`defaults.rs:208-211` |
| `images` | `blockImages` | bool | false | 同上 |
| `ui` | `locale` | string | —（缺失时回退旧键 `language`，再回退 `"en"`） | `schema.rs:725-727`；`settings/mod.rs:1674-1696` |
| `ui` | `outputLanguage` / `output_language` | string | 仅通过 raw 读取，不在 schema | `agent_session.rs:2292-2294` |
| `thinkingBudgets` | `minimal` / `low` / `medium` / `high` | number | — | `schema.rs:732-737` |
| `markdown` | `codeBlockIndent` | string | 恰好两个空格 | `schema.rs:745-748`；`defaults.rs:217-221` |
| `markdown` | `mermaid` | `off`/`final`/`streaming` | `streaming` | 同上 |
| `warnings` | `anthropicExtraUsage` | bool | true | `schema.rs:753-755`；`defaults.rs:222-224` |
| `codemode` | `mode` | `on`/`only` | `on` | `schema.rs:760-763`；`defaults.rs:225-228` |
| `codemode` | `inlineBudget` | number | 3000 | 同上 |
| `packages[]` | `source` | string（对象形式必填） | — | `schema.rs:772-779` |
| `packages[]` | `autoload` | bool | — | 同上 |
| `packages[]` | `extensions` / `skills` / `prompts` / `themes` | string[] | — | 同上 |

### 5.3 `SettingsInput` 独有/旧版形状（迁移前）

`SettingsInput` 有 60 个顶层键（`schema.rs:829-890`，测试 `config_schemas.rs:707-772`）。
`Settings` 去掉 `queueMode`、`websockets` 后为 58 项。迁移相关：

| 旧键/形状 | 处理 | 来源 |
| --- | --- | --- |
| `queueMode` | `SettingsInput` 保留；迁移在 `steeringMode` 缺失时把值写入 `steeringMode` 并删除 `queueMode`；`steeringMode` 已存在时不删除 | `schema.rs:888,897-898`；`crates/kiana-config/src/settings/migrations.rs:91-102` |
| `websockets` | `SettingsInput` 保留；迁移仅在 `transport` 缺失且 `websockets` 为布尔时写入 `transport` 并删除；否则保留 | `schema.rs:889,897-898`；`migrations.rs:105-121` |
| `retry.maxDelayMs` | 迁移写入 `retry.provider.maxRetryDelayMs`；运行期 `RetrySettings` 不含旧字段 | `schema.rs:686-697`；`defaults.rs:17-19` 注释 |
| `skills` 对象形式 `{ enableSkillCommands, customDirectories }` | 迁移为顶层 `enableSkillCommands` 与 `skills` 数组 | `schema.rs:782-790`；`schema.rs:899-900` 注释 |
| `language` | 旧 UI 语言键，不在 schema；`ui.locale` 缺失时回退读取 | `settings/mod.rs:1674-1696` |

### 5.4 schema 之外、但运行期会读取的顶层键

这些键不在 `Settings::known_keys()` 的 58 项中，但通过 `Settings::raw` / `extra` 被读取。
写作时必须说明“不是 schema 声明的类型化键”，不要把它们混进 §5.1。

| 键 | 形状 / 用途 | 来源 |
| --- | --- | --- |
| `permissions` | 权限对象；字段见 §5.5 | `crates/kiana-permissions/src/policy.rs:758-800`；`crates/kiana-coding-agent/src/app.rs:2443` |
| `hooks` | `{ EventName: [{ matcher, hooks: [...] }] }`；见 hooks 文档 | `crates/kiana-hooks/src/lib.rs:36-90`；`crates/kiana-session/src/agent_session.rs:2219-2220` |
| `cron` | bool 或 `{ enabled, dir, intervalSecs }` | `crates/kiana-coding-agent/src/app.rs:2588-2630` |
| `continuity` | bool 或 `{ enabled, dir, execution, checkpoints, background, acceptance }` | `crates/kiana-session/src/continuity.rs:117-148` |
| `loopGuard` | `{ enabled, stopOnDetection, softToolCallsPerTurn, hardToolCallMultiplier, globalWindow, thresholds }` | `crates/kiana-session/src/agent_session_services.rs:493-580` |
| `budget` | `{ maxTurns, maxTokens, maxWallClockMs, maxCostUsd, warnRatios }` | `crates/kiana-session/src/agent_session_services.rs:588-627` |
| `tools` | `tools.applyPatch.models` / `tools.apply_patch.models` | `crates/kiana-session/src/agent_session.rs:4814-4827` |
| `output_language` | 模型输出语言（raw 字符串） | `crates/kiana-session/src/agent_session.rs:2285-2296` |
| `skillActivationTtlSeconds` / `skillActivationTtl` | 技能激活 TTL（raw） | `crates/kiana-session/src/agent_session.rs:9875-9876` |
| `language` | 旧 UI 语言键 | `crates/kiana-config/src/settings/mod.rs:1691-1695` |

### 5.5 `permissions` 对象字段（schema 外）

来源：`crates/kiana-permissions/src/policy.rs:758-800`、`crates/kiana-config/src/settings/strict.rs:240-315`。

| 字段 | 类型 | 说明 |
| --- | --- | --- |
| `permissions.mode` / `permissions.defaultMode` | `read-only`/`default`/`accept-edits`/`bypass`/`off` | 未知值按 `default`（fail-closed）；`mode` 优先于 `defaultMode` |
| `permissions.rules` | 规则数组 | 支持 `{tool, decision, reason}`、`{command, decision, reason}`、`{path, decision, reason}`、`{allow: [...]}`/`{deny: [...]}`/`{ask: [...]}` 简写 |
| `permissions.allow` / `permissions.deny` / `permissions.ask` | 规则数组 | 简写形式 |
| `permissions.addDirs` / `permissions.workspaceRoots` / `permissions.additionalDirectories` | string[] | 额外可写根 |
| `permissions.enabledMcpjsonServers` / `permissions.disabledMcpjsonServers` | string[] | MCP 服务器开关 |
| `permissions.allowedMcpServers` / `allowed_mcp_servers` | array | managed allowlist；空表示全部拒绝 |
| `permissions.deniedMcpServers` / `denied_mcp_servers` | array | managed denylist；deny 永远合并，优先于 allow |
| `permissions.disableBypassPermissionsMode` / `disable_bypass_permissions_mode` | bool/disable flag | 禁止 bypass；managed 层强制 |
| `permissions.allowManagedPermissionRulesOnly` / `allow_managed_permission_rules_only` | bool/disable flag | 只保留 managed 的 allow/ask；deny 仍合并 |
| `permissions.allowManagedHooksOnly` / `allow_managed_hooks_only` | bool/disable flag | 只保留 managed hooks |

注意：`permissions` 不是 `Settings` schema 的 58 个 known key 之一；它通过原始 JSON 读取。
`settings.local.json` 不作为 `SettingsManager` 的类型化 Settings 键读取；项目受信时，
会话配置层会把整个文件按 local 配置层加载，其 `permissions`、`hooks` 等顶层键参与分层合并
（见 §6）。项目 settings 与 local 的 `permissions` 合并时，`rules`/`allow`/`deny`/`addDirs`
取并集，`mode` 以 local 为准（`crates/kiana-config/src/settings/permissions.rs:1-16`，
`crates/kiana-session/src/agent_session.rs:2044,2215`）。

### 5.6 配置分层与策略文件

`ConfigLayerSource` 顺序（低 → 高）：`Builtin`(0) / `System`(10) / `Managed`(20) /
`Enterprise`(30) / `User`(40) / `Project`(50) / `Local`(60) / `CliFlag`(70) / `Session`(80)。
来源：`crates/kiana-config/src/settings/layers.rs:97-160`。

策略键（可在顶层或 `permissions` 内，camelCase/snake_case 都接受）：
`allowManagedPermissionRulesOnly`、`allowManagedHooksOnly`、`disableBypassPermissionsMode`、
`allowedMcpServers`、`deniedMcpServers`。来源：`layers.rs:960-1060`。

---

## 6. 路径表

`<agentDir>` 默认为 `~/.kiana/agent`，可用 `KIANA_CODING_AGENT_DIR` 覆盖。
`<cwd>` 为当前项目目录；项目配置目录为 `<cwd>/.kiana`（`CONFIG_DIR_NAME = ".kiana"`，
`crates/kiana-core/src/config.rs:160`）。

| 用途 | 全局（用户） | 项目 | 项目本地 | 覆盖 / 说明 | 来源 |
| --- | --- | --- | --- | --- | --- |
| agent 目录 | `~/.kiana/agent` | — | — | `KIANA_CODING_AGENT_DIR` | `config.rs:1632-1647` |
| settings | `<agentDir>/settings.json` | `<cwd>/.kiana/settings.json` | `<cwd>/.kiana/settings.local.json` | 项目层需项目受信；local 作为 local 配置层参与 `permissions`、`hooks` 等合并（类型化 Settings 键不读取） | `crates/kiana-config/src/settings/storage.rs:92-100`；`crates/kiana-config/src/settings/permissions.rs:27-44`；`crates/kiana-session/src/agent_session.rs:2044,2215` |
| 会话 | `<agentDir>/sessions/--<cwd>--/<timestamp>_<session-id>.jsonl` | 同一全局根下按 cwd 分目录 | — | 优先级：`--session-dir` > `KIANA_CODING_AGENT_SESSION_DIR` > `settings.sessionDir` > 默认 | `crates/kiana-session-core/src/session_manager.rs:2547-2590,4510-4512`；`app.rs:445-463` |
| memory（用户） | `~/.kiana/memory` | — | — | `MEMORY.md`、`memory_summary.md`、`topics/`、`audit.jsonl`、`state/`、`.memory.lock` | `crates/kiana-memory/src/config.rs:1-25,203-260` |
| memory（项目） | — | `<cwd>/.kiana/memory` | — | 同上布局 | `crates/kiana-memory/src/config.rs:1-25,255-260` |
| MCP | `<agentDir>/mcp.json` | `<cwd>/.kiana/mcp.json` | — | 项目文件仅在项目受信时读取 | `crates/kiana-builtin-extensions/src/mcp/config.rs:50-52,556-582` |
| IDE lockfile | `~/.kiana/ide/<port>.lock` | — | — | `KIANA_IDE_DIR` 覆盖目录 | `crates/kiana-coding-agent/src/ide/lockfile.rs:1-20`；`ide/startup.rs:75-85` |
| i18n 用户语言包 | `<agentDir>/locales` | — | — | 内置资源在二进制内：`crates/kiana-interactive/assets/locales/en.json`、`zh.json` | `crates/kiana-interactive/src/interactive_mode.rs:1790-1796`；`render_core_host.rs:50-56`；`i18n.rs:45-47` |
| auth | `<agentDir>/auth.json` | — | — | — | `config.rs:1655-1657` |
| trust | `<agentDir>/trust.json` | — | — | 目录键 → true/false/null；锁文件为 `<trust.json>.lock` | `crates/kiana-config/src/trust/manager.rs:477-503` |
| models | `<agentDir>/models.json` | — | — | 动态目录缓存 `<agentDir>/models-store.json` | `config.rs:1650-1652`；`crates/kiana-config/src/auth/models_store.rs:89-97` |
| keybindings | `<agentDir>/keybindings.json` | — | — | 迁移写回在 `kiana-coding-agent` | `crates/kiana-config/src/keybindings/mod.rs:49`；`migrations.rs:641-651` |
| debug 日志 | `<agentDir>/kiana-debug.log` | — | — | 文件名由 `APP_NAME` 派生 | `config.rs:1688-1693` |
| skills | `<agentDir>/skills` | `<cwd>/.kiana/skills` | — | 显式 `--skill` 路径另外追加 | `crates/kiana-resources/src/skills/mod.rs:311-330` |
| prompts | `<agentDir>/prompts` | `<cwd>/.kiana/prompts` | — | `--prompt-template` 追加 | `crates/kiana-resources/src/prompts/mod.rs:274-285` |
| themes | `<agentDir>/themes` | `<cwd>/.kiana/themes` | — | `--theme` 追加；内置主题在包资源中 | `crates/kiana-core/src/config.rs:1645-1648`；`crates/kiana-resources/src/loader/themes.rs:77-85` |
| extensions | `<agentDir>/extensions` | `<cwd>/.kiana/extensions` | — | 发现顺序：项目 → 全局 → 配置路径；`--extension` 追加 | `crates/kiana-extensions/src/loader/mod.rs:9013-9071` |
| agents（子代理定义） | `<agentDir>/agents` | `<cwd>/.kiana/agents` | 系统 `/etc/kiana/agents` | 覆盖顺序 builtin < plugin < user < project < policy < flag | `crates/kiana-session/src/agent_definition/mod.rs:44-70` |
| tools / bin | `<agentDir>/tools`、`<agentDir>/bin` | — | — | 受管二进制迁移到 `bin/` | `config.rs:1665-1673`；`migrations.rs:700-720` |
| cron 状态 | `<agentDir>/cron` | — | — | settings `cron.dir` 或 env `KIANA_CRON` 可改 | `app.rs:2588-2594` |
| continuity 状态 | `<agentDir>/continuity` | — | — | `KIANA_CONTINUITY_DIR` 或 settings `continuity.dir` | `crates/kiana-session/src/continuity.rs:84-92,117-136` |
| package 临时目录 | `<agentDir>/tmp/extensions` | — | — | 包解析临时目录 | `crates/kiana-resources/src/package/paths.rs:1097` |
| 系统策略 | `/etc/kiana/settings.json`、`/etc/kiana/managed-settings.json`、`/etc/kiana/enterprise-settings.json` | — | — | Windows 使用 `%ProgramData%\kiana\...` | `crates/kiana-config/src/settings/layers.rs:151-190` |

废弃/易误写：`<agentDir>/hooks` 目录是旧布局，当前会触发“Hooks 已改名为 extensions”的警告，
不是 hooks 配置位置（`crates/kiana-coding-agent/src/migrations.rs:729-742`）。Hooks 配置在
settings 的 `hooks` 键（§5.4）。

---

## 7. 功能 → 实现 / 说明文档索引

### 7.1 主要功能

| 用户功能 | 主要实现 | 规范/说明文档 | 过程记录 |
| --- | --- | --- | --- |
| CLI 参数、帮助、启动流程 | `crates/kiana-coding-agent/src/cli/args.rs`、`app.rs` | `spec/ca-cli.md`；`docs/EXECUTION_FLOW.md` | `docs/START_HERE.md` |
| 交互式 TUI、编辑器、自动补全 | `crates/kiana-interactive`、`kiana-tui`、`kiana-ui-kit` | `spec/ca-interactive-mode.md`、`spec/ca-interactive-components.md`、`spec/tui-core.md`、`spec/tui-components.md`、`spec/tui-text.md` | `phase-12a` |
| 主题 | `crates/kiana-theme`、`kiana-interactive` | `spec/tui-*`；主题资源在 `kiana-theme/src/assets.rs` | — |
| 会话保存/恢复/fork/tree | `crates/kiana-session-core`、`crates/kiana-session` | `spec/ca-session.md` | `phase-11b` |
| 权限与审批 | `crates/kiana-permissions` | `spec/ca-session.md`；`docs/SANDBOX.md` 相关章节 | `phase-01`、`fix-passes/pass-1-cross-cutting.md` |
| 沙箱与统一执行策略 | `crates/kiana-sandbox`、`crates/kiana-tools` | `docs/SANDBOX.md`；`spec/ca-tools-compaction-export.md` | `phase-02`、`fix-passes/pass-2-phases-02-03.md` |
| 配置分层与 settings | `crates/kiana-config` | `spec/ca-config-models.md` | `phase-03`、`fix-passes/pass-2-phases-02-03.md` |
| Hooks | `crates/kiana-hooks`、`crates/kiana-session` 接线 | `spec/ca-session.md` 相关章节 | `phase-04`、`fix-passes/pass-3-phases-04-08.md` |
| MCP | `crates/kiana-mcp`、`crates/kiana-builtin-extensions/src/mcp` | `spec/telemetry-mcp.md`、`spec/ca-builtin-extensions.md` | `phase-05`、`fix-passes/pass-4-phases-05-06-09.md` |
| Skills | `crates/kiana-resources/src/skills` | `spec/ca-resources.md` | `phase-05`、`fix-passes/pass-4-phases-05-06-09.md` |
| 上下文压缩与工具输出预算 | `crates/kiana-compaction`、`crates/kiana-tools` | `spec/ca-tools-compaction-export.md` | `phase-06` |
| 编辑体验（multi_edit / apply_patch） | `crates/kiana-tools/src/multi_edit.rs`、`edit.rs`、`apply_patch` 相关模块 | `spec/ca-tools-compaction-export.md` | `phase-07`、`fix-passes/pass-3-phases-04-08.md` |
| 评测、loop guard、turn/token/cost 预算 | `crates/kiana-evals`、`crates/kiana-agent-core`、`crates/kiana-session` | `spec/env-evals-durable-testing.md` | `phase-08`、`fix-passes/pass-3-phases-04-08.md` |
| Provider 请求策略、能力矩阵、OTel | `crates/kiana-ai/src/request_policy`、`crates/kiana-telemetry` | `spec/ai-core.md`、`spec/telemetry-mcp.md` | `phase-09`、`fix-passes/pass-4-phases-05-06-09.md` |
| 持久记忆 | `crates/kiana-memory`、`crates/kiana-session/src/memory_wiring.rs` | `spec/ca-session.md` 相关章节 | `phase-10`、`wiring/phase-10-11a-12a.md` |
| 子代理运行时 | `crates/kiana-session/src/subagent`、`agent_definition` | `spec/ca-session.md` 相关章节 | `phase-11a` |
| 后台任务、恢复、checkpoint、continuity | `crates/kiana-session/src/background`、`continuity.rs`、`checkpoint/` | `spec/ca-session.md`、`spec/durable-*` | `phase-11b`、`fix-passes/pass-6-phases-11b-12b.md` |
| 下一代替换渲染核心（render_core） | `crates/kiana-interactive/src/render_core/` | — | `phase-12a`、`fix-passes/pass-5-phase-12.md` |
| Daemon / IDE / cron / web 工具 | `crates/kiana-coding-agent/src/{web,ide,cron}`、`crates/kiana-rpc-print/src/daemon.rs` | `spec/ca-rpc-print.md` | `phase-12b`、`fix-passes/pass-6-phases-11b-12b.md` |
| HTML 导出 | `crates/kiana-export` | `spec/ca-tools-compaction-export.md` | — |
| 扩展运行时 | `crates/kiana-extensions`、`kiana-ext-api`、`kiana-builtin-extensions` | `spec/ca-extensions.md`、`spec/design-extension-runtime*.md` | `phase-05` 等 |
| codemode | `crates/kiana-codemode`、`crates/kiana-builtin-extensions/src/codemode` | `spec/agent-codemode.md` | — |

### 7.2 `docs/history/porting/phase-*.md` 标题与摘要

| 文档 | 标题 | 关键内容（摘要） |
| --- | --- | --- |
| `phase-00-convergence.md` | W4 — 半成品收敛与文档对齐 | 半成品收敛、文档对齐 |
| `phase-00-convergence-verify.md` | W4 验证报告 | 命令与真实验证结果 |
| `phase-00-scaffold.md` | 按周并行移植 — 脚手架记录 | 并行移植脚手架 |
| `phase-01-permissions-approval.md` | W1 — 权限与审批闭环 | 结构化审批、Always allow 作用域、持久化与事件 |
| `phase-02-sandbox-execution-paths.md` | Week 2 — 所有执行路径都走沙箱 + 读后写 | 统一执行策略、ExecutionStatus、符号链接与写入门禁 |
| `phase-03-config-shell-risk.md` | W3 — 配置分层 + shell 风险分析 | ConfigLayer、严格诊断、managed policy、shell 风险 |
| `phase-04-hooks.md` | W4 — Hooks 配置、派发与执行契约 | 10 事件、matcher、退出码/JSON、聚合顺序 |
| `phase-05-mcp-skills.md` | W5 — MCP 管理面与 Skills 按需加载 | MCP prompts/stdio 环境白名单/重定向；Skills 激活与 TTL |
| `phase-06-compaction-output.md` | 第 6 周：上下文压缩和工具输出 | pruning、输出预算、压缩恢复 |
| `phase-07-editing-experience.md` | 第 7 周：编辑体验 | multi_edit、apply_patch、流式 patch |
| `phase-08-evals-loop-guard.md` | 第 8 周：评测和 loop guard | mock provider、loop guard、预算 |
| `phase-09-request-policy-otel.md` | 第 9 周：Provider 请求策略 + OTel | 能力矩阵、输出预算、错误分类、OTel |
| `phase-10-persistent-memory.md` | Week 10 — 持久记忆 | scope/路径、索引、召回、抽取、治理 |
| `phase-11a-subagent-runtime.md` | 第 11 周前半：子代理运行时 | AgentDefinition、运行时、隔离 |
| `phase-11b-background-checkpoints.md` | 第 11 周后半：后台任务、恢复与 checkpoint | BackgroundTaskManager、journal、checkpoint、acceptance |
| `phase-12a-tui-frame-contract.md` | 第 12 周（前半）— TUI 帧契约与审批界面 | FrameWriter/TranscriptLedger、审批卡、modal、i18n |
| `phase-12b-completion.md` | W12a / W12b 补完记录 | daemon/IDE/cron/web 接线；W12a 仍未完成项 |

### 7.3 其他已有文档

| 文档 | 用途 |
| --- | --- |
| `docs/START_HERE.md` | 整体心智模型、仓库地图、阅读路线 |
| `docs/ARCHITECTURE_OVERVIEW.md` | 架构导览 |
| `ARCHITECTURE.md` | crate 划分、依赖边、共享约定、决策记录 |
| `docs/EXECUTION_FLOW.md` | 从进程启动到一次任务结束的时序 |
| `docs/SANDBOX.md` | `kiana-sandbox` 设计与策略 |
| `docs/UNDERSTANDING_GUIDE.md` | 按 crate 的阅读顺序 |
| `docs/GLOSSARY.md` | 术语表 |
| `docs/COMMENT_STYLE.md` | 注释与文档写作规范（不面向最终用户） |
| `docs/COMMON_MISTAKES.md` | 常见理解误区（开发向） |
| `docs/DEBUGGING_GUIDE.md` | 调试指南（开发向） |
| `docs/UNVERIFIED.md` | 注释作者标记的待核实项（开发向） |
| `docs/release/v0.1.0-alpha.md` | 本地产物、冒烟与已知缺口 |
| `spec/*.md` | 各移植单元的规范与差距；写用户文档时用于核对实现边界 |

---

## 8. 已知未完成 / 半接线清单（写作禁区）

以下内容在用户文档中**不得写成已完成**。引用时标注状态与来源。

### 8.1 W12a：渲染核心与审批

1. `[NOT IMPLEMENTED]` **`FrameWriter` / `TranscriptLedger` 未替换默认 TUI 写端。**
   现有 `TuiMainScreen` / `TuiAltScreen` 仍是默认写路径；`render_core::frame` 只有单测覆盖，
   **没有生产调用点**。`KIANA_NEXT_RENDER_CORE` 的 `experimental_enabled()` 默认关闭；
   它不能单独把整条输出管线切到 `FrameWriter`。
   来源：`docs/history/porting/phase-12b-completion.md` §2.2.1；`docs/history/porting/integration-audit.md` §7.10；
   `crates/kiana-interactive/src/render_core/mod.rs:44-73`（`FrameWriter` 无生产调用点）；
   `docs/release/v0.1.0-alpha.md` §6.2.1。
2. `[NOT IMPLEMENTED]` **风险解释的真实 sideQuery 未接。**
   `ApprovalCard::begin_risk` / `resolve_risk` 状态机与 3s 超时存在，但没有向 `kiana-ai`
   发起模型调用；`PermissionCardRequest` 没有异步风险通道。`RiskAdvisory` 类型与展示状态机
   不等于“模型风险解释已可用”。
   来源：`crates/kiana-interactive/src/render_core/approval.rs:104-125,459-470`；
   `phase-12b-completion.md` §2.2.2；`pass-5-phase-12.md` §三.3。
3. `[PARTIAL]` **i18n 只覆盖部分 UI。**
   内置语言包只有 `en.json` / `zh.json`；`SUPPORTED_LANGUAGES` 声明了 en、zh、zh-TW、ja、de、fr、pt、ru、ca
   九个代码，但没有对应内置包的语言会回退英文。生产接线只把“待发消息”文案改成 key；
   审批卡/模态框的英文常量尚未全部换成 key。`ui.locale` 与模型输出语言（`KIANA_OUTPUT_LANGUAGE`
   或 `ui.outputLanguage`）是两套独立设置。
   来源：`crates/kiana-interactive/src/render_core/i18n.rs:45-47,84-125,249-330`；
   `crates/kiana-interactive/src/interactive_mode/render_core_host.rs:50-56`；
   `phase-12b-completion.md` §2.2.4；`docs/release/v0.1.0-alpha.md` §6.2.4。
4. `[NOT IMPLEMENTED]` **48 个旧组件未迁移到 render_core。**
   来源：`phase-12b-completion.md` §2.2.3；`integration-audit.md` §7.10。
5. `[PARTIAL]` 乐观用户消息队列默认开启；`KIANA_NEXT_RENDER_CORE` 只控制实验开关与
   显式关闭队列。不要写成“整条新渲染核心默认开启”。
   来源：`crates/kiana-interactive/src/render_core/mod.rs:62-73`。

### 8.2 W11a：子代理

- `[PARTIAL]` 子代理 skills 正文与 `CLAUDE.md` 未注入；运行时只列出技能名与 `omit_claude_md`。
  来源：`pass-5-phase-12.md` §三.5；`integration-audit.md` §7.8。
- `[PARTIAL]` `NeedsApproval` 在 headless runner 中会变成 failed tool result；交互式审批未接 UI。
  来源：`pass-5-phase-12.md` §三.2；`integration-audit.md` §7.8。
- `[UNVERIFIED]` overlayfs 隔离需要 root，当前未实测。
  来源：`integration-audit.md` §7.8。
- `[PARTIAL]` worktree 隔离不会自动带入父工作区未提交改动。
  来源：`integration-audit.md` §7.8。

### 8.3 W11b：后台任务与 continuity

- `[NOT IMPLEMENTED]` `BackgroundTaskResumer` 未实现；进程崩溃后 `sweep` 把 Running 记录
  terminalize 为 `interrupted/process_lost`，不收养/重跑子进程。
  来源：`pass-6-phases-11b-12b.md` §四.2；`integration-audit.md` §7.9。
- `[PARTIAL]` `ExecutionStore` 仍使用 `FileExecutionStore`，claim 文件不是 kiana-durable 事务存储。
  来源：`pass-6-phases-11b-12b.md` §四.3；`integration-audit.md` §7.9。
- `[NOT IMPLEMENTED]` 后台通知的跨进程投递未接；通知进入当前进程队列，父会话必须在同一进程
  后续 run 中消费。来源：`pass-6-phases-11b-12b.md` §四.6；`integration-audit.md` §7.9。
- `[NOT IMPLEMENTED]` 没有 `--continuity` CLI flag；主渠道是 settings `continuity` 与
  `KIANA_CONTINUITY`。来源：`pass-6-phases-11b-12b.md` §四.4。
- `[PARTIAL]` cron `context_from` 只接受会话文件路径，不支持任意表达式/跨会话引用。
  来源：`pass-6-phases-11b-12b.md` §四.7；`integration-audit.md` §7.9。

### 8.4 W10：持久记忆

- `[PARTIAL]` 抽取只在 `AgentSession::new` 启动一次后台任务，没有周期性重跑调度器。
  来源：`pass-5-phase-12.md` §三.6；`integration-audit.md` §7.7。
- `[PARTIAL]` provider 抽取/召回依赖宿主配置；没有 provider 时召回回退启发式。
  来源：`integration-audit.md` §7.7。
- `[VERIFIED]` 自动抽取默认关闭（`auto_extract = false`），必须显式开启。
  来源：`crates/kiana-memory/src/config.rs:20-28`；`memory_wiring.rs:77-82`。

### 8.5 W2 / W3 / W4 / W5 / W6 / W7 / W8 / W9：功能边界

- `[PARTIAL]` W2：PowerShell 工具选项没有固定沙箱 manager 字段；`MCP stdio` 的沙箱注入已补，
  但提前返回的 CLI 子命令不经过安装点（见 §8.7）。
  来源：`integration-audit.md` §7.1；同文件 §14.1“残余”。
- `[PARTIAL]` W3：配置层来源未展示在 UI；`SettingsManager::apply_overrides` 的 permissions
  覆盖不进入分层。来源：`integration-audit.md` §4 表格的 W3 行。
- `[PARTIAL]` W4：`HookSource` 没有 `Managed` 变体，`allowManagedHooksOnly=true` 会停用所有
  hooks（安全但功能缺失）；hook audit 只在内存环形缓冲；SessionStart `additionalContext`
  只审计；async hook 无全局并发上限；HTTP hook 不支持企业代理。
  来源：`integration-audit.md` §7.2。
- `[NOT IMPLEMENTED]` W5：技能 `context: fork` 未执行；技能激活 hooks、`agent_scope`、`model`、
  `effort` 未实现；`/mcp` 结构化状态未进管理器详情页。
  来源：`integration-audit.md` §7.3。
- `[PARTIAL]` W6：PTL 头部丢弃仍是单次 compact+retry；恢复附件只注入一次。
  来源：`integration-audit.md` §7.4。
- `[NOT IMPLEMENTED]` W7：`EditStrategy::Regex` 只是预留值，当前不会自动选用；`apply_patch`
  没有 TUI 渲染；跨文件没有真正文件系统事务。
  来源：`crates/kiana-tools/src/multi_edit.rs:28,110`；`integration-audit.md` §7.5。
- `[PARTIAL]` W8：`StopReason` 未新增 cap 变体；真实 provider eval 未接；`pass@k` 无置信区间。
  来源：`integration-audit.md` §7.6。
- `[PARTIAL]` W9：gRPC e2e 缺失；OTLP 为子集；内容捕获只在构造期读取；tracestate 只校验。
  来源：`integration-audit.md` §7.4。

### 8.6 W12b：daemon / IDE / cron / web 的剩余风险

- `[PARTIAL]` daemon token 只打印到 stderr，没有轮换/持久化；默认只绑定 loopback，
  非 loopback 部署仍需反向代理提供 TLS 与额外认证。
  来源：`phase-12b-completion.md` §4；`docs/release/v0.1.0-alpha.md` §6.3。
- `[PARTIAL]` `--serve` 仍要求会话有可用模型；没有模型时会在进入 daemon 分支前退出 1。
  来源：`phase-12b-completion.md` §4；`docs/release/v0.1.0-alpha.md` §6.3。
- `[PARTIAL]` IDE 连接只在启动时建立一次；没有持续编辑器事件推送；`openDiff` 有 broker 与
  通知路由，但没有工具调用点。来源：`phase-12b-completion.md` §4。
- `[PARTIAL]` cron 受管脚本沿用 `KIANA_SANDBOX_POLICY`，没有独立 cron 策略配置项。
  来源：`phase-12b-completion.md` §4。
- `[PARTIAL]` web 工具默认激活依赖注册时的环境变量；在 settings 里配置 key 不会改变默认激活。
  来源：`phase-12b-completion.md` §4。
- `[PARTIAL]` `EventFeed` replay 有界（4096），可能产生 gap。
  来源：`integration-audit.md` §7.11。

### 8.7 权限 / 沙箱 / 其他

- `[PARTIAL]` 非交互模式没有 `--yes` 自动批准实现（审计 W1 行）。
  来源：`docs/history/porting/integration-audit.md` §4 的 W1 行。
- `[PARTIAL]` MCP stdio 沙箱残余：`Unconfirmed`/`BackendUnavailable`/`Interrupted` 错误路径仍以
  `Err` 文本返回，不带结构化状态；扩展 JS `user_bash` 不解析 `executionStatus`；提前返回的
  CLI 子命令（`kiana mcp ...`、package、config）不经过默认沙箱安装点。
  来源：`integration-audit.md` §14.1“残余”。
- `[PARTIAL]` 平台验证：只构建/验证 Linux x86_64；没有 macOS/Windows 产物；沙箱语义依赖 Linux。
  来源：`docs/release/v0.1.0-alpha.md` §6.4；`integration-audit.md` §14.5。
- `[UNVERIFIED]` 真实模型 provider / 真实网络路径未做冒烟；只有 mock 与单元/集成测试。
  来源：`docs/release/v0.1.0-alpha.md` §6.4；`integration-audit.md` §14.5。
- `[PARTIAL]` 版本号：workspace `0.1.0` 与 `kiana --version` 的 `1.4.0` 不一致（有意保留）。
  来源：`docs/release/v0.1.0-alpha.md` §1；`crates/kiana-core/src/config.rs:162`。

### 8.8 文档写作禁区（不要出现的说法）

- 不要写“下一代替换渲染核心已默认启用”或“`FrameWriter` 已在生产使用”。
- 不要写“风险解释 sideQuery 已接入模型”。
- 不要写“内置支持 9 种 UI 语言”；内置只有 en、zh，其他语言需要用户语言包或会回退英文。
- 不要写“`/help` 是斜杠命令”；本仓库没有该实现。
- 不要写“`--max-turns` / `--max-tokens` / `--max-cost-usd` / `--max-wall-clock-ms` /
  `--no-loop-guard` / `--loop-guard-stop` 是 `--help` 中的公开选项”（它们是代码接受但
  未列出的内部选项）。
- 不要写“`--offline` 是死参数”；它通过原始 argv 扫描生效（`app.rs:231-240`）。
- 不要写“`KIANA_ATTRIBUTION` / `KIANA_AI_VERSION` 是环境变量”；它们是常量。
- 不要写“`.kiana/settings.local.json` 是完整 settings 层”或“它只合并 `permissions`”；它不提供类型化 Settings 键，但项目受信时会话配置层会整文件加载它并合并 `permissions`、`hooks` 等键。
- 不要写“`/llama` 只在某些安装中可用”；`llama.cpp` 是内建扩展，但 `--no-extensions` 会
  关闭全部内建扩展。
- 不要写“`--serve` 已具备完整生产级鉴权/轮换”；token 只打印到 stderr 且不轮换。

---

## 9. UNVERIFIED / 待核对清单

| 项 | 原因 | 写作建议 |
| --- | --- | --- |
| 各 provider 的 API key 优先级、OAuth 细节 | 本清单只核对 `--help` 名称与粗粒度说明 | 写 provider 专章前读 `spec/ai-auth.md` 与对应 `kiana-ai/src/auth` 代码 |
| `--models` 的 glob/fuzzy 具体算法 | `--help` 只说明“支持 glob 与模糊匹配” | 不写具体匹配规则；只给示例 |
| `--export` 未指定输出路径时的默认文件名 | 代码为 `kiana-session-<fallback_base>.html`，`fallback_base` 的取值未完整核对 | 需要时写“默认文件名由导出模块生成”，或先核对 `kiana-export/src/index.rs` |
| `builtin:<name>` 的可用名称全集 | 代码中内建扩展有 `llama.cpp`、`codemode`、`tool-search`、`mcp`，但 CLI 字符串是否支持这些名字未逐项实测 | 只写 `builtin:<name>` 形式；具体名单标 UNVERIFIED |
| `/share` 的 Radius vs gist 行为细节 | 本清单只核对命令描述与代码入口 | 写 `/share` 前读 `session_share.rs` 与 `spec` |
| `--min-expiry` 大写单位 | 正则大小写不敏感，但单位比较大小写敏感，`30M` 会按小时处理 | 文档只推荐小写 `ms/s/m/h`；不要提大写 |
| 设置项的默认值（§5.1 中标“—”） | `SETTINGS_DEFAULTS` 未列出，getter 可能另有回退 | 不写默认值，或标“由运行时决定” |
| `KIANA_*` 测试变量的完整清单 | 数量多且随测试变化 | 用户文档不列；开发文档引用 §4.5 前缀 |
| 平台差异（Windows/macOS） | 未构建、未实测 | 明确写“当前验证范围：Linux x86_64” |

---

## 10. 快速事实核对命令

用于后续写作者复核本文件：

```bash
# 顶层与子命令帮助
/home/ctyun/code/kiana/dist/v0.1.0-alpha/kiana --help
/home/ctyun/code/kiana/dist/v0.1.0-alpha/kiana auth --help
/home/ctyun/code/kiana/dist/v0.1.0-alpha/kiana mcp --help
/home/ctyun/code/kiana/dist/v0.1.0-alpha/kiana install --help
/home/ctyun/code/kiana/dist/v0.1.0-alpha/kiana remove --help
/home/ctyun/code/kiana/dist/v0.1.0-alpha/kiana update --help
/home/ctyun/code/kiana/dist/v0.1.0-alpha/kiana list --help
/home/ctyun/code/kiana/dist/v0.1.0-alpha/kiana config --help

# 版本
/home/ctyun/code/kiana/dist/v0.1.0-alpha/kiana --version

# 关键源码
sed -n '729,932p' /home/ctyun/code/kiana/crates/kiana-coding-agent/src/cli/args.rs
sed -n '110,156p' /home/ctyun/code/kiana/crates/kiana-core/src/slash_commands.rs
sed -n '829,961p' /home/ctyun/code/kiana/crates/kiana-config/src/settings/schema.rs
sed -n '171,235p' /home/ctyun/code/kiana/crates/kiana-config/src/settings/defaults.rs
```
