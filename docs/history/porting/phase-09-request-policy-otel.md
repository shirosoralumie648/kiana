# 第 9 周：Provider 请求策略 + OTel

日期：2026-11-07
对应 `docs/history/porting/plan.md` 第 9 周。

本波只修改了 `crates/kiana-ai/**`、`crates/kiana-telemetry/**`、
`crates/kiana-agent-core/**`、`crates/kiana-sandbox/src/**`（以及这些 crate 的测试）。
没有修改 `crates/kiana-session/**`、根 `Cargo.toml`、`Cargo.lock`、`docs/history/porting/plan.md`
或其它周的文件。所有接线点写在 `docs/history/porting/handoffs/phase-09.md`。

## 改了什么

### 1. `kiana-ai`：provider 能力矩阵（`src/request_policy/capabilities.rs`）

新增纯数据层，解决「manifest 声明了、但 adapter 没实现」和反过来的情况：

- `ModelOperation`：`language` / `image-generation` / `speech-generation` / `transcription` /
  `realtime` / `video-generation`，serde kebab-case；未知值反序列化失败（fail-closed）。
- `CapabilitySet { operations: BTreeSet<ModelOperation>, tools, json_output,
  parallel_tool_calls, vision }`：`text_only()`、`language_chat()`、
  `image_generation()`、`with_operation/with_tools/with_json_output/...`。
- `ProviderCapabilityManifest`：provider 级默认 + 按模型 id 覆盖；`capabilities_for()`
  合并（模型覆盖优先，未列出的模型继承 provider 声明）。
- `RequestRequirements`：一次请求实际需要的能力（operation + 四个布尔）。
- `CapabilityGate { model_id, manifest, adapter }`：`authorize(&requirements)` 同时检查
  **manifest 声明**与**adapter 实现**；任一侧缺失即返回 `UnsupportedCapability`，
  `missing` 逐项记录 `side`（`manifest` / `adapter`）与 `capability`
  （`operation:image-generation` / `tools` / ...）。
- `Display` 形如 ``model `x` does not support the request: missing
  manifest:operation:image-generation, adapter:operation:image-generation``；转成
  `AiErrorKind::Unsupported`，details 带结构化字段。
- 失败在发网络前发生：`CapabilityGate::authorize` 是纯函数，没有 IO 能力。

测试 5 个，覆盖 manifest/adapter 单侧缺失、两侧都缺、布尔能力、operation 枚举/覆盖。

### 2. `kiana-ai`：输出预算（`src/request_policy/budget.rs`）

实现 DoD 公式：

```text
available = context_window - estimated_prompt - 1024          （窗口已知且 > 0）
budget   = min(caller_max_tokens?, model.max_output?, available?, 32000)
floor    = min(floor(0.3 * model.max_output), available)       （model.max_output 存在）
max_tokens = max(budget, floor)
```

- 常量 `PRODUCT_DEFAULT_MAX_OUTPUT_TOKENS = 32_000`、`OUTPUT_RESERVE_TOKENS = 1_024`、
  `OUTPUT_FLOOR_FRACTION = 0.3`。
- `available < 1` → typed `OutputBudgetError::ContextOverflow`（保留负值）；
  `context_window` 为 `None`/`Some(0)` 视为未知，不做上下文裁剪。
- 显式 `Some(0)` 保留 0 语义：不参与 floor 抬升（0 是「不产出 token」，不是「未指定」）。
- `OutputBudget { max_tokens, min_budget, floor, available, capped_by, floor_applied }`；
  `capped_by` 按 caller → model → context → product_default 的并列优先级记录最小值来源。
- `output_budget_for_model(model, estimated, caller)`、`output_floor(...)` 便捷入口。

测试 9 个：公式各分支、显式 0、窗口未知、精确用尽窗口、floor 抬升与 `<= available`，
外加 2 万组伪随机输入的不变量循环（`max_tokens <= available` 且 `>= floor`）。

### 3. `kiana-ai`：错误分类与可重试（`src/request_policy/classification.rs`）

- `ProviderErrorClass`：`Auth` / `RateLimit` / `ContextWindow` / `Quota` /
  `InvalidRequest` / `Transient` / `Unknown`，`default_retryable()` 只对
  RateLimit/Transient 为真。
