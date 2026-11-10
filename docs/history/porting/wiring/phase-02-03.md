# Wire A — 第 2/3 周共享接线与集成验证

日期：2026-11-07
范围：`docs/history/porting/phase-02-sandbox-execution-paths.md` + `docs/history/porting/handoffs/phase-02.md`、
`docs/history/porting/phase-03-config-shell-risk.md` + `docs/history/porting/handoffs/phase-03.md` 的跨 crate 接线。

本文件记录实际应用到工作树的 handoff、对 handoff 偏差的核对结论、共享文件改动、
真实检查/测试输出、剩余缺口，以及给后续波的风险。

---

## 1. 应用的 handoff 与代码核对

### Week 2 / handoff §1.1：会话级读记录 + 统一执行策略 — 已接线

实际代码与 handoff 一致，应用如下：

- `Core` 新增 `read_state: Arc<ReadStateRegistry>`，`Core::new` 创建，每个会话独立
  （不再依赖 `ReadStateRegistry::global()` 的进程级共享）。
- `Core::builtin_tool_options()` 把同一份 `Arc<ReadStateRegistry>` 传给
  `read` / `write` / `edit`，把同一份 `Arc<ExecutionPolicy>` 传给 `write` / `edit`，
  把对应的 `Arc<SandboxManager>` 传给 `bash`。
- 权限模式映射：
  - `read-only` → `ExecutionPolicy::new(SandboxPolicy::read_only(), cwd)`；
  - `bypass` → `SandboxPolicy::danger_full_access()`；
  - 其它模式 → `ExecutionPolicy::from_env(cwd)`，再
    `with_additional_writable_roots(engine.policy().workspace_roots)`（权限引擎的
    `addDirs` / `workspaceRoots` 已经归一化在这个列表里，因此不需要权限层新增
    getter）。
- 运行期 `set_permission_mode` 与 `add_permission_directory` 会重算
  `execution_policy` + `sandbox`，并重建内建工具定义（`write`/`edit` 在构造时捕获
  策略，bash 捕获 manager，必须重建才会生效）。run 进行中只置
  `pending_tool_refresh`，在安全读取点刷新，避免扩展派发期间改注册表。

与 handoff 的差异（均为实现约束导致，已核对）：

- handoff 示例的 `permission_engine.add_dirs()` 不存在；权限层把 addDirs 存进
  `PermissionPolicy::workspace_roots`（`PermissionEngine::policy()` 可读）。
- handoff 示例总是传 `sandbox: Some(manager)`。`BashToolOptions::sandbox = None`
  的真实语义是“由 `KIANA_SANDBOX_POLICY` 决定”，不是“明确不隔离”。当会话策略是
  `DangerFullAccess`（例如 bypass）时，如果传 `None`，bash 工具会重新读环境变量，
  可能出现“文件工具按 bypass 放行、shell 仍被 env 沙箱限制”的分裂。实现改为：
  - 策略需要隔离 → `sandbox: Some(manager)`；
  - 策略是 `DangerFullAccess` → 显式传无沙箱 `operations`
    （`create_local_bash_operations` / `create_local_power_shell_operations`）。
- `SandboxManager::new` 在 `Core::new` / 模式切换时构造（bubblewrap 探针结果进程内
  缓存），不是每次 exec 重新解析环境；这样 shell 与文件工具看到的是同一份策略。

### Week 2 / handoff §1.2：`execute_bash` — 已接线（比 handoff 更严格）

`AgentSession::execute_bash` 不再调用 `create_local_bash_operations_from_env`，而是：

- 权限检查（`Core::bash_refusal`）保持最先执行；
- 使用 `Core::sandbox_manager()`：
  - `Some(manager)` → `create_local_shell_operations_with_sandbox("bash", …)`；
  - `None`（会话策略明确 DangerFullAccess）→ `create_local_bash_operations(shell_path)`。

这样 `execute_bash`、工具表里的 `bash`、`write`/`edit` 的门禁使用同一份
`ExecutionPolicy`；不再出现“工具调用被沙箱、直接执行重新读 env”的两套结论。
`ExecutionStatus` 仍未结构化进 `BashResult`（`kiana-ext-api` 契约，见缺口）。

