# 注释清理工作单：kiana-ext-api

- 待处理条目：9
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

- `crates/kiana-ext-api/src/events.rs`
- `crates/kiana-ext-api/src/tool_result.rs`
- `crates/kiana-ext-api/src/tool_types.rs`
- `crates/kiana-ext-api/tests/tool_types.rs`

## 待处理条目

| # | 位置 | 类别 | 命中 | 摘要 | 建议 |
|---:|---|---|---|---|---|
| 1 | `crates/kiana-ext-api/src/events.rs:105` | A | 逐字一致, TS的 | // 经 `#[serde(rename = ...)]` 与 TS 的字符串值逐字一致。41 个枚举由宏生成，避免手写漂移。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 2 | `crates/kiana-ext-api/src/events.rs:2844` | A | 逐字一致 | /// 线上字面量（与 TS 事件名逐字一致）。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 3 | `crates/kiana-ext-api/src/events.rs:3122` | A | 逐字一致 | // 指数区间等与 TS `JSON.stringify` 逐字一致，而不是 serde 默认的 ryu 形式。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 4 | `crates/kiana-ext-api/src/tool_result.rs:59` | A | 与TS一致, TS的 | /// `cwd` 与 TS 一致是**字符串**；TS 的 `execCommand` 从不读取它（真正生效的是其第 3 个 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 5 | `crates/kiana-ext-api/src/tool_types.rs:202` | A | 与TS一致, 完全一致 | /// `ARCHITECTURE.md` §5.2 的偏离说明）。14 个字段名/顺序与 TS 完全一致。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 6 | `crates/kiana-ext-api/src/tool_types.rs:414` | A | TS等价物, TS的 | /// provider 侧受限采样；`None` 等价于 TS 的 `false`/`undefined`。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 7 | `crates/kiana-ext-api/tests/tool_types.rs:640` | A | 与TS一致 | // `cwd` 与 TS 一致是字符串（且 `execCommand` 从不读取它）。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 8 | `crates/kiana-ext-api/src/events.rs:3598` | D | 逐字一致 | /// （消息逐字保留，`ARCHITECTURE.md` §4.1）。 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 9 | `crates/kiana-ext-api/src/tool_result.rs:187` | D | 逐字一致 | /// `Box<dyn Error + Send + Sync>` 透传（消息逐字保留，`ARCHITECTURE.md` §4.1）。 | 人工判断：确认是否仍有独立技术原因；没有则删除 |

## 完成后返回

返回本组改写的条目数、删除的条目数、保留的条目数，以及任何需要人工确认的 D 类条目。
