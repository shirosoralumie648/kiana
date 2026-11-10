# 注释清理工作单：kiana-resources

- 待处理条目：32
- 涉及文件：23

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

- `crates/kiana-resources/src/bug_report.rs`
- `crates/kiana-resources/src/footer_data_provider.rs`
- `crates/kiana-resources/src/lib.rs`
- `crates/kiana-resources/src/loader/context_files.rs`
- `crates/kiana-resources/src/loader/mod.rs`
- `crates/kiana-resources/src/loader/source_info_map.rs`
- `crates/kiana-resources/src/loader/themes.rs`
- `crates/kiana-resources/src/package/discover.rs`
- `crates/kiana-resources/src/package/mod.rs`
- `crates/kiana-resources/src/package/npm.rs`
- `crates/kiana-resources/src/package/paths.rs`
- `crates/kiana-resources/src/package/patterns.rs`
- `crates/kiana-resources/src/package/settings_io.rs`
- `crates/kiana-resources/src/package/types.rs`
- `crates/kiana-resources/src/skills/discover.rs`
- `crates/kiana-resources/src/skills/mod.rs`
- `crates/kiana-resources/src/skills/prompt.rs`
- `crates/kiana-resources/src/skills/validate.rs`
- `crates/kiana-resources/src/system_prompt.rs`
- `crates/kiana-resources/tests/package_manager.rs`
- `crates/kiana-resources/tests/prompt_templates.rs`
- `crates/kiana-resources/tests/resource_loader.rs`
- `crates/kiana-resources/tests/resource_loader_theme.rs`

## 待处理条目

