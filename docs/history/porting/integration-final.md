# FINAL — 全量集成验证与收尾

日期：2026-11-07
工作目录：`/home/ctyun/code/kiana`
范围：第 0/1/2/3/4/5/6/7/8/9/10/11a/11b/12a/12b 周与 wire A–E 全部完成后的统一验证。
本文件不重做实现；只记录真实执行的命令、结果、修复、剩余缺口与发布建议。

环境：

- `rustc 1.99.1 (58cf8f9c3 2026-10-13)`、`cargo 1.99.1 (5f94df478 2026-08-27)`。
- 统一构建目录 `CARGO_TARGET_DIR=/data/cargo-target-sandbox`；最终测试追加
  `CARGO_INCREMENTAL=0`（避免再次写满磁盘缓存的 query-cache）。
- `bubblewrap 0.6.1`（`/usr/bin/bwrap`）可用，`kiana-sandbox` 的真实隔离测试可以执行。
- 磁盘事件：首次按任务命令跑五批测试时 `/data` 已 100% 满，五批全部在编译期以
  `No space left on device (os error 28)` 失败（不是代码失败）。清理
  `/data/cargo-target-sandbox/debug/incremental` 与两份旧构建缓存
  `/data/cargo-target`、`/data/cargo-target-audit`（约 61 GB，均为可重建的 cargo
  产物，不是源码）后重跑，全部通过。

## 1. 结论

- workspace 共 **32 个 crate**（`cargo` members 与 `crates/` 目录一致）。
- `cargo fmt --all`、`cargo check --workspace --all-targets`、
  `cargo clippy --workspace --all-targets` 均通过。
- 任务指定的五批测试全部通过：**0 failed**；ignored 均为既有 live/凭据/`KIANA_BIN`
  类用例，未新增 ignore，未删除测试。
- 为覆盖任务五批之外的 14 个 crate，另补跑 2 批（同样 `timeout 900`）：1428 + 3035
  passed、0 failed。**32 个 crate 全部跑过测试**，合计 **12961 passed / 0 failed /
  401 ignored（935 个测试目标）**。
- 修复了 1 个阻塞 clippy 的 `clippy::never_loop` deny（`kiana-session`），行为不变，
  clippy 全量重跑退出码 0。
- 四份文档的 crate 数量与层表已核实：README 为 32；根 `ARCHITECTURE.md`、
  `docs/ARCHITECTURE_OVERVIEW.md`、`docs/START_HERE.md` 的数量为 32，层表均包含
  `kiana-hooks` 与 `kiana-memory`（L6）。未发现残留的“当前 30/31 个 crate”表述。
- 剩余工作都是已在各周/wire 文档中声明的**接线型缺口**（daemon/cron/IDE 启动接线、
  durable 适配、memory 抽取 job 等），不是本次集成引入的回归。

## 2. 各周 status

| 周 | 主题 | 状态 | 验证依据 |
|---|---|---|---|
| W0（`phase-00-convergence.md`） | 权限启用入口、运行期切换、`execute_bash` 权限路径、文档止血 | 完成 | `docs/history/porting/phase-00-convergence-verify.md`；batch1 + batch4 |
| W1 | 结构化审批（AllowOnce/AllowAlways/Reject）、持久化、事件 | 完成 | `docs/history/porting/phase-01-permissions-approval.md`；batch1 + batch4 |
| W2 | 所有执行路径统一沙箱、`ExecutionStatus`、read-before-write | 完成 | `phase-02-sandbox-execution-paths.md` + `wiring/phase-02-03.md`；batch1 + batch4 |
| W3 | 配置分层、严格诊断、shell 风险分析 | 完成 | `phase-03-config-shell-risk.md` + `wiring/phase-02-03.md`；batch1 |
| W4 | hooks 配置、matcher、派发、退出码/JSON 契约 | 完成 | `phase-04-hooks.md` + `wiring/phase-04-07-08.md`；batch2 |
| W5 | MCP prompts/stdio 环境白名单/重定向校验、Skills 按需加载 | 完成 | `phase-05-mcp-skills.md` + `wiring/phase-05-06-09.md`；batch2 + batch7 |
| W6 | 压缩前 pruning、工具输出预算、恢复 | 完成 | `phase-06-compaction-output.md` + `wiring/phase-05-06-09.md`；batch1 + batch2 |
| W7 | multi_edit、apply_patch、流式 patch 解析 | 完成 | `phase-07-editing-experience.md` + `wiring/phase-04-07-08.md`；batch1 |
| W8 | mock provider、loop guard、turn/token/cost 预算 | 完成 | `phase-08-evals-loop-guard.md` + `wiring/phase-04-07-08.md`；batch3 |
| W9 | provider 能力矩阵、输出预算、错误分类、OTel | 完成 | `phase-09-request-policy-otel.md` + `wiring/phase-05-06-09.md`；batch3 |
| W10 | 持久记忆（存储/索引/召回/抽取/治理） | 完成 | `phase-10-persistent-memory.md` + `wiring/phase-10-11a-12a.md`；batch2 + batch4 |
| W11a | 子代理运行时、AgentDefinition、隔离 | 完成 | `phase-11a-subagent-runtime.md` + `wiring/phase-10-11a-12a.md`；batch4 |
| W11b | 后台任务、恢复、checkpoint、acceptance | 完成（durable 适配为已声明缺口） | `phase-11b-background-checkpoints.md` + `wiring/phase-11b-12b.md`；batch4 |
| W12a | TUI 帧契约、finality 账本、审批卡 | 完成（渲染核心仍由实验开关控制） | `phase-12a-tui-frame-contract.md` + `wiring/phase-10-11a-12a.md`；batch5 + batch7 |
| W12b | web/IDE/daemon EventFeed/cron、发布硬化 | 完成（启动流程接线为已声明缺口） | `phase-12a-tui-frame-contract.md` + `wiring/phase-11b-12b.md`；batch5 |

