# 注释清理工作单：kiana-compaction

- 待处理条目：15
- 涉及文件：5

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

- `crates/kiana-compaction/src/branch_summarization.rs`
- `crates/kiana-compaction/src/compaction.rs`
- `crates/kiana-compaction/src/utils.rs`
- `crates/kiana-compaction/tests/branch_summarization.rs`
- `crates/kiana-compaction/tests/malformed_sessions.rs`

## 待处理条目

| # | 位置 | 类别 | 命中 | 摘要 | 建议 |
|---:|---|---|---|---|---|
| 1 | `crates/kiana-compaction/src/branch_summarization.rs:46` | A | 必须与TS, 逐字一致 | //! `BRANCH_SUMMARY_PREAMBLE` / `BRANCH_SUMMARY_PROMPT` 必须与 TS 逐字一致（UTF-16 长度 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 2 | `crates/kiana-compaction/src/branch_summarization.rs:95` | A | 逐字一致, TS的, 对应TS | /// 失败文本（逐字对应 TS 的 `error.message`）。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 3 | `crates/kiana-compaction/src/branch_summarization.rs:120` | A | 与TS一致 | /// 消息的估算 token 总数（JS `number`；畸形摘要条目会是 `NaN`，与 TS 一致）。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 4 | `crates/kiana-compaction/src/branch_summarization.rs:413` | A | 与TS一致 | /// toolResult 上记录的 `nestedCalls` 文件操作不会进入分支摘要的 `file_ops`（与 TS 一致）。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 5 | `crates/kiana-compaction/src/branch_summarization.rs:460` | A | 与TS一致 | // 因此预算失效、更老的条目全部保留（与 TS 一致）。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 6 | `crates/kiana-compaction/src/branch_summarization.rs:582` | A | 与TS一致 | // 与 TS 一致：只设置 apiKey/headers/env/signal，不设置 reasoning、sessionId 或 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 7 | `crates/kiana-compaction/src/branch_summarization.rs:607` | A | 与TS一致 | // 分流顺序与 TS 一致：aborted 优先，其次 error/length，最后才是 toolCall。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 8 | `crates/kiana-compaction/src/compaction.rs:36` | A | 逐字一致, TS的 | //! - prompt 常量与 TS 逐字一致，由文件底部测试锁定长度；改动会破坏与 TS 的行为一致性。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 9 | `crates/kiana-compaction/src/compaction.rs:201` | A | 与TS一致 | /// `content: null`），使投影、切点与 token 估算与 TS 一致。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 10 | `crates/kiana-compaction/src/compaction.rs:1693` | A | 必须与TS, 逐字一致 | /// （`label = "Branch summarization"`），所以错误文本必须与 TS 逐字一致。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 11 | `crates/kiana-compaction/src/utils.rs:296` | A | 与TS一致 | /// # 扫描规则（与 TS 一致） | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 12 | `crates/kiana-compaction/tests/branch_summarization.rs:895` | A | 逐字一致 | /// 空摘要的 `Unknown` `branch_summary` 仍出现在分支摘要 prompt 中（与 TS 逐字一致）。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 13 | `crates/kiana-compaction/tests/branch_summarization.rs:997` | A | 与TS一致 | // 消息放不进预算，但文件操作在预算检查之前提取（与 TS 一致）。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 14 | `crates/kiana-compaction/tests/malformed_sessions.rs:2` | A | 与TS一致 | //! `null`/`undefined`/`NaN` 的形式影响压缩逻辑。这里锁定与 TS 一致的边界行为。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 15 | `crates/kiana-compaction/src/branch_summarization.rs:44` | D | 保真 | //! # 保真约束 | 人工判断：确认是否仍有独立技术原因；没有则删除 |

## 完成后返回

返回本组改写的条目数、删除的条目数、保留的条目数，以及任何需要人工确认的 D 类条目。
