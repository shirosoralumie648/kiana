# 从这里开始

> 读者假设：你会一点 Rust（能读懂 `struct`、`enum`、`Result`、`async/await`），
> 但**从未接触过 Kiana**，也不知道 TypeScript 参照实现长什么样。
> 本文用十分钟建立整体心智模型，然后把你送到合适的深入文档。

---

## 1. Kiana 是什么

Kiana 是一个**编程代理**（coding agent）：你在终端里用自然语言描述任务，它调用大模型，
由模型决定读文件、改文件、跑命令，并把过程实时显示在终端里。

这个仓库是它的 **Rust 实现**，共 32 个 crate，5 个可执行目标：

| 二进制 | 用途 | 入口 |
|---|---|---|
| `kiana` | 主程序：交互式 TUI、print、json、rpc 四种模式 | `crates/kiana-coding-agent/src/main.rs` |
| `kiana-rpc` | 等价于 `kiana --mode rpc`，供外部程序驱动 | `crates/kiana-coding-agent/src/bin/kiana-rpc.rs` |
| `kiana-env` | 远端执行守护进程（通常经 SSH 启动） | `crates/kiana-env/src/main.rs` |
| `kiana-ai` | 交互式 OAuth 配置工具 | `crates/kiana-ai/src/bin/kiana-ai.rs` |
| `kiana-evals` | Docker 化评测运行器 | `crates/kiana-evals/src/main.rs` |

Kiana 的产品目标是**与 TypeScript 参照实现行为一致**：相同的错误文案、事件顺序、
JSON 键序、落盘格式、默认值和超时。因此你在源码里会大量看到
`TS 源：packages/...` 这样的溯源注释——它们是保真约束，不是冗余信息。

---

## 2. 先记住五个词

1. **`crate`（Rust 编译单元）**：本仓库的一个库或二进制。32 个 crate 通过 `Cargo.toml`
   的 path 依赖组成一张有向无环图（DAG）。
2. **`AgentSession`（会话）**：一次对话的完整状态：消息历史、当前模型、工具集合、
   配置与订阅关系。它把“用户输入”变成“模型请求”，再把“模型输出”变成“事件 + 工具调用”。
3. **`Agent` / agent loop（代理循环）**：真正驱动“请求模型 → 流式接收 → 执行工具 →
   继续请求”的循环，位于 `kiana-agent-core`。
4. **`Provider`（模型提供方）**：把统一的消息类型翻译成某个厂商的 HTTP 协议
   （Anthropic、OpenAI、Google、Bedrock……），位于 `kiana-ai`。
5. **`Extension`（扩展）**：用 TypeScript/JavaScript 写的插件，由内嵌的 QuickJS
   运行时加载，可以注册工具、命令、UI 组件。扩展 API 的唯一权威定义在 `kiana-ext-api`。

其余术语见 [GLOSSARY.md](GLOSSARY.md)。

---

## 3. 十分钟心智模型

一次任务的路径可以压缩成下面这条线：

```text
终端输入
  │
  ▼
kiana-coding-agent          CLI 解析、配置/认证/信任、选择会话、选择运行模式
  │
  ▼
kiana-session               AgentSession：消息、模型、工具、事件订阅
  │
  ▼
kiana-agent-core            agent loop：调用模型、流式消费、执行工具、循环
  │            │
  │            ├── kiana-ai            provider HTTP/SSE → 统一的 AssistantMessageEvent
  │            └── kiana-tools         内置工具（read/edit/bash/grep…）
  │                 └── kiana-builtin-extensions   以扩展形式提供的工具（mcp/codemode/…）
  ▼
kiana-interactive / kiana-rpc-print    把事件渲染成 TUI，或写成 JSONL/RPC 帧
  │
  ▼
kiana-session-core         会话 JSONL、消息转换、用量统计
kiana-compaction           上下文压缩与分支摘要
kiana-export               导出 HTML
```

几条重要事实：

- **单线程事件循环**：主程序在 `tokio::task::LocalSet` 上运行。扩展运行时（rquickjs）
  和一部分会话状态是 `!Send`，不能跨线程移动。看到 `spawn_local` 是正常的。
- **事件不是返回值**：模型输出通过 `AssistantMessageEventStream` 逐块推送；
  上层订阅事件并同时等待最终结果。
- **取消是显式的**：TypeScript 的 `AbortSignal` 在 Rust 里是 `CancellationToken`，
  沿调用链向下传递。
- **JSON 不是 serde 默认输出**：凡是写到磁盘、网络或模型上下文里的 JSON，都要走
  `kiana-json`，以复刻 JavaScript 的数字格式和对象键序。

---

## 4. 仓库地图

