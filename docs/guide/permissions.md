# 权限与审批

本页讲 kiana 如何决定一次工具调用是放行、询问还是拒绝，以及你如何用模式、规则、审批和项目信任来控制它。权限层回答「这次调用允不允许」，[沙箱与执行隔离](sandbox.md) 回答「允许之后在什么环境里执行」；两者相互独立，可以只开其中一个，推荐同时开启。

> 版本说明：本文对应 workspace 版本 `0.1.0`。运行 `kiana --version` 会输出 `1.4.0`，该值来自 TypeScript 兼容常量，是有意保留的差异，不是构建错误。

## 快速开始

交互式会话中最常用的两种启动方式：

```bash
# 读放行；写、执行 shell、MCP 工具逐次询问
kiana --permission-mode default

# 只读：读放行，其余默认拒绝
kiana --permission-mode read-only
```

用规则放行或拒绝具体操作：

```bash
# 放行 git status，拒绝 rm；其余仍按 default 模式询问
kiana --permission-mode default \
  --allow "command:git status" \
  --deny "command:rm"
```

把同样的配置写进全局 settings（`~/.kiana/agent/settings.json`）：

```json
{
  "permissions": {
    "mode": "default",
    "allow": ["command:git status"],
    "deny": ["command:rm"],
    "addDirs": ["../shared"]
  }
}
```

项目级配置写在 `<项目>/.kiana/settings.json`，但只有项目受信任时才会加载；项目本地配置写在 `<项目>/.kiana/settings.local.json`。详见下文「配置与持久化」与「项目信任」。

## 权限层什么时候启用

当前实现中，权限层默认不启用。只有出现下面任一配置时才会创建权限引擎：

| 配置来源 | 说明 |
| --- | --- |
| `permissions` 对象中存在有效的 `mode`、`rules`、`allow`、`deny`、`ask`、`addDirs`、`workspaceRoots`、`additionalDirectories` 或 `disableBypassPermissionsMode` | 从 settings 启用；没有写 `mode` 时按 `default` |
| `--permission-mode <mode>` | 本次运行启用并覆盖 settings 的模式 |
| `--allow <rule>` / `--deny <rule>` / `--add-dir <dir>` | 本次运行启用；没有 `--permission-mode` 时模式为 `default` |
| `KIANA_PERMISSIONS` 或 `KIANA_PERMISSION_MODE` | settings 没有有效 `permissions` 配置时作为回退；值为 `off` 时关闭 |
| `permissions.mode: "off"` 或环境变量 `off` | 显式关闭权限检查 |

没有上述任何配置时，工具调用不会进入权限引擎；这不影响沙箱层，沙箱是否启用见 [沙箱与执行隔离](sandbox.md)。

## 权限模式

`--permission-mode` 与 settings 的 `permissions.mode` 接受四个规范值：

| 模式 | 读 | 写 | 执行 shell | MCP / 其它工具 |
| --- | --- | --- | --- | --- |
| `read-only` | 允许 | 拒绝 | 拒绝 | 拒绝；声明 `readOnlyHint: true` 的 MCP 工具可放行 |
| `default` | 允许 | 询问 | 询问 | 询问 |
| `accept-edits` | 允许 | 工作区内允许；区外与 `.git`/`.kiana` 询问 | 询问 | 询问 |
| `bypass` | 允许 | 允许 | 允许 | 允许 |

表中的「允许/询问/拒绝」是模式给出的默认结论，不是最终结论：

- 显式 `allow` 规则优先于模式。例如 `--permission-mode read-only --allow "command:git status"` 仍会放行 `git status`，其它命令继续按只读拒绝。
- 显式 `deny` 规则永远优先，包括在 `bypass` 模式下：`--permission-mode bypass --deny bash` 仍然拒绝 `bash`。
- 灾难性命令检查先于一切规则；只有 `bypass` 会跳过它。
- 写入 `.git` / `.kiana` 是安全确认：在 `default` / `accept-edits` 下即使有 `allow` 规则也会每次询问；`read-only` 默认拒绝（显式 `allow` 规则仍可覆盖）；`bypass` 跳过确认。
- `bypass` 可以被 `disableBypassPermissionsMode` 强制降级为 `default`（管理员层常用它做强制限制）。

