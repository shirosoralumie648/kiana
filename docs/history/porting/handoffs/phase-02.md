# Week 2 handoff — 接线清单

本文件列出第 2 周**不能**直接修改的共享文件需要的接线。所有 API 已在
`kiana-sandbox` / `kiana-tools` 中实现并有测试；这里只描述调用点、顺序与注意事项。

---

## 1. `kiana-session`：会话级读记录 + 统一执行策略

文件：`crates/kiana-session/src/agent_session.rs`

### 1.1 工具定义构造（约 7199 行 `create_all_tool_definitions`）

现在 `ReadToolOptions` / `WriteToolOptions` / `EditToolOptions` 都有
`read_state: Option<Arc<ReadStateRegistry>>`，`Write/EditToolOptions` 还有
`execution_policy: Option<Arc<ExecutionPolicy>>`。缺省分别是进程级共享读表和
`KIANA_SANDBOX_POLICY`；单进程多会话建议改成会话级实例。

建议在 `Core` 保存：

```rust
use kiana_tools::read::ReadStateRegistry;
use kiana_sandbox::{ExecutionPolicy, SandboxManager, SandboxPolicy};

// Core::new / AgentSession::build_runtime
let read_state = Arc::new(ReadStateRegistry::new());

// 权限模式 → 执行策略（示例；最终以权限层语义为准）
let execution_policy = Arc::new(match permission_mode {
    PermissionMode::ReadOnly => ExecutionPolicy::new(SandboxPolicy::read_only(), cwd.clone()),
    PermissionMode::Bypass => ExecutionPolicy::new(SandboxPolicy::danger_full_access(), cwd.clone()),
    _ => {
        let mut policy = ExecutionPolicy::from_env(&cwd);
        policy = policy.with_additional_writable_roots(
            permission_engine.add_dirs().to_vec(), // 需要权限层暴露这个列表
        );
        policy
    }
});
```

然后：

```rust
create_all_tool_definitions(
    self.core.cwd.clone(),
    Some(ToolsOptions {
        read: Some(ReadToolOptions {
            auto_resize_images,
            read_state: Some(Arc::clone(&read_state)),
            ..Default::default()
        }),
        write: Some(WriteToolOptions {
            read_state: Some(Arc::clone(&read_state)),
            execution_policy: Some(Arc::clone(&execution_policy)),
            ..Default::default()
        }),
        edit: Some(EditToolOptions {
            read_state: Some(Arc::clone(&read_state)),
            execution_policy: Some(Arc::clone(&execution_policy)),
            ..Default::default()
        }),
        bash: Some(BashToolOptions {
            command_prefix: shell_command_prefix,
            shell_path,
            sandbox: Some(Arc::new(SandboxManager::new(
                execution_policy.policy().clone(),
                &self.core.cwd,
            ))),
            ..Default::default()
        }),
        ..Default::default()
    }),
)
```

注意事项：

- `ToolsOptions` 的字段在 `crates/kiana-tools/src/index.rs`，键序无关。
- `ReadToolOptions` 目前只有 `read` 字段被 session 构造；`write`/`edit` 的选项字段已
  存在，直接填即可。
- 同一份 `ExecutionPolicy` 必须同时给 write/edit 与 `SandboxManager`，否则会出现
  「shell 写不进、文件工具写得进」（或反过来）。
- `ExecutionPolicy::from_env` 在 `KIANA_SANDBOX_POLICY` 未设置时返回
  `DangerFullAccess`。若产品要求「权限模式 read-only 必须禁止所有写入」，不要只依赖
  环境变量，按上面的 match 显式构造 `SandboxPolicy::read_only()`。
- 模式/`addDirs` 运行期变化时，重建 `Arc<ExecutionPolicy>` 并重建工具定义（现有
  `base_tool_definitions` 重建路径即可）。

### 1.2 `execute_bash`（约 8617 行）

现状：`execute_bash` → `create_local_bash_operations_from_env` →
`LocalShellOperations::exec`。后者已经：

