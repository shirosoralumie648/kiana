# telemetry + mcp 单元移植规格（TypeScript → Rust）

本文件是 `@earendil-works/kiana-telemetry` 与 `@earendil-works/kiana-mcp` 两个包的完整移植规格（单元 id：`telemetry-mcp`）。
源仓库 `packages/telemetry`、`packages/mcp` 为**只读参考**；Rust 实现位于 `/home/ctyun/code/kiana_rev/kiana`。

- 源文件范围：`/home/ctyun/code/kiana_rev/kiana/packages/telemetry/**`（src、test、README、CHANGELOG）与
  `/home/ctyun/code/kiana_rev/kiana/packages/mcp/**`（src、test、README、CHANGELOG、LICENSES）。
- 本文件同时是 (a) 逐条可测的 Rust 移植验收清单，(b) Rust 侧设计说明。所有内容来自对范围内每个文件的完整阅读。
- 约定：`verbatim`（逐字）表示字符串必须逐字节一致；`name` 表示错误对象的 `name` 属性；所有算法用伪代码给出，
  行内引用的 TS 标识符保持原名。

两个包都**没有** CLI、TUI、斜杠命令、键位、ANSI 输出、进度条或自有磁盘文件——它们都是库。其可观察表面是：

1. TypeScript API 形状与运行时语义；
2. 精确的错误类与错误消息；
3. `renderTrace()` 与 `toLlmContent()` 产生的精确字符串；
4. stdio / Streamable HTTP / SSE 上的 MCP 线协议；
5. 回环 OAuth 回调 HTTP 服务器及其面向浏览器的纯文本页面；
6. 嵌入方持久化的 OAuth 状态 JSON（coding-agent 写入 `mcp-auth.json`；文件由 coding-agent 单元负责，
   但其 schema 就是本单元定义的 `McpOAuthState` 类型）。

## 目录

1. 概览
2. `packages/telemetry/src/index.ts` — 契约与 schema 类型
3. `packages/telemetry/src/memory.ts` — `InMemoryTelemetryContext`
4. `packages/telemetry/src/noop.ts`
5. `packages/telemetry/src/metrics.ts`
6. `packages/telemetry/src/render.ts` — `renderTrace`
7. `packages/telemetry/src/testing/*` — 适配器一致性套件
8. `packages/telemetry/test/store-conformance.ts`（共享测试，非包导出）
9. `packages/mcp/src/protocol/jsonrpc.ts`
10. `packages/mcp/src/protocol/types.ts`
11. `packages/mcp/src/protocol/content.ts`
12. `packages/mcp/src/auth-provider.ts`
13. `packages/mcp/src/client.ts` — `McpClient`
14. `packages/mcp/src/transports/transport.ts`
15. `packages/mcp/src/transports/stdio.ts` — `StdioTransport`
16. `packages/mcp/src/transports/in-memory.ts` + `src/testing/index.ts`
17. `packages/mcp/src/transports/streamable-http.ts` — SSE + `StreamableHttpTransport`
18. `packages/mcp/src/oauth/*`
19. 跨单元接口（精确类型）与导出清单
20. 外部格式
21. npm 依赖 → Rust crate 映射
22. 必须桥接的 JS 语义
23. 测试清单
24. 建议的 Rust 模块布局
25. 最难逐字复现的部分
26. 验收清单
27. 错误与用户可见字符串总表（逐字核对用）
28. 未决问题
29. 版本相关行为来源（CHANGELOG 中的契约决定）

---

## 1. 概览

### 1.1 `@earendil-works/kiana-telemetry`（版本 1.4.0）

厂商无关的遥测契约。没有 exporter，没有全局 current-span 状态，没有后端依赖，根入口不含仅 Node 可用的 API
（`/testing` 子路径确实使用 `node:assert`）。提供：

- 回调作用域的 `TelemetryContext` / `TelemetrySpan` 契约；
- 共享的冻结 no-op 上下文（`NOOP_TELEMETRY_CONTEXT`）与 no-op metrics/propagation；
- 可选适配器能力：`metrics`（`TelemetryMetrics`）与 `propagation`（`TelemetryPropagation`）；
- 参考实现 `InMemoryTelemetryContext`，同时实现 `TelemetryPropagation`、`TelemetryMetrics`、`TelemetryStore`；
- 与后端无关的读契约 `TelemetryStore`（`findTraces`、`getTrace`、`findMetrics`）；
- 纯文本渲染器 `renderTrace`（`@earendil-works/kiana-telemetry/render` 子路径）；
- 仅类型层面有效的 schema 工具（`defineTelemetrySchema`、`createTypedSpanStarter`、`createTypedMetrics`），
  运行时行为极其简单（恒等 / Map 查表）；
- 运行器无关的适配器一致性套件（`/testing`）。

入口（package.json `exports`）：

| 子路径 | 源文件 | 用途 |
|---|---|---|
| `.` | `src/index.ts` | 契约、no-op、内存适配器、schema 辅助、指标辅助 |
| `./render` | `src/render.ts` | `renderTrace`（不进入根依赖图） |
| `./testing` | `src/testing/index.ts` | `createTelemetryAdapterConformance` + fixture 类型 |

仓库内消费者：`@earendil-works/kiana-ai`（请求选项中的 `TelemetrySpan`、`AI_TELEMETRY_SCHEMA`、
`createTypedMetrics`、span starter）、`@earendil-works/kiana-durable`（`DURABLE_TELEMETRY_SCHEMA`、span starter、
metrics、持久化类型中的 `TelemetryCarrier`）、`@earendil-works/chord`（`Context` 中的 `TelemetrySpan`；
`SqliteTelemetry` 实现 `TelemetryStore`）、`@earendil-works/kiana-agent-core`、`@earendil-works/kiana-coding-agent`（传递依赖）。
`chord/test/telemetry-sqlite-store.test.ts` 直接从 `packages/telemetry/test/store-conformance.ts` 导入
`telemetryStoreConformance`（源码级测试复用，不是包导出）。

其他构建元数据：`type: "module"`，`main` 为 `./dist/index.js`，`exports` 的 `.` 只有 `types`/`import`（无 `source`）；
`./render` 带 `source` 指向 `src/render.ts`，而 `./testing` **没有** `source`（只有 `types`/`import`，指向
`dist/testing/index.js`）——两包中只有 mcp 的三个子路径全部带 `source`。`files` 为 `dist` 与 `README.md`
（**不含** CHANGELOG 与 LICENSES，与 mcp 不同）。telemetry 包**没有** `vitest.config.ts`（mcp 有，见审计补充）。
`clean` 脚本调用 `shx`，但 `shx` 不在 telemetry 的 devDependencies 里（由工作区根 devDependencies 提供；
mcp 则显式声明 `shx@0.3.4`）。`license: MIT`，
author `Mario Zechner`。`engines.node >= 22.19.0`。devDependencies：`@types/node@22.19.19`、`vitest@4.1.12`。
`scripts`：`clean`（`shx rm -rf dist`）、`build`（`tsc -p tsconfig.build.json`）、`test`（`vitest --run`）、
`prepublishOnly`。

### 1.2 `@earendil-works/kiana-mcp`（版本 1.4.0）

独立的 Model Context Protocol 客户端。运行时**不**依赖官方 MCP SDK，也不依赖其他 kiana 包。唯一的运行时依赖是
`cross-spawn@7.0.6`（`StdioTransport` 用它启动子进程，使 Windows 的 `.cmd` shim 可用）。使用的 Node 内置模块：
`node:child_process`（spawn）、`node:process`、`node:http`（OAuth 回调服务器）、`Buffer`、`crypto.webcrypto`
（`crypto.getRandomValues`、`crypto.subtle.digest`）、`TextDecoder`、Web 标准的 `fetch` / `Headers` / `Response` /
`ReadableStream`。

入口：

| 子路径 | 源文件 | 用途 |
|---|---|---|
| `.` | `src/index.ts` | client、协议类型、内容转换、stdio 与 streamable-HTTP transport、transport 监听器类型 |
| `./oauth` | `src/oauth/index.ts` | MCP OAuth 子集：discovery、PKCE 流程、DCR、令牌刷新、`McpOAuthProvider`、回环回调服务器 |
| `./testing` | `src/testing/index.ts` | `createInMemoryTransportPair`、`InMemoryTransport` |

`sideEffects: false`；`main` 为 `./dist/index.js`；`exports` 三个子路径都带 `source`/`types`/`import`；
`files` 为 `dist`、`README.md`、`CHANGELOG.md`、`LICENSES`；`license: MIT`；author `Earendil Works`；
`engines.node >= 22.19.0`；dependencies `cross-spawn@7.0.6`；devDependencies `@types/cross-spawn@6.0.6`、
`shx@0.3.4`、`vitest@4.1.12`。许可备注：`src/oauth/{flow,discovery}.ts` 与 `src/oauth/types.ts` 改编自 MIT 许可的
Model Context Protocol TypeScript SDK v1.29.0（版权 Anthropic, PBC），原文见 `packages/mcp/LICENSES/`。
`packages/mcp/LICENSES/modelcontextprotocol-typescript-sdk.txt` 是标准 MIT 文本（Copyright (c) 2024 Anthropic, PBC）。
Rust 移植时在对应源文件或 crate 的 NOTICE 中保留同样的 MIT 归属声明。

支持范围（README 原文要点）：

- MCP 协议版本 `2025-11-25`，接受服务器协商到 `2025-06-18`、`2025-03-26` 或 `2024-11-05`；
- 初始化与 `notifications/initialized`；
- ping；
- 分页 `tools/list`；
- `tools/call`，含 structured content；
- 进度通知与超时续期；
- 请求取消；
- Streamable HTTP 会话、服务端到客户端的 GET 流及其重连、以及用 `Last-Event-ID` 恢复中断的响应流；
- 按规范关闭 stdio（关闭 stdin，然后 SIGTERM，然后 SIGKILL），作用于服务器整个进程组；
- 服务端 `ping` 与 `roots/list` 请求；
- 通过通用通知 API 暴露 logging 与 tool-list-change 通知；
- OAuth 受保护资源与授权服务器 discovery；
- PKCE 授权码流程、动态客户端注册、令牌刷新（并发 401 共享一次刷新）、以及 `insufficient_scope` 的升级授权。

明确不支持：批量 JSON-RPC、旧版 HTTP+SSE、服务端实现、sampling、tasks。

消费者：`kiana-coding-agent`（`src/extensions/mcp/{runtime,tools,resources,oauth,config,index,cli}.ts`、测试、
conformance runner），以及 agent 包内的示例 `packages/agent/examples/mcp-codemode`（路径以
`packages/agent/README.md` 与 `packages/mcp/README.md` 的链接为准；仓库根**没有**顶层 `examples/` 目录）。
coding-agent 的实际直接导入点只有 4 个文件：`src/extensions/mcp/{runtime,tools,resources,oauth}.ts`；`index.ts`
本身不 import 本包，而是通过 `runtime.lazy.ts` 动态加载 `runtime.ts`（因此 lazy 边界之外的工具/资源路径
`tools.ts`/`resources.ts` 仍是静态导入）。`config.ts`/`cli.ts`/`ui.ts`/`log.ts` 不导入本包。

---

## 2. `packages/telemetry/src/index.ts` — 契约与 schema 类型

本文件的运行时代码只有 `defineTelemetrySchema`、`createTypedSpanStarter`（及私有 `bindTypedSpanStarter`），
其余是类型与再导出。

### 2.1 值类型

```ts
type AttributeValue = string | number | boolean | readonly string[] | readonly number[] | readonly boolean[];
interface SpanAttributes { [name: string]: AttributeValue | undefined }
type TelemetryCarrier = Record<string, string>;
```

- `SpanAttributes` 的值可以是 `undefined`；所有记录路径在拷贝时丢弃 `undefined`。
- 数组按浅拷贝处理（元素为原始值）。类型上不允许嵌套数组。
- `TelemetryCarrier` 是不透明的、JSON 安全的字符串 map；同一 key 不可能重复。

```ts
interface TelemetryPropagation {
  inject(context: TelemetryContext): TelemetryCarrier | undefined;
  extract(carrier: TelemetryCarrier | undefined): TelemetryContext;
}
interface SpanLink { context: TelemetryContext; attributes?: SpanAttributes }
interface SpanOptions { name: string; attributes?: SpanAttributes; links?: readonly SpanLink[] }
type SpanStatus = { status: "ok" } | { status: "error"; error?: { name: string; message: string } };
```

两个 propagation 方法都是**同步**的。未知/缺失的 carrier 回退到适配器根上下文。`inject` 对根、no-op 上下文、
不支持的上下文返回 `undefined`。

### 2.2 已结算 span / trace 类型

```ts
interface StoredSpan {
  readonly id: string;            // 适配器拥有的全局唯一 span id
  readonly traceId: string;
  readonly parentId?: string;     // 根或父级属于另一个 recorder 时缺省
  readonly name: string;
  readonly startedMs: number;     // 纪元毫秒，墙钟
  readonly endedMs: number;       // 纪元毫秒（内存实现：startedMs + 单调时钟流逝）
  readonly attributes: SpanAttributes;
  readonly events: readonly { readonly name: string; readonly timeMs: number; readonly attributes: SpanAttributes }[];
  readonly links: readonly { readonly traceId: string; readonly spanId: string; readonly attributes?: SpanAttributes }[];
  readonly status: SpanStatus;
}
interface StoredTrace { readonly traceId: string; readonly spans: readonly StoredSpan[] }
```

- `parentId` 缺省时应**不出现该键**（不是 `undefined` 值）→ JSON 无此键。Rust：`Option<String>` +
  `skip_serializing_if = "Option::is_none"`。
- `links[].attributes` 缺省时省略；内存适配器里总是存在（`copyAttributes(undefined)` → `{}`），见 §3.2。
- `events[].timeMs` 相对于 span 起点，可能是小数（performance.now 差值）。

### 2.3 查询类型

```ts
interface TraceQuery {
  readonly name?: string;                    // 最早的已存储 span 的名字；时间戳并列时由 span ID 决定
  readonly attributes?: Readonly<Record<string, string | number | boolean>>;  // 全部等值必须命中同一个 span
  readonly errors?: true;                    // trace 中任一 span 状态为 "error"
  readonly startMin?: number;                // 对最早 span 起点的闭区间
  readonly startMax?: number;
  readonly limit?: number;                   // 正数页大小，默认 20
  readonly cursor?: string;                  // 上一页的不透明续页标记；必须保留相同过滤条件
}
interface MetricQuery {
  readonly name: string;
  readonly startMin?: number;                // 对区间起点时间的闭区间
  readonly startMax?: number;
  readonly attributes?: Readonly<Record<string, string | number | boolean>>;
}
interface StoredMetric {
  readonly intervalMs: number;
  readonly name: string;
  readonly kind: "counter" | "upDownCounter" | "histogram";
  readonly unit?: string;
  readonly attributes: SpanAttributes;
  readonly count: number;
  readonly sum: number;
  readonly min: number;
  readonly max: number;
}
interface TelemetryStore {
  findTraces(query?: TraceQuery): Promise<{ traces: readonly StoredTrace[]; cursor?: string }>;
  getTrace(traceId: string): Promise<StoredTrace | undefined>;
  findMetrics(query: MetricQuery): Promise<readonly StoredMetric[]>;
}
```

两个 store 的属性等值判定都是对内联原值的 JS `===`：`1` ≠ `true`，`"1"` ≠ `1`；数组永远不等于标量过滤值
（`===` 按引用比较，所以数组值属性无法匹配标量过滤器）。对 `SpanAttributes` 中的数组值，内存 store 无法按该键过滤成功。

### 2.4 上下文 / span 契约

```ts
interface TelemetryContext {
  readonly metrics?: TelemetryMetrics;
  readonly propagation?: TelemetryPropagation;
  startSpan<T>(options: SpanOptions, callback: (span: TelemetrySpan) => T | Promise<T>): Promise<T>;
}
interface TelemetrySpan extends TelemetryContext {
  addLink(link: SpanLink): void;
  addEvent(name: string, attributes?: SpanAttributes): void;
  setAttributes(attributes: SpanAttributes): void;
  setStatus(status: SpanStatus): void;
}
```

契约（README 为准，一致性套件逐条编码）：

1. 创建子 span 并**同步**调用回调，恰好一次；
2. 在上下文、子 span、extract 得到的父上下文（包括惰性回退 span）上暴露可选的 metrics 与 propagation 能力；
3. 保留回调的返回值与 rejection 值；同步 throw 产生的 promise 必须用**同一个值** reject；
4. 返回的 promise 结算前，span 保持打开；
5. 正常完成 ⇒ `ok`；throw/reject ⇒ error，除非已显式设置 status；
6. 重复 `setStatus()` 后写覆盖；
7. `setAttributes()` 合并；后面定义的值覆盖前面；`undefined` 忽略；
8. 记录方法同步；
9. 结算后的调用被忽略（惰性）。

"同步调用回调" 的含义：`startSpan(...)` 返回之前回调已经开始执行。Rust 中可移植的等价判据是：
调用 `startSpan(...)` 后**不 await** 就能观察到 `admitted === true`。Rust 只能做到"首次 poll 时创建 span 并进入回调"，
详见 §24。

### 2.5 Schema 定义类型（仅类型层面）

```ts
type TelemetryAttributeType = "string" | "number" | "boolean" | "string[]" | "number[]" | "boolean[]";
interface TelemetryAttributeMetadata { description: string; sensitive?: boolean; cardinality?: "low" | "high" }
type TelemetryAttributeDefinition = TelemetryAttributeMetadata & (
  | { type: "string"; values?: readonly string[]; examples?: readonly string[] }
  | { type: "number"; values?: readonly number[]; examples?: readonly number[] }
  | { type: "boolean"; values?: readonly boolean[]; examples?: readonly boolean[] }
  | { type: "string[]"; elementValues?: readonly string[]; examples?: readonly (readonly string[])[] }
  | { type: "number[]"; elementValues?: readonly number[]; examples?: readonly (readonly number[])[] }
  | { type: "boolean[]"; elementValues?: readonly boolean[]; examples?: readonly (readonly boolean[])[] }
);
type TelemetryStartAttributeDefinition = TelemetryAttributeDefinition & { required: boolean };
type TelemetryEventAttributeDefinition = TelemetryAttributeDefinition & { required: boolean };
interface TelemetryEventDefinition { description: string; attributes: Record<string, TelemetryEventAttributeDefinition> }
type TelemetryParentDefinition =
  | { kind: "any" } | { kind: "root_or_external" } | { kind: "spans"; spans: readonly string[] };
interface TelemetrySpanDefinition {
  description: string;
  parents: TelemetryParentDefinition;
  startAttributes: Record<string, TelemetryStartAttributeDefinition>;
  endAttributes: Record<string, TelemetryAttributeDefinition>;
  events?: Record<string, TelemetryEventDefinition>;
  status: { default: "ok"; errorWhen: string };
}
interface TelemetrySchemaDefinition {
  version: number;
  spans: Record<string, TelemetrySpanDefinition>;
  metrics?: Record<string, TelemetryMetricDefinition>;
}
```

运行时：schema 就是普通可 JSON 序列化的对象字面量，没有任何校验。`version` 是普通数字
（ai schema 用 1，durable 用 1，测试中的 request schema 用 3）。

### 2.6 `defineTelemetrySchema`

```ts
export function defineTelemetrySchema<const T extends TelemetrySchemaDefinition>(schema: T): T
```

类型化恒等：返回**同一个对象引用**（`expect(schema).toBe(definition)`），不校验、不克隆、不冻结。
Rust 中是 schema 结构体的 no-op 构造函数。

### 2.7 `createTypedSpanStarter`（运行时行为）

```ts
export function createTypedSpanStarter<const Schemas extends TelemetrySchemaTuple>(
  telemetryContext: TelemetryContext,
  _schemas: Schemas & UniqueTelemetrySchemas<Schemas>,
): TypedSpanStarter<Schemas>;
```

运行时行为（私有 `bindTypedSpanStarter`）：

```
bind(ctx):
  return (name, attributes, callback) =>
    ctx.startSpan({ name, attributes }, (span) =>
      callback(span, bind(span)))
```

- schema 参数在运行时被忽略（形参名为 `_schemas`，只用于类型推断）。
- 子 starter 在**每次**调用时重新绑定到回调 span，所以孙 span 是当前 span 的子级。
- 类型化 starter 无法传入 `SpanOptions.links`（只有 `name` + `attributes`）。
- 返回值 / rejection 传播完全由 `startSpan` 决定（包括同值 rejection）。
- Rust 没有重载集合；建模为 `TypedSpanStarter { ctx: Arc<dyn TelemetryContext> }`，提供
  `async fn start(&self, name: &str, attributes: SpanAttributes, f: F) -> T`。编译期 schema 检查丢失；
  运行时行为一致。这是不可避免的偏差，需在文档中标注（仅依赖类型报错的调用点会退化）。

### 2.8 index.ts 的再导出

来自 `./metrics.ts`：类型 `HistogramOptions`、`InstrumentOptions`、`SchemaTelemetryCounter`、`SchemaTelemetryHistogram`、
`TelemetryCounter`、`TelemetryHistogram`、`TelemetryMetricAttributeDefinition`、`TelemetryMetricDefinition`、
`TelemetryMetrics`、`TelemetryUpDownCounter`、`TypedTelemetryMetrics`；值 `createTypedMetrics`。
来自 `./noop.ts`：`NOOP_TELEMETRY_CONTEXT`、`NOOP_TELEMETRY_METRICS`、`NOOP_TELEMETRY_PROPAGATION`。
来自 `./memory.ts`：类型 `RecordedTelemetryEvent`、`RecordedTelemetryLink`、`RecordedTelemetryMeasurement`、
`RecordedTelemetrySpan`；值 `InMemoryTelemetryContext`。

---

## 3. `packages/telemetry/src/memory.ts` — `InMemoryTelemetryContext`

### 3.1 公开的 recorded 类型

```ts
interface RecordedTelemetryEvent { readonly name: string; readonly attributes: Readonly<SpanAttributes> }
interface RecordedTelemetryLink { readonly context: Readonly<TelemetryCarrier>; readonly attributes: Readonly<SpanAttributes> }
interface RecordedTelemetrySpan {
  readonly id: number;
  readonly parentId: number | null;              // 本地父 id；根或外部父级为 null
  readonly context: Readonly<TelemetryCarrier>;
  readonly parentContext: Readonly<TelemetryCarrier> | null;
  readonly links: readonly RecordedTelemetryLink[];
  readonly name: string;
  readonly attributes: Readonly<SpanAttributes>;
  readonly events: readonly RecordedTelemetryEvent[];
  readonly status: SpanStatus;
  readonly settled: boolean;
  readonly endSequence?: number;                 // 结算后才出现
}
interface RecordedTelemetryMeasurement {
  readonly kind: "counter" | "upDownCounter" | "histogram";
  readonly name: string;
  readonly description?: string;
  readonly unit?: string;
  readonly boundaries?: readonly number[];
  readonly value: number;
  readonly attributes: Readonly<SpanAttributes>;
  readonly context: Readonly<TelemetryCarrier> | null;   // exemplar；缺省为 null
}
```

注意：`RecordedTelemetrySpan` 刻意不含时间戳（起止时间私有）；快照中的事件也不带 `timeMs`。
`RecordedTelemetryMeasurement` 不含时间，区间私有。

内部可变状态：

```ts
interface InMemoryTelemetryState {
  metrics: TelemetryMetrics; propagation: TelemetryPropagation;
  scope: string;                                   // 每实例一个 crypto.randomUUID()
  spans: MutableRecordedTelemetrySpan[];
  measurements: { intervalMs: number; measurement: RecordedTelemetryMeasurement }[];
  nextSpanId: number;                              // 从 1 开始
  nextEndSequence: number;                         // 从 1 开始
}
```

span 身份表：模块级 `const contextCarriers = new WeakMap<TelemetryContext, TelemetryCarrier>()`。
这是传播身份的核心：适配器交出的任何 `TelemetryContext`（span、extract 得到的父上下文，以及通过对象展开得到的
惰性回退包装）都会登记在这里，只有登记过的上下文才能被 inject 或作为 link 目标。根（`InMemoryTelemetryContext`
自身）**不**登记，所以 `inject(root)` → `undefined`，指向根的 link 也被丢弃。

注意 WeakMap 是**模块级**的，不是每实例的：实例 A 的 span 可以被实例 B 的 `inject` 命中，也可以作为 B 的 link
目标或 histogram exemplar（已在本机验证：`b.inject(spanA)` 返回 A 的 carrier，B 的 span link 与 B 的测量
exemplar 都带有 A 的 carrier）。`extract` 得到的父上下文只能被**同一个**实例（或任何实例）的 `inject` 识别，
因为登记时已存入 carrier。

Rust：把 WeakMap 换成每个上下文对象上的身份键——从进程级原子计数器分配的 `u64` token，存于
`ContextIdentity { id: u64, carrier: TelemetryCarrier }`。**必须使用进程级（`static`）注册表**，例如
`static CONTEXT_CARRIERS: LazyLock<Mutex<HashMap<u64, TelemetryCarrier>>>`，而不是每实例一张表：JS 的
模块级 WeakMap 允许跨实例 inject/link/exemplar，测试（§8 的跨适配器用例与 §23.1 的 propagation 测试）依赖这一点。
若担心泄漏，只能用弱引用/显式释放策略替代（JS 用 WeakMap 自动回收；Rust 没有等价的自动机制，可让
`InMemoryTelemetryContext` 的 span 包装对象在 Drop 时移除条目，或接受进程内增长并文档化）。JS WeakMap 的键是对象身份：
同一 carrier 的两次 extract 得到两个不同的键，但 carrier 值相等。Rust 的 trait object 不能作为 map 键，
所以每个 context/span 结构体实现 `fn identity(&self) -> Option<u64>`；inject/link 查找走该 token。

### 3.2 辅助函数

`carrierId(carrier)` = `` `${carrier["kiana-memory-scope"]}:${carrier["kiana-memory-span"]}` `` —— 用作
`StoredSpan.id` 与 `links[].spanId`。

`copyAttributes(attributes?)`：
`Object.fromEntries(Object.entries(attributes ?? {}).filter(([,v]) => v !== undefined).map(([n,v]) => [n, Array.isArray(v) ? [...v] : v]))`。
后果：只处理自有可枚举字符串键；整数样式的键会被重排到最前并按数值升序（JS 对象键序）；`undefined` 值被丢弃；
数组被浅拷贝。

`copyLink(link)`：查 `contextCarriers.get(link.context)`；未知上下文（根、no-op、外部适配器）返回 `undefined`；
否则 `{ context: {...carrier}, attributes: copyAttributes(link.attributes) }`。因此指向未知上下文的 link 被静默丢弃。

`copyStatus(status)`：`ok` → 全新 `{ status: "ok" }`；`error` 且带 `error` → 全新
`{ status: "error", error: { name, message } }`；`error` 不带 → `{ status: "error" }`。

### 3.3 `createSpan`

```
startedMs = Date.now()
clock = performance.now()                 // 单调时钟
id = state.nextSpanId++
parentId = parent?.localId ?? null
context = {
  "kiana-memory-scope": state.scope,
  "kiana-memory-span": String(id),
  "kiana-memory-trace": parent?.context["kiana-memory-trace"] ?? `${state.scope}:${id}`,
}
parentContext = parent ? { ...parent.context } : null
links = options.links.map(copyLink).filter(defined)
name = options.name
attributes = copyAttributes(options.attributes)
events = [], status = {status:"ok"}, explicitStatus = false, settled = false
```

后果：trace id 从父 carrier 继承（包括外部/远端 trace id）；根的 trace id 等于自身 context 派生的
`${scope}:${id}`；数字 `id` 是 recorder 内部的，每个实例都从 1 开始。

### 3.4 `startInertSpan`

当**已结算**的 span 被用作父上下文（`parent?.span?.settled`）时，新 span 完全不被记录。回调仍然执行，收到一个
展开了共享 no-op span 的对象（所有记录方法是冻结的 no-op），但覆盖了 `metrics`、`propagation` 与 `startSpan`
（递归 `startInertSpan`）。它通过 `NOOP_TELEMETRY_CONTEXT.startSpan(options, ...)` 执行。可观察效果：

- 不向 `state.spans` 推送任何 span（`spans.length` 不变）；
- 回调同步执行，结果/rejection 经 no-op 上下文保留；
- 惰性 span 的 `addLink`/`addEvent`/`setAttributes`/`setStatus` 都是 no-op；
- 从惰性 span 再 `startSpan` 仍是惰性的；
- `metrics`/`propagation` 仍是真实适配器能力；
- 惰性 span 对象**不**登记到 `contextCarriers`（它是全新对象字面量），因此不能被 inject 或作为 link 目标。
  `inject(inertSpan)` → `undefined`；以惰性 span 为上下文的 link 被丢弃。

### 3.5 `startInMemorySpan`（核心算法）

```
if parent?.span?.settled: return startInertSpan(state, options, callback)
recorded = createSpan(state, parent, options)
state.spans.push(recorded)
span = {
  metrics, propagation,
  startSpan(childOptions, childCallback):
      startInMemorySpan(state, { context: recorded.context, localId: recorded.id, span: recorded }, childOptions, childCallback)
  addLink(link):  if recorded.settled return; copyLink → push if defined
  addEvent(name, attrs): if recorded.settled return;
      push { name, timeMs: performance.now() - recorded.clock, attributes: copyAttributes(attrs) }
  setAttributes(attrs): if recorded.settled return; recorded.attributes = { ...recorded.attributes, ...copyAttributes(attrs) }
  setStatus(status): if recorded.settled return; recorded.status = copyStatus(status); recorded.explicitStatus = true
}
contextCarriers.set(span, recorded.context)
try { return await callback(span) }
catch (error) {
  if (!recorded.explicitStatus)
    recorded.status = error instanceof Error
      ? { status: "error", error: { name: error.name, message: error.message } }
      : { status: "error" }
  throw error
} finally {
  recorded.settled = true
  recorded.endedMs = recorded.startedMs + performance.now() - recorded.clock
  recorded.endSequence = state.nextEndSequence++
}
```

必须测试的精确语义：

- 回调在 async 函数内、首个 `await` 之前同步调用，所以 `admitted`/`calls` 立即可见（`startSpan` 返回 promise，
  但回调体已经开始）。回调同步 throw 被同一个 `catch` 捕获（`await` 会把它变成 rejection）并重新抛出，
  因此返回的 promise 用同一个值 reject。
- `endSequence` 在结算时按 span 分配；并发打开的多个子 span 得到递增序号；后启动但先结算的子 span 序号小于父 span。
- `endedMs` = 启动时的墙钟 + 单调流逝，不受 span 期间墙钟跳变影响。
- 结算后设置的属性与状态被忽略；结算后启动的**子** span 是惰性的（不被记录）。
- `setAttributes` 带 `undefined` 值不会删除已设置的键（`copyAttributes` 先移除 `undefined` 项，展开时保留旧值）。
- `setStatus({ count: undefined })` 之类在 `{count:1}` 之后调用，`count` 仍为 1（一致性套件断言
  `{ start: "value", overwrite: "end", count: 1 }`）。
- error → status 映射使用 JS `Error` 接口：`name`（默认 `"Error"`）与 `message`。非 `Error` 的 rejection 值
  ⇒ `{ status: "error" }`，不带 `error`。
- 显式 `setStatus`（即使是 `ok`）会抑制自动错误赋值，包括回调之后 throw/reject 的情况（最后显式状态优先）。

### 3.6 `inject` / `extract`

`inject(context)`：`contextCarriers.get(context)` → 分离的 `{ ...carrier }` 或 `undefined`。
对已结算 span 与 extract 的父上下文都有效。不包含外部适配器的 scope。

`extract(carrier)`：
```
scope = carrier?.["kiana-memory-scope"]; span = carrier?.["kiana-memory-span"]; traceId = carrier?.["kiana-memory-trace"]
if (typeof scope !== "string" || typeof span !== "string" || typeof traceId !== "string") return this  // 根
context = { "kiana-memory-scope": scope, "kiana-memory-span": span, "kiana-memory-trace": traceId }
localId = scope === this.state.scope ? Number(span) : null
parent = {
  metrics: this, propagation: this,
  startSpan(options, cb) => startInMemorySpan(this.state, { context, localId }, options, cb),
}
contextCarriers.set(parent, context)
return parent
```
- 任一键缺失或值非字符串（`undefined`、`{}`、`{unknown:"format"}`、`{...carrier, key: 42}`）⇒ 返回 `this`（根）。
  注意判据是 `typeof === "string"`，所以带数字值的 carrier 失败。
- 额外的键被忽略（不复制）。
- `localId = Number(span)`；`Number("abc")` 是 `NaN`。`NaN` 不是 `null`，所以同 scope 且 span id 非数字的
  extract 父上下文产生的子 span 记录 `parentId: NaN`；而 `JSON.stringify(NaN) === "null"`，因此序列化后的
  `RecordedTelemetrySpan.parentId` 变成 `null`，而 `StoredSpan.parentId` 变成字符串 `"<scope>:abc"`（来自
  `parentContext`）——见 `storedSpans`。Rust 必须复刻 `Number(...)` 语义（按 JS 数字文法解析，失败为 `NaN`）。
  可用 `Option<f64>` + 支持 NaN 的表示，或枚举 `ParentId::None | Local(f64) | Foreign`。
  `RecordedTelemetrySpan.parentId` 的类型是 `number | null`，整数 id 保持整数；Rust 用 `Option<f64>` 并按
  JS 数字格式化输出非整数值（见 §22.3）。
- 每次 `extract` 都返回**新**的父上下文对象；同一 carrier 的两次 extract 身份不同但 carrier 相等。
- extract 的父上下文是跨适配器的：recorder A 的 carrier 用在 recorder B 上时，B 记录的 span `parentId: null`
  （scope 不同），`parentContext` 是该 carrier。

### 3.7 指标工厂

```ts
counter(name, options?)         -> { add: recorder("counter", name, options) }
upDownCounter(name, options?)   -> { add: recorder("upDownCounter", name, options) }
histogram(name, options?)       -> { record: recorder("histogram", name, options) }
```

