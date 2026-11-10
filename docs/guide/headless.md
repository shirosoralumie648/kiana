# 无头模式（`-p`、`--mode json`、RPC）

本页讲什么：怎样在脚本、CI 和编辑器插件里非交互地运行 kiana。内容包括 `-p` / `--print` 的文本输出、`--mode json` 的 JSONL 事件流、`--mode rpc` 的 JSONL 命令协议，以及 stdin、`@file`、退出码和管道注意事项。flag 的完整清单见 [CLI 参考](cli.md)。

> 版本说明：workspace 包版本是 `0.1.0`，但 `kiana --version` 输出 `1.4.0`。后者来自 `kiana-core` 的 TS 兼容常量（`crates/kiana-core/src/config.rs` 的 `VERSION`），是有意保留的差异，不是构建错误。`v0.1.0-alpha` 只是本地产物目录名，不是 git tag。

## 快速开始

```bash
# 一次性文本运行，回答写到 stdout
kiana -p "总结这个仓库的目录结构"

# 管道输入作为初始消息
cat error.log | kiana -p "分析这个错误"

# 管道输入 + 提示词（两者直接拼接，没有自动分隔符；需要时自己加换行）
{ git diff; printf '\n请审查上面的 diff\n'; } | kiana -p

# 附带文件
kiana -p @src/main.rs @Cargo.toml "这两个文件如何协作？"

# 以短横线开头的提示词：用 -- 结束选项解析
kiana -p -- "- 总结以下三点：性能、可维护性、风险"

# CI：不写会话文件，并关闭启动期网络操作
kiana --no-session --offline -p "运行 cargo check 并总结错误"
```

`--offline` 只关闭启动期网络操作（版本检查、模型目录刷新、IDE 连接等），不会阻止真正执行任务时的模型请求；模型凭证仍然必需。

## 模式是怎么选的

启动时按固定优先级选择运行模式（`crates/kiana-coding-agent/src/app.rs` 的 `resolve_app_mode`）：

| 优先级 | 条件 | 模式 |
| --- | --- | --- |
| 1 | `--mode rpc` | RPC |
| 2 | `--mode json` | JSON |
| 3 | `--print` / `-p`，或 stdin 不是 TTY，或 stdout 不是 TTY | Print（文本） |
| 4 | 以上都不满足 | Interactive（交互 TUI） |

几个容易踩的边界：

- `--mode text` 只是“输出模式为文本”，**不会**强制非交互。终端里单独运行 `kiana --mode text` 仍然进入交互模式；要一次性运行请加 `-p`，或让 stdin/stdout 不是 TTY。
- 管道 stdin 会把原本的交互模式自动降级为 Print。
- `--mode rpc` 优先于 `--print`；`--serve` 是另一条独立路径，见 [Daemon 事件服务](daemon.md)。

三种非交互模式的输出面：

| 模式 | stdout | stderr | 典型退出码 |
| --- | --- | --- | --- |
| Print（`text`） | 最后一条 assistant 消息的 text 块 | 诊断、错误、警告 | `0` / `1` / `143` / `129` |
| JSON（`--mode json`） | 一行会话头 + 每行一个事件对象 | 诊断、错误、警告 | `0` / `1` / `143` / `129` |
| RPC（`--mode rpc`） | `response`、会话事件、`extension_ui_request` | 诊断、扩展错误、警告 | `0` / `143` / `129` |

## 输入：stdin 与 `@file`

### stdin

- 只在**非 RPC** 模式下读取；RPC 的 stdin 是命令通道，不是提示词。
- stdin 是 TTY 时不读取；不是 TTY 时读尽整个流并做 JS 语义的 `trim()`。
- 空内容按“没有 stdin 输入”处理。
- 初始消息的拼接顺序固定为：**管道 stdin → `@file` 展开文本 → 第一条位置消息**。三者用空字符串直接拼接，不自动插入换行。
- 第一条位置消息会被并入初始 prompt；其余位置消息在初始 prompt 之后**逐条**串行执行。
- 在文本模式下，无论中间执行了多少轮工具或消息，stdout 只输出**最后一条** assistant 消息的 text 块；需要中间过程请用 `--mode json`。

### `@file`

```bash
kiana -p @README.md "用三句话概括"
kiana @prompt.md @image.png "这张图和文档有什么关系？"
```

| 行为 | 说明 |
| --- | --- |
| 文本文件 | 展开为 `<file name="绝对路径">\n内容\n</file>\n` 后拼进初始消息 |
| 图片 | 作为图片附件随第一条消息发送；CLI 的 `@file` 路径不做自动缩放 |
| 空文件 | 静默跳过，不产生文本或附件 |
| 文件不存在 | stderr 输出 `Error: File not found: <绝对路径>`，退出 `1` |
| RPC 模式 | 直接拒绝，stderr 输出 `Error: @file arguments are not supported in RPC mode`，退出 `1` |
| `--` 之后 | `--` 后的 `@...` 仍按文件参数处理，其余 token 按消息处理 |

