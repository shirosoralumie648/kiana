# week-11a handoff

无。

本周没有修改根 `Cargo.toml`、`Cargo.lock`、`crates/kiana-session-core/**` 的共享契约，
也没有需要 wire agent 统一应用的接线；`crates/kiana-session/src/agent_session.rs` 由本
lane 直接修改（本周所有权包含该文件）。`crates/kiana-session/Cargo.toml` 未新增任何
依赖：sha256 在 `subagent/result.rs` 内用标准 FIPS 180-4 实现，`ioctl`/`mount`/
`umount2`/`geteuid` 在 `subagent/isolation.rs` 的 `sys` 模块里直接 `extern "C"` 声明。

供后续 lane 使用的公开入口（直接调用，不需要 wire）：

- `AgentSession::subagent_parent_context()` / `prepare_subagent()` /
  `resume_subagent()`：创建/恢复子代理。
- `kiana_session::subagent::isolation::list_ownership_records(state_dir)`：week11b 启动
  sweep 可用来枚举待清理的隔离工作区；记录加载时会拒绝状态目录外的 workspace 路径。
- `kiana_session::subagent::result::{TerminalCell, UsageAccumulator, ReceiptAccumulator}`：
  后台任务的终态与用量/凭证累积。
- `kiana_session::subagent::yield_tool::YieldAssembler`：yield 校验与 label 装配。
