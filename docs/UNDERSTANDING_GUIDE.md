# 理解指南：按什么顺序读、每个 crate 看什么

> 本文是 [START_HERE.md](START_HERE.md) 之后的地图。它不重复架构细节，而是回答：
> **“我现在要理解 X，应该按什么顺序打开哪些文件？”**
>
> 所有路径都相对于仓库根。每个 crate 的小节包含：一句话职责、推荐阅读顺序、
> 读完后应该能回答的问题、以及最容易踩的坑。

---

## 0. 三条主线

不要试图按字母顺序读完 28 个 crate。先选一条主线：

| 主线 | 涉及 crate | 适合谁 |
|---|---|---|
| **请求链** | coding-agent → session → agent-core → ai → tools → interactive/rpc-print | 想理解代理如何工作 |
| **扩展链** | ext-api → extensions → builtin-extensions → resources → ui-kit | 想写/改扩展 |
| **持久化链** | session-core → durable → chord → env/env-client | 想理解会话与可恢复执行 |

每条主线都从 `START_HERE.md` 的心智模型开始；读完一条再读另一条。

---

## 1. 最底层：语义与基础设施

### `kiana-json`（先读）

- **职责**：全工作区唯一的 JavaScript 语义 JSON 层（数字格式、键序、解析、字符串）。
- **顺序**：`lib.rs` → `js_num.rs` → `js_string.rs` → `js_object.rs` → `js_parse.rs`
  → `js_parse/partial.rs`。
- **读完应能回答**：为什么不能直接用 `serde_json::to_string`？`1e21`、`-0`、`NaN`
  分别写成什么？`parse` 返回前为什么要重排键序？
- **坑**：`parse` 已经重排过键序；`normalize_numbers` 不重排键；孤立代理项→U+FFFD、
  `1e400`→`null` 是**被迫偏差**（仓库根 `ARCHITECTURE.md` §8 第 31、32 条）。

### `kiana-telemetry`

- **职责**：遥测契约（context/span/metrics/store）与内存/空实现、渲染。
- **顺序**：`lib.rs` → `index.rs` → `memory.rs` → `metrics.rs` → `render.rs` → `testing.rs`。
- **读完应能回答**：span 如何传播？为什么不能用普通 `sort_by`（比较器可能返回 NaN）？
- **坑**：cursor 解码不能用 `kiana_json::parse`（可能差 1 ULP）；timsort 是结果契约的一部分。

### `kiana-env`

- **职责**：远端执行守护进程（独立二进制），在 stdin/stdout 上跑帧协议。
- **顺序**：`Cargo.toml`/`build.rs` → `main.rs` → `frame.rs` → `errors.rs` → `output.rs`
  → `fs.rs`/`list.rs` → `exec.rs`/`pty.rs` → `watch.rs` → `sys/`。
- **读完应能回答**：一帧的字节布局？为什么 stdout 只有单写线程？control 与 bulk 的优先级？
- **坑**：这是 bin-only crate，没有 `lib.rs`；`crates/kiana-env/docs/protocol.md` 等文档被测试逐字锁定，
  不要改动它们来“修注释”。

---

## 2. 模型与协议层

### `kiana-ai`（大，但必须懂）

- **职责**：统一模型/Provider 抽象、42 个 provider、API 适配器、认证/OAuth、HTTP/SSE、
  流式事件、计费与模型目录。
- **顺序**：`lib.rs` → `types.rs`（消息/模型/事件契约）→ `event_stream.rs`
  → `http/mod.rs` + `http/reqwest_client.rs` → `api/lazy.rs` → `api/anthropic_messages.rs`
  （最完整的适配器示例）→ `models/provider.rs` → `providers/catalog.rs`
  → 需要时再看具体 provider/API 文件。
- **读完应能回答**：`AssistantMessageEvent` 有哪些阶段？为什么适配器不能返回 `Err`？
  `lazy_stream` 的同步首 poll 为什么是语义的一部分？
- **坑**：所有出网/落盘 JSON 走 `kiana_json`；错误文本是契约；`stream_simple` 才返回
  `Result`，`*_api()` 包装成 lazy 错误流。