- `TypedProviderError<'a> { classification, status, code, message, is_retryable }`：
  typed 字段优先；`classify_provider_error_with_typed` 仅在 typed 无法判定时回落到 JSON。
- `collect_provider_error_signals(value)`：BFS/DFS 遍历 JSON 结构，最大深度 8
  （`MAX_WALK_DEPTH`），收集 status、code、message 三类信号；支持 `message` / `detail` /
  `error_message` / `error` / `errors` / `cause` / `responseBody` 等嵌套键。
- 判定优先级（`verdict_from_signals`）：
  1. 401/403 → Auth（只看状态码，避免 provider 文案误伤）；
  2. 配额/账单类（`insufficient_quota`、`billing`、`quota exceeded` 等）→ Quota
     （优先于瞬时故障，重试不会成功）；
  3. 429 或 rate-limit 文案 → RateLimit（在 context 判定之前，429 + "context length"
     不会被误判成上下文溢出）；
  4. 上下文码/文案，且状态在 {400, 413, 422} → ContextWindow；
  5. 408/409/425/429/5xx 或瞬时文案 → Transient；
  6. 其它 4xx → InvalidRequest；
  7. 仅剩文案信号 → Auth / InvalidRequest / Transient；
  8. 否则 Unknown。
- `is_retryable_provider_error(typed, value)`：typed `isRetryable` 有值时**直接采用**
  （false 覆盖状态码集合）；否则 status ∈ {408, 409, 425, 429, 500, 502, 503, 504, 529}
  （`RETRYABLE_STATUS_CODES`）；没有任何 status 时只认单条
  "Provider returned error" 消息兜底，避免纯文案误重试。
- `classify_typed` 支持 typed classification 直接短路。

测试 12 个：429/context 混淆、quota 优先、auth 状态式、depth 边界（第 8 层可读、第 9 层
不读）、typed 覆盖、可重试集合、无 status 文案兜底等。

### 4. `kiana-ai`：输入预检（`src/request_policy/precheck.rs`）

- `estimate_input_tokens(context)` 复用既有 `utils::estimate::estimate_context_tokens`
  （UTF-16 字符 / 3.5，图片固定 4800），不新造第二套口径。
- `precheck_input(context, model, reserve)`：窗口已知时
  `available = window - estimated - reserve`，`< 1` 返回 typed
  `ContextWindowExceeded { context_window, estimated_input_tokens, reserve_tokens,
  available }`；窗口未知恒成功。
- `precheck_input_with_defaults` 使用 1024 预留。
- 失败路径是纯函数返回，调用方在真正建立 HTTP 连接前拿到错误，因此零请求。
- 与 `utils::overflow`（响应后判定）互补：预检在请求前，分类在失败后。

测试 4 个，包含 exact-fit 边界与「失败时不 dispatch」断言。

### 5. `kiana-ai`：usage 记账（`src/request_policy/usage.rs`）

- `CostSource`：`Canonical` / `UserDeclared` / `ProviderReported`（snake_case）。
- 优先级固定 canonical → 用户声明 → provider-reported：
  `resolve_usage(canonical, user_declared, provider_reported)`。
- `canonical_usage(model, usage)` 仅在模型目录存在有限非零费率时调用
  `models::cost::calculate_cost`；全零价格视为「价格未知」，不把未知成本记成 0。
- `account_provider_usage(model, provider_reported, user_declared)` 是 provider 成功路径的
  入口，总返回一份 `AccountedUsage` 并带 `cost_source`。

测试 5 个：三种来源、价格未知回退、canonical 优先。

### 6. `kiana-ai`：prompt cache 语义（`src/request_policy/cache.rs`）

- `CacheSemantics` tagged enum：
  - `ExplicitBreakpoints { max_breakpoints }`（Anthropic 方言，4 个）；
  - `ImplicitStrict`（只复用逐字节一致前缀）；
  - `ImplicitTolerant`（最长匹配前缀）；
  - `Uncached`。
