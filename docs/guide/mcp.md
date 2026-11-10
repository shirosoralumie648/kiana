# MCP 服务器

本页讲什么：如何用 `kiana mcp` 配置和管理 MCP 服务器、`mcp.json` 写在哪里、stdio 与 Streamable HTTP 两种传输怎么填、OAuth 登录怎么走，以及 MCP 子进程的沙箱与环境变量规则。

## 快速开始

```bash
# 添加一个 stdio 服务器（-- 之后是启动命令与参数）
kiana mcp add files -- npx -y @modelcontextprotocol/server-filesystem /tmp/docs

# 添加一个 Streamable HTTP 服务器
kiana mcp add example --url https://example.com/mcp

# 查看状态、工具与错误
kiana mcp list

# 对需要 OAuth 的 HTTP 服务器发起浏览器登录
kiana mcp login example
```

`kiana mcp` 是不启动会话的管理入口：`add`/`remove` 直接改配置文件，`list`/`login` 会真实连接服务器，`logout` 删除已保存的凭据。命令退出码只有 `0` 与 `1`。

## 配置文件与作用域

MCP 服务器从两个 `mcp.json` 读取：

| 作用域 | 路径 | 何时读取 | 写入方式 |
| --- | --- | --- | --- |
| 全局 | `<agentDir>/mcp.json`，默认 `~/.kiana/agent/mcp.json` | 总是 | `kiana mcp add/remove`（默认） |
| 项目 | `<cwd>/.kiana/mcp.json` | 只有项目受信任时 | `kiana mcp add/remove -l` |

`<agentDir>` 可用 `KIANA_CODING_AGENT_DIR` 覆盖；项目配置目录名固定为 `.kiana`。

项目信任：

- `--approve` / `-a` 在本次运行中信任项目本地文件；`--no-approve` / `-na` 忽略它们。
- 交互会话中的 `/trust` 可以保存信任决定，供后续会话使用。
- `kiana mcp add -l` 会先写文件、再检查信任状态。项目未受信任时会提示该文件“在信任项目前会被忽略”，但命令仍返回 0。
- `kiana mcp list/login/logout` 只把 `trust.json` 中明确的 `true` 当作受信任；读取 `trust.json` 失败会让命令返回 1。

合并规则：

- 先读全局，再读项目；同名项目条目替换全局条目。
- 项目条目如果**没有** `command`、`url`、`type`，只作为覆盖条目，只能设置 `enabled`、`exposure`、`toolExposure`。例如在某个项目里关闭全局服务器：
  ```json
  {
    "mcpServers": {
      "internal-tools": { "enabled": false }
    }
  }
  ```
- 项目文件里不允许设置 `auth`；带 `auth` 的服务器只能写在全局 `mcp.json`。
- 服务器名经 `-`/`_` 归一后不能与已有服务器命名空间冲突。
- 扩展也可以通过 `kiana.registerMcpServer()` 注册服务器；这类条目的 `scope` 为 `extension`，改动不会写回文件。

## `kiana mcp` 命令

### `add`

```text
kiana mcp add <server> [options] -- <command> [args...]
kiana mcp add <server> [options] --url <url>
```

选项：

| 选项 | 适用传输 | 说明 |
| --- | --- | --- |
| `-l`, `--local` | 两者 | 写入当前项目的 `.kiana/mcp.json`，而不是全局文件 |
| `--url <url>` | HTTP | Streamable HTTP 服务器 URL；与 `--` 后的命令二选一 |
| `--env <KEY=VALUE>` | stdio | 为子进程设置环境变量；可重复，同名后写覆盖先写 |
| `--cwd <dir>` | stdio | 子进程工作目录；相对路径按会话工作目录解析 |
| `--header <KEY=VALUE>` | HTTP | 额外请求头；可重复，同名后写覆盖先写 |
| `--bearer-token-env-var <NAME>` | HTTP | 追加 `Authorization: Bearer ${NAME}` 请求头；变量值在连接时读取 |
| `--oauth-client-id <id>` | HTTP | 预注册的 OAuth client id |
| `--oauth-client-secret <secret>` | HTTP | OAuth client secret；可写 `${NAME}` 或 `!command` |
| `--oauth-callback-port <port>` | HTTP | 固定 OAuth 回调端口 |
| `--oauth-client-name <name>` | HTTP | 动态注册时发送的 client name |
| `--exposure <mode>` | 两者 | `codemode`（默认）/ `deferred` / `direct` / `hidden` |
| `--description <text>` | 两者 | 服务器能力说明；出现在系统提示的 MCP 段（该段只列 `codemode`/`deferred` 服务器） |

