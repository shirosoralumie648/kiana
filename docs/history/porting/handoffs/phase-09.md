# 第 9 周 wire-agent 接线清单

本波只改了 `crates/kiana-ai/**`、`crates/kiana-telemetry/**`、
`crates/kiana-agent-core/**`、`crates/kiana-sandbox/src/**` 及对应测试。
**没有直接修改** `crates/kiana-session/**`、`crates/kiana-tools/**`、`crates/kiana-mcp/**`、
`crates/kiana-evals/**`、根 `Cargo.toml`、`Cargo.lock` 或其它 week 的文件。
下面是把第 9 周能力接进会话/交互/provider 层需要做的事。

## 一、必须：在 provider 请求路径上调用请求策略

### 位置

- `crates/kiana-ai/src/providers/*.rs`：每个适配器的 `stream` / `stream_simple` 入口，
  或统一的 `crates/kiana-ai/src/api/*` 请求构造处；
- `crates/kiana-agent-core/src/agent_loop.rs`：如果希望策略与 provider 无关，
  在 `prepare_request` 之前调用一次（`kiana-agent-core` 是本波所有权范围，可直接改）。

### 1. 构造能力门

`kiana_ai::request_policy` 已导出：

```rust
use kiana_ai::request_policy::{
    CapabilityGate, CapabilitySet, ModelOperation, ProviderCapabilityManifest,
    RequestRequirements,
};
```

- **manifest 侧**：模型目录/内置 provider 表里声明该模型的实际能力。例如文本模型用
  `CapabilitySet::language_chat()`；有 vision 的模型
  `.with_vision(true)`；图像模型 `CapabilitySet::image_generation()`。如果走
  `ProviderCapabilityManifest`，provider 级默认能力 + `models` 的每模型覆盖由
  `capabilities_for(model_id)` 合并。
- **adapter 侧**：适配器结构体上声明它确实实现了什么。**必须保守**：没有实现的字段
  不要声明 true，否则能力门会放行一个必然失败的请求。
- **请求需求**：

```rust
let requirements = RequestRequirements {
    operation: ModelOperation::Language,
    tools: !tools.is_empty(),
    json_output: options.response_format.is_some(),
    parallel_tool_calls: options.parallel_tool_calls.unwrap_or(false),
    vision: context_contains_image(&context),
};
```

### 2. 发请求前调用 `prepare_request`

```rust
use kiana_ai::request_policy::{RequestPolicyInput, prepare_request};

let mut input = RequestPolicyInput::new(
    &model,          // 解析后的 kiana_ai::types::Model
    &context,        // 已归一化的 TranscriptContext
    &manifest_caps,  // 该模型的 manifest 声明
    &adapter_caps,   // 该适配器的实现声明
    &requirements,
);
input.caller_max_tokens = options.max_tokens; // Option<u64>，显式 0 会保留

let plan = match prepare_request(&input) {
    Ok(plan) => plan,
    Err(error) => return Err(policy_error_to_provider_error(error)),
};
```

`prepare_request` 的顺序是能力门 → 输入预检 → 输出预算 → cache 语义，纯函数、无 IO；
任何失败都**没有发过网络请求**。错误映射建议：

- `RequestPolicyError::Unsupported(_)`：终止本次请求/run，向用户显示
  `to_string()`（它同时点名缺失的 manifest/adapter 能力）；**不要重试**。
- `RequestPolicyError::ContextWindow(_)`：走压缩/换模型；**不要重试原请求**。
- `RequestPolicyError::Budget(OutputBudgetError::ContextOverflow { .. })`：同上。

`plan` 字段：

```rust
plan.max_tokens            // 写进请求的 max_output_tokens / max_tokens
plan.precheck              // 估算输入/窗口/可用空间（诊断用）
plan.cache                 // CacheSemantics
plan.cache_warmer_eligible // 是否可安排 cache warmer（本波只有资格判断）
```

### 3. 把 cache 语义落到 wire payload

- 只有 `plan.cache == CacheSemantics::ExplicitBreakpoints { .. }` 时调用
  `apply_cache_breakpoints(&mut payload, plan.cache)`；返回注入数。