- `for_model(provider, model)` 映射：anthropic/minimax/zai/kimi-coding → Explicit 4；
  Bedrock/Vertex/Databricks 上的 Claude → Explicit 4；OpenRouter/LiteLLM 的
  `anthropic/` 模型 → Explicit 4；OpenAI/Azure/Copilot 的 Responses 系模型 →
  ImplicitStrict，其余 Chat Completions → ImplicitTolerant；
  moonshot/deepseek/groq/together/fireworks/mistral/xai/zhipu/alibaba/qwen →
  ImplicitTolerant；snowflake/sagemaker → Uncached；未知组合默认 ImplicitStrict
  （安全默认）。
- `apply_cache_breakpoints(payload, semantics)`：仅显式语义注入 `cache_control`；放置
  尾块、约 20 个 block 的回看锚点、system 块、最后一个 tool；跳过空文本/空 tool
  result/thinking 块；`max_breakpoints` 为 0 表示不限；返回注入数。
- `cache_warmer_eligible(model, semantics)` 要求语义有缓存且模型 `promptCache` 声明了
  非零 TTL；`prompt_cache_lifetime_seconds`、`has_known_prompt_cache_lifetime`。

测试 9 个：映射表、显式注入/幂等、零上限、回看锚点、warmer 资格。

### 7. `kiana-ai`：声明式 provider（`src/request_policy/declarative.rs`）

- `DeclarativeProvider` / `DeclarativeModel` 用 `#[serde(flatten)] extra: IndexMap`
  保留未知字段，重新序列化原样往返（未来版本字段不丢）。
- 加载期校验：provider id 字符集、`baseUrl` 必须 http(s)、models 必须是数组、
  模型 id 非空/字符集/唯一、token 预算 > 0、能力字段类型。
- `load_declarative_provider(value)`：provider 级错误返回
  `DeclarativeProviderError`（`NotAnObject` / `InvalidProviderId` / `InvalidBaseUrl` /
  `InvalidModels` / `NoUsableModels`）；单条坏模型只跳过该条并在
  `DeclarativeLoadResult.diagnostics` 里记录 `DeclarativeDiagnostic`
  （`invalid_model_id` / `duplicate_model_id` / `invalid_model_entry` /
  `invalid_capability` / `invalid_token_budget`），不连坐其它模型。

测试 4 个：未知字段往返、坏条目跳过且其余可用、重复 id、provider 级错误。

### 8. `kiana-ai`：策略组合入口（`src/request_policy/policy.rs`）

`prepare_request(&RequestPolicyInput)` 固定顺序：

```text
能力门（manifest × adapter） → 输入预检 → 输出预算 → cache 语义/资格
```

- 成功返回 `RequestPlan { max_tokens, precheck, cache, cache_warmer_eligible }`。
- 失败返回 `RequestPolicyError::{Unsupported, ContextWindow, Budget}`，
  `policy_ai_error` / `error_kind` 映射到 `AiErrorKind::{Unsupported, Request}` 等。
- 纯函数、无 IO：任何失败路径都不会发请求。
- 模块在 `crates/kiana-ai/src/lib.rs` 以 `pub mod request_policy;` 暴露。

测试 5 个：顺序（能力门先于预检）、超窗先于预算、请求计划字段。

### 9. `kiana-telemetry`：OTLP 导出

新增模块（`src/otel/`）：

- `types.rs`：OTLP/HTTP JSON 类型（proto3 JSON 映射）：camelCase、int64 用字符串、
  枚举常量（`SPAN_KIND_SERVER` 等）、externally tagged `AnyValue`。覆盖
  `ExportTraceServiceRequest` / `ExportLogsServiceRequest` /
  `ExportMetricsServiceRequest` 及 Span/Event/Status/Resource/Scope、LogRecord、
  Gauge/Sum/Histogram/NumberDataPoint/HistogramDataPoint。
- `convert.rs`：内存 `StoredSpan` / `RecordedTelemetryMeasurement` → OTLP 结构。
  文本 trace/span id 用 FNV-1a 确定性派生为 16/8 字节十六进制（合法 hex 原样保留、
  全零视为缺失）；毫秒按整毫秒 + 小数部分拆成 int64 纳秒，避免 f64 精度丢失；
  属性转换（整数 Number → IntValue、非有限数 → String）；资源带 `service.name` 与
  `telemetry.sdk.*`；metrics 按名字聚合（Counter/UpDownCounter → delta Sum，
  Histogram → delta Histogram 带 bucket counts）。
