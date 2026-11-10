# 注释清理工作单：kiana-durable--part-2

- 待处理条目：40
- 涉及文件：14

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

- `crates/kiana-durable/src/harness/compaction.rs`
- `crates/kiana-durable/src/harness/events.rs`
- `crates/kiana-durable/src/harness/prompt.rs`
- `crates/kiana-durable/src/harness/view.rs`
- `crates/kiana-durable/src/session/session.rs`
- `crates/kiana-durable/src/session/transaction.rs`
- `crates/kiana-durable/src/storage/jsonl/storage.rs`
- `crates/kiana-durable/src/storage/scan.rs`
- `crates/kiana-durable/src/storage/sqlite/migrations.rs`
- `crates/kiana-durable/src/testing/env_conformance.rs`
- `crates/kiana-durable/src/testing/storage_conformance.rs`
- `crates/kiana-durable/src/tools/edit.rs`
- `crates/kiana-durable/src/tools/edit_diff.rs`
- `crates/kiana-durable/tests/tools_images.rs`

## 待处理条目

| # | 位置 | 类别 | 命中 | 摘要 | 建议 |
|---:|---|---|---|---|---|
| 1 | `crates/kiana-durable/src/harness/events.rs:39` | A | 必须与TS, TS的 | //! - 序列化必须与 TS 的 `JSON.stringify` 对齐：含 `f64` 的载荷走 `serialize_js_json` 或 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 2 | `crates/kiana-durable/src/harness/events.rs:86` | A | 与TS一致 | /// `contentIndex` 与 TS 一致是数组下标；`block` 是 kiana-ai 的内容块。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 3 | `crates/kiana-durable/src/harness/events.rs:261` | A | 逐字节一致 | /// `serde_json::to_string` 还是先 `to_value`，都得到同一份与 TS 逐字节一致的文本。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 4 | `crates/kiana-durable/src/harness/events.rs:1560` | A | 与TS一致 | // 是否换 run 只看首个输入（与 TS 一致）：同一 run 追加输入不算换 run。换 run 时先结束 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 5 | `crates/kiana-durable/src/session/session.rs:8` | A | TS等价物, TS的 | //! 交给运行时（等价于 TS 的 `#tail.then(job)` 立即排入 microtask），因此并发创建的提交按 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 6 | `crates/kiana-durable/src/session/session.rs:681` | A | 与TS一致, 完全一致 | /// - 传入 [`ContextRef`]（或它的引用）时保留同一对象，行为与 TS 完全一致； | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 7 | `crates/kiana-durable/src/session/session.rs:1408` | A | 逐字一致 | /// TS `TypeError` 的等价物（消息逐字相同）。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 8 | `crates/kiana-durable/src/session/session.rs:3482` | A | TS等价物, TS的 | /// 运行期间到达的帧只更新 `newest`，循环结束后补一次排程。监听器 panic 等价于 TS 的 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 9 | `crates/kiana-durable/src/storage/jsonl/storage.rs:174` | A | 逐字一致, 对应TS | /// 缺失，以及重放时被 `MemoryStorage` 拒绝的"语义非法"状态。错误消息逐字对应 TS，调用方 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 10 | `crates/kiana-durable/src/storage/jsonl/storage.rs:510` | A | 逐字一致, 对应TS | /// `description` 形如 `main.jsonl line N`，会拼进每条错误消息；文案逐字对应 TS。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 11 | `crates/kiana-durable/src/storage/jsonl/storage.rs:9` | D | 逐字一致 | //! 布局都逐字保留（§4.1）。JSON 一律经 [`kiana_json::js_object::stringify`] 写出，与 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 12 | `crates/kiana-durable/src/storage/jsonl/storage.rs:10` | D | 逐字节一致 | //! `JSON.stringify` 逐字节一致（数字格式与整数键序）。 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 13 | `crates/kiana-durable/src/harness/prompt.rs:30` | D | 保真 | //! # 顺序保真（本模块的主要复杂度） | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 14 | `crates/kiana-durable/src/harness/prompt.rs:172` | D | 逐字一致 | /// 逐字一致）。同名 key 在 `sections` 里出现多次时，结果的位置取第一次、值取最后一次。 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 15 | `crates/kiana-durable/src/harness/prompt.rs:342` | D | 完全一致 | // 保留 = 名字在目标里且声明文本完全一致；声明变化的工具按「移除再添加」处理。 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 16 | `crates/kiana-durable/src/storage/scan.rs:32` | A | 必须与TS, 逐字一致 | //! - 错误文案必须与 TS 逐字一致：`Invalid storage cursor`、`Invalid scan order: ...` 与 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 17 | `crates/kiana-durable/src/storage/scan.rs:103` | A | 逐字一致, TS的 | /// `null` 写成 `null`、对象写成 `[object Object]`，与 TS 的模板字符串逐字一致。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 18 | `crates/kiana-durable/src/storage/scan.rs:181` | A | 与TS一致 | /// 校验顺序与 TS 一致：先校验 `requested`，再解析游标。因此“非法 `order` + 非法游标”同时 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 19 | `crates/kiana-durable/src/testing/storage_conformance.rs:108` | A | TS等价物 | /// [`start_read`] 在回调里立刻 poll 一次（等价于 TS 执行到第一个 `await`）：已完成的读取直接 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 20 | `crates/kiana-durable/src/testing/storage_conformance.rs:173` | A | 必须与TS, TS的 | /// 所以比较前在这里显式组装；字段名必须与 TS 的 `StoredDocument` 保持一致。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 21 | `crates/kiana-durable/src/testing/storage_conformance.rs:14` | D | 逐字一致 | //! （[`StorageConformanceOptions::assertions`]），跑的就是逐字相同的 26 个用例。 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 22 | `crates/kiana-durable/src/tools/edit.rs:151` | A | 逐字一致 | // 手工校验输入而不是只依赖 schema：只有 `edits` 为空时的错误文案与 TS 逐字一致；`path` | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 23 | `crates/kiana-durable/src/tools/edit.rs:156` | A | 与TS一致 | // 注意 `edits` 必须是非空数组（与 TS 一致）：空数组得到 invalid 错误，而不是成功结果。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 24 | `crates/kiana-durable/src/tools/edit.rs:207` | A | 与TS一致 | // 把文件系统错误折成工具错误文案。文案只含路径与错误码名（与 TS 一致）；`FileError` 的 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 25 | `crates/kiana-durable/src/tools/edit_diff.rs:51` | A | 逐字一致, 对应TS | //! - **错误文案是模型可见输出**：`get_*_error` 的字符串逐字对应 TS，单个 edit 与多个 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 26 | `crates/kiana-durable/src/tools/edit_diff.rs:128` | A | 与TS一致 | /// 顺序与 TS 一致：先 NFKC（把全角/兼容字符折叠成常规形式），再逐行去掉行尾空白， | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 27 | `crates/kiana-durable/src/tools/edit_diff.rs:525` | A | 逐字一致, TS的, 对应TS | // 下面四个函数生成逐字对应 TS 的错误消息。这些字符串是模型在工具结果里看到的文本： | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 28 | `crates/kiana-durable/tests/tools_images.rs:1210` | A | 必须与TS, TS的 | /// JPEG 输入（含 EXIF 方向、缩放、字节限额）与 PNG→JPEG 重编码必须与 TS 的 photon wasm | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 29 | `crates/kiana-durable/tests/tools_images.rs:1272` | A | 完全一致 | /// 这样重建的输入与 TS 参考实现生成的字节完全一致。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 30 | `crates/kiana-durable/tests/tools_images.rs:1211` | D | 逐字节一致 | /// 构建**逐字节一致**：解码用的是同一份 `image` 0.24 和 wasm 构建里的 `jpeg-decoder` 0.3.1 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 31 | `crates/kiana-durable/src/harness/compaction.rs:708` | A | 逐字一致, TS的, 对应TS | // `error_message` 缺失时的兜底文本：逐字对应 TS 的 stopReason 名（`toolUse` 为 camelCase）。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 32 | `crates/kiana-durable/src/harness/compaction.rs:835` | D | 完全一致 | /// 替换成 U+FFFD，而 JS 的 `slice` 会保留孤立代理项，这是两种运行时无法完全一致的边界。 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 33 | `crates/kiana-durable/src/harness/view.rs:384` | A | TS等价物, TS里 | // 自己的取消来源）；监听器 panic 等价于 TS 里的抛错——只结束这一个 watch， | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 34 | `crates/kiana-durable/src/harness/view.rs:558` | A | 与TS一致 | /// `start_listener` 直接 panic（与 TS 一致），不会静默忽略。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 35 | `crates/kiana-durable/src/session/transaction.rs:2785` | A | 逐字一致 | /// TS `TypeError` 的等价物（消息逐字相同）。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 36 | `crates/kiana-durable/src/session/transaction.rs:1801` | D | 强约束 | // 阶段 3（已存在）：定义语义与持久记录必须一致；持久版本高于定义或需要迁移却没有 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 37 | `crates/kiana-durable/src/storage/sqlite/migrations.rs:4` | A | 逐字一致 | //! CHECK 约束）。语句文本与 TS 模板字面量逐字一致（缩进是制表符），因为 `sqlite_schema.sql` | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 38 | `crates/kiana-durable/src/storage/sqlite/migrations.rs:230` | D | 逐字一致 | // 这五个定义与版本 1 中 `tasks` 的索引逐字相同（名称、列序、顺序都一致）， | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 39 | `crates/kiana-durable/src/testing/env_conformance.rs:269` | A | 与TS一致 | /// 这里按该默认实现的特征识别“未实现”，让失败消息与 TS 一致（ARCHITECTURE §4.9 冻结了 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 40 | `crates/kiana-durable/src/testing/env_conformance.rs:547` | A | 与TS一致 | /// `list_files`（默认 false，与 TS 一致；`FileSystem::list_files` 是带 `not_supported` | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |

## 完成后返回

返回本组改写的条目数、删除的条目数、保留的条目数，以及任何需要人工确认的 D 类条目。
