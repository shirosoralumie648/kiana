# 注释清理工作单：kiana-ai--part-5

- 待处理条目：40
- 涉及文件：13

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

- `crates/kiana-ai/build.rs`
- `crates/kiana-ai/src/api/classifier_shared.rs`
- `crates/kiana-ai/src/auth/oauth/callback_server.rs`
- `crates/kiana-ai/src/auth/oauth/kimi_coding.rs`
- `crates/kiana-ai/src/compat/extension_oauth_types.rs`
- `crates/kiana-ai/src/env_api_keys.rs`
- `crates/kiana-ai/src/providers/catalog.rs`
- `crates/kiana-ai/src/providers/model_schema.rs`
- `crates/kiana-ai/src/utils/identity.rs`
- `crates/kiana-ai/src/utils/model_operations.rs`
- `crates/kiana-ai/src/utils/node_http_proxy.rs`
- `crates/kiana-ai/src/utils/provider_retry.rs`
- `crates/kiana-ai/src/utils/retry.rs`

## 待处理条目

| # | 位置 | 类别 | 命中 | 摘要 | 建议 |
|---:|---|---|---|---|---|
| 1 | `crates/kiana-ai/src/utils/model_operations.rs:9` | A | 与TS一致 | //! 保证 `code`/`message` 与 TS 一致（`"provider"` 对应 `ModelsErrorCode::Provider`）。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 2 | `crates/kiana-ai/src/utils/model_operations.rs:17` | A | 逐字一致 | //! 文本、`code` 与 `name` 均与 TS 逐字一致；下游若按 TS 文本做断言，可继续成立。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 3 | `crates/kiana-ai/src/utils/model_operations.rs:76` | A | 逐字一致, TS的 | /// chat model", cause_detail: None }`；文本与 TS 的 `throw` 逐字一致，`provider`/`id` | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 4 | `crates/kiana-ai/src/utils/model_operations.rs:6` | D | 逐字一致 | //! `Result`（`assertChatModel` 抛的 `ModelsError` 逐字保留），调用方用 `?` 传播。 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 5 | `crates/kiana-ai/src/utils/node_http_proxy.rs:104` | A | 与TS一致 | /// `port == 0` 表示该记录未指定端口。规则与 TS 一致： | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 6 | `crates/kiana-ai/src/utils/node_http_proxy.rs:305` | A | 逐字一致 | /// 文本与 TS 逐字一致；`tests/node_http_proxy.rs` 断言错误里包含它，改动会破坏对外 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 7 | `crates/kiana-ai/src/utils/node_http_proxy.rs:311` | A | 逐字一致, TS的, 对应TS | /// 返回 `Ok(None)` 表示不用代理；`Err` 的文本逐字对应 TS 的两条 `Error`。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 8 | `crates/kiana-ai/src/utils/node_http_proxy.rs:351` | D | 逐字一致 | // 因此下面的错误格式串在 `{}` 之后补了一个 `:`，以保持逐字一致。 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 9 | `crates/kiana-ai/src/utils/retry.rs:50` | A | 与TS一致 | /// `NON_RETRYABLE_PROVIDER_LIMIT_ERROR_PATTERN` 的分支（顺序与 TS 一致）。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 10 | `crates/kiana-ai/src/utils/retry.rs:64` | A | 与TS一致, TS源 | /// `RETRYABLE_PROVIDER_ERROR_PATTERN` 的分支（顺序与 TS 一致，逐条注释见 TS 源）。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 11 | `crates/kiana-ai/src/utils/retry.rs:426` | A | 与TS一致 | /// 判定顺序（与 TS 一致）： | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 12 | `crates/kiana-ai/src/utils/retry.rs:228` | D | 强约束 | /// 回调不能改变重试结果，也不能把错误传回循环。 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 13 | `crates/kiana-ai/build.rs:14` | A | 完全一致, TS的 | //! 与 TS 的 `Object.fromEntries` / `Object.keys(MODELS)` 完全一致。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 14 | `crates/kiana-ai/build.rs:128` | D | 完全一致 | // manifest 的键集合必须与目录里的数据文件集合完全一致（多一个少一个都报错）。 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 15 | `crates/kiana-ai/build.rs:374` | D | 逐字一致 | /// 逐字一致（`${providerId}/${modelId} ...`）。`model_id` 是 `type:id` 键里冒号之后的部分 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 16 | `crates/kiana-ai/src/auth/oauth/callback_server.rs:659` | A | 与TS一致 | // 与 TS 一致：预取消在任何 socket 创建之前就失败（spec/ai-auth.md §2.8 步骤 1； | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 17 | `crates/kiana-ai/src/auth/oauth/callback_server.rs:5` | D | 逐字一致 | //! §6.2 #20：测试只断言状态码与 `content-type`，头部集合无需与 Node 逐字一致）。 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 18 | `crates/kiana-ai/src/auth/oauth/callback_server.rs:265` | D | 逐字一致 | /// （spec/ai-auth.md §6.2 #20），不要求与 Node 的头部集合逐字一致。`content-length` 是 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 19 | `crates/kiana-ai/src/auth/oauth/kimi_coding.rs:29` | A | 逐字一致 | //! - 错误文案与 TS 逐字一致：`tests/kimi_coding_oauth.rs` 对 expired/denied/字段缺失等 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 20 | `crates/kiana-ai/src/auth/oauth/kimi_coding.rs:471` | A | 与TS一致 | // 退避只发生在重试前：1s、2s、4s（1000 * 2^(attempt-1)），与 TS 一致。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 21 | `crates/kiana-ai/src/auth/oauth/kimi_coding.rs:540` | A | 逐字一致, TS的, 对应TS | // 为了逐字对应 TS 的分支结构。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 22 | `crates/kiana-ai/src/compat/extension_oauth_types.rs:90` | A | TS等价物 | /// 可选字段为 `None` 时序列化会省略对应键，等价于 TS 对象上的 `undefined`；反序列化时缺键 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 23 | `crates/kiana-ai/src/compat/extension_oauth_types.rs:228` | A | 逐字一致 | /// 与 TS `adaptOAuth` 逐字一致（spec/ai-auth.md §A.9）。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 24 | `crates/kiana-ai/src/compat/extension_oauth_types.rs:4` | D | 逐字一致 | //! 这些类型是**扩展作者可见的问题面**，字段名与可选性必须逐字保留；它们由 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 25 | `crates/kiana-ai/src/env_api_keys.rs:38` | A | 与TS一致 | //! - Vertex ADC 的探测结果缓存在进程级 `VERTEX_ADC_CACHE`（`Mutex<Option<bool>>`）。与 TS 一致， | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 26 | `crates/kiana-ai/src/env_api_keys.rs:71` | A | 必须与TS, 逐字一致 | // 同一组名字。变量名必须与 TS 逐字一致（用户文档与报错会展示它们）。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 27 | `crates/kiana-ai/src/env_api_keys.rs:103` | A | 与TS一致 | /// 分两条路径（与 TS 一致）： | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 28 | `crates/kiana-ai/src/providers/catalog.rs:28` | A | 与TS一致 | //! 因此三层与 TS 一致： | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 29 | `crates/kiana-ai/src/providers/catalog.rs:57` | D | 逐字一致 | //! 本模块返回 `&Value`，逐字保留目录条目的全部键（radius 的 `lab`/`enabled`/`providers` | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 30 | `crates/kiana-ai/src/providers/catalog.rs:280` | D | 完全一致 | // 测试锁定三类不变量：provider 顺序与 spec/ai-core.md §2.3.1 完全一致且三张表各有 42 个 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 31 | `crates/kiana-ai/src/providers/model_schema.rs:18` | A | 与TS一致, 完全一致 | //! 与 TS 导出的 fixture 对照；需要与 TS 完全一致的补全结果时应使用它，而不是自行遍历 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 32 | `crates/kiana-ai/src/providers/model_schema.rs:7` | D | 完全一致 | //! 的运行时形状与 TypeBox 1.3 完全一致（见 `tests/fixtures/providers/schemas.json`）。 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 33 | `crates/kiana-ai/src/providers/model_schema.rs:20` | D | 强约束 | //! （不是写 `null`），record 的递归条件与 union 的分支选择也必须一致。 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 34 | `crates/kiana-ai/src/utils/identity.rs:265` | A | 与TS一致 | /// 分支优先级与 TS 一致： | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 35 | `crates/kiana-ai/src/utils/identity.rs:272` | D | 逐字一致 | /// UA 字符串，`spec/ca-cli.md` §2.21.4 也要求默认格式逐字一致。 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 36 | `crates/kiana-ai/src/utils/identity.rs:577` | D | 逐字一致 | /// 两个包装器（`IdentityModels` / `IdentityMutableModels`）的 `Models` 实现逐字相同， | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 37 | `crates/kiana-ai/src/utils/provider_retry.rs:102` | A | TS等价物, TS的 | /// 所有字段都可省略；`Default::default()` 等价于 TS 的 `{}`——只请求一次、上限 60 秒、 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 38 | `crates/kiana-ai/src/utils/provider_retry.rs:122` | A | 与TS一致 | /// 判定顺序（与 TS 一致，靠前的规则优先）： | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 39 | `crates/kiana-ai/src/utils/provider_retry.rs:159` | A | 逐字一致 | // 严格大于才拒绝：恰好等于上限的延迟可以等。错误文本与 TS 模板逐字一致。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 40 | `crates/kiana-ai/src/api/classifier_shared.rs:9` | A | TS等价物, TS的 | //! - TS 的“抛 `Error`”变成 [`ClassifierHttpError`]（带 HTTP 字段，等价于 TS 的 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |

## 完成后返回

返回本组改写的条目数、删除的条目数、保留的条目数，以及任何需要人工确认的 D 类条目。
