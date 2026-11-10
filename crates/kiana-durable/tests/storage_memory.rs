//! `packages/durable/test/storage-memory.ts` 的移植。
//!
//! TS 的驱动脚本对每个 `(backend, scale)` 组合都 **fork 一个 Node 进程**（`--expose-gc`），
//! 在子进程里测量 `process.memoryUsage()` 的 `heapUsed`/`rss`/`external` 增量，再把每个 worker
//! 打印的 JSON 汇总成表格。本文件保持同样的结构：
//!
//! - [`storage_memory_worker`]：`#[ignore]` 的 worker，从环境变量 `KIANA_STORAGE_MEMORY_BACKEND` /
//!   `KIANA_STORAGE_MEMORY_SCALE` 读取参数（未设置时直接返回，供 `--ignored` 全量运行使用）。
//!   它先取 RSS 基线，种子化数据，验证每个读取基准并与 TS 一样再跑 9 轮，最后输出磁盘占用。
//! - [`storage_memory_footprint_across_backends_and_scales`]：驱动，`current_exe` 为每个组合
//!   spawn 一个 worker 子进程（用 `current_exe` 启动独立进程），解析报告并打印
//!   与 TS 相同的表格列。
//!
//! **Rust 差异**：Rust 没有 JS 的 `heapUsed`/`external` 计数器，也没有可调用的 `gc()`，因此
//! `heap`/`external` 列一律显示 `-`，内存数据只有进程 RSS（读 `/proc/self/status` 的 `VmRSS`），
//! 这是本移植能给出的、与 `rss` 列等价的部分。其余列（记录数、磁盘主/辅文件字节、文件数、
//! sqlite `page_count`/`freelist_count`、jsonl 文档/任务文件数）保持相同语义。
//!
//! 运行：`cargo test -p kiana-durable --features sqlite --test storage_memory -- --ignored --nocapture`

#![cfg(feature = "sqlite")]

use std::sync::Arc;

use kiana_chord::context::{BACKGROUND_CONTEXT, Context};
use kiana_chord::sqlite::SqliteExecutor;
use kiana_durable::Storage;
use kiana_durable::storage::jsonl::{JsonlStorageOptions, open_node_jsonl_storage};
use kiana_durable::storage::memory::MemoryStorage;
use kiana_durable::storage::sqlite::node::{NodeSqliteDatabase, open_node_sqlite_database};
use kiana_durable::storage::sqlite::{SqliteStorage, SqliteStorageOptions};
use kiana_durable::testing::{
    STORAGE_MEMORY_SCALES, STORAGE_READ_BENCHMARKS, StorageBenchmarkDataset, StorageBenchmarkScale,
    seed_storage_benchmark_with_scale, storage_benchmark_primary_record_count,
};

const BACKENDS: [&str; 3] = ["memory", "sqlite", "jsonl"];
const REPORT_PREFIX: &str = "KIANA_STORAGE_MEMORY_REPORT ";

fn context() -> &'static dyn Context {
    &**BACKGROUND_CONTEXT
}

fn ok<T, E: std::fmt::Display>(result: Result<T, E>) -> T {
    match result {
        Ok(value) => value,
        Err(error) => panic!("{error}"),
    }
}

/// 独占的临时目录；`Drop` 时尽力删除（TS 的 `finally { rm(directory) }`）。
struct TempDir {
    path: String,
}

