# 注释清理工作单：kiana-extensions--part-2

- 待处理条目：12
- 涉及文件：10

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

- `crates/kiana-extensions/src/index.rs`
- `crates/kiana-extensions/src/lib.rs`
- `crates/kiana-extensions/src/loader/resolver.rs`
- `crates/kiana-extensions/src/runner/keybindings.rs`
- `crates/kiana-extensions/tests/engine_timers.rs`
- `crates/kiana-extensions/tests/extension_api_transaction.rs`
- `crates/kiana-extensions/tests/extensions_discovery.rs`
- `crates/kiana-extensions/tests/extensions_loader.rs`
- `crates/kiana-extensions/tests/intl.rs`
- `crates/kiana-extensions/tests/virtual_modules.rs`

## 待处理条目

| # | 位置 | 类别 | 命中 | 摘要 | 建议 |
|---:|---|---|---|---|---|
| 1 | `crates/kiana-extensions/tests/intl.rs:3` | D | 逐字一致 | //! 本地化文本只与 Node 对**已实测逐字一致**的 locale 比较（§6.4 的清单）； | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 2 | `crates/kiana-extensions/tests/intl.rs:343` | D | 逐字一致 | /// 与 Node 逐字一致的 locale（§6.4 已实测清单，20 个 locale 夹具核对）。 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 3 | `crates/kiana-extensions/tests/virtual_modules.rs:687` | A | 必须与TS, 与TS一致 | // 设计 §5.1 的 19 个语料符号必须全部在导出面里（类型必须与 TS 一致）。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 4 | `crates/kiana-extensions/tests/virtual_modules.rs:307` | D | 强约束 | // 随机字节由测试桩提供，序列位（variant）与 Node 夹具不同；版本/长度/形状必须一致。 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 5 | `crates/kiana-extensions/src/index.rs:124` | D | 逐字一致 | // 因此这里补齐缺的 5 个；签名与 `kiana-ext-api` 的字段类型逐字一致，可以直接赋值。 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 6 | `crates/kiana-extensions/src/lib.rs:554` | A | TS等价物, TS的 | /// TS 的等价物是 `VIRTUAL_MODULES["@earendil-works/kiana-tui"] = bundledKianaTui`：模块 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 7 | `crates/kiana-extensions/src/loader/resolver.rs:32` | D | 逐字节一致 | //! 时用同名键提供源码。**名字必须逐字节一致**（QuickJS 的模块缓存键就是名字）： | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 8 | `crates/kiana-extensions/src/runner/keybindings.rs:17` | A | 与TS一致 | //! 诊断顺序与 TS 一致：先内建冲突、后扩展间冲突。`has_ui == false` 时每条诊断还会复制一份 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 9 | `crates/kiana-extensions/tests/engine_timers.rs:823` | D | 逐字一致 | // 必须逐字一致（全局枚举性、参数校验文案+codes、AbortSignal 表面、句柄形状）。 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 10 | `crates/kiana-extensions/tests/extension_api_transaction.rs:5` | D | 逐字一致 | //! 的同一套事务实现；JS 定义的扩展经 `js/api.js` 的宿主原语，两边的文案必须逐字一致。 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 11 | `crates/kiana-extensions/tests/extensions_discovery.rs:3` | A | 逐字一致 | //! 每个用例的标题与 TS 逐字一致；加载走真实的 `discoverAndLoadExtensions` 流水线 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 12 | `crates/kiana-extensions/tests/extensions_loader.rs:1519` | A | 强约束 | /// TS `path.dirname("my-ext")` 是 `"."`（不是 `""`）；`SourceInfo.baseDir` 必须一致。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |

## 完成后返回

返回本组改写的条目数、删除的条目数、保留的条目数，以及任何需要人工确认的 D 类条目。
