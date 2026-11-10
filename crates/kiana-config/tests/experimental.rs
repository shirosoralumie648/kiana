//! `packages/coding-agent/test/experimental.test.ts` 的移植（5 个用例）。
//!
//! `are_experimental_features_enabled()` 只认字符串 `"1"`。

use std::sync::{Mutex, MutexGuard};

use kiana_config::defaults::are_experimental_features_enabled;

static ENV_LOCK: Mutex<()> = Mutex::new(());

fn serial_env() -> MutexGuard<'static, ()> {
    ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn set_experimental(value: Option<&str>) {
    // SAFETY: 本文件的用例都持有 `serial_env()` 的互斥锁。
    unsafe {
        match value {
            Some(value) => std::env::set_var("KIANA_EXPERIMENTAL", value),
            None => std::env::remove_var("KIANA_EXPERIMENTAL"),
        }
    }
}

fn with_restored_env(run: impl FnOnce()) {
    let _guard = serial_env();
    let original = std::env::var("KIANA_EXPERIMENTAL").ok();
    run();
    set_experimental(original.as_deref());
}

// TS: "returns false when KIANA_EXPERIMENTAL is unset"
#[test]
fn returns_false_when_kiana_experimental_is_unset() {
    with_restored_env(|| {
        set_experimental(None);
        assert!(!are_experimental_features_enabled());
    });
}

// TS: "returns false when KIANA_EXPERIMENTAL is empty"
#[test]
fn returns_false_when_kiana_experimental_is_empty() {
    with_restored_env(|| {
        set_experimental(Some(""));
        assert!(!are_experimental_features_enabled());
    });
}

// TS: "returns true when KIANA_EXPERIMENTAL is set to 1"
#[test]
fn returns_true_when_kiana_experimental_is_set_to_1() {
    with_restored_env(|| {
        set_experimental(Some("1"));
        assert!(are_experimental_features_enabled());
    });
}

// TS: "returns false when KIANA_EXPERIMENTAL is set to 0"
#[test]
fn returns_false_when_kiana_experimental_is_set_to_0() {
    with_restored_env(|| {
        set_experimental(Some("0"));
        assert!(!are_experimental_features_enabled());
    });
}

// TS: "returns false when KIANA_EXPERIMENTAL is set to a non-1 value"
#[test]
fn returns_false_when_kiana_experimental_is_set_to_a_non_1_value() {
    with_restored_env(|| {
        set_experimental(Some("true"));
        assert!(!are_experimental_features_enabled());
    });
}
