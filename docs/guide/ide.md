# IDE 集成

本页讲什么：kiana 怎样通过 `~/.kiana/ide/*.lock` 发现本机编辑器、建立 MCP Streamable HTTP 连接、发送 `ide/contextUpdate`，以及怎样处理 `ide/diffAccepted` / `ide/diffRejected`。同时说明当前实现只完成了哪些接线、哪些能力还不能用。

## 快速开始

前置条件：需要一个**外部 IDE 插件**在 `~/.kiana/ide/` 写入 lockfile，并在 `127.0.0.1:<port>` 上提供 MCP Streamable HTTP 端点。本仓库只包含 kiana 侧的客户端代码，不包含 VS Code / JetBrains 插件。

```bash
# 默认启用：在已打开 IDE 的项目里正常启动
kiana

# 关闭 IDE 集成
KIANA_IDE=0 kiana

# 自定义 lockfile 目录
KIANA_IDE_DIR=/path/to/ide-locks kiana

# 离线运行会跳过 IDE 连接
kiana --offline
```

连接是在会话创建之后、进入交互 / print / JSON / RPC / daemon 之前尝试的一次性动作。没有 IDE 时什么也不做；找到但连接失败只打印 warning，不阻止 kiana 启动。

## 启动时发生了什么

1. 清理 lockfile 目录：删除无法解析、pid 已死、或与当前 workspace 不匹配的 `.lock` 文件。
2. 发现可连接的 IDE：解析 `.lock`、检查进程存活与 workspace 匹配，按文件 mtime 从新到旧排序，取第一个。
3. `transport` 为 `ws` 时直接拒绝（未实现 WebSocket 传输）。
4. 向 `POST http://127.0.0.1:<port>/mcp` 发送 MCP `initialize`（`protocolVersion` 为 `2024-11-05`），随后发送 `notifications/initialized`。
5. 发送一次 `ide/contextUpdate` 通知，当前只带项目信任状态。
6. 把连接存入进程级 `OnceLock`，供 `IdeConnection::open_diff` 与通知路由使用。

启动时的判断条件：

| 条件 | 结果 |
| --- | --- |
| `KIANA_IDE` 为 `0` / `false` / `no` / `off`（忽略大小写与首尾空白） | 跳过 |
| `KIANA_IDE` 为其它值或空字符串 | 启用（默认启用） |
| `--offline` 或 `KIANA_OFFLINE` 为真 | 跳过 |
| 没有匹配的 lockfile | 静默跳过 |
| 找到但连接 / initialize 失败 | stderr 输出 `Warning: failed to connect to IDE: ...`，继续启动 |
| 连接成功但 `contextUpdate` 发送失败 | stderr 输出 `Warning: failed to send IDE context update: ...`，继续启动 |

`KIANA_IDE` 与 `KIANA_IDE_DIR` **没有**列在 `kiana --help` 的 Environment Variables 段，但可以在 `crates/kiana-coding-agent/src/app.rs` 与 `crates/kiana-coding-agent/src/ide/startup.rs` 中核对。

## Lockfile 参考

默认目录是 agent 目录的同级 `ide/` 子目录；默认 agent 目录为 `~/.kiana/agent`，因此默认是 `~/.kiana/ide`。`KIANA_IDE_DIR` 非空时按原样使用，**不做 `~` 展开**；空串视为未设置。

文件名通常是 `<port>.lock`，例如 `45123.lock`；端口优先取文件名中的数字，文件名不是数字时回退到 JSON 的 `port`。推荐格式（camelCase，与 Claude Code 的 IDE 插件形状兼容）：

```json
{
  "workspacePath": "/repo",
  "workspaceFolders": ["/repo", "/repo/sub"],
  "port": 45123,
  "authToken": "...",
  "ppid": 1234,
  "pid": 1234,
  "ideName": "vscode",
  "transport": "http",
  "runningInWindows": false
}
```

| 字段 | 类型 | 当前用途 |
| --- | --- | --- |
| `workspacePath` | string | 单个 workspace 路径；与 `workspaceFolders` 合并后参与匹配 |
| `workspaceFolders` | string[] | 打开的 workspace 目录列表 |
| `port` | number | 文件名不是数字时作为端口回退；文件名优先 |
| `authToken` | string | 放进 `Authorization: Bearer` 头；不写日志、不进错误文本 |
| `ppid` | number | 进程存活检查的后备；`pid` 缺失时使用 |
| `pid` | number | 进程存活检查的首选字段 |
| `ideName` | string | 解析但当前不参与判断 |
| `transport` | string | `ws` 会被拒绝；缺省、`http`、`sse` 都按 Streamable HTTP 处理 |
| `runningInWindows` | bool | 解析但当前不参与判断 |