`createMeasurementRecorder(state, kind, name, options?)` 在创建时捕获：`description = options?.description`、
`unit = options?.unit`、
`boundaries = kind === "histogram" && options?.boundaries !== undefined ? [...options.boundaries] : undefined`。
元数据对象用条件展开构造，缺省的键完全省略（不是 `undefined`）。三个工厂都接受 `HistogramOptions`
（工厂层面类型允许 counter 带 boundaries，但运行时只在 `kind === "histogram"` 时使用 boundaries）。

每次调用返回的 recorder：
```
intervalMs = Math.floor(Date.now() / 10_000) * 10_000
carrier = context === undefined ? undefined : contextCarriers.get(context)
push { intervalMs, measurement: { ...metadata, value, attributes: copyAttributes(attributes), context: carrier ? {...carrier} : null } }
```
- 计数方式：每个测量单独一行；内存中不做聚合。
- `context` 参数在**类型**上只存在于 `record`（histogram），但运行时三个 recorder 都是同一个三参数函数：
  额外传入 `counter.add(value, attrs, span)` 也会记录 exemplar（已在本机验证：`context` 为 span 的 carrier）。
  Rust 若把 `add` 建模为两参数方法则会丢失该运行时行为；若按类型建模则在文档中标注为不可达路径。
  传入根/no-op/未知上下文得到 `context: null`（根不在表中）；未传 context 也是 `null`（不是省略键）。
- `NaN`/Infinity 值原样存储（调用方受信任）。
- counter/upDownCounter 即使 options 带 `boundaries`，测量对象也**没有** `boundaries` 键（已验证只保留 `unit`）；
  `boundaries` 仅在 `kind === "histogram"` 且 `options.boundaries !== undefined` 时出现。
- 仪表元数据在工厂调用时复制；之后修改 options 对象或 boundaries 数组无效（一致性套件断言）。
  测量属性在记录时复制（之后修改调用方数组无效）。

### 3.8 `storedSpans()`（私有）与 store 方法

```
storedSpans = state.spans.filter(settled).map(span => ({
  id: carrierId(span.context),
  traceId: span.context["kiana-memory-trace"],
  ...(span.parentContext ? { parentId: carrierId(span.parentContext) } : {}),
  name, startedMs, endedMs: span.endedMs,
  attributes: copyAttributes(span.attributes),
  events: span.events.map(e => ({ name, timeMs, attributes: copyAttributes(e.attributes) })),
  links: span.links.map(l => ({ traceId: l.context["kiana-memory-trace"], spanId: carrierId(l.context), attributes: copyAttributes(l.attributes) })),
  status: copyStatus(span.status),
})).sort((a,b) => a.startedMs - b.startedMs || (a.id < b.id ? -1 : a.id > b.id ? 1 : 0))
```
- 只出现已结算 span。已结算 span 的 `endedMs` 非空（`!` 断言）。
- 排序：`startedMs` 升序；并列时按 `"scope:id"` 字符串比较。注意 tie-breaker 比较完整 ID 字符串，
  因此 `"...:10" < "...:9"`（字典序）。
- `links[].attributes` 始终存在（可能是 `{}`），因为 `copyLink` 总是设置它。
- `parentId` 当且仅当 `parentContext` 非 null 时出现（远端父级得到字符串 id）。

`getTrace(traceId)`：按 traceId 过滤 `storedSpans()`；返回 `{ traceId, spans }`，为空时 `undefined`。
span 按上述顺序排序。每次调用返回全新快照；修改它不影响 store。

`findTraces(query = {})`：
```
limit = query.limit ?? 20
after = query.cursor === undefined ? undefined : JSON.parse(query.cursor) as [number, string]
group storedSpans() by traceId preserving first-seen order
matches = groups.filter(trace => {
  first = trace.spans[0]
  (query.name === undefined || first.name === query.name)
  && (startMin === undefined || first.startedMs >= startMin)
  && (startMax === undefined || first.startedMs <= startMax)
  && (!after || first.startedMs < after[0] || (first.startedMs === after[0] && trace.traceId > after[1]))
  && (!query.errors || trace.spans.some(s => s.status.status === "error"))
  && (!query.attributes || trace.spans.some(s => every entry [k,v] of query.attributes: s.attributes[k] === v))
})
.sort((a,b) => b.spans[0].startedMs - a.spans[0].startedMs || (a.traceId < b.traceId ? -1 : a.traceId > b.traceId ? 1 : 0))
traces = matches.slice(0, limit)
last = traces.at(-1)!
return { traces, ...(matches.length > limit ? { cursor: JSON.stringify([last.spans[0].startedMs, last.traceId]) } : {}) }
```
- 顺序：trace 起点最新在前；并列时 traceId 升序（字符串比较）。
- `cursor` 是 `JSON.stringify([startedMs, traceId])`，例如 `[20000,"<uuid>:3"]`。
- 续页是严格 "after" 语义：`startedMs < cursor[0]`，或起点相等且 traceId 严格更大。
- `limit` 受信任（**已在本机对 3 个 trace 的 store 实测**）：`slice(0, limit)` 使用 JS 的 ToIntegerOrInfinity
  语义，负数表示"从末尾丢弃"，所以 `limit: -1` 返回前 2 个 trace 并附 cursor，`limit: -2` 返回前 1 个，
  `limit: -3/-5` 抛 `TypeError: Cannot read properties of undefined (reading 'spans')`；`limit: 0` 在有匹配时
  抛同一 `TypeError`；`limit: NaN` → 0 个 trace、无 cursor、**不抛**（不满足 `matches.length > NaN`）；
  `limit: 2.5` → 向下取整为 2 个。空 store 时：`limit: 0` 与正数都返回 `{traces: []}`，任何负数都抛
  `TypeError`（`slice(0,-1)` 得空数组后 `at(-1)` 为 undefined）。
  **精确的负数规则**（在 n 个匹配上实测）：`slice(0, -k)` 得到前 `max(0, n-k)` 个；当且仅当 `n - k <= 0`
  时数组为空、`at(-1)` 为 undefined，于是抛 `TypeError`。所以 `n=3` 时 `-1` 得 2 个（带 cursor）、`-2` 得 1 个
  （带 cursor）、`-3`/`-4` 抛；`n=2` 时 `-1` 得 1 个（带 cursor）、`-2` 抛；`n=1` 时连 `-1` 都抛。
  NaN limit 也走 `slice`（ToIntegerOrInfinity(NaN) = 0），但 `matches.length > NaN` 为假 → 无 cursor、
  不触发 `at(-1)`，所以不抛。Rust 需决定：复刻该 `slice` 语义
  （含负数）还是拒绝非正 limit；测试不覆盖，标记为未定义边界，但**不要**简单声称"`limit <= 0` 返回空页"。
- cursor 非法 JSON 会抛（`JSON.parse`）。已实测的其它 cursor 边界：`cursor: "null"` 解析为 `null`，
  `!after` 为真 → 等价于无 cursor，返回全部；`cursor: "[]"`（`after[0] === undefined`）与
  `cursor: "\"ab\""`（字符串索引）使所有比较为 false → 返回空页；`after` 长度不足不会抛。
- 属性过滤：请求的每个条目都必须命中**同一个** span（trace 中任一 span 全部匹配即可）。
- trace 的 "first" span 是最早存储的 span；若父级缺失/远端，则子 span 可能是第一个。

`findMetrics(query)`：
```
filter measurements where measurement.name === query.name
  && (startMin === undefined || intervalMs >= startMin)
  && (startMax === undefined || intervalMs <= startMax)
  && every entry [k,v] of (query.attributes ?? {}): measurement.attributes[k] === v
map to { intervalMs, name, kind, ...(unit === undefined ? {} : {unit}), attributes: copyAttributes(...),
         count: 1, sum: value, min: value, max: value }
```
- 顺序是记录顺序（不排序）。不暴露 `description`/`boundaries`/`context`。
- 没有 `limit`。`attributes` 默认 `{}`，匹配一切。

`getMetrics()`：分离的 `RecordedTelemetryMeasurement` 列表，按记录顺序：
`{ ...measurement, ...(boundaries === undefined ? {} : { boundaries: [...boundaries] }),
   attributes: copyAttributes(attributes), context: context ? {...context} : null }`。
`description`/`unit` 键在缺省时不存在（条件展开）。不带时间。

`getSpans()`：分离的 `RecordedTelemetrySpan` 列表，按 span 启动顺序（push 顺序），包括未结算 span
（`settled: false`，无 `endSequence`）。快照中的事件只有 `{name, attributes}`。
字段：`id`、`parentId`（number|null）、`context`、`parentContext`、`links`、`name`、`attributes`、
`events`、`status`、`settled`、可选 `endSequence`。

### 3.9 类形状

```ts
class InMemoryTelemetryContext implements TelemetryContext, TelemetryPropagation, TelemetryMetrics, TelemetryStore {
  readonly metrics = this; readonly propagation = this;
  private state = { metrics: this, propagation: this, scope: crypto.randomUUID(), spans: [], measurements: [], nextSpanId: 1, nextEndSequence: 1 };
  startSpan(options, callback) -> startInMemorySpan(state, undefined, options, callback)
  inject, extract, counter, upDownCounter, histogram, getTrace, findTraces, findMetrics, getMetrics, getSpans
}
```
`crypto.randomUUID()` 产生小写 RFC 4122 v4 UUID（36 字符，如 `e39f03fc-47f7-4ef0-baf8-632e60f12f34`）。
测试正则：`id =~ /^[0-9a-f-]{36}:[1-9][0-9]*$/`。

---

## 4. `packages/telemetry/src/noop.ts`

```ts
async function startNoopSpan<T>(_options, callback) { return callback(noopTelemetrySpan); }
const noopCounter = Object.freeze({ add: () => {} });
const noopUpDownCounter = Object.freeze({ add: () => {} });
const noopHistogram = Object.freeze({ record: () => {} });
export const NOOP_TELEMETRY_METRICS = Object.freeze({ counter: () => noopCounter, upDownCounter: () => noopUpDownCounter, histogram: () => noopHistogram });
export const NOOP_TELEMETRY_PROPAGATION = Object.freeze({ inject: () => undefined, extract: () => NOOP_TELEMETRY_CONTEXT });
const noopTelemetrySpan = { metrics: NOOP_TELEMETRY_METRICS, propagation: NOOP_TELEMETRY_PROPAGATION, startSpan: startNoopSpan, addLink(){}, addEvent(){}, setAttributes(){}, setStatus(){} };
Object.freeze(noopTelemetrySpan);
export const NOOP_TELEMETRY_CONTEXT: TelemetrySpan = noopTelemetrySpan;
```

- 根与所有嵌套 span 共用一个冻结 span（`child === parent` 身份）。
- `startSpan` 声明为 `async`，但回调在首个 await 之前同步调用；抛出的错误变成具有相同值的 rejected promise。
- 仪表是共享冻结单例：`NOOP_TELEMETRY_METRICS.counter("a") === NOOP_TELEMETRY_METRICS.counter("b")`，
  `Object.isFrozen(counter)` 为 true；记录时完全忽略参数。
- `NOOP_TELEMETRY_CONTEXT.metrics === NOOP_TELEMETRY_METRICS`，`.propagation === NOOP_TELEMETRY_PROPAGATION`。
- `NOOP_TELEMETRY_PROPAGATION.extract(任何值)` 返回 `NOOP_TELEMETRY_CONTEXT`。

Rust：用 `OnceLock<Arc<NoopTelemetrySpan>>`（或用 `LazyLock`）实现单例，保证 `Arc::ptr_eq` 身份；
`start_span` 直接调用回调（首次 poll 时）。

---

## 5. `packages/telemetry/src/metrics.ts`

```ts
interface InstrumentOptions { description?: string; unit?: string }
interface HistogramOptions extends InstrumentOptions { boundaries?: readonly number[] }  // 有限、严格递增，调用方负责
interface TelemetryCounter      { add(value: number, attributes?: SpanAttributes): void }
interface TelemetryUpDownCounter{ add(value: number, attributes?: SpanAttributes): void }
interface TelemetryHistogram    { record(value: number, attributes?: SpanAttributes, context?: TelemetryContext): void }
interface TelemetryMetrics {
  counter(name: string, options?: InstrumentOptions): TelemetryCounter;
  upDownCounter(name: string, options?: InstrumentOptions): TelemetryUpDownCounter;
  histogram(name: string, options?: HistogramOptions): TelemetryHistogram;
}
```

类型化指标 schema：

```ts
type TelemetryMetricAttributeDefinition = TelemetryStartAttributeDefinition & { cardinality?: "low" };
type TelemetryMetricDefinition = { description: string; unit?: string;
  attributes: Record<string, TelemetryMetricAttributeDefinition> } &
  ({ kind: "counter" | "upDownCounter"; boundaries?: never } | { kind: "histogram"; boundaries?: readonly number[] });
```

`createTypedMetrics(metrics, schemas)` 运行时行为：
```
definitions = new Map()
for (schema of schemas) for ([name, def] of Object.entries(schema.metrics ?? {})) definitions.set(name, def)
return {
  counter: name => metrics.counter(name, definitions.get(name)),
  upDownCounter: name => metrics.upDownCounter(name, definitions.get(name)),
  histogram: name => metrics.histogram(name, definitions.get(name)),
}
```
- 后面的 schema 按名覆盖前面的（运行时无重复检查；那是类型层面）。
- 所有 schema 中都没有的名字传 `undefined` 选项。
- 完整定义对象（含 `kind`、`description`、`unit`、`attributes`、`boundaries`）作为选项传入；内存适配器只读
  `description`、`unit` 与（histogram 的）`boundaries`，所以没有 boundaries 的 histogram 定义不产生 boundaries。
- 运行时不做属性校验。
- 每次访问器调用都会调用仪表工厂（`metrics.counter(name)` 每次返回新仪表；各自携带复制的元数据）。
- Rust 中丢失的编译期保证：kind 限制、精确属性、重复名、`cardinality: "high"` 拒绝——标注为仅类型层面。

---

## 6. `packages/telemetry/src/render.ts` — `renderTrace`

```ts
function formatAttributes(attributes: Readonly<SpanAttributes> = {}): string {
  return Object.entries(attributes).map(([key, value]) => ` ${key}=${JSON.stringify(value)}`).join("");
}
export function renderTrace(spans: readonly StoredSpan[]): string
```

算法，精确如下：

1. `byId = new Map(spans.map(s => [s.id, s]))` —— 后来的重复 id 覆盖先前的，但 `Map.set` 对已存在键保留**首次**
   出现的插入顺序。
2. 遍历 `[...byId.values()]` 按 `left.startedMs - right.startedMs` 排序（V8 的 `Array#sort` 是稳定排序，
   起点相同保持插入顺序；比较函数返回 `NaN` 时视为 0，所以含 NaN 起点时这些元素保持原相对位置，
   **不会**崩溃——已实测 `[{a,NaN},{b,NaN},{c,5}]` 排序后仍是 a,b,c。Rust 的 `sort_by` 对 `partial_cmp`
   返回 None 会 panic，必须用 `sort_by(|l,r| l.started_ms.partial_cmp(&r.started_ms).unwrap_or(Equal))`
   或 `total_cmp`（`total_cmp` 会把 NaN 排到末尾，与 JS 行为不同，仅在不含 NaN 时等价））。
3. 根：`parentId === undefined || !byId.has(parentId)` → `roots`；否则 append 到 `children[parentId]`
   （兄弟保持排序后的顺序）。
4. `pending = roots.reverse().map(s => ({span:s, depth:0}))`，然后 while 循环从尾部 pop（因此根按起点升序渲染）。
5. 每个 span：`indent = "  ".repeat(depth)`；`error = ""`；若状态为 error：`error = " error"`，并且
   若 `status.error !== undefined`：`error += " " + name + ": " + message`。
6. 行：`` `${indent}${span.name}${formatAttributes(span.attributes)} (${span.endedMs - span.startedMs}ms)${error}` ``。
7. 按顺序每个事件：`` `${indent}  · ${event.name}${formatAttributes(event.attributes)} (+${event.timeMs}ms)` ``。
8. 按顺序每个 link：`` `${indent}  → link trace ${link.traceId} span ${link.spanId}${formatAttributes(link.attributes ?? {})}` ``。
   注意：`link.attributes` 可能是 `undefined`；`formatAttributes` 默认 `{}`，所以无输出。
9. 子级逆序压栈，以便按升序 pop；depth+1。
10. 返回 `lines.join("\n")`；空输入 → `""`。

字符细节（逐字）：每层两个空格；事件前缀是两个空格、`·`（U+00B7）、一个空格；link 前缀是两个空格、`→`（U+2192）、
一个空格、字面量 `link trace `、trace id、字面量 ` span `、span id；属性前缀是一个空格；`key=`，然后
`JSON.stringify(value)`；时长用 JS 数字转字符串（`20`、`4.5`、`0.19999999999999998`）；事件时间 `+${timeMs}ms`。

测试中的逐字期望输出：

```
root request="a b" attempt=2 cached=false tags=["a","b"] sizes=[1,2] flags=[true,false] (20ms)
  · checkpoint phase="tools" (+2ms)
  · tick (+3.5ms)
  → link trace remote-trace span remote reason="steer"
  → link trace other-trace span other
  first (4.5ms) error Error: failed call
    nested (1ms)
  second (2ms) error
```

```
earlier (5ms)
  → link trace trace span later
later (10ms)
  child (5ms)
```

```
server (10ms)
  → link trace worker-trace span worker:1
worker (10ms)
  → link trace server-trace span server:1
```

`root (10ms)\n  child (10ms)\n    · done (+3ms)\n    grandchild (10ms)`（按 id 去重）。

`root (10ms)\n  z (10ms)\n  a (10ms)\nother (10ms)`（起点相同时保持输入顺序；冻结的输入不被修改）。

`root text="line one\\n\\"line two\\"" (10ms)`（JSON 转义）。

2000 层链渲染 2000 行，最后一行是 `"  ".repeat(1999) + "1999 (10ms)"` —— 迭代实现，无递归。
环不可能无限渲染：只有 `byId` 中存在的 span 会被遍历，每个 span 只出现一次；自父 span 是自己的子级，
从根不可达，所以静默丢弃（除非另一个根能到达它）。必须保持相同行为；不要加环检测。

备注 / Rust：
- 属性值的 `JSON.stringify`：字符串按 JSON 转义，数字按 JS 格式化，布尔字面量，数组为 `[...]` 并以 `,` 分隔
  （无空格）；`undefined` 元素会序列化为 `null`（通过存储快照不可达；若调用方直接构造 `StoredSpan`，
  顶层 `undefined` 值可能出现——无测试覆盖的边界；只有在 Rust 类型允许 "undefined" 变体时才复刻为
  `u=undefined`，否则标注偏差）。
- 数字格式化必须遵循 ECMAScript Number::toString（§22.3）。
- 负零打印为 `0`。
- 无 ANSI、无颜色、无宽度处理。

---

## 7. `packages/telemetry/src/testing/*` — 适配器一致性套件

### 7.1 `types.ts`

```ts
interface TelemetryAdapterFixture extends AsyncDisposable {
  readonly context: TelemetryContext;
  readonly propagation?: TelemetryPropagation;
  readonly metrics?: TelemetryMetrics;
  getMetrics?(): Promise<readonly RecordedTelemetryMeasurement[]>;
  getSpans(): Promise<readonly RecordedTelemetrySpan[]>;
}
type TelemetryAdapterFixtureFactory = () => Promise<TelemetryAdapterFixture>;
interface TelemetryAdapterConformanceCase { readonly group: string; readonly name: string; run(): Promise<void> }
```

`AsyncDisposable` 表示 `[Symbol.asyncDispose](): Promise<void>`；每个 case 执行
`await using fixture = await factory()`，因此 case 结束后（包括失败）一定 dispose。
Rust：fixture 为带 `async fn dispose(&mut self)` 的结构体；cases 返回 `Vec<Case>`，其中
`Case { group, name, run: Box<dyn Fn() -> BoxFuture<'static, Result<(), String>>> }`；run 包装器创建 fixture、
执行测试、始终 dispose，并传播测试错误。

### 7.2 `conformance.ts` 辅助

- `createCase(factory, group, name, test)` → `{ group, name, async run() { await using f = await factory(); await test(f); } }`。
- `findSpan(spans, name)` → 第一个同名 span；缺失时 `ok(span, \`Expected recorded span ${name}\`)` 抛
  `AssertionError`，消息即为该文本。
- `rejectsWithSameValue(operation, expected)` → await；若 fulfilled，`fail("Expected operation to reject")`；
  rejection 时断言 `strictEqual(error, expected)`（对象按身份，原始值按 SameValue——`undefined` rejection 可用）。

### 7.3 14 个 case（group、name 逐字，及断言行为）

group 与 name 是用户可见的测试标识，必须逐字移植：

1. `capabilities` / `carries adapter capabilities through contexts and spans`
   - `metrics = fixture.metrics ?? fixture.context.metrics`，
     `propagation = fixture.propagation ?? fixture.context.propagation`。
   - 对每个上下文（根、span、extract 的父上下文、从已结算 span 启动的惰性 span、extract 父上下文的子 span）：
     `context.metrics` 与 `context.propagation` 与 `metrics`/`propagation` 是**同一对象**。
   - 启动 `capabilities`，子 `capabilities.child`，然后若有 propagation：
     `extracted = propagation.extract(propagation.inject(span))`，从中启动 `capabilities.extracted`；
     结算后从已结算 span 启动 `capabilities.inert` 并同样检查能力。
2. `metrics` / `records ordered measurements and optional histogram exemplars`
   - 仅当 `fixture.metrics`；要求 `getMetrics`。
   - counter `"bytes"` `{description:"Encoded bytes", unit:"By"}` `.add(12, {direction:"out"})`；
     upDownCounter `"active"` `.add(1)`、`.add(-1)`；histogram `"duration"` `{unit:"ms", boundaries:[1,10]}`
     在 span `measured` 内 `.record(5, {method:"read"}, span)`。
   - 测量按顺序：`["counter","bytes",12]`、`["upDownCounter","active",1]`、`["upDownCounter","active",-1]`、
     `["histogram","duration",5]`。
   - `measurements[0].attributes` 深度等于 `{direction:"out"}`；description/unit 精确；
     `measurements[3].boundaries` 深度等于 `[1,10]`；`measurements[3].context` 深度等于已记录 span `measured`
     的 `context`。
3. `metrics` / `copies instrument metadata and measurement attributes`
   - `boundaries = [1,10]`，`options = {description:"Original", boundaries}`；创建 histogram `"snapshot"`，
     然后 `boundaries.push(100)`、`options.description = "Changed"`。
   - `attributes = {tags:["original"]}`；`record(-1, attributes)`；然后 `attributes.tags.push("changed")`。
   - 只有一个测量；attributes `{tags:["original"]}`；boundaries `[1,10]`；description `"Original"`；
     `context === null`。
4. `links` / `records initial and late links without changing parents`
   - span `target` 先结算。`attributes = {reason:["initial"]}`；span `linked` 带
     `links:[{context: target, attributes}]` 创建；回调内 `attributes.reason.push("mutated")`、
     `span.addLink({context: targetSpan, attributes:{reason:"late"}})`、
     `span.addLink({context: NOOP_TELEMETRY_CONTEXT})`。
   - `linked.parentId === null`，`linked.parentContext === null`。
   - `linked.links` 深度等于 `[{context: <target.context>, attributes:{reason:["initial"]}},
     {context: <target.context>, attributes:{reason:"late"}}]` —— no-op link 被丢弃、顺序保留、
     数组在添加时复制、父级不变。
5. `links` / `ignores links added after source settlement`
   - span `link-source` 结算；然后 `source.addLink({context: source})`；`links` 为 `[]`。
6. `propagation` / `roundtrips parent identity through JSON without retaining the carrier`
   - 在 span `sender` 内：`carrier = inject(span)` 为真值；`wire = JSON.parse(JSON.stringify(carrier))`；
     `parent = extract(wire)`；把 `wire` 每个键改成 `"mutated"`；`parent.startSpan("receiver", () => 42)`
     返回 42；`inject(span)` 仍深度等于 `carrier`。
   - `receiver.parentContext` 深度等于 `sender.context`。
7. `propagation` / `absent and unknown carriers produce recorded roots`
   - `inject(NOOP_TELEMETRY_CONTEXT) === undefined`。
   - 对 `[undefined, {}, {unknown:"format"}]`：`extract(c).startSpan(\`fallback-${i}\`, () => 42)` → 42。
   - 每个已记录 span 的 `parentContext === null`。
8. `propagation` / `propagates across fresh adapters and multiple hops`
   - 通过工厂创建两个额外 fixture（`receiver`、`worker`）（各自必须有 propagation）。
   - sender 的 carrier 来自 span `sender`；receiver extract JSON 往返副本并启动 `receiver`，
     捕获 `receiverCarrier = receiver.propagation.inject(span)`；worker extract `receiverCarrier` 的 JSON
     往返副本并启动 `worker`。
   - `receiver.parentContext` 深度等于 `senderCarrier`；`worker.parentContext` 深度等于 `receiverCarrier`。
   - 变异隔离：JSON 往返不得与原对象共享引用。
9. `callback lifecycle` / `admits once synchronously and preserves the result`
   - 在 `startSpan(...)` 之后**不 await**：`admitted === true`、`calls === 1`。`await result === expected`
     （同一对象）。
   - 已记录 `success` 状态 `{status:"ok"}`、`settled: true`。
10. `callback lifecycle` / `preserves synchronous and asynchronous rejection values`
    - 同步 throw Error("sync") → 用**同一** Error 实例 reject。
    - async throw `{kind:"async"}` → 同一对象。
    - `Promise.reject(undefined)` → 用 `undefined` reject。
    - 三个 span（`sync-error`、`async-error`、`undefined-error`）的 status.status 都是 `"error"`。
11. `status` / `uses last explicit status without automatic overwrite`
    - `last-status`：先设 error `{Expected, "first"}`，再 `{status:"ok"}` → 最终 `{status:"ok"}`。
    - `explicit-before-throw`：`setStatus ok` 后 throw → 最终 `{status:"ok"}`，rejection 保留。
    - `explicit-before-rejection`：设 error `{Expected, "async failure"}` 后 `Promise.reject` →
      最终是该 error，rejection 保留。
    - `expected-failure`：设 error `{Expected, "returned failure"}`，返回 `{ok:false}` → error 保留。
12. `recording` / `merges attributes and records ordered events`
    - 起始属性 `{start:"value", overwrite:"start", ignored: undefined}`。
    - `setAttributes({count:1, overwrite:"middle"})`；`setAttributes({count:undefined, overwrite:"end"})`。
    - `addEvent("first", {index:1, ignored:undefined})`；`addEvent("second", {index:2})`。
    - 最终属性深度等于 `{start:"value", overwrite:"end", count:1}`（键序按插入：start、overwrite、count——
      `ignored` 被丢弃，`count` 未被 `undefined` 删除）。
    - 事件 `[{name:"first", attributes:{index:1}}, {name:"second", attributes:{index:2}}]`。
13. `recording` / `makes calls after settlement inert`
    - 结算 span `settled`（`{value:"initial"}`）并捕获；然后 `setAttributes({value:"late"})`、
      `addEvent("late", {value:true})`、`setStatus({status:"error"})`。
    - `capturedSpan.startSpan({name:"late-child"}, () => { childAdmitted = true; return 7; })`：
      `childAdmitted === true` 同步成立，await → 7，但没有任何 span 被记录。
    - `spans.length === 1`；attributes `{value:"initial"}`；events `[]`；status `{status:"ok"}`。
14. `parentage` / `records nested and concurrent child relationships`
    - 父 `parent`；内部启动 `first-child`（异步，等待 gate）与 `second-child`（同步返回 `"done"`），
      **不 await 第一个**；`await second === "done"`；释放 gate；`await first`。
    - `first.parentId === parent.id`，`second.parentId === parent.id`，`parent.parentId === null`。
    - 三者都有 `endSequence`；`second.endSequence < first.endSequence < parent.endSequence`。

### 7.4 `testing/index.ts`

`export { createTelemetryAdapterConformance } from "./conformance.ts";` 加上三个类型。
Rust：`testing` 模块可用 feature 开关，但应当移植，因为下游适配器测试使用它（chord 的 sqlite store 测试
只导入 `store-conformance.ts`，那是测试文件，不是包导出）。

---

## 8. `packages/telemetry/test/store-conformance.ts`（共享测试，非包导出）

`telemetryStoreConformance(name, factory)` 注册一个 vitest `describe(name)`，含 6 个 `it`。
`StoreFixture { context: TelemetryContext & { metrics; propagation }; store: TelemetryStore; flush(): Promise<void>; [Symbol.asyncDispose] }`。
每个测试内 mock `Date.now`；`flush()` 在内存实现中是 no-op，在 SQLite 中是 commit。六个测试与断言
（移植到 Rust 针对内存 store，并保持可复用于未来的 sqlite store 单元）：

1. `returns only settled traces, complete span snapshots, and detached results`
   - `Date.now = 20_000`。启动 `root`，属性 `{tags:["original"]}`，`addEvent("event",{tags:["original"]})`，
     等待 gate，然后启动子 `child`。
   - 释放前：`findTraces().traces` 为 `[]`；`getTrace("missing")` 为 undefined。
   - 释放 + flush 后：一个 trace；`root.id` 匹配 `/^[0-9a-f-]{36}:[1-9][0-9]*$/`，
     `root.traceId === root.id`，`root.parentId` 为 undefined，`child.parentId === root.id`，
     `child.traceId === root.traceId`，`root.startedMs === 20_000`，`root.endedMs >= root.startedMs`，
     `root.events[0].timeMs >= 0`。
   - 修改返回的 `root.attributes.tags` 与 `events[0].attributes.tags` 不得影响新的
     `getTrace(trace.traceId)`（仍为 `["original"]`）。
2. `filters by first-span name and start, any error, and all attributes on one span`
   - `now = 10_000` 启动 `old`；内部设 `30_000` 启动子 `recent-child` `{left:1}` 与 `split` `{right:2}`。
     然后 `20_000` 启动 `wanted`；内部 `20_001` 启动 `failure`
     `{left:1,right:2,valid:true,'odd."key':"value"}` 并设 error 状态。然后 `40_000` 启动 `new` `{valid:1}`。
   - 查询（都通过 `findTraces` 映射 `trace.spans[0].name`）：
     `{name:"wanted",attributes:{left:1,right:2},errors:true,startMin:20_000,startMax:20_000}` → `["wanted"]`；
     `{name:"failure"}` → `[]`（第一个 span 是 `wanted`）；
     `{attributes:{left:1,right:2}}` → `["wanted"]`；
     `{attributes:{valid:true,'odd."key':"value"}}` → `["wanted"]`；`{attributes:{valid:1}}` → `["new"]`；
     `{errors:true}` → `["wanted"]`；`{startMin:20_000}` → `["new","wanted"]`；`{startMax:10_000}` → `["old"]`。
3. `pages complete traces without duplicates when trace starts tie`
   - 25 个 trace，每个 root 在 `20_000` 带 `{index}`，child 在 `20_001`。
   - 默认页：20 个 trace 加 cursor；用该 cursor 的下一页：5 个 trace，无 cursor；`limit:100` 返回全部 25；
     拼接等于完整列表；每个 trace 有 2 个 span。
   - 用 `{name:"root", limit:3, cursor}` 分页得到的 id 序列等于 `all.map(t => t.traceId)`。
4. `inherits remote trace IDs and preserves remote parents and cross-trace links`
   - sender 启动 `sender`，捕获 carrier；flush；`sent = sender.store.findTraces().traces[0]`。
   - receiver extract（JSON 往返）得 `remote`；`remote.startSpan("receiver")`；
     receiver 启动 `linked`，`links:[{context: remote, attributes:{reason:"background"}}]`。
   - `receiver.store.getTrace(sent.traceId)` 有 1 个 span 匹配
     `{traceId: sent.traceId, parentId: sent.spans[0].id, name:"receiver"}`，且其 `id` 不同。
   - `linked` 的 trace：traceId ≠ sent.traceId；`links` 深度等于
     `[{traceId: sent.traceId, spanId: sent.spans[0].id, attributes:{reason:"background"}}]`。
   - 对 carrier 的每个键：分别 extract 删除该键的副本、以及该键设为数字 `42` 的副本，然后启动 `fallback`
     → 记录为根。flush 后 `fallback` trace 数量为 `keys * 2`，每个首个 span `parentId === undefined` 且
     `traceId === id`。
5. `accepts external string identities without imposing an ID format`
   - carrier 的键值替换为 `external-<key>`；`extract` 后 `inject(parent)` 深度等于该外部 carrier；
     `parent.startSpan("continued")`；通过 `findTraces({name:"continued"})` 检查：
     `trace.traceId` 以 `external-` 开头，`spans[0].parentId` 以 `external-` 开头。
6. `queries metric names, inclusive interval bounds, typed attribute equality, and detached values`
   - `now = 20_001`；counter `"hits"` `{unit:"By"}` 依次 add `2 {method:"read",valid:true}`、
     `3 {valid:true,method:"read"}`、`11 {method:"write",valid:1}`；counter `"other"` add
     `100 {method:"read",valid:true}`；`now = 30_001` add `7 {method:"read",valid:true}`。
   - 查询 `{name:"hits", startMin:20_000, startMax:20_000, attributes:{method:"read", valid:true}}`：
     count 求和 2，sum 求和 5，min 的最小值 2，max 的最大值 3；每行 kind 为 counter、unit `"By"`、
     `intervalMs === 20_000`。
   - 修改 `rows[0].attributes.method` 不影响重新查询的结果。
   - `findMetrics({name:"missing"})` → `[]`。
   - `{name:"hits", attributes:{valid:1}}` → sum 11（数字 1 不等于布尔 true）。
   - `{name:"hits", startMin:30_000}` → sum 7。

内存 store 中 fixture 就是上下文本身（`store === context`，`flush` 为 no-op）。

---

---

**Part B — `@earendil-works/kiana-mcp`**

## 9. `packages/mcp/src/protocol/jsonrpc.ts`

