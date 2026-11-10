# 注释清理工作单：kiana-env-client

- 待处理条目：15
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

- `crates/kiana-env-client/src/connection.rs`
- `crates/kiana-env-client/src/lib.rs`
- `crates/kiana-env-client/src/list_files.rs`
- `crates/kiana-env-client/src/ssh.rs`
- `crates/kiana-env-client/tests/conformance.rs`
- `crates/kiana-env-client/tests/remote.rs`
- `crates/kiana-env-client/tests/ssh.rs`
- `crates/kiana-env-client/tests/terminal.rs`

## 待处理条目

| # | 位置 | 类别 | 命中 | 摘要 | 建议 |
|---:|---|---|---|---|---|
| 1 | `crates/kiana-env-client/src/connection.rs:405` | A | 与TS一致 | /// 在 REQUEST 帧入队之前调用一次（顺序与 TS 一致），拿到 id 之后调用方才能取消它。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 2 | `crates/kiana-env-client/src/connection.rs:960` | A | 与TS一致 | // 顺序与 TS 一致：先回调 on_start（调用方拿到 id/session），再把帧入队。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 3 | `crates/kiana-env-client/src/connection.rs:1367` | A | 与TS保持 | // 空 JSON 都算坏帧）。daemon 的帧读取端对解析失败更宽松，这里与 TS 保持一致。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 4 | `crates/kiana-env-client/src/list_files.rs:182` | A | 与TS一致 | // 5. 回复已到但 signal 可能在这期间中止：与 TS 一致，aborted 优先于解析结果。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 5 | `crates/kiana-env-client/src/ssh.rs:2057` | A | 逐字一致 | /// ssh 仍能收到信号做清理；拒绝消息与 TS 逐字相同。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 6 | `crates/kiana-env-client/tests/remote.rs:1` | A | 逐字一致, packages/ | //! `packages/env/test/remote.test.ts` 的移植（9 个用例，名称逐字保留）。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 7 | `crates/kiana-env-client/tests/ssh.rs:1` | A | 逐字一致, packages/ | //! `packages/env/test/ssh.test.ts` 的移植：本机临时 `sshd` 上的 8 个用例（名称逐字保留）。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 8 | `crates/kiana-env-client/tests/terminal.rs:1` | A | 逐字一致, packages/ | //! `packages/env/test/terminal.test.ts` 的移植（7 个用例，名称逐字保留；整个套件仅在非 Windows | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 9 | `crates/kiana-env-client/src/lib.rs:51` | D | 保真 | //! 本文件（crate 根）是模块树加一组跨模块共用的 Node 保真工具：`SharedResult`（一次完成、可多次 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 10 | `crates/kiana-env-client/src/lib.rs:89` | D | 保真 | //! 6. [`list_files`]、[`watch`]、[`terminal`]。本文件其余部分是上述模块共用的保真工具，可最后读。 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 11 | `crates/kiana-env-client/src/lib.rs:451` | D | 逐字一致 | /// 文案会原样进入用户可见的错误消息，必须逐字保留（测试 `filesystem_error_text_matches_node` | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 12 | `crates/kiana-env-client/src/list_files.rs:268` | D | 完全一致 | /// 因此本地读写、bash、watch 的行为与直接使用 `NodeExecutionEnv` 完全一致；`id()` 也返回它的 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 13 | `crates/kiana-env-client/src/ssh.rs:414` | D | 逐字一致 | /// 放进 `RemoteError.message`，所以消息必须逐字一致，不能用 Rust `io::Error` 的 `Display`。 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 14 | `crates/kiana-env-client/src/ssh.rs:1202` | D | 逐字一致 | /// 判断只看每行第一个 JS 空白分隔的字段：等于 `host_key_alias` 的行全部删除，其它行逐字保留 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 15 | `crates/kiana-env-client/tests/conformance.rs:3` | D | 逐字一致 | //! 两个套件（名称逐字保留）：`RemoteExecutionEnv over a pipe` 与 `LocalExecutionEnv`，都用 | 人工判断：确认是否仍有独立技术原因；没有则删除 |

## 完成后返回

返回本组改写的条目数、删除的条目数、保留的条目数，以及任何需要人工确认的 D 类条目。
