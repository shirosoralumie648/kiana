//! 对应 packages/chord（package name kiana-chord，lib name chord）。
//! TS 源：packages/chord/src/**
//!
//! 第 1 行括号里的 `package name`/`lib name` 来自 `spec/chord-rest.md` §8.1 建议的 Rust
//! crate 布局（`crates/chord/  # package name: kiana-chord（lib name: chord）`；本仓库
//! 实际目录是 `crates/kiana-chord/`）：package name 是 Cargo 包名 `kiana-chord`，spec
//! 建议的 lib name 是 `chord`。当前 `crates/kiana-chord/Cargo.toml` 没有声明 `[lib] name`，
//! lib 目标名按 Cargo 默认规则取 package 名并把 `-` 换成 `_`，所以代码里的导入名是
//! `kiana_chord`。TS 侧 npm 包名是 `@earendil-works/chord`（v1.4.0，见
//! `spec/chord-core.md` §1.2），与本行括号无关。
//!
//! # 这个 crate 解决什么问题
//!
//! Chord 是对象/契约运行时：契约（contract）描述一个对象提供哪些成员、参数与返回值的
//! schema、以及它被放在哪个 host 上；状态用 delta 操作做增量复制；消费者与提供者之间用
//! `wire` 的 JSON 帧通信；host/object/reload/connection 负责对象生命周期、热更新与链路。
//! 本 crate 是 `packages/chord` 的 Rust 移植，按 TS 源文件一一对应（映射表见仓库根
//! `ARCHITECTURE.md` 的 `kiana-chord` 一节）。
//!
//! `lib.rs` 只提供总览和下面的 `pub mod` 清单，**不做任何再导出**：调用方必须写完整模块
//! 路径（例如 `kiana_chord::context::Context`）。TS 的 `index.ts` 对应物是 [`index`] 模块，
//! 但它是普通模块且尚未补齐（见该文件的“现阶段”一节），所以 `kiana_chord::ChordError`
//! 这类短路径不存在。
//!
//! # 在系统中的位置
//!
//! - 上游依赖：`kiana-json`（与 `JSON.stringify` 一致的字符串转义与数字格式，
//!   `schema::canonical_json` 与 `sqlite::schema` 的报错文案会用到）和 `kiana-telemetry`
//!   （`TelemetrySpan`/`BoxFuture`，见 `types`、`declare`、`context`）。`tests/boundary.rs`
//!   锁定这条边界：`[dependencies]` 里只允许这两个 workspace crate。
//! - 下游调用方：`kiana-durable` 使用 `context`/`delta`/`output`/`json`/`sqlite`；
//!   `kiana-env-client` 使用 `context` 与 `contract::is_js_whitespace`。二者都按完整模块路径
//!   导入，与上面的“不做再导出”一致。
//! - 对应 TS：`packages/chord/src/**`。行为级端口集中在 `tests/*.rs`（`context`、`contract`、
//!   `delta_*`、`output*`、`schema`、`types`、`sqlite_node`、`boundary`），各文件头注明了它
//!   对应的 TS 测试或参考实现。
//!
//! # 核心概念与类型
//!
//! - 错误：[`errors::ChordError`] 携带 `code`/`message`/`reason`/`details`；
//!   [`errors::ChordErrorCode`] 固定 12 个码，顺序与 TS 的 `CHORD_ERROR_CODES` 一致，
//!   `retryable` 只由错误码推出。
//! - 契约：[`contract`] 的 `method`/`opaque` 构造成员描述，`host_named`/`host_family`
//!   构造 placement，`remote`/`local` 构造带 `Description` 与 fingerprint 的契约值；
//!   `coverage` 判断 provider 是否覆盖 consumer，`bind` 综合 fingerprint/version 选择
//!   adapter 或报 `contract_mismatch`。[`declare`] 描述资源与监听器声明，[`types`] 给出
//!   引用/调用上下文的 trait。
//! - 状态复制：[`delta::Op`]（7 种操作）与 `apply`/`diff`/`Encoder`/`Decoder`；[`schema`]
//!   提供 canonical JSON、`hash64`、`deep_equal` 与 TypeBox 兼容的校验/默认值/清理。
//! - 链路协议：[`wire::Frame`] 的 23 个变体、[`wire::PROTOCOL_VERSION`]（= 4）与
//!   [`wire::Frame::validate`]。
//! - 调用上下文：[`context::Context`]（不可变分层上下文，附带取消与 telemetry）与
//!   [`types::Ctx`]（Chord 的 root 调用上下文）是两个不同概念，名字相近、用途不同。
//! - 支撑件：[`registry`]（注册表与视图）、[`publish`]（进程级 FIFO 发布队列）、[`output`]
//!   （有界输出缓冲与进度节流）、[`json`]（严格 JSON 深拷贝）、[`sqlite`]（可移植 SQLite
//!   接口；`cloudflare` 是明确的 not supported 占位）。
//!
//! # 一次典型调用
//!
//! 当前快照里 host/object/connection/reload 仍是骨架（见下节），没有“打开对象 → 调用方法”
//! 的端到端路径。已经实现、且有下游实际使用的链路有两条：
//!
//! 1. **状态应用**（`kiana-durable` 的 SQLite 存储）：revision 内容是一段 JSON；逐条调用
//!    `delta::index::parse_op` 解码并校验成 [`delta::Op`]，再用
//!    `delta::index::apply_immutable` 得到新根值（该 crate 的 `apply_revision_ops` 就是按
//!    这个顺序做的）。批量场景可用 `delta::index::apply_immutable_batches`；慢消费者用
//!    [`delta::pending::PendingOps`] 折叠两次读取之间的叶子 op，`delta::index::Encoder`
//!    负责把 op 转回 wire 形式。
//! 2. **调用上下文**（`kiana-durable`/`kiana-env-client`）：下游源码实际使用的是
//!    [`context::Context`]/[`context::ContextRef`]（`ContextRef` 即 `Arc<dyn Context>`）、
//!    `context::with_abort_signal` 与 `context::await_with_context`（`kiana-durable`），
//!    以及 [`context::Context`]/[`context::AbortSignal`]（`kiana-env-client`）。
//!    `context::await_with_context` 等待异步结果，取消时立即返回 `AbortError`，被等的
//!    future 随函数返回而丢弃。`context::create_context_key`/`context::with_context_value`/
//!    `context::with_cancel` 的用法示例目前只在测试里（`crates/kiana-chord/tests/context.rs`
//!    与 `crates/kiana-durable/tests/*.rs`）。
//!
//! 契约构造（`contract::remote`/`contract::local` → `Description`，线上由
//! `wire::DescriptionToken` 承载）目前只有单元与集成测试驱动，还没有 host/connection 调用方。
//!
//! # 现状与边界（重要）
//!
//! 本 crate 是**分单元移植中的骨架**，模块完成度差异很大；按名字找不到项时先确认状态：
//!
//! - 已实现：`errors`、`wire`、`schema`、`contract`、`types`、`declare`、`registry`、
//!   `publish`、`context`、`json`、`output`、`delta`（`draft`/`tracker` 除外）、`sqlite`。
//! - 只有文件级文档、没有任何可调用项的占位：`cell`、`cluster`、`connection`、`host`、
//!   `link`、`loader`、`manager`、`node`、`object`、`reload`、`remote`、`state`、
//!   `telemetry_sqlite`、`watch`、`websocket`，以及 `delta::draft` 与 `delta::tracker`。
//!   这些文件的 `//!` 大多记录了从 spec/TS 源摘出的**计划职责**（少数只有一句 TODO），
//!   都不是已经生效的行为。
//! - [`index`] 只再导出了已存在的 contract/errors/registry/types/declare/context/json/wire
//!   条目；其余等对应单元落地后补齐。
//! - feature：`Cargo.toml` 声明了 `delta`/`context`/`node`/`sqlite`/`sqlite-node`/
//!   `sqlite-cloudflare`/`telemetry-sqlite`/`ws` 以及 `default = ["delta", "context"]`，
//!   但源码里唯一的 `#[cfg(feature = ...)]` 是 `sqlite/mod.rs` 对 `sqlite-node` 的门控
//!   （`context` 模块文档也说明它不受 feature 开关影响）；`sqlite`/`ws` 另外只负责启用可选
//!   依赖 `rusqlite`/`tokio-tungstenite`。
//!
//! # 失败模式
//!
//! 公开 API 以值类型错误为主。契约构造与 schema 校验失败是 [`errors::ChordError`]（schema
//! 校验失败为 `invalid`，`details` 保存前 3 条错误）。帧的完整校验由 `wire::is_frame`、
//! `serde_json::from_value::<Frame>` 与 [`wire::Frame::validate`] 三者叠加，任何一项单独都不
//! 能完成完整校验；连接层尚未实现，所以“校验失败后如何断链”只写在 `wire` 的文档里。
//! delta 用自己的 `delta::index::OpError`（`Invalid`/`UnknownVerb`/`Path`/`UnsafePath` 四个
//! 变体），output 模块用 `output::OutputError`。`errors::as_chord_error` 接受
//! `&(dyn std::error::Error + 'static)`：传入 [`errors::ChordError`] 时原样克隆返回
//! （`context` 参数不参与），其它错误才包成 `unavailable` 并把 `context` 拼进消息
//! （`"{context}: {error}"`）；两条路径都不设置 `cause`（[`errors::ChordError`] 没有该
//! 字段），错误链会丢失，需要保留原始错误类型的调用方应自行记录。
//!
//! # 阅读顺序
//!
//! 1. `errors`：错误码是其余模块错误语义的锚点。
//! 2. `types` 与 `declare`：引用/上下文 trait 与声明形状（TS 的纯类型文件）。
//! 3. `contract`：描述、placement、coverage 与指纹。
//! 4. `schema`：canonical JSON、校验、默认值与清理。
//! 5. `delta`：状态操作语言、应用器与 codec。
//! 6. `wire`：链路帧与协议版本。
//! 7. 支撑模块：`context`、`publish`、`registry`、`output`、`json`、`sqlite`。
//! 8. [`index`]：TS `index.ts` 的再导出对照表。
//! 9. 占位模块：读它们的文件级文档，了解 host/object/connection/reload 的计划边界。

// `ChordError` 的形状由 `errors.rs` 固定（ARCHITECTURE §4.9：12 个码 + message + reason +
// details，`details: Option<serde_json::Value>`），因此它 ≥128 字节。TS 的错误是引用类型、没有
// 这个成本；Rust 侧若把它装箱成 `Box<ChordError>` 会改变整个 crate 的公开签名（而签名是冻结的）。
// 这是**唯一**要压掉的 clippy 警告类别，仅限这一条 lint，不是 `allow(clippy::all)`。
#![allow(clippy::result_large_err)]

// --- 子模块 ---
pub mod cell;
pub mod cluster;
pub mod connection;
pub mod context;
pub mod contract;
pub mod declare;
pub mod delta;
pub mod errors;
pub mod host;
pub mod index;
pub mod json;
pub mod link;
pub mod loader;
pub mod manager;
pub mod node;
pub mod object;
pub mod output;
pub mod publish;
pub mod registry;
pub mod reload;
pub mod remote;
pub mod schema;
pub mod sqlite;
pub mod state;
pub mod telemetry_sqlite;
pub mod types;
pub mod watch;
pub mod websocket;
pub mod wire;
