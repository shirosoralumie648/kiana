# 交互模式（TUI）

本页讲什么：如何启动 kiana 的终端交互界面，界面由哪些区域组成，斜杠命令、排队消息、审批卡、主题、TUI 模式与快捷键分别怎么用。命令行参数全集见 [CLI 参考](cli.md) 与 `kiana --help`。

## 启动

在项目目录下直接运行 `kiana` 即可进入交互模式：

```bash
# 在当前项目启动交互界面
kiana

# 启动时直接带上第一条消息
kiana "解释这个仓库的目录结构"

# 使用常规（主屏）模式，把输出留在终端自己的滚动缓冲区里
kiana --tui-mode regular

# 继续上一个会话；或从列表中选择要恢复的会话
kiana --continue
kiana --resume

# 打开指定会话，或从某个会话复制出新会话
kiana --session <path|id>
kiana --fork <path|id>

# 本次运行指定初始主题
kiana --use-theme dark
```

是否进入交互模式由 kiana 自动判断：

| 条件 | 进入的模式 |
| --- | --- |
| 默认（stdin 与 stdout 都是 TTY） | 交互模式 |
| `--print` / `-p` | 非交互（处理 prompt 后退出） |
| stdin 或 stdout 不是 TTY | 非交互（即使没有写 `--print`） |
| `--mode json` | JSON 输出（非交互） |
| `--mode rpc` | RPC 模式 |

补充说明：

- `--mode text` 是默认值；只要有 TTY、没有 `--print`，仍然进入交互模式。
- `--mode <mode>` 缺值时直接报错，不会消费下一个参数。
- `kiana --version` 输出 `1.4.0`，而 workspace 包版本是 `0.1.0`。前者是刻意保留的 TS 兼容常量，不是构建错误。

## 界面组成

交互界面自上而下由这些区域组成：

| 区域 | 内容 |
| --- | --- |
| Header | 启动横幅、版本与入门提示 |
| Loaded resources | 本次加载的扩展、skills、prompts、themes 等资源摘要（启动和 `/reload` 时输出） |
| Chat | 会话正文：用户消息、assistant 回复、工具执行、压缩摘要、分支摘要等 |
| 待发消息区 | 尚未处理完的排队消息（Steering / Follow-up）与尚未回显的待发消息 |
| 状态行 | 短提示，例如模型切换、HTTP idle timeout、TUI mode、reload 状态 |
| 编辑器 | 输入框；边框颜色会随 thinking level 与 bash 模式变化 |
| Footer | 工作目录、git 分支、会话名、用量统计、当前模型与思考等级、扩展状态 |
| Widgets | 扩展在编辑器上方或下方挂载的自定义组件 |

常用输入规则：

- `Enter` 提交消息。
- `Shift+Enter` 或 `Ctrl+J` 插入换行。
- `Tab` 做路径补全或接受自动补全候选。
- 以 `/` 开头的输入按斜杠命令处理；以 `!` 开头的输入执行 bash 命令；`!!` 执行 bash 命令并把它排除出模型上下文。
- `Esc` 取消自动补全；自动补全未展开时，流式回复中会中止当前回合并把排队消息取回编辑器，bash 运行中会中止 bash。
- `Ctrl+C` 第一次清空编辑器；两次按键间隔小于 500ms 时退出。编辑器为空时 `Ctrl+D` 直接退出。

## 斜杠命令

在编辑器中输入 `/` 会打开命令补全。内建斜杠命令共 24 个：

