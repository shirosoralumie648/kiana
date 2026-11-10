//! reload/commit.rs：热更新的发布与回滚（TS `Reload.commit()`/`#publish()`/`install()`/
//! `rollback()`）。
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
//! §8.1 把 `commit`/`#publish`/`install`/`rollback` 分到本文件。TS 侧 `commit` 由
//! `runReload` 在 `prepare()` 成功后调用，集群协调路径（`cluster.ts`，属于 `cluster.rs`
//! 单元）也会直接调用 `commit`/`rollback`；`rollback` 另外由 `prepare()` 的失败路径调用，
//! 因此物理上放在本文件、逻辑上服务于 prepare 与集群协调。
//!
//! ## commit 的边界
//!
//! commit 只做同步发布：所有可能失败的检查（schema 校验、迁移、dry run、listener 订阅）
//! 已在 prepare 完成，TS 的注释明确说它“only installs”。整个发布包在
//! [`crate::publish::hold_publications`] 中：
//!
//! 1. `#publish()` 换代：把 `host.pending` 取为 next，登记进 `codeVersions`，令
//!    `host.previous = current`、`host.current = next`。
//! 2. 遍历 host 的对象：被移除的类型跳过；没有候选的对象把旧实现复制到新 codeVersion；
//!    有候选但对象非 live 时跳过；其余调用 `install` 并记入报告的 `prepared`。
//! 3. 收尾本次新增类型的 deferred 运行期；安装 registry 表；dispose 被移除类型的对象并
//!    删除其 host 引用；写入新 facet、删除 `remove` 中的 facet；清空 `host.gating`；
//!    处理 lifetime 变化的 host 对象（启动它，或释放不再 pinned 的引用）；返回
//!    `ReloadReport`。
//! 4. 安装阶段收集到 `#starts` 的启动动作（task、host 对象、pinned 引用释放、facet run
//!    变更）在 `beforeStart()` 之后才执行。协调者用这个回调先把 commit 帧发给子 host，
//!    保证新代码启动的 root 不会先于 commit 帧到达。
//! 5. 如果安装阶段仍然抛错，host 调用 `fail()` 进入终态（不再接活），再抛 `reloadError`。
//!
//! ## install 的要点
//!
//! - 先停止对象的 task；gated 对象还要 detach 新 run 不再保留的旧 derivation（旧代码不能
//!   再跑在迁移后的状态上）。
//! - 写入新 `decls`/`service`，adopt 保留的资源，登记实现；gated 对象立刻退役其它 code
//!   version 的实现（不等退役截止）。
//! - 先刷新 schema，再对迁移过的 cell 调用 `reset`（订阅者收到 reset 事件）；新 derivation
//!   用 prepare 算好的值 attach，未保留的 refresh。
//! - `client` 引用绑定失败时收到 `upgrade_required`（让浏览器重载），不会阻塞 commit；
//!   其余引用收到 `shapeChanged`。
//!
//! ## rollback 的补偿
//!
//! `rollback()` 幂等（`rolledBack` 标记），中止本 reload 与 facet run 的取消信号，丢弃
//! staged listener，逐个 `pending.delete` + `unwind` 候选，重新打开 gate 期间关闭的独占
//! 资源，重开 gate 并恢复 task，dispose 本次新增但未提交类型上的对象，最后清空
//! `host.pending`/`host.gating`。重开资源失败只标记 unavailable 并上报，不静默关闭。
//!
//! ## 约束
//!
//! - 发布期间不得 await，也不得把 `#starts` 里的启动动作提前到 `#publish` 中执行；TS 的
//!   commit 注释说明观察者在整个步骤结束后才被通知。
//! - 新版本的 task/root 只能在 `beforeStart()` 之后启动。