```text
kiana/
├── crates/                 32 个 crate（见下表）
├── docs/                   面向新人的文档（本目录）
├── spec/                   每个移植单元的详细规范 + 进度 + 差距清单
├── ARCHITECTURE.md         架构定稿：crate 划分、依赖、共享约定、决策记录
├── Cargo.toml              workspace 定义与全部依赖的精确 pin
├── Cargo.lock              锁定后的依赖图
└── README.md               构建、运行、端点配置
```

32 个 crate 按依赖层次（下层被上层使用）：

| 层 | crate | 一句话 |
|---|---|---|
| L0 | `kiana-json` | JavaScript 语义的 JSON（数字格式、键序、解析） |
| L0 | `kiana-sandbox` | 进程级文件系统/网络沙箱（bubblewrap + Landlock） |
| L0 | `kiana-env` | 远端执行守护进程（独立二进制，帧协议） |
| L0 | `kiana-telemetry` | 遥测 span/指标存储与渲染 |
| L1 | `kiana-ai` | 模型类型、provider 适配、认证、流式事件 |
| L1 | `kiana-chord` | 分布式会话帧协议与 SQLite/对象存储 |
| L1 | `kiana-codemode` | 在沙箱里执行模型生成的代码 |
| L1 | `kiana-mcp` | Model Context Protocol 客户端与传输 |
| L1 | `kiana-tui` | 终端渲染、组件、输入、剪贴板 |
| L2 | `kiana-agent-core` | 代理循环、事件流、工具执行协议 |
| L2 | `kiana-durable` | 持久化执行（harness）、存储后端、执行环境 |
| L3 | `kiana-core` | 基础设施：路径、配置、子进程、Git、HTTP dispatcher… |
| L3 | `kiana-env-client` | 远端环境客户端（SSH、连接、远程文件系统） |
| L4 | `kiana-session-core` | 消息模型、会话文件、用量统计 |
| L4 | `kiana-permissions` | 工具调用权限：模式、规则、命令/路径分类与审批决策 |
| L5 | `kiana-compaction` | 上下文压缩与摘要 |
| L5 | `kiana-config` | 设置、认证存储、模型目录、信任、键位 |
| L6 | `kiana-theme` | 主题、语法高亮映射、系统主题检测 |
| L6 | `kiana-hooks` | hooks 配置到可执行 handler 的派发层 |
| L6 | `kiana-memory` | 跨会话记忆的存储、召回、抽取与整合 |
| L7 | `kiana-ext-api` | 扩展 API 类型契约（唯一权威） |
| L7 | `kiana-ui-kit` | 可复用的 TUI 小组件 |
| L8 | `kiana-export` | 会话导出 HTML |
| L8 | `kiana-tools` | 内置工具实现 |
| L9 | `kiana-extensions` | 扩展加载器、JS 宿主、UI 桥 |
| L10 | `kiana-builtin-extensions` | 随程序发布的内建扩展 |
| L10 | `kiana-resources` | skills、prompt 模板、系统提示、资源加载 |
| L11 | `kiana-session` | 把会话各层组装成可运行的 AgentSession |
| L12 | `kiana-interactive` | 交互式 TUI 模式 |
| L12 | `kiana-rpc-print` | print/json/rpc 模式 |
| L13 | `kiana-coding-agent` | CLI、启动编排、迁移、SDK 导出 |
| L14 | `kiana-evals` | 评测运行器 |

精确的依赖边与拆分理由见 [ARCHITECTURE_OVERVIEW.md](ARCHITECTURE_OVERVIEW.md) 与仓库根的 `ARCHITECTURE.md`。

---

## 5. 选一条阅读路线

### 路线 A：我想知道“一次请求到底怎么跑”

1. `crates/kiana-coding-agent/src/main.rs`（进程入口，约 40 行）
2. `crates/kiana-coding-agent/src/app.rs` 的 `main`（启动编排）
3. `crates/kiana-session/src/agent_session.rs`（会话门面）
4. `crates/kiana-agent-core/src/agent_loop.rs`（循环与事件流）
5. `crates/kiana-ai/src/api/`（选一个 provider 看请求/流解析）
6. `crates/kiana-tools/src/`（工具如何被执行）
7. [EXECUTION_FLOW.md](EXECUTION_FLOW.md)（把以上串成完整时序）

### 路线 B：我想加一个模型提供方

1. `crates/kiana-ai/src/types.rs`（统一消息/事件类型）
2. `crates/kiana-ai/src/providers/catalog.rs`（provider 目录）
3. `crates/kiana-ai/src/api/anthropic_messages.rs`（最完整的适配器示例）
4. 仓库根 `ARCHITECTURE.md` §4.3（JSON/键序/数字约定）
5. `crates/kiana-ai/tests/`（黄金对照测试）

### 路线 C：我想改终端界面

