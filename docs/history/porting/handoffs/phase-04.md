# Week 4 接线清单（wire agent 用）

本文件说明把 `kiana-hooks` 接进 `kiana-session` 需要改哪些共享文件。第 4 周 agent
只修改了 `crates/kiana-hooks/**` 与 `docs/history/porting/phase-04-hooks.md`，**没有**修改
`crates/kiana-session/**`、根 `Cargo.toml` 或 `Cargo.lock`。

## 0. 前置条件

- 在 `crates/kiana-session/Cargo.toml` 增加：
  ```toml
  kiana-hooks = { workspace = true }
  ```
  根 `Cargo.toml` 的 `[workspace.dependencies]` 已有 `kiana-hooks`，workspace
  members 也已有 `crates/kiana-hooks`，**不需要**改根 `Cargo.toml` / `Cargo.lock`。
  `Cargo.lock` 里的 `kiana-hooks` 依赖条目会由 cargo 在首次构建时自动补齐（不是
  手工编辑）；如果并行 agent 已经改过 lock，让 cargo 合并即可，不要手工覆盖。
- `kiana-hooks` 的公开面（`use kiana_hooks::{...}`）：
  - 配置/注册：`HooksConfig::{from_value, from_settings}`、`HookRegistry::add_source`、
    `HookSource::{Local, Project, User}`；
  - 执行：`HookRunner::new(registry, HookRunnerConfig::new(project_root))`、
    `run_event(&HookInput)`、`run_event_with_state(&HookInput, &mut HookTurnState)`；
  - 输入/输出：`HookInput`、`HookEvent`、`HookOutcome`（`decision` / `reason` /
    `updated_input` / `additional_context_xml()` / `blocking_error()` /
    `model_feedback` / `system_messages` / `stop_reason` / `continue_execution`）；
  - 审计：`HookAuditSink` / `HookAuditRecord`；
  - 其它：`HookRunnerConfig`、`HookTurnState`、`HookError`、
    `DEFAULT_HOOK_ENV_ALLOWLIST`。
- `HookRunner` 是 `Send + Sync`，建议在 `Core` 里存
  `hook_runner: Mutex<Option<Arc<HookRunner>>>`（与 `permissions` 字段同一模式），
  另存 `hook_turn: Mutex<HookTurnState>`。不要跨 `.await` 持锁：
  ```rust
  let runner = { lock(&self.hook_runner).clone() };
  let Some(runner) = runner else { return ... };
  let mut turn = { *lock(&self.hook_turn) };
  let outcome = runner.run_event_with_state(&input, &mut turn).await;
  { *lock(&self.hook_turn) = turn; }
  ```
- `HookTurnState` 必须**按回合共享**（PreToolUse / Stop / PreCompact / … 用同一个），
  否则每轮 deny 上限不生效。建议在 `Core::prompt` 用户提交新 prompt 时
  `turn.reset_turn()`（保留 depth）；`HookTurnState::new()` 已经会从
  `KIANA_HOOK_DEPTH` 继承被 hook 启动的嵌套进程深度。

## 1. 启动时构建 registry（信任与顺序）

文件：`crates/kiana-session/src/agent_session.rs`

现有函数 `load_session_config_layers`（约 1795 行）已经：

- 读取 System/Managed/Enterprise 策略层；
- User 用 `SettingsManager::get_global_settings()`；
- Project 用 `SettingsManager::get_project_settings()`，**仅在受信时**；
- Local 读 `<cwd>/.kiana/settings.local.json`，也**仅在受信时**。

在 `Core::new` 里 `let (config_layers, layer_errors) = load_session_config_layers(...)`
（约 1996 行）之后、构造 `Core` 之前：

