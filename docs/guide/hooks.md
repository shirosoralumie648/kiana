# Hooks（钩子）

本页讲什么：如何用 settings 里的 `hooks` 对象，在工具调用前后、权限拒绝、prompt 提交、会话开始/结束、回合结束和上下文压缩前后执行本地命令或 HTTP 请求；以及事件、matcher、退出码、JSON 输出契约、超时与排查方法。

版本说明：workspace 包版本是 `0.1.0`，但 `kiana --version` 输出 `1.4.0`（有意保留的 TS 兼容常量，见 `crates/kiana-core/src/config.rs`），两者不一致不是构建错误。

## 快速开始

### 最小配置

把下面的 JSON 写进用户全局 settings（默认 `~/.kiana/agent/settings.json`），或项目里的 `.kiana/settings.json`（项目需已受信）：

```json
{
  "hooks": {
    "PreToolUse": [
      {
        "matcher": "bash",
        "hooks": [
          {
            "type": "command",
            "command": "$KIANA_PROJECT_DIR/.kiana/hooks/guard.sh",
            "timeout": 30
          }
        ]
      }
    ]
  }
}
```

保存后需要重新启动会话才会生效（`/reload` 当前不会重载 hooks，见“限制与排查”）。

### 一个可以阻断工具的 hook

`.kiana/hooks/guard.sh`：

```sh
#!/bin/sh
input=$(cat)

# 演示用：真实项目请用 JSON 解析器读取 tool_input，不要依赖字符串匹配。
case "$input" in
  *'"command":"rm -rf /"'*)
    echo "refusing to run a destructive command" >&2
    exit 2
    ;;
esac

exit 0
```

```bash
chmod +x .kiana/hooks/guard.sh
```

行为：

- 退出码 `0`：放行，stdout 按 JSON 或纯文本解析（见“输出与退出码”）。
- 退出码 `2`：阻断本次工具调用，stderr 作为阻断原因返回给模型。
- 其它退出码：只记警告，不改变决策。

### 一个给模型补充上下文的 hook

`UserPromptSubmit` 的纯文本 stdout 会作为 `additionalContext` 追加到用户消息：

```json
{
  "hooks": {
    "UserPromptSubmit": [
      {
        "hooks": [
          { "type": "command", "command": "$KIANA_PROJECT_DIR/.kiana/hooks/context.sh" }
        ]
      }
    ]
  }
}
```

```sh
#!/bin/sh
cat >/dev/null
printf '%s\n' 'This repository uses pnpm, not npm.'
exit 0
```

### 手动验证一个 command hook

hook 的 stdin 是一行 JSON；可以脱离 kiana 直接调用脚本：

```bash
cat <<'JSON' | sh .kiana/hooks/guard.sh
{"session_id":"s1","cwd":"/path/to/project","hook_event_name":"PreToolUse","permission_mode":"default","tool_name":"bash","tool_input":{"command":"ls"}}
JSON
echo "exit=$?"
```

`exit=0` 表示放行；构造一个命中规则的 `tool_input` 应得到 `exit=2`，stderr 就是阻断原因。

## 配置参考

### 配置位置

`hooks` 是 settings 的顶层对象。会话创建时按配置层读取：

| 层 | 文件 | 说明 |
| --- | --- | --- |
| User | `<agentDir>/settings.json`，默认 `~/.kiana/agent/settings.json` | 总是加载；`KIANA_CODING_AGENT_DIR` 可改 agent 目录 |
| Project | `<cwd>/.kiana/settings.json` | 仅项目受信时加载 |
| Local | `<cwd>/.kiana/settings.local.json` | 仅项目受信时加载；会话的 Local 配置层会读取其中的 `hooks` |
| System / Managed / Enterprise | `/etc/kiana/*.json` | **当前实现不执行这里的 hooks**（`kiana-hooks` 没有 Managed 来源） |

`allowManagedHooksOnly`（顶层或 `permissions` 内，camelCase/snake_case 均可）为真时，当前实现会停用**全部** hooks，并写一条诊断；原因是还没有 managed hook 来源。这是当前的安全折衷，不是“只允许管理员 hook”。

### 顶层结构

```json
{
  "hooks": {
    "<EventName>": [
      {
        "matcher": "bash|write",
        "hooks": [
          { "type": "command", "command": "sh guard.sh", "timeout": 30, "async": false }
        ]
      }
    ]
  }
}
```

