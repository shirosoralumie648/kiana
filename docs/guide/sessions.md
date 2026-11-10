# 会话、分支与恢复

本页讲什么：`kiana` 如何保存会话、怎样用命令行继续/恢复/分叉会话，交互界面里的 `/tree`、`/fork`、
`/clone`、`/compact`、`/export`、`/import`、`/share` 各自做什么，以及 compaction、自动重试、
continuity（执行所有权、checkpoint、后台任务）的当前实现和限制。

> 版本说明：本仓库 workspace 包版本是 `0.1.0`，但 `kiana --version` 输出 `1.4.0`。后者来自
> `kiana-core` 的 TS 兼容常量，是有意保留的差异，不是构建错误。当前只在 Linux x86_64 上
> 验证；macOS/Windows 行为未实测。

## 快速开始

```bash
kiana                          # 新建会话
kiana -c "上次改到哪了？"       # 继续当前目录最近的会话
kiana -r                       # 打开会话选择器
kiana --session-id refactor    # 用固定 ID 新建/打开项目会话
kiana --session ~/.kiana/agent/sessions/--home-me-proj--/2026-11-08T10-00-00-000Z_0194f0e2-....jsonl
kiana --fork 0194f0e2 --name "另一条思路"   # 复制会话后再改
kiana --no-session -p "临时问一句"          # 不保存到磁盘
```

交互界面里最常用的会话命令：

| 命令 | 作用 | 说明 |
| --- | --- | --- |
| `/tree` | 浏览会话树、切换分支 | 可生成分支摘要 |
| `/fork` | 从某条历史用户消息分叉 | 新建会话，并把该消息文本放回编辑器 |
| `/clone` | 在当前叶子上克隆会话 | 新建会话，编辑器清空 |
| `/resume` | 切换到另一个已保存会话 | 打开会话选择器 |
| `/new` | 开始新会话 | 显示 `✓ New session started` |
| `/compact` | 手动压缩上下文 | 可附带自定义压缩指令 |
| `/export` | 导出当前会话 | `.jsonl` 或 HTML |
| `/import` | 导入 JSONL 并切换 | 会先确认替换当前会话 |
| `/share` | 分享当前会话 | Radius 或私有 GitHub gist |
| `/name` | 设置会话显示名 | 不带参数时显示当前名称 |
| `/session` | 查看会话文件、ID 与统计 | 纯展示，不写会话 |

完整斜杠命令列表见 [交互模式](interactive-mode.md)。

## 会话存放在哪里

`<agentDir>` 默认是 `~/.kiana/agent`，可用 `KIANA_CODING_AGENT_DIR` 覆盖。

默认目录结构：

```text
<agentDir>/sessions/--<cwd 去掉开头 / 或 \，再把 / \ : 换成 -->/
    <ISO 时间戳，: 和 . 换成 -->_<session-id>.jsonl
```

例如 cwd 为 `/home/me/proj`：

```text
~/.kiana/agent/sessions/--home-me-proj--/2026-11-08T10-00-00-000Z_0194f0e2-....jsonl
```

会话目录优先级（从高到低）：

```text
--session-dir > KIANA_CODING_AGENT_SESSION_DIR > settings.sessionDir > 默认 <agentDir>/sessions
```

| 项 | 说明 |
| --- | --- |
| `--session-dir <dir>` | 本次运行覆盖会话存储目录，同时影响查找 |
| `KIANA_CODING_AGENT_SESSION_DIR` | 环境变量覆盖；空串按未设置处理 |
| `settings.sessionDir` | 用户/项目 settings 中的目录 |
| 默认目录 | 按 cwd 编码目录名，不同项目互不混放 |

补充：

- 新会话在**第一条 user/assistant 消息落盘时**才真正创建 `.jsonl`；只打开界面、没有对话不会留下文件。
- 会话文件是 append-only 的 JSONL。第一行是 header，其余每行一个条目。
- header 形如 `{"type":"session","version":3,"id":"...","timestamp":"...","cwd":"..."}`；
  fork 出来的会话额外带 `parentSession`。
- 条目用 `id` 与 `parentId` 组成树；`message`、`model_change`、`thinking_level_change`、`compaction`、
  `branch_summary`、`custom`、`custom_message`、`label`、`session_info`、`usage`、`context_edit` 等类型
  都可能出现。`session_info` 的最新一条决定会话显示名。
