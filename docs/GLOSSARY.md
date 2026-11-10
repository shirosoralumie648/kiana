# 术语表

按主题分组。每条给出**通俗解释**与**代码落点**，便于从概念跳到源码。
术语首次出现时若与 TypeScript 参照实现同名，会注明 TS 名称。

---

## 1. 基础与构建

| 术语 | 解释 | 代码落点 |
|---|---|---|
| **workspace（工作区）** | 一个 Cargo 管理多个 crate 的集合；本仓库的 28 个 crate 共享依赖版本与构建配置 | `Cargo.toml` |
| **crate** | Rust 的编译单元：一个库（`lib.rs`）或一个二进制（`main.rs`/`bin/*.rs`） | `crates/*/` |
| **feature** | Cargo 的条件编译开关，例如 `wasm`、`images`、`runtime`；关闭后相关模块可能完全不参与编译 | 各 crate 的 `Cargo.toml` `[features]` |
| **`build.rs`** | 在编译 crate 之前运行的构建脚本，用来生成代码或嵌入资源 | `crates/kiana-ai/build.rs`、`crates/kiana-env/build.rs` |
| **path dependency** | 用本地路径而不是版本号引用的 crate 依赖；本仓库 crate 之间全部如此 | `[workspace.dependencies]` |
| **`!Send`** | 该值不能安全地跨线程移动。rquickjs 的 JS 值和部分会话状态是 `!Send`，因此整个应用跑在单线程 `LocalSet` 上 | `crates/kiana-coding-agent/src/main.rs` |
| **`LocalSet`** | tokio 提供的“单线程任务集合”，允许 `spawn_local` 非 `Send` future | 同上 |
| **`CancellationToken`** | tokio-util 的取消信号；对应 TS 的 `AbortSignal`，可克隆、可向下传递 | `crates/kiana-core/src/abort.rs` |

---

## 2. 代理与会话

| 术语 | 解释 | 代码落点 |
|---|---|---|
| **Agent** | 有状态的代理对象：持有消息历史、当前模型、工具集合与订阅者，负责发起一轮轮模型调用 | `crates/kiana-agent-core/src/agent.rs` |
| **agent loop（代理循环）** | 驱动“请求模型 → 消费流式事件 → 执行工具 → 再请求”的状态机 | `crates/kiana-agent-core/src/agent_loop.rs` |
| **AgentSession** | 面向应用层的会话门面：把 Agent、配置、工具、扩展、持久化、UI 事件组装在一起 | `crates/kiana-session/src/agent_session.rs` |
| **AgentMessage** | 代理内部的消息类型，比 LLM 消息更宽：可包含 UI 专用或自定义消息 | `crates/kiana-agent-core/src/types.rs` |
| **Message** | 只发给模型的三种消息：`user`、`assistant`、`toolResult`；由 `convert_to_llm` 从 `AgentMessage` 转换而来 | `crates/kiana-ai/src/types.rs` |
| **turn（轮）** | 一次“用户输入 → 模型输出（可能含多次工具调用）→ 结束”的过程 | `kiana-agent-core` |
| **content block（内容块）** | 消息内容的最小单元：文本、图片、思考、工具调用等 | `crates/kiana-ai/src/types.rs` |
| **tool call（工具调用）** | 模型请求执行某个工具及其 JSON 参数 | `crates/kiana-ai/src/types.rs`、`kiana-tools` |
| **tool result（工具结果）** | 工具执行后回填给模型的结果，通常以 `toolResult` 消息进入下一轮 | 同上 |
| **thinking level（思考级别）** | 模型推理预算的抽象（off/low/medium/high 等），映射到各 provider 的参数 | `crates/kiana-ai/src/types.rs` |
| **compaction（压缩）** | 上下文过长时，把旧消息摘要成更短的消息，保留任务连续性 | `kiana-compaction` |
| **branch summarization（分支摘要）** | 从某条消息分叉出新会话时，为分叉点生成摘要 | `crates/kiana-compaction/src/branch_summarization.rs` |

---

## 3. 模型与网络

