//! `packages/coding-agent/test/mcp-oauth-store.test.ts`：`mcp-auth.json` 的键与 legacy 迁移。

use kiana_builtin_extensions::mcp::oauth::{McpOAuthCredentialStore, McpOAuthServerStore};
use kiana_config::auth::storage::{
    AuthStorageBackend, FileAuthStorageBackend, InMemoryAuthStorageBackend, LockResult,
};
use kiana_mcp::oauth::{McpOAuthState, McpOAuthStateStore, OAuthTokens};
use serde_json::{Map, Value};

const SERVER_URL: &str = "https://mcp.example.com/mcp";

fn state(access_token: &str) -> McpOAuthState {
    let mut fields = Map::new();
    fields.insert(
        "access_token".to_string(),
        Value::String(access_token.to_string()),
    );
    fields.insert(
        "token_type".to_string(),
        Value::String("Bearer".to_string()),
    );
    McpOAuthState {
        server_url: SERVER_URL.to_string(),
        tokens: Some(OAuthTokens::from_fields(fields)),
        ..McpOAuthState::new(SERVER_URL)
    }
}

fn backend() -> InMemoryAuthStorageBackend {
    InMemoryAuthStorageBackend::new()
}

fn store(backend: &InMemoryAuthStorageBackend) -> McpOAuthCredentialStore {
    McpOAuthCredentialStore::new(Some(AuthStorageBackend::InMemory(backend.clone())), None)
}

fn stored_keys(backend: &InMemoryAuthStorageBackend) -> Vec<String> {
    let current = backend
        .with_lock(|current| Ok(LockResult::new(current.unwrap_or("{}").to_string())))
        .expect("in-memory backend never fails");
    let parsed: Value = serde_json::from_str(&current).expect("stored JSON parses");
    parsed
        .as_object()
        .map(|map| map.keys().cloned().collect())
        .unwrap_or_default()
}

fn store_for(store: &McpOAuthCredentialStore, name: &str) -> McpOAuthServerStore {
    store.for_server(name, SERVER_URL)
}

#[tokio::test]
async fn keeps_separate_credentials_for_servers_sharing_a_server_url() {
    let backend = backend();
    let store = store(&backend);
    store_for(&store, "work")
        .save(state("work-token"))
        .await
        .unwrap();
    store_for(&store, "personal")
        .save(state("personal-token"))
        .await
        .unwrap();

    assert_eq!(
        store_for(&store, "work")
            .load()
            .await
            .unwrap()
            .and_then(|state| state.tokens)
            .and_then(|tokens| tokens.string("access_token").map(str::to_string))
            .as_deref(),
        Some("work-token")
    );
    assert_eq!(
        store_for(&store, "personal")
            .load()
            .await
            .unwrap()
            .and_then(|state| state.tokens)
            .and_then(|tokens| tokens.string("access_token").map(str::to_string))
            .as_deref(),
        Some("personal-token")
    );

    assert!(store.remove("work", SERVER_URL).unwrap());
    assert!(store_for(&store, "work").load().await.unwrap().is_none());
    assert_eq!(
        store
            .tokens("personal", SERVER_URL)
            .unwrap()
            .and_then(|tokens| tokens.string("access_token").map(str::to_string))
            .as_deref(),
        Some("personal-token")
    );
}