- 打开 v1/v2 旧会话时会在加载阶段自动迁移到 v3，并**直接重写原文件**；没有备份文件。需要保留旧文件时
  先自行复制。
- 会话列表通过扫描目录下的 `.jsonl` 并读取 header 得到；没有合法 header 的文件不会被当成会话。

## 用 CLI 选择会话

| Flag | 取值 | 行为 |
| --- | --- | --- |
| `--continue`, `-c` | 无 | 打开当前会话目录中 mtime 最新的会话；没有则新建 |
| `--resume`, `-r` | 无 | 打开会话选择器；取消时打印 `No session selected` 并以 `0` 退出 |
| `--session <path 或 id>` | 会话文件路径或部分 UUID | 按下面“解析顺序”打开 |
| `--session-id <id>` | 精确的项目会话 ID | 已存在则打开；不存在则警告后按该 ID 新建 |
| `--fork <path 或 id>` | 会话文件路径或部分 UUID | 复制源会话为新会话 |
| `--session-dir <dir>` | 目录 | 覆盖本次运行的会话目录 |
| `--no-session` | 无 | 会话只存在内存中，不写盘 |
| `--name`, `-n <name>` | 名称 | 设置会话显示名；空白值报错 |
| `--export <file> [output]` | 输入会话文件；可选输出 | 导出 HTML 后退出，详见“导出、导入与分享” |

互斥与冲突（启动前报错，退出码 `1`）：

- `--fork` 不能与 `--session`、`--continue`、`--resume`、`--no-session` 同时使用。
- `--session-id` 不能与 `--session`、`--continue`、`--resume` 同时使用。
- `--fork` 与 `--session-id` 可以同时使用；但当前项目已存在同 ID 会话时，报
  `Session already exists with id '<id>'`。

### `--session` 的解析顺序

1. 参数含 `/` 或 `\`，或以 `.jsonl` 结尾：按文件路径处理，相对路径相对当前 cwd 解析。
   文件不存在时按该路径**新建**会话（仍延迟到首条对话消息才落盘）；存在但不是合法会话文件则报错。
2. 当前项目会话中精确命中 ID。
3. 当前项目会话中第一个 ID 前缀匹配。
4. 全部会话中先精确、再前缀匹配。
5. 都不命中：报 `No session found matching '<参数>'`，退出码 `1`。

第 4 步命中另一个项目的会话时，会打印：

```text
Session found in different project: <会话记录的 cwd>
Fork this session into current directory? [y/N]
```

回答 `y`/`yes` 会把它 fork 到当前项目；回答否打印 `Aborted.` 并以 `0` 退出；stdin 提前结束（EOF）
时不打印 `Aborted.`，直接以 `0` 退出。

### `--session-id`

合法 ID 等价于正则 `^[A-Za-z0-9](?:[A-Za-z0-9._-]*[A-Za-z0-9])?$`：首尾必须是 ASCII 字母或数字，
中间只允许字母、数字、`-`、`_`、`.`。非 ASCII 字符会被拒绝。

- 当前项目已有同 ID 会话：直接打开（`--fork` 除外，见上）。
- 没有：打印 `Warning: No project session found with id '<id>'; creating a new session with that id.`，
  然后按该 ID 新建。

### `--fork`

从源会话复制出新会话：

- 新 header 的 `cwd` 是当前 cwd，`parentSession` 指向解析后的源文件路径。
- 源文件中除 header 外的**全部条目**原样复制（保留 id/parentId 与未知字段）；不是只复制当前分支。
- 新会话 ID 由 UUIDv7 生成；给了 `--session-id` 时用指定值。
- 源文件必须存在且至少有一个合法 header；否则报错退出 `1`。

### `--no-session` 与 `--name`

- `--no-session` 的会话 `File:` 显示为 `In-memory`；`/session` 能看到 ID 与统计，但退出即丢失。
  内存会话调用 `/export` 会失败（没有会话文件）。
- `--name` 先做 JS `trim`；只有空白时输出 `Error: --name requires a non-empty value` 并退出 `1`。
  名称以 `session_info` 条目写入会话，最新一条生效。`--name` 会把下一个 token 当作值消费，即使它以
  `-` 开头。

## 会话树、分支与摘要

JSONL 里的条目本身就是一棵树：每次追加都以当前 leaf 为 `parentId`，然后把 leaf 移到新条目。交互界面
可以在这棵树上切换。

### `/tree`

- 打开树选择器；空会话显示 `No entries in session`。
- 目标就是当前 leaf 时显示 `Already at this point`。
- 选中条目并确认后显示 `Navigated to selected point`；聊天区按新分支重放。
- 如果目标是用户消息，且编辑器当前为空，会把该消息原文放回编辑器，方便修改后重发。
- 流式回复中导航会先中止当前回合，并把 steering/follow-up 队列恢复回编辑器。
- 压缩进行中导航会报
  `Wait for the current compaction or tree navigation to finish before navigating the session tree.`

设置 `doubleEscapeAction` 控制编辑器为空时连按两次 `Esc` 的动作：

| 值 | 行为 |
| --- | --- |
| `tree`（默认） | 打开 `/tree` |
| `fork` | 打开 `/fork` 的用户消息选择器 |
| `none` | 不动作 |

树选择器的初始过滤模式由 `treeFilterMode` 决定：

| 值 | 显示内容 |
| --- | --- |
| `default`（默认） | 隐藏 label、context_edit、custom、model_change、thinking_level_change、session_info 等簿记条目 |
| `no-tools` | 在 `default` 基础上再隐藏 tool result |
| `user-only` | 只显示用户消息 |
| `labeled-only` | 只显示有 label 的条目 |
| `all` | 全部条目（`usage` 条目仍不显示） |

选择器里还可以编辑 label、复制选中条目的文本；没有可复制文本时报
`Selected entry has no text to copy`。

### 分支摘要

导航到另一条分支时，默认先询问：

```text
Summarize branch?
  No summary
  Summarize
  Summarize with custom prompt
