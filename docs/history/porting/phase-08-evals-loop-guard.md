# 第 8 周：评测和 loop guard

日期：2026-11-07
对应 `docs/history/porting/plan.md` 第 8 周。

## 改了什么

### 1. `kiana-ai`：mock-LLM / 确定性 provider

新增 `crates/kiana-ai/src/providers/mock.rs`（`providers/mod.rs` 增加 `pub mod mock;`）：

- **命名轨迹**：`MockLlmAdmin::{register, reset, reset_all, unregister, requests,
  clear_requests, call_count, pending_steps}`；`MockLlmAdmin::global()` 是进程级注册表，
  `MockLlmAdmin::new()` 是隔离实例（测试推荐）。同名注册替换旧状态；`reset` 游标归零、
  清空请求记录与调用计数，步骤保留。
- **轨迹步骤**：`MockTrajectory` + `MockStep::{Respond, Dynamic}`。动态步骤拿到
  `&MockRequestRecord`，可依据 prompt、上一轮 tool result、changed-files 分支。
- **请求记录**：每次 `stream`/`stream_simple` 在消费脚本**之前**记录
  `MockRequestRecord { trajectory, sequence, model, provider, api, messages, prompt_text,
  tool_names, changed_files, affected_tests }`。`last_user_text()` / `prompt_contains()`
  供动态轨迹与测试断言使用。
- **无 API key**：provider 注册的 auth 固定返回 `ModelAuth::default()`；
  `models.complete(...)` 在没有任何凭据时也能完成（测试
  `models_complete_without_any_api_key`）。
- **用量/费用**：显式 `usage` 原样保留；缺省时用 `kiana_ai::utils::estimate` 估算
  prompt/输出 token，并按模型 `cost` 调 `calculate_cost` 计价。
- **changed-files → affected-tests**：`AffectedTestMap` / `AffectedTestRule` 支持前缀规则、
  `always`（回归分片）、`unmappedRunsAll`（默认 `__ALL__`）、`tests/...` 变更取所在目录；
  `from_json` 读取 `{"rules":[{"path","tests"}],"always":[],"unmappedRunsAll":true}`。
  `MockRequestRecord.changed_files` 从 assistant tool call 的参数递归提取路径。
- 队列耗尽返回 `stop_reason = error` 的消息（文本
  `No more mock responses queued for trajectory "<name>"`），不 panic。

### 2. `kiana-agent-core`：loop guard

新增 `crates/kiana-agent-core/src/loop_guard.rs`：

- `repeat_key = sha256(tool_name + canonical_json(args))`；`canonical_json` 递归排序对象键，
  计算前剔除顶层 `description` / `display`。
- 12 个检测器，每个有独立的 `DetectorThreshold { reminder_at, stop_at }`：
  `ConsecutiveToolCalls`（3/5）、`GlobalRepeatedToolCalls`（8/15）、
  `AlternatingToolCalls`（3/5 周期）、`ReadFileLoop`（4/8）、`ShellStagnation`（2/4）、
  `ActionStagnation`（4/6）、`InvalidArgumentRepeat`（2/3）、
  `ExecutionFailureRepeat`（3/5）、`RepeatedThought`（2/3）、`ContentChanting`（6/10，
  64 字符块）、`LongCycleRepetition`（2/4，160 字符块）、`TurnToolCallLimit`（软 100 /
  硬 1000，`hard_tool_call_multiplier = 10`）。
- 默认 `stop_on_detection = false`：先 `Warn`（缓存 `pending_reminder`），达到 `stop_at`
  才 `Stop`。`warned` 集合保证同一检测器/同一 key 只提醒一次。
- shell 停滞与 action 停滞都要求**输出逐字相同**；stateful poll 的输出变化会重置计数。
- 注入点：`agent_loop::run_loop` 在工具结果全部写入 transcript、下一次
  `prepare_request` 之前才调用 `take_reminder()` 并追加一条 system 消息；绝不在
  `assistant(tool_calls)` 与 `ToolResult` 之间插入。
- 硬停返回 `AgentLoopError::LoopDetected(Box<LoopDetection>)`，不 panic；此前的
  assistant 消息与 tool result 已经通过事件发出。

### 3. `kiana-agent-core`：turn / token / cost / wall-clock 预算

新增 `crates/kiana-agent-core/src/budget.rs`：

- `BudgetConfig { max_turns, max_tokens, max_cost_usd, max_wall_clock_ms, warn_ratios }`；
  `BudgetController::{begin_turn, record_usage, finalize, snapshot}`。
