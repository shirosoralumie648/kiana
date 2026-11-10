# 架构导览

> 这是**面向新人的架构讲解**，不是实现规范。逐文件规范见仓库根 `ARCHITECTURE.md`
> 与 `spec/*.md`；本文只讲“系统为什么长这样、各部分如何协作”。

---

## 1. 系统形状

Kiana 是一个终端编程代理：用户给出任务，代理循环调用大模型，模型返回文本或工具调用，
代理执行工具并把结果回灌给模型，直到任务结束。系统还必须负责：

- 多厂商模型协议（Anthropic、OpenAI、Google、Bedrock、兼容端点……）；
- 工具（读写文件、搜索、执行命令、MCP、代码沙箱）；
- 会话持久化、上下文压缩、导出；
- 交互式 TUI、非交互 print、JSONL、RPC 四种前端；
- 用户扩展（TS/JS 插件）与内建扩展；
- 远端执行环境与可恢复的长任务（durable）；
- 配置、认证、模型目录、信任决策、遥测。

这些职责被拆成 32 个 crate，按依赖方向分层。核心原则是：
**下层只提供机制，上层决定策略；跨 crate 的契约集中在少数“权威 crate”里。**

---

## 2. 分层与依赖方向

```text
L14  kiana-evals
L13  kiana-coding-agent                  CLI / 启动编排 / SDK 导出
L12  kiana-interactive  kiana-rpc-print  四种前端中的两种
L11  kiana-session                       组装可运行的 AgentSession
L10  kiana-builtin-extensions  kiana-resources
L9   kiana-extensions                    扩展加载器 + JS 宿主
L8   kiana-export  kiana-tools
L7   kiana-ext-api  kiana-ui-kit
L6   kiana-theme  kiana-hooks  kiana-memory
L5   kiana-compaction  kiana-config
L4   kiana-session-core  kiana-permissions
L3   kiana-core  kiana-env-client
L2   kiana-agent-core  kiana-durable
L1   kiana-ai  kiana-chord  kiana-codemode  kiana-mcp  kiana-tui
L0   kiana-json  kiana-env  kiana-sandbox  kiana-telemetry
```

依赖只能向下。每一条边都有实际用途，不允许“顺手加依赖”。完整的边表见仓库根
`ARCHITECTURE.md` §2.2。

> 校验记录（2026-11-07）：扫描 32 个 crate 的 `Cargo.toml` 依赖键，按上表分层
> **没有**一条边指向更高的层。该统计包含 dev/build 依赖，因此比运行期依赖图更严格。

几个容易困惑的点：

- **`kiana-core` 不在最底层**。它包含路径、配置、子进程、Git、HTTP dispatcher 等
  基础设施，但其中一些实现依赖 `kiana-tui` 的公开 trait 和 `kiana-agent-core` 的类型，
  因此位于 L3。
- **`kiana-ext-api` 位于中层**，因为它定义的扩展 API 类型需要引用消息、配置、主题等
  下层的类型；`kiana-extensions` 才是运行时。
- **`kiana-durable` 不依赖 `kiana-agent-core`**。durable 有自己的任务/工具抽象；
  若两者形状需要共享，应上移到 durable 自己的类型，而不是反向依赖。
- **`kiana-env` 是独立二进制**，零 workspace 依赖，可以被单独部署到远端机器。
- **`kiana-hooks` 与 `kiana-memory` 是新增 crate**：前者把 hooks 配置变成可执行
  handler（只依赖 `kiana-json`，位于 `kiana-config` 与 `kiana-ext-api` 之间），后者负责
  跨会话记忆的存储、召回与整合（当前零 workspace 依赖，被 `kiana-session` 使用）。
  两者都没有 TS 侧的独立包，模块规范见 `docs/history/porting/plan.md` §五。

---

## 3. 运行时模型

### 3.1 单线程 + LocalSet

主程序（`crates/kiana-coding-agent/src/main.rs`）做三件事：

1. 安装主题 JSON 校验器（TS 在模块加载期完成，Rust 没有等价的钩子）；
2. 创建 **current-thread** tokio 运行时；
3. 在 `LocalSet` 上创建 `ExtensionHost`，再调用 `kiana_coding_agent::main`。

为什么必须单线程：扩展运行时基于 rquickjs，JS 值与宿主对象是 `!Send`；部分会话状态
也按单线程设计（与 TS 的事件循环一致）。因此：

- 用 `spawn_local` 而不是 `tokio::spawn` 启动 `!Send` 任务；
- `kiana-durable`、`kiana-extensions`、`kiana-tui` 的公开 API 不要求 `Send` future；
- 需要真正并行的重活（HTTP、文件 IO、图片处理）交给 tokio 的多线程运行时或专用线程池，
  通过通道把结果送回单线程。

