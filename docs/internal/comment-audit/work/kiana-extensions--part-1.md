# 注释清理工作单：kiana-extensions--part-1

- 待处理条目：40
- 涉及文件：12

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

- `crates/kiana-extensions/src/engine/promise.rs`
- `crates/kiana-extensions/src/loader/loader_impl.rs`
- `crates/kiana-extensions/src/loader/mod.rs`
- `crates/kiana-extensions/src/loader/transpile.rs`
- `crates/kiana-extensions/src/runner/commands.rs`
- `crates/kiana-extensions/src/runner/context.rs`
- `crates/kiana-extensions/src/runner/dispatch.rs`
- `crates/kiana-extensions/src/runner/mcp_registration.rs`
- `crates/kiana-extensions/src/runner/mod.rs`
- `crates/kiana-extensions/src/runner/noop_ui.rs`
- `crates/kiana-extensions/src/ui/mod.rs`
- `crates/kiana-extensions/src/virtual_modules.rs`

## 待处理条目

| # | 位置 | 类别 | 命中 | 摘要 | 建议 |
|---:|---|---|---|---|---|
| 1 | `crates/kiana-extensions/src/loader/mod.rs:83` | A | TS等价物, TS的 | //! 注册，等价于 TS 的对象身份。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 2 | `crates/kiana-extensions/src/loader/mod.rs:803` | A | TS等价物, TS的 | /// panic 的 future（等价于 TS 的 rejected promise；JS 侧在 `js/api.js` 里直接用 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 3 | `crates/kiana-extensions/src/loader/mod.rs:917` | A | 与TS一致 | /// `unregisterVirtualModel` 立即生效（与 TS 一致）。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 4 | `crates/kiana-extensions/src/loader/mod.rs:4249` | A | 与TS一致 | /// `ToolRenderContext` → JS 对象（字段名/顺序与 TS 一致）。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 5 | `crates/kiana-extensions/src/loader/mod.rs:6163` | A | TS等价物, TS的 | /// 等价于 TS 的 `for await (const event of stream)`：先取 `[Symbol.asyncIterator]()`， | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 6 | `crates/kiana-extensions/src/loader/mod.rs:6919` | A | TS等价物 | /// 跳过该注册（等价于 TS 在 bind 时抛错、provider 未注册）。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 7 | `crates/kiana-extensions/src/loader/mod.rs:8558` | A | 与TS一致 | /// 缓存命中时**不重新执行模块体**（复用缓存的函数），与 TS 一致；模块没有有效默认导出时 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 8 | `crates/kiana-extensions/src/loader/mod.rs:3643` | D | 保真 | /// 因此 `undefined`、函数等保真（`user_bash` 的 `{ operations }` 与事件 handler 依赖它）。 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 9 | `crates/kiana-extensions/src/loader/loader_impl.rs:48` | D | 逐字节一致 | //! `url` 的百分号编码与 Node 的 `pathToFileURL` 逐字节一致（`file_url`，见 golden fixture）， | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 10 | `crates/kiana-extensions/src/loader/loader_impl.rs:289` | D | 逐字一致 | /// 与 jiti/Node 逐字一致的裸消息（`Cannot find module …` / `ParseError: …`）。 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 11 | `crates/kiana-extensions/src/loader/loader_impl.rs:321` | D | 逐字节一致 | /// 模块名（与 `Module::declare` 的 name 逐字节一致，可能带 `?v=`）。 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 12 | `crates/kiana-extensions/src/loader/loader_impl.rs:565` | D | 完全一致 | /// 用当前开关装饰一个 `Resolver`（§3.4 的严格去缓存）。开关关闭时行为与内层完全一致。 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 13 | `crates/kiana-extensions/src/loader/loader_impl.rs:799` | D | 逐字节一致 | /// `name` 可以是带 `?v=` 的入口名：声明出的模块名与 `name` 逐字节一致，`import.meta.url` | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 14 | `crates/kiana-extensions/src/loader/loader_impl.rs:2053` | D | 逐字节一致 | /// 逐字节一致。 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 15 | `crates/kiana-extensions/src/runner/mod.rs:27` | A | 与TS一致 | //! `ctx.getSystemPrompt()` 看到结果）与 TS 一致。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 16 | `crates/kiana-extensions/src/runner/mod.rs:1687` | A | 与TS一致 | /// 的结果（当前实现恒为空，与 TS 一致）。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 17 | `crates/kiana-extensions/src/runner/mod.rs:1725` | A | 与TS一致 | /// 与 TS 一致先做 `assertActive()`（`spec/ca-extensions.md` §2.3.9 的 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 18 | `crates/kiana-extensions/src/runner/mod.rs:1878` | A | 与TS一致 | /// 动态 `kiana.on()` / `off()` 的结果与 TS 一致。返回最终的完整消息列表。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 19 | `crates/kiana-extensions/src/runner/mod.rs:2072` | A | 与TS一致 | /// 缺省值与 TS 一致（测试 `normalizes_system_prompt_options_with_ts_defaults` 锁定）： | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 20 | `crates/kiana-extensions/src/engine/promise.rs:6` | D | 逐字一致 | //! JS 协议（`js/prelude_core.js`，与设计文档 §2.3 的 `globalThis.__kiana` 逐字一致）： | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 21 | `crates/kiana-extensions/src/engine/promise.rs:235` | D | 完全一致 | /// 走 `JSON.parse` 而不是逐键 `Object::set`：这样才能拿到与 JS 完全一致的对象语义 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 22 | `crates/kiana-extensions/src/engine/promise.rs:589` | D | 保真 | /// 保真 JS 的数字/对象字符串化）；prelude 尚未安装时退回 Rust 侧的属性读取。 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 23 | `crates/kiana-extensions/src/runner/dispatch.rs:57` | A | 与TS一致 | //! 语义与 TS 一致； | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 24 | `crates/kiana-extensions/src/runner/dispatch.rs:1601` | A | 与TS一致 | // 因此 phase 1 里动态 `kiana.on()/off()` 的结果与 TS 一致。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 25 | `crates/kiana-extensions/src/runner/dispatch.rs:1850` | A | 与TS一致 | /// 不写 `images` 键（JS 读到 `undefined`，与 TS 一致），`streamingBehavior` 同理。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 26 | `crates/kiana-extensions/src/virtual_modules.rs:250` | A | 与TS一致 | /// （对象身份与 TS 一致）。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 27 | `crates/kiana-extensions/src/virtual_modules.rs:199` | D | 逐字一致 | /// 拼写必须与 bridge.js 的判断逐字一致：未知拼写会落到 callable 分支，`Object` 成员会被 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 28 | `crates/kiana-extensions/src/virtual_modules.rs:785` | D | 逐字节一致 | /// 必须与 loader 交给 `Module::declare` 的名字逐字节一致，否则同一个模块会在缓存里出现两份 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 29 | `crates/kiana-extensions/src/loader/transpile.rs:54` | A | 逐字一致 | //! 的源码内联到模块顶部（来源：`@oxc-project/runtime@0.155.0`，MIT，函数体与上游逐字一致）。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 30 | `crates/kiana-extensions/src/loader/transpile.rs:4` | D | 逐字一致 | //! 流水线与设计文档逐字一致： | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 31 | `crates/kiana-extensions/src/runner/commands.rs:28` | A | 与TS一致 | //! 冲突不会产生诊断——与 TS 一致，`commandDiagnostics` 始终为空；算法细节由 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 32 | `crates/kiana-extensions/src/runner/commands.rs:83` | A | 完全一致, TS的 | // 每个后缀只依赖已分配集合，因此结果与 TS 的逐条分配完全一致。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 33 | `crates/kiana-extensions/src/runner/context.rs:22` | A | 与TS一致 | //! JS 外观重建这个形状（`Object.keys`/展开/`typeof` 特性检测与 TS 一致），每个 getter 读取 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 34 | `crates/kiana-extensions/src/runner/context.rs:1597` | A | 与TS一致 | /// 特性检测与 TS 一致）；原生实例仍是全部状态与宿主调用的落点。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 35 | `crates/kiana-extensions/src/runner/mcp_registration.rs:22` | A | 逐字一致, TS的 | //! 即使名字已被占用，也报校验错误。错误 `Display` 与 TS 的 `Error.message` 逐字一致。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 36 | `crates/kiana-extensions/src/runner/mcp_registration.rs:100` | A | 逐字一致, TS: | /// TS: `loader.ts#registerMcpServer`。步骤与顺序逐字对应： | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 37 | `crates/kiana-extensions/src/runner/noop_ui.rs:26` | A | 与TS一致 | //! 因此扩展代码拿到的行为与 TS 一致。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 38 | `crates/kiana-extensions/src/runner/noop_ui.rs:40` | D | 强约束 | //! （`noop_ui_object`），与这里的 Rust 实现是两份代码；两者的可见默认值必须一致。 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 39 | `crates/kiana-extensions/src/ui/mod.rs:191` | A | 逐字一致, TS: | /// 超出行数时追加的逐字文本（`interactive-mode.ts:2427`）。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 40 | `crates/kiana-extensions/src/ui/mod.rs:3207` | A | 与TS一致 | /// `AbortController().signal`（不会 abort，但形状与 TS 一致）。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |

## 完成后返回

返回本组改写的条目数、删除的条目数、保留的条目数，以及任何需要人工确认的 D 类条目。