- `proto.rs`：手写 protobuf wire 编码（varint / length-delimited / fixed64 / packed），
  覆盖 Trace/Logs/Metrics 请求、AnyValue/KeyValue/Resource/Scope/Span/Event/Status/
  LogRecord/NumberDataPoint/HistogramDataPoint/Metric；`grpc_frame(payload)` 加 5 字节
  gRPC 前缀。
- `exporter.rs`：`OtlpProtocol`（`grpc` / `http/json` / `http/protobuf`）、
  `OtlpConfig`（endpoint、protocol、timeout_ms、headers、service_name；
  `from_env` 读取标准 `OTEL_EXPORTER_OTLP_*`）、带缓冲的 `OtlpExporter`：
  `enqueue_spans/logs/metrics`、`export_spans/logs/metrics`、
  `export_spans_best_effort`、`flush`、幂等 `shutdown`。
  HTTP JSON/Protobuf 走 `POST /v1/{traces,logs,metrics}`；gRPC 用 reqwest
  `http2_prior_knowledge()` 走 `/<service>/Export`。导出失败只体现在
  `ExportOutcome`/`OtlpError`，不 panic、不影响 agent。

新增 `src/w3c.rs`：W3C Trace Context v00：

- `TraceId`(16B) / `SpanId`(8B) 解析、格式化、随机生成；只接受小写 hex，全零非法。
- `TraceParent { version, trace_id, parent_span_id, flags }`：解析/格式化/`child()`
  （子 span 共享 trace id、生成新 span id）；`TraceContext` 的
  `extract(headers, value)` / `inject` / `to_env_pairs` / `from_env_values`；
  `TRACEPARENT` / `TRACESTATE` 常量；`validate_tracestate`（≤32 条、≤512 字符）。

新增 `src/capture.rs`：`ContentCapture` 只在
`OTEL_INSTRUMENTATION_GENAI_CAPTURE_MESSAGE_CONTENT=true`（大小写不敏感、trim）时
启用；`maybe` / `maybe_string` 在关闭时返回 `None`，调用方不需要到处写 if。

新增 `src/names.rs`：指标名常量（`kiana.session.turns`、`kiana.turn.ttft_ms`、
`kiana.turn.ttfm_ms`、`kiana.api.call.duration_ms`、`kiana.api.call.tokens`、
`kiana.api.call.cost_usd`、`kiana.api.sse.events`、`kiana.tool.call.duration_ms`、
`kiana.tool.calls`、`kiana.compaction.duration_ms`、`kiana.compactions`、
`kiana.hook.run.duration_ms`、`kiana.hook.runs`、`kiana.startup.phase.duration_ms`）与
`tool_kind()` 三分类（builtin/mcp/other），保证指标标签低基数、不含工具名。

### 10. `kiana-agent-core`：会话埋点与 run 汇总

`src/session_telemetry.rs`：

- `SessionTelemetry`：默认内存 context + 环境变量内容捕获；可
  `with_context` / `with_capture` / `with_exporter`。
- 显式 span 句柄 `SessionSpan`（`start_child` / `finish` / `finish_with_error`，
  `Drop` 也发收尾信号但不等待），底层用 oneshot 启停通道 + tokio task，跨 `.await`
  持有不阻塞主流程。
- 类型化 API：`begin_agent_run`、`begin_turn`、`record_turn`（`TurnTracker` 的
  TTFT/TTFM）、`record_api_call`（延迟/token/cost/stop reason/error）、
  `record_sse_event`、`record_tool_call`、`record_compaction`、`record_hook_run`、
  `record_startup_phase`、`set_content_attributes`（仅捕获开启时写入）、
  `recorded_spans`、`flush`。
- 指标标签只放低基数/脱敏值：工具名只在 span 属性 `gen_ai.tool.name`，指标用
  `tool.kind`；hook 指标只放 hook event；关闭捕获时 prompt/tool 参数不写入任何 span。
- `flush()` 把内存记录交给可选 `OtlpExporter`；无 exporter 时 no-op；失败只返回
  `ExportOutcome`。

`src/run_collector.rs`：