### 3.2 事件流

模型输出不是一次性返回，而是通过 `AssistantMessageEventStream`（`kiana-ai`）逐块推送。
代理层（`kiana-agent-core`）消费它并产生 `AgentEvent`；前端订阅 `AgentEvent` 渲染。
关键语义：

- `push` 同步入队，`end` 提交最终结果并唤醒所有等待者；
- `result()` 可重复读取；未 `end` 时永远 pending；
- 终态事件（如 `agent_end`）决定何时提交结果；
- 取消通过 `CancellationToken` 向下传递，流不会自动合成“取消”事件，调用方要区分
  “流结束”和“任务失败/取消”。

### 3.3 错误模型

- 公开错误类型（如 `AiError`、`McpError`、`ChordError`）的 `Display` 文案与 TS 的
  `error.message` **逐字一致**；实现方式不统一（`AiError`/`ChordError` 手写 `Display`，
  `McpError` 用 `thiserror::Error` 的 `#[error("{message}")]`），但输出契约相同；
- 叶子 crate 内部可用 `thiserror`，但不允许 `anyhow` 进入库代码；
- 任何跨进程/进入 JSON 的错误都必须保持字段形状与文本；
- 取消不是普通错误：多数层用专门的 `Abort` 分支或 `CancellationToken`，
  上层按错误名字/类型判断。

---

## 4. 核心数据模型

### 4.1 消息

- `AgentMessage`（`kiana-agent-core`）：代理内部消息，可包含 UI 专用/自定义消息；
- `Message`（`kiana-ai`）：只发给模型的三类：`user`、`assistant`、`toolResult`；
- 两者之间由 `convert_to_llm` 转换；转换前还可有 `transform_context` 做裁剪/注入。

消息内容由 **content block** 组成：文本、图片、思考块、工具调用、工具结果等。
工具调用与结果通过 `toolCallId` 关联；并行工具调用时，事件结束顺序与消息顺序可能不同，
这是刻意保持的 TS 语义。

### 4.2 工具

工具（`kiana-tools`）的协议大致是：

1. 模型返回 `toolCall`（名称 + JSON 参数）；
2. 代理做 prepare/validate（JSON Schema 校验、参数归一化）；
3. 执行工具，期间可多次推送进度更新；
4. 结果转为 `toolResult` 消息，进入下一轮。

内建扩展（`kiana-builtin-extensions`）用扩展 API 实现同一协议，因此 MCP、codemode、
tool-search 与普通工具在代理层看起来一致。

### 4.3 会话与持久化

`AgentSession`（`kiana-session`）是业务门面；`SessionManager`（`kiana-session-core`）
负责把消息与元数据写成 JSONL。写入必须遵守 JS 的键序与数字格式（见 §6.1）。
读取时按记录类型恢复；损坏/缺失的记录按 TS 的容错行为处理。

上下文过长时，`kiana-compaction` 生成摘要并替换旧消息；分支会话用分支摘要保留语义。
导出（`kiana-export`）把会话渲染为 HTML，包含 ANSI 转 HTML 与工具结果渲染。

---

## 5. 五个可执行目标与四种模式

| 二进制 | 说明 |
|---|---|
| `kiana` | 主程序；`--mode` 分发到 interactive / print / json / rpc |
| `kiana-rpc` | 先设置进程标题与标记，再走 RPC 模式 |
| `kiana-env` | 远端执行守护进程，帧协议，零 workspace 依赖 |
| `kiana-ai` | OAuth 认证配置 CLI |
| `kiana-evals` | 评测运行器（需要 Docker） |

四种模式共享同一套会话/代理，只在“输入从哪来、事件到哪去”上不同：

- **interactive**：`kiana-interactive`，全屏 TUI，订阅事件并渲染；
- **print**：`kiana-rpc-print`，单次执行，最终文本写 stdout；
- **json**：逐行 JSON 事件写 stdout；
- **rpc**：行协议 JSON-RPC 风格，供外部程序驱动。

---

## 6. 跨切面约定（最容易踩坑的地方）

### 6.1 JavaScript 语义

目标是与 TS 可观察行为一致，因此：

- **数字**：一律用 `kiana_json::format_js_number`（`1e+21`、`1e-7`、`-0`→`"0"`、
  `NaN`/`Infinity`→`null`）；
- **对象键序**：`serde_json` 开启 `preserve_order`（插入序），再用
  `kiana_json::sort_js_keys` 复刻 JS 的整数键优先规则；