```rust
use kiana_hooks::{HookRegistry, HookRunner, HookRunnerConfig, HookSource, HooksConfig};

let mut hook_registry = HookRegistry::new();
let mut hook_diagnostics: Vec<String> = Vec::new();
for layer in config_layers.layers() {
    let source = match layer.source {
        ConfigLayerSource::User => HookSource::User,
        ConfigLayerSource::Project => HookSource::Project,
        ConfigLayerSource::Local => HookSource::Local,
        _ => continue, // System/Managed/Enterprise 见 §11
    };
    let hooks_value = layer
        .value
        .get("hooks")
        .cloned()
        .unwrap_or(serde_json::Value::Null);
    let load = HooksConfig::from_value(&hooks_value);
    let diagnostics = hook_registry.add_source(
        source,
        load,
        // layers 已经在 load_session_config_layers 里按 trust 过滤；这里再传一次
        // 真实信任结论，防止未来调用路径变化时静默加载 project hooks。
        settings_manager.is_project_trusted(),
    );
    hook_diagnostics.extend(diagnostics.into_iter().map(|d| d.message));
}

let mut hook_config = HookRunnerConfig::new(cwd.clone());
hook_config.audit = Arc::new(SessionHookAudit::new(/* event emitter / tracing */));
let hook_runner = HookRunner::new(hook_registry, hook_config)
    .map_err(|error| format!("failed to initialize hooks: {error}"));
// 失败时把 hook_runner 设为 None 并记入 config_diagnostics；hook 初始化失败不应
// 阻断会话启动。
```

注意事项：

- **不要**从 `effective_config`（合并后的值）里读 `hooks`。week-3 的分层合并会对
  `hooks.*` 数组做 concat，合并后失去“哪一条来自哪个来源”；本 crate 的
  `local > project > user` 依赖逐层 `add_source`。
- `Settings` 把未知键（包括 `hooks`）保留在 `extra` 并原样序列化，`layer.value`
  里可以直接拿到 `hooks`。
- `ConfigLayerSource` 的 import 已经在文件顶部（约 109 行）。
- `HookRunnerConfig::new` 默认：30s handler 超时、8 次/轮 deny 上限、深度 5、默认
  环境变量白名单。需要调时直接改公开字段（`max_denies_per_turn` / `max_depth` /
  `env_allowlist` / `http_policy` / `audit`）。
- 配置 reload：registry 不可变；如果 `SettingsManager` reload 后要热更新，重建
  `HookRunner` 并替换 `Arc`。旧 runner 的 async hook 任务会自然结束。

## 2. PreToolUse：hook 改写 → 权限判断 → 执行

文件/函数：`Core::before_tool_call`（约 3386 行）、
`Core::permission_before_tool_call`（约 3409 行）。

当前顺序是：扩展 `before_tool_call` → 权限。必须改成：

1. 用**当前** `context.args.get()` 跑 `PreToolUse` hooks；
2. 若 `outcome.is_denied()`：直接返回 `BeforeToolCallResult` 的 block，
   `reason = outcome.blocking_error()`（**只出现一次**，不要再额外发一条错误事件）；
3. 若 `outcome.updated_input` 是 `Some`：`context.args.set(updated)`；
4. 再跑扩展 `before_tool_call`（如果扩展 block，仍以 block 为准）；
5. 最后跑 `permission_before_tool_call`（此时看到的是改写后的参数；权限 deny 仍然
   优先于 hook allow，绝不能用 hook 的 `Allow` 跳过权限层）；
6. 没有 hook / hook 失败：保持现有行为。

示例（放在 `Core::before_tool_call` 开头）：

```rust
async fn before_tool_call(
    &self,
    context: BeforeToolCallContext,
    parent_tool_call_id: Option<String>,
) -> Option<BeforeToolCallResult> {
    // 1) PreToolUse
    if let Some(outcome) = self.run_pre_tool_use_hook(&context).await {
        if let Some(reason) = outcome.blocking_error() {
            // 模型可见错误只在这里出现一次；runs/warnings 走审计。
            return Some(BeforeToolCallResult {
                block: Some(true),
                reason: Some(reason.to_string()),
                terminate: None,
            });
        }
        if let Some(updated) = outcome.updated_input {
            // 整体替换；不要 merge。
            context.args.set(updated);
        }
        // HookDecision::Ask 的处理见下。
        if outcome.is_asking() {
            // 推荐：走现有 extension_ui_context.select 审批；无 UI 时 fail-closed：
            // return Some(BeforeToolCallResult { block: Some(true),
            //     reason: outcome.reason.clone(), terminate: None });
        }
    }

    // 2) 现有扩展 hook
    let extension_result = self
        .extension_before_tool_call(&context, parent_tool_call_id)
        .await;
    if extension_result
        .as_ref()
        .is_some_and(|result| result.block == Some(true))
    {
        return extension_result;
    }

    // 3) 权限层：必须使用改写后的 context.args
    if let Some(result) = self.permission_before_tool_call(&context).await {
        return Some(result);
    }

    extension_result
}
```