`accept-edits` 的「工作区内」按会话工作目录与 `addDirs` 计算；符号链接会先解析真实路径再判断。`accept-edits` 只影响审批，不会自动启用 `workspace-write` 沙箱。

代码还接受一组兼容别名（如 `plan`、`workspace-write`、`danger-full-access`），但 `--help` 与本文以四个规范值为准。

## 工具如何分类

权限引擎会先给工具调用归类，再套用模式与规则：

| 工具名 | 动作类别 | 说明 |
| --- | --- | --- |
| `read`、`ls`、`grep`、`find` | 读 | 模式默认放行 |
| `edit`、`write` | 写 | `accept-edits` 可在工作区内自动放行 |
| `bash`、`powershell` | 执行 | 会做命令与写入路径分析 |
| `mcp__<server>__<tool>` | MCP | 按 MCP 规则与 annotations 处理 |
| 其它，包括扩展工具、Web 工具、`apply_patch` | 其它 | 默认询问；`read-only` 下拒绝 |

当前实现中，当 `apply_patch` 取代 `edit` 作为模型可见的编辑工具时，权限层仍把它归为「其它」，因此不享受 `accept-edits` 的工作区内自动放行，需要单独批准或写规则。

## 审批交互

在 TUI 中，权限层返回「询问」时会显示 per-tool 审批卡。多个并发请求共用一个对话框，卡片按 FIFO 排队，标题始终显示 `Approval 1/N`（N 为当前审批队列总数）。RPC 或其它没有覆盖审批卡的 UI 会退化为普通选择列表；两种路径的选项含义一致。

审批卡提供 `Allow once`、`Always allow ...` 与 `Reject`；没有可用持久作用域时只显示 `Allow once` 与 `Reject`。允许持久批准时，卡片按 `Always allow (this session)`、`Always allow (this project)`、`Always allow (this machine)` 的顺序列出三个作用域。

审批卡按键：

| 按键 | 行为 |
| --- | --- |
| `Up` / `Down`、`j` / `k` | 移动焦点（不环绕） |
| `Enter` | 确认当前选项；焦点在 `Reject` 时直接拒绝 |
| `1`-`9` | 直接选择对应编号并确认 |
| `Escape` | 拒绝，按取消处理 |
| `Ctrl+Enter` | 危险动作的显式确认键 |

危险动作的判定来自命令分析（例如 `rm`、`dd`、`mkfs`、`kill`、`sudo`、`git reset --hard`、`git clean -f`、`git push --force`、递归 `chmod`、`find -delete`，或分析结果不可信）。危险动作的初始焦点固定在 `Reject`，`Enter` 不会批准；必须先移动到 Allow 项，再按 `Ctrl+Enter`。

「总是允许」写到哪里：

| 作用域 | 选择列表标签 | 持久化位置 | 生效范围 |
| --- | --- | --- | --- |
| 本会话 | `Always allow (this session)` | 仅内存 | 当前会话，重启后失效 |
| 本项目 | `Always allow (this project)` | `<cwd>/.kiana/settings.json` 的 `permissions.rules` | 该项目，要求项目受信任 |
| 本机（项目本地） | `Always allow (this machine)` | `<cwd>/.kiana/settings.local.json` 的 `permissions.rules` | 该项目，要求项目受信任 |

中文界面使用同一模板 `总是允许（{{scope}}）`，其中 scope 值保持英文，因此显示为「总是允许（this session）」「总是允许（this project）」「总是允许（this machine）」。

「总是允许」只生成 `Allow` 规则，不能覆盖 `Deny`、灾难性命令检查和 `.git`/`.kiana` 安全确认。规则粒度大致如下：