1. 从 `SandboxManager` 取 `ExecutionPolicy` 并调用 `resolve("bash", Shell{command})`；
2. 沙箱路径使用 `spawn_relayed`，把 `BackendUnavailable` / `Unconfirmed` /
   `Interrupted` 映射成工具错误文本（前缀分别是 `BackendUnavailable:` /
   `Unconfirmed:` / `Interrupted:`）。

因此 `execute_bash` **不需要**额外接线也能获得统一策略与 relay 状态；错误文本经
`AgentSessionError::other(error.to_string())` 原样保留。若要：

- 让策略层看到字面工具名 `execute_bash`：`ExecutionPolicy::resolve` 已接受该名字；
  需要在 `kiana-tools` 增加一个「工具名参数」（当前由 `shell_name` 推导，`bash` 路径
  固定为 `bash`）；
- 把 `ExecutionStatus` 结构化放进 `BashResult`：`BashResult` 在 `kiana-ext-api`，
  该 crate 不在第 2 周所有权内；需要时新增字段并让
  `LocalShellOperations::exec` 返回结构化结果（当前只有 `BashExecResult{exit_code}`）。

注意：调用方显式传入 `ExecuteBashOptions.operations` 时仍会绕过策略（既有契约），
文档里已声明由调用方负责隔离。

### 1.3 压缩后失效读记录

文件：`crates/kiana-session/src/agent_session.rs`（`compact` / 自动压缩提交点）与
`crates/kiana-compaction`（当前没有删除 read 工具结果的实现）。

计划要求「压缩把 read 结果删掉时同步失效」。compaction 提交后（compacted messages
替换 live history 之后）调用：

```rust
// 方案 A（最保守）：整表清空
self.core.read_state.clear();

// 方案 B（保留未受影响的文件）：遍历被移除消息里的 read 工具调用，
// 对每个参数 path 失效
for path in removed_read_paths {
    self.core.read_state.invalidate(path);
}
```

`ReadStateRegistry::clear()` / `invalidate(path)` 都是公开 API。方案 B 需要
`kiana-compaction` 提供被移除消息的路径列表；在它没有该信息前，方案 A 是安全的
（只影响体验：压缩后第一次 edit/write 需要重新 read）。

注意：`invalidate` 按 realpath 匹配记录；传入 read 工具当时的 `path` 参数即可
（内部会 `resolve_real_path`）。

---

## 2. `kiana-mcp`：stdio 子进程接入沙箱

文件：`crates/kiana-mcp/src/transports/stdio.rs`、`crates/kiana-mcp/Cargo.toml`

现状：`spawn_child`（约 485 行）直接 `build_command(options).spawn()`，完全没有沙箱。

接线：

1. `Cargo.toml` 增加 `kiana-sandbox = { workspace = true }`（根 `Cargo.toml` 已有 workspace
   依赖，不需要改根文件）。
2. `StdioTransportOptions`（约 314 行）增加
   `pub sandbox: Option<Arc<kiana_sandbox::SandboxManager>>`。注意
   `crates/kiana-builtin-extensions/src/mcp/runtime.rs:671` 有结构体字面量，需要补
   `..Default::default()` 或显式字段（该 crate 不在第 2 周所有权内，请 wire agent
   一并改）。
3. 在 `spawn_child` 中：

```rust
use kiana_sandbox::{ExecutionPolicy, SandboxManager, SandboxDecision, SpawnOptions, StdioMode, ToolInput};

fn spawn_child(options: &StdioTransportOptions) -> Result<Child, (McpTransportError, bool)> {
    if let Some(sandbox) = &options.sandbox {
        let policy = sandbox.execution_policy();
        let args = options.args.clone().unwrap_or_default();
        match policy.resolve(
            "mcp",
            &ToolInput::McpProcess { program: &options.command, args: &args },
        ) {
            SandboxDecision::Deny(denial) => {
                return Err((McpTransportError::new(denial.message()), false));
            }
            SandboxDecision::Allow(_) => {}
        }
        let spawn_options = SpawnOptions {
            cwd: options.cwd.clone(),
            stdin: StdioMode::Pipe,
            stdout: StdioMode::Pipe,
            stderr: StdioMode::Pipe, // Inherit 时传 StdioMode::Inherit
            detached: cfg!(unix),   // 与现有 USE_PROCESS_GROUPS 一致
            windows_hide: false,
            env: Some(computed_env(options).into_iter().collect()),
        };
        return sandbox
            .spawn(&options.command, &args, &spawn_options)
            .map_err(|error| (McpTransportError::new(error.to_string()), false));
    }
    // 原路径：build_command(options).spawn()
}
```