构造输入：

```rust
let mut input = HookInput::new(
    HookEvent::PreToolUse,
    self.session_id_string(),
    self.cwd.as_str(),
)
.with_tool_call(context.tool_call.name.clone(), context.args.get());
if let Some(path) = self.transcript_path() {
    input = input.with_transcript_path(path);
}
if let Some(mode) = self.permission_mode_label() {
    input = input.with_permission_mode(mode);
}
let outcome = runner.run_event_with_state(&input, &mut turn).await.ok()?;
```

注意：

- `BeforeToolCallContext::args` 是 `ToolArguments` 共享句柄（`set` 取 `&self`），
  不需要 `mut context`；改写会直接传给 `execute()`。
- 权限层必须仍看到改写后的参数，且 `deny` 规则、灾难性命令检查、`read-before-write`
  门禁都在 `permission_before_tool_call` 里执行。hook 的 `Allow` 只是一个输入，不是
  最终放行。
- `HookOutcome::updated_input` 在最终 deny 时也可能有值（顺序传递）；不要应用。
- hook `Ask`：`BeforeToolCallResult` 没有 ask 三态。若暂时不接 UI，就按
  fail-closed 转成 block 并写清原因；不要静默当成 allow。

## 3. PostToolUse / PostToolUseFailure

文件/函数：`Core::after_tool_call`（约 3912 行）。

在函数开头（扩展 `tool_result` 处理之前或之后均可，建议之前）按
`context.is_error` 选择事件：

```rust
let event = if context.is_error {
    HookEvent::PostToolUseFailure
} else {
    HookEvent::PostToolUse
};
let mut input = HookInput::new(event, self.session_id_string(), self.cwd.as_str())
    .with_tool_call(context.tool_call.name.clone(), context.args.get());
if context.is_error {
    input = input.with_error(/* context.result 的错误文本 */);
} else {
    input = input.with_tool_response(json!({
        "content": context.result.content,
        "details": context.result.details,
        "isError": false,
    }));
}
let outcome = runner.run_event_with_state(&input, &mut turn).await.ok();
```

合并规则：

- `outcome.model_feedback`（exit 2 的 stderr）与 `outcome.additional_context_xml()`
  追加为工具结果里的文本内容块；`blocking_error()` 不会在 post 事件出现（crate 把
  exit 2 映射成 feedback）。
- `outcome.decision == Deny`（JSON `decision: block`）表示“把这次结果标记为失败/给
  模型反馈”，不能撤销已经执行的工具。
- `outcome.system_messages` 走用户可见提示；`suppress_output` 控制 transcript。
- 返回值如果改了 content，用 `AfterToolCallResult { content: Some(content), .. }`；
  否则保持 `None`。注意 `content` 的类型是 `Vec<UserContentBlock>`，追加文本块请
  复用现有的 text block 构造方式。

## 4. PermissionDenied

文件/函数：`Core::permission_before_tool_call`（约 3409 行）与
`Core::tool_permission_refusal`（约 3429 行）。

在 `refusal_for` 返回 `Some(拒绝原因)` 之后：

```rust
let mut input = HookInput::new(
    HookEvent::PermissionDenied,
    self.session_id_string(),
    self.cwd.as_str(),
)
.with_tool_call(tool_name, args.clone())
.with_reason(reason.clone());
let outcome = runner.run_event_with_state(&input, &mut turn).await.ok();
if outcome.retry == Some(true) {
    // 在拒绝原因后追加“hook 表示可以重试一次”，不要绕过权限层重放工具。
}
for feedback in outcome.model_feedback { /* 追加到模型可见错误 */ }
```

