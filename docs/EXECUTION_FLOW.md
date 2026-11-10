# 执行流程：从进程启动到一次任务结束

> 本文只描述**当前源码实际做什么**，每一步都给出文件与函数。若行为与文档冲突，
> 以源码和测试为准，并请修正本文。
>
> 阅读建议：先看第 1、4、5 节；需要调试时再看第 3、8、9 节。

---

## 1. 进程入口

文件：`crates/kiana-coding-agent/src/main.rs`

```text
main()
 ├─ kiana_theme::set_theme_json_validator(...)   // 安装主题 JSON 校验器
 ├─ kiana_coding_agent::setup_cli()              // 进程标题、环境标记、HTTP dispatcher
 ├─ tokio::runtime::Builder::new_current_thread()// 单线程运行时
 ├─ LocalSet::new()                              // 允许 !Send future
 ├─ local.block_on(...)
 │    ├─ kiana_extensions::ExtensionHost::new()  // 必须在任何扩展加载之前创建
 │    ├─ kiana_coding_agent::main(args, None)    // 真正的启动编排
 │    └─ Output::process().flush().await
 └─ std::process::exit(code)
```

为什么是 current-thread + `LocalSet`：扩展运行时（rquickjs）与部分会话状态是 `!Send`，
整个应用必须留在同一个线程上。`ExtensionHost` 必须早于任何资源/扩展加载创建，否则
扩展加载会报 `Extension runtime engine is not installed`。

`kiana-rpc` 是第二个入口（`src/bin/kiana-rpc.rs`）：它先设置进程标题与
`KIANA_CODING_AGENT`/`AI_AGENT` 标记，再走同一套 `main`，最终进入 RPC 模式。

---

## 2. 启动编排：`run_main`

文件：`crates/kiana-coding-agent/src/app.rs`，函数 `run_main`。
源码用编号注释标出了 43 个步骤，可以按阶段理解：

### 2.1 早期短路（步骤 1–10）

1. 重置启动计时、安装主题校验器；
2. 组装扩展工厂：内建扩展 + 调用方注入的扩展；
3. `--offline` / `KIANA_OFFLINE`：设置 `KIANA_OFFLINE=1` 与 `KIANA_SKIP_VERSION_CHECK=1`；
4. `auth` 子命令直接执行并返回；
5. Windows 自更新隔离清理、托管安装清理；
6. 读取 cwd、agent 目录，创建 **bootstrap** 设置管理器，应用 HTTP 代理配置；
7. `package`、`config`、`mcp` 子命令各自短路；
8. 解析参数并打印诊断；有错误直接退出码 1。

“bootstrap”设置只用于启动早期（代理、主题、会话目录），此时项目尚未被信任，
所以创建时 `project_trusted: Some(false)`。

### 2.2 配置、模型与信任（步骤 11–39 的中间段）

按顺序包括：迁移旧配置、加载全局/项目设置、解析 provider 凭据、建立信任存储、
刷新模型目录（离线时跳过）、选择模型、处理 `--list-models`/`--help` 等短路路径。
关键点：

- **信任先于项目资源**：项目级扩展、skills、命令是否加载，取决于 `ProjectTrustStore`
  的决策；未信任时只加载全局资源。
- **模型目录可能来自缓存**：远端目录未配置或离线时，只用内置与已缓存模型。
- **所有用户可见输出经 `Output`**：stdout 被 RPC/JSON 模式接管后，普通输出自动改走
  stderr，避免污染协议流。

### 2.3 会话选择（步骤 21–24）

`sessionDir` 优先级：`--session-dir` > `KIANA_CODING_AGENT_SESSION_DIR` > 设置。
然后 `create_session_manager`（`app.rs` 的 `create_session_manager`）打开或新建会话文件：

- `--continue`/`--resume`/选择器决定打开哪个 JSONL；
- 会话记录的 cwd 已不存在时，交互模式弹窗让用户重选，非交互模式报错退出；
- `--name` 追加会话名。

### 2.4 运行时与服务装配（步骤 26–39）

`createRuntime` 工厂（`CreateAgentSessionRuntimeFactory`）把以下东西装配成
`AgentSessionRuntime`：

- `SettingsManager`、`AuthStorage`、`ModelRuntime`；
- 资源加载器（skills、prompt 模板、系统提示、扩展路径）；
- 工具集合、扩展宿主、主题；
- 会话管理器与事件监听。

随后 `create_agent_session_from_services` 创建 `AgentSession`。任何扩展加载失败都会
被汇总成启动诊断；非交互模式下若没有可用模型，直接报错退出。

### 2.5 模式分发（步骤 40–43）

| `AppMode` | 调用 | 说明 |
|---|---|---|
| `Rpc` | `run_rpc_mode(runtime, global_raw_stdout())` | 行协议 JSON-RPC |
| `Interactive` | `InteractiveMode::new(...).run()` | 全屏 TUI |
| `Print` / `Json` | `run_print_mode(...)` | 单次执行 / 逐行 JSON |

