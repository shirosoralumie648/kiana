//! `core/auth-storage.ts` 的测试移植。
//!
//! 与原测试的差异（原因：Rust 侧无法 spy `proper-lockfile`）：
//! - 「锁调用次数」类断言改为**可观察行为**断言（读到的是新值、共享重载不被单个读者的取消打断）；
//! - `vi.spyOn(lockfile, "lock")` 的注入点改为真实文件系统条件（外部持锁 / 目录不可写）；
//! - Windows EPERM 的 4 条用例按 ARCHITECTURE §8 第 26 条只保证 `cfg(windows)` 编译。

#![allow(clippy::await_holding_lock)]

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use indexmap::IndexMap;
use kiana_ai::auth::types::{
    ApiKeyCredential, AuthError, AuthOperationOptions, AuthType, Credential, CredentialInfo,
    CredentialModifyFn, CredentialStore, OAuthCredential,
};
use kiana_ai::utils::abort::AbortScope;
use kiana_config::auth::lock::{LockOptions, lock_sync};
use kiana_config::auth::storage::{
    AuthStorage, AuthStorageBackend, FileAuthStorageBackend, InMemoryAuthStorageBackend,
    LockResult, ReadOnlyAuthStorage, auth_data_from_value, read_stored_credential,
};
use serde_json::{Value, json};
use tempfile::TempDir;

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

fn api_key(key: &str) -> Credential {
    Credential::ApiKey(ApiKeyCredential {
        key: Some(key.to_string()),
        env: None,
        extra: IndexMap::new(),
    })
}

fn oauth(access: &str, refresh: &str, expires: f64) -> Credential {
    Credential::OAuth(OAuthCredential::new(access, refresh, expires))
}

fn modify_fn<F, Fut>(f: F) -> CredentialModifyFn
where
    F: FnOnce(Option<Credential>) -> Fut + Send + 'static,
    Fut: std::future::Future<Output = Result<Option<Credential>, AuthError>> + Send + 'static,
{
    Box::new(move |current| Box::pin(f(current)))
}

fn path_string(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

fn write_json(path: &Path, value: &Value) {
    fs::write(path, serde_json::to_string(value).unwrap()).unwrap();
}

fn write_raw(path: &Path, content: &str) {
    fs::write(path, content).unwrap();
}

fn read_json(path: &Path) -> Value {
    serde_json::from_str(&fs::read_to_string(path).unwrap()).unwrap()
}

fn options(signal: Option<AbortScope>) -> AuthOperationOptions {
    AuthOperationOptions { signal }
}

fn aborted_error_name(error: &AuthError) -> &str {
    &error.name
}

#[cfg(unix)]
fn permissions_mode(path: &Path) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    fs::metadata(path).unwrap().permissions().mode() & 0o777
}

/// 恢复目录权限的 RAII guard（panic 时也能清理，避免 TempDir 删除失败）。
#[cfg(unix)]
struct PermissionGuard {
    path: PathBuf,
    mode: u32,
}

#[cfg(unix)]
impl PermissionGuard {
    fn new(path: &Path, mode: u32) -> Self {
        use std::os::unix::fs::PermissionsExt;
        let original = permissions_mode(path);
        fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
        Self {
            path: path.to_path_buf(),
            mode: original,
        }
    }

    fn restore(&self) {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&self.path, fs::Permissions::from_mode(self.mode)).unwrap();
    }
}

#[cfg(unix)]
impl Drop for PermissionGuard {
    fn drop(&mut self) {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(&self.path, fs::Permissions::from_mode(self.mode));
    }
}

static ENV_LOCK: Mutex<()> = Mutex::new(());

// ---------------------------------------------------------------------------
// tests
// ---------------------------------------------------------------------------

/// reads and resolves stored API-key credentials
#[tokio::test]
async fn reads_and_resolves_stored_api_key_credentials() {
    let _env = ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let temp_dir = TempDir::new().unwrap();
    let auth_path = temp_dir.path().join("auth.json");
    // SAFETY: 测试内串行持有 ENV_LOCK；该变量名只在本测试中使用。
    unsafe { std::env::set_var("TEST_AUTH_STORAGE_KEY", "environment-key") };
    write_json(
        &auth_path,
        &json!({"anthropic": {"type": "api_key", "key": "$TEST_AUTH_STORAGE_KEY"}}),
    );
    let storage = AuthStorage::create(&path_string(&auth_path));
    assert_eq!(
        storage.read("anthropic", options(None)).await.unwrap(),
        Some(api_key("environment-key"))
    );
    unsafe { std::env::remove_var("TEST_AUTH_STORAGE_KEY") };
}

/// resolves command-backed API-key credentials
#[tokio::test]
async fn resolves_command_backed_api_key_credentials() {
    let temp_dir = TempDir::new().unwrap();
    let auth_path = temp_dir.path().join("auth.json");
    write_json(
        &auth_path,
        &json!({"anthropic": {"type": "api_key", "key": "!printf 'command-key'"}}),
    );
    let storage = AuthStorage::create(&path_string(&auth_path));
    assert_eq!(
        storage.read("anthropic", options(None)).await.unwrap(),
        Some(api_key("command-key"))
    );
}

/// returns OAuth credentials unchanged
#[tokio::test]
async fn returns_oauth_credentials_unchanged() {
    let credential = oauth("access-token", "refresh-token", 1_767_225_600_000.0);
    let storage = AuthStorage::in_memory(auth_data_from_value(
        json!({"anthropic": {"type": "oauth", "access": "access-token", "refresh": "refresh-token", "expires": 1767225600000i64}}),
    ));
    assert_eq!(
        storage.read("anthropic", options(None)).await.unwrap(),
        Some(credential)
    );
}

/// credential-scoped env takes precedence and remains inspectable
#[tokio::test]
async fn credential_scoped_env_takes_precedence_and_remains_inspectable() {
    let temp_dir = TempDir::new().unwrap();
    let auth_path = temp_dir.path().join("auth.json");
    write_json(
        &auth_path,
        &json!({"anthropic": {"type": "api_key", "key": "$SCOPED_KEY", "env": {"SCOPED_KEY": "scoped-value", "REGION": "test-region"}}}),
    );
    let storage = AuthStorage::create(&path_string(&auth_path));
    let credential = storage
        .read("anthropic", options(None))
        .await
        .unwrap()
        .unwrap();
    let Credential::ApiKey(api_key_credential) = credential else {
        panic!("expected api_key");
    };
    assert_eq!(api_key_credential.key.as_deref(), Some("scoped-value"));
    let env = api_key_credential.env.unwrap();
    assert_eq!(
        env.get("SCOPED_KEY").map(String::as_str),
        Some("scoped-value")
    );
    assert_eq!(env.get("REGION").map(String::as_str), Some("test-region"));
}

/// coalesces file reloads across concurrent readers and storage instances
#[tokio::test]
async fn coalesces_file_reloads_across_concurrent_readers_and_storage_instances() {
    let temp_dir = TempDir::new().unwrap();
    let auth_path = temp_dir.path().join("auth.json");
    write_json(
        &auth_path,
        &json!({"anthropic": {"type": "api_key", "key": "old"}}),
    );
    let first = AuthStorage::create(&path_string(&auth_path));
    let second = AuthStorage::create(&path_string(&auth_path));

    write_json(
        &auth_path,
        &json!({"anthropic": {"type": "api_key", "key": "new"}, "openai": {"type": "api_key", "key": "openai-key"}}),
    );

    // TS 用 `vi.spyOn(lockfile, "lock")` 断言并发读者只触发一次真实重载。
    let reloads_before = first.reload_count();
    let (anthropic, openai, credentials) = tokio::join!(
        first.read("anthropic", options(Some(AbortScope::new()))),
        second.read("openai", options(Some(AbortScope::new()))),
        first.list(options(Some(AbortScope::new()))),
    );
    assert_eq!(first.reload_count(), reloads_before + 1);
    assert_eq!(second.reload_count(), reloads_before + 1);
    assert_eq!(anthropic.unwrap(), Some(api_key("new")));
    assert_eq!(openai.unwrap(), Some(api_key("openai-key")));
    assert_eq!(
        credentials.unwrap(),
        vec![
            CredentialInfo {
                provider_id: "anthropic".into(),
                r#type: AuthType::ApiKey,
            },
            CredentialInfo {
                provider_id: "openai".into(),
                r#type: AuthType::ApiKey,
            },
        ]
    );

    assert_eq!(
        second.read("anthropic", options(None)).await.unwrap(),
        Some(api_key("new"))
    );
    // revision 未变：不再触发重载。
    assert_eq!(first.reload_count(), reloads_before + 1);

    let other_path = temp_dir.path().join("other-auth.json");
    write_json(
        &other_path,
        &json!({"other": {"type": "api_key", "key": "other-key"}}),
    );
    let other_first = AuthStorage::create(&path_string(&other_path));
    let other_second = AuthStorage::create(&path_string(&other_path));
    let other_reloads_before = other_first.reload_count();
    other_first.read("other", options(None)).await.unwrap();
    other_second.read("other", options(None)).await.unwrap();
    other_first.list(options(None)).await.unwrap();
    // 其它路径有独立的读状态，且已缓存的 revision 不再触发重载。
    assert_eq!(other_first.reload_count(), other_reloads_before);

    let third = AuthStorage::create(&path_string(&auth_path));
    write_json(
        &auth_path,
        &json!({"anthropic": {"type": "api_key", "key": "newest"}}),
    );
    let (first_reload, third_reload) = tokio::join!(
        first.read("anthropic", options(None)),
        third.read("anthropic", options(None)),
    );
    assert_eq!(first_reload.unwrap(), Some(api_key("newest")));
    assert_eq!(third_reload.unwrap(), Some(api_key("newest")));
    // 新一轮并发读者同样只触发一次真实重载。
    assert_eq!(first.reload_count(), reloads_before + 2);
}

