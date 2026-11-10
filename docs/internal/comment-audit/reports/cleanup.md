# 注释清理报告

本报告依据本次工作流的编辑摘要（50 个分组）与验证结果编写。所有数字均由这两份数据直接汇总，未做推测或补数。

## 1. 总体统计

| 指标 | 数值 | 说明 |
| --- | ---: | --- |
| 工作单分组数 | 50 | 编辑摘要中共 50 个 group |
| 处理条目总数 | 1405 | 改写 + 删除 + 保留 |
| 改写条目 | 1141 | 去掉以 TS 为权威的措辞，改为当前设计原因或稳定契约表述 |
| 删除条目 | 1 | 仅 `kiana-ai--part-7` 的 `crates/kiana-ai/tests/models_runtime.rs:4` 一处 |
| 保留条目 | 263 | 判定仍有独立技术原因，或属于无需改动的契约说明 |
| 涉及文件路径条目 | 538 | `files_touched` 汇总；逐项核对未见跨组重复路径 |
| 其中 Rust 文件 | 535 | 538 − 1 个 Markdown − 2 个 Cargo.toml |
| 其中非 Rust 文件 | 3 | `ARCHITECTURE.md`、`crates/kiana-ai/Cargo.toml`、`crates/kiana-durable/Cargo.toml` |

说明：

- 改写/删除/保留三项之和为 1141 + 1 + 263 = 1405。
- 唯一被删除的条目是 `kiana-ai--part-7` 中 `crates/kiana-ai/tests/models_runtime.rs:4` 的“断言与测试标题逐字保留”说明；该组备注指出，如团队仍需要移植约定，应恢复该行。
- 本次只做了注释/文档文字编辑与只读验证，未运行 git，未做暂存或提交。

## 2. 按组统计

