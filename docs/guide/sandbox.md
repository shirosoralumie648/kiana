# 沙箱与执行隔离

本页讲 kiana 如何把 shell 命令、MCP stdio 子进程和文件写入限制在一个受控环境里，以及如何选择策略、后端和排查失败。沙箱只回答「这条命令在什么环境里跑」，不回答「用户是否允许」；审批与规则属于 [权限与审批](permissions.md)。两层可以独立开启，推荐同时使用。

## 快速开始

```bash
# 工作区可写：当前目录可写，.git/.kiana 只读，禁止网络
KIANA_SANDBOX_POLICY=workspace-write kiana

# 只读：全盘可读，写入与网络受限
KIANA_SANDBOX_POLICY=read-only kiana

# 显式指定后端（需要同时设置策略，否则 shell 本来就不启用沙箱）
KIANA_SANDBOX_POLICY=workspace-write KIANA_SANDBOX=bwrap kiana
KIANA_SANDBOX_POLICY=workspace-write KIANA_SANDBOX=landlock kiana

# 关闭已配置的沙箱（逃生舱，不推荐）
KIANA_SANDBOX_POLICY=workspace-write KIANA_SANDBOX=none kiana
```

当前实现中，除下面两种例外，沙箱需要显式设置 `KIANA_SANDBOX_POLICY`：

- 权限模式 `read-only` 会为会话显式选择只读策略。
- MCP stdio 子进程会按权限模式安装进程级默认沙箱（见下文「MCP stdio」）。

没有设置 `KIANA_SANDBOX_POLICY`、也没有触发上面两种例外时，shell 直接在宿主机权限下运行。

## 常用操作

### 为 shell 启用工作区隔离

```bash
KIANA_SANDBOX_POLICY=workspace-write kiana
```

这条策略同时作用于模型发起的 `bash`、`powershell`、会话接口 `execute_bash` 以及 TUI 里 `!command` 的默认本地执行路径；它们共用同一个会话级 `SandboxManager`。如果扩展通过 `user_bash` 事件提供了自己的 `operations`，执行环境由扩展负责。

### 添加可写根

```bash
KIANA_SANDBOX_POLICY=workspace-write kiana --add-dir /data/shared
```

或在 settings 里写：

```json
{
  "permissions": {
    "mode": "accept-edits",
    "addDirs": ["/data/shared"]
  }
}
```

`--add-dir` 与 `permissions.addDirs` 只在 `workspace-write` 策略下扩展可写根；`read-only` 策略会忽略它们。它们同时扩展权限层 `accept-edits` 的「工作区内」范围。

### MCP stdio

MCP stdio 子进程的沙箱按下面的顺序解析：

1. 扩展显式传入的 `SandboxManager`。
2. 进程级默认沙箱；`kiana` 启动时按有效权限模式安装。
3. `SandboxManager::from_env`：`KIANA_SANDBOX_POLICY` 构造策略，`KIANA_SANDBOX` 选择后端或关闭。

按权限模式安装的默认策略：

| 权限模式 | MCP stdio 默认策略 |
| --- | --- |
| `read-only` | `read-only` |
| `default` | `workspace-write`（cwd 与 `--add-dir` 作为可写根） |
| `accept-edits` | `workspace-write`（cwd 与 `--add-dir` 作为可写根） |
| `bypass` | `danger-full-access` |

两个覆盖规则：

- 用户显式设置了非空的 `KIANA_SANDBOX_POLICY` 时，不再安装进程级默认，直接按该环境变量构造；此时它优先于权限模式。
- `KIANA_SANDBOX=none` 会关闭沙箱安装，stdio 子进程直接启动。

进程级默认只把 CLI `--add-dir` 并入可写根；settings 的 `permissions.addDirs` 只影响会话 shell 与文件工具，不参与这次 MCP 默认策略计算。该默认值在进程内只安装一次（`OnceLock`），同一进程的后续会话沿用第一次的 manager。

HTTP MCP 传输没有子进程，不经过这一层。后端不可用时 stdio 传输启动失败，不会静默回退到无沙箱启动。

### cron 受管脚本

cron 的受管脚本只按环境变量启动：`KIANA_SANDBOX_POLICY` 决定策略，`KIANA_SANDBOX` 决定后端。

- 设置了策略：脚本通过 `SandboxManager` 启动（bubblewrap / Landlock）；后端不可用时脚本不执行。
- 未设置策略：退化为宿主机 `sh`。