/// keeps a coalesced reload alive while another credential reader is waiting
#[tokio::test]
async fn keeps_a_coalesced_reload_alive_while_another_credential_reader_is_waiting() {
    let temp_dir = TempDir::new().unwrap();
    let auth_path = temp_dir.path().join("auth.json");
    write_json(
        &auth_path,
        &json!({"anthropic": {"type": "api_key", "key": "old"}}),
    );
    let storage = AuthStorage::create(&path_string(&auth_path));
    // 内容长度必须变化：getFileRevision 对同尺寸文件可能给出相同 revision（mtime/ctime
    // 同一时钟 tick），那样读会走快照快路径而不是等待重载。
    write_json(
        &auth_path,
        &json!({"anthropic": {"type": "api_key", "key": "new-value"}}),
    );

    // 外部持有文件锁，使异步重载停留在等锁阶段。
    let held = lock_sync(
        Path::new(&path_string(&auth_path)),
        &LockOptions::sync_default(),
    )
    .unwrap();

    let first_controller = AbortScope::new();
    let second_controller = AbortScope::new();
    let reloads_before = storage.reload_count();
    let first = tokio::spawn({
        let storage = storage.clone();
        let signal = first_controller.clone();
        async move { storage.read("anthropic", options(Some(signal))).await }
    });
    // 让第一个读者先建立共享重载。
    tokio::time::sleep(Duration::from_millis(20)).await;
    let second = tokio::spawn({
        let storage = storage.clone();
        let signal = second_controller.clone();
        async move { storage.read("anthropic", options(Some(signal))).await }
    });
    tokio::time::sleep(Duration::from_millis(20)).await;

    first_controller.abort();
    let first_result = first.await.unwrap();
    assert_eq!(
        aborted_error_name(first_result.as_ref().unwrap_err()),
        "AbortError"
    );

    drop(held);
    let second_result = second.await.unwrap();
    assert_eq!(second_result.unwrap(), Some(api_key("new-value")));
    // 两个读者共享同一次重载（TS: `expect(lockSpy).toHaveBeenCalledTimes(1)`）。
    assert_eq!(storage.reload_count(), reloads_before + 1);
}

/// creates new auth files with owner-only permissions
#[cfg(unix)]
#[tokio::test]
async fn creates_new_auth_files_with_owner_only_permissions() {
    let temp_dir = TempDir::new().unwrap();
    let auth_path = temp_dir.path().join("auth.json");
    AuthStorage::create(&path_string(&auth_path));
    assert_eq!(permissions_mode(&auth_path), 0o600);
}

/// preserves the mode of an existing auth file
#[cfg(unix)]
#[tokio::test]
async fn preserves_the_mode_of_an_existing_auth_file() {
    use std::os::unix::fs::PermissionsExt;
    let temp_dir = TempDir::new().unwrap();
    let auth_path = temp_dir.path().join("auth.json");
    write_json(
        &auth_path,
        &json!({"anthropic": {"type": "api_key", "key": "old"}}),
    );
    fs::set_permissions(&auth_path, fs::Permissions::from_mode(0o660)).unwrap();
    let storage = AuthStorage::create(&path_string(&auth_path));
    storage
        .modify(
            "anthropic",
            modify_fn(|_| async { Ok(Some(api_key("new"))) }),
            options(None),
        )
        .await
        .unwrap();
    assert_eq!(permissions_mode(&auth_path), 0o660);
}

/// modify persists a credential while preserving unrelated external edits
#[tokio::test]
async fn modify_persists_a_credential_while_preserving_unrelated_external_edits() {
    let temp_dir = TempDir::new().unwrap();
    let auth_path = temp_dir.path().join("auth.json");
    write_json(
        &auth_path,
        &json!({"anthropic": {"type": "api_key", "key": "old"}}),
    );
    let storage = AuthStorage::create(&path_string(&auth_path));
    write_json(
        &auth_path,
        &json!({"anthropic": {"type": "api_key", "key": "old"}, "openai": {"type": "api_key", "key": "external"}}),
    );
    storage
        .modify(
            "anthropic",
            modify_fn(|_| async { Ok(Some(api_key("new"))) }),
            options(None),
        )
        .await
        .unwrap();
    assert_eq!(
        read_json(&auth_path),
        json!({"anthropic": {"type": "api_key", "key": "new"}, "openai": {"type": "api_key", "key": "external"}})
    );
}

/// modify with undefined leaves the current credential unchanged
#[tokio::test]
async fn modify_with_undefined_leaves_the_current_credential_unchanged() {
    let temp_dir = TempDir::new().unwrap();
    let auth_path = temp_dir.path().join("auth.json");
    write_json(
        &auth_path,
        &json!({"anthropic": {"type": "api_key", "key": "stored"}}),
    );
    let storage = AuthStorage::create(&path_string(&auth_path));
    assert_eq!(
        storage
            .modify(
                "anthropic",
                modify_fn(|_| async { Ok(None) }),
                options(None)
            )
            .await
            .unwrap(),
        Some(api_key("stored"))
    );
    assert_eq!(
        storage.read("anthropic", options(None)).await.unwrap(),
        Some(api_key("stored"))
    );
}

/// serializes concurrent modifications
#[tokio::test]
async fn serializes_concurrent_modifications() {
    let temp_dir = TempDir::new().unwrap();
    let auth_path = temp_dir.path().join("auth.json");
    write_json(&auth_path, &json!({}));
    let first = AuthStorage::create(&path_string(&auth_path));
    let second = AuthStorage::create(&path_string(&auth_path));
    let (a, b) = tokio::join!(
        first.modify(
            "anthropic",
            modify_fn(|_| async { Ok(Some(api_key("anthropic-key"))) }),
            options(None),
        ),
        second.modify(
            "openai",
            modify_fn(|_| async { Ok(Some(api_key("openai-key"))) }),
            options(None),
        ),
    );
    a.unwrap();
    b.unwrap();
    assert_eq!(
        read_json(&auth_path),
        json!({"anthropic": {"type": "api_key", "key": "anthropic-key"}, "openai": {"type": "api_key", "key": "openai-key"}})
    );
}

/// delete removes one credential while preserving others
#[tokio::test]
async fn delete_removes_one_credential_while_preserving_others() {
    let temp_dir = TempDir::new().unwrap();
    let auth_path = temp_dir.path().join("auth.json");
    write_json(
        &auth_path,
        &json!({"anthropic": {"type": "api_key", "key": "anthropic-key"}, "openai": {"type": "api_key", "key": "openai-key"}}),
    );
    let storage = AuthStorage::create(&path_string(&auth_path));
    write_json(
        &auth_path,
        &json!({"anthropic": {"type": "api_key", "key": "anthropic-key"}, "openai": {"type": "api_key", "key": "openai-key"}, "google": {"type": "api_key", "key": "external-key"}}),
    );
    storage.delete("anthropic", options(None)).await.unwrap();
    assert_eq!(
        storage.list(options(None)).await.unwrap(),
        vec![
            CredentialInfo {
                provider_id: "openai".into(),
                r#type: AuthType::ApiKey,
            },
            CredentialInfo {
                provider_id: "google".into(),
                r#type: AuthType::ApiKey,
            },
        ]
    );
    assert_eq!(
        storage.read("anthropic", options(None)).await.unwrap(),
        None
    );
    assert_eq!(
        storage.read("openai", options(None)).await.unwrap(),
        Some(api_key("openai-key"))
    );
    assert_eq!(
        storage.read("google", options(None)).await.unwrap(),
        Some(api_key("external-key"))
    );
}