| # | 位置 | 类别 | 命中 | 摘要 | 建议 |
|---:|---|---|---|---|---|
| 1 | `crates/kiana-resources/src/bug_report.rs:1414` | A | 与TS一致 | /// 字段顺序与 TS 一致：`id`、`createdAt`、`hint`、`sessionIncluded`、`summaryIncluded`、 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 2 | `crates/kiana-resources/src/bug_report.rs:2099` | A | 与TS一致 | // prompt 顺序与 TS 一致：可选的行数提示、<conversation>、可选 <user-report>、指令。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 3 | `crates/kiana-resources/src/bug_report.rs:2166` | A | 与TS一致 | // 错误优先级与 TS 一致：取消 → Error/Length → toolCall → 空文本。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 4 | `crates/kiana-resources/src/lib.rs:21` | A | 与TS一致 | //! 文案，并且无法保证加载顺序与 TS 一致。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 5 | `crates/kiana-resources/src/loader/mod.rs:1875` | A | 与TS一致 | /// `errors`。`hidden` / `replaceable` 的映射与 TS 一致：匿名扩展为 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 6 | `crates/kiana-resources/src/loader/source_info_map.rs:330` | A | 与TS一致 | /// `... stat '<path>'` 形式，使 `ResourceLoaderError::Io` 的 Display 与 TS 一致。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 7 | `crates/kiana-resources/src/loader/source_info_map.rs:389` | A | 与TS一致 | // `io::Error`，`ResourceLoaderError::Io` 的 Display 便与 TS 一致。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 8 | `crates/kiana-resources/src/loader/themes.rs:181` | A | 逐字一致, TS的, 对应TS | /// 解析失败 → `warning`，message 是 `kiana-theme` 的 `ThemeError` 文案（逐字对应 TS 的 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 9 | `crates/kiana-resources/src/package/discover.rs:10` | A | 逐字一致 | //! （`toPosixPath` / `prefixIgnorePattern` / `addIgnoreRules`，与 `skills.ts` 逐字相同）。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 10 | `crates/kiana-resources/src/package/discover.rs:20` | A | TS等价物 | //! 目录的规则共存；由于每条规则都带自己的根相对前缀，这等价于 TS 中一个 `ignore()` | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 11 | `crates/kiana-resources/src/package/mod.rs:17` | A | 与TS一致 | //! [`DefaultPackageManager::check_for_available_updates`]（与 TS 一致，不在接口里）。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 12 | `crates/kiana-resources/src/package/mod.rs:704` | A | 与TS一致 | /// 不在 [`PackageManager`] 接口里（与 TS 一致），调用方需要具体类型。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 13 | `crates/kiana-resources/src/package/npm.rs:178` | A | 逐字一致 | /// 判定顺序（与 TS `getPackageManagerName` 逐字对应）： | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 14 | `crates/kiana-resources/src/package/patterns.rs:2476` | A | 与TS一致 | // `**` 可以匹配零个目录，但 `.ignored.ts` 以 `.` 开头，仍被过滤（与 TS 一致）。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 15 | `crates/kiana-resources/src/package/settings_io.rs:29` | A | 保真 | //! # TS 保真 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 16 | `crates/kiana-resources/src/package/settings_io.rs:31` | A | 逐字一致, TS的 | //! `Cannot read properties of undefined (reading 'startsWith')`）逐字对应 V8/TS 的抛错， | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 17 | `crates/kiana-resources/src/package/settings_io.rs:460` | A | 逐字一致, 对应TS | /// 否则为 `No matching package found for <source>`（标点与大小写逐字对应 TS）。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 18 | `crates/kiana-resources/src/package/types.rs:13` | A | 与TS一致 | //! 不新增字段。字段顺序与 TS 一致：`source`、`scope`、`origin`、`baseDir`、`packageRoot`。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 19 | `crates/kiana-resources/src/skills/discover.rs:353` | A | 与TS一致 | /// 判定顺序（与 TS 一致）： | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 20 | `crates/kiana-resources/src/skills/mod.rs:156` | A | 与TS一致 | /// `file_path`，两个 `source` 字段留空（与 TS 一致）。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 21 | `crates/kiana-resources/src/skills/prompt.rs:111` | A | 与TS一致 | /// “先替换 `&`”的链式写法。`'` 也会转成 `&apos;`，与 TS 一致（XML 文本节点里并非必需）。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 22 | `crates/kiana-resources/src/skills/validate.rs:8` | A | 逐字节一致 | //! 上限与错误消息都要和 TS 逐字节一致。JS 的 `string.length` 数的是 UTF-16 code unit， | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 23 | `crates/kiana-resources/src/system_prompt.rs:150` | A | 与TS一致 | /// 连接。`path` 与 `content` 原样拼入，**不做 XML 转义**（与 TS 一致）；内容里若含 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 24 | `crates/kiana-resources/src/system_prompt.rs:296` | A | 强约束 | // 多个非法名字同时存在时，TS 报第一个；顺序必须一致。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 25 | `crates/kiana-resources/tests/package_manager.rs:3` | A | 逐字一致, TS里 | //! 描述块与 `it` 标题逐字保留。TS 里通过 `vi.spyOn` 覆盖的私有方法，这里用 PATH 注入的 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 26 | `crates/kiana-resources/tests/prompt_templates.rs:3` | A | 与TS一致 | //! describe 分布与 TS 一致：`substituteArgs`（37）、`- positional defaults`（10）、 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 27 | `crates/kiana-resources/tests/resource_loader_theme.rs:5` | A | 逐字一致 | //! `detectCapabilities(() => false)` 的结果上（与 TS 逐字一致）。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 28 | `crates/kiana-resources/src/footer_data_provider.rs:525` | D | 完全一致 | /// 行为与生产路径的 watcher 错误回调完全一致，仅用于测试。 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 29 | `crates/kiana-resources/src/loader/context_files.rs:41` | D | 逐字一致 | /// 每个目录的候选文件（顺序固定，大小写变体逐字保留）。 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 30 | `crates/kiana-resources/src/package/paths.rs:1562` | D | 逐字一致 | // 的实际行为一致（待核实：是否与某组官方样例逐字对应，仓库内未找到出处），作为 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 31 | `crates/kiana-resources/src/system_prompt.rs:461` | D | 完全一致 | /// 构造系统提示文本（与 transcript 回放 system 消息的渲染完全一致）。 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 32 | `crates/kiana-resources/tests/resource_loader.rs:3` | D | 逐字一致 | //! 测试标题逐字保留。少数断言依赖 `ExtensionRunner` 的命令重名解析（`deploy:1` / | 人工判断：确认是否仍有独立技术原因；没有则删除 |

## 完成后返回

返回本组改写的条目数、删除的条目数、保留的条目数，以及任何需要人工确认的 D 类条目。