| 分组 | 改写 | 删除 | 保留 | 备注 |
| --- | ---: | ---: | ---: | --- |
| docs-root | 11 | 0 | 14 | 顶部目标重写；crate 数由 29 更新为 30（3 处）；用户可见契约改为稳定契约表述 |
| kiana-agent-core | 33 | 0 | 4 | A 类 22 条；D 类 11 条改写、4 条保留 |
| kiana-ai--part-1 | 30 | 0 | 10 | 29 条 A 类；D#9 改写；两处注释合并导致后续行号上移 1 |
| kiana-ai--part-2 | 29 | 0 | 11 | A 类 18 条；D 类 #3/#4/#14/#39/#40 改写 |
| kiana-ai--part-3 | 33 | 0 | 7 | #20/#23 与 #37/#40 分别共用同一注释行，已合并处理 |
| kiana-ai--part-4 | 38 | 0 | 2 | 保留 TS 出处；D 类补明后果说明 |
| kiana-ai--part-5 | 35 | 0 | 5 | 一次编辑产生的重复词已发现并修正 |
| kiana-ai--part-6 | 32 | 0 | 8 | A 类 27 条；D 类 8 条保留 |
| kiana-ai--part-7 | 28 | 1 | 11 | 全报告唯一删除项；A 类 20 条 |
| kiana-ai--part-8 | 7 | 0 | 3 | 7 条改写；3 条 D 类保留 |
| kiana-builtin-extensions--part-1 | 36 | 0 | 4 | D 类 10/11/12/15/25 改写，16/26/34/35 保留 |
| kiana-builtin-extensions--part-2 | 2 | 0 | 0 | 两条均为改写 |
| kiana-chord--part-1 | 35 | 0 | 5 | A 类加 D#6/#40 改写；5 条 D 类保留 |
| kiana-chord--part-2 | 14 | 0 | 5 | A 类 10 条、D 类 4 条改写；5 条保留 |
| kiana-codemode--part-1 | 32 | 0 | 8 | A 类 30 条；D 类 #13/#18 改写 |
| kiana-codemode--part-2 | 5 | 0 | 0 | 5 条均改写 |
| kiana-coding-agent | 20 | 0 | 1 | A 类 17 条；D#18/#19 改写 |
| kiana-compaction | 14 | 0 | 1 | A 类 14 条；1 条保真约束小节保留 |
| kiana-config | 25 | 0 | 14 | A 类 22 条；D 类多数保留，#24/#34 改写 |
| kiana-core--part-1 | 39 | 0 | 1 | A 类 33 条；6 条 D 类改写，1 条保留 |
| kiana-core--part-2 | 31 | 0 | 4 | A 类改写；D 类多条改写 |
| kiana-durable--part-1 | 32 | 0 | 8 | A 类 31 条；8 条 D 类保留 |
| kiana-durable--part-2 | 39 | 0 | 1 | A 类 31 条；D 类 9 条中 8 条改写、1 条保留 |
| kiana-durable--part-3 | 40 | 0 | 0 | 40 条全部改写 |
| kiana-durable--part-4 | 4 | 0 | 0 | 4 条均改写 |
| kiana-env | 2 | 0 | 1 | 2 条改写；`tests/docs.rs` 保留 |
| kiana-env-client | 11 | 0 | 4 | A 类改写；4 条 D 类保留 |
| kiana-evals | 27 | 0 | 2 | A 类 16 条；D 类 11 条改写 |
| kiana-export | 13 | 0 | 2 | A 类 12 条；D#13 改写；2 条保留 |
| kiana-ext-api | 7 | 0 | 2 | 7 条改写；2 条 D 类保留并补充原因 |
| kiana-extensions--part-1 | 27 | 0 | 13 | A 类 27 条；13 条 D 类保留 |
| kiana-extensions--part-2 | 9 | 0 | 3 | 9 条改写；3 条保留 |
| kiana-interactive--part-1 | 39 | 0 | 1 | 39 条改写；1 条保留 |
| kiana-interactive--part-2 | 35 | 0 | 5 | 35 条改写；5 条 D 类保留 |
| kiana-interactive--part-3 | 19 | 0 | 0 | 19 条全部改写 |
| kiana-json | 8 | 0 | 8 | 8 条改写；8 条 D 类保留 |
| kiana-mcp--part-1 | 27 | 0 | 13 | A 类 15 条；D 类 9 条改写措辞 |
| kiana-mcp--part-2 | 10 | 0 | 6 | A 类 9 条；另 1 条改写；6 条 D 类保留 |
| kiana-resources | 27 | 0 | 5 | A 类 27 条；5 条 D 类保留或补充原因 |
| kiana-rpc-print | 17 | 0 | 10 | A 类 17 条；10 条 D 类保留 |
| kiana-session | 5 | 0 | 0 | 5 条全部改写 |
| kiana-session-core | 22 | 0 | 7 | 22 条改写；7 条保留 |
| kiana-telemetry | 27 | 0 | 7 | A 类 22 条；D 类 5 条改写 |
| kiana-theme | 10 | 0 | 13 | A 类 10 条；13 条 D 类保留 |
| kiana-tools | 29 | 0 | 3 | A 类 22 条；D 类 7 条改写、3 条保留 |
| kiana-tui--part-1 | 34 | 0 | 6 | A 类 30 条；6 条 D 类保留 |
| kiana-tui--part-2 | 29 | 0 | 11 | A 类 29 条；11 条 D 类保留 |
| kiana-tui--part-3 | 36 | 0 | 4 | 36 条改写；4 条保留 |
| kiana-tui--part-4 | 20 | 0 | 7 | A 类 20 条；7 条 D 类保留或补充原因 |
| kiana-ui-kit | 7 | 0 | 3 | 7 条改写；3 条 D 类保留 |

## 3. 验证结果

### 3.1 最终验证

| 检查项 | 命令 | 结果 |
| --- | --- | --- |
| 仅注释改动（排除并发会话文件） | `python3 scripts/check_comment_only_diff.py --exclude <6 个文件> --json` | 通过：590 个 Rust 文件，0 处代码差异 |
| 格式检查 | `cargo fmt --all -- --check` | 通过（exit 0） |
| 编译检查 | `CARGO_TARGET_DIR=/data/cargo-target-audit timeout 900 cargo check --workspace` | 通过（exit 0，22 个 crate，约 29s） |

### 3.2 六个并发改动文件

初次验证发现 6 个文件含代码改动：

1. `crates/kiana-ai/src/api/google_shared.rs`
2. `crates/kiana-ai/src/api/kiana_messages.rs`
3. `crates/kiana-ai/src/models/mod.rs`
4. `crates/kiana-coding-agent/src/app.rs`
5. `crates/kiana-resources/src/bug_report.rs`
6. `crates/kiana-session/src/agent_session.rs`

这些改动**不是本次注释清理计划内的内容**。其中 `app.rs` / `agent_session.rs` 与并发会话正在进行的权限 CLI/设置集成一致；`kiana_messages.rs` / `models/mod.rs` / `bug_report.rs` / `google_shared.rs` 是代码修复（内存上限、刷新竞态、递归栈溢出、重复匹配等），可能来自并发会话，也可能是编辑 agent 越界。来源需要人工确认。

