# 注释覆盖进度

本文件由 `tools/gen_progress.py` 从 `doc_inventory.json`、`doc_units.json` 与 `doc_state.json` 生成；数字是**实测值**，不是估计。

## 统计口径

- **src 文件/行**：`crates/*/src/**/*.rs`，不含 `target/` 与测试 fixture。
- **模块文档**：文件在首个条目之前含有 `//!`。
- **公开项**：匹配 `pub fn/struct/enum/trait/type/const/static/mod` 的行（含 `pub async/unsafe/const fn`）；**已文档**：紧邻其上的连续注释/属性块中至少有一条 `///`。
- **新增注释行**：与基线快照 `pi-rs-work/comment-baseline/kiana` 做 unified diff 后，新增的注释行与空行数。
- **单元**：按文件规模划分的注释工作单元（大文件单独成单元，小文件按目录聚合，约 1600 行/单元）。
- **已审阅**：独立审阅者已逐条核对注释与源码/测试；**审阅通过**：审阅未发现需要修复的问题（发现问题并已修复的单元计入“已审阅”但不计入“审阅通过”）。

## 总览

| crate | src 文件 | src 行 | 现有注释行 | 公开项 | 未文档项 | 单元 | 已完成 | 已审阅 | 审阅通过 | 新增注释行 |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| kiana-agent-core | 9 | 7069 | 1177 | 192 | 1 | 6 | 6 | 6 | 0 | 1327 |
| kiana-ai | 156 | 93226 | 7316 | 1688 | 200 | 56 | 56 | 56 | 1 | 19486 |
| kiana-builtin-extensions | 28 | 25157 | 1865 | 267 | 32 | 18 | 18 | 18 | 0 | 4597 |
| kiana-chord | 60 | 17600 | 2017 | 511 | 63 | 18 | 18 | 18 | 1 | 4113 |
| kiana-codemode | 19 | 11596 | 2410 | 285 | 18 | 10 | 10 | 10 | 0 | 1797 |
| kiana-coding-agent | 22 | 11270 | 1025 | 187 | 21 | 11 | 11 | 11 | 5 | 2235 |
| kiana-compaction | 5 | 3904 | 580 | 53 | 4 | 3 | 3 | 3 | 0 | 564 |
| kiana-config | 36 | 27357 | 3003 | 750 | 44 | 20 | 20 | 20 | 2 | 5229 |
| kiana-core | 33 | 26596 | 3202 | 396 | 47 | 16 | 16 | 16 | 0 | 3781 |
| kiana-durable | 84 | 69509 | 5431 | 1364 | 86 | 43 | 43 | 43 | 0 | 12680 |
| kiana-env | 15 | 9267 | 2108 | 154 | 0 | 6 | 6 | 6 | 0 | 2157 |
| kiana-env-client | 9 | 8996 | 700 | 125 | 14 | 5 | 5 | 5 | 0 | 1427 |
| kiana-evals | 7 | 9622 | 1160 | 147 | 15 | 7 | 7 | 7 | 3 | 1083 |
| kiana-export | 5 | 2202 | 370 | 31 | 4 | 3 | 3 | 3 | 0 | 411 |
| kiana-ext-api | 9 | 7113 | 2172 | 317 | 11 | 5 | 5 | 5 | 2 | 770 |
| kiana-extensions | 32 | 46095 | 4914 | 726 | 50 | 19 | 19 | 19 | 5 | 5000 |
| kiana-interactive | 69 | 54349 | 4030 | 1214 | 76 | 37 | 37 | 37 | 9 | 9826 |
| kiana-json | 6 | 3877 | 1227 | 59 | 5 | 4 | 4 | 4 | 1 | 873 |
| kiana-mcp | 18 | 26008 | 3565 | 585 | 55 | 15 | 15 | 15 | 1 | 2466 |
| kiana-resources | 36 | 21434 | 1721 | 429 | 40 | 18 | 18 | 18 | 2 | 4062 |
| kiana-rpc-print | 9 | 8338 | 1391 | 148 | 12 | 6 | 6 | 6 | 0 | 631 |
| kiana-session | 6 | 11969 | 865 | 166 | 5 | 5 | 5 | 5 | 0 | 804 |
| kiana-session-core | 10 | 9104 | 1436 | 230 | 0 | 5 | 5 | 5 | 0 | 1841 |
| kiana-telemetry | 12 | 10189 | 2677 | 179 | 0 | 7 | 7 | 7 | 0 | 2336 |
| kiana-theme | 8 | 8402 | 883 | 192 | 7 | 5 | 5 | 5 | 2 | 1196 |
| kiana-tools | 27 | 12616 | 1305 | 294 | 31 | 11 | 11 | 11 | 0 | 2735 |
| kiana-tui | 57 | 60879 | 4838 | 1190 | 205 | 30 | 30 | 30 | 0 | 9409 |
| kiana-ui-kit | 11 | 2433 | 329 | 56 | 10 | 3 | 3 | 3 | 0 | 697 |
| **合计** | 798 | 606177 | 63717 | 11935 | 1056 | 392 | 392 | 392 | 34 | 103533 |

