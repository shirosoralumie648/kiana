# 注释清理工作单：kiana-chord--part-1

- 待处理条目：40
- 涉及文件：11

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

- `crates/kiana-chord/src/context/index.rs`
- `crates/kiana-chord/src/contract/describe.rs`
- `crates/kiana-chord/src/delta/diff.rs`
- `crates/kiana-chord/src/delta/index.rs`
- `crates/kiana-chord/src/delta/mod.rs`
- `crates/kiana-chord/src/delta/revision_validator.rs`
- `crates/kiana-chord/src/errors.rs`
- `crates/kiana-chord/src/output.rs`
- `crates/kiana-chord/src/schema.rs`
- `crates/kiana-chord/src/wire.rs`
- `crates/kiana-chord/tests/output_skip.rs`

## 待处理条目

| # | 位置 | 类别 | 命中 | 摘要 | 建议 |
|---:|---|---|---|---|---|
| 1 | `crates/kiana-chord/src/context/index.rs:18` | A | 与TS一致, 完全一致 | //! [`await_handle_with_context`]（等 `JoinHandle`；放弃等待时任务继续跑，与 TS 完全一致）。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 2 | `crates/kiana-chord/src/context/index.rs:1088` | A | 与TS一致 | /// 回调返回的 `T` 原样成为结果（与 TS 一致）；telemetry 层错误以 `Err` 返回。**没有** | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 3 | `crates/kiana-chord/src/context/index.rs:1118` | A | TS等价物 | /// [`trace_in`] 的"异常通道"版本：回调返回 `Result`，`Err` 等价于 TS 回调 `throw` | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 4 | `crates/kiana-chord/src/context/index.rs:1289` | A | 与TS一致 | /// kiana-telemetry 与 TS 一致：父 span 结算后再起的子 span 是惰性的（不记录），所以测试必须 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 5 | `crates/kiana-chord/src/context/index.rs:550` | D | 强约束 | /// - 实现必须不可变：`&self` 上的查询不得改变可观察状态，因为同一个 [`ContextRef`] 会被 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 6 | `crates/kiana-chord/src/context/index.rs:1031` | D | 完全一致 | /// 语义完全一致）。 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 7 | `crates/kiana-chord/src/delta/revision_validator.rs:28` | A | 逐字一致 | //! 检查（普通对象/稠密数组/symbol/访问器），保留它们是为了与 TS 逐字一致。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 8 | `crates/kiana-chord/src/delta/revision_validator.rs:36` | A | 逐字一致, TS的, 对应TS | /// 校验错误：消息逐字对应 TS 的 `TypeError`。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 9 | `crates/kiana-chord/src/delta/revision_validator.rs:62` | A | 逐字一致 | /// 各条固定文案（与 TS 逐字一致）。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 10 | `crates/kiana-chord/src/delta/revision_validator.rs:111` | A | 逐字一致, TS的 | /// TS 的 `TypeError` 逐字一致。每次调用都完整遍历，不复用上一次的校验结果。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 11 | `crates/kiana-chord/src/delta/revision_validator.rs:196` | A | 与TS一致 | /// 只关心是否通过、丢弃校验结果；错误没有路径/位置信息，只有与 TS 一致的固定文案。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 12 | `crates/kiana-chord/src/wire.rs:3` | A | 与TS一致, 完全一致 | //! 帧是 JSON 对象，`t` 为标签。Rust 用 `#[serde(tag = "t")]` 的枚举；字段名与 TS 完全一致 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 13 | `crates/kiana-chord/src/wire.rs:6` | A | 逐字一致 | //! ## 23 个标签与方向（TS 注释逐字保留） | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 14 | `crates/kiana-chord/src/wire.rs:649` | A | TS等价物, TS的 | /// 整值载荷（用于 `snap`/`reset`）；缺省等价于 TS 的 `undefined`，显式 `null` 保留。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 15 | `crates/kiana-chord/src/wire.rs:1030` | A | 逐字一致, TS的, 对应TS | /// 逐字对应 TS 的 `/^[a-z_]+(\/[a-z_]+)?: /`。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 16 | `crates/kiana-chord/src/wire.rs:961` | D | 保真 | /// 的——它唯一的用途是喂给 `schema::check`，逐字节保真比可读性重要。结构上，顶层 `anyOf` 的 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 17 | `crates/kiana-chord/src/delta/diff.rs:134` | A | 必须与TS, 与TS一致 | /// 因此顺序必须与 TS 一致。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 18 | `crates/kiana-chord/src/delta/diff.rs:172` | A | 与TS一致 | /// （只可能出现在 `before` 与 `after` 是同一棵树的情况）地址相同，同样与 TS 一致。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 19 | `crates/kiana-chord/src/delta/diff.rs:818` | A | 与TS一致 | /// `assert_safe_path`）。这是与 TS 一致的保守退化。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 20 | `crates/kiana-chord/src/delta/diff.rs:1087` | A | 强约束 | // 等值对象互不相同。TS 对独立解析输入发出整段 splice，Rust 必须一致。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 21 | `crates/kiana-chord/src/delta/index.rs:4` | A | 逐字一致 | //! `UnsafePathError`）逐字保留，统一由 [`OpError`] 承载（`Display` = TS `error.message`， | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 22 | `crates/kiana-chord/src/delta/index.rs:571` | A | 与TS一致 | /// `"1e+21"`），因此保留为 [`Seg::Key`]：对象写入/读取与 TS 一致；数组父容器随后按 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 23 | `crates/kiana-chord/src/delta/index.rs:668` | A | 与TS一致 | /// 错误选择与 TS 一致：元组不是非空数组 → `op is not a tuple`；首元素不是字符串 → | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 24 | `crates/kiana-chord/src/delta/index.rs:1205` | A | 与TS一致 | /// `PathError` 失败，与 TS 一致。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 25 | `crates/kiana-chord/src/output.rs:428` | A | 逐字一致, TS的, 对应TS | /// `push` 的契约错误；消息逐字对应 TS 的 `Error`。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 26 | `crates/kiana-chord/src/output.rs:436` | A | 逐字一致 | /// 与 TS `Error.message` 逐字一致，测试会直接比较字符串。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 27 | `crates/kiana-chord/src/output.rs:443` | A | 逐字一致, TS的 | /// 这段文本是契约的一部分：`push` 的错误消息与 TS 的 `new Error(...)` 逐字相同， | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 28 | `crates/kiana-chord/src/output.rs:50` | D | 强约束 | //! 一字节或一行的后缀，head 合并成一块），而压缩不得改变窗口内容。 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 29 | `crates/kiana-chord/src/schema.rs:10` | A | 与TS一致 | //! 经 [`strip_live_markers`] 剥除；直接来自 JSON 的 schema 没有标记，行为与 TS 一致。`check` | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 30 | `crates/kiana-chord/src/schema.rs:36` | A | 与TS一致 | //! `live=false`，行为与 TS 一致）。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 31 | `crates/kiana-chord/src/schema.rs:3931` | A | 与TS一致 | /// 使 `Object.keys(schema)` 顺序与 TS 一致（也决定 `Default` 遍历 `properties` 的顺序）。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 32 | `crates/kiana-chord/tests/output_skip.rs:4` | A | 逐位一致 | //! 随时发生且不改变窗口"的模糊用例。确定性 PRNG（mulberry32）与 TS 参考实现逐位一致。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 33 | `crates/kiana-chord/tests/output_skip.rs:67` | D | 强约束 | /// 两个缓冲看到同样的输出时快照必须相同。 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 34 | `crates/kiana-chord/tests/output_skip.rs:144` | D | 强约束 | // 进度提交会在任意时刻快照；快照压缩存储，但绝不能改变窗口。 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 35 | `crates/kiana-chord/src/contract/describe.rs:26` | A | 必须与TS, TS的 | //! 字段顺序、缺省键、额外字段都必须与 TS 的对象字面量一致。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 36 | `crates/kiana-chord/src/contract/describe.rs:499` | A | 与TS一致 | /// `Value` 原样放进 `{kind:"state",schema}`。这与 TS 一致，但意味着这样造出的描述可能通不过 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 37 | `crates/kiana-chord/src/delta/mod.rs:10` | A | 逐字一致, 对应TS | //! 本文件只给出被跨文件引用的那三个类型。`Op` 的变体与 tuple 编码逐字对应 TS： | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 38 | `crates/kiana-chord/src/delta/mod.rs:77` | A | 与TS一致 | /// 非负整数保留为 `Seg::Key`，对象写入仍与 TS 一致，但数组父容器会按“非数字段”报 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 39 | `crates/kiana-chord/src/errors.rs:186` | A | 逐字一致, TS的 | /// Rust 版本没有缺省参数：要与 TS 的缺省行为逐字一致，传 `"forbidden"`。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 40 | `crates/kiana-chord/src/errors.rs:7` | D | 逐字一致 | //! `error.message` 逐字一致），原始消息在 [`ChordError::message`]。 | 人工判断：确认是否仍有独立技术原因；没有则删除 |

## 完成后返回

返回本组改写的条目数、删除的条目数、保留的条目数，以及任何需要人工确认的 D 类条目。
