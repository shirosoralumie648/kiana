# 注释清理工作单：kiana-session

- 待处理条目：5
- 涉及文件：4

## 规则（必须遵守）

1. **只改注释/文档文字，不改代码、字符串字面量、测试断言、类型签名或行为。**
2. 目标是把“因为 TS 这样，所以必须这样”改成“当前设计的技术原因”，或者删除纯 TS 理由。
3. 如果注释描述的是用户可见契约（错误文案、字段顺序、落盘格式、ANSI 输出），保留契约，但不要再用 TS 作为权威；改成“当前稳定契约，改动需同步测试”。
4. 如果注释只是出处（`TS 源：...`），保留出处，把约束性措辞去掉；不要删除整个出处。
5. D 类没有明确提到 TS：先判断是否仍有独立技术原因；有就保留并补技术原因，没有就删除。
6. 不要重写无关注释，不要扩大 diff；一个文件尽量只动列出的行附近。
7. 不要运行 `cargo fmt` / `cargo check` / 测试；统一由 verifier 执行。
8. 不要修改本工作单或 `docs/internal/comment-audit/scan*.csv`。

## 文件清单

- `crates/kiana-session/src/agent_session.rs`
- `crates/kiana-session/src/agent_session_runtime.rs`
- `crates/kiana-session/tests/regression_8328_zero_usage_auto_compaction.rs`
- `crates/kiana-session/tests/regression_9340_auto_compaction_cancellation.rs`

## 待处理条目

| # | 位置 | 类别 | 命中 | 摘要 | 建议 |
|---:|---|---|---|---|---|
| 1 | `crates/kiana-session/src/agent_session.rs:803` | A | 逐字一致, TS的 | /// 其它（逐字保留 TS 的 message）。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 2 | `crates/kiana-session/src/agent_session_runtime.rs:1347` | A | 与TS一致 | // agent_session_services 模块；这里只是保持 import 路径与 TS 一致（调用方可从任一 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 3 | `crates/kiana-session/tests/regression_8328_zero_usage_auto_compaction.rs:53` | A | 与TS一致 | // 与 TS 一致：消息既要进 agent 状态，也要在会话分支里（压缩准备按分支切分）。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 4 | `crates/kiana-session/tests/regression_9340_auto_compaction_cancellation.rs:95` | A | TS等价物, TS里 | // 同步设置 abort 标志，等价于 TS 里 `void this.abort()` 的同步部分。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 5 | `crates/kiana-session/src/agent_session.rs:224` | D | 逐字一致 | /// `readFileSync` 的调用方只读取 `error.message`；libuv 的 errno 文案必须逐字一致。 | 人工判断：确认是否仍有独立技术原因；没有则删除 |

## 完成后返回

返回本组改写的条目数、删除的条目数、保留的条目数，以及任何需要人工确认的 D 类条目。