- 50% / 80% 各提醒一次，作为 `<budget-warning ...>` system 消息在下一次请求末尾注入。
- 100%：
  - turn / wall-clock：`begin_turn` 返回 `stop`，agent loop 直接结束（不产生额外
    assistant turn）；
  - token / cost：返回 `force_final`，agent loop 把 `context.tools` 置空（
    `declare_tool_changes` 因此写入 `toolsRemoved`）、追加
    `<budget-exhausted ...>` 指令、强制模型输出最终文本；若模型仍返回工具调用，不执行，
    改为合成失败 tool result 后结束。
- **stop reason 表示**：`kiana-ai::StopReason` 是跨 crate 公开枚举，新增变体会破坏下游
  穷尽 match（`kiana-interactive/components/tree_selector.rs`、`kiana-durable/harness/*`
  等非本波所有权文件）。因此精确 cap 用 `BudgetStopReason::{TurnCapped, TokenCapped,
  CostCapped, TimeCapped}` 表达，写入 assistant 消息时 `stop_reason` 折到既有的 `Length`，
  同名 camelCase 字符串写进 `raw_stop_reason`；`BudgetController::stop_reason()` /
  `BudgetSnapshot` 也可直接读取。`completed`（`Stop`）采用 cap；`failed`
  （`Error` / `Aborted`）不被覆盖。
- 未知价格：`model_cost_known(model)` 为 false 且配置了 cost 预算时，
  `BudgetStatus::CostUnknown` / `snapshot.cost_unknown = true`，不把成本按 0 处理。

### 4. `kiana-agent-core`：context report 与手工压缩

新增 `crates/kiana-agent-core/src/context_report.rs`：

- `build_context_report(context, model, options)`：复制 transcript，追加 stand-in user
  prompt，经 `convert_to_llm` 投影后用 `estimate_context_tokens_for_messages` 计算总量
  （有 assistant usage 时以 usage 为锚，只补算其后的消息）。按
  `tool` / `prompt` / `turnContext` / `segment` 分类，每段给出 `tokens`、`chars`、
  截断预览；usage 锚点与逐段估算的残差记为 `prompt:overhead`。
- `ContextReport { total_tokens, usage_tokens, trailing_tokens, context_window,
  utilization, tool_tokens, prompt_tokens, turn_context_tokens, segment_tokens, ... }`。
- `observe_condensation(baseline, current_tokens_fn, config)`：默认
  `settle = 2.5s`、`timeout = 90s`、`poll = 100ms`、`min_saved_tokens = 1`。看到 token
  低于基线并稳定 `settle` 后返回 `CondensationEvent { phase: Settled, before_tokens,
  after_tokens, saved_tokens, elapsed_ms }`；超时返回 `Failed`。

### 5. `kiana-agent-core`：接线

- `AgentLoopConfig` / `AgentOptions` / `AgentConfig` 新增 `loop_guard`、`budget` 两个
  `Option<Arc<Mutex<...>>>` 字段，并提供 `AgentLoopConfig::with_loop_guard` /
  `with_budget`。
- `agent_loop::run_loop` 每次内层迭代 = 一次请求 = 一个 budget turn；预算提醒与 loop
  guard 提醒在 `declare_tool_changes` 之后、`prepare_request` 之前统一注入。
- `AgentLoopError::LoopDetected` 新增。
- 新增模块从 crate 根再导出：`budget::{...}`、`loop_guard::{...}`、
  `context_report::{...}`。

### 6. `kiana-evals`：原生 task suite

新增 `crates/kiana-evals/src/native.rs`；`cli::run` 识别 `run` 子命令并转交
`native::run_cli`（其余参数保持旧 docs A/B 行为）：

- 任务布局 `suites/<suite>/<task>/{meta.json,solution/,tests/,examples/}`；
  `meta.json` 的 `files[].role` 区分 `test` / `example` / `solution`，缺省按目录发现。
  solution 每次 trial 前重置进 workspace。
- `kiana-evals run --suite smoke --trials 3 --sandbox`：
  - 每个 task × trial 使用独立 workspace；
  - mock 轨迹驱动 `kiana-agent-core` 的完整工具循环（`write_file` / `read_file` /
    `bash`）；
  - 测试命令通过 `CommandRunner` 执行；CLI 实现是 `SandboxCommandRunner`，所有命令
    （测试命令与模型 `bash`）都在 `kiana-sandbox` 的 workspace-write 策略下运行；
  - 失败时把 exit code/stdout/stderr 追加为下一轮 prompt 的 `Test failed ...` 反馈，
    产生第 2 次尝试；
  - 每个 task 写 `artifacts/<suite>/tasks/<task>/results.json`，suite 汇总写
    `artifacts/<suite>/results.json`，字段含 `pass`、`cost`、`durationMs`、`tokens`、
    `malformed`、`contextExhausted`、`attempts[]`；
  - 汇总 `passAt1`、`passAtK`、`k`、`wellFormedPercent`、`totalCost`、`costUnknown`、
    `allPassed`。