| 术语 | 解释 | 代码落点 |
|---|---|---|
| **provider（提供方）** | 一家模型服务及其协议适配器，例如 Anthropic、OpenAI、Google、Bedrock | `crates/kiana-ai/src/providers/`、`src/api/` |
| **model catalog（模型目录）** | 内置 + 远端可用的模型清单、能力与价格 | `crates/kiana-ai/src/model_catalog.rs`、`crates/kiana-ai/data/` |
| **streaming / SSE** | 服务端逐块推送响应；本仓库把 SSE 解析成统一的 `AssistantMessageEvent` | `crates/kiana-ai/src/api/*`、`src/utils/streaming_json.rs` |
| **`AssistantMessageEventStream`** | 代理层消费的流：`push`/`end`/`result`/`recv`，容量 4096 | `crates/kiana-ai/src/event_stream.rs` |
| **MCP** | Model Context Protocol：让外部进程以 JSON-RPC 向代理提供工具/资源 | `kiana-mcp` |
| **JSON-RPC** | MCP 使用的请求/响应/通知消息格式 | `crates/kiana-mcp/src/jsonrpc.rs` |
| **transport（传输）** | MCP 的承载方式：stdio、HTTP、WebSocket 等 | `crates/kiana-mcp/src/transports/` |
| **CIMD / DCR** | MCP 的 OAuth 客户端元数据发现方式；本仓库在未配置端点时明确报错而非回退上游 | `crates/kiana-mcp/src/oauth/` |
| **HTTP dispatcher** | 进程级 HTTP 代理/超时配置的统一入口 | `crates/kiana-core/src/http_dispatcher.rs` |

---

## 4. 工具与扩展

| 术语 | 解释 | 代码落点 |
|---|---|---|
| **tool（工具）** | 模型可调用的一个动作，如读文件、改文件、跑 bash；参数与结果都是 JSON | `kiana-tools` |
| **extension（扩展）** | 用 TS/JS 写的插件，可注册工具、命令、UI、事件钩子 | `kiana-ext-api`、`kiana-extensions` |
| **ExtensionHost** | 扩展运行时的宿主：持有 JS 引擎、加载器、已注册扩展，必须早于应用创建并一直存活 | `kiana-extensions` |
| **JS engine（JS 引擎）** | 内嵌的 QuickJS（通过 rquickjs）；与 Rust 同线程，值是 `!Send` | `crates/kiana-extensions/src/loader/js_host.rs` |
| **host bridge（宿主桥）** | Rust 暴露给 JS 的函数与对象；调用方向是 JS → Rust | `crates/kiana-extensions/src/runner/` |
| **virtual module（虚拟模块）** | 加载器为扩展提供的伪模块（如内置 API），不来自磁盘 | `crates/kiana-extensions/src/virtual_modules.rs` |
| **builtin extension（内建扩展）** | 随程序发布、用同一套扩展 API 实现的扩展：mcp、codemode、llama、tool-search | `kiana-builtin-extensions` |
| **skills** | 预置的任务说明/提示包，由资源加载器发现并注入系统提示 | `crates/kiana-resources/src/skills/mod.rs` |
| **prompt template** | 可复用的提示片段 | `crates/kiana-resources/src/prompts/mod.rs` |
| **resource loader** | 启动时加载 skills、模板、系统提示、扩展的统一入口 | `crates/kiana-resources/src/loader/mod.rs` |
| **codemode** | 让模型生成一段代码、在沙箱（QuickJS/WASM）里执行，而不是逐个调用工具 | `kiana-codemode` |

---

## 5. 持久化与执行

| 术语 | 解释 | 代码落点 |
|---|---|---|
| **session JSONL** | 会话落盘格式：一行一个 JSON 记录（消息、元数据、事件） | `crates/kiana-session-core/src/session_manager.rs` |
| **session dir** | 会话文件所在目录，默认 `~/.kiana/agent/sessions` | `crates/kiana-core/src/config.rs` 的 `get_sessions_dir` |
| **durable（持久化执行）** | 把长任务拆成可恢复的步骤，状态落盘，崩溃后可继续 | `kiana-durable` |
| **harness** | durable 的执行框架：调度器、任务图、工具、会话日志 | `crates/kiana-durable/src/harness/` |
| **task graph（任务图）** | durable 中任务之间的依赖关系 | `crates/kiana-durable/src/harness/task_graph.rs` |
| **storage backend（存储后端）** | durable 的存储抽象及实现：JSONL、SQLite；IndexedDB/Cloudflare 是明确不支持的占位 | `crates/kiana-durable/src/storage/` |
| **`ExecutionEnv`** | 工具执行环境的抽象：本地、远端（SSH）、测试替身 | `kiana-durable`、`kiana-env-client` |
| **env daemon** | 跑在远端机器上的小守护进程，通过 stdin/stdout 帧协议执行文件与命令操作 | `kiana-env` |
| **frame protocol（帧协议）** | `kiana-env` 的二进制帧格式：请求/结果/错误/取消/心跳 | `crates/kiana-env/src/frame.rs` |
| **chord** | 多方会话/对象的同步协议与存储层（帧、delta、对象图） | `kiana-chord` |
| **delta** | 对对象图的最小变更集，可应用、可合并 | `crates/kiana-chord/src/delta/` |
| **trust（信任）** | 项目级安全决策：是否允许执行该项目的配置/扩展/命令 | `crates/kiana-config/src/trust/` |
| **settings / auth store / models store** | 三类持久化配置：普通设置、凭据、模型目录缓存 | `crates/kiana-config/src/` |

