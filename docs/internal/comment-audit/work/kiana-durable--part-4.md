# 注释清理工作单：kiana-durable--part-4

- 待处理条目：4
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

- `crates/kiana-durable/tests/session_watches.rs`
- `crates/kiana-durable/tests/sqlite_facade.rs`
- `crates/kiana-durable/tests/sqlite_storage.rs`
- `crates/kiana-durable/tests/types.rs`

## 待处理条目

| # | 位置 | 类别 | 命中 | 摘要 | 建议 |
|---:|---|---|---|---|---|
| 1 | `crates/kiana-durable/tests/session_watches.rs:331` | A | 与TS一致 | // `started` 一旦置位就不再复位，停止后再 start 仍是 "already started"（与 TS 一致）。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 2 | `crates/kiana-durable/tests/sqlite_facade.rs:365` | A | TS等价物, TS的 | // 驱动到被门控的查询：每次轮询后让出调度器（等价于 TS 的微任务让出）。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 3 | `crates/kiana-durable/tests/sqlite_storage.rs:945` | A | 必须与TS, 与TS一致 | /// 的短形式/`#` 引用。存出的字节必须与 TS 一致，否则 TS 读 Rust 写的库会按 `assertValidOp` | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 4 | `crates/kiana-durable/tests/types.rs:1137` | A | 与TS一致 | // 带类型的家族入口仍只接受字符串 key，地址身份与 TS 一致。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |

## 完成后返回

返回本组改写的条目数、删除的条目数、保留的条目数，以及任何需要人工确认的 D 类条目。
