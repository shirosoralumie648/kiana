# CLI 参考

本页讲什么：`kiana` 命令行的完整参考。先说明启动模式和最常用命令，再按会话、模型、工具、权限、
扩展资源、输出模式、daemon、诊断分组列出 flag，然后给出 `install` / `remove` / `update` /
`list` / `config` / `auth` / `mcp` 子命令、退出码和 stdin/stdout 约定。

第一次使用请先看 [快速开始](getting-started.md)。

## 常用命令

```bash
kiana                                   # 交互式会话
kiana -p "List all .ts files in src/"   # 一次性运行
kiana -c "What did we discuss?"         # 继续上一个会话
kiana -r                                # 选择要恢复的会话
kiana --model sonnet:high "Solve this"  # 指定模型和思考级别
kiana --permission-mode read-only -p "Review src/"
kiana auth check --provider anthropic --no-refresh
kiana --list-models
kiana mcp list
kiana --serve --serve-token "$TOKEN"    # daemon 事件服务
```

主程序用法：

```text
kiana [options] [--] [@files...] [messages...]
```

## 启动方式

### 运行模式如何选择

| 模式 | 进入条件 | stdout 内容 |
| --- | --- | --- |
| Interactive（交互） | stdin 和 stdout 都是 TTY，且没有 `-p` / `--mode json` / `--mode rpc` | TUI 界面 |
| Print（文本一次性） | 给了 `-p` / `--print`，或 stdin/stdout 任一不是 TTY，或检测到管道 stdin | 最终 assistant 文本 |
| JSON | `--mode json` | 每行一个 JSON 对象 |
| RPC | `--mode rpc`，或使用 `kiana-rpc` 二进制 | RPC 协议字节 |

补充约定：

- `--mode text` 只是“输出模式为文本”，不会强制非交互；是否为交互仍取决于 TTY 与 `-p`。
- 管道 stdin 会被读取；如果此时原本是交互模式，会自动降级为 Print。
- `kiana-rpc` 等价于在参数前追加 `--mode rpc`。用户再传 `--mode text|json` 会覆盖它；需要 RPC 的
  调用方不应再传 `--mode`。`kiana-rpc --help` 与 `kiana --help` 相同。

### 启动相关选项

