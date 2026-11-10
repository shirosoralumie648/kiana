# 注释清理工作单：kiana-codemode--part-2

- 待处理条目：5
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

- `crates/kiana-codemode/tests/api_surface.rs`
- `crates/kiana-codemode/tests/deterministic.rs`
- `crates/kiana-codemode/tests/protocol.rs`
- `crates/kiana-codemode/tests/source.rs`
- `crates/kiana-codemode/tests/vm.rs`

## 待处理条目

| # | 位置 | 类别 | 命中 | 摘要 | 建议 |
|---:|---|---|---|---|---|
| 1 | `crates/kiana-codemode/tests/api_surface.rs:136` | D | 逐字一致 | /// spec §2.2.5：`loadQuickJSWasm(path?)` 的加载失败是 Node 的逐字文本。 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 2 | `crates/kiana-codemode/tests/deterministic.rs:927` | A | 与TS一致 | /// 接受——可观察语义与 TS 一致的部分全部保留。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 3 | `crates/kiana-codemode/tests/protocol.rs:310` | D | 保真 | // JSON 文本保真（`ARCHITECTURE.md` §4.3 第 3、4 条）：数字按 ECMAScript | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 4 | `crates/kiana-codemode/tests/source.rs:3` | A | 逐字一致, TS的 | //! 每个 `describe`/`it` 标题逐字保留（ARCHITECTURE.md §4.8 第 1 条），便于对照 TS 的失败报告。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 5 | `crates/kiana-codemode/tests/vm.rs:1509` | A | 逐字一致 | /// 所以 `done` 的返回值与 TS 逐字一致。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |

## 完成后返回

返回本组改写的条目数、删除的条目数、保留的条目数，以及任何需要人工确认的 D 类条目。