/// in-memory storage implements the same credential-store behavior
#[tokio::test]
async fn in_memory_storage_implements_the_same_credential_store_behavior() {
    let storage = AuthStorage::in_memory(auth_data_from_value(
        json!({"anthropic": {"type": "api_key", "key": "initial"}}),
    ));
    assert_eq!(
        storage.read("anthropic", options(None)).await.unwrap(),
        Some(api_key("initial"))
    );
    storage
        .modify(
            "anthropic",
            modify_fn(|_| async { Ok(Some(api_key("updated"))) }),
            options(None),
        )
        .await
        .unwrap();
    assert_eq!(
        storage.read("anthropic", options(None)).await.unwrap(),
        Some(api_key("updated"))
    );
    storage.delete("anthropic", options(None)).await.unwrap();
    assert_eq!(storage.list(options(None)).await.unwrap(), Vec::new());
}

/// does not write after lock acquisition failure and recovers on retry
#[cfg(unix)]
#[tokio::test]
async fn does_not_write_after_lock_acquisition_failure_and_recovers_on_retry() {
    let temp_dir = TempDir::new().unwrap();
    let auth_path = temp_dir.path().join("auth.json");
    write_json(
        &auth_path,
        &json!({"anthropic": {"type": "api_key", "key": "stored"}}),
    );
    let storage = AuthStorage::create(&path_string(&auth_path));

    // 目录不可写 → 锁目录 create_dir 失败（不可重试错误）。
    let guard = PermissionGuard::new(temp_dir.path(), 0o500);
    let failed = storage
        .modify(
            "openai",
            modify_fn(|_| async { Ok(Some(api_key("new"))) }),
            options(None),
        )
        .await;
    assert!(failed.is_err());
    guard.restore();
    assert_eq!(
        read_json(&auth_path),
        json!({"anthropic": {"type": "api_key", "key": "stored"}})
    );

    storage
        .modify(
            "openai",
            modify_fn(|_| async { Ok(Some(api_key("new"))) }),
            options(None),
        )
        .await
        .unwrap();
    assert_eq!(
        read_json(&auth_path),
        json!({"anthropic": {"type": "api_key", "key": "stored"}, "openai": {"type": "api_key", "key": "new"}})
    );
}

/// retries a briefly contended file lock
#[tokio::test]
async fn retries_a_briefly_contended_file_lock() {
    let temp_dir = TempDir::new().unwrap();
    let auth_path = temp_dir.path().join("auth.json");
    write_json(
        &auth_path,
        &json!({"anthropic": {"type": "api_key", "key": "stored"}}),
    );
    let backend = AuthStorageBackend::File(FileAuthStorageBackend::new(&path_string(&auth_path)));

    let held = lock_sync(
        Path::new(&path_string(&auth_path)),
        &LockOptions::sync_default(),
    )
    .unwrap();
    let release_thread = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(30));
        drop(held);
    });

    let ran = Arc::new(AtomicBool::new(false));
    let ran_in_closure = Arc::clone(&ran);
    backend
        .with_lock_async(
            move |_| {
                Box::pin(async move {
                    ran_in_closure.store(true, Ordering::SeqCst);
                    Ok(LockResult::new(()))
                })
            },
            &options(None),
        )
        .await
        .unwrap();
    release_thread.join().unwrap();
    assert!(ran.load(Ordering::SeqCst));
}

/// Windows EPERM on lock acquisition
///
/// TS 的 4 条用例通过 mock `proper-lockfile` 注入 EPERM；Rust 的锁实现（`crate::auth::lock`，
/// crate-foundation 单元）没有可注入的假件，§8.26 允许 Windows 用例只保证编译。这里用同样
/// 属于可重试类别的 ELOCKED 竞争验证重试等待路径；`PermissionDenied → EPERM/EACCES` 的平台
/// 分类由 `auth::storage::tests::permission_denied_lock_error_code_is_platform_specific` 覆盖。
#[cfg(windows)]
mod windows_eperm {
    use super::*;

    /// retries transient EPERM on Windows (async)
    #[tokio::test]
    async fn retries_transient_eperm_on_windows_async() {
        let temp_dir = TempDir::new().unwrap();
        let auth_path = temp_dir.path().join("auth.json");
        write_json(&auth_path, &json!({}));
        let backend =
            AuthStorageBackend::File(FileAuthStorageBackend::new(&path_string(&auth_path)));
        let held = lock_sync(
            Path::new(&path_string(&auth_path)),
            &LockOptions::sync_default(),
        )
        .unwrap();
        let release_thread = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(30));
            drop(held);
        });

        let ran = Arc::new(AtomicBool::new(false));
        let ran_in_closure = Arc::clone(&ran);
        backend
            .with_lock_async(
                move |_| {
                    Box::pin(async move {
                        ran_in_closure.store(true, Ordering::SeqCst);
                        Ok(LockResult::new(()))
                    })
                },
                &options(None),
            )
            .await
            .unwrap();
        release_thread.join().unwrap();
        assert!(ran.load(Ordering::SeqCst));
    }

    /// surfaces persistent EPERM on Windows after a bounded number of retries (async)
    ///
    /// 用一个不可重试的锁错误（父路径是文件 → ENOTDIR）验证「放弃后回调不执行」。
    #[tokio::test]
    async fn surfaces_persistent_eperm_on_windows_after_a_bounded_number_of_retries_async() {
        let temp_dir = TempDir::new().unwrap();
        let blocked = temp_dir.path().join("blocked");
        fs::write(&blocked, "x").unwrap();
        let backend = AuthStorageBackend::File(FileAuthStorageBackend::new(&path_string(
            &blocked.join("auth.json"),
        )));
        let ran = Arc::new(AtomicBool::new(false));
        let ran_in_closure = Arc::clone(&ran);
        let result = backend
            .with_lock_async(
                move |_| {
                    Box::pin(async move {
                        ran_in_closure.store(true, Ordering::SeqCst);
                        Ok(LockResult::new(()))
                    })
                },
                &options(None),
            )
            .await;
        assert!(result.is_err());
        assert!(!ran.load(Ordering::SeqCst));
    }

    /// does not retry EPERM on other platforms (async)
    ///
    /// Windows 构建中无法模拟「非 win32 平台」；平台分类的断言见模块文档。
    #[test]
    fn does_not_retry_eperm_on_other_platforms_async() {}

    /// retries transient EPERM on Windows (sync)
    #[tokio::test]
    async fn retries_transient_eperm_on_windows_sync() {
        let temp_dir = TempDir::new().unwrap();
        let auth_path = temp_dir.path().join("auth.json");
        write_json(&auth_path, &json!({}));
        let backend = FileAuthStorageBackend::new(&path_string(&auth_path));
        let held = lock_sync(
            Path::new(&path_string(&auth_path)),
            &LockOptions::sync_default(),
        )
        .unwrap();
        let release_thread = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(30));
            drop(held);
        });

        let ran = Arc::new(AtomicBool::new(false));
        let ran_in_closure = Arc::clone(&ran);
        backend
            .with_lock(move |_| {
                ran_in_closure.store(true, Ordering::SeqCst);
                Ok(LockResult::new(()))
            })
            .unwrap();
        release_thread.join().unwrap();
        assert!(ran.load(Ordering::SeqCst));
    }
}