注意事项：

- MCP 是长生命周期服务器，建议用 `SandboxManager::spawn`（不套 relay）；relay 是为
  一次性命令的 `Unconfirmed` 语义设计的，transport 自己负责关闭阶梯与进程组。
- `SandboxManager::spawn` 的 `detached` 会 `setsid()`，与现有
  `configure_command` 的进程组逻辑等价；但仍需保留 `kill_on_drop(false)` 语义（tokio
  `Child` 默认 `kill_on_drop(false)`，无需额外设置）。
- `SandboxError::Unavailable` 必须当作 transport 启动失败上报，不能回退到无沙箱
  `spawn`。
- 没有 `sandbox`（`None`）时保持现状；会话层负责把 manager 传进来。
- `env`: 现有 `computed_env` 返回 `IndexMap<String,String>`；转成
  `Vec<(OsString, OsString)>` 传给 `SpawnOptions::env`（`Some` 会整体替换环境，与
  `configure_command` 的 `env_clear + envs` 等价）。

---

## 3. `kiana-tools/src/index.rs` / `definition.rs`

本周**不需要**修改：

- 新选项字段都有 `Default`，现有 `ToolsOptions` 调用点用 `..Default` 即可编译。
- 工具名、描述、schema、输出黄金文本未变。
- 如果后续要在 `edit`/`write` 的 prompt guidelines 里加「先 read」提示，会改动
  `tests/tool_definitions_golden.rs` 的黄金文本，属于产品文案决策，单独做。

---

## 4. 验收建议（wire agent 跑 workspace 时）

```bash
CARGO_TARGET_DIR=/data/cargo-target-sandbox cargo check --workspace
CARGO_TARGET_DIR=/data/cargo-target-sandbox cargo test -p kiana-session
CARGO_TARGET_DIR=/data/cargo-target-sandbox cargo test -p kiana-mcp
CARGO_TARGET_DIR=/data/cargo-target-sandbox cargo test -p kiana-builtin-extensions
```

重点观察：

- session 的 MCP runtime 构造是否因新字段编译失败（`runtime.rs:671`）；
- `kiana-session` 的新会话工具集是否同时给 read/write/edit 传了同一个 registry；
- `execute_bash` 的错误文本断言（`Unconfirmed:` / `BackendUnavailable:`）是否需要
  更新黄金测试。

---

## 5. 行为变化（不是接线，但验收时会遇到）

- `write`/`edit` 对**已存在的普通文件**要求本会话先 `read`：`write` 还要求读记录
  覆盖整个文件，`edit` 的 `oldText` 必须落在读到的行范围内。因此任何现存会话测试/
  扩展如果直接用 `write` 覆盖已有文件，会得到 `ReadBeforeEdit`；先用 `read` 工具读一次
  即可。新文件不需要读。
- 默认读记录是进程级共享表（`ReadStateRegistry::global()`），read/write/edit 的默认
  构造已经共用它；只有需要「一会话一隔离」时才按第 1 节传入独立 registry。
- 成功 `write` 后不是「完全没有记录」，而是用写出的字节建立新基线（旧快照被替换）；
  并行排队的第二个写因此能继续，同时不会用过期内容授权修改。
- 沙箱路径下 `bash` / `execute_bash` / PowerShell 的退出码只来自 relay 的
  `ConfirmedExit`；`BackendUnavailable` 与 `Unconfirmed` 是工具错误而不是退出码，
  调用方不要把它们折叠成 128+signal。