#[tokio::test]
async fn moves_credentials_stored_by_server_url_to_the_first_server_that_loads_them() {
    let backend = backend();
    backend
        .with_lock(|_| {
            let mut states = Map::new();
            states.insert(
                SERVER_URL.to_string(),
                serde_json::to_value(state("legacy-token")).unwrap(),
            );
            let legacy = serde_json::to_string(&Value::Object(states)).unwrap();
            Ok(LockResult::with_next((), legacy))
        })
        .unwrap();
    let store = store(&backend);

    // 读 tokens 不接管 legacy 状态。
    assert_eq!(
        store
            .tokens("work", SERVER_URL)
            .unwrap()
            .and_then(|tokens| tokens.string("access_token").map(str::to_string))
            .as_deref(),
        Some("legacy-token")
    );
    assert_eq!(stored_keys(&backend), vec![SERVER_URL.to_string()]);

    assert_eq!(
        store_for(&store, "my_work")
            .load()
            .await
            .unwrap()
            .and_then(|state| state.tokens)
            .and_then(|tokens| tokens.string("access_token").map(str::to_string))
            .as_deref(),
        Some("legacy-token")
    );
    // 只在 `-`/`_` 上不同的名字是同一个服务器。
    assert_eq!(
        store
            .tokens("my-work", SERVER_URL)
            .unwrap()
            .and_then(|tokens| tokens.string("access_token").map(str::to_string))
            .as_deref(),
        Some("legacy-token")
    );
    assert!(
        store_for(&store, "personal")
            .load()
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(
        stored_keys(&backend),
        vec![format!("mcp__my_work|{SERVER_URL}")]
    );
}

#[tokio::test]
async fn signs_out_of_credentials_stored_by_server_url() {
    let backend = backend();
    backend
        .with_lock(|_| {
            let mut states = Map::new();
            states.insert(
                SERVER_URL.to_string(),
                serde_json::to_value(state("legacy-token")).unwrap(),
            );
            let legacy = serde_json::to_string(&Value::Object(states)).unwrap();
            Ok(LockResult::with_next((), legacy))
        })
        .unwrap();
    let store = store(&backend);

    assert!(store.remove("work", SERVER_URL).unwrap());
    assert!(stored_keys(&backend).is_empty());
    assert!(!store.remove("work", SERVER_URL).unwrap());
}

fn raw_content(backend: &InMemoryAuthStorageBackend) -> String {
    backend
        .with_lock(|current| Ok(LockResult::new(current.unwrap_or("{}").to_string())))
        .expect("in-memory backend never fails")
}

/// TS 的 `remove()` 在没删到东西时返回 `{result:false}`（没有 `next`），文件一个字节都不变。
#[tokio::test]
async fn removing_missing_credentials_does_not_rewrite_the_file() {
    let compact = r#"{"other|https://example.com/mcp":{"serverUrl":"https://example.com/mcp"}}"#;
    let first = backend();
    first
        .with_lock(|_| Ok(LockResult::with_next((), compact.to_string())))
        .unwrap();
    let credentials = store(&first);
    assert!(!credentials.remove("work", SERVER_URL).unwrap());
    assert_eq!(raw_content(&first), compact);

    let empty_backend = backend();
    empty_backend
        .with_lock(|_| Ok(LockResult::with_next((), "{}".to_string())))
        .unwrap();
    let credentials = store(&empty_backend);
    assert!(!credentials.remove("work", SERVER_URL).unwrap());
    assert_eq!(raw_content(&empty_backend), "{}");
}

/// TS 的 `tokens()`/`remove()` 直接调用 `backend.withLock`：`JSON.parse` 失败会抛出，
/// 不会被当成"没有凭据"。
#[tokio::test]
async fn surfaces_corrupt_credential_files_instead_of_treating_them_as_empty() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("mcp-auth.json");
    std::fs::write(&path, "not json").unwrap();
    let store = McpOAuthCredentialStore::new(
        Some(AuthStorageBackend::File(FileAuthStorageBackend::new(
            &path.to_string_lossy(),
        ))),
        None,
    );
    let tokens_error = store.tokens("work", SERVER_URL).unwrap_err();
    assert_eq!(tokens_error.name(), "SyntaxError");
    let remove_error = store.remove("work", SERVER_URL).unwrap_err();
    assert_eq!(remove_error.name(), "SyntaxError");
}

