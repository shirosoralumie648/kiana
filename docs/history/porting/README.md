# 并行移植过程记录（2026-11-07 / 2026-11-08）

这是历史过程记录，不是现行契约。现行文档请看 `docs/START_HERE.md` 与根 `ARCHITECTURE.md`。

## 命名说明

- `phase-NN-*.md`：并行移植工作流的阶段记录。
- `handoffs/phase-NN.md`：各阶段留给跨 crate 接线 agent 的清单。
- `wiring/phase-*.md`：阶段间集成验证记录。
- `fix-passes/pass-*.md`：审计发现的分批深度修复记录。
- `integration-*.md`：最终验收/审计（`integration-e2e.md`、`integration-final.md`、`integration-audit.md`）。

## 阶段索引

| phase 文件 | 主题 | 主要 crate |
|---|---|---|
| `phase-00-scaffold.md` | 新 crate 脚手架与 workspace 接线 | `kiana-hooks`、`kiana-memory` |
| `phase-00-convergence.md` | 半成品收敛：权限启用入口、运行期切换、`execute_bash` 权限路径、文档对齐 | `kiana-permissions`、`kiana-session`、`kiana-coding-agent` |
| `phase-00-convergence-verify.md` | 第 0 周收敛的验证报告 | `kiana-session`、`kiana-permissions`、`kiana-coding-agent` |
| `phase-01-permissions-approval.md` | 结构化审批（AllowOnce / AllowAlways / Reject）与持久化 | `kiana-permissions`、`kiana-session`、`kiana-config` |
| `phase-02-sandbox-execution-paths.md` | 所有执行路径走沙箱 + read-before-write | `kiana-sandbox`、`kiana-tools` |
| `phase-03-config-shell-risk.md` | 配置分层与 shell 风险分析 | `kiana-config`、`kiana-permissions` |
| `phase-04-hooks.md` | Hooks 配置、派发与执行契约 | `kiana-hooks` |
| `phase-05-mcp-skills.md` | MCP 管理面与 Skills 按需加载 | `kiana-mcp`、`kiana-resources`、`kiana-builtin-extensions` |
| `phase-06-compaction-output.md` | 上下文压缩与工具输出预算 | `kiana-compaction`、`kiana-core`、`kiana-tools` |
| `phase-07-editing-experience.md` | 编辑体验（`multi_edit` / `apply_patch` 等） | `kiana-tools` |
| `phase-08-evals-loop-guard.md` | 评测、mock LLM 与 loop guard | `kiana-ai`、`kiana-agent-core`、`kiana-evals` |
| `phase-09-request-policy-otel.md` | Provider 请求策略与 OTel | `kiana-ai`、`kiana-telemetry`、`kiana-agent-core` |
| `phase-10-persistent-memory.md` | 持久记忆 | `kiana-memory` |
| `phase-11a-subagent-runtime.md` | 子代理运行时（第 11 周前半） | `kiana-session` |
| `phase-11b-background-checkpoints.md` | 后台任务、验收、恢复与 checkpoint（第 11 周后半） | `kiana-session`、`kiana-session-core` |
| `phase-12a-tui-frame-contract.md` | TUI 帧契约与审批界面 | `kiana-interactive`、`kiana-coding-agent` |
| `phase-12b-completion.md` | Web / IDE / daemon / cron 补完 | `kiana-coding-agent`、`kiana-rpc-print`、`kiana-interactive` |

## 接线、修复与集成索引

- `handoffs/phase-02.md` … `handoffs/phase-12b.md`：13 份跨 crate 接线清单，与顶层 phase 文件一一对应。
- `wiring/phase-02-03.md`、`phase-04-07-08.md`、`phase-05-06-09.md`、`phase-10-11a-12a.md`、`phase-11b-12b.md`：5 次波次集成验证。
- `fix-passes/pass-1-cross-cutting.md`、`pass-2-phases-02-03.md`、`pass-3-phases-04-08.md`、`pass-4-phases-05-06-09.md`、`pass-5-phase-12.md`、`pass-6-phases-11b-12b.md`：6 批审计修复。
- `integration-e2e.md`、`integration-final.md`、`integration-audit.md`：端到端验收、最终验证、发布前审计。
- `baseline.md`：工作流开始前的构建基线。

## 关于旧文件名与测试名

原文件名 `week-N.md` / `W*.md` / `scaffold.md` / `wire-X.md` / `deep-fix-X.md` / `AUDIT.md` / `E2E.md` / `FINAL.md` / `BASELINE.md` 已于 2026-11-08 重命名；正文保留当时的周次叙述（如“第 6 周”“Wave B”“Wire C”）。

当时引用的测试二进制 `week6_*` / `week11_*` / `week12_*` / `wire_c_wiring` 现名为 `pipeline` / `pruning` / `acceptance` / `background` / `checkpoint` / `execution` / `subagent_runtime` / `subagent_runner` / `cross_crate_wiring`。

移植计划原文见 `plan.md`（原文件名 `PORTING_PLAN.md`）。
