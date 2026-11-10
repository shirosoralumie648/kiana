# 注释清理工作单：kiana-json

- 待处理条目：16
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

- `crates/kiana-json/src/js_object.rs`
- `crates/kiana-json/src/js_parse.rs`
- `crates/kiana-json/src/js_parse/partial.rs`
- `crates/kiana-json/src/js_string.rs`
- `crates/kiana-json/tests/api_golden.rs`
- `crates/kiana-json/tests/json_repair.rs`
- `crates/kiana-json/tests/parse_streaming.rs`
- `crates/kiana-json/tests/parse_v8.rs`

## 待处理条目

| # | 位置 | 类别 | 命中 | 摘要 | 建议 |
|---:|---|---|---|---|---|
| 1 | `crates/kiana-json/src/js_parse/partial.rs:210` | A | 逐字一致 | /// 错误消息与 TS 逐字一致（含 ` at position N` 后缀与 `SyntaxError: ` 前缀）。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 2 | `crates/kiana-json/src/js_parse/partial.rs:466` | A | 与TS一致 | /// 与 TS 一致，本函数不检查第一个值之后是否还有内容。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 3 | `crates/kiana-json/src/js_parse/partial.rs:508` | A | 逐字一致, TS的 | // 连续反斜杠转义」。条件与 TS 的 while 逐字对应。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 4 | `crates/kiana-json/src/js_parse/partial.rs:522` | A | 逐字一致 | // TS `++index - Number(escape)` 的逐字对应；此时 index 指向闭合引号。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 5 | `crates/kiana-json/src/js_parse/partial.rs:782` | A | TS等价物, TS的 | /// 帧内错误（等价于 TS 的 `catch`）：`Allow` 命中时返回已收集的部分容器，否则转成该层 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 6 | `crates/kiana-json/src/js_parse/partial.rs:972` | A | 与TS一致 | // 与 TS 一致：无论 NUM 位是否打开，失败后都尝试从最后一个 e 处截断 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 7 | `crates/kiana-json/src/js_parse.rs:80` | A | 逐字一致 | /// `message` 与 V8 的 `SyntaxError.message` 逐字一致（`JSON.parse` 面），或与 TS | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 8 | `crates/kiana-json/tests/json_repair.rs:3` | A | 逐字一致, TS的 | //! 逐字保留 TS 的测试意图：修复规则与「抛修复后那次错误」的分支。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 9 | `crates/kiana-json/src/js_object.rs:3` | D | 逐字节一致 | //! 三个必须逐字节一致的地方： | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 10 | `crates/kiana-json/src/js_object.rs:418` | D | 强约束 | /// - 对象：键集合必须相同（先比长度，再逐键在对方中查找），与插入序无关； | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 11 | `crates/kiana-json/src/js_object.rs:419` | D | 强约束 | /// - 数组：长度必须相同，元素逐项比较； | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 12 | `crates/kiana-json/src/js_string.rs:85` | D | 逐字一致 | /// `typeof` 的字符串形式（与 V8 的返回值逐字一致，全部小写）。 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 13 | `crates/kiana-json/src/js_string.rs:225` | D | 强约束 | /// 类型必须相同且值相等：`1` 与 `1.0` 都走数字分支，按 `f64` 比较（相等）；`NaN` 与 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 14 | `crates/kiana-json/tests/api_golden.rs:49` | D | 逐字一致 | /// 只用于对照 fixture；`kiana-ai` 的实现必须与它逐字一致（`tests/api_golden.rs` 的 `tool_call` | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 15 | `crates/kiana-json/tests/parse_streaming.rs:4` | D | 逐字一致 | //! 的 3 个 `it` 逐字保留；其余用例来自 golden fixture。 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 16 | `crates/kiana-json/tests/parse_v8.rs:1` | D | 兼容面 | //! `JSON.parse` 的 V8 兼容面：消息、位置、excerpt、接受度。 | 人工判断：确认是否仍有独立技术原因；没有则删除 |

## 完成后返回

返回本组改写的条目数、删除的条目数、保留的条目数，以及任何需要人工确认的 D 类条目。