cron 是无人值守执行，没有交互式审批通道；它不读取权限模式，也不经过权限引擎的「询问」。脚本路径必须位于受管目录内，绝对路径、`..` 与符号链接逃逸会被拒绝。

### 只读会话

```bash
kiana --permission-mode read-only
```

权限模式 `read-only` 会为 bash、PowerShell、`execute_bash` 和文件写入门禁显式选择 `read-only` 沙箱策略，不依赖 `KIANA_SANDBOX_POLICY`。MCP stdio 是例外：显式的 `KIANA_SANDBOX_POLICY` 或 `KIANA_SANDBOX=none` 仍优先于权限模式。

即使 `KIANA_SANDBOX=none` 关闭了 OS 级后端，权限层仍会拒绝写与执行，文件写入门禁也继续使用只读策略，不会因为后端关闭而放行。

## 参考

### 策略

| 策略 | 读 | 写 | 网络 | 说明 |
| --- | --- | --- | --- | --- |
| `read-only` | 全盘可读 | bubblewrap：仅私有 `/tmp`（默认）；Landlock：无 | 禁止 | `bubblewrap` 的 `/tmp` 是 tmpfs，退出即丢弃 |
| `workspace-write` | 全盘可读 | 仅可写根；`.git` / `.kiana` 在 bubblewrap 下保持只读 | 禁止 | 可写根默认包含会话 cwd；可由 `--add-dir` 或 `permissions.addDirs` 扩展 |
| `danger-full-access` | 全盘 | 全盘 | 允许 | 不做任何隔离 |

策略名称的稳定字符串就是 `read-only` / `workspace-write` / `danger-full-access`。`.git` / `.kiana` 的只读保护是固定默认值，当前没有通过环境变量或 settings 自定义受保护子路径的入口。

### 环境变量

| 变量 | 取值 | 默认与边界 |
| --- | --- | --- |
| `KIANA_SANDBOX_POLICY` | `read-only` / `readonly`、`workspace-write` / `workspace`、`danger-full-access` / `danger` / `none` / `off` / `disabled` | 未设置或空串表示不启用沙箱；无法识别的值会 fail-closed，不会被当成未设置（shell 报 `BackendUnavailable`，文件工具报 `INVALID_CONFIG`） |
| `KIANA_SANDBOX` | `none` / `off` / `disabled` / `false`、`bwrap` / `bubblewrap`、`landlock`、`auto` 或其它值 | 未设置或其它值自动探测；`none` 是显式关闭，优先于策略 |
| `KIANA_BWRAP` | `bwrap` 可执行文件路径 | 优先级高于 PATH，但仍要经过用户命名空间探针 |

这三个变量由沙箱层直接读取，未出现在 `kiana --help` 的 Environment Variables 段。

`KIANA_SANDBOX_POLICY` 被显式设为 `danger-full-access`（或同义的 `danger` / `none` / `off` / `disabled`）时，会得到「不启用沙箱」而不是 fail-closed。`KIANA_SANDBOX=none` 在策略值非法或非 UTF-8 时同样是逃生舱。

### 后端选择

`SandboxManager::new` 按下面的顺序选择后端：

| 顺序 | 条件 | 结果 |
| --- | --- | --- |
| 1 | 策略是 `danger-full-access` | 无后端，直接执行 |
| 2 | `KIANA_SANDBOX` 为 `none` / `off` / `disabled` / `false` | 显式关闭，直接执行 |
| 3 | `KIANA_SANDBOX=bwrap` / `bubblewrap` | 探测成功则使用 bubblewrap；失败则 fail-closed |
| 4 | `KIANA_SANDBOX=landlock` | 内核支持且能兑现策略则使用 Landlock；失败则 fail-closed |
| 5 | 未设置或其它值（自动） | bubblewrap 探针 → Landlock → fail-closed |

自动选择有一个重要限制：Landlock 没有减法规则。只要策略里存在「可写根内保持只读」的子路径，自动选择就不会退回 Landlock，而是 fail-closed。`workspace-write` 默认总会列出 `.git` / `.kiana`，因此：

- `workspace-write` 在 bwrap 不可用时默认 fail-closed。
- 想在这类机器上接受较弱的隔离，必须显式设置 `KIANA_SANDBOX=landlock`。
- `read-only` 没有可写根与受保护子路径，可以自动回退到 Landlock。

bubblewrap 探针最多阻塞 500ms；成功结果在进程内永久缓存，失败结果 30 秒后重试。

### bubblewrap 后端

bubblewrap 通过挂载命名空间构造环境，参数顺序大致为：