## 3. Wire 结果

| Wire | 覆盖 | 结果 | 本次最终回归 |
|---|---|---|---|
| A | 第 2/3 周共享接线 | 通过：`cargo check --workspace`、sandbox/tools/config/permissions 测试全绿 | batch1（1349 passed） |
| B | 第 4/7/8 周共享接线 | 通过：hooks、apply_patch、loop guard/预算测试全绿 | batch1 + batch2 + batch3 |
| C | 第 5/6/9 周共享接线 | 通过：MCP/resources/compaction/provider/telemetry 测试全绿 | batch2 + batch3 |
| D | 第 10/11a/12a 周共享接线 | 通过：memory/session/session-core/durable/tui/coding-agent 测试全绿 | batch2 + batch4 + batch5 |
| E | 第 11b/12b 周共享接线 | 通过：recovery/background/checkpoint/acceptance/cron/daemon 测试全绿 | batch4 + batch5 |

五个 wire 文档均记录 `cargo check --workspace` 退出码 0；本文件第 4 节是所有这些
工作的统一最终回归。

## 4. 最终真实命令与结果

全部在 `/home/ctyun/code/kiana` 下执行，`CARGO_TARGET_DIR=/data/cargo-target-sandbox`。

### 4.1 格式化

```text
$ cargo fmt --all
（无输出，退出码 0）

$ cargo fmt --all -- --check        # 修复 clippy error 之后复核
（无输出，退出码 0）
```

### 4.2 `cargo check`

```text
$ timeout 900 cargo check --workspace --all-targets
cargo check: 0 errors, 1 warnings (40 crates)
CHECK_EXIT=0

$ timeout 900 cargo check --workspace --all-targets   # clippy 修复后复核
cargo check: 0 errors, 1 warnings (5 crates, 增量)
CHECK_EXIT=0
```

唯一 warning 是既有的测试辅助函数未使用：
`crates/kiana-permissions/tests/shell_analysis.rs:39` 的 `prefix_deny`
（`dead_code`），不是本次改动引入。

### 4.3 `cargo clippy`

首次运行失败（退出码 101），原因是默认 deny 的 lint：

```text
$ timeout 900 cargo clippy --workspace --all-targets
error: this loop never actually loops
   --> crates/kiana-session/src/execution.rs:386:9
    = note: `#[deny(clippy::never_loop)]` on by default
CLIPPY_EXIT=101
```

修复（见第 5 节）后重跑：

```text
$ timeout 900 cargo clippy --workspace --all-targets
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 47.75s
CLIPPY_EXIT=0
```

统计：**0 error**；日志中 197 条 warning 诊断行（包含同一 crate 的 lib 与
lib-test 目标重复计数）、62 条 crate 级汇总。按任务要求没有为消除 warning 做
大改，也没有新增 `#[allow]`。

### 4.4 测试批次

每条命令都使用 `timeout 900`；下表的 passed/failed/ignored 由该批日志中的
`test result:` 行汇总，“目标”是实际运行的测试目标数（每个 `test result:` 行，
包含 lib/integration/doc 测试）。