- 断点续跑：记录先写 `status = running`；只有 `passed/failed/error` 视为完整并跳过。
- fingerprint = suite 文件内容 + meta + 模型费用表的 sha256；`--dry-run` 在 fingerprint
  不变时零模型调用返回旧结果，变化时拒绝运行；`--replay <results.json>` 只读结果文件。
- 未启用 `--sandbox` 时在创建 provider 之前返回 `SandboxRequired`；后端不可用返回
  `SandboxUnavailable`。
- 新增内置 `suites/smoke/{write-42,already-correct}`：`write-42` 第一次写 41（测试失败），
  收到反馈后写 42（通过）；`already-correct` 一次通过。

### 7. 参考设计对照

| 参考项目文件 | 借鉴的设计点 | Kiana 落点 |
|---|---|---|
| `reference_project/qwen-code/packages/cli/src/commands/review/mock-provider.ts` | 协议/应答分离、请求日志（JSONL/记录）、responder 按请求内容分支、body 上限与错误形态 | `kiana-ai/src/providers/mock.rs`：`MockTrajectory` / `MockStep::Dynamic` / `MockRequestRecord` / `MockLlmAdmin` |
| `reference_project/OpenHands/tests/e2e/mock-llm/scripts/resolve-affected-tests.mjs`（及 `__tests__/e2e/resolve-affected-tests.test.ts`） | source → test shard 映射、`__ALL__` 兜底、test-only 变更取所在目录、regressions 追加 | `mock.rs::AffectedTestMap::{from_json,resolve}`，`MockRequestRecord.affected_tests` |
| `reference_project/gemini-cli/packages/core/src/services/loopDetectionService.ts` | `sha256(name+JSON(args))`、连续/周期工具调用、A-B 交替、64 字符内容块 chanting、代码块/结构元素重置、阈值常量 | `kiana-agent-core/src/loop_guard.rs`：`repeat_key`、`observe_consecutive/global/alternating`、`detect_content_chanting` |
| `reference_project/goose/crates/goose/src/agents/context_report.rs` | stand-in prompt 重建下一次请求、`tool/prompt/turnContext/segment` 分类、预览截断、逐段和与请求总量的残差行 | `kiana-agent-core/src/context_report.rs::build_context_report` |
| `reference_project/goose/crates/goose/src/agents/state_machine/ops_maxturns.rs` | turn 预算与「接近上限」提示、到上限后收尾而不是继续请求 | `kiana-agent-core/src/budget.rs`：`begin_turn` / `BudgetWarning` / `TurnCapped` |
| `reference_project/aider/benchmark/benchmark.py` | 每个任务独立目录、逐题测试命令、每题结果 JSON、失败重试统计 | `kiana-evals/src/native.rs`：`SuiteSpec` / `run_trial` / `TrialResult` |
| `reference_project/SWE-agent/sweagent/run/hooks/swe_bench_evaluate.py` | 每实例 `results.json`、按实例输出目录聚合报告 | `native.rs` 的 `TaskResults` / `NativeSuiteResults` 双写 |
| `reference_project/qwen-code` / `goose`（eval 结果与预算 evidence 的分离） | 评测结果与预算/成本字段同记录、未知价格显式标记 | `TrialResult.cost/cost_unknown`、`NativeSummary.cost_unknown` |

## 测试命令与真实结果

全部使用 `CARGO_TARGET_DIR=/data/cargo-target-sandbox`（部分运行加了
`CARGO_INCREMENTAL=0`，共享 target 盘接近满时避免增量缓存继续膨胀）。

```text
$ CARGO_INCREMENTAL=0 cargo test -p kiana-ai
cargo test: 2043 passed, 341 ignored (202 suites, 75.22s)
（ignored 均为原有的 live/凭据相关测试，未新增 ignore，也未被本波改动）

$ CARGO_INCREMENTAL=0 cargo test -p kiana-agent-core
cargo test: 225 passed (9 suites, 5.84s)

$ CARGO_INCREMENTAL=0 cargo test -p kiana-evals
cargo test: 204 passed (11 suites, 3.86s)

$ cargo fmt -p kiana-ai -- --check
$ cargo fmt -p kiana-agent-core -- --check
$ cargo fmt -p kiana-evals -- --check
（三个包 fmt 全部干净，exit 0）
```

新增/重点用例：

```text
$ cargo test -p kiana-ai --lib providers::mock
running 9 tests
... 9 passed

$ cargo test -p kiana-agent-core --test loop_guard
running 11 tests
... 11 passed

$ cargo test -p kiana-agent-core --test loop_guard_budget
running 10 tests
... 10 passed

$ cargo test -p kiana-evals --test native_suite
running 6 tests
... 6 passed
```

