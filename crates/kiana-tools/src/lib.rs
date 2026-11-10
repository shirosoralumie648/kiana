//! 对应 ca-tools-compaction-export 的工具部分。
//! TS 源：packages/coding-agent/src/core/tools/**
//!
//! # 这个 crate 解决什么问题
//!
//! 实现模型可直接调用的 8 个内置工具——`read`、`bash`、`powershell`、`edit`、
//! `write`、`grep`、`find`、`ls`——以及它们共用的执行与展示基础设施：参数 schema、
//! 工具定义、TUI 渲染器、输出截断、路径解析、文件变更队列和 bash 流式执行器。
//! 它不负责扩展加载（那是 `kiana-extensions`）：本 crate 产出的工具定义与扩展注册的
//! 工具使用同一条 `ToolDefinition` 契约，所以 agent loop 不需要区分二者。
//!
//! 这是 TS `core/tools/**`（加上 `core/bash-executor.ts`）的逐字移植。工具描述、
//! 错误文案、details 形状和 JSON 键序都是对外契约：`tests/` 用 TS 参考实现生成的
//! golden fixture 做字节级比较（如 `tests/tool_definitions_golden.rs`），因此修改
//! 字面量前必须先核对对应测试与 spec。
//!
//! # 在系统中的位置
//!
//! 位于依赖分层 L8（见 `docs/ARCHITECTURE_OVERVIEW.md` §2），只能依赖更低的层：
//!
//! - 契约来源：`kiana-ext-api` 提供 `ToolDefinition`、`ToolRenderers`、
//!   `BashOperations` 等跨 crate 类型；`kiana-agent-core` 提供 `AgentTool` 和 agent
//!   loop 使用的执行签名。
//! - 基础能力：`kiana-core`（路径、子进程、ANSI、临时文件、shell 配置、外部工具
//!   解析）、`kiana-ai`/`kiana-json`（消息类型与 JS 数字/字符串语义）、
//!   `kiana-tui`/`kiana-theme`/`kiana-ui-kit`（渲染）。
//! - 下游：`kiana-session`（`AgentSession::build_runtime` 用 `create_all_tool_definitions`
//!   组装默认工具表；`execute_bash` 直接使用 `bash_executor`）、`kiana-extensions`
//!   （扩展工具经 `definition::wrap_tool_definition` 适配）、`kiana-builtin-extensions`
//!   （MCP/codemode 复用 `truncate`、`render_utils`、`definition`）、
//!   `kiana-interactive`（渲染器与截断）、`kiana-coding-agent`（`sdk_exports` 的 SDK
//!   再导出面）；`kiana-rpc-print` 也依赖本 crate。
//!
//! # 核心概念与类型
//!
//! 每个工具有两份表示，这是读本 crate 最先要建立的模型：
//!
//! - `ToolDefinition`（定义在 `kiana-ext-api`）：完整定义，包含给模型的
//!   `parameters` JSON Schema、系统提示贡献（`prompt_snippet`/`prompt_guidelines`）、
//!   TUI 渲染器、schema 校验前的 `prepare_arguments` 兼容钩子，以及真正的执行逻辑。
//!   `create_*_tool_definition` 返回它。
//! - `AgentTool`（定义在 `kiana-agent-core`）：agent loop 实际执行的瘦视图，只有
//!   `name`/`label`/`description`/`parameters`/`prepare_arguments`/`execute` 等字段，
//!   不携带渲染器与提示元数据。`definition::wrap_tool_definition` 由前者投影出后者
//!   （有损：投影后无法恢复被丢弃的字段）；反向的
//!   `definition::create_tool_definition_from_agent_tool` 供 `AgentSession::build_runtime`
//!   把 `base_tools_override`（`AgentTool`）合成为内部注册表所需的最小
//!   `ToolDefinition`；SDK 的 `custom_tools` 本身就是 `ToolDefinition`，不走这条路径。
//! - [`ToolName`]：8 个工具名的封闭枚举，`as_str`/`Display` 给出线格式字符串。
//!   工具集合与映射使用 `IndexMap` 保持 TS 的键序
//!   `read, bash, powershell, edit, write, grep, find, ls`；这个顺序由
//!   `tests/index.rs` 的 `tool_sets_and_key_order_match_ts` 锁定。
//! - [`ToolsOptions`]：按工具分字段的可选配置。字段为 `None` 时对应的
//!   `create_*` 使用该工具的默认实现（例如默认的本地文件操作）。
//!
//! # 模块地图
//!
//! - 工具实现：`bash`、`powershell`、`edit`（diff 与模糊匹配在 `edit_diff`）、
//!   `read`、`write`、`grep`、`find`、`ls`。
//! - 共享基础设施：`definition`（两份表示的适配）、`file_mutation_queue`（同一
//!   realpath 的文件变更串行化）、`output_accumulator`（bash 输出的有界缓冲与临时
//!   文件）、`truncate`（行/字节双上限截断）、`path_utils`（路径解析与 macOS 变体
//!   回退）、`render_utils`（TUI 通用格式化）、`bash_executor`（`executeBash` 的
//!   流式执行器）。
//! - 注册与展示：`index`（工具名、按名创建、coding/read-only/all 三组集合）、
//!   `renderers`（每个工具的 TUI 渲染器）。
//!
//! crate 根只做 `pub use index::*`；`definition`、`renderers`、`bash_executor` 等
//! 其余 API 必须走模块路径（例如 `kiana_tools::definition::wrap_tool_definition`）。
//!
//! # 一次典型调用
//!
//! 1. `AgentSession::build_runtime` 调用 `create_all_tool_definitions(cwd, options)`
//!    （存在 `base_tools_override` 时改用 `create_tool_definition_from_agent_tool`
//!    逐项合成）；coding / read-only 两组构造函数目前只由测试和外部调用方按需使用。
//!    结果是按固定键序排列的 `ToolDefinition` 集合。
//! 2. 会话把定义存进 `tool_definitions` 注册表（用于系统提示与渲染），再通过
//!    `kiana-extensions` 的 `wrap_registered_tools` 适配成 `AgentTool` 注册表；
//!    适配内部调用的就是本 crate 的 `definition::wrap_tool_definition`。
//! 3. agent loop 以固定签名调用 `AgentTool::execute(tool_call_id, params, signal,
//!    on_update)`：没有扩展上下文参数，包装器用 `ctx_factory` 现建上下文，缺省则用
//!    “无 UI/无会话”的占位单例。
//! 4. 工具实现把 JSON 参数反序列化为自己的 `*ToolInput`，失败时直接作为工具错误
//!    返回；随后执行实际工作，期间可通过 `on_update` 多次推送进度。
//! 5. 成功时返回 `AgentToolResult`：`content` 给模型，`details` 给前端/程序调用者，
//!    还可带 `structured_content`（codemode 使用）。TUI 用 `renderers` 把 `details`
//!    渲染成终端输出。
//!
//! # 边界与失败模式
//!
//! - 未知工具名：`create_tool` / `create_tool_definition` 返回 [`ToolNameError`]，
//!   文案为 `Unknown tool name: {name}`（对应 TS `switch` 的 default 分支）。
//!   直接调用 `create_*_tool_definition` 不会失败，它们不返回 `Result`。
//! - 文件工具（read/write/edit/ls）的自定义操作用 `read` 模块里的
//!   `ToolOperationError`：`message` 是用户可见文案，`code` 通常是 Node 的 errno 名
//!   （如 `ENOENT`；含 NUL 的路径是 `ERR_INVALID_ARG_VALUE`）。默认实现在能识别
//!   errno/`ErrorKind` 时映射成 Node/libuv 风格的 `{code}: {description}, {syscall}
//!   '{path}'`，与 Node 的 `Error.message` 格式一致（当前稳定契约，改动需同步测试）；无法识别时回落为
//!   `error.to_string()` 且 `code` 为 `None`，此时不再是 Node 格式（见
//!   `ToolOperationError::from_io_parts`）；自定义操作可以只给 `message`。
//! - `write`/`edit` 的变更在 `file_mutation_queue` 中按 realpath 串行化。注册 key
//!   时的 `realpath` 错误会直接返回错误，工具不会执行、也不会产生任何文件副作用；
//!   等待中的 future 被取消时队列必须释放，否则同路径后续调用会永久挂起（实现细节
//!   见模块文档）。
//! - `grep`/`find` 依赖外部 `rg`/`fd`，由 `kiana_core::tools_manager` 解析（本地没有
//!   时尝试下载）；不可用时返回工具错误而不是 panic。
//! - 取消用 `CancellationToken` 表达，通过 `execute` 签名传入。取消**不会自动丢弃**
//!   future（`ToolDefinitionExecute` 的文档明确要求实现自己检查并返回）；各工具在
//!   等待点后检查，已取消的调用返回 `Operation aborted` 一类的错误。
//!
//! # 注意事项（修改前必读）
//!
//! - `create_coding_*` / `create_read_only_*` **不含** `powershell`，而 `create_tool` /
//!   `create_all_*` 含。这是照抄 TS 的既有不一致，`tests/index.rs` 与
//!   `default_tools_setting` 测试依赖它，不要“顺手修正”。
//! - `renderers` 不在 crate 根的重导出面上，`index::*` 也不包含它。
//! - `bash_executor` 与 `bash` 工具的输出处理语义不同（前者逐块清理 ANSI/二进制，
//!   后者保留原始字节供渲染阶段解释），不要把两者合并。
//! - 大量行为以“保持当前稳定契约”为准，而不是以更符合 Rust 直觉的实现为准；遇到
//!   看起来奇怪的量纲、排序或错误文案，先查对应模块的“忠实度/已知偏差”说明（改动需同步测试）。
//!
//! # 阅读顺序
//!
//! 1. `index.rs`：先看 [`ToolName`]、[`ToolsOptions`] 与三组集合，理解注册面；
//! 2. `definition.rs`：理解 `ToolDefinition` 与 `AgentTool` 的投影关系；
//! 3. 工具从简到繁：`ls` → `read` → `write` → `edit`（配合 `edit_diff`）→
//!    `grep`/`find` → `bash`（配合 `output_accumulator`、`bash_executor`、
//!    `powershell`）；
//! 4. 最后看横切模块 `file_mutation_queue`、`truncate`、`path_utils`、
//!    `render_utils` 与 `renderers/`。

// --- 子模块 ---
pub mod bash;
pub mod bash_executor;
pub mod definition;
pub mod edit;
pub mod edit_diff;
pub mod file_mutation_queue;
pub mod find;
pub mod grep;
pub mod index;
pub mod ls;
pub mod output_accumulator;
pub mod path_utils;
pub mod powershell;
pub mod read;
pub mod render_utils;
pub mod renderers;
pub mod truncate;
pub mod write;

// --- 与 TS `core/tools/index.ts` 一致的 crate 根导出面 ---
pub use index::*;
