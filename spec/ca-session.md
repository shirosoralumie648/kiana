# ca-session 单元移植规格（TypeScript -> Rust）

本文件是 `packages/coding-agent/src/core/` 中 **ca-session** 单元的完整移植规格。源仓库
`/home/ctyun/code/kiana_rev/kiana` 为只读参考；Rust 实现位于 `/home/ctyun/code/kiana_rev/kiana`。

- 源包：`/home/ctyun/code/kiana_rev/kiana/packages/coding-agent`，名称 `@earendil-works/kiana-coding-agent`，
  版本 `1.4.0`，ESM，`"type": "module"`，`engines.node >= 22.19.0`。
- 本单元文件范围（全部逐行读过；第 9 节「Audit additions」补充了首轮漏读的 `src/config.ts`、
  `src/utils/paths.ts`、`src/core/auth-guidance.ts`、`src/core/settings-defaults.ts`、
  以及 `test/suite/regressions/**` 与 `docs/**`）：
  `src/core/agent-session.ts`（4458 行）、`src/core/session-manager.ts`（2018 行）、
  `src/core/agent-session-runtime.ts`（447 行）、`src/core/agent-session-services.ts`（233 行）、
  `src/core/messages.ts`（196 行）、`src/core/session-cwd.ts`（59 行）、
  `src/core/session-export.ts`（45 行）、`src/core/nested-tool-calls.ts`（263 行）、
  `src/core/bash-executor.ts`（146 行）、`src/core/exec.ts`（116 行）、
  `src/core/output-guard.ts`（108 行）、`src/core/event-bus.ts`（33 行）、`src/core/index.ts`（97 行）。
- 相关文档：`docs/session-format.md`、`docs/sessions.md`、`docs/message-types.md`、`docs/how-kiana-works.md`。
- **不在本单元范围但被本单元直接调用**：`core/extensions/**`、`core/compaction/**`、`core/model-runtime.ts`、
  `core/settings-manager.ts`、`core/resource-loader.ts`、`core/tools/**`、`core/system-prompt.ts`、
  `core/export-html/**`、`core/bug-report.ts`、`core/cache-warmer.ts`、`core/virtual-models.ts`、
  `core/mcp-servers.ts`、`core/source-info.ts`、`core/usage-totals.ts`、`core/prompt-templates.ts`、
  `core/sdk.ts`、`core/auth-guidance.ts`、`core/defaults.ts`、`src/config.ts`、`src/utils/**`、
  `packages/agent`（`@earendil-works/kiana-agent-core`）、`packages/ai`（`@earendil-works/kiana-ai`）。
  它们的**接口契约**在本文件第 5 节逐字记录；实现由对应单元负责。
- 本文件同时是 (a) Rust 移植的验收清单（逐条可测），(b) Rust 侧设计说明。

## 目录

1. 概览
2. 逐文件规格（导出符号 + 行为）
3. 用户可见表面（逐字文本）
4. 外部格式（磁盘 / 环境变量 / 协议）
5. 跨单元接口
6. npm 依赖与 Rust crate 映射
7. 测试清单
8. Rust 模块布局与最难复现的部分

---

## 1. 概览

### 1.1 目的

本单元是 kiana coding-agent 的**会话内核**：它把 `packages/agent` 的通用 agent loop 与
「可持久化的会话树 + 扩展系统 + 压缩 / 重试 / 队列 / 模型管理」缝合在一起，向上为
interactive、print、rpc、SDK 四种运行模式提供统一 API。

它负责：

1. `AgentSession`：一次运行会话的门面。持有 `Agent`、`SessionManager`、`SettingsManager`、
   `ModelRuntime`、`ResourceLoader`，安装 agent 钩子（tool hooks、请求投影、边界钩子、system prompt 投影），
   管理消息队列（steering / followUp / nextTurn / pending custom / pending bash）、
   自动重试、自动 / 手动压缩、bash 执行、模型与 thinking 切换、工具注册表与 loadout、
   扩展绑定、树导航、统计与导出。
2. `SessionManager`：JSONL 会话树文件的读写、迁移、索引、投影（projection）、分支、fork、
   会话发现（list / find / continue recent）与会话信息聚合。
3. `AgentSessionRuntime`：会话替换生命周期（new / switch / fork / import / dispose），
   先拆旧再建新，并保证扩展事件顺序。
4. `AgentSessionServices`：按 cwd 创建一套一致的服务（ModelRuntime、SettingsManager、
   ResourceLoader、diagnostics），并注册扩展携带的 provider / virtual model / flag。
5. 辅助设施：`messages.ts`（coding-agent 专有 message 与 `convertToLlm`）、`session-cwd.ts`
   （会话 cwd 缺失诊断）、`session-export.ts`（JSONL 导出）、`nested-tool-calls.ts`
   （工具内再调用工具的记录与串行化）、`bash-executor.ts`（流式 bash 输出管道）、
   `exec.ts`（一次性子进程执行）、`output-guard.ts`（stdout 接管与原始写）、`event-bus.ts`。

### 1.2 在 monorepo 中的位置

- 依赖 `@earendil-works/kiana-agent-core`（Agent 类、AgentEvent、runToolCall、createAllToolDefinitions 等）
  与 `@earendil-works/kiana-ai`（消息类型、usage、重试分类器、溢出检测、模型工具函数、uuidv7）。
- 被 `packages/coding-agent` 的 `modes/*`（interactive / print / rpc）、`core/sdk.ts`、
  `core/export-html`、`core/bug-report.ts` 等大量使用。
- `core/index.ts` 是 core 的公共再导出入口（见 2.13）。

### 1.3 关键不变量（必须在 Rust 中逐条成立）

1. **会话文件是 append-only 树**：条目一旦写入不会被修改或删除；分支只移动 leaf 指针；
   迁移是唯一例外（加载时原地重写整个文件）。
2. **只有在会话包含 user 或 assistant 消息时才创建文件**：仅有 setup 条目（model / thinking /
   system prompt）时保持内存态，退出后不留文件。延迟落盘用排他创建（`wx` / `O_EXCL`）。
3. **leaf 指针决定活动分支**：`getBranch()` 从 leaf 回溯到 root；`buildContextEntries()` 在
   分支上应用**最新一次 compaction** 的裁剪规则。
4. **模型可见上下文由 projection 决定**：`buildSessionProjection()` 返回 `sourceEntry -> messages`
   的对应关系；`context_edit` 只影响模型上下文，不改原始条目、UI、导出与统计。
5. **每个 `message_end` 都会持久化**（除 bashExecution / compactionSummary / branchSummary，
   它们由各自路径写入）；扩展的 `message_end` 可返回替换消息，替换必须**原地**进行以驱动后续
   所有消费者。
6. **事件顺序固定**：先发扩展监听器（`ExtensionRunner.emit*`），再发 `AgentSession` 公共监听器；
   扩展事件在公共事件之前（测试 `emits extension events before public event subscribers` 固定此顺序）。
7. **压缩、重试、分支摘要互斥且可中止**：`isCompacting` 只要任一 AbortController 存在即为 true；
   `isIdle = !isStreaming && !isCompacting`。
8. **队列语义**：steering 在「当前 turn 的工具调用执行完之后、下一次 LLM 请求之前」投递；
   followUp 在「agent 本来要停下来时」投递；`one-at-a-time` 每次取一条，`all` 一次取全部。
9. **溢出恢复只做一次**：`_overflowRecoveryAttempted` 置位后若再次溢出，只发事件不重试；
   成功（非 error / 非 length 的 assistant）或新的 user 消息开始会重置该标志。
10. **自动重试有界**：`maxRetries` 次，指数退避 `baseDelayMs * 2^(attempt-1)`，被
    `maxAgentDelayMs`（默认 60000）截断；重试前把失败尝试与其 tool result 用 `context_edit:null`
    从模型上下文中持久省略（原文仍保留在历史里）。
11. **手动压缩永不重试、永不继续被中断的 turn**：`compact()` 先 `abort()`，失败抛出。
12. **工具声明与可执行实现分离**：`agent.state.tools` 是「声明给模型」的工具；`context.tools`
    与 `_toolRegistry` 是「可执行实现」；`_getCallableTools()` 决定 `ctx.executeTool()` 可达集合。
13. **嵌套工具调用不持久化**：其记录与 usage 汇总到发起调用的 tool result 消息上；
    其事件带 `parentToolCallId`。
14. **JSON 键顺序**：写入磁盘的条目依赖对象字面量构造顺序（`JSON.stringify` 保序）；Rust 侧必须
    用 `serde_json` 的 `preserve_order`（IndexMap）保持同样的插入顺序，否则字节级输出不同。

### 1.4 术语

- **entry**：会话树的一个节点，必有 `id` / `parentId` / `timestamp`（ISO 8601 字符串）。
- **leaf**：当前活动节点 id（可为 null，表示「第一个条目之前」）。
- **branch**：从 root 到 leaf 的 entry 列表。
- **projection**：把 branch 投影成模型可见消息，并保留来源 entry 的映射。
- **loadout**：当前活动工具集合（声明 + 可调用 + 已注册）。
- **boundary**：`turn_end` 与 `agent_before_settle` 两个扩展边界，可追加草稿条目并请求继续。

---

## 2. 逐文件规格

### 2.1 `src/core/messages.ts`

#### 2.1.1 常量（逐字）

```ts
export const COMPACTION_SUMMARY_PREFIX = `The conversation history before this point was compacted into the following summary:

<summary>
`;
export const COMPACTION_SUMMARY_SUFFIX = `
</summary>`;
export const BRANCH_SUMMARY_PREFIX = `The following is a summary of a branch that this conversation came back from:

<summary>
`;
export const BRANCH_SUMMARY_SUFFIX = `</summary>`;
```

即：

- `COMPACTION_SUMMARY_PREFIX` = `"The conversation history before this point was compacted into the following summary:\n\n<summary>\n"`（结尾有换行）。
- `COMPACTION_SUMMARY_SUFFIX` = `"\n</summary>"`（开头有换行）。
- `BRANCH_SUMMARY_PREFIX` = `"The following is a summary of a branch that this conversation came back from:\n\n<summary>\n"`。
- `BRANCH_SUMMARY_SUFFIX` = `"</summary>"`（**没有**前导换行）。

#### 2.1.2 类型

```ts
export interface BashExecutionMessage {
  role: "bashExecution";
  command: string;
  output: string;
  exitCode: number | undefined;
  cancelled: boolean;
  truncated: boolean;
  fullOutputPath?: string;
  timestamp: number;              // Unix ms
  excludeFromContext?: boolean;   // !! 前缀时为 true
}

export interface CustomMessage<T = unknown> {
  role: "custom";
  customType: string;
  content: string | (TextContent | ImageContent)[];
  display: boolean;
  details?: T;
  timestamp: number;
}

export interface BranchSummaryMessage {
  role: "branchSummary";
  summary: string;
  fromId: string | null;
  timestamp: number;
}

export interface CompactionSummaryMessage {
  role: "compactionSummary";
  summary: string;
  tokensBefore: number;
  timestamp: number;
}
```

并通过 declaration merging 扩展 `kiana-agent-core` 的 `CustomAgentMessages`，加入
`bashExecution` / `custom` / `branchSummary` / `compactionSummary` 四种 role。
Rust 中应把 `AgentMessage` 定义为带这四种 coding-agent 变体的枚举（见第 8 节）。

#### 2.1.3 `bashExecutionToText(msg): string`

逐字拼接：

```ts
let text = `Ran \`${msg.command}\`\n`;
if (msg.output) {
  text += "```\n" + msg.output + "\n```";
} else {
  text += "(no output)";
}
if (msg.cancelled) {
  text += "\n\n(command cancelled)";
} else if (msg.exitCode !== null && msg.exitCode !== undefined && msg.exitCode !== 0) {
  text += `\n\nCommand exited with code ${msg.exitCode}`;
}
if (msg.truncated && msg.fullOutputPath) {
  text += `\n\n[Output truncated. Full output: ${msg.fullOutputPath}]`;
}
return text;
```

注意：`exitCode === 0` 不追加；`exitCode` 为 undefined（被取消或未取得）也不追加；
`cancelled` 与 `exitCode` 是互斥分支（先判 cancelled）；`truncated` 但无 `fullOutputPath` 时不追加。

#### 2.1.4 构造函数

- `createBranchSummaryMessage(summary, fromId, timestamp: string): BranchSummaryMessage`
  → `{ role: "branchSummary", summary, fromId, timestamp: new Date(timestamp).getTime() }`。
  `timestamp` 是 ISO 字符串，被解析为 Unix ms（无效日期为 `NaN`，Rust 需用 f64 或 Option<i64> 语义对齐）。
- `createCompactionSummaryMessage(summary, tokensBefore, timestamp: string)` → 同上，role 为
  `compactionSummary`。
- `createCustomMessage(customType, content, display, details, timestamp: string)` → role `custom`，
  `timestamp` 同上。

#### 2.1.5 `convertToLlm(messages: AgentMessage[]): Message[]`

按顺序映射，undefined 结果被过滤：

| 输入 role | 输出 |
|---|---|
| `bashExecution` | 若 `excludeFromContext` 为真 → 丢弃；否则 `{role:"user", content:[{type:"text",text:bashExecutionToText(m)}], timestamp:m.timestamp}` |
| `custom` | `{role:"user", content: typeof m.content==="string" ? [{type:"text",text:m.content}] : m.content, timestamp:m.timestamp}` |
| `branchSummary` | `{role:"user", content:[{type:"text",text: BRANCH_SUMMARY_PREFIX + m.summary + BRANCH_SUMMARY_SUFFIX}], timestamp}` |
| `compactionSummary` | `{role:"user", content:[{type:"text",text: COMPACTION_SUMMARY_PREFIX + m.summary + COMPACTION_SUMMARY_SUFFIX}], timestamp}` |
| `system` / `user` / `assistant` / `toolResult` | 原样返回 |
| 其它（穷尽检查） | undefined（丢弃） |

`custom` 里的图片块保留原样（多模态 user message）。

### 2.2 `src/core/session-manager.ts`

#### 2.2.1 常量与导出清单

- `export const CURRENT_SESSION_VERSION = 3;`
- `assertValidSessionId(id: string): void`
- `migrateSessionEntries(entries: FileEntry[]): void`（供测试导出）
- `parseSessionEntries(content: string): FileEntry[]`（供 compaction 测试导出）
- `getLatestCompactionEntry(entries: SessionEntry[]): CompactionEntry | null`
- `sessionEntryToContextMessages(entry: SessionEntry): AgentMessage[]`
- `buildContextEntries(entries, leafId?, byId?): SessionEntry[]`
- `buildSessionProjection(entries, leafId?, byId?): SessionProjection`
- `buildSessionContext(entries, leafId?, byId?): SessionContext`
- `getDefaultSessionDir(cwd: string, agentDir?: string): string`
- `loadEntriesFromFile(filePath: string): FileEntry[]`
- `findMostRecentSession(sessionDir: string, cwd?: string): string | null`
- `class SessionManager`（静态：`create` / `open` / `continueRecent` / `inMemory` / `forkFrom` / `findById` / `list` / `listAll`）
- 类型：`SessionHeader`、`NewSessionOptions`、`SessionEntryBase`、`SessionMessageEntry`、
  `ThinkingLevelChangeEntry`、`ModelChangeEntry`、`UsageEntry`、`CompactionEntry`、
  `BranchSummaryEntry`、`CustomEntry`、`LabelEntry`、`SessionInfoEntry`、`CustomMessageEntry`、
  `ContextEditableContent`、`ContextEditEntry`、`SessionEntry`、`FileEntry`、`SessionTreeNode`、
  `ProjectedSessionEntry`、`SessionProjection`、`SessionContext`、`SessionInfo`、
  `ReadonlySessionManager`、`SessionListProgress`
- 内部：`createSessionId`（= `uuidv7()`）、`generateId`、`migrateV1ToV2`、`migrateV2ToV3`、
  `migrateToCurrentVersion`、`buildEntryIndex`、`buildSessionPath`、`getSessionContextSettings`、
  `projectContextEntry`、`getDefaultSessionDirPath`、`SessionHeaderScanLimitError`、
  `parseSessionEntryLine`、`parseSessionHeaderCandidate`、`readSessionHeader`、
  `readSessionHeaderForDiscovery`、`getSessionHeaderCwd`、`sessionCwdMatches`、
  `isMessageWithContent`、`extractTextContent`、`getMessageActivityTime`、`buildSessionInfo`、
  `mapWithConcurrency`、`sortSessionInfos`、`buildSessionInfosWithConcurrency`、
  `listSessionsFromDir`。

#### 2.2.2 条目类型（字段与顺序）

所有条目（除 header）继承：

```ts
interface SessionEntryBase {
  type: string;
  id: string;          // 通常 8 位十六进制；冲突回退为完整 UUID
  parentId: string | null;
  timestamp: string;   // ISO 8601
}
```

| type | 额外字段（按对象字面量顺序，影响 JSON 字节） |
|---|---|
| `session`（header） | `version?: number`（v1 无）、`id`、`timestamp`、`cwd`、`parentSession?: string` |
| `message` | `message: AgentMessage` |
| `thinking_level_change` | `thinkingLevel: string` |
| `model_change` | `provider: string`、`modelId: string` |
| `usage` | `kind: string`、`provider: string`、`model: string`、`usage: Usage`、`note?: string` |
| `compaction` | `summary`、`firstKeptEntryId`、`tokensBefore`、`details?`、`usage?`、`fromHook?`、`systemMessage?` |
| `branch_summary` | `fromId`、`summary`、`details?`、`usage?`、`fromHook?` |
| `custom` | `customType`、`data?` |
| `custom_message` | `customType`、`content`、`display`、`details?`（落盘字段顺序为 `type,customType,content,display,details,id,parentId,timestamp`） |
| `context_edit` | `targetId`、`replacement: { content } \| null` |
| `label` | `targetId`、`label: string \| undefined` |
| `session_info` | `name?: string` |

注意 `appendCustomMessageEntry` 的字面量顺序为
`{ type, customType, content, display, details, id, parentId, timestamp }`，
`appendCompaction` 为
`{ type, id, parentId, timestamp, summary, firstKeptEntryId, tokensBefore, details, usage, fromHook, ...(systemMessage ? {systemMessage} : {}) }`，
`appendBranchSummary`（`branchWithSummary`）为
`{ type, id, parentId, timestamp, fromId, summary, details, usage, fromHook }`，
`appendUsage` 为 `{ type, id, parentId, timestamp, kind, provider, model, usage, ...(note?{note}:{}) }`。
Rust 侧对每种条目用带固定字段顺序的 `#[derive(Serialize)]` 结构体 + `#[serde(skip_serializing_if = "Option::is_none")]`
复刻；`label`/`data`/`details` 等「值为 undefined 也写键」的情形需逐一核对 JS 行为：
`JSON.stringify` 会**省略**值为 `undefined` 的键，因此 `label: undefined` 的 label 条目落盘后没有 `label` 键。

`CompactionEntry.systemMessage`：`SystemMessage` 且 `timestamp` 被替换为
`new Date(entry.timestamp).getTime()`（毫秒数）。

#### 2.2.3 ID 生成

- `createSessionId()` = `uuidv7()`：会话 ID 必须是 UUIDv7（时间有序）。Rust 用 `uuid` crate 的
  `Uuid::now_v7()`（需要 feature `v7`）。
- `generateId(byId)`：循环最多 100 次，取 `randomUUID().slice(0, 8)`（8 个十六进制字符，
  即 4 字节随机数的小写 hex）并检查 `byId.has(id)`；100 次仍冲突则返回完整 UUID。
  注意取的是 **UUID 字符串的前 8 个字符**（形如 `"3f2a9c1b"`）。
- 会调用 `generateId` 的写方法：`appendMessage`、`appendThinkingLevelChange`、`appendModelChange`、
  `appendUsage`、`appendCompaction`、`appendCustomEntry`、`appendSessionInfo`、
  `appendCustomMessageEntry`、`appendContextEdit`、`appendLabelChange`、`branchWithSummary`，
  以及 `createBranchedSession` 里生成 label 条目。

#### 2.2.4 `assertValidSessionId`

正则：`/^[A-Za-z0-9](?:[A-Za-z0-9._-]*[A-Za-z0-9])?$/`。
不匹配时抛：

```
Session id must be non-empty, contain only alphanumeric characters, '-', '_', and '.', and start and end with an alphanumeric character
```

即：非空、仅字母数字与 `-_.`、首尾必须是字母数字（单字符 id 合法）。

#### 2.2.5 迁移

`migrateToCurrentVersion(entries): boolean`：

1. `header = entries.find(e => e.type === "session")`；`version = header?.version ?? 1`。
2. `version >= 3` → 返回 false（不做任何修改）。
3. `version < 2` → `migrateV1ToV2(entries)`；`version < 3` → `migrateV2ToV3(entries)`。
4. 返回 true。

`migrateV1ToV2`（原地）：

- 维护 `ids: Set<string>` 与 `prevId: string | null = null`。
- header 条目：`entry.version = 2`，continue。
- 其它条目：`entry.id = generateId(ids)`（注意传入的是 `Set`，其 `has` 只反映显式加入的 id；
  代码**没有**把新 id 加入 `ids` — 但实际上 `generateId` 只调用 `has`，而此函数未 `ids.add(entry.id)`，
  这是源码行为，Rust 需保持等效：v1 迁移中 id 冲突检测退化为「与已收集集合比较」，而集合始终为空，
  所以等价于「不检测冲突」。**实现建议：照抄该行为**，即 v1 迁移不登记 id。
  这一点必须用测试 `should add id/parentId to v1 entries` 验证（其只检查 id/parentId 存在与链式关系）。
- `entry.parentId = prevId`；`prevId = entry.id`。
- compaction 条目：若存在数值型 `firstKeptEntryIndex`，取 `entries[index]`，
  若该条目存在且不是 header，则 `comp.firstKeptEntryId = targetEntry.id`；随后 `delete comp.firstKeptEntryIndex`。
  注意 `entries` 是包含 header 的原数组，索引是**文件行索引**。

`migrateV2ToV3`（原地）：header → `version = 3`；`message` 条目里
`message.role === "hookMessage"` 改为 `"custom"`。

#### 2.2.6 解析

`parseSessionEntries(content: string)`：`content.trim().split("\n")`，逐行跳过空行与 JSON 解析失败行。

`parseSessionEntryLine(line): FileEntry | null`：`line.trim()` 为空返回 null；`JSON.parse` 失败返回 null。

`loadEntriesFromFile(filePath)`：`normalizePath(filePath)`；不存在返回 `[]`。

1. 用 `openSync(path,"r")` + `StringDecoder("utf8")` + 1 MiB（`SESSION_READ_BUFFER_SIZE = 1024*1024`）缓冲
   逐块 `readSync(fd, buffer, 0, buffer.len, null)`；每块用 `decoder.write` 解码（可跨块拼接多字节字符），
   按 `\n` 切行并解析，剩余尾部留到下一块。
2. 读到 0 字节后 `decoder.end()`，再解析最后残留（可能无换行的末行）。
3. 关闭 fd（finally）。
4. **头校验**：`entries.length === 0` → 返回；`entries[0].type !== "session"` 或 `entries[0].id` 不是字符串
   → 返回 `[]`（这不是报错，而是「不是 kiana 会话」）。
5. 若 `pending` 非空（文件末尾没有换行但最后一行被成功解析，或残留被忽略）→ `appendFileSync(path, "\n")`。
   注意判断用的是变量 `pending`（最后一块的残留），语义等价于「文件不以换行结尾」时补一个换行。
6. 返回 entries。

`readSessionHeader(filePath): SessionHeader | null`（内部）：

- `SESSION_HEADER_READ_BUFFER_SIZE = 4096`；`MAX_SESSION_HEADER_SCAN_BYTES = 1024*1024`。
- 逐块读取，扫描至多 1 MiB；把每个物理行交给 `parseSessionHeaderCandidate`：
  - 空行或解析失败 → `undefined`（继续扫描）；
  - 解析成功但 `type !== "session"` 或 id 非字符串 → `null`（停止，视为无头）；
  - 解析成功且是合法 header → 返回该 header。
- 到达扫描上限后，额外 `readSync` 1 字节探测 EOF：若已是 EOF，则把当前累积行作最后一次候选；
  否则抛 `SessionHeaderScanLimitError`，消息为
  `Session header exceeds ${MAX_SESSION_HEADER_SCAN_BYTES}-byte scan limit: ${filePath}`。
- `readSessionHeaderForDiscovery` 吞掉一切异常返回 null。

#### 2.2.7 会话目录

`getDefaultSessionDirPath(cwd, agentDir = getAgentDir())`：

```
resolvedCwd = resolvePath(cwd)
resolvedAgentDir = resolvePath(agentDir)
safePath = `--${resolvedCwd.replace(/^[/\\]/, "").replace(/[/\\:]/g, "-")}--`
return join(resolvedAgentDir, "sessions", safePath)
```