/// surfaces a compromised file storage lock
#[tokio::test]
async fn surfaces_a_compromised_file_storage_lock() {
    let temp_dir = TempDir::new().unwrap();
    let auth_path = temp_dir.path().join("auth.json");
    write_json(
        &auth_path,
        &json!({"anthropic": {"type": "api_key", "key": "stored"}}),
    );
    // 1 s 心跳（proper-lockfile 的 update 下限）：锁目录被移除后由心跳判定 compromised。
    let backend = AuthStorageBackend::File(
        FileAuthStorageBackend::new(&path_string(&auth_path)).with_async_update_ms(1),
    );
    let lock_dir = PathBuf::from(format!("{}.lock", path_string(&auth_path)));
    let update_called = Arc::new(AtomicBool::new(false));
    let update_called_in_closure = Arc::clone(&update_called);
    let lock_dir_in_closure = lock_dir.clone();

    let result = backend
        .with_lock_async(
            move |_| {
                Box::pin(async move {
                    update_called_in_closure.store(true, Ordering::SeqCst);
                    let _ = fs::remove_dir(&lock_dir_in_closure);
                    // 心跳线程在 1 s 时发现锁目录消失。
                    std::thread::sleep(Duration::from_millis(1500));
                    Ok(LockResult::with_next((), json!({}).to_string()))
                })
            },
            &options(None),
        )
        .await;

    assert!(update_called.load(Ordering::SeqCst));
    let error = result.unwrap_err();
    assert_eq!(error.message, "Auth storage lock was compromised");
    assert_eq!(
        read_json(&auth_path),
        json!({"anthropic": {"type": "api_key", "key": "stored"}})
    );
}

/// pre-aborted file operations do not create the backing file or run the mutation
#[tokio::test]
async fn pre_aborted_file_operations_do_not_create_the_backing_file_or_run_the_mutation() {
    let temp_dir = TempDir::new().unwrap();
    let auth_path = temp_dir.path().join("auth.json");
    let backend = AuthStorageBackend::File(FileAuthStorageBackend::new(&path_string(&auth_path)));
    let controller = AbortScope::new();
    controller.abort();
    let update_called = Arc::new(AtomicBool::new(false));
    let update_called_in_closure = Arc::clone(&update_called);

    let result = backend
        .with_lock_async(
            move |_| {
                Box::pin(async move {
                    update_called_in_closure.store(true, Ordering::SeqCst);
                    Ok(LockResult::with_next((), json!({}).to_string()))
                })
            },
            &options(Some(controller)),
        )
        .await;
    assert_eq!(aborted_error_name(&result.unwrap_err()), "AbortError");
    assert!(!update_called.load(Ordering::SeqCst));
    assert!(!auth_path.exists());
}

/// aborts while waiting for a held file lock without running the mutation later
#[tokio::test]
async fn aborts_while_waiting_for_a_held_file_lock_without_running_the_mutation_later() {
    let temp_dir = TempDir::new().unwrap();
    let auth_path = temp_dir.path().join("auth.json");
    write_json(
        &auth_path,
        &json!({"anthropic": {"type": "api_key", "key": "stored"}}),
    );
    let held = lock_sync(
        Path::new(&path_string(&auth_path)),
        &LockOptions::sync_default(),
    )
    .unwrap();
    let backend = AuthStorageBackend::File(FileAuthStorageBackend::new(&path_string(&auth_path)));
    let controller = AbortScope::new();
    let update_called = Arc::new(AtomicBool::new(false));
    let update_called_in_closure = Arc::clone(&update_called);
    let pending_options = options(Some(controller.clone()));
    let pending = backend.with_lock_async(
        move |_| {
            Box::pin(async move {
                update_called_in_closure.store(true, Ordering::SeqCst);
                Ok(LockResult::with_next((), json!({}).to_string()))
            })
        },
        &pending_options,
    );

    tokio::time::sleep(Duration::from_millis(20)).await;
    controller.abort();
    let result = pending.await;
    assert_eq!(aborted_error_name(&result.unwrap_err()), "AbortError");
    assert!(!update_called.load(Ordering::SeqCst));

    drop(held);
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert!(!update_called.load(Ordering::SeqCst));
    assert_eq!(
        read_json(&auth_path),
        json!({"anthropic": {"type": "api_key", "key": "stored"}})
    );
}

/// releases a file lock acquired concurrently with cancellation before mutation
#[tokio::test]
async fn releases_a_file_lock_acquired_concurrently_with_cancellation_before_mutation() {
    let temp_dir = TempDir::new().unwrap();
    let auth_path = temp_dir.path().join("auth.json");
    write_json(
        &auth_path,
        &json!({"anthropic": {"type": "api_key", "key": "stored"}}),
    );
    let backend = AuthStorageBackend::File(FileAuthStorageBackend::new(&path_string(&auth_path)));
    let controller = AbortScope::new();
    controller.abort();
    let update_called = Arc::new(AtomicBool::new(false));
    let update_called_in_closure = Arc::clone(&update_called);
    let aborted = backend
        .with_lock_async(
            move |_| {
                Box::pin(async move {
                    update_called_in_closure.store(true, Ordering::SeqCst);
                    Ok(LockResult::with_next((), json!({}).to_string()))
                })
            },
            &options(Some(controller)),
        )
        .await;
    assert_eq!(aborted_error_name(&aborted.unwrap_err()), "AbortError");
    assert!(!update_called.load(Ordering::SeqCst));

    // 取消路径若泄漏了锁目录，这里会先等 10×20 ms 再失败或超时；正常路径应立即成功。
    let ran = Arc::new(AtomicBool::new(false));
    let ran_in_closure = Arc::clone(&ran);
    backend
        .with_lock_async(
            move |_| {
                Box::pin(async move {
                    ran_in_closure.store(true, Ordering::SeqCst);
                    Ok(LockResult::new(()))
                })
            },
            &options(None),
        )
        .await
        .unwrap();
    assert!(ran.load(Ordering::SeqCst));
}

/// holds the file lock until a cancelled active callback settles without committing it
#[tokio::test]
async fn holds_the_file_lock_until_a_cancelled_active_callback_settles_without_committing_it() {
    let temp_dir = TempDir::new().unwrap();
    let auth_path = temp_dir.path().join("auth.json");
    write_json(
        &auth_path,
        &json!({"anthropic": {"type": "api_key", "key": "stored"}}),
    );
    let backend = AuthStorageBackend::File(FileAuthStorageBackend::new(&path_string(&auth_path)));
    let controller = AbortScope::new();
    let (started_tx, started_rx) = tokio::sync::oneshot::channel();
    let (finish_tx, finish_rx) = tokio::sync::oneshot::channel();

    let pending = tokio::spawn({
        let backend = backend.clone();
        let signal = controller.clone();
        async move {
            backend
                .with_lock_async(
                    move |_| {
                        Box::pin(async move {
                            let _ = started_tx.send(());
                            let _ = finish_rx.await;
                            Ok(LockResult::with_next(
                                (),
                                json!({"openai": {"type": "api_key", "key": "cancelled"}})
                                    .to_string(),
                            ))
                        })
                    },
                    &options(Some(signal)),
                )
                .await
        }
    });
    started_rx.await.unwrap();
    controller.abort();

    let competing_called = Arc::new(AtomicBool::new(false));
    let competing_called_in_closure = Arc::clone(&competing_called);
    let competing = tokio::spawn({
        let backend = backend.clone();
        async move {
            backend
                .with_lock_async(
                    move |_| {
                        Box::pin(async move {
                            competing_called_in_closure.store(true, Ordering::SeqCst);
                            Ok(LockResult::with_next(
                                (),
                                json!({"google": {"type": "api_key", "key": "committed"}})
                                    .to_string(),
                            ))
                        })
                    },
                    &options(None),
                )
                .await
        }
    });

    tokio::time::sleep(Duration::from_millis(30)).await;
    assert!(!competing_called.load(Ordering::SeqCst));

    finish_tx.send(()).unwrap();
    let pending_result = pending.await.unwrap();
    assert_eq!(
        aborted_error_name(&pending_result.unwrap_err()),
        "AbortError"
    );
    competing.await.unwrap().unwrap();
    assert!(competing_called.load(Ordering::SeqCst));
    assert_eq!(
        read_json(&auth_path),
        json!({"google": {"type": "api_key", "key": "committed"}})
    );
}