端口与解析规则：

- 端口优先取文件名 `45123.lock` 中的数字；文件名不是数字时退回 JSON 的 `port`；两者都没有则视为无效 lockfile。
- 旧格式（每行一个 workspace 路径，没有 JSON）仍能解析，但因为没有 `pid` / `ppid`，按当前连接条件**不会连接**。
- 无法解析的 JSON 会尝试按旧格式解析；空文件、无端口、无法解析的文件会被清理。

### 可连接条件

1. `pid`（优先）或 `ppid` 对应的进程仍存活。Unix 下使用 `kill(pid, 0)`，`EPERM` 视为存活；非 Unix 平台的 `process_is_alive` 恒为 `true`。
2. 至少一个 lockfile workspace 与当前 workspace 相同，或当前 workspace 位于该目录之下。路径先做 `canonicalize`，失败时按原样比较。
3. 多个候选按 mtime 从新到旧排序，只连接第一个。

### 清理行为

启动时会删除以下 `.lock` 文件：

- 无法解析或没有端口；
- `pid` / `ppid` 已死；
- 进程活着但 workspace 不匹配；
- 没有 `pid` 也没有 `ppid`。

清理只删除 `.lock` 文件本身，不跟随符号链接。注意：判定条件包含“workspace 不匹配”，因此多个项目共享默认 `~/.kiana/ide` 时，在项目 A 启动 kiana 可能删除项目 B 正在使用的 lockfile。需要隔离时给每个项目设置不同的 `KIANA_IDE_DIR`。

## 连接与协议参考

| 项 | 值 |
| --- | --- |
| 端点 | `POST http://127.0.0.1:<port>/mcp` |
| 请求头 | `content-type: application/json`；`accept: application/json, text/event-stream`；有 `authToken` 时加 `Authorization: Bearer <token>` |
| 响应 | `application/json` 或 `text/event-stream`；SSE 中逐条读取 `data:` 行，只接受 id 匹配的响应，其余无 id 消息按服务器通知收集 |
| `initialize` 超时 | 10 秒 |
| 通知（`notify`）超时 | 30 秒 |
| `ide/openDiff` 请求超时 | 300 秒 |
| 鉴权失败 | HTTP `401` / `403` 映射为 `IDE authorization failed (HTTP <status>)` |
| 其它非 2xx | `IDE returned HTTP <status>` |

`initialize` 是幂等的：同一客户端只成功发送一次。`notify` 接受 2xx（含 `202`）。

## `contextUpdate`

方法名是 `ide/contextUpdate`，是一条 JSON-RPC 通知（没有 id）。payload 字段：

| 字段 | 类型 | 说明 |
| --- | --- | --- |
| `openFiles` | array | 最多 10 个打开的文件；元素为 `{path, content?, languageId?}` |
| `currentFile` | string? | 当前文件绝对路径 |
| `cursor` | object? | `{line, character}`，0 起始；`character` 按 UTF-16 code unit 计 |
| `selection` | object? | `{start, end}`，结构同 `cursor` |
| `trust` | bool | 当前项目是否被信任 |
| `truncated` | bool | 是否发生了文件数或字节数裁剪 |

上限与裁剪：

- 最多 10 个文件；JSON 编码后最多 16 KiB（`MAX_CONTEXT_FILES = 10`、`MAX_CONTEXT_BYTES = 16384`）。
- 超限时先按 UTF-8 边界截断文件内容，仍超限则从尾部丢弃文件，并把 `truncated` 置为 `true`。
- 极端情况下（例如 `currentFile` 自身超长）会清空 `openFiles`，并把 `currentFile` 截到 1024 字节。

当前实现只在启动时发送**一次** `contextUpdate`，且 `IdeContext` 只设置了 `trust`，所以实际 payload 形如：

```json
{"openFiles":[],"trust":true,"truncated":false}
```

没有编辑器的打开/切换事件源，也没有周期性推送；编辑器状态不会持续同步给 kiana。

## `openDiff` 与 diff 通知

`ide/openDiff` 请求字段：

```json
{
  "id": "diff-1",
  "path": "/repo/a.rs",
  "oldContent": "...",
  "newContent": "...",
  "readOnly": false
}
```

