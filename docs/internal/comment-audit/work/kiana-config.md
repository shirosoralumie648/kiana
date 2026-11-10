# 注释清理工作单：kiana-config

- 待处理条目：39
- 涉及文件：17

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

- `crates/kiana-config/src/auth/storage.rs`
- `crates/kiana-config/src/cache/warmer.rs`
- `crates/kiana-config/src/models/catalog_http.rs`
- `crates/kiana-config/src/models/composer.rs`
- `crates/kiana-config/src/models/config_file.rs`
- `crates/kiana-config/src/models/runtime.rs`
- `crates/kiana-config/src/resolve_config_value.rs`
- `crates/kiana-config/src/settings/mod.rs`
- `crates/kiana-config/src/settings/schema.rs`
- `crates/kiana-config/src/trust/manager.rs`
- `crates/kiana-config/src/trust/project_trust.rs`
- `crates/kiana-config/tests/auth_storage.rs`
- `crates/kiana-config/tests/config_schemas.rs`
- `crates/kiana-config/tests/config_value_migration.rs`
- `crates/kiana-config/tests/model_catalog_protocol.rs`
- `crates/kiana-config/tests/resolve_config_value.rs`
- `crates/kiana-config/tests/settings_manager_bug.rs`

## 待处理条目

| # | 位置 | 类别 | 命中 | 摘要 | 建议 |
|---:|---|---|---|---|---|
| 1 | `crates/kiana-config/src/auth/storage.rs:65` | A | 与TS一致 | //! `ReadOnlyAuthStorage` 包装后的错误只保留 message（与 TS 一致），`error_code` 为 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 2 | `crates/kiana-config/src/auth/storage.rs:1091` | A | 与TS一致 | /// `withLock(fn)`（同步路径直接读写；不经过异步串行链，与 TS 一致）。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 3 | `crates/kiana-config/src/auth/storage.rs:1413` | A | TS等价物 | /// **test hook, not public API**：该路径已执行的存储读取次数（等价于 TS 测试里 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 4 | `crates/kiana-config/src/auth/storage.rs:1675` | A | 与TS一致 | /// （与 TS 一致），因此下一次带路径读会重新比对并重载。任何错误（解析、回调、加锁、 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 5 | `crates/kiana-config/src/cache/warmer.rs:267` | A | 必须与TS, 逐字一致, TS的 | /// 哪个字段会同时影响单价档位的选择，所以调用点必须与 TS 的字段布局逐字一致 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 6 | `crates/kiana-config/src/cache/warmer.rs:494` | A | TS等价物, TS的 | /// - `getBranch()` 不传参数：`get_branch(None)` 等价于 TS 的 `getBranch()`（从当前 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 7 | `crates/kiana-config/src/cache/warmer.rs:1189` | A | 与TS一致 | /// 与 TS 一致：三个价格都按同一个 prompt 规模算—— | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 8 | `crates/kiana-config/src/models/catalog_http.rs:436` | A | TS等价物, TS的 | /// 2. 必须包含 `id` 键（值为 `null` 也算，等价于 TS 的 `"id" in entry`）； | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 9 | `crates/kiana-config/src/models/catalog_http.rs:1242` | A | 必须与TS | /// 返回值直接参与 `remote_catalog::merge_models` 的键 `"{type}\0{id}"`，必须与 TS | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 10 | `crates/kiana-config/src/models/composer.rs:1144` | A | 必须与TS, 逐字一致 | // image/classifier 没有这个后缀；拼出的整句必须与 TS 逐字一致。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 11 | `crates/kiana-config/src/models/composer.rs:1238` | A | 逐字一致 | // 里）。错误消息与 TS 逐字一致。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 12 | `crates/kiana-config/src/models/config_file.rs:791` | A | 与TS一致 | /// id 里含 `.`、`/` 时，输出路径会有歧义。这与 TS 一致，错误的黄金测试依赖它。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 13 | `crates/kiana-config/src/resolve_config_value.rs:961` | A | 与TS一致 | /// 注意它调用的是 [`resolve_config_value_uncached`]（与 TS 一致）：命令每次都会真的 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 14 | `crates/kiana-config/src/settings/mod.rs:27` | A | 与TS一致 | //! 真假判断/算术时结果与 TS 一致。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 15 | `crates/kiana-config/src/settings/mod.rs:862` | A | 与TS一致 | /// `migrate_settings`，所以保存会顺带把旧格式迁移成新格式写回（与 TS 一致）。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 16 | `crates/kiana-config/src/settings/mod.rs:1261` | A | 与TS一致 | /// 加载之间无条件清空（与 TS 一致）：此前写失败遗留的「待重试」标记也会一起丢弃。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 17 | `crates/kiana-config/src/settings/schema.rs:8` | A | 逐字一致 | //! - **类型化字段**：字段名、声明顺序、JSON 名称与 `settings-schema.ts` 逐字一致 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 18 | `crates/kiana-config/src/trust/manager.rs:53` | A | 与TS一致 | //! （与 TS 一致，测试逐字断言）； | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 19 | `crates/kiana-config/src/trust/project_trust.rs:171` | A | TS等价物 | /// `emit_project_trust` 为 `None` 等价于 TS 没有 `extensionsResult`：跳过扩展事件。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 20 | `crates/kiana-config/tests/config_schemas.rs:588` | A | 必须与TS, 逐字一致 | // 枚举的 serde 表示必须与 TS 字符串逐字相同。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 21 | `crates/kiana-config/tests/model_catalog_protocol.rs:181` | A | 与TS一致 | // 与 TS 一致：只断言 pathname（Anthropic 兼容端点可能附加查询参数，如 `?beta=true`）。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 22 | `crates/kiana-config/tests/resolve_config_value.rs:287` | A | 逐字一致 | // 以下为补充的规范测试（TS 测试文件未直接覆盖，但 §2.11.3/§2.11.4 要求逐字一致）。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 23 | `crates/kiana-config/src/auth/storage.rs:28` | D | 逐字一致 | //! - 写路径在锁内重新解析磁盘内容、只改目标 provider，其余键（含未知形状、键序）逐字保留； | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 24 | `crates/kiana-config/src/auth/storage.rs:198` | D | 逐字一致 | /// `JSON.parse` 逐字一致（[`kiana_json::js_parse`]）。 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 25 | `crates/kiana-config/src/cache/warmer.rs:812` | D | 逐字一致 | /// 真正的清理由 `on_mode_changed` 负责。判定顺序与 spec 逐字一致： | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 26 | `crates/kiana-config/src/cache/warmer.rs:1606` | D | 逐字一致 | /// 输出模板逐字对应 spec §3.7，四种形态：`Inactive (reason)`、`Stopped (details)`、 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 27 | `crates/kiana-config/src/models/catalog_http.rs:98` | D | 逐字一致 | /// 条目，因此同时给出强类型投影 [`Self::models`] 与逐字保留的 [`Self::raw_models`] | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 28 | `crates/kiana-config/src/models/catalog_http.rs:1131` | D | 逐字一致 | /// 先读头：`Last-Modified` 缺失、非法或为空时 `last_modified = 0`；`ETag` 逐字保留 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 29 | `crates/kiana-config/src/models/composer.rs:22` | D | 逐字一致 | //! - `withConfiguredAuth` 的 `Authorization: Bearer <key>` 键名逐字保留大写 `A`。 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 30 | `crates/kiana-config/src/models/composer.rs:128` | D | 逐字一致 | /// 方法名与字段适配逐字对应 `adaptOAuth`：`onAuth` → `auth_url`，`onDeviceCode` → | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 31 | `crates/kiana-config/src/models/composer.rs:498` | D | 逐字一致 | /// 逐字一致；`Runtime` 由 `ModelRuntime` 在运行时 key 优先时填充，`Stored` 来自凭证存储， | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 32 | `crates/kiana-config/src/models/runtime.rs:155` | D | 完全一致 | /// [`AuthResolutionOverrides`] 完全一致（`apiKey`/`env`/`minOAuthValidityMs`/`signal`）。 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 33 | `crates/kiana-config/src/settings/mod.rs:1196` | D | 完全一致 | /// 相同的 JSON 形态再交给后端，后续读取路径因此与文件后端完全一致。两个 `expect` | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 34 | `crates/kiana-config/src/trust/project_trust.rs:9` | D | 强约束 | //! 判定顺序（不得改变）： | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 35 | `crates/kiana-config/tests/auth_storage.rs:1328` | D | 逐字一致 | /// JSON.parse 的错误文案与 V8 逐字一致（不是 serde_json 的文案）。 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 36 | `crates/kiana-config/tests/auth_storage.rs:1485` | D | 逐字一致 | /// 未知形状的条目在修改其它 provider 时逐字保留（auth.json 不因未知 type 丢数据）。 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 37 | `crates/kiana-config/tests/auth_storage.rs:1567` | D | 逐字一致 | // 其它无法表达的条目在写回时逐字保留。 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 38 | `crates/kiana-config/tests/config_value_migration.rs:137` | D | 逐字一致 | // 迁移/读取流程不触碰 models.json：内容逐字保留。 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 39 | `crates/kiana-config/tests/settings_manager_bug.rs:10` | D | 完全一致 | //! 断言落盘字节完全一致。 | 人工判断：确认是否仍有独立技术原因；没有则删除 |

## 完成后返回

返回本组改写的条目数、删除的条目数、保留的条目数，以及任何需要人工确认的 D 类条目。
