# 故障排查

本页讲什么：按“现象 → 原因 → 处理”整理 Kiana 启动、权限、沙箱、MCP、daemon、IDE 与磁盘/日志相关问题，并给出可直接运行的诊断命令。

先说明一个常见误解：`--verbose` 不是日志级别开关，它只影响交互式启动页是否展开。真正的诊断入口见下面的命令表。

## 快速诊断

```bash
# 版本与帮助
kiana --version
kiana --help

# 有没有模型、模型是否可用
kiana --list-models
kiana --list-models sonnet

# 某个 provider 的凭据是否就绪
kiana auth check --provider anthropic --json

# MCP 服务器状态与错误
kiana mcp list
kiana mcp list --json

# 已安装的扩展包
kiana list
```

交互式会话内还可以用：

- `/debug`：把当前渲染内容与会话消息写入 `<agentDir>/kiana-debug.log`。
- `/mcp`：查看和管理已配置的 MCP 服务器。
- `/model`、`/login`：切换模型或配置 provider 凭据。

> 版本号差异见 [遥测、离线与外部服务](telemetry-and-offline.md) 开头的版本说明。

## 启动失败

### 现象总表

| 现象 | 常见原因 | 处理入口 |
| --- | --- | --- |
| 非交互启动退出 1，stderr 出现 `No models available...` | 没有可用 provider/模型/凭据 | `kiana --list-models`；`/login`；`--model` |
| 交互模式启动但没有模型 | 同上；设置里的默认模型失效 | `/model`、`/login`、`settings.json` |
| 请求时出现 `No API key found for <provider>.` | 模型已选中，但该 provider 没有凭据 | `kiana auth check --provider <p>`；`/login <provider>` |
| 请求时出现 `Authentication failed for "<provider>". ...` | OAuth 凭据过期、刷新失败或网络不可达 | `/login <provider>`；检查网络 |
| 启动退出 1，`Failed to load extension "<path>": ...` + `Hint: ... "kiana -ne"` | 扩展解析/执行失败 | `kiana -ne` 隔离；`kiana list`；`kiana config` |
| `Warning: Invalid settings file <path>: ...` | settings JSON 语法/类型错误 | 修复或删除对应键 |
| `Error: Invalid httpIdleTimeoutMs setting: ...` 且退出 1 | `httpIdleTimeoutMs` 值非法 | 修正 settings 后重启 |
| `Error: failed to start daemon on <addr>: ...` | `--serve` 端口被占用/地址非法 | 更换 `--serve-addr` |

### 没有可用模型

现象：

- 非交互模式（`-p`、`--mode json`、`--mode rpc`，或 stdin/stdout 被重定向）退出码 1，并打印 `No models available. Use /login to log into a provider via OAuth or API key. See: ...`。
- 交互模式仍能启动，但启动页会提示同样的信息，此时无法正常对话。
- `kiana --list-models` 输出同一段 `No models available...`，退出码 0。

原因：

1. 没有配置任何 provider 凭据，模型目录为空。
2. `settings.json` 的 `defaultProvider` / `defaultModel` 指向不存在的模型。
3. `models.json` 解析失败；`kiana --list-models` 会先打印 `Warning: errors loading models.json: ...`。
4. 当前运行处于离线模式且远端模型目录从未缓存过。

处理：

```bash
# 看当前能解析出的模型
kiana --list-models

# 用 provider/id 直接指定一个可用模型
kiana --model anthropic/claude-sonnet-4-5 -p "hello"

# 没有凭据时先登录
kiana
# 然后在交互式会话里执行 /login
```

补充：

- `--provider` 只用于配合 `--model` 搜索 provider，不能单独选模型。
- `--models` 控制 Ctrl+P 循环列表，不解决“初始没有模型”的问题。
- 非交互模式没有交互式审批/登录界面；需要先在一个交互式会话里完成 `/login`。

### 有模型但没有凭据

现象：

- 发送消息后返回 `No API key found for <provider>.`，下面附 `/login` 与文档路径提示。
- OAuth provider 返回 `Authentication failed for "<provider>". Credentials may have expired or network is unavailable. Run '/login <provider>' to re-authenticate.`

