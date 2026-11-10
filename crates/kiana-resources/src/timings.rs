//! `KIANA_TIMING` 启动计时的再导出面。
//!
//! 定义在 `kiana_core::timings`（`core/timings.ts`）。`kiana-extensions`（L9）的 `loader.ts` 也调用
//! `time("extensions")`，因此不能由 L10 的 `kiana-resources` 持有（`ARCHITECTURE.md` §3.5）。
//!
//! 与 TS 的唯一签名差别：`resetTimings(namespace = "main")` / `time(label, namespace = "main")`
//! 的默认参数在 Rust 里不存在，调用方显式传 [`TimingLabel::Main`]（或 `Extensions`）。
//! 这里再导出枚举本身，而不是另加一层 `*_default` 包装，避免出现两个名字相同的入口。
//!
//! # 调用约定
//! - `reset_timings(TimingLabel)` 建立/清空一个分组；
//! - `time(label, TimingLabel)` 记录一条“距上一条”的毫秒数；
//! - `print_timings()` 把全部分组写到 **stderr**。
//!
//! `KIANA_TIMING` 只在进程首次调用任一函数时求值一次：值为 `"1"` 才启用，否则三个函数
//! 都是空操作（不建表、不输出）。输出按 namespace 插入顺序，空分组连标题都不打印。
//!
//! # 常见误用
//! - 忘记先 `reset_timings`：`time` 会自动建组，但第一条的毫秒可能是 0 或负数，这是与 TS
//!   双次取时对齐的原样行为，不是错误；
//! - 期望有默认 namespace：Rust 没有默认参数，必须显式传 `TimingLabel::Main` 或
//!   `TimingLabel::Extensions`；
//! - 并行测试同时改写全局计时表会互相干扰，调用方需自行串行化。
//!
//! 并发保证与逐条 TS 语义见 `kiana_core::timings` 的模块文档；本文件只决定再导出哪些名字。

pub use kiana_core::timings::{TimingLabel, print_timings, reset_timings, time};
