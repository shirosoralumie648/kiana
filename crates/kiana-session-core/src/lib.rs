//! 对应 coding-agent 会话数据层。
//! TS 源：packages/coding-agent/src/core/{messages,session-manager,session-cwd,session-export,usage-totals,cache-stats,virtual-models,nested-tool-calls}.ts
//!
//! 公开面见 [`index`]（TS `core/index.ts` 不导出这些符号，本 crate 按
//! `ARCHITECTURE.md` §4.10 统一从 crate 根再导出）。
//!
//! # 这个 crate 解决什么问题
//!
//! coding-agent 把一次对话保存成 JSONL 会话文件：每条记录（entry）带 `id` / `parentId`
//! 组成一棵树，`leaf` 指针标出当前分支。`kiana-session` 的 `AgentSession`、
//! `kiana-compaction`、`kiana-extensions`、`kiana-coding-agent` 等上层共享同一套会话类型
//! 与投影规则，从而只有一种“把树投影成 LLM 消息”的实现、也只有一种与 TS 版逐字节兼容的
//! 记录格式。实际打开与写入会话文件的调用方在 `kiana-session`、`kiana-coding-agent` 等
//! 上层；`kiana-compaction` 只接收 `&dyn ReadonlySessionManager`，`kiana-extensions` 通过
//! `Arc<dyn ReadonlySessionManager>` 或 `Arc<Mutex<SessionManager>>` 的内存视图访问
//! （JS 侧只暴露 15 个只读成员），两者都不直接打开会话文件、也不写盘。若没有本 crate，
//! 这些逻辑会在每个上层各写一份，文件格式和 JS 兼容语义很难保持一致。
//!
//! 本 crate 不含 agent 主循环（主循环在 `kiana-agent-core`）；它只提供会话层的数据结构、
//! 存储与转换。
//!
//! # 在系统中的位置
//!
//! - 上游：`kiana-ai`（`Message`、`AssistantMessage`、`Usage`、`Model`）、
//!   `kiana-agent-core`（`AgentMessage` 与工具调用结果类型）、`kiana-core`（路径与
//!   `APP_NAME` 等配置）、`kiana-json`（JS 语义的 JSON 写出）。依赖列表见 `Cargo.toml`。
//! - 下游（主要调用方，清单不穷举；完整依赖见 `cargo tree -i kiana-session-core`）：
//!   `kiana-session`（`AgentSession` 持有 `Mutex<SessionManager>`）、`kiana-compaction`、
//!   `kiana-extensions`（只读视图 [`session_manager::ReadonlySessionManager`]、虚拟模型定义）、
//!   `kiana-coding-agent`（SDK 再导出与会话列表查询）、`kiana-config`（虚拟模型运行时、
//!   `CacheWarmer` 的 `SessionManager` 适配）、`kiana-interactive`（会话列表与切换、会话
//!   cwd 校验、usage 统计）、`kiana-export`（直接 `SessionManager::open` 读取原始条目）、
//!   `kiana-ext-api`（事件与类型），以及 `kiana-resources`、`kiana-rpc-print`、
//!   `kiana-tools`、`kiana-builtin-extensions` 等。
//! - TS 对应：文件头第二行列出的 8 个 `core/*.ts`；逐符号对应关系写在各条目的 `TS: ...`
//!   注释里。
//!
//! # 核心概念与类型
//!
//! - [`session_manager::SessionManager`]：会话的主要写入口。提供 `create` / `open` /
//!   `continue_recent` / `in_memory` / `fork_from` 等构造方式、各类 `append_*` 写入、
//!   分支切换与查询。内存中维护 `by_id` 索引与 leaf 指针，写盘以追加为主。
//! - [`session_manager::SessionEntry`]：带类型的条目视图（message、compaction、label 等）。
//!   从文件加载的条目同时保留原始 JSON，未知条目类型落到 `SessionEntry::Unknown`；重写、
//!   fork 与导出时逐字回放，避免丢失未知字段和键序。
//! - [`session_manager::ReadonlySessionManager`]：供扩展上下文与压缩单元使用的只读视图。
//! - [`session_manager::SessionProjection`] / `SessionContext`：沿 leaf 分支把条目投影成
//!   模型可见消息，并给出生效的 thinking level 与模型。
//! - [`messages::convert_to_llm`]：把 `AgentMessage`（含四种自定义消息）转换成 LLM 认识的
//!   标准 `Message`。
//! - [`virtual_models`]：虚拟模型只存在于选择层，路由后下游只看到物理模型。
//! - [`nested_tool_calls`]：`ctx.executeTool()` 嵌套调用的记录与按需串行化执行（仅当
//!   host 的 `is_sequential()` 为真或工具 `execution_mode == Sequential`、且调用自身未持有
//!   队列锁时才在 `tokio::sync::Mutex` 上排队；其余调用不取锁、可并发执行）。
//! - [`usage_totals`] / [`cache_stats`]：token、成本与 prompt cache 浪费统计。
//! - [`session_export`] / [`session_cwd`]：JSONL 导出与持久化会话的 cwd 校验。
//!
//! # 一次典型调用
//!
//! 以 `kiana-session` 的 `AgentSession` 为例（见 `crates/kiana-session/src/agent_session.rs`）：
//!
//! 1. 启动时调用 `SessionManager::create` / `open` / `in_memory`，把得到的管理器包在
//!    `Mutex` 里持有。`fork_from` 不在 `kiana-session` 内调用，它由 `kiana-coding-agent`
//!    的 CLI fork 入口使用；`AgentSession::fork` 走 `SessionManager::open` +
//!    `create_branched_session`（目标 leaf 为空时改用 `create` + `new_session`）。
//! 2. 每轮产生消息后调用 `append_message` 等 append API，返回新条目 id；落盘时机见下方
//!    “边界与失败模式”。
//! 3. 构建模型请求前调用 `build_session_projection`（或 `build_session_context`），得到
//!    `SessionProjection.messages`、thinking level 与模型；`AgentSession` 用它刷新 agent
//!    的消息列表。
//! 4. 交给模型前再经 [`messages::convert_to_llm`] 投影为标准 `Message`；`AgentSession`
//!    也用它的输出为虚拟模型路由准备输入。
//!
//! 文件读路径与写路径的逐步说明在 [`session_manager`] 的模块文档里。
//!
//! # 边界与失败模式
//!
//! - 构造、写入/切换（`set_session_file`、`new_session`、`append_*`、`branch*`、
//!   `create_branched_session`）以及可能失败的查询（如 `find_by_id`）返回
//!   [`session_manager::SessionManagerError`]；纯读取 getter（`get_entry` / `get_branch` /
//!   `get_entries` / `get_tree` / `get_leaf_id` / `build_session_projection` 等）直接返回
//!   `Option` / `Vec` / 投影值，不会失败。该错误的 `Display` 文本是当前稳定契约（改动需
//!   同步测试），但只有经内部 `node_fs_error` 包装的打开/创建/mkdir 错误才按 Node 的
//!   `<CODE>: <description>, <syscall> '<path>'` 形式重建；读取/写入中途的 `io::Error`
//!   （如 `load_stored_entries_from_file` / `read_session_header` 里的 `File::open`、
//!   `reader.read`）经 `From<io::Error>` 直接变成 `SessionManagerError::Io`，仍按 Rust 的
//!   `Display` 输出（如 `Is a directory (os error 21)`）。展示错误时应使用该错误的
//!   `Display`，而不是底层 `io::Error` 的 `Display`。
//! - 首次落盘要求会话里已有 user/assistant 消息：只有 setup 条目（模型、thinking level）
//!   的会话不会创建文件；首个 user 消息即使没有得到回复也会被保存。
//! - `open` 读取 header 超过 `MAX_SESSION_HEADER_SCAN_BYTES` 时回退为全量加载。
//! - `in_memory` 构造的会话永不落盘。
//! - 解析刻意宽松：未知条目类型保留为原始 JSON / `SessionEntry::Unknown`，不会因为单个
//!   未知字段拒绝打开；但首个条目必须能反序列化成 header，否则报 `InvalidSessionFile`。
//! - 列表 API `list` / `list_all` / `list_all_in` 是 async（tokio 并发加载），错误类型为
//!   [`session_manager::SessionListError`]；通过 `AbortScope` 取消时 `name` 为
//!   `"AbortError"`（中止原因自带 name 时以它为准）。
//! - 写盘字节必须保持稳定（字段顺序、数字格式、缺失键省略）：这是当前稳定契约，改动需
//!   同步测试。改动序列化或条目顺序会影响磁盘上的会话文件，以及所有直接依赖本 crate 的下游，例如
//!   `kiana-export`（直接 `SessionManager::open`，并用 `get_entries_json` /
//!   `get_header_json` 读取原始 JSON）、`kiana-interactive`、`kiana-config` 等
//!   （完整列表见 `cargo tree -i kiana-session-core`）。
//!
//! # 阅读顺序
//!
//! 1. [`session_manager`]：文件格式、树与分支、投影以及 `SessionManager` 本身。
//! 2. [`messages`]：自定义消息与 `convert_to_llm`。
//! 3. [`usage_totals`]、[`cache_stats`]、[`session_export`]、[`session_cwd`]：围绕会话
//!    条目的统计、导出与校验。
//! 4. [`virtual_models`]、[`nested_tool_calls`]：两个相互独立的子系统。

// --- 子模块 ---
/// TS: cache-stats.ts#computeCacheWaste
pub mod cache_stats;
/// 第 6 周：压缩提交事务（字节级快照 + 内存回滚）。
pub mod compaction_support;
/// TS: session-manager.ts#SessionManager
pub mod index;
/// TS: messages.ts#convertToLlm
pub mod messages;
/// TS: nested-tool-calls.ts#NestedToolCallRunner
pub mod nested_tool_calls;
/// 第 11 周后半：会话恢复分类与修复（Clean/InterruptedPrompt/InterruptedTurn/DegradedHistory）。
pub mod recovery;
/// TS: session-cwd.ts#getMissingSessionCwdIssue
pub mod session_cwd;
/// TS: session-export.ts#serializeSessionBranch
pub mod session_export;
/// TS: session-manager.ts#SessionManager
pub mod session_manager;
/// TS: usage-totals.ts#combineUsage
pub mod usage_totals;
/// TS: virtual-models.ts#getBranchSelection
pub mod virtual_models;

pub use index::*;
