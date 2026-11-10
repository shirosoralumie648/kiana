# 注释清理工作单：kiana-agent-core

- 待处理条目：37
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

- `crates/kiana-agent-core/Cargo.toml`
- `crates/kiana-agent-core/src/agent.rs`
- `crates/kiana-agent-core/src/agent_loop.rs`
- `crates/kiana-agent-core/src/lib.rs`
- `crates/kiana-agent-core/src/proxy.rs`
- `crates/kiana-agent-core/src/stream_fn.rs`
- `crates/kiana-agent-core/src/types.rs`
- `crates/kiana-agent-core/src/wire_keys.rs`
- `crates/kiana-agent-core/tests/agent.rs`
- `crates/kiana-agent-core/tests/agent_loop.rs`
- `crates/kiana-agent-core/tests/proxy.rs`
- `crates/kiana-agent-core/tests/public_api.rs`
- `crates/kiana-agent-core/tests/util/calculate.rs`
- `crates/kiana-agent-core/tests/util/get_current_time.rs`

## 待处理条目

| # | 位置 | 类别 | 命中 | 摘要 | 建议 |
|---:|---|---|---|---|---|
| 1 | `crates/kiana-agent-core/src/agent.rs:345` | A | TS等价物, TS的 | /// 非 nullish 值。JSON 里没有 `undefined`，因此“没有该键”等价于 TS 的 `undefined`。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 2 | `crates/kiana-agent-core/src/agent.rs:1442` | A | 逐字一致 | /// `conflict` 是 `begin_run` 竞争失败时返回的错误（各入口的 TS 逐字文本不同）。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 3 | `crates/kiana-agent-core/src/agent.rs:1675` | A | 与TS一致 | /// 顺序与 TS 一致：先登记 `activeRun`，再置 `isStreaming = true` 并清空 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 4 | `crates/kiana-agent-core/src/agent.rs:2148` | A | TS等价物, TS的 | /// 退订（等价于 TS 的 `unsubscribe()` 闭包：`this.listeners.delete(listener)`）。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 5 | `crates/kiana-agent-core/src/agent_loop.rs:444` | A | 与TS一致 | /// 同步前置校验与 TS 一致（在创建流之前失败）： | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 6 | `crates/kiana-agent-core/src/agent_loop.rs:1302` | A | 与TS一致 | // 其余增量事件只在已有草稿时生效；没有 `start` 的 delta 被忽略（与 TS 一致）。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 7 | `crates/kiana-agent-core/src/agent_loop.rs:1485` | A | TS等价物, TS的 | /// 工具运行中的增量回报；`None` 表示丢弃（等价于 TS 的 no-op `onUpdate`）。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 8 | `crates/kiana-agent-core/src/agent_loop.rs:1991` | A | 逐字一致 | /// 算法在 JSON 值上校验。工具的 schema 都是对象类型时，结果是与 TS 逐字相同的 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 9 | `crates/kiana-agent-core/src/agent_loop.rs:2069` | A | 与TS一致 | /// 顺序（与 TS 一致）： | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 10 | `crates/kiana-agent-core/src/agent_loop.rs:2072` | A | 逐字一致 | /// 2. 校验（见 `validate_prepared_tool_arguments`）；失败即立即结果，文本与 TS 逐字一致。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 11 | `crates/kiana-agent-core/src/agent_loop.rs:2473` | A | TS等价物 | /// 键序与插入序一致（等价于 TS 事件里直接传参数对象的可观察形状）；序列化失败时退回 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 12 | `crates/kiana-agent-core/src/lib.rs:77` | A | 逐字一致 | //! - **构造期**：没有可用的流函数时 [`agent::Agent::new`] 失败，错误文本与 TS 逐字一致 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 13 | `crates/kiana-agent-core/src/proxy.rs:439` | A | 与TS一致, TS里 | /// ——与 TS 一致（TS 里 `streamProxy` 永不抛，异常被 IIFE 的 catch 转成 error 事件）。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 14 | `crates/kiana-agent-core/src/stream_fn.rs:20` | A | 逐字一致, TS的, 对应TS | //! （骨架固定签名），[`require_default_stream_fn`] 对应 TS 的“未配置即抛错”，错误文本逐字一致。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 15 | `crates/kiana-agent-core/src/stream_fn.rs:73` | A | 逐字一致 | /// `getDefaultStreamFn()`：未配置时返回与 TS 抛错文本逐字一致的 [`DefaultStreamFnError`]。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 16 | `crates/kiana-agent-core/src/types.rs:149` | A | 与TS一致, 完全一致 | /// `role` 字段在两者中都出现在 JSON 顶层，因此序列化结果与 TS 完全一致。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 17 | `crates/kiana-agent-core/src/types.rs:1003` | A | TS等价物, TS的 | /// 长耗时工作（如压缩）。返回 `None` 等价于 TS 的 `void`：不干预本轮决策。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 18 | `crates/kiana-agent-core/tests/agent.rs:174` | A | TS等价物, TS的 | /// 构造 `StreamFn`：`f` 同步返回一个事件流（等价于 TS 的 `() => { ... return stream; }`）。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 19 | `crates/kiana-agent-core/tests/agent_loop.rs:4245` | A | 逐字一致 | /// 回归（minor）：`prepareArguments` 返回非对象时交给校验器，产生与 TS 逐字相同的 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 20 | `crates/kiana-agent-core/tests/proxy.rs:25` | A | 逐字一致 | // 夹具（逐字对应 proxy.test.ts 顶部的 `model` / `usage`） | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 21 | `crates/kiana-agent-core/tests/public_api.rs:738` | A | 必须与TS | // 回归（reviewer：major）：标准消息 / usage 的数字格式必须与 TS `JSON.stringify` 一致 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 22 | `crates/kiana-agent-core/tests/util/calculate.rs:577` | A | 逐字一致 | // TS 抛 `new Error(e.message \|\| String(e))`；Rust 的解析错误消息不逐字相同， | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 23 | `crates/kiana-agent-core/Cargo.toml:14` | D | 逐位一致 | # （差 1 ULP，会把该值变成 `0.00024375`），开启后与 JS `JSON.parse` 的位模式逐位一致。 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 24 | `crates/kiana-agent-core/src/agent.rs:55` | D | 逐字一致 | //! [`Agent::continue_`]）先在锁内检查 `active_run`，冲突时返回各自逐字对应的 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 25 | `crates/kiana-agent-core/src/agent.rs:111` | D | 逐字一致 | //! 错误文本逐字保持（见 `spec/agent-codemode.md` §3.2）。 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 26 | `crates/kiana-agent-core/src/agent.rs:543` | D | 逐字一致 | /// [`Agent`] 的同步前置错误（逐字对应 `spec/agent-codemode.md` §3.2）。 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 27 | `crates/kiana-agent-core/src/agent.rs:1688` | D | 逐字一致 | /// 检查抛 `Agent is already processing.`，但正常路径上各入口的同步检查先抛各自逐字文本； | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 28 | `crates/kiana-agent-core/src/proxy.rs:42` | D | 逐字一致 | //! ## 必须逐字保留的文本 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 29 | `crates/kiana-agent-core/src/proxy.rs:57` | D | 逐字一致 | //! [`kiana_json::js_string::js_trim`]（V8 的 `SyntaxError.message` 逐字一致）；模板字符串里的 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 30 | `crates/kiana-agent-core/src/stream_fn.rs:14` | D | 逐字一致 | //! 求值时机（逐字保持）：`agentLoop`/`agentLoopContinue` 里 `streamFn ?? getDefaultStreamFn()` | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 31 | `crates/kiana-agent-core/src/types.rs:80` | D | 强约束 | /// `details`/`structuredContent`）都经过 `JSON.stringify`，落盘与上屏的字节必须一致 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 32 | `crates/kiana-agent-core/src/wire_keys.rs:148` | D | 保真 | /// 分支在当前仓库不可达，本函数不会产生上述包装对象；远端 JSON 的保真（未知字段与插入序） | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 33 | `crates/kiana-agent-core/tests/agent_loop.rs:4565` | D | 逐字一致 | /// spec §3.2 的验收项，单独锁住逐字文本。 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 34 | `crates/kiana-agent-core/tests/public_api.rs:777` | D | 逐位一致 | // 0.00024375 / 0.0254715；`float_roundtrip` 打开后与 `JSON.parse` 的位模式逐位一致。 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 35 | `crates/kiana-agent-core/tests/util/get_current_time.rs:25` | D | 完全一致 | //! 其余 id 与 Node 完全一致，对拍覆盖在 `tests/fixtures/`： | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 36 | `crates/kiana-agent-core/tests/util/get_current_time.rs:220` | D | 完全一致 | // 而 ICU4X 的解析器大小写不敏感，因此要求拼写与归一化结果完全一致。 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 37 | `crates/kiana-agent-core/tests/util/get_current_time.rs:1336` | D | 强约束 | // 的 tzdata 特判）外，本地时区分支与「显式 timeZone」分支对这些名字必须一致。 | 人工判断：确认是否仍有独立技术原因；没有则删除 |

## 完成后返回

返回本组改写的条目数、删除的条目数、保留的条目数，以及任何需要人工确认的 D 类条目。