### `kiana-mcp`

- **职责**：MCP 客户端：JSON-RPC 2.0、协议类型、stdio/HTTP/in-memory 传输、OAuth。
- **顺序**：`lib.rs` → `jsonrpc.rs` → `protocol_types.rs` → `transports.rs`
  → `transports/in_memory.rs` → `oauth/flow.rs`。
- **读完应能回答**：请求/通知/响应的判别顺序？连接关闭与超时如何映射成 `McpError`？
- **坑**：这是逐字行为移植，不是通用 SDK；不要“修正”看似奇怪的空白/正则/键序行为。

### `kiana-chord`

- **职责**：Chord 契约、schema、delta、wire 帧、上下文与取消。
- **顺序**：`lib.rs` → `errors.rs` → `schema.rs` → `wire.rs` → `delta/`
  → `contract/`。
- **读完应能回答**：12 个错误码？delta 如何应用/合并？帧的版本与校验？
- **坑**：实际有 60 个源文件，但其中二十多个只有 3 行占位说明；不要把占位模块当作可用 API。

### `kiana-codemode`

- **职责**：把模型生成的 JS 放进 QuickJS(wasm) 沙箱执行，桥接工具调用。
- **顺序**：`lib.rs` → `types.rs` → `runtime/execution.rs` → `runtime/protocol.rs`
  → `runtime/vm.rs` → `wasm.rs`。
- **读完应能回答**：宿主与 VM 的消息协议？为什么需要两个 Engine？为什么 VM 要专用线程？
- **坑**：双 Engine 是 wasm 栈预算要求；VM 线程栈必须 16 MiB；inline 的“调用线程”
  语义依赖事件循环闸门。

---

## 3. 代理与会话层

### `kiana-agent-core`

- **职责**：有状态 Agent、agent loop 状态机、事件流、工具执行管线、默认 StreamFn。
- **顺序**：`lib.rs` → `types.rs`（AgentMessage/AgentEvent/AgentTool/配置）
  → `identity.rs`/`stream_fn.rs` → `agent_loop.rs`（模块文档 → AgentEventStream
  → run_loop → stream_assistant_response → 工具批次）→ `agent.rs`。
- **读完应能回答**：一轮有哪些事件？工具顺序/并行如何决定？失败时流为什么不会 end？
- **坑**：`prompt()` 同步登记 active_run；丢弃 future 不会中断已启动的 run；
  `agent_loop` 不在 tokio runtime 内会 panic。

### `kiana-session-core`

- **职责**：会话 JSONL 的存储/迁移/投影、消息转换、usage/cache 统计。
- **顺序**：`lib.rs` → `messages.rs` → `session_manager.rs` 的模块文档与常量/错误定义
  → 条目结构体与 `SessionEntry`/`FileEntry` → 解析/加载/迁移
  → `TreeNode`/投影/context edit。
- **读完应能回答**：为什么条目有 typed 与 raw 两份？键序如何恢复？何时创建会话文件？
- **坑**：迁移必须同时同步 typed 与 raw；结构体字段顺序会影响磁盘字节。

### `kiana-session`

- **职责**：把 Agent、配置、工具、扩展、持久化组装成可运行的 `AgentSession`。
- **顺序**：`lib.rs` → `agent_session.rs` 模块文档 → `new`/`prompt`/`abort`
  → `agent_session_runtime.rs` → `agent_session_services.rs` → `sdk.rs`。
- **读完应能回答**：会话线程辅助函数解决什么问题？事件如何从 Agent 到前端？
- **坑**：这是业务门面，不要把策略逻辑下沉到 Agent；`!Send` 状态要回到会话线程。

### `kiana-compaction`

- **职责**：上下文压缩与分支摘要（纯计算，I/O 在 session）。
- **顺序**：`lib.rs` → `utils.rs`（文件操作集合、UTF-16 截断）→ `compaction.rs`
  的类型与错误 → token 估算与切点算法 → prompt 与 summary 生成 → `branch_summarization.rs`。- **读完应能回答**：切点如何选？两段式合并解决什么？压缩为何可取消？