## Print 模式的输出与退出码

文本模式只检查会话中的**最后一条**消息：

| 场景 | stdout | stderr | 退出码 |
| --- | --- | --- | --- |
| assistant 正常结束（`stop` / `toolUse` 等） | 每个 text 块后跟一个换行 | — | `0` |
| 没有消息，或最后一条不是 assistant | 不输出 | — | `0` |
| assistant `stopReason` 为 `error` | 不输出文本 | `errorMessage`，为空时 `Request error` | `1` |
| assistant `stopReason` 为 `aborted` | 不输出文本 | `errorMessage`，为空时 `Request aborted` | `1` |
| assistant `stopReason` 为 `length` | 已生成的 text 块照常输出 | `Response was truncated: the model reached its output token limit.` | `1` |
| 本地预算耗尽（turn / token / cost / wall-clock） | 已生成的 text 块照常输出 | `Request stopped: the <kind> budget was exhausted.` | `1` |
| 执行 prompt 时抛错 | 可能已有部分输出 | 错误文本 | `1` |

JSON 模式不同：assistant 以 `error` 结束**不会**改变退出码，错误只出现在事件流的 `stopReason` / `errorMessage` 里；只有宿主或执行路径抛错才返回 `1`（已用连接失败的运行核对：事件流包含 `"stopReason":"error"`，进程仍退出 `0`）。

Print / JSON 模式的信号处理：

| 信号 | 退出码 | 说明 |
| --- | --- | --- |
| `SIGTERM` | `143` | 只处理 Unix；信号路径不等待输出队列冲刷 |
| `SIGHUP` | `129` | 只处理 Unix |

## JSON 事件流（`--mode json`）

```bash
kiana --mode json --no-session -p "列出顶层目录" > events.jsonl
```

- stdout 是 **JSON Lines**：每行一个完整 JSON 对象，以 LF 分隔，不是 JSON 数组。
- 有会话头时，第一行是原始会话头对象，例如：
  `{"type":"session","version":3,"id":"...","timestamp":"2026-11-08T10:50:03.993Z","cwd":"/repo"}`
- 之后是该次运行的全部 `AgentSessionEvent`，每行一个。事件至少包含 `type` 字段。
- `message_update` 是投影后的记录：没有 `message` 键，也没有内层 `partial` 键；标准消息通常带 `usage`，并一定带 `assistantMessageEvent`。内层 `assistantMessageEvent.type` 可能为：
  `start`、`text_start`、`text_delta`、`text_end`、`thinking_start`、`thinking_delta`、`thinking_end`、`toolcall_start`、`toolcall_delta`、`toolcall_end`、`done`、`error`。
- stdout 被协议占用后，普通日志和诊断走 stderr；不要把它们混进 JSON 解析。

常用事件分组：

| 类别 | `type` |
| --- | --- |
| 运行生命周期 | `agent_start`、`agent_end`、`agent_settled`、`turn_start`、`turn_end` |
| 消息 | `message_start`、`message_update`、`message_end` |
| 工具与 bash | `tool_execution_start`、`tool_execution_update`、`tool_execution_end`、`bash_execution_update` |
| 队列与会话 | `queue_update`、`entry_appended`、`session_info_changed`、`thinking_level_changed` |
| 压缩与重试 | `compaction_start`、`compaction_end`、`condensation`、`auto_retry_start`、`auto_retry_end`、`summarization_retry_scheduled`、`summarization_retry_attempt_start`、`summarization_retry_finished` |
| 权限与钩子 | `permission_requested`、`permission_decided`、`permission_denied`、`hook_notification` |

用 `jq` 读取结果：

```bash
# 只取最终的 assistant 文本
kiana --mode json --no-session -p "回答问题" \
  | jq -r 'select(.type=="message_end" and .message.role=="assistant") | .message.content[]? | select(.type=="text") | .text'

# 查看是否发生错误
kiana --mode json --no-session -p "回答问题" \
  | jq -c 'select(.type=="message_end" and .message.role=="assistant" and .message.stopReason=="error")'

# 只看工具调用
kiana --mode json --no-session -p "列出文件" \
  | jq -c 'select(.type=="tool_execution_start") | {toolName, args}'
```

注意：`message_start` 里会出现完整的 system prompt，体积可能很大；做日志采集时建议先按 `type` 过滤。

