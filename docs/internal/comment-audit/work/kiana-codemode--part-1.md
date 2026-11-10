# 注释清理工作单：kiana-codemode--part-1

- 待处理条目：40
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

- `crates/kiana-codemode/src/declarations.rs`
- `crates/kiana-codemode/src/lib.rs`
- `crates/kiana-codemode/src/runtime/execution.rs`
- `crates/kiana-codemode/src/runtime/host.rs`
- `crates/kiana-codemode/src/runtime/prelude_source.rs`
- `crates/kiana-codemode/src/runtime/protocol.rs`
- `crates/kiana-codemode/src/runtime/remote.rs`
- `crates/kiana-codemode/src/runtime/vm.rs`
- `crates/kiana-codemode/src/runtime/worker.rs`
- `crates/kiana-codemode/src/source.rs`
- `crates/kiana-codemode/src/types.rs`
- `crates/kiana-codemode/src/wasm.rs`
- `crates/kiana-codemode/tests/declarations.rs`
- `crates/kiana-codemode/tests/execution.rs`

## 待处理条目

| # | 位置 | 类别 | 命中 | 摘要 | 建议 |
|---:|---|---|---|---|---|
| 1 | `crates/kiana-codemode/tests/declarations.rs:3` | A | 逐字一致, TS的 | //! 每个 `describe`/`it` 标题逐字保留（ARCHITECTURE.md §4.8 第 1 条），便于对照 TS 的失败报告。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 2 | `crates/kiana-codemode/tests/declarations.rs:955` | A | 逐字一致 | // 128 层以内不受影响（与 TS 逐字一致，见黄金样例）。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 3 | `crates/kiana-codemode/tests/declarations.rs:1026` | A | 逐字一致 | /// `{ ...schema, type: entry }` 的浅展开语义与 TS 逐字一致：嵌套的类型数组、非字符串条目、 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 4 | `crates/kiana-codemode/tests/declarations.rs:1164` | A | 与TS一致 | /// 与上一条互补：`const`/`enum` 以及**嵌套**在默认值里的非有限数与 TS 一致，因为 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 5 | `crates/kiana-codemode/tests/declarations.rs:1249` | A | 逐字一致, TS的 | /// 逐字相同的类型，要么同样抛错（TS 的 `URIError` ↔ Rust 的 panic）。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 6 | `crates/kiana-codemode/tests/declarations.rs:1295` | D | 逐字一致 | /// 偏差条目逐条钉死「只少了那一条 tag，其余逐字相同」，因此偏差既不会被误当成 bug 修掉 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 7 | `crates/kiana-codemode/tests/declarations.rs:1390` | D | 逐字一致 | // 除 tag 之外的部分必须逐字一致：把两侧的 tag 都抹掉再比。 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 8 | `crates/kiana-codemode/src/runtime/vm.rs:8` | A | 逐字一致, TS的, 对应TS | //! # 与调用方的契约（逐字对应 TS 的模块注释） | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 9 | `crates/kiana-codemode/src/runtime/vm.rs:372` | A | 与TS一致 | /// `wasm_impl::Handle`），本函数等它跑到第一次等待后返回，因此宿主看到的时序与 TS 一致。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 10 | `crates/kiana-codemode/src/runtime/vm.rs:709` | A | 逐字一致 | /// 文本要与 TS 逐字一致，所以这里逐项列出。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 11 | `crates/kiana-codemode/src/runtime/vm.rs:889` | A | 逐字节一致 | /// 3 个 U+FFFD，文本长度与 TS 差得更远。合法 UTF-8（含 4 字节的星平面字符）逐字节一致。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 12 | `crates/kiana-codemode/src/runtime/vm.rs:2562` | A | TS等价物, TS的 | /// 真正的释放由 [`super::VmSession::dispose`] 丢弃整个会话完成（等价于 TS 的 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 13 | `crates/kiana-codemode/src/runtime/vm.rs:688` | D | 完全一致 | /// 同一个异常抛到 `startVm`/`deliver` 的 catch 里完全一致。 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 14 | `crates/kiana-codemode/src/declarations.rs:50` | A | 逐字一致 | //! **抛错的时机**与 TS 逐字一致：每段先 `decodeURIComponent` 再判断「段是否存在」，而「是否 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 15 | `crates/kiana-codemode/src/declarations.rs:114` | A | 逐字一致 | //! （`schema_cases.json`），转义后的文本与 TS 逐字一致。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 16 | `crates/kiana-codemode/src/declarations.rs:175` | A | 逐字节一致 | /// 这里从第 128 层起渲染 `unknown`。≤127 层与 TS 逐字节一致；真实工具 schema | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 17 | `crates/kiana-codemode/src/declarations.rs:225` | A | 逐字一致 | /// 这两种形态都与 TS 逐字一致，是冻结行为，不要“顺手修复”。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 18 | `crates/kiana-codemode/src/declarations.rs:57` | D | 逐字节一致 | //! （到数千层才抛），这里从第 128 层起是 `unknown`；≤127 层逐字节一致（黄金样例锁定），真实 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 19 | `crates/kiana-codemode/src/runtime/execution.rs:98` | A | 与TS一致 | //! 与 TS 一致，只有这个畸形值本身无法透传）。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 20 | `crates/kiana-codemode/src/runtime/execution.rs:1598` | A | 与TS一致, TS: | /// TS: `execution.ts#CodemodeSandboxBase.constructor`。顺序与 TS 一致：先注册 tools， | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 21 | `crates/kiana-codemode/src/runtime/execution.rs:1703` | A | TS等价物, TS的 | /// 载荷），因此这里显式接收。`reason` 为 [`None`] 等价于 TS 的无参 `abort()`：Node/Bun | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 22 | `crates/kiana-codemode/src/runtime/execution.rs:83` | D | 逐字一致 | //! 保证，其余三条逐字保留。 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 23 | `crates/kiana-codemode/src/runtime/remote.rs:95` | A | TS等价物, TS的 | //! - `exchange` future 的 panic 等价于 TS 的 Promise 拒绝：远端把它当作 `end(undefined)` | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 24 | `crates/kiana-codemode/src/runtime/remote.rs:269` | A | TS等价物, TS的 | /// - future 的 panic 等价于 TS 的 Promise 拒绝；`serve_codemode_remote` 的分发器用 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 25 | `crates/kiana-codemode/src/runtime/remote.rs:522` | A | TS等价物, TS的 | // 拒绝（launcher 的 Err 与 panic 都等价于 TS 的 Promise 拒绝）：`end({type:"failure", | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 26 | `crates/kiana-codemode/tests/execution.rs:1513` | A | 逐字一致 | // 与 TS 实测逐字一致（`deterministic.test.ts` 的 `deliver(1, NaN, …)`）。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 27 | `crates/kiana-codemode/tests/execution.rs:1582` | A | 与TS一致 | // 消息的线格式（JSON）与 TS 一致。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 28 | `crates/kiana-codemode/tests/execution.rs:1979` | A | 必须与TS, 与TS一致 | /// 这些都不是 spec §7.7 的 7 个坏桥接用例，但文本必须与 TS 一致。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 29 | `crates/kiana-codemode/src/runtime/protocol.rs:11` | A | 与TS一致, 完全一致 | //! `#[serde(tag = "type", rename_all = "lowercase")]`，成员名与 TS 完全一致。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 30 | `crates/kiana-codemode/src/runtime/protocol.rs:175` | A | TS等价物, TS的 | /// （`worker_data.code` 等价于 TS 的 `workerData.code`）。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 31 | `crates/kiana-codemode/src/runtime/worker.rs:14` | A | 与TS一致 | //! # 顺序（与 TS 一致，逐条） | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 32 | `crates/kiana-codemode/src/runtime/worker.rs:470` | D | 完全一致 | /// 完全一致。测试用它注入会 panic / 会卡住的实现，而不污染 [`set_start_vm`] 的全局单例 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 33 | `crates/kiana-codemode/src/types.rs:13` | A | 必须与TS, TS的 | //! 协议的线上形状，写出的字节必须与 TS 的 `JSON.stringify` 一致： | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 34 | `crates/kiana-codemode/src/types.rs:10` | D | 保真 | //! # JSON 文本保真（`ARCHITECTURE.md` §4.3 第 3、4 条） | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 35 | `crates/kiana-codemode/src/wasm.rs:752` | A | TS等价物, TS的 | // 3. 缓存语义等价于 TS 的 Promise 缓存：并发成功只编译一次、并发失败共享同一次尝试、 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 36 | `crates/kiana-codemode/src/wasm.rs:837` | D | 逐字一致 | // 实测 `readFile("/etc/passwd/quickjs.wasm")` 给出逐字相同的这一行）。 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 37 | `crates/kiana-codemode/src/lib.rs:96` | A | 必须与TS, 与TS一致 | //! 必须与 TS 一致；声明文本与错误消息也按 TS 逐字复核，各模块文档列明已知差异与锁定测试。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 38 | `crates/kiana-codemode/src/runtime/host.rs:518` | D | 逐字一致 | /// `ERR_WORKER_PATH(specifier)` 的逐字文本（node 26 的 `internal/errors`）。 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 39 | `crates/kiana-codemode/src/runtime/prelude_source.rs:7` | A | 必须与TS, 逐字一致 | //! ARCHITECTURE.md §8 第 22 条与 spec 附录 A），因此本模块的常量必须与 TS 逐字一致。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 40 | `crates/kiana-codemode/src/source.rs:67` | D | 逐字一致 | /// Rust 侧只逐字保留文本（不解析它）。注意首字符是换行、缩进为 TAB、行尾为 `\n`。 | 人工判断：确认是否仍有独立技术原因；没有则删除 |

## 完成后返回

返回本组改写的条目数、删除的条目数、保留的条目数，以及任何需要人工确认的 D 类条目。