- `AgentRunCollector` 内部 `Mutex` 状态 + 可注入 `RunClock`（`SystemRunClock` 默认）。
- 显式 API：`begin_chat/end_chat/fail_chat`、`begin_tool/end_tool/
  end_tool_with_latency`、`record_orphan_tool`、`note_available_tools`、
  `mark_run_ended`。
- `observe_event(&AgentEvent)`：`turn_start` 开 chat、`turn_end` 收 chat、
  `tool_execution_start/end` 收工具、`agent_end` 标记结束。
- 汇总值：`AgentRunSummary`（chat 数/stop reason 分布/延迟、工具状态计数与按名计数、
  token 分解、cost/cost_unavailable_reason、错误分类、step 数）与
  `AgentRunCoverage`（注册工具 vs 实际调用、使用过的 model/provider，排序去重）；
  `snapshot`/`summary`/`coverage` 均为纯值，之后继续记录不影响已返回值。
- 不变量：`input_tokens` 含 cache（`input + cache_read + cache_write`）；`by_*` 键排序；
  `end_chat` 延迟优先 provider `duration_ms`，否则时钟差；cost 未知时记录原因而不是 0。
- `aggregate_agent_run_summaries` / `aggregate_agent_run_coverage` 支持多 run 聚合。

### 11. `kiana-sandbox`：子进程 TRACEPARENT 注入

`src/spawn.rs` 的 `SpawnOptions` 新增 `traceparent` / `tracestate`（`Option<String>`）
与 `with_traceparent` / `with_tracestate`。在 `env_clear()` **之后**注入 `TRACEPARENT`
/ `TRACESTATE`，避免被清空；未设置时不注入。`tests/traceparent.rs` 3 个用例覆盖：
`env_clear` 后仍可见、未请求时不存在、转发 spawn（`child_mut().wait()`）同样收到。

## 参考项目对照表

所有下列参考只用于理解行为/字段形状；Kiana 代码是重写的，没有复制 GPL 代码。

| 参考文件 | 借鉴的设计点 | Kiana 落点 |
| --- | --- | --- |
| `reference_project/cline/sdk/packages/llms/src/providers/gateway.ts` | `DEFAULT_GATEWAY_MAX_OUTPUT_TOKENS=32000`、`DEFAULT_GATEWAY_MAX_OUTPUT_FRACTION=0.3`、`GATEWAY_OUTPUT_RESERVE_TOKENS=1024` 与 min/floor 公式 | `crates/kiana-ai/src/request_policy/budget.rs::compute_output_budget` |
| `reference_project/cline/sdk/packages/llms/src/providers/error-classification.ts` | typed 优先、depth 8 结构遍历、context 码/文案、429 对 context 的 veto | `crates/kiana-ai/src/request_policy/classification.rs::collect_provider_error_signals` / `verdict_from_signals` |
| `reference_project/opencode/packages/ai/src/provider-error.ts` | 上下文/配额/限流的分类边界 | `classification.rs::ProviderErrorClass` 判定顺序 |
| `reference_project/qwen-code/packages/core/src/utils/retry.ts`、`retryErrorClassification.ts` | 可重试状态集合、429/529 语义、无 status 时的保守兜底 | `classification.rs::RETRYABLE_STATUS_CODES` / `is_retryable_provider_error` |
| `reference_project/goose/crates/goose-provider-types/src/cache_semantics.rs` | `CacheSemantics` 枚举、`for_model` 映射、断点放置与 20 block 回看 | `crates/kiana-ai/src/request_policy/cache.rs` |
| `reference_project/cline/sdk/packages/core/src/services/llms/provider-defaults.ts` | provider manifest 的 `capabilities` / model `contextWindow` / `maxTokens` 形状 | `capabilities.rs::ProviderCapabilityManifest` / `CapabilitySet` |
| `reference_project/qwen-code/packages/core/src/core/openaiResponsesContentGenerator/prompt-caching.ts` | 隐式（严格/容错）vs 显式断点语义 | `cache.rs::CacheSemantics` |
| `reference_project/oh-my-pi/packages/agent/src/telemetry.ts` | 内容捕获环境变量、session span/metric 组织 | `crates/kiana-telemetry/src/capture.rs`、`kiana-agent-core/src/session_telemetry.rs` |
| `reference_project/goose/crates/goose/src/agents/gen_ai_telemetry.rs` | `gen_ai.*` 属性、内容捕获开关 | `session_telemetry.rs` 的 span 属性与 `capture.rs` |
| `reference_project/qwen-code/packages/core/src/telemetry/metrics.ts`、`session-tracing.ts`、`gen-ai-request.ts` | 会话指标名、turn/API/tool span 形状、TTFT/TTFM | `kiana-telemetry/src/names.rs`、`session_telemetry.rs` |
| `reference_project/qwen-code/packages/core/src/telemetry/sdk-exporters-http.ts`、`sdk-exporters-grpc.ts`、`otlp-urls.ts` | OTLP HTTP/gRPC 协议选择、`/v1/{traces,logs,metrics}` 路径与标准环境变量 | `kiana-telemetry/src/otel/exporter.rs` |
| `reference_project/qwen-code/packages/core/src/telemetry/sanitize.ts` | 指标标签脱敏/低基数 | `kiana-telemetry/src/names.rs::tool_kind` |
| `reference_project/qwen-code/packages/core/src/telemetry/sdk-impl.ts`、`daemon-tracing.ts` | W3C `traceparent` 传播到子 span/子进程 | `kiana-telemetry/src/w3c.rs`、`kiana-sandbox/src/spawn.rs` |
| `reference_project/oh-my-pi/packages/agent/src/run-collector.ts` | Agent run 汇总字段、inputTokens 含 cache、coverage 排序去重 | `kiana-agent-core/src/run_collector.rs` |