### Week 2 / handoff §1.3：压缩后失效读记录 — 已接线（方案 A）

在三个压缩提交点之后调用 `read_state.clear()`：

- 手动 `Core::compact`（`append_compaction` 成功、`refresh_finalized_context` 之后）；
- 自动压缩 `run_auto_compaction`（同上）；
- 边界草稿 `commit_boundary_drafts`（仅当 drafts 含
  `SessionBoundaryDraft::Compaction`）。

`clear()` 是最保守方案：只影响体验（压缩后第一次 write/edit 需要重新 read），
不会让已删除的 read 结果继续授权写。

### Week 2 / handoff §2：MCP stdio 子进程 — 传输层已接线，会话注入未接线

实际代码应用：

- `kiana-mcp`：
  - `StdioTransportOptions` 新增 `sandbox: Option<Arc<SandboxManager>>`（`new` /
    `frozen` / `Debug` 同步更新）；
  - `spawn_child` 有 sandbox 时走 `spawn_sandboxed_child`：先
    `ExecutionPolicy::resolve("mcp", ToolInput::McpProcess)`，Deny 直接返回
    transport 启动错误；Allow 时 `SandboxManager::spawn`（不套 relay，符合长生命
    周期服务器语义）；`SandboxError::Unavailable` 原样上报，**不回退**无沙箱；
  - 保留无沙箱路径的 `ENOEXEC → /bin/sh` 兼容行为；沙箱路径同样实现了该重试。
- `kiana-builtin-extensions`：
  - `McpExtensionOptions` 新增 `sandbox` 字段；
  - `create_default_transport_with_sandbox(...)` 新增，`create_default_transport`
    委托它并传 `None`；
  - `create_connection` 在 `options.sandbox` 存在时用捕获 sandbox 的默认工厂。

未接线部分：`builtin_extensions()` 仍用 `McpExtensionOptions::default()`
（sandbox = None），`kiana-coding-agent` 也没有把会话的 `SandboxManager` 传进
extension factory。当前仓库没有“session → 内建 MCP extension”的通道；见第 4 节。

### Week 3 / handoff §1：用分层配置生成权限 settings — 已接线（有偏差）

`Core::new` 改为：

1. System / Managed / Enterprise 从标准路径加载（逐层 `ConfigLayer::load_file`，
   保持各自的 `LayerLoadMode`），合并后 `ManagedPolicy` 自动生效；
2. User / Project 使用 `SettingsManager` 已加载的 `get_global_settings()` /
   `get_project_settings()` 建内存层；
3. 项目受信时把 `<cwd>/.kiana/settings.local.json` 作为 Local 层；
4. `effective.value()["permissions"]` 交给
   `PermissionEngine::configuration_disables` / `from_settings_and_flags`。

为什么 User/Project 不直接从磁盘再读一遍：

- `SettingsManager::in_memory`（测试与 SDK 的常见构造）没有磁盘文件，直接
  `load_standard_filtered` 会丢掉这些设置，`permissions_integration` 的
  12 个用例都会失败；
- 文件后端下 `SettingsManager` 本来就从同一路径加载，重复读盘会重复计算
  origins，并让测试依赖宿主 `~/.kiana/agent/settings.json`（本机确实存在该文件）。

`null` 语义：旧的 `merge_permission_settings` 把 `null` 当作“没有值”。
`ConfigLayers` 默认让高层 `null` 覆盖低层，因此新代码在**合并前**逐层清理
`permissions` 顶层的 `null`（`sanitize_config_layer_value`），合并后再做一次
防御性清理。这样 local 的 `{"mode": null}` 不会抹掉项目里的 `read-only`。

### Week 3 / handoff §2：managed 禁用 bypass — 已接线

没有给 `Core` 增加新字段：`PermissionEngine::from_settings_and_flags` 从传入的
`permissions` 对象读取 `disableBypassPermissionsMode`，而
`ManagedPolicy::apply` 已把它强制写进有效 `permissions`。接线只需保证第 1 步传的
是分层/策略应用后的子对象（已保证）。`bypass_permissions_disabled()` 可供诊断。

