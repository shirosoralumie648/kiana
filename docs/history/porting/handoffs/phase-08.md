# 第 8 周 wire-agent 接线清单

本波只改了 `crates/kiana-ai/src/**`、`crates/kiana-agent-core/**`、
`crates/kiana-evals/**`（含 `crates/kiana-evals/Cargo.toml` 与 suites fixtures）。
**没有直接修改** `crates/kiana-session/**`、根 `Cargo.toml`、`Cargo.lock` 或其它 week 的文件。
下面是把第 8 周能力接进会话/交互层需要做的事。

## 一、必须：把 loop guard / budget 接入 `AgentOptions`

### 位置

`crates/kiana-session/src/sdk.rs`

- `pub struct CreateAgentSessionOptions`（约 176 行，所有字段可缺省）
- `let agent = Agent::new(AgentOptions { ... })`（约 736 行，函数
  `create_agent_session(...)`）
- `AgentSession::new(AgentSessionConfig { agent, ... })`（约 784 行）

### 1. 增加会话级选项字段

`kiana-agent-core` 的 `AgentOptions` 已有两个可选字段（`agent.rs` 约 537-540 行）：

```rust
pub loop_guard: Option<kiana_agent_core::LoopGuardHandle>, // Arc<Mutex<LoopGuard>>
pub budget: Option<kiana_agent_core::BudgetHandle>,        // Arc<Mutex<BudgetController>>
```

两者都实现 `Default`（`..Default::default()` 时为 `None`），不传即完全保持旧行为。
在 `CreateAgentSessionOptions` 里加：

```rust
/// 失控保护；`None` 表示不启用。
pub loop_guard: Option<kiana_agent_core::LoopGuardHandle>,
/// turn / token / cost / wall-clock 预算；`None` 表示不启用。
/// 建议用 `budget_handle(config, &resolved_model)` 在模型解析后构造。
pub budget_config: Option<kiana_agent_core::BudgetConfig>,
```

之所以用 `BudgetConfig` 而不是直接收 `BudgetHandle`：`budget_handle(config, model)`
需要最终解析出的 `kiana_ai::Model` 才能按模型价格表判断 `cost_unknown`。如果调用方
坚持自己构造 handle，也可以把字段改成 `Option<BudgetHandle>`，两条路都可用。

`CreateAgentSessionOptions` 已有 `#[derive(Default)]`，新增 `Option` 字段不会破坏
现有构造点。

### 2. 在模型解析之后构造 budget handle

模型解析发生在 `create_agent_session` 前半段（`options.model` → 设置 →
`model_runtime.get_available_models()` 的回退链）。在拿到最终 `model`、构造
`AgentOptions` 之前：

```rust
use kiana_agent_core::{
    BudgetController, LoopGuard, LoopGuardConfig, budget_handle,
};

let loop_guard = options.loop_guard.clone().or_else(|| {
    // 默认不启用；若要默认开启，用下面这行并删掉 or_else 的 None 分支：
    // Some(Arc::new(std::sync::Mutex::new(LoopGuard::new(LoopGuardConfig::default()))))
    None
});
let budget = options
    .budget_config
    .clone()
    .map(|config| budget_handle(config, &model));
```

注意 `LoopGuardHandle` 用的是 `std::sync::Mutex`，`BudgetHandle` 也是
`std::sync::Mutex`（不是 tokio Mutex）；不要在 async 上下文里跨 `.await` 持锁。
`LoopGuard::new(LoopGuardConfig::default())` 或 `LoopGuard::default()` 均可。

### 3. 透传进 `AgentOptions`

```rust
let agent = Agent::new(AgentOptions {
    initial_state: Some(AgentInitialState { /* 原样 */ }),
    // ... 原有字段全部原样保留 ...
    loop_guard,
    budget,
    ..Default::default()
})
.map_err(|error| CreateAgentSessionError::Other(error.to_string()))?;
```

不需要改 `AgentSessionConfig`：`AgentSession::new` 直接接管构造好的 `Agent`，预算/
guard 已存在 `Agent` 的 `AgentConfig` 里。`Agent::run` 内部
`create_loop_config` 会把它们复制进 `AgentLoopConfig`（`agent.rs` 约 1670-1685 行）。

### 4. 运行期读取结果

`kiana-agent-core` 已提供（本波新增）：

```rust
let guard = session.agent().loop_guard();   // Option<LoopGuardHandle>
let budget = session.agent().budget();      // Option<BudgetHandle>
// 运行中/结束后：
let snapshot = budget.as_ref().map(|b| b.lock().unwrap().snapshot());
// snapshot.stop_reason: Option<BudgetStopReason>
// snapshot.cost_unknown: bool
```

如果要运行前替换（例如用户在设置里改预算）：

```rust
session.agent().set_loop_guard(Some(handle));
session.agent().set_budget(Some(handle));
```

只影响之后开始的 run；不会打断正在进行的 run。

### 5. 语义/调用顺序（重要）

- **提醒注入时机**：`agent_loop` 只在「assistant(tool_calls) 的 `ToolResult` 已全部写入
  transcript」且「下一次 `prepare_request` 之前」调用 `take_reminder()`。绝不在
  assistant 工具调用与 tool result 之间插入消息，所以会话层的消息顺序契约不变。
- **硬停**：达到 `stop_at` 时 `Agent::prompt`/run 以
  `AgentLoopError::LoopDetected(Box<LoopDetection>)` 结束（错误可能在 run 事件里出现
  而不是同步返回，取决于现有 `Agent::prompt` 的错误合成路径）。会话层应把它当作
  「运行结束」而不是 panic；事件流里此前的 assistant/tool result 已完整。