## 测试命令与真实结果

全部使用 `CARGO_TARGET_DIR=/data/cargo-target-sandbox`。

```text
$ cargo test -p kiana-ai
cargo test: 2102 passed, 341 ignored (203 suites, 74.53s)

$ cargo test -p kiana-telemetry
cargo test: 154 passed (17 suites, 0.67s)

$ cargo test -p kiana-agent-core
cargo test: 238 passed (9 suites, 5.80s)

$ cargo test -p kiana-sandbox
cargo test: 59 passed (5 suites, 0.28s)

$ cargo test -p kiana-ai -p kiana-telemetry -p kiana-agent-core -p kiana-sandbox
cargo test: 2553 passed, 341 ignored (234 suites, 80.17s)
```

`kiana-ai` 的 341 个 ignored 是既有测试（真实 provider/golden 类），本波没有新增
`#[ignore]`。上述命令是在 `cargo fmt -p kiana-ai -p kiana-telemetry -p kiana-agent-core
-p kiana-sandbox` 之后执行的最终版本。

静态检查：

```text
$ cargo check -p kiana-ai -p kiana-telemetry -p kiana-agent-core -p kiana-sandbox --all-targets
cargo check (4 crates compiled)
Finished `dev` profile [unoptimized + debuginfo] target(s) in 31.22s
```

本波新增测试共 108 个：

- `kiana-ai` request_policy 单元 53 个 + 集成 `tests/request_policy.rs` 6 个；
- `kiana-telemetry` 单元 28 个（w3c 8、capture 2、names 2、otel 16）+
  `tests/otlp_export.rs` 5 个；
- `kiana-agent-core` run_collector 6 + session_telemetry 7；
- `kiana-sandbox` tests/traceparent.rs 3 个。

关键行为测试：

- 能力门：manifest/adapter 任一侧缺 `image-generation` 都在 dispatch 前失败，错误文本
  同时点名两侧；集成测试用 `dispatch_count` 断言 0 次请求。
- 预检：70 万字符 context 对 200k 窗口返回 `ContextWindow`，dispatch 计数为 0。
- 预算：5000 组伪随机输入验证 `max_tokens <= available`、`>= floor`、显式 0 保持 0、
  只有 `available < 1` 才返回错误。
- 分类：429 + "context length" → `RateLimit` 且可重试；401 → `Auth` 不重试；
  400 + malformed tool schema → `InvalidRequest`；529/503 可重试；typed
  `isRetryable=false` 覆盖状态码。