```

- 选 `No summary`：只切换 leaf，不生成摘要。
- 选 `Summarize`：用当前模型总结被离开的那段分支，生成 `branch_summary` 条目并加入上下文。
- 选 `Summarize with custom prompt`：再输入自定义摘要指令；取消输入会回到三选一询问。
- 摘要生成中按 `Esc` 会中止，显示 `Branch summarization cancelled` 并重新打开树选择器。
- 没有可用模型时导航会报 `No model available for summarization`。

相关设置：

| 键 | 类型 | 默认 | 说明 |
| --- | --- | --- | --- |
| `branchSummary.reserveTokens` | number | `16384` | 摘要时可用的 token 预算 |
| `branchSummary.skipPrompt` | bool | `false` | 为 `true` 时跳过三选一询问，按“不总结”处理 |

注意：只切换 leaf、没有追加任何条目时，新的 leaf 位置不会单独落盘。退出后重新打开该会话，leaf 会回到
文件最后一条条目；想固定分支，继续发送一条消息，或使用 `/clone` 生成独立会话文件。

### `/fork`、`/clone`、`/resume`、`/new`

| 命令 | 位置 | 结果 |
| --- | --- | --- |
| `/fork` | 选中的历史用户消息**之前** | 新建会话；被选消息及其之后的内容不进入新会话；消息原文放回编辑器 |
| `/clone` | 当前 leaf | 新建会话，保留当前状态；编辑器清空；空会话显示 `Nothing to clone yet` |
| `/resume` | 选择磁盘上的会话 | 在当前进程切换到该会话；显示 `Resumed session` |
| `/new` | — | 新建空会话；显示 `✓ New session started` |

`/fork` 没有可选的用户消息时显示 `No messages to fork from`。`/resume` 的会话列表分“当前目录”和
“全部会话”；选中会话记录的 cwd 已不存在时会提示选择新的 cwd，成功显示 `Resumed session in current cwd`。

## 上下文压缩（compaction）

当上下文接近模型窗口时，kiana 会自动压缩；也可以手动压缩。

### 自动压缩

- 触发条件：估算的上下文 token 数 **大于** `contextWindow - reserveTokens`（实现用
  `contextTokens + reserveTokens > contextWindow` 比较）。
- 请求发生上下文溢出（context overflow）时，会尝试一次“压缩 + 重试”；连续失败后提示
  减少上下文或换更大窗口的模型。
- 设置 `compaction.enabled` 为 `false` 可以关闭；`/settings` 中的自动压缩开关写的就是这个键。

### 手动压缩

```text
/compact
/compact 重点保留 API 变更和未完成的测试
```

`/compact` 会先中止当前回合，再开始压缩；后面的文本作为自定义压缩指令。成功后会追加一条
`compaction` 条目，失败通过事件显示：

| 文本 | 含义 |
| --- | --- |
| `Already compacted` | 当前分支末尾已经是压缩条目 |
| `Nothing to compact (session too small)` | 没有可压缩的内容 |
| `Compaction cancelled` | 扩展或用户取消了压缩 |
| `Compaction failed: <原因>` | 压缩过程失败 |

### compaction 设置

| 键 | 类型 | 默认 | 说明 |
| --- | --- | --- | --- |
| `compaction.enabled` | bool | `true` | 是否允许自动压缩 |
| `compaction.reserveTokens` | number | `16384` | 压缩阈值保留的 token |
| `compaction.keepRecentTokens` | number | `20000` | 压缩后保留的最近 token 数 |
| `compaction.modelOverrides` | record | — | 按模型覆盖 `reserveTokens` / `keepRecentTokens` |

`modelOverrides` 的键是精确的 `provider/model-id`，不做 glob；条目里写 `enabled` 会被忽略。
压缩摘要本身使用模型生成，也会消耗 token；摘要一旦写入就是会话的一部分，没有“撤销压缩”命令。

## 自动重试

模型请求遇到可重试错误时，kiana 会自动退避重试；状态指示器显示
`Retrying (<n>/<max>) in <秒>s... (<按键> to cancel)`，默认按键是 `Esc`，按 `Esc` 可取消。

| 键 | 类型 | 默认 | 说明 |
| --- | --- | --- | --- |
| `retry.enabled` | bool | `true` | 总开关 |
| `retry.maxRetries` | number | `3` | 最多自动重试次数 |
| `retry.baseDelayMs` | number | `2000` | 指数退避基数 |
| `retry.maxAgentDelayMs` | number | `60000` | 单次退避上限 |
| `retry.provider.timeoutMs` | number | — | provider 请求超时 |
| `retry.provider.maxRetries` | number | — | provider 适配器内部重试次数 |
| `retry.provider.maxRetryDelayMs` | number | `60000` | provider 内部重试延迟上限 |

第 n 次重试的等待时间约为 `baseDelayMs * 2^(n-1)`，再被 `maxAgentDelayMs` 截断。上下文溢出不走这条
路径，而是先走上面的一次压缩重试；被取消的重试会显示 `Aborted after <n> retry attempt(s)`。

## 连续性（continuity）：执行所有权、checkpoint 与后台任务

continuity 默认**关闭**。它是一组会话级可靠性能力，不是会话保存本身：

- 执行所有权：同一会话同一时间只允许一个存活进程执行。
- 私有 git checkpoint：在写文件的工具执行前备份工作区。
- 后台任务：`bash` 工具可以 `background=true`。
- 验收记录（acceptance）：记录工具执行回执，供验收 criteria 使用。

启用方式（二选一）：

```json
{ "continuity": true }
```

```json
{
  "continuity": {
    "enabled": true,
    "dir": "/custom/state",
    "execution": true,
    "checkpoints": true,
    "background": true,
    "acceptance": true
  }
}
```

环境变量方式：`KIANA_CONTINUITY=1` 启用；`KIANA_CONTINUITY_DIR` 覆盖状态目录；
`KIANA_CONTINUITY_EXECUTION`、`KIANA_CONTINUITY_CHECKPOINTS`、`KIANA_CONTINUITY_BACKGROUND`、
`KIANA_CONTINUITY_ACCEPTANCE` 可分别设为 `0`/`false`/`off` 关闭子功能。**没有 `--continuity`
CLI flag**；`continuity` 也不是 settings schema 的 58 个类型化键之一，而是运行时读取的原始键。

状态目录默认 `<agentDir>/continuity`，子目录为 `executions/`、`checkpoints/`、`background/`。

### 执行所有权

- 每个会话在启动时申请 claim；另一个存活进程持有 claim 时，配置诊断显示
  `execution ownership: session is already being executed by <owner> (pid <pid>)`，启动 run 会报
  `Cannot start a run: session is already being executed by ...`。
- 正常结束时释放 claim；崩溃后下次启动会接管并增加尝试计数，默认最多 10 次；耗尽后报
  `execution_attempts_exhausted: ...`。
- 打开同一会话的多个只读宿主也会申请 claim，因此可能互相阻塞。

### 私有 git checkpoint

- 在 `write`、`edit`、`multiedit`、`apply_patch`、`patch`、`str_replace`、`create_file`、`delete_file`、
  `bash`、`powershell` 等 mutating 工具执行前创建；每个 turn 每个目录最多一次。
- 使用独立 git dir 与 `refs/kiana/checkpoints/<key>`，不碰项目的 `.git`；依赖系统里可用的 `git`。
- 跳过 root/home（及 home 祖先）、`node_modules`、`target`、`.git`、`.kiana`；超过 20,000 个文件或
  256 MiB 时跳过该次快照。
- 恢复只还原“当前内容仍等于 agent 最后一次写入”的文件；用户手改、符号链接、嵌套 repo 等会被跳过。
- **当前实现限制**：没有 `/checkpoint`、`/restore` 斜杠命令，也没有对应 CLI flag；恢复入口只在
  Rust SDK 的 `AgentSession::restore_checkpoint`，TUI 尚未接。

### 后台任务

continuity 的 background 子功能开启后，内建 `bash` 工具会多出一个可选参数 `background`：

```json
{ "command": "cargo build 2>&1", "background": true }
```

任务启动后，工具结果返回：

```text
Background task started: <task-id>
Output file: <continuity 状态目录>/background/<task-id>.output
The task continues after this turn; its terminal status is delivered to this session.
```

- 父回合结束不会取消后台任务；输出文件默认上限 8 MiB，超出部分会截断。
- 终态通知先写入 durable journal；下一轮 run 开始时以 `kiana.background-task` 自定义消息注入
  父会话。若进程在“取走通知、写入 canonical 条目”之间退出，下次会话启动的 sweep 会重放；
  按 `task_id:terminal_seq` 去重，属于 at-least-once 投递。
- 进程崩溃后**没有进程收养/重跑**：生产构造没有配置 resumer，下次 sweep 会把仍在 Running
  的记录终止为 `interrupted/process_lost`。恢复尝试计数上限默认 3，仅在配置了 resumer 时
  才会真正重跑。
- 当前没有列出、查看或取消后台任务的斜杠命令/CLI；取消入口只在 Rust API
  `AgentSession::cancel_background_task`。
- continuity 未启用或 background 子功能关闭时，`background: true` 会报
  `background execution requires continuity (set the continuity setting or KIANA_CONTINUITY=1)`；
  不会静默改成前台执行。

### 验收记录

continuity 开启且 `acceptance` 启用时，会为工具执行保存回执（参数/输出的 SHA-256、状态、序号），
并可对 `file:<path> exists|non-empty|json-valid|csv-columns:...`、`file_written:<path>`、
`tests_passed:<command>` 等 criteria 做判定。当前**没有用户命令查看验收报告**，它主要供 SDK/子代理
运行时使用；没有匹配执行的 criteria 结果为 Unverified，不会静默通过。

## 导出、导入与分享

### `--export`（命令行）

```bash
kiana --export ~/.kiana/agent/sessions/--home-me-proj--/session.jsonl
kiana --export session.jsonl output.html
```

- 读取一个已有会话文件，导出为 HTML，然后退出；不启动会话，也不要求模型可用。
- 成功时 stdout 打印 `Exported to: <路径>`；失败打印 `Error: ...` 并退出 `1`。
- 输出路径是 `--export <file>` 之后的第一个位置参数；省略时默认文件名为
  `kiana-session-<输入文件名去掉 .jsonl>.html`。相对路径相对进程 cwd。
- 导出内容是完整会话文件（所有条目与 leaf），不是只导出当前分支。

### `/export`（交互界面）

```text
/export
/export notes.html
/export branch.jsonl
```

| 参数 | 格式 | 说明 |
| --- | --- | --- |
| 省略 | HTML | 默认文件名 `kiana-session-<当前会话文件名去掉 .jsonl>.html` |
| 以 `.jsonl` 结尾 | JSONL | 导出**当前分支**；条目会按当前 leaf 重新串成线性 `parentId` 链 |
| 其它路径 | HTML | 写到指定路径（相对路径相对 cwd） |

成功后显示 `Session exported to: <路径>`；失败只显示
`Failed to export session: <原因>`，不退出进程。`--no-session` 的内存会话无法导出。

JSONL 导出会写一个新的 header（`version: 3`，不带 `parentSession`），因此它适合分享/归档当前分支，
不保留其它分支和原始 leaf 关系。HTML 则保留完整条目树。

### `/import <path.jsonl>`

- 先弹出确认框 `Import session` / `Replace current session with <path>?`；拒绝则显示 `Import cancelled`。
- 把源 JSONL **复制进当前会话目录**再打开（不是原地读取）。同名文件已存在时追加 `-1`、`-2` 后缀；
  源文件本身就在会话目录时原地导入。
- 复制保留源文件权限。会话记录的 cwd 不存在时会额外询问新的 cwd；拒绝则取消。
- 文件不存在只显示普通错误，不退出进程；其它导入失败按致命错误处理，会记录崩溃并退出。
- 扩展的 `before_switch` 处理器可以取消导入；取消不算错误。

### `/share`

`/share` 把当前会话导出并上传，成功后状态栏显示链接：

| `sessionShareDestination` | 行为 |
| --- | --- |
| `auto`（默认） | 先尝试 Radius；未配置认证时回退私有 GitHub gist |
| `radius` | 只用 Radius；认证缺失时报 `Radius authentication is not configured. Run '/login radius' first.`，不回退 |
| `gist` | 直接用 GitHub gist |

- Radius 通常需要先 `/login radius`（或配置 `RADIUS_API_KEY`）；成功时显示响应里的
  `canonical_url`。
- gist 需要系统安装 `gh` 并已 `gh auth login`；否则报
  `GitHub CLI is not logged in. Run 'gh auth login' first.`。
- gist 创建后，如果配置了 `KIANA_SHARE_VIEWER_URL` 或 `KIANA_API_BASE_URL`，会同时显示查看器链接；
  否则显示 gist 链接并提示
  `Share viewer URL is not configured (set KIANA_SHARE_VIEWER_URL or KIANA_API_BASE_URL).`
- 分享使用临时目录，结束后递归删除；不会修改当前会话。

### `/session` 与 `/name`

`/session` 显示会话名、文件路径（内存会话为 `In-memory`）、会话 ID、消息与工具调用数、token
用量、cache warming 状态，以及有数据时的成本明细；它只是快照，重跑命令才会刷新。

`/name <name>` 写入显示名并显示 `Session name set: <name>`；`/name` 不带参数时显示当前名称或
`Usage: /name <name>`。名称中的换行会被合并为空格，首尾空白会被 trim。

## 参考：会话相关设置

| 键 | 类型 | 默认 | 作用 |
| --- | --- | --- | --- |
| `sessionDir` | string | — | 会话存储目录；被 `--session-dir` / `KIANA_CODING_AGENT_SESSION_DIR` 覆盖 |
| `sessionShareDestination` | `auto`/`gist`/`radius` | `auto` | `/share` 目的地 |
| `compaction.enabled` | bool | `true` | 自动压缩开关 |
| `compaction.reserveTokens` | number | `16384` | 压缩阈值保留 token |
| `compaction.keepRecentTokens` | number | `20000` | 压缩后保留的最近 token |
| `compaction.modelOverrides` | record | — | 按 `provider/model-id` 覆盖 token 设置 |
| `branchSummary.reserveTokens` | number | `16384` | 分支摘要 token 预算 |
| `branchSummary.skipPrompt` | bool | `false` | 跳过摘要询问，默认不总结 |
| `retry.enabled` | bool | `true` | 自动重试开关 |
| `retry.maxRetries` | number | `3` | 最大自动重试次数 |
| `retry.baseDelayMs` | number | `2000` | 指数退避基数 |
| `retry.maxAgentDelayMs` | number | `60000` | 单次退避上限 |
| `retry.provider.*` | object | 见上表 | provider 请求超时与内部重试 |
| `doubleEscapeAction` | `tree`/`fork`/`none` | `tree` | 空编辑器双击 Esc 的动作 |
| `treeFilterMode` | 见上文 | `default` | `/tree` 初始过滤模式 |
| `continuity` | bool 或对象 | 关闭 | continuity 总开关与子功能（原始键，不在 58 个类型化键内） |

settings 的层级、项目信任与写入方式见 [设置（settings）](settings.md)。

## 限制与排查

### 当前限制

- **平台**：只验证 Linux x86_64；会话路径编码、checkpoint 的 `flock`/`O_NOFOLLOW` 等行为在其他平台未实测。
- **continuity 默认关闭**；没有 `--continuity` flag，必须用 settings `continuity` 或 `KIANA_CONTINUITY=1`。
- **checkpoint 没有用户入口**：只有写工具执行前自动快照；TUI/CLI 不能列出或恢复 checkpoint，恢复仅在
  Rust `AgentSession::restore_checkpoint`。
- **后台任务没有用户入口**：只能通过模型调用 `bash` 的 `background: true` 触发；没有列表/取消命令。
  终态通知可以在下次启动时重放，但运行中的任务崩溃后不会被收养或重跑。
- **会话文件迁移会重写原文件**：v1/v2 → v3 自动迁移没有备份。
- **分支位置不持久**：只切换 leaf 而不追加条目时，重开会话 leaf 回到文件最后一条条目。
- **内存会话不能导出**：`--no-session` 下 `/export` 会失败。
- **`/import` 需要交互确认**；在 print/RPC 模式没有这个斜杠命令。
- **`/share` 依赖外部条件**：Radius 登录或 `gh` 登录；查看器链接还需要 `KIANA_SHARE_VIEWER_URL` 或
  `KIANA_API_BASE_URL`。
- **没有 `/help` 斜杠命令**：查看帮助用 `kiana --help` 或 `kiana <command> --help`。

### 常见错误

| 现象 | 原因 | 处理 |
| --- | --- | --- |
| `No session found matching '<x>'` | `--session`/`--fork` 的 ID 或路径不存在 | 用 `kiana -r` 看可用会话，或检查路径 |
| `Session already exists with id '<id>'` | `--fork --session-id` 指定的 ID 在当前项目已存在 | 换一个 ID，或先删除旧会话文件 |
| `No session selected` | `--resume` 选择器被取消 | 正常退出（`0`），重新运行并选择 |
| `Session found in different project` | `--session` 命中了别的项目的会话 | 按提示 fork，或先 `cd` 到那个项目 |
| `Already at this point` | `/tree` 选中了当前 leaf | 选其他条目 |
| `Nothing to compact (session too small)` | 没有足够历史可压缩 | 继续对话后再试 |
| `Already compacted` | 分支末尾已是压缩条目 | 无需重复压缩 |
| `Wait for the current compaction...` | 压缩/树导航进行中再次导航 | 等待当前操作结束 |
| `background execution requires continuity...` | 未开启 continuity/background | 设置 `continuity` 或 `KIANA_CONTINUITY=1` |
| `session is already being executed by ...` | 另一个存活进程持有该会话 claim | 关闭另一个进程，或等待其释放 |
| `Failed to export session: Cannot export in-memory session to HTML` | 对 `--no-session` 会话执行 `/export` | 去掉 `--no-session`，或先保存会话 |
| `GitHub CLI is not logged in...` | `/share` 走到 gist 分支但 `gh` 未登录 | `gh auth login`，或配置 Radius |
| `Radius authentication is not configured...` | `sessionShareDestination=radius` 但未登录 | `/login radius`，或改为 `auto`/`gist` |
| `Share viewer URL is not configured...` | 缺少查看器 URL | 设置 `KIANA_SHARE_VIEWER_URL` 或 `KIANA_API_BASE_URL` |

更多启动、权限和网络排查见 [故障排查](troubleshooting.md)。

## 相关页面

- [CLI 参考](cli.md)：完整 flag、子命令与退出码
- [交互模式](interactive-mode.md)：斜杠命令、键位与界面
- [设置（settings）](settings.md)：settings 分层、项目信任与写入方式
- [模型与 provider](models-and-providers.md)：模型选择、thinking 等级与认证命令
- [权限与审批](permissions.md)：工具审批与权限模式
- [故障排查](troubleshooting.md)：通用启动与运行问题