- **预算 cap**：turn / wall-clock 到 100% 时直接结束本轮 run，不额外发 assistant 消息；
  token / cost 到 100% 时会在清空工具列表后强制模型输出最终文本。精确 cap 名称从
  `BudgetSnapshot.stop_reason`（`BudgetStopReason::{TurnCapped, TokenCapped,
  CostCapped, TimeCapped}`）读取；assistant 消息上的 `stop_reason` 是既有的
  `StopReason::Length`，精确名在 `raw_stop_reason`（camelCase）。交互层展示时请优先用
  `BudgetStopReason`，不要只根据 `stop_reason == Length` 推断「输出被长度截断」。
- **completed/failed 优先**：`finalize(Stop)` 才采用 cap；`finalize(Error/Aborted)`
  返回 `None`，不会覆盖失败/中止。会话层无需额外处理。
- **未知模型价格**：`BudgetConfig` 配了 `max_cost_usd` 且 `model_cost_known(model)==false`
  时 `snapshot.cost_unknown == true`，不把 cost 当 0；UI 应显示「成本未知」。

### 6. 配置来源建议

如果走 `SettingsManager`：

- 新增 `settings.compaction` 之外的 `settings.loopGuard` / `settings.budget`（字段映射
  可直接用 `LoopGuardConfig` / `BudgetConfig` 的 serde camelCase 形状；
  `DetectorThreshold` 也是 camelCase）。
- `create_agent_session` 读取设置构造；
- 不建议把这两个配置塞进模型配置，因为它们与 provider 无关。

## 二、建议：context report / 手工压缩事件

### 位置

- 报告构造函数：`kiana-agent-core::context_report::build_context_report`
- 压缩观测：`kiana-agent-core::context_report::observe_condensation`
- 会话手工压缩入口：`crates/kiana-session/src/agent_session.rs`
  `AgentSession::compact(custom_instructions)`（约 8739 行），内部
  `AgentCore::compact` 的取消令牌在 `compaction_abort`（约 1654 行）。

### 接法

1. 命令/UI 请求「上下文报告」时：在会话线程上复制当前
   `AgentState` 的 messages/model/tools，调用 `build_context_report(&context, &model,
   ContextReportOptions::default())`。报告自带 `prompt:overhead` 残差行与
   `utilization`，不需要再读私有字段。
   - 若 `prepare_request` 注入了临时指令，报告用 stand-in user prompt 重建，因此
     结果对「下一次请求」是准确的。
2. 手工压缩：在调用 `compact()` **之前**记录
   `baseline = get_context_usage_inner().tokens`（私有方法，可在 agent_session.rs 内
   直接调用；或从 `ContextReport.total_tokens` 取），启动观测：

   ```rust
   let event = observe_condensation(
       baseline,
       || current_tokens(),       // 每次 poll 重新读 get_context_usage_inner().tokens
       CondensationConfig::default(), // settle 2.5s, timeout 90s, poll 100ms
   ).await;
   // event.phase: Settled / Failed (timeout), event.saved_tokens
   ```

   `observe_condensation` 是 async 且内部用 `tokio::time`；在 `compact()` 的
   future 上 `tokio::select!`/`join!` 时注意：如果压缩在 90s 内没完成，
   `CondensationEvent.phase == Failed`，但压缩本身可能仍在跑——此时应继续
   `abort_compaction()` 或报错，不要把 Failed 当作压缩失败的唯一依据。
3. `ContextReport` / `CondensationEvent` 都是纯数据，可直接序列化给交互层；
   `ContextSegmentKind` 的 serde 是 camelCase（`tool`/`prompt`/`turnContext`/
   `segment`）。

## 三、可选：把 mock provider 用于交互层测试

- `kiana_ai::providers::mock::MockLlmAdmin::global()` 可以按名字注册轨迹，然后把
  `models` 指向 `MockLlm` 同名模型；无需 API key。
- 会话集成测试里推荐 `MockLlmAdmin::new()` 隔离实例 + `MockLlm::new(admin.clone())`，
  通过 `admin.requests()` 断言实际发给模型的 prompt（例如 loop guard 提醒是否只在
  `prepare_request` 之后出现）。
- `MockLlmAdmin::reset(name)` 只重置游标/请求记录，不删除步骤；换场景用
  `register` 覆盖同名轨迹。

## 四、不需要改的共享文件

- 根 `Cargo.toml`：不需要新增 workspace member，也不需要在根依赖里加任何东西。
- `crates/kiana-session/src/agent_session.rs`：本波**没有**要求改它。loop guard /
  budget 都通过 `Agent` 内部配置生效；若只做最小接线，sdk.rs 的两处改动即可。
- `Cargo.lock`：`kiana-evals` 新增依赖（`kiana-agent-core`、`kiana-sandbox`）都是
  workspace 内已有 crate，锁文件无需手工编辑；wire agent 不要手改根 Cargo.toml。

## 五、验证方式（wire 完成后）

```bash
CARGO_TARGET_DIR=/data/cargo-target-sandbox cargo check -p kiana-session
CARGO_TARGET_DIR=/data/cargo-target-sandbox cargo test -p kiana-session
CARGO_TARGET_DIR=/data/cargo-target-sandbox cargo test -p kiana-agent-core --test loop_guard_budget
```

会话层至少加一个测试：用 `MockLlmAdmin` 注册「连续 5 次相同 bash 调用」的轨迹，断言
第 3 次后 prompt 里出现 `<loop-guard ...>` 提醒，第 5 次后 run 以 `LoopDetected` 结束，
且提醒消息只出现在 tool result 之后、下一次 assistant 请求之前。