- `--ro-bind / /`：先把整个文件系统只读绑定。
- `--dev /dev`、`--proc /proc`：提供最小设备与进程视图。
- `--tmpfs /tmp`：默认给命令私有临时目录；cwd 位于 `/tmp` 时会再把真实目录挂回。
- `--bind <root> <root>`：逐个打开可写根。
- `--ro-bind <subpath> <subpath>`：重新保护 `.git` / `.kiana`；目标不存在时用只读 tmpfs 占位。
- `--unshare-user`、`--unshare-pid`、`--unshare-ipc`；禁止网络时追加 `--unshare-net`。
- `--new-session`、`--die-with-parent`、`--cap-drop ALL`、`--chdir <cwd>`。

bubblewrap 仍然全盘可读；它只限制写与网络。宿主上的 Unix socket（例如容器运行时 socket）不会被文件只读限制阻止 `connect(2)`。

### Landlock 后端

Landlock 是 Linux 的 unprivileged LSM，只处理写类文件系统访问与部分 TCP 网络操作：

- 全盘可读，白名单可写；没有可写根之外的写权限。
- ruleset 在每次 `spawn` 时按当下策略重建，构造 manager 之后才创建或替换的可写根仍然可写。
- 写掩码包含设备节点创建位，但可写根不授予它们，因此可写根内也不能 `mknod`。
- 要求 ABI 3 及以上（ABI 3 才有 `TRUNCATE`）；策略禁止网络时要求 ABI 4 及以上（ABI 4 才有 TCP bind/connect 位）。不满足时拒绝启用。
- ABI 5 及以上处理 `IOCTL_DEV`，只对标准设备放行 ioctl。
- 不覆盖 UDP、ICMP、Unix socket；需要完整网络隔离时使用 bubblewrap。
- 没有减法规则，`.git` / `.kiana` 的只读保护只在 bubblewrap 下生效。
- `read-only` 策略下没有任何可写根，`/tmp` 也是只读的（Landlock 不能挂 tmpfs）。

### 权限模式与沙箱的映射

| 权限模式 | bash / powershell / `execute_bash` | `write` / `edit` / `apply_patch` 写入门禁 | MCP stdio | cron 脚本 |
| --- | --- | --- | --- | --- |
| `read-only` | 显式 `read-only` 策略 | 显式 `read-only`，所有写入拒绝 | `read-only` | 只按环境变量 |
| `default` | 由 `KIANA_SANDBOX_POLICY` 决定；未设置则不隔离 | 同左；未设置时门禁放行 | `workspace-write`（cwd + `--add-dir`） | 只按环境变量 |
| `accept-edits` | 由 `KIANA_SANDBOX_POLICY` 决定；未设置则不隔离 | 同左；`accept-edits` 本身不会启用沙箱 | `workspace-write`（cwd + `--add-dir`） | 只按环境变量 |
| `bypass` | `danger-full-access`，不创建 manager | 门禁放行 | `danger-full-access` | 只按环境变量 |

补充：

- 沙箱可以比权限层更严格。权限层批准了一次写入，不代表沙箱会允许：`workspace-write` 之外的路径仍会被 `OUTSIDE_WORKSPACE` 拒绝。
- 文件写入工具本身是进程内操作，没有独立的 OS 沙箱；它们与 shell 共用同一份 `ExecutionPolicy` 做前置路径门禁。
- 修改权限模式会重算会话执行策略并重建内建工具定义，新策略对后续调用生效。

### 统一执行策略与拒绝码

`ExecutionPolicy::resolve` 是 shell、MCP 子进程与文件写入的共同入口。工具名与输入形状必须匹配，否则 fail-closed：

| 工具名 | 输入形状 |
| --- | --- |
| `bash` / `powershell` / `execute_bash` | shell |
| `mcp` / `mcp_stdio` | MCP 子进程 |
| `write` / `edit` / `apply_patch` | 文件写入 |
| `read` | 文件读取 |

拒绝时返回稳定错误码：

| 错误码 | 含义 |
| --- | --- |
| `INVALID_CONFIG` | 沙箱配置非法（例如无法识别的 `KIANA_SANDBOX_POLICY`） |
| `UNKNOWN_TOOL` | 没有为该工具定义执行策略 |
| `TOOL_INPUT_MISMATCH` | 工具名与输入形状不匹配（调用方错误） |
| `NO_WRITE_PATHS` | 文件写入没有给出任何目标路径 |
| `PATH_UNRESOLVED` | 路径无法解析（符号链接环、权限等） |
| `READ_ONLY` | 只读策略下尝试写入 |
| `OUTSIDE_WORKSPACE` | 目标在所有可写根之外 |
| `PROTECTED_PATH` | 目标位于 `.git` / `.kiana` 内 |

