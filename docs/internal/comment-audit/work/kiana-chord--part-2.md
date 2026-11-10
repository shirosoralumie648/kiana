# 注释清理工作单：kiana-chord--part-2

- 待处理条目：19
- 涉及文件：15

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

- `crates/kiana-chord/src/contract/coverage.rs`
- `crates/kiana-chord/src/contract/mod.rs`
- `crates/kiana-chord/src/delta/pending.rs`
- `crates/kiana-chord/src/json.rs`
- `crates/kiana-chord/src/lib.rs`
- `crates/kiana-chord/src/sqlite/mod.rs`
- `crates/kiana-chord/src/sqlite/node.rs`
- `crates/kiana-chord/src/sqlite/schema.rs`
- `crates/kiana-chord/src/telemetry_sqlite/store.rs`
- `crates/kiana-chord/tests/context.rs`
- `crates/kiana-chord/tests/contract.rs`
- `crates/kiana-chord/tests/delta_ops.rs`
- `crates/kiana-chord/tests/schema.rs`
- `crates/kiana-chord/tests/sqlite_node.rs`
- `crates/kiana-chord/tests/types.rs`

## 待处理条目

| # | 位置 | 类别 | 命中 | 摘要 | 建议 |
|---:|---|---|---|---|---|
| 1 | `crates/kiana-chord/src/sqlite/node.rs:458` | A | 与TS一致 | // 结算规则与 TS 一致：成功先 COMMIT，失败先 ROLLBACK。回滚成功时保留原来的 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 2 | `crates/kiana-chord/src/sqlite/node.rs:46` | D | 保真 | //! # 与 `node:sqlite` 的保真点 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 3 | `crates/kiana-chord/src/sqlite/schema.rs:14` | A | 逐字一致 | //! 包装成自己的错误类型，消息与 TS 逐字一致。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 4 | `crates/kiana-chord/src/sqlite/schema.rs:19` | A | 逐字一致, 对应TS | /// 错误消息逐字对应 TS（前缀用 `JSON.stringify` 形式引用）。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 5 | `crates/kiana-chord/tests/contract.rs:1050` | A | 与TS一致 | /// 指纹由 `canonicalJson`（键排序）计算，因此与 TS 一致。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 6 | `crates/kiana-chord/tests/contract.rs:173` | D | 逐字一致 | // 夹具：description JSON 与指纹逐字一致。 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 7 | `crates/kiana-chord/tests/schema.rs:245` | A | 与TS一致 | // 失败路径：`invalid/input`，摘要文本与 TS 一致。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 8 | `crates/kiana-chord/tests/schema.rs:120` | D | 强约束 | // `raw_errors` 是同一趟遍历的未本地化版本：条数必须一致。 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 9 | `crates/kiana-chord/src/contract/coverage.rs:238` | D | 完全一致 | /// 1. fingerprint 相同 → `Ok(None)`：canonical JSON 完全一致（version 也相同）。 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 10 | `crates/kiana-chord/src/contract/mod.rs:57` | D | 逐字节一致 | //! 逐字节一致（`canonicalJson` 对键排序，数字格式与字符串转义由 `schema` 侧负责）。 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 11 | `crates/kiana-chord/src/delta/pending.rs:3` | A | 与TS一致, 完全一致 | //! 状态机与 TS 完全一致：`empty → batch → folding → replace`；`r` 丢弃此前的一切； | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 12 | `crates/kiana-chord/src/json.rs:28` | D | 保真 | //! 需要逐字节保真的地方是**键的顺序**：工作区启用了 `serde_json` 的 `preserve_order`， | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 13 | `crates/kiana-chord/src/lib.rs:105` | A | TS等价物, TS的 | //! 等价于 TS 的 `isFrame`；连接层尚未实现，所以“校验失败后如何断链”只写在 `wire` 的文档里。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 14 | `crates/kiana-chord/src/sqlite/mod.rs:19` | D | 保真 | //! 4. `node`：`rusqlite` 实现，含串行队列、事务、取消与 `node:sqlite` 的错误/绑定保真； | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 15 | `crates/kiana-chord/src/telemetry_sqlite/store.rs:49` | D | 逐字一致 | //! `"1"`）。SQL 的逐字文本（包括 `entry.type IN ('integer', 'real')` 中逗号后的空格） | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 16 | `crates/kiana-chord/tests/context.rs:491` | D | 逐字一致 | // `String(error)`：DOMException 分支逐字一致；`Error` 分支在 JS 里是 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 17 | `crates/kiana-chord/tests/delta_ops.rs:6` | A | 逐字一致 | //! 覆盖，错误消息与 TS 逐字一致。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 18 | `crates/kiana-chord/tests/sqlite_node.rs:37` | A | TS等价物, TS里 | /// 让出调度器，等价于 TS 里 `await Promise.resolve()` 给其他任务一次机会。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 19 | `crates/kiana-chord/tests/types.rs:153` | A | 与TS一致 | // `RefStatus` 的字符串与 TS 一致。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |

## 完成后返回

返回本组改写的条目数、删除的条目数、保留的条目数，以及任何需要人工确认的 D 类条目。