| 命令 | 参数 | 作用 | 说明 |
| --- | --- | --- | --- |
| `/settings` | — | 打开设置面板 | 写入用户设置，详见 [设置（settings）](settings.md) |
| `/model` | `[provider/model]` | 打开模型选择器；参数精确命中时直接切换 | 选择器内按 `Ctrl+S` 可保存为默认模型 |
| `/tree` | — | 浏览会话树、切换分支 | — |
| `/thinking` | `[level]` | 打开思考等级选择器，或直接设置等级 | 完整级别为 `off`、`minimal`、`low`、`medium`、`high`、`xhigh`、`max`，实际可用值取决于当前模型；未知值会报错并列出可用等级 |
| `/scoped-models` | — | 启用/禁用 `Ctrl+P` 循环使用的模型 | 选择结果可持久化到 `enabledModels` |
| `/export` | `[path]` | 导出当前会话 | 路径以 `.jsonl` 结尾时导出 JSONL，其余情况导出 HTML；省略路径时由导出模块生成默认文件名 |
| `/import` | `<path.jsonl>` | 从 JSONL 导入并恢复会话 | 会先确认是否替换当前会话 |
| `/share` | — | 分享当前会话 | 目的地由 `sessionShareDestination` 决定（`auto` / `gist` / `radius`） |
| `/bug` | `[description]` | 向 Kiana developers 报告 bug | — |
| `/copy` | — | 复制最后一条 agent 消息到剪贴板 | — |
| `/name` | `[name]` | 设置会话显示名 | 省略名称时显示当前名称；没有名称时提示 `Usage: /name <name>` |
| `/session` | — | 显示会话信息与统计 | — |
| `/changelog` | — | 显示 changelog 条目 | — |
| `/hotkeys` | — | 显示当前键位下的快捷键表 | 表格按生效键位渲染，并追加扩展注册的快捷键 |
| `/fork` | — | 从之前的用户消息创建 fork | 打开用户消息选择器 |
| `/clone` | — | 在当前点复制整个会话 | — |
| `/trust` | — | 保存项目信任决定 | 写入 `trust.json`；提示重启 kiana 后生效 |
| `/login` | `[provider]` | 配置 provider 认证 | 打开认证方式与 provider 选择器 |
| `/logout` | — | 移除 provider 认证 | 打开 OAuth 登出选择器 |
| `/new` | — | 开始新会话 | — |
| `/compact` | `[instructions]` | 手动压缩上下文 | 可附带自定义压缩指令 |
| `/resume` | — | 恢复另一个会话 | 打开会话选择器 |
| `/reload` | — | 重载键位、扩展、skills、prompts、主题与上下文文件 | 流式回复或压缩进行中会被拒绝 |
| `/quit` | — | 退出 kiana | — |

### 面板命令

以下命令由 TUI 面板注册表提供，不属于上面的 24 项内建命令表：

| 命令 | 参数 | 作用 | 说明 |
| --- | --- | --- | --- |
| `/permissions` | — | 打开权限面板 | Allow / Ask / Deny / Workspace 四页；查看、新增、删除规则，切换权限模式，管理工作区目录。工作区目录删除当前版本尚未接线，会提示不支持 |
| `/tasks` | — | 打开后台任务面板 | 列出 agent / shell / monitor 任务，查看详情、刷新、取消；任务功能未启用时显示提示 |
| `/mcp` | — | 打开 MCP 管理面板 | 仅精确 `/mcp` 打开面板；`/mcp login`、`/mcp logout`、`/mcp reconnect` 等带子命令的形式仍由内建 MCP 扩展处理 |

除内建命令外，扩展还可以注册命令：

| 命令 | 来源 | 说明 |
| --- | --- | --- |
| `/mcp` | 内建 MCP 扩展 | 带子命令时管理 MCP 登录、重连、启停与 exposure；无参数时由上面的 TUI 面板接管 |
| `/llama` | 内建 `llama.cpp` 扩展 | 仅交互模式可用；`--no-extensions` 会连同其它内建扩展一起关闭 |
| `/skill:<name> [args]` | skills | 仅当 `enableSkillCommands` 为真时注册；`/skill:deactivate <name>` 或 `/skill:off <name>` 显式停用 |
| `/<template-name> [args]` | prompt templates | 名称精确匹配、大小写敏感；未命中时按普通输入处理 |
| `/debug` | 内部调试命令 | 不在 24 项内建命令表中，也不进入自动补全；把当前渲染行与会话消息写入 `<agentDir>/kiana-debug.log` |

注意：

- 本仓库**没有** `/help` 斜杠命令。查看帮助请运行 `kiana --help` 或 `kiana <command> --help`。
- `/settings`、`/tree`、`/session`、`/hotkeys`、`/fork`、`/trust`、`/logout`、`/new`、`/resume`、`/quit` 等命令只匹配精确文本，带参数时不会按该命令处理（`/model`、`/thinking`、`/export`、`/import`、`/bug`、`/name`、`/login`、`/compact` 支持参数）。
- 压缩或流式回复期间，普通命令的排队规则见下一节。

