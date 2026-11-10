# 内置工具

本页讲什么：Kiana 内置工具（`read`、`bash`、`powershell`、`edit`、`write`、`grep`、`find`、`ls`）分别能做什么、默认哪些开启、如何用命令行与 `settings` 选择工具，以及工具审批与沙箱之间的关系。

> 版本说明：本页按 workspace 包版本 `0.1.0`（本地产物目录 `dist/v0.1.0-alpha/`）核对。`kiana --version` 输出 `1.4.0`，来自代码中有意保留的 TS 兼容常量，不是构建错误，也不代表另一个发布版本。

## 快速开始

```bash
# 默认启动：read、bash、edit、write 四个内建工具处于激活状态
kiana

# 只启用一组只读工具
kiana --tools read,grep,find,ls -p "Review the code in src/"

# 在默认工具集上增加 codemode
kiana --tools +codemode

# 只屏蔽一个工具，其余保持可用
kiana --exclude-tools bash

# 完全关闭工具
kiana --no-tools

# 关闭内建工具，保留扩展/自定义工具
kiana --no-builtin-tools
```

`--tools` 的普通名单是 allowlist；`+name` / `-name` 是对默认名单的增删。两种写法不能混用。

## 工具一览

### 默认工具集

| 工具 | 用途 | 默认激活 |
| --- | --- | --- |
| `read` | 读取文本文件或图片 | 是 |
| `bash` | 在当前目录执行 shell 命令 | 是 |
| `powershell` | 执行 PowerShell 命令（仅 Windows 可用） | 否 |
| `edit` | 用精确文本替换编辑单个文件 | 是 |
| `write` | 新建文件或整文件覆盖 | 是 |
| `grep` | 按正则或字面量搜索文件内容 | 否 |
| `find` | 按 glob 模式查找文件 | 否 |
| `ls` | 列出目录内容 | 否 |

没有 `defaultTools` 设置时，启动工具集固定为 `read`、`bash`、`edit`、`write`。`grep`、`find`、`ls` 仍会注册，但需要 `--tools` 或 `defaultTools` 才会声明给模型。`powershell` 不在默认集合，也不在 coding / read-only 工具组里，需要按名启用。