---

## 6. 界面与输出模式

| 术语 | 解释 | 代码落点 |
|---|---|---|
| **TUI** | Terminal User Interface：全屏终端界面 | `kiana-tui`、`kiana-interactive` |
| **Component** | TUI 的可组合单元，负责渲染与处理输入 | `crates/kiana-tui/src/tui.rs` |
| **overlay（覆盖层）** | 浮在基础界面之上的临时窗口（菜单、对话框） | `kiana-tui`、`kiana-interactive` |
| **alt screen（备用屏）** | 终端的第二块屏幕缓冲区，退出时恢复原屏幕 | `crates/kiana-tui/src/alt_screen.rs` |
| **ANSI / OSC** | 终端控制序列；OSC 52 用于剪贴板 | `crates/kiana-tui/src/colors.rs`、`src/native/` |
| **interactive mode** | 默认的全屏交互模式 | `kiana-interactive` |
| **print mode** | `-p` 单次执行，把最终文本写到 stdout | `crates/kiana-rpc-print/src/print_mode.rs` |
| **json mode** | 把事件写成逐行 JSON | `crates/kiana-rpc-print/src/json_event.rs` |
| **RPC mode** | 外部程序通过 JSON-RPC 风格的行协议驱动代理 | `crates/kiana-rpc-print/src/rpc_mode.rs` |
| **JSONL** | 每行一个 JSON 对象；会话文件与 RPC 输出都用它 | 多处 |
| **share** | 把会话发布为 gist 并提供查看器链接 | `crates/kiana-interactive/src/session_share.rs` |

---

## 7. 测试与保真

| 术语 | 解释 | 代码落点 |
|---|---|---|
| **golden test（黄金测试）** | 用参照实现的真实输出作为基准，逐字节比较 | `crates/*/tests/`、`crates/*/tests/golden/` |
| **fixture** | 测试输入/期望值的固定文件 | `crates/*/tests/fixtures/` |
| **snapshot** | 用 `insta` 保存的结构化快照 | `*.snap` |
| **conformance（一致性测试）** | 同一套接口契约在多个实现上重复运行，确保行为一致 | `crates/kiana-telemetry/src/store_conformance.rs`、`crates/kiana-durable/src/testing/` |
| **parity（对齐）** | 与 TypeScript 参照实现在可观察行为上一致 | `spec/PROGRESS.md`、`spec/GAPS.md` |
| **wire format（线格式）** | 跨进程/网络传输的字节格式；改动会破坏兼容 | `crates/kiana-env/src/frame.rs`、`kiana-mcp`、`kiana-chord` |

---

## 8. 容易混淆的词

- **Agent vs AgentSession**：Agent 是“会思考的循环”，AgentSession 是“一次对话的业务门面”。
- **Message vs AgentMessage**：前者是模型协议，后者是代理内部（可含 UI 消息）。
- **Session vs session file**：会话是内存对象；session file 是它的 JSONL 持久化。
- **Extension vs builtin extension**：前者是用户插件，后者是官方用同一套 API 写的内建插件。
- **Provider vs model**：provider 是厂商/协议，model 是具体模型条目。
- **Compaction vs export**：压缩是为了继续对话；导出是生成 HTML 归档。
- **Durable vs session**：session 记录一次对话；durable 记录跨步骤可恢复的任务执行。