| 批次 | 命令 | 退出码 | passed | failed | ignored | 目标 | 用时 |
|---|---|---|---|---|---|---|---|
| 1 | `cargo test -p kiana-permissions -p kiana-sandbox -p kiana-config -p kiana-tools` | 0 | 1349 | 0 | 1 | 84 | 104s（首次 ENOSPC，清理后重跑） |
| 2 | `cargo test -p kiana-hooks -p kiana-memory -p kiana-compaction -p kiana-mcp` | 0 | 720 | 0 | 2 | 37 | 46s |
| 3 | `cargo test -p kiana-ai -p kiana-telemetry -p kiana-evals -p kiana-agent-core` | 0 | 2698 | 0 | 341 | 240 | 146s |
| 4 | `cargo test -p kiana-session -p kiana-session-core -p kiana-durable` | 0 | 1673 | 0 | 17 | 194 | 354s |
| 5 | `cargo test -p kiana-tui -p kiana-coding-agent -p kiana-rpc-print` | 0 | 2058 | 0 | 35 | 101 | 52s |
| **合计** | 五批任务指定命令 | 0 | **8498** | **0** | **396** | **656** | — |

补充说明：

- 首次执行这五批时，`/data` 已满，所有批次在编译阶段失败（ENOSPC）；清理可重建的
  cargo 缓存后的重跑结果才是上表。
- ignored 全部是既有用例，未新增：batch1 为 1 个文档示例；batch2 为
  `kiana-compaction` 的 2 个既有用例；batch3 为 `kiana-ai` 的 341 个 live/provider/
  golden 用例；batch4 为 `kiana-session` 5 个、`kiana-session-core` 1 个、
  `kiana-durable` 11 个既有用例；batch5 为 `kiana-tui` 6 个、
  `kiana-coding-agent` 5 个、`kiana-rpc-print` 24 个既有用例。
- 所有失败都被视为需要修复的问题；本次没有删除测试、没有新增 `#[ignore]`、
  没有把失败记录为通过。

### 4.5 补充测试（覆盖任务五批之外的 crate）

任务指定的五批涉及 18 个 crate；为核实其余 14 个 crate，按同样的 `timeout 900`
补跑两批（这不是任务的硬性要求，只用于补足全 workspace 覆盖）：

| 批次 | 命令 | 退出码 | passed | failed | ignored | 目标 | 用时 |
|---|---|---|---|---|---|---|---|
| 6 | `cargo test -p kiana-json -p kiana-core -p kiana-theme -p kiana-export -p kiana-ext-api -p kiana-ui-kit -p kiana-env -p kiana-env-client` | 0 | 1428 | 0 | 1 | 129 | 155s |
| 7 | `cargo test -p kiana-resources -p kiana-extensions -p kiana-builtin-extensions -p kiana-interactive -p kiana-chord -p kiana-codemode` | 0 | 3035 | 0 | 4 | 150 | 376s |
| **合计** | 两批补充命令 | 0 | **4463** | **0** | **5** | **279** | — |

连同 §4.4 的五批，32 个 workspace 成员全部至少运行过一次自己的测试；总计
**12961 passed / 0 failed / 401 ignored / 935 个测试目标**。补充批次中的 ignored
同样是既有用例（batch6 为 `kiana-core` 的 1 个，batch7 为 4 个），未新增。

## 5. 修复记录

`crates/kiana-session/src/execution.rs` 的 `ExecutionController::recover`：

- 原代码对一个至多包含一个元素的 `sweep.actions` 使用 `for ... { match ... }`，
  且两个 match 分支都以 `return` 结束，触发默认 deny 的 `clippy::never_loop`，
  使 `cargo clippy --workspace --all-targets` 失败。
- 改为 `if let Some(action) = sweep.actions.into_iter().next() { match action { ... } }`，
  保留原有“至多处理一个 sweep 动作”的语义（`sweep_state` 在所有分支都返回
  0 或 1 个动作），没有改变行为，也没有引入新的错误路径。
- 修复后 clippy 全量重跑退出码 0；batch4 的 `kiana-session` 测试通过。

## 6. 文档核对（crate 数量与层表）

| 文档 | crate 数量 | 层表/说明 |
|---|---|---|
| `README.md` | 32 | README 不含层表；数量正确 |
| 根 `ARCHITECTURE.md` | 32 | §2.1 层表 32 个 crate，含 L6 `kiana-hooks`、`kiana-memory`；`kiana-session` 的依赖边含两者 |
| `docs/ARCHITECTURE_OVERVIEW.md` | 32 | §2 层表 32 个 crate，含 L6 `kiana-hooks`、`kiana-memory` |
| `docs/START_HERE.md` | 32 | 四处数量均为 32；层表 32 行，含 L6 两行 |

自动核对结果：把三份层表解析成 crate 名后，各自恰好是 workspace 的 32 个成员，
无缺失、无多余。

关于残留的“30/31”：

- 根 `ARCHITECTURE.md` 第 11 行与第 2164 行的“30 个 crate”明确标注为
  “骨架期（2026-10-26）快照”，紧随其后就是“当前为 32 个 crate”，属于历史记录，
  不应改写。