## RPC 模式（`--mode rpc`）

RPC 供编辑器插件、SDK 和长生命周期脚本使用：进程保持运行，stdin 收 JSONL 命令，stdout 写 JSONL 记录。

### `kiana-rpc` 与 `kiana --mode rpc` 的区别

| 项 | `kiana --mode rpc` | `kiana-rpc` |
| --- | --- | --- |
| 入口 | 主二进制 | 独立二进制（`dist/v0.1.0-alpha/kiana-rpc`） |
| 参数处理 | 按你给的参数 | 在用户参数前自动追加 `--mode rpc` |
| 进程标题 | `kiana` | `kiana-rpc`（Linux 写在 `/proc/self/comm`） |
| `--help` / `--version` | 正常 | 与 `kiana` 相同 |
| `--mode` 覆盖 | 按最后一次 `--mode` | 用户再传 `--mode text` / `json` 会覆盖强制的 `rpc`；需要 RPC 的调用方不要传 `--mode` |
| stdout | 协议字节 | 协议字节 |
| stderr | 日志、诊断、扩展错误 | 日志、诊断、扩展错误 |

两者都把普通输出改道 stderr，真实 stdout 只留给协议记录。`kiana-rpc` 同样拒绝 `@file`。

### 协议帧

命令（客户端 → kiana，每行一个 JSON 对象）：

```json
{"type":"get_state","id":"1"}
{"type":"prompt","id":"2","message":"列出顶层目录"}
{"type":"abort","id":"3"}
```

响应（kiana → 客户端）：

```json
{"id":"1","type":"response","command":"get_state","success":true,"data":{"model":{"id":"<model-id>","provider":"<provider>","api":"<api>"},"thinkingLevel":"high","isStreaming":false,"isCompacting":false,"steeringMode":"one-at-a-time","followUpMode":"one-at-a-time","sessionId":"...","autoCompactionEnabled":true,"messageCount":0,"pendingMessageCount":0}}
{"id":"2","type":"response","command":"prompt","success":false,"error":"..."}
```

响应字段：

| 字段 | 说明 |
| --- | --- |
| `id` | 原样回带客户端发送的 `id`（任意 JSON 值）；解析错误记录没有 `id` |
| `type` | 固定为 `response` |
| `command` | 对应的命令名；解析失败时为 `parse` |
| `success` | 布尔值 |
| `data` | 成功时的数据；没有数据时省略该键；显式 `null` 会保留为 `"data":null` |
| `error` | 失败时的错误文本 |

`prompt` 的响应只表示“已受理/已排队”，`data.disposition` 为 `handled`、`queued` 或 `started`；如果触发了模型运行，输出通过随后的事件流返回，收到 `agent_settled` 表示该次运行结束。`prompt`、`steer`、`follow_up` 的 `message` 必须是字符串；`images` 原样透传。

最小示例：

```bash
printf '%s\n' '{"type":"get_state","id":"1"}' \
  | kiana --offline --no-session --mode rpc
```

用一个 Python 客户端持续交互：

```python
import json, subprocess

proc = subprocess.Popen(
    ["kiana", "--offline", "--no-session", "--mode", "rpc"],
    stdin=subprocess.PIPE, stdout=subprocess.PIPE, text=True,
)
proc.stdin.write(json.dumps({"type": "get_state", "id": "1"}) + "\n")
proc.stdin.flush()
print(json.loads(proc.stdout.readline()))
proc.stdin.close()          # EOF：立即关停，不等待在途命令
print("exit", proc.wait())
```

关闭 stdin 会立即触发关停，并中止在途命令；要等一次 `prompt` 完成，请先读到 `agent_settled`，再关闭 stdin。

### 命令表（33 个 `type`）

| 分组 | 命令 | 主要字段 |
| --- | --- | --- |
| 提示词 | `prompt` | `message`、`images?`、`streamingBehavior?` |
| 提示词 | `steer` | `message`、`images?` |
| 提示词 | `follow_up` | `message`、`images?` |
| 提示词 | `abort` | — |
| 提示词 | `clear_queue` | — |
| 会话 | `get_state` | — |
| 会话 | `get_messages` | — |
| 会话 | `get_entries` | `since?`（起始条目 id） |
| 会话 | `get_tree` | — |
| 会话 | `get_fork_messages` | — |
| 会话 | `get_last_assistant_text` | — |
| 会话 | `get_commands` | — |
| 会话 | `get_session_stats` | — |
| 会话 | `new_session` | `parentSession?` |
| 会话 | `switch_session` | `sessionPath` |
| 会话 | `fork` | `entryId` |
| 会话 | `clone` | — |
| 会话 | `set_session_name` | `name` |
| 模型 | `set_model` | `provider`、`modelId` |
| 模型 | `cycle_model` | — |
| 模型 | `get_available_models` | — |
| 模型 | `set_thinking_level` | `level` |
| 模型 | `cycle_thinking_level` | — |
| 模型 | `get_available_thinking_levels` | — |
| 队列与设置 | `set_steering_mode` | `mode`：`all` / `one-at-a-time` |
| 队列与设置 | `set_follow_up_mode` | `mode`：`all` / `one-at-a-time` |
| 队列与设置 | `set_auto_compaction` | `enabled` |
| 队列与设置 | `set_auto_retry` | `enabled` |
| 队列与设置 | `compact` | `customInstructions?` |
| 执行与导出 | `bash` | `command`、`excludeFromContext?` |
| 执行与导出 | `abort_bash` | — |
| 执行与导出 | `abort_retry` | — |
| 执行与导出 | `export_html` | `outputPath?` |