impl TempDir {
    fn new(tag: &str) -> Self {
        static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let counter = COUNTER.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or(0);
        let path = std::env::temp_dir().join(format!(
            "kiana-durable-{tag}-{}-{nanos}-{counter}",
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

/// `process.memoryUsage().rss` 的 Rust 等价物：`/proc/self/status` 的 `VmRSS`。
fn rss_bytes() -> u64 {
    let status = ok(std::fs::read_to_string("/proc/self/status"));
    for line in status.lines() {
        if let Some(rest) = line.strip_prefix("VmRSS:") {
            return rest
                .split_whitespace()
                .next()
                .and_then(|value| value.parse::<u64>().ok())
                .unwrap_or(0)
                * 1024;
        }
    }
    0
}

fn file_size(path: &str) -> u64 {
    std::fs::metadata(path)
        .map(|metadata| metadata.len())
        .unwrap_or(0)
}

/// worker 的测量结果。
struct Measurement {
    backend: &'static str,
    scale: &'static str,
    record_count: usize,
    /// 基线与两次采样（字节，进程 RSS）。
    baseline: u64,
    post_seed: u64,
    post_read: u64,
    disk: Option<serde_json::Value>,
}

impl Measurement {
    fn report(&self) -> serde_json::Value {
        let mut value = serde_json::json!({
            "backend": self.backend,
            "scale": self.scale,
            "recordCount": self.record_count,
            "baselineRss": self.baseline,
            "postSeedRss": self.post_seed,
            "postReadRss": self.post_read,
        });
        if let Some(disk) = &self.disk {
            value["disk"] = disk.clone();
        }
        value
    }
}

/// 验证读取基准并返回 checksum；语义同 TS：
/// 先逐项 `result === expected`，再额外跑 9 轮累加，最后要求 checksum 有限。
async fn run_read_benchmarks(storage: &Arc<dyn Storage>, dataset: &StorageBenchmarkDataset) {
    let mut checksum = 0.0;
    for scenario in STORAGE_READ_BENCHMARKS.iter() {
        let result = (scenario.run)(Arc::clone(storage), dataset.clone()).await;
        let expected = (scenario.expected)(dataset);
        assert_eq!(
            result, expected,
            "Invalid benchmark result: {}",
            scenario.name
        );
        checksum += result;
    }
    for _ in 1..10 {
        for scenario in STORAGE_READ_BENCHMARKS.iter() {
            checksum += (scenario.run)(Arc::clone(storage), dataset.clone()).await;
        }
    }
    assert!(
        checksum.is_finite(),
        "Storage memory read checksum is invalid"
    );
}

/// 磁盘探测（在读取完成、存储关闭前进行）。
enum DiskProbe<'a> {
    Sqlite {
        database: &'a NodeSqliteDatabase,
        path: String,
    },
    Jsonl {
        directory: String,
    },
}

async fn disk_footprint(probe: DiskProbe<'_>) -> serde_json::Value {
    match probe {
        DiskProbe::Sqlite { database, path } => {
            let page_count = database
                .get("SELECT page_count AS value FROM pragma_page_count()", &[])
                .await
                .ok()
                .flatten()
                .and_then(|row| row.get_i64("value"));
            let freelist_count = database
                .get(
                    "SELECT freelist_count AS value FROM pragma_freelist_count()",
                    &[],
                )
                .await
                .ok()
                .flatten()
                .and_then(|row| row.get_i64("value"));
            serde_json::json!({
                "kind": "sqlite",
                // TS 固定报告 3 个文件（主库 + -wal + -shm）。
                "mainBytes": file_size(&path),
                "auxiliaryBytes": file_size(&format!("{path}-wal")) + file_size(&format!("{path}-shm")),
                "fileCount": 3,
                "pageCount": page_count,
                "freelistCount": freelist_count,
            })
        }
        DiskProbe::Jsonl { directory } => {
            let mut main_bytes = 0_u64;
            let mut auxiliary_bytes = 0_u64;
            let mut file_count = 0_u64;
            let mut document_files = 0_u64;
            let mut task_files = 0_u64;
            let entries = ok(std::fs::read_dir(&directory));
            for entry in entries {
                let entry = ok(entry);
                if !ok(entry.file_type()).is_file() {
                    continue;
                }
                let name = entry.file_name().to_string_lossy().into_owned();
                let bytes = file_size(&entry.path().to_string_lossy());
                file_count += 1;
                if name == "main.jsonl" {
                    main_bytes += bytes;
                } else {
                    auxiliary_bytes += bytes;
                }
                if name.starts_with("doc-") && name.ends_with(".jsonl") {
                    document_files += 1;
                }
                if name.starts_with("task-") && name.ends_with(".jsonl") {
                    task_files += 1;
                }
            }
            serde_json::json!({
                "kind": "jsonl",
                "mainBytes": main_bytes,
                "auxiliaryBytes": auxiliary_bytes,
                "fileCount": file_count,
                "documentFiles": document_files,
                "taskFiles": task_files,
            })
        }
    }
}

async fn measure(
    storage: Arc<dyn Storage>,
    backend: &'static str,
    scale: &StorageBenchmarkScale,
    probe: Option<DiskProbe<'_>>,
) -> Measurement {
    let baseline = rss_bytes();
    let dataset = ok(seed_storage_benchmark_with_scale(storage.as_ref(), scale).await);
    let post_seed = rss_bytes();
    run_read_benchmarks(&storage, &dataset).await;
    let post_read = rss_bytes();
    let disk = match probe {
        Some(probe) => Some(disk_footprint(probe).await),
        None => None,
    };
    Measurement {
        backend,
        scale: scale.name,
        record_count: storage_benchmark_primary_record_count(scale),
        baseline,
        post_seed,
        post_read,
        disk,
    }
}

async fn measure_memory(scale: &StorageBenchmarkScale) -> Measurement {
    let storage: Arc<dyn Storage> = Arc::new(MemoryStorage::new());
    measure(storage, "memory", scale, None).await
}

async fn measure_sqlite(scale: &StorageBenchmarkScale) -> Measurement {
    let directory = TempDir::new("sqlite-memory");
    let path = directory.join("storage.sqlite");
    let database = ok(open_node_sqlite_database(&path, Default::default()).await);
    let storage: Arc<dyn Storage> = Arc::new(ok(SqliteStorage::open(
        Arc::new(database.clone()),
        SqliteStorageOptions::default(),
    )
    .await));
    let measurement = measure(
        Arc::clone(&storage),
        "sqlite",
        scale,
        Some(DiskProbe::Sqlite {
            database: &database,
            path,
        }),
    )
    .await;
    storage.close(context()).await;
    measurement
}

async fn measure_jsonl(scale: &StorageBenchmarkScale) -> Measurement {
    let directory = TempDir::new("jsonl-memory");
    let path = directory.join("storage");
    let storage: Arc<dyn Storage> = Arc::new(ok(open_node_jsonl_storage(
        &path,
        context(),
        JsonlStorageOptions::default(),
    )
    .await));
    let measurement = measure(
        Arc::clone(&storage),
        "jsonl",
        scale,
        Some(DiskProbe::Jsonl { directory: path }),
    )
    .await;
    storage.close(context()).await;
    measurement
}

async fn run_measurement(backend: &str, scale: &StorageBenchmarkScale) -> Measurement {
    match backend {
        "memory" => measure_memory(scale).await,
        "sqlite" => measure_sqlite(scale).await,
        "jsonl" => measure_jsonl(scale).await,
        other => panic!("Unknown storage benchmark backend: {other}"),
    }
}

/// worker：由驱动通过环境变量调用；没有环境变量时（例如 `--ignored` 全量运行）直接返回。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "memory measurement worker: spawned by storage_memory_footprint_across_backends_and_scales"]
async fn storage_memory_worker() {
    let Ok(backend) = std::env::var("KIANA_STORAGE_MEMORY_BACKEND") else {
        return;
    };
    let scale_name = std::env::var("KIANA_STORAGE_MEMORY_SCALE")
        .expect("KIANA_STORAGE_MEMORY_SCALE is set by the driver");
    if !BACKENDS.contains(&backend.as_str()) {
        panic!("Unknown storage benchmark backend: {backend}");
    }
    let scale = STORAGE_MEMORY_SCALES
        .iter()
        .find(|candidate| candidate.name == scale_name)
        .unwrap_or_else(|| panic!("Unknown storage benchmark scale: {scale_name}"));
    let measurement = run_measurement(&backend, scale).await;
    println!(
        "{REPORT_PREFIX}{}",
        serde_json::to_string(&measurement.report()).expect("serialize report")
    );
}

fn mebibytes(bytes: i64) -> String {
    format!("{:.2}", bytes as f64 / 1024.0 / 1024.0)
}

fn delta(after: u64, before: u64) -> i64 {
    after as i64 - before as i64
}

/// 驱动：为每个 `(backend, scale)` spawn 一个 worker 子进程，解析它的报告并打印表格。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "memory measurement: run with --ignored --nocapture"]
async fn storage_memory_footprint_across_backends_and_scales() {
    let executable = std::env::current_exe().expect("current test executable");
    let mut results: Vec<serde_json::Value> = Vec::new();
    for backend in BACKENDS {
        for scale in STORAGE_MEMORY_SCALES {
            let output = std::process::Command::new(&executable)
                .args([
                    "--exact",
                    "storage_memory_worker",
                    "--ignored",
                    "--nocapture",
                    "--test-threads=1",
                ])
                .env("KIANA_STORAGE_MEMORY_BACKEND", backend)
                .env("KIANA_STORAGE_MEMORY_SCALE", scale.name)
                .output()
                .expect("spawn storage memory worker");
            let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
            let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
            assert!(
                output.status.success(),
                "worker {backend}/{} failed:\n{stdout}\n{stderr}",
                scale.name
            );
            let line = stdout
                .lines()
                .find_map(|line| {
                    line.find(REPORT_PREFIX)
                        .map(|index| &line[index + REPORT_PREFIX.len()..])
                })
                .unwrap_or_else(|| {
                    panic!(
                        "worker {backend}/{} printed no report:\n{stdout}\n{stderr}",
                        scale.name
                    )
                });
            results.push(serde_json::from_str(line).expect("parse worker report"));
        }
    }

    println!(
        "Storage footprint after deterministic synthetic workloads; values are process deltas, not limits."
    );
    println!(
        "(Rust has no JS heap/external counters; those columns are \"-\", RSS comes from /proc/self/status.)"
    );
    println!(
        "{:<8} {:<6} {:>8} {:>18} {:>19} {:>18} {:>14} {:>19} {:>15} {:>11}  disk detail",
        "backend",
        "scale",
        "records",
        "RSS after seed MiB",
        "RSS after reads MiB",
        "RSS bytes/record",
        "heap MiB",
        "disk main/aux MiB",
        "disk total MiB",
        "disk files"
    );
    for result in &results {
        let record_count = result["recordCount"].as_u64().unwrap_or(0);
        let baseline = result["baselineRss"].as_u64().unwrap_or(0);
        let post_seed = result["postSeedRss"].as_u64().unwrap_or(0);
        let post_read = result["postReadRss"].as_u64().unwrap_or(0);
        let seed_delta = delta(post_seed, baseline);
        let read_delta = delta(post_read, post_seed);
        let per_record = if record_count == 0 {
            "-".to_owned()
        } else {
            (seed_delta / record_count as i64).to_string()
        };
        let disk = result.get("disk");
        let (main, auxiliary, files, detail) = match disk {
            Some(disk) => {
                let main = disk["mainBytes"].as_u64().unwrap_or(0) as i64;
                let auxiliary = disk["auxiliaryBytes"].as_u64().unwrap_or(0) as i64;
                let files = disk["fileCount"].as_u64().unwrap_or(0);
                let detail = match disk["kind"].as_str() {
                    Some("sqlite") => format!(
                        "pages/free {}/{}",
                        disk["pageCount"].as_i64().unwrap_or(-1),
                        disk["freelistCount"].as_i64().unwrap_or(-1)
                    ),
                    Some("jsonl") => format!(
                        "documents/tasks {}/{}",
                        disk["documentFiles"].as_u64().unwrap_or(0),
                        disk["taskFiles"].as_u64().unwrap_or(0)
                    ),
                    _ => "-".to_owned(),
                };
                (
                    mebibytes(main),
                    mebibytes(auxiliary),
                    files.to_string(),
                    detail,
                )
            }
            None => (
                "-".to_owned(),
                "-".to_owned(),
                "-".to_owned(),
                "-".to_owned(),
            ),
        };
        println!(
            "{:<8} {:<6} {:>8} {:>18} {:>19} {:>18} {:>14} {:>19} {:>15} {:>11}  {}",
            result["backend"].as_str().unwrap_or("?"),
            result["scale"].as_str().unwrap_or("?"),
            record_count,
            mebibytes(seed_delta),
            mebibytes(read_delta),
            per_record,
            "n/a",
            format!("{main}/{auxiliary}"),
            mebibytes(
                disk.map(|disk| {
                    disk["mainBytes"].as_u64().unwrap_or(0) as i64
                        + disk["auxiliaryBytes"].as_u64().unwrap_or(0) as i64
                })
                .unwrap_or(0)
            ),
            files,
            detail
        );
    }
}