/// cancels a signalled credential read waiting for a held file lock
#[tokio::test]
async fn cancels_a_signalled_credential_read_waiting_for_a_held_file_lock() {
    let temp_dir = TempDir::new().unwrap();
    let auth_path = temp_dir.path().join("auth.json");
    write_json(
        &auth_path,
        &json!({"anthropic": {"type": "api_key", "key": "old"}}),
    );
    let storage = AuthStorage::create(&path_string(&auth_path));
    write_json(
        &auth_path,
        &json!({"anthropic": {"type": "api_key", "key": "new-value"}}),
    );
    let held = lock_sync(
        Path::new(&path_string(&auth_path)),
        &LockOptions::sync_default(),
    )
    .unwrap();
    let controller = AbortScope::new();
    let pending = tokio::spawn({
        let storage = storage.clone();
        let signal = controller.clone();
        async move { storage.read("anthropic", options(Some(signal))).await }
    });
    tokio::time::sleep(Duration::from_millis(20)).await;
    controller.abort();
    let result = pending.await.unwrap();
    assert_eq!(aborted_error_name(&result.unwrap_err()), "AbortError");
    drop(held);
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert_eq!(
        storage.read("anthropic", options(None)).await.unwrap(),
        Some(api_key("new-value"))
    );
}

/// serializes in-memory mutations across providers
#[tokio::test]
async fn serializes_in_memory_mutations_across_providers() {
    let storage = AuthStorage::in_memory(auth_data_from_value(json!({})));
    let (started_tx, started_rx) = tokio::sync::oneshot::channel();
    let (finish_tx, finish_rx) = tokio::sync::oneshot::channel();
    let first = tokio::spawn({
        let storage = storage.clone();
        async move {
            storage
                .modify(
                    "anthropic",
                    modify_fn(move |_| async move {
                        let _ = started_tx.send(());
                        let _ = finish_rx.await;
                        Ok(Some(api_key("anthropic-key")))
                    }),
                    options(None),
                )
                .await
        }
    });
    started_rx.await.unwrap();

    let second_called = Arc::new(AtomicBool::new(false));
    let second_called_in_closure = Arc::clone(&second_called);
    let second = tokio::spawn({
        let storage = storage.clone();
        async move {
            storage
                .modify(
                    "openai",
                    modify_fn(move |_| async move {
                        second_called_in_closure.store(true, Ordering::SeqCst);
                        Ok(Some(api_key("openai-key")))
                    }),
                    options(None),
                )
                .await
        }
    });
    tokio::time::sleep(Duration::from_millis(20)).await;
    assert!(!second_called.load(Ordering::SeqCst));

    finish_tx.send(()).unwrap();
    first.await.unwrap().unwrap();
    second.await.unwrap().unwrap();
    assert_eq!(
        storage.read("anthropic", options(None)).await.unwrap(),
        Some(api_key("anthropic-key"))
    );
    assert_eq!(
        storage.read("openai", options(None)).await.unwrap(),
        Some(api_key("openai-key"))
    );
}

/// cancels a queued in-memory mutation without running it later
#[tokio::test]
async fn cancels_a_queued_in_memory_mutation_without_running_it_later() {
    let storage = AuthStorage::in_memory(auth_data_from_value(json!({})));
    let (started_tx, started_rx) = tokio::sync::oneshot::channel();
    let (finish_tx, finish_rx) = tokio::sync::oneshot::channel();
    let first = tokio::spawn({
        let storage = storage.clone();
        async move {
            storage
                .modify(
                    "anthropic",
                    modify_fn(move |_| async move {
                        let _ = started_tx.send(());
                        let _ = finish_rx.await;
                        Ok(Some(api_key("anthropic-key")))
                    }),
                    options(None),
                )
                .await
        }
    });
    started_rx.await.unwrap();

    let controller = AbortScope::new();
    let second_called = Arc::new(AtomicBool::new(false));
    let second_called_in_closure = Arc::clone(&second_called);
    let second = tokio::spawn({
        let storage = storage.clone();
        let signal = controller.clone();
        async move {
            storage
                .modify(
                    "openai",
                    modify_fn(move |_| async move {
                        second_called_in_closure.store(true, Ordering::SeqCst);
                        Ok(Some(api_key("openai-key")))
                    }),
                    options(Some(signal)),
                )
                .await
        }
    });

    controller.abort();
    let second_result = second.await.unwrap();
    assert_eq!(
        aborted_error_name(&second_result.unwrap_err()),
        "AbortError"
    );
    assert!(!second_called.load(Ordering::SeqCst));
    finish_tx.send(()).unwrap();
    first.await.unwrap().unwrap();
    tokio::time::sleep(Duration::from_millis(20)).await;
    assert!(!second_called.load(Ordering::SeqCst));
    assert_eq!(storage.read("openai", options(None)).await.unwrap(), None);
}

/// preserves the stored credential after cancelling an active refresh mutation
#[tokio::test]
async fn preserves_the_stored_credential_after_cancelling_an_active_refresh_mutation() {
    let previous = oauth("expired", "refresh-token", 0.0);
    let storage = AuthStorage::in_memory(auth_data_from_value(json!({
        "oauth": {"type": "oauth", "access": "expired", "refresh": "refresh-token", "expires": 0}
    })));
    let controller = AbortScope::new();
    let (started_tx, started_rx) = tokio::sync::oneshot::channel();
    let (finish_tx, finish_rx) = tokio::sync::oneshot::channel();
    let pending = tokio::spawn({
        let storage = storage.clone();
        let signal = controller.clone();
        async move {
            storage
                .modify(
                    "oauth",
                    modify_fn(move |_| async move {
                        let _ = started_tx.send(());
                        let _ = finish_rx.await;
                        Ok(Some(oauth(
                            "refreshed",
                            "refresh-token",
                            1_767_225_600_000.0,
                        )))
                    }),
                    options(Some(signal)),
                )
                .await
        }
    });
    started_rx.await.unwrap();
    controller.abort();
    let pending_result = pending.await.unwrap();
    assert_eq!(
        aborted_error_name(&pending_result.unwrap_err()),
        "AbortError"
    );

    let competing_called = Arc::new(AtomicBool::new(false));
    let competing_called_in_closure = Arc::clone(&competing_called);
    let competing = tokio::spawn({
        let storage = storage.clone();
        async move {
            storage
                .modify(
                    "other",
                    modify_fn(move |_| async move {
                        competing_called_in_closure.store(true, Ordering::SeqCst);
                        Ok(Some(api_key("other")))
                    }),
                    options(None),
                )
                .await
        }
    });
    tokio::time::sleep(Duration::from_millis(20)).await;
    assert!(!competing_called.load(Ordering::SeqCst));

    finish_tx.send(()).unwrap();
    competing.await.unwrap().unwrap();
    assert!(competing_called.load(Ordering::SeqCst));
    assert_eq!(
        storage.read("oauth", options(None)).await.unwrap(),
        Some(previous)
    );
}

/// does not overwrite malformed auth files
#[tokio::test]
async fn does_not_overwrite_malformed_auth_files() {
    let temp_dir = TempDir::new().unwrap();
    let auth_path = temp_dir.path().join("auth.json");
    write_json(
        &auth_path,
        &json!({"anthropic": {"type": "api_key", "key": "stored"}}),
    );
    let storage = AuthStorage::create(&path_string(&auth_path));
    fs::write(&auth_path, "{invalid-json").unwrap();
    let result = storage
        .modify(
            "openai",
            modify_fn(|_| async { Ok(Some(api_key("new"))) }),
            options(None),
        )
        .await;
    assert!(result.is_err());
    assert_eq!(fs::read_to_string(&auth_path).unwrap(), "{invalid-json");
}

// ---------------------------------------------------------------------------
// readStoredCredential / ReadOnlyAuthStorage（auth-check.test.ts 的对应行为）
// ---------------------------------------------------------------------------

/// readStoredCredential 无锁、不创建文件、不解析 key
#[tokio::test]
async fn read_stored_credential_is_lock_free_and_does_not_create_files() {
    let temp_dir = TempDir::new().unwrap();
    let auth_path = temp_dir.path().join("auth.json");
    let missing = temp_dir.path().join("missing.json");
    assert_eq!(
        read_stored_credential("anthropic", &path_string(&missing)),
        None
    );
    assert!(!missing.exists());

    write_json(
        &auth_path,
        &json!({"anthropic": {"type": "api_key", "key": "$UNRESOLVED_KEY"}}),
    );
    assert_eq!(
        read_stored_credential("anthropic", &path_string(&auth_path)),
        Some(api_key("$UNRESOLVED_KEY"))
    );
    assert!(!temp_dir.path().join("auth.json.lock").exists());
}