## 排队消息（Steering / Follow-up / 待发消息）

交互模式把“还没轮到处理”的输入分成三类，并显示在同一个待发消息区：

| 类型 | 产生方式 | 何时处理 | 显示 |
| --- | --- | --- | --- |
| Steering | 流式回复中按 `Enter` 提交 | 注入当前回合；`steeringMode` 控制一次取一条还是一次全取 | `Steering: <内容>` |
| Follow-up | 按 `Alt+Enter`（Windows/WSL 键位为 `Ctrl+Q`）排队 | 等 agent 停止后再处理；`followUpMode` 控制取用方式 | `Follow-up: <内容>` |
| 待发消息 | 正常提交后、服务端回显前的乐观展示 | 收到同内容的用户消息回显后消失；150 秒无回显会转为发送失败（`Send timed out`，可重试） | `Sending... <内容>`；失败时显示 `Failed to send: ...` |

其他行为：

- 空闲时按 `Alt+Enter` 与按 `Enter` 相同，立即提交。
- 取回排队消息：`Alt+Up`（Windows/WSL 键位为 `Alt+Q`）。它清空 Steering 与 Follow-up 队列，把内容用空行连接后写回编辑器，排队区底部提示 `↳ <快捷键> to edit all queued messages`。
- 压缩进行中提交的消息进入本地压缩队列（按提交方式分为 Steer / Follow-up），压缩结束后重放；状态行提示 `Queued message for after compaction`。
- `steeringMode` 与 `followUpMode` 的取值均为 `one-at-a-time`（默认，一次一条）或 `all`（一次全部）。

## 审批卡与模态

当权限引擎对某次工具调用返回“需要询问”时，交互界面会弹出审批卡。没有可用的交互 UI 时（例如非交互模式），该动作会直接以拒绝收场。

卡片按工具类型展示不同内容：

| 工具 | 展示内容 |
| --- | --- |
| Bash / PowerShell | 完整命令、重定向（`>`、`>>`、`<`、`2>&1`、heredoc）与子命令（`&&`、`\|\|`、`;`、`\|`、换行） |
| FileEdit | 文件路径 + 行级 diff（`-` 删除 / `+` 新增 / 上下文） |
| FileWrite | 文件路径 + Create/Overwrite + 内容预览 |
| WebFetch | 只显示域名，不显示 path 与 query |
| MCP | server / tool + 脱敏后的参数 |
| Skill | skill 名 + prompt 预览 |
| 其他工具 | 工具名 + 脱敏 JSON |

可选操作固定按顺序：

1. `Allow once`
2. `Always allow (this session)` / `Always allow (this project)` / `Always allow (this machine)`（仅在允许持久批准时出现）
3. `Reject`

快捷键：

| 键 | 作用 |
| --- | --- |
| `Up` / `Down`、`k` / `j` | 移动选项 |
| `1`–`9` | 直接选中对应序号并确认 |
| `Enter` | 确认当前选项 |
| `Esc` | 拒绝，原因为 “Approval was cancelled.” |
| `Ctrl+Enter` | 危险动作的显式批准；终端必须发送对应的 `Ctrl+Enter` 序列 |

行为约束：

- 同一时刻只显示一张审批卡，标题始终为 `Approval 1/N`（中文界面为“审批 1/N”，N 为当前审批队列总数，包含正在显示的这张）；决定按先进先出顺序结算。
- 危险动作（例如被权限层标记为危险命令）默认焦点在 `Reject`，按 `Enter` 不会批准；需要先移动到允许项，再用 `Ctrl+Enter` 显式确认。
- 关闭界面或取消审批按 fail-closed 处理，结果一律是拒绝。
- destructive / open-world 的 MCP 工具以及必须每次重新确认的场景，卡片只提供 `Allow once` 与 `Reject`，不提供“总是允许”。
- 风险解释目前只有 UI 状态机与单元测试（加载态、3 秒超时），**没有任何生产调用点，也没有接入真实的模型旁路查询**；生产路径构造的审批卡固定处于隐藏态，不会显示“分析中”加载态、真实分析或超时行为，也不阻塞审批。

## 主题与 TUI 模式

### 主题

主题决定界面颜色。相关入口：