- 根 `ARCHITECTURE.md` 第 81/2054 行的“31 个 path 依赖”是当前正确值：
  `[workspace.dependencies]` 有 31 个 path 条目 + 88 个外部条目 = 119 项；
  32 个 workspace member 中的 `kiana-env` 是独立二进制，没有被其他 crate
  作为 path 依赖引用。
- 除上述历史/正确值外，四份文档没有残留的旧 crate 数量。

## 7. 已知缺口

均已由对应周/wire 文档声明；本次集成没有引入新缺口。

1. **启动流程接线**：daemon 尚无 `--daemon`/EventFeed 桥；cron 循环未在 `run_main`
   启动；IDE `connect_for_workspace` 未在启动流程调用；`bash background=true`
   未接到 `kiana-tools` 工具层（`wiring/phase-11b-12b.md` §6.2/6.5/6.7/6.3）。
2. **durable 适配**：`BackgroundTaskJournal`/`BackgroundTaskNotifier` 的生产实现仍是
   进程内实现，`ExecutionStore` 仍是文件实现；跨进程崩溃只能 terminalize，没有
   pid/fd 收养（`wiring/phase-11b-12b.md` §6.1、`phase-11b-background-checkpoints.md`）。
3. **记忆子系统**：抽取 job（SessionSource/Phase1/Phase2 provider/durable lease）
   未落地，`auto_extract` 只是配置；交互式记忆审批在 `default`/`accept-edits`
   下 fail-closed，记忆写入实际需要 bypass 或显式 Allow；user scope 未启用；
   召回 side query 未实现（`wiring/phase-10-11a-12a.md` §7.1–7.5）。
4. **continuity 默认关闭**：`KIANA_CONTINUITY=1` 或 `AgentSessionConfig::continuity`
   才启用执行 claim/恢复行为（`wiring/phase-11b-12b.md` §7）。
5. **TUI 渲染核心**：`KIANA_NEXT_RENDER_CORE` 仍是实验开关，默认路径未切换；
   交互式审批卡未接管 `permission_card`（`wiring/phase-10-11a-12a.md` §7.7/7.10）。
6. **结构化状态**：`BashResult` 仍没有结构化 `ExecutionStatus`；PowerShell 工具选项
   没有 `sandbox` 字段；内建 MCP extension 未注入会话级 `SandboxManager`
   （`wiring/phase-02-03.md` §5.4/5.5/5.1）。
7. **技能/MCP 管理面**：`context: fork`、技能激活 hooks/agent_scope/model/effort
   未实现；`/mcp` 的结构化状态未进入管理器详情页；MCP annotations 缺端到端
   进程内 fixture（`wiring/phase-05-06-09.md` §7.1/7.2、`phase-05-mcp-skills.md`）。
8. **子代理/隔离**：overlayfs 需要 root，无权限时显式 `Unsupported`；父工作区
   未提交改动不会自动 seed 到 worktree（`phase-11a-subagent-runtime.md`）。
9. **平台**：Windows/macOS 未做验证；沙箱语义依赖 Linux 的 bubblewrap/Landlock。
10. **既有 ignored 测试**：`kiana-ai` 的 live/provider/golden 用例、
    `kiana-coding-agent`/`kiana-rpc-print` 需要 `KIANA_BIN` 的用例等保持 ignored；
    本次没有新增 ignore。最终各批 ignored 数量见 §4.4。
11. **clippy warning**：全 workspace 仍有 197 条 warning 诊断（含 lib/lib-test
    重复），按要求未做大规模清理。

## 8. 发布建议

- 适合作为 **0.1.0 内部预览 / alpha**（Linux x86_64）发布：
  `cargo build --release -p kiana-coding-agent` 可产出 `kiana` 与 `kiana-rpc`；
  格式化、全 target check、clippy、五批任务测试 + 两批补充测试均为通过状态
  （覆盖全部 32 个 crate）。
- **不建议直接作为稳定版对外发布**。发布前至少需要：
  1. 完成第 7 节第 1–3 项的启动接线（daemon、cron、IDE、durable 适配、记忆抽取/审批），
     否则对应功能只存在于库层或需要隐藏开关；
  2. 决定 continuity 与 `KIANA_NEXT_RENDER_CORE` 的默认值，并跑一次全量回归；
  3. 本次已用任务五批 + 两批补充覆盖全部 32 个 crate；发布前仍建议在干净、磁盘
     充足（测试目标目录需要 >100 GB）的环境再跑一次 `cargo test --workspace` 复核；
  4. 至少一次真实 provider 冒烟与 macOS/Windows 构建验证；
  5. 清理或分批消化 clippy warning，避免后续变更被噪声掩盖。
- 版本号当前为 `0.1.0`（workspace `publish = false`）；建议以 tag 形式标记
  `v0.1.0-alpha`，保留回滚到当前已验证状态的能力。