- 必须在**最终** wire payload 上调用一次（适配器之后不要再重组消息，否则断点丢失）。
- `ImplicitStrict` / `ImplicitTolerant` / `Uncached` 不修改 payload。
- `cache_warmer_eligible == true` 只说明「有资格」（模型 `promptCache` 声明了非零 TTL
  且语义有缓存）；调度器尚未实现。

### 4. provider 失败后的分类/重试

错误可能来自 HTTP 状态、SDK 错误对象或响应 body。先构造 typed 字段，再回落到结构遍历：

```rust
use kiana_ai::request_policy::{
    TypedProviderError, ProviderErrorClass,
    classify_provider_error_with_typed, is_retryable_provider_error,
};

let typed = TypedProviderError {
    status: response_status,          // Option<u16>
    code: error_code.as_deref(),      // provider 的结构化 code
    message: Some(message.as_str()),  // provider 的错误消息
    is_retryable: sdk_is_retryable,   // Option<bool>，provider/SDK 明确给出时才有
    classification: None,             // 已有结构化分类时直接填
};
let class = classify_provider_error_with_typed(Some(&typed), &error_body_json);
let retry = is_retryable_provider_error(Some(&typed), &error_body_json);

match class {
    ProviderErrorClass::ContextWindow => { /* 压缩后重试一次，或返回给 agent 层 */ }
    ProviderErrorClass::RateLimit | ProviderErrorClass::Transient if retry => {
        /* 指数退避；Retry-After 头另行处理 */
    }
    ProviderErrorClass::Quota | ProviderErrorClass::Auth => { /* fail fast，不重试 */ }
    ProviderErrorClass::InvalidRequest => { /* 不应重试；把消息交给调用方修复 */ }
    _ => { /* Unknown：保守，最多一次或直接失败 */ }
}
```

要点：

- 429 + "context length" 会被判成 `RateLimit`，**不会**触发压缩；
- typed `isRetryable=false` 覆盖状态码集合；
- 无 status 的纯文案错误只在单条 "Provider returned error" 兜底时可重试，避免误重试；
- 不要用 `classify_provider_error` 单独决定压缩；响应后的 `utils::overflow` 与
  usage 仍是权威，分类只在错误路径使用。

### 5. 成功路径的 usage 记账

```rust
use kiana_ai::request_policy::account_provider_usage;

let accounted = account_provider_usage(&model, &provider_usage, user_declared.as_ref());
message.usage = accounted.usage;
// accounted.cost_source: Canonical / UserDeclared / ProviderReported
```

canonical 只在模型目录有有限非零价格时生成；价格未知时回退用户声明/provider 上报，
不会把未知成本记成 0。把 `cost_source.as_str()` 写进会话 diagnostics 或遥测属性，
让下游能回答「这个 cost 从哪来」。

### 6. 声明式 provider（可选）

如果会话/配置加载声明式 provider，用：

```rust
use kiana_ai::request_policy::load_declarative_provider;

let result = load_declarative_provider(&value)?; // Result<DeclarativeLoadResult, DeclarativeProviderError>
// result.provider，result.diagnostics: Vec<DeclarativeDiagnostic>
```

坏模型条目只跳过该条（诊断码 `invalid_model_id` / `duplicate_model_id` /
`invalid_model_entry` / `invalid_capability` / `invalid_token_budget`），未知字段
原样保留。加载后可把 `DeclarativeModel.capabilities` 直接转成 `CapabilitySet` 接能力门。

## 二、必须：SessionTelemetry 接入会话生命周期

### 位置

- `crates/kiana-session/src/sdk.rs`：`CreateAgentSessionOptions`（约 176 行）与
  `create_agent_session`（约 736 行）；
- `crates/kiana-session/src/agent_session.rs`：`AgentSession`/`Core` 的 run、工具执行、
  压缩、hook、启动阶段；
- CLI 退出路径（`kiana-coding-agent` 或 `kiana-interactive`）：在 runtime 关闭前
  `flush()`。

### 1. 构造

