# 注释清理工作单：kiana-coding-agent

- 待处理条目：21
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

- `crates/kiana-coding-agent/src/app.rs`
- `crates/kiana-coding-agent/src/cli/auth_command.rs`
- `crates/kiana-coding-agent/src/cli/session_picker.rs`
- `crates/kiana-coding-agent/src/cli/startup_ui.rs`
- `crates/kiana-coding-agent/src/migrations.rs`
- `crates/kiana-coding-agent/src/package_manager_cli.rs`
- `crates/kiana-coding-agent/tests/cli_smoke.rs`
- `crates/kiana-coding-agent/tests/config_value_migration.rs`
- `crates/kiana-coding-agent/tests/regressions_5661_uppercase_header_values.rs`
- `crates/kiana-coding-agent/tests/regressions_7269_cli_end_of_options.rs`
- `crates/kiana-coding-agent/tests/stdout_cleanliness.rs`

## 待处理条目

| # | 位置 | 类别 | 命中 | 摘要 | 建议 |
|---:|---|---|---|---|---|
| 1 | `crates/kiana-coding-agent/src/app.rs:405` | A | 与TS一致 | // 这里用默认构造（默认信任项目，与 TS 一致）创建启动期实例：它服务于首次设置、 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 2 | `crates/kiana-coding-agent/src/app.rs:1450` | A | 与TS一致 | /// 分支顺序与 TS 一致（先命中先返回）： | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 3 | `crates/kiana-coding-agent/src/app.rs:1472` | A | 与TS一致 | // `settings_manager` 只被 `--resume` 选择器使用（与 TS 一致）。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 4 | `crates/kiana-coding-agent/src/cli/auth_command.rs:21` | A | 逐字一致 | //! usage、命令名与帮助文本与 TS 逐字一致（`tests/credential_print.rs` 有 golden 断言）； | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 5 | `crates/kiana-coding-agent/src/cli/session_picker.rs:7` | A | TS等价物, TS的 | //! 回调只把结果送进通道；`ui.stop()` 在驱动循环收到结果后执行（等价于 TS 的同一事件循环 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 6 | `crates/kiana-coding-agent/src/cli/startup_ui.rs:378` | A | 与TS一致, TS里 | /// 与 TS 一致，`resolve()` 的失败会向上传播（TS 里 `loadStartupThemes` 没有 try/catch， | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 7 | `crates/kiana-coding-agent/src/migrations.rs:420` | A | TS等价物, TS的 | // 阶段 1：oauth.json（优先级最高）。解析失败等价于 TS 的 catch，直接跳过；解析成功 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 8 | `crates/kiana-coding-agent/src/package_manager_cli.rs:469` | A | 与TS一致, 完全一致 | /// 进度条。参数与 TS 完全一致，属于外部契约，不能随手增删。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 9 | `crates/kiana-coding-agent/src/package_manager_cli.rs:1226` | A | 逐字一致, TS的 | // 规则（值缺失时是否吃掉下一个参数）与 TS 的 while/index++ 逐字对应。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 10 | `crates/kiana-coding-agent/src/package_manager_cli.rs:2277` | A | 与TS一致 | // help 优先：解析阶段记录的其它错误在这里被忽略（与 TS 一致）。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 11 | `crates/kiana-coding-agent/src/package_manager_cli.rs:2415` | A | 与TS一致 | // 保证调用方在 `await` 之后读到的文件内容与 TS 一致。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 12 | `crates/kiana-coding-agent/tests/cli_smoke.rs:307` | A | 与TS一致 | /// stderr 保持为空（与 TS 一致）。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 13 | `crates/kiana-coding-agent/tests/config_value_migration.rs:3` | A | 与TS一致 | //! 迁移部分与 TS 一致；`createModelRegistry` 的半段用 `kiana-config` 的 `ModelRuntime` | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 14 | `crates/kiana-coding-agent/tests/config_value_migration.rs:237` | A | 与TS一致 | // #5661：同名 env 变量在整个注册表断言期间保持设置（与 TS 一致），否则一个会把 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 15 | `crates/kiana-coding-agent/tests/config_value_migration.rs:361` | A | 必须与TS | // 必须与 TS 一样继续执行，而不是静默跳过整份文件。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 16 | `crates/kiana-coding-agent/tests/regressions_7269_cli_end_of_options.rs:4` | A | 逐字一致 | //! 这里只保留 `parseArgs` 的断言（与 TS 前半段逐字一致）。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 17 | `crates/kiana-coding-agent/tests/stdout_cleanliness.rs:9` | A | 与TS一致 | //! 夹具与 TS 一致：`packages` 与 `npmCommand` 都写在项目的 `.kiana/settings.json` 里 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 18 | `crates/kiana-coding-agent/src/package_manager_cli.rs:1098` | D | 逐字一致 | /// `printConfigCommandHelp()` 的逐字文本（含 `console.log` 的换行）。 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 19 | `crates/kiana-coding-agent/src/package_manager_cli.rs:1114` | D | 逐字一致 | /// `printPackageCommandHelp(command)` 的逐字文本（含 `console.log` 的换行）。 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 20 | `crates/kiana-coding-agent/src/package_manager_cli.rs:1866` | D | 逐字一致 | /// 该行更误导，因此这里只打印逐字一致的 warning 行。 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 21 | `crates/kiana-coding-agent/tests/regressions_5661_uppercase_header_values.rs:4` | D | 逐字一致 | //! 迁移后的 `models.json` 必须逐字保持不变；随后用 `ModelRuntime` 断言大写 env-var | 人工判断：确认是否仍有独立技术原因；没有则删除 |

## 完成后返回

返回本组改写的条目数、删除的条目数、保留的条目数，以及任何需要人工确认的 D 类条目。