## 覆盖校验

- `crates/*/src/**/*.rs` 实际文件数：798
- 已分配到注释单元的文件数：798
- 未分配的文件数：0
- 单元清单里不存在的文件数：0
- 被多个单元同时认领的文件数：0

## 批次记录

| 批次 | 运行 | 单元 | 完成 | 已审阅 | 审阅通过 | 日期 | 备注 |
|---|---|---:|---:|---:|---:|---|---|
| P0 试点：kiana-json | wf_4aecceac-de7 | 4 | 4 | 0 | 0 | 2026-11-06 | 4 个注释单元完成；2 个 verify 因网关 403 失败；2 个 verify 发现问题并由主代理人工修正；cargo fmt --check 与 cargo doc 通过 |
| kiana-ai crate 根（T0 试点） | manual-session | 1 | 1 | 0 | 0 | 2026-11-06 | crates/kiana-ai/src/lib.rs crate 级 //!；diff 纯度通过，cargo doc 无新增警告；待独立复审 |
| batch1: kiana-env + kiana-telemetry | wf_72d2e795-b3d | 13 | 13 | 12 | 0 | 2026-11-06 | 12/13 单元完成对抗性审阅，共 71 个问题已由修复代理逐条改正；u339 的审阅因网关 403 未完成，已加入复审队列；u342 由专项工作流复审并修复。主代理另修复 kiana-telemetry 的 rustdoc 作用域问题（删除 pub mod 外层 ///、清理冗余显式链接目标）与 kiana-env-client 的 8 处私有项链接警告。 |
| re-verify: kiana-json + kiana-ai root | wf_aed5a98a-9f0 | 4 | 4 | 4 | 0 | 2026-11-06 | 对试点单元做二次审阅：u283/u284/u286/u061 共发现 18 处问题并全部修复（u061 修复 9 处，含 kiana-json 逐字节一致性的被迫偏差、ModelsError 范围、HttpClient 命名、重试分散等）；u285 的审阅代理未返回，待重跑。 |
| re-verify: telemetry u339/u342 | wf_f3085d60-939 | 2 | 2 | 2 | 0 | 2026-11-06 | 补审 batch1 中未完成的 u339（index/jsjson）与 u342（crate 根）：共 8 处问题并全部修复（外部父级的 parent_id、StoredSpanLink.attributes、js_string 对象常量、serde_json 保序差异、取消语义、错误类型范围等）。 |
| batch-tui | wf_6c439348-a9b | 30 | 30 | 30 | 0 | 2026-11-06 | journal 自动汇总；失败调用 0 次（多为网关 403，已换模型重试） |
| batch-core | wf_13c73305-a24 | 26 | 26 | 26 | 0 | 2026-11-06 | journal 自动汇总；失败调用 3 次（多为网关 403，已换模型重试） |
| batch-ai | wf_210d8862-c1d | 56 | 56 | 56 | 1 | 2026-11-06 | journal 自动汇总；失败调用 4 次（多为网关 403，已换模型重试） |
| batch-chord | wf_6b37d6c6-891 | 43 | 43 | 43 | 2 | 2026-11-06 | journal 自动汇总；失败调用 2 次（多为网关 403，已换模型重试） |
| batch-agent-durable | wf_993622f1-b2d | 49 | 49 | 49 | 0 | 2026-11-06 | journal 自动汇总；失败调用 0 次（多为网关 403，已换模型重试） |
| batch-mid | wf_c6a17951-6ba | 50 | 50 | 50 | 6 | 2026-11-06 | journal 自动汇总；失败调用 65 次（多为网关 403，已换模型重试） |
| batch-ext | wf_e19bf1ca-d66 | 60 | 60 | 60 | 7 | 2026-11-06 | 60/60 完成（ann 全落地，53 个有问题单元的 fix 全部成功）。网关 429/403 重试 125 次，模型轮换后全部恢复。验证：4 crate fmt/doc/check 全过；kiana-extensions 修 11 处链接（含 JS_CALLBACKS→RETAINED_CALLBACKS 更名纠错）、kiana-session 修 6 处。 |
| batch-front | wf_b422a943-eeb | 61 | 61 | 61 | 17 | 2026-11-06 | 61/61 完成（ann 全落地，44 个有问题单元的 fix 全部成功）。网关 429/403 重试 129 次。验证：5 crate fmt/check 全过；doc 方面 kiana-coding-agent 修 3 处、kiana-interactive 修 4 处后通过，evals/rpc-print 一次通过。 |