为不破坏并发工作，完整 diff 已保存为 [`unexpected-code.patch`](../unexpected-code.patch)，工作区保留这些改动；仅注释检查通过 `--exclude` 跳过这 6 个文件。其余 590 个 Rust 文件均为注释/文档变化，检查通过。

### 3.3 结论

- 注释清理本身没有留下代码改动；改写的 1,141 条、删除的 1 条、保留的 263 条均只涉及注释/文档文字。
- 6 个并发文件需要与另一个会话协调后再合并；它们的代码差异已单独留档，便于审阅或回滚。
- `fmt` 与 `cargo check --workspace` 均通过，说明当前工作树可编译。

## 4. 仍需人工确认的条目（needs_review 汇总）

编辑摘要的 `needs_review` 共 209 条。下面按分组给出条数与要点；完整逐条文本以本次编辑摘要的 `needs_review` 字段为准。

| 分组 | 条数 | 待确认要点（摘要） |
| --- | ---: | --- |
| docs-root | 3 | `ARCHITECTURE.md:1840-1841` 旧目标残留；§4.5 标题措辞；kiana-env 远端路径强约束是否放宽 |
| kiana-agent-core | 4 | float_roundtrip 舍入；`agent.rs:1688` 并发前置检查竞态；落盘与上屏字节一致；wire 未知字段与插入序 |
| kiana-ai--part-1 | 10 | 保留项：UTF-16 lossy、kiana_json 的 JS 语义、tool call id 种子、别名转发、错误文案、Display 等值于 message、serde_json 与 V8 文本差异、js_parse 文案、error_message 文本匹配 |
| kiana-ai--part-2 | 4 | 字节稳定产物（`model_catalog.rs:106`）；map 键与 `model.api` 精确匹配（`provider.rs:328/333`）；JSON.stringify 逐字节（`types.rs:90`）；spec 作为权威（`constrained_sampling.rs:65`） |
| kiana-ai--part-3 | 9 | V8 文案与 JSON.parse 输入面；lazy 能力与实现错误语义；codex 算法条件；消息相等复用；错误保真；协议 notice；V8 TypeError；redirect_uri 相等；Azure 文案 |
| kiana-ai--part-4 | 2 | forbidden method 与 fetch 的差异（`reqwest_client.rs:286`，待核实）；V8 `Math.exp` 黄金值（`llama_cpp_classify.rs:1027`） |
| kiana-ai--part-5 | 14 | 保留项集中于 `model_operations`、`node_http_proxy`、`retry`、`build.rs`、`callback_server`、`extension_oauth_types`、`catalog`、`model_schema`、`identity` |
| kiana-ai--part-6 | 6 | 占位文本需发送给 provider；raw_models 原样回写；两入口行为相同；typesafe/vercel 的 api 键匹配；abort 两条错误文案 |
| kiana-ai--part-7 | 7 | 删除 `models_runtime` 移植约定说明；pass-through；旧别名兼容理由；oauth_page 实体写法；非 UTF-8 环境变量；details 断言；cli_login 的 spec 出处 |
| kiana-ai--part-8 | 3 | manifest 的 sha256 依赖字段序；raw_models 无损往返；流式参数不变量 |
| kiana-builtin-extensions--part-1 | 5 | renderer 的 JS 数值保真；mcp/ui 逐字节；theme_snapshot 两处；mcp/tools 的 last_component |
| kiana-builtin-extensions--part-2 | 0 | — |
| kiana-chord--part-1 | 5 | ContextRef 并发共享的不可变契约；schema 喂给 `schema::check`；压缩不改窗口内容；两个快照不变量 |
| kiana-chord--part-2 | 5 | raw_errors 条数一致；golden 夹具一致；指纹判定阶梯；preserve_order 键序；`String(error)` 夹具 |
| kiana-codemode--part-1 | 8 | 偏差条目与 skeleton 比较；冻结类型使校验不可达；测试注入动机；JSON 文本保真；Node 实测黄金值；ERR_WORKER_PATH；字面量换行/TAB |
| kiana-codemode--part-2 | 0 | — |
| kiana-coding-agent | 2 | regressions_5661 保留；package_manager_cli 的 golden 契约措辞 |
| kiana-compaction | 1 | `branch_summarization.rs:44` 保真约束小节 |
| kiana-config | 14 | 外部可编辑文件不丢数据；黄金字节；ETag；Authorization；raw_models；in-memory 与文件后端共享 JSON；V8 错误文案等 |
| kiana-core--part-1 | 0 | — |
| kiana-core--part-2 | 5 | Node Buffer 字节保真；photon-rs；BOM 单码元；zlib match-search 差异；Windows 文本差异 |
| kiana-durable--part-1 | 8 | 地址键字节稳定；copy 校验；stringify 与 panic；错误还原；scope 身份；UTF-16 代理项差异 |
| kiana-durable--part-2 | 3 | 事务阶段 3 语义；JSONL 磁盘格式互操作；photon wasm 字节 |
| kiana-durable--part-3 | 4 | photon-rs 输出一致；posix/win32 的 Node 语义；registry 删除 TS 标识符；storage_benchmark 删除出处句 |
| kiana-durable--part-4 | 0 | — |
| kiana-env | 1 | `tests/docs.rs` 的 npm 上游文档与指纹锁定 |
| kiana-env-client | 4 | crate 文档；阅读顺序；转发语义；known_hosts 逐行规则 |
| kiana-evals | 2 | 两变体发现结果一致；比较 Vec 意味着条数一致 |
| kiana-export | 0 | — |
| kiana-ext-api | 0 | — |
| kiana-extensions--part-1 | 13 | loader_impl 多处逐字节/golden；undefined 与函数保真；promise 协议、JSON.parse、字符串化；缓存键；设计文档；noop_ui 默认值 |
| kiana-extensions--part-2 | 0 | — |
| kiana-interactive--part-1 | 4 | 字节保真；errno 文案；bug_report 文案；fdlibm 金帧 |
| kiana-interactive--part-2 | 5 | trust_selector、radius_login 两处、program_status_reporter、session_share 的保留判断 |
| kiana-interactive--part-3 | 1 | components_mermaid 的黄金字面量 |
| kiana-json | 7 | 三处逐字节；深比较；typeof 与 ===；跨 crate fixture；3 个 it；V8 兼容面 |
| kiana-mcp--part-1 | 7 | jsonrpc 标题与字段序；content 三处；oauth/errors 四处；protocol_types 两处；长度上限；IPv6 方括号 |
| kiana-mcp--part-2 | 6 | 黄金数据归一化；原始字节断言；JSON 回写；键序；冻结契约；wire 字节 |
| kiana-resources | 1 | `package/paths.rs:1562` 官方样例出处待核实 |
| kiana-rpc-print | 2 | 测试函数名含 TS（代码标识符未改）；D 18-27 的保留判断 |
| kiana-session | 0 | — |
| kiana-session-core | 0 | — |
| kiana-telemetry | 8 | jsjson、lib、memory、store_conformance、testing、memory_edges 的保留项；`index.rs:1079` 标题仍含 TS 且未列入条目 |
| kiana-theme | 0 | — |
| kiana-tools | 4 | bash.rs 三处行为说明；tool_definitions_golden 的 spec 引文改写 |
| kiana-tui--part-1 | 9 | latex 三处；editor 3424/4230；tui 3045；stdin 32；alt_screen 3729/4053 |
| kiana-tui--part-2 | 1 | `stack.rs:486` 残留“TS 默认值” |
| kiana-tui--part-3 | 5 | box_component 两处；block.rs 表头；flash.rs 顺序；markdown/mod.rs 措辞 |
| kiana-tui--part-4 | 7 | darwin 文案；linux_x11 的 INCR；win32 代理；includes 探测范围；native_clipboard 文案；编译期开关一致性；用例数与标题 |
| kiana-ui-kit | 0 | — |

