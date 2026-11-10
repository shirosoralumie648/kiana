//! reload/gate.rs：热更新的准入门槛与状态迁移（TS `Reload.gate()`/`Reload.migrate()` 与模块级
//! 的 `dryRun()`/`candidateValue()`）。
//!
//! TODO: 按 TS 源逐文件移植；本 crate 内不得出现第二份实现。
//!
//! ## 当前状态
//!
//! 空占位：没有类型或函数。下面的职责来自 `spec/chord-core.md` §8.1 的布局与
//! `spec/chord-rest.md` §2.20 的流程描述，尚未实现。
//!
//! ## 计划职责
//!
//! §8.1 把 `gate`/`migrate`/`dryRun`/`candidateValue` 分到本文件。`gate()` 只在 prepare
//! 的末尾被调用：`prepareObject` 给需要等待的对象填 `reasons`——状态版本升高，或本次 run
//! 新建独占资源/依赖 deferred 资源的新资源（即使没有旧资源可替换，也会填 reason 并进入
//! gate）；`gate()` 处理这些对象，其余对象在 prepare 里已经完成 `initCells` + `dryRun`。
//!
//! ## gate 的执行顺序（TS 侧，`spec/chord-rest.md` §2.20）
//!
//! 1. 选出 `reasons` 非空且对象仍 live 的候选。
//! 2. 对每个候选安装 gate：`grandfathered` 记录对象当前 leases 的键集合，随后停止对象的
//!    task；集合为空则立即通过。
//! 3. 最多等 `gateDeadline`（默认 5000 ms）。超时抛 `aborted`，reason 为 `gate_deadline`，
//!    message 带截止毫秒数与阻塞项列表，`details.blocking` 就是这些仍未释放的项。
//! 4. 再次检查协调者是否已中止，然后先对全部 gated 候选逐个执行：关闭 `replaced` 里的旧
//!    独占资源（关闭前记入 `closed`，供 rollback 恢复）、`open(atGate)`、`initCells`。
//! 5. 最后统一对每个 gated 候选执行 `migrate`。
//!
//! ## 迁移与 dry run
//!
//! - `migrate` 只处理对象仍在使用的那份 cell 声明：版本升高且有迁移函数时调用
//!   `fn(structuredClone(value), from)`；只有 schema 变化时保留原值，但要按新 schema 重新
//!   校验；其余 cell 跳过。通过校验的值写进候选的 `migrated`。
//! - `dryRun` 在迁移值之上按 `derives` 的 `seq` 升序计算新派生值，对每个值用提交后适用的
//!   全部 schema 校验，结果存进候选的 `derived`。commit 因此不必再校验。
//! - `candidateValue` 读外部 readable 时吞掉 `not_ready`/`not_in_variant` 并返回
//!   `undefined`：被本次 reload 一并升级的 provider 在 commit 前读不到值，dry run 允许
//!   该缺省；其它错误继续抛出。
//!
//! ## 失败与回滚
//!
//! gate 的错误冒泡到 `prepare()` 的 catch：先 rollback，再抛 `reloadError`。gate 期间已
//! 关闭的独占资源由 rollback 重新打开；重开失败时只标记 unavailable 并上报 `unavailable`，
//! 不会静默保持关闭（TS `rollback()` 的注释）。