| 字段 | 类型 | 必填 | 说明 |
| --- | --- | --- | --- |
| `<EventName>` | array | 是 | 事件名；大小写不敏感（先精确匹配，再忽略大小写）。未知事件会被诊断并跳过 |
| `matcher` | string | 否 | 匹配当前事件的“主题字段”；缺省、空串、`*`、`.*` 都匹配全部 |
| `hooks` | array | 是 | 一个 matcher 组内的 handler，按声明顺序执行；空数组会产生诊断并被忽略 |

解析是逐条宽松的：未知事件、非法 matcher、缺 `hooks`、坏 handler、非法 URL、非法 `timeout` 都会产生诊断并跳过该条目，同一份 settings 里的其他 hook 照常加载。

### handler

`type` 缺省为 `command`；当前只支持 `command` 和 `http` 两种。

Command handler：

| 字段 | 类型 | 必填 | 默认 | 说明 |
| --- | --- | --- | --- | --- |
| `type` | string | 否 | `"command"` | 省略或写 `"command"` |
| `command` | string | 是 | — | 交给 `/bin/sh -c`（Windows 为 `cmd /C`）执行，cwd 固定为项目根 |
| `timeout` | number | 否 | `30` | 秒，可为小数；必须 `> 0` 且 `<= 3600` |
| `async` | bool | 否 | `false` | 为 `true` 时立即返回，结果只进审计，不参与本轮决策；仅 command 支持 |

HTTP handler：

| 字段 | 类型 | 必填 | 默认 | 说明 |
| --- | --- | --- | --- | --- |
| `type` | string | 是 | — | 必须为 `"http"` |
| `url` | string | 是 | — | 只允许 `http` / `https`；加载期做 URL 与 SSRF 校验 |
| `headers` | object | 否 | `{}` | header 名和值都必须是字符串；值中的 `$VAR` / `${VAR}` 只对 `allowedEnvVars` 生效 |
| `allowedEnvVars` | string[] | 否 | `[]` | 允许在 header 值中插值的环境变量；也接受 `allowed_env_vars` |
| `timeout` | number | 否 | `30` | 秒，可为小数；必须 `> 0` 且 `<= 3600`；覆盖整条重定向链 |

HTTP handler 不读取 `async` 字段。

HTTP hook 的全局策略来自合并后的有效 settings（顶层键优先，其次 `security.*`）：

| 键 | 类型 | 语义 |
| --- | --- | --- |
| `allowedHttpHookUrls` | string[] | URL 白名单；`*` 匹配任意字符、大小写不敏感、必须整串匹配。缺省不限制；`[]` 表示全部拒绝 |
| `httpHookAllowedEnvVars` | string[] | 全局变量白名单；存在时与每个 hook 的 `allowedEnvVars` 取交集；缺省不额外限制 |

## 事件类型

当前实现有 10 个事件（`crates/kiana-hooks/src/event.rs`）：

| 事件 | 触发时机 | matcher 字段 | 退出码 2 的行为 | 纯文本 stdout 的去向 |
| --- | --- | --- | --- | --- |
| `PreToolUse` | 工具执行前；hook 改写先于扩展 `tool_call` 与权限判断 | `tool_name` | 阻断工具调用，stderr 作为原因 | 只进审计（决策必须走 JSON） |
| `PostToolUse` | 工具成功执行后（扩展 `tool_result` 之前） | `tool_name` | stderr 作为模型反馈追加到工具结果，不撤销副作用 | `additionalContext`，追加到工具结果 |
| `PostToolUseFailure` | 工具执行失败后 | `tool_name` | 同上 | 同上 |
| `PermissionDenied` | 权限层拒绝工具调用后 | `tool_name` | stderr 作为模型反馈追加到拒绝原因 | 忽略 |
| `UserPromptSubmit` | 用户提交 prompt 后、发送给模型前；stdin 的 `prompt` 是技能/模板展开后的文本 | 无 | 阻止本次提交 | `additionalContext`，追加到用户消息 |
| `SessionStart` | 会话启动、绑定扩展时 | `source` | 忽略（只记警告） | `additionalContext`，进入下一次请求的 hidden context |
| `SessionEnd` | 会话关闭时（best-effort） | `reason` | 忽略（只记警告） | 忽略 |
| `Stop` | 模型准备结束回合时 | 无 | 拒绝结束回合，让模型继续工作 | 忽略 |
| `PreCompact` | 上下文压缩前 | `trigger`（`manual` / `auto`） | 手动压缩取消，自动压缩跳过 | `additionalContext`，追加到压缩指令 |
| `PostCompact` | 上下文压缩成功后 | `trigger` | 忽略（只记警告） | `additionalContext`，进入下一次请求的 hidden context |

没有 matcher 字段的事件（`UserPromptSubmit`、`Stop`）对任何 matcher 都视为匹配。

### matcher 规则

