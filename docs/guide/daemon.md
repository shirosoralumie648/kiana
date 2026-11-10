# Daemon 事件服务（`--serve`）

本页讲什么：怎样用 `kiana --serve` 把一个会话的事件流暴露成本机 HTTP 服务。内容包括启动参数、bearer token、SSE / WebSocket / REST 三个端点、事件与 durable/volatile 通道，以及当前实现的限制。

## 快速开始

```bash
# 启动 daemon；token 自动生成并打印到 stderr
kiana --serve

# 指定监听地址与固定 token
kiana --serve --serve-addr 127.0.0.1:8787 --serve-token my-token
```

stderr 会打印类似内容（端口 `0` 时由系统分配）：

```text
Daemon listening on http://127.0.0.1:39613
Bearer token: <token>
Endpoints: GET /events (SSE or WebSocket), GET /events/latest?limit=N, GET /healthz
```

`--serve` 持续运行，收到 `SIGINT`（终端里的 Ctrl-C）后打印 `Daemon shutting down.` 并以 `0` 退出。stdin EOF **不会**停止 daemon。

### 用 curl 访问

```bash
ADDR=http://127.0.0.1:8787
TOKEN=my-token

# 健康检查：不需要 token
curl -s "$ADDR/healthz"          # ok

# 最新一页 durable 事件
curl -s -H "Authorization: Bearer $TOKEN" "$ADDR/events/latest?limit=20"

# SSE：从头补放 durable 事件并保持连接
curl -N -H "Authorization: Bearer $TOKEN" "$ADDR/events?since=0"

# 浏览器 EventSource / WebSocket 无法设置请求头，可用 query token
curl -N "$ADDR/events?token=$TOKEN"
```

SSE 帧格式（连接建立后的第一条事件）：

```text
id: 0
event: server.connected
data: {"seq":0,"type":"server.connected","data":{"heartbeatIntervalSeconds":15}}
```

WebSocket 使用同一个 URL，带 `Upgrade: websocket` 与 `Sec-WebSocket-Key` 即可：

```text
ws://127.0.0.1:8787/events?token=my-token&since=0
```

WebSocket 每一帧是一条预编码的 JSON 对象，不是 SSE 文本。

## 启动参数

| Flag | 取值 | 默认 / 说明 |
| --- | --- | --- |
| `--serve` | 无 | 启动 daemon 事件服务；`SIGINT` 退出 |
| `--serve-addr <host:port>` | 数字 IP + 端口 | 默认 `127.0.0.1:0`（系统分配端口）。只接受 `SocketAddr` 语法，`localhost:8787` 会报 `invalid socket address syntax` |
| `--serve-token <token>` | 字符串 | 覆盖自动生成的 bearer token；空串按未提供处理，仍会自动生成 |

token 生成规则：

- 优先读 `/dev/urandom` 的 32 字节并写成 64 个十六进制字符。
- `/dev/urandom` 不可用时退回“时间戳 + pid + 进程内计数器”，不是密码学安全的随机源。
- CLI 启动的 daemon **始终**设置 token，没有“关闭鉴权”的开关。
- token 只打印到 stderr；不持久化、不轮换。重启后若未给 `--serve-token` 会得到新 token。

## 端点参考

| 端点 | 方法 | 鉴权 | 说明 |
| --- | --- | --- | --- |
| `/healthz` | GET | 免鉴权 | 返回纯文本 `ok`，供进程健康检查 |
| `/events` | GET | 需要 | `Accept: text/event-stream` 或普通 GET 走 SSE；带 `Upgrade: websocket` 与 `Sec-WebSocket-Key` 时走 WebSocket |
| `/events/latest?limit=N` | GET | 需要 | 返回最新一页 durable 事件的 JSON |
| 其它路径 | 任意 | — | `404 not found` |
| 任意路径 | 非 GET | — | `405 method not allowed` |

### `/events/latest`

```json
{"events":[],"since":0,"has_more":false}
```

| 字段 | 说明 |
| --- | --- |
| `events` | durable 事件数组，按 `seq` 升序；默认最多 200 条 |
| `since` | 本页最后一条事件的 `seq`；空页时为当前最新序号。把它作为 `?since=` 传给 `/events` 即可续传 |
| `has_more` | 是否还有更早的 durable 事件未返回 |

`limit` 的取值会被夹到 `1..10000`；非数字按默认值 200 处理。

### 鉴权

- 请求头：`Authorization: Bearer <token>`（也接受小写 `bearer` 前缀）。
- 查询参数：`?token=<token>`，用于浏览器 `EventSource` / WebSocket 这类无法设置请求头的客户端。
- 比较使用与长度无关的逐字节比较。
- `/healthz` 有意免鉴权。
- `?token=` 会做 URL 解码，`+` 会被解码为空格；自定义 token 建议只用字母数字，或改用请求头。

## 事件与通道

每条事件的 wire 形状（REST 与 WebSocket 直接返回该对象；SSE 把同样内容放进 `data:`）：

```json
{"seq":1,"channel":"durable","type":"...","data":{...}}
```

| 字段 | 说明 |
| --- | --- |
| `seq` | feed 内的单调序号，从 `1` 开始；`server.connected` 使用当前最新序号（初始为 `0`），不占用新序号 |
| `channel` | `durable` 或 `volatile` |
| `type` | 事件类型。当前会话事件的取值见下面的“当前实现限制” |
| `data` | 事件载荷。会话事件在这里保留原始事件对象，含它自己的 `type` |

`server.connected` 是手工构造的固定事件，JSON 里没有 `channel` 字段；`server.heartbeat` 与其它通过 feed 发布的事件都带 `channel`。

### 固定事件