```rust
use std::sync::Arc;
use kiana_agent_core::SessionTelemetry;
use kiana_telemetry::{OtlpExporter, OtlpConfig};

// 没有 endpoint 时返回 Ok(None)，遥测整体关闭（不要报错）。
let exporter = match OtlpExporter::from_env()? {
    Some(exporter) => Some(Arc::new(exporter)),
    None => None,
};
let telemetry = Arc::new(match exporter {
    Some(exporter) => SessionTelemetry::new().with_exporter(exporter),
    None => SessionTelemetry::new(),
});
```

`OtlpExporter::from_env()` 读取：

- `OTEL_EXPORTER_OTLP_TRACES_ENDPOINT`（优先）/ `OTEL_EXPORTER_OTLP_ENDPOINT`；
- `OTEL_EXPORTER_OTLP_PROTOCOL`（`grpc` / `http/json` / `http/protobuf`，默认
  `http/protobuf`）；
- `OTEL_EXPORTER_OTLP_HEADERS`（`k=v,k2=v2`）；
- `OTEL_SERVICE_NAME`（默认 `kiana`）。

内容捕获默认关闭；只有
`OTEL_INSTRUMENTATION_GENAI_CAPTURE_MESSAGE_CONTENT=true`（大小写不敏感、trim）
才记录 prompt/tool 参数。`SessionTelemetry::new()` 在构造时读取，运行期切换环境变量
需重建实例。

### 2. 埋点调用点

```rust
// run 开始 / 结束
let run_span = telemetry.begin_agent_run(agent_name).await?;
run_span.finish().await;                 // 失败用 finish_with_error("...", "...")

// 每一轮
let turn_span = telemetry.begin_turn(turn_index, &model.id).await?;
// 流式过程中用 TurnTracker 记 TTFT/TTFM：
let mut tracker = TurnTracker::new();
tracker.record_token(is_meaningful_token);
// 轮结束可用 record_turn(turn_index, &tracker, stop_reason_str)，或
// turn_span 上 set_attributes 后 finish。

// 每次模型请求（含重试）
telemetry.record_api_call(&ApiCallRecord {
    provider, model, response_model,
    duration_ms, ttft_ms, ttfm_ms,
    usage, cost_source: Some(accounted.cost_source.as_str().into()),
    stop_reason, error_type,
}).await?;

// SSE 事件（可选，高频时采样）
telemetry.record_sse_event(Some(&api_span), "content_block_delta");

// 工具
telemetry.record_tool_call(&ToolCallRecord {
    tool_name, tool_call_id, status: ToolStatus::Ok,
    duration_ms, error_type: None,
}).await?;

// 压缩 / hook / 启动阶段
telemetry.record_compaction(&CompactionRecord {
    trigger: "auto".into(), duration_ms,
    input_tokens: before_tokens, output_tokens: after_tokens,
    outcome: "ok".into(),
}).await?;
telemetry.record_hook_run(&HookRunRecord {
    hook_event: "PreToolUse".into(), hook_name: Some(name),
    duration_ms, outcome: "allow".into(),
}).await?;
telemetry.record_startup_phase("model_resolution", duration_ms).await?;

// 内容属性（内部再判断 capture 开关，关闭时是 no-op）
telemetry.set_content_attributes(&span, Some(prompt), Some(tool_args_json));
```

`record_*` 的错误只代表遥测记录失败，不应中断 agent；建议 `let _ = ...` 或记录一次
内部诊断。

### 3. flush / shutdown 顺序

- `telemetry.flush().await` 把内存记录交给 `OtlpExporter` 并导出；
- `OtlpExporter::shutdown().await` 冲刷缓冲并关闭（幂等）；
- 必须在 tokio runtime 关闭前调用（span 收尾依赖 tokio task）；顺序建议：
  `agent run 结束 → telemetry.flush() → exporter.shutdown() → runtime 退出`。
- exporter 不可达只返回 `ExportOutcome` 失败，不影响 agent；不要因为 flush 失败改变
  会话退出码。

## 三、建议：AgentRunCollector 接 TUI/评测汇总

### 位置

- `crates/kiana-session/src/agent_session.rs`：每次 run 创建 collector；
- 事件分发处（`Agent::run` 的 AgentEvent 消费者）或 CLI 的 run 结果处理。

### 接法