示例：

```bash
# stdio：带环境变量与工作目录
kiana mcp add docs \
  --env DOCS_ROOT=/srv/docs \
  --cwd /srv/docs \
  -- npx -y @modelcontextprotocol/server-filesystem /srv/docs

# HTTP：固定请求头（单引号防止 shell 展开 ${API_TOKEN}）
kiana mcp add api \
  --url https://example.com/mcp \
  --header 'X-API-Key=${API_TOKEN}'

# HTTP：用环境变量生成 Authorization 头（这会让该服务器不再走 OAuth）
kiana mcp add api \
  --url https://example.com/mcp \
  --bearer-token-env-var API_TOKEN

# 只写项目配置
kiana mcp add files -l -- npx -y @modelcontextprotocol/server-filesystem .
```

约束与行为：

- `--url` 与 `--` 后的命令必须二选一；同时提供会报错。
- HTTP 专属选项（`--header`、`--bearer-token-env-var`、`--oauth-client-id`、`--oauth-client-secret`、`--oauth-callback-port`、`--oauth-client-name`）不能用于 stdio；stdio 专属选项（`--env`、`--cwd`）不能用于 `--url`。错误文本如 `--env only applies to stdio servers.`。
- 参数与配置校验都在写文件之前完成；非法输入不会留下半个条目。
- 写入是“读—改—写”：保留文件中的其它服务器、未知键、键序与缩进，文件末尾补一个换行。
- `add` 成功后输出 `Added` 或 `Replaced`，并提示用 `kiana mcp list` 检查；没有 `Authorization` 头的 HTTP 服务器还会提示 `kiana mcp login <server>`。这条提示不看 `auth` 字段，配置了 `auth.provider` 的服务器应改用 `/login <provider>`。
- **任何位置**出现 `--help` / `-h` 都会打印 `kiana mcp` 帮助并返回 0，不会写文件；即使它出现在 `--` 之后也一样。给 MCP 服务器传 `--help` 参数时要避开这一点。`kiana mcp`（无参数）和 `kiana mcp help` 也会打印帮助。

### `remove`

```bash
kiana mcp remove <server> [-l]
```

`-l` 指定项目文件，否则操作全局文件。目标文件里没有该条目时，命令会重新加载配置，判断服务器是否定义在另一个作用域，并在错误信息里提示该加还是去掉 `--local`。

### `list`

```bash
kiana mcp list
kiana mcp list --json
```

`list` 会为每个**启用**的服务器建立一次真实连接；`enabled: false` 的条目只报告 `disabled`，不连接。所有服务器并发推进，报告顺序与配置顺序一致。

人类可读输出包含：服务器名、状态、exposure、作用域、传输描述、工具列表、错误。失败判定：

- 配置文件读取/校验错误，或任一启用服务器最终状态不是 `connected`，退出码为 1。
- 即使退出码为 1，完整报告仍会输出。

`--json` 输出顶层 `servers`、`errors`，以及项目未受信任且存在项目配置时的 `note`。`servers[]` 的关键字段：

| 字段 | 说明 |
| --- | --- |
| `name` / `scope` / `source` | 服务器名、`global`/`project`/`extension`、定义文件路径或注册它的扩展 |
| `enabled` | 是否启用 |
| `exposure` | 默认 exposure |
| `transport` | stdio 显示命令与参数，HTTP 显示 URL |
| `state` | `connecting`、`connected`、`disconnected`、`needs-auth`、`failed`、`closed`，或禁用的 `disabled` |
| `tools` | 服务器返回的工具名 |
| `toolExposure` | 与默认 exposure 不同的工具覆盖 |
| `resources` | 服务器提供 resources 时为 `true` |
| `error` | 未连接时的最近错误 |

### `login` / `logout`

```text
kiana mcp login <server> [--timeout <seconds>]
kiana mcp logout <server>
```