`read` 不受写策略限制：三种策略下读取都允许，这与 bubblewrap/Landlock「全盘可读、只限制写」的语义一致。

文件写入的路径检查会先解析符号链接，包括悬空链接与嵌套链接；无法解析时拒绝，不做猜测。大小写不敏感的平台上按大小写不敏感比较。

### 执行状态

沙箱中的 shell 通过 relay 监督进程报告结果：

| 状态 | 含义 | 调用方应做什么 |
| --- | --- | --- |
| `ConfirmedExit { code, signal }` | 拿到确定退出状态 | 相信退出码；信号终止时按 `128 + signal` |
| `Unconfirmed { reason }` | 命令已启动，但 relay 被外部杀死，状态未知 | 命令可能已经执行，不要自动重试；先检查工作区 |
| `BackendUnavailable { reason }` | 后端不可用或启动失败，命令没有执行 | 修复后端后可以重试 |
| `Interrupted { reason }` | 命令确认启动前被中断，命令没有执行 | 可以重新发起 |

工具错误文本会带状态名，例如：

```text
BackendUnavailable: The command was not executed: the sandbox backend is unavailable (...). Do not fall back to running it unsandboxed.
```

超时或中止发生在 relay 已经启动命令之后时，错误文本会追加 `|unconfirmed`，例如 `timeout:30|unconfirmed`、`aborted|unconfirmed`。非沙箱路径仍然直接 spawn，信号按 `128 + signal` 折算成退出码。

### 文件写入门禁

`write` 与 `edit` 的检查在文件变更队列的锁内执行，顺序为：

1. `ExecutionPolicy::resolve` / `resolve_write_target`：解析真实路径，检查可写根与受保护子路径。
2. read-before-write：已存在的普通文件必须在本会话读过。
3. `edit` 额外检查截断读是否覆盖了要修改的行。

相关错误文本：

| 文本 | 含义 |
| --- | --- |
| `ReadBeforeEdit: ... has not been read in this session` | 没有先读就修改已存在的文件 |
| `ReadBeforeEdit: ... has only been partially read` | 只读了部分内容却整文件覆盖 |
| `FileChangedSinceRead (FILE_CHANGED_SINCE_READ): ...` | 读完之后文件被外部修改 |
| `READ_RANGE_NOT_SEEN: ...` | 截断读没有看到要修改的行 |
| `READ_NOT_TEXT: ...` | 读取的是图片等非文本内容，不能用于 edit/write |
| `PRIOR_READ_VERIFICATION_FAILED: ...` | 无法验证读记录（例如 inode 为 0），fail-closed |

新文件不需要先读。成功写入后会以写出的字节重建读基线。

## 诊断与排查

当前没有 `kiana sandbox` 子命令，也没有 `--sandbox` 之类的 CLI flag；沙箱只通过环境变量、权限模式和会话内部接线控制。排查时可以用下面的系统命令和工具错误文本。

### 检查环境变量

```bash
printenv KIANA_SANDBOX_POLICY KIANA_SANDBOX KIANA_BWRAP
```

空输出表示未设置。注意空字符串与未设置等价。

### 检查 bubblewrap

```bash
command -v bwrap
bwrap --version
```

`bwrap --version` 只能确认程序存在，不能证明用户命名空间可用。仓库使用的探针等价于：

```bash
bwrap --die-with-parent \
  --ro-bind / / --dev /dev --proc /proc \
  --unshare-user --unshare-pid --unshare-ipc \
  --cap-drop ALL -- /bin/sh -c true
```

退出码为 0 才表示探针通过。探针还需要 `/bin/sh` 或 `true` 可执行；找不到时会判为不可用。

### 检查 Landlock

```bash
uname -r
cat /sys/kernel/security/lsm 2>/dev/null
```

第二行在 securityfs 已挂载时会列出启用的 LSM；包含 `landlock` 表示内核启用了该模块。代码还会查询 Landlock ABI：低于 3 时拒绝启用；策略禁止网络时低于 4 也拒绝启用。

### 解读工具错误