```ts
type JsonRpcId = string | number;
interface JsonRpcRequest { jsonrpc: "2.0"; id: JsonRpcId; method: string; params?: unknown }
interface JsonRpcNotification { jsonrpc: "2.0"; method: string; params?: unknown }
interface JsonRpcErrorObject { code: number; message: string; data?: unknown }
interface JsonRpcSuccessResponse { jsonrpc: "2.0"; id: JsonRpcId; result: unknown }
interface JsonRpcErrorResponse { jsonrpc: "2.0"; id: JsonRpcId; error: JsonRpcErrorObject }
type JsonRpcResponse = JsonRpcSuccessResponse | JsonRpcErrorResponse;
type JsonRpcMessage = JsonRpcRequest | JsonRpcNotification | JsonRpcResponse;
const JSON_RPC_ERROR_CODES = { parseError: -32700, invalidRequest: -32600, methodNotFound: -32601, invalidParams: -32602, internalError: -32603 } as const;
```

类（都继承 `Error`；`name` 显式赋值）：

| 类 | 构造器 | `.name` | 消息 |
|---|---|---|---|
| `McpError` | `(code: number, message: string, data?: unknown)` | `"McpError"` | `message` |
| `McpConnectionClosedError` | `(message = "MCP connection closed")` | `"McpConnectionClosedError"` | `message` |
| `McpTimeoutError` | `(timeoutMs: number)` | `"McpTimeoutError"` | `` `MCP request timed out after ${timeoutMs}ms` `` |
| `McpAbortError` | `(message = "MCP request aborted")` | `"AbortError"` | `message` |

额外字段：`McpError.code: number`、`.data: unknown`；`McpTimeoutError.timeoutMs: number`。
`McpAbortError` 刻意报告 name `"AbortError"`（测试匹配 `{name:"AbortError"}`）。

函数：

- `isObject(v)`：`typeof v === "object" && v !== null && !Array.isArray(v)`。Rust：`Value::Object`。
- `toError(v)`：`v instanceof Error` 时返回 `v`，否则 `new Error(String(v))`。`String(v)` 语义：
  `undefined` → `"undefined"`，`null` → `"null"`，对象 → `"[object Object]"`，数组 → `"a,b"`，
  数字按 JS 格式化，字符串本身。Rust：定义 `fn to_error_message(v: &Value) -> String` 实现 JSON 值的
  JS `String()` 语义。
- `isJsonRpcId(v)`：字符串，或有限数字（`Number.isFinite`；`NaN`/±Infinity 为 false）。
- `isJsonRpcRequest(m)`：对象、`jsonrpc === "2.0"`、`isJsonRpcId(id)`、`typeof method === "string"`。
  注意：同时带 `id`、`result` 与 `method` 的消息先按 request 归类。
- `isJsonRpcNotification(m)`：对象、`jsonrpc === "2.0"`、`!("id" in m)`、`method` 为字符串。
  `"id" in m` 包含 `id: undefined`（键存在）→ 该消息不是通知。
- `isJsonRpcResponse(m)`：对象、`jsonrpc === "2.0"`、`isJsonRpcId(id)`；若 `"result" in m` →
  当 `"error" in m` 时为 false，否则 true；若没有 `error` 键或 `error` 不是对象 → false；
  否则 `typeof error.code === "number" && typeof error.message === "string"`。
  （`code` 可以非有限？这里只检查 `typeof`。Rust 保持 f64。）
- `parseJsonRpcMessage(v)`：若为 request/notification/response 则返回 `v`；否则抛
  `new McpError(-32600, "Invalid JSON-RPC message")`。

`parseJsonRpcMessage` 内部用的是 **request → notification → response** 的顺序（短路 `||`）；客户端
`handleMessage` 用的是 **response → request → notification → invalid**。`parseJsonRpcMessage` 只判断
"任一谓词成立"，因此顺序不影响其结果；**顺序只在 `handleMessage` 中影响归类**，且 request 与 response
谓词**并不互斥**（`id` 存在 + `method` 字符串 + `result`/`error` 合法时两者都为真）。已实测的重叠向量
（`req`/`notif`/`resp`）：
`{jsonrpc:"2.0",id:1,method:"m",error:{code:-1,message:"x"}}` → true/false/true（客户端按 response 处理：
pending 里没有 id 1 → `emitError("Received response for unknown MCP request 1")`，**不会**当作服务端请求应答）；
`{jsonrpc:"2.0",id:1,method:"m",result:{}}` → true/false/true；`{jsonrpc:"2.0",id:1,method:"m"}` → true/false/false；
`{jsonrpc:"2.0",id:1,result:{},error:{...}}` → 全 false（有 result 又有 error）；
`{jsonrpc:"2.0",id:NaN,method:"m"}` 与 `{jsonrpc:"2.0",id:undefined,method:"m"}`（键存在）→ 全 false（`Number.isFinite(NaN)` 为假；
`"id" in m` 为真所以不是通知）。

Rust 设计：解析后保留 `serde_json::Value`，编写精确镜像上述逻辑的谓词函数（零拷贝检查），再加类型化访问器。
不要在分类前反序列化成枚举：同时带 `method` 与 `result`/`error` 的消息要按客户端的顺序（response 优先）归类。

## 10. `packages/mcp/src/protocol/types.ts`

```ts
export const LATEST_PROTOCOL_VERSION = "2025-11-25";
export const SUPPORTED_PROTOCOL_VERSIONS = [LATEST_PROTOCOL_VERSION, "2025-06-18", "2025-03-26", "2024-11-05"] as const;
export type SupportedProtocolVersion = (typeof SUPPORTED_PROTOCOL_VERSIONS)[number];
interface Implementation { name: string; version: string; title?: string }
interface Root { uri: string; name?: string }
interface ClientCapabilities {
  experimental?: Record<string, unknown>; roots?: { listChanged?: boolean };
  sampling?: Record<string, unknown>; elicitation?: Record<string, unknown>;
}
interface ServerCapabilities {
  experimental?: Record<string, unknown>; logging?: Record<string, unknown>;
  prompts?: { listChanged?: boolean }; resources?: { subscribe?: boolean; listChanged?: boolean };
  tools?: { listChanged?: boolean }; completions?: Record<string, unknown>;
}
interface InitializeParams { protocolVersion: string; capabilities: ClientCapabilities; clientInfo: Implementation }
interface InitializeResult { protocolVersion: string; capabilities: ServerCapabilities; serverInfo: Implementation; instructions?: string }
interface ProgressNotification { progressToken: string | number; progress: number; total?: number; message?: string }
interface CancelledNotification { requestId: JsonRpcId; reason?: string }
interface ToolAnnotations { title?: string; readOnlyHint?: boolean; destructiveHint?: boolean; idempotentHint?: boolean; openWorldHint?: boolean }
interface ToolExecution { taskSupport?: "forbidden" | "optional" | "required" }
interface Tool {
  name: string; title?: string; description?: string; inputSchema: Record<string, unknown>;
  outputSchema?: Record<string, unknown>; annotations?: ToolAnnotations;
  execution?: ToolExecution; _meta?: Record<string, unknown>;
}
interface ListToolsResult { tools: Tool[]; nextCursor?: string; _meta?: Record<string, unknown> }
interface Resource { uri: string; name: string; title?: string; description?: string; mimeType?: string; size?: number; annotations?: ContentAnnotations; _meta?: Record<string, unknown> }
interface ResourceTemplate { uriTemplate: string; name: string; title?: string; description?: string; mimeType?: string; annotations?: ContentAnnotations; _meta?: Record<string, unknown> }
interface ListResourcesResult { resources: Resource[]; nextCursor?: string; _meta?: Record<string, unknown> }
interface ListResourceTemplatesResult { resourceTemplates: ResourceTemplate[]; nextCursor?: string; _meta?: Record<string, unknown> }
interface ReadResourceResult { contents: (TextResourceContents | BlobResourceContents)[]; _meta?: Record<string, unknown> }
```

这些只是结构类型；客户端只校验一个子集（见 §13），其余解析后的 JSON 原样传递。Rust 应把已校验的客户端输出
（`Tool`、`Resource`、`ResourceTemplate`、`ReadResourceResult`）建模为 `serde_json::Value` 支撑的结构体，
序列化时保留未知字段（`#[serde(flatten)] extra: Map<String, Value>`），因为调用方会读取额外字段（模板中的
`icons`、`_meta`），且 `tools[]` 条目必须逐字往返（一致性套件按序列化相等比较 `inputSchema`）。

## 11. `packages/mcp/src/protocol/content.ts`

在 §10 基础上加：

```ts
interface ContentAnnotations { audience?: ("user" | "assistant")[]; priority?: number; lastModified?: string }
interface TextContent { type: "text"; text: string; annotations?; _meta? }
interface ImageContent { type: "image"; data: string; mimeType: string; annotations?; _meta? }
interface AudioContent { type: "audio"; data: string; mimeType: string; annotations?; _meta? }
interface ResourceLinkContent { type: "resource_link"; uri: string; name: string; title?; description?; mimeType?; size?; annotations?; _meta? }
interface TextResourceContents { uri: string; mimeType?: string; text: string; _meta? }
interface BlobResourceContents { uri: string; mimeType?: string; blob: string; _meta? }
interface EmbeddedResourceContent { type: "resource"; resource: TextResourceContents | BlobResourceContents; annotations?; _meta? }
type ContentBlock = TextContent | ImageContent | AudioContent | ResourceLinkContent | EmbeddedResourceContent;
interface CallToolResult { content: ContentBlock[]; structuredContent?: unknown; isError?: boolean; _meta? }
type LlmContent = { type: "text"; text: string } | { type: "image"; data: string; mimeType: string };
```

`blockToLlmContent(block)`（按 `block.type` 分支）：
- `"text"` → `{type:"text", text: block.text}`
- `"image"` → `{type:"image", data, mimeType}`
- `"audio"` → `{type:"text", text: `[audio ${block.mimeType} omitted]`}`
- `"resource_link"` → `{type:"text", text: `${block.name}: ${block.uri}`}`
- `"resource"` → 若 `"text" in resource` → 文本块，内容为 `resource.text`；否则若
  `resource.mimeType?.startsWith("image/")` → 图片，`data: resource.blob`，mimeType 相同；否则
  `{type:"text", text: `[binary resource ${resource.uri} (${resource.mimeType ?? "unknown type"}) omitted]`}`
- default（未知/缺失 type）→ `{type:"text", text: `[unsupported MCP content ${block.type}]`}`，
  `block.type` 按字符串读取（缺失 → `"undefined"`）。

`toLlmContent(result: Pick<CallToolResult,"content"|"structuredContent">): LlmContent[]`：
```
content = (result.content ?? []).map(blockToLlmContent)
if (content.length === 0 && result.structuredContent !== undefined)
   content.push({type:"text", text: JSON.stringify(result.structuredContent, null, 2)})
return content
```
- `structuredContent: undefined`（缺省）→ 无回退；显式 JSON `null` → 文本 `"null"`。
- pretty JSON 用 2 空格缩进、`\n` 换行、无尾随换行；数组/对象按 ECMAScript `JSON.stringify` 与 indent
  渲染（如 `[\n  1,\n  2\n]`、`{\n  "n": 1\n}`）。
- `content` 为空且 structuredContent 缺省 → `[]`。
- 测试中的逐字输出：`[{type:"text",text:"hello"}, {type:"image",data:"aW1n",mimeType:"image/png"},
  {type:"text",text:"[audio audio/wav omitted]"}, {type:"text",text:"a.txt: file:///a.txt"},
  {type:"text",text:"inline"}, {type:"image",data:"Yw==",mimeType:"image/png"},
  {type:"text",text:"[binary resource file:///d.bin (unknown type) omitted]"}]`。

Rust：`ContentBlock` 必须容忍未知/畸形块——在 fallback 变体中保留原始 `Value`，按
`value["type"].as_str()` 分支。`to_llm_content` 返回类型化枚举
`LlmContent::Text { text } | LlmContent::Image { data, mime_type }`，序列化为驼峰 `mimeType` 与 `type` 标签。

## 12. `packages/mcp/src/auth-provider.ts`

```ts
type McpFetch = (input: string | URL, init?: RequestInit) => Promise<Response>;
interface UnauthorizedContext { response: Response; serverUrl: URL; fetch: McpFetch; token?: string }
interface AuthProvider {
  token(): Promise<string | undefined>;
  onUnauthorized?(context: UnauthorizedContext): Promise<void>;
}
```

语义：`token()` 可能走网络刷新；transport 只在需要等待答案的请求（POST）之前调用它，`close()` 必须**不**调用。
当一个请求收到 401（或 `insufficient_scope` 的 403）时，`onUnauthorized` 对每个请求调用一次，然后用 provider
留下的凭据重试该请求一次；上下文中的 `token` 是被拒请求携带的令牌（用于检测并发刷新）。

Rust：`#[async_trait] trait AuthProvider { async fn token(&self) -> Option<String>; async fn on_unauthorized(&self, ctx: UnauthorizedContext<'_>) -> Result<(), Error> { Ok(()) } }`。
`UnauthorizedContext` 必须暴露状态、头（至少 `www-authenticate`）、响应体读取器（transport 会丢弃它）、
服务器 URL、fetch 句柄与被拒令牌。TS 传入的是活的 `Response` 与 `fetch` 函数，因此建模为
`{ status: u16, headers: HeaderMap, body: Vec<u8>（已 drain，上限未指定）, server_url: Url, fetch: Arc<dyn McpFetch>, token: Option<String> }`。
注意：TS 的 `onUnauthorized` 在响应体被丢弃**之前**收到响应；它可以读取（coding-agent 的一致性客户端会调用
`context.fetch(url)`）。为等价性必须保留完整 body；本单元测试从不读取 body。

## 13. `packages/mcp/src/client.ts` — `McpClient`

常量：`DEFAULT_REQUEST_TIMEOUT_MS = 30_000`、`MAX_LIST_PAGES = 1_000`。
状态：`"idle" | "connecting" | "connected" | "closed"`。

```ts
interface McpClientOptions extends Implementation {   // name、version 必需，title 可选
  capabilities?: ClientCapabilities;
  protocolVersion?: SupportedProtocolVersion;
  requestTimeoutMs?: number;
  roots?: readonly Root[] | (() => readonly Root[] | Promise<readonly Root[]>);
}
interface McpRequestOptions { signal?: AbortSignal; timeoutMs?: number; onProgress?: (p: ProgressNotification) => void }
```

### 13.1 构造

- `options` 冻结的浅拷贝。
- `requestHandlers` 预置：`"ping" → () => ({})`。
- 若 `options.roots` 为真值：`"roots/list" → async () => ({ roots: [...(typeof roots === "function" ? await roots() : roots)] })`
  （每次调用返回新数组；函数返回值会被 await）。

### 13.2 getter

`connectionState`、`serverInfo`（`Implementation | undefined`）、`serverCapabilities`、`instructions`、
`protocolVersion` —— 都是连接时赋值的普通字段。

### 13.3 `connect(transport)`

```
if state !== "idle": throw new Error(`Cannot connect MCP client in ${state} state`)
state = "connecting"; this.transport = transport
disposers = [transport.onMessage(handleMessage), transport.onError(emitError), transport.onClose(handleTransportClose)]
try:
  await transport.start()
  capabilities = { ...options.capabilities }
  if (options.roots && capabilities.roots === undefined) capabilities.roots = {}
  result = validateInitializeResult(await requestInternal("initialize", {
      protocolVersion: options.protocolVersion ?? LATEST_PROTOCOL_VERSION,
      capabilities,
      clientInfo: { name, version, ...(title === undefined ? {} : { title }) },
    }, {}, allowConnecting=true))
  if (!SUPPORTED_PROTOCOL_VERSIONS.includes(result.protocolVersion))
      throw new Error(`MCP server selected unsupported protocol version ${result.protocolVersion}`)
  protocolVersionValue = result.protocolVersion; serverInfoValue = result.serverInfo
  serverCapabilitiesValue = result.capabilities; instructionsValue = result.instructions
  transport.setProtocolVersion?.(result.protocolVersion)
  await notifyInternal("notifications/initialized", undefined, true)
  state = "connected"; return result
catch (error) { await close().catch(() => {}); throw error }
```

- 字段赋值顺序：protocolVersion、serverInfo、capabilities、instructions，然后 transport 的协议版本设置，
  然后 `notifications/initialized`，最后 state 置 connected（HTTP transport 只有在发出
  `notifications/initialized` 后才打开 GET 流，见 §17.10）。
- 任何失败都经 `close()` 使最终状态为 `"closed"`；`serverInfo` 保持原样（未设置）。
- `validateInitializeResult`：对象；`protocolVersion` 字符串；`capabilities` 对象；`serverInfo` 对象且
  `name`/`version` 为字符串；`instructions` 缺省或字符串。任何违反 →
  `McpError(-32600, "Invalid MCP initialize result")`。
- 不支持的版本错误是普通 `Error`，消息如上；transport 被关闭，client 用该错误 reject。
- 在 connecting/connected/closed 状态再 `connect` 抛普通 `Error`；状态不变。

### 13.4 公开请求 / 通知 API

- `request<Result>(method, params?, options = {})` → `requestInternal(..., allowConnecting=false)`。
- `notify(method, params?)` → `notifyInternal(..., false)`；发送 `{jsonrpc:"2.0", method, params?}` 并等待
  transport send。无 `id`。
- `setRequestHandler(method, handler)`：注册；返回幂等 disposer，仅当当前 handler 是同一函数时才删除。
- `onNotification(method, listener)`：按 method 设置；返回 disposer，删除该 listener，并在集合为空时删除 method 键。
- `onError(listener)`、`onClose(listener)`：设置；disposer 删除。
- `ping(options)` → `await request("ping")`。
- `listTools(options)` → `listAll("tools/list","tools",isTool,options)`。
- `listResources(options)` → `listAll("resources/list","resources",isResource,...)` 并经 `toResource` 映射。
- `listResourcesPage(cursor?, options)` → `listPage(...)` 然后 `{resources: items.map(toResource), ...pageCursor(page)}`。
- `listResourceTemplates(options)`、`listResourceTemplatesPage(cursor?, options)` 类似，键为 `"resourceTemplates"`，
  映射函数 `toResourceTemplate`。
- `readResource(uri, options)` → `validateReadResourceResult(await request("resources/read",{uri},options))`。
- `callTool(name, args?, options)` → `validateCallToolResult(await request("tools/call",
   {name, ...(args === undefined ? {} : {arguments: args})}, options))`。
- `close()`：脱离 transport（`this.transport = undefined`）、dispose 监听器、
  `markClosed(new McpConnectionClosedError())`，然后 `await transport?.close()`。幂等。

`isTool(t)` = `name` 为字符串 && `inputSchema` 为对象。
`isResource(r)` = `uri` 为字符串 &&（`name` 缺省或为字符串）。
`isResourceTemplate(t)` = `uriTemplate` 为字符串 &&（`name` 缺省或为字符串）。
`toResource(item)` = `{...item, name: item.name ?? item.uri}`（在已有键之后添加 `name`）。
`toResourceTemplate(item)` = `{...item, name: item.name ?? item.uriTemplate}`。
`pageCursor(page)` = `page.nextCursor === undefined ? {} : {nextCursor}`。

`validateListPage(method, key, value, isItem)`：
- 对象且 `value[key]` 为数组，否则 `McpError(-32600, \`Invalid MCP ${method} result\`)`。
- 每个条目都是对象且通过 `isItem`，否则 `McpError(-32600, \`Invalid entry in MCP ${method} result\`)`。
- `nextCursor`：`null` 或 `""` → undefined；否则必须是字符串，否则
  `McpError(-32600, \`Invalid MCP ${method} cursor\`)`。
- 返回 `{items, nextCursor?}`。

`validateReadResourceResult(v)`：对象且 `contents` 为数组；每个元素为对象，`uri` 为字符串，且 `text` 或 `blob`
为字符串；否则 `McpError(-32600, "Invalid MCP resources/read result")` /
`McpError(-32600, "Invalid contents in MCP resources/read result")`。

`validateCallToolResult(v)`：对象且（`content` 缺省或为数组），否则
`McpError(-32600, "Invalid MCP tools/call result")`；`content` 缺省时返回 `{...v, content: []}`。
`structuredContent` 可以是任意 JSON 值（含数组、`null`、标量）。

`listAll(method, key, isItem, options)`：
```
items = []; cursors = Set(); cursor = undefined
for pageNumber in 0..MAX_LIST_PAGES:
  page = await listPage(method,key,isItem,cursor,options)   // request(method, cursor===undefined ? undefined : {cursor})
  items.push(...page.items)
  if page.nextCursor === undefined: return items
  if cursors.has(nextCursor): throw new Error(`MCP ${method} returned duplicate cursor: ${nextCursor}`)
  cursors.add(nextCursor); cursor = nextCursor
throw new Error(`MCP ${method} exceeded ${MAX_LIST_PAGES} pages`)
```
- 空的重复 cursor 不会发生，因为 `""` 在 `validateListPage` 中变成 undefined。
- 每一页请求使用同一个 `options`（同一个 signal/timeout/progress 回调）。

### 13.5 `requestInternal(method, params, options, allowConnecting)`

```
transport = requireTransport(allowConnecting)         // 抛 McpConnectionClosedError(`MCP client is ${state}`)
if options.signal?.aborted: throw new McpAbortError()
id = nextRequestId++                                   // 从 1 开始
progressToken = options.onProgress ? id : undefined
requestParams = progressToken === undefined ? params
  : { ...params, _meta: { ...(isObject(params?._meta) ? params._meta : {}), progressToken } }
message = { jsonrpc:"2.0", id, method, ...(requestParams === undefined ? {} : {params: requestParams}) }
return new Promise((resolve, reject) => {
  entry = { resolve, reject,
    timeoutMs: options.timeoutMs ?? this.options.requestTimeoutMs ?? DEFAULT_REQUEST_TIMEOUT_MS,
    timer: undefined, signal, onAbort: () => cancelPending(id, new McpAbortError(), method !== "initialize",
        String(options.signal?.reason ?? "Aborted")),
    sendController: new AbortController(), cancellable: method !== "initialize",
    onProgress: options.onProgress, progressToken }
  pending.set(id, entry)
  if (progressToken !== undefined) progressRequests.set(progressToken, id)
  options.signal?.addEventListener("abort", entry.onAbort, {once:true})
  armTimeout(id, entry)
  transport.send(message, { signal: entry.sendController.signal }).catch(e => cancelPending(id, e, false))
})
```
- `_meta` 合并：若 `params._meta` 是对象则复制；progressToken 被添加/覆盖。若 `params` 为 undefined
  且设置了 progress 回调，请求发送 `params: {_meta:{progressToken}}`。
- `String(signal.reason ?? "Aborted")`（**已在本机 Node 26 逐项实测**）：先做 nullish 合并，再 `String()`。
  `AbortController` 在 `abort()` / `abort(undefined)` 时的 `reason` 是默认 `DOMException`，其 `String()` 为
  `"AbortError: This operation was aborted"`（name + ": " + message）；所以裸 abort 的取消通知携带
  `reason: "AbortError: This operation was aborted"`。逐项结果：
  | 调用 | `signal.reason` | 发送的 `reason` 字符串 |
  |---|---|---|
  | `abort()` | DOMException | `"AbortError: This operation was aborted"` |
  | `abort(undefined)` | DOMException | `"AbortError: This operation was aborted"` |
  | `abort(null)` | `null` | `"Aborted"`（`null` 走 `??`） |
  | `abort(0)` / `abort(false)` / `abort("")` | 对应值 | `"0"` / `"false"` / `""`（**空字符串**，不是 `"Aborted"`） |
  | `abort(NaN)` | `NaN` | `"NaN"` |
  | `abort("stop")` | `"stop"` | `"stop"` |
  | `abort({})` | 对象 | `"[object Object]"` |
  | `abort(Symbol("x"))` | symbol | `"Symbol(x)"`（`String()` 对 symbol 不抛） |
  只有 `undefined`（不可能由 `addEventListener` 路径产生）与 `null` 得到 `"Aborted"`。
  Rust 的 `CancellationToken` 没有 reason 概念；需在客户端 `PendingRequest` 中显式保存触发取消的原因字符串，
  并实现一个 `js_string_or_aborted(Option<Reason>) -> String`，其中"无 reason"映射为
  `"AbortError: This operation was aborted"`，而不是 `"Aborted"`。

实测（真实 TS 实现 + 内存 transport）的完整消息序列（可作为 Rust 集成测试的黄金样例）：
```
[{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},
   "clientInfo":{"name":"c","version":"1"}}},
 {"jsonrpc":"2.0","method":"notifications/initialized"},
 {"jsonrpc":"2.0","id":2,"method":"ping"},
 {"jsonrpc":"2.0","method":"notifications/cancelled",
   "params":{"requestId":2,"reason":"AbortError: This operation was aborted"}}]
```
被取消的请求以 `McpAbortError`（name `AbortError`，message `MCP request aborted`）reject。
注意键序：`jsonrpc`、`id`、`method`、`params`；通知为 `jsonrpc`、`method`、`params`；
`params` 内 `requestId` 在前、`reason` 在后。
- id 是数字 1,2,3,…（从不复用）。测试期望 initialize 的 `id: 1`，下一个请求 `id: 2`。
- **id 自增发生在两项检查之后**：`requireTransport(allowConnecting)` 与 `options.signal?.aborted` 都在
  `nextRequestId++` **之前**。因此"idle/closed 状态下调用"或"传入已 abort 的 signal"抛错时**不消耗 id**
  （实测：idle 下 `ping()` 失败后 initialize 仍是 1；预先 abort 的 `ping()` 失败后下一个 `ping()` 是 2，
  即没有跳过 2）。但一旦通过了这两项检查，后续任何失败（超时、transport send 拒绝、服务端 error 响应）
  都已消耗 id——id 序列中会出现空洞（实测：超时的 `tools/call` 占 3，之后 `ping` 是 4）。

### 13.6 `notifyInternal` / `requireTransport`

`requireTransport(allow)`：存在 transport 且状态为 `"connected"`（或 `allow` 且 `"connecting"`）时返回它；
否则抛 `McpConnectionClosedError(\`MCP client is ${state}\`)`。
`notifyInternal` 发送 `{jsonrpc:"2.0", method, params?}`（无 id、无超时、无跟踪）。

### 13.7 消息处理

`handleMessage(m)`：response → `handleResponse`；request → `void handleRequest(m)`；notification →
`handleNotification(m.method, m.params)`；否则 `emitError(new McpError(-32600, "Received invalid JSON-RPC message"))`。

`handleResponse(m)`：
- 按 id 查找 pending；缺失 → `emitError(new Error(\`Received response for unknown MCP request ${String(m.id)}\`))`
  （不抛；`String(id)` 对数字/字符串）。
- `removePending(id, entry)`；若 `"error" in m` → 用
  `new McpError(m.error.code, m.error.message, m.error.data)` reject；否则用 `m.result` resolve。

`handleRequest(m)`（服务端 → 客户端）：
- 无 transport → 静默返回。
- 无 handler → 发送 `{jsonrpc:"2.0", id, error:{code:-32601, message: \`Method not found: ${m.method}\`}}`；
  发送失败 → `emitError`。
- 否则创建 `AbortController`，按 id 存入 `incoming`，`await handler(m.params, {signal})`，发送
  `{jsonrpc:"2.0", id, result: result ?? {}}`。throw 时：`McpError` → `{code, message, data}`
  （data 即使 undefined 也包含 → JSON 会省略）；其他 → `{code:-32603, message: toError(error).message}`。
  发送失败 emit 错误。`finally` 删除 incoming 条目。并发重复 id 会覆盖 controller。

`handleNotification(method, params)`：若 `notifications/progress` → `handleProgress`；若
`notifications/cancelled` → `handleCancelled`；然后按注册顺序调用该 method 的每个 listener；
listener throw → `emitError(error)`（try/catch 是按 listener 的，所以抛出者之后的 listener 仍会运行）。

`handleProgress(params)`：要求对象、`isJsonRpcId(progressToken)`、`typeof progress === "number"`；
查 `requestId = progressRequests.get(token)`；requestId 缺失或 pending 条目缺失 → 返回；否则
`armTimeout(requestId, entry)`，然后 `onProgress?.(params as ProgressNotification)`（throw → emitError）。
每次进度通知都会重置超时（但只对仍存在的请求）。

`handleCancelled(params)`：对象且 `isJsonRpcId(requestId)` → `incoming.get(requestId)?.abort(params.reason)`。

`armTimeout(id, entry)`：清除已有定时器；若 `!Number.isFinite(timeoutMs) || timeoutMs <= 0` 直接返回（不设定时器）；
否则 `setTimeout(() => cancelPending(id, new McpTimeoutError(timeoutMs), cancellable, "Request timed out"), timeoutMs)`。
注意 Node 的 `setTimeout` 会把超过 2^31-1 的延迟钳到 **1 ms**（已实测 `setTimeout(fn, 2**31)` 立即触发并打印
`TimeoutOverflowWarning`）；因此 `timeoutMs = 1e10` 实际会**立刻**超时并发出 `McpTimeoutError`。Rust 移植若用
`tokio::time::sleep(Duration::from_secs_f64(1e10))` 会等待而非立即超时——这是一处有意的行为差异，需在文档中
标注（或复刻钳位）。`Infinity` 则在 `Number.isFinite` 检查处被挡下（不设定时器 = 永不超时）。

`cancelPending(id, error, notifyServer, reason?)`：无条目则返回；remove；用 `error` reject；
若 `notifyServer && transport` 则发送 `{jsonrpc:"2.0", method:"notifications/cancelled",
params:{requestId: id, ...(reason ? {reason} : {})}}`（发送失败 → emitError）。`initialize` 从不取消
（`cancellable=false`），因此超时的 initialize 不发送取消通知。

`removePending(id, entry)`：从 pending 删除；清除定时器；删除 `progressRequests[entry.progressToken]`；
移除 abort 监听；`entry.sendController.abort()`（使正在等待 auth 刷新的 transport 不再派发该消息，见 §17.6）。

`rejectPending(error)`：对每个 pending 条目，remove 后用同一错误 reject。
`handleTransportClose()` → `markClosed(new McpConnectionClosedError())`。
`markClosed(error)`：`wasClosed = state === "closed"`；state = `"closed"`；rejectPending(error)；
用 `error` abort 所有 `incoming` controller；清空 incoming；若 `wasClosed` 返回；按快照顺序调用 close
listener，listener throw → emitError。因此即使 `close()` 与 transport close 都发生，close listener 在第一次
`markClosed` 后最多触发一次。
`emitError(v)`：先 `toError(v)`，再调用每个 error listener（**无** try/catch，所以抛出的 listener 会中断循环
并把异常传播给调用方；测试不抛）。保持此行为（Rust 中"捕获并忽略"是偏差；建议遍历快照时传播）。
`disposeTransportListeners()`：调用并清空所有 disposer。

### 13.8 可观察的时序 / 异步要点

- `request()` 在注册 pending 条目并设置定时器之后才返回 Promise；`transport.send` 在 promise executor 内
  同步调用。
- transport 的 `send` rejection（例如 HTTP 401 分类错误）会用该错误 reject pending 请求，且不通知服务端。
- `close()` 时若有 pending 请求，全部用 `McpConnectionClosedError("MCP connection closed")`
  （默认消息）reject，并用该错误作为 abort reason 中止正在处理的 incoming 服务端请求。
- close 后无法重连（`connect` 要求 idle）。

## 14. `packages/mcp/src/transports/transport.ts`

```ts
export const DEFAULT_MAX_MESSAGE_BYTES = 16 * 1024 * 1024;
type McpTransportMessageListener = (message: JsonRpcMessage) => void;
type McpTransportErrorListener = (error: Error) => void;
type McpTransportCloseListener = () => void;
interface McpTransportSendOptions { signal?: AbortSignal }
interface McpTransport {
  start(): Promise<void>;
  send(message: JsonRpcMessage, options?: McpTransportSendOptions): Promise<void>;
  close(): Promise<void>;
  onMessage(l): () => void; onError(l): () => void; onClose(l): () => void;
  setProtocolVersion?(version: string): void;
}
abstract class TransportEvents {
  onMessage/onError/onClose → 加入 Set，返回 disposer
  protected emitMessage(m): 调用每个 listener
  protected emitError(e): 先 toError 再调用每个 listener
  protected emitClose(): 每个 transport 实例最多触发一次
}
```

Rust：`#[async_trait] trait McpTransport { async fn start(&self) -> Result<()>; async fn send(&self, message: Value, options: SendOptions) -> Result<()>; async fn close(&self) -> Result<()>; fn on_message(&self, ...) -> Subscription; ...; fn set_protocol_version(&self, _: &str) {} }`。
监听器集合用 `Mutex<Vec<...>>` + 一次性 `Subscription` 句柄；`emit_close` 的"最多一次"用
`AtomicBool::swap(true)`。

## 15. `packages/mcp/src/transports/stdio.ts` — `StdioTransport`

常量：`DEFAULT_MAX_STDERR_BYTES = 64*1024`、`DEFAULT_CLOSE_TIMEOUT_MS = 2_000`、
`STDIN_CLOSE_GRACE_MS = 500`、`USE_PROCESS_GROUPS = process.platform !== "win32"`。
模块状态：`liveProcessGroups: Set<number>`、`exitHookInstalled: boolean`；一个 `process.once("exit")` 钩子用
SIGTERM 杀掉每个存活进程组（`process.kill(-pid,"SIGTERM")`，错误忽略）。

```ts
interface StdioTransportOptions {
  command: string; args?: readonly string[]; cwd?: string; env?: Record<string,string>;
  inheritEnv?: boolean; stderr?: "pipe" | "inherit"; onStderr?: (chunk: string) => void;
  maxMessageBytes?: number; maxStderrBytes?: number; closeTimeoutMs?: number;   // 默认 2000
}
class StdioTransport extends TransportEvents implements McpTransport {
  readonly options; private child; private stdoutBuffer = Buffer.alloc(0); private stderrBuffer = Buffer.alloc(0);
  private started = false; private closed = false;
  get pid(): number | undefined
  get stderr(): string     // stderrBuffer.toString("utf8")（最后 maxStderrBytes 字节）
  start(); send(message); close(); private handleStdout(chunk); private handleStderr(chunk);
}
```
构造函数：`Object.freeze({ ...options, args: options.args ? [...options.args] : undefined })`。