| 调用 | 记住的规则 |
| --- | --- |
| 简单命令且第二个词是已知子命令（如 `git status`） | `program + subcommand` 前缀 |
| 其它简单命令（如带选项的 `git -C /tmp status`） | 完整精确命令 |
| 复合命令或分析不完整 | 完整精确命令 |
| `write` / `edit` | 每个写入路径一条路径前缀规则，且只对当初批准的动作生效 |
| MCP / 未知工具 | 按工具名放行 |

以下情况不提供「总是允许」，只能逐次批准：

- 写入 `.git` / `.kiana`（安全确认每次重新询问）。
- 声明 `destructiveHint: true` 或 `openWorldHint: true` 的 MCP 工具。

没有交互式 UI（`--print`、`--mode json`、`--mode rpc` 且无审批通道）时，「询问」按拒绝处理，不会静默放行。当前实现没有专门的 `--yes` 自动批准开关。

## 规则参考

### 命令行规则语法

`--allow` 与 `--deny` 接受同一种规则文本：

| 写法 | 示例 | 匹配对象 |
| --- | --- | --- |
| 裸工具名或以 `*` 结尾/开头的模式 | `bash`、`mcp__docs__*`、`*__read` | 工具名 |
| `tool:<pattern>` / `tool=<pattern>` | `tool:mcp__*` | 工具名 |
| `command:<words>` / `command=<words>` | `command:git status` | 命令前缀（只对简单命令生效） |
| `command:<words>*` | `command:git status*` | 显式通配的命令 |
| `path:<prefix>` / `path=<prefix>` | `path:/tmp/shared` | 路径前缀 |
| 裸文本含空白 | `git status` | 命令前缀 |
| 裸文本像路径 | `/tmp/shared`、`~/.ssh` | 路径前缀 |

补充规则：

- 工具名模式只支持开头或结尾的 `*`，不支持中间通配。
- 路径前缀支持 `~`、`$HOME` 与相对路径；相对路径按第一个工作区根（通常是会话 cwd）解析。
- `--allow` / `--deny` 只影响当前进程，不会写回 settings。
- `--allow` / `--deny` 的规则值不能以 `-` 开头（会被当成下一个选项）；遇到这种规则时改用 settings。

### settings 规则对象

`permissions.rules` 是对象数组：

```json
{
  "permissions": {
    "mode": "default",
    "rules": [
      { "tool": "read", "decision": "allow" },
      { "command": ["git", "status"], "decision": "allow" },
      { "command": "echo a; rm -rf ./build", "exact": true, "decision": "ask" },
      { "path": "/workspace", "decision": "allow", "actions": ["read"] },
      { "pattern": "mcp__docs__*", "decision": "allow", "reason": "docs server is read-only" }
    ]
  }
}
```

字段：

| 字段 | 类型 | 说明 |
| --- | --- | --- |
| `tool` / `pattern` | string | 工具名模式，支持开头/结尾 `*` |
| `command` | string 或 string[] | 默认是命令前缀；数组逐项作为前缀词 |
| `exact` | bool | 配合 `command`，按完整命令精确匹配 |
| `wildcard` | bool | 配合 `command`，把 `*` 当作通配符 |
| `path` | string | 路径前缀；支持 `~`、`$HOME`、相对路径 |
| `actions` | string[] | 只让路径规则作用于指定动作：`read`、`write`、`execute`、`network`、`mcp`、`other` |
| `decision` | `allow` / `ask` / `deny` | 必填；其它值会让整条规则被忽略 |
| `reason` | string | 展示在审批卡或拒绝信息中的原因 |

也可以用字符串简写，写法与命令行规则相同：

```json
{
  "permissions": {
    "allow": ["tool:read", "command:git status"],
    "deny": ["command:rm", "path:~/.ssh"],
    "ask": ["command:git push"],
    "addDirs": ["../shared"]
  }
}
```

`permissions.addDirs`、`permissions.workspaceRoots`、`permissions.additionalDirectories` 都表示额外可写工作区根；`--add-dir` 是它们的命令行等价物。相对路径按会话 cwd 解析。

### 匹配顺序

权限引擎按下面的顺序给出结论，先命中先返回：

