//! 对应 packages/evals（私有）：docker 化评测运行器与报告。
//!
//! TS 源：packages/evals/src/{cli,docker,harness,plan,report}.ts
//!
//! 模块一一对应 TS 文件；入口是 `main.rs`（薄壳，逻辑在 [`cli`]）。
//!
//! # 这个 crate 解决什么问题
//!
//! 文档评测：把同一批 `.docs.eval.ts` 用例分别在 `without_docs` 与 `with_docs` 两个
//! 变体镜像里运行，收集每次运行的结果、token 用量与耗时，再按「评测集/用例 + 变体 +
//! 模型 + 重复序号」配对，汇总成 `report.json` / `report.txt`。产物目录形如 `.eval/<runId>/`，
//! 包含 `protocol.json`、`expected-runs.json`、`observations.jsonl` 与两个报告文件。
//!
//! # 为什么存在，以及为什么强调输出字节稳定
//!
//! `packages/evals` 在 TS 仓库里是私有包；本 crate 是它的 Rust 对应物，提供同一个可执行
//! 入口来构建镜像、跑用例并生成可比较的报告。各模块文档把「输出的字节级稳定」列为硬约束：
//! 错误消息要复刻 Node 的 `SystemError` / V8 `SyntaxError` 文本，JSON 字段顺序、文件
//! 排序、JS 数字格式与文件末尾换行都参与 `protocolDigest`（紧凑序列化的 sha256）或
//! 集成测试的字节比较。改动这些表面细节会改变摘要、破坏既有断言，而不只是换一种输出格式。
//!
//! # 在系统中的位置
//!
//! - 调用方：只有本 crate 的 `kiana-evals` 二进制（`src/main.rs` 调用 [`cli::run`]）和
//!   `tests/` 下的集成测试（用 `cli::run_with` 注入参数、包根与环境）。仓库内没有其它
//!   crate 依赖 `kiana-evals`。
//! - 依赖：`kiana-ai`（模型、凭证与 JS 兼容工具）、`kiana-coding-agent`（agent SDK，
//!   仅 [`harness`] 使用）、`kiana-json`（V8 兼容的 `JSON.parse`，`cli` 与 [`docker`]
//!   使用）以及 serde/tokio 等通用库。
//! - 外部进程：镜像构建与容器运行调用 `PATH` 中的 `docker`；build context 默认取 TS
//!   monorepo（环境变量 `KIANA_EVAL_TYPESCRIPT_REPOSITORY` 可覆盖）。集成测试用假
//!   docker 脚本，不需要真实 daemon。
//!
//! # 核心概念与类型
//!
//! - [`plan::DocumentationVariant`]：两个文档变体；[`plan::DOCUMENTATION_VARIANTS`]
//!   的顺序就是用例发现与报告遍历的顺序。
//! - [`plan::EvalTask`]：一个 `(用例, 变体, 模型, run)` 组合，是容器运行与报告配对的
//!   单位；[`plan::create_task_plan`] 按轮次交替两个变体的先后顺序。
//! - [`docker::DockerContext`]：镜像、产物目录、凭证路径与 provider/model 的只读组合。
//! - [`report::EvalObservation`]：单次运行的观测；[`report::EvalComparisonReport`] 是
//!   成对比较后的最终报告。
//! - [`harness`]：与「容器里跑 Vitest」并列的进程内路线，把 Kiana coding agent 包装成
//!   `@vitest-evals/core` 的 `HarnessRun` 形状；`cli` / `docker` 不调用它。
//!
//! # 一次典型调用（二进制路径）
//!
//! 1. `main.rs` 建 multi-thread tokio 运行时后调用 [`cli::run`]；`run` 读取 `argv[1..]`
//!    与进程环境，转交可注入的 `cli::run_with`。
//! 2. `cli` 解析参数/环境，选出评测文件（要求 `.docs.eval.ts` 后缀）并按 UTF-16 码元序
//!    排序；先校验 provider 凭证，再创建 `.eval/<runId>`（权限 0o700）。
//! 3. [`docker::build_images`] 构建两个变体镜像，[`docker::create_docker_context`] 组装
//!    上下文；对每个变体调用 [`docker::discover_cases`]，用 `kiana-json` 读回发现结果并
//!    比较两个变体的用例身份。
//! 4. [`plan::create_task_plan`] 展开任务矩阵；把 `protocol.json`（含 sha256 摘要）与
//!    `expected-runs.json` 写入产物目录。
//! 5. 逐个任务调用 [`docker::run_task`]：docker 退出码被忽略，`vitest.json` 存在
//!    （`Path::exists`，与 `existsSync` 相同）就读取并归一到 [`report::EvalObservation`]，
//!    不存在则用 [`report::errored_observation`]；每跑完一个任务全量重写
//!    `observations.jsonl`。
//! 6. [`report::summarize_eval_observations`] 汇总、
//!    [`report::format_eval_comparison_report`] 生成文本，写 `report.json` /
//!    `report.txt` 并打印；返回码为 1 当且仅当 `blocked_pairs`（JSON 字段
//!    `blockedPairs`）非空。
//!
//! # 边界与失败模式
//!
//! - [`cli::CliError`] 汇总参数、文件 I/O、V8 兼容 JSON 解析、plan、docker 与报告读写
//!   错误。`main.rs` 只把 `Display` 文本打印到 stderr 并以 1 退出，所以错误文本是对外
//!   接口，不能随意改写。
//! - 参数与文件校验在任何 docker 调用之前完成；`.eval/<runId>` 在镜像构建前创建，且没有
//!   清理逻辑：后续步骤失败会留下空的产物目录，重复运行会累积多个 `<runId>` 目录。
//! - 任务级问题通过观测表达而不是 `Err`：缺失 `vitest.json` 记为 `errored` 观测；只有
//!   基础设施失败（参数、I/O、docker 启动、用例发现、任务规划、会话持久化 I/O）才中止
//!   整个运行。
//! - 全流程串行：两个变体的发现、每个任务的容器运行都是顺序执行；[`docker`] 用同步
//!   `Command` 等待子进程且没有超时/取消接口，在 async 函数里调用会阻塞当前 tokio 线程。
//! - [`harness`] 直接改进程级环境变量（`HOME` / `USERPROFILE` /
//!   `KIANA_CODING_AGENT_DIR`）并在结束时恢复；调用方必须保证期间没有并发的环境读者。
//!
//! # 阅读顺序
//!
//! `src/main.rs` → [`cli`]（阶段与产物）→ [`plan`]（任务矩阵）→ [`docker`]（镜像、
//! 容器、Node 错误文本）→ [`report`]（观测归一与比较报告）。[`harness`] 不在主流程上，
//! 可最后单独阅读；`tests/` 演示如何用假 docker 与临时目录驱动整条链路。

pub mod cli;
pub mod docker;
pub mod harness;
pub mod native;
pub mod plan;
pub mod report;