### 15.1 `start()`

- 已启动 → `new Error("MCP stdio transport already started")`；已关闭 → `McpConnectionClosedError()`。
- 在 spawn 之前置 `started = true`。
- env：`inheritEnv === false ? {...options.env} : {...process.env, ...options.env}`（字符串值，后者覆盖前者）。
  Rust：`std::env::vars()`（lossy UTF-8）合并；非 UTF-8 差异需文档化。
- spawn：`crossSpawn(command, args ?? [], { cwd, env, stdio: ["pipe","pipe", stderr === "inherit" ? "inherit" : "pipe"],
  windowsHide: true, detached: USE_PROCESS_GROUPS })`。`args ?? []`；`undefined` 的 cwd/env 直接透传（Node 默认）。
- POSIX 上：安装 exit hook，把 pid 加入 live groups，在子进程 `exit` 时移除。
- stdout `data` → `handleStdout`；stdout `error` → emitError；stdin `error` → 未关闭时 emitError；
  stderr `data` → `handleStderr`（仅当是 pipe）；stderr `error` → emitError。
- 子进程 `close` → `this.child = undefined`；若剩余 stdout 缓冲（utf8、trim 后）非空 →
  `emitError(new Error("MCP stdio server closed with an incomplete JSON-RPC message"))`；清空缓冲；`emitClose()`。
- 等待 spawn：`spawn` 事件 resolve，`error` 事件 reject（并移除另一监听器）。spawn 之后安装常驻 `error`
  监听器：未关闭时 emit。
- spawn 失败会留下 `started = true`、`closed = false`；client 的 connect catch 会关闭它。

### 15.2 `send(message, {signal}?)`

- 要求 `started && !closed && child.stdin?.writable`，否则 `McpConnectionClosedError()`。
- 载荷 `` `${JSON.stringify(message)}\n` ``（紧凑 JSON，单个 `\n`，无 CR）。
- `signal` 被 stdio transport 忽略（为接口对齐而接受）。
- 写回调报告成功时 resolve；写错误时 reject。背压由写回调自然处理。

### 15.3 `close()`

- 已关闭 → 返回。
- 置 `closed = true`。无子进程 → `emitClose()` 并返回。
- 子进程已退出（`exitCode !== null || signalCode !== null`）→ `child.stdin?.end()` 并返回
  （注意：此处**不** `emitClose()`——之前的 `close` 事件已触发；若子进程在未触发 close 事件的情况下退出，
  则 close listener 不会再次调用）。
- 否则以两个定时器等待子进程 `close`：
  `grace = Math.min(500, closeTimeoutMs)`；在 `grace` 时 → `killProcessTree(child,"SIGTERM")`；
  在 `grace + closeTimeoutMs` 时 → `killProcessTree(child,"SIGKILL")`；立即 `child.stdin.end()`。
  子进程 `close` 时：清除两个定时器，再次 `killProcessTree(child,"SIGTERM")`（回收忽略 stdin EOF 的孙进程），
  resolve。
- `killProcessTree`：win32 上：若 pid 已定义**且 `child.exitCode === null`**（只检查退出码，不检查 `signalCode`），
  `spawn("taskkill",["/pid",String(pid),"/T","/F"],{stdio:"ignore",windowsHide:true}).on("error", () => {})`
  且返回（未定义 pid 时落到下面的 `child.kill`）。
  否则若 USE_PROCESS_GROUPS 且 pid 已定义：`process.kill(-pid, signal)`；抛错则回退 `child.kill(signal)`
  （错误吞掉）。否则 `child.kill(signal)`（同样 try/catch 吞错）。
  `installExitHook()` 只安装一次（模块级 `exitHookInstalled`），`process.once("exit")` 对
  `liveProcessGroups` 中每个 pid 尝试 `process.kill(-pid, "SIGTERM")`（错误吞掉）；非 POSIX 平台不安装。
- 测试契约：`closeTimeoutMs: 100` 时，关闭一个忽略 stdin EOF 与 SIGTERM 的服务器耗时 < 5000 ms，
  且之后孙进程 PID 已死（进程组 SIGKILL）。

### 15.4 `handleStdout(chunk)`

```
stdoutBuffer = concat(stdoutBuffer, chunk)
max = options.maxMessageBytes ?? 16 MiB
loop:
  nl = stdoutBuffer.indexOf(0x0A)
  if nl < 0:
     if stdoutBuffer.length > max: stdoutBuffer = empty; emitError(new Error(`MCP stdio message exceeds ${max} bytes`))
     return
  line = stdoutBuffer[0..nl]; stdoutBuffer = stdoutBuffer[nl+1..]
  if line.length > max: emitError(new Error(`MCP stdio message exceeds ${max} bytes`)); continue
  text = line.toString("utf8").replace(/\r$/, "")      // 去掉一个尾随 CR
  if (!text.trim()) continue                            // 跳过空白行
  try emitMessage(parseJsonRpcMessage(JSON.parse(text))) catch (e) emitError(e)
```
- 上限基于原始行字节（不含 `\n`）。
- 非法 UTF-8 被 Node 解码器替换为 U+FFFD。
- `JSON.parse` 错误作为 error 发出（如 `SyntaxError: Unexpected token ...`）；解析失败不关闭 transport。
  Node 的错误文本在 Rust 中改为 serde_json 消息（偏差，无测试断言该文本）。
- 一个 chunk 中的多条消息按顺序同步派发。

### 15.5 `handleStderr(chunk)`

追加到 `stderrBuffer`；若长度超过 `maxStderrBytes ?? 64 KiB`，保留**最后** `maxStderrBytes` 字节
（`subarray(-max)`）；用 **chunk**（不是缓冲）调用 `onStderr?.(chunk.toString("utf8"))`。
`get stderr()` 返回缓冲的 UTF-8 解码（尾部可能截断多字节字符）。

### 15.6 依赖此 transport 的协议细节

- `JSON.stringify(message)` 键序遵循对象插入顺序（请求：jsonrpc、id、method、params；通知：jsonrpc、method、params；
  响应：jsonrpc、id、result/error）。Rust 必须发出相同键序（使用 `serde_json::Map` 插入顺序，或字段顺序明确的
  结构体）。
- 测试中 `process.execPath` 是 node 二进制；fixture 向 stdout 每行打印一个 JSON，日志写 stderr。

### 15.7 Rust 设计要点

- `tokio::process::Command`（`kill_on_drop(false)`，显式管理生命周期）；Unix 上用
  `pre_exec(|| { libc::setsid(); Ok(()) })` 建立独立进程组，`libc::killpg(pid, SIGTERM/SIGKILL)` 杀组；
  失败回退 `child.start_kill()`。
- 测试环境无 Windows 需求时，`taskkill` 分支可以返回 `Unsupported` 或直接实现（跨平台目标需说明）。
- 退出钩子：`std::sync::Once` + `libc::atexit` 或进程退出时 Drop 全局注册表；Rust 无 Node 的 `exit` 事件，
  行为差异是"如果宿主进程被 SIGKILL，则没有清理"（Node 同样无法处理 SIGKILL）。
- stdout 读循环用 `tokio::io::BufReader` + `read_until(b'\n')` 才能精确区分"行超长"与"缓冲超长"；
  注意上限按原始字节计算，`\r` 在字节上限之后才剥离。
- stderr 尾部缓冲用 `VecDeque<u8>` 或 `BytesMut` + 截断；`stderr()` 用 `String::from_utf8_lossy`。

## 16. `packages/mcp/src/transports/in-memory.ts` + `src/testing/index.ts`

```ts
class InMemoryTransport extends TransportEvents implements McpTransport {
  private peer; private started = false; private closed = false;
  connectPeer(peer) { if (this.peer) throw new Error("In-memory MCP transport already has a peer"); this.peer = peer }
  async start() { if (closed) throw new McpConnectionClosedError(); started = true }
  async send(message) {
    if (!started || closed) throw new McpConnectionClosedError();
    peer = this.peer; if (!peer?.started || peer.closed) throw new McpConnectionClosedError("In-memory MCP peer is not connected");
    copy = structuredClone(message); queueMicrotask(() => peer.deliver(copy));
  }
  async close() { if (closed) return; closed = true; emitClose(); await this.peer?.close() }
  override emitError(error) { super.emitError(error) }      // 测试可用
  private deliver(message) { if (closed) return; emitMessage(message) }
}
function createInMemoryTransportPair(): { client, server }  // 两者互相连接
```
- 投递是异步的（microtask），消息被深克隆，因此发送方后续修改不影响接收方，且 `send` 在投递之前 resolve。
- `close()` 递归关闭 peer；对已关闭 transport 调 `start()` 抛错。
- `emitError` 在实例上公开（测试调用 `clientTransport.emitError(...)`）。
- `src/testing/index.ts` 恰好导出 `createInMemoryTransportPair` 与 `InMemoryTransport`。

Rust：`queueMicrotask` → 若保证每个发送方的顺序，用 `tokio::spawn`/`yield_now` 在投递前即可；
测试依赖"send resolve，随后投递发生在之后的 task"。用 unbounded channel 发往 peer，在 peer 的接收 task 中投递。
`structuredClone` 对 JSON 消息等价于 `Value::clone`。

## 17. `packages/mcp/src/transports/streamable-http.ts` — SSE + `StreamableHttpTransport`

常量：`MAX_ERROR_BODY_BYTES = 8 * 1024`、`ERROR_MESSAGE_BODY_CHARS = 500`、
`DEFAULT_RECONNECT_INITIAL_DELAY_MS = 1_000`、`DEFAULT_RECONNECT_MAX_DELAY_MS = 30_000`、
`DEFAULT_RECONNECT_MAX_RETRIES = 5`。

### 17.1 `consumeSseStream(stream, options)` — 导出

```ts
interface SseEvent { event?: string; data: string; id?: string }
interface ConsumeSseOptions {
  maxEventBytes?: number;
  onEvent(event: SseEvent): void;
  onId?(id: string): void;          // 每个 `id` 字段都会调用，包括没有 data 的事件
  onRetry?(delayMs: number): void;  // 每个合法 `retry` 字段，单位毫秒
}
```

算法（WHATWG SSE 子集，精确如下）：
- `reader = stream.getReader()`，`decoder = new TextDecoder()`，`buffered = ""`，
  `eventName = undefined`，`eventId = undefined`，`dataLines = []`，`dataBytes = 0`，
  `maxEventBytes = options.maxEventBytes ?? 16 MiB`。
- `dispatch()`：若 `dataLines.length === 0` → 重置 eventName/eventId 并返回（不调用 onEvent；只有 `id` 行而无
  data 时，`onId` 在解析时已即时调用，且不会延续到下一事件，因为这里被清除）。否则 `data = dataLines.join("\n")`；
  `onEvent({...eventName ? {event} : {}, data, ...eventId ? {id} : {}})`；重置 eventName、eventId、
  dataLines、dataBytes。空字符串的 event/id 被省略（`eventId ? ...` 对 `""` 为假）。
- `processLine(rawLine)`：剥掉一个尾随 `\r`；若行为 `""` → dispatch 并返回；若以 `":"` 开头 → 忽略（注释）；
  `colon = line.indexOf(":")`；`field = colon < 0 ? line : line.slice(0,colon)`；
  `value = colon < 0 ? "" : line.slice(colon+1)`；若 value 以单个空格开头，去掉它。
  - `field === "data"`：`dataBytes += Buffer.byteLength(value) + (dataLines.length > 0 ? 1 : 0)`；
    若 `dataBytes > maxEventBytes` 抛 `new Error(\`MCP SSE event exceeds ${maxEventBytes} bytes\`)`；
    push value。
  - `field === "event"` → `eventName = value`（后者覆盖；允许空字符串，dispatch 时被省略）。
  - `field === "id" && !value.includes("\0")` → `eventId = value`；`onId?.(value)`。
  - `field === "retry" && /^\d+$/.test(value)` → `onRetry?.(Number(value))`。
  - 其他字段忽略。
- 读循环：每个 chunk，`buffered += decoder.decode(value, {stream:true})`；然后 while 存在 `\n`，
  处理换行前的行（在 processLine 中剥 CR）；处理完所有完整行后，若 `Buffer.byteLength(buffered) > maxEventBytes`
  抛 `new Error(\`MCP SSE event exceeds ${maxEventBytes} bytes\`)`（防止未换行的超长行）。
- 结束时：`buffered += decoder.decode()`；若 `buffered` 非空 → `processLine(buffered)`；然后 `dispatch()`。
  `finally { reader.releaseLock() }`。
- `onEvent`/`onId`/`onRetry` 的异常向外传播（无 catch）；reader 锁总是释放。

逐字测试：chunks `': keepalive\r\nid: 7\r\ndata: {"one":\r\n'` 和 `'data: 1}\r\n\r\n'`
→ 恰好一个事件 `{id:"7", data:'{"one":\n1}'}`。

实现上验证过的边界（可直接作为额外单元测试向量）：
```
': keepalive\r\nid: 7\r\ndata: {"one":\r\n' + 'data: 1}\r\n\r\n'
  → events [{"data":"{\"one\":\n1}","id":"7"}]；onId ["7"]；onRetry []
'id: a\n\n' + 'id: b\ndata: x\n\n'      → onId ["a","b"]；events [{"data":"x","id":"b"}]（无 data 的 id 不产生事件）
'id: \ndata: x\n\n'                      → onId [""]；events [{"data":"x"}]（空 id 被省略）
'id: a\u0000b\ndata: x\n\n'              → onId []（含 NUL 的 id 被忽略）；events [{"data":"x"}]
'retry: 1x\ndata: x\n\n'                 → onRetry []（非纯数字忽略）
'data\n\n'                                → events [{"data":""}]（无冒号字段：field=data，value=""）
'event: custom\ndata: x\n\n'             → events [{"event":"custom","data":"x"}]
'data: y'（无换行，EOF）                    → events [{"data":"y"}]（EOF 冲刷）
': hi\n\n'                                → 无事件（纯注释）
'id: a\ndata: 1\n\n' + 'data: 2\n\n'     → 第二个事件没有 id（dispatch 后 id 被清除）
```
逐字测试：无空行地无限发送 `data: xxxxxxxxxxxxxxxx\n` 行，`maxEventBytes:256` 时在少于 100 次 pull 后
以 `MCP SSE event exceeds 256 bytes` reject（测试同时断言 `sent < 100`，即超限在有界读次数内被发现）。

### 17.2 错误与辅助函数

```ts
class McpHttpError extends Error { readonly status: number; readonly body: string;
  constructor(status, message, body = "") { super(message); this.name = "McpHttpError"; this.status = status; this.body = body } }
class McpAuthRequiredError extends McpHttpError { readonly wwwAuthenticate: string | null;
  constructor(response: Response, body = "") { super(401, "MCP server requires authentication", body); this.name="McpAuthRequiredError";
    this.wwwAuthenticate = response.headers.get("www-authenticate") } }
class McpSessionExpiredError extends McpHttpError {
  constructor(body = "") { super(404, "MCP session expired", body); this.name="McpSessionExpiredError" } }
```
- `contentType(response)` = `headers.get("content-type")?.split(";",1)[0]?.trim().toLowerCase()`。
- `needsAuthorization(response)`：401 为 true；403 时仅当
  `/(?:^|[\s,])error="?insufficient_scope"?/i` 匹配 `www-authenticate` 头（头缺失视为 `""`）。
  注意 `xerror="insufficient_scope"` **不**匹配；`Bearer,error="insufficient_scope"` 匹配；引号可选。
  无尾边界：`error=insufficient_scopeX` 也匹配；`error = "insufficient_scope"`（`=` 旁有空格）不匹配
  （完整实测向量见 §27）。Rust 用 `regex` crate 时需写成 `(?i)(?:^|[\\s,])error="?insufficient_scope"?`
  并确认 Unicode 空白语义（JS 的 `\s` 含 `﻿`，Rust `\s` 不含；实际头值不含 BOM，可忽略但需注释）。
- `isTransientStatus(status)`：`408 || 429 || >= 500`。
- `describeHttpFailure(status, body)`：`text = body.trim()`；`snippet = text.length > 500 ?
  text.slice(0,497)+"..." : text`；返回 `MCP HTTP request failed with status ${status}${snippet ? `: ${snippet}` : ""}`。
  （`slice` 按 UTF-16 代码单元计数。）
- `discard(response)`：`response.body?.cancel().catch(() => {}) ?? Promise.resolve()`。

### 17.3 选项与字段

```ts
interface StreamableHttpReconnectOptions { initialDelayMs?: number; maxDelayMs?: number; maxRetries?: number }
interface StreamableHttpTransportOptions {
  url: string | URL; headers?: Record<string, string>; fetch?: McpFetch;
  openGetStream?: boolean;        // 默认 true
  maxMessageBytes?: number;
  authProvider?: AuthProvider;
  reconnect?: StreamableHttpReconnectOptions;
}
class StreamableHttpTransport extends TransportEvents implements McpTransport {
  readonly url: URL; readonly options;
  get sessionId(): string | undefined
}
```
- 构造函数冻结 options 的浅拷贝（headers 被复制），`url = new URL(options.url)`，
  `fetch = (input, init) => fetch(input, init)`，其中 `fetch = options.fetch ?? globalThis.fetch`
  ——**不带接收者**调用（Cloudflare Workers 的 `Illegal invocation` 规避；测试注入一个严格 fetch，
  当 `this` 既不是 undefined 也不是 globalThis 时抛 `TypeError("Illegal invocation")`）。
- 字段：`controller = new AbortController()`（close 时中止所有出站 fetch）、`started`、`closed`、
  `sessionIdValue`、`protocolVersion`（经 `setProtocolVersion` 设置）、`getStreamStarted`、`lastToken`。
- `start()`：已启动 → `Error("MCP Streamable HTTP transport already started")`；已关闭 →
  `McpConnectionClosedError()`；否则 `started = true`。
- `setProtocolVersion(v)` 设置字段；之后每个请求都发送 `MCP-Protocol-Version`。

### 17.4 `send(message, {signal}?)`

```
if (!started || closed) throw McpConnectionClosedError()
response = await authorizedFetch("POST", { headers: { accept: "application/json, text/event-stream",
  "content-type": "application/json" }, body: JSON.stringify(message) }, signal)
await checkResponse(response)
captureSession(response)                       // 若存在 mcp-session-id 头则替换会话
if (!isJsonRpcRequest(message)) {
  await discard(response)                      // 通知与响应：忽略任何 body
  if ("method" in message && message.method === "notifications/initialized") startGetStream()
  return
}
if (response.status === 202 || response.status === 204)
  throw new McpHttpError(status, `MCP server accepted request ${message.method} without a response`)
type = contentType(response)
if (type === "application/json") { body = await response.json(); for (item of Array.isArray(body) ? body : [body]) emitMessage(parseJsonRpcMessage(item)); return }
if (type === "text/event-stream" && response.body) { void this.consumeResponseStream(response.body, message.id); return }
await discard(response)
throw new McpHttpError(response.status, `Unsupported MCP response content type: ${type ?? "missing"}`)
```
- JSON 路径接受单个对象或消息数组（此处容忍批量响应，尽管请求不支持批量）。
- SSE 路径在响应到达之前返回；请求的 promise 通过 `emitMessage` → client `handleResponse` 结算。
  失败的流会合成一个错误响应。
- 请求头：先调用方提供的 `options.headers`，再每次请求的额外头（覆盖），然后 `Mcp-Session-Id`（若已知）、
  `MCP-Protocol-Version`（若已设置）、`Authorization: Bearer <token>`（若有 token）。头名大小写不敏感，
  值被 `Headers` 处理（WHATWG / undici，**已在本机 Node 26 实测**）：只**裁剪首尾** HTTP 空白
  （空格、`\t`、`\r`、`\n`），**内部空白原样保留**（`"  x   y  "` → `"x   y"`，三个空格不变；
  `"a\tb"` 原样保留 tab）。同名 `set` 大小写不敏感地覆盖；`append` 用 `", "` 连接
  （`append("v2")` 到 `"v1"` → `"v1, v2"`）；名字含空格或值含 `\n`/`\r`/`\0` 时抛 `TypeError`。
  **注意**：本规格早期版本错误地说"内部连续空白折叠为单个空格"——实测**不成立**，不要实现折叠。
  Rust 的 `HeaderMap` 相反：`HeaderValue::from_str` 拒绝首尾空白，且保留内部空白；需要一层 JS 兼容包装
  （先裁剪首尾空白再构造；保留内部空白；拒绝 `\n`/`\r`/`\0` 与非法头名，抛 TypeError 等价错误）。
- GET 头：`accept: text/event-stream`（恢复时加 `last-event-id`）。
- DELETE（close）：`buildHeaders({}, lastToken)` + `AbortSignal.timeout(1000)`，错误忽略。

### 17.5 `close()`

```
if closed return; closed = true; controller.abort()
if (started && sessionId) { try { response = await fetch(url, {method:"DELETE", headers: buildHeaders({}, lastToken),
    signal: AbortSignal.timeout(1000)}); await discard(response) } catch {} }
emitClose()
```
- 从不调用 `authProvider.token()`（复用 last token；测试断言 token() 调用次数不变）。
- 不等待刷新；忽略所有错误；1 秒超时。

### 17.6 `authorizedFetch(method, init, signal?)`

```
onUnauthorized = options.authProvider?.onUnauthorized?.bind(options.authProvider)
for attempt = 0;; attempt++:
  { headers, token } = await this.headers(init.headers)      // token = await authProvider?.token(); lastToken = token
  if (signal?.aborted) throw new McpAbortError()
  response = await fetch(url, { method, headers, body: init.body, signal: controller.signal })
  if (attempt > 0 || !onUnauthorized || !needsAuthorization(response)) return response
  try { await onUnauthorized({ response, serverUrl: url, fetch: this.fetch, token }) }
  finally { await discard(response) }
```
- 只尝试一次恢复。`signal` 检查发生在每次 fetch **之前**，因此在等待 token 期间被中止的请求永远不会派发
  （测试：被 abort/超时的请求不执行；只执行 `kept`）。
- 出站 fetch 使用 `controller.signal` 而非调用方 signal；取消调用方不会中止在途 HTTP 交换（close 才会）。
- `headers(extra)`：从 provider 获取 token（可能刷新）；返回 `Headers` 对象与 token。
- `buildHeaders(extra, token)`：`new Headers(options.headers)`；逐个设置 extra；设置 session id；
  设置 protocol version；`Authorization: Bearer <token>`。
- `captureSession`：响应的 `mcp-session-id`（任意非空）更新 session id。
- `discard(response)`：`response.body?.cancel().catch(()=>{})`。

### 17.7 `checkResponse(response)`

```
if response.ok return
text = (await response.text().catch(() => "")).slice(0, 8192)
body = `${text} `.slice(0, -1)        // 复制，避免 V8 slice 保留整个 body
if status === 401 throw new McpAuthRequiredError(response, body)
if status === 404 && sessionId throw new McpSessionExpiredError(body)
throw new McpHttpError(status, describeHttpFailure(status, body), body)
```
- `response.ok` 是状态 200–299。
- body 摘录上限 8192 UTF-16 代码单元；尾部空格技巧等价于 `text.slice(0, 8192)`。
- 无 session id 的 404 是普通 `McpHttpError`；401 总是 `McpAuthRequiredError`。
- `wwwAuthenticate` 从活的响应头读取（缺失为 null）。

### 17.8 `consumeSse(stream, cursor, onMessage?)`

用 `maxEventBytes = options.maxMessageBytes ?? 16 MiB` 包装 `consumeSseStream`；
`onId` → `cursor.lastEventId = id`；`onRetry` → `cursor.retryMs = delay`；
`onEvent`：`cursor.received = true`；若 `!event.data.trim()` 或（`event.event !== undefined &&
event.event !== "message"`）→ 返回；否则 `parseJsonRpcMessage(JSON.parse(event.data))`（解析错误 →
`emitError` 并返回）；然后 `onMessage?.(message)` 与 `emitMessage(message)`。
`StreamCursor { lastEventId?: string; retryMs?: number; received: boolean }`。

### 17.9 `consumeResponseStream(body, requestId)`（单请求 SSE）

```
cursor = {received:false}; answered = false
onMessage = m => { if isJsonRpcResponse(m) && m.id === requestId: answered = true }
stream = body; failure = undefined
for (attempt = 0;;) {
  if (stream) { try { await consumeSse(stream, cursor, onMessage); failure = undefined } catch (e) { failure = e } }
  if (answered || closed) return
  if (failure !== undefined && !isRetryable(failure)) break
  if (cursor.lastEventId === undefined || attempt >= maxRetries()) break
  if (cursor.received) attempt = 0
  cursor.received = false
  if (!(await sleep(reconnectDelay(attempt++, cursor.retryMs)))) return
  try { stream = await openSseStream(cursor.lastEventId) }
  catch (e) { failure = e; if (!isRetryable(e)) break; stream = undefined }
}
if (closed) return
reason = failure === undefined ? "stream ended without a response" : toError(failure).message
emitMessage({ jsonrpc:"2.0", id: requestId, error: { code:-32603, message: `MCP response stream failed: ${reason}` } })
```
- 恢复要求服务器至少发送过一个 `id`；否则流结束后请求立即失败。
- 流至少投递过一个事件（`received`）时 `attempt` 重置为 0——收到数据但未得到答案的流，每轮最多重试 maxRetries 次。
- 重新打开时的瞬时错误计为一次失败尝试；非可重试的打开错误直接跳出。
- 测试的逐字消息：`MCP response stream failed: stream ended without a response`；
  对 `data: not json`，JSON 解析错误经 `emitError` 发出（1 个错误），流结束 → 请求以
  `MCP response stream failed: stream ended without a response` reject。
- 服务器用带 `id` 与 `data:`（空 data）的 priming 事件设置 lastEventId 并计入 `received`；
  `retry: 5` 设置延迟 → 用 `Last-Event-ID: 1` 恢复。

### 17.10 `startGetStream` / `runGetStream`

`startGetStream()`：当 `openGetStream === false`、已启动或已关闭时跳过；置 started；`void runGetStream()`。

`runGetStream()`：
```
cursor = {received:false}
for (attempt = 0; !closed;) {
  try {
    stream = await openSseStream(cursor.lastEventId)
    if (!stream) return                              // 405 → 没有 GET 流
    openedAt = Date.now()
    await consumeSse(stream, cursor)
    if (cursor.received || Date.now() - openedAt > maxDelay()) attempt = 0
  } catch (e) {
    if (closed) return
    if (!isRetryable(e)) { emitError(e); return }
  }
  cursor.received = false
  if (attempt >= maxRetries()) { emitError(new Error("MCP server-to-client stream dropped and could not be reopened")); return }
  if (!(await sleep(reconnectDelay(attempt++, cursor.retryMs)))) return
}
```
- 存活时间超过 `maxDelayMs` 的流即使没事件也算健康。
- 测试中的重连：初始延迟 1 ms；第一次 GET 无 `last-event-id`，第二次发送 `g1`
  （断言 `[undefined, "g1"]`）。
- GET 流只在成功的 `notifications/initialized` 发送之后启动（测试断言 GET 顺序在该 POST 之后）。

### 17.11 `openSseStream(lastEventId)`

```
response = await authorizedFetch("GET", { headers: { accept: "text/event-stream", ...(lastEventId === undefined ? {} : {"last-event-id": lastEventId}) } })
if (response.status === 405) { discard; return undefined }
await checkResponse(response); captureSession(response)
type = contentType(response)
if (type !== "text/event-stream" || !response.body) { discard; throw new McpHttpError(response.status, `Unsupported MCP GET response content type: ${type ?? "missing"}`) }
return response.body
```

### 17.12 重试分类与退避

`isRetryable(error)`：
- `McpHttpError` → `isTransientStatus(status)`（408/429/≥500）。
- `TypeError` → true（fetch 网络失败，包括 body 中途断开连接）。
- 否则对象带字符串 `code` 且以 `"E"` 或 `"UND_ERR"` 开头 → true（Node 网络错误码）。
- 其他一律 false（认证、会话、协议错误）。

`reconnectDelay(attempt, serverDelayMs)`：若定义了 `serverDelayMs` 则用它；否则
`Math.min((options.reconnect?.initialDelayMs ?? 1000) * 2 ** attempt, maxDelay())`。
`maxDelay()` = `options.reconnect?.maxDelayMs ?? 30000`；`maxRetries()` = `...maxRetries ?? 5`。

`sleep(ms)`：transport 在等待期间关闭时 resolve false（先检查 `controller.signal.aborted`，然后设置定时器与
abort 监听；`timer.unref?.()` 使空闲重连不会阻止进程退出）。延迟后 resolve true。

Rust：在 `tokio::select!` 中与 `controller.cancelled()` 一起 `tokio::time::sleep`；没有 unref 概念。
因为不持有文件描述符/线程，行为等价。

## 18. `packages/mcp/src/oauth/*`

### 18.1 `oauth/types.ts` — 结构化校验

所有 parse 函数抛普通 `Error("Invalid <name>")`；标量字段的 `null` 与 `""` 视为缺省（`absent(v)`）；字符串数组
必须每项为字符串。**但 `optionalStrings` 的缺省规则更窄**：只有 `undefined`/`null` 视为缺省，**`""` 会抛
`Invalid <name>`**（`optionalString` 与 `optionalUrl` 用 `absent`，`optionalStrings` 不用）。已实测：
`parseProtectedResourceMetadata({resource:"https://a", scopes_supported:""})` → `Invalid scopes_supported`；
`scopes_supported: []` / `redirect_uris: null` → `[]`（保留空数组，不变成 undefined）。
`requiredString` 对空串**抛错**（`Invalid <name>`）；`optionalString` 的 `null`/`""` → `undefined`
（`client_secret: ""` → 省略）。
`expires_in` 走 `Number()`，所以 `[]` → `0`、`true` → `1`、`"0x10"` → `16`、`"1e3"` → `1000`、
超长数字串（`Number` 为 `Infinity`）→ `Invalid expires_in`（已实测）；`null`/`""` 是 `undefined`，
因此**不会**出现"`Number(null) === 0` 导致立刻过期"的陷阱。数字字段（`client_id_issued_at` 等）用
`typeof === "number"` 判定，`NaN` 也算 number（会保留）。

注意：本文件中的 `parseProtectedResourceMetadata`、`parseAuthorizationServerMetadata`、`parseOAuthTokens`、
`parseClientInformation`、`isHttpUrl` **不**由 `./oauth` 子路径导出（见 §19.2 的导出清单），它们只在包内使用；
`OAuthChallenge` 等类型则被导出。

- `isHttpUrl(url)`：协议为 `http:` 或 `https:`。
- `safeUrl(v, name)`：先 `requiredString`（非空字符串），再 `URL.canParse` 且 http(s)，否则
  `Error(\`Invalid ${name}\`)`。
- `object(v, name)`：非数组对象，否则 `Error(\`Invalid ${name}\`)`。
- `compact(v)`：丢弃值为 `undefined` 的自有可枚举键。
- `parseProtectedResourceMetadata(v)`：对象，名为 `OAuth protected resource metadata`；`resource` 必填安全 URL
  （错误名 `OAuth protected resource metadata resource`）；`authorization_servers` 可选字符串数组，每项经
  `safeUrl(_, "authorization server URL")`；`scopes_supported` 可选字符串数组；未知字段保留
  （先 `...input` 再 compact）。
- `parseAuthorizationServerMetadata(v)`：对象 `authorization server metadata`；`issuer` 安全 URL
  （`authorization server issuer`）；`authorization_endpoint`（`authorization endpoint`）；
  `token_endpoint`（`token endpoint`）；`registration_endpoint` 可选 URL（`registration endpoint`）；
  `scopes_supported`；`response_types_supported` **必需**的非 null 字符串数组
  （`Invalid response_types_supported`）；`grant_types_supported`；
  `token_endpoint_auth_methods_supported`；`code_challenge_methods_supported`；布尔值
  `client_id_metadata_document_supported`、`authorization_response_iss_parameter_supported`
  （非布尔 → 省略）；未知字段保留。
- `parseOAuthTokens(v)`：对象 `OAuth token response`；`expires_in`：缺省（undefined/null/""）→ undefined，
  否则 `Number(...)`；非有限 → `Error("Invalid expires_in")`；
  `access_token` 必填字符串（`access_token`），`token_type` 必填（`token_type`）；
  `scope`/`refresh_token`/`id_token` 可选字符串（`scope`、`refresh_token`、`id_token`）。
  `Number` 接受数字字符串（`"3600"` → 3600），拒绝 `"abc"`（NaN → 非有限）。
- `parseClientInformation(v)`：对象 `OAuth client registration response`；先展开输入（客户端元数据字段保留），
  然后 `client_id` 必填字符串；`client_secret` 可选字符串；`client_id_issued_at`/`client_secret_expires_at`
  为数字（否则省略）；`redirect_uris` 可选字符串数组（否则 `[]`）。

### 18.2 `oauth/errors.ts`

```ts
class OAuthError extends Error { code: string; errorUri: string | undefined;
  constructor(code, message, errorUri?) { super(message || code); this.name="OAuthError"; this.code=code; this.errorUri=errorUri } }
class OAuthIssuerMismatchError extends Error { expected: string|undefined; received: string|undefined;
  constructor(expected, received) {
    format = i => i === undefined ? "none" : JSON.stringify(i)
    super(`OAuth issuer mismatch: expected ${format(expected)}, received ${format(received)}`);
    this.name="OAuthIssuerMismatchError" } }
class OAuthInsecureEndpointError extends Error { endpoint: string;
  constructor(endpoint) { super(`Refusing to send OAuth credentials to non-HTTPS endpoint ${endpoint}`); this.name=... } }
class OAuthRegistrationError extends Error { status: number; body: string;
  constructor(status, body) { super(`OAuth dynamic client registration failed with status ${status}: ${body}`); this.name=... } }
class McpOAuthAuthorizationRequiredError extends Error {
  constructor() { super("MCP OAuth authorization requires user interaction"); this.name="McpOAuthAuthorizationRequiredError" } }
```
`OAuthError` 的 message 在 `""` 时回退到 `code`。

