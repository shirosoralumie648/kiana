# ADR: `kiana-durable` 的定位

- 状态：已接受
- 日期：2026-11-07
- 相关：`crates/kiana-durable`、`crates/kiana-session`、`crates/kiana-env-client`

## 背景

`kiana-durable` 已经实现了一套完整的持久化执行设施：文档/条目存储、搜索、任务与
inbox、harness 模块树、执行环境（`env`）、测试夹具，以及可选的 SQLite 后端。它比主程序
当前使用的会话数据层更大，也更容易被误当成“第二套 session 存储”。

`kiana-session` / `kiana-session-core` 则负责前台会话：`AgentSession` 的生命周期、
JSONL 会话树、消息投影、压缩与事件订阅。两者如果都承担“后台任务调度”和“跨进程恢复”，
就会出现两套事务语义、两套恢复规则和两条写入路径，最终一定分叉。

## 决定

1. **`kiana-durable` 是后台任务、子代理、cron/定时任务与跨进程恢复的唯一调度和事务
   存储底座。**
   - 需要可靠投递、重试、租约（lease）、watermark、inbox/scheduler 的后台工作，一律
     使用 `kiana-durable` 的 API。
   - 需要“崩溃后恢复、attempt 计数、execution claim、terminal 通知恰好一次”的语义，
     也由 `kiana-durable` 提供；上层不得自行实现第二套队列或状态机。
2. **`kiana-session` 只保留前台会话。**
   - `AgentSession`、`SessionManager`（JSONL 会话树）继续负责用户可见的交互会话。
   - 子代理的后台运行可以挂在 `kiana-session` 的前台运行时之上，但它的持久化任务记录、
     调度与恢复必须落到 `kiana-durable`。
3. **`kiana-env-client` 是 `kiana-durable` 的远端执行实现**，不是独立的任务系统；
   远端命令的生命周期同样由 `kiana-durable` 的任务/执行记录描述。
4. **不新增队列。** 后续工作（子代理后台任务、cron、长任务恢复）优先复用
   `kiana-durable` 的 inbox/scheduler/lease 原语；如果原语不够，先扩展
   `kiana-durable` 的契约，而不是在 `kiana-session` 里再长一套。

## 理由

- 后台任务和前台会话的失败模型不同：前者要跨进程、能重试、需要幂等与租约；后者以
  单进程事件流为主。把两者分开，才能让各自的 API 保持诚实。
- 只有一份调度器，才能保证“同一个任务不会因为进程重启被执行两次”。
- `AgentSession` 已经是 god file 风险区（`agent_session.rs` 体量很大）；把后台调度留在
  它之外，避免继续膨胀。

## 后果

- `kiana-session` 不直接依赖 `kiana-durable` 的存储类型；两者通过任务/执行 id 与
  上层编排交互（需要时在 `kiana-session` 内部做适配层，而不是复制实现）。
- 任何“后台任务”相关的 PR 必须说明它使用的是 `kiana-durable` 的哪个原语；如果新增了
  存储语义，必须同步更新本 ADR。
- `kiana-durable` 的 SQLite / 远端执行 feature 门控保持不变；默认构建不引入额外后端。
