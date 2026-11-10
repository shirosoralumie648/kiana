# 注释清理工作单：kiana-evals

- 待处理条目：29
- 涉及文件：8

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

- `crates/kiana-evals/src/cli.rs`
- `crates/kiana-evals/src/docker.rs`
- `crates/kiana-evals/src/harness.rs`
- `crates/kiana-evals/src/lib.rs`
- `crates/kiana-evals/src/plan.rs`
- `crates/kiana-evals/src/report.rs`
- `crates/kiana-evals/tests/support/acme_server.rs`
- `crates/kiana-evals/tests/support/configured_runtime.rs`

## 待处理条目

| # | 位置 | 类别 | 命中 | 摘要 | 建议 |
|---:|---|---|---|---|---|
| 1 | `crates/kiana-evals/src/cli.rs:10` | A | 逐字一致 | //! - 参数解析的顺序与错误文本逐字一致（`.docs.eval.ts` 检查最先，其次 `-t` / | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 2 | `crates/kiana-evals/src/cli.rs:153` | A | 与TS一致 | /// 五条规则的判定顺序与 TS 一致；其中第 1 条（`.docs.eval.ts` 后缀）必须最先判定，否则 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 3 | `crates/kiana-evals/src/cli.rs:155` | A | 逐字一致 | /// 但仍保持同样次序，以便错误文本与 TS 逐字一致。除通过 `environment` 闭包读环境外 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 4 | `crates/kiana-evals/src/cli.rs:626` | A | TS等价物, TS的 | /// evals CLI 入口：等价于 TS 的模块顶层执行。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 5 | `crates/kiana-evals/src/docker.rs:14` | A | 逐字一致 | //! - 所有错误文本与 TS 逐字一致（见各函数文档）。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 6 | `crates/kiana-evals/src/docker.rs:638` | A | 与TS一致 | /// 判定顺序（每一步失败都直接返回，文本与 TS 一致）： | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 7 | `crates/kiana-evals/src/harness.rs:116` | A | 与TS一致 | /// Rust 侧直接用 `serde_json::Value`（`preserve_order` 已开启，键序与 TS 一致）。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 8 | `crates/kiana-evals/src/harness.rs:131` | A | TS等价物, TS里 | /// `Display` 只输出 `message`（等价于 TS 里把错误转成字符串、或读取 `error.message`）； | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 9 | `crates/kiana-evals/src/harness.rs:454` | A | 必须与TS, TS的 | /// 组成的数组。失败 run 的合成 transcript 用这个值，因此形状必须与 TS 的 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 10 | `crates/kiana-evals/src/harness.rs:2569` | A | 与TS一致 | // 5) 合并运行错误与 cleanup 错误（规则与 TS 一致）： | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 11 | `crates/kiana-evals/src/harness.rs:2748` | A | 与TS一致 | /// 规则与顺序（与 TS 一致）： | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 12 | `crates/kiana-evals/src/harness.rs:2787` | A | 逐字一致 | // camelCase 别名（与 TS 导出名逐字对应） | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 13 | `crates/kiana-evals/src/lib.rs:17` | A | 逐字一致 | //! 入口来构建镜像、跑用例并生成可比较的报告。各模块文档把「与 TS 逐字一致」列为硬约束： | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 14 | `crates/kiana-evals/src/plan.rs:4` | A | 逐字一致, 参照/母本 | //! 逐行移植自 TS；错误文本、字段顺序与用例顺序都必须逐字一致（`expected-runs.json` | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 15 | `crates/kiana-evals/src/plan.rs:136` | A | 逐字一致 | /// 消息文本与 TS 逐字一致；上层 CLI 只打印该文本。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 16 | `crates/kiana-evals/src/report.rs:1986` | A | 与TS一致 | // 变体的 serde 表示与 `as_str` 相同（snake_case），因此这里的字节与 TS 一致。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 17 | `crates/kiana-evals/src/cli.rs:9` | D | 保真 | //! 保真点： | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 18 | `crates/kiana-evals/src/cli.rs:460` | D | 强约束 | /// 两个变体的发现结果必须一致，比较 `JSON.stringify([fullName, normalizedFile])` 的排序数组。 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 19 | `crates/kiana-evals/src/cli.rs:465` | D | 强约束 | /// 的是 `Vec` 而不是集合，因此条数也必须一致。 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 20 | `crates/kiana-evals/src/docker.rs:5` | D | 保真 | //! 保真点： | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 21 | `crates/kiana-evals/src/docker.rs:9` | D | 逐字一致 | //! 逐字保留； | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 22 | `crates/kiana-evals/src/harness.rs:1791` | D | 逐字一致 | /// 三个错误文本逐字保留。 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 23 | `crates/kiana-evals/src/lib.rs:14` | D | 逐字一致 | //! # 为什么存在，以及为什么强调逐字一致 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 24 | `crates/kiana-evals/src/report.rs:7` | D | 保真 | //! 保真点： | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 25 | `crates/kiana-evals/src/report.rs:34` | D | 保真 | //! - **字节级保真**：`tests/comparison.rs` 逐字比较 `report.txt`（黄金文件 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 26 | `crates/kiana-evals/src/report.rs:209` | D | 逐字一致 | /// 与 `JSON.stringify` 逐字一致；`Deserialize` 按 `f64` 读取（不做有限性/范围校验）。 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 27 | `crates/kiana-evals/src/report.rs:243` | D | 逐字一致 | /// `usage.metadata` 里的 token 数/费用是 JS number，必须与 `JSON.stringify` 逐字一致。 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 28 | `crates/kiana-evals/tests/support/acme_server.rs:4` | D | 逐字一致 | //! 保留为 kiana-evals 内的 test fixture。这里逐字保留 fixture 常量、请求校验顺序、状态码、 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 29 | `crates/kiana-evals/tests/support/configured_runtime.rs:4` | D | 逐字一致 | //! 内的 test fixture。这里逐字保留 `ModelRuntime.create` 的三个路径参数、错误作为数据 | 人工判断：确认是否仍有独立技术原因；没有则删除 |

## 完成后返回

返回本组改写的条目数、删除的条目数、保留的条目数，以及任何需要人工确认的 D 类条目。