注意：`PermissionDenied` 的 hook 输出 `retry: true` 只是给模型的提示；不能自动重试
工具调用，更不能把 deny 改成 allow。

## 5. UserPromptSubmit

文件/函数：`Core::prompt`（约 6210 行），在 `run_input_handlers`、模板展开和
skill command 展开之后、把用户消息入队/落盘之前。

```rust
let input = HookInput::new(
    HookEvent::UserPromptSubmit,
    self.session_id_string(),
    self.cwd.as_str(),
)
.with_prompt(expanded.clone());
let outcome = runner.run_event_with_state(&input, &mut turn).await?;

if outcome.is_denied() {
    // 阻塞提交：把 blocking_error() 作为用户/模型可见错误返回一次；
    // 建议返回 AgentSessionError 或 emit 一条队列外的错误，不要静默丢弃。
    return Err(/* 或 Ok(()) + emit rejection */);
}
if !outcome.continue_execution {
    // 结束当前回合；stop_reason 作为停止原因。
}
if let Some(context) = outcome.additional_context_xml() {
    // 作为转义文本块注入，例如 <hook-context>{context}</hook-context>。
    // 不要再 escape 第二次。
}
for message in outcome.system_messages { /* 用户提示 */ }
```

## 6. SessionStart

文件/函数：`Core::new` 末尾或 `AgentSession::new` 创建完成后、第一次 `prompt`
之前。`session_start_event.reason` 映射到 `HookInput.source`：

| `SessionStartReason` | `source` |
|---|---|
| `Startup` | `"startup"` |
| `Resume` | `"resume"` |
| `Fork` | `"fork"` |
| `Reload` | `"reload"` |

```rust
let input = HookInput::new(HookEvent::SessionStart, session_id, cwd)
    .with_source(source);
let outcome = runner.run_event_with_state(&input, &mut turn).await?;
if let Some(context) = outcome.additional_context_xml() {
    // 追加到初始 system/developer 上下文。
}
```

`SessionStart` 的 exit 2 在本 crate 里被忽略（只记 warning）；不要因为 hook 返回
非零就拒绝启动会话。`continue:false` 同样不应阻止会话创建，最多提示用户。

## 7. SessionEnd

现有 `kiana-session` 没有统一的 session-end 生命周期点。建议在
`AgentSession::shutdown`（约 9833 行）/ `extension_shutdown_handler` 里做
best-effort：

- 用 `HookEvent::SessionEnd` + `with_reason("shutdown" | "other")` 运行；
- 给 `run_event` 包一层短超时（例如 2s），避免退出被 hook 拖住；
- 忽略决策，只保留 `system_messages`/审计。

如果 `shutdown` 是同步函数，需要 `spawn_local`/runtime handle；这属于产品决策，可
先只做审计日志。

## 8. Stop

文件/函数：`Core::run_before_settle_boundary`（约 5085 行），这是 agent 即将
settle 的边界，函数返回值已经表示“是否继续下一轮”。

在扩展 `agent_before_settle` 派发之前运行：

```rust
let input = HookInput::new(HookEvent::Stop, session_id, cwd);
let outcome = runner.run_event_with_state(&input, &mut turn).await?;
if outcome.is_denied() {
    // Stop 的 deny 语义是“不要结束回合”：返回 true 让 agent 继续。
    // 把 blocking_error() 作为 steering/user 消息注入一次。
    return true;
}
// additional_context_xml() 作为上下文追加；system_messages 展示一次。
```

注意 deny 上限：crate 在达到 `max_denies_per_turn` 后把 deny 降级为 `Ask`。对 Stop
来说，`Ask` 应视为“不再继续”（结束回合），否则 hook 可以让回合无限续命；请按产品
语义处理 `outcome.is_asking()`。

## 9. PreCompact / PostCompact

文件/函数：