### 18.3 `oauth/discovery.ts`

- `parseWwwAuthenticate(header: string | null): OAuthChallenge`
  - 无 header → `{}`。
  - `scheme = header.trimStart().split(/\s+/,1)[0].toLowerCase()`；必须是 `"bearer"` 或 `"dpop"`，否则 `{}`。
  - `field(header, name)` 正则（大小写不敏感）：
    `(?:^|[,\s])NAME=(?:"([^"]*)"|([^\s,]+))`；返回第 1 组或第 2 组，且 `""` → `undefined`
    （因此 `scope=""` 视为缺省）。**已实测的边界**：
    - 名字前必须有行首、逗号或空白，所以 `xerror="insufficient_scope"` 里的 `xerror` 不匹配 `error`
      （前一个字符 `x` 不在集合中）；
    - `=` 两侧**不能**有空格：`error = "insufficient_scope"` 解析不出（返回 undefined）；
    - 无引号值读到空白/逗号为止，因此 `error=insufficient_scopeX` → `"insufficient_scopeX"`（无尾边界检查）；
    - 值内部的空格只在带引号时保留（`scope="repo read"` → `"repo read"`）；
    - 引号值中的 `=` 属于值（`error_description="bad scope=x"` → `"bad scope=x"`），但后续字段搜索仍会
      扫过引号内部：`error_description="bad scope=x"` 之后 `field(header,"scope")` 会命中引号内的
      `scope=x"`（返回 `x"`）——这是移植时必须复刻的正则行为，不要"改进"成 RFC 7235 解析器；
    - `scope="a"error="b"`（两个参数间无分隔）→ `scope="a"`，`error` 为 undefined（`"` 后紧跟 `e` 不构成
      分隔符）；
  - `resource_metadata` 用 `new URL` 解析（解析失败 → `undefined`，不报错）。
  - 返回 `{resourceMetadataUrl, scope, error, errorDescription}`（键总是存在；值可为 `undefined`）。
- `discoverProtectedResourceMetadata(serverUrl, {resourceMetadataUrl?, protocolVersion?, fetch?, signal?} = {})`
  - `server = new URL(serverUrl)`；`version = protocolVersion ?? LATEST_PROTOCOL_VERSION`。
  - 首个 URL：若给了 `resourceMetadataUrl` 用它，否则
    `new URL('/.well-known/oauth-protected-resource' + pathSuffix(server.pathname), server.origin)`。
  - `pathSuffix(p)` = `p.endsWith("/") ? p.slice(0,-1) : p`（根 `/` → `""`，因此根 URL 就是裸 well-known 路径；
    `/mcp` → `/mcp`）。
  - 若未显式给 URL、pathname !== "/" 且状态是 discovery miss（4xx 或 502）：discard 后重试
    `new URL('/.well-known/oauth-protected-resource', server.origin)`。
  - 非 ok → `Error(\`HTTP ${status} loading OAuth protected resource metadata\`)`。
  - `parseProtectedResourceMetadata(await response.json())`。请求：`GET`，头
    `Accept: application/json`、`MCP-Protocol-Version: <version>`，带 `signal`。
- `buildAuthorizationServerDiscoveryUrls(authorizationServerUrl)`：issuer URL；`path = pathSuffix(pathname)`；
  返回
  `[{url: /.well-known/oauth-authorization-server${path}, type:"oauth"},
    {url: /.well-known/openid-configuration${path}, type:"oidc"}]`，若 `path` 非空再加第三个
  `{url: ${path}/.well-known/openid-configuration, type:"oidc"}`（均基于 `issuer.origin`）。
- `discoverAuthorizationServerMetadata(url, {fetch?, protocolVersion?, skipIssuerValidation?, signal?} = {})`
  - 按顺序尝试每个 discovery URL。非 ok：视为 discovery miss → 继续（状态被丢弃）；否则
    `Error(\`HTTP ${status} loading authorization server metadata from ${url}\`)`。
  - 解析元数据；除非 `skipIssuerValidation`：把 `trim(metadata.issuer)` 与
    `trim(String(authorizationServerUrl))` 比较，`trim` 只去掉**一个**尾随 `/`；不匹配 →
    `OAuthIssuerMismatchError(expected, received)`。返回元数据。
  - 全部候选耗尽 → `undefined`。
- `discoverOAuthServerInfo(serverUrl, {resourceMetadataUrl?, authorizationServerMetadataUrl?, fetch?, skipIssuerValidation?, signal?} = {})`
  - 尝试受保护资源 discovery；`TypeError` 向外传播（网络错误），其他任何错误被吞掉
    （包括元数据非法 → `resourceMetadata` 保持 undefined）。
  - 若给了 `authorizationServerMetadataUrl`：用 LATEST 协议版本 fetch；非 ok →
    `Error(\`HTTP ${status} loading authorization server metadata from ${url}\`)`；解析；返回
    `{authorizationServerUrl: metadata.issuer, authorizationServerMetadata: metadata, resourceMetadata}`
    （**不**校验 issuer）。
  - 否则 `authorizationServerUrl = resourceMetadata?.authorization_servers?.[0] ?? String(new URL("/", serverUrl))`
    （带尾斜杠的 origin）；discovery 元数据（透传 skipIssuerValidation）；返回三者。
- `resourceUrlFromServerUrl(v)`：`new URL(v)` 且 `hash = ""`。
- `selectResource(serverUrl, metadata?)`：metadata 缺省 → undefined；`requested =
  resourceUrlFromServerUrl(serverUrl)`，`configured = new URL(metadata.resource)`；origin 不同 →
  `Error(\`Protected resource ${metadata.resource} does not match MCP server ${requested}\`)`；
  对两者追加尾斜杠后比较路径前缀；若 requested 路径不以 configured 路径开头 → 同一错误；
  否则返回 `metadata.resource`。

### 18.4 `oauth/flow.ts`

类型：

```ts
type AddClientAuthentication = (headers: Headers, params: URLSearchParams, url: string | URL, metadata?: AuthorizationServerMetadata) => void | Promise<void>;
interface OAuthClientMetadataDocument { url: string; redirectUrl: string }
interface OAuthClientProvider {
  readonly redirectUrl: string | URL;
  readonly clientMetadata: OAuthClientMetadata;
  clientMetadataDocument?(metadata): OAuthClientMetadataDocument | undefined;
  state?(): string | Promise<string>;
  clientInformation(): ...;
  saveClientInformation?(info): void | Promise<void>;
  tokens(): OAuthTokens | undefined | Promise<...>;
  saveTokens(tokens): void | Promise<void>;
  redirectToAuthorization(url: URL): void | Promise<void>;
  saveCodeVerifier(v): void | Promise<void>;
  codeVerifier(): string | Promise<string>;
  addClientAuthentication?: AddClientAuthentication;
  invalidateCredentials?(kind: "all"|"client"|"tokens"|"verifier"|"discovery"): void | Promise<void>;
  saveDiscoveryState?(state: OAuthDiscoveryState): void | Promise<void>;
  discoveryState?(): OAuthDiscoveryState | undefined | Promise<...>;
}
interface OAuthFlowOptions {
  serverUrl: string | URL; authorizationCode?: string; iss?: string; scope?: string;
  resourceMetadataUrl?: URL; authorizationServerMetadataUrl?: URL; fetch?: McpFetch;
  signal?: AbortSignal; skipIssuerValidation?: boolean; skipRefresh?: boolean;
}
type OAuthFlowResult = "AUTHORIZED" | "REDIRECT";
type ClientAuthMethod = "client_secret_basic" | "client_secret_post" | "none";
interface TokenRequestOptions { metadata?; clientInformation: OAuthClientInformationMixed; resource?: string;
  addClientAuthentication?: AddClientAuthentication; fetch?; signal? }
```

辅助函数：
- `loopback(hostname)`：`"localhost" | "127.0.0.1" | "[::1]" | "::1"`（IPv6 的 URL hostname 带方括号）。
- `applicationType(redirectUris)`：无法解析的 URI 被跳过（`if (!URL.canParse(uri)) return false`）；
  若任一已解析 URI 的 scheme 非 http(s) 或 hostname 是 loopback → `"native"`，否则 `"web"`。
  测试：`http://127.0.0.1:1234/callback` → native、`http://[::1]/callback` → native、
  `com.example.app:/callback` → native、`https://app.example/callback` → web、
  `http://localhost/callback` → native（除非显式设置了 `application_type`）。
- `secureEndpoint(v)`：`new URL(v)`；非 https 且非 loopback → `OAuthInsecureEndpointError(url.href)`。
- `selectClientAuthMethod(info, supported)`：
  - 若 `token_endpoint_auth_method` 提示值在 `["client_secret_basic","client_secret_post","none"]` 中且
    （`supported` 为空或包含该提示）→ 用提示。
  - `supported` 为空 → `client_secret ? "client_secret_basic" : "none"`。
  - `client_secret && supported.includes("client_secret_basic")` → basic；然后
    `client_secret && includes("client_secret_post")` → post；`includes("none")` → none；
    兜底 `client_secret ? "client_secret_post" : "none"`。
- `applyClientAuthentication(method, info, headers, params)`：
  - basic：要求 `client_secret`，否则 `Error("client_secret_basic requires a client secret")`；
    `Authorization: Basic base64(\`${client_id}:${client_secret}\`)`（UTF-8）。
  - 其他：`params.set("client_id", info.client_id)`；post 且存在 client_secret 时再设 `client_secret`。
- `pkce()`：32 随机字节（`crypto.getRandomValues`）→ verifier = 这些字节的 base64url（43 字符）；
  challenge = base64url(SHA-256(UTF-8(verifier)))（无 padding）。

`startAuthorization(authorizationServerUrl, {metadata?, clientInformation, redirectUrl, scope?, state?, resource?})`：
- metadata 的 `response_types_supported` 不含 `"code"` →
  `Error("Authorization server does not support authorization codes")`。
- metadata 的 `code_challenge_methods_supported` 存在且不含 `"S256"` →
  `Error("Authorization server does not support PKCE S256")`。
- `url = new URL(metadata?.authorization_endpoint ?? new URL("/authorize", authorizationServerUrl))`；
  非 http(s) → `Error(\`Invalid authorization endpoint ${url.href}\`)`。
- Search params 按此顺序设置（append 顺序影响 `href`）：`response_type=code`、
  `client_id=<client_id>`、`code_challenge=<challenge>`、`code_challenge_method=S256`、
  `redirect_uri=<String(redirectUrl)>`；若 `state` 真值 → `state`；若 `scope` 真值 → `scope`；
  若 `scope.split(/\s+/).includes("offline_access")` → `prompt=consent`；若 `resource` → `resource`。
  全部经 `url.searchParams.set`（同名已存在则原位替换，保持位置；新的追加）。返回
  `{authorizationUrl, codeVerifier}`。

`tokenRequest(authorizationServerUrl, options, params)`（私有）：
- `url = secureEndpoint(metadata?.token_endpoint ?? new URL("/token", authorizationServerUrl))`。
- 头 `Accept: application/json`、`content-type: application/x-www-form-urlencoded`。
- 若有 `options.resource` → `params.set("resource", resource)`。
- 客户端认证：`options.addClientAuthentication ? await it(headers, params, url, metadata) : applySelect(...)`。
- `POST url`，`body: params`（URL 编码；`URLSearchParams` 序列化：空格为 `+`，`~!*'()` 等被百分号转义），
  带 `signal`。
- 读取 `text`；尝试 `JSON.parse`（失败则 value 为 undefined，忽略）。
- 若解析结果是对象且 `error` 为字符串 → `OAuthError(error, string error_description ?? error,
  string error_uri ?? undefined)`（body 中的 error 优先于状态码）。
- 否则若 `!response.ok` → `OAuthError("server_error", \`HTTP ${status}: ${text}\`)`。
- 否则 `parseOAuthTokens(value)`（body 非法 → parse Error）。

`registerClient(authorizationServerUrl, {metadata?, clientMetadata, scope?, fetch?, signal?})`：
- metadata 存在但没有 `registration_endpoint` →
  `Error("Authorization server does not support dynamic client registration")`。
- `POST new URL(registration_endpoint ?? new URL("/register", authorizationServerUrl))`，
  头 `Accept: application/json`、`content-type: application/json`，
  body = `JSON.stringify({...clientMetadata, application_type: clientMetadata.application_type ??
  applicationType(clientMetadata.redirect_uris), ...(scope ? {scope} : {})})`。
- 非 ok → `OAuthRegistrationError(response.status, await response.text())`。
- ok → `parseClientInformation(await response.json())`。

`exchangeAuthorizationCode(url, options & {code, codeVerifier, redirectUrl})` → `tokenRequest`，params 依次为
`grant_type=authorization_code`、`code`、`code_verifier`、`redirect_uri`。

`refreshAuthorization(url, options & {refreshToken})` → params `grant_type=refresh_token`、`refresh_token`；
结果 `{refresh_token: options.refreshToken, ...tokens}`（展开在**右侧**：服务端返回了 `refresh_token` 时，
**服务端的新 token 覆盖**显式传入的旧 token；只在服务端未返回时才回退到 `options.refreshToken`。
已实测：`{refresh_token:"old", ...{refresh_token:"new"}}` → `new`；`{refresh_token:"old", ...{access_token:"a"}}` → `old`。
测试 `shares one refresh ... when refresh tokens rotate` 依赖这一覆盖：服务端返回 `r2`，store 中存的就是 `r2`）。
注释"显式 refresh token 总是存在"指的是**键一定存在**（结果中总有 `refresh_token`），不是"值一定来自参数"。

`withScope(tokens, scope)`（私有）：当 `tokens.scope === undefined && scope` 时 → `{...tokens, scope}`。

`stepUpScope(granted, challenged)`：
- `!challenged`（undefined 或空串）→ `undefined`。
- 两者按 `/\s+/` 切分，丢弃空串，拼接 `[granted, challenged]`，用 `Set` 去重（保留首次出现顺序），
  以 `" "` 连接。`stepUpScope("repo read:org", "repo admin")` → `"repo read:org admin"`。

`runFlow(provider, options)` —— 主流程：
1. `metadataUrl = options.authorizationServerMetadataUrl && secureEndpoint(...)`（https 或 loopback）。
2. `cached = metadataUrl ? undefined : await provider.discoveryState?.()`。
3. 若 `cached?.authorizationServerUrl`：`discovered = {authorizationServerUrl: cached...,
   authorizationServerMetadata: cached.authorizationServerMetadata ?? await discoverAuthorizationServerMetadata(cached.url, {...}),
   resourceMetadata: cached.resourceMetadata}`；否则 `discovered = await discoverOAuthServerInfo(serverUrl, {...})`。
4. 若无 `metadataUrl`：`await provider.saveDiscoveryState?.({...discovered, ...(resourceMetadataUrl ? {resourceMetadataUrl: href} : {})})`。
5. `metadata = discovered.authorizationServerMetadata`。
6. `resource = selectResource(serverUrl, discovered.resourceMetadata)`（可能抛不匹配错误）。
7. `scope = options.scope || discovered.resourceMetadata?.scopes_supported?.join(" ") || provider.clientMetadata.scope`
   （JS `||`，所以 `""` 会落到下一来源；`scopes_supported: []` → `""` → 继续下落）。
8. `stored = await provider.clientInformation()`；`clientDocument = stored ? undefined :
   provider.clientMetadataDocument?.(metadata)`。若有 document：`new URL(url)` 必须是 https 且 pathname !== "/"，
   否则 `Error("Invalid OAuth client metadata URL")`。
9. `client = stored ?? (clientDocument && {client_id: clientDocument.url})`。
   若仍无 client：`options.authorizationCode` → `Error("OAuth client information is missing during code exchange")`；
   无 `provider.saveClientInformation` → `Error("OAuth client information cannot be persisted")`；
   否则 `client = await registerClient(discovered.authorizationServerUrl, {metadata, clientMetadata: provider.clientMetadata, scope, fetch, signal})`
   且 `await provider.saveClientInformation(client)`。
10. `redirectUrl = clientDocument?.redirectUrl ?? provider.redirectUrl`。
11. `tokenOptions = {metadata, clientInformation: client, resource, addClientAuthentication:
    provider.addClientAuthentication, fetch, signal}`。
12. 若有 `authorizationCode`：
    - `iss = options.iss`；若（`iss !== undefined` 或 `metadata?.authorization_response_iss_parameter_supported`）
      且 `iss !== metadata?.issuer` → `OAuthIssuerMismatchError(metadata?.issuer, iss)`。
    - `tokens = await exchangeAuthorizationCode(url, {...tokenOptions, code, codeVerifier: await provider.codeVerifier(), redirectUrl})`。
    - `await provider.saveTokens(withScope(tokens, scope))`；返回 `"AUTHORIZED"`。
13. 否则 `existing = options.skipRefresh ? undefined : await provider.tokens()`。
    若 `existing?.refresh_token`：尝试 `refreshAuthorization(url, {...tokenOptions, refreshToken: existing.refresh_token})`；
    保存 `withScope(tokens, existing.scope)`；返回 `"AUTHORIZED"`。
    捕获：若 `options.signal?.aborted` 或 `error instanceof OAuthInsecureEndpointError` → 重新抛出；
    若 `error instanceof OAuthError && error.code !== "server_error"` → 重新抛出；否则落入新的授权流程。
14. `state = await provider.state?.()`；`authorization = await startAuthorization(url, {metadata,
    clientInformation: client, redirectUrl, scope, state, resource})`；
    `await provider.saveCodeVerifier(authorization.codeVerifier)`；
    `await provider.redirectToAuthorization(authorization.authorizationUrl)`；返回 `"REDIRECT"`。

`authorizeMcp(provider, options)`：
- 运行 flow；遇到 `OAuthError` 且 code 为 `invalid_client` 或 `unauthorized_client` →
  `invalidateCredentials?.("all")` 后重跑一次；`invalid_grant` → `invalidateCredentials?.("tokens")` 后重跑一次；
  其他重新抛出。（只重试一次；第二次失败传播第二个错误。）

`adaptOAuthProvider(provider)`：
```
let inFlight: Promise<void> | undefined;
return {
  token: async () => (await provider.tokens())?.access_token,
  onUnauthorized: async (context) => {
    challenge = parseWwwAuthenticate(context.response.headers.get("www-authenticate"))
    insufficientScope = challenge.error === "insufficient_scope"
    if (!insufficientScope && !inFlight && context.token !== undefined) {
      current = (await provider.tokens())?.access_token
      if (current !== undefined && current !== context.token) return     // 另一个请求已刷新
    }
    inFlight ??= Promise.resolve(insufficientScope ? provider.tokens() : undefined)
      .then(granted => authorizeMcp(provider, {
        serverUrl: context.serverUrl,
        resourceMetadataUrl: challenge.resourceMetadataUrl,
        scope: insufficientScope ? stepUpScope(granted?.scope, challenge.scope) : challenge.scope,
        fetch: context.fetch,
        skipRefresh: insufficientScope,
      }))
      .then(result => { if (result === "REDIRECT") throw new McpOAuthAuthorizationRequiredError() })
      .finally(() => { inFlight = undefined })
    await inFlight
  },
}
```
- 并发 401 共享一次刷新（测试：两个并发 + 一个迟到的旧 token → grants `["r1"]`，`token()` 变为 `"a2"`）。
- `challenge.scope` 可能是 `undefined`；作为 `scope: undefined` 传入即缺省。注意 `scope=""` 时正则返回 undefined
  （视为缺省）。
- 升级授权在保存 code verifier 并重定向后抛 `McpOAuthAuthorizationRequiredError`。

### 18.5 `oauth/provider.ts`

```ts
interface McpOAuthState {
  serverUrl: string; clientInformation?: OAuthClientInformationMixed; tokens?: OAuthTokens;
  tokensExpireAt?: number; codeVerifier?: string; oauthState?: string; discovery?: OAuthDiscoveryState;
}
interface McpOAuthStateStore { load(): McpOAuthState | undefined | Promise<...>; save(state): void | Promise<void> }
interface McpOAuthProviderOptions {
  serverUrl: string | URL; redirectUrl: string | URL;
  clientMetadata: Omit<OAuthClientMetadata,"redirect_uris"> & { redirect_uris?: string[] };
  clientMetadataDocument?: (metadata) => OAuthClientMetadataDocument | undefined;
  clientId?: string; clientSecret?: string; store?: McpOAuthStateStore;
  onRedirect(url: URL): void | Promise<void>;
}
class MemoryOAuthStateStore { load() → structuredClone(value) | undefined; save(state) → value = structuredClone(state) }
class McpOAuthProvider implements OAuthClientProvider { ... }
```

`McpOAuthProvider` 构造：
- `serverUrl = String(new URL(options.serverUrl))`（规范化 href），`redirectUrl = String(options.redirectUrl)`。
- `clientMetadata = {...options.clientMetadata,
  redirect_uris: options.clientMetadata.redirect_uris ?? [this.redirectUrl],
  grant_types: options.clientMetadata.grant_types ?? ["authorization_code","refresh_token"],
  response_types: options.clientMetadata.response_types ?? ["code"],
  token_endpoint_auth_method: options.clientMetadata.token_endpoint_auth_method
    ?? (options.clientSecret ? "client_secret_post" : "none")}`。
- `configuredClient = clientId ? {client_id, ...(clientSecret ? {client_secret} : {})} : undefined`。
- `store = options.store ?? new MemoryOAuthStateStore()`；`onRedirect` 保存。
- `writes: Promise<void>` 串行化更新。

方法：
- `state()`：已有 `oauthState` 则返回；否则 32 随机字节的 hex（64 字符）先保存（`update`）再返回。
- `clientInformation()`：`configuredClient ?? load().clientInformation`。
- `saveClientInformation(info)`：存在 configured client 时 no-op；否则保存。
- `tokens()`：`load().tokens`；`saveTokens(tokens)`：计算
  `expiresAt = tokens.expires_in === undefined ? undefined : Date.now() + expires_in*1000`；保存
  `{...value, tokens}` 并设置或删除 `tokensExpireAt`。
- `redirectToAuthorization(url)`：`await onRedirect(url)`。
- `saveCodeVerifier`、`codeVerifier()`（缺失抛 `Error("No OAuth PKCE code verifier is stored")`）。
- `invalidateCredentials(kind)`：`all` 清除 clientInformation、tokens、tokensExpireAt、codeVerifier、
  discovery、oauthState；`client`/`tokens`/`verifier`/`discovery` 清除各自部分（tokens 同时清 tokens 与
  tokensExpireAt）；`oauthState` 只在 `all` 时清除。
- `saveDiscoveryState`、`discoveryState`。
- `load()`：等待 write 链后返回 `own(state)`；`own` 仅当 `state.serverUrl === this.serverUrl` 时返回该 state，
  否则 `{serverUrl: this.serverUrl}`（凭据绝不跨服务器 URL 泄漏；测试断言两个共享同一 store 的 provider
  隔离 tokens）。
- `update(fn)`：链到 `writes`；`fn(own(store.load()))` 然后 `store.save`；等待。

### 18.6 `oauth/callback.ts` — `OAuthCallbackServer`

```ts
interface OAuthCallback { code: string; state: string; iss?: string }
type OAuthCallbackPage = { ok: true } | { ok: false; message: string; details?: string };
interface OAuthCallbackServerOptions {
  host?: string;        // 默认 "127.0.0.1"
  redirectHost?: string;// 默认 host
  port?: number;        // 默认 0（临时端口）
  path?: string;        // 默认 "/callback"
  extraPaths?: string[];
  timeoutMs?: number;   // 默认 5*60_000
  renderPage?: (page: OAuthCallbackPage) => string;
}
static async listen(options = {}): Promise<OAuthCallbackServer>
readonly redirectUrl: string
waitForCallback(state: string, path?: string): Promise<OAuthCallback>
async close(): Promise<void>
```

- `listen`：`createServer((req,res) => instance?.handle(req.url ?? "/", res))`；`server.listen(port, host)`；
  listen 错误 reject；绑定后 `address()` 必须是 TCP，否则 `Error("OAuth callback server did not bind to TCP")`。
- `redirectUrl = \`http://${redirectHost.includes(":") ? \`[\${redirectHost}]\` : redirectHost}:\${address.port}${path}\``
  （IPv6 字面量 redirectHost 加方括号；已含方括号的 redirectHost 会被双层包裹——未处理的边界）。
- `paths = [path, ...(extraPaths ?? [])]`。
- `waitForCallback`：重复 state → `Error("OAuth state is already pending")`；`timeoutMs` 后定时器删除 pending
  并以 `Error("OAuth callback timed out")` reject。
- `close()`：以 `Error("OAuth callback server closed")` reject 所有 pending，清空，关闭 HTTP 服务器
  （close 错误则 reject）。
- `reply(response, status, page)`：有 `renderPage` → 状态码 + 头
  `content-type: text/html; charset=utf-8`、`cache-control: no-store`，body 为 `renderPage(page)`；
  否则 `content-type: text/plain; charset=utf-8`，body 为 `plainText(page)`。
- `plainText(page)`：ok → `"Authorization complete. You may close this window."`；否则
  `details ? \`${message}\n\n${details}\` : message`。
- `handle(rawUrl, response)`：
  1. `url = new URL(rawUrl, redirectUrl)`；pathname 不在 `paths` 中 → 404 `{ok:false, message:"Not found"}`。
  2. `state = searchParams.get("state")`；查 pending；缺失/无效 → 400
     `{ok:false, message:"Invalid or expired OAuth state"}`。
  3. 清除定时器，删除 pending。
  4. 若 `pending.path !== undefined && url.pathname !== pending.path`：以
     `Error("The authorization response arrived on another redirect URI")` reject；回复 400
     `{ok:false, message:"Unexpected redirect URI"}`。
  5. `error` 参数：`description = error_description ?? error`；以 `new Error(description)` reject；
     回复 200 `{ok:false, message:"Authorization failed. You may close this window.", details: description}`。
  6. 无 `code`：以 `Error("OAuth callback did not include an authorization code")` reject；回复 400
     `{ok:false, message:"Missing authorization code"}`。
  7. `iss` 参数（真值）时包含；resolve `{code, state, iss?}`；回复 200 `{ok:true}`。
- 所有 HTTP 方法都走同一处理；请求体被忽略。
- 面向浏览器的纯文本字符串逐字如上（也是 coding-agent HTML 渲染器的默认页输入）。

## 19. 跨单元接口（精确类型）与导出清单

### 19.1 telemetry 的依赖与暴露

依赖：不依赖其他 kiana 包。运行环境：`.` 与 `./render` 不含 Node 专属 API；`./testing` 引入
`node:assert/strict`（`findSpan`/`rejectsWithSameValue` 使用）。

暴露给：`kiana-ai`、`kiana-durable`、`kiana-chord`（这三个是 package.json 中的直接依赖）；`kiana-agent-core`、
`kiana-coding-agent` 只经 `kiana-ai`/`kiana-chord` **传递**依赖，其源码没有任何 `@earendil-works/kiana-telemetry` 导入
（已 grep 验证：`packages/agent`、`packages/coding-agent` 命中数为 0）。`kiana-evals` **不使用**该包
（其 README 中的 "telemetry" 指 eval 结果 JSONL，不是本包；源码/测试无导入）。原 spec 的这句话不准确。
其他包按名导入的类型：`TelemetryContext`、`TelemetrySpan`、`SpanOptions`、`SpanAttributes`、`SpanStatus`、
`SpanLink`、`TelemetryCarrier`、`TelemetryPropagation`、`TelemetryMetrics`、`TelemetryCounter`、
`TelemetryUpDownCounter`、`TelemetryHistogram`、`InstrumentOptions`、`HistogramOptions`、`TelemetryStore`、
`StoredSpan`、`StoredTrace`、`TraceQuery`、`MetricQuery`、`StoredMetric`、`NOOP_TELEMETRY_CONTEXT`、
`NOOP_TELEMETRY_METRICS`、`NOOP_TELEMETRY_PROPAGATION`、`createTypedMetrics`、`createTypedSpanStarter`、
`defineTelemetrySchema`、`InMemoryTelemetryContext`、`RecordedTelemetrySpan`、`RecordedTelemetryMeasurement`、
`SchemaTelemetrySpan`、`TelemetrySpanDefinition`、`TelemetrySchemaDefinition`、`TelemetryMetricDefinition`、
`TypedSpanStarter`。

Rust 转换指引：Rust 没有结构类型，定义 trait：

```rust
pub trait TelemetryContext: Send + Sync {
    fn metrics(&self) -> Option<Arc<dyn TelemetryMetrics>> { None }
    fn propagation(&self) -> Option<Arc<dyn TelemetryPropagation>> { None }
    fn start_span_boxed<'a>(&'a self, options: SpanOptions,
        callback: Box<dyn FnOnce(Arc<dyn TelemetrySpan>) -> BoxFuture<'a, Result<Value, Value>> + Send + 'a>)
        -> BoxFuture<'a, Result<Value, Value>>;
}
```
再加上泛型辅助 `start_span<T, E, F, Fut>(&self, options, f)`。rejection 值可以是任意 JS 值（`undefined`、对象）；
把结果建模为 `Result<T, Value>`（`T` 是回调结果，JS 的 `undefined`/值）。内存适配器必须保留任意 rejection 载荷
以便测试：用 `Result<T, JsValue>`，`JsValue` 是小枚举
（`Undefined | Null | Bool | Number | String | Object(Arc<...>)`），使身份（same-value）语义可以用
`Arc::ptr_eq` 测试。

`chord::Context` 持有 `TelemetrySpan`，默认 `NOOP_TELEMETRY_CONTEXT`；`durable` 在持久化事务类型中保存
`TelemetryCarrier`（因此 carrier 的 serde 必须是普通字符串 map）。`ai::RequestOptions.telemetry?: TelemetrySpan`。

### 19.2 导出清单（逐字，用于核对 Rust 公开 API）

`telemetry/src/index.ts`（59 个导出；名字集合为准，下面按"契约类型 → 指标 → no-op → schema → 再导出"分组列出）：
`AttributeValue`、`SpanAttributes`、`TelemetryCarrier`、`TelemetryPropagation`、`SpanLink`、`SpanOptions`、
`SpanStatus`、`StoredSpan`、`StoredTrace`、`TraceQuery`、`MetricQuery`、`StoredMetric`、`TelemetryStore`、
`TelemetryContext`、`TelemetrySpan`、`HistogramOptions`、`InstrumentOptions`、`SchemaTelemetryCounter`、
`SchemaTelemetryHistogram`、`TelemetryCounter`、`TelemetryHistogram`、`TelemetryMetricAttributeDefinition`、
`TelemetryMetricDefinition`、`TelemetryMetrics`、`TelemetryUpDownCounter`、`TypedTelemetryMetrics`、
`createTypedMetrics`、`NOOP_TELEMETRY_CONTEXT`、`NOOP_TELEMETRY_METRICS`、`NOOP_TELEMETRY_PROPAGATION`、
`TelemetryAttributeType`、`TelemetryAttributeMetadata`、`TelemetryAttributeDefinition`、
`TelemetryStartAttributeDefinition`、`TelemetryEventAttributeDefinition`、`TelemetryEventDefinition`、
`TelemetryParentDefinition`、`TelemetrySpanDefinition`、`TelemetrySchemaDefinition`、`defineTelemetrySchema`、
`InferRequiredAndOptionalAttributes`、`InferStartAttributes`、`InferOptionalAttributes`、
`ExactTelemetryAttributes`、`InferEventAttributes`、`TelemetrySchemaSpanName`、
`TelemetrySchemaSpanStartAttributes`、`TelemetrySchemaSpanEndAttributes`、`TelemetrySchemaSpanEventName`、
`TelemetrySchemaSpanEventAttributes`、`SchemaTelemetrySpan`、`TelemetrySchemaSpanUnion`、`TypedSpanStarter`、
`createTypedSpanStarter`、`RecordedTelemetryEvent`、`RecordedTelemetryLink`、`RecordedTelemetryMeasurement`、
`RecordedTelemetrySpan`、`InMemoryTelemetryContext`。

`telemetry/src/render.ts`：`renderTrace`。
`telemetry/src/testing/index.ts`（4 个）：`createTelemetryAdapterConformance`、`TelemetryAdapterConformanceCase`、
`TelemetryAdapterFixture`、`TelemetryAdapterFixtureFactory`。

`mcp/src/index.ts`（67 个，逐字顺序）：
`AuthProvider`、`McpFetch`、`UnauthorizedContext`、`McpClient`、`McpClientOptions`、`McpRequestOptions`、
`AudioContent`、`BlobResourceContents`、`CallToolResult`、`ContentAnnotations`、`ContentBlock`、
`EmbeddedResourceContent`、`ImageContent`、`LlmContent`、`ResourceLinkContent`、`TextContent`、
`TextResourceContents`、`toLlmContent`、`isJsonRpcNotification`、`isJsonRpcRequest`、`isJsonRpcResponse`、
`JSON_RPC_ERROR_CODES`、`JsonRpcErrorObject`、`JsonRpcErrorResponse`、`JsonRpcId`、`JsonRpcMessage`、
`JsonRpcNotification`、`JsonRpcRequest`、`JsonRpcResponse`、`JsonRpcSuccessResponse`、`McpAbortError`、
`McpConnectionClosedError`、`McpError`、`McpTimeoutError`、`parseJsonRpcMessage`、`CancelledNotification`、
`ClientCapabilities`、`Implementation`、`InitializeParams`、`InitializeResult`、`LATEST_PROTOCOL_VERSION`、
`ListResourcesResult`、`ListResourceTemplatesResult`、`ListToolsResult`、`ProgressNotification`、
`ReadResourceResult`、`Resource`、`ResourceTemplate`、`Root`、`ServerCapabilities`、
`SUPPORTED_PROTOCOL_VERSIONS`、`SupportedProtocolVersion`、`Tool`、`ToolAnnotations`、`ToolExecution`、
`StdioTransport`、`StdioTransportOptions`、`McpAuthRequiredError`、`McpHttpError`、`McpSessionExpiredError`、
`StreamableHttpTransport`、`StreamableHttpTransportOptions`、`McpTransport`、`McpTransportCloseListener`、
`McpTransportErrorListener`、`McpTransportMessageListener`、`McpTransportSendOptions`。