### 4.1 待确认条目的主要类型

1. **用户可见契约**：错误文案、CLI stdout/stderr、退出码、session JSONL 键序、环境变量名与取值。此类统一保留文本，改为“当前稳定契约，改动需同步测试”。
2. **字节与序列化稳定**：JSON.stringify 兼容、golden fixtures、manifest sha256、ETag、auth.json/models.json 等落盘格式。保留的技术理由是下游读取或字节比较。
3. **运行时与平台差异**：UTF-16 代理项、V8 错误文案、JS 正则语义、libuv errno、Node Buffer 与非 UTF-8 环境变量。
4. **spec 作为依据**：`spec/ai-core.md`、`spec/ai-auth.md`、`spec/ca-cli.md`、`spec §2.3.1`、`spec §3.7` 等被引用处，需确认这些规范仍是当前权威。
5. **仅保留出处**：`TS 源：...`、`TS: ...`、`移植自 ...` 等历史标记未删除，仅去除约束性措辞，需要确认是否继续保留。

## 5. 未自动处理的范围

- `spec/` 与 `docs/` 下的旧规范需要整体重写，不是逐行改注释。本次仅处理工作单列出的注释行，未改写任何规范正文。
- `reference_project/`（参考项目源码）未改动。
- `scripts/`（审计与校验脚本）未改动。
- `docs/internal/comment-audit/work/` 与 `docs/internal/comment-audit/scan*.csv` 未改动。
- 工作单之外的 TS 指涉按“不扩大 diff”原则未触碰，例如：
  - `crates/kiana-ai/src/error.rs:57`、`error.rs:5/15/65/149/277`、`auth/oauth/github_copilot.rs:318/490/535/822`
  - `crates/kiana-ai/src/api/kiana_messages.rs:748`
  - `crates/kiana-builtin-extensions/src/codemode/renderer.rs:60`
  - `crates/kiana-compaction/src/branch_summarization.rs:36/79/149`、`compaction.rs:432/1461`
  - `crates/kiana-config/src/trust/manager.rs:57`
  - `crates/kiana-tui/src/components/stack.rs:486`
  - `crates/kiana-telemetry/src/index.rs:1079`