- **坑**：`find_cut_point` 不是生产路径，生产用 `find_projected_cut_point`。

---

## 4. 配置、主题与工具

### `kiana-config`

- **职责**：settings/auth/models/keybindings/trust 的磁盘格式与运行期快照。
- **顺序**：`lib.rs` → `settings/schema.rs` → `settings/defaults.rs` → `settings/storage.rs`
  → `settings/migrations.rs` → `auth/lock.rs` → `auth/storage.rs`
  → `models/resolver.rs` → `trust/manager.rs`。
- **读完应能回答**：设置写入为什么是选择性的？未信任项目为什么不能写？凭据如何加锁？
- **坑**：settings 运行期不做 schema 校验；写失败进 `drain_errors`，必须 flush 才能确认落盘。

### `kiana-theme`

- **职责**：56 个语义 token、主题 JSON 校验/加载、系统主题、全局单例、热重载。
- **顺序**：`lib.rs` → `theme_tokens.rs` → `assets.rs` → `theme_schema.rs` → `theme.rs`。
- **读完应能回答**：token 如何映射到颜色？热重载如何通知？全局状态为何要串行测试？
- **坑**：名字与主题由不同锁保护，不保证原子一致；颜色缓存按 `Arc` 指针身份失效。

### `kiana-tools`

- **职责**：8 个内置工具（read/bash/powershell/edit/write/grep/find/ls）与渲染器。
- **顺序**：`lib.rs` → `index.rs` → `definition.rs` → `truncate.rs`
  → `output_accumulator.rs` → 各工具文件 → `renderers/`。
- **读完应能回答**：工具定义如何适配成 `AgentTool`？截断规则？bash 输出如何流式累积？
- **坑**：`create_coding_*` 与 `create_all_*` 的工具集合刻意不同；`wrap_tool_definition`
  会丢弃 `promptSnippet`/`renderers`，这是语义要求。

### `kiana-ui-kit`

- **职责**：9 个无业务依赖的 TUI 小组件 + `diffWords` 移植。
- **顺序**：`lib.rs` → `index.rs` → 任选一个组件 → `diff.rs`。
- **坑**：`render_diff` 必须用 `split('\n')`；只有“单删+单增”才做行内词级 diff。

### `kiana-export`

- **职责**：把会话快照导出成与 TS 原版逐字节一致的单文件自包含 HTML（模板/CSS/JS 内嵌）。
- **顺序**：`lib.rs` → `index.rs`（`SessionData` 组装与手写 JS 语义 JSON）→
  `ansi_to_html.rs` → `tool_renderer.rs` → `assets.rs`（`include_str!` 的内嵌资产）。
- **读完应能回答**：为什么 `SessionData` 有 serde 却仍手写 JSON？工具渲染结果如何进入
  `renderedTools`？ANSI SGR 如何变成内联样式？
- **坑**：`generate_html` 刻意不走 `serde_json::to_value`（深条目会递归栈溢出，且键序/
  数字格式不可控）；`assets.rs` 的注释字节数与实际文件存在文档漂移，以测试哈希为准。

---

## 5. 扩展与资源层

### `kiana-ext-api`

- **职责**：扩展 API 的唯一权威类型契约（41 个事件、工具、UI、provider）。
- **顺序**：`lib.rs` → `index.rs` → `js_number.rs` → `tool_types.rs` → `tool_result.rs`
  → `events.rs` → `ui.rs`。
- **读完应能回答**：扩展能看到哪些类型？事件在何时发出？超时单位是什么？
- **坑**：`ExecOptions.timeout` 是毫秒，`BashExecOptions.timeout` 是秒；
  有两个同名 `ExecOptions`（扩展契约 vs 低层），不能混用。

### `kiana-extensions`

- **职责**：扩展加载器、TS 去类型、CJS、虚拟模块、rquickjs 宿主、UI 桥、事件分发。
- **顺序**：`lib.rs` → `loader/mod.rs` 模块文档 → `loader/js_host.rs`
  → `runner/mod.rs` → `runner/context.rs` → `ui/mod.rs`。