RPC 模式在分发前还会后台刷新模型目录（15 秒超时），不阻塞启动。

---

## 3. 会话与代理的边界

- `AgentSession`（`kiana-session/src/agent_session.rs`）是**业务门面**：
  持有 `Agent`、`ModelRuntime`、`SettingsManager`、`SessionManager`、事件监听器；
  公开 `prompt`、`abort`、`set_model`、`subscribe`、`dispose` 等。
- `Agent`（`kiana-agent-core/src/agent.rs`）是**循环的持有者**：
  消息、模型、工具、思考级别、steering/follow-up 队列、订阅者。
- 事件订阅：前端通过 `AgentSession::subscribe` 注册监听器；Agent 产生
  `AgentEvent`，会话层转发给监听器，同时由会话层负责持久化与压缩。

会话线程辅助函数 `run_on_session_thread` / `spawn_on_session_thread`
（`agent_session.rs`）用于把工作投递回会话所在线程，避免跨线程访问 `!Send` 状态。

---

## 4. 一次任务的入口

```text
用户输入
  → 前端（interactive/print/rpc）调用 AgentSession::prompt(text, options)
  → AgentSession 把输入转成 AgentMessage，加入队列
  → Agent::prompt / prompt_messages
  → agent_loop(...) 或 agent_loop_continue(...)
  → 返回 AgentEventStream（事件流）
```

`agent_loop`（`kiana-agent-core/src/agent_loop.rs` 的 `agent_loop`）是**同步返回流**的：

- 创建 `AgentEventStream`；
- `tokio::spawn` 一个任务运行 `run_agent_loop`；
- 循环正常结束时 `stream.end(messages)`；
- 循环失败（provider 报错、`convertToLlm` panic 等）时任务直接退出，
  **不 `end()`、不合成错误事件**——与 TS 没有 `catch` 的行为一致。
  调用方若只等 `result()`，会永远 pending；必须同时处理失败/取消路径。

`agent_loop_continue` 在创建流之前做两项校验：上下文不能为空、最后一条不能是
assistant 消息。

---

## 5. Agent loop 状态机

核心函数：`run_loop`（`agent_loop.rs` 的 `run_loop`）。

```text
run_agent_loop
 ├─ declare_tool_changes(context, prompts)     // 工具声明变化 → 合成消息
 ├─ emit AgentStart / TurnStart
 ├─ 对每条初始消息 emit MessageStart / MessageEnd
 ├─ resolve_stream_fn
 └─ run_loop
      outer loop:
        inner loop（while 有工具调用或有待处理 steering）:
          1. 若有上一轮：prepare_next_turn（可替换 context/model/thinking）
          2. 取 steering 消息（poll_steering）
          3. declare_tool_changes + emit 消息事件
          4. prepare_request（可再次替换 context/model/thinking）
          5. stream_assistant_response：
             transform_context → convert_to_llm → get_api_key
             → stream_fn → 逐事件转成 AgentEvent（MessageStart/Update/End）
          6. stop_reason 为 Error/Aborted：
                finish_turn → TurnEnd → AgentEnd → 返回
          7. stop_reason 为 Length：工具调用参数可能被截断，
                全部按失败处理（不执行）
          8. 否则执行工具批次（顺序或并行）
          9. 工具结果写入 context 与 new_messages
         10. finish_turn → TurnEnd
         11. decision == End → AgentEnd → 返回
         12. decision == Continue → 记录显式继续
         13. 继续条件：有更多工具调用 / 有 steering / 显式继续
        inner 结束:
          - 取 follow-up 消息；有则回到 outer
          - 显式继续则再跑一次
          - 否则 emit AgentEnd 并返回
```

### 5.1 消息与上下文变换

- `transform_context`（可选）：裁剪旧消息、注入外部上下文；
- `convert_to_llm`（必需）：把 `AgentMessage[]` 转成模型能理解的 `Message[]`；
- `prepare_request`：在真正请求前调整模型/思考级别/上下文；
- `prepare_next_turn`：在下一轮开始前调整，常用于压缩后替换上下文。

### 5.2 工具批次

`execute_tool_calls`（`agent_loop.rs` 的 `execute_tool_calls`）先决定模式：

- 配置强制顺序，或**任一**工具声明 `Sequential` → 顺序执行；
- 否则并行执行（`Promise.all` 语义）。

顺序模式：逐个 `start → prepare → execute → end`，结果按调用顺序追加。
并行模式：

1. **预检阶段顺序执行** `tool_execution_start` 与 prepare；未知工具、参数校验失败、
   被钩子拦截、已取消的调用在这里立即产生结果；
2. 所有真正要执行的调用**并发启动**；
3. 结果按**源顺序**收集，但 `tool_execution_end` 事件按**完成顺序**发出。

这是 TS 的精确语义（`spec/agent-codemode.md` §8.2 第 5 点），不要“顺手”改成统一顺序。

`stop_reason == Length` 时，消息被 token 上限截断，工具参数可能不完整，
因此整批按失败处理，而不是执行可能损坏的调用。