即去掉 cwd 开头的一个路径分隔符，把 `/`、`\`、`:` 全部替换为 `-`，两端加 `--`。
`getDefaultSessionDir` 在目录不存在时 `mkdirSync(recursive:true)` 后返回。

会话文件名：`${timestamp.replace(/[:.]/g,"-")}_${sessionId}.jsonl`，
例如 `2026-02-12T09-31-05-123Z_0194f0e2-....jsonl`。

#### 2.2.8 `SessionManager` 内部状态与构造

```ts
private sessionId: string = "";
private sessionFile: string | undefined;
private sessionDir: string;
private cwd: string;
private persist: boolean;
private flushed: boolean = false;
private fileEntries: FileEntry[] = [];
private byId: Map<string, SessionEntry> = new Map();
private labelsById: Map<string, string> = new Map();
private labelTimestampsById: Map<string, string> = new Map();
private leafId: string | null = null;
```

构造函数（private）：`cwd = resolvePath(cwd)`、`sessionDir = normalizePath(sessionDir)`；
若 `persist && sessionDir && !exists(sessionDir)` → 递归创建。

分支：

- 有 `sessionFile` → `_setSessionFile(sessionFile, preloadedFileEntries)`；
- 否则若 `preloadedFileEntries?.length` → `_loadEntries(preloadedFileEntries, newSessionOptions)`；
- 否则 → `newSession(newSessionOptions)`。

`_setSessionFile(sessionFile, preloaded?)`：

- `this.sessionFile = resolvePath(sessionFile)`。
- 若文件存在：`entries = preloaded ?? loadEntriesFromFile(sessionFile)`。
  - `entries.length === 0`（空文件或无效会话）：
    - 若 `statSync(path).size > 0` → 抛
      `Session file is not a valid ${APP_NAME} session: ${explicitPath}`（APP_NAME 通常为 `kiana`）。
      **不修改文件**。
    - 否则（真正空文件）：`newSession()`；恢复 `this.sessionFile = explicitPath`；
      `_rewriteFile()`；`flushed = true`。
  - 否非空：`_loadEntries(entries)`；`flushed = true`。
- 文件不存在：`newSession()`；保留显式路径。

`newSession(options?)`：

- `options.id !== undefined` → `assertValidSessionId(id)`。
- `sessionId = options?.id ?? uuidv7()`。
- `timestamp = new Date().toISOString()`（`YYYY-MM-DDTHH:mm:ss.sssZ`，毫秒 3 位）。
- header：`{ type:"session", version: 3, id, timestamp, cwd: this.cwd, parentSession: options?.parentSession }`
  （`parentSession` 为 undefined 时序列化会省略）。
- 重置 `fileEntries=[header]`、清空三张 map、`leafId=null`、`flushed=false`。
- 若 `persist`：`sessionFile = join(sessionDir, `${timestamp.replace(/[:.]/g,"-")}_${sessionId}.jsonl`)`。
- 返回 `sessionFile`（persist=false 时为 undefined）。

`_loadEntries(entries, options?)`：

- `header = entries.find(e => e.type === "session")`。
- 有 header：`fileEntries = entries`；`sessionId = header.id`；`migrateToCurrentVersion(fileEntries)`
  为 true 时 `_rewriteFile()`。
- 无 header：`newSession(options)`；`fileEntries = fileEntries.concat(entries)`（把无头条目追加到新头之后）。
- 最后 `_buildIndex()`。

`_buildIndex()`：清空 maps 与 leaf，遍历 `fileEntries`（跳过 header）：

- `byId.set(entry.id, entry)`；`leafId = entry.id`（最后一个非 header 条目成为 leaf）。
- `label` 条目：有 `label` → 写入 `labelsById` / `labelTimestampsById`；无 label → 删除两条目。

`_rewriteFile()`：`persist && sessionFile` 时以 `"w"` 打开，逐条 `JSON.stringify(entry) + "\n"` 写入。

`isPersisted()`、`getCwd()`、`getSessionDir()`、`usesDefaultSessionDir()`（`sessionDir === getDefaultSessionDirPath(cwd)`）、
`getSessionId()`、`getSessionFile()`。

`setSessionFile(sessionFile: string): void`（公开）= `_setSessionFile(sessionFile)`，用于 resume 与分支切换；
它会把当前 manager 切换到另一个文件（沿用同一 cwd/sessionDir/persist 配置）。

#### 2.2.9 落盘规则（关键）

`_hasConversation()`：`fileEntries` 中存在 `type === "message"` 且 `message.role` 为
`user` 或 `assistant` 的条目。

`_persist(entry)`：

- `!persist || !sessionFile` → 直接返回。
- 若 `!flushed`：
  - `!_hasConversation()` → 返回（仍未落盘）。
  - 否则以 **`wx`**（排他创建）打开文件，把**全部** `fileEntries` 逐条写入，`flushed = true`。
- 否则：`appendFileSync(sessionFile, JSON.stringify(entry) + "\n")`。

注意：首次落盘写的是 `fileEntries` 全量（含 header 和之前所有内存条目），
包含本次追加的 entry 本身（`_appendEntry` 先 push 再 `_persist`）。

`_appendEntry(entry)`：`fileEntries.push(entry)`；`byId.set`；`leafId = entry.id`；`_persist(entry)`。

#### 2.2.10 追加 API（全部以当前 leaf 为 parent，追加后 leaf 前移）

| 方法 | 生成条目 | 返回 |
|---|---|---|
| `appendMessage(message: Message \| CustomMessage \| BashExecutionMessage)` | `{type:"message", id, parentId:leafId, timestamp:nowISO, message}` | `entry.id` |
| `appendThinkingLevelChange(thinkingLevel)` | `thinking_level_change` | id |
| `appendModelChange(provider, modelId)` | `model_change` | id |
| `appendUsage(kind, provider, model, usage, note?)` | `usage`（`note` 用条件展开） | **条目对象**（不是 id） |
| `appendCompaction(summary, firstKeptEntryId: string\|null, tokensBefore, details?, fromHook?, usage?)` | `compaction` | id |
| `appendCustomEntry(customType, data?)` | `custom` | id |
| `appendSessionInfo(name)` | `session_info` | id |
| `appendCustomMessageEntry(customType, content, display, details?)` | `custom_message` | id |
| `appendContextEdit(targetId, replacement)` | `context_edit` | id |
| `appendLabelChange(targetId, label?)` | `label` | id |

`appendCompaction` 细节：

- 先取 `systemMessage = getCurrentSystemMessage(buildSessionProjection().messages)`（在本条目追加之前）。
- 生成 `id` 后，`firstKeptEntryId = firstKeptEntryId ?? id`（retain-none 时用自己的 id）。
- 若存在 `systemMessage`，写入 `{...systemMessage, timestamp: new Date(timestamp).getTime()}`。

`appendSessionInfo`：`name.replace(/[\r\n]+/g," ").trim()`。

`appendContextEdit` 校验（顺序很重要）：

1. `replacement !== null` 且（不是对象、无 `content`、`content` 既非字符串也非数组）→
   抛 `Context edit replacement must be null or contain string/array content`。
2. 目标不存在 → `Entry ${targetId} not found`。
3. 目标不在**当前分支** → `Entry ${targetId} is not on the active branch`。
4. 目标类型必须是 `custom_message`，或 `message` 且 role ∈ {user, assistant, toolResult}
   → 否则 `Entry ${targetId} does not contribute editable model content`。
5. 归一化：目标 role 为 assistant/toolResult 且 `replacement.content` 是字符串时，
   转成 `{ content: [{ type: "text", text: replacement.content }] }`。

`appendLabelChange`：目标不存在 → `Entry ${targetId} not found`；判定用的是 **truthiness**（`if (label)`），
因此 `label === ""`（空串）与 `undefined` 走同一条「删除」分支：条目仍会落盘且带 `"label": ""`
（`JSON.stringify` 只省略 `undefined`，不省略空串），但内存里的 `labelsById`/`labelTimestampsById`
被删除（`getLabel` 返回 undefined、`getTree` 的 label 为 undefined）。

`getSessionName()`：**反向遍历 `fileEntries`**（不是 `getEntries()`），找最新 `session_info`，
返回 `entry.name?.trim() || undefined`（空串 → undefined）。

#### 2.2.11 读取与遍历

- `getLeafId()`、`getLeafEntry()`、`getEntry(id)`。
- `getChildren(parentId)`：遍历 `byId.values()` 顺序（插入顺序），返回 `parentId` 匹配者。
- `getLabel(id)`。
- `getBranch(fromId?)`：从 `fromId ?? leafId` 起沿 parentId 回溯到 root，**reverse** 后返回（root 在首）。
- `buildContextEntries()`、`buildSessionProjection()`、`buildSessionContext()`（见 2.2.13）。
- `getHeader()`：`fileEntries.find(type==="session")` 或 null。
- `getEntryCount()`：`byId.size`。
- `getEntries()`：`fileEntries.filter(e => e.type !== "session")`（浅拷贝新数组，元素同一对象）。
- `getTree(): SessionTreeNode[]`：
  - 为每个 entry 建节点 `{ entry, children: [], label, labelTimestamp }`。
  - `parentId === null || parentId === entry.id` → root；父存在 → push 到父的 children；父不存在（孤儿）→ root。
  - 用**显式栈**做后序处理：弹出节点，把其 children 按 `new Date(timestamp).getTime()` **升序**排序，
    再把 children 压栈（避免深树递归爆栈）。
  - 返回 roots。

#### 2.2.12 分支

- `branch(branchFromId)`：不存在 → `Entry ${id} not found`；否则 `leafId = branchFromId`。
- `resetLeaf()`：`leafId = null`。
- `branchWithSummary(branchFromId: string|null, summary, details?, fromHook?, usage?)`：
  - `branchFromId !== null` 且不存在 → `Entry ${id} not found`。
  - `fromId = this.leafId ?? "root"`（注意字面量 `"root"`）。
  - `leafId = branchFromId`；追加 `branch_summary`（parentId = branchFromId）；返回 id。
- `createBranchedSession(leafId): string | undefined`：
  1. `previousSessionFile = sessionFile`；`path = getBranch(leafId)`；空 → `Entry ${leafId} not found`。
  2. 过滤 label 条目并**重新串链**：对每个非 label 条目，`parentId = pathParentId`；
     compaction 条目额外把 `firstKeptEntryId` 映射：若 `firstKeptEntryId === entry.id` 保持自身 id，
     否则用「被移除 label 的替换映射」`replacementByLabelId`（label 之后第一个非 label 条目的 id）或原值。
  3. `newSessionId = uuidv7()`；`timestamp = nowISO()`；新文件名同 `newSession` 规则。
  4. 新 header：`{ type:"session", version:3, id:newSessionId, timestamp, cwd:this.cwd, parentSession: this.persist ? previousSessionFile : undefined }`。
  5. 收集 label：只保留 target 在 `pathWithoutLabels` 中的 label（用最新值 + 其 label 时间戳）。
  6. persist 模式：重建 label 条目（parentId 以路径末条目 id 起链），写入
     `fileEntries = [header, ...pathWithoutLabels, ...labelEntries]`，`sessionId = newSessionId`，
     `sessionFile = newSessionFile`，`_buildIndex()`；若 `_hasConversation()` → `_rewriteFile()` 且
     `flushed = true`，否则 `flushed = false`；返回新路径。
     每个 label 条目用 `generateId(new Set(pathEntryIds))`，且生成后 `pathEntryIds.add(labelEntry.id)`
     （**同一个 Set 逐步增长**，所以同一会话内的多个 label id 互相不冲突）。
  7. in-memory 模式：同样重建（label id 需避免与已有 id 冲突），写 `fileEntries`/`sessionId`，
     `_buildIndex()`；返回 undefined。
     注意 in-memory 分支的 id 冲突集合是**每次重建**的
     `new Set([...pathEntryIds, ...labelEntries.map(e => e.id)])`（与 persist 分支的可变 Set 不同），
     两分支行为等价但实现不同，Rust 侧可统一为「逐步增长的 Set」，但必须保证同一次 fork 内 label id 互异。

#### 2.2.13 投影算法（核心，必须逐字复刻）

`buildEntryIndex(entries, byId?)`：给了 `byId` 直接用；否则按顺序建 map。

`buildSessionPath(entries, leafId?, byId?)`：

- `leafId === null` → `[]`。
- `leafId` 给了但 map 里没有 → 回退到 `entries[entries.length-1]`。
- 都没有 → `[]`。
- 从 leaf 沿 parentId 回溯（parentId 为空或找不到即停），再 reverse。

`getSessionContextSettings(path)`：初值 `thinkingLevel="off"`、`model=null`；
顺序遍历 path：`thinking_level_change` → 覆盖 thinkingLevel；`model_change` → 覆盖 model；
`message` 且 role 为 assistant → 覆盖 model 为 `{provider: message.provider, modelId: message.model}`。

`sessionEntryToContextMessages(entry)`：

- `message`：容错处理——`system` 且 `content == null` → `[{...message, content: ""}]`；
  `user`/`assistant`/`toolResult` 且 `content == null` → `[{...message, content: []}]`；否则 `[message]`。
- `custom_message` → `[createCustomMessage(customType, content ?? [], display, details, timestamp)]`。
- `branch_summary` 且 `summary` 非空 → `[createBranchSummaryMessage(summary, fromId, timestamp)]`。
- `compaction` → `[summaryMessage]`，若 `systemMessage` 存在则 `[systemMessage, summaryMessage]`。
- 其它（含 `usage`、`custom`、`context_edit`、`label`、`session_info`、`model_change`、
  `thinking_level_change`）→ `[]`。

`buildContextEntries(entries, leafId?, byId?)`：

1. `path = buildSessionPath(...)`。
2. 找 path 上**最后一个** compaction 条目 `compaction`。没有 → 返回 path。
3. `compactionIdx = path.findIndex(id === compaction.id)`；<0 → 返回 path。
4. `contextEntries = [compaction]`；`foundFirstKept = false`；遍历 `path[0 .. compactionIdx-1]`：
   - `entry.id === compaction.firstKeptEntryId` → `foundFirstKept = true`；
   - 若 `foundFirstKept` 且**不是** system message → push。
5. push `path.slice(compactionIdx+1)`。
6. 返回。

即：compaction 条目自身在首位，其后是「保留区」的非 system 条目与 compaction 之后的所有条目；
更早的 system 消息被折叠进 compaction 的 `systemMessage`。

`projectContextEntry(entry, edit)`：

- `messages = sessionEntryToContextMessages(entry)`；无 edit → 返回。
- `replacement === null` → `[]`（省略）。
- 否则对每条消息：role 不在 {user, assistant, toolResult, custom} 中原样返回；
  计算 `content`：若（role 为 assistant 或 toolResult）且 `replacement.content` 是字符串 →
  `[{type:"text", text: replacement.content}]`；否则 `replacement.content`；
  返回 `{...message, content}`（保留其余元数据）。

`buildSessionProjection(entries, leafId?, byId?)`：

1. `path = buildSessionPath(...)`；`{thinkingLevel, model} = getSessionContextSettings(path)`。
2. `contextEntries = buildContextEntries(...)`。
3. `edits = Map<targetId, ContextEditEntry>`：顺序遍历 `contextEntries`，
   后出现的 `context_edit` 覆盖前者（最新生效）。
4. 逐条投影：`messages = (sourceEntry.type === "compaction" && index > 0) ? [] : projectContextEntry(sourceEntry, edits.get(sourceEntry.id))`。
   即 `buildContextEntries` 可能因保留区覆盖了更早的 compaction id 而保留旧 compaction 条目；
   只有**下标 0** 的 compaction 贡献 checkpoint 与 summary。
5. 返回 `{ entries: projectedEntries, messages: flatMap, thinkingLevel, model }`。

`buildSessionContext(...)`：取 projection 的 `messages/thinkingLevel/model`。

`getLatestCompactionEntry(entries)`：反向找第一个 `type === "compaction"`，没有返回 null。

#### 2.2.14 静态工厂

- `SessionManager.create(cwd, sessionDir?, options?)`：`dir = sessionDir ? normalizePath(sessionDir) : getDefaultSessionDir(cwd)`；
  `new SessionManager(cwd, dir, undefined, true, options)`。
- `SessionManager.open(path, sessionDir?, cwdOverride?)`：
  - `resolvedPath = resolvePath(path)`。
  - 当 `cwdOverride === undefined && exists(resolvedPath)`：先 `readSessionHeader`；若抛
    `SessionHeaderScanLimitError`，则退化为 `loadEntriesFromFile` 全量加载，并取首条目为 header（类型不符则 null）。
    其它异常向上抛。
  - `cwd = cwdOverride ?? headerCwd ?? process.cwd()`。
  - `dir = sessionDir ? normalizePath(sessionDir) : resolve(resolvedPath, "..")`。
  - `new SessionManager(cwd, dir, resolvedPath, true, undefined, preloadedFileEntries)`。
- `SessionManager.continueRecent(cwd, sessionDir?)`：
  - `dir = sessionDir ? normalizePath(sessionDir) : getDefaultSessionDir(cwd)`。
  - `filterCwd = sessionDir !== undefined && dir !== getDefaultSessionDirPath(cwd)`。
  - `mostRecent = findMostRecentSession(dir, filterCwd ? cwd : undefined)`。
  - 有则 `new SessionManager(cwd, dir, mostRecent, true)`，否则新建。
- `SessionManager.inMemory(cwd = process.cwd(), options?, entries?: FileEntry[])`：
  `new SessionManager(cwd, "", undefined, false, options, entries)`。
- `SessionManager.forkFrom(sourcePath, targetCwd, sessionDir?, options?)`：
  1. `resolvePath` 两者；`loadEntriesFromFile(source)`。
  2. 空 → `Cannot fork: source session file is empty or invalid: ${resolvedSourcePath}`。
  3. 无 header → `Cannot fork: source session has no header: ${resolvedSourcePath}`。
  4. `dir = sessionDir ? normalizePath : getDefaultSessionDir(targetCwd)`，不存在则创建。
  5. `options.id` 校验；`newSessionId = options.id ?? uuidv7()`。
  6. 新文件 `wx` 打开，写新 header（`parentSession = resolvedSourcePath`、`cwd = targetCwd`），
     再逐条写源文件中**非 header** 条目（原样，不改 id/parentId）。
  7. 返回以该文件打开的 SessionManager。
- `SessionManager.findById(cwd, id, sessionDir?)`：在目录内遍历 `.jsonl`（`readdirSync` 顺序），
  用 `readSessionHeaderForDiscovery` 取 header，id 精确匹配且（需要时）cwd 匹配则返回路径；
  异常吞掉返回 undefined。
- `SessionManager.list(cwd, sessionDir?, onProgress?, signal?)`：
  - `dir` 同上；`filterCwd = sessionDir !== undefined && dir !== getDefaultSessionDirPath(cwd)`；
  - `progress` 包装：只在 `includeSession` 通过的 partial 列表上调用。
  - 结果过滤 + `sortSessionInfos`（按 `modified` 降序）。
- `SessionManager.listAll(...)`（三种重载）：
  - 参数可以是 `(onProgress?, signal?)`、`(sessionDir?, onProgress?, signal?)`。
  - 自定义 dir → 直接 `listSessionsFromDir(dir, progress, signal)` 后排序。
  - 否则遍历 `getSessionsDir()`：只取目录或符号链接；对每个目录并发 64 个任务读取 `.jsonl`；
    再并发 64 个任务 `stat`（失败只留 path）；排序键为
    `(b.stats?.mtimeMs ?? -Infinity) - (a.stats?.mtimeMs ?? -Infinity) || basename(b.path).localeCompare(basename(a.path))`
    （注意 `localeCompare` 是 locale 相关排序，见第 8 节难点）。
  - 加载用 10 并发；每完成一个调用 `onProgress(loaded, totalFiles, partial?)`：
    `publishPartial = firstCandidateLoaded && (index === 0 || loaded % 100 === 0 || loaded === totalFiles)`，
    partial 为当前已加载会话的排序副本。
  - 异常：`signal?.throwIfAborted()` 后返回 `[]`（**取消会抛出**）。

`findMostRecentSession(sessionDir, cwd?)`：

- `normalizePath(sessionDir)`；`resolvedCwd = cwd ? resolvePath(cwd) : undefined`。
- `readdirSync` 过滤 `.jsonl`，map 到 `{path, mtime: statSync(path).mtimeMs}`，按 mtime **降序**排序。
- 依次 `readSessionHeaderForDiscovery`，若 header 存在且（无 cwd 过滤 或 cwd 匹配）返回该路径。
- 全部不匹配返回 null；任何异常返回 null。
- `sessionCwdMatches(cwd, resolvedCwd)`：`cwd !== undefined && cwd !== "" && resolvePath(cwd) === resolvedCwd`。

`listSessionsFromDir(dir, onProgress?, signal?)`：

- signal 已中止 → 抛出；dir 不存在 → `[]`。
- `readdir(dir)`，过滤 `.jsonl`，按 `basename` 的 `b.localeCompare(a)` **降序**排序（与 mtime 无关）。
- 进度：`loaded === 1 || loaded % 10 === 0 || loaded === files.length` 时给出排序副本。
- 结果过滤 null 返回；异常 → 检查取消后返回 `[]`。

`buildSessionInfo(filePath, signal?, fileStats?)`（内部，异步）：

- `stat`；用 `readline.createInterface({ input: createReadStream(path, {encoding:"utf8", signal}), crlfDelay: Infinity })` 逐行读。
- 第一个可解析条目必须是 header（否则返回 null）。
- `session_info` → `name = entry.name?.trim() || undefined`（最新覆盖，包括显式清空）。
- `message` 条目：`messageCount++`；`activityTime = getMessageActivityTime(entry)`：
  - 非 user/assistant 或没有 content 属性 → undefined；
  - message 自带数值 `timestamp` → 用它；否则 `new Date(entry.timestamp).getTime()`（NaN → undefined）。
  - `lastActivityTime = Math.max(lastActivityTime ?? 0, activityTime)`。
- user/assistant 的文本内容（`extractTextContent`：字符串直接返回；数组取 text 块并用 **`" "`（空格）** join）：
  非空则 push 到 `allMessages`；`firstMessage` 只在为空且 role 为 user 时取。
- 返回 `SessionInfo`：`path`、`id`、`cwd`（header.cwd 为字符串否则 `""`）、`name`、`parentSessionPath`、
  `created: new Date(header.timestamp)`、
  `modified = lastActivityTime > 0 ? new Date(lastActivityTime) : (!isNaN(headerTime) ? new Date(headerTime) : stats.mtime)`、
  `messageCount`、`firstMessage: firstMessage || "(no messages)"`、`allMessagesText: allMessages.join(" ")`。
- 任何异常：`signal?.throwIfAborted()`（若已取消则抛出），否则返回 null。

### 2.3 `src/core/agent-session.ts`

这是本单元最大的文件（4458 行）。下面按「导出的类型 → 私有状态 → 构造与钩子安装 → 事件流 →
prompt/队列 → 模型/thinking → 压缩 → 扩展 → 重试 → bash → 树导航 → 统计/导出」组织。

#### 2.3.1 导出的辅助符号

`export interface ParsedSkillBlock { name: string; location: string; content: string; userMessage: string | undefined }`

`export function parseSkillBlock(text: string): ParsedSkillBlock | null`

正则（**逐字**）：

```
/^<skill name="([^"]+)" location="([^"]+)">\n([\s\S]*?)\n<\/skill>(?:\n\n([\s\S]+))?$/
```

- 必须以 `<skill name="..." location="...">` 开头、紧跟一个换行；内容非贪婪到 `\n</skill>`；
  其后可选 `\n\n` + 任意内容（作为 `userMessage`）。
- 返回 `{ name: m[1], location: m[2], content: m[3], userMessage: m[4]?.trim() || undefined }`。
- 不匹配返回 null。

`function withoutDeletedHeaders(headers?: ProviderHeaders): Record<string,string> | undefined`
：过滤掉值为 `null` 的 header（`ProviderHeaders` 允许 null 表示删除），不过滤则返回 undefined。

`function estimateMessagesTokens(messages: AgentMessage[]): number`：`Σ estimateTokens(m)`。

#### 2.3.2 导出类型

```ts
type WithParentToolCallId<E> =
  E extends { type: "tool_execution_start" | "tool_execution_update" | "tool_execution_end" }
    ? E & { parentToolCallId?: string }
    : E;

export type AgentSessionEvent =
  | WithParentToolCallId<Exclude<AgentEvent, { type: "agent_end" }>>
  | { type: "agent_end"; messages: AgentMessage[]; willRetry: boolean }
  | { type: "agent_settled"; aborted: boolean }
  | { type: "queue_update"; steering: readonly string[]; followUp: readonly string[] }
  | { type: "compaction_start"; reason: "manual" | "threshold" | "overflow" }
  | { type: "entry_appended"; entry: SessionEntry }
  | { type: "session_info_changed"; name: string | undefined }
  | { type: "thinking_level_changed"; level: ThinkingLevel }
  | { type: "compaction_end"; reason: "manual" | "threshold" | "overflow";
      result: CompactionResult | undefined; aborted: boolean; willRetry: boolean; errorMessage?: string }
  | { type: "auto_retry_start"; attempt: number; maxAttempts: number; delayMs: number; errorMessage: string }
  | { type: "auto_retry_end"; success: boolean; attempt: number; finalError?: string }
  | { type: "summarization_retry_scheduled"; attempt: number; maxAttempts: number; delayMs: number; errorMessage: string }
  | { type: "summarization_retry_attempt_start"; source: "branchSummary" }
  | { type: "summarization_retry_attempt_start"; source: "compaction"; reason: "manual" | "threshold" | "overflow" }
  | { type: "summarization_retry_finished" }
  | { type: "bash_execution_update"; id?: string; delta: string };

export type AgentSessionEventListener = (event: AgentSessionEvent) => void;
```

注意 `agent_end` 被**重新定义**（增加 `willRetry`），其余 `AgentEvent` 原样透传但可为
`tool_execution_*` 附加 `parentToolCallId`。

```ts
export interface AgentSessionConfig {
  agent: Agent;
  sessionManager: SessionManager;
  settingsManager: SettingsManager;
  cwd: string;
  scopedModels?: Array<{ model: Model<any>; thinkingLevel?: ThinkingLevel }>;
  resourceLoader: ResourceLoader;
  customTools?: ToolDefinition[];
  modelRuntime: ModelRuntime;
  cacheWarmer?: Pick<CacheWarmer, "cancel" | "status" | "onAgentSettled" | "onModeChanged" | "onWarmed">;
  initialActiveToolNames?: string[];
  usesDefaultTools?: boolean;          // 默认 false
  defaultToolModifiers?: string[];     // 默认 []
  allowedToolNames?: string[];
  excludedToolNames?: string[];
  baseToolsOverride?: Record<string, AgentTool>;
  extensionRunnerRef?: { current?: ExtensionRunner };
  sessionStartEvent?: SessionStartEvent;  // 默认 { type:"session_start", reason:"startup" }
}

export interface ExtensionBindings {
  uiContext?: ExtensionUIContext;
  mode?: ExtensionMode;
  commandContextActions?: ExtensionCommandContextActions;
  abortHandler?: () => void;
  shutdownHandler?: ShutdownHandler;
  onError?: ExtensionErrorListener;
}

export type QueuedInputDisposition = "handled" | "queued";
export type PromptDisposition = QueuedInputDisposition | "started";

export interface PromptOptions {
  expandPromptTemplates?: boolean;   // 默认 true
  images?: ImageContent[];
  streamingBehavior?: "steer" | "followUp";   // streaming 时必填
  source?: InputSource;              // 默认 "interactive"
  preflightResult?: (disposition: PromptDisposition) => void;
}

export interface ModelMutationOptions { persist?: boolean }
export interface ModelCycleResult { model: Model<any>; thinkingLevel: ThinkingLevel; isScoped: boolean }

export interface SessionStats {
  sessionFile: string | undefined;
  sessionId: string;
  userMessages: number; assistantMessages: number; toolCalls: number; toolResults: number; totalMessages: number;
  tokens: { input: number; output: number; cacheRead: number; cacheWrite: number; total: number };
  cost: number;
  contextUsage?: ContextUsage;   // { tokens: number|null; contextWindow: number; percent: number|null }
}
```

#### 2.3.3 私有状态（逐条，全部有语义）

事件订阅：
- `_scopedModels: Array<{model, thinkingLevel?}>`
- `_unsubscribeAgent?: () => void`
- `_eventListeners: AgentSessionEventListener[]`
- `_isAgentRunActive = false`
- `_agentRunAbortRequested = false`
- `_idleWaitPromise?: Promise<void>`、`_resolveIdleWait?: () => void`

队列：
- `_steeringMessages: string[]`（仅用于 UI 显示，投递时按文本匹配移除）
- `_followUpMessages: string[]`
- `_pendingNextTurnMessages: CustomMessage[]`（「顺带」的上下文，随下一个 user prompt 一起注入）
- `_pendingCustomMessages: CustomMessage[]`（运行中排队的仅上下文消息，turn 结束后落地）

压缩：
- `_compactionAbortController?`（手动）、`_autoCompactionAbortController?`（自动）、`_overflowRecoveryAttempted = false`
- `_branchSummaryAbortController?`

重试：
- `_retryAbortController?`、`_retryAttempt = 0`、`_failedResponse?: AssistantMessage`

bash：
- `_bashAbortControllers: Set<AbortController>`、`_pendingBashMessages: BashExecutionMessage[]`

扩展/边界：
- `_extensionRunner!: ExtensionRunner`、`_turnIndex = 0`
- `_entryIdsByMessage: WeakMap<object,string>`（消息对象 → 持久化条目 id）
- `_boundaryDispatchedMessages: WeakSet<object>`（已派发 turn_end 边界的消息）
- `_lastAssistantMessage?`、`_lastActivityOutcome: "completed"|"aborted"|"error" = "completed"`
- `_isBeforeSettle = false`、`_abortDuringBeforeSettle = false`、`_isEmittingAgentSettled = false`
- `_deferredSettledActions: Array<() => Promise<void>>`

资源/工具：
- `_resourceLoader`、`_customTools`、`_baseToolDefinitions: Map<string, ToolDefinition>`
- `_cwd`、`_extensionRunnerRef?`、`_initialActiveToolNames?`
- `_pendingToolNames: Set<string>`、`_usesDefaultTools`、`_defaultToolModifiers`
- `_allowedTools?: (name)=>boolean`、`_allowlistFiltersMcp = false`、`_excludedTools?`
- `_baseToolsOverride?`、`_sessionStartEvent`
- `_extensionUIContext?`、`_extensionMode: ExtensionMode = "print"`、`_extensionCommandContextActions?`
- `_extensionAbortHandler?`、`_extensionShutdownHandler?`、`_extensionErrorListener?`、`_extensionErrorUnsubscriber?`
- `_modelRuntime`、`_cacheWarmer?`
- `_toolRegistry: Map<string, AgentTool>`、`_nestedToolCalls?: NestedToolCallRunner`
- `_hiddenDeclarations: ReadonlySet<string>`
- `_toolDefinitions: Map<string, {definition, sourceInfo}>`
- `_toolPromptSnippets: Map<string,string>`、`_toolPromptGuidelines: Map<string,string[]>`
- `_baseSystemPromptOptions!: NormalizedBuildSystemPromptOptions`
- `_runSystemPromptOptions?: NormalizedBuildSystemPromptOptions`

**Rust 提示**：`_entryIdsByMessage` / `_boundaryDispatchedMessages` 依赖 JS 对象身份。
Rust 需给每条消息分配稳定 handle（如 `MessageId(u64)` 或 `Arc` 指针身份），
并在消息被替换（`_replaceMessageInPlace`）时保持 handle 不变。

#### 2.3.4 构造函数（顺序严格）

```
agent = config.agent; sessionManager; settingsManager;
_scopedModels = config.scopedModels ?? [];
_resourceLoader; _customTools = config.customTools ?? []; _cwd; _modelRuntime; _cacheWarmer;
if (_cacheWarmer) _cacheWarmer.onWarmed = (entry) => this._emit({type:"entry_appended", entry});
_extensionRunnerRef; _initialActiveToolNames; _usesDefaultTools = ?? false; _defaultToolModifiers = ?? [];
if (allowedToolNames) { _allowedTools = createToolNameMatcher(allowedToolNames);
  _allowlistFiltersMcp = allowedToolNames.length === 0 || allowedToolNames.some(e => e.startsWith("mcp__")); }
_excludedTools = excludedToolNames ? createToolNameMatcher(...) : undefined;
_baseToolsOverride; _sessionStartEvent = config.sessionStartEvent ?? {type:"session_start", reason:"startup"};

_unsubscribeAgent = agent.subscribe(this._handleAgentEvent);
_installAgentToolHooks();
_installAgentNextTurnRefresh();
_installAgentRequestProjection();
_installAgentBoundaryHooks();
_installHiddenDeclarationsProjection();
_installAgentForcedPromptProjection();

_buildRuntime({ activeToolNames: _initialActiveToolNames, includeAllExtensionTools: true });
if (_usesDefaultTools) _restoreToolsFromTranscript(_defaultToolModifiers);
```

注意 `_installAgentNextTurnRefresh` 在 `_installAgentRequestProjection` **之前**；
两个 transformContext 安装顺序为 hidden declarations 先、forced prompt 后（后者包住前者）。

#### 2.3.5 认证辅助

`_getRequiredRequestAuth(model, signal?)` → `{model, apiKey?, headers?, env?}`：

1. `_modelRuntime.getAuth(model, {signal})`；抛错时若 `error.cause.message === "authHeader requires a resolved API key"`
   → 抛 `formatNoApiKeyFoundMessage(model.provider)`；否则原样抛。
2. 若结果存在且（`auth.apiKey` 或 `auth.headers`）：`requestModel = auth.baseUrl ? {...model, baseUrl} : model`；
   返回 `{ model: requestModel, apiKey, headers: withoutDeletedHeaders(auth.headers), env: result.env }`。
3. 否则：若 `_modelRuntime.isUsingOAuth(provider)` → 抛
   `Authentication failed for "${provider}". Credentials may have expired or network is unavailable. Run '/login ${provider}' to re-authenticate.`
   （字符串由三段拼接，见 3.1）。否则抛 `formatNoApiKeyFoundMessage(provider)`。

`_getSummarizationRequestAuth(selectedModel, signal?)` → `{model, apiKey?, headers?, env?, thinkingLevel}`：

1. 若是虚拟模型：`await resolveModel(selectedModel, convertToLlm(this.messages), {reason:"direct", thinkingLevel: this.thinkingLevel, signal})`；
   否则 `{model: selectedModel, thinkingLevel: this.thinkingLevel}`。
2. 若 `agent.streamFunction === streamSimple`（即用 kiana 自己的流函数）→ `{...(await _getRequiredRequestAuth(model, signal)), thinkingLevel}`。
3. 否则（自定义 streamFn）：尝试 `getAuth`；无结果返回 `{model, thinkingLevel}`；
   有结果返回带 apiKey/headers/env 的对象；抛错时若 `signal?.aborted` 重新抛，否则降级返回 `{model, thinkingLevel}`。

`_modelForMessage(message)`：若当前模型是虚拟模型 → `getPhysicalModel(message.provider, message.model)`；
否则当前模型 provider+id 与消息一致时返回当前模型，否则 undefined。
（用途：判断 assistant 消息是否来自「同一模型」，决定是否允许触发溢出/阈值压缩。）

`_recordSelection()`：把当前选择记到分支上，供 resume 恢复。
- 无模型直接返回。
- `recorded = getBranchSelection(getBranch(), getModel)`；`!recorded` 或与当前 provider/id 相同 → 返回。
- `recordedModel = getModel(recorded...)`；若当前模型**不是**虚拟模型且 recordedModel 也不是虚拟模型 → 返回。
- 否则 `sessionManager.appendModelChange(model.provider, model.id)`。

`_limitsModel()`：`routedModel?.model ?? model`（虚拟模型时用最近一次成功响应命中的物理模型）。

#### 2.3.6 Agent 钩子安装

`_installAgentToolHooks()`：

```
agent.beforeToolCall = (context) => this._beforeToolCall(context);
agent.afterToolCall  = (context) => this._afterToolCall(context);
```

（回调在执行时读取 `this._extensionRunner`，因此扩展 reload 后无需重装。）

`_beforeToolCall({toolCall,args}, parentToolCallId?)`：
- 无 `tool_call` handler → undefined。
- 否则 `runner.emitToolCall({ type:"tool_call", toolName, toolCallId, ...(parentToolCallId?{parentToolCallId}:{}), input: args })`。
- 捕获异常：Error 原样抛；否则抛 `Extension failed, blocking execution: ${String(err)}`。

`_afterToolCall({toolCall,args,result,isError}, parentToolCallId?)`：
1. 有 `tool_result` handler → `emitToolResult({type:"tool_result", toolName, toolCallId, ...(parent?
   {parentToolCallId}:{}), input, content: result.content, details: result.details,
   ...(structuredContent === undefined ? {} : {structuredContent}), isError, usage: result.usage})`；否则 undefined。
2. `content = hookResult?.content ?? result.content ?? []`。
3. `resizeOptions = _limitsModel()?.inputLimits?.images?.resize`；
   `normalizeToolResultImages(content, { autoResizeImages: settingsManager.getImageAutoResize(), ...(resizeOptions?{resizeOptions}:{}) })`。
4. 若 `!hookResult && normalizedContent === content` → undefined（不变更）。
5. 返回 `{ content: normalizedContent, details: hookResult?.details,
   structuredContent: hookResult ? hookResult.structuredContent : result.structuredContent,
   isError: hookResult?.isError ?? isError, usage: hookResult?.usage }`。

`_executeNestedToolCall(parentToolCallId, name, args, options)`：
- 惰性创建 `NestedToolCallRunner`（`_nestedToolCalls ??= ...`），host：
  - `getTools: () => _getCallableTools()`
  - `isSequential: () => agent.toolExecution === "sequential"`
  - `runToolCall(toolCall, parentId, signal, onUpdate)`：取 `_findLastAssistantMessage()`；
    没有则返回 `{ toolCall, result: { content:[{type:"text",text:"No assistant message issued this call"}], details:{} }, isError:true }`；
    否则 `runToolCall(toolCall, { tools: _getCallableTools(), assistantMessage, context: { messages: agent.state.messages, tools: agent.state.tools },
      beforeToolCall: ctx => _beforeToolCall(ctx, parentId), afterToolCall: ctx => _afterToolCall(ctx, parentId), signal, onUpdate })`。
  - `emit: async (event) => { await _extensionRunner.emit(event); this._emit(event); }`
- `return _nestedToolCalls.execute(parentToolCallId, name, args, options)`。

`_exceedsCompactionThreshold(model, projection)`：`model.contextWindow <= 0` → false；
否则 `shouldCompact(estimateProjectedContextTokens(projection, getBranch()).tokens, model.contextWindow,
settingsManager.getCompactionSettings(this.model))`。

`_compactBeforeNextAssistantResponse(context)`：
- `projection = buildSessionProjection()`；`model = this.model`；
- 无模型 / 虚拟模型 / 未超阈值 → `{...context, messages: projection.messages}`。
- 否则 `await _runAutoCompaction("threshold", false)`；
  返回 `{...context, messages: buildSessionProjection().messages}`。

`_installAgentRequestProjection()`：包装 `agent.prepareRequest`：

```
previousPrepareRequest = agent.prepareRequest
agent.prepareRequest = async (request, signal) => {
  failed = _failedResponse; _failedResponse = undefined;
  prepare = async () => {
    projection = sessionManager.buildSessionProjection();
    canonicalContext = { ...request.context, messages: projection.messages, tools: agent.state.tools.slice() };
    previous = await previousPrepareRequest?.({ ...request, context: canonicalContext,
        model: agent.state.model, thinkingLevel: agent.state.thinkingLevel }, signal);
    return { previous, context: previous?.context ?? canonicalContext, projection };
  };
  ({previous, context, projection} = await prepare());
  model = previous?.model ?? agent.state.model;
  thinkingLevel = previous?.thinkingLevel ?? agent.state.thinkingLevel;
  if (!isVirtualModel(model)) return { ...previous, context, model, thinkingLevel };

  lastResponse = context.messages.findLastIndex(m => m.role === "assistant");
  userTurn = context.messages.slice(lastResponse + 1).some(m => m.role === "user");
  state = getVirtualModelState(getBranch(), model.provider, model.id);
  route = await modelRuntime.resolveModel(model, convertToLlm(context.messages), {
    reason: failed ? "retry" : userTurn ? "user" : "continuation",
    thinkingLevel, signal, failed, state });
  if (route.state !== undefined && route.state !== state) {
    data = { provider: model.provider, modelId: model.id, state: route.state };
    entry = sessionManager.getEntry(sessionManager.appendCustomEntry(VIRTUAL_MODEL_STATE_ENTRY, data));
    if (entry) _emit({type:"entry_appended", entry});
  }
  if (_exceedsCompactionThreshold(route.model, projection)) {
    await _runAutoCompaction("threshold", false);
    ({previous, context} = await prepare());
  }
  return { ...previous, context, model: route.model, thinkingLevel: route.thinkingLevel };
};
```

要点：`_failedResponse` **只消费一次**（读取即清空）；虚拟路由失败会 reject 并以错误响应结束本次 run；
`route.state === state` 时不写 state 条目；阈值检查用路由后的物理模型。

`_installAgentBoundaryHooks()`：

```
previousFinishTurn = agent.finishTurn;
agent.finishTurn = async (turn, signal) => {
  _boundaryDispatchedMessages.add(turn.message);
  extensionContinue = await _dispatchTurnEndBoundary(turn.message, turn.toolResults);
  previousDecision = await previousFinishTurn?.(turn, signal);
  if (previousDecision?.action === "end") return previousDecision;
  if (extensionContinue || previousDecision?.action === "continue") return { action: "continue" };
  return undefined;
};
```

`_dispatchTurnEndBoundary(message, toolResults)`：
1. `_lastActivityOutcome = message.stopReason === "aborted" ? "aborted" : message.stopReason === "error" ? "error" : "completed"`。
2. `messageEntryId = _findPersistedMessageEntryId(message)`；无 `turn_end` handler → false。
3. 无 entryId → `extensionRunner.emitError({extensionPath:"<boundary>", event:"turn_end",
   error:"turn_end could not resolve the persisted assistant entry ID"})`；返回 false。
4. `toolResultEntryIds = toolResults.flatMap(r => entryId ? [entryId] : [])`。
5. `boundary = await emitBoundary({ type:"turn_end", turnIndex: _turnIndex, message, toolResults,
   messageEntryId, toolResultEntryIds, outcome: _lastActivityOutcome }, (entries) => _buildBoundaryContext(entries, "turn_end"))`。
6. `_commitBoundaryDrafts(boundary.entries)`。
7. 若 `boundary.continue` 且 `_buildBoundaryContext([], "turn_end").canContinue` 为假 →
   `_reportInvalidBoundaryContinuation("turn_end")`；返回 false。
8. 返回 `boundary.continue`。

`_installAgentNextTurnRefresh()`：包装 `agent.prepareNextTurnWithContext`（若不存在则用旧
`prepareNextTurn` 适配）：

```
previousPrepareNextTurnWithContext = agent.prepareNextTurnWithContext
  ?? (agent.prepareNextTurn ? async (_turn, signal) => await agent.prepareNextTurn?.(signal) : undefined);
agent.prepareNextTurnWithContext = async (turn, signal) => {
  context = await _compactBeforeNextAssistantResponse(turn.context);
  previousSnapshot = await previousPrepareNextTurnWithContext?.({...turn, context}, signal);
  nextContext = previousSnapshot?.context ?? context;
  runOptions = _runSystemPromptOptions ?? _baseSystemPromptOptions;
  options = normalizeBuildSystemPromptOptions({
    ...runOptions,
    selectedTools: getActiveToolNames(),
    toolSnippets: { ..._baseSystemPromptOptions.toolSnippets, ...runOptions.toolSnippets },
    toolGuidelines: { ..._baseSystemPromptOptions.toolGuidelines, ...runOptions.toolGuidelines },
  });
  updateMessage = _preparePromptAndToolLoadout(options, nextContext.messages);
  _runSystemPromptOptions = options;
  return {
    ...previousSnapshot,
    context: { ...nextContext, tools: agent.state.tools.slice() },
    messages: updateMessage ? [...(previousSnapshot?.messages ?? []), updateMessage] : previousSnapshot?.messages,
    model: agent.state.model,
    thinkingLevel: agent.state.thinkingLevel,
  };
};
```

`_installHiddenDeclarationsProjection()`：包装 `agent.transformContext`（先运行前一个）：

```
transformed = previousTransformContext ? await previousTransformContext(messages, signal) : messages;
if (_hiddenDeclarations.size === 0) return transformed;
return transformed.map(m => {
  if (m.role !== "system" || (!m.toolsAdded && !m.toolsRemoved)) return m;
  const { toolsAdded, toolsRemoved, ...rest } = m;
  added = toolsAdded?.filter(t => !hidden.has(t.name)) ?? [];
  removed = toolsRemoved?.filter(t => !hidden.has(t.name)) ?? [];
  return { ...rest, ...(added.length>0?{toolsAdded:added}:{}), ...(removed.length>0?{toolsRemoved:removed}:{}) };
});
```

`_installAgentForcedPromptProjection()`：再包一层 transformContext：

```
forced = _runSystemPromptOptions?.forceSystemPrompt;
if (forced === undefined) return transformed;
current = getCurrentSystemMessage(transformed);
head = { role:"system", content: forced, ...(current?.toolsAdded ? {toolsAdded: current.toolsAdded} : {}),
         timestamp: current?.timestamp ?? Date.now() };
return [head, ...transformed.filter(m => m.role !== "system")];
```

#### 2.3.7 上下文刷新与投影辅助

`_refreshFinalizedContext()`：`projection = buildSessionProjection()`；对每个
`entry.messages` 中的消息 `_entryIdsByMessage.set(message, entry.sourceEntry.id)`；
`agent.state.messages = projection.messages`。

`_applyBoundaryDrafts(manager, drafts): SessionEntry[]`：按 draft 类型追加：
- `custom` → `appendCustomEntry(customType, data)`
- `custom_message` → `appendCustomMessageEntry(customType, content, display, details)`
- `context_edit` → `appendContextEdit(targetId, replacement)`
- `compaction` → 先算 `tokensBefore = estimateProjectedContextTokens(manager.buildSessionProjection(), manager.getBranch()).tokens`，
  再 `appendCompaction(summary, firstKeptEntryId, tokensBefore, details, true, usage)`
最后 `getEntry(entryId)` 存在则收集，返回。

`_createBoundaryPreviewManager(drafts)`：`header = getHeader()`；无 header → 抛 `Session header is missing`；
`SessionManager.inMemory(_cwd, undefined, [header, ...sessionManager.getBranch()])`，再 apply drafts。

`_getPendingBoundaryMessages()`：`[...agent.peekQueuedMessages(), ..._pendingCustomMessages]`。

`_buildBoundaryContext(drafts, boundary): BoundaryContextPreview`：

```
projection = _createBoundaryPreviewManager(drafts).buildSessionProjection();
pendingMessages = _getPendingBoundaryMessages();
llmMessages = convertToLlm(projection.messages);
finalRole = llmMessages.at(-1)?.role;
hasNonSystemContext = llmMessages.some(m => m.role !== "system");
contextCanContinue = hasNonSystemContext && finalRole !== "assistant";
pendingCustomContext = _pendingCustomMessages.length > 0;
return {
  contextEntries: projection.entries, contextMessages: projection.messages, llmMessages, pendingMessages,
  canContinue: contextCanContinue || pendingCustomContext ||
    (boundary === "turn_end" ? agent.hasQueuedMessages() : finalRole === "assistant" && agent.hasQueuedMessages()),
};
```

`_commitBoundaryDrafts(drafts)`：`appended = _applyBoundaryDrafts(sessionManager, drafts)`；`_refreshFinalizedContext()`；
逐个 `_emit({type:"entry_appended", entry})`。

`_reportInvalidBoundaryContinuation(event)`：`emitError({extensionPath:"<boundary>", event,
error: `${event} requested continuation without runnable model context`})`。

`_emit(event)`：同步遍历 `_eventListeners` 调用（**不** await、不捕获异常）。

`_emitQueueUpdate()`：`_emit({type:"queue_update", steering:[..._steeringMessages], followUp:[..._followUpMessages]})`。

`_emitSessionCompactFailed(event)`：有 `session_compact_failed` handler 时
`await extensionRunner.emit({type:"session_compact_failed", ...event})`。

`_getIdleWaitPromise()`：惰性创建、复用；`_resolveIdleWaitIfIdle()`：`isIdle && resolver` 时清空并 resolve。

`_emitAgentSettled()`：

```
_cacheWarmer?.onAgentSettled();
_isAgentRunActive = false;
_isEmittingAgentSettled = true;
try {
  aborted = _agentRunAbortRequested;
  await extensionRunner.emit({type:"agent_settled", aborted});
  _emit({type:"agent_settled", aborted});
} finally { _isEmittingAgentSettled = false; }
deferred = _deferredSettledActions.splice(0);
if (deferred.length > 0) {
  try { for (a of deferred) await a(); } finally { _resolveIdleWaitIfIdle(); }
  return;
}
_resolveIdleWaitIfIdle();
```

#### 2.3.8 `_handleAgentEvent`（核心事件处理，顺序严格）

```
1. 若 event.type === "agent_start" && _agentRunAbortRequested → agent.abort()
2. 嵌套调用记录：
   若 _nestedToolCalls 存在：
     若 event.type === "message_start" && event.message.role === "toolResult"：
        summary = _nestedToolCalls.takeRecord(message.toolCallId);
        if (summary?.calls) message.nestedCalls = summary.calls;
        if (summary?.usage) message.usage = message.usage ? combineUsage(message.usage, summary.usage) : summary.usage;
     否则若 event.type === "agent_end" → _nestedToolCalls.clear()
3. 若 event.type === "message_start" && message.role === "user"：
   _overflowRecoveryAttempted = false;
   text = contentText(message.content, "");
   if (text) {
     i = _steeringMessages.indexOf(text);
     if (i !== -1) { splice(i,1); _emitQueueUpdate(); }
     else { j = _followUpMessages.indexOf(text); if (j !== -1) { splice(j,1); _emitQueueUpdate(); } }
   }
4. await _emitExtensionEvent(event)
5. _emit(event.type === "agent_end" ? {...event, willRetry: _willRetryAfterAgentEnd(event)} : event)
6. 若 event.type === "message_end"：
   entryId = undefined
   if (message.role === "custom") entryId = sessionManager.appendCustomMessageEntry(customType, content, display, details)
   else if (role ∈ {system,user,assistant,toolResult}) entryId = sessionManager.appendMessage(message)
   if (entryId) _entryIdsByMessage.set(event.message, entryId)
   if (message.role === "assistant") {
     _lastAssistantMessage = message;
     if (stopReason !== "error" && stopReason !== "length") _overflowRecoveryAttempted = false;
     if (stopReason !== "error" && _retryAttempt > 0) {
       _emit({type:"auto_retry_end", success:true, attempt:_retryAttempt});
       _retryAttempt = 0;
     }
   }
7. 若 event.type === "turn_end" → _flushPendingCustomMessages()
```

注意：`turn_end` 的扩展事件在步骤 4 已发（其中会派发 `turn_end` 边界，或由 `finishTurn` 钩子派发并
用 `_boundaryDispatchedMessages` 去重）；步骤 7 在扩展与公共事件之后，确保 `turn_end` handler 排队的
custom message 也能在本 turn 末落地。

`_willRetryAfterAgentEnd(event)`：
- `_agentRunAbortRequested` → false。
- `settings = getRetrySettings()`；`!settings.enabled || _retryAttempt >= settings.maxRetries` → false。
- 反向扫描 `event.messages` 找第一个 assistant：返回 `_isRetryableError(message)`。没有 assistant → false。

`_findPersistedMessageEntryId(message)`：
1. `_entryIdsByMessage.get(message)` 命中即返回。
2. 反向遍历 `getBranch()`：`type === "message" && entry.message === message` → `entry.id`。
3. `messageIndex = agent.state.messages.indexOf(message)`；<0 → undefined。
4. 遍历 projection 的 entries 与其中 messages，按「扁平索引」等于 `messageIndex` 时把
   `entry.sourceEntry.id` 写入 map 并返回；遍历完无果 → undefined。

`_omitRecoveryAttempt(message)`：
1. `toolCallIds = message.content 中 type==="toolCall" 的 id 集合`。
2. `entryId = _findPersistedMessageEntryId(message)`。
3. `messages = agent.state.messages`；`index = messages.findIndex(m => m === message ||
   (entryId !== undefined && _entryIdsByMessage.get(m) === entryId))`。
4. `toolResults`：从 `index+1` 起遍历，遇 assistant 停止；收集属于 `toolCallIds` 的 toolResult。
5. `targets = [message, ...toolResults]`；`targetIds = targets.map(_findPersistedMessageEntryId)`。
6. 若存在 target 无 id 但仍在 `agent.state.messages` 中 → 抛
   `Cannot persist recovery omission because a projected message has no source entry`。
7. 对每个非空 targetId：`appendContextEdit(targetId, null)`，并对产生的 entry `_emit({type:"entry_appended", entry})`。
8. `_refreshFinalizedContext()`。

`_findLastAssistantMessage()`：反向找 assistant，含被中止的。

`_replaceMessageInPlace(target, replacement)`：同一对象直接返回；否则删除 target 的所有自有键，
再 `Object.assign(target, replacement)`（**保持对象身份**，因为 agent state、后续事件、监听器、
持久化都持有同一引用）。

`_emitExtensionEvent(event)`（逐个 type，**必须 await 扩展**）：

| AgentEvent | 扩展事件 |
|---|---|
| `agent_start` | `_turnIndex = 0`；`emit({type:"agent_start"})` |
| `agent_end` | `emit({type:"agent_end", messages: event.messages})` |
| `turn_start` | `emit({type:"turn_start", turnIndex:_turnIndex, timestamp: Date.now()})` |
| `turn_end` | 若 `message.role === "assistant"` 且 `!_boundaryDispatchedMessages.delete(message)` → `await _dispatchTurnEndBoundary(message, toolResults)`；然后 `_turnIndex++` |
| `message_start` | `emit({type:"message_start", message})` |
| `message_update` | `emit({type:"message_update", message, assistantMessageEvent})` |
| `message_end` | `replacement = await emitMessageEnd({type:"message_end", message})`；若 replacement：对 user/assistant/toolResult/custom 且 content 为 null/缺失者归一化为 `content: []`，再 `_replaceMessageInPlace(event.message, normalized)` |
| `tool_execution_start` | `emit({type, toolCallId, toolName, args})` |
| `tool_execution_update` | `emit({type, toolCallId, toolName, args, partialResult})` |
| `tool_execution_end` | `emit({type, toolCallId, toolName, result, isError, ...(durationMs===undefined?{}:{durationMs})})` |

#### 2.3.9 生命周期

`subscribe(listener): () => void`：push 到 `_eventListeners`，返回从数组移除该 listener 的函数。

`_disconnectFromAgent()`：调用并清空 `_unsubscribeAgent`。

`dispose()`：

```
try { abortRetry(); abortCompaction(); abortBranchSummary(); abortBash(); agent.abort(); } catch {}
_extensionRunner.invalidate("This extension ctx is stale after session replacement or reload. Do not use a captured kiana or command ctx after ctx.newSession(), ctx.fork(), ctx.switchSession(), or ctx.reload(). For newSession, fork, and switchSession, move post-replacement work into withSession and use the ctx passed to withSession. For reload, do not use the old ctx after await ctx.reload().");
_disconnectFromAgent();
_eventListeners = [];
if (_cacheWarmer) { _cacheWarmer.onWarmed = undefined; _cacheWarmer.cancel(); }
cleanupSessionResources(this.sessionId);
```

`refreshContext()`：`_refreshFinalizedContext()`。

#### 2.3.10 只读访问器（逐条，全部为纯 getter）

| getter | 返回 |
|---|---|
| `state` | `agent.state` |
| `cacheWarmingStatus` | `_cacheWarmer?.status` |
| `model` | `agent.state.model` |
| `thinkingLevel` | `agent.state.thinkingLevel` |
| `routedModel` | 仅虚拟模型时：`latest = findLatestResponse(agent.state.messages)`；`model = getPhysicalModel(latest.provider, latest.model)`；`model && {model, thinkingLevel: latest.thinkingLevel}`；否则 undefined |
| `isStreaming` | `_isAgentRunActive` |
| `isIdle` | `!_isAgentRunActive && !isCompacting` |
| `systemPrompt` | `buildSystemPrompt(_runSystemPromptOptions ?? _baseSystemPromptOptions)` |
| `retryAttempt` | `_retryAttempt` |
| `modelRuntime` | **规格原表遗漏**：`return this._modelRuntime`（公开 getter，扩展/SDK 可直接拿 ModelRuntime） |
| `isCompacting` | 三个 AbortController 任一存在 |
| `messages` | `agent.state.messages` |
| `steeringMode` / `followUpMode` | 透传 agent |
| `sessionFile` / `sessionId` / `sessionName` | 透传 sessionManager |
| `scopedModels` | `_scopedModels` |
| `promptTemplates` | `_resourceLoader.getPrompts().prompts` |
| `pendingMessageCount` | `_steeringMessages.length + _followUpMessages.length` |
| `resourceLoader` | `_resourceLoader` |
| `extensionRunner` | `_extensionRunner` |
| `isBashRunning` | `_bashAbortControllers.size > 0` |
| `hasPendingBashMessages` | `_pendingBashMessages.length > 0` |
| `isRetrying` | `_retryAbortController !== undefined` |
| `autoRetryEnabled` | `settingsManager.getRetryEnabled()` |
| `autoCompactionEnabled` | `settingsManager.getCompactionEnabled()` |

`setCacheWarmingMode(mode)`：`settingsManager.setCacheWarmingMode(mode)`；`_cacheWarmer?.onModeChanged()`。

`setScopedModels(models)`：直接替换 `_scopedModels`。

#### 2.3.11 工具注册表与 loadout

`getActiveToolNames()`：`agent.state.tools.map(t => t.name)`。
`getCallableToolNames()`：`_getCallableTools().map(t => t.name)`。

`getAllTools(): ToolInfo[]`：遍历 `_toolDefinitions.values()`，返回
`{ name, description, parameters, promptGuidelines, exposure: _getToolExposure(name),
   ...(namespace?{namespace}:{}), ...(annotations?{annotations:{...}}:{}), ...(reachesTools?{reachesTools}:{}), sourceInfo }`。

`getToolDefinition(name)`：`_toolDefinitions.get(name)?.definition`。

`setActiveToolsByName(names)`：
- `previous = getActiveToolNames()`；`_setActiveTools(names)`；
- `active = new Set(getActiveToolNames())`；若 `previous` 中有名字不在 `active` 中 → `_pendingToolNames.clear()`。
（即「减少工具」会丢弃待激活集合，「只增加」保留。）

`_setActiveTools(names)`：`tools = _applyToolLoadout(names)`；对每个 tool 从 `_pendingToolNames` 删除；
`_rebuildSystemPrompt(tools.map(t=>t.name))`。

`_isAllowedTool(name)`：`_excludedTools?.(name)` → false；无 allowlist 或匹配 → true；
否则若 `!_allowlistFiltersMcp && isMcpToolName(name)` → true；否则 false。

`_isActivatable(name)`：无 allowlist 或匹配或非 MCP → true；否则
`_getToolExposure(name) !== "direct" && _toolRegistry.has("tool_search")`。

`_getToolExposure(name)`：`_toolDefinitions.get(name)?.definition.exposure ?? "direct"`。

`_getCallableTools(active = new Set(getActiveToolNames()))`：遍历 `_toolRegistry.values()`，
保留 `exposure === "codemode" || exposure === "deferred" || (exposure === "direct" && active.has(name))`。

`_applyToolLoadout(toolNames): AgentTool[]`：

1. `tools = [...new Set(toolNames)].flatMap(name => registry.get(name) 存在且 exposure !== "hidden" 且 _isActivatable(name) ? [tool] : [])`。
2. `hooks` = 这些工具中定义带 `prepareLoadout` 的条目。
3. 若有 hooks：
   - 构造 `ToolLoadout = { declared: tools, callable: _getCallableTools(new Set(tools.map(name))),
     registered: [...registry.values()], getExposure, getNamespace, getPromptGuidelines }`。
   - 依序对每个 hook 调用 `definition.prepareLoadout(loadout)`，收集 `descriptions`（后者覆盖前者）
     与 `hiddenDeclarations`；抛错则 `emitError({extensionPath: sourceInfo.path, event: "prepare_loadout",
     error: msg, stack})`（不中断）。
   - `declared = tools.map(t => descriptions.has(t.name) ? {...t, description} : t)`。
4. `_hiddenDeclarations = hidden`；`agent.state.tools = declared`；返回 declared。

`_normalizePromptSnippet(text?)`：空 → undefined；`replace(/[\r\n]+/g," ")` → `replace(/\s+/g," ")` → trim；
空串 → undefined。

`_normalizePromptGuidelines(list?)`：空/未给 → `[]`；逐个 trim，非空按首次出现去重（Set 插入序）。

`_rebuildSystemPrompt(toolNames)`：

```
validToolNames = toolNames.filter(n => _toolRegistry.has(n))
toolSnippets = {}; for (name of _toolRegistry.keys()) if (_toolPromptSnippets.get(name)) toolSnippets[name] = snippet
loaderSystemPrompt = resourceLoader.getSystemPrompt()
appendSystemPrompt = join(resourceLoader.getAppendSystemPrompt(), "\n\n")  // 空数组 → ""
_baseSystemPromptOptions = normalizeBuildSystemPromptOptions({
  cwd: _cwd, skills: resourceLoader.getSkills().skills, contextFiles: resourceLoader.getAgentsFiles().agentsFiles,
  customPrompt: loaderSystemPrompt, appendSystemPrompt, selectedTools: validToolNames,
  hiddenTools: [..._hiddenDeclarations], toolSnippets, toolGuidelines: Object.fromEntries(_toolPromptGuidelines),
})
```

`_preparePromptAndToolLoadout(options, messages = agent.state.messages): SystemMessage | undefined`：

```
options.selectedTools = _applyToolLoadout(options.selectedTools).map(t => t.name);
options.hiddenTools = [..._hiddenDeclarations];
sections = diffSystemPromptSections(getCurrentSystemMessage(messages)?.sections ?? {}, buildSystemPromptSections(options));
return sections ? { role:"system", content:"", sections, timestamp: Date.now() } : undefined;
```

`_restoreToolsFromTranscript(modifiers = [])`：

1. `_pendingToolNames.clear()`。
2. `systemMessages = getBranch().flatMap(e => e.type==="message" && e.message.role==="system" ? [e.message] : [])`。
3. 空 → 返回（保留当前工具）。
4. `mentioned = Set(所有 toolsAdded/toolsRemoved 的 name)`。
5. `newTools = [..._toolRegistry.keys()].filter(n => !mentioned.has(n) && !_baseToolDefinitions.has(n) && _isActivatedOnRegistration(n))`。
6. `names = applyToolModifiers([...getCurrentTools(systemMessages).map(t=>t.name), ...newTools], modifiers)`。
7. `_pendingToolNames = new Set(names.filter(n => _isAllowedTool(n)))`。
8. `_setActiveTools(names)`（未注册的名字被忽略）。

`_refreshToolRegistry(options?)`：完整重建注册表：

1. `previousActivatedOnRegistration = Set(现有 registry 中 _isActivatedOnRegistration 的名字)`。
2. `previousActiveToolNames = getActiveToolNames()`。
3. `registeredTools = extensionRunner.getAllRegisteredTools()`；
   `allCustomTools = [...registeredTools, ..._customTools.map(d => ({definition:d,
     sourceInfo: createSyntheticSourceInfo(`<sdk:${d.name}>`, {source:"sdk"})}))].filter(t => _isAllowedTool(t.definition.name))`。
4. `definitionRegistry`：先放内置（`_baseToolDefinitions` 中 `_isAllowedTool` 的，sourceInfo 为
   `createSyntheticSourceInfo(`${BUILTIN_PATH_PREFIX}${name}`, {source:"builtin"})`，`BUILTIN_PATH_PREFIX = "builtin:"`），
   再被 `allCustomTools` 覆盖（同名后者胜）。
5. `_toolDefinitions = definitionRegistry`；
   `_toolPromptSnippets` / `_toolPromptGuidelines` 由该表重建（空值不放入）。
6. `wrappedExtensionTools = wrapRegisteredTools(allCustomTools, runner)`；
   `wrappedBuiltInTools = wrapRegisteredTools(内置（过滤 allowlist）映射的同形状条目, runner)`。
7. `toolRegistry = Map(wrappedBuiltInTools)` 然后逐个 `set` wrappedExtensionTools（扩展覆盖内置同名）。
8. `nextActiveToolNames = (options?.activeToolNames ? [...options.activeToolNames] : [...previousActiveToolNames])
   .filter(n => _isAllowedTool(n))`。
9. 分支：
   - 有 allowlist：把 registry 中 `allowedTools(name) && _isDeclarable(name)` 的全部加入（未默认激活的也被命名激活；
     被保留但未被命名的 MCP 工具保持不激活）。
   - 否则若 `options?.includeAllExtensionTools`：把 wrappedExtensionTools 中 `_isActivatedOnRegistration` 的加入。
   - 否则若未给 `activeToolNames`：把 registry 中「之前未 activatedOnRegistration 而现在 activatedOnRegistration」
     的加入（新注册工具的自动激活）。
10. `nextActiveToolNames.push(..._pendingToolNames)`。
11. `_setActiveTools([...new Set(nextActiveToolNames)])`。

`_isDeclarable(name)`：exposure ∈ {`direct`, `model-only`}。
`_isActivatedOnRegistration(name)`：`_isDeclarable(name) && definition.defaultActive !== false`。

`_buildRuntime(options)`：

1. `autoResizeImages = settingsManager.getImageAutoResize()`；`shellCommandPrefix`；`shellPath`。
2. `baseToolDefinitions = _baseToolsOverride
   ? Object.fromEntries(entries.map(([n,t]) => [n, createToolDefinitionFromAgentTool(t)]))
   : createAllToolDefinitions(_cwd, { read: { autoResizeImages }, bash: { commandPrefix, shellPath } })`。
3. `_baseToolDefinitions = Map(entries)`。
4. `extensionsResult = resourceLoader.getExtensions()`；若 `options.flagValues`：逐个写入 `extensionsResult.runtime.flagValues`。
5. `_extensionRunner = new ExtensionRunner(extensionsResult.extensions, extensionsResult.runtime, _cwd, sessionManager, new ModelRegistry(_modelRuntime))`；
   `_extensionRunnerRef?.current = _extensionRunner`；`_bindExtensionCore`；`_applyExtensionBindings`。
6. `defaultActiveToolNames = _baseToolsOverride ? Object.keys(_baseToolsOverride) : ["read","bash","edit","write"]`；
   `baseActiveToolNames = options.activeToolNames ?? defaultActiveToolNames`；
   `_refreshToolRegistry({ activeToolNames: baseActiveToolNames, includeAllExtensionTools: options.includeAllExtensionTools })`。

`reload(options?)`：

1. `oldRunner = _extensionRunner`；`previousFlagValues = oldRunner.getFlagValues()`。
2. `await emitSessionShutdownEvent(oldRunner, {type:"session_shutdown", reason:"reload"})`；`oldRunner.invalidate()`。
3. `getDefaultTools = () => _usesDefaultTools ? applyToolModifiers(settingsManager.getDefaultTools() ?? DEFAULT_TOOL_NAMES, _defaultToolModifiers) : []`
   （`DEFAULT_TOOL_NAMES = ["read","bash","edit","write"]`）。
4. `previousDefaultTools = new Set(getDefaultTools())`；`await settingsManager.reload()`；`syncQueueModesFromSettings()`；
   `resetApiProviders()`；`await resourceLoader.reload()`。
5. `addedDefaultTools = getDefaultTools().filter(n => !previousDefaultTools.has(n))`。
6. 对 `getActiveToolNames()` 每个名字加入 `_pendingToolNames`。
7. `_buildRuntime({ activeToolNames: [...getActiveToolNames(), ...addedDefaultTools], flagValues: previousFlagValues,
   includeAllExtensionTools: true })`。
8. `hasBindings = _extensionUIContext || _extensionCommandContextActions || _extensionShutdownHandler || _extensionErrorListener`；
   若有：`await options?.beforeSessionStart?.()`；`await _extensionRunner.emit({type:"session_start", reason:"reload"})`；
   `reportUnhandledMcpServers()`；`await extendResourcesFromExtensions("reload")`。

`bindExtensions(bindings)`：
- 逐个可选字段赋值（`!== undefined` 才覆盖）。
- `_applyExtensionBindings(_extensionRunner)`；
- `await _extensionRunner.emit(_sessionStartEvent)`；
- `_extensionRunner.reportUnhandledMcpServers()`；
- `await extendResourcesFromExtensions(_sessionStartEvent.reason === "reload" ? "reload" : "startup")`。

`extendResourcesFromExtensions(reason)`：无 `resources_discover` handler 直接返回；
`{skillPaths, promptPaths, themePaths} = await emitResourcesDiscover(_cwd, reason)`；三者皆空返回；
`resourceLoader.extendResources({skillPaths: buildExtensionResourcePaths(skillPaths), ...})`；
`_rebuildSystemPrompt(getActiveToolNames())`。

`buildExtensionResourcePaths(entries)`：每项
`{ path, metadata: { source: getExtensionSourceLabel(extensionPath), scope: "temporary", origin: "top-level",
   baseDir: isSyntheticPath(extensionPath) ? undefined : dirname(extensionPath) } }`。

`getExtensionSourceLabel(extensionPath)`：合成路径 → `extension:${path.replace(/[<>]/g,"")}`；
否则 `extension:${basename(path).replace(/\.(ts|js)$/,"")}`。

`_applyExtensionBindings(runner)`：`setUIContext(_extensionUIContext, _extensionMode)`；`bindCommandContext(_extensionCommandContextActions)`；
先退订旧 error listener，再按需 `onError(_extensionErrorListener)`。

`_refreshCurrentModelFromRegistry()`：当前模型存在时 `refreshed = modelRuntime.getModel(provider, id)`；
存在且不是同一对象 → `agent.state.model = refreshed`。

`_bindExtensionCore(runner)`：向 `runner.bindCore(coreActions, contextActions, providerActions)` 注入。
详见 2.3.12。

#### 2.3.12 扩展 bindCore 注入面（逐条）

`getCommands()`：`extensionCommands`（name=`invocationName`, description, source:"extension", sourceInfo）
+ `templates`（source:"prompt"）+ `skills`（name=`skill:${name}`, source:"skill"），按此顺序拼接。

core actions：
- `sendMessage(msg, opts)` → `sendCustomMessage(...).catch(err => emitError({extensionPath:"<runtime>", event:"send_message", error}))`
- `sendUserMessage(content, opts)` → 同上，event 为 `send_user_message`
- `appendEntry(customType, data)` → `appendCustomEntry` 后 `getEntry` 并 `_emit({type:"entry_appended", entry})`
- `setSessionName(name)` → `setSessionName(name)`
- `getSessionName()` → `sessionManager.getSessionName()`
- `setLabel(entryId, label)` → `sessionManager.appendLabelChange(entryId, label)`
- `getActiveTools()` → `getActiveToolNames()`
- `getAllTools()` → `getAllTools()`
- `getSettings()` → `settingsManager.getSettings()`
- `setActiveTools(names)` → `setActiveToolsByName(names)`
- `refreshTools()` → `_refreshToolRegistry()`
- `getCommands()`
- `setModel(model)` → 无配置鉴权返回 false；否则 `await setModel(model)` 返回 true
- `getThinkingLevel()` → `thinkingLevel`
- `setThinkingLevel(level)` → `setThinkingLevel(level)`

context actions：
- `getModel()`、`getScopedModels()`、`isIdle()`、`isProjectTrusted()` → `settingsManager.isProjectTrusted()`
- `getSignal()` → `agent.signal`
- `abort()` → 有 `_extensionAbortHandler` 则调它，否则 `void this.abort()`
- `hasPendingMessages()` → `pendingMessageCount > 0`
- `shutdown()` → `_extensionShutdownHandler?.()`
- `getContextUsage()`、`getSystemPrompt()`、`getSystemPromptOptions()` → `_baseSystemPromptOptions`
- `compact(options)` → 异步执行 `compact(customInstructions)`，成功 `onComplete(result)`，失败 `onError(err)`
- `executeTool(callerId, name, args, options)` → `_executeNestedToolCall`
- `getCallableTools()` → `_getCallableTools()`

provider actions（全部随后调用 `_refreshCurrentModelFromRegistry()`）：
`registerProvider(name, config)`、`registerNativeProvider(provider)`、`unregisterProvider(name)`、
`registerVirtualModel(definition)`、`unregisterVirtualModel(provider, id)`。

#### 2.3.13 运行循环

`_startRun(trigger, onStarted?)`：

```
_isAgentRunActive = true; _agentRunAbortRequested = false;
try { messages = await _prepareRun(trigger); }
catch (e) { _isAgentRunActive = false; _flushPendingBashMessages(); _flushPendingCustomMessages();
            _resolveIdleWaitIfIdle(); throw e; }
_flushPendingBashMessages(); _flushPendingCustomMessages(); onStarted?.();
_failedResponse = undefined; _recordSelection(); _pendingToolNames.clear();
try {
  await agent.prompt(messages);
  while (!_agentRunAbortRequested) {
    if (await _handlePostAgentRun()) { if (_agentRunAbortRequested) break; await agent.continue(); continue; }
    if (_agentRunAbortRequested || !(await _runBeforeSettleBoundary())) break;
    if (_agentRunAbortRequested) break;
    await agent.continue();
  }
} finally {
  if (_agentRunAbortRequested) _finishCancelledRetry();
  _failedResponse = undefined; _runSystemPromptOptions = undefined;
  _flushPendingBashMessages(); _flushPendingCustomMessages();
  await _emitAgentSettled();
}
```

`_prepareRun(trigger): Promise<AgentMessage[]>`：

1. `lastAssistant = _findLastAssistantMessage()`；存在 → `await _checkCompaction(lastAssistant, false)`。
2. `selectedToolsBefore = _baseSystemPromptOptions.selectedTools`；
   `result = await extensionRunner.emitBeforeAgentStart(trigger, _baseSystemPromptOptions)`。
3. `handlerEditedTools` = 长度不同或任一位置不同；为假时 `result.systemPromptOptions.selectedTools = getActiveToolNames()`。
4. 构造起始消息：
   - `trigger.messageType === "custom"` → push `trigger.message`。
   - 否则：`normalized = await _normalizePromptImages(trigger.images)`；
     `text = normalized.hints.length > 0 ? `${trigger.prompt}\n\n${normalized.hints.join("\n")}` : trigger.prompt`；
     push `{role:"user", content:[{type:"text",text}, ...normalized.images], timestamp: Date.now()}`。
5. `messages.push(..._pendingNextTurnMessages)`；`_pendingNextTurnMessages = []`。
6. 对 `result.messages` 每个 → push `{role:"custom", customType, content: msg.content ?? [], display, details, timestamp: Date.now()}`。
7. `updateMessage = _preparePromptAndToolLoadout(result.systemPromptOptions)`；
   `_runSystemPromptOptions = result.systemPromptOptions`；若有 updateMessage `messages.unshift(updateMessage)`。
8. 返回 messages。

`_normalizePromptImages(images?)`：无 images → `{images:[], hints:[]}`；逐个
`processImage(Buffer.from(image.data,"base64"), image.mimeType, {autoResizeImages: settingsManager.getImageAutoResize(),
resizeOptions: _limitsModel()?.inputLimits?.images?.resize})`：
失败（`!ok`）→ `hints.push(processed.message)` 并跳过；成功 → push 归一化 image，并 `hints.push(...processed.hints)`。

`_handlePostAgentRun(): Promise<boolean>`：

```
message = _lastAssistantMessage; _lastAssistantMessage = undefined;
if (_agentRunAbortRequested) { _finishCancelledRetry(); return false; }
if (!message) return agent.hasQueuedMessages();
if (_isRetryableError(message) && await _prepareRetry(message)) {
  if (_agentRunAbortRequested) _finishCancelledRetry();
  _failedResponse = message;
  return !_agentRunAbortRequested;
}
if (_agentRunAbortRequested) { _finishCancelledRetry(); return false; }
if (message.stopReason === "error" && _retryAttempt > 0) {
  _emit({type:"auto_retry_end", success:false, attempt:_retryAttempt, finalError: message.errorMessage});
  _retryAttempt = 0;
}
if (await _checkCompaction(message)) return !_agentRunAbortRequested;
return !_agentRunAbortRequested && agent.hasQueuedMessages();
```

`_runBeforeSettleBoundary(): Promise<boolean>`：

```
if (!extensionRunner.hasHandlers("agent_before_settle")) return agent.hasQueuedMessages();
_isBeforeSettle = true; _abortDuringBeforeSettle = false;
try {
  result = await emitBoundary({type:"agent_before_settle", outcome:_lastActivityOutcome},
              entries => _buildBoundaryContext(entries, "agent_before_settle"));
  _commitBoundaryDrafts(result.entries);
  _flushPendingCustomMessages();
  finalContext = _buildBoundaryContext([], "agent_before_settle");
  if (_abortDuringBeforeSettle) return false;
  shouldContinue = result.continue || agent.hasQueuedMessages();
  if (shouldContinue && !finalContext.canContinue) {
    if (result.continue) _reportInvalidBoundaryContinuation("agent_before_settle");
    return false;
  }
  return shouldContinue;
} finally { _isBeforeSettle = false; }
```

`_runInputHandlers(text, images, source, streamingBehavior?)`：无 `input` handler → `{text, images}`；
`emitInput(text, images, source, streamingBehavior)`：
`action === "handled"` → undefined；`"transform"` → `{text: inputResult.text, images: inputResult.images ?? images}`；
其它 → `{text, images}`。

#### 2.3.14 prompt / steer / followUp / 自定义消息

`prompt(text, options?)`（完整顺序）：

```
1. 若 _isEmittingAgentSettled：_deferredSettledActions.push(() => this.prompt(text, options)); return;
2. expandPromptTemplates = options?.expandPromptTemplates ?? true; preflightResult = options?.preflightResult;
3. 若 expandPromptTemplates && text.startsWith("/")：
     handled = await _tryExecuteExtensionCommand(text);
     若 handled → preflightResult?.("handled"); return;
4. 若 _compactionAbortController !== undefined → 抛
   "Cannot submit a prompt while compaction is in progress. Wait for compaction to finish and retry."
5. processedInput = await _runInputHandlers(text, options?.images, options?.source ?? "interactive",
     isStreaming ? options?.streamingBehavior : undefined);
   若为 undefined → preflightResult?.("handled"); return;
6. expandedText = currentText；
   若 expandPromptTemplates: expandedText = _expandSkillCommand(expandedText)；
                            expandedText = expandPromptTemplate(expandedText, [...promptTemplates]);
7. 若 isStreaming：
     若 !options?.streamingBehavior → 抛
       "Agent is already processing. Specify streamingBehavior ('steer' or 'followUp') to queue the message."
     behavior === "followUp" → _queueFollowUp(expandedText, images) 否则 _queueSteer(...)
     preflightResult?.("queued"); return;
8. 若 !model → 抛 formatNoModelSelectedMessage()
9. hasConfiguredAuth = modelRuntime.hasConfiguredAuth(model.provider) || (await modelRuntime.checkAuth(model.provider)) !== undefined
   若 !hasConfiguredAuth：
     若 modelRuntime.isUsingOAuth(provider) → 抛 Authentication failed ... （见 3.1）
     否则抛 formatNoApiKeyFoundMessage(provider)
10. await _startRun({messageType:"user", prompt: expandedText, images: currentImages},
                   () => preflightResult?.("started"));
```

`_tryExecuteExtensionCommand(text)`：
- `spaceIndex = text.indexOf(" ")`；`commandName = spaceIndex === -1 ? text.slice(1) : text.slice(1, spaceIndex)`；
  `args = spaceIndex === -1 ? "" : text.slice(spaceIndex+1)`。
- 无此命令 → false。
- `ctx = extensionRunner.createCommandContext()`；`await command.handler(args, ctx)` → true；
  抛错 → `emitError({extensionPath: \`command:${commandName}\`, event:"command", error})` → **仍返回 true**。

`_expandSkillCommand(text)`：
- 不以 `/skill:` 开头 → 原样返回。
- `spaceIndex = text.indexOf(" ")`；`skillName = spaceIndex===-1 ? text.slice(7) : text.slice(7, spaceIndex)`；
  `args = spaceIndex===-1 ? "" : text.slice(spaceIndex+1).trim()`。
- 找不到 skill → 原样返回。
- 读取 `skill.filePath`（UTF-8），`body = stripFrontmatter(content).trim()`；
  `skillBlock = `<skill name="${skill.name}" location="${skill.filePath}">\nReferences are relative to ${skill.baseDir}.\n\n${body}\n</skill>``；
  `args` 非空 → `${skillBlock}\n\n${args}`，否则 `skillBlock`。
- 读失败 → `emitError({extensionPath: skill.filePath, event:"skill_expansion", error})`；返回**原文**。

`_queueUserInput(text, images, behavior, source): Promise<QueuedInputDisposition>`：
1. 以 `/` 开头 → `_throwIfExtensionCommand(text)`。
2. `processedInput = _runInputHandlers(text, images, source, isStreaming ? behavior : undefined)`；
   undefined → "handled"。
3. `expandedText = _expandSkillCommand(processedInput.text)`；再 `expandPromptTemplate(...)`。
4. behavior 分派到 `_queueSteer` / `_queueFollowUp`；返回 "queued"。

`steer(text, images?, options?: {source?})` → `_queueUserInput(text, images, "steer", options?.source ?? "interactive")`。
`followUp(...)` 同理，behavior 为 "followUp"。

`_queueSteer(text, images?)`：`_steeringMessages.push(text)`；`_emitQueueUpdate()`；
`content = [{type:"text",text}, ...(images ?? [])]`；`agent.steer({role:"user", content, timestamp: Date.now()})`。
`_queueFollowUp` 同构，用 `agent.followUp`。

`_throwIfExtensionCommand(text)`：解析同样的命令名；若存在 → 抛
`Extension command "/${commandName}" cannot be queued. Use prompt() or execute the command when not streaming.`

`sendCustomMessage(message, options?)`（四/五分支，顺序严格）：

```
appMessage = { role:"custom", customType, content: message.content ?? [], display, details, timestamp: Date.now() }
if (options?.deliverAs === "nextTurn") _pendingNextTurnMessages.push(appMessage)
else if (isStreaming && options?.triggerTurn !== false) {
  if (options?.deliverAs === "followUp") agent.followUp(appMessage) else agent.steer(appMessage)
}
else if (options?.triggerTurn) {
  if (_isEmittingAgentSettled) { _deferredSettledActions.push(() => this.sendCustomMessage(message, options)); return; }
  await _startRun({messageType:"custom", message: appMessage})
}
else if (isStreaming) _pendingCustomMessages.push(appMessage)
else _appendCustomMessage(appMessage)
```

`_appendCustomMessage(appMessage)`：`appendCustomMessageEntry(...)`；`_refreshFinalizedContext()`；
`_emit({type:"message_start", message})`；`_emit({type:"message_end", message})`。

`_flushPendingCustomMessages()`：为空直接返回；取出并清空，逐个 `_appendCustomMessage`。

`sendUserMessage(content, options?)`：
- 字符串 → `text = content`，无 images。
- 数组 → text 块以 `"\n"` join；image 块收集；无 image 时 `images = undefined`。
- `await prompt(text, { expandPromptTemplates: options?.expandPromptTemplates ?? false,
  streamingBehavior: options?.deliverAs, images, source: "extension" })`。

`clearQueue(): {steering, followUp}`：拷贝两数组、清空、`agent.clearAllQueues()`、`_emitQueueUpdate()`、返回拷贝。

`getSteeringMessages()` / `getFollowUpMessages()`：直接返回**内部数组本身**（不是拷贝，
类型标注为 `readonly string[]` 但运行时可变；调用方改写会污染队列）。`pendingMessageCount` 只是两数组长度之和。

#### 2.3.15 abort / waitForIdle

`abort()`：`_isAgentRunActive` → `_agentRunAbortRequested = true`；`abortRetry()`；`abortCompaction()`；
`abortBranchSummary()`；若 `_isBeforeSettle` → `_abortDuringBeforeSettle = true`；`agent.abort()`；`await waitForIdle()`。

`waitForIdle()`：`isIdle` 直接返回；否则 await `_getIdleWaitPromise()`。

#### 2.3.16 模型与 thinking

`_emitModelSelect(nextModel, previousModel, source)`：`modelsAreEqual(previous, next)` 时返回；
否则 `await extensionRunner.emit({type:"model_select", model: nextModel, previousModel, source})`。

`setModel(model, options = {})`：
1. `!(await modelRuntime.checkAuth(model.provider))` → 抛 `No API key for ${provider}/${id}`。
2. `previousModel = this.model`；`thinkingLevel = _getThinkingLevelForModelSwitch(model)`。
3. `agent.state.model = model`；`sessionManager.appendModelChange(provider, id)`。
4. `options.persist` → `settingsManager.setDefaultModelAndProvider(provider, id)`；`_addPersistedDefaultToNonEmptyScope(model)`。
5. `setThinkingLevel(thinkingLevel)`。
6. `await _emitModelSelect(model, previousModel, "set")`。

`_addPersistedDefaultToNonEmptyScope(model)`：
- `_scopedModels.length === 0` → 返回。
- 已有相等模型 → 返回。
- `_scopedModels = [..._scopedModels, {model}]`。
- `enabledModels = settingsManager.getEnabledModels()`；空/未定义 → 返回。
- `modelReference = `${provider}/${id}``；已有大小写不敏感相等项 → 返回；否则 `setEnabledModels([...enabledModels, modelReference])`。

`cycleModel(direction = "forward", options = {})`：`_scopedModels.length > 0` → `_cycleScopedModel`；否则 `_cycleAvailableModel`。

`_cycleScopedModel(direction, options)`：
1. `availableIds = Set(modelRuntime.getAvailableSnapshot().map(m => `${provider}\0${id}`))`（**分隔符是 NUL**）。
2. `scopedModels = _scopedModels.filter(s => availableIds.has(...))`；`length <= 1` → undefined。
3. `currentIndex = scopedModels.findIndex(s => modelsAreEqual(s.model, currentModel))`；-1 → 0。
4. `nextIndex = forward ? (i+1)%len : (i-1+len)%len`；`next = scopedModels[nextIndex]`。
5. `thinkingLevel = _getThinkingLevelForModelSwitch(next.model, next.thinkingLevel)`。
6. 应用模型（同 setModel 的 3–5 步）；`await _emitModelSelect(..., "cycle")`。
7. 返回 `{model: next.model, thinkingLevel: this.thinkingLevel, isScoped: true}`。

`_cycleAvailableModel(direction, options)`：`availableModels = getAvailableSnapshot()`；<=1 → undefined；
其余同构，`thinkingLevel = _getThinkingLevelForModelSwitch(nextModel)`；返回 `isScoped: false`。

`setThinkingLevel(level, options = {})`：
1. `availableLevels = getAvailableThinkingLevels()`；
   `effectiveLevel = availableLevels.includes(level) ? level : _clampThinkingLevel(level, availableLevels)`。
2. `previousLevel = agent.state.thinkingLevel`；`isChanging = effectiveLevel !== previousLevel`。
3. `agent.state.thinkingLevel = effectiveLevel`。
4. `options.persist` → `settingsManager.setDefaultThinkingLevel(level)`（**注意存的是请求值，不是 clamp 后值**）。
5. 若 changing：`appendThinkingLevelChange(effectiveLevel)`；`_emit({type:"thinking_level_changed", level})`；
   `void extensionRunner.emit({type:"thinking_level_select", level, previousLevel})`（不 await）。

`cycleThinkingLevel(options = {})`：不支持 thinking → undefined；`levels = getAvailableThinkingLevels()`；
`nextIndex = (indexOf(thinkingLevel)+1) % levels.length`（找不到时 indexOf=-1 → 0）；`setThinkingLevel(next, options)`；返回 next。

`getAvailableThinkingLevels()`：无模型 → `[...THINKING_LEVEL_OPTIONS]`（`["off","minimal","low","medium","high","xhigh","max"]`）；
否则 `getSupportedThinkingLevels(model)`。

`supportsThinking()`：`!!model?.reasoning`。

`_getThinkingLevelForModelSwitch(targetModel?, explicitLevel?)`：显式 → 用之；目标模型有 per-model 设置 → 用之；
否则 `settingsManager.getDefaultThinkingLevel() ?? this.thinkingLevel ?? DEFAULT_THINKING_LEVEL`（`"medium"`）。

`_clampThinkingLevel(level, _availableLevels)`：有模型 → `clampThinkingLevel(model, level)`；否则 `"off"`。

`syncQueueModesFromSettings()`：`agent.steeringMode = settingsManager.getSteeringMode()`；
`agent.followUpMode = settingsManager.getFollowUpMode()`。

`setSteeringMode(mode)` / `setFollowUpMode(mode)`：设置 agent 模式并写 settings。

#### 2.3.17 压缩

`_runDefaultCompaction(preparation, model, customInstructions, signal, reason)`：
`request = await _getSummarizationRequestAuth(model, signal)`；调用 `compact(preparation, request.model,
request.apiKey, request.headers, customInstructions, signal, request.thinkingLevel, agent.streamFunction,
request.env, settingsManager.getRetrySettings(), _summarizationRetryCallbacks({source:"compaction", reason}),
undefined /* sessionId */)`。

`compact(customInstructions?)`（手动，**完整状态机**）：

```
1. await this.abort()
2. _compactionAbortController = new AbortController(); _emit({type:"compaction_start", reason:"manual"});
   fromExtension = false; cancelledByExtension = false;
3. try {
   model = this.model; if (!model) throw formatNoModelSelectedMessage();
   settings = getCompactionSettings(model); pathEntries = getBranch();
   preparation = prepareCompaction(pathEntries, settings);
   if (!preparation) {
     lastEntry = pathEntries.at(-1);
     if (lastEntry?.type === "compaction") throw new Error("Already compacted");
     throw new Error("Nothing to compact (session too small)");
   }
   if (hasHandlers("session_before_compact")) {
     result = await emit({type:"session_before_compact", preparation, branchEntries: pathEntries,
                          customInstructions, reason:"manual", willRetry:false, signal});
     if (result?.cancel) { cancelledByExtension = true; throw new Error("Compaction cancelled"); }
     if (result?.compaction) { extensionCompaction = result.compaction; fromExtension = true; }
   }
   若 extensionCompaction：取 summary/firstKeptEntryId/tokensBefore/usage/details
   否则：result = await _runDefaultCompaction(preparation, model, customInstructions, signal, "manual")，取其字段
   if (signal.aborted) throw new Error("Compaction cancelled");
   appendCompaction(summary, firstKeptEntryId, tokensBefore, details, fromExtension, usage);
   newEntries = getEntries(); _refreshFinalizedContext();
   estimatedTokensAfter = estimateMessagesTokens(buildSessionProjection().messages);
   savedCompactionEntry = newEntries.find(e => e.type === "compaction" && e.summary === summary);
   if (extensionRunner && savedCompactionEntry) await emit({type:"session_compact", compactionEntry,
        fromExtension, reason:"manual", willRetry:false});
   compactionResult = { summary, firstKeptEntryId, tokensBefore, estimatedTokensAfter, usage, details };
   _clearManualCompactionState();
   _emit({type:"compaction_end", reason:"manual", result: compactionResult, aborted:false, willRetry:false});
   return compactionResult;
 } catch (error) {
   message = error instanceof Error ? error.message : String(error);
   aborted = signal.aborted || cancelledByExtension;
   errorMessage = aborted ? undefined : `Compaction failed: ${message}`;
   _clearManualCompactionState();
   _emit({type:"compaction_end", reason:"manual", result: undefined, aborted, willRetry:false, errorMessage});
   await _emitSessionCompactFailed({reason:"manual", errorMessage, aborted, willRetry:false, fromExtension});
   throw error;
 } finally { _clearManualCompactionState(); }
```

`_clearManualCompactionState()`：`_compactionAbortController = undefined`；`_resolveIdleWaitIfIdle()`。

`abortCompaction()`：中止手动与自动两个 controller。`abortBranchSummary()`：中止分支摘要。

`_checkCompaction(assistantMessage, skipAbortedCheck = true): Promise<boolean>`（**判定链，逐条**）：

```
settings = getCompactionSettings(this.model); if (!settings.enabled) return false;
if (skipAbortedCheck && stopReason === "aborted") return false;
messageModel = _modelForMessage(assistantMessage); sameModel = messageModel !== undefined;
contextWindow = (messageModel ?? this.model)?.contextWindow ?? 0;
compactionEntry = getLatestCompactionEntry(getBranch());
assistantIsFromBeforeCompaction = compactionEntry !== null &&
    assistantMessage.timestamp <= new Date(compactionEntry.timestamp).getTime();
if (assistantIsFromBeforeCompaction) return false;

currentProjection = buildSessionProjection();
assistantEntryId = _findPersistedMessageEntryId(assistantMessage);
assistantIsProjected = assistantEntryId === undefined ||
    currentProjection.entries.some(e => e.sourceEntry.id === assistantEntryId &&
        e.messages.some(m => m.role === "assistant"));
branch = getBranch();
assistantIndex = assistantEntryId ? branch.findIndex(e => e.id === assistantEntryId) : -1;
entriesAfterAssistant = assistantIndex >= 0 ? branch.slice(assistantIndex+1) : [];
hasPostAssistantContextEdit = entriesAfterAssistant.some(e => e.type === "context_edit");
latestAssistantEdit = entriesAfterAssistant.filter(e => e.type === "context_edit" && e.targetId === assistantEntryId).at(-1);
assistantRetainedForExplicitRecovery = assistantEntryId === undefined ||
    (!entriesAfterAssistant.some(e => e.type === "compaction") && latestAssistantEdit?.replacement !== null);
assistantUsageMatchesProjection = assistantIsProjected && !hasPostAssistantContextEdit;
explicitOverflow = stopReason === "error" && isContextOverflow(assistantMessage);
contextOverflow = sameModel && ((explicitOverflow && assistantRetainedForExplicitRecovery) ||
    (assistantUsageMatchesProjection && isContextOverflow(assistantMessage, contextWindow)));
recoverableLength = sameModel && assistantIsProjected && isRecoverableLength(assistantMessage, messageModel.maxTokens);

if (contextOverflow || recoverableLength) {
  willRetry = stopReason !== "stop";
  if (!willRetry) return await _runAutoCompaction("overflow", false);
  if (_overflowRecoveryAttempted) {
    errorMessage = contextOverflow
      ? "Context overflow recovery failed after one compact-and-retry attempt. Try reducing context or switching to a larger-context model."
      : "Truncated response recovery failed after one compact-and-retry attempt.";
    _emit({type:"compaction_end", reason:"overflow", result:undefined, aborted:false, willRetry:false, errorMessage});
    await _emitSessionCompactFailed({reason:"overflow", errorMessage, aborted:false, willRetry:false, fromExtension:false});
    return false;
  }
  _overflowRecoveryAttempted = true;
  _omitRecoveryAttempt(assistantMessage);
  retry = await _runAutoCompaction("overflow", willRetry);
  if (retry) _failedResponse = assistantMessage;
  return retry;
}

// 阈值分支
projection = currentProjection;
hasContextEdits = projection.entries.some(e => e.sourceEntry.type === "context_edit");
directContextTokens = assistantMessage.usage ? calculateContextTokens(usage) : 0;
if (hasContextEdits) contextTokens = estimateProjectedContextTokens(projection, branch).tokens;
else if (stopReason === "error" || directContextTokens === 0) {
  messages = agent.state.messages; estimate = estimateContextTokens(messages);
  if (estimate.lastUsageIndex !== null) {
    usageMsg = messages[estimate.lastUsageIndex];
    if (compactionEntry && usageMsg.role === "assistant" &&
        usageMsg.timestamp <= new Date(compactionEntry.timestamp).getTime()) return false;
  }
  contextTokens = estimate.tokens;
} else contextTokens = directContextTokens;
if (shouldCompact(contextTokens, contextWindow, settings)) return await _runAutoCompaction("threshold", false);
return false;
```

`_runAutoCompaction(reason, willRetry): Promise<boolean>`（**完整状态机**）：

```
model = this.model; settings = getCompactionSettings(model);
abortController = undefined; started = false; fromExtension = false; cancelledByExtension = false;
try {
  if (!model) return false;
  pathEntries = getBranch(); preparation = prepareCompaction(pathEntries, settings);
  if (!preparation) return false;            // 不启动、不发事件
  abortController = new AbortController(); _autoCompactionAbortController = abortController;
  started = true; _emit({type:"compaction_start", reason}); abortController.signal.throwIfAborted();
  if (hasHandlers("session_before_compact")) {
    extensionResult = await emit({type:"session_before_compact", preparation, branchEntries: pathEntries,
        customInstructions: undefined, reason, willRetry, signal});
    if (extensionResult?.cancel) { cancelledByExtension = true; throw new Error("Compaction cancelled"); }
    if (extensionResult?.compaction) { extensionCompaction = ...; fromExtension = true; }
  }
  abortController.signal.throwIfAborted();
  取或生成 summary/firstKeptEntryId/tokensBefore/usage/details（extension 或 _runDefaultCompaction(preparation, model, undefined, signal, reason)）
  abortController.signal.throwIfAborted();
  appendCompaction(...); newEntries = getEntries(); _refreshFinalizedContext();
  estimatedTokensAfter = estimateMessagesTokens(buildSessionProjection().messages);
  savedCompactionEntry = newEntries.find(e => e.type === "compaction" && e.summary === summary);
  if (extensionRunner && savedCompactionEntry) await emit({type:"session_compact", compactionEntry, fromExtension, reason, willRetry});
  result = { summary, firstKeptEntryId, tokensBefore, estimatedTokensAfter, usage, details };
  _emit({type:"compaction_end", reason, result, aborted:false, willRetry});
  if (willRetry) return true;
  return agent.hasQueuedMessages();
} catch (error) {
  message = error instanceof Error ? error.message : "compaction failed";
  aborted = abortController?.signal.aborted === true || cancelledByExtension;
  if (started) {
    errorMessage = aborted ? undefined : reason === "overflow"
        ? `Context overflow recovery failed: ${message}` : `Auto-compaction failed: ${message}`;
    _emit({type:"compaction_end", reason, result:undefined, aborted, willRetry:false, errorMessage});
    await _emitSessionCompactFailed({reason, errorMessage, aborted, willRetry:false, fromExtension});
  }
  return false;
} finally {
  if (_autoCompactionAbortController === abortController) _autoCompactionAbortController = undefined;
  _resolveIdleWaitIfIdle();
}
```

`setAutoCompactionEnabled(enabled)`：`settingsManager.setCompactionEnabled(enabled)`。

#### 2.3.18 自动重试

`_isRetryableError(message)`：若 `isContextOverflow(message, (_modelForMessage(message) ?? this.model)?.contextWindow ?? 0)`
→ false；否则 `isRetryableAssistantError(message)`。

`_summarizationRetryCallbacks(source)`：返回三回调，分别发出
`summarization_retry_scheduled` / `summarization_retry_attempt_start`（展开 source） / `summarization_retry_finished`。

`_finishCancelledRetry()`：`_retryAttempt === 0` 返回；`attempt = _retryAttempt`；置 0；
`_emit({type:"auto_retry_end", success:false, attempt, finalError:"Retry cancelled"})`。

`_prepareRetry(message): Promise<boolean>`：

```
settings = getRetrySettings(); if (!settings.enabled) return false;
_retryAttempt++;
if (_retryAttempt > settings.maxRetries) { _retryAttempt--; return false; }
delayMs = retryDelayMs(settings, _retryAttempt);
_emit({type:"auto_retry_start", attempt:_retryAttempt, maxAttempts:settings.maxRetries, delayMs,
       errorMessage: message.errorMessage || "Unknown error"});
_omitRecoveryAttempt(message);
_retryAbortController = new AbortController();
try { await sleep(delayMs, signal); }
catch { _finishCancelledRetry(); return false; }
finally { _retryAbortController = undefined; }
return true;
```

`abortRetry()`：`_retryAbortController?.abort()`。
`setAutoRetryEnabled(enabled)`：`settingsManager.setRetryEnabled(enabled)`。

#### 2.3.19 bash 执行

`executeBash(command, onChunk?, options?: {excludeFromContext?, id?, operations?}): Promise<BashResult>`：

```
abortController = new AbortController(); _bashAbortControllers.add(abortController);
prefix = settingsManager.getShellCommandPrefix(); shellPath = settingsManager.getShellPath();
resolvedCommand = prefix ? `${prefix}\n${command}` : command;
try {
  result = await executeBashWithOperations(resolvedCommand, sessionManager.getCwd(),
    options?.operations ?? createLocalBashOperations({ shellPath }),
    { onChunk: delta => { onChunk?.(delta); _emit({type:"bash_execution_update", id: options?.id, delta}); },
      signal: abortController.signal });
  recordBashResult(command, result, options);       // 注意记录的是「原始 command」，不含 prefix
  return result;
} finally { _bashAbortControllers.delete(abortController); }
```

`recordBashResult(command, result, options?)`：构造
`{role:"bashExecution", command, output, exitCode, cancelled, truncated, fullOutputPath, timestamp: Date.now(),
excludeFromContext: options?.excludeFromContext}`；
`isStreaming` → `_pendingBashMessages.push`；否则 `appendMessage` + `_refreshFinalizedContext()`。

`abortBash()`：对 `[..._bashAbortControllers]` 逐个 abort。
`_flushPendingBashMessages()`：为空返回；逐个 `appendMessage`；清空；`_refreshFinalizedContext()`。

#### 2.3.20 会话命名与树导航

`setSessionName(name)`：`appendSessionInfo(name)`；`event = {type:"session_info_changed", name: getSessionName()}`；
`_emit(event)`；`void extensionRunner.emit(event)`。

`navigateTree(targetId, options = {}): Promise<{editorText?, cancelled, aborted?, summaryEntry?}>`：

1. `isStreaming` → 抛 "Wait for the current response to finish before navigating the session tree."
2. `isCompacting` → 抛 "Wait for the current compaction or tree navigation to finish before navigating the session tree."
3. `oldLeafId = getLeafId()`；`targetId === oldLeafId` → `{cancelled:false}`。
4. `options.summarize && !model` → 抛 "No model available for summarization"。
5. `targetEntry = getEntry(targetId)`；不存在 → 抛 `Entry ${targetId} not found`。
6. `{entries: entriesToSummarize, commonAncestorId} = collectEntriesForBranchSummary(sessionManager, oldLeafId, targetId)`。
7. 可变 `customInstructions` / `replaceInstructions` / `label`；
   `preparation = {targetId, oldLeafId, commonAncestorId, entriesToSummarize, userWantsSummary: options.summarize ?? false,
   customInstructions, replaceInstructions, label}`。
8. `_branchSummaryAbortController = new AbortController()`；**try/finally**：finally 清空 controller 并
   `_resolveIdleWaitIfIdle()`。
9. 有 `session_before_tree` handler：
   `result = await emit({type:"session_before_tree", preparation, signal})`；
   - `result?.cancel` → 返回 `{cancelled:true}`。
   - `result?.summary && options.summarize` → 用扩展摘要（`fromExtension = true`）。
   - `result?.customInstructions !== undefined` → 覆盖；`replaceInstructions` 同；`label` 同。
10. 需要默认摘要时（`options.summarize && entriesToSummarize.length > 0 && !extensionSummary`）：
    `result = await generateBranchSummary(entriesToSummarize, { ...(await _getSummarizationRequestAuth(model!, signal)),
    signal, customInstructions, replaceInstructions, reserveTokens: settingsManager.getBranchSummarySettings().reserveTokens,
    streamFn: agent.streamFunction, retry: settingsManager.getRetrySettings(),
    callbacks: _summarizationRetryCallbacks({source:"branchSummary"}) })`；
    `result.aborted` → 返回 `{cancelled:true, aborted:true}`；`result.error` → 抛 `new Error(result.error)`；
    否则 `summaryText = result.summary`、`summaryUsage = result.usage`、
    `summaryDetails = { readFiles: result.readFiles || [], modifiedFiles: result.modifiedFiles || [] }`。
    若用扩展摘要：`summaryText/details/usage` 来自扩展。
11. 计算新 leaf：
    - 目标是 user message → `newLeafId = parentId`；`editorText = contentText(content, "")`。
    - 目标是 `custom_message` → `newLeafId = parentId`；`editorText = contentText(content, "")`。
    - 其它 → `newLeafId = targetId`。
12. 应用：
    - 有 summaryText → `summaryId = branchWithSummary(newLeafId, summaryText, summaryDetails, fromExtension, summaryUsage)`；
      `summaryEntry = getEntry(summaryId)`；`label` 存在则 `appendLabelChange(summaryId, label)`。
    - 否则 `newLeafId === null` → `resetLeaf()`；否则 `branch(newLeafId)`。
    - `label && !summaryText` → `appendLabelChange(targetId, label)`。
13. `_refreshFinalizedContext()`；`_restoreToolsFromTranscript()`。
14. `await emit({type:"session_tree", newLeafId: getLeafId(), oldLeafId, summaryEntry,
    fromExtension: summaryText ? fromExtension : undefined})`。
15. 返回 `{editorText, cancelled:false, summaryEntry}`。

`getUserMessagesForForking(): Array<{entryId, text}>`：遍历 `getEntries()`，只看 `message` 且 role 为 user；
`text = contentText(content,"")` 非空才收集。

#### 2.3.21 统计、上下文用量、导出

`getSessionStats(): SessionStats`：遍历 `getEntries()`：
- `usage` 条目 → `addUsageToTotals(entry.usage)`；
- `branch_summary` / `compaction` 且有 `usage` → 累加；
- `message`：`totalMessages++`；user → `userMessages++`；toolResult → `toolResults++` 且有 usage 累加；
  assistant → `assistantMessages++`；`content` 为数组时 `toolCalls += type==="toolCall" 的块数`；累加 `usage`。
返回 `tokens.total = input + output + cacheRead + cacheWrite`（注意不是 usage.totalTokens 之和），
`cost = usageTotals.cost`（= Σ `usage.cost.total`），`contextUsage = getContextUsage()`。

`getContextUsage(): ContextUsage | undefined`：

```
model = _limitsModel(); if (!model) return undefined;
contextWindow = model.contextWindow ?? 0; if (contextWindow <= 0) return undefined;
projection = buildSessionProjection(); branch = getBranch(); latestCompaction = getLatestCompactionEntry(branch);
if (latestCompaction) {
  projectedAssistants = Set(projection.entries.filter(e => e.messages.some(m => m.role === "assistant" &&
      m.stopReason !== "aborted" && m.stopReason !== "error" && calculateContextTokens(m.usage) > 0))
      .map(e => e.sourceEntry.id));
  compactionIndex = branch.findIndex(e => e.id === latestCompaction.id);
  hasPostCompactionUsage = branch.slice(compactionIndex+1).some(e => projectedAssistants.has(e.id));
  if (!hasPostCompactionUsage) return { tokens: null, contextWindow, percent: null };
}
estimate = estimateProjectedContextTokens(projection, branch);
return { tokens: estimate.tokens, contextWindow, percent: (estimate.tokens / contextWindow) * 100 };
```

`exportToHtml(outputPath?, options = {})`：`themeName` = 首个「非 undefined 且 `getThemeByName(candidate) !== undefined`」
的候选（先 `options.themeName`，后 `settingsManager.getTheme()`）；构造
`createToolHtmlRenderer({ getToolRenderers: name => _extensionRunner.resolveToolRenderers(name,
() => getToolDefinition(name)), theme, cwd: sessionManager.getCwd() })`；
`await exportSessionToHtml(sessionManager, this.state, { outputPath, themeName, toolRenderer })`。

`exportToJsonl(outputPath?)`：`exportSessionToJsonl(sessionManager, outputPath)`。

`summarizeForBugReport({hint?, signal})`：无模型 → 抛 `No model selected`；
`generateBugReportSummary({...(await _getSummarizationRequestAuth(model, signal)), messages: this.messages,
hint, signal, streamFn: agent.streamFunction, retry: settingsManager.getRetrySettings(), sessionId: this.sessionId})`。

`getLastAssistantText(): string | undefined`：反向找第一条 assistant（跳过 `stopReason==="aborted" && content.length===0`）；
拼接所有 text 块；`text.trim() || undefined`。

`createReplacedSessionContext()`：以 `Object.getOwnPropertyDescriptors(extensionRunner.createCommandContext())`
定义新对象，然后覆盖 `sendMessage` / `sendUserMessage` 绑定到本 session 方法。

`hasExtensionHandlers(eventType)`：透传。

### 2.4 `src/core/agent-session-runtime.ts`

#### 2.4.1 导出

- `interface CreateAgentSessionRuntimeResult extends CreateAgentSessionResult { services: AgentSessionServices;
  diagnostics: AgentSessionRuntimeDiagnostic[] }`
- `type CreateAgentSessionRuntimeFactory = (options: { cwd: string; agentDir: string; sessionManager: SessionManager;
  sessionStartEvent?: SessionStartEvent; projectTrustContext?: ProjectTrustContext }) => Promise<CreateAgentSessionRuntimeResult>`
- `class SessionImportFileNotFoundError extends Error`：`name = "SessionImportFileNotFoundError"`；
  `message = \`File not found: ${filePath}\``；只读 `filePath`。
- `class AgentSessionRuntime`
- `async function createAgentSessionRuntime(createRuntime, options): Promise<AgentSessionRuntime>`
- 再导出 services 模块的若干类型与函数（`createAgentSessionServices` 等）。

内部 `extractUserMessageText(content)`：字符串原样；数组取 `part.type === "text" && typeof part.text === "string"`
的 text 并用 **`""`（空串）** join（与 `session-manager` 的 `extractTextContent` 用空格不同）。

#### 2.4.2 `AgentSessionRuntime` 成员

字段：`rebindSession?: (s: AgentSession) => Promise<void>`、`beforeSessionInvalidate?: () => void`、
`_session`、`_services`、`readonly createRuntime`、`_diagnostics`、`_modelFallbackMessage?`。

getter：`services`、`session`、`cwd`（= `_services.cwd`）、`diagnostics`（readonly）、`modelFallbackMessage`。

`setRebindSession(fn?)`、`setBeforeSessionInvalidate(fn?)`。

`emitBeforeSwitch(reason: "new"|"resume", targetSessionFile?)`：无 `session_before_switch` handler →
`{cancelled:false}`；否则 `emit({type:"session_before_switch", reason, targetSessionFile})`，
`{cancelled: result?.cancel === true}`。

`emitBeforeFork(entryId, options: {position:"before"|"at"})`：无 handler → `{cancelled:false}`；
否则 `emit({type:"session_before_fork", entryId, ...options})`。

`teardownCurrent(reason, targetSessionFile?)`（**顺序严格**）：
1. `await this.session.abort()`（先让进行中的响应落地，含工具结果，写入旧会话）。
2. `await emitSessionShutdownEvent(this.session.extensionRunner, {type:"session_shutdown", reason, targetSessionFile})`。
3. `this.beforeSessionInvalidate?.()`（同步，不得让出事件循环——用于 host 的 UI 拆除）。
4. `this.session.dispose()`。

`apply(result)`：替换 `_session` / `_services` / `_diagnostics` / `_modelFallbackMessage`。

`finishSessionReplacement(withSession?)`：`rebindSession` 存在则 `await rebindSession(this.session)`；
`withSession` 存在则 `await withSession(this.session.createReplacedSessionContext())`。

#### 2.4.3 `switchSession(sessionPath, options?)`

```
beforeResult = await emitBeforeSwitch("resume", sessionPath);
if (beforeResult.cancelled) return beforeResult;
previousSessionFile = session.sessionFile;
sessionManager = SessionManager.open(sessionPath, undefined, options?.cwdOverride);
assertSessionCwdExists(sessionManager, this.cwd);
await teardownCurrent("resume", sessionManager.getSessionFile());
apply(await createRuntime({ cwd: sessionManager.getCwd(), agentDir: services.agentDir, sessionManager,
  sessionStartEvent: { type:"session_start", reason:"resume", previousSessionFile },
  projectTrustContext: options?.projectTrustContextFactory?.(sessionManager.getCwd()) }));
await finishSessionReplacement(options?.withSession);
return { cancelled: false };
```

#### 2.4.4 `newSession(options?)`

```
beforeResult = await emitBeforeSwitch("new");
if (cancelled) return;
previousSessionFile = session.sessionFile;
sessionDir = session.sessionManager.getSessionDir();
sessionManager = sessionManager.isPersisted() ? SessionManager.create(this.cwd, sessionDir) : SessionManager.inMemory(this.cwd);
if (options?.parentSession) sessionManager.newSession({ parentSession: options.parentSession });
await teardownCurrent("new", sessionManager.getSessionFile());
apply(await createRuntime({ cwd: this.cwd, agentDir: services.agentDir, sessionManager,
  sessionStartEvent: { type:"session_start", reason:"new", previousSessionFile } }));
if (options?.setup) { await options.setup(this.session.sessionManager); this.session.refreshContext(); }
await finishSessionReplacement(options?.withSession);
return { cancelled: false };
```

#### 2.4.5 `fork(entryId, options?)`

```
position = options?.position ?? "before";
beforeResult = await emitBeforeFork(entryId, { position });
if (cancelled) return { cancelled: true };
selectedEntry = session.sessionManager.getEntry(entryId);
if (!selectedEntry) throw new Error("Invalid entry ID for forking");
if (position === "at") targetLeafId = selectedEntry.id;
else {
  if (selectedEntry.type !== "message" || selectedEntry.message.role !== "user")
    throw new Error("Invalid entry ID for forking");
  targetLeafId = selectedEntry.parentId;
  selectedText = extractUserMessageText(selectedEntry.message.content);
}
previousSessionFile = session.sessionFile;

// A) persist 模式
if (session.sessionManager.isPersisted()) {
  currentSessionFile = session.sessionFile;
  if (!currentSessionFile) throw new Error("Persisted session is missing a session file");
  sessionDir = session.sessionManager.getSessionDir();
  if (!targetLeafId) {
    sessionManager = SessionManager.create(this.cwd, sessionDir);
    sessionManager.newSession({ parentSession: currentSessionFile });
    await teardownCurrent("fork", sessionManager.getSessionFile());
    apply(await createRuntime({cwd:this.cwd, agentDir, sessionManager,
        sessionStartEvent:{type:"session_start", reason:"fork", previousSessionFile}}));
    await finishSessionReplacement(options?.withSession);
    return { cancelled:false, selectedText };
  }
  if (!existsSync(currentSessionFile))
    throw new Error("This session has not been saved yet. Send a message before cloning or forking it.");
  sessionManager = SessionManager.open(currentSessionFile, sessionDir);
  forkedSessionPath = sessionManager.createBranchedSession(targetLeafId);
  if (!forkedSessionPath) throw new Error("Failed to create forked session");
  await teardownCurrent("fork", sessionManager.getSessionFile());
  apply(await createRuntime({cwd: sessionManager.getCwd(), ...}));
  await finishSessionReplacement(...); return { cancelled:false, selectedText };
}

// B) in-memory 模式
sessionManager = session.sessionManager;
await teardownCurrent("fork", sessionManager.getSessionFile());
if (!targetLeafId) sessionManager.newSession({ parentSession: previousSessionFile });
else sessionManager.createBranchedSession(targetLeafId);
apply(await createRuntime({ cwd: this.cwd, ... }));
await finishSessionReplacement(...);
return { cancelled:false, selectedText };
```

要点：in-memory 模式复用**同一个** SessionManager 对象（拆完再改），persist 模式新建/重开。

#### 2.4.6 `importFromJsonl(inputPath, cwdOverride?)`

```
resolvedPath = resolvePath(inputPath);
if (!existsSync(resolvedPath)) throw new SessionImportFileNotFoundError(resolvedPath);
sessionDir = session.sessionManager.getSessionDir();
if (!existsSync(sessionDir)) mkdirSync(sessionDir, { recursive: true });
destinationPath = join(sessionDir, basename(resolvedPath));
sourceAlreadyStored = resolve(destinationPath) === resolvedPath;
if (!sourceAlreadyStored) {
  { name, ext } = parse(destinationPath);
  suffix = 1;
  while (existsSync(destinationPath)) destinationPath = join(sessionDir, `${name}-${suffix++}${ext}`);
}
beforeResult = await emitBeforeSwitch("resume", destinationPath);
if (cancelled) return beforeResult;
previousSessionFile = session.sessionFile;
if (!sourceAlreadyStored) copyFileSync(resolvedPath, destinationPath, constants.COPYFILE_EXCL);
sessionManager = SessionManager.open(destinationPath, sessionDir, cwdOverride);
assertSessionCwdExists(sessionManager, this.cwd);
await teardownCurrent("resume", sessionManager.getSessionFile());
apply(await createRuntime({ cwd: sessionManager.getCwd(), ... reason:"resume", previousSessionFile }));
await finishSessionReplacement();
return { cancelled: false };
```

命名冲突：`name-1.ext`、`name-2.ext` …（从 1 开始）。

#### 2.4.7 `dispose()`

`await emitSessionShutdownEvent(extensionRunner, {type:"session_shutdown", reason:"quit"})`；
`beforeSessionInvalidate?.()`；`session.dispose()`。

#### 2.4.8 `createAgentSessionRuntime`

`assertSessionCwdExists(options.sessionManager, options.cwd)`；`result = await createRuntime(options)`；
返回 `new AgentSessionRuntime(result.session, result.services, createRuntime, result.diagnostics, result.modelFallbackMessage)`。

### 2.5 `src/core/agent-session-services.ts`

#### 2.5.1 导出

```ts
export interface AgentSessionRuntimeDiagnostic { type: "info" | "warning" | "error"; message: string }

export interface CreateAgentSessionServicesOptions {
  cwd: string;
  agentDir?: string;
  settingsManager?: SettingsManager;
  modelRuntime?: ModelRuntime;
  modelRuntimeSignal?: AbortSignal;
  extensionFlagValues?: Map<string, boolean | string>;
  resourceLoaderOptions?: Omit<DefaultResourceLoaderOptions, "cwd" | "agentDir" | "settingsManager">;
  resourceLoaderReloadOptions?: ResourceLoaderReloadOptions;
}

export interface CreateAgentSessionFromServicesOptions {
  services: AgentSessionServices;
  sessionManager: SessionManager;
  sessionStartEvent?: SessionStartEvent;
  model?: Model<any>;
  thinkingLevel?: ThinkingLevel;
  scopedModels?: Array<{ model: Model<any>; thinkingLevel?: ThinkingLevel }>;
  tools?: string[];
  excludeTools?: CreateAgentSessionOptions["excludeTools"];
  noTools?: CreateAgentSessionOptions["noTools"];
  customTools?: ToolDefinition[];
}

export interface AgentSessionServices {
  cwd: string; agentDir: string;
  modelRuntime: ModelRuntime; settingsManager: SettingsManager;
  resourceLoader: ResourceLoader; diagnostics: AgentSessionRuntimeDiagnostic[];
}

export async function createAgentSessionServices(options): Promise<AgentSessionServices>
export async function createAgentSessionFromServices(options): Promise<CreateAgentSessionResult>
```

#### 2.5.2 `applyExtensionFlagValues(resourceLoader, extensionFlagValues)`（内部）

- 无 flagValues → `[]`。
- 收集扩展注册的 flags：`extension.flags` → `Map<name, {type}>`。
- 遍历输入：
  - 未注册 → 记入 `unknownFlags`。
  - 类型 boolean → `runtime.flagValues.set(name, true)`。
  - 类型 string 且值为字符串 → `set(name, value)`。
  - 类型 string 但值不是字符串（即 boolean 形式给了无值）→ diagnostics error
    `Extension flag "--${name}" requires a value`。
- `unknownFlags` 非空 → diagnostics error
  `Unknown option${unknownFlags.length === 1 ? "" : "s"}: ${unknownFlags.map(n => `--${n}`).join(", ")}`。
- 返回 diagnostics。

#### 2.5.3 `createAgentSessionServices`

```
cwd = resolvePath(options.cwd)
agentDir = options.agentDir ? resolvePath(options.agentDir) : getAgentDir()
modelRuntime = options.modelRuntime ?? await ModelRuntime.create({ authPath: join(agentDir, "auth.json"),
                        modelsPath: join(agentDir, "models.json"), signal: options.modelRuntimeSignal })
settingsManager = options.settingsManager ?? SettingsManager.create(cwd, agentDir)
resourceLoader = new DefaultResourceLoader({ ...(options.resourceLoaderOptions ?? {}), cwd, agentDir, settingsManager })
await resourceLoader.reload(options.resourceLoaderReloadOptions)
diagnostics = []
extensionsResult = resourceLoader.getExtensions()
// 1) pendingProviderRegistrations → modelRuntime.registerProvider(name, config)
//    失败：diagnostics.push({ type:"error", message:`Extension "${extensionPath}" error: ${message}` })
//    完成后清空数组
// 2) pendingNativeProviderRegistrations → registerNativeProvider(provider)，同样错误格式
// 3) pendingVirtualModelRegistrations → registerVirtualModel(definition)，同样错误格式
await modelRuntime.refresh({ allowNetwork: false })
diagnostics.push(...applyExtensionFlagValues(resourceLoader, options.extensionFlagValues))
return { cwd, agentDir, modelRuntime, settingsManager, resourceLoader, diagnostics }
```

注意三个 pending 数组都必须在处理完后**清空**（`= []`）。

`createAgentSessionFromServices`：把 services 的 `cwd/agentDir/modelRuntime/settingsManager/resourceLoader`
连同 options 的其余字段透传给 `createAgentSession({...})`。

### 2.6 `src/core/session-cwd.ts`

```ts
export interface SessionCwdIssue { sessionFile?: string; sessionCwd: string; fallbackCwd: string }

interface SessionCwdSource { getCwd(): string; getSessionFile(): string | undefined }

export function getMissingSessionCwdIssue(sessionManager, fallbackCwd): SessionCwdIssue | undefined {
  const sessionFile = sessionManager.getSessionFile();
  if (!sessionFile) return undefined;                  // 无文件（内存态）永不报错
  const sessionCwd = sessionManager.getCwd();
  if (!sessionCwd || existsSync(sessionCwd)) return undefined;
  return { sessionFile, sessionCwd, fallbackCwd };
}

export function formatMissingSessionCwdError(issue): string {
  const sessionFile = issue.sessionFile ? `\nSession file: ${issue.sessionFile}` : "";
  return `Stored session working directory does not exist: ${issue.sessionCwd}${sessionFile}\nCurrent working directory: ${issue.fallbackCwd}`;
}

export function formatMissingSessionCwdPrompt(issue): string {
  return `cwd from session file does not exist\n${issue.sessionCwd}\n\ncontinue in current cwd\n${issue.fallbackCwd}`;
}

export class MissingSessionCwdError extends Error {
  readonly issue: SessionCwdIssue;
  constructor(issue) { super(formatMissingSessionCwdError(issue)); this.name = "MissingSessionCwdError"; this.issue = issue; }
}

export function assertSessionCwdExists(sessionManager, fallbackCwd): void {
  const issue = getMissingSessionCwdIssue(sessionManager, fallbackCwd);
  if (issue) throw new MissingSessionCwdError(issue);
}
```

### 2.7 `src/core/session-export.ts`

```ts
type TrailingEntries = (parentId: string | null, timestamp: string) => readonly object[];

export function serializeSessionBranch(sessionManager, createTrailingEntries?): string {
  const timestamp = new Date().toISOString();
  const header: SessionHeader = { type:"session", version: CURRENT_SESSION_VERSION,
      id: sessionManager.getSessionId(), timestamp, cwd: sessionManager.getCwd() };
  const entries: object[] = [header];
  let parentId: string | null = null;
  for (const entry of sessionManager.getBranch()) {
    entries.push({ ...entry, parentId });   // 重新串成线性链，parentId 覆盖为前一个条目 id
    parentId = entry.id;
  }
  entries.push(...(createTrailingEntries?.(parentId, timestamp) ?? []));
  return `${entries.map(e => JSON.stringify(e)).join("\n")}\n`;
}

export function exportSessionToJsonl(sessionManager, outputPath?, createTrailingEntries?): string {
  const filePath = resolvePath(outputPath ?? `session-${new Date().toISOString().replace(/[:.]/g, "-")}.jsonl`,
                                process.cwd());
  const dir = dirname(filePath);
  if (!existsSync(dir)) mkdirSync(dir, { recursive: true });
  writeFileSync(filePath, serializeSessionBranch(sessionManager, createTrailingEntries));
  return filePath;
}
```

要点：
- header **没有** `parentSession` 字段（与 `SessionManager` 相比少一个键）。
- 导出重新串联 parentId（线性化），因此分支/断链在导出文件中不存在。
- 输出末尾永远有一个换行；行间一个 `\n`。
- 默认文件名形如 `session-2026-02-12T09-31-05-123Z.jsonl`。

### 2.8 `src/core/nested-tool-calls.ts`

#### 2.8.1 常量

```ts
export const NESTED_CALL_LIMITS = {
  maxCalls: 256,
  maxArgumentBytesPerCall: 8 * 1024,
  maxArgumentBytesTotal: 32 * 1024,
  maxErrorChars: 500,
} as const;
```

#### 2.8.2 类型

```ts
export interface NestedCallSummary { calls: NestedToolCalls | undefined; usage: Usage | undefined }
export interface NestedToolCallOptions { signal?: AbortSignal; onUpdate?: AgentToolUpdateCallback }
export type NestedToolExecutionEvent =
  | { type:"tool_execution_start"; toolCallId; toolName; args: unknown; parentToolCallId: string }
  | { type:"tool_execution_update"; toolCallId; toolName; args; partialResult: AgentToolResult<unknown>; parentToolCallId: string }
  | { type:"tool_execution_end"; toolCallId; toolName; result: AgentToolResult<unknown>; isError: boolean;
      durationMs?: number; parentToolCallId: string };
export interface NestedToolCallHost {
  getTools(): readonly AgentTool[];
  isSequential(): boolean;
  runToolCall(toolCall, parentToolCallId, signal, onUpdate): Promise<AgentToolCallOutcome>;
  emit(event: NestedToolExecutionEvent): Promise<void>;
}
```

`interface CallScope { recorder: NestedCallRecorder; nextId: number; holdsQueue: boolean }`（内部）。

#### 2.8.3 `NestedCallRecorder`

字段：`calls: NestedToolCallRecord[]`、`startedAt: Map<record, number>`、`complete = true`、
`argumentBytes = 0`、`usage?: Usage`。

`start(toolCall): NestedToolCallRecord | undefined`：

1. `calls.length >= 256` → `complete = false`；返回 undefined（**不记录**）。
2. `record = { id: toolCall.id, name: toolCall.name, status: "unfinished" }`。
3. `json = JSON.stringify(toolCall.arguments ?? {})`；`bytes = TextEncoder().encode(json).length`（UTF-8 字节数）。
4. 若 `bytes > 8192` 或 `argumentBytes + bytes > 32768`：
   `record.argumentsBytes = bytes`；`complete = false`（不保存 arguments）。
5. 否则 `record.arguments = JSON.parse(json)`；`argumentBytes += bytes`。
6. `calls.push(record)`；`startedAt.set(record, performance.now())`；返回 record。

`finish(record, isError, errorText)`：record 为空返回；
`status = isError ? "error" : "ok"`；`durationMs = Math.round(performance.now() - (startedAt.get(record) ?? performance.now()))`；
删除 startedAt；`isError && errorText` → `error = errorText.slice(0, 500)`。

`addUsage(usage)`：首次赋值，之后 `combineUsage`。`get totalUsage()`：累计值。

`snapshot(): NestedToolCalls | undefined`：
- `calls.length === 0 && complete` → undefined。
- 否则 `calls = 深一层浅拷贝`；返回
  `{ calls, complete: complete && calls.every(c => c.status !== "unfinished") }`。

#### 2.8.4 `NestedToolCallRunner`

字段：`host`、`scopes: Map<string, CallScope>`、`queueTail: Promise<void> = Promise.resolve()`。

`execute(callerId, name, args, options = {}): Promise<AgentToolCallOutcome>`：

1. 取或建 caller 的 scope：`{ recorder: new NestedCallRecorder(), nextId: 1, holdsQueue: false }`。
2. `toolCall = { type:"toolCall", id: `${callerId}/${scope.nextId++}`, name, arguments: (args ?? {}) }`。
3. `record = scope.recorder.start(toolCall)`。
4. `await host.emit({type:"tool_execution_start", toolCallId, toolName: name, args: toolCall.arguments,
   parentToolCallId: callerId})`。
5. `exclusive = !scope.holdsQueue && (host.isSequential() ||
   host.getTools().find(t => t.name === name)?.executionMode === "sequential")`。
6. 若 exclusive：`previous = queueTail`；`queueTail = new Promise(resolve => release = resolve)`；`await previous`。
7. `scopes.set(toolCall.id, { recorder: scope.recorder, nextId: 1, holdsQueue: scope.holdsQueue || exclusive })`
   （使嵌套的嵌套调用共享 recorder，且不会自锁队列）。
8. `try { outcome = await host.runToolCall(toolCall, callerId, options.signal, async partial => {
       options.onUpdate?.(partial);
       await host.emit({type:"tool_execution_update", toolCallId, toolName: name, args, partialResult: partial,
                        parentToolCallId: callerId}); }); }
   finally { scopes.delete(toolCall.id); release?.(); }`
9. `scope.recorder.finish(record, outcome.isError, textOf(outcome.result))`；
   `if (outcome.result.usage) scope.recorder.addUsage(outcome.result.usage)`。
10. `await host.emit({type:"tool_execution_end", toolCallId, toolName: name, result, isError,
    ...(durationMs===undefined?{}:{durationMs}), parentToolCallId: callerId})`。
11. 返回 outcome。**永不因工具失败而 reject**。

`textOf(result)`：`(result.content ?? []).filter(text).map(text).join("\n")`。

`takeRecord(toolCallId): NestedCallSummary | undefined`：取并删除该 scope；无 → undefined；
否则 `{ calls: recorder.snapshot(), usage: recorder.totalUsage }`。

`clear()`：`scopes.clear()`。

### 2.9 `src/core/bash-executor.ts`

类型：

```ts
export interface BashExecutorOptions { onChunk?: (chunk: string) => void; signal?: AbortSignal }
export interface BashResult {
  output: string; exitCode: number | undefined; cancelled: boolean; truncated: boolean; fullOutputPath?: string;
}
```

`executeBashWithOperations(command, cwd, operations, options?): Promise<BashResult>`：

```
outputChunks: string[] = []; outputBytes = 0; maxOutputBytes = DEFAULT_MAX_BYTES * 2 (= 102400)
tempFilePath?; tempFileStream?; totalBytes = 0

ensureTempFile = () => { if (tempFilePath) return;
  ({path: tempFilePath, stream: tempFileStream} = createOutputFileStream("kiana-bash", ".log"));
  for (chunk of outputChunks) tempFileStream.write(chunk); };

decoder = new TextDecoder(); pendingAnsi = "";

appendText(rawText) {
  text = sanitizeBinaryOutput(stripAnsi(rawText)).replace(/\r/g, "");
  if (!text) return;
  if (totalBytes > DEFAULT_MAX_BYTES) ensureTempFile();
  if (tempFileStream) tempFileStream.write(text);
  outputChunks.push(text); outputBytes += text.length;
  while (outputBytes > maxOutputBytes && outputChunks.length > 1) {
    removed = outputChunks.shift(); outputBytes -= removed.length;      // 滚动缓冲，保留尾部
  }
  options?.onChunk?.(text);
}

onData(data: Buffer) {
  totalBytes += data.length;
  { complete, pending } = splitIncompleteAnsiSuffix(pendingAnsi + decoder.decode(data, { stream: true }));
  pendingAnsi = pending;
  appendText(complete);
}

flushOutput() { rest = pendingAnsi + decoder.decode(); pendingAnsi = ""; appendText(rest); }

exitCode = null;
try { ({exitCode} = await operations.exec(command, cwd, { onData, signal })); }
catch (err) { if (!signal?.aborted) { tempFileStream?.end(); throw err; } }

flushOutput();
fullOutput = outputChunks.join("");
truncationResult = truncateTail(fullOutput);           // 2000 行 / 50*1024 字节，取先到者，尾部截断
if (truncationResult.truncated) ensureTempFile();
tempFileStream?.end();
cancelled = signal?.aborted ?? false;
return {
  output: truncationResult.truncated ? truncationResult.content : fullOutput,
  exitCode: cancelled ? undefined : (exitCode ?? undefined),
  cancelled, truncated: truncationResult.truncated, fullOutputPath: tempFilePath,
};
```

关键细节（**三个不同的计量单位，Rust 必须分开实现，不能合并成一个计数器**）：
- **`totalBytes`（原始入站字节）**：`onData` 里 `totalBytes += data.length`，`data` 是 `Buffer`，
  故 `data.length` 是**字节数**。临时文件的创建条件是 `totalBytes > DEFAULT_MAX_BYTES`（严格大于
  50*1024 **字节**，与已清洗文本无关，也不等于滚动缓冲长度）。
- **`outputBytes`（滚动缓冲计量）**：`outputChunks.push(text)` 后 `outputBytes += text.length`，
  `text.length` 是 **UTF-16 码元数**；滚动上限 `maxOutputBytes = DEFAULT_MAX_BYTES * 2 = 102400`
  **码元**；`while (outputBytes > maxOutputBytes && outputChunks.length > 1) shift()` 保留尾部。
- **`truncateTail`（最终裁剪）**：默认 `DEFAULT_MAX_LINES = 2000` 行与
  `DEFAULT_MAX_BYTES = 50*1024` **字节**，**两者先到者生效**；按 `Buffer.byteLength` 计字节
  （不是 UTF-16 长度）。若最后一行本身就超过字节上限且尚未放入任何整行，则取该行尾部字节并置
  `lastLinePartial = true`（因此返回值可能以半行开头；`outputLines`/`totalLines` 等统计只在
  `TruncationResult` 内部，`BashResult` 只取 `content` 与 `truncated`）。
- 临时文件内容：创建时把**当前滚动缓冲里还留着的 chunk** 追写进去（更早被滚出的部分已丢失），
  之后每个新 chunk 都写入；因此临时文件不是「完整输出」，而是「触发阈值之后的输出 + 当时缓冲残留」。
  文件前缀 `kiana-bash-<16 hex>.log`（`randomBytes(8).toString("hex")`），权限 0600，`wx` 创建，
  位置 `os.tmpdir()`。
- `\r` 全部删除（不做 CRLF→LF 归一）。
- `sanitizeBinaryOutput` 删除 `[\x00-\x08\x0B\x0C\x0E-\x1F\uFFF9-\uFFFB]`（保留 `\t` `\n` `\r`，但 `\r` 随后被删）。
- `stripAnsi` 处理 OSC / CSI；流式时尾部不完整转义序列被挂起：`splitIncompleteAnsiSuffix` 只在
  最后 **256 个 UTF-16 码元**窗口内匹配「未完成的 OSC 或 CSI」，窗口外不匹配则原样放行
  （`MAX_PENDING_ANSI_LENGTH = 256`）。
- `TextDecoder` 默认非 fatal：非法 UTF-8 序列替换为 U+FFFD（不会抛错）。

### 2.10 `src/core/exec.ts`

```ts
export interface ExecOptions { signal?: AbortSignal; timeout?: number; cwd?: string }
export interface ExecResult { stdout: string; stderr: string; code: number; killed: boolean }

export async function execCommand(command: string, args: string[], cwd: string, options?): Promise<ExecResult>
```

行为：

- `spawn(command, args, { cwd, shell: false, stdio: ["ignore","pipe","pipe"] })`。
  注意 `ExecOptions.cwd?: string` 虽然存在，但源码**从不读取**它——实际 spawn 用的是第 3 个位置参数
  `cwd`；`options.cwd` 是纯装饰字段（Rust 侧应保留字段但同样忽略，或用 `#[allow(dead_code)]`）。
- stdout/stderr 各自用 `StringDecoder("utf8")` 增量解码（跨块的多字节字符正确拼接）。
- `killed = false`；`killProcess()`：未 killed 时置 true，`SIGTERM`，并在 5 秒后若 `!proc.killed` 则 `SIGKILL`。
- signal：已中止立即 kill；否则 `addEventListener("abort", killProcess, {once:true})`。
- `timeout > 0` → 定时 kill。
- 用 `waitForChildProcess(proc)`（`src/utils/child-process.ts`；不等继承的 stdio 句柄，避免 detached
  孙进程挂住）：先等 `exit`，再等 `stdout`/`stderr` 都 `end`；若 `exit` 已发生但流仍未结束，则在最后一次
  `data` 之后重置一个 **100 ms**（`EXIT_STDIO_GRACE_MS = 100`）空闲定时器，超时即 `finalize(exitCode)`
  并 `destroy()` 两个流（这是 `5303-bash-output-truncation` 锁定的「孙进程持有 stdout」行为）。
  - resolve：清 timeout，追加 `decoder.end()`，移除 abort 监听，
    `resolve({stdout, stderr, code: code ?? 0, killed})`。
  - reject：清 timeout，追加 `decoder.end()`，移除监听，
    `resolve({stdout, stderr: stderr || error.message, code: 1, killed})`。
- 该函数**从不 reject**。

### 2.11 `src/core/output-guard.ts`

模块级：`stdoutTakeoverState?`、`RAW_STDOUT_RETRY_DELAY_MS = 10`、`rawStdoutWriteTail: Promise<void> = Promise.resolve()`。

- `getRawStdoutWrite()`：已接管 → 接管的 `rawStdoutWrite`；否则 `process.stdout.write.bind(process.stdout)`。
- `writeRawStdoutChunk(text)`：循环写；错误码为 `ENOBUFS` / `EAGAIN` / `EWOULDBLOCK` 时
  sleep 10ms 重试；其它错误抛出。
- `takeOverStdout()`：已接管直接返回；保存 `rawStdoutWrite` / `rawStderrWrite` / `originalStdoutWrite`；
  把 `process.stdout.write` 替换为「把内容写到 stderr」的包装（支持 `(chunk, cb)` 与
  `(chunk, encoding, cb)` 两种签名，`String(chunk)` 转换）。
- `restoreStdout()`：恢复原 write；清除状态。
- `isStdoutTakenOver()`。
- `writeRawStdout(text)`：空串直接返回；否则链到 `rawStdoutWriteTail`；`catch` → `process.exit(1)`。
- `waitForRawStdoutBackpressure()`：循环等待 tail 直到不再变化。
- `flushRawStdout()`：先等背压，再写空串（触发底层 flush 语义）。

**Rust 映射**：JS 里这是为了在 TUI 全屏渲染期间保护 stdout（把意外 print 重定向到 stderr），
并允许原始输出（如 JSON 模式）绕过。Rust 侧需要等价的「全局 stdout 接管」抽象
（`std::io::stdout` 包装或专用 writer 单例 + 写锁）。

### 2.12 `src/core/event-bus.ts`

```ts
export interface EventBus { emit(channel: string, data: unknown): void; on(channel: string, handler: (data: unknown) => void): () => void }
export interface EventBusController extends EventBus { clear(): void }
export function createEventBus(): EventBusController
```

行为：内部 Node `EventEmitter`；`emit(channel, data)` 直接转发；`on(channel, handler)` 包装为
`async data => { try { await handler(data) } catch (err) { console.error(\`Event handler error (${channel}):\`, err) } }`
并注册；返回 `() => emitter.off(channel, safeHandler)`；`clear()` → `removeAllListeners()`。

语义：**同一 channel 的监听器同步依序被调用**（EventEmitter 语义），但每个 handler 的返回值被忽略、
异常被吞并打印。Rust 侧可用 `tokio::sync::broadcast` 或自建 `HashMap<channel, Vec<Handler>>` + 串行调用；
注意错误输出格式必须逐字一致。

### 2.13 `src/core/index.ts`

再导出（**顺序与集合**，供 Rust crate 的 `pub use` 对齐）：

- 从 `agent-session.ts`：`AgentSession`、类型 `AgentSessionConfig`、`AgentSessionEvent`、
  `AgentSessionEventListener`、`ModelCycleResult`、`PromptOptions`、`SessionStats`。
- 从 `agent-session-runtime.ts`：`AgentSessionRuntime`、`CreateAgentSessionRuntimeFactory`、
  `CreateAgentSessionRuntimeResult`、`createAgentSessionRuntime`。
- 从 `agent-session-services.ts`：`AgentSessionRuntimeDiagnostic`、`AgentSessionServices`、
  `CreateAgentSessionFromServicesOptions`、`CreateAgentSessionServicesOptions`、
  `createAgentSessionFromServices`、`createAgentSessionServices`。
- 从 `bash-executor.ts`：`BashExecutorOptions`、`BashResult`、`executeBashWithOperations`。
- 从 `cache-warmer.ts`：`CacheWarmingDecision`、`CacheWarmingStatus`。
- 从 `compaction/index.ts`：`CompactionResult`。
- 从 `event-bus.ts`：`createEventBus`、`EventBus`、`EventBusController`。
- 从 `experimental.ts`：`areExperimentalFeaturesEnabled`。
- 从 `extensions/index.ts`（**逐字完整清单**，共 60 个名字）：
  值：`defineTool`、`discoverAndLoadExtensions`、`ExtensionRunner`；
  类型：`AgentActivityOutcome`、`AgentBeforeSettleEvent`、`AgentBeforeSettleEventResult`、`AgentEndEvent`、
  `AgentSettledEvent`、`AgentStartEvent`、`AgentToolResult`、`AgentToolUpdateCallback`、`BeforeAgentStartEvent`、
  `BeforeAgentStartEventResult`、`BoundaryContextPreview`、`BoundaryResult`、`BoundaryState`、
  `BuildSystemPromptOptions`、`CacheWarmingDecisionEvent`、`CacheWarmingDecisionEventResult`、
  `CompactionEntryDraft`、`ContextEditEntryDraft`、`ContextEvent`、`ContextWithSystemEvent`、
  `CustomEntryDraft`、`CustomMessageEntryDraft`、`ExecOptions`、`ExecResult`、`Extension`、`ExtensionAPI`、
  `ExtensionCommandContext`、`ExtensionContext`、`ExtensionError`、`ExtensionEvent`、`ExtensionFactory`、
  `ExtensionFlag`、`ExtensionHandler`、`ExtensionShortcut`、`ExtensionUIContext`、`InlineExtension`、
  `LoadExtensionsResult`、`MessageRenderer`、`NormalizedBuildSystemPromptOptions`、`RegisteredCommand`、
  `SessionBeforeCompactEvent`、`SessionBeforeForkEvent`、`SessionBeforeSwitchEvent`、`SessionBeforeTreeEvent`、
  `SessionBoundaryDraft`、`SessionCompactEvent`、`SessionShutdownEvent`、`SessionStartEvent`、`SessionTreeEvent`、
  `ToolCallEvent`、`ToolCallEventResult`、`ToolDefinition`、`ToolRenderResultOptions`、`ToolResultEvent`、
  `TurnEndEvent`、`TurnEndEventResult`、`TurnStartEvent`、`WorkingIndicatorOptions`。
  **注意**：`ExecOptions` 与 `ExecResult` 是从 `core/exec.ts` 经 `extensions/types.ts`
  re-export（`export type { ExecOptions, ExecResult } from "../exec.ts"`）**再**从 `core/index.ts` 导出的，
  所以 `core/index.ts` 里没有 `exec.ts` 的单独 `export ... from "./exec.ts"`；Rust 侧的 `pub use`
  需要保证 `ExecOptions`/`ExecResult` 从 crate 根可见（来源模块无所谓）。
- 从 `source-info.ts`：`createSyntheticSourceInfo`。

注意 `agent-session.ts` 的 `parseSkillBlock`、`ParsedSkillBlock`、`AgentSessionConfig` 的兄弟类型
`ExtensionBindings`、`QueuedInputDisposition`、`PromptDisposition`、`ModelMutationOptions` **未**从
`core/index.ts` 导出（只在本文件内部与直接 import 处使用）；`messages.ts`、`session-cwd.ts`、
`session-export.ts`、`nested-tool-calls.ts`、`exec.ts`、`output-guard.ts` 的符号也**全部未**从
`core/index.ts` 导出。`session-manager.ts` 同样未从 core/index 导出（由 `sdk.ts` 与 modes 直接 import）。
Rust crate 的 `pub use` 应与 `core/index.ts` 对齐，但这些符号仍需 `pub` 给同 crate 其他模块用。
`agent-session-runtime.ts` 自己在文件末尾 re-export 了 services 的
`AgentSessionRuntimeDiagnostic`、`AgentSessionServices`、`CreateAgentSessionFromServicesOptions`、
`CreateAgentSessionServicesOptions`、`createAgentSessionFromServices`、`createAgentSessionServices`
（所以从 runtime 模块也能拿到这些）。

---

## 3. 用户可见表面（逐字文本）

本节所有字符串必须逐字复现（大小写、标点、换行、空格），否则用户可见输出不一致。

### 3.1 错误消息（Error.message）

| 出处 | 文本 |
|---|---|
| agent-session `_getRequiredRequestAuth` | `Authentication failed for "${provider}". Credentials may have expired or network is unavailable. Run '/login ${provider}' to re-authenticate.`（三段字符串拼接，中间各一个空格，见下） |
| agent-session `prompt`（同认证检查） | 同上 |
| agent-session `_beforeToolCall` | `Extension failed, blocking execution: ${String(err)}` |
| agent-session `_createBoundaryPreviewManager` | `Session header is missing` |
| agent-session `_omitRecoveryAttempt` | `Cannot persist recovery omission because a projected message has no source entry` |
| agent-session `prompt`（压缩中） | `Cannot submit a prompt while compaction is in progress. Wait for compaction to finish and retry.` |
| agent-session `prompt`（流式中无 behavior） | `Agent is already processing. Specify streamingBehavior ('steer' or 'followUp') to queue the message.` |
| agent-session `_throwIfExtensionCommand` | `Extension command "/${commandName}" cannot be queued. Use prompt() or execute the command when not streaming.` |
| agent-session `setModel` | `No API key for ${provider}/${id}`（`checkAuth` 返回 falsy 时） |
| agent-session `_getRequiredRequestAuth`（非 OAuth 且无凭证） | `formatNoApiKeyFoundMessage(provider)`，逐字见下 |
| agent-session `prompt` / `compact`（无模型） | `formatNoModelSelectedMessage()`，逐字见下 |
| agent-session `compact` | `Compaction cancelled`（扩展取消或 signal 中止） |
| agent-session `compact` | `Already compacted` |
| agent-session `compact` | `Nothing to compact (session too small)` |
| agent-session `compact` catch | `Compaction failed: ${message}`（仅非 aborted 时作为 errorMessage） |
| agent-session `_checkCompaction` | `Context overflow recovery failed after one compact-and-retry attempt. Try reducing context or switching to a larger-context model.` |
| agent-session `_checkCompaction` | `Truncated response recovery failed after one compact-and-retry attempt.` |
| agent-session `_runAutoCompaction` | `Compaction cancelled`；`compaction failed`（非 Error 的兜底 message）；`Context overflow recovery failed: ${message}`；`Auto-compaction failed: ${message}` |
| agent-session `navigateTree` | `Wait for the current response to finish before navigating the session tree.` |
| agent-session `navigateTree` | `Wait for the current compaction or tree navigation to finish before navigating the session tree.` |
| agent-session `navigateTree` | `No model available for summarization` |
| agent-session `navigateTree` | `Entry ${targetId} not found` |
| agent-session `summarizeForBugReport` | `No model selected` |
| agent-session `_summarizationRetryCallbacks` 相关 | （无错误文本） |
| session-manager `assertValidSessionId` | `Session id must be non-empty, contain only alphanumeric characters, '-', '_', and '.', and start and end with an alphanumeric character` |
| session-manager `_setSessionFile` | `Session file is not a valid ${APP_NAME} session: ${explicitPath}` |
| session-manager `appendContextEdit` | `Context edit replacement must be null or contain string/array content` |
| session-manager `appendContextEdit`/`branch`/`branchWithSummary`/`createBranchedSession`/`appendLabelChange` | `Entry ${id} not found` |
| session-manager `appendContextEdit` | `Entry ${targetId} is not on the active branch` |
| session-manager `appendContextEdit` | `Entry ${targetId} does not contribute editable model content` |
| session-manager `forkFrom` | `Cannot fork: source session file is empty or invalid: ${resolvedSourcePath}` |
| session-manager `forkFrom` | `Cannot fork: source session has no header: ${resolvedSourcePath}` |
| session-manager `SessionHeaderScanLimitError` | `Session header exceeds ${MAX_SESSION_HEADER_SCAN_BYTES}-byte scan limit: ${filePath}`（= `1048576`） |
| agent-session-runtime `SessionImportFileNotFoundError` | `File not found: ${filePath}` |
| agent-session-runtime `fork` | `Invalid entry ID for forking` |
| agent-session-runtime `fork` | `Persisted session is missing a session file` |
| agent-session-runtime `fork` | `This session has not been saved yet. Send a message before cloning or forking it.` |
| agent-session-runtime `fork` | `Failed to create forked session` |
| session-cwd `formatMissingSessionCwdError` | `Stored session working directory does not exist: ${sessionCwd}${sessionFile}\nCurrent working directory: ${fallbackCwd}`（`sessionFile` 段为 `\nSession file: ${path}` 或空） |
| agent-session-services `applyExtensionFlagValues` | `Extension flag "--${name}" requires a value` |
| agent-session-services `applyExtensionFlagValues` | `Unknown option${s}: ${flags}`（`s` 单数不带；flags 形如 `--a, --b`） |
| agent-session-services 注册失败 | `Extension "${extensionPath}" error: ${message}` |
| sleep（`utils/sleep.ts`） | `Aborted` |
| prompt-templates 展开 | （见 ai-core/其他单元） |

`Authentication failed` 的精确拼接（源码）：

```ts
throw new Error(
  `Authentication failed for "${model.provider}". ` +
    `Credentials may have expired or network is unavailable. ` +
    `Run '/login ${model.provider}' to re-authenticate.`,
);
```

即：`Authentication failed for "<provider>". Credentials may have expired or network is unavailable. Run '/login <provider>' to re-authenticate.`

`session-cwd.formatMissingSessionCwdPrompt`（用于交互式确认提示）：
`cwd from session file does not exist\n${sessionCwd}\n\ncontinue in current cwd\n${fallbackCwd}`。

`core/auth-guidance.ts` 的四个函数（**逐字**，全部依赖 `getDocsPath()`，即 `KIANA_PACKAGE_DIR` 或包内 `docs/`）：

```ts
const UNKNOWN_PROVIDER = "unknown";

getProviderLoginHelp():
  "Use /login to log into a provider via OAuth or API key. See:\n" +
  `  ${join(getDocsPath(), "providers.md")}\n` +
  `  ${join(getDocsPath(), "models.md")}`

formatNoModelsAvailableMessage():
  `No models available. ${getProviderLoginHelp()}`

formatNoModelSelectedMessage():
  `No model selected.\n\n${getProviderLoginHelp()}\n\nThen use /model to select a model.`

formatNoApiKeyFoundMessage(provider):
  providerDisplay = provider === "unknown" ? "the selected model" : provider;
  `No API key found for ${providerDisplay}.\n\n${getProviderLoginHelp()}`
```

注意三点：(a) `summarizeForBugReport` 抛的是**裸** `No model selected`（源码 `throw new Error("No model selected")`），
而 `prompt`/`compact` 抛的是 `formatNoModelSelectedMessage()` 的**多行全文**——两者不同，原规格把前者当成通用文本会误导；
(b) `formatNoApiKeyFoundMessage` 对 provider `"unknown"` 显示 `the selected model`；(c) 这些文本含**绝对文档路径**，
黄金测试须用临时 `KIANA_PACKAGE_DIR` 或路径替换后再比对。

`agent-session` 中的 `Authentication failed for ...`（OAuth 分支）与 `formatNoApiKeyFoundMessage`（非 OAuth 分支）
是互斥的两条路径：`isUsingOAuth(provider)` 为真 → 前者，否则 → 后者。

### 3.2 非错误、但用户可见的固定文本

- `bashExecutionToText`：`Ran \`<command>\`\n` + 输出块 / `(no output)` + 可选
  `\n\n(command cancelled)` / `\n\nCommand exited with code <n>` / `\n\n[Output truncated. Full output: <path>]`。
- 会话摘要前缀（见 2.1.1）。
- 图片处理提示（来自 `processImage`，跨单元）：`[Image omitted: could not be converted to a supported inline image format.]`、
  `[Image omitted: could not be resized below the inline image size limit.]`。
- 空会话的 firstMessage：`(no messages)`。
- 无 assistant 消息时内部 tool 结果文本：`No assistant message issued this call`。
- 扩展错误路径标记：`<boundary>`、`<runtime>`、`<sdk:${name}>`。
- 命令错误的 extensionPath：`command:${commandName}`。
- 扩展 stale 消息（`dispose`，逐字，见 2.3.9）。
- 边界续跑失败的错误文本：
  `${event} requested continuation without runnable model context`（event 为 `turn_end` / `agent_before_settle`）。
- `turn_end could not resolve the persisted assistant entry ID`。
- 事件总线错误：`Event handler error (${channel}):`（`console.error` 的第一个参数）。

### 3.3 公开 API 形状（对 SDK / 扩展作者可见）

见第 2 节各导出符号。Rust 侧应以等价 trait/struct 提供；命名建议保留 snake_case 对应，
但**文本与 JSON 字段名**必须与 TS 完全一致。

---

## 4. 外部格式

### 4.1 会话文件（JSONL）

- 路径：`<agentDir>/sessions/--<encoded-cwd>--/<timestamp>_<session-id>.jsonl`。
- `agentDir` 默认 `~/.kiana/agent`，可用环境变量 `KIANA_CODING_AGENT_DIR` 覆盖（`ENV_AGENT_DIR = ${APP_NAME.toUpperCase()}_CODING_AGENT_DIR`）。
- `KIANA_CODING_AGENT_SESSION_DIR`（`ENV_SESSION_DIR`）用于会话目录覆盖（CLI `--session-dir` 优先）。
- UTF-8，LF 换行，每行一个 JSON 对象，无 BOM。
- 第一行必须是 header：`{"type":"session","version":3,"id":"<uuid>","timestamp":"<ISO>","cwd":"<abs path>"[,"parentSession":"<path>"]}`。
- 条目按追加顺序写入；`timestamp` 为 `Date.prototype.toISOString()` 格式
  （`YYYY-MM-DDTHH:mm:ss.sssZ`，UTC，毫秒 3 位，年份 4 位）。
- 加载时容忍：空行、畸形 JSON 行（跳过）、末行无换行（读取时按末行解析，随后补写 `\n`）、
  v1/v2 旧版本（迁移并重写文件）。
- 加载时**拒绝**：首个可解析条目不是合法 header 且文件非空（抛错，不改文件）；
  空文件（0 字节）会被重写为带新 header 的文件。

### 4.2 条目 JSON 形状（示例，逐字）

```json
{"type":"session","version":3,"id":"0194f0e2-6a1c-7d3b-9f2e-1c2d3e4f5a6b","timestamp":"2026-02-12T09:31:05.123Z","cwd":"/home/u/proj"}
{"type":"message","id":"3f2a9c1b","parentId":null,"timestamp":"2026-02-12T09:31:05.200Z","message":{"role":"user","content":"Hello","timestamp":1770888665200}}
{"type":"message","id":"7d1e4b22","parentId":"3f2a9c1b","timestamp":"2026-02-12T09:31:06.000Z","message":{"role":"assistant","content":[{"type":"text","text":"Hi!"}],"api":"anthropic-messages","provider":"anthropic","model":"claude-sonnet-4-5","usage":{"input":10,"output":3,"cacheRead":0,"cacheWrite":0,"totalTokens":13,"cost":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"total":0}},"stopReason":"stop","timestamp":1770888666000}}
{"type":"compaction","id":"a1b2c3d4","parentId":"7d1e4b22","timestamp":"2026-02-12T09:40:00.000Z","summary":"...","firstKeptEntryId":"7d1e4b22","tokensBefore":50000,"systemMessage":{"role":"system","content":"","sections":{...},"toolsAdded":[...],"timestamp":1770889200000}}
{"type":"branch_summary","id":"b2c3d4e5","parentId":"3f2a9c1b","timestamp":"2026-02-12T09:45:00.000Z","fromId":"a1b2c3d4","summary":"Branch explored approach A..."}
{"type":"usage","id":"c3d4e5f6","parentId":"b2c3d4e5","timestamp":"2026-02-12T09:46:00.000Z","kind":"cache_warm","provider":"anthropic","model":"claude-sonnet-4-5","usage":{...}}
{"type":"custom","id":"d4e5f6a7","parentId":"c3d4e5f6","timestamp":"2026-02-12T09:47:00.000Z","customType":"kiana.virtual-model-state","data":{"provider":"x","modelId":"y","state":{}}}
{"type":"custom_message","id":"e5f6a7b8","parentId":"d4e5f6a7","timestamp":"2026-02-12T09:48:00.000Z","customType":"my-ext","content":"Injected context...","display":true}
{"type":"context_edit","id":"f6a7b8c9","parentId":"e5f6a7b8","timestamp":"2026-02-12T09:49:00.000Z","targetId":"7d1e4b22","replacement":null}
{"type":"label","id":"a7b8c9d0","parentId":"f6a7b8c9","timestamp":"2026-02-12T09:50:00.000Z","targetId":"3f2a9c1b","label":"checkpoint-1"}
{"type":"session_info","id":"b8c9d0e1","parentId":"a7b8c9d0","timestamp":"2026-02-12T09:51:00.000Z","name":"Refactor auth module"}
{"type":"model_change","id":"c9d0e1f2","parentId":"b8c9d0e1","timestamp":"2026-02-12T09:52:00.000Z","provider":"openai","modelId":"gpt-4o"}
{"type":"thinking_level_change","id":"d0e1f2a3","parentId":"c9d0e1f2","timestamp":"2026-02-12T09:53:00.000Z","thinkingLevel":"high"}
```

`model_change` 与 `thinking_level_change` 的键顺序是 `type,id,parentId,timestamp,provider,modelId` /
`type,id,parentId,timestamp,thinkingLevel`；`session_info` 是 `type,id,parentId,timestamp,name`。

### 4.3 导出文件

- JSONL（`session-export.ts`）：header **不含** `parentSession`；branch 被重新线性化
  （每条 `parentId` 覆盖为前一条 id，首条为 null）；末尾一个换行；可选 trailing entries 追加在最后。
- 默认文件名：`session-<ISO with : and . replaced by ->.jsonl`，相对 `process.cwd()` 解析；
  目录不存在则递归创建（`writeFileSync` 覆盖已存在文件）。
- HTML 导出：委托 `core/export-html`（其他单元），本单元只提供 `themeName` 选择与 tool renderer 注入。

### 4.4 会话列表 / 发现

- 目录扫描只看 `*.jsonl`。
- `list(cwd, sessionDir?, onProgress?, signal?)`：目录为 `normalizePath(sessionDir)` 或默认目录；
  `filterCwd = sessionDir !== undefined && dir !== getDefaultSessionDirPath(cwd)`（**显式传了 sessionDir
  但不是默认目录**时才按 cwd 过滤）；`onProgress(loaded, total, partial?)` 的 partial 在
  `loaded === 1 || loaded % 10 === 0 || loaded === total` 时发布（仅含已通过 `includeSession` 的会话）；
  结果按 `modified` 降序（`sortSessionInfos` 原地排序）。
- `listAll(...)`：跨所有 cwd 目录（`getSessionsDir()` 下的目录与符号链接）；文件候选先按 mtime 降序、
  同 mtime 按 `basename` 的 **localeCompare 降序**；并发：目录/stat 64，解析 10；
  progress 的 partial 条件是 `firstCandidateLoaded && (index === 0 || loaded % 100 === 0 || loaded === total)`
  ——`firstCandidateLoaded` 只在 **index 0 完成**后为真，故在此之前的进度回调 `partial` 恒为 `undefined`。
  传入自定义 sessionDir 时直接 `listSessionsFromDir` 后排序（不做 mtime 预排序）。
- 取消：`listAll` 开头与 `listSessionsFromDir` 开头都 `signal?.throwIfAborted()`；`mapWithConcurrency`
  在每次取新任务前也 `throwIfAborted()`，因此**已取消**的调用会抛出（不是返回 `[]`）。
  内部 catch 分支里同样先 `signal?.throwIfAborted()` 再返回 `[]`。
- `findById(cwd, id, sessionDir?)`：按 `readdirSync` 顺序（**不排序**）返回第一个 id 精确匹配者；
  仅在 `filterCwd` 时校验 header cwd；任何异常吞掉返回 undefined。
- `findMostRecentSession(sessionDir, cwd?)`：`readdirSync` 过滤 `.jsonl`、按 mtime **降序**，
  第一个 header cwd 匹配者（`cwd` 省略则不过滤）；任何异常返回 null。
- `SessionManager.listAll` 有**两个重载**：`(onProgress?, signal?)` 与 `(sessionDir?, onProgress?, signal?)`；
  实现用 `typeof sessionDirOrOnProgress === "string"` / `"function"` 分派，第三个参数可能被当作
  `abortSignal`（当第二个参数是函数时用 `signal`，否则用 `onProgressOrSignal`——后者可能是 AbortSignal）。

### 4.5 环境变量

本单元**直接**读取的环境变量仅通过 `src/config.ts`：

| 变量 | 用途 |
|---|---|
| `KIANA_CODING_AGENT_DIR` | agent 目录（`getAgentDir`，经 `expandTildePath` = `normalizePath`，会展开 `~`） |
| `KIANA_CODING_AGENT_SESSION_DIR` | 会话目录（由 CLI/settings 层使用；`getSessionsDir` 返回 `<agentDir>/sessions`） |
| `KIANA_PACKAGE_DIR` | **规格原表遗漏**：`getPackageDir()` 优先返回 `normalizePath(process.env.KIANA_PACKAGE_DIR)`（Nix/Guix 场景），否则 Bun 二进制取 `dirname(process.execPath)`，否则从 `__dirname` 向上找包目录。`getDocsPath()` = `<packageDir>/docs`，直接决定 `auth-guidance` 的错误文本 |
| `KIANA_OFFLINE` | **不在本单元**（`model-runtime.ts` 用它决定是否允许联网刷新；本单元只在测试里设置它） |

`APP_NAME = pkg.kianaConfig?.name || "kiana"`；`APP_TITLE = kianaConfigName ? APP_NAME : "π"`；
`CONFIG_DIR_NAME = pkg.kianaConfig?.configDir || ".kiana"`；`ENV_AGENT_DIR = ${APP_NAME.toUpperCase()}_CODING_AGENT_DIR`；
`ENV_SESSION_DIR = ${APP_NAME.toUpperCase()}_CODING_AGENT_SESSION_DIR`。
本包 `package.json` 只有 `kianaConfig.configDir = ".kiana"`（**没有** `kianaConfig.name`），故实际
`APP_NAME = "kiana"`、`APP_TITLE = "π"`、`ENV_AGENT_DIR = "KIANA_CODING_AGENT_DIR"`。
`getAgentDir()` 默认 `join(homedir(), CONFIG_DIR_NAME, "agent")`（注意此处**没有**包一层 `kiana/`）。
bash 工具执行时会注入 `KIANA_SESSION_ID` / `KIANA_SESSION_FILE` / `KIANA_PROVIDER` / `KIANA_MODEL` /
`KIANA_REASONING_LEVEL`（由 `core/tools/bash.ts` 的 `createLocalBashOperations` 负责，非本单元）。

### 4.6 嵌套调用记录（`nestedCalls`，写在 tool result 消息上）

形状（`NestedToolCalls`）：

```json
{ "calls": [ { "id": "<callerId>/1", "name": "read", "status": "ok"|"error"|"unfinished",
               "arguments": {...}, "argumentsBytes": 1234, "durationMs": 12, "error": "..." } ],
  "complete": true }
```

规则：`arguments` 只在未超限时存在；超限时写 `argumentsBytes` 且 `complete=false`；
call 数超 256 时丢弃后续（`complete=false`）；`error` 仅错误时、最多 500 字符；
`complete` 还要求无 `unfinished`。汇总 `usage` 加到消息自身的 `usage` 上（`combineUsage`）。

### 4.7 bash 输出文件

- 路径：`<tmpdir>/kiana-bash-<16 位小写 hex>.log`（`randomBytes(8).toString("hex")`）。
- 权限 `0600`；创建标志 `wx`（已存在则失败）。
- 内容：经 ANSI 剥离 / 二进制清洗 / `\r` 移除后的文本（滚动缓冲可能丢失最早片段）。
- 同族函数（`src/utils/output-files.ts`，本单元只用到 stream 版）：`writeOutputFile(prefix, ext, data)`
  用 `writeFile(..., {mode:0o600, flag:"wx"})`；`createOutputFileStream` 用
  `createWriteStream(..., {mode:0o600, flags:"wx"})`。路径一律 `join(tmpdir(), ...)`，**没有**可配置目录。

### 4.8 官方文档里与本单元相关、但规格正文未覆盖的语义

来自 `docs/session-format.md`、`docs/sessions.md`、`docs/message-types.md`、`docs/how-kiana-works.md`：

- **会话删除**：`/resume` 选择会话按 `Ctrl+D` 后确认即可删除；可用时 kiana 调用 `trash` CLI 而非永久删除
  （文件系统层就是删 `<agentDir>/sessions/**/*.jsonl`）。这属于 UI 层，但 Rust 的 session-discovery 单元
  不要假设文件永存。
- **`usage` 条目的 `kind` 是任意字符串**：文档明确「Consumers should treat unknown `kind` values as normal
  usage rather than rejecting them」；cache warming 使用 `"cache_warm"`。本单元 `appendUsage`/`getSessionStats`
  不校验 `kind`，Rust 侧也不得按枚举拒绝未知值。
- **会话名显示**：设置了 `session_info.name` 后，`/resume` 选择器显示会话名**而不是**首条消息
  （`SessionInfo.name` 优先于 `firstMessage`）。
- **`--session-id`**：文档提到调用方可通过 SDK 或 `--session-id` 提供自定义 id（对应 `assertValidSessionId` 与
  `NewSessionOptions.id`）。
- **路径编码**：`--<path>--` 中 `<path>` 去掉开头一个分隔符并把 `/`、`\`、`:` 替换为 `-`（与 2.2.7 一致；
  文档写「`~/\.kiana/agent/sessions/...`」）。
- **版本迁移语义**：v1 线性条目 → v2 加 `id`/`parentId` → v3 把 `hookMessage` role 改名 `custom`；
  加载时自动迁移到 v3。
- **system 消息重放**：会话里第一条 system 消息携带全部 prompt section 与工具声明；后续变更以 system 消息
  形式持久化，按名 patch `sections`（`null` 删除）并列出 `toolsAdded`/`toolsRemoved`；按序重放即得当前
  prompt 与工具集，**没有**单独的 prompt 状态条目。
- **树结构可以有多个 root**：`resetLeaf()` 或 `branchWithSummary(null, ...)` 会让后续条目成为另一个 root。
- **`buildSessionContext` 的转换表**（文档逐条）：`message` → 存储的 `AgentMessage`；`compaction` →
  完整 system checkpoint + `compactionSummary`；`branch_summary` → `branchSummary`；`custom_message` →
  `CustomMessage`；`context_edit` / `usage` / `custom` → **不产生** context 消息。
- **`message-types.md` 中规格未列的字段**（Rust 的消息枚举必须容纳，否则序列化丢字段）：
  - `TextContent.textSignature?`（provider 私有，视为 opaque）；
  - `ThinkingContent { type:"thinking", thinking, thinkingSignature?, redacted? }`；
  - `ToolCall.thoughtSignature?`、`ToolCall.namespace?`（OpenAI Responses 的命名空间工具）；
  - `Usage.cacheWrite1h?`（`cacheWrite` 中按 1 小时保留的子集）、`Usage.reasoning?`（**已包含在 `output` 里**，
    不得重复相加）；
  - `AssistantMessage.responseId?`、`providerThinkingLevel?`、`diagnostics?: AssistantMessageDiagnostic[]`、
    `rawStopReason?`、`endTurn?`；
  - `stopReason` 还有第 7 个取值 `"deferred"`，此时带 `deferred?: DeferredHandle
    { provider, modelId, api, id, expiresAt?, pollAfterMs?, data? }`；
  - `"pending"` 只用于流式中的部分消息，**不会**写入会话 JSONL；`message_end` 收到的是终态 stopReason。
  - `ToolResultMessage<TDetails>` 完整字段：`role, toolCallId, toolName, content, details?, usage?,
    nestedCalls?, isError, timestamp`。

---

## 5. 跨单元接口

本单元不自己实现下列模块，但**逐字依赖**它们的契约。Rust 侧由对应单元提供，接口形状必须匹配。

### 5.1 `@earendil-works/kiana-agent-core`（packages/agent）

`Agent`（类）——本单元使用的成员：

| 成员 | 说明 |
|---|---|
| `subscribe(listener: (event: AgentEvent, signal: AbortSignal) => Promise<void> \| void): () => void` | 事件订阅；listener 收**两个**参数（第二个是本次运行的 AbortSignal）；返回退订（`this.listeners.delete`） |
| `state: AgentState` | `{ systemPrompt(readonly), model, thinkingLevel, tools, messages, isStreaming, streamingMessage?, pendingToolCalls: ReadonlySet<string>, errorMessage? }`——注意 `pendingToolCalls`（当前执行中的 tool call id 集合）在规格原表里漏了 |
| `prompt(message: AgentMessage \| AgentMessage[])` / `prompt(input: string, images?)` | 运行一轮；已在运行时抛 `Agent is already processing a prompt. Use steer() or followUp() to queue messages, or wait for completion.` |
| `continue()` | 从最后一条消息继续。运行中抛 `Agent is already processing. Wait for completion before continuing.`；无消息或全 system 抛 `No messages to continue from`；最后一条是 assistant 时**先** drain steering 队列（若有则用它 `runPromptMessages(..., {skipInitialSteeringPoll:true})`），**再** drain followUp 队列，两者都空才抛 `Cannot continue from message role: assistant`；否则 `runContinuation()` |
| `steer(message)` / `followUp(message)` | 入队 |
| `clearAllQueues()` / `clearSteeringQueue()` / `clearFollowUpQueue()` / `hasQueuedMessages()` / `peekQueuedMessages()` | 队列查看/清空（peek 优先返回 steering，非空则不再看 followUp） |
| `signal` | 当前运行的 AbortSignal 或 undefined |
| `abort()` | 中止当前运行 |
| `waitForIdle()` | 运行与事件监听器都结束后 resolve |
| `reset()` | 运行中抛 `Agent is already processing. Wait for completion before resetting.` |
| `steeringMode` / `followUpMode` | `"all" \| "one-at-a-time"`（默认 `"one-at-a-time"`） |
| `toolExecution` | `"sequential"` 时工具串行 |
| `streamFunction` | 默认为 `kiana-ai` 的 `streamSimple`（用于判断是否走 kiana 自己的鉴权路径） |
| 钩子属性 | `beforeToolCall`、`afterToolCall`、`finishTurn`、`prepareRequest`、`prepareNextTurn`、`prepareNextTurnWithContext`、`transformContext`、`convertToLlm`、`getApiKey`、`onPayload`、`onResponse`、`onProviderStreamEvent` |

`AgentEvent`（判别联合）：

```ts
{ type:"agent_start" }
{ type:"agent_end"; messages: AgentMessage[] }
{ type:"turn_start" }
{ type:"turn_end"; message: AgentMessage; toolResults: ToolResultMessage[] }
{ type:"message_start"; message: AgentMessage }
{ type:"message_update"; message: AgentMessage; assistantMessageEvent: AssistantMessageEvent }
{ type:"message_end"; message: AgentMessage }
{ type:"tool_execution_start"; toolCallId: string; toolName: string; args: any }
{ type:"tool_execution_update"; toolCallId: string; toolName: string; args: any; partialResult: any }
{ type:"tool_execution_end"; toolCallId: string; toolName: string; result: any; isError: boolean; durationMs?: number }
```

`AgentToolCall` = `AssistantMessage["content"]` 中 `{type:"toolCall"}` 的成员。
`AgentToolResult` = `{ content: (TextContent|ImageContent)[]; details; structuredContent?; usage?; isError?; terminate? }`。
`AgentToolCallOutcome` = `{ toolCall; result; isError; durationMs? }`。
`AgentTool` 在 `Tool` 基础上增加 `label`、`prepareArguments?`、`outputSchema?`、`executionMode?`
（`"sequential"` 等）、`parameters` 等。

`runToolCall(toolCall, options): Promise<AgentToolCallOutcome>`，`options` 为
`{ tools, assistantMessage, context: { messages, tools }, beforeToolCall, afterToolCall, signal, onUpdate }`。

`PrepareNextTurnContext`：`{ context: AgentContext, messages?: AgentMessage[], model?, thinkingLevel? }`
（本单元用 `prepareNextTurnWithContext` 的返回值覆盖 context/messages/model/thinkingLevel）。

`AgentContext`：`{ messages: AgentMessage[]; tools: AgentTool[]; ... }`。

### 5.2 `@earendil-works/kiana-ai`（packages/ai）

本单元使用：

- 类型：`AssistantMessage`、`AuthResult`、`ImageContent`、`Model`、`ProviderHeaders`、`SystemMessage`、
  `TextContent`、`ToolResultMessage`、`Usage`、`Message`、`UserMessage`、`JsonObject`、
  `NestedToolCallRecord`、`NestedToolCalls`、`AuthOperationOptions`。
- 值：`uuidv7`、`contentText`、`getCurrentSystemMessage`、`getCurrentTools`、`retryDelayMs`、
  `clampThinkingLevel`、`getSupportedThinkingLevels`、`isContextOverflow`、`isRecoverableLength`、
  `isRetryableAssistantError`、`modelsAreEqual`、`resetApiProviders`、`streamSimple`、
  `cleanupSessionResources`（来自 compat 入口）、`getSystemMessageText`（间接）。
- `SystemMessage` 关键字段：`role:"system"`、`content: string | TextContent[]`、`sections?: Record<string,string|null>`、
  `toolsAdded?: Tool[]`、`toolsRemoved?: {name}[]`、`timestamp: number`。
- `Usage`：`{ input, output, cacheRead, cacheWrite, cacheWrite1h?, reasoning?, totalTokens,
  cost: { input, output, cacheRead, cacheWrite, total, totalFromProvider? } }`。
- `AssistantMessage`：`{ role:"assistant", content:(Text|Thinking|ToolCall)[], api, provider, model,
  responseModel?, thinkingLevel?, usage, stopReason: "pending"|"stop"|"length"|"toolUse"|"error"|"aborted",
  errorMessage?, timestamp }`。
- `ToolResultMessage`：`{ role:"toolResult", toolCallId, toolName, content:(Text|Image)[], details?,
  usage?, isError, nestedCalls?, timestamp }`。
- `isContextOverflow(message, contextWindow?)`：error 消息按模式匹配；否则按 usage 与 contextWindow 比较。
- `isRecoverableLength(message, desiredMaxOutput)`：`stopReason === "length" && desiredMaxOutput > 0 &&
  message.usage.output < desiredMaxOutput`。
- `isRetryableAssistantError(message)`：`stopReason === "error"` 且有 `errorMessage`；
  先排除 `NON_RETRYABLE_PROVIDER_LIMIT_ERROR_PATTERN`，再匹配 `RETRYABLE_PROVIDER_ERROR_PATTERN`。
- `retryDelayMs(policy, attempt)`：`delay = baseDelayMs * 2^max(0, attempt-1)`；
  非安全整数则 `Number.MAX_SAFE_INTEGER`；`min(delay, maxAgentDelayMs ?? 60000)`。
- `clampThinkingLevel(model, level)`：不可用则取「`EXTENDED_THINKING_LEVELS` 中不高于请求值的最近可用级别」，
  找不到则 `availableLevels[0] ?? "off"`。
- `getSupportedThinkingLevels(model)`：`!model.reasoning` → `["off"]`；否则过滤
  `EXTENDED_THINKING_LEVELS`：`thinkingLevelMap[level] === null` 排除；`xhigh`/`max` 仅当映射有定义时保留。
- `modelsAreEqual(a,b)`：两者非空、`getModelType` 相同、id 与 provider 相同。
- `contentText(content, fallback)`：把字符串/内容数组中的文本拼接（跨单元实现）。
- `getCurrentSystemMessage(messages)`：把 transcript 中所有 system 消息重放成一条头部消息
  （`content` 以 `"\n\n"` 连接，`sections` 按名 patch，`toolsAdded` 由 `getCurrentTools` 解析）。
- `getCurrentTools(messages)`：用 `Map<string, Tool>` 依序重放**所有** system 消息：每条先 `toolsRemoved`
  逐个 `delete(name)`，再 `toolsAdded` 逐个 `set(name, tool)`，返回 `[...map.values()]`。
  因此顺序**不是**「首次出现顺序」：`Map.set` 已存在的键会保留**原位置**、只更新值，而
  `delete` 后再 `set` 会移动到**末尾**。即某工具被移除后重新加入会排到最后；值总是最后一次 `toolsAdded`
  的对象。Rust 用 `IndexMap` 时必须复刻「insert 保留位置 / remove 后 insert 追加」的语义。
- `getCurrentSystemMessage(messages)` 的边界：`timestamp ??= 第一条 system 消息的 timestamp`
  （可为 0）；若**没有任何** system 消息且工具列表为空 → 返回 `undefined`；否则返回
  `{role:"system", content: content.join("\n\n"), ...(sections.size>0?{sections}:{}),
  ...(tools.length>0?{toolsAdded:tools}:{}), timestamp: timestamp ?? 0}`。
  空字符串 content 段被跳过（`if (text.length > 0)`），`sections` 的 `null` 值表示删除该键。
- `collapseSystemMessages(context)`：把重放出的头部 system 消息放在最前，丢弃后续所有 system 消息。
  `resolveTranscript` 视 provider 是否支持 mid-conversation system messages 决定是否折叠。
- `cleanupSessionResources(sessionId)`：清理该会话的临时资源（如 bash 输出文件/缓存），由 ai/coding-agent 实现。

### 5.3 `core/extensions`（扩展系统）

`ExtensionRunner` 本单元使用的方法：

| 方法 | 契约 |
|---|---|
| `hasHandlers(eventType: string): boolean` | 是否有该事件的处理器 |
| `emit(event): Promise<unknown>` | 依次 await 所有处理器，返回最后一个非 undefined 结果（用于可取消事件） |
| `emitBoundary(event, buildContext): Promise<BoundaryResult>` | 边界事件；`buildContext(drafts)` 供处理器查询预览 |
| `emitMessageEnd(event): Promise<AgentMessage \| undefined>` | 可返回替换消息 |
| `emitToolCall(event)` / `emitToolResult(event)` | 工具拦截 |
| `emitInput(text, images, source, streamingBehavior?)` | 返回 `{action:"handled"\|"transform"\|"continue", text?, images?}` |
| `emitBeforeAgentStart(trigger, baseOptions)` | 返回 `{messages, systemPromptOptions}` |
| `emitResourcesDiscover(cwd, reason)` | 返回 `{skillPaths, promptPaths, themePaths}`（元素 `{path, extensionPath}`） |
| `emitError(error: ExtensionError)` | `{extensionPath, event, error, stack?}` |
| `invalidate(reason?: string)` | 使 ctx 失效（后续调用抛错） |
| `reportUnhandledMcpServers()` | 报告未处理的 MCP server |
| `getAllRegisteredTools(): RegisteredTool[]` | `{definition, sourceInfo}` |
| `wrapRegisteredTools(registered, runner)` | 把注册工具包装成 `AgentTool` |
| `resolveToolRenderers(name, base)` | HTML 导出渲染器 |
| `createCommandContext(): ExtensionCommandContext` | 命令上下文（含 `newSession`/`fork`/`switchSession`/`reload`/`withSession` 等） |
| `bindCore(core, context, providers)` | 注入本单元实现 |
| `bindCommandContext(actions?)` | 注入命令上下文动作 |
| `setUIContext(ctx?, mode?)` | 设置 UI 上下文 |
| `onError(listener): () => void` | 错误订阅，返回退订 |
| `getFlagValues(): Map<string, boolean|string>` | 扩展 flag 值 |
| `getCommand(name)` / `getRegisteredCommands()` | 命令查询 |

`emitSessionShutdownEvent(runner, event)`：包装 `session_shutdown` 事件（`runner.ts` 提供）。

`SessionBoundaryDraft`：`CustomEntryDraft | CustomMessageEntryDraft | ContextEditEntryDraft | CompactionEntryDraft`。

`BoundaryContextPreview`：`{ contextEntries, contextMessages, llmMessages, pendingMessages, canContinue }`。

`ToolDefinition` 关键字段：`name`、`label`、`description`、`promptSnippet?`、`promptGuidelines?`、
`parameters`、`constrainedSampling?`、`renderShell?`、`prepareArguments?`、`outputSchema?`、
`exposure?: "direct"|"model-only"|"codemode"|"deferred"|"hidden"`、`namespace?`、`reachesTools?`、
`annotations?`、`prepareLoadout?: (loadout: ToolLoadout) => ToolLoadoutChanges | undefined`、
`defaultActive?: boolean`（`false` 时注册不自动激活）。

`ToolLoadout`：`{ declared, callable, registered, getExposure(name), getNamespace(name), getPromptGuidelines(name) }`。
`ToolLoadoutChanges`：`{ descriptions?: Record<string,string>, hiddenDeclarations?: readonly string[] }`。

### 5.4 `core/model-runtime.ts`（ModelRuntime）

| 方法 | 契约 |
|---|---|
| `static create({authPath, modelsPath, signal?})` | 构建并（可能）联网刷新 |
| `getModel(provider, id)` / `getModels(provider?)` / `getProvider(id)` | 目录查询（同步，返回最后已知） |
| `getPhysicalModel(provider, id)` | 非虚拟模型的目录项 |
| `getAvailableSnapshot()` | 当前可用模型快照（数组） |
| `hasConfiguredAuth(provider)` | 是否配置了凭证 |
| `checkAuth(provider, options?)` | 异步检查，返回状态或 undefined |
| `isUsingOAuth(provider)` / `isUsingSubscription(provider)` | 鉴权类型 |
| `getAuth(modelOrProvider, overrides?)` | 返回 `AuthResult \| undefined`：`{auth: {apiKey?, headers?, baseUrl?}, env?, source?}` |
| `refresh({allowNetwork?, providers?, signal?})` | 刷新目录 |
| `resolveModel(model, messages, {reason, thinkingLevel, signal?, failed?, state?})` | 虚拟模型路由，返回 `{model, thinkingLevel, state?}`；失败抛错 |
| `registerProvider(name, config)` / `registerNativeProvider(p)` / `unregisterProvider(name)` | 动态 provider |
| `registerVirtualModel(def)` / `unregisterVirtualModel(provider, id)` | 虚拟模型 |
| `stream` / `streamSimple` / `complete` / `completeSimple` | 请求入口 |

`resolveModel` 的错误文本：`Virtual model ${provider}/${id} is not registered.`、
`${name} routed to ${p}/${m}, which is not a physical model.`、`... which has no credentials.`。

### 5.5 `core/settings-manager.ts`（本单元调用的访问器）

`getSettings()`、`isProjectTrusted()`、`async reload()`、`setDefaultModelAndProvider(provider, modelId)`、
`getEnabledModels()`、`setEnabledModels(list)`、`getSteeringMode()` / `setSteeringMode(mode)`、
`getFollowUpMode()` / `setFollowUpMode(mode)`、`getTheme()`、`getDefaultThinkingLevel()`、
`setDefaultThinkingLevel(level)`、`getModelThinkingLevel(provider, modelId)`、
`getCompactionSettings(model?)` → `{enabled, reserveTokens, keepRecentTokens}`、
`getCompactionEnabled()` / `setCompactionEnabled(b)`、`getBranchSummarySettings()` → `{reserveTokens, skipPrompt}`、
`getRetryEnabled()` / `setRetryEnabled(b)`、`getRetrySettings()` → `{enabled, maxRetries, baseDelayMs, maxAgentDelayMs}`、
`setCacheWarmingMode(mode)`、`getImageAutoResize()`、`getShellPath()`、
`getShellCommandPrefix()`、`getDefaultTools(): string[] | undefined`、
`getEnabledModels(): string[] | undefined`、`setEnabledModels(patterns: string[] | undefined)`、
`applyToolModifiers(list, modifiers)`。

`applyToolModifiers(base, entries)` 的精确语义（本单元在 `_restoreToolsFromTranscript` 与 `reload` 中使用）：

```ts
const tools = [...base];
for (const entry of entries) {
  if (!(typeof entry === "string" && (entry.startsWith("+") || entry.startsWith("-")))) continue;
  const name = entry.slice(1);
  const index = tools.indexOf(name);
  if (entry.startsWith("+") && index === -1 && name) tools.push(name);
  else if (entry.startsWith("-") && index !== -1) tools.splice(index, 1);
}
return tools;
```

即：`+name` 追加（去重、空名忽略），`-name` 删除第一次出现，非修饰项忽略，按列表顺序依次应用。
`isToolModifier(entry)` = 字符串且以 `+` 或 `-` 开头。

默认值（`SETTINGS_DEFAULTS`）：`steeringMode: "one-at-a-time"`、`followUpMode: "one-at-a-time"`、
`compaction: {enabled:true, reserveTokens:16384, keepRecentTokens:20000}`、
`branchSummary: {reserveTokens:16384, skipPrompt:false}`、
`retry: {enabled:true, maxRetries:3, baseDelayMs:2000, maxAgentDelayMs:60000,
        provider:{maxRetryDelayMs:60000}}`（**嵌套的 `retry.provider` 原规格漏了**；旧键
`retry.maxDelayMs` 在 `SettingsManager` 构造时迁移为 `retry.provider.maxRetryDelayMs` 并删除旧键）、
`images: {autoResize:true, blockImages:false}`、`cacheWarming: "streaming"`。本单元只用其中
`steeringMode`/`followUpMode`/`compaction`/`branchSummary`/`retry`/`images.autoResize`/`cacheWarming`，
其余（transport、terminal、markdown、codemode、tuiMode、fullscreen*、defaultProjectTrust、warnings 等）
属于其他单元，不要在本 crate 重复定义。

`DEFAULT_TOOL_NAMES = ["read","bash","edit","write"]`（`settings-manager.ts` **导出** `readonly string[]`）。

`getDefaultTools()` = `settings.defaultTools === undefined ? undefined : resolveDefaultTools(过滤后的字符串数组)`；
`resolveDefaultTools(entries)` = 先取全部「非 `+/-` 前缀」的普通名字，若非空则以此为基础，否则以
`DEFAULT_TOOL_NAMES` 为基础，再 `applyToolModifiers(base, entries)` 按顺序应用修饰项。
即：**纯普通名单替换默认值；纯修饰名单在默认值上增删；混合名单不合法**（由
`getToolListError` 拒绝：`tool names cannot be mixed with +name or -name entries`；
修饰项含 `*` 时：`+name and -name entries take exact tool names, not patterns: ${pattern}`）。
`deepMergeSettings` 里 `mergeDefaultTools(base, overrides)`：覆盖值为纯修饰列表则**追加**到基列表后，
否则整体替换（非数组或非纯修饰时直接替换，不抛错，因为 settings 文件不校验）。

`getCompactionSettings(model?)` 的完整优先级：对 `reserveTokens` 与 `keepRecentTokens` **各自**
`modelOverrides["<provider>/<id>"][field]` → 普通 `compaction[field]` → 内置默认（16384 / 20000）；
非法值（非 number、非安全整数、负数）分别抛
`Invalid compaction.${field} setting: ${String(ordinary)}. Expected a non-negative safe integer.` /
`Invalid compaction.modelOverrides["${modelKey}"] setting: ... Expected an object.` /
`Invalid compaction.modelOverrides["${modelKey}"].${field} setting: ... Expected a non-negative safe integer.`。
`enabled` 只走全局 `getCompactionEnabled()`（**不**按模型覆盖）。

### 5.6 `core/compaction/*`（压缩算法）

本单元调用（接口契约）：

- `prepareCompaction(pathEntries, settings): CompactionPreparation | undefined`
- `compact(preparation, model, apiKey, headers, customInstructions, signal, thinkingLevel, streamFn, env,
  retry, callbacks, sessionId): Promise<CompactionResult>`
- `CompactionResult<T> = { summary, firstKeptEntryId, tokensBefore, estimatedTokensAfter?, usage?, details? }`
- `calculateContextTokens(usage)`：`usage.totalTokens || input+output+cacheRead+cacheWrite`
- `estimateContextTokens(messages): { tokens, usageTokens, trailingTokens, lastUsageIndex: number|null }`
- `estimateProjectedContextTokens(projection, branchEntries): ContextUsageEstimate`（见 2.2.13 与源码逻辑）
- `estimateTokens(message)`：chars/4 上取整；system 额外加 sections 与 `JSON.stringify(toolsAdded).length`；
  图片块固定 `ESTIMATED_IMAGE_CHARS`（4800）
- `shouldCompact(contextTokens, contextWindow, settings)`：`contextTokens > contextWindow - settings.reserveTokens`
- `collectEntriesForBranchSummary(sessionManager, oldLeafId, targetId): { entries, commonAncestorId }`
- `generateBranchSummary(entries, options): Promise<{ summary?, usage?, readFiles?, modifiedFiles?, aborted?, error? }>`
- `DEFAULT_COMPACTION_SETTINGS = { enabled: true, reserveTokens: 16384, keepRecentTokens: 20000 }`

### 5.7 其他本单元依赖

- `core/system-prompt.ts`：`buildSystemPrompt(options)`、`buildSystemPromptSections(options)`、
  `diffSystemPromptSections(previous, current)`、`normalizeBuildSystemPromptOptions(input)`，
  类型 `NormalizedBuildSystemPromptOptions`（含 `selectedTools`、`hiddenTools`、`toolSnippets`、
  `toolGuidelines`、`forceSystemPrompt?`、`sections`、`cwd`、`contextFiles`、`skills`）。
- `core/resource-loader.ts`：`getExtensions()`、`getSkills()`、`getAgentsFiles()`、`getPrompts()`、
  `getSystemPrompt()`、`getAppendSystemPrompt()`、`extendResources(paths)`、`async reload(options?)`。
- `core/tools/*`：`createAllToolDefinitions(cwd, {read:{autoResizeImages}, bash:{commandPrefix, shellPath}})`
  返回 `read/bash/powershell/edit/write/grep/find/ls`；`createToolDefinitionFromAgentTool(tool)`；
  `BashOperations { exec(command, cwd, {onData, signal}): Promise<{exitCode: number|null}> }`；
  `createLocalBashOperations({shellPath})`；`DEFAULT_MAX_BYTES = 50*1024`；`truncateTail(content, options?)`。
- `core/export-html`：`exportSessionToHtml(sessionManager, state?, options?)`、`createToolHtmlRenderer(...)`、
  `ToolHtmlRenderer`。导出错误：`Cannot export in-memory session to HTML`、`Nothing to export yet - start a conversation first`。
- `core/bug-report.ts`：`generateBugReportSummary({model, apiKey?, headers?, env?, thinkingLevel?, messages, hint?,
  signal, streamFn, retry, sessionId})`。
- `core/cache-warmer.ts`：`CacheWarmingStatus`、`CacheWarmingMode`、`CacheWarmer` 的四方法子集
  （`cancel`、`status`、`onAgentSettled`、`onModeChanged`、`onWarmed`（可赋值））。
- `core/virtual-models.ts`：`isVirtualModel(model)`、`findLatestResponse(messages)`、
  `getBranchSelection(branch, getModel)`、`getVirtualModelState(branch, provider, modelId)`、
  `VIRTUAL_MODEL_STATE_ENTRY = "kiana.virtual-model-state"`、`VirtualModelStateData {provider, modelId, state}`、
  `VIRTUAL_MODEL_API = "kiana-virtual"`。
- `core/mcp-servers.ts`：`createToolNameMatcher(entries)`（`*` 通配，其余字符转义）、`isMcpToolName(name)`
  （`mcp__` 前缀或三个资源工具名）。
- `core/source-info.ts`：`SourceInfo`、`BUILTIN_PATH_PREFIX = "builtin:"`、`isSyntheticPath(path)`、
  `createSyntheticSourceInfo(path, metadata)`、`createSourceInfo(path, metadata)`。
- `core/usage-totals.ts`：`createUsageTotals()`、`addUsageToTotals(totals, usage)`、`combineUsage(a,b)`。
- `core/prompt-templates.ts`：`expandPromptTemplate(text, templates)`、`PromptTemplate`。
- `core/auth-guidance.ts`：`getProviderLoginHelp()`、`formatNoApiKeyFoundMessage(provider)`、
  `formatNoModelSelectedMessage()`、`formatNoModelsAvailableMessage()`（逐字文本见 3.1）。
- `core/settings-manager.ts` 的辅助导出：`DEFAULT_TOOL_NAMES`、`isToolModifier(entry)`、
  `applyToolModifiers(base, entries)`、`getToolListError(entries)`、`CACHE_WARMING_MODES = ["off","streaming","idle"]`。
- `core/defaults.ts`：`DEFAULT_THINKING_LEVEL = "medium"`、
  `THINKING_LEVEL_OPTIONS = ["off","minimal","low","medium","high","xhigh","max"]`。
- `src/utils/*`：`normalizePath`、`resolvePath`、`sleep(ms, signal)`（中止时 reject `Error("Aborted")`）、
  `stripAnsi`、`splitIncompleteAnsiSuffix`、`sanitizeBinaryOutput`、`createOutputFileStream(prefix, ext)`、
  `stripFrontmatter`、`processImage(bytes, mimeType, opts)`、`normalizeToolResultImages(content, opts)`、
  `waitForChildProcess(proc)`。
- `src/config.ts`：`APP_NAME`、`CONFIG_DIR_NAME`、`ENV_AGENT_DIR`、`ENV_SESSION_DIR`、`getAgentDir()`、
  `getSessionsDir()`、`getDocsPath()`。

---

## 6. npm 依赖与 Rust crate 映射

`packages/coding-agent/package.json` 中与本单元相关的依赖（其余见其他单元规格）。

| npm 包 | 用途（本单元） | Rust 等价 |
|---|---|---|
| `node:crypto`（`randomUUID`, `randomBytes`） | id 生成、bash 输出文件名 | `uuid`（`Uuid::new_v4()`、`now_v7()` 需 `v7` feature）；`rand`（`rand::random::<[u8;8]>()` + `hex`） |
| `node:fs` / `node:fs/promises` | 会话文件读写、stat、readdir、copyFile、mkdir | `std::fs`、`tokio::fs`；`fs2`/`rustix` 用于 `O_EXCL`（`OpenOptions::new().create_new(true)`）；`std::os::unix::fs::PermissionsExt` 设 0600 |
| `node:path` | join/resolve/dirname/basename/parse/relative | `std::path::{Path, PathBuf}`；`path-clean` 或自行实现 `resolve`（注意 JS 语义见第 8 节） |
| `node:os`（`homedir`, `tmpdir`） | 家目录、临时目录 | `dirs` / `home` crate；`std::env::temp_dir()` |
| `node:readline`（`createInterface`） | 逐行读会话文件 | `tokio::io::BufReader` + `AsyncBufReadExt::lines()`（需处理 `\r\n`） |
| `node:string_decoder`（`StringDecoder`） | 增量 UTF-8 解码 | `encoding_rs` 的 `Decoder`（`new_decoder_without_bom_handling()`）或自写 `StringDecoder`（保留 0–3 字节不完整尾） |
| `node:stream`（`WriteStream`） | bash 临时文件流 | `tokio::fs::File` / `std::fs::File` + `BufWriter` |
| `node:child_process`（`spawn`, `spawnSync`） | exec / shell 查找 | `tokio::process::Command`；`which` crate |
| `node:util` `TextDecoder` / `TextEncoder` | UTF-8 编解码、字节计数 | `String::from_utf8_lossy`（注意 `TextDecoder` 默认非 fatal，非法序列替换为 U+FFFD）、`str::as_bytes().len()` |
| `node:events`（`EventEmitter`） | event-bus | `tokio::sync::broadcast` 或自建 `HashMap<String, Vec<Handler>>`；`tokio::sync::mpsc` |
| `node:perf_hooks`（`performance.now()`） | 嵌套调用耗时 | `std::time::Instant`（单调） |
| `string-width`（间接） | 清洗二进制输出（U+FFF9..FFFB 会导致其崩溃） | 本单元只需删除这些字符；若 Rust 侧有宽度计算，用 `unicode-width` |
| 无（`uuid` 由 kiana-ai 提供 `uuidv7()`） | 会话 id | `uuid` crate `Uuid::now_v7()` |

注释：

- Node 22 内置 `fetch`、`AbortController`、`structuredClone` 等不需要 crate；Rust 侧用
  `tokio_util::sync::CancellationToken` 或 `tokio::select!` + `oneshot` 传播取消。
- `TextDecoder` 的 `{stream:true}` 语义 = 保留不完整多字节序列到下次解码；`decode()` 无参 = flush。
  Rust 里可用 `encoding_rs::Decoder::decode_to_string` 配合 `last` 参数，或自写 3 字节缓冲。
- 本单元**不**引入 `reqwest`/`serde` 之外的网络栈；网络由 model-runtime / ai-api 单元负责。

### 6.1 行为差异清单（必须显式处理）

1. **字符串长度**：JS `String.length` 与 `text.length` 是 UTF-16 码元数；Rust `str::len()` 是字节数，
   `chars().count()` 是码点数。凡是用到 `.length` 的预算（bash 滚动缓冲、ANSI 挂起窗口 256、
   `maxErrorChars` 500、token 估算 `chars/4`）都需要 `encode_utf16().count()` 或等价。
2. **字符串切片**：`slice(n)` / `slice(a,b)` 按 UTF-16 索引；Rust 需按码元索引到码点边界（可能切在代理对中间，
   JS 会产生孤立代理；Rust `char` 无法表示）。这些位置包括：`text.slice(1)`、`text.slice(spaceIndex+1)`、
   `text.slice(7)`、`errorText.slice(0, 500)`、`randomUUID().slice(0,8)`（ASCII 安全）。
   实践上可对非 ASCII 边界做「码元就近」近似，但 `parseSkillBlock` 等正则位置需保持一致。
3. **正则**：`parseSkillBlock` 使用 `[\s\S]*?` 与可选组，`regex` crate 支持（`.` 不匹配换行时用 `(?s)`）。
   `createToolNameMatcher` 把 `*` 转成 `.*`，其余字符用 `regex::escape`。会话 id 校验是简单的字符类。
   **不可用**：反向引用、lookbehind（本单元未使用）。
4. **`JSON.stringify` 数字格式**：JS 输出 `1`、`1.5`、`1e+21`、`-0`→`0`；serde_json 输出 `1.0`（对 f64）
   与 `1e21`。会话文件中的 `tokensBefore`、`tokens`、`cost` 等通常是整数，但 `cost.total` 可能是小数，
   必须自行格式化以匹配 JS 的 `Number::toString`（最少位数往返表示）。建议所有序列化数值走自定义
   `serialize_with`。
5. **`Date.prototype.toISOString()`**：`YYYY-MM-DDTHH:mm:ss.sssZ`；年份 4 位、毫秒 3 位零填充。
   `chrono` 的 `to_rfc3339_opts(SecondsFormat::Millis, true)` 输出 `2026-02-12T09:31:05.123Z`，匹配。
   负年份/超范围年份 JS 会抛 RangeError，Rust 需对齐（极少触发）。
6. **`new Date(iso).getTime()`**：无效输入 → NaN；比较型代码（`timestamp <= new Date(...).getTime()`）
   在 NaN 时恒为 false。Rust 用 `Option<i64>` 表达，比较时 NaN → false。
7. **`localeCompare`**：`listAll` 与 `listSessionsFromDir` 的同 mtime 排序依赖 ICU locale 比较。
   默认 Node 使用系统 ICU（en-US 时等价于 Unicode 码点序，但对数字/变音符有差异）。建议 Rust 用
   `icu_collator`（`Collator::try_new(&locale!("en-US"), Default::default())`）或退化为 `str::cmp`
   并在规格中记录风险。
8. **`Math.round`**：`.5` 向 +∞ 取整（不是银行家舍入）。Rust `f64::round()` 行为一致（远离零），
   对正数相同；负数 `-0.5.round()` 在 JS 是 `-0`，Rust 是 `-1`。`durationMs` 恒非负，安全。
9. **`performance.now()`**：单调、毫秒浮点；Rust 用 `Instant::elapsed().as_secs_f64() * 1000.0`。
10. **`WeakMap`/`WeakSet` 身份**：Rust 需用 `u64` 消息句柄或 `Arc` 指针身份；替换消息时必须保句柄。
11. **`wx` / `w` 文件标志**：`create_new(true)` / `truncate(true)`；`appendFileSync` = `OpenOptions::append(true).create(true)`。
12. **`Object.keys` 顺序**：字符串键按插入顺序（非整数键），整数样键按数值升序——Rust 的
    `IndexMap` 只保证插入序，因此凡是对含整数样键的普通对象做 `Object.keys` 的地方（本单元基本没有）
    需额外排序。会话条目字段都是非整数键，安全。
13. **`structuredClone` 缺失**：本单元用展开运算符浅拷贝；Rust 用 `Clone`，注意 `Arc` 与深拷贝差异。
14. **异常 vs Result**：JS 的 catch 面很宽（`catch {}` 吞掉一切）。Rust 需显式把每处 `catch` 映射为
    `match`/`if let Err`，保持「吞掉」语义（尤其 `dispose()` 的 try/catch 与 `buildSessionInfo`）。
15. **`AbortSignal.throwIfAborted()`**：抛出 `DOMException`/`AbortError`；Rust 用自定义
    `Error::Aborted`，并在需要时保留消息文本（用户可见处多为 `undefined` errorMessage）。
16. **`String(chunk)`**：Node `process.stdout.write` 接收 Buffer 时会 `toString()`（UTF-8，非 fatal）。
17. **`JSON.parse` 宽松性**：允许重复键（后者胜）、大整数丢精度（> 2^53）。Rust serde_json 对
    `u64`/`f64` 的处理需与 JS 对齐（`tokensBefore` 等按 `i64` 或 `f64` 存储）。

---

## 7. 测试清单

路径相对 `/home/ctyun/code/kiana_rev/kiana/packages/coding-agent/`。列出的每个文件都必须移植为 Rust 测试，
用例名与断言语义保持一致（Rust 测试名可用 `mod::case` 形式）。

### 7.1 单元测试（`test/`）

#### `test/session-manager/build-context.test.ts`（16 例）

- empty entries returns empty context
- single user message
- simple conversation
- tracks thinking level changes
- tracks model from assistant message
- tracks model from model change entry
- includes summary before kept messages
- handles compaction keeping from first message
- multiple compactions uses latest
- buildContextEntries returns compaction-aware entries including custom entries
- keeps settings from the full path after compaction
- follows path to specified leaf
- includes branch summary in path
- complex tree with multiple branches and compaction
- uses last entry when leafId not found
- handles orphaned entries gracefully

#### `test/session-manager/custom-session-id.test.ts`（13 例）

- uses the provided id instead of generating one
- uses the provided id when creating an in-memory session
- allows alphanumeric session ids with interior punctuation
- rejects invalid custom session ids
- generates a UUIDv7 id when no id is provided
- generates a UUIDv7 id when options is provided without id
- includes the custom id in the session header
- generates a UUIDv7 id when constructed without an explicit id
- uses the provided id when creating a persisted session
- generates a UUIDv7 id when creating a branched session
- generates a UUIDv7 id when forking from another session file
- uses the provided id when forking from another session file
- copies all entries unchanged into the forked session

#### `test/session-manager/file-operations.test.ts`（30 个 `it` + 1 个 `it.each`×3 = 33 次执行）

- returns empty array for non-existent file
- returns empty array for empty file
- returns empty array for file without valid session header
- returns empty array for malformed JSON
- loads valid session file
- skips malformed lines but keeps valid ones
- adds a newline after an unterminated valid record
- adds a newline after an unterminated malformed final fragment
- does not modify an unterminated non-session file
- opens compatible sessions beyond the discovery scan limit
- opens session files larger than Node's max string length
- returns null for empty directory（findMostRecentSession）
- returns null for non-existent directory
- ignores non-jsonl files
- ignores jsonl files without valid session header
- returns single valid session file
- returns most recently modified session
- skips invalid files and returns valid one
- skips oversized corrupt files and returns a valid session
- filters most recent session by cwd
- scopes current-folder APIs by cwd while listing all flat sessions
- rejects a cancelled session listing
- truncates and rewrites empty file with valid header
- throws and preserves non-empty file without valid header
- throws and preserves non-session JSONL files
- preserves explicit session file path when recovering from corrupted file
- subsequent loads of initialized empty file work correctly
- does not create a file for a session with only setup entries
- creates the file when the first user message is appended
- appends later entries to the file without rewriting earlier ones

#### `test/session-manager/labels.test.ts`（9 例）

- sets and gets labels
- clears labels with undefined
- last label wins
- labels are included in tree nodes
- labels are preserved in createBranchedSession
- rewires children of removed labels when forking
- labels not on path are not preserved in createBranchedSession
- labels are not included in buildSessionContext
- throws when labeling non-existent entry

#### `test/session-manager/load-entries.test.ts`（13 例）

- adopts entries verbatim
- keeps the loaded leaf so appends continue the conversation
- never mints an id that collides with a loaded entry
- rebuilds the branch structure rather than a flat chain
- rebuilds labels
- resolves a compaction against the entry it was written against
- creates a header from the options when the entries carry none
- generates a session id when the options carry none
- stays off the filesystem
- starts an empty session when the entries are empty
- takes the session identity from a header among the entries
- migrates entries restored with an older header
- adopts headerless entries as current-version without migrating them

（该文件覆盖 `SessionManager.inMemory(cwd, options, entries)` 的「从外部条目加载」路径。）

#### `test/session-manager/migration.test.ts`（2 例）

- should add id/parentId to v1 entries
- should be idempotent (skip already migrated)

#### `test/session-manager/save-entry.test.ts`（1 例）

- saves custom entries and includes them in tree traversal

#### `test/session-manager/tree-traversal.test.ts`（31 例，逐字）

- appendMessage creates entry with correct parentId chain
- appendThinkingLevelChange integrates into tree
- appendModelChange integrates into tree
- appendCompaction integrates into tree
- appendCustomEntry integrates into tree
- leaf pointer advances after each append
- returns empty array for empty session（describe "getPath"，即 `buildSessionPath`）
- returns single entry path
- returns full path from root to leaf
- returns path from specified entry to root
- returns empty array for empty session（getTree）
- returns single root for linear session
- returns tree with branches after branch
- handles multiple branches at same point
- handles deep branching
- moves leaf pointer to specified entry
- throws for non-existent entry（branch）
- new appends become children of branch point
- inserts branch summary with the source and destination and advances leaf
- throws for non-existent entry（branchWithSummary）
- returns undefined for empty session（getLeafEntry）
- returns current leaf entry
- returns undefined for non-existent id
- returns entry by id
- returns messages from current branch only
- throws for non-existent entry（createBranchedSession）
- creates new session with path to specified leaf (in-memory)
- extracts correct path from branched tree
- does not duplicate entries when forking from before the first user message
- preserves tool and summary usage across a file-backed reload
- writes file immediately when forking at a user message

#### `test/session-context-edit.test.ts`（18 例）

覆盖：appendContextEdit 的校验与归一化；projection 替换/省略；多个 edit 最新生效；
branch 相对性（导航回 edit 之前恢复原文）；compaction 与 edit 交互；`_omitRecoveryAttempt` 的
source-entry 匹配；工具结果编辑；custom_message 编辑；`system` 不可编辑等。

#### `test/nested-tool-calls.test.ts`（6 例，逐字）

1. assigns ids below the caller, emits events with the parent id, and records the calls
2. records calls of nested tools on the model-issued call
3. sums the usage of nested results at every depth
4. serializes concurrent calls to sequential tools
5. omits oversized arguments and drops calls beyond the limit
6. caps the total argument size and marks unfinished calls incomplete

覆盖：`<callerId>/<n>` id 生成；嵌套的嵌套共享 recorder；`maxCalls` / 参数字节上限；
`complete` 标志；`usage` 汇总到 tool result；sequential 队列串行化（含 `holdsQueue` 自锁规避）；
`tool_execution_*` 事件带 `parentToolCallId`。

#### `test/exec.test.ts`（2 例，逐字）

1. reports spawn errors in stderr
2. keeps stderr empty when a command exits silently

即：该文件只覆盖「spawn 失败（ENOENT）→ code 1、killed=false、stdout=""、stderr 含错误」与
「命令静默以非零码退出 → {stdout:"",stderr:"",code:1,killed:false}」两条**进程失败/静默**路径。
跨块多字节 UTF-8 解码与 abort/timeout kill 在本仓库**没有**测试（`StringDecoder` 逻辑见 2.10，
需在 Rust 侧自行补测）。第一条对应上游 issue #11085。

#### `test/session-cwd.test.ts`（3 例，逐字）

1. detects missing session cwd from persisted sessions
2. supports overriding the effective cwd when opening a session
3. throws a controlled error before runtime creation when the stored cwd is missing

覆盖：`getMissingSessionCwdIssue` 对持久化会话缺失 cwd 的检测；`SessionManager.open(..., cwdOverride)`
覆盖后不再报错；`createAgentSessionRuntime` 在调用 `createRuntime` **之前**抛 `MissingSessionCwdError`。
（不是「内存会话不报错 / prompt 文本」的直接用例。）

#### `test/session-file-invalid.test.ts`（1 例，逐字）

`prints a friendly error and preserves non-session file content`
——非空且非 kiana 会话的文件 → 抛错且文件内容不变。

#### `test/session-id-readonly.test.ts`（6 例，逐字）

1. does not persist a custom ID for metadata commands
2. creates missing IDs and reopens existing IDs in process
3. looks up exact IDs without building full session listings
4. reopens an exact ID from a renamed session file
5. filters exact IDs by cwd in a custom session directory
6. rejects an existing fork target in process

覆盖：自定义 id 的只读性（元数据命令不落盘 custom id）；`findById` 精确查找不走全量列表；
重命名文件后按 id 重开；自定义 sessionDir 下的 cwd 过滤；fork 目标已存在的拒绝。

#### `test/sdk-session-manager.test.ts`（4 例，逐字）

1. uses agentDir for the default persisted session path
2. keeps an explicit sessionManager override
3. derives cwd from an explicit sessionManager when cwd is omitted
4. exposes current session state to the built-in bash tool

覆盖：`createAgentSession` 的默认 `agentDir/sessions/--<encoded-cwd>--` 路径、显式
`sessionManager` 覆盖、cwd 从 sessionManager 派生、bash 工具注入 `KIANA_SESSION_*` 环境变量。

#### `test/startup-session-name.test.ts`（1 例，逐字）

`sets --name on the selected session before runtime model validation`
——子进程跑 CLI：`--session <file> --name "  CLI Named Session  " --model missing-model -p hi`，
退出码 1，且会话文件中恰好写入一条 `session_info`，name 已被 trim 为 `CLI Named Session`。
（被测语义是「`--name` 在模型校验**之前**写入所选会话」；不是单独验证 `getSessionName`。）

#### `test/bash-execution-width.test.ts`（2 例，逐字）

1. collapsed preview lines respect render-time width, not construction-time width
2. re-computes lines when width changes between renders

所属 describe：`BashExecutionComponent width handling (#2569)`。这是 **TUI 渲染层**（bash 输出组件的
行折叠/宽度重算），不是 ANSI 清洗；不要把它当作 bash-executor 的 ANSI 测试。

#### `test/compaction-nested-calls.test.ts`（1 例，逐字）

`include files touched by nested calls recorded on tool results`（describe `compaction file operations`）
——`extractFileOpsFromMessage` 对 `toolResult.nestedCalls.calls` 也提取 `read`/`write`/`edit` 路径
（`nestedCalls` 本身不进入 LLM 文本，只影响摘要里的 `<read-files>` / `<modified-files>`）。

#### `test/session-info-modified-timestamp.test.ts`（1 例，逐字）

`uses last user/assistant message timestamp instead of file mtime`
——`buildSessionInfo` 的 `modified` 取最后 user/assistant 消息活动时间（而非文件 mtime）。

#### `test/agent-session-*.test.ts`（单元层）

| 文件 | 例数 | 覆盖点 |
|---|---|---|
| `agent-session-branching.test.ts` | 3 | forking from single message / in-memory forking in --no-session / fork from middle of conversation |
| `agent-session-compaction.test.ts` | 5 | 手动 compact、压缩后状态有效、持久化、内存态、compaction 事件 |
| `agent-session-stats.test.ts` | 9 | context usage、压缩后未知、post-compaction usage、branch summary usage、compaction usage、cache-warming usage 恰好一次、tool result usage、用量分组、零 usage 忽略 |
| `agent-session-retry.test.ts` | 6 | 瞬态错误重试成功、耗尽、延迟上限、message_end 延迟下 prompt 等待、network_error、重试产生工具调用 |
| `agent-session-runtime-events.test.ts` | 4 | session_before_switch/start、取消、beforeSessionInvalidate 顺序、session_before_fork |
| `agent-session-tree-navigation.test.ts` | 10 | 编辑文本、非 user 无文本、summarize、嵌套 user 的 summary 父节点、assistant 目标、abort、无 summarize、同位置 no-op、自定义指令、跨分支 |
| `agent-session-dynamic-tools.test.ts` | 4 | 动态注册、source metadata、无 promptSnippet 的 custom tool、bash spawn hook 前暴露状态 |
| `agent-session-concurrent.test.ts` | 7 | prompt 流式中抛错、steer/followUp、扩展 steering、完成后 prompt、tool_call 前等待事件、慢 handler 的顺序持久化 |
| `agent-session-auto-compaction-queue.test.ts` | 6 | 阈值压缩后恢复、不重复溢出恢复、忽略 stale usage、error 消息用最后成功 usage、无 usage 不压缩、仅保留的 pre-compaction usage 不压缩 |
| `agent-session-dynamic-provider.test.ts` | 5 | 运行中注册 provider / 刷新当前模型 |
| （另一份 `test/agent-session-compaction.test.ts` 见 suite） | | |

### 7.2 集成测试（`test/suite/`）

| 文件 | 例数 | 覆盖点 |
|---|---|---|
| `agent-session-boundaries.test.ts` | 26 个 `it` 名 + 1 个 `it.each`×3 = 29 次执行 | turn_end 与 agent_before_settle 的全部边界语义（见下） |
| `agent-session-queue.test.ts` | 15 | 队列语义全量 |
| `agent-session-prompt.test.ts` | 21 | prompt 全路径、图片、模板、错误 |
| `agent-session-bash-persistence.test.ts` | 16 | bash 记录/延迟/取消/流式/ANSI |
| `agent-session-retry-events.test.ts` | 15 | 重试与事件顺序 |
| `agent-session-compaction.test.ts` | 27 个 `it` + 1 个 `it.each`×2 = 29 次执行 | 压缩全路径（含扩展摘要、认证、长度恢复）；each 用例 `compacts after an oversized tool result in the same run (model override: %s)`（回归 #8133/#9740） |
| `agent-session-runtime.test.ts` | 12 | 会话替换、导入、fork |
| `agent-session-codemode.test.ts` | 25 | codemode / 嵌套调用（与 nested-tool-calls 单元交叉） |
| `agent-session-mcp.test.ts` | 44 个 `it` + 4 个 `it.each`×2 = 52 次执行 | MCP 工具注册与 allowlist；each 标题：`routes tools whose names differ only in - and _ (reverse: %s)`、`withdraws and restores %s MCP tools the server changes`、`prefers the mcp.json server over a registered %s`、`%s restored tools when an extension sets the loadout before they register` |
| `agent-session-mcp-oauth.test.ts` | 9 | MCP OAuth |
| `agent-session-model-extension.test.ts` | 18 | 扩展 setModel/registerProvider |
| `agent-session-compaction-model-overrides.test.ts` | 2 个 `it` + 1 个 `it.each`×4 = 6 次执行 | 按模型覆盖压缩设置；each 标题 `uses model token settings for %s compaction and extension preparation`，参数 `["manual","pre-prompt","post-run","overflow"]`；另两例：`uses the newly selected model without changing ordinary settings`、`captures model identity before awaiting summarization auth` |
| `agent-session-tool-orchestration.test.ts` | 3 | 工具编排 |
| `agent-session-tool-result-images.test.ts` | 1 | 工具结果图片归一化 |
| `lax-message-content.test.ts` | 6 | null/缺失 content 的容错 |
| `virtual-models.test.ts` | 13 | 虚拟模型路由与状态条目 |

`agent-session-boundaries.test.ts` 的 26 个 `it` 用例名（逐字；文件另有一个
`it.each(["steering","follow-up","both"])` 标题为 `preserves %s queue scheduling around a turn_end handoff`，
展开后共 3 次执行，总数 29）：

1. commits a retain-none turn_end compaction and explicitly continues once
2. keeps a boundary replacement verbatim through threshold compaction
3. keeps boundary input verbatim through threshold compaction when metadata follows it
4. refreshes canonical context before publishing boundary entry notifications
5. continues from an agent_before_settle custom message before final settlement
6. persists custom context queued by agent_end before pre-settlement continuation
7. keeps a pre-settlement follow-up deferred until the explicit continuation would stop
8. defers runs started by agent_settled handlers until every settled handler completes
9. does not let an invalid explicit continuation suppress natural tool continuation
10. dispatches actionable turn_end for synthetic run failures
11. does not compact from usage belonging to a boundary-omitted assistant
12. does not trigger successful-response overflow from usage captured before a boundary edit
13. does not trigger threshold compaction from post-edit usage captured before a later compaction
14. does not treat retained pre-compaction assistant usage as post-compaction usage
15. persists custom context sent during pre-settlement before continuing
16. does not consume queued input when pre-settlement drafts leave system-only context
17. commits pre-settlement drafts but suppresses continuation when aborted during the hook
18. keeps truncated tool attempts in context for the natural next turn
19. resets length recovery after a successful intermediate assistant turn
20. gives a distinct queued follow-up its own length-recovery budget
21. finishes retry bookkeeping when a retry receives a nonretryable error
22. omits a recoverable projected replacement by its source entry ID
23. recovers an explicit overflow error after a retained boundary replacement
24. keeps follow-up work behind an automatic error retry
25. marks the exhausted retry run as final
26. keeps omissions and does not retry when recovery compaction fails

补充：该文件有两个 `describe` 块——第 1–17 例（外加上面的 `it.each`）在
`AgentSession actionable boundaries`，第 18–26 例在 `durable length recovery`。

`agent-session-queue.test.ts` 的 15 个用例名：

1. dispatches extension commands immediately when prompted while idle
2. delivers extension-origin steering messages before the next LLM call
3. delivers follow-up messages only after the current run finishes
4. runs direct steering and follow-up messages through input handlers
5. delivers multiple steering messages in order in one-at-a-time mode
6. delivers multiple follow-up messages in order in one-at-a-time mode
7. delivers all steering messages in one batch in all mode
8. delivers all follow-up messages in one batch in all mode
9. queues custom messages with deliverAs steer while streaming
10. queues custom messages with deliverAs followUp while streaming
11. injects nextTurn custom messages into the next prompt
12. updates pendingMessageCount and removes queued text before message_start is emitted
13. throws when queueing an extension command with steer
14. throws when queueing an extension command with followUp
15. delivers follow-ups queued during agent_end

`agent-session-prompt.test.ts` 的 21 个用例名：

1. prompts while idle and records a single text response
2. handles a tool call turn and waits for the follow-up LLM response
3. executes multiple tool calls from one response and continues with a single follow-up response
4. preserves image attachments in the provider context
5. uses the model selected by before_agent_start for image normalization
6. expands skill commands before sending the prompt
7. expands prompt templates before sending the prompt
8. sendUserMessage can opt into prompt template expansion
9. dispatches extension commands without consuming a provider response
10. extension sendUserMessage can opt into extension command dispatch
11. sendUserMessage while idle triggers a turn
12. does not report streamingBehavior to input handlers while idle
13. reports streamingBehavior to input handlers while streaming
14. throws when prompted during streaming without a streamingBehavior
15. throws when prompted during manual compaction
16. throws when prompting without a model
17. throws when prompting without configured auth
18. keeps the same system prompt for every request of the run
19. queues a custom message sent while before_agent_start runs into the starting run
20. includes a custom message sent without triggerTurn while the run starts in its first request
21. aborts a run when abort is requested while before_agent_start runs

`agent-session-bash-persistence.test.ts` 的 16 个用例名：

1. records bash results immediately while idle
2. defers bash results while streaming and flushes them before the next prompt
3. executes bash commands and records the result
4. cancels running bash commands with abortBash
5. keeps newer bash execution tracked when an older execution finishes
6. aborts all active bash executions
7. persists user, assistant, toolResult, and custom messages in order
8. does not emit message_end for bash execution messages
9. persists aborted assistant messages
10. records bash output through custom operations
11. streams bash output to the callback and session events
12. strips a color reset split inside its parameters
13. strips a color code split right after ESC
14. strips an OSC sequence split before its terminator
15. flushes an incomplete multi-byte character at the end of output
16. does not hold back output behind a long unterminated sequence

`agent-session-retry-events.test.ts` 的 15 个用例名：

1. retries after a transient error and succeeds
2. retries multiple transient failures and succeeds on the final attempt
3. finalizes retry state when abort is requested after a retry attempt fails
4. exhausts max retries and emits a failure event
5. prompt waits for retry completion even when assistant message_end handling is delayed
6. does not retry when retry is disabled
7. does not retry non-retryable errors
8. cancels retry sleep when abortRetry is called
9. waits for the full loop when retry recovery produces tool calls
10. emits extension events before public event subscribers
11. emits the expected event order for a single prompt
12. emits the expected event order for a tool call turn
13. emits streaming deltas for text, thinking, and tool calls in message_update events
14. emits agent_end for error responses
15. emits agent_end for aborted runs and persists the aborted assistant message

`agent-session-compaction.test.ts` 的 27 个用例名：

1. manually compacts using an extension-provided summary
2. checkpoints the replayed system state and folds summarized and retained system patches into it
3. allows a queued prompt to start when manual compaction ends
4. throws when compacting without a model
5. throws when compacting without configured auth
6. manually compacts with a custom streamFn when registry auth is absent
7. manually compacts with provider-resolved bearer auth
8. uses the standalone compaction request context
9. persists usage from kiana-generated manual compaction
10. auto-compacts with a custom streamFn when registry auth is absent
11. notifies extensions when auto-compaction fails
12. compacts and resumes after a length stop below the desired output limit
12a. （`it.each([false,true])`，位于第 12 与 13 例之间，展开 2 次）
     `compacts after an oversized tool result in the same run (model override: %s)`
     覆盖 #8133（模型覆盖在 assistant turn 之间也生效）与 #9740（超大 trailing tool result 仍产生切点）；
     断言 `observedSettings` 为 `{enabled:true, reserveTokens:400, keepRecentTokens:1750}`、
     `compaction_start` 在 provider 请求之前、resumed request 同时含摘要与 `large-tool-result`。
13. includes steering queued during compaction in the resumed assistant request
14. does not compact after a terminating tool result
15. does not compact when a length stop reaches the desired output limit
16. stops after one compact-and-retry when a second response is also truncated
17. keeps overflow wording when a repeated length stop fills the context window
18. cancels in-progress manual compaction when abortCompaction is called
19. aborts an in-progress manual compaction and waits until the session is idle
20. resumes after threshold compaction when only agent-level queued messages exist
21. does not retry overflow recovery more than once
22. compacts successful overflow responses without retrying
23. ignores stale pre-compaction assistant usage on pre-prompt checks
24. triggers threshold compaction for error messages using the last successful usage
25. does not trigger threshold compaction for error messages when no prior usage exists
26. does not trigger threshold compaction when only kept pre-compaction usage exists
27. does not trigger threshold compaction below the threshold or when disabled

`agent-session-compaction-model-overrides.test.ts` 的用例（逐字）：`it.each(["manual","pre-prompt","post-run","overflow"])`
标题 `uses model token settings for %s compaction and extension preparation`（4 次执行），
断言 `session_before_compact` 的 `preparation.firstKeptEntryId` 走 `compaction.modelOverrides["faux/faux-1"]`
的 `{reserveTokens:2000, keepRecentTokens:150}` 而非普通设置 `{10, 20000}`；另两例
`uses the newly selected model without changing ordinary settings`、
`captures model identity before awaiting summarization auth`。

`agent-session-runtime.test.ts` 的 12 个用例名：

1. persists message_end assistant replacements to the session manager
2. settles the active response before session replacement
3. preserves an existing session when importing a file with the same name
4. emits session_before_switch and session_start for new and resume flows
5. honors session_before_switch cancellation for new and resume
6. emits session_before_fork and session_start and honors cancellation
7. reports why an unflushed session cannot be forked
8. duplicates the current active branch when forking at the current position
9. duplicates the current active branch in-memory when forking at the current position
10. throws when forking with an invalid entry id
11. updates the runtime session cwd on cross-cwd session replacement
12. restores model and thinking state from the destination session

### 7.3 测试基础设施

`test/suite/harness.ts`（9.3 KB）：构造真实 AgentSession 的测试夹具。**实际实现**不是走
`createAgentSessionServices` + `createAgentSessionFromServices`，而是直接 `new AgentSession(...)`：

- `registerFauxProvider({provider?, models?})` 创建 faux provider，`SessionManager.inMemory()`，
  `SettingsManager.inMemory(options.settings)`，`AuthStorage.inMemory()`；默认写入一条
  `{type:"api_key", key:"faux-key"}` 凭证（`withConfiguredAuth ?? true`）。
- `Agent` 配置：`getApiKey`、`streamFn: streamSimple`、`convertToLlm`（来自 `core/messages.ts`），
  以及三个转发到 `extensionRunnerRef.current` 的钩子——`onPayload` → `emitBeforeProviderRequest`、
  `onResponse` → `after_provider_response` 事件、`transformContext` → `emitContext`。
- `new AgentSession({agent, sessionManager, settingsManager, cwd: tempDir, modelRuntime: getModelRuntime(modelRegistry),
  resourceLoader, baseToolsOverride: toolMap, initialActiveToolNames, usesDefaultTools: options.initialActiveToolNames === undefined,
  allowedToolNames, excludedToolNames, extensionRunnerRef})`。
- `modelsJson` 存在时把 JSON 写到 `<tempDir>/models.json` 并 `createModelRegistry`，否则
  `createInMemoryModelRegistry`（均来自 `test/model-runtime-test-utils.ts`）。
- 返回的 `Harness` 完整成员：`session`、`sessionManager`、`settingsManager`、`authStorage`、`faux`、
  `models: [Model, ...Model[]]`、`getModel()` / `getModel(modelId)`、`setResponses`、`appendResponses`、
  `getPendingResponseCount`、`events: AgentSessionEvent[]`、`eventsOfType<T>(type)`、`tempDir`、`cleanup`。
- 辅助函数：`getMessageText`（string 原样；数组只取 text 块并以 **`"\n"`** 连接）、`getUserTexts`、
  `getAssistantTexts`、`getToolResult(harness, toolName)`（取最后一条匹配的 toolResult，缺失则抛
  `No ${toolName} tool result`）、`createTestUiContext(overrides?)`（全部方法为 no-op 的
  `ExtensionUIContext`，`theme` getter 返回真实主题，`setTheme` 返回
  `{success:false, error:"Theme switching not available in tests"}`）。
- `HarnessOptions`：`provider`、`models`、`settings`、`tools`、`initialActiveToolNames`、`allowedToolNames`、
  `excludedToolNames`、`resourceLoader`、`extensionFactories`、`withConfiguredAuth`、`modelsJson`、`sessionManager`。

`test/suite/README.md` 的规则（Rust harness 也应遵守）：只用 `test/suite/harness.ts` 与
`packages/ai/src/providers/faux.ts` 的 faux provider；**不得**使用真实 provider API、真实 API key、
网络请求或付费 token；测试必须 CI 安全且确定；除非缺少能力，不要扩展旧的 `test/test-harness.ts`。

`test/utilities.ts`（被 harness 复用）：`API_KEY = process.env.ANTHROPIC_OAUTH_TOKEN || process.env.ANTHROPIC_API_KEY`、
`createTestResourceLoader`、`createTestExtensionsResult`、`assistantMsg` / `userMsg` 等消息构造器，
以及从 `~/.kiana/agent/auth.json` 读取 OAuth 凭证的辅助逻辑。Rust 侧需提供等价 harness（`#[cfg(test)]` 模块）。

### 7.4 回归测试（`test/suite/regressions/`，**本单元规格原先完全遗漏**）

`test/suite/regressions/` 有 **84 个文件、200 个用例（按 `it.each` 展开后）**：179 条直接 `it`/`test` 声明（含 1 条 `it.skipIf`）+ 9 个 `it.each`/`test.each` 块展开为 21 例。全部基于同一 harness，
其中大量直接锁定本单元行为。Rust 移植必须至少覆盖下列与本单元直接相关的文件
（标题逐字；文件名为 `<issue>-<slug>.test.ts`）：

| 文件 | 例数 | 锁定的行为 |
|---|---|---|
| `11227-recovery-omits-tool-results.test.ts` | 1 | `pre-prompt recovery of a resumed length stop omits the truncated call and its result`——恢复省略必须同时省略 tool call 与其 result |
| `1717-2113-agent-session-event-settlement.test.ts` | 2 | `keeps persisted assistant/toolResult message order when extension message_end handlers yield`；`runs tool_call handlers after the assistant tool-use message is settled in the session` |
| `2023-queued-slash-command-followup.test.ts` | 1 | `treats extension-origin queued slash-command follow-ups as raw user text instead of dispatching the command` |
| `2860-replaced-session-context.test.ts` | 3 | `rebinds before withSession, targets the replacement session, and invalidates stale kiana/ctx`；`supports withSession for fork`；`supports withSession for switchSession`（`createReplacedSessionContext` 语义） |
| `3686-session-name-event.test.ts` | 3 | `emits session_info_changed when AgentSession.setSessionName is called` / `when an extension calls kiana.setSessionName` / `to extensions` |
| `3688-tree-cancel-compacting.test.ts` | 1 | `clears branch summary state when session_before_tree cancels navigation` |
| `3982-message-end-cost-override.test.ts` | 1 | `allows extensions to replace finalized assistant usage cost` |
| `5208-late-bash-output.test.ts` | 1 | `ignores output callbacks after bash operations resolve` |
| `5217-compaction-reason.test.ts` | 3 | `reports manual reason for compact()`；`reports threshold reason for auto-compaction`；`reports overflow reason and willRetry for overflow recovery` |
| `5303-bash-output-truncation.test.ts` | 2 | `captures output emitted after exit while a descendant holds stdout open`；`resolves after the grace when a descendant holds stdout open but stays quiet`（`waitForChildProcess` 的 100 ms 宽限） |
| `6324-branch-summary-ambient-auth.test.ts` | 1 | `summarizes tree branches when request auth has no API key` |
| `6363-agent-settled-event.test.ts` | 3 | `emits one agent_settled event after automatic retry finishes`；`settles only after follow-ups queued by agent_end handlers run`；`extension command waitForIdle waits for session-level settlement` |
| `6647-compaction-retries-transient-stream-drop.test.ts` | 5 | 压缩中的瞬态流中断重试 |
| `7048-compaction-truncated-summary.test.ts` | 1 | `does not persist a length-limited summary` |
| `7150-rpc-prompt-during-compaction.test.ts` | 1 | `rejects an RPC prompt while manual compaction is in progress` |
| `7193-event-bus-lifecycle.test.ts` | 1 | `removes extension-owned event-bus listeners on reload and dispose` |
| `7253-manual-compact-during-response.test.ts` | 1 | 响应进行中的手动压缩 |
| `8328-zero-usage-auto-compaction.test.ts` | 2 | `uses the message estimate when no assistant has reported usage`；`does not compact when the zero-usage message estimate is below the threshold` |
| `8537-custom-message-tool-result-ordering.test.ts` | 3 | `appends the message after the turn's tool results instead of between call and result`；`keeps session entries and message events in the same order as agent state`；`produces an llm history where every tool result follows its tool call` |
| `8724-in-memory-fork-active-tool.test.ts` | 1 | `does not append the aborted turn to the replacement session` |
| `8935-parallel-preflight-abort.test.ts` | 1 | 并行 preflight 中止 |
| `8989-fork-compaction-label-boundary.test.ts` | 1 | `preserves compaction context when a fork removes the boundary label`（`createBranchedSession` 的 label 重链） |
| `9068-user-bash-fail-closed.test.ts` | 5 | `RPC user_bash failure handling (#9068)`（`test.each(rpcCases)` 3 行：`fails the request without executing bash when a handler throws`、`... when a handler returns an empty result`、`executes bash normally when a handler returns undefined`）；`Interactive user_bash failure handling (#9068)`（`fails closed for %s when a handler returns an empty result` 2 行：`!pwd` / `!!pwd`） |
| `9178-tree-during-compaction.test.ts` | 2 | `rejects navigation before the active leaf can change`；`rejects a second navigation while the first is waiting` |
| `9340-9777-auto-compaction-cancellation.test.ts` | 6 | `does not start post-run auto-compaction after abort`；`cancels summarization authentication`；`cancels synchronously from compaction_start`；`reports extension cancellation as aborted` 等 |
| `9789-context-handler-system-messages.test.ts` | 6 | context handler 对 system 消息的重放/折叠 |
| `pre-prompt-compaction-no-continue.test.ts` | 1 | pre-prompt 压缩后不自动继续 |
| `startup-session-rebind-duplicate-subscription.test.ts` | 1 | 启动重绑不得重复订阅 |
| `tree-during-streaming.test.ts` | 1 | 流式中导航被拒 |
| `5080-signal-shutdown-extension-cleanup.test.ts` | 12 | 信号关闭时扩展清理顺序 |
| `6104-find-root-relativization.test.ts` | 11 | find 工具根相对化（非本单元，列在此以免误判为遗漏） |

其余文件（args/clipboard/theme/model-registry/package-manager 等）属于其他单元。
注意 `test/suite/regressions/` 下也有 `extension-factory-cache.test.ts`（4 例）与
`copilot-request-size-overflow.test.ts`（1 例）这类无 issue 编号的文件。

---

## 8. Rust 模块布局与最难复现的部分

### 8.1 建议模块布局（`kiana/crates/ca-session/`）

```
src/
  lib.rs                     // pub use 与 crate 文档；对应 core/index.ts
  messages.rs                // BashExecutionMessage/CustomMessage/... + convertToLlm + 前缀常量
  session/
    mod.rs                   // SessionManager、SessionEntry、投影函数
    entry.rs                 // 条目类型 + serde（保序）+ 迁移
    store.rs                 // 文件读写（loadEntriesFromFile/readSessionHeader/_persist/_rewriteFile）
    path.rs                  // getDefaultSessionDir / 编码
    projection.rs            // buildSessionPath / buildContextEntries / buildSessionProjection
    discovery.rs             // findMostRecentSession / list / listAll / buildSessionInfo（并发、进度）
  session_export.rs          // serializeSessionBranch / exportSessionToJsonl
  cwd.rs                     // session-cwd
  agent_session/
    mod.rs                   // AgentSession 主体（字段 + 构造 + getter/setter）
    hooks.rs                 // tool hooks / prepareRequest / next-turn / boundaries / transformContext 安装
    events.rs                // _handleAgentEvent / _emitExtensionEvent / _emit / 事件类型
    prompt.rs                // prompt/steer/followUp/sendCustomMessage/sendUserMessage/队列
    run.rs                   // _startRun / _prepareRun / _handlePostAgentRun / _runBeforeSettleBoundary
    compaction.rs            // compact / _checkCompaction / _runAutoCompaction / 手动状态机
    retry.rs                 // _prepareRetry / _finishCancelledRetry / abortRetry
    bash.rs                  // executeBash / recordBashResult / 待落地队列
    models.rs                // setModel / cycleModel / thinking
    tools.rs                 // 工具注册表 / loadout / refreshToolRegistry / restoreToolsFromTranscript
    extensions_bind.rs       // bindExtensions / bindCore 注入 / reload
    stats.rs                 // getSessionStats / getContextUsage / getLastAssistantText
    tree.rs                  // navigateTree / getUserMessagesForForking
    export.rs                // exportToHtml / exportToJsonl / summarizeForBugReport
    boundary.rs              // _buildBoundaryContext / _applyBoundaryDrafts / _commitBoundaryDrafts
  runtime.rs                 // AgentSessionRuntime + SessionImportFileNotFoundError
  services.rs                // createAgentSessionServices / applyExtensionFlagValues
  nested.rs                  // NestedCallRecorder / NestedToolCallRunner / NESTED_CALL_LIMITS
  bash_exec.rs               // executeBashWithOperations（流式管道）
  exec.rs                    // execCommand
  output_guard.rs            // stdout 接管
  event_bus.rs               // createEventBus
tests/
  session_manager_*.rs       // 7.1 的分组镜像
  agent_session_*.rs         // 7.2 的分组镜像
  parity_*.rs                // 与 TS 输出逐字对比的黄金测试
```

并发模型建议：

- `AgentSession` 内部可变状态用 `tokio::sync::Mutex<SessionState>`（或单线程 actor：所有事件经
  mpsc 送入一个任务顺序处理，最贴近 JS 单线程微任务语义）。
- **推荐 actor 模型**：`_handleAgentEvent`、队列操作、压缩/重试状态机都串行化到一个任务，
 外部 API 通过 channel 发命令并 await oneshot 响应。这能把 JS 的「同一 tick 内不会交错」近似出来。
- 取消：每次 run / 压缩 / 重试 / bash 各自持有 `CancellationToken`；`abort()` 全部 cancel 后
  等待 actor 空闲。
- 事件监听器：`Vec<Box<dyn Fn(&AgentSessionEvent) + Send + Sync>>`；扩展事件是 async，
  用 `async_trait` 或 boxed future。

### 8.2 最难复现的部分（按风险排序）

1. **边界（boundary）语义与 `finishTurn` 的三方裁决**
   `_installAgentBoundaryHooks` 的返回值优先级（`end` > 扩展 continue / 前一个 continue > undefined）
   与 `_boundaryDispatchedMessages` 去重（`turn_end` 在 `finishTurn` 与 `_emitExtensionEvent` 两处可能触发，
   只允许一次）非常微妙；`agent_before_settle` 的 `canContinue` 判定（只有 system 上下文时不可继续、
   但 pending custom 可救）与 `_abortDuringBeforeSettle` 的抑制也要精确。
   对应测试：`agent-session-boundaries.test.ts` 全 26 例。

2. **压缩判定链 `_checkCompaction`**
   涉及 `_modelForMessage`（虚拟模型的物理模型）、compaction 边界时间戳比较、
   `assistantIsProjected` / `assistantRetainedForExplicitRecovery` / `assistantUsageMatchesProjection`
   的布尔组合、`hasContextEdits` 时改用投影估算、error/零 usage 时回退到最后成功 usage 且校验其是否
   post-compaction。错一个分支就会出现「压缩后立刻再压缩」或「永不压缩」。

3. **消息身份（identity）与原地替换**
   `_entryIdsByMessage: WeakMap`、`_boundaryDispatchedMessages: WeakSet`、
   `_replaceMessageInPlace`（删键再 assign 保持引用）、`_findPersistedMessageEntryId` 的三级回退。
   Rust 必须引入稳定消息句柄，否则 tool result 与 assistant 的配对、`_omitRecoveryAttempt` 的 source-entry
   匹配都会失效（会触发 `Cannot persist recovery omission...`）。

4. **投影与压缩的交互**
   `buildContextEntries` 的「保留区」逻辑允许旧 compaction 条目因 ID 落在新 compaction 的保留范围内
   而被保留，`buildSessionProjection` 用「index > 0 的 compaction 不贡献消息」修正。
   `estimateProjectedContextTokens` 还要判断 usage 条目是否早于最近的 `context_edit`/`compaction`。
   三者必须一致，否则 `getContextUsage` 与阈值判定会漂移。

5. **系统提示词的分层投影**
   四层叠加：`transformContext`（hidden declarations）→ `transformContext`（forced prompt）→
   `prepareRequest`（canonical context + 虚拟路由 + 阈值压缩后重投影）→ `prepareNextTurnWithContext`
   （loadout 变更产生 system patch）。任何一层顺序错都会导致「同一 run 内 system prompt 变化」
   （测试 `keeps the same system prompt for every request of the run`）。

6. **队列与「什么时候 flush」**
   `_pendingCustomMessages` 在 `turn_end` 之后 flush（不能在 tool call 与 tool result 之间插入）；
   `_pendingBashMessages` 在 run 开始/结束与 `_handlePostAgentRun` 之外 flush；
   `_pendingNextTurnMessages` 在 `_prepareRun` 消费；`_deferredSettledActions` 在 `agent_settled`
   之后依次执行且期间 `_isEmittingAgentSettled` 会把新 prompt 再排队。这套时序是 UI 正确性的基础。

7. **虚拟模型路由**
   `_installAgentRequestProjection` 里「`_failedResponse` 只消费一次」、
   `reason` 的选择（`retry`/`user`/`continuation`）、state 条目只在变化时写入、
   阈值检查用路由后模型并重跑 `prepare()`。还有 `_recordSelection` 的「只在虚拟/记录不一致时写
   model_change」规则。

8. **重试与压缩的失败恢复**
   `_prepareRetry` 在 `maxRetries` 边界上的 `++/--` 技巧（保留 attempt 计数供 `_handlePostAgentRun`
   发出最终失败）、`_finishCancelledRetry` 的幂等、`_omitRecoveryAttempt` 在重试前后各调用一次、
   溢出恢复只一次且有专门的 errorMessage 文案（两类）。测试
   `finishes retry bookkeeping when a retry receives a nonretryable error` 与
   `marks the exhausted retry run as final` 直接锁死这些边界。

9. **UTF-16 语义**
   见 6.1 第 1、2 条。尤其 `parseSkillBlock`、`_throwIfExtensionCommand` / `_expandSkillCommand`
   的 `slice` 索引、bash 滚动缓冲与 ANSI 挂起窗口。

10. **JSON 字节级输出**
    会话文件必须与 TS 逐字节可比（至少字段顺序、数字格式、转义）。这要求 serde 自定义序列化器：
    - 键顺序固定（结构体字段顺序 + `skip_serializing_if`）；
    - 浮点按 JS `Number::toString` 最短往返；
    - 字符串转义按 `JSON.stringify`（`"`、`\`、`\b\f\n\r\t`、`\u00XX` 控制字符；**不**转义 `/`；
      非 BMP 用代理对输出，即 Rust 的 `\u{1F600}` 需写成 `😀`）；
    - `undefined` 值省略（`Option::None` + `skip_serializing_if`）。

11. **并发会话发现**
    `list` / `listAll` 的并发上限（10 / 64）、进度发布节奏（10 / 100、首个/最后一个）、
    取消语义（`throwIfAborted` 会抛出）、mtime 与 `localeCompare` 的排序组合。
    测试 `scopes current-folder APIs by cwd while listing all flat sessions` 与
    `rejects a cancelled session listing` 覆盖这些。

12. **stdout 接管（output-guard）**
    Rust 无 `process.stdout.write` 可替换的等价物；需设计全局 `StdoutSink` 单例
    （`OnceLock<AtomicPtr>` 或多态 writer），并保留「补写 stderr」「ENOBUFS 重试 10ms」
    「尾链 + `exit(1)`」的语义。TUI 单元会依赖此行为。

13. **bash 流式管道**
    `TextDecoder({stream:true})` + `splitIncompleteAnsiSuffix`（256 码元窗口）+ `stripAnsi`
    （OSC/CSI 完整文法）+ `sanitizeBinaryOutput` + `\r` 删除 + 滚动缓冲（按 UTF-16 长度）
    + `truncateTail(50KB)` + 「超阈值才建临时文件且追写滚动缓冲」。测试
    `strips a color reset split inside its parameters` 等 5 例逐一锁死。

14. **`exec` 的「永不 reject」与 kill 升级**
    `SIGTERM` → 5s → `SIGKILL`；`waitForChildProcess` 不等继承 stdio；
    `stderr || error.message` 的回退。Rust 用 `tokio::process::Child` + `tokio::select!` 实现。

15. **错误文本与事件顺序的组合**
    第 3 节的每一行都要进黄金测试；事件顺序由
    `emits extension events before public event subscribers`、`emits the expected event order for a single prompt`
    等用例固定。建议实现一个「事件记录器」测试夹具，把 TS 侧的期望序列逐字抄成 Rust 断言。

### 8.3 移植顺序建议

1. `messages.rs` + `session/entry.rs` + `session/projection.rs`（纯函数，易测）。
2. `session/store.rs` + `session/path.rs` + `session/discovery.rs`（文件与并发）。
3. `session_export.rs`、`cwd.rs`、`event_bus.rs`、`exec.rs`、`bash_exec.rs`、`nested.rs`、
   `output_guard.rs`（独立设施）。
4. `agent_session` 的只读访问器与工具注册表（可先不接扩展）。
5. 事件流 `events.rs` + `run.rs` + 队列，最后接扩展系统。
6. 压缩 / 重试 / 边界（最难，配合全部 suite 测试）。
7. `runtime.rs` + `services.rs`（依赖其他单元）。


---

## 9. Audit additions（对抗性完整性审计补充）

本节由独立审计追加：上文的正文已就地修正若干错误陈述（测试例数/用例名、bash 三类计量的阈值语义、
`appendLabelChange` 空串、`getCurrentTools` 排序、`Agent.continue()`/`subscribe`、`createBranchedSession`
的两种 id 冲突集合、`exec.options.cwd` 未被使用、`waitForChildProcess` 的 100 ms 宽限、
`KIANA_PACKAGE_DIR`、auth-guidance 逐字文本、`retry.provider` 默认值、`core/index.ts` 完整再导出清单、
`core/auth-guidance.ts` 与 `settings-manager.ts` 的辅助导出、文档派生语义）。以下列出**仍未被正文覆盖**
或容易被 Rust 实现忽略的条目。所有行号/名称均以当前源仓库为准。

### 9.1 导出符号补充

| 文件 | 正文未列（或只以「等」带过）的导出 |
|---|---|
| `src/core/agent-session.ts` | `AgentSession.modelRuntime`（getter，见 2.3.10 已补）；`parseSkillBlock`/`ParsedSkillBlock` 已列；`ExtensionBindings`、`QueuedInputDisposition`、`PromptDisposition`、`ModelMutationOptions` 仅作类型导出、不在 `core/index.ts` |
| `src/core/session-manager.ts` | `CURRENT_SESSION_VERSION`；`assertValidSessionId`、`migrateSessionEntries`、`parseSessionEntries`、`getLatestCompactionEntry`、`sessionEntryToContextMessages`、`buildContextEntries`、`buildSessionProjection`、`buildSessionContext`、`getDefaultSessionDir`、`loadEntriesFromFile`、`findMostRecentSession`、`SessionListProgress` 均已在 2.2.1 列出，**但** `SessionManager` 的静态方法 `listAll` 有两个重载（`(onProgress?, signal?)` 与 `(sessionDir?, onProgress?, signal?)`），Rust 需要两个入口名或一个带 `Option<String>` 的签名 |
| `src/core/messages.ts` | 四个常量、四个接口、`bashExecutionToText`、三个 `create*Message`、`convertToLlm` 均已列；`convertToLlm` 的 `default` 分支返回 `undefined` 并被 filter 掉，Rust 用 `Option<Message>` |
| `src/core/nested-tool-calls.ts` | `NestedCallSummary`、`NestedCallRecorder`、`NestedToolCallOptions`、`NestedToolExecutionEvent`、`NestedToolCallHost`、`NestedToolCallRunner`、`NESTED_CALL_LIMITS`（已列）——**未列**：`NestedCallRecorder.startedAt` 使用对象身份作键（Rust 不能以 id 字符串做键，因为同一 id 理论上可重复；应使用记录索引/`Arc` 指针） |
| `src/core/exec.ts` | `ExecOptions`/`ExecResult`/`execCommand` 已列；`ExecOptions.cwd` 未被使用（已补） |
| `src/core/output-guard.ts` | 6 个函数已列；**未列**：`stdoutTakeoverState.originalStdoutWrite` 用于 `restoreStdout()`，Rust 需要保存原 sink |
| `src/utils/paths.ts`（间接） | `normalizePath(input, options?)` 会：trim 前导/尾随空白（`trim` 选项）、展开 `~`（`expandTilde` 默认 true）、去 `@` 前缀（`stripAtPrefix`）、把 `file://` URL 转路径、把 Unicode 空格（`  -   　`）规范为普通空格；`resolvePath(input, baseDir?, options?)` = 先 `normalizePath` 再 `path.resolve`。会话文件路径（`--<cwd>--` 编码前）依赖此行为 |
| `src/core/auth-guidance.ts` | `getProviderLoginHelp`、`formatNoModelsAvailableMessage` 正文未列（已补） |
| `src/core/settings-manager.ts` | `getToolListError`、`mergeDefaultTools`（内部）、`CACHE_WARMING_MODES` 正文未列（已补） |

### 9.2 行为/边界条件补充

1. **`_persist` 的首次落盘不是「只写新条目」**：`flushed === false` 且出现第一条 user/assistant 消息时，
   以 `wx` 打开并写入**整个** `fileEntries`（header + 此前所有内存条目，含 setup 类的
   `thinking_level_change`/`model_change`）；只有 `flushed === true` 之后才逐条 append。
   `createBranchedSession` 的 persist 分支复用同一规则（`_hasConversation()` 为真才立即 `_rewriteFile()`）。
2. **`newSession()` 不会删除旧文件**：只重置内存状态与 `sessionFile`；旧 `.jsonl` 留在磁盘上
   （`--no-session` 之外的所有 `newSession` 都如此）。
3. **`getSessionName()` 反向遍历 `fileEntries`**（不是 `getEntries()`/`byId`，因为 footer 每帧都会调用），
   **遇到第一条 `session_info` 就返回** `entry.name?.trim() || undefined`（不会继续往前找）。
   `appendSessionInfo("")` 写入 `name:""`，因此 `getSessionName()` 返回 undefined——空名显式清除标题。
4. **`buildSessionInfo`**：`messageCount` 统计**所有** `message` 条目（含 system/toolResult）；
   `firstMessage` 只取第一条 **user** 的 `extractTextContent`（非空才记）；`allMessagesText` 是所有
   user+assistant 文本以**空格**连接。`name` 取**最后一条** `session_info` 的 `name?.trim() || undefined`。
   `modified`：最后 user/assistant 消息活动时间 > 0 → 用它；否则 header.timestamp 可解析 → 用它；
   否则 `stats.mtime`。`created` 恒为 `new Date(header.timestamp)`（可能是 Invalid Date）。
   `parentSessionPath = header.parentSession`；`cwd` 非字符串时取 `""`。
   `firstMessage` 为空时回退到字面量 `(no messages)`。
5. **`extractUserMessageText`（runtime）与 `extractTextContent`（session-manager）的 join 分隔符不同**：
   前者 `""`，后者 `" "`。这两个函数分别用于 fork 的 `selectedText` 与会话列表的 `allMessagesText`。
6. **`switchSession` 的 `cwdOverride`**：`SessionManager.open(sessionPath, undefined, options?.cwdOverride)`——
   第二个参数是 `sessionDir`（显式传 `undefined`，因此目录取文件所在目录）；随后
   `assertSessionCwdExists(sessionManager, this.cwd)` 用的是 runtime 当前 cwd 作为 fallback。
7. **`newSession` 的 `setup` 回调**：在 `apply(...)` 之后、`finishSessionReplacement` 之前执行
   `options.setup(this.session.sessionManager)` 并 `this.session.refreshContext()`；Rust 必须保证顺序，
   否则 setup 写的条目会落在旧投影上。
8. **`fork` 的 in-memory 分支复用同一个 `SessionManager` 对象**：`teardownCurrent` 之后直接在旧对象上
   `newSession()` / `createBranchedSession()`，再 `apply(...)` 用新 runtime 包住它；persist 分支则新建
   （`SessionManager.create`）或重开（`SessionManager.open(currentSessionFile, sessionDir)`）。
   in-memory 分支**不会**校验 `existsSync(currentSessionFile)`，因此 `Persisted session is missing...` /
   `This session has not been saved yet...` 两个错误只可能出现在 persist 分支。
9. **`importFromJsonl` 的 `sourceAlreadyStored`**：当目标路径与源路径 `resolve()` 相等时**不复制**，
   直接 `SessionManager.open`；否则用 `copyFileSync(..., COPYFILE_EXCL)`（原子、已存在即失败）。
   目标名冲突时 `name-1.ext`、`name-2.ext`……（从 1 开始）。
10. **`AgentSessionRuntime.dispose()` 的 teardown 顺序与 `teardownCurrent` 不同**：`dispose` **不**调用
    `session.abort()`，只发 `session_shutdown{reason:"quit"}` → `beforeSessionInvalidate?.()` → `session.dispose()`。
11. **`sessionManager` 的 `appendUsage` 返回条目对象**（不是 id），且 `note` 用 truthiness 条件展开
    （`note === ""` 不写键）。`appendCompaction` 的 `systemMessage` 只在存在时写键，且其 `timestamp`
    被覆盖为**条目 ISO 时间的毫秒数**（`new Date(entry.timestamp).getTime()`），不是原消息的 timestamp。
12. **`getTree()` 的排序**：节点先按 `getEntries()`（即 `fileEntries`）插入顺序建 `nodeMap` 与 children 数组，
    再用**显式栈**（非递归，避免深树爆栈）对每个节点的 children 做
    `new Date(a.entry.timestamp).getTime() - new Date(b.entry.timestamp).getTime()` **升序**排序；
    `parentId === null`、`parentId === entry.id`（自引用）以及父节点不存在（孤儿）都作为 root。
    时间戳无效（NaN）时比较函数返回 NaN，V8 视为相等（稳定排序保持插入序）。Rust 的 `sort_by` 需同样处理。
    注意 roots 数组本身的顺序**不**排序（保持插入序）。
13. **`sessionCwdMatches` 的空值语义**：`cwd !== undefined && cwd !== "" && resolvePath(cwd) === resolvedCwd`；
    header 缺 `cwd` 或为空串时**永不匹配**（旧会话在 `--resume` 的自定义目录过滤下会被隐藏）。
14. **`listAll` 的进度语义**：`firstCandidateLoaded` 只在 **index 0 完成**时置位；在此之前 `partial` 始终
    `undefined`，即使已加载多个（并发 10 时 index 1..9 可能先完成）。`loaded` 是完成计数，不是 index。
    `list()`/`listSessionsFromDir` 的 partial 条件是 `loaded === 1 || loaded % 10 === 0 || loaded === total`。
15. **`mapWithConcurrency`**：`new Array(items.length)` + 固定 `min(limit, n)` 个 worker，每个 worker 循环
    `signal?.throwIfAborted(); index = nextIndex++`。取消是**协作式**的：只有 worker 再次取任务时才抛，
    已在 `await map(...)` 中的任务不会被中断。Rust 用 `FuturesUnordered` 或 `tokio::task::JoinSet` +
    `CancellationToken` 时须复刻「已启动任务跑完、未启动任务不再启动」的语义。
16. **`localeCompare`**：`listSessionsFromDir` 按文件名 `b.localeCompare(a)` 降序；`listAll` 先按
    `mtimeMs` 降序、相同时按 `basename(b).localeCompare(basename(a))`。Node 默认 ICU 的 `en-US`
    collation 与 `str::cmp` 在**数字串**（`"10" < "9"` vs 码点）与**变音符**上不同；
    会话文件名以 `YYYY-MM-DDTHH-mm-ss-sssZ_<uuid>.jsonl` 开头，纯 ASCII，故风险主要在同名不同序的
    时间戳碰撞（毫秒相同）——建议 Rust 用 `icu_collator` 或在规格里固定 `str::cmp` 并加黄金测试。
17. **`sortSessionInfos` 是原地 `Array.sort`**：`modified.getTime()` 差值可能为 NaN（无效日期），
    此时比较函数返回 NaN → V8 视为 0（稳定排序保持原序）。Rust `sort_by` 需处理 NaN（`partial_cmp`
    返回 `None` 时按 `Equal`）。
18. **`SessionHeaderScanLimitError` 的扫描细节**：`readSessionHeader` 以 4096 字节块读取，最多 1 MiB；
    到达上限后再 `readSync` 1 字节探测 EOF——是 EOF 才把当前累积行当最后候选（可能成功），否则抛错。
    `SessionManager.open` 对 `SessionHeaderScanLimitError` 有**回退**：改用 `loadEntriesFromFile` 全量加载
    并取首条目为 header（类型不符则为 null）；其它异常上抛。
19. **`parseSessionEntries` vs `loadEntriesFromFile` 的差别**：前者 `content.trim().split("\n")`（**会**
    丢掉文件末尾空白与首尾空行），逐行 `JSON.parse` 且吞掉错误，**不**补写换行、**不**校验 header；
    后者按块读（`StringDecoder`）、保留「末行无换行」的解析，并在文件不以换行结尾时
    `appendFileSync(path, "\n")`，且首条目不是合法 `session` header 时返回 `[]`。Rust 若统一成一个函数，
    必须保证「补充 `\n`」与「header 校验」这两个副作用只在 `loadEntriesFromFile` 发生。
20. **`getContextUsage()` 的「压缩后未知」分支**：只要路径上有 compaction，且**没有任何** post-compaction
    的 assistant（`stopReason !== "aborted" && !== "error"` 且 `calculateContextTokens(usage) > 0`）落在
    投影 entries 中，就返回 `{tokens:null, contextWindow, percent:null}`——**不是**回退到估算。
    `percent = tokens / contextWindow * 100`（不做 `Math.round`）。
21. **`getSessionStats()` 的 `toolCalls` 只统计 assistant 消息 content 数组里的 `toolCall` 块**；
    `branch_summary`/`compaction` 的 `usage` 计入 tokens/cost，但**不计入**消息计数；
    `usage` 条目（cache warming 等）计入 tokens/cost 且**不**计入 `totalMessages`。
22. **`getLastAssistantText()` 的跳过条件**：对 `this.messages` 做一次 `slice().reverse()` 后
    `find(...)`，只跳过 `role === "assistant" && stopReason === "aborted" && content.length === 0` 的消息；
    找到第一条即停止（不会为「空文本」继续往前找）。随后拼接该消息所有 `text` 块，`text.trim() || undefined`
    （纯空白也算 undefined）。
23. **`exportToHtml` 的 theme 解析**：候选数组 `[options.themeName, settingsManager.getTheme()]` 用
    `.find(c => c !== undefined && getThemeByName(c) !== undefined)`；两个都无效时 `themeName` 为
    `undefined`（由 `exportSessionToHtml` 用默认主题）。`toolRenderer` 总是构造（即使没有扩展）。
24. **`exportToJsonl`** 忽略 `options`（没有 theme 参数）；`serializeSessionBranch` 的 `timestamp` 是
    **导出时刻**，不是会话 header 的时间；header 的 `cwd` 取 `sessionManager.getCwd()`（不是 `process.cwd()`）。
    默认文件名用 `process.cwd()` 作 baseDir。
25. **`summarizeForBugReport` 的 `signal` 是必填**（`options: {hint?: string; signal: AbortSignal}`），
    与 `compact` 的可选 signal 不同；`generateBugReportSummary` 的 `model` 来自
    `_getSummarizationRequestAuth`（虚拟模型会先路由）。
26. **`dispose()` 的 stale 消息全文**（逐字，见 2.3.9）包含两处 `withSession` 说明；Rust 必须一字不差。
    `_extensionRunner.invalidate(message)` 之后所有 `ExtensionContext` 方法都应抛同一文本，
    本条锁定 `2860-replaced-session-context` 的第三条测试。
27. **`recordBashResult` 记录的 `command` 是原始命令（不含 shell prefix）**，而实际执行的是
    `` `${prefix}\n${command}` ``；`bashExecutionToText` 展示的也是原始命令。
28. **`executeBash` 的 `onChunk` 与事件顺序**：先 `onChunk?.(delta)` 再 `_emit({type:"bash_execution_update", id, delta})`
    （`id` 为 `options?.id`，可能 undefined）；`recordBashResult` 在 `executeBashWithOperations` resolve 之后、
    `finally` 删除 controller 之前。
29. **`recordBashResult` 的 `excludeFromContext`**：`!!` 前缀由 CLI 层解析并传入，本单元只透传；
    `bashExecutionToText` 在 `excludeFromContext` 时被 `convertToLlm` **整条丢弃**（见 2.1.5）。
30. **扩展 flag 的 `applyExtensionFlagValues`**：boolean 型 flag 忽略传入值一律 `true`；string 型 flag
    收到非字符串（即 CLI 只给了 `--flag` 没给值）才报 `Extension flag "--${name}" requires a value`；
    未注册的 flag 汇总为**一条** `Unknown option(s): --a, --b`（单数时不加 `s`）。
31. **`createAgentSessionServices` 的刷新顺序**：先注册三类 pending（provider → native provider → virtual
    model），每类注册完**立即清空数组**，最后 `modelRuntime.refresh({allowNetwork:false})`，
    再 `applyExtensionFlagValues`。注册失败只进 diagnostics，不抛。
32. **`_addPersistedDefaultToNonEmptyScope`**：仅当 `_scopedModels` 非空时，把新模型**追加**到 scoped
    models，并在 `enabledModels` 非空时把 `provider/id` 追加进去（大小写不敏感去重）。这会**改变**后续
    `cycleModel` 的候选集，是 `setModel(persist:true)` 的隐藏副作用。
33. **`_emitModelSelect`**：`modelsAreEqual(previous, next)` 为真时**不**发 `model_select` 事件；
    `source` 为 `"set" | "cycle" | "restore"`。
34. **`reload()` 的 `_pendingToolNames`**：第 6 步把当前所有 active 工具名放进 `_pendingToolNames`，
    第 7 步再以 `[...getActiveToolNames(), ...addedDefaultTools]` 重建；两者顺序不可交换。
35. **`setActiveToolsByName` 的「减少即清空 pending」**：`previous` 中有名字不在新集合中时
    `_pendingToolNames.clear()`；只增不减则保留。这是扩展按 loadout 恢复工具的测试
    （`%s restored tools when an extension sets the loadout before they register`）的语义基础。
36. **`_isAllowedTool`/`_isActivatable` 的 MCP 细节**：`allowedToolNames` 为空数组时
    `_allowlistFiltersMcp = true`（`length === 0` 也算），因此空 allowlist 会**过滤掉** MCP 工具并且
    不保留 codemode/tool_search 通道；非空且**不含** `mcp__` 前缀项时 `_allowlistFiltersMcp = false`，
    MCP 工具仅对 codemode/tool_search 可达。
37. **`getAllTools()` 的 `sourceInfo`**：每个工具都带 `sourceInfo`（builtin 为
    `createSyntheticSourceInfo(...)`，`builtin:` 前缀）；`namespace`/`annotations`/`reachesTools` 只在存在时写键。
38. **`ToolInfo`/`ToolLoadout`/`ToolLoadoutChanges`** 定义在 `extensions/types.ts`，不在本单元；
    `AgentSession.getAllTools()` 的返回形状依赖它们（`exposure` 默认 `"direct"`）。
39. **`_refreshFinalizedContext()` 的实际调用点**（共 9 处，逐处对齐）：
    `_commitBoundaryDrafts`（1026）、`_emitExtensionEvent` 的 `entry_appended` 收尾（1271）、
    `refreshContext()`（1441）、`sendCustomMessage` 的 message_start 前（2402）、
    手动 compact 调 `appendCompaction` 后（2913）、自动 compact 同处（3239）、
    `recordBashResult` 的非流式分支（3958）、`_flushPendingBashMessages`（3992）、
    `navigateTree` 第 13 步（4197，之后还调 `_restoreToolsFromTranscript()`）。
    遗漏任一都会让 `session.messages` 与投影不一致（`8537-custom-message-tool-result-ordering` 锁定顺序）。
40. **`_emitExtensionEvent` 的 `message_end` 规范化**：扩展可返回替换消息（`emitMessageEnd` 返回非
    undefined）；若替换消息 role ∈ {user, assistant, toolResult, custom} 且 `content == null`，先归一化为
    `{...replacement, content: []}`，再 `_replaceMessageInPlace(event.message, normalized)`——**原地**改写
    旧对象（逐 key `delete` 后 `Object.assign`），从而 agent state、后续事件、监听器与随后
    `SessionManager.appendMessage(event.message)` 看到同一个对象。`message_end` 的**持久化**发生在
    `_handleAgentEvent` 里（role 为 `custom` → `appendCustomMessageEntry`；system/user/assistant/toolResult →
    `appendMessage`；`bashExecution`/`compactionSummary`/`branchSummary` 在别处持久化）。
    `3982-message-end-cost-override` 锁定扩展可改写 `usage.cost`；`1717-2113` 锁定
    「扩展 handler yield 时 assistant/toolResult 仍按顺序落盘」。

### 9.3 环境变量与外部格式补充

| 项 | 说明 |
|---|---|
| `KIANA_PACKAGE_DIR` | `getPackageDir()` 的首选来源；影响 `getDocsPath()` → auth-guidance 文本（已补） |
| `KIANA_OFFLINE` | 不属于本单元（`model-runtime`/`package-manager`），但测试会设置它 |
| `KIANA_SESSION_ID` / `KIANA_SESSION_FILE` / `KIANA_PROVIDER` / `KIANA_MODEL` / `KIANA_REASONING_LEVEL` | bash 工具执行时注入；`core/tools/bash.ts:194-208` 会先 `delete` 全部再按 `ctx.sessionManager`/`ctx.model`/`ctx.thinkingLevel` 赋值（`sessionFile` 可能不存在 → 不设） |
| `ANTHROPIC_OAUTH_TOKEN` / `ANTHROPIC_API_KEY` | 仅测试用（`test/utilities.ts` 的 `API_KEY`） |
| `~/.kiana/agent/auth.json` | `ModelRuntime.create({authPath})` 默认路径；测试 harness 也读它做 OAuth |
| `~/.kiana/agent/models.json` | `ModelRuntime.create({modelsPath})` 默认路径 |
| `<tmpdir>/kiana-bash-*.log` | 0600、`wx`（已补） |
| 会话文件 `parentSession` | **只有** `newSession({parentSession})` / `forkFrom` 写；`serializeSessionBranch` 的导出 header **不写**；`createBranchedSession` persist 分支写 `previousSessionFile`，in-memory 分支写 `undefined`（序列化省略） |

### 9.4 测试清单补充

- **`test/suite/regressions/`（84 文件 / 200 例，按 `it.each`/`test.each(rpcCases)` 展开）**：见 7.4 的表格；原规格完全没有提及。
- **`test/suite/harness.ts` 的真实实现**、`test/suite/README.md` 的规则、`test/utilities.ts` 的复用：见 7.3。
- **单元层 `it.each` 展开**：`file-operations`（3 行）、`boundaries`（3 行）、`compaction`（2 行）、
  `compaction-model-overrides`（4 行）、`mcp`（4 个 each × 2 行）——表格里的「例数」应理解为
  `it` 数，展开后执行次数更大（见 7.2、7.1 已标注）。
- **未列但相关的单元测试文件**（属其他单元，读规格时容易误认为本单元遗漏）：
  `test/compaction.test.ts`（28 例，`estimateContextTokens`/`shouldCompact`/`findCutPoint`/`prepareCompaction`）、
  `test/compaction-serialization.test.ts`（4 例，`serializeConversation` 的 2000 字符截断）、
  `test/compaction-summary-reasoning.test.ts`（15 例）、`test/branch-summarization.test.ts`（3 例）、
  `test/branch-summary-extensions.test.ts`（1 例）、`test/settings-manager-compaction.test.ts`（34 例，
  `compaction model overrides`）、`test/default-tools-setting.test.ts`（20 例，`defaultTools setting` 的
  `reload`/`resume` 子块）、`test/session-share.test.ts`（3 例）、`test/tool-result-images.test.ts`（7 例）、
  `test/cache-warmer.test.ts`（9 例）、`test/ansi-utils.test.ts`（5 例，`stripAnsi` 与 chalk 的兼容性）、
  `test/session-selector-*.test.ts`（8/3/14 例）、`test/exec.test.ts`（2 例，见上）、
  `test/bash-execution-width.test.ts`（2 例，TUI 层）。
  这些文件不在 ca-session 的移植范围，但本单元的 `getContextUsage`/`prepareCompaction`/`truncateTail`/
  `stripAnsi` 行为与它们共享实现，Rust 侧若把实现拆到不同 crate，需要保留同等测试。
- **`test/virtual-models.test.ts`（单元层，17 例）与 `test/suite/virtual-models.test.ts`（13 例）**：
  规格 7.2 只列了 suite 的 13 例；单元层的 17 例覆盖 `findLatestResponse`/`getBranchSelection`/
  `getVirtualModelState`/`createVirtualModel`/`withVirtualModels`，属 virtual-models 单元，但本单元的
  `routedModel`/`_modelForMessage`/`_recordSelection` 直接依赖它们。
- **`test/export-jsonl-share.test.ts`（1 例）** 与 `test/session-share.test.ts` 覆盖
  `exportSessionToJsonl` 的上层用法（`exportSessionForShare` 加 presentation data），可作为
  `serializeSessionBranch` 的黄金测试参考。

### 9.5 Rust crate 映射补充（行为缺口）

1. **`serde_json` 必须开 `preserve_order`**（正文已提），但仅此不够：`BashExecutionMessage` 等
   `undefined` 键省略要靠 `skip_serializing_if = "Option::is_none"`；而 `label: ""` **必须写键**，
   因此 `LabelEntry.label` 不能用 `Option<String>` + `skip_serializing_if` 的默认语义——需要
   `Option<Option<String>>` 或自定义序列化，区分「无键」「空串」「有值」三种状态。
   同理 `session_info.name` 在 `appendSessionInfo` 里永远是字符串（可为空串），`name?` 的 `undefined`
   只出现在**加载**旧文件时。
2. **`Date` 语义**：`SessionInfo.created/modified` 是 `Date` 对象；`buildSessionInfo` 的
   `new Date(header.timestamp)` 可能为 `Invalid Date`（`getTime()` NaN），`modified` 会回退到
   `stats.mtime`（也是 Date）。Rust 用 `Option<SystemTime>`/`Option<DateTime<Utc>>` 时，
   `sortSessionInfos` 的 NaN 比较（见 9.2.17）要显式处理。
3. **`structuredClone`**：`SettingsManager.getSettings()` 深拷贝（`structuredClone`），
   `_addPersistedDefaultToNonEmptyScope` 用展开浅拷贝；Rust 的 `Clone` 对 `Arc` 是浅拷贝，
   需要 `Arc::make_mut` 或 `serde_json::Value` 深拷贝来保持「修改 settings 不影响已发出的快照」。
4. **`performance.now()`**：`NestedCallRecorder` 的 `durationMs = Math.round(now - started)`；
   `Math.round` 对 `.5` 向 +∞，`f64::round` 对正数一致（正文已提）；但 Rust `Instant` 分辨率
   与 V8 不同，黄金测试不要断言具体毫秒值，只断言「字段存在且 >= 0」。
5. **`TextEncoder().encode(json).length`（嵌套调用的 `argumentsBytes`）是 UTF-8 字节数**，
   而 `text.length` 是 UTF-16 码元、`Buffer.byteLength` 是 UTF-8 字节——三者混用必须逐处对照
   （`nested-tool-calls` 用前者，`bash-executor` 滚动缓冲用 UTF-16，`truncateTail` 用 Buffer 字节）。
6. **`uuid v7` 必须单调**：`generateId` 的 100 次重试依赖 `randomUUID()` 的随机性；
   会话 id 用 `Uuid::now_v7()`。Rust 的 `uuid` crate 需 `v7` feature；`randomUUID().slice(0,8)`
   等价于 `Uuid::new_v4().simple().to_string()[..8]`（小写十六进制，含连字符的字符串切片前 8 个字符
   恰好是前 4 字节，无连字符）。
7. **`localeCompare`**：见 9.2.16；若用 `icu_collator`，注意 `Collator::compare` 返回 `Ordering`，
   且默认 `en-US` 对 `-` 与数字的处理与 `str::cmp` 不同。
8. **`AbortSignal`/`DOMException`**：`throwIfAborted()` 在 JS 抛 `AbortError`（`DOMException`）；
   `sleep(ms, signal)` 的 `reject(new Error("Aborted"))` 是普通 Error，文本逐字 `Aborted`。
   Rust 侧需要区分「用户取消」与「真实错误」：`_runAutoCompaction` 的 catch 用 `signal.aborted` 判断，
   不能只看错误类型。
9. **stdout 接管**：Rust 无 `process.stdout.write` 可替换，正文已建议 `StdoutSink` 单例；
   补充：`takeOverStdout()` 必须**幂等**（已接管直接返回），`writeRawStdout("")` 直接返回（不排队），
   `flushRawStdout()` 先 `waitForRawStdoutBackpressure()` 再写空串——空串写入只是为了等底层 flush。
   `writeRawStdout` 的 `catch` 调 `process.exit(1)`，Rust 用 `std::process::exit(1)` 且不能 unwinding。
10. **`EventEmitter` 语义**：`emit` 是**同步**调用所有监听器（`safeHandler` 是 async 函数，但 `emit`
    不 await 它），因此同一 channel 的监听器**启动顺序**是注册顺序，但执行到第一个 `await` 就交错；
    `event-bus.ts` 的 Rust 实现若用 `tokio::spawn` 会丢失「同步启动」的顺序保证。
    `Event handler error (${channel}):` 是 `console.error` 的第一个参数（第二参数是错误对象）。
11. **`node:readline` 的 `crlfDelay: Infinity`**：`buildSessionInfo` 用 `createInterface` 逐行读，
    会把 `\r\n` 合并为一个换行；`loadEntriesFromFile` 则**不**处理 `\r\n`（`\r` 会留在行尾导致
    JSON 解析失败？——实际 `JSON.parse` 接受尾随 `\r` 之前的空白，`\r` 是 JSON 空白，故仍可解析）。
    Rust 逐行读时对两者要分别处理。
12. **`fs.readdir` 的 `withFileTypes`**：`listAll` 只取「目录或符号链接」（`entry.isDirectory() ||
    entry.isSymbolicLink()`），符号链接指向文件也会被当作目录尝试 `readdir`（失败则空数组）；
    Rust 的 `read_dir` + `file_type().is_symlink()` 需复刻，且**不**做 `canonicalize`。
13. **`normalizePath` 的 `file://` 与 `~`**：会话 `--session` 参数可能传入 `file://` URL 或 `~/...`；
    Rust 的 `resolvePath` 必须先 `normalizePath`（含 `file://` 转路径、`~` 展开、Unicode 空格规范化）
    再 `resolve`，否则 `--<cwd>--` 编码与 `findById` 的 cwd 过滤会不一致。
14. **`JSON.stringify` 的键顺序**：`appendCustomMessageEntry` 的字面量顺序是
    `type,customType,content,display,details,id,parentId,timestamp`（**不是** base 的
    `type,id,parentId,timestamp` 前置），正文已提；但 `appendCompaction` 的顺序是
    `type,id,parentId,timestamp,summary,firstKeptEntryId,tokensBefore,details,usage,fromHook,systemMessage`，
    `details`/`usage`/`fromHook` 为 `undefined` 时仍**写键**（`JSON.stringify` 会省略 `undefined`，
    所以实际落盘没有这些键——Rust 用 `skip_serializing_if` 即可）。真正会写 `null` 的是
    `firstKeptEntryId: null`（retain-none 时被替换为自身 id，故实际不会为 null）。
15. **`SessionManager.forkFrom` 的 `cwd` 覆盖**：新 header 的 `cwd = targetCwd`（不是源会话的 cwd），
    `parentSession = resolvedSourcePath`；条目 id/parentId **原样保留**（不重写），因此 fork 出的文件
    可能含有与源会话相同 id 的条目——这是合规的，因为不同文件。
16. **`createBranchedSession` 的 `firstKeptEntryId` 重映射**：当 compaction 的 `firstKeptEntryId`
    指向一个被移除的 label 条目时，用 `replacementByLabelId`（label 之后第一个非 label 条目）替换；
    若 `firstKeptEntryId === entry.id`（retain-none 自引用）则保持自身。`8989-fork-compaction-label-boundary`
    锁定此行为。
17. **`_omitRecoveryAttempt` 的 source-entry 匹配**：依赖 `_entryIdsByMessage`（WeakMap）——
    Rust 用消息句柄映射时，**替换消息必须保留句柄**，否则 `Cannot persist recovery omission because
    a projected message has no source entry`。
18. **`getCurrentSystemMessage` 的 `timestamp: timestamp ?? 0`**：没有任何 system 消息但有工具时仍返回
    一条 `timestamp: 0` 的 system 消息；`appendCompaction` 会把它写进 `systemMessage` 并覆盖
    timestamp 为条目时间。Rust 需允许 `timestamp = 0`。
19. **`Version 3` header 的 `version` 字段**：`SessionHeader.version?: number`（v1 无）。新写文件必须
    是 `3`；加载 v1/v2 时 `migrateToCurrentVersion` 就地改 `version`，但只有 `_rewriteFile()` 才落盘
    （`_loadEntries` 在迁移返回 true 时调用 `_rewriteFile()`）。
20. **`exec` 的 `stderr || error.message`**：spawn 错误（ENOENT）时 `stderr` 为空，故返回
    `error.message`（如 `spawn nonexistent ENOENT`）；`code: 1`、`killed: false`。`exec.test.ts` 的第一例
    断言 `stderr` 匹配 `/ENOENT/`，Rust 的 `std::io::Error` 文本**不同**，需要自行拼出 Node 风格消息
    （或放宽测试为「非空且含 ENOENT」）。
21. **`waitForChildProcess` 的错误路径**：`child.once("error")` → reject，`execCommand` 的 `.catch`
    统一转成 `{code:1, killed, stderr: stderr || error.message}`；`exit` 的 `code` 为 `null`（被信号杀死）
    时 resolve 用 `code ?? 0`。Rust 需区分「exit code」与「signal」。

### 9.6 审计未覆盖/存疑项（供后续复核）

- `agent-session.ts` 的 `_installAgentBoundaryHooks`、`_installAgentRequestProjection`、
  `_installHiddenDeclarationsProjection`、`_installAgentForcedPromptProjection` 四个安装函数的**逐行**
  顺序（正文 2.3.6–2.3.8 已给状态机，但未列每个 hook 内部的 try/catch 边界与 `_boundaryDispatchedMessages`
  的 WeakSet 生命周期）。
- `ExtensionRunner` 的 `emitBoundary`/`emitMessageEnd`/`emitToolCall` 等在本单元的调用点已列，
  但 `ExtensionRunner` 自身的 handler 执行顺序（是否串行 await）属扩展单元，本规格只给契约。
- `packages/ai/src/utils/overflow.ts` 的 `OVERFLOW_PATTERNS` 全文（含 HTTP 413、各 provider 文本）
  未逐条抄录；`isContextOverflow` 的匹配顺序影响 `_checkCompaction` 的 overflow 分支，建议在 ai 单元规格中
  逐字固定。
- `packages/ai/src/models.ts` 的 `clampThinkingLevel` 向上/向下搜索顺序、`EXTENDED_THINKING_LEVELS`
  的具体值未在本规格展开（属 ai 单元）。
- `docs/sessions.md` 的 CLI 细节（`--continue`/`--resume`/`--no-session`/`/session`/`/name`）属于
  CLI 单元；本规格只记录与 SessionManager 直接相关的部分。
