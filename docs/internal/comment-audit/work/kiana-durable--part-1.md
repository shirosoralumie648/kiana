# 注释清理工作单：kiana-durable--part-1

- 待处理条目：40
- 涉及文件：6

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

- `crates/kiana-durable/src/env/node.rs`
- `crates/kiana-durable/src/harness/generation.rs`
- `crates/kiana-durable/src/harness/live.rs`
- `crates/kiana-durable/src/storage/memory.rs`
- `crates/kiana-durable/src/storage/sqlite/storage.rs`
- `crates/kiana-durable/src/types.rs`

## 待处理条目

| # | 位置 | 类别 | 命中 | 摘要 | 建议 |
|---:|---|---|---|---|---|
| 1 | `crates/kiana-durable/src/storage/memory.rs:323` | A | 与TS一致, 完全一致 | /// 其余索引按 ID 有序的 `Vec` 保存，与 TS 完全一致。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 2 | `crates/kiana-durable/src/storage/memory.rs:617` | A | 必须与TS, 与TS一致 | /// （`usize` 的 0 是合法入参，行为必须与 TS 一致，不能静默返回空页）。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 3 | `crates/kiana-durable/src/storage/memory.rs:804` | A | 与TS一致 | /// `seq` 为 `None` 时默认取 `seqFromNumber(nextSeq)`。校验顺序与 TS 一致，因此多种错误 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 4 | `crates/kiana-durable/src/storage/memory.rs:814` | A | 逐字一致 | /// 文案与 TS 逐字一致。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 5 | `crates/kiana-durable/src/storage/memory.rs:911` | A | 与TS一致 | // 边校验边应用，保持错误优先级与 TS 一致（Rust 的 `apply_immutable_batches` 就是 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 6 | `crates/kiana-durable/src/storage/memory.rs:2171` | A | 逐字节一致, TS的 | // 1. `scope_key` / `address_key` 与 TS 的 `JSON.stringify` 逐字节一致； | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 7 | `crates/kiana-durable/src/storage/memory.rs:2182` | A | 逐字节一致, TS的 | /// `scopeKey` / `addressKey` 与 TS 的 `JSON.stringify` 逐字节一致（spec §2.12 要求）。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 8 | `crates/kiana-durable/src/storage/memory.rs:89` | D | 逐字节一致 | //! - 地址键经 `kiana_json::js_object::stringify` 生成，必须与 `JSON.stringify` 逐字节一致 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 9 | `crates/kiana-durable/src/storage/memory.rs:1092` | D | 完全一致 | /// `kind`/`key`/`history`/`fork` 完全一致； | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 10 | `crates/kiana-durable/src/harness/generation.rs:3` | A | 逐字一致, 对应TS | //! 任务阶段逐字对应 TS：`prepare` 渲染系统提示与工具装载、决定阈值压缩并进入 `request`； | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 11 | `crates/kiana-durable/src/harness/generation.rs:660` | A | 与TS一致 | /// 与 TS 一致的顺序是： | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 12 | `crates/kiana-durable/src/harness/generation.rs:934` | A | 逐字一致, 对应TS | /// `Failed`/`Faulted` 取错误消息，`Orphaned` 取原因，`Aborted` 用固定文本（逐字对应 TS）。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 13 | `crates/kiana-durable/src/harness/generation.rs:966` | A | 与TS一致 | /// 交给 `classify` 的 `Request.messages` 是**钩子之前**的 `view.messages`（与 TS 一致）：工具轮 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 14 | `crates/kiana-durable/src/harness/generation.rs:1057` | A | TS等价物 | // off 时不设置 reasoning（等价于 TS 不展开该字段）。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 15 | `crates/kiana-durable/src/harness/generation.rs:1466` | A | 逐字一致, 对应TS | // 失败文本优先用 provider 的 errorMessage；没有时用 stop reason 拼出（逐字对应 TS 模板）。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 16 | `crates/kiana-durable/src/harness/generation.rs:2284` | A | 与TS一致 | /// `detail` 是 `{ "reason": "no_model" }`（与 TS 一致）。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 17 | `crates/kiana-durable/src/harness/generation.rs:2595` | A | 必须与TS, 与TS一致 | /// 用于失败文本 `Model response ended with stop reason ...`，字符串必须与 TS 一致 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 18 | `crates/kiana-durable/src/types.rs:10` | A | 与TS一致 | //! `DocumentCommitChange.type`）→ 内部 tag 的 enum，serde 先写 tag，键序与 TS 一致； | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 19 | `crates/kiana-durable/src/types.rs:512` | A | 与TS一致 | /// 其他值只可能来自类型违约的调用，行为与 TS 一致（进入地址、进入 `addressId`）。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 20 | `crates/kiana-durable/src/types.rs:655` | A | 完全一致, TS的 | /// 保证与 TS 的 `Op[]` 完全一致。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 21 | `crates/kiana-durable/src/types.rs:1068` | A | 必须与TS, TS的, TS里 | /// 条目是持久化 JSON，键序必须与 TS 的构造序一致。`kiana.tool-result` 的消息在 TS 里由 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 22 | `crates/kiana-durable/src/types.rs:373` | D | 逐字节一致 | /// 把一条记录写成与 `JSON.stringify` 逐字节一致的文本。 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 23 | `crates/kiana-durable/src/types.rs:2620` | D | 逐字一致 | /// 或运行期 `String`；空串同样 `panic!`，消息逐字相同。 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 24 | `crates/kiana-durable/src/types.rs:2917` | D | 逐字一致 | // 逐字对应 `const key = definition.family === true ? (args[index++] as string) : undefined`： | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 25 | `crates/kiana-durable/src/types.rs:2944` | D | 逐字一致 | /// 得到逐字相同的字符串。 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 26 | `crates/kiana-durable/src/env/node.rs:15` | A | TS等价物, TS的 | //! 逐块 `write_all`；写入即背压（await 期间不读管道，等价于 TS 的 `pause()`）。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 27 | `crates/kiana-durable/src/env/node.rs:354` | A | 与TS一致 | /// 展开是有条件的，顺序也与 TS 一致： | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 28 | `crates/kiana-durable/src/env/node.rs:650` | A | 必须与TS, 逐字一致 | /// `unknown`）；错误文案必须与 TS 逐字一致，测试只断言其中的 "Invalid timeout"。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 29 | `crates/kiana-durable/src/env/node.rs:904` | A | 与TS一致 | /// 探测顺序（与 TS 一致）： | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 30 | `crates/kiana-durable/src/env/node.rs:1911` | A | 逐字一致 | // spill 判定与 TS 逐字一致：按**累计**字节数与“含未终结末行” | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 31 | `crates/kiana-durable/src/env/node.rs:2690` | A | 与TS一致 | // 与 TS 一致：先用固定的 `tmp-` 前缀建一个唯一目录，再在目录里建文件。所以 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 32 | `crates/kiana-durable/src/storage/sqlite/storage.rs:305` | A | 必须与TS, 与TS一致 | /// （`usize` 的 0 是合法入参，行为必须与 TS 一致，不能静默返回空页）。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 33 | `crates/kiana-durable/src/storage/sqlite/storage.rs:381` | A | 逐字节一致 | /// 都存 JSON 编码（字符串值带引号），这样与 TS 写出的库逐字节一致。`family = 0` 时 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 34 | `crates/kiana-durable/src/storage/sqlite/storage.rs:1074` | A | TS等价物, TS的 | /// `admitRead` 的计数器守卫：`Drop` 时递减（等价于 TS 的 `finally`）。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 35 | `crates/kiana-durable/src/storage/sqlite/storage.rs:2273` | A | 与TS一致 | /// `order` 也不会被读取——`DocumentQuery` 本身没有顺序字段，与 TS 一致。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 36 | `crates/kiana-durable/src/storage/sqlite/storage.rs:228` | D | 逐字一致 | /// 把 `error` 记进槽并转成 [`SqliteError`]（消息逐字保留）。 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 37 | `crates/kiana-durable/src/storage/sqlite/storage.rs:1457` | D | 完全一致 | /// 作用域，且 kind/key/history/fork 与目标记录完全一致，否则把失败包成 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 38 | `crates/kiana-durable/src/harness/live.rs:26` | A | 与TS一致 | //! - 键名与 TS 一致，全部 camelCase；结构体的字段声明顺序就是序列化键序（测试 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 39 | `crates/kiana-durable/src/harness/live.rs:522` | A | 逐字一致 | /// 按 UTF-16 code unit 计，非 BMP 字符占 2 个。这里的计数被两处以"与 TS 逐字一致"为前提 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 40 | `crates/kiana-durable/src/harness/live.rs:525` | D | 完全一致 | /// 会把落单的代理项替换成 U+FFFD，而 JS 的 `slice` 保留孤立代理项，两者并不完全一致， | 人工判断：确认是否仍有独立技术原因；没有则删除 |

## 完成后返回

返回本组改写的条目数、删除的条目数、保留的条目数，以及任何需要人工确认的 D 类条目。