| 现象 | 可能原因 |
| --- | --- |
| `INVALID_CONFIG: ... unrecognized KIANA_SANDBOX_POLICY value ...` | 策略拼写错误（文件工具路径）；改成 `read-only` / `workspace-write` / `danger-full-access`，或修复环境变量 |
| `BackendUnavailable: ... unrecognized KIANA_SANDBOX_POLICY value ...` | 策略拼写错误（shell 路径）；同样需要修正环境变量 |
| `BackendUnavailable: ... no working bubblewrap or Landlock backend ...` | 没有 bwrap，Landlock 也不可用；安装 bubblewrap，或显式 `KIANA_SANDBOX=landlock`，或 `KIANA_SANDBOX=none` |
| `BackendUnavailable: ... cannot keep protected subpaths ... read-only` | bwrap 不可用且策略是 `workspace-write`；安装 bwrap，或显式接受 Landlock 的弱隔离 |
| `OUTSIDE_WORKSPACE: ... is outside the writable roots` | 写入目标不在 cwd / `--add-dir` / `permissions.addDirs` 内 |
| `PROTECTED_PATH: ... .git/.kiana cannot be modified` | 目标位于可写根内的 `.git` / `.kiana` |
| `READ_ONLY: ... the sandbox policy is read-only` | 当前策略是只读，不能写文件 |
| `Unconfirmed: ... may have already executed ...` | relay 被外部杀死；不要自动重试，先检查工作区 |

## 限制与排查

- **默认不隔离**：`default` / `accept-edits` 权限模式下（以及未启用权限层的会话），未设置 `KIANA_SANDBOX_POLICY` 时 shell 直接在宿主机运行。需要隔离必须显式设置策略。
- **全盘可读**：`read-only` 与 `workspace-write` 都只限制写与网络，不限制读取与执行。需要限制读取时要扩展挂载策略，当前没有对应配置项。
- **Unix socket 不受保护**：文件只读不阻止 `connect(2)`；bubblewrap 不隐藏宿主的 `/var/run/docker.sock` 等 socket。
- **Landlock 较弱**：不保护 `.git` / `.kiana`，不覆盖 UDP、ICMP、Unix socket，且 `read-only` 下 `/tmp` 也是只读。自动回退只适用于没有受保护子路径的策略。
- **平台范围**：当前只验证 Linux x86_64。macOS 的 seatbelt 尚未实现，Windows 没有后端；需要沙箱时会 fail-closed，不会静默放行。
- **文件写入不是 OS 沙箱**：`write` / `edit` / `apply_patch` 使用进程内前置检查，检查与真实写入之间仍有 TOCTOU 窗口；只有 shell（含配置了策略的 cron 受管脚本）与 MCP 子进程走 OS 级隔离。
- **read-before-write 有成本**：写入门禁会把文件读入内存并计算哈希，大文件上的写入有额外 I/O 开销。
- **MCP 残余**：提前返回的 CLI 子命令（例如 `kiana mcp ...`、package、config）不经过默认沙箱安装点；部分 MCP 错误路径只返回文本，不带结构化状态；扩展 JS 的 `user_bash` 不解析 `executionStatus`。
- **没有 bubblewrap 时的 MCP 行为**：`default` / `accept-edits` 的 MCP 默认策略是 `workspace-write`，而 `workspace-write` 带 `.git` / `.kiana` 保护，bwrap 不可用时默认 fail-closed，stdio 服务器可能启动失败。需要显式设置 `KIANA_SANDBOX=landlock`（较弱隔离）或 `KIANA_SANDBOX=none`。
- **MCP 默认不看 local 层**：MCP 的进程级默认沙箱按启动期 `SettingsManager` 的全局/受信项目 `permissions` 与 CLI flag 计算，不读取 `settings.local.json` 里的 `permissions.mode`；项目未受信任时项目层也不参与。
- **cron 没有审批**：cron 只按环境变量（`KIANA_SANDBOX_POLICY` 与 `KIANA_SANDBOX`）隔离，不读取权限模式；未设置策略时脚本直接在宿主机运行。
- **bwrap 探针缓存**：成功结果永久缓存，失败结果 30 秒后重试；如果启动时探针失败，后续命令在一段时间内仍会报后端不可用。
- **没有独立的沙箱诊断命令**：只能结合环境变量、系统命令和工具错误文本排查。
- **`KIANA_SANDBOX=none` 是全局逃生舱**：它会让策略要求沙箱时也不隔离；只在确认风险时临时使用。
- **受保护子路径不可配置**：`.git` / `.kiana` 是固定默认值；当前没有 settings key 或环境变量可以增删。

## 相关页面

- [权限与审批](permissions.md)
- [设置（settings）](settings.md)
- [内置工具](tools.md)
- [MCP 服务器](mcp.md)
- [故障排查](troubleshooting.md)
