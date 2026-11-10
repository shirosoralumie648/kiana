# `chord-rest` 移植规范（TypeScript → Rust）

本文件是 `@earendil-works/chord` 中 **chord-rest** 单元的完整移植规范，同时充当：

1. **验收清单**：Rust 实现必须逐条满足的行为契约；
2. **Rust 设计**：模块划分、类型、错误、并发模型与最难复刻之处的说明。

- 单元 id：`chord-rest`
- 参考实现（只读，**不得修改**）：`/home/ctyun/code/kiana_rev/kiana/packages/chord/`
- 目标工作区：`/home/ctyun/code/kiana_rev/kiana`
- 上游依赖单元：`@earendil-works/kiana-telemetry`（其类型被本单元使用，见 §5）

> 约定：文中所有标识符、字段名、错误文案、用户可见字符串均按 TypeScript 源码**逐字**抄录。Rust 实现必须输出完全一致的文本与字节。中文用于解释语义。凡标 **【逐字】** 的字符串/JSON/SQL，测试会逐字节比对。
>
> 本单元**没有 CLI、没有 TUI、没有 slash 命令、没有按键绑定**：它是一个库。所谓"用户可见面"是公开 API 的错误码/错误消息、线上帧格式、落盘 SQLite schema、控制消息与环境变量（§3、§4）。

---

## 0. 单元边界与阅读顺序

### 0.1 属于本单元的源文件（全部必须移植）

```
packages/chord/package.json
packages/chord/src/index.ts                  (91 行)   根导出面
packages/chord/src/node.ts                   (16 行)   Node 子路径导出
packages/chord/src/errors.ts                 (76 行)
packages/chord/src/json.ts                   (135 行)
packages/chord/src/schema.ts                 (219 行)
packages/chord/src/types.ts                  (89 行)
packages/chord/src/declare.ts                (38 行)
packages/chord/src/registry.ts               (30 行)
packages/chord/src/publish.ts                (39 行)
packages/chord/src/cell.ts                   (402 行)
packages/chord/src/state.ts                  (423 行)
packages/chord/src/watch.ts                  (343 行)
packages/chord/src/contract.ts               (440 行)
packages/chord/src/wire.ts                   (181 行)
packages/chord/src/link.ts                   (415 行)
packages/chord/src/connection.ts             (1327 行)
packages/chord/src/host.ts                   (1303 行)
packages/chord/src/object.ts                 (1932 行)
packages/chord/src/remote.ts                 (658 行)
packages/chord/src/reload.ts                 (723 行)
packages/chord/src/cluster.ts                (251 行)
packages/chord/src/manager.ts                (443 行)
packages/chord/src/loader.ts                 (67 行)
packages/chord/src/websocket.ts              (35 行)
packages/chord/src/output.ts                 (406 行)
packages/chord/src/context/index.ts          (161 行)

packages/chord/src/delta/index.ts            (742 行)
packages/chord/src/delta/diff.ts             (525 行)
packages/chord/src/delta/pending.ts          (173 行)
packages/chord/src/delta/tracker.ts          (2292 行)
packages/chord/src/delta/draft.ts            (10 行)
packages/chord/src/delta/apply-immutable-trusted.ts (125 行)
packages/chord/src/delta/revision-validator.ts      (75 行)

packages/chord/src/sqlite/index.ts           (3 行)
packages/chord/src/sqlite/schema.ts          (9 行)
packages/chord/src/sqlite/database.ts        (38 行)
packages/chord/src/sqlite/migrations.ts      (43 行)
packages/chord/src/sqlite/node.ts            (201 行)
packages/chord/src/sqlite/cloudflare.ts      (180 行)

packages/chord/src/telemetry-sqlite/index.ts (363 行)
packages/chord/src/telemetry-sqlite/store.ts (171 行)
```

文档（**已读**，作为语义依据，不移植）：`docs/spec.md`（288 行）、`docs/guide.md`（814 行）、`docs/tutorial.md`（931 行）、`src/delta/README.md`（260 行）、`README.md`（235 行）、`CHANGELOG.md`（116 行，含 1.4.0 全部 breaking changes，是行为的权威说明）。

`docs/spec.md` 的 §12「Known limitations and decisions」列出 **12 条**仍然开放的已知限制（Retirement across hosts、The commit decision is not persisted、One level of reload participants、Reconciliation needs the child's module list、Links do not resume、Refused clients cannot tell、The publication queue is process-wide and synchronous、No automatic restart、The Node loader leaks、Stale socket locks、Listener `select` identity、Shutdown and dead links），以及 **18 条**「Decisions」（Errors、Reload errors、New objects during prepare、Gate admission、Tasks and listeners during the gate、Grandfathered roots、Removed types and missed retirement、Joining during a reload、Two-version rule、Validation、Contracts、Principals、Cells and readables、References and calls、Resources and disposal、Links and the host manager、Flow control holds back state only、The state API stays）。这些都是**行为契约的一部分**：§2.9/§2.12/§2.16/§2.20/§2.21/§2.22/§2.23 已逐条落到实现细节，Rust 侧不得「顺手修好」（会改变可观察行为）。

相关测试（本单元的所有测试，全部在 `packages/chord/test/`，见 §7 清单）：`*.test.ts`、`sqlite/*.test.ts`、`delta-tracker/*.test.ts`、`fixtures/*.ts`、`delta-benchmark/*`、`delta-traversal.bench.ts`。

### 0.2 建议阅读顺序

1. `errors.ts`、`json.ts`、`schema.ts`、`types.ts`、`declare.ts`、`registry.ts` —— 基础值语义。
2. `delta/*` —— 所有状态变更的底层操作语言（`Op`/`WireOp`/tracker/diff/pending）。
3. `context/index.ts`、`publish.ts`、`cell.ts`、`state.ts`、`watch.ts` —— 可观察状态。
4. `contract.ts`、`wire.ts`、`link.ts` —— 契约与线上帧。
5. `connection.ts`、`remote.ts`、`object.ts`、`host.ts` —— 运行时核心。
6. `reload.ts`、`cluster.ts`、`manager.ts`、`loader.ts`、`websocket.ts` —— 热重载与进程管理。
7. `output.ts`、`sqlite/*`、`telemetry-sqlite/*` —— 独立子系统。

### 0.3 npm 子路径（Rust crate 的 feature/模块划分依据）

`package.json` 的 `exports` 决定公共 API 分段：

| 子路径 | 源文件 | 说明 |
| --- | --- | --- |
| `.` | `src/index.ts` | 根导出面 |
| `./context` | `src/context/index.ts` | Context/ContextKey + 组合子 |
| `./output` | `src/output.ts` | OutputBuffer / Progress |
| `./delta` | `src/delta/index.ts` | 操作语言 + tracker/diff/pending |
| `./node` | `src/node.ts` | nodeLoader + host manager |
| `./telemetry-sqlite` | `src/telemetry-sqlite/index.ts` | SQLite telemetry |
| `./sqlite` | `src/sqlite/index.ts` | 可移植 SQLite 接口 |
| `./sqlite/node` | `src/sqlite/node.ts` | Node 驱动 |
| `./sqlite/cloudflare` | `src/sqlite/cloudflare.ts` | Durable Object 驱动 |
| `./package.json` | — | 元数据 |

依赖：`@earendil-works/kiana-telemetry@1.4.0`、`typebox@1.3.27`。devDeps：`@types/ws`、`shx`、`vitest@4.1.12`、`ws@8.21.0`。`engines.node >= 22.19.0`。`"type": "module"`、`"sideEffects": false`。

---

## 1. 概览

### 1.1 包的用途

Chord 是"带类型的宿主运行时（typed host runtime）"：

- **contract（契约）**：一个运行期值，含名字、版本、输入 schema、成员描述（state / method / opaque）与远程放置（placement）。消费者的契约令牌（`Description`）随线上帧传输，提供者校验"覆盖（coverage）"或经 `accepts[version]` 适配器绑定。
- **object（对象）**：`(type, normalized input, principal)` 唯一确定一个对象；对象拥有 cells（可观察 JSON）、resources（open/close 的原生值）、对其它对象的引用、listeners、tasks、derivations、registrations。
- **reference（引用）**：调用者拿到的是 `Ref`（本地 `RefImpl` 或远程 `RemoteRef` 的 Proxy），不是对象本身。引用可跨进程、跨热重载存活。
- **root（根）**：一次短调用树，携带 `rootId`、`codeVersion`、`principal` 与可取消信号。跨链路的 root 以 `root_end` 结束其租约。
- **link（链路）**：FIFO 有序帧通道；memory / Node IPC / WebSocket 三种实现；协议版本 `PROTOCOL_VERSION = 4`；大于 64 KiB 的负载用 deflate-raw 压缩（packed）。
- **hot reload**：两版本规则；prepare（录制模式）/ gate / commit / retire；跨主机由 server 协调（prepare/vote/commit/abort/retire）。
- **replicated state / delta**：不可变 JSON 修订 + `StateEvent`（snap/ops/reset）；`Op` 元组语言（`r/s/d/a/t/p/m`），带路径内联与 wire 压缩（`#` id、省略相邻路径）。

### 1.2 与其他包/单元的关系

- 下游使用方：`packages/durable`（大量 `Context`、`Op`、`tracker`、`applyImmutable`、`PendingOps`、`SqliteDatabase`）、`packages/env`。它们通过子路径 `@earendil-works/chord`、`/context`、`/delta`、`/output`、`/sqlite`、`/sqlite/node`、`/sqlite/cloudflare` 使用本单元。统计（在 `durable/src`、`env/src` 中）：
  - `.`：`Context`(59)、`Draft`(13)、`JsonRepresentation`(6)、`JsonValue`(39)、`Watch`(3)、`WatchHandle`(4)、`WatchEnd`(1)、`NativeWatch`(1)、`copyJson`(11)
  - `/context`：`BACKGROUND_CONTEXT`(66)、`awaitWithContext`(3)、`createContextKey`(1)、`withAbortSignal`(10)、`withCancel`(2)、`withContextValue`(1)、`withTelemetry`(3)、`withoutAbortSignal`(5)
  - `/delta`：`Op`(17)、`applyImmutable`(7)、`track`(3)、`appendText`(2)、`ownedSlice`(3)、`PendingOps`(2)、`apply`(2)、`applyImmutableBatches`(1)、`overlap`(1)、`Change`/`Prepared`/`Tracker`/`Path`/`NonEmptyPath`/`Draft` 各 1–2
  - `/output`：`OutputBuffer`、`OutputLimits`、`Progress`、`ProgressClock`、`PROGRESS_BYTES_PER_SECOND`、`boundOutput`、`characterEnd`、`utf8ByteLength`
  - `/sqlite`：`SqliteDatabase`、`SqliteExecutor`、`SqliteMigration`、`SqliteValue`、`applySqliteMigrations`、`validateSqliteSchemaPrefix`；`/sqlite/node`：`NodeSqliteDatabase`、`NodeSqliteDatabaseOptions`、`openNodeSqliteDatabase`；`/sqlite/cloudflare`：`DurableObjectSqliteDatabase`、`DurableObjectSqliteStorage`
- 上游：`@earendil-works/kiana-telemetry`。本单元只消费其类型（`TelemetrySpan`、`TelemetryContext`、`SpanOptions`、`SpanStatus`、`SpanLink`、`SpanAttributes`、`InstrumentOptions`、`TelemetryMetrics`、`TelemetryStore`、`TraceQuery`、`MetricQuery`、`StoredSpan`、`StoredTrace`、`StoredMetric`、`NOOP_TELEMETRY_CONTEXT`）与 `crypto.randomUUID()`、`performance.now()`。Rust 端口需自建等价 trait（见 §5）。

### 1.3 入口点

- 根模块：`createHost`（唯一创建入口）、`defineFacet`、契约构造器（`remote`/`local`/`method`/`opaque`/`hostNamed`/`hostFamily`）、errors、`copyJson`/`isJsonValue`、`defineRegistry`、`replicatedState`、`memoryLinkPair`/`ipcLink`/`webSocketLink`/`acceptWebSockets`/`inflateMessage`/`PACK_THRESHOLD`、`Watch`/`NativeWatch`、`PROTOCOL_VERSION`。
- `Host` 对象 API：`name`、`codeVersion`、`previousCodeVersion`、`serve`、`install`、`load`、`ready`、`get`、`hold`、`root`、`registry`、`invalidatePrincipal`、`accept`、`connect`、`route`、`reload`、`close`。
- 工厂 API（`self`）：`state`、`get`、`resource`、`on`、`task`、`derive`、`register`、`expose`。
- 远程引用 API：`status`、`ready`、`variant`、`shape`、`error`、`onShape`、`onStatus`、`release`、`contract`，以及成员（state → `Readable`，method → 调用函数）。

### 1.4 运行面特征（决定 Rust 设计）

| 特征 | TS 实现 | Rust 设计影响 |
| --- | --- | --- |
| 工厂同步、激活异步 | `factory` 必须同步返回对象；返回 Promise 抛 `invalid/implementation` | 工厂签名为 `Fn(Value, Principal, SelfApi) -> Result<Implementation>`，不得 async |
| 大量 JS 动态成员/Proxy | `RefImpl`/`RemoteRef` 用 `Proxy` 绑定迟到成员 | Rust 用显式 `Ref` 枚举 + `member(name)` 访问器 + 每方法生成调用器；类型安全由 trait 层补 |
| draft 依赖 ES Proxy（set/get/delete/ownKeys/descriptor） | `delta/tracker.ts` 2292 行 | Rust 必须显式 API（见 §8.3），不能 1:1 复制代理语义；行为约定用 `Draft` trait/`DraftValue` |
| 全局 FIFO 通知队列 | `publish.ts` 模块级 `queue` | Rust 用进程级（或 Host 级）`Mutex<VecDeque>`+重入保护；注意 TS 中是 **模块级全局** |
| `WeakRef`/`WeakMap`/`WeakSet` | tracker 上下文注册、引用所有权、schemata | Rust 用 `Arc`/`Weak`、generation id、显式释放；见 §8.4 |
| 微任务/定时器 | `queueMicrotask`、`setTimeout(...).unref()` | tokio `spawn`/`sleep`；"绝不内联回调" 用 `tokio::spawn` 或显式调度器 |
| AbortSignal | `object.ts` 的 `linkedSignal` **手写**（刻意不用 `AbortSignal.any`：避免 Node 依赖信号泄漏）；`context/index.ts` 的 `withAbortSignal` **则用** `AbortSignal.any([parent, signal])` | Rust 用 `tokio_util::sync::CancellationToken` + 子 signal 树（带 detach）；两处语义不同：`linkedSignal` 可在 release 时断开，`AbortSignal.any` 不可 |
| UTF-16 字符串索引 | `str.length`/`slice`/`charCodeAt` 出现在 hash64、`t` 操作计数、字符串 append/truncate | **关键差异**：JS `t`/`a` 的计数是 UTF-16 code unit，不是 UTF-8 字节；见 §6.2 |
| JSON 键序 | `Object.keys` 插入序 + 整数键优先 | Rust `serde_json::Map` 默认 BTreeMap（排序）需改成 `IndexMap`/`preserve_order`，否则 delta 输出顺序、canonical identity、registry 顺序全错 |
| 数字格式 | `String(number)`、`JSON.stringify(number)` | Rust 需匹配 ECMAScript `Number::toString`（最短往返、`1e+21` 等格式）；见 §6.3 |

---

## 2. 逐文件规范

### 2.1 `src/index.ts`（91 行）— 公共导出面

纯 re-export，无逻辑。导出清单（逐项，Rust 必须全部提供等价物）：

- `type Cell`（`cell.ts`）
- `type AcceptOptions, LinkOptions`（`connection.ts`）
- `type Context, ContextKey`（`context/index.ts`）
- `contract.ts`：`Accepts`、`Adapter`、`AnyContract`、`bind`、`coverage`、`Description`、`HostFamily`、`HostNamed`、`hostFamily`、`hostNamed`、`ImplementationOf`、`InputOf`、`LocalContract`、`local`、`method`、`opaque`、`Placement`、`PlacementFor`、`Ref`、`RemoteContract`、`remote`
- `type EventOf, Resource, ResourceDeclaration, Subscribable, ValueOf`（`declare.ts`）
- `type Draft`（`delta/index.ts`）
- `ChordError`、`type ChordErrorCode`、`forbidden`、`invalid`、`isChordError`（`errors.ts`）
- `type ConnectOptions, PrincipalMatch, PrincipalPattern, Route`（`host.ts`）
- `createHost`、`defineFacet`、`type Facet, FacetApi, FacetDefinition, FacetLoader, Host, HostOptions, LoadOptions, PluginRef, RootOptions`（`host.ts`）
- `type CopyJsonOptions, copyJson, isJsonValue, type JsonRepresentation, JsonValue`（`json.ts`）
- `type IpcChannel, inflateMessage, ipcLink, type Link, memoryLinkPair, PACK_THRESHOLD, type WebSocketLike, WebSocketLinkOptions, webSocketLink`（`link.ts`）
- `type Factory, HostServeOptions, Lifetime, ReferencedServeOptions, Self, ServeOptions, ServeOptionsFor, StateInit, StateOptions`（`object.ts`）
- `defineRegistry`、`type Registry, RegistryTable, RegistryView`（`registry.ts`）
- `type ReloadOptions, ReloadReport`（`reload.ts`）
- `type AttachedReplicatedState, MutableReplicatedState, ReplicatedState, ReplicatedStateDelivery, ReplicatedStateSource, ReplicatedStateSourceAttachment, ReplicatedStateSourceFrame, ReplicatedStateSourceOptions, replicatedState`（`state.ts`）
- `type Ctx, GetOptions, Readable, RefStatus, Shape, StateEvent`（`types.ts`）
- `NativeWatch`、`type NativeWatchable, NativeWatchHandle, Watch, Watchable, WatchEnd, WatchHandle`（`watch.ts`）
- `acceptWebSockets`、`type WebSocketAcceptOptions, WebSocketServerLike`（`websocket.ts`）
- `type Frame, PROTOCOL_VERSION`（`wire.ts`）

**注意**：`index.ts` 导出 `type Link`（不是 `Link` 值）、`PACK_THRESHOLD`、`PROTOCOL_VERSION`、`ChordError`/`forbidden`/`invalid`/`isChordError`、`copyJson`/`isJsonValue`、`createHost`/`defineFacet`、`defineRegistry`、`replicatedState`、`NativeWatch`/`Watch`、`acceptWebSockets`，以及 `contract` 的 **8 个值**（`bind`、`coverage`、`hostFamily`、`hostNamed`、`local`、`method`、`opaque`、`remote`）与 13 个类型（`Accepts`、`Adapter`、`AnyContract`、`Description`、`HostFamily`、`HostNamed`、`ImplementationOf`、`InputOf`、`LocalContract`、`Placement`、`PlacementFor`、`Ref`、`RemoteContract`）。`index.ts` **不**导出 `Context` 的值（只有类型）、不导出 `OutputBuffer`/`Progress`/`delta` 的任何值（它们在子路径）。

### 2.2 `src/node.ts`（16 行）

```
export { nodeLoader } from "./loader.ts";
export { type ControlMessage, childControl, createHostManager, type Driver, type Exit, type ForkDriverOptions,
         forkDriver, type HostManager, type HostManagerOptions, type Instance, type Liveness, processLock } from "./manager.ts";
```

### 2.3 `src/errors.ts`（76 行）

```ts
export const CHORD_ERROR_CODES = [
  "aborted","contract_mismatch","forbidden","gone","invalid","not_in_variant",
  "not_ready","released","superseded","unavailable","upgrade_conflict","upgrade_required",
] as const;
export type ChordErrorCode = (typeof CHORD_ERROR_CODES)[number];
export type ChordErrorReason = string;   // 细分原因，如 invalid/args、invalid/state、invalid/duplicate、
                                        // unavailable/unknown_type、unavailable/ready_timeout
```

`class ChordError extends Error`：
- 字段：`readonly code: ChordErrorCode`、`readonly reason: ChordErrorReason | undefined`、`readonly retryable: boolean`、`readonly details: unknown`。
- 构造函数 `(code, message, options?: { cause?, details?, reason? })`。
- **消息格式【逐字】**：`` `${code}${options?.reason ? `/${options.reason}` : ""}: ${message}` ``。
- `this.name = "ChordError"`。
- `RETRYABLE = new Set(["superseded","upgrade_conflict"])`；`retryable = RETRYABLE.has(code)`。
- `cause` 传给 `Error` 的 `{ cause }`。

- `isChordError(error, code?) => error instanceof ChordError && (code === undefined || error.code === code)`
- `forbidden(message = "forbidden") => new ChordError("forbidden", message)` **【逐字默认值 "forbidden"】**
- `invalid(reason, message, details?) => new ChordError("invalid", message, { reason, details })`
- `asChordError(error, context)`：是 `ChordError` 则原样返回；否则 `` new ChordError("unavailable", `${context}: ${error instanceof Error ? error.message : String(error)}`, { cause: error }) ``。
- `async runAll(steps, message)`：**串行**执行全部步骤，收集错误；0 个错误正常返回；1 个抛该错误；>1 抛 `new AggregateError(errors, message)`。

Rust：`ChordError { code: ChordErrorCode, reason: Option<String>, message: String, cause: Option<...>, details: Option<serde_json::Value> }`，`Display` 按上式；`retryable()`；`run_all` 返回聚合错误（`Vec<Box<dyn Error>>`）。

### 2.4 `src/json.ts`（135 行）

类型：
```ts
export type JsonValue = null | boolean | number | string | JsonValue[] | { [key: string]: JsonValue };
export type JsonRepresentation<T> = ...;        // 类型级，Rust 无需移植
export type CopyJsonOptions = { readonly omitUndefinedProperties?: boolean };
```

`copyJson(value, options?) : JsonValue` —— 深拷贝为"严格 JSON、无别名、调用者拥有"的树。规则（**顺序敏感**）：
1. `null` / `string` / `boolean` 直接返回。
2. `number`：非有限（NaN/±Infinity）抛 `TypeError("Value contains a non-finite number and is not strict JSON")`。
3. 其它非 object（undefined、function、symbol、bigint）抛 `` TypeError(`Value contains a non-JSON ${typeof value}; expected strict JSON`) ``。
4. 用祖先集合检测环：命中抛 `TypeError("Value contains cycles and is not strict JSON")`。
5. 数组：原型必须是 `Array.prototype`，且 `Reflect.ownKeys(value).length === value.length + 1`（索引 + `length`，无空洞、无额外键），否则 `TypeError("Value must contain strict JSON dense plain arrays")`；每个索引必须有可枚举 data descriptor，否则 `TypeError("Value must contain strict JSON enumerable indexed data properties")`；结果用 `defineData`（`writable/enumerable/configurable` 全 true）写入。
6. 对象：原型必须是 `Object.prototype` 或 `null`（否则 `TypeError("Value must contain strict JSON plain objects or arrays")`），结果为 `Object.create(prototype)`（**保留 null 原型**）；符号键抛 `TypeError("Value contains a symbol key and is not strict JSON")`；非可枚举或访问器抛 `TypeError("Value must contain strict JSON enumerable data properties")`；`omitUndefinedProperties` 时跳过值为 `undefined` 的属性（数组元素不会被跳过 —— 数组里 `undefined` 会在第 5 步的 descriptor 检查通过后递归到第 3 步抛错？注意：数组元素 `undefined` 走到 `copy` 的第 3 步，`typeof undefined !== "object"` 抛 `Value contains a non-JSON undefined`；实现必须维持这一行为）。

`isJsonValue(value)`：不拷贝的检查，规则同上（有限数字、普通对象/数组、无符号键、无可枚举检查失败、无环）；数组还必须所有 own key 都是 string。

Rust：`JsonValue` 用 `serde_json::Value` + `preserve_order`；`copy_json` 返回新值并保留"null 原型"信息（可忽略原型但必须保留 `__proto__` 作为普通键；见 §6.4）。

### 2.5 `src/schema.ts`（219 行）

`canonicalJson(value: unknown): string` —— 规范 JSON：
- `null` 或非 object：`JSON.stringify(value) ?? "null"`（`undefined` → `"null"`，函数/symbol → `"null"`）。
- 数组：`[` + `value.map(canonicalJson).join(",")` + `]`（`undefined` 元素成为 `null`）。
- 对象：`Object.keys(value)` **过滤值为 `undefined` 的键**，`.sort()`（默认 UTF-16 码元序），输出 `` {"k":v,...} ``，键用 `JSON.stringify(key)`。

`hash64(text: string): string` —— 非加密 64 位散列，两条 FNV-1a 车道：
```
let a = 0x811c9dc5;
let b = 0x01000193 ^ 0x5bd1e995;
for (let i = 0; i < text.length; i++) {          // UTF-16 code unit 级
  const code = text.charCodeAt(i);
  a = Math.imul(a ^ code, 0x01000193);           // 32 位有符号乘法，取低 32 位
  b = Math.imul(b ^ code, 0x5bd1e995) ^ (b >>> 13);
}
return (a >>> 0).toString(16).padStart(8,"0") + (b >>> 0).toString(16).padStart(8,"0");  // 16 个十六进制小写字符
```
Rust：`u32` wrapping_mul 与 `charCodeAt` 等价（必须按 UTF-16 code unit 迭代，代理对拆成两个 unit）。

`deepEqual(a, b) = a === b || canonicalJson(a) === canonicalJson(b)`。

`assertValid(reason, what, schema, value)`：
- `Value.Check(schema, value)` 通过则返回；
- 否则取 `Value.Errors(...)` 的**前 3 个**，格式 `${error.instancePath || "/"} ${error.message}`，用 `"; "` 连接；
- 抛 `invalid(reason, `${what}: ${summary}`, errors)`（`details` 是这 3 个错误对象数组）。

`normalize(schema, value, what)`：`Value.Clean(schema, Value.Default(schema, Value.Clone(value ?? {})))` 然后 `assertValid("input", what, schema, normalized)`；返回新值。注意 `value ?? {}`：`undefined`/`null` 变成 `{}`。

`canonicalIdentity(schema, value)`：`canonicalJson(stripDefaults(schema, value))`。`stripDefaults`：
- 数组：按 `schema.items`（tuple 数组或单 schema）递归；
- 非对象：原样；
- 对象：按 `schema.properties[key]` 找属性 schema；若该 schema 含 `default` 且 `deepEqual(default, item)` 则**省略该键**；否则递归。

`fits(subSchema, supSchema): boolean` —— 保守结构子类型（"sub 的每个合法值都是 sup 的合法值"）。算法（顺序敏感）：
1. 两侧去注解（`ANNOTATIONS = title, description, default, examples, $id, $schema, $comment, readOnly, writeOnly, deprecated`）。
2. sup 无键 → true。
3. `deepEqual(sub, sup)` → true。
4. sup 含 `KNOWN` 之外的键 → false。`KNOWN = CONSTRAINTS ∪ {type, properties, required, additionalProperties, patternProperties, items, additionalItems, const, enum, anyOf, parameters, returnType}`；`CONSTRAINTS = minLength,maxLength,pattern,format,minimum,maximum,exclusiveMinimum,exclusiveMaximum,multipleOf,minItems,maxItems,uniqueItems,minProperties,maxProperties`。
5. sup 的 `additionalProperties` 存在且非 boolean → false。
6. sub 有 `anyOf` → 每个分支都 fits sup。
7. sup 有 `anyOf` → 任一分支被 sub fits。
8. sup 有 `const` → sub 必须有 `const` 且 deepEqual。
9. sup 有 `enum` → sub 的候选值（`const` 或 `enum`）每一个都在 sup.enum 中（deepEqual）。
10. CONSTRAINTS：sup 中出现而 sub 对应值不 deepEqual → false。
11. `typesOf`：`schema.type` 为字符串或字符串数组；两侧都必须是数组，否则 false。sub 每个 type 必须出现在 sup（`integer` 可由 `number` 覆盖）。
12. sub 有 `const` 或 `enum` → true。
13. sub 含 `object` → `objectFits`；含 `array` → `arrayFits`；含 `function` → `functionFits`。
14. 否则 true。

`objectFits(sub, sup)`（逐字）：
- `subRequired = new Set(sub.required ?? [])`；sup.required 每个键都必须在 subRequired 中，否则 false。
- 遍历 `Object.entries(supProperties)`（sup.properties）：
  - `key in subProperties` → `fits(subProperties[key], property)`，失败 → false；
  - 否则 `typeof sub.additionalProperties === "object" && !fits(sub.additionalProperties, property)` → false。**注意**：sub 的 `additionalProperties` 为 `true`、`false` 或缺失时这一支**不返回 false**（开放 consumer 对象在调用前会按消费者 schema 把未知名清理掉，见 `RecordImpl.call`）；只有 schema 形式（对象）的 `additionalProperties` 才需要 fits。
- `sup.additionalProperties === false` → 要求 `sub.additionalProperties === false`，且 subProperties 的每个键都在 supProperties 中。
- `supPatterns` 非空时：
  - `sub.additionalProperties !== false && Object.keys(subPatterns).length === 0` → false（否则 consumer 允许任意额外名字，等于允许匹配名下任意值）；
  - 对每个 `[pattern, property]`：`matcher = new RegExp(pattern, "u")`；遍历 subProperties，凡"不在 supProperties 中且 `matcher.test(key)`"的键必须 `fits(subProperty, property)`；
  - 若 subPatterns 非空，则 sup 的每个 pattern 必须**同名**出现在 subPatterns 中且 fits；
  - 最后 subPatterns 的每个 pattern 必须出现在 supPatterns 中（同名）。
- 其余情况 true。

`arrayFits(sub, sup)`：sup.items 为数组（tuple）→ sub.items 也必须是等长数组且逐项 fits；否则若 sub.items 是数组 → 每项 fits sup.items；否则 `fits(sub.items, sup.items)`。

`functionFits(sub, sup)`：sub.parameters 长度 > sup.parameters 长度 → false；每个共享参数逆变（`fits(supParam, subParam)`）；`fits(sub.returnType, sup.returnType)`。

Rust：需自建 JSON Schema 子集求值器（TypeBox 的 `Value.Check` 覆盖 draft-07 语义 + `Type.Object`/`Type.Union`/`Type.Literal`/`Type.Enum`/`Type.Record`/`Type.Array`/`Type.Integer`/`Type.String`/`Type.Number`/`Type.Boolean`/`Type.Unknown`/`Type.Unsafe`/`Type.Optional`/`Type.Immutable`/`Type.Function`/`Type.Void`）。建议：`jsonschema` crate 无法给 TypeBox 默认值/Clean 语义，需自写 `check/default/clean/errors`（错误对象的 `instancePath` 与 `message` 文案必须逐字，TypeBox 的 message 形如 `Expected string`、`Expected union value`、`must be at least 1` 等 —— 见 §3.4 与 §8.5）。

### 2.6 `src/types.ts`（89 行）

```ts
export interface GetOptions<P = unknown> { readonly principal?: P; readonly state?: boolean; }
export interface Ctx<P = unknown> {
  readonly signal: AbortSignal;
  readonly rootId: string;
  readonly codeVersion: number;
  readonly principal: P;
  get<C extends AnyContract>(type: C, input: InputOf<C>, options?: GetOptions<P>): Promise<Ref<C>>;
}
export type StateEvent<T> =
  | { readonly type: "snap"; readonly value: T }
  | { readonly type: "ops"; readonly ops: readonly Op[]; readonly value: T }
  | { readonly type: "reset"; readonly value: T };
export interface Readable<T> {
  readonly value: T;
  subscribe(listener: (value: T) => void): () => void;
  events(listener: (event: StateEvent<T>) => void): () => void;
}
export type StateSource<T> = Readable<T>;
export interface Exposed<T> { readonly [EXPOSED]: T; }     // 品牌 symbol
export type RefStatus = "loading" | "live" | "released" | "gone";
export interface Shape {
  readonly variant: string | undefined;
  readonly members: readonly string[];
  readonly contract: string;      // provider 契约指纹
}
export interface RefBase<C extends AnyContract> {
  readonly contract: C; readonly status: RefStatus; readonly ready: Promise<void>;
  readonly variant: string | undefined; readonly shape: Shape | undefined;
  readonly error: unknown;
  onShape(listener: (shape: Shape) => void): () => void;
  onStatus(listener: (status: RefStatus, error: unknown) => void): () => void;
  release(): void;
}
```

语义要点（写入 Rust 文档与测试）：
- `subscribe` **立即**用当前值回调一次；返回取消函数。
- `events` 立即发 `{type:"snap", value}`，之后 `ops` 或 `reset`。
- `status` 的 `loading` 也出现在上游链路重连时（保留最后值）。

### 2.7 `src/declare.ts`（38 行）

```ts
export interface Subscribable<E> { subscribe(listener: (event: E) => void): () => void; }
export interface ResourceDeclaration<T> {
  readonly version: number;
  readonly deps?: readonly unknown[];
  readonly exclusive?: boolean;
  open(options: { readonly signal: AbortSignal }): T | Promise<T>;
  close?(value: T): void | Promise<void>;
}
export interface Resource<T> { readonly key: string; readonly value: T; }   // value: not_ready 直到 open
export interface ListenerDecl { readonly source: unknown;
  readonly select: ((value: never) => Subscribable<unknown>) | undefined;
  readonly fn: (event: unknown, ctx: Ctx) => void | Promise<void>; }
export type ValueOf<S> = ...; export type EventOf<S> = ...;   // 类型级
```

### 2.8 `src/registry.ts`（30 行）

```ts
export interface Registry<V> { readonly name: string; readonly conflict: "error" | "last"; readonly value?: V; }
export const defineRegistry = <V>(name, options: { conflict?: "error" | "last" } = {}): Registry<V> =>
  ({ name, conflict: options.conflict ?? "error" });
export type RegistryTable<V> = ReadonlyMap<string, V>;
export interface RegistryView<V> {
  table(ctx?: Ctx): RegistryTable<V>;
  get(key: string, ctx?: Ctx): V | undefined;
  subscribe(listener: (table, codeVersion: number) => void): () => void;   // 立即回调当前表
}
export interface Registration { readonly registry: Registry<unknown>; readonly key: string; readonly value: unknown; }
```
`conflict` 默认 `"error"`。`"last"` 保留后注册者。表按 **first-registration order** 排序并跨 reload 保留（`HostImpl.#order`）。

### 2.9 `src/publish.ts`（39 行）— 全局发布队列

模块级状态（**整个进程共享，不是每 Host**）：
```
const queue: (() => void)[] = [];   // FIFO
let held = 0;                       // holdPublications 嵌套深度
let draining = false;               // 重入保护
```
- `enqueue(task)`：`queue.push(task); drain();`
- `holdPublications(fn)`：`held++`，`try { return fn() } finally { held--; drain(); }`
- `drain()`：`if (draining || held > 0) return;` 置 `draining = true`，循环 `queue.shift()` 调用，`finally` 复位。

语义要点：
- 回调**同步**执行（不是在微任务里）；`enqueue` 在队列空闲时立即执行任务。
- 任务抛错会中断 drain（调用方必须自行捕获；文档要求"tasks must not throw"）。
- 重入的 `enqueue` 在 `draining` 期间只入队，由外层循环继续消费，因此**同一 drain 循环** 内保证 FIFO。
- Rust：`static PUBLISH: Mutex<VecDeque<Box<dyn FnOnce()>>>` + 线程局部 `held/draining`（或用 `parking_lot` + `RefCell`）。回调必须是 `FnOnce() + Send`（若跨线程），实际 Chord 是单线程事件循环，Rust 端建议限定在 tokio 当前线程（`LocalSet`）或用 `!Send` 的 `Rc` 回调。

### 2.10 `src/cell.ts`（402 行）

```ts
export interface SourceOptions { readonly dev: boolean; readonly report: (error: unknown) => void; }
export type WriteGuard = (ctx: Ctx) => void;
let sequence = 0;                      // 模块级：每个 Source 分配单调递增 seq
class Fanout<T> { subscribe(listener, current); events(listener, current); emit(event); }
export const HYDRATION: unique symbol = Symbol("chord.hydration");
export interface Hydration { readonly hydrated: boolean; whenHydrated(): Promise<void>; }
export function hydrationOf(value: unknown): Hydration | undefined;
export const unhydrated = (sources: Iterable<unknown>): Hydration[];
export abstract class Source<T> implements Readable<T> {
  readonly key: string; readonly seq: number; readonly dependents: Set<Derived<unknown>>;
  schemas: readonly TSchema[]; protected options; protected fanout;
  abstract get value(): T;
  subscribe(listener): () => void;       // 立即用当前值回调
  events(listener): () => void;          // 立即 {type:"snap", value}
  validate(value: unknown): void;        // 仅 dev：对 ownSchemas() + schemas 逐个 assertValid("state", `state ${key}`, …)
  protected ownSchemas(): readonly TSchema[];   // 默认 []
}
export class Cell<T> extends Source<T> {
  schema: TSchema;                       // reload commit 时替换
  constructor(key, schema, initial: {value:T}|undefined, guard: WriteGuard, options: SourceOptions);
  initialize(value: T): void;            // 仅 init cell；已初始化抛 Error(`state ${key} is already initialized`)
  get value(): T;                        // 未初始化抛 not_ready(`state ${key} (its init has not run)`)
  change(ctx, fn: (draft) => T|void): void;
  replace(ctx, value: T, options?: {diff?: boolean}): void;
  reset(value: T, schema: TSchema): void;
}
function unbox(ops, value): Op[];        // 去掉 {v: …} 包装
export type Sources = readonly Readable<unknown>[];
export class Derived<T> extends Source<T> {
  readonly sources; attach(precomputed?); detach(); compute(read); refresh(); install(next);
}
export const isSource = (value): value is Cell|Derived;
export const notReady = (what: string): ChordError => new ChordError("not_ready", `${what} is not ready`);
```

Fanout 细节：
- `subscribe(l,current)`：加入集合 → 用 current **同步**调用（异常上报，不抛给订阅者之外的写者）→ 返回移除函数。
- `events(l,current)`：加入 → 同步回调 `{type:"snap", value: current}`。
- `emit(event)`：对当前集合做快照，`enqueue(() => …)`；投递时**再次检查**监听者是否仍订阅（"a listener that unsubscribes before delivery is skipped"）；events 监听者收事件，values 监听者收 `event.value`。

Cell 细节：
1. 构造时若有 `initial`，`assertValid("state", `initial value of state ${key}`, schema, initial.value)`；tracker 以 `{v: initial?.value ?? null}` 初始化；`#initialized = initial !== undefined`。
2. `initialize(value)`：若已初始化抛 `Error`；校验 `initial value of state ${key}`；**重建 tracker**（`track({v:value})`）；不发布任何通知。
3. `change(ctx, fn)`：
   - `guard(ctx)`（写权限）；
   - 未初始化 → `not_ready`；
   - `change = tracker.beginChange()`；draft 为 `change.state`（Box 的 draft），传给 `fn(draft.v)`；
   - 返回 `Promise` → `TypeError("cell.change callbacks must be synchronous")`；
   - 返回值非 `undefined`：若 `result !== draft.v` 则 `assertValid("state", `state ${key}`, schema, result)`；然后 `draft.v = result`；
   - `prepared = change.prepare()`；任一异常 → `change.abort()` 并重抛；
   - `#publish(prepared, "ops")`。
4. `replace(ctx, value, {diff})`：
   - `diff` 默认 `sharesStructure(before, value)`（`b` 在某个顶层键上复用了 `a` 的容器 → 视为同一状态的新修订）；
   - `ops = diff ? diffRevisions(before, value) : undefined`；
   - `#publish(tracker.prepareReplace({v:value}), "ops", true, ops)`。
5. `reset(value, schema)`：安装迁移值与 schema，`#publish(prepareReplace({v:value}), "reset", false)` → 订阅者收到 `{type:"reset", value}`。
6. `#publish(prepared, type, validate=true, ops?)`：
   - 若 `prepared.ops.length === 0 && type === "ops"`：仅 `tracker.adopt(prepared)`，**不发布**（**也不重算 dependents**）；
   - `value = prepared.value.v`；`try { if (validate) this.validate(value); candidates = computeDependents(this, value, validate) } catch { prepared.abort(); throw }`；
   - `holdPublications(() => { tracker.adopt(prepared); const published = ops !== undefined && ops.length > 0 ? ops : unbox(prepared.ops, value); fanout.emit(type === "reset" ? {type, value} : {type, ops: published, value}); for (const [d,next] of candidates) d.install(next); })`。
7. `unbox` 规则：遇到 `r` 或空路径（`op[1].length === 0`）→ 整体返回 `[["r", value]]`；`p`/`m` 去掉路径首段 `v`；`d` 去掉首段；`s`/`a`/`t` 去掉首段，且去后路径为空则整体 `[["r", value]]`。
8. `sharesStructure(a,b)`：任一非容器/`a===b` → false；数组性不同 → false；对 `b` 的每个 `Object.entries`，若 `child` 是容器且 `a[key] === child` → true。
9. `computeDependents(changed, value, validate=true)`：`pending` 集合从 changed 出发**递归 DFS**（先加后递归，递归序即插入序）；`values = Map([[changed, value]])`；按 `derived.seq` **升序**排序逐个 `compute(source => values.has(source) ? values.get(source) : source.value)`，validate 时 `derived.validate(next)`，写入 `values` 与结果 map。
10. `Source.validate(value)`（仅 dev）：对 `ownSchemas()`（Cell → `[schema]`，Derived → `[]`）与 `schemas` 逐个 `assertValid("state", `state ${key}`, schema, value)`。
11. `seq` 是模块级 `let sequence = 0; ++sequence`，Cell 与 Derived 共用一个计数器（排序用）。

Derived 细节：
- 构造：`sources.map` 检查 `isReadable`（有 `subscribe` 函数且 `"value" in`），否则 `TypeError(`derive ${key}: sources must be readables`)`；初值 `undefined`。
- `[HYDRATION]`：若存在未 hydrated 的 sources → `{ hydrated:false, whenHydrated: () => Promise.all(...).then(()=>{}) }`；否则 `undefined`。
- `attach(precomputed?)`：值 = `precomputed?.has(this) ? precomputed.get(this) : compute(s => s.value)`；对每个 source：是 `Source` → `source.dependents.add(this)`；否则 `external.push(source.subscribe(() => this.refresh()))`。
- `detach()`：从 `Source.dependents` 移除并取消 external 订阅。
- `compute(read)`：`fn(...sources.map(read))`；返回 Promise → `TypeError(`derive ${key}: fn must be synchronous`)`。
- `install(next)`：先 `#value = next`；`ops = (before === undefined || next === undefined) ? [["r", next ?? null]] : diffRevisions(before,next)`；`ops.length > 0` 才 `fanout.emit({type:"ops", ops, value: next})`（**值相同不发事件**）。
- `refresh()`：`compute` → `computeDependents(this,next)` → `holdPublications(() => { install(next); dependents install })`；异常上报（源写已经发生）。

Rust：`Source` trait（`value()`，`subscribe`，`events`，`schemas`，`dependents`）；`Cell<T>` 内部用 `Tracker`（§2.29）；`Derived` 用 `Rc<dyn Fn>`。

### 2.11 `src/state.ts`（423 行）— replicated state

接口（**本节完整给出**；§2.6 只讲 `types.ts`，与本节无关）：`ReplicatedState<T>` value 可能为 undefined；`subscribe` 两种重载（返回 Promise 或 void）。`MutableReplicatedState<T extends object>` 有 `change(context, mutate)` 与 `replace(context, value)`。

```ts
export interface ReplicatedStateDelivery { readonly kind: "hydrate" | "update"; readonly sequence: number }
export interface ReplicatedState<T> {
  readonly value: T | undefined;
  subscribe(listener: (value: T, context: Context, delivery: ReplicatedStateDelivery) => Promise<void>): () => void;
  subscribe(listener: (value: T, context: Context, delivery: ReplicatedStateDelivery) => void): () => void;
}
export interface MutableReplicatedState<T extends object> extends ReplicatedState<T> {
  readonly value: T;
  change(context: Context, mutate: (draft: Draft<T>) => void): void;
  replace(context: Context, value: T): void;
}
export interface ReplicatedStateSourceFrame<T> {
  readonly cursor: number;                       // 单调游标；snapshot 之后第一帧必须 = snapshot.cursor + 1
  readonly value: T;
  readonly ops: readonly Op[];                   // 产生该 value 的不可变操作批次（Chord 只发布，不应用、不重算）
  readonly context: Context;
}
export interface ReplicatedStateSourceAttachment<T> {
  readonly snapshot: { readonly value: T; readonly cursor: number };
  activate(listener: (frame: ReplicatedStateSourceFrame<T>) => void): void;   // 单次使用；同步排空已缓冲帧
  dispose(): void;                                                            // 必须幂等
}
export interface ReplicatedStateSource<T> { attach(): ReplicatedStateSourceAttachment<T> }
export interface ReplicatedStateSourceOptions {
  readonly onError?: (error: Error) => void;     // 默认 reportErrorAsync
}
export interface AttachedReplicatedState<T> extends ReplicatedState<T> {
  readonly value: T;
  dispose(): void;                               // 幂等；最后发布的值仍可读
}
```

常量与类：
- `StateSubscriber<T>`：每订阅者自己的队列。`push(frame)`：closed 时丢弃；若 `pending.length === 100`，**保留未开始投递时的第一个 hydration 帧**，清空队列再 push 新帧（"overflow keeps only the newest pending value/context/delivery"）。`drain()`：若 running/closed 返回；循环 shift；第一帧把 `started = true`；listener 返回 PromiseLike → 挂 `.then(resume, error => { report(error); resume(); })` 并 **return**（串行）；同步抛错 → report 后继续。`close()`：closed + clear。
  注意：溢出判定阈值是 100（硬编码，非导出常量）。`push` 的精确逻辑：`if (#closed) return; if (#pending.length === 100) { const hydration = #started ? undefined : #pending[0]; #pending.length = 0; if (hydration !== undefined) #pending.push(hydration); } #pending.push(frame);` —— 即**只在恰好 100 时**触发一次折叠，折叠后队列为 `[首个 hydration?]` 再 push 新帧，因此 `delivery.sequence` 可能跳号。`drain` 的串行点是：listener 返回 PromiseLike 时 `void Promise.resolve(result).then(() => #resume(), error => { #report(error); #resume(); })` 后**立即 return**；`#resume()` 复位 running 再 `drain()`。
- `ReplicatedStatePublisher<T>`：
  - 构造 `(initial, reportError = reportErrorAsync)`；`#sequence = 0`。
  - `snapshot()` → `{ value, sequence }`。
  - `subscribe(listener)`：创建 subscriber 并记录 `hydratedSequence = sequence`，`push({value, context: BACKGROUND_CONTEXT, delivery: { kind: "hydrate", sequence }})`，`drain()`；返回 `() => { close(); listeners.delete }`。
  - `publish(value, context)`：更新 value、`sequence++`、push 到 `#publications`；若已在 delivering 直接返回；否则循环 shift，每个 publication 对每个 subscriber，若 `publication.sequence <= hydratedSequence` 跳过，否则 push `{kind:"update", sequence}` 并 `drain()`。
- `MutableReplicatedStateImpl<T>`：
  - 构造：`tracker = track(initial)`，publisher 以 `tracker.value` 建立。
  - `change(context, mutate)`：`#changing` 为真 → `Error("Replicated state cannot be changed reentrantly from a change callback")`；否则置真，`beginChange()`；回调返回 PromiseLike → 挂 catch 后 `TypeError("Replicated state change callbacks must be synchronous")`；`prepare()` 失败 → `abort()` 重抛；`finally` 复位 `#changing`；`tracker.adopt(prepared)`；`prepared.ops.length > 0` 才 `publisher.publish(prepared.value, context)`。
  - `replace(context, value)`：`#changing` → `Error("Replicated state cannot be replaced from a change callback")`；`prepareReplace` + `adopt`；ops 非空才 publish。
- `AttachedReplicatedStateImpl<T>`：
  - 构造：`assertCursor(snapshot.cursor, "snapshot")`（`Number.isSafeInteger` 否则 `TypeError(`Replicated state source ${kind} cursor must be a safe integer`)`）；`#cursor = snapshot.cursor`；publisher 以 snapshot.value 建立。
  - `activate()` 把 `#receive` 交给 attachment。
  - `dispose()` 幂等。
  - `#receive(frame)`：disposed 返回；`try { assertCursor(frame.cursor, "frame"); const expected = #cursor + 1; 不等 → Error(`Replicated state source cursor has a gap: expected ${expected}, received ${frame.cursor}`); #cursor = frame.cursor; publisher.publish(frame.value, frame.context) } catch (error) { #fail(toError(error)) }` —— **游标错误与 publish 抛错都会 `#fail`**。
  - `#fail(error)`：disposed 返回；置 disposed；调用 attachment.dispose()；dispose 也抛 → `AggregateError([error, disposeError], "Replicated state source contract failed")`；否则 report(error)。
- `attachReplicatedStateSource(source, options = {})`：`attach()` → 构造 → `activate()`；任一步抛错则 dispose 后重抛；dispose 也抛 → `AggregateError([error, disposeError], "Failed to attach replicated state source")`。
- `replicatedState(initialOrSource, options?)`：`typeof value.attach === "function"` → attached；否则 mutable。
- `reportErrorAsync(error)`：`queueMicrotask(() => { throw error; })`（未捕获异常）。

Rust：`tokio::sync::mpsc` + 每订阅者串行任务；注意 "最多 100 个 pending、保留首个 hydration" 的精确规则；`BACKGROUND_CONTEXT` 的文本逐字。

### 2.12 `src/watch.ts`（343 行）

类型：
```ts
export type WatchEnd =
  | { reason: "stopped" | "cancelled" | "retired" | "ended" }
  | { reason: "listener_error"; error: Error }
  | { reason: "failed"; error: unknown };
export interface WatchHandle<T> { readonly value: T; start(listener: (value, ops: readonly Op[], context) => Promise<void>): void;
  stop(): Promise<WatchEnd>; readonly closed: Promise<WatchEnd>; }
export interface Watchable<T> { readonly value: T; watch(context: Context): Promise<WatchHandle<T>>; }
export interface NativeWatchable<T> { readonly value: T; watch(context: Context): Promise<NativeWatchHandle<T>>; }
export interface NativeWatchHandle<T> { readonly value: T; start(listener: (value, context) => Promise<void>): void;
  stop(): Promise<WatchEnd>; readonly closed: Promise<WatchEnd>; }
```
`WatchLifecycle`（抽象）：
- 字段：`#detach`、`#report`、`#closed = Promise.withResolvers<WatchEnd>()`、`#value`、`#listener`、`#started`、`#scheduled`、`#running`、`#end`、`#cancellationSignal`、`#cancellationListener`。
- `start(listener)`：已 started → `Error("Watch is already started")`；已 end → `Error("Watch is stopped")`；置 started/listener，`#schedule()`。
- `stop()`：`#terminate({reason:"stopped"})`，返回 `closed`。
- `observeCancellation(signal)`：已安装过 → `Error("Watch cancellation is already installed")`；已 end 直接返回；记录并在 abort 时 `cancel()`；若已 aborted 立即 cancel。
- `cancel()` → `{reason:"cancelled"}`；`end(failure?)` → `ended` 或 `{reason:"failed", error}`。
- `pending()`：started 才 `#schedule()`。
- `#schedule()`：`#scheduled || #running || #end` 时返回；置 scheduled；`queueMicrotask(() => { #scheduled=false; void #drain(); })` —— **绝不内联调用 listener**。
- `#drain()`：running/end/未 started 返回；置 running；`while (#end === undefined)`：取 `listener`（undefined → break）；`next()` 无 → break；`#value = delivery.value`；`try { await invoke(listener, delivery, withoutAbortSignal(delivery.context)) } catch (error) { if (#end === undefined) { #report?.(error); #terminate({reason:"listener_error", error: toError(error)}) } break }`；`delivery.retires` → `#terminate({reason:"retired"})` + break。finally 复位 running。
  要点：`#value` 在 **invoke 之前**更新（`handle.value` 在 callback 运行期间已是新值）；传给 listener 的 context 是 **`withoutAbortSignal(delivery.context)`**（遮蔽取消信号）；`#report` 在 `#terminate` 把它清成 undefined 之前捕获（用 `#report?.(error)`）。
- `#terminate(end)`：幂等；记录 end；先取出 `detach`，再把 `#detach = undefined`、`#report = undefined`、`#listener = undefined`；`release()`（子类清 pending 及对 source 的引用）；若装了取消监听 → `removeEventListener("abort", …)` 并清两个字段；`detach?.()`；`#closed.resolve(end)`。
- `stop()` **总是**调用 `#terminate({reason:"stopped"})` 再返回 `closed`（已终止时是 no-op，返回既有 end）；`cancel()`/`end(failure?)` 同理。
- `open` getter（protected）：`#end === undefined`。
- `pending()`（protected）：`#started` 才 `#schedule()`（start 之前到达的帧只排队，不投递）。
- `next()`/`invoke()`/`release()` 是**抽象方法**，由 `Watch` 与 `NativeWatch` 实现；`invoke(listener, delivery, context)` 只负责把 `delivery` 拆成实参（`Watch` → `listener(value, ops, context)`；`NativeWatch` → `listener(value, context)`）。
- `toError`：Error 原样；否则尝试 `new Error(String(error))`，再失败 `new Error(Object.prototype.toString.call(error))`。

Watch（JSON）：
- `#ops = new PendingOps()`、`#newest`、`#retiring`。
- `advance(value, ops, context)`：`!open || #retiring` 返回；`#ops.push(ops)`（整批交给 `PendingOps`）；`#newest = {value, context}`；`pending()`。
- `retire(context)`：非 open 或已 retiring 返回；置 retiring；push `RETIREMENT_OPERATIONS = [["r", null]]`；newest = `{value: null, context}`；pending()。（类型上 `this: Watch<T|null>`）
- `next()`：`newest = #newest`；`undefined` → 返回 `undefined`（无待投递）；否则**先** `#newest = undefined`；`batch = #ops.take()`；`ops = batch.kind === "ops" ? batch.ops : [["r", newest.value]]`（replace 分支由 owner 用最新值自建，见 §2.29）；返回 `{value: newest.value, ops, context: newest.context, retires: #retiring}`。
- `release()`：`#ops.take()`（丢弃）+ `#newest = undefined`。

NativeWatch：
- `MAX_PENDING_NATIVE_VALUES = 100`（未导出）。
- `advance(value, context)`：`!open` 返回；`#pending.length < 100` → push `{value, context, retires:false}`；否则**清空**（`length = 0`）后 push 一个 `{value: replace === undefined ? value : replace(), context, retires:false}`（即替换值只在溢出那一刻求值一次）。`pending()`。
- 构造签名：`(value, detach, report, replace?)` —— 第四参 `replace?: () => T` 在溢出折叠时**替代**最新值（延迟求值，仅当真的折叠且未提供新值时调用 `replace?.() ?? value`）。
- `next()` → `#pending.shift()`；`invoke(listener, delivery, context)` → `listener(delivery.value, context)`；`release()` → `#pending.length = 0` 且 `#replace = undefined`。

测试编码的关键行为（§7）：start 前保持 acquisition value；不内联调用；串行 await；投递 newest frame 的 context 且去掉 abortSignal；溢出时 in-flight 帧不被折叠；retirement 丢弃 pending；重复 start 抛；cancel/ended/failed/listener_error 五类终止；NativeWatch 101 帧折叠为最新（或 replace 值）；终止后不保留 source/listener/signal。

### 2.13 `src/contract.ts`（440 行）

构造器与校验（全部逐字）：
- `method()` / `method(args)` / `method(args, result)`：返回 `{ [MEMBER]: "method", args, result }`。
- `opaque(id)`：正则 `^[^@\s]+@\d+$`，否则 `TypeError(`opaque id must be name@version: ${id}`)`。
- `hostNamed(name)`：非字符串或空 → `TypeError("hostNamed needs a host name")`；返回 `{name}`。
- `hostFamily(prefix, key)`：prefix 非字符串或空 → `TypeError("hostFamily needs a host-name prefix")`；key 同理 → `TypeError("hostFamily needs an input field name")`；返回 `{prefix,key}`。
- `resolveHost(typeName, placement, input)`：`"name" in placement` → `placement.name`；否则读 `input[key]`，必须是非空字符串，否则 `invalid("placement", `${typeName}: input field ${placement.key} names no host`)`；返回 `prefix + value`。
- `placedOn(placement, hostName)`：named → 相等；family → `hostName.startsWith(prefix) && hostName.length > prefix.length`。
- `placementText(placement)`：named → name；family → `` `${prefix}<${key}>` ``。
- `remote(name, def)`：`checkHeader`（空名 → `TypeError("contract name is empty")`；版本非正整数 → `TypeError("contract version must be a positive integer")`）；`host` 缺失或 `Value.Check(PlacementSchema, …)` 失败 → `TypeError(`remote contract ${name} needs \`host\`: hostNamed(…) or hostFamily(…)`)`；placement 只保留 `{name}` 或 `{prefix,key}`（**丢弃其它字段**）；`def.variants` 为空 → `TypeError(`remote contract ${name} has no variants`)`；每个 variant 先放 state（`describeMember(key, schema, "remote")`），再放 methods，键冲突 → `TypeError(`member ${key} is both state and method in ${name}.${kind}`)`，且 method 成员的 `describeMember` 结果必须 `kind === "method"` 否则 `TypeError(`methods.${key} is not a method()`)`；description = `{ name, scope:"remote", version, input, host, variants }`。
- `local(name, def)`：members 逐项 describe；description = `{ name, scope:"local", version, input, variants: { "": members } }`（**无 host 键**）。
- `describeMember(name, member, scope)`：保留名 → `TypeError(`member name is reserved: ${name}`)`；brand 为 method → `{kind:"method", ...(args 有则带), ...(result 有则带)}`；brand 为 opaque → remote 时 `TypeError(`remote contracts cannot expose opaque members: ${name}`)`，否则 `{kind:"opaque", id}`；其它一律视为 state → `{kind:"state", schema: member}`。
- `RESERVED_MEMBERS` 逐字 10 个：`variant`、`status`、`ready`、`shape`、`error`、`onShape`、`onStatus`、`release`、`then`、`contract`。`LOCAL_VARIANT = ""`。
- 指纹：`fingerprint = hash64(canonicalJson(description))`。
- `DescriptionSchema`（TypeBox）：`{ name: minLength 1, scope: "remote"|"local", version: integer, input: object, host?: PlacementSchema, variants: Record<string, Record<string, MemberDescriptionSchema>> }`；`PlacementSchema` 两个分支都 `additionalProperties:false`。`MemberDescriptionSchema` 三选一：`{kind:"method", args?, result?}`（`SchemaObject` 是宽松对象）、`{kind:"state", schema}`、`{kind:"opaque", id: string}`。
- `contractFromDescription(description)`：校验 `typeof name === "string"`、scope 合法、`Number.isInteger(version)`、`(scope === "remote") === (host !== undefined)`，失败 → `new ChordError("invalid","malformed contract token",{reason:"token"})`；remote 的 `def` = `{version, input, host, variants: {}}`（**variants 故意为空**），local 的 def = `{version, input}`。
- `coverage(provider, consumer): string | undefined`（返回原因短语，逐字）：
  1. `provider.name !== consumer.name || scope 不同` → `"different type"`；
  2. `samePlacement` 失败（canonicalJson(host ?? null) 不等）→ `"host"`；
  3. `!fits(consumer.input, provider.input)` → `"input"`；
  4. 逐 variant：provider 缺该 variant → `` `variant ${kind} missing` ``；逐成员：offered 缺失或 kind 不同 → `` `${kind}.${name} missing or of another kind` ``；method：`(wanted.args === undefined) !== (offered.args === undefined)` → `` `${kind}.${name} arguments` ``；两者都有 args 且 `!fits(wanted.args, offered.args)` → 同上；`wanted.result !== undefined && (offered.result === undefined || !fits(offered.result, wanted.result))` → `` `${kind}.${name} result` ``；state：`!fits(offered.schema, wanted.schema)` → `` `${kind}.${name} state` ``；opaque：id 不同 → `` `${kind}.${name} opaque ${offered.id} != ${wanted.id}` ``。全部通过返回 undefined。
- `bind(provider, accepts, consumer): Adapter | undefined`：
  - 指纹相同 → undefined；
  - placement 不同 → `ChordError("contract_mismatch", `${provider.name} lives on ${placementOf(provider.description)}, the consumer expects ${placementOf(consumer.description)}`)`（`placementOf`：无 host → `"(local)"`，否则 `placementText`）；
  - 版本相同：`coverage` 无原因 → undefined；否则 `ChordError("contract_mismatch", `${provider.name}@${provider.version} does not cover the consumer's description (${reason}); forgotten version bump?`)`；
  - 版本不同：`accepts?.[consumer.version]` 缺失 → `ChordError("contract_mismatch", `${provider.name}: provider version ${provider.version}, consumer version ${consumer.version}, no accepts adapter`)`；存在则返回该 adapter。
- `takesNoArgs(contract, name)`：遍历 variants，找到第一个 kind 为 method 的同名成员，返回 `member.args === undefined`；找不到返回 false。

### 2.14 `src/wire.ts`（181 行）

见 §4.3 的完整帧表。此处记录函数：
- `PROTOCOL_VERSION = 4`。
- `isFrame = (value) => Value.Check(FrameSchema, value)`。
- `toWireError(error)`：ChordError 且 reason 为 undefined → `{code, message}`；有 reason → `{code, message, reason}`；非 ChordError → `{ code: "unavailable", message: error instanceof Error ? error.message : String(error) }`。
- `fromWireError(error)`：`new ChordError(error.code, error.message.replace(/^[a-z_]+(\/[a-z_]+)?: /, ""), { reason: error.reason })`（**去掉消息前缀**，避免重复）。

### 2.15 `src/link.ts`（415 行）

接口与实现要点：

`Link`：`closed`、`send(frame): number | undefined`（返回编码后大小；transport 不报大小返回 undefined → 流控计 0）、`receive(handler)`（只设一个接收者；早到的帧排队）、`onClose(handler)`（已关闭立即以 `"closed"` 调用）、`close(reason?)`（发 `bye` 后本地 ended）。

`BaseLink`（抽象基类，非导出）：
- `send`：closed → undefined；否则 `transmit(frame)`。
- `receive`：设置 handler 并排空队列。
- `onClose`：closed → 立即 `handler("closed")`。
- `close(reason = "closed")`：幂等；尝试 `transmit({t:"bye", reason})`（异常吞掉）；`ended(reason)`。
- `deliver(frame)`：若有 `#decoding` 则 `deliverLater(Promise.resolve(frame))` 否则 `#deliverNow`。
- `deliverLater(promise)`：串到解码链尾部，`finally` 清理；解码失败 → `close("protocol error")`。
- `#deliverNow`：closed 丢弃；`frame.t === "bye"` → `ended(frame.reason ?? "peer closed")`；无 handler → 入队；否则调用。
- `ended(reason)`：幂等；closed=true；清队列；`shutdown()`（finally 中）通知所有 close handler 一次并清空。

Packing：
- `PACK_THRESHOLD = 64 * 1024`（**字符数**，不是字节）。
- zlib 通过 `globalThis.process?.getBuiltinModule?.("node:zlib")` 获取；浏览器无 → 用 `DecompressionStream("deflate-raw")` 异步解压。
- `Packed { bytes: Uint8Array }`。
- `unpack(value)`：非 Packed 原样；Packed 且无 zlib → `TypeError("packed payload without zlib")`；否则 `JSON.parse(inflateRawSync(bytes).toString("utf8"))`。
- `payloadField(frame)`：`call` → 有 `args` 则 `"args"`；`result`/`state` → 有 `value` 则 `"value"`；其它 undefined。
- `packedBytes(value, compress, json?)`：Packed → 原 bytes；`!compress || !zlib || json===undefined || json.length <= PACK_THRESHOLD` → undefined；否则 `deflateRawSync(json, { level: 1 })`。
- `packObject(frame, compress)`：有 payload 字段且需打包 → `{ ...envelope, z: [field, base64(bytes)] }`（envelope = 去掉 payload 字段的帧）。**base64 是标准表，无换行**。
- `unpackObject(message)`：无 `z` 原样；`z` 不是 `[string, string]` 或 field 不在 `{"args","value"}` → `TypeError("bad packed payload")`；否则 `{...rest, [field]: new Packed(base64decode)}`。
- `packMessage(frame, compress)`（WebSocket）：无打包 → `JSON.stringify(frame)`；有打包 → `[u32 BE envelopeLength][envelope JSON，含 "z": field][deflated payload]`；未打包时用**字符串拼接**避免二次编码：`` `${head.slice(0,-1)},${JSON.stringify(field)}:${json}}` ``。
- `splitMessage(data)`：读 u32 BE 长度；解析 envelope（非对象 → `TypeError("bad envelope")`）；取 `z` 必须是 `"args"|"value"` 否则 `TypeError("bad packed payload")`；返回 `{envelope: rest, field, bytes: data.slice(4+length)}`（**slice 复制**）。
- `inflateMessage(data)`（导出；TUI launcher 用）：无 zlib → `TypeError("inflateMessage needs zlib")`；split 后把 payload 内联回文本帧。
- `parseFrame(data)`：`JSON.parse` → `unpackObject`；非对象或无 string `t` → `TypeError("not a frame")`。

MemoryLink：
- `peer?: MemoryLink`；`sever()` 双向静默丢帧（不触发 close）。
- `transmit`：severed → undefined；`JSON.stringify(packObject(frame, true))`（**压缩开启**；JSON 往返用于脱离引用并拒绝 bigint 等）；`#pending = #pending.then(() => { if (!severed && peer) peer.deliver(parseFrame(data)); })` —— 每方向一条 Promise 链保证 FIFO 且**异步**；返回 `data.length`。
- `shutdown()`：链上追加 `peer?.ended("peer closed")`。
- `memoryLinkPair()` 返回 `[a,b]` 互连。

IpcLink：
- 帧包装 `{ chord2: frame }`；其它消息（控制）原样通过。
- 消息处理：取 `.chord2`，undefined 忽略；否则 `deliver(unpackObject(frame))`，异常 → `close("protocol error")`。
- `disconnect` → `ended("ipc disconnected")`。
- 发送回调错误 → `ended(`ipc send failed: ${error.code ?? error.message}`)`。
- `transmit`：channel 已连接则 `send({chord2: packObject(frame, true)}, onSent)`；**返回 undefined**（IPC 不报大小、不受窗口约束）。
- `shutdown()` 移除监听。

WebSocketLink：
- 选项 `compress`（默认 false）、`acceptPacked`（默认 true）。
- 构造时若 socket 有 `binaryType` 设为 `"arraybuffer"`；`open` 事件排空 outbox；`message` 事件：string → `parseFrame` → deliver；二进制：`acceptPacked` 为 false → `TypeError("packed frames are not accepted")`；ArrayBuffer / ArrayBufferView → bytes；其它 → `TypeError("unknown message type")`；有 zlib → 同步 unpack 成 `Packed`；无 zlib → `deliverLater(inflate(payload)…)`；任何异常 → `close("protocol error")`。`close` 事件 → `ended("socket closed")`。
- `transmit`：`packMessage(frame, compress)`；`readyState === 1`（OPEN）立即 send，否则入 outbox；返回 text 的 `.length` 或 bytes 的 `.byteLength`。
- `shutdown()`：清 outbox；`readyState <= 1` 时 `socket.close(1000)`（**正常关闭码 1000**）。

### 2.16 `src/connection.ts`（1327 行）— 协议与流控核心

`LinkOptions`：`pingInterval`（默认 15000）、`pingTimeout`（默认 45000）、`window`（默认 256 KiB = `DEFAULT_WINDOW`）、`pendingCap`（默认 `window * 4`，`PENDING_WINDOWS = 4`）、`onProtocolMismatch({local, peer})`。

`AcceptOptions extends LinkOptions`：`principal: P`（必需）、`kind?: "host"|"client"`（默认 client）、`name?`（host 必需）、`restamp?(principal): P|undefined`、`maxCalls?: number`（默认无穷）。

`Peer { kind, name }`。

**RelayRef**（导出的类）：字段 `origin: Connection`、`frame: hold 帧`、`principal`、`originTag`、`generation`、`down?: Connection`、`downId`、`closed`、`stateWanted`、`urgent?: Set<string>`、`#shaped`、`awaiting: Set<string>`、`calls: Map<number, RelayCall>`。
- `originId` = `frame.id`；`client` = `originTag !== undefined`；`current` = `!closed && origin.open && origin.generation === generation`。
- `ready()`：closed → reject `ChordError("gone","relayed hold closed")`；已有 down → resolve；否则登记 waiter。
- `open(down)`：记录 down；`downId = down.nextId()`；`down.relayedRefs.set(downId, this)`；转发 hold 帧（保留 `at/token/input/principal`，按需带 `origin`、`state:false`、`again:true`）；resolve 全部 waiters。
- `wantState(wanted)`：未变化返回；否则更新；不想要时 `origin.dropHoldState(originId)` + 清 awaiting；向 down 发 `sub`/`unsub`。
- `resync(m)`：`awaiting.add(m)`；向 down 发 `resync`。
- `shape(frame)`：首个 shape 且 `frame.again !== true` 时 `urgent = new Set(frame.state)`；向 origin 发 `{...frame, id: originId}`。
- `state(frame)`：awaiting 中的 ops 丢弃；非 ops 的 reset 解除 awaiting；urgent 中的成员直接 `origin.send`，否则 `origin.sendState(out, {resync})`（`out.hold = originId`）。
- `close(error, {releaseDown, notify})`：幂等；取消所有 calls；`origin.dropHoldState`；从 origin.served 删除 relay 项；从 down.relayedRefs 删除；releaseDown 时向 down 发 `release`；reject waiters；notify 时发 `gone` 帧。

**RelayCall**：`relay`、`originId`、`downId`、`#pending`、`#settled`；构造时登记到 `relay.calls` 与 `relay.down?.relayedCalls`；`finish(error, value?)` 只结算一次并清理两处映射；`cancel(error, forward)` 可选转发 `cancel` 后 finish。

**Connection**（`implements RemoteChannel`）：
构造：
1. 设置 host/link/role/peer/originId/principal/restamp；`synced = !(role === "downstream" && peer.kind === "host")`；window/pendingCap/maxCalls/onProtocolMismatch。
2. `link.onClose(reason => #closed(reason))`；`link.receive(frame => #receive(frame))`。
3. **立即发送 hello**：`{ t:"hello", protocol: 4, name: host.name, kind: host.kind, codeVersion: host.current.id, acks: true }`。
4. **`if (link.closed) return;`** —— 构造过程中链路已被关闭，则不再起心跳（`hello` 已经发过）。
5. `n = 0`；`setInterval(interval)`：`Date.now() - lastSeen > timeout` → `link.close("ping timeout")`，否则 `send({t:"ping", n: ++n})`（`n` 从 1 开始）；随后 `pinger.unref?.()`（不阻止进程退出）。

公开成员：`open`、`name`（upstream 用 `#parentName`，downstream 用 `peer.name`）、`send`、`sendState`、`#makeRoom`、`#drop`、`#resetLater`、`#queueUnit`、`dropState(hold,m)`、`dropHoldState(hold)`、`waiting`（`{slots, bytes}`）、`#full`、`#onAck`、`#flush`、`#scheduleAck`、`nextId`、`whenSynced`、`markSynced`、`revoke`、`#protocolMismatch`、`#violation`、`hold`、`release`、`wantState`、`call`、`#receive`、`#dispatch`、`#onHello`、`#principalOf`、`#originOf`、`#onHold`、`#stateMembers`、`#unit`、`#sendUnit`、`#attach`、`#replay`、`#snapshot`、`#feedEvent`、`#resend`、`#onWantState`、`#onResync`、`#dropServed`、`#onCall`、`#admitRoot`、`#onHoldReply`、`#onResult`、`#onState`、`#closed`。

发送与流控（逐条）：
- `send(frame)`：`size = link.send(frame) ?? 0`；`#peerAcks === false` → 直接返回（不计数）；否则 `#sent++`、`#unacked.push(size)`、`#unackedBytes += size`。
- `sendState(frame, overflow)`：key = `` `s${hold}\u0000${m}` ``。无 slot 且 `!#full` → 直接 send。slot 是 unit → 丢弃本帧（unit 会覆盖）。无 slot → 建 slot（`base:undefined, ops:[], size:0, lazy:false, overflow`）。`frame.as !== "ops"` → 替换 base、清 ops、`size=0`、`lazy=false`。ops：`lazy` 时丢弃（reset 会覆盖）；`size = JSON.stringify(frame.ops).length`；超出 cap 先 `#makeRoom(size, slot)`；若 slot 已变 lazy 或被移除则返回；否则逐 op push 并累加。
- `#makeRoom(size, current)`：按 pending 插入顺序，若超 cap 且 slot 是 state、非 current、size>0 → `#drop`；循环后仍超则 drop current。
- `#drop(slot)`：减 pendingBytes；清 base/ops/size；有 `resend` → `lazy = true`；否则从 pending 删除并调用 `overflow.resync()`。
- `#resetLater(hold, m, resend)`：已有 unit slot → 返回；否则建立/复用 lazy state slot 并 `#flush()`。
- `#queueUnit(hold, flush)`：无 `u<hold>` 时设定 unit slot；`#flush()`。
- `dropState(hold,m)` / `dropHoldState(hold)`：删除并扣减字节。
- `#full = #peerAcks === true && #unackedBytes >= #window`。
- `#onAck(n)`：peerAcks 非 true、`n < #acked`、`n > #sent` → `#violation(`ack ${n} outside ${#acked}..${#sent}`)`；否则 `#unacked.splice(0, n - #acked)` 扣字节、`#acked = n`、`#flush()`。
- `#flush()`：按插入顺序遍历 pending；`#full` 时 return；逐个删除；unit → `slot.flush()`；state → 扣字节；lazy → `"resend" in overflow` 则 `overflow.resend()`；否则依次 send base 和 `{t:"state", hold, m, as:"ops", ops}`。
- `#scheduleAck()`：非 ack peer、已有 timer、link 未开 → 返回；`setTimeout(…, 0)` 发 `{t:"ack", n: #received}`；`unref()`。
- `whenSynced()`：link 未开 → reject `unavailable(`link to ${name ?? "peer"} closed`)`；已 synced → resolve；否则登记 waiter。
- `markSynced(codeVersion)`：设置 peerCodeVersion、synced=true，resolve 所有 waiter。
- `revoke(next)`：`generation++`；对所有 relay served 调 `relay.close(gone "principal invalidated", {releaseDown:true, notify:true})`；`next === undefined` → `link.close("principal invalidated")`，否则 `principal = next`。
- `#protocolMismatch(peer, close = true)`：幂等；回调 `onProtocolMismatch`（异常 report）；`close` 时 `link.close(`protocol-version ${PROTOCOL_VERSION}`)`。
- `#violation(reason)`：`host.report(new ChordError("invalid", `link to ${name ?? "peer"}: ${reason}`, { reason:"protocol" }))`；`link.close(`protocol error: ${reason}`)`。

消费者侧：
- `hold(binding, again)`：`id = nextId()`；`remotes.set(id, binding)`；`binding.bound(this, id)`；发送 hold（token 用 `binding.contract.description`，principal 有则带，`stateOn` 为 false 时带 `state:false`，again 时带 `again:true`）。
- `release(id)`：从 remotes 删除并发送。
- `wantState(hold, wanted)`：仅在 remotes 有该 hold 时发送 `sub`/`unsub`。
- `call(hold, member, args, root, signal)`：见源码；要点：signal 已 abort → 立即 reject `ChordError("aborted","call aborted")`；abort 监听 → 删除 call、发 `cancel`、reject `{cause: signal.reason}`；`carried = root !== undefined && host.kind === "host"`，carried 时 `host.rootSent(root.rootId, this)`；发送 `{t:"call", id, hold, member, args, ...(root)}`。

接收：
- `#receive(frame)`：mismatched → 丢弃；更新 lastSeen；`isFrame` 失败 → `#violation("malformed frame")`；`ack` → `#onAck` 且**不计数**；否则 `#received++`，try `#dispatch`，catch → `host.report`；最后 `#scheduleAck()`。
- `#dispatch` 分支（逐字 reason）：`hello`；`ping` → 回 `pong`；`pong/bye/ack` 忽略；`release` → `#dropServed(id, true)`；`cancel` → `incoming.get(id)?.abort(new ChordError("aborted","cancelled by caller"))`；`sub/unsub`；`resync`；`shape/gone`；`result`；`state`；`root_end` → 删除 admitted 条目 + `host.foreignRootEnded`；`invalidate` → `host.invalidatePrincipal(pattern)`（catch report）；`prepare/commit/abort` → 仅 upstream，否则 `#violation(`${frame.t} from a child`)`；`retire` → 仅 upstream，`host.allowRetire(codeVersion)`；`vote` → 仅 downstream 且 peer.kind 为 host，否则 `#violation("vote from a non-participant")`，否则 resolve/reject 对应 votes 条目。
- `#onHello`：`(frame.protocol ?? 1) !== 4` → `#protocolMismatch`；首次确定 `#peerAcks = frame.acks === true`，为 false 时清空 unacked；upstream：kind 非 host → `#violation("parent is not a host")`，否则记录 `#parentName ??= frame.name`；downstream：kind/name 不匹配 → `#violation(`hello ${frame.kind} ${frame.name} does not match the accepted ${peer.kind} ${peer.name ?? ""}`)`；peer.kind 为 host → `host.peerHello(this, frame.codeVersion)`。
- `#principalOf`：downstream 恒为本链路 principal；upstream 用 `frame.principal ?? this.principal`。
- `#originOf`：downstream 恒为 `this.originId`；upstream 用 `frame.origin`。
- `#onHold`：重复 id → `#violation(`duplicate hold id ${frame.id}`)`；`at !== host.name`：upstream → 回 `gone` 帧（`unavailable(`${host.name} does not relay`)`）；downstream → 建 RelayRef 并 `host.relay(relay)`（catch → relay.close(error,{releaseDown:true,notify:true})）。否则解析 token：非 remote scope → `invalid("token", `${token.name} is local; only remote types cross links`)`；`resolveHost(...) !== host.name` → `invalid("placement", `${token.name} with this input does not live on ${host.name}`)`；`host.acquire(token, input, {principal}, client ? "client" : "connection")`；非 RefImpl → `unavailable("not a local reference")`；失败统一回 `gone` 帧（client 用 `clientError` 包装 contract_mismatch → upgrade_required）。成功则建立 ServedLocal，注册 `onShape` → `#unit`，`goneListeners.add` → `#dropServed` + 发 gone；ref 已 live 则立刻 `#unit`。
- `#stateMembers(ref, shape)`：`shape.members.filter(name => ref.describe(shape.variant, name)?.kind === "state")`。
- `#unit(served)`：shape 未定义返回；members = wantsState ? stateMembers : []；移除已离开的 feed（unsubscribe + dropState）；`urgent = feeds.size === 0 && !(again && !unitSent)`；urgent → `#sendUnit`；否则 `#queueUnit`。
- `#sendUnit(served)`：删 unit slot；shape 未定义返回；`unitSent = true`；`listed` = members 中尚无 feed 者；逐个 `#attach` 收集 snapshot 与 feed；`send({t:"shape", id, ...(variant 有则带), members, contract, state: snaps.map(m)})`；逐个 send snapshot；再 `#replay`。
- `#attach(served, name, as="snap")`：读 `ref.member(name)` 需有 `events`；建 feed `{unsubscribe:noop, encoder: encoder(), buffer: []}`；订阅 events：第一帧存 `first`（作为 snapshot 值），后续入 buffer 或直接 `#feedEvent`；返回 `{frame: #snapshot(served,name,as,first?.value), feed}`。
- `#snapshot`：`validateSnapshot` 失败 → `{as:"error", error: toWireError}`；成功 → `{t:"state", hold, m, as, ...(value === undefined ? {} : {value})}`。
- `#feedEvent`：feed 已换则丢弃；ops → `sendState({as:"ops", ops: feed.encoder.encode(event.ops)}, {resend})`；其它 → **换新 encoder**，`sendState(#snapshot(...,"reset",event.value), overflow)`。
- `#resend(served,name)`：取消旧 feed；`#attach(..., "reset")`；send 该帧并 replay。
- `#onWantState`：relay → 转发；`wantsState` 不变返回；变 true → `#unit`；变 false → 退订所有 feed、清空、`dropHoldState`。
- `#onResync`：relay → 转发 `resync`；本地无该 feed → 返回；否则 `#resetLater(hold, m, () => #resend(served,m))`。
- `#dropServed(id, release)`：删除；relay → `relay.close(released "relayed hold released", {releaseDown: release, notify:false})`；本地 → 退订 feeds、`dropHoldState`、可选 `ref.release()`。
- `#onCall`：重复 id → `#violation(`duplicate call id ${frame.id}`)`；超过 maxCalls → 回 result error：`ChordError("unavailable", `Too many calls in progress on this connection (${maxCalls}); wait for earlier ones to finish`, { reason:"too_many_calls" })`；未知 hold → result error `gone "unknown hold"`；建 AbortController；root = peer.kind==="host" ? `#admitRoot(frame.root)` : undefined；principal 取 served；本地 → `ref.object.call(ref, member, unpack(args), ctx)`，relay → `relayCall(...)`；root 未定义 → `host.root(run, options)`，否则 `host.joinRoot(root.rootId, root.codeVersion, run, {...options, from:this})`；结果/错误回 result 帧；`finally` 删除 incoming。
- `#admitRoot`：非 downstream/fenced/已有版本 → 原样；root 未被 fencing（`host.hasCodeVersion`）→ 原样；否则按 rootId 缓存为本 host 当前版本。
- `#onHoldReply`：`relayedRefs` 命中 → gone 时 `relay.close(fromWireError, {releaseDown:false, notify:true})`，shape 时 `relay.shape`；`remotes` 命中 → gone 删除并 `binding.gone`，shape 时 `binding.shaped({variant,members,contract}, frame.state)`。
- `#onResult`：relayed → `finish`；本链 calls 删除；error → reject；否则 `unpack(value)`（失败 reject）后 resolve。
- `#onState`：relay → 转发；binding 未命中返回；value 非 Packed → 直接 `binding.state(m, frame)`；Packed → unpack 失败回 `{as:"error", error: toWireError(error)}`，成功用 `{...frame, value}`。
- `#closed(reason)`：`/^protocol-version (\d+)$/` 且版本不同 → `#protocolMismatch(n, false)`；清心跳与 ack timer；清 pending/字节；`error = unavailable(`link to ${name ?? "peer"} closed: ${reason}`)`；释放所有 served（`#dropServed(id, true)`）；abort 所有 incoming；reject 所有 calls、votes、syncWaiters；对所有 relayedRefs `close(error,{releaseDown:false, notify:true})`；清映射；对本链 remotes：upstream → `binding.detached()`，否则 `binding.gone(error)`；`host.connectionClosed(this)`。

辅助函数：
- `relayCall(relay, frame, root, signal, host)`：signal 已 abort → `ChordError("aborted","call cancelled",{cause})`；`Promise.race([relay.ready(), aborted])`；`relay.closed` → `gone "relayed hold closed"`；建 RelayCall（downId = `down.nextId()`）；abort → `call.cancel(abortError(), true)`；`host.rootSent(root.rootId, down)`；发送 call（携带原 `frame.args` 与 root）；finally 移除监听。
- `validateSnapshot(ref, member, value)`：undefined 直接返回；取 `object.currentInstalled()`；view 与 installed 决定用 consumer 还是 provider 的 description；`description.kind === "state"` 时 `assertValid("state", `${ref.address}.${member}`, description.schema, value)`。
- `clientError(error)`：ChordError 且 code 为 `contract_mismatch` → `new ChordError("upgrade_required", error.message, {cause:error})`；否则原样。

Rust：`Connection` 用 `Mutex` 保护的哈希表 + 单线程 actor；`#unacked: VecDeque<usize>`；ping 用 `tokio::time::interval` + `MissedTickBehavior`。

### 2.17 `src/host.ts`（1303 行）— Host 入口、引用获取、路由、reload 协调

#### 选项与工厂

```ts
export interface HostOptions<P = unknown> {
  readonly name?: string;                                   // 默认 "host"
  readonly self: NoInfer<P>;                                // 必需
  readonly principalKey?: (principal: P) => string;         // 默认 canonicalJson
  readonly dev?: boolean;                                   // 默认 false
  readonly onError?: (error: unknown) => void;              // 默认 (e) => console.error("chord:", e)
  readonly taskStopDeadline?: number;                       // 默认 5000
  readonly readyTimeout?: number;                           // 默认 60000
  readonly linger?: { readonly types?: Readonly<Record<string, number>>; readonly cap?: number };
                                                            // types: 类型名 -> 毫秒；cap 默认 4。
                                                            // 未列出的类型不 linger（不是"全部"）
  readonly loader?: FacetLoader;
  readonly kind?: "host" | "client";                        // 默认 "host"
  readonly upstream?: { readonly link: Link } & ConnectOptions<P>;
}
export interface ConnectOptions<P = unknown> extends LinkOptions { readonly principal: P }
export interface Route<P = unknown> {
  authorize(principal: P, hostName: string): boolean | Promise<boolean>;
  resolve(hostName: string): Promise<Link>;
  readonly principal: (hostName: string) => P;
}
export type PrincipalPattern<P> = P extends object ? { readonly [K in keyof P]?: P[K] } : Readonly<Record<string, unknown>>;
export type PrincipalMatch<P = unknown> = ((principal: P) => boolean) | PrincipalPattern<P>;
export interface RootOptions<P = unknown> { readonly principal?: P; readonly signal?: AbortSignal; readonly codeVersion?: number }
export interface LoadOptions { readonly awaitHostObjects?: boolean }   // 默认 true
export function createHost<P = unknown>(options: HostOptions<P>): Host<P>;
```
- `self` 缺失（`!("self" in options)`）→ `TypeError("createHost needs `self`: the principal of the host's own code")`。
- `principalKey` 默认 `canonicalJson`；`#selfKey = principalKey(self)`；`isSelf(p) = p === principal || principalKey(p) === selfKey`。
- `#incarnation = Math.random().toString(36).slice(2, 8)`；`#codeVersionSeq = 1`；初始 code version `{id: 1, services: new Map(), tables: new Map(), roots: new Map(), abort: new AbortController()}`。
- 构造末尾若给了 `upstream` → `connect(options.upstream.link, options.upstream)`。

```ts
export interface FacetApi<P = unknown> {
  readonly serve: Serve<P>;                       // 一个可调用对象：serve(type, impl, options?)
  readonly name: string;                          // host 名
  root<T>(fn: (ctx: Ctx<P>) => T | Promise<T>, options?: RootOptions<P>): Promise<T>;
  registry<V>(registry: Registry<V>): RegistryView<V>;
  readonly signal: AbortSignal;                   // 该 facet run 被替换/移除/host 关闭时 abort
}
export interface FacetDefinition<P = unknown> {
  readonly uses?: readonly AnyContract[];         // 在 reload 时检查 provider
  setup(api: FacetApi<P>): void;                  // 同步
}
export interface Facet<P = unknown> extends FacetDefinition<P> { readonly name: string }
export const defineFacet = <P = unknown>(setup: (api: FacetApi<P>) => void,
                                         options: { readonly uses?: readonly AnyContract[] } = {}): FacetDefinition<P>;
export type FacetLoader = (modules: readonly (string | URL)[]) => Promise<Facet[]>;
export type PluginRef<P = unknown> = Facet<P> | string | URL;
```
- **注意**：`FacetDefinition.setup(api)` 是同步函数，**没有返回值**；`defineFacet(setup, { uses })` 只是 `{ setup, uses }`。
- `isFacet(ref)`：`typeof ref === "object" && !(ref instanceof URL)`。
- `PluginRef` 的字符串/URL 由 loader 载入；loader 缺失且存在模块 → 报错（见下）。

```ts
export interface Host<P = unknown> {
  readonly name: string;
  readonly codeVersion: number;
  readonly previousCodeVersion: number | undefined;
  readonly serve: Serve<P>;
  install(facet: Facet<P>, options?: LoadOptions): Promise<void>;
  load(plugins: readonly PluginRef<P>[], options?: LoadOptions): Promise<void>;
  ready(): Promise<void>;
  get<C extends AnyContract>(type: C, input: InputOf<C>,
      options?: GetOptions<P> & { readonly signal?: AbortSignal }): Promise<Ref<C>>;
  hold<C extends AnyContract>(type: C, input: InputOf<C>, options?: GetOptions<P>): Ref<C>;
  root<T>(fn: (ctx: Ctx<P>) => T | Promise<T>, options?: RootOptions<P>): Promise<T>;
  registry<V>(registry: Registry<V>): RegistryView<V>;
  invalidatePrincipal(match: PrincipalMatch<P>): Promise<void>;
  accept(link: Link, options: AcceptOptions<P>): void;
  connect(link: Link, options: ConnectOptions<P>): void;
  route(prefix: string, route: Route<P>): void;
  reload(plugins: readonly PluginRef<P>[], options?: ReloadOptions): Promise<ReloadReport>;
  close(): Promise<void>;
}
export interface CodeVersionState {
  readonly id: number;
  readonly services: Map<string, Service>;
  tables: ReadonlyMap<string, RegistryTable<unknown>>;
  readonly roots: Map<string, number>;     // rootId -> 该版本下在此 host 运行的调用数
  readonly abort: AbortController;
}
```
- `HostImpl` 额外公开（非 `Host` 接口）：`dev`、`principal`、`inFactory`、`failed`、`objects`、`codeVersions`、`facets`、`hostObjects`、`kind`、`connections`、`upstream`、`bindings`、`remoteModules`、`remoteRemoved`、`current`、`previous`、`pending`、`gating`、`reloading`、`taskStopDeadline`、`readyTimeout`、`isSelf`、`nextCodeVersionId`、`report`、`resolve`、`facetApi`、`facetRunChanged`、`serveInto`、`startHostObject`、`addressOf`、`acquire`、`forget`、`disposed`、`touch`、`root`、`joinRoot`、`rootSent`、`endRoot`、`foreignRootEnded`、`maybeRetire`、`retirable`、`allowRetire`、`watchRetirement`、`announceRetired`、`retire`、`registry`、`buildTables`、`installTables`、`fail`、`get`、`hold`、`invalidatePrincipal`、`accept`、`connect`、`route`、`peerHello`、`reloadEnded`、`whenReloadEnded`、`connectionClosed`、`bindingIdle`、`forgetBinding`、`relay`、`reload`、`onReloadFrame`、`rememberRemoteModules`、`peekNextCodeVersion`、`close`。
- `Serve` 是**可调用对象**（`FacetApi.serve` 与 `Host.serve`）：`serve(type, implementation, options?)` → `Service`。`ServeOptions = ReferencedServeOptions | HostServeOptions`：
  ```ts
  interface ReferencedServeOptions { lifetime?: "referenced"; linger?: number; accepts?: Accepts }
  interface HostServeOptions { lifetime: "host" }   // 另有约束：无 linger、input 接受 {}、仅本地类型
  ```
- `nextCodeVersionId(given?)`：`given !== undefined && given <= current.id` → `Error(`code version ${given} is not newer than ${this.current.id}`)`（**注意 `<=`**，且是普通 `Error`）；然后 `#codeVersionSeq = given ?? #codeVersionSeq + 1`；返回 `#codeVersionSeq`。

#### HostImpl 生命周期

- `serve`（可调用）：`reloading` → `Error("host.serve during a reload; serve from a facet passed to reload()")`（**普通 Error**）；`serveInto(current.services, makeService(type, factory, options, ""))`（facet 名为空串）；`options.lifetime === "host"` → `startHostObject`。
- `install(facet, options = {})`：`reloading` → `Error("host.install during a reload")`；同名 → `Error(`facet ${facet.name} is already installed; use reload()`)`；新建 `AbortController run`；`facet.setup(facetApi(serve, run.signal))`，其中 serve 把 service 加入 `current.services` 并收集到 `added`；setup 抛错 → `run.abort(error)` 后重抛；成功 → `facets.set`、`facetRunChanged(name, run)`；`refs = added.filter(hostLifetime).map(startHostObject)`；`options.awaitHostObjects === false` → `Promise.resolve()`，否则 `#hostObjectsReady(refs)`。
- `load(plugins, options = {})`：`facets = await resolve(plugins)`；`await Promise.all(facets.map(f => install(f, options)))` —— **install 之间没有 await 点**，所以"后面的 facet 提供前面 host object 需要的类型"也能成立。
- `ready()`：`#hostObjectsReady(hostObjects.values())`。
- `#hostObjectsReady(refs)`：`Promise.all([...refs].map(r => r.ready)).then(() => {})`，并 `ready.catch(() => {})`（保证忽略返回值时也不会 unhandled rejection，错误经 `onError` 报告）。
- `resolve(plugins)`：`modules = plugins.filter(ref => !isFacet(ref))`；`modules.length > 0 && loader === undefined` → `Error("loading modules needs createHost({ loader }), e.g. nodeLoader from @earendil-works/chord/node")`；`loaded = modules.length > 0 && loader !== undefined ? await loader(modules) : []`；返回 `plugins.map(ref => isFacet(ref) ? ref : loaded.shift()!)`（**保持原顺序**）。
- `facetApi(serve, signal)`：`{ serve, name: host.name, root: (fn, o) => this.root(fn, o), registry: (r) => this.registry(r), signal }`。
- `facetRunChanged(name, run)`：`replaced = #facetRuns.get(name)`；`run === undefined` → delete，否则 set；`replaced !== run` → `replaced?.abort(ChordError("gone", `facet ${name} was replaced or removed`))`。
- `serveInto(services, service)`：`services.has(service.contract.name)` → `Error(`${service.contract.name} is already served`)`；`placement = contract.description.host` 存在且 `!placedOn(placement, this.name)` → `TypeError(`${name} is declared for ${placementText(placement)}, but ${this.name} serves it`)`；否则 `services.set`。
- `startHostObject(service)`：`ref = acquire(contract, {}, undefined, "host")`；`hostObjects.set(name, ref)`；`ref.ready.catch(report)`；返回 ref。
- `nextCodeVersionId(given?)`：`given !== undefined && given <= current.id` → `Error(`code version ${given} is not newer than ${current.id}`)`；`#codeVersionSeq = given ?? #codeVersionSeq + 1`；返回。
- `currentCodeVersion()`；`hasCodeVersion(cv)` = `codeVersions.has`；`retainedCodeVersions()` 先遍历 `codeVersions`（插入序）再补 `pending`；`pendingCodeVersion()`；`service(typeName)` = `current.services.get(name) ?? pending?.services.get(name)`。
- `#bindingService(typeName, candidate)`：`candidate ? pending?.services.get(name) : undefined`，再 `?? service(name)`。
- `addressOf(type, input, principal, candidate = false)`：`#elsewhere` 命中 → `#remoteKey(type, elsewhere.input, principal)`；否则 `#bindingService(type.name, candidate)`（无 → `unknownType`）→ `#locate(...).address`。
- `#elsewhere(type, input)`：仅 `scope === "remote"` 且有 `description.host`；`normalized = normalize(type.input, input, `${type.name} input`)`；`at = resolveHost(...)`；`at === this.name` → undefined；否则 `{at, input: normalized}`。
- `#locate(service, input, principal)`：`normalized = normalize(schema, input, `${name} input`)`；`identity = canonicalIdentity(schema, normalized)`；host lifetime：`identity !== canonicalIdentity(schema, normalize(schema, {}, `${name} input`))` → `invalid("input", `${name} has lifetime "host": its one object has the empty input`)`，且 `principal = this.principal`；返回 `{ address: `${name}\u0000${identity}\u0000${principalPart}`, normalized, principal }`。
- `#principalPart(principal)`：`isSelf(principal)` → `"host"`；否则 `` `p:${principalKey(principal)}` ``。
- `#remoteKey(type, normalized, principal)`：`` `@${type.name}\u0000${canonicalIdentity(type.input, normalized)}\u0000${principalPart}\u0000${type.fingerprint}` ``。
- `acquire(type, input, options, owner, from?, candidate = false)`（逐条顺序）：
  1. `#elsewhere` → `#acquireRemote(type, at, normalizedInput, options, owner)`；
  2. `#bindingService(type.name, candidate)` 无 → `unknownType(type.name, this.name)`；
  3. `bind(service.contract, service.options.accepts, type)`；
  4. `#locate(service, input, options?.principal ?? this.principal)`；
  5. `placement = service.contract.description.host` 存在且 `resolveHost(...) !== this.name` → `invalid("placement", `${type.name} with this input lives on ${resolved}, not ${this.name}`)`（**永远以 provider 自己的 placement 为准，不看请求说要去哪**）；
  6. `object = objects.get(address)`；`from !== undefined && object !== undefined && (object === from || reaches(object, from))` → `invalid("cycle", `${from.address} -> ${address} would create a ref cycle`)`；
  7. `object === undefined`：`gating?.has(type.name)` → `ChordError("upgrade_conflict", `${type.name} is being upgraded; retry`, { reason:"gate" })`；否则处理 `#dying`（`dying?.status === "disposed"` → 删除条目），`new ObjectImpl(this, {address, typeName, input: normalized, principal, hostLifetime: hostLifetime(service)})`，`objects.set`，`object.start(dying?.status === "disposing" ? dying.dispose(false) : undefined)`（**同地址仍在销毁时要等它清理完**）；
  8. `ref = new RefImpl(object, type, owner)`；`object.addRef(ref)`；返回 ref。
- `forget(object)`：`objects.get(address) === object` → 删除；`object.status === "disposing"` → `#dying.set(address, object)`。
- `disposed(object)`：`#dying.get(address) === object` → 删除。
- `touch(rootId, object)`：把 object 加入 `#touches[rootId]` 集合。
- `root(fn, options = {})`：`rootId = `${this.name}#${this.#incarnation}:${++this.#rootSeq}``；`#local.add(rootId)`；`joinRoot(rootId, options.codeVersion ?? current.id, fn, options)`。
- `joinRoot(rootId, codeVersionId, fn, options = {})`：
  1. `codeVersion = codeVersions.get(codeVersionId)`；无 → `#local.delete(rootId)` + `ChordError("superseded", `code version ${codeVersionId} is not retained by ${this.name}`)`；
  2. `options.from !== undefined && !#local.has(rootId)` → 把 from 加入 `#foreign[rootId]` 集合；
  3. `codeVersion.roots.set(rootId, (get ?? 0) + 1)`；
  4. `ctx = new CtxImpl(this, { signals: options.signal === undefined ? [codeVersion.abort.signal] : [options.signal, codeVersion.abort.signal], rootId, codeVersion: codeVersion.id, principal: options.principal ?? this.principal })`；
  5. `try { return await fn(ctx) } finally { ctx.close(); count = (roots.get(rootId) ?? 1) - 1; count > 0 ? roots.set(rootId,count) : (roots.delete(rootId), !#foreign.has(rootId) && this.endRoot(rootId), this.maybeRetire(), this.#retirementChanged()) }`。
- `rootSent(rootId, connection)`：`#rootLinks[rootId].add(connection)`。
- `endRoot(rootId)`：`#local.delete`；`#foreign.delete`；对 `#touches.get(rootId)` 每个 object `rootEnded(rootId)`；删 touches；对 `#rootLinks.get(rootId)` 每条 connection `send({t:"root_end", rootId})`；删 rootLinks。
- `foreignRootEnded(rootId, from)`：`links = #foreign.get(rootId)`；无、或 `links.delete(from)` 为假、或 `links.size > 0` → 返回；若某个 codeVersion 的 roots 仍有该 rootId（本地还有调用在飞）→ 只 `#foreign.delete(rootId)`（等它 settle 时由 joinRoot 的 finally 收尾）；否则 `endRoot(rootId)`。
- `maybeRetire()`：`retirable()` → `retire()`。
- `retirable()`：`previous === undefined || previous.roots.size > 0` → false；否则 `kind === "client" || upstream === undefined || previous.id <= #retireAllowed`。
- `allowRetire(codeVersion)`：`#retireAllowed = Math.max(#retireAllowed, codeVersion)`；`maybeRetire()`；`#retirementChanged()`。
- `watchRetirement(watcher)`：加入集合，返回移除器。
- `#retirementChanged()`：对 watcher 快照逐个调用。
- `announceRetired(connection)`：`retired = (previous ?? current).id - 1`；`retired > 0` → `send({t:"retire", codeVersion: retired})`。
- `retire()`：无 previous → 返回；`previous = undefined`；`codeVersions.delete(previous.id)`；对每个 object `retireImplementation(object, previous.id)`；对每个 downstream host 链路 `send({t:"retire", codeVersion: previous.id})`；`#retirementChanged()`。
- `retireImplementation(object, codeVersion)`：
  1. `installed = object.implementations.get(codeVersion)`；`implementations.delete(codeVersion)`；`installed === undefined || installed.decls === object.decls` → 返回；
  2. `kept = object.allDecls()`；`keep(pick)` = 所有 kept decls 里该集合的并集；分别求出 refs/resources/derives；
  3. 未被保留的 derives `detach()`；
  4. `closing` = installed.decls 里未被保留的 resources，按 `object.openOrder.indexOf(b) - indexOf(a)` 逆序；
  5. `releasing` = 未被保留的 refs；
  6. `object.refreshSchemas()`；
  7. `runAll([...closing.map(r => () => r.close(object.openOrder)), ...releasing.map(r => () => r.release())], `${object.address}: retiring code version ${codeVersion} failed`).catch(report)`（**不 await**）。
- `registry(registry)`：`table(ctx?)`：`codeVersion = ctx === undefined ? current : codeVersions.get(ctx.codeVersion)`；无 → `ChordError("superseded", `code version ${ctx?.codeVersion} is retired`)`；返回 `codeVersion.tables.get(registry.name) ?? EMPTY_TABLE`（`EMPTY_TABLE` 是共享的空 `Map`）。返回 `{ table, get: (key, ctx) => table(ctx).get(key), subscribe(listener) { 加入 #subscribers[registry.name]；**立即** wrapped(table(), current.id)；返回移除器 } }`。
- `buildTables(entries)`：
  1. `byRegistry: Map<registryName, Map<key, {value, object}>>`；
  2. 对每个 `(object, decls)` 的每条 registration：已有同 key 且 `existing.object !== object` 且 `registry.conflict === "error"` → `invalid("registry_conflict", `${registry.name}: ${key} registered by ${existing.object.address} and ${object.address}`)`；写入（`"last"` 时覆盖）；`#order[registry.name]` 里若该 key 没有序号则记为 `order.size`（**首次注册顺序，跨 reload 保留**）；
  3. 每个 registry 按 order 升序排序后建 `Map<key, value>`（**注意：插入序 = 首次注册序，而不是对象遍历序**）。
- `liveDecls(candidates = false)`：`objects` 中 `status === "live"` 且（`candidates || !deferredRuntime`）的 `[object, object.decls]`。
- `checkRegistrations(object, decls)`：`decls.registrations.size === 0` → 返回；`buildTables([...liveDecls().filter(([other]) => other !== object), [object, decls]])`（**纯校验，不安装**）。
- `registriesChanged(including?)`：`entries = liveDecls().filter(([o]) => o !== including)`；`including` 存在则补 `[including, including.decls]`；`installTables(current, buildTables(entries))`。
- `fail(error)`：`failed ??= error`；`report(error)`。
- `installTables(codeVersion, tables, all = false)`：`before = codeVersion.tables`；`codeVersion.tables = tables`；`codeVersion !== current` → 返回；对每个 `#subscribers` 的 `[name, listeners]`：`next = tables.get(name) ?? EMPTY_TABLE`；`!all && sameTable(before.get(name) ?? EMPTY_TABLE, next)` → 跳过；`enqueue(() => 快照逐个 listener(next, codeVersion.id)，异常 report)`。
- `sameTable(a, b)`：长度不同 → false；`[...a.keys()]` 与 `[...b]` 逐位比较 key 与 value **引用相等**。
- `get(type, input, options)`：`ref = acquire(type, input, options, "external")`；`await awaitReady(ref, options?.signal === undefined ? [] : [options.signal], readyTimeout)`；返回 `ref.proxy`。
- `hold(type, input, options)`：`acquire(type, input, options, "external").proxy`（同步，可能 loading）。
- `invalidatePrincipal(match)`：
  1. `matches = typeof match === "function" ? match : (p) => matchesPattern(match, p)`；
  2. `reason = ChordError("gone", "principal invalidated")`；
  3. 对 `connections` 快照中 `role === "downstream" && matches(connection.principal)` 的链路 `revoke(connection.restamp?.(connection.principal))`（`restamp` 返回 undefined → 链路关闭）；
  4. `match` 不是函数（pattern）→ 对每个 `role === "downstream" && peer.kind === "host"` 的链路 `send({t:"invalidate", pattern: match})`（**只发给 host 子链路，不发给 client**）；
  5. `matching = objects` 中 `!isSelf(principal) && matches(principal)` 的对象；`await disposeHoldersFirst(matching, reason, "invalidatePrincipal: disposal failed")`。
- `matchesPattern(pattern, principal)`：`principal !== null && typeof principal === "object"`，且 `Object.entries(pattern)` 每个 `[key, value]` 都 `deepEqual(principal[key], value)`（**空 pattern `{}` 匹配任何对象 principal**）。
- `accept(link, options)`：`kind = options.kind ?? "client"`；`kind === "host" && options.name === undefined` → `TypeError("accepting a host link needs its name")`；`#addConnection(link, { ...options, role: "downstream", principal: options.principal, peer: { kind, name: kind === "host" ? options.name : undefined }, ...(kind === "client" ? { originId: `${incarnation}:${++originSeq}` } : {}) })`。
- `#addConnection(link, config)`：`new Connection(this, link, config)`；**`connection.open` 才加入 `connections`**（构造时已关闭的链路会立刻关闭 connection，留着会永远路由到死链路）。
- `connect(link, options)`：`!("principal" in options)` → `TypeError("connect needs `principal`: what the parent's own requests act as")`；`upstream?.open` → `upstream.link.close("replaced")`；`#addConnection(..., {role:"upstream", peer:{kind:"host", name:undefined}})`；`upstream = connection`；对每个 binding：`binding.hold === undefined && !binding.ended` → `connection.hold(binding, binding.applied)`。
- `route(prefix, route)`：`typeof route.principal !== "function"` → `` TypeError(`route ${prefix} needs \`principal\`: what the routed hosts act as when they call back`) ``；`#routes.set(prefix, route)`。
- `peerHello(connection, codeVersion)`：
  1. `codeVersion > current.id` → `report(ChordError("invalid", `${connection.name} runs code version ${codeVersion}, ahead of ${current.id}`))` + `link.close("code version ahead of parent")`；
  2. `reloading` → `connection.peerCodeVersion = codeVersion`；`codeVersion < current.id` 时 `connection.fencedCodeVersions.add(codeVersion)`；`#deferredHellos.set(connection, codeVersion)`；返回；
  3. `codeVersion === current.id` → `markSynced(codeVersion)` + `announceRetired(connection)`；返回；
  4. 落后：`peerCodeVersion = codeVersion`；`fencedCodeVersions.add(codeVersion)`；`reconcileChild(this, connection).catch(e => { report(e); link.close("reconciliation failed") })`。
- `reloadEnded()`：`reloading = false`；取走并清空 `#deferredHellos`；对仍 open 的逐个 `peerHello(connection, codeVersion)`；resolve 所有 `#reloadWaiters`；`maybeRetire()`。
- `whenReloadEnded()`：非 reloading → `Promise.resolve()`；否则登记 waiter。
- `connectionClosed(connection)`：`connections.delete`；`#deferredHellos.delete`；对 `#foreign` 快照中 links 含该链路的 root `foreignRootEnded(rootId, connection)`；从所有 `#rootLinks` 集合里删该链路；`upstream === connection` → `upstream = undefined` 且 `#childReloads.parentLost()`。
- `bindingIdle(binding)`：`ms = Object.hasOwn(lingerTypes, binding.contract.name) ? lingerTypes[name] : 0`；`ms <= 0 || binding.ended || !binding.applied || lingerCap <= 0` → `binding.close()` 并返回（**从未拿到 shape 的 binding 没有保留价值**）；否则 `lingerTimer = setTimeout(() => binding.close(), ms)`（`unref()`）；`#lingering.set(binding.key, binding)`；`for (const oldest of #lingering.values()) { if (size <= cap) break; oldest.close() }`（**按插入序淘汰最旧**）。
- `forgetBinding(binding)`：`bindings.get(key) === binding` → 删除；`#lingering.get(key) === binding` → 删除。
- `#childNamed(name)`：`connections` 中 `open && role === "downstream" && peer.kind === "host" && peer.name === name`（**只认配置名，不认 peer 自报的名字**）。
- `#acquireRemote(type, at, input, options, owner)`：
  1. `principal = options?.principal === undefined || isSelf(options.principal) ? undefined : options.principal`（**host 自己的 principal 不转发**）；
  2. `key = #remoteKey(type, input, principal ?? this.principal)`；
  3. `binding = bindings.get(key)`；`fresh = binding === undefined || binding.ended`；fresh → 新建 `RemoteBinding(this, {key, contract: type, at, input, principal})` 并 set；
  4. `ref = new RemoteRef(binding, owner, options?.state !== false)`；
  5. `#lingering.delete(key)`；`binding.addView(ref)`；fresh → `#hold(binding)`；返回 ref。
- `#hold(binding)`：`bindWhenSynced(connectionPromise)` 成功且 `!binding.ended && binding.hold === undefined` → `target.hold(binding, false)`，失败 → `binding.gone(error)`；
  - `upstream !== undefined` → **直接** `upstream.hold(binding, false)`（不等 synced）；
  - 否则 `direct = #childNamed(binding.at)`；有 → `bindWhenSynced(direct.whenSynced().then(() => direct))`；
  - 否则 `route = #routeFor(binding.at)`；无 → `binding.gone(ChordError("unavailable", `no route to ${binding.at}`))`；有 → `bindWhenSynced(#resolve(route, binding.at, binding.principal ?? this.principal))`。
- `#routeFor(hostName)`：最长前缀匹配 `#routes`，无 → undefined。
- `#resolve(route, hostName, principal, still = () => true)`：`refuse = () => ChordError("forbidden", `${hostName} refused for this principal`)`；`revoked = () => ChordError("gone", "principal invalidated while resolving")`；
  1. `await route.authorize(principal, hostName)` 为假 → refuse；`!still()` → revoked；
  2. `link = await route.resolve(hostName)`；`!still()` → revoked；
  3. 再 `authorize`，假 → refuse；`!still()` → revoked；
  4. `connection = connections` 中 `link` 相同的链路；无 → `#addConnection(link, {role:"downstream", principal: route.principal(hostName), peer:{kind:"host", name: hostName}})`；
  5. `await connection.whenSynced()`；`!still()` → revoked；返回 connection。
- `relay(relay)`：`at = relay.frame.at`；`placement = contractFromDescription(relay.frame.token).description.host`；`placement === undefined || resolveHost(token.name, placement, frame.input) !== at` → `invalid("placement", `${token.name} with this input does not live on ${at}`)`；`route = #routeFor(at)`；无 → `#childNamed(at) === undefined ? ChordError("unavailable", `no route to ${at}`) : ChordError("forbidden", `no route authorizes ${at}`)`；`down = await #resolve(route, at, relay.principal, () => relay.current)`；`!relay.current` → 返回；`relay.open(down)`。
- `reload(plugins, options = {})`：`kind !== "client" && upstream !== undefined` → reject `ChordError("unavailable", "reloads are coordinated by the server", { reason: "not_coordinator" })`；`children = connections` 中存在 `role === "downstream" && peer.kind === "host"`；`children || options.remote !== undefined || options.remoteRemove !== undefined` → `runClusterReload`，否则 `runReload`。
- `onReloadFrame(connection, frame)`：`#childReloads.handle(connection, frame)`。
- `rememberRemoteModules(remote, remove)`：对 prefixes 并集去重（`new Set([...keys(remote), ...keys(remove)])`）逐个：`modules = remote?.[prefix] ?? []`；`removed = remove?.[prefix] ?? []`；`remoteModules[prefix] = [...new Set([...已有, ...modules])].filter(m => !removed.includes(m))`；`remoteRemoved[prefix] = [...new Set([...已有, ...removed])].filter(m => !modules.includes(m))`。
- `peekNextCodeVersion()`：`#codeVersionSeq + 1`。
- `close()`：
  1. 对 `#lingering` 快照逐个 `binding.close()`；
  2. `hostObjects.clear()`；
  3. 对 `#facetRuns` 的每个 name `facetRunChanged(name, undefined)`（abort 各 run）；
  4. `reason = ChordError("gone", `host ${this.name} closed`)`；`try { await disposeHoldersFirst([...objects.values()], reason, "host close: disposal failed") } finally { 对每条链路 link.close("host closed") }`。
- `disposeHoldersFirst(objects, reason, message)`（非导出，`invalidatePrincipal`/`close` 共用）：按"持有者先销毁"分波：
  1. `remaining = new Set(objects)`；`held(object)` = 其所有 decls 的 refs 中 `ref.object !== undefined` 的对象集合；
  2. `refs = Map(object → held 集合)`；`errors = []`；
  3. while remaining 非空：`heldByRemaining` = remaining 中对象持有的所有对象；`wave` = remaining 中**不被任何 remaining 对象持有**者；`wave` 为空（跨 host 环）→ 用整个 remaining（**不循环**）；从 remaining 删除 wave；`await Promise.allSettled(wave.map(o => o.dispose(true, reason)))`，收集 rejected；
  4. 1 个错误 → 抛它；多个 → `AggregateError(errors, message)`。
- `unknownType(type, host)`：`ChordError("unavailable", `${type} is not served by ${host}`, { reason: "unknown_type" })`。
- `reaches(from, to)`：显式栈 DFS（`pop` 顺序），`object === to` → true；已见跳过；只沿 `allDecls()` 的 refs 中 `ref.object !== undefined` 的本地对象（**远程引用不参与，跨 host 环靠约定**）。

#### 地址与身份

- **本地对象地址**：`` `${typeName}\u0000${identity}\u0000${principalPart}` ``（`identity = canonicalIdentity(input schema, normalized input)`）。
- **远程绑定键**：`` `@${typeName}\u0000${identity}\u0000${principalPart}\u0000${fingerprint}` ``（无 host 名）。
- `principalPart`：`"host"` 或 `` `p:${principalKey(principal)}` ``。
- **root id**：`` `${host.name}#${incarnation}:${rootSeq}` ``。
- **client origin id**：`` `${incarnation}:${originSeq}` ``。
- `incarnation = Math.random().toString(36).slice(2, 8)`（6 位 base36）。

### 2.18 `src/object.ts`（1932 行）— 对象、方法、状态、资源、任务、派生

#### 服务与 lifetime

```ts
export type ServeOptions = ReferencedServeOptions | HostServeOptions;
export interface ReferencedServeOptions {
  readonly lifetime?: "referenced";      // 默认
  readonly linger?: number;              // 最后引用释放后对象存活毫秒
  readonly accepts?: Accepts;            // 旧版本消费者的适配器
}
export interface HostServeOptions {
  readonly lifetime: "host";
  readonly linger?: undefined;           // 显式设为 undefined 才合法
  readonly accepts?: Accepts;
}
export type ServeOptionsFor<C> = C extends LocalContract
  ? (Record<never, never> extends InputOf<C> ? ServeOptions : ReferencedServeOptions)
  : ReferencedServeOptions;
export type Lifetime = "referenced" | "host";
export type Factory<C extends AnyContract, P = unknown> =
  (input: InputOf<C>, principal: P, self: Self<P>) => ImplementationOf<C, P>;
export interface Service {
  readonly contract: AnyContract;
  readonly factory: AnyFactory;          // (input: unknown, principal: unknown, self: Self) => unknown
  readonly options: ServeOptions;
  readonly facet: string;
}
export const makeService = <C extends AnyContract>(contract: C, factory: Factory<C>, options: ServeOptions, facet: string): Service;
export const hostLifetime = (service: Service): boolean => service.options.lifetime === "host";
```
`checkLifetime` 校验（逐字，全部 `TypeError`）：
- `lifetime = options.lifetime ?? "referenced"`；不是 `"referenced"`/`"host"` → `` `${contract.name}: unknown lifetime ${String(lifetime)}` ``；
- `"host"` 且 `contract.scope !== "local"` → `` `${contract.name}: lifetime "host" needs a local type (remote types are referenced)` ``；
- `"host"` 且 `options.linger !== undefined` → `` `${contract.name}: an object of lifetime "host" does not linger (it lives as long as the host)` ``；
- `"host"` 且 `normalize(contract.input, {}, `${contract.name} input`)` 抛错 → `` `${contract.name}: lifetime "host" needs an input that accepts {} (the object's one address)` ``。

#### 资源（Resource）

```ts
export interface ResourceImpl<T> {
  readonly key: string;
  spec: ResourceDeclaration<T>;                 // 当前代码版本的声明（commit 时 adopt 新声明）
  readonly deps: readonly (RefCore | ResourceImpl)[];
  state: "closed" | "opening" | "open" | "unavailable";
  get value(): T;
  get exclusive(): boolean;                     // spec.exclusive === true
  open(signal: AbortSignal, order: ResourceImpl[]): Promise<void>;
  close(order: ResourceImpl[]): Promise<void>;
  markUnavailable(): void;
  adopt(spec: ResourceDeclaration<T>): void;
}
```
状态机 `closed → opening → open`，任一阶段可 `unavailable`（reload 回滚中 close/reopen 失败 → 值永久丢失）。
- `value`：`unavailable` → `ChordError("unavailable", `resource ${key} could not be restored`)`；不是 `open` → `notReady(`resource ${key}`)`；否则值。
- `open(signal, order)`：
  1. 状态不是 `closed` → 直接返回（幂等，opening 时也返回 —— 去重靠 `ObjectImpl.open` 的 `opening` Map）；
  2. `state = "opening"`；`await spec.open({ signal })`；抛错 → `state = "closed"` 并原样抛；
  3. 打开后 `signal.aborted` → `state = "closed"`，`await spec.close?.(value)`，抛 `ChordError("aborted", `resource ${key} opened after abort`)`（**注意 code 是 aborted，不是 gone**）；
  4. `#value = value`；`state = "open"`；`order.push(this)`（`openOrder` 记录打开顺序，unwind 逆序 close）。
- `close(order)`：非 `open` → 返回；`state = "closed"`；从 `order` 中 splice 掉自己；清 `#value`；`await spec.close?.(value)`。
- `markUnavailable()`：`state = "unavailable"`；`#value = undefined`。
- `adopt(spec)`：替换 `spec`（旧声明连同其闭包代码不再被引用）。

`ResourceFacade`（`self.resource()` 返回的东西，`facades: WeakMap<object, ResourceImpl>` 反查）：
- `key` 透传；
- `value`：`host.inFactory > 0` → `notReady(`resource ${key} (inside a factory)`)`（prepare 重跑不得碰旧代码正在用的本地资源）；否则透传。

`resourceOf(value)`：`instanceof ResourceImpl` 或 `facades.get(value)`，否则 undefined。

#### 声明容器 `Decls` 与 `Run`

```ts
export class Decls {
  readonly cells = new Map<string, Cell>();
  readonly cellSchemas = new Map<string, TSchema>();
  readonly cellVersions = new Map<string, number>();
  readonly cellMigrations = new Map<string, (old: unknown, from: number) => unknown>();
  readonly refs = new Map<string, RefCore>();          // 按 address
  readonly resources = new Map<string, ResourceImpl>();
  readonly adopted = new Map<ResourceImpl, ResourceDeclaration>();   // 保留的旧资源 + 本次声明（commit 时 adopt）
  readonly listeners = new Map<string, ListenerDecl>();
  readonly tasks = new Map<string, TaskFn>();          // 键是位置 `#0`、`#1`…（task 没有名字）
  readonly derives = new Map<string, Derived>();       // 键是位置 `#0`、`#1`…（derive 没有名字，每次 run 都是新的）
  readonly registrations = new Map<string, Registration>();
}
export interface Run { implementation; decls; created: { refs: RefCore[]; resources: ResourceImpl[]; derives: Derived[]; inits: [Cell, InitFn][] } }
export interface Installed { codeVersion: number; service: Service; implementation: Record<string, unknown>; decls: Decls }
```
`isStateInit(value)`：非 null 对象且 `.init` 是函数。
`StateOptions<T> = { version?: number; migrate?: (old: unknown, from: number) => T }`；`StateInit<T,P> = { init: (ctx: Ctx<P>) => T | Promise<T> }`。
`memberSchemas(service, implementation)`（导出）：仅 `contract.scope === "remote"`；取 `description.variants[variantOf(implementation) ?? ""]`，对每个 `kind === "state"` 且 `isSource(impl[name])` 的成员收集 `description.schema`。

#### `SelfImpl`（factory 的 `self`）

```ts
class SelfImpl implements Self {
  state(name, schema, initial, options?): Cell
  get(type, input, options?): Ref
  resource(name, declaration): Resource
  on(name, source, handler) / on(name, source, select, handler): void
  task(fn): void
  derive(sources, fn): Readable
  expose(resource): Exposed
  register(registry, entries): void
}
```
- `#unique(map, kind, name)`：`name` 非 string 或空串 → `TypeError(`${kind} needs a name`)`；已存在 → `invalid("duplicate", `${typeName}: duplicate ${kind} name ${name}`)`。
- `state(name, schema, initial, options = {})`：
  1. `#unique(decls.cells, "state", name)`；
  2. `version = options.version ?? 1`；不是正整数 → `TypeError(`state ${name}: version must be a positive integer`)`；
  3. `init = isStateInit(initial) ? initial.init : undefined`（JSON 值不可能含函数，所以带 `init` 函数的对象就是 initializer）；
  4. `cell = previous?.cells.get(name)`；无 → 新建 `Cell(name, schema, init === undefined ? { value: initial } : undefined, object.guard, { dev, report })`；有 init 则 push 到 `created.inits`；
  5. 写入 `decls.cells / cellSchemas / cellVersions`；`options.migrate` → `decls.cellMigrations`；返回 cell（旧 cell 原样返回，`initial` 被忽略）。
- `get(type, input, options?)`（"candidate" 指 `previous !== undefined`，即 prepare 重跑）：
  1. `address = host.addressOf(type, input, options?.principal ?? host.principal, candidate)`；
  2. 本 run 已声明同 address：fingerprint 不同 → `invalid("duplicate", `${typeName}: ${type.name} held twice with different contracts`)`；否则返回既有 proxy；
  3. previous run 的 ref 可复用（fingerprint 相同且 status 不是 gone/released）→ 记入本 run 并返回；
  4. 否则 `host.acquire(type, input, options, "object", object, candidate)`，记入 `decls.refs` 与 `created.refs`。
- `resource(name, declaration)`：
  1. `#unique(decls.resources, "resource", name)`；`declaration.version` 非整数 → `TypeError(`resource ${name}: version must be an integer`)`；
  2. `deps` 逐项 `refOf(dep) ?? resourceOf(dep)`，未解析 → `TypeError(`resource ${name}: deps must be references or resources of this object`)`；
  3. 复用条件（全部成立才保留旧实例）：`existing.spec.version === declaration.version`、`existing.exclusive === (declaration.exclusive === true)`、deps 长度与逐项相同；
  4. 保留 → `decls.adopted.set(existing, declaration)`；新建 → push 到 `created.resources`；
  5. 返回 `new ResourceFacade(resource, host)`。
- `on(name, source, ...rest)`：`#unique(decls.listeners, "listener", name)`；`rest.length === 1` → `[select, fn] = [undefined, rest[0]]`，否则 `[select, fn] = [rest[0], rest[1]]`；`fn` 非函数 → `TypeError(`listener ${name}: the handler must be a function`)`；`select` 非 undefined 且非函数 → `TypeError(`listener ${name}: select must be a function`)`。
- `task(fn)`：非函数 → `TypeError("a task must be a function")`；键 `#${decls.tasks.size}`。
- `derive(sources, fn)`：非函数 → `TypeError("a derivation needs a function")`；键 `#${decls.derives.size}`；`new Derived(key, sources, fn, { dev, report })` 同时进 `created.derives` 与 `decls.derives`。
- `expose(resource)`：`resourceOf(resource)` 必须属于本 run 已声明的资源（`[...decls.resources.values()].includes(resolved)`），否则 `TypeError("expose takes a resource declared by this object")`；返回 `new ExposedImpl(resolved)`。
- `register(registry, entries)`：非 host lifetime → `invalid("not_host_lifetime", `${typeName}: only objects of lifetime "host" may register`)`；逐个 `id = `${registry.name}\u0000${key}`` → `#unique(decls.registrations, "register", id)`，存入 `{ registry, key, value }`。

#### `ObjectImpl`（对象生命周期）

```ts
export class ObjectImpl {
  readonly host: HostCore; readonly address: string; readonly typeName: string;
  readonly input: unknown; readonly principal: unknown; readonly hostLifetime: boolean;
  status: ObjectStatus = "activating";              // "activating"|"live"|"failed"|"disposing"|"disposed"
  ready: Promise<void> = Promise.resolve();
  service: Service | undefined;
  minCodeVersion = 0;                                // 低于此版本的 call 报 upgrade_conflict
  decls = new Decls();
  readonly pending = new Set<Decls>();               // 尚未 install 的 factory run 的声明（activation / reload candidate）
  readonly implementations = new Map<number, Installed>();  // 按 codeVersion
  readonly refs = new Set<RefImpl>();
  readonly leases = new Map<string, number>();       // rootId → codeVersion
  readonly openOrder: ResourceImpl[] = [];
  readonly callAbort = new AbortController();
  readonly activationAbort = new AbortController();
  gate: Gate | undefined;
  deferredRuntime = false;
  start(after: Promise<void> | undefined): void;
  run(service: Service, previous: Decls | undefined): Run;
  open(resources, refs, signal): Promise<void>;
  waitHydrated(run: Run): Promise<void>;
  withinReadyTimeout<T>(work, waiting: () => readonly string[]): Promise<T>;
  initCells(run, signal): Promise<void>;
  unwind(run: Run): Promise<void>;
  currentInstalled(): Installed | undefined;
  refreshSchemas(): void;
  allDecls(): Set<Decls>;
  stageListeners(decls: Decls): StagedListeners;
  startRuntime(): void; startTasks(): void; stopListeners(): void; stopTasks(): void;
  openGate(): void; admit(ctx: Ctx): void; lease(ctx: Ctx): void;
  guard: (ctx: Ctx) => void;                          // 箭头函数属性（作为 Cell 的写入门卫传入）
  call(ref: RefImpl, name: string, args: unknown, ctx: CtxImpl): Promise<unknown>;
  rootEnded(rootId: string): void;
  addRef(ref: RefImpl): void; release(ref: RefImpl): void;
  dispose(forced: boolean, reason?: unknown): Promise<void>;
}
```
- `start(after)`：`ready = #activate(after).then(成功回调, 失败回调)`，并 `ready.catch(noop)`。
  - 成功：对每个 ref `live()`；`#maybeIdle()`。
  - 失败：仍 `activating` → `status = "failed"`；`host.forget(this)`；`error = asChordError(cause, `activating ${address}`)`（ChordError 保留 code，其它变 `unavailable`）；对每个 ref `gone(error)`；**重新抛出**。
- `#activate(after)`（单事务）：
  1. `await after?.catch(noop)`；`signal = activationAbort.signal`；`abandoned = () => new ChordError("aborted", `${address} abandoned`)`；已 abort → 抛；
  2. `service = host.service(typeName)`；无 → `ChordError("unavailable", `${typeName} is not served`, { reason: "unknown_type" })`；
  3. `run = this.run(service, undefined)`；
  4. `await open(run.created.resources, run.created.refs, signal)` → abort 检查 → `await waitHydrated(run)` → abort 检查 → `await initCells(run, signal)` → abort 检查；
  5. `host.checkRegistrations(this, run.decls)`；`this.decls = run.decls`；
  6. 对所有 `host.retainedCodeVersions()`：若某版本的 `services.get(typeName) === service`，把本 run 的 implementation/decls 也登记到该版本（旧版本消费者看到同一实现）；
  7. 若 `implementations` 既没有当前版本、也没有 pending 版本 → `ChordError("superseded", `${address}: code changed during activation`)`；
  8. 每个 `run.created.derives` → `attach()`；`refreshSchemas()`；
  9. `deferredRuntime = !implementations.has(host.currentCodeVersion())`（只被 reload 的 pending 版本服务：commit 前不得跑 listener/task/registration）；非 deferred：registrations 非空 → `host.registriesChanged(this)`；`startRuntime()`；
  10. 再检查 `signal.aborted`（listener 启动时可能同步释放了最后一个引用）→ 抛 abandoned；`status = "live"`。
  - catch：`callAbort.abort(error)`；`stopListeners()`；`stopTasks()`；记录 `registered`；`implementations.clear()`；`decls = new Decls()`；若 registered → `host.registriesChanged()`；`await Promise.allSettled([...#invocations, ...#stopped])`；`unwind(run)` 的失败走 `host.report`；**抛出**。
  - finally：`pending.delete(run.decls)`。
- `run(service, previous)`：同步跑 factory。
  1. `self = new SelfImpl(this, previous)`；`discard(run)` = 从 pending 删除 + `unwind`（失败 report）；
  2. `pending.add(self.run(undefined).decls)`；
  3. `host.inFactory++`；`service.factory(input, principal, self)`；抛错 → `inFactory--`、`discard(...)`、抛出；`inFactory--`；
  4. `run = self.run(implementation)`；factory 返回 Promise → `invalid("implementation", `${typeName}: factories must be synchronous`)`；`validateImplementation(service, run.implementation)`；失败 → `discard(run)` 抛出。
- `open(resources, refs, signal)`：
  - `opening: Map<ResourceImpl, Promise<void>>` 逐资源去重；`openOne(resource)` 先 `Promise.all(resource.deps.map(dep => !(dep instanceof ResourceImpl) ? dep.ready : resources.includes(dep) ? openOne(dep) : undefined))`，再 `resource.open(signal, openOrder)`；
  - 同时 `withinReadyTimeout(Promise.all(refs.map(r => r.ready)), () => refs.filter(r => r.status === "loading").map(r => r.address))`；
  - `Promise.allSettled([...resources.map(openOne), ready])`，取**第一个 rejected** 的 reason 抛出（所有先 settle，避免悬挂 open）。
- `waitHydrated(run)`：`exposed = Object.entries(run.implementation ?? {})`；`pending = unhydrated([...run.created.derives, ...exposed 的值])`；空 → 返回；否则 `withinReadyTimeout(Promise.all(pending.map(h => h.whenHydrated())), () => [...未 hydrate 的 `${address}.${name}`, ...`derive ${derived.key}`])`。
- `withinReadyTimeout(work, waiting)`：`ms = host.readyTimeout`；非有限 → 直接 `work`；否则 `setTimeout(ms)` 后 reject：
  `` ChordError("unavailable", `${address} was not ready after ${ms} ms${list.length === 0 ? "" : `, waiting on ${list.join(", ")}`} (a reference cycle across hosts?)`, { reason: "ready_timeout" }) ``；timer `unref()`；`Promise.race([work, timeout])`，`finally` `clearTimeout`。
- `initCells(run, signal)`：无 init → 返回；并行 `host.root((ctx) => init(ctx), { signal })`，**未 abort 才** `cell.initialize(value)`；`allSettled` 后抛第一个失败。
- `unwind(run)`：detach 所有 derives；资源按 `openOrder` **逆序**排序（`indexOf(b) - indexOf(a)`）；`runAll([...close, ...ref.release], `${address}: cleanup failed`)`。
- `currentInstalled()`：`implementations.get(host.currentCodeVersion()) ?? [...values()].at(-1)`。
- `refreshSchemas()`：汇总所有 installed 的 `memberSchemas`，把同一 source 的 schema 列表写回所有 `allDecls()` 中 cells/derives 的 `schemas`。
- `allDecls()`：`{this.decls, ...this.pending}` + 每个 installed 的 decls。
- `stageListeners(decls)`：见 §2.18「listener 与 task」。
- `guard(ctx)`（顺序逐字）：
  1. `host.inFactory > 0` → `invalid("read_only", "cells cannot be written while a factory runs")`；
  2. `ctx instanceof CtxImpl` → `ctx.check(host)`；
  3. `ctx.signal.aborted` → `ChordError("aborted", "write with an aborted ctx")`；
  4. `status === "disposed"` → `ChordError("gone", `${address} is disposed`)`；
  5. `ctx.rootId.startsWith("task:")` → **直接放行**（task 的写不经过 admit）；
  6. 否则 `admit(ctx)`。
- `call(ref, name, args, ctx)`（逐字顺序）：
  1. `ctx.check(host)`；ref released/gone → `ref.terminalError()`；loading → `await raceSignal(ref.ready, ctx.signal)`；
  2. ref 或 object 非 live → ref live 时 `ChordError("gone", `${address} is ${this.status}`)`，否则 `ref.terminalError()`；
  3. `admit(ctx)`；
  4. `installed = implementations.get(ctx.codeVersion)`；无 → `ChordError("superseded", `${address} has no implementation for code version ${ctx.codeVersion}`)`；
  5. `view = ref.viewOf(installed)`；`variant = variantOf(view)`；`wanted = ref.describe(contract.scope === "remote" ? variant : LOCAL_VARIANT, name)`；`fn = view[name]`；
  6. `typeof fn !== "function" || (scope === "remote" && wanted?.kind !== "method")` → `ChordError("not_in_variant", `${address}: ${name} is not a method of variant ${variant ?? "(local)"}`)`；
  7. `direct = view === installed.implementation`；`schema = direct ? service.contract.description.variants[variant ?? LOCAL_VARIANT]?.[name] : wanted`；`method = schema?.kind === "method" ? schema : undefined`；`validated = scope === "remote" ? method : undefined`；
  8. `takesArgs = method === undefined || method.args !== undefined`；`returns = method === undefined || method.result !== undefined`；
  9. 参数（仅 `takesArgs`）：`own = scope === "remote" && wanted?.kind === "method" ? wanted.args : undefined`；`sent = own === undefined ? args : Value.Clean(own, Value.Clone(args))`；`input = validated?.args ? Value.Clean(validated.args, Value.Clone(sent)) : sent`；`validated?.args` 时 `assertValid("args", `${address}.${name} args`, validated.args, input)`；
  10. `lease(ctx)`；`child = ctx.child(this)`；`#invoke(() => takesArgs ? fn(input, child) : fn(child))`；`finally` `child.close()`；
  11. `!returns` → 返回 `undefined`（忽略 handler 的返回值）；否则 `validated?.result` → `assertValid("result", `${address}.${name} result`, validated.result, result)`。
- `#invoke(fn)`：`(async () => fn())()` 记入 `#invocations`；settle 后删除，空 → `#maybeIdle()`。
- `rootEnded(rootId)`：删 lease；若 gate 的 `grandfathered` 删掉了且变空 → `gate.drained()`。
- `addRef(ref)`：加入 `refs`；清 linger timer。
- `release(ref)`：`refs.delete(ref)` 成功 → `#maybeIdle()`。
- `#maybeIdle()`：`refs.size > 0 || #invocations.size > 0 || #linger !== undefined` → 返回；`activating` → 后台 `dispose(false)`；非 `live` → 返回；`linger = service.options.linger ?? 0`；`<= 0` → 后台 dispose；否则 `setTimeout(linger)`（`unref()`），到期后仍 `refs.size === 0 && #invocations.size === 0 && status === "live"` 才 dispose。
- `dispose(forced, reason?)`：`#disposal ??= #dispose(...)`（幂等，返回同一 promise）。
- `#dispose(forced, reason)`：
  1. `wasActivating = status === "activating"`；`status = "disposing"`；`host.forget(this)`；清 linger；
  2. `error = reason ?? ChordError("gone", `${address} was disposed`)`；
  3. forced → 所有 ref `gone(error)`；`callAbort.abort(error)`；
  4. `activationAbort.abort(error)`；wasActivating 时 `await ready.catch(noop)`；再对所有 ref `gone(error)`（覆盖激活中新增的 ref）；
  5. `decls = allDecls()`；`stopTasks()`；`stopListeners()`；`await Promise.allSettled([...#invocations])`；
  6. `registered = [...decls].some(s => s.registrations.size > 0) && !deferredRuntime`；`implementations.clear()`；`decls = new Decls()`；registered → `host.registriesChanged()`；
  7. 收集各 decls 的 refs、detach derives；`runAll([() => Promise.allSettled([...#stopped]), ...openOrder 逆序 close, ...refs release], `${address}: disposal failed`)`；
  8. finally：`status = "disposed"`；`host.disposed(this)`。
- `validateImplementation(service, implementation)`（逐字，全部 `invalid("implementation", `${contract.name}: ${message}`)`）：
  - 非对象 → `factory must return an object`；
  - remote 作用域：`variant` 非 string 或 `description.variants[variant] === undefined` → `unknown variant ${String(variant)}`；任何非 `variant` 且不在该 variant 描述里的键 → `${key} is not in variant ${String(variant)}`；
  - member 检查（local 用 `LOCAL_VARIANT`）：`method` 且非函数 → `${name} must be a method`；`state` 且既非 `isSource` 也非 `isReadableLike` → `${name} must be a cell, a derivation or a held object's state member`；`opaque` 且 `undefined` → `${name} is missing`。
- `isReadableLike(value)`：非 null 对象，`events` 与 `subscribe` 都是函数。

#### listener 与 task 运行时

```ts
export interface StagedListeners { activate(): void; discard(): void }
interface Gate { readonly grandfathered: Set<string>; readonly queue: { key: string; event: unknown }[]; drained: () => void }
```
- `sameSelect(a, b)`：`a === b`，或两者都有且 `String(a) === String(b)`（内联箭头每次 run 都是新对象，源码文本相同即视为同一 select；依赖闭包值的 select 不会被识别 —— 已知局限）。
- `#origin(decl)`：`resourceOf(decl.source) ?? decl.source`（订阅归属按资源本身而非 facade）。
- `#listenSource(key, decl)`：`value = resource === undefined ? decl.source : resource.value`；`source = select === undefined ? value : select(value)`；`typeof source?.subscribe !== "function"` → `invalid("implementation", `listener ${key}: source has no subscribe()`)`。
- `stageListeners(decls)`：
  - 逐 listener：同 key 且 `origin` 与 `select` 都相同 → `kept`（不重新订阅、不调用 `select`，只换 handler）；
  - 新建：`entry = { origin, select, unsubscribe: noop, buffer: [] }`；
    - 源是 state（`isSource(source) || isReadableLike(source)`）→ `buffer = undefined`，`entry.subscribe = () => entry.unsubscribe = source.subscribe(e => this.#deliver(key, e))`（**activate 时才订阅**，拿到的是 commit 时的值，而非候选版本的值）；
    - 其它源 → 立即 `subscribe`，事件在 `buffer` 里攒着，activate 后回放；
  - 任一订阅抛错 → 对所有已建 entry `unsubscribe()` 后抛出（不留半订阅）；
  - `activate()`：退订所有未 keep 的旧订阅并从 `#subscriptions` 删除；把 fresh 写进 `#subscriptions`；`entry.subscribe?.()`；回放 buffer（并把 `buffer` 置 undefined 后续直达）；
  - `discard()`：仅退订 fresh。
- `startRuntime()`：`stageListeners(this.decls).activate()`；`startTasks()`。
- `startTasks()`：
  - `run = ++#taskRuns`；`start()` = 对 `decls.tasks` 每个 key `#startTask(key, host.currentCodeVersion())`；
  - `#stopped` 为空 → **同步** start；否则 `Promise.race([Promise.allSettled([...#stopped]).then(() => false), new Promise(r => setTimeout(() => r(true), host.taskStopDeadline))])`；
  - race 后 `clearTimeout`；若 `run !== #taskRuns || status !== "live"` → 丢弃（后来的 start/stop 或离 live 取消延迟启动）；
  - 超时未停（`missed`）→ `host.report(ChordError("aborted", `${address}: aborted tasks still running after ${deadline} ms; starting their replacements anyway`))`，然后照常 start。
- `#startTask(key, codeVersion)`：取 `decls.tasks.get(key)`；`controller = new AbortController()`；`ctx = new CtxImpl(host, { signals: [controller.signal, callAbort.signal], rootId: `task:${address}:${key}`, codeVersion, principal: host.principal, holder: true })`；`done` 内 `await fn(ctx)`，抛错且 `!ctx.signal.aborted` → `host.report`，`finally` `ctx.close()` 并按 controller 身份从 `#tasks` 删除。
- `stopTasks()`：`#taskRuns++`；对每个 task `controller.abort(ChordError("aborted", "task stopped"))`，把 `done` 加入 `#stopped` 并在完成时移除；清空 `#tasks`。
- `stopListeners()`：退订全部并清空 `#subscriptions`。
- `#deliver(key, event)`：
  - `enqueue(() => {...})`（发布顺序）；
  - `gate !== undefined` → 直接 `gate.queue.push({ key, event })` 并返回（gate 期间事件排队，openGate 时用**当前** listeners 重放）；
  - `decl = decls.listeners.get(key)`；无或 status 不是 `live`/`activating` → 返回；
  - `host.root((root) => { this.lease(root); const child = root.child(this); return this.#invoke(async () => { try { await decl.fn(event, child) } finally { child.close() } }) })`；
  - `invocation.catch(host.report)`。
- `openGate()`：取 gate → 置 undefined → 对队列里每个 `#deliver(key, event)`（重新走 enqueue，因此按新顺序发布）。

#### 调用准入与引用计数

- `admit(ctx)`（逐字顺序与消息）：
  1. `host.failed !== undefined` → `ChordError("unavailable", `${host.name} failed and admits no work`, { reason: "host_failed" })`；
  2. `!host.hasCodeVersion(ctx.codeVersion)` → `ChordError("superseded", `code version ${ctx.codeVersion} is retired`)`；
  3. `ctx.codeVersion < minCodeVersion` → `ChordError("upgrade_conflict", `${address} was upgraded in code version ${minCodeVersion}`)`；
  4. `gate !== undefined && !gate.grandfathered.has(ctx.rootId)` → `ChordError("upgrade_conflict", `${address} is being upgraded`)`。
- `lease(ctx)`：`leases.has(rootId)` → 返回；否则记录 `rootId → codeVersion` 并 `host.touch(rootId, this)`。

#### Ctx（`CtxImpl`、`linkedSignal`、`awaitReady`、`raceSignal`）

```ts
export class CtxImpl implements Ctx {
  readonly host: HostCore;
  readonly signal: AbortSignal;
  readonly rootId: string;
  readonly codeVersion: number;
  readonly principal: unknown;
  readonly holder: boolean;         // code holder（task）：每次调用开新 root
  get closed(): boolean;
  check(host: HostCore | RemoteHost): void;
  constructor(host, { signals, rootId, codeVersion, principal, holder? });
  get<C>(type, input, options?): Promise<Ref<C>>;
  child(object: ObjectImpl): CtxImpl;
  close(): void;
}
export function linkedSignal(sources: readonly AbortSignal[]): { readonly signal: AbortSignal; release(): void };
export async function awaitReady(ref: RefCore, signals: readonly AbortSignal[], ms: number): Promise<void>;
export async function raceSignal<T>(promise: Promise<T>, signal: AbortSignal): Promise<T>;
```
- `check(host)`：
  1. `this.host !== host` → `invalid("foreign_ctx", "ctx belongs to another host")`；
  2. `#closed` → `ChordError("aborted","ctx expired: its call already settled",{reason:"expired"})`；
  3. `signal.aborted`：`reason` 是 `ChordError` 且 code `superseded` → `new ChordError("superseded", reason.message, {cause: reason})`；否则 `ChordError("aborted","ctx aborted",{cause: reason})`。
- 构造：`linkedSignal(fields.signals)` → `signal` 与 `release`。
- `get(...)`：`check(host)`；`host.acquire(type, input, {principal: this.principal, ...options}, "call")`；`#refs.push(ref)`；`await awaitReady(ref, [this.signal, this.#closing.signal], host.readyTimeout)`；返回 `ref.proxy`。
- `child(object)`：新 CtxImpl，`signals: [this.signal, object.callAbort.signal]`，同 rootId/codeVersion，`principal = host.principal`（**对象内部以 host principal 行事**）。
- `close()`：`#closed = true`；`#closing.abort(ChordError("aborted","ctx expired: its call already settled",{reason:"expired"}))`；`#release()`；对 `#refs` 逐个 `release()`。
- `linkedSignal(sources)`（**导出**，但在 `chord` 入口面之外——只从 `object.ts` 导出，`index.ts` 未 re-export）：**不是** `AbortSignal.any`（注释：避免 Node 的依赖信号泄漏）。构造 `controller` 与 `detach: (()=>void)[]`；`release()` 逐个执行并清空 `detach`。遍历 sources：**第一个已 abort 的** → `release()`、`controller.abort(source.reason)`、`break`；否则注册 `forward = () => { release(); controller.abort(source.reason) }`（`{once:true}`）并 push 对应的 remove。返回 `{ signal: controller.signal, release }`。注意 `forward` 只保留**触发它的那个** source 的 reason，且触发后其余监听全部移除。
- `awaitReady(ref, signals, ms)`：`ms` 有限时 `setTimeout(ms)` 触发 `timeout.abort(ChordError("unavailable", `${ref.address} was not ready after ${ms} ms`, {reason:"ready_timeout"}))`，timer `unref()`；`linkedSignal([...signals, timeout.signal])`；`await raceSignal(ref.ready, linked.signal)`；失败 → `ref.release()`，若 `linked.signal.reason` 是 `ready_timeout` 的 ChordError 则抛它，否则抛原错误；`finally` 清 timer + `linked.release()`。
- `raceSignal(promise, signal)`：已 abort → `ChordError("aborted","call aborted",{cause: reason})`；否则 race 一个 abort promise（同消息），`finally` 移除监听。

#### Ref 与成员可读性

`RefCore`（导出接口）：`address`、`contract`、`status`、`ready: Promise<void>`、`proxy: object`、`object: ObjectImpl | undefined`、`release()`。
`RefOwner = "object" | "call" | "host" | "external" | "connection" | "client"`。
`REF_BASE`（导出，代理保留键，逐字 9 个）：`contract`、`status`、`ready`、`variant`、`shape`、`error`、`onShape`、`onStatus`、`release`。
`notifyStatus(listeners, status, error, report)`：空集返回；否则取快照 `enqueue(() => 逐个调用，异常 report)`。
`refsByProxy: WeakMap<object, RefCore>`（导出）；`refOf(value)`（导出）。

`MemberReadable`（非导出，`Readable`）：
- `value`：
  1. `ref.status` 为 `released`/`gone` → `ref.terminalError()`；
  2. `status !== "live"` → `notReady(`${ref.address}.${name}`)`；
  3. `currentView()` undefined → 同上；
  4. `#current()` undefined → `ChordError("not_in_variant", `${ref.address}: ${name} is not a state member of variant ${variantOf(view) ?? "(local)"}`)`；
  5. 返回 `source.value`。
- `[HYDRATION]`：live → `hydrationOf(#current())`；否则 `{hydrated:false, whenHydrated: async () => { await ref.ready; await hydrationOf(#current())?.whenHydrated(); }}`。
- `subscribe(l)`：加入；`#readable()` 存在 → 记录 `#seen` 并立刻回调；`bind()`；返回移除器。
- `events(l)`：加入；可读 → 立刻 `{type:"snap", value}` 并记录 `#seen`；否则进 `#unseen`；`bind()`；返回移除器（同时从 `#unseen` 删）。
- `bind()`：`source = (values.size + events.size > 0) ? #current() : undefined`；与 `#source` 相同 → 返回；否则退订旧的、订阅新的；**首个事件**转成 `{type:"reset", value}`（除非：首个事件、`#unseen` 为空、且值与 `#seen.value` 相同 → 丢弃）；之后的事件原样；投递统一 `enqueue`，且投递前重新检查监听者仍在集合中；`#unseen` 中的监听者只收到 `{type:"snap", value}`。
- `stop()`：清三个集合 + `bind()`（退订）。
- `#current()`：`currentView()` 中 `inVariant(view, name)` 的成员且是 `isSource` 或 `isReadableLike` → 返回它；否则 undefined。
- `#readable()`：`#current()` 存在且 `hydrationOf(source)?.hydrated !== false`。
- `#safe(fn)`：异常 → `ref.object.host.report`。

`RefImpl`（导出，`RefCore` 实现）：
- 公开字段：`object`、`contract`、`owner`、`address`、`proxy`、`ready`、`status`、`error`、`shapeListeners`、`goneListeners`、`readables: Map<string, MemberReadable>`、`bindings: WeakMap<Service, Adapter|undefined>`、`views: WeakMap<impl, view>`、`callers: Map<string, fn>`；私有 `#statusListeners`。
- 构造：`status = object.status === "live" ? "live" : "loading"`；`ready = object.ready.then(() => { if (status === "gone") throw error; if (status === "released") throw released })` 且 `ready.catch(noop)`；`proxy = new Proxy(this, { get, has })`：`get` 对 `REF_BASE` 属性返回值（函数绑定到 target），其它字符串（`"then"` 除外）返回 `member(property)`，非字符串返回 undefined；`has` 对 `REF_BASE` 或 `member(name) !== undefined` 为真；`refsByProxy.set(proxy, this)`。
- `variant`：`currentView()` 的 `variantOf`。
- `shape`：仅 live 时，`{ variant, members: Object.keys(view).filter(k => k !== "variant"), contract: installed.service.contract.fingerprint }`。
- `onShape` / `onStatus`（返回移除器）。
- `release()`：已 released/gone 返回；status `"released"`；停 readables；`#statusChanged()`；`object.release(this)`。
- `live()`：仅 loading→live；`#statusChanged()` + `shapeChanged()`。
- `gone(error)`：已结束返回；status `"gone"`；记录 error；停 readables；`#statusChanged()`；通知 `goneListeners`。
- `shapeChanged()`：所有 readable `bind()`；shape 存在 → **入队**通知 `shapeListeners`（快照，异常 report）。
- `terminalError()`：gone → error；否则 `released`。
- `viewOf(installed)`：按 service 缓存 adapter（`bind(installed.service.contract, installed.service.options.accepts, this.contract)`）；无 adapter → implementation；有 → 按 implementation 缓存 `adapter(implementation)`。
- `currentView()`：非 live → undefined；否则 `#safeView(currentInstalled())`。
- `#safeView(installed)`：`viewOf` 抛错 → `object.host.report(error)`，返回 undefined（调用方随后以 bind 错误 reject）。
- `describe(variant, member)`：`contract.description.variants`；给定 variant → `variants[variant]?.[member]`；否则遍历 `Object.values(variants)` 返回首个含该 member 的描述；都没有 → undefined。
- `inVariant(view, name)`：`contract.scope !== "remote"` 恒 true；remote 作用域仅当 `describe(variantOf(view), name) !== undefined`。
- `member(name)`（代理 `get` 的落点，逐字逻辑）：
  1. `declared = describe(undefined, name)`；`view = currentView()`；`value = view?.[name]`；
  2. `declared.kind === "method"`，或未声明但 `typeof value === "function"` → `caller(name)`；
  3. `declared.kind === "state"`，或未声明但 `isSource(value) || isReadableLike(value)` → 惰性创建并缓存 `MemberReadable`（`readables: Map<name, MemberReadable>`）；
  4. `view === undefined || !inVariant(view, name)` → undefined；
  5. 否则 `value instanceof ExposedImpl ? value.resource.value : value`（资源按值暴露）。
- `caller(name)`（惰性创建并缓存到 `callers`）：`noArgs = takesNoArgs(contract, name)`；
  - 实参解构：`noArgs` → `[args, ctx] = [undefined, given[0]]`（`ref.name(ctx)`）；否则 `[args, ctx] = given`；
  - `object.host.inFactory > 0` → **同步抛** `ChordError("not_ready", `${address}.${name} called inside a factory`)`；
  - `ctx !== undefined && !(ctx instanceof CtxImpl)` → `Promise.reject(invalid("foreign_ctx", "ctx must come from a Chord host"))`；
  - `ctx === undefined || ctx.holder`（code holder/task）→ 先 `ctx?.check(host)`（失败 reject），再 `host.root((root) => object.call(this, name, args, root), { signal: ctx?.signal })` —— **每次调用开新 root**；
  - 否则 `object.call(this, name, args, ctx)`。
- `#stopReadables()`：对每个 readable `stop()`。
- `variantOf(impl)`（导出）：`impl.variant` 是 string 则返回，否则 undefined。

### 2.19 `src/remote.ts`（658 行）— 远程引用的本地镜像

```ts
export interface StateIn { readonly as: "snap"|"reset"|"ops"|"error"; readonly value?: unknown;
                           readonly ops?: readonly unknown[]; readonly error?: WireError }
export interface RemoteChannel {          // 由 Connection 实现
  hold(binding: RemoteBinding, again: boolean): number;
  release(id: number): void;
  call(hold: number, member: string, args: unknown, root: RootRef | undefined, signal: AbortSignal): Promise<unknown>;
  wantState(hold: number, wanted: boolean): void;
}
export interface RemoteHost {             // 由 HostImpl 实现
  readonly inFactory: number;
  report(error: unknown): void;
  root<T>(fn: (ctx: Ctx) => T | Promise<T>, options?: { signal?: AbortSignal }): Promise<T>;
  bindingIdle(binding: RemoteBinding): void;
  forgetBinding(binding: RemoteBinding): void;
}
export class RemoteBinding { … }          // 公开字段见下
```
`RemoteBinding` 公开字段：`host`、`key`、`contract`、`at`、`input`、`principal: unknown | undefined`、`status: "loading"|"live"|"ended" = "loading"`、`error: unknown`、`shape: Shape|undefined`、`hydrated = false`、`hold: {channel,id}|undefined`、`stateOn = false`、`views: Set<RemoteRef>`、`members: Map<string, MemberState>`（`MemberState = {value, decoder}`）、`lingerTimer`。
私有：`#listeners: Map<string, Set<ViewReadable>>`、`#pending: {shape, missing: Set<string>, values: Map<string, MemberState>}|undefined`、`#live`（deferred，且 `.catch(noop)`）、`#applied = false`、`#hydrationWaiters: Set<Settle>`。

访问器：`ended`、`stateViews`（`wantsState` 的视图数）、`wantsState`（无视图时取 `stateOn`，否则 `stateViews > 0`）、`applied`。

视图与状态：
- `addView(view)`：加入；清 lingerTimer；`#syncState()`；`view.update()`。
- `removeView(view)`：未命中返回；从每个 `#listeners` 集里删掉该 view 的 readable；视图清空 → `host.bindingIdle(this)` 并**返回**（不 sync）；否则 `#syncState()`。
- `#syncState()`：`ended || views.size === 0` → 返回；`wanted = stateViews > 0`；未变化 → 返回；否则 `stateOn = wanted`；`!wanted` → `hydrated = false`、`members.clear()`、清 `#pending.missing`；有 hold → `wantState(hold.id, wanted)`；所有 view `update()`。
- `listen(name, readable)` / `unlisten(name, readable)`。
- `inVariant(name)`：`shape.variant !== undefined && contract.description.variants[variant]?.[name]?.kind === "state"`。
- `read(name)`：ended → `terminalError()`；`!hydrated` → `ChordError("not_ready", `${key}.${name} is not hydrated yet`)`；不在 variant → `ChordError("not_in_variant", `${key}: ${name} is not a state member of variant ${shape?.variant}`)`；返回 `members.get(name)?.value`。
- `whenHydrated()`：已 hydrated → resolve；ended → reject terminalError；否则登记 waiter。
- `#settleHydration(error?)`。
- `readable(name)`：`status !== "ended" && hydrated && inVariant(name)`。
- `terminalError()`：`error ?? new ChordError("released", `reference ${key} released`)`。
- `bound(channel, id)`：`hold = {channel,id}`；`stateOn = wantsState`；`applied` 为真时下次 `hold` 帧带 `again: true`（`HostImpl.connect` 与 `#acquireRemote` 的 re-hold 传 `binding.applied`）。
- `shaped(shape, listed)`：ended 返回；`missing = new Set(listed)`；若还有旧的 `#pending`，把它的 missing 也并入（中继可能乱序）；`wanted = #stateMembers(shape)`；从 missing 中删掉"不在 wanted 或 `!stateOn`"的；`#pending = {shape, missing, values: 旧 values ?? new Map()}`；`missing.size === 0` → `#apply()`。
- `state(name, frame)`：
  - `as === "error"`：`error = fromWireError(frame.error)`；若 `!hydrated && stateOn` → `gone(error)` + `close()`；否则 `host.report(error)`；
  - `as === "ops"`：`target = pending?.values.get(name) ?? members.get(name)`；无 → 返回；`ops = target.decoder.decode(frame.ops ?? [])`；`target.value = applyImmutable(target.value, ops)`；若 target 就是 `members.get(name)` → `#deliver(name, {type:"ops", ops, value})`；
  - `snap`/`reset`：`fresh = {value: frame.value, decoder: decoder()}`；若在 pending 中（missing 或 values 含该名）→ 存入 pending.values、删 missing、空则 `#apply()`、返回；否则 `inVariant(name)` 为假 → 返回；`members.set(name, fresh)`；`#deliver(name, {type:"reset", value})`。
- `gone(error)`：ended 返回；`status = "ended"`；记录 error；清 hold/pending；`#live.reject(error)`；`#settleHydration(error)`；所有 view `end(error)`；清 listeners；`host.forgetBinding(this)`。
- `detached()`：ended 返回；清 hold/pending；`status === "live"` → `"loading"` 且新 deferred；所有 view `update()`。
- `close()`：清 lingerTimer；hold 存在 → `release(hold.id)`；`status !== "ended"` → 置 ended、error = `released`、reject `#live`、settle hydration、所有 view `end(error)`；清 pending/listeners；`forgetBinding`。
- `call(name, args, ctx)`：**循环**直到 `status === "live"` 且 hold 存在；等待用 `raceSignal(#live.promise, ctx.signal)`（ctx 取消 → abort）；`description = contract.description.variants[variant]?.[name]`；`kind !== "method"` → `not_in_variant`（消息 `` `${key}: ${name} is not a method of variant ${variant}` ``）；`sent = description.args === undefined ? undefined : Value.Clean(description.args, Value.Clone(args))`；`hold.channel.call(hold.id, name, sent, {rootId: ctx.rootId, codeVersion: ctx.codeVersion}, ctx.signal)`。
- `#stateMembers(shape)`：`shape.members.filter(n => contract 的该 variant 中 n 是 state)`。
- `#apply()`：`shape = pending.shape`；`wanted = #stateMembers`；删除不再 wanted 的 members；把 pending.values 中 wanted 的写入 members；`hydrated = stateOn && wanted 全部有值`；hydrated 时 settle hydration；`status = "live"`；`#applied = true`；`#live.resolve()`；所有 view `update()`；对 pending.values 中 wanted 的成员 `#deliver(name, {type:"reset", value})`；所有 view `shapeChanged()`。
- `#deliver(name, event)`：对 `#listeners.get(name)` 快照逐个 `deliver(event)`。

`ViewReadable`（非导出）：`view`、`#name`、`#values`、`#events`、`#unseen`。
- `value`：`view.ended` → `view.terminalError()`；否则 `view.binding.read(name)`。
- `[HYDRATION]`：binding 已 hydrated → undefined；否则 `{hydrated:false, whenHydrated: () => binding.whenHydrated()}`。
- `subscribe(l)`：加入；`#attach()`；若 `#readable()` → 立刻用 `members.get(name)?.value` 回调（`#safe` 包裹）。
- `events(l)`：加入；`#attach()`；可读 → `{type:"snap", value}`；否则进 `#unseen`。
- `deliver(event)`：对 events 监听者：若在 `#unseen` 中则删除它并只投递 `snap`（当 event 不是 ops），否则投递原 event；对 values 监听者投递 `event.value`（每个都用 `#safe` 隔离异常，异常上报 host）。
- `stop()`：清空三个集合 + `binding.unlisten(name, this)`。
- `#readable()`：`!view.ended && binding.readable(name)`；`#attach()`：未 ended 时 `binding.listen(name, this)`；`#remove(set, l)`：删后若两个集合都空 → `unlisten`；`#safe(fn)`：异常 → `host.report`。

`RemoteRef`（导出，`RefCore` 子类，`object` 为 `undefined`）：
- 字段：`binding`、`owner: RemoteHost & {inFactory}`、`wantsState: boolean`、`proxy`、`ready`（deferred）、`status: RefStatus`、`error`、`shapeListeners`、`#statusListeners`、`#readables`、`#callers`、`#settleReady`。
- `contract`、`address`、`ended`、`variant()`、`shape()`、`onShape(fn)`、`onStatus(fn)`、`terminalError()`、`release()`（settle ready 为 reject `` `reference ${address} released` ``）、`update()`、`end(error)`、`shapeChanged()`、`#notify()`、`member(name)`、`#caller(name, ctx)`、`#call(...)`。
- `member(name)`：在**所有** variant 的成员里找第一个同名描述；`kind === "method"` → `#caller(name)`；`kind === "state"` → 缓存并返回 `ViewReadable`；否则 undefined。
- `#caller(name)`：缓存的调用函数：
  - `noArgs = takesNoArgs(contract, name)`；无参方法签名为 `ref.name(ctx)`；
  - `host.inFactory > 0` → **同步抛** `ChordError("not_ready", `${address}.${name} called inside a factory`)`；
  - `ctx !== undefined && !(ctx instanceof CtxImpl)` → `Promise.reject(invalid("foreign_ctx","ctx must come from a Chord host"))`；
  - `ctx?.check(host)` 抛错 → reject；
  - `ctx === undefined || ctx.holder` → `host.root(root => this.#call(name, args, root), { signal: ctx?.signal })`（**每次调用新 root**）；否则 `#call(name, args, ctx)`。
- `#call(name, args, ctx)`：ended → reject terminalError；否则 `binding.call(name, args, ctx)`。
- `update()`：`live = binding.status === "live" && binding.hold !== undefined && (!wantsState || binding.hydrated)`；状态变化 → `#notify()`；live 且此前 loading → resolve ready。
- `end(error)`：`error` 是 code 为 `released` 的 ChordError → status `"released"` 且**不**记录 error；否则 `"gone"` 并记录；`#notify()`；reject ready；停止所有 readable。
- `release()`：ended 返回；status `"released"`；`#notify()`；reject ready `` `reference ${address} released` ``；停止 readables；`binding.removeView(this)`。
- `shapeChanged()`：shape 存在且未 ended → 逐个通知 `shapeListeners`（异常 report）。
- `#notify()`：`notifyStatus(#statusListeners, status, error, report)`（入队投递）。

### 2.20 `src/reload.ts`（723 行）— 两版本热更新

```ts
export interface ReloadOptions {
  readonly remove?: readonly string[];              // 要移除的 facet 名
  readonly gateDeadline?: number;                   // 默认 5000（超时即 abort）
  readonly retireDeadline?: number;                 // 默认 5000（超时强制退役旧版本）
  readonly remote?: Readonly<Record<string, readonly string[]>>;        // host 名前缀 -> facet 模块
  readonly remoteRemove?: Readonly<Record<string, readonly string[]>>;  // host 名前缀 -> 要移除的 facet
  readonly voteDeadline?: number;                   // 默认 gate + retire + 5000
}
export interface ReloadReport {
  readonly codeVersion: number;
  readonly previous: number;
  readonly changed: readonly string[];
  readonly removed: readonly string[];
  readonly prepared: readonly string[];   // 安装了新 factory 的对象地址
  readonly gated: readonly string[];      // 走过 gate 的对象地址
  readonly children?: Readonly<Record<string, ReloadReport>>;  // 集群 reload 时各子 host 的投票报告
}
```
- `runReload(host, plugins, options)`：`beginReload` → `reload.prepare()` → `reload.commit()`，`finally` 中 `endReload(host)`。**串行**：第二次调用会被拒绝。
- `Reload.summary()`（公开；子 host 用它投票）：`{ codeVersion: host.pending?.id ?? host.current.id, previous: host.current.id, changed: [...changed], removed: [...removed], prepared: 所有 status 为 live 的 candidate 的 object.address, gated: 所有 gated candidate 的 object.address }`。
- `beginReload(host, plugins, options, codeVersion?)`：`host.reloading` 为真 → `ChordError("unavailable","a reload is already running",{reason:"reload_running"})`（**code 是 `unavailable` 不是 `invalid`**）；置 `reloading = true` 后 `new Reload(host, await host.resolve(plugins), options, codeVersion)`；失败 → `host.reloadEnded()` 后抛 `reloadError(error)`。
- `endReload(host)` = `host.reloadEnded()`。
- `reloadError(error)`：`cause = asChordError(error, "reload")`；若 `cause.message.replace(/^[^:]*: /, "")` 以 `reload aborted` 开头 → 原样返回 cause；否则 `new ChordError(cause.code, `reload aborted: ${cause.message}`, { cause, reason: cause.reason, details: cause.details })`——**保留底层 code**（如 `contract_mismatch`）。
- `dryRun(candidate)`：`exposed = memberSchemas(service, run.implementation)`；对每个迁移 cell 的每个暴露 schema `assertValid("state", `${address}: migrated state ${cell.key}`, schema, value)`；`values = new Map(migrated)`；derives 按 `derived.seq` **升序**逐个 `compute(source => values.has(source) ? values.get(source) : candidateValue(source))`，每个暴露 schema `assertValid("state", `${address}: derivation ${name}`, schema, value)`，写入 values；结果存 `candidate.derived`。
- `candidateValue(source)`：读 `source.value`，吞掉 `not_ready` / `not_in_variant` 返回 undefined（被升级的 provider 的引用在 commit 前读不到东西）。
- `entries()`：`host.liveDecls(true).filter(([object]) => !removed.has(object.typeName)).map(([object, decls]) => [object, candidates.get(object)?.run.decls ?? decls])`。
- `ABORTED_ROOTS_GRACE = 1000`；`until(host, ready, ms)`：`ready()` 立即真 → `Promise.resolve(true)`；否则用 `watchRetirement` 监听并在超时后 resolve false（timer 会清理）。

`Reload` 类字段（全部公开）：`host`、`facets`、`options`、`codeVersion: number | undefined`、`candidates: Map<ObjectImpl, Candidate>`、`abort: AbortController`、`closed: [ObjectImpl, ResourceImpl][]`、`services: Map<string, Service>`、`changed: Set<string>`、`removed: Set<string>`、`tables: Map<string, RegistryTable<unknown>>`、`gated: Candidate[]`、`rolledBack`、`deferred: Map<ObjectImpl, StagedListeners>`、`facetRuns: Map<string, AbortController>`。

`Reload` 流程（顺序即语义）：
1. **prepare 管道**（`prepare()`，逐条顺序）：
   1. `await waitForRetirement()`；
   2. `load()`（同步；建候选 service 表、`nextCodeVersionId`）；
   3. `checkProviders()`；
   4. `await prepareAll()`（第一遍）；
   5. `host.gating = this.changed`（此后新对象报 `upgrade_conflict`，可重试；注释逐字：从此候选集在飞行中的激活都准备好之后即为最终）；
   6. `await withDeadline(this.prepareAll(), "activations in flight did not finish")`（**第二遍**；`withDeadline` 的 deadline 用的是 **`gateDeadline ?? 5000`**，超时 reason 为 `deadline`，message 为 `` `${what} within ${deadline} ms` ``）；
   7. `checkRefs()`；
   8. `tables = host.buildTables(entries())`；
   9. `await gate()`；
   10. `#alive()`；
   11. `#stage()`。
   任一失败 → `await rollback()` 后抛 `reloadError(error)`。
   `#alive()`：`rolledBack` 为真 → `ChordError("aborted", "reload aborted while preparing", {reason:"aborted"})`（每次 await 之后都要检查）。
   `withDeadline(promise, what)`：超时来自 **`options.gateDeadline ?? 5000`**，到点 `ChordError("aborted", `${what} within ${deadline} ms`, { reason: "deadline" })`。
2. `waitForRetirement()`：
   - `previous = host.previous`；无 → 返回；
   - `deadline = options.retireDeadline ?? 5000`；`gone = () => host.previous !== previous`；
   - `await until(host, () => gone() || host.retirable(), deadline)`；若超时且仍未 gone → `previous.abort.abort(ChordError("superseded", `code version ${previous.id} was retired`))`，再 `await until(host, () => gone() || previous.roots.size === 0, Math.min(deadline, ABORTED_ROOTS_GRACE = 1000))`；
   - 仍未 gone → `host.retire()`。
3. `checkProviders()`：
   - 对 `changed` 中每个服务：`old.version === next.version` 且 `coverage(next.description, old.description)` 有原因 → `ChordError("contract_mismatch", `${name}@${next.version} changed incompatibly without a version bump (${reason})`)`；
   - facet 的 `uses`（含被移除后的最终 facet 集合）必须 `bind`，失败即 `contract_mismatch`。
4. `prepareAll()` / `prepareObject(object, run)`：
   - 每个 cell 自带版本：`to < from` → `invalid("state", …)`，消息 `` `state: ${address}: version of state ${name} lowered from ${from} to ${to}` ``；`to > from` → reasons 追加 `` `state ${name} version ${from} -> ${to}` ``（走 gate + migrate）；`to === from` 且 `!deepEqual(cell.schema, run.decls.cellSchemas.get(name))` → `` `state: ${address}: schema of state ${name} changed without a version bump` ``；
   - 资源推迟到 gate：`deferred` 集合 = `run.created.resources` 中满足 `resource.exclusive || resource.deps.some(dep => deferred.has(dep))` 的资源（**独占资源，以及依赖任何一个 deferred 资源的新资源，递归传递**）。对每个 `deferred`：`candidate.atGate.push(resource)`；若 `resource.exclusive && old?.exclusive && old.state === "open"`（`old = object.decls.resources.get(resource.key)`）→ `candidate.replaced.push(old)`。
   - **`deferred.size > 0` 就 reasons 追加 `"exclusive resource replaced"`** —— 注意触发条件比字面更宽：**只要本次 run 产生任何独占资源（即使对应的旧资源不存在，或没有依赖链）就会 gate**，并不要求真的"替换了旧的独占资源"。依赖 deferred 资源的非独占新资源同样计入。
   - 随后 `await object.open(run.created.resources.filter(r => !deferred.has(r)), run.created.refs, this.abort.signal)`；`await object.waitHydrated(run)`；**仅当 `candidate.reasons.length === 0` 时**才 `await object.initCells(run, this.abort.signal)` + `dryRun(candidate)`（gated 对象的 initCells 推迟到 `gate()` 里做）。
5. `checkRefs()`：**不是** facet `uses` 检查（那是 `checkProviders()`），也**不是** registry 表检查（那是 `buildTables()`）。它只保证每个活到新代码版本的引用都能绑定到新的 provider：
   - 先算 `dropped`：对每个 candidate，枚举 `object.decls.refs`，凡不在 `run.decls.refs` 里的引用都记入 `dropped`；
   - 再遍历 `host.objects`：无 service 或不在 `changed` 中 → 跳过；对每个 `object.refs`：是 `host.hostObjects.get(typeName)`（host 的 bootstrap 引用，它跟着 provider 在 commit 时换）→ **跳过**；`!dropped.has(ref) && ref.owner !== "client"` → `bind(service.contract, service.options.accepts, ref.contract)`，失败即 `contract_mismatch`。
   - **浏览器（`owner === "client"`）引用不阻塞 reload**：`install` 阶段改为 `upgrade_required`（见第 10 条）。
   `entries()`：`host.liveDecls(true).filter(([object]) => !removed.has(object.typeName)).map(([object, decls]) => [object, candidates.get(object)?.run.decls ?? decls])`。
6. `gate()`：`gated = candidates 中 reasons.length > 0 且 object.status === "live" 的项`；每个 gated 对象建 `object.gate = { grandfathered: new Set(object.leases.keys()), queue: [], drained }`、`object.stopTasks()`，`grandfathered` 为空则立刻 `drained()`；`Promise.race([Promise.all(drained).then(() => false), setTimeout(gateDeadline) → true])`，到点后 `clearTimeout`；超时 → `ChordError("aborted", `gate deadline (${deadline} ms) missed; blocking roots: ${blocking.join(", ")}`, { reason:"gate_deadline", details:{ blocking } })`，`blocking` 来自 `this.gated.flatMap(({object}) => [...(object.gate?.grandfathered ?? [])])`；随后 `#alive()`；再**对每个 gated candidate**：先逐项关闭 `candidate.replaced`（关闭前 `this.closed.push([object, old])` 以便回滚恢复），再 `await object.open(candidate.atGate, [], this.abort.signal)`、`#alive()`、`await object.initCells(candidate.run, this.abort.signal)`、`#alive()`；全部处理完后才对每个 gated candidate `migrate(candidate)`。
7. `migrate(candidate)`：对 `run.decls.cells` 每个 `[name, cell]`：`object.decls.cells.get(name) !== cell` → 跳过；`schema = run.decls.cellSchemas.get(name)`；`from = object.decls.cellVersions.get(name) ?? 1`；`fn = run.decls.cellMigrations.get(name)`；`to = run.decls.cellVersions.get(name) ?? 1`。取值：`to > from && fn !== undefined` → `value = fn(structuredClone(cell.value), from)`；否则 `!deepEqual(cell.schema, schema)` → `value = cell.value`（值不变，但要按新 schema 重新校验）；否则 `continue`。然后 `assertValid("state", `${object.address}: migrated state ${name}`, schema, value)`、`candidate.migrated.set(cell, value)`。最后 `dryRun(candidate)`。
8. `rollback()`：幂等（`rolledBack`）；`this.abort.abort(new ChordError("aborted","reload aborted"))`，各 facet run 同样 abort；discard 每 candidate 与 `deferred` 的 staged；对每 candidate `object.pending.delete(run.decls)` + `object.unwind(run)`（失败 report）；对 `this.closed` 中 `state === "closed"` 的资源用新 `AbortController().signal` **重新 open** 恢复，失败 → `resource.markUnavailable()` + `ChordError("unavailable", `${object.address}: resource ${key} could not be restored`, { cause })`（**只标记不可用，绝不静默关闭**）；对每个 gated 对象若还有 gate → `object.openGate()` + `object.startTasks()`；对 `host.objects` 中不在 `host.current.services` 的类型 `dispose(true, ChordError("gone","reload aborted; type not served"))`（失败 report）；最后 `host.pending = undefined`、`host.gating = undefined`。
9. `commit(beforeStart?)`：整个提交包在 **`holdPublications(() => { const report = #publish(); beforeStart?.(); for (const start of #starts.splice(0)) start(); return report })`** 里（`#starts` 是"会启动新版本 root 的工作"队列：task 启动、facet run 信号 abort、host 对象启动、pinned ref 释放）。抛错 → `host.fail(error)`（**终态**，host 不再接活）后抛 `reloadError(error)`。
   `#publish()`（同步、不可失败的部分全在 prepare 做过）：
   1. `next = host.pending`；`host.pending = undefined`；`host.codeVersions.set(next.id, next)`；`host.previous = previous`；`host.current = next`；
   2. 遍历 `host.objects` 快照：类型被移除 → 跳过；无 candidate → 把 `implementations.get(previous.id)` 复制为 `{...installed, codeVersion: next.id}`；`status !== "live"` → 跳过；否则 `install(candidate, next.id)` 并记入 `prepared`；
   3. 对 status 非 live 的 candidate：`staged?.discard()`、`object.pending.delete(run.decls)`、`object.unwind(run)`（失败 report）；
   4. 对 `deferred`（本次新增类型的活对象）：非 live → discard；否则 `deferredRuntime = false`、`staged.activate()`、`#starts.push(() => object.startTasks())`；
   5. `host.installTables(next, this.tables, true)`；
   6. 被移除的类型：`object.dispose(true, ChordError("gone", "type no longer served"))`（失败 report）；删 `host.hostObjects`；
   7. `host.facets` 写入新 facet、删 `options.remove`；
   8. `#starts` 里推入被替换/移除 facet run 的 `host.facetRunChanged(name, run | undefined)`；
   9. `host.gating = undefined`；
   10. 对每个 service：host lifetime 且 changed 且现有 host 对象非 live → `#starts.push(() => host.startHostObject(service))`；lifetime 从 host 变回 referenced 且有 pinned 引用 → 删 `host.hostObjects` 并 `#starts.push(() => pinned.release())`；
   11. 返回 `{ codeVersion: next.id, previous: previous.id, changed, removed, prepared, gated }`。
10. `install(candidate, codeVersion)`（逐条顺序，`gated = candidate.reasons.length > 0`）：
    1. `object.pending.delete(run.decls)`；`object.stopTasks()`；
    2. gated：detach 掉新 run 没有保留的旧 derivations（旧代码再也不可能跑在迁移后的状态上）；
    3. `object.decls = run.decls`；`object.service = service`；
    4. 对 `run.decls.adopted` 逐项 `resource.adopt(declaration)`（保留资源用新代码的 `close`）；
    5. `object.implementations.set(codeVersion, {codeVersion, service, implementation, decls})`；
    6. gated：`object.minCodeVersion = codeVersion`，并对所有其它版本 `host.retireImplementation(object, old)`（**立刻退役，不等 retire 截止**）；
    7. `object.refreshSchemas()`；
    8. 每个 cell：迁移过 → `cell.reset(migrated, schema)`（**订阅者收到 reset 事件**）；否则 `cell.schema = schema`；
    9. 新 derivations `attach(candidate.derived)`；未被保留的 derivations `refresh()`；
    10. `candidate.staged?.activate()`；`#starts.push(() => object.startTasks())`；`object.openGate()`；
    11. host 对象重绑：pinned 且指纹变了 → **先** `object.addRef(replacement)`（避免释放唯一引用导致 dispose/reopen）、换 `host.hostObjects`、再 `pinned.release()`；
    12. 遍历 `object.refs` 快照：client 引用 `bind` 失败 → `ref.gone(ChordError("upgrade_required", `${object.typeName} changed; reload the web facets`, { cause: error }))` + `object.release(ref)`；否则 `ref.shapeChanged()`。

### 2.21 `src/cluster.ts`（251 行）— 集群级 reload（server 协调）

- `let reloadSeq = 0;`（模块级；每次 prepare 递增，作为 `reload` id）。
- `modulesFor(remote, hostName)`：遍历 `Object.entries(remote ?? {})`，取 `hostName?.startsWith(prefix)` 且前缀最长者，返回其数组的**拷贝**（`[...list]`）；无匹配或 `hostName === undefined` → `[]`。
- `awaitVote(connection, id, deadline)`：`connection.votes.set(id, {resolve, reject})`；`setTimeout(deadline)` 到点删除条目并 reject `ChordError("aborted", `votes missing after ${deadline} ms`, { reason:"vote_deadline" })`；`vote.finally(clearTimeout)`。

`runClusterReload(host, plugins, options): Promise<ReloadReport>`：
1. `synced = [...host.connections].filter(c => c.role === "downstream" && c.peer.kind === "host" && c.open && c.synced)`（在 beginReload **之前**快照；reload 期间新到的 hello 由 `peerHello` 在本轮结束后 sync/reconcile）。
2. `codeVersion = host.peekNextCodeVersion()`；`reload = await beginReload(host, plugins, options, codeVersion)`。
3. `await reload.waitForRetirement()`（retire 帧先于 prepare 到达每个子 host，FIFO）。
4. `participants = synced.filter(c => c.open)`；`id = ++reloadSeq`；`deadline = options.voteDeadline ?? (options.gateDeadline ?? 5000) + (options.retireDeadline ?? 5000) + 5000`。
5. 对每个参与者：`name = connection.name ?? "child"`；先登记 vote，再发
   ```json
   {"t":"prepare","reload":id,"codeVersion":cv,"plugins":modulesFor(options.remote,name),
    "options":{"remove":modulesFor(options.remoteRemove,name),"gateDeadline":…,"retireDeadline":…}}
   ```
   （`gateDeadline`/`retireDeadline` 为 `undefined` 时 JSON 里省略）；vote 成功 resolve `[name, report ?? undefined]`；vote 失败 → `cause = reloadError(error)`，抛 `new ChordError(cause.code, `${name}: ${cause.message}`, { cause, reason: cause.reason })`（**保留底层 code**）。
6. `Promise.allSettled([reload.prepare().then(() => local = true), ...votes])`。
7. 任一 rejected：对所有参与者 `votes.delete(id)` + 发 `{t:"abort", reload:id}`；若本地 prepare 成功则 `await reload.rollback()`；抛第一个 rejected 的 reason。
8. 全部成功：`report = reload.commit(() => { 对每个参与者发 {t:"commit",reload:id} 并置 peerCodeVersion = codeVersion })`；`host.rememberRemoteModules(options.remote, options.remoteRemove)`；`children` 收集各 fulfilled vote 的 report（undefined 跳过）；返回 `{ ...report, children }`。
9. `finally { endReload(host) }`。

`reconcileChild(host, connection)`：无限循环：
1. `!connection.open` → 返回；
2. `host.reloading` → `await host.whenReloadEnded()` 后 continue；
3. `codeVersion = host.current.id`；若 `connection.peerCodeVersion === codeVersion` → `markSynced(codeVersion)` + `host.announceRetired(connection)` → 返回；
4. `host.announceRetired(connection)`（必须在 prepare 前：子端的 prepare 会先退役其上一版本，等待这个许可）；
5. `id = ++reloadSeq`；`vote = awaitVote(connection, id, 30_000)`；
6. 发 `{t:"prepare", reload:id, codeVersion, plugins: modulesFor(host.remoteModules, connection.name), options:{remove: modulesFor(host.remoteRemoved, connection.name)}}`；
7. `await vote`；若 `host.current.id !== codeVersion` → 发 `{t:"abort", reload:id}` 并 continue（用新版本重试）；
8. 发 `{t:"commit", reload:id}`；`connection.peerCodeVersion = codeVersion`。

`ChildOperation { readonly id; reload: Reload | undefined; state: "loading"|"preparing"|"prepared"; cancelled: boolean }`（在第一次 await 前登记，abort/断链不会丢）。

`ChildReloads`（导出类）：
- 字段：`#host`、`#operation: ChildOperation | undefined`。
- `handle(connection, frame: prepare|commit|abort)`：
  - `prepare`：已有操作 → 回 `{ t:"vote", reload: frame.reload, error: toWireError(new ChordError("unavailable", "a reload is already running", { reason:"reload_running" })) }`；否则**先**登记 `{ id: frame.reload, reload: undefined, state: "loading", cancelled: false }`，再 `#run(...).catch(e => #host.report(e))`（登记在首次 await 前，abort/断链不会丢）；
  - 其它帧：无操作或 `id` 不匹配 → 忽略；`abort` → `#cancel(operation)`；`commit` 且 `state !== "prepared"` → 忽略；否则清空 `#operation`，`try { operation.reload?.commit() } finally { endReload(host) }`（提交后不再取消）。
- `parentLost()`：有操作 → `#cancel`。
- `#cancel(operation)`：`cancelled = true`；`state !== "prepared"` → 直接 `operation.reload?.rollback().catch(report)`（让 prepare 尽快失败，`#run` settle 后再收尾）；`state === "prepared"` → 清 `#operation`，`rollback().catch(report).finally(() => endReload(host))`。
- `#run(operation, connection, frame)`：
  1. `operation.reload = await beginReload(host, frame.plugins, frame.options, frame.codeVersion)`；已取消 → 抛；
  2. `state = "preparing"`；`await reload.prepare()`；已取消 → 抛；
  3. `state = "prepared"`；`connection.send({ t:"vote", reload: frame.reload, report: reload.summary() })`；
  4. catch：有 reload → `await rollback().catch(report)` 且 `endReload(host)`；`#operation === operation` → 清空；**未取消** → `connection.send({ t:"vote", reload: frame.reload, error: toWireError(error) })`（已取消则不投票）。
  - 取消错误逐字：`ChordError("aborted", "reload aborted by the server", { reason: "aborted" })`。

Rust 注意：`votes` 的登记**先于** `send`（避免同步回复丢失），且 vote 的 reject 是 ChordError `aborted/vote_deadline`。

### 2.22 `src/manager.ts`（443 行）— 宿主进程管理（Node）

```ts
export type ControlMessage =
  | { readonly type: "init"; readonly value: unknown }
  | { readonly type: "ready" }
  | { readonly type: "fatal"; readonly message: string; readonly reason?: string }
  | { readonly type: "shutdown" }
  | { readonly type: "status"; readonly activeCalls?: number; readonly viewers?: number; readonly durableTasks?: number };
```
包装：`{ chord2control: message }`。**注意字段名是 `type` 不是 `t`**（与 wire 帧不同）。

```ts
export interface Liveness { booting: boolean; activeCalls: number; viewers: number; durableTasks: number; idleFor: number; }
export interface Exit { code: number | null; signal: string | null; }
export interface Instance {
  readonly pid: number;
  readonly link: IpcLink;                          // 交给 host.accept
  readonly exited: Promise<Exit>;
  send(message: ControlMessage): void;
  killTree(): void;
  onControl(listener: (message: ControlMessage) => void): void;
}
export interface Driver { spawn(): Instance; }     // 同步 spawn（不是 async start）
export interface ForkDriverOptions {
  readonly entry: string;
  readonly args?: readonly string[];
  readonly env?: Readonly<Record<string, string>>;
  readonly execArgv?: readonly string[];
  readonly init?: unknown;
  readonly lock?: string;                                    // 锁名（字符串，不是对象）
  readonly onOutput?: (stream: "stdout" | "stderr", chunk: Uint8Array) => void;
  readonly onMessage?: (message: unknown) => void;
}
export interface HostManager {
  readonly status: "stopped" | "starting" | "running" | "stopping";
  readonly instance: Instance | undefined;
  readonly liveness: Liveness;
  start(): Promise<Instance>; stop(): Promise<void>; replace(): Promise<Instance>;
}
export interface HostManagerOptions {
  readonly driver: Driver;
  readonly keepAlive?: (liveness: Liveness) => boolean;
  readonly checkInterval?: number;                 // 默认 1000
  readonly grace?: number;                         // 默认 5000
  readonly readyTimeout?: number;                  // 默认 30000
  readonly onExit?: (exit: Exit) => void;
}
```
- `processLock(name): Promise<() => Promise<void>>`：`key = sha256(name) 的 hex 前 32 字符`；`file = platform 不是 win32/linux/android`；
  - `win32` → `\\.\pipe\chord2-${key}`；`file` → `await lockSocketPath(`chord2-${key}.sock`)`；其余 → `\0chord2-${key}`（抽象命名空间，**首选**）；
  - `server = createServer(socket => socket.end())`，无限循环 `listen`：
    - 成功 → break；
    - 错误码不是 `EADDRINUSE` → 原样抛；
    - `!file || (await accepting(address))` → `ChordError("unavailable", `lock ${name} is held by another process`, { reason: "locked" })`；
    - 否则 `rm(address, {force:true})` 后重试（陈旧文件接管）；
  - `file` 时 `process.once("exit", removeFile)`（`unlinkSync`，吞异常）；返回的释放函数先移除 exit 监听再 `server.close()`。
- `MAX_SOCKET_PATH_BYTES = 103`（sun_path 104 含终止 NUL）。
- `lockSocketPath(file)`：`preferred = join(tmpdir(), file)`；`Buffer.byteLength(preferred) <= 103` → 返回；否则 `directory = `/tmp/chord2-${process.getuid?.() ?? 0}``，`mkdir(mode 0o700, recursive)`，`lstat` 检查 `isDirectory() && uid 匹配 && (mode & 0o077) === 0`，否则 `ChordError("unavailable", `lock directory ${directory} must be a directory only its owner can access`)`（**注意是 ChordError 不是普通 Error**）；返回 `join(directory, file)`。
- `accepting(address)`：`connect` 成功 → true；错误码不是 `ECONNREFUSED`/`ENOENT` → true；否则 false。
- `LOCK_ENV = "CHORD2_LOCK"`；`ignoreSendError = () => {}`。
- `forkDriver(options)` → `{ spawn() }`：
  1. `env = {...process.env}`；**`delete env[LOCK_ENV]`**（继承的锁不是子进程的）；`Object.assign(env, options.env)`；`options.lock !== undefined` → `env[LOCK_ENV] = options.lock`；
  2. `fork(entry, [...args], { env, execArgv: [...execArgv], detached: true, stdio: ["ignore", onOutput ? "pipe" : "inherit", onOutput ? "pipe" : "inherit", "ipc"] })`；
  3. `onOutput` 存在时对 stdout/stderr `on("data", chunk => { onOutput(stream, chunk); process[stream].write(chunk) })`（**仍镜像到父进程**）；
  4. `pid = child.pid ?? -1`；`killGroup()` = `process.kill(-pid, "SIGKILL")` 吞异常；
  5. `exited`：`child.once("exit", (code, signal) => { killGroup(); resolve({code, signal}) })`——**leader 怎么退出都好，它启动的东西都不许活着**；
  6. `child.on("message", …)`：先 `options.onMessage?.(message)`，再取 `.chord2control` 分发给 `listeners`；
  7. `send(message)`：`child.connected` 时才 `child.send({ chord2control: message }, ignoreSendError)`；
  8. `options.init !== undefined` → 立即 `send({ type: "init", value: options.init })`；
  9. 返回 `{ pid, link: ipcLink(child as IpcChannel), exited, send, killTree: () => { try { process.kill(-pid, "SIGKILL") } catch { child.kill("SIGKILL") } }, onControl: l => listeners.add(l) }`。
- `createHostManager(options)`：
  - 内部状态 `status`、`instance`、`queue: Promise`（`serialized(step)`：`queue.then(step, step)`，`queue = next.catch(() => {})`）、`report = {activeCalls,viewers,durableTasks}`、`idleSince`、`booting`、`timer`；
  - `liveness()`：`{ booting, ...report, idleFor: idleSince === undefined ? 0 : Date.now() - idleSince }`；
  - `doStop()`：无实例 → `status = "stopped"`、`booting = false`、返回；否则 `status = "stopping"`；清 timer；`send({type:"shutdown"})`；`Promise.race([exited.then(() => true), setTimeout(grace ?? 5000) 后 false])`；未优雅退出 → `killTree()`；`await exited`；清实例；`status = "stopped"`；
  - `doStart()`：
    1. 已有 running 实例 → 直接返回；
    2. `status = "starting"`；`booting = true`；`report = {0,0,0}`；
    3. `current = options.driver.spawn()`（同步抛 → `status = "stopped"`、`booting = false`、抛出）；
    4. `instance = current`；`current.exited.then(exit => { if (instance === current) { instance = undefined; status = "stopped"; clearInterval(timer) } options.onExit?.(exit) })`；
    5. 等 ready：`setTimeout(readyTimeout ?? 30_000)` → reject `ChordError("unavailable", "child not ready in time")`；`exited` → reject `ChordError("unavailable", `child exited before ready (${exit.code ?? exit.signal})`)`；`onControl`：`ready` → resolve；`fatal` → reject `ChordError("unavailable", `child failed: ${message.message}`, { reason: message.reason })`；`status` → 更新 `report`（缺省 0），有活动 → `idleSince = undefined`，否则 `idleSince ??= Date.now()`；
    6. 失败路径：`current.killTree()`；`await current.exited`；**再抛原错误**；
    7. `booting = false`；无活动 → `idleSince ??= Date.now()`；`status = "running"`；
    8. `keepAlive` 存在 → `setInterval(checkInterval ?? 1000)`（`unref()`）：`status === "running" && !keepAlive(liveness())` → `serialized(doStop)`（**不重启**：停止后由外部或下次 start 拉起）；
  - 返回 `{ get status, get instance, get liveness, start: () => serialized(doStart), stop: () => serialized(doStop), replace: () => serialized(async () => { await doStop(); return doStart() }) }`。
- `childControl(channel = process)`（子进程侧）：
  - `send(message)`：`channel.connected` 时才 `channel.send?.({ chord2control: message }, undefined, undefined, ignoreSendError)`（父进程已死时写入 EPIPE，随后的 `disconnect` 是子进程的察觉方式）；
  - `init`：promise，收到 `{type:"init"}` 时以 `value` resolve；
  - `shutdown: Set<handler>`，收到 `{type:"shutdown"}` 逐个 `void handler()`；
  - `fatal(error)`：`{ type: "fatal", message: error instanceof Error ? error.message : String(error), ...(ChordError 且有 reason ? {reason} : {}) }`；
  - `lock()`：`process.env[LOCK_ENV]` 为 undefined → 返回；否则 `await processLock(name)`，失败 → `fatal(error)` 并重抛；
  - `ready()` → `send({type:"ready"})`；`report(status)` → `send({type:"status", ...status})`；`onShutdown(handler)` → 加入集合。

### 2.23 `src/loader.ts`（67 行）— Node loader

模块级状态：`PARAM = "chord2-reload"`、`roots: Set<string>`（已加载 facet 模块的目录 URL，带尾斜杠）、`hooked = false`、`generation = 0`。
- `hook()`（惰性、只做一次）：`registerHooks` 注册 ESM `resolve` 钩子：
  1. `result = nextResolve(specifier, context)`；
  2. `version = context.parentURL === undefined ? null : new URL(context.parentURL).searchParams.get(PARAM)`；
  3. `version === null || !result.url.startsWith("file:")` → 原样返回；
  4. URL 已有 `PARAM` 或**不属于任何 root** → 原样返回（Chord 自身与共享代码保持单实例）；
  5. 否则 `url.searchParams.set(PARAM, version)` 并返回 `{...result, url}`。
- `toUrl(module)`：`URL` 实例 → `new URL(module.href)`；字符串以 `file:` 开头 → `new URL(module)`；否则 `pathToFileURL(module)`。
- `nodeLoader(modules): Promise<Facet[]>`（`FacetLoader`；**是常量不是工厂函数**）：
  1. `hook()`；`version = String(++generation)`；
  2. `urls = modules.map(m => { const u = toUrl(m); u.search = ""; return u })`（**清空 query**）；
  3. **先**对所有 url `roots.add(new URL(".", url).href)`（每个 facet 的目录都先登记，所以 facet A 导入"排在它后面"的 facet B 的文件时也得到本次的副本 —— 这是 CHANGELOG 里修过的 bug）；
  4. 再逐个：`name = url.href`（**不带 query 的原始 href 作为 facet 名**）；`url.searchParams.set(PARAM, version)`；`import(url.href)`；`definition = loaded.default`；
  5. `definition === undefined || typeof definition.setup !== "function"` → `TypeError(`${name}: default export must be a facet (defineFacet)`)`；
  6. push `{ name, setup: definition.setup, uses: definition.uses }`。
- 已知缺陷（源码 TODO 注释逐字要点）：每次 reload 都导入新模块副本、旧副本永不驱逐（进程退出前一直泄漏）；不做隔离 —— TUI 插件需要隔离。

### 2.24 `src/websocket.ts`（35 行）

```ts
export interface WebSocketServerLike<Request> { on(event: "connection", listener: (socket: WebSocketLike, request: Request) => void): unknown; }
export interface WebSocketAcceptOptions<Request, P = unknown> extends Omit<AcceptOptions<P>, "principal" | "kind" | "name"> {
  principal(request: Request): P | undefined | Promise<P | undefined>;
}
export function acceptWebSockets<Request, P = unknown>(server, host: Host<P>, options): void;
```
- 每个 `connection` **立刻**建 `webSocketLink(socket, { compress: true, acceptPacked: false })`（认证期间发出的帧入 outbox 而不是丢失；大 payload 出站压缩；客户端不得发压缩帧 —— 小消息可能解压成任意大）。
- `Promise.resolve(options.principal(request)).then(principal => principal === undefined ? link.close("unauthorized") : host.accept(link, { ...options, principal, kind: "client" }), () => link.close("authentication failed"))`。

### 2.25 `src/output.ts`（406 行）— 有界输出缓冲与进度节流

```ts
export type OutputLimits = { readonly maxBytes: number; readonly maxLines: number; readonly retain: "head" | "tail" };  // 全必需
export type BoundedOutput = { readonly text: string; readonly droppedBytes: number; readonly droppedLines: number };
export type OutputSlice = { readonly text: string; readonly bytes: number; readonly droppedBytes: number; readonly droppedLines: number };
export interface OutputSkip { readonly bytes: number; readonly newlines: number; readonly endsWithNewline: boolean; }
```
- `NEWLINE = 0x0a`（非导出）；`INVALID_OUTPUT = /[\x00-\x08\x0b-\x1f\ufff9-\ufffb]/g`（**非导出**；源码逐字写作转义序列 `\ufff9-\ufffb`，不是字面字符）；`encoder = new TextEncoder()`；`decoder = new TextDecoder("utf-8", { ignoreBOM: true })`。
- `sanitizeOutput(text): string` = `text.replace(INVALID_OUTPUT, "")` —— **删除**这些控制字符（**不是**替换成 U+FFFD；保留 tab `\t` 与换行 `\n`）。
- `boundOutput(text, limits): OutputSlice`：`bytes = encode(text)`；`head` → `headRange`，`tail` → `tailRange`；`kept = bytes.subarray(from,to)`；`text` 在未裁剪时原样返回，否则 `decoder.decode(kept)`；`droppedLines = lineCount(bytes) - lineCount(kept)`。
- `headRange(bytes, limits)`：`maxLines === 0 || maxBytes === 0` → `[0,0]`；从头找到第 `maxLines` 个 `\n`（含）为 end；若 `end > maxBytes` → 取 `bytes.lastIndexOf(NEWLINE, maxBytes-1)`，找到则 `newline+1`，否则 `characterEnd(bytes, maxBytes)`。
- `tailRange(bytes, limits)`：`maxLines===0 || maxBytes===0` → `[len,len]`；末尾是 `\n` 时从 `len-2` 起算（尾随换行不另起一行），否则 `len-1`；`lines` 从 1 起，遇到第 `maxLines` 个换行时 `start = index+1`；若 `len - start > maxBytes` → `from = len - maxBytes`，`newline = bytes.indexOf(NEWLINE, from-1)`，`newline !== -1 && newline+1 < len` 时 `start = newline+1`，否则 `characterStart(bytes, from)`。
- `characterEnd(bytes, index)`（导出）：向前跳过 UTF-8 续字节（`& 0xc0 === 0x80`）。
- `characterStart(bytes, index)`（非导出）：向后跳过续字节。
- `lineCount(bytes)`（非导出）：空 → 0；否则换行数 + （末尾不是换行 ? 1 : 0）。
- `utf8ByteLength(content)`（导出）：有 `Buffer` → `Buffer.byteLength(content,"utf8")`；否则：全 ASCII 直接 `length`；从首个非 ASCII 起逐 UTF-16 单元计算（≤0x7f 1 字节，≤0x7ff 2，代理对高位且后随低位 → 4 并跳过，其它 3）。孤立代理 → 3 字节。
- `countNewlines(text)`（非导出）。
- `OutputBuffer`：
  - 私有：`#limits`、`#decoder`（`ignoreBOM: true`）、`#started`、`#chunks: {text,bytes,newlines}[]`、`#storedBytes`、`#storedNewlines`、`#full`、`#totalBytes`、`#totalNewlines`、`#endsWithNewline = true`。
  - `get storedBytes()`。
  - `push(chunk: string | Uint8Array, skipped?: OutputSkip): boolean`：
    1. `pending = (typeof chunk === "string" || skipped !== undefined) ? #decoder.decode() : ""`（字符串块或 skip 会**结束**之前字节块留下的不完整字符，冲成 U+FFFD）；
    2. `text = typeof chunk === "string" ? chunk : #decoder.decode(chunk, { stream: true })`；
    3. `first = !#started && pending === "" && skipped === undefined`；`pending !== "" || text !== "" || skipped !== undefined` → `#started = true`；
    4. `first && typeof chunk !== "string" && text.startsWith("\ufeff")` → `text = text.slice(1)`（**只**丢流最开头的 BOM；字符串块开头的 U+FEFF 保留）；
    5. 无 `skipped` → `return #accept(pending + text)`；
    6. `limits.retain !== "tail"` → `throw new Error("Skipped output requires tail retention")`；
    7. `#accept(pending)` → `#skip(skipped)` → `#accept(text)` → `return true`（**顺序固定**）。
  - `#skip(skipped)`：`bytes === 0` 直接返回；否则 `#totalBytes += skipped.bytes`、`#totalNewlines += skipped.newlines`、`#endsWithNewline = skipped.endsWithNewline`、**清空 `#chunks` 与 stored 计数**（`#full` **不**复位）。
  - `end()`：`#accept(#decoder.decode())`（把未完成字符冲成 U+FFFD；返回的 boolean 被忽略）。
  - `get storedBytes()`：`#storedBytes`（≤ limits + 一个块）。
  - `#accept(text)`：空 → false；算 bytes/newlines，累加 totals，更新 `#endsWithNewline = text.endsWith("\n")`；`#full` → true；
    - head：`#full = storedBytes > maxBytes || storedNewlines >= maxLines`；
    - tail：每次 push 后从头部丢 chunk，直到剩余 `bytesAfter <= maxBytes + 1 && newlinesAfter <= maxLines + 1`（**多留 1 字节/1 行**，方便之后定位行首），且至少保留 1 个 chunk。
  - `snapshot(): BoundedOutput`：拼 stored → `boundOutput`；`storedLines = lines(#storedNewlines, stored === "" || stored.endsWith("\n"))`；`keptLines = storedLines - kept.droppedLines`；tail 或 chunk 数 > 1 时把 stored 收缩为 `tailMargin(stored, limits)`（tail）或原 stored（head），重置 chunks/stored 计数；返回 `{text: sanitizeOutput(kept.text), droppedBytes: totalBytes - kept.bytes, droppedLines: lines(totalNewlines, endsWithNewline) - keptLines}`。
- `tailMargin(text, limits)`：最短后缀，使其字节数 > maxBytes 或换行数 > maxLines（或整段）；`byteStart = len > maxBytes ? characterEnd(bytes, len - maxBytes - 1) : 0`；从末尾向前数换行，超过 `maxLines` 时取该换行位置为 `lineStart`；`start = max(byteStart, lineStart)`。
- `lines(newlines, terminated) = newlines + (terminated ? 0 : 1)`。
- 进度：`PROGRESS_BYTES_PER_SECOND = 100 * 1024`；`ProgressClock = { now(): number; setTimer(cb, delayMs): () => void }`；`SYSTEM_CLOCK = { now: Date.now, setTimer: setTimeout/clearTimeout }`。
- `Progress`（构造 `(write: () => Promise<number>, onError, minIntervalMs, clock = SYSTEM_CLOCK)`）：
  - 字段 `#waiters: PromiseWithResolvers<void>[]`、`#timer`、`#inFlight`、`#nextAt = 0`、`#dirty = false`、`#stopped = false`。
  - `mark()`：`#dirty = true; #schedule()`。
  - `markAndWait()`：建 waiter，`#waiters.push`，`mark()`，返回其 promise。
  - `stop()`：`#stopped = true`；取消防抖 timer；`await #inFlight`；`return #waiters.splice(0)`（**由调用方负责结算最终提交的 waiter**）。
  - `#schedule()`：`#stopped || #timer || #inFlight` → 返回；`wait = #nextAt - now()`；`<= 0` → 立即 `#flush()`；否则 `#timer = setTimer(() => { #timer = undefined; #schedule(); }, wait)`（回调里**重新检查时钟**，提前触发会再等）。
  - `#flush()`：`#stopped || !#dirty` → 返回；`#dirty = false`；取走 waiters；`started = now()`；`#inFlight = #write().then(bytes => { #nextAt = started + Math.max(minIntervalMs, bytes*1000/102400); resolve(waiters) }, error => { #nextAt = started + minIntervalMs; reject(waiters); #onError(error) }).finally(() => { #inFlight = undefined; if (#dirty) #schedule(); })`。

语义要点：同一时刻最多一个提交在飞；期间的 `mark` 合并进下一次；首次（`#nextAt = 0`）立即提交；`stop` 返回未结算 waiter 供调用方用最终提交结算。

### 2.26 `src/context/index.ts`（161 行）

```ts
export interface ContextKey<T> {
  readonly token: symbol;
  readonly valueType?: (value: T) => T;    // 仅类型标记
}
export interface Context {
  readonly abortSignal: AbortSignal | undefined;
  readonly telemetry: TelemetrySpan;        // 注意：不是 TelemetryContext，是 Span
  value<T>(key: ContextKey<T>): T | undefined;
  toString(): string;
}
```
- `ABORT_SIGNAL_CONTEXT_KEY = Object.freeze({ token: Symbol("chord.abortSignal") })`；`TELEMETRY_CONTEXT_KEY = Object.freeze({ token: Symbol("chord.telemetry") })`（**均不导出**）。
- `abstract class BaseContext`：`get abortSignal()` = `value(ABORT_SIGNAL_CONTEXT_KEY)`；`get telemetry()` = `value(TELEMETRY_CONTEXT_KEY) ?? NOOP_TELEMETRY_CONTEXT`。
- `class EmptyContext(name)`（非导出）：`value()` 恒 undefined；`toString()` 返回 name。
- `class ContextValue(parent, key, value)`（非导出）：`value(k)` 若 `k.token === key.token` 返回自身值，否则委托 parent；`toString()` = `` `${parent}.WithValue(${key.token.description ?? "anonymous"})` ``。
- `BACKGROUND_CONTEXT = new EmptyContext("[Context BACKGROUND_CONTEXT]")`；`TODO_CONTEXT = new EmptyContext("[Context TODO_CONTEXT]")`。
- `createContextKey<T>(description: string)` = `Object.freeze({ token: Symbol(description) })`。
- `withContextValue(key, value, parent)`（**参数顺序：key, value, parent**）。
- `withTelemetry(telemetry, context)`（**参数顺序：telemetry, context**）= `withContextValue(TELEMETRY_CONTEXT_KEY, telemetry, context)`。
- `traceIn(options, fn, context)`：`context.telemetry.startSpan(options, span => fn(withTelemetry(span, context)))`（**fn 在 span 内运行，且 ctx 的 telemetry 换成该 span**）。
- `withAbortSignal(signal, context)`：`parentSignal` 存在 → `AbortSignal.any([parentSignal, signal])`，否则直接用 `signal`；`withContextValue(ABORT_SIGNAL_CONTEXT_KEY, combined, ctx)`。
- `withoutAbortSignal(context)`：`withContextValue(ABORT_SIGNAL_CONTEXT_KEY, undefined, ctx)`（**遮蔽**而非移除）。
- `withCancel(context)` → `{ context: withAbortSignal(controller.signal, context), cancel: (reason?) => controller.abort(reason) }`。
- `awaitWithContext(promise, context)`：`signal === undefined` → 原 promise；已 aborted → `Promise.reject(abortError(signal))`；否则 race，abort 时 reject（**不取消底层 promise**），settle 时移除监听。
- `abortError(signal)`（**不导出**）：`signal.reason instanceof Error ? reason : new DOMException("The operation was aborted", "AbortError")`。

### 2.27 `src/delta/index.ts`（742 行）

**Op 形态（wire 与内存一致，除 `#` 交错）**：
```
["r", value]                          replace whole root
["s", path, value]                    set value at path（对象键，也可以是数组索引）
["d", path]                           delete value at path（对象删键；数组删索引 = splice 1）
["a", path, text]                     append text to the STRING at path
["t", path, count]                    truncate the STRING at path to its first `count` UTF-16 units
["p", path, index, remove, items]     splice: remove `remove` items at `index`, insert `items`
["m", path, permutation]              reorder: new[i] = old[permutation[i]]
```
**注意**：`s`/`d` 的第二元素是**路径数组**（`NonEmptyPath`），不是单个键；`d` 只有 2 个元素（没有 value/key 第三项）。`t` 的 `count` 是"保留下来的长度"，不是"删掉的长度"（`apply` 执行 `value.slice(count)`；diff 产生 `["t", path, before.length - shared]`）。

`WireOp` 额外允许 `["#", id, path]`（定义路径 id）与**省略路径的短形式**：
```
["s", value]            // 与上一条 op 同路径
["d"]
["a", text]
["t", count]
["p", index, remove, items]
["m", permutation]
["#", id]               // 见 decoder：路径 id 由数字 PathRef 引用
```
路径引用（`PathRef`）可以是**内联路径数组**，也可以是**已定义的数字 id**。

函数与规则：
- `isReplace` / `isBase`。
- `overlap(a, b, scan, probe = 64, maxCandidates = 8)`（导出）：`a` 或 `b` 为空或 `scan === 0` → 0。`tail = a.length > scan ? a.slice(a.length - scan) : a`。对 `h` 依次取 `[Math.min(probe, b.length), 1]`：`head = b.slice(0, h)`，在 `tail` 里用 `indexOf(head)` 找候选（逐个 `indexOf(head, k+1)`），每个候选 `n = tail.length - k`，若 `n <= b.length && tail.slice(k) === b.slice(0, n)` → 返回 `n`；候选数 `++tried > maxCandidates` 即放弃该 `h`。两个 `h` 都失败 → 0（发出的会是 `s`：更大但绝不错误）。返回值恒满足 `a.slice(a.length-n) === b.slice(0,n)`。
- `ownedSlice(text, start, end = text.length)`：`` `${text.slice(start, end)} `.slice(0, -1) ``（绕开 V8 的 substring 视图，强制独立字符串；Rust 无此问题，但必须保持"返回等值字符串"）。
- `RESERVED_SEGMENTS: ReadonlySet<string> = new Set(["__proto__","constructor","prototype"])`（**导出**）。
- `UnsafePathError extends Error`：`readonly segment: Seg`；`name = "UnsafePathError"`；消息 `` `unsafe path segment: ${String(segment)}` ``（**`String()` 而非 `JSON.stringify()`**，所以 `__proto__` 不带引号）。
- `assertValidOp(op)`（导出，用于**已解码** op）：首行 `if (!Array.isArray(op) || op.length === 0) throw new TypeError("op is not a tuple")`。随后逐 verb 的逐字消息：`"r arity"`（length ≠ 2）、`"s arity"`（≠3）、`"d arity"`（≠2）、`"a shape"`（≠3 或 `op[2]` 非 string）、`"t shape"`（≠3 或 `op[2]` 非非负整数）、`"p arity"`（≠5）、`"p index"`、`"p remove"`、`"p items"`、`"m arity"`（≠3）、`"path is not an array"`、`"path is empty"`（`s`/`d`/`a`/`t` 的路径必须非空；`p`/`m` 可为空）、`"m permutation is not an array"`、`"m permutation is not a bijection"`（用 `Uint8Array(value.length)` 查重）、`` `unknown op verb: ${String(op[0])}` ``。
- `assertValidWireOp(op)`（导出，用于 wire 语法）：同样的 `"op is not a tuple"`；额外 `"bad path id"`（number ref 必须是非负整数）、`"a value"`、`"t count"`、`"# shape"`（`length !== 3 || !Number.isInteger(op[1]) || op[1] < 0 || !Array.isArray(op[2])`）、`"a arity"`、`"t arity"`；`okRef(r)` 对非数组非数字 → `"path is not an array"`（注释：String 会被 `slice(0,-1)` 静默变成根路径）。`p` 允许 length 5 或 4；`m` 允许 length 3 或 2，且 `assertPermutation(op[op.length - 1])`。
- `assertSafePath(path)`（导出）：string 段命中 `RESERVED_SEGMENTS` → `UnsafePathError`；非 string 段若不是非负整数 → `UnsafePathError`。
- `assertIndexInRange(parent, index)`（**非导出**，注意参数顺序是 `(parent, index)`）：`index > parent.length` → `throw new UnsafePathError(index)`（**不是 `RangeError`**）。注释给出两条理由：稀疏数组不能 JSON 往返（洞序列化为 `null` 回来变真属性）；阻止 `["s", ["xs", 4294967290], 1]` 这类一次 op 分配 43 亿项数组的 DoS。
- `PathError extends Error`：`readonly path: Path | number`；`name = "PathError"`；消息 `` `unresolvable path: ${JSON.stringify(path)}` ``（数字 id 时即 `unresolvable path: 3`）。
- `apply(target, ops)`（导出；**返回新根**，因为 `r` 无法就地替换；转发给内部的 `applyOps`）：
  - 逐条先 `assertValidOp(op)`；`r` → `root = op[1]`（**不复制**，所有权转移；注释明确：同一批次扇出给多个消费者会让副本互相别名，这是所有权规则而非缺陷）；
  - 之后对每个 op 再 `assertSafePath(op[1])`（与 `assertValidOp` 内的重复，但顺序如此）；
  - `p`：目标 = `path.length === 0 ? root : resolve(root, path)`，非数组 → `PathError(path)`；先 `splice(op[2], op[3])`；再按 **10_000 项分块** `splice(op[2] + offset, 0, ...op[4].slice(offset, offset + 10_000))`；
  - `m`：目标 = `path.length === 0 ? root : resolve(root, path)`，非数组或 `length !== perm.length` → `PathError(path)`；`previous = target.slice()`；`target[i] = previous[perm[i]]`；
  - `s`/`d`/`a`/`t`：`parent = resolve(root, path.slice(0,-1))`，`key = path[path.length-1]`；
    - parent 是数组 → key 必须是 number（否则 `UnsafePathError(key)`），且 `assertIndexInRange(parent, key)`（`key > parent.length` → `UnsafePathError`）；
    - 写入用 `Object.defineProperty(parent, key, {value, writable:true, enumerable:true, configurable:true})`（注释：避免原型链上的 setter 被执行）；
    - 读取用 `Object.hasOwn(parent,key) ? parent[key] : undefined`；
    - `s` → write(op[2])；`d` → 数组：`key >= parent.length` → `PathError(path)`，否则 `splice(key,1)`；对象：`delete parent[key]`；
    - `a` → 当前值必须是 string，否则 `PathError(path)`；write(current + op[2])；
    - `t` → 当前值必须是 string，否则 `PathError(path)`；write(current.slice(op[2]))。
- `applyImmutable(target, ops)` = `applyImmutableBatches(target, [ops])`（导出）。`applyImmutableBatches(target, batches)`（导出）：一次 `owned = new WeakSet<object>()` 贯穿所有批次（**只为最终结果服务，不暴露中间修订**）；每个 op：`assertValidOp`；`r` → `root = op[1]`；`p` 先试 `appendCopy`（成功即用其返回值作新 root 并 continue）；否则 `root = copyContainers(root, op[0] === "p" || op[0] === "m" ? op[1] : op[1].slice(0,-1), owned)`，再 `root = applyOps(root, [op])`（**逐条复制后立即应用**）。
  - `appendCopy(root, op, owned)`：要求 `deleteCount === 0`；沿路径走，每段必须"plain own property"（string 段：非保留段、target 是对象且非数组；number 段：非负整数且 target 是数组），否则返回 undefined 走通用路径；目标必须是数组、**未被本轮回拷**（`!owned.has`）、且 `index === target.length`；`appended = target.concat(items)`、`owned.add(appended)`；`path.length === 0` 直接返回 appended；否则 `copiedRoot = copyContainers(root, parentPath, owned)`，再 `defineProperty` 挂到父上，返回 `copiedRoot`。
  - `copyContainers(root, path, owned)`：数组 → `slice()`；`null` 原型对象 → `Object.assign(Object.create(null), value)`；其它对象 → `{...value}`；非对象 → `PathError(path)`。`root` 未 owned 则先复制并 `owned.add`；逐段：必须 `Object.hasOwn`（否则 `PathError(path)`）、数组上的 string 段 → `UnsafePathError`、子节点必须 `isObj`（否则 `PathError(path)`）；已 owned 则直接下潜，否则复制、`defineProperty` 挂上、`owned.add`。
- `resolveValue(root, path)`（**非导出**）：逐段走；节点非对象 → `PathError(path)`；数组上的非 number 段 → `UnsafePathError(seg)`；**只认自有属性**（`Object.hasOwn`，否则 `PathError(path)`；注释：继承的 getter 不得执行，遍历不得逃逸到原型链）。
- `resolve(root, path)`（**非导出**）：`resolveValue` 结果必须 `isObj`，否则 `PathError(path)`。
- `pathKey(path) = JSON.stringify(path)`（**非导出**；与 `PendingOps` 各自的同名局部函数）。
- `interface Encoder { encode(ops: readonly Op[]): WireOp[] }`；`interface Decoder { decode(wire: readonly unknown[]): Op[] }`（**导出类型**）。`encoder()` / `decoder()` 是工厂。
- `encoder()`（返回单方法对象 `{ encode }`；Rust 用 `struct Encoder`）：
  - **状态**：`seen: Set<pathKey>`、`ids: Map<pathKey, id>`、`nextId = 0`、`previous: string | undefined`。
  - **跨批次语义（注释逐字要点）**：同一 encoder 的**连续批次可以被接收方拼成一个批次**（链路就是这样合并等待状态的），因此只有 path id 跨批次，且每个批次的第一条 op 都带显式路径 —— `encode()` 开头 `previous = undefined`；`seen`/`ids`/`nextId` 保留。
  - 逐条：
    1. `r` → `out.push(op)` 原样；随后**清空 `seen`、`ids`、`nextId = 0`、`previous = undefined`**，continue（`r` 是**恢复点**：读者从最后一个 base 用全新 decoder 重放，保留 id 会引用读者从未见过的定义 → 恢复失败）。
    2. `key = pathKey(path)`；若 `key === previous` → 输出**短形式**（`["s", v]` / `["d"]` / `["a", text]` / `["t", n]` / `["p", index, remove, items]` / `["m", perm]`），continue（**不更新 `previous`**，它已等于 key）。
    3. 否则：`ids` 命中 → `ref = id`；未命中但 `seen` 命中 → `id = nextId++`、`ids.set(key,id)`、**先** `out.push(["#", id, path])`、`ref = id`；否则 `seen.add(key)`、`ref = path`（内联）。
    4. 输出 `[verb, ref, ...payload]`；`previous = key`。
  - 因此 `#` 在**第二次**使用该路径时才发出（intern on second use；定义比它替代的路径更贵，首次即定义在"只写一次的路径"上亏）。
- `decoder()`（状态 `paths: Map<number, Path>`，**跨调用保留**；`previous` 是 `decode` 内的局部变量，每批重置）：
  - 逐条 `assertValidWireOp(op)`。
  - `#` → `assertSafePath(op[2])`、`paths.set(op[1], op[2])`、continue（**不产生 op**）。
  - `r` → `out.push(op)`、`paths.clear()`、`previous = undefined`、continue。
  - `short` 判定（**按 arity**，逐字）：`(op[0] === "d" && op.length === 1) || (op[0] !== "d" && op[0] !== "p" && op.length === 2) || (op[0] === "p" && op.length === 4)`。
  - `short`：`previous === undefined` → `throw new PathError([])`（消息 `unresolvable path: []`）；否则 `path = previous`。
  - 非 short：`ref = op[1]`；number → `paths.get(ref)`，缺失 → `PathError(ref)`；数组 → 直接用它；随后 `previous = path`。
  - `op[0] !== "p" && op[0] !== "m" && path.length === 0` → `PathError(path)`（`s`/`d`/`a`/`t` 不能打根）。
  - 组回 `Op`；短形式时 payload 在 `op[1..]`，长形式在 `op[2..]`（`p` 分别为 `[op[1],op[2],op[3]]` / `[op[2],op[3],op[4]]`）。
  - **注释逐字要点**：一个独立的 state stream 必须用一对独立 codec；共享同一条传输连接不会让分别 hydrate 的状态变成一条流。

### 2.28 `src/delta/diff.ts`（525 行）

常量（均**非导出**）：`DEFAULT_OVERLAP_SCAN = 65_536`、`MAX_DELTA_OPERATIONS = 4_096`、`MAX_IDENTITY_CANDIDATES = 200_000`、`MAX_SEMANTIC_CELLS = 65_536`、`overflowedBatches = new WeakSet<Op[]>()`。

**溢出机制（逐字语义）**：`emitOperation(operations, operation)`：
```ts
if (overflowedBatches.has(operations)) return;
if (operations.length >= MAX_DELTA_OPERATIONS) { overflowedBatches.add(operations); return; }
operations.push(operation);
```
即第 4097 条 op 不再压入，而是把批次标记为溢出；此后所有 emit 与大部分递归都以 `overflowedBatches.has(operations)` 提前返回（`diffArray` 的 `processArrayMatches` 循环、`diffObject` 的两个键循环、`diffArrayRegion` 入口均检查）。`emitSet(path, value, out)`：`path.length === 0` → `["r", value]`，否则 `["s", path, value]`。

辅助函数：`isContainer(v)`（`v !== null && typeof v === "object"`）、`equalJson`（深比较；数组比长度逐项，对象比 `Object.keys` 长度 + `Object.hasOwn` + 逐键）、`sameValue`、`permutation(a, b)`（返回 `number[]` 或 undefined，必须构成双射）、`lowerBound`、`lcsMatches`、`semanticallyAligned`、`identitySubsequence`、`greedyIdentityAnchors`、`identityAnchors`（LIS）。

`emitString(before, after, path, out)`：
- 相等 → 无；
- `after` 以 `before` 为前缀且更长 → `["a", path, ownedSlice(after, before.length)]`；
- 否则 `shared = overlap(before, after, DEFAULT_OVERLAP_SCAN)`；`shared === 0` → `["s", path, after]`；
- 否则先 `["t", path, before.length - shared]`，若 `after.length > shared` 再 `["a", path, ownedSlice(after, shared)]`。

`diffValue(before, after, path, out)` 分派顺序：相同或已溢出 → 无；两边都是 string 且 `path.length > 0` → `emitString`；两边都是数组 → `diffArray`；两边都是非数组容器 → `diffObject`；否则 `emitSet(path, after)`。

`diffArray(before, after, path, out)`（逐字顺序）：
```ts
if (before === after || equalJson(before, after)) return;
if (before.length === after.length && before.length > 1 &&
    !sameValue(before[0]!, after[0]!) && !sameValue(before[before.length-1]!, after[after.length-1]!)) {
  const order = permutation(before, after);
  if (order !== undefined) { emitOperation(out, ["m", path, order]); return; }
}
diffArrayRegion(before, after, path, out, 0, before.length, 0, after.length, 0);
```
注意 `equalJson` 深比较在最前（**整数组深等价 → 零 op**，这是"重排的深等价对象归一化为 no-op"的基础）；置换只在**等长、长度 > 1、首尾都不同**时尝试；`diffArray` 本身不调用 identity 锚点。

`diffArrayRegion(before, after, path, out, beforeStart, beforeEnd, afterStart, afterEnd, outputStart)`：
1. 已溢出 → 返回；
2. trim 公共前缀（`sameValue`，同时 `outputStart++`）与公共后缀（不推进 `outputStart`）；
3. 两侧都空 → 返回；
4. 只有一侧有内容（`beforeCount === 0 || afterCount === 0`，含"纯插入"）→ 单条 `["p", path, outputStart, beforeCount, after.slice(afterStart, afterEnd)]`；
5. 等长 → 收集逐位相等的 `positional: ArrayMatch[]`；非空则 `processArrayMatches(..., positional)` 并返回；
6. 子序列匹配 `identitySubsequence(...)` 非空 → `processArrayMatches` 并返回；
7. identity 锚点 `identityAnchors(...)` 非空 → `processArrayMatches` 并返回；
8. 语义 LCS `lcsMatches(before.slice(...), after.slice(...), semanticallyAligned, MAX_SEMANTIC_CELLS)` 非空 → 映射回绝对下标后 `processArrayMatches` 并返回；
9. `beforeCount === 1 && afterCount === 1` → `diffValue(before[beforeStart], after[afterStart], [...path, outputStart])`；
10. 否则整体 `["p", path, outputStart, beforeCount, after.slice(afterStart, afterEnd)]`。

`processArrayMatches`：对每个匹配，先对 `[beforeAt, beforeMatch)` × `[afterAt, afterMatch)` 调 `diffArrayRegion`（`outputStart = outputAt`），`outputAt += afterMatch - afterAt`；匹配本身若 `!sameValue` 则 `diffValue([...path, outputAt])`；`outputAt += 1`；推进 `beforeAt/afterAt`。收尾再对剩余区间调一次 `diffArrayRegion`（每步前检查溢出）。

`diffObject(before, after, path, out)`：
- **保留段守卫**：`const beforeKeys = Object.keys(before); const afterKeys = Object.keys(after);` 若 `[...beforeKeys, ...afterKeys].some(key => RESERVED_SEGMENTS.has(key))` → 若 `!equalJson(before, after)` 则 `emitSet(path, after, out)`（即含 `__proto__`/`constructor`/`prototype` 键的对象**永不逐键 diff**），然后返回。
- 遍历 `afterKeys`（源顺序）：`Object.hasOwn(before, key)` → `diffValue([...path,key])`，否则 `emitSet([...path,key], after[key])`。
- 遍历 `beforeKeys`：`!Object.hasOwn(after,key)` → `["d", [...path,key]]`。
- 两个循环每条前检查溢出。

`jsonCost(value)`：`null` → 4；string → `length + 2`；number → `String(value).length`；boolean → `true` 4 / `false` 5；数组 → `2 + Σ(itemCost + (i===0?0:1))`；对象 → `2 + Σ(key.length + 3 + valueCost + (i===0?0:1))`（按 `Object.keys` 顺序）。
`pathCost(path)`：`2 + Σ((string ? len+2 : String(seg).length) + (i===0?0:1))`。
`operationCost(op)`：`r` → `6 + jsonCost(v)`；`s` → `7 + pathCost + jsonCost(v)`；`d` → `6 + pathCost`；`a` → `7 + pathCost + text.length + 2`；`t` → `7 + pathCost + String(n).length`；`p` → `10 + pathCost + String(index).length + String(remove).length + jsonCost(items)`；`m` → `7 + pathCost + jsonCost(permutation)`。

`diffRevisions(before, after)`（导出，**逐字顺序**）：
```ts
const operations: Op[] = [];
diffValue(before, after, [], operations);
if (overflowedBatches.has(operations)) return [["r", after]];
if (operations.length === 0 || operations[0]?.[0] === "r") return operations;
let deltaCost = 2;
for (const operation of operations) deltaCost += operationCost(operation) + 1;
if (deltaCost < 65_536) return operations;
const snapshotCost = jsonCost(after) + 6;
return deltaCost >= snapshotCost ? [["r", after]] : operations;
```
注意：op 数达上限时**不是**"op 数 > 4096 才退化为 r"，而是第 4097 条 emit 触发溢出标记，最终由 `overflowedBatches.has` 决定返回 `[["r", after]]`；成本比较只在 `deltaCost >= 65_536` 时才做，且等号时取快照。

### 2.29 `src/delta/pending.ts`（173 行）— PendingOps 折叠

```ts
export type PendingTake = { readonly kind: "ops"; readonly ops: readonly Op[] } | { readonly kind: "replace" };
export const MAX_PENDING_OPS = 10_000;
type LeafOp = readonly ["a", NonEmptyPath, string] | readonly ["s", NonEmptyPath, JsonValue];
type Folding = { readonly kind: "folding"; readonly ops: Op[]; readonly leaves: Map<string, number> };
type State = {kind:"empty"} | {kind:"batch"; ops: readonly Op[]} | Folding | {kind:"replace"};
```
注意：`take()` 的 replace 分支**不携带值**（`REPLACE_TAKE = {kind:"replace"}`），owner（`watch.ts`）自己用当前值造 `[["r", value]]`；`PendingTake.ops` 是 `Op[]` 而非 `WireOp[]`（等待期间从不编码）。

状态机 `empty → batch(ops) → folding → replace`，但入口规则更细：
- `push(ops)`：`ops.length === 0` → 什么都不做（**不改变状态**）。
- **`ops[0][0] === "r"`**（tracker 与 `diffRevisions` 唯一会放 `r` 的位置）→ 直接 `state = {kind:"batch", ops}`，即**丢弃一切 pending（包括已溢出状态），并保留该批次原样**。这是唯一能把 `replace` 状态救回 `batch` 的路径。
- 否则按当前状态：`empty` → `{kind:"batch", ops}`（**按引用保存**，跟得上的消费者不付折叠成本）；`batch` → 新建 `Folding`，先 `#fold(state.ops, folding)`，成功再 `#fold(ops, folding)`；`folding` → `#fold(ops, state)`；`replace` → 直接 return（不再累积）。

`#fold(ops, folding): boolean`（返回 false 表示已溢出）逐条处理：
- `r` → `folding.ops.length = 0; folding.leaves.clear();`，然后 push 该 op；
- `a` → `mergeLeaf(op, folding)` 成功则 continue（**不 push**），否则落到 push；
- `s` → 若 `isPrimitive(op[2])`：`mergeLeaf` 成功则 continue；否则 `folding.leaves.size > 0 && leaves.clear()`（**非原始值 set 清空叶索引**）；
- 其它（`d`/`t`/`p`/`m`）→ `leaves.size > 0 && leaves.clear()`；
- push 后 `if (folding.ops.length > MAX_PENDING_OPS) { this.#state = REPLACE; return false; }`（**严格大于**，即 10001 条时溢出）。

`mergeLeaf(op, folding)`：`key = JSON.stringify(path)`；`at = leaves.get(key)`；命中则 `merge(folding.ops[at] as LeafOp, op)`，成功 → 替换原位并返回 true；否则 `leaves.set(key, folding.ops.length)` 返回 false（调用方随后 push）。
`merge(previous, next)`（同路径折叠规则）：
- `next[0] === "s"` → **直接返回 next**（set 覆盖任何先前叶 op，包括 `a`）；
- `previous[0] === "a"` → `["a", previous[1], previous[2] + next[2]]`（两个 append 拼成一个）；
- `typeof previous[2] === "string"` → `["s", previous[1], previous[2] + next[2]]`（字符串 set 吸收其后的 append）；
- 否则 undefined（不折叠）。

`get size()`：`empty` → 0；`replace` → 1；否则 `state.ops.length`。`take()`：`empty` → `{kind:"ops", ops: []}`（常量 `NO_OPS`）；`batch`/`folding` → `{kind:"ops", ops: state.ops}`；`replace` → `{kind:"replace"}`；随后 `state = EMPTY`（可继续使用）。

注释中的语义保证（Rust 侧必须保留）：叶 op 在不同路径上可交换，但**祖先路径上的原始值 `s` 之后**，后代路径在下一条结构 op 清空索引前不能再写；`r` 丢弃它之前的一切；收到/传入的 op 数组与元组**从不被原地修改**（合并产生新元组）。

### 2.30 `src/delta/tracker.ts`（2292 行）— ES Proxy 事务式 overlay 草案

同目录的 `draft.ts`（10 行，**纯类型**）：
```ts
export type Draft<T, Depth extends readonly unknown[] = []> =
  Depth["length"] extends 8 ? T
  : T extends null | boolean | number | string ? T
  : T extends (...args: never[]) => unknown ? T
  : T extends object ? { -readonly [Key in keyof T]: Draft<T[Key], [...Depth, unknown]> }
  : T;
```
即"把只读类型变可写、最多下潜 8 层、函数与原始值原样"。Rust 无对应物（显式 `Draft` API，见 §8.4 第 1 条与 §8.6 第 3 条）。

这是全包最复杂的模块。核心数据结构与公开面（**逐字**）：

```ts
export type StringKeys<T> = T extends readonly unknown[] ? never
  : { [K in keyof T]: string extends T[K] ? (T[K] extends string ? K : never) : never }[keyof T];

export interface Prepared<T extends object> {
  readonly base: T;               // 基线值
  readonly value: T;              // 提交后的值
  readonly ops: readonly Op[];
  readonly baseRevision: number;
  abort(): void;
}
export interface Change<T extends object> {
  readonly state: Draft<T>;
  prepare(): Prepared<T>;
  abort(): void;
}
export interface Tracker<T extends object> {
  readonly value: T;
  readonly revision: number;
  beginChange(): Change<T>;
  prepareReplace(value: T): Prepared<T>;
  adopt(prepared: Prepared<T>): void;
}
export function track<T extends object>(initial: T): Tracker<T>;   // O(1)，接管所有权
export function appendText<T extends object>(draft: Draft<T>, key: StringKeys<T>, text: string): void;
```
内部常量：`NODE = Symbol("chord.delta.overlay.node")`、`PREPARED = new WeakMap<object, OverlayContext>()`、`RELEASED: Record<string, JsonValue> = {}`、`ARRAY_MUTATORS`（`push,pop,shift,unshift,splice,reverse,sort,fill,copyWithin`）、`MAX_DELTA_OPERATIONS = 4_096`、`MAX_SIMPLE_OBJECT_NODES = 128`、`DATA_DESCRIPTOR = {value:undefined,writable:true,enumerable:true,configurable:true}`。

内部类型（逐字，Rust 结构体应对应）：
```ts
type Status = "open" | "prepared" | "consumed" | "aborted" | "stale";
type InsertSource = { refs: StoredRef[] };
type ParentKind = 0 | 1 | 2;   // object | base-array entry | inserted-array entry
type BasePiece = { kind: "base"; start: number; length: number; step: 1 | -1 };
type InsertPiece = { kind: "insert"; source: InsertSource; start: number; length: number; step: 1 | -1 };
type PieceNode = { piece; left; right; priority: number; elements: number };          // treap
type PieceLocation = { piece; logicalStart; minimum; maximum };
type DenseRegion = { start: number; length: number };
type DenseCandidates = { indices: number[]; bits: Uint8Array | undefined; length: number };
type ArrayPlan = { removeRuns: number[]; permutation: number[] | undefined; insertRuns: number[] };
type ArrayOverlay = { root: PieceNode|undefined; pieces: Piece[]|undefined; baseOverrides; insertOverrides;
                      structural: boolean; generation: number; plan: ArrayPlan|undefined; seed: number;
                      locatedOffset: number; baseLocations; insertLocations };
type OverlayNode = { context; baseIndex; parentIndex; parentKind; parentKey; parentSource?; parentPlacement;
                     target: object; proxy: object; writeKey?; writeValue?; writes?; deleteKey?; deletes?;
                     readded?; appends?: Map<string,string>; array?: ArrayOverlay; dirty?; subtreeDirty?; preparedPath? };
type OverlayContext = { owner: object; tracker: WeakRef<TrackerImpl>; baseRevision; status: StatusCell;
                        root: OverlayNode|undefined; bases; stored; dirty: OverlayNode[]; nodes: OverlayNode[];
                        rawNodes?: WeakMap<object, OverlayNode>; ops: Op[]|undefined;
                        replacement: boolean; replacementNoop: boolean; baseValue; overlayReleased: boolean;
                        simpleObjectMaterialization: boolean; registryRef: WeakRef<OverlayContext>|undefined };
```

`PreparedImpl`：字段 `value`、`base`（= `context.baseValue`）、`ops`；`get baseRevision()`；`abort()` 调 `abortContext`；构造时 `PREPARED.set(this, context)`。

`ChangeImpl`：
- `get state`：`context.root?.proxy` 不存在 → `TypeError("Cannot use a settled overlay")`。
- `prepare()`：已 settled → `Error("Change has already been settled")`；`assertWritable`；`status = "prepared"`；非 replacement 时 `ops = dirty.length === 0 ? [] : emitOperations(context)`；`materializePrepared`；成功记录 `#preparedStatus`、settled；失败 → `status = "aborted"`、`clearContext`、settled、重抛；`finally { #context = undefined }`。
- `abort()`：已 settled → 若 `#preparedStatus` 为 `"prepared"` 则改为 `"aborted"`，返回；否则 settled、`abortContext`。
- **注意**：`Change.state` 的 `Draft` 是 Proxy；settle 后访问即 TypeError。

`TrackerImpl`：
- 字段 `#owner = {}`、`#contexts: Set<WeakRef<OverlayContext>>`、`#selfRef: WeakRef<TrackerImpl>`、`#value`、`#revision = 0`、`#pruneBudget = 256`。
- `get value`、`get revision`。
- `beginChange()`：`createContext(selfRef, owner, revision, value, false, value)` → 注册 → `new ChangeImpl`。
- `prepareReplace(value)`：`createContext(..., value, true, this.#value)`（`replacement = true`），`status = "prepared"`，注册，`materializePrepared`；失败 → aborted + clear + 重抛。
- `adopt(prepared)` 校验顺序与文案（**逐字**）：
  1. `context?.owner !== #owner` → `Error("Prepared change belongs to a different tracker")`
  2. `status === "consumed"` → `Error("Prepared change has already been used")`
  3. `status === "aborted"` → `Error("Prepared change has been aborted")`
  4. `status === "stale"` → `Error("Prepared change is stale")`
  5. `status !== "prepared"` → `Error("Prepared change is not ready")`
  6. `context.baseRevision !== #revision` → 标 stale、clear、`Error("Prepared change is stale")`
  7. `#value !== prepared.base` → 标 stale、`Error("Prepared change is stale")`
  8. 成功：`#value = prepared.value`、`status = "consumed"`、`#revision += 1`、`#invalidate(context)`（把其它所有 open/prepared context 标 stale 并 `releaseOverlayReferences`，然后 `#contexts.clear()`、`#pruneBudget = 256`）。
- `releaseContext(context)`：从 `#contexts` 删除 `registryRef`。
- `#register`：`#pruneBudget -= 1`，为 0 时 `#prune()` 并重置为 `max(256, #contexts.size)`；`#prune()` 删除已 GC 的 WeakRef。
- `replacementBase(context)`：`context.baseRevision === #revision ? #value : undefined`。

`appendText(draft, key, text)`：
1. `draft[NODE]` 不是 OverlayNode → `TypeError("appendText() needs a draft")`；
2. `assertWritable`；
3. `current`：数组 base 或无该键 → undefined，否则 `objectValue`；
4. 非 string → `` TypeError(`appendText() needs a string at ${JSON.stringify(name)}`) ``；
5. `text.length === 0` → 返回（**不记录 op**）；
6. `owned = ownedSlice(text, 0)`；`appended = node.appends?.get(name) ?? (hasObjectWrite(node,name) ? undefined : "")`；
7. `setObjectWrite(node, name, current + owned)`；`markDirty`；若 `appended !== undefined` 则 `node.appends.set(name, appended + owned)`（**同一 change 内多次 append 合并成一个 op**）。

Proxy handler（`sharedObjectHandler` 与 `sharedArrayHandler` **是同一个对象**；逐条列出全部 trap，全部可观察）：
- 公共前置：`nodeForTarget(target)` 通过隐藏键 `target[NODE]` 取 node，取不到 → `TypeError("Cannot use a settled overlay")`；`isSettledContext(c) = c.overlayReleased || c.status.value ∈ {"consumed","aborted","stale"}`；`assertReadable` → settled 抛 `TypeError("Cannot use a settled overlay")`；`assertWritable` = `assertReadable` + `status.value !== "open"` → `TypeError("Prepared overlays are read-only")`。
- `get(target, property)`：**先特判 `property === "then"`** —— 若 `target[NODE]` 不存在或 context 已 settled，直接返回 `undefined`（让 draft 在 `await`/Promise 解析下不被当作 thenable）；否则 `node = nodeForTarget(target)`，`property === NODE` 返回 node；真正读取走 `getProperty(node, property)`（内部 `assertReadable`）：
  - **数组**：`"length"` → `arrayLength(overlay)`；mutator 名（`ARRAY_MUTATORS` 命中）→ `arrayMutators[name]`（绑定到 proxy 的函数）；合法索引 → `getArrayIndex(node, index)`（越界 → `undefined`；否则 `locatePiece` 定位后 `entryValueAt`；容器值 → **新建子 node 并返回其 proxy**，`parentKind` 为 base entry(1)/insert entry(2)，insert 来源带上 `parentSource`，`parentPlacement = piece.kind === "insert" || hasEntryOverrideAt(...)`）；其它键 → **`Reflect.get(Array.prototype, property, node.proxy)`**（所以 `draft.map` 是 Array.prototype 上的泛型方法，receiver 是 proxy，`this.length` 读 overlay 长度）。
  - **对象**：符号键 → `Reflect.get(nodeBase(node), property, node.proxy)`；字符串键：若 `!objectHas(node,key)`，则 `isObjectDeleted(node,key) || Object.hasOwn(base,key)` → `undefined`，否则 `Reflect.get(base, key, proxy)`（原型链 getter 会被执行、receiver 是 proxy）；若 `objectHas` 命中 → `objectValue(node, key)`，容器值同样包装成子 node proxy（`parentKind = 0`，`parentKey = key`，`parentPlacement = hasObjectWrite(node,key)`）。
- `set(target, property, value)` → `setProperty(nodeForTarget(target), property, value)`：`assertWritable`；符号键 → `TypeError("Symbol writes are not supported")`；
  - **数组**：`"length"` → `setArrayLength(toArrayLength(v))`（`toArrayLength` 拒绝非整数、负数、`>= 4_294_967_296` → `RangeError("Invalid array length")`），返回 true；其它属性必须是合法数组索引，否则 `TypeError("Only array indices and length can be written")`；`index > arrayLength(overlay)` → `TypeError("Overlay arrays cannot contain holes")`（**等于长度是允许的 append**）；否则 `setArrayIndex(node, index, clonePlacement(v))`，返回 true。
  - **对象**：`v === undefined` → 转 `deleteProperty(node, key)`；否则 `stored = clonePlacement(v)`（按值放置：对象/数组被深克隆）；**短路优化**：若该键未被删过、`stored` 不是容器、且 `current === stored`（同一原始值）→ 直接返回 true（不记录写入）；否则 `node.appends?.delete(key)`、`setObjectWrite(node, key, stored)`，若之前删过且 base 自有该键 → 加入 `readded`；`deleteObjectDeletion(node, key)`；`markDirty(node)`；返回 true。
- `deleteProperty`：`assertWritable`；符号键 → `TypeError("Symbol writes are not supported")`；**数组一律** `TypeError("Overlay arrays cannot contain holes")`（数组元素只能通过 mutator/splice 删除）；对象：不存在（既非 write 也非 base 自有）→ true；否则删 write、清 appends/readded、记 deletion、`markDirty`、true。
- `has` → `hasProperty(node, property)`（`assertReadable`）：**数组**：`"length"` → true；合法索引 → `index < arrayLength(overlay)`；其它（含符号键）→ **`property in Array.prototype`**（所以 `"push" in draft` 为 true，这是可观察的）；**对象**：符号键 → `property in nodeBase(node)`（查原型链）；字符串键 → `objectHas(node, key)`（write/readded/deletion 覆盖后的自有键判断）。
- `ownKeys` → `ownKeys(node)`（`assertReadable`）：**数组**直接返回 `["0","1",…,"length"]`（长度为 overlay 长度 + 1，全部字符串、无符号）。**对象**：先判断 `existingKeysOnly`（无任何 deleteKey/deletes/readded，且 writeKey 未定义或 base 自有之，且 writes 的每个键 base 都自有）；成立 → 直接返回 `Object.keys(nodeBase(node))`。否则：`keys = Object.keys(base).filter(k => !isObjectDeleted(node,k) && !readded.has(k))`，再按序 push `writeKey`（若不在）与 `writes` 的每个新键；最后把所有键分成「可解析为数组索引的」与「其余」，索引按数值升序在前（`indices.map(String)`），其余保持收集顺序 —— 结果**恒为 `string[]`**（不返回符号键）。
- `getOwnPropertyDescriptor` → `getDescriptor(node, property)`：`assertReadable`；**数组**：`length` → 先把真实 target 数组的 `length` 设为 overlay 长度，再返回 `Reflect.getOwnPropertyDescriptor(node.target, "length")`，即原生 `{value: 长度, writable: true, enumerable: false, configurable: false}`（注意这里 `writable` **恒为 true**，与下面的一般分支不同）；非索引或索引 ≥ overlay 长度 → **`undefined`**；否则一般分支。**对象**：符号键或 `!objectHas(node, String(property))` → `undefined`。一般分支返回 `{configurable: true, enumerable: true, writable: status.value === "open", value: getProperty(node, property)}`。
- `getPrototypeOf` → `assertReadable` + `Object.getPrototypeOf(nodeBase(node))`（保留 null 原型）。
- `isExtensible` → `assertReadable` + 恒 `true`。
- `defineProperty` → `assertWritable` 后 `TypeError("Defining overlay properties is not supported")`。
- `preventExtensions` → `assertWritable` 后 `TypeError("Overlays cannot be made non-extensible")`。
- `setPrototypeOf` → `assertWritable` 后 `TypeError("Changing an overlay prototype is not supported")`。
- **无** `apply`/`construct` trap（draft 不是函数）。

数组 overlay（treap 分段）：
- 段：`base {start,length,step}` 或 `insert {source,start,length,step}`；`step: 1 | -1` 支持 reverse；
- 优先级用 xorshift（13/17/5）；`seed = 0x9e3779b9`；
- `merge`/`split`/`joinNormalized`/`mergeablePieces`/`flatten`/`piecesOf`/`treeFromPieces`/`replaceAllPieces`；
- `arrayLength`、`locatePiece`（越界 → `RangeError("Array overlay index is out of range")`）、`arrayIndex(property)`（**逐字规则**：非 string → undefined；长度 0 或 > 10 → undefined；`"0"` → 0；首字符必须是 `'1'..'9'`；逐位必须 `'0'..'9'`；累计值 `>= 4_294_967_295` → undefined；**注意 `"00"`/`"01"` 都被拒**）、`entryValueAt`、`hasEntryOverrideAt`；
- `extendRightmostPiece`（push 快路径）、`invalidatePieceCaches`、`replacePieceRange`、`mergePieces`、`insertPiece`、`insertPlacementPiece`；
- `setArrayIndex`、`setArrayLength(node, next)`（相等则 no-op；缩短 → `replacePieceRange(node, next, current-next, [])`；**增长 → 用 `Array.from({length: gap}, () => null)` 显式填 `null`**，即空洞变真值 `null`，与 `assertIndexInRange` 的注释一致）、`toArrayLength`（`RangeError("Invalid array length")`）、`toIntegerOrInfinity`（`NaN`/`0` → 0；有限 → `Math.trunc`；`±Infinity` 原样）、`clampIndex`（`-Infinity` → 0；负 → `max(length+value,0)`；否则 `min(value,length)`）；
- `mutatorNode(receiver)`：非容器或读不到 `NODE` → **undefined**（此时 mutator 通过 `Reflect.apply(Array.prototype.X, this, args)` 回落到原生行为，这是「借用泛型 receiver」路径）；读到 node 但 base 不是数组 → `TypeError("Array mutator called on incompatible receiver")`；否则 `assertWritable(context)` 后返回 node；
- `arrayMutators`：`push/pop/shift/unshift/splice/reverse/sort/fill/copyWithin` 的**全部 JS 语义**；
- `sort(comparator)`：非函数 → `TypeError("Comparator must be a function")`；遵循 JS 稳定排序、`undefined` 恒排末尾、`NaN` 视为相等；`sortTokenSource`/`Index`/`publicSortValue`/`restoreSortOverride`/`sameSortTokenAt`/`deduplicateArrayEntries`/`piecesFromSortOrder`/`appendMergedPiece`/`singletonPiece`。

克隆：`clonePlacement`、`cloneNode`、`cloneStored`、`clonePlacementNode`、`clonePlacementStored`、`defineData`。

Op 生成：
- `emitOperations(root)`：
  - **dense region**：数组候选位图（> 256 元素启用 `bits`；否则 `indices`）；区域数 ≥ 256 且 `count * 2 >= length` 时按 region 记录位置；`locateDenseArrayPosition`/`hasCoveringDenseRegion`/`addDenseCandidate`/`recordDenseArrayPosition`/`buildDenseRegions`/`regionContaining`；
  - `forcedFolds`：保留键（`__proto__` 等）强制折叠父对象；`hasReservedMutation`/`hasFoldedAncestor`/`hasPlacementAncestor`；
  - 按深度分桶；
  - op 数 > `MAX_DELTA_OPERATIONS` → `[["r", cloneNode(root)]]`。
- `emitSimpleObjectOperations(root)`：节点数 ≤ 128，按深度**插入排序**；否则走通用路径。
- `emitObjectWrite`/`emitObjectOperations`：写 → `s`；删 → `d`；删除后再读回 → 显式 `s`。
- 数组计划：`buildArrayPlan`/`orderedArrayPlan`/`elementArrayPlan`/`insertRunsOf`/`rangeLength`/`emitArrayOperations`/`cloneArrayRegion`。
- `emitChangedValue`（见 §2.28）、`emitSet`、`resolvePath`、`ensurePieceLocations`、`findEntryIndex`、`ensureOperations`（替换为 noop 用 `equalTrustedJson`）、`abortContext`、`releaseOverlayReferences`、`clearContext`、`isContainer`。
- 物化：`materializePrepared` — `operations.length === 0` → `base`；否则 `simpleObjectMaterialization` → `cloneNode(context.root)`，否则 `materializeOperations`（有 `p`/`m` 用 `applyImmutable`，否则 `applyImmutableTrusted`）。

Rust 移植建议：不要复刻 ES Proxy；改为显式 `Draft` API（`get/set/remove/keys/len/iter`）并在其上生成**相同**的 op 序列。

### 2.31 `src/delta/apply-immutable-trusted.ts`（125 行）

`applyImmutableTrusted<T>(target: T, operations: readonly Op[]): T`（导出）：**完全不校验 op**（名称即承诺：调用方自产、可信），逐条处理，每个被触碰的容器只复制一次。
- 模块常量 `DATA_DESCRIPTOR = { value: undefined, writable: true, enumerable: true, configurable: true }`（复用同一个 descriptor 对象，`defineData` 前后改 `value`）。
- `r` → `root = operation[1]`，continue。
- `p` / `m` → `copied = copyPath(root, op[1], op[1].length, owned)`；`root = copied.root`；`copied.target` 非数组 → `TypeError("Trusted array operation target is not an array")`；`p` → `spliceTrusted(target, op[2], op[3], op[4])`；`m` → `permuteTrusted(target, op[2])`。
- `s`/`d`/`a`/`t` → `copied = copyPath(root, path, path.length - 1, owned)`（**只复制到父级**）；`key = path[path.length-1]`；
  - `s` → `defineData(target, key, op[2])`；
  - `d` → 数组：key 非 number → `TypeError("Trusted array deletion key is not numeric")`，否则 `spliceTrusted(target, key, 1, [])`；对象：`Reflect.deleteProperty(target, key)`；
  - `a` → `read(target,key)` 非 string → `TypeError("Trusted append target is not a string")`；否则 `defineData(target, key, current + op[2])`；
  - `t` → `read(target,key)` 非 string → `TypeError("Trusted truncate target is not a string")`；否则 `defineData(target, key, current.slice(op[2]))`。
- `copyPath(root, path, length, owned)`：`root` 非容器 → `TypeError("Trusted operation root is not a container")`；未 owned 则 `shallowCopy` + `owned.add`；逐段（`index < length`）`child = read(destination, segment)`，非容器 → `TypeError("Trusted operation path is not a container")`；已 owned 直接下潜，否则复制、`defineData` 挂上、`owned.add`。返回 `{root: copiedRoot, target: destination}`。
- `shallowCopy(value)`：数组 → `Array.from(value)`；`null` 原型对象 → `Object.assign(Object.create(null), value)`；其它 → `{...value}`。
- `read(target, key)`：直接 `target[key]`（**不查 `hasOwn`**，可读到继承值），与 `apply` 的 `Object.hasOwn` 读取不同。
- `defineData(target, key, value)`：复用 `DATA_DESCRIPTOR` 的 `Object.defineProperty`。
- `spliceTrusted(target, start, remove, items)`：`delta = items.length - remove`；`delta > 0` → 先 `target.length = oldLength + delta`，再 `copyWithin(start + items.length, start + remove, oldLength)`；`delta < 0` → 先 `copyWithin(...)`，再 `target.length = oldLength + delta`（**长度截断放最后**）；`delta === 0` → 不移动。然后用 `defineData` 逐项写入 `items`（按 `start + index`）。
- `permuteTrusted(target, permutation)`：`target.length !== permutation.length` → `TypeError("Trusted permutation length mismatch")`；`previous = target.slice()`；`target[index] = previous[permutation[index]]`（**直接赋值**，不是 `defineData`）。

Rust：等价于对 `Vec`/`IndexMap` 的 `splice` + `copy_within`；`copyWithin` 的语义（重叠区间 memmove）用 `Vec::copy_within` 对应。

### 2.32 `src/delta/revision-validator.ts`（75 行）

`class JsonRevisionValidator`（导出）：一个**可复用**实例，`#validated = new WeakSet<object>()` 记住已经校验过的容器（跨调用复用，避免重复遍历）。

`validate<T extends JsonValue>(value: T): T` → `#validate(value, new Set<object>(), new WeakSet<object>())`（每次调用新的 `ancestors` 与 `finished`）。

`#validate(value, ancestors, finished)` 逐字逻辑：
1. 非容器（`typeof value !== "object" || value === null`）→ `assertPrimitive(value)` 并返回；
2. **已 `#validated`** → 直接返回该值；
3. `finished.has(value)` → 返回（同一修订内的 DAG 共享节点只验一次）；
4. `ancestors.has(value)` → `TypeError("Replicated state cannot contain cycles")`；
5. `ancestors.add(value)`，`try { … } finally { ancestors.delete(value) }`；
6. 数组：`assertDenseArray(value)`，然后逐项递归；
7. 对象：`assertPlainObject(value)`，遍历 `Reflect.ownKeys(value)`：symbol 键 → `TypeError("Replicated state cannot contain symbol properties")`；`descriptor === undefined || !descriptor.enumerable || !("value" in descriptor)` → `TypeError("Replicated state objects must contain enumerable data properties")`；否则递归 `descriptor.value`；
8. 成功 → `finished.add(value)`、`#validated.add(value)`、返回 value。

`assertPrimitive(value)`：`null` / string / boolean → 通过；number 且 `Number.isFinite` → 通过；其它（含 `undefined`、NaN、Infinity、bigint、function）→ `TypeError("Replicated state values must be strict JSON")`。

`assertPlainObject(value)`：原型必须是 `Object.prototype` 或 `null`，否则 `TypeError("Replicated state containers must be plain objects or arrays")`。

`assertDenseArray(value)`：
- `keys = Reflect.ownKeys(value)`；`keys.length !== value.length + 1 || keys.some(k => typeof k !== "string")` → `TypeError("Replicated state arrays must be dense and contain only indexed entries")`（`+1` 是 `length` 自身）；
- 逐索引：`descriptor === undefined || !descriptor.enumerable || !("value" in descriptor) || descriptor.value === undefined` → `TypeError("Replicated state arrays must contain enumerable indexed data properties with defined values")`。

Rust 对应：一个带 `HashSet<*const>`/索引的递归校验器；`undefined` 不能存在于 `serde_json::Value`，所以对象里的 `undefined` 值在 JS 侧就是"strict JSON"错误（**不是**删除键语义），Rust 只需拒绝 `Null` 之外的非法原语；`#validated` 用 `WeakSet` 语义 → Rust 用 `HashSet` + 代数（或干脆每次都验，用池化判断成本）。

### 2.33 `src/sqlite/*`（6 文件）

`index.ts`（3 行）：re-export。

`schema.ts`（9 行）：
- `validateSqliteSchemaPrefix(prefix)`：正则 `^[A-Za-z_][A-Za-z0-9_]*$`，失败 → `` `Invalid SQLite schema prefix ${JSON.stringify(prefix)}` ``；保留名（`sqlite_` 前缀）→ `` `SQLite schema prefix ${JSON.stringify(prefix)} is reserved` ``。

`database.ts`（38 行）：
- `SqliteValue = null | number | bigint | string | Uint8Array`。
- `SqliteExecutor`：`exec(sql)`、`run(sql, ...params)`、`get<T>(sql, ...params)`、`all<T>(sql, ...params)`。
- `SqliteDatabase extends SqliteExecutor`：`transaction<T>(callback: (transaction: SqliteExecutor) => Promise<T>): Promise<T>`、`close(): Promise<void>`（**回调签名是 `Promise<T>`，不是 `T | Promise<T>`**）。文档语义：事务回调抛错 → ROLLBACK 后原样重抛；ROLLBACK 也失败 → `AggregateError([error, rollbackError], "SQLite transaction failed and rollback failed")`。

`migrations.ts`（43 行）：
- 迁移数组必须版本**连续**（从 1 开始），否则 `Error`；
- 迁移表用 STRICT 单例行（`version` 单行）；
- 表不存在 → 视为版本 0；版本高于代码支持 → 报"too new"；否则逐条 `UPDATE version`。

`node.ts`（201 行）：
```ts
export type NodeSqliteDatabaseOptions = { readonly walAutoCheckpointPages?: number; readonly busyTimeoutMs?: number };
const DEFAULT_WAL_AUTO_CHECKPOINT_PAGES = 1_000;   // 非导出
const DEFAULT_BUSY_TIMEOUT_MS = 5_000;             // 非导出
class SerialOperationQueue { run(op); runAsync(op); }   // 非导出
abstract class NodeSqliteExecutor implements SqliteExecutor { exec/run/get/all; private statement(sql) 缓存 }
class NodeSqliteTransaction extends NodeSqliteExecutor { … }   // 非导出
export class NodeSqliteDatabase extends NodeSqliteExecutor implements SqliteDatabase { transaction(cb); close(); }
export async function openNodeSqliteDatabase(path, options = {}): Promise<NodeSqliteDatabase>;
```
- `SerialOperationQueue`：`tail: Promise<void>`、`pending: number`。
  - `run(op)`：`pending > 0` → `enqueue(op)`；否则**同步**执行 `op()` 并 `Promise.resolve`（同步异常 → `Promise.reject`）。**注意两条路径的 `pending` 行为不同**：队列空闲时 `run` 直接执行且**不**增加 `pending`（不会挡住后续同步调用）；`pending > 0` 时它走 `enqueue` 并 `pending++`。`runAsync` 无论哪条路径都会使 `pending` 至少为 1，直到它 settle。
  - `runAsync(op)`（只有 `transaction` 用）：`pending > 0` → `enqueue(op)`；`pending++`；**先**发布 barrier（`const {promise: barrier, resolve: releaseBarrier} = Promise.withResolvers(); this.tail = barrier;` —— 使 op 内**同步**发起的调用排在它后面）；执行 `op()`（同步抛 → `Promise.reject`）；`return started.finally(() => { pending--; releaseBarrier(); })`。
  - `enqueue(op)`：`pending++`；`return this.release(this.tail.then(op))`。
  - `release(p)`：`settled = p.finally(() => pending--)`；`tail = settled.then(ignore, ignore)`（吞掉结果/异常，让后续操作继续）；返回 `settled`（保持 rejection）。
- `NodeSqliteExecutor`：`exec` → `database.exec(sql)`；`run` → `statement(sql).run(...params)`；`get` → `.get(...)`；`all` → `.all(...)`；语句按 SQL 文本缓存在**共享** `Map` 中（数据库与事务句柄共享）。
- `NodeSqliteTransaction.runOperation`：`!scope.active` → `Error("SQLite transaction handle is no longer active")`。
- `NodeSqliteDatabase.transaction(cb)`：在 `access.runAsync` 内：
  1. `exec("BEGIN IMMEDIATE")`；
  2. `scope = {active:true}`；`await cb(new NodeSqliteTransaction(...))`；
  3. 成功：`scope.active = false`、`exec("COMMIT")`、返回结果；
  4. 失败：`scope.active = false`；`exec("ROLLBACK")` 失败 → `AggregateError([error, rollbackError], "SQLite transaction failed and rollback failed")`；否则重抛原 error。
- `close()`：`access.run` 内：已关闭 → 返回；`statements.clear()`；`try { exec("PRAGMA wal_checkpoint(TRUNCATE)") } finally { database.close() }`。
- `openNodeSqliteDatabase(path, options)`：`checkpointPages = options.walAutoCheckpointPages ?? 1000`；`timeout = options.busyTimeoutMs ?? 5000`；`path !== ":memory:"` → `mkdir(dirname(path), {recursive:true})`；`new DatabaseSync(path, {timeout})`；依次 `PRAGMA journal_mode = WAL`、`PRAGMA synchronous = NORMAL`、`` `PRAGMA wal_autocheckpoint = ${checkpointPages}` ``（**插值，不是绑定参数**）；失败 → `close()`（吞掉 close 错误）后重抛**原始**错误。

`cloudflare.ts`（180 行）：
```ts
type DurableObjectSqlValue = ArrayBuffer | string | number | null;      // 非导出
export type DurableObjectSqliteStorage = {
  readonly sql: { exec(query: string, ...bindings: DurableObjectSqlValue[]): {
    next(): { done?: false; value: Record<string, DurableObjectSqlValue> } | { done: true; value?: undefined };
    toArray(): Record<string, DurableObjectSqlValue>[];
  } };
  transaction<T>(closure: () => Promise<T>): Promise<T>;
};
class SerialQueue { run(op); hold(op); }                    // 非导出
const storageQueues = new WeakMap<DurableObjectSqliteStorage, SerialQueue>();
function queueFor(storage): SerialQueue;                     // 非导出
function bindValue(value: SqliteValue): DurableObjectSqlValue;
const bind = (params) => params.map(bindValue);
function row<T>(row): T;                                     // BLOB → Uint8Array（原地）
class DurableObjectSqliteExecutor implements SqliteExecutor; // 非导出
class DurableObjectSqliteTransaction extends …;              // 非导出，含 active 标志
export class DurableObjectSqliteDatabase extends DurableObjectSqliteExecutor implements SqliteDatabase;
```
- `SerialQueue`：`run(op)`：`pending > 0` → enqueue；否则同步执行并 `Promise.resolve`。`hold(op)`：**总是** enqueue。`enqueue(op)`：`pending++`；`tail.then(op).finally(pending--)`；`tail` 接在其后（吞错）。
- `queueFor(storage)`：同一 storage 对象共享一个队列（模块级 `WeakMap<DurableObjectSqliteStorage, SerialQueue>`）；**直接调用底层 storage 会绕过队列**。
- `bindValue(value)`：`Uint8Array` → `value.slice().buffer`（**复制**，`ArrayBuffer`）；非 bigint → 原样；bigint 超出 `[MIN_SAFE_INTEGER, MAX_SAFE_INTEGER]` → `` RangeError(`Durable Object SQL cannot bind ${value} without losing precision`) ``；否则 `Number(value)`。
- `row(r)`：对每个 `ArrayBuffer` 值**原地**替换为 `new Uint8Array(value)`；不复制其它值。
- executor：`exec(sql)` → `check()` + `storage.sql.exec(sql)`（无绑定）；`run` → `check()` + `storage.sql.exec(sql, ...bind(params))`；`get` → `check()` + `.exec(...).next()`（`done` → `undefined`，否则 `row<T>(value)`）；`all` → `check()` + `.exec(...).toArray().map(row)`。基类 `check()` 是**空方法**。
- `DurableObjectSqliteTransaction.check()`：`!active` → `Error("SQLite transaction handle is no longer active")`。
- `DurableObjectSqliteDatabase`：`exec/run/get/all` 都经 `queue.run`；`transaction(cb)` 经 `queue.hold(() => storage.transaction(async () => { const t = new …Transaction(storage); try { return await cb(t) } finally { t.active = false } }))`（**commit/rollback 由 DO 的 storage.transaction 负责**）；`close()` = `queue.run(() => {})`（只等待队列）。
- **注意**：导出名是 `DurableObjectSqliteDatabase`（不是 `openDurableObjectSqlite`）。

### 2.34 `src/telemetry-sqlite/*`（2 文件）

#### `index.ts`（363 行）

```ts
export interface SqliteTelemetryOptions {
  readonly retentionMs?: number;    // 默认 7*24*60*60*1000（7 天）
  readonly maxQueuedSpans?: number; // 默认 10_000；同时独立限制 pending 的 metric interval/series 对
}
export interface SqliteTelemetry extends TelemetryContext, TelemetryStore {
  readonly metrics: TelemetryMetrics;
  readonly propagation: TelemetryPropagation;
  readonly droppedSpans: number;
  readonly droppedMeasurements: number;
  flush(): Promise<void>;    // 持久化已结算 span 与已完成的 10 秒 metric interval
  close(): Promise<void>;    // 停止记录并 drain（含未满 interval）；**不关闭数据库**
}
```
模块常量：`identities = new WeakMap<TelemetryContext, Identity>()`（`Identity = {id, traceId}`）；`carrierKey = "kiana-sqlite-span"`；`traceKey = "kiana-sqlite-trace"`；`intervalMs = 10_000`。

`copyAttributes(attributes?)`：`Object.entries(...)` → 过滤 `undefined` → 数组元素 `[...value]` 浅拷贝 → `Object.fromEntries`（**保留原键插入顺序**；空属性 → `{}`）。
`copyLink(link)`：`identities` 查不到 context → undefined；否则 `{trace_id, span_id, attributes}`。

`class Recorder implements SqliteTelemetry, TelemetryPropagation`：
- 公开：`propagation = this`；`metrics = { counter: (name, options) => ({add: this.measurement("counter", name, options)}), upDownCounter: …({add}), histogram: …({record}) }`；`droppedSpans`、`droppedMeasurements`。
- 私有：`db`、`retention`、`capacity`、`scope = crypto.randomUUID()`、`sequence`、`spans: SqliteValue[][]`、`measurements: Map<string, Metric>`、`inFlightMeasurements`、`timer`、`stopped`、`failed`、`failure`、`writing?`、`closing?`。
- 构造：`setInterval(1000)`，回调 `if (!this.writing) void this.flush().catch(() => {})`；`timer.unref?.()`。**1 秒周期**。
- `startSpan(options, cb)` → `this.start(undefined, options, cb)`。
- `inject(context)`：有 identity → `{[carrierKey]: id, [traceKey]: traceId}`；否则 undefined。
- `extract(carrier)`：两个键都是 string 才建 parent context（`{metrics, propagation:this, startSpan: (o,cb) => this.start(identity,o,cb)}`），并 `identities.set(parent, identity)`；否则返回 `this`。
- `#inert(options, cb)`：已停止时委托 `NOOP_TELEMETRY_CONTEXT.startSpan`，但覆盖 `metrics`/`propagation`/`startSpan`（嵌套仍 inert）。
- `#start(parent, options, cb)`：
  1. `stopped` → `inert`；
  2. `attributes = copyAttributes(options.attributes)`；links 逐个 `copyLink`（跳过 undefined）；
  3. `id = `${scope}:${++sequence}``；`identity = {id, traceId: parent?.traceId ?? id}`；`started = Date.now()`；`clock = performance.now()`；`events = []`；`status = {status:"ok"}`；`explicit = false`；`settled = false`；
  4. `record(action)`：`!settled && !stopped` 才执行；
  5. span 对象：`metrics`、`propagation`、`startSpan`（settled → inert，否则递归 `this.start(identity, …)`）、`addLink`、`addEvent`（`time_ms = performance.now() - clock`）、`setAttributes`（**合并**到已有属性上）、`setStatus`（error 且带 error 时只保留 `{status:"error", error:{...}}`；否则 `{status}`；置 `explicit = true`）；
  6. `identities.set(span, identity)`；
  7. `await cb(span)`；抛错且未显式设状态 → `{status:"error", error:{name: error.name, message: error.message}}`（非 Error → `{status:"error"}`），重抛；
  8. `finally`：`settled = true`；未停止时若 `spans.length >= capacity` → `droppedSpans++`，否则 push **10 列**：
     ```
     [id, traceId, parent?.id ?? null, name, started, started + performance.now() - clock,
      JSON.stringify(attributes), JSON.stringify(events), JSON.stringify(links), JSON.stringify(status)]
     ```
     注意 `ended_ms` 用 `performance.now()` 的差值加到 `Date.now()` 起点上（混合两个时钟）。
- `#measurement(kind, name, options?)` → `(value, attributes?) => void`：
  - `stopped` → 返回；
  - `unit = options?.unit ?? null`；`copied = copyAttributes(attributes)`；`encoded = JSON.stringify(Object.fromEntries(Object.keys(copied).sort().map(...)))`（**键排序**）；
  - `key = JSON.stringify([name, kind, unit, encoded])`；
  - `interval = Math.floor(Date.now() / 10000) * 10000`；`pendingKey = `${interval}:${key}``；
  - 无 previous 且 `measurements.size + inFlightMeasurements >= capacity` → `droppedMeasurements++`；
  - 否则 upsert：`count+1`、`sum+value`、`min = Math.min(prev.min ?? value, value)`、`max = Math.max(prev.max ?? value, value)`。
- `getTrace` / `findTraces` / `findMetrics`：直接委托 store（**只读已提交行**）。
- `flush()`：`this.closing ?? this.drain(false)`。
- `close()`：首次调用时 `stopped = true`、`clearInterval`、`closing = this.drain(true)`；返回同一个 promise。
- `drain(partial)`：
  1. `while (this.writing) await this.writing`；
  2. `failed` → `throw this.failure`（**同一个错误对象**）；
  3. `spans = this.spans.slice()`；`completed = Math.floor(Date.now()/10000)*10000`；对 `measurements`：`!partial && metric.interval >= completed` → 跳过（未完成的 interval 留给下次）；否则放入 `metrics` 并从 map 删除；
  4. `inFlightMeasurements = metrics.length`；`writing = db.transaction(async tx => { 逐个 INSERT telemetry_spans VALUES (?,?,?,?,?,?,?,?,?,?); 逐个 INSERT telemetry_metrics VALUES (?,?,?,?,?,?,?,?,?,?) ON CONFLICT(interval_ms, series_key) DO UPDATE SET count = count + excluded.count, sum = sum + excluded.sum, min = min(min, excluded.min), max = max(max, excluded.max); DELETE FROM telemetry_spans WHERE ended_ms < cutoff; DELETE FROM telemetry_metrics WHERE interval_ms < cutoff })`，`cutoff = Date.now() - retention`；
  5. 成功 → `this.spans.splice(0, spans.length)`；失败 → `failed = stopped = true`、`failure = error`、清空 spans 与 measurements、`clearInterval`、抛出；`finally` → `writing = undefined`、`inFlightMeasurements = 0`；
  6. `await this.writing`。

`createSqliteTelemetry(db, options = {})`：`retention = options.retentionMs ?? 7*24*60*60*1000`；`capacity = options.maxQueuedSpans ?? 10_000`；`applySqliteMigrations(db, [...], { table: "telemetry_schema" })`，迁移 v1 的 5 条语句见 §4.7；返回 `new Recorder(db, retention, capacity)`。

#### `store.ts`（171 行）

- `SpanRow` / `storedSpan(row)`：snake_case ↔ camelCase；`parent_id` 为 null 时省略 `parentId`；`attributes`/`events`/`links` JSON.parse；`status` 还原为对象。
- `attributesSql(attributes, column, params)`：对 `Object.entries(attributes ?? {})` 每项生成
  `` EXISTS (SELECT 1 FROM json_each(${column}) entry WHERE entry.key = ? AND ${type} AND entry.value = ?) ``，
  并用 `params.push(key, typeof value === "boolean" ? Number(value) : value)` 推两个参数；多个条件用 `" AND "` 连接，**无属性时返回字面量 `"1"`**。`type` 的取值逐字：
  - number → `entry.type IN ('integer', 'real')`（**逗号后有一个空格**）；
  - boolean → `entry.type = 'true'` / `entry.type = 'false'`；
  - 其它（string）→ `entry.type = 'text'`。
  注释逐字：JSON 相等必须区分布尔 `true` 与数字 `1`，`json_each` 也支持任意属性键。
- `getTrace(traceId)`：`ORDER BY started_ms, id`；无行 → undefined。
- `findTraces(query = {})`：`limit = query.limit ?? 20`；按顺序拼条件与参数（**顺序即 SQL 里 `?` 的顺序**）：
  1. `name` → `(SELECT name FROM telemetry_spans WHERE trace_id = heads.trace_id ORDER BY started_ms, id LIMIT 1) = ?`（**该 trace 最早那个 span 的 name**，不是任意 span）；
  2. `startMin` → `started_ms >= ?`；`startMax` → `started_ms <= ?`（作用在 heads 的 `min(started_ms)` 上）；
  3. `errors` → `EXISTS (SELECT 1 FROM telemetry_spans WHERE trace_id = heads.trace_id AND json_extract(status, '$.status') = 'error')`；
  4. `attributes` → `EXISTS (SELECT 1 FROM telemetry_spans candidate WHERE candidate.trace_id = heads.trace_id AND ${attributesSql(query.attributes, "candidate.attributes", params)})`；
  5. `cursor` → `JSON.parse(cursor)` 成 `[number, string]`，条件 `(started_ms < ? OR (started_ms = ? AND trace_id > ?))`，绑定 `cursor[0], cursor[0], cursor[1]`。
  然后在 `db.transaction` 内：
  ```sql
  WITH heads AS (
    SELECT trace_id, min(started_ms) started_ms FROM telemetry_spans GROUP BY trace_id
  ) SELECT * FROM heads WHERE <conditions.join(" AND ") || "1">
    ORDER BY started_ms DESC, trace_id LIMIT ?
  ```
  多绑一个 `limit + 1`；`more = heads.length > limit` 时 `heads.pop()`；`heads` 为空 → `{ traces: [] }`（**无 cursor 键**）。
  再 `SELECT * FROM telemetry_spans WHERE trace_id IN (SELECT value FROM json_each(?)) ORDER BY started_ms, id`，参数 `JSON.stringify(heads.map(h => h.trace_id))`；按 `trace_id` 分组（组内保持 `started_ms, id` 顺序）；`last = heads.at(-1)`；`more` 时返回 `cursor: JSON.stringify([last.started_ms, last.trace_id])`。
- `findMetrics(query)`：`params = [query.name]`、`conditions = ["name = ?"]`；`startMin` → `interval_ms >= ?`；`startMax` → `interval_ms <= ?`；再 **无条件** push `attributesSql(query.attributes, "attributes", params)`（无属性时为字面量 `"1"`，即恒真）；SQL 为 `` `SELECT * FROM telemetry_metrics WHERE ${conditions.join(" AND ")} ORDER BY interval_ms, series_key` ``；`unit` 为 `null` 时**省略 `unit` 键**（不是 `unit: undefined`）。

---

## 3. 用户可见表面（逐字）

**重要前提**：`@earendil-works/chord` 是**库**，不是 CLI/TUI。它**没有**命令行参数、斜杠命令、快捷键、ANSI 渲染、颜色、布局宽度或 spinner 帧。任何 TUI 层面的表现属于 `packages/tui` 与 `packages/coding-agent` 单元，不在本单元。

本单元的"用户可见表面"= **调用方程序员可见的 API 契约** + **线协议** + **磁盘格式** + **控制消息** + **错误文案**。以下全部逐字。

### 3.1 错误码表（`ChordError.code`，12 个，顺序即 `CHORD_ERROR_CODES` 声明顺序）

| # | code | 语义（文档原文） | `retryable` |
|---|------|------------------|-------------|
| 1 | `aborted` | The call's `ctx` was cancelled | 否 |
| 2 | `contract_mismatch` | The provider's contract doesn't cover the consumer's, and no adapter exists | 否 |
| 3 | `forbidden` | The factory or a method refused this principal | 否 |
| 4 | `gone` | The object was forced away while referenced. Call `get` again. | 否 |
| 5 | `invalid` | Input, arguments or a result failed validation | 否 |
| 6 | `not_in_variant` | The member doesn't exist in the variant you have | 否 |
| 7 | `not_ready` | A declared resource or reference was used inside a factory, or state read before its reference's first hydration | 否 |
| 8 | `released` | The reference was released | 否 |
| 9 | `superseded` | The root's code version is no longer available on this host. Retry. | **是** |
| 10 | `unavailable` | The target host can't be reached or started; reason `ready_timeout`: a reference was not ready within `readyTimeout` | 否 |
| 11 | `upgrade_conflict` | An old root reached an object migrated meanwhile. Retry as a new root. | **是** |
| 12 | `upgrade_required` | A browser's contract is outdated. Reload its web facets. | 否 |

`Error.name = "ChordError"`。`String(error)` / `error.message` 的格式【逐字】：

```
<code>: <message>                     // 无 reason
<code>/<reason>: <message>            // 有 reason
```
（例：`invalid/state: state count: Expected number`、`unavailable/unknown_type: Foo is not served by host`。传输时 `fromWireError` 用 `/^[a-z_]+(\/[a-z_]+)?: /` 去掉该前缀，得到裸 message。）

### 3.2 全部 reason（细分原因）逐字清单

| code + reason | 出现处 / 含义 |
|---|---|
| `invalid/args` | 方法实参校验失败（`assertValid("args", …)`） |
| `invalid/result` | 方法返回值校验失败 |
| `invalid/state` | cell 初始值/变更值/迁移后状态校验失败，以及 reload 的版本与 schema 检查 |
| `invalid/input` | 输入校验失败；host-lifetime 输入非空 |
| `invalid/implementation` | factory / 声明实现不合法 |
| `invalid/duplicate` | 同一 run 内重名声明、同一 type 以不同 contract 二次 hold |
| `invalid/cycle` | 跨 host 引用环 |
| `invalid/read_only` | factory 运行期间写 cell |
| `invalid/not_host_lifetime` | 非 host lifetime 对象调用 `self.register` |
| `invalid/registry_conflict` | 两个对象注册同一 registry 键且 `conflict === "error"`；消息 `` `${registry.name}: ${key} registered by ${existing.object.address} and ${object.address}` `` |
| `invalid/placement` | 输入字段解析不出 host，或 type 被部署到别处 |
| `invalid/token` | contract token 结构非法 |
| `invalid/foreign_ctx` | ctx 来自另一个 host |
| `invalid/protocol` | 链路协议违规（见 §3.5） |
| `aborted/aborted` | reload 回滚后 prepare 抛 `ChordError("aborted", "reload aborted while preparing", {reason:"aborted"})`；集群 abort 帧导致 `` "reload aborted by the server" `` |
| `aborted/expired` | ctx 已随调用结算 |
| `aborted/deadline` | reload 的 `withDeadline` 超时 |
| `aborted/gate_deadline` | reload gate 超时（`details.blocking`） |
| `aborted/vote_deadline` | 集群投票超时 |
| `unavailable/unknown_type` | 类型未在本 host 服务 |
| `unavailable/ready_timeout` | 引用在 `readyTimeout` 内未 ready |
| `unavailable/host_failed` | host 失败后不再接受工作 |
| `unavailable/too_many_calls` | 连接上并发调用超 `maxCalls` |
| `unavailable/not_coordinator` | 非 server 发起集群 reload |
| `unavailable/locked` | 进程锁被占用 |
| `unavailable/reload_running` | 已有 reload 在跑 |
| `upgrade_conflict/gate` | 对象正在升级（acquire 时） |
| `superseded/*` | 版本被淘汰（无 reason 时省略） |
| `not_ready/*` | （无 reason） |
| `WatchEnd.reason`（非 ChordError） | `stopped`、`cancelled`、`retired`、`ended`、`listener_error`、`failed` |

### 3.3 全部错误文案（逐字；`${…}` 为插值）

**cell / derived**
- `` `state ${key} is already initialized` ``（`Error`）
- `` `state ${key} (its init has not run)` ``（not_ready）
- `"cell.change callbacks must be synchronous"`（TypeError）
- `` `derive ${key}: sources must be readables` ``
- `` `derive ${key}: fn must be synchronous` ``
- `` `${what} is not ready` ``（`notReady`）

**state**
- `"Replicated state cannot be changed reentrantly from a change callback"`
- `"Replicated state cannot be replaced from a change callback"`
- `"Replicated state change callbacks must be synchronous"`（TypeError）
- `` `Replicated state source cursor has a gap: expected ${expected}, received ${frame.cursor}` ``
- `` `Replicated state source ${kind} cursor must be a safe integer` ``
- `"Replicated state source contract failed"`（AggregateError message）
- `"Failed to attach replicated state source"`（AggregateError message）

**watch**
- `"Watch is already started"`、`"Watch is stopped"`、`"Watch cancellation is already installed"`

**contract**
- `` `opaque id must be name@version: ${id}` ``
- `"hostNamed needs a host name"`、`"hostFamily needs a host-name prefix"`、`"hostFamily needs an input field name"`
- `` `${typeName}: input field ${placement.key} names no host` ``
- `` `member name is reserved: ${name}` ``
- `` `remote contracts cannot expose opaque members: ${name}` ``
- `"contract name is empty"`、`"contract version must be a positive integer"`
- `` `remote contract ${name} needs \`host\`: hostNamed(…) or hostFamily(…)` ``
- `` `remote contract ${name} has no variants` ``
- `` `member ${key} is both state and method in ${name}.${kind}` ``
- `` `methods.${key} is not a method()` ``
- `"malformed contract token"`（ChordError invalid, reason `token`）
- coverage 原因串：`"different type"`、`"host"`、`"input"`、`` `variant ${kind} missing` ``、`` `${kind}.${name} missing or of another kind` ``、`` `${kind}.${name} arguments` ``、`` `${kind}.${name} result` ``、`` `${kind}.${name} state` ``、`` `${kind}.${name} opaque ${offered} != ${wanted}` ``
- bind 三条：`` `${name} lives on ${placementOf(provider)}, the consumer expects ${placementOf(consumer)}` ``；`` `${name}@${version} does not cover the consumer's description (${reason}); forgotten version bump?` ``；`` `${name}: provider version ${p}, consumer version ${c}, no accepts adapter` ``

**wire / link**
- `"packed payload without zlib"`、`"bad packed payload"`、`"inflateMessage needs zlib"`、`"bad envelope"`、`"not a frame"`、`"packed frames are not accepted"`、`"unknown message type"`

**connection**
- `"relayed hold closed"`（gone）
- `` `link to ${this.name ?? "peer"} closed: ${reason}` ``（unavailable）
- `` `link to ${this.name ?? "peer"} closed` ``（whenSynced 时）
- `"principal invalidated"`（gone）
- `"call aborted"`、`"call cancelled"`（aborted）
- `"not a local reference"`（unavailable）
- `"unknown hold"`（gone，回 result error）
- `` `Too many calls in progress on this connection (${max}); wait for earlier ones to finish` ``（unavailable/too_many_calls）
- `"released"` 相关：`"relayed hold released"`
- `"actor ..."`（无；见下）
- violation（`invalid/protocol`）逐字：`"malformed frame"`、`` `ack ${n} outside ${acked}..${sent}` ``、`` `duplicate hold id ${id}` ``、`` `duplicate call id ${id}` ``、`"parent is not a host"`、`` `hello ${kind} ${name} does not match the accepted ${peer.kind} ${peer.name ?? ""}` ``、`` `${frame.t} from a child` ``、`"vote from a non-participant"`；关闭原因 `protocol error: ${reason}`、`protocol-version ${PROTOCOL_VERSION}`
- `` `${token.name} is local; only remote types cross links` ``（invalid/token）
- `` `${token.name} with this input does not live on ${host.name}` ``（invalid/placement）
- `` `${host.name} does not relay` ``（unavailable，gone 帧）
- `"protocol error"`（link 解码失败时 close 原因）

**host**
- `` `createHost needs \`self\`: the principal of the host's own code` ``（TypeError）
- `` `code version ${given} is not newer than ${current.id}` ``
- `"host.serve during a reload; serve from a facet passed to reload()"`
- `"host.install during a reload"`、`` `facet ${name} is already installed; use reload()` ``
- `"loading modules needs createHost({ loader }), e.g. nodeLoader from @earendil-works/chord/node"`
- `` `facet ${name} was replaced or removed` ``（gone）
- `` `${name} is already served` ``
- `` `${name} is declared for ${placementText(placement)}, but ${hostName} serves it` ``（TypeError）
- `` `${name} has lifetime "host": its one object has the empty input` ``（invalid/input）
- `` `${type.name} with this input lives on ${resolved}, not ${host.name}` ``（invalid/placement）
- `` `${from.address} -> ${address} would create a ref cycle` ``（invalid/cycle）
- `` `${type.name} is being upgraded; retry` ``（upgrade_conflict/gate）
- `` `code version ${id} is not retained by ${name}` ``（superseded）
- `` `code version ${cv} is retired` ``（superseded）
- `` `${registry.name}: ${key} registered by ${addrA} and ${addrB}` ``（invalid/registry_conflict）
- `"accepting a host link needs its name"`（TypeError）
- `` `connect needs \`principal\`: what the parent's own requests act as` ``（TypeError）
- `` `route ${prefix} needs \`principal\`: what the routed hosts act as when they call back` ``（TypeError）
- `` `${name} runs code version ${cv}, ahead of ${current}` ``（invalid；随后 close `code version ahead of parent`）
- `"code version ahead of parent"`、`"reconciliation failed"`、`"replaced"`、`"host closed"`
- `` `${hostName} refused for this principal` ``（forbidden）
- `"principal invalidated while resolving"`（gone）
- `` `no route to ${at}` ``（unavailable）、`` `no route authorizes ${at}` ``（forbidden）
- `"reloads are coordinated by the server"`（unavailable/not_coordinator）
- `` `host ${name} closed` ``（gone）
- `` `${type} is not served by ${host}` ``（unavailable/unknown_type）
- `"host close: disposal failed"`

**object**
- `` `${contract.name}: unknown lifetime ${lifetime}` ``（TypeError；注意用 `String(lifetime)` 而非 JSON.stringify）
- `` `${contract.name}: lifetime "host" needs a local type (remote types are referenced)` ``
- `` `${contract.name}: an object of lifetime "host" does not linger (it lives as long as the host)` ``
- `` `${contract.name}: lifetime "host" needs an input that accepts {} (the object's one address)` ``
- `` `resource ${key} could not be restored` ``（unavailable）
- `` `resource ${key}` ``（not_ready）
- `` `resource ${key} opened after abort` ``（aborted）
- `` `resource ${key} (inside a factory)` ``（Error）
- `"ctx belongs to another host"`（invalid/foreign_ctx）
- `"ctx expired: its call already settled"`（aborted/expired）
- `"ctx aborted"`（aborted）
- `` `${address} was not ready after ${ms} ms[, waiting on ${list}] (a reference cycle across hosts?)` ``（unavailable/ready_timeout）
- `"call aborted"`
- `` `${address}.${name} called inside a factory` ``（not_ready）
- `"ctx must come from a Chord host"`（invalid/foreign_ctx）
- `` `${address} abandoned` ``（aborted）
- `` `${typeName} is not served` ``（unavailable/unknown_type）
- `` `${address}: code changed during activation` ``（superseded）
- `"activating ${address}"`（错误上下文）
- `` `${typeName}: factories must be synchronous` ``（invalid/implementation）
- `` `listener ${key}: source has no subscribe()` ``（invalid/implementation）
- `` `${address}: aborted tasks still running after ${deadline} ms; starting their replacements anyway` ``（aborted）
- `"task stopped"`（aborted）
- `` `${host.name} failed and admits no work` ``（unavailable/host_failed）
- `` `code version ${cv} is retired` ``（superseded）
- `` `${address} was upgraded in code version ${minCodeVersion}` ``（upgrade_conflict）
- `` `${address} is being upgraded` ``（upgrade_conflict）
- `"cells cannot be written while a factory runs"`（invalid/read_only）
- `"write with an aborted ctx"`（aborted）
- `` `${address} is disposed` ``（gone）
- `` `${address} is ${status}` ``（gone）
- `` `${address} has no implementation for code version ${cv}` ``（superseded）
- `` `${address}: ${name} is not a method of variant ${variant ?? "(local)"}` ``（not_in_variant）
- `` `${ref.address}: ${name} is not a state member of variant ${variant ?? "(local)"}` ``（not_in_variant）
- `` `${address} was disposed` ``（gone）
- `` `${contract.name}: factory must return an object` ``、`` `…: unknown variant ${v}` ``、`` `…: ${key} is not in variant ${v}` ``、`` `…: ${name} must be a method` ``、`` `…: ${name} must be a cell, a derivation or a held object's state member` ``、`` `…: ${name} is missing` ``（均为 invalid/implementation）
- `` `${kind} needs a name` ``（TypeError）、`` `${typeName}: duplicate ${kind} name ${name}` ``（invalid/duplicate）
- `` `state ${name}: version must be a positive integer` ``、`` `resource ${name}: version must be an integer` ``
- `` `${typeName}: ${type.name} held twice with different contracts` ``（invalid/duplicate）
- `` `resource ${name}: deps must be references or resources of this object` ``
- `` `listener ${name}: the handler must be a function` ``、`` `listener ${name}: select must be a function` ``
- `"a task must be a function"`、`"a derivation needs a function"`、`` `derive ${key}: sources must be readables` ``、`` `derive ${key}: fn must be synchronous` ``
- `"expose takes a resource declared by this object"`
- `` `${typeName}: only objects of lifetime "host" may register` ``（invalid/not_host_lifetime）

**remote**
- `` `${key}.${name} is not hydrated yet` ``（not_ready）
- `` `${key}: ${name} is not a state member of variant ${variant}` ``（not_in_variant）
- `` `reference ${key} released` `` / `` `reference ${address} released` ``（released）
- `` `${key}: ${name} is not a method of variant ${variant}` ``（not_in_variant）

**reload / cluster**
- `` `reload aborted: ${message}` ``、`"reload aborted"`、`"reload aborted while preparing"`、`"reload aborted; type not served"`、`"reload aborted by the server"`
- `"a reload is already running"`（unavailable/reload_running）
- `` `${what} within ${deadline} ms` ``（aborted/deadline）
- `` `code version ${previous.id} was retired` ``（superseded）
- `` `${name}@${version} changed incompatibly without a version bump (${reason})` ``（contract_mismatch）
- `` `${object.address}: version of state ${name} lowered from ${from} to ${to}` ``
- `` `${object.address}: schema of state ${name} changed without a version bump` ``
- `` `state ${name} version ${from} -> ${to}` ``、`"exclusive resource replaced"`（gate reasons）
- `` `gate deadline (${ms} ms) missed; blocking roots: ${blocking}` ``（aborted/gate_deadline，`details.blocking`）
- `` `${address}: resource ${key} could not be restored` ``（unavailable）
- `"type no longer served"`（gone）
- `` `${object.typeName} changed; reload the web facets` ``（upgrade_required）
- `` `votes missing after ${deadline} ms` ``（aborted/vote_deadline）
- `` `${name}: ${cause.message}` ``（集群投票失败的包装）

**manager / loader / sqlite / output / telemetry**
- `` `lock ${name} is held by another process` ``（unavailable/locked）
- `` `lock directory ${directory} must be a directory only its owner can access` ``
- `"child not ready in time"`、`` `child exited before ready (${code ?? signal})` ``、`` `child failed: ${message}` ``
- `` `${name}: default export must be a facet (defineFacet)` ``（TypeError）
- `` `Invalid SQLite schema prefix ${JSON.stringify(prefix)}` ``、`` `SQLite schema prefix ${JSON.stringify(prefix)} is reserved` ``
- `"SQLite migrations must have contiguous versions starting at 1"`、`"SQLite schema metadata is missing"`、`` `SQLite schema version ${v} is newer than supported version ${c}` ``
- `"SQLite transaction handle is no longer active"`、`"SQLite transaction failed and rollback failed"`（AggregateError message）
- `` `Durable Object SQL cannot bind ${value} without losing precision` ``（RangeError）
- `"Skipped output requires tail retention"`
- 输出清洗：`sanitizeOutput` **删除**非法控制字符（`\x00-\x08`、`\x0b-\x1f`、`\ufff9-\ufffb`，即 U+FFF9–U+FFFB 三个"interlinear annotation"字符）；tab 与换行保留。

### 3.4 TypeBox 校验错误文案（影响所有 `invalid/*` 消息的拼接）

`assertValid` 的摘要格式【逐字】：
```
${error.instancePath || "/"} ${error.message}
```
多个错误用 `"; "` 连接，**只取前 3 个**；整条消息为 `` `${what}: ${summary}` ``。`details` 是这 3 个 TypeBox 错误对象的数组（`{ schema, path, value, message }`，实际为 `ValueError` 结构）。

**校验状态提示**：本仓库（`/home/ctyun/code/kiana_rev/kiana`）**未安装 `node_modules`**，`typebox@1.3.27` 的源码不在磁盘上；§3 的这些文案是从调用点与 `Value.Errors` 的公开行为推导的，**移植前必须对照 `typebox-1.3.27.tgz` 的 `build/value/errors/*.mjs` 逐条核对**（见 §8 开放问题）。相对可靠的是格式部分：`assertValid` 只依赖 `error.instancePath` 与 `error.message` 两个字段，且只取前 3 个。

Rust 端必须复刻 TypeBox 的 `Value.Errors` 文案，最常见者（**待与 typebox@1.3.27 核对**）：
- `"Expected string"`、`"Expected number"`、`"Expected integer"`、`"Expected boolean"`、`"Expected object"`、`"Expected array"`、`"Expected union value"`、`"Expected literal value"`、`"Expected null"`、`"Expected unknown"`；
- `"Unexpected property"`（additionalProperties:false）；
- `"must be at least 1"`、`"must be at most N"`、`"must be a multiple of N"`；
- `"Expected string length greater or equal to N"`、`"Expected string length less or equal to N"`、`"Expected string to match ..."`；
- `"Expected array length greater or equal to N"`、`"Expected array length less or equal to N"`、`"Expected unique items"`；
- `"Expected required property"`；
- `"Expected object properties greater or equal to N"`；
- integer 检查失败时 TypeBox 报 `"Expected integer"`（`type: "integer"`）。
- 另需覆盖 Chord 自己的 `Type.Unsafe`/`Type.Function`（`parameters`/`returnType` 校验）错误，以及 `additionalProperties: false` 时 `Value.Clean` 已先删键、因此 `"Unexpected property"` 只在 `assertValid` 直接调用（不经过 `normalize`）时出现。

`instancePath` 形如 `""`（根，显示为 `/`）、`"/count"`、`"/args/0"`、`"/variants/x/m"`。

### 3.5 协议违规（会在 host 上 `report` 一条 `invalid/protocol` 并关闭链路）

可观察的关闭原因【逐字】：
- `protocol-version ${n}`（版本不匹配；对端从 bye 的 reason 也能学到）
- `protocol error: ${reason}`，reason ∈ {`malformed frame`、`ack N outside A..B`、`duplicate hold id N`、`duplicate call id N`、`parent is not a host`、`hello … does not match …`、`prepare from a child`、`commit from a child`、`abort from a child`、`vote from a non-participant`}
- `ping timeout`（`Date.now() - lastSeen > pingTimeout`）
- `code version ahead of parent`、`reconciliation failed`、`replaced`、`host closed`、`principal invalidated`、`ipc disconnected`、`socket closed`
- `unauthorized`、`authentication failed`（仅 `acceptWebSockets` 的 principal 回调）
- 本地 link 解码失败：`protocol error`（`BaseLink.deliverLater` 的解码 promise reject → `close("protocol error")`）
- `bye` 帧的 `reason` 缺省时对端记为 `peer closed`（`#deliverNow`）；`MemoryLink.shutdown()` 也让对端 `ended("peer closed")`
- `IpcLink` 写失败：`` `ipc send failed: ${error.code ?? error.message}` ``（如 `ipc send failed: EPIPE`）

关闭原因会出现在 consumer 侧的错误消息里：`` `link to ${name ?? "peer"} closed: ${reason}` ``（`unavailable`），`whenSynced` 未同步时则是无 reason 的 `` `link to ${name ?? "peer"} closed` ``。

### 3.6 公开 API 名称（Rust 侧需保持语义等价）

**导出面统计（精确）**：9 个入口模块合计 **189 个唯一导出名**，其中 **74 个值**（runtime exports）与 **115 个类型**（type-only）。`chord/delta` 的精确值是：`isReplace`、`isBase`、`overlap`、`ownedSlice`、`diffRevisions`、`appendText`、`track`、`PendingOps`、`RESERVED_SEGMENTS`、`UnsafePathError`、`assertValidOp`、`assertValidWireOp`、`assertSafePath`、`PathError`、`apply`、`applyImmutable`、`applyImmutableBatches`、`encoder`、`decoder`（19 个）；类型是 `JsonValue`、`Seg`、`Path`、`NonEmptyPath`、`PathRef`、`Op`、`WireOp`、`Draft`、`Change`、`Prepared`、`StringKeys`、`Tracker`、`PendingTake`、`Encoder`、`Decoder`（15 个）。
`chord/output` 的值：`sanitizeOutput`、`boundOutput`、`characterEnd`、`utf8ByteLength`、`PROGRESS_BYTES_PER_SECOND`、`Progress`（6 个；`OutputBuffer` 也是值导出 → 共 7 个：`OutputBuffer`、`sanitizeOutput`、`boundOutput`、`characterEnd`、`utf8ByteLength`、`PROGRESS_BYTES_PER_SECOND`、`Progress`）。类型：`OutputLimits`、`BoundedOutput`、`OutputSlice`、`OutputSkip`、`ProgressClock`。

**注意**：`MAX_PENDING_OPS` 由 `pending.ts` 导出，但 `delta/index.ts` 只 re-export `PendingOps` 与 `PendingTake`，所以它**不在**公共导出面；`MAX_DELTA_OPERATIONS` 在 `diff.ts` 与 `tracker.ts` 各有一份**非导出**常量；`JsonRevisionValidator`（`revision-validator.ts`）与 `applyImmutableTrusted`（`apply-immutable-trusted.ts`）都**未被** `index.ts` re-export；`equalTrustedJson` 是 `tracker.ts` 的内部函数，不导出。

各子路径的导出如下：

见 §2.1 的 `index.ts` 导出清单；其余子路径导出：
- `@earendil-works/chord/context`：`BACKGROUND_CONTEXT`、`TODO_CONTEXT`、`createContextKey`、`withContextValue`、`withTelemetry`、`traceIn`、`withAbortSignal`、`withoutAbortSignal`、`withCancel`、`awaitWithContext`（共 10 个值导出；`Context`/`ContextKey` 仅类型；`EmptyContext`、`abortError` 与两个 key 常量**不导出**）。
- `@earendil-works/chord/output`：**值只有 7 个** —— `sanitizeOutput`、`boundOutput`、`characterEnd`、`OutputBuffer`、`utf8ByteLength`、`PROGRESS_BYTES_PER_SECOND`、`Progress`（`SYSTEM_CLOCK` 只作 `Progress` 的默认参数，**不导出**）；类型 `OutputLimits`、`BoundedOutput`、`OutputSlice`、`OutputSkip`、`ProgressClock`。`NEWLINE`、`INVALID_OUTPUT`、`lineCount`、`countNewlines`、`headRange`、`tailRange`、`tailMargin`、`lines`、`characterStart`、`SYSTEM_CLOCK` **均不导出**。
- `@earendil-works/chord/delta`：**19 个值** —— `PathError`、`PendingOps`、`RESERVED_SEGMENTS`、`UnsafePathError`、`appendText`、`apply`、`applyImmutable`、`applyImmutableBatches`、`assertSafePath`、`assertValidOp`、`assertValidWireOp`、`decoder`、`diffRevisions`、`encoder`、`isBase`、`isReplace`、`overlap`、`ownedSlice`、`track`；**15 个类型** —— `Change`、`Decoder`、`Draft`、`Encoder`、`JsonValue`、`NonEmptyPath`、`Op`、`Path`、`PathRef`、`PendingTake`、`Prepared`、`Seg`、`StringKeys`、`Tracker`、`WireOp`（共 34）。**不在**入口导出：`MAX_PENDING_OPS`、`MAX_DELTA_OPERATIONS`、`JsonRevisionValidator`、`applyImmutableTrusted`、`equalTrustedJson`、`assertIndexInRange`（它们各自所在文件导出或完全不导出）。
- `@earendil-works/chord/node`：`nodeLoader`、`createHostManager`、`forkDriver`、`processLock`、`childControl` + 类型 `ControlMessage`/`Driver`/`Exit`/`ForkDriverOptions`/`HostManager`/`HostManagerOptions`/`Instance`/`Liveness`。
- `@earendil-works/chord/telemetry-sqlite`：值 `createSqliteTelemetry`；类型 `SqliteTelemetry`、`SqliteTelemetryOptions`。`store.ts` 的 `getTrace`/`findTraces`/`findMetrics` **不**从入口导出（它们只通过 `SqliteTelemetry` 的方法暴露）。
- `@earendil-works/chord/sqlite`：值 `applySqliteMigrations`、`validateSqliteSchemaPrefix`；类型 `SqliteDatabase`、`SqliteExecutor`、`SqliteValue`、`SqliteMigration`。
- `@earendil-works/chord/sqlite/node`：值 `openNodeSqliteDatabase`、`NodeSqliteDatabase`；类型 `NodeSqliteDatabaseOptions`（仅这 3 个导出；`NodeSqliteTransaction`/`NodeSqliteExecutor`/`SerialOperationQueue`/`DEFAULT_WAL_AUTO_CHECKPOINT_PAGES`/`DEFAULT_BUSY_TIMEOUT_MS` 均**不导出**）。
- `@earendil-works/chord/sqlite/cloudflare`：`DurableObjectSqliteDatabase`、`DurableObjectSqliteStorage`（仅这两个导出；`SerialQueue`、`bindValue`、`row` 不导出）。

### 3.7 环境变量与进程行为

| 名称 | 值语义 |
|---|---|
| `CHORD2_LOCK` | 由 manager 写入子进程 env，值为锁名；子进程 `childControl` 读取它并 `processLock(name)`。`forkDriver` **先删除继承值再写入自己的**。 |
| `process.platform` | 决定锁地址形式：`win32` → 命名管道；`linux`/`android` → 抽象 socket；其它 → socket 文件。 |
| `globalThis.process.getBuiltinModule("node:zlib")` | 决定是否有同步 deflate/inflate；浏览器无 → 走 `DecompressionStream("deflate-raw")`（异步）。 |
| `TMPDIR` / `TMP` / `TEMP`（经 `os.tmpdir()`） | 进程锁 socket 文件的首选目录（仅 macOS/BSD 路径）；`Buffer.byteLength(path) > 103` 时改用 `/tmp/chord2-<uid>`。 |
| `PATH` 等 | Chord 自身不读；`fork()` 继承整个 `process.env`（除被删除的 `CHORD2_LOCK`）。 |

其余不读任何环境变量。无配置文件、无 XDG 路径、无 `~/.chord`。进程级副作用只有：`registerHooks`（loader，一次）、`process.once("exit")` 删锁文件（仅文件形式）、`process.kill(-pid, "SIGKILL")`（进程组）、`unref()` 的定时器不影响进程退出。

---

## 4. 外部格式（逐字）

### 4.1 Contract token / description（跨进程传递，`hold.token` 字段）

**JSON Schema（TypeBox，`description.ts` 内）**：
```json
{
  "name": "string (minLength 1)",
  "scope": "remote" | "local",
  "version": "integer",
  "input": { },                        // 任意对象，视为 JSON Schema
  "host": {                            // 仅 remote；local 必须缺席
    "oneOf": [
      { "name": "string (minLength 1)" },                                  // additionalProperties: false
      { "prefix": "string (≥1)", "key": "string (≥1)" }                    // additionalProperties: false
    ]
  },
  "variants": { "<variant>": { "<member>": <MemberDescription> } }
}
```
`MemberDescription`：
```json
{ "kind": "method", "args"?: <SchemaObject>, "result"?: <SchemaObject> }
{ "kind": "state", "schema": <SchemaObject> }
{ "kind": "opaque", "id": "string" }
```
`SchemaObject = Type.Unsafe<TSchema>(Type.Object({}))`（空 schema 对象，任何 object 都通过）。

**local contract 的 variant 键恒为空字符串 `""`**（`LOCAL_VARIANT`）。local description **没有** `host` 键（而非 `host: null`）。

`fingerprint = hash64(canonicalJson(description))` → 16 个小写十六进制字符。**描述对象的键顺序由构造器决定**：remote 为 `{name, scope:"remote", version, input, host, variants}`；local 为 `{name, scope:"local", version, input, variants}`。

`contractFromDescription` 校验失败 → `ChordError("invalid","malformed contract token",{reason:"token"})`；Remote 的 `def` 故意 `variants: {}`。

### 4.2 Payload 打包格式（三种 transport 共用语义）

**阈值**：`PACK_THRESHOLD = 64 * 1024`（**字符数** = `json.length`，即 UTF-16 长度；不是字节）。

**打包对象**：只有 `call.args`、`result.value`、`state.value` 三个字段（`PAYLOAD_FIELDS = {"args","value"}`）。

**压缩**：`zlib.deflateRawSync(json, { level: 1 })` —— **raw deflate，level 1**，无 zlib 头、无校验和。解压 `zlib.inflateRawSync`。

#### (a) 对象传输（MemoryLink、IpcLink）——`packObject`
帧对象去掉 payload 字段，加：
```json
{ ...envelope, "z": ["args" | "value", "<base64 of raw-deflate bytes>"] }
```
base64 为标准表、无换行（Node `Buffer.toString("base64")`）。解码 `Buffer.from(z[1], "base64")`。

#### (b) WebSocket 二进制消息——`packMessage`
```
[u32 BE envelopeLength][envelope JSON UTF-8，含 "z": "<field>"][raw-deflate payload bytes]
```
- `envelopeLength` 是 header 的**字节数**（不含 4 字节前缀，不含 payload）。
- `header` 前缀写入用 `new DataView(message.buffer).setUint32(0, head.byteLength)`（**默认大端**）。
- 非打包帧仍是**文本**消息（`JSON.stringify(frame)`）；打包帧是**二进制**消息。
- 解析：`getUint32(0)` → `data.subarray(4, 4+len)` 解码 JSON → 取 `z` → `bytes = data.slice(4+len)`（`slice` 复制）。
- 未打包但含 payload 时，为避免二次编码用字符串拼接：`` `${head.slice(0,-1)},${JSON.stringify(field)}:${json}}` ``，其中 `head = JSON.stringify(envelope)`。

#### (c) 内存中已压缩值的表示
`Packed { bytes: Uint8Array }`（导出的类）。中继 host 原样转发该对象（不 inflate）。

**边界**：`json.length <= PACK_THRESHOLD` → 不打包（恰等于 64Ki 字符也不打包）；`compress === false` → 不打包；无 zlib → 不打包。

### 4.3 线协议 v4 —— 全部帧（`PROTOCOL_VERSION = 4`）

帧 schema 是 `Type.Union`（**23 个变体**，按源码顺序：hello、ack、hold、release、shape、gone、call、result、cancel、sub、unsub、resync、state、ping、pong、bye、root_end、invalidate、prepare、vote、commit、retire、abort）。下表按同一顺序列出，编号 10/11、14/15、21/23 各是两个独立变体，只是合并了标题。**每个帧的字段名与可选性都必须逐字保留**；`isFrame` 用 `Value.Check` 校验，失败即协议违规。
共享子 schema（逐字）：`Id = Type.Integer({minimum:1})`、`Name = Type.String({minLength:1})`、`Reload = Type.Integer({minimum:1})`、`List = (item) => Type.Immutable(Type.Array(item))`、`RootRefSchema = {rootId: Name, codeVersion: integer≥1}`、`WireErrorSchema = {code: Type.Enum(CHORD_ERROR_CODES), message: string, reason?: string}`、`WireReloadOptionsSchema = {remove?: string[], gateDeadline?: number, retireDeadline?: number}`、`WireReloadReportSchema = {codeVersion: integer, previous: integer, changed: string[], removed: string[], prepared: string[], gated: string[]}`。注意 `codeVersion` 在 `hello` 上是 **≥0**，在 `call.root`/`prepare`/`retire` 上是 **≥1**；`ping`/`pong` 的 `n` 是**无下界**的 `Type.Integer()`。

```jsonc
// 1. hello（双方，连接建立时立即发送，先于一切）
{ "t": "hello", "protocol"?: int≥1, "name": string≥1, "kind": "host"|"client",
  "codeVersion": int≥0, "acks"?: boolean }

// 2. ack（接收方 → 发送方；不计入 #received）
{ "t": "ack", "n": int≥1 }                       // 已处理帧的累计数（不含 ack 帧）

// 3. hold（consumer → provider）
{ "t": "hold", "id": int≥1, "at": string≥1, "token": <Description>,
  "input": unknown, "principal"?: unknown, "origin"?: string≥1,
  "state"?: false, "again"?: true }

// 4. release（consumer → provider）
{ "t": "release", "id": int≥1 }

// 5. shape（provider → consumer）
{ "t": "shape", "id": int≥1, "variant"?: string, "members": string[],
  "contract": string, "state": string[] }        // state: 随后会有 snapshot 的成员名

// 6. gone（provider → consumer）
{ "t": "gone", "id": int≥1, "error": <WireError> }

// 7. call（consumer → provider）
{ "t": "call", "id": int≥1, "hold": int≥1, "member": string≥1,
  "args"?: unknown, "root"?: { "rootId": string≥1, "codeVersion": int≥1 } }

// 8. result（provider → consumer）
{ "t": "result", "id": int≥1, "value"?: unknown, "error"?: <WireError> }

// 9. cancel（consumer → provider）
{ "t": "cancel", "id": int≥1 }

// 10. sub / 11. unsub（consumer → provider）
{ "t": "sub", "hold": int≥1 }
{ "t": "unsub", "hold": int≥1 }

// 12. resync（consumer → provider，经中继）
{ "t": "resync", "hold": int≥1, "m": string≥1 }

// 13. state（provider → consumer）
{ "t": "state", "hold": int≥1, "m": string≥1,
  "as": "snap"|"reset"|"ops"|"error",
  "value"?: unknown, "ops"?: unknown[][], "error"?: <WireError> }

// 14. ping / 15. pong（双方）
{ "t": "ping", "n": integer }
{ "t": "pong", "n": integer }

// 16. bye（双方；由 link 自身消费）
{ "t": "bye", "reason"?: string }

// 17. root_end（双方）
{ "t": "root_end", "rootId": string≥1 }

// 18. invalidate（parent → child）
{ "t": "invalidate", "pattern": { "<key>": unknown } }

// 19. prepare（parent → child）
{ "t": "prepare", "reload": int≥1, "codeVersion": int≥1,
  "plugins": string[],
  "options": { "remove"?: string[], "gateDeadline"?: number, "retireDeadline"?: number } }

// 20. vote（child → parent）
{ "t": "vote", "reload": int≥1, "error"?: <WireError>,
  "report"?: { "codeVersion": integer, "previous": integer, "changed": string[],
               "removed": string[], "prepared": string[], "gated": string[] } }

// 21. commit / 23. abort（parent → child）
{ "t": "commit", "reload": int≥1 }
{ "t": "abort", "reload": int≥1 }

// 22. retire（parent → child）
{ "t": "retire", "codeVersion": int≥1 }
```
`WireError = { "code": <one of 12>, "message": string, "reason"?: string }`。

**方向约束**（`#dispatch` 强制）：`hold/release/call/cancel/sub/unsub/resync` 由本端作为 provider 处理（consumer → provider）；`shape/gone/result/state` 由本端作为 consumer 处理（provider → consumer）；`invalidate/prepare/commit/abort/retire` 五个控制帧在 `role !== "upstream"` 时调用 `#violation(`${frame.t} from a child`)` 并关闭链路（**只从 parent 来**）；`vote` 在 `role !== "downstream" || peer.kind !== "host"` 时 `#violation("vote from a non-participant")`；`root_end` 只从 root 来的那条链路被采信（`#admitted` 过滤 + `host.foreignRootEnded`）。`hello` 走 `#onHello`；`ping` 立即回 `pong`（`{t:"pong", n: frame.n}`）；`pong`/`bye`/`ack` 在 `#dispatch` 内无动作（`ack` 在 `#receive` 里更早被 `#onAck` 处理，且**不**计入 `#received`）；`cancel` → `incoming.get(id)?.abort(new ChordError("aborted","cancelled by caller"))`。每次分发后**先** `#scheduleAck()`。

**`state` 帧的三态语义**：
- `as: "snap"` / `"reset"`：`value` 缺席表示值就是 `undefined`；
- `as: "ops"`：`ops` 是 `WireOp[]` 数组的数组（每条 op 本身是数组），由流的 delta encoder 编码、decoder 逐条校验；
- `as: "error"`：`error` 是 `WireError`；consumer 侧把它当作该成员的终止错误。

**`shape.state`**：名字列表，指明"紧跟在此帧之后会收到哪些成员的 snapshot"；consumer 在收齐这些 snapshot 后才应用 shape（`binding.shaped`）。

### 4.4 流控（可观察的线行为）

- **窗口**：`window` 默认 `256 * 1024` 字节（`DEFAULT_WINDOW`）；`pendingCap` 默认 `4 * window`。
- **计数对象**：除 `ack` 外的**所有**发出帧；`size = link.send(frame) ?? 0`（IPC 恒 0 → 不受窗口限制；MemoryLink 返回 `data.length`；WebSocket 返回文本 `.length` 或二进制 `.byteLength`）。
- **仅当双方 hello 都带 `acks: true`** 才启用流控（`#peerAcks`）。
- **ack 语义**：`ack.n` 是"已处理的接收帧累计数（不含 ack 帧）"。收到 `ack` 时若 `n < acked || n > sent` → 违规 `` ack ${n} outside ${acked}..${sent} ``；否则释放 `unacked` 队列前 `n - acked` 个大小。
- **ack 节奏**：每批接收后 `setTimeout(0)` 发一个 ack（**一个 burst 一个 ack**）。
- **满窗**：`unackedBytes >= window` → 停止 flush；`state` 帧的 slot 累积 ops 直到 `pendingCap`，超了就丢弃最旧的 state slot（有 `resend` 则标 lazy → 之后发 `resync` 让对端重发；否则调用 `overflow.resync()`）。
- **reset 取代 ops**：同 `(hold, m)` 的 slot 遇 `reset/snap` 直接替换 base 并清空累积 ops；`lazy` 期间收到的 ops 直接丢弃。
- **unit 帧**（`shape` + 紧随的 snapshot）在窗口满时**整体延后**（slot kind = unit，保持顺序）。
- **心跳**：每 `pingInterval`（默认 15000 ms）发 `{t:"ping", n: ++n}`；`Date.now() - lastSeen > pingTimeout`（默认 45000 ms）→ `link.close("ping timeout")`；定时器 `unref()`。
- **最大并发调用**：`AcceptOptions.maxCalls` 默认无穷；超出时回 result error `unavailable/too_many_calls`，消息 `` Too many calls in progress on this connection (${max}); wait for earlier ones to finish ``。

### 4.5 Node IPC 包装

Chord 帧在 IPC 上包一层：
```json
{ "chord2": <Frame 或 packObject 后的对象> }
```
非 `chord2` 消息（含 `{chord2control: ...}`）忽略。

### 4.6 控制消息（manager，与帧共享同一条 IPC 通道）

```json
{ "chord2control": { "type": "init", "value": <unknown> } }
{ "chord2control": { "type": "ready" } }
{ "chord2control": { "type": "fatal", "message": "<string>", "reason"?: "<string>" } }
{ "chord2control": { "type": "shutdown" } }
{ "chord2control": { "type": "status", "activeCalls"?: <n>, "viewers"?: <n>, "durableTasks"?: <n> } }
```
- `init` 必须是子进程收到的**第一条**消息。
- manager 侧对每个 `status` 计算 `Liveness { booting, activeCalls, viewers, durableTasks, idleFor }`。
- 子进程 `fatal` 拒绝 `start()`，消息 `` child failed: ${message} `` 且保留 `reason`。
- 协议默认值：`checkInterval = 1000`、`grace = 5000`、`readyTimeout = 30000`；`keepAlive` 默认 false。

### 4.7 SQLite 磁盘格式（逐字 DDL）

**Schema 版本表**（`applySqliteMigrations`，`options.table` 由调用方给出）：
```sql
CREATE TABLE IF NOT EXISTS <table> (
  singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
  version INTEGER NOT NULL CHECK (version >= 0)
) STRICT;
INSERT OR IGNORE INTO <table> (singleton, version) VALUES (1, 0);
SELECT version FROM <table> WHERE singleton = 1;
UPDATE <table> SET version = ? WHERE singleton = 1;
```
执行顺序（逐字）：先 `validateSqliteSchemaPrefix(table)`（表名也要过前缀正则），再逐条检查 `migrations[index].version === index + 1`，否则 `Error("SQLite migrations must have contiguous versions starting at 1")`；然后**在 `database.transaction(...)` 内**依次：建表 → `INSERT OR IGNORE` → `SELECT version` → 校验 → 逐条迁移。
- 行缺失 → `Error("SQLite schema metadata is missing")`。
- `currentVersion = migrations.at(-1)?.version ?? 0`；`row.version > currentVersion` → `` Error(`SQLite schema version ${row.version} is newer than supported version ${currentVersion}`) ``。
- 只执行 `migration.version > row.version` 的迁移；每条迁移先跑完所有 `statements`（逐个 `exec`），再 `run(UPDATE … version = ?, migration.version)`（**一个迁移一个版本号写入**）。
- 全部在**一个事务**内完成（建表 + 逐条 DDL + 逐条 UPDATE）。

**Telemetry schema**（`table = "telemetry_schema"`，迁移 v1，5 条语句）：
```sql
CREATE TABLE telemetry_spans (id TEXT PRIMARY KEY, trace_id TEXT NOT NULL, parent_id TEXT, name TEXT, started_ms REAL,
  ended_ms REAL NOT NULL, attributes TEXT, events TEXT, links TEXT, status TEXT);
CREATE INDEX telemetry_spans_ended_ms ON telemetry_spans(ended_ms);
CREATE INDEX telemetry_spans_trace_started ON telemetry_spans(trace_id, started_ms);
CREATE INDEX telemetry_spans_started_ms ON telemetry_spans(started_ms);
CREATE TABLE telemetry_metrics (interval_ms INTEGER, series_key TEXT, name TEXT, kind TEXT,
  unit TEXT, attributes TEXT, count INTEGER, sum REAL, min REAL, max REAL, PRIMARY KEY(interval_ms, series_key));
```
列顺序即插入参数顺序（10 列，`INSERT INTO telemetry_spans VALUES (?,?,?,?,?,?,?,?,?,?)`）。

**行内容编码**：
- `id` = `` `${scope}:${sequence}` ``，`scope = crypto.randomUUID()`（UUID v4 字符串）。
- `trace_id` = 父 traceId 或自身 id。
- `parent_id` = 父 span id 或 `NULL`。
- `started_ms` = `Date.now()`；`ended_ms` = `started_ms + (performance.now() 差值)`（REAL）。
- `attributes` / `events` / `links` / `status` = `JSON.stringify` 的文本（列类型 TEXT，但未声明 STRICT，可存 NULL）。
- `events` 元素：`{ "name": string, "time_ms": number, "attributes": {} }`（**相对 span 起点**）。
- `links` 元素：`{ "trace_id": string, "span_id": string, "attributes"?: {} }`。
- `status`：`{ "status": "ok" }` 或 `{ "status": "error", "error"?: { "name": string, "message": string } }`。
- `attributes` JSON 的键顺序 = 首次 `setAttributes` 合并的顺序（`{...old, ...new}`），但 **metric 的 attributes 键排序**。

**Metric 行**：`interval_ms = floor(now/10000)*10000`；`series_key = JSON.stringify([name, kind, unit, encodedAttributes])`，其中 `encodedAttributes = JSON.stringify(排序键后的属性对象)`（`unit` 为 `null` 时键里是 `null`，行里 `unit` 列也是 NULL）。

**Store 查询 SQL**（`store.ts`）：
- `getTrace`：`SELECT … FROM telemetry_spans WHERE trace_id = ? ORDER BY started_ms, id`。
- `findTraces`：heads CTE `min(started_ms)` 分组；`ORDER BY started_ms DESC, trace_id`；`LIMIT limit + 1`；游标条件 `(started_ms < ? OR (started_ms = ? AND trace_id > ?))`；trace id 集合用 `json_each(?)`。
- 属性过滤（traces 与 metrics 共用 `attributesSql`）：每个属性一个 `EXISTS (SELECT 1 FROM json_each(<attributes column>) entry WHERE entry.key = ? AND …)`：
  - number → `entry.type IN ('integer', 'real')` 且 `entry.value = ?`（**逗号后有一个空格**，与 `store.ts` 第 55 行逐字一致；traces 与 metrics 共用同一个 `attributesSql`，全文**只有**这一个写法）；
  - boolean → `entry.type = 'true'` / `'false'` 且绑定 `Number(value)`；
  - string → `entry.type = 'text'` 且 `entry.value = ?`。
- `findMetrics`：`ORDER BY interval_ms, series_key`。

### 4.8 进程锁（可观察的文件系统/内核对象）

`key = sha256(name) 的 hex，前 32 字符`。地址：
| 平台 | 地址 |
|---|---|
| win32 | `\\.\pipe\chord2-<key>`（命名管道） |
| linux / android | `\0chord2-<key>`（**抽象命名空间** socket） |
| 其它（macOS/BSD） | 文件 `chord2-<key>.sock`，优先 `os.tmpdir()`，路径字节 > 103 时改用 `/tmp/chord2-<uid>/`（mode 0700，属主与权限校验） |

- 锁是一个 `net.Server`，连接到来立即 `socket.end()`。
- `EADDRINUSE` 且（非文件形式 **或** 对端仍接受连接）→ `ChordError("unavailable", `lock ${name} is held by another process`, { reason:"locked" })`；文件形式且无人接受 → 删除陈旧文件后重试。
- 文件形式在 `process.once("exit")` 时删除。
- `accepting(address)`：`connect` 成功 → true；错误码不是 `ECONNREFUSED`/`ENOENT` → true；否则 false。

### 4.9 Root 标识

`rootId` 格式【逐字】：`` `${host.name}#${incarnation}:${++rootSeq}` ``（**是 host 名，不是 facet 名**）。`incarnation = Math.random().toString(36).slice(2, 8)`（6 个小写字母数字，host 构造时一次），`rootSeq` 从 0 单调递增。任务 root 的 id 为 `` `task:${address}:${key}` ``。客户端 originId 为 `` `${incarnation}:${++originSeq}` ``（`originSeq` 从 0 起）。这些字符串跨线传输（`call.root.rootId`、`root_end.rootId`），**必须保持格式**（`incarnation` 的字符集与长度也是可观察的）。

### 4.10 地址与 identity 键

- **引用地址**（内部，不跨线）：`` `${name}\u0000${identity}\u0000${principalPart}` ``，其中 `identity = canonicalIdentity(schema, normalized)`（**省略等于默认值的字段**），`principalPart` 为 `"host"`（principal 是 host 自己的 `self`）或 `` `p:${principalKey(principal)}` ``（默认 `principalKey = canonicalJson`）。host lifetime 的 principal 恒为 host 自身，且输入必须是空输入。
- **远程绑定键**（内部，不跨线）：`` `@${type.name}\u0000${identity}\u0000${principalPart}\u0000${type.fingerprint}` `` —— **没有 host 名**（类型 + 输入决定它住在哪）。
- 对象地址里**没有** `#n` 后缀：同一 (type, input, principal) 恒为同一对象；不同 input 由 identity 区分。
- state slot 键：`` `s${hold}\u0000${m}` ``；unit slot 键：`` `u${hold}` ``；registration 键：`` `${registry.name}\u0000${key}` ``。

---

## 5. 跨单元接口

### 5.1 本单元消费的外部类型（`@earendil-works/kiana-telemetry@1.4.0`）

`chord` 对 telemetry 的依赖面很小，但**类型必须逐个对齐**（Rust 侧需要一个 `kiana-telemetry` crate 或等价的 trait 集）：

```ts
type AttributeValue = string | number | boolean | readonly string[] | readonly number[] | readonly boolean[];
interface SpanAttributes { [name: string]: AttributeValue | undefined }
type TelemetryCarrier = Record<string, string>;
interface TelemetryPropagation {
  inject(context: TelemetryContext): TelemetryCarrier | undefined;
  extract(carrier: TelemetryCarrier | undefined): TelemetryContext;
}
interface SpanLink { context: TelemetryContext; attributes?: SpanAttributes }
interface SpanOptions { name: string; attributes?: SpanAttributes; links?: readonly SpanLink[] }
type SpanStatus = { status: "ok" } | { status: "error"; error?: { name: string; message: string } };
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
interface InstrumentOptions { unit?: string; description?: string }
type TelemetryMetrics = {
  counter(name: string, options?: InstrumentOptions): { add(value: number, attributes?: SpanAttributes): void };
  upDownCounter(name: string, options?: InstrumentOptions): { add(value: number, attributes?: SpanAttributes): void };
  histogram(name: string, options?: InstrumentOptions): { record(value: number, attributes?: SpanAttributes): void };
};
// store 侧
interface StoredSpan { id; traceId; parentId?; name; startedMs; endedMs; attributes; events; links; status }
interface StoredTrace { traceId: string; spans: readonly StoredSpan[] }
interface TraceQuery { name?; attributes?; errors?: true; startMin?; startMax?; limit?; cursor? }
interface MetricQuery { name: string; startMin?; startMax?; attributes? }
interface StoredMetric { intervalMs; name; kind; unit?; attributes; count; sum; min; max }
interface TelemetryStore {
  findTraces(query?: TraceQuery): Promise<{ traces: readonly StoredTrace[]; cursor?: string }>;
  getTrace(traceId: string): Promise<StoredTrace | undefined>;
  findMetrics(query: MetricQuery): Promise<readonly StoredMetric[]>;
}
```
`chord` 直接用到的符号：`SpanAttributes`、`SpanLink`、`SpanOptions`、`SpanStatus`、`TelemetryCarrier`、`TelemetryContext`、`TelemetryMetrics`、`TelemetryPropagation`、`TelemetrySpan`、`TelemetryStore`、`InstrumentOptions`、`NOOP_TELEMETRY_CONTEXT`（`telemetry-sqlite/index.ts` 的 inert 路径）。

`context/index.ts` 中的 `TELEMETRY_CONTEXT_KEY: ContextKey<TelemetrySpan> = Object.freeze({ token: Symbol("chord.telemetry") })` 把 `TelemetrySpan` 挂在 `Context` 上；`withTelemetry(telemetry, context)`（**参数顺序是 telemetry 在前、context 在后**）返回 `withContextValue(TELEMETRY_CONTEXT_KEY, telemetry, context)`；`traceIn(options, fn, context)`（**三个参数依次为 options、fn、context**）= `context.telemetry.startSpan(options, span => fn(withTelemetry(span, context)))`。**注意：`traceIn` 没有"无 telemetry 就直调 `fn`"的分支**——`BaseContext.telemetry` getter 在 `value(TELEMETRY_CONTEXT_KEY)` 为 `undefined` 时回退到 `NOOP_TELEMETRY_CONTEXT`，因此永远走 `startSpan`（NOOP 实现的 `startSpan` 直接 `await` 回调）。

**Rust 建议**：定义 `trait TelemetryContext { fn start_span<T>(&self, options: SpanOptions, cb: impl FnOnce(TelemetrySpanRef) -> BoxFuture<T>) -> BoxFuture<T>; fn metrics(&self) -> Option<&dyn TelemetryMetrics>; fn propagation(&self) -> Option<&dyn TelemetryPropagation> }`，并为 `NOOP` 提供零大小实现；`startSpan` 的回调是 **async-aware** 的（回调可返回 Promise），Rust 需要用 `Pin<Box<dyn Future>>` 或 async-trait。

### 5.2 本单元暴露给下游的类型（`durable`、`env`）

实际 import 统计（`durable/src` + `env/src`）：

| 子路径 | 符号 | 次数 |
|---|---|---|
| `@earendil-works/chord` | `copyJson` | 10 |
| | `type Context` | 10 |
| | `type JsonValue` | 9 |
| | `type Draft` | 6 |
| | `type JsonRepresentation` | 4 |
| | `Watch` | 3 |
| | `type WatchHandle` | 2 |
| | `NativeWatch` | 1 |
| | `type WatchEnd` | 1 |
| `@earendil-works/chord/delta` | `type Op` | 7+ |
| | `type Draft` | 4 |
| | `track` | 2 |
| | `ownedSlice` | 3 |
| | `applyImmutable` | 2 |
| | `apply` | 2 |
| | `appendText` | 2 |
| | `type Tracker` / `type Prepared` / `type PendingOps` / `type Path` / `type NonEmptyPath` / `type Change` / `PendingOps` / `overlap` / `applyImmutableBatches` | 各 1–2 |
| `@earendil-works/chord/context` | `BACKGROUND_CONTEXT`、`withAbortSignal`、`withoutAbortSignal`、`awaitWithContext`、`withTelemetry` | 2–6 各 |
| `@earendil-works/chord/sqlite` | `validateSqliteSchemaPrefix` 等 | 8 |
| `@earendil-works/chord/output` | `utf8ByteLength`、`characterEnd` 等 | 3 |
| `@earendil-works/chord/sqlite/node` | `openNodeSqliteDatabase` | 2 |
| `@earendil-works/chord/sqlite/cloudflare` | `DurableObjectSqliteDatabase` | 2 |

**必须在 Rust 侧保持签名稳定的核心类型**：
- `copyJson(value, { omitUndefinedProperties? }) -> JsonValue`（严格 JSON、无别名、保留 null 原型语义可忽略但键集合与顺序必须一致）；
- `Watch<T>` / `NativeWatch<T>` / `WatchHandle<T>` / `WatchEnd`（`watch(ctx)` 返回 handle，`start(listener)` 后串行投递）；
- `Draft`（overlay 草案的类型面：`track` 返回 `Tracker`，`beginChange()` 返回 `Change`，`change.state` 是 `Draft`，`prepare()` 返回 `Prepared`，`abort()`）；
- `Op` / `WireOp` / `Path` / `NonEmptyPath`（路径是 `(string|number)[]`；`NonEmptyPath` 是类型级非空）；
- `ownedSlice(text, start, end)`、`overlap(a, b, scan?, probe?, maxCandidates?)`、`appendText(tracker, path, text)`、`apply`、`applyImmutable`、`applyImmutableBatches`、`PendingOps`；
- `BACKGROUND_CONTEXT`（`toString()` 必须是 `"[Context BACKGROUND_CONTEXT]"`）。

### 5.3 本单元内部的跨模块契约（Rust 模块边界）

| 依赖方向 | 类型 |
|---|---|
| `contract` ← `host`/`object`/`connection` | `AnyContract`、`Description`、`Placement`、`resolveHost`、`coverage`、`bind`、`MEMBER`、`RESERVED_MEMBERS`、`LOCAL_VARIANT` |
| `schema` ← 所有 | `canonicalJson`、`hash64`、`deepEqual`、`assertValid`、`normalize`、`canonicalIdentity`、`fits` |
| `delta` ← `cell`/`state`/`watch`/`object`/`connection`/`remote` | `Op`、`WireOp`、`encoder`、`decoder`、`diffRevisions`、`applyImmutable`、`PendingOps`、`track`/`Tracker`/`Prepared`/`Change`、`JsonRevisionValidator` |
| `publish` ← `cell`/`state`/`watch` | `enqueue`、`holdPublications`、`drain` |
| `connection` ← `host`/`remote`/`cluster` | `Connection`、`RelayRef`、`RelayCall`、`LinkOptions`、`AcceptOptions`、`Peer`、`relayCall`、`clientError`、`validateSnapshot` |
| `host` ← `object`/`remote`/`reload`/`cluster` | `HostImpl`、`HostCore`、`Facet`、`FacetApi`、`PluginRef`、`CodeVersionState`、`Serve`、`RefCore`、`ObjectImpl` |
| `object` ← `reload`/`host` | `ServeOptions`、`makeService`、`hostLifetime`、`Self`、`ResourceImpl`、`StagedListeners`、`Run`、`Installed`、`Decls` |
| `remote` ← `host`/`connection` | `RemoteBinding`、`RemoteRef`、`ViewReadable`、`MemberState` |
| `wire` ← `connection`/`cluster` | `Frame`、`WireError`、`toWireError`、`fromWireError`、`PROTOCOL_VERSION`、`isFrame`、全部 `*Schema` |

---

## 6. npm 依赖与 Rust crate 对应

### 6.1 运行时依赖

| npm | 版本 | 用途 | 建议 Rust crate | 必须弥合的行为差异 |
|---|---|---|---|---|
| `typebox` | 1.3.27 | 所有 schema 定义、`Value.Check/Errors/Clean/Default/Clone`、`Type.*` 构造器 | **自写 `jsonschema` 子集**（见下） | TypeBox 的错误文案、`Value.Clean` 的"移除 additionalProperties"、`Value.Default` 的默认值注入、`Value.Clone` 的深拷贝语义、`Type.Unsafe`、`Type.Immutable`（仅类型）、`Type.Function/Void/Unknown/Literal/Enum/Record/Union/Integer`。第三方 `jsonschema` crate 的默认值/清洗行为不匹配，**不可直接替代**。 |
| `@earendil-works/kiana-telemetry` | 1.4.0 | telemetry 类型与 NOOP 实现 | 内部 crate `kiana-telemetry` | `TelemetryContext.startSpan` 的 callback 可返回 Promise；`SpanAttributes` 的数组类型；`NOOP_TELEMETRY_CONTEXT`。**测试还用其 `testing` 子路径**：`packages/telemetry/src/testing/conformance.ts`（386 行）导出的 `createTelemetryAdapterConformance` 被 `test/telemetry-sqlite.test.ts` 取用（14 例去掉 `metrics` 组 2 例 = 12 例），Rust 侧需要一个等价的 `kiana-telemetry::testing`（见 §7 与 §8.6 第 17 条）。 |

### 6.2 Node 内置模块（浏览器构建里以 `globalThis.process?.getBuiltinModule` 探测）

| Node API | 用途 | Rust 替代 | 差异 |
|---|---|---|---|
| `node:zlib`（`deflateRawSync`/`inflateRawSync`） | payload 压缩 | `flate2`（`DeflateEncoder` with `Compression::fast()`） | 必须生成/接受 **raw deflate**（无 zlib 头尾）；level 1 对应 `Compression::fast()`，但**压缩字节流不要求与 zlib 逐字节相同**（对端只要能解压即可）。若要逐字节一致（测试对比），需绑定 zlib。 |
| `node:crypto`（`createHash("sha256")`） | 进程锁键 | `sha2` | 无差异（取 hex 前 32 字符）。 |
| `crypto.randomUUID()` | telemetry scope | `uuid` v4 | 格式一致（小写带连字符）。 |
| `node:child_process`（`fork`） | manager | **不能等价移植**：需要 Node 运行时。Rust 端应改为 `tokio::process::Command` + 自定 IPC（长度前缀 JSON 或 stdio JSONL），并把 `ChildProcess` IPC 语义用 `--experimental` 的 Node 子进程边界适配。**这是唯一无法在纯 Rust 中保持"同一 wire"的模块**（IPC 是 Node 私有通道）。 | 见 §8.6。 |
| `node:net`（`createServer`/`connect`） | 进程锁 | `tokio::net::UnixListener` + Linux `abstract socket`（`std::os::linux::net::SocketAddrExt`） | 抽象命名空间只有 Linux/Android；Windows 用命名管道（`tokio::net::windows::named_pipe`）。 |
| `node:fs/promises`（`mkdir`,`lstat`,`rm`）、`node:os`（`tmpdir`）、`node:path` | 锁文件路径 | `std::fs`, `std::env::temp_dir`, `std::path` | `tmpdir()` 的取值顺序（`TMPDIR`/`TMP`/`TEMP`/`/tmp`）需照抄。 |
| `node:sqlite`（`DatabaseSync`） | sqlite/node.ts | `rusqlite`（bundled） | `DatabaseSync` 的 `timeout` 选项 → `busy_timeout`；`PRAGMA` 语句序列必须一致；`statement.all()` 的行对象键名与 BLOB 类型（Uint8Array）需映射。 |
| `Buffer`（`byteLength`,`toString("base64")`,`from(base64)`,`from(arrayBuffer)`） | output/link | `base64` crate + 手写 `utf8_len` | base64 标准表、无填充差异（必须有 `=` 填充）。`Buffer.byteLength` = UTF-8 长度且孤立代理算 3 字节——Rust `str` 无孤立代理，需在"从 JS 来的字符串"路径上保留 WTF-8 或先转义。 |
| `TextEncoder`/`TextDecoder`（UTF-8，`ignoreBOM`） | output/link | `encoding_rs` 或 `String::from_utf8` | `TextDecoder` 的"流式解码 + 不完整字符留待下次"与"`decode()` 冲成 U+FFFD"必须实现；`ignoreBOM:true` 表示**不剥离** BOM（与默认相反），而 `OutputBuffer` 自己只在首块剥一次 BOM。 |
| `DecompressionStream("deflate-raw")` + `Blob` + `Response` | 浏览器解压 | `flate2` | 无需移植（有 zlib 的路径覆盖）。 |
| `performance.now()` | span 结束时间、事件时间 | `std::time::Instant` | 与 `Date.now()` 的**混用**语义要保留（`ended = started + (perf_end - perf_start)`）。 |
| `structuredClone` | reload 迁移复制 | `serde_json::Value::clone` | JS 的 `structuredClone` 支持 Map/Set/Date/TypedArray 等；Chord 状态是严格 JSON，因此可用 JSON 深拷贝，但 `undefined` 键的处理要与 `copyJson` 一致。 |
| `Promise.withResolvers()` | lifecycle/manager | `tokio::sync::oneshot` 或自定义 deferred | 无差异（语义为"立即拿到 promise + resolve/reject"）。 |
| `AbortSignal` / `AbortController` / `AbortSignal.any` / `DOMException("…","AbortError")` | ctx/取消 | `tokio_util::sync::CancellationToken` + `futures::future::select` | `signal.reason` 是任意值；`AbortSignal.any([a,b])` 需要组合；`DOMException` 的 `name === "AbortError"`、`message === "The operation was aborted"` 必须复刻。 |
| `WeakRef` / `WeakMap` / `WeakSet` | tracker/registry/telemetry identities | `std::sync::Weak` / `slotmap` + 显式回收 | Rust 无 GC；需为 `Tracker` 的 `#contexts: WeakRef[]` 与 `identities: WeakMap` 设计显式生命周期（`Arc`/`Weak` 或 `slotmap` 代次）。**这是语义最容易偏离的地方**（见 §8）。 |
| `queueMicrotask` | publish/watch/state | `tokio::task::yield_now` 或 `LocalSet::spawn_local` | 顺序：`queueMicrotask` 在当前同步栈之后、`setTimeout(0)` 之前。Rust 端需用一个显式的"微任务队列"来复刻（不能用 `spawn` 的调度顺序）。 |
| `setTimeout(…).unref()` | 心跳、linger、readyTimeout、vote | `tokio::time::sleep` + 分离任务 | `unref` 的含义是"不阻止进程退出"；Rust 中对应"不阻止 runtime 关闭"，通常可直接忽略，但 manager 的 `keepAlive` 检查需要保留。 |
| `node:worker`? | 无 | — | — |
| `ws` (devDep) | 测试用 WebSocket 服务器 | `tokio-tungstenite` | 仅测试。 |

### 6.3 建议引入的 crate

| crate | 用途 | 备注 |
|---|---|---|
| `tokio`（`rt`,`sync`,`time`,`net`,`process`,`io-util`） | 异步运行时、通道、定时器 | 单线程 `LocalSet` 更贴近 JS 语义（无并行回调）。 |
| `serde` + `serde_json`（`preserve_order`） | JSON | **必须开 `preserve_order`**（`IndexMap`），否则对象键顺序丢失 → canonicalJson 与 fingerprint 全变。 |
| `indexmap` | 有序 map | 对象键顺序、registry 顺序、encoder 字典。 |
| `smallvec` / `bytes` | 缓冲 | 可选。 |
| `flate2`（`rust_backend` 或 `zlib-rs`） | raw deflate | 见 §6.2。 |
| `sha2` | 进程锁 | — |
| `uuid`（v4） | telemetry scope | — |
| `rusqlite`（`bundled`, `backup` 可选） | SQLite | 需 `bigint`/`u64` 绑定精度处理（对应 Cloudflare 的 RangeError 规则）。 |
| `base64` | IPC 打包 | 标准表、带填充。 |
| `thiserror` | 错误定义（可选） | 但错误 **message 必须手写**（见 §3）。 |
| `tracing` | 日志（可选） | **不要**用它替代 `onError`/`report`。 |
| `futures` | 组合子 | `select`, `join_all`。 |
| `parking_lot` | 同步原语 | 可选。 |
| `arc-swap` / `dashmap` | 并发表 | 若不用单线程 actor 模型。 |

**不建议**：`jsonschema`（默认值/清洗语义不符）、`js-sys`/`wasm-bindgen`（本单元是 Node 侧库）、`axum`/`hyper`（chord 不实现 HTTP，WebSocket 是**被动接受**外部 server 的连接）。

### 6.4 JS 语义差异清单（必须逐项决策）

| JS 行为 | 影响 | Rust 对策 |
|---|---|---|
| **字符串是 UTF-16 码元序列** | `delta` 的 `t`（truncate count）、`overlap`、`slice`、`length`、`charCodeAt`、`hash64`、`json.length <= PACK_THRESHOLD` 全部按 UTF-16 计 | 内部统一用 `Vec<u16>`/`Utf16Str`（crate `widestring`/`utf16string`）或自定义 `JsString`；**不要**直接用 `String` 的长度。输出/编码处再转 UTF-8。孤立代理必须可表示（WTF-8 或 `Vec<u16>`）。 |
| **对象键顺序**：整数键（数组索引形式）按数值升序在前，其余按插入顺序 | `canonicalJson`、`Object.keys`、`ownKeys`、JSON.stringify | `IndexMap` + 自定义序列化：先输出可解析为数组索引的键（按数值排序），再输出其余（插入序）。 |
| **JSON.stringify 数字格式**：最短往返表示；`-0` → `0`；`1e21` → `1e+21`；`1e-7` → `1e-7`；整数无小数点 | canonicalJson / 指纹 / 线协议 | 用 `ryu` 的 JS 兼容模式或 `serde_json` 的 `float_roundtrip`；逐步对照 ECMAScript `Number::toString`（需要自写 `js_number_to_string`，参考 `ryu-js`）。 |
| **JSON.stringify 字符串转义**：`"`、`\\`、`\b\f\n\r\t`、`\u00XX`（小写 hex？实际 `String.fromCharCode` → `\u0000`…`\u001f`，字母小写） | 线上/磁盘文本逐字 | 自写 `escape_json_string`，控制字符用 `\u00xx`（4 位小写十六进制）。 |
| **`Math.imul` 是 32 位有符号乘法后取低 32 位** | `hash64` | `u32::wrapping_mul`（与无符号低 32 位等价）。 |
| **`>>>` 是无符号右移** | `hash64` 的 `b >>> 13` | `u32` 右移。 |
| **正则方言**：`/^[^@\s]+@\d+$/`、`/^[a-z_]+(\/[a-z_]+)?: /`、`/[\x00-\x08\x0b-\x1f\ufff9-\ufffb]/g`、`/^[A-Za-z_][A-Za-z0-9_]*$/`、`new RegExp(pattern,"u")`（patternProperties） | contract/schema/output/sqlite | 简单者手写；patternProperties 需要 `regex` crate 并映射 JS 的 `u` 标志差异（`\p{…}` 支持不同）。 |
| **`Array.prototype.sort` 默认把 `undefined` 排到末尾、字符串按 UTF-16 码元比较** | delta tracker 的 `sort()`、attribute key 排序 | 自定义比较器，逐码元比较。 |
| **`Array.prototype.sort` 是稳定的** | sort 的 op 生成 | Rust `sort_by` 稳定。 |
| **`Object.hasOwn` / 原型链 / `__proto__` 特殊语义** | json/tracker/reserved keys | 显式把 `__proto__` 当普通键；`Object.create(null)` 的原型差异需在 API 文档中声明（Rust 无原型）。 |
| **稀疏数组** | delta 校验明确拒绝 | 无需支持。 |
| **`-0`、`NaN`、`Infinity`** | `copyJson` 拒绝非有限数；`-0` 允许且 JSON 化为 `0` | 用 `f64` + 自定义序列化。 |
| **`structuredClone` 的 `undefined`** | reload 迁移 | 与 `copyJson` 对齐（省略 `undefined` 对象属性）。 |
| **`WeakMap`/`WeakSet`/`WeakRef`** | tracker、identities、overflowedBatches | 显式 `Arc`/`Weak` 或 `slotmap` 代次；`overflowedBatches` 可以用"返回结果里带标记"替代（**但不可观察**，可自由实现）。 |
| **微任务 vs 宏任务顺序** | publish 队列、Watch 投递、state 订阅 | 需要一个**显式微任务队列**（`VecDeque` + 每轮 drain），不能依赖 tokio 的 `spawn` 顺序。 |
| **`Date.now()` 毫秒、`performance.now()` 亚毫秒** | telemetry | `SystemTime` / `Instant`。 |
| **`Error.stack`** | 未被使用（仅 `Error.name/message` 进 telemetry） | 无需复刻。 |
| **`AggregateError.errors`** | `runAll`、sqlite rollback、attach | 定义 `struct AggregateError { errors: Vec<ChordError>, message: String }`。 |

---

## 7. 测试清单（53 个文件，682 个声明点 / 665 条字面标题 / 630 个运行时用例）

测试是行为的**权威来源**：下面按文件逐条列出标题（逐字，`describe` 容器标题与 `it`/`test` 用例标题都在内），并在每节前给出该文件断言的行为摘要。移植时每条用例都应有一个对应的 Rust 测试（名称可本地化，但断言必须一致）。

计数口径（三个不同的数，不要混用）：

1. **声明点 = 682**：`describe(…)` 111 + `describe.each(…)` 1 + `it(…)` 539 + `it.each(…)` 9 + `test(…)` 21 + `test.each(…)` 1。这一层按源码里字面出现的调用点数（`test` 与 `it` 等价；`it`/`test` 出现在 4 个文件：`boundary`、`host-objects-reload`、`json`、`replicated-state-delivery`）。
2. **字面标题 = 665**：682 − 1（`describe.each` 是容器，本身不产生标题）− 16（标题含 `${…}`/`%s`/`%i` 的参数化点）。逐文件小节的「N 条」即此口径，合计恰为 665。下表每条 `- ` 项目就是一个字面标题（`describe` 标题也在其中，按源码顺序与 `it`/`test` 混排）。
3. **运行时用例 = 630**：`vitest` 把参数化声明展开后的实际用例数（不含 describe 容器）。630 = 570 个非 describe 声明点 + 60 个展开增量，增量来自 8 个文件：`delta-diff`(+6)、`state-diff`(+6)、`delta-tracker/retention`(+14)、`delta-tracker/tracker`(+1)、`state-draft`(+1)、`readiness`(+7)、`telemetry-sqlite`(+11)、`replicated-state-delivery`(+14)。各参数化点的源码表达式、参数表与展开数见对应文件节末的「参数化」小节。

下表按文件列出**全部字面标题**（逐字，含 describe 标题）。带插值 / `%s` / `%i` 的 16 个参数化点无法静态给出唯一标题，统一在对应文件节末以「参数化」小节列出其源码表达式、参数表与展开数。移植时每个声明点都应有一个对应的 Rust 测试（名称可本地化，但断言必须一致）；参数化点要按展开数逐例覆盖。

除 `*.test.ts` 外，`test/` 下还有支撑文件（不产生用例，但移植时可能需要对应的 Rust 辅助设施）：
- `test/fixtures/`：`counting.ts`（2.2K）、`echo.ts`（327B）、`loader.ts`（3.5K）、`worker.ts`（1.5K）——跨 host/link 的测试脚手架。
- `test/sqlite/helpers.ts`：SQLite 测试的共享辅助（开库/迁移）。
- `packages/telemetry/src/testing/conformance.ts`（386 行，**跨包**）：`createTelemetryAdapterConformance` 提供 14 个 adapter 一致性用例；`test/telemetry-sqlite.test.ts` 取其中非 `metrics` 的 12 个（见该文件小节的「参数化」）。
- `test/delta-benchmark/`：`benchmark.ts` / `benchmark.worker.ts` / `conversation-view-benchmark.ts` / `conversation-view-benchmark.worker.ts` / `memory-benchmark.ts` / `memory-benchmark.worker.ts`（**基准，不是测试**，CI 不跑断言）。
- `test/delta-tracker/retention.worker.ts`（12.9K）：`retention.test.ts` 的 worker 端，验证 overlay 释放后的内存保留行为；它依赖 `WeakRef`/`FinalizationRegistry`/`--expose-gc`，**Rust 侧无法逐条等价**（见 §8.4 第 9 条）。
- `test/delta-traversal.bench.ts`（6.7K）：基准。

**测试侧的 Node 专属 API（仅测试用到，`src/` 里只有 `loader.ts` 用 `node:module`/`node:url`）**——Rust 测试要么找等价物，要么显式声明"该用例不可移植"：

| Node API | 出现位置 | 用途 | Rust 测试对策 |
|---|---|---|---|
| `node:v8`：`setFlagsFromString("--expose-gc")` + `runInNewContext("gc")` | `delta-pending`、`delta-append-text`、`delta-op-text-retention`、`generations`、`watch`（均在文件头建 `gc()` 辅助） | 在断言前强制 GC，验证 `WeakRef`/`FinalizationRegistry` 已回收 | Rust 无 GC；改为 `Drop` 后断言弱引用升级失败（`Weak::upgrade().is_none()`），或删掉该断言 |
| `node:v8`：`getHeapStatistics()` / `GCProfiler` | `delta-op-text-retention`、`delta-append-text` | 断言大字符串不再被 heap 保留 | 用分配计数/`Weak` 探针替代，数值断言不可移植 |
| `node:util/types`：`isProxy(v)` | `delta-tracker/tracker.test.ts`（5 处：`expect(isProxy(prepared.value)).toBe(false)` 等） | 断言 `Prepared.value` 是普通对象而非 Proxy | Rust 无 Proxy，该断言在 Rust 侧恒真，可直接删除 |
| `node:http`（仅 `type { IncomingMessage }`） | `websocket.test.ts` | `ws` 服务器的 request 类型 | 类型层，迁移时丢弃；`ws` → `tokio-tungstenite` |
| `node:perf_hooks`：`performance.now` | 6 个 benchmark/worker 文件 | 计时 | `std::time::Instant` |
| `node:timers/promises`：`setTimeout` | benchmark/worker、`retention.worker.ts` | 让出事件循环再采内存 | `tokio::time::sleep` |
| `node:url`：`pathToFileURL`/`fileURLToPath` | `loader*`、`manager`、`boundary`、`retention`、benchmark | 把 fixture 路径转 URL | `std::path::Path` + `url` crate |
| `node:assert/strict`（32 处） | 全部测试 | 断言 | `assert_eq!`/`assert!`（错误文案不必逐字） |

### `test/awaiting.test.ts`（5 条）

`ctx.get` 的等待语义：ready 后 resolve；ctx abort/调用结算 → aborted；readyTimeout → `unavailable/ready_timeout` 且释放 hold；调用结束后引用自动 release（其它引用仍在时 binding 保留）。

- awaited gets
- an aborted get rejects and releases only its reference; the binding stays for the others
- a ctx.get still waiting when its call settles rejects aborted and releases its reference
- a get times out after readyTimeout with ready_timeout and releases its hold
- ctx.get resolves hydrated; the call's reference is released when the call ends


### `test/bindings.test.ts`（18 条）

共享 binding：多引用一次 hold、每成员一次 snapshot、每次变更一帧、一次 release；linger 与 cap 淘汰（LRU）；identity 共享规则（规范化输入相等 + principalKey 相等 + 同指纹）；hydration（null/空/undefined 值、state-free variant、失败 snapshot reject、首次 hydration 前读 → not_ready）；`state:false` 的方法专用引用；client hold 的 origin；协议版本 4（v3 被拒、shape 不带 code version）。

- shared bindings: traffic
- N references to one object: one hold, one snapshot per member, one frame per change, one release
- a held reference stays subscribed without listeners; values are readable at once after ready
- shared bindings: linger
- a lingering binding keeps its hold and state; a reference taken meanwhile costs nothing on the link
- at most `cap` bindings linger: the least recently released goes first
- shared bindings: identity
- normalized-equal inputs and equal principal keys share; other principals and fingerprints do not
- hydration over a link
- null, empty and undefined values hydrate; a state-free variant is ready; a failed snapshot rejects
- reading before the first hydration throws not_ready
- method-only references (state: false)
- transfer no state alone; a full reference joining starts it once; its release stops it
- origins of client holds
- a relayed hold carries its client connection's id; the server's own holds carry none
- an origin a client sends is ignored: the server stamps its own
- protocol version 4
- a version 3 peer is refused; shapes carry no code version


### `test/boundary.test.ts`（2 条）

包边界：除 `kiana-telemetry` 外不依赖其它 Kiana 包，且不引用 Chord 之外的文件。

- package boundary
- does not depend on Kiana packages other than kiana-telemetry, or on files outside Chord


### `test/cluster.test.ts`（15 条）

跨 host reload：所有 host prepare、server 先 commit 再 worker；投票失败 abort 全部且错误保留 code 并带 host 名；worker gate 超时 abort 全局；facet 移除与 tombstone、重新添加时清除；只有 server 能协调；浏览器不参与（shape/reset 或 upgrade_required）；prepare 期间 abort（迟到的 open 自行关闭，可再次 reload）；reload 期间 hello 的子 host 在结束后 reconcile；子 host 保留上一版本直到 parent retire；reconcile 期间新一轮 reload；落后子 host 被立即 fence 但上行调用可放行；重连子 host 有上一版本时无需等 retire deadline；强制退役会取消经 server 中继到 worker 的浏览器调用。

- cross-host reload
- prepares every host, commits the server first, then the workers; code versions stay in step
- a failing vote aborts every host; the error keeps its code and names the host
- a worker gate blocked past its deadline aborts the whole reload
- removes facets from server and workers, reconciles removal tombstones, and clears them on re-add
- only the server coordinates reloads
- browsers are not participants: they see shape and reset pushes, or upgrade_required
- an abort while a worker is still preparing: a late open closes itself and the worker can reload again
- a child that says hello while a reload runs is reconciled after it, never left in step with the old code
- a child keeps the previous code version until its parent retired it (spec 12)
- a child reconciled after its parent retired old code versions may retire its own previous one at once
- a reconciliation running when a reload starts syncs the child only after it, to the committed code version
- a child behind whose hello waits for a running reload is fenced at once: its calls up are admitted (spec 9)
- a reconnecting child behind its parent, still retaining its previous code version, reconciles without waiting out the retire deadline
- forced retirement cancels a browser call relayed to a worker before the server closes the old code's resources


### `test/context.test.ts`（8 条）

Context：独立空根；分层值不改父；继承父取消、子取消隔离；`withoutAbortSignal` 为强制清理屏蔽调用方取消；telemetry 与值/取消独立派生；`traceIn` 并发分支隔离且保留返回值与错误；取消时停止等待（`awaitWithContext`）。

- Context
- provides distinct empty root contexts
- layers typed values without modifying parents
- inherits parent cancellation and isolates child cancellation
- masks caller cancellation for mandatory cleanup
- derives telemetry independently from values and cancellation
- isolates concurrent traceIn branches and preserves callback results and errors
- stops waiting when the invocation is cancelled


### `test/contract.test.ts`（8 条）

contracts：input 规范化且 identity 省略默认值；指纹稳定且基于 description；拒绝保留成员名与 remote opaque 成员；覆盖加法变更、拒绝破坏性变更；无参/无返回值方法的描述（缺席 vs 存在必须匹配，void consumer 接受 result）。

- contracts
- normalizes input and omits defaults from the identity
- fingerprints are stable and description-based
- rejects reserved member names and opaque remote members
- covers additive changes and refuses breaking ones
- methods without arguments or result: described without them; their presence must match, a void consumer accepts a result
- binds other versions only through accepts adapters
- compares opaque members by name@version and function members structurally


### `test/delta-append-text.test.ts`（10 条）

`appendText` 快路径：仅 draft；路径类型检查；与普通写混合；生成 `a` op 而非 `s`；多次 append 合并；draft 结算后不可用。

- appendText
- records an append op and the appended value
- joins several appends to one key in one change into one op, next to other changes
- falls back to the string comparison when the same key is also assigned or deleted
- appends inside a block the same change inserted or replaced
- rejects what a JavaScript caller can get wrong
- accepts only keys whose type is exactly string
- keeps an op text that does not hold a larger string it was cut from alive
- appendText cost
- costs O(chunk) per change: a 500 KB stream allocates linearly


### `test/delta-apply-immutable.test.ts`（12 条）

`applyImmutable`/`applyImmutableBatches`：结构共享（未改动的子树按引用同一）；append/truncate/splice/permute；批量按序；非法路径。

- checked immutable operation application
- copies each touched container once while preserving input and payload ownership
- protects root replacement payloads before later object and array edits
- shares one private copy-on-write scope across batch partitions
- handles tracker-produced batches across arbitrary revision boundaries
- does not expose partial application when validation or iteration fails
- allows one immutable batch to fan out without mutating shared payloads
- immutable container copies
- keep an own __proto__ member as data and the prototype of the copied object
- immutable application complexity
- appends to an array not yet copied in one copy, sharing everything else
- copies a wide object once rather than once per repeated write


### `test/delta-clone.test.ts`（5 条）

`cloneNode`/prepared 物化的隔离性：结果不与 overlay 共享可变结构；settle 后 overlay 不可用。

- tracker ownership
- takes immutable ownership of the imported revision in O(1)
- preserves null prototypes in an alias-free owned root
- does not traverse a trusted root while taking ownership
- copies assigned and inserted values immediately


### `test/delta-diff.test.ts`（21 条）

`diffRevisions`：对象增删改、数组插入/删除/移位、置换（`m`）、字符串 append 前缀/overlap 截断、语义 LCS、identity 锚点、4096 op 溢出为 `r`、成本比较（delta vs snapshot）、dense region、保留键。

- immutable revision diff
- emits sets and deletes
- emits string append and front truncation
- represents array insertion, removal, and shift with splices
- collapses a same-length queue update to two splices
- emits a permutation for a pure reorder
- normalizes reordered distinct deeply-equal objects to a no-op
- validates and encodes permutations
- emits nothing for deeply equal reconstructed values
- keeps a leaf edit inside a reconstructed array narrow
- emits payload-free splices for scattered removals
- does not field-diff unrelated shifted objects with common fields
- does not treat coincidental id or key fields as structural identity
- combines removals, append, and a shared-subtree survivor edit
- keeps forty-thousand-row sparse edits narrow
- keeps a reconstructed large-array leaf edit narrow
- splices an ambiguous equal-count moved-and-edited gap
- keeps a large rotation payload-free
- encodes five hundred unshifts without snapshotting retained rows
- bounds wide-object operation emission with a root replacement
- bounds a wide normalized array fallback with a root replacement

**参数化**（3 个 `it.each` 站点、共 9 例）：
  - `it.each([["front",[1,2,3,4],[2,3,4],[["p",["values"],0,1,[]]]], ["tail",[1,2,3,4],[1,2,3],[["p",["values"],3,1,[]]]], ["middle",[1,2,3,4],[1,3,4],[["p",["values"],1,1,[]]]], ["all",[1,2,3,4],[],[["p",["values"],0,4,[]]]], ["none",[1,2,3,4],[1,2,3,4],[]]] as const)("encodes %s removal canonically", (_name, before, after, expected) => …)` —— 展开 5 例；每个 `expectDiff({values:[...before]}, {values:[...after]}, expected)`。名称为 `front`/`tail`/`middle`/`all`/`none`，期望分别是 `["p",["values"],0,1,[]]`、`["p",["values"],3,1,[]]`、`["p",["values"],1,1,[]]`、`["p",["values"],0,4,[]]`、`[]`；即连续整段删除一律编码为单个 `p`，而**等值**输入产生空 op 列表。
  - `it.each([256 * 1024, 1024 * 1024])("does not retain a %i-byte removed-neighbor payload", (size) => …)` —— 展开 2 例。
  - `it.each([1_001, 10_000])("keeps push, pop, and middle removal narrow at %,i items", (size) => …)` —— 展开 2 例（注意标题里的 `%,i` 是源码原文，vitest 不识别该占位符，会原样保留）。


### `test/delta-op-text-retention.test.ts`（4 条）

字符串 op 的文本保留：`a`/`t` 产生的字符串不与源共享（`ownedSlice` 语义）。

- append op text
- tracker append ops do not retain the string they were cut from
- diffRevisions append ops do not retain the string they were cut from
- a replica that applies a long stream of appends retains the text once, not once per commit


### `test/delta-pending.test.ts`（29 条）

`PendingOps`：push/take 状态机、10000 op 触发折叠、`s` 覆盖、`a`+`a` 合并、`s` 吸收 append、`d`/`t`/`p`/`m` 直通、`r` 变 replace、take 后重置、mergeLeaf 的 leaves 下标维护。

- PendingOps folding
- is invisible to a replica: tracker edits anywhere, appendText, splices and reorders
- is invisible to a replica: leaf edits of list elements between splices and reorders of the list
- is invisible to a replica: a root array
- is invisible to a replica: diffRevisions between arbitrary values
- PendingOps adversarial sequences
- a leaf set in an array element, a splice before it, the same path set again
- `t` then `a`, and appends around a trim
- an object `s` at a parent, then leaf sets below it
- an ancestor primitive `s` between leaf ops below it
- deletes, moves and splices shift what a path names
- keys of one parent never collide: `a.b` and `a`/`b`, `1` on an object and on an array
- an array set at the end appends, and a later set of that index folds into it
- `r` drops everything before it
- joins appends, folds appends into a string set, and keeps split surrogates
- interleaved leaves, a structural op, then the same leaves again
- PendingOps cap
- counts ops after folding
- accumulates nothing once replaced, until taken or a batch starting with `r`
- hands one batch over as received, whatever its size
- PendingOps ownership
- hands over a received batch by identity and never mutates batches or op tuples
- PendingOps size
- counts pending ops after folding, the replacement once overflowed, and nothing once taken
- PendingOps cost
- computes one path key per leaf op once folding, none for one batch or structural ops
- keeps one op per leaf path however many commits write it
- PendingOps references
- releases a folded-over op, and the index and everything pending once taken

**参数化**（1 个站点、1 例）：
  - ``it(`keeps ${MAX_PENDING_OPS} pending ops and replaces past them`, () => …)`` —— 标题插值 `MAX_PENDING_OPS = 10_000`；运行时标题即 `keeps 10000 pending ops and replaces past them`。断言 push 恰好 10_000 个 op 时仍为 batch，第 10_001 个触发 replace。


### `test/delta-tracker/retention.test.ts`（1 条）

（仅 describe）delta tracker retention across GC jobs。

- delta tracker retention across GC jobs

**参数化**（1 个 `it.each` 站点、15 例）：
  - `it.each(["aborted-payload","unadopted-prepared","draft-proxies","settled-lifecycle","obsolete-revisions","retained-settled-prepared","retained-settled-change","retained-settled-proxy","retained-large-settled-proxy","retained-large-placement-proxies","stale-unprepared-change","same-job-fast-cleanup","same-job-folded-ops-cleanup","lifecycle-churn","adopted-changes-flat"])("validates %s retention semantics", (scenario) => …)` —— 15 个场景逐个 `spawnSync` 子进程（`retention.worker.ts`，需 `--expose-gc`），断言对应对象在图释放后不再被 overlay 持有。Rust 侧只能以 `Drop` + 分配计数近似（见 §8.6 第 16 条）。


### `test/delta-tracker/tracker.test.ts`（75 条）

Tracker/overlay 的行为（本节 75 条 = 7 个 describe 容器标题 + 68 条用例标题；另有 1 个 `it.each` 展开 2 例，运行时 70 个用例）：读穿透、写/删/读回、数组 mutator 全语义、`then` 为 undefined、settled 后不可用、abort、stale 检测、clone、op 生成顺序、dense region、保留键、置换、sealed/preventExtensions 报错等。

- delta tracker transactional overlay lifecycle
- materializes an immutable next revision and adopts it by pointer swap
- keeps a transaction live across await and seals it only at prepare
- allows Promise resolution to inspect a settled draft without reporting a false failure
- aborts idempotently and revokes held descendants
- allows competing contexts and invalidates loser views
- invalidates competing open overlays without changing their base revision
- adopts object edits as ordinary fast-layout immutable revisions
- supports adopt-then-publish ordering and retained publication reads
- keeps replacement base/value structurally compatible and readable after adoption
- keeps aborted and stale materialized candidates readable
- lets a settled Change abort its prepared result without retaining its context
- rejects foreign and aborted prepared values
- reads deleted own properties as absent
- detaches old handles for deeply equal object and array assignments before normalizing
- normalizes a deeply equal replacement without changing committed identity
- takes O(1) immutable ownership for replacement and its operation payload
- delta tracker policy view
- supports native reads, descriptors, keys, iteration, map, and JSON
- keeps document keys distinct from object proxy target fields
- keeps native coercion side effects and string operation forms
- emits deletion and supports root arrays
- preserves native object key order after delete and re-add
- orders new integer-like object keys before strings for recipe-visible reads
- normalizes deeply equal container assignments before direct candidate materialization
- keeps native integer and string ordering across deletion and re-addition
- orders integer-like keys on null-prototype objects
- forwards borrowed mutators to ordinary generic array receivers
- matches native structural coercion ordering for splice, fill, and copyWithin
- does not report inherited methods as own array properties
- keeps repeated identical writes instead of reverting their pending override
- rejects array index gaps without mutation while allowing replacement and append
- supports optional deletion, explicit null length growth, shrinking, and default sort
- enumerates wide objects without duplicate keys
- does not dirty read-only traversals
- uses one proxy identity per accessed container
- delta tracker by-value placements
- clones property, index, push, unshift, splice, fill, and copyWithin placements
- rejects non-strict JSON placements before changing the draft
- expands repeated source aliases into independent placements
- deep-clones draft-sourced placements within and across revisions
- distinguishes raw committed references from draft references
- folds edits to introduced object and array subtrees into placement payloads
- folds pathological operation counts without payload-cost estimation
- shares immutable operation placements with the materialized candidate
- delta tracker piece arrays
- emits one insert for an append to a long array
- emits removals from the end backwards, then inserts, when kept elements keep their order
- emits a permutation when kept elements change order
- keeps adoption independent from mutable detached permutation metadata
- matches native splice with zero or one argument
- supports all structural mutators and native return values
- tracks held handles after reindex and suppresses detached writes
- replays introduced, moved, then edited array descendants
- combines movement and edits with permutation plus final paths
- keeps comparator writes and structurally reentrant appends
- normalizes duplicate entries created by structurally reentrant sort callbacks
- folds dense child and direct-index edits into one full-array operation
- folds only a deeply nested dense array region
- does not cache emission paths for nested reserved-key folds covered by an outer region
- suppresses nested structural and leaf operations covered by an outer dense region
- emits multiple disjoint dense regions and preserves operations outside their boundaries
- normalizes a 20,000-operation queue transaction to two operations
- handles adversarial fragmentation and resolves held handles without piece scans
- normalizes restored overrides and cancelled structural edits to no operations
- handles null sparse overrides without confusing absence
- supports self-overlapping fill and copyWithin by value
- delta tracker randomized transactions
- matches policy, detached replay, and adopted state after multi-operation transactions
- delta tracker object emission scaling
- orders many reverse-depth object edits through the bounded fallback
- delta tracker security and storage-style cloning
- defines own properties without invoking inherited setters
- handles reserved own keys without prototype pollution
- supports a MemoryStorage-style recursive clone of Prepared.value

**参数化**（1 个 `it.each` 站点、2 例）：
  - `it.each(["unshift", "splice"] as const)("inserts 100,000 items with %s", (method) => …)` —— 展开 `unshift` / `splice` 2 例；断言 100 001 长度、位置 99 999/100 000 的元素值、以及 `replay(base, prepared.ops)` 等于 `prepared.value`（即不靠 "参数过多" 溢出）。


### `test/delta.test.ts`（50 条）

Op 语言本身：`assertValidOp`/`assertValidWireOp` 的全部 arity/shape 错误、`apply` 语义、encoder/decoder（路径 interning `#`、短形式、`r` 重置）、`overlap`、`ownedSlice`、`PathError`、保留段。

- immutable tracker lifecycle
- keeps a draft alive across await and adopts only a prepared change
- aborts and revokes without changing the committed value
- grows arrays with explicit nulls and revokes drafts after preparation
- normalizes a deep no-op to exact previous identity
- takes immutable ownership of replacement input and applies no-op normalization
- shares immutable operation placements with the prepared candidate
- rejects foreign, stale, and repeated preparations while allowing competing changes
- invalidates a prepared result when its change is aborted
- makes competing same-base preparations stale after adopting a no-op
- emits an owned root replacement without traversing large replacement input
- canonical strings
- emits append and rolling-window operations
- finds bounded overlaps
- operation application and validation
- applies mutable and immutable operations
- supports root replacement, root splice, and permutations
- rejects unsafe and malformed paths
- validates immutable operations before traversing the target
- validates decoded and wire vocabularies separately
- codec
- interns paths, omits adjacent paths, and round-trips
- resets path dictionaries on a base
- rejects unresolved short forms and unsafe interned paths
- omits an adjacent repeated path
- interns on second use rather than first
- does not collide paths containing null characters
- clears decoder ids on a base batch
- makes batches after a base self-contained
- round-trips deterministic mixed operation streams
- immutable application ownership
- does not mutate a replacement payload targeted by a later operation
- adopts a mutable replacement payload rather than copying it
- path and prototype safety
- rejects constructor walks and forbidden interned paths
- does not run inherited setters
- allows reserved names inside values without prototype pollution
- imports own properties without invoking inherited setters
- array index safety
- writes an existing index and appends exactly one past the end
- rejects gaps, huge indices, and string-spelled indices
- allows explicit growth values and rejects deletion past the end
- applies large splice payloads without spreading them at once
- operation structure safety
- rejects unknown verbs, malformed tuples, paths, and splice payloads
- rejects invalid append and truncation operations
- clamps splice removal past the end
- operation assertions
- accepts decoded operations and rejects wire-only forms
- does not recursively inspect operation payloads


### `test/flow-units.test.ts`（8 条）

unit 帧（shape + snapshot）在窗口满时的排队与顺序；lazy reset；`resync` 触发重发。

- flow control with units
- with large streams waiting behind a full window, a new get resolves at once and calls are answered
- a snapshot larger than the window arrives whole; later changes wait for acknowledgements
- a link that reports no sizes (IPC) never holds state back
- waiting ops stay under the per-link cap however many streams stall; each then gets one reset
- relay topology
- each origin hold is one downstream hold; nothing is left after release, close or revoke
- a stalled tab overflows and gets a reset while another tab keeps getting every change


### `test/flow.test.ts`（9 条）

流控：ack 计数与越界违规、窗口（256KiB）、pendingCap、state slot 合并/丢弃、`state:false`、ack 不计数、`too_many_calls`。

- link flow control
- holds state back while the window is full, answers calls meanwhile, and sends one coalesced frame
- replaces ops that outgrow their slot with the current value
- drops what waits for a reference that is released
- coalesces relayed state for the origin and asks downstream for a reset when it overflows
- neither windows nor acknowledges a peer whose hello does not announce acks
- closes the link on an acknowledgement of frames never sent
- link call limit
- answers calls beyond maxCalls at once with too_many_calls and keeps the link


### `test/generations.test.ts`（5 条）

代次：principal 失效（revoke）后 relay 关闭并通知；`invalidate` 帧按 pattern 匹配；generation 递增使旧链路失效。

- generation GC
- a reference held across reloads keeps only the current generation alive
- a lifetime "host" object with state, a kept resource, a listener, a task and registrations keeps only the current generation alive
- abort listeners that calls and tasks add to their ctx.signal do not outlive them
- disposed objects are not kept once their disposal finished


### `test/host-objects-reload.test.ts`（6 条）

host lifetime 对象在 reload 中的 token/地址稳定、重新安装、dispose。

- the host's reference to a host object follows contract bumps at commit without reopening its object or resources; failures retain the old reference
- external old-version consumers still refuse a host object's upgrade without accepts, and keep their adapter when it is provided
- object-owned old-version references still block a host object's upgrade unless their consumer is replaced in the same batch
- a forgotten contract bump remains refused even when the only consumer is the host's own reference
- a cluster contract upgrade retokens the server's and worker's references to host objects only after unanimous preparation
- a reload that changes a type's lifetime creates its host object, or lets it go


### `test/host.test.ts`（31 条）

host：createHost 选项与默认值、serve/install 守卫、root/joinRoot、registry、principal 匹配、route 最长前缀、reload 错误、close 顺序、未知类型、环检测。

- host: activation and lifetime
- activates, goes live, binds members late, disposes after release
- coalesces concurrent gets and keeps the object while held
- lingers after the last release and revives on a new get
- disposes an abandoned activation once it settles
- propagates activation failures to dependents and retries on the next get
- unwinds declarations when a factory throws and rejects async factories
- rejects duplicate declaration names
- detects hold cycles
- closes resources in reverse order, runs all steps and aggregates errors
- releases ctx.get holds when the call settles
- host: declarations and methods
- refuses a second declaration of one name per kind; kinds have their own names
- methods without arguments are called with ctx alone; void methods resolve to undefined
- a listener on state gets the current value at start and again after a reload replaced its derivation
- host: lifetime
- serve refuses a lifetime "host" the type cannot have, at compile time and at runtime
- every get reaches the one object, created at serve for the host principal; another input is invalid
- host: principals, variants and disposal
- keeps objects per principal key and enforces variants
- validates remote args and results at the provider boundary
- forced disposal makes references gone and aborts in-flight calls
- state members are late-bound readables: values, then snapshot/reset/ops events
- host: registries
- only objects of lifetime "host" register; tables follow registration order
- rejects conflicting registrations
- host: composition
- reference state members are sources for listeners and derivations
- ctx.get acts as the root principal at ingress and as the host inside objects
- calling a reference inside a factory throws not_ready
- a ChordError from a factory keeps its code through dependents


### `test/hydration.test.ts`（5 条）

hydration：ref `ready` 与 state 成员 hydration 的先后、HYDRATION 符号、unhydrated()、whenHydrated。

- local hydration
- an object that forwards and derives from a remote member goes live with its value
- a member read inside a factory throws not_ready; it is a plain value once live
- a host object of a child holding the parent's object goes live once the parent admits the child
- a reference cycle across hosts fails at readyTimeout with a cycle hint


### `test/json.test.ts`（7 条）

`copyJson`/`isJsonValue`：全部 TypeError 文案（非有限数、非 JSON 类型、环、稀疏数组、非普通对象、符号键、非可枚举/访问器）、omitUndefinedProperties、null 原型、无别名。

- isJsonValue
- checks strict JSON without normalizing it
- copyJson
- copies strict JSON without retaining aliases
- optionally omits undefined object properties without normalizing arrays
- preserves null prototypes and own __proto__ data properties
- rejects cycles and non-strict container properties


### `test/link.test.ts`（26 条）

Link：MemoryLink 双向异步 FIFO、sever、关闭回调、bye、协议错误；IPC 包装；WebSocket 文本/二进制；`PACK_THRESHOLD` 打包边界；`z: [field, base64]`；`inflateMessage`；`parseFrame` 错误。

- links: calls and state across hosts
- a child calls the server; the server stamps the child's principal
- state members: snapshot, then delta-encoded ops; values decode to the provider's
- the server reaches a connected child by name
- rejects local types and unknown hosts
- links: relay, stamping, lifetime
- relays browser → server → worker; the worker sees the browser's stamped principal
- ignores payload principals from clients and from child hosts
- a route that returns an already closed link does not poison the host name
- authorizes before resolving and again after
- a browser disconnect releases exactly its holds; the worker link stays up
- a dead link (no pongs) is closed and everything held through it is released
- checks contract tokens across links: contract_mismatch for hosts, upgrade_required for browsers
- invalidatePrincipal re-stamps the link and propagates to workers
- after a reconnect, state subscribers get a reset and calls work again
- links: methods without arguments or result
- travel without args and without a value, and resolve to the result or undefined
- links: protocol version
- a peer without a protocol version (an old client) is told and dropped; nothing it sends is served
- a client learns that its server speaks another version, and stops
- peers of the same version talk; an ordinary close is no mismatch
- links: packed payloads
- large args, results and snapshots cross a relay deflated and arrive intact
- only payloads above the threshold are packed; the receiver unpacks them
- links: IPC
- a write that fails after the peer process exited (EPIPE) ends the link instead of raising an unhandled error


### `test/links-review.test.ts`（18 条）

链路细节复查：close 幂等、早到帧排队、onClose 已关闭立即回调、decode 链顺序、packing 阈值与 level、错误 close 原因。

- links review: identity and stamping
- #1 the acceptor decides what a peer is: a client announcing itself as a host is cut off
- #1 root_end is honored only from the link the root came over
- #2 a revocation while the route is still resolving closes the relay; no worker object is made
- #10 malformed frames and duplicate ids close the link
- links review: relay ownership
- #7 a browser disconnect cancels its call downstream
- #9 state stopped while the route resolves is forwarded as a hold without state
- #9 a release while the route resolves settles waiting calls and forwards nothing
- #9 an origin disconnect removes its relayed holds and subscriptions downstream
- two browsers share one worker connection; one leaving releases only its own holds
- #11 snapshots through an accepts adapter are validated against the consumer's schema
- links review: cross-host reload
- #3 an abort while the child is still loading modules frees its reload slot; a later reload works
- #3 the parent link dying while the child prepares unwinds the child
- #4 tasks the commit starts cannot reach a child before the child's commit
- #5 a child that missed a commit is reconciled on reconnect, never relabeled
- #6 a server root that bounces through a worker and back ends normally (no leaked lease)


### `test/loader.test.ts`（3 条）

nodeLoader：每次 reload 得到新模块实例、default export 非 facet 报错。

- node loader
- loads facet modules and reloads them with fresh copies of their relative imports
- needs a configured loader for modules


### `test/manager.test.ts`（11 条）

HostManager：串行 start/stop、ready 超时、提前退出、fatal、keepAlive 重启、liveness、shutdown grace 后 killTree、进程锁。

- processLock
- admits one holder when the temporary directory is too deep for a socket path
- host manager (real child processes)
- a child without a lock does not inherit the parent's lock
- starts a child host, links it, stops it gracefully
- a child-owned kernel lock admits one live process; replace waits for the exit before the next start
- a graceful exit still takes the child's descendants with it
- idleFor counts from the busy → idle transition
- kills the process tree when a child ignores shutdown past the grace period
- reports a fatal child as a failed start
- stops an idle child when keepAlive says so


### `test/output-skip.test.ts`（5 条）

`OutputSkip`：tail-only 限制、跳过计数、清空已存块、endsWithNewline。

- OutputBuffer skipped output
- keeps the same tail whenever progress snapshots happen
- matches the full stream for every legal skip pattern
- counts skipped bytes, lines and the final newline exactly
- refuses skips for head retention


### `test/output.test.ts`（24 条）

`boundOutput`/`OutputBuffer`/`sanitizeOutput`/`utf8ByteLength`/`lineCount`/`Progress`：head/tail 窗口、字节/行限制、UTF-8 边界、BOM 只剥首块、行数、丢弃统计、进度节流 100KiB/s。

- tool output bounds
- removes control characters but keeps tabs, newlines, and other text
- keeps output within the limits unchanged
- keeps nothing with a zero limit
- keeps exact slices of whole lines, trailing newline included
- cuts at the byte limit on whole lines when possible
- cuts a single line longer than the byte limit on a character boundary
- keeps a U+FEFF at the start of a kept slice
- cuts tails of surrogate edge cases exactly like Buffer
- cuts tails exactly like Buffer across deterministic fuzz cases
- OutputBuffer
- keeps exact totals across chunks and decodes UTF-8 split across byte chunks
- sanitizes the retained text but counts the raw stream
- drops a byte-order mark only at the start of the output
- flushes an incomplete character before a string chunk and at the end
- matches bounding the whole stream when several chunks arrive between snapshots
- stops storing head output once the window is full
- stores only the tail window after each snapshot
- Progress
- commits the first change at once, then waits at least 100 ms and the written size at 100 KiB/s
- waits the configured minimum interval between small commits
- paces on the clock it is given
- rejects the waiters of a failed commit and reports its error
- stops: waits for the commit in flight and hands back waiters no commit covered yet


### `test/placement.test.ts`（7 条）

placement：hostNamed/hostFamily、resolveHost、placedOn、placementText、错误消息。

- placement
- a get goes where the contract places the input: here, a child, or the parent, never named by the caller
- serve refuses a type the host is not the place of, also at a reload
- a host refuses a hold its own contract places elsewhere, and a relay one whose target disagrees
- the host comes from the normalized input: unknown fields and defaults do not change it
- placement is part of the contract: a moved type is a breaking change, never bridged by an adapter
- types: the family's field is a required string of the input, and remote contracts need a host


### `test/principals.test.ts`（5 条）

principal：self/principalKey/规范化、每 principal 独立对象、invalidatePrincipal 的 pattern 匹配与 revoke。

- principals
- factories, methods and contexts see the app's principal type
- the host's own principal is recognized by key: an equal object reaches the same objects
- a child's parent acts as the principal the child gave `connect`; relayed callers keep theirs
- refuses a host, a parent link or a route without its principal


### `test/provider-cost.test.ts`（2 条）

provider 侧开销（一个测试）：共享 binding 不重复传输。

- provider cost
- one change to K clients: K encodes (one per feed), one frame per link, the same ops on every link


### `test/readiness.test.ts`（4 条）

ready 语义：readyTimeout、跨 host 环导致的 ready_timeout 消息、waiting on 列表。

- host: eager readiness
- load installs every facet before awaiting eager consumers
- missing eager dependencies reject load instead of only reaching onError
- ready includes direct host serves, but not later serves

**参数化**（3 个站点、10 例）：
  - ``for (const operation of ["install","load"]) it(`${operation} awaits resource activation and registration, leaving lazy objects lazy`, …)`` —— 2 例。
  - 在外层 `operation` 循环内的 ``for (const failure of ["factory","dependency","resource"]) it(`${operation} rejects eager ${failure} failure with the activation error`, …)`` —— 2 × 3 = 6 例。
  - ``for (const operation of ["install","load"]) it(`${operation} can defer eager readiness until the parent admits the worker`, { timeout: 2000 }, …)`` —— 2 例（注意带 `{ timeout: 2000 }` 选项）。


### `test/reload.test.ts`（19 条）

reload：prepare/gate/commit、状态版本迁移、schema 无 bump 拒绝、版本降低拒绝、exclusive resource 替换、rollback、retire deadline、gate deadline（blocking roots）、upgrade_required 给客户端、remove facets、registry 冲突、reload_running。

- reload
- compatible reload: no gate, pinned roots keep old code, state preserved, previous retires
- prepare runs in recording mode: writes throw and a failing factory leaves everything unchanged
- refuses a changed cell schema without a version bump
- state bump: gate, migrate(old, fromVersion) into the new schema, reset to watchers, old roots upgrade_conflict
- versions are per cell: a raised one migrates only that cell, a lowered one is refused
- gate: late old roots get upgrade_conflict, grandfathered roots re-enter, then commit
- gate deadline: reload aborts, reports blocking roots, nothing changes
- exclusive resources are replaced at the gate; non-exclusive ones at prepare and closed at retirement
- two-version rule: the next reload retires the previous code version; its roots get superseded
- forced retirement aborts the leftover roots before it closes the resources only their code used
- a kept resource closes with the newest declaration's close
- a task that ignores its abort delays its replacement at most taskStopDeadline (reported)
- a facet's signal aborts when its run is replaced or removed, when its reload aborts, and when the host closes
- checks contracts at reload: forgotten bumps, live consumers, accepts adapters
- rebinds listeners to the new code and restarts tasks
- keeps registry tables per code version
- a reload can add a type together with a consumer that holds it
- the gate rejects new objects of a changed type; after commit they get the new code


### `test/replicated-state-delivery.test.ts`（10 条）

`ReplicatedStateDelivery`：hydrate/update 的 sequence、订阅者串行投递、100 pending 溢出保留首个 hydration、Promise listener 等待、错误上报。

- awaits hydration and each update independently of other subscribers
- excludes a running update from overflow and retains exact value/context/delivery
- serializes reentrant hydration and update callbacks
- treats two subscriptions of the same callback independently
- unsubscribe drops queued callbacks without joining or aborting the running callback
- attached listener errors
- isolates a synchronous hydration failure without removing the subscription
- observes hydration rejection, synchronous throw, and update rejection while continuing delivery
- still observes a callback rejection after unsubscribe
- mutable state reports rejected callbacks through its default error reporter

**参数化**（1 个 `describe.each` + 1 个 `test.each`，合计把块内 10 个非 describe 声明点变成 24 个运行时用例）：
  - `describe.each<Kind>(["mutable","attached"])("%s public delivery", (kind) => …)` —— 整个块跑两遍。两种 fixture 都是 `replicatedState(...)` 的返回值：`mutable` 走 `MutableReplicatedStateImpl`（`publish` = `state.replace(context, value)`）；`attached` 传入带 `attach()` 的 source（snapshot `{value:{value:0},cursor:0}`，`activate` 只登记 listener，`dispose(){}`），`publish` 手工构造 `{value, cursor: value.value, ops: [["s",["value"],value.value]], context}` 喂给 `receive`。块内 9 个 `test(…)` 与 1 个 `test.each(…)` 全部 ×2。
  - ``test.each([100, 101, 102, 201, 202])("bounds %i pending deliveries", async (count) => …)`` —— 5 例 ×2 = 10 例；断言 `received === [0, ...从 (count-1) 向下取整到 100 的倍数 + 1 开始]`。
  - 块外还有 3 个 `test(…)`（`attached listener errors` 下）与 1 个顶层 `test("mutable state reports rejected callbacks through its default error reporter")`，不参与 ×2。


### `test/replicated-state.test.ts`（18 条）

replicatedState：mutable change/replace 的重入错误、同步回调、attached source 的 cursor 连续性、gap 错误、dispose 聚合错误、订阅立即 snapshot。

- transactional replicated state
- publishes one immutable structurally shared revision
- rolls back callback failures and revokes escaped drafts
- rejects PromiseLike callbacks and aborts their draft
- rejects nested changes and replacements without losing the outer rollback
- queues listener-triggered changes in sequence order
- isolates listener failures after committing the revision
- copies assigned values by value
- takes immutable ownership of alias-free replacements
- replaces atomically and ignores deeply equal replacements
- authoritative replicated state sources
- captures before activation and drains queued commits in order
- hydrates at sequence zero when attaching after existing commits
- publishes exact source values without applying or re-diffing
- buffers reentrant frames and skips updates covered by a late hydration
- reports listener failures without throwing them into the source
- reports cursor gaps, disposes the broken attachment, and ignores later frames
- keeps attachments independent and disposes each idempotently


### `test/requests.test.ts`（12 条）

请求/调用：call/result/cancel、root 传播、args Value.Clean、错误映射、maxCalls、abort。

- application requests
- #1 a held object's state member can be returned as this object's state member
- #2 facets reach their own host through FacetApi.root and FacetApi.registry
- #3 a listener keeps its subscription across reloads when resource and select are unchanged
- #3 a changed select resubscribes
- #4 replace diffs a new revision that shares structure; unrelated values replace
- #5 roots of a child that connected behind the server are admitted in the server's code version
- #6 self.expose: an opaque member backed by a resource, readable once live
- #7 a cluster reload reports what each child prepared
- #8 a held remote reference keeps its state without listeners; a later subscriber sees the current value
- #9 references report status changes: live, loading while a link reconnects, gone, released
- #10 host close disposes holders before what they hold, and before closing links


### `test/review.test.ts`（22 条）

综合复查 18 项（含协议与生命周期边角）。

- review: reload commit
- #1 a failing listener subscription fails prepare; the code version and all objects stay unchanged
- #2 migration is validated against the new remote schemas, not the obsolete ones (dev mode)
- #1 a commit that still fails midway fails the host terminally (no half-published code version serving work)
- reentrant observers calling another object during a commit see the whole new code version
- review: leases, invocations, activation
- #4 a running listener holds a lease: the gate waits for it before migrating
- #5 disposal waits for running methods before closing resources (released and forced)
- #6 a registry conflict found while finalizing activation unwinds the loser completely
- #6 releasing the last hold while listeners start up does not resurrect the object
- review: prepare isolation and contracts
- #7 a prepare rerun cannot touch retained resources, even open ones
- #7 objects of types a reload adds start listeners and tasks only at commit
- #8 a breaking provider change and its updated consumer reload in one batch
- #9 an exclusive resource whose close failed is restored on rollback, or marked unavailable
- #10 retirement closes dropped resources before releasing dropped holds
- #11 fits has no false positives for unmodelled keywords; counterexamples checked with Value.Check
- #11 arguments are stripped to what the consumer knows, then to what the provider knows
- review: ctx races
- an aborted task ctx cannot start fresh calls
- an expired ctx and a foreign ctx are refused
- a remote reference refuses a foreign ctx and a ctx not made by Chord, without calling


### `test/sqlite/cloudflare.test.ts`（9 条）

Durable Object 适配：bindValue 的 bigint 精度、BLOB→Uint8Array、事务句柄、SerialQueue、close 等待队列。

- Durable Object SQLite driver
- binds and returns BLOB values as Uint8Array
- reads only the cursor's first row for get
- rejects a bigint binding outside the safe integer range
- closes after an active transaction settles
- runs an uncontended statement before returning its promise
- rolls back a rejected transaction and rejects its handle afterwards
- runs operations from another adapter after a transaction, outside its rollback
- includes direct underlying calls in an active transaction


### `test/sqlite/migrations.test.ts`（5 条）

迁移：连续版本校验、STRICT 单例表、metadata missing、版本过新、逐条 UPDATE、事务原子性。

- SQLite migrations
- tracks independent histories and skips migrations already applied
- rejects invalid migration versions before touching the database
- validates prefixes and migration table identifiers before executing SQL
- rolls initial bootstrap and every pending migration back together


### `test/sqlite/node.test.ts`（9 条）

Node 驱动：WAL pragma、busy timeout、语句缓存、事务 BEGIN IMMEDIATE/COMMIT/ROLLBACK、AggregateError、close 的 checkpoint。

- Node SQLite driver
- configures connection defaults and accepts explicit settings
- commits work done through the transaction handle and closes idempotently
- serializes concurrent transactions
- runs operations in call order whether they start immediately or wait
- queues ordinary operations behind an active transaction
- queues database calls made synchronously by a transaction that started immediately
- rejects a transaction handle used after its transaction settles
- does not preserve the callback error when rollback itself fails


### `test/state-diff.test.ts`（20 条）

cell 与 diff 的协作：change/replace 生成的 op、reset、schema 交换、derivation 依赖顺序。

- immutable revision diff
- emits sets and deletes
- emits string append and front truncation
- represents array insertion, removal, and shift with splices
- collapses a same-length queue update to two splices
- emits a permutation for a pure reorder
- normalizes reordered distinct deeply-equal objects to a no-op
- validates and encodes permutations
- emits nothing for deeply equal reconstructed values
- keeps a leaf edit inside a reconstructed array narrow
- emits payload-free splices for scattered removals
- does not field-diff unrelated shifted objects with common fields
- does not treat coincidental id or key fields as structural identity
- combines removals, append, and a shared-subtree survivor edit
- keeps forty-thousand-row sparse edits narrow
- keeps a reconstructed large-array leaf edit narrow
- splices an ambiguous equal-count moved-and-edited gap
- encodes five hundred unshifts without snapshotting retained rows
- bounds wide-object operation emission with a root replacement
- falls back to a base operation when leaf operations are larger

**参数化**（3 个站点、9 例；与 `test/delta-diff.test.ts` 同名同表）：
  - `("encodes %s removal canonically")` ×5、`("does not retain a %i-byte removed-neighbor payload")` ×2、`("keeps push, pop, and middle removal narrow at %,i items")` ×2。


### `test/state-draft.test.ts`（8 条）

draft 写语义：对象/数组写入、删除、length、嵌套。

- transactional overlay drafts
- copies only changed branches
- supports frozen bases, deletion, and native array mutators
- copies inserted draft values without aliasing their original reference
- keeps comparator edits when sorting object drafts
- drops writes through detached child references
- rejects non-strict JSON placements without mutating the draft or base
- rejects array holes without mutating the committed base

**参数化**（1 个站点、2 例）：
  - `it.each(["unshift", "splice"] as const)("inserts 100,000 items with %s without argument overflow", (method) => …)` —— 2 例；`unshift` 用 `Reflect.apply(change.state.values.unshift, …, items)`，`splice` 用 `[1, 0, ...items]`。


### `test/state-fuzz.test.ts`（1 条）

随机 fuzz（1 项）：随机操作序列下 apply(diff(a,b)) 等于 b。

- converges across randomized prepared revisions


### `test/state-init.test.ts`（9 条）

init cell：initialize、重复初始化、未初始化读取 not_ready、schema 校验文案。

- state init
- runs after the resources are open and before the object goes live: nothing sees another value
- an init cell cannot be read inside the factory
- a throwing or invalid init fails the activation and closes its resources; the next get tries again
- init runs in a root: it can call other objects
- reload
- keeps an existing cell without running init; a cell the reload adds runs init while it prepares
- a failing init of an added cell refuses the reload; the old code keeps running
- in a gated reload, an added cell runs init at the gate


### `test/state-value.test.ts`（4 条）

state 值读取：未初始化、terminated、变体。

- immutable revision ownership
- takes ownership of an alias-free mutable JSON root without freezing
- commits transaction copies in place while sharing unchanged branches
- makes repeated placements independent


### `test/state.test.ts`（8 条）

cell/derived：subscribe/events 立即回调、dependents 排序、holdPublications、错误上报。

- state cells
- publishes snapshots then exact ops; derivations follow
- supports primitive cells and value-returning changes
- rejects a returned value that does not fit the schema, even outside dev mode
- validates initial values always and writes only in dev mode
- rejects writes from inside a factory and placeholder use before activation
- listeners run each event in a fresh root; tasks get an abortable ctx
- re-entrant writes publish in adoption order; derivations never go stale (Sol #3)


### `test/telemetry-sqlite-store.test.ts`（4 条）

store 查询：getTrace 排序、findTraces 过滤/游标/limit+1、findMetrics、属性类型过滤。

- SQLite telemetry reads
- uses two span queries for a page, not one query per trace
- reads committed rows without implicitly flushing, including after close
- keeps surviving descendants discoverable after a stored parent is removed


### `test/telemetry-sqlite.test.ts`（14 条）

telemetry recorder：span 10 列、事件相对时间、status 映射、容量丢弃、metric 聚合与 10s interval、flush/close、失败永久停止、retention 清理、inject/extract。

- SQLite telemetry span and propagation conformance
- SQLite telemetry buffering and aggregation
- does not execute SQL while recording and stores only settled spans
- aggregates canonical series by interval and writes partial intervals only on close
- separates kind, name, unit, and caller attributes without retaining descriptions or boundaries
- strictly bounds queued spans including the in-flight batch and bounds pending metric series
- counts in-flight metric intervals against capacity until commit
- flushes once per second and retains only rows inside the retention window
- serializes concurrent flush and close without losing measurements recorded during writes
- close does not wait for active business callbacks, ignores late completions, and leaves the database open
- stops permanently after a write failure and reports the identical error without retry
- remembers an undefined background write failure and stops its timer
- rolls back spans and metric aggregates together on SQL failure
- reuses migrations, preserves identities across reopen, and merges aggregates from multiple adapters

**参数化**（1 个站点、12 例）：
  - ``for (const test of createTelemetryAdapterConformance(fixture).filter((test) => test.group !== "metrics")) it(`${test.group}: ${test.name}`, test.run)`` —— 来自 `packages/telemetry/src/testing/conformance.ts` 的 14 个 `createCase`，去掉 `metrics` 组 2 个后为 **12** 例，每组名与用例名（逐字，运行时标题为 `组名: 用例名`）：
    - `capabilities: carries adapter capabilities through contexts and spans`
    - `links: records initial and late links without changing parents`
    - `links: ignores links added after source settlement`
    - `propagation: roundtrips parent identity through JSON without retaining the carrier`
    - `propagation: absent and unknown carriers produce recorded roots`
    - `propagation: propagates across fresh adapters and multiple hops`
    - `callback lifecycle: admits once synchronously and preserves the result`
    - `callback lifecycle: preserves synchronous and asynchronous rejection values`
    - `status: uses last explicit status without automatic overwrite`
    - `recording: merges attributes and records ordered events`
    - `recording: makes calls after settlement inert`
    - `parentage: records nested and concurrent child relationships`
  这 12 例的断言来自 **telemetry 单元**（`@earendil-works/kiana-telemetry` 的 testing 子路径），移植时若 `kiana-telemetry` 是独立单元，这里只需保证 `createSqliteTelemetry` 满足该 conformance 契约。


### `test/tutorial.test.ts`（11 条）

文档 tutorial 中的示例行为（11 项）。

- 01: objects live inside hosts
- 02: first contract, factory, reference and root
- 03: cells publish values and derivations
- 04: holds, asynchronous resources and release
- 05: principals select views of one shared store
- 06: remote methods and live state over a memory link
- 07: listeners react and tasks start fresh calls
- 08: plugins contribute registry entries and expose callbacks
- 09: compatible reload, migration and an old-contract adapter
- 10: stub a provider with a fake facet and check error codes
- 11: short roots release temporary holds


### `test/types.test.ts`（2 条）

类型级（1 项）：导出面类型可用。

- reference types
- state members are T; other variants' members need narrowing; gets are awaited


### `test/variants.test.ts`（4 条）

variant：shape.variant、not_in_variant、state 成员按 variant、remote/local 差异。

- variants
- a new variant's members arrive with its shape; a member outside the variant throws not_in_variant
- re-hold after a link loss
- keeps the last values readable, sends one fresh unit, and a reference joining meanwhile waits for it


### `test/watch.test.ts`（33 条）

Watch：acquisition 值保持到 start、不内联调用、串行 await、合并 frame、投递 newest context 且去 abortSignal、溢出不折叠 in-flight、retirement（最后投递 `RETIREMENT_OPERATIONS = [["r", null]]`）、五种结束方式、引用不保留、NativeWatch 101 折叠。

- Watch delivery
- keeps the acquisition value until start, then delivers the frames since in one call
- passes a frame's ops on by identity to a listener that keeps up
- never invokes the listener inline
- awaits each callback and folds frames advanced while one is in flight into the next call
- delivers the newest frame's context, without its abort signal
- Watch with a slow listener
- gets one call per batch whose ops turn the previous delivered value into the newest
- Watch overflow
- never folds the in-flight call into an overflow
- starts folding again after an overflow was delivered
- Watch retirement
- delivers `[["r", null]]` as the last call and then ends retired
- drops pending frames, also an overflow, for the retirement
- ends retired only on its source's retirement, never on a null value
- is only available to watches whose values include null
- discards a retirement still pending when the source ends
- Watch ends
- stops idempotently and rejects a repeated or late start
- ends `ended` or `failed` from its source, dropping pending frames
- stops future delivery without aborting or awaiting an in-flight callback
- ends `cancelled` when its acquisition signal aborts, also when already aborted
- ends `listener_error` on a throw, reports it, and delivers nothing more
- wraps a thrown non-Error in an Error
- Watch references
- retains neither its source, its listener, nor its signal once ended, while still referenced
- releases its pending queue and leaf index once delivered or ended
- NativeWatch
- delivers values by identity in order with a context, never inline
- collapses 101 pending values to the newest
- collapses 101 pending values to the value of `replace`
- ends with every reason of its lifecycle
- retains neither its source nor its listener once stopped, while still referenced

**参数化**（1 个站点、1 例）：
  - ``it(`keeps ${MAX_PENDING_OPS} pending ops after folding and replaces the root past them`, async () => …)`` —— 标题插值为 `keeps 10000 pending ops after folding and replaces the root past them`；断言 `Watch` 的 `PendingOps` 在折叠后恰好保留 10 000 个 op，超过后 `take()` 返回 `{kind:"replace"}`（owner 用最新值自建 `[["r", value]]`）。


### `test/websocket.test.ts`（4 条）

WebSocket：真实 socket 服务浏览器式客户端（authenticator stamp）；大 snapshot/result 以二进制发送、客户端不得发送打包帧。

- WebSocket links
- serves a browser-style client over a real socket, stamped by the authenticator
- WebSocket links: packed payloads
- large snapshots and results reach the client as binary messages; clients may not send them


---

## 8. 建议的 Rust 模块布局与最难复刻的部分

### 8.1 crate 划分

单 crate 可行（约 20k–30k 行），但建议按 npm 子路径拆 feature，便于下游只编译所需：

```
crates/chord/                    # package name: kiana-chord（lib name: chord）
├── Cargo.toml                   # features: default = ["delta","context"]; node = [...]; sqlite, sqlite-node, sqlite-cloudflare, telemetry-sqlite, ws
└── src/
    ├── lib.rs                   # 等价 src/index.ts（re-export）
    ├── error.rs                 # CHORD_ERROR_CODES, ChordError, is_chord_error, forbidden, invalid, as_chord_error, run_all, AggregateError
    ├── json.rs                  # JsonValue（serde_json + preserve_order）, copy_json, is_json_value, JsonError
    ├── schema/
    │   ├── mod.rs               # canonical_json, hash64, deep_equal, assert_valid, normalize, canonical_identity, fits
    │   ├── check.rs             # Value::check / errors（TypeBox 兼容消息）
    │   ├── clean.rs             # clean / default / clone（TypeBox Value.* 语义）
    │   └── types.rs             # Schema 表示（serde_json::Value 之上的一层，或自建 enum + 保留未知关键字）
    ├── canonical.rs             # js_number_to_string, escape_json_string, js_key_order
    ├── types.rs                 # Ctx, GetOptions, Readable, StateEvent, Shape, RefStatus, RefBase, Exposed, StateSource
    ├── declare.rs               # ResourceDeclaration, Resource, ListenerDecl (类型)
    ├── registry.rs              # Registry, define_registry, RegistryTable, RegistryView, Registration
    ├── publish.rs               # 全局发布队列（thread_local）
    ├── cell.rs                  # Fanout, HYDRATION, Hydration, hydration_of, unhydrated, Source, Cell, Derived, is_source, not_ready
    ├── state.rs                 # ReplicatedState 家族 + StateSubscriber + Publisher + Attached
    ├── watch.rs                 # WatchLifecycle, Watch, NativeWatch, WatchEnd, WatchHandle
    ├── contract.rs              # method/opaque/host_named/host_family/remote/local/coverage/bind/description
    ├── wire.rs                  # PROTOCOL_VERSION, Frame（serde tagged enum）, WireError, to/from_wire_error, is_frame
    ├── link/
    │   ├── mod.rs               # Link trait, BaseLink, Packed, pack/unpack, split/inflate, parse_frame
    │   ├── memory.rs            # MemoryLink, memory_link_pair
    │   ├── ipc.rs               # IpcChannel, IpcLink（`{chord2: frame}`）
    │   └── websocket.rs         # WebSocketLike, WebSocketLink, WebSocketLinkOptions
    ├── connection.rs            # Connection, RelayRef, RelayCall, LinkOptions, AcceptOptions, Peer, relay_call, client_error, validate_snapshot
    ├── host.rs                  # create_host, HostOptions, Host (trait), HostImpl, FacetApi, Facet, PluginRef, CodeVersionState, Serve
    ├── object.rs                # ServeOptions, make_service, host_lifetime, ResourceImpl/Facade, CtxImpl, RefCore/RefImpl, MemberReadable, ObjectImpl, SelfImpl, validate_implementation
    ├── remote.rs                # RemoteBinding, ViewReadable, RemoteRef, RemoteChannel
    ├── reload.rs                # ReloadOptions/Report, Reload, run_reload, begin_reload, end_reload, reload_error, dry_run
    ├── cluster.rs               # run_cluster_reload, reconcile_child, ChildReloads, modules_for, await_vote
    ├── loader.rs                # node_loader（feature node；JS 语义无法在 Rust 内实现同名能力，见 §8.6）
    ├── websocket.rs             # accept_websockets（被动接受）
    ├── output.rs                # OutputLimits, OutputBuffer, bound_output, sanitize_output, Progress, utf8_byte_length, character_end
    ├── context/
    │   ├── mod.rs               # Context trait, ContextKey, EmptyContext, Background/Todo, with_* 组合子, trace_in, abort_error
    │   └── telemetry.rs         # kiana-telemetry 绑定（trait 定义或依赖内部 crate）
    ├── delta/
    │   ├── mod.rs               # Op/WireOp, apply, apply_immutable, encoder, decoder, PathError, assert_*
    │   ├── diff.rs              # diff_revisions, overlap, json_cost
    │   ├── pending.rs           # PendingOps, MAX_PENDING_OPS
    │   ├── tracker.rs           # Tracker/Prepared/Change, overlay（显式 Draft API）, treap, array mutators, op 生成
    │   ├── apply_immutable_trusted.rs
    │   └── revision_validator.rs
    ├── sqlite/
    │   ├── mod.rs               # SqliteDatabase/Executor/Value（trait）
    │   ├── migrations.rs
    │   ├── schema.rs            # validate_sqlite_schema_prefix
    │   ├── node.rs              # feature sqlite-node（rusqlite）
    │   └── cloudflare.rs        # feature sqlite-cloudflare（Durable Object 绑定接口）
    ├── telemetry_sqlite/
    │   ├── mod.rs               # Recorder, create_sqlite_telemetry
    │   └── store.rs             # get_trace, find_traces, find_metrics, attributes_sql
    ├── manager.rs               # feature node：ControlMessage, Liveness, Driver, HostManager, fork_driver, process_lock, child_control
    └── node.rs                  # 等价 src/node.ts（re-export）
```

**Cargo.toml 关键点**：
```toml
[features]
default = ["delta", "context"]
delta = []
context = []
node = ["dep:tokio", "dep:sha2", "dep:serde", "manager"]
sqlite = []
sqlite-node = ["sqlite", "dep:rusqlite"]
sqlite-cloudflare = ["sqlite"]
telemetry-sqlite = ["sqlite", "dep:uuid"]
ws = ["dep:tokio-tungstenite"]
```
若同一 crate 内做 feature，注意 `#[cfg]` 交叉；拆分 `chord-sqlite`、`chord-node` 子 crate 更干净（对应 npm 子路径导出）。

### 8.2 核心类型草图

```rust
// error.rs
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChordErrorCode { Aborted, ContractMismatch, Forbidden, Gone, Invalid, NotInVariant, NotReady, Released, Superseded, Unavailable, UpgradeConflict, UpgradeRequired }
impl ChordErrorCode { pub fn as_str(self) -> &'static str; pub fn retryable(self) -> bool; }

#[derive(Debug, Clone)]
pub struct ChordError {
    pub code: ChordErrorCode,
    pub reason: Option<String>,
    pub message: String,
    pub details: Option<JsonValue>,
    pub cause: Option<Arc<dyn std::error::Error + Send + Sync>>,
}
impl std::fmt::Display for ChordError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.reason { Some(r) => write!(f, "{}/{}: {}", self.code.as_str(), r, self.message), None => write!(f, "{}: {}", self.code.as_str(), self.message) }
    }
}
```

```rust
// schema
pub struct Schema(pub JsonValue);        // 任意 JSON Schema（TypeBox 的产物）
pub fn canonical_json(v: &JsonValue) -> String;
pub fn hash64(text: &str) -> String;      // 16 hex；内部按 UTF-16 code unit 迭代
pub fn deep_equal(a: &JsonValue, b: &JsonValue) -> bool;
pub fn assert_valid(reason: &str, what: &str, schema: &Schema, value: &JsonValue) -> Result<(), ChordError>;
pub fn normalize(schema: &Schema, value: Option<&JsonValue>, what: &str) -> Result<JsonValue, ChordError>;
pub fn canonical_identity(schema: &Schema, value: &JsonValue) -> String;
pub fn fits(sub: &Schema, sup: &Schema) -> bool;
```

```rust
// delta —— 注意第二个字段始终是**路径**（Vec<Seg>），不是单个键
pub type Seg = String | i64;            // JS: string | number；JSON 对象键与数组下标共用（数字段恒为非负整数）
pub type Path = Vec<Seg>;               // 非空约束由类型/断言保证，不是 Path 本身
pub type NonEmptyPath = Path;           // Rust 侧用运行时断言（"path is empty"）

pub enum Op {
    Replace(JsonValue),                          // ["r", value]
    Set(Path, JsonValue),                        // ["s", path, value]
    Delete(Path),                                // ["d", path]
    Append(Path, String),                        // ["a", path, text]
    Truncate(Path, usize),                       // ["t", path, count]  count = 保留的 UTF-16 单元数（JS 侧是非负整数）
    Splice { path: Path, index: usize, remove: usize, items: Vec<JsonValue> },  // ["p", …]
    Permute(Path, Vec<usize>),                   // ["m", path, permutation]
}

pub enum WireOp {
    Intern(u32, Path),                           // ["#", id, path]
    Op(PathRef, Op),                             // 完整形式：ref 是内联 Path 或已定义 id
    Short(Op),                                   // 省略路径的短形式（与上一条 op 同路径）
}
pub type PathRef = either::Either<Path, u32>;

/// 所有 `assertValidOp` 的 TypeError、`PathError`、`UnsafePathError` 都归到这里；
/// `message()` 必须逐字复刻（`r arity`、`unresolvable path: [...]`、`unsafe path segment: x` …）。
#[derive(Debug, Clone)]
pub enum OpError {
    Invalid(&'static str),       // TypeError 系：消息是静态串
    UnknownVerb(String),
    Path(JsonValue),             // PathError：path 或 id
    UnsafeSegment(JsonValue),    // UnsafePathError：segment
}
impl OpError { pub fn message(&self) -> String; }

pub fn apply(root: JsonValue, ops: &[Op]) -> Result<JsonValue, OpError>;   // 返回**新根**（r 无法就地替换）
pub fn apply_immutable(root: JsonValue, ops: &[Op]) -> Result<JsonValue, OpError>;
pub fn apply_immutable_batches(root: JsonValue, batches: impl IntoIterator<Item = Vec<Op>>) -> Result<JsonValue, OpError>;
/// 不校验、不返回错误：调用方已保证 op 自产（错误在 JS 里也是 TypeError，属于"违反了调用契约"）。
pub fn apply_immutable_trusted(root: JsonValue, ops: &[Op]) -> JsonValue;
pub struct Encoder { seen: HashSet<PathKey>, ids: HashMap<PathKey, u32>, next_id: u32, previous: Option<PathKey> }
pub struct Decoder { paths: HashMap<u32, Path>, previous: Option<Path> }    // paths 跨批，previous 每批
pub fn diff_revisions(a: &JsonValue, b: &JsonValue) -> Vec<Op>;
pub struct PendingOps { /* Empty | Batch(Vec<Op>) | Folding(Folding) | Replace */ }
```
注：`PendingOps::take()` 的 replace 分支**不带值**，owner 用当前值自建 `[["r", value]]`。

```rust
// host / object
pub trait Host: Send + Sync { … }                 // 等价 Host<P>
pub struct HostImpl<P> { … }
pub struct FacetApi<P> { … }
pub type Facet<P> = Arc<dyn Fn(FacetApi<P>) -> BoxFuture<'static, ()> + Send + Sync>;
pub struct Serve { contract: AnyContract, options: ServeOptions, impl_: ??? }
```
`impl_` 是最难的：TS 里实现是普通对象（方法即函数）。Rust 中建议：
```rust
pub trait ObjectImplementation: Send + Sync {
    fn variant(&self) -> Option<&str> { None }
    fn method(&self, name: &str, ctx: Ctx) -> Option<BoxFuture<'static, Result<JsonValue, ChordError>>>;
    fn state(&self, name: &str) -> Option<StateSourceRef>;
}
```
或让用户用 `#[derive]`/宏生成。

### 8.3 必须保留的"不可见但可观察"的顺序

1. **publish 队列**：`enqueue` 在空闲时**同步**执行；在 `holdPublications` 内只入队；drain 在 `finally` 触发。订阅者回调因此发生在写操作**之后**、下一个宏任务之前。
2. **微任务**：`WatchLifecycle.#schedule` 用 `queueMicrotask`（不是 `Promise.resolve().then` 之外的东西，二者等价）；`state.reportErrorAsync` 用 `queueMicrotask` 抛出（→ 未捕获异常）。
3. **dependents 排序**：`computeDependents` 结果按 `derived.seq` 升序（= 创建顺序），**不是**拓扑序。
4. **connection 的 send 顺序**：`#flush` 遍历 pending **按插入顺序**；新 slot 追加到末尾；`unit` slot 在窗口满时保持原位（不重排）。
5. **#scheduleAck**：`setTimeout(0)`，因此 ack 在同一 burst 处理完后才发出，且多个帧共用一个 ack。
6. **connection 关闭**：`#closed` 先清 timer，再按 `served → incoming → calls/votes/syncWaiters → relayedRefs → remotes` 顺序清理。
7. **reload install 顺序**：`minCodeVersion → retireImplementation → refreshSchemas → cell.reset → derivations → staged.activate → openGate → host objects → client upgrade`。
8. **telemetry flush**：`while (this.writing) await` 保证串行；`finally` 清 `inFlightMeasurements`。

### 8.4 最难复刻的部分（按风险排序）

1. **ES Proxy draft（`delta/tracker.ts`，2292 行）**
   - TS 里用户直接对 `draft.x.y = …` 赋值，一切经由 Proxy handler（`get/set/deleteProperty/has/ownKeys/getOwnPropertyDescriptor/defineProperty/preventExtensions/setPrototypeOf`）。
   - Proxy 使**大规模对象/数组的读写**、`in`、`Object.keys`、`JSON.stringify`（会触发 `ownKeys` + `get`）都走 overlay。
   - Rust 无 Proxy：必须改为显式 `Draft` API（`draft.get(path)`, `draft.set(path, v)`, `draft.remove(path)`, `draft.keys()`, `draft.len()`, `draft.iter()`…）并在其上生成**逐字节相同**的 op 序列。
   - **最硬的点**：op 序列由遍历顺序、dense region 位图、treap 分段、sort 去重、append 合并共同决定。任何顺序差异都会导致线协议内容不同（虽然值相同）。若只要求"值等价"，可放宽；若要求"op 等价"（测试对比 `ops`），必须复刻算法。
   - `p`/`m` 的生成依赖 `insertRunsOf`/`buildArrayPlan`/`orderedArrayPlan` 的精确规则，`sort` 的 `deduplicateArrayEntries` 依赖排序稳定性与 token 比较。
2. **`schema.ts` 的 TypeBox 兼容层**
   - `Value.Check` 覆盖 draft-07 子集 + TypeBox 扩展；`Value.Clean` 会删除 `additionalProperties:false` 之外的多余键；`Value.Default` 深注入默认值；`Value.Clone` 深拷贝。
   - `Value.Errors` 的 `instancePath` 与 `message` 文案决定所有 `invalid/args`、`invalid/state` 的用户可见消息。**这是逐字级要求**，需要按 TypeBox 源码逐条移植（`errors.ts` 的 message 表）。
   - `fits` 是**手写**的保守子类型算法（14 步），不是 JSON Schema 标准，必须逐行照搬（包括 `objectFits` 里那些"看起来反直觉"的 `return false`）。
3. **canonicalJson + hash64 + 指纹**
   - 键顺序（整数键优先、其余插入序）、`undefined` 键过滤、`JSON.stringify` 的数字/字符串格式。
   - `hash64` 必须按 UTF-16 code unit（与 Rust `char` 不同）迭代，`Math.imul` 用 `wrapping_mul`。
   - 指纹错误 → contract_mismatch 全线漂移（binding 不再共享、reload 误判"schema changed"）。
4. **流控 slot 机（`connection.ts` 的 `#queueUnit`/`#makeRoom`/`#drop`/`#flush`/`#resetLater`）**
   - `sendState` 的 4 种 slot 状态（无 slot / unit / state 累积 / lazy）、`pendingBytes` 的精确扣减、`#makeRoom` 的插入序选择、`overflow.resync` 回调。
   - 窗口按 `link.send()` 的返回值计费，因此 **MemoryLink（`data.length`）、WebSocket（文本 length / 二进制 byteLength）、IPC（0）** 的行为各不相同；移植时必须让每个 transport 返回相同的"计费单位"（UTF-16 长度 vs 字节）。
5. **reload 的 gate / retire / rollback**
   - `gate` 用 grandfathered roots 集合；`waitForRetirement` 超时后**强制 abort**（`superseded/code version N was retired`）；`rollback` 要恢复被替换的独占资源（close 失败也要 restore）。
   - `install` 的 cell `reset` 会让订阅者收到 `reset` 事件（不是 ops），且 schema 同时替换。
   - 顺序错误会出现"旧 root 访问新版本"或"新 root 访问已退役版本"。
6. **集群 reload**
   - 投票先登记后发送；`Promise.allSettled` 的"本地 prepare 与远端投票并行"；失败时**先** aborts 再 rollback；`commit(beforeStart)` 里先发 commit 再置 `peerCodeVersion`。
   - `reconcileChild` 的 announceRetired-before-prepare 与"host 已前进则 abort 重试"。
7. **watch 的串行投递 / 溢出 / retirement**
   - "不内联"（必须 queueMicrotask）、"串行 await"、"投递时用最新 frame 的 context 且 `withoutAbortSignal`"、"in-flight 不参与溢出折叠"、"retirement 丢弃 pending 但投递 `[["r", null]]`"。
8. **Node IPC / manager**
   - Node 的 IPC 是私有协议（JSON + `NODE_` 句柄），纯 Rust 无法复用。要么保留 Node 子进程边界（用 `napi`/sidecar），要么定义新协议（**会改变 wire 可见行为**，需产品决策）。
9. **`structuredClone` 与 `WeakMap` 语义**
   - `structuredClone` 对 `undefined`、`-0`、`NaN`、Date/Map/Set 的处理；Chord 状态限定严格 JSON，可用 JSON 深拷贝近似，但 `-0` 会变 `0`（需确认测试是否覆盖）。
   - `WeakMap` 用于 `identities`（telemetry）、`PREPARED`（tracker）、`bindings`/`views`（Ref）——Rust 需显式回收，否则内存增长或悬垂。
10. **UTF-16 与孤立代理**
    - `t`（truncate）的 count 是 UTF-16 码元；`overlap` 用 `slice`；`ownedSlice` 用 `slice`；`PACK_THRESHOLD` 用 `json.length`（UTF-16）。若内部用 UTF-8 `String`，这些全错。
    - 孤立代理在 `utf8ByteLength` 中算 3 字节（U+FFFD），但在 `JSON.stringify` 中会被转义为 `\udXXX`；两条路径不同。
11. **Deflate 输出**
    - 逐字节一致的 deflate 需要绑定 zlib（`flate2` 的 zlib backend 也未必同版本）；若测试只校验"能解压"，可用任意 level。
12. **`assertValid` 的 3 条截断 + `"; "` 连接 + `instancePath || "/"`**
    - 细节小但全线可见。

### 8.5 建议的迁移顺序

1. 纯函数层：`error`、`json`、`canonical`、`schema`（check/clean/default/errors/fits）、`delta`（op、apply、applyImmutable、diff、pending）。
2. `context`、`publish`、`cell`、`state`、`watch`（不依赖网络，测试最密）。
3. `contract`、`wire`、`link`（memory/ipc/ws）、`connection`（流控）。
4. `object`、`host`（对象生命周期、地址、引用）。
5. `remote`、`reload`、`cluster`。
6. `sqlite`、`telemetry-sqlite`、`output`。
7. `manager`、`loader`、`websocket`（Node 专属，最后处理，且需要与产品确认 IPC 方案）。

每一步的"完成判据"：该模块在本单元测试清单（§7）中的全部 `it` 都有对应 Rust 测试通过。跨模块行为由四个"综合"文件把关，建议最后整体迁移：`test/host.test.ts`（25 条）、`test/links-review.test.ts`（15 条）、`test/review.test.ts`（18 条）、`test/tutorial.test.ts`（11 条）——它们覆盖从 `createHost` 到链路、reload、流控的端到端路径。

### 8.6 开放问题（需产品/架构决策）

1. **Node IPC 兼容**：`forkDriver`/`childControl` 依赖 Node 的 `child_process.fork` IPC。纯 Rust 无法逐字节兼容。选项：(a) 保留 Node sidecar 只做 IPC 桥；(b) 定义新的长度前缀协议并接受不兼容；(c) manager 整体不进 Rust（作为 Node-only 模块保留）。
2. **loader**：`nodeLoader` 依赖 Node ESM loader hooks 与 `?chord2-reload` query 产生新模块实例。Rust 中动态重载用户代码只有两种路径：WASM 组件（`wasmtime` + 每次 reload 实例化）或进程外插件（IPC）。**功能无法 1:1**，需要决定 Chord 在 Rust 里是否仍然承载"用户 JS 插件"（若是，则嵌入 `rquickjs`/`deno_core`）。
3. **Proxy Draft 的 API 破坏**：若下游 Rust 代码需要 `draft.x = y` 的体验，只能靠宏（`draft!` DSL）或 `Index`/`IndexMut`（但无法记录路径与类型）。是否接受显式 `Draft` API 语义变化？
4. **`structuredClone` 的边界**：是否要求支持 Date/Map/Set（当前 Chord 状态限定严格 JSON）；`-0` 是否必须保留。
5. **`Type.Unsafe(TSchema)` 的 SchemaObject**：Description 里的 `input`/`args`/`result` 是任意 JSON Schema。Rust 端是否需要支持 `patternProperties` 的 JS `u` 标志语义（影响 `fits` 与 `assertValid`）。
6. **`WeakRef` 语义**：`tracker` 的 `#contexts` 在 TS 中用于 GC 后清理；Rust 是否需要等价的"对象失效检测"（否则 `Prepared` 可能长期存活）。
7. **Deflate 逐字节一致**：是否有测试比对压缩字节（若有，必须绑定同版本 zlib）。
8. **`unref()` 的进程退出语义**：是否要求 Rust 进程在只剩心跳/linger 定时器时也能退出（影响 `tokio` runtime 关闭策略）。
9. **`crypto.randomUUID`**：是否要求与 Node 完全相同的实现（v4，随机源不同不影响格式）。
10. **`kiana-telemetry` 的 crate 边界**：本单元需要 `TelemetryContext` 等 trait；是由 `kiana-telemetry` crate 提供，还是 `chord` 内定义并由上层适配？
11. **`typebox@1.3.27` 源码不可得**：仓库未安装 `node_modules`，`Value.Errors` 的逐字文案无法从磁盘核对（§3.4）。移植 `schema/check.rs` 前必须取得该版本源码（`npm pack typebox@1.3.27`），否则 `invalid/*` 消息无法保证逐字一致。
12. **`TypeBox` 的 `Value.Clean` 与 `additionalProperties`**：`normalize` 依赖 `Clean` 删除 `additionalProperties:false` 之外的未声明键；需确认 `Clean` 在遇到 `patternProperties`、`anyOf`、嵌套 `additionalProperties` schema 时的删除范围（Rust 侧必须完全一致，因为这决定了 `canonicalIdentity` 与 `fits` 的输入）。
13. **`process.getBuiltinModule` 的可用性**：浏览器构建下 `zlib` 为 undefined，走 `DecompressionStream("deflate-raw")` 异步路径（`deliverLater`）。Rust 端若只做原生，可恒为"有 zlib"，但**跨实现互操作**（Rust host ↔ 浏览器 client）时，浏览器仍会在 `PACK_THRESHOLD` 之上发/收 packed 帧，因此 Rust 必须实现完整的 `pack/unpack/splitMessage` 三条路径。
14. **`MemoryLink.transmit` 的计费单位**：返回 `data.length`（**UTF-16 字符数**），而 `WebSocketLink` 返回 `byteLength`（字节）或 text 的 `length`（UTF-16）。流控窗口以这些返回值累加，因此**同一帧在两种 transport 上的计费不同**。Rust 的 `Link::send` 必须保留这一差异，不能用统一的"字节数"。
15. **`IpcLink` 不受流控**：`transmit` 返回 `undefined` → 计 0 → 永不触发窗口。若 Rust 改用长度前缀 socket，必须显式保留"不参与流控"的行为，否则跨实现会出现窗口死锁。
16. **`test/delta-tracker/retention.worker.ts`**：依赖 `--expose-gc` + `FinalizationRegistry` 验证 overlay 释放后不再保留大字符串。Rust 侧只能以 `Drop` + 内存断言近似，无法等价。
17. **`test/telemetry-sqlite.test.ts` 依赖 telemetry 单元的 testing 子路径**：`createTelemetryAdapterConformance` 来自 `packages/telemetry/src/testing/conformance.ts`（14 个用例，去掉 `metrics` 组 2 个后本单元跑 12 个）。它是否随 `kiana-telemetry` 一起移植、以什么 crate 路径暴露（`kiana-telemetry::testing`？），决定本单元这 12 个用例在 Rust 侧能否原样复用。

---

## Audit additions（对抗性完整性审计的补充与勘误）

本节由审计方逐文件复核 `packages/chord` 全部源码（42 个源文件 + `package.json`）与 53 个测试文件后追加。**上文（§0–§8）保持原样**，此处只补上文遗漏或写错的内容。所有条目均已对照源码逐字核对。

### A.1 已在上文内联修正的错误陈述

1. **§5.1 的 `withTelemetry`/`traceIn` 签名与"无 telemetry 直调 `fn`"分支（已改正）**。源码与 §2.26 均为 `withTelemetry(telemetry, context)`、`traceIn(options, fn, context)`；`traceIn` 恒走 `context.telemetry.startSpan(...)` —— `BaseContext.telemetry` getter 在无值时回退 `NOOP_TELEMETRY_CONTEXT`，**不存在**"无 telemetry 就直接调用 `fn`"的短路。Rust 侧必须保持"即使 NOOP 也经过 `start_span`"这一层（否则 span 生命周期钩子的可观察行为不同）。
2. **§2.1"`contract` 的 12 个值"（已改正为 8 个值 + 13 个类型）**。`index.ts` 从 `contract.ts` 只 re-export 8 个值：`bind`、`coverage`、`hostFamily`、`hostNamed`、`local`、`method`、`opaque`、`remote`。
3. **§2.17 / §3.3 的 `nextCodeVersionId` 消息（已改正）**。逐字是 `` `code version ${given} is not newer than ${this.current.id}` ``（插值的是 `current.id`，不是 `current` 对象）。§3.3 列表中的模板也一并改为 `${current.id}`。
4. **§2.11 的"见 §2.6 补充"（已改正）**。§2.6 是 `types.ts`，与 `state.ts` 无关；已将 §2.11 的完整接口内联进去（原先只有一句话，`ReplicatedStateSourceFrame` / `ReplicatedStateSourceAttachment` / `ReplicatedStateSource` / `ReplicatedStateSourceOptions` / `AttachedReplicatedState` 的字段全部缺失）。
5. **§2.20 `prepareObject` 的 `"exclusive resource replaced"` 触发条件（已改正/扩写）**。源码是 `if (deferred.size > 0) candidate.reasons.push("exclusive resource replaced")`，而 `deferred` = 所有独占新资源 **加上依赖任一 deferred 资源的资源**。因此：**任何独占新资源都会让对象走 gate**（哪怕没有对应的旧资源、没有任何依赖链），消息措辞只是历史遗留。上文原句"新的独占资源替换旧的 → reasons 追加"会误导实现者写成"仅当旧资源存在且 open 时才 gate"。
6. **§2.20 `migrate()` 的取值分支（已补全）**。原句只写"`structuredClone` 旧值"。实际：只有 `to > from && fn !== undefined` 才 `fn(structuredClone(cell.value), from)`；`to > from` 但无迁移函数、或 `to === from` 但 schema 变了 → **直接用 `cell.value`**（值不变，仍按新 schema `assertValid`）；两者都没变 → `continue`（完全不进 `candidate.migrated`，install 时只更新 `cell.schema`）。另外 `dryRun(candidate)` 在 `migrate()` 末尾对**所有** gated candidate 调用（且 `gate()` 里的 `initCells` 也在每个 gated 对象上跑，与 §2.20 第 6 条一致）。
7. **§2.20 `checkRefs()` 的语义（已改写）**。原句"facets 的 `uses` 与 registry 表的绑定检查"是错的：facet `uses` 检查在 `checkProviders()`，registry 表在 `buildTables()`。`checkRefs()` 只做"活到新代码版本的引用必须 `bind` 得上"，且**跳过** host bootstrap 引用（`host.hostObjects.get(typeName)`）与 `owner === "client"` 的引用。
8. **§2.19 `RemoteRef.update()` 的重复条目（已删除重复项）**。原文在同一节出现两次 `update()` 描述，且第一次的措辞不完整；保留与源码一致的那条（`ended` 提前返回、状态未变提前返回、`live` 时 resolve ready 并清 `#settleReady`）。
9. **§4.7 store 小节的 `IN ('integer','real')`（已改正）**。源码 `telemetry-sqlite/store.ts` 只有**一个** `attributesSql`，number 分支逐字是 `entry.type IN ('integer', 'real')`（**逗号后一个空格**）；原 §4.7 写成无空格版本，会诱导 Rust 侧写出与 TS 不同的 SQL。已在 §4.7 就地更正并在 A.3 补出完整模板。
10. **§2.20 `rollback()` 的字段补充（已扩写）**。补上：幂等、discard staged、`markUnavailable()`、"gated 对象 `openGate()` + `startTasks()`"、以及最后 `host.pending = undefined` / `host.gating = undefined`。

### A.2 上文完全未提及的导出符号（Rust 必须提供等价物）

以下名称在源码中带 `export`，但 §0.1 / §2.x / §3.6 的任何清单里都没有出现。它们**都不在 9 个入口模块的导出面内**（§3.6 的 189/74/115 已复核为正确），但按"逐文件移植"的要求仍需在对应模块内保留（Rust 侧即 `pub` 条目或 crate-内部 `pub(crate)`，取决于该模块是否成为独立 crate 边界）。

**`src/contract.ts`（9 个导出类型，全部缺失）**
```ts
export interface MethodSpec<A extends TSchema | undefined = TSchema | undefined,
                            R extends TSchema | undefined = TSchema | undefined> {
  readonly [MEMBER]: "method"; readonly args: A; readonly result: R;
}
export interface OpaqueSpec<T = unknown> { readonly [MEMBER]: "opaque"; readonly id: string; readonly type?: T }
export interface VariantDef {
  readonly state?: Readonly<Record<string, TSchema>>;
  readonly methods?: Readonly<Record<string, MethodSpec>>;
}
export interface RemoteDef {
  readonly version: number; readonly input: TSchema;
  readonly host: Placement;                                   // hostNamed(…) 或 hostFamily(…)
  readonly variants: Readonly<Record<string, VariantDef>>;
}
export type LocalMember = MethodSpec | OpaqueSpec | TSchema;
export interface LocalDef {
  readonly version: number; readonly input: TSchema;
  readonly members?: Readonly<Record<string, LocalMember>>;
}
export type MethodImplementation<M, P = unknown> = M extends MethodSpec<infer A, infer R>
  ? [A] extends [TSchema] ? (args: Static<A>, ctx: Ctx<P>) => Returned<R> : (ctx: Ctx<P>) => Returned<R>
  : never;
export type Call<M> = M extends MethodSpec<infer A, infer R>
  ? [A] extends [TSchema] ? (args: Static<A>, ctx?: Ctx) => Promise<Resolved<R>> : (ctx?: Ctx) => Promise<Resolved<R>>
  : never;
export type MemberDescription = Static<typeof MemberDescriptionSchema>;
// = { kind: "method"; args?: TSchema; result?: TSchema }
// | { kind: "state"; schema: TSchema }
// | { kind: "opaque"; id: string }
```
（`MEMBER = Symbol("chord.member")` 是 `contract.ts` 的模块私有 brand；`MethodSpec`/`OpaqueSpec` 是 `method()`/`opaque()` 的返回类型，`VariantDef`/`RemoteDef`/`LocalDef` 是 `remote()`/`local()` 的入参类型 —— 三者是**公共类型面的一部分**，只是因为 `index.ts` 未 re-export 而漏在 §3.6 之外。**第 9 个（也是本次审计新发现的）是 `MemberDescription`**：§4.1 / §2.13 只把它当作 JSON 形状 `<MemberDescription>` 描述，从未指出它同时是一个具名导出类型（`contract.ts:134`，`Static<typeof MemberDescriptionSchema>`）—— Rust 侧 `Description.variants` 的值类型需要这个具名枚举，否则 `description` 的序列化/反序列化只能退化成 `serde_json::Value`。`Call` 本身则被 `VariantRef`（`contract.ts:398`）与 `LocalRefMember`（`:409`）引用，是 `Ref<C>` 上方法签名的来源；`MethodImplementation` 被 `VariantImplementation`（`:389`）与 `LocalImplementationMember`（`:402`）引用。）

**`src/cell.ts`**
```ts
export type SourceValues<S extends Sources> = { [K in keyof S]: S[K] extends Readable<infer T> ? T : never };
```
用途：`Self.derive` 的 `fn` 参数类型（`fn: (...values: SourceValues<S>) => T`，见 `object.ts` 第 195 行与 1902 行）。Rust 侧对应用泛型元组/`HList` 风格的辅助 trait。

**`src/state.ts`**：无遗漏导出（`ReplicatedStateSourceOptions.onError` 见 A.3）。

**`src/object.ts`（2 个导出类型）**
```ts
export type StatusListener = (status: RefStatus, error: unknown) => void;
export interface ObjectIdentity {
  readonly address: string; readonly typeName: string; readonly input: unknown;
  readonly principal: unknown; readonly hostLifetime: boolean;
}
```
`StatusListener` 被 `remote.ts` 直接 import（`#statusListeners` 的元素类型）；`ObjectIdentity` 是 `ObjectImpl` 构造函数的入参类型（`constructor(host: HostCore, identity: ObjectIdentity)`）。两者属于跨模块契约（见 A.5）。

**`src/wire.ts`（2 个导出类型，且不属于 `index.ts` 导出面）**
```ts
export type WireReloadOptions = Static<typeof WireReloadOptionsSchema>;
// { remove?: string[]; gateDeadline?: number; retireDeadline?: number }（数组为 Type.Immutable(Type.Array(...))）
export type WireReloadReport = Static<typeof WireReloadReportSchema>;
// { codeVersion: integer; previous: integer; changed: string[]; removed: string[]; prepared: string[]; gated: string[] }
```
§3.6 逐字列出了这两个 schema（`WireReloadOptionsSchema` / `WireReloadReportSchema`），但从未指出它们**同时以 `WireReloadOptions` / `WireReloadReport` 类型名导出**（`cluster.ts` 的 `prepare` 帧组装与 `Reload.summary()` 依赖它们）。§4.3 只说"同 `ReloadReport` 去掉 `children`"—— 那条是语义等价，不是类型名。

### A.3 上文写漏的行为细节（按源文件）

**`src/context/index.ts` 与 `src/telemetry-sqlite/*`（§5.1 / §2.34）**
- §5.1 的"`chord` 直接用到的符号"清单**漏了 5 个**：`store.ts` 还 `import type { MetricQuery, StoredMetric, StoredSpan, StoredTrace, TraceQuery }`（§5.1 的代码块里给出了这 5 个接口的形状，但清单句子里没列，Rust `kiana-telemetry` crate 的导出面因此会被少做）。实际 import 并集是 17 个：`InstrumentOptions`、`NOOP_TELEMETRY_CONTEXT`、`SpanAttributes`、`SpanLink`、`SpanOptions`、`SpanStatus`、`TelemetryCarrier`、`TelemetryContext`、`TelemetryMetrics`、`TelemetryPropagation`、`TelemetrySpan`、`TelemetryStore`（`index.ts`）+ `MetricQuery`、`StoredMetric`、`StoredSpan`、`StoredTrace`、`TraceQuery`（`store.ts`）。
- §5.1 代码块与源码一致（`TelemetryContext.startSpan<T>(options, callback): Promise<T>` 已在块内），此处只需提醒：`traceIn` 是**唯一**依赖 `startSpan` 的调用点，且它要求 `context.telemetry` 恒为 `TelemetrySpan`（`NOOP_TELEMETRY_CONTEXT` 必须也实现 `startSpan`，即"立刻调用回调并 await"）；若 Rust 的 NOOP 实现只返回 `ready(())` 而不调用回调，`traceIn` 的语义会整个丢失。

**`src/state.ts`（§2.11）**
- **`ReplicatedStateSourceOptions.onError?: (error: Error) => void`**：`AttachedReplicatedStateImpl` 构造时 `#reportError = options.onError ?? reportErrorAsync`，并用它包住 publisher（`new ReplicatedStatePublisher(snapshot.value, error => this.#report(error))`）。`#report(error)` 的实现是：
  ```ts
  try { this.#reportError(toError(error)); } catch (reportError) { reportErrorAsync(toError(reportError)); }
  ```
  —— 即**用户回调自己抛错时降级为 `reportErrorAsync`（微任务里 throw，成为未捕获异常）**。这条"用户 onError 抛错也安全"的行为在 §2.11 中完全缺失，且是 Rust 侧 `on_error: Option<Box<dyn Fn(Error)>>` 必须复刻的语义。`MutableReplicatedStateImpl` 不暴露该选项（构造只有 `initial`），其 publisher 用默认 `reportErrorAsync`。
- `#fail(error)` 在 `attachment.dispose()` 抛错时**先 report `AggregateError` 再 `return`**，与 §2.11 的描述一致（已核对）。
- `replicatedState` 的两个重载：`replicatedState<T extends object>(initial: T)` 与 `replicatedState<T>(source: ReplicatedStateSource<T>, options?)`。判据是 `typeof (value as {attach?: unknown}).attach === "function"`（§2.11 已写对）。

**`src/schema.ts`（§2.5）**
- `asSchema(schema: unknown): Schema`：**非对象且非 `false` → `{}`；`false` → `{ not: {} }`**；否则原样。`asSchema` 用于 `canonicalIdentity`→`stripDefaults`、以及 `fits`/`objectFits`/`arrayFits`/`functionFits` 的每一步取子 schema。这是 `fits` 能正确处理"`additionalProperties: false` / `true` / schema 三态"与"schema 为 boolean"的前提；§2.5 只提"两侧去注解"，没提这个归一化，Rust 实现若直接 `&SchemaObj` 会漏掉 `false` 分支（`false` 在 draft-07 里表示"无合法值"，对应 `{not:{}}`；`true` 等价 `{}`）。
- `withoutAnnotations(schema)` = `Object.fromEntries(Object.entries(schema).filter(([key]) => !ANNOTATIONS.has(key)))` —— 与"去注解"字面等价，但注意它**只做浅层**去除（嵌套 schema 的注解在各自递归调用点去除）。
- `assertValid` 的 `details` 是**前 3 个 TypeBox 错误对象数组本身**（不是字符串），`invalid(reason, message, details)` 的第三参即 `errors`。Rust 侧 `details` 必须能序列化成同样的形状（`instancePath`/`message`/`schemaPath`/`keyword`/`params` 等字段）。
- `normalize` 的顺序是 `Clean(Default(Clone(value ?? {})))` —— **先 Clone 再 Default 再 Clean**（`Clean` 在最后，因此 `Clean` 能看到默认值注入后的结果）。§2.5 的写法 `Value.Clean(schema, Value.Default(schema, Value.Clone(value ?? {})))` 已正确。

**`src/contract.ts`（§2.13）**
- `resolveHost` 对 `hostFamily` 的输入字段校验用的是 `Value.Check`？—— **不是**：源码直接判断 `typeof value !== "string" || value.length === 0`，然后 `invalid("placement", `${typeName}: input field ${placement.key} names no host`)`。§2.13 写"必须是非空字符串"已覆盖。补一条：`placedOn(placement, hostName)` 对 family 的判据是 `hostName.startsWith(prefix) && hostName.length > prefix.length`（**必须严格长于前缀**，否则 `prefix = "session:"` 且 `hostName = "session:"` 会被接受 —— 这是 Rust 移植时最容易写成 `>=` 的地方）。
- `coverage` 第 2 条 `samePlacement` 的比较对象是 `canonicalJson(host ?? null)`；即 **remote 的 host 参与比较、local（无 host）视为 `null`**。
- `DescriptionSchema` 的 `host` 是 `Type.Optional(PlacementSchema)`，而 `PlacementSchema` 两分支都 `additionalProperties: false`；`MemberDescriptionSchema` 的 `args`/`result`/`schema` 都是 `SchemaObject = Type.Unsafe<TSchema>(Type.Object({}))`（宽松对象，**不**校验内部 schema 结构）。
- `contractFromDescription` 的校验是 `typeof name === "string"`、`scope === "remote" || scope === "local"`、`Number.isInteger(version)`、`(scope === "remote") === (host !== undefined)`；失败统一 `new ChordError("invalid", "malformed contract token", { reason: "token" })`。remote 的 def 里 `variants: {}`（**故意为空**）—— 因此 `contractFromDescription` 得到的 contract 不能用于 `coverage` 逐成员比较，只能用于 `name/version/input/host`。（§2.13 已写。）

**`src/wire.ts`（§2.14 / §4.3）**
- `hello` 的 `protocol` 是 `Type.Optional(Type.Integer({ minimum: 1 }))`，缺省按 1 处理（`(frame.protocol ?? 1) !== 4`）。
- `FrameSchema` 的 23 个变体**每一个都是 `Type.Object(...)`，都没有显式传 `additionalProperties: false`**。因此**多出的未知字段是否算协议违规，取决于 `typebox@1.3.27` 里 `Type.Object` 的默认行为 —— 而该版本源码不在磁盘上（见 §3.4 与 §8.6 第 11 条），无法核对**。**已知事实**：仓库里**唯一**显式写 `additionalProperties: false` 的地方是 `contract.ts` 的 `PlacementSchema` 两个分支（`hostNamed`/`hostFamily` 的描述）；`DescriptionSchema`、`FrameSchema`、`WireErrorSchema`、`RootRefSchema`、`WireReload*Schema`、`MemberDescriptionSchema` 都**没有**。Rust 侧在拿到 typebox 源码前**不要**擅自给帧加 `deny_unknown_fields`：若 TypeBox 实际允许额外字段，加了会与 TS 实现产生可观察分歧（对端发来带额外字段的帧时一边接受、一边关闭链路）。已确认的违规路径只有：缺必填字段、类型不符、`t` 不属于 23 个字面量 —— 测试 `test/links-review.test.ts` 的 "#10 malformed frames and duplicate ids close the link" 用的就是**缺 `id` 的 `{t:"call", hold:1}`**，不是额外字段。
- `toWireError` / `fromWireError` 的 `reason` 为 `undefined` 时**完全不出现该键**（不是 `reason: undefined`）—— 影响 JSON 逐字节。

**`src/link.ts`（§2.15）**
- 导出的类/函数里，`Packed`、`MemoryLink`、`IpcLink`、`WebSocketLink`、`unpack` **也都带 `export`**，但 `index.ts` 只 re-export `inflateMessage`、`ipcLink`、`memoryLinkPair`、`PACK_THRESHOLD`、`webSocketLink` + 类型 `IpcChannel`/`Link`/`WebSocketLike`/`WebSocketLinkOptions`。§3.6 未指出 `Packed`（日志里出现过，标注"(导出的类)"但未列入任何子路径清单）与三个 Link 类是"`link.ts` 内导出、入口面之外"。
- `packedBytes` 的判据顺序：`value instanceof Packed` → 原 bytes；`!compress || !zlib || json === undefined || json.length <= PACK_THRESHOLD` → `undefined`。**`json.length` 是 UTF-16 码元数**（§2.15/§4.2 已强调）。
- `packMessage` 未打包路径不是简单 `JSON.stringify(frame)`：源码先 `JSON.stringify(frame)`，若 payload 字段存在且未压缩，则用**字符串拼接**把该字段内联（`` `${head.slice(0,-1)},${JSON.stringify(field)}:${json}}` ``）以避免二次编码（§2.15 已写）。
- `IpcLink.#onSent`：`error !== null` → `ended(`ipc send failed: ${(error as {code?: string}).code ?? error.message}`)` —— 注意**取 `.code` 优先于 `.message`**（§2.15 已写，此处提醒 Rust 需要 `Error::raw_os_error`/`ErrorKind` 到 `.code` 的映射）。

**`src/connection.ts`（§2.16）**
- `DEFAULT_WINDOW = 256 * 1024`、`PENDING_WINDOWS = 4`、`#pendingCap = config.pendingCap ?? #window * PENDING_WINDOWS`（§2.16 已写）。
- 内部 slot 键常量：`const stateKey = (hold, m) => \`s${hold}\u0000${m}\``、`const unitKey = (hold) => \`u${hold}\``（§4.10 已给出两种键格式，但 §2.16 的类成员清单里没有这两个函数名）。
- `#onCall` 的 `maxCalls` 判据是 `this.incoming.size >= this.#maxCalls`（**先查重再查上限**：重复 id 优先报 `duplicate call id`）。§2.16 的 `#onCall` 描述已按此顺序。
- `#closed(reason)` 的协议版本分支是 `/^protocol-version (\d+)$/.exec(reason)`，且仅在 `n !== PROTOCOL_VERSION` 时 `#protocolMismatch(n, false)`。

**`src/host.ts`（§2.17）**
- `#principalPart(principal)` = `` `p:${this.#principalKey(principal)}` ``（§4.10 已含）。
- `addressOf` / `#locate` / `#remoteKey` 的键格式在 §4.10 已逐字给出；补一条**易错点**：`#remoteKey` 的返回串带前导 `@` 且是四段 `` `@${type.name}\u0000${identity}\u0000${principalPart}\u0000${type.fingerprint}` ``（identity 用 `canonicalIdentity(type.input, normalized)` 重算，而不是复用 `#locate` 的 `normalize` 结果），而本地 address 是三段且**没有** `@` 前缀。两者共处同一个 `bindings` map，前缀是唯一的防碰撞手段。
- `HostImpl` 的 `lingerTypes`/`lingerCap` 来自 `options.linger?.types ?? {}` / `options.linger?.cap ?? 4`；`#lingerCap <= 0` 时 `bindingIdle` 直接 `binding.close()`（§2.17 已写）。
- `nextCodeVersionId` 的两处描述上文重复且其中一处消息插值写法不同（A.1 第 3 条已统一）。

**`src/object.ts`（§2.18）**
- **`ResourceFacade.value`**：`this.#host.inFactory > 0` → `notReady(\`resource ${this.key} (inside a factory)\`)`；否则 `this.#resource.value`。§2.18 已写 `resource ${key} (inside a factory)`，但**`ResourceFacade` 的 `facades: WeakMap<object, ResourceImpl>` 反查表与 `resourceOf` 的判定**需要显式保留（§2.18 已提及 `facades`）。
- `MemberReadable` 的 `bind()` 首事件抑制条件逐字是：`isFirst && this.#unseen.size === 0 && this.#seen !== undefined && event.value === this.#seen.value` → `return`（完全丢弃）；否则首事件归一为 `{type:"reset", value}`。§2.19 已写到 `MemberReadable`，但 **§2.18 中 `RefImpl` 一节的 `shapeChanged()` 会调用所有 readable 的 `bind()`**，而 `bind()` 里 `first` 的语义是"每次重新订阅后的第一个事件"（不是"进程内第一个事件"）—— Rust 移植时容易把 `first` 提到结构体上。
- `ObjectImpl.#activate` 第 6 步"对所有 `host.retainedCodeVersions()` 把本 run 的 implementation/decls 也登记到该版本"（§2.18 已写），补充：登记的是**同一个 `run.implementation` 对象**（共享引用，不是拷贝），因此旧版本消费者看到的是同一份实现。
- `SelfImpl` 的 `#decls`/`#created`/`#previous`/`#object` 是私有字段（未在 §2.18 展开）；`ObjectImpl.#background` 保存 background 任务的 controller。
- 导出的 `ObjectIdentity` / `StatusListener` 见 A.2。

**`src/remote.ts`（§2.19）**
- `#caller(name)` 的 `ctx?.check(host)` 抛错路径是 `try { ctx?.check(host) } catch (error) { return Promise.reject(error) }` —— **不是**让异常同步冒出（§2.19 只写"抛错 → reject"，需明确是 `Promise.reject`）。同理 `host.inFactory > 0` 是**同步 throw**（两者行为不同：一个同步、一个异步拒绝）。
- `RemoteBinding.#live` 是 `deferred()`：`promise.catch(noop)`（**避免未处理拒绝**）；`detached()` 会**换一个新 deferred**（§2.19 已写"新 deferred"）。
- `RemoteBinding.state(name, frame)` 的 `as === "ops"` 分支里 `target.decoder.decode(frame.ops ?? [])` 后 `applyImmutable(target.value, ops)` —— 即 ops 走 `applyImmutable`（**不可变应用**），旧值不被修改。§2.19 已写。
- `RemoteHost` 接口的 `root(fn, options?: { signal?: AbortSignal })` 与 `RemoteChannel.call(..., root: RootRef | undefined, signal)` 签名在 §2.19 已给。

**`src/reload.ts`（§2.20）**
- `Reload.load()` 的 `changed`/`removed` 计算：`changed` = `services` 中 `current.get(name) !== service` 的键；`removed` = `current.keys()` 中不在 `services` 里的键。`host.pending = { id: nextCodeVersionId(this.codeVersion), services, tables: new Map(), roots: new Map(), abort: new AbortController() }`（§2.20 只写"建候选 service 表、nextCodeVersionId"）。
- `prepareAll()` 是**循环到不动点**：每轮取 `changed.has(typeName) && !candidates.has(object) && object.service !== services.get(typeName) && (status === "live" || status === "activating")` 的对象，`await Promise.allSettled(todo.map(o => o.ready))`、`#alive()`、再对 `status === "live" && service 不同` 的逐个 `prepareObject` + `#alive()`；`todo.length === 0` 才返回。（§2.20 只写"第一遍/第二遍"。）
- `withDeadline(promise, what)` 的 timer 在 `finally` 里 `clearTimeout`；超时消息 `` `${what} within ${deadline} ms` ``，`reason: "deadline"`，code `aborted`（§2.20 已写）。
- `#stage()`：对 status 为 live 的 candidate `candidate.staged = object.stageListeners(candidate.run.decls)`；对 `host.objects` 中 `status === "live" && deferredRuntime` 的对象 `this.deferred.set(object, object.stageListeners(object.decls))`。（§2.20 只在管道里写了"11. `#stage()`"，未展开；这条对"新增类型的对象在 commit 前不得跑 listener"至关重要。）
- `summary()` 的 `codeVersion` 是 `host.pending?.id ?? host.current.id`（§2.20 已写）。
- `install()` 的 `gated` 分支里 detach 的是"`run.decls.derives` 未保留的旧 derivations"（`kept = new Set(run.decls.derives.values())`）—— §2.20 第 10.2 步已写。
- `Reload` 的 `#starts` 是私有字段（`readonly #starts: (() => void)[] = []`），不在"全部公开"的字段清单里 —— §2.20 的字段清单没列它，是对的。

**`src/cluster.ts`（§2.21）**
- `runClusterReload` 第 7 步"任一 rejected"的**顺序**是：先 `votes.delete(id)` + 发 `{t:"abort", reload:id}`（对**所有**参与者），若本地 `prepare` 成功才 `await reload.rollback()`，然后抛**第一个** rejected 的 reason（`Promise.allSettled` 的结果顺序 = `[local, ...votes]` 顺序）。
- `reconcileChild` 的 `vote` deadline 是硬编码 `30_000`（不是 `voteDeadline`）；`awaitVote` 的超时消息 `` `votes missing after ${deadline} ms` ``。
- `modulesFor(remote, hostName)` 的"最长前缀胜出"实现是遍历 `Object.entries` 时维护当前最长匹配（**不是**排序后取第一个）；返回 `[...list]` 拷贝。

**`src/manager.ts`（§2.22）**：§2.22 已逐条覆盖。补两条：
- `processLock` 的 `accepting(address)` 对非 `ECONNREFUSED`/`ENOENT` 的错误返回 `true`（保守认为被占用）—— 即 `EACCES` 等也算"有人在听"。
- `forkDriver` 的 `onOutput` 镜像写回父进程 `process[stream].write(chunk)` 时**不检查父进程对应流是否可写**（若父是 `stdio: "inherit"` 之外的场景可能抛错）—— 逐字保留即可。

**`src/output.ts`（§2.25）**
- `runtimeBuffer = (globalThis as { Buffer?: RuntimeBuffer }).Buffer`、`nonAsciiPattern = /[^\x00-\x7f]/` 是模块私有；`utf8ByteLength` 在有 `Buffer` 时直接 `Buffer.byteLength(content, "utf8")`（**孤立代理按 U+FFFD = 3 字节**），否则手算。§2.25 已写。
- `Progress` 的 `#minIntervalMs` / `#clock` 是私有字段（构造签名 `(write, onError, minIntervalMs, clock = SYSTEM_CLOCK)` 已在 §2.25 给出）。

**`src/telemetry-sqlite/*`（§2.34）**
- `Recorder` 的 scope 生成：`${this.scope}:${++this.sequence}`（`scope` 来自 `crypto.randomUUID()`）。§2.34 已写 `identities`/`carrierKey`/`traceKey`/`intervalMs`，未写这个 id 拼接模板 —— 它出现在 `StoredSpan.id`/`traceId` 上，属**外部可见格式**，需逐字。
- SQL 文本逐字：`"INSERT INTO telemetry_spans VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)"`、`"DELETE FROM telemetry_spans WHERE ended_ms < ?"`、`"DELETE FROM telemetry_metrics WHERE interval_ms < ?"`、`"SELECT * FROM telemetry_spans WHERE trace_id = ? ORDER BY started_ms, id"`。§2.34 只给了语义（"逐个 INSERT … DELETE FROM … WHERE ended_ms < cutoff"），没有给**这些字符串的逐字形态（含占位符之间的空格）**；§4.7 有 DDL 但没有 store 侧 SELECT/DELETE 原文。Rust 侧若要逐字节兼容日志/SQL 追踪，需照抄。
- `store.ts` 的属性过滤（`attributesSql`）逐字：`boolean` → `entry.type = '${value ? "true" : "false"}'`；`number` → `entry.type IN ('integer', 'real')`（逗号后一个空格）；其它 → `entry.type = 'text'`；多个条件 `" AND "` 连接；**无属性时整个表达式是字面量 `"1"`**。整条模板是 `` EXISTS (SELECT 1 FROM json_each(${column}) entry WHERE entry.key = ? AND ${type} AND entry.value = ?) ``，参数 `key` 与（boolean → `Number(value)`，否则原值）成对 push。**注意**：`attributesSql` 同时服务 `findTraces`（`column = "candidate.attributes"`）与 `findMetrics`（`column = "attributes"`）—— **只有一个写法**，不存在"两处空格不同"的分支（§4.7 的 store 小节曾误写成 `IN ('integer','real')`，已在此更正）。

**`src/sqlite/*`（§2.33）**
- `validateSqliteSchemaPrefix` 的两条消息：`` `Invalid SQLite schema prefix ${JSON.stringify(prefix)}` ``、`` `SQLite schema prefix ${JSON.stringify(prefix)} is reserved` ``（§2.33 已写）。
- `migrations.ts` 的 SQL：`"INSERT OR IGNORE INTO ${table} (singleton, version) VALUES (1, 0)"`、`"SELECT version FROM ${table} WHERE singleton = 1"`、`"UPDATE ${table} SET version = ? WHERE singleton = 1"`（与 §4.7 的 `<table>` 版本一致；`INSERT OR IGNORE INTO <table> (singleton, version) VALUES (1, 0)` 的**空格**在 §4.7 已逐字给出）。
- 私有类型 `SchemaRow`（`migrations.ts`）、`TransactionScope`（`node.ts`）未在上文出现（行为无关，但按"逐文件"要求列出）。

**`src/index.ts` / `src/node.ts`**
- `index.ts` 共 **27 个值 + 81 个类型 = 108 个导出名**（不是 189；189 是 9 个入口模块的并集）。§2.1 的逐项清单已复核**完整且无多余**（108 项全部命中，0 项缺失）。
- `node.ts` 16 行：`export { nodeLoader }` + 从 `manager.ts` re-export 11 个（`childControl`、`createHostManager`、`forkDriver`、`processLock` + 8 个类型）。§2.2/§3.6 已写。

### A.4 清单核对结论（只记录，不改）

1. **§3.6 的 189 / 74 / 115 复核为正确**：脚本枚举 9 个入口模块的 `export`（含 `export async function`、`export {…} from`、`export type {…} from`）得到 74 个值 + 115 个类型 = 189 个唯一名。`@earendil-works/chord/delta` 的 19 值 / 15 类型、`/context` 的 10 值、`/output` 的 7 值、`/sqlite/node` 的 3、`/sqlite/cloudflare` 的 2、`/telemetry-sqlite` 的 1 值 2 类型、`/sqlite` 的 2 值 4 类型，全部与源码一致。
2. **§7 的测试清单复核为正确**：`test/**/*.test.ts` 共 53 个文件（另有 13 个非测试支持文件），§7 的逐文件条目与 665 条字面标题一一对应（0 条遗漏、0 条多余）；682 个声明点 / 630 个运行时用例的换算一致。**逐项复核结果**（括号匹配式解析，而非逐行正则）：682 个调用点 = `describe` 111 + `describe.each` 1 + `it` 539 + `it.each` 9 + `test` 21 + `test.each` 1；扣除 1 个 `describe.each` 容器与 16 个含插值/`%s`/`%i`/`%,i` 的参数化标题 = **665** 条字面标题，与 §7 各文件小节「N 条」之和 665 完全吻合，且 665 条中**每条都能在其所属文件的 `###` 小节内找到**（0 条错位到别的小节），也没有任何字面标题被漏列。16 个参数化标题的确切构成（这是 §7 说"参数化点"时的口径）：**10 个 `.each` 站点**（`it.each` × 9 + `test.each` × 1）＋ **4 个 `for (const … of …)` 生成点**（`readiness.test.ts:24`、`:72`、`:193`，`telemetry-sqlite.test.ts:94` 那一行 `for (const test of createTelemetryAdapterConformance(fixture).filter(…))`）＋ **2 个纯模板标题**（`delta-pending.test.ts:554`、`watch.test.ts:226`，都插值 `MAX_PENDING_OPS`）。注意后 6 个（4 个 for 生成 + 2 个模板标题）在 §7 里都按"参数化"处理，`it.each` 只有 10 个而不是 16 个。
3. **§3.3 的文案清单复核为正确**：源码中所有 `ChordError`/`TypeError`/`Error`/`RangeError` 的模板字符串逐条比对，未发现 §3.3 缺失的用户可见消息（A.3 中补出的 SQL 文本不是 `Error` 消息）。§3.3 现为 142 个条目（含分组的子项），覆盖 9 个源文件族。
4. **§3.2 的 33 个 reason 字符串**、§3.1 的 12 个错误码与顺序、§3.5 的协议关闭原因，全部与源码逐字一致。
5. **§0.1 的 42 个源文件行数**全部复核一致（含 `sqlite/index.ts` 3 行、`delta/draft.ts` 10 行、`context/index.ts` 161 行）。

### A.5 内部（非导出）标识符对照表（补齐"逐文件"完整性，行为无缺口）

下列名称在上文任何地方都未出现。它们不是公共 API，但 Rust 移植时对应模块的私有结构体/函数，**供实现者核对结构划分是否漏项**：

| 文件 | 遗漏的内部标识符 |
|---|---|
| `connection.ts` | `StateFrame`、`Overflow`、`Slot`、`Feed`、`ServedLocal`、`Served`、`Pending`、`stateKey`、`unitKey` |
| `object.ts` | `RunningTask`、`Subscription`、`#background`（`ObjectImpl.#background(disposal)`，背景销毁任务的登记点） |
| `state.ts` | `Publication`、`StateDelivery`、`StateListener`、`isPromiseLike`、`isReplicatedStateSource`、`toError`（`watch.ts` 另有一份同名 `toError`） |
| `watch.ts` | `Delivery`、`WatchDelivery`、`WatchListener`、`NativeWatchListener` |
| `contract.ts` | `ContractBase`、`Empty`、`StateOf`、`StaticOf`、`MethodsOf`、`Returned`、`Resolved`、`VariantImplementation`、`VariantRef`、`LocalImplementationMember`、`LocalRefMember`、`LocalMembers`、`SchemaObject`、`MemberDescriptionSchema`、`PlacementSchema` |
| `link.ts` | `PayloadField`、`PAYLOAD_FIELDS`、`envelopeOf` |
| `output.ts` | `RuntimeBuffer`、`runtimeBuffer`、`nonAsciiPattern` |
| `schema.ts` | `asSchema`、`withoutAnnotations` |
| `host.ts` | `AnyServe`、私有 `#lingerTypes`/`#lingerCap`/`#originSeq`/`#principalKey`/`#retirementWatchers`/`#loader` |
| `delta/diff.ts` | `MatchCandidate` |
| `delta/index.ts` | `assertPathArg` |
| `delta/apply-immutable-trusted.ts`、`delta/revision-validator.ts` | `JsonContainer`（**两个文件各有一份同形定义**：`type JsonContainer = JsonValue[] \| Record<string, JsonValue>`；还各自有一份 `isContainer`）、`CopiedPath`（仅 `apply-immutable-trusted.ts`：`{ root: JsonValue; target: JsonContainer }`） |
| `delta/tracker.ts` | `Container`、`Primitive`、`Stored`、`arrayOverlay`、`baseOverridesForWrite`、`createNode`、`createPieceNode`、`deleteObjectWrite`、`flattenPieceTree`、`insertOverridesForWrite`、`leftmostPieceNode`、`locatedDenseArray`、`locatedDenseIndex`、`mergePieceTrees`、`nextPiecePriority`、`nodeParent`、`releaseStoredValue`、`replaceStoredValue`、`rightmostPieceNode`、`setObjectDeletion`、`sortTokenIndex`、`splitPieceTree`、`storeValue`、`storedValue`、`treeElements`、`updatePieceNode` |
| `json.ts` | `IsAny` |
| `sqlite/migrations.ts`、`sqlite/node.ts` | `SchemaRow`（`migrations.ts`，`type SchemaRow = { readonly version: number }`）、`TransactionScope`（`node.ts`，`type TransactionScope = { active: boolean }`） |

### A.6 Rust 映射的额外行为缺口（在 §8 之外）

1. **`Draft` 的深度上限 8**：`draft.ts` 的 `Depth["length"] extends 8 ? T` 是纯类型层，Rust 无对应物；但 §8.6 第 3 条只讨论"显式 Draft API"，未提"嵌套超过 8 层的写入在 TS 里仍是合法运行时行为（类型退化为 `T`）"。Rust 显式 API 必须允许任意深度，不能把 8 当成运行时限制。
2. **`Value.Clean` 与 `additionalProperties: false`**：`normalize` 依赖 `Clean` 删除未声明键；§8.6 第 12 条已列。补一条：`coverage`/`fits` 的 `objectFits` 对"sub 的 `additionalProperties` 是 `true`/`false`/缺失"三态有不同处理（§2.5 已写），Rust 的 `additionalProperties` 表示不能塌缩成 `Option<Schema>`。
3. **`AbortSignal.any` 的 reason 传播**：`withAbortSignal` 用 `AbortSignal.any([parent, signal])` —— 组合信号的 `reason` 是**第一个 abort 的那个**。`linkedSignal`（§2.18）自己实现了同样的语义但**额外用 `{once: true}` 并在触发后移除其余监听**，因此行为等价但资源语义不同（§6.4 已提 `linkedSignal` 不是 `AbortSignal.any`）。
4. **`process.getBuiltinModule` 探测顺序**：`link.ts` 里是 `(globalThis as { process?: { getBuiltinModule?: (id: string) => unknown } }).process?.getBuiltinModule?.("node:zlib")` —— 浏览器下 `process` 可能不存在，`?.` 链必须三层都可短路。§6.2 已提"探测"，但未强调**三层可选链**（Rust 侧若用 `cfg` 判断则不存在此问题，但跨实现的行为表格需要注明"无 zlib 时 `packObject` 永不压缩、`unpack` 抛 `packed payload without zlib`"）。
5. **`Math.random().toString(36).slice(2, 8)` 的 incarnation**（`host.ts`）与 `crypto.randomUUID()`（telemetry scope）都是**不可复现的随机数**；§8.6 第 9 条只提 UUID。Rust 侧若要 protocol 兼容需保留"6 字符 base36"的形状（`#incarnation` 参与 relay originTag 的生成）。

### A.7 审计方法（可复现）

- 导出面：Node 脚本对 `src/**/*.ts` 逐文件解析 `export` 声明与 `export {…} from` / `export type {…} from` 块，按 9 个入口模块归并去重 → 189/74/115；对 `index.ts` 单独归并 → 108/27/81。
- 文案与常量：正则提取源码中所有字符串字面量与模板字面量的**非插值片段**（先按行提取单/双引号字面量并过滤长度 <3、import 路径、URL、纯小写标识符后剩 644 处；另提取含插值的模板字面量 204 处，其中非插值片段能在 spec 中命中的 201 处，剩下 3 处是 `manager.ts` 的 Windows 命名管道双反斜杠转义、`schema.ts` 的 `canonicalJson` 拼接片段、`errors.ts` 的 `forbidden()` 示例，均非用户可见文案），逐一在 spec 文本中做子串查找；未命中的逐条人工分类为"JSDoc/测试标题/内部键"或"真实缺口"（A.3 中补出的即真实缺口）。
- 标识符：对每个文件提取 `const|let|function|class|interface|type|enum` 声明与类成员，按词边界在 spec 中查找；缺失项按"导出 / 内部 / 类私有"三类归档（A.2 / A.5）。
- 测试：枚举 `test/**/*` 全部 66 个文件，比对 §7 的文件名、逐文件条目、字面标题、参数化模板与 682/665/630 的换算。
- 所有脚本都在 `/tmp`（`/tmp/expcheck.mjs`、`/tmp/decl2.mjs`、`/tmp/strcheck.mjs`、`/tmp/tmpl.mjs`、`/tmp/allmods.mjs`、`/tmp/idx2.mjs`、`/tmp/members.mjs`、`/tmp/ximports.mjs`、`/tmp/tchk.mjs`、`/tmp/sub.mjs`），repo 只读。