- `login` 只适用于“使用 OAuth”的 HTTP 服务器：配置里有 `url`、没有 `auth`、且 `headers` 中没有名为 `Authorization` 的键（大小写不敏感）。
- `--timeout` 是整次登录的秒数上限，默认 `300`，必须是正数。
- `login` 会先连接：已经登录成功时直接输出 `Already signed in to MCP server "<name>" (<N> tools).` 并返回 0；连接失败但不是 `needs-auth` 时输出失败原因并返回 1。
- 进入登录流程后，默认监听 `127.0.0.1` 的回环回调地址，打开浏览器完成授权码 + PKCE 流程。stdin 是终端时，也允许粘贴回调 URL；非交互环境只能等浏览器回调或超时。
- 登录成功后重新连接并输出工具数；超时或未完成时报 `Sign-in to MCP server "<name>" was cancelled or not completed within <N> seconds.`。
- `logout` 删除该服务器保存的 OAuth 凭据；没有凭据时输出 `No stored credentials for MCP server "<name>".` 并返回 0。对非 OAuth 服务器执行 `login`/`logout` 会报：`MCP server "<name>" does not use OAuth. Only HTTP servers without an Authorization header do.`

## `mcp.json` 参考

顶层是 `mcpServers` 对象，键为服务器名。服务器名只允许字母、数字、`_`、`-`（`^[A-Za-z0-9_-]+$`）。

顶层还可以有 `autoEnableCodemode`（boolean，默认 `true`）：任一启用的服务器使用 `codemode` exposure 时，MCP 扩展会在会话开始时自动激活 `codemode` 工具；设为 `false` 可关闭这一自动激活。项目文件里的同名键覆盖全局值。该键需要直接编辑 `mcp.json`，`kiana mcp add` 没有对应选项。

```json
{
  "mcpServers": {
    "files": {
      "command": "npx",
      "args": ["-y", "@modelcontextprotocol/server-filesystem", "/tmp/docs"],
      "env": { "LOG_LEVEL": "info" },
      "cwd": ".",
      "exposure": "codemode",
      "enabled": true,
      "description": "Read files under /tmp/docs",
      "timeout": 60
    },
    "api": {
      "url": "https://example.com/mcp",
      "headers": { "X-API-Key": "${API_KEY}" },
      "oauth": { "callbackPort": 43210 },
      "toolExposure": { "search": "direct" }
    }
  }
}
```

### stdio 字段

| 字段 | 类型 | 说明 |
| --- | --- | --- |
| `command` | string，必填 | 启动命令；支持 `~` 展开 |
| `args` | string[] | 命令参数；每项支持 `~` 展开 |
| `env` | object<string,string> | 子进程环境变量；值可写 `${NAME}` 或 `!command` |
| `cwd` | string | 工作目录；相对路径按会话工作目录解析 |
| `type` | string | 可省略；显式写 `"stdio"` 时保留 |

### Streamable HTTP 字段

| 字段 | 类型 | 说明 |
| --- | --- | --- |
| `url` | string，必填 | 只接受 http/https |
| `headers` | object<string,string> | 请求头；值可写 `${NAME}` 或 `!command` |
| `oauth` | object | OAuth client 设置，见下文 |
| `auth` | object | `{ "provider": "<name>" }`，用 Kiana provider 的 token 代替 OAuth；只允许写在全局文件 |
| `type` | string | 可省略；`"http"` 或 `"streamable-http"` |

`type: "sse"` 会被拒绝：`legacy SSE transport is not supported; use the streamable HTTP URL`。当前只支持 stdio 与 Streamable HTTP 两种传输。

### 公共字段

| 字段 | 类型 | 默认 | 说明 |
| --- | --- | --- | --- |
| `exposure` | `codemode` / `deferred` / `direct` / `hidden` | `codemode` | 默认工具暴露方式 |
| `toolExposure` | object<tool, exposure> | — | 单个工具的 exposure 覆盖；精确名优先，其次按插入顺序匹配含 `*` 的模式 |
| `enabled` | boolean | `true` | `false` 时保留条目但不连接 |
| `description` | string | — | 服务器能力说明；系统提示的 MCP 段取它的第一行，单台上限 250 个字符；未配置时回退到服务器 `initialize` 返回的 instructions |
| `timeout` | number | `60` | 每次请求的超时秒数，必须为正数 |
| `enabledTools` / `enabled_tools` | string[] | — | 只启用匹配 glob 的工具（支持 `*`、`?`、`[]`、`**`；非法模式按字面量匹配） |
| `disabledTools` / `disabled_tools` | string[] | — | 禁用匹配 glob 的工具；exclude 优先于 include |

