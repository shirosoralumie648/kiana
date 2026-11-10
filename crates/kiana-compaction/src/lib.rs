//! 对应 core/compaction：切点选择、摘要生成、分支摘要。
//!
//! TS 源：`packages/coding-agent/src/core/compaction/{compaction,branch-summarization,utils,index}.ts`
//!
//! 模块树（`ARCHITECTURE.md` §5 的 kiana-compaction 小节）：
//! - [`utils`]：文件操作跟踪与消息序列化。
//! - [`compaction`]：token 估算、切点选择、摘要 prompt 与 `completeSummarization`。
//! - [`branch_summarization`]：分支摘要。
//! - [`index`]：TS `index.ts` 的再导出面。
//!
//! # 这个 crate 解决什么问题
//!
//! 对话历史超过模型上下文窗口后，继续请求会失败。本 crate 把旧消息压缩成一段结构化摘要：
//! 先决定“保留区从哪里开始”，再把要丢弃的消息序列化成 prompt 交给 LLM 生成摘要，最后返回
//! [`compaction::CompactionResult`] 供调用方写入会话。树导航离开一条分支时，它也能为被放弃
//! 的分支生成“分支摘要”，让返回后的上下文保留该分支的结论。
//!
//! 边界：本 crate 只做“纯计算 + LLM 调用”，不读写会话文件，也不决定何时压缩。触发时机
//! （手动、阈值、溢出）和 settings 由 `kiana-session` 的 `AgentSession` 决定，摘要条目由
//! `SessionManager::append_compaction` 写入会话（是否持久化到文件由该管理器的配置决定）。
//! 依据：`compaction.rs` 文件头注释、
//! `spec/ca-tools-compaction-export.md` §1.1–§1.2、`agent_session.rs` 的 `compact` /
//! `run_auto_compaction`。
//!
//! # 在系统中的位置
//!
//! 分层为 L5（仓库根 `ARCHITECTURE.md` §2.1 的分层表）。代码实际使用的上游 crate 是：
//! `kiana-ai`（模型/消息/流/重试/中断）、`kiana-agent-core`（`AgentMessage`、`StreamFn`）、
//! `kiana-session-core`（`SessionEntry`、会话投影、`convert_to_llm`）、`kiana-json`
//! （JS 数字与 JSON 语义）。
//!
//! 下游调用方：
//! - `kiana-session` 的 `AgentSession`：手动压缩与自动压缩的默认路径都是
//!   [`compaction::prepare_compaction`] → [`compaction::compact`]，然后把结果写入
//!   `SessionManager`，并自行计算 `estimated_tokens_after` 回填。
//! - `kiana-ext-api` 的事件类型直接携带 [`compaction::CompactionPreparation`] 与
//!   [`compaction::CompactionResult`]；扩展可在 `session_before_compact` 中取消压缩或
//!   返回自己的结果（是否采用由 `kiana-session` 决定）。
//! - `kiana-coding-agent` 的 `sdk_exports.rs` 按 TS 原名（camelCase）重导出主要函数
//!   （如 `findCutPoint`、`generateSummary`）；`kiana-rpc-print` 的 RPC 接口也返回
//!   `CompactionResult`。
//!
//! # 核心类型
//!
//! - [`compaction::CompactionSettings`]：`enabled`、`reserve_tokens`、`keep_recent_tokens`；
//!   默认值见 [`compaction::DEFAULT_COMPACTION_SETTINGS`]（`reserve_tokens = 16384`，
//!   `keep_recent_tokens = 20000`）。
//! - [`compaction::CompactionPreparation`]：一次压缩的输入快照——待总结消息、turn 前缀消息、
//!   保留区起点 id、`tokens_before`、前次摘要、文件操作与 settings。
//! - [`compaction::CompactionResult`]：摘要文本（若有文件操作则末尾附清单）、
//!   `first_kept_entry_id`、`tokens_before`、`usage`、`details`。`estimated_tokens_after`
//!   不由 [`compaction::compact`] 填写，由调用方写入条目后回填。
//! - [`utils::FileOperations`] / [`utils::FileValue`]：read/written/edited 三个去重集合，
//!   文件清单由 [`utils::compute_file_lists`] 生成。文件操作会从上次 kiana 生成的压缩条目
//!   的 `details` 继承（`fromHook` 为 JS 真值的条目除外，那类 details 由扩展自己解释）。
//! - [`compaction::ContextUsageEstimate`]、[`compaction::CutPointResult`]：token 估算与切点结果。
//!
//! # 关键机制
//!
//! - token 估算以“最后一次有效 assistant usage”为锚：`tokens = usage 总量 + 其后消息估算`；
//!   没有有效 usage 时全部按 `chars / 4` 估算。usage 早于最近的 `context_edit`/`compaction`
//!   时视为失效（投影会改写上下文），此时重新逐条估算，见
//!   [`compaction::estimate_projected_context_tokens`]。
//! - [`compaction::should_compact`] 的判定是 `enabled && context_tokens + reserve_tokens >
//!   context_window`；Rust 侧用 `u128` 比较，避免 `reserve_tokens > context_window` 时的
//!   无符号下溢（JS 会得到负数，比较结果等价）。
//! - 切点从最新往回累加消息估算，达到 `keep_recent_tokens` 后停在最近的有效切点；toolResult
//!   不是合法切点（它必须紧跟对应的 toolCall）。切点若落在某个 turn 内部，`is_split_turn`
//!   为 true，调用方需要额外总结该 turn 在切点之前的前缀。
//!
//! # 一次典型的压缩调用
//!
//! 1. 调用方取当前分支条目，调用 [`compaction::prepare_compaction`]：构建会话投影、估算
//!    `tokens_before`、在投影后的条目上选保留区起点（内部用 `find_projected_cut_point`；
//!    公开的 [`compaction::find_cut_point`] 是其未投影版本，只供 SDK/测试直接调用），
//!    并收集待总结消息与文件操作。最后一条已经是压缩条目、没有可总结的消息、或取不到有效的保留区起点条目时
//!    返回 `None`（调用方据此跳过，或报 AlreadyCompacted / NothingToCompact）。
//! 2. 调用方可以选择先让扩展的 `session_before_compact` 检查；扩展取消时不会生成摘要，
//!    返回自己的 `CompactionResult` 时采用扩展结果。
//! 3. [`compaction::compact`] 生成摘要：切点落在 turn 中间且存在 turn 前缀消息时，先总结
//!    历史消息（若存在）、再单独总结 turn 前缀，两段用 `---` 与
//!    `**Turn Context (split turn):**` 标题拼接；否则只生成一段。最后把 `<read-files>` /
//!    `<modified-files>` 文件清单追加到摘要末尾。
//! 4. 调用方把压缩条目写入会话并计算压缩后的 token 估算（本 crate 不参与）。
//!
//! 分支摘要的流程：`collect_entries_for_branch_summary` 沿 `parentId` 上溯到共同祖先
//! （不在压缩边界停止）→ `prepare_branch_entries` 按 token 预算从新到旧收集消息 →
//! `generate_branch_summary` 调 LLM，返回 `BranchSummaryResult`。
//!
//! # 边界与失败模式
//!
//! - 摘要生成路径统一返回 `Result<_, CompactionError>`；`Display` 逐字等于 TS 抛出的
//!   `error.message`（`compaction.rs` 的 `error_messages_are_verbatim` 测试锁定）。LLM 的
//!   `StopReason::Length`（TS 的 `"length"`）映射为 `TokenCap`，摘要里出现 toolCall 映射为
//!   `ToolCallAttempted`：两者都避免把不完整的摘要写进会话。
//! - 分支摘要把软失败编码进结果字段而非 `Err`：中止 → `aborted: true`；LLM 错误/length/
//!   toolCall → `error` 文本。只有 `stream_fn` 拒绝、流结果 reject、uuidv7 失败这类硬错误
//!   才返回 `Err`（见 [`branch_summarization::generate_branch_summary`] 的类型说明）。
//! - 所有摘要请求固定关闭 prompt cache（`cacheRetention: "none"`）；`sessionId` 未提供时
//!   现场生成 `uuidv7`。`stream_fn` 的拒绝会先记入共享槽，待重试流程结束后以
//!   `CompactionError::StreamFailed` 原样冒泡。
//! - token 估算、截断与排序按 JS 语义实现：UTF-16 code unit 长度、`Math.ceil(chars / 4)`、
//!   JS 真值、`String.prototype.sort()` 的码元比较。畸形会话条目（`null`、数字、缺字段）
//!   沿用 TS 的 NaN/真值行为，由 `tests/malformed_sessions.rs` 锁定。
//! - 摘要 prompt、分支摘要 preamble 与文件清单格式都是逐字节契约（单元测试断言 UTF-16
//!   长度与首尾文本）；切点、token 估算与文件操作行为则由
//!   `tests/differential_ts_reference.rs` 与 TS 参考实现做差分对比。
//!
//! # 设计约束（修改前必读）
//!
//! - 这是 TS `core/compaction` 的逐行为移植：函数名、序列化字段名（camelCase）、错误文本
//!   都与会话文件和扩展事件共享，属于冻结契约，不要顺手改名或“美化”文本。
//! - 本 crate 的公开项通过 `pub use crate::index::*;` 同时出现在 crate 根：根路径
//!   `CompactionResult` 与 [`compaction::CompactionResult`] 是同一类型，新增公开项会自动
//!   扩大两处公开面。`FileOperations` 只在 [`utils`] 定义一次以避免 glob 歧义（见 [`index`]）。
//!
//! # 阅读顺序
//!
//! 1. [`utils`]：`FileValue`/`FileOperations`、`compute_file_lists`、`serialize_conversation`。
//! 2. [`compaction`]：token 估算 → `should_compact` → 切点 → `prepare_compaction` →
//!    prompt/`generate_summary` → `compact`。
//! 3. [`branch_summarization`]：收集 → 预算裁剪 → 生成。
//! 4. [`index`] 与本文件：再导出面，没有逻辑。
pub mod branch_summarization;
pub mod compaction;
pub mod index;
pub mod utils;

// 第 6 周：上下文压缩与工具输出。
// - pruning：压缩前 tool-result pruning + 聚合预算；
// - structured_summary：split-turn 摘要模板与 prior summary 合并；
// - fallback：确定性 no-LLM 回退；
// - ptl：prompt-too-long 头部丢弃与 overflow 恢复状态机；
// - gate：no-op 缓存 + 5 分钟冷却；
// - recovery：压缩后文件/技能/plan/增量恢复；
// - pipeline：prune → prepare → 摘要 / fallback 的 all-or-nothing 流水线。
pub mod fallback;
pub mod gate;
pub mod pipeline;
pub mod pruning;
pub mod ptl;
pub mod recovery;
pub mod structured_summary;

pub use crate::index::*;