所有命令都可以带 `id`。每条命令的 `id` 是响应回带的唯一依据；并发发送多条命令时按 `id` 匹配。

### 事件与扩展 UI

- RPC 的 stdout 同时包含 `response` 与会话事件；会话事件与 `--mode json` 的 `AgentSessionEvent` 相同，客户端必须按 `type` 区分。
- RPC 模式下扩展拥有 UI：kiana 会写出 `extension_ui_request`（`method` 为 `select`、`confirm`、`input`、`editor`、`notify`、`setStatus`、`setWidget`、`setTitle`、`set_editor_text`），客户端用 `{"type":"extension_ui_response","id":"..."}` 回传，并按请求携带 `value`、`confirmed` 或 `cancelled` 字段。print/json 模式没有 UI，扩展的交互请求会失败或退化为空。
- 无法解析的行会得到 `command:"parse"` 的错误响应；未知命令返回 `success:false` 与 `Unknown command: <name>`，进程继续运行。

### 关停与退出码

| 触发 | 退出码 | 说明 |
| --- | --- | --- |
| stdin EOF | `0` | 立即关停，中止在途命令 |
| 扩展 `shutdownHandler` | `0` | 延迟到 `agent_settled` 或当前命令处理完之后 |
| `SIGTERM` | `143` | 不等待输出队列冲刷 |
| `SIGHUP` | `129` | 会冲刷输出队列 |
| 启动/参数错误 | `1` | 在进入 RPC 循环之前退出 |

## 限制与排查

- **无头模式没有交互审批**。print/json 模式不绑定 UI 上下文；需要审批的工具调用会被拒绝，工具结果中的提示是 `No interactive UI is available to approve this action.`，不存在 `--yes` 这类自动批准开关。请在启动时用 `--allow` 规则或 `--permission-mode` 预先放行，例如：
  ```bash
  kiana --no-session -p --permission-mode accept-edits \
    --allow 'command:cargo test' --allow 'command:cargo check' "修复失败测试"
  ```
  规则语法与风险说明见 [权限与审批](permissions.md)。
- **非交互模式需要可用模型**。解析不到模型时会在执行 prompt 前以 `1` 退出。
- **`--mode text` 不等于 `--print`**。脚本里请显式使用 `-p`，或保证 stdin/stdout 不是 TTY。
- **`--help` / `--list-models` 与 `--print` 或 `--mode` 同用时**，stdout 已被协议占用，帮助文本会写到 stderr，stdout 为空。需要干净帮助请单独运行 `kiana --help`。
- **JSON 模式的退出码不反映模型错误**。请解析事件中的 `stopReason` / `errorMessage`；只依赖退出码会把失败的运行当成成功。
- **文本模式只输出最后一条 assistant 消息**。中间的工具调用、思考块、前几轮消息都不会出现在 stdout。
- **RPC 模式不接受 `@file`**，且 stdin 是命令通道，管道内容不会作为提示词。
- **信号退出码只覆盖 Unix 的 `SIGTERM` / `SIGHUP`**；当前验证平台是 Linux x86_64。
- `--mode` 非法时输出 `Error: Invalid mode "<value>". Valid values: text, json, rpc`；缺值时输出 `Error: --mode requires text, json, or rpc`，两者都退出 `1`。

## 相关页面

- [CLI 参考](cli.md)：全部 flag、子命令、环境变量和通用退出码。
- [快速开始](getting-started.md)：安装、首次认证与第一次运行。
- [权限与审批](permissions.md)：`--allow` / `--deny` / `--permission-mode` 的规则语法。
- [交互模式（TUI）](interactive-mode.md)：交互界面、斜杠命令与快捷键。
- [Daemon 事件服务](daemon.md)：把会话事件暴露给其它进程。