1. `crates/kiana-tui/src/lib.rs`（模块树）
2. `crates/kiana-tui/src/tui.rs`（`Tui` 与 `Component` 契约）
3. `crates/kiana-interactive/src/interactive_mode.rs`（交互模式状态机）
4. `crates/kiana-interactive/src/components/`（具体界面组件）
5. `crates/kiana-ui-kit/src/`（可复用组件）

### 路线 D：我想写扩展

1. `crates/kiana-ext-api/src/types.rs`（扩展能看到的全部 API）
2. `crates/kiana-extensions/src/lib.rs`（宿主生命周期）
3. `crates/kiana-extensions/src/loader/`（发现、编译、加载）
4. `crates/kiana-builtin-extensions/src/`（内建扩展示例）
5. `spec/design-extension-runtime.md`（运行时设计定稿）

### 路线 E：我想理解持久化执行（durable）

1. `crates/kiana-durable/src/lib.rs`
2. `crates/kiana-durable/src/harness/`（调度器、任务图、工具）
3. `crates/kiana-durable/src/storage/`（JSONL / SQLite 后端）
4. `crates/kiana-durable/src/session/`（会话日志）
5. `spec/durable-harness.md`

---

## 6. 第一次运行

```bash
# 编译主程序（首次会编译全部依赖，耗时较长）
cargo build --release -p kiana-coding-agent

# 交互式运行
./target/release/kiana

# 非交互：单次提示
./target/release/kiana -p "解释这个仓库的目录结构"

# 只编译、测试某一个 crate，避免全仓库构建
cargo test -p kiana-json
cargo check -p kiana-ai --all-targets
```

配置默认在 `~/.kiana/agent`，会话在 `~/.kiana/agent/sessions`；可用
`KIANA_CODING_AGENT_DIR`、`KIANA_CODING_AGENT_SESSION_DIR` 覆盖。
Kiana 不内置任何上游服务域名；远端模型目录、版本检查、遥测等都需要显式配置
`KIANA_API_BASE_URL`（见根 `README.md`）。

> 测试提示：整个 workspace 的测试二进制非常多。只想验证一个改动时，始终先用
> `cargo test -p <crate>`，不要直接 `cargo test --workspace`。

---

## 7. 注释可信度（2026-11-06 全量完成）

本仓库的注释经过一轮完整的工程化改造与验证，你可以把它们当作**可靠的入门材料**：

- **覆盖**：32 个 crate（统计时点 798 个源文件）、392 个注释单元全部完成（模块文档 `//!`、
  公开项 `///`、复杂逻辑 `//` 三层），详见 [COMMENT_PROGRESS.md](COMMENT_PROGRESS.md)。
- **对抗性审阅**：每个单元由独立代理逐条核对注释断言与源码/测试的一致性，
  共发现 1881 条问题并修复 1979 条（含措辞重写与链接修正）。
- **纯度保证**：全部改动仅涉及注释行与空行。确定性检查（基线 diff + 代码行序列比对）
  确认没有任何代码字符、签名、属性或测试被改动。
- **编译验证**：`cargo fmt --all --check`、`RUSTDOCFLAGS='-D warnings' cargo doc --workspace`、
  `cargo check --workspace --all-targets` 全部通过；文档示例全部为 `text` 块，
  不含会被 `cargo test --doc` 编译的示例。
- **存疑项**：注释作者无法验证的断言集中列在 [UNVERIFIED.md](UNVERIFIED.md)（按单元分组），
  阅读时若发现与源码矛盾，先查该文件再怀疑自己。

---

## 8. 继续阅读

| 文档 | 回答的问题 |
|---|---|
| [UNDERSTANDING_GUIDE.md](UNDERSTANDING_GUIDE.md) | 按什么顺序读、每个 crate 该看哪些文件 |
| [ARCHITECTURE_OVERVIEW.md](ARCHITECTURE_OVERVIEW.md) | 系统如何分层、关键设计决策与边界 |
| [EXECUTION_FLOW.md](EXECUTION_FLOW.md) | 从进程启动到一次任务结束的完整时序 |
| [GLOSSARY.md](GLOSSARY.md) | 术语表 |
| [COMMON_MISTAKES.md](COMMON_MISTAKES.md) | 新人最容易误解的地方 |
| [DEBUGGING_GUIDE.md](DEBUGGING_GUIDE.md) | 出问题时如何定位 |
| [COMMENT_STYLE.md](COMMENT_STYLE.md) | 本仓库注释与文档的写作规范 |
| [COMMENT_PROGRESS.md](COMMENT_PROGRESS.md) | 注释覆盖进度、验证结果与待核实事项 |

仓库根还有两份**实现者视角**的权威资料：

- `ARCHITECTURE.md`：crate 划分、每个模块树、共享约定、§8 的 33 条已决/未决事项与风险。
- `spec/*.md`：每个移植单元的逐文件规范（很大，按需定点查阅）。