- **读完应能回答**：扩展如何被发现/编译/加载？JS↔Rust 调用方向？宿主何时关闭？
- **坑**：单线程、`!Send`；宿主必须早于加载创建；关闭顺序会影响 Drop。

### `kiana-builtin-extensions`

- **职责**：内建扩展：mcp、codemode、llama、tool-search。
- **顺序**：`lib.rs` → `index.rs` → 按兴趣选一个子目录 → `tool_search/`（BM25 示例）。
- **坑**：它们用与用户扩展相同的 API；不要直接调用其内部函数绕过生命周期。

### `kiana-resources`

- **职责**：skills、prompt 模板、系统提示、资源加载、崩溃日志、bug 报告。
- **顺序**：`lib.rs` → `loader/mod.rs` → `skills.rs` → `prompt_templates.rs`
  → `system_prompt.rs` → `crash_log.rs`。
- **读完应能回答**：资源发现顺序？未信任项目加载什么？系统提示如何拼装？

---

## 6. 前端与入口

### `kiana-tui`

- **职责**：终端组件协议、主屏/备用屏、布局、输入、文本度量、Markdown/LaTeX、剪贴板。
- **顺序**：`lib.rs` → `tui.rs`（Component/Tui）→ `terminal_colors.rs` → `terminal.rs`
  + `stdin.rs` → `main_screen.rs` → 需要时看 `text.rs`/`markdown/`。
- **读完应能回答**：一次按键如何从 stdin 到组件？渲染差分如何工作？为什么要宿主轮询？
- **坑**：没有 main；宿主必须循环调用 `poll_input`/`poll_timers`/`poll_resize`；
  单线程非 `Send`，不要给组件树加锁。

### `kiana-interactive`

- **职责**：交互模式状态机、组件、命令、会话分享、模型刷新。
- **顺序**：`lib.rs` → `interactive_mode.rs` 模块文档 → `interactive_mode/` 的
  `lifecycle`/`events`/`commands` → `components/` → `tui_renderer.rs`。
- **读完应能回答**：交互模式的事件循环？命令如何分发？崩溃如何恢复终端？

### `kiana-rpc-print`

- **职责**：print/json/rpc 三种非交互前端。
- **顺序**：`lib.rs` → `print_mode.rs` → `json_event.rs` → `jsonl.rs`
  → `rpc_mode.rs` → `rpc_types.rs`/`rpc_client.rs`。
- **坑**：协议 stdout 必须经 `RawStdout`/序列化函数；普通输出会被重定向到 stderr。

### `kiana-coding-agent`

- **职责**：CLI、启动编排、迁移、SDK 导出、包管理命令。
- **顺序**：`main.rs` → `app.rs` 的 `run_main` → `cli/args.rs` → `cli/setup.rs`
  → `cli/project_trust.rs` → `migrations.rs` → `sdk_exports.rs`。
- **读完应能回答**：43 步启动顺序？退出码如何统一？信任在何时决定？

### `kiana-evals`

- **职责**：Docker 化评测运行器。
- **顺序**：`main.rs` → `cli.rs` → `plan.rs` → `docker.rs` → `harness.rs` → `report.rs`。

---

## 7. 持久化与远端执行（进阶）

### `kiana-core`

- **职责**：路径、配置、子进程、Git、HTTP dispatcher、输出守卫、语法高亮等基础设施。
- **顺序**：`lib.rs` → `config.rs` → `paths.rs` → `output_guard.rs` → `exec.rs`
  → 按需看 `child_process.rs`、`git.rs`、`http_dispatcher.rs`。
- **坑**：很多签名被冻结为不可失败（TS 的 throw 折叠成 `None`/默认值）；
  `exec_command` 是同步阻塞，不能在 tokio worker 上直接调用。

### `kiana-durable`

- **职责**：可恢复执行：harness、任务图、存储后端、会话日志、执行环境、图像。
- **顺序**：`lib.rs` → `types.rs` → `harness/scheduler.rs` → `harness/task_graph.rs`
  → `harness/tool.rs` → `storage/` → `session/` → `env/`。
