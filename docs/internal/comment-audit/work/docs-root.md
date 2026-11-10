# 注释清理工作单：docs-root

- 待处理条目：25
- 涉及文件：1

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

- `ARCHITECTURE.md`

## 待处理条目

| # | 位置 | 类别 | 命中 | 摘要 | 建议 |
|---:|---|---|---|---|---|
| 1 | `ARCHITECTURE.md:408` | A | 必须与TS, 与TS一致 | 但**反序列化的默认值**必须与 TS 一致：`??` 与 `\|\|` 的差别要在 `Default` 实现里体现。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 2 | `ARCHITECTURE.md:436` | A | 兼容面, TS的 | - `RUST_LOG` 与 `KIANA_DEBUG` 不作为兼容面；调试开关沿用 TS 的 `KIANA_TIMING`（见 §4.5）。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 3 | `ARCHITECTURE.md:499` | A | 必须与TS | 场景。**任何**进入会话文件/遥测的 id 格式必须与 TS 相同（`uuid` v4 字符串、`sha2` 十六进制前缀）。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 4 | `ARCHITECTURE.md:1637` | A | 与TS一致 | 按 Node 的做法短暂重试；`start()` 与 TS 一致地在 spawn 前就置 `started`（spawn 失败也 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 5 | `ARCHITECTURE.md:1668` | A | 逐字节一致 | normalizeWindowsShellPath）与 `config.ts` 的 7 个常量 + 4 种环境状态逐字节一致；kiana-session-core | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 6 | `ARCHITECTURE.md:1697` | A | 完全一致 | `tests/fixtures/corpus/registrations.json` 完全一致（0 diff）；自制的 ESM（相对 `.ts` | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 7 | `ARCHITECTURE.md:1740` | A | 完全一致 | 黄金测试与 `kiana`（TS 二进制）逐字节对齐。**这是“功能完全一致”的判定点。** | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 8 | `ARCHITECTURE.md:1757` | A | 逐字节一致 | TS 竞态说明）；两轮会话写出的 session JSONL 在规范化 id/timestamp/cwd 后**逐字节一致**， | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 9 | `ARCHITECTURE.md:1784` | A | 逐字节一致 | 仍逐字回放）。现在 system/assistant/toolResult 的键序与 TS 逐字节一致。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 10 | `ARCHITECTURE.md:1944` | A | 与TS一致 | `options.fetch` 无关）。签名与 TS 一致：`(model, context, options) -> AssistantMessageEventStream`， | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 11 | `ARCHITECTURE.md:1950` | A | 逐字一致 | `HttpError::message` 必须是 TS `error.message` 的逐字文本（reqwest 的错误文本要转写， | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 12 | `ARCHITECTURE.md:6` | D | 完全一致 | - 目标：**功能与用户交互完全一致**——相同的公开 API 语义、相同的错误消息、相同的事件顺序、 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 13 | `ARCHITECTURE.md:249` | D | 逐字一致 | - `js_string`：`String(v)`、truthiness、`typeof`（错误消息与日志要逐字一致）。 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 14 | `ARCHITECTURE.md:430` | D | 逐字一致 | `[kiana] Failed to load extension ...`），必须逐字保留；实现方式： | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 15 | `ARCHITECTURE.md:438` | D | 兼容面 | ### 4.5 环境变量（兼容面，全部逐字） | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 16 | `ARCHITECTURE.md:510` | D | 逐字一致 | 当测试需要访问私有项时）。`describe`/`it` 的**标题逐字保留**（英文原文），因为失败报告要能 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 17 | `ARCHITECTURE.md:1523` | D | 强约束 | - `kiana-env` 的部署由 `kiana-env-client::ssh` 完成（`~/.kiana/mobile/tools/kiana-env`），二进制名与路径不得改变。 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 18 | `ARCHITECTURE.md:1621` | D | 逐字节一致 | `JSON.parse`/`JSON.stringify` 22/23 向量逐字节一致（唯一差异是已文档化的孤立代理项 → | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 19 | `ARCHITECTURE.md:1622` | D | 逐字节一致 | U+FFFD），`parseStreamingJson` 全部向量一致；`kiana-tui` 的 `Text.render` 8/8 组逐字节一致； | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 20 | `ARCHITECTURE.md:1674` | D | 逐字节一致 | 3 个夹具 HTML 逐字节一致（sha256 `9775e404…` / `15c8de4f…` / `2c5cdd74…`）。 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 21 | `ARCHITECTURE.md:1705` | D | 逐字节一致 | package 路径（docs/examples/README）外逐字节一致。 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 22 | `ARCHITECTURE.md:1754` | D | 逐字节一致 | 非法值、缺少选项值、`-p --unknown-opt` 的 stdout/stderr/退出码**逐字节一致**。 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 23 | `ARCHITECTURE.md:1755` | D | 逐字节一致 | 2. **print/json**：`kiana -p`（纯文本、触发 `read` 工具）stdout 逐字节一致； | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 24 | `ARCHITECTURE.md:1816` | D | 完全一致 | `checkedAt` 后完全一致，包括未知字段、JS 数字/键序与目录顺序。compaction 的 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 25 | `ARCHITECTURE.md:1883` | D | 逐字节一致 | 的签名逐字节一致）； | 人工判断：确认是否仍有独立技术原因；没有则删除 |

## 完成后返回

返回本组改写的条目数、删除的条目数、保留的条目数，以及任何需要人工确认的 D 类条目。