### Week 3 / handoff §5：诊断与可观察性 — 部分接线

- `Core` 保存 `config_fingerprint`、`config_origins`、`config_diagnostics`；
- 新增公开方法：
  - `AgentSession::settings_config_fingerprint()`
  - `AgentSession::settings_config_origin(key_path)`
  - `AgentSession::settings_config_diagnostics()`
- `kiana-coding-agent` 的启动诊断汇总处调用新增的
  `kiana_session::agent_session::session_config_diagnostics(...)`，把层加载错误与
  严格诊断并入 `AgentSessionRuntimeDiagnostic`（Warning）。

尚未接到 UI/事件流；“值从哪来”目前只能通过上面的访问器读取。

### Week 3 / handoff §3 / §4：MCP policy 过滤、hooks policy — 未接线

见第 4 节缺口。第 4 周 hooks 尚未实现，`effective["hooks"]` 没有消费者。

### Week 2 / handoff §3：`kiana-tools/src/index.rs` — 无需修改

所有新选项字段都有 `Default`，工具名/schema/黄金文本未变；本次未改该文件。

### Week 2 / handoff §5：行为变化 — 已在会话层生效

`write`/`edit` 的 read-before-write 门禁现在通过会话级 registry 生效；
`kiana-session` 现有测试没有直接执行 `write`/`edit` 工具，因此没有黄金测试需要
调整。`bash`/`execute_bash` 的沙箱退出码只信 `ConfirmedExit`。

---

## 2. 共享文件改动

| 文件 | 改动 |
| --- | --- |
| `crates/kiana-session/Cargo.toml` | 新增 `kiana-sandbox = { workspace = true }` |
| `crates/kiana-session/src/agent_session.rs` | 配置层组装（`load_session_config_layers` / `sanitize_config_layer_value` / `session_config_diagnostics`）；`Core` 新增 `read_state` / `execution_policy` / `sandbox` / 配置元数据字段；`Core::new` 用分层 `permissions` 建权限引擎并计算执行上下文；`builtin_tool_options`、`rebuild_builtin_tool_definitions`、`refresh_execution_policy_and_tools`；`set_permission_mode` / `add_permission_directory` 触发重建；`execute_bash` 使用会话 sandbox；三处压缩提交清空读记录；新增配置指纹/来源/诊断访问器 |
| `crates/kiana-mcp/Cargo.toml` | 新增 `kiana-sandbox = { workspace = true }` |
| `crates/kiana-mcp/src/transports/stdio.rs` | `StdioTransportOptions.sandbox`；`spawn_sandboxed_child`（policy resolve + `SandboxManager::spawn` + ENOEXEC 兼容 + fail-closed） |
| `crates/kiana-builtin-extensions/Cargo.toml` | 新增 `kiana-sandbox = { workspace = true }` |
| `crates/kiana-builtin-extensions/src/mcp/runtime.rs` | `create_default_transport_with_sandbox`；stdio options 传 `sandbox` |
| `crates/kiana-builtin-extensions/src/mcp/index.rs` | `McpExtensionOptions.sandbox`；默认传输工厂按 sandbox 选择 |
| `crates/kiana-coding-agent/src/app.rs` | 启动诊断追加配置层加载/严格诊断 |

未修改：`reference_project/**`、`docs/history/porting/plan.md`、根 `Cargo.toml` / `Cargo.lock`
（没有新增 workspace 依赖）、`kiana-tools/src/index.rs`。

说明：第 2/3 周 lane 的 `kiana-config` / `kiana-permissions` / `kiana-sandbox` 文件在
本次接线期间被并发修改过（mtime 2026-11-07 13:32），最终验证是在这些修改之后重跑的。

---

## 3. 执行顺序与 deny 优先

代码核对结果（未改顺序，只验证接线没有破坏它）：

1. `Core::before_tool_call`（`crates/kiana-session/src/agent_session.rs`）先调用
   `extension_before_tool_call`。扩展 handler 对 `event.input` 的修改通过
   `context.args.set(...)` 写回共享参数（`extension_before_tool_call` 内的
   `context.args.set(input.clone())`），因此权限层看到的是**改写后的参数**。
