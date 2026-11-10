//! 无人值守 cron：job、调度表达式、持久化账本与 tick 调度器。
//!
//! 与 `kiana-durable` 的关系：按 ADR `docs/adr/durable.md`，cron 的事务存储落在
//! `kiana-durable` 的 [`kiana_durable::harness::record_store::JsonRecordStore`] 上——
//! 跨进程 `flock`、临时文件 + rename、`version` 乐观并发与锁内读改写都由 durable 提供。
//! 本模块只保留 cron 特有的调度算法（`pending_slot` / `next_run_at` / execution 账本 /
//! delivery claim 的**字段与推进规则**），不再自带第二套锁与 JSON 账本。
//! 接线层把 [`scheduler::CronScheduler`] 的 [`scheduler::DispatchRequest`] 转成
//! `AgentSession` 的 prompt（见 `docs/history/porting/handoffs/phase-12b.md`）。
//!
//! # 快速使用
//!
//! ```no_run
//! # async fn run() -> Result<(), Box<dyn std::error::Error>> {
//! use std::sync::Arc;
//! use kiana_coding_agent::cron::{
//!     CronConfig, CronJob, CronScheduler, CronStore, FileCronStore, JobDispatcher,
//!     ProcessFingerprint, ScheduleSpec, DispatchRequest, time::now_secs,
//! };
//!
//! struct Host;
//! #[async_trait::async_trait]
//! impl JobDispatcher for Host {
//!     async fn dispatch(&self, _request: DispatchRequest) -> Result<(), String> { Ok(()) }
//! }
//!
//! let store = Arc::new(FileCronStore::new("~/.kiana/cron")?);
//! let mut job = CronJob::new("daily", "review open PRs", ScheduleSpec::cron("0 9 * * 1-5"));
//! job.grace_minutes = 30;
//! store.save_job(&job)?;
//! let scheduler = CronScheduler::new(
//!     store,
//!     ProcessFingerprint::current(),
//!     CronConfig::default(),
//!     Arc::new(Host),
//! );
//! let report = scheduler.tick(now_secs()).await?;
//! # Ok(())
//! # }
//! ```
//!
//! # job 文件形状
//!
//! ```json
//! {
//!   "id": "daily",
//!   "prompt": "review open PRs",
//!   "schedule": {"cron": "0 9 * * 1-5"},
//!   "timezone": "UTC",
//!   "graceMinutes": 30,
//!   "skills": ["review"],
//!   "script": "daily.sh",
//!   "contextFrom": ["session-123"],
//!   "workdir": "/repo"
//! }
//! ```
//!
//! `schedule` 也接受字符串（5-field cron / ISO 8601 / `90m`）。`script` 是受管目录内的
//! 相对路径；`workdir` 必须是绝对路径。
//!
//! # 默认行为
//!
//! - 新 job 的 `next_run_at` 在第一次 tick 时初始化；
//! - `grace_minutes = 0` 表示严格按槽位，tick 晚一秒就记 `missed`（建议生产值 ≥ 5）；
//! - `duration/every` 的 catch-up 半周期分别取间隔的一半；
//! - `cron` 的 catch-up 半周期取相邻两次计划时间的一半。

pub mod runner;
pub mod schedule;
pub mod scheduler;
pub mod store;
pub mod time;

pub use runner::{
    ChannelTurnRunner, CronTurnEnvelope, CronTurnHost, CronTurnRequest, CronTurnRunner,
    DEFAULT_MAX_SCRIPT_OUTPUT_BYTES, DEFAULT_SCRIPT_TIMEOUT_SECS, DEFAULT_TICK_INTERVAL_SECS,
    SessionJobDispatcher, build_cron_scheduler, cron_turn_channel, execute_cron_turn,
    run_cron_loop, run_cron_worker, run_cron_worker_with,
};

pub use schedule::{
    CompiledSchedule, CronExpression, ScheduleError, ScheduleObject, ScheduleSpec, parse_cron,
    parse_interval,
};
pub use scheduler::{
    CATCH_UP_MAX_SECS, CATCH_UP_MIN_SECS, CronConfig, CronScheduler, DispatchRequest,
    JobDispatcher, TickReport,
};
pub use store::{
    CronError, CronExecution, CronJob, CronStore, CronStoreError, DeliveryRecord, ExecutionStatus,
    FileCronStore, MemoryCronStore, ProcessFingerprint, resolve_script_path,
};
pub use time::{FixedOffset, TimeError, format_iso8601, now_secs, parse_iso8601};
