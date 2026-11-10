//! `test-workers/wake.test.ts` 的 Rust 占位：Cloudflare Durable Object 的 alarm/eviction
//! 运行时不在本 crate 的移植范围内（ARCHITECTURE.md §8 第 3 条：Cloudflare 后端不移植）。
//!
//! 这里用 `#[ignore]` 记录 TS 用例表达的 wake 契约，供以后接入等价运行时：
//! 1. 一个 `sleepUntil` 的任务把 `wake()` 报成 `{ active: false, at }`；实例被驱逐后，新实例
//!    仍从存储读出同一个 `at`，alarm 到期后任务从休眠恢复并完成，完成后没有剩余 alarm；
//! 2. 驱逐切断进行中的一轮时，心跳 alarm 让下一实例恢复它；用被接纳时的同一 `requestId`
//!    重新提交会找回原 submission（不会重复开始一轮），并读到恢复后的回答；
//! 3. 保留文档/上下文的计时器只在主线程运行，不阻止 Durable Object 被驱逐。

#![allow(dead_code)]

#[test]
#[ignore = "needs workers runtime: Durable Object alarm/eviction"]
fn sleeps_through_an_eviction_and_resumes_the_task_from_its_alarm() {
    // TS 断言：wake() == { active: false, at }；alarm 在 at 触发；
    // waitForTask 的结果是 completed，且 result >= at；最后 storage.getAlarm() == null。
}

#[test]
#[ignore = "needs workers runtime: Durable Object alarm/eviction"]
fn recovers_a_turn_an_eviction_cut_off_mid_stream_through_the_heartbeat_alarm() {
    // TS 断言：被驱逐的一轮在 alarm 后恢复；用 requestId "turn-1" 重新 submit 得到 done，
    // answer 条目是恢复后的 assistant 消息 [{ type: "text", text: "recovered" }]。
}

#[test]
#[ignore = "needs workers runtime: Durable Object alarm/eviction"]
fn can_evict_an_object_whose_harness_keeps_idle_documents_and_contexts_in_memory() {
    // TS 断言：后台保留计时器在 Workers 里返回 undefined，因而对象能在 2s 内被驱逐。
}