| 顺序 | 检查 | 结论 |
| --- | --- | --- |
| 1 | 灾难性命令（`rm -rf /`、`mkfs`、`dd of=/dev/...`、fork bomb、把远程脚本通过管道交给 shell 等） | 拒绝；`bypass` 跳过 |
| 2 | `deny` 规则 | 拒绝 |
| 3 | shell 等价操作的 `deny` 规则（`cat`、`ls`、`curl`、重定向等） | 拒绝 |
| 4 | 写入受保护目录 `.git` / `.kiana` | 询问；`read-only` 与 `bypass` 不走此分支 |
| 5 | 「总是允许」生成的 `approved` 规则 | 允许 |
| 6 | 本会话的批准记录 | 允许 |
| 7 | `ask` 规则 | 询问 |
| 8 | shell 等价操作的 `ask` 规则 | 询问 |
| 9 | 命令分析不可信（未知语法、嵌套超过 50 段等） | 询问；`read-only` 与 `bypass` 不走此分支 |
| 10 | `allow` 规则 | 允许 |
| 11 | 模式默认 | 按模式表得出允许/询问/拒绝 |

规则匹配的几个安全边界：

- `allow` 型命令规则只对单一简单命令生效。含 `;`、`|`、`&`、`>`、`<`、反引号、`$(`、换行或未闭合引号的命令不会被前缀放行。
- `ask` / `deny` 型命令规则会扫描复合命令的每一段，包括 `bash -c` 内层与命令替换，因此 `echo hi; rm -rf ~` 也会命中 `command:rm`。
- `allow` 型路径规则要求命令的所有写入路径都在前缀内，且分析完整；`ask` / `deny` 型路径规则会同时检查读取路径。
- `allow` 型命令规则不会因为相对路径或不可信目录里的同名程序而命中，例如 `./git status`、`sub/git status`、`/tmp/evil/git status` 都不匹配 `git status` 规则。
- 被 `sudo`、`env`、`timeout` 等包装的提权变体不会命中 `allow` 型前缀规则，但会命中 `ask` / `deny` 型规则。

## 配置与持久化

### 文件位置

| 作用域 | 路径 | 是否受项目信任限制 |
| --- | --- | --- |
| 用户（全局） | `<agentDir>/settings.json`，默认 `~/.kiana/agent/settings.json` | 否 |
| 项目 | `<cwd>/.kiana/settings.json` | 是 |
| 项目本地 | `<cwd>/.kiana/settings.local.json` | 是 |
| 系统 / 管理员 | `/etc/kiana/settings.json`、`/etc/kiana/managed-settings.json`、`/etc/kiana/enterprise-settings.json` | 否；作为策略层 |

`<agentDir>` 可用 `KIANA_CODING_AGENT_DIR` 覆盖。Windows 上系统策略目录是 `%ProgramData%\kiana`。

`settings.local.json` 会作为 `local` 配置层参与 `permissions` 与 hooks 等分层合并，但类型化的 `Settings` 键（例如主题、模型默认值）不从该文件读取；这些键请写进 `settings.json`。项目未受信任时，项目与本地两层都会被跳过。

### 分层与合并

配置层从低到高为：`Builtin` → `System` → `Managed` → `Enterprise` → `User` → `Project` → `Local` → `CliFlag` → `Session`。

- `permissions` 下的 `rules`、`allow`、`deny`、`ask`、`addDirs`、`workspaceRoots`、`additionalDirectories` 跨层取并集并去重。
- `mode` 由更高层覆盖：`local` 覆盖 `project`，`project` 覆盖 `user`，命令行覆盖所有文件层。
- 显式 `null` 不覆盖低层配置（例如 `{"mode": null}` 不会关掉项目里的 `read-only`）。
- 格式错误或 `decision` 非法的单条规则会被忽略，不会让整个配置报错。

### 管理员策略

系统/管理员层（`/etc/kiana/...`）可以强制以下键；它们也可以写在 `permissions` 内，camelCase 与 snake_case 两种拼写都接受：