`web_fetch` 与 `web_search` 不是内建工具，而是由 `web-tools` 内联扩展注册的扩展工具，默认激活规则见[Web 工具](#web-工具)。

### 工具名与匹配规则

- 工具名是小写、精确匹配的：`Bash`、`read `（带空格）都不等于 `bash`、`read`。
- `--tools` 的普通名单与 `--exclude-tools` 支持 `*` 通配；只有 `*` 是特殊字符，`.`、`?`、`[` 等按字面量处理，匹配大小写敏感。
- `+name` / `-name` 只接受精确工具名，不接受 `*`，也不能与普通名单混写。

## 选择与过滤工具

### 命令行选项

| 选项 | 取值 | 作用 |
| --- | --- | --- |
| `--tools`, `-t <tools>` | 逗号分隔的名单或 `+name` / `-name` | 普通名单是 allowlist；`+/-` 条目只对默认名单做增删。默认保留 MCP 工具，除非名单里出现以 `mcp__` 开头的条目 |
| `--exclude-tools`, `-xt <tools>` | 逗号分隔的 denylist / 模式 | 对所有工具生效，包括 MCP 工具 |
| `--no-tools`, `-nt` | 无 | 默认不启用任何工具（内建与扩展都不启用） |
| `--no-builtin-tools`, `-nbt` | 无 | 默认不启用内建工具，但保留扩展/自定义工具 |

`--no-tools` 与 `--no-builtin-tools` 改变的是“默认起点”。如果同时用 `--tools` 显式点名工具，被点名的工具仍会启用。

### `--tools` 的两种写法

```bash
# allowlist：只启用 read 和 grep
kiana --tools read,grep

# 模式：启用所有以 mcp__docs__ 开头的工具
kiana --tools 'mcp__docs__*'

# 修饰符：在默认集合上增加 find、删除 bash
kiana --tools +find,-bash
```

约束：

- 普通名单和 `+name` / `-name` 不能混用。混用时命令在启动前报错：
  `tool names cannot be mixed with +name or -name entries`。
- `+name` / `-name` 带 `*` 会报错：`+name and -name entries take exact tool names, not patterns: <entry>`。
- 修饰符按出现顺序应用；重复的 `+name` 不会重复添加，`-name` 只删除第一个匹配。
- 普通 allowlist 不过滤 MCP 工具；只有名单里出现 `mcp__` 开头的条目时，MCP 工具才一起按名单过滤（例如 `--tools 'mcp__docs__*'`）。
- 普通 allowlist 会过滤掉未列出的扩展工具。例如 `--tools read,grep` 不会留下默认激活的 `web_fetch`。这也会排除 `codemode`/`tool_search`；MCP 的默认 `codemode` exposure 因此还需要把它们列进名单，详见 [MCP 服务器](mcp.md)。

### settings 中的 `defaultTools`

`defaultTools` 是 `settings.json` 的顶层键：

```json
{
  "defaultTools": ["read", "bash", "grep", "find", "ls"]
}
```

```json
{
  "defaultTools": ["+grep", "+find", "-bash"]
}
```

规则：

- 普通名单（`["read","bash"]`）整体替换内建默认集合，不继承 `read,bash,edit,write`。
- 空数组 `[]` 得到空的内建选择。
- 只有 `+name` / `-name` 的列表从内建默认集合起步做增删。
- 配置分层合并时，项目层的普通名单替换全局名单；项目层只有修饰符时追加到继承来的名单之后。
- `defaultTools` 的普通名单决定初始激活集合：列出的内建工具会替换内建默认集合，列出的扩展工具名也会被点名激活。没有列出的扩展工具是否激活仍由扩展自己的 `default_active` 决定（例如 `web_fetch` 在默认情况下仍会激活）。要一并关闭扩展工具，使用 `--exclude-tools` 或 `--no-tools`。
- 例外：只要对应 MCP 服务器启用，且 `codemode`/`tool_search` 没有被工具过滤排除，内置 MCP 扩展会自动激活它们（前者可用 `mcp.json` 顶层 `autoEnableCodemode: false` 关闭），见 [MCP 服务器](mcp.md)。

设置文件位置与信任规则见[配置与设置](settings.md)：全局为 `<agentDir>/settings.json`（默认 `~/.kiana/agent/settings.json`），项目为 `<cwd>/.kiana/settings.json`，项目文件只在项目受信任时读取。

### 组合优先级

| 输入 | 结果 |
| --- | --- |
| 未设置 | `read`、`bash`、`edit`、`write` |
| `defaultTools` 普通名单 | 该名单 |
| `--tools` 普通名单 | 该名单（覆盖 `defaultTools` 的选择） |
| `--tools +name/-name` | 在 `defaultTools` 或内建默认集合上增删 |
| `--exclude-tools` | 从最终集合中删除匹配项 |
| `--no-tools` | 空起点；显式 `--tools` 仍可点名 |
| `--no-builtin-tools` | 无内建起点；扩展工具按自身默认激活，显式 `--tools` 仍可点名 |

## Web 工具

`web_fetch` 与 `web_search` 由 `web-tools` 内联扩展注册，不是内建工具。

| 工具 | 默认激活 | 说明 |
| --- | --- | --- |
| `web_fetch` | 是 | 抓取一个或多个 http(s) URL，HTML 转文本后截断 |
| `web_search` | 条件激活 | **注册时**至少配置一个搜索 API key 才默认激活 |

`web_search` 的 key 条件（环境变量在注册时读取）：

- `BRAVE_API_KEY`；或
- `BING_API_KEY`；或
- `GOOGLE_CSE_API_KEY` 与 `GOOGLE_CSE_CX` 同时存在且非空。

搜索链顺序为 Brave → Google CSE → Bing，第一个成功的结果直接返回。参数：

| 参数 | 类型 | 说明 |
| --- | --- | --- |
| `query` | string，必填 | 搜索词 |
| `limit` | integer，1–20 | 返回结果数；默认 10 |

`web_fetch` 参数：

| 参数 | 类型 | 说明 |
| --- | --- | --- |
| `url` | string | 单个 URL |
| `urls` | string[] | 批量 URL，最多 20 个 |

每个页面最多返回 10,000 个字符，响应体硬上限 2 MiB，单请求超时 30 秒；私有地址、链路本地地址与云元数据地址会被拒绝。`url` 与 `urls` 至少给一个；只给 `urls` 时按输入顺序返回。

强制启用或禁用：

```bash
# 没有 key 也强制启用（调用时会报 no web search engine configured）
kiana --tools web_search

# 禁用默认激活的 web_fetch
kiana --exclude-tools web_fetch
```

注意：搜索 key 只从上述环境变量读取，`settings.json` 中没有对应的激活开关；激活判断发生在注册时，运行中再改环境变量不会改变本次会话的默认激活结果。请用环境变量提供 key，或用 `--tools web_search` 强制启用。

## 各工具行为

### read

读取文本文件或图片。文本输出按“2000 行或 50 KiB，先到者为准”截断；图片以附件形式发送，支持 jpg、png、gif、webp、bmp。

| 参数 | 类型 | 说明 |
| --- | --- | --- |
| `path` | string，必填 | 文件路径（相对或绝对） |
| `offset` | number | 起始行号，1 起；默认从第 1 行开始 |
| `limit` | number | 最多读取的行数 |

大文件用 `offset`/`limit` 分段读取；输出被截断时会有脚注说明当前显示的行范围。

`read` 还会记录会话内的读取历史，供 `edit`/`write` 的“先读后写”门禁使用：

- 对已存在的普通文件，`edit` 要求本会话读过该文件，并且只能修改实际看到过的行。
- `write` 要求本会话完整读过已存在的普通文件；新建文件不需要预先读取。
- 读取后文件被外部修改（内容哈希不一致）时，`edit`/`write` 会拒绝并要求重新读取。

### bash

执行 shell 命令，合并 stdout 与 stderr 返回。

| 参数 | 类型 | 说明 |
| --- | --- | --- |
| `command` | string，必填 | 要执行的命令 |
| `timeout` | number | 超时秒数；可选，默认不设超时 |
| `background` | boolean | 仅当 continuity 启用时出现在 schema 中 |

输出截断与结果：

- 文本结果截断到**最后** 2000 行或 50 KiB（先到者为准）；被截断时完整输出写入临时文件，脚注给出路径。
- 非零退出码不是工具级错误：结果带 `is_error: true`，文本末尾追加 `Command exited with code <N>`，同时保留结构化输出。
- 模型可见的 `structuredContent` 字段为 `output`、`truncated`、`full_output_path`（仅截断时）、`exit_code`、`wall_time_seconds`。

`background: true` 需要 continuity 开启（`continuity` 设置或 `KIANA_CONTINUITY=1`）。未开启时工具返回明确错误，不会静默改成前台执行；开启时立即返回 task id，任务在父 turn 结束后继续运行，终态输出随后投递回父会话。

### powershell

与 `bash` 共用参数 schema、超时、输出截断、临时文件和结构化输出，只是可执行文件与调用行前缀（`PS>`）不同。默认操作会在命令前注入 UTF-8 输出编码设置。

当前仅在 Windows 可用；在其它平台调用会失败，错误文本为 `The powershell tool is only available on Windows.`。它不在默认工具集，需要 `--tools powershell` 或 `defaultTools`。

### edit

对一个文件做一处或多处精确文本替换。

| 参数 | 类型 | 说明 |
| --- | --- | --- |
| `path` | string，必填 | 要编辑的文件 |
| `edits` | 数组，必填且非空 | 每项为 `{ "oldText": "...", "newText": "..." }` |

规则：

- 每个 `oldText` 必须在**原始文件**中唯一匹配，且多个 `edits` 之间不能重叠或嵌套。
- 同一文件的多处修改应放在一次 `edit` 调用里；相近的改动应合并成一个 edit，而不是拆成多条。
- 已存在的普通文件必须先被本会话读取；截断读取只能修改实际看到过的行。读取后文件被外部改动会报错。
- 写回时保留原文件的 BOM 与检测到的行尾风格；`details` 中包含 diff/patch，成功文本为 `Successfully replaced <N> block(s) in <path>.`
- 同一真实路径的编辑通过文件变更队列串行执行，避免并发写互相覆盖。

### write

新建文件或整文件覆盖。

| 参数 | 类型 | 说明 |
| --- | --- | --- |
| `path` | string，必填 | 目标文件 |
| `content` | string，必填 | 完整文件内容 |

行为：

- 自动递归创建父目录；目标已存在时直接覆盖。
- 已存在的普通文件必须在本会话中被完整读取过；新文件没有这个要求。
- 成功文本为 `Successfully wrote to <path>`；没有 `structuredContent`。
- 与 `edit` 共用同一套执行策略与文件变更队列。

### grep

按模式搜索文件内容。

| 参数 | 类型 | 说明 |
| --- | --- | --- |
| `pattern` | string，必填 | 正则或字面量 |
| `path` | string | 搜索目录或文件；默认当前目录 |
| `glob` | string | 文件过滤，如 `*.ts`、`**/*.spec.ts` |
| `ignoreCase` | boolean | 忽略大小写；默认 false |
| `literal` | boolean | 把 pattern 当作字面量；默认 false |
| `context` | number | 每个匹配前后显示的行数；默认 0 |
| `limit` | number | 最多返回的匹配数；默认 100 |

- 遵守 `.gitignore`。
- 输出截断到 100 个匹配或 50 KiB（先到者为准）；单行超过 500 字符会被截断，并提示用 `read` 查看完整行。
- 默认实现调用 `rg`（ripgrep）。本地找不到时会尝试下载；仍不可用则报 `ripgrep (rg) is not available and could not be downloaded`。

### find

按 glob 模式查找文件。

| 参数 | 类型 | 说明 |
| --- | --- | --- |
| `pattern` | string，必填 | 文件 glob，如 `*.ts`、`src/**/*.spec.ts` |
| `path` | string | 搜索目录；默认当前目录 |
| `limit` | number | 最多返回的结果数；默认 1000 |

- 遵守 `.gitignore`；返回相对于搜索目录的路径。
- 输出截断到 1000 个结果或 50 KiB（先到者为准）。
- 默认实现调用 `fd`。本地找不到时会尝试下载；仍不可用则报 `fd is not available and could not be downloaded`。

### ls

列出目录内容。

| 参数 | 类型 | 说明 |
| --- | --- | --- |
| `path` | string | 目录；默认当前目录 |
| `limit` | number | 最多返回的条目数；默认 500 |

- 条目按大小写不敏感的规则排序；目录名带 `/` 后缀；包含点文件。
- 输出截断到 500 个条目或 50 KiB（先到者为准）。
- 路径不存在报 `Path not found`；不是目录报 `Not a directory`。

## BashExecutionStatus

`BashExecutionStatus` 是 bash 执行结果的结构化状态，定义在扩展 API 中，不依赖沙箱实现。JSON 使用外部标签 + camelCase 变体名：

| JSON 形状 | 含义 | 命令是否可能已执行 |
| --- | --- | --- |
| `{"confirmedExit":{"code":0}}` | 沙箱 relay 给出了确定的退出码 | 是 |
| `{"unsandboxed":{"code":0,"signal":null}}` | 未经过沙箱直接 spawn；被信号终止时 `code` 为 `null`、`signal` 为信号编号 | 是 |
| `"unconfirmed"` | 命令已启动，但最终状态不可知 | 是，且不要自动重试 |
| `{"backendUnavailable":{"reason":"..."}}` | 沙箱后端不可用或启动失败，命令没有执行 | 否 |
| `{"interrupted":{"reason":"..."}}` | 在确认命令启动前被中断，命令没有执行 | 否 |

它出现在程序化结果的 `executionStatus` 字段中：

- `BashExecResult.executionStatus`：`BashOperations::exec` 的返回值，扩展/SDK 可读取。
- `BashResult.executionStatus`：执行器整理后的结果；`exitCode` 没有值时该键省略（不是 `null`），`executionStatus` 缺失表示旧版结果或没有状态。
- 内置 `bash` 工具给模型的文本与 `structuredContent` **不包含**这个字段；模型看到的是输出、截断信息与退出码。需要结构化状态时使用 SDK/RPC/扩展接口。
- 当前实现中，`unconfirmed`、`backendUnavailable`、`interrupted` 三种状态在工具执行路径上以错误文本返回，错误对象里不带 `executionStatus`；这是已知的残余，不要把错误文本当成结构化状态解析。
- 超时或被取消时，如果 relay 已经启动过命令，错误文本会追加 `|unconfirmed` 语义的提示，提醒不要自动重跑；先用 `read`/`ls` 检查工作区。

## 工具审批与沙箱

审批与沙箱是两层独立机制：

- **审批**回答“这次调用是否允许执行”，由权限引擎决定是否弹出审批卡或直接拒绝。
- **沙箱**回答“命令在什么隔离环境里执行”，由 `kiana-sandbox` 限制可写路径与网络。

可以只开审批不开沙箱，也可以只开沙箱不审批；推荐同时开启。

### 默认状态

如果既没有配置 `permissions`，也没有设置 `KIANA_PERMISSIONS` / `KIANA_PERMISSION_MODE`，也没有使用 `--permission-mode`、`--allow`、`--deny`、`--add-dir`，那么权限引擎不会启用，不会有审批提示。

同样，在未启用 `read-only` 权限模式、也未设置 `KIANA_SANDBOX_POLICY` 时，shell 与文件工具不会启用沙箱；`bash`/`powershell` 直接 spawn，并将结果标记为 `unsandboxed`。

### 启用审批

| 来源 | 示例 |
| --- | --- |
| settings 的 `permissions` 对象 | `{"permissions":{"mode":"default"}}` |
| 环境变量 | `KIANA_PERMISSIONS=default`，或别名 `KIANA_PERMISSION_MODE=default` |
| 命令行 | `--permission-mode default`；规则用 `--allow` / `--deny`；额外可写根用 `--add-dir` |

`KIANA_PERMISSIONS` 可取 `read-only`、`default`、`accept-edits`、`bypass`、`off`。空值视为未设置；无法识别的值按 `default` 处理（fail-closed）。`off`（以及 `disabled`、`none`、`false`、`0`）关闭权限检查。

### 模式与动作

权限引擎按工具类别决策：

| 模式 | `read`/`ls`/`grep`/`find` | `edit`/`write` | `bash`/`powershell` | MCP / 其它 |
| --- | --- | --- | --- | --- |
| `read-only` | 允许 | 拒绝 | 拒绝 | 拒绝 |
| `default` | 允许 | 询问 | 询问 | 询问 |
| `accept-edits` | 允许 | 工作区内允许，区外询问 | 询问 | 询问 |
| `bypass` | 允许 | 允许 | 允许 | 允许 |

说明：

- 模式表给的是默认结论；显式 `allow` / `deny` 规则会改变具体调用的结果，受保护目录与灾难性命令另有约束，完整顺序见[权限与审批](permissions.md)。`read-only` 下声明 `readOnlyHint: true` 的 MCP 工具可以放行。
- 内建工具按名字归类；扩展/SDK 工具一般归为 `Other`，同名 `bash`/`powershell` 仍归为执行类。
- `accept-edits` 的“工作区内”按工作区根判断；`.git` 与 `.kiana` 的写入不会因为“在工作区内”而自动放行。
- 决策顺序大致为：灾难性命令拒绝 → 显式 Deny 规则 → 受保护目录检查 → “总是允许”的批准 → 会话批准 → Ask 规则 → Allow 规则 → 模式默认。
- 用户批准有“只允许一次”“总是允许（本会话 / 本项目 / 本机）”“拒绝并填写原因”三种；`总是允许` 生成的规则仍是 Allow，Deny 规则与灾难性命令检查优先。
- 需要更细的规则语法、审批作用域与持久化位置，见[权限与审批](permissions.md)。

### 启用沙箱

| 变量 | 取值 | 作用 |
| --- | --- | --- |
| `KIANA_SANDBOX_POLICY` | `read-only`、`workspace-write`、`danger-full-access` | 选择策略；未设置或空串表示不启用；无法识别的值 fail-closed |
| `KIANA_SANDBOX` | `bwrap`（别名 `bubblewrap`）/ `landlock` / `none`（及 `off`、`disabled`、`false`） | 选择后端；`none` 及同义值显式关闭沙箱 |

不设置 `KIANA_SANDBOX` 时按 bubblewrap → Landlock → fail-closed 的顺序自动探测。`workspace-write` 含受保护子路径（`.git`/`.kiana`）时，自动选择不会退回 Landlock，而是 fail-closed；只有显式设置 `KIANA_SANDBOX=landlock` 才接受这层较弱隔离。后端不可用时命令直接失败，不会回退到无沙箱执行。

权限模式与默认沙箱的对应关系：

| 权限模式 | 默认执行策略 |
| --- | --- |
| `read-only` | `read-only` 沙箱 |
| `default` / `accept-edits` | 读取 `KIANA_SANDBOX_POLICY`；未设置或为 `danger-full-access` 时不启用 |
| `bypass` | `danger-full-access`，不使用沙箱 |

文件工具（`write`/`edit`）与 shell 共用同一份执行策略：在 `workspace-write` 下，工作区外的写入和 `.git`/`.kiana` 内的写入会被执行策略直接拒绝，不会因为审批通过而放行；可写根由当前工作区、`--add-dir` 与权限层的 `permissions.addDirs`/`workspaceRoots` 决定。

`read-only` 模式下，写与执行默认被权限层拒绝；显式 `allow` 规则仍可放行匹配的调用。即使 `KIANA_SANDBOX=none` 关闭了操作系统级后端，文件写入门禁仍继续使用只读执行策略。

`bash`/`powershell` 的沙箱状态会通过 `BashExecutionStatus` 暴露给程序化调用方；沙箱拒绝与后端不可用是 fail-closed，详见[沙箱](sandbox.md)。

## 限制与排查

| 现象 | 可能原因与处理 |
| --- | --- |
| 只看到 `read`、`bash`、`edit`、`write` | `grep`/`find`/`ls` 默认不激活；用 `--tools read,grep,find,ls` 或 `defaultTools` 启用 |
| `--tools` 报 `tool names cannot be mixed with +name or -name entries` | 普通名单与 `+/-` 不能混用，拆成一种写法 |
| `--tools +foo*` 报 `+name and -name entries take exact tool names, not patterns` | 修饰符只接受精确名字；模式请用普通名单或 `--exclude-tools` |
| 工具明明写了却不可用 | 检查是否被 `--exclude-tools` 删除；检查 `--no-tools`、`--no-builtin-tools` 的默认起点；检查普通 allowlist 是否漏掉该工具 |
| `grep`/`find` 报 `... is not available and could not be downloaded` | 默认分别需要 `rg`、`fd`；安装对应工具或允许启动期下载 |
| `bash` 报 `BackendUnavailable` 或沙箱启动失败 | 沙箱策略要求隔离但后端不可用；安装 bubblewrap，或显式设置 `KIANA_SANDBOX=none` 接受无沙箱 |
| `read-only` 模式下 `edit`/`write`/`bash` 被拒 | `read-only` 默认只允许读；切换 `--permission-mode`、调整 `permissions.mode`，或为确需放行的操作写显式 `allow` 规则 |
| `workspace-write` 下写 `.git`/`.kiana` 被拒 | 执行策略默认保护这些目录；这不是审批问题，改写到工作区其它位置 |
| 编辑被要求先读文件 | `edit`/`write` 有先读后写门禁；先用 `read` 完整读取（`edit` 至少读到要改的行） |
| 编辑报内容已变化 | 读取后文件被外部修改；重新 `read` 再编辑 |
| `web_search` 默认不出现 | 注册时没有 `BRAVE_API_KEY`、`BING_API_KEY` 或成对的 `GOOGLE_CSE_API_KEY`/`GOOGLE_CSE_CX`；用环境变量提供，或 `--tools web_search` 强制启用 |
| `web_search` 调用报 `no web search engine configured` | 工具被强制启用但没有可用 key |
| 命令超时或取消后提示可能已执行 | 结果处于 unconfirmed；不要自动重试，先检查工作区 |
| `powershell` 在 Linux/macOS 失败 | 当前只在 Windows 可用 |

当前实现边界：

- 仅验证 Linux x86_64；没有 macOS/Windows 产物。`powershell` 的 Windows 路径未在本机验证。
- `BashExecutionStatus` 的三种错误状态（`unconfirmed`、`backendUnavailable`、`interrupted`）仍以错误文本返回，没有结构化结果字段。
- 审批引擎默认关闭，这是当前行为；不要假定“没有配置也会自动询问”。
- 工具输出还有会话级的输出预算处理，超长输出可能被截断并落盘；本页列出的上限是各工具自身的上限。

## 相关页面

- [权限与审批](permissions.md)：规则语法、审批作用域与持久化。
- [沙箱](sandbox.md)：bubblewrap / Landlock 后端与策略细节。
- [MCP 服务器](mcp.md)：MCP 工具、exposure 与 MCP 服务器的沙箱/环境变量规则。
- [配置与设置](settings.md)：`settings.json` 位置、分层与 `defaultTools`。
- [CLI 参考](cli.md)：`--tools` 等命令行选项的完整列表。