- usage：canonical → user_declared → provider_reported 的 `cost_source` 断言。
- cache：显式断点注入产生 `cache_control`，ImplicitTolerant 不改 payload。
- OTLP e2e：本地 TCP collector 收到 root + chat + tool 三个 span，父子关系/trace id
  正确、`gen_ai.*` 属性存在、内容捕获关闭时 prompt 文本不出现在载荷里；logs/metrics
  分别走 `/v1/logs`、`/v1/metrics`；`shutdown()` 冲刷缓冲；不可达 exporter 不阻塞
  agent 路径。
- sandbox：`TRACEPARENT` 在 `env_clear` 后仍注入，未请求时不出现。

## 未完成项（gaps）

- **provider 管线未接线**：`request_policy` 是独立纯函数层。`kiana-ai` 现有 provider
  适配器/agent loop 尚未调用 `prepare_request`、`precheck_input`、
  `classify_provider_error`、`account_provider_usage`。接线步骤见 handoff。
- **gRPC e2e 未覆盖**：手写 protobuf 与 5 字节 gRPC 帧有单元测试（tag/长度/frame 头），
  HTTP/JSON 有本地 collector 端到端测试；本地没有 HTTP/2 collector，gRPC 传输路径只做
  了编译/构造级验证。接真实 OTLP collector 时应补一条 e2e。
- **OTLP 编码是子集**：只覆盖本仓库会产生的 span/log/metric 类型；不支持 exemplar、
  exponential histogram、Summary、部分 AnyValue 类型（bytes/array/kvlist 仅保留基本
  编码）。未知指标类型按 gauge/string 退化，不做完整 proto 兼容。
- **内容捕获只有构造期开关**：`ContentCapture::from_env()` 在 `SessionTelemetry::new`
  时读取；运行期切换环境变量不会生效。也没有按 span 的采样/白名单策略。
- **tracestate 只验证不修改**：`validate_tracestate` 做长度/条数校验，没有实现
  tracestate 的供应商条目合并/删除语义。
- **sandbox 只注入环境变量**：`kiana-tools` 若自行重建 Command 或再次 `env_clear`，
  需要它主动把 `SpawnOptions.traceparent/tracestate` 转进去；本波只保证 sandbox 层。
- **cache warmer 只是资格判断**：`cache_warmer_eligible` 没有调度器；实际预热需要上层
  定时器与 provider 适配器配合。
- **声明式 provider 只做加载校验**：没有把 `DeclarativeProvider` 转成运行期 provider
  注册表；模型/能力合并仍由调用方完成。
- **run collector 的 step 计数**：`observe_event` 按 turn 自增；直接 API
  `begin_chat(step_number, ...)` 的调用方需自己传一致的 step。

## 风险

- 错误分类的文案正则是启发式：provider 新文案可能落到 `Unknown`/`InvalidRequest`。
  判定顺序已尽量保守（quota/auth 优先、429 veto context、无 status 不重试），但仍需
  真实错误样本校准；不要把 `classify_provider_error` 单独用于「压缩」决策，应结合
  响应后的 `utils::overflow`。
- 输入估算沿用字符启发式（UTF-16 / 3.5 + 图片 4800），对中文/代码/图片混排可能与
  provider 计数偏差 10%+；预检只用于明显超窗的早失败，最终仍以 provider usage 为准。
- 预算的 floor 在 `model_max_output` 很小时不会把产品默认 32k 强加给模型（这是设计
  目标），因此小模型的实际输出上限可能远低于 32k；调用方若期望固定 32k，需要显式
  传 `caller_max_tokens`。
- `ExplicitBreakpoints` 的 `apply_cache_breakpoints` 直接改 JSON payload：如果适配器
  随后又重组消息，断点可能丢失；接线时应在最终 payload 上调用一次。
- OTLP 导出的 id 派生（FNV-1a）不是密码学安全的，只用于把内存文本 id 映射为合法
  hex；跨进程的 trace 关联应以 `w3c::TraceContext` 为准，而不是这两个派生 id。
- `SessionTelemetry` 的 span 收尾依赖 tokio task；在 runtime 关闭（shutdown 期间）创建
  span 可能失败，`flush()` 应先于 runtime 退出调用。
- `AgentRunSummary` 的 cost 以 USD 计且依赖模型目录价格；价格未知时 `cost_usd=None`
  并带 `cost_unavailable_reason`，UI 不应显示 0。