真实 CLI smoke（bwrap 0.6.1，Python 3.10.12）：

```text
$ CARGO_TARGET_DIR=/data/cargo-target-sandbox cargo run -q -p kiana-evals -- \
    run --suite smoke --trials 3 --sandbox --artifacts /tmp/kiana-eval-native
suite smoke: 2 task(s) × 3 trial(s)
pass@1 100.0%  pass@3 100.0%  well-formed 100.0%  cost $0.020331
（exit 0）

$ ... run --suite smoke --trials 3 --artifacts /tmp/kiana-eval-native --dry-run
suite smoke: ...（同上，零模型调用）
（exit 0）

$ ... run --suite smoke --replay /tmp/kiana-eval-native/smoke/results.json
suite smoke: ...（只有 summary，不访问模型/沙箱）
（exit 0）
```

`/tmp/kiana-eval-native/smoke/results.json` 关键字段（真实输出）：

```json
{
  "summary": {
    "tasks": 2, "trials": 3, "passAt1": 1.0, "passAtK": 1.0, "k": 3,
    "wellFormedPercent": 100.0, "totalCost": 0.020331,
    "costUnknown": false, "allPassed": true
  },
  "records": [
    {"task": "write-42", "trial": 1, "status": "passed", "pass": true,
     "attempts": [
       {"attempt": 1, "test": {"exitCode": 1}, "tools": ["write_file"]},
       {"attempt": 2, "test": {"exitCode": 0}, "tools": ["write_file"]}
     ], "tokens": 1400}
  ],
  "modelCalls": 18
}
```

## 未完成项（gaps）

- `StopReason` 没有新增 `TurnCapped/TokenCapped/CostCapped/TimeCapped` 变体：这会破坏
  `kiana-interactive` / `kiana-durable` 等非所有权文件的穷尽 match。cap 用
  `BudgetStopReason` 表达，assistant 消息的 `stop_reason` 折到 `Length`，精确名称在
  `raw_stop_reason` 与 `BudgetSnapshot.stop_reason`。如果后续允许修改下游，可再把这些
  变体升为一等 `StopReason`。
- 原生 suite 目前只有确定性 mock 轨迹（`meta.mock`），没有把真实 provider 接进
  `run --suite`；真实模型评测仍走旧 docs A/B 路径。
- workspace 会复制 `tests/` 文件，模型理论上可以读取隐藏测试；当前 smoke 任务不依赖
  隐藏性。若要严格隐藏，需要把测试文件放到 workspace 外并在运行测试命令前临时挂载。
- `context_exhausted` 只根据 assistant `error_message` 里的 context/prompt + exceed/
  overflow/limit 文案推断；没有和 provider 的结构化错误分类（第 9 周）联动。
- `observe_condensation` 是独立观测器；还没有接到会话的实际压缩触发点（session 接线
  见 handoff）。
- `pass@k` 没有置信区间与预算控制；`k` 超过实际 trial 数时按已有 trial 计算。
- 预算 `TurnCapped` / `TimeCapped` 不发 assistant 消息（只结束 run）；调用方需从
  `BudgetController`/`BudgetSnapshot` 读取。`TokenCapped`/`CostCapped` 的 final turn
  若模型仍返回 tool call，会合成失败 tool result 后结束，不重试。
- 原生 suite 的 `bash` 工具与测试命令在 CLI 下都走沙箱；测试里的 `FakeRunner` 只验证
  控制流，不重复验证 sandbox 后端。
- loop guard 的非法参数识别是文本启发式（"invalid arguments"/"validation failed" 等），
  不是从校验器拿到结构化错误码。

## 风险

- `ContentChanting` / `LongCycleRepetition` 对长段重复的代码/模板文本可能误报；默认只
  Warn，且 64/160 字符块与距离阈值是经验值，需要真实轨迹校准。
- `GlobalRepeatedToolCalls`（8/15）对「批量读同一文件的不同区段」可能偏严；`ReadFileLoop`
  已按路径去重，但 offset 变化不算不同路径。
- 预算在 `prepare_request` 之后再次清空 tools；如果 `prepare_request` 依赖自己注入的
  tools，final turn 会看不到它们（这是 100% 时的预期行为，但需要会话层知晓）。
- 原生 suite 的 workspace 保留在磁盘上用于调试，长时间重复运行会累积；尚未做清理策略。
- `model_calls` 是 mock provider 的请求数，不包含真实网络重试（mock 不重试）。
- sandbox 的 `.git`/`.kiana` 只读 tmpfs 挂载点会在宿主 workspace 留下空目录（bwrap
  创建挂载点的副作用），不影响任务。