`oauth` 对象字段：

| 字段 | 说明 |
| --- | --- |
| `clientId` | 预注册的 client id |
| `clientSecret` | client secret；可写 `${NAME}` 或 `!command` |
| `callbackPort` | 回环回调端口，1–65535 |
| `callbackUrl` | 已注册的回调地址；必须是 `localhost`、`127.0.0.1` 或 `[::1]` 上的 http URI，且不带查询串/片段 |
| `scope` | 请求的 scope，空格分隔 |
| `clientName` | 动态注册时的 client name，非空 |
| `clientRegistration` | `dcr`（默认）或 `cimd` |
| `authServerMetadataUrl` | 代替自动发现使用的授权服务器元数据 URL；必须是 https，或回环地址上的 http |

校验规则（`kiana mcp add` 与运行时读取都会执行）：

- `url` 必须是可解析的 http/https URL。
- `headers`、`env` 必须是“字符串到字符串”的对象。
- `args` 必须是字符串数组。
- `timeout` 必须是正数；`enabled` 必须是布尔；`description` 必须是字符串。
- `exposure` / `toolExposure` 的值只能是 `codemode`、`deferred`、`direct`、`hidden`；旧别名 `codemode-deferred` 会先规范化为 `codemode`。
- `auth.provider` 必须是非空字符串；带 `auth` 的服务器要求 https，或 http + 回环主机。
- `oauth.clientRegistration` 为 `cimd` 时不能同时设置 `clientId` 或 `clientName`。
- `oauth.callbackUrl` 与 `oauth.callbackPort` 都写明端口时必须一致。
- `oauth.clientRegistration` 为 `cimd` 且设置了 `callbackUrl` 时，主机必须是 `localhost` 或 `127.0.0.1`（不能是 `[::1]`），路径必须是 `/callback`。

加载 `mcp.json` 时还会检查顶层 `autoEnableCodemode` 是否为布尔；类型错误只记一条配置错误，同一文件里的服务器条目仍会继续加载。

## stdio 与 Streamable HTTP

### stdio

`command`/`args` 启动一个本地子进程，JSON-RPC 通过 stdin/stdout 按行传输：