## 修复复检（2026-11-06）

抽样复检 10 个单元（每批次 issues 最多的已修复单元：u283/u026/u104/u331/u068/u109/u124/u161/u202/u378，
覆盖 kiana-json / ai / codemode / session-core / builtin-extensions / coding-agent / config / durable / env / tui）：
**112 个审阅问题全部确认已修复，0 条未解决**；复检代理同时确认未发现新引入的明显错误。
这意味着“修复代理声称已修复”的抽查通过率为 100%（样本为问题最集中的高危单元，非随机样本）。

## 待核实清单

- 库代码使用 tracing::warn! 等，但 2026-11-05 全仓审计未找到任何 tracing_subscriber 安装点（crates 与 tests 中 0 处引用），因此这些日志默认不会输出；是否应安装 subscriber 属于产品决策，待维护者确认。
- kiana-env 的 docs/protocol.md、docs/semantics.md 等被测试逐字锁定，注释改造不得修改它们；本次只改源码注释。

## 人工修正记录

| 日期 | 单元 | 文件 | 修正内容 |
|---|---|---|---|
| 2026-11-05 | u284-kiana-json-group | js_num.rs, js_string.rs | 把“仓库里唯一实现”限定为 kiana-json 内部并注明两处本地移植；补 parse_js_number 空串→+0；删除 JsType 跨语言边界的无依据动机。 |
| 2026-11-05 | u286-kiana-json-root | lib.rs | stringify 链接改为模块限定；修正 serde_json::Number 相等的表述；把“刻意偏差”改为“被迫偏差”并引用 ARCHITECTURE §8 第 31、32 条。 |
| 2026-11-05 | u342-kiana-telemetry-root | lib.rs, store_conformance.rs | 删除 pub mod 声明上方新增的外层 ///（rustdoc 会把合并文档放到父作用域解析链接，导致 StoreFixture/ErasedValue 等短链接失效）；随之移除 store_conformance.rs 中为绕过该问题而加的显式 crate:: 链接目标。 |
| 2026-11-05 | batch1-kiana-env-client | lib.rs, connection.rs, remote_env.rs, ssh.rs, terminal.rs | 把 8 处指向私有/pub(crate) 项的 rustdoc 链接改为行内代码，消除 rustdoc::private_intra_doc_links 新增警告；RUSTDOCFLAGS=-D warnings cargo doc -p kiana-env -p kiana-env-client 通过。 |

## 未执行项与限制

- 测试文件（crates/*/tests/**/*.rs）不逐文件添加注释；测试策略与 fixture 来源汇总到 docs/DEBUGGING_GUIDE.md 与各 crate 的测试说明。
- examples/ 与 benches/ 未纳入本轮逐文件注释（以可运行示例为主）；2026-11-05 已用 `cargo check -p kiana-durable -p kiana-codemode --examples` 验证示例可编译。