```bash
# 本次运行使用某个主题（不写入设置）
kiana --use-theme dark

# 追加一个主题文件或目录
kiana --theme ./themes/my-theme.json

# 禁用主题发现与加载
kiana --no-themes
```

- 内置主题：`system`、`dark`、`light`。
- `system` 是保留名：它按终端上报的前景/背景色与 16 色调色板即时生成颜色，用户在终端切换明暗时会跟随变化。
- 用户主题放在 `<agentDir>/themes` 与 `<cwd>/.kiana/themes`；`--theme` 可以再追加路径。
- 在 `/settings` 的 `Theme` 子菜单中可以选择固定主题，或选择 `automatic`：为浅色与深色终端分别指定主题，设置值保存为 `light/dark` 形式。
- 主题加载失败会回退到 `system`，并在界面中给出错误信息。

### TUI 模式

| 模式 | 行为 |
| --- | --- |
| `fullscreen`（默认） | 使用终端备用屏与内置视口渲染，界面占满终端，滚动由 kiana 自己处理 |
| `regular` | 使用终端主屏与正常滚动缓冲区，退出后内容留在终端历史里 |

切换方式：

- 启动参数：`--tui-mode fullscreen` / `--tui-mode regular`。
- 设置项：`tuiMode`，或 `/settings` → `TUI mode`。
- 当前有活动 overlay（例如选择器或审批卡）时，切换会被拒绝并提示 `Close active overlays before changing TUI mode`。

全屏模式相关设置：

| 设置键 | 取值 | 说明 |
| --- | --- | --- |
| `fullscreenExitOutput` | `transcript`（默认）/ `resume-hint` | 退出全屏时是否把 transcript 重绘到终端主屏；只有 `transcript` 会这样做，`resume-hint` 只结束界面。该设置不影响恢复提示的打印 |
| `fullscreenScrollbar` | `auto`（默认）/ `always` / `hidden` | 全屏滚动条行为；regular 模式无效果 |
| `fullscreenCopyOnSelect` | `true`（默认）/ `false` | 选中文本后自动复制；关闭后可用 `Ctrl+X` 复制选区 |
| `fullscreenWheelScrollLines` | `auto`（默认）/ 数字 | 鼠标滚轮每格滚动行数；`auto` 会在快速滚轮时加速 |

恢复提示 `To resume this session:` 与 `fullscreenExitOutput` 无关：只要 stdout 是 TTY、会话已持久化且会话文件存在，任何正常退出（包括 `transcript` 与 `resume-hint` 两种模式）都会打印；信号退出路径不打印。

全屏模式快捷键（默认值）：

| 键 | 作用 |
| --- | --- |
| `PageUp` / `PageDown` | 上/下翻一页 |
| `Ctrl+Shift+Up` / `Ctrl+Up` | 跳到上一条语义提示（Windows/WSL 键位只有 `Ctrl+Up`） |
| `Ctrl+Shift+Down` / `Ctrl+Down` | 跳到下一条语义提示（Windows/WSL 键位只有 `Ctrl+Down`） |
| `Ctrl+Shift+F` | 搜索 transcript（Windows/WSL 键位为 `Ctrl+F`） |
| `Enter` / `Ctrl+G` | 下一个匹配 |
| `Shift+Enter` / `Ctrl+Shift+G` | 上一个匹配 |
| `Esc` | 关闭搜索 |
| `Ctrl+Home` / `Ctrl+End` | 跳到顶部 / 底部 |

## 快捷键

以下是主编辑器与常用上下文中的默认键位；`/hotkeys` 会按当前生效配置渲染同样的表格。完整键位 id 有 90 个（47 个 `tui.*` + 43 个 `app.*`）。

### 编辑器与全局

