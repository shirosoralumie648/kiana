//! `packages/durable/test/storage.bench.ts` 的移植。
//!
//! TS 用 vitest 的 `bench` 跑三个后端（memory / sqlite / jsonl）。Rust 稳定版没有基准框架，
//! 因此本文件把它们移植成 **`#[ignore]` 测试**：先验证每个场景的观测值等于期望值（这是
//! bench 文件里 `strictEqual` 的断言），再用 `Instant` 做固定次数的计时并把结果打印出来。
//!
//! 运行：`cargo test -p kiana-durable --features sqlite --test storage_bench -- --ignored --nocapture`
//!
//! 依赖 jsonl 与 env-node（与 TS 一样跨三个后端）。

#![cfg(feature = "sqlite")]

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use kiana_chord::context::{BACKGROUND_CONTEXT, Context};
use kiana_durable::Storage;
use kiana_durable::storage::jsonl::{JsonlStorageOptions, open_node_jsonl_storage};
use kiana_durable::storage::memory::MemoryStorage;
use kiana_durable::storage::sqlite::node::{NodeSqliteStorageOptions, open_node_sqlite_storage};
use kiana_durable::testing::{
    STORAGE_READ_BENCHMARKS, STORAGE_WRITE_BENCHMARKS, seed_storage_benchmark,
    seed_storage_write_benchmark,
};

fn context() -> &'static dyn Context {
    &**BACKGROUND_CONTEXT
}

fn ok<T, E: std::fmt::Display>(result: Result<T, E>) -> T {
    match result {
        Ok(value) => value,
        Err(error) => panic!("{error}"),
    }
}

/// 独占的临时目录；`Drop` 时尽力删除。
struct TempDir {
    path: String,
}

impl TempDir {
    fn new(tag: &str) -> Self {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let counter = COUNTER.fetch_add(1, Ordering::SeqCst);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or(0);
        let path = std::env::temp_dir().join(format!(
            "kiana-durable-benchmark-{tag}-{}-{nanos}-{counter}",
            std::process::id()
        ));
        std::fs::create_dir_all(&path).expect("create temp dir");
        TempDir {
            path: path.to_string_lossy().into_owned(),
        }
    }

