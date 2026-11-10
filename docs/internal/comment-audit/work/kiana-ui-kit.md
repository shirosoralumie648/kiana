# 注释清理工作单：kiana-ui-kit

- 待处理条目：10
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

- `crates/kiana-ui-kit/src/bordered_loader.rs`
- `crates/kiana-ui-kit/src/countdown_timer.rs`
- `crates/kiana-ui-kit/src/diff.rs`
- `crates/kiana-ui-kit/src/dynamic_border.rs`
- `crates/kiana-ui-kit/src/visual_truncate.rs`

## 待处理条目

| # | 位置 | 类别 | 命中 | 摘要 | 建议 |
|---:|---|---|---|---|---|
| 1 | `crates/kiana-ui-kit/src/bordered_loader.rs:15` | A | TS等价物, TS的 | //! - 传入的正是当前全局主题 → 每次着色读全局主题（等价于 TS 的 Proxy，主题切换后立即生效）； | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 2 | `crates/kiana-ui-kit/src/countdown_timer.rs:15` | A | 与TS一致 | //! 先 `dispose()` 再 `on_expire()`（**顺序**与 TS 一致：先 tick、再 requestRender、再判断过期）； | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 3 | `crates/kiana-ui-kit/src/diff.rs:59` | A | 逐字节一致 | //! 换掉（例如改用 `similar` crate），都会破坏与 TS 输出的逐字节一致性。`tests/fixtures/ | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 4 | `crates/kiana-ui-kit/src/diff.rs:102` | A | TS等价物, TS的 | /// `Default` 得到 `file_path: None`，等价于 TS 的 `{}`。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 5 | `crates/kiana-ui-kit/src/diff.rs:142` | A | 逐字一致 | /// `parseDiffLine` 的结果（字段名与 TS 逐字对应）。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 6 | `crates/kiana-ui-kit/src/diff.rs:360` | A | 逐字节一致 | // 相等性、空白归一）都会破坏与 TS 输出的逐字节一致性；请用 `tests/fixtures/diff_words.json` | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 7 | `crates/kiana-ui-kit/src/visual_truncate.rs:106` | A | 逐字一致 | /// `VisualLinePreviewOptions`（TS 接口逐字对应）。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 8 | `crates/kiana-ui-kit/src/diff.rs:12` | D | 完全一致 | //! 处理与 JS 完全一致；只有「两个不同星形平面字符共享高代理项」这类病态输入会在 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 9 | `crates/kiana-ui-kit/src/diff.rs:17` | D | 逐字一致 | //! `render_diff` 的解析规则逐字保留：`parseDiffLine` 的正则 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 10 | `crates/kiana-ui-kit/src/dynamic_border.rs:56` | D | 完全一致 | /// 保存同一个 trait object，渲染行为完全一致。 | 人工判断：确认是否仍有独立技术原因；没有则删除 |

## 完成后返回

返回本组改写的条目数、删除的条目数、保留的条目数，以及任何需要人工确认的 D 类条目。