`mcp/src/oauth/index.ts`（44 个，逐字顺序）：
`OAuthCallback`、`OAuthCallbackPage`、`OAuthCallbackServer`、`OAuthCallbackServerOptions`、
`buildAuthorizationServerDiscoveryUrls`、`discoverAuthorizationServerMetadata`、`discoverOAuthServerInfo`、
`discoverProtectedResourceMetadata`、`parseWwwAuthenticate`、`resourceUrlFromServerUrl`、`selectResource`、
`McpOAuthAuthorizationRequiredError`、`OAuthError`、`OAuthInsecureEndpointError`、`OAuthIssuerMismatchError`、
`OAuthRegistrationError`、`AddClientAuthentication`、`adaptOAuthProvider`、`authorizeMcp`、
`exchangeAuthorizationCode`、`OAuthClientMetadataDocument`、`OAuthClientProvider`、`OAuthFlowOptions`、
`OAuthFlowResult`、`refreshAuthorization`、`registerClient`、`startAuthorization`、`stepUpScope`、
`TokenRequestOptions`、`McpOAuthProvider`、`McpOAuthProviderOptions`、`McpOAuthState`、`McpOAuthStateStore`、
`MemoryOAuthStateStore`、`AuthorizationServerMetadata`、`OAuthChallenge`、`OAuthClientInformation`、
`OAuthClientInformationFull`、`OAuthClientInformationMixed`、`OAuthClientMetadata`、`OAuthDiscoveryState`、
`OAuthProtectedResourceMetadata`、`OAuthServerInfo`、`OAuthTokens`。

`mcp/src/testing/index.ts`（2 个）：`createInMemoryTransportPair`、`InMemoryTransport`。
mcp 三个入口合并去重后共 67 + 44 + 2 = 113 个名字。telemetry 三个入口共 59 + 1 + 4 = 64 个名字。

### 19.3 mcp 的依赖与暴露

依赖：不依赖任何 kiana 包。平台：fetch/Headers/Response/ReadableStream、`node:http`、`node:child_process`、
`node:process`、`Buffer`、`crypto`（getRandomValues、subtle.digest）、`TextDecoder`、`URL`/`URLSearchParams`、
`AbortController`/`AbortSignal`（代码中实际只用 `AbortSignal.timeout(1000)`；**没有** `AbortSignal.any` 的调用；
  也**没有** `performance.now`）、`queueMicrotask`、
`structuredClone`（mcp 不用 `performance.now`）。

暴露给 `kiana-coding-agent`（以及 agent 示例）：§9–§18 的全部。coding-agent 具体依赖：

- `McpClient` 的 getter `connectionState`、`serverCapabilities`、`instructions`；
- `client.listTools()`、`callTool(name,args,{signal,timeoutMs,onProgress})`、
  `listResourcesPage(cursor, options)`、`listResources`、`listResourceTemplatesPage(cursor)`、
  `listResourceTemplates`、`readResource(uri, options)`、`onNotification`、`onClose`、`close`；
- `McpHttpError.status`、`McpSessionExpiredError`、`McpAuthRequiredError`、`McpError.code` 与
  `JSON_RPC_ERROR_CODES.methodNotFound`、`McpOAuthAuthorizationRequiredError`；
- `StdioTransport.stderr`（尾部用于错误消息）、`StreamableHttpTransport` 选项；
- `toLlmContent`；`ContentBlock` 变体；`CallToolResult` 形状（`content`、`structuredContent`、`isError`、`_meta`）；
- OAuth：`McpOAuthProvider`、`McpOAuthStateStore`、`McpOAuthState`、`authorizeMcp`、`stepUpScope`、
  `parseWwwAuthenticate`、`OAuthCallbackServer`、`McpOAuthAuthorizationRequiredError`、`OAuthChallenge`、
  `OAuthClientMetadataDocument`、`AuthorizationServerMetadata`、`OAuthCallback`/`OAuthCallbackPage`
  （coding-agent 的 HTML 页面渲染器）与 `MemoryOAuthStateStore`；
  **注意**：`adaptOAuthProvider` 在本仓库内只有 `packages/mcp/test/oauth.test.ts` 使用，
  coding-agent 并不使用它（尽管它在导出清单里，仍按测试契约移植）。
- testing：`createInMemoryTransportPair`（`packages/mcp/test/client.test.ts`、
  `packages/coding-agent/test/mcp-extension.test.ts`、`packages/coding-agent/test/suite/agent-session-mcp.test.ts`
  使用；`InMemoryTransport` 类型在客户端测试中被用作类型标注）。
- 子路径：coding-agent 源码 import `@earendil-works/kiana-mcp`（runtime/tools/resources）与
  `@earendil-works/kiana-mcp/oauth`（runtime/oauth）；测试另用 `@earendil-works/kiana-mcp/testing`。

`McpOAuthState` 由 coding-agent 持久化到 `<agent-dir>/mcp-auth.json`（文件由那边负责）；JSON 形状是
`Record<string, McpOAuthState>`（不是单个对象），键为 `` `${mcpNamespace(serverName)}|${String(new URL(serverUrl))}` ``，
旧版纯 URL 键会被首个加载的服务器接管（`load` 时迁移、`remove` 时清理）；文件内容按
`JSON.stringify(states, null, 2) + "\n"` 序列化，读取时非对象/数组视为空表。`McpOAuthState` 本身驼峰键，
省略 undefined 字段。

---

## 20. 外部格式

### 20.1 stdio 分帧（MCP stdio transport）

- 每行一个 JSON-RPC 消息，UTF-8，以 `\n` 结尾（服务器可能发 `\r\n`；解析器剥一个尾随 `\r`）。
- 不使用 `Content-Length` 分帧。空行/纯空白行忽略。不允许注释。
- 消息紧凑序列化（`JSON.stringify` 无空格）。键序 = 插入顺序。
- stderr 不是协议；被捕获（最后 64 KiB）并可选择性转发。
- 关闭：关闭 stdin（EOF），等待 `min(500, closeTimeoutMs)`，SIGTERM 进程组，等待 `closeTimeoutMs`，
  SIGKILL 进程组；子进程退出后再次 SIGTERM 进程组。

### 20.2 Streamable HTTP 线格式

- POST `<url>`：请求头 `Accept: application/json, text/event-stream`、`Content-Type: application/json`，
  加 session/protocol/auth 头；body 为一个 JSON-RPC 消息。
- 响应：`Content-Type: application/json`（单消息或数组）或 `text/event-stream`（SSE 携带消息；请求的答案可能
  稍后到达，或在 GET 恢复之后）。对请求返回 202/204 视为错误（"accepted request ... without a response"）。
  其他 content type 也是错误。
- 捕获 `Mcp-Session-Id` 响应头；后续请求发送。initialize 之后发送 `MCP-Protocol-Version`。
  来自 auth provider 的 `Authorization: Bearer`。
- GET `<url>`：`Accept: text/event-stream`；可选 `Last-Event-ID`；405 表示没有服务端流。
- 关闭时 DELETE `<url>`（1 秒超时），尽力而为。
- SSE 格式：行 `field: value`，冒号后可有一个前导空格；`data` 行以 `\n` 连接；空行派发；`:` 注释；
  `event`、`id`（含 NUL 拒绝）、`retry`（仅数字）。空/缺失 data 或 event 类型不是 `message` 的事件被忽略
  （但 `id` 仍用于预备恢复）。默认字节上限 16 MiB。
- 进度：client 发送 `_meta.progressToken` = 请求 id 数字；server 发送 `notifications/progress`，带
  `{progressToken, progress, total?, message?}`；每次都会续期超时。
- 取消：向 server 发 `notifications/cancelled` `{requestId, reason?}`；server 发来的取消
  `{requestId, reason?}` 会中止相应 incoming handler 的 signal。
- client transport 合成的错误：`{jsonrpc:"2.0", id, error:{code:-32603, message:"MCP response stream failed: ..."}}`。
- 来自 server 的 `ping` 请求答 `{}`；`roots/list` 答 `{roots:[...]}`；其他 server 请求答
  `-32601 Method not found: <method>`；handler 异常 → `-32603`（或用 `McpError` 的 code）并在有 `data` 时带上。

### 20.3 OAuth 线格式

- 受保护资源元数据：`GET <origin>/.well-known/oauth-protected-resource<path>`，回退
  `<origin>/.well-known/oauth-protected-resource`；头 `Accept: application/json`、
  `MCP-Protocol-Version: <version>`。
- 授权服务器元数据：`GET <origin>/.well-known/oauth-authorization-server<path>`、
  `<origin>/.well-known/openid-configuration<path>`、`<origin><path>/.well-known/openid-configuration`。
- 动态客户端注册：`POST <registration_endpoint>`，JSON body 为
  `clientMetadata + application_type + scope?`；响应按 client information 解析。
- 授权：浏览器 GET
  `authorization_endpoint?response_type=code&client_id=...&code_challenge=...&code_challenge_method=S256&redirect_uri=...&state=...&scope=...&prompt=consent?&resource=...`。
- Token endpoint：`POST` form-urlencoded（`grant_type=authorization_code|refresh_token`、`code`、
  `code_verifier`、`redirect_uri`、`refresh_token`、`resource`，加客户端认证）。响应 JSON
  `access_token`、`token_type`、`expires_in?`、`scope?`、`refresh_token?`、`id_token?`；OAuth 错误
  `{error, error_description?, error_uri?}` 可能带任意状态码。
- `WWW-Authenticate` challenge 解析：Bearer/DPoP，带 `resource_metadata`、`scope`、`error`、
  `error_description`。
- 回环回调：HTTP GET `http://<host>:<port><path>?code=...&state=...&iss=...` 或
  `error=...&error_description=...`；响应见 §18.6。

### 20.4 环境变量与磁盘文件

**运行时代码**（`src/**`）不读取任何环境变量。`StdioTransport` 在 `inheritEnv !== false` 时把 `process.env`
转发给子进程（coding-agent 传入已解析的 env 值）。两个包都**不**写文件（唯一的 `node:http` 监听是回环
回调服务器）。`McpOAuthState` JSON 是嵌入方使用的持久化格式；coding-agent 的实现把它存在
`<agent-dir>/mcp-auth.json`，形状是 `Record<string, McpOAuthState>`，键为 `` `${mcpNamespace(name)}|${new URL(serverUrl)}` ``
（另兼容旧版的纯 URL 键），写入时 `JSON.stringify(states, null, 2) + "\n"`——**这些由嵌入方负责，不在本包内**。

唯一的例外在包外配置：`packages/mcp/vitest.config.ts` 读取 `process.env.GITHUB_ACTIONS` 来选择 reporter
（`["dot", "github-actions"]` 或 `["dot"]`）；telemetry **没有** vitest 配置文件，用工作区根配置。
Rust 移植的测试运行器无需复刻该环境变量。

---

## 21. npm 依赖 → Rust crate 映射

| npm | 使用方 | Rust 替代 | 需要桥接的差异 |
|---|---|---|---|
| `cross-spawn@7.0.6` | stdio transport | `tokio::process::Command`（Unix 原生）；Windows 需自行解析 `.cmd`/`.bat` shim 为 `cmd.exe /c` | 本移植目标为 Linux/macOS 时可直接省略 Windows 分支并文档化；`detached` 进程组 → Unix `pre_exec(|| { libc::setsid(); Ok(()) })`，杀组用 `libc::killpg` 或 `nix`。 |
| Node `node:child_process` | stdio | `tokio::process` | 信号语义不同；杀进程组用 `libc::killpg`。 |
| Node `node:http` | OAuth 回调 | `hyper` + `hyper-util`（或 `axum`；`tiny_http` 更简单但阻塞） | 必须绑定 127.0.0.1:0、报告端口、返回精确的纯文本 body 与状态码；不需要 keep-alive。 |
| Web `fetch` / `Headers` / `Response` / `ReadableStream` | HTTP transport、OAuth | `reqwest`（rustls-tls）、`http::HeaderMap`、`bytes`、`futures_util::StreamExt` | `reqwest` 默认解压 gzip（fetch 也会），默认跟随重定向（fetch 也是 `redirect: follow`）；SSE 需要 `stream` feature 才能拿到字节流。`Headers` 需要兼容层：规范化、合并、小写名、裁剪值。client 不设整体超时（与 fetch 一致）；每请求取消用 `tokio::select!` 监听 transport 的取消令牌。 |
| `Buffer` | 字节长度、base64、hex | `bytes::Bytes`、`base64` crate（`URL_SAFE_NO_PAD`、`STANDARD`）、手写 hex | `Buffer.byteLength` = UTF-8 字节长度。PKCE 的 base64url 不带 padding。 |
| `crypto.getRandomValues` / `crypto.subtle.digest` | PKCE、state | `rand::rngs::OsRng` 或 `getrandom`、`sha2` | verifier = 32 随机字节的 base64url（43 字符）；challenge = base64url(sha256(verifier UTF-8))。 |
| `crypto.randomUUID` | 内存 scope | `uuid` crate v4（小写） | 必须是 RFC 4122 v4 文本形式。 |
| `structuredClone` | in-memory transport、MemoryOAuthStateStore | `serde_json::Value` 克隆（消息是 JSON） | OAuth state 按契约可 JSON 序列化（会被持久化），用 serde 克隆可以。也可以定义纯数据结构的 `Clone`。 |
| `TextDecoder`（流式 UTF-8） | SSE | 自实现增量解码 | 必须处理跨 chunk 拆分的多字节字符：用待解码字节缓冲 + `std::str::from_utf8` 的 `valid_up_to`，非法字节替换为 U+FFFD。 |
| `URL` / `URLSearchParams` | OAuth URL、查询参数 | `url` crate（`Url`、`form_urlencoded::Serializer`） | 大体匹配 WHATWG。差异：非特殊 scheme 的百分号编码集合略有不同；`form_urlencoded` 对 `~!*'()` 的编码与 JS 不同（JS 会编码 `~!*'()`，Rust 序列化器不会）。桥接方案：实现与 `URLSearchParams` 一致的自定义编码器（除 `A-Z a-z 0-9 * - . _` 外全部百分号编码，空格为 `+`）。 |
| `AbortSignal` / `AbortController` | 全局 | `tokio_util::sync::CancellationToken`、`tokio::select!`、`futures::future::Abortable` | 唯一内置组合器用法是 `AbortSignal.timeout(1000)`（close 的 DELETE）→ `tokio::time::timeout`。`AbortSignal.any` 在源码中**不存在**。`addEventListener("abort", ..., {once:true})` 与 `removeEventListener` → 令牌 + `select!`。`AbortSignal` 的 reason 概念（字符串/DOMException）需要客户端自己保存（见 §13.5）。 |
| `performance.now` / `Date.now` | 内存适配器 | `std::time::Instant`（单调）+ `SystemTime`（纪元毫秒） | 测试需要可 mock：注入 `Clock` trait，提供 `now_ms()` 与 `now_monotonic_ms()`。 |
| `node:assert/strict` | testing 子路径 | Rust 测试用 `assert_eq!`/`panic!`，或返回 `Err(String)` 的小型 `assert_deep_eq` | 一致性 case 应返回结果而不是 panic（便于跨运行器注册）。 |
| `queueMicrotask` | in-memory transport | `tokio::spawn` / channel | 每个发送方的顺序必须保持。 |
| `JSON.stringify` / `JSON.parse` | 全局 | `serde_json` | 键序（插入顺序 + 整数键前置）、JS 数字格式、`undefined` 省略、`-0`、`NaN`→`null`。见 §22。 |
| `Number.isFinite`、`Number(x)` | client/oauth/in-memory | 自定义 `js_number_from_value` | JS 数字字符串：`Number("") = 0`、`Number(" 2 ") = 2`、`Number("0x10") = 16`、`Number("1e3") = 1000`、`Number("12abc") = NaN`、`Number("Infinity") = Infinity`。 |
| `String(x)` | 错误消息 | `js_string(v)` | 见 §22.2。 |
| `Headers` | HTTP transport、token 请求 | `http::HeaderMap` + JS 兼容规范化层 | JS 只裁剪**首尾**空白，**内部空白原样保留**（实测 `"  x   y  "` → `"x   y"`）；重复头用 `", "` 合并；`get` 用小写名。若调用方能提供头名（coding-agent 会），需像 `Headers.set` 一样校验非法名（抛 `TypeError`）。 |
| `vitest` fake timers | 测试 | `tokio::time::pause` + `advance` | 不进入产物代码。 |

不需要替换的库：SSE 解析器**不要**用 `eventsource-stream` 之类——本单元自定义的解析器有精确的字节上限与
逐事件的 `onId`/`onRetry` 回调语义，必须按 §17.1 手写。

## 22. 必须桥接的 JS 语义

### 22.1 对象键序（`JSON.stringify`、`Object.entries`、`Object.fromEntries`）

JS 对象键序：整数样式自有键按数值升序在前，然后字符串键按插入顺序。整数样式 = 数组索引键（`"0"`、`"1"`、…、
最大 2^32-2；规范形式：无前导零、无 `+`、无 `-`，除 `"0"` 外不以 `0` 开头）。`"01"`、`"-1"`、`"1.5"`
**不是**整数样式。影响：

- `copyAttributes`（fromEntries）与属性快照；
- `renderTrace` 与 `toLlmContent`（structuredContent）的属性 JSON.stringify，以及请求/响应 body 的序列化；
- 头？不（Headers 按名小写排序）。

Rust：`serde_json::Map` 保留插入顺序（需 `preserve_order` feature）。桥接方式：在输出可观察的边界调用
`js_order(Object) -> Object` 重排：`renderTrace` 属性格式化、`toLlmContent` 的 structuredContent JSON、
stdio `send` body、HTTP `send` body、cursor JSON（`[startedMs, traceId]` 是数组，不受影响）、
嵌入方写入的 OAuth state JSON（由嵌入方负责）。从服务器读到的字段保持原顺序用于往返（工具 JSON 往返按序列化
相等比较；JS 也是 parse+stringify 后重排整数键，因此 parse 后应用 `js_order` 与 JS 一致）。

### 22.2 `String(v)`（`toError`、取消 reason、错误消息插值使用）

- `undefined` → `"undefined"`；`null` → `"null"`；布尔 → `"true"/"false"`。
- 数字 → ECMAScript Number::toString(10)。
- 字符串 → 自身。
- 数组 → 元素以 `","` 连接（`[]`→`""`、`[1,[2,3]]`→`"1,2,3"`、空槽 → `""`）。
- 普通对象 → `"[object Object]"`（JSON 解析出的对象有 `Object.prototype`；本移植中不存在自定义
  `toString`/`Symbol.toPrimitive` 的对象）。

### 22.3 ECMAScript Number::toString（模板字面量、JSON.stringify、renderTrace 使用）

规则（最短可往返表示）：
- `NaN` → `"NaN"`；`±Infinity` → `"Infinity"/"-Infinity"`；`-0` → `"0"`。
- 若 `0 < |x| < 1e-6` 或 `|x| >= 1e21` → 指数形式 `d.ddde±k`（如 `1e-7`、`1e+21`、`1.5e+300`）；
- 否则十进制、无多余 `.0`：`1`、`2`、`4.5`、`0.000001`、`1234567890123456800`。
- `JSON.stringify` 对非有限数字与 `-0` → `null` / `0`。

Rust 的 `f64::to_string` 不同（指数阈值 `1e16`/`1e-5`，指数不带 `+`）。实现
`js_number_to_string(f64)`（最短数字位 + ECMA 格式规则）与 `js_json_number(f64)`
（NaN/±Inf → `null`，`-0` → `0`，否则 `js_number_to_string`）。用 §22.7 的值验证。

### 22.4 `JSON.stringify(value, null, 2)`（`toLlmContent` 回退使用）

- 每层缩进 2 空格；`\n` 换行；无尾随空格或换行。
- 容器内的空对象/数组内联为 `{}` / `[]`。顶层空 `[]`/`{}` 同理。
- `key:` 之后的嵌套容器另起一行并缩进。
- `undefined` 对象值省略；`undefined` 数组元素 → `null`。

实现 `js_json_pretty(value) -> String`，或用 serde_json 的 2 空格 `PrettyFormatter` 再后处理：
serde_json 的 pretty 也会把空数组/对象内联（`[]`/`{}`），布局相同；用 §22.7 的测试验证。数字必须用 JS 格式化。

### 22.5 JSON.parse

- 重复键：后者胜（serde_json 相同）。
- 超过 2^53 的整数：JS 解析为 f64（精度丢失）；serde_json 在未启用 `arbitrary_precision` 时对
  `Value::Number` 中的大整数会保留 `u64`/`i64`，精度比 JS 高。为精确等价，移植中把数字解析为 f64
  （自定义反序列化，或接受差异；MCP 载荷可能含大整数，如服务器 id；只有当工具 schema 含此类数字时往返输出
  才会不同——不常见但需记录）。JS 中 `JSON.stringify(9007199254740993)` 得到 `9007199254740992`。
- 非法 JSON：抛 `SyntaxError`；Rust 消息不同（可接受的偏差；无测试断言）。

### 22.6 UTF-16 与 UTF-8 字符串

- `String.length`、`slice`、`split`、`indexOf`、正则都按 UTF-16 代码单元。
  - `describeHttpFailure` 按 500 代码单元切片（可能切断代理对；后续编码时变成 U+FFFD）。Rust `String`
    不能容纳孤立代理项。桥接：用 `encode_utf16` 按 UTF-16 单元切片，孤立代理项替换为 U+FFFD（文档化偏差）。
  - coding-agent 的 `stderrTail = transport.stderr.trim().slice(-2000)` 在本单元之外。
  - `MAX_ERROR_BODY_BYTES` 切片有同样的注意点。
- `Buffer.byteLength(value)` 计 UTF-8 字节（孤立代理项 → 3 字节）。`TextEncoder` 为 UTF-8。
- `renderTrace` 的行长度不用于宽度；无宽度计算。
- JSON 转义把孤立代理项转义为 `\ud800`。

### 22.7 已验证的参考值（用作单元测试向量）

```
JSON.stringify({b:1,"2":2,a:3,"1":4})                 = '{"1":4,"2":2,"b":1,"a":3}'
Object.entries({"2":2,b:1,"1":4,a:3})                 = [["1",4],["2",2],["b",1],["a",3]]
JSON.stringify(4.5)= "4.5"; (2.0)="2"; (1e21)="1e+21"; (1e-7)="1e-7"; (0.1+0.2)="0.30000000000000004"; (-0)="0"; NaN=null; Infinity=null
JSON.stringify({a:-0,b:1.0,c:1e21,d:1e-7,e:NaN,f:Infinity,g:[1.5,2]}) = '{"a":0,"b":1,"c":1e+21,"d":1e-7,"e":null,"f":null,"g":[1.5,2]}'
String(1e20)="100000000000000000000"; String(1e21)="1e+21"; String(1e-6)="0.000001"; String(1e-5)="0.00001"; String(1.234e-7)="1.234e-7"
String(123456789012345678)="123456789012345680"; JSON.stringify(1234567890123456789)="1234567890123456800"
String(2**53)="9007199254740992"; String(2**53+2)="9007199254740994"
JSON.stringify("line one\n\"line two\"") = '"line one\\n\\"line two\\"'
JSON.stringify("héllo \u0001 \t") = '"héllo \\u0001 \\t"'
JSON.stringify("a\u0000b\bb\fc\r") = '"a\\u0000b\\bb\\fc\\r"'
JSON.stringify("\u007f") = '"\u007f"'（DEL 原样输出，不转义）；JSON.stringify("\u2028") = '"\u2028"'（U+2028/2029 不转义）
JSON.stringify({n:1},null,2) = '{\n  "n": 1\n}'
JSON.stringify([1,2],null,2) = '[\n  1,\n  2\n]'
JSON.stringify({a:[],b:{},c:[{}],d:[[]]},null,2) = '{\n  "a": [],\n  "b": {},\n  "c": [\n    {}\n  ],\n  "d": [\n    []\n  ]\n}'
JSON.stringify(structuredContent=[1,2]) = '[\n  1,\n  2\n]'
new URLSearchParams({grant_type:"authorization_code", code:"a b", redirect_uri:"http://127.0.0.1/cb?x=1"}).toString()
  = 'grant_type=authorization_code&code=a+b&redirect_uri=http%3A%2F%2F127.0.0.1%2Fcb%3Fx%3D1'
URLSearchParams: "~!*'()" → "%7E%21*%27%28%29"（注意 * 不编码）；"é 中" → "%C3%A9+%E4%B8%AD"
new URL("https://x.example/mcp?z=1") + set("state","s t") → "https://x.example/mcp?z=1&state=s+t"
new URL("/.well-known/oauth-protected-resource/mcp", "https://x.example").href = "https://x.example/.well-known/oauth-protected-resource/mcp"
new URL("/", "https://x.example/mcp").href = "https://x.example/"
new URL("HTTP://EXAMPLE.com:80/a").href = "http://example.com/a"
new URL("https://x.example/a/../b").href = "https://x.example/b"
new URL("com.example.app:/callback").protocol = "com.example.app:"
URLSearchParams.get on `?scope=&state=s` → ""（空字符串）
Headers 迭代：名小写且按字母升序
Date.now() 整数毫秒；performance.now() 小数毫秒
```

---

## 23. 测试清单

### 23.1 `packages/telemetry/test/*`（vitest）

测试计数（已逐文件核对）：`telemetry.test.ts` 5、`metrics.test.ts` 4、`conformance.test.ts` 2、
`propagation.test.ts` 6、`render.test.ts` 8、`store.test.ts` 3、`store-conformance.ts` 6（被 `store.test.ts`
以 `describe` 名 **"in-memory telemetry store conformance"** 注册；第二个 `describe` 是 **"in-memory store snapshots"**）、
`propagation-worker.ts` 0（fixture）。telemetry 共 34 个 `it`（含 6 个共享 store 测试）。

| 文件 | 测试（逐字标题） | 断言要点 |
|---|---|---|
| `telemetry.test.ts` | `telemetry schemas` › *preserves serializable definitions and infers exact attributes* | `defineTelemetrySchema` 返回同一对象；`JSON.stringify` 不抛；类型级检查（运行时无操作） |
| | *combines schema vocabularies and binds child starters to their parent spans* | 子 starter 的父级是回调 span；结果 42；同步/异步同值 rejection；类型级失败 |
| | `NOOP_TELEMETRY_CONTEXT` › *admits callbacks synchronously and reuses one inert span* | 同步接纳；`child === span`；结果；冻结；能力恒等 |
| | *preserves synchronous and asynchronous rejection values* | 同值 rejection |
| | *ignores typed recording payloads* | 记录调用无效果；结果 42 |
| `metrics.test.ts` | `metrics` › *binds schema names, kinds, metadata, and exact measurement attributes* | 类型化指标接线；测量字段；exemplar 上下文；类型级失败；`JSON.stringify(traffic)` 不抛 |
| | *requires histogram attributes when the schema declares them* | 类型级；带属性记录 |
| | *returns detached measurement snapshots including exemplars and boundaries* | boundaries/attributes/context 的变异隔离 |
| | *shares frozen no-op instruments* | 单例恒等、冻结、记录被忽略 |
| `conformance.test.ts` | 14 个一致性 case（group/name 见 §7.3） | 同 §7.3 |
| | *returns detached snapshots without exposing mutable recording state* | 未结算 span 快照 `settled:false`、无 `endSequence`；结算后 `endSequence:1`；attributes/events 变异隔离 |
| `propagation.test.ts` | *continues a parent in another Node process without shared recorder state* | 用 carrier JSON 启动 `propagation-worker.ts`；worker 记录一个 span，`parentContext === carrier`、`parentId === null`、`context` 不同 |
| | *isolates crossed concurrent remote parents and links shared roots* | 并发 provider extract；父上下文分别匹配各自 caller span；link 目标与根 |
| | *detaches carrier and link snapshots and retains settled target identities* | inject 快照分离；link context/attributes 复制；已结算目标仍可 link |
| | *links a new root to an extracted remote target* | 新根 link 到外部 carrier（经 extract）；迟到 `addLink` 也记录；attributes 为 `{}` |
| | *uses the root for absent and unknown carriers* | `extract(undefined|{}|{unknown}) === telemetry`；NOOP propagation 行为 |
| | *treats recognized carrier identifiers as opaque strings* | 外部字符串被记为 parentContext |
| `render.test.ts` | 8 个测试（标题见 §23.1 末尾） | 精确字符串、去重、顺序、转义、2000 层深度、空输入 |
| `store.test.ts` | `in-memory store conformance`（6 个 store-conformance 测试） | §8 |
| | *records wall-clock boundaries and relative event times without changing normalized event snapshots* | mock `Date.now`/`performance.now`；精确 StoredSpan；`getSpans()[0]` 无 `startedMs` |
| | *maps raw metric measurements to count-one rows without exposing private timing in getMetrics* | 区间 20000；行 `[20000,1,2]`、`[20000,1,3]`；`getMetrics()` 精确对象 |
| | *detaches link attributes and error details* | link/status 的变异隔离 |
| `store-conformance.ts` | 共享 harness（6 个测试，§8） | 被 chord 复用 |
| `propagation-worker.ts` | 子进程 fixture | 读取 argv[2] 的 carrier、extract、记录 `remote`、打印 `getSpans()` JSON |

render.test.ts 的 8 个标题（逐字）：
1. `renders an unordered trace with attributes, durations, errors, events, and links`
2. `treats missing parents as roots and orders roots and children by start time`
3. `prints cross-trace links without following them, including cycles`
4. `deduplicates spans merged from stores by global ID`
5. `preserves input order for equal start times without mutating the input`
6. `escapes string attribute values without hiding their contents`
7. `renders deep trees without recursive calls`
8. `renders an empty input as an empty string`

### 23.2 `packages/mcp/test/*`（vitest）

测试计数（已逐文件核对）：`client.test.ts` 13、`content.test.ts` 2、`streamable-http.test.ts` 16、
`stdio.test.ts` 2（第二个是 `it.skipIf(process.platform === "win32")`）、`oauth.test.ts` 13（两个 `describe`：
`MCP OAuth` 10 个 + `OAuthCallbackServer pages` 3 个）。mcp 共 46 个 `it`。`test/helpers.ts` 与两个 fixture
不是测试。跑测试需要真实网络栈（回环 HTTP 服务器、子进程、`process.execPath`），Rust 移植建议保留
"回环 127.0.0.1:0 + 真实 TCP" 的形态而不是替换成纯内存 mock，因为 session/SSE/回调路径依赖真实 HTTP。

| 文件 | 测试 | 断言 |
|---|---|---|
| `content.test.ts` | `toLlmContent` › *passes text and images through and replaces other blocks with text* | 精确 `toLlmContent` 数组（§11） |
| | *falls back to the structured content as JSON when there are no blocks* | pretty JSON、`null`、有 content 时非对象原样 |
| `client.test.ts` | `McpClient` › *initializes the connection before exposing server information* | 精确请求消息（initialize id 1，然后 `notifications/initialized`）；getter |
| | *paginates tools and preserves protocol tool definitions* | 两页；`nextCursor: ""` 结束分页；额外字段保留 |
| | *lists and reads resources* | name 回退为 URI、页透传、校验错误逐字 |
| | *returns structured tool content and surfaces JSON-RPC errors* | 结果 + 带 code/data 的 `McpError` |
| | *renews the timeout on progress* | fake timers；progress token 等于请求 id；超时续期 |
| | *cancels aborted and timed-out requests* | `McpAbortError`；`notifications/cancelled` 的 reason 为 `"stop"`；`McpTimeoutError` |
| | *reports transport errors without failing pending requests* | onError 收到 transport 错误；请求仍 resolve |
| | *accepts servers that answer with an older protocol version* | 接受 `2024-11-05`；拒绝 `1999-01-01`（"unsupported protocol version"）；状态 closed |
| | *defaults missing tool result content to an empty list* | `content: []`；非数组 content 被拒 |
| | *accepts structured content that is not an object* | 数组、null、字符串、数字、布尔 |
| | *does not send notifications/cancelled for a timed-out initialize* | initialize 不发送取消 |
| | *notifies close listeners once when the transport drops* | pending 以 "MCP connection closed" reject；状态 closed；close 监听只一次 |
| | *answers roots/list and dispatches notifications* | `{jsonrpc, id:"roots", result:{roots:[...]}}`；listener 收到 `undefined` params |
| `stdio.test.ts` | `StdioTransport` › *connects to a newline-delimited MCP server and captures stderr* | listTools/callTool；数字 pid；stderr 含 "stdio fixture ready" |
| | *kills a server that ignores shutdown, including its children*（win32 跳过，10 s） | close < 5 s；孙进程 PID 消失 |
| `streamable-http.test.ts` | `consumeSseStream` › *parses chunked CRLF events, comments, IDs, and multiline data* | §17.1 向量 |
| | *rejects events whose data lines exceed the limit without a blank line* | §17.1 向量 |
| | `StreamableHttpTransport` › *handles JSON and SSE responses with session and protocol headers* | 捕获/发送 session id；protocol version 头；GET 与 DELETE 发生 |
| | *classifies authentication failures* | `McpAuthRequiredError` 的 name/status/body/wwwAuthenticate |
| | *fails only the request whose SSE stream breaks* | 只发出一个错误；另一请求不受影响 |
| | *opens the GET stream after initialization and sends Last-Event-ID only when resuming* | GET 在 `notifications/initialized` 之后；初始无 last-event-id |
| | *resumes a response stream the server closed before answering* | 恢复头 `"1"`；priming 事件 + retry |
| | *fails a request whose response stream ends without an answer* | 精确消息 |
| | *reconnects the GET stream after it drops* | `[undefined, "g1"]`；两个通知 |
| | *rejects a request the server accepts without a response* | 202 → "without a response" |
| | *includes the response body in HTTP errors* | 精确消息 |
| | *hands 401 and insufficient-scope 403 responses to the auth provider with the rejected token* | seen `[{401,"old"},{403,"new"}]` |
| | *does not retry requests that were cancelled while authorization was recovering* | 只执行 `kept`；一次刷新 |
| | *closes the session with the last request's token, without asking the auth provider* | `token()` 调用次数不变；DELETE Authorization 为 `Bearer token-N`；带 session id |
| | *calls fetch without a receiver* | 严格 fetch 校验 `this` |
| | *classifies an expired established session* | `McpSessionExpiredError` |
| `oauth.test.ts` | `MCP OAuth` › *discovers, registers, authorizes with PKCE, and refreshes on 401* | 完整 discovery/DCR/PKCE/refresh；scope `org:read`；`resource` 参数；刷新保留请求的 scope |
| | *shares one refresh between concurrent 401s when refresh tokens rotate* | grants `["r1"]`；token 为 `a2`；`tokensExpireAt` > now+3.5e6 |
| | *asks for authorization instead of refreshing when the server needs more scope* | `McpOAuthAuthorizationRequiredError`；scope `repo read:org admin`；旧 token 保留 |
| | *binds persisted credentials to the exact MCP server URL* | 第二个 server 看不到 token |
| | *registers with an application_type derived from the redirect URIs unless one is set* | `["native","native","native","web","web"]` |
| | *rejects authorization metadata whose issuer does not match discovery* | `OAuthIssuerMismatchError` |
| | *rejects non-HTTP authorization endpoints, which the platform URL handler would launch* | `file:`、`search-ms:`、`ms-msdt:` → "Invalid authorization endpoint"（discovery 解析与 startAuthorization 都覆盖） |
| | *uses a configured authorization server metadata document as is* | 不校验 issuer；`REDIRECT`；resource 参数；非 https URL → `OAuthInsecureEndpointError` |
| | *exchanges a code only when its iss parameter names the authorization server* | 不匹配用例；`codes === ["matching","omitted","no-metadata"]` |
| | *stops when its signal aborts, without falling back to a redirect* | discovery 期间与 refresh 期间 abort；`AbortError`；最后一次停滞路径 |
| | `OAuthCallbackServer pages` › *renders plain text by default* | content-type、body、状态码 |
| | *rejects a response on another path than the expected one* | 400；"arrived on another redirect URI"；正确路径 200 |
| | *renders pages through renderPage* | html content-type；失败页 details；成功页；以 "Denied" reject |
| `test/fixtures/stdio-server.mjs` | fixture | 应答 initialize（`2025-06-18`）、tools/list、tools/call echo、ping；其他方法 `-32601 not found`；stderr "stdio fixture ready" |
| `test/fixtures/stubborn-server.mjs` | fixture | 忽略 SIGTERM/stdin EOF；派生孙进程向 stderr 打印 "grandchild <pid>" |
| `test/helpers.ts` | 辅助 | 绑定 127.0.0.1:0 的回环 HTTP 服务器；`readBody`；`closeServers` |