原因：模型已选中，但 `auth.json`、环境变量或 OAuth 凭据中没有该 provider 的可用凭证；也可能是 OAuth token 过期且刷新失败。

处理：

```bash
# 查看状态（默认会刷新过期 OAuth；--no-refresh 只读检查）
kiana auth check --provider anthropic --json
kiana auth check --provider anthropic --no-refresh

# 查看将会使用的凭证（注意：输出包含密钥，不要贴到公开渠道）
kiana auth print-api-key --provider anthropic
```

`kiana auth check` 的状态与退出码：

| `status` | 退出码 | 含义 | 处理 |
| --- | --- | --- | --- |
| `ready` | 0 | 找到可用凭据 | 可以直接使用 |
| `not_ready` | 1 | provider 不存在（`provider_not_found`）或没有凭据（`credentials_not_configured`） | 检查 provider 名；`/login <provider>` 或设置环境变量 |
| `invalid` | 2 | 运行时/读取失败（`invalid_state`），无法判断 | 检查 settings、`models.json`、文件权限后重试 |

参数错误（例如缺少 `--provider`/`--model`）同样以退出码 2 结束，并打印 `Error: Auth checks require --provider <provider> or --model <model>`。

其他处理方式：

- 在交互式会话中运行 `/login <provider>`。
- 使用 provider 对应的环境变量（完整列表见 `kiana --help` 的 Environment Variables 段），例如 `ANTHROPIC_API_KEY`、`OPENAI_API_KEY`、`DEEPSEEK_API_KEY`。
- 单次运行使用 `--api-key <key>`（会覆盖环境变量来源）。
- `--model <pattern>` 也可以让 `auth check` 通过模型反推 provider：`kiana auth check --model anthropic/claude-sonnet-4-5 --json`。

### 扩展加载失败

现象：启动退出码 1，stderr 出现：

```text
Error: Failed to load extension "<path>": <message>
Hint: Start without extensions using "kiana -ne".
```

原因：

- 扩展文件有 JS/TS 语法错误或解析失败。
- 扩展没有导出预期的默认工厂。
- 扩展在执行工厂时抛出异常（例如依赖缺失、运行时未初始化）。
- 扩展包的 peer dependency 不满足；这类问题也可能先以 `Extension package "<path>": <warning>` 的 Warning 出现。

处理：

```bash
# 1) 先确认是扩展问题：本次启动禁用扩展发现与全部内建扩展
kiana -ne

# 2) 列出已安装的包
kiana list

# 3) 用 TUI 禁用某个包资源（Tab 切换 global/project 作用域）
kiana config

# 4) 移除有问题的包
kiana remove <source>
# 或
kiana uninstall <source>
```

说明：

- `-ne` / `--no-extensions` 会同时关闭内建扩展（包括 MCP 与 `/llama`）；显式 `-e <path>` 仍会加载。要完全隔离，使用 `kiana -ne` 且不要同时给 `-e`。
- 通过 `-e` 临时加载的扩展出错时，从命令中移除该 `-e` 参数即可。
- 扩展错误属于运行时诊断；只要出现一条 `Failed to load extension`，启动就会以 1 退出，不会进入会话。

### 设置文件无效或启动即退出

现象：

- `Warning: Invalid settings file <path>: ...` 或 `Warning: Invalid <scope> settings: ...`：通常不阻止启动，相关设置回退到默认值。
- `Error: Invalid httpIdleTimeoutMs setting: <value>`：直接退出 1，不打印帮助。
- 系统/Managed/Enterprise 策略层（Unix 为 `/etc/kiana/...`，Windows 为 `%ProgramData%\kiana\...`）默认严格校验，未知字段会报成 `unknown field "<key>"` 的 Warning；用户/项目 settings 是宽松格式，未知键不会让整份配置报错。

原因：JSON 语法错误、字段类型不符、未知字段，或 `httpIdleTimeoutMs` 不是合法的毫秒值/`"disabled"`。

处理：

1. 按提示路径打开 settings 文件，修正 JSON。
2. 删除报错的键，先用默认值启动。
3. `.kiana/settings.local.json` 作为 local 配置层参与 `permissions`、`hooks` 等分层合并；类型化 Settings 键（`theme`、`defaultModel` 等）不从该文件读取，且仅在项目受信任时加载。
4. 检查文件权限：`EACCES: permission denied, ...` 表示当前用户无法读写对应路径。

