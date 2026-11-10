# `durable-rest` 移植规范（TypeScript → Rust）

本文件是 `@earendil-works/kiana-durable` 中 **durable-rest** 单元的完整移植规范，同时充当：

1. **验收清单**：Rust 实现必须逐条满足的行为契约；
2. **Rust 设计**：模块划分、类型、错误、并发模型与最难复刻之处的说明。

单元 id：`durable-rest`。
参考实现（只读）：`/home/ctyun/code/kiana_rev/kiana/packages/durable/`。
目标工作区：`/home/ctyun/code/kiana_rev/kiana`。

> 约定：文中所有标识符、字段名、错误文案、用户可见字符串均按 TypeScript 源码**逐字**抄录，Rust 实现必须输出完全一致的文本（除另有说明）。中文用于解释语义。凡标 **【逐字】** 的字符串，测试会逐字节比对。

---

## 0. 单元边界与阅读顺序

### 0.1 属于本单元的源文件（全部必须移植）

```
packages/durable/src/index.ts
packages/durable/src/types.ts
packages/durable/src/documents.ts
packages/durable/src/entries.ts
packages/durable/src/errors.ts
packages/durable/src/ids.ts
packages/durable/src/search.ts
packages/durable/src/tasks.ts
packages/durable/src/timers.ts
packages/durable/src/truncate.ts

packages/durable/src/storage/scan.ts
packages/durable/src/storage/memory.ts
packages/durable/src/storage/indexeddb.ts
packages/durable/src/storage/jsonl/index.ts
packages/durable/src/storage/jsonl/node.ts
packages/durable/src/storage/jsonl/storage.ts
packages/durable/src/storage/sqlite/index.ts
packages/durable/src/storage/sqlite/node.ts
packages/durable/src/storage/sqlite/cloudflare.ts
packages/durable/src/storage/sqlite/database.ts
packages/durable/src/storage/sqlite/migrations.ts
packages/durable/src/storage/sqlite/schema.ts
packages/durable/src/storage/sqlite/storage.ts

packages/durable/src/session/session.ts
packages/durable/src/session/transaction.ts
packages/durable/src/session/forks.ts

packages/durable/src/env/index.ts
packages/durable/src/env/decode.ts
packages/durable/src/env/line-scan.ts
packages/durable/src/env/node.ts
packages/durable/src/env/node-watch.ts

packages/durable/src/tools/index.ts
packages/durable/src/tools/env.ts
packages/durable/src/tools/path-utils.ts
packages/durable/src/tools/file-mutation-queue.ts
packages/durable/src/tools/image-processor.ts
packages/durable/src/tools/image.ts
packages/durable/src/tools/read.ts
packages/durable/src/tools/write.ts
packages/durable/src/tools/edit.ts
packages/durable/src/tools/edit-diff.ts
packages/durable/src/tools/bash.ts

packages/durable/src/images/index.ts
packages/durable/src/images/node.ts
packages/durable/src/images/node-worker.ts
packages/durable/src/images/exif.ts
packages/durable/src/images/cloudflare.ts
packages/durable/src/images/photon-wasm.d.ts     # 环境声明（无运行时；移植时作为 crate 的 wasm 绑定契约）

packages/durable/src/tool-search/index.ts
packages/durable/src/tool-search/tool-search.ts

packages/durable/src/codemode/index.ts
packages/durable/src/codemode/codemode.ts
packages/durable/src/codemode/describe.ts
packages/durable/src/codemode/models.ts
packages/durable/src/codemode/output.ts
```

文档：`packages/durable/README.md`（67,826 B ≈ 66.2 KiB，规范性用户文档）、`packages/durable/docs/spec.md`（303.9 KB，设计规范；重点 §1–§4、§7.3、§8.4–§8.5、§10、§11）、`packages/durable/CHANGELOG.md`（36.3 KB）、`packages/durable/package.json`、`packages/durable/vitest.config.ts`。

### 0.2 明确**不**属于本单元（由其他单元移植）

- `packages/durable/src/harness/**` — **23 个文件**：`agent.ts`、`compaction.ts`、`context.ts`、`define.ts`、`events.ts`、`generation.ts`、`harness.ts`、`inbox.ts`、`json.ts`、`live.ts`、`partial-changes.ts`、`prompt.ts`、`provider.ts`、`registry.ts`、`scheduler.ts`、`submissions.ts`、`task-graph.ts`、`telemetry.ts`、`tool.ts`、`types.ts`、`usage.ts`、`util.ts`、`view.ts`。本单元只依赖其中**少量类型**与 `define.ts` 的恒等构造函数（见 §5.4）。
- `packages/durable/src/testing/**` — **7 个文件**：`assertions.ts`、`env-conformance.ts`、`index.ts`、`runner.ts`、`storage-benchmark.ts`、`storage-conformance.ts`、`types.ts`。
- **不存在**其他顶层源文件：`src/` 顶层恰好是 10 个文件（`documents.ts`、`entries.ts`、`errors.ts`、`ids.ts`、`index.ts`、`search.ts`、`tasks.ts`、`timers.ts`、`truncate.ts`、`types.ts`），全部属于本单元。

但**注意**：`src/index.ts` 转出了大量 Harness 符号（见 §2.1）。Rust 侧需要保证 crate 的公共导出面与 `package.json` 的 exports 子路径一一对应（§5.6）。

### 0.3 建议阅读顺序

`errors.ts` → `ids.ts` → `types.ts` → `documents.ts` → `entries.ts` → `truncate.ts` → `search.ts` → `timers.ts` → `storage/scan.ts` → `storage/memory.ts` → `jsonl/storage.ts` → `sqlite/{schema,migrations,storage}.ts` → `indexeddb.ts` → `session/{transaction,session,forks}.ts` → `env/**` → `tools/**` → `images/**` → `tool-search/**` → `codemode/**`。

---

## 1. 概览

### 1.1 包的用途

`@earendil-works/kiana-durable`（v1.4.0，ESM）是 **持久化 agent harness 内核**。它建立在两个同级包之上：

- `@earendil-works/chord`：不可变 JSON 值（`JsonValue`/`JsonObject`）、JSON 拷贝（`copyJson`）、文档增量操作（`Op`、`applyImmutableBatches`、`PendingOps`、`track`/`Tracker`）、`Watch` 观察器、`Context`（含 `abortSignal`、`telemetry`）、`@earendil-works/chord/output` 的 `utf8ByteLength`/`characterEnd`/`OutputBuffer`、`@earendil-works/chord/sqlite` 的 SQLite 门面与迁移器。
- `@earendil-works/kiana-ai`：`Message`/`Usage`/`Models`/`Tool`/TypeBox schema 类型、`ImageContent`/`TextContent`。

本单元提供：

1. **存储层**（Storage 契约 + 四个后端：Memory、JSONL、SQLite、IndexedDB）；
2. **Session 内核**（单一变更线、原子提交、adopt 回调、文档缓存/保留/钉住、deferred commits、watches、只读模式、失败闩锁）；
3. **事务层**（`Tx`：表读写、文档草稿、fork 复制、提交结算、发布）；
4. **执行环境抽象**（`ExecutionEnv`：文件系统 + shell + 监视器；Node 实现；行扫描/流解码；watch 的 native/polling 双模式）；
5. **编码工具**（`read`/`write`/`edit`/`bash`/`powershell`，`coding-tools` 扩展）；
6. **图像处理**（格式探测、EXIF 方向、基于 Photon 的缩放/转码 `ImageProcessor`）；
7. **工具搜索**（`tool_search` 工具，BM25）；
8. **Code Mode**（`codemode` 工具：QuickJS 沙箱、确定性重放、journal、store、`models.*` 内部工具、输出格式化、系统提示段）。

### 1.2 与其他包/单元的关系

```
kiana-ai (Messages, Models, Tool, TypeBox)
      │
      ▼
chord (JsonValue, delta/Op, Tracker, Watch, Context, sqlite facade, output utils)
      │
      ▼
kiana-durable
 ├── harness/**        ← 其他单元（消费本单元的 Storage/Session/env/tools）
 ├── session/**        ← 本单元
 ├── storage/**        ← 本单元
 ├── env/**            ← 本单元
 ├── tools/**          ← 本单元
 ├── images/**         ← 本单元
 ├── tool-search/**    ← 本单元
 └── codemode/**       ← 本单元（依赖外部 @earendil-works/kiana-codemode）
```

本单元向 Harness 暴露的关键接口：`Storage`、`Session`、`createSession`、`Tx`、`Transaction`（内部）、`defineDoc`/`defineDocFamily`、`ExecutionEnv`/`NodeExecutionEnv`、`createCodingTools`、`createNodePhotonImages`、`createToolSearchTool`、`createCodemodeTool`。

### 1.3 入口点（package exports 子路径）

`package.json` 的 `exports` 映射（Rust 侧应以同等模块边界呈现）：

| 子路径 | TS 入口 | 内容 |
|---|---|---|
| `.` | `src/index.ts` | 全部公共 API（含 Harness 类型） |
| `./env` | `src/env/index.ts` | 可移植 `ExecutionEnv` 契约（不含 Node） |
| `./env/node` | `src/env/node.ts` | `NodeExecutionEnv`、`NodeWatchOptions` |
| `./tools` | `src/tools/index.ts` | 编码工具与 `createCodingTools` |
| `./images` | `src/images/index.ts` | `createPhotonImages`（WASM 由调用方提供） |
| `./images/node` | `src/images/node.ts` | `createNodePhotonImages` |
| `./images/cloudflare` | `src/images/cloudflare.ts` | Workers 版本（WASM 模块导入） |
| `./tool-search` | `src/tool-search/index.ts` | `tool_search` |
| `./codemode` | `src/codemode/index.ts` | `codemode` 工具/扩展 |
| `./storage/memory` | `src/storage/memory.ts` | `MemoryStorage` |
| `./storage/jsonl` | `src/storage/jsonl/index.ts` | 可移植 JSONL（不含 Node） |
| `./storage/jsonl/node` | `src/storage/jsonl/node.ts` | `openNodeJsonlStorage` |
| `./storage/sqlite` | `src/storage/sqlite/index.ts` | 可移植 SQLite 后端 + 迁移 |
| `./storage/sqlite/node` | `src/storage/sqlite/node.ts` | `openNodeSqliteStorage` |
| `./storage/sqlite/cloudflare` | `src/storage/sqlite/cloudflare.ts` | Durable Object 适配 |
| `./storage/indexeddb` | `src/storage/indexeddb.ts` | 浏览器 IndexedDB 后端 |
| `./testing` | `src/testing/index.ts` | **不在本单元**（`src/testing/**`，7 个文件） |
| `./package.json` | `./package.json` | 原样透出（**无** `source`/`types`/`import` 条件） |

**逐字核对**：`exports` 共 **18 个子路径 + `./package.json`**；每个非 `./package.json` 条目都有四个条件 `source` → `types` → `import` → `default`，且 `source` 指向 `./src/**`、其余三者指向 `./dist/**`。

包元数据（`package.json`，**原 spec 缺失**）：
```json
"name": "@earendil-works/kiana-durable",
"version": "1.4.0",
"description": "Durable conversation, task, and document runtime for Kiana",
"type": "module",
"main": "./dist/index.js",
"types": "./dist/index.d.ts",
"sideEffects": false,
"files": ["dist", "README.md", "CHANGELOG.md"],
"engines": { "node": ">=22.19.0" },
"keywords": ["durable-execution", "agents", "conversations", "tasks"],
"license": "MIT",
"author": "Earendil Works",
"repository": { "type": "git", "url": "git+https://github.com/earendil-works/kiana.git", "directory": "packages/durable" }
```
- `"type": "module"`（纯 ESM；所有相对导入都带 `.ts` 扩展名，构建后 `.js`）。
- `"sideEffects": false`——**直接导入任何子路径都不得产生副作用**；Rust crate 不应在 `lazy_static`/`ctor` 里做 I/O 或注册全局。
- `engines.node >= 22.19.0`——用到了 `Promise.withResolvers`、`os.constants.signals`、`fs.opendir`、`Array.prototype.at`、`structuredClone` 等；Rust 侧无对应约束，但**测试基线是 Node 22.19+**。
- `devDependencies`（**逐字版本**）：`@cloudflare/vitest-plugin 1.4.4`、`fake-indexeddb 6.2.6`（IndexedDB 后端测试用）、`shx 0.3.4`（`clean` 脚本）、`vitest 4.1.12`。
- `scripts`：`clean`（`shx rm -rf dist`）、`build`（`tsc -p tsconfig.build.json`）、**`test` = `vitest --run && KIANA_DURABLE_DOCUMENT_RETENTION_MS=0 vitest --run && npm run test:workers`（同一套测试跑两遍，第二遍把文档保留期设为 0 强制每次提交后逐出）**、`test:workers`（`vitest --run --config test-workers/vitest.config.ts`）、**10 个 `bench:*`**（`bench:storage` 用 `vitest bench --config vitest.benchmark.config.ts`；`bench:storage:memory` 直跑 `test/storage-memory.ts`；`bench:tool-output`/`bench:nested-tools`/`bench:nested-images` 带 `--expose-gc`；`bench:scheduler-fanout`/`bench:scheduler-memory`/`bench:document-cache`/`bench:document-append`/`bench:codemode` 直跑对应 `test/*-bench.ts`；除 `bench:storage` 外都是 `node --conditions=source --experimental-strip-types`）、`prepublishOnly`（`npm run clean && npm run build`）。
  - **更正**：原 spec 写成"9 个 `bench:*`"，实际是 **10 个键**（`bench:storage` 之外的九个子命令 + `bench:storage` 本身）；`bench:storage:memory` 跑的 `test/storage-memory.ts` 同时也是**共享 conformance 注册器**（不是纯基准文件）。
  - 另有两个配置文件：`vitest.benchmark.config.ts`（`bench:storage` 用）与 `tsconfig.build.json`（`build` 用）；前者不在 §7 的测试矩阵内。
  - **测试矩阵含义**：任何依赖"文档缓存命中"的断言必须在 retention = 0 时仍成立（缓存全部失效后从存储重新加载的路径同样是验收项）。Rust 移植必须提供等价开关。**环境变量 `KIANA_DURABLE_DOCUMENT_RETENTION_MS` 并不在 `src/` 中读取**——它只由测试夹具 `test/setup-document-retention.ts` 读取：`const forced = process.env.KIANA_DURABLE_DOCUMENT_RETENTION_MS; if (forced !== undefined) setDefaultDocumentRetention(Number(forced));`（vitest 每个测试文件都会加载它）。因此 Rust 侧只需暴露等价的 `setDefaultDocumentRetention` 测试钩子，**不要**在 crate 启动时读该变量（见 §3.7 的修正）。
- `dependencies` 与 §6.1 一致（7 个，无 `peerDependencies`/`optionalDependencies`）。

### 1.4 运行面特征（决定 Rust 设计）

- **单线程事件循环**语义：所有 Session 变更串行化在一条 promise 链（mutation line）上；不存在数据竞争。Rust 侧需要一个显式的串行执行器（见 §8）。
- **同步 adopt**：`Storage.commit(writes, adopt, context)` 必须在写入持久后、返回前**同步**调用 `adopt(seq)` 恰好一次；此契约是内存与磁盘一致性的核心。
- **错误即失败**：任何 Storage 调用抛错（除 `StorageRequestError` 与被取消的读）都会让 Session 进入不可恢复失败态。
- **时间与超时**：`Date.now()` 毫秒；计时器由宿主注入（`SetTimer`），Rust 侧对应 `tokio::time`。
- **文本语义**：JS 字符串是 UTF-16；大量逻辑按 `string.length`、`indexOf`、`slice` 计算偏移。Rust 必须明确选择字节/字符/UTF-16 表示（§8.3）。

---

## 2. 逐文件规范

### 2.1 `src/index.ts`（216 行）— 公共导出面（**逐字**）

源码就是 26 个 `export` 语句，**顺序即源码顺序**。Rust 侧 `kiana_durable` 的 crate root 必须一一对应。

```ts
export type { WatchEnd, WatchHandle } from "@earendil-works/chord";
export { defineDoc, defineDocFamily } from "./documents.ts";
export { AssistantEntry, CompactionEntry, defineEntry, ResetEntry, SystemEntry, ToolResultEntry, UserEntry } from "./entries.ts";
export { ConversationBusy, ReadAfterWrite, ReadOnlySession, SessionFailed, StorageRequestError } from "./errors.ts";
export { AgentDoc, configure, DEFAULT_COMPACTION_POLICY, DEFAULT_PROGRESS_POLICY, DEFAULT_RESTART_LIMIT, DEFAULT_RETRY_POLICY } from "./harness/agent.ts";          // 外单元
export { type CompactionCheckpoint, type CompactionInput, CompactionTask, type SummaryRequest } from "./harness/compaction.ts";                                      // 外单元
export { defineExtension, defineTool, hook, section, wrapSection, wrapTool } from "./harness/define.ts";                                                             // 外单元
export { type AgentEvent, type AgentEventStream, type MessageChange, type SnapshotEvent, type ToolEventCall, watchEvents } from "./harness/events.ts";               // 外单元
export { type GenerationCheckpoint, type GenerationInput, type GenerationResult, GenerationTask } from "./harness/generation.ts";                                    // 外单元
export { DEFAULT_HARNESS_IDENTITY, Harness } from "./harness/harness.ts";                                                                                            // 外单元
export { InboxDoc, type InboxItem, type InboxState } from "./harness/inbox.ts";                                                                                      // 外单元
export { type CompactionFailure, type CompactionStatus, LiveDoc, type LiveState, type NestedToolSlot, type NestedToolSummary, type ToolSlot } from "./harness/live.ts"; // 外单元
export { ProviderDoc, type ProviderState } from "./harness/provider.ts";                                                                                             // 外单元
export { createRegistry } from "./harness/registry.ts";                                                                                                              // 外单元
export type { BlockedReason } from "./harness/scheduler.ts";                                                                                                         // 外单元
export type { TaskGraph, TaskGraphNode, TaskGraphState, TaskGraphWatch } from "./harness/task-graph.ts";                                                             // 外单元
export { DURABLE_TELEMETRY_SCHEMA } from "./harness/telemetry.ts";                                                                                                   // 外单元
export { NestedCallDoc, type NestedCallState, ToolTask, type ToolTaskCheckpoint, type ToolTaskInput, type ToolTaskResult } from "./harness/tool.ts";                 // 外单元
export type { /* 57 个 Harness 类型 */ } from "./harness/types.ts";                                                                                                  // 外单元
export { UsageDoc, type UsageState } from "./harness/usage.ts";                                                                                                      // 外单元
export type { ConversationView } from "./harness/view.ts";                                                                                                           // 外单元
export { createSession } from "./session/session.ts";
export { MemoryStorage } from "./storage/memory.ts";
export { defineTask } from "./tasks.ts";
export type { /* 见下，全部来自 types.ts */ } from "./types.ts";
export { ROOT_CONVERSATION_ID } from "./types.ts";
```

**type-only 导出清单（`types.ts`，逐字，共 84 个）**：
`CheckpointInfo`, `CommitChange`, `CommitOptions`, `CommitPersistence`, `CommitPublication`, `CommonDocDefinition`, `ContextEdit`, `ConversationDocFamilyToken`, `ConversationDocToken`, `ConversationId`, `ConversationOwnership`, `ConversationQuery`, `ConversationRecord`, `Cursor`, `DocDefinition`, `DocFamilyDefinition`, `DocFamilyToken`, `DocToken`, `DocumentAddress`, `DocumentCommitChange`, `DocumentContent`, `DocumentCopySource`, `DocumentCreate`, `DocumentId`, `DocumentObserver`, `DocumentPoint`, `DocumentQuery`, `DocumentReader`, `DocumentRecord`, `DocumentSemantics`, `DocumentWatch`, `Entry`, `EntryDraft`, `EntryId`, `EntryQuery`, `EntryRecord`, `HookRunner`, `HostCallback`, `Id`, `JoinPolicy`, `JsonObject`, `LatestConversationSemantics`, `NextTaskState`, `Page`, `PhaseHandler`, `ReportFrom`, `ReportInfo`, `ReportTask`, `RewindableConversationDocFamilyToken`, `RewindableConversationDocToken`, `RewindableConversationSemantics`, `RunningTask`, `ScanOrder`, `Seq`, `Session`, `SessionDocFamilyToken`, `SessionDocToken`, `SessionEnd`, `SetTimer`, `Storage`, `StorageWrite`, `StoredDocument`, `SubmissionCreate`, `SubmissionId`, `SubmissionQuery`, `SubmissionRecord`, `SubmissionSettlement`, `TableCommitChange`, `Task`, `TaskDefinition`, `TaskDocFamilyToken`, `TaskDocToken`, `TaskId`, `TaskOptions`, `TaskOutcome`, `TaskOutcomeError`, `TaskOwnership`, `TaskQuery`, `TaskRecord`, `TaskRuntime`, `TaskState`, `Tx`, `TypedEntry`, `TypedEntryDraft`。

**value 导出（本单元贡献的部分）**：`defineDoc`、`defineDocFamily`、7 个 entry 常量 + `defineEntry`、5 个错误类、`createSession`、`MemoryStorage`、`defineTask`、`ROOT_CONVERSATION_ID`。
**value 导出（来自 chord）**：仅 `WatchEnd`、`WatchHandle` 两个**类型**。

---

### 2.2 `src/ids.ts`（11 行）

```ts
export function idFromNumber<I extends Id<string>>(value: number): I   // 纯 brand 断言
export function seqFromNumber(value: number): Seq                      // 纯 brand 断言
```

行为：仅在"可信的数字分配/解码边界"把裸 `number` 标记为 `Id`/`Seq`。无运行时校验。
**Rust**：`fn id_from_number<I: IdKind>(value: i64) -> Id<I>` 与 `fn seq_from_number(value: i64) -> Seq`，零成本 newtype 构造。

---

### 2.3 `src/errors.ts`（68 行）

#### `errorMessage(error: unknown): string`
- `error instanceof Error` → `error.message`；
- 否则 `String(error)`，若 `String()` 抛错（例如 `Object.create(null)`）→ `Object.prototype.toString.call(error)`。
- **绝不自身抛错**：格式化错误的代码不得因被格式化值而失败。

#### `class ReadAfterWrite extends Error`
构造签名 `(method: string)`；`name = "ReadAfterWrite"`；
消息 **【逐字】**：`` `Tx.${method}() cannot read tables after the first table write` ``。
语义：事务在第一次表写之后又读表。调用方必须在写之前读完所需行。

#### `class StorageRequestError extends Error`
`(message: string)`；`name = "StorageRequestError"`。
语义：Storage 读拒绝了非法请求（未知会话、来自其他扫描的 cursor 等），**无持久副作用**。与其他 Storage 抛错不同，它只让该次读失败，不判死 Session；但由 `commit()` 抛出时与其他错误一样致命。

#### `class SessionFailed extends Error`
`(cause: unknown)`；消息 **【逐字】**：`"Session failed after a storage error; close and reopen it"`；`cause` 为第一个错误；`name = "SessionFailed"`。
语义：Session 已失败，此后不能运行或提交。触发条件：Storage 方法抛错、提交无法 adopt、Harness 自身簿记抛错（scheduler 提交、内部提交监听器、`snapshot()` 抛错的 registry）。重新打开 Session 可从已提交状态恢复。

#### `class ReadOnlySession extends Error`
`(what: string)`；消息 **【逐字】**：`` `Read-only Harness: ${what} refused` ``；`name = "ReadOnlySession"`。
用法：`new ReadOnlySession("a commit with writes")` → `"Read-only Harness: a commit with writes refused"`。另有 `what = "running tasks"`（Harness 侧）。

#### `class ConversationBusy extends Error`
字段 `readonly conversationId: ConversationId`；消息 **【逐字】**：`` `Conversation ${conversationId} is busy` ``；`name = "ConversationBusy"`。
语义：提交到达忙碌会话，未被接纳。

---

### 2.4 `src/types.ts`（1257 行）— 核心类型

以下按源码顺序给出**所有**导出类型/常量及其语义。Rust 侧以 newtype/struct/enum 表达，字段名保持 `snake_case` 映射到 JSON 的输出必须保持 `camelCase`（见 §8.4）。

#### 值类型
```ts
export type JsonObject = { [key: string]: JsonValue };
```
`JsonValue` 来自 chord（null | boolean | number | string | array | object）。

#### ID 品牌
```ts
export type Id<Kind extends string, Type = unknown> = number & { readonly [brand]: Kind; readonly [type]: Type };
export type ConversationId = Id<"conversation">;
export type EntryId        = Id<"entry">;
export type TaskId<Result = unknown> = Id<"task", Result>;
export type SubmissionId   = Id<"submission">;
export type DocumentId     = Id<"document">;
```
- ID 是**正整数**，来自 Session 全局单一数字命名空间；**唯一性跨表**（`record_ids`/`recordTypes` 保证）。
- 顺序语义：**按 ID 升序 = 创建顺序**（Harness 依赖）。
- `TaskId<Result>` 携带结果类型（纯类型层）。
- `export const ROOT_CONVERSATION_ID = 1 as ConversationId;` — 根会话固定 ID 1；`next_id` 初值 2。

#### 提交序号
```ts
export type Seq = number & { readonly [seqBrand]: "sequence" };
```
正整数、严格递增（每个成功提交 +1）；`MemoryStorage` 的 `nextSeq` 初值 1。

#### 文档语义（逐字，types.ts L40-64）
```ts
/** Conversation document that retains only its current state. */
export type LatestConversationSemantics = {
	readonly scope: "conversation";
	readonly history: "latest";
	readonly fork: "current" | "initial";
};

/** Conversation document whose history remains addressable for as-of reads. */
export type RewindableConversationSemantics = {
	readonly scope: "conversation";
	readonly history: "rewindable";
	readonly fork: "asOf" | "current" | "initial";
};

/** Ownership and lifetime of a document; only conversation documents declare history and fork behavior. */
export type DocumentSemantics =
	| { readonly scope: "session"; readonly history?: never; readonly fork?: never }
	| LatestConversationSemantics
	| RewindableConversationSemantics
	| { readonly scope: "task"; readonly history?: never; readonly fork?: never };
```
**关键**：
- `history: "latest"` 只允许 `fork: "current" | "initial"`（**不接受 `"asOf"`**）；`history: "rewindable"` 才允许 `"asOf"`。
- session/task 文档**不得**声明 `history`/`fork`（`?: never`）。
- 语义差异：`fork: "current"` 从源当前状态初始化；`fork: "initial"` 用定义的 `initial()`；`fork: "asOf"` 在 fork 点（`ConversationRecord.parent.at`）的快照初始化——只有 rewindable 文档能这样做。

#### `CheckpointInfo`（逐字，只有这一个字段）
```ts
/** Stored replay state supplied to a document's checkpoint predicate. */
export type CheckpointInfo = {
	/** Deltas already stored after the newest base, excluding the change being evaluated. */
	readonly deltasSinceBase: number;
};
```
> 实现细节：`Transaction.planDocument()` 调用 `definition.checkpointWhen(value, ops, info)`；`this` 绑定为 definition 本身（`session-checkpoints-migrations.test.ts`："calls checkpointWhen with its definition as the receiver"）。**没有 `version` 字段**——Rust 侧不要多传。

#### 定义与 token
```ts
export type CommonDocDefinition<T extends JsonObject> = {
  readonly kind: string;
  readonly version: number;                 // 正整数
  initial(): T;                             // 单例：无参；family 用 Omit 去掉后换成 initial(seed: I): T
  migrate?(value: JsonObject, fromVersion: number): T;   // 注意返回 T，不是 JsonObject
  readonly checkpointWhen: (value: Readonly<T>, ops: readonly Op[], info: CheckpointInfo) => boolean;
};
export type DocDefinition<T>          = CommonDocDefinition<T> & DocumentSemantics;   // 单例；family 另见 DocFamilyDefinition
// **逐字**：family 用 Omit 去掉 initial，再加回带 seed 的版本
export type DocFamilyDefinition<T, I> = Omit<CommonDocDefinition<T>, "initial"> &
  DocumentSemantics & { readonly family: true; initial(seed: I): T };
export interface DocToken<T, D extends DocDefinition<T>> {
  readonly definition: D;
  readonly [docType]?: T;                   // declare const docType: unique symbol
}
export interface DocFamilyToken<T, I, D extends DocFamilyDefinition<T, I>> {
  readonly definition: D;
  readonly [docType]?: T;
}
```
**修正点**：(1) `common` 的 `initial` 是**无参**（原 spec 写成 `initial(seed?)`）；`migrate` 返回 `T`（原 spec 写成 `JsonObject`）。(2) `DocFamilyDefinition` 不是 `CommonDocDefinition & ...`，而是 **`Omit<CommonDocDefinition<T>, "initial"> & DocumentSemantics & { readonly family: true; initial(seed: I): T }`**——两者在类型层不等价（`initial` 签名冲突会被 TS 拒绝）。(3) 两个 token 接口都带**唯一的 `docType` 可选品牌**（`declare const docType: unique symbol`），Rust 侧用泛型 `PhantomData<T>` 对应；**该品牌不参与运行时**。

`DocumentSemantics` 全量逐字（`types.ts`）：
```ts
export type LatestConversationSemantics = {
  readonly scope: "conversation";
  readonly history: "latest";
  readonly fork: "current" | "initial";
};
export type RewindableConversationSemantics = {
  readonly scope: "conversation";
  readonly history: "rewindable";
  readonly fork: "asOf" | "current" | "initial";
};
export type DocumentSemantics =
  | { readonly scope: "session"; readonly history?: never; readonly fork?: never }
  | LatestConversationSemantics
  | RewindableConversationSemantics
  | { readonly scope: "task"; readonly history?: never; readonly fork?: never };
```
别名：`SessionDocToken`、`ConversationDocToken`、`RewindableConversationDocToken`、`TaskDocToken`、`SessionDocFamilyToken`、`ConversationDocFamilyToken`、`RewindableConversationDocFamilyToken`。

#### 报告/计时器（**Harness 单元负责精确定义**；本单元只用到 `Tx.report`/`Tx.setTimer` 的签名，逐字定义见 `harness/types.ts` L138-206）

```ts
export type ReportTask = {
	readonly id: TaskId;
	/** The task's registered name. */
	readonly kind: string;
	readonly conversationId: ConversationId;
};

/** A host callback, by its `HarnessOptions` name; `env` is the environment builder, `models` a provider call. */
export type HostCallback = "now" | "settings" | "registry" | "env" | "models" | "setTimer" | "onWake";

export type ReportInfo =
	| { readonly type: "session_failed" }
	| { readonly type: "task_faulted"; readonly task: ReportTask; readonly extension?: string; readonly reason: "threw" | "restart_limit" }
	| { readonly type: "task_parked"; readonly task: ReportTask; readonly extension?: string }
	| { readonly type: "migration_failed"; readonly task: ReportTask; readonly extension?: string }
	| { readonly type: "task_outdated"; readonly task: ReportTask; readonly extension?: string; readonly reason: "missing_task" | "incompatible_task" }
	| { readonly type: "task_error"; readonly task: ReportTask; readonly extension?: string }
	| { readonly type: "extension_error"; readonly extension?: string; readonly task?: ReportTask; readonly conversationId?: ConversationId }
	| { readonly type: "host_error"; readonly callback: HostCallback; readonly task?: ReportTask }
	| { readonly type: "listener_error" };

export type ReportFrom =
	| { readonly type: "extension"; readonly extension?: string }
	| { readonly type: "host"; readonly callback: HostCallback };

export type SetTimer = (callback: () => void, delayMs: number, options: { readonly background: boolean }) => (() => void) | undefined;
```

#### 任务运行时（Harness 用；本单元 `Tx.createTask` 需与其类型对接）

```ts
/** Live task record reserved by one invocation. */
export type RunningTask<I, S, R> = TaskRecord<I, S, R> & {
	readonly state: Extract<TaskState<S, R>, { readonly status: "running" }>;
};

export type NextTaskState<S, R> = Extract<TaskState<S, R>, { readonly status: "running" | "waiting" | "sleeping" | "terminal" }>;

export type PhaseHandler<I, P, S, R, H extends object> = (
	task: RunningTask<I, P, R>, runtime: TaskRuntime<I, S, R, H>, context: Context,
) => Promise<void>;

export interface HookRunner<H extends object> {
	each<K extends keyof H>(name: K, invoke: (handler: NonNullable<H[K]>) => void | Promise<void>): Promise<void>;
}

export interface TaskRuntime<I, S, R, H extends object> extends DocumentObserver, DocumentReader {
	readonly taskId: TaskId<R>;
	readonly conversationId: ConversationId;
	readonly signal: AbortSignal;
	readonly registry: RegistrySnapshot;
	agent(context: Context): Promise<Agent>;
	readonly settings: Settings;
	readonly models: Models;
	env(context: Context): Promise<ExecutionEnv | undefined>;
	readonly hooks: HookRunner<H>;
	commit(change: (tx: Tx, current: RunningTask<I, S, R>) => NextTaskState<S, R> | undefined | Promise<NextTaskState<S, R> | undefined>, context: Context, options?: CommitOptions): Promise<void>;
	memo<T extends JsonValue>(name: string, context: Context): Promise<T | undefined>;
	memo<T extends JsonValue>(name: string, candidate: T, context: Context): Promise<T>;
	getTask<T>(id: TaskId<T>, context: Context): Promise<TaskRecord<JsonValue, JsonValue, T> | undefined>;
	waitForTask<T>(id: TaskId<T>, context: Context): Promise<SettledTask<T>>;
	abortOwned(id: TaskId, context: Context): Promise<void>;
	ownedTasks(context: Context): Promise<TaskRecord<JsonValue, JsonValue, JsonValue>[]>;
	outcomes<T>(ids: readonly TaskId<T>[], context: Context): Promise<TaskOutcome<T>[]>;
	conversation(id: ConversationId, context: Context): Promise<ConversationHandle | undefined>;
	entry(id: EntryId, context: Context): Promise<EntryRecord | undefined>;
	entry<D extends JsonValue>(token: Entry<D>, id: EntryId, context: Context): Promise<TypedEntry<D> | undefined>;
	context(conversationId: ConversationId, context: Context, options?: { readonly at?: EntryId }): Promise<ContextView>;
	now(): number;
	setTimer(callback: () => void, delayMs: number): () => void;
	report(error: unknown, from?: ReportFrom): void;
	sleepUntil(until: number, checkpoint: S, context: Context): Promise<void>;
}

export interface Task<I, S extends { phase: string }, R, H extends object> {
	readonly definition: TaskDefinition<I, S, R, H>;
}

export type TaskDefinition<I, S extends { phase: string }, R, H extends object> = {
	readonly name: string;
	readonly version: number;
	initial(input: I): S;
	readonly phases: { readonly [P in S["phase"]]: PhaseHandler<I, Extract<S, { phase: P }>, S, R, H> };
	abort(task: RunningTask<I, S, R>, runtime: TaskRuntime<I, S, R, H>, context: Context): Promise<void>;
	migrate?(input: JsonValue, checkpoint: JsonValue, fromVersion: number): { input: I; checkpoint: S };
	readonly hooks?: H;
};

export type TaskOwnership = { readonly kind: "conversation" } | { readonly kind: "task"; readonly taskId: TaskId };
export type JoinPolicy = "failFast" | "allSettled";

export type TaskOptions = {
	readonly trace?: TelemetryCarrier | "root";
	readonly links?: readonly TelemetryCarrier[];
	readonly ownership: TaskOwnership;              // 必填
	readonly conversationId?: ConversationId;
	readonly background?: boolean;
	readonly abandonOnRestart?: boolean;
};
```

#### 记录类型（**逐字**，types.ts L396-905）

这些类型决定 JSONL/SQLite 的列与 JSON 字段名——**字段名即协议**，不可改名/省略可选字段语义。

```ts
export type ConversationOwnership = { readonly kind: "ownerless" } | { readonly kind: "task"; readonly taskId: TaskId };

/** Immutable identity, history ancestry, and task ownership of a transcript scope. */
export type ConversationRecord = {
	readonly id: ConversationId;
	/** Fork source and inclusive parent entry through which history is inherited. */
	readonly parent?: { readonly conversationId: ConversationId; readonly at: EntryId };
	/** Creator edge used for attribution, subtree abort, and subtree idle waits. */
	readonly owner?: { readonly conversationId: ConversationId; readonly taskId: TaskId };
};

export type ContextEdit = { readonly target: EntryId } & (
	| { readonly action: "omit"; readonly messages?: never }
	| { readonly action: "replace"; readonly messages: readonly Message[] }
);

/** Immutable transcript event with separate model-facing and application-facing payloads. */
export type EntryRecord = {
	readonly id: EntryId;
	readonly conversationId: ConversationId;
	/** Application-defined entry discriminator. */
	readonly kind: string;
	/** Messages contributed to model context; absent for display or bookkeeping entries. */
	readonly model?: readonly Message[];
	/** JSON payload consumed by views, extensions, or bookkeeping logic. */
	readonly data?: JsonValue;
	/** First entry in the active context selected by this entry. */
	readonly head?: EntryId;
	/** Context-only overrides of earlier visible entries. */
	readonly edits?: readonly ContextEdit[];
	/** Task that appended this entry, when it was produced by durable work. */
	readonly byTaskId?: TaskId;
};

export type EntryDraft = Omit<EntryRecord, "id" | "conversationId" | "byTaskId" | "head"> & {
	/** `"self"` starts active context at the newly assigned entry ID. */
	readonly head?: EntryId | "self";
};

export type TypedEntry<D extends JsonValue> = Omit<EntryRecord, "data"> &
	([D] extends [never] ? { readonly data?: never } : { readonly data: D });
export type TypedEntryDraft<D extends JsonValue> = Omit<EntryDraft, "kind" | "data"> &
	([D] extends [never] ? { readonly data?: never } : { readonly data: D });

export interface Entry<D extends JsonValue = never> {
	readonly kind: string;
	is(entry: EntryRecord | undefined): entry is TypedEntry<D>;
}
```
**关键**：`EntryRecord.head` 是 `EntryId`（无 `"self"`）；只有 `EntryDraft.head` 允许 `"self"`，由 Session 在写入时解析为真实 ID。`Entry` 是**带 `is()` 守卫的接口**，不是联合类型。

```ts
type SubmissionRecordBase = {
	readonly trace?: TelemetryCarrier;
	readonly id: SubmissionId;
	readonly conversationId: ConversationId;
	/** Host-provided deduplication key, scoped to the conversation. */
	readonly requestId?: string;
};

export type SubmissionRecord = SubmissionRecordBase & (
	| { readonly type: "input" } & (
			| { readonly status: "queued" }
			| { readonly status: "placed"; readonly entry: EntryId }
			| { readonly status: "done"; readonly entry: EntryId; readonly answer: EntryId }
			| { readonly status: "unanswered"; readonly entry?: EntryId; readonly reason: string; readonly detail?: JsonValue }
	  )
	| { readonly type: "write" } & (
			| { readonly status: "queued" }
			| { readonly status: "done"; readonly entry: EntryId }
			| { readonly status: "unanswered"; readonly reason: string; readonly detail?: JsonValue }
	  )
);
```
（源码以 `readonly entry?: never` 等互斥字段显式禁止非法组合；Rust 侧用 enum 天然表达，但**序列化后不得出现这些字段**。）
- `input`：`queued` 无 entry；`placed` 有 `entry` 无 `answer`；`done` 有 `entry` + `answer`；`unanswered` 有 `reason`（必需）、可选 `entry`、可选 `detail`。
- `write`：只有 `queued` / `done` / `unanswered`；`unanswered` **没有** `entry`。

```ts
export type SubmissionSettlement =
	| { readonly status: "done"; readonly answer: EntryId }
	| { readonly status: "unanswered"; readonly reason: string; readonly detail?: JsonValue };
export type SubmissionCreate = SubmissionRecord extends infer Record
	? Record extends SubmissionRecord ? Omit<Record, "id"> : never : never;

export type TaskOutcomeError = { readonly message: string; readonly detail?: JsonValue };

export type TaskOutcome<R> =
	| { readonly status: "completed"; readonly result: R }
	| { readonly status: "failed"; readonly error: TaskOutcomeError; readonly result?: R }
	| { readonly status: "aborted"; readonly reason?: string; readonly result?: R }
	| { readonly status: "orphaned"; readonly reason: string }
	| { readonly status: "faulted"; readonly error: TaskOutcomeError };
```

```ts
export type TaskState<S, R> =
	| { readonly status: "pending";   readonly checkpoint: S }
	| { readonly status: "running";   readonly checkpoint: S }
	| { readonly status: "waiting";   readonly checkpoint: S; readonly on: readonly TaskId[]; readonly policy: JoinPolicy }
	| { readonly status: "sleeping";  readonly checkpoint: S; readonly until: number }
	| { readonly status: "completing"; readonly outcome: TaskOutcome<R> }
	| { readonly status: "terminal";   readonly outcome: TaskOutcome<R> };

type TaskRecordBase<I, R> = {
	readonly trace?: TelemetryCarrier;
	readonly links?: readonly TelemetryCarrier[];
	readonly id: TaskId<R>;
	readonly conversationId: ConversationId;
	readonly kind: string;
	readonly version: number;
	readonly input: I;
	readonly owner?: TaskId;
	readonly background: boolean;
	readonly abortRequested: boolean;
	readonly abortReason?: "restart";
	readonly abandonOnRestart?: true;
	readonly startedAt?: number;
	readonly endedAt?: number;
	readonly restarts?: number;
};

export type TaskRecord<I, S, R> = TaskRecordBase<I, R> &
	(
		| { readonly state: Extract<TaskState<S,R>, { status: "pending"|"running"|"waiting"|"sleeping" }>;
		    readonly memos?: Readonly<Record<string, JsonValue>> }
		| { readonly state: Extract<TaskState<S,R>, { status: "completing"|"terminal" }>; readonly memos?: never }
	);
```

```ts
export type DocumentRecord = {
	readonly id: DocumentId;
	readonly kind: string;
	readonly key?: string;
	readonly createdAt: Seq;
	readonly retiredAt?: Seq;
} & (
	| { readonly scope: { readonly kind: "session" }; readonly history?: never; readonly fork?: never }
	| ({ readonly scope: { readonly kind: "conversation"; readonly conversationId: ConversationId } } & (
			| { readonly history: "latest";     readonly fork: "current" | "initial" }
			| { readonly history: "rewindable"; readonly fork: "asOf" | "current" | "initial" }
	  ))
	| { readonly scope: { readonly kind: "task"; readonly taskId: TaskId }; readonly history?: never; readonly fork?: never }
);

/** Fields supplied when storage creates and stamps a new `DocumentRecord`. */
export type DocumentCreate = DocumentRecord extends infer Record
	? Record extends DocumentRecord ? Omit<Record, "createdAt" | "retiredAt"> : never : never;

export type Page<T, C> = { readonly items: readonly T[]; readonly next?: C };
export type Cursor = Readonly<Record<string, JsonValue>>;
export type ScanOrder = "ascending" | "descending";

export type ConversationQuery = {
	readonly ownerConversationId?: ConversationId;
	readonly ownerTaskId?: TaskId;
	/** Default `ascending`; with a cursor, the cursor's order. */
	readonly order?: ScanOrder;
};

export type EntryQuery = {
	readonly conversationId: ConversationId;
	readonly minEntryId?: EntryId;
	readonly maxEntryId?: EntryId;
	/** Default `descending`. */
	readonly order?: ScanOrder;
};

export type TaskQuery = {
	readonly conversationId?: ConversationId;
	readonly kind?: string;
	readonly status?: TaskState<JsonValue, JsonValue>["status"];
	readonly abortRequested?: boolean;
	readonly background?: boolean;
	/** Default `ascending`. */
	readonly order?: ScanOrder;
};

export type SubmissionQuery = {
	readonly conversationId?: ConversationId;
	readonly status?: SubmissionRecord["status"];
	/** Default `ascending`. */
	readonly order?: ScanOrder;
};

export type DocumentPoint = Seq | "current";

export type DocumentAddress = {
	readonly kind: string;
	readonly scope: DocumentRecord["scope"];
	readonly key?: string;
};

export type DocumentQuery = {
	readonly scope: DocumentRecord["scope"];
	readonly at: DocumentPoint;
	readonly kind?: string;
};

export type DocumentContent =
	| { readonly version: number; readonly kind: "base"; readonly value: JsonObject }
	| { readonly version: number; readonly kind: "delta"; readonly ops: readonly Op[] };

export type DocumentCopySource = { readonly id: DocumentId; readonly at: DocumentPoint };

export type StoredDocument = {
	readonly record: DocumentRecord;
	readonly version: number;
	readonly value: JsonObject;
	readonly deltasSinceBase: number;
};

export type StorageWrite =
	| { readonly type: "conversation"; readonly value: ConversationRecord }
	| { readonly type: "entry"; readonly value: EntryRecord }
	| { readonly type: "task"; readonly value: TaskRecord<JsonValue, JsonValue, JsonValue> }
	| { readonly type: "submission"; readonly value: SubmissionRecord }
	| { readonly type: "document.create"; readonly record: DocumentCreate;
	    readonly content: Extract<DocumentContent, { readonly kind: "base" }> }   // 创建只允许 base
	| { readonly type: "document.copy"; readonly record: DocumentCreate; readonly source: DocumentCopySource }
	| { readonly type: "document.change"; readonly id: DocumentId; readonly content: DocumentContent }
	| { readonly type: "document.retire"; readonly id: DocumentId };

export type DocumentCommitChange =
	| { readonly type: "document";
	    readonly record: DocumentRecord;
	    /** 拥有该文档的会话；task 文档从其 task 记录推导，仅 Session 文档为 undefined。 */
	    readonly conversationId: ConversationId | undefined;
	    /** 退休时为 undefined。 */
	    readonly version: number | undefined;
	    /** 退休时为 null。 */
	    readonly value: JsonObject | null;
	    /** 普通更新时为被 adopt 的精确 ops；创建与退休为空数组。 */
	    readonly ops: readonly Op[] }
	| { readonly type: "document.copy"; readonly record: DocumentRecord;
	    readonly conversationId: ConversationId; readonly source: DocumentCopySource };

export type TableCommitChange = Extract<StorageWrite, { type: "conversation"|"entry"|"task"|"submission" }>;
export type CommitChange = TableCommitChange | DocumentCommitChange;
export type CommitPublication = { readonly seq: Seq; readonly changes: readonly CommitChange[] };
export interface Tx { /* 见 §2.4.1 */ }
export type DocumentWatch<T> = ...;    // chord Watch 的封装
export type DocumentReader = { ... };
export type DocumentObserver = { ... };
export type SessionEnd = { readonly reason: "closed" } | { readonly reason: "failed"; readonly error: unknown };
export type CommitPersistence = { readonly kind: "deferred"; readonly withinMs?: number };
export type CommitOptions = { readonly persistence?: CommitPersistence };
export interface Session { /* 见 §2.4.2 */ }
export interface Storage { /* 见 §2.4.3 */ }
```

**易错点**：
- `EntryQuery` 默认 `descending`，其余查询默认 `ascending`。
- `DocumentPoint = Seq | "current"`（**顺序无关，但类型是 `Seq | "current"`，不是 `"current" | Seq` 的字符串判断**——Rust 用 enum）。
- `document.create` 的 `content` **只能是 base**。
- `ConversationRecord.owner` 是 `{ conversationId, taskId }`，不是 `ConversationOwnership`。

#### 2.4.1 `Tx` 完整契约（逐字，types.ts L918-1040）

```ts
/**
 * Transaction surface of one Session commit callback.
 * Table reads and creation results are trusted immutable values and may be shared with internal commit state.
 */
export interface Tx {
	conversation(id: ConversationId): Promise<ConversationRecord | undefined>;
	entry(id: EntryId): Promise<EntryRecord | undefined>;
	/** Undefined when the entry is absent or has another kind. */
	entry<D extends JsonValue>(token: Entry<D>, id: EntryId): Promise<TypedEntry<D> | undefined>;
	task(id: TaskId): Promise<TaskRecord<JsonValue, JsonValue, JsonValue> | undefined>;
	scanConversations(query: ConversationQuery, limit: number, cursor?: Cursor): Promise<Page<ConversationRecord, Cursor>>;
	scanEntries(query: EntryQuery, limit: number, cursor?: Cursor): Promise<Page<EntryRecord, Cursor>>;
	/** Newest visible entry of the conversation that carries a `head`. */
	latestHeadMarker(conversationId: ConversationId): Promise<(EntryRecord & { readonly head: EntryId }) | undefined>;
	scanTasks(query: TaskQuery, limit: number, cursor?: Cursor): Promise<Page<TaskRecord<JsonValue, JsonValue, JsonValue>, Cursor>>;
	/** Committed submission with a conversation-scoped request ID. */
	submissionByRequest(conversationId: ConversationId, requestId: string): Promise<SubmissionRecord | undefined>;

	/** Create a conversation with explicitly selected ownership. */
	createConversation(options: { readonly ownership: ConversationOwnership }): Promise<ConversationRecord>;
	/** Create a history fork at one concrete visible entry with explicitly selected ownership. */
	forkConversation(parentConversationId: ConversationId, at: EntryId,
		options: { readonly ownership: ConversationOwnership }): Promise<ConversationRecord>;
	/** Returned records are Session-owned immutable values and may be shared with commit listeners. */
	appendEntry(conversationId: ConversationId, value: EntryDraft): Promise<EntryRecord>;
	/** The token supplies `kind` and types `data`. */
	appendEntry<D extends JsonValue>(token: Entry<D>, conversationId: ConversationId,
		value: TypedEntryDraft<NoInfer<D>>): Promise<TypedEntry<D>>;
	createTask<I, S extends { phase: string }, R, H extends object>(
		task: Task<I, S, R, H>, input: I, options: TaskOptions): Promise<TaskId<R>>;
	/**
	 * Create a raw submission record with a fresh ID. No admission rules apply: no busy check, no inbox queueing, no
	 * placement. Use `Conversation.submit()` or a conversation handle unless the caller implements admission itself.
	 */
	createSubmission(create: SubmissionCreate): Promise<SubmissionRecord>;
	/**
	 * Settle a queued or placed submission; only a placed input can be answered, and a settled submission stays
	 * unchanged. Resolved against this transaction's latest record of the submission, so it works after table writes.
	 * Run tasks settle the inputs they answer.
	 */
	settleSubmission(id: SubmissionId, settlement: SubmissionSettlement): void;
	/**
	 * Place a queued submission at `entry`: an input becomes `placed`, a write `done`. Resolved like
	 * `settleSubmission()`. Inbox boundaries place the submissions they select.
	 */
	placeSubmission(id: SubmissionId, entry: EntryId): void;

	// doc(...)：6 个重载（session / conversation / task 单例 + 各自的 family 版本，family 多一个 seed 参数）
	doc<T extends JsonObject>(token: SessionDocToken<T>): Promise<Draft<T>>;
	doc<T extends JsonObject>(token: ConversationDocToken<T>, conversationId: ConversationId): Promise<Draft<T>>;
	doc<T extends JsonObject>(token: TaskDocToken<T>, taskId: TaskId): Promise<Draft<T>>;
	doc<T extends JsonObject, I extends JsonValue>(token: SessionDocFamilyToken<T, I>, key: string, seed: I): Promise<Draft<T>>;
	doc<T extends JsonObject, I extends JsonValue>(token: ConversationDocFamilyToken<T, I>, conversationId: ConversationId, key: string, seed: I): Promise<Draft<T>>;
	doc<T extends JsonObject, I extends JsonValue>(token: TaskDocFamilyToken<T, I>, taskId: TaskId, key: string, seed: I): Promise<Draft<T>>;

	// retireDoc(...)：6 个重载（同上，但 family 无 seed）
	retireDoc<T extends JsonObject>(token: SessionDocToken<T>): Promise<void>;
	retireDoc<T extends JsonObject>(token: ConversationDocToken<T>, conversationId: ConversationId): Promise<void>;
	retireDoc<T extends JsonObject>(token: TaskDocToken<T>, taskId: TaskId): Promise<void>;
	retireDoc<T extends JsonObject, I extends JsonValue>(token: SessionDocFamilyToken<T, I>, key: string): Promise<void>;
	retireDoc<T extends JsonObject, I extends JsonValue>(token: ConversationDocFamilyToken<T, I>, conversationId: ConversationId, key: string): Promise<void>;
	retireDoc<T extends JsonObject, I extends JsonValue>(token: TaskDocFamilyToken<T, I>, taskId: TaskId, key: string): Promise<void>;
}
```
- `Draft<T>` 来自 chord：可变的草稿代理，回调结算后再写入无效（`session-documents.test.ts`："revokes escaped drafts when the callback settles"）。
- `retireDoc` 对不存在地址是 no-op（"retires without acquisition and recreates both existing and absent addresses"）。
- `settleSubmission`/`placeSubmission` 返回 `void`（**同步**），但作用对象是"本事务最新记录"。
- `latestHeadMarker` 返回的记录**必有** `head`（不是 `"self"`）。

#### 2.4.2 `Session` 完整契约（逐字，types.ts L1043-1150）

```ts
/** Why a Session ended: `close()`, or a failed Storage call, whose error it carries. */
export type SessionEnd = { readonly reason: "closed" } | { readonly reason: "failed"; readonly error: unknown };

export type CommitPersistence =
	| { readonly kind: "immediate" }
	| { readonly kind: "deferred"; readonly withinMs?: number };

export type CommitOptions = { readonly persistence?: CommitPersistence };

export interface Session extends DocumentObserver {
	commit<T>(change: (tx: Tx) => T | Promise<T>, context: Context, options?: CommitOptions): Promise<T>;
	/** Seal admission, settle admitted commits, then close storage. */
	close(context: Context): Promise<void>;
	/**
	 * Settles once the Session has closed, Storage included: after `close()`, or after the first failed Storage call,
	 * which closes the Session itself. A failed Session is reopened from Storage; nothing else is left to clean up.
	 */
	readonly closed: Promise<SessionEnd>;
	subscribeCommits(listener: (publication: CommitPublication, context: Context) => void): () => void;
	subscribeClose(listener: () => void): () => void;
	// 6 个 snapshot 重载 + 2 个 snapshotAsOf 重载（见下）
}

/** Watch of one committed document incarnation. A retirement delivers `[["r", null]]` with value `null`, then ends the
 *  watch as `retired`; the Session's close ends it as `ended`, its failure as `failed` with the error that failed it. */
export type DocumentWatch<T extends JsonObject> = WatchHandle<Readonly<T> | null>;

/** Committed document reads. */
export type DocumentReader = Pick<Session, "snapshot" | "snapshotAsOf">;

/** Non-creating document watch acquisition shared by Session and later invocation APIs. */
export interface DocumentObserver {
	watchDoc<T extends JsonObject>(token: SessionDocToken<T>, context: Context): Promise<DocumentWatch<T> | undefined>;
	watchDoc<T extends JsonObject>(token: ConversationDocToken<T>, conversationId: ConversationId, context: Context): Promise<DocumentWatch<T> | undefined>;
	watchDoc<T extends JsonObject>(token: TaskDocToken<T>, taskId: TaskId, context: Context): Promise<DocumentWatch<T> | undefined>;
	watchDoc<T extends JsonObject, I extends JsonValue>(token: SessionDocFamilyToken<T, I>, key: string, context: Context): Promise<DocumentWatch<T> | undefined>;
	watchDoc<T extends JsonObject, I extends JsonValue>(token: ConversationDocFamilyToken<T, I>, conversationId: ConversationId, key: string, context: Context): Promise<DocumentWatch<T> | undefined>;
	watchDoc<T extends JsonObject, I extends JsonValue>(token: TaskDocFamilyToken<T, I>, taskId: TaskId, key: string, context: Context): Promise<DocumentWatch<T> | undefined>;
}
```
**`CommitPersistence` 逐字语义**（`types.ts` 注释）：
> `immediate`，默认：在 `commit()` resolve 之前写入 Storage。`deferred`：当这次提交的每个写都只改动**已存在的**、`history:"latest"` 且 `fork:"initial"` 的会话文档（无创建、无迁移、无退休）时，Session 先在内存中 adopt、展示给 watch 与视图并 resolve，但稍后才存储：在下一次读（`tx.doc()`）或写该文档的提交时、在该文档离开内存或退休之前、在 `close()` 时，最迟在提交后约 `withinMs`（默认 `settings.progress.persistWithinMs`，1000ms；`Infinity` 表示只等上述其他时机）。任何其他写入 → immediate。崩溃会丢掉被挂起的内容，所以只有可接受丢失的状态（如流式进度）才应延迟，且不得有持久内容在不读它的情况下依赖它。

**`commit()` 逐字警告**："Run one atomic transaction on the Session mutation line. Inside the callback read only through `tx`: `snapshot()`, `watchDoc()`, `getTask()`, and the other Session and runtime reads may queue on the line the callback holds, and then never resolve."

**`subscribeCommits` 逐字**："Observe stored commits synchronously after adoption, each with exactly what it stored; a commit held in memory is published when it is stored (`CommitPublication`). The listener must not block or call Session APIs."

**`subscribeClose` 逐字**："Observe close synchronously when it begins, also when a failed Storage call closes the Session. The listener must not block or call Session APIs."

**`CommitPublication` 逐字注释**（语义关键）：
> 一次提交发布的内容。一次提交只在被**存储**时发布（被 adopt 之后）；在内存中挂起（`CommitPersistence`）的提交在存储时才发布：由下一次读/写其文档的提交发布，或单独发布（带自己的 `seq`），或与那个提交一起发布——此时那个提交的 document change 先携带挂起的 ops，然后才是自己的。

**6 个 `snapshot` 重载**（返回 `Promise<Readonly<T> | undefined>`，文档不存在 → `undefined`）：
`(token)`、`(token, conversationId)`、`(token, taskId)`、`(token, key)`、`(token, conversationId, key)`、`(token, taskId, key)`，均以 `context` 结尾。
**2 个 `snapshotAsOf` 重载**（仅 rewindable 会话文档 token）：
`(token: RewindableConversationDocToken<T>, conversationId, at: EntryId, context)`、
`(token: RewindableConversationDocFamilyToken<T, I>, conversationId, key, at: EntryId, context)`。

#### 2.4.3 `Storage` 完整契约（逐字，types.ts L1152-1257）

**接口级文档（逐字）**：
> Atomic persistence boundary for Session records.
>
> Storage trusts the owning Session to supply semantically valid records, references, ancestry, and transitions. Implementations enforce atomicity, global ID ownership, immutable conversation/entry creation, document record consistency, and detached values; Session serializes commits.
>
> An error a method throws is final: it fails the Session, which nothing retries, so retry transient failures inside the method. Two errors of a read fail only that read: a `StorageRequestError` for an invalid request (an unknown conversation, a foreign or malformed cursor, history a document does not keep), which must have no durable effect, and a rejection because its caller's context was aborted. Any error from `commit()` or `mintId()` is fatal.

```ts
export interface Storage {
	commit(writes: readonly StorageWrite[], adopt: (seq: Seq) => void, context: Context): Promise<Seq>;
	mintId<I extends Id<string>>(): Promise<I>;
	conversation(id: ConversationId, context: Context): Promise<ConversationRecord | undefined>;
	scanConversations(query: ConversationQuery, limit: number, cursor: Cursor | undefined, context: Context): Promise<Page<ConversationRecord, Cursor>>;
	entry(id: EntryId, context: Context): Promise<{ readonly entry: EntryRecord; readonly commitSeq: Seq } | undefined>;
	entry(conversationId: ConversationId, id: EntryId, context: Context): Promise<{ readonly entry: EntryRecord; readonly commitSeq: Seq } | undefined>;
	findLatestHeadMarker(conversationId: ConversationId, atOrBeforeEntryId: EntryId | undefined, context: Context): Promise<(EntryRecord & { readonly head: EntryId }) | undefined>;
	scanEntries(query: EntryQuery, limit: number, cursor: Cursor | undefined, context: Context): Promise<Page<EntryRecord, Cursor>>;
	task(id: TaskId, context: Context): Promise<TaskRecord<JsonValue, JsonValue, JsonValue> | undefined>;
	scanTasks(query: TaskQuery, limit: number, cursor: Cursor | undefined, context: Context): Promise<Page<TaskRecord<JsonValue, JsonValue, JsonValue>, Cursor>>;
	submission(id: SubmissionId, context: Context): Promise<SubmissionRecord | undefined>;
	scanSubmissions(query: SubmissionQuery, limit: number, cursor: Cursor | undefined, context: Context): Promise<Page<SubmissionRecord, Cursor>>;
	submissionByRequest(conversationId: ConversationId, requestId: string, context: Context): Promise<SubmissionRecord | undefined>;
	findDocument(address: DocumentAddress, at: DocumentPoint, context: Context): Promise<DocumentRecord | undefined>;
	document(id: DocumentId, at: DocumentPoint, context: Context): Promise<StoredDocument | undefined>;
	scanDocuments(query: DocumentQuery, limit: number, cursor: Cursor | undefined, context: Context): Promise<Page<DocumentRecord, Cursor>>;
	close(context: Context): Promise<void>;
}
```

各方法**逐字注释**（决定实现的边界）：
- `commit`："Atomically persist one batch and return its sequence. Once the batch is durable, call `adopt(seq)` exactly once, synchronously and before this storage's next commit starts, then resolve with `seq`; a commit that fails before it is durable never calls `adopt`. `adopt` is the caller's in-memory adoption of the batch, so memory always shows a complete prefix of the log; it must not throw. Once resolved, later reads through this storage observe the batch."
- `mintId`："Return a fresh branded candidate from the Session-global numeric ID namespace, greater than every ID minted since this storage was opened and every ID stored, also across reopen: scans order by ID as creation order, and the Harness relies on it."
- `entry(id, context)`："Look up one global entry and the sequence of the commit that persisted it."
- `entry(conversationId, id, context)`："Look up one entry only when it is visible through the requested conversation's ancestry."
- `findLatestHeadMarker`："Return the newest visible entry with a `head` at or below the optional inclusive cutoff. The returned entry is the marker; its `head` value is the range's actual lower bound."
- `scanEntries`："Scan the inclusive visible range in `query.order` (default descending), returning at most `limit` entries."
- `scanTasks`："Scan task records matching every supplied filter in `query.order` (default ascending) by ID."
- `submissionByRequest`："Find a submission by its conversation-scoped host deduplication key."
- `findDocument`："Resolve the incarnation occupying one exact logical address at the selected point."
- `document`："Materialize one specific incarnation by ID at the selected point without following a replacement at its address."
- `scanDocuments`："Scan incarnations alive in one exact scope at the selected point."
- `close`："Release backend resources; all later operations must reject."

### 2.5 `src/documents.ts`（208 行）

#### 导出

```ts
export function defineDoc<T>(definition: SessionDocDefinition<T>): SessionDocToken<T>;            // 4 个重载
export function defineDoc<T>(definition: DocDefinition<T>): DocToken<T, DocDefinition<T>>;
export function defineDocFamily<T, I>(definition: FamilyInput<T,I> & { scope: "session" }): SessionDocFamilyToken<T,I>;
export function defineDocFamily<T, I>(definition: DocFamilyDefinition<T,I>): DocFamilyToken<T,I,DocFamilyDefinition<T,I>>;
export type AnyDocDefinition = DocumentSemantics & {
  readonly kind: string;
  readonly version: number;
  readonly family?: true;
  initial(seed?: JsonValue): JsonObject;
  migrate?(value: JsonObject, fromVersion: number): JsonObject;
  checkpointWhen(value: Readonly<JsonObject>, ops: readonly Op[], info: CheckpointInfo): boolean;
};
export type AnyDocToken = { readonly definition: AnyDocDefinition };
export type ResolvedAddress = { readonly address: DocumentAddress; readonly id: string; readonly nextArgument: number };
export function resolveAddress(definition: AnyDocDefinition, args: readonly unknown[]): ResolvedAddress;
export function addressId(address: DocumentAddress): string;
export function documentCreate(definition: AnyDocDefinition, address: DocumentAddress, id: DocumentId): DocumentCreate;
export function checkRecordScope(definition: AnyDocDefinition, record: DocumentCreate | DocumentRecord): void;
export function checkRecordVersion(definition: AnyDocDefinition, record: DocumentCreate | DocumentRecord, version: number): void;
export function materializeDocument(definition: AnyDocDefinition, stored: StoredDocument): JsonObject;
export function materializeDocumentValue(definition: AnyDocDefinition, record: DocumentCreate | DocumentRecord, version: number, value: JsonObject): JsonObject;
```

#### 行为细节

`validateDefinition(definition)`（内部，由 `defineDoc`/`defineDocFamily` 调用）：
- 若 `!Number.isSafeInteger(version) || version < 1` → `TypeError("Document ${kind} version must be a positive integer")` **【逐字】**。
- `kind` 无校验（可为空？不校验）。

`resolveAddress(definition, args)`：
- 按 `definition.scope`：
  - `"session"` → `scope = { kind: "session" }`，不消耗参数；
  - `"conversation"` → `scope = { kind: "conversation", conversationId: ownerId(args[i++]) }`；
  - `"task"` → `scope = { kind: "task", taskId: ownerId(args[i++]) }`。
- 若 `definition.family === true` → `key = args[i++] as string`。**注意：key 不做类型校验**（若传入非字符串会在下游 JSON 化时出问题）。
- 返回 `{ address, id: addressId(address), nextArgument: index }`。

`ownerId(value, definition)`：
- 若 `typeof value !== "number" || !Number.isSafeInteger(value)` → `TypeError("Document ${kind} requires a ${scope} ID")` **【逐字】**（`scope` 为 `"conversation"` 或 `"task"`）。
- 注意：此处上下文为 `definition.scope`，因此 session 文档不会走到。

`addressId(address)` **【格式逐字】**：
```js
JSON.stringify([address.kind, address.scope.kind, owner, address.key ?? null])
// owner: session → null; conversation → conversationId; task → taskId
```
例：`["kiana.agent","conversation",7,null]`、`["mcp.tool","session",null,"github"]`。

`documentCreate(definition, address, id)`：
- `key` 存在时并入 `{ key }`。
- `session`/`task`：`{ id, kind, [key], scope }`。
- `conversation`：额外并入 `history: definition.history!`、`fork: definition.fork!`。

`checkRecordScope(definition, record)`：
- 若 `record.scope.kind !== definition.scope`，或 conversation 且 `(record.history !== definition.history || record.fork !== definition.fork)`
  → `TypeError("Document ${record.id} (${record.kind}) does not match the supplied definition semantics")` **【逐字】**。

`checkRecordVersion(definition, record, version)`：
- `version > definition.version` → `Error("Document ${id} (${kind}) has newer version ${version} than ${definition.version}")` **【逐字】**。
- `version < definition.version && definition.migrate === undefined` → `Error("Document ${id} (${kind}) requires migration from version ${version}")` **【逐字】**。

`materializeDocument` / `materializeDocumentValue`：
- 先 `checkRecordScope`，再 `checkRecordVersion`。
- `version === definition.version` → 直接返回 `value`（**不拷贝**；调用方负责所有权）。
- 否则 `copyJson(definition.migrate!(value, version)) as JsonObject`（migrate 结果深拷贝，得到脱钩值）。

### 2.6 `src/entries.ts`（34 行）

```ts
export function defineEntry<D extends JsonValue = never>(kind: string): Entry<D>;
export const UserEntry        = defineEntry("kiana.user");
export const AssistantEntry   = defineEntry("kiana.assistant");
export const SystemEntry      = defineEntry("kiana.system");
export const ToolResultEntry  = defineEntry<{ diagnostics: ToolDiagnostic[] }>("kiana.tool-result");
export const ResetEntry       = defineEntry("kiana.reset");
export const CompactionEntry  = defineEntry<{ reason: CompactionReason }>("kiana.compaction");
```

`defineEntry`：
- 若 `typeof kind !== "string" || kind.length === 0` → `TypeError("Entry kind must be a non-empty string")` **【逐字】**。
- 返回 `{ kind, is(entry) { return entry !== undefined && entry.kind === kind } }`。

各 entry 的 `model` 内容（文档注释，Harness 使用）：
- `kiana.user`：`[UserMessage]`，由 submissions 写入。
- `kiana.assistant`：`[AssistantMessage]`（任意 stopReason），由 generation 写入。
- `kiana.system`：`[SystemMessage]` 且 `content` 为空；位置化提示与工具变更。
- `kiana.tool-result`：`[ToolResultMessage]`，content 末尾是渲染后的诊断块；`data` 为结构化诊断（可能为空）。
- `kiana.reset`：总是 `head: "self"`，`model` absent（纯 reset）或 `[UserMessage]`（handoff 文本）。
- `kiana.compaction`：`[UserMessage]` 包裹摘要，`head` 为第一个保留 entry。

### 2.7 `src/tasks.ts`（8 行）

```ts
export function defineTask<I, S extends { phase: string }, R, H extends object = object>(
  definition: TaskDefinition<I,S,R,H>,
): Task<I,S,R,H> { return { definition }; }
```
恒等包装。

### 2.8 `src/timers.ts`（79 行）

```ts
const MAX_TIMER_DELAY = 2_147_483_647;
const inWorkers = typeof navigator !== "undefined" && navigator.userAgent === "Cloudflare-Workers";
export const nativeTimer: SetTimer;
export type Timers = {
  foreground(callback: () => void, delayMs: number): () => void;
  background(callback: () => void, delayMs: number): (() => void) | undefined;
};
export function hostTimers(setTimer: SetTimer | undefined, report: (error: unknown) => void): Timers;
```

`nativeTimer(callback, delayMs, options)`：
- `options.background && inWorkers` → 返回 `undefined`（不武装）。
- 分段武装：单次 `setTimeout` 最长 `MAX_TIMER_DELAY` ms；超出时链式接续（每次减去 2_147_483_647）。负延迟按 0。
- `background` 时对每个 handle 调 `unref?.()`；若首个 handle 没有 `unref` 函数 → `clearTimeout` 并返回 `undefined`。
- 返回 `() => clearTimeout(handle)`；注意 handle 变量会随接续更新，disarm 应清掉当前 handle（实现上闭包捕获可变 `handle`）。

`hostTimers(setTimer, report)`：
- `arm = setTimer ?? nativeTimer`；`reported` 只报第一个失败。
- `foreground`：调用 `arm(callback, delayMs, { background: false })`；若返回非 undefined → 直接返回。若返回 undefined → `fallBack(new Error("HarnessOptions.setTimer declined a timer that paces work; using setTimeout"))` **【逐字】**，然后 `return nativeTimer(callback, delayMs, { background: false })!`。若 `arm` 抛错 → `fallBack(error)` 后同样回落到 `nativeTimer`。
- `background`：`try { return arm(callback, delayMs, {background:true}) } catch(e) { report(e); return undefined }`。
- 语义要点：**前台计时器必须触发**（否则工作停摆）；后台计时器只释放内存，宿主可拒绝。

### 2.9 `src/truncate.ts`（174 行）

```ts
export const DEFAULT_MAX_LINES = 2000;
export const DEFAULT_MAX_BYTES = 50 * 1024;      // 51200
export interface TruncationResult {
  content: string;
  truncated: boolean;
  truncatedBy: "lines" | "bytes" | null;
  totalLines: number;
  totalBytes: number;
  outputLines: number;
  outputBytes: number;
  lastLinePartial: boolean;         // 本实现恒 false
  firstLineExceedsLimit: boolean;
  maxLines: number;
  maxBytes: number;
}
export interface TruncationOptions { maxLines?: number; maxBytes?: number }
export { utf8ByteLength };          // from chord/output
export function formatSize(bytes: number): string;
export function truncateHead(content: string, options?: TruncationOptions): TruncationResult;
export function truncateHeadOf(prefix: string, totals: { lines: number; bytes: number }, options?: TruncationOptions): TruncationResult;
```

`splitLinesForCounting(content)`（内部）：
- `content.length === 0` → `[]`；
- `lines = content.split("\n")`；若 `content.endsWith("\n")` → `lines.pop()`。

`formatSize(bytes)` **【逐字】**：
- `bytes < 1024` → `` `${bytes}B` ``（整数，无小数）。
- `< 1024*1024` → `` `${(bytes / 1024).toFixed(1)}KB` ``。
- else → `` `${(bytes / (1024*1024)).toFixed(1)}MB` ``。
- 例：`1024 → "1.0KB"`、`1536 → "1.5KB"`、`1048576 → "1.0MB"`、`51200 → "50.0KB"`、`0 → "0B"`。

`truncateHead(content, options)`：计算 `lines = splitLinesForCounting(content).length`、`bytes = utf8ByteLength(content)`，委托 `truncateHeadOf`。

`truncateHeadOf(prefix, totals, options)` 精确算法：
1. `maxLines = options.maxLines ?? 2000`，`maxBytes = options.maxBytes ?? 51200`。
2. `totalBytes = totals.bytes`；`lines = splitLinesForCounting(prefix)`；`totalLines = totals.lines`。
3. 若 `totalLines <= maxLines && totalBytes <= maxBytes`：
   `{ content: prefix, truncated: false, truncatedBy: null, totalLines, totalBytes, outputLines: totalLines, outputBytes: totalBytes, lastLinePartial: false, firstLineExceedsLimit: false, maxLines, maxBytes }`。
4. `firstLineBytes = utf8ByteLength(lines[0])`；若 `firstLineBytes > maxBytes`：
   `{ content: "", truncated: true, truncatedBy: "bytes", totalLines, totalBytes, outputLines: 0, outputBytes: 0, lastLinePartial: false, firstLineExceedsLimit: true, maxLines, maxBytes }`。
   （注：`lines[0]` 在 prefix 为空且需截断时可能为 undefined；`utf8ByteLength(undefined)` 会抛错——实际不会到达，因为空 prefix 不可能超限。）
5. 收集完整行：`outputLinesArr = []`，`outputBytesCount = 0`，`truncatedBy = "lines"`；对 `i` 从 0 到 `min(lines.length, maxLines)-1`：
   - `lineBytes = utf8ByteLength(line) + (i > 0 ? 1 : 0)`（换行符计 1）；
   - 若 `outputBytesCount + lineBytes > maxBytes` → `truncatedBy = "bytes"`，break；
   - push，累加。
6. **【关键】** `if (truncatedBy !== "bytes") truncatedBy = outputLinesArr.length < totalLines ? "lines" : "bytes";`
   —— 没有字节越界时，只有"确实有行被省略"才证明行数达到上限；否则说明是末尾换行导致字节超限。
7. `outputContent = outputLinesArr.join("\n")`；`outputBytes = utf8ByteLength(outputContent)`。
8. 返回 `{ content: outputContent, truncated: true, truncatedBy, totalLines, totalBytes, outputLines: outputLinesArr.length, outputBytes, lastLinePartial: false, firstLineExceedsLimit: false, maxLines, maxBytes }`。

`prefix` 的契约（文档字符串）：prefix 必须是全文，或超过 `maxBytes + 1` 个 UTF-8 字节，或含至少 `maxLines` 个换行；此时结果与对全文调用 `truncateHead` 相同。

### 2.10 `src/search.ts`（85 行）

```ts
export type SearchDocument = { readonly name: string; readonly text: string };
const STOP_WORDS: ReadonlySet<string> = new Set([
  "a","an","and","are","as","at","be","by","for","from","in","is","it","of","on","or","that","the","this","to","with",
]);
export function tokenize(text: string): string[];
export function rankDocuments(query: string, documents: readonly SearchDocument[], limit: number): string[];
```

`stem(term)`（内部）：
- `term.length > 4 && term.endsWith("ies")` → `term.slice(0,-3) + "y"`；
- 否则 `term.length > 4 && /(ches|shes|sses|xes|zes)$/.test(term)` → `term.slice(0,-2)`；
- 否则 `term.length > 3 && term.endsWith("s") && !term.endsWith("ss")` → `term.slice(0,-1)`；
- 否则原样。
（长度按 JS UTF-16 code unit 计。）

`tokenize(text)`：
1. `text.replace(/([a-z0-9])([A-Z])/g, "$1 $2")`（camelCase 边界）；
2. `.replace(/([A-Z]+)([A-Z][a-z])/g, "$1 $2")`（连续大写边界，如 `HTTPServer` → `HTTP Server`）；
3. `.toLowerCase()`；
4. `.split(/[^a-z0-9]+/)`；
5. 过滤空串与 stop words；
6. `.map(stem)`。

`rankDocuments(query, documents, limit)`（Okapi BM25，`k1 = 1.2`，`b = 0.75`）：
- `queryTerms = [...new Set(tokenize(query))]`；若空、文档空、`limit <= 0` → `[]`。
- 每文档 term 计数（对 `tokenize(document.text)`）；`lengths` = 每文档词数；`averageLength = sum / n || 1`（注意 `|| 1`：0 时取 1）。
- `idf(term) = Math.log(1 + (N - df + 0.5) / (df + 0.5))`。
- `score = Σ_{q∈queryTerms} idf(q) * (tf * (k1+1)) / (tf + k1 * (1 - b + b * len/avgLen))`。
- 仅保留 `score > 0`；按降序排序（**稳定性依赖 JS `Array.prototype.sort` 稳定性**：相同分数保留文档顺序）；取前 `limit`；返回名字数组。

### 2.11 `src/storage/scan.ts`（40 行）

```ts
export type ScanStart = { readonly order: ScanOrder; readonly after: number | undefined };
export function scanStart(requested: ScanOrder | undefined, cursor: Readonly<Record<string, unknown>> | undefined, fallback: ScanOrder): ScanStart;
export function nextCursor(id: Id<string>, order: ScanOrder): { readonly after: number; readonly order: ScanOrder };
```

`scanStart` 规则 **【错误文案逐字】**：
1. `requested !== undefined && requested !== "ascending" && requested !== "descending"` → `StorageRequestError("Invalid scan order: ${String(requested)}")`。
2. `cursor === undefined` → `{ order: requested ?? fallback, after: undefined }`。
3. 从 cursor 取 `{ after, order: stored }`：
   - `typeof after !== "number" || !Number.isSafeInteger(after)` → `StorageRequestError("Invalid storage cursor")`；
   - `stored !== undefined && stored !== "ascending" && stored !== "descending"` → `StorageRequestError("Invalid storage cursor")`；
4. `order = stored ?? fallback`。
5. 若 `requested !== undefined && requested !== order` → `StorageRequestError("The cursor continues a ${order} scan; the query asks for ${requested}")`。
6. 返回 `{ order, after }`。

语义：cursor 沿用其创建时的顺序，即使查询重复了该顺序或省略了顺序；无 `order` 的旧 cursor 使用扫描默认顺序。

`nextCursor(id, order)` → `{ after: id, order }`。

---

### 2.12 `src/storage/memory.ts`（866 行）— 参考语义

**定位**：Memory 是**参考语义**。它有意克隆保留的写入值与所有读结果，以模拟 SQLite 编解码/JSONL 序列化天然产生的所有权边界（"这是后端一致性，不是防御性校验"）。它保留 rewindable 记录，只在已提交 base 或 retirement 之后回收 latest 记录。

#### 内部状态

```ts
type TableName = "conversation" | "entry" | "task" | "submission" | "document";
type DocumentRevision = DocumentContent & { readonly seq: Seq };
type StoredDocumentState = { record: DocumentRecord; revisions: DocumentRevision[] };
type DocumentAction = { create?: DocumentCreate; content?: DocumentContent; retire: boolean };
type DocumentAddressIndex = { ids: DocumentId[]; currentId?: DocumentId };

type State = {
  recordTypes: Map<Id, TableName>;                                // 全局 ID → 表（跨表唯一性）
  conversations: Map<ConversationId, ConversationRecord>;
  conversationIds: ConversationId[];                              // 升序
  conversationIdsByOwnerConversation: Map<ConversationId, ConversationId[]>;
  conversationIdsByOwnerTask: Map<TaskId, ConversationId[]>;
  entries: Map<EntryId, EntryRecord>;
  entryIds: Map<ConversationId, EntryId[]>;                       // 每会话升序
  headEntryIds: Map<ConversationId, EntryId[]>;
  entryCommitSeqs: Map<EntryId, Seq>;
  tasks: Map<TaskId, StoredTask>;
  taskIds: TaskId[];
  taskIdsByStatus: Record<TaskStatus, TaskId[]>;
  submissions: Map<SubmissionId, SubmissionRecord>;
  submissionIds: SubmissionId[];
  submissionIdsByStatus: Record<SubmissionStatus, SubmissionId[]>;
  submissionIdsByRequest: Map<ConversationId, Map<string, SubmissionId>>;
  documents: Map<DocumentId, StoredDocumentState>;
  documentAddresses: Map<string, DocumentAddressIndex>;           // addressKey → 索引
  documentIdsByScope: Map<string, DocumentId[]>;
};
private nextId = 2;
private nextSeq = 1;
private closed = false;
```

#### 键函数（**必须逐字节一致，JSONL/SQLite/IndexedDB 依赖同类语义**）

```ts
const scopeKey = (scope) =>
  scope.kind === "session"      ? JSON.stringify(["session"])
: scope.kind === "conversation" ? JSON.stringify(["conversation", scope.conversationId])
:                                 JSON.stringify(["task", scope.taskId]);

const addressKey = (address) =>
  JSON.stringify([ address.kind, scopeKey(address.scope),
                   address.key === undefined ? ["singleton"] : ["family", address.key] ]);

const recordAddressKey = (record) => addressKey({ kind: record.kind, scope: record.scope, key: record.key });
```
注意 `scopeKey` 结果本身是字符串并被嵌套进数组序列化，因此 `addressKey` 会产生形如 `["kiana.agent","[\"conversation\",7]",["singleton"]]` 的字符串。

#### 存活判定

```ts
const isAliveAt = (record, at) =>
  at === "current" ? record.retiredAt === undefined
                   : record.createdAt <= at && (record.retiredAt === undefined || at < record.retiredAt);
const isCurrentOnly = (record) => record.scope.kind !== "conversation" || record.history === "latest";
```
即：`at` 为 seq 时，记录在 `createdAt <= at < retiredAt` 期间存活。

#### 克隆/冻结

- `clone(value)`：递归克隆；`null`/非对象原样返回；数组 `map`；对象：若原型为 `null` 则用 `Object.create(null)`；逐 key 递归；当目标为普通对象且 key 已存在（如 `__proto__`）时用 `Object.defineProperty` 写数据属性（避免原型污染陷阱）。**Rust 侧**：`serde_json::Value` 深拷贝即可；注意 `__proto__` 键在 `serde_json::Map` 中天然是普通键，不需要特殊处理，但**不得**在反序列化时触发特殊语义。
- `freeze(value)`：递归 `Object.freeze`；已冻结则跳过。**Rust 侧**：无对应概念（可用 `Arc` + 不可变访问），但语义上"返回的写值不可被后续修改"必须成立。

#### 有序数组工具

`lowerBound(ids, target)`：二分，返回第一个 `>= target` 的下标。
`upperBound(ids, target)`：返回第一个 `> target` 的下标。
`insertSorted(ids, id)`：空或末尾 `< id` 则 push，否则 `splice(lowerBound, 0, id)`。
`removeSorted(ids, id)`：`lowerBound` 命中则 `splice`。
`insertMapId(index, key, id)`：取出/创建数组并 `insertSorted`。

#### 分页

```ts
const page = (values, limit, order = "ascending") => {
  const items = values.slice(0, limit);
  if (values.length <= limit) return { items: clone(items) };
  return { items: clone(items), next: nextCursor(items.at(-1)!.id, order) };
};
```
**关键细节**：所有扫描都**多取一个**元素用于判断是否还有下一页（`if (values.length > limit) break`），因此 `values.length === limit` 时**不**产生 `next`。

#### `scanIndexes(ids, start)` 生成器
- 升序：`first = after === undefined ? 0 : upperBound(ids, after)`，从 `first` 到末尾。
- 降序：`end = after === undefined ? ids.length : lowerBound(ids, after)`，从 `end-1` 到 0。

#### `PreparedMemoryCommit`
```ts
export interface PreparedMemoryCommit {
  readonly seq: Seq;
  readonly writes: readonly StorageWrite[];   // 深冻结、脱钩
  apply(): Seq;                               // 幂等（只生效一次）
}
```

#### `MemoryStorage` 方法逐个说明

`async commit(writes, adopt, _context)`：
```ts
const seq = this.prepareCommit(writes).apply();
adopt(seq);
return seq;
```

`prepareCommit(writes, seq = seqFromNumber(this.nextSeq))`：
1. `assertOpen()`（关闭 → `Error("MemoryStorage is closed")` **【逐字】**）。
2. 若 `!Number.isSafeInteger(seq) || seq < this.nextSeq` → `Error("Commit sequence ${seq} does not strictly increase")` **【逐字】**。
3. `detachedWrites = freeze(this.resolveDocumentCopies(writes.map(clone)))`。
4. `checkGlobalIds(detachedWrites)`。
5. `documentActions = prepareDocumentActions(detachedWrites)`。
6. `checkDocumentActions(documentActions)`。
7. 返回惰性 `apply()`（首次调用执行 `applyPreparedCommit` 并置 applied）。

`resolveDocumentCopies(writes)`：
- 若无 `document.copy` 直接返回。
- `changedDocumentIds` 收集所有 `document.create`/`document.copy` 的 `record.id` 与 `document.change`/`document.retire` 的 `id`。
- 对每个 `document.copy`：
  - 若 `changedDocumentIds.has(write.source.id)` → 抛 `Error("Fork source document ${id} is changed in the copy batch")`；
  - `stored = materializeDocument(write.source.id, write.source.at)`；为 undefined → `Error("Fork source document ${id} cannot be read")`；
  - 若源与目标在 `scope.kind`（均为 conversation）、`kind`、`key`、`history`、`fork` 上不一致 → `Error("Fork source document ${id} does not match the copied record")`；
  - 替换为 `{ type: "document.create", record: write.record, content: { kind: "base", version: stored.version, value: stored.value } }`。
- 任何上述错误都被包裹为 `Error("Document copy ${write.record.id} was rejected", { cause })` **【逐字】**。

`applyPreparedCommit(prepared, documentActions, seq)`：
- 逐 write 应用，注意：
  - `conversation`：登记 `recordTypes`、`conversations`、`conversationIds`（有序）；有 `owner` 时同时写入 `conversationIdsByOwnerConversation`（按 `owner.conversationId`）与 `conversationIdsByOwnerTask`（按 `owner.taskId`）；`nextId = max(nextId, id + 1)`。
  - `entry`：登记 `entries`、`entryCommitSeqs = seq`；`entryIds[conversationId]` 有序插入；有 `head` 时插入 `headEntryIds`；更新 `nextId`。
  - `task`：首次出现插入 `taskIds` 与 `taskIdsByStatus[status]`；已存在且 status 变化则从旧状态数组移除、插入新状态数组；覆盖 `tasks`；更新 `nextId`。
  - `submission`：首次插入 `submissionIds` 与 `submissionIdsByStatus[status]`；status 变化时迁移；**requestId 索引维护**：若旧记录有 `requestId` 且该索引指向本 id，则删除（必要时删掉空的内层 map）；写入新记录；若新记录有 `requestId` 则登记。更新 `nextId`。
  - `document.copy`：→ `Error("Prepared document copy was not resolved")` **【逐字】**（理论上不可达）。
  - `document.*`：由 `applyDocumentActions` 统一处理（此处 break）。
- 之后 `applyDocumentActions(documentActions, seq)`，`nextSeq = seq + 1`，返回 `seq`。

`mintId<I>()`：
- `assertOpen()`；若 `!Number.isSafeInteger(nextId)` → `Error("ID space is exhausted")`；返回 `nextId++`。

读方法（全部 `assertOpen()`，全部 `clone` 返回值）：
- `conversation(id)`。
- `scanConversations(query, limit, cursor)`：索引选择优先级 `ownerTaskId` > `ownerConversationId` > 全局 `conversationIds`；`scanStart(query.order, cursor, "ascending")`；迭代 `scanIndexes`，`values.length > limit` 时 break；**额外过滤**：若 `query.ownerConversationId !== undefined` 且 `value.owner?.conversationId !== query.ownerConversationId` 则 continue（因为按 owner task 索引选出后仍需校验）。`page(values, limit, start.order)`。
- `entry(id, ctx)` / `entry(conversationId, id, ctx)`：
  - 两参数形式（`context === undefined`）→ 直接查 `entries`；返回 `{ entry: clone, commitSeq: entryCommitSeqs.get(id)! }`。
  - 三参数形式：`typeof idOrContext !== "number"` → `StorageRequestError("Storage.entry() requires an entry ID")`；通过 `visibleEntries(conversationId, id, id).next().value` 做可见性校验。
- `findLatestHeadMarker(conversationId, atOrBeforeEntryId)`：
  - 未知会话 → `StorageRequestError("Unknown conversation: ${conversationId}")`。
  - 沿 fork 祖先链逐层：在 `headEntryIds[currentId]` 上 `upperBound(ids, upperEntryId) - 1`；命中则返回 `clone({ ...entry, head: entry.head! })`；否则若 `conversation.parent === undefined` 返回 undefined，否则 `upperEntryId = min(upperEntryId, parent.at)`、`currentId = parent.conversationId` 继续。
- `scanEntries(query, limit, cursor)`：
  - `{ order, after } = scanStart(query.order, cursor, "descending")`（**默认降序**）。
  - cursor 收窄边界：降序 `maxEntryId = min(maxEntryId ?? after, after - 1)`；升序 `minEntryId = max(minEntryId ?? after, after + 1)`。
  - 降序用 `visibleEntries`，升序用 `visibleEntriesAscending`；`values.length > limit` break；`page(visible, limit, order)`。
- `task(id)`、`submission(id)`：直接查表+clone。
- `scanTasks(query, limit, cursor)`：`scanStart(..., "ascending")`；id 索引 = `query.status === undefined ? taskIds : taskIdsByStatus[status]`；过滤 `conversationId`、`kind`、`abortRequested`、`background`（全部为"若给定则必须相等"）。
- `scanSubmissions(query, limit, cursor)`：`scanStart(..., "ascending")`；按 status 或全部；过滤 `conversationId`。
- `submissionByRequest(conversationId, requestId)`：`submissionIdsByRequest.get(conv)?.get(requestId)` → clone。
- `findDocument(address, at)`：
  - `index = documentAddresses.get(addressKey(address))`；
  - `at === "current"` → `index.currentId` 存在则 clone 其 record；
  - 否则遍历 `index.ids`（升序）返回第一个 `isAliveAt(record, at)` 的 record。
- `document(id, at)`：`materializeDocument(id, at)` 后 clone 为 `{ record, version, value, deltasSinceBase }`。
- `scanDocuments(query, limit, cursor)`：`ids = documentIdsByScope.get(scopeKey(query.scope)) ?? []`；cursor（`cursorId`：`after` 必须是 safe integer，否则 `StorageRequestError("Invalid storage cursor")`）→ 起点 `upperBound(ids, after)`；过滤 `query.kind` 与 `isAliveAt(record, query.at)`；`page(values, limit)`（默认升序）。
- `close()`：`closed = true`。

`materializeDocument(id, at)`（私有）：
1. 不存在 → undefined。
2. `at !== "current" && isCurrentOnly(record)` → `StorageRequestError("Document ${id} does not retain historical content")` **【逐字】**。
3. `!isAliveAt(record, at)` → undefined。
4. `revisions = at === "current" ? stored.revisions : stored.revisions.filter(r => r.seq <= at)`。
5. 从末尾向前找最后一个 `kind === "base"`；没有 → `Error("Document ${id} is missing a required base")` **【逐字】**。
6. `value = applyImmutableBatches(base.value, documentDeltaBatches(id, base.version, revisions, baseIndex + 1))`；
   `documentDeltaBatches` 对每个 delta 校验 `revision.kind === "delta" && revision.version === version`，否则 `Error("Document ${id} crosses a stored version boundary without a base")` **【逐字】**。
7. 返回 `{ record, version: base.version, value, deltasSinceBase: revisions.length - baseIndex - 1 }`。
   **注意**：`version` 是最后一个 base 的版本，而不是最新 delta 的版本。

`visibleEntries(conversationId, minEntryId = -Infinity, maxEntryId = +Infinity)`（生成器，降序）：
- 未知会话 → `StorageRequestError("Unknown conversation: ${conversationId}")`。
- 沿 fork 链：先当前会话 `entryIds` 的 `[lowerBound..., upperBound(upperEntryId)-1]` 反向产出（`id < minEntryId` 时 break）；然后 `upperEntryId = min(upperEntryId, parent.at)`；若 `< minEntryId` break；否则切到父会话继续。

`visibleEntriesAscending(...)`：先收集链上的 `segments`（`{conversationId, upper}`），`segments.reverse()` 后每段从 `lowerBound(ids, minEntryId)` 正向产出直到 `id > segment.upper`。

`checkGlobalIds(writes)` **【错误逐字】**：
- 遍历写；跳过 `document.change`/`document.retire`；`document.create`/`document.copy` 归入 `"document"` 表，其余按其 `type`。
- 对 `conversation`/`entry`/`document`（**只能新建**）：
  - 已存在于任何表 → `Error("ID ${id} already belongs to ${existing}")`；
  - 本批已出现 → `Error("ID ${id} is written more than once")`。
- 对 `task`/`submission`（**可替换**）：
  - 已存在但属于别的表 → `Error("ID ${id} already belongs to ${existing}")`；
  - 本批已出现但表不同 → `Error("ID ${id} is written as two record types")`。
  （同表同 ID 多次写允许；后写覆盖。）

`prepareDocumentActions(writes)` **【错误逐字】**：
- 对每个文档写取 `id`（create → `record.id`，其余 → `id`），按 id 聚合 `DocumentAction`。
- `document.create`：若已有 `create` 或 `content` → `Error("Document ${id} has more than one content command")`；设置 `create`、`content`。
- `document.change`：若已有 `content` → 同上错误；设置 `content`。
- `document.retire`：若已 `retire` → `Error("Document ${id} is retired more than once")`；置 `retire = true`。

`checkDocumentActions(actions)` **【错误逐字】**：
- 对每个 `[id, action]`：
  - `action.create === undefined && existing === undefined` → `Error("Unknown document: ${id}")`；
  - `action.create !== undefined && existing !== undefined` → `Error("Document ${id} already exists")`；
  - `existing?.record.retiredAt !== undefined` → `Error("Document ${id} is retired")`；
  - 若 `action.content?.kind === "delta"`：`previous`（最后一条 revision）不存在 → `Error("Document ${id} delta has no base")`；`previous.version !== content.version` → `Error("Document ${id} version transition requires a base")`。
  - 地址并发计数：`key = recordAddressKey(create ?? existing.record)`；`live = currentId === undefined ? 0 : 1`；若 `action.retire && currentId === id` 则 `live--`；若 `create !== undefined && !retire` 则 `live++`；记录 `liveCounts[key] = live`。
- 最后任一 `live > 1` → `Error("Document address already has a current incarnation")` **【逐字】**。

`applyDocumentActions(actions, seq)`：
- `create`：
  - 生成 `DocumentRecord = { ...create, createdAt: seq, ...(retire ? { retiredAt: seq } : {}) }`；
  - `stored = { record, revisions: [{ ...content, seq }] }`（**注意 create 必须带 content**）；
  - 登记 `recordTypes`、`documents`；地址索引 `insertSorted(address.ids, id)`；scope 索引 `insertSorted(scopeIds, id)`；`nextId = max(nextId, id+1)`。
- `content` 且无 `create`：`revision = { ...content, seq }`；若 `kind === "base" && isCurrentOnly(record)` → `revisions = [revision]`（**替换全部历史**）；否则 push。
- `retire && create === undefined` → `record = { ...record, retiredAt: seq }`。
- `retire && isCurrentOnly(record)` → `revisions = []`（回收 current-only 历史）。
- 若 `create !== undefined || retire`：更新地址索引 `currentId`——retire 且 `currentId === id` 则删除；create 且 `!retire` 则置为 id。

`assertOpen()`：`closed` → `Error("MemoryStorage is closed")`。

**Rust 注意事项**
- `Map` 顺序：JS `Map` 保持插入顺序；`idleDocuments` 等依赖顺序（见 §2.16）。状态结构里只有 `documentAddresses`/`map` 无顺序依赖，但 `applyDocumentActions` 遍历 `actions` 的顺序 = 首次出现顺序（Map 插入顺序），而对象创建顺序不影响结果。Rust 可用 `BTreeMap` 替代 `HashMap`，但**遍历顺序必须只影响无副作用的结果**；若某处有可观察顺序（如错误"第一个"），需用 `IndexMap`/`Vec` 保持插入序。
- `applyImmutableBatches` 来自 chord：对 `base.value` 依次应用 `Op[]` 批次，返回新不可变值。
- `Page.next` 只在"多取到一个"时出现。

---

### 2.13 `src/storage/jsonl/index.ts` 与 `node.ts`

`index.ts`（125 B）：
```ts
export { JsonlCorruptionError, JsonlStorage, type JsonlStorageOptions, JsonlStoragePoisonedError } from "./storage.ts";
```

`node.ts`（583 B）：
```ts
export async function openNodeJsonlStorage(directory: string, context: Context, options: JsonlStorageOptions = {}): Promise<JsonlStorage> {
  return JsonlStorage.open(directory, new NodeExecutionEnv({ cwd: process.cwd() }), context, options);
}
export { JsonlStorage, type JsonlStorageOptions } from "./storage.ts";
```
注意：Node 适配器**每次调用**都用当前 `process.cwd()` 新建一个 `NodeExecutionEnv`。

---

### 2.14 `src/storage/jsonl/storage.ts`（848 行）— JSONL 后端

#### 常量与类型

```ts
const FORMAT_VERSION = 1;
const MAIN_FILE = "main.jsonl";
const RECLAIM_SUFFIX = ".reclaim";
const textDecoder = new TextDecoder("utf-8", { fatal: true });
```

```ts
type MainOperation =
  | Extract<StorageWrite, { type: "conversation" | "entry" | "submission" | "document.retire" }>
  | { type: "task"; value: StoredTask }
  | { type: "task.sidecar"; id: TaskId; ordinal: number }
  | { type: "document.create"; record: DocumentCreate; ordinal: number }
  | { type: "document.change"; id: DocumentId; ordinal: number };

type MainMarker = { format: 1; type: "commit"; seq: Seq; writes: readonly MainOperation[] };

type SidecarPayload =
  | { type: "task"; value: StoredTask }
  | { type: "document"; id: DocumentId; content: DocumentContent };

type SidecarRecord = { format: 1; type: "record"; seq: Seq; ordinal: number; payload: SidecarPayload };

type ParsedLine<T>  = { value: T; start: number };       // start = 该行在文件中的字节偏移
type ParsedFile<T>  = { path: string; lines: readonly ParsedLine<T>[] };
type EncodedCommit  = { marker: string; sidecars: ReadonlyMap<string, string> };

export type JsonlStorageOptions = { fsync?: boolean };    // 默认 false
```

#### 错误类

```ts
export class JsonlCorruptionError extends Error {        // name = "JsonlCorruptionError"
  constructor(message: string, cause?: Error)            // super(message, { cause })
}
export class JsonlStoragePoisonedError extends Error {   // name = "JsonlStoragePoisonedError"
  constructor(cause: Error) { super("JSONL storage is poisoned and must be reopened", { cause }); }
}
```

#### 校验器（**全部错误文案逐字**）

`isObject(v)`：非 null、typeof object、非数组。
`isSafeInteger(v)`：`Number.isSafeInteger`。

文件名：
```ts
const sidecarFileName = (kind: "doc" | "task", id) => `${kind}-${id}.jsonl`;
const isSidecarFileName  = (name) => /^(?:doc|task)-(?:0|[1-9]\d*)\.jsonl$/.test(name);
const isReclaimFileName  = (name) => /^(?:doc|task)-(?:0|[1-9]\d*)\.jsonl\.reclaim$/.test(name);
```
（只接受十进制、无前导零、非负。）

```ts
const isCurrentOnly = (record: DocumentCreate) => record.scope.kind !== "conversation" || record.history === "latest";
const sidecarKey = (file, seq, ordinal) => JSON.stringify([file, seq, ordinal]);
const jsonLine = (value) => `${JSON.stringify(value)}\n`;
const errorFromFile = (action, error) => new Error(`JSONL ${action} failed: ${error.message}`, { cause: error });
```

`parseJson(text, description)`：`JSON.parse` 失败 → `JsonlCorruptionError("Malformed complete ${description}", cause)`。

`validateMainOperation(value, description)`：
- 非对象或无字符串 `type` → `Invalid write in ${description}`。
- `conversation`/`entry`/`submission`：`value.value` 非对象或 `id` 非 safe integer → `Invalid ${value.type} write in ${description}`。
- `task`：`value`/`value.state` 非对象、`id` 非安全整数、或 `state.status !== "terminal"` → `Invalid terminal task write in ${description}`。
- `document.retire`：`id` 非安全整数 → `Invalid document retirement in ${description}`。
- `task.sidecar`：`id`/`ordinal` 非安全整数或 `ordinal < 0` → `Invalid task sidecar write in ${description}`。
- `document.create`：`record` 非对象、`record.id`/`ordinal` 非安全整数、`ordinal < 0` → `Invalid document creation in ${description}`。
- `document.change`：`id`/`ordinal` 非安全整数、`ordinal < 0` → `Invalid document change in ${description}`。
- 其他 type → `Unknown write type in ${description}`。

`parseMainMarker(text, line)`：
- `description = "${MAIN_FILE} line ${line}"`（例：`main.jsonl line 3`）。
- 要求：对象、`format === 1`、`type === "commit"`、`seq` 安全整数且 `>= 1`、`writes` 是数组；否则 `Invalid commit marker in ${description}`。
- 返回规范化对象（`format` 固定 1，`writes` 逐项 validate）。

`validateDocumentContent(value, description)`：
- 非对象、`version` 非安全整数或 `< 1` → `Invalid document content in ${description}`。
- `kind === "base"` 且 `value` 是对象 → OK；`kind === "delta"` 且 `ops` 是数组 → OK；否则同错误。

`parseSidecarRecord(text, file, line)`：
- `description = "${file} line ${line}"`。
- 要求：对象、`format === 1`、`type === "record"`、`seq` 安全整数 `>= 1`、`ordinal` 安全整数 `>= 0`、`payload` 对象且 `payload.type` 为字符串；否则 `Invalid sidecar record in ${description}`。
- `payload.type === "task"`：`value`/`value.state` 非对象、`id` 非安全整数、或 `state.status === "terminal"` → `Invalid live task record in ${description}`（sidecar 只存**非终态**任务）。
- `payload.type === "document"`：`id` 非安全整数 → `Invalid document record in ${description}`；再 `validateDocumentContent(payload.content, description)`。
- 其他 → `Unknown sidecar record type in ${description}`。

#### `JsonlStorage` 类

私有字段：`fs: FileSystem`、`directory: string`、`mainPath: string`、`fsync: boolean`、`memory = new MemoryStorage()`、`currentOnlyDocuments: Set<DocumentId>`、`liveTaskSidecars: Set<TaskId>`、`closed: boolean`、`poisonError?: JsonlStoragePoisonedError`。

`static async open(directory, fs, context, options = {})`：
1. `fs.absolutePath(directory)` 失败 → `errorFromFile("path resolution", err)`。
2. `fs.createDir(absolute, { recursive: true })` 失败 → `errorFromFile("directory creation", err)`。
3. `fs.joinPath([absolute, "main.jsonl"])` 失败 → `errorFromFile("path join", err)`。
4. `new JsonlStorage(...)` 并 `await storage.recover(context)`。

`commit(writes, adopt, context)`：
1. `assertUsable()`。
2. `prepared = this.memory.prepareCommit(writes)`（**同步完成全部校验与脱钩**）。
3. `encoded = this.encodeCommit(prepared.seq, prepared.writes)`。
4. `reclamations = this.planReclamations(prepared.writes, encoded)`。
5. 并发解析所有 sidecar 路径（`Promise.all`）。
6. 逐个 `fs.appendFile(sidecar.path, content)`；失败 → `throw this.poison(errorFromFile("append to ${file}", err))`。
7. `fsync` 时逐个 `fs.flushFile(sidecar.path)`；失败 → `poison(errorFromFile("flush of ${file}", err))`。
8. `fs.appendFile(mainPath, encoded.marker)`；失败 → `poison(errorFromFile("append to ${MAIN_FILE}", err))`。
9. `seq = prepared.apply()`；`this.adoptSidecarState(prepared.writes)`。
10. `adopt(seq)`（**此时 marker 已落盘 = 批已持久**）。
11. `await this.reclaimSidecars(reclamations, context)`（**adoption 之后**，尽力而为）。
12. 返回 `seq`。

`encodeCommit(seq, writes)`：
- `nextOrdinal = 0`，`records: Map<file, SidecarRecord[]>`。
- `addSidecar(file, payload)`：`ordinal = nextOrdinal++`，push `{ format:1, type:"record", seq, ordinal, payload }`。
- 对每个 write：
  - `conversation`/`entry`/`submission`/`document.retire` → 直入 `mainWrites`。
  - `task`：`state.status === "terminal"` → 直入 `mainWrites`（**终态任务写主日志**）；否则 sidecar `task-${id}.jsonl` 并写 `{type:"task.sidecar", id, ordinal}`。
  - `document.create` → sidecar `doc-${id}.jsonl`，payload `{ type:"document", id, content }`，main `{type:"document.create", record, ordinal}`。
  - `document.change` → sidecar `doc-${id}.jsonl`，main `{type:"document.change", id, ordinal}`。
- sidecar 内容 = 该文件所有记录的 JSON 行拼接。
- marker = `jsonLine({ format:1, type:"commit", seq, writes: mainWrites })`。

`planReclamations(writes, encoded)`：
- 收集：`createdCurrentOnlyDocuments`（`document.create` 且 `isCurrentOnly`）、`baseDocuments`（`document.change` 且 `content.kind === "base"`）、`retiredDocuments`（`document.retire`）、`finalTasks`（`task` 写的最终记录）。
- `isCurrentOnlyDocument(id) = this.currentOnlyDocuments.has(id) || createdCurrentOnlyDocuments.has(id)`。
- **retirement**：每个 `retiredDocuments` 中 current-only 的 id → `replacements["doc-${id}.jsonl"] = ""`（删除文件）。
- **base**：每个 `baseDocuments` 中非 current-only 或同时 retired 的跳过；否则 `replacements[file] = encoded.sidecars.get(file)`（替换为只含本次 base 的内容）。
- **terminal task**：每个终态 task，若 `this.liveTaskSidecars.has(id) || encoded.sidecars.has("task-${id}.jsonl")` → `replacements["task-${id}.jsonl"] = ""`。

`adoptSidecarState(writes)`：`document.create` 且 current-only → 加入 `currentOnlyDocuments`；`task` → 终态则从 `liveTaskSidecars` 删除，否则加入。

`reclaimSidecars(replacements, context)`：
- 空 → 返回。
- `fsync` 时先 `fs.flushFile(mainPath)`；失败 → **静默返回**（延迟回收）。
- 逐个 `replaceSidecar`。

`replaceSidecar(file, content, context)`（**所有失败静默**）：
- `joinPath([directory, file])` 失败 → 返回。
- `content === ""` → `fs.remove(path, { force: true })`；返回。
- `joinPath([directory, file + ".reclaim"])` 失败 → 返回。
- `fs.writeFile(tmp, content)` 失败 → 返回。
- `fsync` 时 `fs.flushFile(tmp)` 失败 → 返回。
- `fs.renameFile(tmp, path)`。
- 语义：临时文件 + rename 原子替换；不改动 main.jsonl 的压缩（初始实现不压缩主日志）。

`recover(context)` — 详细步骤：
1. `readLines(fs, mainPath, "main.jsonl", ...parseMainMarker)`。
2. 检查 marker seq 严格递增；否则 `JsonlCorruptionError("Commit sequence does not strictly increase in ${MAIN_FILE}")` **【逐字】**。
3. `fs.listDir(directory)` 失败 → `errorFromFile("directory listing", err)`。
4. 删除所有 `.reclaim` 文件（`remove(path, { force: true })`）。
5. `sidecarFiles` = 目录中 kind=file 且匹配 sidecar 名的名字，**按名字排序**（`.sort()`，JS 默认 UTF-16 字典序）。
6. 逐个 sidecar `readLines(..., (text, line) => parseSidecarRecord(text, file, line))`；同时校验文件内顺序：若上一条存在且 `(seq < prev.seq || (seq === prev.seq && ordinal <= prev.ordinal))` → `JsonlCorruptionError("Sidecar records are out of order in ${file}")`。登记 `recordByKey[sidecarKey(file, seq, ordinal)] = line`。
7. 遍历 main markers 计算：
   - `currentOnlyDocuments`：`document.create` 且 `isCurrentOnly(record)`；
   - `retiredDocuments`：`document.retire` 的 id；
   - `finalTaskIsLive: Map<TaskId, boolean>`：`task` 写 → `false`；`task.sidecar` → `true`（同 ID 后写覆盖）。
8. `retiredCurrentOnlyDocuments = retiredDocuments ∩ currentOnlyDocuments`。
9. `latestBases: Map<DocumentId, SidecarRecord>`：遍历 main markers，仅对 current-only 文档的 `document.create`/`document.change`：查 `recordByKey[sidecarKey("doc-"+id+".jsonl", marker.seq, operation.ordinal)]`；要求 payload 为 document、id 匹配、`content.kind === "base"`；保留 `(seq, ordinal)` 最大者。
10. `isBeforeLatestBase(id, seq, ordinal)`：`base !== undefined && (seq < base.seq || (seq === base.seq && ordinal < base.ordinal))`。
11. `terminalTasks = { id | finalTaskIsLive[id] === false }`。
12. `confirmed: Set<string>`；重建 writes 并逐 marker `memory.prepareCommit(writes, marker.seq).apply()`：
    - 直接透传：`conversation`、`entry`、`submission`、`task`（**注意：主日志中的 task 只可能是终态**）、`document.retire`。
    - `task.sidecar`：`optional = terminalTasks.has(id)`；`confirmRecord(...)`；若拿到 record：payload 必须是 `task` 且 `value.id === id`，否则 `Confirmed task sidecar data does not match commit ${marker.seq}`；`!optional` 时 push `{ type: "task", value: record.payload.value }`。
    - `document.create` / `document.change`：`reclaimed = retiredCurrentOnlyDocuments.has(id) || isBeforeLatestBase(id, marker.seq, operation.ordinal)`；`confirmRecord`；record 的 payload 必须 document 且 id 匹配，否则 `Confirmed document sidecar data does not match commit ${marker.seq}`；`content = record?.payload.content`。
      - `create`：若 `content !== undefined && content.kind !== "base"` → `Document creation lacks a confirmed base in commit ${marker.seq}`；push `{ type:"document.create", record, content: (reclaimed || content === undefined) ? { kind:"base", version:1, value:{} } : content }`。
      - `change`：若 `!reclaimed && content !== undefined` → push `{ type:"document.change", id, content }`。
    - `prepareCommit/apply` 抛错 → `JsonlCorruptionError("Invalid committed state at sequence ${marker.seq}", cause)`。
13. 尾部截断与回收：对每个 sidecar 文件，遍历其行：
    - `confirmed.has(key)` 且此前已见未确认行 → `Confirmed record follows an unconfirmed tail in ${file}`；
    - 首个未确认行的 `start` 记为 `unconfirmedAt`。
    - 若 `unconfirmedAt !== undefined` → `fs.truncateFile(path, unconfirmedAt)`；失败 → `errorFromFile("tail truncation of ${file}", err)`。
14. 计算保留行：
    - `numericId` 从文件名解析（`file.slice(file.indexOf("-")+1, -".jsonl".length)`）。
    - `task-*` 且属于 `terminalTasks` → `retainedLines = []`。
    - `doc-*`：属于 `retiredCurrentOnlyDocuments` → `[]`；否则若有 `latestBases` → 只保留"不早于最新 base"的已确认行。
    - 若 `retainedLines !== undefined && (retainedLines.length < confirmedLines.length || retainedLines.length === 0)` → `reclamations[file] = 保留行重新 jsonLine 拼接`。
15. `await this.reclaimSidecars(reclamations, context)`。
16. 最后更新内存集合：`currentOnlyDocuments` 全部加入；`finalTaskIsLive` 为 true 的加入 `liveTaskSidecars`。

`confirmRecord(marker, ordinal, file, recordByKey, confirmed, optional)`：
- `key = sidecarKey(file, marker.seq, ordinal)`；若 `confirmed.has(key)` → `Sidecar record is confirmed more than once`。
- 查 `recordByKey`；缺失时：`optional` → 返回 undefined；否则 `Missing confirmed sidecar record ${file} at sequence ${marker.seq}`。
- 加入 `confirmed` 并返回记录。

`readLines(fs, path, name, context, parse)`：
- `fs.readBinaryFile(path)`；`not_found` → `{ path, lines: [] }`；其他错误 → `errorFromFile("read of ${name}", err)`。
- 若末字节不是 `0x0a`：`completeSize = bytes.lastIndexOf(0x0a) + 1`，`fs.truncateFile(path, completeSize)`；失败 → `errorFromFile("torn-line truncation of ${name}", err)`。
- 逐行（以 `0x0a` 切分，`start` 为行首字节偏移）：用 **fatal UTF-8 解码器** 解码；解码失败 → `JsonlCorruptionError("Invalid UTF-8 in complete ${name} line ${lineNumber}")`。
- `parse(text, lineNumber)`（1 起）。

读委托：`mintId`/`conversation`/`scanConversations`/`entry`（含重载分派；三参数形式 `typeof idOrContext !== "number"` → `StorageRequestError("Storage.entry() requires an entry ID")`）/`findLatestHeadMarker`/`scanEntries`/`task`/`scanTasks`/`submission`/`scanSubmissions`/`submissionByRequest`/`findDocument`/`document`/`scanDocuments` 全部转发给 `this.store`（`store` getter 先 `assertUsable()`）。

`close(context)`：幂等；`closed = true`；`memory.close(context)`。

`poison(cause)`：`poisonError ??= new JsonlStoragePoisonedError(cause)`；返回它。
`assertUsable()`：`closed` → `Error("JsonlStorage is closed")`；`poisonError !== undefined` → 抛出它。

**磁盘格式样例**（用于逐字节比对）：

`main.jsonl` 每行一个 JSON 对象：
```json
{"format":1,"type":"commit","seq":1,"writes":[{"type":"conversation","value":{...}},{"type":"document.create","record":{...},"ordinal":0}]}
```
`doc-42.jsonl`：
```json
{"format":1,"type":"record","seq":1,"ordinal":0,"payload":{"type":"document","id":42,"content":{"kind":"base","version":1,"value":{...}}}}
```
`task-7.jsonl`：
```json
{"format":1,"type":"record","seq":3,"ordinal":1,"payload":{"type":"task","value":{...}}}
```
**序号语义**：`ordinal` 是**整个提交内全局递增**的计数器（跨文件共享），不是每文件从 0 开始。

**Rust 注意事项**
- `JSON.stringify` 的键顺序 = 对象字面量插入顺序。Rust 侧若用 `serde_json::Value`（`preserve_order` 特性）可保留；否则序列化顺序会不同，导致**磁盘格式不一致**。必须启用 `serde_json` 的 `preserve_order` 特性并在构造时严格按此顺序插入字段。
- `JSON.stringify` 会省略值为 `undefined` 的对象键；Rust 侧 `Option::None` + `skip_serializing_if` 等效。
- `Number` 的 JSON 表示：整数输出为整数；`-0` 输出 `0`；非有限数会变成 `null`（本单元应避免）。
- 文件名排序 `.sort()` 是 UTF-16 code unit 字典序；Rust 的 `sort()` 对 `String` 是 UTF-8 字节序。对 ASCII 文件名（本处均如此）两者一致。
- 重建时的 `prepareCommit(writes, marker.seq)` 复用了 Memory 的全部校验；因此任何"磁盘上语义非法"的状态都会在 open 时以 `Invalid committed state at sequence N` 暴露。

---

### 2.15 `src/storage/sqlite/{schema,migrations,storage,index,node,cloudflare,database}.ts`

#### 2.15.1 `schema.ts`（63 行）

标识符表（**不可变历史**：永不修改/删除既有值；重命名只新增条目）：

```ts
const TABLE_NAMES = {
  durableSchema: "durable_schema",
  durableMetadata: "durable_metadata",
  recordIds: "record_ids",
  conversations: "conversations",
  entries: "entries",
  tasks: "tasks",
  submissions: "submissions",
  documents: "documents",
  documentRevisions: "document_revisions",
} as const;

const INDEX_NAMES = {
  conversationsByOwnerConversation: "conversations_by_owner_conversation",
  conversationsByOwnerTask: "conversations_by_owner_task",
  entriesByConversation: "entries_by_conversation",
  entryHeadsByConversation: "entry_heads_by_conversation",
  tasksByStatus: "tasks_by_status",
  tasksByConversation: "tasks_by_conversation",
  tasksByKind: "tasks_by_kind",
  tasksByAbortRequested: "tasks_by_abort_requested",
  tasksByBackground: "tasks_by_background",
  submissionsByRequest: "submissions_by_request",
  submissionsByConversation: "submissions_by_conversation",
  submissionsByStatus: "submissions_by_status",
  documentsByAddress: "documents_by_address",
  documentsByScope: "documents_by_scope",
  documentsByScopeKind: "documents_by_scope_kind",
  documentRevisionsByKind: "document_revisions_by_kind",
} as const;

export type SqliteSchema = { tables: {...}; indexes: {...} };
export function createSqliteSchema(prefix?: string): SqliteSchema;   // 前缀拼接 `${prefix}${name}`；prefix 先 validate
export const DEFAULT_SQLITE_SCHEMA = createSqliteSchema();
export { validateSqliteSchemaPrefix };                              // 来自 chord/sqlite
```
`validateSqliteSchemaPrefix` 的契约（chord）：必须是合法 SQLite 标识符且不得以保留前缀 `sqlite_` 开头。

#### 2.15.2 `migrations.ts`（138 行）— SQL **逐字**

`createSqliteMigrations(schema)` 返回 `[{ version: 1, statements }, { version: 2, statements }]`。

**version 1 语句（顺序敏感，逐字）**：

```sql
CREATE TABLE {durable_metadata} (
  singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
  next_id TEXT NOT NULL,
  next_seq INTEGER NOT NULL
) STRICT
INSERT INTO {durable_metadata} (singleton, next_id, next_seq) VALUES (1, '2', 1)
CREATE TABLE {record_ids} (
  id INTEGER PRIMARY KEY,
  record_type TEXT NOT NULL CHECK (record_type IN ('conversation', 'entry', 'task', 'submission', 'document'))
) STRICT
CREATE TABLE {conversations} (
  id INTEGER PRIMARY KEY,
  owner_conversation_id INTEGER,
  owner_task_id INTEGER,
  record TEXT NOT NULL CHECK (json_valid(record))
) STRICT
CREATE INDEX {conversations_by_owner_conversation} ON {conversations} (owner_conversation_id, id)
CREATE INDEX {conversations_by_owner_task} ON {conversations} (owner_task_id, id)
CREATE TABLE {entries} (
  id INTEGER PRIMARY KEY,
  conversation_id INTEGER NOT NULL,
  head INTEGER,
  commit_seq INTEGER NOT NULL,
  record TEXT NOT NULL CHECK (json_valid(record))
) STRICT
CREATE INDEX {entries_by_conversation} ON {entries} (conversation_id, id DESC)
CREATE INDEX {entry_heads_by_conversation} ON {entries} (conversation_id, id DESC) WHERE head IS NOT NULL
CREATE TABLE {tasks} (
  id INTEGER PRIMARY KEY,
  conversation_id INTEGER NOT NULL,
  kind TEXT NOT NULL,
  status TEXT NOT NULL CHECK (status IN ('pending', 'running', 'waiting', 'completing', 'terminal')),
  abort_requested INTEGER NOT NULL CHECK (abort_requested IN (0, 1)),
  background INTEGER NOT NULL CHECK (background IN (0, 1)),
  record TEXT NOT NULL CHECK (json_valid(record))
) STRICT
CREATE INDEX {tasks_by_status} ON {tasks} (status, id)
CREATE INDEX {tasks_by_conversation} ON {tasks} (conversation_id, id)
CREATE INDEX {tasks_by_kind} ON {tasks} (kind, id)
CREATE INDEX {tasks_by_abort_requested} ON {tasks} (abort_requested, id)
CREATE INDEX {tasks_by_background} ON {tasks} (background, id)
CREATE TABLE {submissions} (
  id INTEGER PRIMARY KEY,
  conversation_id INTEGER NOT NULL,
  request_id TEXT,
  status TEXT NOT NULL CHECK (status IN ('queued', 'placed', 'done', 'unanswered')),
  record TEXT NOT NULL CHECK (json_valid(record))
) STRICT
CREATE INDEX {submissions_by_request} ON {submissions} (conversation_id, request_id)
CREATE INDEX {submissions_by_conversation} ON {submissions} (conversation_id, id)
CREATE INDEX {submissions_by_status} ON {submissions} (status, id)
CREATE TABLE {documents} (
  id INTEGER PRIMARY KEY,
  kind TEXT NOT NULL,
  family INTEGER NOT NULL CHECK (family IN (0, 1)),
  key_value TEXT NOT NULL,
  scope_kind TEXT NOT NULL CHECK (scope_kind IN ('session', 'conversation', 'task')),
  owner_id INTEGER NOT NULL,
  created_at INTEGER NOT NULL,
  retired_at INTEGER,
  record TEXT NOT NULL CHECK (json_valid(record))
) STRICT
CREATE INDEX {documents_by_address}
  ON {documents} (kind, scope_kind, owner_id, family, key_value, created_at DESC, retired_at)
CREATE INDEX {documents_by_scope} ON {documents} (scope_kind, owner_id, id)
CREATE INDEX {documents_by_scope_kind} ON {documents} (scope_kind, owner_id, kind, id)
CREATE TABLE {document_revisions} (
  document_id INTEGER NOT NULL,
  seq INTEGER NOT NULL,
  kind TEXT NOT NULL CHECK (kind IN ('base', 'delta')),
  version INTEGER NOT NULL,
  content TEXT NOT NULL CHECK (json_valid(content)),
  PRIMARY KEY (document_id, seq)
) STRICT
CREATE INDEX {document_revisions_by_kind} ON {document_revisions} (document_id, kind, seq DESC)
```
说明（源码注释）：`next_id` 用 TEXT 因为 `node:sqlite` 拒绝超出 JS 安全整数范围的 INTEGER 结果。

**version 2**：重建 tasks 表以加入 `sleeping` 状态（SQLite 不能就地改 CHECK）：`{tasks}_v2` 表（status CHECK 含 `'pending','running','waiting','sleeping','completing','terminal'`）→ `INSERT ... SELECT` 复制 → `DROP TABLE {tasks}` → `ALTER TABLE {tasks}_v2 RENAME TO {tasks}` → 重建 5 个 tasks 索引。

导出：
```ts
export const SQLITE_MIGRATIONS = createSqliteMigrations(DEFAULT_SQLITE_SCHEMA);
export const CURRENT_SQLITE_SCHEMA_VERSION = SQLITE_MIGRATIONS.at(-1)?.version ?? 0;   // = 2
export async function applySqliteMigrations(database, migrations = SQLITE_MIGRATIONS): Promise<void>;
export async function applySqliteMigrationsForSchema(database, schema, migrations = createSqliteMigrations(schema)): Promise<void>;
export function createSqliteMigrations(schema: SqliteSchema): readonly SqliteMigration[];
```
迁移器（chord 的 `applySqliteMigrations(database, migrations, { table })`）语义：单事务内应用所有待应用版本；`durable_schema` 记录已应用版本；库版本高于可用版本时拒绝（`sqlite-migrations.test.ts`："rejects a database newer than the portable core"）；失败整体回滚。

#### 2.15.3 `storage.ts`（959 行）

辅助（**逐字**）：
```ts
const parseJson = <T>(value: string): T => JSON.parse(value) as T;
const encodeJson = (value: unknown): string => JSON.stringify(value) as string;
// 某些 SQLite 绑定会替换孤立 UTF-16 代理；JSON 编码保持被索引标识无损。
const encodeIndexedString = (value: string): string => JSON.stringify(value);
```
注意：`kind`、`request_id`、`documents.kind`、`documents.key_value` 都以 **JSON 字符串字面量**（带引号与转义）入库。

`cursorId(cursor)`：`after` 必须 safe integer，否则 `StorageRequestError("Invalid storage cursor")`。
`page(values, limit, order = "ascending")`：与 memory 相同（截断 + `next`）。
`scanSql(start)`：
- 升序 → `{ clause: "id > ?", param: start.after ?? -1, direction: "ASC" }`
- 降序 → `{ clause: "id < ?", param: start.after ?? Number.MAX_SAFE_INTEGER, direction: "DESC" }`
`scopeColumns(scope)`：session → `{ scopeKind: "session", ownerId: 0 }`；conversation → ownerId = conversationId；task → ownerId = taskId。
`addressParts(address)`：`{ kind: JSON.stringify(kind), ...scopeColumns, family: key === undefined ? 0 : 1, keyValue: JSON.stringify(key ?? "") }`。
`addressKey(address)`：`JSON.stringify([parts.kind, parts.scopeKind, parts.ownerId, parts.family, parts.keyValue])`。
`isAliveAt` / `isCurrentOnly`：同 memory。
`writeId(write)`：conversation/entry/task/submission → `value.id`；document.create/copy → `record.id`；document.change/retire → undefined。

**`SqliteStorageOptions`**：`{ prefix?: string }`（必须合法 SQLite 标识符、不得用 `sqlite_` 前缀；改动不会迁移既有数据）。

**`static async open(db, options = {})`**：
1. `schema = createSqliteSchema(options.prefix)`。
2. `applySqliteMigrationsForSchema(db, schema)`。
3. `SELECT next_id, next_seq FROM {metadata} WHERE singleton = 1`；无行 → `Error("Durable SQLite metadata is missing")` **【逐字】**。
4. 返回 `new SqliteStorage(db, schema, Number(metadata.next_id))`。
5. 任何失败：尝试 `db.close()`（吞掉其错误），再抛出初始失败。

**`commit(writes, adopt, _context)`**（一个 SQL 事务 = 一次 Session 提交）：
```ts
assertOpen();
const documentActions = prepareDocumentActions(writes);
const candidateNextId = candidateNextId(writes);
const seq = await db.transaction(async (t) => {
  const meta = await t.get("SELECT next_id, next_seq FROM {metadata} WHERE singleton = 1");
  if (meta === undefined) throw new Error("Durable SQLite metadata is missing");
  const committedSeq = seqFromNumber(meta.next_seq);
  await checkGlobalIds(t, writes);
  await checkDocumentActions(t, documentActions);
  for (const write of writes) await applyTableWrite(t, write, committedSeq);
  await applyDocumentActions(t, documentActions, committedSeq);
  await t.run("UPDATE {metadata} SET next_id = ?, next_seq = ? WHERE singleton = 1",
              String(Math.max(Number(meta.next_id), candidateNextId)), committedSeq + 1);
  return committedSeq;
});
this.nextId = Math.max(this.nextId, candidateNextId);
adopt(seq);      // COMMIT 已返回 = 已持久
return seq;
```
- **表写入**：
  - `conversation`：`claimId(id, "conversation")` + `INSERT INTO conversations (id, owner_conversation_id, owner_task_id, record) VALUES (?,?,?,?)`（owner 缺失写 NULL）。
  - `entry`：`claimId` + `INSERT INTO entries (id, conversation_id, head, commit_seq, record)`（`head ?? null`，`commit_seq = committedSeq`）。
  - `task`：`claimId` + `INSERT ... ON CONFLICT(id) DO UPDATE SET conversation_id=excluded..., kind, status, abort_requested, background, record`（`kind` 用 `encodeIndexedString`，布尔转 0/1）。
  - `submission`：`claimId` + `INSERT ... ON CONFLICT(id) DO UPDATE`（`request_id` 为 `undefined ? null : encodeIndexedString(requestId)`）。
  - `document.*`：跳过（由 `applyDocumentActions` 处理）。
- `claimId`：`INSERT OR IGNORE INTO record_ids (id, record_type) VALUES (?, ?)`。
- `candidateNextId(writes)`：从 `this.nextId` 起，对每个 `writeId` 取 `max(nextId, id+1)`。
- `checkGlobalIds`：与 memory 相同规则，但查询 `SELECT record_type FROM record_ids WHERE id = ?`。
- `prepareDocumentActions`：与 memory 相同 + `document.copy` 的 `action.copy = write.source`；`action.create = write.record`；互斥条件包含 `action.copy`。
- `checkDocumentActions`：
  - `action.copy !== undefined && actions.has(action.copy.id)` → `Error("Document copy ${id} source is changed in the copy batch")` **【逐字】**（注意与 memory 的 `Fork source document ... is changed in the copy batch` 不同）。
  - 其余错误文案与 memory 一致。
  - delta 的 previous 版本来自 `SELECT version FROM document_revisions WHERE document_id = ? ORDER BY seq DESC LIMIT 1`。
  - 地址存活计数使用 `currentDocumentId(executor, record)`（`SELECT id ... retired_at IS NULL LIMIT 1`）。
  - **与 memory 的实质差异**：retire 的扣减条件是 `action.retire && existing !== undefined`（只要该文档存在就扣 1），而 memory 是 `action.retire && currentId === id`（只有当前化身才扣）。对"退休一个已被新化身取代的旧化身"这一批写，memory 会把 live 算成 0（允许），sqlite/indexeddb 会算成 -1（同样不触发 >1），但对"退休一个非当前化身 + 同批创建新化身"的组合，两者净结果不同；移植时**必须按各后端原文照抄，不要统一**。
- `applyDocumentActions`：
  - `document.copy`：`materializeDocument(executor, copy.id, copy.at)`；undefined → `Fork source document ${id} cannot be read`；字段校验不一致 → `Fork source document ${id} does not match the copied record`；内容置为 `{ kind:"base", version, value }`。任何错误包裹 `Document copy ${id} was rejected`。
  - `create`：`record = { ...create, createdAt: seq, ...(retire ? { retiredAt: seq } : {}) }`；`claimId(id, "document")`；`INSERT INTO documents (id, kind, family, key_value, scope_kind, owner_id, created_at, retired_at, record)`；`retired_at` 为 `retire ? seq : null`。
  - 非 create：读回 `record`。
  - `content !== undefined`：若 `base && isCurrentOnly(record)` → `DELETE FROM document_revisions WHERE document_id = ?`；然后 `INSERT INTO document_revisions (document_id, seq, kind, version, content)`，content 为 base 时存 `value` 的 JSON，delta 时存 `ops` 的 JSON。
  - `retire`：非 create → `record = { ...record, retiredAt: seq }` 并 `UPDATE documents SET retired_at = ?, record = ? WHERE id = ?`；`isCurrentOnly(record)` → 删除全部 revision。
- `mintId`：`assertOpen()`；非安全整数 → `Error("ID space is exhausted")`；返回 `nextId++`。
- 读：
  - `conversation`：单查询。
  - `scanConversations`：`scanStart(order, cursor, "ascending")` + `scanSql` + 可选 `owner_conversation_id = ?`/`owner_task_id = ?`，`ORDER BY id {ASC|DESC} LIMIT ?`（`limit + 1`）。
  - `entry` 系列：**全部经过 `admitRead`**（见下）；`readEntry` 语义：全局形式直接查 entries（`SELECT record, commit_seq`）；会话形式先读会话（未知 → `StorageRequestError("Unknown conversation: ${conversationId}")`），若 entry 不属于该会话则沿 parent 链走，若到达根仍不匹配 → undefined；若 entry 的 `id > upperEntryId` → undefined。
  - `findLatestHeadMarker`：沿 fork 链逐层 `WHERE conversation_id = ? AND head IS NOT NULL [AND id <= ?] ORDER BY id DESC LIMIT 1`。
  - `scanEntries`：降序用 `readEntries`（沿链逐段查 `id >= min AND id <= upper ORDER BY id DESC LIMIT ?`，`limit + 1 - values.length`）；升序用 `readEntriesAscending`（先收集链 segments，反转后逐段 `ORDER BY id ASC`）。
  - `task`/`submission`：单查询。
  - `scanTasks`：可选 `conversation_id`、`kind`（`encodeIndexedString`）、`status`、`abort_requested`（0/1）、`background`（0/1）。
  - `scanSubmissions`：可选 `conversation_id`、`status`。
  - `submissionByRequest`：`WHERE conversation_id = ? AND request_id = ?`（requestId 用 `encodeIndexedString`）。
  - `findDocument`：current → `retired_at IS NULL ORDER BY created_at DESC LIMIT 1`；历史点 → `created_at <= ? AND (retired_at IS NULL OR retired_at > ?) ORDER BY created_at DESC LIMIT 1`。
  - `document`：**在 事务内** 执行 `materializeDocument`（记录与 revision 查询必须观察同一提交状态）。
  - `scanDocuments`：`scope_kind = ? AND owner_id = ? AND id > ?`（cursor 缺省 -1），可选 `kind`，current → `retired_at IS NULL`，否则 `created_at <= ? AND (retired_at IS NULL OR retired_at > ?)`，`ORDER BY id LIMIT ?`。
- `materializeDocument(executor, id, at)`：与 memory 等价，但：`upper = at === "current" ? Number.MAX_SAFE_INTEGER : at`；base 查询 `kind = 'base' AND seq <= upper ORDER BY seq DESC LIMIT 1`；tail 查询 `seq > base.seq AND seq <= upper ORDER BY seq`；每个 tail 校验 `kind === "delta" && version === base.version`；`deltasSinceBase = tail.length`。
- `close`：幂等；置 `closed = true`；`closing = closeDatabase()`；`closeDatabase` 若有 `admittedReads > 0` 先等 `readsDrained`，再 `db.close()`。
- `admitRead(read)`：`assertOpen()`；`admittedReads++`；`try { await read() } finally { if (--admittedReads === 0) readsDrained?.() }`。语义：**多查询读**在 close 前完成，后续查询不会打到已关闭数据库。
- `assertOpen()`：`Error("SqliteStorage is closed")`。

**Rust 注意事项**
- 使用 `rusqlite`（bundled）可保持相同 SQL；注意 `STRICT` 表需要 SQLite ≥ 3.37。
- `db.transaction(async ...)` 在 chord 是异步事务门面；Rust 侧用 `rusqlite::Transaction`，但需保证 Session 的串行线上不跨 await 持有（或将整个 commit 作为 `spawn_blocking` 内的同步块）。
- `Number.MAX_SAFE_INTEGER`（9007199254740991）作为降序 cursor 缺省参数。
- `next_id` 以 TEXT 存储，读出后 `Number(...)`。
- `record` 列的 JSON 必须与 JS `JSON.stringify` 输出**完全一致**（键插入序、省略 undefined）。这决定 `jsonl`/`sqlite` 互操作与测试的字符串断言。

#### 2.15.4 `index.ts` / `node.ts` / `cloudflare.ts` / `database.ts`

`index.ts`：
```ts
export type { SqliteDatabase, SqliteExecutor, SqliteValue } from "@earendil-works/chord/sqlite";
export { applySqliteMigrations, CURRENT_SQLITE_SCHEMA_VERSION, SQLITE_MIGRATIONS, type SqliteMigration } from "./migrations.ts";
export { SqliteStorage, type SqliteStorageOptions } from "./storage.ts";
```

`node.ts`：
```ts
export type NodeSqliteStorageOptions = NodeSqliteDatabaseOptions & SqliteStorageOptions;
export async function openNodeSqliteStorage(path: string, options: NodeSqliteStorageOptions = {}): Promise<SqliteStorage> {
  const { prefix, ...databaseOptions } = options;
  if (prefix !== undefined) validateSqliteSchemaPrefix(prefix);
  return SqliteStorage.open(await openNodeSqliteDatabase(path, databaseOptions), { prefix });
}
export { NodeSqliteDatabase, type NodeSqliteDatabaseOptions, openNodeSqliteDatabase } from "@earendil-works/chord/sqlite/node";
```
（`NodeSqliteDatabaseOptions` 来自 `@earendil-works/chord/sqlite`，属于 **chord 单元**；本单元只把用户选项透传。Rust 侧对应 `rusqlite::OpenFlags` + `pragma` 设置；具体字段名以 chord 单元规范为准，本单元不得自造。）

`cloudflare.ts`：
```ts
export function openDurableObjectSqliteStorage(storage: DurableObjectSqliteStorage, options: SqliteStorageOptions = {}): Promise<SqliteStorage> {
  return SqliteStorage.open(new DurableObjectSqliteDatabase(storage), options);
}
```

`database.ts`：仅类型再导出。

### 2.16 `src/storage/indexeddb.ts`（1211 行）— 浏览器后端

文件首行是 `/// <reference lib="dom" />`；仅浏览器/`fake-indexeddb` 可用。

#### 常量与名字（逐字）
```ts
const DATABASE_VERSION = 1;                    // 只此一版，没有迁移路径
const MAX_KEY = Number.MAX_SAFE_INTEGER;
const META_KEY = "state";

const STORES = {
  meta: "meta", recordIds: "record_ids", conversations: "conversations", entries: "entries",
  tasks: "tasks", submissions: "submissions", documents: "documents", documentRevisions: "document_revisions",
} as const;
const ALL_STORES = Object.values(STORES);

const INDEXES = {
  conversationsByOwnerConversation: "conversations_by_owner_conversation",
  conversationsByOwnerTask: "conversations_by_owner_task",
  entriesByConversation: "entries_by_conversation",
  entryHeadsByConversation: "entry_heads_by_conversation",
  tasksByStatus: "tasks_by_status",
  tasksByConversation: "tasks_by_conversation",
  tasksByKind: "tasks_by_kind",
  tasksByAbortRequested: "tasks_by_abort_requested",
  tasksByBackground: "tasks_by_background",
  submissionsByRequest: "submissions_by_request",
  submissionsByConversation: "submissions_by_conversation",
  submissionsByStatus: "submissions_by_status",
  documentsByAddress: "documents_by_address",
  documentsByScope: "documents_by_scope",
  documentsByScopeKind: "documents_by_scope_kind",
  documentRevisionsByKind: "document_revisions_by_kind",
} as const;
```

#### 行类型（逐字键名）
```ts
type MetaRow = { key: "state"; nextId: number; nextSeq: number };
type RecordIdRow = { id: number; recordType: "conversation"|"entry"|"task"|"submission"|"document" };
type ConversationRow = { id; ownerConversationId?; ownerTaskId?; record: ConversationRecord };
type EntryRow = { id; conversationId; headConversationId?; commitSeq: Seq; record: EntryRecord };
type TaskRow = { id; conversationId; kind: string; status; abortRequested: 0|1; background: 0|1; record };
type SubmissionRow = { id; conversationId; requestId?; status; record: SubmissionRecord };
type DocumentRow = { id; kind; family: 0|1; keyValue: string; scopeKind; ownerId; createdAt; retiredAt?; record };
type RevisionRow = { documentId; seq: Seq; kind: "base"|"delta"; version: number; content: JsonObject | readonly Op[] };
```

#### schema（`createSchema`，只在 `oldVersion === 0` 时执行）
- `meta` keyPath `"key"`，**建库即 `add({ key: "state", nextId: 2, nextSeq: 1 })`**（`nextId` 从 **2** 起：`1` 预留给根会话）；`record_ids` keyPath `"id"`。
- 复合索引键路径（**逐字数组**，注意每条都以 `"id"` 结尾以便按 ID 定序）：`conversations_by_owner_conversation ["ownerConversationId","id"]`、`conversations_by_owner_task ["ownerTaskId","id"]`、`entries_by_conversation ["conversationId","id"]`、`entry_heads_by_conversation ["headConversationId","id"]`、`tasks_by_status ["status","id"]`、`tasks_by_conversation ["conversationId","id"]`、`tasks_by_kind ["kind","id"]`、`tasks_by_abort_requested ["abortRequested","id"]`、`tasks_by_background ["background","id"]`、`submissions_by_request ["conversationId","requestId"]`（**注意不是以 id 结尾**）、`submissions_by_conversation ["conversationId","id"]`、`submissions_by_status ["status","id"]`。
- `documents_by_address ["kind","scopeKind","ownerId","family","keyValue","createdAt","id"]`（地址 + 化身排序）；`documents_by_scope ["scopeKind","ownerId","id"]`；`documents_by_scope_kind ["scopeKind","ownerId","kind","id"]`。
- `document_revisions` keyPath **`["documentId","seq"]`**；索引 `document_revisions_by_kind ["documentId","kind","seq"]`。

#### 打开/删除
```ts
type IndexedDbStorageOptions = { readonly indexedDB?: IDBFactory };
static async open(name: string, options: IndexedDbStorageOptions = {}): Promise<IndexedDbStorage>
static async delete(name: string, options: IndexedDbStorageOptions = {}): Promise<void>
```
- `factory = options.indexedDB ?? globalThis.indexedDB`；undefined → `Error("IndexedDB is not available")` **【逐字】**。
- `factory.open(name, DATABASE_VERSION)`；`upgradeneeded` 里 **`if (event.oldVersion === 0) createSchema(request.result)`**——已存在但 schema 不全（理论上只有降级/损坏）的库**不会被修复**。
- 打开后用一个 `meta` readonly 事务读 `META_KEY`；undefined → `Error("Durable IndexedDB metadata is missing")` **【逐字】**；任何失败 → `database.close()` 后重抛。
- `delete` 用 `factory.deleteDatabase(name)`（注释：其他连接会阻塞删除）。

#### `commit(writes, adopt, _context)`
1. `assertOpen()`；`prepareDocumentActions(writes)`（**同步、在事务之外**）。
2. 开一个覆盖 **`ALL_STORES`** 的 `"readwrite"` 事务，事务体内：读 `meta`（缺失 → `Durable IndexedDB metadata is missing`）→ `seq = seqFromNumber(metadata.nextSeq)` → `checkGlobalIds` → `checkDocumentActions` → 逐条 `applyTableWrite(write, seq)` → `applyDocumentActions(actions, seq)` → `nextId = max(metadata.nextId, 所有带 ID 的 write 的 id + 1)` → `meta.put({ key:"state", nextId, nextSeq: seq + 1 })`。
3. **事务 complete 之后**才 `adopt(seq)`，再 `return seq`（与 memory/sqlite 的"落盘后同步采纳"一致）。

#### `mintId<I>()`
`assertOpen()`；**独立** `meta` readwrite 事务：读 meta → 非安全整数 → `Error("ID space is exhausted")` **【逐字】**；`put({ ...metadata, nextId: nextId + 1 })`；返回 `idFromNumber<I>(metadata.nextId)`。注意 `commit` 会把 `nextId` 推到显式 ID 之后，`mintId` 只做自增。

#### 事务包装与错误
- `transaction(stores, mode, op)`：`assertOpen()`；`activeOperations++`；成功时 `await done`；失败时 `.abort()`（吞掉 abort 自身的异常）、`await done`（吞掉事务错误以保留原始错误）、重抛原始错误；`finally` 里 `--activeOperations === 0` 时 resolve `operationsDrained`。
- `close(_context)`：幂等（`closing` 缓存）；`closed = true`；等待 `activeOperations` 归零后 `database.close()`。
- `assertOpen()` → `Error("IndexedDbStorage is closed")` **【逐字】**。
- 逐字错误文案：`IndexedDB request failed`（`request.error` 为空时）、`IndexedDB transaction was aborted`、`IndexedDB transaction failed`、`IndexedDB cursor failed`、`IndexedDB key ranges are not available`（`IDBKeyRange` 未定义时 `keyRanges()` 抛）、`Invalid storage cursor`（`StorageRequestError`，`cursor.after` 非安全整数）。
- `requestResult` 用 `{ once: true }` 监听 `success`/`error`；`transactionResult` 监听 `complete`/`abort`/`error`。

#### 查询辅助（逐字语义）
- `page(values, limit, order = "ascending")`：`items = values.slice(0, limit)`；`values.length <= limit` → `{ items }`；否则 `{ items, next: nextCursor(items.at(-1)!.id, order) }`。**扫描函数一律取 `limit + 1` 条来判断是否还有下一页。**
- `cursorId(cursor)`：`cursor?.after`；undefined → undefined；非安全整数 → `StorageRequestError("Invalid storage cursor")`。
- `primaryRange(start)`：无 `after` → `null`；升序 `lowerBound(after, true)`、降序 `upperBound(after, true)`（**开区间**）。
- `compoundIdRange(prefix, minimum, maximum, minimumOpen = false, maximumOpen = false)`：`bound([...prefix, minimum], [...prefix, maximum], ...)`。
- `scanCompoundRows(index, prefix, start, limit, accept)`：升序用 `after` 作下界（开），降序用 `after` 作上界（开），方向 `next`/`prev`。
- `cursorValues(source, range, direction, limit, accept = () => true)`：`openCursor`；**先 `accept` 再入数组**；`values.length >= limit` 时提前 `resolve`（不继续游标）；`null` → resolve 已收集值。
- `firstCursorValue(...)`：返回第一个 `accept` 通过的 row，否则 `undefined`。
- 地址列：`scopeColumns(scope)` → session 用 `ownerId: 0`、conversation 用 `conversationId`、task 用 `taskId`；`addressParts` → `{ kind, ...scope, family: key === undefined ? 0 : 1, keyValue: key ?? "" }`；`addressPrefix` → `[kind, scopeKind, ownerId, family, keyValue]`；`addressKey` → **`JSON.stringify(addressPrefix)`**（与 memory 的 `recordAddressKey` 同构）。
- 存活：`isAliveAt(record, at)`：`at === "current"` → `retiredAt === undefined`；否则 `createdAt <= at && (retiredAt === undefined || at < retiredAt)`。`isCurrentOnly(record)`：`scope.kind !== "conversation" || history === "latest"`。
- `writeId(write)`：conversation/entry/task/submission → `value.id`；document.create/copy → `record.id`；change/retire → undefined。

#### 读方法
- `conversation(id)`：直接 `get`。
- `scanConversations(query, limit, cursor)`：index 选择顺序 **`ownerTaskId` > `ownerConversationId` > 主键**；`accept` 只校验 `ownerConversationId`（未给则全收）。
- `entry` **两个重载**：`entry(id, context)` 与 `entry(conversationId, id, context)`。第二个重载解析：`id = typeof idOrContext === "number" ? idFromNumber(idOrContext) : undefined`，undefined → `StorageRequestError("Storage.entry() requires an entry ID")` **【逐字】**；读会话，缺失 → `StorageRequestError(\`Unknown conversation: ${conversationId}\`)`；**沿 `conversation.parent.at` 向上回溯**，维护 `upperEntryId = min(...)`，`row.id > upperEntryId` → `undefined`（条目在该会话中不可见）。
- `findLatestHeadMarker(conversationId, atOrBeforeEntryId)`：从 `entry_heads_by_conversation` 索引按 `[conversation.id, 0..upper]` 反向取第一条；未命中则向 `conversation.parent.at` 回溯并把 `upper` 收紧；会话不存在 → `Unknown conversation: ${id}`。
- `scanEntries(query, limit, cursor)`：`scanStart(query.order, cursor, "descending")`；降序 `readEntriesDescending`（从 `maxEntryId ?? MAX_KEY` 起，`after` 时 `upper = min(upper, after - 1)`，逐祖先段聚合直到超出 `limit` 或 `conversation.parent === undefined`）；升序 `readEntriesAscending`（**先收集全部 `{conversationId, upper}` 段，再 `.reverse()` 从根到叶读取**，`minimum = max(requestedMinimum, after + 1)`）。两者遇缺失会话都抛 `Unknown conversation: ${conversationId}`。
- `task` / `scanTasks`：index 优先级 **`status` > `conversationId` > `kind` > `abortRequested` > `background`**；`accept` 校验全部已给字段（布尔字段比较 `1/0`）；否则主键扫描。
- `submission` / `scanSubmissions`：`status` > `conversationId` > 主键。`submissionByRequest(conversationId, requestId)` 用 `[conversationId, requestId]` 复合键精确 `get`。
- `findDocument(address, at)`：`documents_by_address` 上 `[[...prefix, 0, 0], [...prefix, MAX_KEY, MAX_KEY]]` 反向游标，`accept = isAliveAt(row.record, at)`。
- `document(id, at)` → `materializeDocument`。
- `scanDocuments(query, limit, cursor)`：`kind === undefined` 用 `documents_by_scope` 前缀 `[scopeKind, ownerId]`，否则 `documents_by_scope_kind` 前缀 `[scopeKind, ownerId, kind]`；游标用 `compoundIdRange(prefix, after ?? 0, MAX_KEY, after !== undefined)`；`accept = isAliveAt(row.record, query.at)`；**只按升序**（`"next"`），无 `order`。

#### `materializeDocument(transaction, id, at)`（逐字）
1. `documents.get(id)`；无记录 → `undefined`。
2. **`at !== "current" && isCurrentOnly(record)` → `StorageRequestError(\`Document ${id} does not retain historical content\`)`**（这是 StorageRequestError，会被 Session 的豁免逻辑识别）。
3. `!isAliveAt(record, at)` → `undefined`。
4. `upper = at === "current" ? MAX_KEY : at`；用 `document_revisions_by_kind` 索引在 `[id,"base",0] .. [id,"base",upper]` 上反向取最近 base；缺失或 `kind !== "base"` → `Error(\`Document ${id} is missing a required base\`)`。
5. `base.seq >= upper` → tail 为空；否则取 `([id, base.seq], [id, upper]]`（下界开）升序全部 revision。逐条：`kind !== "delta" || version !== base.version` → `Error(\`Document ${id} crosses a stored version boundary without a base\`)`；`apply(value, revision.content as readonly Op[])`。
6. 返回 `{ record, version: base.version, value, deltasSinceBase: tail.length }`。

#### `checkGlobalIds(transaction, writes)`
- `claimed: Map<Id, TableName>`；跳过 `document.change`/`document.retire`；`document.create`/`document.copy` 记为 `"document"`，其余用 `write.type`。
- 对 `conversation` / `entry` / `document`：已有 `record_ids` 行 → `Error(\`ID ${id} already belongs to ${existing}\`)`；本批已出现 → `Error(\`ID ${id} is written more than once\`)`。
- 对 `task` / `submission`（可覆盖写）：已有且类型不同 → `ID ${id} already belongs to ${existing}`；本批已有且类型不同 → `Error(\`ID ${id} is written as two record types\`)`。

#### `prepareDocumentActions(writes)`
把四类 document 写折叠成 `Map<DocumentId, { create?, copy?, content?, retire }>`：同 ID 第二条内容命令 → `Error(\`Document ${id} has more than one content command\`)`；重复 retire → `Error(\`Document ${id} is retired more than once\`)`。

#### `checkDocumentActions(transaction, actions)`
1. `action.copy !== undefined && actions.has(action.copy.id)` → `Error(\`Document copy ${id} source is changed in the copy batch\`)` **【逐字】**。
2. `action.create === undefined && existing === undefined` → `Error(\`Unknown document: ${id}\`)`；`create && existing` → `Error(\`Document ${id} already exists\`)`；`existing?.retiredAt !== undefined` → `Error(\`Document ${id} is retired\`)`。
3. `action.content?.kind === "delta"`：取该文档最后一条 revision（`compoundIdRange([id], 0, MAX_KEY)` 反向）；无 → `Error(\`Document ${id} delta has no base\`)`；`previous.version !== action.content.version` → `Error(\`Document ${id} version transition requires a base\`)`。
4. **地址 live 计数**（按 `addressKey(record)` 分组）：`live = (currentDocumentId(...) === undefined ? 0 : 1)`；`action.retire && existing !== undefined` → `live--`；`action.create !== undefined && !action.retire` → `live++`；最后任一 `live > 1` → `Error("Document address already has a current incarnation")`。
   - 与 memory 的差异：此处 retire 分支条件是 `existing !== undefined`，memory 是 `currentId === id`（见 §2.12 与 §2.15.3）。

#### `applyTableWrite(transaction, write, seq)`
- `conversation`：`claimId` 后 `add` 行（`ownerConversationId`/`ownerTaskId` 仅在 `value.owner !== undefined` 时写入）。
- `entry`：`claimId` 后 `add`（`headConversationId` 仅在 `value.head !== undefined` 时写入，值恒为 `write.value.conversationId`；`commitSeq: seq`）。
- `task`：`claimId` 后 **`put`**（可覆盖），行带 `kind`、`status = value.state.status`、`abortRequested/background` 转 `0|1`。
- `submission`：`claimId` 后 `put`，`requestId` 仅在定义时写入，带 `status`。
- `document.*`：`break`（不写表，由 `applyDocumentActions` 处理）。

#### `applyDocumentActions(transaction, actions, seq)`（逐字）
1. 若 `action.copy`：`materializeDocument(copy.id, copy.at)`；undefined → `Error(\`Fork source document ${copy.id} cannot be read\`)`；随后校验 `source.scope.kind === "conversation" && create.scope.kind === "conversation" && kind/key/history/fork` 全部相等，否则 `Error(\`Fork source document ${copy.id} does not match the copied record\`)`；`content = { kind:"base", version: stored.version, value: stored.value }`。上述任一错误都被包成 **`Error(\`Document copy ${id} was rejected\`, { cause })`**。
2. `action.create !== undefined`：`record = { ...action.create, createdAt: seq, ...(action.retire ? { retiredAt: seq } : {}) }`；`claimId(id, "document")`；`documents.add({ id, ...addressParts(record), createdAt: seq, ...(retire ? {retiredAt: seq} : {}), record })`。否则 `record = documents.get(id).record`（此时必存在）。
3. 若有内容：`content.kind === "base" && isCurrentOnly(record)` → **先删除该文档所有 revision**（`compoundIdRange([id], 0, MAX_KEY)`）；再 `revisions.add({ documentId: id, seq, kind: content.kind, version: content.version, content: content.kind === "base" ? content.value : content.ops })`。**revision 的 `seq` 恒为本次提交序号**，所以同一文档在一次提交中最多一条 revision。
4. 若 `action.retire`：非 create 路径要 `record = { ...record, retiredAt: seq }` 并 `documents.put({ ...row, retiredAt: seq, record })`；`isCurrentOnly(record)` → 删除全部 revision（**current-only 文档退休即丢历史**）。
5. 注意 `document.create` + `retire` 同批时，`createdAt` 与 `retiredAt` 都等于同一个 `seq`。

**Rust 决策**：IndexedDB 是浏览器专用。Rust 侧有两个选项：(a) 用 `indexed_db_futures`/`web-sys` 在 `wasm32-unknown-unknown` 下实现同构后端；(b) 明确标记为 out-of-scope 并在 `spec` 中声明。**建议 (b)**，除非 wasm 目标是硬需求；若实现，必须复用同一套 ID/地址/文档语义，且 `commit` 的 adopt 时序与 SQLite 完全一致。

---

### 2.17 `src/session/transaction.ts`（1211 行）— 事务

#### 导出与类型

```ts
export type LoadedDocument = { addressId: string; record: DocumentRecord; storedVersion: number; readonly valueVersion: number; deltasSinceBase: number; readonly tracker: Tracker<JsonObject> };
export type HeldDocument = { readonly loaded: LoadedDocument; readonly pending: PendingOps; base: boolean };
export type AdoptedDocuments = { readonly observed: readonly DocumentCommitChange[]; readonly stored: readonly DocumentCommitChange[] };
export interface TransactionHost {
  readonly storage: Storage;
  now(): number;
  cached(addressId: string): LoadedDocument | undefined;
  load(definition: AnyDocDefinition, addressId: string, address: DocumentAddress, context: Context): Promise<LoadedDocument | undefined>;
  install(document: LoadedDocument): void;
  evict(addressId: string, recordId: DocumentId): void;
  conversationCreated(tx: Transaction, record: ConversationRecord): Promise<void>;
}
export type TaskCreationOptions = TaskOptions & { readonly submissionIds?: readonly SubmissionId[] };
export type TransactionScope = { readonly conversationId?: ConversationId; readonly taskId?: TaskId };
export class Transaction implements Tx { ... }
export function storeHeld(hold: HeldDocument, ops: readonly Op[], value: JsonObject, version: number): { content: DocumentContent; ops: readonly Op[] };
```

内部类型：
```ts
type AnyTaskRecord = TaskRecord<JsonValue, JsonValue, JsonValue>;
type SubmissionChange = SubmissionSettlement | { readonly status: "placed"; readonly entry: EntryId };
type DocumentTarget =
  | { kind: "loaded"; document: LoadedDocument }
  | { kind: "created"; record: DocumentCreate; version: number; tracker: Tracker<JsonObject> }
  | { kind: "fork-copy"; record: DocumentCreate; source: DocumentCopySource }
  | { kind: "retire-only"; record: DocumentRecord };
type DocumentEntry = { addressId; address; definition?; draftPromise?; target?; change?; prepared?; retireOnCommit: boolean };
type DocumentPlan = { addressId; record: DocumentCreate | DocumentRecord; retire: boolean; content?; change?; conversationId?; held? };
```

#### `applySubmissionChange(current, change)`（**导出为内部逻辑，测试直接覆盖**）
```ts
if (current.status === "done" || current.status === "unanswered") return current;   // 已结算不变
if (change.status === "placed") {
  if (current.status !== "queued") throw new Error(`Submission ${current.id} is not queued`);
  const status = current.type === "input" ? "placed" : "done";                     // write 直接 done
  return { ...current, status, entry: change.entry };
}
if (change.status === "done" && current.status !== "placed")
  throw new Error(`Submission ${current.id} is not a placed input`);
return { ...current, ...change };    // done/unanswered：合并 answer/reason/detail；unanswered 保留 entry
```

#### 常量
```ts
const INTERNAL_SCAN_PAGE_SIZE = 256;
const EMPTY_OPERATIONS: readonly Op[] = [];
const TABLE_JSON_COPY_OPTIONS = { omitUndefinedProperties: true } as const;
```
`TABLE_JSON_COPY_OPTIONS` 用于表记录的 `copyJson`：**省略值为 undefined 的属性**（见 §8.4）。

#### `Transaction` 的语义要点（按方法族）

**表读（全部经 `#read`）**：
- 第一次表写之后任何表读 → `ReadAfterWrite`（`Tx.${method}()`）；已 `sealed` → `Error("Transaction has settled")` **【逐字】**（`#assertOpen()`，是本文件里**每一次**表读/表写/`doc()`/`retireDoc()`/`settleSubmission()`/`placeSubmission()`/`setTask()` 的第一道检查）。
- `conversation`/`entry`/`task`/`submissionByRequest`/`scan*`/`latestHeadMarker`：**全部直接读 Storage**（`#host.storage.*`），并非先查 staged（原 spec 的"staged 覆盖"只适用于写路径与结算路径：`#currentTask`、`#assemble` 里的 `#submissions`/`#tasksById`/`#plans`）。`task(id)` 走 `#committedTask`，而 `#committedTask` 对同一 id **只读一次并缓存 promise**（`task.committedRead ??= storage.task(...)`）——同一事务内重复 `task(id)` 不会重复查询，但仍看不到本事务的 `setTask` 候选。
- `entry(token, id)`：kind 不匹配返回 undefined。

**表写**（全部先 `#assertOpen()`、再置 `#hasTableWrite = true`）：
- `createConversation({ ownership })`：`owner` 为 `{ kind: "task", taskId }` 时先 `#currentTask(taskId)`，undefined → `Error("Conversation owner task ${ownerTaskId} does not exist")` **【逐字】**；写 `ConversationRecord`（`owner = { conversationId: task.conversationId, taskId }`）；调用 `host.conversationCreated(tx, record)`（Harness 在此写内置文档）；登记 `#createdConversationIds`。
- `createRootConversation()`：**内部路径**，`#stageConversation(undefined, { kind: "ownerless" }, ROOT_CONVERSATION_ID)`——用**保留 ID 1**、不 mint，注释 "Internal final-form bootstrap path for the reserved root identity"。
- `forkConversation(parentConversationId, at, { ownership })`：
  - 校验 fork 点可见：`Entry ${at} is not visible from conversation ${parentConversationId}` **【逐字】**（来自 `forks.ts`）。
  - 新会话记录带 `parent: { conversationId: parent, at }`。
  - `prepareForkDocumentCopies(...)` 生成 `document.copy` 写（见 §2.19），每个 copy 的 `source.id` 进 `#forkSourceDocumentIds`。
  - 调用 `conversationCreated`。
- `appendEntry`：
  - `head: "self"` 解析为自身 ID（先 mint）。
  - `byTaskId` 来自 `scope.taskId`。
  - 校验 `Entry ${input.assistant} has no tool call ${input.callId}`（Harness 侧触发；本单元需保留该路径）。
- `createTask(task, input, options)`：
  - `options.conversationId` 必需：`Tx.createTask() requires options.conversationId` **【逐字】**。
  - 归属校验（**逐字**，两条路径共用 `#validateOwners()`）：
    - 创建时：`Task owner ${ownership.taskId} does not exist`（`Error`）、`A child task cannot be background`（**`TypeError`**）、`A child task lives in its owner's conversation ${owner.conversationId}`（`Error`）。
    - 装配时（对**最终候选**再校验一次；`what` 为 `"Task owner"` 或 `"Conversation owner task"`）：`${what} ${taskId} does not exist` / `${what} ${taskId} is ${task.state.status}`（仅当 `terminal` 或 `completing`）/ `${what} ${taskId} is abort-marked`。
    - 另有 fork 相关：`Cannot change fork source document ${record.id} in the fork transaction`、`Cannot fork conversation ${scope.conversationId} while changing its current-policy documents`。
  - trace 继承、link 写入；记录初始 `state.status = "pending"`；`copyJson(record, TABLE_JSON_COPY_OPTIONS)`。
  - `submissionIds` 用于内部 run 创建时解析提交载体。
- `setTask`（内部，非 Tx 接口）：
  - **先 `#assertOpen()`，再置 `#hasTableWrite = true`**（因此 `setTask` 算"第一次表写"）。
  - 若本事务**已有的候选**已 terminal → `Task ${value.id} already has a terminal candidate` **【逐字】**（**注意判据是候选，不是已提交记录**）。
  - 候选存在且 `conversationId` 变化 → `Task ${value.id} cannot change conversations` **【逐字】**。
  - 写入前经过 `#stampTimes(value, candidate)`：`startedAt = candidate?.startedAt ?? value.startedAt ?? (status === "running" ? now() : undefined)`；`endedAt = candidate?.endedAt ?? value.endedAt ?? (status === "terminal" ? now() : undefined)`；两者都没变则**原样返回同一个对象**（不复制）；否则合并新值（**undefined 的属性不写**）。随后 `copyJson(record, TABLE_JSON_COPY_OPTIONS)` 取所有权。`kind` 保持 `create`/`replace` 不变。
  - 终态任务替换会退休其 task 作用域文档（在 `#assemble` 里按 `scanDocuments` 分页，每页 256；见"结算"）。
- `createSubmission(create)`：mint ID；requestId 去重（同会话同 requestId 且 type 不同 → `Request ${requestId} already identifies a submission of type ${existing.type}`）。
- `settleSubmission(id, settlement)` / `placeSubmission(id, entry)`：同步，作用于本事务最新记录（`applySubmissionChange`）。
- `stagedTasks` / `stagedConversations`：供 Harness 读取。

**文档族（`doc`/`retireDoc`）**：
- 重载分派用 `resolveAddress(definition, args)`；**先 `#assertTaskDocumentsOpen(resolved)`**：task 作用域且 `#tasksById.get(taskId)?.write?.record.state.status === "terminal"` → `Error("Task ${scope.taskId} is terminal")` **【逐字】**（注意判据是**本事务已写的候选**，不是已提交状态；`doc()` 与 `retireDoc()` 都会先过这一关）。
- `doc` 对同一地址**记忆化**（`draftPromise`），并发重复获取返回同一 promise；只初始化一次。`retireOnCommit` 的条目不可复用，`fork-copy` 目标走 `#acquireForkCopy`。
- `#acquire(entry, seed, skipLoad)`（**原 spec 未列的错误路径，全部逐字**）：
  - `skipLoad` 为真（地址上已有 `retireOnCommit` 的旧条目）时**不读 Storage**，直接建新化身。
  - 否则 `#host.load(...)`；有值 → `checkRecordScope`/`checkRecordVersion`，`entry.target = { kind: "loaded", document }`，`entry.change = loaded.tracker.beginChange()`。
  - 无值 → 先按 scope 校验：conversation 作用域 → `#requireConversation`（见下）；task 作用域 → `#currentTask(taskId)`：undefined → `Error("Task ${scope.taskId} does not exist")` **【逐字】**，`terminal` → `Error("Task ${scope.taskId} is terminal")` **【逐字】**。
  - 然后 `value = copyJson(definition.family === true ? definition.initial(seed) : definition.initial())`；`id = await storage.mintId<DocumentId>()`；`entry.target = { kind: "created", record: documentCreate(definition, entry.address, id), version: definition.version, tracker }`。
  - 每次 `await` 之后都再 `#assertOpen()`（结算后就抛）。
- `#requireConversation(id)`：`#createdConversationIds.has(id)` 直接通过；否则 `storage.conversation(id)` undefined → `Error("Conversation ${id} does not exist")` **【逐字】**。
- `#acquireForkCopy(entry, definition, target)`：`storage.document(target.source.id, target.source.at)`；undefined → `Error("Fork source document ${target.source.id} cannot be read")` **【逐字】**；`stored.record.scope.kind !== "conversation"` 或 `kind`/`key`/`history`/`fork` 任一项与 `target.record` 不符 → `Error("Fork source document ${target.source.id} does not match the copied record")` **【逐字】**；成功则 `materializeDocumentValue(...)` 建 tracker，`entry.target = { kind:"created", record: target.record, ... }`。
- `#findRetirement(entry)`：先取 `#host.cached(addressId)?.record`，否则 `storage.findDocument(entry.address, "current")`；undefined → **no-op 返回**（不建 target）；有值 → `checkRecordScope` 后 `entry.target = { kind: "retire-only", record }`。
- 首次访问：若 Storage 无该地址 → 用 `initial(seed)` 建 base（`session-documents.test.ts`："creates an initial base on first access and adopts it after Storage success"）。
- family：第一个 seed 生效；已存在成员忽略 seed。
- 回调结算后再使用 `Draft`/`Tx` → 抛错（"rejects Tx use after the callback settles"）。
- 结算时若仍有 pending acquisition → 拒绝并排空。
- `retireDoc` 多种变体：已存在则退休；不存在则 no-op；与 pending acquisition 竞争时退休已存在者。

**结算**（四个方法由 Session 在 §2.18 的固定顺序里调用，**不是**一个 `settle()` 内部依次执行；原 spec 的编号容易误读为单次调用内的顺序）：
- `settleFailure()`：置 `#sealed = true` → `#abortChanges()`（对每个 `document.change?.abort()`）→ `await Promise.allSettled(#pendingOperations)` → `#context = BACKGROUND_CONTEXT`。**之后所有 `#assertOpen()` 抛 `Transaction has settled`**。
- `settleSuccess()`：置 `#sealed = true`；若 `#pendingOperations.size > 0` → `#abortChanges()`、`await Promise.allSettled(...)`、`#context = BACKGROUND_CONTEXT`、抛 `Session commit callback settled before its pending Tx operations` **【逐字】**；否则**同步**对每个 `document.change` 调 `prepare()`（"this revokes every draft"），再 `await #assemble()`；任何错误 → `#abortChanges()` 后重抛；`finally` 一律 `#context = BACKGROUND_CONTEXT`（**结算后的 Storage 调用不受调用方取消影响**）。
- `discard()`：`#abortChanges()`；在空批、`#readOnly` 拒绝、以及 `storage.commit` 抛错且**尚未 adopt** 时由 Session 调用。
- `holdable()`：仅当**每个 write 都是 `document.change`**、每个 plan 既未 retire 也有 `change.loaded` 且 `content.type === "document.change"`、记录为 conversation 作用域 + `history === "latest"` + `fork === "initial"`、且 `loaded.storedVersion === version`（未发生迁移）时才返回 `[{ loaded, ops: prepared.ops, base: content.content.kind === "base" }]`；任一不满足 → `undefined`。批里**有任何表写** → 直接 `undefined`。
- `adoptHeld()`（`#hold` 用）：逐 plan，`prepared.ops.length === 0` → `prepared.abort()` 并跳过；否则 `tracker.adopt(prepared)` 并产出 `{ type:"document", record, conversationId, version, value, ops }`。
- `takeHeld(getHold)`：**必须在"只剩 Storage 可能失败"之后调用**（它会清空 holds，早失败要保留 holds）。逐 plan：新建化身（`!("createdAt" in plan.record)`）跳过；`getHold(id)` 无 → 跳过；`storeHeld(hold, change?.prepared.ops ?? [], change?.prepared.value ?? hold.loaded.tracker.value, hold.loaded.valueVersion)`；本提交自己选中的 `base` 覆盖 held 的 content；`#writes` 里若有本提交的 content 写则**原位替换**，否则用 `#insertBeforeRetirement`（插到同 id 的 `document.retire` 之前，没有 retire 则 push）；`!plan.retire` 时记 `plan.held = { ops: stored.ops, observed: own !== undefined }`；conversation 作用域补 `plan.conversationId`。
- `adopt(seq)`（**Storage 持久化后的同步回调**）：逐 plan，`record = "createdAt" in plan.record ? plan.record : { ...plan.record, createdAt: seq }`，retire 再加 `retiredAt: seq`；有 change 时：新化身未在同批退休 → `tracker.adopt(prepared)`，否则 `prepared.abort()`（loaded 化身则 `prepared.ops.length > 0` 才 adopt）；`loaded.storedVersion < version` → 更新；`content` 为 base → `deltasSinceBase = 0`，delta → `++`；**新安装**的化身 → `#host.install({ addressId, record, storedVersion: version, valueVersion: version, deltasSinceBase: 0, tracker })`；`retire && committed` → `#host.evict(addressId, record.id)`。随后 `describe(plan, record)` 生成发布项：`plan.held === undefined` → 直接 push；否则 `stored` 数组才带 held 的 `ops`（`observed` 为真时也 push 到 `observed`）。返回 `{ observed, stored }`（`stored` 为 `undefined` 时语义上等于 `observed`——Session 用 `documents.stored === documents.observed` 判断）。
- `#assemble()` 内部顺序（**固定**）：`planDocument` 逐文档 → `#rejectForkSourceWrites` → `#validateOwners` → 对每个 `replace` 任务 `#committedTask(id)`：undefined → `Task ${id} does not exist`、terminal → `Task ${id} is already terminal`、`conversationId` 变 → `Task ${id} cannot change conversations`（三条都 **【逐字】**，注意与 `setTask()` 的 `already has a terminal candidate` 是**不同**文案）→ 终态任务退休其全部 task 作用域文档（`scanDocuments` 每页 256；本事务创建的任务跳过、本事务已计划退休的去重）→ 为发布解析 `plan.conversationId`（task 作用域取 `#currentTask` 的 conversationId）→ 处理 `#submissionChanges`（`storage.submission` 兜底，undefined → `Submission ${id} does not exist` **【逐字】**）→ 写 `#writes`（表写 → plans 的 content → retire）→ **`checkpointWhen` 最后逐个评估**（只对 `content.kind === "delta"` 且 `change.loaded` 的 plan；真则把 content 换成 `base`，`info = { deltasSinceBase: change.loaded.deltasSinceBase }`）。
- `#validateOwners`：收集 `what = "Conversation owner task"`（`#writes` 里带 owner 的 conversation）与 `"Task owner"`（`#tasksById` 里 `kind === "create"` 且带 owner 的任务），逐个 `#currentTask`：undefined → `${what} ${taskId} does not exist`、`terminal`/`completing` → `${what} ${taskId} is ${status}`、`abortRequested` → `${what} ${taskId} is abort-marked`。
- `checkpointWhen` 评估**最后**执行（`session-checkpoints-migrations.test.ts`：谓词抛错会让整批回滚；空批不调用；非空结构 no-op 会调用）。

**发布**：`publishes(plan)` 判断是否需要发布；`describe(plan, record)` 生成 `DocumentCommitChange`；`storeHeld(hold, ops, value, version)` 生成 `{ content, ops }`：
- 若 `hold.base` 或 `hold.pending` 超过 `MAX_PENDING_OPS`（阈值在 chord `PendingOps`，实现中常量）→ 生成 `{ kind: "base", version, value }`；
- 否则 `{ kind: "delta", version, ops: hold.pending.ops }`。

### 2.18 `src/session/session.ts`（1307 行）— Session 内核

（本节只列**不重复**于 §2.17 的 Session 专有语义；方法签名见 §2.4.2。）

#### 常量与工具
```ts
const DEFAULT_PERSIST_WITHIN_MS = 1_000;
let defaultRetention = 600_000;                     // 可被 setDefaultDocumentRetention 改写（测试用）
export function setDefaultDocumentRetention(ms: number): void;
export function documentRetention(ms: number | undefined): number;   // number && >=0 ? ms : defaultRetention
export function persistWithin(ms: number | undefined): number;       // number && >=0 ? ms : 1000
export const createSession: (storage, options?) => Session;
export class SessionImpl implements Session;
export function contained(callback: () => unknown, onError: (error: unknown) => void): void;

// **导出类型（原 spec 漏列）**
export type ObservedCommit =
  | { readonly kind: "stored"; readonly seq: Seq; readonly changes: readonly CommitChange[] }
  | { readonly kind: "memory"; readonly changes: readonly DocumentCommitChange[] };
```

`contained(callback, onError)` **逐字语义**（原 spec 只有签名）：`try { const result = callback(); if (typeof result?.then === "function") Promise.resolve(result).catch(onError); } catch (error) { onError(error); }`——**同步抛错与返回的 thenable 的拒绝都走 `onError`，且 `onError` 不被 await**（注释："Any thenable, also a promise from another realm; `Promise.resolve` adopts it with handlers of its own."）。Rust 侧对应 `catch_unwind` + `Future` 的 `.catch`，并必须保证 listener 抛错绝不影响提交结果。

`ObservedCommit`（**本文件唯一的导出类型，`src/index.ts` 未再导出它；`Session.observeCommits()` 的载荷类型**）：注释逐字说明"a stored commit with its own changes, or a deferred one adopted in memory only (`CommitPersistence`), which changes documents only. Memory is never behind: a later store of held changes is not observed again."——即 **memory 形态只可能出现在 deferred commit 上，且只有 `DocumentCommitChange`**（`kind: "memory"` 无 `seq`）。后续把 held 变更真正落盘时**不会再观察一次**。

**构造期容错包装（原 spec 缺失，三条都重要）**：`SessionImpl` 构造时把三个回调包成永不抛错的版本：
- `#clock = () => { try { return now(); } catch { return Date.now(); } }`；
- `#documentRetentionMs = () => { try { return documentRetention(documentRetentionMs()); } catch { return documentRetention(undefined); } }`；
- `#persistWithinMs = () => { try { return persistWithin(persistWithinMs()); } catch { return persistWithin(undefined); } }`。

源码注释："Cache bookkeeping must never change the outcome of the operation it follows: a clock or a retention setting that throws counts as now, and as the default."——对应测试 "a throwing clock does not change a commit's outcome" 与 "a non-millisecond retention counts as the default"。**注意 `#host.now` 仍是原始的（未包装）`now`**：`TransactionHost.now()` 直接传 `now`，因此任务时间戳路径上时钟抛错仍会传播（由调用方承担）。

`createSession(storage, options?)` 的 options：`{ now?: () => number; documentRetentionMs?: number; setTimer?: SetTimer; persistWithinMs?: number }`；实现里 `documentRetentionMs` 转成 `retention === undefined ? undefined : () => retention` 闭包、`persistWithinMs` 转成 `() => within` 闭包传给 `SessionImpl`（**构造期固定的常量，不是每提交读取**）。

`SessionImpl` 构造签名（**原 spec 把形参名字写错了，没有 `testsOnly`**）：
```ts
constructor(
  storage: Storage,
  now: () => number = Date.now,
  documentRetentionMs: () => number | undefined = () => undefined,
  setTimer?: SetTimer,
  readOnly = false,                                   // 第 5 参：HarnessOptions.readOnly，拒绝一切有写提交
  persistWithinMs: () => number | undefined = () => undefined,   // 第 6 参
)
```
`createSession` 固定传 `readOnly = false`。构造期还做三件事：`#failed.promise.catch(() => {})`（"Waits race it; none has to."）；`this.#storage = new GuardedStorage(storage, this)`；`this.#timers = hostTimers(setTimer, error => this.report(error, { type: "host_error", callback: "setTimer" }))`——**`setTimer` 抛错或拒绝会以 `{ type:"host_error", callback:"setTimer" }` 报告**（对应消息 `HarnessOptions.setTimer declined a timer that paces work; using setTimeout` 见 §3.6）。

#### 失败语义
- `fail(error)`：首次失败生效。置 `#failure`；`#failed.reject(new SessionFailed(error))`；`this.close(BACKGROUND_CONTEXT).catch(() => {})`；`report(error, { type: "session_failed" })`。
- `#failed.promise.catch(() => {})`（避免 unhandled rejection）。
- 任何 `assertUsable()`：失败 → `SessionFailed`；关闭中 → `Error("Session is closed")` **【逐字】**。

#### 缓存与保留
字段：`#documents: Map<addressId, LoadedDocument>`、`#idleDocuments: Map<addressId, number>`（最旧使用在前，JS Map 插入序）、`#attachedDocuments: Map<DocumentId, number>`、`#holds: Map<DocumentId, Hold>`、`#holdTimer?`、`#documentSweep?`、`#sweepJobs`、`#documentTimersUnavailable`。

`#touchDocument(addressId)`：
- 未缓存或已 attached → 返回。
- `now = clock()`；`since = #idleDocuments.get(addressId)`；
- **granule**：若 `since !== undefined && now - since < min(1_000, retention/10)` → 返回（热文档最多每个 granule 移到队尾一次；可能提前一个 granule 被逐出，代价是一次重载）。
- 否则 delete+set（移到队尾）。
- 若 `#documentSweep === undefined || #documentSweep.at > now + retention` → `#scheduleDocumentSweep()`。

`#scheduleDocumentSweep()`：
- 关闭中 → 返回；无 idle → 返回。
- `at = 最旧 idle 的 since + retention`；已有 sweep 且 `at <= 新 at` → 返回。
- 取消旧的；创建 `sweep = { at, disarm: undefined }`；**sweep 保持设置直到其 job 运行**，因此线忙时 touch 不会排队更多。
- `run()`：`#sweepJobs++`；`#enqueue(async () => { if (#documentSweep === sweep) #documentSweep = undefined; if (!closing) #sweepDocuments(); }).catch(()=>{})`。
- `delay = at - clock()`；`delay <= 0` → `queueMicrotask(run)`。
- `#documentTimersUnavailable` → 清 sweep 返回（改为每次提交结束时清扫）。
- `disarm = timers.background(run, delay)`；`undefined` → `#documentTimersUnavailable = true`，清 sweep。

`#sweepDocuments()`：`expired = clock() - retention`；按 idle 顺序遍历，`since > expired` break，否则 `#dropDocument`；最后 `#scheduleDocumentSweep()`。

`#dropDocument(addressId)`（**原 spec 只有一处提及，未给实现**）：`#documents.delete(addressId)` + `#idleDocuments.delete(addressId)`——**不动 `#attachedDocuments`**（pin 计数按 `DocumentId` 记，与地址无关；被 pin 的化身本来就不在 idle 队列里）。

`#pinDocument(addressId, recordId)`：`attached[recordId]++`；从 idle 删除；返回幂等 release（闭包里的 `pinned` 布尔保证只生效一次）：计数减到 0 时 `attached.delete(recordId)` 并（若 `#documents.get(addressId)?.record.id === recordId` 仍是当前占用者）`#touchDocument(addressId)`——**已被替换的旧化身不会因 unpin 被重新 touched**（对应测试 "does not retain a replaced incarnation for its observer"）。

`#release(holds)`：逐 hold `#holds.delete(hold.loaded.record.id)` + `hold.unpin()`；最后 `#armHoldTimer()`（把 timer 移到下一个最小 deadline，没有 hold 时 `at === Infinity` → 不设 timer）。

#### 提交（`#runCommit`）— **精确顺序**
1. `#assertHealthy()`；`context.abortSignal?.throwIfAborted()`。
2. `tx = new Transaction(#host, context, scope)`。
3. `result = await change(tx)`；随后再次 `#assertHealthy()`（回调吞掉了失败的读也必须不能提交）。
4. 回调抛错 → `await tx.settleFailure()`；`#sweepDocuments()`；rethrow。
5. `writes = await tx.settleSuccess()`；抛错 → `#sweepDocuments()`；rethrow。
6. `writes.length === 0` → `tx.discard()`；sweep；返回 result。
7. `#readOnly` → `tx.discard()`；sweep；抛 `new ReadOnlySession("a commit with writes")`。
8. `withinMs !== undefined`：`held = tx.holdable()`；非空 → `#hold(...)`；sweep；返回 result。
9. `taken = #holds.size === 0 ? NO_HOLDS : tx.takeHeld(id => #holds.get(id))`。
10. `adopt(committed)`：`try { adoption = { documents: tx.adopt(committed) } } catch (e) { adoption = { error: e } }`（回调不得抛到 Storage）。
11. `seq = await #storage.commit(writes, adopt, withoutAbortSignal(context))`；抛错时：若 `adoption === undefined` → `tx.discard()`；rethrow。
12. `adoption === undefined` → `Error("Storage committed without adopting the batch")` **【逐字】**；`this.fail(error)`；抛出。
13. `"error" in adoption` → `this.fail(adoption.error)`；抛出（Storage 已提交，adoption 失败 → 内存落后于磁盘）。
14. `taken.length > 0` → `#release(taken)`。
15. 对每个 `documents.stored` → `#touchDocument(addressId(record))`。
16. `#publish(seq, writes, documents, context)`。
17. `#sweepDocuments()`；返回 result。

#### `#hold(tx, held, withinMs, context)`
- `documents = tx.adoptHeld()`；`deadline = clock() + withinMs`。
- 对每个 `{ loaded, ops, base }`：`id = loaded.record.id`；无 hold 则 `unpin = #pinDocument(loaded.addressId, id)`，`hold = { loaded, pending: new PendingOps(), base: false, deadline, unpin }`；`hold.pending.push(ops)`；`base` 则 `hold.base = true`；`deadline < hold.deadline` 则更新。
- `#armHoldTimer()`；向内部监听器发布 `{ kind: "memory", changes: documents }`；监听器抛错 → `fail`。

#### `#storeHolds(holds, context)`
- 空 → 返回。
- 对每个 hold：`value = tracker.value`；`stored = storeHeld(hold, NO_OPERATIONS, value, version)`；push `document.change` 写（`content: stored.content`）；`changes.push({ type:"document", record, conversationId?, version, value, ops: stored.ops })`。
- `adopt` 中：`content.kind === "base"` → `deltasSinceBase = 0`，否则 `++`。
- `await #storage.commit(writes, adopt, withoutAbortSignal(context))`；未 adopt → 同上错误 + fail。
- `#release(holds)`；向 `#commitListeners` 发布 `{ seq, changes }`（逐个 `contained`，抛错只 report）。

#### `#armHoldTimer()`
- 关闭中 → 返回。取最小 deadline `at`。已有 timer 且（fired 或 `at` 相同）→ 返回。否则 disarm 旧的。
- `at === Infinity` → 返回。创建 `timer = { at, disarm: noop, fired: false }`；`run()`：`timer.fired = true`；`#enqueue(async () => { if (#holdTimer === timer) #holdTimer = undefined; if (closing || failure) return; await #storeHolds([...holds], BACKGROUND_CONTEXT) }).catch(()=>{})`。
- `timer.disarm = timers.foreground(run, max(0, at - clock()))`。

#### `#publish(seq, writes, documents, context)`
- 若 `#commitListeners.size > 0 || #internalListeners.size > 0` 才构造 `changes`；**两者都为空 → 立刻返回**（连 telemetry 都不发）。
- 逐 write（**switch 只认这四类**；`document.change`/`document.create`/`document.retire`/`document.copy` 不进 `changes`，文档变更只来自 `documents.observed`）：
  - `entry` → `context.telemetry.addEvent("kiana.durable.entry", { "entry.id": id, "entry.kind": kind })`；push。
  - `submission` 且状态为 `done`/`unanswered` → `addEvent("kiana.durable.submission", { "submission.id": id, status, "answer.entry_id": answer })`；**无论状态**都 push（`queued`/`placed` 也 push，只是不发 telemetry）。
  - `conversation`/`task` → push。
- `tables = changes.length`；append `documents.observed`；`observed = { kind:"stored", seq, changes }`（**`observed` 与 `changes` 是同一个数组对象**，后续 `stored` 复用其前缀）。
- `stored = documents.stored === documents.observed ? observed : { seq, changes: [...changes.slice(0, tables), ...documents.stored] }`。
- 先内部监听器（抛错 → `fail`），再宿主监听器（抛错 → `report`）。两侧都在**本次提交已持久化之后**同步调用；监听器抛错**不会**让 `#runCommit` 抛出。

#### `#attachDocument(definition, loaded)`
- `checkRecordScope(definition, loaded.record)`/`checkRecordVersion(definition, loaded.record, loaded.storedVersion)`（**注意用的是 `storedVersion`，不是 `valueVersion`**）。
- `let unsubscribeCommit = () => {}` / `let unsubscribeClose = () => {}` 先占位；`unpin = #pinDocument(loaded.addressId, loaded.record.id)`；`detach = () => { unsubscribeCommit(); unsubscribeClose(); unpin(); }`（**detach 只 unpin 一次，靠 `#pinDocument` 的幂等 release**）。
- `observer = new Watch(loaded.tracker.value, detach, error => this.report(error))`。
- 订阅 `observeCommits`：只处理 `change.type === "document" && change.record.id === loaded.record.id`：
  - `change.value === null`（退休）→ `unpin()`；`observer.retire(context)`；continue。
  - `ops = observedOperations(observed, change)`：若 `change.version === observed.version` 用 `change.ops`，否则更新 `observed.version` 并返回 `[["r", change.value]]`（根替换）。
  - `ops.length === 0`（迁移专用 base）→ continue。
  - `observer.advance(change.value, ops, context)`。
- 订阅 close：`unsubscribeClose = this.subscribeClose(() => observer.end(this.#failure))`；返回 `observer`。
- 注意：**被退休的化身不再 pin**（先 `unpin()` 再 `observer.retire(context)`），因此退休后的 observer 不会阻止其缓存条目过期。

#### `#loadDocument(definition, addressId, address, context)`
- 缓存命中且 `valueVersion === definition.version` → touch 并返回。
- 有缓存但版本不符：若有 held → `await #storeHolds([hold], context)`；然后 `#dropDocument`。
- `record = await #storage.findDocument(address, "current", context)`；undefined → undefined。
- `stored = await #storage.document(record.id, "current", context)`；undefined → `Error("Current document ${id} (${kind}) cannot be read")` **【逐字】**。
- `value = materializeDocument(definition, stored)`；构造 `LoadedDocument`（`storedVersion: stored.version`、`valueVersion: definition.version`、`deltasSinceBase`、`tracker: track(value)`）；install + touch。

#### `#enqueue(job)`
```ts
const run = this.#tail.then(job);
this.#tail = run.then(() => undefined, () => undefined);
return run;
```
**这就是"单一变更线"**：任务按入队顺序串行；前一个失败不阻断后续（tail 吞错）。

#### `observedOperations(observed, change)`（模块级）
```ts
if (change.version === observed.version) return change.ops;
observed.version = change.version!;
return [["r", change.value]];
```

#### `cancellationError(signal)`
`signal.reason instanceof Error ? signal.reason : new DOMException("The operation was aborted", "AbortError")`。

#### `GuardedStorage`（Storage 包装）
- 字段：`#storage`、`#latch`、`#underway: Set<Promise>`、`#closing`。
- 每个方法 `#call(readContext, run)`。
- `close`：`#closing = true`；`await Promise.allSettled([...#underway])`；`#storage.close(context)`。
- `#call(read, run)`：
  1. `#assertHealthy()`；`#closing` → `Error("Session is closed")`。
  2. `call = (async () => run())()`（包装同步抛错）。
  3. `#underway.add(call)`；`try { result = await call } catch (e) { #assertHealthy(); exempt = read !== undefined && (e instanceof StorageRequestError || read.abortSignal?.aborted === true); if (!exempt) #latch.fail(e); throw e } finally { #underway.delete(call) }`。
  4. 成功后再次 `#assertHealthy()`（若期间他人失败，本调用以 `SessionFailed` 结束）。
- `commit`/`mintId` 传 `read = undefined` → **永不豁免**（批已接纳则是否提交未知）。

#### `SessionImpl` 生命周期
- `close(context)`：幂等（`#closing` 缓存）；`#notifyClose()` 同步执行一次（disarm sweep/hold timer，逐个 `contained` 调用 close listeners）；链路：
  1. `beforeClose()`（子类钩子）；
  2. `#enqueue`：
     - 若未失败且有 holds → `#storeHolds(...)`（失败被捕获到 `unstored`）；
     - 清空 `#commitListeners`/`#internalListeners`/`#documents`/`#idleDocuments`/`#attachedDocuments`/`#holds`；
     - `await #storage.close(cleanup)`；抛错时若未失败则置失败并 report，再抛出；
     - 若有 `unstored` 则抛出它。
  3. `finally`：`#closed.resolve(failure === undefined ? { reason:"closed" } : { reason:"failed", error })`。
- `writeAtClose(writes)`：`#enqueue`：未失败 → `batch = writes()`；非空 → `#storage.commit(batch, () => {}, BACKGROUND_CONTEXT)`（不发布）。
- `unloadDocuments()`：`#enqueue`：未失败先 `#storeHolds(all)`；清 `#documents`/`#idleDocuments`。
- `documentCacheSizes`：`{ cached, idle, attached, sweeps, held }`。
- `readOnLine(job)`：`#enqueue` 内 `#assertHealthy()` 后执行。
- `conversationDocumentOnLine(token, conversationId, context)`：`resolveAddress(definition, [conversationId, context])`（注意把 context 作为第二个参数——`nextArgument` 会跳过它）→ load → `checkRecordScope`/`checkRecordVersion` → `{ record, version: valueVersion, value: tracker.value }`。
- `snapshot(...)`：先查缓存且版本一致则 touch；否则 `#enqueue(#loadDocument)`；返回 `loaded.tracker.value`（**只读**，不拷贝）。
- `watchDoc(...)`：先注册 abort 监听（`cancelled` 标志）；`#enqueue` 内检查 cancelled → `cancellationError`；load 后若 cancelled → `watch.cancel()` 并抛；否则 `signal` 存在时 `watch.observeCancellation(signal)`。
- `snapshotAsOf(...)`：`resolved.address.scope.kind !== "conversation"` → `TypeError("Session.snapshotAsOf() requires a conversation document")`；`at` 非 safe integer → `TypeError("Session.snapshotAsOf() requires an entry ID")`；`#enqueue` 内：`storage.entry(conversationId, at, context)` 不存在 → `Error("Entry ${at} is not visible from conversation ${conversationId}")`；`findDocument(addressWithEntryConversation, entry.commitSeq)`；`storage.document(record.id, commitSeq)` 不存在 → `Error("Historical document ${record.id} (${record.kind}) cannot be read")`；`materializeDocument`。
- `subscribeCommits`/`observeCommits`/`subscribeClose`：都先 `assertUsable()`，返回退订函数。
- `report(...)`：基类 no-op（Harness 覆写）。
- `conversationCreated(...)`：基类 `Promise.resolve()`。
- `beforeClose()`：基类 `Promise.resolve()`。
- `failure` / `failed` getter：供 Harness 使用。

### 2.19 `src/session/forks.ts`（97 行）

```ts
const SCAN_PAGE_SIZE = 256;
export type ForkDocumentCopy = { readonly record: DocumentCreate; readonly source: DocumentCopySource };
export async function prepareForkDocumentCopies(
  storage: Storage,
  parentConversationId: ConversationId,
  at: EntryId,
  childConversationId: ConversationId,
  context: Context,
): Promise<readonly ForkDocumentCopy[]>;
```

行为：
1. `storage.entry(parentConversationId, at, context)`；undefined → `Error("Entry ${at} is not visible from conversation ${parentConversationId}")` **【逐字】**。
2. **固定两轮 `collectCopies`，两轮都会执行**（不是在 `fork` 上分支）：
   - **asOf 轮**：scope 为 **`{ kind:"conversation", conversationId: entry.entry.conversationId }`**（**条目所属的祖先会话，不一定是 `parentConversationId`**——条目可能是继承来的父会话条目），`at = entry.commitSeq`（`DocumentPoint`，不是 entry id），policy `"asOf"`；
   - **current 轮**：scope 为 `{ kind:"conversation", conversationId: parentConversationId }`，`at = "current"`，policy `"current"`。
   - 每轮按 `storage.scanDocuments({ scope, at }, SCAN_PAGE_SIZE /* 256 */, cursor, context)` 分页，直到 `page.next === undefined`；循环体内 `source.scope.kind !== "conversation" || source.fork !== policy` → `continue`（**过滤在循环内，两轮扫描覆盖全部 conversation 文档**，`fork === "initial"` 的文档被两轮都跳过，子会话中为 absent）。
3. 对每个命中：
   - `id = await storage.mintId<DocumentId>()`（**每个源文档各铸造一个新 ID**，不是批量/复用）；
   - `identity = { id, kind: source.kind, ...(source.key === undefined ? {} : { key: source.key }), scope: { kind:"conversation", conversationId: childConversationId } }`；
   - `record = source.history === "latest" ? { ...identity, history: "latest", fork: source.fork } : { ...identity, history: "rewindable", fork: source.fork }`（**`history` 只可能是 `"latest" | "rewindable"`；`fork` 保留源策略**；`key` 缺省时**不写该键**）；
   - `source = { id: source.id, at }`，其中 `at` 是该轮的 `DocumentPoint`（asOf 轮为 `entry.commitSeq`，current 轮为 `"current"`）；
   - 以 `addressId(record)` 去重（`copiedAddresses`）：重复 → `member = record.key === undefined ? record.kind : \`${record.kind}/${record.key}\``，`Error(\`Fork selects multiple source documents for ${member}\`)` **【逐字】**。
4. 返回 `readonly ForkDocumentCopy[]`（顺序：先 asOf 轮全部，再 current 轮全部）。

**注意**：`document.copy` 的解析发生在 Storage 层（memory/sqlite/indexeddb/jsonl 经由 MemoryStorage），因此 `prepareForkDocumentCopies` 只负责选择源与目标地址。**`document.copy` 是内部的 `StorageWrite` 类型，不会落进 `main.jsonl` 的 marker**（`encodeCommit` 的 `switch` 只处理 conversation/entry/submission/document.retire/task/document.create/document.change 七种写——`document.copy` 在 Transaction 装配阶段已被展开为 `document.create` + Storage 内部的 copy 解析，见 §2.14 与 §4.1.1）。

---

### 2.20 `src/env/index.ts`（377 行）— 执行环境契约

#### 结果类型
```ts
export type Result<T, E> = { ok: true; value: T } | { ok: false; error: E };
export function ok<T, E>(value: T): Result<T, E>;
export function err<T, E>(error: E): Result<T, E>;
export function getOrThrow<T, E>(result: Result<T, E>): T;              // 不 ok 则 throw error
export function getOrUndefined<T extends object, E>(result): T | undefined;
export function toError(error: unknown): Error;                          // Error → 原样；string → new Error(string)；否则 JSON.stringify 或 String
```

#### `FileError`
```ts
export type FileKind = "file" | "directory" | "symlink";
export type FileErrorCode = "aborted" | "not_found" | "permission_denied" | "not_directory" | "is_directory" | "invalid" | "not_supported" | "unknown";
export class FileError extends Error {
  code: FileErrorCode;
  path?: string;
  constructor(code, message, path?, cause?)    // name = "FileError"
}
export type ExecutionErrorCode = "aborted" | "timeout" | "shell_unavailable" | "spawn_error" | "callback_error" | "unknown";
export class ExecutionError extends Error {
  code: ExecutionErrorCode;
  spillPath?: string;                          // 超时/中止且输出超过 spill 阈值时
  constructor(code, message, cause?)           // name = "ExecutionError"
}
```

#### 基础结构
```ts
export interface FileInfo { name: string; path: string; kind: FileKind; size: number; mtimeMs: number }
export interface TextLine { text: string; terminated: boolean }
export interface TextLineReader { readLine(context): Promise<Result<TextLine | undefined, FileError>>; close(context): Promise<void> }
export interface BinaryReader {
  info(context): Promise<Result<FileInfo, FileError>>;
  read(offset: number, length: number, context): Promise<Result<Uint8Array, FileError>>;
  scanLines(options: { startLine: number; endLine?: number }, context): Promise<Result<LineScan, FileError>>;
  close(context): Promise<void>;
}
```
`BinaryReader` 语义（逐字要点）：位置读基于同一打开的文件（路径被重命名也不变）；`read` 只在 EOF 时少于 `length`；`scanLines` 一次遍历定位 `[startLine, endLine)`（0 基，`endLine` 缺省到末尾，第 k 行在第 k 个换行字节之后开始）；解码尺寸与 `new TextDecoder().decode(file)` 对该范围的输出一致，**开头 BOM 不计入**。

#### 监视
```ts
export interface WatchTarget { path: string; recursive?: boolean; exclude?: { hidden?: boolean; names?: readonly string[] } }
export type WatchChange = { paths: string[] } | { overflow: true } | { error: FileError };
export interface FileWatcher { readonly mode: "native" | "polling"; close(context): Promise<void> }
```
语义：
- `paths`：每个路径（或其下）可能变化；目录路径覆盖整棵子树；调用可**虚假**（spurious），但健康期内**不会漏报**。
- `overflow`：一段时间内覆盖不确定（丢事件/重连）→ 全量重扫。
- `error`：watcher 停止（如树超出环境上限）；此后不再有调用。
- `watch()` 返回 watcher 即表示**覆盖已建立**（先 watch 再 load 不会漏掉加载期间的变化）。
- `mode`：`native` 约 2 秒内报告；`polling` 通过快照比较（网络/FUSE 文件系统不可靠），两次快照间被撤销的变化可能漏掉。

#### `LineScan`
```ts
export interface LineScan {
  newlines: number;        // 全文件换行字节数；共 newlines+1 行
  start: number;           // 选中行字节范围起点（第一行行首）
  end: number;             // 终点（最后一行行尾，不含结尾换行）
  firstLineEnd: number;    // 第一选中行的结尾（其换行处或文件末尾）
  lastLineStart: number;   // 最后选中行的起点
  selectedBytes: number;   // 选中范围解码后的 UTF-8 字节长度
  firstLineBytes: number;  // 第一行解码后的 UTF-8 字节长度
}
```
边界情形：越界选择在文件末尾处为空（`start === end === size`）。

#### `DirReader`
```ts
export interface DirReader {
  next(maxEntries: number, context): Promise<Result<{ entries: FileInfo[]; done: boolean }, FileError>>;
  close(context): Promise<void>;
}
```
语义：按文件系统返回顺序分页；`done` 标记结束（可能与最后一批一起或单独空页返回）；元数据读取前消失的条目被跳过，不支持类型也跳过；失败/中止后应 close。

#### `ListFilesOptions` / `FileListing`
```ts
export interface ListFilesOptions {
  hidden?: boolean;        // 默认 true（Windows 上还包括隐藏属性文件）；名为 .git 的条目**永不**列出
  gitignore?: boolean;     // 默认 true；在 git 工作树内跳过被忽略项（含父目录 .gitignore、.git/info/exclude、全局 excludes）。
                            // 与 `git ls-files` 不同：被跟踪但匹配忽略规则的文件也会跳过。非工作树内 .gitignore 无效。
  exclude?: readonly string[];   // 连同其下全部内容跳过的名字，如 node_modules
  maxEntries?: number;           // 默认 50_000
  maxBytes?: number;             // 默认 4 MiB，最多 15.9 MiB（每个条目的 UTF-8 路径 + 2）
  timeoutMs?: number;            // 默认 10_000
}
export interface FileListEntry { path: string; kind: FileKind }   // path 相对、始终 "/" 分隔；符号链接按 symlink 列出不跟随
export interface FileListing { entries: FileListEntry[]; truncated: boolean }  // 深度优先、父先于子、同级按名字字节序
```

#### `FileSystem`（完整接口，逐字见源码）
```ts
export interface FileSystem {
  readonly id: string;     // 文件命名空间：相同 id 在相同路径看到相同文件（与 cwd 无关）；本地 Node 环境共享 "node:local"
  cwd: string;
  absolutePath(path, context): Promise<Result<string, FileError>>;
  joinPath(parts: string[], context): Promise<Result<string, FileError>>;
  readTextFile(path, context): Promise<Result<string, FileError>>;
  openTextLineReader(path, context): Promise<Result<TextLineReader, FileError>>;
  readTextLines(path, options: { maxLines?: number } | undefined, context): Promise<Result<string[], FileError>>;
  readBinaryFile(path, context): Promise<Result<Uint8Array, FileError>>;
  openBinaryReader(path, options: { noFollow?: boolean } | undefined, context): Promise<Result<BinaryReader, FileError>>;
  writeFile(path, content: string | Uint8Array, context): Promise<Result<void, FileError>>;
  appendFile(path, content: string | Uint8Array, context): Promise<Result<void, FileError>>;
  truncateFile(path, size: number, context): Promise<Result<void, FileError>>;
  flushFile(path, context): Promise<Result<void, FileError>>;
  renameFile(source, destination, context): Promise<Result<void, FileError>>;
  fileInfo(path, context): Promise<Result<FileInfo, FileError>>;
  listDir(path, context): Promise<Result<FileInfo[], FileError>>;
  listFiles?(path, options, context): Promise<Result<FileListing, FileError>>;   // 可选
  openDirReader(path, context): Promise<Result<DirReader, FileError>>;
  watch(targets, onChange, context): Promise<Result<FileWatcher, FileError>>;
  canonicalPath(path, context): Promise<Result<string, FileError>>;
  exists(path, context): Promise<Result<boolean, FileError>>;
  createDir(path, options: { recursive?: boolean } | undefined, context): Promise<Result<void, FileError>>;
  remove(path, options: { recursive?: boolean; force?: boolean } | undefined, context): Promise<Result<void, FileError>>;
  createTempDir(prefix: string | undefined, context): Promise<Result<string, FileError>>;
  createTempFile(options: { prefix?: string; suffix?: string } | undefined, context): Promise<Result<string, FileError>>;
  cleanup(context): Promise<void>;
}
```
`openBinaryReader` 语义：目录 → `is_directory`；其他非常规文件 → `invalid`；`noFollow` 时最终组件为符号链接 → `invalid`（前面的组件仍会解析）。

#### Shell
```ts
export interface ShellSpillOptions { afterBytes: number; afterLines: number }   // 超过任一阈值即把完整输出落盘
export interface ShellExecResult { exitCode: number; spillPath?: string }
export interface ShellExecOptions {
  cwd?: string;
  env?: Record<string, string>;
  inheritEnv?: boolean;
  timeout?: number;                        // 秒
  onOutput?: (text: string, context: Context, info: ShellOutputInfo) => void;
  spill?: ShellSpillOptions;
  window?: ShellOutputWindow;
}
export interface ShellOutputWindow { maxBytes: number; maxLines: number; minIntervalMs: number; bytesPerSecond: number }
export interface ShellOutputSkip { bytes: number; newlines: number; endsWithNewline: boolean }
export interface ShellOutputInfo { stream: "stdout" | "stderr"; skipped?: ShellOutputSkip }
export interface Shell {
  exec(command: string | readonly string[], options: ShellExecOptions | undefined, context): Promise<Result<ShellExecResult, ExecutionError>>;
  cleanup(context): Promise<void>;
}
export interface ExecutionEnv extends FileSystem, Shell {}
```
语义要点：
- `command` 为字符串 → 经环境 shell；为数组 → 直接执行 `command[0]`，其余作为参数**不经 shell**。
- 中止 context 或超时**只杀本命令的进程**。
- `onOutput`：stdout/stderr 的每个解码块按到达顺序、带流标识；**原始、无界、不限速**；两个流**分别解码**，跨块的字符不会损坏。
- `window`：调用方只保留尾部，环境可省略窗口外输出并以 `info.skipped` 报告；省略段与随后块可能跨两个流；`stream` 报告该块的最后一个流。
- `skipped` 度量：按 `onOutput` 会收到的解码文本计；每个 U+FFFD 计 3 字节，不做净化。
- `ShellOutputInfo.skipped` 与块的关系：块包含省略之后到其末尾的全部输出，且**超出窗口至少 1 字节或 1 行**，因此省略段绝不落在保留尾部内。

再导出：`rangeDecoder, StreamDecoder, startsWithBom`（`./decode.ts`）、`LineScanner`（`./line-scan.ts`）。

### 2.21 `src/env/decode.ts`（30 行）

```ts
export function rangeDecoder(): TextDecoder;        // new TextDecoder("utf-8", { ignoreBOM: true })
export function startsWithBom(firstBytes: Uint8Array): boolean;   // 0xef 0xbb 0xbf
export class StreamDecoder {
  decode(bytes?: Uint8Array): string;
}
```
`StreamDecoder.decode`：
- 无参 → 结束流（flush，可能产出"待决的高位代理"对应字符）。
- 首个非空文本若以 `U+FEFF` 开头 → 去掉一个（因为 `ignoreBOM: true` 时 Node 会把**任何**位置（不只开头）的 U+FEFF 保留，需要手工丢弃流首的 BOM）。
- 之后 `#started = true`，不再丢弃。
- 保证：任意分块解码的结果 == 一次性解码整个流。

**Rust**：`encoding_rs::UTF_8` 的 `Decoder` 流式接口即可；注意 `ignoreBOM` 语义（我们要自己去掉流首的 `EF BB BF`），以及非法字节 → U+FFFD（与 `TextDecoder` 非 fatal 模式一致；JSONL 用的是 fatal 模式）。

### 2.22 `src/env/line-scan.ts`（145 行）

```ts
export class LineScanner {
  constructor(startLine: number, endLine?: number);   // endLine 缺省 +Infinity
  push(chunk: Uint8Array): void;
  finish(): LineScan;
}
```

构造校验：
- `!Number.isSafeInteger(startLine) || startLine < 0 || !(endLine > startLine)` → `RangeError("Invalid line range")`。
- `endLine !== Infinity && !Number.isSafeInteger(endLine)` → 同上。
- `startLine === 0` 时立即 `#begin(0)`。

内部状态：`#position`、`#newlines`、`#lineStart`、`#start?`、`#end?`、`#firstLineEnd?`、`#lastLineStart?`、`#selectedBytes`、`#firstLineBytes`、`#selection?`、`#firstLine?`（两个 `TextDecoder`）、`#head: number[] | undefined = []`、`#bom`。

`push(chunk)`：
1. 若 `#head !== undefined`：取 `min(3 - head.length, chunk.length)` 字节入 head；未满 3 → 返回；满则 `#releaseHead()`，`chunk = chunk.subarray(take)`。
2. `#process(chunk)`。

`#releaseHead()`：`head = Uint8Array.from(this.#head)`；`this.#head = undefined`；`this.#bom = startsWithBom(head)`；`#process(head)`。
**注意顺序**：先处理已缓冲的 3 字节，再处理剩余（`push` 中顺序为 releaseHead 后 `#process(剩余)`；但 `#process(head)` 已在 releaseHead 内执行）。这保证 BOM 检测先于正文。

`#process(chunk)` 核心循环（**精确**）：
```
base = #position
from = 0
for (index = chunk.indexOf(0x0a); index !== -1; index = chunk.indexOf(0x0a, index+1)) {
  #feed(chunk, base, from, index)                 // 换行前的部分（属于仍打开的范围）
  line = #newlines
  position = base + index
  if (line === #startLine) #endFirstLine(position)
  if (line === #endLine - 1) #endSelection(position)
  #feed(chunk, base, index, index+1)              // 换行字节本身
  from = index + 1
  #newlines++
  #lineStart = position + 1
  if (#newlines === #startLine) #begin(#lineStart)
  if (#newlines === #endLine - 1) #lastLineStart = #lineStart
}
#feed(chunk, base, from, chunk.length)
#position += chunk.length
```

`finish()`：
- 若 `#head !== undefined` → `#releaseHead()`。
- `size = #position`。
- `#start === undefined` → 返回 `{ newlines, start: size, end: size, firstLineEnd: size, lastLineStart: size, selectedBytes: 0, firstLineBytes: 0 }`。
- `#firstLineEnd === undefined` → `#endFirstLine(size)`；`#end === undefined` → `#endSelection(size)`。
- 返回 `{ newlines, start: #start, end: #end ?? size, firstLineEnd: #firstLineEnd ?? size, lastLineStart: #lastLineStart ?? #lineStart, selectedBytes, firstLineBytes }`。

`#begin(start)`：`#start = start`；若 `#startLine === #endLine - 1` 则 `#lastLineStart = start`；创建两个 `rangeDecoder()`（selection、firstLine）。
`#endFirstLine(position)`：`#firstLineEnd = position`；flush firstLine 的**剩余**字节并累加 `decodedBytes(firstLine.decode())`；置 undefined。
`#endSelection(position)`：`#end = position`；flush selection 并累加；置 undefined。
`#feed(chunk, base, from, to)`：
- **BOM 跳过**：若 `#bom && base + from < 3` → `from = min(to, 3 - base)`（解码整个文件会丢掉开头的 BOM）。
- `to <= from` → 返回。
- `bytes = chunk.subarray(from, to)`；各自 `decodedBytes(decoder.decode(bytes, {stream:true}))` 累加到 `selectedBytes`/`firstLineBytes`。

`decodedBytes(text)`：`text === "" ? 0 : new TextEncoder().encode(text).length`。

**Rust**：按字节流实现同一状态机；`TextEncoder().encode(text).length` 等于字符串的 UTF-8 字节长度（含 U+FFFD 各 3 字节）。

### 2.23 `src/env/node.ts`（1235 行）— Node 执行环境

#### 常量
```ts
const MAX_TIMEOUT_MS = 2_147_483_647;
const MAX_TIMEOUT_SECONDS = MAX_TIMEOUT_MS / 1000;    // 2147483.647
const EXIT_STDIO_GRACE_MS = 100;
const SPILL_HIGH_WATER_MARK = 1024 * 1024;            // 1 MiB
const BINARY_READ_CHUNK = 1024 * 1024;                // 1 MiB
```

#### `resolveTimeoutMs(timeout?: number): Result<number | undefined, ExecutionError>`
- undefined → ok(undefined)。
**`resolveTimeoutMs(timeout)` 逐字**（`env/node.ts` L59-70）：
```ts
if (timeout === undefined) return ok(undefined);
if (!Number.isFinite(timeout) || timeout <= 0)
    return err(new ExecutionError("timeout", "Invalid timeout: must be a finite number of seconds"));
const timeoutMs = timeout * 1000;
if (timeoutMs > MAX_TIMEOUT_MS)
    return err(new ExecutionError("timeout", `Invalid timeout: maximum is ${MAX_TIMEOUT_SECONDS} seconds`));
return ok(timeoutMs);
```
`MAX_TIMEOUT_MS = 2_147_483_647`，`MAX_TIMEOUT_SECONDS = MAX_TIMEOUT_MS / 1000 = 2147483.647`。**错误码是 `"timeout"`**（不是 `"unknown"`），且 `timeout <= 0` 也报"must be a finite number of seconds"。
- `timeout <= 0` → 同上错误。
- `timeout > MAX_TIMEOUT_SECONDS` → `Invalid timeout: maximum is ${MAX_TIMEOUT_SECONDS} seconds`。
- 否则 **`timeout * 1000`**（毫秒，**不做 `Math.floor`/取整**；源码直接 `const timeoutMs = timeout * 1000; return ok(timeoutMs)`，小数秒会得到小数毫秒，例如 `0.0015` → `1.5`）。

#### `resolvePath(cwd, path)`
- `path === "~"` 或以 `~/` 开头 → `os.homedir()` + 余下部分。
- 以 `file://` 开头 → `fileURLToPath`。
- 否则 `path.resolve(cwd, path)`。

#### `fileKindFromStats(stats)` / `fileInfoFromStats`
- `isFile()` → `"file"`；`isDirectory()` → `"directory"`；`isSymbolicLink()` → `"symlink"`；其他 → 抛 `Unsupported file type`（`FileError("invalid", ...)`）。
- `FileInfo = { name: basename(path), path, kind, size: stats.size, mtimeMs: stats.mtimeMs }`。

#### `toFileError(error, fallbackPath?)`
- 已是 `FileError` → 原样返回（`if (error instanceof FileError) return error`）。
- `path`：优先 `nodeError.path`（字符串时），否则 `fallbackPath`；`cause = toError(error)`。
- errno 映射（**源码只有这 6 个 case，没有 ENOSYS/ENOTSUP/EOPNOTSUPP 分支**）：
  - `ABORT_ERR` → `aborted`
  - `ENOENT` → `not_found`
  - `EACCES` / `EPERM` → `permission_denied`
  - `ENOTDIR` → `not_directory`
  - `EISDIR` → `is_directory`
  - `EINVAL` → `invalid`
  - 其他一律 → `unknown`（含 `ENOSYS`/`ENOTSUP`/`EOPNOTSUPP`/`ELOOP`/`ENAMETOOLONG` 等；`ELOOP`/`EMLINK` 只在 `openBinaryReader` 的 `noFollow` 路径上被单独改写为 `symlinkRefused`，见下）。
- 构造 `FileError(code, message, path, cause)`，`message` 取 errno 错误的 `message`；`unknown` 分支取 `cause.message`。

#### `abortResult(signal, path?)`：`signal?.aborted` → `err(new FileError("aborted", "aborted", path))`。

#### `runCommand(...)`
以 `child_process.spawn` 启动（`detached: true` 在 POSIX 上以创建进程组），返回 stdout/stderr/exitCode；`killProcessTree(pid)` 在 POSIX 用 `process.kill(-pid, "SIGKILL")`（进程组），Windows 用 `taskkill /pid <pid> /T /F`。

#### `findBashOnPath()`：`which bash`（Windows `where bash`），5000 ms 超时。

#### `isLegacyWslBashPath(path)`：判断是否 `System32\bash.exe`（WSL 旧版），如同时存在优先选 Git Bash。

#### `getBashShellConfig(shell)` / `getShellConfig(customShellPath?)`
`ShellConfig = { shell: string; args: string[]; commandTransport?: "argv" | "stdin" }`。
`getBashShellConfig(shell)`：`isLegacyWslBashPath(shell)` → `{ shell, args: ["-s"], commandTransport: "stdin" }`（旧版 WSL 的 bash 从 stdin 读脚本）；否则 `{ shell, args: ["-c"] }`。
- 若给定 `customShellPath`：`pathExists` 为真 → `getBashShellConfig(customShellPath)`；否则 → `ExecutionError("shell_unavailable", \`Custom shell path not found: ${customShellPath}\`)`（**逐字**）。
- Windows：候选顺序**逐字**为 `process.env.ProgramFiles + "\\Git\\bin\\bash.exe"`、`process.env["ProgramFiles(x86)"] + "\\Git\\bin\\bash.exe"`（各自仅在环境变量存在时），然后 `where bash.exe`（5 秒超时）找到的 PATH 上的 bash；都找不到 →
  **【逐字】**：
  ```
  No bash shell found. Options:
    1. Install Git for Windows: https://git-scm.com/download/win
    2. Add your bash to PATH (Cygwin, MSYS2, etc.)
    3. Configure an explicit shellPath

  Searched Git Bash in:
  <候选路径逐行>
  ```
- 非 Windows：若 `/bin/bash` 存在 → 用它；否则 PATH 上的 `bash`；否则 `ok({ shell: "sh", args: ["-c"] })` 回退（**是 `ok`，不是错误**）。
- `findBashOnPath()`：`where bash.exe`（Windows）/ `which bash`（其他），超时 5000 ms；`status !== 0` 或 stdout 为空 → null；取 stdout **第一行**并 `pathExists` 校验，否则 null。
- `isLegacyWslBashPath(path)`：`win32.normalize(path).toLowerCase()`；若设了 `process.env.SystemRoot`，先要求它是盘符路径（`/^[a-z]:[\\/]/i`，否则直接 false），再与 `${SystemRoot}\System32\bash.exe`、`${SystemRoot}\Sysnative\bash.exe` 比较；**未设 SystemRoot 时**才回退到正则 `/^[a-z]:\\windows\\(?:system32|sysnative)\\bash\.exe$/`。

#### `getShellEnv(baseEnv, extraEnv, inheritEnv)`
**签名是三参数**（源码 `fn getShellEnv(baseEnv?, extraEnv?, inheritEnv = true)`）：
- `inheritEnv === false` → `{ ...extraEnv }`；
- 否则 → `{ ...process.env, ...baseEnv, ...extraEnv }`（`process.env` < 构造时的 `shellEnv` < 每次调用的 `options.env`）。
- `exec` 中调用为 `getShellEnv(this.shellEnv, options?.env, options?.inheritEnv)`（`inheritEnv !== false` 即默认继承）。

#### `waitForChildProcess(child, ...)`
- 跟踪 stdout/stderr 的 `end`/`close`；进程退出后再等 `EXIT_STDIO_GRACE_MS`（100 ms）确保 stdio 排空；超时/中止会 kill。

#### `NodeTextLineReader`
- 字段：`file`（`openFile` 句柄）、`path`、`decoder = new StreamDecoder()`、`chunk = new Uint8Array(64 * 1024)`、`byteOffset = 0`、`buffered = ""`、`ended`、`closed`。
- `readLine(context)`：
  1. 先 `abortResult`；已 `closed` → `FileError("invalid", "Text line reader is closed", this.path)` **【逐字】**。
  2. 循环：缓冲里有 `\n` → 切出 `text`（**不含** `\n`，保留 `\r`），返回 `{ text, terminated: true }`；`ended` 且缓冲空 → `ok(undefined)`；`ended` 且有残余 → 返回 `{ text, terminated: false }`。
  3. 否则 `file.read(chunk, 0, 64 KiB, byteOffset)`（**显式位置**，注释逐字："Explicit positions allow an aborted read to be retried without skipping bytes."）；读后**再查一次 abort**；`byteOffset += bytesRead`；`bytesRead === 0` → `buffered += decoder.decode()`（flush）并 `ended = true`，否则 `buffered += decoder.decode(chunk.subarray(0, bytesRead))`。
  4. 任何异常 → `toFileError(error, this.path)`。
- 跨 chunk 的多字节字符与超长行（大于一个 chunk）必须正确。
- `close`：幂等；置 `closed`、清空 `buffered`、`file.close()` **吞掉所有异常**（注释：closing is best-effort，含取消后或早前 I/O 失败之后）。

#### `NodeBinaryReader`
- `info()`：`fs.promises.fstat` 得到打开文件（不是当前路径）的元数据。
- 已关闭 → `FileError("invalid", "Binary reader is closed", path)` **【逐字】**（模块级 `closedResult(what, path)` = `` err(new FileError("invalid", `${what} is closed`, path)) ``，`info`/`read`/`scanLines` 三处都用它）。
- `read(offset, length)`：`Offset and length must be non-negative safe integers` **【逐字】**（`FileError("invalid", ...)`）；用 `BINARY_READ_CHUNK`（1 MiB）循环 `file.read(chunk, 0, min(length-total, 1 MiB), offset+total)`，每轮后查 abort；`bytesRead === 0` 提前结束；单块且满 → 直接返回该块（`subarray` 到 `bytesRead` 才 push），多块 → 拼成一个 `Uint8Array(total)`。
- `scanLines({startLine, endLine})`：以 **64 KiB**（源码里是局部 `const chunk = new Uint8Array(64 * 1024)`，**不是** `BINARY_READ_CHUNK`）分块喂 `LineScanner`；`LineScanner` 构造抛 `RangeError` → 转成 `FileError("invalid", "Invalid line range", this.path)`（**逐字**，含 `path`）；`bytesRead === 0` → `scanner.finish()`。
- `close`：幂等；`file.close().catch(() => undefined)`。

#### `NodeDirReader`
- `next(maxEntries)`：已关闭 → `FileError("invalid", "Directory reader is closed", path)` **【逐字】**；`maxEntries must be a positive safe integer` **【逐字】**；用 `fs.opendir` 迭代（`dir.read()` 返回 `null` → `done = true`）；每条先 `resolve(this.path, entry.name)` 再 `lstat`，`ENOENT`（枚举与 lstat 之间被删）→ `continue`，其他错误 → `toFileError(error, entryPath)`（**注意用 entryPath 不是 path**）；`fileInfoFromStats` 失败（不支持的类型）→ 该条跳过、不报错；返回 `{ entries, done: this.done }`。
- `close`：幂等。

#### `symlinkRefused(path)`：`FileError("invalid", "Refusing to follow a symbolic link", path)` **【逐字】**。

#### `NodeExecutionEnv`（`implements ExecutionEnv`）

构造：`new NodeExecutionEnv({ cwd, shellPath?, shellEnv?, watch? })`；`id = "node:local"`。

- `watch(targets, onChange, context)`：委托 `NodeFileWatcher`（§2.24）；失败 → `err(FileError)`。
- `absolutePath(path, _context)`：`ok(resolvePath(this.cwd, path))`。
- `joinPath(parts, _context)`：`ok(path.join(...parts))`。
- `exec(command, options, context)` — **最复杂的方法**：
  0. **预检查**：`context.abortSignal?.aborted` → 立即 `err(new ExecutionError("aborted", "aborted"))`（**在解析 cwd/timeout 之前**）。
  1. `resolveTimeoutMs(options?.timeout)`；错误 → `err`。
  2. 解析 `cwd`：`options?.cwd ? resolvePath(this.cwd, options.cwd) : this.cwd`（**同样走 `~`/`file://` 展开**）；随后 `await access(cwd, F_OK)`，失败 → `ExecutionError("spawn_error", \`Working directory does not exist: ${cwd}\nCannot execute bash commands.\`, cause)`（**`code: "spawn_error"`，逐字，带 `cause`**）。
  3. 命令形式：字符串 → `getShellConfig(this.shellPath)`（失败直接返回该 `ExecutionError`），`program = shell`、`args = commandTransport === "stdin" ? config.args : [...config.args, command]`，`stdinCommand = commandTransport === "stdin" ? command : undefined`（**旧版 WSL bash 走 `-s` + stdin 传输**）；数组 → `program = command[0]`、`args = command.slice(1)`；空数组 → `ExecutionError("spawn_error", "Empty argv: no program to run")` **【逐字】**。
  4. 环境：`getShellEnv(this.shellEnv, options?.env, options?.inheritEnv)`。
  5. spawn（POSIX `detached: true`、Windows `detached: false`，`stdio: [stdinCommand === undefined ? "ignore" : "pipe", "pipe", "pipe"]`、`windowsHide: true`）；`child.pid` 加入 `activeChildPids`；有 `stdinCommand` 时 `child.stdin.end(stdinCommand)` 并挂空 `error` 监听。
  6. **两个独立的 `StreamDecoder`**（stdout/stderr）；每个 chunk 解码后按到达顺序调 `onOutput(text, context, { stream })`；回调抛错 → 终止并 `ExecutionError("callback_error", msg)`。
  7. **spill**（**逐字对齐源码**）：若给定 `options.spill`，只在 spill 尚未开始（`spillStart === undefined`）时累计 `seenBytes += chunk.length`，并统计 `seenNewlines`（数 `0x0a`）；**判定式为 `seenBytes <= spill.afterBytes && lines <= spill.afterLines` 时继续缓冲前缀**，其中 `lines = seenNewlines + (chunk[chunk.length - 1] === 0x0a ? 0 : 1)`（末字节不是换行则视为多一行）。一旦该条件不成立 → 把 `spillPrefix` 中所有 chunk 依序 `startSpill`，再 `startSpill(当前 chunk)`。
     - 注意三点易错：(a) 阈值为 `afterBytes`/`afterLines` 时**不算越界**（`<=`）；(b) 判定用**累计**字节 + **行数含"未终结的末行"**，不是逐 chunk；(c) spill 一旦开始，后续 chunk 不再计数、直接 `startSpill`。
     - `startSpill` 首次调用时 `pauseOutput()`（暂停 stdout/stderr），异步 `createTempFile({ prefix: "kiana-output-", suffix: ".log" }, context)` → `spillPath`，用 `createWriteStream(spillPath, { flags: "a", highWaterMark: 1 MiB })`（**`flags: "a"` 追加**）按 `spillQueue` 顺序写入；写侧背压时 `spillBackpressured = true` 并 `pauseOutput()`，`drain` 后 `resumeOutput()`。spill 失败 → `ExecutionError("unknown", \`Failed to preserve complete shell output: ${cause.message}\`, cause)` 并 `onAbort()`（"fails rather than silently losing a requested spill"）。
  8. 超时：到点 → `timedOut = true` + `killProcessTree`；错误对象为 `ExecutionError("timeout", \`timeout:${options?.timeout}\`)`（**逐字，注意不是自然语言句子**；`Command timed out after N seconds` 是 `tools/bash.ts` 自己包装的文案），`spillPath` 存在时挂到 `error.spillPath`。
  9. 中止：`context.abortSignal` 触发 → `killProcessTree`；`ExecutionError("aborted", "aborted")` + `spillPath`。
  10. 退出：先 `await finishSpill()`，再 flush 两个 decoder 的尾部（`emit(stdoutDecoder.decode(), "stdout")` / stderr 同）；`callbackError` 优先（`settle(err(callbackError))`），然后 timeout/abort，然后 `spillError`；否则 resolve `{ exitCode, spillPath? }`；信号退出 → `exitCode = code ?? (exitSignal ? 128 + (osConstants.signals[exitSignal] ?? 0) : 1)`（信号名不在 `os.constants.signals` 时补 0，既无 code 也无 signal 时用 1）。
  11. `spawn` 同步抛错或 `waitForChildProcess` reject → `ExecutionError("spawn_error", cause.message, cause)`；shell 不可用 → `shell_unavailable`。
  12. `emit` 的三重闸门：`settled || text === "" || options?.onOutput === undefined || callbackError !== undefined` → 丢弃（**`exec()` 结算后任何后裔进程仍持有的 stdio 输出都不会再回调**）。
- `openTextLineReader` / `readTextFile` / `readTextLines`：
  - `readTextFile` = `readFile(path, "utf8")`，失败 → `toFileError`。
  - `readTextLines(path, { maxLines }, context)`：`maxLines !== undefined && maxLines <= 0` → `ok([])`；否则 `openTextLineReader` 逐行读，最多 `maxLines` 行；空文件 → `[]`；逐行读失败原样返回该 `FileError`。
- `readBinaryFile`：`readFile` → `Uint8Array`。
- `openBinaryReader(path, options, context)`：
  - POSIX：`flags = O_RDONLY | O_NONBLOCK`（**`O_NONBLOCK` 用于让 FIFO 打开不等待写端**）；`noFollow` 时再加 `O_NOFOLLOW`。
  - Windows：没有 `O_NOFOLLOW`，`noFollow` 时先 `lstat` 最终组件，是符号链接 → `symlinkRefused`（源码注释明言**非竞态安全**）。
  - 打开后 `file.stat()`：非普通文件 → 目录用 `FileError("is_directory", "EISDIR: illegal operation on a directory, read", path)`，其他用 `FileError("invalid", "Not a regular file", path)`（**两条都逐字**，且打开后先 `close` 再返回）。
  - 捕获异常时：`noFollow && code === "ELOOP" || code === "EMLINK"` → `symlinkRefused(resolved, cause)`（**只有 noFollow 路径**）；否则 `toFileError(error, resolved)`。
  - 打开成功与打开失败后都再次检查 `abortSignal`，中止则关闭句柄并返回 `aborted`。
- `writeFile(path, content)`：先 `mkdir` 父目录（recursive），再写；中止 → `aborted`。
- `appendFile`、`truncateFile`（`File size must be a non-negative safe integer` **【逐字】**；不创建缺失文件）、`flushFile`（`fsync`；目录 → `Is a directory` **【逐字】**）、`renameFile`、`fileInfo`（`lstat`，不跟随符号链接）、`listDir`、`openDirReader`、`canonicalPath`（`realpath`）、`exists`、`createDir`（`recursive` 默认 true）、`remove`（`recursive`/`force` 默认 false）、`createTempDir`（前缀默认 `"tmp-"`）、`createTempFile`（`join(dir, prefix + randomUUID + suffix)` 并创建空文件）、`cleanup`（杀死所有活跃 pid 的子进程）。

**上述每个方法的精确行为（原 spec 只有名字；移植时逐条对齐）**：
- 所有路径参数先 `resolvePath(this.cwd, path)`；**每个方法在入口、每个 `await` 之后都查 `abortResult`**，中止 → `FileError("aborted","aborted", resolved)`。`renameFile` 的 abort 与错误都用 **destination**（abort）/ **source**（`toFileError(error, source)`）——不是同一个路径。
- `writeFile`：`mkdir(resolve(resolved, ".."), { recursive: true })` → 查 abort → `writeFile(resolved, content, { signal })`（**把 `signal` 传给 fs，取消可中断写**）。
- `appendFile`：同样先 `mkdir` 父目录并查 abort，但 `appendFile(resolved, content)` **不传 signal**；写后**再查一次 abort**（已写入但取消 → 返回 aborted）。
- `truncateFile`：`openFile(resolved, "r+")`（**不创建**）→ `file.truncate(size)` → 查 abort；`finally` 关闭并吞错。
- `flushFile`：`openFile(resolved, "r+")` → **若 `stat().isDirectory()` → `FileError("is_directory", "Is a directory", resolved)`**（注释逐字："POSIX refuses to open a directory for writing; Windows opens it, so check explicitly."）→ `file.sync()`；`finally` 关闭并吞错。
- `fileInfo`：`lstat`（不跟随符号链接）→ `fileInfoFromStats`。
- `listDir`：`readdir(resolved, { withFileTypes: true })` → 逐条 `resolve` + `lstat`；条目级失败 → `toFileError(error, entryPath)`（**entryPath**）；不支持类型 → 跳过（`if (info.ok)`）。
- `openDirReader`：`opendir(resolved)`；打开后再查 abort，中止则关目录并返回 aborted。
- `canonicalPath`：`realpath(resolved)`。
- `exists`：**实现为 `fileInfo()`**：ok → true；`code === "not_found"` → false；其他错误 → 原样返回（**不是**吞掉）。
- `createDir`：`mkdir(resolved, { recursive: options?.recursive ?? true })`。
- `remove`：`rm(resolved, { recursive: options?.recursive ?? false, force: options?.force ?? false })`。
- `createTempDir(prefix)`：`prefix ??= "tmp-"`；`mkdtemp(join(tmpdir(), prefix))`；**错误路径是 `toFileError(error)`（无 path）**。
- `createTempFile({prefix, suffix})`：**先 `createTempDir("tmp-")` 得到一个全新目录**，再 `join(dir, `${prefix ?? ""}${randomUUID()}${suffix ?? ""}`)`，`writeFile(filePath, "")`；错误 → `toFileError(error, filePath)`。**注意**：不是"在系统临时目录直接建文件"，而是"先建临时目录再在其中建文件"——codemode 的 `output.txt`/`image-N` 与 spill 都依赖这一层语义（文件名可预测性为零，但目录一定唯一）。
- `cleanup()`：对 `activeChildPids` 中每个 pid `killProcessTree(pid)`，随后清空集合。

**Rust 注意事项**
- 用 `tokio::process::Command`；进程组：`CommandExt::process_group(0)`（Unix）/`CREATE_NEW_PROCESS_GROUP` + `taskkill`（Windows）。
- 超时用 `tokio::time::timeout`；`abortSignal` 用 `tokio_util::sync::CancellationToken` 或 `tokio::select!`。
- spill 用 `tokio::fs::File` + `BufWriter`（`highWaterMark` 无直接对应，用 1 MiB 缓冲刷写）。
- `randomUUID` → `uuid::Uuid::new_v4()`。
- `mtimeMs` → `SystemTime` 转毫秒（浮点，保留小数以匹配 `Node` 的 `mtimeMs`；比较时精度需一致）。

### 2.24 `src/env/node-watch.ts`（522 行）

```ts
export type NodeWatchOptions = { mode?: "native" | "polling"; pollIntervalMs?: number; maxDirectories?: number };
const DEBOUNCE_MS = 50;
const FSEVENTS_SETTLE_MS = 500;
const DEFAULT_POLL_MS = 2_000;
const DEFAULT_MAX_DIRECTORIES = 10_000;
const HASH_MAX_BYTES = 256 * 1024;
const HASH_RECENT_MS = 5_000;
```

**不可靠文件系统魔数集合 `UNRELIABLE_FILE_SYSTEMS`**（`statfs` 的 `type` 值，**逐字全部 12 个**）：
`0x6969` NFS、`0x517b` SMB、`0xff534d42` CIFS、`0xfe534d42` SMB2、`0x65735546` FUSE（sshfs、Android shared storage）、`0x01021997` 9P（WSL2 Windows 盘）、`0x0bd00bd0` Lustre、`0x47504653` GPFS、`0x00c36400` Ceph、`0x5346414f` OpenAFS、`0x6b414653` kAFS、`0x5dca2df5` sdcardfs。
命中 → 强制 polling。**判定细节**：只有 `linux`/`android` 会做 statfs；对每个被监视 path，按 `[path, ...ancestorsOf(path)]` 顺序找**第一个 statfs 成功**的候选，命中集合 → true，否则继续下一个 path；全部失败/不命中 → false（**statfs 失败视为可靠**）。只有 `options.mode` 未指定时才自动判定，显式 `mode` 优先。

行为：
- **`static async open(targets, resolvePath, onChange, options)`**（**签名逐字：第二参是 `resolvePath: (path: string) => string`，不是 `Context`；第四参是 `options`；没有 `context`**）：
  - Windows 一律 polling。
  - `anyUnreliable`（任一被监视根位于不可靠 FS）→ polling。
  - `native`：Linux 上**逐目录** `fs.watch`；其他平台递归 watch；watch 失败 → 切 polling（并先投递 `{ overflow: true }`）。
  - 目录预算：扫描时统计目录数，超过 `maxDirectories` → 内部 `BudgetExceeded`（`class BudgetExceeded extends Error {}`）→ `open()` 捕获并抛 **`new FileError("invalid", error.message)`**，消息为 **`` `Watched paths exceed ${this.#maxDirectories} directories` ``**（**逐字：复数 `directories`，不是 `directories` 之外的 `dirs`**；原 spec 的模板写成 `Watched paths exceed ${max} directories` 但未点明错误类与阶段）。**注意两处差异**：(a) `open()` 阶段（初次 `#sync(false)`）超预算 → **抛 `FileError`**（`NodeExecutionEnv.watch` 再包成 `err(...)`）；(b) `#flush` 阶段（后续 `#sync(true)`）超预算 → `#deliver({ error: FileError(invalid|permission_denied, message) })` 并 `#stop()`，**不抛**。
- `mode` getter 返回当前模式。
- `close` 幂等；停止后不再有 `onChange`（`#deliver` 由 `#stop` 保护）。
- **去抖**：事件到达后 `DEBOUNCE_MS`(50) 合并；darwin 额外 `FSEVENTS_SETTLE_MS`(500) settle；`#flush` 投递 `{ paths: sorted }` 或 `{ error }` 后停止。
- **`#flush` 细节（原 spec 缺失）**：重入时只置 `#dirty = true` 并复用同一个 `#running`；否则 `do { ... } while (#dirty && !closed)` 循环，每轮取走 `#events`、`#sync(true)`、把事件的 `#reported(path)` 归并进 changed；`changed.size > 0` → `#deliver({ paths: [...changed].sort() })`（**字典序 sort**）。异常 → 转 `FileError`（`isDenied(error)` → `"permission_denied"`，否则 `"invalid"`；`error instanceof FileError` 原样用），`#deliver({ error })` 后 `#stop()`。
- **`#reported(path)`**：若 path 在某 target 之内 → 返回 path；否则返回"包含 path 的 target 的根"（祖先身份变化 → 报告整棵 target），都不匹配 → path。
- **`#scan` 细节（原 spec 缺失）**：
  - 目录按 target **各遍历一次**（`traversed` 以 `` `${index}\0${directory}` `` 去重），但**计数全局只算一次**（`counted`）；`counted.size > maxDirectories` 抛 `BudgetExceeded`。
  - 每个 target 先 `lstat` 其**全部祖先**并记录 `{ ...entryOf(stats), size: 0, mtimeMs: 0 }`（**只比身份**，因为祖先的时间戳会随无关兄弟变化）。
  - target 本身用 `stat`（**跟随符号链接**）：`isDenied(error)` → 抛出（不可达即失败）；其他错误（ENOENT 等）→ `continue`（缺失的 target 被监视其"出现"）。
  - target 是"指向文件的符号链接" → `linkedFiles.add(path)`（该文件的变化不是链接所在目录的事件）。
  - 子项用 `lstat`（**不跟随**），缺失则跳过；`excluded(target, name)` 为真的名字直接跳过（`exclude.hidden`/`exclude.names`）。
  - 目录读取失败：**target 自身**且 `isDenied` → 抛出；其下目录的 `ENOENT`/`EACCES`/`EPERM`/`ENOTDIR` → 跳过；其他 → 抛出。
  - **polling 模式的哈希**：仅 `stats.isFile() && size <= HASH_MAX_BYTES(256 KiB) && Date.now() - mtimeMs < HASH_RECENT_MS(5000)` 时读全文算 **sha256 hex**；读失败 → `undefined`。
- **`#reconcileWatchers`**：wanted 集合 = 所有 `linkedFiles` + 每个 target 的已存在祖先目录 + target 目录本身；**Linux/Android 逐目录**（`fs.watch` 非递归），其他平台每个 target 一个递归 watcher；新增/移除 watcher 返回"是否有新增"，`#sync` 据此再扫一轮（最多 10 轮，`#sync(report)` 的 `report` 首轮为 `false`，之后为 `true`，darwin 每轮 `#scheduleSettle()`）。**两处容易混淆的错误路径**（原 spec 把两者混为一谈）：
  - `fs.watch()` **同步抛错**时：若 `code` 不是 `ENOENT`/`EACCES`/`EPERM`（例如 `ENOSPC` 超 watch 配额、平台不支持递归）→ `#switchToPolling()` 并**立即 `return false`**；否则静默跳过该目录。`#switchToPolling()` 只在当前不是 polling 时生效：`#mode = "polling"`、关闭并清空所有 watcher、`#deliver({ overflow: true })`、清 `#timer`、`#schedulePoll()`。
  - 已安装 watcher 的 **`"error"` 事件**：只 `watcher.close()` + 从 `#watchers` 删除（仅当仍是当前实例）+ `#scheduleFlush()`；**不切 polling、不投递 overflow**——重新扫描会重建 watcher。
  - watcher 的替换条件：`!wanted.has(path) || entry === undefined || entry.dev !== installed.dev || entry.ino !== installed.ino`（**目录被重建即替换**，因为 watcher 跟随的是那个 inode）。`fs.watch` 参数：`{ recursive, persistent: false }`。

- **`#onEvent(directory, filename)`**：path = `filename === undefined ? directory : dir + sep + filename`；`relevant = #inScope(path)`；relevant → `#events.add(#reported(path))`；`relevant || filename === undefined` → `#scheduleFlush()`。
- **`#onLinkedFileEvent(target)`**：直接 `#events.add(target)` + `#scheduleFlush()`。
- **`#inScope(path)`**：对每个 target，`isWithin(target.path, path)` → true；若 path 在 target 内（且不是 target 本身），非递归 target 只允许**一层**（`relative(...).split(sep).length > 1` → 跳过），路径组件命中 `excluded` → 跳过；否则 true。全部 target 都不匹配 → false（**祖先目录的无关兄弟事件被丢弃**）。
- **`anyUnreliable(paths)`** 只对 `linux`/`android` 生效；对每个 path 依次尝试 `path` 与其**祖先**，取**第一个能 `statfs` 成功**的（失败即继续），命中集合 → true，否则 false。
- **polling**：每 `pollIntervalMs`(2000) 扫描快照并 diff；`#sync` 最多 10 轮（darwin settle 处理）。
- **快照**：`Entry` 记录 kind/size/mtimeMs/ino；对大文件（≤ `HASH_MAX_BYTES`）且"最近"（mtime 在 `HASH_RECENT_MS` 内）计算内容哈希，避免 mtime 精度遗漏。
- **排除**：`exclude.hidden`（名字以 `.` 开头）与 `exclude.names`；符号链接**不跟随**。
- `#reconcileWatchers`：按需新增/移除目录 watcher；`#onLinkedFileEvent` 处理被链接文件。

**Rust**：`notify` crate（`RecommendedWatcher`）+ 自实现快照 diff；`statfs` 用 `nix::sys::statfs::statfs`（Unix）；Windows 用轮询（与实现一致）。

### 2.25 `src/tools/**`

#### 2.25.1 `tools/index.ts`（39 行）
```ts
export { createBashTool, createPowerShellTool, type BashExecution, type BashPrepare, type BashToolInput, type BashToolOptions, type PowerShellToolInput, type PowerShellToolOptions } from "./bash.ts";
export { createEditTool, type EditToolDetails, type EditToolInput } from "./edit.ts";
export { DEFAULT_IMAGE_LIMITS, type ImageLimits, type ImageProcessor, type PreparedImage } from "./image-processor.ts";
export { createReadTool, type ReadToolDetails, type ReadToolInput, type ReadToolOptions } from "./read.ts";
export { createWriteTool, type WriteToolInput } from "./write.ts";
export function createCodingTools(options: ReadToolOptions = {}) {          // 扩展名 "coding-tools"
  return defineExtension({ name: "coding-tools", tools: [createReadTool(options), createWriteTool(), createEditTool(), createBashTool()] });
}
export const CodingTools = createCodingTools();
```

#### 2.25.2 `tools/env.ts`
```ts
export function requireEnv(api: ToolExecutionApi): ExecutionEnv;   // api.env undefined → Error("No execution environment is configured") 【逐字】
```

#### 2.25.3 `tools/path-utils.ts`
```ts
const UNICODE_SPACES = /[\u00A0\u2000-\u200A\u202F\u205F\u3000]/g;
const NARROW_NO_BREAK_SPACE = "\u202F";
function normalizeToolPath(path) { const n = path.replace(UNICODE_SPACES, " "); return n.startsWith("@") ? n.slice(1) : n; }
export async function resolveToolPath(env, path, context): Promise<string>;      // getOrThrow(env.absolutePath(normalizeToolPath(path)))
export async function resolveReadToolPath(env, path, context): Promise<string>;
```
`resolveReadToolPath` 依次尝试（去重后第一个 `exists` 为真者）：
1. `resolved`
2. `resolved.replace(/ (AM|PM)\./gi, "\u202F$1.")`（窄不换行空格）
3. `resolved.normalize("NFD")`
4. `resolved.replace(/'/g, "\u2019")`
5. `resolved.normalize("NFD").replace(/'/g, "\u2019")`
都失败 → 返回 `resolved`。

#### 2.25.4 `tools/file-mutation-queue.ts`
```ts
const queues = new Map<string, Promise<void>>();
export async function withFileMutationQueue<T>(env, path, fn, context): Promise<T>;
```
- key = `` `${env.id}\0${canonical(abs)}` ``（同一进程内同一文件系统 id + 规范路径的 edit/write 串行化；与 bash 或其他进程无关）。
- `canonical`：`env.canonicalPath`；`not_supported` → 原绝对路径；`not_found` → 递归规范化父目录后 join 名字（父目录末尾是 `/` 或 `\` 时不额外加分隔符）；其他错误抛出。
- 入队：取 `previous = queues.get(key) ?? Promise.resolve()`；创建 `done`；`tail = previous.then(() => done)`；`queues.set(key, tail)`；`await previous`；执行 `fn()`；`finally { release(); if (queues.get(key) === tail) queues.delete(key) }`。
- **重要**：槽位在 `await` 之前占据（同步 `set`），因此后续调用看到正确的尾。

#### 2.25.5 `tools/image-processor.ts`
```ts
export type ImageLimits = { readonly maxWidth: number; readonly maxHeight: number; readonly maxBytes: number };
export const DEFAULT_IMAGE_LIMITS = { maxWidth: 2000, maxHeight: 2000, maxBytes: 4.5 * 1024 * 1024 };
export type PreparedImage = {
  readonly data: string;                     // base64
  readonly mimeType: string;
  readonly resized?: { readonly from: {width,height}; readonly to: {width,height} };
  readonly convertedFrom?: string;
};
export type ImageProcessor = { prepare(bytes: Uint8Array, mimeType: string, limits: ImageLimits): Promise<PreparedImage | undefined> };
export const INLINE_IMAGE_TYPES: ReadonlySet<string> = new Set(["image/png","image/jpeg","image/gif","image/webp"]);
export function toBase64(bytes: Uint8Array): string;      // 8 KiB 分块 String.fromCharCode + btoa；string 标准 base64
export function base64Length(byteLength: number): number; // Math.ceil(n/3)*4
```
`toBase64` 的分块原因（源码注释）：单次 `String.fromCharCode.apply` 传几 MB 参数会爆栈。

#### 2.25.6 `tools/image.ts`（格式探测，182 行）

```ts
const PNG_SIGNATURE = [0x89,0x50,0x4e,0x47,0x0d,0x0a,0x1a,0x0a];
const HEADER_BYTES = 32;
const BLOCK_BYTES = 64 * 1024;
export type ByteSource = { readonly size: number; read(offset: number, length: number): Promise<Uint8Array> };
export async function detectSupportedImageMimeTypeOf(source: ByteSource): Promise<string | undefined>;
export function detectSupportedImageMimeType(buffer: Uint8Array): string | undefined;
export function imageDimensions(bytes: Uint8Array, mimeType: string): { width: number; height: number } | undefined;
```

`detectSupportedImageMimeType(buffer)` 顺序：
1. `startsWith([0xff,0xd8,0xff])` → `buffer[3] === 0xf7` ? undefined : `"image/jpeg"`（0xF7 为 JPG 扩展流，不支持）。
2. PNG 签名 → `isPng(buffer) && !isAnimatedPng(buffer) ? "image/png" : undefined`。
3. `"GIF87a"` 或 `"GIF89a"` → `"image/gif"`。
4. `"RIFF"` @0 且 `"WEBP"` @8 → `"image/webp"`。
5. `"BM"` 且 `isBmp(buffer)` → `"image/bmp"`。
6. 否则 undefined。

`isPng(buffer)`：`length >= 16 && readUint32BE(buffer, 8) === 13 && startsWithAscii(buffer, 12, "IHDR")`。
`isAnimatedPng(buffer)`：从 offset 8 遍历 chunk header：`acTL` → true；`IDAT` → false；`next = offset + 8 + chunkLength + 4`，若 `next <= offset || next > length` → false。
`isAnimatedPngOf(source)`：分块（64 KiB）读取的等价实现。
`detectSupportedImageMimeTypeOf(source)`：读 0..32；非 PNG 签名 → 直接 `detectSupportedImageMimeType(header)`；否则 `isPng(header) && !(await isAnimatedPngOf(source)) ? "image/png" : undefined`。

`imageDimensions(bytes, mimeType)`：
- `image/png`：`length < 24` → undefined；`{ width: readUint32BE(16), height: readUint32BE(20) }`。
- `image/gif`：`length < 10` → undefined；`{ readUint16LE(6), readUint16LE(8) }`。
- `image/webp`：`length < 30` → undefined；
  - `"VP8 "` @12 → `{ readUint16LE(26) & 0x3fff, readUint16LE(28) & 0x3fff }`；
  - `"VP8L"` @12 → `bits = readUint32LE(21)`；`{ (bits & 0x3fff) + 1, ((bits >>> 14) & 0x3fff) + 1 }`；
  - `"VP8X"` @12 → `uint24(24) + 1`, `uint24(27) + 1`，其中 `uint24(o) = readUint16LE(o) + ((bytes[o+2] ?? 0) << 16)`。
- `image/jpeg`：从 offset 2 扫描 marker：非 `0xff` → undefined；`0xff` → offset++ 继续；`0xc0..0xcf` 且非 `0xc4/0xc8/0xcc`（SOF）→ `{ readUint16BE(offset+7), readUint16BE(offset+5) }`；否则 `offset += 2 + readUint16BE(offset+2)`。
- 默认 undefined。

`isBmp(buffer)`（**精确**）：
- `length < 26` → false；
- `declaredFileSize = readUint32LE(2)`；`pixelDataOffset = readUint32LE(10)`；`dibHeaderSize = readUint32LE(14)`；
- `declaredFileSize !== 0 && declaredFileSize < 26` → false；
- `pixelDataOffset < 14 + dibHeaderSize` → false；
- `declaredFileSize !== 0 && pixelDataOffset >= declaredFileSize` → false；
- `dibHeaderSize === 12` → `planes = readUint16LE(22)`, `bpp = readUint16LE(24)`；
- `dibHeaderSize >= 40 && <= 124` → `length < 30` → false；`planes = readUint16LE(26)`, `bpp = readUint16LE(28)`；
- 否则 false；
- 返回 `planes === 1 && [1,4,8,16,24,32].includes(bpp)`。

#### 2.25.7 `tools/read.ts`（312 行）

Schema（**描述逐字**）：
```ts
const readSchema = Type.Object({
  path: Type.String({ description: "Path to the file to read (relative or absolute)" }),
  offset: Type.Optional(Type.Number({ description: "Line number to start reading from (1-indexed)" })),
  limit: Optional(Type.Number({ description: "Maximum number of lines to read" })),
});
```
`export type ReadToolOptions = { readonly images?: ImageProcessor }`。

**工具描述 【逐字】**（`formats = options.images === undefined ? "jpg, png, gif, webp" : "jpg, png, gif, webp, bmp"`）：
```
Read the contents of a file. Supports text files and images (${formats}). Images are sent as attachments. For text files, output is truncated to 2000 lines or 50KB (whichever is hit first). Use offset/limit for large files. When you need the full file, continue with offset until complete.
```

`execute(args, api, context)`：
1. `requireEnv(api)`。
2. `absolutePath = await resolveReadToolPath(env, path, context)`。
3. `reader = getOrThrow(await env.openBinaryReader(absolutePath, undefined, context))`。
4. 循环（最多 2 次重试）：
   - `before = reader.info()`；`mimeType = detectSupportedImageMimeTypeOf({ size: before.size, read: (off,len) => reader.read(off,len,context) })`。
   - `result = mimeType === undefined ? readText(...) : readImage(...)`。
   - `after = reader.info()`；`unchanged = after.size === before.size && after.mtimeMs === before.mtimeMs`。
   - `unchanged || (mimeType === undefined && after.size > before.size)` → 返回 result；
   - `attempt === 1` → `throw new Error("${path} changed while it was read")` **【逐字】**；否则重试。
5. `finally { await reader.close(context) }`。

`readText(reader, path, offset, limit, context)`（**精确**）：
- `startLine = offset ? Math.max(0, offset - 1) : 0`；`startLineDisplay = startLine + 1`。
- `sliceIndex(v) = Number.isNaN(v) ? 0 : Math.trunc(v)`（模拟 `Array.prototype.slice`）。
- `sliceStart = sliceIndex(startLine)`；`scanStart = Number.isSafeInteger(sliceStart) ? sliceStart : 0`；
- `requestedEnd = limit === undefined ? undefined : Math.max(scanStart + 1, sliceIndex(startLine + limit))`；
- `scanEnd = requestedEnd !== undefined && Number.isSafeInteger(requestedEnd) ? requestedEnd : undefined`。
- `scan = reader.scanLines({ startLine: scanStart, ...(scanEnd === undefined ? {} : { endLine: scanEnd }) })`。
- `totalFileLines = scan.newlines + 1`。
- `startLine >= totalFileLines` → `throw new Error("Offset ${offset} is beyond end of file (${totalFileLines} lines total)")` **【逐字】**。
- `userLimitedLines`、`selectedLineCount` 计算（支持负 limit 的 `slice` 语义：`relativeEnd < 0` 时从末尾反算，必要时重新 `scanOf(sliceEnd)`）。
- `empty = selectedLineCount === 0`。
- `endsWithNewline = !empty && scan.lastLineStart === scan.end && scan.lastLineStart > scan.start`。
- `totals = { lines: empty || scan.selectedBytes === 0 ? 0 : selectedLineCount - (endsWithNewline ? 1 : 0), bytes: empty ? 0 : scan.selectedBytes }`。
- `firstBytes = reader.read(0, 3)`；`skipBom = startsWithBom(firstBytes)`。
- `head = empty ? "" : readHead(reader, scan.start, scan.end, skipBom, context)`。
- `{ content: headText, ...truncation } = truncateHeadOf(head, totals)`。
- 诊断与输出：
  - `truncation.firstLineExceedsLimit`：
    - `integral = Number.isInteger(startLine)`；`lineBytes = new TextEncoder().encode(integral ? (head.split("\n")[0] ?? "") : "")`；`lineSize = integral ? scan.firstLineBytes : 0`；`end = characterEnd(lineBytes, DEFAULT_MAX_BYTES)`；
    - `outputText = new TextDecoder().decode(lineBytes.subarray(0, end))`；
    - diagnostic `{ severity: "warn", code: "truncated", message: `Line ${startLineDisplay} is ${formatSize(lineSize)}, exceeds the ${formatSize(DEFAULT_MAX_BYTES)} limit; showing its first ${formatSize(end)}. Use bash: sed -n '${startLineDisplay}p' ${path} | tail -c +${end + 1}` }` **【逐字】**；
    - `details = { truncation: { ...truncation, outputBytes: end, outputLines: 1 } }`。
  - 否则 `truncation.truncated`：
    - `endLineDisplay = startLineDisplay + truncation.outputLines - 1`；`nextOffset = endLineDisplay + 1`；
    - `limitText = truncation.truncatedBy === "lines" ? "" : ` (${formatSize(DEFAULT_MAX_BYTES)} limit)``；
    - diagnostic `{ severity:"info", code:"truncated", message: "Showing lines ${startLineDisplay}-${endLineDisplay} of ${totalFileLines}${limitText}. Use offset=${nextOffset} to continue." }` **【逐字】**；
    - `details = { truncation }`。
  - 否则若 `userLimitedLines !== undefined && startLine + userLimitedLines < totalFileLines`：
    - `remaining = totalFileLines - (startLine + userLimitedLines)`；`nextOffset = startLine + userLimitedLines + 1`；
    - diagnostic `{ severity:"info", message: "${remaining} more lines in file. Use offset=${nextOffset} to continue." }` **【逐字】**（**无 `code`**）。
- 返回 `{ output: outputText === "" ? [] : [{ type:"text", text: outputText }], ...(details?{details}:{}) , diagnostics }`。

`readHead(reader, start, end, skipBom, context)`：
- `decoder = rangeDecoder()`；`text = ""`；`newlines = 0`；
- 从 `skipBom && start === 0 ? 3 : start` 到 `end`，每次读 `min(65536, end - position)`；空读 → break；
- `decoded = decoder.decode(bytes, {stream:true})`；`text += decoded`；统计 `decoded` 中的 `\n`；
- `newlines >= 2000 || utf8ByteLength(text) > 51200 + 1` → 返回 text（**不 flush**）。
- 结束 → `text + decoder.decode()`。

`readImage(reader, info, path, mimeType, processor, { vision, limits }, context)`：
- 有 processor：
  - `bytes = reader.read(0, info.size)`；`prepared = processor.prepare(bytes, mimeType, limits)`；
  - undefined → `imageError("${path} is an image (${mimeType}) that cannot be prepared for the model")`；
  - `convertedFrom` → note `Converted from ${convertedFrom} to ${mimeType}.`；
  - `resized` → note `Resized from ${from.width}x${from.height} to ${to.width}x${to.height}. Multiply coordinates by ${(from.width / to.width).toFixed(2)} to map them to the original.`。
- 无 processor：
  - `!INLINE_IMAGE_TYPES.has(mimeType)` → `imageError("${path} is an image (${mimeType}) that needs converting, and no image processor is configured")`；
  - `base64Length(info.size) > limits.maxBytes` → `imageError("${path} is an image (${mimeType}) of ${formatSize(info.size)}, too large to send, and no image processor is configured to shrink it")`；
  - `bytes = reader.read(0, info.size)`；`size = imageDimensions(bytes, mimeType)`；undefined → `imageError("${path} is an image (${mimeType}) whose size cannot be read")`；
  - 超尺寸 → `imageError("${path} is an image (${mimeType}) of ${w}x${h}, larger than ${maxW}x${maxH}, and no image processor is configured to shrink it")`；
  - `image = { data: toBase64(bytes), mimeType }`。
- `!vision` → note `"The current model does not support images; it sees a placeholder instead."`。
- diagnostics：`[{ severity:"info", code:"image", message: [ "Read image file [${mimeType}].", ...notes ].join(" ") }]`。
- 返回 `{ output: [{ type:"image", data, mimeType }], diagnostics }`。

`imageError(message)`：`{ output: [], isError: true, diagnostics: [{ severity:"error", code:"unsupported_image", message }] }`。

`imageModel(api, context)`：
- `ref = (await api.agent(context)).model`；`model = ref === undefined ? undefined : api.models.getModel(ref.provider, ref.modelId)`；
- `resize = model?.inputLimits?.images?.resize`；
- 返回 `{ vision: model?.input.includes("image") ?? true, limits: { maxWidth: resize?.maxWidth ?? 2000, maxHeight: resize?.maxHeight ?? 2000, maxBytes: resize?.maxBytes ?? 4.5*1024*1024 } }`。

`ReadToolDetails = { truncation?: Omit<TruncationResult, "content"> }`。

#### 2.25.8 `tools/write.ts`
Schema 描述 **逐字**：`path: "Path to the file to write (relative or absolute)"`、`content: "Content to write to the file"`。
工具描述 **【逐字】**：
```
Write content to a file. Creates the file if it doesn't exist, overwrites if it does. Automatically creates parent directories.
```
`execute`：
1. `resolveToolPath`；`withFileMutationQueue(env, abs, async () => {...})`。
2. 队列内：`context.abortSignal?.aborted` → `throw new Error("Operation aborted")` **【逐字】**；`env.writeFile(abs, content)`；再次 abort 检查；返回 `{ output: [{ type:"text", text: "Successfully wrote to ${path}" }] }` **【逐字】**。

#### 2.25.9 `tools/edit.ts`（144 行）
Schema：
```ts
const replaceEditSchema = Type.Object({
  oldText: Type.String({ description: "Exact text for one targeted replacement. It must be unique in the original file and must not overlap with any other edits[].oldText in the same call." }),
  newText: Type.String({ description: "Replacement text for this targeted edit." }),
});
const editSchema = Type.Object({
  path: Type.String({ description: "Path to the file to edit (relative or absolute)" }),
  edits: Type.Array(replaceEditSchema, { description: "One or more targeted replacements. Each edit is matched against the original file, not incrementally. Do not include overlapping or nested edits. If two changes touch the same block or nearby lines, merge them into one edit instead." }),
});
```
工具描述 **【逐字】**：
```
Edit a single file using exact text replacement. Every edits[].oldText must match a unique, non-overlapping region of the original file. If two changes affect the same block or nearby lines, merge them into one edit instead of emitting overlapping edits. Do not include large unchanged regions just to connect distant changes.
```

`prepareEditArguments(input)`（**纯函数，不得修改入参**）：
- 非对象或数组 → 原样返回。
- `args = { ...input }`；
- `typeof args.edits === "string"` → `JSON.parse`（失败静默）；数组 → 采用；单个 `{oldText,newText}` → `[parsed]`。
- 否则 `isSingleEditInput(args.edits)` → `[args.edits]`。
- 遗留形态：若 `args.oldText` 与 `args.newText` 都是 string → `edits = Array.isArray(args.edits) ? [...args.edits] : []`；`edits.push({oldText, newText})`；去掉顶层 `oldText`/`newText`；返回 `{ ...rest, edits }`。

`validateEditInput(input)`：`!Array.isArray(input.edits) || input.edits.length === 0` → `Error("Edit tool input is invalid. edits must contain at least one replacement.")` **【逐字】**。

`editAccessError(path, error)`：`new Error("Could not edit file: ${path}. Error code: ${error.code}.", { cause: error })` **【逐字】**。

`execute`：
1. `validateEditInput`；`requireEnv`；`resolveToolPath`；`withFileMutationQueue`。
2. abort 检查 → `"Operation aborted"`。
3. `info = env.fileInfo(abs)`；`!info.ok` → `editAccessError`。
4. `info.value.kind !== "file" && !== "symlink"` → `Error("Could not edit file: ${path}. Path is not a file.")` **【逐字】**。
5. `readTextFile`；失败 → `editAccessError`；abort 检查。
6. `{ bom, text: content } = stripBom(...)`；`originalEnding = detectLineEnding(content)`；`normalizedContent = normalizeToLF(content)`；
   `{ baseContent, newContent } = applyEditsToNormalizedContent(normalizedContent, edits, path)`；abort 检查。
7. `finalContent = bom + restoreLineEndings(newContent, originalEnding)`；`writeFile`；失败 → `editAccessError`；abort 检查。
8. `diffResult = generateDiffString(baseContent, newContent)`；`details = { diff, patch: generateUnifiedPatch(path, baseContent, newContent), ...(firstChangedLine === undefined ? {} : { firstChangedLine }) }`。
9. 返回 `{ output: [{ type:"text", text: "Successfully replaced ${edits.length} block(s) in ${path}." }], details }` **【逐字】**。

`EditToolDetails = { diff: string; patch: string; firstChangedLine?: number }`。

#### 2.25.10 `tools/edit-diff.ts`（500 行）— **模糊匹配与 diff**

```ts
export function detectLineEnding(content: string): "\r\n" | "\n";
export function normalizeToLF(text: string): string;      // /\r\n/g → "\n"，/\r/g → "\n"
export function restoreLineEndings(text: string, ending): string;
export function normalizeForFuzzyMatch(text: string): string;
export function applyReplacementsPreservingUnchangedLines(originalContent, baseContent, replacements): string;
export interface FuzzyMatchResult { found: boolean; index: number; matchLength: number; usedFuzzyMatch: boolean; contentForReplacement: string }
export interface Edit { oldText: string; newText: string }
export interface AppliedEditsResult { baseContent: string; newContent: string }
export function fuzzyFindText(content: string, oldText: string): FuzzyMatchResult;
export function stripBom(content: string): { bom: string; text: string };
export function applyEditsToNormalizedContent(normalizedContent: string, edits: Edit[], path: string): AppliedEditsResult;
export function generateUnifiedPatch(path: string, oldContent: string, newContent: string, contextLines = 4): string;
export function generateDiffString(oldContent: string, newContent: string, contextLines = 4): { diff: string; firstChangedLine: number | undefined };
```

`detectLineEnding`：`crlfIdx = indexOf("\r\n")`，`lfIdx = indexOf("\n")`；`lfIdx === -1` → `"\n"`；`crlfIdx === -1` → `"\n"`；`crlfIdx < lfIdx ? "\r\n" : "\n"`。

`normalizeForFuzzyMatch(text)` 顺序（**必须一致**）：
1. `.normalize("NFKC")`；
2. 按 `\n` 切分，每行 `trimEnd()`，再 `join("\n")`；
3. `[\u2018\u2019\u201A\u201B]` → `'`；
4. `[\u201C\u201D\u201E\u201F]` → `"`；
5. `[\u2010\u2011\u2012\u2013\u2014\u2015\u2212]` → `-`；
6. `[\u00A0\u2002-\u200A\u202F\u205F\u3000]` → `" "`（普通空格）。

`splitLinesWithEndings(content)`：`content.match(/[^\n]*\n|[^\n]+/g) ?? []`（每行含其换行；最后一行可无换行）。

`getLineSpans(content)`：逐行累计 `{ start, end }`。
`getReplacementLineRange(lines, replacement)`：
- `startLine` = 第一个满足 `replacementStart >= line.start && < line.end` 的行；
- 找不到或找不到结束行 → `Error("Replacement range is outside the base content.")`；
- `endLine` 从 `startLine` 起，直到 `lines[endLine].end >= replacementEnd`；返回 `{ startLine, endLine: endLine + 1 }`。

`applyReplacements(content, replacements, offset = 0)`：逆序应用（`matchIndex - offset`，`substring(0, i) + newText + substring(i + matchLength)`）。

`applyReplacementsPreservingUnchangedLines(originalContent, baseContent, replacements)`：
- `originalLines = splitLinesWithEndings(originalContent)`；`baseLines = getLineSpans(baseContent)`；
- 行数不同 → `Error("Cannot preserve unchanged lines because the base content has a different line count.")` **【逐字】**；
- 按 `matchIndex` 排序后将重叠行区间合并成 groups；
- 逐 group：拼接 group 之前的原始行（**原文保留**），再对 `baseContent.slice(groupStartOffset, groupEndOffset)` 应用该组的替换；最后拼接尾部原始行。

`fuzzyFindText(content, oldText)`：
- 先 `content.indexOf(oldText)` 精确匹配 → `{ found:true, index, matchLength: oldText.length, usedFuzzyMatch:false, contentForReplacement: content }`；
- 否则 `fuzzyContent = normalizeForFuzzyMatch(content)`，`fuzzyOldText = normalizeForFuzzyMatch(oldText)`，`fuzzyIndex = fuzzyContent.indexOf(fuzzyOldText)`；-1 → `{ found:false, index:-1, matchLength:0, usedFuzzyMatch:false, contentForReplacement: content }`；
- 命中 → `{ found:true, index: fuzzyIndex, matchLength: fuzzyOldText.length, usedFuzzyMatch:true, contentForReplacement: fuzzyContent }`。

`stripBom(content)`：以 `\uFEFF` 开头 → `{ bom: "\uFEFF", text: content.slice(1) }`，否则 `{ bom:"", text: content }`。

`countOccurrences(content, oldText)`：`normalizeForFuzzyMatch(content).split(normalizeForFuzzyMatch(oldText)).length - 1`。

错误构造器 **【逐字】**：
```ts
getNotFoundError: 1 个 edit → `Could not find the exact text in ${path}. The old text must match exactly including all whitespace and newlines.`
                  n 个 edit → `Could not find edits[${i}] in ${path}. The oldText must match exactly including all whitespace and newlines.`
getDuplicateError: 1 → `Found ${n} occurrences of the text in ${path}. The text must be unique. Please provide more context to make it unique.`
                   n → `Found ${n} occurrences of edits[${i}] in ${path}. Each oldText must be unique. Please provide more context to make it unique.`
getEmptyOldTextError: 1 → `oldText must not be empty in ${path}.`; n → `edits[${i}].oldText must not be empty in ${path}.`
getNoChangeError: 1 → `No changes made to ${path}. The replacement produced identical content. This might indicate an issue with special characters or the text not existing as expected.`
                  n → `No changes made to ${path}. The replacements produced identical content.`
```

`applyEditsToNormalizedContent(normalizedContent, edits, path)`（**精确**）：
1. `normalizedEdits = edits.map(e => ({ oldText: normalizeToLF(e.oldText), newText: normalizeToLF(e.newText) }))`。
2. 逐个检查空 oldText → `getEmptyOldTextError`。
3. `initialMatches = normalizedEdits.map(e => fuzzyFindText(normalizedContent, e.oldText))`；
   `usedFuzzyMatch = initialMatches.some(m => m.usedFuzzyMatch)`；
   `replacementBaseContent = usedFuzzyMatch ? normalizeForFuzzyMatch(normalizedContent) : normalizedContent`。
4. 对每个 edit：`matchResult = fuzzyFindText(replacementBaseContent, e.oldText)`；未找到 → `getNotFoundError`；
   `occurrences = countOccurrences(replacementBaseContent, e.oldText)`；`> 1` → `getDuplicateError`；
   记录 `{ editIndex, matchIndex, matchLength, newText }`。
5. `matchedEdits.sort((a,b) => a.matchIndex - b.matchIndex)`；相邻重叠（`prev.matchIndex + prev.matchLength > cur.matchIndex`）→ `Error("edits[${prev.editIndex}] and edits[${cur.editIndex}] overlap in ${path}. Merge them into one edit or target disjoint regions.")` **【逐字】**。
6. `newContent = usedFuzzyMatch ? applyReplacementsPreservingUnchangedLines(normalizedContent, replacementBaseContent, matchedEdits) : applyReplacements(replacementBaseContent, matchedEdits)`；
   `baseContent === newContent` → `getNoChangeError`。
7. 返回 `{ baseContent: normalizedContent, newContent }`。

`generateUnifiedPatch(path, oldContent, newContent, contextLines = 4)`：
`Diff.createTwoFilesPatch(path, path, oldContent, newContent, undefined, undefined, { context: contextLines, headerOptions: Diff.FILE_HEADERS_ONLY })`。
**`diff` 库（jsdiff）的输出格式**：`--- path\n+++ path\n@@ -a,b +c,d @@\n...`，`FILE_HEADERS_ONLY` 表示不输出 `Index:` 行。Rust 需保证字节级一致（可用 `similar` 的 unified diff 并手动构造头，或移植 jsdiff 的 `createTwoFilesPatch`）。

`generateDiffString(oldContent, newContent, contextLines = 4)`：
- `parts = Diff.diffLines(oldContent, newContent)`（**按行**的 LCS diff）。
- `oldLines = oldContent.split("\n")`，`newLines = newContent.split("\n")`；`maxLineNum = max(len)`；`lineNumWidth = String(maxLineNum).length`。
- 逐 part：
  - `raw = part.value.split("\n")`；若末元素为 `""` → `pop()`。
  - `added || removed`：若 `firstChangedLine === undefined` → `firstChangedLine = newLineNum`；逐行输出：
    - added：`` `+${String(newLineNum).padStart(lineNumWidth," ")} ${line}` ``，`newLineNum++`；
    - removed：`` `-${String(oldLineNum).padStart(lineNumWidth," ")} ${line}` ``，`oldLineNum++`。
    - `lastWasChange = true`。
  - 上下文行（未变更）：
    - `nextPartIsChange = i < parts.length-1 && (parts[i+1].added || parts[i+1].removed)`；
    - `hasLeadingChange = lastWasChange`；`hasTrailingChange = nextPartIsChange`；
    - 两者皆真：
      - `raw.length <= contextLines * 2` → 全部输出（` NNN line`），两个行号都 ++；
      - 否则输出前 `contextLines` 行；输出跳过行 `` ` ${"".padStart(lineNumWidth)} ...` ``；两个行号各加 `skippedLines`；再输出后 `contextLines` 行。
    - 仅 leading：
      - 输出前 `contextLines` 行；若 `skippedLines > 0` → 输出 ` ...` 行并加进位。
    - 仅 trailing：
      - `skippedLines = max(0, raw.length - contextLines)`；`> 0` → 输出 ` ...` 行并加进位；再输出 `raw.slice(skippedLines)`。
    - 都不是 → 两个行号各加 `raw.length`（完全跳过）。
    - `lastWasChange = false`。
- 返回 `{ diff: output.join("\n"), firstChangedLine }`。
- **格式**：上下文行前缀为空格 + 行号 + 空格 + 文本；跳过行为 `` ` ${padding} ...` ``（一个空格、行号宽度的空格、空格、`...`）。

#### 2.25.11 `tools/bash.ts`（184 行）
```ts
const MAX_TIMEOUT_SECONDS = 2_147_483_647 / 1000;    // 2147483.647
const bashSchema = Type.Object({
  command: Type.String({ description: "Bash command to execute" }),
  timeout: Optional(Type.Number({ description: "Timeout in seconds (optional, no default timeout)" })),
});
// powershellSchema 同上，描述为 "PowerShell command to execute"
const shellStructuredOutputSchema = Type.Object({
  output: Type.String({ description: "Combined stdout and stderr: the retained tail, as the model sees it" }),
  truncated: Type.Boolean({ description: "Whether earlier output was dropped" }),
  fullOutputPath: Optional(Type.String({ description: "File with the complete output, when it was spilled" })),
  exitCode: Type.Number(),
});
export type BashToolInput = Static<typeof bashSchema>;
export type PowerShellToolInput = Static<typeof powershellSchema>;
export type ShellStructuredOutput = Static<typeof shellStructuredOutputSchema>;   // **原 spec 漏列**
export interface BashExecution { command: string; cwd: string; env: Record<string,string>; inheritEnv: boolean }
export type BashPrepare = (execution: BashExecution, api: ToolExecutionApi, context: Context) => void | Promise<void>;
export interface BashToolOptions { commandPrefix?: string; prepare?: BashPrepare }
export interface PowerShellToolOptions { commandPrefix?: string; prepare?: BashPrepare; programs?: readonly string[] }
```
**注意**：`ShellStructuredOutput` 是本文件的第 9 个本地声明（§6.3 的 9 已含它），但 **`tools/index.ts` 的 `export {...} from "./bash.ts"` 里没有它**——`./tools` 子路径对外**不可见** `ShellStructuredOutput`（只能从 `tools/bash.ts` 直接导入）。Rust 侧对应类型应存在但不必出现在 tools 模块的公开面。
`validateTimeout(timeout)`：
- undefined → ok；`!Number.isFinite(timeout) || timeout <= 0` → `Error("Invalid timeout: must be a finite number of seconds")`；
- `timeout > MAX_TIMEOUT_SECONDS` → `Error("Invalid timeout: maximum is ${MAX_TIMEOUT_SECONDS} seconds")`。

`prepareExecution(command, options, api, context)`：
- `command = options?.commandPrefix ? `${options.commandPrefix}\n${command}` : command`；
- `cwd = requireEnv(api).cwd`；`env = {}`；`inheritEnv = true`；
- `await options?.prepare?.(execution, api, context)`。

`runCommand(commands, execution, timeout, api, context)`：
- 对每个命令形式依次尝试 `env.exec(...)`（`cwd`、`env`、`inheritEnv`、`timeout`（若有）、`onOutput: (text, _c, info) => api.output(text, info.skipped)`、`spill: { afterBytes: 51200, afterLines: 2000 }`、`...(api.outputWindow === undefined ? {} : { window: api.outputWindow })`）。
- 若失败且 `error.code === "spawn_error"` → 尝试下一种；否则 break。
- `result === undefined` → `Error("No command to run")`。
- `spillPath = result.ok ? result.value.spillPath : result.error.spillPath`；存在 → `api.diagnostic({ severity:"info", code:"full_output", message: "Full output: ${spillPath}" })` **【逐字】**。
- 失败：`aborted && context.abortSignal?.aborted` → `throw result.error`；`timeout` → `Error("Command timed out after ${timeout} seconds")` **【逐字】**；`aborted` → `Error("Command aborted")` **【逐字】**；其他 → `throw result.error`。
- 成功：`exitCode`；`retained = api.retainedOutput()`；`structuredOutput = { output: retained.text, truncated: retained.truncated, ...(spillPath?{fullOutputPath}:{}), exitCode }`；
  - `exitCode === 0` → `{ structuredOutput }`；
  - 否则 `{ structuredOutput, isError: true, diagnostics: [{ severity:"error", code:"exit_code", message: "Command exited with code ${exitCode}" }] }` **【逐字】**。

`bash` 工具描述 **【逐字】**（`DEFAULT_MAX_LINES = 2000`，`DEFAULT_MAX_BYTES/1024 = 50`）：
```
Execute a bash command in the current working directory. Returns combined stdout and stderr. Output is truncated to last 2000 lines or 50KB (whichever is hit first). If truncated, full output is saved to a temp file. Optionally provide a timeout in seconds.
```
`powershell` 描述相同，仅首句为 `Execute a PowerShell command in the current working directory.`。
`outputLimits: { retain: "tail" }`。
`powershell`：`UTF8_OUTPUT = "try { [Console]::OutputEncoding=[System.Text.Encoding]::UTF8 } catch {}"`；`POWERSHELL_ARGS = ["-NoProfile","-NonInteractive","-ExecutionPolicy","Bypass","-Command"]`；`script = `${UTF8_OUTPUT}\n${execution.command}``；`commands = programs.map(p => [p, ...POWERSHELL_ARGS, script])`（默认 `["pwsh","powershell"]`）。

### 2.26 `src/images/**`

#### 2.26.1 `images/exif.ts`（2.6 KB）

```ts
export function exifOrientation(bytes: Uint8Array): number;   // 1..8；缺失/不可读 → 1
```

`tiffStart(bytes)`：
- JPEG（`ff d8`）：从 offset 2 遍历 marker：非 `0xff` → -1；`0xff` → offset++；`0xe1` 且 `isExifHeader(bytes, offset+4)` → 返回 `offset + 10`；否则 `offset += 2 + ((bytes[offset+2] << 8) | bytes[offset+3])`。
- WebP（`RIFF` @0、`WEBP` @8）：从 offset 12 遍历 chunk：`size = LE u32 @ offset+4`；`ascii(bytes, offset, "EXIF")` → `isExifHeader(bytes, offset+8) ? offset+14 : offset+8`；`offset += 8 + size + (size % 2)`（RIFF 块偶数对齐）。
- 否则 -1。

`exifOrientation(bytes)`：
- `tiff = tiffStart(bytes)`；`tiff < 0 || tiff + 8 > bytes.length` → 1。
- `little = bytes[tiff] === 0x49 && bytes[tiff+1] === 0x49`（`"II"` 小端；否则按大端）。
- `u16/u32` 按端序读取。
- `directory = tiff + u32(tiff + 4)`；`directory + 2 > length` → 1。
- `entries = u16(directory)`；对每个 `entry = directory + 2 + i*12`：越界 → 1；`u16(entry) === 0x0112`（Orientation，SHORT，值在 entry+8）→ 返回 `1..8` 的值，否则 1。
- 未找到 → 1。

`isExifHeader(bytes, offset)`：`ascii(bytes, offset, "Exif") && bytes[offset+4] === 0 && bytes[offset+5] === 0`。
`ascii(bytes, offset, text)`：逐字符比较 `charCodeAt`。

#### 2.26.2 `images/index.ts`（6 KB）— Photon 图像处理器

```ts
const JPEG_QUALITIES = [80, 70, 55, 40];
export function createPhotonImages(wasm: SyncInitInput): ImageProcessor;
```

`createPhotonImages(wasm)`：`initSync({ module: wasm })`；返回 `{ prepare: async (bytes, mimeType, limits) => prepare(bytes, mimeType, limits) }`。
**注意**：Photon 每进程只初始化一次；后续调用复用第一个 `wasm`。

`prepare(bytes, mimeType, limits)`（**精确流程**）：
1. `orientation = exifOrientation(bytes)`；`inline = INLINE_IMAGE_TYPES.has(mimeType)`；`image = undefined`。
2. `try`：
   - `image = PhotonImage.new_from_byteslice(bytes)`（**即使原样返回也要解码**：损坏文件是错误，不是让 provider 拒绝的图片）。
   - 若 `inline && orientation === 1 && width <= maxWidth && height <= maxHeight && base64Length(bytes.byteLength) <= maxBytes` → 返回 `{ data: toBase64(bytes), mimeType }`。
   - `orientation >= 5`：`upright = transposed(image, orientation)`；先 `image.free()` 再 `new PhotonImage(upright.pixels, upright.width, upright.height)`。
   - `orientation !== 1`（2/3/4）：`if (orientation !== 4) fliph(image); if (orientation !== 2) flipv(image);`
   - `width/height` 取当前尺寸；`scale = min(1, maxWidth/width, maxHeight/height)`；
     `target = { width: max(1, round(width*scale)), height: max(1, round(height*scale)) }`。
   - 循环：
     - `encoded = encodeWithin(image, target, maxBytes, mimeType === "image/jpeg")`；
     - 成功 → `resized = (target 与当前尺寸相同) ? {} : { resized: { from: {width,height}, to: target } }`；`converted = encoded.mimeType === mimeType ? {} : { convertedFrom: mimeType }`；返回合并对象。
     - `target === (1,1)` → 返回 undefined。
     - `target = { max(1, floor(w*0.75)), max(1, floor(h*0.75)) }`。
3. `catch` → 返回 undefined（Photon 对无法解码的字节抛错）。
4. `finally` → `image?.free()`。

`encodeWithin(image, size, maxBytes, jpegFirst)`：
- `same = size 与 image 当前尺寸相同`；`scaled = same ? image : resize(image, w, h, SamplingFilter.Lanczos3)`；
- 候选顺序：`jpegFirst ? [...jpegs, png] : [png, ...jpegs]`；`png = get_bytes()`；`jpegs = JPEG_QUALITIES.map(q => get_bytes_jpeg(q))`；
- 第一个 `base64Length(bytes.byteLength) <= maxBytes` 的候选 → `{ data: toBase64(bytes), mimeType }`；都不行 → undefined。
- `finally`：`if (!same) scaled.free()`。

`transposed(image, orientation)`（5–8，宽高互换）：
```
outX = (orientation === 5 || orientation === 8) ? y : height - 1 - y
outY = (orientation === 5 || orientation === 6) ? x : width - 1 - x
```
把源像素 `(x,y)` 的 4 字节复制到 `(outY * height + outX) * 4`；返回 `{ pixels, width: height, height: width }`。
（源码注释：Photon 自己的 `rotate` 在直角处丢像素，所以在此手工搬移。）

**Rust 替代**：`image` crate（解码/缩放/编码）+ `imgref`；EXIF 方向用 `kamadak-exif` 或保留自写解析；缩放滤镜用 Lanczos3（`image::imageops::FilterType::Lanczos3`）；JPEG 质量用 `image::codecs::jpeg::JpegEncoder::new_with_quality`。**必须保证**：JPEG 质量链 [80,70,55,40]、PNG 先/后顺序、`3/4` 收缩直到 1x1、base64 长度按 `ceil(n/3)*4` 判定。

#### 2.26.3 `images/node.ts`（5.2 KB）

```ts
export type NodePhotonImagesOptions = { readonly worker?: boolean; readonly idleMs?: number };  // 默认 true / 30000
export async function createNodePhotonImages(options = {}): Promise<ImageProcessor>;
```

- `worker === false` → `inProcess()`。
- 否则返回处理器：`fallback` 已设置时直接用；否则 `thread ??= new PhotonThread(idleMs, () => { thread = undefined })`；`thread.prepare(...)` 抛 `WorkerUnavailable` → `fallback = inProcess()`；其他错误照抛。
- `inProcess()`：`createRequire(import.meta.url).resolve("@silvia-odwyer/photon/photon_rs_bg.wasm")` + `readFile` → `createPhotonImages`。

`class PhotonThread`：
- 构造：`file = import.meta.url.endsWith(".ts") ? "./node-worker.ts" : "./node-worker.js"`；`new Worker(new URL(file, import.meta.url))`；失败 → `WorkerUnavailable("The Photon image worker cannot start", { cause })`；`worker.unref()`。
- `message`：`#started = true`；按 `reply.id` 找到请求；`"error" in reply` → reject `new Error(reply.error)`；否则 resolve `reply.result`；`#settled()`。
- `error`/`exit`：`fail(error)`；若未 started → 包成 `WorkerUnavailable("The Photon image worker cannot start", { cause: error })`；reject 全部 pending；`#exit()`。exit 的错误为 `Error("The Photon image worker exited with code ${code}")` **【逐字】**。
- `prepare(bytes, mimeType, limits)`：`id = #nextId++`；登记；清 idle 计时器；`worker.ref()`；`copy = bytes.slice()`；`postMessage({ id, bytes: copy, mimeType, limits }, [copy.buffer])`（转移缓冲）。
- `#settled()`：pending 为空 → `worker.unref()`；`#idle = setTimeout(() => { #exit(); worker.terminate() }, idleMs)`；`#idle.unref()`。
- `#exit()`：清 idle 计时器；`#onExit()`。

**Rust**：无需 worker 线程隔离（Photon 的 WASM 内存不会缩回 → Rust 用 `image` crate 无此问题）。可直接同步在 `spawn_blocking` 中执行。

#### 2.26.4 `images/node-worker.ts`（1.4 KB）
Worker 入口：`parentPort` 为 null → `Error("The Photon image worker needs a parent thread")`；`images = createPhotonImages(readFileSync(createRequire(...).resolve(".../photon_rs_bg.wasm")))`；`port.on("message", ...)`：`images.prepare(...).then(result => postMessage({id, result}), error => postMessage({id, error: message}))`。
消息类型：
```ts
export type PrepareRequest = { readonly id: number; readonly bytes: Uint8Array; readonly mimeType: string; readonly limits: ImageLimits };
export type PrepareReply =
  | { readonly id: number; readonly result: PreparedImage | undefined }
  | { readonly id: number; readonly error: string };
```

#### 2.26.5 `images/cloudflare.ts`
```ts
import photonWasm from "@silvia-odwyer/photon/photon_rs_bg.wasm";   // Wrangler CompiledWasm 规则
export function createCloudflarePhotonImages(): ImageProcessor { return createPhotonImages(photonWasm); }
```

#### 2.26.6 `images/photon-wasm.d.ts`
仅类型声明 `declare module "@silvia-odwyer/photon/photon_rs_bg.wasm" { const wasm: WebAssembly.Module; export default wasm; }`（大意）。

### 2.27 `src/tool-search/tool-search.ts`（92 行）

```ts
export const TOOL_SEARCH_TOOL_NAME = "tool_search";
export const DEFAULT_TOOL_SEARCH_LIMIT = 8;
const TOOL_SEARCH_DESCRIPTION = `# Tool discovery\n\nSearches over deferred tool metadata with BM25 and exposes matching tools for the next model call.\n\nSome of the tools, such as tools of MCP servers, may not have been provided to you upfront, and you should use this tool (\`tool_search\`) to search for the required tools. For MCP tool discovery, always use \`tool_search\`.`;
const toolSearchSchema = Type.Object({
  query: Type.String({ description: "Search query for deferred tools." }),
  limit: Optional(Type.Number({ description: "Maximum number of tools to return. Defaults to 8." })),
});
export type ToolSearchToolDetails = { loaded: string[] };
export function createToolSearchTool(): ToolRegistration;
export function createToolSearchExtension(): Extension;      // name "tool-search"
```

`searchText(tool)`：`[tool.name, tool.name.replaceAll("_", " "), tool.description, schemaText(tool.parameters)]` 过滤 `part.trim() !== ""` 后 `join(" ")`。

`schemaText(schema, parts)`（递归）：
- 非对象 → 返回。
- `typeof schema.description === "string"` → push。
- `schema.properties` 是对象 → 对每个 `[name, property]`：push `name`，递归 `property`。
- 递归 `schema.items`。
- 对 `["anyOf","oneOf","allOf"]`：数组则逐个递归。

`execute({ query, limit }, api, context)`：
1. `query.trim() === ""` → `throw new Error("query must not be empty")` **【逐字】**。
2. `max = limit ?? 8`；`!Number.isInteger(max) || max <= 0` → `throw new Error("limit must be a positive integer")` **【逐字】**。
3. `candidates = (await api.agent(context)).deferred`。
4. `documents = candidates.map(t => ({ name: t.name, text: searchText(t) }))`。
5. `loaded = rankDocuments(query, documents, max)`。
6. 文本：
   - 空 → `"No matching tools found."` **【逐字】**；
   - 否则 `` `Loaded ${loaded.length} tool${loaded.length === 1 ? "" : "s"}. They are available from your next call:\n${loaded.map(name => `- ${name}: ${(candidates.find(t => t.name === name)?.description ?? "").trim().split(/\r?\n/)[0]}`).join("\n")}` ``。
7. `details = { loaded }`；返回 `{ output: [{ type:"text", text }], details, ...(loaded.length === 0 ? {} : { control: { addTools: loaded } }) }`。
- 注册属性：`callers: ["model"]`、`replay: "safe"`。

### 2.28 `src/codemode/**`

#### 2.28.1 `codemode/index.ts`（逐字，共 2 个 export 语句）
```ts
export {
	CODEMODE_MODULE_VERSION,
	CODEMODE_TOOL_NAME,
	type CodemodeGlobal,
	type CodemodeNestedCall,
	CodemodeStoreDoc,
	type CodemodeToolDetails,
	type CodemodeToolInput,
	type CodemodeToolOptions,
	createCodemodeExtension,
	createCodemodeTool,
} from "./codemode.ts";
export { type CodemodeNamespace, DEFAULT_CODEMODE_INLINE_BUDGET } from "./describe.ts";
```
**明确**：`codemode/index.ts` **不**导出 tool-search 的符号（那是 `tool-search/index.ts` 的职责），也**不**导出 `CodemodeRunDoc`/`CodemodeJournalDoc`/`createModelTools`/`MODEL_CALLS`/`CATALOG_GLOBALS`/`MAX_CONCURRENT_MODEL_CALLS`/`GENERATE_IMAGES_TOOL`/`CLASSIFY_TOOL`/`CODEMODE_INTERNAL_PREFIX`（它们只在包内可见）。

#### 2.28.2 `codemode/describe.ts`（157 行）— 描述与系统提示段

```ts
export type CodemodeNamespace = { readonly name: string; readonly description?: string; readonly instructions?: string; readonly prefix: string };
export const DEFAULT_CODEMODE_INLINE_BUDGET = 3000;
const CHARS_PER_TOKEN = 4;
const TEXT_OUTPUT_SCHEMA: CodemodeJsonSchema = { type: "string" };
const DESCRIPTION_INTRO = `Run JavaScript that calls other tools. The input is raw JavaScript (not JSON, no code fence), run as an async function body in a QuickJS sandbox: top-level \`await\` and \`return\` work. No Node, file system, network, or timers.
- \`await tools.<name>({ ...args })\` resolves to a string, or an object if the tool's declaration says so, and rejects with an Error on failure. A tool whose declaration returns an object resolves on failure too when it returned that object; check its fields. Calls still running when the script ends are cancelled.
- The script survives a restart: it resumes where it was, without running finished calls again.
- Optional first line: \`// @options: {"max_output_tokens": 10000, "timeout_ms": 60000}\``;
export function codemodeDescription(models: boolean): string;
export function declarationOf(tool: ToolRegistration): { name; description; inputSchema; outputSchema };
export function sampleOf(tool: ToolRegistration): string;       // renderToolSample(declarationOf(tool))
export function namespaceOf(name: string, namespaces): CodemodeNamespace | undefined;
export function codemodeSection(tools, namespaces, budget): string | undefined;
```

`codemodeDescription(models)` **逐字拼装**：
```
[DESCRIPTION_INTRO,
 globals.join("\n"),
 ...(models ? [MODELS_DESCRIPTION] : []),
 "The `codemode` section of the system prompt lists the tools scripts can call, by namespace, as far as space allows."].join("\n\n")
```
其中 `globals` **逐字**：
```
Globals:
- `text(value)`, `image(dataUrlOrImageBlock)`, `console.log(...)`, and top-level `return` add output; `exit()` ends the script. With several text items, each starts with a `==> text N/M <==` line, and `console` lines follow the other output in one `<console_output>` block. `image()` also saves the image to a temp file and the result names its path.
- `store(key, value)` and `load(key)` keep JSON values across codemode calls.
- `ALL_TOOLS`, `await searchTools(query, { limit?, namespace? })`, `await describeTool(name)`, `await describeNamespace(name)`: find tools the `codemode` section does not list.
```

`declarationOf(tool)`：`outputSchema = tool.structuredOutputSchema ?? { type: "string" }`。

`namespaceOf(name, namespaces)`：最长 `prefix` 命中者（`name.startsWith(prefix)` 且比已找到的更长）。

`renderToolSection(tool)`：`id = toCodemodeIdentifier(tool.name)`；标题 `### \`${id}\`` 或（当 `id !== tool.name`）`### \`${id}\` (\`${tool.name}\`)`；正文 `` `${heading}\n${sampleOf(tool).trim()}` ``。

`selectCatalog(groups, budget)`（**预算算法，精确**）：
- 每组队列按 `cost` 升序；
- 循环：`active = active.filter(queue => { next = queue[0]; if (next.cost > remaining) return false; remaining -= next.cost; shown.add(next.name); queue.shift(); return queue.length > 0 })`；
- 直到 `active` 为空。语义：**每一轮每个组（先无命名空间组，再按名字排序的命名空间）放置其最便宜的剩余工具；某组的下一个工具放不下时该组退出，其他组继续；因此任何命名空间都先被表示，再谈完整。**

`codemodeSection(tools, namespaces, budget)`：
- `tools.length === 0` → undefined。
- 分组：`groups = new Map([["", {namespace: undefined, entries: []}]])`；每个 tool 找到 `namespaceOf`，key = `namespace ? `ns:${name}` : ""`；生成 `section = renderToolSection(tool)`；`cost = Math.ceil(section.length / 4)`。
- 排序：无命名空间组**最前**；其余按 `namespace.name.localeCompare` 升序。
- `shown = selectCatalog(ordered, budget)`。
- 输出行 `["Tools scripts can call (`await tools.<name>(args)`):"]`，然后对每组：
  - 空组跳过；`visible = entries.filter(shown)`；
  - 有 namespace：`listing = visible.length === entries.length ? "" : visible.length === 0 ? " (tools not listed)" : " (some tools not listed)"`；`description = namespace.description?.trim()`；push `` `## ${namespace.name}${listing}${description ? `\n${description}` : ""}` ``；
  - 无 namespace 且有省略：push ``"Some tools are not listed: find them with `searchTools()`."``；
  - 逐个 push `entry.section`。
- `join("\n\n")`。
- **【逐字标题】**：`` `## ${namespace.name}${listing}` ``；无命名空间省略行：``Some tools are not listed: find them with `searchTools()`.``

#### 2.28.3 `codemode/models.ts`（287 行）

常量（**逐字**）：
```ts
export const CODEMODE_INTERNAL_PREFIX = "kiana.codemode.";
export const CLASSIFY_TOOL = "kiana.codemode.models.classify";
export const GENERATE_IMAGES_TOOL = "kiana.codemode.models.generateImages";
export const MODEL_CALLS: Record<string,string> = { "models.classify": CLASSIFY_TOOL, "models.generateImages": GENERATE_IMAGES_TOOL };
export const CATALOG_GLOBALS = ["models.getModelsOfType","models.getAvailableOfType","models.getModelOfType"];
export const MAX_CONCURRENT_MODEL_CALLS = 4;
const MODEL_TYPES = new Set(["chat","image","classifier"]);
export const MODELS_DESCRIPTION = [...].join("\n");
```
`MODELS_DESCRIPTION` **逐字** 5 行见源码（`models` 命名空间说明、`getModelsOfType` 系列、`classify`、`generateImages`、并发上限）。

`describeValue(value)`：undefined/null → `String(value)`；数组 → 空 `"an empty array"` 否则 `"an array"`；对象 → 空 `"{}"` 否则 `{ k1, k2, ... }`（最多 6 个，超出加 `", ..."`）；string → `"a string"`；其他 → `` `a ${typeof value}` ``。
`withArticle(word)`：元音开头 → `an`，否则 `a`。
`toModelType(value)`：string 且在集合内 → 返回；否则 `Error("Unknown model type ${JSON.stringify(value)}. Use \"chat\", \"image\", or \"classifier\".")` **【逐字】**。
`toProvider(value)`：undefined/null → undefined；非 string → `Error("provider must be a string")`。
`toModelInfo(model)`：`{ ...model }` 后 `delete info.headers`；再 `JSON.parse(JSON.stringify(info))`（去 credentials）。
`catalogCall(name, args, models, context)`：
- `list = Array.isArray(args) ? args : []`；`[type, provider, id] = list`。
- `models.getModelsOfType` → `models.getModelsOfType(toModelType(type), toProvider(provider)).map(toModelInfo)`。
- `models.getAvailableOfType` → `await` 同上 + `{ signal }`。
- 否则（`getModelOfType`）：`provider`/`id` 非 string → **【逐字】**
  `` `models.getModelOfType(type, provider, id) expects three strings, got (${list.map(describeValue).join(", ")}). The provider and the id are separate arguments, for example models.getModelOfType("classifier", "typesafe", "jev-latest").` ``；
  然后 `models.getModelOfType(...)` → undefined 或 `toModelInfo`。

`CLASSIFIER_CONTEXT_SHAPE` **逐字**（用于错误）：
```
{ state: { ... }, images?: [{ type: "image", data: <base64>, mimeType }], questions: { <id>: { type: "choice", instructions, criteria: { <label>: <meaning> } } | { type: "score", instructions, criteria: [<lowest level>, ..., <highest level>] } | { type: "bool", instructions, criteria: { true: <meaning>, false: <meaning> } } } }
```

`checkClassifierContext(context)` 的**全部错误文案**【逐字，前缀 `models.classify() ${problem}. Expected context: ${CLASSIFIER_CONTEXT_SHAPE}.`】：
- 非对象 → `expects a context object as its second argument, got ${describeValue(context)}`
- `context.state` 非对象 → `context.state must be an object, got ${...}`
- `images` 给定但非数组 → `context.images must be an array, got ${...}`
- 某 image 非 `{type:"image",data:string,mimeType:string}` → `context.images[${i}] must be an image block, got ${...}`
- `questions` 非对象或空 → `context.questions must map question IDs to questions, got ${...}`
- 某 question 非对象 → `context.questions.${id} must be a question object, got ${...}`
- `instructions` 非 string → `context.questions.${id}.instructions must be a string`
- `choice` 且 criteria 非对象或值非全 string → `context.questions.${id} is a "choice" question, so criteria must map each label to its meaning`
- `score` 且 criteria 非非空 string 数组 → `... is a "score" question, so criteria must list the levels as strings, lowest first`
- `bool` 且 criteria 非 `{true:string,false:string}` → `... is a "bool" question, so criteria must be { true: string, false: string }`
- 其他 type → `context.questions.${id}.type must be "choice", "score", or "bool", got ${JSON.stringify(question.type)}`

`checkImagesContext(context)` 错误前缀 **【逐字】**：`models.generateImages() ${problem}. Expected context: { input: [{ type: "text", text: <prompt> }, ...optional { type: "image", data: <base64>, mimeType } references] }.`
- 非对象 → `expects a context object as its second argument, got ${...}`
- `input` 非非空数组 → `context.input must be a non-empty array of blocks, got ${...}`
- 某块非 text/image 合法块 → `context.input[${i}] must be a text or image block, got ${...}`

`resolveModel(name, type, model, models)`：
- `listHint = `List the ${type} models you can use with models.getAvailableOfType("${type}").``；
- 非对象或 `provider`/`id` 非 string → `Error(`${name}() expects ${withArticle(type)} model as its first argument, got ${describeValue(model)}.${undefinedHint} ${listHint}`)`，其中 `undefinedHint = (model === undefined || model === null) ? " models.getModelOfType() returns undefined for an unknown provider or id." : ""`；
- `models.getModelOfType(type, provider, id)` 命中 → 返回；
- 否则找"另一种类型存在"：有 → `"${provider}/${id}" is ${withArticle(actualType)} model, not ${withArticle(type)} model. ${listHint}`；否则 `Unknown ${type} model "${provider}/${id}". ${listHint}` **【逐字】**。

`ModelCallParameters = Type.Object({ model: Type.Unknown(), context: Type.Unknown() })`。

`modelResult(label, result, value)`：
- `ok = result.stopReason === "stop"`；
- `output: [{ type:"text", text: ok ? `${label} done` : `${label} ${result.stopReason}: ${result.errorMessage ?? ""}` }]`；
- `structuredOutput: JSON.parse(JSON.stringify(value))`；
- `...(ok ? {} : { isError: true })`；`...(result.usage === undefined ? {} : { usage: result.usage })`。

`createModelTools()`：
- `classify`：`name: "kiana.codemode.models.classify"`、`description: "models.classify() of code mode scripts."`、`parameters: ModelCallParameters`、`structuredOutputSchema: Type.Unknown()`、`callers: ["tools"]`；`execute`：`resolveModel("models.classify","classifier",args.model,api.models)` → `checkClassifierContext(args.context)` → `api.models.classify(model, checked, { telemetry: context.telemetry, signal? })` → `modelResult(`classify ${provider}/${id}`, result, result)`。
- `generateImages`：同上，description `"models.generateImages() of code mode scripts."`，`resolveModel("models.generateImages","image",...)`，`checkImagesContext`，`modelResult(`generateImages ${provider}/${id}`, ...)`。

#### 2.28.4 `codemode/output.ts`（206 行）— 脚本输出格式化

```ts
export type OutputFile = (name: string, data: string | Uint8Array) => Promise<{ path: string } | { error: string }>;
type Limits = { readonly maxBytes: number; readonly maxLines: number };
const LINE_MARGIN = 100;
export const DEFAULT_MAX_OUTPUT_TOKENS = 10_000;
const CHARS_PER_TOKEN = 4;
const IMAGE_EXTENSIONS = { "image/png": ".png", "image/jpeg": ".jpg", "image/gif": ".gif", "image/webp": ".webp" };
export async function formatResult(result: CodemodeResult, options: {...}): Promise<{ content: Item[]; fullOutputPath?: string }>;
```

`valueText(value)`：string → 原样；否则 `JSON.stringify(value) ?? String(value)`。

`formatItems(output)`：
- `total = output 中 type==="text" && !console 的个数`；
- 逐个：image → 直接 push；console → 收集行；text → `index++`；`text: total > 1 ? `==> text ${index}/${total} <==\n${item.text}` : item.text`；
- console 行数 > 0 → push `{ type:"text", text: `<console_output>\n${lines.join("\n")}\n</console_output>` }`。

`joinAdjacentText(items)`：合并相邻 text（若 `lastPart !== "" && !lastPart.endsWith("\n")` 则插入 `"\n"`）；image 打断合并。（源码注释：只看最后一个非空片段，避免 V8 每次复制增长中的字符串。）

`truncate(items, maxTokens, limits, write)`（**逐字**）：
```ts
const texts = items.filter((item): item is TextContent => item.type === "text").map((item) => item.text);
const combined = texts.join("\n");
// Within the limits too: a UTF-16 unit is at most 3 UTF-8 bytes, and the rest of the result is a few short lines.
const budget = Math.min(maxTokens * CHARS_PER_TOKEN, Math.floor(limits.maxBytes / 4));
const lineBudget = Math.floor((limits.maxLines - LINE_MARGIN) / 2);          // LINE_MARGIN = 100
const lines = combined.split("\n").length;
if (texts.length === 0 || (combined.length <= budget && lines <= 2 * lineBudget)) return { items: [...items] };
const headChars = Math.floor(budget / 2);
const tailChars = budget - headChars;
const head = combined.slice(0, headChars).split("\n").slice(0, lineBudget).join("\n");
const tail = tailChars > 0 ? combined.slice(-tailChars).split("\n").slice(-lineBudget).join("\n") : "";
const removed = combined.length - head.length - tail.length;
let text = `Warning: truncated output (original token count: ${Math.ceil(combined.length / CHARS_PER_TOKEN)})\nTotal output lines: ${combined.split("\n").length}\n\n${head}…${Math.ceil(removed / CHARS_PER_TOKEN)} tokens truncated…${tail}`;
const spilled = await write("output.txt", combined);
text += "path" in spilled
	? `\n\n[Full output: ${spilled.path} (read with offset/limit)]`
	: `\n\n[Could not save the full output: ${spilled.error}]`;
return {
	items: [{ type: "text", text }, ...items.filter((item) => item.type === "image")],
	...("path" in spilled ? { fullOutputPath: spilled.path } : {}),
};
```
**注意**：省略号是 `…`（U+2026），不是 `...`；`combined.length`、`slice` 全部是 **UTF-16 code unit** 语义（Rust 需用 UTF-16 视图，见 §6.2 与 §8.3）。

`saveImages(items, write)`：
```ts
const labels = new Map<string, Promise<string>>();           // 按 image data 去重
const label = async ({ data, mimeType }: ImageContent, index: number): Promise<string> => {
	const bytes = Uint8Array.from(atob(data), (char) => char.charCodeAt(0));
	const kind = `${mimeType}, ${bytes.length} bytes`;
	const saved = await write(`image-${index}${IMAGE_EXTENSIONS[mimeType] ?? ".bin"}`, bytes);
	return "path" in saved
		? `[Image saved to ${saved.path} (${kind})]`
		: `[Image (${kind}) could not be saved: ${saved.error}]`;
};
// 逐项：非 image 原样；image 先取/建 label（label index = labels.size + 1），再返回 [{type:"text", text: await pending}, item]
// 最后 result.flat()
```
**注意**：`Promise.all` 并发写文件，但 label 的**编号**按首次遇到的顺序（`labels.size + 1`）；相同 `data` 复用同一 label 与同一编号。

`formatError(result, calls)`：
- `head`：
  - `error.kind === "script"` → `error.stack ?? `${error.name ?? "Error"}: ${error.message}``；
  - `"timeout"` → `Script timed out: ${error.message}`；
  - `"aborted"` → `Script aborted: ${error.message}`；
  - 否则 → `Script sandbox failed: ${error.message}`；
- `summary`：`calls.length === 0 ? "No tool calls were made." : `Tool calls made before the failure (they are not undone): ${calls.map(c => `${c.name} (${c.status})`).join(", ")}``；
- 返回 `` `${head}\n\n${summary}` ``。

`formatResult(result, options)`：
- `output = [...result.output]`；`result.ok && result.value !== undefined` → push `{ type:"text", text: valueText(result.value) }`；
- `items = formatItems(output)`；
- `!result.ok` → push `{ type:"text", text: `Script error:\n${formatError(result, options.calls)}` }`；
- `generated > 0 && items 中无 image` → push 提示 `` `Note: models.generateImages() returned ${generated} image${generated === 1 ? "" : "s"} that the script did not show. Show each image block of result.output with image(block).` ``；
- `truncated = await truncate(joinAdjacentText(items), maxOutputTokens, limits, write)`；
- `content = joinAdjacentText(await saveImages(truncated.items, write))`；
- `header = `${result.ok ? "Script completed" : "Script failed"}\nWall time ${(wallMs / 1000).toFixed(1)} seconds\nOutput:\n``；
- 返回 `{ content: [{ type:"text", text: header }, ...content], ...(fullOutputPath? {fullOutputPath}:{}) }`。

#### 2.28.5 `codemode/codemode.ts`（1046 行）— **最复杂模块**

##### 常量与文档（逐字）
```ts
export const CODEMODE_MODULE_VERSION = 2;
export const CODEMODE_TOOL_NAME = "codemode";

type RunState = {
	start?: Start;
	/** Directory of the result's files (the full output, images), created once: a rerun writes the same files again. */
	outputDir?: string;
	/** The outcome, decided once. */
	result?: JsonObject;
};

/** One tool as the script sees it: `tools.<jsName>`. `structured`: the tool declares `structuredOutputSchema`, so an
 * error result with a value resolves with it. `hidden`: an internal tool of this extension (`kiana.codemode.*`), which
 * scripts reach only through a global such as `models.classify()`; never in `tools`, `ALL_TOOLS`, or tool search. */
type DeclaredTool = { name: string; jsName: string; description: string; structured?: true; hidden?: true };

/** What a call starts with; a rerun starts the VM from exactly this. **原 spec 漏了整个 `Start` 类型** */
type Start = {
	version: number;
	startedAt: number;
	seed: number;
	/** The VM's fingerprint, recorded by the first run before any call is admitted. */
	fingerprint?: string;
	/** `startedAt + timeout_ms` when the script set one. */
	deadline?: number;
	tools: DeclaredTool[];
	/** The namespaces of the tools, as `describeNamespace()` and `searchTools()` see them. */
	namespaces: { name: string; description?: string; instructions?: string; tools: string[] }[];
	globals: { name: string; spread: boolean }[];
	/** `max_output_tokens` of the script's options line, or the default. */
	maxOutputTokens: number;
	store: Record<string, JsonValue>;
};
```
**`Start`/`DeclaredTool` 是 `kiana.codemode.run` 文档的全部持久内容**——`start` 一旦写入只增一个字段 `start.fingerprint`（首次指纹）；`outputDir`/`result` 属 `RunState` 层。`start.tools` 存的是**首次运行时的目录快照**，重启后哪怕工具已变化也按快照重放。

```ts
export const CodemodeRunDoc = defineDoc<RunState>({
	kind: "kiana.codemode.run",
	version: 1,
	scope: "task",
	initial: () => ({}),
	// The document retires with its task.
	checkpointWhen: () => false,
});

type JournalEntry = {
	/** The VM's call ID. Its result is the entry's position + 1 (`seq`). */
	id: number;
	/** The VM clock while it processes this result. */
	at: number;
	/** Calls the VM had issued after processing this result; absent until acknowledged. */
	issuedAfter?: number;
	/** A global's call and outcome; a tool's result is its nested call's. */
	global?: { name: string; args?: JsonValue; outcome: JsonObject };
};

type JournalState = {
	entries: JournalEntry[];
	/** Calls the VM had issued after the script's first run, before any result; absent until acknowledged. */
	firstIssued?: number;
};

export const CodemodeJournalDoc = defineDoc<JournalState>({
	kind: "kiana.codemode.journal",
	version: 1,
	scope: "task",
	initial: () => ({ entries: [] }),
	checkpointWhen: () => false,
});

export const CodemodeStoreDoc = defineDoc<{ values: Record<string, JsonValue> }>({
	kind: "kiana.codemode.store",
	version: 1,
	scope: "conversation",
	history: "rewindable",
	fork: "asOf",
	initial: () => ({ values: {} }),
	checkpointWhen: (_value, _ops, { deltasSinceBase }) => deltasSinceBase >= 64,
});
```
**注意**：task 作用域文档按类型层禁止 `history`/`fork` 字段（`types.ts` 中 task/session 分支为 `history?: never; fork?: never`），因此 run/journal 文档**没有**这两个字段，`documentCreate` 也不会写入它们；只有 conversation 作用域文档才写 `history`/`fork`。

##### 类型（逐字）
```ts
/** One tool call a script made, as the code mode call's details show it. */
export type CodemodeNestedCall = {
	/** The VM's number for the call; its nested call ID is `<callId>/c<id>`. */
	id: number;
	name: string;
	/** Compact JSON of the arguments, cut for display. */
	args: string;
	status: "running" | "ok" | "error" | "cancelled";
	durationMs?: number;
	/** Error text, cut for display. */
	error?: string;
};

/** The code mode call's details, in its result: its tool calls in the order it made them, with their status. Calls to
 * globals are not listed. While the script runs, UIs show its nested calls from `kiana.live.nestedTools`, so the list is
 * written once, with the result. */
export type CodemodeToolDetails = {
	calls: CodemodeNestedCall[];
	/** Temp file with the complete text output, when the output was cut to its token budget. */
	fullOutputPath?: string;
};

const ARGS_PREVIEW_CHARS = 200;
const ERROR_PREVIEW_CHARS = 500;

function cut(text: string, max: number): string {
	// A copy: the row outlives the arguments or error text it previews.
	return text.length > max ? `${ownedSlice(text, 0, max - 3)}...` : text;
}

/** A function scripts call as a top-level global; its result is journaled. */
export type CodemodeGlobal = {
	readonly name: string;
	readonly spread?: boolean;
	execute(args: unknown, api: ToolExecutionApi, context: Context): Promise<JsonValue | undefined>;
};

const codemodeSchema = Type.Object({ code: Type.String({ description: "Raw JavaScript source." }) });

export type CodemodeToolInput = Static<typeof codemodeSchema>;

export type CodemodeToolOptions = {
	/** Runs the scripts: `CodemodeSandbox` on Node, `RemoteCodemodeSandbox` on Cloudflare. */
	readonly sandbox: CodemodeSandboxBase;
	/** Groups of tools by name prefix, such as MCP servers, for the `codemode` section and tool search. */
	readonly namespaces?: readonly CodemodeNamespace[];
	/** Estimated tokens the tool declarations in the `codemode` section may use. Default 3000. */
	readonly inlineBudget?: number;
	/** Give scripts `models.*`: the model catalog, classifiers, and image generation. Default true. */
	readonly models?: boolean;
	/** @internal Host functions scripts call as globals. */
	readonly globals?: readonly CodemodeGlobal[];
};

/** Outputs bounded by the script's own token budget; the Harness's limits only stop a runaway. */
const OUTPUT_LIMITS = { maxBytes: 4 * 1024 * 1024, maxLines: 100_000 } as const;

/** Globals for finding tools, answered from the start data, so a replay sees what the first run saw. */
const DISCOVERY_GLOBALS = ["searchTools", "describeTool", "describeNamespace"] as const;
const DEFAULT_SEARCH_LIMIT = 8;
```
**注意**：`CodemodeToolOptions.sandbox` 是**必填**；`timeoutMs`/`maxOutputTokens`/`outputDir` 不在此类型中（分别来自脚本 options 行与 `api`），不要按旧版 API 假设。

##### 工具结果映射辅助（逐字）
```ts
function outcomeOf(name: string, structured: boolean, result: NestedToolExecutionResult): CodemodeOutcome {
	const value = structured ? result.structuredOutput : textOf(result.structuredOutput);
	if (!result.isError || (structured && value !== undefined)) return { ok: true, value };
	const message = structured ? "" : (value as string);
	const diagnostics = result.diagnostics
		.filter((diagnostic) => diagnostic.severity === "error")
		.map((diagnostic) => diagnostic.message)
		.join("\n");
	return {
		ok: false,
		message: message || diagnostics || `Tool ${name} failed`,
		...(value === undefined ? {} : { properties: { structuredOutput: value } }),
	};
}

function textOf(value: JsonValue | undefined): string {
	if (typeof value === "string") return value;
	if (!Array.isArray(value)) return "";
	return value.flatMap((item) => (isText(item) ? [item.text] : [])).join("\n");
}
```

##### `CodemodeCall` 生命周期（关键语义）
`CodemodeCall` 表示一次脚本执行，围绕"确定性重放"：
- **启动**：读取 run 文档（`start` 数据）；校验 `version`（`CODEMODE_MODULE_VERSION`）与 deadline；不一致 → 结束并报错。
- **沙箱**：`sandbox.start(code, globals)`；每次 `tools.<name>(args)` 成为一次 **嵌套调用**（`api.executeTool`），其 `key` 由调用顺序决定。
- **journal**：每次调用的结果在**投递前**先提交 journal（`durableIssued`）；`firstIssued` 与 `durableIssued` 分离，保证"已投递结果"不早于"已持久记录"。
- **重放**：重启后按 journal 顺序重放结果；脚本若产生与记录不符的调用序列 → 分叉错误（见下）。
- **模型调用**：`models.classify`/`generateImages` 走内部工具 `kiana.codemode.models.*`，每个槽位（`modelSlot`）最多 `MAX_CONCURRENT_MODEL_CALLS = 4` 并发，脚本结束后不再启动等待槽位的调用。
- **输出**：`outputFiles`/`outputDir` 决定 `image-N.ext` 与 `output.txt` 的落盘位置；`decide/complete` 生成结果。

**分叉检测（divergence）规则与文案**（**逐字**，测试 `codemode.test.ts` 逐条覆盖；下表左列为 `it` 标题对应的规则）：

`diverge(message)` 的产出**固定为**：
```ts
const result = errorResult(`The script took a different path when resumed after a restart: ${message}`);
// errorResult(m) = { output: [{ type: "text", text: m }], isError: true }
this.ended = result;
this.decide(result).then(() => this.controller.abort(new Error(message)), (error) => this.fail(error));
```
`describe(call)` = `` `${call.target === "tool" ? "tools." : ""}${call.name}(${JSON.stringify(call.args) ?? ""})` ``（**逐字**；`JSON.stringify(undefined)` 为 `undefined`，故 `?? ""` 生效）。

**7 条 `diverge()` 调用点（逐字模板）**：
| 位置 | message 模板 | 触发规则 |
|---|---|---|
| `deliver` 全局 vs 记录 | `` `call ${call.id} is ${describe(call)}, but was a call to ${entry.global.name}` `` | 第 10 条（重放在早前 global 处发工具调用，反之亦然） |
| `deliver` 工具 vs 记录 | `` `call ${call.id} is ${describe(call)}, but was a tool call` `` | 同上 |
| 全局结果分叉 | `` `call ${call.id} is ${describe(call)}: ${errorMessage(error)}` `` | 第 3 条（同 global 不同参数） |
| `onIdle` 计数不符（seq 0） | `` `its first run made ${issued} calls, where it had made ${recorded}` `` | 第 20/21 条（首跑调用数不符） |
| `onIdle` 计数不符（seq>0） | `` `after result ${seq} it had made ${issued} calls, where it had made ${recorded}` `` | 同第 1 条（调用数不同） |
| 重放等待却未发调用 | `` `it waits without making call ${next.id}, which it made before` `` | 第 2 条（立即结束） |
| `release` 早前尝试键冲突 | `` `call ${call.id} is ${describe(call)}, but was ${other}` `` | 第 7 条（早前尝试以其它参数接纳过同一 key） |

**另有 6 条非 `diverge` 的结束路径（逐字；原 spec 只有 5 条，漏了第 6 条）**：
- 版本不符：`` `The script cannot resume: it started under code mode version ${this.start.version}, this is ${CODEMODE_MODULE_VERSION}.` ``（`errorResult` → `decide`）。
- 截止时间已过：`` `Script timed out: its deadline passed while it was interrupted.` ``。
- 沙箱指纹不符：`` `The script cannot resume: the sandbox changed since it started (${recorded} -> ${fingerprint}).` ``，随后 `controller.abort(new Error("Sandbox fingerprint changed"))`（**注意两条消息不同**）。
- **重放结束点晚于/等于记录（"continue" 判定）**：`` `The script took a different path when resumed after a restart: it ended after result ${during.seq}, where it went on before.` ``——**这条不走 `diverge()`**：直接 `return this.decide(errorResult(...))`，因此**不置 `this.ended`、不 abort controller**（与 7 个 `diverge()` 调用点在副作用上不同；Rust 侧若把它并入 `diverge()` 会多一次 abort，属可观察差异）。触发条件：`this.rerun && during !== undefined && during.seq <= this.replayed.length` 且 `continued`——其中 `continued` = （`during.seq === 0` ? `replayed.length > 0 || firstIssued !== undefined` : `replayed[during.seq-1]?.issuedAfter !== undefined`）`|| replayed.slice(during.seq).some(entry => entry.id > during.issued)`。
- 脚本结束后再发调用：`throw new Error("The script has ended")`（在 `executeTool` 包装里，被 `outcomeOf` 捕获为失败结果）。**两处来源**：(a) `callTool` 里等待 `modelSlot()` 期间脚本结束 → `releaseModelSlot()` 后抛出；(b) 沙箱在已结束后仍发调用。
- 沙箱在脚本运行期间关闭：`throw new Error("The code mode sandbox was closed while the script ran")`——**前置条件很特殊**：`!result.ok && result.error.kind === "aborted" && !this.controller.signal.aborted`（即中止来自宿主而非本调用），此时先 `await` 到 `context.abortSignal` 也中止（若已有/未中止则立即），再 `throwIfAborted()` 后抛；**不写任何结果**，留给下次重放（测试 "stores no outcome when the host closes the sandbox, and replays the script after a restart"）。

**12 条规则文字（对应测试）**：
1. 重放脚本的调用数与记录不同 → 结束。
2. 重放在某处等待却没有发出此前已发出的调用 → 立即结束。
3. 重放用不同参数调用同一 global → 结束。
4. 分叉的重放中**任何**调用都不被接纳（即使脚本从未发过）。
5. 重放在此前继续处结束 → 结束。
6. 重放在此前调用之前结束（即使没有记录计数）→ 结束。
7. 重放发出早前尝试已接纳但参数不同的调用 → 结束。
8. 早前尝试未记录的计数，一旦重放确认即记录。
9. 每次重放运行都把新的调用与早前尝试的调用比较，然后才接纳后续调用。
10. 重放在早前尝试发工具调用处发 global 调用（反之亦然）→ 结束。
11. 无 journal 条目的重放在首次运行中结束于此前继续处 → 结束。
12. 重放持有的每个调用都要在启动任何一个之前检查。

**沙箱指纹**：`sandbox fingerprint changed` → 拒绝恢复（`"Sandbox fingerprint changed"`）。
**超时**：跨重启计时；超时后不再接纳新调用（`"Script timed out: ..."`）。
**已决结果**：重启后直接返回已决 outcome，不再运行脚本。
**store()**：按会话隔离；fork 的 `asOf` 快照（`keeps store() values per conversation, as of a fork's entry`）。
**journal 批量**：同时完成的结果在**一次提交**中记录（`journals results that finish together in one commit`）。

**工具结果映射**：
- 带 `structuredOutputSchema` 的工具：成功 → 校验后的 `structuredOutput`；失败但仍带该值 → 同样 resolve；失败且无值 → 以错误文本 reject。
- 无 schema 的工具：resolve 其文本（即使返回了图片）；错误 → 以文本 reject。
- 全局 `undefined` 结果在 live 与 replay 中都保持 `undefined`。
- 脚本结束后到达的 global 结果被丢弃。

**输出的 `details`**：`calls` 列出脚本的工具调用（name/status/参数预览 200 字符/错误预览 500 字符）。

**`CodemodeCall` 内部机制补遗（原 spec 只给语义概述，以下为实现细节，移植时必须一致）**：

字段（逐字名）：`replayed: JournalEntry[]`（本次启动时 journal 的内容）、`journalLength`（重放 + 实时的总条目数；下一次投递是 `journalLength + 1`）、`nextReplayed`（已投递的重放条数）、`replaying`、`rerun`（早前尝试跑过本调用）、`released`（是否允许实时调用启动；之前一律 held）、`firstIssued`、`durableIssued`（已提交的最大 issued 计数）、`waiting: CodemodeHostCall[]`、`replayIndex: Map<number, number>`（**VM 调用 id → journal 下标**）、`issued`、`ready: Map<number, CodemodeOutcome>`（重放：按 journal 顺序待投递的结果）、`held: CodemodeHostCall[]`、`queue`（实时：已完成的调用等 journal 提交）、`acks: Map<number, number>`（`seq → issued`；0 = 首跑）、`committing`、`ended`、`failure`、`modelCalls`/`modelWaiters`、`generatedImages`、`rows`、**`gate: Promise<void>` + `openGate()`**。

- **`execute()` 的前置提交**（一次 `api.commit` 里做完）：读 `CodemodeRunDoc`；`run.result !== undefined` → **直接返回已决结果（不启 VM）**；`rerun = run.start !== undefined`；首跑时在同一次提交里写 `run.start = { version, startedAt: now, seed: crypto.getRandomValues(new Uint32Array(1))[0], ...(timeoutMs===undefined?{}:{deadline: now+timeoutMs}), tools, namespaces, globals, maxOutputTokens: sourceOptions.maxOutputTokens ?? DEFAULT_MAX_OUTPUT_TOKENS, store: copyJson(store.values) }`；再读 `CodemodeJournalDoc`，返回 `{ start, journal, firstIssued, rerun }`——**`start`/`journal` 都经 `copyJson`**。
- `tools: DeclaredTool[]` 由 `agent.callable` 映射：`{ name, jsName: toCodemodeIdentifier(name), description: sampleOf(tool), ...(structuredOutputSchema ? { structured: true } : {}), ...(name.startsWith(CODEMODE_INTERNAL_PREFIX) ? { hidden: true } : {}) }`。
- `globals` 顺序：`DISCOVERY_GLOBALS`（`spread: true`）→ `models`（`options.models === false ? [] : [...CATALOG_GLOBALS, ...Object.keys(MODEL_CALLS)]`，均 `spread: true`）→ `options.globals`（`spread: options.spread === true`）。
- `namespaces` 从 callable 工具构造（隐藏工具被排除）：首个成员建条目，`tools.push(tool.name)`（**存原始名，不是 jsName**）。
- 启动后：`this.durableIssued = journal.reduce((max, e) => Math.max(max, e.issuedAfter ?? 0), firstIssued ?? 0)`（注释：用循环而非 spread，journal 可能比参数上限长）。
- `context.abortSignal`：注册 `onAbort = () => this.controller.abort(context.abortSignal?.reason)`（`{ once: true }`），已中止则立刻调用；`finally` 里移除。
- `sandbox.start(code, { startedAt, seed, tools: start.tools.filter(t => t.hidden !== true), globals, store, signal: controller.signal, ...(remaining===undefined?{}:{timeoutMs: remaining}), onStarted, onCall, onIdle })`；`onStarted: (fingerprint) => void this.checkFingerprint(fingerprint)`。
- **`checkFingerprint`**：`start.fingerprint` 已有且不等 → 置 `ended`、`await decide(...)`、`controller.abort(new Error("Sandbox fingerprint changed"))`、**不 openGate**；没有记录 → 一次提交写 `run.start.fingerprint = fingerprint`（失败 → `fail`）；随后 `openGate()`，然后 `if (this.replaying) this.deliverReplayed(); else if (!this.rerun) void this.release();`——**门开之前不投递、不接纳任何调用**。
- **`onCall(made)`**：`stopped` → 返回；`call = asToolCall(made)`；`call.target === "tool"` 才 `started(call)`；`issued = Math.max(issued, call.id)`；`index = replaying ? (replayIndex.get(call.id) ?? -1) : -1`；`index === -1` → 已 `released` 则 `waiting.push` + `launchCovered()`，否则 `held.push`；否则比对 journal 条目（global 结果分叉见 §7 表）。
- **`asToolCall(call)`**（**逐字，原 spec 未列**）：`call.target === "global"` 且 `MODEL_CALLS[call.name]` 有对应内部工具时，改写为 `{ id, target: "tool", name: tool, args: { model: model ?? null, context: context ?? null } }`，其中 `[model, context] = Array.isArray(call.args) ? call.args : []`；其他调用原样返回。**这就是"脚本调用 global 变成内部工具调用"的唯一落点**，`null` 而非 `undefined` 是有意的（`Type.Unknown()` 参数需要显式值）。
- **`deliverReplayed()`**：`while (nextReplayed < replayed.length && !stopped)`：`entry = replayed[nextReplayed]`；`outcome = ready.get(entry.id)`；`entry.id > issued || outcome === undefined` → return；否则 `ready.delete`、`nextReplayed++`、`run.deliver(nextReplayed, entry.at, entry.id, outcome)`（**第一个参数是 seq = 新长度**）。
- **`onIdle(seq, issued)`**：`stopped` → 返回；`rerun && seq <= replayed.length` 且记录值与 `issued` 不符 → `diverge`（两条文案见上表）；否则 `acks.set(seq, issued)`。非 `replaying` 时：`seq === 0 && rerun` → `void this.release()`；`seq === journalLength` → `void this.pump()`。`replaying` 时：`seq !== nextReplayed` → 返回；`next = replayed[seq]`；`next !== undefined` → `next.id > issued` 则 `diverge`，否则返回；没有下一条 → `replaying = false; void this.pump(); void this.release();`。
- **`release()`**（**逐字语义**）：`await gate`；`stopped || released` → 返回；`calls = held.splice(0)`；`rerun` 时**先逐个** `await this.earlier(call)`（抛错 → `fail`，`stopped` → 返回），有冲突 → `diverge` 并**一个都不启动**；然后 `released = true`；`waiting.push(...calls, ...held.splice(0))`；`launchCovered()`。
- **`earlier(call)`**：`api.snapshot(NestedCallDoc, api.taskId, `c${call.id}`, context)?.taskId`；无 → undefined；`api.getTask(taskId)` 的 `input` 不是 `{ kind: "nested" }` → undefined；`call.target === "tool" && made.name === call.name && sameJson(made.arguments, call.args ?? {})` → undefined（同一个调用，允许）；否则返回 `tools.${made.name}(${JSON.stringify(made.arguments)})`（用于 `but was ${other}` 文案）。
- **`launchCovered()`**：`!released || stopped` → 返回；`covered = waiting.filter(c => c.id <= durableIssued)`；空 → 返回；`waiting = waiting.filter(c => c.id > durableIssued)`；逐个 `launch`。
- **`launch(call)`**：`stopped` → 返回；tool → `callTool().then(outcome => ({ outcome }))`；global → `callGlobal().then(outcome => ({ outcome, global: { name, ...(args===undefined?{}:{args}), outcome } }))`；成功回调里 `stopped` → 丢弃；否则 `queue.push({ id, at: api.now(), ...done })` + `void pump()`；失败回调里 `stopped` → 丢弃；`context.abortSignal?.aborted` → `fail`；否则 `diverge`（`call ${id} is ${describe(call)}: ${errorMessage(error)}`）。
- **`callTool(call)`**：`args = (call.args ?? {}) as JsonObject`（**原样传给 `executeTool`，不校验**）；`model = call.name.startsWith(CODEMODE_INTERNAL_PREFIX)`；model 调用先 `await modelSlot()`，若等待期间 `stopped` → `releaseModelSlot()` + `throw new Error("The script has ended")`；`api.executeTool(call.name, args, context, { key: `c${call.id}` })`，`finally` 里 `releaseModelSlot()`；`call.name === GENERATE_IMAGES_TOOL` → `generatedImages += countImages(result.structuredOutput)`；`structured = start.tools.find(t => t.name === call.name)?.structured === true`；`outcome = outcomeOf(...)`；`finishRow(call.id, outcome, result)`。
- **`countImages(value)`**（**原 spec 未列**）：`typeof value === "object" && value !== null ? value.output : undefined`；`Array.isArray(output) ? output.filter(item => item?.type === "image").length : 0`——用于"生成了却没展示"的提示。
- **`started(call)`**：每个 id 只建一次行；`name` 去掉 `kiana.codemode.` 前缀（`call.name.slice(CODEMODE_INTERNAL_PREFIX.length)`）；`args = call.args === undefined ? "" : (JSON.stringify(call.args) ?? "")`；`cut(args, 200)`；`status: "running"`。
- **`finishRow(id, outcome, result)`**：`row.status = result.isError ? "error" : "ok"`；`durationMs` 有则记；`error = !outcome.ok ? outcome.message : errors.map(m => m.message).join("\n")`（`errors` = severity `"error"` 的 diagnostics）；**只有 `result.isError && error !== ""`** 才写 `row.error = cut(error, 500)`。
- **`details()`**：所有行按 id 升序；`status === "running"` 的改成 `"cancelled"`（浅拷贝，不改原行）——"脚本结束时仍在跑的调用被取消"。
- **`callGlobal(call)`**：`discovery`/`catalog`/自定义 global 三选一；都不是 → `{ ok: false, message: `Unknown global ${call.name}` }`；`discovery` 同步求值，`catalog` `await catalogCall(...)`，自定义 `await global.execute(args, api, context)`；**`undefined` 结果保持 `undefined`**（journal 省略该值，重放再得到 undefined）；`catch`：`context.abortSignal?.aborted` → **重抛**（不转成失败 outcome），否则 `{ ok: false, message: errorMessage(error) }`。
- **`modelSlot()`/`releaseModelSlot()`**：`modelCalls >= MAX_CONCURRENT_MODEL_CALLS(4)` → `await new Promise(resolve => modelWaiters.push(resolve))`；然后 `modelCalls++`；释放时 `modelCalls--` 并 `modelWaiters.shift()?.()`（**FIFO 唤醒**）。
- **`pump()`**（**原 spec 只有一句"批量"**）：`committing || stopped` → 返回；`queue.length === 0 && acks.size === 0` → 返回；`committing = true`；`batch = queue.splice(0)`；`acks = [...this.acks]`；`this.acks.clear()`；一次 `api.commit`：读 journal → 对每个 `[seq, issued]`：`seq === 0` → `journal.firstIssued ??= issued`，否则 `entry = journal.entries[seq-1]` 且 `entry !== undefined && entry.issuedAfter === undefined` 才写 `issuedAfter`；对 batch 每个条目 push `copyJson({ id, at, ...(global===undefined?{}:{global}) }, { omitUndefinedProperties: true })`。失败 → `fail` 返回。成功后：`for (const [, issued] of acks) durableIssued = Math.max(durableIssued, issued)`；对 batch：`journalLength++`，**`if (!this.run.finished)` 才 `run.deliver(journalLength, item.at, item.id, item.outcome)`**（**`run.finished` 是唯一阻止向已结束 VM 投递的闸门**——原 spec 未提）；`launchCovered()`；`committing = false`；`void this.pump()`（递归排空）。
- **`decide(decided)`**：`result = { ...decided, details: this.details() }`；一次提交里 `run.result ??= copyJson(result, { omitUndefinedProperties: true })`；返回 result。
- **`complete(result)`**：`formatResult(result, { generatedImages, wallMs: now - start.startedAt, maxOutputTokens: start.maxOutputTokens, calls: details.calls, limits: OUTPUT_LIMITS, write: this.outputFiles() })`；`tool = { output: formatted.content, isError: !result.ok, details: { ...details, ...(fullOutputPath?{fullOutputPath}:{}) } }`；`writes = result.ok ? result.storeWrites : undefined`；**在同一次提交里**：读 run，若 `run.result !== undefined` → **返回已有的那个**（不再覆盖）；否则写 `run.result = copyJson(tool, { omitUndefinedProperties: true })`；`writes` 且非空（`Object.keys(writes.set).length > 0 || writes.delete.length > 0`）→ 读 `CodemodeStoreDoc`，先 `for (key of writes.delete) delete store.values[key]`，再 `for ([key, value] of Object.entries(writes.set)) store.values[key] = value`。**注意 `storeWrites` 只在脚本成功时应用**。
- **`outputFiles()`**：返回闭包 `async (name, data) => ...`：`env === undefined` → `{ error: "no execution environment" }` **【逐字，小写】**；`directory ??= this.outputDir(env)`；`path = `${await directory}/${name}``（**字符串拼接，不是 `join`**）；`env.writeFile(path, data, context)`；ok → `{ path }`，否则 `{ error: written.error.message }`；任何抛错 → `{ error: errorMessage(error) }`。
- **`outputDir(env)`**：先一次提交读 `run.outputDir`；已记录 → 直接返回；否则 `env.createTempDir("kiana-codemode-", context)`（**前缀逐字**），失败 → `throw new Error(created.error.message)`；再一次提交 `run.outputDir ??= created.value`，返回 `run.outputDir!`。**目录在任何文件写出之前就已持久化**（重启后重写同一批文件）。
- **`stopped`**：`ended !== undefined || failure !== undefined || this.run?.finished === true`。
- 文件固定名由 `output.ts` 决定：溢出全量文本 → `output.txt`；图片 → `image-<index><ext>`（见 §2.28.4 与 §3.5）。

##### 输出/发现全局（`discover(call)`，逐字）
发现全局 `DISCOVERY_GLOBALS = ["searchTools","describeTool","describeNamespace"]` 与目录全局 `CATALOG_GLOBALS` 都由 start 数据回答（重放看到的是首次运行所见的目录）。未知名字 → `Unknown global ${call.name}`（**逐字**）。

```ts
const args = Array.isArray(call.args) ? call.args : [];
const { namespaces } = this.start;
const tools = this.start.tools.filter((tool) => tool.hidden !== true);   // 隐藏工具不出现在发现结果
const namespaceOfTool = (name) => namespaces.find((namespace) => namespace.tools.includes(name));
const matchesNamespace = (namespace, query) =>
	namespace.name === query || toCodemodeIdentifier(namespace.name) === toCodemodeIdentifier(query);
```

- `describeTool(name)`：非 string → `"describeTool() expects a tool name"`；返回 `tools.find(t => t.name === name || t.jsName === name)?.description`（**未找到返回 `undefined`，不报错**）。
- `describeNamespace(name)`：非 string → `"describeNamespace() expects a namespace name"`；未找到 → `undefined`；找到 →
  `{ name, ...(description===undefined?{}:{description}), ...(instructions===undefined?{}:{instructions}), tools: namespace.tools.map(toCodemodeIdentifier) }`。
- `searchTools(query, options?)`：
  - 非 string → `"searchTools() expects a query string"`；
  - `limit = options?.limit ?? DEFAULT_SEARCH_LIMIT(8)`；非整数或 `<= 0` → `"searchTools() limit must be a positive integer"`；
  - `namespace !== undefined && namespace !== null && typeof namespace !== "string"` → `"searchTools() namespace must be a string"`；
  - 候选文档 = `tools.flatMap(tool => { const own = namespaceOfTool(tool.name); if (typeof namespace === "string" && (own === undefined || !matchesNamespace(own, namespace))) return []; const text = [tool.name, tool.name.replaceAll("_", " "), tool.description, own?.name ?? "", own?.description ?? ""]; return [{ name: tool.name, text: text.join(" ") }]; })`
    （**注意**：与 `tool-search` 的 `searchText` 不同——这里没有 `schemaText(parameters)`，而多了 namespace 的 name/description）；
  - 返回 `rankDocuments(query, documents, limit).map(name => ({ name: tool.jsName, description: tool.description }))`——**按 jsName 返回**。
- **`ALL_TOOLS` 不在本文件里**：它是 `kiana-codemode` prelude 由 start 数据的 `tools` 数组构建的全局，形如 `Object.freeze([{ name: jsName, description }, ...])`（按 `jsName` 去重，首个胜出）。本文件只负责把 `DeclaredTool[]` 放进 start 数据；隐藏的 `kiana.codemode.*` 工具永不进入该数组，故永不出现于 `ALL_TOOLS`、`tools` 或 `searchTools()`。
- 同一 prelude 还提供 `tools` 的 Proxy 保护：读取不存在的成员抛 `TypeError`，文案见 `kiana-codemode` 单元规范（不在本单元范围）。

##### 工具注册
```ts
createCodemodeTool(options) → defineTool({
  name: "codemode",
  description: codemodeDescription(options.models !== false),
  parameters: codemodeSchema,          // Type.Object({ code: Type.String({ description: "Raw JavaScript source." }) })
  outputLimits: OUTPUT_LIMITS,         // { maxBytes: 4 * 1024 * 1024, maxLines: 100_000 }
  constrainedSampling: { type: "grammar", variants: { openai_lark: CODEMODE_SOURCE_GRAMMAR } },
  callers: ["model"],
  replay: "safe",
  execute: (args, api, context) => new CodemodeCall(options, api, context).execute(args.code),
})
```
字段顺序即源码顺序（`name`→`description`→`parameters`→`outputLimits`→`constrainedSampling`→`callers`→`replay`→`execute`）；`codemodeSchema` 的定义是
`const codemodeSchema = Type.Object({ code: Type.String({ description: "Raw JavaScript source." }) })`，
`CodemodeToolInput = Static<typeof codemodeSchema>`。

`createCodemodeExtension(options)` → **无 `tasks`**：
```ts
defineExtension({
  name: "codemode",
  tools: [createCodemodeTool(options), ...(options.models === false ? [] : createModelTools())],
  sections: [
    section("codemode", (input) =>
      codemodeSection(
        // 延迟工具由 searchTools() 查找，coding agent 同样不列出
        input.agent.callable.filter(
          (tool) => !tool.name.startsWith(CODEMODE_INTERNAL_PREFIX) && tool.deferred !== true,
        ),
        options.namespaces ?? [],
        options.inlineBudget ?? DEFAULT_CODEMODE_INLINE_BUDGET,
      ),
    ),
  ],
})
```

**注意**：`codemode.ts` 依赖外部包 `@earendil-works/kiana-codemode`（`QuickJsSandbox`、`CodemodeOutputItem`、`CodemodeResult`、`renderToolSample`、`toCodemodeIdentifier`、`CodemodeJsonSchema`）。这些由另一个单元移植；本规范只约束本文件的编排逻辑。

**Rust 注意事项**
- QuickJS 沙箱：`rquickjs`。必须保证：确定性（随机数/时钟注入）、`await`/顶层 `return`、脚本结束取消未完成 Promise、字节码/指纹一致性。
- 脚本内 `JSON` 语义、`Error` 的 `stack`/`name`/`message` 必须与 V8 输出一致（`formatError` 直接使用 `error.stack`）。若 Rust 侧 QuickJS 的 stack 格式不同，会破坏输出一致性——这是最高风险点之一（见 §8）。
- `constrainedSampling` 的 lark 语法由 kiana-ai 处理，本单元只传字符串。

---

## 3. 用户可见表面（逐字）

> 本单元**不含** CLI 参数、slash 命令、键绑定、TUI 布局（属于 Harness/CLI 单元）。本单元的用户可见表面是：
> **(a)** 模型可见的工具名/描述/参数 schema/输出文本；**(b)** 诊断与错误消息；**(c)** 落盘文件；**(d)** 环境变量与进程行为。

### 3.1 工具注册面（名称、描述、schema）

| 工具名 | 来源 | description（逐字见 §2） | 参数 | 备注 |
|---|---|---|---|---|
| `read` | `tools/read.ts` | "Read the contents of a file. Supports text files and images (jpg, png, gif, webp[ , bmp]). Images are sent as attachments. For text files, output is truncated to 2000 lines or 50KB (whichever is hit first). Use offset/limit for large files. When you need the full file, continue with offset until complete." | `path: string`（"Path to the file to read (relative or absolute)"）、`offset?: number`（"Line number to start reading from (1-indexed)"）、`limit?: number`（"Maximum number of lines to read"） | `ReadToolDetails.truncation` |
| `write` | `tools/write.ts` | "Write content to a file. Creates the file if it doesn't exist, overwrites if it does. Automatically creates parent directories." | `path`（"Path to the file to write (relative or absolute)"）、`content`（"Content to write to the file"） | |
| `edit` | `tools/edit.ts` | "Edit a single file using exact text replacement. Every edits[].oldText must match a unique, non-overlapping region of the original file. If two changes affect the same block or nearby lines, merge them into one edit instead of emitting overlapping edits. Do not include large unchanged regions just to connect distant changes." | `path`、`edits: Array<{oldText,newText}>`（描述见 §2.25.9） | `prepareArguments: prepareEditArguments`；`EditToolDetails {diff, patch, firstChangedLine?}` |
| `bash` | `tools/bash.ts` | "Execute a bash command in the current working directory. Returns combined stdout and stderr. Output is truncated to last 2000 lines or 50KB (whichever is hit first). If truncated, full output is saved to a temp file. Optionally provide a timeout in seconds." | `command`（"Bash command to execute"）、`timeout?`（"Timeout in seconds (optional, no default timeout)"） | `structuredOutputSchema`；`outputLimits: { retain: "tail" }` |
| `powershell` | `tools/bash.ts` | 同 bash，首句 "Execute a PowerShell command in the current working directory." | `command`（"PowerShell command to execute"）、`timeout?` | 同上 |
| `tool_search` | `tool-search/tool-search.ts` | 固定文本（§2.27） | `query`（"Search query for deferred tools."）、`limit?`（"Maximum number of tools to return. Defaults to 8."） | `callers: ["model"]`、`replay: "safe"` |
| `codemode` | `codemode/codemode.ts` | `codemodeDescription(models)`（§2.28.2） | `code: string` | `callers: ["model"]`、`constrainedSampling`、`outputLimits: {4 MiB, 100000}` |
| `kiana.codemode.models.classify` | `codemode/models.ts` | "models.classify() of code mode scripts." | `{ model: unknown, context: unknown }` | `callers: ["tools"]`、`structuredOutputSchema: unknown`、**不出现在任何列表** |
| `kiana.codemode.models.generateImages` | `codemode/models.ts` | "models.generateImages() of code mode scripts." | 同上 | 同上 |

### 3.2 扩展与系统提示段

| 扩展名 | 内容 |
|---|---|
| `coding-tools` | tools: `read`、`write`、`edit`、`bash`（`createCodingTools(options)` 中 `read` 用传入的 `images`） |
| `tool-search` | tools: `tool_search` |
| `codemode` | tools: `codemode`（`options.models !== false` 时再加 `kiana.codemode.models.classify`、`kiana.codemode.models.generateImages`）；section key `"codemode"`；无 tasks |

系统提示段（`codemode`）渲染格式 **【逐字】**：
```
Tools scripts can call (`await tools.<name>(args)`):

## <namespace>[ (tools not listed) | (some tools not listed)]
<namespace.description（trimmed）>

### `<identifier>`[ (`raw name`)]
<tool 声明（renderToolSample，trim 后）>

...
```
无命名空间且有省略时，插入一行：
```
Some tools are not listed: find them with `searchTools()`.
```
段间以 `\n\n` 连接。工具数为 0 → 段返回 `undefined`（不渲染）。

### 3.3 模型可见的诊断（`ToolDiagnostic`）

| 工具 | severity | code | message |
|---|---|---|---|
| read | `info` | `image` | `Read image file [<mime>].`（其后按需追加：`Converted from X to Y.`、`Resized from WxH to WxH. Multiply coordinates by S to map them to the original.`、`The current model does not support images; it sees a placeholder instead.`，以空格连接） |
| read | `info` | `truncated` | `Showing lines A-B of N. Use offset=M to continue.` 或 `Showing lines A-B of N (50.0KB limit). Use offset=M to continue.` |
| read | `warn` | `truncated` | `Line A is XB, exceeds the 50.0KB limit; showing its first YB. Use bash: sed -n 'Ap' <path> \| tail -c +<Y+1>` |
| read | `info` | *(无 code)* | `N more lines in file. Use offset=M to continue.` |
| read | `error` | `unsupported_image` | 见 §2.25.7 的 5 条 |
| bash/powershell | `info` | `full_output` | `Full output: <spillPath>` |
| bash/powershell | `error` | `exit_code` | `Command exited with code N` |

### 3.4 工具输出文本（`output` 通道，逐字）

| 工具 | 文本 |
|---|---|
| write | `Successfully wrote to <path>` |
| edit | `Successfully replaced N block(s) in <path>.` |
| read | 文件文本（截断后）；空 → `[]` |
| tool_search | `No matching tools found.` 或 `Loaded N tool(s). They are available from your next call:\n- name: firstLineOfDescription`（`tool`/`tools` 单复数） |
| bash/powershell | 保留的输出（`output` 省略时由 Harness 取 `retainedOutput()`） |
| codemode | `Script completed\nWall time X.X seconds\nOutput:\n` 或 `Script failed\n...`，后接内容项 |

### 3.5 渲染细节（codemode 输出与 diff）

- 多项文本：每项前缀 `==> text N/M <==\n`（`N` 从 1，`M` 为文本项总数）。
- console：单独一块 `<console_output>\n...\n</console_output>`，位于其他输出之后。
- 截断：`Warning: truncated output (original token count: N)\nTotal output lines: M\n\n<head>…K tokens truncated…<tail>`。
- 全量文件：`\n\n[Full output: <path> (read with offset/limit)]` 或 `\n\n[Could not save the full output: <error>]`。
- 图片：`[Image saved to <path> (<mime>, <bytes> bytes)]` 或 `[Image (<mime>, <bytes> bytes) could not be saved: <error>]`；文件名 `image-<index><ext>`，扩展名 `.png/.jpg/.gif/.webp`，未知 → `.bin`。
- 生成图提示：`Note: models.generateImages() returned N image(s) that the script did not show. Show each image block of result.output with image(block).`
- 脚本错误头：`Script error:\n<stack | "Name: message" | "Script timed out: ..." | "Script aborted: ..." | "Script sandbox failed: ...">\n\n<No tool calls were made. | "Tool calls made before the failure (they are not undone): name (status), ...">`
- diff 行格式：`+NNN line`、`-NNN line`、` NNN line`、` <padding> ...`（行号宽度 = `String(maxLineNum).length`）。
- unified patch：`--- <path>\n+++ <path>\n@@ -a,b +c,d @@\n`（context 4 行，无 `Index:` 行）。

### 3.6 错误消息总表（供测试逐字比对）

见 §2 各文件中标注 **【逐字】** 的字符串。附加汇总（按模块）：

**Session/Transaction/Storage 核心**
```
Tx.<method>() cannot read tables after the first table write
Session failed after a storage error; close and reopen it
Read-only Harness: <what> refused
Conversation <id> is busy
Session is closed
Transaction has settled
Task <id> does not exist
Task <id> is terminal
Task <id> is already terminal
Task <id> cannot change conversations
Task <id> already has a terminal candidate
Conversation <id> does not exist
Conversation owner task <id> does not exist
Submission <id> does not exist
Submission <id> is not queued
Submission <id> is not a placed input
Request <requestId> already identifies a submission of type <type>
Fork source document <id> cannot be read          (Transaction #acquireForkCopy)
Fork source document <id> does not match the copied record
Current document <id> (<kind>) cannot be read
Historical document <id> (<kind>) cannot be read
Text line reader is closed / Binary reader is closed / Directory reader is closed
Session commit callback settled before its pending Tx operations
Storage committed without adopting the batch
Storage.entry() requires an entry ID
Unknown document: <id>
Document <id> already exists / is retired / delta has no base / version transition requires a base /
  is missing a required base / crosses a stored version boundary without a base /
  does not retain historical content / has more than one content command / is retired more than once
Document address already has a current incarnation
Unknown conversation: <id>
Commit sequence <seq> does not strictly increase
ID <id> already belongs to <table>
ID <id> is written more than once
ID <id> is written as two record types
ID space is exhausted
MemoryStorage is closed / SqliteStorage is closed / JsonlStorage is closed / IndexedDbStorage is closed
Prepared document copy was not resolved
Document copy <id> was rejected / source is changed in the copy batch
Fork source document <id> is changed in the copy batch / cannot be read / does not match the copied record
Fork selects multiple source documents for <member>
Entry <at> is not visible from conversation <id>
Current document <id> (<kind>) cannot be read
Historical document <id> (<kind>) cannot be read
Session.snapshotAsOf() requires a conversation document / requires an entry ID
CommitPersistence.withinMs must be a number of milliseconds, 0 or more
Durable SQLite metadata is missing
Durable IndexedDB metadata is missing
Invalid storage cursor
Invalid scan order: <value>
The cursor continues a <order> scan; the query asks for <requested>
```
**JSONL 专有**
```
JSONL <action> failed: <message>
Malformed complete <description>
Invalid write in <description> / Invalid <type> write in <description>
Invalid terminal task write in <description>
Invalid document retirement in <description>
Invalid task sidecar write in <description>
Invalid document creation in <description> / Invalid document change in <description>
Unknown write type in <description>
Invalid commit marker in <description>
Invalid document content in <description>
Invalid sidecar record in <description> / Invalid live task record in <description>
Invalid document record in <description> / Unknown sidecar record type in <description>
Commit sequence does not strictly increase in main.jsonl
Sidecar records are out of order in <file>
Invalid committed state at sequence <seq>
Confirmed task sidecar data does not match commit <seq>
Confirmed document sidecar data does not match commit <seq>
Document creation lacks a confirmed base in commit <seq>
Confirmed record follows an unconfirmed tail in <file>
Sidecar record is confirmed more than once
Missing confirmed sidecar record <file> at sequence <seq>
Invalid UTF-8 in complete <name> line <n>
JSONL storage is poisoned and must be reopened
```
**索引 DB 专有**
```
IndexedDB is not available / request failed / transaction failed / transaction was aborted /
cursor failed / key ranges are not available
```
**env / tools 专有**
```
No execution environment is configured
Invalid line range
Invalid timeout: must be a finite number of seconds
Invalid timeout: maximum is 2147483.647 seconds
Working directory does not exist: <cwd>\nCannot execute bash commands.
Empty argv: no program to run
No command to run
Operation aborted
Offset <n> is beyond end of file (<total> lines total)
<path> changed while it was read
File size must be a non-negative safe integer
Is a directory
Offset and length must be non-negative safe integers
Not a regular file
Refusing to follow a symbolic link
maxEntries must be a positive safe integer
Unsupported file type
The environment does not implement listFiles
Watched paths exceed <max> directories
No bash shell found. Options:\n  1. Install Git for Windows: https://git-scm.com/download/win\n  2. Add your bash to PATH (Cygwin, MSYS2, etc.)\n  3. Configure an explicit shellPath\n\nSearched Git Bash in:\n<candidates>
HarnessOptions.setTimer declined a timer that paces work; using setTimeout
```
**工具/codemode 专有**
```
Edit tool input is invalid. edits must contain at least one replacement.
Could not edit file: <path>. Error code: <code>.
Could not edit file: <path>. Path is not a file.
Cannot preserve unchanged lines because the base content has a different line count.
Replacement range is outside the base content.
Could not find the exact text in <path>. The old text must match exactly including all whitespace and newlines.
Could not find edits[<i>] in <path>. The oldText must match exactly including all whitespace and newlines.
Found <n> occurrences of the text in <path>. The text must be unique. Please provide more context to make it unique.
Found <n> occurrences of edits[<i>] in <path>. Each oldText must be unique. Please provide more context to make it unique.
oldText must not be empty in <path>. / edits[<i>].oldText must not be empty in <path>.
No changes made to <path>. The replacement produced identical content. This might indicate an issue with special characters or the text not existing as expected.
No changes made to <path>. The replacements produced identical content.
edits[<i>] and edits[<j>] overlap in <path>. Merge them into one edit or target disjoint regions.
query must not be empty
limit must be a positive integer
Unknown model type <json>. Use "chat", "image", or "classifier".
provider must be a string
models.classify() <problem>. Expected context: <shape>.
models.generateImages() <problem>. Expected context: { input: [...] }.
Unknown classifier model "<provider>/<id>". List the classifier models you can use with models.getAvailableOfType("classifier").
"<provider>/<id>" is an image model, not a classifier model. List the ...
searchTools() expects a query string / limit must be a positive integer / namespace must be a string
describeTool() expects a tool name
describeNamespace() expects a namespace name
The script has ended
Task <id> invocation has ended
Invalid timeout: ...
```

### 3.7 进程/环境变量

| 变量/条件 | 作用 |
|---|---|
| `process.cwd()` | `openNodeJsonlStorage` 每次调用新建 `NodeExecutionEnv({ cwd })` 的基准 |
| `SystemRoot` | Windows 下定位 `System32\bash.exe`、`taskkill` |
| `ProgramFiles`、`ProgramFiles(x86)` | Windows 下定位 Git Bash |
| `PATH` | 查找 `bash`（`which`/`where`）、`pwsh`/`powershell` |
| `HOME`（`os.homedir()`） | `~` 展开 |
| `navigator.userAgent === "Cloudflare-Workers"` | `nativeTimer` 拒绝后台计时器 |
| `process.env` | `inheritEnv: true` 时的基础环境 |
| `NO_COLOR`/终端 | **不适用**（本单元不渲染彩色输出） |

本单元**不**定义任何自己的环境变量。

### 3.8 退出码与信号

- `Shell.exec` 的 `exitCode`：正常为进程退出码；被信号杀死时为 `128 + signalNumber`。
- 本单元不决定进程退出码（由 CLI 单元决定）。

---

## 4. 外部格式（逐字）

### 4.1 JSONL 存储目录

目录内容（`JsonlStorage.open(directory)` 创建，`recursive: true`）：

```
<directory>/
  main.jsonl                    提交标记（append-only，不压缩）
  doc-<DocumentId>.jsonl        一个文档化身（incarnation）
  task-<TaskId>.jsonl           活动（非终态）任务替换
  doc-<id>.jsonl.reclaim        回收临时文件（崩溃遗留；open 时删除）
```

#### 4.1.1 `main.jsonl` 行格式

每行一个完整 JSON 对象 + `\n`：
```ts
{ format: 1, type: "commit", seq: <int ≥ 1>, writes: MainOperation[] }
```
`MainOperation` 的 **8 种**形态（**键顺序即源码对象字面量顺序**；源码类型为
`Extract<StorageWrite, { type: "conversation" | "entry" | "submission" | "document.retire" }> | { type:"task"; value: StoredTask } | { type:"task.sidecar"; id: TaskId; ordinal: number } | { type:"document.create"; record: DocumentCreate; ordinal: number } | { type:"document.change"; id: DocumentId; ordinal: number }`）：
```json
{"type":"conversation","value":{...}}
{"type":"entry","value":{...}}
{"type":"submission","value":{...}}
{"type":"document.retire","id":<int>}
{"type":"task","value":{...}}                        // 仅终态任务
{"type":"task.sidecar","id":<int>,"ordinal":<int ≥ 0>}
{"type":"document.create","record":{...},"ordinal":<int ≥ 0>}
{"type":"document.change","id":<int>,"ordinal":<int ≥ 0>}
```
（原 spec 写作"7 种"，实际 **8 种**；`document.copy` **不**出现在主日志里——fork 复制在 `Transaction`/Storage 层被展开为 `document.create` + `source` 的内部动作，落盘时已变成 `document.create` 写。）

外层键顺序：`format`、`type`、`seq`、`writes`；`task.sidecar` 为 `type`、`id`、`ordinal`；`document.create` 为 `type`、`record`、`ordinal`；`document.change` 为 `type`、`id`、`ordinal`。

**写入顺序规则**（`commit` 时按 `writes` 数组顺序编码，`ordinal` 在同一提交内**跨文件**从 0 递增）：`conversation`/`entry`/`submission`/`document.retire`/终态 `task` 直接进 marker；非终态 `task` → `task.sidecar`（写 `task-<id>.jsonl`，marker 记 `task.sidecar`）；`document.create`/`document.change` → `document.create`/`document.change`（写 `doc-<id>.jsonl`，marker 记同形）。

#### 4.1.2 sidecar 行格式

```json
{"format":1,"type":"record","seq":<int ≥ 1>,"ordinal":<int ≥ 0>,"payload":{...}}
```
payload 两种：
```json
{"type":"task","value":{...}}                        // 非终态
{"type":"document","id":<int>,"content":{...}}
```
`content` 两种：
```json
{"kind":"base","version":<int ≥ 1>,"value":{...}}
{"kind":"delta","version":<int ≥ 1>,"ops":[...]}
```
键顺序：外层 `format`、`type`、`seq`、`ordinal`、`payload`；document payload 为 `type`、`id`、`content`；base 为 `kind`、`version`、`value`；delta 为 `kind`、`version`、`ops`。

#### 4.1.3 不变量
- 一个 sidecar 文件内的记录须严格递增：`seq` 升序，同 `seq` 时 `ordinal` 升序，否则 `Sidecar records are out of order in <file>`。
- `ordinal` 在一次提交内**跨文件全局递增**（从 0 开始）。
- `main.jsonl` 中 marker 的 `seq` 严格递增（起始 ≥ 1）。
- 只有 `main.jsonl` 中**已确认**的 sidecar 记录才算数；未确认尾部在 open 时被截断。
- `.reclaim` 文件在 open 时直接删除。
- 主日志中的 `task` 写**只能**是 `state.status === "terminal"`。

#### 4.1.4 恢复（open）算法
见 §2.14 `recover()` 完整 16 步；要点：读主日志 → 校验 seq → 删 `.reclaim` → 按名排序列出 sidecar → 解析并校验顺序 → 计算 currentOnly/retired/finalTaskIsLive/latestBases → 逐 marker 确认记录并重放进 MemoryStorage → 截断未确认尾部 → 计算回收并执行。

### 4.2 SQLite 存储

单文件数据库；WAL 模式 + `synchronous = NORMAL`（README 表格：提交能挺过进程崩溃；最新提交可能在断电/宿主故障时丢失）。
Schema 见 §2.15.2（逐字 SQL）。`durable_schema` 表记录迁移版本；`durable_metadata` 单行 `{ singleton=1, next_id TEXT, next_seq INTEGER }`，初值 `('2', 1)`。
`prefix` 应用于全部表名与索引名：`<prefix><name>`。

### 4.3 IndexedDB

数据库名由 `IndexedDbStorage.open(name)` 给定；`DATABASE_VERSION = 1`；对象仓库/索引名与 SQLite 表/索引同名；元数据键 `"state"`。

### 4.4 文档地址与身份

- `addressId(address) = JSON.stringify([kind, scope.kind, owner, key ?? null])`（§2.5）。
- Memory 的 `addressKey(address) = JSON.stringify([kind, scopeKey(scope), key === undefined ? ["singleton"] : ["family", key]])`，`scopeKey` 本身是 `JSON.stringify([...])`（§2.12）。
- SQLite/IndexedDB 的 `addressKey = JSON.stringify([JSON.stringify(kind), scopeKind, ownerId, family, JSON.stringify(key ?? "")])`，session ownerId 为 `0`（§2.15.3）。

三种后端的地址键**字符串不同**，但语义等价；**不得**跨后端比较。

### 4.5 文档 revision 语义（所有后端一致）
- 每个化身（incarnation）在 `document_revisions`/`revisions` 中存一串 `{ seq, kind, version, content }`。
- `base` 存完整值；`delta` 存 `Op[]`。
- 读取某点 `at`：取 `seq <= at` 的最后一条 `base` 与其后的 `delta`，逐个要求 `delta.version === base.version`，否则 `crosses a stored version boundary without a base`。
- `deltasSinceBase` = 该 base 之后的 revision 数。
- current-only 文档在写入新 `base` 时**丢弃全部旧 revision**；退休时也丢弃（回收历史）。
- rewindable 文档保留全部历史。

### 4.6 任务状态机（存储层视角）

`TaskState.status`：`pending` | `running` | `waiting` | `sleeping` | `completing` | `terminal`。
- SQLite `tasks.status` CHECK（v2 起）包含全部 6 个；v1 缺 `sleeping`。
- JSONL：非终态进 sidecar，终态进主日志并回收 sidecar。
- Memory：`taskIdsByStatus` 六个数组。

### 4.7 提交记录（Submission）状态

`queued` | `placed` | `done` | `unanswered`。
- `applySubmissionChange` 规则见 §2.17。
- SQLite `request_id` 以 JSON 字符串形式索引（`encodeIndexedString`）。
- JSONL 直接存 `SubmissionRecord`。

### 4.8 进程/网络协议

本单元**没有** HTTP 端点、SSE 解析或 RPC 消息（这些属于其他单元）。唯一的"线协议"是：
- **Storage 契约**（§2.4.3）—— 进程内接口；
- **`ExecutionEnv` 契约**（§2.20）—— 进程内接口，但可被远程实现（例如远程守护进程）；
- **JSONL 文件格式**（§4.1）—— 跨进程/跨重启的持久格式。

`ShellOutputWindow`/`ShellOutputSkip` 定义了远程环境可用来省流量的语义（§2.20），若实现远程 env，需要按此编码。

---

## 5. 跨单元接口

### 5.1 本单元消费（来自 chord）

| 符号 | 用途 |
|---|---|
| `JsonValue`、`JsonObject` | 全部值类型 |
| `copyJson(value, options?)` | 表记录深拷贝；`{ omitUndefinedProperties: true }` |
| `Op`、`applyImmutableBatches(base, batches)` | 文档增量 |
| `PendingOps`、`track(value)`/`Tracker<T>`、`Change<T>`、`Prepared<T>`、`Draft<T>` | 文档草稿与追踪 |
| `Watch<T>`、`WatchHandle` | watchDoc |
| `Context`（`abortSignal`、`telemetry`） | 取消与遥测 |
| `@earendil-works/chord/output`：`utf8ByteLength`、`characterEnd`、`OutputBuffer` | 截断与输出缓冲 |
| `@earendil-works/chord/sqlite`：`SqliteDatabase`、`SqliteExecutor`、`SqliteValue`、`SqliteMigration`、`applySqliteMigrations`、`validateSqliteSchemaPrefix` | SQLite 后端 |
| `@earendil-works/chord/sqlite/node`：`openNodeSqliteDatabase`、`NodeSqliteDatabase` | Node 适配 |
| `@earendil-works/chord/sqlite/cloudflare`：`DurableObjectSqliteDatabase` | Workers 适配 |

**Rust**：这些由 `chord` 单元的 Rust 版本提供（假定 crate `kiana-chord`）。若尚未就绪，本单元需要先定义 trait：
```rust
pub trait JsonValueExt { }         // 或直接使用 serde_json::Value
pub type Op = serde_json::Value;   // chord 的 Op 是 ["r", value] | ["a", path, value] | ... 的具体枚举
pub trait DocumentTracker { fn value(&self) -> &Map<String, Value>; fn change(&mut self) -> ...; }
```

### 5.2 本单元消费（来自 kiana-ai）

| 符号 | 用途 |
|---|---|
| `Message`、`UserMessage`、`AssistantMessage`、`SystemMessage`、`ToolResultMessage` | EntryRecord.model |
| `TextContent`、`ImageContent`、`ToolResultMessage["content"]` | 工具输出 |
| `Models`、`AnyModel`、`ModelType`、`ModelTypeMap`、`ClassifierContext`、`ImagesContext`、`Usage` | codemode models |
| `Tool`、`ToolRegistration` 的基础字段 | 工具注册 |
| `Type`/`Static`（TypeBox） | 参数与结构化输出 schema |

**Rust**：`kiana-ai` crate 提供 `Message` 等（serde 枚举）。TypeBox schema → Rust 侧需要 `serde_json::Value` 形式的 JSON Schema（因为 `structuredOutputSchema` 会被 kiana-ai 的验证器使用）。建议：
- 类型层：用 Rust struct + `serde`；
- schema 层：用 `schemars` 生成 JSON Schema，或以 `serde_json::json!` 手写字面量（与 TypeBox 输出对比时注意 `additionalProperties` 等差异）。

### 5.3 本单元消费（来自 kiana-codemode）

| 符号 | 用途 |
|---|---|
| `CodemodeJsonSchema` | 声明 schema |
| `renderToolSample(declaration)`、`toCodemodeIdentifier(name)` | describe/render |
| `CodemodeOutputItem`、`CodemodeResult` | output |
| `CodemodeSandbox`（`start`/`deliver`/`onCall`/`onIdle`/`acks`/`release`/`fingerprint` …） | codemode 执行 |
| `portable` 子路径 | 可移植类型 |

**Rust**：`kiana-codemode` crate（QuickJS via rquickjs）。

### 5.4 本单元消费（来自 Harness 单元，仅类型/恒等函数）

**`harness/define.ts`（44 行，全部是恒等包装，逐字）**：
```ts
export function defineExtension<Tool extends ToolRegistration = ToolRegistration>(extension: Extension<Tool>): Extension<Tool>;   // 原样返回
export function defineTool<TParameters extends TSchema, TDetails extends JsonValue = JsonValue>(
	tool: ToolRegistration<TParameters, TDetails>): ToolRegistration<TParameters, TDetails>;                                      // 原样返回
export function section<Tool extends ToolRegistration = ToolRegistration>(
	key: string, render: PromptSection<Tool>["render"], options?: { readonly tag?: boolean }): PromptSection<Tool>;
	// options?.tag === undefined ? { key, render } : { key, render, tag: options.tag }
export function hook<K extends AnyTask>(task: K, handlers: Partial<HooksOf<K>>): HookRegistration;
	// → { task: task.definition.name, handlers }
export function wrapTool<Tool extends ToolRegistration>(tool: Tool, wrapper: (tool: Tool) => Tool): Wrap<Tool>;
	// → { tool: tool.name, wrap: wrapper }
export function wrapSection<Tool extends ToolRegistration = ToolRegistration>(
	key: string, wrapper: (section: PromptSection<Tool>) => PromptSection<Tool>): Wrap<Tool>;
	// → { section: key, wrap: wrapper }
```
**Rust 对应**：这 6 个函数在 Rust 中是**恒等函数或结构体构造**，但 `section`/`hook`/`wrapTool`/`wrapSection` 的"取名字"行为必须保留（`hook` 读 `task.definition.name`、`wrapTool` 读 `tool.name`）。

**`harness/types.ts` 中本单元用到的类型（逐字，节选）**：
```ts
export type ToolControl = { readonly addTools?: readonly string[]; readonly terminate?: boolean; readonly handoff?: string };
export type ToolDiagnostic = { readonly severity: "info"|"warn"|"error"; readonly message: string; readonly code?: string };
export type ToolExecutionResult<TDetails extends JsonValue = JsonValue> = {
	readonly output?: ToolResultMessage["content"];
	readonly structuredOutput?: JsonValue;
	readonly isError?: boolean;
	readonly details?: TDetails;
	readonly diagnostics?: readonly ToolDiagnostic[];
	readonly usage?: Usage;
	readonly control?: ToolControl;
};
export type NestedToolExecutionResult = {
	readonly taskId: TaskId;
	readonly structuredOutput?: JsonValue;
	readonly isError: boolean;
	readonly details?: JsonValue;
	readonly diagnostics: readonly ToolDiagnostic[];
	readonly usage?: Usage;
	readonly durationMs?: number;
};
export interface ToolExecutionApi<TDetails extends JsonValue = JsonValue> extends DocumentObserver, DocumentReader {
	readonly taskId: TaskId;
	readonly conversationId: ConversationId;
	readonly callId: string;
	readonly registry: RegistrySnapshot;
	agent(context: Context): Promise<Agent>;
	readonly models: Models;
	readonly env: ExecutionEnv | undefined;
	now(): number;
	output(chunk: string | Uint8Array, skipped?: ShellOutputSkip): void;
	readonly outputWindow: ShellOutputWindow | undefined;
	retainedOutput(): { readonly text: string; readonly truncated: boolean };
	diagnostic(diagnostic: ToolDiagnostic): void;
	details(value: TDetails, context: Context): Promise<void>;
	commit<T>(change: (tx: Tx) => T | Promise<T>, context: Context, options?: CommitOptions): Promise<T>;
	memo<T extends JsonValue>(name: string, context: Context): Promise<T | undefined>;
	memo<T extends JsonValue>(name: string, candidate: T, context: Context): Promise<T>;
	createTask<I, S extends { phase: string }, R, H extends object>(task: Task<I, S, R, H>, input: I,
		options: Omit<TaskOptions, "conversationId">, context: Context): Promise<TaskId<R>>;
	getTask<R>(id: TaskId<R>, context: Context): Promise<TaskRecord<JsonValue, JsonValue, R> | undefined>;
	waitForTask<R>(id: TaskId<R>, context: Context): Promise<SettledTask<R>>;
	conversation(id: ConversationId, context: Context): Promise<ConversationHandle | undefined>;
	executeTool(name: string, args: JsonObject, context: Context,
		options?: { readonly key?: string; readonly progress?: boolean }): Promise<NestedToolExecutionResult>;
}
export type ToolRegistration<TParameters extends TSchema = TSchema, TDetails extends JsonValue = JsonValue> = Tool<TParameters> & {
	readonly replay?: "safe" | "unsafe";
	readonly executionMode?: ToolExecutionMode;
	prepareArguments?(args: unknown): Static<TParameters>;
	readonly structuredOutputSchema?: TSchema;
	readonly callers?: readonly ("model"|"tools")[];
	readonly deferred?: true;
	readonly outputLimits?: { readonly maxBytes?: number; readonly maxLines?: number; readonly retain?: "head"|"tail" };
	execute(args, api: ToolExecutionApi<TDetails>, context: Context): Promise<ToolExecutionResult<TDetails>>;
};
export type PromptInput<Tool extends ToolRegistration = ToolRegistration> = {
	readonly conversationId: ConversationId;
	/** The request's resolution; `agent.tools` are the tools offered in this request. */
	readonly agent: Agent<Tool>;
	readonly env: ExecutionEnv | undefined;
	/** Sections already in effect after replaying the active transcript. */
	readonly shown: Readonly<Record<string, string>>;
	readonly read: DocumentReader;
};
export type PromptSection<Tool extends ToolRegistration = ToolRegistration> = {
	readonly key: string;
	render(input: PromptInput<Tool>, context: Context): string | undefined | Promise<string | undefined>;
	/** Default true: wrap the text as `<key>\n...\n</key>`. */
	readonly tag?: boolean;
};
/** Built by `hook()`; matches tasks by name. */
export type HookRegistration = { readonly task: string; readonly handlers: object };
/** Built by `wrapTool()` and `wrapSection()`; targets a tool name or a section key. Wrappers are pure. */
export type Wrap<Tool extends ToolRegistration = ToolRegistration> =
	| { readonly tool: string; wrap(tool: Tool): Tool }
	| { readonly section: string; wrap(section: PromptSection<Tool>): PromptSection<Tool> };
export interface Extension<Tool extends ToolRegistration = ToolRegistration> {
	readonly name: string;
	readonly tools?: readonly Tool[];
	readonly sections?: readonly PromptSection<Tool>[];
	readonly hooks?: readonly HookRegistration[];
	/** Apply where this extension is selected, in order. */
	readonly wraps?: readonly Wrap<Tool>[];
	/** Resolved by name for every task, whichever conversations select this extension. */
	readonly tasks?: readonly AnyTask[];
}
export interface RegistrySnapshot<Tool extends ToolRegistration = ToolRegistration> {
	installed(): readonly Extension<Tool>[];
	extension(name: string): Extension<Tool> | undefined;
	/** Every installed tool with its extension, in install order. Names may repeat across extensions. */
	tools(): readonly { readonly extension: Extension<Tool>; readonly tool: Tool }[];
	sections(): readonly { readonly extension: Extension<Tool>; readonly section: PromptSection<Tool> }[];
	/** Built-in and installed task definitions. */
	tasks(): readonly AnyTask[];
	task(name: string): AnyTask | undefined;
}
```
**`executeTool` 的 key 规则（逐字，影响 codemode）**：
> The nested call's key names it within this call; its call ID is `<callId>/<key>`. By default the key is the call's position among this invocation's nested calls, `1`, `2`, ... An explicit `key` must be non-empty, without `/`, not `__proto__`, and not a positive integer, so it never collides with a default one.

### 5.5 本单元向其他单元暴露（Harness 依赖）

| 导出 | 说明 |
|---|---|
| `createSession(storage, options?)` / `SessionImpl` | Harness 的 `Session` 实现；Harness 通过 `Session` 接口 + 内部 getter（`failure`/`failed`）+ `commitWith`/`readOnLine`/`observeCommits`/`unloadDocuments`/`documentCacheSizes`/`fail` 使用 |
| `Storage` 接口与 4 个后端 | Harness 打开并传给 Session |
| `MemoryStorage.prepareCommit` | JSONL 复用 |
| `Transaction` 及其 `commitWith(scope)` | Harness 的调度器提交 |
| `defineDoc`/`defineDocFamily`/`defineEntry`/`defineTask` | 扩展与内置任务定义文档/条目/任务 |
| `ExecutionEnv`/`NodeExecutionEnv` | 工具与 Harness 环境 |
| `createCodingTools`/`createToolSearchTool`/`createCodemodeTool` | 工具 |
| `NodeFileWatcher` | `HarnessOptions` 的 watch |
| `truncateHead`/`formatSize`/`DEFAULT_MAX_*` | Harness 工具结果限制 |
| 错误类 | Harness 识别 `ReadOnlySession`/`SessionFailed`/`ConversationBusy` |

### 5.6 包的模块边界（Rust 侧必须保持）

`storage-runtime-boundary.test.ts` 断言：
- 包根只含**可移植**核心存储；
- `/storage/sqlite` 与 `/storage/jsonl` 与 `/env` **不得**引入 Node API；
- Node 专属代码只在 `/storage/sqlite/node`、`/storage/jsonl/node`、`/env/node`、`/images/node` 等子路径。

**Rust 映射建议**：同一 crate 内用 `#[cfg(feature = "node")]` 或独立 crate（`kiana-durable-core`、`kiana-durable-node`）实现；CI 需检查 `kiana-durable-core` 不依赖 `tokio::process`/`std::fs`（或仅通过 trait）。

---

## 6. npm 依赖与 Rust crate 对应

### 6.1 直接依赖（`packages/durable/package.json`，**逐字版本**）

```json
"dependencies": {
  "@earendil-works/chord": "1.4.0",
  "@earendil-works/kiana-ai": "1.4.0",
  "@earendil-works/kiana-codemode": "1.4.0",
  "@earendil-works/kiana-telemetry": "1.4.0",
  "@silvia-odwyer/photon": "0.3.3",
  "diff": "8.0.4",
  "typebox": "1.3.27"
}
```
无 `peerDependencies`；包版本 `1.4.0`。`@earendil-works/kiana-telemetry` 只用于 `TelemetryCarrier` 类型（本单元记录里的 `trace`/`links` 字段），Rust 侧对应 `kiana-telemetry` crate 的载体类型。

| npm 包 | 用途（本单元） | Rust crate | 差异与桥接 |
|---|---|---|---|
| `@earendil-works/chord` | JsonValue/copyJson/delta/Tracker/Watch/Context/output/utils | `kiana-chord`（自研，对应本 monorepo 的另一单元） | 必须逐位对齐 `Op` 编码与 `copyJson` 的 `omitUndefinedProperties` |
| `@earendil-works/chord/sqlite` | SQLite 门面与迁移 | `rusqlite`（bundled）+ 自研 facade | `rusqlite` 是同步的；需要一个异步门面（`tokio::task::spawn_blocking` 或专用线程 + channel）以匹配 `SqliteDatabase` 的 promise 语义 |
| `@earendil-works/kiana-ai` | Messages/Models/Tool/Usage | `kiana-ai`（自研） | TypeBox → JSON Schema；`Usage` 字段名保持一致 |
| `@earendil-works/kiana-codemode` | QuickJS 沙箱、声明渲染 | `kiana-codemode`（自研，`rquickjs`） | `renderToolSample`/`toCodemodeIdentifier` 输出必须逐字一致 |
| `typebox` | 工具参数 schema | `schemars` 或手写 `serde_json::json!` | TypeBox 默认 `additionalProperties: false`？需逐工具核对；`Type.Unknown()` → `{}` |
| `diff`（jsdiff） | `createTwoFilesPatch`/`diffLines` | `similar` | **必须**逐字节复刻 jsdiff 的 `createTwoFilesPatch` 与 `diffLines` 输出（含 `@@` 范围的计数规则、`\ No newline at end of file` 的处理）。`similar` 的算法不同，建议自行实现 Myers/patience 后按 jsdiff 格式输出 |
| `@silvia-odwyer/photon` | WASM 图像处理 | `image` + `kamadak-exif`（或保留自写 EXIF） | 编码参数、缩放滤镜（Lanczos3）、JPEG 质量链必须一致；Photon 的 `get_bytes()` 是 PNG 编码 |
| `@silvia-odwyer/photon/photon_rs.js` | Photon JS API（`PhotonImage`、`resize`、`fliph`、`flipv`、`SamplingFilter`） | 同上 | |
| `node:fs` / `node:fs/promises` | 全部文件操作 | `tokio::fs` + `std::fs`（元数据） | `mtimeMs` 是浮点毫秒；`fs.watch` → `notify` |
| `node:path` | `resolve`/`join`/`basename` | `std::path`（Unix）/`path` 语义差异 | Windows 下需 `dunce`/手工处理 |
| `node:os` | `homedir` | `dirs` / `home` | Linux `$HOME`，Windows `%USERPROFILE%` |
| `node:url` | `fileURLToPath` | `url` crate | `file://` 解码规则 |
| `node:child_process` | `spawn`/`execFile` | `tokio::process` | 进程组、`taskkill` |
| `node:worker_threads` | Photon worker | `tokio::task::spawn_blocking` | 不需要独立线程池 |
| `node:crypto` | `randomUUID` | `uuid`（v4） | 大小写与连字符格式一致 |
| `node:module` | `createRequire` | 不适用（资源内嵌/路径查找） | |
| `btoa`/`atob` | base64 | `base64` | 标准字母表 + padding；`atob` 忽略空白（我们输入无空白） |
| `TextDecoder`/`TextEncoder` | UTF-8 流式解码/编码 | `encoding_rs` | 非 fatal 模式 → U+FFFD 替换；fatal → 错误（JSONL 用）；`ignoreBOM` 语义需自己实现（§2.21） |
| `fetch`/IndexedDB | 浏览器后端 | `web-sys`/`indexed_db_futures`（仅 wasm） | 见 §2.16 的取舍 |
| `navigator.storage.persist` | 仅 README 建议 | n/a | |
| `AbortSignal`/`DOMException` | 取消 | `tokio_util::sync::CancellationToken` + 自定义错误 | 错误消息 `The operation was aborted` 与 `name: "AbortError"` |
| `queueMicrotask` | 延迟 0 的清扫 | `tokio::task::yield_now` / `spawn` | 语义：在当前宏任务后立即执行 |
| `setTimeout`/`clearTimeout` | 计时器 | `tokio::time::sleep`（需 `unref` 语义 → 用独立 `JoinHandle` 并在 drop 时 abort） | 后台计时器不得阻止进程退出：Rust 中即"不 join、drop 即取消" |
| `structuredClone`（若使用） | 未使用 | n/a | |
| `Object.freeze` | 冻结返回值 | 无直接对应 | 用不可变所有权保证 |

### 6.2 关键桥接点（按风险排序）

1. **JS 字符串 vs Rust 字符串**
   - `string.length` / `slice` / `indexOf` 都是 UTF-16 code unit 语义。本单元中受影响处：
     - `read.ts` 的 `sliceIndex`、`startLine + limit`、`head.split("\n")[0]`（按 `\n`，UTF-16 无影响）；
     - `edit-diff.ts` 的所有偏移（`matchIndex`、`matchLength`、`line.length`）—— 这些偏移进入 `substring`/`slice`，若 Rust 用字节偏移会与 JS 不一致；
     - `output.ts` 的 `combined.slice(0, headChars)`/`slice(-tailChars)`、`combined.length`；
     - `describe.ts` 的 `section.length / 4`；
     - `truncate.ts` 只用 `utf8ByteLength` 与按 `\n` 切分，安全；
     - `search.ts` 的 `term.length`、`slice`（token 为 ASCII，安全）。
   - **决策**：Rust 内部统一用 `String`（UTF-8），但凡需要与 JS 偏移一致的算法，改用 **UTF-16 索引适配层**（`Utf16Index`：缓存 `char_indices` 或对短文本线性扫描）。**不要**简单用 `char_indices`（code point ≠ code unit，emoji 等会差 2）。
   - 也可考虑内部统一用 `Vec<u16>`（`widestring::Utf16String`）表示"参与偏移计算的文本"，只在 I/O 边界转换。**推荐**：对 `edit-diff`/`output`/`describe` 使用 `Utf16String`。

2. **正则方言**
   - **全量正则清单（本单元 in-scope 源文件，逐字）**：
     | 文件 | 正则 | 用途 |
     |---|---|---|
     | `search.ts` | `/([a-z0-9])([A-Z])/g` → `"$1 $2"` | camelCase 切分 |
     | `search.ts` | `/([A-Z]+)([A-Z][a-z])/g` → `"$1 $2"` | HTTPServer 切分 |
     | `search.ts` | `/[^a-z0-9]+/` | 分词 |
     | `search.ts` | `/(ches|shes|sses|xes|zes)$/` | 词干 |
     | `edit-diff.ts` | `/\r\n/g`、`/\r/g`、`/\n/g` | 换行归一化 |
     | `edit-diff.ts` | `/[^\n]*\n|[^\n]+/g` | `splitLinesWithEndings` |
     | `edit-diff.ts` | `/[...smart quotes...]/g` 等 4 条 | 模糊匹配归一化（见 §2.25.9） |
     | `path-utils.ts` | `/ (AM|PM)\./gi` → `` `${NARROW_NO_BREAK_SPACE}$1.` `` | macOS 截图文件名 |
     | `path-utils.ts` | `/'/g`、`/\u2019/g`（NFD 后） | 同上 |
     | `env/node.ts` | `/\r?\n/` | `where`/`which` 输出首行 |
     | `tool-search.ts` | `/\r?\n/` | 描述首行 |
     | `jsonl/storage.ts` | `/^(?:doc|task)-(?:0|[1-9]\d*)\.jsonl$/`、`...\.reclaim$/` | 文件名识别 |
     | `harness/tool.ts`（外单元） | `/^[1-9][0-9]*$/` | 嵌套调用 key 校验 |
     | `images/*` | **无正则**；data URL 手写解析（见 §2.26） | — |
     | `env/node-watch.ts` | 无正则（用字符串比较） | — |
   - 全部是 ASCII 范围，`regex` crate 可精确表达（无 backreference/lookaround）。
   - **注意**：JS 的 `.` 不匹配换行（无 `s` 标志）；`regex` crate 默认也不匹配 `\n`。JS 的 `$` 在无 `m` 时匹配字符串末尾（且允许末尾 `\n` 前？—— JS 中 `$` 不匹配末尾换行前，除非 `m`）。`/(ches|...)$/` 在 `regex` 中相同。
   - `String.prototype.normalize("NFKC"/"NFD")` → `unicode-normalization` crate。

3. **JSON 键顺序与数字格式**
   - `serde_json` 默认 `BTreeMap` 会**排序**键 → 与 JS 插入序不符。**必须**启用 `preserve_order` 特性（内部用 `IndexMap`）。
   - 浮点数：`serde_json` 输出 `1.0` 对 `f64`，而 JS 输出 `1`。本单元的 JSON 值来自 Entry/Task 记录，若含 `1.0` 需输出 `1` → 需要自定义序列化（用 `serde_json::Number` 的 `from_f64` 会保留 `.0`）。**决策**：内部值统一用 `Value`，数字只在确为整数时用 `Number::from(i64)`；解析时用 `arbitrary_precision` 或保证反序列化后整数保持整数。
   - JS `JSON.stringify` 省略 `undefined` 属性；`serde` 用 `#[serde(skip_serializing_if = "Option::is_none")]`。但 `copyJson(value, { omitUndefinedProperties: true })` 作用于**任意 JsonValue**（其中 `undefined` 在 Rust 中不存在）→ Rust 侧天然满足。
   - JS 对 `NaN`/`Infinity` 输出 `null`；Rust `serde_json` 会报错。本单元不应产生这些值（超时/尺寸都有限）。

4. **Date / 时间**
   - `Date.now()` → 毫秒整数；`FileInfo.mtimeMs` 是浮点毫秒（含小数）。比较 `after.mtimeMs === before.mtimeMs` 需要位级相等 → Rust 用 `f64` 或 `SystemTime` 纳秒转毫秒浮点。**注意**：不同平台的 `mtimeMs` 精度不同（Linux ns，Windows 100ns）。
   - 无时区/格式化（本单元不格式化日期）。

5. **base64**
   - `toBase64` 用 `String.fromCharCode` + `btoa` → 标准 base64（`+/`、`=` padding）。
   - `base64Length(n) = ceil(n/3)*4`。
   - `atob` 在 `saveImages` 中解码（输入是我们自己的 base64，无空白）。

6. **TextDecoder 的 BOM 语义**
   - `new TextDecoder("utf-8")`（默认 `ignoreBOM: false`）会**丢弃开头的 BOM**。
   - `new TextDecoder("utf-8", { ignoreBOM: true })` 会**保留**所有 BOM（包括开头）。
   - 本单元：`rangeDecoder()` 用 `ignoreBOM: true`（保留），然后 `StreamDecoder`/`LineScanner`/`readHead` 手工去掉**流首**的 `EF BB BF`。
   - Rust `encoding_rs::UTF_8.new_decoder_without_bom_handling()` 对应 `ignoreBOM: true`；`new_decoder()` 对应默认（去 BOM）。

7. **QuickJS vs V8**
   - `formatError` 直接用 `error.stack`；`Error: message\n    at ...` 的格式由引擎决定。**若沙箱执行在 QuickJS（TS 侧也是 QuickJS！）**，则 Rust 的 `rquickjs` 需要产生相同 stack 格式。TS 侧 `@earendil-works/kiana-codemode` 使用 QuickJS，因此 stack 格式是 QuickJS 的格式——Rust 用 `rquickjs` 可以接近，但需验证。
   - `JSON.stringify` 在沙箱内也是 QuickJS 的。

8. **`Map`/`Set` 顺序**
   - `#documents`/`#idleDocuments`/`#holds` 的迭代顺序可观察（清扫顺序、`#publish` 顺序）。Rust 用 `indexmap::IndexMap`（保持插入序）或 `Vec` + `HashMap`。

9. **Promise 微任务顺序**
   - Session 的串行线基于 promise `.then`。Rust 用 `tokio::sync::Mutex` + 队列，或 `futures::stream::FuturesUnordered` + 单任务循环。**关键**：`#enqueue` 的语义是"前一个 job 完成后才开始下一个，前一个失败不影响下一个"，且**入队顺序 = 调用顺序**。用 `tokio::sync::mpsc::unbounded_channel` + 单消费者任务即可，但需要支持"返回 job 的结果"（用 oneshot）。

10. **`queueMicrotask` 与计时器**
    - `delay <= 0` 的清扫用 `queueMicrotask`（在当前同步代码后、下一宏任务前）。Rust 无微任务概念，用 `tokio::task::yield_now().await` 或直接在当前 job 内同步执行（语义近似）。
    - 后台计时器"不得阻止进程退出"：Rust 中 `tokio::spawn` 的任务不会阻止 runtime 关闭（`Runtime::shutdown_background`），但持有 `JoinHandle` 不影响；关键是不要 `await` 它。

11. **`structuredOutputSchema: Type.Unknown()`**
    - TypeBox `Type.Unknown()` 序列化为 `{}`。Rust 需输出 `{}` 而非 `{"type":"object"}`。

12. **`constrainedSampling`（lark 语法）**
    - 由 kiana-ai 使用；本单元只传字符串，无需实现语法。

13. **`process.platform` 差异**
    - Windows 路径分隔符、`taskkill`、Git Bash 候选路径、`where` vs `which`、`fs.watch` 强制 polling、`mtimeMs` 精度。
    - Rust：用 `cfg!(windows)` 分支；`which` crate。

14. **符号链接**
    - `lstat`（不跟随）用于 `fileInfo`/`listDir`/`DirReader`；`openBinaryReader` 默认跟随，`noFollow` 时拒绝；`canonicalPath` = `realpath`（跟随）。
    - Rust：`std::fs::symlink_metadata` vs `metadata`；`canonicalize`。
    - **注意**：`remove(path, { recursive: true })` 对符号链接不应递归进目标（Node 的 `fs.rm` 对 symlink 是删除链接本身）。

15. **`process.cwd()` 与相对路径**
    - `NodeExecutionEnv.cwd` 可被外部修改（`cwd` 是可变字段）；`absolutePath` 在每次调用时读取。
    - `openNodeJsonlStorage` 在**调用时**捕获 `process.cwd()`。

16. **错误 `cause` 链**
    - TS 中 `new Error(msg, { cause })`；Rust 用 `thiserror` 的 `#[source]` 或 `anyhow` 的 context。**消息文本**必须只用外层 `message`，不得把 `cause` 拼进消息。

17. **`Number.MAX_SAFE_INTEGER`**
    - `9007199254740991`；SQLite 参数与降序 cursor 缺省值。
    - Rust 用 `i64` 可容纳；但 JS 的 safe integer 上限是 2^53-1，超过会精度丢失 → 本单元的 ID/seq 远小于此，但校验必须按 `Number.isSafeInteger`（`|v| <= 2^53-1`）判定。

18. **`ID space is exhausted`**
    - 当 `nextId > Number.MAX_SAFE_INTEGER`。

19. **`JSON.stringify` 对 `-0`**
    - `-0` → `"0"`；Rust `serde_json` 输出 `-0.0`。本单元不应出现 `-0`。

20. **整数字段的边界**
    - `seq >= 1`（JSONL 校验）；`ordinal >= 0`；`version >= 1`。

---

### 6.3 本单元导出符号计数（供移植时核对完整性）

按"每个源文件声明的 export 符号数"（不含 barrel 再导出；`photon-wasm.d.ts` 与环境声明不计）：

| 文件 | 导出数 |
|---|---|
| `src/types.ts` | 85 |
| `src/env/index.ts` | 32（声明 32 + 末尾再导出 4 个 `rangeDecoder`/`StreamDecoder`/`startsWithBom`/`LineScanner`；**barrel 语义上对外可见 36 个名字**） |
| `src/tools/edit-diff.ts` | 13 |
| `src/codemode/codemode.ts` | 12 |
| `src/documents.ts` | 12 |
| `src/codemode/models.ts` | 9 |
| `src/tools/bash.ts` | 9 |
| `src/session/transaction.ts` | 8 |
| `src/codemode/describe.ts` | 7 |
| `src/entries.ts` | 7 |
| `src/session/session.ts` | 7 |
| `src/tools/image-processor.ts` | 7 |
| `src/truncate.ts` | 7（本地声明 7 个；`export { utf8ByteLength }` 是转出 chord 的名字，**不计入本列**，故该文件对外可见 8 个名字） |
| `src/errors.ts` | 6 |
| `src/storage/sqlite/migrations.ts` | 5（本地声明 5 个；`export type { SqliteMigration }` 转出 chord 的名字不计，对外可见 6 个） |
| `src/tool-search/tool-search.ts` | 5 |
| `src/storage/jsonl/storage.ts` | 4 |
| `src/tools/image.ts` | 4 |
| `src/tools/read.ts` | 4 |
| `src/codemode/output.ts` | 3 |
| `src/env/decode.ts` | 3 |
| `src/search.ts` | 3 |
| `src/storage/scan.ts` | 3 |
| `src/storage/sqlite/schema.ts` | 3（声明 `SqliteSchema`/`createSqliteSchema`/`DEFAULT_SQLITE_SCHEMA`；另 `export { validateSqliteSchemaPrefix }` 转出 chord 的 1 个，对外 4 个名字） |
| `src/timers.ts` | 3 |
| `src/tools/edit.ts` | 3 |
| `src/env/node-watch.ts` | 2 |
| `src/ids.ts` | 2 |
| `src/images/node-worker.ts` | 2 |
| `src/images/node.ts` | 2 |
| `src/session/forks.ts` | 2 |
| `src/storage/memory.ts` | 2 |
| `src/storage/sqlite/node.ts` | 2（声明 `NodeSqliteStorageOptions`/`openNodeSqliteStorage`；另从 chord 转出 `NodeSqliteDatabase`/`NodeSqliteDatabaseOptions`/`openNodeSqliteDatabase` 3 个，对外 5 个名字） |
| `src/storage/sqlite/storage.ts` | 2 |
| `src/tools/index.ts` | 2（**仅本地声明**：`createCodingTools`、`CodingTools`；另有 5 个 `export {}` 块从 bash/edit/image-processor/read/write 转出 **21 个名字**，故 `./tools` 子路径对外可见 **23 个名字**） |
| `src/tools/path-utils.ts` | 2 |
| `src/tools/write.ts` | 2 |
| `src/env/line-scan.ts` | 1 |
| `src/env/node.ts` | 1 |
| `src/images/cloudflare.ts` | 1 |
| `src/images/exif.ts` | 1 |
| `src/images/index.ts` | 1 |
| `src/storage/indexeddb.ts` | 1（声明 `IndexedDbStorage`；文件末尾 `export type { IndexedDbStorageOptions }`，对外 2 个名字） |
| `src/storage/jsonl/node.ts` | 1（声明 `openNodeJsonlStorage`；另转出 `JsonlStorage` 与 `JsonlStorageOptions`，对外 3 个名字） |
| `src/storage/sqlite/cloudflare.ts` | 1（声明 `openDurableObjectSqliteStorage`；另从 chord 转出 `DurableObjectSqliteDatabase`/`DurableObjectSqliteStorage` 2 个，对外 3 个名字） |
| `src/tasks.ts` | 1 |
| `src/tools/env.ts` | 1 |
| `src/tools/file-mutation-queue.ts` | 1 |
| **合计** | **297** |

（barrel 文件 `src/index.ts`、`src/codemode/index.ts`、`src/storage/jsonl/index.ts`、`src/storage/sqlite/index.ts`、`src/tool-search/index.ts`、`src/tools/index.ts` 自身不声明符号，只再导出；`src/storage/sqlite/database.ts` 仅一行 `export type { SqliteDatabase, SqliteExecutor, SqliteValue } from "@earendil-works/chord/sqlite"`——**注意原 spec 把它写成 `NodeSqliteDatabase`，实际是这三个可移植类型**。)

上表共 **48 行，逐行加总 = 297**，覆盖 `src/**` 下所有含本地声明的文件（不含 `harness/**`、`testing/**` 与本单元外文件）。纯 barrel（0 个本地声明）：`src/index.ts`、`src/codemode/index.ts`、`src/storage/jsonl/index.ts`、`src/storage/sqlite/index.ts`、`src/storage/sqlite/database.ts`、`src/tool-search/index.ts`；`src/images/photon-wasm.d.ts` 是环境声明，不计。

**计数口径提醒**：本表数的是"本地声明的 export 符号"（按名字去重，函数重载只算 1）。**子路径对外可见的名字数与此不同**，因为 barrel 还会转出依赖包的名字：`./env` 36、`./tools` 23、`./images` 1、`./images/node` 2、`./images/cloudflare` 1、`./tool-search` 5、`./codemode` 12、`./storage/memory` 2、`./storage/indexeddb` 2、`./storage/jsonl` 4、`./storage/jsonl/node` 3、`./storage/sqlite` 9（3 个 chord 类型 + 4 个迁移 + 2 个存储）、`./storage/sqlite/node` 5、`./storage/sqlite/cloudflare` 3、根 `./`（`index.ts` 的 26 条 export 语句：chord 2 + 各模块本地/转出 + harness 约 160 + types.ts 84 + `ROOT_CONVERSATION_ID`）。

---

## 7. 测试清单

测试目录：`packages/durable/test/`（**75 个 `.test.ts`，38,240 行**）。支持文件（非测试）：`chat-support.ts`、`harness-support.ts`、`session-support.ts`、`task-support.ts`、`storage-memory.ts`（共享 conformance 注册器）、`setup-document-retention.ts`（vitest setup）、`heap-retained.ts`；bench（**不在移植范围**）：`codemode-bench.ts`、`document-append-bench.ts`、`nested-images-bench.ts`、`nested-tools-bench.ts`、`scheduler-fanout-bench.ts`、`scheduler-memory-bench.ts`、`session-document-cache-bench.ts`、`storage.bench.ts`、`tool-output-bench.ts`；另有 `test/fixtures/` 与 `test/examples/` 目录。配置：`vitest.config.ts`（environment `node`；`exclude: [...configDefaults.exclude, "test-workers/**"]`；`setupFiles: ["./test/setup-document-retention.ts"]`；别名用**正则**：`/^@earendil-works\/kiana-durable$/` → `src/index.ts`、`/^@earendil-works\/kiana-durable\/testing$/` → `src/testing/index.ts`；`resolve.conditions: ["source"]` 且 **`ssr.resolve.conditions: ["source"]` 也要设**）。`vitest.benchmark.config.ts`：`benchmark.include: ["test/**/*.bench.ts"]`、`reporters: ["verbose"]`。`test-workers/vitest.config.ts`：`root` 指向 `test-workers/`、`plugins: [cloudflareTest({ wrangler: { configPath: "./wrangler.jsonc" } })]`、`test.include: ["*.test.ts"]`、别名额外含 `/^@earendil-works\/kiana-durable\/storage\/sqlite\/cloudflare$/`。

> Rust 移植策略：本单元的测试必须**全部**移植为 Rust 集成测试。下表按文件给出 `describe` 组与测试数，`it` 标题本身即验收项（英文原文保留，便于对照）。**标 ★ 的文件与本单元强相关，必须先移植。**

### 7.1 存储后端

| 文件 | tests | describe | 备注 |
|---|---|---|---|
| `storage-memory.ts` | — | — | **支持文件（非测试）**：共享 `registerStorageConformance` 注册器 + `node:sqlite`/`node:fs` 实现的 conformance 辅助（被 `memory-storage`/`jsonl-storage`/`sqlite-storage` 使用） |
| `memory-storage.test.ts` | 1 | — | "does not expose retained state through a prepared commit" |
| `jsonl-storage.test.ts` ★ | 23 | "Pico JsonlStorage publication and recovery" | 全部 23 条见下 |
| `sqlite-storage.test.ts` ★ | 13 | "Pico SqliteStorage" | 见下 |
| `sqlite-migrations.test.ts` ★ | 4 | "durable SQLite migrations" | |
| `sqlite-facade.test.ts` | 6 | "portable SQLite facade settlement" | chord 门面 |
| `sqlite-cloudflare.test.ts` | 4 | "Durable Object SQLite adapter" | 可选（wasm/Workers） |
| `indexeddb-storage.test.ts` | 4 | — | 浏览器；若不做可跳过（需记录） |
| `storage-runtime-boundary.test.ts` ★ | 4 | "durable storage runtime boundaries" | 包边界断言 |

**jsonl-storage.test.ts 的 23 条**：
1. opens through the Node adapter
2. publishes every commit before reclaiming current-only document and terminal-task sidecars
3. orders multiple live-task replacements in one sidecar by commit ordinal
4. removes a complete-plus-torn unconfirmed multi-record sidecar append
5. serializes the complete candidate before I/O and leaves preparation failures usable
6. `poisons after ${mode} failure at append ${call} and recovers only confirmed state`（参数化）
7. `poisons after ${mode} failure at flush ${call} and never writes a marker`
8. orders publication flushes exactly and flushes main only to authorize reclamation
9. `recovers a committed base across reclaim ${operation} ${mode}`
10. `recovers document-retirement reclamation across remove ${mode}`
11. `defers reclamation after authorizing-main flush ${mode} failure`
12. `keeps a committed base usable after reclaim-temp flush ${mode} failure`
13. `recovers terminal-task reclamation across remove ${mode}`
14. appends later deltas to the replacement sidecar after a current-only base
15. never reclaims rewindable document history, including after a base and retirement
16. reclaims retired task-, session-, and latest-conversation document sidecars
17. truncates torn UTF-8 tails at exact byte offsets and reuses the unconfirmed sequence
18. removes complete unconfirmed sidecar tails without resurrecting terminal tasks
19. fails open when confirmed sidecar data is missing
20. rejects a confirmed record after an unconfirmed sidecar record
21. rejects non-increasing main commit sequences
22. rejects structurally invalid confirmed document content
23. rejects malformed complete main and sidecar lines

**sqlite-storage.test.ts 的 13 条**：
1. persists records, sequence allocation, and global ID allocation across reopen
2. namespaces tables and indexes with a prefix across reopen
3. rejects an invalid prefix before creating the database
4. rejects persisted metadata corruption on reopen
5. rejects a document whose required base is missing
6. replays detached root replacements and follow-up edits while rejecting corrupt operations
7. rolls SQL rows and sequence allocation back as one transaction
8. reconstructs recent and ancient rewindable points after reopen
9. uses indexes for exact addresses, exact scopes, entry history, and document revision tails（查询计划断言）
10. reclaims current-only revisions only after a base or retirement
11. auto-checkpoints WAL frames and truncates the WAL on close
12. reuses pages released by current-only checkpoints
13. keeps representative row and document storage bounded（体积上界）

**sqlite-migrations.test.ts**：creates the current schema and can be applied repeatedly / migrates a version 1 database to store sleeping tasks, keeping its tasks and indexes / rejects a database newer than the portable core / rolls a failed migration back and preserves stored data for a successful retry。

**storage-runtime-boundary.test.ts**：keeps the package root limited to portable core storage / keeps the portable SQLite subpath free of Node imports / keeps the portable environment subpath free of Node imports / keeps the portable JSONL subpath free of Node imports。

### 7.2 Session 内核

| 文件 | tests | describe |
|---|---|---|
| `session-adoption.test.ts` ★ | 5 | "Session adoption inside Storage.commit()" |
| `session-documents.test.ts` ★ | 27 | "Session document transactions" |
| `session-tables.test.ts` ★ | 12 | "Session transaction tables" |
| `session-forks.test.ts` ★ | 14 | "Session conversation document forks" |
| `session-watches.test.ts` ★ | 23 | "Session document watches" |
| `session-deferred-commits.test.ts` ★ | 25 | "holding a deferred commit" / "storing held changes" / "bases and overflow" / "publications" / "pinning" / "crash" |
| `session-document-retention.test.ts` ★ | 21 | "document cache retention" |
| `session-checkpoints-migrations.test.ts` ★ | 20 | "Session document checkpoints" / "Session document migrations" / "Session historical document snapshots" / "Session tracker cache across definition versions" |
| `session-definitions.test.ts` | 4 | "document definitions" |
| `types.test.ts` | 3 | —（类型层，Rust 编译期） |
| `spec-usage.test.ts` | 1 | — |
| `provider-session-cache-e2e.test.ts` | 1 | "Durable provider session cache e2e"（`it.skipIf(!codexToken)`："reuses the Codex prompt cache across Durable turns"，`{ retry: 2, timeout: 120_000 }`；需要 `~/.kiana/agent/auth.json` 凭据，无凭据时跳过） |
| `system-order-cache-e2e.test.ts` | 1 | "Durable system message order e2e"（`it.skipIf(!anthropicToken)`："keeps the prompt cache across a tool change only when the system message leads"，`{ retry: 1, timeout: 180_000 }`；断言 tool 变更后 cache read < 0.2 与 > 0.8） |

关键断言（摘录，作为验收点）：
- **adoption**：adopt 在 commit 内同步发生、早于 Storage resolve；Storage 在 adopt 后拒绝 → Session 失败但不丢弃；在 adopt 前拒绝 → 丢弃 staged；（Session 自身的 adoption 错误不进入 Storage）；Storage resolve 但未 adopt → 失败。
- **documents**：首次访问创建 base 并在 Storage 成功后采纳；snapshot 从不创建；返回共享不可变快照且旧 revision 稳定；adopt 用指针交换并共享操作载荷；每次放置复制赋值；空批抑制写与发布；可重放的非空结构 no-op 会写并发布；回调结算后撤销逃逸的 draft；回调失败中止全部变更；并发重复获取记忆化；family 首个 seed 生效；回调成功但仍有 pending acquisition → 拒绝并排空；不初始化也不 mint 在结算后完成的 absent acquisition；回调失败时以回调错误拒绝；结算后拒绝 Tx 使用；token 语义/版本不符拒绝；非 JSON 初始化值与 draft 放置被拒；批装配失败回滚全部已准备文档；Storage 失败 → Session 失败且不发布；Storage 结算期间旧 revision 不变；退休并同址新建；无 acquisition 的退休；退休与 pending acquisition 竞争；重新加载已卸载文档；adopt 后同步投递完整发布；close 同步发布并支持退订；close 前结算已接纳提交并拒绝后续接纳。
- **tables**：表读只能在第一次表写之前；分页 limit/cursor 透传；同步 `setTask` 算第一次表写；mint ID 创建会话/条目/任务；显式 staged 任务 owner 创建会话；缺失/终态/abort-marked owner 原子拒绝；表 JSON 取所有权并拒绝非严格值；任务记录完全替换；同一事务内创建任务再创建其文档不触发 ReadAfterWrite；终态候选之后拒绝任务文档；终态结算退休任务文档（含同事务创建的）；校验文档 owner。
- **forks**：复制 as-of 与 current 的单例与 family base，initial 保持 absent；使用 fork 条目提交时的最终文档状态且排除同提交后续条目；as-of 选条目所属祖先、current 选直接父；复制存储值与版本，不咨询迁移定义或已迁移缓存；合并类型化迁移与 override 到复制创建 base；跨退休/重建复制 fork 点存活化身；不可见 fork 点预接纳拒绝且仍可用；同一子地址的重复 as-of/current 选择预接纳拒绝；fork 事务中拒绝 current 与 as-of 源写；后续预接纳装配失败回滚全部复制 base；fork 提交被拒 → Session 失败且无效果；fork 事务中退休复制文档并可重建地址；只复制会话文档；跨存储扫描页复制每个 family 成员。
- **watches**：从不创建 absent 文档；保持 acquisition revision 直到 start，随后一次投递其间提交；串行化回调并把在途期间提交折叠进下一次；允许监听器发起后续 Session 提交；101 个 pending 提交折叠为一次 set；>10,000 ops 后根替换；从不在途帧折叠进下一次；退休折叠后关闭；投递可重放结构 no-op；保留 Context 值且不继承生产者取消；旧 revision 稳定；投递退休且不跟随重建；close 丢弃 start 前缓冲的退休；close 丢弃在途回调后的退休；幂等 stop 且拒绝重复/late start；取消 acquisition 不泄漏已注册 watch；取消未来投递不中止在途回调；close 停止未来投递不 join 在途；监听器失败只影响该 watch；迁移水合不写并观察之后精确编辑；可从 acquisition revision 重放每个已投递精确操作批；跟踪器缓存卸载后继续；停止后不保留 Session 或 listener。
- **deferred commits**：仅内存采纳既有 latest + initial-fork 会话文档的提交（不存储）；写任何其他东西 → 立刻存储；迁移其文档 → 立刻作为 base 存储；只读 Session 上像任何有写提交一样拒绝；拒绝非毫秒数/负的 withinMs；与其自身写折叠存储在写该文档的提交中；在只读该文档的提交中与其他写一起存储；既不读也不写该文档、或什么都不写的提交保持 held；读该文档的提交在 Storage 前失败 → 保持；最早 deadline 时一次事务存储全部 held 文档；按各自 withinMs 或默认值武装最早 deadline；无限窗口不武装计时器；提交存储该 held 文档后计时器后移；close 时在之前接纳的提交之后存储，且该存储失败则关闭 Storage；Session 失败后不存储；在 `unloadDocuments()` 丢弃 tracker 之前存储；在另一版本 token 重载该文档之前存储；在文档退休的提交中、退休之前存储；held 提交选了 base 则存 base，否则计一次 delta；超过 MAX_PENDING_OPS 存整值；溢出 hold 存提交自身值的 base；`subscribeCommits()` 只看到实际存储的内容（held 提交无）；每条 held 提交对 watches 与 Session 自身观察者只显示一次；held 文档在存储前以 retention 0 保持缓存，之后释放；held revision 只保留到存储；崩溃后 Storage 停在最后存储状态，重开后无 held。
- **retention**：retention 过后驱逐未附加文档，重新加载得到相同值；retention 0 在线立即驱逐；watch 观察的文档永不驱逐，最后一个 watch 解除后驱逐；从不从持有它的事务取走 tracker；重新加载后写继续存储的 deltas 与 bases；rewindable 文档驱逐后保留历史与 fork；第三方 family 每个事件保持一个成员且有界；长时间 Session 跨多会话缓存有界；时钟抛错不改变提交结果；结算失败的提交后仍驱逐；retention 缩短后及时驱逐；不为已退休化身的观察者保留替换；观察者加载与 close 竞争失败不留 pin；无提交时在计时器上驱逐，容忍重复 stop；驱逐后再次迁移；非毫秒数 retention 视为默认，Infinity 为永不；无法 unref 计时器的环境不留计时器、在提交时驱逐；Harness 设置变化在下次使用时生效；文档立即驱逐与保留时事件相同；旧版本观察者附加期间保留重载的新版本缓存；提交持线时只排队一个 sweep job 而非每文档一个。
- **checkpoints/migrations**：以定义为 receiver 调用 `checkpointWhen`；只为非空普通批选择 base 并传递精确的已准备 revision 与 ops；传递自最新 base 以来的存储 delta 数（含卸载与版本 base 之后）；空批跳过谓词，但为非空结构 no-op 调用；谓词抛错回滚全部已准备文档；重复 false 决定持久为 deltas 并重放完整尾部；谓词为 false 时根替换保持为 delta；退休前使用普通 checkpoint 选择；只读迁移每冷加载一次、拷贝回调结果、不写；第一次成功事务写所需 base，之后写 deltas；迁移与编辑随回调回滚，之后编辑合并为一个 base；在 tracker 所有权前严格校验迁移结果，拒绝后仍可用；拒绝更新的存储版本与无迁移的旧版本（snapshot 与事务）；退休前持久所需迁移 base 而不咨询谓词；未访问的旧文档与不可用定义不动；当前与历史 rewindable 值独立迁移并跟随 fork 祖先；选择在条目提交时存活的化身（跨退休与重建）；旧 token 缓存的文档迁移而不卸载；新 token 写入后旧 shape 观察者收到根替换；旧 token 从 Storage 服务新 token 仅在内存迁移的情况。

### 7.3 env

| 文件 | tests | describe |
|---|---|---|
| `env-node.test.ts` ★ | **47**（`it`/`it.each`/`it.skipIf` 声明数；filesystem 18 + text line reader 6 + shell 23） | "NodeExecutionEnv filesystem" / "NodeExecutionEnv text line reader" / "NodeExecutionEnv shell" |
| `env-node-spill.test.ts` ★ | **1** | "NodeExecutionEnv spill backpressure" |
| `env-node-conformance.test.ts` ★ | **3** | "NodeExecutionEnv watch limits" / "NodeExecutionEnv readers" |
| `env-line-scan.test.ts` ★ | 4 | "StreamDecoder" / "LineScanner" |
| `env-truncate.test.ts` ★ | 8 | "truncate utilities" |

**filesystem 组**（18 条）：reads, writes, lists, and removes files and directories / expands home-relative paths and file URLs / returns fileInfo for files, directories, and symlinks without following symlinks / lists symlinks as symlinks / stops reading text lines at the requested limit / returns FileError for missing paths and keeps exists false for missing paths / returns FileError for listing non-directories / appends to new files and creates parent directories / atomically renames a file and replaces the destination / reports the source path when rename fails because the source is missing / creates temporary directories and files / honors createDir recursive false and remove recursive/force options / returns aborted results without side effects for pre-aborted file operations / truncates and extends files to exact byte sizes / rejects invalid truncation sizes and never creates missing files / flushes existing files without changing content and reports missing paths / syncs through an opened handle, returns sync failures, and always closes the handle / cleanup is best-effort。

**text line reader 组**（6 条）：reports whether each line was newline-terminated and preserves carriage returns / returns no lines for an empty file and one terminated empty line for a lone newline / decodes multi-byte characters split across read chunks and lines longer than one chunk / rejects a read cancelled while pending without consuming its bytes / rejects reads after close and closes idempotently / reports missing files when opening a reader。

**shell 组**（**23 条声明**；原 spec 漏了 5 条）：executes commands in cwd with env overrides / `applies string shell environment overrides when %s`（**`it.each` 3 例**：a missing override preserves the base value / an empty override shadows the base value / a string override replaces the base value） / can replace rather than inherit the default shell environment / `selects command transport for %s (SystemRoot=%j)`（**`it.each` 5 例**：`C:\Windows\System32\bash.exe`+无 SystemRoot → `-s`；`D:\WinNT\System32\bash.exe`+`D:\WinNT` → `-s`；`D:\WinNT\Sysnative\bash.exe`+`D:\WinNT` → `-s`；`D:\WinNT\System32\bash.exe`+空 SystemRoot → `-c`；`E:\Windows\System32\bash.exe`+`D:\WinNT` → `-c`） / **settles after the shell exits when a detached descendant retains inherited stdio**（`it.skipIf(platform !== "win32")`） / cleanup terminates active shell processes / streams combined stdout and stderr / decodes UTF-8 split across raw process chunks / reports a missing working directory before spawning / returns non-zero command exit codes as successful execution results / **maps signal-killed processes to a non-zero exit code**（`it.skipIf(win32)`） / returns timeout errors for commands exceeding the timeout / rejects invalid timeouts before spawning / returns callback errors from exec stream handlers / returns shell unavailable and spawn errors / returns an aborted result for pre-aborted and aborted commands / **ignores asynchronous taskkill spawn errors during abort**（`it.skipIf(win32)`） / does not create a spill before output crosses its thresholds / preserves exact raw bytes in the spill while streaming decoded text / reports the spill of a command that times out / fails rather than silently losing a requested spill / preserves complete large output in the spill / streams every line and spills them all once output crosses its line threshold。
- `applies string shell environment overrides when %s` 的语义：构造时 `shellEnv: { KIANA_SESSION_FILE: "/stale/parent.jsonl", KIANA_CODING_AGENT: "true", ... }`，每次 `exec` 传 `{ env: overrides }`；断言 `KIANA_CODING_AGENT` 与构造期其他键仍保留 → **`process.env` < `shellEnv` < `options.env` 三层合并，且空字符串会"遮蔽"而非回退**。
- `selects command transport ...` 的语义：**`isLegacyWslBashPath` 必须用 `process.env.SystemRoot` 判定**，且 `SystemRoot` 为空串或与路径盘符/目录不符时返回 `-c`；测试在非 Windows 上通过 `vi.stubEnv("SystemRoot", ...)` + `Object.defineProperty(process, "platform", { value: "win32" })` 模拟。
- `ignores asynchronous taskkill spawn errors during abort`：Windows 分支下 `taskkill` spawn 失败不得让 `exec` 变成非 aborted 错误。
- `maps signal-killed processes to a non-zero exit code`：验证 `128 + signal`（含 `os.constants.signals` 缺名时补 0）。

**env-truncate**（8 条）：reports UTF-8 byte counts in truncation results / counts UTF-8 bytes and truncates correctly in a runtime without Buffer / does not count a trailing newline as an extra line / truncates head by line limits / reports bytes when only a trailing newline exceeds limits at the line cap / truncates head on UTF-8 byte limits without partial lines / reports head truncation when the first line exceeds the byte limit / formats sizes。

**env-line-scan**（4 条）：StreamDecoder: keeps a U+FEFF that does not start the stream / decodes any chunking like decoding the whole stream；LineScanner: agrees with decoding and splitting the whole file / rejects empty or invalid ranges。

**env-node-conformance**（**3 条**）：refuses a tree over the directory budget and stops with an error when one grows past it（watch limits 组）；reads ranges spanning several internal chunks exactly / **refuses a FIFO without waiting for a writer**（`it.skipIf(windows)`，readers 组——验证 `O_NONBLOCK` 打开 FIFO 不会阻塞）。

**env-node-spill**（**1 条**）："keeps inherited stdio open past the exit grace period while a spill write is pending"（`it.skipIf(process.platform === "win32")`）。做法：`vi.mock("node:fs")` 把 `createWriteStream` 的 `highWaterMark` 改成 1、每次 `_write`/`_writev` 延迟 600 ms，并统计被拒绝的 `write()` 次数；断言 shell 退出后 `exec` **不**因 `EXIT_STDIO_GRACE_MS` 提前结算，spill 写完才返回，且输出按序完整。原 spec 写"0（describe 内动态 it）"是错的——文件里只有这一个 `it`。

### 7.4 编码工具

| 文件 | tests | describe |
|---|---|---|
| `tools.test.ts` ★ | **35**（durable tools 1 / read 7 / write 2 / edit 12 / powershell 4 / bash 9） | "durable tools" / "read" / "write" / "edit" / "powershell" / "bash" |
| `tools-images.test.ts` ★ | 27 | "read of images, without an image processor" / "read of an image that changes while it is read" / "read of images, with the Photon image processor" / "EXIF orientation" / "imageDimensions" / "toBase64" / "createNodePhotonImages" |
| `tools-read-differential.test.ts` ★ | 2 | "read tool"（差分测试：与整文件解码+slice+truncateHead 对比） |
| `tool-search.test.ts` ★ | 6 | "deferred tools and tool_search" |

**tools.test.ts 覆盖点**（**35 条**，逐组标题见下）：工具注册与描述；read 的文本/图片/offset/limit/截断诊断；write 的创建/覆盖/父目录/abort；edit 的精确/模糊/多编辑/重叠/重复/空 oldText/无变化/CRLF/BOM/legacy 参数/JSON 字符串 edits/单对象 edits/`firstChangedLine`/patch/diff 格式；powershell 的 program 回退；bash 的 exit code/timeout/abort/spill 诊断/structuredOutput。

**分组逐字标题（补全原 spec 漏项）**：
- `durable tools`（1）：fail with an ordinary error when no environment is configured
- `read`（7 条声明 / 8 个用例）：**`detects the complete %s signature`（`it.each(["GIF87a","GIF89a"])`，直接测 `detectSupportedImageMimeType`，不经工具）** / reads text with offsets and limits and reports continuation as a diagnostic / truncates large text by line count / does not count a trailing newline as an extra line at the truncation limit / shows the start of a line longer than the byte limit / rejects offsets beyond the file / reads a log that grows while it is read / returns images by content as one image block
- `write`（2）：writes files and creates parent directories / keeps the mutation queue locked until an aborted write settles
- `edit`（**12**）：applies disjoint edits and returns both diff formats / repairs edits sent as a JSON string, a single object, or top-level oldText/newText without mutating them / matches all edits against the original and rejects overlaps / rejects missing and duplicate target text / keeps the mutation queue locked until an aborted edit write settles / serializes concurrent edits through canonical and symlink paths / serializes edits of one file across environment objects of one file system / serializes a new file created through a symlinked directory with its canonical path / **keys a missing file whose name contains a backslash like the created file**（`it.skipIf(win32)`） / does not serialize the same path on different file systems / edits regular files through symlinks / preserves BOM and CRLF line endings
- `powershell`（4）：runs the command with pwsh as one argument, forcing UTF-8 output / falls back to Windows PowerShell and reports the last start failure / answers a nonzero exit with an error result carrying the exit code / **runs real PowerShell with UTF-8 output**（`it.runIf(process.platform === "win32")`；原 spec 未列）
- `bash`（**8**）：passes the retained window to the environment and forwards what it skipped / streams combined stdout and stderr, returns no output of its own, and repeats it with the exit code / answers a nonzero exit with an error result, and throws on a timeout, after streaming the output / reports the spill of a command that times out / prepares command, cwd, and an explicit environment with the call's api / supports command prefixes / streams every byte and spills complete output beyond the default limits / does not spill output within the limits。

**tools-images.test.ts** 覆盖点：无 processor 时的 bmp 拒绝/超尺寸/超字节/尺寸不可读；文件读取期间变化；Photon 的缩放/转码/方向；EXIF 1..8（JPEG 与 WebP）；`imageDimensions` 的 png/gif/webp(3 种)/jpeg；`toBase64` 与 `base64Length`；`createNodePhotonImages` 的 worker 与 fallback。

**tool-search.test.ts**：deferred 工具不被提供；`tool_search` 找到并 `addTools`；BM25 排序；limit 校验；空 query；描述文本。

### 7.5 images

| 文件 | tests | describe |
|---|---|---|
| `images-wasm-module.test.ts` | 1 | —（Photon WASM 模块加载） |

### 7.6 codemode

| 文件 | tests | describe |
|---|---|---|
| `codemode.test.ts` ★ | 41 | "durable code mode" / "durable code mode output and tool search" |
| `codemode-checkpoints.test.ts` ★ | 2 | "built-in document checkpoint policies" |
| `codemode-describe.test.ts` ★ | 5 | "code mode description and section" |
| `codemode-models.test.ts` ★ | 7 | "code mode models.*" |

**codemode.test.ts 的 41 条**（作为验收项，标题见 §2.28.5 的分叉/重放规则）：runs a script that calls tools as nested calls, and journals each result before delivering it / resumes a script after a restart without running its finished calls again / replays results in the order they arrived, not the order the calls were made / journals globals with their results and hands a replay the recorded ones / keeps the script's clock and randomness across a restart / ends the call when a resumed script made a different number of calls than before / ends the call at once when a resumed script waits without making a call it made before / ends the call when a resumed script calls a global with other arguments / admits no call of a diverging replay, even one the script never made before / ends the call when a resumed script ends where it went on before / ends the call when a resumed script ends before calls the live run made, even without a recorded count / ends the call when a resumed script makes a call an earlier attempt admitted with other arguments / records a count the live run never recorded as soon as a replay acknowledges it / lists the script's tool calls in its result's details / stores no outcome when the host closes the sandbox, and replays the script after a restart / aborts a call still running when the script ends / admits no call once the script has ended, even one it made before ending / checks a rerun's calls against an earlier attempt's before admitting any later call / ends the call when a rerun makes a global call where an earlier attempt made a tool call / ends the call when a rerun's first run makes fewer calls than an earlier attempt admitted / ends the call when a rerun without journal entries ends in its first run, where it went on before / checks every call a rerun holds before launching any of them / keeps a global's undefined result undefined, live and replayed / drops a global's result that arrives after the script finished / rejects a failed tool call with its error text and structured output / resolves a schema tool's error result that carries its value, and rejects one without with its text / resolves a schema-less tool with its text, as its declaration says, also when it returned images / delivers a result only after its journal commit / refuses to resume when the sandbox's fingerprint or the code mode version changed / times a script out across a restart, without admitting new calls / returns a decided outcome after a restart without running the script again / keeps store() values per conversation, as of a fork's entry / journals results that finish together in one commit；output/tool search 组：lays out text items, console lines, and the returned value under a header / cuts text past the token budget to its start and end, with the full text in a file / keeps a result inside the tool's output limits whatever max_output_tokens says, spilling the rest / writes the result's files to the same paths when it reruns after a crash / saves images to files and names them next to the image / calls a deferred tool the model is not offered, and finds it in ALL_TOOLS / reports a failed script with its error and the calls already made / finds tools with searchTools(), describeTool(), and describeNamespace(), and lists them in ALL_TOOLS。

**codemode-describe.test.ts**：keeps the tool description the same for every conversation / lists callable tools as declarations, grouped by namespace, with namespace descriptions / lists what fits the budget, every namespace before any is complete, and says what is left out / uses the longest matching namespace prefix / renders the section from the conversation's callable tools into the system prompt。

**codemode-models.test.ts**：reads the model catalog without credentials in headers / classifies through an internal tool: a nested call with its usage, hidden from tools and listings / resolves a model call that failed with its result, as the coding agent does / rejects a bad classifier context with the expected shape, before any request / runs at most four model calls at once / does not start a model call that waited for a slot after the script ended / notes generated images the script did not show。

**codemode-checkpoints.test.ts**：keeps task-scoped documents as deltas until their task retires / checkpoints the codemode store after 64 deltas without changing historical reads or forks。

### 7.7 辅助文件（移植测试时需要用）

| 文件 | 作用 |
|---|---|
| `test/harness-support.ts` | Harness 测试脚手架（外单元） |
| `test/session-support.ts` | Session 测试脚手架（内存 Storage、context、辅助提交） |
| `test/task-support.ts` | 任务定义辅助 |
| `test/storage-memory.ts` | `registerStorageConformance` 的共享调用 |
| `test/chat-support.ts` | 假模型/消息 |
| `test/setup-document-retention.ts` | **逐字语义**：读 `process.env.KIANA_DURABLE_DOCUMENT_RETENTION_MS`；若已定义则 `setDefaultDocumentRetention(Number(forced))`（`Number("")→0`、`Number("abc")→NaN`，后者按 retention 校验视为默认）。脚本注释提到 `npm run test:evicting`，但 `package.json` 里**没有这个脚本**——实际是 `test` 脚本的第二遍 `KIANA_DURABLE_DOCUMENT_RETENTION_MS=0 vitest --run`。**Rust 测试必须复现同一环境变量开关。** |
| `test-workers/`（4 个源文件 + 配置） | `storage.test.ts`、`wake.test.ts`（workerd 里的 Durable Object storage/alarms/eviction）、`worker.ts`（Worker 入口）、`cloudflare.d.ts`（Workers 类型）、`wrangler.jsonc`、`vitest.config.ts`（`cloudflareTest` 插件、`include: ["*.test.ts"]`、别名含 `@earendil-works/kiana-durable/storage/sqlite/cloudflare`）。主 `vitest.config.ts` 用 `exclude: [...configDefaults.exclude, "test-workers/**"]` 把它排除。 |
| `test/fixtures/delete-buffer.ts`（156 B） | 删除全局 `Buffer`，验证 `utf8ByteLength` 的无 Buffer 路径 |
| `test/fixtures/utf8-byte-length-without-buffer.ts`（349 B） | 同上，供 `env-truncate` 的"runtime without Buffer"用例 |
| `test/heap-retained.ts` | 内存保持断言 |
| `test/examples/00-conversation.ts` … `30-reload-and-restart.ts` | **31 个示例**（00–30；**目录里没有 README.md**，只有这 31 个 `.ts`）；Rust 侧作为文档测试或集成测试 |
| `test/*.bench.ts` | 基准（codemode-bench、document-append-bench 等） |
| `test-workers/**` | Cloudflare Workers 测试（`vitest.config.ts` 排除） |

### 7.8 移植优先级

1. **P0**：`env-truncate`、`env-line-scan`、`tools.test`、`tools-read-differential`、`storage-memory`、`jsonl-storage`、`sqlite-storage`、`sqlite-migrations`、`session-*`（全部）。
2. **P1**：`env-node`（含 spill）、`env-node-conformance`、`tools-images`、`tool-search`、`codemode-describe`、`codemode-checkpoints`。
3. **P2**：`codemode.test`（依赖 QuickJS 沙箱）、`codemode-models`、`indexeddb-storage`、`sqlite-facade`、`sqlite-cloudflare`、`storage-runtime-boundary`。
4. **P3**：bench、examples、workers。

---

## 8. Rust 模块布局与最难复刻之处

### 8.1 crate 与模块

建议两个 crate（保持"可移植核心"边界，对应 `storage-runtime-boundary.test.ts`）：

```
kiana-durable/                    (crate: kiana-durable)          # 可移植核心，无 Node API
  src/
    lib.rs                     # 公共导出面（对应 src/index.ts）
    ids.rs
    errors.rs
    types/
      mod.rs                   # JsonValue 别名、Id newtype、Page/Cursor/Query
      entries.rs
      documents.rs
      tasks.rs
      submissions.rs
      storage.rs               # StorageWrite / DocumentContent / StoredDocument
      session.rs               # Session / Tx / CommitOptions / CommitPersistence
    documents.rs               # define_doc / address_id / materialize
    entries.rs                 # define_entry + 6 个内置 entry
    tasks.rs
    timers.rs
    truncate.rs
    search.rs
    storage/
      mod.rs
      scan.rs
      memory.rs
      jsonl.rs                 # 可移植 JSONL（依赖 FileSystem trait）
      sqlite/
        mod.rs
        schema.rs
        migrations.rs
        storage.rs
      indexeddb.rs             # 仅 wasm 目标
    session/
      mod.rs
      session.rs
      transaction.rs
      forks.rs
    env/
      mod.rs                   # ExecutionEnv / FileSystem / Shell / Result / FileError
      decode.rs
      line_scan.rs
    tools/
      mod.rs
      env.rs
      path_utils.rs
      file_mutation_queue.rs
      image_processor.rs
      image.rs
      read.rs
      write.rs
      edit.rs
      edit_diff.rs
      bash.rs
    images/
      mod.rs                   # create_photon_images（基于 image crate）
      exif.rs
    tool_search.rs
    codemode/
      mod.rs
      codemode.rs
      describe.rs
      models.rs
      output.rs

kiana-durable-node/               (crate: kiana-durable-node)     # Node 等价物（Rust 原生）
  src/
    lib.rs
    env/
      node.rs                  # NodeExecutionEnv
      node_watch.rs
    storage/
      jsonl_node.rs            # open_node_jsonl_storage
      sqlite_node.rs           # open_node_sqlite_storage（rusqlite）
    images/
      node.rs                  # create_node_photon_images（spawn_blocking）
    tools/
      coding_tools.rs          # create_coding_tools / CodingTools
```

（若偏好单 crate，用 `#[cfg(feature = "node")]`；但 CI 必须能验证 `--no-default-features` 不含 Node 依赖。）

### 8.2 核心类型草图

```rust
// ids.rs
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Id<K: IdKind>(pub i64, PhantomData<fn() -> K>);
pub trait IdKind { const NAME: &'static str; }
pub struct ConversationKind; pub struct EntryKind; pub struct TaskKind; pub struct SubmissionKind; pub struct DocumentKind;
pub type ConversationId = Id<ConversationKind>;
// ...
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Seq(pub i64);
pub const ROOT_CONVERSATION_ID: ConversationId = ConversationId(1, PhantomData);

pub fn id_from_number<K: IdKind>(v: i64) -> Id<K> { Id(v, PhantomData) }
pub fn seq_from_number(v: i64) -> Seq { Seq(v) }
```

```rust
// types/storage.rs
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum StorageWrite {
    Conversation { value: ConversationRecord },
    Entry { value: EntryRecord },
    Task { value: TaskRecord },
    Submission { value: SubmissionRecord },
    #[serde(rename = "document.create")]
    DocumentCreate { record: DocumentCreate, content: DocumentContent },
    #[serde(rename = "document.copy")]
    DocumentCopy { record: DocumentCreate, source: DocumentCopySource },
    #[serde(rename = "document.change")]
    DocumentChange { id: DocumentId, content: DocumentContent },
    #[serde(rename = "document.retire")]
    DocumentRetire { id: DocumentId },
}
```
**注意**：`#[serde(tag = "type")]` 会把 tag 放在**最前**，与 JS 对象字面量 `{ type: ..., value: ... }` 一致（Rust 序列化顺序：tag 先，字段按声明序）。JSONL 的 marker 需要**不同形状**（`task.sidecar`/`document.create` 带 `ordinal`），因此 `MainOperation` 要单独定义，不要复用 `StorageWrite` 的序列化。

```rust
// env/mod.rs
pub enum Result<T, E> { Ok(T), Err(E) }          // 或用标准 Result + 自定义 Err
pub type FsResult<T> = std::result::Result<T, FileError>;

#[derive(Debug, Clone, Serialize)]
pub struct FileError { pub code: FileErrorCode, pub message: String, pub path: Option<String> }

#[async_trait]
pub trait FileSystem: Send + Sync {
    fn id(&self) -> &str;
    fn cwd(&self) -> &str;
    async fn absolute_path(&self, path: &str, cx: &Context) -> FsResult<String>;
    // ...
}
```
**决策点**：TS 用 `Result<T,E>` 表示期望失败（不抛异常）。Rust 可用 `Result<T, FileError>`（推荐，简洁），但**必须**保留"不 panic、不以异常传播"的语义，且 `getOrThrow` 等价物为 `?`。

```rust
// session/session.rs
pub struct SessionImpl<S: Storage> {
    storage: GuardedStorage<S>,
    documents: IndexMap<String, LoadedDocument>,
    idle_documents: IndexMap<String, f64>,
    attached_documents: HashMap<DocumentId, u32>,
    holds: IndexMap<DocumentId, Hold>,
    // ...
    tail: Mutex<Option<JoinHandle<()>>>,   // 或 mpsc 队列
}
```
**串行线**推荐实现：
```rust
struct MutationLine { tx: mpsc::UnboundedSender<Job> }
struct Job { fut: BoxFuture<'static, ()>, }
// 单消费者任务：while let Some(job) = rx.recv().await { job.fut.await; }
```
更简单的做法：`tokio::sync::Mutex<()>` 无法保证 FIFO（`tokio::sync::Mutex` 是公平的，FIFO —— 可以用），但无法表达"前一个失败不影响下一个"（用 guard 的 drop + `catch_unwind`/手动忽略错误即可）。**推荐** `tokio::sync::Mutex<()>` 公平锁 + 每个操作内 `let _guard = line.lock().await;`，因为它是 FIFO 且不需要额外任务。但注意：`#enqueue` 的语义是"同步取得位置"，`Mutex::lock()` 也是公平排队的（tokio 文档保证 FIFO）。**要求**：`commit()` 必须**同步**返回一个已排队的 future（不能等到被调度才排队），否则并发提交顺序可能错乱。可用 `tokio::sync::Mutex::lock_owned()` 在调用时立即创建。

### 8.3 文本表示策略（关键决策）

**选项 A**：全程 `String`（UTF-8）+ 需要 JS 语义处用 `Utf16Index` 适配。
**选项 B**：参与偏移计算的模块（`edit_diff`、`codemode/output`、`codemode/describe`、`tools/read` 的部分）内部用 `Utf16String`。

推荐 **B（局部）+ A（其余）**：
```rust
pub struct Utf16Str { units: Vec<u16> }
impl Utf16Str {
    pub fn from_str(s: &str) -> Self;          // 正确编码代理对
    pub fn as_str(&self) -> Cow<str>;          // 仅在无代理对时可借用
    pub fn len(&self) -> usize;                // = JS string.length
    pub fn slice(&self, start: usize, end: usize) -> Utf16Str;
    pub fn index_of(&self, needle: &Utf16Str) -> Option<usize>;
}
```
- `normalize("NFKC"/"NFD")` 需要先在 UTF-8 上做（`unicode-normalization`），再转回 UTF-16（长度会变）。
- `charCodeAt` 在 `startsWithAscii` 等处只用于 ASCII，安全。

### 8.4 JSON 序列化的三个层次

1. **磁盘/线格式**（JSONL、SQLite `record` 列、`details` 等）：必须与 `JSON.stringify` **逐字节**一致。
   - `serde_json` + `preserve_order`；
   - 结构体字段顺序 = 声明顺序；`#[serde(rename_all = "camelCase")]`；
   - `Option<T>` 用 `skip_serializing_if = "Option::is_none"`（JS 省略 `undefined`）；
   - 数字：整数用 `Number::from(i64)`；浮点只在必要时出现（`mtimeMs` 不进入 JSON；`wallMs` 的 `toFixed(1)` 是字符串）。
   - **测试**：为每个 `StorageWrite` 变体、每个 `MainMarker`、每个 `SidecarRecord` 写"golden JSON"测试（用 TS 侧输出作为 fixture）。
2. **任意 `JsonValue` 透传**（Entry/Task 记录内的 `model`/`data`）：`serde_json::Value` + `preserve_order`，不要重新排序。
3. **`copyJson(value, { omitUndefinedProperties: true })`**：`Value` 天然没有 `undefined`；若内部用 `Option<Value>` 表示"可能 undefined 的字段"，则据此跳过。

### 8.5 错误模型

```rust
#[derive(Debug, thiserror::Error)]
pub enum DurableError {
    #[error("Tx.{0}() cannot read tables after the first table write")]
    ReadAfterWrite(&'static str),
    #[error("{0}")]
    StorageRequest(String),
    #[error("Session failed after a storage error; close and reopen it")]
    SessionFailed { #[source] cause: Box<dyn Error + Send + Sync> },
    #[error("Read-only Harness: {0} refused")]
    ReadOnly(&'static str),
    #[error("Conversation {0} is busy")]
    ConversationBusy(ConversationId),
    #[error("{0}")]
    Message(String),
}
```
**要求**：所有 `Message` 变体的文本必须逐字一致；测试直接比对 `to_string()`。**不要**把 `source` 拼进 `Display`（TS 只用 `error.message`）。

`SessionFailed` 的 `cause` 需要保留（TS 的 `cause`），但 `Display` 只输出固定消息。

### 8.6 并发模型

- **Session 线**：单一 FIFO。所有 `commit`/`snapshot`/`watchDoc`/`load`/`sweep` 都排队。
- **Storage 的"underway"集合**：`GuardedStorage` 需要记录所有在途调用（不在线上），close 等它们 `allSettled`。Rust 用 `tokio::sync::Semaphore` 或 `Arc<AtomicUsize>` + `Notify`，或用 `JoinSet`。
- **失败闩锁**：`AtomicBool` + `OnceLock<Box<dyn Error>>`；`#assertHealthy()` 每次检查。
- **adopt 同步性**：`Storage::commit(writes, adopt, cx)` 的 `adopt` 必须是**同步闭包**（`FnOnce(Seq)`），在持久成功后、返回前调用。Rust 中：
  ```rust
  async fn commit(&self, writes: &[StorageWrite], adopt: &mut dyn FnMut(Seq), cx: &Context) -> Result<Seq, Error>;
  ```
  因为 adopt 需要可变借用事务状态（TS 中 `tx.adopt(seq)` 会改 tx），Rust 必须让 `adopt` 捕获 `&mut Transaction`。**这在 async 中会导致借用冲突**（`Transaction` 在 `commit` 的 await 期间被可变借用，而 `commit` 也可能需要 `&self` 的事务状态）。
  **解决方案**：使用 `RefCell`/`Mutex` 包裹 `Transaction` 的内部状态，adopt 闭包捕获 `Arc<Mutex<TxState>>`；或让 `Storage::commit` 返回 seq，由调用方在返回后自行 adopt（但这就破坏了"Storage 在持久后立刻 adopt、且下一次 commit 前"的语义——对 Memory 无影响，对并发 Storage 有影响）。
  **推荐**：`adopt: Box<dyn FnOnce(Seq) + Send>`，捕获 `Arc<Mutex<TxInner>>`，在闭包内加锁并 adopt。这保持语义且不冲突。

### 8.7 计时器与"不阻止退出"

```rust
pub trait Timers {
    fn foreground(&self, cb: Box<dyn FnOnce() + Send>, delay_ms: f64) -> Box<dyn FnOnce()>;  // 必须触发
    fn background(&self, cb: Box<dyn FnOnce() + Send>, delay_ms: f64) -> Option<Box<dyn FnOnce()>>;
}
```
实现：
- `foreground`：`tokio::spawn(async move { sleep(delay).await; cb() })`；返回 abort handle。
- `background`：若当前是在 `Runtime` 的 idle 期，用 `tokio::spawn` 但**不**让 runtime 等待——`Runtime::shutdown_background` 不等待；或者用一个独立的 `std::thread` + `park_timeout`（保证不阻止主进程）。**推荐**：与 Tokio runtime 配合时，后台计时器只是 `spawn`，主程序退出时 `Runtime` drop 会取消它（`shutdown_background` 或 `shutdown_timeout(0)`）。
- Cloudflare Workers 的等价物：`background` 返回 `None`。

### 8.8 文件监视

```rust
pub struct NodeFileWatcher { mode: WatchMode, /* notify watcher / 轮询句柄 */ }
```
- `notify::RecommendedWatcher` + `notify::Config::default()`；
- 去抖：`tokio::time::sleep(50ms)` 后合并；
- darwin settle：额外 500ms；
- 快照 diff：`HashMap<PathBuf, Entry>`，`Entry { kind, size, mtime_ns, hash: Option<[u8;32]> }`；
- 目录预算：扫描时计数；
- `statfs` 检测不可靠 FS（`nix::sys::statfs`）；
- Windows → 强制 polling。

### 8.9 最难复刻之处（Hard Parts）

1. **`edit-diff` 的模糊匹配与偏移语义**
   - 所有偏移是 UTF-16 code unit；NFKC 规范化会改变长度；`applyReplacementsPreservingUnchangedLines` 的"行分组 + 原文保留"算法必须逐分支一致。
   - 错误文案 9 条必须逐字。
   - `normalizeForFuzzyMatch` 的 6 步顺序不可交换（NFKC 在前，trimEnd 在其次）。

2. **`generateDiffString` 的上下文行折叠**
   - jsdiff `diffLines` 的分组方式（相同/新增/删除块）决定 `parts` 序列；`hasLeadingChange`/`hasTrailingChange` 的组合共 4 种；` ...` 跳过的行数计算必须一致。
   - `lineNumWidth` 基于 `max(oldLines.length, newLines.length)`；`firstChangedLine` 是**新文件**行号。
   - 建议：直接移植 jsdiff 的 `diffLines`（Myers 或 jsdiff 默认的 `diffLines` 实现）而不是换算法。

3. **`generateUnifiedPatch` 的 jsdiff 输出**
   - `@@ -a,b +c,d @@` 中 `b`/`d` 为 1 时 jsdiff 会省略 `,1`（`--` 需要核对）；上下文行数 4；`\ No newline at end of file` 的处理；空文件情况（`@@ -0,0 +1,N @@`）。
   - 必须写"golden patch"测试。

4. **JSONL 恢复的确认/回收矩阵**
   - 23 条测试覆盖 append/flush/reclaim 的失败注入点（`failure.mode` × `failure.call`）。Rust 侧需要一个可注入失败的 `FileSystem` 包装器（测试用），并精确复刻：
     - 何时 poison（append/flush 失败 → poison；reclaim 失败 → 静默）；
     - `fsync` 时 main 的 flush 时机（仅回收前）；
     - 尾部截断的字节偏移（`lastIndexOf(0x0a) + 1`）；
     - `confirmed` 集合与 `optional` 参数。
   - 这是**最容易出错**的部分之一。

5. **`LineScanner` 的字节级状态机**
   - BOM 的 3 字节头部缓冲、`#feed` 的 BOM 跳过、`#begin`/`#endFirstLine`/`#endSelection` 的时序、`finish()` 的兜底。
   - `env-line-scan.test.ts` 会以"任意分块 == 整块解码"做差分；Rust 测试应做同样的随机分块属性测试。

6. **`read` 工具的 offset/limit 语义**
   - 模拟 `Array.prototype.slice(startLine, endLine)`，包括：负数、NaN、小数（`Math.trunc`）、`Number.isSafeInteger` 检查、`limit` 为负、`offset` 超出、空选择。
   - `truncateHeadOf` 的 `truncatedBy` 二次判定（末尾换行导致字节超限时报告 `"bytes"`）。
   - `firstLineExceedsLimit` 分支的 `characterEnd(lineBytes, 51200)`（在 UTF-8 字符边界截断）与 `sed` 提示文本。

7. **quickjs 的确定性重放**
   - journal 顺序 = **结果到达顺序**（不是调用发出顺序）；同一提交中完成的结果合并；
   - `firstIssued` vs `durableIssued`：结果只在 journal 提交后投递；
   - 12 条分叉规则；
   - 时钟与随机的注入与跨重启保持；
   - `store()` 的 fork asOf 语义（依赖 `document.copy` 与 `snapshotAsOf`）。
   - Rust + rquickjs 需要复刻 `@earendil-works/kiana-codemode` 的沙箱协议（`start`/`deliver`/`onCall`/`onIdle`/`acks`），这是跨单元强耦合点。

8. **Photon 图像处理的像素级等价**
   - resize（Lanczos3）在不同实现间会有细微差异 → 编码后的字节不同 → `base64Length` 判定可能跨越阈值。
   - **风险**：`prepare()` 的"是否直接原样返回"与"何时缩到 3/4"取决于编码长度；如果 `image` crate 的 PNG/JPEG 编码器输出大小与 Photon 不同，行为会在边界处分歧。
   - **缓解**：测试只断言"结果满足 limits 且 mimeType/尺寸符合规则"，不断言字节；或固定一个与 Photon 等价的编码器参数集。

9. **`toFixed(1)` / `Math.ceil` / `Math.round` / `Math.floor` 的精确行为**
   - JS `(x).toFixed(1)` 使用"四舍六入五成双"？实际是**最近值，平局远离零**（但受二进制表示影响）；Rust `format!("{:.1}", x)` 是 round-half-to-even。
   - 受影响的输出：`formatSize`（`1.5KB` 等）、`Wall time X.X seconds`、`Multiply coordinates by S`。
   - **建议**：实现一个 `js_to_fixed(x, 1)`，按 JS 规范（ECMA-262 `Number.prototype.toFixed`）实现：先取精确十进制表示（`x` 的二进制值 → 精确十进制），再做"四舍五入，平局取较大绝对值"，最后格式化。可用 `rust_decimal`/`bigdecimal` 或直接移植 V8 的 double-to-string。

10. **`JSON.stringify` 的浮点格式**
    - JS 的 `Number::toString`（最短往返表示，如 `0.1`、`1e+21`、`1e-7`）与 Rust 的 `Display for f64` 不同（Rust 也做最短往返，但指数格式阈值不同：JS 在 `>=1e21` 或 `<1e-6` 用指数）。
    - 本单元的值多为整数，风险低；但 `EntryRecord` 中若含模型返回的浮点，必须一致。**建议**：实现 `js_number_to_string(f64)`（移植 ECMAScript Number::toString）。

11. **`session` 的 held/deferred 状态机**
    - hold 的 deadline 取最小值、pin/unpin 计数、溢出（`MAX_PENDING_OPS`）、`base` 标志合并、`takeHeld` 在"只剩 Storage 可能失败"时取出、close 时先存 held 再关 Storage、失败后不存。
    - 25 条测试逐条覆盖，必须全绿。

12. **`GuardedStorage` 的失败语义**
    - `StorageRequestError` 与被取消的读**不**判死；其他错误判死；在途调用即便成功也以 `SessionFailed` 结束；close 等全部在途；失败立即触发同步 close 监听器。
    - Rust 需要注意：**在途调用的结果必须被丢弃**，即使它成功（`#call` 成功后再 `#assertHealthy()`）。

13. **`adopt` 与"内存不落后于磁盘"**
    - `adopt` 必须在 Storage resolve 前同步执行；Session 在 adopt 抛错时 `fail`；Storage 已提交但 adopt 失败 = 不可恢复。
    - Rust 的 `FnOnce(Seq)` 闭包与借用检查的交互（见 §8.6）。

14. **`Bash` 工具的 spill**
    - 原始字节完整落盘、按到达顺序、`afterBytes`/`afterLines` 阈值（"或"关系）、超时/中止时仍报告 `spillPath`、spill 失败必须报错而不是静默。
    - 18 条 shell 测试 + spill 测试覆盖。

15. **`NodeFileWatcher` 的 budget/polling/native 切换**
    - 目录预算超限 → `{ error }` 并停止（不是抛错）；
    - 不可靠 FS 检测；
    - `overflow` 投递后全量重扫；
    - darwin settle。

16. **`Sweep` 与 `touch` 的 granule**
    - `min(1000, retention / 10)`；`retention = 0` → `queueMicrotask`；没有 unref 计时器 → 提交结束时清扫；只排队一个 sweep job。

17. **`applySubmissionChange` 与会话/条目/任务表的交互**
    - `settleSubmission`/`placeSubmission` 是**同步**的，且解析"本事务最新记录"（含本事务刚写入的）。
    - `Request ${draft.requestId} already identifies a submission of type ${existing.type}`。

18. **`Document` 的地址与化身模型**
    - `findDocument(address, at)` 返回"该地址在该点的化身"；`document(id, at)` 按 ID 读特定化身（不跟随地址替换）。
    - `checkDocumentActions` 的 `liveCounts` 算法（同批多个 create/retire 的净效果）。
    - current-only 的"base 替换全部 revision""退休删除 revision"。

19. **`sqlite` 的 `encodeIndexedString`**
    - `kind`/`request_id`/`key_value` 以带引号的 JSON 字符串入库；查询参数也必须同样编码。若 Rust 直接存裸字符串，`WHERE kind = ?` 将永不匹配 → 静默错误。
    - 孤立的 UTF-16 代理（lone surrogate）：JS 的 `JSON.stringify` 会输出 `"\ud800"`；Rust 的 `String` 无法表示 → 需要在 `serde_json::Value` 中用 `\u{FFFD}`？**不行**。**决策**：Rust 侧字符串必须是合法 UTF-8；若上游（模型/用户）产生孤立代理，必须在进入存储前拒绝或替换为 U+FFFD，并记录差异。这是 JS↔Rust 的固有差异，需在测试中明确期望。

20. **`JSONL` 的 `ordinal` 全局递增**
    - 跨 sidecar 文件共享计数器；同一文件内多条记录（一次提交写同一文档多次）按写入顺序。

### 8.10 验收方式

- `cargo test -p kiana-durable` 覆盖 §7 的 P0/P1；`cargo test -p kiana-durable-node` 覆盖 P2 的 node 部分。
- **差分测试**：用 TS 实现产生 golden 文件（JSONL 目录、SQLite 文件、patch/diff 文本、错误消息、工具输出），Rust 测试读取并逐字节比对。建议建立 `kiana/spec/golden/` 目录（由 TS 侧脚本生成，**只读**）。
- **属性测试**：`LineScanner`、`truncateHead`、`readText` 与"整文件解码 + split + slice"的等价性（proptest）。
- **失败注入**：`FileSystem` 的测试包装器可在指定调用序号抛错（对应 `jsonl-storage.test.ts` 的参数化矩阵）。
- **边界测试**：UTF-8 多字节跨块、CRLF、BOM、空文件、超长行、`>2^31` 字节数、非法 UTF-8。

### 8.11 明确的非目标（Out of Scope）

- 浏览器 IndexedDB 后端（除非 wasm 是硬需求）。
- Cloudflare Workers 的 `CompiledWasm` 与 Durable Objects（可用 `worker` crate 实现，但优先级最低）。
- `bench` 与 `examples` 的性能基准（功能正确后再说）。
- 跨进程锁（TS 明确"一次一个进程拥有 Storage"）。
- 主日志压缩（TS 初始实现不压缩 `main.jsonl`）。