- **字符串**：长度/切片/截断按 UTF-16 code unit，不能按 Rust 的 `char`；
- **相等**：深比较用 `kiana_json::deep_equal`，不要用 `Value` 的 `==`。

### 6.2 层与锁

- 跨 `await` 持锁必须显式允许并注释原因；优先 `tokio::sync::Mutex`，
  短纯内存临界区用 `std::sync::Mutex`；
- 单线程 crate 内的 `Rc/RefCell` 不是线程安全问题，但要注意重入与借用冲突；
- 事件流的等待者用 `oneshot`/`watch`，锁只保护队列与状态，不跨 `await` 持锁。

### 6.3 平台与 feature

- `kiana-tui` 的 native 后端按平台 feature 选择（`x11`/`darwin`/`win32`，另有 `native`
  聚合开关），默认 `[]`，无后端时返回 `Unavailable` 而不是 panic；
- `kiana-codemode` 默认启用 `wasm`；`kiana-extensions` 默认启用 `runtime` + `locale-data`；
- `kiana-durable` 的 `sqlite` 是可选的；IndexedDB/Cloudflare 是**明确不移植**的占位。

### 6.4 明确不移植的边界

- IndexedDB 后端：构造即返回 `StorageBackendUnsupported::IndexedDb`；
- Cloudflare Workers/Durable Object 适配：feature 保留、调用即明确报错；
- 浏览器/Workers 专用能力不实现。

这些不是“待办”，而是有意的产品边界；修改前先看仓库根 `ARCHITECTURE.md` §8。

---

## 7. 关键设计决策（以及为什么）

1. **一个 TS 包一个 crate，唯一例外是 `coding-agent` 拆成 16 个**：按依赖 SCC 拆分，
   避免 25 个实现者改同一批文件。
2. **新增 `kiana-json`**：把 JS 语义集中到一处，避免每个 crate 各写一份数字/键序逻辑。
3. **扩展运行时用 rquickjs + 手写 Promise 协议**：单线程、显式 Drop 关闭，避免
   `!Send` 与线程亲和问题（设计定稿见 `spec/design-extension-runtime.md`）。
4. **durable 不依赖 agent**：两者是不同层次的可恢复执行模型，共享形状应上移。
5. **不用 AWS SDK**：手写 SigV4 与 event-stream 解码，才能与 TS 的错误文本/重试语义对齐。
6. **Photon 用原生 `photon-rs`**：与 TS 的 wasm 构建同源，JPEG 逐字节一致、PNG 允许末位差异。
7. **模型目录数据 vendored**：构建不依赖 Node；`KIANA_MODEL_DATA_DIR` 仍可覆盖。
8. **不内置上游域名**：所有自建服务由 `KIANA_API_BASE_URL` 配置，未配置时相关功能
   明确停用，而不是回退到外部站点。

---

## 8. 修改指南：改哪里

| 想做的事 | 先看 | 注意 |
|---|---|---|
| 改模型协议/新增 provider | `kiana-ai/src/api/`、`providers/catalog.rs` | 事件顺序、错误文本、JSON 键序 |
| 改代理循环/工具执行 | `kiana-agent-core/src/agent_loop.rs` | 事件顺序、并行批次语义、取消 |
| 改会话落盘 | `kiana-session-core/src/session_manager.rs` | JSONL 键序、原子写入、兼容旧文件 |
| 改系统提示/技能 | `kiana-resources` | 资源发现顺序、模板变量 |
| 改 TUI | `kiana-tui`、`kiana-interactive` | 单线程、终端能力、ANSI/OSC |
| 改扩展 API | `kiana-ext-api`（唯一权威） | 先改契约再改运行时与内建扩展 |
| 改配置/认证 | `kiana-config` | 存储格式、迁移、信任边界 |
| 改 durable | `kiana-durable/src/harness/` | 可恢复性、事务边界、调度顺序 |
| 改远端执行 | `kiana-env` + `kiana-env-client` | 帧协议版本、错误码、SSH 安全 |

---

## 9. 与 TypeScript 参照实现的关系

- `spec/*.md` 是逐单元移植规范，包含 TS 源路径、行为差异与验收清单；
- 仓库根 `ARCHITECTURE.md` §8 记录所有已决差异（例如 IndexedDB 不移植、RPC 默认
  `cliPath` 改为 `current_exe()`）；
- 源码里的 `TS 源：...` 注释是保真溯源，不是过时信息；修改行为前先查对应 spec 与测试。

一个实用判断：**如果 Rust 代码看起来“多此一举”地复刻了某个 JS 细节（键序、UTF-16、
错误文本、事件顺序），先假设它是保真要求，再找测试确认。**