如果错误信息以 `Error:` 开头且进程直接退出，优先按提示修复；不要在带病设置上继续启动。

### `--serve` 无法启动

现象：

- 非 TTY 环境（systemd、容器、管道）下运行 `kiana --serve`，可能在 daemon 启动前退出 1，并打印 `No models available...`。
- `Error: failed to start daemon on <addr>: <error>`：地址非法或被占用。
- `Error: invalid --serve-addr "<value>": <error>`：`--serve-addr` 不是合法的 `host:port`。

原因：daemon 分支位于非交互模式的模型检查之后；没有 TTY 时会被当作打印模式，从而要求可用模型。端口被占用或地址格式错误则无法绑定。

处理：

- 先确保 `kiana --list-models` 能找到模型并完成 `/login`。
- 换一个端口：`kiana --serve --serve-addr 127.0.0.1:8787`。
- daemon 相关问题继续看下面的 [Daemon 返回 401](#daemon-返回-401)。

## 沙箱后端不可用

现象：命令没有执行，工具结果以以下文本开头：

```text
BackendUnavailable: The command was not executed: the sandbox backend is unavailable (<reason>). Do not fall back to running it unsandboxed.
```

在工具结果中也可能直接看到稳定错误码（文件工具与 shell 都走同一套执行策略）：

```text
INVALID_CONFIG: invalid sandbox configuration: ...
READ_ONLY: <path> is not writable: the sandbox policy is read-only
OUTSIDE_WORKSPACE: <path> is outside the writable roots (...)
PROTECTED_PATH: <path> is inside the protected path ... (.git/.kiana cannot be modified by tools)
```

常见原因：

| 原因 | 典型 `reason` / 场景 |
| --- | --- |
| 设置了 `KIANA_SANDBOX_POLICY=workspace-write`，但主机没有可用的 bubblewrap/Landlock | `no working bubblewrap or Landlock backend is available; install bubblewrap or set KIANA_SANDBOX=none to run unsandboxed` |
| 自动选择时只有 Landlock，而 `workspace-write` 需要保护 `.git` / `.kiana` | `the Landlock fallback cannot keep protected subpaths (.git/.kiana) read-only inside writable roots; install bubblewrap, or set KIANA_SANDBOX=landlock to accept the weaker isolation` |
| `KIANA_SANDBOX=bwrap`，但 `bwrap` 不在 `PATH` 或用户命名空间探针失败 | `no working bubblewrap or Landlock backend is available; install bubblewrap or set KIANA_SANDBOX=none to run unsandboxed` |
| `KIANA_SANDBOX=landlock`，但内核不支持或 ABI 不满足 | 后端选择失败，返回 `no working bubblewrap or Landlock backend is available; install bubblewrap or set KIANA_SANDBOX=none to run unsandboxed`（ABI < 4 且策略禁止网络时同样拒绝） |
| `KIANA_SANDBOX_POLICY` 拼写错误或不是合法 UTF-8 | `INVALID_CONFIG: invalid sandbox configuration: unrecognized KIANA_SANDBOX_POLICY value ...`（执行策略层拒绝，不是 `BackendUnavailable`） |
| 在 macOS / Windows 上要求沙箱 | 后端尚未实现，返回 `Unsupported` / `Unavailable`，不会静默放行 |

处理：

```bash
# 1) 检查 bubblewrap 是否存在
command -v bwrap

# 2) 用系统包管理器安装 bubblewrap（常见包名 bubblewrap），然后重试

# 3) 不在 PATH 时显式指定
KIANA_BWRAP=/path/to/bwrap kiana

# 4) 接受较弱隔离：显式选择 Landlock（.git/.kiana 的只读保护会失效）
KIANA_SANDBOX=landlock KIANA_SANDBOX_POLICY=workspace-write kiana

# 5) 显式关闭沙箱（危险；只在明确知道后果时使用）
KIANA_SANDBOX=none kiana
```

相关环境变量：

| 变量 | 取值 | 说明 |
| --- | --- | --- |
| `KIANA_SANDBOX` | `none`/`off`/`disabled`/`false`；`bwrap`/`bubblewrap`；`landlock` | 显式选择后端或关闭沙箱；未设置时自动探测 |
| `KIANA_SANDBOX_POLICY` | `read-only`/`readonly`、`workspace-write`/`workspace`、`danger-full-access`/`danger`/`none`/`off`/`disabled` | 空值或未设置 = 不启用沙箱；无法识别的值 fail-closed |
| `KIANA_BWRAP` | 可执行文件路径 | 显式指定 `bwrap` |

注意：

- `KIANA_SANDBOX=none` 是显式放行；如果没有显式关闭，而策略需要沙箱但后端不可用，则**不会**自动放行。
- 只读权限模式本身要求沙箱；但只读模式通常先在权限层拒绝执行/写入，看到的是权限拒绝而不是沙箱错误。
- MCP stdio 服务器在会话内也会按权限模式安装默认沙箱；后端不可用时 MCP 会连接失败。见下一节。

## MCP 连接失败

现象：

- 会话内某个 MCP 服务器的工具不可用，或状态显示连接失败。
- `kiana mcp list` 显示 `failed`、`disconnected`、`needs sign-in`，或末尾出现 `config error: ...`。
- JSON 模式下 `servers[].error` 或顶层 `errors[]` 有内容。

原因：

1. stdio 命令不存在、参数错误、工作目录不存在。
2. HTTP URL 不可达、证书错误、代理问题。
3. 服务器要求 OAuth 登录（`needs-auth`）。
4. `mcp.json` 语法错误或字段类型错误。
5. 项目未受信任，`.kiana/mcp.json` 被忽略。
6. 会话以 `--no-mcp` 启动。
7. 会话内 MCP stdio 沙箱后端不可用（见上一节）。

处理：

```bash
# 查看每个服务器的状态、工具和错误；任一启用的服务器未连接时退出码为 1
kiana mcp list
kiana mcp list --json

# OAuth 服务器：浏览器登录
kiana mcp login <server>

# 重新添加 / 修正服务器
kiana mcp add <server> -- <command> [args...]
kiana mcp add <server> --url <url>
kiana mcp remove <server>

# 临时确认是不是 MCP 导致的问题
kiana --no-mcp
```

`kiana mcp list` 的状态含义：

| 状态 | 含义 | 处理 |
| --- | --- | --- |
| `connected` | 已连接，后面会列出工具数 | 正常 |
| `needs-auth` | 服务器要求登录 | `kiana mcp login <server>` |
| `failed` | 连接失败，`error` 保存原因 | 检查命令/URL/凭据/沙箱 |
| `disconnected` | 连接曾建立但已断开，下次调用会重连 | 检查服务器进程是否退出 |
| `disabled` | 配置里 `enabled: false` | 在 `mcp.json` 中启用 |

其他说明：

- `kiana mcp list` 会真实连接每个启用的服务器；服务器无响应时命令会变慢。禁用的条目不会连接。
- 项目未受信任时，输出会附带：`<cwd>/.kiana/mcp.json is ignored because the project is not trusted. Start kiana in the project to trust it.` 用 `--approve` 启动或执行 `/trust`。
- `kiana mcp list` 在会话之外运行，不会安装会话级的默认 MCP 沙箱；它显示 `connected` 不代表会话内一定能启动 stdio 服务器。若会话内失败而 `mcp list` 正常，优先检查沙箱后端（`command -v bwrap`）。
- 配置写入位置：全局 `<agentDir>/mcp.json`；项目 `.kiana/mcp.json`（仅在项目受信任时读取）。`-l/--local` 让 `add`/`remove` 操作项目文件。

## 权限被拒

先区分“权限系统拒绝”和“文件工具要求先读后写”。

| 现象（逐字或前缀） | 原因 | 处理 |
| --- | --- | --- |
| `Permission mode 'read-only' does not allow <action> actions (tool '<tool>').` | 当前是 `read-only` 权限模式 | 改用 `--permission-mode default` 或 `accept-edits`；确实只读时不需要处理 |
| `Permission denied by rule for tool '<tool>' (<action>).` | 命中 `permissions` 或 `--deny` 的 Deny 规则（规则自带 `reason` 时直接显示该 reason） | 删除/放宽对应规则；Deny 永远优先于 Allow |
| `Permission denied: command is blocked as destructive (<reason>).` | 命令被判定为灾难性操作；非 bypass 模式一律拒绝 | 拆分为更小的安全命令；只有 `bypass` 模式不做该检查，且 managed 策略可能禁用 bypass |
| `Permission denied by the user: <reason>` / `Permission denied by the user.` | 用户在审批卡上拒绝 | 重新执行并在审批卡上允许，或预先加 `--allow` 规则 |
| `... No interactive UI is available to approve this action.` | 非交互模式下遇到 `Ask`，没有审批界面 | 用 `--permission-mode accept-edits`、`--allow` 或 settings `permissions` 预先放行 |
| `READ_ONLY: ...` / `OUTSIDE_WORKSPACE: ...` / `PROTECTED_PATH: ...` | 文件写入被沙箱执行策略拒绝 | 参见对应处理：改权限模式、用 `--add-dir`、避免写 `.git`/`.kiana` |
| `ReadBeforeEdit: ... has not been read in this session...` | 不是权限规则；文件工具要求先读后写 | 先用 `read` 查看完整文件，再 `edit`/`write` |
| `EACCES: permission denied, ...` | 操作系统文件权限，不是 Kiana 权限规则 | 修正文件/目录权限或更换 `KIANA_CODING_AGENT_DIR` |

具体入口：

```bash
# 本次运行使用“工作区内编辑自动允许”
kiana --permission-mode accept-edits

# 追加可写工作区根
kiana --add-dir /path/to/another/repo

# 预先允许某条规则（规则语法见权限页）
kiana --allow '<rule>'

# 在交互式会话中永久保存项目信任（信任与权限是两件事）
# /trust
```

补充：

- `accept-edits` 只自动允许工作区内的写；写 `.git` / `.kiana`、写工作区外仍会询问（非交互模式下即失败）。
- `.git` / `.kiana` 的安全确认不提供“总是允许”，每次都需要确认。
- 非交互模式没有 `--yes` 自动批准；必须通过权限模式或规则预先放行。
- 权限规则的完整语法、作用域与合并顺序见 [权限与审批](permissions.md)。

## Daemon 返回 401

现象：`curl` 或其他客户端访问 `--serve` 的端点时收到：

```text
HTTP/1.1 401 Unauthorized
content-type: text/plain; charset=utf-8

unauthorized
```

原因：

1. 请求没有携带 token。
2. token 拼错，或用了上一次运行的 token（没有显式 `--serve-token` 时，每次启动都会重新生成）。
3. 用了 `Authorization` 头以外的形式但没有加 `?token=`。
4. 反向代理剥掉了 `Authorization` 头。

处理：

```bash
# 启动 daemon：token 会打印到 stderr
kiana --serve --serve-addr 127.0.0.1:8787 --serve-token 'my-token'

# 请求头方式
curl -sS -H 'Authorization: Bearer my-token' \
  'http://127.0.0.1:8787/events/latest?limit=5'

# EventSource / WebSocket 无法设置请求头时，用 query 参数
curl -sS 'http://127.0.0.1:8787/events/latest?limit=5&token=my-token'

# /healthz 不需要鉴权
curl -sS http://127.0.0.1:8787/healthz
```

说明：

- 启动时 stderr 会打印 `Daemon listening on http://<addr>`、`Bearer token: <token>` 和端点列表；token 没有持久化，也不会轮换。丢失后只能重启进程。
- `--serve-token` 为空串时按“未提供”处理，仍会自动生成 token。
- 只有 GET 被接受；其他方法返回 405 `method not allowed`。
- 默认只监听 `127.0.0.1:0`（随机端口）。绑定非 loopback 地址时，需要自行在反向代理层提供 TLS 与额外认证。
- 事件 replay 有界（4096 条），客户端落后太多会出现 gap；可用 `/events/latest?limit=N` 重新拉取最新页。
- 没有模型的非 TTY 环境会在 daemon 启动前退出 1，见上文“`--serve` 无法启动”。

## IDE 未发现

现象：

- 编辑器里没有 Kiana 的 IDE 集成（diff、上下文更新）。
- 找到 lockfile 但连接失败时，启动 stderr 出现 `Warning: failed to connect to IDE: <error>`。
- 连接成功但上下文推送失败时出现 `Warning: failed to send IDE context update: <error>`。

原因与检查：

| 条件 | 说明 |
| --- | --- |
| lockfile 位置 | 默认 `~/.kiana/ide/<port>.lock`；`KIANA_IDE_DIR` 可覆盖目录 |
| 进程存活 | `pid`（或 `ppid`）对应进程必须存活；两者都没有时不会连接 |
| workspace 匹配 | `workspacePath` 或 `workspaceFolders` 必须等于当前目录，或当前目录位于其下 |
| transport | 只实现 HTTP；lockfile 要求 `ws` 时直接报错 `IDE lockfile requests WebSocket transport, which is not implemented; use HTTP transport or no IDE` |
| 开关 | `KIANA_IDE=0/false/no/off` 会关闭 IDE 集成；默认开启 |
| 离线模式 | `--offline` / `KIANA_OFFLINE` 真值会跳过 IDE 连接 |

处理：

```bash
# 看有没有 lockfile
ls -la ~/.kiana/ide

# 查看单个 lockfile（注意：其中含 authToken，不要贴到公开渠道）
cat ~/.kiana/ide/<port>.lock

# 确认 IDE 插件仍在运行、打开的是同一个 workspace
```

补充：

- 没有 lockfile 时是静默跳过（`Ok(None)`），不会打印错误；这属于正常行为。
- 代码会自动清理无法解析、pid 已死、workspace 不匹配的 lockfile；手动删除陈旧文件也可以。
- 如果 IDE 插件只支持 WebSocket transport，当前版本无法连接；需要改用 HTTP transport 或关闭 IDE 集成。
- 没有专门的 IDE CLI flag；只有 `KIANA_IDE` 与 `KIANA_IDE_DIR`。
- 当前实现只在启动时建立一次 IDE 连接，没有持续的编辑器事件推送；`openDiff` 虽有 broker 与通知路由，但没有工具调用点，不能当作已可用的 diff 功能。

## 磁盘与日志位置

`<agentDir>` 默认为 `~/.kiana/agent`，可用 `KIANA_CODING_AGENT_DIR` 覆盖。`<cwd>` 是当前项目目录。

| 内容 | 默认路径 | 说明 |
| --- | --- | --- |
| agent 目录 | `~/.kiana/agent` | `KIANA_CODING_AGENT_DIR` 覆盖；settings/auth/trust 都在这下面 |
| 全局设置 | `<agentDir>/settings.json` | 主要设置文件 |
| 项目设置 | `<cwd>/.kiana/settings.json` | 项目受信任时读取 |
| 项目本地设置 | `<cwd>/.kiana/settings.local.json` | 参与 `permissions`、`hooks` 等分层合并；类型化 Settings 键不读取；仅项目受信任时加载 |
| 凭据 | `<agentDir>/auth.json` | 含密钥，不要公开 |
| 项目信任 | `<agentDir>/trust.json` | 目录 → 信任决定 |
| 模型配置/缓存 | `<agentDir>/models.json`、`<agentDir>/models-store.json` | 后者是动态目录缓存 |
| MCP 配置 | `<agentDir>/mcp.json`；`<cwd>/.kiana/mcp.json` | 项目文件仅在受信任时读取 |
| 会话 | `<agentDir>/sessions/--<cwd>--/<timestamp>_<session-id>.jsonl` | 追加式 JSONL；目录名由 cwd 派生 |
| 调试日志 | `<agentDir>/kiana-debug.log` | 仅 `/debug` 写入；每次覆盖 |
| 崩溃日志 | `<agentDir>/crashes.json` | 交互模式未捕获异常/致命错误时写入；最多保留 5 条，下次启动会提示运行 `/bug` |
| IDE lockfile | `~/.kiana/ide/<port>.lock` | 含 `authToken`，不要公开 |
| TUI 渲染日志 | `/tmp/tui/render-*.log` | 仅 `KIANA_TUI_DEBUG=1` 时写入；内部调试开关 |

会话目录优先级：`--session-dir` > `KIANA_CODING_AGENT_SESSION_DIR` > `settings.sessionDir` > 默认目录。

磁盘处理：

- 会话文件只追加，当前没有自动清理磁盘文件的机制，也没有内置的会话清理命令；`compaction` 只影响上下文，不会删除历史 `.jsonl` 条目。
- 需要释放空间时，可以在确认没有会话正在使用后，手动删除旧的会话文件；删除后对应会话无法恢复。
- 不需要保存会话时用 `--no-session`。
- 把会话移到大磁盘：`kiana --session-dir /path/to/sessions`，或设置 `KIANA_CODING_AGENT_SESSION_DIR`。
- 磁盘满时通常会看到 Node 风格的 `ENOSPC: no space left on device, ...`。清理会话/缓存或更换 agent 目录后重试。
- Kiana 没有通用的滚动日志文件；daemon 日志写 stderr，`/debug` 写 `kiana-debug.log`，崩溃摘要写 `crashes.json`。需要收集问题现场时优先保存 stderr 与这些文件。

隐私提醒：`kiana-debug.log` 包含渲染行与会话消息全文，`crashes.json` 包含错误消息/堆栈，二者可能含代码与密钥；`auth.json`、`mcp.json`（含 env/header）、IDE lockfile 也可能含凭据。对外提交前先脱敏。

## 诊断命令参考

| 命令 | 作用 | 关键输出 / 退出码 |
| --- | --- | --- |
| `kiana --help` | 列出公开 flag、子命令与 provider 环境变量 | 退出 0 |
| `kiana <command> --help` | 子命令帮助（`auth`、`mcp`、`install`、`update` 等） | 退出 0 |
| `kiana --version` | 版本号 | 输出 `1.4.0`（见版本说明） |
| `kiana --list-models [search]` | 列出可用模型，可用搜索串过滤 | 无模型时打印 `No models available...`，仍退出 0 |
| `kiana auth check --provider <p> --json` | 检查 provider 凭据 | `ready`/`not_ready`/`invalid`；退出 0/1/2 |
| `kiana auth check --model <provider/model> --json` | 通过模型反推 provider 后检查 | 同上；建议用 `provider/model` 精确写法 |
| `kiana auth print-api-key --provider <p>` | 打印匹配的 API key | 输出即密钥，注意保密 |
| `kiana auth print-bearer-token --provider <p>` | 打印 OAuth bearer token；默认 `--min-expiry 30m` | 会刷新过期 token |
| `kiana mcp list [--json]` | 连接并报告每个 MCP 服务器 | 任一启用的服务器未连接或配置读取失败时退出 1 |
| `kiana list` | 列出用户/项目 settings 中安装的包 | 用于扩展加载失败时的隔离 |
| `kiana config` | 打开资源启用/禁用 TUI | `Tab` 切换 global/project |
| `--verbose` | 强制展开交互式启动页详情（覆盖 `quietStartup`） | 仅交互模式；不是日志级别 |
| `/debug` | 把渲染结果与会话消息写入 `<agentDir>/kiana-debug.log` | 内部命令，不在自动补全中 |
| `/mcp` | 会话内查看 MCP 状态/登录/重连 | 由内建 MCP 扩展注册 |

## 限制与排查边界

- 当前验证平台是 Linux x86_64；macOS 的 seatbelt 与 Windows 沙箱后端尚未实现，需要沙箱时会 fail-closed。
- `/debug` 是内部调试命令，不在 24 项内建斜杠命令表与自动补全中，后续可能调整。
- `--verbose` 只影响启动页展示；要记录日志请用 `/debug` 或 `KIANA_TUI_DEBUG=1`（内部开关）。
- 没有 `kiana doctor` 之类的总诊断命令；按本页的命令组合逐层排查。
- daemon token 只打印到 stderr，不持久化、不轮换；生产部署需要额外的认证与 TLS 代理。
- `.kiana/settings.local.json` 作为 local 配置层参与 `permissions`、`hooks` 等分层合并；类型化 Settings 键（`theme`、`defaultModel` 等）不从该文件读取，且仅在项目受信任时加载。

## 相关页面

- [遥测、离线与外部服务](telemetry-and-offline.md)
- [权限与审批](permissions.md)
- [沙箱与执行隔离](sandbox.md)
- [MCP 服务器](mcp.md)
- [Daemon 事件服务](daemon.md)
- [IDE 集成](ide.md)
- [配置与设置](settings.md)
- [CLI 参考](cli.md)