2. 若 hook 返回 `block: true`，直接阻止，不再进入权限层。
3. 然后 `permission_before_tool_call` → `PermissionEngine::evaluate`
   （`Deny > Ask > Allow`，deny 永远优先）。
4. 权限 `Allow` 后才执行工具。
5. `execute_bash` 路径没有 hook 改写（不是工具调用），但 `bash_refusal` 在构造
   operations / spawn 之前执行，拒绝优先于任何副作用。
6. 文件工具的执行期还有 `file_mutation_queue` 锁内的执行策略复检
   （`write`/`edit` 的 read-before-write 也在锁内），与权限层的 deny 不冲突：
   两层都拒绝时先看到权限层错误。

---

## 4. 真实检查与测试输出

全部使用 `CARGO_TARGET_DIR=/data/cargo-target-sandbox`，在 `/home/ctyun/code/kiana`
执行。最终一轮（并发 lane 修改之后）：

```text
$ cargo check --workspace
Finished `dev` profile [unoptimized + debuginfo] target(s) in 3.02s

$ cargo test -p kiana-sandbox -p kiana-tools -p kiana-config -p kiana-permissions
cargo test: 1288 passed, 1 ignored (80 suites, 39.86s)

$ cargo test -p kiana-session --test permissions_integration
cargo test: 12 passed (1 suite, 0.71s)

$ cargo test -p kiana-session
cargo test: 359 passed, 5 ignored (66 suites, 125.74s)

$ cargo test -p kiana-mcp
cargo test: 437 passed (12 suites, 26.82s)

$ cargo test -p kiana-builtin-extensions
cargo test: 272 passed (18 suites, 29.16s)

$ cargo test -p kiana-coding-agent
cargo test: 331 passed, 5 ignored (27 suites, 11.58s)

$ cargo fmt -p kiana-session -p kiana-mcp -p kiana-builtin-extensions \
    -p kiana-coding-agent -p kiana-sandbox -- --check
（无输出，退出码 0）
```

测试过程中发现并处理的问题：

- `kiana-sandbox` 的 `relay_forwards_termination_to_the_command_and_confirms_the_exit`
  一度失败：测试期望 `ConfirmedExit { code: 143, signal: Some(15) }`，而
  `relay::parse_status` 只从状态文件读退出码、永远给 `signal: None`。
  检查期间该测试文件被并发修改为期望 `signal: None`（POSIX `wait` 无法区分
  “被信号终止”与“程序自己 exit 143”，不猜测），当前工作树两者一致，测试通过。
  我们**没有**删除或 `#[ignore]` 任何测试；最终以工作树中的测试期望为准，未保留
  会与其冲突的 relay 改动。
- lane 的 `kiana-config` / `kiana-permissions` / `kiana-sandbox` 文件在验证期间被
  并发更新（13:32），之后重新跑了上述全部命令；测试数从 1282 变为 1288 即新增
  lane 用例。

---

## 5. 剩余缺口（未接线，非本波退化）

1. **session → 内建 MCP extension 的 sandbox 注入**：`McpExtensionOptions` 与
   `StdioTransportOptions` 已支持 sandbox，但 `builtin_extensions()` 用
   `McpExtensionOptions::default()`，`kiana-coding-agent` 也没有把会话级
   `SandboxManager` 传给 extension factory。需要：
   - 让 `builtin_extensions()` 接受一个 `McpExtensionOptions`（或至少
     `Option<Arc<SandboxManager>>`）；
   - 在 `kiana-coding-agent` 创建会话 runtime 时，从当前权限模式/settings 构造
     `SandboxManager` 并传入；
   - 注意 MCP 连接可能在模式切换后重连，最好传一个能按当前策略重建 manager 的
     provider，而不是固定一个 manager。
2. **MCP managed allow/deny 过滤**：`MergedConfig::policy().mcp_server_decision(...)`
   没有消费者。MCP 配置加载器在 `kiana-builtin-extensions` 内部，需要把
   `effective.policy()`（或决策结果）注入 `McpExtensionOptions`，在
   `State::from_options` / `create_connection` 之前跳过 Denied 服务器并把
   `policy_conflicts()` 作为启动诊断。
