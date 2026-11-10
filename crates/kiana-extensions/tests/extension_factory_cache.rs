//! 工厂缓存回归测试（`extension-factory-cache.test.ts`，4 个用例）。
//!
//! TS 的「模块执行次数 / 工厂执行次数」由扩展模块体里的全局计数器观察；Rust 侧等价
//! （`globalThis.__extensionFactoryCacheTest`）。缓存本身是线程本地状态，测试开始前清空。
//!
//! 与 TS 的差异：TS 断言两次加载得到的 `Extension` 对象不是同一个引用；Rust 的 `Extension`
//! 是值类型，改为断言两次的 `runtime` 不是同一个 `Arc`（每次批量加载都会新建 runtime）。
//! 第三个用例在 TS 里经 `DefaultResourceLoader.reload()` 清缓存；Rust 的 `kiana-resources` 尚未
//! 实现，直接测底层契约 `clear_extension_cache()`。
#![cfg(feature = "runtime")]

#[path = "fixtures/extensions/support.rs"]
mod support;

use std::sync::Arc;

use kiana_extensions::loader::factory_cache::{
    cache_generation, cached_len, clear_extension_cache,
};
use kiana_extensions::loader::{load_extensions, load_extensions_cached};
use support::{LoaderHarness, temp_root, write_file};

/// 计数扩展源：模块体 +1 `moduleLoads`，工厂体 +1 `factoryRuns`。
const COUNTING_EXTENSION: &str = r#"
const state = (globalThis.__extensionFactoryCacheTest ??= {});
state.moduleLoads = (state.moduleLoads ?? 0) + 1;

export default function () {
    state.factoryRuns = (state.factoryRuns ?? 0) + 1;
}
"#;

fn read_counters(harness: &LoaderHarness) -> (u64, u64) {
    let text = harness
        .engine
        .eval_script::<String>(
            "JSON.stringify(globalThis.__extensionFactoryCacheTest)",
            "<counter>",
        )
        .expect("read counters");
    let value: serde_json::Value = serde_json::from_str(&text).expect("valid json");
    (
        value
            .get("moduleLoads")
            .and_then(|value| value.as_u64())
            .unwrap_or(0),
        value
            .get("factoryRuns")
            .and_then(|value| value.as_u64())
            .unwrap_or(0),
    )
}

fn cwd_string(path: &std::path::Path) -> String {
    path.to_string_lossy().into_owned()
}

#[tokio::test(flavor = "current_thread")]
async fn caches_extension_modules_for_cached_same_cwd_loads_but_reruns_factories() {
    let harness = LoaderHarness::new();
    clear_extension_cache();
    let root = temp_root();
    let extension_path = write_file(root.path(), "counting.ts", COUNTING_EXTENSION);
    let cwd = cwd_string(root.path());
    let path = extension_path.to_string_lossy().into_owned();

    let first = load_extensions_cached(std::slice::from_ref(&path), &cwd, None, None).await;
    let second = load_extensions_cached(&[path], &cwd, None, None).await;
    assert!(first.errors.is_empty(), "{:?}", first.errors);
    assert!(second.errors.is_empty(), "{:?}", second.errors);

    assert_eq!(read_counters(&harness), (1, 2));
    assert!(!Arc::ptr_eq(&first.runtime, &second.runtime));
    clear_extension_cache();
}

#[tokio::test(flavor = "current_thread")]
async fn does_not_cache_direct_load_extensions_calls() {
    let harness = LoaderHarness::new();
    clear_extension_cache();
    let root = temp_root();
    let extension_path = write_file(root.path(), "counting.ts", COUNTING_EXTENSION);
    let cwd = cwd_string(root.path());
    let path = extension_path.to_string_lossy().into_owned();

    load_extensions(std::slice::from_ref(&path), &cwd, None, None).await;
    load_extensions(&[path], &cwd, None, None).await;

    assert_eq!(read_counters(&harness), (2, 2));
    assert_eq!(cached_len(), 0);
}

// TS 经 `DefaultResourceLoader.reload()` 间接触发 `clearExtensionCache()`。`kiana-resources`
// 的 `DefaultResourceLoader` 尚未落地，本用例直接驱动它调用的底层契约（`reload()` 就是
// “清缓存 + 重新加载”），断言与 TS 相同：清缓存后模块与工厂都重新执行。
#[tokio::test(flavor = "current_thread")]
async fn clears_the_cache_on_resource_loader_reload() {
    let harness = LoaderHarness::new();
    clear_extension_cache();
    let root = temp_root();
    let extension_path = write_file(root.path(), "counting.ts", COUNTING_EXTENSION);
    let cwd = cwd_string(root.path());
    let path = extension_path.to_string_lossy().into_owned();

    // 两次 `reload()`：`DefaultResourceLoader.reload()` 会调用 `clearExtensionCache()`。
    load_extensions_cached(std::slice::from_ref(&path), &cwd, None, None).await;
    let generation = cache_generation();
    clear_extension_cache();
    assert_eq!(cache_generation(), generation + 1);
    load_extensions_cached(&[path], &cwd, None, None).await;

    assert_eq!(read_counters(&harness), (2, 2));
    clear_extension_cache();
}

#[tokio::test(flavor = "current_thread")]
async fn keeps_the_cache_scoped_to_one_cwd() {
    let harness = LoaderHarness::new();
    clear_extension_cache();
    let root = temp_root();
    let extension_path = write_file(root.path(), "counting.ts", COUNTING_EXTENSION);
    let first_cwd = root.path().join("first");
    let second_cwd = root.path().join("second");
    std::fs::create_dir_all(&first_cwd).expect("first cwd");
    std::fs::create_dir_all(&second_cwd).expect("second cwd");
    let path = extension_path.to_string_lossy().into_owned();

    load_extensions_cached(
        std::slice::from_ref(&path),
        &cwd_string(&first_cwd),
        None,
        None,
    )
    .await;
    load_extensions_cached(
        std::slice::from_ref(&path),
        &cwd_string(&second_cwd),
        None,
        None,
    )
    .await;
    load_extensions_cached(&[path], &cwd_string(&second_cwd), None, None).await;

    assert_eq!(read_counters(&harness), (2, 3));
    clear_extension_cache();
}
