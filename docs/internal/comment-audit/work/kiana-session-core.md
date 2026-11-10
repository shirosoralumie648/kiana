# 注释清理工作单：kiana-session-core

- 待处理条目：29
- 涉及文件：11

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

- `crates/kiana-session-core/src/cache_stats.rs`
- `crates/kiana-session-core/src/lib.rs`
- `crates/kiana-session-core/src/messages.rs`
- `crates/kiana-session-core/src/session_export.rs`
- `crates/kiana-session-core/src/session_manager.rs`
- `crates/kiana-session-core/src/virtual_models.rs`
- `crates/kiana-session-core/tests/messages.rs`
- `crates/kiana-session-core/tests/session_export.rs`
- `crates/kiana-session-core/tests/session_info_modified_timestamp.rs`
- `crates/kiana-session-core/tests/session_manager_migration.rs`
- `crates/kiana-session-core/tests/session_manager_save_entry.rs`

## 待处理条目

| # | 位置 | 类别 | 命中 | 摘要 | 建议 |
|---:|---|---|---|---|---|
| 1 | `crates/kiana-session-core/src/cache_stats.rs:213` | A | 完全一致, TS里 | /// 缺失、JSON `null` 或非数字都返回 `NaN`。注意这与 TS 不完全一致：TS 里**缺失**字段是 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 2 | `crates/kiana-session-core/src/cache_stats.rs:294` | A | 与TS一致 | /// 返回 `None` 的情况（任一条件成立即提前返回，顺序与 TS 一致）： | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 3 | `crates/kiana-session-core/src/lib.rs:98` | A | 必须与TS, 逐字节一致 | //! - 写盘必须与 TS `JSON.stringify` 逐字节一致（字段顺序、数字格式、`undefined` 键省略）。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 4 | `crates/kiana-session-core/src/messages.rs:44` | A | 必须与TS, 逐字一致 | //! - 四个 role 判别式的 serde 重命名必须与 TS 字面量逐字一致：`AgentMessage::role()` | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 5 | `crates/kiana-session-core/src/messages.rs:677` | A | 与TS一致 | // 本身始终保留，保证条数与 TS 一致。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 6 | `crates/kiana-session-core/src/session_export.rs:11` | A | 必须与TS, TS的 | //! 每一行必须与 TS 的 `JSON.stringify` **逐字节**一致：未知字段、条目顺序与对象键序原样 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 7 | `crates/kiana-session-core/src/session_export.rs:77` | A | TS等价物, TS的 | /// 会沿用该原始值（`id` 缺失则删除键，等价于 TS 的 `undefined` 被省略）。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 8 | `crates/kiana-session-core/src/session_manager.rs:124` | A | 逐字一致, TS的 | /// 会话数据层错误。`Display` 与 TS 的 `error.message` 逐字一致。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 9 | `crates/kiana-session-core/src/session_manager.rs:1929` | A | TS等价物, TS的 | /// 等价于 TS 的正则 `^[A-Za-z0-9](?:[A-Za-z0-9._-]*[A-Za-z0-9])?$`：非空、首尾必须是 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 10 | `crates/kiana-session-core/src/session_manager.rs:1931` | A | 与TS一致 | /// （如 `é`）一律拒绝；失败返回 [`SessionManagerError::InvalidSessionId`]（文案与 TS 一致）。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 11 | `crates/kiana-session-core/src/session_manager.rs:2772` | A | 与TS一致 | /// 从 leaf 回溯到 root，返回 root→leaf 顺序的路径。`leaf_id` 语义与 TS 一致：`Null` | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 12 | `crates/kiana-session-core/src/session_manager.rs:3006` | A | 与TS一致 | /// [`AgentMessage::Custom`] 回放原始对象（键序与额外键与 TS 一致）；通过 append API 新建的 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 13 | `crates/kiana-session-core/src/session_manager.rs:3184` | A | 与TS一致 | /// 真值判断与 TS 一致（`null`/`false`/`0`/`""` 为假）；对象原样返回（额外键与键序保留）。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 14 | `crates/kiana-session-core/src/session_manager.rs:3676` | A | 与TS一致 | /// 扫描顺序与 TS 一致：首个真值行必须是 `{type:"session"}` 对象，否则整个文件返回 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 15 | `crates/kiana-session-core/src/virtual_models.rs:532` | A | 与TS一致 | /// 默认值与 TS 一致（tests/virtual_models.rs 锁定）：`thinking_levels` 缺省为 `["off"]`； | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 16 | `crates/kiana-session-core/tests/messages.rs:37` | A | 逐字一致 | /// 摘要前缀/后缀与 TS 模板字符串逐字一致（注意换行的不对称）。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 17 | `crates/kiana-session-core/tests/session_info_modified_timestamp.rs:64` | A | 与TS一致 | /// 回归（评审 #6）：`buildSessionInfo` 对畸形会话文件的处理与 TS 一致： | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 18 | `crates/kiana-session-core/tests/session_info_modified_timestamp.rs:156` | A | 与TS一致, 完全一致 | // 与 TS 完全一致：a/b/c/d/f/g 被丢弃；i（message "oops"）保留但没有文本。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 19 | `crates/kiana-session-core/tests/session_manager_migration.rs:133` | A | 逐字一致, TS的 | /// 未建模的条目类型、以及键位置都逐字保留（TS 的 `_rewriteFile` 行为）。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 20 | `crates/kiana-session-core/tests/session_manager_save_entry.rs:5` | A | 与TS一致, 完全一致 | //! 每个已知条目类型都必须 `parse(serialize(x)) == x` 且写出字节与 TS 完全一致。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 21 | `crates/kiana-session-core/tests/session_manager_save_entry.rs:13` | A | 与TS一致 | /// 黄金夹具逐行回环：`JSON.stringify(entry)` 的字节与 TS 一致。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 22 | `crates/kiana-session-core/src/lib.rs:83` | D | 逐字一致 | //! 逐字一致，但只有经内部 `node_fs_error` 包装的打开/创建/mkdir 错误才按 Node 的 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 23 | `crates/kiana-session-core/src/session_export.rs:71` | D | 保真 | /// # 保真约束 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 24 | `crates/kiana-session-core/src/session_manager.rs:2177` | D | 强约束 | /// `migrate_hook_message_role` 同时改 `raw.message.role` 与 typed 消息，二者必须一致， | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 25 | `crates/kiana-session-core/src/session_manager.rs:5640` | D | 逐字一致 | /// 逐字保留。这里返回与 [`SessionManager::get_branch`] 相同路径的原始值。 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 26 | `crates/kiana-session-core/src/session_manager.rs:5802` | D | 保真 | /// 边界）。需要逐字保真时用 [`SessionManager::get_entries_json`]。 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 27 | `crates/kiana-session-core/src/session_manager.rs:5811` | D | 逐字一致 | /// 逐字保留（见模块文档）。typed [`SessionEntry`] 无法承载未建模字段，因此导出这类 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 28 | `crates/kiana-session-core/src/session_manager.rs:5812` | D | 逐字节一致 | /// 需要逐字节一致的消费方必须用本方法。 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 29 | `crates/kiana-session-core/tests/session_export.rs:167` | D | 逐字一致 | /// `{ ...entry, parentId }`——未知字段、未建模条目类型与键位置逐字保留。 | 人工判断：确认是否仍有独立技术原因；没有则删除 |

## 完成后返回

返回本组改写的条目数、删除的条目数、保留的条目数，以及任何需要人工确认的 D 类条目。
