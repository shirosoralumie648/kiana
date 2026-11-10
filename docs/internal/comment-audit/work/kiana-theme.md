# 注释清理工作单：kiana-theme

- 待处理条目：23
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

- `crates/kiana-theme/src/assets.rs`
- `crates/kiana-theme/src/lib.rs`
- `crates/kiana-theme/src/theme.rs`
- `crates/kiana-theme/src/theme_controller.rs`
- `crates/kiana-theme/src/theme_schema.rs`
- `crates/kiana-theme/tests/theme_picker.rs`

## 待处理条目

| # | 位置 | 类别 | 命中 | 摘要 | 建议 |
|---:|---|---|---|---|---|
| 1 | `crates/kiana-theme/src/assets.rs:33` | A | 逐字节一致, TS的 | /// 内容与 TS 的 `dark.json` 逐字节一致（含制表符缩进），键序见该文件本身。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 2 | `crates/kiana-theme/src/assets.rs:47` | A | 逐字节一致 | // 与 TS 仓库里的文件逐字节一致（含 `$schema` 行与制表符缩进）。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 3 | `crates/kiana-theme/src/lib.rs:23` | A | 必须与TS, 逐字一致, TS版本 | //! 主题也没有统一入口；而校验错误是主题作者直接看到的用户文案，必须与 TS 版本逐字一致。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 4 | `crates/kiana-theme/src/lib.rs:88` | A | 逐字一致 | //! - [`ThemeError`] 的 `Display` 就是 TS `Error.message` 的逐字文本（不含包装前缀）， | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 5 | `crates/kiana-theme/src/theme.rs:1485` | A | 与TS一致, 逐字一致 | /// 因此永远到不了已注册/自定义主题分支（与 TS 一致），错误文案也逐字保留。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 6 | `crates/kiana-theme/src/theme.rs:1582` | A | 与TS一致 | /// 返回的 `Arc<Theme>` 不注册、不设为全局主题，调用方自行决定用途。错误文案与 TS 一致： | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 7 | `crates/kiana-theme/src/theme_controller.rs:16` | A | TS等价物, TS的 | //! tokio 运行时上下文里先轮询一次 —— 已经就绪的结果当场应用（等价于 TS 的 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 8 | `crates/kiana-theme/src/theme_controller.rs:602` | A | TS等价物, TS的 | /// 发起时已经就绪的查询当场应用（等价于 TS 的 `promise.then(apply)`），因此丢弃返回的 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 9 | `crates/kiana-theme/src/theme_schema.rs:474` | A | 与TS一致 | // 没有 `/` 或 `~`，`vars` 的键可能任意，行为与 TS 一致）。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 10 | `crates/kiana-theme/src/theme_schema.rs:1933` | A | 与TS一致 | /// 回归：`createTheme()` 的错误优先级与 TS 一致 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 11 | `crates/kiana-theme/src/lib.rs:52` | D | 逐字一致 | //! 手写的校验器，错误文案逐字一致。校验器通过 [`set_theme_json_validator`] 安装， | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 12 | `crates/kiana-theme/src/theme.rs:29` | D | 逐位一致 | //! 逐位一致，见 kiana-tui 的 localeCompare 说明）。 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 13 | `crates/kiana-theme/src/theme.rs:31` | D | 逐字一致 | //! position/line/column、片段规则、整串特例与错误文案逐字一致）。它接受 `1e999` | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 14 | `crates/kiana-theme/src/theme.rs:764` | D | 逐字节一致 | /// 得到的（`Theme::style` 必须和 `styleText(..., theme.colors[…], mode)` 逐字节一致）。 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 15 | `crates/kiana-theme/src/theme.rs:1262` | D | 逐字一致 | /// `error.message` 逐字相同。 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 16 | `crates/kiana-theme/src/theme.rs:1310` | D | 逐位一致 | // （`kiana-tui` 已提供与 Node 26 逐位一致的实现）。 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 17 | `crates/kiana-theme/src/theme.rs:1455` | D | 逐字一致 | /// 位置、行/列（`\r`/`\r\n` 都是换行）、整串特例与片段截断都与 V8 逐字一致。 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 18 | `crates/kiana-theme/src/theme.rs:1462` | D | 逐字节一致 | /// U+FFFD 表示。Node 把该消息写成 UTF-8 时同样编码成 EF BF BD，因此输出逐字节一致。 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 19 | `crates/kiana-theme/src/theme.rs:3099` | D | 逐字节一致 | // 编码成 EF BF BD（U+FFFD），与 Rust 的字符串逐字节一致。 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 20 | `crates/kiana-theme/src/theme_schema.rs:8` | D | 逐字一致 | //! 错误分类与聚合格式都必须逐字一致，因为主题作者会看到这些消息。 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 21 | `crates/kiana-theme/src/theme_schema.rs:29` | D | 逐字一致 | //! 这一点也逐字保留。 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 22 | `crates/kiana-theme/src/theme_schema.rs:1313` | D | 逐字一致 | // 这里的断言锁定冻结契约：错误文案逐字一致、错误产生顺序、8 条上限，以及 JS 的 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 23 | `crates/kiana-theme/tests/theme_picker.rs:86` | D | 逐字一致 | /// 逐字相同的 Node `error.message`。 | 人工判断：确认是否仍有独立技术原因；没有则删除 |

## 完成后返回

返回本组改写的条目数、删除的条目数、保留的条目数，以及任何需要人工确认的 D 类条目。
