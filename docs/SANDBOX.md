# kiana-sandbox

`kiana-sandbox` 把 `bash` 命令包进一个受控的执行环境。它只回答「这条命令在什么沙箱里跑」，
不回答「用户是否允许」；审批与 per-tool 规则属于后续的 `kiana-permissions`。

## 策略

| 策略 | 读 | 写 | 网络 |
|---|---|---|---|
| `SandboxPolicy::read_only()` | 全盘可读 | bubblewrap：仅私有 `/tmp`；Landlock：无 | 禁止 |
| `SandboxPolicy::workspace_write(root)` | 全盘可读 | 仅 `root`；`.git` / `.kiana` 仅 bubblewrap 强制只读 | 禁止 |
| `SandboxPolicy::danger_full_access()` | 全盘 | 全盘 | 允许 |

`read_only_with_network()` 与 `.with_network_access(true)` 可以打开网络。
`workspace_write_with_roots(vec![...])` 支持多个可写根。Landlock 后端没有减法规则，
`.git` / `.kiana` 的只读保护只在 bubblewrap 生效；可用
`SandboxManager::enforces_read_only_subpaths()` 检查。

## 后端

选择顺序：

1. `KIANA_SANDBOX` 显式指定：`bwrap` / `landlock` / `none`。
2. bubblewrap：PATH（或 `KIANA_BWRAP`）里存在 `bwrap`，且用户命名空间探针通过。
3. Landlock：内核支持且策略要求的网络限制可被满足。
4. 都没有：**fail-closed**，`spawn` 返回 `SandboxError::Unavailable`。

Landlock 没有减法规则：自动选择时，只要策略里列出了需要保持只读的子路径
（`workspace-write` 默认总会列出 `.git` / `.kiana`，与它们当前是否已创建无关），
就不会退回 Landlock，而是 fail-closed；需要在这类宿主上接受较弱的隔离时，必须显式
设置 `KIANA_SANDBOX=landlock`。

`KIANA_SANDBOX=none` 是显式放行；策略仍需沙箱但后端不可用时不会自动放行。
`KIANA_SANDBOX_POLICY` 的无法识别值同样 fail-closed（第一条命令报错），不会被当成
「不启用沙箱」。

### bubblewrap

- `--ro-bind / /` 让整个文件系统只读；
- `--bind <root> <root>` 逐层打开可写根；
- `--ro-bind <subpath> <subpath>` 重新保护 `.git` / `.kiana`；
- `--tmpfs /tmp` 提供命令私有临时目录；
- `--unshare-user --unshare-pid --unshare-ipc`，禁止网络时加 `--unshare-net`；
- `--new-session` 让命令脱离调用方终端（防止终端转义/TIOCSTI 注入）；
- `--cap-drop ALL`。

### Landlock

- 只处理写类文件系统访问：全盘可读，白名单可写；
- ruleset 在**每次 `spawn`** 时按当时的策略重建，构造 manager 之后才创建/替换的
  可写根仍然可写；
- 写掩码包含 `MAKE_CHAR` / `MAKE_BLOCK`，但可写根的 path rule 不授予它们，
  因此可写根内也不能 `mknod` 设备节点；
- ABI 5+ 还处理 `IOCTL_DEV`：只有标准设备（`/dev/null` 等）显式放行 ioctl；
- `read_only` 策略下没有可写根，`/tmp` 也是只读的（Landlock 不能挂 tmpfs）；
- ABI 4+ 处理 `BIND_TCP` / `CONNECT_TCP`，策略禁止网络时不添加网络规则；
- ABI < 4 且策略禁止网络时拒绝启用（fail-closed）；
- 不覆盖 UDP、ICMP、Unix socket；需要完整网络隔离时用 bubblewrap。

### macOS / Windows

macOS seatbelt 尚未实现；需要沙箱时返回 `Unsupported` / `Unavailable`，不会静默放行。
Windows 目前没有后端，行为同上。

## 使用

零配置（只对默认 bash 工具生效）：

```bash
KIANA_SANDBOX_POLICY=workspace-write kiana ...
```

程序化：

```rust
use std::sync::Arc;
use kiana_sandbox::{SandboxManager, SandboxPolicy};
use kiana_tools::bash::{BashToolOptions, create_bash_tool_definition};

let cwd = std::env::current_dir()?;
let sandbox = Arc::new(SandboxManager::new(
    SandboxPolicy::workspace_write(&cwd),
    cwd.clone(),
));
let definition = create_bash_tool_definition(
    cwd.to_string_lossy().into_owned(),
    Some(BashToolOptions {
        sandbox: Some(sandbox),
        ..Default::default()
    }),
);
```

## 已知边界

- `read_only` / `workspace_write` 仍然是**全盘可读**；bubblewrap 只限制写与网络，
  不限制读取与执行。
  如果要把读取也限制在工作区，需要后续扩展挂载策略（`--tmpfs /` + 按需 `--ro-bind`）。
- bubblewrap 不隐藏宿主 Unix socket（例如 `/var/run/docker.sock`）：文件本身只读，但
  `connect(2)` 不受文件只读限制。后续可在策略里列出要 mask 的 socket 目录。
- `.git` / `.kiana` 的保护在 bubblewrap 每次 `spawn` 构造参数时检查存在性；Landlock
  不支持减法规则，不会施加这层保护。
- **Landlock 不支持减法规则**：它只能「允许写某些根」，不能在允许写整个根之后再把
  `.git` 挖成只读。因此 `.git` / `.kiana` 保护只在 bubblewrap 后端生效；Landlock
  回退只保证可写根之外的写被拒绝。可用 `SandboxManager::enforces_read_only_subpaths()`
  检查当前后端是否支持。
- 该 crate 不处理审批、命令前缀匹配、网络代理或 per-tool 规则。

## 测试

```bash
cargo test -p kiana-sandbox
cargo test -p kiana-tools --test bash_sandbox
```

集成测试会真实启动 `bwrap` / Landlock；后端不可用时打印原因并跳过。

## 参考

bubblewrap 的挂载顺序与「先只读绑定 `/`，再逐层打开可写根」的模型来自
`reference_project/codex/codex-rs/sandboxing` 与 `linux-sandbox`（Apache-2.0）。
Kiana 侧重新实现了策略模型与命令构造，没有直接复制 Codex 的 permission profile 类型。