| 键 | 作用 |
| --- | --- |
| `disableBypassPermissionsMode` | 禁止进入 `bypass`；settings、`--permission-mode` 与运行期切换都会被降级为 `default` |
| `allowManagedPermissionRulesOnly` | `allow` / `ask` 规则只采信管理员层；其它层的 `deny` 仍然合并 |
| `allowedMcpServers` / `deniedMcpServers` | 当前只参与配置解析与合并，运行期未发现 MCP 服务器过滤调用（见「限制与排查」） |

### settings、环境变量与命令行的优先级

1. 命令行 `--permission-mode` / `--allow` / `--deny` / `--add-dir`。
2. settings 中的有效 `permissions` 配置。
3. 环境变量 `KIANA_PERMISSIONS` / `KIANA_PERMISSION_MODE`。

环境变量只在 settings 没有有效 `permissions` 配置时作为回退：

| 变量 | 取值 | 说明 |
| --- | --- | --- |
| `KIANA_PERMISSIONS` | `read-only` / `default` / `accept-edits` / `bypass` / `off` | 主变量；空值按未设置处理 |
| `KIANA_PERMISSION_MODE` | 同上 | 别名；两个都设置时按顺序先命中的非空值优先 |

未知值按 `default` 处理；`off`、`disabled`、`none`、`false`、`0` 表示关闭权限检查。这两个变量未出现在 `kiana --help` 的 Environment Variables 段，但由权限层直接读取。

settings 的 `permissions.mode` 与 `permissions.defaultMode` 同时存在时，`mode` 优先。未知的模式值同样按 `default` 处理。

注意：如果配置把权限层显式设为 `off`，单独给 `--allow` / `--deny` / `--add-dir` 不会重新创建权限引擎；必须同时给 `--permission-mode`。

## 项目信任

项目信任决定是否加载项目本地资源。需要触发信任判断的项目资源是 `<cwd>/.kiana` 下的 `settings.json`、`mcp.json`、`extensions`、`skills`、`prompts`、`themes`、`SYSTEM.md`、`APPEND_SYSTEM.md`，以及 cwd 或祖先目录中的 `.agents/skills`（`$HOME/.agents/skills` 除外）。`settings.local.json` 本身不在触发列表中，但项目一旦需要信任，它也会随项目层一起被加载或跳过。

| 入口 | 行为 |
| --- | --- |
| `--approve` / `-a` | 本次运行信任项目本地文件；不写 `trust.json` |
| `--no-approve` / `-na` | 本次运行忽略项目本地文件 |
| `/trust` | 交互式保存项目信任决定，写入 `<agentDir>/trust.json` |
| `defaultProjectTrust` setting | `ask`（默认）/ `always` / `never`，在没有其它决定时生效 |

`/trust` 的选择器提供 `Trust`、`Trust parent folder (<parent>)` 与 `Do not trust`。保存后不会立刻改变当前会话，界面会提示重启 kiana 后生效。

没有 UI 且需要询问时按不信任处理（`false`）。项目里没有需要信任的资源时，项目会被视为受信任，不会弹选择器。

项目未受信任时：

- 项目与项目本地的 `permissions` 配置不加载，仓库不能通过 `settings.json` 给自己授予 `bypass` 或放行规则。
- 选择 `Always allow (this project)` 或 `Always allow (this machine)` 会失败，错误信息为 `Project is not trusted; refusing to write project settings`；当前调用按拒绝处理，但你仍可选择 `Always allow (this session)`。

## 常见规则示例

| 目标 | 配置 |
| --- | --- |
| 完全只读，但允许 `git status` | `--permission-mode read-only --allow "command:git status"` |
| 默认询问，但完全禁止 `rm` | `--permission-mode default --deny "command:rm"` |
| 允许读，禁止读取 `~/.ssh` | `{"permissions": {"mode": "default", "deny": ["path:~/.ssh"]}}` |
| 接受工作区内编辑，并额外允许 `../shared` | `{"permissions": {"mode": "accept-edits", "addDirs": ["../shared"]}}` |
| 只允许一个 MCP server 免询问 | `{"permissions": {"mode": "default", "allow": ["tool:mcp__docs__*"]}}` |
| 放行一条固定的复合命令 | `{"permissions": {"mode": "default", "rules": [{"command": "echo a; rm -rf ./build", "exact": true, "decision": "allow"}]}}` |
| 禁止 MCP 工具，但保留内建工具 | `--deny "tool:mcp__*"` |