- 手动：`Core::compact`（约 6711 行）；
- 自动：`should_compact` 分支（约 4729–4798 行）与 `run_default_compaction`
  （约 4500 行）。

建议在两条路径进入真正压缩之前统一跑 `PreCompact`（`trigger` = `"manual"` /
`"auto"`，附 `custom_instructions`）：

```rust
let input = HookInput::new(HookEvent::PreCompact, session_id, cwd)
    .with_trigger(trigger)
    .with_custom_instructions(custom_instructions.clone().unwrap_or_default());
let outcome = runner.run_event_with_state(&input, &mut turn).await?;
if outcome.is_denied() {
    return Err(AgentSessionError::CompactionCancelled); // 手动
    // 自动：跳过本次压缩并记录 warning
}
if let Some(context) = outcome.additional_context_xml() {
    // 追加到 compact 的自定义指令（转义后的文本），不要直接拼原始 additional_context。
}
```

`PostCompact` 在压缩结果产生/落盘之后运行，`additional_context` 可追加到压缩摘要
后的系统消息；它不阻塞压缩，exit 2 只记 warning。

## 10. 审计 sink

在 `kiana-session` 实现 `HookAuditSink`：

```rust
struct SessionHookAudit { events: /* mpsc::UnboundedSender 或 Arc<...> */ }

impl HookAuditSink for SessionHookAudit {
    fn record(&self, record: HookAuditRecord) {
        // 必须非阻塞：try_send / tracing；不要做磁盘 IO。
        // record.event/source/handler/summary/outcome/exit_code/http_status/
        // duration/asynchronous 都是公开字段。
    }
}
```

建议映射到现有的 `AgentSessionEvent`（新增一个 hooks 事件）或 `tracing`。`summary`
是 command 文本 / URL，可能包含用户配置里的敏感串；不要额外记录 `tool_input`。

## 11. Managed policy（allowManagedHooksOnly）

第 3 周已经在 `ManagedPolicy`/有效配置里实现了 `allowManagedHooksOnly`。本 crate
目前只有 `HookSource::{Local, Project, User}`，没有 Managed/System 来源。接线时：

- 读有效配置的 `allowManagedHooksOnly`（week-3 的 `ManagedPolicy::apply` 会把它强制
  为 true，project 层写 false 不能覆盖）；
- 为 true 时只允许 policy 层（System/Managed/Enterprise）的 `hooks` 生效；
- 由于 crate 没有 Managed 来源，短期做法是：跳过 User/Project/Local 层（最安全），
  或后续由第 4 周 owner 增加 `HookSource::Managed`（需要改 crate，不建议 wire agent
  在本波直接改）。

如果产品要求 managed hooks 可执行，追加一个 `HookSource::Managed`（优先级在 Local
之前或之后需要产品定义）是一个小的后续改动；本波先按“只跳过不受信层”接线。

## 12. 注意事项汇总

1. **顺序不能错**：PreToolUse → 权限 → 执行；updatedInput 不能绕过 deny。
2. **`blocking_error()` 只消费一次**（工具错误或 steering 消息），不要再当 warning 发。
3. **`additional_context_xml()` 已转义**，直接放进 `<hook-context>` 标签；不要二次
   转义，也不要使用原始 `additional_context`。
4. **`HookTurnState` 按回合共享**；每个事件都新建 state 会让 deny 上限失效，也会
   丢失 `KIANA_HOOK_DEPTH` 的继承。
5. `HookRunner::run_event` 是 async；`HookRunner::new` 可能返回
   `HookError::Config`（HTTP client 初始化）；启动时按诊断处理，不要 panic。
6. 异步 hook 的结果不会回到 `HookOutcome`，只在 `HookAuditSink`；不要等它。
7. 所有 hook 的进程 cwd 与 stdin `cwd` 都是 `HookRunnerConfig::project_root`；如果
   会话支持 worktree，传“稳定项目根”，不要传每次变化的 worktree 路径。
8. 递归：被 hook 启动的 Kiana 进程通过 `KIANA_HOOK_DEPTH` 继承深度；
   `HookTurnState::new()` 会自动读取，不要覆盖成 0。
