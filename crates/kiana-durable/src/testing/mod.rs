//! `testing/index.ts`：与测试 runner 无关的 Storage/ExecutionEnv 一致性用例。
//!
//! 公开路径镜像 `@earendil-works/kiana-durable/testing`。Rust 侧另外公开了：没有动态 runner
//! 时必需的 [`assertions::PanicAssertions`]/[`assertions::PanicExpect`]（见 [`assertions`]）；
//! `registerEnvConformance` 的 `options` 参数 [`EnvConformanceRegistration`]；以及
//! [`seed_storage_benchmark_with_scale`]（TS 的默认参数 `scale = TIMING_SCALE` 拆成单参数的
//! [`seed_storage_benchmark`] 加这个显式规模的入口）。此外 `assertions` 子模块还公开了
//! `ExpectResult`、`RejectsFuture`、`is_truthy`、`matches_subset`，`storage_benchmark` 还公开了
//! `StorageBenchmarkSeedError` 与 `BenchmarkFuture`。
//!
//! # 这个模块解决什么问题
//!
//! `Storage` 有内存、JSONL、SQLite 等后端，`ExecutionEnv` 有本地 Node 与远程 env-client 等
//! 实现。如果每个实现各写一套测试，契约会随实现漂移，且很难判断某个差异是 bug 还是后端特性。
//! 这里把“一个合格实现必须表现出的行为”固定成与测试框架无关的用例数组：实现方只需提供
//! “怎么拿到一个干净实例”的 provider，失败如何报告由注入的
//! [`StorageConformanceAssertions`] 决定。
//!
//! # 模块结构
//!
//! - [`types`]：断言 trait、provider 别名、用例与选项等共享数据结构；
//! - [`assertions`]：把 Vitest/Jest 形状的 `expect` 适配成断言集合，并给出会 panic 的默认实现；
//! - [`storage_conformance`] / [`env_conformance`]：两组一致性用例（`create_*` 构造数组）；
//! - [`runner`]：`describe`/`it` 形状的注册适配器（`register_*`）；
//! - [`storage_benchmark`]：确定性的基准数据集与读写基准表，不参与一致性套件。
//!
//! # 执行流程
//!
//! 1. `create_storage_conformance` / `create_env_conformance` 按 TS 顺序构造用例数组；
//!    构造期不做 I/O，也不调用 provider；
//! 2. `register_storage_conformance` / `register_env_conformance` 把同一批用例注册到
//!    [`StorageConformanceRunner`]，或把数组直接返回给调用方用 `#[test]`/`#[tokio::test]` 循环执行；
//! 3. 执行某个用例时，用例通过 provider 申请实例，把实例与断言交给用例体；provider 负责创建
//!    与清理，本模块既不缓存实例也不在用例之间清理状态。
//!
//! # 注意事项
//!
//! - 用例名与顺序逐字来自 TS，`tests/storage_conformance.rs` 用标题数组锁定；改名或重排会让
//!   该测试失败，也会破坏与 TS 的逐条对应。
//! - provider 必须为每次执行提供干净、隔离的实例（storage 未写入 / env 的 `cwd` 是全新空目录）；
//!   这一点由用例断言体现，类型系统无法强制。
//! - 断言集合与用例闭包都要求 `Send + Sync`，因此用例可以交给多线程 runner；是否真正并发
//!   执行由调用方决定。

// --- 子模块 ---
pub mod assertions;
pub mod env_conformance;
pub mod runner;
pub mod storage_benchmark;
pub mod storage_conformance;
pub mod types;

// --- 与 index.ts 对应的重导出 ---
// `create_*` 构造用例数组，`register_*` 是 runner 适配；除上面列出的 Rust 专有条目
// （`EnvConformanceRegistration`、`seed_storage_benchmark_with_scale`、`PanicAssertions`/
// `PanicExpect` 等）外，其余与 TS 的 `testing/index.ts` 同名，`storage_benchmark` 的表在首次
// 访问时才惰性构造。
pub use assertions::{ExpectLike, create_expect_assertions};
pub use env_conformance::create_env_conformance;
pub use runner::{
    EnvConformanceRegistration, StorageConformanceRunner, register_env_conformance,
    register_storage_conformance,
};
pub use storage_benchmark::{
    STORAGE_MEMORY_SCALES, STORAGE_READ_BENCHMARKS, STORAGE_WRITE_BENCHMARKS,
    StorageBenchmarkDataset, StorageBenchmarkScale, StorageReadBenchmark, StorageWriteBenchmark,
    TIMING_SCALE, seed_storage_benchmark, seed_storage_benchmark_with_scale,
    seed_storage_write_benchmark, storage_benchmark_primary_record_count,
};
pub use storage_conformance::create_storage_conformance;
pub use types::{
    EnvConformanceAssertions, EnvConformanceCase, EnvConformanceOptions, EnvConformanceProvider,
    StorageConformanceAssertions, StorageConformanceCase, StorageConformanceOptions,
    StorageConformanceProvider,
};