3. **hooks policy**：`allowManagedHooksOnly` 的有效 `hooks` 子树没有消费者；
   hooks 本身是第 4 周工作。
4. **结构化 `ExecutionStatus`**：`BashResult` 仍是 `kiana-ext-api` 的旧契约；
   `Unconfirmed` / `BackendUnavailable` / `Interrupted` 只以工具错误文本出现。
5. **PowerShell 的固定策略**：`PowerShellToolOptions` 没有 `sandbox` 字段。
   策略需要隔离时 PowerShell 仍走 `create_local_power_shell_operations_for_cwd`
   的 env 入口（与 `KIANA_SANDBOX_POLICY` 一致）；策略是 DangerFullAccess 时已显式
   传无沙箱 operations。若后续要让 PowerShell 跟随会话策略的固定 manager，需要给
   `PowerShellToolOptions` 增加字段。
6. **配置元数据的消费者**：指纹/来源/诊断已有访问器与启动诊断，但 TUI/事件流
   尚未展示 `origins()`；“这个值从哪来”的 UI 展示留给后续波。
7. **`SettingsManager::apply_overrides` 的 permissions 覆盖**：分层接线的 User 层
   读 `global_settings` 而不是合并后的 `settings`，因此 `apply_overrides` 对
   `permissions` 的临时覆盖不会进入层（当前仓库没有调用方这样做；只有 `--use-theme`
   覆盖 theme）。若以后需要，增加一个最高优先级的 Session 层。
8. **MCP 沙箱子进程的错误形状**：沙箱路径的 spawn 失败用
   `McpTransportError::new(error.to_string())`，不带 `ENOENT`/`EACCES` 的 Node
   错误码；无沙箱路径仍保持原形状。

---

## 6. 给后续波的风险

- **并发 lane 写同一工作树**：本次接线期间 lane 文件被并发修改过。后续波应在
  Wire 阶段冻结 lane 写入，否则测试结果会随文件变动漂移（本次已重跑并记录时间）。
- **`Core::new` 的 eager 沙箱探测**：read-only / workspace-write 会构造
  `SandboxManager`（bubblewrap 探针最多阻塞 500ms，进程内缓存）。会话大量创建的
  测试/嵌入方会看到首次构造变慢；不要把这段放进 async worker（当前在同步构造
  路径上，可接受）。
- **bypass 与 env 沙箱**：按 handoff 的映射，`bypass` 显式禁用沙箱（即使
  `KIANA_SANDBOX_POLICY=workspace-write`）。这是有意的“执行策略优先”语义；如果
  产品希望 bypass 仍保留 env 沙箱，需要改 `execution_context_for` 的映射。
- **模式切换重建工具表**：idle 时立即 `refresh_tool_registry`，run 中延迟。若后续
  在扩展派发内部调用 `set_permission_mode`，依赖 `pending_tool_refresh` 的安全读取
  点；不要在派发中途直接调用 `refresh_tool_registry`。
- **配置文件来源偏差**：User/Project 层来自 `SettingsManager`（已加载的值），
  不是再次 `load_standard_filtered`。这保证内存 manager 与测试确定性，但要求
  后续任何修改 settings 的路径都先更新 `SettingsManager` 再期望层看到新值。
- **`null` 清理是逐层、顶层**：`permissions` 下的嵌套对象里的 `null` 不清理；
  如果未来 `permissions` 增加嵌套对象字段，需要扩展清理逻辑。
- **MCP 传输的进程组语义**：沙箱路径用 `SandboxManager::spawn` 的 `detached`
  （setsid），与无沙箱路径的 `configure_command` 等价；关闭阶梯仍按 relay 外的
  进程组杀。若以后在 Windows 上启用 MCP 沙箱，`SandboxManager::spawn` 的
  `windows_hide` 为 false，窗口行为与无沙箱路径不同。
- **relay 状态不再携带信号**：POSIX `wait` 只能给出 128+signal 的退出码；测试与
  实现约定 `signal: None`。调用方不要从 `ConfirmedExit.code = 143` 推断“被
  SIGTERM 杀死”，只能当作确定退出。