```rust
use std::sync::Arc;
use kiana_agent_core::{AgentRunCollector, ToolStatus};

let collector = Arc::new(AgentRunCollector::new());
collector.note_available_tools(tools.iter().map(|tool| tool.name.as_str()));

// 事件流（AgentEvent）直接喂：
collector.observe_event(&event);
// agent_end 后：
collector.mark_run_ended();
let snapshot = collector.snapshot();      // 状态栏即时值
let summary = collector.summary();        // 持久化/评测
let coverage = collector.coverage();      // 注册工具 vs 实际调用、model/provider
```

`observe_event` 已处理：

- `TurnStart` → `begin_chat`（step 自增；model/provider 在 `TurnEnd` 从 assistant
  消息补全）；
- `TurnEnd { message, .. }` → `end_chat`（usage/cost/stop reason 从 message 提取）；
- `ToolExecutionStart/End` → `begin_tool` / `end_tool_with_latency`（使用事件里的
  `duration_ms`，缺失时用时钟差）；
- `AgentEnd` → `mark_run_ended`。

事件表达不了的终态用直接 API 补：`ToolStatus::{Skipped, Blocked, Timeout, Aborted}`、
`record_orphan_tool`（被中断、从未产生工具事件）、`fail_chat`（流函数直接报错、
没有 assistant 消息）。不要对同一调用同时用事件和直接 API，否则会重复计数。

多 run 汇总用 `aggregate_agent_run_summaries(&[...])` 与
`aggregate_agent_run_coverage(&[...])`；`input_tokens` 已含 cache，
`cost_unavailable_reason` 不应显示成 0。

## 四、必须：W3C traceparent 传入 sandbox 子进程

### 1. 从上游取 trace 上下文

```rust
use kiana_telemetry::{TraceContext, TraceParent};

// 来自 HTTP headers（大小写不敏感）或环境变量
let parent: Option<TraceParent> = headers
    .get("traceparent")
    .and_then(|value| value.parse::<TraceParent>().ok());
// 或 TraceContext::from_env_values(std::env::var("TRACEPARENT").ok(), ...)

let child = parent.map(|parent| parent.child()); // 共享 trace id，新 span id
```

### 2. 传给 `kiana-sandbox::SpawnOptions`

`crates/kiana-sandbox/src/spawn.rs` 已新增字段（`SpawnOptions` 仍 `Default`）：

```rust
use kiana_sandbox::SpawnOptions;

let options = SpawnOptions {
    // ...原有字段...
    traceparent: child.as_ref().map(ToString::to_string),
    tracestate: tracestate,
    ..SpawnOptions::default()
};
```

或 builder：

```rust
let options = SpawnOptions::default()
    .with_traceparent(child.to_string())
    .with_tracestate(tracestate);
```

注入发生在 `env_clear()` 之后，所以即使 `env` 整体替换也不会丢；未设置时不写
`TRACEPARENT`/`TRACESTATE`。

### 3. 下游调用点（共享文件）

- `crates/kiana-tools/src/bash.rs` 约 701 行的 `SandboxSpawnOptions` 字面量目前把
  `traceparent: None, tracestate: None` 写死。接线时把当前 span 的 child traceparent
  填进去（或者把 `TRACEPARENT` 合并进 `spawn_options.env`）。注意 bash 的 `env`
  语义是整体替换，合并在 env 里时要保留原环境。
- `crates/kiana-mcp/src/transports/stdio.rs` 约 649 行已用
  `..SandboxSpawnOptions::default()`，当前注释声明 MCP 不注入 trace；如果 MCP 也要
  传播，填 `traceparent`/`tracestate`。
- **编译兼容**：`crates/kiana-evals/src/native.rs` 约 603 行的 `SpawnOptions` 字面量
  是穷尽字段写法，新增 `traceparent`/`tracestate` 后需要补
  `traceparent: None, tracestate: None` 或 `..SpawnOptions::default()`。这是共享文件
  改动，不在本波所有权内。

### 4. 子 span 的父子关系

用 `TraceParent::child()` 生成子 span 的 traceparent：trace id 不变、parent span id
换成新 span id、flags 继承。`TraceContext::inject` 输出小写键 `traceparent` /
`tracestate`；`to_env_pairs` 输出 `TRACEPARENT` / `TRACESTATE`。`tracestate` 只做
长度/条数校验（≤32 条、≤512 字符），没有供应商条目合并/删除语义。