匹配对象是事件对应的字段值（工具名、`source`、`reason` 或 `trigger`）：

| matcher | 语义 |
| --- | --- |
| 缺省、空串、`*`、`.*` | 匹配全部 |
| 只含字母、数字、空格、`_`、`-`、`.`、`:` 或竖线 | 精确名匹配；竖线分隔多个名字（例如 bash 和 write）；`.` 是字面量，`mcp.foo` 不会匹配 `mcpXfoo` |
| 其它形式 | Rust `regex` 正则，例如 `^mcp__.*` |
| 非法正则 | 加载期诊断，整个 matcher 组被跳过 |

## hook 输入

command hook 的 stdin 是**一行 JSON**；HTTP hook 的 POST body 是同一个 JSON。`null` 字段会被省略。

```json
{
  "session_id": "…",
  "transcript_path": "/path/to/session.jsonl",
  "cwd": "/path/to/project",
  "hook_event_name": "PreToolUse",
  "permission_mode": "default",
  "tool_name": "bash",
  "tool_input": { "command": "ls" }
}
```

| 字段 | 出现的事件 | 说明 |
| --- | --- | --- |
| `session_id` | 全部 | 会话 ID |
| `transcript_path` | 有会话文件时 | 会话记录路径 |
| `cwd` | 全部 | 固定为项目根（不是调用方任意给出的值） |
| `hook_event_name` | 全部 | 事件名 |
| `permission_mode` | 权限引擎存在时 | `read-only` / `default` / `accept-edits` / `bypass` |
| `tool_name` | 工具事件 | 工具名 |
| `tool_input` | 工具事件 | 工具参数；`updatedInput` 会沿 hook 顺序更新这个字段 |
| `tool_response` | `PostToolUse` | 形如 `{"content": …, "details": …, "isError": false}` |
| `error` | `PostToolUseFailure` | 工具错误文本 |
| `prompt` | `UserPromptSubmit` | 用户提交的 prompt（技能/模板展开后） |
| `source` | `SessionStart` | `startup` / `reload` / `new` / `resume` / `fork` |
| `reason` | `SessionEnd`、`PermissionDenied` | 结束或拒绝原因 |
| `trigger` | `PreCompact`、`PostCompact` | `manual` / `auto` |
| `custom_instructions` | `PreCompact`、`PostCompact` | 压缩自定义指令（若提供） |

工具事件缺少非空 `tool_name` 时，hook 执行返回错误；`PreToolUse` 对该错误 fail-closed（阻断工具调用）。

## 输出与退出码

### command hook 退出码

| 退出码 | 效果 |
| --- | --- |
| `0` | 解析 stdout：空 → 无动作；`{` 开头 → 按 JSON 契约解析；其它 → 纯文本（仅部分事件作为 `additionalContext`） |
| `2` | 对 `PreToolUse` / `UserPromptSubmit` / `Stop` / `PreCompact` 表示阻断；对 `PostToolUse` / `PostToolUseFailure` / `PermissionDenied` 把 stderr 作为模型反馈；其它事件只记警告 |
| 其它数字 | 警告，决策保持原样 |
| 被信号终止 | 警告 |
| 超时 | 警告；kill 整个进程组（Unix） |
| 启动失败 | 警告 |

退出码 2 时，stdout 里如果还有 JSON，其中的 `systemMessage`、`updatedInput`、`additionalContext` 等**非决策**字段仍会应用；对会产生阻断的事件（`PreToolUse` / `UserPromptSubmit` / `Stop` / `PreCompact`），最终决策固定为 deny，避免同一个 hook 的 deny 被计数两次。

### JSON 输出

stdout（HTTP 为响应 body）trim 后以 `{` 开头才会按 JSON 解析；语法错误或字段类型不符只记警告并忽略。

```json
{
  "continue": true,
  "stopReason": "…",
  "suppressOutput": false,
  "systemMessage": "…",
  "decision": "allow",
  "reason": "…",
  "hookSpecificOutput": {
    "hookEventName": "PreToolUse",
    "permissionDecision": "allow",
    "permissionDecisionReason": "policy passed",
    "updatedInput": { "command": "ls" },
    "additionalContext": "…",
    "retry": false
  }
}
```