注意：`test/client.test.ts` 用 `createServer()` 与内存配对，并期望第二个请求 id 为 2（被中止的 `wait` 调用是 id 2）；
请求 id 会计入 initialize 与每个通过前置检查的请求（含超时/被拒绝/服务端报错的），但**不**计入在
`requireTransport` 或"signal 已 abort"处提前抛出的调用（见 §13.5 的实测序列）。

---

## 24. 建议的 Rust 模块布局

工作区成员（名字可与其他单元对齐调整）：

```
kiana/
  crates/
    kiana-telemetry/
      Cargo.toml            # name = "kiana-telemetry"; lib
      src/
        lib.rs              # 再导出；`pub mod render; pub mod testing;`
        contract.rs         # AttributeValue, SpanAttributes, SpanStatus, SpanLink, SpanOptions,
                            # TelemetryCarrier, TelemetryPropagation, TelemetryContext, TelemetrySpan,
                            # StoredSpan/Trace, TraceQuery, MetricQuery, StoredMetric, TelemetryStore
        metrics.rs          # InstrumentOptions, HistogramOptions, TelemetryCounter/UpDown/Histogram,
                            # TelemetryMetrics, TelemetryMetricDefinition, create_typed_metrics
        noop.rs             # NOOP_TELEMETRY_CONTEXT/METRICS/PROPAGATION（LazyLock 单例）
        memory.rs           # InMemoryTelemetryContext, Recorded* 类型, store 实现
        schema.rs           # TelemetrySchemaDefinition 等, define_telemetry_schema（恒等）, TypedSpanStarter
        render.rs           # render_trace
        jsval.rs            # JsValue, js_string, js_number_to_string, js_json_number, js_json_pretty, js_order
        testing/
          mod.rs            # create_telemetry_adapter_conformance
          types.rs          # TelemetryAdapterFixture, Factory, Case
          conformance.rs    # 14 个 case
      tests/                # 移植 telemetry 测试（telemetry、metrics、conformance、propagation、
                            # render、store、store_conformance 辅助）
    kiana-mcp/
      Cargo.toml            # name = "kiana-mcp"
      src/
        lib.rs              # 镜像 src/index.ts 的再导出
        protocol/
          mod.rs
          jsonrpc.rs        # JsonRpcId, 谓词, parse_json_rpc_message, McpError, 其他错误类型
          types.rs          # 常量与结构体（serde flatten 额外字段）
          content.rs        # ContentBlock, CallToolResult, LlmContent, to_llm_content
        client.rs           # McpClient + options + pending 表
        auth_provider.rs    # trait AuthProvider, UnauthorizedContext, McpFetch
        transports/
          mod.rs            # trait McpTransport, TransportEvents, DEFAULT_MAX_MESSAGE_BYTES
          stdio.rs
          streamable_http.rs# consume_sse_stream + StreamableHttpTransport + 错误类
          in_memory.rs
        oauth/
          mod.rs
          types.rs          # 元数据结构 + 解析器（Error("Invalid ...")）
          errors.rs
          discovery.rs
          flow.rs
          provider.rs
          callback.rs
        testing.rs          # create_in_memory_transport_pair
      tests/                # 移植 mcp 测试 + fixtures（可复用 Node fixtures，或改为 Rust/shell fixtures）
      tests/fixtures/*.mjs  # 在 Node 可用时直接复用；否则用 Rust 二进制
```

关键 Rust 结构体与 trait：

```rust
// contract
pub enum AttributeValue { String(String), Number(f64), Boolean(bool),
  StringArray(Vec<String>), NumberArray(Vec<f64>), BooleanArray(Vec<bool>) }
pub type SpanAttributes = indexmap::IndexMap<String, AttributeValue>;   // 设置时丢弃 undefined
pub enum SpanStatus { Ok, Error { error: Option<StatusError> } }
pub struct StatusError { pub name: String, pub message: String }

#[async_trait]
pub trait TelemetryPropagation: Send + Sync {
    fn inject(&self, context: &dyn TelemetryContext) -> Option<TelemetryCarrier>;
    fn extract(&self, carrier: Option<&TelemetryCarrier>) -> Arc<dyn TelemetryContext>;
}

#[async_trait]
pub trait TelemetryContext: Send + Sync {
    fn metrics(&self) -> Option<Arc<dyn TelemetryMetrics>>;
    fn propagation(&self) -> Option<Arc<dyn TelemetryPropagation>>;
    async fn start_span(&self, options: SpanOptions, callback: SpanCallback<'_>) -> JsResult;
    // SpanCallback = Box<dyn FnOnce(Arc<dyn TelemetrySpan>) -> BoxFuture<'_, JsResult> + Send + '_>
    // JsResult = Result<JsValue, JsValue>  （任意 rejection 值）
}

pub trait TelemetrySpan: TelemetryContext {
    fn add_link(&self, link: SpanLink);
    fn add_event(&self, name: &str, attributes: SpanAttributes);
    fn set_attributes(&self, attributes: SpanAttributes);
    fn set_status(&self, status: SpanStatus);
    fn identity(&self) -> Option<u64>;   // carrier/link 用的内部身份 token
}

#[async_trait]
pub trait TelemetryStore: Send + Sync {
    async fn find_traces(&self, query: Option<TraceQuery>) -> FindTracesResult;
    async fn get_trace(&self, trace_id: &str) -> Option<StoredTrace>;
    async fn find_metrics(&self, query: MetricQuery) -> Vec<StoredMetric>;
}

// memory
pub struct InMemoryTelemetryContext {
    state: Arc<Mutex<MemState>>,      // spans、measurements、next ids、scope
    // 注意：carrier 注册表不能是每实例字段；JS 是模块级 WeakMap，跨实例可见（见 §3.1）
    // carriers: Mutex<HashMap<u64, TelemetryCarrier>>,   ← 不要这样建模
    scope: String,
    clock: Arc<dyn Clock>,            // Date.now + performance.now，可 mock
}
```
`InMemoryTelemetryContext` 的 clone 共享状态（Arc），等价于 JS 对象引用；所有 `startSpan` 取 `&self`/`Arc<Self>`。

同步接纳：`start_span` 是 `async fn`，其首步同步创建并 push recorded span、注册 span 对象，然后调用回调 future。
Rust 的 async fn 体只在被 poll 时执行，所以调用方必须至少 poll 一次才能观察到接纳。为了让测试可移植，Rust 版本
的一致性 case 应在 `tokio::task::yield_now().await`/首次 poll 之后断言接纳，并在文档中说明"startSpan 返回前
回调已开始"是 JS 特有保证。若必须严格等价，提供 `fn start_span_now(...) -> impl Future`：先创建 span 并返回
future（回调在 future 内调用），测试调用后用 `futures::poll!`/`now_or_never` poll 一次。

错误类型镜像 TS：

```rust
#[derive(Debug, Clone, thiserror::Error)]
pub enum McpError { #[error("{message}")] Rpc { code: i64, message: String, data: Option<Value> },
   #[error("{0}")] Closed(String), #[error("MCP request timed out after {timeout_ms}ms")] Timeout { timeout_ms: f64 },
   #[error("{message}")] Aborted { message: String }, ... }
```
下游按变体匹配并读取 `.code`/`.status`；保留访问器方法。注意 `McpAbortError` 在 JS 中 `name = "AbortError"`，
Rust 需用 `error_name()` 之类的方法暴露（例如 `fn name(&self) -> &'static str`）。

`McpClient` 内部：`Mutex<ClientState>`，`HashMap<JsonRpcId, Pending>`，其中
`JsonRpcId` 是 `enum JsonRpcId { Number(f64), String(String) }`（JS 相等语义：`1` 与 `"1"` 不同；
数字按 f64 位比较，NaN 不可能出现，因为 `isJsonRpcId` 要求有限）。Pending 条目持有
`oneshot::Sender<Result<Value, JsError>>`、超时截止/`JoinHandle`、progress token、取消令牌与
`send_was_aborted` 标志。`McpClient` 应 `Arc` 共享；`connect`/`close` 用 mutex 串行化状态转换。

## 25. 最难逐字复现的部分

1. **JS 对象/属性键序**（整数样键前置）出现在每个序列化的属性包、请求/通知 body、renderTrace 输出与
   structuredContent JSON 中。`serde_json` 保留插入顺序但不模拟整数键前置；只有当服务器或调用方使用
   数字样式属性名（`"1"`）时才会出现，因此在边界加 `js_order` 重排并测试。
2. **ECMAScript 数字格式化**（`1e+21`、`1e-7`、`0.30000000000000004`、`-0` → `0`、`NaN`/`Infinity` → JSON
   的 `null`），用于 renderTrace、JSON body、错误消息、cursor 字符串。
3. **任意 rejection 值身份**：`startSpan` 必须用同一对象 throw/reject（包括 `undefined`）——需要 JS 值错误类型
   与 `Arc::ptr_eq` 风格的测试。
4. **同步回调接纳**（JS 在返回 promise 之前调用回调）与 Rust future 的惰性。
5. **`InMemoryTelemetryContext` 身份注册表**（WeakMap 语义）：只对适配器交出的上下文 inject/link、分离快照、
   已结算目标仍可 inject、惰性 span 不可 inject、远端 carrier 的 `Number(span)` 本地 id 计算（含 `NaN`）。
6. **`findTraces` 的 cursor/并列规则**：按 `"scope:id"` 字符串字典序比较、起点相同时 traceId 升序；
   分组顺序；最新在前的分页；`limit` 边界。
7. **两种排序的交汇**：`storedSpans`（先 `startedMs`，再字符串 id）与 `renderTrace`（按起点稳定排序、
   根/子顺序、迭代 DFS、重复 ID 覆盖）。
8. **SSE 解析器的字节记账**（UTF-8 字节上限含 `\n` 连接字节、逐行 CR 剥离、注释/字段规则、`id` 拒绝 NUL、
   `retry` 仅数字、跨 chunk 的流式 UTF-8 解码、EOF 派发）。
9. **Streamable HTTP 恢复状态机**（`received` 时重置 attempt、重试分类含 `TypeError`/`E*`/`UND_ERR` 码、
   指数退避、close 时 sleep 中止、priming 事件、仅恢复时发 `Last-Event-ID`、GET 流 405、健康期 `maxDelayMs`）。
10. **`send` 的 signal 语义**（#10598）：被取消/超时的请求在共享刷新之后不得派发；transport 在拿到 token 后、
    fetch 之前检查 `signal.aborted`；关闭时用 transport controller（而非调用方 signal）中止 fetch。
11. **头规范化**匹配 WHATWG `Headers`（只裁剪首尾、**不**折叠内部空白、重复用 `", "` 合并、小写查找、
    拒绝非法名），因为 coding-agent 会传入用户配置的头。
12. **URL/URLSearchParams 编码差异**：form-urlencoded 字符集、origin 尾斜杠规范化、`pathSuffix` discovery URL、
    `String(new URL("/", serverUrl))`。
13. **OAuth 流程顺序/重试规则**：discovery 缓存到 provider、`saveDiscoveryState`、`||` 的 scope 回退、
    `iss` 校验矩阵、刷新错误的下落规则、单个共享刷新（`inFlight`）、step-up scope 合并、
    `application_type` 推导、`invalid_client`/`invalid_grant` 单次重试、非安全端点检查（loopback 豁免）。
14. **Provider 更新串行化**（`writes` promise 链）与 server-URL 绑定（`own`）。
15. **回环回调服务器**的精确状态码/body/content-type 与 pending 状态语义（路径匹配、重复 state、超时、
   close 拒绝）。
16. **`McpClient` close/markClosed 幂等**与精确 rejection 错误/消息；取消通知的 reason
    （`"AbortError: This operation was aborted"` vs 自定义 reason 字符串）。
17. **stdio/HTTP body 的 `JSON.stringify(message)` 键序**（jsonrpc、id、method、params / jsonrpc、method、params /
    jsonrpc、id、result 或 error），因为服务器可能比较原始字节，测试也比较消息对象。
18. **stdio 服务器的进程组关闭**（stdin EOF → 500 ms → SIGTERM 组 → closeTimeout → SIGKILL 组，退出后再一次
    SIGTERM）与退出钩子。
19. **服务端请求 handler 的响应**，包括 `result ?? {}` 与错误映射（`McpError` code 透传并带 `data`，否则 `-32603`）。
20. **`toLlmContent` 的 pretty JSON 回退**与精确占位字符串。

## 26. 验收清单（评审时逐项打勾）

- [ ] §19.2 公示的所有导出名与类型名存在且大小写一致。
- [ ] 错误 `name` 与消息匹配 §13、§17.2、§18.2（含 `McpAbortError` 的 `"AbortError"`）。
- [ ] `renderTrace` 逐字节复现 §6 的 8 个测试字符串。
- [ ] `toLlmContent` 逐字节复现 §11 的两个内容测试。
- [ ] 内存适配器：结算顺序、endSequence、trace 继承、外部 id、NaN 本地 id、limit/分页行为、分离快照、
      区间下取整（`Math.floor(now/10000)*10000`）。
- [ ] No-op：单一冻结 span 身份、同步接纳、同值 rejection。
- [ ] 一致性套件：14 个 case 的 group/name 字符串精确。
- [ ] MCP client：状态机、id、超时、进度续期、取消 reason、close 幂等。
- [ ] Stdio：分帧、CR 剥离、空白跳过、大小上限、stderr 尾部、关闭阶梯。
- [ ] SSE：字节上限、CRLF、注释、id/retry、多行、EOF 冲刷、恢复。
- [ ] HTTP：头集合/顺序、session 捕获、GET 流启动触发、重试、auth 一次性交接、刷新后取消的不派发、
      用 last token 发 DELETE、错误分类。
- [ ] OAuth：discovery URL 顺序/回退、issuer 校验、resource 校验、PKCE、DCR body、token 错误优先级、
      刷新下落、iss 矩阵、step-up scope、provider 绑定/串行化、回调服务器页面。
- [ ] 跨单元 serde 形状：`TelemetryCarrier`、`StoredSpan`、`StoredTrace`、`StoredMetric`、`McpOAuthState`、
      `CallToolResult`、`Tool`/`Resource`/`ResourceTemplate` 保留未知字段。

## 27. 错误与用户可见字符串总表（逐字核对用）

本表汇总两个包中所有抛出的错误、合成的协议错误与面向用户的文本；移植时逐条对照。

### 27.1 telemetry

| 位置 | 文本 | 类型 |
|---|---|---|
| `testing/conformance.ts` | `Expected target span` | `Error` |
| `testing/conformance.ts` | `Expected source span` | `Error` |
| `testing/conformance.ts` | `Expected propagation on each fixture` | `Error` |
| `testing/conformance.ts` | `Expected callback span` | `Error` |
| `testing/conformance.ts` (`findSpan`) | `` `Expected recorded span ${name}` `` | `AssertionError`（node:assert `ok`） |
| `testing/conformance.ts` (`rejectsWithSameValue`) | `Expected operation to reject` | `AssertionError`（node:assert `fail`） |

### 27.2 mcp — 错误类

| 类 | name | 消息构造 |
|---|---|---|
| `McpError` | `McpError` | `message` |
| `McpConnectionClosedError` | `McpConnectionClosedError` | `message`（默认 `MCP connection closed`） |
| `McpTimeoutError` | `McpTimeoutError` | `` `MCP request timed out after ${timeoutMs}ms` `` |
| `McpAbortError` | `AbortError` | `message`（默认 `MCP request aborted`） |
| `McpHttpError` | `McpHttpError` | `message`；字段 `status`、`body` |
| `McpAuthRequiredError` | `McpAuthRequiredError` | `MCP server requires authentication`；`status=401`；`wwwAuthenticate` |
| `McpSessionExpiredError` | `McpSessionExpiredError` | `MCP session expired`；`status=404` |
| `OAuthError` | `OAuthError` | `message || code`；字段 `code`、`errorUri` |
| `OAuthIssuerMismatchError` | `OAuthIssuerMismatchError` | `` `OAuth issuer mismatch: expected ${fmt(expected)}, received ${fmt(received)}` ``，`fmt = undefined → "none" : JSON.stringify` |
| `OAuthInsecureEndpointError` | `OAuthInsecureEndpointError` | `` `Refusing to send OAuth credentials to non-HTTPS endpoint ${endpoint}` `` |
| `OAuthRegistrationError` | `OAuthRegistrationError` | `` `OAuth dynamic client registration failed with status ${status}: ${body}` `` |
| `McpOAuthAuthorizationRequiredError` | `McpOAuthAuthorizationRequiredError` | `MCP OAuth authorization requires user interaction` |

### 27.3 mcp — 普通 `Error` 与合成错误

| 位置 | 文本 / 条件 |
|---|---|
| `client.ts` | `` `Cannot connect MCP client in ${state} state` `` |
| `client.ts` | `` `MCP server selected unsupported protocol version ${protocolVersion}` `` |
| `client.ts` | `` `MCP ${method} returned duplicate cursor: ${nextCursor}` `` |
| `client.ts` | `` `MCP ${method} exceeded ${MAX_LIST_PAGES} pages` ``（1000） |
| `client.ts` | `` `Received response for unknown MCP request ${String(id)}` ``（emitError，不抛） |
| `client.ts` | 服务器请求无 handler 时回 `-32601`，消息 `` `Method not found: ${method}` `` |
| `client.ts` | `validateInitializeResult` → `McpError(-32600, "Invalid MCP initialize result")` |
| `client.ts` | `Invalid MCP ${method} result` / `Invalid entry in MCP ${method} result` / `Invalid MCP ${method} cursor`（均 `-32600`） |
| `client.ts` | `Invalid MCP resources/read result` / `Invalid contents in MCP resources/read result`（`-32600`） |
| `client.ts` | `Invalid MCP tools/call result`（`-32600`） |
| `client.ts` | `McpError(-32600, "Received invalid JSON-RPC message")`（emitError） |
| `jsonrpc.ts` | `McpError(-32600, "Invalid JSON-RPC message")`（parseJsonRpcMessage） |
| `stdio.ts` | `MCP stdio transport already started` |
| `stdio.ts` | `` `MCP stdio message exceeds ${maxMessageBytes} bytes` `` |
| `stdio.ts` | `MCP stdio server closed with an incomplete JSON-RPC message` |
| `stdio.ts` | 发送时未启动/已关闭/不可写 → `McpConnectionClosedError()`（默认消息） |
| `in-memory.ts` | `In-memory MCP transport already has a peer` |
| `in-memory.ts` | `McpConnectionClosedError()` / `McpConnectionClosedError("In-memory MCP peer is not connected")` |
| `streamable-http.ts` | `MCP Streamable HTTP transport already started` |
| `streamable-http.ts` | `` `MCP server accepted request ${method} without a response` ``（`McpHttpError(status)`） |
| `streamable-http.ts` | `` `Unsupported MCP response content type: ${type ?? "missing"}` `` |
| `streamable-http.ts` | `` `Unsupported MCP GET response content type: ${type ?? "missing"}` `` |
| `streamable-http.ts` | `` `MCP HTTP request failed with status ${status}${snippet ? `: ${snippet}` : ""}` `` |
| `streamable-http.ts` | `` `MCP SSE event exceeds ${maxEventBytes} bytes` `` |
| `streamable-http.ts` | 合成响应错误 `` `MCP response stream failed: ${reason}` ``，`code -32603`；`reason` 默认 `stream ended without a response` |
| `streamable-http.ts` | `MCP server-to-client stream dropped and could not be reopened`（emitError） |
| `oauth/types.ts` | `` `Invalid ${name}` ``（`name` 取值见 §18.1）；`Invalid response_types_supported`；`Invalid expires_in` |
| `oauth/discovery.ts` | `` `HTTP ${status} loading OAuth protected resource metadata` `` |
| `oauth/discovery.ts` | `` `HTTP ${status} loading authorization server metadata from ${url}` `` |
| `oauth/discovery.ts` | `` `Protected resource ${metadata.resource} does not match MCP server ${requested}` `` |
| `oauth/flow.ts` | `client_secret_basic requires a client secret` |
| `oauth/flow.ts` | `Authorization server does not support authorization codes` |
| `oauth/flow.ts` | `Authorization server does not support PKCE S256` |
| `oauth/flow.ts` | `` `Invalid authorization endpoint ${url.href}` `` |
| `oauth/flow.ts` | `` `HTTP ${status}: ${text}` ``（`OAuthError("server_error", ...)`） |
| `oauth/flow.ts` | `Authorization server does not support dynamic client registration` |
| `oauth/flow.ts` | `Invalid OAuth client metadata URL` |
| `oauth/flow.ts` | `OAuth client information is missing during code exchange` |
| `oauth/flow.ts` | `OAuth client information cannot be persisted` |
| `oauth/provider.ts` | `No OAuth PKCE code verifier is stored` |
| `oauth/callback.ts` | `OAuth callback server did not bind to TCP` / `OAuth state is already pending` / `OAuth callback timed out` / `OAuth callback server closed` / `The authorization response arrived on another redirect URI` / `OAuth callback did not include an authorization code`；失败页 `error` 参数的值作为 `Error` 消息 |

### 27.4 浏览器可见页面文本（逐字）

| 页面 | 状态码 | 文本 |
|---|---|---|
| 成功 | 200 | `Authorization complete. You may close this window.` |
| 未知路径 | 404 | `Not found` |
| state 缺失/过期 | 400 | `Invalid or expired OAuth state` |
| 路径不匹配 | 400 | `Unexpected redirect URI` |
| 授权失败 | 200 | `Authorization failed. You may close this window.` + 空行 + `error_description ?? error` |
| 缺 code | 400 | `Missing authorization code` |

content-type：`text/plain; charset=utf-8`（默认）或 `text/html; charset=utf-8`（`renderPage`）；
有 `renderPage` 时附加 `cache-control: no-store`。

### 27.5 `WWW-Authenticate` 解析的测试向量

在真实 TS 实现上验证过的结果（`JSON.stringify` 会省略值为 undefined 的键；返回对象总是有
`resourceMetadataUrl`/`scope`/`error`/`errorDescription` 四个键，下表的 `{}` 实际是四个 undefined 值的键）：

```
'Bearer resource_metadata="https://auth.example/.well-known/oauth-protected-resource", scope="repo read"'
  → {resourceMetadataUrl: URL("https://auth.example/.well-known/oauth-protected-resource"), scope: "repo read"}
'Bearer error="insufficient_scope", scope="repo admin", error_description="need admin"'
  → {scope: "repo admin", error: "insufficient_scope", errorDescription: "need admin"}
'Basic realm="x"'                     → {}                 // scheme 不是 bearer/dpop，直接返回空对象
'Bearer scope=""'                     → {scope: undefined}  // 空值视为缺省
'dpop resource_metadata="not a url"'  → {resourceMetadataUrl: undefined}   // new URL 失败被吞
'Bearer,error="insufficient_scope"'   → {}                 // 第一个 token 是整个 "bearer,error=..."，scheme 不匹配
'xerror="insufficient_scope"'         → {}                 // 没有 scheme
'bearer error=insufficient_scope'     → {error: "insufficient_scope"}      // 无引号值可解析
null / ""                             → {}
```

`needsAuthorization`（注意：与 `parseWwwAuthenticate` 不同，这里用正则直接搜头字符串）：
401 恒 true；403 需头中出现 `error="insufficient_scope"`（引号可省），且其前一字符是行首、空白或逗号——
所以 `xerror="insufficient_scope"` 不匹配，而 `Bearer,error="insufficient_scope"` **匹配**。
**已实测的边界**（正则 `/(?:^|[\s,])error="?insufficient_scope"?/i`，**无尾边界**）：
- `Bearer error=insufficient_scopeX` → **匹配**（`X` 不被检查）；
- `Bearer error="insufficient_scopeX"` → **匹配**（同上）；
- `Bearer errors="insufficient_scope"` → 不匹配（`errors` 后不是 `=`）；
- `Bearer error = "insufficient_scope"`（`=` 两侧有空格）→ **不匹配**；
- `Bearer error=""insufficient_scope""` → 不匹配（多了一层引号）。

## 28. 未决问题

1. **文档语言**：本规格用中文书写（遵循用户的"说中文"要求）；源 TS 注释与 README 为英文。Rust 代码的
   doc comment 建议保持英文以与仓库其他单元一致，仅本规格与面向用户的说明用中文。若评审要求规格英文，可整篇
   机械翻译，术语以中英并列形式给出即可。
2. **`findTraces` 的非正 `limit`**：准确规则见 §3.8（`Array#slice` 的 ToIntegerOrInfinity 语义），
   不是简单的"`limit <= 0` 就抛"。摘要：`0` 在有匹配时抛
   `TypeError: Cannot read properties of undefined (reading 'spans')`；`-k` 返回前 `max(0, n-k)` 个并在
   `n-k > 0` 时带 cursor（`n` 为匹配数），`n-k <= 0` 时抛同一个 `TypeError`（空 store 时任何负数都抛）；
   `NaN` 返回 0 个 trace、无 cursor、不抛；
   `Infinity` 返回全部、无 cursor；小数向下取整。需决定 Rust 复刻为 `Err`、返回空页，还是保留 panic 语义
   （无测试覆盖）。
3. **Node 错误文本保真度**：`JSON.parse` 的 `SyntaxError` 文本、`fetch` 的 `TypeError`/`UND_ERR_*` 消息、
   `AbortSignal` 的 `DOMException` 消息（`"AbortError: This operation was aborted"`）在 Rust 中无法逐字复刻。
   已确认的测试相关点只有"取消 reason 的字符串形式"（客户端构造，可精确复刻）；其余建议用等价但不同文本的
   错误，并在测试中只断言错误类别。
4. **仅类型层面的保证**：`defineTelemetrySchema`/`createTypedSpanStarter`/`createTypedMetrics` 的编译期
   schema 校验、`ExactTelemetryAttributes` 的精确属性、重复 span/metric 名的类型级拒绝，在 Rust 中丢失。
   是否需要用过程宏/const 泛型补回部分检查（成本高、收益低）需产品决策；当前建议只保留运行时行为。
5. **Windows 支持**：`cross-spawn` 的 `.cmd` shim、`taskkill /T /F`、`USE_PROCESS_GROUPS` 在 Rust 中需要
   单独的 Windows 实现。本单元默认目标为 Linux/macOS（与 CI 一致），Windows 分支可先返回 `Unsupported` 并
   在文档标注。
6. **大整数 JSON 精度**：JS 把超过 2^53 的整数解析为 f64（丢精度），serde_json 默认保留 u64/i64。是否要在
   `kiana-mcp` 的 JSON 解析中强制 f64（完全等价）还是接受差异（保真但可能与 JS 输出不同），需在实现时拍板。
7. **同步接纳的可移植性**：JS 保证 `startSpan` 返回前回调已开始执行；Rust future 惰性。测试策略（首次 poll
   后断言 vs 提供 `start_span_now`）见 §24，需与测试基础设施统一。

---

## 29. 版本相关行为来源（CHANGELOG 中的契约决定）

CHANGELOG 记录了若干"看似可选、实为契约"的行为，移植时不要把它们当作实现细节丢掉：

telemetry `1.4.0`（Breaking Changes / Added）——**准确范围**：telemetry 的 CHANGELOG 只有 `[Unreleased]`（空）、
`1.4.0` 与 `0.84.0` 三个条目有内容，其余版本（`1.3.0`…`0.84.1`）只有标题、无正文。因此 telemetry 侧**没有**
更早的行为契约可挖；凡是 §2–§8 未写的行为一律以源码与测试为准。

- 适配器信任类型化输入；畸形值行为未指定。carrier extract 只检查必需的字符串身份，不校验格式。
- 适配器必须实现 `TelemetrySpan.addLink()`；规范化 span 快照包含 carrier 形状的 context/parent 身份与有序 links。
- 带可选 metric/propagation 能力的适配器必须在根、span、extracted parent 上通过
  `TelemetryContext.metrics`/`.propagation` 携带同一能力对象。
- `NOOP_TELEMETRY_CONTEXT` 的类型是 `TelemetrySpan`（与共享惰性 span 一致）。
- `StoredSpan`/`StoredTrace`/`TelemetryStore` 是后端无关契约；`renderTrace` 渲染纯文本树，合并的 span 按 ID 去重，
  从不跟随 links。