| 阶段 | 行为 |
| --- | --- |
| `readOnly: true` | 立即返回 `IdeError::ReadOnlyDiff`，**不发送任何请求**、不写文件 |
| 发送请求 | 以 300 秒超时请求 `ide/openDiff` |
| 立即结算 | 响应为 `{accepted:true, content?}` 时接受，缺 `content` 时使用 `newContent`；`{accepted:false}` 或 `{rejected:true}` 时拒绝 |
| 等待通知 | `{pending:true}`、`{}`、`null` 都表示继续等待；最多再等 300 秒 |
| 接受通知 | `ide/diffAccepted`，参数 `{id, content}`；`content` 是用户在 IDE 中最终确认的内容 |
| 拒绝通知 | `ide/diffRejected`，参数 `{id}` |
| 超时 / 传输失败 | 注销等待项并返回错误，不会留下悬挂条目 |

通知路由由 `route_ide_notification` 完成，并在建立连接时通过 `HttpIdeTransport::with_notification_sink` 挂到传输上。通知从同一次 MCP 请求的 SSE 响应体中收集；当前没有单独的常驻通知连接。

**当前实现限制：`IdeConnection::open_diff` 没有生产调用点。** diff broker、`ide/openDiff` 请求、`diffAccepted` / `diffRejected` 路由和超时都已实现并有单元测试，但没有任何内置工具或 TUI 路径会调用它。因此当前编辑仍然通过 `edit` / `write` / `bash` 等工具完成，不会弹出 IDE 的 diff 视图。`IdeConnection::open_diff` 目前只对嵌入方或测试可用。

## 限制与排查

- **只在启动时连接一次。** IDE 重启、端口变化或 lockfile 更新后不会自动重连；需要重启 kiana。
- **没有持续上下文推送。** `contextUpdate` 只在启动时发一次，且不带打开文件；编辑器不会把光标、选区或文件内容持续同步给 kiana。
- **`openDiff` 未接入工具。** 见上一节；不要期待编辑文件时自动出现 IDE diff。
- **WebSocket 传输未实现。** lockfile 的 `transport` 为 `ws` 时返回 `IDE lockfile requests WebSocket transport, which is not implemented; use HTTP transport or no IDE`，应用只打印 warning，不连接。
- **没有 pid 的 lockfile 不会连接。** 旧格式解析后也会因为没有 `pid` / `ppid` 而无法通过存活检查。
- **清理会删除其它 workspace 的 live lockfile。** 多项目共享默认目录时尤其明显；用 `KIANA_IDE_DIR` 按项目隔离。
- **实际启动会话时都会尝试连接。** 交互、`--print`、`--mode json/rpc`、`--serve` 都会在启动时尝试一次（`--help`、`--list-models`、`--export` 在此之前返回）；CI 或无 IDE 环境建议设置 `KIANA_IDE=0`，避免 lockfile 存在但服务不可达时最多等待 10 秒的 `initialize` 超时。
- **没有 settings 键。** IDE 集成只能通过 `KIANA_IDE` / `KIANA_IDE_DIR` 控制，二者都不在 `kiana --help` 中。
- **loopback HTTP，没有 TLS。** `authToken` 只通过请求头传输；不要把它写入日志或错误信息。
- **当前验证范围是 Linux x86_64。** 非 Unix 平台的进程存活检查恒为真，行为未实测。

排查表：

| 现象 | 可能原因 | 处理 |
| --- | --- | --- |
| 完全没有连接日志 | `KIANA_IDE` 为假值、处于 offline、没有 `.lock`、或 lockfile 条件不满足 | 检查环境变量与 `~/.kiana/ide`（或 `KIANA_IDE_DIR`） |
| `Warning: failed to connect to IDE` | IDE 服务未启动、端口错误、token 错误、`initialize` 超时 | 确认 IDE 插件监听 `127.0.0.1:<port>/mcp`，并重启 kiana |
| 提示 WebSocket transport 未实现 | lockfile `transport` 为 `ws` | 改用 HTTP 传输的插件，或禁用 IDE 集成 |
| 启动变慢 | lockfile 匹配但服务不可达，`initialize` 等满 10 秒 | 修复 IDE 服务，或设置 `KIANA_IDE=0` |
| 没有 diff 弹窗 | 当前没有工具调用 `openDiff` | 使用 `edit` / `write` 工具，或等待后续接线 |
| 其它项目的 IDE 失联 | 启动清理删除了 workspace 不匹配的 lockfile | 为每个项目设置独立的 `KIANA_IDE_DIR` |

## 相关页面

- [CLI 参考](cli.md)：`--offline` 与运行模式。
- [设置（settings）](settings.md)：项目信任与设置分层。
- [权限与审批](permissions.md)：`trust` 与工具审批的关系。
- [内置工具](tools.md)：当前实际使用的文件编辑工具。
- [无头模式](headless.md)：在 CI 中关闭 IDE 与其它启动期动作。
