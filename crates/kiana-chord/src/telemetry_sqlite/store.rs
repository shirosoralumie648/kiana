//! telemetry-sqlite/store.ts。
//!
//! TODO: 按 TS 源逐文件移植；本 crate 内不得出现第二份实现。
//!
//! # 职责与边界
//!
//! 本文件对应 telemetry-sqlite 的只读查询端（TS `store.ts`）：对写入端（目录根，
//! 对应 `index.ts`）建好的 `telemetry_spans`/`telemetry_metrics` 执行 SELECT，把行
//! 还原为 `kiana-telemetry` 的 `StoredSpan`/`StoredTrace`/`StoredMetric`。按 TS 设计它
//! 不写入、不执行迁移、也不 flush。查询只读取已提交的行：在 `flush`/`close` 把它们写出
//! 之前，内存缓冲中的内容不可见（已结算但未 flush 的 span、尚未 drain 的 metric 区间）；
//! 而 `close` 会以 `drain(true)` 把当前未满 10 秒的 metric 区间也一并写出，因此 `close`
//! 之后 `findMetrics` 能查到该行（TS 测试 `reads committed rows without implicitly
//! flushing, including after close` 锁定）。
//!
//! TS 侧的 `getTrace`/`findTraces`/`findMetrics` 只在模块内导出，**不**出现在
//! `./telemetry-sqlite` 的入口面上；入口只把它们作为 `SqliteTelemetry` 的同名方法
//! 暴露（见 `index.ts` 中的委托）。本文件目前在 Rust 侧还没有任何类型或函数。
//!
//! # 数据模型与查询语义（TS 设计，即移植目标）
//!
//! - 没有独立的 trace 表：一条 trace 由 `trace_id` 相同的 span 行还原。删除某条父 span
//!   行后，它的子 span 仍会出现在 `findTraces`/`getTrace` 的结果中，`parentId` 指向一个
//!   已不存在的 id（测试 `keeps surviving descendants discoverable after a stored
//!   parent is removed` 锁定）。
//! - `getTrace`：按 `started_ms, id` 升序取该 trace 的全部 span；没有行时 TS 返回
//!   `undefined`（而不是空 trace）。
//! - `findTraces`：先用 `WITH heads AS (...)` 按每个 trace 的最早 `started_ms` 生成分页
//!   头，按 `started_ms` 降序、`trace_id` 升序排序并多取一条以判断是否还有下一页
//!   （默认页大小 20）；有结果时再执行一条 `IN` 查询取回本页 trace 的全部 span——因此
//!   非空页固定两条 SELECT，空结果只执行第一条（测试 `uses two span queries for a page,
//!   not one query per trace` 锁定）。结果按 heads 顺序返回，每条 trace 内部的 span 按
//!   `started_ms, id` 升序。`name` 过滤比较的是该 trace 最早那个 span 的名字，
//!   `startMin`/`startMax` 也作用在这个最早起点上；`attributes` 的所有条目必须命中
//!   **同一个** span。cursor 是 `[started_ms, trace_id]` 的 JSON 文本，续页必须原样带回
//!   并保持其余过滤条件。
//! - `findMetrics`：`name` 必填且精确匹配，可选按 `interval_ms` 闭区间与属性过滤，
//!   按 `interval_ms, series_key` 升序返回；不分页。
//! - 行到类型的转换逐字段进行（snake_case → camelCase）：`parent_id` 为 NULL 时结果
//!   省略 `parentId`，`unit` 为 NULL 时省略 `unit`，`attributes`/`events`/`links`/
//!   `status` 从 JSON 文本解析。
//!
//! # 易错点
//!
//! 属性过滤用 `json_each` 逐条生成 `EXISTS (SELECT 1 FROM json_each(...) ...)`，并用
//! `entry.type` 区分布尔与数字：JSON 的 `true` 与数字 `1` 必须视为不相等（TS 源码有
//! 逐字注释说明）。`attributesSql` 在没有属性条目时返回字面量 `"1"`：`findMetrics` 总是
//! 拼上它；`findTraces` 只在 `query.attributes` 存在时拼（存在但为空对象 `{}` 时同样得到
//! `"1"`）。SQL 的文本（包括 `entry.type IN ('integer', 'real')` 中逗号后的空格）与参数顺序
//! 是稳定契约，见 `spec/chord-rest.md` §2.34 与 §4.7；本文件尚未实现，本节只描述语义，
//! 不要据此手写 SQL。
//!
//! 数据库句柄对应本 crate 的 [`crate::sqlite::SqliteDatabase`]（TS 侧同名参数类型是
//! `sqlite` 模块的 `SqliteDatabase`）。