- `ARCHITECTURE.md:1840-1841` 仍写“原始目标‘功能和交互完全一样’未被完整证明”，与新顶部目标存在措辞冲突；该处不在工作单条目内，按要求未改。
- 代码标识符中的 TS 字样未改，例如 `tests/rpc_client_commands.rs:324` 的 `request_timeout_uses_ts_error_text`；改名属于代码变更，应另开变更单。

### 5.1 清理后的剩余量

清理完成后重新运行 `python3 scripts/comment_audit.py`，剩余：

| 类别 | 清理前 | 清理后 |
| --- | ---: | ---: |
| A（TS 保真硬约束） | 1,044 | **86** |
| B（功能说明） | 18,708 | 18,692 |
| C（历史来源） | 14,491 | 14,460 |
| D（待人工判断） | 682 | 530 |

剩余 A 类的分布：

- `spec/`：74 条。旧移植规范整体基于“TS 行为一致”前提，应整体重写，不适合逐行清理。
- `docs/`：10 条。`ARCHITECTURE.md`、`COMMENT_STYLE.md`、`DEBUGGING_GUIDE.md`、`EXECUTION_FLOW.md`、`START_HERE.md`、`UNDERSTANDING_GUIDE.md` 中的旧表述。
- `crates/`：2 条，均在 `kiana-ui-kit`，指“上游接口”而非 TS，可人工确认后保留或改成中性措辞。

代码注释里的 A 类已基本清零；剩余主要是规范文档和少量保留项。

## 6. 下一步建议

1. **确认 6 个文件的代码改动来源**：`google_shared.rs`、`kiana_messages.rs`、`models/mod.rs`、`app.rs`、`bug_report.rs`、`agent_session.rs` 含非注释代码改动。`app.rs` / `agent_session.rs` 与并发会话的权限 CLI/设置集成一致；其余 4 个可能是并发修复或编辑 agent 越界。完整 diff 已留档为 `docs/internal/comment-audit/unexpected-code.patch`，工作区已保留这些改动；不要按误改直接还原，先确认来源和最终版本。
2. **人工确认 209 条 needs_review**：建议按第 4.1 节的类型分批，优先确认用户可见契约、spec 引用与跨平台差异，再处理仅保留出处的条目。
3. **修正 `ARCHITECTURE.md:1840-1841`** 的旧目标表述，使其与顶部“以参考项目的最佳设计和实际效果为准”的新目标一致。
4. **规划 `spec/` 与 `docs/` 旧规范的整体重写**：这是独立的文档工作流，不应继续以逐行注释清理的方式推进。
5. **清理工作单之外的 TS 指涉**：先补充工作单再修改，避免再次出现“摘要声明仅注释、实际 diff 超出范围”的问题。
6. **补充测试**：本次只运行了 `cargo fmt --check` 与 `cargo check`，未运行测试；在代码差异核实并还原/拆分后，应运行 `cargo test --workspace`（或按 crate 分批），并对“用户可见契约”条目同步测试。
7. **复核删除项**：确认 `crates/kiana-ai/tests/models_runtime.rs:4` 的移植约定说明是否确实不需要；若需要，恢复该行并重新统计。