| 事件 | 通道 | 说明 |
| --- | --- | --- |
| `server.connected` | volatile | 每个新连接的第一条事件，`data.heartbeatIntervalSeconds` 为 `15` |
| `server.heartbeat` | volatile | 心跳，`data.at` 为 Unix 毫秒时间戳；SSE 连接每约 15 秒触发一次 |

### 会话事件

daemon 订阅启动它的那一个会话，把每个 `AgentSessionEvent` 序列化一次后发布。可能出现的 `data.type` 值为：

| 类别 | 事件 `type` |
| --- | --- |
| 运行生命周期 | `agent_start`、`agent_end`、`agent_settled`、`turn_start`、`turn_end` |
| 消息 | `message_start`、`message_update`、`message_end` |
| 工具与 bash | `tool_execution_start`、`tool_execution_update`、`tool_execution_end`、`bash_execution_update` |
| 队列与会话 | `queue_update`、`entry_appended`、`session_info_changed`、`thinking_level_changed` |
| 压缩与重试 | `compaction_start`、`compaction_end`、`condensation`、`auto_retry_start`、`auto_retry_end`、`summarization_retry_scheduled`、`summarization_retry_attempt_start`、`summarization_retry_finished` |
| 权限与钩子 | `permission_requested`、`permission_decided`、`permission_denied`、`hook_notification` |

### 当前实现限制：事件名与通道

`crates/kiana-coding-agent/src/app.rs` 的 `run_daemon_mode` 用 `serde_json::to_value(event)` 后的**第一个键**作为事件名。`AgentSessionEvent` 是内部 tag 枚举，`type` 键最先写出（在 `--mode json` 的输出里可以观察到每条事件都以 `"type":...` 开头）。因此当前版本：

- 会话事件发布到 feed 时，外层 `type` 实际是 `"type"`，真实事件类型在 `data.type`；
- SSE 的 `event:` 行也是 `type`，WebSocket/REST 的外层 `type` 同样是 `type`；
- `channel_for_event_type` 只把以 `.delta` / `.update`（带点）结尾的名字判为 volatile，而当前会话事件名使用下划线（`message_update`、`tool_execution_update` 等），因此会话事件实际都进入 **durable**。

客户端请按 `data.type` 分派事件，不要依赖外层 `type` 或 SSE 的 `event:` 行。`server.connected` 与 `server.heartbeat` 是手工构造/发布的固定事件，名字不受影响。

### 续传与补放

- `/events?since=N`：只补放 `seq > N` 的 **durable** 事件；volatile 事件从不补放。
- `server.connected` 总是先入队，然后是补放的 durable 事件，最后是实时事件。
- durable replay log 容量为 4096。`since` 早于 replay 起点时无法补齐，可能产生 gap；REST 的 `has_more` 可用于重新拉取最新页。
- 每个订阅者的队列容量为 4096。队列满时只丢弃**该订阅者**并结束其连接，不会阻塞其它客户端。

## 限制与排查

- **只读事件流，没有命令入口。** daemon 只提供上表中的 GET 端点；不能通过 HTTP 发送 prompt、abort 或切换会话。它只转发该会话之后产生的事件（例如该进程内的 cron 任务触发的运行）。
- **只服务一个会话。** 没有多会话注册表；要观察另一个会话需要另起一个 `--serve` 进程。
- **非交互启动要求可用模型。** `--serve` 不改变 `AppMode`；当以 `--print`、`--mode json/rpc` 或非 TTY 方式启动时，会话解析不到模型会在进入 daemon 前以 `1` 退出。即使 daemon 启动成功，真正执行模型请求仍需要可用的 provider 凭证。
- **没有 TLS / 多用户鉴权。** 默认只绑定 loopback。绑定到非 loopback 地址时，必须自己在前面加 TLS 与额外认证；所有客户端共享同一个 bearer token。
- **token 不轮换、不持久化。** 只打印到 stderr；用 `--serve-token` 传入的 token 还会出现在进程列表中，共享主机上需要注意。
- **只处理 SIGINT。** 其它信号按操作系统默认动作处理；stdin EOF 不会停止 daemon。
- **事件名与通道的当前行为**见上一节；不要依赖外层 `type`/SSE `event:` 过滤会话事件。
- **`--serve-addr` 只接受数字 IP。** `localhost:8787`、主机名都会报 `Error: invalid --serve-addr ...` 并退出 `1`。
- **SSE 慢客户端会被断开。** 队列溢出时连接结束；重连时用最后收到的 `seq` 作为 `since`。

常见现象与处理：

| 现象 | 可能原因 | 处理 |
| --- | --- | --- |
| `401 unauthorized` | 没带 token、token 错误，或 `?token=` 中的 `+` 被解码为空格 | 用 `Authorization: Bearer`，或换用不含特殊字符的 token |
| 连接被拒绝 | 地址/端口不对，或进程已退出 | 检查 stderr 打印的实际地址 |
| `/events` 只有 `server.connected` | 会话尚未产生事件；daemon 没有 prompt 入口 | 在该进程内配置 cron 等触发源，或等待当前会话产生事件 |
| SSE 连接一段时间后断开 | 客户端消费太慢导致队列溢出 | 重连并用最后 `seq` 作为 `since` |
| 续传缺事件 | `since` 早于 4096 条 replay 窗口，或事件是 volatile | 用 `/events/latest` 重新取最新页 |

## 相关页面

- [CLI 参考](cli.md)：`--serve*` flag 与通用退出码。
- [无头模式](headless.md)：JSON 事件流的字段与事件类型。
- [遥测、离线与外部服务](telemetry-and-offline.md)：`--offline` / `KIANA_OFFLINE` 的范围。
- [权限与审批](permissions.md)：daemon 观察到的权限事件背后的审批规则。