| Flag | 取值 | 说明 |
| --- | --- | --- |
| `--help`, `-h` | 无 | 打印帮助并退出 `0`；子命令用 `kiana <command> --help` |
| `--version`, `-v` | 无 | 打印 `1.4.0` 并退出 `0`（版本差异见 [快速开始](getting-started.md#版本说明)） |
| `--tui-mode <mode>` | `fullscreen`（默认）/ `regular` | 本次运行的 TUI 模式；未给时读 settings `tuiMode` |
| `--` | 无 | 结束选项解析，其后 token 全部按消息 / `@file` 处理 |

## 会话

| Flag | 取值 | 说明 |
| --- | --- | --- |
| `--continue`, `-c` | 无 | 继续当前目录最近的会话 |
| `--resume`, `-r` | 无 | 打开会话选择器；未选择时打印 `No session selected` 并以 `0` 退出 |
| `--session <path\|id>` | 会话文件路径或部分 UUID | 打开指定会话 |
| `--session-id <id>` | 精确的项目会话 ID | 已存在则打开；不存在时打印 `Warning` 并以该 ID 新建；只允许字母数字与 `-` `_` `.`，且首尾必须是字母数字 |
| `--fork <path\|id>` | 会话文件路径或部分 UUID | 复制源会话为新会话 |
| `--session-dir <dir>` | 目录 | 覆盖会话存储目录 |
| `--no-session` | 无 | 不保存会话（临时） |
| `--name`, `-n <name>` | 名称 | 设置会话显示名；空值报 `--name requires a non-empty value` 并退出 `1` |
| `--export <file> [output]` | 输入会话文件；可选输出路径 | 导出 HTML 后退出；输出路径取自第一个位置消息参数（`@file` 不计入） |

会话目录优先级：

```text
--session-dir > KIANA_CODING_AGENT_SESSION_DIR > settings.sessionDir > 默认 <agentDir>/sessions
```

互斥与冲突（都会在启动前报错并退出 `1`）：

- `--fork` 不能与 `--session`、`--continue`、`--resume`、`--no-session` 同时使用。
- `--session-id` 不能与 `--session`、`--continue`、`--resume` 同时使用。
- 非法 `--session-id` 会报：只允许字母数字、`-`、`_`、`.`，且必须以字母数字开头和结尾。

其他解析边界：

- `--name` 缺值时报错；它会把下一个 token 当作值消费，即使这个 token 以 `-` 开头。
- `--fork` 可以与 `--session-id` 一起用；如果目标 ID 在当前项目已存在，会报
  `Session already exists with id '<id>'` 并退出 `1`。
- `--export` 不要求模型可用；导出成功时 stdout 打印 `Exported to: <路径>`。
- `--export` 的输出路径取自**全局第一个位置消息参数**，不是限定在 `--export <file>` 之后；`@file` 走 file_args、不计入 messages。因此 `kiana out.html --export in.jsonl` 也会把 `out.html` 当作输出路径，常规写法是紧跟 `--export <file>` 再写一个路径。
- 省略 `--export` 的输出路径时，文件名由导出模块生成（本页不固化具体命名，未验证）。

示例：

```bash
kiana --continue "上次改到哪了？"
kiana --resume
kiana --session ~/.kiana/agent/sessions/--path--/session.jsonl
kiana --session-id refactor-auth --name "Refactor auth module"
kiana --fork abc123 --name "Try another approach"
kiana --export session.jsonl output.html
```

## 模型

| Flag | 取值 | 说明 |
| --- | --- | --- |
| `--provider <name>` | provider id | 与 `--model` 配合，缩小模型搜索范围；单独使用会报 `--provider requires --model` |
| `--model <pattern>` | 模型 pattern 或 ID | 支持 `provider/id` 与可选 `:<thinking>`，例如 `sonnet:high`、`openai/gpt-4o` |
| `--api-key <key>` | 字符串 | 本次运行的 API key；默认从 provider 环境变量读取，不写入磁盘 |
| `--thinking <level>` | `off` / `minimal` / `low` / `medium` / `high` / `xhigh` / `max` | 设置思考级别；非法值只打印 `Warning`，不中止启动 |
| `--models <patterns>` | 逗号分隔的 pattern | 定义 Ctrl+P 可循环的模型范围；支持 glob 与模糊匹配 |
| `--list-models [search]` | 可选搜索串 | 列出可用模型后退出；搜索串不能以 `-` 或 `@` 开头，否则退化为无搜索 |

未指定 `--model` 时，初始模型来自 settings 的 `defaultProvider` / `defaultModel`，或 `--models`
范围内的默认模型/第一项；显式 `--thinking` 最后覆盖以上来源的思考级别。`--api-key` 要求先选出
模型；没有 `--model` / `--models` 时会报 `--api-key requires a model ...` 并退出 `1`。

示例：

```bash
kiana --model sonnet:high "Solve this problem"
kiana --model openai/gpt-4o "Refactor this module"
kiana --provider openai --model gpt-4o-mini
kiana --models "github-copilot/*"
kiana --models sonnet:high,haiku:low
kiana --thinking high
kiana --list-models sonnet
```

## 工具

| Flag | 取值 | 说明 |
| --- | --- | --- |
| `--no-tools`, `-nt` | 无 | 禁用所有工具（内建与扩展） |
| `--no-builtin-tools`, `-nbt` | 无 | 只禁用内建工具，保留扩展/自定义工具 |
| `--tools`, `-t <tools>` | 逗号分隔的 allowlist / pattern | 只启用匹配的工具；MCP 工具默认保留，除非条目以 `mcp__` 开头；`+name` / `-name` 用于在默认集合上增删 |
| `--exclude-tools`, `-xt <tools>` | 逗号分隔的 denylist / pattern | 禁用匹配的工具；作用于所有工具，包括 MCP |
| `--no-mcp` | 无 | 关闭内建 MCP 支持：不连接服务器，也不暴露 MCP 工具 |

内建工具名（`kiana --help` 的 Built-in Tool Names）：

| 名称 | 作用 | 默认 |
| --- | --- | --- |
| `read` | 读文件 | 启用 |
| `bash` | 执行 bash 命令 | 启用 |
| `powershell` | 执行 PowerShell 命令（Windows） | Windows 工具；默认集合不含，需显式启用 |
| `edit` | 查找/替换编辑 | 启用 |
| `write` | 写文件（创建/覆盖） | 启用 |
| `grep` | 搜索文件内容 | 只读，默认不启用 |
| `find` | 按 glob 查找文件 | 只读，默认不启用 |
| `ls` | 列出目录 | 只读，默认不启用 |

说明：

- `--tools read,grep,find,ls` 把内建工具限制为这四个，并过滤未列出的扩展工具；MCP 工具仍会保留
  （除非 allowlist 里出现 `mcp__` 条目）。不要把它当成完整的只读权限边界，真正禁止写入要用
  `--permission-mode read-only`。
- `--tools` 的普通名单与 `+name` / `-name` 修饰符不能混用；`+name` / `-name` 只接受精确工具名，
  不接受 `*` 模式。
- `codemode` 与 `tool_search` 是内建扩展工具，默认不激活；用 `--tools +codemode` 或
  `--tools tool_search` 显式启用。
- `web_fetch` 默认激活；`web_search` 只有在注册时检测到 `BRAVE_API_KEY`、`BING_API_KEY` 或
  `GOOGLE_CSE_API_KEY` + `GOOGLE_CSE_CX` 才默认激活。两者都可用 `--tools` / `--exclude-tools`
  覆盖。
- `--no-mcp` 只关闭 MCP；`--no-extensions` 会关闭包括 MCP 在内的所有内建扩展。

示例：

```bash
kiana --tools read,grep,find,ls -p "Review the code in src/"
kiana --tools +codemode
kiana --tools read,bash,codemode,'mcp__radius__*'
kiana --exclude-tools bash
kiana --no-mcp
```

## 权限

| Flag | 取值 | 说明 |
| --- | --- | --- |
| `--permission-mode <mode>` | `read-only` / `default` / `accept-edits` / `bypass` | 覆盖本次运行的权限模式 |
| `--allow <rule>` | 规则文本 | 追加允许规则；可重复 |
| `--deny <rule>` | 规则文本 | 追加拒绝规则；可重复；deny 永远优先 |
| `--add-dir <dir>` | 目录 | 追加可写工作区根；可重复 |
| `--approve`, `-a` | 无 | 本次运行信任项目本地文件 |
| `--no-approve`, `-na` | 无 | 本次运行忽略项目本地文件 |

模式行为：

| 模式 | 行为 |
| --- | --- |
| `read-only` | 读允许；写、执行、网络、MCP/其它全部拒绝 |
| `default` | 读允许；其余询问 |
| `accept-edits` | 工作区内写允许；区外写、执行、网络、MCP/其它询问 |
| `bypass` | 全部放行（仍记录决策；灾难性命令也放行） |

规则语法（`--allow` / `--deny` 相同）：

| 写法 | 匹配对象 |
| --- | --- |
| `tool:<pattern>` 或裸工具名 | 工具名；`*` 用于开头/结尾通配 |
| `command:<words>` 或含空白的文本 | 命令前缀（只对简单命令生效）；以 `*` 结尾时是通配规则 |
| `path:<prefix>` 或绝对路径形态 | 路径前缀；`~`、`$`、`./`、Windows 盘符与 UNC 都识别 |
| `tool:` / `command:` / `path:` 的 `=` 形式 | 同上 |

示例：

```bash
kiana --permission-mode read-only -p "Review src/"
kiana --permission-mode accept-edits
kiana --permission-mode bypass
kiana --allow 'command:git status'
kiana --deny 'path:~/secrets'
kiana --allow 'tool:mcp__*'
kiana --add-dir ../shared-lib
```

优先级与启用条件（当前实现）：

1. 命令行 `--permission-mode` 覆盖 settings 和 env。
2. settings 的 `permissions` 对象优先于环境变量；`permissions.mode` / `permissions.defaultMode`
   决定该对象里的模式。只要对象生效（包含规则或目录），环境变量就不再参与。
3. 环境变量 `KIANA_PERMISSIONS` / `KIANA_PERMISSION_MODE`；两者都设置时按数组顺序先命中者
   优先（即 `KIANA_PERMISSIONS` 优先）。
4. 三个来源都没有时不会创建权限引擎，工具按引入权限层之前的路径直接执行。

其他行为：

- 只给 `--allow` / `--deny` 而未给 `--permission-mode` 时，会以 `default` 模式创建权限引擎；
  未匹配的操作仍会询问（非交互下按拒绝处理）。
- CLI 的 `--allow` / `--deny` 只影响当前进程，不写回 `settings.json`。
- print/json 模式没有审批 UI，任何 `Ask` 都按拒绝处理；没有 `--yes` 自动批准。
- settings 的 `permissions.disableBypassPermissionsMode` 可以禁止 `bypass`；managed 策略还可以
  限制可用的 allow 规则和 MCP 服务器列表。

完整的审批作用域、持久化与沙箱行为见 [权限与审批](permissions.md)；schema 外的 `permissions`
字段见 [设置](settings.md)。

## 扩展资源

| Flag | 取值 | 说明 |
| --- | --- | --- |
| `--extension`, `-e <path>` | 扩展文件或 `builtin:<name>` | 追加扩展；可重复 |
| `--no-extensions`, `-ne` | 无 | 关闭扩展发现与内建扩展；显式 `-e` 路径仍生效 |
| `--skill <path>` | skill 文件或目录 | 追加 skill；可重复 |
| `--no-skills`, `-ns` | 无 | 关闭 skills 发现与加载 |
| `--prompt-template <path>` | prompt 模板文件或目录 | 追加 prompt template；可重复 |
| `--no-prompt-templates`, `-np` | 无 | 关闭 prompt template 发现与加载 |
| `--theme <path>` | 主题文件或目录 | 追加主题；可重复 |
| `--use-theme <name[/name]>` | 主题名 | 本次运行的初始交互主题 |
| `--no-themes` | 无 | 关闭主题发现与加载 |
| `--no-context-files`, `-nc` | 无 | 关闭 `AGENTS.md` 与 `CLAUDE.md` 的发现与加载 |
| `--system-prompt <text>` | 文本或已存在文件的内容 | 替换默认 system prompt |
| `--append-system-prompt <text>` | 文本或已存在文件的内容 | 追加到 system prompt；可重复 |

说明：

- 内建扩展名为 `llama.cpp`、`codemode`、`tool-search`、`mcp`；可以用 `builtin:<name>` 显式加载。
  `--no-mcp` 只关闭其中的 MCP，`--no-extensions` 会关闭全部内建扩展。
- 不加 flag 时的资源发现目录：

| 资源 | 全局 | 项目（受信时） |
| --- | --- | --- |
| extensions | `<agentDir>/extensions` | `<cwd>/.kiana/extensions` |
| skills | `<agentDir>/skills` | `<cwd>/.kiana/skills` |
| prompts | `<agentDir>/prompts` | `<cwd>/.kiana/prompts` |
| themes | `<agentDir>/themes` | `<cwd>/.kiana/themes` |

- `--system-prompt` 与 `--append-system-prompt` 的每一项都按“指向已存在的文件则读取内容，否则按
  字面文本”解析。显式 `--system-prompt` 优先于自动发现的受信项目 `.kiana/SYSTEM.md` /
  `<agentDir>/SYSTEM.md`；没有显式给 `--append-system-prompt` 时，还会自动读取受信项目的
  `.kiana/APPEND_SYSTEM.md` 或全局 `<agentDir>/APPEND_SYSTEM.md`（存在时）。
- 资源路径也可以写进 settings 的 `extensions` / `skills` / `prompts` / `themes` / `packages`
  键；详见 [设置](settings.md)。

示例：

```bash
kiana -e ./my-extension.ts
kiana -e builtin:codemode
kiana --skill ./skills/review
kiana --prompt-template ./prompts/release.md
kiana --theme ./themes/night.json --use-theme night
kiana --no-context-files
kiana --append-system-prompt "回答时先给结论。"
kiana --append-system-prompt ./APPEND_SYSTEM.md
```

## 输出模式

| Flag | 取值 | 说明 |
| --- | --- | --- |
| `--print`, `-p` | 无 | 非交互模式：处理 prompt 后退出 |
| `--mode <mode>` | `text`（默认）/ `json` / `rpc` | 输出模式 |
| `--export <file> [output]` | 输入会话文件；可选输出路径 | 导出 HTML 后退出；不进入会话 |

### text

- stdout 只写最后一条 assistant 消息的 text 块，每块后追加换行。
- 诊断、警告、错误写 stderr。
- 最后一条 assistant 的 `stopReason` 为 `error` / `aborted` 时退出 `1`；`length` 或本地预算 cap
  也会在写出文本后退出 `1`。

### json

- stdout 是 JSON Lines：先写一行原始会话头对象，再写完整事件流（每行一个 JSON 对象）。
- 诊断写 stderr。
- 退出码只反映 prompt 执行本身是否失败：assistant 以 `error` 结束仍可能是 `0`，需要读取事件里的
  `stopReason` / 错误信息。

### rpc

- stdout 独占 RPC 协议字节；普通输出改走 stderr。
- `@file` 参数在 RPC 模式会被拒绝：`Error: @file arguments are not supported in RPC mode`，
  退出 `1`。
- 外部客户端可以使用 `kiana-rpc` 强制以 RPC 启动；本页不展开 RPC 协议帧。

### export

`--export` 在创建会话之前处理，因此不要求模型可用：

```bash
kiana --export ~/.kiana/agent/sessions/--path--/session.jsonl
kiana --export session.jsonl output.html
```

成功时 stdout 打印 `Exported to: <路径>` 并退出 `0`；失败退出 `1`。

## daemon

| Flag | 取值 | 说明 |
| --- | --- | --- |
| `--serve` | 无 | 运行 daemon 事件服务（SSE/WebSocket/REST），收到 SIGINT 后退出 |
| `--serve-addr <host:port>` | 地址 | 默认 `127.0.0.1:0`（系统分配端口） |
| `--serve-token <token>` | 字符串 | 覆盖自动生成的 bearer token；空串视为未提供 |

启动：

```bash
kiana --serve --serve-token my-token
```

stderr 会打印监听地址、token 和端点列表。端点：

| 端点 | 说明 |
| --- | --- |
| `GET /events` | SSE；带 `Upgrade: websocket` 时为 WebSocket |
| `GET /events/latest?limit=N` | 最新一页事件，`since` 可作为续传游标 |
| `GET /healthz` | 健康检查，免鉴权，返回 `ok` |

`/events` 与 `/events/latest` 需要 `Authorization: Bearer <token>`，或浏览器
`EventSource`/WebSocket 使用的 `?token=<token>`。`/healthz` 有意免鉴权。

```bash
ADDR=http://127.0.0.1:41365
curl -s "$ADDR/healthz"                                  # ok
curl -s -H "Authorization: Bearer my-token" "$ADDR/events/latest"
```

限制：

- `--serve` 在非交互路径上会先做“必须有可用模型”的检查；没有模型时会在进入 daemon 分支前退出
  `1`（服务管理器或重定向 stdin/stdout 启动属于这一类）。
- token 只打印到 stderr，没有轮换或持久化；默认只监听 loopback。
- 非 loopback 部署需要自己在前面加 TLS 与认证；daemon 没有内建的多用户鉴权。
- 事件 replay 有界（4096），长时间断线可能出现 gap。

## 诊断

| Flag | 说明 |
| --- | --- |
| `--verbose` | 强制 verbose 启动，覆盖 settings 的 `quietStartup` |
| `--offline` | 关闭启动期网络操作，等价于 `KIANA_OFFLINE=1` |

诊断约定：

- 参数错误、运行时错误和警告分别以 `Error:` / `Warning:` 前缀写 stderr。
- `--help`、`--version`、`--list-models`、`--export` 的元数据输出保留在 stdout。
- 非交互模式的普通文本会被改道 stderr，stdout 留给协议/最终回答。
- `--offline` 关闭启动期与后台网络操作（包括模型目录刷新）；显式命令如 `kiana update --models`
  仍可打开网络。web 工具等运行期网络访问仍取决于权限、沙箱与网络环境。

### 代码接受但 `--help` 未列出的选项

以下选项能在源码中核对（`crates/kiana-coding-agent/src/cli/args.rs`），但**没有**出现在
`kiana --help` 中，属于内部/非稳定接口，不建议写进脚本：

| Flag | 取值 | 效果 |
| --- | --- | --- |
| `--max-turns <n>` | 正整数 | 覆盖 `settings.budget.maxTurns` |
| `--max-tokens <n>` | 正整数 | 覆盖累计 token 预算 |
| `--max-cost-usd <usd>` | 正数 | 覆盖累计费用预算 |
| `--max-wall-clock-ms <ms>` | 正整数 | 覆盖墙钟预算 |
| `--no-loop-guard` | 无 | 关闭 loop guard |
| `--loop-guard-stop` | 无 | loop guard 命中即硬停 |

## 子命令

### `kiana install`

```text
kiana install <source> [-l] [--approve|--no-approve]
```

安装包并把 source 写入 settings。

| 选项 | 说明 |
| --- | --- |
| `-l`, `--local` | 写入项目 settings（`.kiana/settings.json`），而不是全局 settings |
| `--approve`, `-a` | 本次命令信任项目本地文件 |
| `--no-approve`, `-na` | 本次命令忽略项目本地文件 |

支持的 source 形式：

```text
npm:@foo/bar
git:github.com/user/repo
git:git@github.com:user/repo
https://github.com/user/repo
ssh://git@github.com/user/repo
./local/path
```

示例：

```bash
kiana install npm:@foo/bar
kiana install git:github.com/user/repo
kiana install ./local/path
kiana install npm:@foo/bar -l
```

缺少 source 时打印 `Missing install source.` 和用法，退出 `1`；安装失败退出 `1`。

### `kiana remove` / `kiana uninstall`

```text
kiana remove <source> [-l] [--approve|--no-approve]
kiana uninstall <source> [-l]
```

从 settings 移除包及其 source。`uninstall` 是 `remove` 的别名。

```bash
kiana remove npm:@foo/bar
kiana uninstall npm:@foo/bar
kiana remove npm:@foo/bar -l
```

### `kiana update`

```text
kiana update [source|self|kiana] [--self|--extensions|--models|--all]
             [--extension <source>] [--approve|--no-approve] [--force]
```

更新 kiana、已安装包或模型目录。

| 选项 / 位置参数 | 说明 |
| --- | --- |
| 无参数 / `self` / `kiana` | 只更新 kiana（默认） |
| `<source>` | 只更新指定包 |
| `--self` | 只更新 kiana |
| `--extensions` | 只更新已安装包 |
| `--models` | 只刷新模型目录 |
| `--all` | 更新 kiana 和已安装包（不包含模型目录） |
| `--extension <source>` | 只更新一个包 |
| `--force` | 即使当前已是最新版本也重新安装 kiana |
| `--approve`, `-a` | 本次命令信任项目本地文件 |
| `--no-approve`, `-na` | 本次命令忽略项目本地文件 |

目标之间互斥；例如 `--all` 与具体 source、`--models` 与 source、`--extension` 与 `--self`
混用会报错并退出 `1`。

```bash
kiana update                    # 只更新 kiana
kiana update --all              # 更新 kiana 和所有已安装包
kiana update --models           # 只刷新模型目录
kiana update npm:@foo/bar       # 更新指定包
kiana update --extension npm:@foo/bar
kiana update --self --force     # 强制重装 kiana
```

### `kiana list`

```text
kiana list [--approve|--no-approve]
```

列出用户与项目 settings 中安装的包：

```bash
kiana list
kiana list --approve
```

没有安装包时输出 `No packages installed.`。

### `kiana config`

```text
kiana config [-l] [--approve|--no-approve]
```

打开资源启用/禁用 TUI。不带 `-l` 时从全局 settings（`~/.kiana/agent/settings.json`）开始；
在 TUI 里按 `Tab` 在全局和项目本地之间切换。

```bash
kiana config
kiana config -l
```

`-l` 编辑项目覆盖（`.kiana/settings.json`）；此时 `--approve` / `--no-approve` 控制项目本地文件
是否受信。

### `kiana auth`

```text
kiana auth print-api-key [--provider <provider>] [--model <model>]
kiana auth print-bearer-token [--provider <provider>] [--model <model>] [--min-expiry <duration>]
kiana auth check [--provider <provider>] [--model <model>] [--json] [--credentials] [--no-refresh]
```

共同约束：至少给出 `--provider` 或 `--model` 之一。裸 `kiana auth` 打印帮助并退出 `0`。

| 子命令 | 说明 |
| --- | --- |
| `print-api-key` | 打印匹配 provider 的 API key |
| `print-bearer-token` | 打印 OAuth bearer token；过期会刷新 |
| `check` | 检查 provider 是否就绪；默认刷新过期 OAuth，`--no-refresh` 禁止刷新 |

`print-bearer-token` 的 `--min-expiry` 取值形如 `<数字><ms|s|m|h>`（例如 `30m`、`1h`），默认
`30m`。`--json` / `--credentials` / `--no-refresh` 只允许用于 `check`。

`check` 的状态与退出码：

| 输出 | 含义 | 退出码 |
| --- | --- | --- |
| `ready` | 找到可用凭据 | `0` |
| `not_ready` | provider 未注册或没有配置凭据 | `1` |
| `invalid` | 运行时/读取失败，无法判断 | `2` |

`check` 的输出默认只有状态词；`--credentials` 额外打印凭据，`--json` 输出对象。参数解析失败
（例如缺少 provider/model）也按 `invalid` 处理，退出 `2`。

```bash
kiana auth print-api-key --provider anthropic
kiana auth print-bearer-token --provider anthropic --min-expiry 1h
kiana auth check --provider anthropic --no-refresh
kiana auth check --provider anthropic --no-refresh --json
kiana auth check --provider anthropic --no-refresh --credentials
```

### `kiana mcp`

```text
kiana mcp add <server> [options] -- <command> [args...]
kiana mcp add <server> [options] --url <url>
kiana mcp remove <server> [-l]
kiana mcp list [--json]
kiana mcp login <server> [--timeout <seconds>]
kiana mcp logout <server>
```

裸 `kiana mcp` 打印帮助并退出 `0`。

| 子命令 | 说明 |
| --- | --- |
| `add <server>` | 新增或替换 `mcp.json` 中的服务器 |
| `remove <server>` | 从 `mcp.json` 移除服务器 |
| `list` | 显示状态、工具与错误；有失败时退出 `1` |
| `login <server>` | 通过浏览器完成 OAuth 登录 |
| `logout <server>` | 删除已保存的 OAuth 凭证 |

`add` / `remove` 共用：

| 选项 | 说明 |
| --- | --- |
| `-l`, `--local` | 使用当前项目的 `.kiana/mcp.json`，而不是全局文件 |

`add` 专用：

| 选项 | 说明 |
| --- | --- |
| `--url <url>` | Streamable HTTP 服务器 URL（代替命令） |
| `--env <KEY=VALUE>` | stdio 服务器的环境变量；可重复 |
| `--cwd <dir>` | stdio 服务器的工作目录 |
| `--header <KEY=VALUE>` | HTTP header；可重复 |
| `--bearer-token-env-var <NAME>` | 发送 `Authorization: Bearer ${NAME}` |
| `--oauth-client-id <id>` | 预注册的 OAuth client id |
| `--oauth-client-secret <secret>` | OAuth client secret（可以是 `${NAME}` 或 `!command`） |
| `--oauth-callback-port <port>` | 固定 OAuth 回调端口 |
| `--oauth-client-name <name>` | 向 OAuth 服务器注册时发送的客户端名 |
| `--exposure <mode>` | `codemode`（默认）/ `deferred` / `direct` / `hidden` |
| `--description <text>` | 写入 system prompt 的服务器说明 |

`add` 必须恰好提供一种传输：`--url <url>` 或 `-- <command> [args...]`。`--env` / `--cwd` 只适用于
stdio 服务器；`--header`、`--bearer-token-env-var` 和全部 `--oauth-*` 只适用于 `--url` 的 HTTP
服务器，混用会报错并退出 `1`。

`mcp list` 的 `--json` 顶层字段是 `servers` 和 `errors`；空配置时输出：

```json
{"servers":[],"errors":[]}
```

`mcp login` 的 `--timeout` 是整次登录的时限，默认 `300` 秒。

示例：

```bash
kiana mcp add myserver -- /path/to/mcp-server /tmp/data
kiana mcp add myhttp --url https://example.com/mcp
kiana mcp add myserver -l -- /path/to/mcp-server
kiana mcp list
kiana mcp list --json
kiana mcp login myserver --timeout 120
kiana mcp logout myserver
kiana mcp remove myserver
```

读取位置：全局 `<agentDir>/mcp.json`（默认 `~/.kiana/agent/mcp.json`）；项目受信时再读
`<cwd>/.kiana/mcp.json`。`-l` 的 add/remove 直接写项目文件。

## 退出码

| 退出码 | 典型场景 |
| --- | --- |
| `0` | 成功；`--help` / `--version` / `--list-models` 成功；`--export` 成功；`auth check` 为 `ready`；daemon 收到 SIGINT 正常退出 |
| `1` | 用法错误；缺少安装 source；非交互模式没有可用模型；`auth check` 为 `not_ready`；`auth print-*` 取不到凭据；print 模式的请求错误/中止/截断/预算耗尽；会话错误；包安装或更新失败；`mcp list` 有服务器失败；`--export` 失败 |
| `2` | `auth check` 为 `invalid`，或 auth 参数无法解析 |
| `129` | Print/JSON 模式收到 SIGHUP（Unix） |
| `143` | Print/JSON 模式收到 SIGTERM（Unix） |
| `101` | 未预期的 Rust panic；不属于稳定契约 |

说明：

- Print/JSON 模式的信号处理只覆盖 SIGHUP/SIGTERM，退出码固定为 `129`/`143`，信号路径不等待
  输出队列冲刷。
- `--mode json` 的退出码只看 prompt 是否抛错；assistant 以 `error` 结束可能仍是 `0`，需要看事件。
- `kiana mcp list` 在任一服务器连接失败时退出 `1`（即使部分服务器正常）。
- `kiana update`、`kiana install`、`kiana remove`、`kiana config` 成功为 `0`，失败为 `1`。

## 标准输入/输出约定

### stdin

- stdin 不是 TTY 时会被读尽并 `trim()`；空内容按没有输入处理。
- 初始消息的拼接顺序固定为：管道 stdin → `@file` 展开文本 → 第一条位置消息。三者用空字符串
  直接拼接，没有自动分隔符，需要分隔时请自己加换行。
- 管道 stdin 会把交互模式降级为 Print。
- `@file` 参数在非 RPC 模式下展开为文件内容；图片文件作为图片附件。RPC 模式拒绝 `@file`。
- `--` 之后的所有 token 都按消息或 `@file` 处理，适合以 `-` 开头的 prompt。

### stdout

- Interactive 模式：TUI 直接写终端。
- Print/JSON/RPC 模式：stdout 只保留最终回答或协议字节；普通日志、诊断、警告改走 stderr。
- `--help`、`--version`、`--list-models`、`--export` 是元数据命令，保留真实 stdout。
- daemon 模式不接管 stdout；监听地址、token、端点列表都写 stderr。

### 常见组合

```bash
# 管道输入作为初始消息的一部分
cat README.md | kiana -p "Summarize this"

# 以短横线开头的 prompt
kiana -p -- "- Summarize these points"

# JSON 事件流
kiana --mode json -p "List top-level directories" > events.jsonl

# 文件与图片附件
kiana @prompt.md @image.png "What color is the sky?"

# 只读审查
kiana --permission-mode read-only -p "Review src/"
```

## 环境变量

`kiana --help` 的 Environment Variables 段列出以下 KIANA_* 变量：

| 变量 | 作用 | 默认 / 取值 |
| --- | --- | --- |
| `KIANA_CODING_AGENT_DIR` | 配置目录 | 默认 `~/.kiana/agent`；空串视为未设置 |
| `KIANA_CODING_AGENT_SESSION_DIR` | 会话存储目录 | 被 `--session-dir` 覆盖；空串视为未设置 |
| `KIANA_API_BASE_URL` | Kiana 自建服务根地址 | 无默认；未设置时相关网络功能不可用；末尾 `/` 会被去掉 |
| `KIANA_INSTALLER_API_BASE` | 托管更新 release API | 默认 `KIANA_API_BASE_URL + /api/installer/releases` |
| `KIANA_PACKAGE_DIR` | 覆盖 package 目录（Nix/Guix store 等） | 未设置时由可执行文件位置推断 |
| `KIANA_OFFLINE` | 关闭启动期网络操作 | `1` / `true` / `yes`（大小写不敏感）；其他值为 false |
| `KIANA_TELEMETRY` | 覆盖安装遥测 | 只要已定义就覆盖 settings；`1`/`true`/`yes` 为开，空串或其他值为关 |
| `KIANA_SHARE_VIEWER_URL` | `/share` 查看器链接的 base URL | 优先使用；否则回退 `KIANA_API_BASE_URL/session/` |

另有源码中读取、但不在上述 `--help` 段的权限变量：

| 变量 | 作用 | 取值 |
| --- | --- | --- |
| `KIANA_PERMISSIONS` | 权限模式 | `read-only` / `default` / `accept-edits` / `bypass` / `off`；空值按未设置；未知值按 `default` |
| `KIANA_PERMISSION_MODE` | `KIANA_PERMISSIONS` 的别名 | 同上；两者都设置时按数组顺序先命中者优先 |

完整的 provider 凭据变量（逐字来自 `kiana --help`）：

| Provider | 环境变量 |
| --- | --- |
| Anthropic | `ANTHROPIC_AUTH_TOKEN`、`ANTHROPIC_API_KEY`、`ANTHROPIC_OAUTH_TOKEN` |
| Ant Ling | `ANT_LING_API_KEY` |
| OpenAI | `OPENAI_API_KEY` |
| Azure OpenAI | `AZURE_OPENAI_API_KEY`、`AZURE_OPENAI_BASE_URL`、`AZURE_OPENAI_RESOURCE_NAME`、`AZURE_OPENAI_API_VERSION`、`AZURE_OPENAI_DEPLOYMENT_NAME_MAP` |
| DeepSeek | `DEEPSEEK_API_KEY` |
| NVIDIA NIM | `NVIDIA_API_KEY` |
| Google Gemini | `GEMINI_API_KEY` |
| Groq | `GROQ_API_KEY` |
| Cerebras | `CEREBRAS_API_KEY` |
| xAI | `XAI_API_KEY` |
| Fireworks | `FIREWORKS_API_KEY` |
| Together AI | `TOGETHER_API_KEY` |
| Baseten | `BASETEN_API_KEY` |
| OpenRouter | `OPENROUTER_API_KEY` |
| Vercel AI Gateway | `AI_GATEWAY_API_KEY` |
| ZAI | `ZAI_API_KEY`、`ZAI_CODING_CN_API_KEY` |
| Mistral | `MISTRAL_API_KEY` |
| MiniMax | `MINIMAX_API_KEY` |
| Moonshot | `MOONSHOT_API_KEY` |
| OpenCode | `OPENCODE_API_KEY` |
| Kimi | `KIMI_API_KEY` |
| Meta | `META_API_KEY` |
| Cloudflare | `CLOUDFLARE_API_KEY`、`CLOUDFLARE_ACCOUNT_ID`、`CLOUDFLARE_GATEWAY_ID` |
| Qwen Token Plan | `QWEN_TOKEN_PLAN_API_KEY`、`QWEN_TOKEN_PLAN_CN_API_KEY` |
| Xiaomi MiMo | `XIAOMI_API_KEY`、`XIAOMI_TOKEN_PLAN_CN_API_KEY`、`XIAOMI_TOKEN_PLAN_AMS_API_KEY`、`XIAOMI_TOKEN_PLAN_SGP_API_KEY` |
| Amazon Bedrock | `AWS_PROFILE`、`AWS_ACCESS_KEY_ID`、`AWS_SECRET_ACCESS_KEY`、`AWS_BEARER_TOKEN_BEDROCK`、`AWS_REGION` |

注意：

- 上述 provider 变量的作用只是提供凭据；模型是否可用还取决于模型目录。
- 未配置 `KIANA_API_BASE_URL` 时，远端模型目录刷新、版本检查、安装遥测、托管更新等 Kiana
  托管服务不可用；本地仍可使用内置与已缓存模型。
- 测试/构建专用的 `KIANA_TEST_*`、`KIANA_FAKE_*` 等变量不属于用户接口，本页不列出。

## 限制与排查

| 现象 | 原因 / 处理 |
| --- | --- |
| `--provider` 单独使用报错 | `--provider` 必须与 `--model` 一起用 |
| `--api-key` 报 `requires a model` | `--api-key` 必须配合 `--model` 或 `--models`，否则没有可设置密钥的模型 |
| 未知长选项没有立刻报错 | 未知长选项会先作为扩展 flag 暂存；如果没有扩展注册它，会话启动诊断会报 `Unknown option` 并退出 `1` |
| 未知短选项直接报错 | 未知短选项会立即报 `Unknown option` 并退出 `1` |
| `--mode` / `--use-theme` / `--tui-mode` 缺值 | 直接报错，且不会把下一个 token 当作值 |
| `--name --foo` 把 `--foo` 当成名字 | `--name` 会消费下一个 token，即使它以 `-` 开头；名字为空时另行报错 |
| 以 `-` 开头的 prompt 被当成选项 | 用 `kiana -p -- "- ..."` |
| `--list-models -foo` 没有按搜索串处理 | 搜索串不能以 `-` 或 `@` 开头，否则退化为无搜索 |
| print 模式里写文件/执行命令被拒绝 | 非交互模式没有审批 UI，`Ask` 按拒绝处理；改用 `--allow`、`accept-edits` 或在可信环境用 `bypass` |
| 没有配置权限却期望审批 | 权限层需要显式启用；设置 `--permission-mode default` 或 `permissions.mode` |
| `--fork` / `--session-id` 报冲突 | 按“会话”一节的互斥规则去掉冲突项 |
| `--serve` 启动失败 | 先确认有可用模型；再检查 `--serve-addr` 是否是合法的 `host:port` |
| daemon 只能在 loopback 访问 | 默认绑定 `127.0.0.1:0`；跨主机部署需要反向代理提供 TLS 与认证 |
| `--export` 默认文件名不确定 | 本页不固化默认命名；需要稳定文件名时显式传第二个位置参数 |
| 只有 Linux x86_64 产物 | 当前未构建/验证 macOS/Windows |

## 相关页面

- [快速开始](getting-started.md)：运行产物、首次认证与第一次交互
- [设置](settings.md)：settings 键、配置分层与项目信任
- [权限与审批](permissions.md)：审批作用域、规则语法与持久化
- [会话、分支与恢复](sessions.md)：会话保存、恢复、fork 与导出
- [MCP 服务器](mcp.md)：`kiana mcp` 的配置文件与传输细节
- [内置工具](tools.md)：每个工具的行为与工具选择
- [故障排查](troubleshooting.md)：按现象定位启动、权限、daemon 等问题