### 5.3 steering 与 follow-up

- **steering**：用户在代理运行期间输入的消息。`poll_steering` 按配置的
  `QueueMode`（一次一条 / 一次全部）取；
- **follow-up**：代理本应停止时检查的后续消息；有则继续外层循环；
- 这两条路径保证“用户可以在任务中途插话”，且消息顺序与 TS 一致。

---

## 6. 模型请求与流式响应

`stream_assistant_response`（`agent_loop.rs` 的 `stream_assistant_response`）做这些事：

1. `transform_context` → `convert_to_llm` → `normalize_context`；
2. 解析 API key（过期 token 场景下可能异步刷新）；
3. 合并客户端身份，注入取消信号；
4. 调 `stream_fn(model, context, options)` 得到 `AssistantMessageEventStream`；
5. 循环 `recv()`：

   - `Start`：把 partial assistant 消息放进上下文，发 `MessageStart`；
   - `Update`：发 `MessageUpdate`（前端增量渲染）；
   - `Done`/`Error`：取最终消息（`result()` 在终态事件 push 时已提交），替换 partial，
     发 `MessageEnd`。重试发生在 `kiana-ai` 的 provider/`provider_retry` 内部；
     `agent_loop` 只消费终态事件，**不再重试**。

`stream_fn` 的默认实现由 `kiana-agent-core/src/stream_fn.rs` 提供，最终进入
`kiana-ai` 的 provider 适配器。provider 负责把各自的 SSE/JSON 流翻译成统一的
`AssistantMessageEvent`；错误文本与事件顺序必须与 TS 对齐。

---

## 7. 事件到前端

`AgentEvent` 的主要类型（`kiana-agent-core/src/types.rs`）：

- `AgentStart` / `AgentEnd`
- `TurnStart` / `TurnEnd`
- `MessageStart` / `MessageUpdate` / `MessageEnd`
- `ToolExecutionStart` / `ToolExecutionUpdate` / `ToolExecutionEnd`

不同前端的消费方式：

- **interactive**：`kiana-interactive` 订阅事件，驱动 TUI 重绘、工具面板、状态行；
- **print**：只关心最终 assistant 文本与退出码；
- **json**：每个事件编码成一行 JSON（键序走 `kiana-json`）；
- **rpc**：把事件包装成协议消息，并处理 stdout 背压。

所有前端都必须处理“流没有 `end` 就失败”的情况（见第 4 节）。

---

## 8. 持久化、压缩与导出

- 每轮产生的消息由会话层追加到 JSONL（`kiana-session-core/src/session_manager.rs`）；
- 写入顺序、键序、数字格式是兼容性契约，必须走 `kiana-json`；
- 上下文超过阈值时，`kiana-compaction` 生成摘要并替换旧消息；压缩本身也可能被取消；
- 分支会话用 `branch_summarization` 生成分叉摘要；
- `kiana-export` 把会话渲染为 HTML（ANSI → HTML、工具结果、主题）。

持久化失败会作为会话错误向上返回（`AgentSessionError` 中的运行期持久化错误），
不会静默丢失。

---

## 9. 结束、取消与错误

- **正常结束**：`run_loop` 发 `AgentEnd`，`agent_loop` 的任务调用 `stream.end(messages)`；
- **模型错误/中止**：`stop_reason` 为 `Error`/`Aborted` 时，发 `TurnEnd` + `AgentEnd`
  后返回；上层据此显示错误；
- **用户取消**：`AgentSession::abort` → `request_abort` 设置标志并中止 Agent；
  工具批次在每个调用后检查取消，顺序批次会提前 break；
- **进程退出**：正常路径只有两个二进制入口调用 `std::process::exit`；库内另有启动期 panic
  钩子（`kiana-coding-agent/src/cli/setup.rs` 的 `install_fatal_startup_panic_hook`），
  它把 `FatalStartupError` 映射为 Node 风格的 `Error: <message>` 并以 1 退出。
  `run_main` 内部统一返回 `Result<i32, CliExit>`（`CliResult<i32>`），便于测试在进程内断言退出码。

---

## 10. 一个具体例子（print 模式）

```bash
kiana -p "统计 crates 目录下每个 crate 的源码行数"
```

1. `main.rs` 创建运行时与 `ExtensionHost`；
2. `run_main` 解析 `-p`，进入 `AppMode::Print`；
3. 加载设置/认证/模型，打开或新建会话；
4. `run_print_mode` 创建 `AgentSession`，调用 `prompt`；
5. `Agent::prompt` → `agent_loop`；
6. 第一轮：provider 返回一个 `bash` 工具调用；
7. 工具批次执行 `bash`，结果回灌；
8. 第二轮：模型返回最终文本；
9. `AgentEnd` → 会话把消息写入 JSONL；
10. print 前端输出最终文本，进程以 0 退出。

如果中途 Ctrl-C：取消信号沿 `stream_fn`、工具执行、会话写入传播；工具批次停止，
循环以 `Aborted` 结束，前端按 TS 的退出码与消息处理。