## 限制与排查

- **默认不启用**：没有 `permissions`、权限 flag 或环境变量时，不会弹出审批，也不会拒绝任何工具调用。需要审批请显式设置模式。
- **`read-only` 不是不可覆盖的硬拒绝**：显式 `allow` 规则（包括之前「总是允许」写下的 `approved` 规则）仍可放行匹配的调用。要强制阻止某类操作，请写 `deny` 规则。
- **`bypass` 不是无条件放行**：显式 `deny` 规则仍然生效；`disableBypassPermissionsMode` 为真时会被降级为 `default`。
- **非交互模式没有自动批准**：`--print`、`--mode json`、`--mode rpc` 没有审批通道时，「询问」一律转为拒绝。当前没有专门的 `--yes`。
- **「总是允许」有边界**：`.git` / `.kiana` 写入、`destructiveHint` / `openWorldHint` 的 MCP 工具只能逐次批准；`Project` / `Local` 作用域要求项目受信任。
- **选择 `Always allow (this project)` / `Always allow (this machine)` 会清空之前的本会话批准记录**，之前记住的操作可能重新询问；方向偏严，不会放宽权限。
- **规则粒度可能偏粗**：MCP 与未知工具按工具名放行；`git -C /path status` 这类命令会退化为完整精确命令，而不是宽泛前缀。
- **手工编辑不会热加载**：`settings.local.json` 的修改需要新会话或重启；运行期通过审批写入的规则会立即加入当前引擎。
- **损坏的本地文件会被跳过**：`settings.local.json` 解析失败时该层不生效，其中的 `deny` 规则可能暂时失效；写回路径会报错且不会覆盖坏文件。
- **并发审批不去重**：同时发起的两个相同调用会各自询问一次。
- **CLI 规则不持久化**：`--allow` / `--deny` / `--add-dir` 只影响当前进程。
- **`/permissions` 面板**：TUI 中输入 `/permissions` 打开权限面板，可查看/新增/删除规则、切换模式、管理工作区目录；`--print` 文本模式会直接输出权限文本。启动参数与 settings 仍然优先；工作区目录删除当前版本尚未接线，会提示不支持。可以用 `/trust` 管理项目信任。
- **`apply_patch` 的权限分类**：当前按「其它」处理，`accept-edits` 不会像 `edit` 那样自动放行工作区内修改。
- **MCP 服务器开关尚未接线**：`permissions.enabledMcpjsonServers`、`disabledMcpjsonServers`、`allowedMcpServers`、`deniedMcpServers` 目前只参与配置解析与合并，未发现运行期的 MCP 服务器过滤调用；不要依赖它们做访问控制。

排查建议：

1. 用 `kiana --help` 确认启动参数拼写；权限相关 flag 是 `--permission-mode`、`--allow`、`--deny`、`--add-dir`、`--approve`、`--no-approve`。
2. 检查 `settings.json` 里是否有合法的 `permissions` 对象；JSON 错误会让该层被跳过，单条规则的 `decision` 拼错只会让该条规则被忽略。
3. 确认项目是否受信任：未受信任时项目与本地配置完全不加载。
4. 被拒绝时先区分来源：拒绝信息带 `Permission denied by rule...` 表示规则命中；带 `Permission denied by the user...` 表示审批被拒绝；带 `Project is not trusted...` 表示写回作用域失败。
5. 权限放行后仍失败时，检查 [沙箱与执行隔离](sandbox.md) 的策略与错误码；两层是独立检查。

## 相关页面

- [沙箱与执行隔离](sandbox.md)
- [设置（settings）](settings.md)
- [CLI 参考](cli.md)
- [内置工具](tools.md)
- [MCP 服务器](mcp.md)
- [交互模式（TUI）](interactive-mode.md)
- [故障排查](troubleshooting.md)