/// TS 写回用 `JSON.stringify(states, null, 2)`：数字是 ECMAScript `Number::toString`，
/// 不是 serde 的 ryu 形式（`1.2345678901234567e+19`）。
#[tokio::test]
async fn writes_numbers_back_in_javascript_form() {
    let backend = backend();
    // 另一个条目（这里用 legacy 键）里的数字在写回时必须保持 JS 格式。
    let content = format!(
        "{{\"{SERVER_URL}\":{{\"serverUrl\":\"{SERVER_URL}\",\"tokens\":{{\"access_token\":\"legacy\"}},\"expiresAt\":12345678901234567890}}}}"
    );
    backend
        .with_lock(|_| Ok(LockResult::with_next((), content.clone())))
        .unwrap();
    let store = store(&backend);
    store_for(&store, "work")
        .save(state("new-token"))
        .await
        .unwrap();

    let written = raw_content(&backend);
    assert!(
        written.contains("\"expiresAt\": 12345678901234567000"),
        "{written}"
    );
    assert!(!written.contains("e+19"), "{written}");
    assert_eq!(
        store
            .tokens("work", SERVER_URL)
            .unwrap()
            .and_then(|tokens| tokens.string("access_token").map(str::to_string))
            .as_deref(),
        Some("new-token")
    );
}

// ---------------------------------------------------------------------------
// issuer 进凭据键 / 条件性失效
// ---------------------------------------------------------------------------

fn state_with_issuer(access_token: &str, issuer: &str) -> McpOAuthState {
    let mut fields = Map::new();
    fields.insert("issuer".to_string(), Value::String(issuer.to_string()));
    let mut state = state(access_token);
    state.discovery = Some(kiana_mcp::oauth::OAuthDiscoveryState {
        authorization_server_url: issuer.to_string(),
        authorization_server_metadata: Some(
            kiana_mcp::oauth::AuthorizationServerMetadata::from_fields(fields),
        ),
        ..kiana_mcp::oauth::OAuthDiscoveryState::default()
    });
    state
}

fn token_of(state: &McpOAuthState) -> Option<String> {
    state
        .tokens
        .as_ref()
        .and_then(|tokens| tokens.string("access_token"))
        .map(str::to_string)
}

#[tokio::test]
async fn issuer_is_part_of_the_credential_key() {
    let backend = backend();
    let store = store(&backend);
    store_for(&store, "work")
        .save(state_with_issuer("a-token", "https://a.example"))
        .await
        .unwrap();

    let keys = stored_keys(&backend);
    assert_eq!(
        keys,
        vec![format!("mcp__work|{SERVER_URL}|https://a.example")]
    );

    let matching = store.for_server_with_issuer("work", SERVER_URL, Some("https://a.example"));
    assert_eq!(
        token_of(
            &matching
                .load()
                .await
                .unwrap()
                .expect("matching issuer loads")
        )
        .as_deref(),
        Some("a-token")
    );

    let other = store.for_server_with_issuer("work", SERVER_URL, Some("https://b.example"));
    assert!(other.load().await.unwrap().is_none());
}

#[tokio::test]
async fn invalidation_only_removes_mismatched_issuers() {
    let backend = backend();
    let store = store(&backend);
    store_for(&store, "work")
        .save(state_with_issuer("a-token", "https://a.example"))
        .await
        .unwrap();

    assert!(
        !store
            .invalidate_if_issuer_mismatch("work", SERVER_URL, Some("https://a.example"))
            .unwrap()
    );
    assert_eq!(stored_keys(&backend).len(), 1);

    assert!(
        store
            .invalidate_if_issuer_mismatch("work", SERVER_URL, Some("https://b.example"))
            .unwrap()
    );
    assert!(stored_keys(&backend).is_empty());
    assert!(store_for(&store, "work").load().await.unwrap().is_none());
}