## 五、需要修改的共享文件与 API 形状汇总

| 文件 | 需要的改动 | 依赖的本波 API |
| --- | --- | --- |
| `crates/kiana-session/src/sdk.rs` | `CreateAgentSessionOptions` 增加 `telemetry: Option<Arc<SessionTelemetry>>`、`run_collector: Option<Arc<AgentRunCollector>>`（或由 `create_agent_session` 内部构造）；OTLP exporter 在此创建 | `SessionTelemetry::{new, with_exporter}`、`OtlpExporter::from_env` |
| `crates/kiana-session/src/agent_session.rs` | 持有 `Arc<SessionTelemetry>` / `Arc<AgentRunCollector>`；run 开始/结束、turn、API、SSE、tool、compaction、hook、startup 调用埋点；转发 `AgentEvent` 给 collector；shutdown 前 flush | `SessionTelemetry::{begin_agent_run, begin_turn, record_turn, record_api_call, record_sse_event, record_tool_call, record_compaction, record_hook_run, record_startup_phase, set_content_attributes, flush}`、`AgentRunCollector::{observe_event, snapshot, summary, coverage}` |
| `crates/kiana-ai/src/providers/*.rs` / `src/api/*` | 请求前 `prepare_request`；payload 上 `apply_cache_breakpoints`；错误路径分类/重试；成功路径 `account_provider_usage` | `request_policy::*`（见第一节） |
| `crates/kiana-tools/src/bash.rs` | sandbox 选项填 `traceparent`/`tracestate`；环境合并时保留 TRACEPARENT | `kiana_sandbox::SpawnOptions` 字段/`with_traceparent` |
| `crates/kiana-mcp/src/transports/stdio.rs` | 可选：MCP 子进程传播 trace | 同上 |
| `crates/kiana-evals/src/native.rs` | `SpawnOptions` 字面量补新字段或 `..Default::default()`（编译必需） | `kiana_sandbox::SpawnOptions` |

不需要修改根 `Cargo.toml`：`kiana-telemetry` 已有 `reqwest` workspace 依赖，
`kiana-agent-core` 已依赖 `kiana-telemetry`，`kiana-ai` 无新依赖。

## 六、接线后的验收测试建议

1. **能力门零请求**：用现有 mock provider 记录请求数；对文本模型请求
   `image-generation`，断言 `AiErrorKind::Unsupported` 且请求数 0。
2. **预检零请求**：mock 模型 `context_window=100` + 超长 context，断言
   `ContextWindowExceeded` 且请求数 0。
3. **预算**：请求带 `max_tokens=None` 时，wire 上的 `max_tokens` 等于
   `RequestPlan.max_tokens`；显式 `Some(0)` 时 wire 上为 0。
4. **分类**：mock 返回 429 + context 文案 → 不触发压缩、按限流退避；返回 400 +
   `context_length_exceeded` → 触发压缩；401 → 不重试。
5. **usage**：响应 usage 与模型价格已知时，`cost_source=canonical`；价格全零时回退
   provider 上报。
6. **OTLP e2e**：启动本地 collector（HTTP JSON），跑一次 agent；断言 root
   `invoke_agent` + `chat` + `execute_tool` 的父子关系、`gen_ai.*` 属性、
   `/v1/traces` `/v1/logs` `/v1/metrics` 各自收到请求；exporter 不可达时 agent
   仍正常结束。
7. **内容捕获**：默认/`...=false` 时 payload 不含 prompt 文本；`...=true` 时
   `gen_ai.input.messages` 出现。测试后要恢复环境变量，避免并行测试串扰。
8. **sandbox trace**：设置 `traceparent` 后 `bash` 子进程的 `$TRACEPARENT` 等于传入值；
   未设置时为空；`env` 整体替换时仍保留。
9. **collector**：一次 run 产生 2 chat + 3 tool 后，`summary().chats.len()==2`、
   `summary().tools.by_status` 计数正确、`coverage().invoked_tools` 排序去重；
   `observe_event` 与直接 API 不混用。