- **读完应能回答**：任务如何恢复？事务边界在哪？SQLite 与 JSONL 后端差异？
- **坑**：IndexedDB/Cloudflare 是明确不支持的占位；durable 不依赖 agent-core。

### `kiana-env-client`

- **职责**：env daemon 的客户端：帧协议、远端文件/命令/PTY/watch、SSH 部署与主机密钥。
- **顺序**：`lib.rs` → `connection.rs` → `remote_env.rs` → `ssh.rs`
  → `list_files.rs` → `watch.rs`。
- **坑**：某些类型不在根导出面，要从子模块引用；`Cargo.toml` 里声明但未使用的依赖
  不要当作实现线索。

---

## 8. 怎么读一个很大的文件

1. 先读文件头 `//!`：它应该告诉你文件职责、TS 来源、关键差异；
2. 用 `grep -n '^pub \|^impl \|^fn \|^struct \|^enum '` 列出骨架；
3. 从公开入口函数开始，顺着调用往下读；跳过纯数据/表格；
4. 每读一个阶段，回到文件头确认你理解的是“为什么”而不只是“做了什么”；
5. 用对应测试验证理解：测试名通常就是行为说明。

---

## 9. 高危不变量（改动前必读）

| crate | 不变量 | 破坏后果 |
|---|---|---|
| `kiana-json` | 工作区必须保持 `serde_json/preserve_order`；所有落盘/上网 JSON 走 `kiana_json` | 键序/数字格式与 TS 不一致，黄金测试失败 |
| `kiana-agent-core` | `agent_loop` 失败时不 `end()` 流；`prompt()` 会同步登记 run | 调用方永久等待；丢弃 future 不会取消 run |
| `kiana-session-core` | 条目同时有 typed/raw 表示；结构体字段顺序即磁盘键序 | 迁移只改一边会写入旧值；重排字段破坏兼容 |
| `kiana-session` | 会话与资源加载器绑定创建线程；`dispose` 不幂等 | 跨线程调用 panic；重复 dispose 影响共享 loader |
| `kiana-extensions` | `with_js` 不可嵌套；`Persistent` 必须随引擎 Drop | RefCell 嵌套 panic；JS runtime 断言 abort |
| `kiana-codemode` | VM 跑在 16 MiB 专用线程；两个 Engine 不可合并 | 原生栈耗尽 → SIGSEGV；编译拖垮执行 deadline |
| `kiana-interactive` | 不跨 `.await` 持有 `RefCell` 借用 | 重入 panic（同线程） |
| `kiana-durable` | `Storage::commit` 必须在返回前同步 `adopt` | 内存与磁盘不一致，会话可能 fail |
| `kiana-resources` | `reload` 的 27 步顺序固定，且不是事务 | 优先级/诊断顺序改变；失败留下部分状态 |
| `kiana-tui` | 宿主必须循环驱动 `poll_input`/`poll_timers`/`poll_resize` | 动画/输入/渲染不前进 |
| `kiana-tools` | `create_coding_*` 与 `create_all_*` 的工具集合刻意不同 | 默认工具集与 TS 不一致 |
| `kiana-env` | 帧协议与 `docs/protocol.md` 被测试逐字锁定 | 改文档字面量会让测试失败 |

---

## 10. 自检：你真的理解了吗

读完一条主线后，尝试回答：

- 用户按 Ctrl-C 时，取消信号经过哪些层？哪一层可能不响应？
- 模型返回一个并行工具批次，事件顺序与消息顺序分别是什么？
- 会话文件里的一条 assistant 消息，从内存到磁盘经过哪些转换？键序由谁保证？
- 扩展注册的工具与内置工具，在代理层看来有什么不同？
- 为什么 `KIANA_API_BASE_URL` 未配置时某些功能会“静默跳过”而不是报错？

答不上来时，回到对应 crate 的 README/模块文档与测试；并把问题记到
`COMMENT_PROGRESS.md` 的待核实清单。