| 字段 | 类型 | 作用 |
| --- | --- | --- |
| `continue` | bool | `false` 表示结束当前执行流；默认 `true` |
| `stopReason` | string | `continue: false` 时展示/返回的停止原因 |
| `suppressOutput` | bool | `true` 时隐藏 `systemMessage`；警告不受影响，始终展示 |
| `systemMessage` | string | 展示给用户的状态行 |
| `decision` | string | `approve` / `allow` / `ask` / `block` / `deny`；`approve`/`allow` 只在 `PreToolUse` 生效 |
| `reason` | string | 决策原因；多个 deny 的原因用换行拼接 |
| `hookSpecificOutput.hookEventName` | string | 必须与当前事件一致，否则该 `hookSpecificOutput` 的事件专属字段被忽略并告警 |
| `hookSpecificOutput.permissionDecision` | string | `allow` / `ask` / `deny`，仅 `PreToolUse` 使用 |
| `hookSpecificOutput.permissionDecisionReason` | string | 权限决策原因 |
| `hookSpecificOutput.updatedInput` | object | 整体替换工具参数，仅 `PreToolUse`；必须是对象，否则告警并忽略 |
| `hookSpecificOutput.additionalContext` | string | 注入上下文；仅部分事件接受（见事件表） |
| `hookSpecificOutput.retry` | bool | 仅 `PermissionDenied` 使用；`true` 时在拒绝原因后追加“可以重试一次”的提示 |

### 决策合并与顺序

- 多个 hook 的决策按 `deny > ask > allow > passthrough` 合并；`deny` 原因按执行顺序用换行拼接。
- 同一事件的 hook 串行执行：来源顺序为 `local > project > user`，同一来源保持声明顺序。
- `updatedInput` 是**整体替换**而非 merge，并顺序传给下一个 hook 的 stdin；最终参数会先经过扩展 `tool_call` 改写，再进入权限判断。最终决策为 deny 时工具不会执行，改写参数也不会被使用。
- `permissionDecision: allow` 只能跳过权限层的 `Ask`，不能覆盖 deny 规则、灾难性命令检查或只读模式。
- 每轮（turn）有 8 次 deny 上限：第 9 次起 deny 降级为 `ask` 并产生警告。`Stop` 的 `ask` 不会继续回合（否则 hook 可以无限拖住回合）。
- 递归深度上限为 5：hook 子进程的 `KIANA_HOOK_DEPTH` 是当前深度 +1，超过上限返回错误；`PreToolUse` 对该错误 fail-closed。
- `continue: false` 的消费点因事件而异：`PreToolUse` 阻断并终止 run；`UserPromptSubmit` 报错不发送；`Stop` 结束回合；`PreCompact` 取消/跳过压缩；`PostCompact` 终止当前 run（压缩已提交）；`PostToolUse` 把 `stopReason` 追加到结果并终止；`SessionStart` / `SessionEnd` / `PermissionDenied` 只记录，不改变控制流。

## 执行环境与安全

### command hook

- shell：`/bin/sh -c`（Windows `cmd /C`）。
- cwd：固定的项目根；stdin 的 `cwd` 也是项目根。
- 环境变量：先 `env_clear()`，只保留白名单 `PATH`、`HOME`、`USER`、`LOGNAME`、`SHELL`、`TMPDIR`、`TEMP`、`TMP`、`LANG`、`LANGUAGE`、`LC_ALL`、`LC_CTYPE`、`TERM`、`PWD`（以及 Windows 的 `SystemRoot`、`SYSTEMROOT`、`COMSPEC`、`PATHEXT`、`WINDIR`）；provider API key 等敏感变量**不会**继承。
- 额外注入：

| 变量 | 值 |
| --- | --- |
| `KIANA_PROJECT_DIR` | 项目根 |
| `KIANA_HOOK_EVENT` | 事件名 |
| `KIANA_HOOK_SOURCE` | `local` / `project` / `user` |
| `KIANA_HOOK_DEPTH` | 当前深度 + 1 |

- 输出：stdout/stderr 各自最多保留 64 KiB（保留前 64 KiB 并追加截断标记）；超时先杀进程组（Unix），再兜底杀直接子进程。
- hook 本身**不在沙箱内**运行；项目/本地 hook 的项目信任是唯一供应链边界，不要在没有审查的情况下批准不受信项目。

### HTTP hook

- 方法 `POST`，默认 `Content-Type: application/json`；body 与 stdin 相同。
- `url` 只允许 `http`/`https`，不允许 URL 内嵌凭据；私网、链路本地、CGNAT、保留地址和云元数据地址（含 `169.254.169.254`、`100.100.100.200` 及元数据域名）被拒绝；**loopback（`127/8`、`::1`）显式允许**，方便本地策略服务。
- DNS 在 connect 阶段校验，防止 DNS rebinding；客户端显式禁用环境代理。
- 最多跟随 5 跳重定向；每一跳重新校验 URL/SSRF/白名单；跨源重定向会清空配置 header、丢弃 body 并转成 GET。
- header 值中的 CR/LF/NUL 会被清洗；不在 `allowedEnvVars` 里的 `$VAR` 替换为空串并告警。
- 响应 body 最多保留 64 KiB；非 2xx 只告警；2xx 的纯文本 body 会被忽略并告警（HTTP hook 必须返回 JSON）。