    fn join(&self, name: &str) -> String {
        format!("{}/{}", self.path, name)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Backend {
    Memory,
    Sqlite,
    Jsonl,
}

impl Backend {
    fn name(self) -> &'static str {
        match self {
            Backend::Memory => "memory",
            Backend::Sqlite => "sqlite",
            Backend::Jsonl => "jsonl",
        }
    }
}

struct Fixture {
    storage: Arc<dyn Storage>,
    /// 保持临时目录存活直到存储关闭。
    _directory: Option<TempDir>,
}

async fn create_fixture(backend: Backend) -> Fixture {
    match backend {
        Backend::Memory => Fixture {
            storage: Arc::new(MemoryStorage::new()),
            _directory: None,
        },
        Backend::Sqlite => {
            let directory = TempDir::new("sqlite");
            let path = directory.join("storage.sqlite");
            let storage =
                ok(open_node_sqlite_storage(&path, NodeSqliteStorageOptions::default()).await);
            Fixture {
                storage: Arc::new(storage),
                _directory: Some(directory),
            }
        }
        Backend::Jsonl => {
            let directory = TempDir::new("jsonl");
            let path = directory.join("storage");
            let storage =
                ok(open_node_jsonl_storage(&path, context(), JsonlStorageOptions::default()).await);
            Fixture {
                storage: Arc::new(storage),
                _directory: Some(directory),
            }
        }
    }
}

const BACKENDS: [Backend; 3] = [Backend::Memory, Backend::Sqlite, Backend::Jsonl];
/// TS: `READ_OPTIONS = { time: 300, iterations: 10, warmupTime: 75, warmupIterations: 3 }`。
const READ_ITERATIONS: usize = 10;
const READ_WARMUP_ITERATIONS: usize = 3;
/// TS: `WRITE_OPTIONS = { time: 0, iterations: 20, warmupTime: 0, warmupIterations: 5 }`。
const WRITE_ITERATIONS: usize = 20;
const WRITE_WARMUP_ITERATIONS: usize = 5;
/// TS: `REOPEN_OPTIONS = { time: 0, iterations: 20, warmupTime: 0, warmupIterations: 3 }`。
const REOPEN_ITERATIONS: usize = 20;
const REOPEN_WARMUP_ITERATIONS: usize = 3;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "benchmark: run with --ignored --nocapture"]
async fn read_benchmarks_match_expected_results_across_backends() {
    for backend in BACKENDS {
        let fixture = create_fixture(backend).await;
        let dataset = ok(seed_storage_benchmark(fixture.storage.as_ref()).await);
        // `strictEqual(await scenario.run(...), scenario.expected(dataset))`。
        for scenario in STORAGE_READ_BENCHMARKS.iter() {
            let observed = (scenario.run)(Arc::clone(&fixture.storage), dataset.clone()).await;
            let expected = (scenario.expected)(&dataset);
            assert_eq!(observed, expected, "{}: {}", backend.name(), scenario.name);
        }
        // warmup（不计时）。
        for _ in 0..READ_WARMUP_ITERATIONS {
            for scenario in STORAGE_READ_BENCHMARKS.iter() {
                let observed = (scenario.run)(Arc::clone(&fixture.storage), dataset.clone()).await;
                assert_eq!(observed, (scenario.expected)(&dataset));
            }
        }
        for scenario in STORAGE_READ_BENCHMARKS.iter() {
            let started = Instant::now();
            for _ in 0..READ_ITERATIONS {
                let observed = (scenario.run)(Arc::clone(&fixture.storage), dataset.clone()).await;
                assert_eq!(observed, (scenario.expected)(&dataset));
            }
            println!(
                "read benchmark {} / {}: {:?} for {READ_ITERATIONS} iterations",
                scenario.name,
                backend.name(),
                started.elapsed()
            );
        }
        fixture.storage.close(context()).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "benchmark: run with --ignored --nocapture"]
async fn write_benchmarks_match_expected_results_across_backends() {
    for scenario in STORAGE_WRITE_BENCHMARKS.iter() {
        for backend in BACKENDS {
            // 校验 fixture：种子后运行一次，必须等于期望写入条数。
            let validation = create_fixture(backend).await;
            ok(seed_storage_write_benchmark(validation.storage.as_ref()).await);
            assert_eq!(
                (scenario.run)(Arc::clone(&validation.storage)).await,
                scenario.expected,
                "{}: {}",
                backend.name(),
                scenario.name
            );
            validation.storage.close(context()).await;

            // warmup（不计时）：TS 的池包含 warmupIterations 个不计入统计的 fixture。
            for _ in 0..WRITE_WARMUP_ITERATIONS {
                let fixture = create_fixture(backend).await;
                ok(seed_storage_write_benchmark(fixture.storage.as_ref()).await);
                assert_eq!(
                    (scenario.run)(Arc::clone(&fixture.storage)).await,
                    scenario.expected,
                    "{}: {}",
                    backend.name(),
                    scenario.name
                );
                fixture.storage.close(context()).await;
            }

            // 计时：每个迭代使用全新的 fixture（TS 的池在迭代前一次性建好）。
            let mut elapsed = std::time::Duration::ZERO;
            for _ in 0..WRITE_ITERATIONS {
                let fixture = create_fixture(backend).await;
                ok(seed_storage_write_benchmark(fixture.storage.as_ref()).await);
                let started = Instant::now();
                let observed = (scenario.run)(Arc::clone(&fixture.storage)).await;
                elapsed += started.elapsed();
                assert_eq!(
                    observed,
                    scenario.expected,
                    "{}: {}",
                    backend.name(),
                    scenario.name
                );
                fixture.storage.close(context()).await;
            }
            println!(
                "write benchmark {} / {}: {elapsed:?} for {WRITE_ITERATIONS} iterations",
                scenario.name,
                backend.name()
            );
        }
    }
}

fn copy_directory(source: &str, destination: &str) {
    std::fs::create_dir_all(destination).expect("create destination");
    for entry in std::fs::read_dir(source).expect("read source directory") {
        let entry = entry.expect("directory entry");
        let file_type = entry.file_type().expect("file type");
        let target = format!("{destination}/{}", entry.file_name().to_string_lossy());
        if file_type.is_dir() {
            copy_directory(&entry.path().to_string_lossy(), &target);
        } else {
            std::fs::copy(entry.path(), target).expect("copy file");
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "benchmark: run with --ignored --nocapture"]
async fn reopen_and_first_exact_read_across_persistent_backends() {
    for backend in [Backend::Sqlite, Backend::Jsonl] {
        // 种子存储：只写一次，之后每个样本都从复制品重开。
        let seed_directory = TempDir::new("reopen-seed");
        let seed_path = seed_directory.join(match backend {
            Backend::Sqlite => "storage.sqlite",
            _ => "storage",
        });
        let seed: Arc<dyn Storage> = match backend {
            Backend::Sqlite => Arc::new(ok(open_node_sqlite_storage(
                &seed_path,
                NodeSqliteStorageOptions::default(),
            )
            .await)),
            _ => Arc::new(ok(open_node_jsonl_storage(
                &seed_path,
                context(),
                JsonlStorageOptions::default(),
            )
            .await)),
        };
        let dataset = ok(seed_storage_benchmark(seed.as_ref()).await);
        seed.close(context()).await;

        // 与 TS 一样，样本在计时前全部复制好（`iterations + warmupIterations` 个）。
        let sample_count = REOPEN_WARMUP_ITERATIONS + REOPEN_ITERATIONS;
        let mut samples: Vec<(TempDir, String)> = Vec::with_capacity(sample_count);
        for _ in 0..sample_count {
            let sample_directory = TempDir::new("reopen-sample");
            let sample_path = sample_directory.join(match backend {
                Backend::Sqlite => "storage.sqlite",
                _ => "storage",
            });
            match backend {
                Backend::Sqlite => {
                    std::fs::copy(&seed_path, &sample_path).expect("copy sqlite database");
                }
                _ => copy_directory(&seed_path, &sample_path),
            }
            samples.push((sample_directory, sample_path));
        }

        for (index, (_sample_directory, sample_path)) in samples.iter().enumerate() {
            // TS 的 `reopenAndRead` 把 `openPersistentStorage(...)` 与第一次精确读都算在样本内；
            // 重开（含迁移）正是这个基准要测的成本，不能把它排除在计时之外。
            let timed = index >= REOPEN_WARMUP_ITERATIONS;
            let started = Instant::now();
            let storage: Arc<dyn Storage> = match backend {
                Backend::Sqlite => Arc::new(ok(open_node_sqlite_storage(
                    sample_path,
                    NodeSqliteStorageOptions::default(),
                )
                .await)),
                _ => Arc::new(ok(open_node_jsonl_storage(
                    sample_path,
                    context(),
                    JsonlStorageOptions::default(),
                )
                .await)),
            };
            let entry = ok(storage.entry(dataset.first_entry_id, context()).await);
            let elapsed = started.elapsed();
            assert_eq!(
                entry.map_or(-1, |entry| entry.entry.id.get()),
                dataset.first_entry_id.get(),
                "{}: reopen read",
                backend.name()
            );
            if timed {
                println!("reopen benchmark / {}: {elapsed:?}", backend.name());
            }
            storage.close(context()).await;
        }
    }
}