#[tokio::test]
async fn legacy_server_lookup_reads_a_single_issuer_entry() {
    let backend = backend();
    let store = store(&backend);
    store_for(&store, "work")
        .save(state_with_issuer("a-token", "https://a.example"))
        .await
        .unwrap();
    let loaded = store_for(&store, "work").load().await.unwrap();
    assert_eq!(
        loaded.as_ref().and_then(token_of).as_deref(),
        Some("a-token")
    );
    assert_eq!(
        store.stored_issuer("work", SERVER_URL).unwrap().as_deref(),
        Some("https://a.example")
    );
    // tokens() 走同一选择逻辑。
    let tokens = store.tokens("work", SERVER_URL).unwrap();
    assert_eq!(
        tokens
            .as_ref()
            .and_then(|tokens| tokens.string("access_token")),
        Some("a-token")
    );
}

#[tokio::test]
async fn expected_issuer_migrates_a_base_entry_with_the_same_issuer() {
    let backend = backend();
    // 手工把带 issuer 的状态放在旧基础键下（模拟升级前的文件）。
    let base_key = format!("mcp__work|{SERVER_URL}");
    let value = serde_json::to_value(state_with_issuer("old", "https://a.example")).unwrap();
    backend
        .with_lock(|_| {
            let mut states = Map::new();
            states.insert(base_key.clone(), value.clone());
            Ok(LockResult::with_next(
                (),
                serde_json::to_string(&Value::Object(states)).unwrap(),
            ))
        })
        .unwrap();
    let store = store(&backend);
    let scoped = store.for_server_with_issuer("work", SERVER_URL, Some("https://a.example"));
    assert_eq!(
        token_of(&scoped.load().await.unwrap().expect("migrates")).as_deref(),
        Some("old")
    );
    assert_eq!(
        stored_keys(&backend),
        vec![format!("{base_key}|https://a.example")]
    );
}

#[tokio::test]
async fn saving_a_new_issuer_drops_stale_issuer_credentials() {
    let backend = backend();
    let store = store(&backend);
    store_for(&store, "work")
        .save(state_with_issuer("a-token", "https://a.example"))
        .await
        .unwrap();
    // 授权服务器换了：同一基础键保存新的 issuer 状态。
    store_for(&store, "work")
        .save(state_with_issuer("b-token", "https://b.example"))
        .await
        .unwrap();

    assert_eq!(
        stored_keys(&backend),
        vec![format!("mcp__work|{SERVER_URL}|https://b.example")]
    );
    let loaded = store_for(&store, "work")
        .load()
        .await
        .unwrap()
        .expect("the fresh issuer state loads");
    assert_eq!(token_of(&loaded).as_deref(), Some("b-token"));
    assert_eq!(
        store.stored_issuer("work", SERVER_URL).unwrap().as_deref(),
        Some("https://b.example")
    );
}

#[tokio::test]
async fn runtime_helpers_scope_to_the_stored_issuer() {
    let backend = backend();
    let store = store(&backend);
    store_for(&store, "work")
        .save(state_with_issuer("a-token", "https://a.example"))
        .await
        .unwrap();

    // 运行时登录/刷新用：按已存 issuer 打开作用域存储。
    let scoped = store.for_server_with_stored_issuer("work", SERVER_URL);
    assert_eq!(
        token_of(&scoped.load().await.unwrap().expect("scoped state")).as_deref(),
        Some("a-token")
    );
    // 同名 issuer 下没有可清理的条目。
    assert!(
        !store
            .invalidate_stored_issuer_mismatches("work", SERVER_URL)
            .unwrap()
    );
    assert_eq!(stored_keys(&backend).len(), 1);
}

#[tokio::test]
async fn remove_deletes_every_key_in_the_server_family() {
    let backend = backend();
    let store = store(&backend);
    store_for(&store, "work")
        .save(state_with_issuer("a-token", "https://a.example"))
        .await
        .unwrap();
    assert!(store.remove("work", SERVER_URL).unwrap());
    assert!(stored_keys(&backend).is_empty());
}