mcp 未发布条目与 `1.2.0`/`1.1.0`/`1.0.x`/`1.0.0`/`0.99.2`（这些决定解释了 §17/§18 的现状）：
- 未发布 Fixed：OAuth 元数据解析与 `startAuthorization()` 只接受 `http:`/`https:` URL（拒绝 `file:`、
  `search-ms:`、`ms-msdt:` 等会被平台 URL handler 启动的协议）；HTTP 错误 body 摘录必须是副本
  （`${slice} `.slice(0,-1)`），不能是保留整个 body 的 V8 slice（#9265）。
- `1.2.0`：`CallToolResult.structuredContent` 从 `Record<string, unknown>` 改为 `unknown`，`callTool()` 接受任意
  JSON 值（含数组与 `null`），不再报 "Invalid MCP tools/call structured content"（#10416）；
  `authorizeMcp()` 在无法校验 `iss`（没有发现元数据）时抛 `OAuthIssuerMismatchError`，此时 `expected` 为
  `undefined`（RFC 9207）；`McpTransport.send()` 增加可选 `signal`，`McpClient` 在请求结算时 abort 它，
  避免授权刷新后重试已取消/超时的请求（#10598）。
- `1.1.0`：`authorizeMcp()`、`registerClient()`、token 请求函数与 discovery 函数增加 `signal`；被 abort 的刷新
  不再回退到新授权（#10565）；`StreamableHttpTransport.close()` 不再调用 `AuthProvider.token()`，
  DELETE 复用最后一次请求的 token（#10565）。
- `1.0.4`：`registerClient()` 发送 `application_type`（MCP SEP-837），未设置时由 `redirect_uris` 推导：
  loopback 主机与自定义 scheme 为 `native`，否则 `web`（#10493）。
- `1.0.3`：`OAuthClientProvider.clientMetadataDocument(metadata)` 取代 `clientMetadataUrl`；在没有存储 client
  information 时调用（即使服务器未声明支持），document 不再作为 client information 存储（#10302）；
  `McpOAuthProviderOptions.clientMetadataDocument`、`OAuthCallbackServerOptions.extraPaths`、
  `waitForCallback(state, path)` 的 path 参数（其他路径上的响应被拒）也在该版本加入。
- `1.0.0`：`authorizationServerMetadataUrl` 选项（#10172）；`iss` 选项（RFC 9207）；`stepUpScope()`；
  空/`null` 可选字段视为缺省（#10266）；`expires_in: null` 不再标记为已过期；受保护资源元数据中的非法 URL
  改为回退到 MCP 服务器 origin；空的请求 scope 不再覆盖下一个来源；`nextCursor: ""`/`null` 不再导致重复 cursor
  错误；step-up 授权合并已授予的 scope（SEP-2350）。
- `0.99.2`：`fetch`（含 `UnauthorizedContext.fetch`）不得带接收者调用（#10188）。

这些条目对应的测试都已在 §23 列出；若移植后某个测试通过但上述"来源"行为被改坏（例如把 `scope=""` 当作有效
scope），说明测试覆盖不足，应补测试而不是放宽实现。

---

## 30. Audit additions（对抗性完整性审计补充）

以下条目是审计时逐文件核对源码、测试、README 与 CHANGELOG 后补充的遗漏/更正；正文中已就地修正的内容
不在此重复，这里只列此前未写入或需要额外强调的契约点。凡标"实测"的结论，均在 Node 26.12.1 上直接运行
真实 TS 源码（`--experimental-strip-types`/原生类型剥离）得到，不是从注释推断的。

### 30.1 模块级导出 ≠ 包根导出（Rust 公开 API 的边界）

`packages/mcp` 的若干 `src/**` 文件导出了名字，但**没有**从 `src/index.ts` / `src/oauth/index.ts` /
`src/testing/index.ts` 再导出（下表按文件列出全部此类名字，便于核对 Rust 的 `pub` 可见性粒度）。移植时这些应当存在（测试直接按相对路径 import 它们），但**不应**进入
`kiana-mcp` crate 的根 `pub use`，否则公开 API 会大于 TS 包：

| 名字 | 定义于 | 是否包根导出 | 谁在用 |
|---|---|---|---|
| `consumeSseStream`、`SseEvent`、`ConsumeSseOptions` | `transports/streamable-http.ts` | 否 | `test/streamable-http.test.ts` 直接 `import { consumeSseStream, type SseEvent } from "../src/transports/streamable-http.ts"` |
| `StreamableHttpReconnectOptions` | 同上 | 否（仅作为 `StreamableHttpTransportOptions.reconnect` 的类型引用） | 类型层面 |
| `isObject`、`toError`、`isJsonRpcId` | `protocol/jsonrpc.ts` | 否 | 包内（client、transports、oauth/provider 经 jsonrpc 间接使用） |
| `TransportEvents`（抽象类）、`DEFAULT_MAX_MESSAGE_BYTES` | `transports/transport.ts` | 否 | stdio/in-memory/streamable-http 继承与使用；测试不直接用 |
| `McpTransportMessageListener`、`McpTransportErrorListener`、`McpTransportCloseListener`、`McpTransportSendOptions` | 同上 | **是**（4 个都在 §19.2 的 67 个名字里，经 `index.ts` 的 `export type { ... } from "./transports/transport.ts"`） | coding-agent 的 `McpTransportFactory` 等 |
| `safeUrl`、`object`、`compact`、`requiredString`、`absent`、`optionalString`、`optionalStrings`、`isHttpUrl`、`parse*` | `oauth/types.ts` | 否 | 包内 |
| `loopback`、`applicationType`、`secureEndpoint`、`selectClientAuthMethod`、`applyClientAuthentication`、`pkce`、`tokenRequest`、`withScope`、`runFlow` | `oauth/flow.ts` | 否 | 包内 |
| `pathSuffix`、`isDiscoveryMiss`、`field`、`fetchMetadata`、`discard` | `oauth/discovery.ts` | 否 | 包内 |
| `plainText` | `oauth/callback.ts` | 否 | 包内 |
| `DEFAULT_REQUEST_TIMEOUT_MS`、`MAX_LIST_PAGES` | `client.ts` | 否 | 包内 |
| `DEFAULT_MAX_STDERR_BYTES`、`DEFAULT_CLOSE_TIMEOUT_MS`、`STDIN_CLOSE_GRACE_MS`、`USE_PROCESS_GROUPS` | `transports/stdio.ts` | 否 | 包内 |
| `MAX_ERROR_BODY_BYTES`、`ERROR_MESSAGE_BODY_CHARS`、`DEFAULT_RECONNECT_*` | `transports/streamable-http.ts` | 否 | 包内 |

Rust 建议：`kiana-mcp` 的 `lib.rs` 只 `pub use` §19.2 的 113 个名字；SSE 解析器等放在公开但**不**在根再导出的
子模块（例如 `kiana_mcp::transports::streamable_http::consume_sse_stream`），并在测试里按模块路径使用，
以保持"包根 API 恰好等于 TS 包根 API"。

telemetry 侧同理：`bindTypedSpanStarter`（`index.ts:427`）是**私有**函数，不是导出，不要泄漏到 crate 根。

### 30.2 `JsonRpcId` 的 JS 相等语义（Map 键）

`McpClient.pending` / `progressRequests` / `incoming` 都是 `Map<JsonRpcId, ...>`。JS 的 Map 键比较是
**SameValueZero**：数字 `1` 与字符串 `"1"` 是不同键；`-0` 与 `0` 是**同一**键；`NaN` 可以作键（且
`NaN` 等于 `NaN`），但 `isJsonRpcId` 要求 `Number.isFinite`，所以 `NaN` 进不了这三个表。
Rust 的 `enum JsonRpcId { Number(f64), String(String) }` 必须手写 `Hash`/`Eq`（不能用 `f64` 的默认，
也不要用 `PartialEq`），并且 `-0.0` 与 `0.0` 的哈希必须相同（用 `to_bits` 前先 `+ 0.0` 归一化，或
`if x == 0.0 { 0.0 } else { x }`）。`progressRequests` 的键是 progressToken，值与 pending 的键同型。

另：`handleProgress` 只有在 `progressRequests` 里查到且 `pending` 里仍有该 id 时才续期；这两张表在
`removePending` 中都按 `entry.progressToken` 删除，所以请求结算后到达的进度通知被静默忽略（不会报错）。

### 30.3 客户端事件的迭代语义（Set 迭代期修改）

`TransportEvents` 的 message/error/close 监听器、`McpClient` 的 notification/error/close 监听器都是 `Set`，
`emit*` 用 `for (const l of set) l(...)` 直接迭代。V8 的 Set 迭代器语义（已实测）：
- 迭代期间**删除**尚未访问的元素 → 该元素不会被调用；
- 迭代期间**新增**的元素 → 若在迭代结束前被加入，**会**被访问（头部插入的例外不适用于 Set）；
- 删除并重新添加同一元素 → 视作新元素，会再被访问一次。
`markClosed` 特意对 close listener 做了 `[...this.closeListeners]` 快照，而 `emitMessage`/`emitError`
**没有**快照。Rust 若用 `Mutex<Vec<Listener>>` + 克隆快照，就会与 JS 在这两处不同；建议文档化该差异，
或在 `emit_message`/`emit_error` 中实现"迭代期间删除生效"的等价语义（用快照 + 每次调用前检查是否仍注册）。
`mcpserverconnection`（coding-agent）在 listener 里 `close()` 是现实场景。

### 30.4 `McpClient` 的 `emitError` 不捕获 listener 异常

`McpClient.emitError`（以及 `TransportEvents.emitError`）直接调用 listener，**没有** try/catch，因此
一个抛异常的 error listener 会中断后续 listener，并把异常抛给调用方（例如 `handleMessage` 内）。
与之相对，`handleNotification` 对每个 listener 单独 try/catch，`markClosed` 对 close listener 单独 try/catch。
`handleResponse` 找不到 pending 时用的是 `emitError(new Error(...))`。若调用方的 error listener 抛错，
该异常会沿着"listener → emitError → handleMessage → emitMessage"冒回 transport 的 message 派发点。
stdio 的 `handleStdout` 恰好有一层 `try { emitMessage(parseJsonRpcMessage(JSON.parse(text))) } catch (e)
{ emitError(e) }`，所以第一次抛出会被捕获并**再次**交给 `emitError`；由于 `emitError` 自身没有 try/catch，
若 listener 第二次仍抛，异常将冲出 `handleStdout`，进入 Node 的 `stdout.on("data")` 回调（未捕获异常）。
Streamable HTTP 的 JSON 路径与 `consumeSse` 也是直接 `emitMessage(...)`、没有 try/catch，异常会冒到
fetch/流读取的异步调用栈并变成 unhandled rejection（`consumeResponseStream` 里 `void` 调用）。
Rust 移植建议：保留"error listener 异常向上传播"的语义而不是吞掉；测试不覆盖，但这是可观察差异，
需写进文档。

### 30.5 `ServerCapabilities.tools` 缺失时 `listTools` 不被调用

`McpServerConnection.connectOnce`（coding-agent，不在本单元）只在 `client.serverCapabilities?.tools` 为真时
调用 `listTools()`。本单元要保证 `McpClient.serverCapabilities` 在 `connect()` resolve 之前已赋值，
且 `initialize` 结果中缺少 `tools` 字段时不自动调用 `tools/list`。§13.3 已写"字段赋值顺序"，此处补充
动机：coding-agent 依赖它区分"没有 tools 能力的服务器"（`tools: []`）与"尚未连接"（`undefined`）。

### 30.6 `StdioTransport.send` 的 `signal` 与背压

§15.2 说 `signal` 被忽略，这是**完全**忽略：`send(message)` 的签名甚至没有解构第二个参数
（`async send(message: JsonRpcMessage): Promise<void>`），所以调用方传 `{signal}` 不影响行为，
也不会提前 reject。写入用 `stdin.write(payload, cb)`，`cb(error)` 为真时 reject 原始错误值（不包装）。
`EOF`/EPIPE 后再写会走这个 reject 路径；client 的 `cancelPending(id, error, false)` 会把它转成请求的
rejection（不通知服务端）。

### 30.7 `StreamableHttpTransport` 的 `send` 在 SSE 路径上"成功即返回"

`send()` 对请求消息在 `text/event-stream` + `body` 存在时 `void this.consumeResponseStream(...)` 后
**立即 resolve**。也就是说 `McpTransport.send` 的成功语义是"HTTP 交换已受理"，而不是"请求已答复"；
答复通过 `emitMessage` 异步到达。Rust 中不要在 `send` 里等待答复：`requestInternal` 在发送后立即
`armTimeout` 并注册 abort，若 `send` 等到答复才返回，`transport.send(...).catch(...)` 这条路径就会与
"答复先到、pending 已结算"竞争，超时与取消通知会失去意义。

同一函数里 `checkResponse(response)` 在 `captureSession(response)` **之前**：401/404 的错误响应也会先
分类；但 404 的 `McpSessionExpiredError` 判定用的是**旧的** `sessionIdValue`（此时新响应头还没捕获），
所以"服务器在 404 同时给出新 session id"会被当作 session 过期而不是新会话。

### 30.8 `close()` 后 `McpClient` 永远不会再打开

`connect()` 要求 `state === "idle"`。`close()`（含 `connect` 失败时的自动 close）会把状态永久置为
`"closed"`，后续 `connect` 抛 `Error("Cannot connect MCP client in closed state")`。coding-agent 的
`McpServerConnection` 因此每次重连都新建 `McpClient`。Rust 的 `McpClient` 应是一次的（`&mut self` 的
`connect` 或 `connect(self) -> Connected`），不要提供 `reconnect`。

### 30.9 `getMetrics()` / `getSpans()` 的返回时机

telemetry 的 `getMetrics()`/`getSpans()` 是**同步**方法（TS），返回分离快照；Rust 若把它们做成
`async fn` 会破坏 `TelemetryAdapterFixture.getSpans(): Promise<...>` 的调用形态（fixture 里包了一层
`Promise.resolve`）。建议 Rust 的 `InMemoryTelemetryContext` 提供同步的 `get_metrics()`/`get_spans()`
（内部 `Mutex` 锁短暂持有即可），而 `TelemetryStore` 的 `find_traces`/`get_trace`/`find_metrics` 保持
`async`（与 trait 一致）。conformance fixture 的 `get_spans` 则按 trait 包装为 `async`。

### 30.10 `findTraces` 的默认 limit 与 `Infinity`

`limit = query.limit ?? 20`。在 25 个 trace 的 store 上实测：`undefined` 与 `null`（运行时绕过类型）都得到
20 个 trace + cursor；`Infinity` 得到全部 25 个、**无** cursor（`slice(0, Infinity)` 取全部）；
`0` 抛 `TypeError`；`24` 得到 24 个 + cursor；`25` 得到 25 个且无 cursor（`matches.length > limit` 为假）。
Rust 若用 `Option<f64>`，`null` 与缺省会合并为 20（与 TS 运行时一致）；`Infinity` 需显式处理为"全部"。
`cursor` 的非法 JSON 会抛 `SyntaxError`；`cursor: "5"`、`"{}"`、`"\"x\""` 均不抛，而是因比较恒假返回空页
（已实测，见 §3.8）。

### 30.11 `renderTrace` 的 `Map` 去重与输入不可变

`byId = new Map(spans.map(...))` 对重复 id **后者胜**，但**插入位置保持首次出现的位置**。因此
"同一 id 出现两次、第二次内容不同"时，渲染用的是第二次的内容、第一次的位置。测试 `deduplicates spans
merged from stores by global ID` 依赖这一点。Rust 用 `IndexMap`（或 `HashMap` + 显式顺序）时要注意
`insert` 不改变已有键的位置（`IndexMap::insert` 保留原位置），这与 JS `Map.set` 一致。
`renderTrace` 不修改入参：`[...byId.values()].sort(...)` 先复制再排序；`roots.reverse()` 修改的是局部数组。

### 30.12 `toLlmContent` 的 `content ?? []`

`toLlmContent` 接受 `Pick<CallToolResult,"content"|"structuredContent">`，其中 `content` 在类型上必填，
但运行时用 `result.content ?? []`（所以 `content: null`/`undefined` 都能容忍）。`structuredContent`
的判定是 `!== undefined`，因此 `null` 会生成文本 `"null"`（`JSON.stringify(null, null, 2)` 是 `"null"`）。
`blockToLlmContent` 的 `resource` 分支用 `"text" in resource`（**键存在**即可，即使值是 `undefined`），
所以 `{uri, text: undefined}` 会走文本分支并产生 `{type:"text", text: undefined}`（Rust 的类型化
`LlmContent::Text { text: String }` 无法表达；建议文档化为偏差，或在 Rust 中把缺失的 `text` 当空串）。

### 30.13 `McpHttpError` 的 `body` 与 `describeHttpFailure` 的双重截断

`checkResponse` 先 `await response.text()` 截到 8192 个 UTF-16 代码单元（`${slice} `.slice(0,-1)` 的复制技巧），
把该 8192 串作为 `body` 传给错误类，同时用 `describeHttpFailure(status, body)` 再取前 500 个代码单元
（`slice(0,497) + "..."`）拼进 `message`。因此 `error.body.length <= 8192`、`error.message` 里的摘要
`<= 500`（加上前缀）。测试 `includes the response body in HTTP errors` 断言的是最终消息文本。
注意 401 的 `McpAuthRequiredError` 也用同一个 `body`（不是空串）。

### 30.14 `authorizedFetch` 的 `token` 仅在非空时传给 `onUnauthorized`

`headers(extra)` 返回 `{headers, ...(token ? {token} : {})}`——token 为 `undefined` 或**空串**时，
`UnauthorizedContext.token` 是 `undefined` 而不是 `""`。`adaptOAuthProvider` 的
`if (!insufficientScope && !inFlight && context.token !== undefined)` 因此对"无 token 的 401"
直接进入刷新/授权流程。Rust 用 `Option<String>` 时注意不要传 `Some("")`。

### 30.15 `OAuthCallbackServer.handle` 的 method/body

`handle(rawUrl, response)` 完全忽略 HTTP 方法与请求体：GET/POST/HEAD 都走同一逻辑。`new URL(rawUrl, this.redirectUrl)`
所以相对 URL 也能解析（Node 的 `request.url` 总是以 `/` 开头）。URL 的 hash 不参与 `pathname` 匹配。
同一 state 的第二次回调不会命中（pending 已删除），得到 400 "Invalid or expired OAuth state"。
`waitForCallback(state, path)` 的 `path` 只在**回调到达时**比较，不影响 `paths` 白名单的匹配
（`extraPaths` 必须包含该 path，否则先 404）。

### 30.16 `OAuthCallbackServer` 的 `redirectUrl` 对 IPv6 的处理

`redirectHost.includes(":") ? \`[\${redirectHost}]\` : redirectHost` 意味着传入已带方括号的 `"[::1]"`
会得到 `"[[::1]]"`（§18.6 已注明"未处理的边界"）。另外 `redirectHost` 默认等于 `host`（默认
`"127.0.0.1"`），所以本机默认的 redirect URL 是 `http://127.0.0.1:<port>/callback`，不是 `localhost`。

### 30.17 OAuth provider 的 `writes` 链错误传播

`update(fn)` 是 `this.writes = this.writes.then(async () => { ... })`——若某次 `store.save` 抛错，
`this.writes` 变成 rejected promise，**后续**的 `update` 调用会因 `.then` 在 rejected 链上而**永不执行**
（并且 `load()` 的 `await this.writes` 会重新抛出）。测试不覆盖；Rust 若用 `Mutex` 串行化则没有这个
"一次失败永久毒化"的行为。建议文档化，或按 JS 语义保留（用一个会传播错误的队列）。

### 30.18 `McpOAuthProvider.state()` 的竞态

`state()` 先 `load()` 查 `oauthState`，没有才生成并 `update` 保存。两个并发 `state()` 调用会生成两个
不同的 state，**后者覆盖前者**（`update` 串行化但不合并）。`waitForCallback` 若用第一个 state 注册，
第二个回调会 400。测试不覆盖；coding-agent 是串行调用的。Rust 移植保持简单实现即可，但需在文档中注明。

### 30.19 `StreamableHttpTransport` 的 headers 覆盖顺序与大小写

`buildHeaders(extra, token)`：`new Headers(options.headers)` → 对 `extra` 每项 `headers.set(name, value)`
→ session id → protocol version → Authorization。由于 `Headers.set` 大小写不敏感，调用方在
`options.headers` 里写 `authorization`（小写）也会被 `Authorization` 覆盖；而 `extra` 里的
`accept`/`content-type` 同理。`captureSession` 只在响应头有非空 `mcp-session-id` 时更新；空串不覆盖旧值。

### 30.20 `TransportEvents.emitClose` 的"最多一次"与 `close()` 的幂等组合

`emitClose` 用 `closeEmitted` 布尔保证一次；`InMemoryTransport.close()` 递归调用 `peer.close()`，
两边各自 emit 一次（不互相触发第二次）。`StdioTransport.close()` 在"子进程已退出"分支**不** emitClose，
所以要靠子进程的 `close` 事件触发——若 `close()` 在子进程已退出但 `close` 事件尚未派发时被调用，
`emitClose` 仍会由事件补上（Node 的事件顺序保证 `close` 事件迟早派发）。Rust 中若 `wait` 已经返回，
需要显式补一次 `emit_close`，否则 close listener 永远不触发。

### 30.21 错误消息里 `String(id)` 的 `-0`

`handleResponse` 的未知 id 消息用 `String(m.id)`：`-0` 的 `String` 是 `"0"`（不是 `"-0"`）。
`JsonRpcId` 若用 `f64`，Rust 的 `format!("{}", -0.0)` 得到 `"-0"`——需要走 ECMAScript Number::toString
（§22.3），或在格式化前把 `-0.0` 归一为 `0.0`。

### 30.22 测试用 fixture 的逐字内容（便于 Rust 复刻）

- `test/fixtures/stdio-server.mjs`（逐字要点）：启动即向 stderr 写 `stdio fixture ready`；用 `node:readline`
  逐行读 stdin（`crlfDelay: Infinity`），解析每行 JSON；**没有 `id` 的消息直接 continue**（通知不回应）。
  `initialize` → `{protocolVersion:"2025-06-18", capabilities:{tools:{}}, serverInfo:{name:"stdio-fixture", version:"1.0.0"}}`；
  `tools/list` → `{tools:[{name:"echo", inputSchema:{type:"object"}}]}`（**只有** `inputSchema.type`，没有
  properties）；`tools/call` → `{content:[{type:"text", text: String(message.params.arguments.text)}]}`
  （注意 `String(undefined)` = `"undefined"`，所以缺参数时回显字面量 `undefined`）；`ping` → `{}`；
  其他方法 → `{jsonrpc:"2.0", id, error:{code:-32601, message:"not found"}}`。所有应答都是紧凑
  `JSON.stringify(...) + "\n"`。
- `test/fixtures/stubborn-server.mjs`（逐字要点）：`process.on("SIGTERM", () => {})` 忽略 SIGTERM；
  派生孙进程 `spawn(process.execPath, ["-e", "process.on('SIGTERM', () => {}); setInterval(() => {}, 1000)"], {stdio:"ignore"})`
  ——孙进程**也忽略 SIGTERM**，所以只有进程组 SIGKILL 能杀掉它；向 stderr 打印 `grandchild <pid>`；
  主进程用 `setInterval(() => {}, 1000)` 保活并忽略 stdin EOF；只应答 `initialize`（`capabilities: {}`，
  `serverInfo.name: "stubborn-fixture"`），其他方法不应答。测试从 stderr 解析 PID，close 后用
  `process.kill(pid, 0)` 断言其已消失。
- `test/helpers.ts`：`listen(handler)` 绑定 `127.0.0.1:0` 并返回 origin（`http://127.0.0.1:<port>`）；
  `readBody(request)` 累积请求体后 resolve 字符串；`closeServers()` 关闭本文件登记的所有服务器（`afterEach` 调用）。

### 30.23 运行环境与构建元数据的补充

- 两包的 `tsconfig.build.json` 相同：extends 根 `tsconfig.base.json`，`outDir: ./dist`、`rootDir: ./src`、
  `include: ["src/**/*.ts"]`、`exclude: ["node_modules","dist","**/*.d.ts","src/**/*.d.ts"]`。
- 根 `tsconfig.base.json`：`target ES2024`、`module Node16`、`moduleResolution Node16`、`strict`、
  `erasableSyntaxOnly`、`verbatimModuleSyntax`、`declaration` + `declarationMap` + `sourceMap` + `inlineSources`、
  `allowImportingTsExtensions` + `rewriteRelativeImportExtensions`、`types: ["node"]`。
  这些是 TS 侧约束，Rust 不需要复刻；但 `lib: ["ES2024"]` 解释了为什么源码可用 `Array#at` 这类较新的 API
  （`at(-1)` 在 `findTraces` 中用到；`Object.groupBy` 并未使用）。
- vitest 侧：`packages/mcp/vitest.config.ts` 用 `resolve.conditions: ["source"]`（`ssr` 同样），
  让 `@earendil-works/kiana-*` 解析到 `src`；telemetry 用工作区根的 `vitest.base.ts`，其中把
  `@earendil-works/kiana-telemetry`、`/testing`、`@earendil-works/kiana-mcp`、`/oauth`、`/testing` 别名到
  workspace 源文件路径（`workspaceSourcePaths`）。Rust 测试不需要该机制（crate 直接互相引用）。
- telemetry 的 `package.json` 没有 `@types/cross-spawn`、没有 `cross-spawn`，也**没有** `sideEffects` 字段；
  mcp 的 `dependencies` 只有
  `cross-spawn@7.0.6`，`devDependencies` 为 `@types/cross-spawn@6.0.6`、`shx@0.3.4`、`vitest@4.1.12`。
  两包都 `engines.node >= 22.19.0`、`type: "module"`；`sideEffects: false` 只在 mcp 中声明（见上一条，
  §1.1 未提及 telemetry 侧的缺省，注意别照抄 mcp）。
- 许可证：mcp 的 `LICENSES/modelcontextprotocol-typescript-sdk.txt` 是 MIT 全文（Copyright (c) 2024
  Anthropic, PBC）；`src/oauth/{flow,discovery}.ts` 与 `src/oauth/types.ts` 顶部都有改编声明注释。telemetry
  没有 LICENSES 目录，只有根 LICENSE（MIT，Mario Zechner）。

### 30.24 README 中未覆盖的用户可见承诺

- telemetry README 的 "Security and Portability"：遥测是诊断而非持久状态，**不得**持久化
  `TelemetryContext`/`TelemetrySpan`/backend-native trace 对象；`TelemetryCarrier` 可以跨传输或作为可选
  的相关性诊断数据，但**不是**应用状态；属性值刻意限制为原始标量与数组；不用 `AsyncLocalStorage` 或任何
  运行时特定 ambient context，适配 Node.js/Bun/浏览器/worker。Rust 移植的 crate 文档应保留这些承诺，
  并且**不要**在 `kiana-telemetry` 中引入全局 current-span 状态（这与 §3.1 的模块级 carrier 注册表不同：
  注册表只做身份查询，不提供 ambient 传播）。
- telemetry README "Kiana Package Integration" 明确 `kiana-ai` 从**根与 `./models` 两个入口**导出
  `AI_TELEMETRY_SCHEMA`；`kiana-durable` 导出 `DURABLE_TELEMETRY_SCHEMA`；schema 名字用 `kiana.ai.*` /
  `kiana.durable.*`，适配器可翻译到后端约定但不得改变发出的词汇表。这与 §19.1 的消费者列表一致，但
  "根与 ./models 两个入口"这一细节此前未写。
- mcp README 的 OAuth 示例展示了正确的使用顺序：`OAuthCallbackServer.listen()` → 构造 `McpOAuthProvider`
  （`clientMetadata: {client_name}`）→ `adaptOAuthProvider(oauth)` 传给 `StreamableHttpTransport` →
  `connect()` 抛 `McpOAuthAuthorizationRequiredError` → `oauth.state()` + `callback.waitForCallback(state)`
  → `authorizeMcp(oauth, {serverUrl, authorizationCode})` → 重新 connect。README 明确
  "The package does not open a browser or choose where credentials are stored"。这条"由嵌入方打开浏览器"的
  边界是移植时不要越界实现的部分（Rust 同样不应内置浏览器启动）。
- mcp README 的 "Testing" 只承诺 `/testing` 导出 `createInMemoryTransportPair()`；不要把
  `InMemoryTransport.emitError` 之类的测试钩子当作公开契约文档化（它是 `override` 的 public 方法，
  实际可从 `/testing` 子路径访问）。

### 30.25 未被任何测试覆盖、但 Rust 应保持一致的行为（风险清单）

1. `findTraces` 的负数/`0`/`NaN` limit（§3.8、§28.2）：TS 会抛 `TypeError`，无测试；Rust 需选一种明确语义。
2. `renderTrace` 对 `startedMs` 为 `NaN`/`Infinity` 的排序：无测试。
3. `Headers` 名/值非法字符：无测试；只在调用方传坏头时触发。
4. `setTimeout` 延迟 > 2^31-1 的钳位（§13.7）：无测试。
5. `provider.update` 一次失败后链被毒化（§30.17）：无测试。
6. `OAuthCallbackServer` 的 IPv6 双括号（§30.16）：无测试。
7. `adaptOAuthProvider` 对 `token === ""` 的处理（§30.14）：无测试。
8. `listAll` 超过 1000 页（`MAX_LIST_PAGES`）→ 消息 `MCP <method> exceeded 1000 pages`
   （例如 `MCP tools/list exceeded 1000 pages`）：无测试；Rust 需保留该消息格式。
9. 重复 cursor → `Error("MCP ${method} returned duplicate cursor: ${nextCursor}")`：测试用
   `nextCursor: ""` 结束分页，**不**覆盖重复 cursor；Rust 保留消息。
10. `notify()` 在 `connecting` 状态：`notifyInternal(..., false)` 要求 `connected`，所以会抛
    `McpConnectionClosedError("MCP client is connecting")`；无测试。
11. `request()` 在 `connecting` 状态：同样抛（`allowConnecting=false`）；只有 `connect()` 内部的
    initialize/initialized 用 `true`。
12. `McpClient.connect` 的 `transport.start()` 失败：`close()` 被调用，但 `disposeTransportListeners`
    只清理已注册的 disposer，transport 自身可能仍持有资源（取决于 transport 实现）；无测试。

### 30.26 数字/字符串桥接的补充向量（已实测）

```
Number("") = 0; Number(" 5 ") = 5; Number("0x10") = 16; Number("1e3") = 1000
Number("Infinity") = Infinity; Number("-1") = -1; Number("1.5") = 1.5; Number("abc") = NaN
Number("<400 个 9 组成的数字串>") = Infinity（已实测）
JSON.parse("1e400") = Infinity（JS；serde_json 默认拒绝/溢出，需桥接或文档化）
String(-0) = "0"（Node 实测；Rust 的 `format!("{}", -0.0f64)` 得到 "-0"，必须走 §22.3 的 ECMAScript 格式化）
String(NaN) = "NaN"; String(Infinity) = "Infinity"
String(Symbol("x")) = "Symbol(x)"（仅在 abort reason 路径可达）
AbortController.abort() / abort(undefined) → reason 是 DOMException，"AbortError: This operation was aborted"
AbortController.abort(null) → reason null → `String(null ?? "Aborted")` = "Aborted"
AbortController.abort(false) → "false"; abort("") → ""; abort(NaN) → "NaN"; abort({}) → "[object Object]"
```

`JSON.parse("1e400")` 返回 `Infinity` 是 JS 特有行为（JSON 文本本身合法但值溢出）；serde_json 会报
`number out of range`（除非启用 `arbitrary_precision`）。MCP 载荷里出现这种数字的概率极低，但移植时若
遇到 `JSON.parse` 失败路径（SSE 的 `parseJsonRpcMessage(JSON.parse(...))`），错误文本会不同。已记录为偏差。

### 30.27 `requireTransport` 的错误消息与状态

`McpConnectionClosedError(\`MCP client is ${state}\`)`，state ∈ `idle | connecting | connected | closed`。
因此可能的消息是 `MCP client is idle`、`MCP client is connecting`、`MCP client is closed`；`connected`
分支要求"有 transport 但状态为 connected"永远不会失败，所以 `MCP client is connected` 不可达。
测试**没有**逐字断言这些消息（只用 `toBeInstanceOf(McpConnectionClosedError)`）；coding-agent 依赖
`McpConnectionClosedError` 的类型而非文本。Rust 用 `Display` 实现同样的消息，但可视为低风险文本。

### 30.28 SSE 解析器的 `dataBytes` 与 `id`/`retry` 的交互

`dataBytes` 只累计 `data` 字段（含行间 `\n`），**不**包含 `id`/`event`/`retry` 行；因此一个事件可以
有无限多 `id:` 行而不触发上限。`id` 行即使没有 `data` 也会立即调用 `onId`，但 `dispatch()` 在
`dataLines.length === 0` 时会清除 `eventName`/`eventId`（所以"只有 id 的事件"不会把 id 泄漏到下一个
事件，尽管 `onId` 已经被调用过）。`retry` 的 `/^\d+$/` **不接受** `+5`、`5.0`、` 5`、`-5`（已实测），
但**接受** `007`（`Number("007")` = 7）。`Number(value)` 对超长数字串得到 `Infinity`（`onRetry(Infinity)`
→ `reconnectDelay` 用它作为延迟 → `setTimeout(Infinity)` 钳到 1 ms）。Rust 需复刻 `retry` 的正则而不是
`String::parse::<u64>()`：后者**接受**前导 `+`（`"+5".parse::<u64>()` 是 `Ok(5)`），会与 JS 不一致。
**这是移植时最容易漏的一处**。

### 30.29 `consumeSseStream` 的 `finally { reader.releaseLock() }`

无论抛出（包括 `onEvent` 抛出的异常）都会 `releaseLock`，但**不会** cancel 底层流。因此响应体的
`ReadableStream` 在消费失败后仍处于可读状态，由调用方（`consumeResponseStream`/`runGetStream`）
通过 `discard` 或重新打开处理。Rust 中若用 `reqwest` 的 `bytes_stream`，drop reader 即取消；
语义差异会导致重连时服务器看到连接被提前断开。建议文档化：Rust 消费失败时 drop stream 即取消。

### 30.30 `McpClient` 的 `incoming` 表与重复 id

`handleRequest` 用 `this.incoming.set(message.id, controller)`；同一 id 的第二个服务端请求会**覆盖**
第一个 controller（第一个请求的 signal 不再可被 cancel 到达）。`finally` 里的 `incoming.delete(id)`
会删除**当前**条目——若两个同 id 请求交错完成，先完成的那个会删除后一个的 controller。无测试覆盖；
Rust 若要精确复刻需用 `HashMap<JsonRpcId, AbortController>` 并接受同样的行为。这也意味着
`markClosed` 的 `for controller of incoming.values()` 只 abort 最新的 controller。

### 30.31 交叉核对：spec 中被审计更正的断言（汇总）

以下断言在正文中已就地修正，列在这里便于评审 diff：
1. telemetry `./testing` 子路径**没有** `source` 条件（§1.1）。
2. telemetry 的 `files` **不含** CHANGELOG/LICENSES；没有 `vitest.config.ts`；`clean` 用 `shx`
   但 `shx` 不在其 devDependencies（§1.1）。
3. `contextCarriers` 是**模块级** WeakMap，跨实例可见；Rust 必须用进程级静态注册表（§3.1、§24）。
4. `createMeasurementRecorder` 是 3 参数函数，运行时 `counter.add(v, attrs, span)` 会记录 exemplar；
   counter/upDownCounter 永不存 `boundaries`（§3.7）。
5. `findTraces` 的 `limit` 边界不是"`<= 0` 返回空页"：`slice` 的 ToIntegerOrInfinity 语义（§3.8、§28.2）。
6. `Headers` **不**折叠内部空白，只裁剪首尾（§17.4、§21、§25 第 11 条）。
7. `refreshAuthorization` 是 `{refresh_token: options.refreshToken, ...tokens}`，服务器返回的
   `refresh_token` **覆盖**传入值（§18.4）。
8. telemetry 的直接消费者只有 `kiana-ai`、`kiana-durable`、`kiana-chord`；`kiana-agent-core`/`kiana-coding-agent`
   是传递依赖；`kiana-evals` 不用（§19.1）。
9. 源码中**没有** `AbortSignal.any`；只有 `AbortSignal.timeout(1000)`（§19.3、§21）。
10. `renderTrace` 排序比较器是 `left.startedMs - right.startedMs`（原 spec 有 `startedMs - startedMs`
    的笔误）（§6）。
11. `requestInternal` 的默认超时读 `this.options.requestTimeoutMs`，不是 `options.requestTimeoutMs`（§13.5）。
12. `parseWwwAuthenticate` 与 `needsAuthorization` 的正则边界（无尾边界、`=` 旁不能有空格）（§18.3、§27）。
13. `abort(null)` 的 reason 字符串是 `"Aborted"`，不是 `"null"`（§13.5）。
14. `packages/agent/examples/mcp-codemode` 是示例路径，仓库根没有 `examples/`（§1.2）。
15. `parseJsonRpcMessage` 的谓词顺序是 request→notification→response，与 `handleMessage` 的
    response 优先不同，但 `parseJsonRpcMessage` 只做"任一成立"判断，顺序不影响其结果；**归类**顺序
    只在 `handleMessage` 有意义，且 request/response 谓词可以同时成立（§9、§13.7）。
16. `optionalStrings` 的 `""` 会抛错（与 `optionalString` 的 `absent` 不同）（§18.1）。
