# Week 3 接线清单（wire agent 用）

本文件说明把第 3 周的只读分层配置与 managed policy 接入运行期需要改哪些共享文件。
第 3 周 agent 没有修改 `crates/kiana-session/**`、根 `Cargo.toml`、`Cargo.lock` 或
`reference_project/**`。

## 0. 前置条件

- `kiana-config` 已经依赖 `sha2`（`crates/kiana-config/Cargo.toml` 已有条目），无需
  改根 `Cargo.toml` / `Cargo.lock`。
- 新 API 位于 `kiana_config::settings::layers`，同时在 crate 根再导出，以下两种写法
  都可用：
  ```rust
  use kiana_config::settings::layers::{ConfigLayerSource, ConfigLayers};
  // 或
  use kiana_config::{ConfigLayerSource, ConfigLayers};
  ```
- `crates/kiana-session/Cargo.toml` 已经依赖 `kiana-config`，无需新增依赖。

## 1. 用分层配置生成权限 settings

文件：`crates/kiana-session/src/agent_session.rs`
位置：`Core::new` 里现有权限块（约 1786–1818 行，搜索
`settings 的 permissions 对象启用权限引擎`）。

现有代码只合并 global/project + `settings.local.json`。建议接线顺序：

```rust
use kiana_config::settings::layers::{ConfigLayerSource, ConfigLayers};

// 1) 加载标准层。项目未受信时不要读 Project/Local（local 里可能有
//    `mode: "bypass"`，未受信项目不能给自己授权）。
let project_trusted = settings_manager.is_project_trusted();
let (layers, layer_errors) = ConfigLayers::load_standard_filtered(
    Path::new(&cwd),
    |source| match source {
        ConfigLayerSource::Project | ConfigLayerSource::Local => project_trusted,
        _ => true,
    },
);

// 2) 合并 + 应用 managed policy。
let effective = layers.resolve();

// 3) permissions 从有效配置取。
let permission_settings = effective.value().get("permissions").cloned();

let permissions_disabled = PermissionEngine::configuration_disables(
    cwd.as_str(),
    permission_settings.as_ref(),
);
let permissions = PermissionEngine::from_settings_and_flags(
    cwd.as_str(),
    permission_settings.as_ref(),
    &PermissionFlags::default(),
)
.map(Arc::new);
```

关键语义：

- `ConfigLayers::load_standard_filtered` 只加载**实际存在**的文件；不存在的层跳过。
  User 层路径是 `kiana_core::config::get_settings_path()`（默认
  `~/.kiana/agent/settings.json`），Project/Local 是 `cwd/.kiana/settings.json` /
  `settings.local.json`，System/Managed/Enterprise 是 `/etc/kiana/...`（Windows
  `%ProgramData%\kiana\...`）。
- `effective.value()["permissions"]` 已经包含旧 `merge_permission_settings` 的等价
  行为：`rules/allow/deny/ask/addDirs/workspaceRoots` 跨层 concat 去重，`mode` 以高层
  为准。差别：显式 `null` 在分层合并里会覆盖低层，而旧
  `merge_permission_settings` 会忽略 `null`。如果要保持旧行为，先对
  `permission_settings` 做一次 `null` 清理（或暂时保留旧 merge，只把
  System/Managed/Enterprise 的 policy 结果叠加进来）。
- `allow_managed_permission_rules_only` 已由 `ManagedPolicy::restrict_permission_rules`
  写进有效 `permissions`：allow/ask 只来自 managed，deny 来自所有层，`rules` 保留
  managed 全部 + 非 managed 的 deny。这里不需要再过滤。

## 2. 禁止 bypass 模式（managed policy）

第 3 周已在 `kiana-permissions` 内完成，**不需要修改 `kiana-session`**：

- `PermissionEngine::from_settings_and_flags` 会从传入的 `permissions` 对象读取
  `disableBypassPermissionsMode`（camel/snake；`true`/`"disable"`）。
- 为真时，settings 的 `mode: "bypass"`、`--permission-mode bypass` 与运行期
  `set_mode(BypassPermissions)` 都会被降级为 `default`；`evaluate` 还有一次防御性
  降级。只带这个标志、没有 mode/rules 的 `permissions` 对象也会创建引擎。
- 接线只需要保证第 1 步把**分层/策略应用后的** `permissions` 子对象传给
  `from_settings_and_flags`（`ManagedPolicy::apply` 已把
  `permissions.disableBypassPermissionsMode` 强制写成 `true`）。
- 诊断/UI 可以读 `engine.bypass_permissions_disabled()`；不需要在 `Core` 里再加一个
  bool 字段。