| 键 | 作用 |
| --- | --- |
| `Enter` | 提交输入 |
| `Shift+Enter` / `Ctrl+J` | 插入换行 |
| `Tab` | 路径补全 / 接受自动补全 |
| `Esc` | 取消自动补全 / 中止流式回复 |
| `Ctrl+C` | 清空编辑器（第一次）/ 退出（第二次） |
| `Ctrl+D` | 编辑器为空时退出 |
| `Ctrl+Z` | 挂起到后台（原生 Windows 无默认绑定） |
| `Up` / `Down` / `Left` / `Right` | 移动光标或浏览历史 |
| `Ctrl+P` | 切换到下一个模型 |
| `Shift+Ctrl+P` | 切换到上一个模型（Windows/WSL 键位为 `Alt+P`） |
| `Ctrl+L` | 打开模型选择器 |
| `Shift+Tab` | 循环切换 thinking level |
| `Ctrl+T` | 切换 thinking 块显示 |
| `Ctrl+O` | 展开/收起工具输出 |
| `Ctrl+G` | 用外部编辑器编辑当前输入 |
| `Ctrl+X` | 复制选区或最后一条 assistant 消息 |
| `Alt+Enter` | 排队 follow-up 消息（Windows/WSL 键位为 `Ctrl+Q`） |
| `Alt+Up` | 取回排队消息到编辑器（Windows/WSL 键位为 `Alt+Q`） |
| `Ctrl+V` | 粘贴文件路径（macOS）、图片或剪贴板文本（Windows/WSL 键位为 `Alt+V`） |
| 连按两次 `Esc` | 执行 `doubleEscapeAction`：`tree`（默认）/ `fork` / `none`；两次按键需在 500ms 内且编辑器为空 |

### 选择器与对话框

| 键 | 作用 |
| --- | --- |
| `Up` / `Down` | 移动选中项 |
| `Enter` | 确认 |
| `Esc` / `Ctrl+C` | 取消 |
| `Space` | 切换设置项的取值（设置面板搜索框为空时） |

`/model` 选择器内还有：`Ctrl+S` 保存为默认模型、`Ctrl+A` 启用全部模型、`Ctrl+X` 清空选择、`Ctrl+P` 切换整个 provider、`Alt+Up` / `Alt+Down` 调整顺序。`/thinking` 选择器内按 `Ctrl+S` 可把当前选中等级保存为默认等级。

### 审批卡

见上一节“审批卡与模态”。

### 自定义键位

键位保存在 `<agentDir>/keybindings.json`（`agentDir` 默认 `~/.kiana/agent`，可用 `KIANA_CODING_AGENT_DIR` 覆盖）。值可以是单个键位字符串，也可以是字符串数组：

```json
{
  "app.message.followUp": "alt+enter",
  "app.message.dequeue": ["alt+up", "ctrl+u"]
}
```

- 键位 id 形如 `app.interrupt`、`tui.select.confirm`；非法键位值会被静默丢弃。
- 旧键名会在启动迁移中自动改名。
- 修改后执行 `/reload` 重新加载键位（扩展提供的自定义编辑器可能仍使用旧绑定）。

## 限制与排查

- **没有 `/help`**：本仓库没有实现该斜杠命令，请使用 `kiana --help`。
- **界面语言覆盖不完整**：`ui.locale` 可读，内置语言包只有 `en` 与 `zh`；其他语言回退英文，且旧界面仍有英文常量。`/settings` 的 `Language` 项会写入 `ui.locale` 并立即重新加载翻译器，但覆盖范围有限，详见 [国际化（i18n）](i18n.md)。
- **审批风险解释未接线**：只有状态机与单元测试，没有生产调用点；当前卡片不会显示加载态、真实模型风险分析或超时隐藏行为。
- **`Ctrl+Enter` 依赖终端**：部分终端不发送该序列；此时危险动作只能拒绝，或在支持的情况下改用其它终端。
- **`/reload` 不能打断进行中的回合**：流式回复或压缩期间会提示稍后重试。
- **TUI 模式切换受 overlay 限制**：先关闭选择器、审批卡等活动 overlay。
- **`/share` 需要外部服务与凭据**：gist 与 Radius 的真实网络路径当前未做冒烟验证（未验证）。
- **平台范围**：当前只在 Linux x86_64 上构建与验证；macOS/Windows 的键位与终端行为未实测。
- **新帧写端不是默认路径**：第 12 周的 `FrameWriter` / `TranscriptLedger` 实验渲染核心没有替换默认 TUI 输出路径；乐观待发消息队列默认开启，但这不等于“整条新渲染核心默认启用”。

## 相关页面

- [设置（settings）](settings.md)
- [会话、分支与恢复](sessions.md)
- [权限与审批](permissions.md)
- [CLI 参考](cli.md)
- [国际化（i18n）](i18n.md)
