//! telemetry-sqlite 目录。
//!
//! TODO: 按 TS 源逐文件移植；本 crate 内不得出现第二份实现。
//!
//! # 这个目录负责什么
//!
//! 按 TS 源，它对应 `kiana-telemetry` 契约的一个 SQLite 后端：写入端把已结算（settled）
//! 的 span 与按时间区间聚合的 metric 存进调用方提供的 SQLite 数据库，读取端在同一批表
//! 上提供只读查询（见子模块 [`store`]）。遥测契约本身（`TelemetryContext`、
//! `TelemetryStore` 等）定义在 `kiana-telemetry`；本目录不定义契约，也不负责打开数据库。
//!
//! 对应 TS：`packages/chord/src/telemetry-sqlite/index.ts`（入口与写入端）和
//! `store.ts`（读取端）。TS 包通过 `./telemetry-sqlite` 子路径导出
//! `createSqliteTelemetry` 及类型 `SqliteTelemetry`、`SqliteTelemetryOptions`；
//! `store.ts` 的三个查询函数不在入口面上，只作为 `SqliteTelemetry` 的方法暴露。
//!
//! # 一次完整流程（TS 设计，即本模块的移植目标）
//!
//! 1. 创建适配器时先执行迁移，建表 `telemetry_spans` 与 `telemetry_metrics`。
//! 2. 录制只写内存缓冲，期间不执行 SQL：span 在回调结算后才入缓冲；metric 按
//!    `(10 秒区间, name, kind, unit, 键已排序的属性 JSON)` 聚合成 count/sum/min/max，
//!    `description` 与 histogram 的 `boundaries` 不参与存储。
//! 3. 每秒一次的定时器、显式 `flush` 或 `close` 触发 drain：在一个事务里批量 INSERT，
//!    并在同一事务中删除超出保留期（默认 7 天）的旧行。`flush` 跳过尚未结束的 10 秒
//!    metric 区间，只有 `close` 会把它们一并写出。
//! 4. 读取端只看到已提交的行；查询不会隐式 flush，所以刚结算的 span 要先 flush 才可见。
//! 5. `close` 停止录制并 drain，但不等待仍在执行的业务回调；之后才结算的 span 与之后
//!    记录的 metric 会被丢弃，数据库本身保持打开。
//! 6. 写入失败是终态：录制停止、缓冲清空、定时器清除，之后 `flush`/`close` 重抛同一个
//!    拒绝值（即使它以 `undefined` 拒绝），不重试。
//!
//! # 当前状态与约束
//!
//! 本次移植尚未实现本模块：目录内目前只有 [`store`] 子模块声明，没有任何公开类型或
//! 函数。上述行为由 TS 测试锁定（`test/telemetry-sqlite.test.ts`、
//! `test/telemetry-sqlite-store.test.ts`），完整规格见 `spec/chord-rest.md` §2.34 与 §4.7。
//!
//! Cargo.toml 声明了 feature `telemetry-sqlite = ["sqlite"]`，但当前 `lib.rs` 无条件
//! `pub mod telemetry_sqlite;`，且本模块不依赖 `rusqlite`；实现前需先核对 feature 组合
//! 与模块声明是否一致。查询与写入的数据形状使用 `kiana-telemetry` 的 `StoredSpan`、
//! `StoredTrace`、`StoredMetric`、`TraceQuery`、`MetricQuery` 等类型（本 crate 已依赖该
//! crate），不要在本 crate 内另造一套。
//!
//! TS 源码与测试中的 carrier 键逐字是 `pi-sqlite-span` / `pi-sqlite-trace`，而
//! `spec/chord-rest.md` §2.34 写作 `kiana-sqlite-*`；这属于外部可见格式，移植前需确认
//! 采用哪一组，不要照抄其中一边。

// --- 子模块 ---
pub mod store;