注意：如果用户 settings 显式 `mode: "off"`，旧语义仍是整体关闭权限层（返回
`None`），此时没有可执行的 bypass 检查；若产品要求 managed policy 连 `off` 也禁止，
需要在接线层把 `off` 改判为 `default`。

## 3. MCP 服务器策略

在启动/连接 MCP 服务器之前，用有效策略过滤：

```rust
let policy = effective.policy();
for server in configured_servers {
    let name = server.name.as_str();
    let command = server.command_array(); // Option<&[String]>
    let url = server.url.as_deref();       // Option<&str>
    match policy.mcp_server_decision(name, command, url) {
        McpServerDecision::Denied => {
            // 跳过该服务器，并把 policy 冲突/拒绝原因作为 warning 诊断展示。
        }
        McpServerDecision::Allowed | McpServerDecision::Unspecified => {
            // 继续原有连接逻辑（Unspecified 仍受普通权限规则约束）。
        }
    }
}
```

补充：

- `denied_mcp_servers` 永远从所有层合并，有效值在
  `effective.value()["denied_mcp_servers"]`（snake_case 规范键）；
  `allowed_mcp_servers` 在 managed 配置了 allowlist 时只含 managed 条目。用户/项目
  层的 deny 已经进入 `policy.mcp_server_decision`，也会把冲突条目从有效 allowlist
  剔除，所以直接按 `mcp_server_decision` 过滤即可。
- allow 与 deny 冲突的条目已经从有效 allowlist 移除；`effective.policy_conflicts()`
  给出**管理员策略内部**的 allow/deny 冲突列表（用户 deny 与 managed allow 的冲突
  体现在决策 Denied 上，不计入 policy_conflicts），可转成启动诊断。
- 空 allowlist（`Some(vec![])`）表示拒绝全部服务器；`Unspecified` 表示没有策略
  allowlist。

## 4. Hooks policy

`ManagedPolicy::allow_managed_hooks_only == true` 时，hooks 加载器必须使用
`effective.value()["hooks"]`，不要再用原始 `settings_manager.get_settings().raw("hooks")`
（后者包含用户/项目 hooks）。`ManagedPolicy::apply` 已经把有效 `hooks` 替换成
managed-only 子树。

## 5. 诊断与可观察性

在启动诊断收集处（`collect_settings_diagnostics` 的调用点附近）追加：

```rust
for diagnostic in effective.diagnostics() {
    diagnostics.push(AgentSessionRuntimeDiagnostic {
        kind: DiagnosticKind::Warning,
        message: diagnostic.to_string(), // file:line:column: message (at key.path)
    });
}
for error in layer_errors {
    diagnostics.push(AgentSessionRuntimeDiagnostic {
        kind: DiagnosticKind::Warning,
        message: format!("Failed to load config layer: {error}"),
    });
}
```

`effective.fingerprint()`（`sha256:<hex>`）可作为会话/缓存的配置版本号；
`effective.layer_fingerprints()` 给出每层指纹。`effective.origins()` /
`effective.origin("permissions.mode")` 用于“这个值从哪来”的展示。

## 6. ShellAnalyzer 的可选接线

第 3 周已经把 `ShellAnalysis` 放进 `PermissionRequest`，`kiana-session` 不需要改代码
即可获得更严格的 Allow 匹配。若要给审批 UI 增加风险提示：

```rust
if let Some(shell) = request.shell_analysis() {
    // shell.status / shell.has_redirection / shell.has_heredoc /
    // shell.has_command_substitution / shell.has_unclosed_quote / shell.has_newline
    // shell.virtual_operations(&request.cwd)
}
```

不要在 UI 之外用 `ShellAnalysis` 替代 `kiana-sandbox`；它只用于展示与规则建议。

## 7. 不需要改的东西

- 根 `Cargo.toml` / `Cargo.lock`：没有新增 workspace 依赖。
- `SettingsManager` 的写回路径：分层配置只读；字节级 golden 测试（`settings_manager*`
  等）必须保持原样。
- `crates/kiana-tools`：第 2 周所有，未触碰。
- `crates/kiana-core/src/paths.rs`：未修改。

## 8. 验证命令

```bash
CARGO_TARGET_DIR=/data/cargo-target-sandbox timeout 900 cargo check -p kiana-session
CARGO_TARGET_DIR=/data/cargo-target-sandbox timeout 900 cargo test -p kiana-session --test permissions_integration
CARGO_TARGET_DIR=/data/cargo-target-sandbox timeout 900 cargo test -p kiana-config
CARGO_TARGET_DIR=/data/cargo-target-sandbox timeout 900 cargo test -p kiana-permissions
```