## 调试

### 在 TUI 里看什么

- `systemMessage` 以状态行展示；`suppressOutput: true` 时隐藏。
- hook 自身失败（exit 1、超时、非法 JSON、HTTP 非 2xx、白名单/SSRF 拒绝、deny 降级等）以警告展示，前缀是事件名，例如 `PreToolUse hook: …`。警告**不会**被 `suppressOutput` 隐藏。
- `continue: false` 的 `stopReason` 会作为通知展示。

### 审计记录

会话保留最近 256 条 hook 运行记录（内存环形缓冲），包含事件、来源、handler 类型、命令/URL 摘要、结果、退出码、HTTP 状态、耗时、是否 async 与警告。当前通过会话 API `hook_audit_records()` 读取（SDK/嵌入使用），**TUI 没有 `/hooks` 命令或审计面板**。

### 配置诊断

hook 配置的宽松解析诊断（未知事件、非法 matcher、坏 handler、非法 timeout、被拒绝的 URL、未受信来源被丢弃等）保存在会话的配置诊断里，可通过 `settings_config_diagnostics()` 读取。当前 TUI 没有专门的 hooks 诊断面板。

### 常见问题排查

| 现象 | 可能原因 | 处理 |
| --- | --- | --- |
| hook 完全不执行 | 事件名拼错；matcher 不匹配；项目未受信；`allowManagedHooksOnly: true`；改完 settings 没有重启 | 修正事件名/matcher；`--approve` 或信任项目；重启会话 |
| 决策没有生效 | stdout 不是以 `{` 开头的合法 JSON；字段类型不对；`hookEventName` 与当前事件不一致；在非 `PreToolUse` 事件上设置 `permissionDecision` / `updatedInput` | 用上面的“手动验证”命令检查 stdout；对照 JSON 契约 |
| 工具被意外阻断 | 某个 matcher 组以退出码 2 结束；stderr 就是原因 | 检查所有匹配的 hook；临时把 matcher 收窄 |
| 出现 `timed out` 警告 | `timeout` 太小或脚本卡住 | 调大 `timeout`（最大 3600 秒）；确认脚本不等待交互输入 |
| 上下文没有注入 | 该事件不接受 `additionalContext`；纯文本 stdout 在该事件只进审计 | 改用接受上下文的事件，或使用 `hookSpecificOutput.additionalContext` |
| 修改 hooks 后不生效 | `/reload` 不会重建 hook runner | 重新启动 kiana |

## 限制与排查

- **没有 managed/system hooks**：`HookSource` 只有 `Local` / `Project` / `User`；`allowManagedHooksOnly: true` 会停用全部 hooks，而不是只允许管理员 hooks。
- **配置变更需要重启**：hook runner 在会话创建时构建，`/reload` 只重载 keybindings、extensions、skills、prompts、themes 和上下文文件，不会重载 hooks。
- **async 仅 command**：`{"async": true}` 的 command hook 立即返回，结果只进审计，不参与本轮决策；没有全局并发上限；进程立刻退出时最后一条审计可能丢失。运行时返回 `{"async":true}` 再转后台的协议没有实现。
- **HTTP 代理**：为保证 SSRF 校验可靠，HTTP hook 显式禁用环境代理，企业代理环境下不可用。
- **Windows**：命令 shell 是 `cmd /C`，但超时杀进程组只在 Unix 实现；Windows 依赖 `kill_on_drop` 杀直接子进程，没有 job object。
- **SessionEnd 是 best-effort**：只在正常 shutdown 路径触发（reason 为 `shutdown`）；进程被强杀时不保证执行。
- **递归与 deny 上限是固定默认值**：深度 5、每轮 deny 8 次，当前没有 settings 键可调整。
- **非交互模式的 `ask`**：hook 返回 `ask` 时复用权限审批 UI；没有 UI（如 `--print`）时按拒绝处理（fail-closed）。
- **hook 不受沙箱保护**：command hook 以当前用户身份运行；请只信任你审查过的项目配置。
- 当前验证范围是 Linux x86_64；其它平台未实测。

## 相关页面

- [权限与审批](permissions.md)：`PreToolUse` 的 `allow`/`ask` 与权限引擎的交互、项目信任。
- [Skills 与扩展](skills-and-extensions.md)：技能、prompt 模板、主题与扩展/包管理。