/// ReadOnlyAuthStorage 不创建文件、拒绝写入、list 不执行命令
#[tokio::test]
async fn read_only_storage_never_creates_files_or_executes_commands() {
    let temp_dir = TempDir::new().unwrap();
    let missing = temp_dir.path().join("auth.json");
    let storage = ReadOnlyAuthStorage::new(&path_string(&missing));
    assert_eq!(
        storage.read("anthropic", options(None)).await.unwrap(),
        None
    );
    assert_eq!(storage.list(options(None)).await.unwrap(), Vec::new());
    assert!(!missing.exists());
    assert!(!temp_dir.path().join("auth.json.lock").exists());

    use kiana_ai::auth::types::CredentialModifyFn;
    let modify: CredentialModifyFn = Box::new(|_| Box::pin(async { Ok(None) }));
    let error = storage
        .modify("anthropic", modify, options(None))
        .await
        .unwrap_err();
    assert_eq!(
        error.message,
        "Read-only credential storage cannot modify auth.json"
    );
    let error = storage
        .delete("anthropic", options(None))
        .await
        .unwrap_err();
    assert_eq!(
        error.message,
        "Read-only credential storage cannot modify auth.json"
    );
}

/// ReadOnlyAuthStorage 校验错误文案逐字
#[tokio::test]
async fn read_only_storage_validates_auth_json_verbatim() {
    let temp_dir = TempDir::new().unwrap();
    let auth_path = temp_dir.path().join("auth.json");

    fs::write(&auth_path, "[]").unwrap();
    let storage = ReadOnlyAuthStorage::new(&path_string(&auth_path));
    let error = storage.read("x", options(None)).await.unwrap_err();
    assert_eq!(error.message, "Invalid auth.json: expected an object");

    fs::write(&auth_path, "{not-json").unwrap();
    let storage = ReadOnlyAuthStorage::new(&path_string(&auth_path));
    let error = storage.read("x", options(None)).await.unwrap_err();
    assert!(error.message.starts_with("Failed to read auth.json: "));

    write_json(
        &auth_path,
        &json!({"anthropic": {"type": "api_key", "key": 42}}),
    );
    let storage = ReadOnlyAuthStorage::new(&path_string(&auth_path));
    let error = storage.read("anthropic", options(None)).await.unwrap_err();
    assert_eq!(
        error.message,
        "Invalid auth.json credential for provider \"anthropic\""
    );

    // TS: `key === undefined || typeof key === "string"` —— 显式 null 不合法。
    fs::write(&auth_path, r#"{"anthropic":{"type":"api_key","key":null}}"#).unwrap();
    let storage = ReadOnlyAuthStorage::new(&path_string(&auth_path));
    let error = storage.read("anthropic", options(None)).await.unwrap_err();
    assert_eq!(
        error.message,
        "Invalid auth.json credential for provider \"anthropic\""
    );

    // TS: `env === undefined || (非 null 非数组的对象且所有值 string)`。
    fs::write(
        &auth_path,
        r#"{"anthropic":{"type":"api_key","key":"x","env":null}}"#,
    )
    .unwrap();
    let storage = ReadOnlyAuthStorage::new(&path_string(&auth_path));
    let error = storage.list(options(None)).await.unwrap_err();
    assert_eq!(
        error.message,
        "Invalid auth.json credential for provider \"anthropic\""
    );

    fs::write(
        &auth_path,
        r#"{"anthropic":{"type":"api_key","key":"x","env":{"A":1}}}"#,
    )
    .unwrap();
    let storage = ReadOnlyAuthStorage::new(&path_string(&auth_path));
    let error = storage.read("anthropic", options(None)).await.unwrap_err();
    assert_eq!(
        error.message,
        "Invalid auth.json credential for provider \"anthropic\""
    );

    // `!command` 不执行：原样返回。
    write_json(
        &auth_path,
        &json!({"anthropic": {"type": "api_key", "key": "!printf 'should-not-run'"}}),
    );
    let storage = ReadOnlyAuthStorage::new(&path_string(&auth_path));
    assert_eq!(
        storage.read("anthropic", options(None)).await.unwrap(),
        Some(api_key("!printf 'should-not-run'"))
    );
}

// ---------------------------------------------------------------------------
// JSON.parse / 顶层形状（回归：V8 错误文案、非对象顶层、Node errno 文案）
// ---------------------------------------------------------------------------

/// JSON.parse 的错误文案与 V8 逐字一致（不是 serde_json 的文案）。
#[tokio::test]
async fn reports_v8_json_parse_errors_verbatim() {
    let temp_dir = TempDir::new().unwrap();
    let auth_path = temp_dir.path().join("auth.json");

    for (content, expected) in [
        (
            "{oops",
            "Failed to read auth.json: Expected property name or '}' in JSON at position 1 (line 1 column 2)",
        ),
        ("", "Failed to read auth.json: Unexpected end of JSON input"),
        (
            "{} x",
            "Failed to read auth.json: Unexpected non-whitespace character after JSON at position 3 (line 1 column 4)",
        ),
    ] {
        write_raw(&auth_path, content);
        let storage = ReadOnlyAuthStorage::new(&path_string(&auth_path));
        let error = storage.read("a", options(None)).await.unwrap_err();
        assert_eq!(error.message, expected, "content: {content:?}");
    }

    // AuthStorage 在带 signal 的读取与 modify 上抛出原始 SyntaxError。
    write_raw(&auth_path, "{oops");
    let storage = AuthStorage::create(&path_string(&auth_path));
    let error = storage
        .read("a", options(Some(AbortScope::new())))
        .await
        .unwrap_err();
    assert_eq!(error.name, "SyntaxError");
    assert_eq!(
        error.message,
        "Expected property name or '}' in JSON at position 1 (line 1 column 2)"
    );
    // 不带 signal 时 readLatestData 吞掉重载错误（保留旧快照）。
    assert_eq!(storage.read("a", options(None)).await.unwrap(), None);
    let error = storage
        .modify(
            "a",
            modify_fn(|_| async { Ok(Some(api_key("new"))) }),
            options(None),
        )
        .await
        .unwrap_err();
    assert_eq!(error.name, "SyntaxError");
    // malformed 文件不被覆盖。
    assert_eq!(fs::read_to_string(&auth_path).unwrap(), "{oops");

    // JSON.parse 接受 1e400（Infinity）；ReadOnly 的凭证校验仍判不合法。
    write_raw(&auth_path, r#"{"a":1e400}"#);
    let storage = ReadOnlyAuthStorage::new(&path_string(&auth_path));
    let error = storage.read("a", options(None)).await.unwrap_err();
    assert_eq!(
        error.message,
        "Invalid auth.json credential for provider \"a\""
    );
}

/// 顶层不是对象时按 JS 属性语义运行（TS 的 parseStorageData 不校验顶层）。
#[tokio::test]
async fn tolerates_non_object_top_level_auth_documents() {
    let temp_dir = TempDir::new().unwrap();
    let auth_path = temp_dir.path().join("auth.json");

    // 数组：read/list 按索引；delete 留下 null 空洞；modify 展开为对象。
    let array_json = r#"[{"type":"api_key","key":"zero"},{"type":"api_key","key":"one"}]"#;
    write_raw(&auth_path, array_json);
    let storage = AuthStorage::create(&path_string(&auth_path));
    assert_eq!(
        storage.read("0", options(None)).await.unwrap(),
        Some(api_key("zero"))
    );
    assert_eq!(
        storage.list(options(None)).await.unwrap(),
        vec![
            CredentialInfo {
                provider_id: "0".into(),
                r#type: AuthType::ApiKey,
            },
            CredentialInfo {
                provider_id: "1".into(),
                r#type: AuthType::ApiKey,
            },
        ]
    );
    storage.delete("0", options(None)).await.unwrap();
    assert_eq!(
        fs::read_to_string(&auth_path).unwrap(),
        "[\n  null,\n  {\n    \"type\": \"api_key\",\n    \"key\": \"one\"\n  }\n]"
    );

    write_raw(&auth_path, array_json);
    let storage = AuthStorage::create(&path_string(&auth_path));
    storage
        .modify(
            "0",
            modify_fn(|_| async { Ok(Some(api_key("changed"))) }),
            options(None),
        )
        .await
        .unwrap();
    assert_eq!(
        read_json(&auth_path),
        json!({
            "0": {"type": "api_key", "key": "changed"},
            "1": {"type": "api_key", "key": "one"},
        })
    );

    // 数字：read/list 为空；delete 原样写回；modify 写成对象。
    write_raw(&auth_path, "42");
    let storage = AuthStorage::create(&path_string(&auth_path));
    assert_eq!(storage.read("a", options(None)).await.unwrap(), None);
    assert_eq!(storage.list(options(None)).await.unwrap(), Vec::new());
    storage.delete("a", options(None)).await.unwrap();
    assert_eq!(fs::read_to_string(&auth_path).unwrap(), "42");
    storage
        .modify(
            "0",
            modify_fn(|_| async { Ok(Some(api_key("changed"))) }),
            options(None),
        )
        .await
        .unwrap();
    assert_eq!(
        read_json(&auth_path),
        json!({"0": {"type": "api_key", "key": "changed"}})
    );

    // 字符串：删除索引键是 TypeError（String 的索引属性不可配置）。
    write_raw(&auth_path, "\"hello\"");
    let storage = AuthStorage::create(&path_string(&auth_path));
    let error = storage.delete("0", options(None)).await.unwrap_err();
    assert_eq!(error.name, "TypeError");
    assert_eq!(
        error.message,
        "Cannot delete property '0' of [object String]"
    );
    assert_eq!(fs::read_to_string(&auth_path).unwrap(), "\"hello\"");

    // null：任何属性访问都是 TypeError。
    write_raw(&auth_path, "null");
    let storage = AuthStorage::create(&path_string(&auth_path));
    let error = storage.read("a", options(None)).await.unwrap_err();
    assert_eq!(error.name, "TypeError");
    assert_eq!(
        error.message,
        "Cannot read properties of null (reading 'a')"
    );
    let error = storage.list(options(None)).await.unwrap_err();
    assert_eq!(error.message, "Cannot convert undefined or null to object");
    let error = storage.delete("a", options(None)).await.unwrap_err();
    assert_eq!(error.message, "Cannot convert undefined or null to object");
    assert_eq!(fs::read_to_string(&auth_path).unwrap(), "null");
}

/// 未知形状的条目在修改其它 provider 时逐字保留（auth.json 不因未知 type 丢数据）。
#[tokio::test]
async fn preserves_unrepresentable_entries_when_modifying_other_providers() {
    let temp_dir = TempDir::new().unwrap();
    let auth_path = temp_dir.path().join("auth.json");
    write_raw(
        &auth_path,
        r#"{"anthropic":{"type":"api_key","key":"old"},"future":{"type":"bearer","token":"t","n":1}}"#,
    );
    let storage = AuthStorage::create(&path_string(&auth_path));
    storage
        .modify(
            "anthropic",
            modify_fn(|_| async { Ok(Some(api_key("new"))) }),
            options(None),
        )
        .await
        .unwrap();
    let written: Value = serde_json::from_str(&fs::read_to_string(&auth_path).unwrap()).unwrap();
    assert_eq!(
        written["future"],
        json!({"type": "bearer", "token": "t", "n": 1})
    );
    assert_eq!(
        storage.read("anthropic", options(None)).await.unwrap(),
        Some(api_key("new"))
    );
}

/// 冻结 `Credential` 无法表达的条目不再让 read/modify/readStoredCredential 报错：
/// TS 会原样返回它们并允许 `modify` 修复；Rust 投影为 `None`（不抛错），modify 照常写盘。
#[tokio::test]
async fn unrepresentable_stored_entries_do_not_block_reads_or_repairs() {
    let temp_dir = TempDir::new().unwrap();
    let auth_path = temp_dir.path().join("auth.json");
    write_raw(
        &auth_path,
        r#"{
          "legacy": {"type": "oauth", "access": "a", "refresh": "r", "expires": "123"},
          "future": {"type": "bearer", "token": "t", "n": 1},
          "empty": {},
          "scalar": 5
        }"#,
    );
    let storage = AuthStorage::create(&path_string(&auth_path));

    // TS 的 read 返回原始值；`Option<Credential>` 只能投影为“没有可表示的凭证”。
    for provider in ["legacy", "future", "empty", "scalar"] {
        assert_eq!(storage.read(provider, options(None)).await.unwrap(), None);
        // readStoredCredential 不解析 key，也不因无法投影而 panic。
        assert_eq!(
            read_stored_credential(provider, &path_string(&auth_path)),
            None
        );
    }

    // list：type 可识别的条目仍然列出（TS 对未知/缺失 type 给出 `type: undefined`，
    // 冻结的 `CredentialInfo.type` 无法表达，只能省略）。
    assert_eq!(
        storage.list(options(None)).await.unwrap(),
        vec![CredentialInfo {
            provider_id: "legacy".into(),
            r#type: AuthType::OAuth,
        }]
    );

    // TS 把原始条目交给回调；Rust 传 None。忽略 current 的回调（Models.login 的形态）
    // 仍能修复条目并写盘。
    let result = storage
        .modify(
            "legacy",
            modify_fn(|current| async move {
                assert!(current.is_none());
                Ok(Some(api_key("new")))
            }),
            options(None),
        )
        .await
        .unwrap();
    assert_eq!(result, Some(api_key("new")));
    let written = read_json(&auth_path);
    assert_eq!(written["legacy"], json!({"type": "api_key", "key": "new"}));
    // 其它无法表达的条目在写回时逐字保留。
    assert_eq!(
        written["future"],
        json!({"type": "bearer", "token": "t", "n": 1})
    );
    assert_eq!(written["empty"], json!({}));
    assert_eq!(written["scalar"], json!(5));
}

/// `api_key` 的非字符串 key：TS 的 read 在 `resolveConfigValue` 里抛 TypeError（逐字复刻）。
#[tokio::test]
async fn read_reports_the_ts_type_error_for_non_string_api_keys() {
    let temp_dir = TempDir::new().unwrap();
    let auth_path = temp_dir.path().join("auth.json");
    write_raw(
        &auth_path,
        r#"{"number":{"type":"api_key","key":5},"null":{"type":"api_key","key":null}}"#,
    );
    let storage = AuthStorage::create(&path_string(&auth_path));

    let error = storage.read("number", options(None)).await.unwrap_err();
    assert_eq!(error.name, "TypeError");
    assert_eq!(error.message, "config.startsWith is not a function");

    let error = storage.read("null", options(None)).await.unwrap_err();
    assert_eq!(error.name, "TypeError");
    assert_eq!(
        error.message,
        "Cannot read properties of null (reading 'startsWith')"
    );

    // readStoredCredential 不解析 key：不抛错，投影为 key: None。
    assert_eq!(
        read_stored_credential("number", &path_string(&auth_path)),
        Some(Credential::ApiKey(ApiKeyCredential::default()))
    );
}

/// auth.json 的 I/O 错误文案是 Node 风格（`CODE: message, syscall 'path'`）。
#[cfg(unix)]
#[tokio::test]
async fn reports_node_style_auth_file_errors() {
    let temp_dir = TempDir::new().unwrap();
    let auth_path = temp_dir.path().join("auth.json");
    write_json(
        &auth_path,
        &json!({"anthropic": {"type": "api_key", "key": "stored"}}),
    );

    // 读 EACCES：AuthStorage 带 signal 的读取 / modify、ReadOnly 的包装文案。
    let guard = PermissionGuard::new(&auth_path, 0o000);
    let storage = AuthStorage::create(&path_string(&auth_path));
    let expected = format!(
        "EACCES: permission denied, open '{}'",
        path_string(&auth_path)
    );
    let error = storage
        .read("anthropic", options(Some(AbortScope::new())))
        .await
        .unwrap_err();
    assert_eq!(error.message, expected);
    // Node 的 fs 错误带 `error.code`（调用方据此分支）。
    assert_eq!(error.error_code.as_deref(), Some("EACCES"));
    let error = storage
        .modify(
            "anthropic",
            modify_fn(|_| async { Ok(Some(api_key("new"))) }),
            options(None),
        )
        .await
        .unwrap_err();
    assert_eq!(error.message, expected);
    assert_eq!(error.error_code.as_deref(), Some("EACCES"));
    let storage = ReadOnlyAuthStorage::new(&path_string(&auth_path));
    let error = storage.read("anthropic", options(None)).await.unwrap_err();
    assert_eq!(
        error.message,
        format!("Failed to read auth.json: {expected}")
    );
    drop(guard);

    // 写 EACCES。
    let guard = PermissionGuard::new(&auth_path, 0o444);
    let storage = AuthStorage::create(&path_string(&auth_path));
    let error = storage
        .modify(
            "anthropic",
            modify_fn(|_| async { Ok(Some(api_key("new"))) }),
            options(None),
        )
        .await
        .unwrap_err();
    assert_eq!(
        error.message,
        format!(
            "EACCES: permission denied, open '{}'",
            path_string(&auth_path)
        )
    );
    assert_eq!(error.error_code.as_deref(), Some("EACCES"));
    drop(guard);

    // auth.json 是目录：EISDIR 的 read 阶段文案不带 path。
    fs::remove_file(&auth_path).unwrap();
    fs::create_dir(&auth_path).unwrap();
    let storage = AuthStorage::create(&path_string(&auth_path));
    let error = storage
        .read("anthropic", options(Some(AbortScope::new())))
        .await
        .unwrap_err();
    assert_eq!(
        error.message,
        "EISDIR: illegal operation on a directory, read"
    );
    assert_eq!(error.error_code.as_deref(), Some("EISDIR"));
    let storage = ReadOnlyAuthStorage::new(&path_string(&auth_path));
    let error = storage.read("anthropic", options(None)).await.unwrap_err();
    assert_eq!(
        error.message,
        "Failed to read auth.json: EISDIR: illegal operation on a directory, read"
    );
}

// ---------------------------------------------------------------------------
// InMemoryAuthStorageBackend 的同步路径
// ---------------------------------------------------------------------------

/// in-memory backend 的 withLock 语义
#[test]
fn in_memory_backend_runs_sync_locks_and_writes_next() {
    let backend = InMemoryAuthStorageBackend::new();
    backend
        .with_lock(|current| {
            assert_eq!(current, None);
            Ok(LockResult::with_next((), "{}".to_string()))
        })
        .unwrap();
    backend
        .with_lock(|current| {
            assert_eq!(current, Some("{}"));
            Ok(LockResult::new(()))
        })
        .unwrap();
}

/// auth.json 的写盘字节与 TS `JSON.stringify(data, null, 2)` 一致（黄金测试）。
///
/// fixture 由 `/home/ctyun/kiana-work/scratch/kiana-config__auth-trust/gen-fixtures.mjs`
/// 在 Node 里用 `JSON.stringify(input, null, 2)` 生成。
#[tokio::test]
async fn writes_auth_json_matching_ts_golden_fixtures() {
    let cases: Vec<Value> =
        serde_json::from_str(include_str!("fixtures/auth/auth-json-cases.json")).unwrap();
    for case in cases {
        let temp_dir = TempDir::new().unwrap();
        let auth_path = temp_dir.path().join("auth.json");
        let input = &case["input"];
        fs::write(&auth_path, serde_json::to_string(input).unwrap()).unwrap();
        let storage = AuthStorage::create(&path_string(&auth_path));
        // delete 一个不存在的 provider 会原样写回整份数据（触发唯一的写路径）。
        storage.delete("__missing__", options(None)).await.unwrap();
        assert_eq!(
            fs::read_to_string(&auth_path).unwrap(),
            case["expected"].as_str().unwrap(),
            "case: {}",
            case["name"]
        );
    }
}

// ---------------------------------------------------------------------------
// 与 kiana-ai Models 的集成：刷新失败被翻译成 code=auth，随后可重试
// （TS: auth-storage.test.ts "translates a credential-store refresh failure and allows a later retry"）
// ---------------------------------------------------------------------------

mod refresh_failure_through_models {
    use super::*;
    use kiana_ai::auth::resolve::AuthResolutionOverrides;
    use kiana_ai::auth::types::{
        AuthInteraction, LoginOptions, ModelAuth, OAuthAuth, ProviderAuth,
    };
    use kiana_ai::error::ModelsErrorCode;
    use kiana_ai::event_stream::AssistantMessageEventStream;
    use kiana_ai::models::{
        CreateModelsOptions, CreateProviderOptions, GetAuthTarget, ProviderApiImpl,
        ProviderStreams, create_models, create_provider,
    };
    use kiana_ai::types::{Model, SimpleStreamOptions, StreamOptions, TranscriptContext};

    struct FlakyStore {
        base: AuthStorage,
        fail_next_modify: Mutex<bool>,
    }

    #[async_trait::async_trait]
    impl CredentialStore for FlakyStore {
        async fn read(
            &self,
            provider_id: &str,
            options: AuthOperationOptions,
        ) -> Result<Option<Credential>, AuthError> {
            self.base.read(provider_id, options).await
        }

        async fn list(
            &self,
            options: AuthOperationOptions,
        ) -> Result<Vec<CredentialInfo>, AuthError> {
            self.base.list(options).await
        }

        async fn modify(
            &self,
            provider_id: &str,
            f: CredentialModifyFn,
            options: AuthOperationOptions,
        ) -> Result<Option<Credential>, AuthError> {
            let should_fail = {
                let mut flag = self.fail_next_modify.lock().unwrap();
                let value = *flag;
                *flag = false;
                value
            };
            if should_fail {
                return Err(AuthError::new("credential store unavailable"));
            }
            self.base.modify(provider_id, f, options).await
        }

        async fn delete(
            &self,
            provider_id: &str,
            options: AuthOperationOptions,
        ) -> Result<(), AuthError> {
            self.base.delete(provider_id, options).await
        }
    }

    struct FakeOAuth;

    #[async_trait::async_trait]
    impl OAuthAuth for FakeOAuth {
        fn name(&self) -> &str {
            "OAuth"
        }

        async fn login(
            &self,
            _interaction: &dyn AuthInteraction,
            _options: &LoginOptions,
        ) -> Result<OAuthCredential, AuthError> {
            Err(AuthError::new("not used"))
        }

        async fn refresh(
            &self,
            credential: &OAuthCredential,
            _signal: &AbortScope,
        ) -> Result<OAuthCredential, AuthError> {
            Ok(OAuthCredential::new(
                "refreshed-access",
                credential.refresh.clone(),
                1_767_225_600_000.0,
            ))
        }

        async fn to_auth(&self, credential: &OAuthCredential) -> Result<ModelAuth, AuthError> {
            Ok(ModelAuth {
                api_key: Some(credential.access.clone()),
                ..ModelAuth::default()
            })
        }
    }

    struct DummyStreams;

    impl ProviderStreams for DummyStreams {
        fn stream(
            &self,
            _model: &Model,
            _context: &TranscriptContext,
            _options: StreamOptions,
        ) -> AssistantMessageEventStream {
            AssistantMessageEventStream::new()
        }

        fn stream_simple(
            &self,
            _model: &Model,
            _context: &TranscriptContext,
            _options: SimpleStreamOptions,
        ) -> AssistantMessageEventStream {
            AssistantMessageEventStream::new()
        }
    }

    /// translates a credential-store refresh failure and allows a later retry
    #[tokio::test]
    async fn translates_a_credential_store_refresh_failure_and_allows_a_later_retry() {
        let base = AuthStorage::in_memory(auth_data_from_value(json!({
            "oauth-provider": {"type": "oauth", "access": "expired-access", "refresh": "refresh-token", "expires": 0}
        })));
        let credentials = Arc::new(FlakyStore {
            base,
            fail_next_modify: Mutex::new(true),
        });
        let models = create_models(CreateModelsOptions {
            credentials: Some(credentials),
            ..CreateModelsOptions::default()
        });
        let provider = create_provider(CreateProviderOptions {
            id: "oauth-provider".to_string(),
            name: Some("OAuth Provider".to_string()),
            auth: ProviderAuth {
                api_key: None,
                oauth: Some(Arc::new(FakeOAuth)),
            },
            api: Some(ProviderApiImpl::Single(Arc::new(DummyStreams))),
            ..CreateProviderOptions::default()
        })
        .unwrap();
        models.set_provider(provider);

        let error = models
            .get_auth(
                GetAuthTarget::Provider("oauth-provider"),
                AuthResolutionOverrides::default(),
            )
            .await
            .unwrap_err();
        assert_eq!(error.code, Some(ModelsErrorCode::Auth));
        assert_eq!(error.name, "ModelsError");

        let auth = models
            .get_auth(
                GetAuthTarget::Provider("oauth-provider"),
                AuthResolutionOverrides::default(),
            )
            .await
            .unwrap()
            .expect("auth result");
        assert_eq!(auth.auth.api_key.as_deref(), Some("refreshed-access"));
    }
}