- 子进程环境不是“全量继承”，而是白名单 + 配置里的 `env`，见[沙箱与环境变量](#沙箱与环境变量)。
- 单行 JSON-RPC 消息上限为 8 MiB；stdout 不是合法协议消息时按协议错误处理。
- stderr 被捕获。连接失败时，错误信息会附带 stderr 尾部（最多 2,000 个 UTF-16 码元），`kiana mcp list` 会把它打印出来。
- 服务器通过 MCP logging 发送的日志通知会追加到 `<agentDir>/mcp.log`；文件超过 5 MiB 时轮转到 `mcp.log.1`。
- 进程关闭有分级超时：先尝试正常关闭，再强制终止；宿主正常退出时会清理登记的子进程组。

### Streamable HTTP

- `url` 指向 MCP 的 Streamable HTTP 端点；HTTP 传输没有本地子进程，沙箱配置对它没有意义。
- `headers` 中的 `${NAME}`/`!command` 在建立连接时解析；解析失败会让该服务器连接失败，而不是静默发送空值。
- 没有 `auth` 且没有 `Authorization` 头时，服务器可能走 OAuth；否则按配置发送请求头，或使用 Kiana provider token。
- 连接阶段对瞬时网络错误（无 HTTP 状态的 fetch 错误，或 408、429、5xx；501 除外）会重试；只读请求遇到瞬时 HTTP 错误会重试一次；工具调用在会话过期（404）时会重连后重跑一次。401 会先尝试刷新 token，刷新失败后进入 `needs-auth`。

### 工具暴露（exposure）

| 值 | 行为 |
| --- | --- |
| `codemode`（默认） | 可从 codemode 脚本调用；不作为普通工具声明给模型，也不列进 codemode 描述 |
| `deferred` | 在 `tool_search` 加载前不声明给模型 |
| `direct` | 像普通工具一样声明给模型，也可从 codemode 调用 |
| `hidden` | 注册但不可达 |

注意触达工具（codemode / tool_search）的激活规则：

- 只要有启用的服务器使用 `codemode`（默认值），MCP 扩展就会自动把 `codemode` 工具加入激活集合。`mcp.json` 顶层 `autoEnableCodemode: false` 可以关闭这一自动激活，此时需要自己通过 `--tools +codemode` 或 `defaultTools` 启用。
- 只要有启用的服务器使用 `deferred`，MCP 扩展就会自动激活 `tool_search`；这个自动激活没有对应的关闭开关。
- 因此默认的 `codemode` 服务器无需额外参数即可从 codemode 脚本调用；想让 MCP 工具直接出现在模型工具表中，才需要 `--exposure direct`（或 `toolExposure` 单独设置）。
- 旧别名 `codemode-deferred` 在配置校验时按 `codemode` 处理。

## OAuth

### 何时使用 OAuth

同时满足以下条件时，MCP 连接会创建 OAuth provider：

1. 配置里有 `url` 键（正常 HTTP 服务器都满足）。
2. 没有 `auth` 字段（配置校验要求 `auth` 必须是带非空 `provider` 的对象，`null` 或空对象会被拒绝）。
3. `headers` 的键里没有大小写不敏感的 `authorization`。

因此 `--bearer-token-env-var` 或手写 `Authorization` 头都会关闭该服务器的 OAuth；`auth.provider` 也会关闭 OAuth，改用 provider token。

### 登录流程

- 默认使用动态客户端注册（`dcr`）；`oauth.clientRegistration: "cimd"` 使用 Kiana 的 Client ID Metadata Document，基地址为 `KIANA_API_BASE_URL/oauth`。授权服务器必须声明支持 Client ID Metadata Documents 与公共客户端；否则会报错并提示改用 `dcr`。未配置 `KIANA_API_BASE_URL` 时 CIMD 也会失败并提示改用 `dcr`。
- 回调默认监听 `127.0.0.1` 的随机端口，路径 `/callback`；可用 `oauth.callbackPort` 或 `oauth.callbackUrl` 固定。
- 授权使用 PKCE。登录成功后凭据写入 `<agentDir>/mcp-auth.json`，键按服务器命名空间、服务器 URL 与授权服务器 issuer 区分。
- 连接时先使用存储的 access token；临近过期且有 refresh token 时刷新。跨进程刷新用锁文件互斥，锁在 `<agentDir>`；过期/中断的锁可被接管。
- 收到 401 时会先尝试刷新；`insufficient_scope` 不会靠刷新解决，需要重新登录。
- 凭据是本地 JSON 文件，没有额外加密；不要把 `mcp-auth.json` 提交到版本库或分享。

### `kiana mcp login` 与 `/mcp`

- `kiana mcp login <server>` 适合在会话外完成授权；运行中的会话在下一轮会检测处于 `needs-auth` 服务器的凭据变化并重连。
- 会话内可用 `/mcp`、`/mcp login [server]`、`/mcp logout [server]`、`/mcp reconnect [server]` 管理连接（该命令由内置 MCP 扩展注册；`--no-mcp` 或 `--no-extensions` 时不存在）。
- `kiana mcp logout <server>` 只删除该服务器的 OAuth 凭据；没有凭据也返回 0。
- `auth.provider` 类型的服务器不走 `kiana mcp login`；需要先在会话中执行 `/login <provider>` 配置 provider 凭证。

## 沙箱与环境变量

### 沙箱

会话内的 stdio MCP 服务器默认由进程级沙箱 manager 托管，策略按启动时的有效权限模式决定：

| 权限模式 | MCP stdio 的默认策略 |
| --- | --- |
| `read-only` | `read-only` |
| `default` / `accept-edits` | `workspace-write`，可写根为当前工作区 + `--add-dir` |
| `bypass` | `danger-full-access` |

优先级与例外：

- 显式传给传输的 manager 最高；其次是进程级默认 manager；最后才是 `KIANA_SANDBOX_POLICY`。
- 进程级默认策略只把命令行 `--add-dir` 并入可写根；`settings` 里的 `permissions.addDirs` 只影响会话 shell 与文件工具，不参与这次 MCP 默认策略计算。
- `KIANA_SANDBOX_POLICY` 非空时，启动阶段不会安装权限模式对应的默认 manager，由传输层直接按该变量构造策略。也就是说，**显式 policy 环境变量优先于权限模式**。
- `KIANA_SANDBOX=none`（及 `off`、`disabled`、`false`）显式关闭沙箱。
- 策略要求隔离但后端不可用时，`spawn` 失败，连接进入 `failed`；不会回退到无沙箱执行。
- 只有 stdio 服务器有子进程；HTTP 服务器不涉及进程沙箱。
- `kiana mcp list/login/logout` 这类提前返回的 CLI 子命令不经过会话的默认 manager 安装点，默认只按 `KIANA_SANDBOX_POLICY` 决定沙箱；未设置该变量时这些命令启动的 stdio 服务器不受沙箱约束。

### 环境变量白名单

stdio 子进程默认只继承以下变量（`LC_*` 按前缀）：

| 类别 | 变量 |
| --- | --- |
| 基础 | `PATH`、`HOME`、`SHELL`、`LANG`、`TMPDIR`、`TMP`、`TEMP` |
| 代理 | `http_proxy`、`https_proxy`、`no_proxy`、`all_proxy`、`HTTP_PROXY`、`HTTPS_PROXY`、`NO_PROXY`、`ALL_PROXY` |
| 区域 | 所有 `LC_*` |

同时，名字匹配 `*_KEY`、`*_TOKEN`、`*_SECRET`、`*_PASSWORD`、`*_CREDENTIAL*` 的变量默认被剥离（比较前转大写）。配置里的 `env` 最后写入，可以覆盖白名单变量，也可以显式传入敏感变量。

因此：

- provider 的 API key 不会自动泄漏给 MCP 子进程。
- 需要额外变量时，用 `--env KEY=VALUE` 或 `mcp.json` 的 `env` 显式列出。
- 如果命令在子进程里找不到（`spawn ... ENOENT`），先检查它依赖的变量或可执行文件路径是否在 `PATH` 内。

`env`、`headers`、`oauth.clientSecret` 的值支持 `${NAME}`（也支持 `$NAME`）与 `!command` 占位。环境变量模板每次读取；`!command` 的 stdout 会去掉首尾空白，空输出视为解析失败。模板中任一变量未设置或为空串都会让整个值解析失败。

## 会话内使用

### 工具名

MCP 工具注册为 `mcp__<server>__<tool>`：

- 非 `[A-Za-z0-9_]` 字符会变成 `_`，因此服务器 `my-server` 的工具名是 `mcp__my_server__<tool>`。`--tools`/`--exclude-tools` 里要使用清洗后的名字。
- 名字超过 64 个字符，或与已有工具名冲突时，截断并追加 8 位哈希后缀。
- 至少一个未 `hidden` 的服务器提供 resources 时，还会注册 `list_mcp_resources`、`list_mcp_resource_templates`、`read_mcp_resource` 三个资源工具。

### prompt 命令

服务器提供的 MCP prompt 注册为 `/mcp__<server>__<prompt>` 命令。命令参数支持 `name=value` 与位置参数；必填参数缺失、未知参数或参数过多都会报可读错误。

### 工具选择与过滤

| 选项 | 对 MCP 工具的影响 |
| --- | --- |
| 默认 | 按服务器 `exposure` 决定是否声明给模型 |
| `--tools` 普通名单 | 不按 MCP 注册表过滤（除非名单里出现 `mcp__` 开头的条目）；但名单会排除未列出的非 MCP 扩展工具。要让默认 `codemode` 服务器可用，名单里需要同时包含 `codemode`（`deferred` 需要 `tool_search`）；`direct` 工具也要被名单收录或另行激活 |
| `--tools +name/-name` | 只增删默认名单里的工具，不会自动关闭 MCP |
| `--exclude-tools` | 对所有工具生效，包括 MCP 工具 |
| `--no-tools` | 关闭所有工具，包括 MCP |
| `--no-builtin-tools` | 只关内建工具；MCP 工具仍按 exposure 规则存在 |
| `--no-mcp` | 不连接任何 MCP 服务器，不暴露 MCP 工具 |
| `--no-extensions` | 关闭扩展发现与内置扩展，包括 MCP（`--no-mcp` 只关 MCP）；显式 `-e` 加载的扩展仍会运行 |

### 结果形态

- MCP 工具文本结果超过 20 KiB 时保留首尾、切掉中间，全文写入模型可读的临时文件。
- `structuredContent` 给 codemode 等程序化调用者，内容是服务器返回结果的 JSON（去掉 `_meta`），不截断。
- 图片块原样保留；其它二进制资源会存到临时文件，并通过资源链接提示用 `read_mcp_resource` 读取。
- 服务器返回 `isError` 且没有任何文本时，会补一条 `MCP tool <server>/<tool> returned an error`。

## 限制与排查

| 现象 | 检查与处理 |
| --- | --- |
| `list` 显示 `failed`，错误后带一段 stderr | 错误文本已附带 stderr 尾部；检查启动命令、参数、`cwd`、依赖与 `env` |
| `spawn ... ENOENT` | 子进程的 `PATH` 中找不到命令；用绝对路径，或确保 `PATH` 在默认白名单内 |
| `list` 显示 `needs-auth` | OAuth 服务器运行 `kiana mcp login <server>`；配置了 `auth.provider` 的服务器改用会话内 `/login <provider>`。同时确认配置里没有多余的 `Authorization` 头 |
| `login` 报 `does not use OAuth` | 该服务器有 `auth` 或 `Authorization` 头；`auth` 用 `/login <provider>`，`Authorization` 服务器不需要 OAuth 登录 |
| 登录超时 | 默认 300 秒；浏览器无法访问回环回调地址、回调端口被占用或授权未完成都会超时 |
| 回调端口报 `EADDRINUSE` | 换 `oauth.callbackPort`，或去掉固定端口让系统分配 |
| 项目服务器被忽略 | 项目未受信任；用 `--approve` 启动，或在会话中 `/trust`；`add -l` 的提示不代表已生效 |
| `config error: server "..." ...` | JSON 形状或字段校验失败；按本页“校验规则”修正 |
| MCP 工具不出现在模型工具表 | `codemode` 服务器的工具本来就不直接声明，要经 codemode 脚本调用；`autoEnableCodemode: false` 时还要手动启用 `codemode`。要直接声明给模型，改用 `--exposure direct`；`deferred` 工具由 `tool_search` 按需加载 |
| MCP 工具被过滤 | 检查 `--exclude-tools`、`--no-tools`；使用普通 `--tools` 名单时确认 `codemode`/`tool_search` 也在名单里，并检查名单中是否出现了 `mcp__` 条目 |
| 服务器连接被沙箱拒绝 | 后端不可用会 fail-closed；安装 bubblewrap，或显式设置 `KIANA_SANDBOX=none`（仅在你接受无沙箱时） |
| `mcp.log` 很大 | 服务器 logging 通知持续写入；超过 5 MiB 会轮转到 `mcp.log.1`，可定期清理 |
| `mcp-auth.json` 解析失败 | 凭据文件损坏时不会当作空表；修复/删除该文件后重新登录 |
| `add` 只打印帮助、不写文件 | 任意位置的 `--help`/`-h` 都会触发帮助；从命令参数中移除 |
| `list` 很慢 | 每个启用服务器都会真实连接；不可达服务器会等到超时。可先用 `--json` 或临时禁用条目定位 |

当前实现边界：

- 只验证 Linux x86_64；macOS/Windows 未构建、未实测。
- 旧版 SSE 传输不支持；只支持 stdio 与 Streamable HTTP。
- `kiana mcp list/login/logout` 不安装会话级默认沙箱，只认 `KIANA_SANDBOX_POLICY`（见上文）。
- `mcp-auth.json` 是本地明文 JSON，没有系统密钥链集成。
- 项目 `mcp.json` 不能设置 `auth`；需要 provider token 的服务器必须写在全局文件。
- 沙箱错误路径（如 `backendUnavailable`）在 MCP 侧表现为连接失败文本，没有独立的错误码字段。

## 相关页面

- [内置工具](tools.md)：`--tools`、`--exclude-tools`、`--no-tools` 的完整工具选择规则。
- [权限与审批](permissions.md)：MCP 工具属于 `mcp` 动作类别，默认模式下需要审批。
- [沙箱](sandbox.md)：bubblewrap / Landlock 后端、策略与 fail-closed 行为。
- [配置与设置](settings.md)：`KIANA_CODING_AGENT_DIR`、项目信任与 settings 分层。
- [交互模式（TUI）](interactive-mode.md)：会话内 `/mcp` 与 `/mcp__<server>__<prompt>` 命令。
- [CLI 参考](cli.md)：`--no-mcp`、`--no-extensions` 等启动选项。
